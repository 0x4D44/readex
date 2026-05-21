//! Module-level settings — verbatim port of `htmldate/settings.py`.
//!
//! Every constant below cites the exact source line in `htmldate/settings.py`.
//! The Python file is short (~41 LOC including the trailing comment) so this
//! port is small by construction.
//!
//! # Date-typing note
//!
//! Python's `MIN_DATE = datetime.datetime(1995, 1, 1)` is a `datetime.datetime`.
//! `chrono` is **not** a current dependency of this crate (see
//! `Cargo.toml`'s `[dependencies]` block), so per the M4 Stage 1 brief
//! ("if `chrono` is already a dep, otherwise a simple `(i32, u32, u32)` tuple
//! constant"), `MIN_DATE` is exposed as a `(year, month, day)` tuple. Sub-stage
//! B's date-parsing layer is the natural place to grow a richer date type if
//! the algorithm ever needs hour/minute/second precision; the Python source's
//! sole use of `MIN_DATE` is as a calendar-date lower bound, so the tuple is
//! semantically sufficient for now.

/// Function cache size used by the `lru_cache` decorators in `htmldate`.
///
/// Ports `htmldate/settings.py:9` (`CACHE_SIZE: int = 8192`).
pub const CACHE_SIZE: usize = 8192;

/// Maximum acceptable HTML file size in bytes (downloads above this are
/// rejected by `htmldate.utils.is_wrong_document`).
///
/// Ports `htmldate/settings.py:12` (`MAX_FILE_SIZE: int = 20000000`).
pub const MAX_FILE_SIZE: usize = 20_000_000;

/// Earliest plausible date considered by the extractor (inclusive).
///
/// Encoded as a `(year, month, day)` tuple because `chrono` is not a crate
/// dependency (see the module-level "Date-typing note"). Ports
/// `htmldate/settings.py:16` (`MIN_DATE: datetime = datetime(1995, 1, 1)`).
pub const MIN_DATE: (i32, u32, u32) = (1995, 1, 1);

/// Upper limit on the number of date candidates the extractor will consider.
///
/// Ports `htmldate/settings.py:19` (`MAX_POSSIBLE_CANDIDATES: int = 1000`).
pub const MAX_POSSIBLE_CANDIDATES: usize = 1000;

/// Tag names that the HTML pre-cleaner strips out before date extraction.
///
/// Ports `htmldate/settings.py:21-40` (`CLEANING_LIST = [...]`). Order is
/// preserved verbatim from the Python source.
pub const CLEANING_LIST: &[&str] = &[
    "applet", "audio", "canvas", "datalist", "embed", "frame", "frameset", "iframe", "label",
    "map", "math", "noframes", "object", "picture", "rdf", "svg", "track", "video",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports `htmldate/settings.py:16` — pin `MIN_DATE` to Python's
    /// `datetime(1995, 1, 1)`.
    #[test]
    fn min_date_matches_python() {
        assert_eq!(MIN_DATE, (1995, 1, 1));
    }

    /// Ports `htmldate/settings.py:9` — pin `CACHE_SIZE` to Python's `8192`.
    #[test]
    fn cache_size_matches_python() {
        assert_eq!(CACHE_SIZE, 8192);
    }

    /// Ports `htmldate/settings.py:12` — pin `MAX_FILE_SIZE` to Python's
    /// `20000000`.
    #[test]
    fn max_file_size_matches_python() {
        assert_eq!(MAX_FILE_SIZE, 20_000_000);
    }

    /// Ports `htmldate/settings.py:19` — pin `MAX_POSSIBLE_CANDIDATES` to
    /// Python's `1000`.
    #[test]
    fn max_possible_candidates_matches_python() {
        assert_eq!(MAX_POSSIBLE_CANDIDATES, 1000);
    }

    /// Ports `htmldate/settings.py:21-40` — pin `CLEANING_LIST` to Python's
    /// 18-element list in source order.
    #[test]
    fn cleaning_list_matches_python() {
        assert_eq!(CLEANING_LIST.len(), 18);
        assert_eq!(
            CLEANING_LIST,
            &[
                "applet", "audio", "canvas", "datalist", "embed", "frame", "frameset", "iframe",
                "label", "map", "math", "noframes", "object", "picture", "rdf", "svg", "track",
                "video",
            ]
        );
    }
}
