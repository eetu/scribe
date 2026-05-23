//! Filename canonicalization — port of OpenAudible's `FileDestination` + `FilenameUtils`.
//!
//! Path layout is configurable via env templates. The same template engine
//! drives both the canonical library path and the backup path; only the
//! defaults differ.
//!
//! ## Placeholders
//!
//! | token | description |
//! |---|---|
//! | `{title}` | full title |
//! | `{short_title}` | title up to ` - ` or ` (`, else first 64 chars |
//! | `{subtitle}` | subtitle if present |
//! | `{author}` | first author |
//! | `{authors}` | all authors joined with `, ` |
//! | `{narrator}` | first narrator |
//! | `{series_title}` | series name if any |
//! | `{series_num}` | "01", "02", ... — padded to 2 when integer |
//! | `{asin}` | book's ASIN |
//! | `{year}` | first 4 chars of release_date |
//!
//! Two optional flavours:
//!
//! * `{key?}` — **segment-optional**: when that token resolves to empty, the
//!   entire path segment it sits in is dropped. Used to make whole folder
//!   levels disappear (no author → no author folder).
//!
//! * `[literal{key}literal]` — **inline group**: when any `{key}` inside
//!   the group is empty, the group's contents are dropped but the rest of
//!   the surrounding segment is kept. Used to drop a literal prefix that
//!   would only make sense with a populated placeholder.
//!
//! Both combine in the default template:
//!
//! ```text
//! {author?}/{series_title?}/[#{series_num} - ]{title}/{title}.m4b
//! ```
//!
//! Series book →  `Author/Series/#03 - Title/Title.m4b`
//! Standalone   →  `Author/Title/Title.m4b`
//! No author    →  `Title/Title.m4b`
//!
//! ## Sanitization
//!
//! Per-segment scrub matches OA's rules:
//!   * `/ \ : | ` replaced with `-`
//!   * Other illegal chars (`\0–\x1f \" * < > ?`) removed
//!   * Collapse repeated `-` and `  `
//!   * Drop " (Unabridged)" suffix
//!   * Trim trailing dots and whitespace
//!   * Each segment capped at 250 chars (NTFS limit)

use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct NamingInput<'a> {
    pub asin: &'a str,
    pub title: &'a str,
    pub subtitle: Option<&'a str>,
    pub authors: &'a [String],
    pub narrators: &'a [String],
    pub series_title: Option<&'a str>,
    pub series_sequence: Option<&'a str>,
    pub release_date: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct Templates {
    pub library: String,
    /// Path template for the untouched file downloaded from Audible.
    /// Stored under `SCRIBE_ORIGINAL_DIR`; ABS never indexes this tree.
    pub original: String,
}

impl Templates {
    /// Defaults match audiobookshelf's preferred layout: author folder at
    /// top, series sub-folder when applicable, one folder per book so cover
    /// art + optional supplementary files stay together.
    pub const DEFAULT_LIBRARY: &'static str =
        "{author?}/{series_title?}/[#{series_num} - ]{title}/{title}.m4b";
    /// Originals keep the same hierarchy so each `.aaxc`/`.aax` sits next
    /// to whatever folder structure would have housed the M4B — easy to
    /// locate the source if a re-convert is ever needed.
    pub const DEFAULT_ORIGINAL: &'static str =
        "{author?}/{series_title?}/{title}-{asin}.aaxc";

    pub fn from_env() -> Self {
        Self {
            library: std::env::var("SCRIBE_FILENAME_TEMPLATE_M4B")
                .unwrap_or_else(|_| Self::DEFAULT_LIBRARY.into()),
            original: std::env::var("SCRIBE_FILENAME_TEMPLATE_ORIGINAL")
                .unwrap_or_else(|_| Self::DEFAULT_ORIGINAL.into()),
        }
    }
}

pub fn library_path(root: &std::path::Path, tpl: &str, input: &NamingInput<'_>) -> PathBuf {
    apply(root, tpl, input)
}

pub fn original_path(root: &std::path::Path, tpl: &str, input: &NamingInput<'_>) -> PathBuf {
    apply(root, tpl, input)
}

fn apply(root: &std::path::Path, tpl: &str, input: &NamingInput<'_>) -> PathBuf {
    // Render each path segment independently so an empty placeholder can
    // drop a whole folder level, not just leave a stray separator.
    let rendered: Vec<String> = tpl
        .split('/')
        .filter_map(|seg| render_segment(seg, input))
        .collect();
    let mut p = root.to_path_buf();
    for seg in rendered {
        p.push(seg);
    }
    p
}

fn render_segment(template_segment: &str, input: &NamingInput<'_>) -> Option<String> {
    let mut out = String::with_capacity(template_segment.len() * 2);
    let mut chars = template_segment.chars().peekable();
    let mut segment_optional_empty = false;
    while let Some(c) = chars.next() {
        match c {
            '{' => match read_placeholder(&mut chars, input) {
                Placeholder::Filled(v) => out.push_str(&v),
                Placeholder::Empty { optional } => {
                    if optional {
                        segment_optional_empty = true;
                        break;
                    }
                    // Required-but-empty just emits nothing.
                }
            },
            '[' => match read_group(&mut chars, input) {
                Group::Filled(s) => out.push_str(&s),
                Group::Dropped => {}
            },
            other => out.push(other),
        }
    }
    if segment_optional_empty {
        return None;
    }
    let cleaned = sanitize_segment(&out);
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned)
}

enum Placeholder {
    Filled(String),
    Empty { optional: bool },
}

fn read_placeholder<I>(chars: &mut std::iter::Peekable<I>, input: &NamingInput<'_>) -> Placeholder
where
    I: Iterator<Item = char>,
{
    let mut key = String::new();
    for k in chars.by_ref() {
        if k == '}' {
            break;
        }
        key.push(k);
    }
    let optional = key.ends_with('?');
    let real_key = if optional { &key[..key.len() - 1] } else { key.as_str() };
    let value = resolve(real_key, input);
    if value.is_empty() {
        Placeholder::Empty { optional }
    } else {
        Placeholder::Filled(value)
    }
}

enum Group {
    Filled(String),
    Dropped,
}

fn read_group<I>(chars: &mut std::iter::Peekable<I>, input: &NamingInput<'_>) -> Group
where
    I: Iterator<Item = char>,
{
    // Collect raw text until ']'.
    let mut raw = String::new();
    for k in chars.by_ref() {
        if k == ']' {
            break;
        }
        raw.push(k);
    }
    // Render placeholders inside the group. If any comes back empty (with or
    // without `?`), the whole group drops.
    let mut out = String::with_capacity(raw.len());
    let mut inner = raw.chars().peekable();
    while let Some(c) = inner.next() {
        if c != '{' {
            out.push(c);
            continue;
        }
        let mut key = String::new();
        for k in inner.by_ref() {
            if k == '}' {
                break;
            }
            key.push(k);
        }
        let real_key = key.strip_suffix('?').unwrap_or(&key);
        let value = resolve(real_key, input);
        if value.is_empty() {
            return Group::Dropped;
        }
        out.push_str(&value);
    }
    Group::Filled(out)
}

fn resolve(key: &str, input: &NamingInput<'_>) -> String {
    match key {
        "title" => input.title.to_string(),
        "short_title" => short_title(input.title),
        "subtitle" => input.subtitle.unwrap_or_default().to_string(),
        "author" => input.authors.first().cloned().unwrap_or_default(),
        "authors" => input.authors.join(", "),
        "narrator" => input.narrators.first().cloned().unwrap_or_default(),
        "series_title" => input.series_title.unwrap_or_default().to_string(),
        "series_num" => match input.series_sequence {
            Some(s) => pad_series(s),
            None => String::new(),
        },
        "asin" => input.asin.to_string(),
        "year" => input.release_date.map(year_from).unwrap_or_default(),
        _ => String::new(),
    }
}

fn short_title(t: &str) -> String {
    if t.len() <= 64 {
        return t.to_string();
    }
    for sep in [" - ", ": ", " ("] {
        if let Some(idx) = t.find(sep) {
            return t[..idx].to_string();
        }
    }
    // Hard truncate fallback.
    t.chars().take(64).collect()
}

fn pad_series(s: &str) -> String {
    let s = s.trim();
    // Padding only kicks in when the sequence is a pure integer; "3.5" or
    // "II" pass through untouched.
    if let Ok(n) = s.parse::<u32>() {
        return format!("{:02}", n);
    }
    s.to_string()
}

fn year_from(date: &str) -> String {
    date.chars().take(4).collect()
}

fn sanitize_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' | '\\' | ':' | '|' => out.push('-'),
            '"' | '*' | '<' | '>' | '?' => {}
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    // Drop OA's "(Unabridged)" baggage that Audible bakes into many titles.
    out = out.replace(" (Unabridged)", "").replace("(Unabridged)", "");
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    while out.contains("  ") {
        out = out.replace("  ", " ");
    }
    while out.ends_with('.') {
        out.pop();
    }
    let trimmed = out.trim().to_string();
    if trimmed.chars().count() > 250 {
        trimmed.chars().take(250).collect()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample<'a>() -> NamingInput<'a> {
        NamingInput {
            asin: "B0EXAMPLE",
            title: "Project Hail Mary (Unabridged)",
            subtitle: None,
            authors: &[],
            narrators: &[],
            series_title: None,
            series_sequence: None,
            release_date: Some("2021-05-04"),
        }
    }

    #[test]
    fn standalone_book_collapses_series_segments() {
        let mut s = sample();
        let andy = ["Andy Weir".to_string()];
        s.authors = &andy;
        let p = library_path(std::path::Path::new("/lib"), Templates::DEFAULT_LIBRARY, &s);
        assert_eq!(p.to_str().unwrap(), "/lib/Andy Weir/Project Hail Mary/Project Hail Mary.m4b");
    }

    #[test]
    fn series_book_padded_to_two_digits() {
        let andy = ["Dennis E. Taylor".to_string()];
        let s = NamingInput {
            asin: "B0BOBIVERSE",
            title: "All These Worlds",
            subtitle: None,
            authors: &andy,
            narrators: &[],
            series_title: Some("Bobiverse"),
            series_sequence: Some("3"),
            release_date: None,
        };
        let p = library_path(std::path::Path::new("/lib"), Templates::DEFAULT_LIBRARY, &s);
        assert_eq!(
            p.to_str().unwrap(),
            "/lib/Dennis E. Taylor/Bobiverse/#03 - All These Worlds/All These Worlds.m4b"
        );
    }

    #[test]
    fn illegal_chars_swept() {
        let andy = ["A/B".to_string()];
        let s = NamingInput {
            asin: "B0X",
            title: "Q?: a < hard > title",
            subtitle: None,
            authors: &andy,
            narrators: &[],
            series_title: None,
            series_sequence: None,
            release_date: None,
        };
        let p = library_path(std::path::Path::new("/lib"), Templates::DEFAULT_LIBRARY, &s);
        // `/` becomes `-`, `?` and `<>` removed entirely.
        assert!(p.to_str().unwrap().contains("A-B"));
        assert!(!p.to_str().unwrap().contains('?'));
    }

    #[test]
    fn missing_author_drops_to_unknown_friendly() {
        let s = sample();
        // No authors: {author} expands to empty -> author segment drops.
        let p = library_path(std::path::Path::new("/lib"), Templates::DEFAULT_LIBRARY, &s);
        // Should fall through to title-only path.
        assert_eq!(p.to_str().unwrap(), "/lib/Project Hail Mary/Project Hail Mary.m4b");
    }

    #[test]
    fn short_title_kicks_in_for_long_strings() {
        let long = "A".repeat(70) + " - subtitle here";
        let andy = ["X".to_string()];
        let s = NamingInput {
            asin: "B0",
            title: &long,
            subtitle: None,
            authors: &andy,
            narrators: &[],
            series_title: None,
            series_sequence: None,
            release_date: None,
        };
        let st = short_title(s.title);
        assert!(st.len() <= 70);
        assert!(!st.contains(" - "));
    }
}
