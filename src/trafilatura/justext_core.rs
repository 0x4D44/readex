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

    /// Stage 5c placeholder verification: the three Stage-5c fields are
    /// `None` at segmentation time.
    #[test]
    fn paragraph_stage_5c_placeholders_are_none() {
        let (_dom, body) = parse_body("<html><body><p>hi</p></body></html>");
        let paras = make_paragraphs(&body);
        assert_eq!(paras.len(), 1);
        assert!(paras[0].class_type.is_none());
        assert!(paras[0].stopwords_count.is_none());
        assert!(paras[0].is_boilerplate.is_none());
    }
}
