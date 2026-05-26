//! Response shapes mirroring Audiobookshelf's REST API for the subset
//! that Listen This (and other ABS clients) consume. Field coverage
//! aims at ABS spec compliance for the endpoints we expose — see
//! https://api.audiobookshelf.org / the audiobookshelf-api-docs repo
//! for the canonical schema. Where scribe genuinely doesn't track a
//! field (publisher, ISBN, tags, etc) we emit null / empty rather than
//! fabricate values, so the JSON shape stays predictable for clients
//! that branch on optional-but-typed presence.

use serde::Serialize;

// ---------- /api/me ----------

#[derive(Debug, Serialize)]
pub struct MePermissions {
    #[serde(rename = "accessAllLibraries")]
    pub access_all_libraries: u8,
    #[serde(rename = "accessAllTags")]
    pub access_all_tags: u8,
    #[serde(rename = "accessExplicitContent")]
    pub access_explicit_content: u8,
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
    #[serde(rename = "itemTagsAccessible")]
    pub item_tags_accessible: Vec<String>,
    #[serde(rename = "isActive")]
    pub is_active: bool,
    #[serde(rename = "isLocked")]
    pub is_locked: bool,
    #[serde(rename = "lastSeen")]
    pub last_seen: i64,
    #[serde(rename = "createdAt")]
    pub created_at: i64,
}

// ---------- /api/libraries ----------

#[derive(Debug, Serialize)]
pub struct LibraryFolder {
    pub id: String,
    #[serde(rename = "fullPath")]
    pub full_path: String,
    #[serde(rename = "libraryId")]
    pub library_id: String,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
}

#[derive(Debug, Serialize)]
pub struct Library {
    pub id: String,
    pub name: String,
    pub folders: Vec<LibraryFolder>,
    #[serde(rename = "displayOrder")]
    pub display_order: u32,
    pub icon: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub provider: String,
}

#[derive(Debug, Serialize)]
pub struct LibrariesResponse {
    pub libraries: Vec<Library>,
}

// ---------- /api/libraries/{id}/items ----------

#[derive(Debug, Serialize)]
pub struct LibraryItemsResponse {
    pub results: Vec<LibraryItem>,
    pub total: u64,
    pub limit: u64,
    pub page: u64,
    #[serde(rename = "sortBy")]
    pub sort_by: String,
    #[serde(rename = "sortDesc")]
    pub sort_desc: bool,
    #[serde(rename = "filterBy")]
    pub filter_by: String,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub minified: bool,
    pub collapseseries: bool,
    pub include: String,
}

#[derive(Debug, Serialize)]
pub struct LibraryItem {
    pub id: String,
    /// Inode-equivalent — opaque stable identifier ABS uses for cache
    /// keys and watcher diffs. We derive it from the (account, asin)
    /// pair so it stays the same across re-converts.
    pub ino: String,
    #[serde(rename = "libraryId")]
    pub library_id: String,
    #[serde(rename = "folderId")]
    pub folder_id: String,
    pub path: String,
    #[serde(rename = "relPath")]
    pub rel_path: String,
    #[serde(rename = "isFile")]
    pub is_file: bool,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: i64,
    #[serde(rename = "ctimeMs")]
    pub ctime_ms: i64,
    #[serde(rename = "birthtimeMs")]
    pub birthtime_ms: i64,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    #[serde(rename = "isMissing")]
    pub is_missing: bool,
    #[serde(rename = "isInvalid")]
    pub is_invalid: bool,
    #[serde(rename = "mediaType")]
    pub media_type: String,
    pub media: Media,
    /// Top-level size in bytes — duplicated from media.size per the
    /// Library Item Minified + Expanded schemas.
    pub size: u64,
    /// Number of library files. Scribe stores exactly one m4b per
    /// item, so this is 1 when the file is on disk, 0 otherwise.
    #[serde(rename = "numFiles")]
    pub num_files: u32,
}

// ---------- Book ----------

#[derive(Debug, Serialize)]
pub struct Media {
    #[serde(rename = "libraryItemId")]
    pub library_item_id: String,
    pub metadata: Metadata,
    #[serde(rename = "coverPath")]
    pub cover_path: Option<String>,
    pub tags: Vec<String>,
    #[serde(rename = "audioFiles")]
    pub audio_files: Vec<AudioFile>,
    pub chapters: Vec<Chapter>,
    pub tracks: Vec<Track>,
    pub duration: f64,
    pub size: u64,
    /// Minified-shape counters. Listen This doesn't read them, but
    /// some ABS clients do.
    #[serde(rename = "numTracks")]
    pub num_tracks: u32,
    #[serde(rename = "numAudioFiles")]
    pub num_audio_files: u32,
    #[serde(rename = "numChapters")]
    pub num_chapters: u32,
    /// No ebook layer in scribe.
    #[serde(rename = "ebookFormat")]
    pub ebook_format: Option<String>,
    #[serde(rename = "ebookFile")]
    pub ebook_file: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct Metadata {
    pub title: String,
    #[serde(rename = "titleIgnorePrefix")]
    pub title_ignore_prefix: String,
    pub subtitle: Option<String>,
    pub authors: Vec<NamedRef>,
    pub narrators: Vec<String>,
    /// Comma-joined name (Book Metadata Expanded field).
    #[serde(rename = "authorName")]
    pub author_name: Option<String>,
    /// "Last, First; Last, First" form (Book Metadata Expanded field).
    /// Derived best-effort from the same name list — clients that care
    /// (UI sort by surname) read this instead of `authorName`.
    #[serde(rename = "authorNameLF")]
    pub author_name_lf: Option<String>,
    #[serde(rename = "narratorName")]
    pub narrator_name: Option<String>,
    pub series: Vec<SeriesRef>,
    #[serde(rename = "seriesName")]
    pub series_name: Option<String>,
    pub genres: Vec<String>,
    #[serde(rename = "publishedYear")]
    pub published_year: Option<String>,
    #[serde(rename = "publishedDate")]
    pub published_date: Option<String>,
    pub publisher: Option<String>,
    pub description: Option<String>,
    pub isbn: Option<String>,
    pub asin: Option<String>,
    pub language: Option<String>,
    pub explicit: bool,
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

// ---------- Track + AudioFile ----------

#[derive(Debug, Serialize)]
pub struct Track {
    pub index: u32,
    /// Inode-like identifier used in `/api/items/{id}/file/{ino}` URLs.
    /// Stable per ASIN.
    pub ino: String,
    pub title: String,
    #[serde(rename = "contentUrl")]
    pub content_url: String,
    pub duration: f64,
    #[serde(rename = "startOffset")]
    pub start_offset: f64,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub metadata: AudioFileMetadata,
}

#[derive(Debug, Serialize)]
pub struct AudioFile {
    pub index: u32,
    pub ino: String,
    pub metadata: AudioFileMetadata,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
    #[serde(rename = "updatedAt")]
    pub updated_at: i64,
    pub duration: f64,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
    pub codec: Option<String>,
    pub format: Option<String>,
    #[serde(rename = "bitRate")]
    pub bit_rate: Option<u64>,
    pub channels: Option<u32>,
    pub error: Option<String>,
    pub exclude: bool,
    #[serde(rename = "embeddedCoverArt")]
    pub embedded_cover_art: Option<String>,
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioFileMetadata {
    pub filename: String,
    pub ext: String,
    pub path: String,
    #[serde(rename = "relPath")]
    pub rel_path: String,
    pub size: u64,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: i64,
    #[serde(rename = "ctimeMs")]
    pub ctime_ms: i64,
    #[serde(rename = "birthtimeMs")]
    pub birthtime_ms: i64,
}

#[derive(Debug, Serialize)]
pub struct Chapter {
    pub id: u32,
    pub start: f64,
    pub end: f64,
    pub title: String,
}
