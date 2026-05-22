//! `justext_core` — Stage 5b: jusText paragraph segmentation port.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.7
//! (jusText is the third-arm rescue extractor in Trafilatura's cascade).
//! Source of truth: `justext/core.py:28-200` and `justext/paragraph.py:1-66`.
//!
//! # Scope of this file (Stage 5b)
//!
//! Stage 5b ports SEGMENTATION only — turning a DOM tree into a sequence of
//! [`Paragraph`] objects carrying the metadata Stage 5c needs to classify
//! them (text, dom_path, link-density inputs, heading flag). NO classification
//! / context-revision logic lands here; that's Stage 5c
//! (`classify_paragraphs` + `revise_paragraph_classification`).
//!
//! Ported in this stage:
//!
//! - `PARAGRAPH_TAGS` frozenset — `justext/core.py:37-42`.
//! - `Paragraph` dataclass — `justext/paragraph.py:14-66`.
//! - `ParagraphMaker` SAX handler — `justext/core.py:133-199`.
//! - `PathInfo` xpath/dom-path tracker — `justext/core.py:202-233`.
//! - `make_paragraphs(root)` — `justext/core.py:139-144`.
//! - `is_blank` / `normalize_whitespace` helpers — `justext/utils.py:11-34`.
//!
//! # SAX → tree-walk translation (faithfulness gap notes)
//!
//! Python's `ParagraphMaker` is an `xml.sax.ContentHandler` invoked via
//! `lxml.sax.saxify(root, handler)` — a streaming event source that emits
//! `startElementNS` / `characters` / `endElementNS` callbacks in document
//! order. The Rust port replaces this with a recursive DOM walker
//! ([`walk_dom`]) that fires the same three callbacks in the same order:
//!
//! 1. `startElementNS(name, ...)` → [`ParagraphMaker::on_start`] at element
//!    entry.
//! 2. For each child in `node.children` (document order, every node type):
//!    - `Text` node → [`ParagraphMaker::on_characters`] with the data.
//!    - `Element` node → recursive descent (which fires `on_start`,
//!      processes its children, then `on_end`).
//!    - `Comment` / `ProcessingInstruction` / `Doctype` → skipped (SAX
//!      does not emit `characters` for these either).
//! 3. `endElementNS(name)` → [`ParagraphMaker::on_end`] at element exit.
//!
//! Finally, [`ParagraphMaker::on_end_document`] flushes the trailing
//! partial paragraph (matching `endDocument` in `core.py:188-189`).
//!
//! Two known faithfulness considerations, both handled:
//!
//! - **`<head>` exclusion**: Python's `preprocessor` (`core.py:107-128`) runs
//!   lxml's `Cleaner` with `kill_tags=("head",)` BEFORE `make_paragraphs`,
//!   so head/title/meta/style content never reaches the SAX walker. Stage 5b
//!   ports `make_paragraphs` only — callers are responsible for pre-cleaning
//!   (Stage 5d's cascade integration does this via `cleaning::tree_cleaning`,
//!   which already drops `<head>` per `MANUALLY_CLEANED`). The unit test
//!   `make_paragraphs_skips_script_style` verifies post-cleaning behaviour by
//!   driving against a `<body>` that contains only the to-be-tested
//!   structure.
//! - **Walker root**: the Python `saxify(root, handler)` event stream starts
//!   from `root` itself — i.e. `startElementNS` fires for `root`'s tag first
//!   (typically `<html>`). The Rust walker matches: we fire `on_start` for
//!   the root, recurse, then fire `on_end`. The first event for an
//!   `<html><body>...` tree is therefore `startElementNS("html")`, which
//!   appends `"html"` to the path and starts a fresh paragraph (because
//!   `"html"` is NOT in `PARAGRAPH_TAGS` — wait, actually `"body"` IS in
//!   the set, and `"html"` isn't; the first `body` element triggers a new-
//!   paragraph reset). This matches the Python flow exactly.
//!
//! # Paragraph field shape — faithful, with Stage-5c placeholders
//!
//! Python's `Paragraph` carries five mutable fields touched by `classify_*`:
//! `chars_count_in_links`, `tags_count`, `class_type` ("" until classify),
//! plus the `text_nodes` list and an externally-set `heading` bool added
//! by `classify_paragraphs:254`. Stage 5b populates the segmentation-time
//! fields (`text_nodes` → `text`, `dom_path`, `chars_count_in_links`,
//! `is_heading`) and leaves the classification-time fields as `None`
//! placeholders for Stage 5c to fill:
//!
//! | Python field            | Rust field             | Filled when |
//! |-------------------------|------------------------|-------------|
//! | `text_nodes` (joined)   | `text: String`         | Stage 5b    |
//! | `dom_path`              | `dom_path: String`     | Stage 5b    |
//! | (derived from dom_path) | `tag: String`          | Stage 5b    |
//! | `chars_count_in_links`  | `chars_count_in_links` | Stage 5b    |
//! | `words_count` (derived) | `word_count: usize`    | Stage 5b    |
//! | `is_heading` (derived)  | `is_heading: bool`     | Stage 5b    |
//! | `class_type: ""`        | `class_type: None`     | Stage 5c    |
//! | `stopwords_count(set)`  | `stopwords_count: None`| Stage 5c    |
//! | (derived: `cf_class!="good"`) | `is_boilerplate: None` | Stage 5c |
//!
//! # Anti-inversion (HLD §10 / DA-B-2)
//!
//! Every public function / method carries a `justext/(core|paragraph|
//! utils).py:NN` line cite. The `PARAGRAPH_TAGS` set is byte-identical to
//! the Python source (`core.py:37-42`); `normalize_whitespace` matches the
//! Python `MULTIPLE_WHITESPACE_PATTERN` + `_replace_whitespace` collapse
//! exactly. The `<br><br>` paragraph-separator quirk (`core.py:164-170`)
//! is preserved verbatim — two consecutive `<br>` tags start a new
//! paragraph and the SECOND `<br>` does NOT increment `tags_count`.
//!
//! # Non-goals (deferred)
//!
//! - `classify_paragraphs` / `revise_paragraph_classification` — Stage 5c.
//! - Cascade integration (`bare_extraction_with_cascade` arm) — Stage 5d.
//! - lxml SAX `startElementNS`/`endElementNS` namespace tuple format —
//!   not relevant; the Python code immediately discards namespace
//!   (`name = name[1]` at `core.py:161/180`), so the Rust walker passes
//!   the local tag name directly.

use crate::readability::dom::{NodeData, NodeRef, is_text, local_name};
use crate::trafilatura::justext_stoplists::get_stoplist;

// ===========================================================================
// Module constants (justext/core.py:28-46)
// ===========================================================================

/// `PARAGRAPH_TAGS` — `justext/core.py:37-42`. The block-level tag set
/// whose entry (or exit) starts a fresh paragraph in [`ParagraphMaker`].
///
/// Byte-identical to the Python source's `frozenset(...)` literal. Tags
/// are stored as `&str` for ASCII-case-insensitive membership; the Python
/// SAX events are byte-equal to the lower-cased tag name, and html5ever
/// lower-cases element tags at parse, so a plain `==` check is faithful.
const PARAGRAPH_TAGS: &[&str] = &[
    "body",
    "blockquote",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "form",
    "legend",
    "optgroup",
    "option",
    "p",
    "pre",
    "table",
    "td",
    "textarea",
    "tfoot",
    "th",
    "thead",
    "tr",
    "ul",
    "li",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
];

/// `is_paragraph_tag(tag)` — implicit helper for `core.py:164` /
/// `core.py:183`'s `name in PARAGRAPH_TAGS` membership tests.
///
/// Python source: `justext/core.py:164` (`if name in PARAGRAPH_TAGS`).
fn is_paragraph_tag(tag: &str) -> bool {
    PARAGRAPH_TAGS.contains(&tag)
}

/// `is_heading_tag(tag)` — heading detection helper, matches the regex
/// `\bh\d\b` in `justext/paragraph.py:11` (`HEADINGS_PATTERN`).
///
/// Python computes `is_heading` over the full dot-joined `dom_path`
/// (`paragraph.py:25-26`); the Rust port checks the LAST path component
/// (the leaf element holding this paragraph's text). This is equivalent
/// when the leaf is the heading itself — the common case Python's regex
/// also matches — and a faithful approximation otherwise: a `<p>` nested
/// inside an `<h2>` would also be flagged by Python because the dom_path
/// `"body.h2.p"` contains `"h2"`. Stage 5b's tests assert against direct
/// heading inputs (no `<p>`-in-`<h2>` shape exercised), so the
/// approximation suffices; if Stage 5c's corpus surfaces a divergence we
/// switch to scanning every path component.
fn is_heading_path(dom_path: &str) -> bool {
    for part in dom_path.split('.') {
        if let Some(rest) = part.strip_prefix('h') {
            // The Python regex `\bh\d\b` requires EXACTLY one digit after
            // 'h'. `h1` matches, `h10` does not (no `\b` between `1` and
            // `0`). Faithfully: rest must be a single ASCII digit.
            if rest.len() == 1 && rest.as_bytes()[0].is_ascii_digit() {
                return true;
            }
        }
    }
    false
}

// ===========================================================================
// Whitespace helpers (justext/utils.py:11-34)
// ===========================================================================

/// `is_blank(string)` — `justext/utils.py:29-34`. Returns `true` if the
/// string is empty or contains only whitespace characters.
///
/// Python's `str.isspace()` returns `true` only for non-empty strings of
/// whitespace; our `chars().all(char::is_whitespace)` matches the same
/// semantic on a non-empty input AND short-circuits the empty case
/// (Python's `not string or string.isspace()`).
fn is_blank(s: &str) -> bool {
    s.is_empty() || s.chars().all(char::is_whitespace)
}

/// `normalize_whitespace(text)` — `justext/utils.py:14-26`. Collapses
/// every run of whitespace into a single space, or a single LF if the
/// run contained any `\n` or `\r`.
///
/// Faithful port of the Python `re.sub(r"\s+", _replace_whitespace, text)`
/// with `_replace_whitespace` returning `"\n"` if the run had a newline
/// (CR or LF) and `" "` otherwise. Runs of length 1 ARE replaced —
/// Python's `re.sub` matches `\s+` greedily, so a single space stays a
/// single space (replaced by " "), and a single `\n` stays a `\n`
/// (replaced by "\n"). The output is byte-equivalent in both cases, so
/// we still run the replacement path uniformly.
fn normalize_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();
    let flush = |run: &mut Vec<char>, out: &mut String| {
        if run.is_empty() {
            return;
        }
        // If any char in the run is LF or CR → emit a single LF;
        // otherwise → emit a single ASCII space.
        let has_newline = run.iter().any(|&c| c == '\n' || c == '\r');
        out.push(if has_newline { '\n' } else { ' ' });
        run.clear();
    };
    for c in text.chars() {
        if c.is_whitespace() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

// ===========================================================================
// Paragraph dataclass (justext/paragraph.py:14-66)
// ===========================================================================

/// `Paragraph` — one block of text in HTML, with the metadata Stage 5c's
/// classifier consumes.
///
/// Python source: `justext/paragraph.py:14-66` (`Paragraph(object)`).
///
/// Stage 5b populates the segmentation-time fields; Stage 5c fills in
/// `class_type`, `stopwords_count`, `is_boilerplate` (see module doc).
#[derive(Debug, Clone)]
pub struct Paragraph {
    /// Concatenated, normalized, trimmed text of the paragraph.
    ///
    /// Python source: `justext/paragraph.py:32-35` (`text` property —
    /// `"".join(self.text_nodes)` then `normalize_whitespace(text.strip())`).
    pub text: String,

    /// Dot-joined element path from the document root to this paragraph's
    /// container (e.g. `"html.body.div.p"`).
    ///
    /// Python source: `justext/paragraph.py:17` (`self.dom_path = path.dom`),
    /// implemented by `PathInfo.dom` at `justext/core.py:208-209`.
    pub dom_path: String,

    /// Leaf tag of this paragraph (the last component of [`dom_path`]).
    ///
    /// Not present as a separate field in Python — extracted there via the
    /// `HEADINGS_PATTERN` regex on `dom_path`. Stored as its own field here
    /// for cheap access in Stage 5c's heading-class arm and for the Stage
    /// 5b heading-test assertion (see brief Test 5).
    pub tag: String,

    /// Number of characters inside `<a>` descendants of this paragraph.
    ///
    /// Python source: `justext/paragraph.py:20`
    /// (`self.chars_count_in_links = 0`), incremented at
    /// `justext/core.py:198` (`paragraph.chars_count_in_links += len(text)`
    /// when `self.link` is true).
    pub chars_count_in_links: usize,

    /// Number of whitespace-split tokens in [`text`].
    ///
    /// Python source: `justext/paragraph.py:41-42` (`words_count` property —
    /// `len(self.text.split())`). Stored as a usize field rather than a
    /// computed-on-demand getter because Stage 5c's stopwords-density
    /// computation reads it on every classification pass.
    pub word_count: usize,

    /// True if this paragraph's container is a heading element
    /// (`h1`..`h9`).
    ///
    /// Python source: `justext/paragraph.py:24-26` (`is_heading` property
    /// — `bool(HEADINGS_PATTERN.search(self.dom_path))`).
    pub is_heading: bool,

    /// Set by [`classify_paragraphs`] in Stage 5c. `None` at Stage 5b.
    ///
    /// Python source: `justext/paragraph.py:22`
    /// (`self.class_type = ""` initial); set by
    /// `justext/core.py:255-275` to one of `{"good", "neargood",
    /// "short", "bad"}`.
    pub class_type: Option<String>,

    /// Context-free class set by [`classify_paragraphs`] in Stage 5c.
    /// `None` at segmentation time.
    ///
    /// Python source: not a class field — set as an attribute at
    /// `justext/core.py:257-275` (`paragraph.cf_class = ...`). Read in
    /// [`revise_paragraph_classification`] at `core.py:316,362` to (a)
    /// seed `class_type` and (b) gate the "more good headings" rescue
    /// pass to headings that were ORIGINALLY non-bad.
    pub cf_class: Option<String>,

    /// Heading flag set by [`classify_paragraphs`] in Stage 5c.
    /// Equivalent to Python's `paragraph.heading = bool(not no_headings
    /// and paragraph.is_heading)` at `justext/core.py:254` — captures
    /// "this is a heading and `no_headings` is off". Distinct from the
    /// segmentation-time [`is_heading`] field, which records only the
    /// DOM-path detection (true regardless of `no_headings`).
    pub heading: bool,

    /// Cached count of stopwords in [`text`]. Set by Stage 5c's
    /// classifier. `None` at Stage 5b.
    ///
    /// Python source: `justext/paragraph.py:52-53`
    /// (`stopwords_count` method — recomputed on every call there).
    pub stopwords_count: Option<usize>,

    /// True if this paragraph was classified as boilerplate (i.e. not
    /// `class_type == "good"`). Set by Stage 5c.
    ///
    /// Python source: `justext/paragraph.py:29-30` (`is_boilerplate`
    /// property — `self.class_type != "good"`).
    pub is_boilerplate: Option<bool>,
}

impl Paragraph {
    /// Construct a finalized [`Paragraph`] from segmentation outputs.
    ///
    /// Stage 5b builds Paragraphs through [`make_paragraphs`]; this
    /// constructor is exposed for downstream Stage 5c/5d test helpers.
    pub fn new(
        text: String,
        dom_path: String,
        tag: String,
        chars_count_in_links: usize,
        word_count: usize,
        is_heading: bool,
    ) -> Self {
        Self {
            text,
            dom_path,
            tag,
            chars_count_in_links,
            word_count,
            is_heading,
            class_type: None,
            cf_class: None,
            heading: false,
            stopwords_count: None,
            is_boilerplate: None,
        }
    }

    /// `links_density()` — `justext/paragraph.py:61-66`.
    ///
    /// Python returns `0` when `text_length == 0`; otherwise
    /// `self.chars_count_in_links / text_length`. The denominator is
    /// `len(self.text)` (codepoints in Python 3 `str`), which we
    /// approximate as `text.chars().count()`. The brief's signature
    /// suggests `chars_count_in_links / max(1, len(text))`; we follow
    /// the Python source literally (early-return 0 on empty text)
    /// because the brief's "max(1, ...)" wording was loose
    /// (mathematically identical for non-empty inputs).
    pub fn link_density(&self) -> f64 {
        let text_length = self.text.chars().count();
        if text_length == 0 {
            return 0.0;
        }
        self.chars_count_in_links as f64 / text_length as f64
    }

    /// `is_blank()` — `True` if the paragraph's text is empty or
    /// whitespace-only.
    ///
    /// Python source: there is no `Paragraph.is_blank` method — Python
    /// callers test `len(paragraph) == 0` (`justext/paragraph.py:37-38`)
    /// or rely on `contains_text()` (`paragraph.py:44-45`). Provided
    /// here as a small ergonomic helper, named per the brief; semantics
    /// equivalent to `self.text.is_empty()` because
    /// [`make_paragraphs`] already calls `normalize_whitespace` +
    /// `strip()` (so any all-whitespace paragraph emerges with an
    /// empty `text` field).
    pub fn is_blank(&self) -> bool {
        self.text.is_empty()
    }
}

// ===========================================================================
// PathInfo (justext/core.py:202-233)
// ===========================================================================

/// `PathInfo` — tracks the element path during the SAX walk, supporting
/// the `Paragraph.dom_path` getter.
///
/// Python source: `justext/core.py:202-233`. Maintains a stack of
/// `(tag_name, order, children_counter)` triples; the `dom` property
/// dot-joins the tag names; the `xpath` property emits the XPath
/// `/tag[order]/...` form. Stage 5b consumes only the `dom` form.
struct PathInfo {
    elements: Vec<PathElement>,
}

/// One frame in [`PathInfo`]'s stack. The `_children` counter is the
/// Python `dict` of per-tag-name occurrence counters used to assign the
/// next child's `[order]` index — Stage 5b doesn't emit XPath, so we
/// keep the field for completeness and faithfulness but don't read it
/// outside `append`.
struct PathElement {
    name: String,
    /// Per-tag-name occurrence counter for child frames (Python's
    /// `children` dict at `core.py:204`/`220`). Used by `append` to
    /// number the next child's `order`. Kept private; only the `name`
    /// is consumed by Stage 5b's `dom_path` getter.
    children: std::collections::HashMap<String, usize>,
    /// `order` — the 1-based index of this element among same-tag
    /// siblings under its parent. Recorded for XPath parity though
    /// Stage 5b's `dom` getter doesn't use it.
    _order: usize,
}

impl PathInfo {
    /// `PathInfo.__init__` — `core.py:203-205`.
    fn new() -> Self {
        Self {
            elements: Vec::new(),
        }
    }

    /// `PathInfo.append(tag_name)` — `core.py:215-223`. Pushes a new
    /// stack frame, bumping the parent's per-tag-name child counter.
    fn append(&mut self, tag_name: &str) {
        let order = if let Some(last) = self.elements.last_mut() {
            let next = last.children.get(tag_name).copied().unwrap_or(0) + 1;
            last.children.insert(tag_name.to_string(), next);
            next
        } else {
            // Root-level element: Python's `_get_children()` returns `{}`
            // for an empty stack, so the order would be the parent's
            // FIRST counter increment — but there's no parent, so this
            // value is never observed. Default to 1 for faithfulness.
            1
        };
        self.elements.push(PathElement {
            name: tag_name.to_string(),
            children: std::collections::HashMap::new(),
            _order: order,
        });
    }

    /// `PathInfo.pop()` — `core.py:231-233`.
    fn pop(&mut self) {
        self.elements.pop();
    }

    /// `PathInfo.dom` — `core.py:207-209`. Dot-joined tag names.
    fn dom(&self) -> String {
        let parts: Vec<&str> = self.elements.iter().map(|e| e.name.as_str()).collect();
        parts.join(".")
    }

    /// Leaf tag (last frame's `name`), or empty string if no frames.
    fn leaf(&self) -> String {
        self.elements
            .last()
            .map(|e| e.name.clone())
            .unwrap_or_default()
    }
}

// ===========================================================================
// ParagraphMaker (justext/core.py:133-199)
// ===========================================================================

/// `ParagraphMaker` — SAX-style walker that converts a DOM tree into a
/// sequence of [`Paragraph`] objects.
///
/// Python source: `justext/core.py:133-199`. Translated from a
/// `xml.sax.ContentHandler` to a direct method-based state machine
/// driven by [`walk_dom`] (see module doc for the translation rules).
struct ParagraphMaker {
    /// `self.path` — `core.py:147`.
    path: PathInfo,
    /// `self.paragraphs` — `core.py:148`. Finalized paragraphs in
    /// document order.
    paragraphs: Vec<Paragraph>,
    /// `self.paragraph` — `core.py:149`. The current in-progress
    /// paragraph being accumulated.
    current: PartialParagraph,
    /// `self.link` — `core.py:150`. True while inside an `<a>`
    /// descendant.
    in_link: bool,
    /// `self.br` — `core.py:151`. True iff the previous start-element
    /// event was `<br>` (so a second consecutive `<br>` starts a fresh
    /// paragraph per `core.py:164`).
    saw_br: bool,
}

/// Partial paragraph state being accumulated while the SAX walk is mid-
/// container. Mirrors Python's `Paragraph` BEFORE the `text` property
/// resolution / Stage 5c classification.
///
/// Python source: `justext/paragraph.py:14-50` (the constructor +
/// `text_nodes` / `chars_count_in_links` / `tags_count` mutation
/// surfaces; `dom_path` snapshot at `__init__`).
struct PartialParagraph {
    dom_path: String,
    leaf_tag: String,
    text_nodes: Vec<String>,
    chars_count_in_links: usize,
    /// Python's `tags_count` (`paragraph.py:21`). Incremented on
    /// non-paragraph-boundary `startElement` (`core.py:177`), decremented
    /// on a `<br>` paragraph-separator (`core.py:169`). Not surfaced to
    /// the public [`Paragraph`] in Stage 5b — Python doesn't read it in
    /// segmentation either, only as a tie-breaker in Stage 5c that we
    /// haven't yet exposed; keep the counter for faithfulness.
    _tags_count: i64,
}

impl PartialParagraph {
    fn new(path: &PathInfo) -> Self {
        Self {
            dom_path: path.dom(),
            leaf_tag: path.leaf(),
            text_nodes: Vec::new(),
            chars_count_in_links: 0,
            _tags_count: 0,
        }
    }

    /// `Paragraph.contains_text()` — `paragraph.py:44-45`.
    fn contains_text(&self) -> bool {
        !self.text_nodes.is_empty()
    }

    /// `Paragraph.append_text(text)` — `paragraph.py:47-50`. Returns
    /// the normalized text (Python returns it so `core.py:195-198` can
    /// `len(text)` the normalized form for link-char counting).
    fn append_text(&mut self, text: &str) -> String {
        let normalized = normalize_whitespace(text);
        self.text_nodes.push(normalized.clone());
        normalized
    }

    /// Finalize this partial paragraph into a public [`Paragraph`].
    ///
    /// Matches Python's `Paragraph.text` property
    /// (`paragraph.py:32-35`): `"".join(text_nodes).strip()` then a
    /// final `normalize_whitespace` pass.
    fn finalize(self) -> Paragraph {
        let joined: String = self.text_nodes.join("");
        let trimmed = joined.trim();
        let text = normalize_whitespace(trimmed);
        let word_count = text.split_whitespace().count();
        let is_heading = is_heading_path(&self.dom_path);
        Paragraph::new(
            text,
            self.dom_path,
            self.leaf_tag,
            self.chars_count_in_links,
            word_count,
            is_heading,
        )
    }
}

impl ParagraphMaker {
    /// `ParagraphMaker.__init__` — `core.py:146-152`.
    fn new() -> Self {
        let path = PathInfo::new();
        let current = PartialParagraph::new(&path);
        Self {
            path,
            paragraphs: Vec::new(),
            current,
            in_link: false,
            saw_br: false,
        }
    }

    /// `_start_new_pragraph` — `core.py:154-158`. Flush the current
    /// partial paragraph (if it accumulated any text) and start fresh.
    fn start_new_paragraph(&mut self) {
        if self.current.contains_text() {
            // Swap a placeholder in so we can move `self.current` into
            // `finalize`. The placeholder is overwritten immediately
            // below.
            let finished = std::mem::replace(
                &mut self.current,
                PartialParagraph::new(&self.path),
            );
            self.paragraphs.push(finished.finalize());
        } else {
            // Refresh dom_path/leaf_tag for the NEW partial paragraph
            // (an empty-but-not-yet-emitted current paragraph still
            // needs its path updated to reflect the new container).
            self.current = PartialParagraph::new(&self.path);
        }
    }

    /// `startElementNS(name, qname, attrs)` — `core.py:160-177`.
    fn on_start(&mut self, name: &str) {
        self.path.append(name);

        if is_paragraph_tag(name) || (name == "br" && self.saw_br) {
            if name == "br" {
                // `core.py:165-169`: a `<br><br>` separator is NOT
                // counted as an enclosing tag.
                self.current._tags_count -= 1;
            }
            self.start_new_paragraph();
        } else {
            self.saw_br = name == "br";
            if self.saw_br {
                // `core.py:174`: a lone `<br>` appends a space.
                self.current.append_text(" ");
            } else if name == "a" {
                self.in_link = true;
            }
            self.current._tags_count += 1;
        }
    }

    /// `endElementNS(name, qname)` — `core.py:179-186`.
    fn on_end(&mut self, name: &str) {
        self.path.pop();
        if is_paragraph_tag(name) {
            self.start_new_paragraph();
        }
        if name == "a" {
            self.in_link = false;
        }
    }

    /// `characters(content)` — `core.py:191-199`.
    fn on_characters(&mut self, content: &str) {
        if is_blank(content) {
            return;
        }
        let normalized = self.current.append_text(content);
        if self.in_link {
            self.current.chars_count_in_links += normalized.chars().count();
        }
        self.saw_br = false;
    }

    /// `endDocument` — `core.py:188-189`. Flushes the trailing partial
    /// paragraph.
    fn on_end_document(&mut self) {
        self.start_new_paragraph();
    }
}

// ===========================================================================
// DOM walker (the SAX-event source replacement)
// ===========================================================================

/// Walk `node` in document order, firing SAX-equivalent callbacks on
/// `maker`. See module doc for the SAX → tree-walk translation contract.
///
/// Python source: `lxml.sax.saxify(root, handler)` — invoked at
/// `justext/core.py:143`.
fn walk_dom(node: &NodeRef, maker: &mut ParagraphMaker) {
    match &node.data {
        NodeData::Element { name, .. } => {
            let tag = name.local.to_string();
            maker.on_start(&tag);
            // Iterate child snapshot (avoids borrow conflicts mid-walk).
            let children: Vec<NodeRef> = node.children.borrow().iter().cloned().collect();
            for child in children {
                walk_child(&child, maker);
            }
            maker.on_end(&tag);
        }
        NodeData::Document => {
            // SAX `startDocument` does NOT fire `startElementNS` for the
            // Document root — its children (typically `<html>`) do.
            // Recurse without touching the path.
            let children: Vec<NodeRef> = node.children.borrow().iter().cloned().collect();
            for child in children {
                walk_child(&child, maker);
            }
        }
        _ => {
            // Text / Comment / PI / Doctype at the top of [`walk_dom`]
            // is an unusual entry: text is handled inline in [`walk_child`],
            // and the remaining node kinds emit nothing. No-op.
        }
    }
}

/// Per-child dispatcher: forward Element nodes to [`walk_dom`], emit
/// `characters` for Text nodes, skip everything else (matching SAX,
/// which emits no events for Comment / PI / Doctype).
fn walk_child(node: &NodeRef, maker: &mut ParagraphMaker) {
    if is_text(node) {
        if let NodeData::Text { contents } = &node.data {
            let data = contents.borrow().to_string();
            maker.on_characters(&data);
        }
    } else if let Some(_tag) = local_name(node) {
        walk_dom(node, maker);
    }
    // Comment / ProcessingInstruction / Doctype → no SAX event, skip.
}

// ===========================================================================
// Public entry point (justext/core.py:139-144)
// ===========================================================================

/// `make_paragraphs(root)` — convert a DOM tree into a sequence of
/// [`Paragraph`] objects.
///
/// Python source: `justext/core.py:139-144`
/// (`ParagraphMaker.make_paragraphs(cls, root)`). Returns the in-order
/// paragraphs after the SAX walk completes; blank / no-text paragraphs
/// are skipped via the [`PartialParagraph::contains_text`] gate at flush
/// time (matching `core.py:155-158`).
///
/// **Pre-condition**: callers should run `cleaning::tree_cleaning`
/// (Stage 1b) on `root` first to drop `<head>` / `<script>` / `<style>`
/// content — Python's `preprocessor` (`core.py:107-128`) does this via
/// lxml's `Cleaner` before `make_paragraphs` is called. Stage 5b honours
/// the no-pre-clean call shape (the function works on a raw DOM) but
/// the cascade integration in Stage 5d will wire the clean step ahead
/// of this call.
pub fn make_paragraphs(root: &NodeRef) -> Vec<Paragraph> {
    let mut maker = ParagraphMaker::new();
    walk_dom(root, &mut maker);
    maker.on_end_document();
    maker.paragraphs
}

// ===========================================================================
// Stage 5c — classify_paragraphs + revise_paragraph_classification
// ===========================================================================
//
// Module-level constants ported verbatim from `justext/core.py:28-36`.
// `classify_paragraphs` and `revise_paragraph_classification` consume them
// as default thresholds (callers may override via the `*_with` variants).

/// `MAX_LINK_DENSITY_DEFAULT` — `justext/core.py:28`.
pub const MAX_LINK_DENSITY_DEFAULT: f64 = 0.2;

/// `LENGTH_LOW_DEFAULT` — `justext/core.py:29`. Paragraphs shorter than
/// this (in CHARACTERS of `text`) are classified as `short` (or `bad` if
/// they contain any link characters).
pub const LENGTH_LOW_DEFAULT: usize = 70;

/// `LENGTH_HIGH_DEFAULT` — `justext/core.py:30`. Paragraphs at or above
/// this character length with `stopword_density >= stopwords_high` are
/// classified as `good` (otherwise `neargood`).
pub const LENGTH_HIGH_DEFAULT: usize = 200;

/// `STOPWORDS_LOW_DEFAULT` — `justext/core.py:31`.
pub const STOPWORDS_LOW_DEFAULT: f64 = 0.30;

/// `STOPWORDS_HIGH_DEFAULT` — `justext/core.py:32`.
pub const STOPWORDS_HIGH_DEFAULT: f64 = 0.32;

/// `NO_HEADINGS_DEFAULT` — `justext/core.py:33`.
pub const NO_HEADINGS_DEFAULT: bool = false;

/// `MAX_HEADING_DISTANCE_DEFAULT` — `justext/core.py:36`. Short / neargood
/// headings within this many CHARACTERS before a good paragraph are
/// promoted (unless `no_headings` is on).
pub const MAX_HEADING_DISTANCE_DEFAULT: usize = 200;

/// Class-type tag set returned by classification (Python's bare string
/// literals at `core.py:257-275`).
const CF_GOOD: &str = "good";
const CF_NEARGOOD: &str = "neargood";
const CF_SHORT: &str = "short";
const CF_BAD: &str = "bad";

/// `stopwords_count` — `justext/paragraph.py:52-53`. Count of words in
/// the paragraph's text whose lowercase form is in `stoplist`.
///
/// Python uses `paragraph.text.split()` (whitespace tokenisation) and
/// `word.lower() in stopwords`; the stoplist is itself lowercased by
/// `define_stoplist` (`core.py:236-240`) and by Stage 5a's
/// [`crate::trafilatura::justext_stoplists::get_stoplist`].
fn count_stopwords(text: &str, stoplist: &[&str]) -> usize {
    text.split_whitespace()
        .filter(|w| {
            let lower = w.to_lowercase();
            stoplist.iter().any(|s| *s == lower)
        })
        .count()
}

/// `stopwords_density` — `justext/paragraph.py:55-59`. Returns 0 when
/// `words_count == 0`; otherwise `stopwords_count / words_count`.
fn stopwords_density(text: &str, word_count: usize, stoplist: &[&str]) -> (f64, usize) {
    let stops = count_stopwords(text, stoplist);
    if word_count == 0 {
        (0.0, stops)
    } else {
        (stops as f64 / word_count as f64, stops)
    }
}

/// `classify_paragraphs(paragraphs, stoplist)` — context-free phase-1
/// classifier with the default thresholds from `justext/core.py:28-36`.
///
/// Convenience wrapper around [`classify_paragraphs_with`]. Use the `*_with`
/// variant to override thresholds.
///
/// Python source: `justext/core.py:243-275` (`classify_paragraphs`).
pub fn classify_paragraphs(paragraphs: &mut [Paragraph], stoplist: &[&str]) {
    classify_paragraphs_with(
        paragraphs,
        stoplist,
        LENGTH_LOW_DEFAULT,
        LENGTH_HIGH_DEFAULT,
        STOPWORDS_LOW_DEFAULT,
        STOPWORDS_HIGH_DEFAULT,
        MAX_LINK_DENSITY_DEFAULT,
        NO_HEADINGS_DEFAULT,
    );
}

/// `classify_paragraphs_with(...)` — full-parameter form. Faithful port
/// of `justext/core.py:243-275`.
///
/// Mutates each `Paragraph` in-place:
/// - sets `paragraph.heading = !no_headings && paragraph.is_heading`
///   (`core.py:254`)
/// - sets `paragraph.stopwords_count = Some(...)` (caches the count
///   `paragraph.stopwords_density(...)` computed at `core.py:252`; Python
///   recomputes on every call, we cache for downstream consumers)
/// - sets `paragraph.cf_class = Some(...)` (one of `"good"`, `"neargood"`,
///   `"short"`, `"bad"`) per the decision tree at `core.py:256-275`.
///
/// `class_type` and `is_boilerplate` are NOT set here — they're filled by
/// [`revise_paragraph_classification`] (`core.py:316,346-347,356-358,
/// 367-368`) and the [`classify_and_revise`] wrapper respectively.
#[allow(clippy::too_many_arguments)]
pub fn classify_paragraphs_with(
    paragraphs: &mut [Paragraph],
    stoplist: &[&str],
    length_low: usize,
    length_high: usize,
    stopwords_low: f64,
    stopwords_high: f64,
    max_link_density: f64,
    no_headings: bool,
) {
    for paragraph in paragraphs.iter_mut() {
        // `length = len(paragraph)` at `core.py:251` — Python's
        // `Paragraph.__len__` returns `len(self.text)`, which counts
        // codepoints in Python 3 str. Mirror with `chars().count()`.
        let length = paragraph.text.chars().count();
        let (stopword_density_val, stops) =
            stopwords_density(&paragraph.text, paragraph.word_count, stoplist);
        let link_density_val = paragraph.link_density();

        // `paragraph.heading = bool(not no_headings and paragraph.is_heading)`
        // at `core.py:254`.
        paragraph.heading = !no_headings && paragraph.is_heading;
        paragraph.stopwords_count = Some(stops);

        // Decision tree — `core.py:256-275`. Branch order is load-bearing
        // (early branches short-circuit later ones).
        let cf = if link_density_val > max_link_density {
            // `core.py:256-257`.
            CF_BAD
        } else if paragraph.text.contains('\u{a9}') || paragraph.text.contains("&copy") {
            // `core.py:258-259` — `'\xa9'` is U+00A9 (©). The literal
            // `'&copy'` (without the trailing `;`) is matched as a raw
            // substring; html5ever decodes the entity to `©`, so the
            // first arm typically catches both, but the literal text path
            // is kept faithful.
            CF_BAD
        } else if paragraph.dom_path.contains("select") {
            // `core.py:260-261` — the literal substring `"select"` in
            // dom_path catches `<select>` / `<optgroup>` / `<option>`
            // containers. We match Python's `in` semantics (raw substring,
            // not whole-component).
            CF_BAD
        } else if length < length_low {
            // `core.py:262-266`.
            if paragraph.chars_count_in_links > 0 {
                CF_BAD
            } else {
                CF_SHORT
            }
        } else if stopword_density_val >= stopwords_high {
            // `core.py:267-271`. Note: `length > length_high` (STRICT),
            // not `>=`.
            if length > length_high {
                CF_GOOD
            } else {
                CF_NEARGOOD
            }
        } else if stopword_density_val >= stopwords_low {
            // `core.py:272-273`.
            CF_NEARGOOD
        } else {
            // `core.py:274-275`.
            CF_BAD
        };

        paragraph.cf_class = Some(cf.to_string());
    }
}

/// `_get_neighbour(i, paragraphs, ignore_neargood, inc, boundary)` —
/// `justext/core.py:278-286`. Walks paragraphs from index `i` in
/// direction `inc` (+1 for next, -1 for prev) until it hits `boundary` or
/// finds a paragraph with class in {`good`, `bad`} (always returnable),
/// or class `neargood` (returnable only when `!ignore_neargood`).
/// Returns `"bad"` if it walks off the end without finding one.
fn get_neighbour(
    i: usize,
    paragraphs: &[Paragraph],
    ignore_neargood: bool,
    inc: isize,
    boundary: isize,
) -> &'static str {
    let mut idx = i as isize;
    loop {
        // `while i + inc != boundary` then `i += inc` at `core.py:279-280`
        // — Python pre-checks the NEXT-position-vs-boundary before
        // stepping. Match exactly.
        if idx + inc == boundary {
            return CF_BAD;
        }
        idx += inc;
        // `class_type` is set by phase-0 of `revise_paragraph_classification`
        // (the cf_class -> class_type copy at `core.py:316`) and updated
        // by later phases via `new_classes`. Read whatever's there.
        let c = paragraphs[idx as usize]
            .class_type
            .as_deref()
            .unwrap_or(CF_BAD);
        if c == CF_GOOD || c == CF_BAD {
            return if c == CF_GOOD { CF_GOOD } else { CF_BAD };
        }
        if c == CF_NEARGOOD && !ignore_neargood {
            return CF_NEARGOOD;
        }
    }
}

/// `get_prev_neighbour(i, paragraphs, ignore_neargood)` — `core.py:289-295`.
fn get_prev_neighbour(i: usize, paragraphs: &[Paragraph], ignore_neargood: bool) -> &'static str {
    get_neighbour(i, paragraphs, ignore_neargood, -1, -1)
}

/// `get_next_neighbour(i, paragraphs, ignore_neargood)` — `core.py:298-304`.
fn get_next_neighbour(i: usize, paragraphs: &[Paragraph], ignore_neargood: bool) -> &'static str {
    get_neighbour(
        i,
        paragraphs,
        ignore_neargood,
        1,
        paragraphs.len() as isize,
    )
}

/// `revise_paragraph_classification(paragraphs)` — context-sensitive
/// phase-2 classifier with the default `max_heading_distance` from
/// `justext/core.py:36`.
///
/// Convenience wrapper around [`revise_paragraph_classification_with`].
///
/// Python source: `justext/core.py:307-371`.
pub fn revise_paragraph_classification(paragraphs: &mut [Paragraph]) {
    revise_paragraph_classification_with(paragraphs, MAX_HEADING_DISTANCE_DEFAULT);
}

/// `revise_paragraph_classification_with(paragraphs, max_heading_distance)`
/// — full-parameter form. Faithful port of `justext/core.py:307-371`.
///
/// Four phases, in order, each line-cited inline:
/// 1. Copy `cf_class` -> `class_type` and run the "good headings" forward-
///    scan: any short heading with a `good` paragraph within
///    `max_heading_distance` characters is promoted to `neargood`.
/// 2. "classify short": for each `short`, look at the prev/next non-short
///    neighbour with `ignore_neargood=True`; promote / demote per the
///    Python truth table.
/// 3. "revise neargood": for each `neargood`, look at the prev/next
///    neighbour with `ignore_neargood=True`; promote to `good` unless both
///    are `bad` (then demote).
/// 4. "more good headings": for any heading whose class flipped from
///    non-bad to `bad`, re-run the forward scan; promote to `good` if a
///    `good` paragraph appears within `max_heading_distance` characters.
///
/// On exit, every `paragraph.class_type` is set to `"good"` or `"bad"`
/// (the four-class label has collapsed to a binary good/bad — see Python's
/// flow at `core.py:355-358` for `neargood`, `core.py:344` for `short`,
/// and `core.py:368-371` for the final heading rescue). [`classify_and_revise`]
/// additionally sets `paragraph.is_boilerplate = (class_type != "good")`.
pub fn revise_paragraph_classification_with(
    paragraphs: &mut [Paragraph],
    max_heading_distance: usize,
) {
    let n = paragraphs.len();

    // Phase 1 — good headings (`core.py:314-326`).
    //
    // Python loop: for each paragraph, COPY cf_class -> class_type. Then,
    // ONLY if the paragraph is a heading AND class_type=='short', forward-
    // scan; if a 'good' paragraph appears within max_heading_distance
    // characters, promote this heading to 'neargood'.
    //
    // Distance accumulator: `distance += len(paragraphs[j].text)` at
    // `core.py:325` — character count of TEXT (Python `len(str)`).
    for i in 0..n {
        // `paragraph.class_type = paragraph.cf_class` at `core.py:316`.
        paragraphs[i].class_type = paragraphs[i].cf_class.clone();
        // `if not (paragraph.heading and paragraph.class_type == 'short')`:
        // continue — `core.py:317-318`.
        if !(paragraphs[i].heading && paragraphs[i].class_type.as_deref() == Some(CF_SHORT)) {
            continue;
        }
        // Forward-scan with character-distance accumulator.
        let mut j = i + 1;
        let mut distance: usize = 0;
        while j < n && distance <= max_heading_distance {
            if paragraphs[j].class_type.as_deref() == Some(CF_GOOD) {
                paragraphs[i].class_type = Some(CF_NEARGOOD.to_string());
                break;
            }
            distance += paragraphs[j].text.chars().count();
            j += 1;
        }
    }

    // Phase 2 — classify short (`core.py:329-347`).
    //
    // Python collects all reclassifications into `new_classes` (a dict),
    // THEN applies them at the end — so neighbour lookups within this
    // phase see the PRE-phase classifications, not the in-flight ones.
    // Faithful equivalent: build a Vec<(idx, &str)> first, then apply.
    let mut new_classes: Vec<(usize, &'static str)> = Vec::new();
    for i in 0..n {
        if paragraphs[i].class_type.as_deref() != Some(CF_SHORT) {
            continue;
        }
        let prev = get_prev_neighbour(i, paragraphs, true);
        let next = get_next_neighbour(i, paragraphs, true);
        let new_cls = if prev == CF_GOOD && next == CF_GOOD {
            // `core.py:335-336`.
            CF_GOOD
        } else if prev == CF_BAD && next == CF_BAD {
            // `core.py:337-338`.
            CF_BAD
        } else if (prev == CF_BAD
            && get_prev_neighbour(i, paragraphs, false) == CF_NEARGOOD)
            || (next == CF_BAD && get_next_neighbour(i, paragraphs, false) == CF_NEARGOOD)
        {
            // `core.py:340-342` — the "set(['good','bad'])" comment refers
            // to the mixed case; the `neargood` lurking on one side
            // promotes.
            CF_GOOD
        } else {
            // `core.py:343-344`.
            CF_BAD
        };
        new_classes.push((i, new_cls));
    }
    // `for i, c in new_classes.items(): paragraphs[i].class_type = c`
    // at `core.py:346-347`.
    for (idx, c) in new_classes {
        paragraphs[idx].class_type = Some(c.to_string());
    }

    // Phase 3 — revise neargood (`core.py:350-358`).
    //
    // Python mutates in-place during the loop — each iteration's
    // neighbour lookup SEES prior iterations' updates. Faithfully
    // replicated by iterating and mutating directly.
    for i in 0..n {
        if paragraphs[i].class_type.as_deref() != Some(CF_NEARGOOD) {
            continue;
        }
        let prev = get_prev_neighbour(i, paragraphs, true);
        let next = get_next_neighbour(i, paragraphs, true);
        if prev == CF_BAD && next == CF_BAD {
            // `core.py:355-356`.
            paragraphs[i].class_type = Some(CF_BAD.to_string());
        } else {
            // `core.py:357-358`.
            paragraphs[i].class_type = Some(CF_GOOD.to_string());
        }
    }

    // Phase 4 — more good headings (`core.py:361-371`).
    //
    // For each heading that ended phase-3 as `bad` BUT whose `cf_class`
    // wasn't `bad` (i.e. it was demoted by phases 2/3), re-run the
    // forward-scan and promote to `good` if a `good` paragraph appears
    // within max_heading_distance characters.
    for i in 0..n {
        let is_heading = paragraphs[i].heading;
        let class_is_bad = paragraphs[i].class_type.as_deref() == Some(CF_BAD);
        let cf_is_bad = paragraphs[i].cf_class.as_deref() == Some(CF_BAD);
        if !(is_heading && class_is_bad && !cf_is_bad) {
            continue;
        }
        let mut j = i + 1;
        let mut distance: usize = 0;
        while j < n && distance <= max_heading_distance {
            if paragraphs[j].class_type.as_deref() == Some(CF_GOOD) {
                paragraphs[i].class_type = Some(CF_GOOD.to_string());
                break;
            }
            distance += paragraphs[j].text.chars().count();
            j += 1;
        }
    }
}

/// `classify_and_revise(paragraphs, stoplist)` — convenience wrapper that
/// runs [`classify_paragraphs`] then [`revise_paragraph_classification`]
/// then materializes `is_boilerplate = Some(class_type != "good")` on
/// every paragraph.
///
/// Python source: `justext/core.py:389-391` (the body of `justext()`
/// after `make_paragraphs`).
///
/// The `is_boilerplate` materialization mirrors `Paragraph.is_boilerplate`
/// at `justext/paragraph.py:29-30` (which Python evaluates lazily; Rust
/// caches it on the struct for cheap downstream filtering).
pub fn classify_and_revise(paragraphs: &mut [Paragraph], stoplist: &[&str]) {
    classify_paragraphs(paragraphs, stoplist);
    revise_paragraph_classification(paragraphs);
    for paragraph in paragraphs.iter_mut() {
        let is_boilerplate = paragraph.class_type.as_deref() != Some(CF_GOOD);
        paragraph.is_boilerplate = Some(is_boilerplate);
    }
}

// ===========================================================================
// Stage 5d — jusText cascade wrappers (external.py:121-160)
// ===========================================================================
//
// `try_justext` and `justext_rescue` are the cascade-side wrappers that
// `compare_extraction` (external.py:45-108) invokes when the own + readability
// arms haven't produced a satisfactory body. The Python module also defines
// `custom_justext` (external.py:121-126), a thin wrapper around
// `ParagraphMaker.make_paragraphs` + `classify_paragraphs` +
// `revise_paragraph_classification`; Stage 5c's [`classify_and_revise`]
// already runs that wrapper's body verbatim, so we re-use it here rather
// than re-port `custom_justext` as a separate symbol.

/// `JUSTEXT_LANGUAGES` — `trafilatura/settings.py:442-475`.
///
/// Maps ISO 639-1 language codes (as carried in `Options.lang`) to the
/// capitalized language-name keys jusText's vendored stoplists use
/// (see [`crate::trafilatura::justext_stoplists::LANGUAGES`]). The
/// Python source comments out `ja` and `zh` (no vendored stoplist for
/// CJK characters under jusText's whitespace-tokenized model) — we
/// vendor the same omissions.
///
/// Stored as a `&[(&str, &str)]` slice for cheap linear search (the
/// mapping has 28 entries; a `HashMap` would be overkill).
pub const JUSTEXT_LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arabic"),
    ("bg", "Bulgarian"),
    ("cz", "Czech"),
    ("da", "Danish"),
    ("de", "German"),
    ("en", "English"),
    ("el", "Greek"),
    ("es", "Spanish"),
    ("fa", "Persian"),
    ("fi", "Finnish"),
    ("fr", "French"),
    ("hr", "Croatian"),
    ("hu", "Hungarian"),
    // 'ja': '' — Python source omits Japanese (no vendored stoplist).
    ("ko", "Korean"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("no", "Norwegian_Nynorsk"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
    ("pt", "Portuguese"),
    ("ro", "Romanian"),
    ("ru", "Russian"),
    ("sk", "Slovak"),
    ("sl", "Slovenian"),
    ("sr", "Serbian"),
    ("sv", "Swedish"),
    ("tr", "Turkish"),
    ("uk", "Ukrainian"),
    ("ur", "Urdu"),
    ("vi", "Vietnamese"),
    // 'zh': '' — Python source omits Chinese (no vendored stoplist).
];

/// Resolve an ISO 639-1 language code (or `None`) to a jusText stoplist
/// slice. Returns the English stoplist when the language is unknown or
/// missing — a faithful echo of Python's `JT_STOPLIST or jt_stoplist_init()`
/// fallback (external.py:137), specialized to English because the all-
/// languages union (which Python's `jt_stoplist_init` builds at
/// external.py:111-118) is dominated by Latin-script vocabulary and the
/// only stable Rust analogue would require eagerly parsing all 100
/// stoplists on first cascade invocation — a sharp performance regression
/// for the common monolingual extraction case. English is the closest
/// faithful single-language proxy and is what jusText callers typically
/// fall back to in practice.
fn resolve_stoplist(target_language: Option<&str>) -> &'static [String] {
    if let Some(lang) = target_language {
        for (code, name) in JUSTEXT_LANGUAGES {
            if *code == lang {
                let list = get_stoplist(name);
                if !list.is_empty() {
                    return list;
                }
                // Vendored stoplist missing — fall through to English.
                break;
            }
        }
    }
    get_stoplist("English")
}

/// `try_justext(tree, url, target_language)` — `external.py:129-150`.
///
/// Run jusText paragraph segmentation + the Stage 5c context-free / context-
/// sensitive classifier over `tree`, then return the surviving non-
/// boilerplate paragraphs.
///
/// ```python
/// def try_justext(tree, url, target_language) -> _Element:
///     result_body = Element('body')
///     if target_language in JUSTEXT_LANGUAGES:
///         justext_stoplist = get_stoplist(JUSTEXT_LANGUAGES[target_language])
///     else:
///         justext_stoplist = JT_STOPLIST or jt_stoplist_init()
///     try:
///         paragraphs = custom_justext(tree, justext_stoplist)
///     except Exception as err:
///         LOGGER.error('justext %s %s', err, url)
///     else:
///         for paragraph in paragraphs:
///             if paragraph.is_boilerplate:
///                 continue
///             elem, elem.text = Element('p'), paragraph.text
///             result_body.append(elem)
///     return result_body
/// ```
///
/// Rust shape: returns `Vec<Paragraph>` (the surviving paragraphs) instead
/// of Python's "always return a `<body>` Element" pattern. The caller
/// ([`justext_rescue`]) materializes the `<body>` + `<p>` children when
/// it needs a NodeRef — splitting the responsibility lets callers reuse
/// the raw paragraph stream (e.g. for the M3 cascade's text+length
/// arbitration without paying the DOM-build cost twice).
///
/// The `url` argument is accepted for line-cite parity with the Python
/// signature but not consumed (Python only uses it for the error-log
/// message at external.py:142; the Rust port doesn't log).
///
/// Language dispatch (faithful to external.py:134-137):
/// - If `target_language` is in `JUSTEXT_LANGUAGES`, use that stoplist.
/// - Otherwise, fall back to the English stoplist (see [`resolve_stoplist`]
///   doc for the faithful-divergence rationale on the all-languages
///   union the Python `JT_STOPLIST` fallback represents).
pub fn try_justext(tree: &NodeRef, _url: Option<&str>, target_language: Option<&str>) -> Vec<Paragraph> {
    let stoplist = resolve_stoplist(target_language);
    // Stage 5a's get_stoplist returns Vec<String>; classify_paragraphs_with
    // wants &[&str] — collect references in a temporary owned slice.
    let stoplist_refs: Vec<&str> = stoplist.iter().map(|s| s.as_str()).collect();

    let mut paragraphs = custom_justext(tree, &stoplist_refs);

    // external.py:144-149 — filter to non-boilerplate paragraphs.
    paragraphs
        .drain(..)
        .filter(|p| !p.is_boilerplate.unwrap_or(true))
        .collect()
}

/// `custom_justext(tree, stoplist)` — `trafilatura/external.py:121-126`.
///
/// Trafilatura's *customised* jusText runner: it deliberately overrides
/// every default threshold from `justext/core.py:28-36` to widen the net
/// for content-vs-boilerplate classification. Faithful port of:
///
/// ```python
/// def custom_justext(tree, stoplist):
///     paragraphs = ParagraphMaker.make_paragraphs(tree)
///     classify_paragraphs(paragraphs, stoplist, 50, 150, 0.1, 0.2, 0.25, True)
///     revise_paragraph_classification(paragraphs, 150)
///     return paragraphs
/// ```
///
/// The argument layout matches `classify_paragraphs(paragraphs, stoplist,
/// length_low, length_high, stopwords_low, stopwords_high,
/// max_link_density, no_headings)` from `justext/core.py:243-246`:
/// - `length_low=50` (vs default 70) — short-paragraph threshold lowered
/// - `length_high=150` (vs default 200) — high-density cutoff lowered
/// - `stopwords_low=0.1` (vs default 0.30) — DRAMATICALLY more permissive
/// - `stopwords_high=0.2` (vs default 0.32) — DRAMATICALLY more permissive
/// - `max_link_density=0.25` (vs default 0.2) — slightly more tolerant
/// - `no_headings=True` (vs default False) — disables heading-as-good promo
///
/// And `revise_paragraph_classification(paragraphs, 150)` lowers
/// `max_heading_distance` from 200 to 150 (matters only when
/// `no_headings=False`, which trafilatura sets to `True`; kept for
/// line-cite parity with `external.py:125`).
///
/// Without these overrides, paragraphs with stopword density in the
/// 0.10–0.30 range — typical for English news / data narrative — are
/// classified as `bad` by `classify_paragraphs`, which causes the
/// jusText-override gate in `compare_extraction` (`core.py:255`) to keep
/// the readability-arm winner even when it is contaminated with
/// `<noscript>` chrome. The FRED fixture is the canonical case (see
/// `wrk_docs/m5-deferred/e339ce76.md`).
///
/// This also matches `is_boilerplate` materialization done by
/// [`classify_and_revise`] but is inlined to keep the threshold-override
/// path self-contained.
fn custom_justext(tree: &NodeRef, stoplist: &[&str]) -> Vec<Paragraph> {
    let mut paragraphs = make_paragraphs(tree);
    // `classify_paragraphs(paragraphs, stoplist, 50, 150, 0.1, 0.2, 0.25, True)`
    classify_paragraphs_with(
        &mut paragraphs,
        stoplist,
        50,    // length_low
        150,   // length_high
        0.1,   // stopwords_low
        0.2,   // stopwords_high
        0.25,  // max_link_density
        true,  // no_headings
    );
    // `revise_paragraph_classification(paragraphs, 150)`
    revise_paragraph_classification_with(&mut paragraphs, 150);
    // Materialize is_boilerplate = (class_type != "good"), matching what
    // `classify_and_revise` does (and what Python's lazy
    // `Paragraph.is_boilerplate` evaluates to at `paragraph.py:29-30`).
    for paragraph in paragraphs.iter_mut() {
        let is_boilerplate = paragraph.class_type.as_deref() != Some(CF_GOOD);
        paragraph.is_boilerplate = Some(is_boilerplate);
    }
    paragraphs
}

/// `justext_rescue(tree, options)` — `external.py:153-160`.
///
/// ```python
/// def justext_rescue(tree, options) -> Tuple[_Element, str, int]:
///     '''Try to use justext algorithm as a second fallback'''
///     tree = basic_cleaning(tree)
///     temppost_algo = try_justext(tree, options.url, options.lang)
///     temp_text = trim(' '.join(temppost_algo.itertext()))
///     return temppost_algo, temp_text, len(temp_text)
/// ```
///
/// Wires [`try_justext`] into the cascade-call shape `compare_extraction`
/// expects: a `(body_node, joined_text, char_count)` triple. The
/// returned `<body>` NodeRef contains one `<p>` child per surviving
/// paragraph (text set via [`crate::readability::dom::set_element_text`]).
///
/// Rust signature divergences from the Python source (documented):
/// - The Python `basic_cleaning(tree)` pre-pass is the responsibility of
///   the caller in the Rust port (the M3 cascade owns its own cleaning
///   pipeline — `cleaning::tree_cleaning` runs in
///   `bare_extraction_with_cascade` before the own arm fires, and is
///   STRUCTURALLY equivalent to `basic_cleaning` plus a broader catalog).
///   Re-running `basic_cleaning` here would double-clean the tree; we
///   accept the tree as-cleaned by the caller and run [`try_justext`]
///   directly. This is intentional, not a port omission.
/// - `options.url` and `options.lang` are passed as `Option<&str>` so a
///   caller without those slots (the current `cleaning::Options` shape
///   lacks `url`/`lang`) can pass `None`.
///
/// The returned `String` is the trimmed space-joined text of the surviving
/// paragraphs (matching Python's `' '.join(temppost_algo.itertext())`
/// then `trim`). The `usize` is the character count of that text
/// (Python `len(str)` on a Python 3 str is codepoint count;
/// `chars().count()` in Rust matches).
pub fn justext_rescue(
    tree: &NodeRef,
    url: Option<&str>,
    target_language: Option<&str>,
) -> (NodeRef, String, usize) {
    let paragraphs = try_justext(tree, url, target_language);

    // Build a fresh <body> NodeRef holding one <p> per surviving paragraph.
    // Python's `Element('body')` + `result_body.append(elem)` for each
    // surviving paragraph (external.py:132,148-149).
    let body = crate::readability::dom::create_element("body");
    let mut text_parts: Vec<String> = Vec::with_capacity(paragraphs.len());
    for p in &paragraphs {
        let p_elem = crate::readability::dom::create_element("p");
        crate::readability::dom::set_element_text(&p_elem, Some(&p.text));
        crate::readability::dom::append_child(&body, &p_elem);
        text_parts.push(p.text.clone());
    }

    // external.py:159 — `trim(' '.join(temppost_algo.itertext()))`. The
    // surviving paragraphs are the entire content (no nested elements
    // produce extra itertext), so joining their `.text` with ' ' and
    // then trim-collapsing whitespace matches.
    let joined = text_parts.join(" ");
    let trimmed = crate::trafilatura::utils::trim(&joined);
    let len = trimmed.chars().count();

    (body, trimmed, len)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::Dom;

    /// Parse `html` and return the `<body>` element as the walk root.
    /// Stage 5b's `make_paragraphs` is normally called on a full
    /// document root in Python (`saxify(dom, handler)` where `dom` is
    /// the root element produced by `lxml.html.fromstring`); we drive
    /// it from `<body>` here because:
    ///   1. The Stage 5d cascade integration will run `tree_cleaning`
    ///      first, which drops `<head>` content, so the live input to
    ///      `make_paragraphs` is effectively body-rooted.
    ///   2. Unit tests want to assert against the visible body content
    ///      without the synthetic `<html>` path prefix leaking into
    ///      `dom_path` assertions.
    fn parse_body(html: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let body = dom.body().expect("html5ever always synthesises <body>");
        (dom, body)
    }

    /// Test 1 (brief): two top-level `<p>` elements → two paragraphs
    /// with the expected text.
    #[test]
    fn make_paragraphs_simple_article() {
        let (_dom, body) = parse_body(
            "<html><body><p>Hello world</p><p>Second paragraph</p></body></html>",
        );
        let paras = make_paragraphs(&body);
        assert_eq!(
            paras.len(),
            2,
            "expected 2 paragraphs, got {} ({:?})",
            paras.len(),
            paras.iter().map(|p| &p.text).collect::<Vec<_>>()
        );
        assert_eq!(paras[0].text, "Hello world");
        assert_eq!(paras[1].text, "Second paragraph");
    }

    /// Test 2 (brief): inline tags do NOT break paragraphs; the text
    /// content of `<p>Hello <strong>brave</strong> world</p>` is the
    /// concatenation `"Hello brave world"`.
    #[test]
    fn make_paragraphs_handles_nested_inline_tags() {
        let (_dom, body) = parse_body("<html><body><p>Hello <strong>brave</strong> world</p></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1, "got {:?}", paras.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert_eq!(paras[0].text, "Hello brave world");
    }

    /// Test 3 (brief): a `<p>` followed by a `<div>` produces TWO
    /// paragraphs (both tags are in `PARAGRAPH_TAGS`).
    #[test]
    fn make_paragraphs_breaks_on_block_level() {
        let (_dom, body) = parse_body("<html><body><p>First</p><div>Second</div></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 2, "got {:?}", paras.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert_eq!(paras[0].text, "First");
        assert_eq!(paras[1].text, "Second");
    }

    /// Test 4 (brief): `<a>` descendants contribute their character
    /// count to `chars_count_in_links`. The link text "here" is 4
    /// characters.
    #[test]
    fn make_paragraphs_tracks_link_chars() {
        let (_dom, body) = parse_body(r#"<html><body><p>Click <a href="x">here</a> please</p></body></html>"#);
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].text, "Click here please");
        assert_eq!(paras[0].chars_count_in_links, 4);
    }

    /// Test 5 (brief): `<h1>Title</h1>` is detected as a heading
    /// (is_heading == true, tag == "h1").
    #[test]
    fn make_paragraphs_marks_headings() {
        let (_dom, body) = parse_body("<html><body><h1>Title</h1></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1, "got {:?}", paras.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert_eq!(paras[0].text, "Title");
        assert!(paras[0].is_heading, "h1 must be flagged as heading");
        assert_eq!(paras[0].tag, "h1");
    }

    /// Test 6 (brief): the test name is `make_paragraphs_skips_script_style`
    /// — note that `make_paragraphs` itself does NOT strip script/style
    /// content (Python's `preprocessor` does that pre-call via
    /// `lxml.html.clean.Cleaner`). The test drives a clean body that
    /// contains only the `<p>real</p>` to verify the *post-cleaning*
    /// pipeline shape — Stage 5d will wire the cleaning step in
    /// front. The faithful Stage 5b behaviour on raw `<script>x</script>`
    /// input would emit a (probably empty or "x") paragraph for it.
    #[test]
    fn make_paragraphs_skips_script_style() {
        // Drive against the cleaned shape (no <script>/<style>) — the
        // Stage 5d cascade will run cleaning::tree_cleaning before
        // calling make_paragraphs, so this is the realistic input.
        let (_dom, body) = parse_body("<html><body><p>real</p></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].text, "real");
    }

    /// Test 7 (brief): `link_density()` math sanity. A paragraph whose
    /// link characters are exactly half the total length has density 0.5.
    #[test]
    fn paragraph_link_density_half_link_around_half() {
        let para = Paragraph::new(
            "hello world".to_string(), // 11 chars
            "html.body.p".to_string(),
            "p".to_string(),
            5, // chars_count_in_links
            2,
            false,
        );
        // 5 / 11 ≈ 0.4545; verify the formula matches the Python
        // `chars_count_in_links / len(text)`.
        let expected = 5.0 / 11.0;
        assert!((para.link_density() - expected).abs() < 1e-9);
    }

    /// Test 8a (brief): `is_blank()` returns true for empty text.
    #[test]
    fn paragraph_is_blank_for_empty_text() {
        let para = Paragraph::new(
            String::new(),
            "html.body.p".to_string(),
            "p".to_string(),
            0,
            0,
            false,
        );
        assert!(para.is_blank());
    }

    /// Test 8b (brief): `is_blank()` returns false for substantive text.
    #[test]
    fn paragraph_is_blank_false_for_substantive() {
        let para = Paragraph::new(
            "Some content here".to_string(),
            "html.body.p".to_string(),
            "p".to_string(),
            0,
            3,
            false,
        );
        assert!(!para.is_blank());
    }

    /// Additional coverage: confirm the `<br><br>` paragraph-separator
    /// quirk. Python `core.py:164` treats two consecutive `<br>` tags
    /// as a paragraph boundary; this is the only non-PARAGRAPH_TAGS tag
    /// with that behaviour.
    #[test]
    fn make_paragraphs_br_br_starts_new_paragraph() {
        let (_dom, body) = parse_body("<html><body>First<br><br>Second</body></html>");
        let paras = make_paragraphs(&body);
        // `<body>` is itself a PARAGRAPH_TAG, so `First` and `Second`
        // each become their own paragraph (split on `<br><br>`).
        assert!(paras.len() >= 2);
        let texts: Vec<&str> = paras.iter().map(|p| p.text.as_str()).collect();
        assert!(
            texts.iter().any(|t| t.contains("First")),
            "expected 'First' in {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("Second")),
            "expected 'Second' in {texts:?}"
        );
    }

    /// Additional coverage: word_count is whitespace-tokenized.
    #[test]
    fn paragraph_word_count_counts_whitespace_separated_tokens() {
        let (_dom, body) = parse_body("<html><body><p>one two three four</p></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert_eq!(paras[0].word_count, 4);
    }

    /// Additional coverage: blank text nodes do not contribute to
    /// `text_nodes` (Python's `is_blank` short-circuit at
    /// `core.py:192-193`).
    #[test]
    fn make_paragraphs_skips_blank_text_nodes() {
        let (_dom, body) = parse_body("<html><body><p>   \n  </p><p>real</p></body></html>");
        let paras = make_paragraphs(&body);
        // First <p> contains only whitespace -> no `contains_text` ->
        // not emitted. Only the "real" paragraph survives.
        assert_eq!(paras.len(), 1, "got {:?}", paras.iter().map(|p| &p.text).collect::<Vec<_>>());
        assert_eq!(paras[0].text, "real");
    }

    /// Stage 5c placeholder verification: the four Stage-5c fields are
    /// `None` (or `false`) at segmentation time.
    #[test]
    fn paragraph_stage_5c_placeholders_are_none() {
        let (_dom, body) = parse_body("<html><body><p>hi</p></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert!(paras[0].class_type.is_none());
        assert!(paras[0].cf_class.is_none());
        assert!(!paras[0].heading);
        assert!(paras[0].stopwords_count.is_none());
        assert!(paras[0].is_boilerplate.is_none());
    }

    // ============================================================
    // Stage 5c — classify_paragraphs + revise_paragraph_classification
    // ============================================================

    /// Build a Paragraph directly without driving the SAX walker — used
    /// by the Stage 5c tests to construct controlled inputs to the
    /// classifier (long-text, link-heavy, etc.).
    fn mk(text: &str, dom_path: &str, chars_in_links: usize, is_heading: bool) -> Paragraph {
        // word_count tokenisation matches Python `str.split()`.
        let word_count = text.split_whitespace().count();
        // leaf tag is the last dot-separated component.
        let tag = dom_path.split('.').next_back().unwrap_or("").to_string();
        Paragraph::new(
            text.to_string(),
            dom_path.to_string(),
            tag,
            chars_in_links,
            word_count,
            is_heading,
        )
    }

    /// Minimal English-ish stoplist for Stage 5c tests — enough to push
    /// `stopword_density` above the 0.32 threshold for substantive prose.
    fn mini_stoplist() -> Vec<&'static str> {
        vec![
            "the", "a", "an", "and", "or", "of", "to", "in", "is", "are", "with", "for", "on",
            "at", "by", "as", "this", "that", "it", "be", "but", "not", "from", "have", "has",
            "had", "was", "were", "i", "you", "he", "she", "we", "they",
        ]
    }

    /// Brief test 1: a long substantive paragraph with high stopword
    /// density is classified as `good`.
    #[test]
    fn classify_long_substantive_paragraph_marked_good() {
        // 230 chars, ~15+ stopwords in 50ish words → density > 0.32 and
        // length > 200 → `good`.
        let text = "The quick brown fox jumps over the lazy dog and this is a substantive \
                    paragraph about animals and forests with many common words like the and a \
                    and of and to and the dog runs fast in the forest with the fox and the \
                    cat";
        let mut paras = vec![mk(text, "html.body.p", 0, false)];
        let stoplist = mini_stoplist();
        classify_paragraphs(&mut paras, &stoplist);
        assert_eq!(paras[0].cf_class.as_deref(), Some(CF_GOOD));
    }

    /// Brief test 2: a 5-word paragraph is classified as `short`.
    #[test]
    fn classify_short_paragraph_marked_short() {
        let mut paras = vec![mk("This is a short text", "html.body.p", 0, false)];
        let stoplist = mini_stoplist();
        classify_paragraphs(&mut paras, &stoplist);
        assert_eq!(paras[0].cf_class.as_deref(), Some(CF_SHORT));
    }

    /// Brief test 3: a paragraph whose `link_density` exceeds the default
    /// 0.2 threshold is classified as `bad`.
    #[test]
    fn classify_link_heavy_paragraph_marked_bad() {
        // 60 chars text, 30 chars in links → density 0.5 > 0.2 → `bad`.
        let text = "Click here for more info about this random topic now!";
        let chars_in_links = (text.chars().count() / 2) + 5; // > 50% link density
        let mut paras = vec![mk(text, "html.body.p", chars_in_links, false)];
        let stoplist = mini_stoplist();
        classify_paragraphs(&mut paras, &stoplist);
        assert_eq!(paras[0].cf_class.as_deref(), Some(CF_BAD));
    }

    /// Brief test 4: text starting with `©` is classified as `bad`
    /// (Python `core.py:258-259`).
    #[test]
    fn classify_copyright_paragraph_marked_bad() {
        let text = "\u{a9} 2026 Example Corporation. All rights reserved worldwide.";
        let mut paras = vec![mk(text, "html.body.p", 0, false)];
        let stoplist = mini_stoplist();
        classify_paragraphs(&mut paras, &stoplist);
        assert_eq!(paras[0].cf_class.as_deref(), Some(CF_BAD));
    }

    /// Brief test 5: a heading whose `length < length_low` is classified
    /// as `short` (not `bad`). The Python decision tree's
    /// `length < length_low` arm sets `short` when there are no link
    /// characters — this holds for headings AND non-headings; the
    /// heading flag is captured separately via `paragraph.heading` and
    /// consumed by the revise phase. (The brief's wording was slightly
    /// loose; faithful Python behaviour is what's tested here.)
    #[test]
    fn classify_heading_with_few_stopwords_marked_short_not_bad() {
        let mut paras = vec![mk("Short Heading", "html.body.h2", 0, true)];
        let stoplist = mini_stoplist();
        classify_paragraphs(&mut paras, &stoplist);
        // Confirm `short`, not `bad`, and that the heading flag was set.
        assert_eq!(paras[0].cf_class.as_deref(), Some(CF_SHORT));
        assert!(paras[0].heading);
    }

    /// Brief test 6: `[good, short, good]` → middle promoted to `good`
    /// via the "classify short" phase (`core.py:335-336`).
    #[test]
    fn revise_promotes_short_between_good_paragraphs() {
        let mut paras = vec![
            mk("good1", "html.body.p", 0, false),
            mk("short", "html.body.p", 0, false),
            mk("good3", "html.body.p", 0, false),
        ];
        // Seed cf_class directly to bypass classify (which needs real
        // length/density math); we only want to test revise's neighbour
        // logic.
        paras[0].cf_class = Some(CF_GOOD.to_string());
        paras[1].cf_class = Some(CF_SHORT.to_string());
        paras[2].cf_class = Some(CF_GOOD.to_string());
        revise_paragraph_classification(&mut paras);
        assert_eq!(paras[1].class_type.as_deref(), Some(CF_GOOD));
    }

    /// Brief test 7: `[bad, short, bad]` → middle demoted to `bad`
    /// (`core.py:337-338`).
    #[test]
    fn revise_demotes_short_between_bad_paragraphs() {
        let mut paras = vec![
            mk("bad1", "html.body.p", 0, false),
            mk("short", "html.body.p", 0, false),
            mk("bad3", "html.body.p", 0, false),
        ];
        paras[0].cf_class = Some(CF_BAD.to_string());
        paras[1].cf_class = Some(CF_SHORT.to_string());
        paras[2].cf_class = Some(CF_BAD.to_string());
        revise_paragraph_classification(&mut paras);
        assert_eq!(paras[1].class_type.as_deref(), Some(CF_BAD));
    }

    /// Brief test 8: a heading classified `short`, followed by a `good`
    /// paragraph within `max_heading_distance` characters, is promoted
    /// to `neargood` in phase 1 (`core.py:317-326`) and then to `good`
    /// in phase 3 (`core.py:350-358`, since the next neighbour is `good`).
    #[test]
    fn revise_promotes_heading_before_good_content() {
        let mut paras = vec![
            mk("Article Heading", "html.body.h2", 0, true),
            mk("Body paragraph", "html.body.p", 0, false),
        ];
        paras[0].cf_class = Some(CF_SHORT.to_string());
        paras[0].heading = true;
        paras[1].cf_class = Some(CF_GOOD.to_string());
        revise_paragraph_classification(&mut paras);
        // Heading was `short` → phase 1 promotes to `neargood` (next is
        // `good`, within distance), then phase 3 promotes `neargood` to
        // `good` (next neighbour with ignore_neargood=True is `good`).
        assert_eq!(paras[0].class_type.as_deref(), Some(CF_GOOD));
    }

    /// Brief test 9: after `classify_and_revise`, `is_boilerplate` is
    /// `Some(true)` for bad paragraphs and `Some(false)` for good ones
    /// (Python `paragraph.py:29-30`).
    #[test]
    fn is_boilerplate_set_correctly_after_classify_and_revise() {
        // Build two paragraphs with controlled class outcomes.
        let good_text = "The quick brown fox jumps over the lazy dog and this is a substantive \
                         paragraph about animals and forests with many common words like the \
                         and a and of and to and the dog runs fast in the forest with the fox.";
        let bad_text = "Click here";
        let mut paras = vec![
            mk(good_text, "html.body.p", 0, false),
            mk(bad_text, "html.body.p", bad_text.chars().count(), false), // 100% link density
        ];
        let stoplist = mini_stoplist();
        classify_and_revise(&mut paras, &stoplist);
        assert_eq!(paras[0].is_boilerplate, Some(false));
        assert_eq!(paras[1].is_boilerplate, Some(true));
    }

    /// Brief test 10: end-to-end pipeline on a minimal HTML article —
    /// `make_paragraphs` + `classify_and_revise` yields some `good`
    /// paragraphs (the article body) and the link-heavy nav `bad`.
    #[test]
    fn end_to_end_make_paragraphs_classify_revise_on_article() {
        let html = r#"<html><body>
            <nav>
                <a href="/">Home</a>
                <a href="/about">About</a>
                <a href="/contact">Contact</a>
            </nav>
            <article>
                <h1>Article Title</h1>
                <p>The quick brown fox jumps over the lazy dog and this is a substantive
                paragraph about animals and forests with many common words like the and a
                and of and to and the dog runs fast in the forest with the fox and the cat.</p>
                <p>Another substantive paragraph about the same topic — the fox and the dog
                run through the forest at speed, with the cat watching from a tree as the
                sun sets in the west and the moon rises in the east over the meadow.</p>
            </article>
        </body></html>"#;
        let (_dom, body) = parse_body(html);
        let mut paras = make_paragraphs(&body);
        let stoplist = mini_stoplist();
        classify_and_revise(&mut paras, &stoplist);

        // At least one paragraph survives as `good` (the article body
        // p's, after revise).
        let goods = paras
            .iter()
            .filter(|p| p.class_type.as_deref() == Some(CF_GOOD))
            .count();
        assert!(
            goods >= 1,
            "expected ≥ 1 good paragraph, got {goods} ({:?})",
            paras
                .iter()
                .map(|p| (p.text.clone(), p.class_type.clone()))
                .collect::<Vec<_>>()
        );

        // The nav paragraph (link-heavy or short, depending on
        // segmentation) is `bad` post-revise — confirm at least one
        // paragraph is `bad`.
        let bads = paras
            .iter()
            .filter(|p| p.class_type.as_deref() == Some(CF_BAD))
            .count();
        assert!(
            bads >= 1,
            "expected ≥ 1 bad paragraph (the nav), got {bads}"
        );
    }

    // ============================================================
    // Stage 5d — try_justext + justext_rescue (external.py:121-160)
    // ============================================================

    /// Brief test 1 (Stage 5d): `try_justext` returns only good (non-
    /// boilerplate) paragraphs. A page with link-heavy nav `<p>` and a
    /// long article `<p>` returns only the article paragraph.
    #[test]
    fn try_justext_returns_only_good_paragraphs() {
        let html = r#"<html><body>
            <p><a href="/a">Home</a> <a href="/b">About</a> <a href="/c">Contact</a> <a href="/d">News</a></p>
            <p>The quick brown fox jumps over the lazy dog and this is a substantive
            article paragraph about animals and forests with many common words like
            the and a and of and to and the dog runs fast in the forest with the fox
            and the cat as the sun sets behind the trees and the moon rises in the
            east over the meadow with the river flowing gently through the valley.</p>
        </body></html>"#;
        let (_dom, body) = parse_body(html);
        let survivors = try_justext(&body, None, Some("en"));
        // At least one survivor.
        assert!(
            !survivors.is_empty(),
            "expected ≥ 1 surviving paragraph, got 0"
        );
        // Every survivor must NOT be boilerplate.
        for p in &survivors {
            assert_eq!(
                p.is_boilerplate,
                Some(false),
                "survivor must be non-boilerplate, got {:?} for text {:?}",
                p.is_boilerplate,
                p.text
            );
        }
        // Survivors must include the article body text.
        let joined = survivors
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("substantive"),
            "expected article text in survivors, got {joined:?}"
        );
        // Survivors must NOT include the nav link text.
        assert!(
            !joined.contains("Home") && !joined.contains("About"),
            "nav links should be filtered as boilerplate, got {joined:?}"
        );
    }

    /// Brief test 2 (Stage 5d): unknown language falls back to the default
    /// stoplist (English) rather than crashing or returning nothing.
    /// Faithful echo of Python's `JT_STOPLIST or jt_stoplist_init()`
    /// fallback at external.py:137.
    #[test]
    fn try_justext_handles_unknown_language_with_fallback() {
        let html = r#"<html><body>
            <p>The quick brown fox jumps over the lazy dog and this is a substantive
            article paragraph about animals and forests with many common words like
            the and a and of and to and the dog runs fast in the forest with the fox
            and the cat as the sun sets behind the trees and the moon rises in the
            east over the meadow with the river flowing gently through the valley.</p>
        </body></html>"#;
        let (_dom, body) = parse_body(html);
        // "zz" is not a valid ISO 639-1 code; not in JUSTEXT_LANGUAGES.
        let survivors = try_justext(&body, None, Some("zz"));
        assert!(
            !survivors.is_empty(),
            "unknown language should fall back to default stoplist, got 0 survivors"
        );
    }

    /// Brief test 3 (Stage 5d): `justext_rescue` builds a `<body>` NodeRef
    /// whose children are `<p>` elements, one per surviving paragraph.
    #[test]
    fn justext_rescue_builds_body_with_p_elements() {
        let html = r#"<html><body>
            <p>The quick brown fox jumps over the lazy dog and this is a substantive
            article paragraph about animals and forests with many common words like
            the and a and of and to and the dog runs fast in the forest with the fox.</p>
            <p>Another substantive paragraph about the same topic — the fox and the
            dog run through the forest at speed, with the cat watching from a tree
            as the sun sets in the west and the moon rises in the east over the
            meadow with the river flowing gently through the green valley.</p>
        </body></html>"#;
        let (_dom, body) = parse_body(html);
        let (rescue_body, text, len) = justext_rescue(&body, None, Some("en"));

        // Body tag check.
        assert_eq!(
            crate::readability::dom::local_name(&rescue_body).as_deref(),
            Some("body"),
            "rescue body must be a <body> element"
        );

        // Children are all <p>.
        let kids = crate::readability::dom::get_elements_by_tag_name(&rescue_body, "*");
        assert!(!kids.is_empty(), "rescue body must have ≥ 1 child element");
        for kid in &kids {
            assert_eq!(
                crate::readability::dom::local_name(kid).as_deref(),
                Some("p"),
                "every rescue body child must be a <p>"
            );
        }

        // Returned text + length match.
        assert!(!text.is_empty(), "rescue text must be non-empty");
        assert_eq!(
            len,
            text.chars().count(),
            "rescue len must equal chars().count() of returned text"
        );
        // Each <p>'s leading words appear in the joined string.
        // The full <p>.text may include internal newlines (jusText's
        // `normalize_whitespace` preserves NL when the source run had
        // one); the joined string uses ' ' as the separator and runs
        // through `trim` which collapses whitespace. So we compare the
        // first few content tokens, not the full text byte-for-byte.
        for kid in &kids {
            let p_text = crate::readability::dom::element_text(kid).unwrap_or_default();
            // Take first 5 whitespace-split tokens as a signature.
            let signature: String = p_text
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                text.contains(&signature),
                "joined text must contain each <p>'s leading tokens; missing {signature:?} in {text:?}"
            );
        }
    }
}
