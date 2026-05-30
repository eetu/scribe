use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::abs;
use crate::auth::{bearer_guard, bearer_guard_stream};
use crate::error::{ShelfError, ShelfResult};
use crate::state::ShelfState;

pub fn router(state: ShelfState) -> Router {
    // Cover bytes get their own unauthenticated route. Listen This (and
    // typical ABS web clients) render the thumbnail through a plain
    // <img>/AsyncImage which doesn't carry the bearer header; serving
    // covers behind auth made every thumbnail 401. The bytes themselves
    // are not sensitive — they're already publicly fetchable from
    // Audible's CDN, shelf just proxies + caches them. Library content
    // (m4b streams) stays bearer-gated.
    let unauth = Router::new()
        .route("/api/items/{id}/cover", get(item_cover))
        .with_state(state.clone());
    // The audio stream is the one route AVFoundation / a plain <audio> src
    // can only auth via ?token= (no custom header on a media URL), so it
    // gets the stream guard. Everything else is header-only — keeps the
    // long-lived key out of access/proxy logs for routine API calls.
    let stream = Router::new()
        .route("/api/items/{id}/file/{ino}", get(item_file))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            bearer_guard_stream,
        ))
        .with_state(state.clone());
    let protected = Router::new()
        .route("/api/me", get(me))
        .route("/api/libraries", get(libraries))
        .route("/api/libraries/{id}/items", get(library_items))
        .route("/api/items/{id}", get(item_detail))
        .route_layer(middleware::from_fn_with_state(state.clone(), bearer_guard))
        .with_state(state.clone());
    Router::new()
        .route("/ping", get(ping))
        .merge(unauth)
        .merge(stream)
        .merge(protected)
}

async fn ping() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true, "service": "scribe-shelf" }))
}

async fn me(State(state): State<ShelfState>) -> Json<abs::MeResponse> {
    let now = chrono::Utc::now().timestamp_millis();
    Json(abs::MeResponse {
        id: "shelf-user".into(),
        username: "scribe".into(),
        r#type: "user".into(),
        permissions: abs::MePermissions {
            access_all_libraries: 1,
            access_all_tags: 1,
            access_explicit_content: 1,
            download: true,
            update: false,
            delete: false,
            upload: false,
        },
        libraries_accessible: vec![library_id(&state.cfg.library_name)],
        item_tags_accessible: Vec::new(),
        is_active: true,
        is_locked: false,
        last_seen: now,
        created_at: now,
    })
}

async fn libraries(State(state): State<ShelfState>) -> Json<abs::LibrariesResponse> {
    let lib_id = library_id(&state.cfg.library_name);
    let folder_id = format!("{lib_id}-root");
    Json(abs::LibrariesResponse {
        libraries: vec![abs::Library {
            id: lib_id.clone(),
            name: state.cfg.library_name.clone(),
            folders: vec![abs::LibraryFolder {
                id: folder_id,
                full_path: state.cfg.library_dir.display().to_string(),
                library_id: lib_id,
                added_at: 0,
            }],
            display_order: 1,
            icon: "audiobookshelf".into(),
            media_type: "book".into(),
            provider: "audible".into(),
        }],
    })
}

#[derive(Debug, Deserialize)]
struct ItemsQuery {
    #[serde(default = "default_limit")]
    limit: u64,
    #[serde(default)]
    page: u64,
    #[serde(default)]
    search: Option<String>,
}

fn default_limit() -> u64 {
    // ABS semantics: a missing (or 0) limit means "return everything",
    // not a page of 50. Clients like Listen This fetch the whole library
    // in one call and page client-side; capping here silently truncated
    // the shelf to the first 50 done books.
    0
}

async fn library_items(
    State(state): State<ShelfState>,
    Path(id): Path<String>,
    Query(q): Query<ItemsQuery>,
) -> ShelfResult<Json<abs::LibraryItemsResponse>> {
    if id != library_id(&state.cfg.library_name) {
        return Err(ShelfError::NotFound);
    }
    let offset = q.page.saturating_mul(q.limit);
    let library_dir = state.cfg.library_dir.clone();
    let library_id_owned = id.clone();
    let search = q.search.clone();
    // limit==0 → unbounded. SQLite reads `LIMIT -1` as no cap (OFFSET
    // still honoured), so we keep one parameterised query for both cases.
    let sql_limit: i64 = if q.limit == 0 { -1 } else { q.limit as i64 };
    let books: Vec<BookRow> = state
        .db
        .with(move |c| {
            let (sql, where_param) = match &search {
                Some(s) if !s.trim().is_empty() => (
                    "SELECT b.asin, b.account_id, b.title, b.subtitle, b.authors_json,
                            b.narrators_json, b.series_title, b.series_sequence,
                            b.runtime_length_ms, b.cover_url, b.purchase_date,
                            b.first_seen_at,
                            j.m4b_path, j.aaxc_path, j.status, b.chapters_json
                     FROM books b
                     INNER JOIN (
                       SELECT asin, account_id, m4b_path, aaxc_path, status,
                              MAX(updated_at) AS up
                       FROM jobs
                       WHERE status = 'done' AND m4b_path IS NOT NULL
                       GROUP BY asin, account_id
                     ) j ON j.asin = b.asin AND j.account_id = b.account_id
                     WHERE (lower(b.title) LIKE ?1
                        OR lower(b.authors_json) LIKE ?1)
                     ORDER BY b.title COLLATE NOCASE ASC
                     LIMIT ?2 OFFSET ?3",
                    Some(format!("%{}%", s.to_lowercase())),
                ),
                _ => (
                    "SELECT b.asin, b.account_id, b.title, b.subtitle, b.authors_json,
                            b.narrators_json, b.series_title, b.series_sequence,
                            b.runtime_length_ms, b.cover_url, b.purchase_date,
                            b.first_seen_at,
                            j.m4b_path, j.aaxc_path, j.status, b.chapters_json
                     FROM books b
                     INNER JOIN (
                       SELECT asin, account_id, m4b_path, aaxc_path, status,
                              MAX(updated_at) AS up
                       FROM jobs
                       WHERE status = 'done' AND m4b_path IS NOT NULL
                       GROUP BY asin, account_id
                     ) j ON j.asin = b.asin AND j.account_id = b.account_id
                     ORDER BY b.title COLLATE NOCASE ASC
                     LIMIT ?1 OFFSET ?2",
                    None,
                ),
            };
            let mut stmt = c.prepare(sql)?;
            let map = |r: &rusqlite::Row| {
                Ok(BookRow {
                    asin: r.get::<_, String>(0)?,
                    account_id: r.get::<_, String>(1)?,
                    title: r.get::<_, String>(2)?,
                    subtitle: r.get::<_, Option<String>>(3)?,
                    authors_json: r.get::<_, String>(4)?,
                    narrators_json: r.get::<_, String>(5)?,
                    series_title: r.get::<_, Option<String>>(6)?,
                    series_sequence: r.get::<_, Option<String>>(7)?,
                    runtime_length_ms: r.get::<_, Option<i64>>(8)?,
                    cover_url: r.get::<_, Option<String>>(9)?,
                    purchase_date: r.get::<_, Option<String>>(10)?,
                    first_seen_at: r.get::<_, i64>(11)?,
                    language: None,
                    m4b_path: r.get::<_, Option<String>>(12)?,
                    aaxc_path: r.get::<_, Option<String>>(13)?,
                    status: r.get::<_, Option<String>>(14)?,
                    chapters_json: r.get::<_, Option<String>>(15)?,
                })
            };
            let rows: Vec<BookRow> = if let Some(w) = where_param {
                stmt.query_map(rusqlite::params![w, sql_limit, offset as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            } else {
                stmt.query_map(rusqlite::params![sql_limit, offset as i64], map)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            Ok(rows)
        })
        .await?;
    let total: u64 = state
        .db
        .with(move |c| {
            // Match the items query — only count rows we'd actually
            // return. Stale unavailable/unconverted books are filtered
            // out at SQL level so pagination math stays honest.
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM books b
                 INNER JOIN (
                   SELECT asin, account_id FROM jobs
                   WHERE status = 'done' AND m4b_path IS NOT NULL
                   GROUP BY asin, account_id
                 ) j ON j.asin = b.asin AND j.account_id = b.account_id",
                [],
                |r| r.get(0),
            )?;
            Ok(n as u64)
        })
        .await?;

    let results = books
        .into_iter()
        .map(|b| build_item(&library_id_owned, &b, &library_dir, false))
        .collect();
    Ok(Json(abs::LibraryItemsResponse {
        results,
        total,
        limit: q.limit,
        page: q.page,
        sort_by: "media.metadata.title".into(),
        sort_desc: false,
        filter_by: q.search.unwrap_or_default(),
        media_type: "book".into(),
        minified: true,
        collapseseries: false,
        include: String::new(),
    }))
}

async fn item_detail(
    State(state): State<ShelfState>,
    Path(id): Path<String>,
    // `expanded=1` is the only query Listen This passes; we always
    // include the full track list regardless, so the param is ignored.
) -> ShelfResult<Json<abs::LibraryItem>> {
    let (asin, account_id) = parse_item_id(&id)?;
    let library_id = library_id(&state.cfg.library_name);
    let library_dir = state.cfg.library_dir.clone();
    let asin_q = asin.clone();
    let acc_q = account_id.clone();
    let row = state
        .db
        .with(move |c| {
            let mut stmt = c.prepare(
                "SELECT b.asin, b.account_id, b.title, b.subtitle, b.authors_json,
                        b.narrators_json, b.series_title, b.series_sequence,
                        b.runtime_length_ms, b.cover_url, b.purchase_date,
                        b.first_seen_at,
                        j.m4b_path, j.aaxc_path, j.status, b.chapters_json
                 FROM books b
                 LEFT JOIN (
                   SELECT asin, account_id, m4b_path, aaxc_path, status,
                          MAX(updated_at) AS up
                   FROM jobs GROUP BY asin, account_id
                 ) j ON j.asin = b.asin AND j.account_id = b.account_id
                 WHERE b.asin = ?1 AND b.account_id = ?2",
            )?;
            let r = stmt
                .query_row(rusqlite::params![asin_q, acc_q], |r| {
                    Ok(BookRow {
                        asin: r.get::<_, String>(0)?,
                        account_id: r.get::<_, String>(1)?,
                        title: r.get::<_, String>(2)?,
                        subtitle: r.get::<_, Option<String>>(3)?,
                        authors_json: r.get::<_, String>(4)?,
                        narrators_json: r.get::<_, String>(5)?,
                        series_title: r.get::<_, Option<String>>(6)?,
                        series_sequence: r.get::<_, Option<String>>(7)?,
                        runtime_length_ms: r.get::<_, Option<i64>>(8)?,
                        cover_url: r.get::<_, Option<String>>(9)?,
                        purchase_date: r.get::<_, Option<String>>(10)?,
                        first_seen_at: r.get::<_, i64>(11)?,
                        language: None,
                        m4b_path: r.get::<_, Option<String>>(12)?,
                        aaxc_path: r.get::<_, Option<String>>(13)?,
                        status: r.get::<_, Option<String>>(14)?,
                    chapters_json: r.get::<_, Option<String>>(15)?,
                    })
                })
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            Ok(r)
        })
        .await?
        .ok_or(ShelfError::NotFound)?;
    Ok(Json(build_item(&library_id, &row, &library_dir, true)))
}

async fn item_file(
    State(state): State<ShelfState>,
    Path((id, _ino)): Path<(String, String)>,
    headers: HeaderMap,
) -> ShelfResult<Response> {
    let (asin, account_id) = parse_item_id(&id)?;
    let asin_q = asin.clone();
    let acc_q = account_id.clone();
    let m4b: Option<String> = state
        .db
        .with(move |c| {
            let r: Option<String> = c
                .query_row(
                    "SELECT m4b_path FROM jobs WHERE asin = ?1 AND account_id = ?2
                     ORDER BY updated_at DESC LIMIT 1",
                    rusqlite::params![asin_q, acc_q],
                    |r| r.get::<_, Option<String>>(0),
                )
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            Ok(r)
        })
        .await?;
    let path = m4b.ok_or(ShelfError::NotFound)?;
    stream_file_with_range(&path, &headers, "audio/mp4").await
}

async fn item_cover(
    State(state): State<ShelfState>,
    Path(id): Path<String>,
) -> ShelfResult<Response> {
    let (asin, account_id) = parse_item_id(&id)?;

    // Prefer the on-disk cache scribe maintains ({asin}.{ext}); it keeps
    // working after Amazon pulls a title. Only proxy the CDN on a miss.
    if asin.bytes().all(|b| b.is_ascii_alphanumeric()) {
        for (ext, mime) in [
            ("jpg", "image/jpeg"),
            ("png", "image/png"),
            ("webp", "image/webp"),
            ("gif", "image/gif"),
        ] {
            let p = state.cfg.covers_dir.join(format!("{asin}.{ext}"));
            if let Ok(bytes) = tokio::fs::read(&p).await {
                return Ok((
                    [
                        (header::CONTENT_TYPE, mime.to_string()),
                        (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
                    ],
                    bytes,
                )
                    .into_response());
            }
        }
    }

    let asin_q = asin.clone();
    let acc_q = account_id.clone();
    let cover_url: Option<String> = state
        .db
        .with(move |c| {
            let r: Option<String> = c
                .query_row(
                    "SELECT cover_url FROM books WHERE asin = ?1 AND account_id = ?2",
                    rusqlite::params![asin_q, acc_q],
                    |r| r.get::<_, Option<String>>(0),
                )
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })?;
            Ok(r)
        })
        .await?;
    let url = cover_url.ok_or(ShelfError::NotFound)?;
    // SSRF guard: only fetch https Amazon/Audible CDN hosts. cover_url is
    // ingested from Audible, but this route is unauthenticated and proxies
    // it server-side, so don't let it reach internal hosts.
    if !cover_host_allowed(&url) {
        return Err(ShelfError::NotFound);
    }
    // Proxy the Audible CDN cover through the client's authenticated
    // session so the iOS app doesn't need its own CORS / referer dance.
    let resp = state
        .http
        .get(&url)
        .send()
        .await
        .map_err(|e| ShelfError::Internal(anyhow::anyhow!(e)))?;
    if !resp.status().is_success() {
        return Err(ShelfError::NotFound);
    }
    if let Some(len) = resp.content_length() {
        if len > MAX_COVER_BYTES as u64 {
            return Err(ShelfError::NotFound);
        }
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_string();
    // Stream with a hard cap so a missing/lying Content-Length can't OOM us.
    let mut stream = resp.bytes_stream();
    let mut bytes: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ShelfError::Internal(anyhow::anyhow!(e)))?;
        if bytes.len() + chunk.len() > MAX_COVER_BYTES {
            return Err(ShelfError::NotFound);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "public, max-age=86400".to_string()),
        ],
        bytes,
    )
        .into_response())
}

// ---------- helpers ----------

/// Hard ceiling on a proxied cover. Real covers are well under 1 MB.
const MAX_COVER_BYTES: usize = 16 * 1024 * 1024;

/// Amazon/Audible CDN hosts a cover may come from. The cover route is
/// unauthenticated and proxies `cover_url` server-side, so restrict it to
/// https on these hosts — otherwise it's an unauthenticated SSRF primitive
/// against loopback / link-local metadata / internal LAN.
const ALLOWED_COVER_HOSTS: &[&str] = &[
    "media-amazon.com",
    "ssl-images-amazon.com",
    "images-amazon.com",
    "amazon.com",
    "audible.com",
];

fn cover_host_allowed(raw: &str) -> bool {
    let Ok(u) = url::Url::parse(raw) else {
        return false;
    };
    if u.scheme() != "https" {
        return false;
    }
    let Some(host) = u.host_str() else {
        return false;
    };
    ALLOWED_COVER_HOSTS
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
struct BookRow {
    asin: String,
    account_id: String,
    title: String,
    subtitle: Option<String>,
    authors_json: String,
    narrators_json: String,
    series_title: Option<String>,
    series_sequence: Option<String>,
    runtime_length_ms: Option<i64>,
    /// Audible CDN cover URL. The /cover endpoint reads this directly
    /// from the DB rather than re-using the BookRow; keep the field so
    /// future inline-cover endpoints have what they need without an
    /// extra query.
    cover_url: Option<String>,
    purchase_date: Option<String>,
    /// Unix seconds; multiplied by 1000 to produce the addedAt /
    /// updatedAt fields ABS clients require. Sourced from
    /// books.first_seen_at — stable per (asin, account_id) pair.
    first_seen_at: i64,
    /// Audible language tag. Not currently stored in scribe's books
    /// table; left as None for now so the metadata response shape
    /// stays stable if/when scribe starts persisting it.
    language: Option<String>,
    m4b_path: Option<String>,
    aaxc_path: Option<String>,
    /// Most-recent job lifecycle status. Unused for now; ABS doesn't
    /// surface per-item job state, but kept for the inevitable future
    /// scribe-native endpoints that will.
    status: Option<String>,
    /// JSON array of `scribe_shared::Chapter` persisted by scribe, or
    /// None when not yet probed. Emitted as ABS `media.chapters`.
    chapters_json: Option<String>,
}

/// Parse the stored chapters JSON into ABS chapter objects (seconds).
fn parse_chapters(json: Option<&str>) -> Vec<abs::Chapter> {
    let Some(raw) = json else { return Vec::new() };
    let parsed: Vec<scribe_shared::Chapter> = serde_json::from_str(raw).unwrap_or_default();
    parsed
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let start = c.start_offset_ms as f64 / 1000.0;
            abs::Chapter {
                id: i as u32,
                start,
                end: start + c.length_ms as f64 / 1000.0,
                title: c.title,
            }
        })
        .collect()
}

fn library_id(name: &str) -> String {
    // Stable per-deployment id derived from the configured name. ABS uses
    // opaque strings — anything stable across boots works.
    format!("lib-{}", slugify(name))
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn item_id(book: &BookRow) -> String {
    // Single-key per-row id — Listen This treats this as opaque, so
    // `<account>:<asin>` keeps regional duplicates distinct.
    format!("{}:{}", book.account_id, book.asin)
}

fn parse_item_id(s: &str) -> ShelfResult<(String, String)> {
    let (account, asin) = s
        .split_once(':')
        .ok_or_else(|| ShelfError::BadRequest("item id must be <account>:<asin>".into()))?;
    Ok((asin.to_string(), account.to_string()))
}

fn build_item(
    library_id: &str,
    b: &BookRow,
    library_dir: &std::path::Path,
    include_tracks: bool,
) -> abs::LibraryItem {
    let id = item_id(b);
    let authors: Vec<String> = serde_json::from_str(&b.authors_json).unwrap_or_default();
    let narrators: Vec<String> = serde_json::from_str(&b.narrators_json).unwrap_or_default();
    let duration_sec = b.runtime_length_ms.unwrap_or(0) as f64 / 1000.0;
    let chapters = parse_chapters(b.chapters_json.as_deref());
    let m4b_present = b
        .m4b_path
        .as_deref()
        .map(|p| std::path::Path::new(p).is_file())
        .unwrap_or(false);
    // coverPath is a presence sentinel — clients gate the /cover fetch
    // on this being non-null. Actual bytes come from the proxy
    // endpoint, not from disk.
    let cover_path: Option<String> = b
        .cover_url
        .as_deref()
        .map(|_| format!("/api/items/{}/cover", id));
    let size = b
        .m4b_path
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0);

    let added_ms = b.first_seen_at.saturating_mul(1000);
    let ino = item_ino(&b.account_id, &b.asin);
    let folder_id = format!("{library_id}-root");

    // Synthesise filesystem-style paths for the LibraryItem fields ABS
    // expects. relPath is `<Author>/<Title>`, path is library_dir +
    // relPath. Both are informational — clients can't read them
    // directly anyway since they live on the server.
    let rel_path = match (authors.first(), b.series_title.as_deref()) {
        (Some(author), Some(series)) => format!("{}/{}/{}", author, series, b.title),
        (Some(author), None) => format!("{}/{}", author, b.title),
        (None, _) => b.title.clone(),
    };
    let path = library_dir.join(&rel_path).display().to_string();

    let m4b_filename = b
        .m4b_path
        .as_deref()
        .and_then(|p| std::path::Path::new(p).file_name())
        .and_then(|s| s.to_str())
        .unwrap_or("audiobook.m4b")
        .to_string();
    let m4b_rel = format!("{}/{}", rel_path, m4b_filename);

    let track_ino_s = track_ino(&b.asin);
    let audio_meta = abs::AudioFileMetadata {
        filename: m4b_filename.clone(),
        ext: ".m4b".into(),
        path: b.m4b_path.clone().unwrap_or_default(),
        rel_path: m4b_rel.clone(),
        size,
        mtime_ms: added_ms,
        ctime_ms: added_ms,
        birthtime_ms: added_ms,
    };

    let tracks = if include_tracks && m4b_present {
        vec![abs::Track {
            index: 1,
            ino: track_ino_s.clone(),
            title: b.title.clone(),
            content_url: format!("/api/items/{}/file/{}", id, track_ino_s),
            duration: duration_sec,
            start_offset: 0.0,
            mime_type: "audio/mp4".into(),
            metadata: audio_meta.clone(),
        }]
    } else {
        Vec::new()
    };
    let audio_files = if include_tracks && m4b_present {
        vec![abs::AudioFile {
            index: 1,
            ino: track_ino_s.clone(),
            metadata: audio_meta,
            added_at: added_ms,
            updated_at: added_ms,
            duration: duration_sec,
            mime_type: "audio/mp4".into(),
            codec: Some("aac".into()),
            format: Some("MPEG-4".into()),
            bit_rate: None,
            channels: Some(2),
            error: None,
            exclude: false,
            embedded_cover_art: None,
            chapters: Vec::new(),
        }]
    } else {
        Vec::new()
    };

    let metadata = abs::Metadata {
        title: b.title.clone(),
        title_ignore_prefix: title_ignore_prefix(&b.title),
        subtitle: b.subtitle.clone(),
        authors: authors
            .iter()
            .map(|a| abs::NamedRef {
                id: format!("author-{}", slugify(a)),
                name: a.clone(),
            })
            .collect(),
        author_name: if authors.is_empty() {
            None
        } else {
            Some(authors.join(", "))
        },
        author_name_lf: if authors.is_empty() {
            None
        } else {
            Some(
                authors
                    .iter()
                    .map(|a| last_first(a))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        },
        narrators: narrators.clone(),
        narrator_name: if narrators.is_empty() {
            None
        } else {
            Some(narrators.join(", "))
        },
        series: b
            .series_title
            .as_ref()
            .map(|s| {
                vec![abs::SeriesRef {
                    id: format!("series-{}", slugify(s)),
                    name: s.clone(),
                    sequence: b.series_sequence.clone(),
                }]
            })
            .unwrap_or_default(),
        series_name: b.series_title.clone(),
        genres: Vec::new(),
        published_year: b
            .purchase_date
            .as_ref()
            .and_then(|d| d.split('-').next().map(|s| s.to_string())),
        published_date: b.purchase_date.clone(),
        publisher: None,
        description: None,
        isbn: None,
        asin: Some(b.asin.clone()),
        language: b.language.clone(),
        explicit: false,
    };

    abs::LibraryItem {
        id: id.clone(),
        ino: ino.clone(),
        library_id: library_id.to_string(),
        folder_id,
        path,
        rel_path,
        is_file: false,
        mtime_ms: added_ms,
        ctime_ms: added_ms,
        birthtime_ms: added_ms,
        added_at: added_ms,
        updated_at: added_ms,
        is_missing: !m4b_present,
        is_invalid: false,
        media_type: "book".into(),
        size,
        num_files: if m4b_present { 1 } else { 0 },
        media: abs::Media {
            library_item_id: id,
            metadata,
            cover_path,
            tags: Vec::new(),
            audio_files,
            num_chapters: chapters.len() as u32,
            chapters,
            tracks,
            duration: duration_sec,
            size,
            num_tracks: if m4b_present { 1 } else { 0 },
            num_audio_files: if m4b_present { 1 } else { 0 },
            ebook_format: None,
            ebook_file: None,
        },
    }
}

fn item_ino(account_id: &str, asin: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    account_id.hash(&mut h);
    ":".hash(&mut h);
    asin.hash(&mut h);
    format!("item-{:016x}", h.finish())
}

/// Best-effort "Last, First" rendering from a "First Middle Last"
/// name. ABS uses this for surname sort indexes. Single-token names
/// (mononyms) are returned as-is.
fn last_first(name: &str) -> String {
    let trimmed = name.trim();
    let Some(last_space) = trimmed.rfind(' ') else {
        return trimmed.to_string();
    };
    let last = &trimmed[last_space + 1..];
    let rest = &trimmed[..last_space];
    format!("{last}, {rest}")
}

fn track_ino(asin: &str) -> String {
    // Stable per-ASIN identifier. Listen This treats it as opaque.
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    asin.hash(&mut h);
    format!("ino-{:016x}", h.finish())
}

fn title_ignore_prefix(title: &str) -> String {
    for prefix in ["A ", "An ", "The "] {
        if let Some(rest) = title.strip_prefix(prefix) {
            return rest.to_string();
        }
    }
    title.to_string()
}

async fn stream_file_with_range(
    path: &str,
    headers: &HeaderMap,
    content_type: &str,
) -> ShelfResult<Response> {
    let meta = tokio::fs::metadata(path).await.map_err(|_| ShelfError::NotFound)?;
    let total = meta.len();
    let range_header = headers
        .get(header::RANGE)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.to_string());
    let mut file = tokio::fs::File::open(path).await.map_err(|_| ShelfError::NotFound)?;
    if let Some(range) = range_header.as_deref() {
        if let Some((start, end)) = parse_byte_range(range, total) {
            file.seek(std::io::SeekFrom::Start(start)).await?;
            let len = end - start + 1;
            let limited = file.take(len);
            let body = Body::from_stream(ReaderStream::new(limited));
            return Ok((
                StatusCode::PARTIAL_CONTENT,
                [
                    (header::CONTENT_TYPE, content_type.to_string()),
                    (
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{total}"),
                    ),
                    (header::ACCEPT_RANGES, "bytes".to_string()),
                    (header::CONTENT_LENGTH, len.to_string()),
                ],
                body,
            )
                .into_response());
        }
    }
    let body = Body::from_stream(ReaderStream::new(file));
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_string()),
            (header::CONTENT_LENGTH, total.to_string()),
            (header::ACCEPT_RANGES, "bytes".to_string()),
        ],
        body,
    )
        .into_response())
}

fn parse_byte_range(value: &str, total: u64) -> Option<(u64, u64)> {
    let suffix = value.strip_prefix("bytes=")?;
    let (start_s, end_s) = suffix.split_once('-')?;
    let start: u64 = start_s.parse().ok()?;
    let end: u64 = if end_s.is_empty() {
        total.saturating_sub(1)
    } else {
        end_s.parse().ok()?
    };
    if start > end || end >= total {
        return None;
    }
    Some((start, end))
}
