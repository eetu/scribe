//! Small shared helpers.

use chrono::{DateTime, SecondsFormat, Utc};

/// Current time as a canonical ISO 8601 / RFC 3339 UTC string with seconds
/// precision and a `Z` suffix, e.g. `2024-04-01T12:30:00Z`.
///
/// This is the on-disk format for every timestamp column. Fixed width +
/// UTC `Z` means lexical ordering equals chronological ordering, so SQLite
/// `ORDER BY` and JS `localeCompare` both stay correct without parsing.
pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Render a `DateTime<Utc>` in the same canonical format.
pub fn to_iso(dt: DateTime<Utc>) -> String {
    dt.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// Parse an RFC 3339 timestamp to a unix epoch (seconds). `None` if it
/// doesn't parse. Used where a numeric comparison is cheaper than string
/// handling (e.g. re-purchase vs removal).
pub fn parse_iso(s: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_iso_is_canonical_z() {
        let s = now_iso();
        // 2024-04-01T12:30:00Z — 20 chars, ends in Z, no fractional seconds.
        assert_eq!(s.len(), 20, "{s}");
        assert!(s.ends_with('Z'), "{s}");
        assert!(!s.contains('.'), "{s}");
    }

    #[test]
    fn parse_round_trips() {
        let dt = DateTime::parse_from_rfc3339("2024-04-01T00:00:00Z").unwrap();
        let iso = to_iso(dt.into());
        assert_eq!(iso, "2024-04-01T00:00:00Z");
        assert_eq!(parse_iso(&iso), Some(dt.timestamp()));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert_eq!(parse_iso("not a date"), None);
    }

    #[test]
    fn lexical_order_matches_chronological() {
        let a = "2024-04-01T00:00:00Z";
        let b = "2024-12-31T23:59:59Z";
        assert!(a < b);
    }
}
