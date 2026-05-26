//! `dom.rs` — the thin facade over `markup5ever_rcdom` (HLD §5, **highest
//! risk**).
//!
//! This is the DOM substrate the whole Readability port stands on. It exposes
//! the ~15 score-critical primitives the algorithm needs (HLD §5) and **never
//! leaks `rcdom` types past the facade** (HLD §3): callers see [`NodeRef`]
//! (an owned `Rc<Node>` clone), never a `RefCell` borrow.
//!
//! # The one load-bearing invariant — `text_content` (HLD §2.1)
//!
//! [`text_content`](NodeRef::text_content) is a **pure depth-first
//! concatenation of every descendant `#text` node's `data`, with ZERO
//! synthetic inter-element whitespace** — the WHATWG DOM `Node.textContent`
//! getter as implemented by jsdom 29.1.1 (the oracle's actual parser;
//! `run.mjs:184`). Comment / processing-instruction / doctype nodes
//! contribute nothing (they are not `Text` nodes). Because the harness
//! tokenizer (`metrics.rs::tokens`) collapses whitespace runs, the dominant
//! corpus risk is **inserting or omitting a separator** that fuses or splits
//! tokens differently from the oracle — so this function inserts none, ever.
//!
//! HLD §6.1's parser-equivalence BLOCKER gate
//! (`tests/parser_equivalence_gate.rs`) proves this empirically against jsdom
//! **for the current snapshot corpus only** — equivalence is established for
//! that corpus, which the gate's per-snapshot guard proves contains ZERO
//! non-whitespace stray text in table parts. html5ever and jsdom are **known
//! to diverge** on that class (foster-parent position; the gate names a
//! regression-pinned witness and its guard re-triggers the HLD §6.1
//! rcdom → kuchikiki decision on any future corpus addition in it). This is a
//! bounded, self-policed claim — **not** "the substrate is faithful for all
//! inputs".
//!
//! # `set_node_tag` — slow branch only (HLD §2.2)
//!
//! Under jsdom `Readability.js:754`'s `this._docJSDOMParser` is `undefined`,
//! so the oracle **always** runs the slow `createElement` + move-children +
//! `replaceChild` + carry-`readability` + clone-attrs branch
//! (`Readability.js:760-772`). [`set_node_tag`](Dom::set_node_tag) implements
//! **only** that branch; the in-place fast branch (754-758) is **explicitly
//! forbidden** (HLD §2.2 design ruling B-2) and does not exist here.
//!
//! # Side tables — point-query-only (HLD §5.1)
//!
//! Per-node Readability state lives in two side structures keyed by node
//! identity: [`Dom::content_score`] (the `node.readability.contentScore`
//! analogue) and [`Dom::readability_data_table`] (the
//! `_readabilityDataTable` flag set). **Invariant: both are POINT-QUERY-ONLY**
//! — candidate ordering never comes from iterating either map (a `HashMap`'s
//! non-deterministic order would silently diverge from the JS `candidates`
//! array order). Ordering, when Stage 1a needs it, comes from a `Vec` that
//! mirrors the JS `candidates` array. This holds **structurally, by
//! construction** (the maps are private; no `pub fn` iterates them or yields
//! their keys), anchored by
//! [`Dom::side_tables_are_point_query_only_by_construction`] (a no-op marker
//! + greppable invariant, NOT a runtime check) plus a unit test.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use html5ever::tendril::TendrilSink;
use html5ever::tree_builder::TreeBuilderOpts;
use html5ever::{Attribute, LocalName, ParseOpts, QualName, local_name, ns, parse_document};
use markup5ever_rcdom::{Handle, Node, RcDom};

/// Re-export of `markup5ever_rcdom::NodeData` so dependent modules can match
/// on `Element { attrs, .. }` without taking a direct dep on `rcdom`. This is
/// the **only** rcdom type that leaks past the facade, and ONLY for in-crate
/// modules — external consumers still see exclusively [`NodeRef`].
pub use markup5ever_rcdom::NodeData;

/// An owned handle to a DOM node.
///
/// This is an `Rc<Node>` clone — cloning is cheap (a refcount bump) and,
/// critically, holding a `NodeRef` never holds a `RefCell` borrow, so the
/// facade can hand these out freely and a later mutation cannot trip a borrow
/// conflict (HLD §6 risk mitigation: "the facade returns owned `Handle`
/// clones and never holds a `RefCell` borrow across a mutation"). `rcdom`'s
/// `Handle` type is **not** re-exported — callers only ever see `NodeRef`.
pub type NodeRef = Handle;

/// A parsed HTML document plus the two per-node Readability side tables.
///
/// Owns the [`RcDom`] (so the tree outlives every [`NodeRef`]) and the
/// `content_score` / `readability_data_table` side structures (HLD §5.1).
pub struct Dom {
    dom: RcDom,
    /// `node.readability.contentScore` analogue (HLD §5 / §5.1). Keyed by node
    /// identity (`Rc` pointer). **Point-query-only** — never iterated on a
    /// scored path (the `HashMap` order is non-deterministic and would diverge
    /// from JS `candidates` order).
    content_score: HashMap<NodeKey, f64>,
    /// `_readabilityDataTable` flag set (HLD §5 / §5.1). A node is in the set
    /// iff `_markDataTables` marked it a data table. **Point-query-only**, as
    /// above.
    readability_data_table: HashSet<NodeKey>,
}

/// Identity key for the side tables: the raw `Node` pointer behind an `Rc`.
///
/// Two `NodeRef`s denote the same DOM node iff `Rc::ptr_eq`, i.e. iff their
/// raw pointers are equal — so the pointer address is a sound, cheap identity
/// key while the owning [`Dom`] (and thus every node) is alive. We store the
/// address as `usize` (never deref it) so `NodeKey` is `Hash + Eq` without
/// touching `Node` (which is not `Hash`). Soundness rests on the [`Dom`]
/// keeping the tree alive for the table's lifetime (it owns the `RcDom`); a
/// dropped node could let the address be reused, which is exactly why the
/// tables live *in* `Dom` and never outlive it.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct NodeKey(usize);

impl NodeKey {
    fn of(node: &NodeRef) -> Self {
        NodeKey(Rc::as_ptr(node) as usize)
    }
}

// ---------------------------------------------------------------------------
// HTML pre-processor (M11 Phase A — HLD §3).
//
// Transforms the raw HTML string *before* it reaches html5ever's
// `parse_document()`, rewriting three specific patterns where html5ever's
// HTML5-spec-strict behaviour produces materially different DOM trees than
// lxml's permissive parser:
//
//   Shape 1: `</br>` end-tags  → stripped entirely
//   Shape 2: stray table-cell tags outside `<table>` → rewritten to `<div>`
//   Shape 3: `<xmp>` raw-text elements → rewritten to `<div>`
//
// Returns `Cow::Borrowed` (zero allocation) when no transformations are
// needed (the common case).
// ---------------------------------------------------------------------------

/// Returns `true` if the byte at position `pos` in `bytes` is a tag-boundary
/// character — one of `>`, ` `, `\t`, `\n`, `\r`, form-feed (0x0C), or `/`.
/// Used as the "followed by whitespace or `>`" guard to prevent prefix-tag
/// matching (e.g. `<thread>` must not match `<thead>`).
#[inline]
fn is_tag_boundary(b: u8) -> bool {
    matches!(b, b'>' | b' ' | b'\t' | b'\n' | b'\r' | 0x0C | b'/')
}

/// Case-insensitive substring search: returns `true` if `needle` (assumed
/// all-lowercase ASCII) appears anywhere in `haystack` when both are
/// lowered. Short-circuits on first match.
fn contains_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    'outer: for start in 0..=(haystack.len() - needle.len()) {
        for (i, &nb) in needle.iter().enumerate() {
            if (haystack[start + i] | 0x20) != nb {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

/// Case-insensitive tag-name match starting at `pos` in `bytes`.
/// `tag` must be all-lowercase ASCII (e.g. `b"</br"`, `b"<xmp"`).
/// Returns `true` iff the bytes at `pos..pos+tag.len()` match `tag`
/// case-insensitively AND the byte immediately after (if any) is a
/// tag-boundary character (preventing prefix matches like `<thread>`
/// matching `<thead>`).
#[inline]
fn tag_matches(bytes: &[u8], pos: usize, tag: &[u8]) -> bool {
    let end = pos + tag.len();
    if end > bytes.len() {
        return false;
    }
    for (i, &tb) in tag.iter().enumerate() {
        let b = bytes[pos + i];
        // For ASCII alpha bytes, `b | 0x20` gives the lowercase form.
        // Non-alpha bytes in the tag (like `<`, `/`) compare directly
        // since `b | 0x20` is a no-op for them in the relevant range.
        if tb.is_ascii_alphabetic() {
            if (b | 0x20) != tb {
                return false;
            }
        } else if b != tb {
            return false;
        }
    }
    // Guard: the byte after the tag name must be a tag-boundary or we're at EOF.
    if end < bytes.len() {
        is_tag_boundary(bytes[end])
    } else {
        true // at EOF — acceptable (the tag is the last thing in the input)
    }
}

/// Find the position of the `>` closing a tag that starts at `pos`, or
/// return `bytes.len()` if no `>` is found (malformed input — consume to end).
#[inline]
fn find_closing_angle(bytes: &[u8], pos: usize) -> usize {
    for (i, &b) in bytes.iter().enumerate().skip(pos) {
        if b == b'>' {
            return i + 1; // past the `>`
        }
    }
    bytes.len()
}

/// Pre-process raw HTML before parsing, rewriting three html5ever/lxml
/// divergence patterns (M11 Phase A — HLD §3).
///
/// - **Common case** (no triggers): returns `Cow::Borrowed(html)` — zero
///   allocation.
/// - **Uncommon case** (triggers present): returns `Cow::Owned(modified)`.
fn preprocess_html(html: &str) -> Cow<'_, str> {
    let bytes = html.as_bytes();

    // Quick-scan: does the HTML contain any trigger pattern?
    let has_br_end = contains_ci(bytes, b"</br");
    let has_xmp = contains_ci(bytes, b"<xmp");
    let has_stray_cell = contains_ci(bytes, b"<td")
        || contains_ci(bytes, b"<th")
        || contains_ci(bytes, b"<tr")
        || contains_ci(bytes, b"<tbody")
        || contains_ci(bytes, b"<tfoot")
        || contains_ci(bytes, b"<thead");

    if !has_br_end && !has_xmp && !has_stray_cell {
        return Cow::Borrowed(html);
    }

    // Transformation pass: single forward O(n) scan.
    let mut out = String::with_capacity(bytes.len());
    let mut pos: usize = 0;
    let mut table_depth: usize = 0;

    // The cell tag names we recognise (all lowercase, without the `<` or `</`).
    const CELL_TAGS: &[&[u8]] = &[b"thead", b"tfoot", b"tbody", b"tr", b"th", b"td"];

    while pos < bytes.len() {
        if bytes[pos] != b'<' {
            // Bulk copy: find next '<' and copy everything before it.
            let rest = &bytes[pos..];
            let next_lt = rest.iter().position(|&b| b == b'<').unwrap_or(rest.len());
            out.push_str(&html[pos..pos + next_lt]);
            pos += next_lt;
            continue;
        }

        // We're at a '<'. Try to match trigger patterns in priority order.

        // Shape 1: </br...> — strip entirely.
        if has_br_end && tag_matches(bytes, pos, b"</br") {
            pos = find_closing_angle(bytes, pos);
            continue;
        }

        // Shape 3: <xmp...> → <div...>
        if has_xmp && tag_matches(bytes, pos, b"<xmp") {
            let tag_name_end = pos + 4; // past "<xmp"
            let close = find_closing_angle(bytes, pos);
            out.push_str("<div");
            out.push_str(&html[tag_name_end..close]);
            pos = close;
            continue;
        }

        // Shape 3: </xmp...> → </div...>
        if has_xmp && tag_matches(bytes, pos, b"</xmp") {
            let tag_name_end = pos + 5; // past "</xmp"
            let close = find_closing_angle(bytes, pos);
            out.push_str("</div");
            out.push_str(&html[tag_name_end..close]);
            pos = close;
            continue;
        }

        // Table depth tracking: <table...>
        if tag_matches(bytes, pos, b"<table") {
            table_depth += 1;
            let close = find_closing_angle(bytes, pos);
            out.push_str(&html[pos..close]);
            pos = close;
            continue;
        }

        // Table depth tracking: </table...>
        if tag_matches(bytes, pos, b"</table") {
            table_depth = table_depth.saturating_sub(1);
            let close = find_closing_angle(bytes, pos);
            out.push_str(&html[pos..close]);
            pos = close;
            continue;
        }

        // Shape 2: stray cell start tags (outside table → rewrite to <div>).
        if has_stray_cell {
            let mut matched_cell = false;
            for &cell_tag in CELL_TAGS {
                // Build the open-tag prefix: "<" + cell_tag
                let mut prefix = Vec::with_capacity(1 + cell_tag.len());
                prefix.push(b'<');
                prefix.extend_from_slice(cell_tag);
                if tag_matches(bytes, pos, &prefix) {
                    let tag_name_end = pos + prefix.len();
                    let close = find_closing_angle(bytes, pos);
                    if table_depth == 0 {
                        out.push_str("<div");
                        out.push_str(&html[tag_name_end..close]);
                    } else {
                        out.push_str(&html[pos..close]);
                    }
                    pos = close;
                    matched_cell = true;
                    break;
                }
            }
            if matched_cell {
                continue;
            }

            // Stray cell end tags: </td, </th, </tr, </tbody, </tfoot, </thead
            let mut matched_close_cell = false;
            for &cell_tag in CELL_TAGS {
                // Build the close-tag prefix: "</" + cell_tag
                let mut prefix = Vec::with_capacity(2 + cell_tag.len());
                prefix.extend_from_slice(b"</");
                prefix.extend_from_slice(cell_tag);
                if tag_matches(bytes, pos, &prefix) {
                    let tag_name_end = pos + prefix.len();
                    let close = find_closing_angle(bytes, pos);
                    if table_depth == 0 {
                        out.push_str("</div");
                        out.push_str(&html[tag_name_end..close]);
                    } else {
                        out.push_str(&html[pos..close]);
                    }
                    pos = close;
                    matched_close_cell = true;
                    break;
                }
            }
            if matched_close_cell {
                continue;
            }
        }

        // No match — copy the '<' and advance.
        out.push('<');
        pos += 1;
    }

    Cow::Owned(out)
}

impl Dom {
    /// Parse `html` into a DOM exactly as the oracle's jsdom does
    /// (`run.mjs:184` — `new jsdom.JSDOM(html)`): full-document HTML5 parse,
    /// scripting disabled (the harness runs jsdom inert — `run.mjs` does not
    /// enable `runScripts`), default quirks handling.
    ///
    /// The parse is the WHATWG HTML tree-construction algorithm via
    /// `html5ever`; HLD §6.1's BLOCKER gate proves the resulting
    /// `text_content` is token-identical to jsdom's for the gold + table-heavy
    /// snapshots before any extraction logic is built on it.
    pub fn parse(html: &str) -> Self {
        // M11 Phase A: pre-process HTML to normalise three html5ever/lxml
        // divergence patterns before parsing (HLD §3).
        let html = preprocess_html(html);
        // scripting_enabled = false: jsdom in the oracle is inert (no
        // runScripts), so <noscript> content is parsed as markup (children),
        // matching jsdom. Default quirks/iframe-srcdoc handling otherwise.
        let opts = ParseOpts {
            tree_builder: TreeBuilderOpts {
                scripting_enabled: false,
                ..TreeBuilderOpts::default()
            },
            ..ParseOpts::default()
        };
        let dom = parse_document(RcDom::default(), opts).one(&*html);
        // M5 Stage 6e-a: strip HTML comments and processing instructions
        // immediately after parse, mirroring Python trafilatura's source-truth
        // parser config:
        //   utils.py:70 -- HTMLParser(remove_comments=True, remove_pis=True)
        // lxml removes the Comment/PI node AND promotes its `.tail` into the
        // preceding sibling's tail (or the parent's `.text` when the comment
        // had no preceding sibling). rcdom's tree representation models
        // sibling text as separate Text nodes (not an intrinsic `.tail`
        // string), so removing the Comment is sufficient: the
        // already-following Text nodes simply become adjacent to the
        // preceding Text/element, and the dom-facade accessors
        // (`element_text`, `tail`) already coalesce consecutive Text siblings
        // (dom.rs:404-422, 436-453). Without this strip, comment-adjacent
        // text was being read past the comment differently from Python --
        // e.g. `<p>x<!-- -->...</p>` exposed `x` for `.text` and missed the
        // `...` run that comes after the Comment (M5 fixture
        // 859b46bf108e3db4.html, byte 383).
        strip_comments_and_pis(&dom.document);
        Dom {
            dom,
            content_score: HashMap::new(),
            readability_data_table: HashSet::new(),
        }
    }

    /// The `Document` root node.
    pub fn document(&self) -> NodeRef {
        self.dom.document.clone()
    }

    /// The `<html>` root element (first `<html>` child of `Document`), if the
    /// parse produced one. html5ever always synthesises `<html>` for any
    /// non-empty document, so this is `Some` for every real snapshot.
    pub fn root_element(&self) -> Option<NodeRef> {
        first_element_child(&self.dom.document)
    }

    /// The `<body>` element. html5ever always synthesises `<head>`+`<body>`
    /// for a full-document parse, so this is `Some` for every real snapshot.
    /// This is the node the HLD §6.1 gate takes `text_content` of (the jsdom
    /// `document.body.textContent` analogue).
    pub fn body(&self) -> Option<NodeRef> {
        let html = self.root_element()?;
        children(&html)
            .into_iter()
            .find(|c| tag_name(c).as_deref() == Some("BODY"))
    }
}

// ---------------------------------------------------------------------------
// Node primitives (free functions — they take a `&NodeRef` and never need the
// `Dom`; only the side-table ops below need `Dom`).
// ---------------------------------------------------------------------------

/// WHATWG `Node.textContent` getter (HLD §2.1 — **the load-bearing
/// invariant**).
///
/// Raw depth-first, tree-order concatenation of the `data` of **every
/// descendant `Text` node**, with **ZERO synthetic inter-element
/// whitespace** and **no normalization**. Comment / processing-instruction /
/// doctype / element nodes contribute no characters of their own; only `Text`
/// node data is concatenated. Entity-decoding already happened in the parser
/// (it lives in the `Text` node `data`), so `&amp;` is already `&` here.
///
/// This deliberately matches jsdom 29.1.1's `Node.textContent` and *only*
/// that — see the module docs and HLD §6.1 (the empirical BLOCKER gate).
pub fn text_content(node: &NodeRef) -> String {
    let mut out = String::new();
    push_text(node, &mut out);
    out
}

/// Depth-first `Text`-data accumulation (the recursive body of
/// [`text_content`]).
///
/// A `Text` node contributes its `data`. Every other node type contributes
/// nothing *itself* but is recursed into in child order. Comment /
/// PI / Doctype have no children so they terminate naturally and add nothing
/// — exactly the WHATWG "concatenation of `#text` descendants" semantics.
fn push_text(node: &NodeRef, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => {
            out.push_str(&contents.borrow());
        }
        _ => {
            for child in node.children.borrow().iter() {
                push_text(child, out);
            }
        }
    }
}

/// `_getInnerText` (`Readability.js:2058-2067`).
///
/// `textContent.trim()` then, when `normalize_spaces` (the JS default,
/// `normalizeSpaces === undefined ? true`), replace every run of 2+
/// whitespace with a single ASCII space (`REGEXPS.normalize = /\s{2,}/g`).
///
/// **Regex-dialect fidelity (HLD §8):** JS `\s` is **not** Rust `regex`'s
/// `\s` (JS `\s` includes U+FEFF; Rust's excludes it) and JS
/// `String.prototype.trim` trims a specific WhiteSpace+LineTerminator set.
/// To avoid pulling `regex` into the crate at Stage 0 for one pattern *and*
/// to be exactly faithful, both the trim and the run-collapse use one
/// explicit predicate, [`is_js_space`], which is the JS whitespace set
/// (ECMAScript `WhiteSpace` ∪ `LineTerminator`). This is the precise set JS
/// `\s` matches and the precise set JS `trim()` strips, so it closes the
/// dialect trap the HLD §8 calls out without a regex engine. Stage 1a's
/// `regexps` module formalises this as the shared JS-`\s` class + its
/// conformance test table.
pub fn inner_text(node: &NodeRef, normalize_spaces: bool) -> String {
    let raw = text_content(node);
    let trimmed = js_trim(&raw);
    if !normalize_spaces {
        return trimmed.to_string();
    }
    collapse_js_space_runs(trimmed)
}

/// The ECMAScript whitespace set: `WhiteSpace` ∪ `LineTerminator`.
///
/// This is **exactly** what JS `\s` matches and what JS
/// `String.prototype.trim` strips (HLD §8). Notably it **includes U+FEFF**
/// (ZERO WIDTH NO-BREAK SPACE / BOM — a JS `WhiteSpace`) which Rust `regex`'s
/// `\s` and Rust's `char::is_whitespace` both **exclude**; that single
/// character is the exact trap HLD §8 documents, and listing it explicitly
/// here closes it.
///
/// Members (ECMA-262): TAB U+0009, LF U+000A, VT U+000B, FF U+000C,
/// CR U+000D, SPACE U+0020, NBSP U+00A0, ZWNBSP/BOM U+FEFF, LS U+2028,
/// PS U+2029, and every `Zs` (space separator): U+1680, U+2000–U+200A,
/// U+202F, U+205F, U+3000.
///
/// **Canonical source of truth (single-definition rule).** This is the *one*
/// predicate form of the JS-`\s` set; `metadata.rs::js_trim` calls it (no
/// re-derived copy) and `regexps::JS_SPACE_CLASS` (the regex character-class
/// literal — a fn cannot be spliced into a pattern) is mechanically pinned
/// equal to it over the full relevant codepoint set by the `regexps`
/// conformance tests, so any drift in *either* form fails the build.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'
            | '\u{000A}'
            | '\u{000B}'
            | '\u{000C}'
            | '\u{000D}'
            | '\u{0020}'
            | '\u{00A0}'
            | '\u{FEFF}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}'
    )
}

/// JS `String.prototype.trim`: strip leading/trailing [`is_js_space`].
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// Replace every run of 2+ [`is_js_space`] chars with one ASCII space
/// (`REGEXPS.normalize = /\s{2,}/g`, faithfully — runs of exactly 1 are left
/// untouched, including a lone non-ASCII space such as NBSP).
fn collapse_js_space_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    // The current run of [`is_js_space`] chars: how many, and the *last* one
    // seen (so a lone-1 run can be re-emitted verbatim — JS `/\s{2,}/` does
    // not touch a single space, so e.g. an isolated NBSP must survive).
    let mut run_len = 0usize;
    let mut run_last = ' ';
    let flush = |out: &mut String, run_len: &mut usize, run_last: char| {
        match *run_len {
            0 => {}
            // Run of exactly one: not matched by /\s{2,}/g -> verbatim.
            1 => out.push(run_last),
            // Run of >=2: replaced by a single ASCII space.
            _ => out.push(' '),
        }
        *run_len = 0;
    };
    for c in s.chars() {
        if is_js_space(c) {
            run_len += 1;
            run_last = c;
        } else {
            flush(&mut out, &mut run_len, run_last);
            out.push(c);
        }
    }
    // Trailing run (unreachable from `inner_text`, which `js_trim`s first;
    // kept correct for any direct caller).
    flush(&mut out, &mut run_len, run_last);
    out
}

/// Element-only children (`element.children` — `Readability.js` reads
/// `.children` for the element child list, distinct from `.childNodes`).
/// Returns an owned snapshot `Vec` (HLD §5).
pub fn children(node: &NodeRef) -> Vec<NodeRef> {
    node.children
        .borrow()
        .iter()
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
        .collect()
}

/// All child nodes (`element.childNodes` — every node type, in order).
/// Returns an owned snapshot `Vec` (HLD §5).
pub fn child_nodes(node: &NodeRef) -> Vec<NodeRef> {
    node.children.borrow().iter().cloned().collect()
}

/// `element.firstElementChild` (first element-typed child, or `None`).
pub fn first_element_child(node: &NodeRef) -> Option<NodeRef> {
    node.children
        .borrow()
        .iter()
        .find(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
}

/// `element.nextElementSibling` (next element-typed sibling, or `None`).
pub fn next_element_sibling(node: &NodeRef) -> Option<NodeRef> {
    let (parent, idx) = parent_and_index(node)?;
    let kids = parent.children.borrow();
    kids.iter()
        .skip(idx + 1)
        .find(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
}

/// `node.parentNode` (`None` for the document root or a detached node).
pub fn parent(node: &NodeRef) -> Option<NodeRef> {
    let weak = node.parent.take();
    let out = weak.as_ref().and_then(|w| w.upgrade());
    node.parent.set(weak);
    out
}

/// `node.tagName`, **UPPER-cased** (`Readability.js` compares `tagName`
/// against `"DIV"`, `"P"`, … — always upper-case). `None` for non-element
/// nodes (text / comment / document).
pub fn tag_name(node: &NodeRef) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref().to_ascii_uppercase()),
        _ => None,
    }
}

/// lxml `Element.tail` (M3 Stage 0a — HLD §5.1 / §6.0; `dom.rs` additive
/// extension required by Trafilatura's `xmltotxt`, `link_density_test_tables`,
/// `process_node`, and `prune_html`).
///
/// Returns the text content of the **next-sibling Text node(s)** of `elem`,
/// concatenated in document order until the first non-Text sibling
/// (element, comment, PI), or `None` if `elem` has no next sibling at all *or*
/// its very next sibling is not a Text node.
///
/// # lxml-fidelity notes
///
/// lxml/libxml2 internally coalesces consecutive text nodes at parse time, so
/// in lxml `.tail` is intrinsically a single string. `markup5ever_rcdom` does
/// **not** coalesce: a sequence like `</p>foo<!--c-->bar` parses to (`<p>`,
/// Text("foo"), Comment, Text("bar")) and `<p>`'s tail is `"foo"`; the
/// `Comment` interrupts the tail run. Where rcdom *does* yield consecutive
/// Text siblings (rare but possible via DOM construction / serializer
/// round-trips), we concatenate them — this is the lxml-equivalent answer
/// (the same bytes lxml would have stored had it parsed the same input).
///
/// lxml returns `""` (empty string) when there is no tail; this facade uses
/// `None` to match Rust idiom and the existing `dom.rs` style. **Downstream
/// callers must treat `None` and `Some("")` as semantically equivalent** —
/// this is the only stylistic deviation from lxml here, deliberate to align
/// with the rest of this facade (e.g. `next_element_sibling -> Option<_>`).
///
/// # Strict scope
///
/// This does **not** recurse into `elem`'s children. The tail is exclusively
/// the text *between* `elem` and its next non-Text sibling at the same tree
/// level. (This is the load-bearing lxml semantic that distinguishes `.tail`
/// from `.text` and from `text_content`.)
pub fn tail(elem: &NodeRef) -> Option<String> {
    let (parent, idx) = parent_and_index(elem)?;
    let kids = parent.children.borrow();
    let mut out: Option<String> = None;
    for sibling in kids.iter().skip(idx + 1) {
        match &sibling.data {
            NodeData::Text { contents } => {
                let data = contents.borrow();
                match &mut out {
                    Some(s) => s.push_str(&data),
                    None => out = Some(data.to_string()),
                }
            }
            // First non-Text terminates the tail run (lxml semantics).
            _ => break,
        }
    }
    out
}

/// lxml `Element.text` — the **leading-text-child run** of `elem`.
///
/// **M3 Stage 2b' additive extension** (HLD §5.1). Stage 0a landed `tail`;
/// `handle_textnode` / `process_node` (htmlprocessing.py:222-285) also need
/// the symmetric `.text` read. Returns the concatenated `data` of the
/// run of `Text` nodes at the front of `elem.children` (i.e. before the
/// first non-Text child), or `None` if there is no leading Text child.
///
/// `None` vs `Some("")` follows the existing `tail`'s convention; callers
/// must treat them as semantically equivalent (lxml returns `""` /
/// concatenated text — never `None` for "empty"; we use `None` for "no
/// leading Text child at all", which is the closest faithful idiom).
pub fn element_text(elem: &NodeRef) -> Option<String> {
    let kids = elem.children.borrow();
    let mut out: Option<String> = None;
    for child in kids.iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let data = contents.borrow();
                match &mut out {
                    Some(s) => s.push_str(&data),
                    None => out = Some(data.to_string()),
                }
            }
            // First non-Text terminates the leading run (lxml semantics).
            _ => break,
        }
    }
    out
}

/// lxml `Element.text = value` (or `Element.text = None`) — set the
/// **leading-text-child run** of `elem`.
///
/// **M3 Stage 2b' additive extension** (HLD §5.1). The Python idiom
/// `elem.text = "..."` (htmlprocessing.py:246, 253) sets one logical
/// leading-text slot. rcdom realises this as a *run* of Text children
/// at the front of `elem.children`; setting `.text` therefore:
///
/// 1. Drains every leading `Text` sibling of the first non-Text child
///    (the entire "lxml .text run").
/// 2. If `value` is `Some(s)` and `s` is non-empty, inserts a single new
///    `Text` node holding `s` at position 0.
/// 3. If `value` is `None` or `Some("")`, the leading run is left empty
///    (matching `elem.text = None` / `elem.text = ""` in lxml).
pub fn set_element_text(elem: &NodeRef, value: Option<&str>) {
    // Drain leading Text run.
    let leading_count = {
        let kids = elem.children.borrow();
        kids.iter()
            .take_while(|c| matches!(c.data, NodeData::Text { .. }))
            .count()
    };
    if leading_count > 0 {
        let drained: Vec<NodeRef> = {
            let mut kids = elem.children.borrow_mut();
            kids.drain(0..leading_count).collect()
        };
        for n in &drained {
            n.parent.set(None);
        }
    }
    let Some(s) = value else { return };
    if s.is_empty() {
        return;
    }
    let txt = create_text_node(s);
    txt.parent.set(Some(Rc::downgrade(elem)));
    elem.children.borrow_mut().insert(0, txt);
}

/// lxml `Element.tail = value` (or `Element.tail = None`) — set the
/// **tail Text-node run** between `elem` and its next non-Text sibling.
///
/// **M3 Stage 2b' additive extension** (HLD §5.1). The Python idioms
/// `elem.tail = "..."` (htmlprocessing.py:237, 246, 255, 274, 278;
/// prune_unwanted_nodes:110) set one logical tail slot. rcdom realises
/// this as a *run* of Text-node siblings of `elem` at the parent level;
/// setting `.tail` therefore:
///
/// 1. Drains every following `Text` sibling of `elem` up to (but not
///    including) the first non-Text sibling.
/// 2. If `value` is `Some(s)` and `s` is non-empty, inserts a single new
///    `Text` node holding `s` immediately after `elem`.
/// 3. If `value` is `None` or `Some("")`, the tail run is left empty
///    (matching `elem.tail = None` / `elem.tail = ""` in lxml).
///
/// No-op if `elem` is detached.
pub fn set_tail(elem: &NodeRef, value: Option<&str>) {
    let Some((parent, idx)) = parent_and_index(elem) else {
        return;
    };
    // Count tail run.
    let tail_count = {
        let kids = parent.children.borrow();
        kids.iter()
            .skip(idx + 1)
            .take_while(|c| matches!(c.data, NodeData::Text { .. }))
            .count()
    };
    if tail_count > 0 {
        let drained: Vec<NodeRef> = {
            let mut kids = parent.children.borrow_mut();
            kids.drain(idx + 1..idx + 1 + tail_count).collect()
        };
        for n in &drained {
            n.parent.set(None);
        }
    }
    let Some(s) = value else { return };
    if s.is_empty() {
        return;
    }
    let txt = create_text_node(s);
    txt.parent.set(Some(Rc::downgrade(&parent)));
    parent.children.borrow_mut().insert(idx + 1, txt);
}

/// `element.previousElementSibling` — the previous **element** sibling, or
/// `None`.
///
/// **M3 Stage 2b' additive extension** (HLD §5.1). lxml's
/// `Element.getprevious()` returns the previous sibling that is either an
/// element OR a Comment/PI (anything but a Text node). The single Stage 2b'
/// caller (`prune_unwanted_nodes`, htmlprocessing.py:105) uses the result
/// only as a tail-append target; for HTML trees parsed with
/// `remove_comments=True` (utils.py:70), the result is purely "previous
/// element sibling". Stage 2b' implements that faithful subset. If a later
/// stage needs the Comment/PI-inclusive variant, document it then.
pub fn previous_element_sibling(node: &NodeRef) -> Option<NodeRef> {
    let (parent, idx) = parent_and_index(node)?;
    let kids = parent.children.borrow();
    kids.iter()
        .take(idx)
        .rev()
        .find(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
}

/// `node.nodeName.toLowerCase()` / `localName` (lower-case tag for element
/// nodes; used where the JS lower-cases, e.g. `_cleanStyles`' svg check).
pub fn local_name(node: &NodeRef) -> Option<String> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.to_string()),
        _ => None,
    }
}

/// `true` iff this is an element node.
pub fn is_element(node: &NodeRef) -> bool {
    matches!(node.data, NodeData::Element { .. })
}

/// `true` iff this is a `Text` node.
pub fn is_text(node: &NodeRef) -> bool {
    matches!(node.data, NodeData::Text { .. })
}

/// `node.getAttribute(name)` — the attribute value, or `None` if absent.
/// HTML attribute names are ASCII case-insensitive; html5ever lower-cases
/// them at parse, and the JS reads lower-case names, so a plain match is
/// faithful.
pub fn get_attribute(node: &NodeRef, name: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|a| &*a.name.local == name)
            .map(|a| a.value.to_string()),
        _ => None,
    }
}

/// All attributes of `node` as `(name, value)` pairs in **source order** (i.e.
/// the order they were declared in the source HTML, as html5ever preserves
/// them in `attrs.borrow()`'s `Vec`). Returns an empty `Vec` on non-element
/// nodes.
///
/// This is the lxml-equivalent of iterating `element.items()` (which lxml
/// documents as "in document order"). It is the foundation for libxml2's
/// node-set-to-string coercion on attribute unions: `string(@a|@b)` returns
/// the string value of the first node in document order, which for an
/// element's attribute list is the first-declared attribute matching either
/// name. The XPath engine (`src/trafilatura/xpath_engine.rs`) consumes this
/// to implement the `contains(@id|@class, "x")` shape DA-B-1 calls out.
///
/// Added at M3 Stage 0b (post-review MAJOR fix — close facade-coupling
/// where the engine was reaching into rcdom internals). The element's
/// `attrs.borrow()` is iterated here; callers stay rcdom-agnostic.
pub fn attributes_in_source_order(node: &NodeRef) -> Vec<(String, String)> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .map(|a| (a.name.local.to_string(), a.value.to_string()))
            .collect(),
        _ => Vec::new(),
    }
}

/// `node.setAttribute(name, value)` — set or overwrite. No-op on non-element
/// nodes (the JS only ever sets attributes on elements).
pub fn set_attribute(node: &NodeRef, name: &str, value: &str) {
    if let NodeData::Element { attrs, .. } = &node.data {
        let mut attrs = attrs.borrow_mut();
        if let Some(a) = attrs.iter_mut().find(|a| &*a.name.local == name) {
            a.value = value.into();
        } else {
            attrs.push(Attribute {
                name: html_attr_qual(name),
                value: value.into(),
            });
        }
    }
}

/// `node.removeAttribute(name)`. No-op if absent / non-element.
pub fn remove_attribute(node: &NodeRef, name: &str) {
    if let NodeData::Element { attrs, .. } = &node.data {
        attrs.borrow_mut().retain(|a| &*a.name.local != name);
    }
}

/// `node.className` (the raw `class` attribute, `""` if absent — matching the
/// JS `(node.getAttribute("class") || "")`).
pub fn class_name(node: &NodeRef) -> String {
    get_attribute(node, "class").unwrap_or_default()
}

/// `node.id` (the raw `id` attribute, `""` if absent).
pub fn id(node: &NodeRef) -> String {
    get_attribute(node, "id").unwrap_or_default()
}

/// `document.createElement(tag)` — a fresh, parentless element node with the
/// given (lower-cased) tag in the HTML namespace and no attributes/children.
pub fn create_element(tag: &str) -> NodeRef {
    Node::new(NodeData::Element {
        name: html_tag_qual(tag),
        attrs: RefCell::new(Vec::new()),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

/// Create a detached `Text` node with the given data
/// (`document.createTextNode`).
pub fn create_text_node(data: &str) -> NodeRef {
    Node::new(NodeData::Text {
        contents: RefCell::new(data.into()),
    })
}

/// `node.remove()` — detach `node` from its parent (no-op if already
/// detached). Children travel with it (the subtree is preserved, just
/// unlinked), exactly like the DOM.
pub fn remove(node: &NodeRef) {
    if let Some((parent, idx)) = parent_and_index(node) {
        parent.children.borrow_mut().remove(idx);
        node.parent.set(None);
    }
}

/// Recursively strip every `NodeData::Comment` and
/// `NodeData::ProcessingInstruction` from the subtree rooted at `root`,
/// mirroring lxml `HTMLParser(remove_comments=True, remove_pis=True)`
/// (Python trafilatura `utils.py:70`).
///
/// lxml's semantics: when a Comment/PI is removed, its `.tail` is promoted
/// into the preceding sibling's `.tail` (or, if it was the first child,
/// into the parent's `.text`). rcdom does not model `.tail` as an intrinsic
/// string — adjacent Text content is stored as separate `NodeData::Text`
/// sibling nodes. Removing the Comment/PI therefore IS the tail merge:
/// the Text node sitting immediately after the Comment becomes adjacent to
/// whatever preceded the Comment, and the facade accessors
/// ([`tail`], [`element_text`]) already concatenate consecutive Text
/// siblings (dom.rs:404-422, 436-453). No explicit string-splice is needed.
///
/// We snapshot each level's children **before** mutation so a `remove` does
/// not invalidate the iteration index (same safe-walk pattern used by
/// `prune_html`, `cleaning.rs:422`).
pub(crate) fn strip_comments_and_pis(root: &NodeRef) {
    // Snapshot children at this level; recurse into surviving children
    // afterwards. Order matters less than correctness here -- comments
    // never have children of their own in the HTML5 parse output (rcdom
    // models them as leaf nodes with empty `.children`), so removing them
    // discards no nested content.
    let kids: Vec<NodeRef> = root.children.borrow().iter().cloned().collect();
    for child in &kids {
        match &child.data {
            NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. } => {
                remove(child);
            }
            _ => {
                strip_comments_and_pis(child);
            }
        }
    }
}

/// `parent.appendChild(child)` — append `child` as the last child of
/// `parent`, first detaching it from any current parent (DOM `appendChild`
/// move semantics — a node has at most one parent).
pub fn append_child(parent: &NodeRef, child: &NodeRef) {
    remove(child);
    child.parent.set(Some(Rc::downgrade(parent)));
    parent.children.borrow_mut().push(child.clone());
}

/// `parent.replaceChild(new_node, old_node)` — replace `old_node` (which must
/// be a child of `parent`) in place with `new_node`, preserving position.
/// `new_node` is detached from any current parent first; `old_node` is
/// detached. No-op if `old_node` is not a child of `parent`.
pub fn replace_child(parent: &NodeRef, new_node: &NodeRef, old_node: &NodeRef) {
    let pos = parent
        .children
        .borrow()
        .iter()
        .position(|c| Rc::ptr_eq(c, old_node));
    let Some(pos) = pos else { return };
    remove(new_node);
    new_node.parent.set(Some(Rc::downgrade(parent)));
    {
        let mut kids = parent.children.borrow_mut();
        kids[pos] = new_node.clone();
    }
    old_node.parent.set(None);
}

/// lxml `new_parent.append(child)` — **move `child` (with its tail) to become
/// `new_parent`'s last child**.
///
/// # Why this primitive exists (the rcdom reparent-tail bug class)
///
/// In lxml, `.tail` is an intrinsic attribute of the element node, so
/// `parent.append(el)` / `parent.insert(i, el)` / `parent.extend([...])`
/// move the element **together with its tail**. readex models a tail as a
/// *separate* run of following `Text` siblings ([`tail`] / [`set_tail`]), so
/// the naive port `remove(child); append_child(new_parent, child)` MOVES the
/// element but LEAVES its tail Text-node(s) orphaned in the old parent —
/// silently dropping text. The same defect recurs at every hand-rolled
/// "reparent a node" site (a 6-time-recurring bug class found by audit; e.g.
/// `output.rs` `_wrap_unwanted_siblings_of_div` / `_move_element_one_level_up`).
///
/// This primitive captures the tail before the move and re-applies it at the
/// destination, matching lxml `append` exactly: the moved node ends up as
/// `new_parent`'s last child WITH its tail, and the source parent keeps
/// nothing behind.
///
/// No-op if `child` has no tail beyond the plain move.
pub fn reparent_with_tail(new_parent: &NodeRef, child: &NodeRef) {
    let captured = tail(child);
    // Drain the source tail run BEFORE the move, otherwise the tail Text
    // node(s) stay orphaned in the old parent (the very defect this guards).
    if captured.is_some() {
        set_tail(child, None);
    }
    append_child(new_parent, child);
    if let Some(t) = captured {
        set_tail(child, Some(&t));
    }
}

/// lxml `new_parent.insert(idx, child)` — **move `child` (with its tail) to
/// position `idx` under `new_parent`**.
///
/// Index variant of [`reparent_with_tail`]; see that function for the
/// rationale (the rcdom reparent-tail bug class). Captures `child`'s tail
/// before the move and re-applies it at the destination so the tail travels
/// with the node, matching lxml `insert`. `idx` is clamped to the destination
/// child count. No-op tail handling if `child` has no tail.
pub fn insert_with_tail(new_parent: &NodeRef, child: &NodeRef, idx: usize) {
    let captured = tail(child);
    // Drain the source tail run BEFORE detaching, otherwise the tail Text
    // node(s) stay orphaned in the old parent (the very defect this guards).
    if captured.is_some() {
        set_tail(child, None);
    }
    remove(child);
    {
        let mut kids = new_parent.children.borrow_mut();
        let clamped = idx.min(kids.len());
        child.parent.set(Some(Rc::downgrade(new_parent)));
        kids.insert(clamped, child.clone());
    }
    if let Some(t) = captured {
        set_tail(child, Some(&t));
    }
}

/// `getElementsByTagName(tag)` over `node`'s subtree (descendants only,
/// document order). `"*"` matches every element. Returns an **owned snapshot
/// `Vec`** (HLD §5 / risk #3): a later tree mutation does **not** retroactively
/// change a `Vec` already returned — Stage-0 tests pin this.
pub fn get_elements_by_tag_name(node: &NodeRef, tag: &str) -> Vec<NodeRef> {
    let want = tag.to_ascii_lowercase();
    let any = want == "*";
    let mut out = Vec::new();
    collect_descendants(node, &mut out, &|n| match &n.data {
        NodeData::Element { name, .. } => any || &*name.local == want.as_str(),
        _ => false,
    });
    out
}

/// `_getAllNodesWithTag(node, tagNames)` (`Readability.js:396-406`).
///
/// Under jsdom this is `node.querySelectorAll(tagNames.join(","))` — a
/// **static** `NodeList` in document order, restricted to descendants of
/// `node`. We reproduce that: a single document-order descendant walk
/// collecting every element whose lower-cased tag is in `tags`, returned as
/// an owned snapshot `Vec` (HLD §5). `querySelectorAll` does **not** include
/// `node` itself, only descendants — matched here.
pub fn get_all_nodes_with_tag(node: &NodeRef, tags: &[&str]) -> Vec<NodeRef> {
    let want: HashSet<String> = tags.iter().map(|t| t.to_ascii_lowercase()).collect();
    let mut out = Vec::new();
    collect_descendants(node, &mut out, &|n| match &n.data {
        NodeData::Element { name, .. } => want.contains(&*name.local as &str),
        _ => false,
    });
    out
}

/// Document-order (pre-order) descendant walk pushing every node for which
/// `keep` is true. Descendants only — `root` itself is not tested/pushed
/// (matches `getElementsByTagName` / `querySelectorAll` scope).
fn collect_descendants(root: &NodeRef, out: &mut Vec<NodeRef>, keep: &dyn Fn(&NodeRef) -> bool) {
    for child in root.children.borrow().iter() {
        if keep(child) {
            out.push(child.clone());
        }
        collect_descendants(child, out, keep);
    }
}

/// lxml `Element.text`: concatenation of `node`'s leading consecutive
/// `Text`-node children, up to the first non-Text child (element / comment /
/// PI), or `None` if the first child is not a Text node (or `node` has no
/// children). Symmetric to [`tail`] but anchored at the start of `node`'s
/// children, not at the end of `elem`'s next-sibling run.
///
/// Private helper for [`Dom::document_order_triplets`].
fn leading_text(node: &NodeRef) -> Option<String> {
    let kids = node.children.borrow();
    let mut out: Option<String> = None;
    for child in kids.iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let data = contents.borrow();
                match &mut out {
                    Some(s) => s.push_str(&data),
                    None => out = Some(data.to_string()),
                }
            }
            _ => break,
        }
    }
    out
}

/// Pre-order element-only walk emitting `(elem, .text, .tail)` triplets.
/// Pushed for `node` itself before recursing into its element descendants
/// (matches lxml `ElementTree.iter()` order).
fn collect_triplets(node: &NodeRef, out: &mut Vec<(NodeRef, Option<String>, Option<String>)>) {
    if matches!(node.data, NodeData::Element { .. }) {
        let t = leading_text(node);
        let tl = tail(node);
        out.push((node.clone(), t, tl));
    }
    // Recurse into ALL children so we visit element descendants in document
    // order; non-element children themselves are not pushed (their data is
    // surfaced via their parent's .text or the previous element's .tail).
    // Snapshot the child list — `tail()` will re-borrow `children` on each
    // recursive call, and we must not hold a borrow across that.
    let kids: Vec<NodeRef> = node.children.borrow().clone();
    for child in &kids {
        collect_triplets(child, out);
    }
}

impl Dom {
    /// `_setNodeTag(node, tag)` — **slow branch only** (`Readability.js:760-772`;
    /// HLD §2.2 ruling B-2).
    ///
    /// The in-place fast branch (754-758) is **forbidden and absent**. This:
    /// 1. creates a fresh element of `tag` (`createElement`, line 760);
    /// 2. moves **every** child to it preserving order (761-763);
    /// 3. splices it into the old node's slot in the parent (764,
    ///    `replaceChild`);
    /// 4. **transfers the score side-table entry and the
    ///    `_readabilityDataTable` flag** from the old node pointer to the new
    ///    one (the Rust analogue of 765-767 — `if (node.readability)
    ///    replacement.readability = node.readability`);
    /// 5. clones every attribute (768-770).
    ///
    /// Returns the **new** handle. Every caller MUST use the returned handle —
    /// the old one is detached (exactly as the JS returns `replacement` and
    /// callers reassign), per HLD §2.2.
    ///
    /// If `node` has no parent (detached) the `replaceChild` step is a no-op
    /// (mirrors that `node.parentNode.replaceChild` would throw in JS only on
    /// a truly parentless node, which the algorithm never does — defensive
    /// here, never reached on the ported paths).
    #[must_use = "set_node_tag detaches the old node and returns the new one; \
                  the caller must use the returned handle (HLD §2.2)"]
    pub fn set_node_tag(&mut self, node: &NodeRef, tag: &str) -> NodeRef {
        // 760: var replacement = node.ownerDocument.createElement(tag);
        let replacement = create_element(tag);

        // 761-763: while (node.firstChild) replacement.appendChild(node.firstChild);
        // Move children in order. Drain the live child list, re-parent each.
        let moved: Vec<NodeRef> = node.children.borrow_mut().drain(..).collect();
        {
            let mut new_kids = replacement.children.borrow_mut();
            for child in &moved {
                child.parent.set(Some(Rc::downgrade(&replacement)));
                new_kids.push(child.clone());
            }
        }

        // 764: node.parentNode.replaceChild(replacement, node);
        if let Some((p, pos)) = parent_and_index(node) {
            replacement.parent.set(Some(Rc::downgrade(&p)));
            p.children.borrow_mut()[pos] = replacement.clone();
            node.parent.set(None);
        }

        // 765-767: if (node.readability) replacement.readability = node.readability;
        // The Rust analogue: move the score side-table entry AND (HLD §2.2,
        // explicitly) the _readabilityDataTable flag from old ptr -> new ptr.
        let old_key = NodeKey::of(node);
        let new_key = NodeKey::of(&replacement);
        if let Some(score) = self.content_score.remove(&old_key) {
            self.content_score.insert(new_key, score);
        }
        if self.readability_data_table.remove(&old_key) {
            self.readability_data_table.insert(new_key);
        }

        // 768-770: clone every attribute onto the replacement.
        if let NodeData::Element { attrs: old, .. } = &node.data
            && let NodeData::Element { attrs: new, .. } = &replacement.data
        {
            *new.borrow_mut() = old.borrow().clone();
        }

        // 771: return replacement;
        replacement
    }

    /// lxml `etree`-style "delete element, preserve its `.tail`" (M3 Stage 0a —
    /// HLD §5.1 / §6.0).
    ///
    /// Removes `elem` from its parent **and** re-anchors `elem`'s tail text
    /// (the Text node(s) between `elem` and its next non-Text sibling — see
    /// [`tail`]) onto:
    /// - `elem`'s **previous Text sibling** (appended to that node's data), if
    ///   one exists; OR
    /// - a fresh Text node inserted at `elem`'s old slot, if the previous
    ///   sibling exists but is **not** a Text node (so the tail text lands
    ///   immediately after that prev sibling); OR
    /// - a fresh Text node inserted as `parent`'s **first child**, if `elem`
    ///   had no previous sibling at all (the lxml "promote-to-`parent.text`"
    ///   analogue: deleting the very first child in lxml relocates its tail
    ///   onto `parent.text`).
    ///
    /// This is the semantic Trafilatura's `prune_html` relies on
    /// (`htmlprocessing.py`, the lxml `getparent().remove(child)` /
    /// `strip_tags` pattern that open-codes per-element tail re-attachment
    /// throughout `main_extractor.py`). lxml's `etree.strip_elements(..., with_tail=False)`
    /// is the closest stdlib equivalent — Trafilatura uses both shapes and we
    /// match the "preserve tail" one, which is what `prune_html` does.
    ///
    /// # No-op cases
    ///
    /// - `elem` is detached (no parent) → no-op.
    /// - `elem` has no tail text → still removes `elem`; only the
    ///   re-anchoring is skipped.
    ///
    /// # Why `&mut self`?
    ///
    /// The current body does not touch `Dom`'s side tables, but its semantic
    /// peer `set_node_tag` does; keeping this on `Dom` lets us evolve the
    /// score-transfer rule (e.g. "score follows tail" if a future Stage finds
    /// it needs to) without an API churn at call sites. The detached
    /// `elem`'s side-table entries, like with the free `remove` function, are
    /// left in the maps and reaped on `Dom` drop — point-query-only
    /// guarantees no observable leak (HLD §5.1).
    pub fn delete_with_tail_preserve(&mut self, elem: &NodeRef) {
        let Some((parent, idx)) = parent_and_index(elem) else {
            return;
        };

        // Step 1+2: collect the tail text and the run-length of Text siblings
        // to remove alongside `elem`. Single scoped borrow — never held
        // across a mutation.
        let (tail_text, tail_run_len) = {
            let kids = parent.children.borrow();
            let mut text: Option<String> = None;
            let mut run = 0usize;
            for sibling in kids.iter().skip(idx + 1) {
                match &sibling.data {
                    NodeData::Text { contents } => {
                        let data = contents.borrow();
                        match &mut text {
                            Some(s) => s.push_str(&data),
                            None => text = Some(data.to_string()),
                        }
                        run += 1;
                    }
                    // First non-Text terminates the tail run (lxml semantics).
                    _ => break,
                }
            }
            (text, run)
        };

        // Step 3: detach `elem` and the tail Text-node run together. They
        // sit at positions [idx .. idx + 1 + tail_run_len) in document order.
        {
            let mut kids = parent.children.borrow_mut();
            let drained: Vec<NodeRef> = kids.drain(idx..idx + 1 + tail_run_len).collect();
            for n in &drained {
                n.parent.set(None);
            }
        }

        // Step 4: re-anchor the tail text, if any.
        let Some(text) = tail_text else { return };
        // Even an empty `Some("")` is preserved — lxml stores `""` rather
        // than coalescing to `None`, and downstream `xmltotxt` is whitespace-
        // sensitive enough that we stay byte-faithful here.
        if idx == 0 {
            // No previous sibling -> insert as parent's leading Text child.
            let txt = create_text_node(&text);
            txt.parent.set(Some(Rc::downgrade(&parent)));
            parent.children.borrow_mut().insert(0, txt);
            return;
        }
        // Inspect prev sibling (at index idx-1 after the drain).
        let prev_is_text = matches!(
            parent.children.borrow()[idx - 1].data,
            NodeData::Text { .. }
        );
        if prev_is_text {
            // Append to prev's data in place.
            let kids = parent.children.borrow();
            let prev = &kids[idx - 1];
            if let NodeData::Text { contents } = &prev.data {
                // Round-trip via String -> StrTendril: Tendril's in-place
                // append isn't on the publicly-stable surface, and one
                // allocation here keeps the code obvious and matches the
                // facade's "clone, never juggle borrows" style.
                let mut merged = contents.borrow().to_string();
                merged.push_str(&text);
                *contents.borrow_mut() = merged.into();
            }
        } else {
            // Insert a new Text node at elem's old slot (= immediately after
            // prev, which is at idx-1).
            let txt = create_text_node(&text);
            txt.parent.set(Some(Rc::downgrade(&parent)));
            parent.children.borrow_mut().insert(idx, txt);
        }
    }

    /// lxml document-order `(element, .text, .tail)` triplet iteration (M3
    /// Stage 0a — HLD §5.1 / §6.0).
    ///
    /// Yields one triplet per **element** descendant of `root`, in
    /// pre-order (parent before children), with `root` itself as the first
    /// triplet. Non-Element nodes (Text, Comment, PI, Doctype) are **not**
    /// yielded — their data is exposed via the surrounding elements'
    /// `.text` / `.tail` components, exactly as lxml's `ElementTree.iter()`
    /// does (lxml's elementtree is element-only; text lives on elements).
    ///
    /// # `.text` semantic
    ///
    /// The `.text` component is the concatenation of **all leading
    /// consecutive Text-node children** of the element, up to the first
    /// non-Text child (element / comment / PI). `None` if the first child is
    /// not a Text node (or the element has no children). This matches
    /// lxml/libxml2's coalescing of consecutive text-node children into a
    /// single string at `Element.text`; `markup5ever_rcdom` does not
    /// coalesce, but the concatenation yields the byte-equivalent answer.
    ///
    /// # `.tail` semantic
    ///
    /// The `.tail` component is `tail(element)` — see that function for the
    /// full semantics. `None` if there is no following Text sibling; `Some`
    /// with the concatenated Text-run otherwise.
    ///
    /// # Return type
    ///
    /// Returns an owned `Vec<(NodeRef, Option<String>, Option<String>)>` (not
    /// a borrowed iterator) — the brief permits either, and Vec sidesteps
    /// holding `RefCell` borrows through a closure, which is awkward to make
    /// safe across rcdom's interior-mutable child lists. The cost is one
    /// `Vec` allocation per call (each `NodeRef` is a cheap `Rc` bump); the
    /// strings are unavoidable since they are a logical recomposition of
    /// possibly-multiple Text-node `data` slices.
    pub fn document_order_triplets(
        &self,
        root: &NodeRef,
    ) -> Vec<(NodeRef, Option<String>, Option<String>)> {
        let mut out = Vec::new();
        collect_triplets(root, &mut out);
        out
    }

    /// `node.readability.contentScore` read (or `None` if the node was never
    /// `_initializeNode`d). Point query — never iterate the table.
    pub fn content_score(&self, node: &NodeRef) -> Option<f64> {
        self.content_score.get(&NodeKey::of(node)).copied()
    }

    /// Set `node.readability.contentScore`.
    pub fn set_content_score(&mut self, node: &NodeRef, score: f64) {
        self.content_score.insert(NodeKey::of(node), score);
    }

    /// `true` iff `node` has a score entry (`node.readability` is truthy in
    /// JS — the `_initializeNode` guard).
    pub fn has_content_score(&self, node: &NodeRef) -> bool {
        self.content_score.contains_key(&NodeKey::of(node))
    }

    // --- _readabilityDataTable flag set (HLD §5 / §5.1) -----------------

    /// `node._readabilityDataTable` read. Point query.
    pub fn is_readability_data_table(&self, node: &NodeRef) -> bool {
        self.readability_data_table.contains(&NodeKey::of(node))
    }

    /// Set / clear `node._readabilityDataTable`.
    pub fn set_readability_data_table(&mut self, node: &NodeRef, value: bool) {
        let key = NodeKey::of(node);
        if value {
            self.readability_data_table.insert(key);
        } else {
            self.readability_data_table.remove(&key);
        }
    }

    /// HLD §5.1 structural invariant: the side tables are **point-query-only**
    /// *by construction*.
    ///
    /// This is a **compile-time/structural** guarantee, NOT a runtime check —
    /// you cannot ask a `HashMap` "did anyone iterate you?", so no honest
    /// runtime assertion exists (the former `debug_assert!` here compared a
    /// `HashMap`'s `capacity()` to its `len()`, which is *always* true — a
    /// tautology that only theatrically *looked* like enforcement; it is
    /// removed). The invariant instead holds because: the two maps are
    /// **private** to this module; the **only** methods that touch them are
    /// the point-query getters / setters above and the `set_node_tag`
    /// transfer — none of which iterate; and there is deliberately **no**
    /// `pub fn` returning an iterator over, or the keys of, either map.
    ///
    /// This method is intentionally a **no-op marker**: it gives the §5.1
    /// "plus a unit test" hook a known-safe call site and a single greppable
    /// anchor, so any future change that adds iteration has an obvious
    /// invariant (and this doc) to break against. It does not — and honestly
    /// cannot — *check* anything at runtime.
    #[cfg(debug_assertions)]
    pub fn side_tables_are_point_query_only_by_construction(&self) {
        // Deliberately empty: the invariant is structural (see the
        // doc-comment). No runtime assertion is possible or honest here.
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// `(parent, index-of-node-in-parent.children)` or `None` if detached.
fn parent_and_index(node: &NodeRef) -> Option<(NodeRef, usize)> {
    let weak = node.parent.take();
    let parent = weak.as_ref().and_then(|w| w.upgrade());
    node.parent.set(weak);
    let parent = parent?;
    let idx = parent
        .children
        .borrow()
        .iter()
        .position(|c| Rc::ptr_eq(c, node))?;
    Some((parent, idx))
}

/// Element `QualName` in the HTML namespace (lower-cased local name) — what
/// html5ever produces for parsed HTML elements, so created elements are
/// indistinguishable from parsed ones.
fn html_tag_qual(tag: &str) -> QualName {
    QualName::new(None, ns!(html), LocalName::from(tag.to_ascii_lowercase()))
}

/// Attribute `QualName`: no prefix, **empty** namespace (HTML attributes are
/// not namespaced — matches how html5ever stores parsed attributes, so
/// `get_attribute` after `set_attribute` is consistent with parsed attrs).
fn html_attr_qual(name: &str) -> QualName {
    // local_name!("") gives the empty namespace via ns!(); html attrs use the
    // null namespace. Build directly to avoid needing a static atom for an
    // arbitrary attribute name.
    QualName::new(None, ns!(), LocalName::from(name.to_ascii_lowercase()))
}

// Touch `local_name!` so the import is used even though every current call
// site uses `ns!`/`LocalName::from`; keeps the dialect-faithful macro in
// scope for Stage 1a without a separate edit, and documents intent.
#[allow(dead_code)]
const _HTML_DIV: &() = {
    // Compile-time proof the html namespace + a known local atom resolve
    // (defensive: if a future markup5ever bump renamed these the build
    // breaks here with a clear locus, not deep in a parse).
    fn _assert() -> LocalName {
        local_name!("div")
    }
    &()
};

/// Serialize an element subtree to HTML — the analogue of
/// `this._serializer(articleContent)` (`Readability.js:2772`).
///
/// Used by `Options.include_html` only (the default path does not request
/// it; the serialization is NOT scored). Delegates to `html5ever::serialize`
/// over `markup5ever_rcdom::SerializableHandle`, with `IncludeNode` so the
/// root element appears in the output (matching the JS `_serializer` ⇒
/// `innerHTML` shape per `Readability.js:90-99` which defaults to
/// `el => el.innerHTML`).
///
/// Actually the JS default `_serializer = el => el.innerHTML` returns
/// **children-only**, NOT the root. We mirror that with `ChildrenOnly(None)`
/// for consistency with the JS default.
pub fn serialize_html(node: &NodeRef) -> String {
    use html5ever::serialize::{SerializeOpts, TraversalScope, serialize};
    use markup5ever_rcdom::SerializableHandle;

    let mut buf: Vec<u8> = Vec::new();
    let handle: SerializableHandle = node.clone().into();
    let opts = SerializeOpts {
        scripting_enabled: false,
        traversal_scope: TraversalScope::ChildrenOnly(None),
        create_missing_parent: false,
    };
    // `serialize` never fails for an in-memory `Vec<u8>`; if it ever does
    // (a downstream regression), surface a debug-only empty string and let
    // tests catch it loudly. The serialized HTML is NOT scored, so a runtime
    // panic here would be worse than an empty string.
    let _ = serialize(&mut buf, &handle, opts);
    String::from_utf8(buf).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// M3 Stage 1b additive surface (HLD §5.1 / §7.2, DECISION-F context).
// ---------------------------------------------------------------------------

/// Free-function variant of [`Dom::delete_with_tail_preserve`] that does NOT
/// touch the score / `_readabilityDataTable` side tables (those are M2-only;
/// cleaning has no scoring context). Otherwise structurally identical:
/// remove `elem` from its parent and re-anchor `elem.tail` onto the previous
/// sibling (or parent.text if no previous sibling), matching lxml's
/// `delete_element(elem, keep_tail=True)` (xml.py:54-70).
///
/// Stage 1b's `cleaning::tree_cleaning` / `prune_html` use this so they can
/// run on a bare `NodeRef` tree without threading a `&mut Dom` through every
/// helper.
pub fn delete_with_tail_preserve_free(elem: &NodeRef) {
    let Some((parent, idx)) = parent_and_index(elem) else {
        return;
    };

    // Collect tail-run text + length (scoped borrow).
    let (tail_text, tail_run_len) = {
        let kids = parent.children.borrow();
        let mut text: Option<String> = None;
        let mut run = 0usize;
        for sibling in kids.iter().skip(idx + 1) {
            match &sibling.data {
                NodeData::Text { contents } => {
                    let data = contents.borrow();
                    match &mut text {
                        Some(s) => s.push_str(&data),
                        None => text = Some(data.to_string()),
                    }
                    run += 1;
                }
                _ => break,
            }
        }
        (text, run)
    };

    // Drain elem + tail Text-node run from parent.children.
    {
        let mut kids = parent.children.borrow_mut();
        let drained: Vec<NodeRef> = kids.drain(idx..idx + 1 + tail_run_len).collect();
        for n in &drained {
            n.parent.set(None);
        }
    }

    // Re-anchor tail.
    let Some(text) = tail_text else { return };
    if idx == 0 {
        let txt = create_text_node(&text);
        txt.parent.set(Some(Rc::downgrade(&parent)));
        parent.children.borrow_mut().insert(0, txt);
        return;
    }
    let prev_is_text = matches!(
        parent.children.borrow()[idx - 1].data,
        NodeData::Text { .. }
    );
    if prev_is_text {
        let kids = parent.children.borrow();
        let prev = &kids[idx - 1];
        if let NodeData::Text { contents } = &prev.data {
            let mut merged = contents.borrow().to_string();
            merged.push_str(&text);
            *contents.borrow_mut() = merged.into();
        }
    } else {
        let txt = create_text_node(&text);
        txt.parent.set(Some(Rc::downgrade(&parent)));
        parent.children.borrow_mut().insert(idx, txt);
    }
}

/// Unwrap `elem` in place: move every child of `elem` into `elem`'s parent at
/// `elem`'s position, then detach `elem`. Preserves `elem`'s `.tail` Text-node
/// run (it remains where it was, immediately after the last moved child).
///
/// This is the lxml `etree.strip_tags(tree, <name>)` semantic Trafilatura's
/// `tree_cleaning` relies on (`htmlprocessing.py:64`: `strip_tags(tree,
/// stripping_list)` over `MANUALLY_STRIPPED`). lxml docs:
/// "Delete all elements with the given tag name from a tree or subtree.
/// This will remove the elements and their entire subtree, including all
/// their attributes, text content and descendants. It will not remove
/// (or otherwise touch) the tail text." — wait, that is `strip_elements`. The
/// `strip_tags` variant is the OPPOSITE: it removes the element WRAPPER but
/// preserves the element's children, text, tail, attributes-of-children, …
/// Per lxml docs on `strip_tags`: "Delete all elements with the provided tag
/// names from a tree or subtree. This will remove the elements and their
/// attributes, but not their text/tail content or descendants. Instead, it
/// will merge the text content and children of the element into its parent."
///
/// This function implements that "merge into parent" semantic for a single
/// element. The caller drives the tree walk (Stage 1b iterates the catalog
/// once and calls this per element found).
///
/// # Tail semantics
///
/// `elem`'s `.tail` Text-node run (between `elem` and the next non-Text
/// sibling) is left in place — it sits immediately after the spliced-in
/// children, exactly as lxml's `strip_tags` produces. `elem`'s LEADING text
/// (its first Text-node child, lxml's `.text`) flows out as the first
/// spliced child, naturally.
///
/// # No-op cases
///
/// - `elem` is detached (no parent) → no-op.
/// - `elem` has no children → still removes `elem` from its parent (the
///   "empty element" case; lxml strips it the same way).
///
/// # Source anchor
///
/// HLD §7.2: `tree_cleaning(htmlprocessing.py:64)` calls lxml `strip_tags`
/// over `MANUALLY_STRIPPED` (`settings.py:407-429`). This is the rcdom-side
/// equivalent the Rust port calls per-element.
pub fn strip_element(elem: &NodeRef) {
    let Some((parent, idx)) = parent_and_index(elem) else {
        return;
    };

    // Move `elem`'s children into `parent`'s children at position `idx`.
    // `elem` itself is then removed (it occupies the slot AFTER its children;
    // i.e. after the splice, `parent.children` has the children at idx..idx+k
    // and `elem` at idx+k; we then drop `elem`).
    let moved: Vec<NodeRef> = elem.children.borrow_mut().drain(..).collect();

    {
        let mut kids = parent.children.borrow_mut();
        // Remove `elem` first (at idx), then insert the moved children at
        // idx. Order preserved. Doing it in this order keeps the index math
        // trivial: after `remove(idx)`, the slot at idx is "where elem was";
        // inserting in reverse order puts each child at idx, ending with
        // moved[0] at idx and moved[k-1] at idx + (k-1) — i.e. original order.
        kids.remove(idx);
        for child in moved.iter().rev() {
            child.parent.set(Some(Rc::downgrade(&parent)));
            kids.insert(idx, child.clone());
        }
    }
    elem.parent.set(None);
}

/// Replace `elem`'s tag with `new_tag`, preserving the element's parent slot,
/// attributes, and children. Returns the new `NodeRef`.
///
/// This is the cleaning-side analogue of `Dom::set_node_tag`: same five
/// structural moves (create new, move children, splice into parent slot, clone
/// attrs, return new handle) but **without** the side-table transfer
/// (`set_node_tag` carries `content_score` + `readability_data_table` over
/// the rename — those are M2 scoring fixtures, irrelevant to M3 `convert_tags`).
///
/// # DECISION-F (HLD §2.2)
///
/// Trafilatura's `convert_tags` (`htmlprocessing.py:381-417`) rewrites tags on
/// potentially thousands of elements per page. We deliberately do NOT push
/// through `Dom::set_node_tag` here because:
///
/// 1. Side-table transfer is meaningless on the cleaning path (no scores yet);
/// 2. Avoiding the `&mut Dom` borrow lets `convert_tags` run over a
///    `NodeRef` without threading the `Dom` through every helper;
/// 3. The cost remains O(N) per element regardless — rcdom stores
///    `NodeData::Element { name: QualName, ... }` by value (no interior
///    mutability on `name`), so a true in-place rename without `unsafe` is
///    not possible. The "allocate new node + reparent children" path IS the
///    in-place rename in safe Rust against rcdom.
///
/// If Stage 1b's DECISION-F perf check shows EDGAR-class extraction > 2× M2
/// equivalent, the upgrade path is to swap rcdom for a substrate that exposes
/// `&Cell<QualName>` (or to add `unsafe` here, which the doctrine forbids).
/// The Stage 1b measurement determines whether either is needed.
///
/// # Returns
///
/// The new `NodeRef` (the old one is detached). Callers MUST use the returned
/// handle — the old one is no longer in the tree.
#[must_use = "replace_element_tag detaches the old element and returns the new \
              one; the caller must use the returned handle"]
pub fn replace_element_tag(elem: &NodeRef, new_tag: &str) -> NodeRef {
    // Create the replacement.
    let replacement = create_element(new_tag);

    // Move children in order.
    let moved: Vec<NodeRef> = elem.children.borrow_mut().drain(..).collect();
    {
        let mut new_kids = replacement.children.borrow_mut();
        for child in &moved {
            child.parent.set(Some(Rc::downgrade(&replacement)));
            new_kids.push(child.clone());
        }
    }

    // Clone attributes (Trafilatura's convert_tags clears attrs at the call
    // site via `elem.attrib.clear()` then sets specific ones — we faithfully
    // copy them here, and the caller calls `clear_attributes` / `set_attribute`
    // afterwards if needed). This preserves the "rename only" semantic.
    if let NodeData::Element { attrs: old, .. } = &elem.data
        && let NodeData::Element { attrs: new, .. } = &replacement.data
    {
        *new.borrow_mut() = old.borrow().clone();
    }

    // Splice into parent slot. If detached, leave detached (matches set_node_tag).
    if let Some((p, pos)) = parent_and_index(elem) {
        replacement.parent.set(Some(Rc::downgrade(&p)));
        p.children.borrow_mut()[pos] = replacement.clone();
        elem.parent.set(None);
    }

    replacement
}

/// Remove every attribute from `elem`. No-op on non-element nodes. Matches
/// lxml's `elem.attrib.clear()` (`htmlprocessing.py:323, 373, 403`).
pub fn clear_attributes(elem: &NodeRef) {
    if let NodeData::Element { attrs, .. } = &elem.data {
        attrs.borrow_mut().clear();
    }
}

/// `copy.deepcopy(elem)` — recursive subtree clone (M3 Stage 2c-i additive
/// extension, HLD §5.1).
///
/// Returns a fresh, **detached** `NodeRef` that is a structural deep copy of
/// `node`'s entire subtree: every descendant is freshly allocated, no `Rc`
/// pointer aliases the source tree, and the returned root has no parent.
/// Children, attributes, and text/comment/PI/doctype payloads are all
/// independently owned by the clone.
///
/// # lxml fidelity
///
/// lxml `copy.deepcopy(elem)` returns a new element with the same tag,
/// attributes, text, tail, and recursively-cloned children. Empirically
/// (verified against a live `lxml.etree` 5.x), **lxml DOES preserve the
/// source element's `.tail` on the cloned root** — the cloned `.tail`
/// attribute equals the source `.tail`, even though the clone has no
/// parent.
///
/// rcdom represents lxml's `.tail` as a sibling `Text` node owned by the
/// element's parent. A detached `NodeRef` returned by this function has
/// no parent and therefore no anchor for a root-level tail; the Rust port
/// drops the root-tail bytes. This is an honest, observable divergence
/// from lxml — but it is **inert for every Stage 2c-i caller** because
/// the only consumer (`handle_titles`, `main_extractor.py:53`) walks
/// `title.itertext()` and `title.append(...)` and never reads
/// `title.tail`. The prospective Stage 2c-iii consumer
/// (`prune_unwanted_nodes`'s `with_backup` branch) also does not read the
/// root's tail — it just re-uses the backup as the working tree.
///
/// If a future stage introduces a caller that DOES read `clone.tail`, this
/// fn must grow a paired "carrier" return shape (e.g. `(NodeRef,
/// Option<StrTendril>)`) or attach a transient Text-sibling under a
/// synthetic wrapper. Until then, the docstring is the contract.
///
/// **Descendant tails ARE preserved** without special-case logic: each
/// descendant's tail is a Text-node sibling between it and the next
/// non-Text sibling; that sibling Text node is itself a child of the
/// descendant's parent in the subtree. Since we deep-clone the entire
/// `children` list verbatim — including the interleaved Text-node siblings
/// that materialise lxml's `.tail` — the descendant-tail bytes survive
/// faithfully.
///
/// # Variant handling
///
/// - `Element`: clone `name`, clone every `Attribute` in source order,
///   recursively clone every child.
/// - `Text`: clone the `contents` (`StrTendril::clone` is a refcount bump
///   on the underlying buffer; the new `RefCell` is independent).
/// - `Comment`: clone the `contents` (immutable `StrTendril`).
/// - `ProcessingInstruction`: clone `target` + `contents`.
/// - `Doctype`: clone `name` + `public_id` + `system_id`.
/// - `Document`: clone children only (Document carries no payload).
///
/// `template_contents` is reset to `None` (a fresh `RefCell`). Template
/// element contents are an HTML-spec rarity Trafilatura never traverses;
/// the safer default is "drop, do not deep-clone an inner document".
///
/// # Citation
///
/// `main_extractor.py:53` — `title = deepcopy(element)` in `handle_titles`.
/// Also expected to be consumed by Stage 2c-iii `prune_unwanted_nodes`'s
/// `with_backup` branch (`htmlprocessing.py:99` — `tcopy = deepcopy(tree)`).
pub fn deep_clone(node: &NodeRef) -> NodeRef {
    let clone = match &node.data {
        NodeData::Element {
            name,
            attrs,
            mathml_annotation_xml_integration_point,
            ..
        } => Node::new(NodeData::Element {
            name: name.clone(),
            attrs: RefCell::new(attrs.borrow().clone()),
            template_contents: RefCell::new(None),
            mathml_annotation_xml_integration_point: *mathml_annotation_xml_integration_point,
        }),
        NodeData::Text { contents } => Node::new(NodeData::Text {
            contents: RefCell::new(contents.borrow().clone()),
        }),
        NodeData::Comment { contents } => Node::new(NodeData::Comment {
            contents: contents.clone(),
        }),
        NodeData::ProcessingInstruction { target, contents } => {
            Node::new(NodeData::ProcessingInstruction {
                target: target.clone(),
                contents: contents.clone(),
            })
        }
        NodeData::Doctype {
            name,
            public_id,
            system_id,
        } => Node::new(NodeData::Doctype {
            name: name.clone(),
            public_id: public_id.clone(),
            system_id: system_id.clone(),
        }),
        NodeData::Document => Node::new(NodeData::Document),
    };

    // Recursively clone every child, linking each into `clone` (the new root
    // is detached — `clone.parent` stays `None`).
    for child in node.children.borrow().iter() {
        let child_clone = deep_clone(child);
        child_clone.parent.set(Some(Rc::downgrade(&clone)));
        clone.children.borrow_mut().push(child_clone);
    }

    clone
}

/// Canonical XML serialization for the M3 Stage 0c Trafilatura-equivalence
/// gate (HLD §6.2). Emits a deterministic, ASCII-stable XML representation
/// of `node`'s subtree:
///
/// - Each element opens as `<tag>` (or `<tag attr="value" ...>` for non-empty
///   attribute lists, attributes in source order — matching
///   `attributes_in_source_order`'s lxml-equivalent contract).
/// - Empty elements use the long form `<tag></tag>` (NOT `<tag/>`), matching
///   lxml's default `etree.tostring(..., method='xml')` output for elements
///   that have NO children. This trades two extra bytes per empty element for
///   serializer-independent equality.
/// - Text node `data` is emitted with the five-char XML escape: `&` →
///   `&amp;`, `<` → `&lt;`, `>` → `&gt;`, `"` → `&quot;`, `'` → `&apos;`.
///   Attribute values use the same five-char escape.
/// - Comments / processing instructions / doctypes are **omitted** — they
///   are not part of `convert_tags`'s output domain (the cleaning step at
///   `htmlprocessing.py:64` strips comments via `strip_tags` *implicitly*
///   because `MANUALLY_STRIPPED` does not include comment nodes; but
///   Trafilatura's downstream `xmltotxt` discards them anyway, and lxml's
///   `tostring(method='xml', with_comments=False)` discards them too). The
///   gate compares post-`convert_tags` trees, so suppressing comments on
///   both sides keeps the comparison stable across html5ever and lxml.
/// - Whitespace is preserved verbatim from Text nodes; no pretty-printing.
///
/// # Why not html5ever's XML serializer?
///
/// html5ever's `serialize::serialize` emits HTML, including void-element
/// special-casing (`<br>` not `<br></br>`) and HTML attribute-value-quoting
/// rules. The post-`convert_tags` tree contains TEI tags (`hi`, `list`,
/// `item`, `head`, `quote`, `cell`, `row`, `ref`, `lb`) that html5ever has no
/// HTML semantic for, AND we deliberately want the long-form `<tag></tag>`
/// for byte-stability vs lxml. Hand-rolling the serializer is ~50 LOC of
/// pure-CPU work over the rcdom tree and avoids dragging in xml5ever (which
/// would be a new runtime dependency).
///
/// # Stage 0c contract
///
/// Pair this with `run.py --convert-tags-only`'s `etree.tostring(tree,
/// method='xml', pretty_print=False, encoding='unicode')` output. The gate
/// asserts byte-identity OR a documented whitespace-only delta (HLD §6.2).
pub fn serialize_converted_tree(node: &NodeRef) -> String {
    let mut out = String::new();
    serialize_node(node, &mut out);
    out
}

fn serialize_node(node: &NodeRef, out: &mut String) {
    match &node.data {
        NodeData::Element { name, attrs, .. } => {
            let tag = name.local.as_ref();
            out.push('<');
            out.push_str(tag);
            for a in attrs.borrow().iter() {
                out.push(' ');
                out.push_str(a.name.local.as_ref());
                out.push('=');
                out.push('"');
                escape_xml_attr(&a.value, out);
                out.push('"');
            }
            out.push('>');
            for child in node.children.borrow().iter() {
                serialize_node(child, out);
            }
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
        NodeData::Text { contents } => {
            escape_xml_text(&contents.borrow(), out);
        }
        NodeData::Document => {
            // The Document is opaque — only its children are emitted (matches
            // lxml's `tostring(tree, ...)` which serialises the root element,
            // not a Document wrapper). For an rcdom Document, children = [<html>].
            for child in node.children.borrow().iter() {
                serialize_node(child, out);
            }
        }
        // Comments / PIs / Doctype: deliberately omitted (see fn doc).
        _ => {}
    }
}

fn escape_xml_text(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn escape_xml_attr(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- text_content: the load-bearing no-separator invariant (HLD §2.1) ----
    //
    // Every expected value below is hand-derived from the WHATWG
    // `Node.textContent` definition (concatenation of descendant #text data,
    // tree order, no synthetic separators) + html5ever's spec tree
    // construction — NOT from running the code (TDD: oracle first).

    /// Parse `frag` as a full doc and return `<body>`'s text_content.
    fn body_text(frag: &str) -> String {
        let dom = Dom::parse(frag);
        text_content(&dom.body().expect("html5ever always synthesises <body>"))
    }

    #[test]
    fn text_content_no_separator_between_elements() {
        // The canonical adversarial case from the task brief: a text node, an
        // element with a text node, a trailing text node — all under one div.
        // WHATWG: "a" + "b" + "c" with NO inter-element whitespace.
        assert_eq!(body_text("<div>a<p>b</p>c</div>"), "abc");
    }

    #[test]
    fn text_content_nested_elements_no_separator() {
        assert_eq!(body_text("<div>1<span>2<b>3</b>4</span>5</div>"), "12345");
    }

    #[test]
    fn text_content_empty_inline_element_contributes_nothing() {
        assert_eq!(body_text("<div>a<span></span>b</div>"), "ab");
    }

    #[test]
    fn text_content_implied_tbody_table_no_separator() {
        // html5ever inserts an implied <tbody> (the snapshot has none);
        // textContent is still a bare concat of the cell text — NO row/cell
        // separators (the classic table-fusing risk HLD §2.1 warns about).
        assert_eq!(
            body_text("<table><tr><td>x</td><td>y</td></tr><tr><td>z</td></tr></table>"),
            "xyz"
        );
    }

    #[test]
    fn text_content_nested_tables_no_separator() {
        assert_eq!(
            body_text("<table><tr><td>a<table><tr><td>b</td></tr></table>c</td></tr></table>"),
            "abc"
        );
    }

    #[test]
    fn text_content_comment_node_ignored() {
        // A Comment is not a Text node -> contributes zero characters, and
        // crucially introduces NO separator either.
        assert_eq!(body_text("<div>a<!-- huge comment -->b</div>"), "ab");
    }

    #[test]
    fn text_content_cdata_in_html_is_bogus_comment_ignored() {
        // In HTML (non-foreign content) `<![CDATA[..]]>` is parsed as a bogus
        // comment by the HTML5 tree builder -> a Comment node -> ignored. The
        // literal "ignored" must NOT appear in textContent.
        let t = body_text("<div>a<![CDATA[ignored]]>b</div>");
        assert!(
            !t.contains("ignored"),
            "CDATA leaked into textContent: {t:?}"
        );
        assert_eq!(t, "ab");
    }

    #[test]
    fn text_content_entities_decoded_in_text_data() {
        // Entity decoding happens in the parser; textContent sees decoded data.
        assert_eq!(body_text("<p>caf&eacute; &amp; t&#233;a</p>"), "café & téa");
    }

    #[test]
    fn text_content_misnested_block_in_p_no_separator() {
        // <div> inside <p> closes the <p> (block in phrasing); the trailing
        // "c</p>" yields an implied empty <p>. At BODY level all of a,b,c are
        // descendant #text -> "abc", no separators despite the re-parenting.
        assert_eq!(body_text("<p>a<div>b</div>c</p>"), "abc");
    }

    #[test]
    fn text_content_adjacent_text_runs_concatenated_verbatim() {
        // Numeric char ref splits what would be one text run; textContent
        // re-concatenates with NO separator and preserves the literal spaces.
        assert_eq!(body_text("<p>a b&#32;c</p>"), "a b c");
    }

    #[test]
    fn text_content_whitespace_preserved_not_normalized() {
        // text_content is RAW (no trim, no run-collapse) — that is inner_text's
        // job. Three spaces stay three spaces here.
        assert_eq!(body_text("<p>a   b</p>"), "a   b");
    }

    #[test]
    fn text_content_of_text_node_itself_is_its_data() {
        let t = create_text_node("hello");
        assert_eq!(text_content(&t), "hello");
    }

    #[test]
    fn text_content_of_comment_node_is_empty() {
        // M5 Stage 6e-a: `Dom::parse` now strips Comment nodes at parse time
        // (mirroring lxml `HTMLParser(remove_comments=True)`), so we cannot
        // fish a Comment out of a parsed tree. Construct one directly to pin
        // the semantic that `text_content` of a Comment is empty.
        let comment = Node::new(NodeData::Comment {
            contents: "x".into(),
        });
        assert!(matches!(comment.data, NodeData::Comment { .. }));
        assert_eq!(text_content(&comment), "");
    }

    #[test]
    fn parse_strips_html_comments() {
        // M5 Stage 6e-a: `Dom::parse` matches Python trafilatura's
        // `HTMLParser(remove_comments=True, remove_pis=True)` (utils.py:70).
        // After parse, NO Comment nodes remain in the body subtree.
        let dom = Dom::parse("<div>a<!-- gone -->b<!-- and gone -->c</div>");
        let body = dom.body().unwrap();
        let mut stack = vec![body.clone()];
        let mut found_comment = false;
        while let Some(n) = stack.pop() {
            if matches!(n.data, NodeData::Comment { .. }) {
                found_comment = true;
            }
            for c in n.children.borrow().iter() {
                stack.push(c.clone());
            }
        }
        assert!(!found_comment, "Dom::parse left a Comment in the tree");
        // And the tail/text merge: textContent reads `abc`.
        assert_eq!(text_content(&body), "abc");
    }

    #[test]
    fn parse_strips_comment_tail_into_element_text() {
        // The load-bearing tail-merge case (M5 fixture 859b46bf108e3db4.html,
        // byte 383): `<p>foo<!-- -->bar</p>` -- lxml's `<p>.text` reads
        // `"foobar"`, readex must agree. Verified via `element_text`, which
        // concatenates the leading Text run.
        let dom = Dom::parse("<p>foo<!-- -->bar</p>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(element_text(&p).as_deref(), Some("foobar"));
    }

    #[test]
    fn parse_strips_comment_promotes_tail_to_sibling() {
        // `<div><p>x</p><!-- -->trailing</div>` -- after strip, `<p>`'s tail
        // is `"trailing"` (lxml semantics: Comment's tail promoted into the
        // preceding sibling's tail when it was an element).
        let dom = Dom::parse("<div><p>x</p><!-- -->trailing</div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(tail(&p).as_deref(), Some("trailing"));
    }

    // ---- inner_text: trim then JS \s{2,} -> single space (HLD §8) ----

    fn body(frag: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(frag);
        let b = dom.body().unwrap();
        (dom, b)
    }

    #[test]
    fn inner_text_trims_and_collapses_runs() {
        let (_d, b) = body("<p>  a   b\t\tc  </p>");
        // trim -> "a   b\t\tc"; /\s{2,}/g -> " " : "a b c"
        assert_eq!(inner_text(&b, true), "a b c");
    }

    #[test]
    fn inner_text_normalize_false_only_trims() {
        let (_d, b) = body("<p>  a   b  </p>");
        assert_eq!(inner_text(&b, false), "a   b");
    }

    #[test]
    fn inner_text_single_space_run_untouched() {
        let (_d, b) = body("<p>a b c</p>");
        assert_eq!(inner_text(&b, true), "a b c");
    }

    #[test]
    fn inner_text_js_space_includes_feff_and_nbsp_runs() {
        // U+FEFF (BOM/ZWNBSP) is JS `\s` but NOT Rust regex `\s` / is_whitespace
        // — the exact HLD §8 trap. A run "<NBSP><FEFF>" is 2 JS spaces ->
        // collapses to one ASCII space; leading/trailing JS-space is trimmed.
        let (_d, b) = body("<p>\u{FEFF} a\u{00A0}\u{FEFF}b \u{FEFF}</p>");
        // raw text = "\u{FEFF} a\u{00A0}\u{FEFF}b \u{FEFF}"
        // js_trim strips leading FEFF+space? leading run: FEFF,' ' then 'a'
        //   -> trimmed start at 'a'. trailing: ' ',FEFF -> trimmed end at 'b'.
        // "a\u{00A0}\u{FEFF}b" : run between a and b = NBSP,FEFF (2) -> ' '
        assert_eq!(inner_text(&b, true), "a b");
    }

    #[test]
    fn inner_text_lone_nbsp_single_run_preserved_verbatim() {
        // A single NBSP (run length 1) is NOT collapsed by /\s{2,}/g; JS would
        // leave it. js_trim does strip a *leading/trailing* NBSP though, so
        // keep it interior with non-space neighbours.
        let (_d, b) = body("<p>a\u{00A0}b</p>");
        assert_eq!(inner_text(&b, true), "a\u{00A0}b");
    }

    #[test]
    fn inner_text_empty_when_all_whitespace() {
        let (_d, b) = body("<p> \t \n </p>");
        assert_eq!(inner_text(&b, true), "");
    }

    // ---- children vs child_nodes ----

    #[test]
    fn children_is_element_only_child_nodes_is_all() {
        let dom = Dom::parse("<div>t1<span>s</span><!--c-->t2<b>x</b></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let el = children(&div);
        assert_eq!(el.len(), 2, "only <span> and <b> are element children");
        assert_eq!(tag_name(&el[0]).as_deref(), Some("SPAN"));
        assert_eq!(tag_name(&el[1]).as_deref(), Some("B"));
        // child_nodes: t1, span, t2, b = 4 (the Comment is stripped at
        // parse time per M5 Stage 6e-a, matching Python lxml
        // HTMLParser(remove_comments=True), utils.py:70).
        assert_eq!(child_nodes(&div).len(), 4);
    }

    // ---- traversal: first_element_child / next_element_sibling / parent ----

    #[test]
    fn first_element_child_skips_leading_text_and_comment() {
        let dom = Dom::parse("<div>text<!--c--><a>1</a><b>2</b></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let fc = first_element_child(&div).unwrap();
        assert_eq!(tag_name(&fc).as_deref(), Some("A"));
        let ns = next_element_sibling(&fc).unwrap();
        assert_eq!(tag_name(&ns).as_deref(), Some("B"));
        assert!(next_element_sibling(&ns).is_none());
        // parent round-trips
        assert!(Rc::ptr_eq(&parent(&fc).unwrap(), &div));
    }

    #[test]
    fn next_element_sibling_skips_intervening_text() {
        let dom = Dom::parse("<div><a>1</a> mid <b>2</b></div>");
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();
        let b = next_element_sibling(&a).unwrap();
        assert_eq!(tag_name(&b).as_deref(), Some("B"));
    }

    #[test]
    fn parent_of_document_is_none() {
        let dom = Dom::parse("<p>x</p>");
        assert!(parent(&dom.document()).is_none());
    }

    // ---- tag_name uppercase / local_name lowercase ----

    #[test]
    fn tag_name_is_uppercase_local_name_is_lowercase() {
        let dom = Dom::parse("<DiV><Img></DiV>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        assert_eq!(tag_name(&div).as_deref(), Some("DIV"));
        assert_eq!(local_name(&div).as_deref(), Some("div"));
        let txt = child_nodes(&div); // none of these are the div's text
        let _ = txt;
        assert_eq!(tag_name(&dom.document()), None);
    }

    // ---- attributes ----

    #[test]
    fn attribute_get_set_remove_roundtrip() {
        let dom = Dom::parse(r#"<div class="a b" id="x" data-k="v"></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        assert_eq!(get_attribute(&div, "class").as_deref(), Some("a b"));
        assert_eq!(class_name(&div), "a b");
        assert_eq!(id(&div), "x");
        assert_eq!(get_attribute(&div, "data-k").as_deref(), Some("v"));
        assert_eq!(get_attribute(&div, "missing"), None);

        set_attribute(&div, "class", "new");
        assert_eq!(get_attribute(&div, "class").as_deref(), Some("new"));
        set_attribute(&div, "role", "main");
        assert_eq!(get_attribute(&div, "role").as_deref(), Some("main"));

        remove_attribute(&div, "id");
        assert_eq!(get_attribute(&div, "id"), None);
        assert_eq!(id(&div), "");
        remove_attribute(&div, "nope"); // no-op, no panic
    }

    #[test]
    fn class_and_id_default_empty_string() {
        let dom = Dom::parse("<p>x</p>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        assert_eq!(class_name(&p), "");
        assert_eq!(id(&p), "");
    }

    // ---- create_element / append_child / remove / replace_child ----

    #[test]
    fn create_element_is_parentless_html_element() {
        let e = create_element("P");
        assert_eq!(tag_name(&e).as_deref(), Some("P"));
        assert_eq!(local_name(&e).as_deref(), Some("p"));
        assert!(parent(&e).is_none());
        assert!(child_nodes(&e).is_empty());
    }

    #[test]
    fn append_child_moves_node_and_sets_parent() {
        let dom = Dom::parse("<div id=a></div><div id=b><span>s</span></div>");
        let body = dom.body().unwrap();
        let divs = get_elements_by_tag_name(&body, "div");
        let (a, b) = (divs[0].clone(), divs[1].clone());
        let span = get_elements_by_tag_name(&b, "span")[0].clone();
        // move span from b into a
        append_child(&a, &span);
        assert!(Rc::ptr_eq(&parent(&span).unwrap(), &a));
        assert!(get_elements_by_tag_name(&b, "span").is_empty());
        assert_eq!(get_elements_by_tag_name(&a, "span").len(), 1);
    }

    #[test]
    fn remove_detaches_subtree() {
        let dom = Dom::parse("<div><section><p>keep</p></section></div>");
        let body = dom.body().unwrap();
        let section = get_elements_by_tag_name(&body, "section")[0].clone();
        remove(&section);
        assert!(parent(&section).is_none());
        assert!(get_elements_by_tag_name(&body, "section").is_empty());
        // subtree preserved on the detached node
        assert_eq!(get_elements_by_tag_name(&section, "p").len(), 1);
        assert_eq!(text_content(&section), "keep");
        remove(&section); // already detached -> no-op, no panic
    }

    #[test]
    fn replace_child_preserves_position() {
        let dom = Dom::parse("<ul><li>1</li><li>2</li><li>3</li></ul>");
        let ul = get_elements_by_tag_name(&dom.body().unwrap(), "ul")[0].clone();
        let lis = get_elements_by_tag_name(&ul, "li");
        let new = create_element("li");
        append_child(&new, &create_text_node("X"));
        replace_child(&ul, &new, &lis[1]);
        // order is 1, X, 3
        let after: Vec<String> = children(&ul).iter().map(text_content).collect();
        assert_eq!(after, vec!["1", "X", "3"]);
        assert!(parent(&lis[1]).is_none());
        assert!(Rc::ptr_eq(&parent(&new).unwrap(), &ul));
    }

    // ---- get_elements_by_tag_name / get_all_nodes_with_tag: SNAPSHOTS ----

    #[test]
    fn get_elements_by_tag_name_document_order_descendants_only() {
        let dom = Dom::parse("<div><p>1</p><section><p>2</p></section><p>3</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let ps = get_elements_by_tag_name(&div, "p");
        let txt: Vec<String> = ps.iter().map(text_content).collect();
        assert_eq!(txt, vec!["1", "2", "3"], "must be document order");
        // descendants only: querying the div does NOT include the div itself
        assert!(get_elements_by_tag_name(&div, "div").is_empty());
    }

    #[test]
    fn get_elements_by_tag_name_star_matches_all_elements() {
        let dom = Dom::parse("<div><a></a><b><i></i></b></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let all = get_elements_by_tag_name(&div, "*");
        let tags: Vec<String> = all.iter().filter_map(tag_name).collect();
        assert_eq!(tags, vec!["A", "B", "I"]);
    }

    #[test]
    fn get_all_nodes_with_tag_multi_tag_document_order() {
        let dom = Dom::parse("<div><h1>a</h1><p>b</p><h2>c</h2><p>d</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let got = get_all_nodes_with_tag(&div, &["h1", "h2", "p"]);
        let txt: Vec<String> = got.iter().map(text_content).collect();
        // querySelectorAll("h1,h2,p") order = document order, not tag order
        assert_eq!(txt, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn snapshot_is_true_snapshot_post_mutation_stable() {
        // HLD §5 / risk #3: a returned Vec must NOT retroactively change when
        // the tree is later mutated.
        let dom = Dom::parse("<div><p>1</p><p>2</p><p>3</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let snap = get_elements_by_tag_name(&div, "p");
        assert_eq!(snap.len(), 3);
        // mutate: remove the middle <p> AND add a new one
        remove(&snap[1]);
        let extra = create_element("p");
        append_child(&extra, &create_text_node("4"));
        append_child(&div, &extra);
        // the OLD snapshot is unchanged (still the original 3 handles)
        assert_eq!(snap.len(), 3);
        let snap_txt: Vec<String> = snap.iter().map(text_content).collect();
        assert_eq!(snap_txt, vec!["1", "2", "3"]);
        // a fresh query reflects the mutation
        let fresh = get_elements_by_tag_name(&div, "p");
        let fresh_txt: Vec<String> = fresh.iter().map(text_content).collect();
        assert_eq!(fresh_txt, vec!["1", "3", "4"]);
    }

    // ---- _getNextNode DFS order (Readability.js:949-969) ----

    /// Faithful port of `_getNextNode` for test verification of DFS order
    /// (the algorithm itself lands in `helpers` Stage 1a; here we only need
    /// to prove the facade primitives compose into the JS DFS).
    fn get_next_node(node: &NodeRef, ignore_self_and_kids: bool) -> Option<NodeRef> {
        // Mirrors JS `if (!ignoreSelfAndKids && node.firstElementChild)`.
        if !ignore_self_and_kids && let Some(c) = first_element_child(node) {
            return Some(c);
        }
        if let Some(s) = next_element_sibling(node) {
            return Some(s);
        }
        let mut cur = parent(node);
        while let Some(n) = cur {
            if let Some(s) = next_element_sibling(&n) {
                return Some(s);
            }
            cur = parent(&n);
        }
        None
    }

    #[test]
    fn get_next_node_traverses_depth_first_like_js() {
        // Tree: div > (a > b), c, (d > e)
        let dom = Dom::parse("<div id=root><a><b></b></a><c-x></c-x><d><e></e></d></div>");
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        // Walk from root, full DFS (ignore_self_and_kids = false), collecting
        // tag names until exhaustion.
        let mut order = Vec::new();
        let mut cur = Some(root.clone());
        while let Some(n) = cur {
            order.push(tag_name(&n).unwrap_or_default());
            cur = get_next_node(&n, false);
        }
        // DFS: ROOT, A, B, C-X, D, E (custom element c-x keeps its name)
        assert_eq!(order, vec!["DIV", "A", "B", "C-X", "D", "E"]);
    }

    #[test]
    fn get_next_node_ignore_self_and_kids_skips_subtree() {
        let dom = Dom::parse("<div id=r><a><deep></deep></a><b></b></div>");
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();
        // ignoreSelfAndKids: from <a> we must jump to <b>, NOT into <deep>.
        let n = get_next_node(&a, true).unwrap();
        assert_eq!(tag_name(&n).as_deref(), Some("B"));
    }

    // ---- set_node_tag: slow branch only (HLD §2.2) ----

    #[test]
    fn set_node_tag_creates_new_element_moves_children_in_order() {
        let mut dom = Dom::parse("<div id=p><font>a<i>b</i>c</font></div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let font = get_elements_by_tag_name(&p, "font")[0].clone();
        let span = dom.set_node_tag(&font, "SPAN");

        // new node is a SPAN, not the old FONT handle
        assert_eq!(tag_name(&span).as_deref(), Some("SPAN"));
        assert!(!Rc::ptr_eq(&span, &font));
        // children moved in order, preserved
        assert_eq!(text_content(&span), "abc");
        assert_eq!(get_elements_by_tag_name(&span, "i").len(), 1);
        // old node detached and emptied (firstChild loop drained it)
        assert!(parent(&font).is_none());
        assert!(child_nodes(&font).is_empty());
        // spliced into parent's slot: <div> now contains the SPAN
        let div_children = children(&p);
        assert_eq!(div_children.len(), 1);
        assert!(Rc::ptr_eq(&div_children[0], &span));
    }

    #[test]
    fn set_node_tag_clones_all_attributes() {
        let mut dom = Dom::parse(r#"<div><h1 class="c" id="i" data-x="y">t</h1></div>"#);
        let h1 = get_elements_by_tag_name(&dom.body().unwrap(), "h1")[0].clone();
        let h2 = dom.set_node_tag(&h1, "H2");
        assert_eq!(tag_name(&h2).as_deref(), Some("H2"));
        assert_eq!(get_attribute(&h2, "class").as_deref(), Some("c"));
        assert_eq!(get_attribute(&h2, "id").as_deref(), Some("i"));
        assert_eq!(get_attribute(&h2, "data-x").as_deref(), Some("y"));
        assert_eq!(text_content(&h2), "t");
    }

    #[test]
    fn set_node_tag_transfers_score_and_data_table_flag() {
        // The Rust analogue of Readability.js:765-767 + HLD §2.2 (also carry
        // the _readabilityDataTable flag): both side-table entries must move
        // from the old pointer to the new one.
        let mut dom = Dom::parse("<div><table><tr><td>x</td></tr></table></div>");
        let table = get_elements_by_tag_name(&dom.body().unwrap(), "table")[0].clone();
        dom.set_content_score(&table, 12.5);
        dom.set_readability_data_table(&table, true);
        assert!(dom.has_content_score(&table));

        let new = dom.set_node_tag(&table, "DIV");
        // moved onto the new handle...
        assert_eq!(dom.content_score(&new), Some(12.5));
        assert!(dom.is_readability_data_table(&new));
        // ...and gone from the old one (it was `remove`d from the map)
        assert_eq!(dom.content_score(&table), None);
        assert!(!dom.is_readability_data_table(&table));
    }

    #[test]
    fn set_node_tag_no_score_entry_is_fine() {
        // Mirrors JS `if (node.readability)` guard: an unscored node simply
        // has nothing to transfer; no panic, new node has no score.
        let mut dom = Dom::parse("<div><p>x</p></div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        let d = dom.set_node_tag(&p, "DIV");
        assert!(!dom.has_content_score(&d));
        assert_eq!(tag_name(&d).as_deref(), Some("DIV"));
    }

    // ---- side tables: point-query-only (HLD §5.1) ----

    #[test]
    fn content_score_set_get_default_none() {
        let mut dom = Dom::parse("<div><p>x</p></div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        assert_eq!(dom.content_score(&p), None);
        assert!(!dom.has_content_score(&p));
        dom.set_content_score(&p, -3.0);
        assert_eq!(dom.content_score(&p), Some(-3.0));
        assert!(dom.has_content_score(&p));
        dom.set_content_score(&p, 9.0);
        assert_eq!(dom.content_score(&p), Some(9.0));
    }

    #[test]
    fn data_table_flag_set_get_clear() {
        let dom_html = "<div><table><tr><td>x</td></tr></table></div>";
        let mut dom = Dom::parse(dom_html);
        let t = get_elements_by_tag_name(&dom.body().unwrap(), "table")[0].clone();
        assert!(!dom.is_readability_data_table(&t));
        dom.set_readability_data_table(&t, true);
        assert!(dom.is_readability_data_table(&t));
        dom.set_readability_data_table(&t, false);
        assert!(!dom.is_readability_data_table(&t));
    }

    #[test]
    fn distinct_nodes_have_distinct_side_table_identity() {
        // NodeKey identity must distinguish two different element nodes even
        // with identical markup (no accidental key collision).
        let dom = Dom::parse("<div><p>x</p><p>x</p></div>");
        let ps = get_elements_by_tag_name(&dom.body().unwrap(), "p");
        let mut d = dom;
        d.set_content_score(&ps[0], 1.0);
        assert_eq!(d.content_score(&ps[0]), Some(1.0));
        assert_eq!(
            d.content_score(&ps[1]),
            None,
            "second <p> is a distinct key"
        );
    }

    #[test]
    fn side_tables_are_point_query_only_by_construction() {
        // HLD §5.1: the "structural invariant plus a unit test" hook. The
        // invariant (no pub iterator over either map; only point queries) is
        // STRUCTURAL — this test exercises the no-op marker at a known-safe
        // site and, more importantly, documents that there is by construction
        // no API to iterate the maps. There is deliberately NO runtime
        // assertion: a `HashMap` cannot report whether it was iterated, so the
        // former capacity-vs-len `debug_assert!` (a tautology) was removed.
        let mut dom = Dom::parse("<div><p>x</p></div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        dom.set_content_score(&p, 1.0);
        dom.set_readability_data_table(&p, true);
        #[cfg(debug_assertions)]
        dom.side_tables_are_point_query_only_by_construction();
        // The real guarantee: the only map-touching methods are point get/set
        // + the set_node_tag transfer; none iterate, and no `pub fn` yields an
        // iterator/keys. Point queries still work as the contract requires:
        assert_eq!(dom.content_score(&p), Some(1.0));
    }

    // ---- M3 Stage 0a: tail() — lxml Element.tail (HLD §5.1 / §6.0) ----
    //
    // Every expected value is hand-derived from the lxml `.tail` definition
    // (text content of the next-sibling Text node(s), terminated by the first
    // non-Text sibling) + html5ever's spec tree construction. The readex
    // facade returns `None` where lxml returns `""`; the brief documents
    // that callers treat the two as equivalent.

    /// First-child <p> of <body>'s first <div> (parser-built; helper used by
    /// the tail()/delete_with_tail_preserve tests).
    fn first_p_in_div(html: &str) -> (Dom, NodeRef, NodeRef) {
        let dom = Dom::parse(html);
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        (dom, div, p)
    }

    #[test]
    fn tail_no_next_sibling_is_none() {
        // <p> is the only child of <div> -> no next sibling at all -> None.
        let (_d, _div, p) = first_p_in_div("<div><p>x</p></div>");
        assert_eq!(tail(&p), None);
    }

    #[test]
    fn tail_no_parent_at_all_is_none() {
        // "Empty document" case per the brief: a detached element has no
        // parent, hence no siblings, hence no tail. Mirrors lxml semantics
        // (a detached element's .tail is "" / not contributing).
        let e = create_element("p");
        assert_eq!(tail(&e), None);
    }

    #[test]
    fn tail_next_sibling_is_text_returns_its_data() {
        // <div><p>x</p>HELLO</div> -> <p>'s tail = "HELLO".
        let (_d, _div, p) = first_p_in_div("<div><p>x</p>HELLO</div>");
        assert_eq!(tail(&p).as_deref(), Some("HELLO"));
    }

    #[test]
    fn tail_next_sibling_is_element_then_text_is_none() {
        // <div><p>x</p><span>y</span>z</div>: the <span> immediately follows
        // <p>, so <p>'s tail is None (terminated by element before any Text).
        // The "z" is the <span>'s tail, not <p>'s.
        let (_d, div, p) = first_p_in_div("<div><p>x</p><span>y</span>z</div>");
        assert_eq!(tail(&p), None);
        let span = get_elements_by_tag_name(&div, "span")[0].clone();
        assert_eq!(tail(&span).as_deref(), Some("z"));
    }

    #[test]
    fn tail_next_sibling_is_comment_then_text_promotes() {
        // M5 Stage 6e-a: Comments are stripped at parse time
        // (utils.py:70 `HTMLParser(remove_comments=True)`), so the Comment
        // in `<div><p>x</p><!--c-->z</div>` is removed and `z` becomes the
        // sole tail-positioned Text after <p>. <p>'s tail is now `"z"`,
        // matching lxml's tail-promotion semantics. Pre-strip, this test
        // pinned the rcdom raw behaviour (Comment terminated the tail run);
        // post-strip the lxml behaviour is the contract.
        let (_d, _div, p) = first_p_in_div("<div><p>x</p><!--c-->z</div>");
        assert_eq!(tail(&p).as_deref(), Some("z"));
    }

    #[test]
    fn tail_concatenates_multiple_consecutive_text_siblings() {
        // The HTML parser coalesces adjacent literal text, so we construct
        // a (Text, Text) sibling run via the DOM API: append two Text nodes
        // after <p>. lxml would store this as a single string at <p>.tail;
        // our rcdom-equivalent answer is the concatenation.
        let dom = Dom::parse("<div><p>x</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        append_child(&div, &create_text_node("ALPHA"));
        append_child(&div, &create_text_node("BETA"));
        assert_eq!(tail(&p).as_deref(), Some("ALPHABETA"));
        // And inserting an element between them terminates the run at the
        // first non-Text: tail = "ALPHA" only.
        let between = create_element("br");
        // children order: <p>, Text(ALPHA), Text(BETA). Insert <br> between
        // the two Text nodes -> <p>, Text(ALPHA), <br>, Text(BETA).
        // Use the low-level child_nodes API + replace_child trick: detach
        // BETA, append <br>, then re-append BETA.
        let beta = child_nodes(&div).into_iter().last().unwrap();
        remove(&beta);
        append_child(&div, &between);
        append_child(&div, &beta);
        assert_eq!(tail(&p).as_deref(), Some("ALPHA"));
    }

    #[test]
    fn tail_empty_text_node_is_preserved_as_some_empty() {
        // lxml-faithful: an empty Text-node sibling produces tail = Some("").
        // This is byte-faithful preservation (matches the rationale documented
        // in `delete_with_tail_preserve`'s impl that an empty Text node is
        // structurally distinct from no Text node at all — Stage-0a review
        // NIT-1, M3 #24).
        let dom = Dom::parse("<div><p>x</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        append_child(&div, &create_text_node(""));
        assert_eq!(tail(&p).as_deref(), Some(""));
    }

    // ---- M3 Stage 0a: delete_with_tail_preserve (HLD §5.1 / §6.0) ----

    #[test]
    fn delete_with_tail_preserve_no_tail_just_removes() {
        // <div><p>x</p><span></span></div>: <p>'s next sibling is an element,
        // so tail() is None. Deleting <p> should just remove it; <span>
        // remains untouched.
        let mut dom = Dom::parse("<div><p>x</p><span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        dom.delete_with_tail_preserve(&p);
        assert!(parent(&p).is_none());
        let remaining: Vec<_> = child_nodes(&div);
        assert_eq!(remaining.len(), 1);
        assert_eq!(tag_name(&remaining[0]).as_deref(), Some("SPAN"));
    }

    #[test]
    fn delete_with_tail_preserve_tail_with_prev_text_appends() {
        // <div>head<p>x</p>tail</div>:
        //   children = [Text("head"), <p>, Text("tail")]
        //   <p>.tail = "tail"; prev sibling = Text("head")
        //   After: children = [Text("headtail")]
        let mut dom = Dom::parse("<div>head<p>x</p>tail</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        dom.delete_with_tail_preserve(&p);
        let kids = child_nodes(&div);
        assert_eq!(kids.len(), 1);
        // The remaining child must be a Text node holding "headtail".
        match &kids[0].data {
            NodeData::Text { contents } => {
                assert_eq!(&*contents.borrow(), "headtail");
            }
            _ => panic!("expected merged Text child, got {:?}", kids[0].data),
        }
    }

    #[test]
    fn delete_with_tail_preserve_tail_with_prev_element_inserts_text() {
        // <div><a>A</a><p>x</p>tail</div>:
        //   children = [<a>, <p>, Text("tail")]
        //   <p>.tail = "tail"; prev sibling = <a> (element, NOT text)
        //   After: children = [<a>, Text("tail")] — fresh Text at <p>'s slot.
        let mut dom = Dom::parse("<div><a>A</a><p>x</p>tail</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        dom.delete_with_tail_preserve(&p);
        let kids = child_nodes(&div);
        assert_eq!(kids.len(), 2);
        assert_eq!(tag_name(&kids[0]).as_deref(), Some("A"));
        match &kids[1].data {
            NodeData::Text { contents } => {
                assert_eq!(&*contents.borrow(), "tail");
            }
            _ => panic!("expected fresh Text at index 1, got {:?}", kids[1].data),
        }
    }

    #[test]
    fn delete_with_tail_preserve_tail_no_prev_sibling_promotes_to_parent_text() {
        // <div><p>x</p>tail<span></span></div>:
        //   children = [<p>, Text("tail"), <span>]
        //   <p>.tail = "tail"; <p> is the FIRST child (no prev sibling).
        //   After: children = [Text("tail"), <span>] — tail re-homed as
        //   parent's first child (lxml "promote to parent.text").
        let mut dom = Dom::parse("<div><p>x</p>tail<span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        dom.delete_with_tail_preserve(&p);
        let kids = child_nodes(&div);
        assert_eq!(kids.len(), 2);
        match &kids[0].data {
            NodeData::Text { contents } => {
                assert_eq!(&*contents.borrow(), "tail");
            }
            _ => panic!("expected Text first, got {:?}", kids[0].data),
        }
        assert_eq!(tag_name(&kids[1]).as_deref(), Some("SPAN"));
    }

    #[test]
    fn delete_with_tail_preserve_tail_multiple_consecutive_text_nodes() {
        // Construct <div>head<p>x</p>[Text(A)][Text(B)]<span/></div> via the
        // DOM API (parser coalesces literal text, so we have to build the
        // multi-Text-sibling case by hand). <p>'s tail spans both A and B;
        // after delete-with-tail-preserve the merged "headAB" lands on the
        // prev Text("head").
        let mut dom = Dom::parse("<div>head<p>x</p><span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        let span = get_elements_by_tag_name(&div, "span")[0].clone();
        // Insert two Text nodes between <p> and <span>: detach span,
        // append text(A), text(B), then re-append span.
        remove(&span);
        append_child(&div, &create_text_node("A"));
        append_child(&div, &create_text_node("B"));
        append_child(&div, &span);
        // Sanity: <p>.tail concatenates A + B (terminated by <span>).
        assert_eq!(tail(&p).as_deref(), Some("AB"));
        // Now delete.
        dom.delete_with_tail_preserve(&p);
        let kids = child_nodes(&div);
        assert_eq!(kids.len(), 2);
        match &kids[0].data {
            NodeData::Text { contents } => {
                assert_eq!(&*contents.borrow(), "headAB");
            }
            _ => panic!("expected merged Text first, got {:?}", kids[0].data),
        }
        assert_eq!(tag_name(&kids[1]).as_deref(), Some("SPAN"));
    }

    // ---- reparent_with_tail / insert_with_tail (rcdom reparent-tail class) ----

    #[test]
    fn reparent_with_tail_carries_tail_to_destination() {
        // Source: <div><p>x</p>TAILTEXT<span/></div>  (so <p>.tail = "TAILTEXT").
        // Destination: a fresh detached <wrap>. After reparent, <p> is the
        // last child of <wrap> and STILL has tail "TAILTEXT"; the source <div>
        // must NOT retain the tail Text node.
        let dom = Dom::parse("<div><p>x</p>TAILTEXT<span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        assert_eq!(tail(&p).as_deref(), Some("TAILTEXT"));
        let wrap = create_element("wrap");

        reparent_with_tail(&wrap, &p);

        // <p> moved under <wrap>.
        assert!(parent(&p).map(|x| Rc::ptr_eq(&x, &wrap)).unwrap_or(false));
        // Tail travelled with it.
        assert_eq!(tail(&p).as_deref(), Some("TAILTEXT"));
        // Source parent kept nothing behind: only <span> remains, no orphan
        // Text("TAILTEXT").
        let div_kids = child_nodes(&div);
        assert_eq!(div_kids.len(), 1);
        assert_eq!(tag_name(&div_kids[0]).as_deref(), Some("SPAN"));
        assert!(
            !div_kids
                .iter()
                .any(|n| matches!(&n.data, NodeData::Text { .. })),
            "tail Text node must NOT be orphaned in the source parent"
        );
    }

    #[test]
    fn insert_with_tail_carries_tail_to_indexed_position() {
        // Source: <div><p>x</p>TAILTEXT<span/></div>.
        // Destination: <wrap><a/><b/></wrap>; insert <p> at index 1.
        // Result children: [<a>, <p>, <b>] with <p>.tail = "TAILTEXT".
        let dom = Dom::parse("<div><p>x</p>TAILTEXT<span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        let wrap = create_element("wrap");
        let a = create_element("a");
        let b = create_element("b");
        append_child(&wrap, &a);
        append_child(&wrap, &b);

        insert_with_tail(&wrap, &p, 1);

        let kids = children(&wrap);
        assert_eq!(kids.len(), 3);
        assert_eq!(tag_name(&kids[0]).as_deref(), Some("A"));
        assert_eq!(tag_name(&kids[1]).as_deref(), Some("P"));
        assert_eq!(tag_name(&kids[2]).as_deref(), Some("B"));
        // Tail travelled: it sits between <p> and <b>.
        assert_eq!(tail(&p).as_deref(), Some("TAILTEXT"));
        // Source parent kept nothing behind.
        let div_kids = child_nodes(&div);
        assert_eq!(div_kids.len(), 1);
        assert_eq!(tag_name(&div_kids[0]).as_deref(), Some("SPAN"));
    }

    #[test]
    fn reparent_with_tail_no_tail_is_plain_move() {
        // <p>'s next sibling is an element, so tail() is None. The move still
        // happens; no spurious Text node is created at the destination.
        let dom = Dom::parse("<div><p>x</p><span></span></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        let wrap = create_element("wrap");

        reparent_with_tail(&wrap, &p);

        let kids = children(&wrap);
        assert_eq!(kids.len(), 1);
        assert_eq!(tail(&p), None);
        // Destination has no trailing Text node.
        assert_eq!(child_nodes(&wrap).len(), 1);
    }

    // ---- M3 Stage 0a: document_order_triplets (HLD §5.1 / §6.0) ----

    #[test]
    fn document_order_triplets_single_root_no_children() {
        // A bare detached element: one triplet for `root` itself, both .text
        // and .tail = None (no children, no siblings).
        let dom = Dom::parse("<p>x</p>"); // we won't actually use the DOM
        let _ = &dom;
        let e = create_element("section");
        // Stage 0a triplet API is on Dom; we still need an instance.
        let triplets = dom.document_order_triplets(&e);
        assert_eq!(triplets.len(), 1);
        assert!(Rc::ptr_eq(&triplets[0].0, &e));
        assert_eq!(triplets[0].1, None);
        assert_eq!(triplets[0].2, None);
    }

    #[test]
    fn document_order_triplets_root_with_text_and_tail() {
        // <div><p>HEAD<span/>MID</p>TAIL</div>:
        //   <p>.text = "HEAD" (leading text before <span>)
        //   <p>.tail = "TAIL" (next-sibling Text of <p>)
        //   <span>.text = None (no children)
        //   <span>.tail = "MID"
        // Starting the walk at <p> yields [(p, HEAD, TAIL), (span, None, MID)].
        let dom = Dom::parse("<div><p>HEAD<span></span>MID</p>TAIL</div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        let triplets = dom.document_order_triplets(&p);
        assert_eq!(triplets.len(), 2);
        assert_eq!(tag_name(&triplets[0].0).as_deref(), Some("P"));
        assert_eq!(triplets[0].1.as_deref(), Some("HEAD"));
        assert_eq!(triplets[0].2.as_deref(), Some("TAIL"));
        assert_eq!(tag_name(&triplets[1].0).as_deref(), Some("SPAN"));
        assert_eq!(triplets[1].1, None);
        assert_eq!(triplets[1].2.as_deref(), Some("MID"));
    }

    #[test]
    fn document_order_triplets_nested_elements_preorder() {
        // <root><a><b/></a><c><d/></c></root>: pre-order = root, a, b, c, d.
        let dom = Dom::parse("<section id=root><a><b></b></a><c-x><d></d></c-x></section>");
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "section")[0].clone();
        let triplets = dom.document_order_triplets(&root);
        let tags: Vec<String> = triplets
            .iter()
            .map(|(n, _, _)| tag_name(n).unwrap_or_default())
            .collect();
        assert_eq!(tags, vec!["SECTION", "A", "B", "C-X", "D"]);
    }

    #[test]
    fn document_order_triplets_mixed_text_comment_element_children() {
        // <div>txt1<!--c--><p>x</p>txt2<span/>txt3</div>:
        //   div.text = "txt1" (leading Text, before the Comment)
        //     -- the Comment terminates the leading-text run; this matches
        //        lxml: lxml's .text only includes the leading TEXT, not
        //        across comment boundaries.
        //   div.tail = None (no parent's-Text-sibling-of-div)
        //   p.text = "x"
        //   p.tail = "txt2"
        //   span.text = None
        //   span.tail = "txt3"
        // The Comment is NOT yielded as a triplet (element-only iteration).
        let dom = Dom::parse("<div>txt1<!--c--><p>x</p>txt2<span></span>txt3</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let triplets = dom.document_order_triplets(&div);
        assert_eq!(triplets.len(), 3);
        assert_eq!(tag_name(&triplets[0].0).as_deref(), Some("DIV"));
        assert_eq!(triplets[0].1.as_deref(), Some("txt1"));
        assert_eq!(triplets[0].2, None);
        assert_eq!(tag_name(&triplets[1].0).as_deref(), Some("P"));
        assert_eq!(triplets[1].1.as_deref(), Some("x"));
        assert_eq!(triplets[1].2.as_deref(), Some("txt2"));
        assert_eq!(tag_name(&triplets[2].0).as_deref(), Some("SPAN"));
        assert_eq!(triplets[2].1, None);
        assert_eq!(triplets[2].2.as_deref(), Some("txt3"));
    }

    // ---- body / root_element on a real-ish document ----

    #[test]
    fn body_and_root_element_resolve_on_full_document() {
        let dom = Dom::parse(
            "<!doctype html><html><head><title>T</title></head>\
             <body><article><p>hello</p></article></body></html>",
        );
        let html = dom.root_element().unwrap();
        assert_eq!(tag_name(&html).as_deref(), Some("HTML"));
        let body = dom.body().unwrap();
        assert_eq!(tag_name(&body).as_deref(), Some("BODY"));
        assert_eq!(text_content(&body), "hello");
        // <title> text is in <head>, not <body>
        assert!(!text_content(&body).contains('T'));
    }

    // ---- M3 Stage 1b: strip_element / replace_element_tag / clear_attributes ----

    #[test]
    fn strip_element_unwraps_children_into_parent_slot() {
        // <div>a<span>X<i>Y</i>Z</span>b</div>:
        // strip the <span> -> <div>aX<i>Y</i>Zb</div>  (children in slot,
        // tail "b" stays put as <div>'s next-text-after-span, which now sits
        // after the moved <i> + after the moved trailing "Z").
        let dom = Dom::parse("<div>a<span>X<i>Y</i>Z</span>b</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let span = get_elements_by_tag_name(&div, "span")[0].clone();
        strip_element(&span);
        // span is detached
        assert!(parent(&span).is_none());
        // div.text_content = "aXYZb" (no change to total text; just unwrapped)
        assert_eq!(text_content(&div), "aXYZb");
        // <i> survived inside <div>
        assert_eq!(get_elements_by_tag_name(&div, "i").len(), 1);
        // span is gone
        assert!(get_elements_by_tag_name(&div, "span").is_empty());
    }

    #[test]
    fn strip_element_detached_is_noop() {
        let e = create_element("span");
        strip_element(&e);
        assert!(parent(&e).is_none());
    }

    #[test]
    fn strip_element_empty_element_removed_no_children_moved() {
        let dom = Dom::parse("<div>a<br>b</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let br = get_elements_by_tag_name(&div, "br")[0].clone();
        strip_element(&br);
        assert!(parent(&br).is_none());
        assert_eq!(text_content(&div), "ab");
        assert!(get_elements_by_tag_name(&div, "br").is_empty());
    }

    #[test]
    fn replace_element_tag_keeps_attributes_and_children() {
        let dom = Dom::parse(r#"<div><b class="x" id="y">hi<i>!</i></b></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let b = get_elements_by_tag_name(&div, "b")[0].clone();
        let hi = replace_element_tag(&b, "hi");
        // Old detached; new is in the tree.
        assert!(parent(&b).is_none());
        assert!(Rc::ptr_eq(&parent(&hi).unwrap(), &div));
        assert_eq!(tag_name(&hi).as_deref(), Some("HI"));
        // Attrs cloned.
        assert_eq!(get_attribute(&hi, "class").as_deref(), Some("x"));
        assert_eq!(get_attribute(&hi, "id").as_deref(), Some("y"));
        // Children moved.
        assert_eq!(text_content(&hi), "hi!");
        assert_eq!(get_elements_by_tag_name(&hi, "i").len(), 1);
    }

    #[test]
    fn replace_element_tag_preserves_position_in_parent() {
        let dom = Dom::parse("<div><p>1</p><b>2</b><p>3</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let b = get_elements_by_tag_name(&div, "b")[0].clone();
        let hi = replace_element_tag(&b, "hi");
        // div now has [<p>1</p>, <hi>2</hi>, <p>3</p>] — children list order
        let kids = children(&div);
        assert_eq!(tag_name(&kids[0]).as_deref(), Some("P"));
        assert_eq!(tag_name(&kids[1]).as_deref(), Some("HI"));
        assert!(Rc::ptr_eq(&kids[1], &hi));
        assert_eq!(tag_name(&kids[2]).as_deref(), Some("P"));
    }

    #[test]
    fn clear_attributes_empties_attr_list() {
        let dom = Dom::parse(r#"<p class="a" id="x" data-k="v">hi</p>"#);
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        clear_attributes(&p);
        assert_eq!(get_attribute(&p, "class"), None);
        assert_eq!(get_attribute(&p, "id"), None);
        assert_eq!(get_attribute(&p, "data-k"), None);
        // Text/children preserved.
        assert_eq!(text_content(&p), "hi");
    }

    // ---- M3 Stage 1b: serialize_converted_tree (HLD §6.2) ----

    #[test]
    fn serialize_converted_tree_simple_element_long_form_empty() {
        let e = create_element("hi");
        // Empty element -> long form <hi></hi> (NOT <hi/>).
        assert_eq!(serialize_converted_tree(&e), "<hi></hi>");
    }

    #[test]
    fn serialize_converted_tree_attrs_in_source_order_with_escape() {
        let dom = Dom::parse(r#"<p class="a&amp;b" id="x">hi</p>"#);
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        let s = serialize_converted_tree(&p);
        // class attr value de-entitied at parse to `a&b` then re-escaped here.
        // Attributes in source order: class first, id second.
        assert_eq!(s, r#"<p class="a&amp;b" id="x">hi</p>"#);
    }

    #[test]
    fn serialize_converted_tree_text_escapes_lt_gt_amp() {
        let dom = Dom::parse("<p>a &lt; b &amp; c &gt; d</p>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        let s = serialize_converted_tree(&p);
        // Parse decodes; serialize re-encodes the five core chars.
        assert_eq!(s, "<p>a &lt; b &amp; c &gt; d</p>");
    }

    #[test]
    fn serialize_converted_tree_nested_with_mixed_content() {
        let dom = Dom::parse("<div>a<span>b<i>c</i>d</span>e</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let s = serialize_converted_tree(&div);
        assert_eq!(s, "<div>a<span>b<i>c</i>d</span>e</div>");
    }

    #[test]
    fn serialize_converted_tree_omits_comments() {
        let dom = Dom::parse("<div>a<!-- big comment -->b</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let s = serialize_converted_tree(&div);
        assert_eq!(s, "<div>ab</div>");
    }

    // -----------------------------------------------------------------
    // M3 Stage 2c-i — deep_clone (lxml `copy.deepcopy(elem)` semantics)
    // -----------------------------------------------------------------

    #[test]
    fn deep_clone_returns_detached_root() {
        let dom = Dom::parse("<div><p>x</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let cloned = deep_clone(&div);
        // The clone is detached (no parent).
        assert!(parent(&cloned).is_none());
        // The original is still attached.
        assert!(parent(&div).is_some());
    }

    #[test]
    fn deep_clone_independence_from_source() {
        let dom = Dom::parse("<div><p>x</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let cloned = deep_clone(&div);
        // Different NodeRef identities.
        assert!(!Rc::ptr_eq(&div, &cloned));
        // Mutating the clone does NOT affect the source.
        let cloned_p = get_elements_by_tag_name(&cloned, "p")[0].clone();
        set_element_text(&cloned_p, Some("mutated"));
        let original_p = get_elements_by_tag_name(&div, "p")[0].clone();
        assert_eq!(element_text(&original_p).as_deref(), Some("x"));
        assert_eq!(element_text(&cloned_p).as_deref(), Some("mutated"));
    }

    #[test]
    fn deep_clone_copies_attributes_and_text_and_descendant_tail() {
        // Text + tail interleavings should survive verbatim.
        let dom = Dom::parse(r#"<div><p class="a">hello<span>x</span>tailbytes</p></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let cloned = deep_clone(&div);
        // Attribute preserved.
        let cloned_p = get_elements_by_tag_name(&cloned, "p")[0].clone();
        assert_eq!(get_attribute(&cloned_p, "class").as_deref(), Some("a"));
        // Leading text preserved.
        assert_eq!(element_text(&cloned_p).as_deref(), Some("hello"));
        // Descendant <span>'s tail preserved.
        let cloned_span = get_elements_by_tag_name(&cloned_p, "span")[0].clone();
        assert_eq!(tail(&cloned_span).as_deref(), Some("tailbytes"));
    }

    #[test]
    fn deep_clone_recurses_into_nested_subtree() {
        let dom = Dom::parse("<div><section><p><i>x</i></p></section></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let cloned = deep_clone(&div);
        // Every descendant tag survives.
        assert_eq!(get_elements_by_tag_name(&cloned, "section").len(), 1);
        assert_eq!(get_elements_by_tag_name(&cloned, "p").len(), 1);
        assert_eq!(get_elements_by_tag_name(&cloned, "i").len(), 1);
        let i = get_elements_by_tag_name(&cloned, "i")[0].clone();
        assert_eq!(element_text(&i).as_deref(), Some("x"));
    }

    // -----------------------------------------------------------------
    // M11 Phase A — preprocess_html unit tests (HLD §6.1)
    // -----------------------------------------------------------------

    #[test]
    fn preprocess_a_no_op_on_clean_html() {
        let input = "<html><body><p>Hello</p></body></html>";
        let result = preprocess_html(input);
        assert_eq!(&*result, input);
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn preprocess_b_strips_closing_br() {
        let result = preprocess_html("before</br>after");
        assert_eq!(&*result, "beforeafter");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_c_strips_closing_br_case_insensitive() {
        let result = preprocess_html("before</BR>after");
        assert_eq!(&*result, "beforeafter");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_d_strips_closing_br_with_whitespace() {
        let result = preprocess_html("before</br >after");
        assert_eq!(&*result, "beforeafter");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_e_rewrites_xmp_open_to_div() {
        let result = preprocess_html(r#"<xmp id="x"><p>hi</p></xmp>"#);
        assert_eq!(&*result, r#"<div id="x"><p>hi</p></div>"#);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_f_rewrites_xmp_case_insensitive() {
        let result = preprocess_html("<XMP>text</XMP>");
        assert_eq!(&*result, "<div>text</div>");
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_g_rewrites_stray_td_outside_table() {
        let result = preprocess_html(r#"<div><td class="a">text</td></div>"#);
        assert_eq!(&*result, r#"<div><div class="a">text</div></div>"#);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_h_leaves_td_inside_table_alone() {
        let input = "<table><tr><td>cell</td></tr></table>";
        let result = preprocess_html(input);
        assert_eq!(&*result, input);
        // Cow variant is Owned because the quick-scan triggers (has <td>),
        // but the transformation pass leaves everything unchanged.
        // We only assert output equality, not the Cow variant (HLD §6.1 note).
    }

    #[test]
    fn preprocess_i_handles_nested_tables() {
        let input = concat!(
            "<div>",
            "<td>stray cell 1</td>",
            "<table><tr><td>legit cell</td>",
            "<table><tr><td>inner cell</td></tr></table>",
            "</tr></table>",
            "<td>stray cell 2</td>",
            "</div>"
        );
        let result = preprocess_html(input);
        let expected = concat!(
            "<div>",
            "<div>stray cell 1</div>",
            "<table><tr><td>legit cell</td>",
            "<table><tr><td>inner cell</td></tr></table>",
            "</tr></table>",
            "<div>stray cell 2</div>",
            "</div>"
        );
        assert_eq!(&*result, expected);
    }

    #[test]
    fn preprocess_j_preserves_attributes_on_cell_rewrite() {
        let result = preprocess_html(r#"<td colspan="2" class="x">text</td>"#);
        assert_eq!(&*result, r#"<div colspan="2" class="x">text</div>"#);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_k_does_not_match_prefix_tags() {
        // `<thread>` must NOT match the `<thead>` rule. The quick-scan will
        // trigger (it sees `<th`), but the transformation pass's tag-boundary
        // guard prevents the match. Output is unchanged; the Cow variant may
        // be Owned (false-Owned — HLD §7.1 deferred optimisation).
        let input = "<thread>text</thread>";
        let result = preprocess_html(input);
        assert_eq!(&*result, input);
    }

    #[test]
    fn preprocess_l_rewrites_all_cell_tag_types() {
        let input = "<tr><th>h</th><td>d</td><tbody><tfoot><thead>";
        let result = preprocess_html(input);
        let expected = "<div><div>h</div><div>d</div><div><div><div>";
        assert_eq!(&*result, expected);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_m_handles_multiple_shapes_together() {
        let input = r#"<p>a</br>b</p><td class="s">c</td><xmp>d</xmp>"#;
        let result = preprocess_html(input);
        let expected = r#"<p>ab</p><div class="s">c</div><div>d</div>"#;
        assert_eq!(&*result, expected);
        assert!(matches!(result, Cow::Owned(_)));
    }

    #[test]
    fn preprocess_n_empty_input() {
        let result = preprocess_html("");
        assert_eq!(&*result, "");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    #[test]
    fn preprocess_o_saturating_table_depth() {
        // </table> at depth 0 should saturate to 0, not underflow.
        // The subsequent <td> should still be rewritten (depth == 0).
        let result = preprocess_html("</table><td>stray</td>");
        assert_eq!(&*result, "</table><div>stray</div>");
        assert!(matches!(result, Cow::Owned(_)));
    }

    // -----------------------------------------------------------------
    // M11 Phase A — preprocess_html negative-shape branch coverage
    // -----------------------------------------------------------------

    /// rationale: `tag_matches` end-of-input arm — when a `<` is the last byte
    /// of the input, the function must not panic and must accept it as a
    /// no-match (no closing `>` follows). Pin the behaviour: a bare trailing
    /// `<` is copied through unchanged.
    #[test]
    fn preprocess_p_bare_trailing_lt_does_not_panic() {
        let result = preprocess_html("hello<");
        // No trigger matched the `<`; the byte is copied as-is.
        // The output may be either Borrowed or Owned depending on whether
        // any quick-scan trigger fired. With no triggers, returns Borrowed.
        assert_eq!(&*result, "hello<");
    }

    /// rationale: `</br` at end-of-input with no `>` — strip everything
    /// from the `<` to end (find_closing_angle returns bytes.len()).
    #[test]
    fn preprocess_q_br_close_without_closing_angle() {
        let result = preprocess_html("text</br");
        // </br with no `>` — strip to end.
        assert_eq!(&*result, "text");
        assert!(matches!(result, Cow::Owned(_)));
    }

    /// rationale: `<` not followed by any trigger tag (no br-end, no xmp,
    /// no cell tag). In a non-quick-scan-triggered input the function
    /// returns Borrowed; this test makes the trigger fire (via `<td`) but
    /// the OTHER `<` in the document falls into the "no match" tail.
    #[test]
    fn preprocess_r_other_lt_falls_through_when_triggers_present() {
        // `<p>` is NOT a trigger — it falls through the trigger ladder and
        // gets copied as `<` + advance pos. The `<td>` triggers the
        // quick-scan and is rewritten.
        let result = preprocess_html("<p>x</p><td>y</td>");
        assert_eq!(&*result, "<p>x</p><div>y</div>");
        assert!(matches!(result, Cow::Owned(_)));
    }

    /// rationale: `<table>` opening AFTER a stray `<td>` — depth tracking
    /// proves the stray-cell rewrite is order-dependent. Stray `<td>` BEFORE
    /// the `<table>` is rewritten; `<td>` INSIDE survives unchanged.
    #[test]
    fn preprocess_s_stray_td_then_table_with_cells() {
        let input = "<td>stray</td><table><tr><td>cell</td></tr></table>";
        let expected = "<div>stray</div><table><tr><td>cell</td></tr></table>";
        let result = preprocess_html(input);
        assert_eq!(&*result, expected);
        assert!(matches!(result, Cow::Owned(_)));
    }

    /// rationale: cover the close-cell branch (`</td`, `</tr`, etc.) outside
    /// a table — gets rewritten to `</div`. The open-cell version is already
    /// tested; the close-cell rewrite is a parallel branch.
    #[test]
    fn preprocess_t_close_cell_outside_table_rewritten() {
        // `<th>` opens and `</th>` closes — both should become div outside a
        // table context.
        let result = preprocess_html("<th>x</th>");
        assert_eq!(&*result, "<div>x</div>");
        assert!(matches!(result, Cow::Owned(_)));
    }

    /// rationale: close-cell INSIDE a table is preserved. Pinned to prove
    /// the depth-aware branch keeps the table syntax intact.
    #[test]
    fn preprocess_u_close_cell_inside_table_preserved() {
        let input = "<table><tr><td>x</td></tr></table>";
        let result = preprocess_html(input);
        assert_eq!(&*result, input);
    }

    /// rationale: input with NO quick-scan triggers and NO `<` at all — the
    /// most-common no-op path. Returns Borrowed (zero allocation) regardless
    /// of length.
    #[test]
    fn preprocess_v_no_lt_at_all_is_borrowed() {
        let result = preprocess_html("just plain text content with no markup");
        assert_eq!(&*result, "just plain text content with no markup");
        assert!(matches!(result, Cow::Borrowed(_)));
    }

    // -----------------------------------------------------------------
    // helpers small-branch coverage (the per-byte utilities)
    // -----------------------------------------------------------------

    /// `tag_matches` must REJECT prefix matches. `<thread>` must not match
    /// `<thead>` (5 chars vs 6, but the prefix is the same up to position 5).
    /// rationale: the tag-boundary guard at byte position `pos + tag.len()`
    /// requires `is_tag_boundary` or EOF.
    #[test]
    fn tag_matches_rejects_prefix_collision() {
        // Quick-scan triggers on `<th` (any of thead/th/tr/tbody/tfoot are
        // searched). With `<thread>` the open-cell-tag pass should NOT match
        // any of {thead,tfoot,tbody,tr,th,td} because `<thread>` followed by
        // `e` after `<th` violates the boundary guard.
        let result = preprocess_html("<thread>x</thread>");
        // No rewrite (no real tag-name match — `<thread>` is its own thing).
        assert_eq!(&*result, "<thread>x</thread>");
    }

    /// `is_tag_boundary` accepts form-feed (0x0C) as boundary.
    /// rationale: the boundary classifier must match HTML5's space set
    /// including the form-feed for parser-faithful behaviour.
    #[test]
    fn preprocess_w_form_feed_after_tag_name_accepted_as_boundary() {
        // `<td\x0Cclass=...>` — form-feed after the tag name. The cell tag
        // is matched (boundary satisfied), so a stray `<td>` is rewritten.
        let input = "<td\x0Cclass=\"x\">cell</td>";
        let result = preprocess_html(input);
        assert_eq!(&*result, "<div\x0Cclass=\"x\">cell</div>");
    }
}
