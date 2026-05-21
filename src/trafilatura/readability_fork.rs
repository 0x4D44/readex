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
    element_text, get_all_nodes_with_tag, get_elements_by_tag_name, id, local_name, parent,
    replace_element_tag, serialize_converted_tree,
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

/// `TEXT_CLEAN_ELEMS` — readability_lxml.py:60. Used by Stage 4c's
/// `sanitize` cleanup pass.
#[allow(dead_code)] // Stage 4c consumer
pub(crate) const TEXT_CLEAN_ELEMS: &[&str] = &["p", "img", "li", "a", "embed", "input"];

/// `FRAME_TAGS` — readability_lxml.py:82. Top-level container tags that
/// are *never* dropped by `remove_unlikely_candidates` (Stage 4b).
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

            // readability_lxml.py:160-161 — sanitize stub for Stage 4b
            // (Stage 4c fills this in; today it serializes the article
            // and returns its length so the retry-trigger gate below
            // still fires on a quantitative signal).
            let cleaned_article = self.stage4b_sanitize_stub(&article);
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

    /// Stage-4b PLACEHOLDER for `sanitize` (readability_lxml.py:326-438).
    ///
    /// Stage 4c will replace this with the faithful sanitize port. Today
    /// it serializes the article to a string so [`summary`]'s retry-trigger
    /// `article_length < retry_length` gate still has a quantitative
    /// signal to fire on. This is INTENTIONALLY a stub — the retry-loop
    /// SHAPE is faithful (the gate exists and is checked); the THRESHOLD
    /// behaviour will tighten as Stage 4c's real sanitize narrows the
    /// output.
    ///
    /// HLD §10 anti-inversion: marking this as `_stage4b_` (leading
    /// underscore) keeps it discoverable + greppable for the Stage 4c
    /// implementer to delete in one stroke.
    fn stage4b_sanitize_stub(&self, article: &NodeRef) -> String {
        serialize_converted_tree(article)
    }
}

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
    /// would strip the article entirely. The article div carries a
    /// `sidebar` class that matches `unlikelyCandidatesRe` — the ruthless
    /// pass strips it, leaving no scoreable paragraphs. The lenient
    /// retry keeps the div in the tree and produces an article
    /// (readability_lxml.py:146-152).
    #[test]
    fn document_summary_falls_back_to_lenient_when_ruthless_fails() {
        // The ONLY content sits inside a div whose class matches
        // `unlikelyCandidatesRe` (sidebar) but NOT `okMaybeItsACandidateRe`.
        // Ruthless drops it; lenient keeps it.
        let html = r#"<html><body>
            <div class="sidebar">
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
            "lenient retry should preserve the sidebar paragraph, got: {text:.200}…"
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
}
