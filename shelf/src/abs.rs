//! Response shapes mirroring Audiobookshelf's REST API for the subset
//! that Listen This consumes. Fields chosen to match what the client
//! actually reads — anything else stays missing rather than fabricated,
//! to avoid promising data scribe doesn't have.

use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MePermissions {
    #[serde(rename = "accessAllLibraries")]
    pub access_all_libraries: u8,
    #[serde(rename = "accessAllTags")]
    pub access_all_tags: u8,
    pub download: bool,
    pub update: bool,
    pub delete: bool,
    pub upload: bool,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub id: String,
    pub username: String,
    pub r#type: String,
    pub permissions: MePermissions,
    #[serde(rename = "librariesAccessible")]
    pub libraries_accessible: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct LibraryFolder {
    pub id: String,
    #[serde(rename = "fullPath")]
    pub full_path: String,
}

#[derive(Debug, Serialize)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub folders: Vec<LibraryFolder>,
    #[serde(rename = "mediaType")]
    pub media_type: String,
}

#[derive(Debug, Serialize)]
pub struct LibrariesResponse {
    pub libraries: Vec<Library>,
}

#[derive(Debug, Serialize)]
pub struct LibraryItemsResponse {
    pub results: Vec<LibraryItem>,
    pub total: u64,
    pub limit: u64,
    pub page: u64,
}

#[derive(Debug, Serialize)]
pub struct LibraryItem {
    pub id: String,
    #[serde(rename = "libraryId")]
    pub library_id: String,
    pub media: Media,
    /// Listen This treats `isMissing` / `isInvalid` as discard flags.
    /// Shelf knows the m4b path on disk; we expose the truth so the
    /// client doesn't show ghost items.
    #[serde(rename = "isMissing")]
    pub is_missing: bool,
    #[serde(rename = "isInvalid")]
    pub is_invalid: bool,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    /// `false` because every scribe item is a folder containing one
    /// m4b — matches ABS's behavior for folder-based libraries.
    #[serde(rename = "isFile")]
    pub is_file: bool,
    /// Unix milliseconds. ABS clients (including Listen This) decode
    /// these as non-optional Int64 and fail the entire response when
    /// either is missing. Sourced from books.first_seen_at × 1000.
    #[serde(rename = "addedAt")]
    pub added_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
}

#[derive(Debug, Serialize)]
pub struct Media {
    pub metadata: Metadata,
    #[serde(rename = "coverPath")]
    pub cover_path: Option<String>,
    pub tracks: Vec<Track>,
    pub chapters: Vec<Chapter>,
    pub duration: f64,
    pub size: u64,
}

#[derive(Debug, Serialize)]
pub struct Metadata {
    pub title: String,
    #[serde(rename = "titleIgnorePrefix")]
    pub title_ignore_prefix: String,
    pub subtitle: Option<String>,
    /// Author list. Each entry has an opaque `id` and a `name`.
    pub authors: Vec<NamedRef>,
    pub narrators: Vec<String>,
    /// Comma-joined author names. ABS's older API surfaced this as a
    /// single string and Listen This still decodes it that way; some
    /// clients ignore the structured `authors` array entirely. Cheap to
    /// emit both.
    #[serde(rename = "authorName")]
    pub author_name: Option<String>,
    #[serde(rename = "narratorName")]
    pub narrator_name: Option<String>,
    pub series: Vec<SeriesRef>,
    /// Comma-joined series name(s). Same compatibility rationale as
    /// `authorName` — older ABS clients read this single-string form.
    #[serde(rename = "seriesName")]
    pub series_name: Option<String>,
    pub genres: Vec<String>,
    #[serde(rename = "publishedYear")]
    pub published_year: Option<String>,
    pub description: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct NamedRef {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct SeriesRef {
    pub id: String,
    pub name: String,
    pub sequence: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct Track {
    pub index: u32,
    /// Inode-like identifier used by Listen This in the
    /// `/api/items/{id}/file/{ino}` URL path. Stable per ASIN.
    pub ino: String,
    pub title: String,
    /// Relative URL the client uses to fetch the audio. Matches what
    /// ABS emits — `Listen This` parses it and appends the server
    /// base + auth.
    #[serde(rename = "contentUrl")]
    pub content_url: String,
    pub duration: f64,
    #[serde(rename = "startOffset")]
    pub start_offset: f64,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[derive(Debug, Serialize)]
pub struct Chapter {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub title: String,
}
