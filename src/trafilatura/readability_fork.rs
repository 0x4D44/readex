//! `readability_fork` — Stage 4a: Trafilatura's internal fork of
//! readability-lxml, **data structures + scoring primitives only**.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.7
//! (Trafilatura's readability-lxml fork is the second-arm rescue extractor
//! in its cascade). Source of truth:
//! `trafilatura@v2.0.0/readability_lxml.py:1-350`.
//!
//! # Scope of this file (Stage 4a)
//!
//! This file ports **only** the small, self-contained, unit-testable
//! pieces of `readability_lxml.py` — the leaf primitives every later
//! sub-stage consumes. NO orchestration logic lands here.
//!
//! Functions / data ported (Python line ranges from `readability_lxml.py`):
//!
//! - Module-level constants (lines 42-84): `DIV_TO_P_ELEMS`, `DIV_SCORES`,
//!   `BLOCK_SCORES`, `BAD_ELEM_SCORES`, `STRUCTURE_SCORES`,
//!   `TEXT_CLEAN_ELEMS`, the `REGEXES` dict (UNLIKELY / OK_MAYBE /
//!   POSITIVE / NEGATIVE / DIV_TO_P_ELEMS / VIDEO), `FRAME_TAGS`,
//!   `LIST_TAGS`.
//! - `text_length(elem)` — readability_lxml.py:87-89.
//! - `Candidate` dataclass — readability_lxml.py:92-99.
//! - `class_weight(elem)` — readability_lxml.py:261-268.
//! - `score_node(elem)` — readability_lxml.py:270-282.
//! - `score_paragraph_text(text)` — the `1 + len(text.split(",")) +
//!   min((elem_text_len/100), 3)` scoring primitive from inside
//!   `score_paragraphs` (readability_lxml.py:245), lifted as a pure
//!   function so Stage 4b's orchestrator can call it.
//! - `link_density(elem)` — readability_lxml.py:220-223 (the body of
//!   `Document.get_link_density`, lifted to a free fn for Stage 4a's
//!   primitive surface; Stage 4b will call it from `Document` methods).
//!
//! # Sub-stage roadmap
//!
//! - **4a (this file):** Candidate + scoring primitives (≈150 LOC).
//! - 4b: `Document::summary()` core orchestration
//!   (`remove_unlikely_candidates` / `transform_misused_divs_into_paragraphs`
//!   / `score_paragraphs` / `select_best_candidate` / `get_article`).
//! - 4c: `sanitize` + the ruthless / lenient retry loop.
//! - 4d: `is_probably_readerable` + cascade integration into the M3
//!   arbiter.
//!
//! # Why a NEW module (not folded into `crate::readability`)
//!
//! `crate::readability` is the M2 port of **Mozilla Readability.js** —
//! a different algorithm with different scoring constants and a
//! different orchestration shape (flag-sieve loop with multiple
//! attempts). Trafilatura's `readability_lxml.py` is its **internal
//! fork** with single-shot scoring + ruthless/lenient retry. Folding
//! the two would silently couple scoring constants that the Python
//! sources keep distinct, so we keep them in two clearly-named
//! modules with the algorithm name in the path
//! (`trafilatura::readability_fork`).
//!
//! # Anti-inversion (HLD §4 / §10)
//!
//! Every constant and function header carries a `readability_lxml.py:NN`
//! source-line cite. The `REGEXES` patterns are byte-identical to the
//! Python source. Faithful regex compilation honours Python's
//! `re.compile(..., re.I)` flag via Rust's inline `(?i)` (utils.rs
//! Stage 2b' precedent uses raw lower-case patterns + ASCII inputs;
//! here we must respect the `re.I` flag because, e.g., the
//! `negativeRe` pattern is matched against `class`/`id` attribute
//! strings which may contain mixed case).

use std::sync::OnceLock;

use regex::Regex;

use crate::readability::dom::{NodeRef, class_name, get_elements_by_tag_name, id, local_name};
use crate::trafilatura::utils::trim;

// ===========================================================================
// Module constants (readability_lxml.py:42-84)
// ===========================================================================
//
// Some constants below are not consumed by Stage 4a (the scoring
// primitives), only by later sub-stages (4b orchestration, 4c
// `sanitize`). They are vendored here so the entire `readability_lxml.py`
// constant surface lives in one module with line-cited entries; the
// `#[allow(dead_code)]` annotations are intentional and will retire as
// Stage 4b/4c lands callers. See module header for the sub-stage
// roadmap.

/// `DIV_TO_P_ELEMS` — readability_lxml.py:42-53. The tag-name set of
/// block-level elements whose presence inside a `<div>` *prevents* that
/// `<div>` from being retagged to `<p>` (consumed by Stage 4b's
/// `transform_misused_divs_into_paragraphs`).
#[allow(dead_code)] // Stage 4b consumer
pub(crate) const DIV_TO_P_ELEMS: &[&str] = &[
    "a", "blockquote", "dl", "div", "img", "ol", "p", "pre", "table", "ul",
];

/// `DIV_SCORES` — readability_lxml.py:55. Tags that earn `+5` in
/// `score_node`. Stored as a slice for membership-test lookup (order is
/// irrelevant; the Python `set` makes this explicit).
pub(crate) const DIV_SCORES: &[&str] = &["div", "article"];

/// `BLOCK_SCORES` — readability_lxml.py:56. Tags that earn `+3`.
pub(crate) const BLOCK_SCORES: &[&str] = &["pre", "td", "blockquote"];

/// `BAD_ELEM_SCORES` — readability_lxml.py:57. Tags that earn `-3`.
pub(crate) const BAD_ELEM_SCORES: &[&str] =
    &["address", "ol", "ul", "dl", "dd", "dt", "li", "form", "aside"];

/// `STRUCTURE_SCORES` — readability_lxml.py:58. Tags that earn `-5`.
pub(crate) const STRUCTURE_SCORES: &[&str] =
    &["h1", "h2", "h3", "h4", "h5", "h6", "th", "header", "footer", "nav"];

/// `TEXT_CLEAN_ELEMS` — readability_lxml.py:60. Used by Stage 4c's
/// `sanitize` cleanup pass.
#[allow(dead_code)] // Stage 4c consumer
pub(crate) const TEXT_CLEAN_ELEMS: &[&str] = &["p", "img", "li", "a", "embed", "input"];

/// `FRAME_TAGS` — readability_lxml.py:82. Top-level container tags that
/// are *never* dropped by `remove_unlikely_candidates` (Stage 4b).
#[allow(dead_code)] // Stage 4b consumer
pub(crate) const FRAME_TAGS: &[&str] = &["body", "html"];

/// `LIST_TAGS` — readability_lxml.py:83. Consumed by Stage 4c's
/// `sanitize` list-pruning path.
#[allow(dead_code)] // Stage 4c consumer
pub(crate) const LIST_TAGS: &[&str] = &["ol", "ul"];

// ---------------------------------------------------------------------------
// REGEXES dict (readability_lxml.py:62-80)
// ---------------------------------------------------------------------------
//
// Python source compiles each pattern with `re.I` (case-insensitive). In
// Rust we honour the flag via an inline `(?i)` prefix on the pattern
// literal — `regex` interprets `(?i)` exactly like Python `re.I` for the
// ASCII alternations used here (no Unicode-case-fold edge cases in the
// vendored alternations).
//
// Each pattern's compile is lazily memoised through a `OnceLock<Regex>`,
// matching Stage 2b' (`utils.rs`)'s precedent. Public via `pub(crate)`
// accessors so Stage 4b orchestrators consume them through stable
// function calls (and so any future regex-engine swap is one edit).

/// `unlikelyCandidatesRe` — readability_lxml.py:63-66. Matches
/// class/id attribute substrings that indicate boilerplate (sidebars,
/// ads, comments, etc.). Consumed by Stage 4b's
/// `remove_unlikely_candidates`; exercised by a Stage 4a regex-literal
/// sanity test below.
#[allow(dead_code)] // Stage 4b consumer (production); test-only at Stage 4a
pub(crate) fn unlikely_candidates_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)combx|comment|community|disqus|extra|foot|header|menu|remark|rss|shoutbox|sidebar|sponsor|ad-break|agegate|pagination|pager|popup|tweet|twitter",
        )
        .expect("readability_lxml.py:63 unlikelyCandidatesRe compiles")
    })
}

/// `okMaybeItsACandidateRe` — readability_lxml.py:67. The exception
/// list: even if an attribute matched `unlikelyCandidatesRe`, an
/// `okMaybeItsACandidateRe` hit keeps the element alive.
#[allow(dead_code)] // Stage 4b consumer
pub(crate) fn ok_maybe_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)and|article|body|column|main|shadow")
            .expect("readability_lxml.py:67 okMaybeItsACandidateRe compiles")
    })
}

/// `positiveRe` — readability_lxml.py:68-71. Matches class/id
/// attributes that earn a `+25` weight bonus in [`class_weight`].
pub(crate) fn positive_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)article|body|content|entry|hentry|main|page|pagination|post|text|blog|story",
        )
        .expect("readability_lxml.py:68 positiveRe compiles")
    })
}

/// `negativeRe` — readability_lxml.py:72-75. Matches class/id
/// attributes that earn a `-25` weight penalty in [`class_weight`].
pub(crate) fn negative_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)button|combx|comment|com-|contact|figure|foot|footer|footnote|form|input|masthead|media|meta|outbrain|promo|related|scroll|shoutbox|sidebar|sponsor|shopping|tags|tool|widget",
        )
        .expect("readability_lxml.py:72 negativeRe compiles")
    })
}

/// `divToPElementsRe` — readability_lxml.py:76-78. Matches the
/// serialized HTML of a `<div>`'s children when deciding whether to
/// retag the `<div>` to `<p>`. Stage 4b's
/// `transform_misused_divs_into_paragraphs` consumes it.
#[allow(dead_code)] // Stage 4b consumer
pub(crate) fn div_to_p_elements_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)<(?:a|blockquote|dl|div|img|ol|p|pre|table|ul)")
            .expect("readability_lxml.py:76 divToPElementsRe compiles")
    })
}

/// `videoRe` — readability_lxml.py:79. Matches YouTube/Vimeo iframe
/// `src` URLs; Stage 4c's `sanitize` keeps these iframes (with
/// `text = "VIDEO"`) instead of dropping them. Exercised by a Stage 4a
/// regex-literal sanity test below.
#[allow(dead_code)] // Stage 4c consumer (production); test-only at Stage 4a
pub(crate) fn video_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)https?:\/\/(?:www\.)?(?:youtube|vimeo)\.com")
            .expect("readability_lxml.py:79 videoRe compiles")
    })
}

// ===========================================================================
// text_length (readability_lxml.py:87-89)
// ===========================================================================

/// `text_length(elem)` — readability_lxml.py:87-89.
///
/// ```python
/// def text_length(elem: HtmlElement) -> int:
///     "Return the length of the element with all its contents."
///     return len(trim(elem.text_content()))
/// ```
///
/// Python's `elem.text_content()` is lxml's concatenated descendant
/// text; `trim` collapses whitespace runs to single spaces and strips.
/// `len(...)` is the resulting *character* count (Python `str` is
/// codepoint-indexed). In Rust we use the dom facade's `text_content`
/// (jsdom-faithful, identical concatenation semantics — see
/// `dom.rs:187`) and `chars().count()` for the character count (Rust
/// `str::len` would return bytes, which diverges from Python on
/// non-ASCII content — non-faithful).
pub fn text_length(elem: &NodeRef) -> usize {
    let raw = crate::readability::dom::text_content(elem);
    trim(&raw).chars().count()
}

// ===========================================================================
// Candidate (readability_lxml.py:92-99)
// ===========================================================================

/// `Candidate` — readability_lxml.py:92-99.
///
/// ```python
/// class Candidate:
///     "Defines a class to score candidate elements."
///
///     __slots__ = ["score", "elem"]
///
///     def __init__(self, score: float, elem: HtmlElement) -> None:
///         self.score: float = score
///         self.elem: HtmlElement = elem
/// ```
///
/// A scored DOM-element pair. `Document::score_paragraphs` (Stage 4b)
/// produces a map of these keyed by element identity; the orchestrator
/// then sorts by `score` to pick the top candidate.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The accumulated readability score (mutable across
    /// `score_paragraphs`). `f64` faithful to Python `float`.
    pub score: f64,
    /// The element this score belongs to.
    pub elem: NodeRef,
}

impl Candidate {
    /// Convenience constructor — saves the `Candidate { score, elem }`
    /// struct-literal boilerplate at call sites. Equivalent to the
    /// Python `Candidate(score, elem)` invocation.
    pub fn new(score: f64, elem: NodeRef) -> Self {
        Self { score, elem }
    }
}

// ===========================================================================
// class_weight (readability_lxml.py:261-268)
// ===========================================================================

/// `class_weight(elem)` — readability_lxml.py:261-268.
///
/// ```python
/// def class_weight(self, elem: HtmlElement) -> float:
///     weight = 0
///     for attribute in filter(None, (elem.get("class"), elem.get("id"))):
///         if REGEXES["negativeRe"].search(attribute):
///             weight -= 25
///         if REGEXES["positiveRe"].search(attribute):
///             weight += 25
///     return weight
/// ```
///
/// Sums a `+25` / `-25` keyword-match weight over the element's `class`
/// and `id` attributes. Both penalty and bonus may fire on the same
/// attribute (Python uses two independent `if`s, not `elif`); we
/// preserve that.
///
/// `filter(None, ...)` drops `None` and empty strings; our dom-facade
/// `class_name` / `id` return `""` when absent, so we filter on `!is_empty`.
pub fn class_weight(elem: &NodeRef) -> f64 {
    let mut weight = 0.0_f64;
    let class = class_name(elem);
    let id_attr = id(elem);
    for attribute in [class.as_str(), id_attr.as_str()] {
        if attribute.is_empty() {
            continue;
        }
        if negative_re().is_match(attribute) {
            weight -= 25.0;
        }
        if positive_re().is_match(attribute) {
            weight += 25.0;
        }
    }
    weight
}

// ===========================================================================
// score_node (readability_lxml.py:270-282)
// ===========================================================================

/// `score_node(elem)` — readability_lxml.py:270-282.
///
/// ```python
/// def score_node(self, elem: HtmlElement) -> Candidate:
///     score = self.class_weight(elem)
///     tag = str(elem.tag)
///     name = tag.lower()
///     if name in DIV_SCORES:
///         score += 5
///     elif name in BLOCK_SCORES:
///         score += 3
///     elif name in BAD_ELEM_SCORES:
///         score -= 3
///     elif name in STRUCTURE_SCORES:
///         score -= 5
///     return Candidate(score, elem)
/// ```
///
/// Produces a fresh [`Candidate`] for `elem`, baseline-scored from its
/// class/id keywords (`class_weight`) plus a tag-based adjustment from
/// one of the four tables. The `if`/`elif` chain is **exclusive** — at
/// most one table applies — and Python's tag names are compared
/// case-insensitively (`tag.lower()`). The dom facade's `local_name`
/// already lower-cases (html5ever stores local names lower-case at
/// parse), so the comparison is direct.
///
/// Non-element nodes have no `local_name`; their tag adjustment is
/// `0.0` and only the class/id weight applies. (Python would `str(None)
/// == "None"` and miss every table — also `0.0` net.)
pub fn score_node(elem: &NodeRef) -> Candidate {
    let mut score = class_weight(elem);
    if let Some(name) = local_name(elem) {
        let n = name.as_str();
        if DIV_SCORES.contains(&n) {
            score += 5.0;
        } else if BLOCK_SCORES.contains(&n) {
            score += 3.0;
        } else if BAD_ELEM_SCORES.contains(&n) {
            score -= 3.0;
        } else if STRUCTURE_SCORES.contains(&n) {
            score -= 5.0;
        }
    }
    Candidate::new(score, elem.clone())
}

// ===========================================================================
// score_paragraph_text (readability_lxml.py:245)
// ===========================================================================

/// `score_paragraph_text(text)` — the paragraph-text scoring primitive
/// at readability_lxml.py:245:
///
/// ```python
/// score = 1 + len(elem_text.split(",")) + min((elem_text_len / 100), 3)
/// ```
///
/// Where `elem_text = trim(elem.text_content())` and `elem_text_len =
/// len(elem_text)`. Lifted to a pure function so Stage 4b's
/// `score_paragraphs` orchestrator calls one named primitive instead of
/// in-lining the formula.
///
/// Breakdown:
/// - `1` — the base score.
/// - `len(elem_text.split(","))` — `split(",")` on an empty string
///   returns `[""]` (length 1), so the minimum contribution is `1` and
///   the term is "comma-clause count plus one". A 5-comma paragraph
///   contributes `6`.
/// - `min(elem_text_len / 100, 3)` — character-count bonus, capped at
///   `3` (so a paragraph of 300+ chars stops growing).
///
/// The Python `elem_text` is already-trimmed text; we trim here too so
/// callers can pass raw `text_content`. The character count is
/// `chars().count()` (Unicode-codepoint) for parity with Python `len(str)`.
pub fn score_paragraph_text(text: &str) -> f64 {
    let trimmed = trim(text);
    let text_len = trimmed.chars().count();
    let comma_term = trimmed.split(',').count() as f64;
    let length_term = (text_len as f64 / 100.0).min(3.0);
    1.0 + comma_term + length_term
}

// ===========================================================================
// link_density (readability_lxml.py:220-223)
// ===========================================================================

/// `link_density(elem)` — readability_lxml.py:220-223.
///
/// ```python
/// def get_link_density(self, elem: HtmlElement) -> float:
///     total_length = text_length(elem) or 1
///     link_length = sum(text_length(link) for link in elem.findall(".//a"))
///     return link_length / total_length
/// ```
///
/// Ratio of `<a>`-descendant text length to total element text length.
/// The `or 1` guard avoids division-by-zero when the element has no
/// text (we replicate it via `.max(1)`). `findall(".//a")` is
/// descendant-only `<a>` elements in document order — our dom facade
/// `get_elements_by_tag_name(_, "a")` matches that contract exactly
/// (`dom.rs:716-719`'s docstring confirms descendants only).
///
/// Free-fn form (the Python is a `Document` method that doesn't
/// actually read any `Document` state — pure over `elem`). Stage 4b
/// calls this from `Document::score_paragraphs` and
/// `Document::get_article`.
pub fn link_density(elem: &NodeRef) -> f64 {
    let total = text_length(elem).max(1);
    let link_total: usize = get_elements_by_tag_name(elem, "a")
        .iter()
        .map(text_length)
        .sum();
    link_total as f64 / total as f64
}

// ===========================================================================
// Unit tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::Dom;

    /// Helper: parse a minimal HTML fragment and return `(dom, root)`
    /// where `root` is the first element child of `<body>`. The `Dom`
    /// is returned so its `Drop` does not iteratively drain descendant
    /// children Vecs while the test still holds NodeRefs — same pattern
    /// the `_extract_probe_body_xpath_evaluation` test pins (the rcdom
    /// Drop quirk M3 Stage 2d called out).
    fn parse_body_first_child(html: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let body = dom.body().expect("body parsed");
        let child = crate::readability::dom::first_element_child(&body)
            .expect("body has at least one element child");
        (dom, child)
    }

    // -----------------------------------------------------------------------
    // class_weight (readability_lxml.py:261-268)
    // -----------------------------------------------------------------------

    #[test]
    fn class_weight_positive_id() {
        // id="main-article" matches positiveRe (`article|main`)
        let (_dom, root) = parse_body_first_child("<div id=\"main-article\"></div>");
        // Two positive hits (both `main` and `article` in the id),
        // but only ONE `+25` fires per attribute (Python is one
        // search-per-attribute, not per-keyword). Class is absent so
        // no second attribute contributes.
        assert_eq!(class_weight(&root), 25.0);
    }

    #[test]
    fn class_weight_negative_class() {
        // class="comment-footer" matches negativeRe.
        let (_dom, root) = parse_body_first_child("<div class=\"comment-footer\"></div>");
        assert_eq!(class_weight(&root), -25.0);
    }

    #[test]
    fn class_weight_neutral() {
        // Nothing matches either regex → 0.
        let (_dom, root) = parse_body_first_child("<div class=\"plain\" id=\"qqq\"></div>");
        assert_eq!(class_weight(&root), 0.0);
    }

    #[test]
    fn class_weight_both_positive_and_negative_on_same_attr() {
        // "article-sidebar" hits BOTH positive (article) and negative
        // (sidebar) — Python's two independent `if`s sum to 0.
        let (_dom, root) = parse_body_first_child("<div class=\"article-sidebar\"></div>");
        assert_eq!(class_weight(&root), 0.0);
    }

    // -----------------------------------------------------------------------
    // score_node (readability_lxml.py:270-282)
    // -----------------------------------------------------------------------

    #[test]
    fn score_node_div_gets_5pts() {
        let (_dom, root) = parse_body_first_child("<div></div>");
        let c = score_node(&root);
        assert_eq!(c.score, 5.0);
    }

    #[test]
    fn score_node_blockquote_gets_3pts() {
        let (_dom, root) = parse_body_first_child("<blockquote></blockquote>");
        let c = score_node(&root);
        assert_eq!(c.score, 3.0);
    }

    #[test]
    fn score_node_bad_elem_neg3() {
        let (_dom, root) = parse_body_first_child("<ul></ul>");
        let c = score_node(&root);
        assert_eq!(c.score, -3.0);
    }

    #[test]
    fn score_node_structure_neg5() {
        let (_dom, root) = parse_body_first_child("<h1></h1>");
        let c = score_node(&root);
        assert_eq!(c.score, -5.0);
    }

    #[test]
    fn score_node_combines_class_weight_and_tag() {
        // <div class="article"> → +25 (positiveRe) + 5 (DIV_SCORES) = 30.
        let (_dom, root) = parse_body_first_child("<div class=\"article\"></div>");
        let c = score_node(&root);
        assert_eq!(c.score, 30.0);
    }

    // -----------------------------------------------------------------------
    // score_paragraph_text (readability_lxml.py:245)
    // -----------------------------------------------------------------------

    #[test]
    fn score_paragraph_text_short_returns_base() {
        // Short text: 1 (base) + 1 (split(",") on no-comma yields [text]) +
        // 0.10 (10 chars / 100) = 2.10.
        let s = "abcdefghij"; // exactly 10 chars
        let expected = 1.0 + 1.0 + 10.0_f64 / 100.0;
        assert!((score_paragraph_text(s) - expected).abs() < 1e-9);
    }

    #[test]
    fn score_paragraph_text_long_capped() {
        // 500-char string: length term = min(5.0, 3.0) = 3.0. Comma term
        // = 1 (no commas). Base = 1. Total = 5.0.
        let s = "a".repeat(500);
        let expected = 1.0 + 1.0 + 3.0;
        assert!((score_paragraph_text(&s) - expected).abs() < 1e-9);
    }

    #[test]
    fn score_paragraph_text_comma_bonus() {
        // "a,b,c,d" → split(",") = ["a","b","c","d"] length 4. Length is
        // 7 chars → 0.07. Base 1. Total = 1 + 4 + 0.07 = 5.07.
        let s = "a,b,c,d";
        let expected = 1.0 + 4.0 + 7.0_f64 / 100.0;
        assert!((score_paragraph_text(s) - expected).abs() < 1e-9);
    }

    #[test]
    fn score_paragraph_text_trims_input() {
        // Whitespace runs collapse via trim, so leading/trailing space
        // contributes 0 chars (trimmed) — only the inner content counts.
        let with_ws = "   hello   world   ";
        let without_ws = "hello world";
        assert!((score_paragraph_text(with_ws) - score_paragraph_text(without_ws)).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // link_density (readability_lxml.py:220-223)
    // -----------------------------------------------------------------------

    #[test]
    fn link_density_empty_returns_zero() {
        // Empty element: text_length = 0, max(1) = 1; no <a> descendants.
        // 0 / 1 = 0.
        let (_dom, root) = parse_body_first_child("<div></div>");
        assert_eq!(link_density(&root), 0.0);
    }

    #[test]
    fn link_density_all_link_returns_one() {
        // Entire content is inside one <a>.
        let (_dom, root) =
            parse_body_first_child("<div><a href=\"#\">hello world</a></div>");
        assert!((link_density(&root) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn link_density_half_link_around_half() {
        // 10 chars of link text, 10 chars of non-link text.
        let (_dom, root) = parse_body_first_child(
            "<div><a href=\"#\">aaaaaaaaaa</a> bbbbbbbbbb</div>",
        );
        let d = link_density(&root);
        // trim collapses the space between </a> and "bbbb...", so the
        // total text is "aaaaaaaaaa bbbbbbbbbb" = 21 chars, link text =
        // 10 chars. Density ≈ 10/21.
        let expected = 10.0_f64 / 21.0_f64;
        assert!((d - expected).abs() < 1e-9, "got {d}, want {expected}");
    }

    // -----------------------------------------------------------------------
    // text_length (readability_lxml.py:87-89)
    // -----------------------------------------------------------------------

    #[test]
    fn text_length_strips_whitespace_and_counts_chars() {
        // "   hello   world   " trims+collapses to "hello world" (11 chars).
        let (_dom, root) =
            parse_body_first_child("<div>   hello   world   </div>");
        assert_eq!(text_length(&root), 11);
    }

    #[test]
    fn text_length_counts_codepoints_not_bytes() {
        // Multi-byte UTF-8 character: "café" is 4 codepoints, 5 bytes.
        // We must return 4 (chars().count()), not 5 (str::len() bytes).
        let (_dom, root) = parse_body_first_child("<div>café</div>");
        assert_eq!(text_length(&root), 4);
    }

    // -----------------------------------------------------------------------
    // Regex literal sanity checks — ensure each REGEXES entry compiled
    // and matches a hand-picked positive example. This catches regex-
    // dialect drift if anyone retypes a pattern with a JS-only escape.
    // -----------------------------------------------------------------------

    #[test]
    fn regex_unlikely_candidates_matches_sidebar() {
        assert!(unlikely_candidates_re().is_match("sidebar-secondary"));
        assert!(unlikely_candidates_re().is_match("DISQUS-COMMENTS")); // case-insensitive
    }

    #[test]
    fn regex_video_matches_youtube_and_vimeo() {
        assert!(video_re().is_match("https://www.youtube.com/embed/abc"));
        assert!(video_re().is_match("HTTP://vimeo.com/12345"));
        assert!(!video_re().is_match("https://example.com/page"));
    }
}
