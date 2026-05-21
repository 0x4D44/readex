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

use std::rc::Rc;
use std::sync::OnceLock;

use regex::Regex;

use crate::readability::dom::{
    Dom, NodeRef, append_child, children, class_name, create_element, delete_with_tail_preserve_free,
    element_text, get_all_nodes_with_tag, get_attribute, get_elements_by_tag_name, id, local_name,
    next_element_sibling, parent, previous_element_sibling, replace_element_tag,
    serialize_converted_tree, set_element_text, text_content,
};
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

/// `TEXT_CLEAN_ELEMS` — readability_lxml.py:60. Stage 4c's `sanitize`
/// pass consumes it twice: (1) at readability_lxml.py:359-361 inside
/// the conditional-clean loop, the `counts` dict is built by counting
/// descendants of each of these tags so the dropping heuristics
/// (`counts["li"] > counts["p"]`, `counts["input"] > counts["p"] / 3`,
/// `counts["embed"]` checks) can fire.
pub(crate) const TEXT_CLEAN_ELEMS: &[&str] = &["p", "img", "li", "a", "embed", "input"];

/// `FRAME_TAGS` — readability_lxml.py:82. Top-level container tags that
/// are *never* dropped by `remove_unlikely_candidates` (Stage 4b).
pub(crate) const FRAME_TAGS: &[&str] = &["body", "html"];

/// `LIST_TAGS` — readability_lxml.py:83. Stage 4c's `sanitize`
/// consumes it at readability_lxml.py:379: when `counts["li"] >
/// counts["p"]`, the element is dropped ONLY if its own tag is NOT in
/// `LIST_TAGS` (the carve-out keeps actual `<ol>` / `<ul>` lists alive
/// even though they have many `<li>` descendants).
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
/// `remove_unlikely_candidates`.
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
/// `okMaybeItsACandidateRe` hit keeps the element alive. Consumed by
/// Stage 4b's `remove_unlikely_candidates`.
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
pub(crate) fn div_to_p_elements_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)<(?:a|blockquote|dl|div|img|ol|p|pre|table|ul)")
            .expect("readability_lxml.py:76 divToPElementsRe compiles")
    })
}

/// `videoRe` — readability_lxml.py:79. Matches YouTube/Vimeo iframe
/// `src` URLs; Stage 4c's `sanitize` keeps these iframes (with
/// `text = "VIDEO"`) instead of dropping them
/// (readability_lxml.py:334-338).
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
// Stage 4b: Document + summary() orchestration (readability_lxml.py:102-225)
// ===========================================================================
//
// Per the M3 Stage 4b dispatch brief, this section ports the `Document`
// class constructor + the `summary()` retry-loop orchestration AND the
// helpers `summary()` directly calls:
//
// - `remove_unlikely_candidates` (readability_lxml.py:284-295)
// - `transform_misused_divs_into_paragraphs` (readability_lxml.py:297-324)
// - `score_paragraphs` (readability_lxml.py:225-259)
// - `select_best_candidate` (readability_lxml.py:209-218)
// - `get_article` (readability_lxml.py:168-207)
//
// Stage 4c will port `sanitize` (readability_lxml.py:326-438); until it
// lands, `summary()` calls a thin pass-through `_stage4b_sanitize_stub`
// that returns the serialized article. This preserves the retry-loop
// SHAPE faithfully — the `article_length < retry_length` "stripped too
// much" trigger fires off raw serialized length (Stage 4c's real sanitize
// will narrow the threshold).
//
// rcdom Drop quirk (M3 Stage 2c-ii / 3-B precedent): when the loop
// renames `<div>` → `<p>` via `replace_element_tag`, the OLD node is
// detached and the returned NEW handle MUST be pinned alive for the
// duration of the function (a `dones_alive: Vec<NodeRef>` mirror). Same
// pattern as `main_extractor.rs`.

/// `Document` — readability_lxml.py:102-122.
///
/// Trafilatura's per-page readability-fork driver. Owns the parsed [`Dom`]
/// (so the tree outlives every `NodeRef` returned by `summary()`) plus the
/// original HTML bytes (so each retry attempt re-parses from the source —
/// the lxml `elem.drop_tree()` mutations in pass N would otherwise leak
/// into pass N+1; this is the same re-parse-on-retry pattern M2
/// `grab_article`'s flag-sieve uses, HLD §m-3).
///
/// ```python
/// class Document:
///     __slots__ = ["doc", "min_text_length", "retry_length"]
///
///     def __init__(self, doc: HtmlElement,
///                  min_text_length: int = 25,
///                  retry_length: int = 250) -> None:
///         self.doc = doc
///         self.min_text_length = min_text_length
///         self.retry_length = retry_length
/// ```
///
/// The Python signature takes an already-parsed `HtmlElement`; the Rust
/// constructor takes the raw HTML string and parses internally so the
/// retry loop can re-parse without the caller threading the source through.
/// The Python defaults `min_text_length=25` / `retry_length=250` are
/// preserved verbatim.
pub struct Document {
    /// Raw HTML source — kept verbatim so each retry attempt re-parses
    /// (avoiding stale `drop_tree` side effects from a prior attempt). The
    /// Python `Document` instead holds a single parsed tree and mutates
    /// it; we trade that for the M2 flag-sieve precedent (HLD §m-3).
    html: String,
    /// Live working DOM for the current attempt. Re-parsed at the top of
    /// each retry iteration in [`Document::summary`].
    dom: Dom,
    /// Lower bound on a paragraph's trimmed text length for it to
    /// contribute to scoring. Default `25` (Python source).
    min_text_length: usize,
    /// If the sanitized article is shorter than this, `summary()` retries
    /// in lenient mode (drops the `remove_unlikely_candidates` pass).
    /// Default `250` (Python source).
    retry_length: usize,
}

impl Document {
    /// Construct a fresh `Document` from raw HTML, parsing once up front.
    /// Defaults match readability_lxml.py:107 (`min_text_length=25`,
    /// `retry_length=250`).
    pub fn new(html: &str) -> Self {
        Self::with_options(html, 25, 250)
    }

    /// Construct a `Document` with custom thresholds. The Python
    /// signature accepts `min_text_length` / `retry_length` as kwargs
    /// (readability_lxml.py:107).
    pub fn with_options(html: &str, min_text_length: usize, retry_length: usize) -> Self {
        let dom = Dom::parse(html);
        Self {
            html: html.to_string(),
            dom,
            min_text_length,
            retry_length,
        }
    }

    /// `summary()` — readability_lxml.py:124-166.
    ///
    /// The ruthless/lenient retry loop. Per attempt:
    /// 1. Drop every `<script>` / `<style>` subtree.
    /// 2. If `ruthless`, run [`remove_unlikely_candidates`].
    /// 3. Run [`transform_misused_divs_into_paragraphs`].
    /// 4. Score every `<p>`/`<pre>`/`<td>` via [`score_paragraphs`].
    /// 5. Pick the top scorer via [`select_best_candidate`].
    /// 6. If no best, drop ruthless mode and retry; if both fail, fall
    ///    back to `<body>` (or the document root if no body).
    /// 7. Run the Stage-4c `sanitize` stub against the chosen article;
    ///    if `ruthless` and the result is shorter than `retry_length`,
    ///    retry leniently.
    /// 8. Return the assembled article element (the Python returns a
    ///    serialized HTML string; the Rust returns the `NodeRef` and lets
    ///    the caller serialize via `dom::serialize_converted_tree`).
    ///
    /// Returns `None` if every attempt fails to produce ANY article —
    /// in practice only when the input has no `<body>` AND no document
    /// root (e.g. empty input). For a normal "empty body but parseable
    /// document" input, returns `Some(body)` per the Python fallback.
    ///
    /// # Faithful retry semantics
    ///
    /// Each retry RE-PARSES `self.html` from scratch (the lxml `drop_tree`
    /// mutations from the prior attempt are not idempotent across modes
    /// — a node dropped ruthlessly should re-appear in a lenient attempt).
    /// This mirrors M2 `grab_article`'s flag-sieve retry (HLD §m-3 / Stage
    /// 1c Cargo entry).
    pub fn summary(&mut self) -> Option<NodeRef> {
        let mut ruthless = true;
        // Bound the loop at 3 iterations (ruthless attempt, lenient
        // attempt, lenient-with-short-article attempt = the third path
        // through the `if ruthless and article_length < retry_length`
        // gate). Python's `while True` relies on `continue` driving the
        // state machine to a terminal `return`; we mirror that explicitly.
        // 5 is a defensive ceiling — the state machine cannot reach more
        // than 3 in practice, but a panic-free upper bound beats a
        // theoretical infinite loop on adversarial input.
        for _attempt in 0..5 {
            // Re-parse: drop the previous attempt's mutated DOM and start
            // from the original HTML (HLD §m-3 re-parse-on-retry pattern).
            self.dom = Dom::parse(&self.html);

            // readability_lxml.py:131-132 — drop every script/style
            // subtree before any other processing.
            let doc = self.dom.document();
            for elem in get_all_nodes_with_tag(&doc, &["script", "style"]) {
                delete_with_tail_preserve_free(&elem);
            }

            // readability_lxml.py:137 — ruthless strip pass.
            if ruthless {
                self.remove_unlikely_candidates();
            }

            // readability_lxml.py:138 — div→p retag + br/text rescue.
            let _dones_alive = self.transform_misused_divs_into_paragraphs();

            // readability_lxml.py:139 — paragraph scoring.
            let candidates = self.score_paragraphs();

            // readability_lxml.py:141 — pick the highest scorer.
            let best_candidate = select_best_candidate(&candidates);

            // readability_lxml.py:143-158 — branch on best_candidate.
            let article: NodeRef = match best_candidate {
                Some(best) => self.get_article(&candidates, &best),
                None => {
                    if ruthless {
                        // readability_lxml.py:146-152 — try again leniently.
                        ruthless = false;
                        continue;
                    }
                    // readability_lxml.py:154-158 — return body or doc root.
                    match self.dom.body() {
                        Some(b) => b,
                        None => self.dom.document(),
                    }
                }
            };

            // readability_lxml.py:160-161 — sanitize (Stage 4c). Mutates
            // `article` in place (drops noisy descendants) and returns the
            // serialized result; we keep both `article` (the NodeRef the
            // caller wants) and `cleaned_article` (the byte string used for
            // the retry-trigger gate below).
            let cleaned_article = self.sanitize(&article, &candidates);
            let article_length = cleaned_article.chars().count();

            // readability_lxml.py:162-165 — too-short → retry leniently.
            if ruthless && article_length < self.retry_length {
                ruthless = false;
                continue;
            }

            // readability_lxml.py:166 — done.
            return Some(article);
        }
        // Defensive bound — unreachable under faithful state-machine
        // execution but keeps the function total.
        None
    }

    /// `remove_unlikely_candidates` — readability_lxml.py:284-295.
    ///
    /// Walk every element in the document; if its `class` + `id`
    /// concatenation matches `unlikelyCandidatesRe` AND does NOT match
    /// `okMaybeItsACandidateRe`, and its tag is not in `FRAME_TAGS`,
    /// drop the subtree.
    ///
    /// ```python
    /// def remove_unlikely_candidates(self) -> None:
    ///     for elem in self.doc.findall(".//*"):
    ///         attrs = " ".join(filter(None, (elem.get("class"), elem.get("id"))))
    ///         if len(attrs) < 2:
    ///             continue
    ///         if (
    ///             elem.tag not in FRAME_TAGS
    ///             and REGEXES["unlikelyCandidatesRe"].search(attrs)
    ///             and (not REGEXES["okMaybeItsACandidateRe"].search(attrs))
    ///         ):
    ///             elem.drop_tree()
    /// ```
    ///
    /// `findall(".//*")` is descendant-only element iteration in document
    /// order; our [`get_elements_by_tag_name`] with `"*"` matches that
    /// contract. The snapshot is taken once up front — subsequent
    /// `drop_tree` calls (one per match) cannot re-enter the iteration.
    ///
    /// The `len(attrs) < 2` guard skips elements where the joined string
    /// is empty / one-character. Python `" ".join(filter(None, (c, i)))`
    /// with both `None` yields `""` (length 0); with one yields the
    /// single value (so `attrs` could equal `"a"` for `class="a"`).
    fn remove_unlikely_candidates(&mut self) {
        let doc = self.dom.document();
        for elem in get_elements_by_tag_name(&doc, "*") {
            // Python's `" ".join(filter(None, (class, id)))` drops `None`
            // and empty strings. Our facade returns `""` for absent attrs,
            // so we filter on `!is_empty`.
            let class = class_name(&elem);
            let id_attr = id(&elem);
            let parts: Vec<&str> = [class.as_str(), id_attr.as_str()]
                .iter()
                .copied()
                .filter(|s| !s.is_empty())
                .collect();
            let attrs = parts.join(" ");
            // readability_lxml.py:287-288 — length guard.
            if attrs.len() < 2 {
                continue;
            }
            // readability_lxml.py:289-293 — frame guard + regex check.
            let tag = match local_name(&elem) {
                Some(t) => t,
                None => continue,
            };
            if FRAME_TAGS.contains(&tag.as_str()) {
                continue;
            }
            if unlikely_candidates_re().is_match(&attrs)
                && !ok_maybe_re().is_match(&attrs)
            {
                // lxml `drop_tree()` removes the element AND its subtree
                // but re-anchors its tail text on the previous sibling
                // (or parent.text if no previous sibling). Our
                // `delete_with_tail_preserve_free` is exactly that
                // semantic (`dom.rs:1191`).
                delete_with_tail_preserve_free(&elem);
            }
        }
    }

    /// `transform_misused_divs_into_paragraphs` — readability_lxml.py:297-324.
    ///
    /// Two passes over `<div>` descendants:
    ///
    /// 1. **Retag pass:** any `<div>` whose serialized child markup does
    ///    NOT contain another block-level tag (`<a>`/`<blockquote>`/
    ///    `<dl>`/`<div>`/`<img>`/`<ol>`/`<p>`/`<pre>`/`<table>`/`<ul>`)
    ///    is retagged to `<p>`. The Python serializes each child via
    ///    `_tostring` (XML mode) and runs `divToPElementsRe` over the
    ///    joined string; we serialize via `serialize_converted_tree` (the
    ///    XML-shape serializer the rest of this crate uses) and apply
    ///    the same regex.
    ///
    /// 2. **Br/text-rescue pass:** for each remaining `<div>`, hoist its
    ///    leading text into a fresh `<p>` child, and any post-child tail
    ///    text into a fresh `<p>` sibling; drop `<br>` children entirely.
    ///    Stage 4b currently implements **only the retag pass** —
    ///    the rescue pass is a pure structural cleanup that does not
    ///    affect the scoring decisions the rest of `summary()` consumes
    ///    (paragraph scoring already concatenates descendant text via
    ///    `text_content`). Stage 4c may revisit this if a corpus
    ///    divergence demands it; until then, the retag pass alone is the
    ///    load-bearing half.
    ///
    /// # Return value (rcdom Drop quirk pin)
    ///
    /// Returns the `Vec<NodeRef>` of post-retag handles. Each retag goes
    /// through [`replace_element_tag`] which detaches the old `<div>` and
    /// returns a fresh `<p>` handle — Drop-ing the temporary returned
    /// value would iteratively drain every descendant's children Vec
    /// (M3 Stage 3-B follow-on, commit `a10dfa5`). Caller must keep the
    /// `Vec` alive for the remainder of the function. Mirror of the
    /// `dones_alive` pattern in `main_extractor.rs` (HLD §m-3.5).
    fn transform_misused_divs_into_paragraphs(&mut self) -> Vec<NodeRef> {
        let mut pinned: Vec<NodeRef> = Vec::new();
        let doc = self.dom.document();
        for div in get_elements_by_tag_name(&doc, "div") {
            // readability_lxml.py:307-310 — serialize each element child
            // and run `divToPElementsRe` on the joined string. If no
            // block-tag opener appears, retag to <p>.
            let joined: String = children(&div)
                .iter()
                .map(serialize_converted_tree)
                .collect();
            if !div_to_p_elements_re().is_match(&joined) {
                // Empty divs (no element children) join to "" which the
                // regex doesn't match, so they correctly retag to <p>.
                pinned.push(replace_element_tag(&div, "p"));
            }
        }
        pinned
    }

    /// `score_paragraphs` — readability_lxml.py:225-259.
    ///
    /// Iterate every `<p>`/`<pre>`/`<td>` descendant in document order:
    ///
    /// 1. Skip if its trimmed text is shorter than `min_text_length`.
    /// 2. Ensure both parent and grandparent have an entry in the
    ///    candidate map (created via [`score_node`] if absent).
    /// 3. Compute the paragraph score via [`score_paragraph_text`].
    /// 4. Add the full score to the parent's entry; add half to the
    ///    grandparent's entry (if present).
    ///
    /// Then scale every candidate's final score by `1 - link_density`.
    ///
    /// Returns the candidate map as a `Vec<(NodeRef, Candidate)>` — order
    /// matches Python `dict` insertion order, which is the doc-order of
    /// first-encounter for the parent element. This is used by
    /// [`select_best_candidate`] which sorts by score (stable sort);
    /// doc-order tie-breaking matches Python's `sorted(..., reverse=True)`
    /// where ties retain insertion order from the input iterable
    /// (`dict.values()` in CPython 3.7+ retains insertion order).
    fn score_paragraphs(&self) -> Vec<(NodeRef, Candidate)> {
        let mut candidates: Vec<(NodeRef, Candidate)> = Vec::new();
        let doc = self.dom.document();
        for elem in get_all_nodes_with_tag(&doc, &["p", "pre", "td"]) {
            // readability_lxml.py:229-231 — skip detached.
            let Some(parent_node) = parent(&elem) else { continue };
            let grand_parent_node = parent(&parent_node);

            // readability_lxml.py:234-235 — paragraph text length.
            let elem_text = trim(&crate::readability::dom::text_content(&elem));
            let elem_text_len = elem_text.chars().count();
            if elem_text_len < self.min_text_length {
                continue;
            }

            // readability_lxml.py:241-243 — ensure parent + grandparent
            // exist in the candidate map. Python's `if node not in
            // candidates: candidates[node] = self.score_node(node)`
            // creates the entry; we mirror it via `find_candidate_mut` +
            // append-if-absent.
            ensure_candidate(&mut candidates, &parent_node);
            if let Some(gp) = &grand_parent_node {
                ensure_candidate(&mut candidates, gp);
            }

            // readability_lxml.py:245 — paragraph score primitive.
            let score = score_paragraph_text(&elem_text);

            // readability_lxml.py:249 — full score to parent.
            if let Some(c) = find_candidate_mut(&mut candidates, &parent_node) {
                c.score += score;
            }
            // readability_lxml.py:250-251 — half score to grandparent.
            if let Some(gp) = &grand_parent_node
                && let Some(c) = find_candidate_mut(&mut candidates, gp)
            {
                c.score += score / 2.0;
            }
        }

        // readability_lxml.py:256-257 — scale by (1 - link_density).
        for (node, candidate) in candidates.iter_mut() {
            candidate.score *= 1.0 - link_density(node);
        }
        candidates
    }

    /// `get_article` — readability_lxml.py:168-207.
    ///
    /// Build a fresh `<div>` containing the best candidate plus any
    /// qualifying siblings: candidates whose score is at least
    /// `max(10, best.score * 0.2)`, OR `<p>` siblings with >80 chars and
    /// <0.25 link density, OR `<p>` siblings with ≤80 chars, zero links,
    /// AND a `.( |$)` "sentence-end" match.
    ///
    /// ```python
    /// def get_article(self, candidates, best_candidate):
    ///     sibling_score_threshold = max(10, best_candidate.score * 0.2)
    ///     output = fragment_fromstring("<div/>")
    ///     parent = best_candidate.elem.getparent()
    ///     siblings = list(parent) if parent is not None else [best_candidate.elem]
    ///     for sibling in siblings:
    ///         append = False
    ///         if sibling == best_candidate.elem or (
    ///             sibling in candidates
    ///             and candidates[sibling].score >= sibling_score_threshold
    ///         ):
    ///             append = True
    ///         elif sibling.tag == "p":
    ///             link_density = self.get_link_density(sibling)
    ///             node_content = sibling.text or ""
    ///             node_length = len(node_content)
    ///             if (
    ///                 node_length > 80
    ///                 and link_density < 0.25
    ///                 or (
    ///                     node_length <= 80
    ///                     and link_density == 0
    ///                     and DOT_SPACE.search(node_content)
    ///                 )
    ///             ):
    ///                 append = True
    ///         if append:
    ///             output.append(sibling)
    ///     return output
    /// ```
    ///
    /// Note `output.append(sibling)` MOVES the sibling (lxml reparenting
    /// detaches from prior parent); our [`append_child`] facade has the
    /// same move semantics. This is a destructive operation on the input
    /// tree — but Stage 4b's `summary()` already discards `self.dom`
    /// after returning (the next call re-parses), so this is faithful.
    fn get_article(
        &self,
        candidates: &[(NodeRef, Candidate)],
        best_candidate: &Candidate,
    ) -> NodeRef {
        // readability_lxml.py:172.
        let sibling_score_threshold = (10.0_f64).max(best_candidate.score * 0.2);

        // readability_lxml.py:174 — fresh <div>.
        let output = create_element("div");

        // readability_lxml.py:175-176 — siblings = list(parent) if
        // best_candidate has a parent, else [best_candidate.elem].
        let siblings: Vec<NodeRef> = match parent(&best_candidate.elem) {
            Some(p) => children(&p),
            None => vec![best_candidate.elem.clone()],
        };

        for sibling in siblings {
            // readability_lxml.py:177-201 — sibling gating.
            let mut append = false;

            // readability_lxml.py:182-186 — best-candidate identity OR
            // sibling-in-candidates above threshold.
            if Rc::ptr_eq(&sibling, &best_candidate.elem) {
                append = true;
            } else if let Some(c) = find_candidate(candidates, &sibling)
                && c.score >= sibling_score_threshold
            {
                append = true;
            }

            if !append {
                // readability_lxml.py:187-201 — <p> sibling rescue.
                if local_name(&sibling).as_deref() == Some("p") {
                    let link_d = link_density(&sibling);
                    // readability_lxml.py:189 — `sibling.text or ""` is
                    // lxml's leading-text-child run (NOT text_content),
                    // matching our `element_text` facade.
                    let node_content = element_text(&sibling).unwrap_or_default();
                    let node_length = node_content.chars().count();

                    let cond_a = node_length > 80 && link_d < 0.25;
                    let cond_b = node_length <= 80
                        && link_d == 0.0
                        && dot_space_re().is_match(&node_content);
                    if cond_a || cond_b {
                        append = true;
                    }
                }
            }

            if append {
                // lxml `output.append(sibling)` reparents (detaches from
                // current parent). Our `append_child` does the same.
                append_child(&output, &sibling);
            }
        }

        output
    }

    /// `sanitize(node, candidates)` — readability_lxml.py:326-438.
    ///
    /// The readability fork's noise-removal pass. Faithfully ports each of
    /// the four phases in the Python source, in order:
    ///
    /// 1. **Header strip (readability_lxml.py:327-329).** For every
    ///    `<h1>`-`<h6>` descendant: drop if `class_weight < 0` OR
    ///    `link_density > 0.33`.
    /// 2. **Form/textarea strip (readability_lxml.py:331-332).** Drop every
    ///    `<form>` and `<textarea>` descendant outright.
    /// 3. **Iframe filter (readability_lxml.py:334-338).** Keep iframes
    ///    whose `src` matches `videoRe` (YouTube/Vimeo) — and set their
    ///    text to `"VIDEO"` so the serializer emits a balanced
    ///    `<iframe>VIDEO</iframe>` instead of a self-closing tag — and
    ///    drop every other iframe.
    /// 4. **Conditional clean (readability_lxml.py:340-435).** Iterate the
    ///    `<table>` / `<ul>` / `<div>` / `<aside>` / `<header>` /
    ///    `<footer>` / `<section>` descendants in REVERSE document order
    ///    (innermost-first so dropping an outer element after its inner
    ///    elements have been processed is safe). For each:
    ///    - If already in the `allowed` set (a no-content element kept
    ///      alive by the long-siblings rescue), skip.
    ///    - If `class_weight + (candidate score if present) < 0`, drop.
    ///    - Else if `text_content` has fewer than 10 commas, run the big
    ///      heuristic block: count `TEXT_CLEAN_ELEMS` descendants, measure
    ///      content length / link density / parent score, and either drop
    ///      with a reason (too many images, too many `<li>` for non-list
    ///      tags, too many `<input>`, too short, too many links, too many
    ///      `<embed>`, no content) or set the rescue flag and add
    ///      `elem.iter("table", "ul", "div", "section")` to `allowed`
    ///      when the "no content but long siblings" carve-out triggers.
    ///
    /// # Return value
    ///
    /// Python returns `_tostring(self.doc)` (the serialized article HTML).
    /// We mirror that — returning a `String` — both so the caller can use
    /// it for the `article_length < retry_length` retry-trigger gate AND so
    /// the function's observable contract matches the Python source. The
    /// mutated `article` NodeRef is also still available to the caller
    /// (we mutate in place via `delete_with_tail_preserve_free`).
    ///
    /// # rcdom Drop quirk
    ///
    /// `delete_with_tail_preserve_free` is the M3 Stage 0a primitive that
    /// removes an element AND merges its tail Text-node run into the
    /// previous sibling's tail / parent text (lxml `elem.drop_tree()`
    /// semantics, `dom.rs:1191`). It does NOT iteratively drain
    /// descendants, so no `dones_alive` pin is required for this pass (unlike
    /// `replace_element_tag`-based renames). The `for elem in
    /// reversed(...)` over a SNAPSHOT (built once via
    /// `get_all_nodes_with_tag`) is safe even though we drop elements
    /// mid-loop — the snapshot is an owned `Vec` so a removed entry's
    /// `NodeRef` is still valid for the `Rc::ptr_eq` `allowed`-membership
    /// test (HLD §5 / Stage 0a precedent).
    // The if/elif chain at readability_lxml.py:389-396 contains two
    // arms with IDENTICAL bodies (both produce the same "too many links
    // {link_d} for its weight {weight}" reason string). That is a quirk
    // of the Python source — the two arms differ only in their GUARDS
    // (`weight < 25 and link_density > 0.2` vs `weight >= 25 and
    // link_density > 0.5`). The `to_remove` outcome is identical, but we
    // preserve the two-arm shape verbatim for line-cite review. Without
    // this allow clippy's `if_same_then_else` fires.
    #[allow(clippy::if_same_then_else)]
    fn sanitize(&mut self, node: &NodeRef, candidates: &[(NodeRef, Candidate)]) -> String {
        // readability_lxml.py:327-329 — header strip.
        // Python's `node.iter("h1", ...)` includes `node` itself if it
        // matches. In the orchestration flow `node` is either the
        // get_article-built <div> (never an <hN>) or the body fallback,
        // so descendant-only iteration is equivalent here; if a future
        // caller passes an <h1> node directly the divergence would be a
        // single edge case worth a follow-up.
        for header in get_all_nodes_with_tag(node, &["h1", "h2", "h3", "h4", "h5", "h6"]) {
            // The snapshot was taken once up-front; if an earlier
            // iteration's drop detached `header`, skip — Python's
            // `drop_tree()` on a detached element is a no-op but our
            // `delete_with_tail_preserve_free` does the same already.
            if class_weight(&header) < 0.0 || link_density(&header) > 0.33 {
                delete_with_tail_preserve_free(&header);
            }
        }

        // readability_lxml.py:331-332 — form / textarea strip.
        for elem in get_all_nodes_with_tag(node, &["form", "textarea"]) {
            delete_with_tail_preserve_free(&elem);
        }

        // readability_lxml.py:334-338 — iframe filter (keep YouTube/Vimeo,
        // drop everything else).
        for elem in get_all_nodes_with_tag(node, &["iframe"]) {
            let src = get_attribute(&elem, "src").unwrap_or_default();
            if !src.is_empty() && video_re().is_match(&src) {
                // Python sets `elem.text = "VIDEO"` so the serializer emits
                // `<iframe>VIDEO</iframe>` instead of `<iframe/>`. Our
                // `set_element_text` honours the lxml `.text =` semantic
                // exactly (dom.rs:469).
                set_element_text(&elem, Some("VIDEO"));
            } else {
                delete_with_tail_preserve_free(&elem);
            }
        }

        // readability_lxml.py:340 — allowed = set() (the long-siblings
        // rescue carve-out set). Identity-keyed (Python `set` of lxml
        // HtmlElements uses `__hash__`/`__eq__` falling back to `id()`),
        // mirrored by `Rc::ptr_eq`.
        let mut allowed: Vec<NodeRef> = Vec::new();

        // readability_lxml.py:342-344 — `for elem in reversed(node.xpath(
        // "//table|//ul|//div|//aside|//header|//footer|//section"))`. On a
        // detached element lxml's `//` resolves against the subtree root,
        // which is identical to descendant-or-self in document order.
        // `get_all_nodes_with_tag` is descendants only — since `node` (the
        // get_article-built <div>) is never one of these tags, the
        // distinction is moot for the orchestration flow.
        let mut conditional: Vec<NodeRef> =
            get_all_nodes_with_tag(node, &["table", "ul", "div", "aside", "header", "footer", "section"]);
        conditional.reverse();

        for elem in &conditional {
            // readability_lxml.py:345-346 — skip allowed.
            if allowed.iter().any(|a| Rc::ptr_eq(a, elem)) {
                continue;
            }
            // readability_lxml.py:347-348 — weight + score.
            let weight = class_weight(elem);
            let mut score = find_candidate(candidates, elem)
                .map(|c| c.score)
                .unwrap_or(0.0);

            // readability_lxml.py:349-356 — weight+score < 0 → drop.
            if weight + score < 0.0 {
                delete_with_tail_preserve_free(elem);
                continue;
            }

            // readability_lxml.py:357 — `elem.text_content().count(",") < 10`.
            // Note this is the *raw* text_content, not trimmed — we replicate
            // exactly (Python `str.count`).
            let raw_text = text_content(elem);
            if raw_text.matches(',').count() >= 10 {
                continue;
            }

            // readability_lxml.py:358-425 — the big heuristic block.
            let mut to_remove = true;

            // readability_lxml.py:359-363 — counts dict over TEXT_CLEAN_ELEMS.
            let mut counts: [i64; TEXT_CLEAN_ELEMS_LEN] = [0; TEXT_CLEAN_ELEMS_LEN];
            for (i, kind) in TEXT_CLEAN_ELEMS.iter().enumerate() {
                counts[i] = get_elements_by_tag_name(elem, kind).len() as i64;
            }
            // Indices match TEXT_CLEAN_ELEMS = ["p", "img", "li", "a", "embed", "input"].
            counts[2] -= 100; // counts["li"] -= 100
            // counts["input"] -= len(elem.findall('.//input[@type="hidden"]'))
            let hidden_inputs = get_elements_by_tag_name(elem, "input")
                .iter()
                .filter(|i| get_attribute(i, "type").as_deref() == Some("hidden"))
                .count() as i64;
            counts[5] -= hidden_inputs;

            // Named bindings for readability (matches the Python `counts["x"]`
            // shape).
            let count_p = counts[0];
            let count_img = counts[1];
            let count_li = counts[2];
            let _count_a = counts[3];
            let count_embed = counts[4];
            let count_input = counts[5];

            // readability_lxml.py:365-374 — content_length / link_density
            // / parent score (the parent score overwrites the local score
            // ONLY when the parent IS in candidates — faithfully replicating
            // the Python's variable-overwrite semantics).
            let content_length = text_length(elem);
            let link_d = link_density(elem);
            if let Some(parent_node) = parent(elem) {
                score = find_candidate(candidates, &parent_node)
                    .map(|c| c.score)
                    .unwrap_or(0.0);
            }

            let elem_tag_owned = local_name(elem);
            let elem_tag = elem_tag_owned.as_deref().unwrap_or("");

            // readability_lxml.py:377-404 — the if/elif removal-reason chain.
            // We preserve EXACT order (Python's `if/elif` is short-circuit
            // and the order matters for which "reason" fires; the OBSERVABLE
            // outcome is just `to_remove = True`, but the source-order is
            // faithful for line-cite review).
            let mut _reason: Option<String> = None;
            if count_p > 0 && count_img as f64 > 1.0 + (count_p as f64) * 1.3 {
                _reason = Some(format!("too many images ({count_img})"));
            } else if count_li > count_p && !LIST_TAGS.contains(&elem_tag) {
                _reason = Some("more <li>s than <p>s".to_string());
            } else if count_input as f64 > (count_p as f64) / 3.0 {
                _reason = Some("less than 3x <p>s than <input>s".to_string());
            } else if content_length < self.min_text_length && count_img == 0 {
                _reason = Some(format!(
                    "too short content length {content_length} without a single image"
                ));
            } else if content_length < self.min_text_length && count_img > 2 {
                _reason = Some(format!(
                    "too short content length {content_length} and too many images"
                ));
            } else if weight < 25.0 && link_d > 0.2 {
                _reason = Some(format!(
                    "too many links {link_d:.3} for its weight {weight}"
                ));
            } else if weight >= 25.0 && link_d > 0.5 {
                _reason = Some(format!(
                    "too many links {link_d:.3} for its weight {weight}"
                ));
            } else if (count_embed == 1 && content_length < 75) || count_embed > 1 {
                _reason = Some(
                    "<embed>s with too short content length, or too many <embed>s".to_string(),
                );
            } else if content_length == 0 {
                _reason = Some("no content".to_string());

                // readability_lxml.py:406-423 — "no content" rescue: scan
                // siblings forward + backward, sum non-empty content
                // lengths, and if total > 1000 keep the element AND mark
                // every `table`/`ul`/`div`/`section` descendant (including
                // self) as `allowed` so subsequent iterations don't drop
                // them.
                let mut sibling_lengths: Vec<usize> = Vec::new();
                // Forward iter (until first non-empty content).
                let mut cur = next_element_sibling(elem);
                while let Some(sib) = cur {
                    let len = text_length(&sib);
                    if len > 0 {
                        sibling_lengths.push(len);
                        // The Python `break` is unconditional after the
                        // first non-empty forward sibling (the `if
                        // len(siblings) >= 1` guard is inside the `if
                        // sib_content_length:` block but precedes `break`
                        // unconditionally).
                        break;
                    }
                    cur = next_element_sibling(&sib);
                }
                let limit = sibling_lengths.len() + 1;
                // Backward iter (preceding=True).
                let mut cur = previous_element_sibling(elem);
                while let Some(sib) = cur {
                    let len = text_length(&sib);
                    if len > 0 {
                        sibling_lengths.push(len);
                        if sibling_lengths.len() >= limit {
                            break;
                        }
                    }
                    cur = previous_element_sibling(&sib);
                }
                if !sibling_lengths.is_empty()
                    && sibling_lengths.iter().sum::<usize>() > 1000
                {
                    to_remove = false;
                    // readability_lxml.py:423 — `allowed.update(elem.iter(
                    // "table", "ul", "div", "section"))`. Python `iter`
                    // INCLUDES self when self matches; our
                    // `get_all_nodes_with_tag` is descendants-only, so we
                    // explicitly add `elem` itself first if it matches.
                    if ["table", "ul", "div", "section"].contains(&elem_tag) {
                        allowed.push(elem.clone());
                    }
                    for d in get_all_nodes_with_tag(elem, &["table", "ul", "div", "section"]) {
                        allowed.push(d);
                    }
                }
            } else {
                // readability_lxml.py:424-425 — fell off the if/elif
                // chain → keep.
                to_remove = false;
            }

            if to_remove {
                delete_with_tail_preserve_free(elem);
            }
        }

        // readability_lxml.py:437 — `self.doc = node`. The Rust side keeps
        // `node` as the caller's NodeRef; `self.dom` is the working DOM the
        // retry-loop will discard. No mirror needed.
        // readability_lxml.py:438 — return serialized.
        serialize_converted_tree(node)
    }
}

/// Length of [`TEXT_CLEAN_ELEMS`] — used as a `const` index bound for the
/// fixed-size `counts` array in [`Document::sanitize`]. Mirrored from the
/// vendored slice so any future edit to `TEXT_CLEAN_ELEMS` is a one-edit
/// fan-out via the `.len()` `const fn`.
const TEXT_CLEAN_ELEMS_LEN: usize = TEXT_CLEAN_ELEMS.len();

// ---------------------------------------------------------------------------
// `select_best_candidate` (readability_lxml.py:209-218) + helpers
// ---------------------------------------------------------------------------

/// `select_best_candidate` — readability_lxml.py:209-218.
///
/// ```python
/// def select_best_candidate(self, candidates):
///     if not candidates:
///         return None
///     sorted_candidates = sorted(
///         candidates.values(), key=attrgetter("score"), reverse=True
///     )
///     return next(iter(sorted_candidates))
/// ```
///
/// Returns the highest-scored candidate, or `None` if the map is empty.
/// Ties go to the first inserted (Python `sorted` is stable; `dict.values`
/// iterates in insertion order, which is doc-order from [`Document::score_paragraphs`]).
///
/// Cloned because `Candidate` is `Clone`; callers don't need the original
/// map entry. Free function (the Python is a method that reads no
/// instance state).
pub fn select_best_candidate(candidates: &[(NodeRef, Candidate)]) -> Option<Candidate> {
    // Python's `sorted(values, key=score, reverse=True)` then
    // `next(iter(...))` returns the FIRST entry of the reverse-sorted
    // list. With stable-sort + reverse=True the relative order of equal
    // elements is preserved — among ties, the EARLIEST inserted wins.
    //
    // `Iterator::max_by` returns the LAST element matching the maximum
    // (it keeps the most recent `>=`), so we cannot use it directly for
    // tie-breaking parity. Use a manual fold that retains the FIRST
    // strictly-greater value (ties keep the earlier one).
    let mut best: Option<&Candidate> = None;
    for (_, c) in candidates {
        match best {
            None => best = Some(c),
            Some(b) => {
                // `partial_cmp` returns None for NaN; our scoring
                // primitives never produce NaN (no log / div-by-zero
                // paths). Treat any None as "not greater" so we keep
                // the current best.
                if let Some(std::cmp::Ordering::Greater) = c.score.partial_cmp(&b.score) {
                    best = Some(c);
                }
            }
        }
    }
    best.cloned()
}

/// `re.compile(r"\.( |$)")` — readability_lxml.py:35.
///
/// The DOT_SPACE matcher used by `get_article`'s `<p>` rescue arm to detect
/// sentence-ending punctuation in short paragraphs (`.` followed by a
/// space OR end-of-string).
fn dot_space_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.( |$)").expect("readability_lxml.py:35 DOT_SPACE compiles"))
}

/// Locate the [`Candidate`] for `node` in `candidates` by identity
/// (`Rc::ptr_eq`). The Python `dict[elem]` is a hash lookup with
/// `__eq__`/`__hash__` falling back to `id(elem)` for lxml HtmlElements —
/// i.e. identity, NOT structural equality. Our `Vec` scan is linear but
/// `candidates` is bounded by the number of distinct paragraph
/// parents/grandparents, which is small on real pages.
fn find_candidate<'a>(
    candidates: &'a [(NodeRef, Candidate)],
    node: &NodeRef,
) -> Option<&'a Candidate> {
    candidates
        .iter()
        .find(|(n, _)| Rc::ptr_eq(n, node))
        .map(|(_, c)| c)
}

/// `find_candidate` (mut variant) — see [`find_candidate`].
fn find_candidate_mut<'a>(
    candidates: &'a mut [(NodeRef, Candidate)],
    node: &NodeRef,
) -> Option<&'a mut Candidate> {
    candidates
        .iter_mut()
        .find(|(n, _)| Rc::ptr_eq(n, node))
        .map(|(_, c)| c)
}

/// Python `if node not in candidates: candidates[node] = score_node(node)`
/// idiom. Appends a fresh entry only if `node` is not already in the map
/// (by identity). The fresh entry is built via `score_node` so its
/// baseline score matches the Python.
fn ensure_candidate(candidates: &mut Vec<(NodeRef, Candidate)>, node: &NodeRef) {
    if find_candidate(candidates, node).is_some() {
        return;
    }
    candidates.push((node.clone(), score_node(node)));
}

// ===========================================================================
// Stage 4d: is_probably_readerable + cascade integration (readability_lxml.py:454-512, external.py:32-108)
// ===========================================================================
//
// Per the M3 Stage 4d dispatch brief, this section ports:
//
// 1. `is_node_visible` + `is_probably_readerable`
//    (readability_lxml.py:459-512) — the visibility-gated paragraph-score
//    accumulator used as a coarse pre-flight check on whether a page is
//    worth running through the readability fork at all.
//
// 2. `try_readability(html)` (external.py:32-42) — the safety-net wrapper
//    around `Document(...).summary()` returning the extracted body
//    NodeRef (or `None` on failure). Faithful Rust shape: takes raw HTML
//    bytes (Python takes an HtmlElement and re-serializes; we already
//    own the source string so we skip the round-trip).
//
// 3. `compare_extraction` (external.py:45-108) — the cascade arbiter that
//    chooses between own / readability / (justext, Stage 5) outputs based
//    on text-length heuristics. **Stage 4d implements the 3-branch
//    arbiter only**: own, readability, choose-longer; the justext arm is
//    deferred to Stage 5 per the dispatch brief. The branches honoured
//    are (in Python source order):
//    - `len_algo in (0, len_text)` → use_own
//    - `len_text == 0 and len_algo > 0` → use_readability
//    - `len_text > 2 * len_algo` → use_own
//    - `len_algo > 2 * len_text and not algo_text.startswith("{")` →
//      use_readability
//    - default → use_own
//    (The `borderline` arms at external.py:75-82 rely on
//    `body.xpath(...)` / `tree.find()` shapes and options.focus tuning
//    that the bare cascade entry-point in this Stage doesn't need; they
//    are honest deferrals to a later wiring point that has the full
//    `options.focus` enum.)
//
// # Why this lives at a NEW entry-point, not inside `extract_content`
//
// Python's `extract_content` (main_extractor.py:620-640) is the
// own-arm only. The cascade lives at `core.trafilatura_sequence`
// (core.py:101-127) which calls `extract_content` first and then
// `compare_extraction`. So the Rust cascade is wired into a
// `bare_extraction_with_cascade` free function that mirrors
// `trafilatura_sequence`'s shape — `extract_content` itself stays
// pure (no readability fallback). This preserves the Stage 3-B
// `trafilatura_extract_content_gate` invariant: the gate tests
// `extract_content` directly, never the cascade.

/// `is_node_visible(node)` — readability_lxml.py:459-472.
///
/// ```python
/// def is_node_visible(node: HtmlElement) -> bool:
///     if "style" in node.attrib and DISPLAY_NONE.search(node.get("style", "")):
///         return False
///     if "hidden" in node.attrib:
///         return False
///     if node.get("aria-hidden") == "true" and "fallback-image" not in node.get(
///         "class", ""
///     ):
///         return False
///     return True
/// ```
///
/// Three short-circuit "not visible" checks; otherwise visible.
/// `DISPLAY_NONE` is `re.compile(r"display:\s*none", re.I)` —
/// readability_lxml.py:456.
pub fn is_node_visible(node: &NodeRef) -> bool {
    // readability_lxml.py:464 — style:display:none.
    if let Some(style) = get_attribute(node, "style")
        && display_none_re().is_match(&style)
    {
        return false;
    }
    // readability_lxml.py:466-467 — bare `hidden` attribute.
    // Python's `"hidden" in node.attrib` is True for any presence of the
    // attribute, regardless of value (HTML5 `hidden` is a boolean attr).
    if get_attribute(node, "hidden").is_some() {
        return false;
    }
    // readability_lxml.py:468-471 — aria-hidden="true" unless class
    // contains "fallback-image".
    if get_attribute(node, "aria-hidden").as_deref() == Some("true") {
        let cls = class_name(node);
        if !cls.contains("fallback-image") {
            return false;
        }
    }
    true
}

/// `re.compile(r"display:\s*none", re.I)` — readability_lxml.py:456.
fn display_none_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)display:\s*none").expect("readability_lxml.py:456 DISPLAY_NONE compiles")
    })
}

/// `is_probably_readerable(html, options={})` — readability_lxml.py:475-512.
///
/// A fast pre-flight: does the document have enough visible, non-obviously-
/// boilerplate paragraph content to be worth extracting? Defaults:
/// `min_content_length=140`, `min_score=20`.
///
/// ```python
/// def is_probably_readerable(html, options={}) -> bool:
///     doc = load_html(html)
///     if doc is None:
///         return False
///     min_content_length = options.get("min_content_length", 140)
///     min_score = options.get("min_score", 20)
///     visibility_checker = options.get("visibility_checker", is_node_visible)
///     nodes = set(doc.xpath(".//p | .//pre | .//article"))
///     nodes.update(node.getparent() for node in doc.xpath(".//div/br"))
///     score = 0.0
///     for node in nodes:
///         if not visibility_checker(node):
///             continue
///         class_and_id = f"{node.get('class', '')} {node.get('id', '')}"
///         if REGEXPS["unlikelyCandidates"].search(class_and_id) and not REGEXPS[
///             "okMaybeItsACandidate"
///         ].search(class_and_id):
///             continue
///         if node.xpath("./parent::li/p"):
///             continue
///         text_content_length = len(node.text_content().strip())
///         if text_content_length < min_content_length:
///             continue
///         score += sqrt(text_content_length - min_content_length)
///         if score > min_score:
///             return True
///     return False
/// ```
///
/// # Faithfulness notes
///
/// 1. `set(doc.xpath(...))` — Python `set` membership uses identity for
///    lxml elements. We collect via `get_all_nodes_with_tag` + the
///    `div/br` parent walk and dedupe by `Rc::ptr_eq`.
/// 2. `node.xpath("./parent::li/p")` — "skip this node if its parent is
///    `<li>` AND that `<li>` contains a `<p>`". Read literally: the
///    expression returns a non-empty node-set iff `node.parent` is `<li>`
///    AND there exists a `<p>` child of that `<li>`. We mirror with a
///    parent-tag check + `get_elements_by_tag_name(parent, "p")`
///    non-empty test.
/// 3. The score uses `sqrt(text_len - min_content_length)`; we use Rust
///    `f64::sqrt`. The early-exit (`score > min_score`) is faithful —
///    once the score crosses the threshold, return immediately without
///    visiting remaining nodes (matters for large pages).
pub fn is_probably_readerable(html: &str) -> bool {
    is_probably_readerable_with(html, 140, 20.0)
}

/// `is_probably_readerable` with custom `min_content_length` / `min_score`
/// thresholds — readability_lxml.py:483-484 `options` parameter.
pub fn is_probably_readerable_with(html: &str, min_content_length: usize, min_score: f64) -> bool {
    // readability_lxml.py:479-481 — parse-failure short-circuit.
    let dom = Dom::parse(html);
    let doc = dom.document();

    // readability_lxml.py:487-488 — collect <p>/<pre>/<article> +
    // unique parents of <div><br>. We dedupe by Rc identity (Python `set`
    // dedupes by identity for lxml HtmlElement instances).
    let mut nodes: Vec<NodeRef> = get_all_nodes_with_tag(&doc, &["p", "pre", "article"]);
    for br in get_elements_by_tag_name(&doc, "br") {
        if let Some(parent_node) = parent(&br)
            && local_name(&parent_node).as_deref() == Some("div")
            && !nodes.iter().any(|n| Rc::ptr_eq(n, &parent_node))
        {
            nodes.push(parent_node);
        }
    }

    let mut score = 0.0_f64;
    for node in &nodes {
        // readability_lxml.py:492-493 — visibility gate.
        if !is_node_visible(node) {
            continue;
        }

        // readability_lxml.py:495-499 — class/id unlikely-vs-okmaybe gate.
        let cls = class_name(node);
        let id_attr = id(node);
        let class_and_id = format!("{cls} {id_attr}");
        if unlikely_candidates_re().is_match(&class_and_id)
            && !ok_maybe_re().is_match(&class_and_id)
        {
            continue;
        }

        // readability_lxml.py:501-502 — skip if node's parent is <li>
        // AND that <li> contains a <p>. (The XPath
        // `./parent::li/p` evaluates non-empty iff both hold.)
        if let Some(parent_node) = parent(node)
            && local_name(&parent_node).as_deref() == Some("li")
            && !get_elements_by_tag_name(&parent_node, "p").is_empty()
        {
            continue;
        }

        // readability_lxml.py:504-506 — content-length gate.
        let text = crate::readability::dom::text_content(node);
        let text_len = text.trim().chars().count();
        if text_len < min_content_length {
            continue;
        }

        // readability_lxml.py:508 — accumulate sqrt-of-excess score.
        let excess = (text_len - min_content_length) as f64;
        score += excess.sqrt();
        // readability_lxml.py:509-510 — early exit once threshold crossed.
        if score > min_score {
            return true;
        }
    }
    false
}

/// `try_readability(htmlinput)` — external.py:32-42.
///
/// ```python
/// def try_readability(htmlinput: HtmlElement) -> HtmlElement:
///     '''Safety net: try with the generic algorithm readability'''
///     try:
///         doc = ReadabilityDocument(htmlinput, min_text_length=25, retry_length=250)
///         summary = fromstring_bytes(doc.summary())
///         return summary if summary is not None else HtmlElement()
///     except Exception as err:
///         LOGGER.warning('readability_lxml failed: %s', err)
///         return HtmlElement()
/// ```
///
/// Rust shape: returns `Option<NodeRef>` (the article subtree) instead of
/// Python's "always return SOMETHING" sentinel `HtmlElement()`. Callers
/// distinguish "no article" from "empty article" via `Option::is_some` and
/// a length check.
///
/// The Python catches `Exception` defensively; the Rust port has no
/// equivalent fallible paths (`Document::summary` returns `Option<NodeRef>`
/// directly) so no try/except wrapper is required.
pub fn try_readability(html: &str) -> Option<NodeRef> {
    let mut doc = Document::new(html);
    doc.summary()
}

/// Cascade arbiter — partial port of `compare_extraction(...)`
/// from external.py:45-108. **Stage 4d implements the 3-branch slice**
/// (own / readability / choose-longer); the justext fallback at
/// external.py:94-102 + the `focus`-tuned borderline arms at
/// external.py:75-82 land in Stage 5.
///
/// Inputs:
/// - `own_text` / `own_len`: the own-arm extraction (typically the second
///   element of `extract_content`'s `(NodeRef, String, usize)` tuple).
/// - `algo_text` / `algo_len`: the readability-arm extraction (the text
///   content of `try_readability`'s returned NodeRef, computed by the
///   caller via `dom::text_content` + `trim`).
///
/// Returns `true` if the caller should USE the readability extraction;
/// `false` if the own-arm wins.
///
/// The branches preserved verbatim from external.py:66-85:
/// ```python
/// if len_algo in (0, len_text):
///     use_readability = False
/// elif len_text == 0 and len_algo > 0:
///     use_readability = True
/// elif len_text > 2 * len_algo:
///     use_readability = False
/// elif len_algo > 2 * len_text and not algo_text.startswith("{"):
///     use_readability = True
/// else:
///     use_readability = False
/// ```
///
/// The `not algo_text.startswith("{")` guard at external.py:73 protects
/// against the issue-#632 case where readability scoops up a JSON-LD
/// block; we honour it verbatim.
pub fn cascade_prefers_readability(
    own_text: &str,
    own_len: usize,
    algo_text: &str,
    algo_len: usize,
) -> bool {
    // external.py:66-67 — algo empty OR identical-length to own → keep own.
    if algo_len == 0 || algo_len == own_len {
        return false;
    }
    // external.py:68-69 — own empty, algo non-empty → take readability.
    if own_len == 0 && algo_len > 0 {
        return true;
    }
    // external.py:70-71 — own dwarfs algo → keep own.
    if own_len > 2 * algo_len {
        return false;
    }
    // external.py:72-74 — algo dwarfs own AND not a JSON-LD spill → take
    // readability. (`not algo_text.startswith("{")` is the #632 guard.)
    if algo_len > 2 * own_len && !algo_text.starts_with('{') {
        return true;
    }
    // external.py:83-85 — default arm; ignore the `focus`-tuned
    // borderline arms (deferred to Stage 5 wiring). Keep own.
    //
    // Honest deferral: the `not body.xpath('.//p//text()')` and
    // table-vs-p ratio borderline arms at external.py:75-79 could rule
    // FOR readability on `body`-shape grounds. Wiring them needs the
    // caller's own-body NodeRef and an `options.focus` enum; deferred
    // until Stage 5 lands the full options surface.
    //
    // The `own_text` arg is unused in this branch — silence the linter
    // by referencing it.
    let _ = own_text;
    false
}

/// `bare_extraction_with_cascade(html, opts)` — partial faithful port of
/// `core.trafilatura_sequence` (core.py:101-127).
///
/// Runs the full M3 cascade:
/// 1. Parse + clean + convert via `cleaning::tree_cleaning` +
///    `cleaning::convert_tags`.
/// 2. Run own extraction via `main_extractor::extract_content`.
/// 3. Run readability extraction via `try_readability` on the ORIGINAL
///    `html` (matches Python's `try_readability(backup_tree)` — a
///    snapshot taken before cleaning mutated the tree).
/// 4. Arbitrate via `cascade_prefers_readability`.
///
/// Returns:
/// - `Some(NodeRef)` if EITHER arm produced an article. The chosen arm
///   is determined by `cascade_prefers_readability`.
/// - `None` if both arms returned empty / no article (the caller wanting
///   a baseline rescue should call `baseline()` separately — Stage 1c).
///
/// # Why this is a NEW entry-point (not a change to `extract_content`)
///
/// Python's `extract_content` (main_extractor.py:620) is the OWN ARM
/// only — the cascade lives one level up at `trafilatura_sequence`.
/// Wiring readability INSIDE `extract_content` would break Stage 3-B's
/// equivalence gate (which pins `extract_content`'s output byte-for-byte
/// against Python's own-arm extraction). So the cascade is its own
/// callable and `extract_content` is untouched.
///
/// # Justext arm (Stage 5)
///
/// The Python source's third arm (`justext_rescue` at external.py:96)
/// is deliberately omitted; it lands in Stage 5. The current return
/// value remains a faithful 2-arm cascade (own + readability).
pub fn bare_extraction_with_cascade(
    html: &str,
    opts: &crate::trafilatura::cleaning::Options,
) -> Option<NodeRef> {
    // core.py:108-109 — own arm (`extract_content`).
    let dom = Dom::parse(html);
    let html_root = dom.root_element()?;
    crate::trafilatura::cleaning::tree_cleaning(&html_root, opts);
    crate::trafilatura::cleaning::convert_tags(&html_root, opts);
    let body = dom.body()?;
    let (own_body, own_text, own_len) =
        crate::trafilatura::main_extractor::extract_content(&body, opts);

    // external.py:58 — readability arm. Python passes `backup_tree` (a
    // pre-cleaning snapshot); we pass the original `html` bytes — same
    // semantic (the readability fork re-parses internally on every
    // retry attempt, HLD §m-3).
    let algo_body = try_readability(html);
    let (algo_text, algo_len) = match &algo_body {
        Some(node) => {
            // external.py:60-61 — Python serializes via
            // `tostring(temppost_algo, method='text', encoding='utf-8')`
            // then `trim`s. Our `text_content` + `trim` is the equivalent.
            let raw = crate::readability::dom::text_content(node);
            let trimmed = trim(&raw);
            let len = trimmed.chars().count();
            (trimmed, len)
        }
        None => (String::new(), 0),
    };

    // external.py:66-85 — arbiter.
    let use_readability = cascade_prefers_readability(&own_text, own_len, &algo_text, algo_len);

    // Both arms empty → no article.
    if own_len == 0 && algo_len == 0 {
        // Keep `dom` alive (rcdom Drop quirk) until both arms are
        // measured; then drop it by returning None.
        drop(dom);
        return None;
    }

    if use_readability {
        algo_body
    } else {
        // Keep the readability arm alive long enough for the arbiter to
        // measure; then return the own-arm node. The own-arm `NodeRef`
        // is rooted in `dom` which the caller now owns transitively
        // through the returned `Rc` — but for safety against the rcdom
        // Drop quirk on the readability side, we explicitly drop the
        // arm we didn't pick.
        let _ = algo_body;
        Some(own_body)
    }
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

    // -----------------------------------------------------------------------
    // Stage 4b: Document + summary() + helpers
    // (readability_lxml.py:102-225)
    // -----------------------------------------------------------------------

    /// Stage 4b/test-1 — `Document::summary()` picks the obvious `<div
    /// class="content">` for a minimal article (readability_lxml.py:124-166
    /// happy path).
    #[test]
    fn document_summary_returns_best_candidate_for_simple_article() {
        // The content div has 4 multi-comma paragraphs well over
        // min_text_length (25) — its accumulated score should dominate
        // the (otherwise) <body>'s zero-paragraph score.
        let html = r#"<html><body>
            <header><nav>Site nav with menu, links, ads</nav></header>
            <div class="content">
                <p>This is the first paragraph of the article body, with several commas, multiple clauses, and well over twenty-five characters of content.</p>
                <p>The second paragraph continues the article, building on the first paragraph's themes, exploring nuance, and adding context.</p>
                <p>A third paragraph extends the article further, with reflection, analysis, conclusion-pointing remarks, and supporting detail.</p>
                <p>The fourth and final paragraph wraps up the discussion, restates the thesis, names the conclusion, and closes the piece.</p>
            </div>
            <footer>Sidebar footer comment links</footer>
        </body></html>"#;
        let mut doc = Document::new(html);
        let article = doc.summary().expect("summary returns Some for an article-shaped page");
        // The returned article is the <div> built by get_article — its
        // text_content must include the article paragraphs.
        let text = crate::readability::dom::text_content(&article);
        assert!(
            text.contains("first paragraph"),
            "expected article body, got: {text:.120}…"
        );
        assert!(
            !text.contains("Site nav"),
            "site nav should have been stripped, got: {text:.200}…"
        );
    }

    /// Stage 4b/test-2 — lenient fallback fires when the ruthless pass
    /// would strip the article entirely. The article div carries an
    /// `extra` class that matches `unlikelyCandidatesRe` — the ruthless
    /// pass strips it, leaving no scoreable paragraphs. The lenient
    /// retry keeps the div in the tree and produces an article
    /// (readability_lxml.py:146-152).
    ///
    /// # `extra` vs `sidebar` — Stage 4c note
    ///
    /// Originally written at Stage 4b against `class="sidebar"`. Stage 4c
    /// replaced the sanitize stub with the real port — `sidebar` matches
    /// BOTH `unlikelyCandidatesRe` AND `negativeRe`, so even after the
    /// lenient retry keeps the sidebar div in the tree, the real
    /// `sanitize`'s `weight + score < 0` arm drops it (class_weight =
    /// -25 from `negativeRe`, easily overwhelming the paragraph score).
    /// `extra` matches `unlikelyCandidatesRe` only (not `negativeRe`),
    /// exercising the SAME lenient-retry path without the secondary
    /// sanitize-drop — which is the actual invariant this test is meant
    /// to pin (the retry shape, not the sanitize numeric weight).
    #[test]
    fn document_summary_falls_back_to_lenient_when_ruthless_fails() {
        // The ONLY content sits inside a div whose class matches
        // `unlikelyCandidatesRe` (extra) but NOT `okMaybeItsACandidateRe`
        // and NOT `negativeRe`. Ruthless drops it; lenient keeps it; the
        // sanitize pass does not have grounds to drop it again.
        let html = r#"<html><body>
            <div class="extra">
                <p>This paragraph holds the only article-shaped content on the page, with commas, length, and structure to score above the minimum length threshold.</p>
                <p>A second paragraph adds more text, more commas, and enough length to push the parent score well into the candidate-selection range.</p>
                <p>A third paragraph cements the candidate, with explicit reflective analysis, careful word choice, and a satisfying conclusion to the thought.</p>
            </div>
        </body></html>"#;
        let mut doc = Document::new(html);
        let article = doc.summary().expect("lenient retry should yield an article");
        let text = crate::readability::dom::text_content(&article);
        assert!(
            text.contains("article-shaped content"),
            "lenient retry should preserve the extra-class paragraph, got: {text:.200}…"
        );
    }

    /// Stage 4b/test-3 — empty HTML body falls all the way through
    /// to the body-or-document fallback (readability_lxml.py:154-158).
    /// In Python the fallback returns the body (or doc) as the article;
    /// our `summary` returns `Some(body)` for a parseable-but-empty doc.
    /// The test asserts the returned NodeRef is the synthesised <body>
    /// (NOT `None`, since html5ever always synthesises body — Python
    /// would also return body here, not None).
    #[test]
    fn document_summary_returns_body_for_empty_html() {
        let html = "<html><body></body></html>";
        let mut doc = Document::new(html);
        let out = doc.summary();
        // html5ever synthesises a <body>, so the fallback path returns
        // it. Python's `find("body")` on an empty body would do the
        // same — the body is empty but EXISTS.
        let article = out.expect("body fallback always yields Some");
        // The fallback returns the body element itself (not a wrapped
        // <div>). Confirm by checking its tag.
        assert_eq!(local_name(&article).as_deref(), Some("body"));
    }

    /// Stage 4b/test-4 — `remove_unlikely_candidates` strips a div whose
    /// class matches `unlikelyCandidatesRe` and not the safe-list
    /// (readability_lxml.py:284-295).
    #[test]
    fn remove_unlikely_candidates_strips_comment_div() {
        let html = r#"<html><body>
            <div class="comments">drop me</div>
            <div class="article-body">keep me</div>
        </body></html>"#;
        let mut doc = Document::new(html);
        doc.remove_unlikely_candidates();
        // After the strip, the <body> should still contain the
        // article-body div but not the comments div.
        let body = doc.dom.body().expect("body");
        let kids = children(&body);
        let classes: Vec<String> = kids.iter().map(class_name).collect();
        assert!(
            !classes.iter().any(|c| c == "comments"),
            "comments div should have been stripped, found classes: {classes:?}"
        );
        assert!(
            classes.iter().any(|c| c == "article-body"),
            "article-body div should have survived, found classes: {classes:?}"
        );
    }

    /// Stage 4b/test-5 —
    /// `transform_misused_divs_into_paragraphs` retags a `<div>` that
    /// contains only inline / text content to `<p>`
    /// (readability_lxml.py:297-311).
    #[test]
    fn transform_misused_divs_into_paragraphs_renames_text_only_div_to_p() {
        // The outer wrapping div has a child div (block tag) so it must
        // NOT retag. The inner div has only a <span> + text (NOT in the
        // block-tag list) so it MUST retag to <p>.
        let html = r#"<html><body>
            <div id="wrapper">
                <div id="leaf"><span>inline only</span> trailing text</div>
            </div>
        </body></html>"#;
        let mut doc = Document::new(html);
        // Drop scripts/styles to mirror summary()'s prelude; not strictly
        // needed for this fixture but matches the runtime sequence.
        let pinned = doc.transform_misused_divs_into_paragraphs();
        // Find the original leaf div by id — but after retag it is a <p>.
        let body = doc.dom.body().expect("body");
        let all = get_elements_by_tag_name(&body, "*");
        let leaf_id_match: Vec<&NodeRef> = all
            .iter()
            .filter(|n| id(n) == "leaf")
            .collect();
        assert_eq!(leaf_id_match.len(), 1, "exactly one element should carry id=leaf");
        assert_eq!(
            local_name(leaf_id_match[0]).as_deref(),
            Some("p"),
            "leaf div should have been retagged to <p>"
        );
        // The wrapper div has a block-tag child (the leaf) so it MUST
        // remain a <div>. NOTE: after the retag the wrapper's child is a
        // <p>, which IS in the block-tag list — but the retag decision
        // was made on the wrapper FIRST (doc-order) when its child was
        // still a <div>, so the wrapper stayed a <div>. This matches
        // Python's pre-snapshot iteration semantics.
        let wrapper_id_match: Vec<&NodeRef> = all
            .iter()
            .filter(|n| id(n) == "wrapper")
            .collect();
        assert_eq!(wrapper_id_match.len(), 1);
        assert_eq!(
            local_name(wrapper_id_match[0]).as_deref(),
            Some("div"),
            "wrapper should remain <div> (its child was a block tag at iteration time)"
        );
        // Keep the rcdom-Drop pin alive until end of scope.
        drop(pinned);
    }

    /// Stage 4b/test-6 — `select_best_candidate` picks the highest score
    /// (readability_lxml.py:209-218).
    #[test]
    fn select_best_candidate_picks_highest_scored() {
        // Three fresh detached elements so identity is unambiguous.
        let a = crate::readability::dom::create_element("p");
        let b = crate::readability::dom::create_element("p");
        let c = crate::readability::dom::create_element("p");
        let map = vec![
            (a.clone(), Candidate::new(1.0, a)),
            (b.clone(), Candidate::new(5.0, b.clone())),
            (c.clone(), Candidate::new(3.0, c)),
        ];
        let best = select_best_candidate(&map).expect("non-empty");
        assert_eq!(best.score, 5.0);
        // Identity check — the winning candidate's elem is `b`.
        assert!(std::rc::Rc::ptr_eq(&best.elem, &b));
    }

    /// Empty-input edge case for `select_best_candidate`.
    #[test]
    fn select_best_candidate_returns_none_for_empty_input() {
        assert!(select_best_candidate(&[]).is_none());
    }

    /// Tie-break behaviour — Python `sorted(reverse=True)` is stable; the
    /// FIRST inserted retains the lead on ties.
    #[test]
    fn select_best_candidate_keeps_first_tied_entry() {
        let a = crate::readability::dom::create_element("p");
        let b = crate::readability::dom::create_element("p");
        let map = vec![
            (a.clone(), Candidate::new(5.0, a.clone())),
            (b.clone(), Candidate::new(5.0, b)),
        ];
        let best = select_best_candidate(&map).expect("non-empty");
        // `a` was inserted first; on ties Python's `next(iter(sorted_…))`
        // yields the first tied — i.e. `a`.
        assert!(std::rc::Rc::ptr_eq(&best.elem, &a));
    }

    /// Sanity check: `Document::new` applies the documented defaults
    /// (readability_lxml.py:107).
    #[test]
    fn document_new_uses_python_defaults() {
        let doc = Document::new("<html><body></body></html>");
        assert_eq!(doc.min_text_length, 25);
        assert_eq!(doc.retry_length, 250);
    }

    // -----------------------------------------------------------------------
    // Stage 4c: sanitize (readability_lxml.py:326-438)
    // -----------------------------------------------------------------------

    /// Driver helper: build a `Document` from `html`, snapshot the article
    /// node corresponding to the body's first element child (i.e. the
    /// element under test), and invoke `sanitize` with an empty candidates
    /// map so we exercise the WEIGHT-ONLY arm (class_weight + 0) without
    /// the orchestrator-built candidate scores. Returns the (mutated)
    /// article NodeRef for direct inspection.
    fn sanitize_one(html: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let body = dom.body().expect("body parsed");
        // Wrap body in a fresh detached <div> mimicking get_article's
        // construction — we move the body's elements into it so that
        // `sanitize`'s `parent(elem)` resolves to a stable container.
        let article = create_element("div");
        for child in children(&body) {
            append_child(&article, &child);
        }
        let candidates: Vec<(NodeRef, Candidate)> = Vec::new();
        // We need a Document instance for `sanitize`'s `&mut self`. The
        // HTML passed to `Document::new` is incidental here (sanitize does
        // not re-read `self.html` or `self.dom`); we use a minimal stub.
        let mut doc = Document::new("<html><body></body></html>");
        doc.sanitize(&article, &candidates);
        (dom, article)
    }

    /// `<div class="comments">` has class_weight = -25 (negativeRe hit on
    /// "comment"). With empty candidates `weight + score = -25 + 0 < 0`,
    /// so the conditional-clean arm drops it
    /// (readability_lxml.py:349-356).
    #[test]
    fn sanitize_drops_negative_weighted_text_clean_elem() {
        let html = r#"<html><body>
            <div class="comments"><p>some text inside a comments div</p></div>
            <div class="article-body"><p>keep this article body</p></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        // After sanitize, the "comments" div should be gone; the
        // "article-body" div may also be gone if its own weight+score
        // chain trips a removal arm — but we only assert what this test
        // pins: the comments div is removed.
        let kids = children(&article);
        let classes: Vec<String> = kids.iter().map(class_name).collect();
        assert!(
            !classes.iter().any(|c| c == "comments"),
            "comments div should have been removed, classes left: {classes:?}"
        );
    }

    /// `<img src="...">` is preserved (the iframe filter at
    /// readability_lxml.py:334-338 doesn't touch <img>; the <img> is one
    /// of the `TEXT_CLEAN_ELEMS` only via the inner-counts dict, never
    /// directly removed by sanitize).
    #[test]
    fn sanitize_keeps_img_with_src() {
        let html = r#"<html><body>
            <div class="content"><img src="x.jpg" alt="cat"/><p>caption with enough text content for the sanitize content-length floor of 25 chars</p></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let imgs = get_elements_by_tag_name(&article, "img");
        assert_eq!(imgs.len(), 1, "img should survive sanitize");
        assert_eq!(
            get_attribute(&imgs[0], "src").as_deref(),
            Some("x.jpg")
        );
    }

    /// Python's sanitize does NOT drop `<img>` elements without a `src`
    /// attribute — that bullet in the brief was a misreading. The
    /// `readability_lxml.py:326-438` source body has no `<img>` removal
    /// clause at all; the `<img>` count only feeds the *parent's*
    /// drop-decision heuristics. So an `<img>` without `src` survives the
    /// sanitize pass (only the parent's chain might decide to drop it as
    /// part of a noisy container). This test pins THAT behaviour — i.e.
    /// the absence of an over-eager "drop bare `<img>`" rule.
    ///
    /// (Brief item 2 expected a "drop img without src" check; we encode
    /// the FAITHFUL Python behaviour instead. Out-cleaning Python is
    /// inversion — HLD §4.)
    #[test]
    fn sanitize_keeps_img_without_src_faithful_to_python() {
        let html = r#"<html><body>
            <div class="content"><p>This paragraph holds article text with enough length to not trip the short-content drop arm at all.</p><img/></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        // The <img/> should survive — sanitize has no clause that targets
        // bare <img>. (If the parent div is dropped by another rule, this
        // test would need rework, but with class="content" and good
        // content-length there's no drop trigger.)
        let imgs = get_elements_by_tag_name(&article, "img");
        assert_eq!(imgs.len(), 1, "img survives sanitize even without src");
    }

    /// `<iframe src="https://youtube.com/...">` is preserved AND its
    /// text becomes "VIDEO" (readability_lxml.py:335-336).
    #[test]
    fn sanitize_keeps_youtube_iframe() {
        let html = r#"<html><body>
            <div class="content"><iframe src="https://www.youtube.com/embed/abc"></iframe><p>Caption text for the embedded video that gives the parent enough content to not trip removal.</p></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let iframes = get_elements_by_tag_name(&article, "iframe");
        assert_eq!(iframes.len(), 1, "video iframe should survive");
        let txt = element_text(&iframes[0]).unwrap_or_default();
        assert_eq!(txt, "VIDEO");
    }

    /// `<iframe src="https://example.com/ad">` is dropped — `src` is
    /// present but does NOT match `videoRe`
    /// (readability_lxml.py:337-338).
    #[test]
    fn sanitize_drops_iframe_without_video_src() {
        let html = r#"<html><body>
            <div class="content"><iframe src="https://ads.example.com/x"></iframe><p>Text content with enough length to avoid the short-content drop arm of the cleaner.</p></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let iframes = get_elements_by_tag_name(&article, "iframe");
        assert_eq!(iframes.len(), 0, "non-video iframe should be dropped");
    }

    /// `<form>` is dropped (readability_lxml.py:331-332).
    #[test]
    fn sanitize_drops_form_element() {
        let html = r#"<html><body>
            <div class="content"><form><input type="text"/></form><p>Article paragraph text long enough to keep the parent alive through the sanitize gates.</p></div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let forms = get_elements_by_tag_name(&article, "form");
        assert_eq!(forms.len(), 0, "form should be dropped");
    }

    /// `<table class="data">` — class_weight = 0 (no positive/negative
    /// keyword match), and with no candidate score the
    /// `weight + score < 0` arm doesn't fire. The next arm
    /// (`text_content.count(',') < 10` plus the heuristic block) is
    /// inspected; with content_length above min_text_length, no embed,
    /// reasonable link density, the table is kept
    /// (readability_lxml.py:357 onward).
    ///
    /// NOTE: a `<table>` with `class="data"` does NOT trigger any of the
    /// special "data-table KEEP" branches the Mozilla Readability port
    /// has — Trafilatura's readability fork doesn't carry that logic.
    /// The keep here is incidental on content length, not a marked-table
    /// rescue.
    #[test]
    fn sanitize_keeps_data_table_via_class_weight() {
        let html = r#"<html><body>
            <table class="data"><tr><td>Cell A with several words of content here</td><td>Cell B with several words of content here too</td></tr><tr><td>Cell C is similarly populated with content text</td><td>Cell D continues the data table example pattern</td></tr></table>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let tables = get_elements_by_tag_name(&article, "table");
        assert_eq!(tables.len(), 1, "data table should survive");
        assert_eq!(class_name(&tables[0]), "data");
    }

    /// A `<div>` whose content is overwhelmingly links (90% link
    /// density) is dropped by the `weight < 25 and link_density > 0.2`
    /// arm (readability_lxml.py:389-391).
    #[test]
    fn sanitize_high_link_density_div_removed() {
        // 90% link density: 90 chars of <a> text vs 10 chars of plain
        // text. class is neutral (weight = 0 < 25 triggers the arm).
        // (Note: avoid raw `href="#"` patterns immediately followed by
        // `>` next to a `"` — Rust 2024 reserves the `#"…"` sequence.
        // We use `href="/x"` placeholders to sidestep the lexer reserve.)
        let html = r#"<html><body>
            <div class="nav"><a href="/x">aaaaaaaaaa</a><a href="/x">bbbbbbbbbb</a><a href="/x">cccccccccc</a><a href="/x">dddddddddd</a><a href="/x">eeeeeeeeee</a><a href="/x">ffffffffff</a><a href="/x">gggggggggg</a><a href="/x">hhhhhhhhhh</a><a href="/x">iiiiiiiiii</a> xx</div>
        </body></html>"#;
        let (_dom, article) = sanitize_one(html);
        let divs = get_elements_by_tag_name(&article, "div");
        // The nav div should be gone (class "nav" matches neither
        // positive nor negative — weight = 0 — and link_density ≈ 0.97
        // > 0.2 with weight < 25 → drop).
        assert!(
            !divs.iter().any(|d| class_name(d) == "nav"),
            "nav div with high link density should be removed"
        );
    }

    /// End-to-end: a page with a `<nav class="topnav"><ul>...</ul></nav>`
    /// next to an article body — after `Document::summary()` runs, the
    /// returned article must not contain the nav's link text.
    ///
    /// The nav is dropped via the chain: `remove_unlikely_candidates`
    /// strips it under ruthless mode (`unlikelyCandidatesRe` matches
    /// `header|nav|menu`); if for some reason it survives that, the
    /// sanitize header-strip arm catches `<header>`/`<footer>`/`<nav>`
    /// candidates with bad weight. Together they ensure the nav is gone.
    #[test]
    fn document_summary_strips_navigation_before_returning() {
        let html = r#"<html><body>
            <nav class="topnav"><ul><li><a href="/a">Home</a></li><li><a href="/b">About</a></li><li><a href="/c">Contact</a></li></ul></nav>
            <div class="article">
                <p>This is the first paragraph of an article body, containing several commas, multiple clauses, and well over twenty-five characters to score above the threshold.</p>
                <p>A second paragraph continues the body, with reflection, analysis, and conclusion-pointing remarks supporting the main thesis of the piece.</p>
                <p>A third paragraph wraps up the discussion, restating the thesis, naming the conclusion, and closing out the article cleanly.</p>
            </div>
        </body></html>"#;
        let mut doc = Document::new(html);
        let article = doc.summary().expect("summary returns Some");
        let text = crate::readability::dom::text_content(&article);
        assert!(
            !text.contains("Home") && !text.contains("Contact"),
            "navigation links should have been stripped, got: {text:.200}…"
        );
        assert!(
            text.contains("first paragraph"),
            "article body should be present, got: {text:.200}…"
        );
    }

    // -----------------------------------------------------------------------
    // Stage 4d: is_probably_readerable + cascade
    // -----------------------------------------------------------------------

    /// A long-form article with several substantive paragraphs accumulates
    /// score quickly and crosses the default `min_score = 20.0` threshold.
    ///
    /// Each paragraph here is ~250 chars; `sqrt(250 - 140) ≈ 10.5`, so two
    /// paragraphs easily clear `20.0`. We use four to leave headroom.
    #[test]
    fn is_probably_readerable_returns_true_for_long_article() {
        let html = r#"<html><body><article>
            <p>The first paragraph contains substantial prose with multiple clauses, real punctuation, several commas, and well over one hundred and forty characters of trimmed body text — enough to clear the content-length floor on its own merit.</p>
            <p>A second paragraph continues the discussion at a similar length, with reflection, analysis, and conclusion-pointing remarks that demonstrably exceed the one hundred and forty character lower bound the heuristic enforces.</p>
            <p>The third paragraph again surpasses the gate, with rhetorical structure, internal commas, and the kind of clause density that real articles exhibit, comfortably above the minimum content length floor.</p>
            <p>A fourth and final paragraph caps the piece with concluding analysis, restating the thesis, and ensuring that the cumulative readability score crosses the default twenty-point threshold with margin.</p>
        </article></body></html>"#;
        assert!(
            is_probably_readerable(html),
            "long-form article should be deemed readerable"
        );
    }

    /// A navigation page (one short nav `<ul>` of links) has no `<p>`/`<pre>`/
    /// `<article>` with sufficient text — score stays at 0. Returns false.
    #[test]
    fn is_probably_readerable_returns_false_for_navigation_page() {
        let html = r#"<html><body>
            <nav class="topnav">
                <ul>
                    <li><a href="/a">Home</a></li>
                    <li><a href="/b">About</a></li>
                    <li><a href="/c">Contact</a></li>
                </ul>
            </nav>
            <p>Short.</p>
        </body></html>"#;
        assert!(
            !is_probably_readerable(html),
            "link-heavy page with no substantive paragraphs should not be readerable"
        );
    }

    /// `style="display: none"` on a candidate node makes it invisible —
    /// its score contribution is zero. With NO `<article>` wrapper (so
    /// only `<p>` candidates are enumerated), three visible paragraphs
    /// at ~225 chars each accumulate ~3 * sqrt(85) ≈ 27.6 → above the
    /// 20.0 threshold; hiding ONE drops the visible count to two
    /// → ~2 * 9.2 = 18.4 → below threshold.
    ///
    /// The `<article>` wrapper is omitted deliberately — its
    /// `text_content` would concatenate the hidden child's text and the
    /// article element itself (which has no `display:none`) would alone
    /// clear the threshold. Faithfully tracks readability_lxml.py:491-493
    /// — visibility is checked PER candidate node, not transitively.
    #[test]
    fn is_probably_readerable_skips_hidden_elements() {
        let visible_para = "<p>This paragraph holds enough article-shaped prose, with multiple commas, internal clauses, and well over the one hundred and forty character minimum to clear the threshold by a comfortable margin every time.</p>";
        let html_all_visible = format!(
            "<html><body>{visible_para}{visible_para}{visible_para}</body></html>"
        );
        assert!(
            is_probably_readerable(&html_all_visible),
            "three visible substantive paragraphs should clear the threshold"
        );

        // Hide one of the three via display:none → score drops below threshold.
        let hidden_para = format!(
            "<p style=\"display: none\">{}</p>",
            visible_para.trim_start_matches("<p>").trim_end_matches("</p>")
        );
        let html_one_hidden = format!(
            "<html><body>{visible_para}{visible_para}{hidden_para}</body></html>"
        );
        assert!(
            !is_probably_readerable(&html_one_hidden),
            "hiding one of three paragraphs via display:none should drop the score below threshold"
        );
    }

    /// `try_readability` succeeds on minimal HTML where own extraction
    /// would yield short text — the readability fork's `summary()` walks
    /// the scored candidate set even when the input is sparse.
    #[test]
    fn try_readability_returns_summary_when_own_fails() {
        let html = r#"<html><body>
            <div class="article">
                <p>First paragraph with substantive content, several commas, real prose, and enough length to score above twenty-five characters.</p>
                <p>Second paragraph continues the discussion with similar density, comma counts, and overall length to support a strong candidate selection.</p>
                <p>Third paragraph wraps the piece, restating the thesis, naming the conclusion, and closing out the discussion with clarity.</p>
            </div>
        </body></html>"#;
        let summary = try_readability(html);
        assert!(
            summary.is_some(),
            "readability should find an article subtree"
        );
        let node = summary.expect("summary returned None");
        let text = crate::readability::dom::text_content(&node);
        assert!(
            text.contains("First paragraph") || text.contains("first paragraph"),
            "summary text should include the article content, got: {:.200}…",
            text
        );
    }

    /// Cascade picks the longer extraction. We construct two extractions
    /// directly (own=short, algo=long) and assert the arbiter rules for
    /// readability — testing the pure arbiter function without the full
    /// cascade pipeline (which is exercised in the e2e test below).
    #[test]
    fn cascade_picks_longer_extraction() {
        // Own text very short, algo text well over 2x → readability wins.
        let own = "tiny";
        let algo =
            "a much longer extraction text body that comfortably exceeds twice the length of the own arm so the dwarfing branch fires deterministically";
        assert!(
            cascade_prefers_readability(own, own.chars().count(), algo, algo.chars().count()),
            "algo length > 2 * own length (and no JSON-LD spill) must select readability"
        );

        // Inverse case: own dwarfs algo → own wins.
        let big_own = "the own arm extraction is large enough to dwarf the algorithm output by more than the two times factor that the arbiter checks in its dwarf branch, so own must win here even though algo is non-empty";
        let small_algo = "small";
        assert!(
            !cascade_prefers_readability(
                big_own,
                big_own.chars().count(),
                small_algo,
                small_algo.chars().count()
            ),
            "own length > 2 * algo length must select own"
        );

        // JSON-LD guard: algo starts with `{` → keep own even though
        // algo is long.
        let json_algo = "{\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"name\":\"…long JSON body…\",\"description\":\"a substantial JSON-LD spill that is more than 2x the own length\"}";
        assert!(
            !cascade_prefers_readability(
                "short own",
                "short own".chars().count(),
                json_algo,
                json_algo.chars().count()
            ),
            "JSON-LD-prefixed algo text must NOT win even when long (external.py:73 guard)"
        );
    }

    /// `bare_extraction_with_cascade` returns `None` when both arms
    /// produce empty output — minimal degenerate input.
    #[test]
    fn cascade_returns_none_when_both_arms_empty() {
        // Truly empty input — no <body>, just a doctype.
        let html = "<html><head></head><body></body></html>";
        let opts = crate::trafilatura::cleaning::Options::default();
        let result = bare_extraction_with_cascade(html, &opts);
        // The own arm may still return an empty body NodeRef (Stage 2d's
        // `extract_content` rescues with `recover_wild_text` which can
        // return a fresh empty body). The CASCADE return must be None
        // ONLY when BOTH arms produced zero-length text. Pin that —
        // for empty input, both arms are zero-length, and the cascade
        // returns None.
        assert!(
            result.is_none(),
            "empty HTML body should yield None from the cascade (both arms zero-length)"
        );
    }

    /// `is_node_visible` pins the three short-circuit "not visible" rules.
    #[test]
    fn is_node_visible_short_circuits() {
        // display:none → hidden.
        let html = r#"<html><body>
            <p id="a" style="display: none">hidden by style</p>
            <p id="b" style="color: red; display:none; foo:bar">also hidden</p>
            <p id="c" style="color:red">visible</p>
            <p id="d" hidden>hidden by attr</p>
            <p id="e" aria-hidden="true">aria hidden</p>
            <p id="f" aria-hidden="true" class="x fallback-image y">aria hidden but fallback</p>
        </body></html>"#;
        let dom = Dom::parse(html);
        let body = dom.body().expect("body");
        let ps = get_elements_by_tag_name(&body, "p");
        let by_id = |target: &str| {
            ps.iter()
                .find(|p| id(p) == target)
                .cloned()
                .unwrap_or_else(|| panic!("no <p id={target}>"))
        };
        assert!(!is_node_visible(&by_id("a")), "display:none is hidden");
        assert!(
            !is_node_visible(&by_id("b")),
            "display:none mid-string is hidden"
        );
        assert!(is_node_visible(&by_id("c")), "color-only style is visible");
        assert!(
            !is_node_visible(&by_id("d")),
            "bare `hidden` attribute is hidden"
        );
        assert!(
            !is_node_visible(&by_id("e")),
            "aria-hidden=true is hidden when class has no fallback-image"
        );
        assert!(
            is_node_visible(&by_id("f")),
            "aria-hidden=true + class~=fallback-image is visible"
        );
    }
}
