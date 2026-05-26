//! Small `htmldate.utils` ports — the bits sub-stages A + G need.
//!
//! Source of truth: `htmldate/utils.py`. Sub-stage A ported the `Extractor`
//! options class (lines 47-65) and the `trim_text` helper (lines 258-260).
//! Sub-stage G adds the `clean_html` tag stripper (lines 249-255).
//! The HTML-loading / fetch surface (`load_html`, `fetch_url`,
//! `isutf8`, `decode_file`, …) remains deferred — the Trafilatura caller
//! already hands `find_date` a pre-parsed DOM (no string/URL input path).

use super::settings::MIN_DATE;

use crate::readability::dom::{NodeRef, get_elements_by_tag_name, remove};

/// All extraction options for `htmldate.find_date` and its helpers.
///
/// Ports `htmldate/utils.py:47-65` (`class Extractor:` with `__slots__ =
/// ["extensive", "format", "max", "min", "original"]`). Field names match the
/// Python `__slots__` verbatim — note that the Python `__init__` *parameter*
/// names (`extensive_search`/`max_date`/`min_date`/`original_date`/
/// `outputformat`) differ from the *attribute* names; the attribute names are
/// what Python code reads off the instance and so are the names we port.
///
/// Python has **no defaults** in `Extractor.__init__` — every argument is
/// positional and required. Our `Default` impl is a Rust convenience for
/// tests; it picks the most defensible "no-op" values: empty format string,
/// `extensive = false`, `original = false`, `max` = today-equivalent (left as
/// the same `(1995, 1, 1)` tuple shape so the type is uniform), `min` =
/// `settings::MIN_DATE`. Callers that care should always populate via
/// `Extractor::new`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extractor {
    /// "Whether to apply extensive search heuristics."
    ///
    /// Ports `htmldate/utils.py:50` (`__slots__[0]`) and the assignment at
    /// `htmldate/utils.py:61` (`self.extensive: bool = extensive_search`).
    pub extensive: bool,

    /// Output date format string (Python `strftime` syntax, e.g. `"%Y-%m-%d"`).
    ///
    /// Ports `htmldate/utils.py:50` (`__slots__[1]`) and the assignment at
    /// `htmldate/utils.py:62` (`self.format: str = outputformat`).
    pub format: String,

    /// Upper bound on accepted dates (inclusive).
    ///
    /// Encoded as a `(year, month, day)` tuple matching `settings::MIN_DATE`'s
    /// shape (see `settings.rs`'s "Date-typing note" — `chrono` is not a
    /// crate dependency). Ports `htmldate/utils.py:50` (`__slots__[2]`) and
    /// the assignment at `htmldate/utils.py:63` (`self.max: datetime =
    /// max_date`).
    pub max: (i32, u32, u32),

    /// Lower bound on accepted dates (inclusive).
    ///
    /// Same date-shape rationale as `max`. Ports `htmldate/utils.py:50`
    /// (`__slots__[3]`) and the assignment at `htmldate/utils.py:64`
    /// (`self.min: datetime = min_date`).
    pub min: (i32, u32, u32),

    /// Whether to prefer the *original* publication date (vs the latest
    /// known modification date) when both are available.
    ///
    /// Ports `htmldate/utils.py:50` (`__slots__[4]`) and the assignment at
    /// `htmldate/utils.py:65` (`self.original: bool = original_date`).
    pub original: bool,
}

impl Extractor {
    /// Mirrors Python's `Extractor.__init__` positional-argument shape.
    ///
    /// Ports `htmldate/utils.py:53-65` — same five parameters in the same
    /// order, same field assignments.
    pub fn new(
        extensive_search: bool,
        max_date: (i32, u32, u32),
        min_date: (i32, u32, u32),
        original_date: bool,
        outputformat: String,
    ) -> Self {
        Self {
            extensive: extensive_search,
            format: outputformat,
            max: max_date,
            min: min_date,
            original: original_date,
        }
    }
}

impl Default for Extractor {
    /// Rust-side convenience — Python has no `Extractor` defaults. See the
    /// struct-level doc-comment for the rationale on each chosen value.
    fn default() -> Self {
        Self {
            extensive: false,
            format: String::new(),
            // `max` defaults to MIN_DATE's shape; sub-stage B's date logic will
            // overwrite with `datetime.now()`-equivalent before the extractor
            // runs. Picking MIN_DATE keeps the type uniform without inventing
            // a calendar value Python doesn't sanction.
            max: MIN_DATE,
            min: MIN_DATE,
            original: false,
        }
    }
}

/// Strip superfluous whitespace and normalize remaining whitespace.
///
/// Ports `htmldate/utils.py:258-260` verbatim:
///
/// ```python
/// def trim_text(string: str) -> str:
///     "Remove superfluous space and normalize remaining space."
///     return " ".join(string.split()).strip()
/// ```
///
/// Python's `str.split()` (no arg) splits on **any** run of whitespace
/// (`str.isspace()` characters) and discards empty fields, which is exactly
/// what Rust's `str::split_whitespace` does. The trailing `.strip()` is a
/// no-op after `" ".join(...)` (the join inserts single spaces between
/// non-empty fields and the input has no leading/trailing whitespace by
/// construction), but is preserved for byte-faithfulness.
pub fn trim_text(string: &str) -> String {
    let joined: String = string.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.trim().to_string()
}

/// Delete every descendant whose tag is in `cleaning_list`.
///
/// Ports `htmldate/utils.py:249-255` verbatim:
///
/// ```python
/// def clean_html(tree: HtmlElement, elemlist: List[str]) -> HtmlElement:
///     "Delete selected elements."
///     for element in tree.iter(elemlist):  # type: ignore[call-overload]
///         parent = element.getparent()
///         if parent is not None:
///             parent.remove(element)
///     return tree
/// ```
///
/// Python `tree.iter(elemlist)` walks every descendant whose tag name is in
/// `elemlist`. The Rust port iterates each tag name once via
/// `get_elements_by_tag_name` and removes the matches — equivalent up to a
/// minor ordering quirk (Python visits the tree in document order across
/// tags, the Rust port visits document order within each tag). The end state
/// (every node with a target tag removed, others untouched) is identical.
///
/// Returns nothing — the input tree is mutated in place, matching Python's
/// `parent.remove(element)` side effect. Python also returns `tree` itself,
/// but every caller (`core.find_date` at `core.py:914`) shadows the variable
/// rather than reading the return, so the void Rust signature is faithful.
pub fn clean_html(tree: &NodeRef, cleaning_list: &[&str]) {
    for tag in cleaning_list {
        for element in get_elements_by_tag_name(tree, tag) {
            // `parent.remove(element)` — `dom::remove` no-ops cleanly if the
            // element is already detached (mirroring Python's
            // `getparent() is not None` guard).
            remove(&element);
        }
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Ports `htmldate/utils.py:53-65` — `Extractor::new` mirrors the Python
    /// constructor exactly. Field assignments map parameter -> attribute.
    #[test]
    fn extractor_new_assigns_fields_like_python_init() {
        let e = Extractor::new(
            true,
            (2024, 12, 31),
            (2000, 1, 1),
            true,
            "%Y-%m-%d".to_string(),
        );
        assert!(e.extensive);
        assert_eq!(e.format, "%Y-%m-%d");
        assert_eq!(e.max, (2024, 12, 31));
        assert_eq!(e.min, (2000, 1, 1));
        assert!(e.original);
    }

    /// Pins the Rust-side `Default` impl values. Python's `Extractor.__init__`
    /// has no defaults at all, so this test asserts the *Rust* defaults the
    /// `Default` impl documents — NOT a Python-faithfulness claim.
    #[test]
    fn extractor_default_matches_documented_rust_defaults() {
        let e = Extractor::default();
        assert!(!e.extensive);
        assert!(e.format.is_empty());
        assert_eq!(e.max, MIN_DATE);
        assert_eq!(e.min, MIN_DATE);
        assert!(!e.original);
    }

    /// Ports `htmldate/utils.py:258-260` — `trim_text` strips leading +
    /// trailing whitespace and collapses interior runs to a single space.
    #[test]
    fn trim_text_strips_leading_and_trailing_whitespace() {
        assert_eq!(trim_text("   hello world   "), "hello world");
    }

    /// Ports `htmldate/utils.py:258-260` — interior single spaces are
    /// preserved; multi-space runs collapse to one.
    #[test]
    fn trim_text_collapses_interior_whitespace_runs() {
        // Tabs / newlines / multi-space all count as whitespace per
        // Python `str.split()` / Rust `split_whitespace`.
        assert_eq!(
            trim_text("a   b\t\tc\n\nd"),
            "a b c d",
            "all whitespace runs collapse to single space"
        );
    }

    /// Ports `htmldate/utils.py:258-260` — empty / all-whitespace inputs
    /// yield empty strings (Python: `" ".join([]).strip() == ""`).
    #[test]
    fn trim_text_handles_empty_and_whitespace_only_inputs() {
        assert_eq!(trim_text(""), "");
        assert_eq!(trim_text("   "), "");
        assert_eq!(trim_text("\t\n\r "), "");
    }

    /// Ports `htmldate/utils.py:249-255` — `clean_html` strips every
    /// descendant whose tag is in CLEANING_LIST.
    #[test]
    fn clean_html_removes_cleaning_list_tags() {
        use crate::htmldate::settings::CLEANING_LIST;
        use crate::readability::dom::{Dom, get_elements_by_tag_name};
        let dom = Dom::parse(
            r#"<html><body>
                <script>noisy</script>
                <p>keep</p>
                <svg>drop</svg>
                <iframe src="..."></iframe>
                <video>drop</video>
            </body></html>"#,
        );
        let body = dom.body().expect("body");
        // <script> isn't in CLEANING_LIST but svg/iframe/video are.
        clean_html(&body, CLEANING_LIST);
        // Verified-stripped tags from CLEANING_LIST.
        assert!(get_elements_by_tag_name(&body, "svg").is_empty());
        assert!(get_elements_by_tag_name(&body, "iframe").is_empty());
        assert!(get_elements_by_tag_name(&body, "video").is_empty());
        // <p> is not in CLEANING_LIST so it survives.
        assert_eq!(get_elements_by_tag_name(&body, "p").len(), 1);
    }
}
