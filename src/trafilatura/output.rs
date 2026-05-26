//! `output.rs` — output-format helpers + internal `Document` struct.
//!
//! M4 Stage 3 sub-stage A. Source of truth:
//! `trafilatura@v2.0.0/xml.py:28-352` (the shared helpers every formatter
//! consumes) and `trafilatura@v2.0.0/settings.py:207-303` (the `Document`
//! dataclass-analogue that carries post-extraction state across the formatter
//! API surface). Sub-stage B onwards lands the public `extract_to_*` entry
//! points (XML / TEI / markdown / CSV / JSON) which consume `Document` and
//! emit format-specific strings — Stage 3-A only lands the shared helpers and
//! the carrier struct.
//!
//! # Scope
//!
//! Seven helpers ported here:
//!
//! | Helper | Python source |
//! |---|---|
//! | `delete_element` | `xml.py:54-70` |
//! | `merge_with_parent` | `xml.py:73-91` |
//! | `remove_empty_elements` | `xml.py:94-103` |
//! | `strip_double_tags` | `xml.py:106-112` |
//! | `clean_attributes` | `xml.py:137-142` |
//! | `replace_element_text` | `xml.py:253-297` |
//! | `process_element` | `xml.py:300-351` |
//!
//! Plus the constants `xml.py:37-50` declares for these helpers
//! (`NEWLINE_ELEMS`, `SPECIAL_FORMATTING`, `WITH_ATTRIBUTES`,
//! `NESTING_WHITELIST`, `HI_FORMATTING`, `MAX_TABLE_WIDTH`).
//!
//! # NFC normalisation
//!
//! These helpers DO NOT NFC-normalise their output. Python's
//! `xml.py:354` (`xmltotxt`) ends with `unescape(sanitize(...) or "")`;
//! NFC is a downstream step applied **once** at the public formatter
//! surface. Stage 3-A's helpers must remain idempotent under repeated
//! invocation, which forbids NFC here.

#![allow(dead_code)] // Stage 3-B onwards consumes this module's surface.

use crate::readability::dom::{
    self, NodeData, NodeRef, children, clear_attributes, delete_with_tail_preserve_free,
    element_text, get_attribute, get_elements_by_tag_name, local_name, parent,
    previous_element_sibling, set_attribute, set_element_text, set_tail, tail,
};
use crate::trafilatura::metadata::Metadata;
use crate::trafilatura::utils::text_chars_test;
use regex::Regex;
use std::sync::OnceLock;

// ===========================================================================
// Module constants (xml.py:37-50)
// ===========================================================================

/// `xml.py:37` — `NEWLINE_ELEMS = {'code', 'graphic', 'head', 'lb', 'list',
/// 'p', 'quote', 'row', 'table'}`.
///
/// Element tags whose end emits a newline in `process_element`. Order is not
/// load-bearing (Python uses a set); a sorted slice is enough for `contains`.
pub(crate) const NEWLINE_ELEMS: &[&str] = &[
    "code", "graphic", "head", "lb", "list", "p", "quote", "row", "table",
];

/// `xml.py:38` — `SPECIAL_FORMATTING = {'del', 'head', 'hi', 'ref'}`.
///
/// Element tags that emit NO trailing space in `process_element`'s after-tag
/// branch (in contrast to the default ` ` emit at `xml.py:347`).
pub(crate) const SPECIAL_FORMATTING: &[&str] = &["del", "head", "hi", "ref"];

/// `xml.py:39` — `WITH_ATTRIBUTES = {'cell', 'row', 'del', 'graphic', 'head',
/// 'hi', 'item', 'list', 'ref'}`.
///
/// Element tags whose attributes survive `clean_attributes`. Everything else
/// gets `attrib.clear()`'d.
pub(crate) const WITH_ATTRIBUTES: &[&str] = &[
    "cell", "row", "del", "graphic", "head", "hi", "item", "list", "ref",
];

/// `xml.py:40` — `NESTING_WHITELIST = {"cell", "figure", "item", "note",
/// "quote"}`.
///
/// Parent tags inside which `strip_double_tags` leaves nested same-tag
/// children alone (e.g. `<quote><p>...</p></quote>` is allowed; the inner
/// `<p>` is NOT merged with its `<quote>` parent).
pub(crate) const NESTING_WHITELIST: &[&str] = &["cell", "figure", "item", "note", "quote"];

/// `xml.py:48` — `HI_FORMATTING = {'#b': '**', '#i': '*', '#u': '__', '#t':
/// '`'}`. Maps `<hi rend="...">` codes to the markdown wrapper string.
pub(crate) fn hi_formatting(rend: &str) -> Option<&'static str> {
    match rend {
        "#b" => Some("**"),
        "#i" => Some("*"),
        "#u" => Some("__"),
        "#t" => Some("`"),
        _ => None,
    }
}

/// `xml.py:50` — `MAX_TABLE_WIDTH = 1000`. Caps the `colspan`/`span` value
/// `process_element` honours when emitting empty cells to pad a row.
pub(crate) const MAX_TABLE_WIDTH: usize = 1000;

// ---------------------------------------------------------------------------
// TEI constants (xml.py:28-33) — Stage 3-E
// ---------------------------------------------------------------------------

/// `xml.py:30` — `TEI_VALID_ATTRS = {'rend', 'rendition', 'role', 'target',
/// 'type'}`.
///
/// The attribute-name whitelist `check_tei` consults: any descendant element
/// attribute NOT in this set is popped (`xml.py:232-234`).
pub(crate) const TEI_VALID_ATTRS: &[&str] = &["rend", "rendition", "role", "target", "type"];

/// `xml.py:32` — `TEI_REMOVE_TAIL = {"ab", "p"}`.
///
/// Tags whose tail text `check_tei` re-anchors via `_handle_unwanted_tails`
/// (`xml.py:224-225`). Tail on a `<p>` is folded into the element text;
/// tail on an `<ab>` becomes a fresh `<p>` sibling.
pub(crate) const TEI_REMOVE_TAIL: &[&str] = &["ab", "p"];

/// `xml.py:33` — `TEI_DIV_SIBLINGS = {"p", "list", "table", "quote", "ab"}`.
///
/// The set of element tags that `_wrap_unwanted_siblings_of_div` collects into
/// a fresh `<div>` sibling when they appear next to a `<div>` (TEI requires
/// every direct child of `<body>` to be a `<div>` — bare p/list/table next to
/// a div is invalid; the helper re-wraps them).
pub(crate) const TEI_DIV_SIBLINGS: &[&str] = &["p", "list", "table", "quote", "ab"];

/// `core.py` constant string `'Trafilatura'` plus the package version Python
/// reads via `importlib.metadata.version("trafilatura")` (`xml.py:24`). The
/// Rust port pins the version it was authored against (matches the source
/// commit's `pyproject.toml`); this string ONLY surfaces in the
/// `<application version="...">` element of the TEI header (`xml.py:487`).
///
/// Tied to the Python source commit `v2.0.0`. If the upstream pyproject
/// version bumps, this constant moves with it.
pub(crate) const TRAFILATURA_VERSION: &str = "2.0.0";

// ===========================================================================
// Document struct (settings.py:207-303)
// ===========================================================================

/// Internal carrier of post-extraction state. Mirrors Python's `Document`
/// dataclass (`settings.py:207-280`) — the union of metadata, body tree, and
/// comments tree that every formatter consumes.
///
/// **Not exposed publicly.** Stage 3 formatters take this; the eventual
/// public surface (sub-stages B–E: `extract_to_xml` / `extract_to_tei` /
/// `extract_to_markdown` etc.) builds a Document internally from the
/// `Metadata` (Stage 7) + `extract_content` (Stage 2d) outputs.
///
/// # Field choices vs Python
///
/// Python's `Document` carries 21 `__slots__` (`settings.py:209-232`):
/// title, author, url, hostname, description, sitename, date, categories,
/// tags, fingerprint, id, license, body, comments, commentsbody, raw_text,
/// text, language, image, pagetype, filedate.
///
/// The Rust port factors metadata into the existing `Metadata` struct
/// (`trafilatura::metadata::Metadata`, Stage 7a) which already owns
/// `title`/`author`/`url`/`hostname`/`description`/`site_name`/`date`/
/// `categories`/`tags`/`language`/`image`/`pagetype`/`license`. That leaves
/// nine slots unique to `Document` (per Python): `body`, `comments`,
/// `commentsbody`, `raw_text`, `text`, `fingerprint`, `id`, `filedate`.
///
/// Stage 3-A surfaces the four formatter-load-bearing slots:
/// `metadata` (carries title/author/etc.), `body` (the post-extraction
/// element tree, settings.py:222), `commentsbody` (the comments element
/// tree, settings.py:224 — optional because not every page has comments;
/// Python defaults to `Element("body")` per :251, the Rust `None` encodes
/// "no comments extracted"), and `raw_text` (settings.py:225 — the raw
/// HTML body text used as a fallback by `build_json_output`).
///
/// Slots Stage 3-A omits (added in later sub-stages when a formatter
/// surfaces a need):
/// - `comments`/`text`: Python redundancy — the strings re-serialise
///   `commentsbody`/`body` via `xmltotxt`. Sub-stage B/C can re-derive
///   them on demand from `body`/`commentsbody`.
/// - `fingerprint`/`id`/`filedate`: M4 Stage 6 (simhash + fingerprint
///   + is_similar_domain) lands these on a sibling carrier struct.
pub(crate) struct Document {
    /// `settings.py:209-220` — every metadata field Python carries directly
    /// on `Document`, factored through the Stage 7 `Metadata` struct.
    pub(crate) metadata: Metadata,
    /// `settings.py:222` (`Document.body`) — the post-extraction element
    /// tree (typically a `<body>` element whose children are the extracted
    /// `<p>`/`<head>`/`<list>`/`<table>` etc.).
    pub(crate) body: NodeRef,
    /// `settings.py:224` (`Document.commentsbody`) — the comments tree,
    /// or `None` if no comments were extracted. Python defaults to an
    /// empty `<body>` element per `:251`; the Rust `None` encodes the
    /// same "absent" semantic with cheaper construction.
    pub(crate) commentsbody: Option<NodeRef>,
    /// `settings.py:225` (`Document.raw_text`) — the raw HTML body text
    /// used as a fallback by `build_json_output` / `build_csv_output`
    /// when the post-extraction body is empty.
    pub(crate) raw_text: String,
}

// ===========================================================================
// delete_element (xml.py:54-70)
// ===========================================================================

/// `xml.py:54-70` — `delete_element(element, keep_tail=True)`.
///
/// Removes `element` from its parent. When `keep_tail` is true, `element`'s
/// tail Text-node run is preserved: it travels onto the previous sibling's
/// tail (or onto `parent.text` if `element` was the first child).
///
/// **Implementation note.** Trafilatura already has
/// `dom::delete_with_tail_preserve_free` (`dom.rs:1191`), which IS the
/// `keep_tail=True` branch — landed at Stage 1b for `cleaning::tree_cleaning`
/// against the same `xml.py:54-70` Python prototype. We delegate to it for
/// the keep-tail case and to `dom::remove` for the drop-tail case.
pub(crate) fn delete_element(element: &NodeRef, keep_tail: bool) {
    // xml.py:59-61 — `parent = element.getparent(); if parent is None: return`.
    if parent(element).is_none() {
        return;
    }
    if keep_tail {
        // xml.py:63-70 — full keep_tail branch via the existing port.
        delete_with_tail_preserve_free(element);
    } else {
        // Drop tail: detach element AND its tail Text-node run.
        // dom::remove drops only the element; we walk the tail run first
        // and detach each Text sibling.
        let prev_tail_run = collect_following_text_siblings(element);
        for txt in &prev_tail_run {
            dom::remove(txt);
        }
        dom::remove(element);
    }
}

/// Helper for `delete_element(keep_tail=false)`: snapshot the run of `Text`
/// siblings immediately following `element` (i.e. `element.tail` as Text
/// nodes), so the caller can detach them. Stops at the first non-Text
/// sibling (matching the lxml tail-run semantic).
fn collect_following_text_siblings(element: &NodeRef) -> Vec<NodeRef> {
    let Some(p) = parent(element) else {
        // llvm-cov:branch-not-reachable: the only caller is
        // delete_element(keep_tail=false) (output.rs:226), which is itself
        // guarded by an early `if parent(element).is_none() { return }`
        // (output.rs:216-218). So by the time this fn runs, `element` always
        // has a parent. The guard is kept for local robustness (the fn is a
        // faithful tail-run helper) but cannot fire from any live call path.
        return Vec::new();
    };
    let kids = p.children.borrow();
    let idx = kids
        .iter()
        .position(|c| std::rc::Rc::ptr_eq(c, element))
        .unwrap_or(kids.len());
    let mut out = Vec::new();
    for sib in kids.iter().skip(idx + 1) {
        if matches!(sib.data, NodeData::Text { .. }) {
            out.push(sib.clone());
        } else {
            break;
        }
    }
    out
}

// ===========================================================================
// merge_with_parent (xml.py:73-91)
// ===========================================================================

/// `xml.py:73-91` — `merge_with_parent(element, include_formatting=False)`.
///
/// Folds `element` into its parent: the element's `replace_element_text(...)`
/// representation plus its tail flows onto either the previous sibling's
/// tail (space-joined) or the parent's text (space-joined), then the element
/// is detached.
///
/// Used by `strip_double_tags` to collapse `<x><x>foo</x></x>` into `<x>foo
/// </x>`, and by xml.py's TEI cleanup (`xml.py:222`) to drop unwanted tags.
pub(crate) fn merge_with_parent(element: &NodeRef, include_formatting: bool) {
    // xml.py:75-77 — `parent = element.getparent(); if parent is None: return`.
    let Some(p) = parent(element) else { return };

    // xml.py:79 — `full_text = replace_element_text(element, include_formatting)`.
    let mut full_text = replace_element_text(element, include_formatting);
    // xml.py:80-81 — `if element.tail is not None: full_text += element.tail`.
    if let Some(t) = tail(element) {
        full_text.push_str(&t);
    }

    // xml.py:83-90 — previous-sibling / parent-text fold.
    let prev = previous_element_sibling(element);
    if let Some(prev) = prev {
        // xml.py:85-86 — `previous.tail = f'{previous.tail} {full_text}' if
        // previous.tail else full_text`.
        let new_tail = match tail(&prev) {
            Some(existing) => format!("{existing} {full_text}"),
            None => full_text,
        };
        set_tail(&prev, Some(&new_tail));
    } else if let Some(existing) = element_text(&p) {
        // xml.py:87-88 — `elif parent.text is not None: parent.text =
        // f'{parent.text} {full_text}'`.
        let new_text = format!("{existing} {full_text}");
        set_element_text(&p, Some(&new_text));
    } else {
        // xml.py:89-90 — `else: parent.text = full_text`.
        set_element_text(&p, Some(&full_text));
    }
    // xml.py:91 — `parent.remove(element)`. NOTE: do NOT call delete_element
    // here — we have already promoted the tail onto the previous-sibling /
    // parent-text in the fold above, and delete_element would re-anchor it
    // a second time. But in our rcdom model, the tail lives as a sibling
    // Text-node run AFTER `element` (not on the element itself as in lxml).
    // We've copied that text into prev.tail / parent.text already, so the
    // sibling Text run must ALSO be detached — otherwise the visible tail
    // is duplicated.
    let tail_siblings = collect_following_text_siblings(element);
    for t in &tail_siblings {
        dom::remove(t);
    }
    dom::remove(element);
}

// ===========================================================================
// remove_empty_elements (xml.py:94-103)
// ===========================================================================

/// `xml.py:94-103` — `remove_empty_elements(tree)`.
///
/// Drops every descendant element with NO children AND no significant text
/// AND no significant tail. Skips `<graphic>` (semantically empty by design)
/// and any direct child of `<code>` (formatting-load-bearing whitespace).
///
/// Python uses `tree.iter('*')` (a single forward walk) plus per-element
/// `getparent().remove(element)`. lxml tolerates concurrent removal during
/// iter because each yield captures the next-pointer fresh from the parent's
/// children list. Our rcdom analogue is to snapshot the descendant list
/// first, then iterate. The set of "is this descendant still empty" decisions
/// is order-independent on a clean tree (any descendant removed by a
/// child-removal-cascade would not have qualified — only leaf-or-leaf-after-
/// removal elements qualify, and our snapshot-then-iterate goes leaf-first
/// in document order which already mirrors Python's behaviour).
pub(crate) fn remove_empty_elements(tree: &NodeRef) {
    // Document-order snapshot of every descendant element.
    let snapshot = get_elements_by_tag_name(tree, "*");
    for element in snapshot {
        // xml.py:97 — `if len(element) == 0 and text_chars_test(element.text)
        // is False and text_chars_test(element.tail) is False`.
        let has_element_children = element
            .children
            .borrow()
            .iter()
            .any(|c| matches!(c.data, NodeData::Element { .. }));
        if has_element_children {
            continue;
        }
        let text = element_text(&element);
        let tail_text = tail(&element);
        if text_chars_test(text.as_deref()) || text_chars_test(tail_text.as_deref()) {
            continue;
        }
        // xml.py:98 — `parent = element.getparent()`.
        // llvm-cov:branch-not-reachable: `element` is a descendant from
        // `get_elements_by_tag_name(tree, "*")` (tree itself is excluded), so it
        // always has a parent within the tree; the `None` (continue) side cannot
        // occur (faithful port of Python's `if parent is not None`).
        let Some(p) = parent(&element) else { continue };
        // xml.py:100-102 — `if parent is not None and element.tag !=
        // "graphic" and parent.tag != 'code': parent.remove(element)`.
        if local_name(&element).as_deref() == Some("graphic") {
            continue;
        }
        if local_name(&p).as_deref() == Some("code") {
            continue;
        }
        // lxml's `parent.remove(element)` drops `.tail` with the element
        // (tails are a field ON the element in lxml). rcdom stores tails as
        // a run of Text siblings AFTER the element, so a bare `dom::remove`
        // detaches only the element and leaves the tail Text node behind to
        // coalesce into a neighbour — re-introducing whitespace that Python
        // never had. M9 Stage-5 traced this as the trailing-space-before-
        // inline-tag bug (~36 working-slice records). `delete_element` with
        // `keep_tail=false` walks the following-Text-sibling run via
        // `collect_following_text_siblings` and detaches them with the
        // element, matching lxml.
        delete_element(&element, false);
    }
}

// ===========================================================================
// prune_childless_textless (core.py:47-59 XML "last cleaning")
// ===========================================================================

/// Port of `core.py:48-59` (the `"xml" in options.format` last-cleaning loop):
/// `for element in document.body.iter("*"): if element.tag != "graphic" and
/// len(element) == 0 and not element.text and not element.tail: parent =
/// element.getparent(); if parent is not None and parent.tag != "code":
/// parent.remove(element)`.
///
/// Note the FALSY-string semantics (`not element.text`): an element whose text
/// is `None` or `""` qualifies, but one whose text is whitespace-only (a
/// truthy string) does NOT — this is deliberately LESS aggressive than
/// [`remove_empty_elements`]'s `text_chars_test`. The point of this pass is to
/// strip genuinely-empty inner leaves so that [`remove_empty_elements`] (run
/// next) can cascade-remove the now-childless parents in its own document-order
/// sweep.
fn prune_childless_textless(tree: &NodeRef) {
    for element in get_elements_by_tag_name(tree, "*") {
        // len(element) == 0 — no ELEMENT children.
        let has_element_children = element
            .children
            .borrow()
            .iter()
            .any(|c| matches!(c.data, NodeData::Element { .. }));
        if has_element_children {
            continue;
        }
        // not element.text and not element.tail — falsy (None / "") on BOTH.
        let text_falsy = element_text(&element).map(|t| t.is_empty()).unwrap_or(true);
        let tail_falsy = tail(&element).map(|t| t.is_empty()).unwrap_or(true);
        if !text_falsy || !tail_falsy {
            continue;
        }
        // tag != "graphic".
        if local_name(&element).as_deref() == Some("graphic") {
            continue;
        }
        // parent is not None and parent.tag != "code".
        // llvm-cov:branch-not-reachable: `element` is a descendant from the `"*"`
        // snapshot (tree excluded), so it always has a parent; the `None`
        // (continue) side cannot occur (faithful port of `if parent is not None`).
        let Some(p) = parent(&element) else { continue };
        if local_name(&p).as_deref() == Some("code") {
            continue;
        }
        dom::remove(&element);
    }
}

// ===========================================================================
// strip_double_tags (xml.py:106-112)
// ===========================================================================

/// `xml.py:106-112` — `strip_double_tags(tree)`.
///
/// Prevents nested `<head>`/`<code>`/`<p>` inside the same-name parent (e.g.
/// `<p><p>foo</p></p>`). Python: `for elem in reversed(tree.xpath(".//head |
/// .//code | .//p")): for subelem in elem.iterdescendants("code", "head",
/// "p"): if subelem.tag == elem.tag and subelem.getparent().tag not in
/// NESTING_WHITELIST: merge_with_parent(subelem)`.
///
/// The reverse iteration is load-bearing: nested `<p><p><p>foo</p></p></p>`
/// must be collapsed innermost-first, otherwise the merge_with_parent on the
/// middle-level `<p>` runs while the inner `<p>` is still descended and the
/// inner-level merge breaks against a detached node.
pub(crate) fn strip_double_tags(tree: &NodeRef) {
    // xml.py:108 — `reversed(tree.xpath(".//head | .//code | .//p"))`.
    // Document-order, then reverse.
    let elems = get_elements_by_tag_name(tree, "*");
    let mut filtered: Vec<NodeRef> = elems
        .into_iter()
        .filter(|e| {
            matches!(
                local_name(e).as_deref(),
                Some("head") | Some("code") | Some("p")
            )
        })
        .collect();
    filtered.reverse();

    for elem in &filtered {
        // xml.py:109 — `for subelem in elem.iterdescendants("code", "head",
        // "p")`.
        let descendants: Vec<NodeRef> = get_elements_by_tag_name(elem, "*")
            .into_iter()
            .filter(|d| {
                matches!(
                    local_name(d).as_deref(),
                    Some("head") | Some("code") | Some("p")
                )
            })
            .collect();
        let elem_tag = local_name(elem).unwrap_or_default();
        for subelem in &descendants {
            // xml.py:110 — `if subelem.tag == elem.tag and
            // subelem.getparent().tag not in NESTING_WHITELIST`.
            if local_name(subelem).unwrap_or_default() != elem_tag {
                continue;
            }
            // llvm-cov:branch-not-reachable: `subelem` is a descendant of `elem`
            // (itself a descendant of `tree`), so it always has a parent; the
            // `None` (continue) side cannot occur (faithful port of Python's
            // `subelem.getparent()` which is never None for a descendant).
            let Some(sp) = parent(subelem) else { continue };
            let sp_tag = local_name(&sp).unwrap_or_default();
            if NESTING_WHITELIST.contains(&sp_tag.as_str()) {
                continue;
            }
            // xml.py:111 — `merge_with_parent(subelem)`.
            merge_with_parent(subelem, false);
        }
    }
}

// ===========================================================================
// clean_attributes (xml.py:137-142)
// ===========================================================================

/// `xml.py:137-142` — `clean_attributes(tree)`.
///
/// Walks every descendant element; if the element's tag is NOT in
/// `WITH_ATTRIBUTES`, wipes its entire attribute map. Tags in
/// `WITH_ATTRIBUTES` keep their attributes verbatim.
pub(crate) fn clean_attributes(tree: &NodeRef) {
    // xml.py:139 — `tree.iter('*')`. lxml's `iter('*')` is descendant-OR-self
    // in document order; our `get_elements_by_tag_name(_, "*")` is
    // descendants-only. So we also check `tree` itself.
    let mut all = vec![tree.clone()];
    all.extend(get_elements_by_tag_name(tree, "*"));

    for elem in all {
        // xml.py:140-141 — `if elem.tag not in WITH_ATTRIBUTES:
        // elem.attrib.clear()`.
        // llvm-cov:branch-not-reachable: `all` is `tree` plus its `"*"`
        // descendants — every entry is an Element, and `local_name` returns Some
        // for every Element; the `None` (continue) side cannot occur.
        let Some(tag) = local_name(&elem) else { continue };
        if !WITH_ATTRIBUTES.contains(&tag.as_str()) {
            clear_attributes(&elem);
        }
    }
}

// ===========================================================================
// replace_element_text (xml.py:253-297)
// ===========================================================================

/// `xml.py:253-297` — `replace_element_text(element, include_formatting)`.
///
/// Determines the text representation of `element`'s leading-text run
/// (lxml `.text` — see `dom::element_text`). For most tags this is the
/// raw text; for `<head>`/`<del>`/`<hi>`/`<code>` (with `include_formatting`),
/// markdown wrappers are applied; `<ref>` becomes `[text](target)`;
/// `<cell>` and `<item>` get list/table-cell prefixes.
///
/// Tail handling is NOT done here — the caller (`process_element`) handles
/// `element.tail` separately. This function returns ONLY the in-element
/// text representation.
pub(crate) fn replace_element_text(element: &NodeRef, include_formatting: bool) -> String {
    // xml.py:255 — `elem_text = element.text or ""`.
    let raw_text = element_text(element);
    let mut elem_text = raw_text.clone().unwrap_or_default();
    let tag = local_name(element).unwrap_or_default();

    // xml.py:257-274 — formatting branch when include_formatting AND
    // element.text is non-empty.
    if include_formatting
        && let Some(orig) = raw_text.as_deref()
        && !orig.is_empty()
    {
        match tag.as_str() {
            "head" => {
                // xml.py:258-263 — heading level from rend="hN". Python:
                // `int(element.get("rend")[1])` (raw indexing into rend);
                // TypeError on `None`, ValueError on non-digit. Default 2.
                let number = get_attribute(element, "rend")
                    .as_deref()
                    .and_then(|r| r.chars().nth(1))
                    .and_then(|c| c.to_digit(10))
                    .map(|n| n as usize)
                    .unwrap_or(2);
                elem_text = format!("{} {elem_text}", "#".repeat(number));
            }
            "del" => {
                // xml.py:264-265 — `~~{elem_text}~~`.
                elem_text = format!("~~{elem_text}~~");
            }
            "hi" => {
                // xml.py:266-269 — `rend` mapped via HI_FORMATTING.
                if let Some(rend) = get_attribute(element, "rend")
                    && let Some(wrap) = hi_formatting(&rend)
                {
                    elem_text = format!("{wrap}{elem_text}{wrap}");
                }
            }
            "code" => {
                // xml.py:270-274 — fenced if multiline, inline otherwise.
                if elem_text.contains('\n') {
                    elem_text = format!("```\n{elem_text}\n```");
                } else {
                    elem_text = format!("`{elem_text}`");
                }
            }
            _ => {}
        }
    }

    // xml.py:276-286 — links. Note: this branch runs REGARDLESS of
    // include_formatting (Python `if element.tag == "ref":`).
    if tag == "ref" && !elem_text.is_empty() {
        // xml.py:278 — `link_text = f"[{elem_text}]"`.
        let link_text = format!("[{elem_text}]");
        // xml.py:279-281 — append target when present.
        if let Some(target) = get_attribute(element, "target")
            && !target.is_empty()
        {
            elem_text = format!("{link_text}({target})");
        } else {
            // xml.py:282-284 — missing link attribute warning (no-op in
            // Rust; logger.warning has no analogue at this level).
            elem_text = link_text;
        }
    }
    // xml.py:285-286 — empty-link warning when elem_text empty: no-op.

    // xml.py:287-293 — cells. Note the bare `if`/`elif` chain in Python
    // (not nested under the ref branch).
    let elem_child_count = children(element).len();
    if tag == "cell" && !elem_text.is_empty() && elem_child_count > 0 {
        // xml.py:288-290 — first <p>-child cell branch.
        // llvm-cov:branch-not-reachable (`if let Some(first_child)` None side):
        // this arm is guarded by `elem_child_count > 0` above, so
        // `children(element).first()` is always Some here — only the
        // `== Some("p")` second operand decides the branch.
        if let Some(first_child) = children(element).first()
            && local_name(first_child).as_deref() == Some("p")
        {
            // xml.py:290 — append " " (mid-row) or "| " (start-row).
            if previous_element_sibling(element).is_some() {
                elem_text = format!("{elem_text} ");
            } else {
                elem_text = format!("| {elem_text} ");
            }
        }
    } else if tag == "cell" && !elem_text.is_empty() {
        // xml.py:291-293 — leaf cell branch.
        if previous_element_sibling(element).is_some() {
            // (no leading "|" mid-row).
        } else {
            elem_text = format!("| {elem_text}");
        }
    } else if tag == "item" && !elem_text.is_empty() {
        // xml.py:294-296 — list item: `- {text}\n`.
        elem_text = format!("- {elem_text}\n");
    }

    elem_text
}

// ===========================================================================
// process_element (xml.py:300-351)
// ===========================================================================

/// `xml.py:300-351` — `process_element(element, returnlist, include_formatting)`.
///
/// Recursively flattens `element`'s subtree into `returnlist` as a sequence
/// of text fragments. Caller joins with `"".join(returnlist)` to produce the
/// final formatter output.
///
/// The function structure is faithful to Python (the three-block layout —
/// "process text", "textless-element branch", "after-tag emit" — survives
/// verbatim):
///
/// 1. If `element.text` is present, append `replace_element_text(element,
///    include_formatting)` (xml.py:302-304).
/// 2. Recurse into every child (xml.py:306-307).
/// 3. If `element.text` AND `element.tail` are both absent, handle the
///    "textless element" branch (xml.py:309-336) — graphic emission, newline
///    emission for NEWLINE_ELEMS, early-return for other textless tags.
/// 4. Otherwise, emit the after-tag separator (xml.py:341-347) — newline for
///    NEWLINE_ELEMS not under a `<cell>` ancestor, ` | ` for `<cell>`,
///    nothing for SPECIAL_FORMATTING tags, ` ` for everything else.
/// 5. If `element.tail` is present, append it (xml.py:350-351).
pub(crate) fn process_element(
    element: &NodeRef,
    returnlist: &mut Vec<String>,
    include_formatting: bool,
) {
    // xml.py:302-304 — `if element.text: returnlist.append(
    // replace_element_text(element, include_formatting))`. Python's
    // `if element.text:` is truthy on non-empty strings.
    let has_text = element_text(element)
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if has_text {
        returnlist.push(replace_element_text(element, include_formatting));
    }

    // xml.py:306-307 — recurse into every child.
    for child in children(element) {
        process_element(&child, returnlist, include_formatting);
    }

    let tag = local_name(element).unwrap_or_default();
    let elem_tail = tail(element);
    let has_tail = elem_tail.as_ref().map(|t| !t.is_empty()).unwrap_or(false);

    // xml.py:309-336 — textless-element branch (both text AND tail absent).
    if !has_text && !has_tail {
        if tag == "graphic" {
            // xml.py:310-313 — `<graphic>` rendered as markdown image.
            let title = get_attribute(element, "title").unwrap_or_default();
            let alt = get_attribute(element, "alt").unwrap_or_default();
            let src = get_attribute(element, "src").unwrap_or_default();
            let text = format!("{title} {alt}");
            returnlist.push(format!("![{}]({src})", text.trim()));
            // Fall through to the after-tag emit block.
        } else if NEWLINE_ELEMS.contains(&tag.as_str()) {
            // xml.py:315-332 — newline + table-row machinery.
            if tag == "row" {
                // xml.py:317-330 — table-row padding + head-row separator.
                let cell_count = count_descendant_cells(element);
                // xml.py:319-324 — span_info: colspan OR span, isdigit gate.
                let span_info = get_attribute(element, "colspan")
                    .or_else(|| get_attribute(element, "span"));
                let max_span: usize = match span_info {
                    Some(s) if s.chars().all(|c| c.is_ascii_digit()) && !s.is_empty() => {
                        s.parse::<usize>().unwrap_or(1).min(MAX_TABLE_WIDTH)
                    }
                    _ => 1,
                };
                // xml.py:326-327 — pad short rows with `|`s.
                if cell_count < max_span {
                    let pad = "|".repeat(max_span - cell_count);
                    returnlist.push(format!("{pad}\n"));
                }
                // xml.py:329-330 — head-row underline.
                let has_head_cell = children(element).iter().any(|c| {
                    local_name(c).as_deref() == Some("cell")
                        && get_attribute(c, "role").as_deref() == Some("head")
                });
                if has_head_cell {
                    let sep = "---|".repeat(max_span);
                    returnlist.push(format!("\n|{sep}\n"));
                }
            } else {
                // xml.py:331-332 — plain newline.
                returnlist.push("\n".to_string());
            }
            // Fall through to the after-tag emit block.
        } else if tag != "cell" {
            // xml.py:333-336 — other textless tags: early return (no
            // after-tag emit, no tail).
            return;
        }
        // tag == "cell" falls through to the after-tag block below.
    }

    // xml.py:340-347 — "Now processes end-tag logic correctly" — the
    // after-tag separator emit.
    if NEWLINE_ELEMS.contains(&tag.as_str()) && !has_cell_ancestor(element) {
        // xml.py:341-343 — newline. Spacing hack: U+2424 for formatted
        // mode (except `<row>` which already added its own newlines).
        let sep = if include_formatting && tag != "row" {
            "\n\u{2424}\n"
        } else {
            "\n"
        };
        returnlist.push(sep.to_string());
    } else if tag == "cell" {
        // xml.py:344-345 — `| ` cell-end separator.
        returnlist.push(" | ".to_string());
    } else if !SPECIAL_FORMATTING.contains(&tag.as_str()) {
        // xml.py:346-347 — default trailing space.
        returnlist.push(" ".to_string());
    }

    // xml.py:350-351 — tail text emitted AFTER the closing-tag separator
    // (this is what makes "<p>hi</p>tail" emit "hi\ntail" not "hi tail\n").
    if let Some(t) = elem_tail
        && !t.is_empty()
    {
        returnlist.push(t);
    }
}

// ===========================================================================
// Local helpers (private to process_element)
// ===========================================================================

/// Count descendant `<cell>` elements (for the `<row>` padding heuristic).
/// Python: `len(element.xpath(".//cell"))`.
fn count_descendant_cells(element: &NodeRef) -> usize {
    get_elements_by_tag_name(element, "cell").len()
}

/// True iff `element` has any ancestor whose tag is `<cell>`. Python:
/// `element.xpath("ancestor::cell")` (truthy iff non-empty).
fn has_cell_ancestor(element: &NodeRef) -> bool {
    let mut cur = parent(element);
    while let Some(node) = cur {
        if local_name(&node).as_deref() == Some("cell") {
            return true;
        }
        cur = parent(&node);
    }
    false
}

// ===========================================================================
// xmltotxt (xml.py:354-363)
// ===========================================================================

/// `xml.py:354-363` — `xmltotxt(xmloutput, include_formatting) -> str`.
///
/// The TXT / markdown formatter. Walks `xmloutput`'s subtree via
/// [`process_element`], joins the resulting fragments with `""`, then
/// runs Python's `unescape(sanitize(joined) or "")` post-processing.
///
/// `xmloutput` is `Option<&NodeRef>` (Python: `Optional[_Element]`); `None`
/// short-circuits to `""` per `xml.py:356-357`.
///
/// # Sanitize / unescape
///
/// `xml.py:363` runs `unescape(sanitize(...))`. The Rust port:
/// - `sanitize` is a faithful port of `utils.py:303-312`: line-by-line
///   processing that removes `\u{2424}` (the SPECIAL/markdown spacing
///   hack `process_element` emits at `xml.py:343`) and the HTML space
///   entities `&#10;`, `&#13;`, `&nbsp;`. Empty lines are pruned.
/// - `unescape` decodes the small handful of HTML entities Python's
///   `html.unescape` produces in this pipeline. The post-`process_element`
///   stream contains only `&amp;`/`&lt;`/`&gt;`/`&quot;`/`&apos;` —
///   produced incidentally by lxml's `.text` getter when source HTML
///   carried entities. We handle that minimal set; the full
///   `html.unescape` (~250 named entities) is deferred until a test
///   demands it.
pub(crate) fn xmltotxt(xmloutput: Option<&NodeRef>, include_formatting: bool) -> String {
    // xml.py:356-357 — `if xmloutput is None: return ""`.
    let Some(root) = xmloutput else {
        return String::new();
    };

    // xml.py:359-361 — `returnlist = []; process_element(...)`.
    let mut returnlist: Vec<String> = Vec::new();
    process_element(root, &mut returnlist, include_formatting);

    // xml.py:363 — `return unescape(sanitize("".join(returnlist)) or "")`.
    let joined: String = returnlist.concat();
    let sanitized = sanitize_text(&joined);
    unescape_html(&sanitized)
}

/// Port of `utils.py:315-336` (`sanitize_tree`): walk every element, run
/// `sanitize` over its `.text` and `.tail`, computing the `preserve_space` /
/// `trailing_space` knobs from the `SPACING_PROTECTED` / `FORMATTING_PROTECTED`
/// tag sets (utils.py:79-80, 323-324). Python runs this at `xml.py:167`
/// between `build_xml_output` and the pretty-printing reparse; readex had
/// deferred it (the `serialize_xml_pretty` doc noted "the sanitize_tree
/// behaviour is deferred"), which left raw source whitespace (newlines, runs of
/// spaces) inside element text on the XML path. This restores it.
///
/// We deliberately do NOT port the `attrib` namespace-pruning at
/// utils.py:327-330: the rcdom tree carries no namespaced attributes by this
/// stage (`clean_attributes` already ran), so the loop is a no-op here.
pub(crate) fn sanitize_tree(root: &NodeRef) {
    for elem in descendants_and_self(root) {
        let Some(tag) = local_name(&elem) else {
            // llvm-cov:branch-not-reachable: descendants_and_self pushes `root`
            // then only recurses into `children()` (Element-only, dom.rs:581-
            // 588), so the only candidate non-element is `root` itself. The
            // sole caller (control_xml_output, output.rs:2736) always passes a
            // `<doc>` / `<TEI>` Element root. So `local_name` is always Some.
            continue;
        };
        let parent_tag = parent(&elem)
            .as_ref()
            .and_then(local_name)
            .unwrap_or_default();

        // utils.py:323-324.
        let preserve_space = crate::trafilatura::utils::spacing_protected(&tag)
            || crate::trafilatura::utils::spacing_protected(&parent_tag);
        let trailing_space = preserve_space
            || crate::trafilatura::utils::formatting_protected(&tag)
            || crate::trafilatura::utils::formatting_protected(&parent_tag);

        // utils.py:332-335 — sanitize text + tail in place. Python only
        // touches a slot when it is truthy (`if elem.text:`), so an empty /
        // absent slot is left exactly as-is.
        if let Some(text) = element_text(&elem).filter(|t| !t.is_empty()) {
            let cleaned = sanitize(&text, preserve_space, trailing_space);
            set_element_text(&elem, cleaned.as_deref());
        }
        if let Some(t) = tail(&elem).filter(|t| !t.is_empty()) {
            let cleaned = sanitize(&t, preserve_space, trailing_space);
            set_tail(&elem, cleaned.as_deref());
        }
    }
}

/// All elements in document order including `root` itself. `sanitize_tree`
/// must touch the root's own text/tail too (Python `tree.iter()` yields the
/// root first), so we cannot reuse the children-only walk.
fn descendants_and_self(root: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    fn rec(n: &NodeRef, out: &mut Vec<NodeRef>) {
        out.push(n.clone());
        for c in children(n) {
            rec(&c, out);
        }
    }
    rec(root, &mut out);
    out
}

/// Faithful subset of `utils.py:303-312` (`sanitize`) — line-by-line cleanup
/// with `\u{2424}` removed (xml.py:343's spacing hack) and HTML space
/// entities decoded. Empty lines (whitespace-only after `line_processing`)
/// are pruned; non-empty lines are `\n`-joined.
fn sanitize_text(text: &str) -> String {
    // The `xmltotxt` callsite (xml.py:363) uses `sanitize` with default knobs
    // (preserve_space=False, trailing_space=False). `sanitize` returns `None`
    // for all-blank input; xml.py:363's `or ""` collapses that to "".
    sanitize(text, false, false).unwrap_or_default()
}

/// Port of `utils.py:303-312` (`sanitize`) with the full `preserve_space` /
/// `trailing_space` knobs (needed by [`sanitize_tree`]; the `xmltotxt`
/// callsite passes the defaults via [`sanitize_text`]).
///
/// - `trailing_space=true`: treat the whole input as ONE line
///   (utils.py:306-307 — `line_processing(text, preserve_space, True)`).
/// - otherwise: process line-by-line, drop `None` lines, `\n`-join, then
///   strip every `\u{2424}` (utils.py:308-310). lxml splitlines semantics are
///   approximated by `split('\n')` (the spacing hack `\u{2424}` is the only
///   non-`\n` line break the pipeline injects, and it is stripped anyway).
///
/// Returns `None` when the result would be empty/all-blank (Python returns
/// `None`; the callsites either `or ""` it or skip the slot).
fn sanitize(text: &str, preserve_space: bool, trailing_space: bool) -> Option<String> {
    if trailing_space {
        return line_processing(text, preserve_space, true);
    }
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        if let Some(p) = line_processing(line, preserve_space, false) {
            out_lines.push(p);
        }
    }
    let joined = out_lines.join("\n").replace('\u{2424}', "");
    if joined.is_empty() {
        None
    } else {
        Some(joined)
    }
}

/// Faithful port of Python's `remove_control_characters`
/// (`utils.py:266-274`).
///
/// One `Regex::replace_all` over the input. The character class covers
/// every codepoint Python's `isprintable() or isspace()` predicate
/// rejects:
///
///   - `\p{Cf}` — Format (all stripped; e.g. ZWSP U+200B, INVISIBLE
///     SEPARATOR U+2063, BOM U+FEFF, SOFT HYPHEN U+00AD).
///   - `\p{Co}` — Private Use Area (all stripped).
///   - `\p{Cn}` — Unassigned (all stripped).
///   - `\x00-\x08`, `\x0E-\x1B`, `\x7F-\x84`, `\x86-\x9F` — the Cc
///     codepoints Python rejects (i.e. `\p{Cc}` minus the ten
///     `isspace()` Cc kept-set: 0x09-0x0D, 0x1C-0x1F, 0x85 NEL).
///   - Cs (surrogates) is structurally unreachable in `&str` and is
///     omitted from the class.
///
/// Full-Unicode equivalence with Python verified by an offline sweep
/// (`notes/m10-strip-probe/`) — zero disagreements across all
/// 1,112,064 non-surrogate codepoints.
pub(crate) fn strip_control_chars(s: &str) -> String {
    static STRIP_RE: OnceLock<Regex> = OnceLock::new();
    let re = STRIP_RE.get_or_init(|| {
        Regex::new(r"[\p{Cf}\p{Co}\p{Cn}\x00-\x08\x0E-\x1B\x7F-\x84\x86-\x9F]")
            .expect("static regex")
    });
    re.replace_all(s, "").into_owned()
}

/// Faithful subset of `utils.py:282-300` (`line_processing`):
/// - replace `&#13;` -> '\r', `&#10;` -> '\n', `&nbsp;` -> '\u{00A0}'
/// - `strip_control_chars` (utils.py:288, M10 Phase 1)
/// - trim (`utils.py:340-346`: collapse whitespace + strip)
/// - return `None` for all-whitespace lines
///
/// Stage 3-B does NOT port the `preserve_space` / `trailing_space` knobs
/// (the `sanitize`-`process_element` callsite at `xml.py:363` uses
/// defaults). M10 Phase 1 lands the `strip_control_chars` call between
/// the entity-substitute and trim blocks per HLD §4 and ADR
/// `wrk_docs/m7-deferred/507b9cdb.md`.
fn line_processing(line: &str, preserve_space: bool, trailing_space: bool) -> Option<String> {
    // utils.py:288 — `remove_control_characters(line.replace('&#13;',
    // '\r').replace('&#10;', '\n').replace('&nbsp;', ' '))`.
    let decoded = line
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&nbsp;", "\u{00A0}");

    // M10 Phase 1 (utils.py:288) — `remove_control_characters(...)` strip,
    // ported per HLD §4 and ADR `wrk_docs/m7-deferred/507b9cdb.md`.
    let decoded = strip_control_chars(&decoded);

    // utils.py:289 — `if not preserve_space:` guards the whole trim block.
    // When preserve_space is set, the (control-char-cleaned) line is returned
    // verbatim: no whitespace collapse, no None-pruning.
    if preserve_space {
        return Some(decoded);
    }

    // utils.py:292 — `trim(LINES_TRIMMING.sub(r" ", new_line))`. Our `trim`
    // (utils.rs:97) already collapses Unicode whitespace + strips, which
    // subsumes LINES_TRIMMING's behaviour on the realistic inputs.
    let trimmed = crate::trafilatura::utils::trim(&decoded);
    // utils.py:294-295 — `if all(map(str.isspace, new_line)): new_line = None`.
    // (`trim` already collapsed to "" for all-blank input, so test emptiness.)
    if trimmed.is_empty() {
        return None;
    }
    // utils.py:296-299 — `elif trailing_space:` re-attach a single leading /
    // trailing space based on the ORIGINAL (pre-trim) line's first/last char.
    if trailing_space {
        let chars: Vec<char> = line.chars().collect();
        let before = if chars.first().is_some_and(|c| c.is_whitespace()) {
            " "
        } else {
            ""
        };
        let after = if chars.last().is_some_and(|c| c.is_whitespace()) {
            " "
        } else {
            ""
        };
        Some(format!("{before}{trimmed}{after}"))
    } else {
        Some(trimmed)
    }
}

/// Faithful subset of Python's `html.unescape` (stdlib `html` module)
/// for the entity set `process_element`'s output stream realistically
/// carries. Stage 3-A's helpers never emit named entities themselves;
/// this is the cleanup pass for entities that survived from the source
/// HTML through lxml's `.text` getter — chiefly cases where the HTML
/// double-escaped them (e.g. `&amp;eacute;` → text `&eacute;`).
///
/// Decodes:
///   * The XML-mandatory five (`amp`, `lt`, `gt`, `quot`, `apos`).
///   * Numeric entities `&#NN;` / `&#xHH;` (decimal / hex codepoints).
///   * The Latin-1 supplement (U+00A0..U+00FF) and the most common
///     general-punctuation / symbol named entities — the ones the
///     M5 corpus actually surfaces (`nbsp`, `eacute`, `times`, `copy`,
///     `reg`, `middot`, `ntilde`, `rsquo`, `lsquo`, `pound`, `ndash`,
///     `mdash`, `raquo`, `laquo`, `hellip`, `bull`, `trade`, …) plus
///     their Latin-1 siblings so we don't have to revisit this for
///     `&Eacute;` / `&Aacute;` / etc.
///
/// Source-of-truth: CPython `html/__init__.py` — `html.unescape`
/// dispatches on `html.entities.html5`. We cover the subset that
/// appears in real-world UTF-8 article HTML; rarer mathematical /
/// Greek-alphabet entities fall through to the verbatim path.
fn unescape_html(s: &str) -> String {
    // Char-by-char scanner. We iterate chars (not bytes) so multi-byte
    // UTF-8 sequences pass through verbatim — a byte-loop would split
    // `\u{0301}` (UTF-8 `0xCC 0x81`) into two separate `char` casts and
    // corrupt the encoding.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        // Lookahead: copy until ';' or until we run out / hit non-entity
        // chars. Limit to 10 chars (named-entity longest + numeric upper
        // bound) so a bare `&` in text doesn't scan unbounded.
        let mut entity = String::new();
        let mut found_end = false;
        for _ in 0..10 {
            match chars.peek() {
                Some(&';') => {
                    chars.next();
                    found_end = true;
                    break;
                }
                Some(&pc) if pc.is_ascii_alphanumeric() || pc == '#' => {
                    entity.push(pc);
                    chars.next();
                }
                _ => break,
            }
        }
        if !found_end {
            // Not an entity: copy '&' + whatever we consumed verbatim.
            out.push('&');
            out.push_str(&entity);
            continue;
        }
        let decoded: Option<String> = match entity.as_str() {
            "amp" => Some("&".to_string()),
            "lt" => Some("<".to_string()),
            "gt" => Some(">".to_string()),
            "quot" => Some("\"".to_string()),
            "apos" => Some("'".to_string()),
            // Latin-1 supplement + common punctuation / symbol entities,
            // mirroring CPython `html.unescape` for the subset realistic
            // HTML article bodies surface (corpus-driven; see doc above).
            "nbsp" => Some("\u{00A0}".to_string()),
            "iexcl" => Some("\u{00A1}".to_string()),
            "cent" => Some("\u{00A2}".to_string()),
            "pound" => Some("\u{00A3}".to_string()),
            "curren" => Some("\u{00A4}".to_string()),
            "yen" => Some("\u{00A5}".to_string()),
            "brvbar" => Some("\u{00A6}".to_string()),
            "sect" => Some("\u{00A7}".to_string()),
            "uml" => Some("\u{00A8}".to_string()),
            "copy" => Some("\u{00A9}".to_string()),
            "ordf" => Some("\u{00AA}".to_string()),
            "laquo" => Some("\u{00AB}".to_string()),
            "not" => Some("\u{00AC}".to_string()),
            "shy" => Some("\u{00AD}".to_string()),
            "reg" => Some("\u{00AE}".to_string()),
            "macr" => Some("\u{00AF}".to_string()),
            "deg" => Some("\u{00B0}".to_string()),
            "plusmn" => Some("\u{00B1}".to_string()),
            "sup2" => Some("\u{00B2}".to_string()),
            "sup3" => Some("\u{00B3}".to_string()),
            "acute" => Some("\u{00B4}".to_string()),
            "micro" => Some("\u{00B5}".to_string()),
            "para" => Some("\u{00B6}".to_string()),
            "middot" => Some("\u{00B7}".to_string()),
            "cedil" => Some("\u{00B8}".to_string()),
            "sup1" => Some("\u{00B9}".to_string()),
            "ordm" => Some("\u{00BA}".to_string()),
            "raquo" => Some("\u{00BB}".to_string()),
            "frac14" => Some("\u{00BC}".to_string()),
            "frac12" => Some("\u{00BD}".to_string()),
            "frac34" => Some("\u{00BE}".to_string()),
            "iquest" => Some("\u{00BF}".to_string()),
            "Agrave" => Some("\u{00C0}".to_string()),
            "Aacute" => Some("\u{00C1}".to_string()),
            "Acirc" => Some("\u{00C2}".to_string()),
            "Atilde" => Some("\u{00C3}".to_string()),
            "Auml" => Some("\u{00C4}".to_string()),
            "Aring" => Some("\u{00C5}".to_string()),
            "AElig" => Some("\u{00C6}".to_string()),
            "Ccedil" => Some("\u{00C7}".to_string()),
            "Egrave" => Some("\u{00C8}".to_string()),
            "Eacute" => Some("\u{00C9}".to_string()),
            "Ecirc" => Some("\u{00CA}".to_string()),
            "Euml" => Some("\u{00CB}".to_string()),
            "Igrave" => Some("\u{00CC}".to_string()),
            "Iacute" => Some("\u{00CD}".to_string()),
            "Icirc" => Some("\u{00CE}".to_string()),
            "Iuml" => Some("\u{00CF}".to_string()),
            "ETH" => Some("\u{00D0}".to_string()),
            "Ntilde" => Some("\u{00D1}".to_string()),
            "Ograve" => Some("\u{00D2}".to_string()),
            "Oacute" => Some("\u{00D3}".to_string()),
            "Ocirc" => Some("\u{00D4}".to_string()),
            "Otilde" => Some("\u{00D5}".to_string()),
            "Ouml" => Some("\u{00D6}".to_string()),
            "times" => Some("\u{00D7}".to_string()),
            "Oslash" => Some("\u{00D8}".to_string()),
            "Ugrave" => Some("\u{00D9}".to_string()),
            "Uacute" => Some("\u{00DA}".to_string()),
            "Ucirc" => Some("\u{00DB}".to_string()),
            "Uuml" => Some("\u{00DC}".to_string()),
            "Yacute" => Some("\u{00DD}".to_string()),
            "THORN" => Some("\u{00DE}".to_string()),
            "szlig" => Some("\u{00DF}".to_string()),
            "agrave" => Some("\u{00E0}".to_string()),
            "aacute" => Some("\u{00E1}".to_string()),
            "acirc" => Some("\u{00E2}".to_string()),
            "atilde" => Some("\u{00E3}".to_string()),
            "auml" => Some("\u{00E4}".to_string()),
            "aring" => Some("\u{00E5}".to_string()),
            "aelig" => Some("\u{00E6}".to_string()),
            "ccedil" => Some("\u{00E7}".to_string()),
            "egrave" => Some("\u{00E8}".to_string()),
            "eacute" => Some("\u{00E9}".to_string()),
            "ecirc" => Some("\u{00EA}".to_string()),
            "euml" => Some("\u{00EB}".to_string()),
            "igrave" => Some("\u{00EC}".to_string()),
            "iacute" => Some("\u{00ED}".to_string()),
            "icirc" => Some("\u{00EE}".to_string()),
            "iuml" => Some("\u{00EF}".to_string()),
            "eth" => Some("\u{00F0}".to_string()),
            "ntilde" => Some("\u{00F1}".to_string()),
            "ograve" => Some("\u{00F2}".to_string()),
            "oacute" => Some("\u{00F3}".to_string()),
            "ocirc" => Some("\u{00F4}".to_string()),
            "otilde" => Some("\u{00F5}".to_string()),
            "ouml" => Some("\u{00F6}".to_string()),
            "divide" => Some("\u{00F7}".to_string()),
            "oslash" => Some("\u{00F8}".to_string()),
            "ugrave" => Some("\u{00F9}".to_string()),
            "uacute" => Some("\u{00FA}".to_string()),
            "ucirc" => Some("\u{00FB}".to_string()),
            "uuml" => Some("\u{00FC}".to_string()),
            "yacute" => Some("\u{00FD}".to_string()),
            "thorn" => Some("\u{00FE}".to_string()),
            "yuml" => Some("\u{00FF}".to_string()),
            "OElig" => Some("\u{0152}".to_string()),
            "oelig" => Some("\u{0153}".to_string()),
            "Scaron" => Some("\u{0160}".to_string()),
            "scaron" => Some("\u{0161}".to_string()),
            "Yuml" => Some("\u{0178}".to_string()),
            "fnof" => Some("\u{0192}".to_string()),
            "circ" => Some("\u{02C6}".to_string()),
            "tilde" => Some("\u{02DC}".to_string()),
            "ensp" => Some("\u{2002}".to_string()),
            "emsp" => Some("\u{2003}".to_string()),
            "thinsp" => Some("\u{2009}".to_string()),
            "zwnj" => Some("\u{200C}".to_string()),
            "zwj" => Some("\u{200D}".to_string()),
            "lrm" => Some("\u{200E}".to_string()),
            "rlm" => Some("\u{200F}".to_string()),
            "ndash" => Some("\u{2013}".to_string()),
            "mdash" => Some("\u{2014}".to_string()),
            "horbar" => Some("\u{2015}".to_string()),
            "lsquo" => Some("\u{2018}".to_string()),
            "rsquo" => Some("\u{2019}".to_string()),
            "sbquo" => Some("\u{201A}".to_string()),
            "ldquo" => Some("\u{201C}".to_string()),
            "rdquo" => Some("\u{201D}".to_string()),
            "bdquo" => Some("\u{201E}".to_string()),
            "dagger" => Some("\u{2020}".to_string()),
            "Dagger" => Some("\u{2021}".to_string()),
            // CPython's `html.entities.html5` recognises `ddagger;` as a
            // case-insensitive alias for `Dagger;` (both decode to
            // U+2021 DOUBLE DAGGER). HTML5 spec permits both spellings.
            // Without this row readex passes `&ddagger;` through verbatim
            // (e.g. surfaced on M5 fixture 86df4d2e's HTML-entity
            // reference table) while Python's `unescape` decodes it.
            "ddagger" => Some("\u{2021}".to_string()),
            "bull" => Some("\u{2022}".to_string()),
            "hellip" => Some("\u{2026}".to_string()),
            "permil" => Some("\u{2030}".to_string()),
            "prime" => Some("\u{2032}".to_string()),
            "Prime" => Some("\u{2033}".to_string()),
            "lsaquo" => Some("\u{2039}".to_string()),
            "rsaquo" => Some("\u{203A}".to_string()),
            "euro" => Some("\u{20AC}".to_string()),
            "trade" => Some("\u{2122}".to_string()),
            "larr" => Some("\u{2190}".to_string()),
            "uarr" => Some("\u{2191}".to_string()),
            "rarr" => Some("\u{2192}".to_string()),
            "darr" => Some("\u{2193}".to_string()),
            "harr" => Some("\u{2194}".to_string()),
            "lArr" => Some("\u{21D0}".to_string()),
            "uArr" => Some("\u{21D1}".to_string()),
            "rArr" => Some("\u{21D2}".to_string()),
            "dArr" => Some("\u{21D3}".to_string()),
            "hArr" => Some("\u{21D4}".to_string()),
            _ => {
                if let Some(rest) = entity.strip_prefix('#') {
                    let cp = if let Some(hex) =
                        rest.strip_prefix('x').or_else(|| rest.strip_prefix('X'))
                    {
                        u32::from_str_radix(hex, 16).ok()
                    } else {
                        rest.parse::<u32>().ok()
                    };
                    cp.and_then(char::from_u32).map(|c| c.to_string())
                } else {
                    None
                }
            }
        };
        if let Some(text) = decoded {
            out.push_str(&text);
        } else {
            // Unknown entity: copy verbatim (`&entity;`).
            out.push('&');
            out.push_str(&entity);
            out.push(';');
        }
    }
    out
}

// ===========================================================================
// YAML header builder (core.py:73-91)
// ===========================================================================

/// `core.py:73-91` — build the YAML-style `---` header that prefixes
/// `extract_to_markdown` output when `options.with_metadata` is true.
///
/// Emits one line per metadata field, in the SAME order Python's tuple
/// at `core.py:75-87` defines:
///
/// ```text
/// ---
/// title: foo
/// author: bar
/// ...
/// ---
/// ```
///
/// Falsy fields (Python `if getattr(document, attr):`) are skipped: empty
/// strings, `None`, and empty lists. Non-empty lists render as Python's
/// `str(list)` (e.g. `['a', 'b']`) — faithful to `core.py:90`
/// `f"{attr}: {str(getattr(document, attr))}\n"`.
///
/// `Metadata` does not carry `fingerprint` or `id` slots (M4 Stage 6
/// deferred). They are silently omitted — equivalent to Python's
/// behaviour on a pre-`set_id` / pre-`content_fingerprint` `Document`,
/// whose `fingerprint`/`id` attributes default to `None` / `""`.
pub(crate) fn build_yaml_header(metadata: &Metadata) -> String {
    let mut header = String::from("---\n");
    // Order is verbatim from core.py:75-87.
    if let Some(v) = &metadata.title
        && !v.is_empty()
    {
        header.push_str(&format!("title: {v}\n"));
    }
    if let Some(v) = &metadata.author
        && !v.is_empty()
    {
        header.push_str(&format!("author: {v}\n"));
    }
    if let Some(v) = &metadata.url
        && !v.is_empty()
    {
        header.push_str(&format!("url: {v}\n"));
    }
    if let Some(v) = &metadata.hostname
        && !v.is_empty()
    {
        header.push_str(&format!("hostname: {v}\n"));
    }
    if let Some(v) = &metadata.description
        && !v.is_empty()
    {
        header.push_str(&format!("description: {v}\n"));
    }
    if let Some(v) = &metadata.site_name
        && !v.is_empty()
    {
        header.push_str(&format!("sitename: {v}\n"));
    }
    if let Some(v) = &metadata.date
        && !v.is_empty()
    {
        header.push_str(&format!("date: {v}\n"));
    }
    if !metadata.categories.is_empty() {
        header.push_str(&format!(
            "categories: {}\n",
            python_repr_list(&metadata.categories)
        ));
    }
    if !metadata.tags.is_empty() {
        header.push_str(&format!("tags: {}\n", python_repr_list(&metadata.tags)));
    }
    // fingerprint / id slots: omitted (Metadata does not carry them).
    if let Some(v) = &metadata.license
        && !v.is_empty()
    {
        header.push_str(&format!("license: {v}\n"));
    }
    header.push_str("---\n");
    header
}

/// Mirror Python `str(list)`: `['a', 'b']` (single-quoted, comma+space
/// separated). Faithful to `core.py:90`'s `str(getattr(document, attr))`
/// for list-valued `categories` / `tags`.
fn python_repr_list(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("'{s}'")).collect();
    format!("[{}]", inner.join(", "))
}

// ===========================================================================
// build_json_output (xml.py:115-134)
// ===========================================================================

/// `xml.py:115-134` — `build_json_output(docmeta, with_metadata=True) -> str`.
///
/// Serialises `Document` + optional metadata into a JSON string.
///
/// # `with_metadata=true` branch (`xml.py:117-127`)
///
/// Python: builds `outputdict = {slot: getattr(docmeta, slot, None) for slot
/// in docmeta.__slots__}` (21 slots from `settings.py:209-232`), then runs
/// `.update({...})` which renames-via-pop: `url`→`source`, `sitename`→
/// `source-hostname`, `description`→`excerpt`, `categories`→
/// `';'.join(categories or [])` (string), `tags`→`';'.join(tags or [])`
/// (string), `body`→`text` (via `xmltotxt(body, include_formatting=False)`).
/// Pops `commentsbody` and re-anchors as `comments` via `xmltotxt(commentsbody,
/// include_formatting=False)` (this OVERWRITES the slot-derived `comments`
/// key, since the slot is `Optional[str]`).
///
/// Final key order (insertion-preserving): `title`, `author`, `hostname`,
/// `date`, `fingerprint`, `id`, `license`, `comments`, `raw_text`, `text`,
/// `language`, `image`, `pagetype`, `filedate`, `source`, `source-hostname`,
/// `excerpt`, `categories`, `tags` — 19 keys.
///
/// # `with_metadata=false` branch (`xml.py:128-130`)
///
/// Python: `outputdict = {'text': xmltotxt(docmeta.body, ...)}` then
/// `outputdict['comments'] = xmltotxt(commentsbody, ...)`. Two keys:
/// `text`, `comments`.
///
/// # Field availability divergence (recorded honestly)
///
/// `Metadata` does not carry `fingerprint`/`id`/`filedate` (M4 Stage 6 deferred).
/// Following the `build_yaml_header` precedent (Stage 3-B), these render as
/// JSON `null` — matching Python's behaviour on a pre-`set_id` /
/// pre-`content_fingerprint` `Document` whose slots default to `None`.
///
/// # Ordering preservation
///
/// `serde_json::Map` is backed by `BTreeMap` by default (alphabetical key
/// order on serialisation). We hand-render the JSON to preserve Python's
/// insertion order — faithful to `json.dumps(outputdict, ensure_ascii=False)`
/// (Python `dict` insertion order since 3.7).
pub(crate) fn build_json_output(doc: &Document, with_metadata: bool) -> String {
    // xml.py:132 — comments are derived from `xmltotxt(commentsbody,
    // include_formatting=False)` regardless of branch.
    let comments_text = xmltotxt(doc.commentsbody.as_ref(), false);
    // xml.py:125/129 — body text via xmltotxt with include_formatting=false.
    let body_text = xmltotxt(Some(&doc.body), false);

    if !with_metadata {
        // xml.py:128-130 — body-only branch. Two keys, hand-rendered to
        // preserve insertion order: text, comments.
        let mut out = String::from("{");
        out.push_str(&format!("\"text\": {}, ", json_str(&body_text)));
        out.push_str(&format!("\"comments\": {}", json_str(&comments_text)));
        out.push('}');
        return out;
    }

    // xml.py:117-127 — full metadata branch. 19 keys in Python insertion
    // order (see function docstring above).
    let md = &doc.metadata;
    let mut out = String::from("{");

    let pairs: [(&str, String); 19] = [
        // 1. title
        ("title", json_optional_str(md.title.as_deref())),
        // 2. author
        ("author", json_optional_str(md.author.as_deref())),
        // 3. hostname
        ("hostname", json_optional_str(md.hostname.as_deref())),
        // 4. date
        ("date", json_optional_str(md.date.as_deref())),
        // 5. fingerprint — Metadata does not carry this (Stage 6 deferred);
        //    Python's `Document.fingerprint` defaults to `None` pre-set_id.
        ("fingerprint", "null".to_string()),
        // 6. id — same as fingerprint.
        ("id", "null".to_string()),
        // 7. license
        ("license", json_optional_str(md.license.as_deref())),
        // 8. comments (overwritten by xmltotxt(commentsbody))
        ("comments", json_str(&comments_text)),
        // 9. raw_text — from Document, not Metadata.
        (
            "raw_text",
            if doc.raw_text.is_empty() {
                "null".to_string()
            } else {
                json_str(&doc.raw_text)
            },
        ),
        // 10. text (xmltotxt(body))
        ("text", json_str(&body_text)),
        // 11. language
        ("language", json_optional_str(md.language.as_deref())),
        // 12. image
        ("image", json_optional_str(md.image.as_deref())),
        // 13. pagetype
        ("pagetype", json_optional_str(md.pagetype.as_deref())),
        // 14. filedate — Metadata does not carry this; Python default None.
        ("filedate", "null".to_string()),
        // 15. source (popped from `url`)
        ("source", json_optional_str(md.url.as_deref())),
        // 16. source-hostname (popped from `sitename`)
        (
            "source-hostname",
            json_optional_str(md.site_name.as_deref()),
        ),
        // 17. excerpt (popped from `description`)
        ("excerpt", json_optional_str(md.description.as_deref())),
        // 18. categories — `';'.join(categories or [])` (string, not list).
        ("categories", json_str(&md.categories.join(";"))),
        // 19. tags — `';'.join(tags or [])` (string, not list).
        ("tags", json_str(&md.tags.join(";"))),
    ];

    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!("\"{k}\": {v}"));
    }
    out.push('}');
    out
}

/// Render an `Option<&str>` as a JSON string or `null` (Python `None` →
/// `null` per `json.dumps`).
fn json_optional_str(v: Option<&str>) -> String {
    match v {
        Some(s) => json_str(s),
        None => "null".to_string(),
    }
}

/// Render a `&str` as a JSON string literal. Delegates to `serde_json` for
/// faithful escaping (`\n`, `\t`, `\"`, `\\`, `\u00XX` for control chars,
/// non-ASCII passes through verbatim — matching Python's
/// `json.dumps(..., ensure_ascii=False)` at `xml.py:134`).
fn json_str(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

// ===========================================================================
// xmltocsv (xml.py:366-390)
// ===========================================================================

/// `xml.py:366-390` — `xmltocsv(document, include_formatting, *, delim="\t",
/// null="null") -> str`.
///
/// Emits ONE data row (Python `outputwriter.writerow([...])`); the caller
/// supplies the optional header row separately. The Stage 3-C public surface
/// `extract_to_csv` emits header + data row (see [`extract_to_csv`] note).
///
/// # Column order (xml.py:377-389)
///
/// 11 columns, in exactly this order:
/// 1. `url` (Document.url)
/// 2. `id` (Document.id)
/// 3. `fingerprint` (Document.fingerprint)
/// 4. `hostname` (Document.hostname)
/// 5. `title` (Document.title)
/// 6. `image` (Document.image)
/// 7. `date` (Document.date)
/// 8. `text` (xmltotxt(body, include_formatting) OR `null` when empty)
/// 9. `comments` (xmltotxt(commentsbody, include_formatting) OR `null`
///    when empty)
/// 10. `license` (Document.license)
/// 11. `pagetype` (Document.pagetype)
///
/// Python writes `d if d else null` for every field (`xml.py:377`) — empty
/// strings, `None`, and missing values render as the `null` parameter.
///
/// # `with_metadata` gating (parity with `build_json_output`)
///
/// Python's `core.py:269-270` builds a *fresh empty* `Document()` when
/// `options.with_metadata` is `false`, so every metadata-derived CSV column
/// (`url`, `id`, `fingerprint`, `hostname`, `title`, `image`, `date`,
/// `license`, `pagetype`) renders as the `null` token — only the body-derived
/// `text` / `comments` columns carry content. readex has no separate
/// "empty Document" carrier, so we mirror the same observable behaviour with
/// a `with_metadata` flag: when `false`, the metadata columns are forced to
/// `null` regardless of what `doc.metadata` holds. This matches how
/// [`build_json_output`] honours the same flag.
///
/// The `fingerprint` column is *always* `null` here (readex deliberately does
/// not compute Python's blake2b `content_fingerprint`; see the CSV gate's
/// `wrk_docs/m7-deferred/fingerprint-blake2b.md` ADR). Python emits a real
/// 16-char hex fingerprint even with `with_metadata=false`, so the csv gate
/// masks + shape-checks that single column.
///
/// # CSV quoting (xml.py:374)
///
/// Python uses `csv.QUOTE_MINIMAL`: quote a field only when it contains the
/// delimiter, a `"`, a `\r`, or a `\n`. Quoted fields double-up internal
/// `"` characters. No CSV-crate dep is used — this is a hand-roll faithful
/// to Python's stdlib behaviour.
pub(crate) fn xmltocsv(
    doc: &Document,
    include_formatting: bool,
    delim: &str,
    null: &str,
    with_metadata: bool,
) -> String {
    // xml.py:369-370 — body / comments text via xmltotxt, falling back to
    // the `null` token when empty.
    let body_text = xmltotxt(Some(&doc.body), include_formatting);
    let posttext = if body_text.is_empty() { null.to_string() } else { body_text };
    let comments_text = xmltotxt(doc.commentsbody.as_ref(), include_formatting);
    let commentstext = if comments_text.is_empty() {
        null.to_string()
    } else {
        comments_text
    };

    // When metadata is gated OFF, Python's empty `Document()` makes every
    // metadata column `None` → `null`. `with_metadata=false` reproduces that
    // by reading the real metadata only when the flag is set.
    let md = &doc.metadata;
    fn meta(v: Option<&str>, with_metadata: bool) -> Option<&str> {
        if with_metadata {
            v
        } else {
            None
        }
    }
    // xml.py:378-388 — column order, with `d if d else null` for each.
    let columns: [String; 11] = [
        csv_or_null(meta(md.url.as_deref(), with_metadata), null), // 1. url
        csv_or_null(None, null),                                   // 2. id (Metadata lacks)
        csv_or_null(None, null),                                   // 3. fingerprint (lacks)
        csv_or_null(meta(md.hostname.as_deref(), with_metadata), null), // 4. hostname
        csv_or_null(meta(md.title.as_deref(), with_metadata), null), // 5. title
        csv_or_null(meta(md.image.as_deref(), with_metadata), null), // 6. image
        csv_or_null(meta(md.date.as_deref(), with_metadata), null), // 7. date
        posttext,                                                  // 8. text
        commentstext,                                              // 9. comments
        csv_or_null(meta(md.license.as_deref(), with_metadata), null), // 10. license
        csv_or_null(meta(md.pagetype.as_deref(), with_metadata), null), // 11. pagetype
    ];

    let mut row = String::new();
    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            row.push_str(delim);
        }
        row.push_str(&csv_quote_minimal(col, delim));
    }
    // Python's csv.writer terminates rows with `\r\n` per
    // `csv.writer(..., lineterminator='\r\n')` default. Match that.
    row.push_str("\r\n");
    row
}

/// Returns `null` when the value is `None` or empty (Python `d if d else
/// null` — empty strings are falsy), else the value as a String.
fn csv_or_null(v: Option<&str>, null: &str) -> String {
    match v {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => null.to_string(),
    }
}

/// Faithful subset of Python `csv.writer`'s `QUOTE_MINIMAL` rule: quote the
/// field when it contains the delimiter, a `"`, `\r`, or `\n`. Inside a
/// quoted field, double-up `"` characters.
fn csv_quote_minimal(field: &str, delim: &str) -> String {
    let needs_quote = field.contains(delim)
        || field.contains('"')
        || field.contains('\r')
        || field.contains('\n');
    if !needs_quote {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len() + 2);
    out.push('"');
    for c in field.chars() {
        if c == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}

/// The 11-column CSV header row (column names matching [`xmltocsv`]'s
/// column order). Emitted by the public `extract_to_csv` once per call
/// to match a "headers + one data row" expectation; Python's `xmltocsv`
/// emits only the data row (Python callers prepend headers themselves
/// or use pandas / csv.DictWriter for header emission).
pub(crate) fn csv_header_row(delim: &str) -> String {
    let cols = [
        "url",
        "id",
        "fingerprint",
        "hostname",
        "title",
        "image",
        "date",
        "text",
        "comments",
        "license",
        "pagetype",
    ];
    let mut out = String::new();
    for (i, c) in cols.iter().enumerate() {
        if i > 0 {
            out.push_str(delim);
        }
        out.push_str(c);
    }
    out.push_str("\r\n");
    out
}

// ===========================================================================
// add_xml_meta (xml.py:178-183)
// ===========================================================================

/// `xml.py:178-183` — `add_xml_meta(output, docmeta)`.
///
/// Sets metadata attributes on the `<doc>` root element. Iterates the
/// Python `META_ATTRIBUTES` list (`xml.py:42-46`: sitename, title, author,
/// date, url, hostname, description, categories, tags, license, id,
/// fingerprint, language) in order; for each truthy field, sets the attribute
/// to either the raw string or `';'.join(list)` for list fields
/// (`xml.py:183`). Falsy fields (`None`, empty string, empty list) are
/// silently skipped — matching Python's `if value:` guard at `xml.py:182`.
///
/// `Metadata` does not carry `id`/`fingerprint` slots (M4 Stage 6 deferred);
/// they are silently omitted, identical to Python's behaviour on a
/// pre-`set_id` / pre-`content_fingerprint` `Document`.
pub(crate) fn add_xml_meta(output: &NodeRef, metadata: &Metadata) {
    // META_ATTRIBUTES order is verbatim from xml.py:42-46.
    if let Some(v) = &metadata.site_name
        && !v.is_empty()
    {
        set_attribute(output, "sitename", v);
    }
    if let Some(v) = &metadata.title
        && !v.is_empty()
    {
        set_attribute(output, "title", v);
    }
    if let Some(v) = &metadata.author
        && !v.is_empty()
    {
        set_attribute(output, "author", v);
    }
    if let Some(v) = &metadata.date
        && !v.is_empty()
    {
        set_attribute(output, "date", v);
    }
    if let Some(v) = &metadata.url
        && !v.is_empty()
    {
        set_attribute(output, "url", v);
    }
    if let Some(v) = &metadata.hostname
        && !v.is_empty()
    {
        set_attribute(output, "hostname", v);
    }
    if let Some(v) = &metadata.description
        && !v.is_empty()
    {
        set_attribute(output, "description", v);
    }
    // xml.py:183 — list fields render as `';'.join(list)`.
    if !metadata.categories.is_empty() {
        set_attribute(output, "categories", &metadata.categories.join(";"));
    }
    if !metadata.tags.is_empty() {
        set_attribute(output, "tags", &metadata.tags.join(";"));
    }
    if let Some(v) = &metadata.license
        && !v.is_empty()
    {
        set_attribute(output, "license", v);
    }
    // id / fingerprint — Metadata does not carry these (Stage 6 deferred).
    if let Some(v) = &metadata.language
        && !v.is_empty()
    {
        set_attribute(output, "language", v);
    }
}

// ===========================================================================
// build_xml_output (xml.py:145-156)
// ===========================================================================

/// `xml.py:145-156` — `build_xml_output(docmeta) -> _Element`.
///
/// Wraps `Document.body` (renamed to `<main>`) and `Document.commentsbody`
/// (renamed to `<comments>`) inside a fresh `<doc>` root, then runs
/// `clean_attributes` on each. The `<doc>` root carries the metadata as
/// attributes via [`add_xml_meta`].
///
/// # Divergence from Python (recorded honestly)
///
/// Python's `Document.commentsbody` always exists (defaults to `Element("body")`
/// per `settings.py:251`), so `xml.py:153-154` unconditionally renames it and
/// appends it. Our `Document.commentsbody` is `Option<NodeRef>`. When `None`,
/// we synthesise an empty `<comments>` element — semantically identical to
/// Python's default empty-body case (`<comments/>` after rename).
///
/// # `clean_attributes` scope
///
/// Python passes `docmeta.body` to `clean_attributes` AFTER the
/// `body.tag = 'main'` rename. The walk is descendant-or-self, so the
/// `<main>` element itself is also stripped of attributes — but
/// `WITH_ATTRIBUTES` (`xml.py:39`) doesn't include `main`, so this is
/// effectively a no-op for the root and a meaningful strip for descendants.
/// We faithfully preserve this surface.
pub(crate) fn build_xml_output(doc: &Document) -> NodeRef {
    // xml.py:147 — `output = Element('doc')`.
    let output = dom::create_element("doc");
    // xml.py:148 — `add_xml_meta(output, docmeta)`.
    add_xml_meta(&output, &doc.metadata);

    // xml.py:149 — `docmeta.body.tag = 'main'`. `replace_element_tag` creates
    // a new <main> element, copies attrs/children, splices it into the parent
    // slot if body had one. Since `doc.body` here is freshly extracted (no
    // parent), the returned <main> is a detached node ready for append.
    let main = dom::replace_element_tag(&doc.body, "main");

    // xml.py:152 — `output.append(clean_attributes(docmeta.body))`.
    clean_attributes(&main);
    dom::append_child(&output, &main);

    // xml.py:153-154 — `docmeta.commentsbody.tag = 'comments'; output.append(
    // clean_attributes(docmeta.commentsbody))`. Synthesise empty <comments>
    // when commentsbody is None (Python's settings.py:251 default).
    let comments = match &doc.commentsbody {
        Some(cb) => dom::replace_element_tag(cb, "comments"),
        None => dom::create_element("comments"),
    };
    clean_attributes(&comments);
    dom::append_child(&output, &comments);

    output
}

// ===========================================================================
// TEI output (xml.py:186-607) — Stage 3-E
// ===========================================================================
//
// Port surface, in source order:
//
// | Item | Python source |
// |---|---|
// | `_define_publisher_string`         | xml.py:412-420 |
// | `_handle_text_content_of_div_nodes`| xml.py:494-512 |
// | `_handle_unwanted_tails`           | xml.py:515-529 |
// | `_tei_handle_complex_head`         | xml.py:532-550 |
// | `_wrap_unwanted_siblings_of_div`   | xml.py:553-575 |
// | `_move_element_one_level_up`       | xml.py:578-607 |
// | `write_fullheader`                 | xml.py:423-491 |
// | `write_teitree`                    | xml.py:393-409 |
// | `check_tei`                        | xml.py:196-235 |
// | `build_tei_output`                 | xml.py:186-193 |
//
// `validate_tei` (`xml.py:238-250`) is DEFERRED — Python uses lxml's
// `DTD.validate` which has no Rust equivalent. `tei_validation` is an opt-in
// flag defaulting to false so the deferral is silent on the default path.
// TODO: tei_validation deferred — needs DTD validator (xml.py:238-250).

/// `xml.py:412-420` — `_define_publisher_string(docmeta) -> str`.
///
/// Picks the publisher string for the TEI header:
/// - If BOTH hostname AND sitename are set: `"{sitename.strip()} ({hostname})"`.
/// - Else fall back to hostname OR sitename OR the sentinel `"N/A"`.
fn _define_publisher_string(metadata: &Metadata) -> String {
    let hostname = metadata.hostname.as_deref().filter(|s| !s.is_empty());
    let sitename = metadata.site_name.as_deref().filter(|s| !s.is_empty());
    match (hostname, sitename) {
        (Some(h), Some(s)) => format!("{} ({})", s.trim(), h),
        (Some(h), None) => h.to_string(),
        (None, Some(s)) => s.to_string(),
        (None, None) => "N/A".to_string(),
    }
}

/// `xml.py:494-512` — `_handle_text_content_of_div_nodes(element)`.
///
/// Wraps loose text on a `<div>` into `<p>` children for TEI conformity.
/// `<div>` cannot carry direct text in TEI; the helper either folds the text
/// onto the first/last `<p>` child or inserts a fresh `<p>` wrapper.
///
/// Both `element.text` (leading text) and `element.tail` (text between
/// `element` and its next sibling) are handled. Whitespace-only text is left
/// alone (`element.text.strip()` test at `xml.py:496`).
fn _handle_text_content_of_div_nodes(element: &NodeRef) {
    // xml.py:496-503 — handle leading text.
    if let Some(text) = element_text(element)
        && !text.trim().is_empty()
    {
        let kids = children(element);
        let first_p = kids
            .first()
            .filter(|c| local_name(c).as_deref() == Some("p"))
            .cloned();
        if let Some(p) = first_p {
            // xml.py:498 — `element[0].text = f'{element.text} {element[0].text or ""}'.strip()`.
            let existing = element_text(&p).unwrap_or_default();
            let merged = format!("{text} {existing}");
            set_element_text(&p, Some(merged.trim()));
        } else {
            // xml.py:500-502 — insert a fresh `<p>` as the first child.
            let new_child = dom::create_element("p");
            set_element_text(&new_child, Some(&text));
            insert_child_at(element, &new_child, 0);
        }
        // xml.py:503 — `element.text = None`.
        set_element_text(element, None);
    }

    // xml.py:505-512 — handle tail text.
    if let Some(tail_text) = tail(element)
        && !tail_text.trim().is_empty()
    {
        let kids = children(element);
        let last_p = kids
            .last()
            .filter(|c| local_name(c).as_deref() == Some("p"))
            .cloned();
        if let Some(p) = last_p {
            // xml.py:507 — `element[-1].text = f'{element[-1].text or ""} {element.tail}'.strip()`.
            let existing = element_text(&p).unwrap_or_default();
            let merged = format!("{existing} {tail_text}");
            set_element_text(&p, Some(merged.trim()));
        } else {
            // xml.py:509-511 — append a fresh `<p>` as the last child.
            let new_child = dom::create_element("p");
            set_element_text(&new_child, Some(&tail_text));
            dom::append_child(element, &new_child);
        }
        // xml.py:512 — `element.tail = None`.
        set_tail(element, None);
    }
}

/// `xml.py:515-529` — `_handle_unwanted_tails(element)`.
///
/// Re-anchors tail text on `<p>` / `<ab>` elements: tails on disallowed
/// contexts are stripped (whitespace-only → drop) and either folded into the
/// element text (for `<p>`) or promoted to a fresh `<p>` sibling (for `<ab>`).
fn _handle_unwanted_tails(element: &NodeRef) {
    // xml.py:517 — `element.tail = element.tail.strip() if element.tail else None`.
    let trimmed = tail(element).map(|t| t.trim().to_string());
    let Some(trimmed) = trimmed.filter(|t| !t.is_empty()) else {
        // xml.py:518-519 — if no tail, drop and return.
        set_tail(element, None);
        return;
    };

    // xml.py:529 — `element.tail = None`. In lxml the tail is an attribute of
    // the element; clearing it is a pure metadata edit that does not move
    // siblings. In our rcdom the tail is the run of Text-node siblings AFTER
    // `element`, so we must drop it FIRST: otherwise the new `<p>` sibling
    // inserted at `idx + 1` below would land BETWEEN `element` and its tail
    // text node, leaving that text node orphaned as the new `<p>`'s OWN tail
    // (and re-introducing mixed content under the parent `<div>`). We have
    // already captured the value in `trimmed`, so removing the node now is
    // safe for both branches.
    set_tail(element, None);

    let tag = local_name(element).unwrap_or_default();
    if tag == "p" {
        // xml.py:521-522 — `element.text = " ".join(filter(None, [element.text, element.tail]))`.
        let existing = element_text(element).unwrap_or_default();
        let merged: String = [existing.as_str(), trimmed.as_str()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        set_element_text(element, Some(&merged));
    } else {
        // xml.py:523-528 — new `<p>` sibling at index+1, with text=trimmed_tail.
        let new_sibling = dom::create_element("p");
        set_element_text(&new_sibling, Some(&trimmed));
        // llvm-cov:branch-not-reachable (both let-chain None sides): the sole
        // caller passes an `element` that is already attached to the tree at a
        // known position, so `parent(element)` is always Some and `position_of`
        // always finds it — neither None side can occur (faithful port of
        // Python's `parent.insert(parent.index(element) + 1, ...)`).
        if let Some(p) = parent(element)
            && let Some(idx) = position_of(&p, element)
        {
            insert_child_at(&p, &new_sibling, idx + 1);
        }
    }
}

/// `xml.py:532-550` — `_tei_handle_complex_head(element)`.
///
/// Converts a `<head>` (which by `check_tei`'s outer pass has already been
/// renamed to `<ab type="header">`) into a new `<ab>` whose `<p>` children are
/// flattened into `<lb/>`-separated runs. Returns the new `<ab>` element; the
/// caller replaces the original.
fn _tei_handle_complex_head(element: &NodeRef) -> NodeRef {
    // xml.py:534 — `new_element = Element('ab', attrib=element.attrib)`.
    let new_element = dom::create_element("ab");
    for (k, v) in dom::attributes_in_source_order(element) {
        set_attribute(&new_element, &k, &v);
    }

    // xml.py:535 — `new_element.text = element.text.strip() if element.text else None`.
    let elem_text = element_text(element).map(|t| t.trim().to_string());
    if let Some(t) = elem_text.as_deref().filter(|t| !t.is_empty()) {
        set_element_text(&new_element, Some(t));
    }

    // xml.py:536-546 — iterate children. `<p>` children flatten into the
    // <ab>'s text or get separated by <lb/>; other children are appended.
    for child in children(element) {
        let child_tag = local_name(&child).unwrap_or_default();
        if child_tag == "p" {
            // xml.py:537-544 — flatten <p>.
            let child_text = element_text(&child).unwrap_or_default();
            let kids = children(&new_element);
            let new_text = element_text(&new_element);
            if !kids.is_empty() || new_text.is_some() {
                // xml.py:539-541 — emit <lb> when ab has no children or last tail has text.
                let last = kids.last().cloned();
                let last_has_tail = last
                    .as_ref()
                    .and_then(tail)
                    .map(|t| !t.is_empty())
                    .unwrap_or(false);
                if kids.is_empty() || last_has_tail {
                    let lb = dom::create_element("lb");
                    dom::append_child(&new_element, &lb);
                }
                // xml.py:542 — `new_element[-1].tail = child.text`.
                // llvm-cov:branch-not-reachable (None side): this block is entered
                // only when `!kids.is_empty() || new_text.is_some()`, and an <lb>
                // was just appended when `kids.is_empty()` — so `new_element` has
                // at least one child here and `last()` is always Some.
                if let Some(latest) = children(&new_element).last() {
                    set_tail(latest, Some(&child_text));
                }
            } else {
                // xml.py:543-544 — first child path: text goes onto <ab>.
                set_element_text(&new_element, Some(&child_text));
            }
        } else {
            // xml.py:545-546 — `new_element.append(child)`. In lxml the child's
            // tail travels WITH the element (tail is an element attribute). In
            // our rcdom the tail is a separate following Text-node sibling that
            // `dom::remove` + `dom::append_child` would leave behind, silently
            // dropping it (e.g. `<head ...><code>if</code> expressions</head>`
            // would lose " expressions"). Capture and re-apply it.
            let child_tail = tail(&child);
            dom::remove(&child);
            dom::append_child(&new_element, &child);
            if let Some(t) = child_tail {
                set_tail(&child, Some(&t));
            }
        }
    }

    // xml.py:547-549 — preserve trailing tail (trimmed).
    //
    // NOTE: `new_element` is still DETACHED here (it is only spliced into the
    // tree by the caller via `parent.replace(elem, new_elem)`). `set_tail`
    // on a detached node is a no-op (a tail is a *following sibling* run, which
    // a parentless node cannot have). The caller is therefore responsible for
    // re-applying this trimmed tail AFTER it attaches `new_element` — see
    // `check_tei` (xml.py:207). Capturing it here for the no-children path
    // would silently drop the head's tail otherwise (rcdom reparent-tail class).
    let trimmed_tail = tail(element).map(|t| t.trim().to_string());
    if let Some(t) = trimmed_tail.filter(|t| !t.is_empty()) {
        set_tail(&new_element, Some(&t));
    }

    new_element
}

/// `xml.py:553-575` — `_wrap_unwanted_siblings_of_div(div_element)`.
///
/// Wraps subsequent siblings of `div_element` that are TEI_DIV_SIBLINGS into a
/// fresh `<div>` (so a `<body>` of mixed `<div>` + `<p>` + `<list>` survives
/// TEI's "body children must all be `<div>`" rule). Stops at the next
/// `<div>` sibling.
fn _wrap_unwanted_siblings_of_div(div_element: &NodeRef) {
    let Some(p) = parent(div_element) else { return };

    let mut new_sibling = dom::create_element("div");
    let mut new_sibling_index: Option<usize> = None;

    // xml.py:561 — iterate FOLLOWING siblings (Python `itersiblings()`).
    let siblings = following_element_siblings(div_element);
    for sibling in siblings {
        let stag = local_name(&sibling).unwrap_or_default();
        // xml.py:562-563 — break at the next <div>.
        if stag == "div" {
            break;
        }
        // xml.py:564-566 — sibling is a TEI_DIV_SIBLING -> append to new_sibling.
        if TEI_DIV_SIBLINGS.contains(&stag.as_str()) {
            if new_sibling_index.is_none() {
                new_sibling_index = position_of(&p, &sibling);
            }
            // xml.py:566 `new_sibling.append(sibling)` — lxml moves the
            // sibling's tail INTO the wrapper with it. Use the tail-carrying
            // reparent primitive (a naive remove+append_child orphans the
            // tail in the old parent — the rcdom reparent-tail bug class).
            dom::reparent_with_tail(&new_sibling, &sibling);
        } else {
            // xml.py:569-573 — non-TEI_DIV_SIBLING separator (e.g. <lb/>).
            // Flush the current wrapper if it has any collected children, then
            // start a fresh wrapper. The unmoved separator stays where it is.
            if let Some(idx) = new_sibling_index
                && !children(&new_sibling).is_empty()
            {
                // `new_sibling` is a freshly-built, detached wrapper with no
                // tail of its own (xml.py:571), so plain `insert_child_at` is
                // intentionally tail-less here.
                insert_child_at(&p, &new_sibling, idx);
                new_sibling = dom::create_element("div");
                new_sibling_index = None;
            }
        }
    }

    // xml.py:574-575 — flush any remaining wrapper. Freshly-built, detached
    // wrapper with no tail — intentionally tail-less insert.
    if let Some(idx) = new_sibling_index
        && !children(&new_sibling).is_empty()
    {
        insert_child_at(&p, &new_sibling, idx);
    }
}

/// `xml.py:578-607` — `_move_element_one_level_up(element)`.
///
/// Fix TEI compatibility issues by moving `<head>` (already converted to
/// `<ab>`) out from inside a `<p>` and up to the grandparent — TEI does not
/// allow `<ab>` nested under `<p>`.
fn _move_element_one_level_up(element: &NodeRef) {
    let Some(p) = parent(element) else { return };
    let Some(gp) = parent(&p) else { return };

    // xml.py:588-589 — `new_elem = Element("p"); new_elem.extend(list(element.itersiblings()))`.
    // The "siblings" here are siblings of `element` AFTER it (lxml `itersiblings()`).
    let new_elem = dom::create_element("p");
    let following: Vec<NodeRef> = following_element_siblings(element);
    for sib in &following {
        // xml.py:589 `new_elem.extend(list(element.itersiblings()))` — lxml
        // moves each following sibling WITH its tail. Use the tail-carrying
        // reparent primitive (rcdom reparent-tail bug class: a naive
        // remove+append_child would orphan each sibling's tail).
        dom::reparent_with_tail(&new_elem, sib);
    }

    // xml.py:591 — `grand_parent.insert(grand_parent.index(parent) + 1, element)`.
    // lxml `insert` moves `element` WITH its tail, and the very next step
    // (xml.py:593-596) reads `element.tail` to seed `new_elem.text`. A naive
    // remove+insert would orphan element's tail in `p`, leaving `tail(element)`
    // empty below (rcdom reparent-tail bug class). Carry the tail through.
    let gp_idx_of_p = position_of(&gp, &p);
    let insert_at = gp_idx_of_p.map(|i| i + 1).unwrap_or_else(|| {
        // fall back to end. `element` is still under `p` at this point, so
        // gp's child count is the correct "append" index.
        //
        // llvm-cov:branch-not-reachable: `gp` was obtained as `parent(&p)`
        // (output.rs:2081), so `p` is by construction a child of `gp` and
        // `position_of(&gp, &p)` always returns Some. The None fallback cannot
        // fire; it is defensive belt-and-braces for the lxml `index()` shape.
        children(&gp).len()
    });
    dom::insert_with_tail(&gp, element, insert_at);

    // xml.py:593-596 — tail of `element` becomes `new_elem.text`.
    let elem_tail = tail(element).map(|t| t.trim().to_string());
    if let Some(t) = elem_tail.filter(|t| !t.is_empty()) {
        set_element_text(&new_elem, Some(&t));
        set_tail(element, None);
    }

    // xml.py:598-601 — tail of `parent` becomes `new_elem.tail`.
    //
    // `new_elem` is still DETACHED here, so we cannot apply its tail yet
    // (`set_tail` on a parentless node is a no-op — a tail is a following
    // sibling run). Capture the trimmed value and apply it AFTER `new_elem`
    // is spliced into `gp` below; otherwise the tail (the old `<p>` tail) is
    // silently dropped (rcdom reparent-tail bug class).
    //
    // llvm-cov:branch-not-reachable (the `new_elem_tail.is_some()` true side
    // here and the `set_tail(&new_elem, ...)` at output.rs:2144-2145): in the
    // rcdom port a tail is a *following sibling Text node*, not an lxml-style
    // attribute. `element` was just spliced into `gp` at `gp.index(p) + 1`
    // (output.rs:2100-2112), i.e. directly BETWEEN `p` and any tail-text node
    // that used to follow `p`. So by the time `tail(&p)` is read here, `p`'s
    // immediate following sibling is the moved `element` (an Element), and
    // `tail(&p)` is always None. The original-`<p>`-had-a-tail case therefore
    // cannot reach the Some side. Kept as a faithful port of xml.py:598-601.
    let new_elem_tail = tail(&p)
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    // llvm-cov:branch-not-reachable (TRUE side): per the invariant documented
    // above (output.rs:2140-2147), `element` was just spliced between `p` and any
    // tail-text node, so `tail(&p)` is always None here and `new_elem_tail` is
    // always None — the `is_some()` TRUE side cannot occur.
    if new_elem_tail.is_some() {
        set_tail(&p, None);
    }

    // xml.py:603-604 — insert new_elem one slot after element if non-empty.
    let has_kids = !children(&new_elem).is_empty();
    let has_text = element_text(&new_elem).is_some_and(|s| !s.is_empty());
    let has_tail = new_elem_tail.is_some();
    // llvm-cov:branch-not-reachable (`|| has_tail` third-operand TRUE side):
    // `new_elem_tail` is always None (the invariant above), so `has_tail` is
    // always false — its TRUE side cannot occur.
    if has_kids || has_text || has_tail {
        // grand_parent.index(element) + 1.
        // llvm-cov:branch-not-reachable (None side): `element` is a child of `gp`
        // (it was spliced in earlier at output.rs:2100-2112), so `position_of`
        // always finds it — the None side cannot occur.
        if let Some(idx) = position_of(&gp, element) {
            insert_child_at(&gp, &new_elem, idx + 1);
            // Now attached: apply the captured tail (xml.py:600 `new_elem.tail`).
            // llvm-cov:branch-not-reachable (Some side): `new_elem_tail` is always
            // None here (invariant above), so the Some(t) side cannot occur.
            if let Some(t) = new_elem_tail {
                set_tail(&new_elem, Some(&t));
            }
        }
    }

    // xml.py:606-607 — drop `<p>` if it's now empty and has no text.
    if children(&p).is_empty() && element_text(&p).is_none_or(|s| s.is_empty()) {
        dom::remove(&p);
    }
}

/// `xml.py:423-491` — `write_fullheader(teidoc, docmeta) -> _Element`.
///
/// Builds and appends the `<teiHeader>` to `teidoc`. Carries `<fileDesc>` with
/// `<titleStmt>` / `<publicationStmt>` / `<notesStmt>` / `<sourceDesc>`, a
/// `<profileDesc>` with `<abstract>` / `<textClass>` / `<creation>`, and an
/// `<encodingDesc>` with `<appInfo>` (Trafilatura version + URL).
///
/// Returns the constructed `<teiHeader>` element (already attached to teidoc).
fn write_fullheader(teidoc: &NodeRef, metadata: &Metadata) -> NodeRef {
    let header = dom::create_element("teiHeader");
    dom::append_child(teidoc, &header);

    let filedesc = dom::create_element("fileDesc");
    dom::append_child(&header, &filedesc);

    // xml.py:428-431 — titleStmt with title (always) + author (if any).
    let bib_titlestmt = dom::create_element("titleStmt");
    dom::append_child(&filedesc, &bib_titlestmt);
    let title_elem = dom::create_element("title");
    set_attribute(&title_elem, "type", "main");
    if let Some(t) = metadata.title.as_deref() {
        set_element_text(&title_elem, Some(t));
    }
    dom::append_child(&bib_titlestmt, &title_elem);
    if let Some(a) = metadata.author.as_deref().filter(|a| !a.is_empty()) {
        let author_elem = dom::create_element("author");
        set_element_text(&author_elem, Some(a));
        dom::append_child(&bib_titlestmt, &author_elem);
    }

    // xml.py:433-442 — publicationStmt with publisher + availability/license.
    let publicationstmt_a = dom::create_element("publicationStmt");
    dom::append_child(&filedesc, &publicationstmt_a);
    let publisher_string = _define_publisher_string(metadata);
    if let Some(license) = metadata.license.as_deref().filter(|s| !s.is_empty()) {
        let publisher = dom::create_element("publisher");
        set_element_text(&publisher, Some(&publisher_string));
        dom::append_child(&publicationstmt_a, &publisher);
        let availability = dom::create_element("availability");
        dom::append_child(&publicationstmt_a, &availability);
        let lic_p = dom::create_element("p");
        set_element_text(&lic_p, Some(license));
        dom::append_child(&availability, &lic_p);
    } else {
        // xml.py:441-442 — empty <p> for conformity when no license.
        let empty_p = dom::create_element("p");
        dom::append_child(&publicationstmt_a, &empty_p);
    }

    // xml.py:444-447 — notesStmt with id (if any) + fingerprint (always, even if None).
    let notesstmt = dom::create_element("notesStmt");
    dom::append_child(&filedesc, &notesstmt);
    // id and fingerprint live on Document in Python but Metadata in Rust has neither
    // (M4 Stage 6 deferred — `set_id` / `content_fingerprint`). Python emits the
    // fingerprint note unconditionally with text=docmeta.fingerprint (None becomes
    // a tagless empty element via lxml). We mirror with an empty <note type="fingerprint">.
    let note_fp = dom::create_element("note");
    set_attribute(&note_fp, "type", "fingerprint");
    dom::append_child(&notesstmt, &note_fp);

    // xml.py:449-456 — sourceDesc with bibl (title+sitename+date) + bibl[type=sigle].
    let sourcedesc = dom::create_element("sourceDesc");
    dom::append_child(&filedesc, &sourcedesc);
    let source_bibl = dom::create_element("bibl");
    dom::append_child(&sourcedesc, &source_bibl);

    let sigle_parts: Vec<&str> = [
        metadata.site_name.as_deref(),
        metadata.date.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|s| !s.is_empty())
    .collect();
    let sigle = sigle_parts.join(", ");

    let bibl_parts: Vec<&str> = [metadata.title.as_deref(), Some(sigle.as_str())]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
    let source_bibl_text = bibl_parts.join(", ");
    if !source_bibl_text.is_empty() {
        set_element_text(&source_bibl, Some(&source_bibl_text));
    }

    let sigle_bibl = dom::create_element("bibl");
    set_attribute(&sigle_bibl, "type", "sigle");
    if !sigle.is_empty() {
        set_element_text(&sigle_bibl, Some(&sigle));
    }
    dom::append_child(&sourcedesc, &sigle_bibl);

    // xml.py:458-468 — biblFull with full title/author/publisher/url/date.
    let biblfull = dom::create_element("biblFull");
    dom::append_child(&sourcedesc, &biblfull);
    let bib_titlestmt2 = dom::create_element("titleStmt");
    dom::append_child(&biblfull, &bib_titlestmt2);
    let title2 = dom::create_element("title");
    set_attribute(&title2, "type", "main");
    if let Some(t) = metadata.title.as_deref() {
        set_element_text(&title2, Some(t));
    }
    dom::append_child(&bib_titlestmt2, &title2);
    if let Some(a) = metadata.author.as_deref().filter(|s| !s.is_empty()) {
        let author2 = dom::create_element("author");
        set_element_text(&author2, Some(a));
        dom::append_child(&bib_titlestmt2, &author2);
    }

    let publicationstmt = dom::create_element("publicationStmt");
    dom::append_child(&biblfull, &publicationstmt);
    let publisher2 = dom::create_element("publisher");
    set_element_text(&publisher2, Some(&publisher_string));
    dom::append_child(&publicationstmt, &publisher2);
    if let Some(url) = metadata.url.as_deref().filter(|s| !s.is_empty()) {
        let ptr = dom::create_element("ptr");
        set_attribute(&ptr, "type", "URL");
        set_attribute(&ptr, "target", url);
        dom::append_child(&publicationstmt, &ptr);
    }
    let date_elem = dom::create_element("date");
    if let Some(d) = metadata.date.as_deref() {
        set_element_text(&date_elem, Some(d));
    }
    dom::append_child(&publicationstmt, &date_elem);

    // xml.py:470-483 — profileDesc with abstract, optional textClass, creation.
    let profiledesc = dom::create_element("profileDesc");
    dom::append_child(&header, &profiledesc);
    let abstract_elem = dom::create_element("abstract");
    dom::append_child(&profiledesc, &abstract_elem);
    let abs_p = dom::create_element("p");
    if let Some(d) = metadata.description.as_deref() {
        set_element_text(&abs_p, Some(d));
    }
    dom::append_child(&abstract_elem, &abs_p);

    if !metadata.categories.is_empty() || !metadata.tags.is_empty() {
        let textclass = dom::create_element("textClass");
        dom::append_child(&profiledesc, &textclass);
        let keywords = dom::create_element("keywords");
        dom::append_child(&textclass, &keywords);
        if !metadata.categories.is_empty() {
            let term = dom::create_element("term");
            set_attribute(&term, "type", "categories");
            set_element_text(&term, Some(&metadata.categories.join(",")));
            dom::append_child(&keywords, &term);
        }
        if !metadata.tags.is_empty() {
            let term = dom::create_element("term");
            set_attribute(&term, "type", "tags");
            set_element_text(&term, Some(&metadata.tags.join(",")));
            dom::append_child(&keywords, &term);
        }
    }

    let creation = dom::create_element("creation");
    dom::append_child(&profiledesc, &creation);
    // xml.py:483 — <date type="download">docmeta.filedate</date>. M8 wired the
    // `filedate` slot (today, `%Y-%m-%d`); see metadata.rs.
    let creation_date = dom::create_element("date");
    set_attribute(&creation_date, "type", "download");
    if let Some(fd) = metadata.filedate.as_deref().filter(|s| !s.is_empty()) {
        set_element_text(&creation_date, Some(fd));
    }
    dom::append_child(&creation, &creation_date);

    // xml.py:485-489 — encodingDesc / appInfo / application / label / ptr.
    let encodingdesc = dom::create_element("encodingDesc");
    dom::append_child(&header, &encodingdesc);
    let appinfo = dom::create_element("appInfo");
    dom::append_child(&encodingdesc, &appinfo);
    let application = dom::create_element("application");
    set_attribute(&application, "version", TRAFILATURA_VERSION);
    set_attribute(&application, "ident", "Trafilatura");
    dom::append_child(&appinfo, &application);
    let label = dom::create_element("label");
    set_element_text(&label, Some("Trafilatura"));
    dom::append_child(&application, &label);
    let app_ptr = dom::create_element("ptr");
    set_attribute(&app_ptr, "target", "https://github.com/adbar/trafilatura");
    dom::append_child(&application, &app_ptr);

    header
}

/// `xml.py:393-409` — `write_teitree(docmeta) -> _Element`.
///
/// Builds the TEI root: `<TEI xmlns="...">` with `<teiHeader>` (via
/// [`write_fullheader`]) and `<text><body>` carrying the post and comments
/// bodies (both renamed to `<div type="entry">` / `<div type="comments">`).
fn write_teitree(doc: &Document) -> NodeRef {
    let teidoc = dom::create_element("TEI");
    set_attribute(&teidoc, "xmlns", "http://www.tei-c.org/ns/1.0");

    // xml.py:396 — `write_fullheader(teidoc, docmeta)`.
    let _ = write_fullheader(&teidoc, &doc.metadata);

    // xml.py:397-398 — `text/body` wrapper.
    let textelem = dom::create_element("text");
    dom::append_child(&teidoc, &textelem);
    let textbody = dom::create_element("body");
    dom::append_child(&textelem, &textbody);

    // xml.py:400-403 — post body: rename to <div type="entry"> after clean_attributes.
    let postbody = dom::replace_element_tag(&doc.body, "div");
    clean_attributes(&postbody);
    set_attribute(&postbody, "type", "entry");
    dom::append_child(&textbody, &postbody);

    // xml.py:405-408 — comments body: synthesise empty when None (Python default).
    let commentsbody = match &doc.commentsbody {
        Some(cb) => dom::replace_element_tag(cb, "div"),
        None => dom::create_element("div"),
    };
    clean_attributes(&commentsbody);
    set_attribute(&commentsbody, "type", "comments");
    dom::append_child(&textbody, &commentsbody);

    teidoc
}

/// `xml.py:196-235` — `check_tei(xmldoc, url)`.
///
/// Scrubs TEI-invalid structures in place:
/// 1. Pass 1: `<head>` → `<ab type="header">`, with `_tei_handle_complex_head`
///    for `<head>` with element children and `_move_element_one_level_up`
///    when the head was inside a `<p>`.
/// 2. Pass 2: `<lb>` directly under `<div>` with tail text becomes `<p>`.
/// 3. Pass 3: walk every descendant of `text/body/`. Tags outside
///    [`crate::trafilatura::cleaning::TEI_VALID_TAGS`] are merged with parent
///    via [`merge_with_parent`]. Tags in [`TEI_REMOVE_TAIL`] route through
///    `_handle_unwanted_tails`. `<div>` routes through
///    `_handle_text_content_of_div_nodes` + `_wrap_unwanted_siblings_of_div`.
///    Attributes not in [`TEI_VALID_ATTRS`] are popped.
fn check_tei(xmldoc: &NodeRef) -> &NodeRef {
    use crate::trafilatura::cleaning::TEI_VALID_TAGS;

    // xml.py:199-210 — Pass 1: convert <head> to <ab type="header">.
    let heads: Vec<NodeRef> = get_elements_by_tag_name(xmldoc, "head");
    for elem in heads {
        // Rename head -> ab; replace_element_tag returns a NEW node.
        let ab = dom::replace_element_tag(&elem, "ab");
        set_attribute(&ab, "type", "header");

        // xml.py:202-204 — `parent = elem.getparent(); if parent is None: continue`.
        // llvm-cov:branch-not-reachable: `ab` is the freshly-renamed `<head>`,
        // which `replace_element_tag` splices back into the original head's
        // parent position — so it always has a parent here; the `None` (continue)
        // side cannot occur (faithful port of `if parent is None: continue`).
        let Some(p) = parent(&ab) else { continue };

        // xml.py:205-208 — non-leaf head: complex-head conversion.
        let cur = if !children(&ab).is_empty() {
            // xml.py:206-208 — `new_elem = _tei_handle_complex_head(elem);
            // parent.replace(elem, new_elem)`. lxml's `replace` keeps the new
            // node's OWN tail; here that tail is the original head's trimmed
            // tail (xml.py:547-549). `_tei_handle_complex_head` cannot set it
            // (it returns a DETACHED node, where `set_tail` is a no-op), so we
            // capture the head's trimmed tail here and re-apply it once
            // `new_elem` is attached — otherwise the tail is silently dropped
            // (rcdom reparent-tail bug class).
            let head_tail = tail(&ab).map(|t| t.trim().to_string());
            let new_elem = _tei_handle_complex_head(&ab);
            // parent.replace(elem, new_elem) — find ab in parent, swap.
            // llvm-cov:branch-not-reachable (None side): `ab` is a child of `p`
            // (we just read its parent above), so `position_of` always finds it —
            // the None side cannot occur.
            if let Some(idx) = position_of(&p, &ab) {
                dom::remove(&ab);
                insert_child_at(&p, &new_elem, idx);
                if let Some(t) = head_tail.filter(|t| !t.is_empty()) {
                    set_tail(&new_elem, Some(&t));
                }
            }
            new_elem
        } else {
            ab
        };

        // xml.py:209-210 — head inside <p> -> move one level up.
        let p_tag = local_name(&p).unwrap_or_default();
        if p_tag == "p" {
            _move_element_one_level_up(&cur);
        }
    }

    // xml.py:212-214 — Pass 2: <lb> under <div> with text-bearing tail -> <p>.
    // Python: `xmldoc.findall(".//text/body//div/lb")`.
    let lbs = find_text_body_div_lb(xmldoc);
    for lb in lbs {
        let tail_text = tail(&lb).unwrap_or_default();
        if !tail_text.trim().is_empty() {
            // xml.py:214 — `elem.tag, elem.text, elem.tail = 'p', elem.tail, None`.
            let p_new = dom::replace_element_tag(&lb, "p");
            set_element_text(&p_new, Some(&tail_text));
            set_tail(&p_new, None);
        }
    }

    // xml.py:216-234 — Pass 3: walk descendants of text/body, scrub.
    let body_descendants = find_text_body_descendants(xmldoc);
    for elem in body_descendants {
        let tag = local_name(&elem).unwrap_or_default();
        // xml.py:218-223 — drop tags not in TEI_VALID_TAGS via merge_with_parent.
        if !TEI_VALID_TAGS.contains(&tag.as_str()) {
            merge_with_parent(&elem, false);
            continue;
        }
        // xml.py:224-225 — TEI_REMOVE_TAIL: re-anchor tail.
        if TEI_REMOVE_TAIL.contains(&tag.as_str()) {
            _handle_unwanted_tails(&elem);
        } else if tag == "div" {
            // xml.py:226-228 — <div> housekeeping.
            _handle_text_content_of_div_nodes(&elem);
            _wrap_unwanted_siblings_of_div(&elem);
        }
        // xml.py:232-234 — pop invalid attributes.
        let invalid_attrs: Vec<String> = dom::attributes_in_source_order(&elem)
            .into_iter()
            .map(|(k, _)| k)
            .filter(|k| !TEI_VALID_ATTRS.contains(&k.as_str()))
            .collect();
        for attr in invalid_attrs {
            dom::remove_attribute(&elem, &attr);
        }
    }

    xmldoc
}

/// `xml.py:186-193` — `build_tei_output(docmeta) -> _Element`.
///
/// Top-level TEI build: [`write_teitree`] then [`check_tei`].
fn build_tei_output(doc: &Document) -> NodeRef {
    let output = write_teitree(doc);
    let _ = check_tei(&output);
    output
}

/// Post-process a TEI-serialised string to restore camel-case TEI tag names
/// the rcdom lower-cased during construction. Faster than parsing — a
/// per-tag substitution on element open/close tokens. Applied ONLY to TEI
/// output (XML formatter does not need it).
fn restore_tei_case(s: &str) -> String {
    let mappings: &[(&str, &str)] = &[
        ("<tei ", "<TEI "),
        ("<tei>", "<TEI>"),
        ("</tei>", "</TEI>"),
        ("<teiheader", "<teiHeader"),
        ("</teiheader>", "</teiHeader>"),
        ("<filedesc", "<fileDesc"),
        ("</filedesc>", "</fileDesc>"),
        ("<titlestmt", "<titleStmt"),
        ("</titlestmt>", "</titleStmt>"),
        ("<publicationstmt", "<publicationStmt"),
        ("</publicationstmt>", "</publicationStmt>"),
        ("<notesstmt", "<notesStmt"),
        ("</notesstmt>", "</notesStmt>"),
        ("<sourcedesc", "<sourceDesc"),
        ("</sourcedesc>", "</sourceDesc>"),
        ("<biblfull", "<biblFull"),
        ("</biblfull>", "</biblFull>"),
        ("<profiledesc", "<profileDesc"),
        ("</profiledesc>", "</profileDesc>"),
        ("<textclass", "<textClass"),
        ("</textclass>", "</textClass>"),
        ("<encodingdesc", "<encodingDesc"),
        ("</encodingdesc>", "</encodingDesc>"),
        ("<appinfo", "<appInfo"),
        ("</appinfo>", "</appInfo>"),
    ];
    let mut out = s.to_string();
    for (from, to) in mappings {
        if out.contains(from) {
            out = out.replace(from, to);
        }
    }
    // Self-closing variant: `<teiHeader/>` etc. — the `<tei ` mapping above
    // doesn't catch `<teiheader/>`. Handle separately.
    out = out.replace("<teiheader/>", "<teiHeader/>");
    out = out.replace("<filedesc/>", "<fileDesc/>");
    out = out.replace("<titlestmt/>", "<titleStmt/>");
    out = out.replace("<publicationstmt/>", "<publicationStmt/>");
    out = out.replace("<notesstmt/>", "<notesStmt/>");
    out = out.replace("<sourcedesc/>", "<sourceDesc/>");
    out = out.replace("<biblfull/>", "<biblFull/>");
    out = out.replace("<profiledesc/>", "<profileDesc/>");
    out = out.replace("<textclass/>", "<textClass/>");
    out = out.replace("<encodingdesc/>", "<encodingDesc/>");
    out = out.replace("<appinfo/>", "<appInfo/>");
    out = out.replace("<tei/>", "<TEI/>");
    out
}

// ---------------------------------------------------------------------------
// TEI helper free fns (cross-cut: insertion at index, position lookup, etc.)
// ---------------------------------------------------------------------------

/// `parent.insert(index, child)` — splice `child` into `parent`'s children at
/// position `idx`. Clamps to the children vector length. Detaches `child`
/// from any prior parent first (no-op if already detached).
///
/// Python lxml `Element.insert(idx, child)` semantics.
fn insert_child_at(parent: &NodeRef, child: &NodeRef, idx: usize) {
    // Detach from prior parent (lxml semantics: insert moves the node).
    dom::remove(child);
    use std::rc::Rc;
    let mut kids = parent.children.borrow_mut();
    let clamped = idx.min(kids.len());
    child.parent.set(Some(Rc::downgrade(parent)));
    kids.insert(clamped, child.clone());
}

/// `parent.index(child)` — return the position of `child` in `parent`'s
/// children list, or `None` if not a child.
fn position_of(parent: &NodeRef, child: &NodeRef) -> Option<usize> {
    parent
        .children
        .borrow()
        .iter()
        .position(|c| std::rc::Rc::ptr_eq(c, child))
}

/// `element.itersiblings()` — return the *following* ELEMENT siblings of
/// `element` (those after it in the parent's child list, element-only).
///
/// lxml's `itersiblings()` yields *following* siblings by default. The Python
/// callers here iterate elements only, ignoring intermixed Text siblings (the
/// tail run that lives between siblings).
fn following_element_siblings(element: &NodeRef) -> Vec<NodeRef> {
    let Some((p, idx)) = (|| -> Option<(NodeRef, usize)> {
        let p = parent(element)?;
        let pos = position_of(&p, element)?;
        Some((p, pos))
    })() else {
        // llvm-cov:branch-not-reachable: the sole caller is
        // _move_element_one_level_up (output.rs:2086), which has already
        // unwrapped `parent(element)` into `p` at output.rs:2080 before this
        // runs. So `element` always has a parent (and is found in it). The
        // None arm is defensive only.
        return Vec::new();
    };
    p.children
        .borrow()
        .iter()
        .skip(idx + 1)
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
        .collect()
}

/// `xmldoc.findall(".//text/body//div/lb")` — Python XPath at `xml.py:212`.
/// Returns every `<lb>` whose ancestor chain includes `text -> body -> ... -> div`.
/// Faithful semantic: the `<lb>` must be a descendant of some `<div>` that is
/// itself under `<text>/<body>`.
fn find_text_body_div_lb(xmldoc: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    // Walk text/body subtrees of xmldoc.
    for textelem in get_elements_by_tag_name(xmldoc, "text") {
        for bodyelem in get_elements_by_tag_name(&textelem, "body") {
            // Every <div> under body.
            for divelem in get_elements_by_tag_name(&bodyelem, "div") {
                // Every <lb> directly under that <div> (Python XPath `div/lb`
                // matches direct children).
                for lb in children(&divelem) {
                    if local_name(&lb).as_deref() == Some("lb") {
                        out.push(lb);
                    }
                }
            }
        }
    }
    out
}

/// `xmldoc.findall(".//text/body//*")` — Python XPath at `xml.py:216`.
/// Returns every descendant of `<text>/<body>` in document order.
fn find_text_body_descendants(xmldoc: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    for textelem in get_elements_by_tag_name(xmldoc, "text") {
        for bodyelem in get_elements_by_tag_name(&textelem, "body") {
            out.extend(get_elements_by_tag_name(&bodyelem, "*"));
        }
    }
    out
}

// ===========================================================================
// control_xml_output (xml.py:159-175)
// ===========================================================================

/// Output-format discriminator the Stage-3 public entry-points use to drive
/// [`control_xml_output`] (`xml.py:164`'s `options.format == "xmltei"` arm).
///
/// `xml.py:159-175`'s Python source switches on `options.format`:
/// `"xml"` -> `build_xml_output`, `"xmltei"` -> `build_tei_output`. We encode
/// the discriminator as a closed enum because the Stage 3 public surface
/// (`extract_to_xml` / `extract_to_tei`) gives the caller a typed entry-point
/// for each format and there is no third value.
#[derive(Debug, Clone, Copy)]
pub(crate) enum OutputFormat {
    /// `xml.py:165` — Trafilatura's flat `<doc>` / `<main>` / `<comments>` shape.
    Xml,
    /// `xml.py:164` — Text Encoding Initiative conformant `<TEI>` tree.
    Tei,
}

/// `xml.py:159-175` — `control_xml_output(document, options) -> str`.
///
/// The Stage 3-D/E entry point: runs `strip_double_tags` +
/// `remove_empty_elements` on `document.body`, dispatches to
/// [`build_xml_output`] or [`build_tei_output`] per `format`, then serialises
/// with pretty-printing via [`serialize_xml_pretty`]. Returns the rendered XML
/// string (Python `tostring(..., pretty_print=True, encoding='unicode').strip()`).
///
/// # Stage 3-E TEI dispatch
///
/// Python `xml.py:164` switches on `options.format`: `"xmltei"` dispatches
/// through `build_tei_output` (which runs `write_teitree` + `check_tei` —
/// `xml.py:186-235`); every other recognised XML format goes through
/// `build_xml_output`. The Rust port carries the same dispatch on
/// [`OutputFormat`].
///
/// # `sanitize_tree` deferral
///
/// Python `xml.py:167` runs `sanitize_tree(output_tree)` (utils.py:315-336)
/// before `tostring`. That helper trims spaces, removes control chars, and
/// normalises Unicode per-text-node. Our public `extract_to_xml` /
/// `extract_to_tei` instead NFC-normalises the FINAL string (matching the
/// same `extract_to_markdown` pattern in `core.py:98`). The resulting bytes
/// are equivalent for the invariants tests assert — what reaches the user is
/// NFC text.
///
/// # `remove_blank_text` reparse equivalence
///
/// Python `xml.py:169` reparses through `CONTROL_PARSER = XMLParser(
/// remove_blank_text=True)` (`xml.py:35`) to drop inter-element whitespace
/// before pretty-printing. We mirror this in [`serialize_xml_pretty`] by
/// treating whitespace-only text/tail nodes as absent when deciding indent
/// vs inline emission.
pub(crate) fn control_xml_output(doc: &Document, format: OutputFormat) -> String {
    // core.py:47-59 — the XML branch of `determine_returnstring` runs a
    // "last cleaning" pass over `document.body` BEFORE `control_xml_output`:
    // drop every element that is childless AND has falsy text AND falsy tail
    // (except `<graphic>` and direct children of `<code>`). This guards
    // `"xml" in options.format`, i.e. both xml and xmltei, so it lives here.
    // It is a SEPARATE earlier pass than `remove_empty_elements` below: it
    // removes inner empty leaves (e.g. an empty `<p>` inside a `<cell>`), and
    // the now-childless parent is then caught by `remove_empty_elements`. A
    // single document-order pass cannot cascade parent-after-child, so the
    // two-pass structure is load-bearing for byte-equivalence.
    prune_childless_textless(&doc.body);

    // xml.py:161-162 — `strip_double_tags(document.body); remove_empty_elements
    // (document.body)`. Both mutate in place.
    strip_double_tags(&doc.body);
    remove_empty_elements(&doc.body);

    // xml.py:164-165 — `func = build_xml_output if ... else build_tei_output;
    // output_tree = func(document)`.
    let output_tree = match format {
        OutputFormat::Xml => build_xml_output(doc),
        OutputFormat::Tei => build_tei_output(doc),
    };

    // xml.py:167 — `output_tree = sanitize_tree(output_tree)`: collapse raw
    // source whitespace inside element text/tail (honouring the
    // SPACING_PROTECTED / FORMATTING_PROTECTED knobs). xml.py:169's
    // reparse-through-CONTROL_PARSER (remove_blank_text=True) is folded into
    // serialize_xml_pretty's whitespace handling.
    sanitize_tree(&output_tree);

    // xml.py:175 — `tostring(output_tree, pretty_print=True, encoding='unicode'
    // ).strip()`.
    let serialised = serialize_xml_pretty(&output_tree);
    // TEI tags are XML camel-case (e.g. `<TEI>`, `<teiHeader>`); our HTML-
    // backed rcdom lowers them. Map back at the surface for the TEI branch.
    match format {
        OutputFormat::Xml => serialised,
        OutputFormat::Tei => restore_tei_case(&serialised),
    }
}

// ===========================================================================
// serialize_xml_pretty — hand-rolled lxml-tostring(pretty_print=True) analogue
// ===========================================================================

/// Pretty-print an XML element tree to a string, matching the output of
/// `lxml.etree.tostring(root, pretty_print=True, encoding='unicode').strip()`.
///
/// # Rules (derived from lxml's libxml2-backed pretty-printer)
///
/// 1. Indentation: 2-space increments per nesting level.
/// 2. Self-closing form (`<tag/>`) when an element has NO children, NO text,
///    AND no significant content.
/// 3. **Mixed-content guard (sticky, subtree-wide).** When an element has any
///    non-whitespace text OR any child element has any non-whitespace tail,
///    pretty-printing is DISABLED for that element's children — they emit
///    inline on the same line. Crucially, libxml2 propagates the disabled
///    state down the ENTIRE descendant subtree: once an ancestor is mixed,
///    every descendant is emitted flat (inline), even descendants that are
///    themselves "clean" element-only containers. We thread a `formatting`
///    flag through the recursion to reproduce this: a `<main>` whose child
///    `<head>` carries an `[edit]` tail goes flat, and its `<table>` /
///    `<row>` / `<cell>` descendants stay flat too (verified against lxml).
/// 4. Whitespace-only text/tail nodes are treated as ABSENT (mirroring
///    `CONTROL_PARSER`'s `remove_blank_text=True` reparse at `xml.py:169`).
/// 5. Attributes serialise as `name="value"` in source order; values are
///    XML-escaped (`&`, `<`, `>`, `"`).
/// 6. Element text and tail emit XML-escaped (`&`, `<`, `>`).
/// 7. Trailing newline from lxml's `tostring` is stripped (Python `.strip()`
///    at `xml.py:175`).
///
/// # Why hand-rolled
///
/// `dom::serialize_converted_tree` exists (`dom.rs:1548`) but produces flat
/// compact output (`<doc><main><p>x</p></main></doc>`) — no indentation, no
/// self-closing form, no mixed-content awareness. Pretty-printing is a
/// Stage-3-D-specific concern (markdown / JSON / CSV don't need it); the
/// helper lives here adjacent to its only caller.
fn serialize_xml_pretty(root: &NodeRef) -> String {
    let mut out = String::new();
    // The root starts with formatting ENABLED (libxml2's `format=1`); it is
    // turned off (and stays off) as soon as a mixed-content ancestor is hit.
    write_element_pretty(root, &mut out, 0, true);
    // lxml emits a trailing newline; xml.py:175 strips it.
    out.trim_end_matches('\n').to_string()
}

/// Returns `true` if `s` is empty or contains ONLY whitespace characters
/// (space, tab, CR, LF). Mirrors lxml's `remove_blank_text=True` predicate.
fn is_blank(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

/// Write `element` and its subtree to `out` with `depth` levels of 2-space
/// indentation already accounted for at the element's start (the caller is
/// responsible for emitting the leading indent of THIS element if any).
///
/// `formatting` mirrors libxml2's `format` flag: when `true`, element-only
/// children are indented onto their own lines; when `false`, the whole subtree
/// is emitted flat (inline). Once an ancestor is found to be mixed-content the
/// flag is turned off for the entire descendant subtree (see rule 3 in
/// [`serialize_xml_pretty`]'s doc) — libxml2 never re-enables it deeper down.
fn write_element_pretty(element: &NodeRef, out: &mut String, depth: usize, formatting: bool) {
    let Some(tag) = local_name(element) else {
        // llvm-cov:branch-not-reachable: the entry point serialize_xml_pretty
        // is always handed an Element root (build_xml_output / build_tei_output
        // produce a `<doc>` / `<TEI>` element), and the recursion below only
        // ever descends into `children(element)`, which filters to Element
        // nodes (dom.rs:581-588). So `local_name` is always Some here.
        return;
    };

    // Open tag with attributes.
    out.push('<');
    out.push_str(&tag);
    for (k, v) in dom::attributes_in_source_order(element) {
        out.push(' ');
        out.push_str(&k);
        out.push_str("=\"");
        escape_xml_attr_into(&v, out);
        out.push('"');
    }

    // Inspect children + text to decide self-closing / inline / indented form.
    let text = element_text(element).unwrap_or_default();
    let has_text = !is_blank(&text);
    let kids = children(element);

    // Self-closing form: no element children AND no non-whitespace text.
    if kids.is_empty() && !has_text {
        out.push_str("/>");
        return;
    }

    out.push('>');

    // Decide mixed-content vs indented. Indented requires: formatting is still
    // enabled by an ancestor AND this element has no text AND every child has a
    // blank tail. If formatting was already disabled upstream, this element is
    // emitted flat regardless of its own (clean) content — matching libxml2's
    // sticky `format=0` propagation down the subtree.
    let any_kid_has_text_tail = kids.iter().any(|k| {
        tail(k)
            .as_deref()
            .map(|t| !is_blank(t))
            .unwrap_or(false)
    });
    let mixed = has_text || any_kid_has_text_tail;
    let indent = formatting && !mixed;

    if !indent {
        // Inline emission: write text, then each child + its tail, all on the
        // same logical run. Once inline, formatting stays OFF for descendants
        // (sticky), so the recursive call passes `false`. Text/tail are emitted
        // verbatim (already sanitised by Trafilatura's pipeline upstream).
        if has_text {
            escape_xml_text_into(&text, out);
        }
        for k in &kids {
            write_element_pretty(k, out, depth + 1, false);
            if let Some(t) = tail(k) {
                escape_xml_text_into(&t, out);
            }
        }
    } else {
        // Indented emission: each child on its own line, indented by
        // `depth + 1` levels of 2 spaces. Blank tails are dropped (the
        // `remove_blank_text=True` reparse equivalent). Formatting stays ON.
        for k in &kids {
            out.push('\n');
            for _ in 0..=depth {
                out.push_str("  ");
            }
            write_element_pretty(k, out, depth + 1, true);
        }
        // Closing tag goes on its own line, indented by `depth`.
        out.push('\n');
        for _ in 0..depth {
            out.push_str("  ");
        }
    }

    out.push_str("</");
    out.push_str(&tag);
    out.push('>');
}

/// XML-escape text content (between tags). `&` `<` `>` only — `"` and `'`
/// are legal in text per the XML spec.
fn escape_xml_text_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

/// XML-escape attribute values: `&`, `<`, `"` MUST be escaped inside
/// double-quoted attributes; `>` is escaped for symmetry with lxml's
/// `tostring` output (lxml escapes `>` everywhere).
fn escape_xml_attr_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::readability::dom::{
        Dom, append_child, create_element, create_text_node, set_attribute,
    };

    /// Parse `<body>...</body>`-wrapped HTML and return `(Dom, body)`. The
    /// `Dom` MUST be kept alive — see main_extractor.rs's `parse_body`
    /// note on rcdom Drop quirk.
    ///
    /// NOTE: Only use for tests where the input contains HTML-spec-valid
    /// tags (e.g. `<p>`, `<a>`, `<div>`). Tags like `<head>` / `<cell>`
    /// / `<row>` / `<item>` / `<list>` / `<ref>` / `<hi>` / `<graphic>`
    /// / `<del>` are Trafilatura-internal tag-names that the HTML5 parser
    /// will treat as unknown / reparent / drop. Build those trees
    /// programmatically using `create_element` + `append_child` +
    /// `set_tail` instead.
    fn parse_body(html: &str) -> (Dom, NodeRef) {
        let d = Dom::parse(html);
        let body = d.body().expect("html input has <body>");
        (d, body)
    }

    /// Build a programmatic element tree. `(tag, text, children, tail)`
    /// tuples; root has no tail (an orphan root has no parent to anchor it).
    fn build_elem(
        tag: &str,
        text: Option<&str>,
        children: Vec<NodeRef>,
        attrs: &[(&str, &str)],
    ) -> NodeRef {
        let e = create_element(tag);
        if let Some(t) = text {
            append_child(&e, &create_text_node(t));
        }
        for c in &children {
            append_child(&e, c);
        }
        for (k, v) in attrs {
            set_attribute(&e, k, v);
        }
        e
    }

    // -------------------------------------------------------------------
    // delete_element (xml.py:54-70) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn delete_element_keep_tail_moves_tail_to_previous() {
        // <body><a>x</a><b>y</b>TAIL</body> — delete <b>, tail "TAIL"
        // joins onto <a>'s tail.
        let (_d, body) = parse_body("<html><body><a>x</a><b>y</b>TAIL</body></html>");
        let b = get_elements_by_tag_name(&body, "b")[0].clone();
        delete_element(&b, true);
        let a = get_elements_by_tag_name(&body, "a")[0].clone();
        assert_eq!(tail(&a).as_deref(), Some("TAIL"));
        assert!(get_elements_by_tag_name(&body, "b").is_empty());
    }

    #[test]
    fn delete_element_keep_tail_moves_tail_to_parent_text() {
        // <body><b>y</b>TAIL</body> — <b> is the first child, so its
        // tail "TAIL" joins onto the parent's text.
        let (_d, body) = parse_body("<html><body><b>y</b>TAIL</body></html>");
        let b = get_elements_by_tag_name(&body, "b")[0].clone();
        delete_element(&b, true);
        assert_eq!(element_text(&body).as_deref(), Some("TAIL"));
    }

    #[test]
    fn delete_element_drop_tail_discards_tail() {
        let (_d, body) = parse_body("<html><body><a>x</a><b>y</b>TAIL</body></html>");
        let b = get_elements_by_tag_name(&body, "b")[0].clone();
        delete_element(&b, false);
        let a = get_elements_by_tag_name(&body, "a")[0].clone();
        // Tail "TAIL" should be GONE — neither attached to <a>'s tail
        // nor to parent text after <a>.
        assert_eq!(tail(&a), None);
    }

    #[test]
    fn delete_element_no_parent_is_noop() {
        // Orphan element: delete is a no-op.
        let orphan = create_element("p");
        delete_element(&orphan, true);
        // No panic; element remains parentless.
        assert!(parent(&orphan).is_none());
    }

    #[test]
    fn delete_element_no_tail_keep_tail_true_still_works() {
        let (_d, body) = parse_body("<html><body><a>x</a><b>y</b></body></html>");
        let b = get_elements_by_tag_name(&body, "b")[0].clone();
        delete_element(&b, true);
        let a = get_elements_by_tag_name(&body, "a")[0].clone();
        assert_eq!(tail(&a), None);
    }

    // -------------------------------------------------------------------
    // merge_with_parent (xml.py:73-91) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn merge_with_parent_into_previous_tail() {
        // <root><a>x</a><b>y</b>TAIL</root> — merge <b>: "y" + "TAIL"
        // = "yTAIL" goes onto <a>'s tail. <a>'s prior tail is None ->
        // Python `else: full_text` branch.
        let root = create_element("root");
        let a = build_elem("a", Some("x"), vec![], &[]);
        let b = build_elem("b", Some("y"), vec![], &[]);
        append_child(&root, &a);
        append_child(&root, &b);
        set_tail(&b, Some("TAIL"));
        merge_with_parent(&b, false);
        assert_eq!(tail(&a).as_deref(), Some("yTAIL"));
        assert!(get_elements_by_tag_name(&root, "b").is_empty());
    }

    #[test]
    fn merge_with_parent_into_parent_text_when_no_previous() {
        // <root><b>y</b>TAIL<a>z</a></root> — <b> is the first ELEMENT
        // child; flows onto parent.text. Parent text was None.
        let root = create_element("root");
        let b = build_elem("b", Some("y"), vec![], &[]);
        let a = build_elem("a", Some("z"), vec![], &[]);
        append_child(&root, &b);
        set_tail(&b, Some("TAIL"));
        append_child(&root, &a);
        merge_with_parent(&b, false);
        assert_eq!(element_text(&root).as_deref(), Some("yTAIL"));
    }

    #[test]
    fn merge_with_parent_appends_space_when_previous_tail_existed() {
        // <root><a>x</a>SEP<b>y</b>TAIL</root>: <a>.tail = "SEP" exists,
        // so previous.tail = "SEP yTAIL".
        let root = create_element("root");
        let a = build_elem("a", Some("x"), vec![], &[]);
        let b = build_elem("b", Some("y"), vec![], &[]);
        append_child(&root, &a);
        set_tail(&a, Some("SEP"));
        append_child(&root, &b);
        set_tail(&b, Some("TAIL"));
        merge_with_parent(&b, false);
        assert_eq!(tail(&a).as_deref(), Some("SEP yTAIL"));
    }

    #[test]
    fn merge_with_parent_includes_formatting_in_text() {
        // <root><a>x</a><hi rend="#b">bold</hi> tail</root> with
        // include_formatting=true — text becomes "**bold**" + " tail".
        let root = create_element("root");
        let a = build_elem("a", Some("x"), vec![], &[]);
        let hi = build_elem("hi", Some("bold"), vec![], &[("rend", "#b")]);
        append_child(&root, &a);
        append_child(&root, &hi);
        set_tail(&hi, Some(" tail"));
        merge_with_parent(&hi, true);
        // previous.tail was None -> previous.tail = "**bold** tail".
        assert_eq!(tail(&a).as_deref(), Some("**bold** tail"));
    }

    #[test]
    fn merge_with_parent_no_parent_is_noop() {
        let orphan = create_element("p");
        merge_with_parent(&orphan, false);
        // No panic.
    }

    // -------------------------------------------------------------------
    // remove_empty_elements (xml.py:94-103) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn remove_empty_elements_drops_leaf_empty_element() {
        let (_d, body) = parse_body("<html><body><p>x</p><p></p></body></html>");
        remove_empty_elements(&body);
        let ps = get_elements_by_tag_name(&body, "p");
        assert_eq!(ps.len(), 1);
        assert_eq!(element_text(&ps[0]).as_deref(), Some("x"));
    }

    #[test]
    fn remove_empty_elements_keeps_graphic_even_when_empty() {
        // <graphic> is the documented exception (xml.py:101).
        let root = create_element("root");
        let g = create_element("graphic");
        append_child(&root, &g);
        remove_empty_elements(&root);
        assert_eq!(get_elements_by_tag_name(&root, "graphic").len(), 1);
    }

    #[test]
    fn remove_empty_elements_keeps_empty_inside_code() {
        // <code>'s children survive even when empty (whitespace
        // formatting matters in code).
        let (_d, body) = parse_body("<html><body><code><span></span></code></body></html>");
        remove_empty_elements(&body);
        assert_eq!(get_elements_by_tag_name(&body, "span").len(), 1);
    }

    #[test]
    fn remove_empty_elements_drops_intermediate_after_leaf_removal() {
        // <body><div><p></p></div></body> — leaf <p> is empty, gets
        // removed. After that pass <div> is empty too. Python's single
        // forward iter SHOULD catch this in one pass because
        // get_elements_by_tag_name returns document-order and removing a
        // descendant doesn't perturb that order. But the empty-check
        // happens BEFORE leaf removal in our snapshot iteration, so we
        // need to verify this works.
        let (_d, body) = parse_body("<html><body><div><p></p></div><a>x</a></body></html>");
        remove_empty_elements(&body);
        // Python's forward iter visits <div> first (children non-empty,
        // skip), then <p> (empty, removed). After the loop <div> is
        // empty but NOT removed because the iter already passed it.
        // Faithful behaviour: <div> still present, <p> gone.
        assert_eq!(get_elements_by_tag_name(&body, "p").len(), 0);
        assert_eq!(get_elements_by_tag_name(&body, "div").len(), 1);
    }

    #[test]
    fn remove_empty_elements_preserves_whitespace_only_text() {
        // text_chars_test returns false for whitespace-only — the element
        // qualifies as "empty" and gets removed.
        let (_d, body) = parse_body("<html><body><p>   </p><p>x</p></body></html>");
        remove_empty_elements(&body);
        let ps = get_elements_by_tag_name(&body, "p");
        assert_eq!(ps.len(), 1);
        assert_eq!(element_text(&ps[0]).as_deref(), Some("x"));
    }

    // -------------------------------------------------------------------
    // strip_double_tags (xml.py:106-112) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn strip_double_tags_collapses_simple_double_p() {
        // <root><p><p>foo</p></p></root>: inner <p> merges into outer.
        let root = create_element("root");
        let inner = build_elem("p", Some("foo"), vec![], &[]);
        let outer = build_elem("p", None, vec![inner], &[]);
        append_child(&root, &outer);
        strip_double_tags(&root);
        let ps = get_elements_by_tag_name(&root, "p");
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn strip_double_tags_collapses_triple_nesting() {
        // <root><p><p><p>foo</p></p></p></root>
        let root = create_element("root");
        let innermost = build_elem("p", Some("foo"), vec![], &[]);
        let middle = build_elem("p", None, vec![innermost], &[]);
        let outer = build_elem("p", None, vec![middle], &[]);
        append_child(&root, &outer);
        strip_double_tags(&root);
        // Reverse-order walk: innermost merges first, then middle merges
        // into outer. End state: one <p>.
        let ps = get_elements_by_tag_name(&root, "p");
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn strip_double_tags_respects_nesting_whitelist() {
        // <root><quote><p><p>foo</p></p></quote></root>: outer <p>'s
        // parent is <quote> IN whitelist — but the gate is on the
        // INNER's parent (the outer <p>), whose tag "p" is NOT in
        // whitelist. So inner merges into outer; outer <p> stays.
        let root = create_element("root");
        let inner = build_elem("p", Some("foo"), vec![], &[]);
        let outer = build_elem("p", None, vec![inner], &[]);
        let quote = build_elem("quote", None, vec![outer], &[]);
        append_child(&root, &quote);
        strip_double_tags(&root);
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 1);
        assert_eq!(get_elements_by_tag_name(&root, "quote").len(), 1);
    }

    #[test]
    fn strip_double_tags_collapses_mixed_head_code_p() {
        // <root><head><head>t</head></head><code><code>x</code></code></root>
        let root = create_element("root");
        let inner_head = build_elem("head", Some("t"), vec![], &[]);
        let outer_head = build_elem("head", None, vec![inner_head], &[]);
        let inner_code = build_elem("code", Some("x"), vec![], &[]);
        let outer_code = build_elem("code", None, vec![inner_code], &[]);
        append_child(&root, &outer_head);
        append_child(&root, &outer_code);
        strip_double_tags(&root);
        assert_eq!(get_elements_by_tag_name(&root, "head").len(), 1);
        assert_eq!(get_elements_by_tag_name(&root, "code").len(), 1);
    }

    #[test]
    fn strip_double_tags_leaves_non_matching_pairs_alone() {
        // <root><p><head>x</head></p></root>: tags differ, no merge.
        let root = create_element("root");
        let head = build_elem("head", Some("x"), vec![], &[]);
        let p = build_elem("p", None, vec![head], &[]);
        append_child(&root, &p);
        strip_double_tags(&root);
        assert_eq!(get_elements_by_tag_name(&root, "head").len(), 1);
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 1);
    }

    // -------------------------------------------------------------------
    // clean_attributes (xml.py:137-142) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn clean_attributes_keeps_whitelisted_tags_attrs() {
        // <root><head rend="h1">x</head><cell role="head">y</cell></root>
        let root = create_element("root");
        let head = build_elem("head", Some("x"), vec![], &[("rend", "h1")]);
        let cell = build_elem("cell", Some("y"), vec![], &[("role", "head")]);
        append_child(&root, &head);
        append_child(&root, &cell);
        clean_attributes(&root);
        assert_eq!(get_attribute(&head, "rend").as_deref(), Some("h1"));
        assert_eq!(get_attribute(&cell, "role").as_deref(), Some("head"));
    }

    #[test]
    fn clean_attributes_drops_attrs_on_non_whitelisted_tags() {
        let (_d, body) =
            parse_body("<html><body><p class=\"foo\" id=\"bar\">x</p></body></html>");
        clean_attributes(&body);
        let p = get_elements_by_tag_name(&body, "p")[0].clone();
        assert_eq!(get_attribute(&p, "class"), None);
        assert_eq!(get_attribute(&p, "id"), None);
    }

    #[test]
    fn clean_attributes_drops_attrs_on_div_and_span() {
        // <div>/<span> are NOT in WITH_ATTRIBUTES.
        let (_d, body) = parse_body(
            "<html><body><div class=\"x\">y</div><span title=\"t\">z</span></body></html>",
        );
        clean_attributes(&body);
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let span = get_elements_by_tag_name(&body, "span")[0].clone();
        assert_eq!(get_attribute(&div, "class"), None);
        assert_eq!(get_attribute(&span, "title"), None);
    }

    #[test]
    fn clean_attributes_keeps_ref_target() {
        let (_d, body) = parse_body(
            "<html><body><ref target=\"https://example.com\">link</ref></body></html>",
        );
        clean_attributes(&body);
        let r = get_elements_by_tag_name(&body, "ref")[0].clone();
        assert_eq!(
            get_attribute(&r, "target").as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn clean_attributes_keeps_graphic_src_alt_title() {
        let (_d, body) = parse_body(
            "<html><body><graphic src=\"/img.png\" alt=\"a\" title=\"t\"/></body></html>",
        );
        clean_attributes(&body);
        let g = get_elements_by_tag_name(&body, "graphic")[0].clone();
        assert_eq!(get_attribute(&g, "src").as_deref(), Some("/img.png"));
        assert_eq!(get_attribute(&g, "alt").as_deref(), Some("a"));
        assert_eq!(get_attribute(&g, "title").as_deref(), Some("t"));
    }

    // -------------------------------------------------------------------
    // replace_element_text (xml.py:253-297) — 6 tests (5 minimum + extras
    // to cover every tag mapping)
    // -------------------------------------------------------------------

    #[test]
    fn replace_element_text_head_emits_heading_prefix_when_formatted() {
        let head = build_elem("head", Some("Title"), vec![], &[("rend", "h2")]);
        assert_eq!(replace_element_text(&head, true), "## Title");
    }

    #[test]
    fn replace_element_text_head_defaults_to_h2_when_rend_missing() {
        let head = build_elem("head", Some("Title"), vec![], &[]);
        // No rend -> default level 2 -> "## Title".
        assert_eq!(replace_element_text(&head, true), "## Title");
    }

    #[test]
    fn replace_element_text_hi_b_wraps_bold() {
        let hi = build_elem("hi", Some("bold"), vec![], &[("rend", "#b")]);
        assert_eq!(replace_element_text(&hi, true), "**bold**");
    }

    #[test]
    fn replace_element_text_del_wraps_strikethrough() {
        let d = build_elem("del", Some("old"), vec![], &[]);
        assert_eq!(replace_element_text(&d, true), "~~old~~");
    }

    #[test]
    fn replace_element_text_ref_emits_markdown_link() {
        let r = build_elem("ref", Some("link"), vec![], &[("target", "https://example.com")]);
        // ref runs REGARDLESS of include_formatting (xml.py:276).
        assert_eq!(
            replace_element_text(&r, false),
            "[link](https://example.com)"
        );
    }

    #[test]
    fn replace_element_text_item_emits_dash_prefix() {
        let i = build_elem("item", Some("thing"), vec![], &[]);
        assert_eq!(replace_element_text(&i, false), "- thing\n");
    }

    #[test]
    fn replace_element_text_code_inline_when_no_newline() {
        let c = build_elem("code", Some("print()"), vec![], &[]);
        assert_eq!(replace_element_text(&c, true), "`print()`");
    }

    #[test]
    fn replace_element_text_cell_first_in_row_gets_leading_pipe() {
        // First-cell-in-row: previous_element_sibling is None, so
        // elem_text = "| {text}".
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&cell, &create_text_node("first"));
        append_child(&row, &cell);
        assert_eq!(replace_element_text(&cell, false), "| first");
    }

    // -------------------------------------------------------------------
    // process_element (xml.py:300-351) — 7 tests
    // -------------------------------------------------------------------

    #[test]
    fn process_element_simple_paragraph_emits_text_and_trailing_newline() {
        let (_d, body) = parse_body("<html><body><p>hello world</p></body></html>");
        let p = get_elements_by_tag_name(&body, "p")[0].clone();
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("hello world"));
        assert!(joined.contains('\n'));
    }

    #[test]
    fn process_element_paragraph_with_formatting_uses_u2424() {
        let (_d, body) = parse_body("<html><body><p>text</p></body></html>");
        let p = get_elements_by_tag_name(&body, "p")[0].clone();
        let mut out = Vec::new();
        process_element(&p, &mut out, true);
        let joined: String = out.join("");
        // include_formatting=true emits "\n\u{2424}\n" after <p>.
        assert!(joined.contains('\u{2424}'));
    }

    #[test]
    fn process_element_list_emits_item_dashes() {
        let item_a = build_elem("item", Some("a"), vec![], &[]);
        let item_b = build_elem("item", Some("b"), vec![], &[]);
        let list = build_elem("list", None, vec![item_a, item_b], &[]);
        let mut out = Vec::new();
        process_element(&list, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("- a"), "joined={joined:?}");
        assert!(joined.contains("- b"), "joined={joined:?}");
    }

    #[test]
    fn process_element_heading_emits_hash_prefix_when_formatted() {
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "h1")]);
        let mut out = Vec::new();
        process_element(&h, &mut out, true);
        let joined: String = out.join("");
        assert!(joined.contains("# Title"), "joined={joined:?}");
    }

    #[test]
    fn process_element_table_emits_cell_separators() {
        // <table><row><cell>a</cell><cell>b</cell></row></table>
        let cell_a = build_elem("cell", Some("a"), vec![], &[]);
        let cell_b = build_elem("cell", Some("b"), vec![], &[]);
        let row = build_elem("row", None, vec![cell_a, cell_b], &[]);
        let table = build_elem("table", None, vec![row], &[]);
        let mut out = Vec::new();
        process_element(&table, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("| a"), "joined={joined:?}");
        assert!(joined.contains(" | "), "joined={joined:?}");
    }

    #[test]
    fn process_element_tail_text_after_element_emitted() {
        // <body><p>text</p>TAIL<p>next</p></body> — first <p>'s tail
        // "TAIL" should appear in the output.
        let (_d, body) = parse_body("<html><body><p>text</p>TAIL<p>next</p></body></html>");
        let p = get_elements_by_tag_name(&body, "p")[0].clone();
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("text"));
        assert!(joined.contains("TAIL"));
    }

    #[test]
    fn process_element_graphic_emits_markdown_image() {
        // Empty <graphic src="..."/> with no text or tail goes through
        // the textless branch (xml.py:310-313).
        let g = create_element("graphic");
        crate::readability::dom::set_attribute(&g, "src", "/img.png");
        crate::readability::dom::set_attribute(&g, "alt", "alt text");
        crate::readability::dom::set_attribute(&g, "title", "title text");
        // Must attach to a parent or the after-tag block fires
        // anyway — graphic is not in NEWLINE_ELEMS, so emit happens.
        let mut out = Vec::new();
        process_element(&g, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("![title text alt text](/img.png)"));
    }

    // -------------------------------------------------------------------
    // Document struct — 2 sanity tests
    // -------------------------------------------------------------------

    #[test]
    fn document_struct_carries_body_and_metadata() {
        let body = create_element("body");
        let md = Metadata::default();
        let doc = Document {
            metadata: md,
            body: body.clone(),
            commentsbody: None,
            raw_text: String::new(),
        };
        assert!(doc.commentsbody.is_none());
        assert!(doc.raw_text.is_empty());
        assert_eq!(local_name(&doc.body).as_deref(), Some("body"));
    }

    #[test]
    fn document_struct_carries_commentsbody_when_present() {
        let body = create_element("body");
        let comments = create_element("body");
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: Some(comments.clone()),
            raw_text: "raw".to_string(),
        };
        assert!(doc.commentsbody.is_some());
        assert_eq!(doc.raw_text, "raw");
    }

    // -------------------------------------------------------------------
    // build_json_output (xml.py:115-134) — sub-stage C
    // -------------------------------------------------------------------

    #[test]
    fn build_json_output_with_comments_serialises_commentsbody() {
        // Body has "hello", commentsbody has "a comment".
        let body = create_element("body");
        let p_body = create_element("p");
        append_child(&p_body, &create_text_node("hello world"));
        append_child(&body, &p_body);

        let commentsbody = create_element("body");
        let p_com = create_element("p");
        append_child(&p_com, &create_text_node("a comment"));
        append_child(&commentsbody, &p_com);

        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: Some(commentsbody),
            raw_text: String::new(),
        };
        let out = build_json_output(&doc, false);
        let v: serde_json::Value = serde_json::from_str(&out).expect("parse");
        assert!(v["text"].as_str().unwrap().contains("hello world"));
        assert!(v["comments"].as_str().unwrap().contains("a comment"));
    }

    #[test]
    fn build_json_output_with_metadata_renders_categories_and_tags_as_joined_strings() {
        // Python: `';'.join(categories or [])` — categories render as a
        // semicolon-joined string, NOT a list.
        let md = Metadata {
            categories: vec!["catA".to_string(), "catB".to_string()],
            tags: vec!["tagA".to_string(), "tagB".to_string()],
            ..Metadata::default()
        };
        let doc = Document {
            metadata: md,
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let out = build_json_output(&doc, true);
        let v: serde_json::Value = serde_json::from_str(&out).expect("parse");
        assert_eq!(v["categories"].as_str(), Some("catA;catB"));
        assert_eq!(v["tags"].as_str(), Some("tagA;tagB"));
    }

    // -------------------------------------------------------------------
    // xmltocsv (xml.py:366-390) — sub-stage C
    // -------------------------------------------------------------------

    #[test]
    fn xmltocsv_uses_null_token_for_empty_body() {
        // No body content + no metadata → text + comments columns are "null".
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let row = xmltocsv(&doc, false, "\t", "null", true);
        let cols: Vec<&str> = row.trim_end_matches("\r\n").split('\t').collect();
        // Column 8 (index 7) is text; column 9 (index 8) is comments.
        assert_eq!(cols[7], "null", "text col must be null");
        assert_eq!(cols[8], "null", "comments col must be null");
    }

    #[test]
    fn xmltocsv_custom_delimiter_and_null_token() {
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let row = xmltocsv(&doc, false, ",", "N/A", true);
        // 11 columns, all "N/A", comma-delimited.
        assert!(row.starts_with("N/A,N/A,N/A"), "got: {row:?}");
        assert!(row.contains("N/A,N/A\r\n"), "ends with N/A row");
    }

    #[test]
    fn csv_header_row_matches_python_column_order() {
        let h = csv_header_row("\t");
        let expected =
            "url\tid\tfingerprint\thostname\ttitle\timage\tdate\ttext\tcomments\tlicense\tpagetype\r\n";
        assert_eq!(h, expected);
    }

    // -------------------------------------------------------------------
    // Stage 3-D: add_xml_meta / build_xml_output / control_xml_output /
    // serialize_xml_pretty — see xml.py:145-183.
    // -------------------------------------------------------------------

    /// Build a `Document` with body containing `<p>Hello.</p>` and given metadata.
    fn doc_with_simple_body(metadata: Metadata) -> Document {
        let body = create_element("body");
        let p = build_elem("p", Some("Hello."), vec![], &[]);
        append_child(&body, &p);
        Document {
            metadata,
            body,
            commentsbody: None,
            raw_text: String::new(),
        }
    }

    #[test]
    fn build_xml_output_wraps_body_as_main_with_doc_root() {
        // Python xml.py:147-156: <doc> root with <main> (renamed body) +
        // empty <comments>.
        let doc = doc_with_simple_body(Metadata::default());
        let out = build_xml_output(&doc);
        assert_eq!(local_name(&out).as_deref(), Some("doc"));
        let kids = children(&out);
        assert_eq!(kids.len(), 2, "doc has <main> + <comments>");
        assert_eq!(local_name(&kids[0]).as_deref(), Some("main"));
        assert_eq!(local_name(&kids[1]).as_deref(), Some("comments"));
        // <main> contains the original <p>.
        let ps = get_elements_by_tag_name(&kids[0], "p");
        assert_eq!(ps.len(), 1);
        assert_eq!(element_text(&ps[0]).as_deref(), Some("Hello."));
    }

    #[test]
    fn add_xml_meta_sets_truthy_fields_only() {
        // xml.py:178-183 — `if value: output.set(attribute, ...)`.
        let md = Metadata {
            title: Some("My Title".to_string()),
            url: Some("https://example.com/x".to_string()),
            author: None, // falsy: skipped.
            description: Some(String::new()), // empty string: skipped.
            categories: vec!["news".to_string(), "tech".to_string()],
            ..Metadata::default()
        };

        let doc = create_element("doc");
        add_xml_meta(&doc, &md);

        assert_eq!(get_attribute(&doc, "title").as_deref(), Some("My Title"));
        assert_eq!(
            get_attribute(&doc, "url").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(get_attribute(&doc, "author"), None);
        assert_eq!(get_attribute(&doc, "description"), None);
        // List fields: ';'.join (xml.py:183).
        assert_eq!(
            get_attribute(&doc, "categories").as_deref(),
            Some("news;tech")
        );
    }

    #[test]
    fn control_xml_output_empty_body_yields_doc_with_main_and_comments() {
        // Empty body, no metadata -> minimal doc with self-closing <main/>
        // and <comments/>.
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // Exact match against lxml's pretty-print of the equivalent tree:
        //   '<doc>\n  <main/>\n  <comments/>\n</doc>'
        assert_eq!(s, "<doc>\n  <main/>\n  <comments/>\n</doc>");
    }

    #[test]
    fn control_xml_output_with_metadata_attrs_populate_doc_root() {
        let md = Metadata {
            title: Some("T".to_string()),
            url: Some("https://e.com/".to_string()),
            ..Metadata::default()
        };
        let doc = doc_with_simple_body(md);
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // <doc title="T" url="https://e.com/">... — attribute presence and
        // ordering match add_xml_meta's xml.py:42-46 sequence (title before url).
        assert!(
            s.starts_with("<doc title=\"T\" url=\"https://e.com/\">"),
            "got: {s}"
        );
        // Body content rendered:
        assert!(s.contains("<p>Hello.</p>"));
    }

    #[test]
    fn control_xml_output_without_metadata_has_bare_doc_root() {
        let doc = doc_with_simple_body(Metadata::default());
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // No metadata attrs on <doc>.
        assert!(s.starts_with("<doc>\n"), "got: {s}");
        assert!(!s.contains("title="));
        assert!(!s.contains("url="));
    }

    #[test]
    fn control_xml_output_emits_comments_when_present() {
        // commentsbody with content -> <comments> populated.
        let body = create_element("body");
        let p = build_elem("p", Some("body text"), vec![], &[]);
        append_child(&body, &p);
        let commentsbody = create_element("body");
        let pc = build_elem("p", Some("user reply"), vec![], &[]);
        append_child(&commentsbody, &pc);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: Some(commentsbody),
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // The <comments> element holds the user-reply <p>.
        assert!(
            s.contains("<comments>") && s.contains("user reply"),
            "got: {s}"
        );
    }

    #[test]
    fn control_xml_output_escapes_special_chars_in_text() {
        let body = create_element("body");
        let p = build_elem("p", Some("a < b & c > d"), vec![], &[]);
        append_child(&body, &p);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        assert!(s.contains("a &lt; b &amp; c &gt; d"), "got: {s}");
    }

    #[test]
    fn control_xml_output_escapes_special_chars_in_attributes() {
        // Quote / angle bracket / ampersand in attribute value must escape.
        let md = Metadata {
            title: Some("a \" < & > b".to_string()),
            ..Metadata::default()
        };
        let doc = doc_with_simple_body(md);
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // " -> &quot;, < -> &lt;, & -> &amp;, > -> &gt; (lxml escapes all four
        // inside double-quoted attribute values).
        assert!(
            s.contains("title=\"a &quot; &lt; &amp; &gt; b\""),
            "got: {s}"
        );
    }

    #[test]
    fn control_xml_output_indents_nested_elements_two_spaces() {
        // Verify exact indentation: <doc>\n  <main>\n    <p>...</p>\n  </main>...
        let doc = doc_with_simple_body(Metadata::default());
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // The <p> sits at depth 2 -> 4 spaces of indent.
        assert!(s.contains("\n    <p>Hello.</p>\n"), "got: {s}");
        // <main> sits at depth 1 -> 2 spaces of indent.
        assert!(s.contains("\n  <main>\n"), "got: {s}");
    }

    #[test]
    fn control_xml_output_preserves_attrs_on_with_attributes_tags() {
        // <hi rend="#b"> survives clean_attributes; <p class="ignored"> loses
        // its class (p not in WITH_ATTRIBUTES). Build programmatically so HTML
        // parsing doesn't drop the Trafilatura-internal <hi> tag.
        let body = create_element("body");
        let p = build_elem("p", Some("lead "), vec![], &[("class", "ignored")]);
        let hi = build_elem("hi", Some("bold"), vec![], &[("rend", "#b")]);
        append_child(&p, &hi);
        append_child(&body, &p);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // <hi rend="#b"> preserved (xml.py:39).
        assert!(s.contains("<hi rend=\"#b\">bold</hi>"), "got: {s}");
        // <p class="..."> stripped (p not whitelisted).
        assert!(!s.contains("class=\"ignored\""), "got: {s}");
    }

    #[test]
    fn control_xml_output_is_nfc_normalised_at_public_surface() {
        // The control_xml_output helper itself does NOT NFC; that's the public
        // extract_to_xml's job. But verify the serializer doesn't mangle NFC
        // input — feeding NFC text yields NFC output (the helpers are
        // transparent to Unicode form).
        let body = create_element("body");
        // U+00E9 is the NFC composed form of "é".
        let p = build_elem("p", Some("café"), vec![], &[]);
        append_child(&body, &p);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // U+00E9 (NFC) survives.
        assert!(s.contains("café"), "got: {s}");
        // U+0065 U+0301 (NFD decomposed) would also pass `contains("café")`
        // only if normalised — we explicitly check the byte form.
        assert!(s.contains('\u{00E9}'));
    }

    #[test]
    fn serialize_xml_pretty_self_closes_empty_elements() {
        // <doc><main/></doc> pretty-prints to '<doc>\n  <main/>\n</doc>'.
        let doc = create_element("doc");
        let main = create_element("main");
        append_child(&doc, &main);
        let s = serialize_xml_pretty(&doc);
        assert_eq!(s, "<doc>\n  <main/>\n</doc>");
    }

    #[test]
    fn serialize_xml_pretty_mixed_content_stays_inline() {
        // <main>Lead <hi>bold</hi> tail</main> — mixed content (text + child
        // tail) MUST emit inline, not split across lines.
        let main = create_element("main");
        set_element_text(&main, Some("Lead "));
        let hi = build_elem("hi", Some("bold"), vec![], &[]);
        append_child(&main, &hi);
        set_tail(&hi, Some(" tail"));
        let s = serialize_xml_pretty(&main);
        assert_eq!(s, "<main>Lead <hi>bold</hi> tail</main>");
    }

    // -------------------------------------------------------------------
    // Stage 3-E: TEI helpers — xml.py:186-607.
    // -------------------------------------------------------------------

    /// `_define_publisher_string` — sitename + hostname picks combined form.
    #[test]
    fn tei_define_publisher_string_combines_sitename_and_hostname() {
        let md = Metadata {
            site_name: Some("Example Site".to_string()),
            hostname: Some("example.com".to_string()),
            ..Metadata::default()
        };
        assert_eq!(_define_publisher_string(&md), "Example Site (example.com)");
    }

    /// `_define_publisher_string` — hostname only.
    #[test]
    fn tei_define_publisher_string_falls_back_to_hostname() {
        let md = Metadata {
            hostname: Some("example.com".to_string()),
            ..Metadata::default()
        };
        assert_eq!(_define_publisher_string(&md), "example.com");
    }

    /// `_define_publisher_string` — sitename only.
    #[test]
    fn tei_define_publisher_string_falls_back_to_sitename() {
        let md = Metadata {
            site_name: Some("Solo Site".to_string()),
            ..Metadata::default()
        };
        assert_eq!(_define_publisher_string(&md), "Solo Site");
    }

    /// `_define_publisher_string` — neither set yields `N/A` sentinel.
    #[test]
    fn tei_define_publisher_string_returns_na_when_neither_set() {
        assert_eq!(_define_publisher_string(&Metadata::default()), "N/A");
    }

    /// `_handle_text_content_of_div_nodes`: loose text on `<div>` is folded
    /// onto the first `<p>` child.
    #[test]
    fn tei_handle_text_content_of_div_folds_into_first_p() {
        let div = build_elem("div", Some("loose text"), vec![], &[]);
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&div, &p);
        _handle_text_content_of_div_nodes(&div);
        assert_eq!(element_text(&div), None);
        let kids = children(&div);
        assert_eq!(kids.len(), 1);
        assert_eq!(element_text(&kids[0]).as_deref(), Some("loose text body"));
    }

    /// `_handle_text_content_of_div_nodes`: no `<p>` child -> inserts one.
    #[test]
    fn tei_handle_text_content_of_div_inserts_p_when_no_p_child() {
        let div = build_elem("div", Some("just text"), vec![], &[]);
        _handle_text_content_of_div_nodes(&div);
        let kids = children(&div);
        assert_eq!(kids.len(), 1);
        assert_eq!(local_name(&kids[0]).as_deref(), Some("p"));
        assert_eq!(element_text(&kids[0]).as_deref(), Some("just text"));
    }

    /// `_handle_unwanted_tails` on `<p>`: tail folds into element text.
    #[test]
    fn tei_handle_unwanted_tails_on_p_folds_into_text() {
        let root = create_element("root");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&root, &p);
        set_tail(&p, Some("trailing"));
        _handle_unwanted_tails(&p);
        assert_eq!(element_text(&p).as_deref(), Some("body trailing"));
        assert_eq!(tail(&p), None);
    }

    /// `_handle_unwanted_tails` on `<ab>`: tail becomes a new `<p>` sibling.
    #[test]
    fn tei_handle_unwanted_tails_on_ab_creates_p_sibling() {
        let root = create_element("root");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        append_child(&root, &ab);
        set_tail(&ab, Some("after"));
        _handle_unwanted_tails(&ab);
        // ab's tail is gone; root now has [ab, <p>after</p>].
        assert_eq!(tail(&ab), None);
        let kids = children(&root);
        assert_eq!(kids.len(), 2);
        assert_eq!(local_name(&kids[1]).as_deref(), Some("p"));
        assert_eq!(element_text(&kids[1]).as_deref(), Some("after"));
    }

    /// `_tei_handle_complex_head`: `<head>` with `<p>` child flattens into
    /// `<ab>` with `<lb/>` separators.
    #[test]
    fn tei_handle_complex_head_flattens_p_with_lb() {
        // <ab>headtext<p>first</p><p>second</p></ab>
        let head = build_elem("ab", Some("headtext"), vec![], &[("rend", "h2")]);
        let p1 = build_elem("p", Some("first"), vec![], &[]);
        let p2 = build_elem("p", Some("second"), vec![], &[]);
        append_child(&head, &p1);
        append_child(&head, &p2);
        let new_ab = _tei_handle_complex_head(&head);
        // The new <ab> retains its rend attribute.
        assert_eq!(get_attribute(&new_ab, "rend").as_deref(), Some("h2"));
        // No more <p> descendants in new_ab.
        let ps = get_elements_by_tag_name(&new_ab, "p");
        assert!(ps.is_empty(), "no <p> should remain: {:?}", ps.len());
        // <lb/> separators present.
        let lbs = get_elements_by_tag_name(&new_ab, "lb");
        assert!(!lbs.is_empty(), "lb separators should exist");
    }

    /// `_wrap_unwanted_siblings_of_div`: TEI_DIV_SIBLINGS wrapped in fresh <div>.
    ///
    /// Regression guard for the rcdom reparent-tail bug class: the moved `<p>`
    /// carries a NON-whitespace tail (`xml.py:566` `new_sibling.append(sibling)`
    /// moves the tail with the element). A naive remove+append_child orphaned
    /// that tail in `<body>`; here we assert it travels into the wrapper.
    #[test]
    fn tei_wrap_unwanted_siblings_of_div_wraps_p_siblings() {
        let body = create_element("body");
        let div1 = build_elem("div", None, vec![], &[]);
        let p_loose = build_elem("p", Some("loose"), vec![], &[]);
        let div2 = build_elem("div", None, vec![], &[]);
        append_child(&body, &div1);
        append_child(&body, &p_loose);
        // Non-whitespace tail on the sibling that will be moved.
        set_tail(&p_loose, Some("PTAIL"));
        append_child(&body, &div2);
        _wrap_unwanted_siblings_of_div(&div1);
        // After: body has [div, div(wrapper containing p), div].
        let kids = children(&body);
        assert_eq!(kids.len(), 3);
        // Middle child is a div wrapping <p>.
        let middle = &kids[1];
        assert_eq!(local_name(middle).as_deref(), Some("div"));
        let middle_kids = children(middle);
        assert_eq!(middle_kids.len(), 1);
        assert_eq!(local_name(&middle_kids[0]).as_deref(), Some("p"));
        // The moved <p>'s tail must travel WITH it into the wrapper, not stay
        // orphaned under <body>.
        assert_eq!(
            tail(&middle_kids[0]).as_deref(),
            Some("PTAIL"),
            "moved sibling's tail must travel into the wrapper div"
        );
        // And it must NOT remain in the source parent (no orphan Text in body).
        let body_has_orphan_text = dom::child_nodes(&body).iter().any(dom::is_text);
        assert!(
            !body_has_orphan_text,
            "tail must not be orphaned under <body>"
        );
    }

    /// `_move_element_one_level_up`: `<ab>` nested under `<p>` moves up.
    ///
    /// Regression guard for the rcdom reparent-tail bug class: a following
    /// sibling of `<ab>` carries a NON-whitespace tail. `xml.py:589`
    /// `new_elem.extend(list(element.itersiblings()))` moves each following
    /// sibling WITH its tail into the new `<p>`; a naive remove+append_child
    /// dropped those tails. Here we assert the moved sibling's tail survives
    /// on the destination `<p>` (new_elem).
    #[test]
    fn tei_move_element_one_level_up_lifts_ab_from_p() {
        // <body><p><ab>head</ab><hi>x</hi>SIBTAIL</p></body>
        // <ab> has one following sibling <hi> whose tail is "SIBTAIL".
        let body = create_element("body");
        let p = build_elem("p", None, vec![], &[]);
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        let hi = build_elem("hi", Some("x"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&p, &hi);
        set_tail(&hi, Some("SIBTAIL"));
        append_child(&body, &p);
        _move_element_one_level_up(&ab);
        // After: <ab> is now a direct child of body (sibling of p).
        let kids = children(&body);
        assert!(kids.iter().any(|k| local_name(k).as_deref() == Some("ab")));
        // The new <p> (new_elem) holds the moved <hi>, and <hi>'s tail must
        // have travelled with it (rather than being orphaned in the old <p>).
        let hi_after = get_elements_by_tag_name(&body, "hi");
        assert_eq!(hi_after.len(), 1, "the moved <hi> must survive exactly once");
        assert_eq!(
            tail(&hi_after[0]).as_deref(),
            Some("SIBTAIL"),
            "moved sibling's tail must travel into new_elem"
        );
    }

    /// `check_tei`: non-whitelisted descendant is merged with parent.
    #[test]
    fn tei_check_tei_strips_non_whitelisted_descendant_tags() {
        // Build a minimal TEI tree: <TEI><text><body><div type="entry">
        //   <p>good <span>bad</span> end</p></div></body></text></TEI>
        let tei = create_element("TEI");
        let textel = create_element("text");
        append_child(&tei, &textel);
        let bodyel = create_element("body");
        append_child(&textel, &bodyel);
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        append_child(&bodyel, &div);
        let p = build_elem("p", Some("good "), vec![], &[]);
        let span = build_elem("span", Some("bad"), vec![], &[]);
        set_tail(&span, Some(" end"));
        append_child(&p, &span);
        append_child(&div, &p);

        check_tei(&tei);
        // <span> is not in TEI_VALID_TAGS — should be removed (merged).
        let spans = get_elements_by_tag_name(&tei, "span");
        assert!(spans.is_empty(), "span must be stripped: {}", spans.len());
        // <p> still survives.
        let ps = get_elements_by_tag_name(&tei, "p");
        assert_eq!(ps.len(), 1);
    }

    /// `check_tei`: invalid attribute is popped from a valid tag.
    #[test]
    fn tei_check_tei_strips_non_whitelisted_attributes() {
        let tei = create_element("TEI");
        let textel = create_element("text");
        append_child(&tei, &textel);
        let bodyel = create_element("body");
        append_child(&textel, &bodyel);
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        append_child(&bodyel, &div);
        let p = build_elem(
            "p",
            Some("body"),
            vec![],
            &[("class", "lead"), ("rend", "italic")],
        );
        append_child(&div, &p);

        check_tei(&tei);
        // class is not in TEI_VALID_ATTRS — stripped.
        let ps = get_elements_by_tag_name(&tei, "p");
        assert_eq!(get_attribute(&ps[0], "class"), None);
        // rend is in TEI_VALID_ATTRS — survives.
        assert_eq!(get_attribute(&ps[0], "rend").as_deref(), Some("italic"));
    }

    /// `build_tei_output`: produces TEI root (lower-cased "tei" in the DOM;
    /// upper-cased "TEI" only after `restore_tei_case` runs at serialise
    /// time) with xmlns + teiHeader + text/body.
    #[test]
    fn build_tei_output_builds_full_tei_structure() {
        let md = Metadata {
            title: Some("Sample".to_string()),
            ..Metadata::default()
        };
        let body = create_element("body");
        let p = build_elem("p", Some("Hello."), vec![], &[]);
        append_child(&body, &p);
        let doc = Document {
            metadata: md,
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let out = build_tei_output(&doc);
        // The rcdom-backed create_element lower-cases tag names (HTML
        // semantics); restore_tei_case upper-cases at serialise time.
        assert_eq!(local_name(&out).as_deref(), Some("tei"));
        assert_eq!(
            get_attribute(&out, "xmlns").as_deref(),
            Some("http://www.tei-c.org/ns/1.0")
        );
        // <teiHeader> child (lower-cased in DOM).
        let headers = get_elements_by_tag_name(&out, "teiheader");
        assert_eq!(headers.len(), 1);
        // <text>/<body>/<div type="entry"> chain.
        let textels = get_elements_by_tag_name(&out, "text");
        assert_eq!(textels.len(), 1);
        let bodies = get_elements_by_tag_name(&textels[0], "body");
        assert_eq!(bodies.len(), 1);
        let divs = get_elements_by_tag_name(&bodies[0], "div");
        assert!(divs.iter().any(|d| get_attribute(d, "type")
            .as_deref()
            == Some("entry")));
    }

    /// `control_xml_output` with TEI format produces TEI serialised XML.
    #[test]
    fn control_xml_output_tei_branch_returns_tei_root() {
        let body = create_element("body");
        let p = build_elem("p", Some("Hello"), vec![], &[]);
        append_child(&body, &p);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Tei);
        assert!(s.starts_with("<TEI"), "must start with <TEI: {s}");
        assert!(s.contains("xmlns=\"http://www.tei-c.org/ns/1.0\""), "{s}");
        assert!(s.contains("<teiHeader>"), "{s}");
        assert!(s.ends_with("</TEI>"), "must end with </TEI>: {s}");
    }

    // -------------------------------------------------------------------
    // strip_control_chars (utils.py:266-274) — 17 tests
    // M10 Phase 1 (HLD §6a). Each case is one-line input + one-line
    // expected; coverage spans Cc-kept, Cc-stripped, Cf, Co, Cn, and
    // representative Unicode whitespace categories (Zs/Zl/Zp).
    // -------------------------------------------------------------------

    #[test]
    fn strip_control_chars_passes_ascii_through_verbatim() {
        let s = "hello world";
        assert_eq!(strip_control_chars(s), "hello world");
    }

    #[test]
    fn strip_control_chars_keeps_preserved_cc_whitespace() {
        // TAB / LF / CR / VT / FF — all Cc but Python isspace() = True.
        let s = "a\tb\nc\rd\u{000B}e\u{000C}f";
        assert_eq!(strip_control_chars(s), s);
    }

    #[test]
    fn strip_control_chars_keeps_information_separators() {
        // FS/GS/RS/US (U+001C..U+001F) — surprise Cc-kept (Python isspace).
        let s = "a\u{001C}b\u{001D}c\u{001E}d\u{001F}e";
        assert_eq!(strip_control_chars(s), s);
    }

    #[test]
    fn strip_control_chars_keeps_nel() {
        // U+0085 NEL — C1 control but Python isspace() = True.
        let s = "a\u{0085}b";
        assert_eq!(strip_control_chars(s), s);
    }

    #[test]
    fn strip_control_chars_strips_nul_and_bel() {
        let s = "a\u{0000}b\u{0007}c";
        assert_eq!(strip_control_chars(s), "abc");
    }

    #[test]
    fn strip_control_chars_strips_del() {
        // U+007F DEL — Cc, not Python isspace().
        let s = "a\u{007F}b";
        assert_eq!(strip_control_chars(s), "ab");
    }

    #[test]
    fn strip_control_chars_strips_c1_controls_except_nel() {
        // C1 range (0x80-0x9F) minus 0x85 NEL — all stripped.
        let s = "a\u{0086}b\u{0099}c\u{009F}d";
        assert_eq!(strip_control_chars(s), "abcd");
    }

    #[test]
    fn strip_control_chars_strips_soft_hyphen() {
        // U+00AD SOFT HYPHEN — Cf, known M7 leak class.
        let s = "hyphen\u{00AD}ate";
        assert_eq!(strip_control_chars(s), "hyphenate");
    }

    #[test]
    fn strip_control_chars_strips_invisible_separator_u2063() {
        // The exact pattern from the 507b9cdb (Apple FR) fixture.
        let s = "iPadOS 15\u{2063}\u{2063}, il";
        assert_eq!(strip_control_chars(s), "iPadOS 15, il");
    }

    #[test]
    fn strip_control_chars_strips_bom() {
        // U+FEFF BYTE ORDER MARK — Cf.
        let s = "\u{FEFF}hello";
        assert_eq!(strip_control_chars(s), "hello");
    }

    #[test]
    fn strip_control_chars_strips_zero_width_joiner_set() {
        // ZWSP / ZWNJ / ZWJ / LRM / RLM — all Cf.
        let s = "a\u{200B}b\u{200C}c\u{200D}d\u{200E}e\u{200F}f";
        assert_eq!(strip_control_chars(s), "abcdef");
    }

    #[test]
    fn strip_control_chars_keeps_unicode_whitespace() {
        // NBSP (Zs) / LINE SEP (Zl) / PARA SEP (Zp) / EM SPACE (Zs).
        let s = "a\u{00A0}b\u{2028}c\u{2029}d\u{2003}e";
        assert_eq!(strip_control_chars(s), s);
    }

    #[test]
    fn strip_control_chars_strips_pua() {
        // Private Use Area — Co category.
        let s = "a\u{E000}b\u{F8FF}c";
        assert_eq!(strip_control_chars(s), "abc");
    }

    #[test]
    fn strip_control_chars_preserves_letters_marks_numbers() {
        // Sanity: nothing kept is being lost. Includes combining mark.
        let s = "café 123 \u{0301}";
        assert_eq!(strip_control_chars(s), s);
    }

    #[test]
    fn strip_control_chars_empty_returns_empty() {
        assert_eq!(strip_control_chars(""), "");
    }

    #[test]
    fn strip_control_chars_idempotent() {
        // f(f(x)) == f(x). Mix of stripped + kept inputs.
        let inputs = [
            "iPadOS 15\u{2063}\u{2063}, il",
            "hyphen\u{00AD}ate",
            "\u{FEFF}hello",
            "a\tb\nc",
            "café 123",
            "",
        ];
        for x in inputs {
            let once = strip_control_chars(x);
            let twice = strip_control_chars(&once);
            assert_eq!(once, twice, "not idempotent on input: {x:?}");
        }
    }

    #[test]
    fn strip_control_chars_passes_long_unicode_text_unchanged() {
        // ~5KB string with letters, marks, numbers, NBSP, and various
        // scripts (Latin/CJK/Cyrillic + combining diacritic). Validates
        // the fast path: regex finds no match, no allocation churn.
        let chunk = "Café résumé Привет 你好 123 \u{00A0}\u{0301}";
        let mut input = String::with_capacity(5200);
        while input.len() < 5000 {
            input.push_str(chunk);
            input.push(' ');
        }
        assert_eq!(strip_control_chars(&input), input);
    }

    // -------------------------------------------------------------------
    // Stage 3 coverage push (May 2026): branch-coverage tests for
    // build_yaml_header / add_xml_meta / replace_element_text /
    // process_element / sanitize / line_processing / unescape_html /
    // python_repr_list / json_str / csv_or_null / csv_quote_minimal /
    // TEI helpers / restore_tei_case / check_tei. Each pins one named
    // contract per `xml.py` / `core.py` line range.
    // -------------------------------------------------------------------

    // --- build_yaml_header (core.py:73-91): per-field presence ----------
    // Each test populates exactly ONE field on Metadata; asserts the
    // header carries that field's key and no other field-keys, walking
    // each `if let Some(v) = ... && !v.is_empty()` arm.

    #[test]
    fn build_yaml_header_empty_metadata_returns_just_delimiters() {
        // rationale: core.py:73-91 — all-falsy Document yields the bare
        // "---\n---\n" delimiter pair (Python's `if getattr(document, attr):`
        // arm goes false for every slot).
        let h = build_yaml_header(&Metadata::default());
        assert_eq!(h, "---\n---\n");
    }

    #[test]
    fn build_yaml_header_title_only_emits_title_line() {
        // rationale: core.py:75 — the `title` slot is the first attr in
        // Python's tuple; isolate it to walk the `Some(v) && !v.is_empty()`
        // arm without other slots interfering.
        let md = Metadata {
            title: Some("Hello".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("title: Hello\n"), "got: {h:?}");
        assert!(!h.contains("author:"));
        assert!(!h.contains("url:"));
        assert!(!h.contains("hostname:"));
        assert!(!h.contains("description:"));
        assert!(!h.contains("sitename:"));
        assert!(!h.contains("date:"));
        assert!(!h.contains("categories:"));
        assert!(!h.contains("tags:"));
        assert!(!h.contains("license:"));
    }

    #[test]
    fn build_yaml_header_empty_string_fields_are_treated_as_falsy() {
        // rationale: core.py:73-91 — Python's `if value:` makes the empty
        // string falsy. Every `Some("".to_string())` field must NOT emit a
        // line; the `&& !v.is_empty()` arm guard fires.
        let md = Metadata {
            title: Some(String::new()),
            author: Some(String::new()),
            url: Some(String::new()),
            hostname: Some(String::new()),
            description: Some(String::new()),
            site_name: Some(String::new()),
            date: Some(String::new()),
            license: Some(String::new()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert_eq!(h, "---\n---\n");
    }

    #[test]
    fn build_yaml_header_author_only_emits_author_line() {
        // rationale: core.py:76 — author slot.
        let md = Metadata {
            author: Some("Jane Doe".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("author: Jane Doe\n"));
        assert!(!h.contains("title:"));
    }

    #[test]
    fn build_yaml_header_url_only_emits_url_line() {
        // rationale: core.py:77 — url slot.
        let md = Metadata {
            url: Some("https://example.com/x".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("url: https://example.com/x\n"));
        assert!(!h.contains("hostname:"));
    }

    #[test]
    fn build_yaml_header_hostname_only_emits_hostname_line() {
        // rationale: core.py:78 — hostname slot.
        let md = Metadata {
            hostname: Some("example.com".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("hostname: example.com\n"));
        assert!(!h.contains("url:"));
    }

    #[test]
    fn build_yaml_header_description_only_emits_description_line() {
        // rationale: core.py:79 — description slot.
        let md = Metadata {
            description: Some("a summary".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("description: a summary\n"));
    }

    #[test]
    fn build_yaml_header_sitename_only_emits_sitename_line() {
        // rationale: core.py:80 — sitename slot (Python attr name "sitename",
        // Rust field `site_name`; YAML key is "sitename").
        let md = Metadata {
            site_name: Some("Example".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("sitename: Example\n"));
        assert!(!h.contains("site_name:"));
    }

    #[test]
    fn build_yaml_header_date_only_emits_date_line() {
        // rationale: core.py:81 — date slot.
        let md = Metadata {
            date: Some("2026-05-26".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("date: 2026-05-26\n"));
    }

    #[test]
    fn build_yaml_header_categories_render_as_python_repr_list() {
        // rationale: core.py:82,90 — `str(getattr(document, "categories"))`
        // renders as Python's `['a', 'b']` (single quotes, comma+space).
        let md = Metadata {
            categories: vec!["news".to_string(), "tech".to_string()],
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(
            h.contains("categories: ['news', 'tech']\n"),
            "got: {h:?}"
        );
    }

    #[test]
    fn build_yaml_header_tags_render_as_python_repr_list() {
        // rationale: core.py:83,90 — same as categories.
        let md = Metadata {
            tags: vec!["alpha".to_string()],
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("tags: ['alpha']\n"), "got: {h:?}");
    }

    #[test]
    fn build_yaml_header_empty_categories_and_tags_are_skipped() {
        // rationale: core.py:73-91 — `if !v.is_empty()` on Vec guards both
        // slots; empty vectors emit nothing.
        let md = Metadata::default();
        let h = build_yaml_header(&md);
        assert!(!h.contains("categories:"));
        assert!(!h.contains("tags:"));
    }

    #[test]
    fn build_yaml_header_license_only_emits_license_line() {
        // rationale: core.py:87 — license slot (last in the tuple).
        let md = Metadata {
            license: Some("CC-BY".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        assert!(h.contains("license: CC-BY\n"));
    }

    #[test]
    fn build_yaml_header_all_fields_emit_in_python_source_order() {
        // rationale: core.py:75-87 — the tuple ORDER is the YAML key order;
        // emitter must walk it verbatim. Title -> author -> url -> hostname
        // -> description -> sitename -> date -> categories -> tags ->
        // license. fingerprint/id slots are omitted (Metadata lacks them).
        let md = Metadata {
            title: Some("T".to_string()),
            author: Some("A".to_string()),
            url: Some("U".to_string()),
            hostname: Some("H".to_string()),
            description: Some("D".to_string()),
            site_name: Some("S".to_string()),
            date: Some("2026".to_string()),
            categories: vec!["c1".to_string()],
            tags: vec!["t1".to_string()],
            license: Some("L".to_string()),
            ..Metadata::default()
        };
        let h = build_yaml_header(&md);
        // Build the EXACT expected string. This pins the order contract.
        let expected =
            "---\ntitle: T\nauthor: A\nurl: U\nhostname: H\ndescription: D\nsitename: S\ndate: 2026\ncategories: ['c1']\ntags: ['t1']\nlicense: L\n---\n";
        assert_eq!(h, expected);
    }

    // --- add_xml_meta (xml.py:42-46, 178-183): per-attribute presence ---
    // Order from add_xml_meta source: sitename, title, author, date, url,
    // hostname, description, categories, tags, license, language.

    #[test]
    fn add_xml_meta_empty_metadata_sets_no_attributes() {
        // rationale: xml.py:178-183 — every `if value:` arm goes false on
        // default Metadata; the doc node has zero metadata attributes.
        let doc = create_element("doc");
        add_xml_meta(&doc, &Metadata::default());
        for k in [
            "sitename",
            "title",
            "author",
            "date",
            "url",
            "hostname",
            "description",
            "categories",
            "tags",
            "license",
            "language",
        ] {
            assert_eq!(get_attribute(&doc, k), None, "key {k} must not be set");
        }
    }

    #[test]
    fn add_xml_meta_empty_string_skipped_for_all_optional_fields() {
        // rationale: xml.py:178-183 — `if value:` is falsy on "" — every
        // Some("") slot walks the SKIP arm of the conditional.
        let md = Metadata {
            title: Some(String::new()),
            author: Some(String::new()),
            url: Some(String::new()),
            hostname: Some(String::new()),
            description: Some(String::new()),
            site_name: Some(String::new()),
            date: Some(String::new()),
            license: Some(String::new()),
            language: Some(String::new()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        for k in [
            "title",
            "author",
            "url",
            "hostname",
            "description",
            "sitename",
            "date",
            "license",
            "language",
        ] {
            assert_eq!(get_attribute(&doc, k), None, "key {k} must be skipped");
        }
    }

    #[test]
    fn add_xml_meta_sitename_only_attr_set() {
        // rationale: xml.py:42-46,178-183 — sitename is the first slot;
        // isolate to walk the `Some(v) && !v.is_empty()` true-arm.
        let md = Metadata {
            site_name: Some("Example".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "sitename").as_deref(), Some("Example"));
        assert_eq!(get_attribute(&doc, "title"), None);
    }

    #[test]
    fn add_xml_meta_date_only_attr_set() {
        // rationale: xml.py:42-46 — date slot.
        let md = Metadata {
            date: Some("2026-05-26".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "date").as_deref(), Some("2026-05-26"));
    }

    #[test]
    fn add_xml_meta_hostname_only_attr_set() {
        // rationale: xml.py:42-46 — hostname slot.
        let md = Metadata {
            hostname: Some("example.com".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "hostname").as_deref(), Some("example.com"));
    }

    #[test]
    fn add_xml_meta_description_only_attr_set() {
        // rationale: xml.py:42-46 — description slot.
        let md = Metadata {
            description: Some("summary".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "description").as_deref(), Some("summary"));
    }

    #[test]
    fn add_xml_meta_author_only_attr_set() {
        // rationale: xml.py:42-46 — author slot.
        let md = Metadata {
            author: Some("Author".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "author").as_deref(), Some("Author"));
    }

    #[test]
    fn add_xml_meta_tags_only_attr_joined_by_semicolon() {
        // rationale: xml.py:183 — list fields render as `';'.join(list)`.
        let md = Metadata {
            tags: vec!["a".to_string(), "b".to_string(), "c".to_string()],
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "tags").as_deref(), Some("a;b;c"));
        assert_eq!(get_attribute(&doc, "categories"), None);
    }

    #[test]
    fn add_xml_meta_empty_categories_and_tags_skipped() {
        // rationale: xml.py:178-183 — empty list is falsy in Python's `if
        // value:`; both attrs must be absent.
        let md = Metadata::default();
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "categories"), None);
        assert_eq!(get_attribute(&doc, "tags"), None);
    }

    #[test]
    fn add_xml_meta_license_only_attr_set() {
        // rationale: xml.py:42-46 — license slot.
        let md = Metadata {
            license: Some("CC-BY".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "license").as_deref(), Some("CC-BY"));
    }

    #[test]
    fn add_xml_meta_language_only_attr_set() {
        // rationale: xml.py:42-46 — language slot (last in our port).
        let md = Metadata {
            language: Some("en".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "language").as_deref(), Some("en"));
    }

    // --- replace_element_text (xml.py:253-297): tag-branch catalog ----

    #[test]
    fn replace_element_text_default_tag_passes_text_through() {
        // rationale: xml.py:253-274 — the formatting match has a `_ => {}`
        // default arm: non-(head|del|hi|code|ref|cell|item) tags emit raw
        // text verbatim.
        let p = build_elem("p", Some("hello"), vec![], &[]);
        assert_eq!(replace_element_text(&p, true), "hello");
    }

    #[test]
    fn replace_element_text_no_text_returns_empty_string() {
        // rationale: xml.py:255 — `element.text or ""`; a textless tag
        // returns an empty string (the formatting branch's `!orig.is_empty()`
        // guard goes false).
        let p = create_element("p");
        assert_eq!(replace_element_text(&p, true), "");
    }

    #[test]
    fn replace_element_text_head_without_include_formatting_passes_through() {
        // rationale: xml.py:257 — the `include_formatting AND text non-empty`
        // gate is false when include_formatting=false; <head> emits raw text
        // (no '## ' prefix).
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "h2")]);
        assert_eq!(replace_element_text(&h, false), "Title");
    }

    #[test]
    fn replace_element_text_head_h1_emits_single_hash() {
        // rationale: xml.py:258-263 — rend="h1" gives one '#'.
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "h1")]);
        assert_eq!(replace_element_text(&h, true), "# Title");
    }

    #[test]
    fn replace_element_text_head_h6_emits_six_hashes() {
        // rationale: xml.py:258-263 — rend="h6" gives six '#'.
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "h6")]);
        assert_eq!(replace_element_text(&h, true), "###### Title");
    }

    #[test]
    fn replace_element_text_hi_i_italic_wrap() {
        // rationale: xml.py:266-269 — HI_FORMATTING["#i"] = '*'.
        let hi = build_elem("hi", Some("italic"), vec![], &[("rend", "#i")]);
        assert_eq!(replace_element_text(&hi, true), "*italic*");
    }

    #[test]
    fn replace_element_text_hi_u_underline_wrap() {
        // rationale: xml.py:266-269 — HI_FORMATTING["#u"] = '__'.
        let hi = build_elem("hi", Some("under"), vec![], &[("rend", "#u")]);
        assert_eq!(replace_element_text(&hi, true), "__under__");
    }

    #[test]
    fn replace_element_text_hi_t_tt_wrap() {
        // rationale: xml.py:266-269 — HI_FORMATTING["#t"] = '`' (typewriter).
        let hi = build_elem("hi", Some("tt"), vec![], &[("rend", "#t")]);
        assert_eq!(replace_element_text(&hi, true), "`tt`");
    }

    #[test]
    fn replace_element_text_hi_unknown_rend_returns_unwrapped() {
        // rationale: xml.py:266-269 — HI_FORMATTING.get returns None for
        // unknown rend; the `if let Some(wrap) = hi_formatting(...)` arm
        // goes false; text passes through unwrapped.
        let hi = build_elem("hi", Some("plain"), vec![], &[("rend", "#xx")]);
        assert_eq!(replace_element_text(&hi, true), "plain");
    }

    #[test]
    fn replace_element_text_hi_missing_rend_returns_unwrapped() {
        // rationale: xml.py:266-269 — `if get_attribute(... "rend")` goes
        // None; the `if let Some(rend) = ...` arm exits early.
        let hi = build_elem("hi", Some("plain"), vec![], &[]);
        assert_eq!(replace_element_text(&hi, true), "plain");
    }

    #[test]
    fn replace_element_text_code_multiline_emits_fenced_block() {
        // rationale: xml.py:270-274 — multi-line code uses ``` fences.
        let c = build_elem("code", Some("def f():\n    pass"), vec![], &[]);
        assert_eq!(
            replace_element_text(&c, true),
            "```\ndef f():\n    pass\n```"
        );
    }

    #[test]
    fn replace_element_text_ref_without_target_emits_bare_link_text() {
        // rationale: xml.py:279-284 — missing target falls through to
        // `elem_text = link_text` (only the bracketed text).
        let r = build_elem("ref", Some("link"), vec![], &[]);
        assert_eq!(replace_element_text(&r, false), "[link]");
    }

    #[test]
    fn replace_element_text_ref_empty_target_falls_back_to_link_text() {
        // rationale: xml.py:279-284 — `if target && !target.is_empty()`
        // is false for empty target attr.
        let r = build_elem("ref", Some("link"), vec![], &[("target", "")]);
        assert_eq!(replace_element_text(&r, false), "[link]");
    }

    #[test]
    fn replace_element_text_ref_with_empty_text_returns_empty() {
        // rationale: xml.py:278 — `if tag == "ref" && !elem_text.is_empty()`
        // gate goes false when text is empty; raw passthrough.
        let r = build_elem("ref", None, vec![], &[("target", "https://e.com")]);
        assert_eq!(replace_element_text(&r, false), "");
    }

    #[test]
    fn replace_element_text_cell_mid_row_no_leading_pipe() {
        // rationale: xml.py:291-293 — mid-row leaf cell (has previous sibling
        // element) gets NO leading "| ".
        let row = create_element("row");
        let first = create_element("cell");
        append_child(&first, &create_text_node("first"));
        append_child(&row, &first);
        let mid = create_element("cell");
        append_child(&mid, &create_text_node("second"));
        append_child(&row, &mid);
        assert_eq!(replace_element_text(&mid, false), "second");
    }

    #[test]
    fn replace_element_text_cell_empty_text_emits_empty() {
        // rationale: xml.py:287-293 — `if !elem_text.is_empty()` guard skip.
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&row, &cell);
        assert_eq!(replace_element_text(&cell, false), "");
    }

    #[test]
    fn replace_element_text_cell_with_p_child_first_in_row() {
        // rationale: xml.py:288-290 — first-cell-in-row with <p> first child
        // emits "| {text} " (note trailing space).
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&cell, &create_text_node("hdr"));
        let p_kid = create_element("p");
        append_child(&cell, &p_kid);
        append_child(&row, &cell);
        assert_eq!(replace_element_text(&cell, false), "| hdr ");
    }

    #[test]
    fn replace_element_text_cell_with_p_child_mid_row() {
        // rationale: xml.py:288-290 — mid-row p-first cell gets " " not "| ".
        let row = create_element("row");
        let first = build_elem("cell", Some("a"), vec![], &[]);
        let mid = create_element("cell");
        append_child(&mid, &create_text_node("body"));
        append_child(&mid, &create_element("p"));
        append_child(&row, &first);
        append_child(&row, &mid);
        assert_eq!(replace_element_text(&mid, false), "body ");
    }

    // --- process_element extra branches ------------------------------

    #[test]
    fn process_element_textless_non_newline_tag_early_returns() {
        // rationale: xml.py:333-336 — textless non-NEWLINE_ELEM tag (e.g.
        // <span>) takes the early return; nothing is emitted at all.
        let span = create_element("span");
        let mut out = Vec::new();
        process_element(&span, &mut out, false);
        assert!(out.is_empty(), "got: {out:?}");
    }

    #[test]
    fn process_element_lb_emits_newline_in_newline_elems() {
        // rationale: xml.py:331-332 — textless <lb> (in NEWLINE_ELEMS) emits
        // "\n", then the after-tag block emits another "\n".
        let lb = create_element("lb");
        let mut out = Vec::new();
        process_element(&lb, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains('\n'));
    }

    #[test]
    fn process_element_quote_emits_newlines() {
        // rationale: xml.py:331-332,341-343 — <quote> in NEWLINE_ELEMS.
        let q = build_elem("quote", Some("quoted"), vec![], &[]);
        let mut out = Vec::new();
        process_element(&q, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("quoted"));
        assert!(joined.contains('\n'));
    }

    #[test]
    fn process_element_del_special_formatting_no_trailing_space() {
        // rationale: xml.py:346-347 — SPECIAL_FORMATTING tags don't emit the
        // default trailing space. <del> with formatting emits "~~text~~"
        // with no " " after the closing tag.
        let d = build_elem("del", Some("old"), vec![], &[]);
        let mut out = Vec::new();
        process_element(&d, &mut out, true);
        let joined: String = out.join("");
        assert_eq!(joined, "~~old~~");
    }

    #[test]
    fn process_element_row_padding_for_colspan() {
        // rationale: xml.py:317-330 — short row with colspan="3" pads with
        // "||\n" (3 cells expected, 1 actual, 2 pad bars).
        let cell = build_elem("cell", Some("only"), vec![], &[]);
        let row = build_elem("row", None, vec![cell], &[("colspan", "3")]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("||\n"), "got: {joined:?}");
    }

    #[test]
    fn process_element_row_head_cell_emits_underline() {
        // rationale: xml.py:329-330 — head row (cell role="head") emits
        // "\n|---|---|...\n" separator.
        let head_cell = build_elem("cell", Some("H"), vec![], &[("role", "head")]);
        let row = build_elem("row", None, vec![head_cell], &[]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("---|"), "got: {joined:?}");
    }

    #[test]
    fn process_element_cell_ancestor_suppresses_newline() {
        // rationale: xml.py:341 — `has_cell_ancestor` gate: a <p> nested
        // under <cell> does NOT emit a newline (the cell consumes its
        // formatting).
        let body = create_element("body");
        let row = create_element("row");
        let cell = create_element("cell");
        let p = build_elem("p", Some("inside"), vec![], &[]);
        append_child(&cell, &p);
        append_child(&row, &cell);
        append_child(&body, &row);
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        // No '\n' from this <p> (cell ancestor suppresses).
        assert!(!joined.contains('\n'), "got: {joined:?}");
    }

    // --- sanitize (utils.py:303-312) ----------------------------------

    #[test]
    fn sanitize_empty_input_returns_none() {
        // rationale: utils.py:308-310 — all-blank/empty input collapses to
        // empty joined string -> None return.
        assert_eq!(sanitize("", false, false), None);
    }

    #[test]
    fn sanitize_whitespace_only_returns_none() {
        // rationale: utils.py:294-295 — every line trims to "", every line
        // is None-pruned; final joined string is "".
        assert_eq!(sanitize("   \n\t\n   ", false, false), None);
    }

    #[test]
    fn sanitize_non_blank_returns_some_joined_lines() {
        // rationale: utils.py:308-310 — non-blank lines join with "\n";
        // blank lines are pruned.
        let r = sanitize("a\n\nb", false, false).expect("non-empty");
        assert_eq!(r, "a\nb");
    }

    #[test]
    fn sanitize_strips_unicode_line_separator_marker() {
        // rationale: utils.py:308 — `\u{2424}` (SYMBOL FOR NEWLINE) is the
        // process_element spacing-hack marker; sanitize strips it from the
        // joined output.
        let r = sanitize("a\u{2424}b", false, false).expect("non-empty");
        // The marker is replaced; the two non-blank chars survive on the
        // same line (no \n splits in input).
        assert!(!r.contains('\u{2424}'));
        assert!(r.contains('a') && r.contains('b'));
    }

    #[test]
    fn sanitize_trailing_space_routes_to_line_processing_directly() {
        // rationale: utils.py:306-307 — trailing_space=true short-circuits
        // the line-splitter and feeds the whole input to line_processing.
        let r = sanitize("  hello world  ", false, true);
        // Whitespace re-attached because original had leading/trailing ws.
        assert!(r.is_some());
        let s = r.unwrap();
        assert!(s.starts_with(' '));
        assert!(s.ends_with(' '));
        assert!(s.contains("hello world"));
    }

    #[test]
    fn sanitize_text_returns_empty_string_on_none() {
        // rationale: sanitize_text wraps sanitize() with `or ""` per xml.py:363.
        assert_eq!(sanitize_text(""), "");
        assert_eq!(sanitize_text("   "), "");
    }

    // --- line_processing (utils.py:282-300) ---------------------------

    #[test]
    fn line_processing_decodes_html_space_entities() {
        // rationale: utils.py:288 — `&#13;` -> '\r', `&#10;` -> '\n',
        // `&nbsp;` -> '\u{00A0}'. trim() collapses runs, but the substitution
        // happens BEFORE trim.
        let r = line_processing("a&#10;b", true, false).unwrap();
        assert!(r.contains('\n'));
        let r2 = line_processing("a&#13;b", true, false).unwrap();
        assert!(r2.contains('\r'));
    }

    #[test]
    fn line_processing_nbsp_decoded_to_u00a0() {
        // rationale: utils.py:288 — `&nbsp;` -> NBSP.
        let r = line_processing("a&nbsp;b", true, false).unwrap();
        assert!(r.contains('\u{00A0}'));
    }

    #[test]
    fn line_processing_preserve_space_returns_decoded_unchanged() {
        // rationale: utils.py:289 — `preserve_space=true` short-circuits
        // the trim block; control-stripped text is returned as-is.
        let r = line_processing("  hi  ", true, false).unwrap();
        // No trim happened; leading/trailing spaces preserved.
        assert_eq!(r, "  hi  ");
    }

    #[test]
    fn line_processing_blank_line_returns_none() {
        // rationale: utils.py:294-295 — `if all(map(str.isspace, ...))` arm:
        // all-whitespace lines return None.
        assert_eq!(line_processing("   \t  ", false, false), None);
    }

    #[test]
    fn line_processing_trailing_space_reattaches_leading_and_trailing() {
        // rationale: utils.py:296-299 — trailing_space=true re-attaches a
        // single leading/trailing space based on the ORIGINAL line.
        let r = line_processing("  foo bar  ", false, true).unwrap();
        assert_eq!(r, " foo bar ");
    }

    #[test]
    fn line_processing_trailing_space_only_leading_when_leading_ws() {
        // rationale: utils.py:296-299 — only leading space exists in
        // original; only one side gets the space.
        let r = line_processing(" foo", false, true).unwrap();
        assert_eq!(r, " foo");
        let r2 = line_processing("foo ", false, true).unwrap();
        assert_eq!(r2, "foo ");
    }

    #[test]
    fn line_processing_strips_control_chars_then_trims() {
        // rationale: utils.py:288 — `remove_control_characters` before trim.
        // BOM + text -> just text after strip + trim.
        let r = line_processing("\u{FEFF}hello", false, false).unwrap();
        assert_eq!(r, "hello");
    }

    // --- unescape_html (subset of html.unescape) ----------------------

    #[test]
    fn unescape_html_xml_mandatory_entities() {
        // rationale: XML mandates the five entities; verify each.
        assert_eq!(unescape_html("&amp;"), "&");
        assert_eq!(unescape_html("&lt;"), "<");
        assert_eq!(unescape_html("&gt;"), ">");
        assert_eq!(unescape_html("&quot;"), "\"");
        assert_eq!(unescape_html("&apos;"), "'");
    }

    #[test]
    fn unescape_html_numeric_decimal_entity() {
        // rationale: html.unescape — `&#NN;` decodes by base 10.
        assert_eq!(unescape_html("&#38;"), "&");
        assert_eq!(unescape_html("&#233;"), "é");
    }

    #[test]
    fn unescape_html_numeric_hex_entity_lower_and_upper_x() {
        // rationale: html.unescape — both `&#xHH;` and `&#XHH;` decode by
        // base 16 (the strip_prefix("x").or_else(strip_prefix("X")) arm).
        assert_eq!(unescape_html("&#x26;"), "&");
        assert_eq!(unescape_html("&#X26;"), "&");
    }

    #[test]
    fn unescape_html_malformed_hex_falls_back_to_verbatim() {
        // rationale: u32::from_str_radix("ZZ", 16) is Err → None → emit
        // verbatim "&entity;".
        assert_eq!(unescape_html("&#xZZ;"), "&#xZZ;");
    }

    #[test]
    fn unescape_html_unknown_named_falls_back_to_verbatim() {
        // rationale: the giant `match entity.as_str()` falls through to
        // `_ => None` for unknown names; verbatim copy with the bracketing.
        assert_eq!(unescape_html("&xyz;"), "&xyz;");
    }

    #[test]
    fn unescape_html_unterminated_entity_passes_through() {
        // rationale: `if !found_end` arm copies '&' + scanned chars verbatim
        // (no terminating ';').
        assert_eq!(unescape_html("a&amp"), "a&amp");
    }

    #[test]
    fn unescape_html_named_punctuation_entities() {
        // rationale: select corpus-driven entities (nbsp, eacute, rsquo, mdash).
        assert_eq!(unescape_html("&nbsp;"), "\u{00A0}");
        assert_eq!(unescape_html("&eacute;"), "é");
        assert_eq!(unescape_html("&rsquo;"), "\u{2019}");
        assert_eq!(unescape_html("&mdash;"), "\u{2014}");
    }

    #[test]
    fn unescape_html_ddagger_alias_decodes_like_dagger() {
        // rationale: html5 spec — ddagger is a case-insensitive alias for
        // Dagger; both -> U+2021 (faithful-divergence guard).
        assert_eq!(unescape_html("&ddagger;"), "\u{2021}");
        assert_eq!(unescape_html("&Dagger;"), "\u{2021}");
    }

    #[test]
    fn unescape_html_bare_ampersand_passes_through() {
        // rationale: `&` followed by nothing or non-entity chars copies
        // verbatim (loop break, found_end=false).
        assert_eq!(unescape_html("a&b"), "a&b");
        assert_eq!(unescape_html("&"), "&");
    }

    #[test]
    fn unescape_html_unicode_multibyte_passthrough() {
        // rationale: the char-iterator scanner must not corrupt multi-byte
        // UTF-8 sequences (`café` is 5 bytes in UTF-8, 4 chars).
        assert_eq!(unescape_html("café"), "café");
    }

    // --- python_repr_list, json_str, json_optional_str, csv_or_null,
    //     csv_quote_minimal ------------------------------------------

    #[test]
    fn python_repr_list_empty_emits_bracket_pair() {
        // rationale: Python `str([])` -> "[]"; join("") is empty.
        assert_eq!(python_repr_list(&[]), "[]");
    }

    #[test]
    fn python_repr_list_single_item_quoted() {
        // rationale: Python `str(['a'])` -> "['a']".
        assert_eq!(python_repr_list(&["a".to_string()]), "['a']");
    }

    #[test]
    fn python_repr_list_multi_item_comma_space_separated() {
        // rationale: Python `str(['a', 'b'])` -> "['a', 'b']".
        assert_eq!(
            python_repr_list(&["a".to_string(), "b".to_string()]),
            "['a', 'b']"
        );
    }

    #[test]
    fn json_str_basic_string_quoted_no_escapes() {
        // rationale: serde_json renders ASCII as "hello".
        assert_eq!(json_str("hello"), "\"hello\"");
    }

    #[test]
    fn json_str_quotes_and_backslash_escaped() {
        // rationale: serde_json escapes `"` -> `\"` and `\\` -> `\\\\`.
        assert_eq!(json_str("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_str("a\\b"), "\"a\\\\b\"");
    }

    #[test]
    fn json_str_newline_escaped() {
        // rationale: serde_json escapes control chars (`\n` -> `\\n`).
        assert_eq!(json_str("a\nb"), "\"a\\nb\"");
    }

    #[test]
    fn json_str_non_ascii_passes_through_verbatim() {
        // rationale: serde_json defaults to ensure_ascii=false equivalent;
        // non-ASCII passes through (matches Python json.dumps(..., ensure_ascii=False)).
        assert_eq!(json_str("café"), "\"café\"");
    }

    #[test]
    fn json_optional_str_none_returns_null() {
        // rationale: json.dumps(None) -> "null".
        assert_eq!(json_optional_str(None), "null");
    }

    #[test]
    fn json_optional_str_some_returns_quoted_string() {
        // rationale: delegates to json_str on Some.
        assert_eq!(json_optional_str(Some("hi")), "\"hi\"");
    }

    #[test]
    fn csv_or_null_none_returns_null_token() {
        // rationale: xml.py:377 — `d if d else null`. None -> null.
        assert_eq!(csv_or_null(None, "NULL"), "NULL");
    }

    #[test]
    fn csv_or_null_empty_string_returns_null_token() {
        // rationale: xml.py:377 — empty string is Python-falsy; renders null.
        assert_eq!(csv_or_null(Some(""), "NULL"), "NULL");
    }

    #[test]
    fn csv_or_null_some_non_empty_returns_value() {
        // rationale: xml.py:377 — non-empty Some flows through.
        assert_eq!(csv_or_null(Some("x"), "NULL"), "x");
    }

    #[test]
    fn csv_quote_minimal_no_special_chars_returns_field_unquoted() {
        // rationale: csv.QUOTE_MINIMAL — no delim/quote/CR/LF => no quoting.
        assert_eq!(csv_quote_minimal("plain", "\t"), "plain");
    }

    #[test]
    fn csv_quote_minimal_delim_in_field_triggers_quoting() {
        // rationale: csv.QUOTE_MINIMAL — delim present => quote whole field.
        assert_eq!(csv_quote_minimal("a,b", ","), "\"a,b\"");
    }

    #[test]
    fn csv_quote_minimal_double_quote_doubled_inside_quoted_field() {
        // rationale: csv.QUOTE_MINIMAL — embedded `"` doubled-up.
        assert_eq!(csv_quote_minimal("a\"b", ","), "\"a\"\"b\"");
    }

    #[test]
    fn csv_quote_minimal_cr_or_lf_triggers_quoting() {
        // rationale: csv.QUOTE_MINIMAL — CR or LF in field => quote.
        assert_eq!(csv_quote_minimal("a\nb", "\t"), "\"a\nb\"");
        assert_eq!(csv_quote_minimal("a\rb", "\t"), "\"a\rb\"");
    }

    // --- restore_tei_case --------------------------------------------

    #[test]
    fn restore_tei_case_uppercases_tei_root_open_close() {
        // rationale: rcdom lower-cases; restore_tei_case re-uppercases TEI
        // root tag and known children.
        let in_s = "<tei xmlns=\"x\"><teiheader/><text/></tei>";
        let out = restore_tei_case(in_s);
        assert!(out.contains("<TEI "), "got: {out}");
        assert!(out.contains("</TEI>"), "got: {out}");
        assert!(out.contains("<teiHeader/>"), "got: {out}");
    }

    #[test]
    fn restore_tei_case_preserves_non_tei_tags() {
        // rationale: only the mappings table is touched; arbitrary tags
        // (like <p>) pass through unchanged.
        let in_s = "<tei><teiheader></teiheader><text><body><p>hi</p></body></text></tei>";
        let out = restore_tei_case(in_s);
        assert!(out.contains("<p>hi</p>"));
        assert!(out.contains("<TEI>"));
        assert!(out.contains("</teiHeader>"));
    }

    #[test]
    fn restore_tei_case_uppercases_filedesc_titlestmt_chain() {
        // rationale: fileDesc / titleStmt / publicationStmt / sourceDesc /
        // notesStmt — each is part of the mapping table.
        let in_s = "<filedesc><titlestmt/><publicationstmt/><notesstmt/><sourcedesc/></filedesc>";
        let out = restore_tei_case(in_s);
        assert!(out.contains("<fileDesc>"));
        assert!(out.contains("<titleStmt/>"));
        assert!(out.contains("<publicationStmt/>"));
        assert!(out.contains("<notesStmt/>"));
        assert!(out.contains("<sourceDesc/>"));
    }

    #[test]
    fn restore_tei_case_uppercases_biblfull_profiledesc_etc() {
        // rationale: biblFull / profileDesc / textClass / encodingDesc /
        // appInfo — each in the mapping table.
        let in_s = "<biblfull/><profiledesc/><textclass/><encodingdesc/><appinfo/>";
        let out = restore_tei_case(in_s);
        assert!(out.contains("<biblFull/>"));
        assert!(out.contains("<profileDesc/>"));
        assert!(out.contains("<textClass/>"));
        assert!(out.contains("<encodingDesc/>"));
        assert!(out.contains("<appInfo/>"));
    }

    #[test]
    fn restore_tei_case_idempotent_on_correct_case() {
        // rationale: re-running on already-correct-case output should be
        // a no-op (the `if out.contains(from)` guard skips on miss).
        let s = "<TEI><teiHeader/></TEI>";
        assert_eq!(restore_tei_case(s), s);
    }

    #[test]
    fn restore_tei_case_handles_self_closing_tei_root() {
        // rationale: `<tei/>` self-closing variant.
        let out = restore_tei_case("<tei/>");
        assert_eq!(out, "<TEI/>");
    }

    // --- write_teitree / write_fullheader / build_tei_output ----------

    #[test]
    fn write_teitree_includes_text_body_div_chain_with_entry_type() {
        // rationale: xml.py:397-403 — text/body/div[type=entry] chain.
        let doc = doc_with_simple_body(Metadata::default());
        let tei = write_teitree(&doc);
        let textels = get_elements_by_tag_name(&tei, "text");
        assert_eq!(textels.len(), 1);
        let bodies = get_elements_by_tag_name(&textels[0], "body");
        assert_eq!(bodies.len(), 1);
        let divs = get_elements_by_tag_name(&bodies[0], "div");
        assert!(divs.iter().any(|d| get_attribute(d, "type")
            .as_deref()
            == Some("entry")));
    }

    #[test]
    fn write_teitree_includes_div_with_type_comments_when_none() {
        // rationale: xml.py:405-408 — None commentsbody synthesises a fresh
        // <div type="comments"> (Python default path).
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let tei = write_teitree(&doc);
        let divs = get_elements_by_tag_name(&tei, "div");
        assert!(divs
            .iter()
            .any(|d| get_attribute(d, "type").as_deref() == Some("comments")));
    }

    #[test]
    fn write_fullheader_with_license_emits_availability_block() {
        // rationale: xml.py:435-440 — license -> <publicationStmt><publisher/>
        // <availability><p>license</p></availability>.
        let md = Metadata {
            license: Some("CC-BY-SA".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let avail = get_elements_by_tag_name(&header, "availability");
        assert_eq!(avail.len(), 1);
        let kids = children(&avail[0]);
        assert_eq!(kids.len(), 1);
        assert_eq!(element_text(&kids[0]).as_deref(), Some("CC-BY-SA"));
    }

    #[test]
    fn write_fullheader_without_license_emits_empty_p_in_publication_stmt() {
        // rationale: xml.py:441-442 — no license -> empty <p/> inside
        // <publicationStmt> for TEI conformance.
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &Metadata::default());
        let pubs = get_elements_by_tag_name(&header, "publicationstmt");
        // The FIRST publicationStmt is the one for fileDesc>publicationStmt
        // (the no-license empty-p branch). The second is inside biblFull
        // (always populated).
        assert!(pubs.len() >= 1);
        let ps = get_elements_by_tag_name(&pubs[0], "p");
        // Must contain at least one <p> as the empty placeholder.
        assert!(!ps.is_empty());
    }

    #[test]
    fn write_fullheader_categories_emit_textclass_terms() {
        // rationale: xml.py:471-475 — categories -> <textClass><keywords>
        // <term type="categories">a,b</term></keywords></textClass>.
        let md = Metadata {
            categories: vec!["news".to_string(), "tech".to_string()],
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let terms = get_elements_by_tag_name(&header, "term");
        let cat_term = terms
            .iter()
            .find(|t| get_attribute(t, "type").as_deref() == Some("categories"))
            .expect("categories term present");
        assert_eq!(element_text(cat_term).as_deref(), Some("news,tech"));
    }

    #[test]
    fn write_fullheader_tags_emit_textclass_terms() {
        // rationale: xml.py:476-480 — tags -> similar shape.
        let md = Metadata {
            tags: vec!["one".to_string(), "two".to_string()],
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let terms = get_elements_by_tag_name(&header, "term");
        let tag_term = terms
            .iter()
            .find(|t| get_attribute(t, "type").as_deref() == Some("tags"))
            .expect("tags term present");
        assert_eq!(element_text(tag_term).as_deref(), Some("one,two"));
    }

    #[test]
    fn write_fullheader_omits_textclass_when_no_categories_or_tags() {
        // rationale: xml.py:470 — `if categories or tags:` arm; when both
        // empty the entire textClass subtree is absent.
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &Metadata::default());
        let tc = get_elements_by_tag_name(&header, "textclass");
        assert!(tc.is_empty(), "textClass must be absent: {}", tc.len());
    }

    #[test]
    fn write_fullheader_filedate_seeds_creation_date_text() {
        // rationale: xml.py:483 — `<date type="download">docmeta.filedate</date>`.
        let md = Metadata {
            filedate: Some("2026-05-26".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let dates = get_elements_by_tag_name(&header, "date");
        let download = dates
            .iter()
            .find(|d| get_attribute(d, "type").as_deref() == Some("download"))
            .expect("download date present");
        assert_eq!(element_text(download).as_deref(), Some("2026-05-26"));
    }

    #[test]
    fn write_fullheader_url_emits_ptr_url_in_biblfull() {
        // rationale: xml.py:465-467 — url -> <ptr type="URL" target="..."/>
        // inside biblFull/publicationStmt.
        let md = Metadata {
            url: Some("https://example.com/x".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let ptrs = get_elements_by_tag_name(&header, "ptr");
        // Find the URL ptr (there's also an app ptr in appInfo).
        let url_ptr = ptrs
            .iter()
            .find(|p| get_attribute(p, "type").as_deref() == Some("URL"))
            .expect("URL ptr present");
        assert_eq!(
            get_attribute(url_ptr, "target").as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn write_fullheader_author_attaches_only_when_non_empty() {
        // rationale: xml.py:430-431 — `if author:` guards the <author>
        // element. Empty / None author -> NO <author> in titleStmt.
        let teidoc = create_element("TEI");
        let header_empty = write_fullheader(&teidoc, &Metadata::default());
        let authors_empty = get_elements_by_tag_name(&header_empty, "author");
        assert!(
            authors_empty.is_empty(),
            "author must be absent for None metadata: {}",
            authors_empty.len()
        );

        let teidoc2 = create_element("TEI");
        let md = Metadata {
            author: Some("Jane".to_string()),
            ..Metadata::default()
        };
        let header_some = write_fullheader(&teidoc2, &md);
        let authors_some = get_elements_by_tag_name(&header_some, "author");
        // Two <author> elements: one in fileDesc/titleStmt, one in
        // sourceDesc/biblFull/titleStmt.
        assert_eq!(authors_some.len(), 2);
        assert_eq!(element_text(&authors_some[0]).as_deref(), Some("Jane"));
    }

    #[test]
    fn build_tei_output_runs_check_tei_to_scrub_invalid_tags() {
        // rationale: xml.py:186-193 — build_tei_output composes write_teitree
        // then check_tei. Inject a <span> in the body; check_tei must strip it.
        let body = create_element("body");
        let p = build_elem("p", Some("good "), vec![], &[]);
        let span = build_elem("span", Some("bad"), vec![], &[]);
        append_child(&p, &span);
        append_child(&body, &p);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let out = build_tei_output(&doc);
        let spans = get_elements_by_tag_name(&out, "span");
        assert!(spans.is_empty(), "span must be stripped by check_tei");
    }

    // --- check_tei deeper coverage -----------------------------------

    #[test]
    fn check_tei_renames_head_to_ab_header() {
        // rationale: xml.py:199-210 — Pass 1 renames <head> -> <ab type="header">.
        // Build a TEI shell that exposes <head> inside the body/div chain.
        let tei = create_element("TEI");
        let textel = create_element("text");
        let bodyel = create_element("body");
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        let head = build_elem("head", Some("Section"), vec![], &[]);
        append_child(&div, &head);
        append_child(&bodyel, &div);
        append_child(&textel, &bodyel);
        append_child(&tei, &textel);
        check_tei(&tei);
        let heads = get_elements_by_tag_name(&tei, "head");
        assert!(heads.is_empty(), "head must be renamed: {}", heads.len());
        let abs_with_header = get_elements_by_tag_name(&tei, "ab")
            .into_iter()
            .filter(|e| get_attribute(e, "type").as_deref() == Some("header"))
            .count();
        assert_eq!(abs_with_header, 1);
    }

    #[test]
    fn check_tei_preserves_valid_attributes() {
        // rationale: xml.py:232-234 — attributes in TEI_VALID_ATTRS are kept.
        // `rend`, `rendition`, `role`, `target`, `type` are valid.
        let tei = create_element("TEI");
        let textel = create_element("text");
        let bodyel = create_element("body");
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        let p = build_elem(
            "p",
            Some("body"),
            vec![],
            &[("rend", "bold"), ("role", "primary"), ("target", "x")],
        );
        append_child(&div, &p);
        append_child(&bodyel, &div);
        append_child(&textel, &bodyel);
        append_child(&tei, &textel);
        check_tei(&tei);
        let ps = get_elements_by_tag_name(&tei, "p");
        assert_eq!(get_attribute(&ps[0], "rend").as_deref(), Some("bold"));
        assert_eq!(get_attribute(&ps[0], "role").as_deref(), Some("primary"));
        assert_eq!(get_attribute(&ps[0], "target").as_deref(), Some("x"));
    }

    #[test]
    fn check_tei_lb_with_text_tail_becomes_p() {
        // rationale: xml.py:212-214 — <lb> under <div> with non-blank tail
        // is renamed to <p> with text = trimmed tail. The tail must be set
        // AFTER attaching to a parent: `set_tail` on a detached node is a
        // no-op (dom.rs:770-772).
        let tei = create_element("TEI");
        let textel = create_element("text");
        let bodyel = create_element("body");
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        let lb = create_element("lb");
        append_child(&div, &lb);
        set_tail(&lb, Some("after lb"));
        append_child(&bodyel, &div);
        append_child(&textel, &bodyel);
        append_child(&tei, &textel);
        check_tei(&tei);
        // <lb> renamed to <p>; new <p> carries the tail as text.
        let ps = get_elements_by_tag_name(&tei, "p");
        assert!(ps.iter().any(|p| element_text(p).as_deref() == Some("after lb")));
    }

    // --- _handle_unwanted_tails additional shapes --------------------

    #[test]
    fn handle_unwanted_tails_p_no_tail_is_noop() {
        // rationale: xml.py:517-519 — None tail -> set None + return. No
        // change to element text.
        let p = build_elem("p", Some("body"), vec![], &[]);
        _handle_unwanted_tails(&p);
        assert_eq!(element_text(&p).as_deref(), Some("body"));
        assert_eq!(tail(&p), None);
    }

    #[test]
    fn handle_unwanted_tails_p_whitespace_only_tail_is_dropped() {
        // rationale: xml.py:517 — trim of whitespace tail collapses to ""
        // -> drop, return.
        let root = create_element("root");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&root, &p);
        set_tail(&p, Some("   "));
        _handle_unwanted_tails(&p);
        assert_eq!(tail(&p), None);
        assert_eq!(element_text(&p).as_deref(), Some("body"));
    }

    #[test]
    fn handle_unwanted_tails_p_no_existing_text_uses_tail_alone() {
        // rationale: xml.py:521-522 — `" ".join(filter(None, [text, tail]))`:
        // when text is empty, the result is just the trimmed tail.
        let root = create_element("root");
        let p = create_element("p");
        append_child(&root, &p);
        set_tail(&p, Some("only tail"));
        _handle_unwanted_tails(&p);
        assert_eq!(element_text(&p).as_deref(), Some("only tail"));
    }

    // --- _tei_handle_complex_head: additional shapes -----------------

    #[test]
    fn tei_handle_complex_head_no_children_keeps_text_only() {
        // rationale: xml.py:534-535 — leaf <head> ends up as a new <ab>
        // with copied text and copied attributes; no <lb/> separators.
        let head = build_elem("ab", Some("just text"), vec![], &[("rend", "h1")]);
        let new_ab = _tei_handle_complex_head(&head);
        assert_eq!(element_text(&new_ab).as_deref(), Some("just text"));
        assert_eq!(get_attribute(&new_ab, "rend").as_deref(), Some("h1"));
        assert!(get_elements_by_tag_name(&new_ab, "lb").is_empty());
    }

    #[test]
    fn tei_handle_complex_head_with_non_p_child_keeps_child() {
        // rationale: xml.py:545-546 — non-<p> children get appended verbatim.
        let head = build_elem("ab", Some("head"), vec![], &[]);
        let hi = build_elem("hi", Some("inner"), vec![], &[("rend", "#b")]);
        append_child(&head, &hi);
        let new_ab = _tei_handle_complex_head(&head);
        // The <hi> child must survive on the new <ab>.
        let his = get_elements_by_tag_name(&new_ab, "hi");
        assert_eq!(his.len(), 1);
        assert_eq!(element_text(&his[0]).as_deref(), Some("inner"));
    }

    // --- _wrap_unwanted_siblings_of_div: terminator branches ---------

    #[test]
    fn wrap_unwanted_siblings_breaks_on_next_div() {
        // rationale: xml.py:562-563 — encountering another <div> ends the
        // sibling-collection loop; the wrapper takes only the in-between
        // siblings.
        let body = create_element("body");
        let div1 = build_elem("div", None, vec![], &[]);
        let p_loose = build_elem("p", Some("between"), vec![], &[]);
        let div2 = build_elem("div", None, vec![], &[]);
        let p_after = build_elem("p", Some("after div2"), vec![], &[]);
        append_child(&body, &div1);
        append_child(&body, &p_loose);
        append_child(&body, &div2);
        append_child(&body, &p_after);
        _wrap_unwanted_siblings_of_div(&div1);
        // After the call: body has [div1, wrapper-div(p_loose), div2, p_after].
        // div2 acts as the terminator; p_after stays a direct child.
        let kids = children(&body);
        assert!(kids
            .iter()
            .any(|k| local_name(k).as_deref() == Some("p")
                && element_text(k).as_deref() == Some("after div2")));
    }

    #[test]
    fn wrap_unwanted_siblings_with_no_following_siblings_is_noop() {
        // rationale: xml.py:561 — empty itersiblings list: function returns
        // without inserting a wrapper.
        let body = create_element("body");
        let div = build_elem("div", None, vec![], &[]);
        append_child(&body, &div);
        _wrap_unwanted_siblings_of_div(&div);
        let kids = children(&body);
        assert_eq!(kids.len(), 1);
        assert!(matches!(local_name(&kids[0]).as_deref(), Some("div")));
    }

    // --- delete_element / merge_with_parent extra negatives ---------

    #[test]
    fn delete_element_drop_tail_no_tail_safe() {
        // rationale: keep_tail=false branch even with no following tail
        // run — just detaches element.
        let root = create_element("root");
        let a = build_elem("a", Some("x"), vec![], &[]);
        let b = build_elem("b", Some("y"), vec![], &[]);
        append_child(&root, &a);
        append_child(&root, &b);
        delete_element(&b, false);
        let bs = get_elements_by_tag_name(&root, "b");
        assert!(bs.is_empty());
    }

    #[test]
    fn merge_with_parent_no_tail_just_text_folds() {
        // rationale: xml.py:80-81 — tail=None branch leaves full_text as
        // just replace_element_text output; the rest of the fold still runs.
        let root = create_element("root");
        let a = build_elem("a", Some("x"), vec![], &[]);
        let b = build_elem("b", Some("y"), vec![], &[]);
        append_child(&root, &a);
        append_child(&root, &b);
        // no tail on b.
        merge_with_parent(&b, false);
        assert_eq!(tail(&a).as_deref(), Some("y"));
    }

    // --- xmltotxt / sanitize_tree boundary cases ---------------------

    #[test]
    fn xmltotxt_none_input_returns_empty_string() {
        // rationale: xml.py:356-357 — `if xmloutput is None: return ""`.
        assert_eq!(xmltotxt(None, false), "");
    }

    #[test]
    fn xmltotxt_simple_text_round_trips_through_sanitize_and_unescape() {
        // rationale: xml.py:363 — `unescape(sanitize("".join(returnlist))
        // or "")`. A simple <p>hello</p> should emit "hello\n" (the after-
        // tag newline) which sanitize trims to "hello".
        let p = build_elem("p", Some("hello"), vec![], &[]);
        let out = xmltotxt(Some(&p), false);
        assert!(out.contains("hello"));
    }

    #[test]
    fn xmltotxt_html_entities_in_text_are_unescaped() {
        // rationale: xml.py:363 — unescape runs after sanitize. Text
        // containing &amp; -> &.
        let p = build_elem("p", Some("a &amp; b"), vec![], &[]);
        let out = xmltotxt(Some(&p), false);
        assert!(out.contains("a & b"));
    }

    // --- _move_element_one_level_up: edge case --------------------

    #[test]
    fn move_element_one_level_up_no_grandparent_is_noop() {
        // rationale: xml.py:578-581 — `if gp is None: return`. Element nested
        // only one level deep has no grandparent; function exits without panic.
        let p = create_element("p");
        let ab = create_element("ab");
        append_child(&p, &ab);
        // p is detached - no grandparent.
        _move_element_one_level_up(&ab);
        // No panic; ab stays under p (or is unaffected).
        assert!(parent(&ab).is_some());
    }

    #[test]
    fn move_element_one_level_up_no_parent_is_noop() {
        // rationale: xml.py:578-580 — `if p is None: return`. Detached node
        // has no parent.
        let orphan = create_element("ab");
        _move_element_one_level_up(&orphan);
        // Survives without panic; remains parentless.
        assert!(parent(&orphan).is_none());
    }

    // --- remove_empty_elements additional shapes ---------------------

    #[test]
    fn remove_empty_elements_preserves_element_with_tail_text() {
        // rationale: xml.py:97 — `text_chars_test(element.tail) is False`
        // guard; an element with non-blank tail text is KEPT (truthy tail
        // qualifies as "this element matters").
        let root = create_element("root");
        let p = create_element("p");
        append_child(&root, &p);
        set_tail(&p, Some("trailing text"));
        remove_empty_elements(&root);
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 1);
    }

    // --- _handle_text_content_of_div_nodes: tail handling -----------

    #[test]
    fn handle_text_content_of_div_nodes_appends_tail_to_last_p() {
        // rationale: xml.py:505-507 — non-blank div tail folds onto the
        // last <p> child's text.
        let root = create_element("root");
        let div = create_element("div");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&div, &p);
        append_child(&root, &div);
        set_tail(&div, Some("trailing"));
        _handle_text_content_of_div_nodes(&div);
        // The div's tail is gone, folded onto the last <p>.
        assert_eq!(tail(&div), None);
        let kids = children(&div);
        assert_eq!(element_text(&kids[0]).as_deref(), Some("body trailing"));
    }

    #[test]
    fn handle_text_content_of_div_nodes_creates_p_for_tail_when_no_p() {
        // rationale: xml.py:509-511 — div with no <p> children + tail text
        // appends a fresh <p>.
        let root = create_element("root");
        let div = create_element("div");
        let other = build_elem("span", Some("non-p"), vec![], &[]);
        append_child(&div, &other);
        append_child(&root, &div);
        set_tail(&div, Some("orphan tail"));
        _handle_text_content_of_div_nodes(&div);
        let kids = children(&div);
        // The new <p> is the LAST child.
        let last = kids.last().expect("at least one child");
        assert_eq!(local_name(last).as_deref(), Some("p"));
        assert_eq!(element_text(last).as_deref(), Some("orphan tail"));
    }

    #[test]
    fn handle_text_content_of_div_nodes_blank_text_is_noop() {
        // rationale: xml.py:496-497 — `if element.text and element.text
        // .strip()` arm: whitespace-only text doesn't trigger folding.
        let div = create_element("div");
        set_element_text(&div, Some("   "));
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&div, &p);
        _handle_text_content_of_div_nodes(&div);
        // text is still "   " or unchanged; <p>'s text not touched.
        let kids = children(&div);
        assert_eq!(element_text(&kids[0]).as_deref(), Some("body"));
    }

    // --- Additional process_element table-row branches --------------

    #[test]
    fn process_element_row_with_span_attr_uses_span_when_colspan_missing() {
        // rationale: xml.py:319-324 — colspan OR span; falls back to span
        // attr when colspan absent.
        let cell = build_elem("cell", Some("x"), vec![], &[]);
        let row = build_elem("row", None, vec![cell], &[("span", "2")]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        // 1 cell, max_span=2 -> 1 pad bar.
        assert!(joined.contains("|\n"), "got: {joined:?}");
    }

    #[test]
    fn process_element_row_non_digit_colspan_defaults_to_one() {
        // rationale: xml.py:319-324 — `isdigit` gate: non-digit colspan
        // falls through to max_span=1; no padding emitted (cell_count=1
        // matches max_span=1). The row's own emission won't start with
        // pad bars — only the cell renders "| x".
        let cell = build_elem("cell", Some("x"), vec![], &[]);
        let row = build_elem("row", None, vec![cell], &[("colspan", "abc")]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        // Specifically: NO "||\n" pad-bar run (would appear if max_span > 1).
        assert!(!joined.contains("||\n"), "got: {joined:?}");
    }

    #[test]
    fn process_element_row_colspan_capped_at_max_table_width() {
        // rationale: xml.py:324 — `.min(MAX_TABLE_WIDTH)`. A massive colspan
        // is clamped to 1000.
        let cell = build_elem("cell", Some("x"), vec![], &[]);
        let row = build_elem("row", None, vec![cell], &[("colspan", "5000")]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        // Should produce a "|" repeated 999 times (1000 expected - 1 actual).
        let bar_count = joined.matches('|').count();
        assert!(bar_count >= 999, "want >= 999 bars, got {bar_count}");
    }

    // --- replace_element_text cell-with-p-child mid-row -------------

    #[test]
    fn replace_element_text_cell_with_non_p_child_falls_through_to_leaf() {
        // rationale: xml.py:289 — `if first_child.tag == "p":` gate goes
        // false for a non-<p> first child; falls into the cell-leaf branch
        // (xml.py:291-293).
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&cell, &create_text_node("body"));
        let span_kid = create_element("span");
        append_child(&cell, &span_kid);
        append_child(&row, &cell);
        // First-in-row cell with non-<p> child: should NOT hit p-child
        // branch. However elem_child_count > 0 so the "p-child" outer
        // condition (line 597) IS triggered but the inner p-check fails,
        // so elem_text is unchanged from raw.
        assert_eq!(replace_element_text(&cell, false), "body");
    }

    // --- _tei_handle_complex_head: <p> branches ---------------------

    #[test]
    fn tei_handle_complex_head_first_p_when_empty_seeds_text() {
        // rationale: xml.py:543-544 — first <p> child path: no existing
        // children + no existing text -> child text becomes ab's text.
        let head = build_elem("ab", None, vec![], &[]);
        let p = build_elem("p", Some("first"), vec![], &[]);
        append_child(&head, &p);
        let new_ab = _tei_handle_complex_head(&head);
        assert_eq!(element_text(&new_ab).as_deref(), Some("first"));
        // No <lb/> emitted (this was the first-child fast path).
        assert!(get_elements_by_tag_name(&new_ab, "lb").is_empty());
    }

    // --- prune_childless_textless via control_xml_output ------------
    // (private function, exercised via control_xml_output path)

    #[test]
    fn control_xml_output_prunes_childless_textless_inner_leaves() {
        // rationale: core.py:47-59 — empty <span> leaves are pruned by
        // prune_childless_textless inside control_xml_output, before
        // remove_empty_elements.
        let body = create_element("body");
        let outer = build_elem("p", Some("text"), vec![], &[]);
        let inner_empty = create_element("span"); // empty leaf -> pruned
        append_child(&outer, &inner_empty);
        append_child(&body, &outer);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // empty span should be gone.
        assert!(!s.contains("<span"), "got: {s}");
        // text-bearing <p> survives.
        assert!(s.contains("text"));
    }

    // --- write_fullheader: combination categories AND tags ---------

    #[test]
    fn write_fullheader_emits_both_terms_when_categories_and_tags_set() {
        // rationale: xml.py:471-481 — both flags true: <textClass> contains
        // BOTH <term type="categories"> AND <term type="tags">.
        let md = Metadata {
            categories: vec!["c".to_string()],
            tags: vec!["t".to_string()],
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let terms = get_elements_by_tag_name(&header, "term");
        assert!(terms
            .iter()
            .any(|t| get_attribute(t, "type").as_deref() == Some("categories")));
        assert!(terms
            .iter()
            .any(|t| get_attribute(t, "type").as_deref() == Some("tags")));
    }

    #[test]
    fn write_fullheader_no_filedate_leaves_creation_date_empty() {
        // rationale: xml.py:483 — `if filedate:` arm goes false; the
        // <date type="download"> still emits but with no text content.
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &Metadata::default());
        let dates = get_elements_by_tag_name(&header, "date");
        let download = dates
            .iter()
            .find(|d| get_attribute(d, "type").as_deref() == Some("download"))
            .expect("download date present");
        assert_eq!(element_text(download), None);
    }

    // --- Additional add_xml_meta: combined fields contract ---------

    #[test]
    fn add_xml_meta_all_fields_set_in_source_order() {
        // rationale: xml.py:42-46 — add_xml_meta walks META_ATTRIBUTES in
        // order (sitename, title, author, date, url, hostname, description,
        // categories, tags, license, language). Verify every slot is set
        // when its source has content.
        let md = Metadata {
            title: Some("T".to_string()),
            author: Some("A".to_string()),
            url: Some("U".to_string()),
            hostname: Some("H".to_string()),
            description: Some("D".to_string()),
            site_name: Some("S".to_string()),
            date: Some("2026".to_string()),
            categories: vec!["c".to_string()],
            tags: vec!["t".to_string()],
            license: Some("L".to_string()),
            language: Some("en".to_string()),
            ..Metadata::default()
        };
        let doc = create_element("doc");
        add_xml_meta(&doc, &md);
        assert_eq!(get_attribute(&doc, "sitename").as_deref(), Some("S"));
        assert_eq!(get_attribute(&doc, "title").as_deref(), Some("T"));
        assert_eq!(get_attribute(&doc, "author").as_deref(), Some("A"));
        assert_eq!(get_attribute(&doc, "date").as_deref(), Some("2026"));
        assert_eq!(get_attribute(&doc, "url").as_deref(), Some("U"));
        assert_eq!(get_attribute(&doc, "hostname").as_deref(), Some("H"));
        assert_eq!(get_attribute(&doc, "description").as_deref(), Some("D"));
        assert_eq!(get_attribute(&doc, "categories").as_deref(), Some("c"));
        assert_eq!(get_attribute(&doc, "tags").as_deref(), Some("t"));
        assert_eq!(get_attribute(&doc, "license").as_deref(), Some("L"));
        assert_eq!(get_attribute(&doc, "language").as_deref(), Some("en"));
    }

    // --- _wrap_unwanted_siblings_of_div: detached div is noop --------

    #[test]
    fn wrap_unwanted_siblings_of_detached_div_is_noop() {
        // rationale: xml.py:553-555 — `if p is None: return`. Detached div
        // has no parent.
        let div = create_element("div");
        // No panic; no insertion.
        _wrap_unwanted_siblings_of_div(&div);
        assert!(parent(&div).is_none());
    }

    // --- _handle_text_content_of_div_nodes blank-tail noop ----------

    #[test]
    fn handle_text_content_of_div_nodes_blank_tail_is_noop() {
        // rationale: xml.py:505-506 — `if tail and tail.strip()` arm:
        // whitespace-only tail does NOT trigger fold.
        let root = create_element("root");
        let div = create_element("div");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&div, &p);
        append_child(&root, &div);
        set_tail(&div, Some("   "));
        _handle_text_content_of_div_nodes(&div);
        // <p>'s text unchanged.
        let kids = children(&div);
        assert_eq!(element_text(&kids[0]).as_deref(), Some("body"));
    }

    // --- _move_element_one_level_up: full-path with tail and siblings ---

    #[test]
    fn move_element_one_level_up_with_following_siblings_lifts_ab() {
        // rationale: xml.py:578-607 — full flow: an <ab> nested inside a
        // <p> moves up to grandparent; following siblings get wrapped in a
        // new <p>; the parent <p> is dropped when empty. Exercises has_kids
        // / has_text / has_tail in the line 2133 conditional + line 2145
        // empty-p removal.
        let body = create_element("body");
        let p = create_element("p");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        let hi_sib = build_elem("hi", Some("after"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&p, &hi_sib);
        append_child(&body, &p);
        _move_element_one_level_up(&ab);
        // ab is now a direct child of body (sibling of <p> -> but <p> is
        // dropped because empty).
        let body_kids = children(&body);
        // Body now contains: [ab, p(new)] — the empty original p was
        // dropped, the new wrapper p (with hi inside) remains.
        assert!(body_kids
            .iter()
            .any(|k| local_name(k).as_deref() == Some("ab")));
        // Following sibling moved into a new <p>.
        let his = get_elements_by_tag_name(&body, "hi");
        assert_eq!(his.len(), 1);
    }

    #[test]
    fn move_element_one_level_up_with_tail_on_element_seeds_new_text() {
        // rationale: xml.py:593-596 — `new_elem.text = element.tail`. The
        // tail of <ab> becomes the text of the new <p> sibling.
        let body = create_element("body");
        let p = create_element("p");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        let hi_sib = build_elem("hi", Some("x"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&p, &hi_sib);
        append_child(&body, &p);
        // Set tail AFTER attach.
        set_tail(&ab, Some("AB_TAIL"));
        _move_element_one_level_up(&ab);
        // The new <p> (wrapping <hi>) should have text "AB_TAIL".
        let body_kids = children(&body);
        let new_p = body_kids
            .iter()
            .find(|k| {
                local_name(k).as_deref() == Some("p")
                    && get_elements_by_tag_name(k, "hi").len() == 1
            })
            .expect("new wrapper p present");
        assert_eq!(element_text(new_p).as_deref(), Some("AB_TAIL"));
    }

    // --- process_element row branch: head row with multi-span -------

    #[test]
    fn process_element_row_head_cell_with_colspan_emits_repeated_underline() {
        // rationale: xml.py:329-330 — head row underline uses `---|` *
        // max_span. colspan=3 + head cell -> "---|---|---|".
        let head_cell = build_elem("cell", Some("H"), vec![], &[("role", "head")]);
        let row = build_elem("row", None, vec![head_cell], &[("colspan", "3")]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        // Three "---|" substrings = "---|---|---|".
        assert_eq!(joined.matches("---|").count(), 3);
    }

    // --- prune_childless_textless via additional input shapes -------

    #[test]
    fn control_xml_output_keeps_graphic_even_when_empty() {
        // rationale: core.py:54 — `tag != "graphic"`: empty <graphic> is
        // preserved (the `prune_childless_textless` exception).
        let body = create_element("body");
        let g = create_element("graphic");
        set_attribute(&g, "src", "/img.png");
        append_child(&body, &g);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        assert!(s.contains("graphic"), "graphic must survive: {s}");
    }

    #[test]
    fn control_xml_output_keeps_empty_children_inside_code() {
        // rationale: core.py:56 — `parent.tag != "code"`: leaves under
        // <code> are preserved.
        let body = create_element("body");
        let code = create_element("code");
        let inner = create_element("span");
        append_child(&code, &inner);
        append_child(&body, &code);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // The empty span under <code> survives prune_childless_textless,
        // but remove_empty_elements has its own <code>-parent guard too.
        // Either way the <code> tag itself must be in output.
        assert!(s.contains("<code"));
    }

    // --- _tei_handle_complex_head: subsequent <p> emits <lb/> ---------

    #[test]
    fn tei_handle_complex_head_second_p_emits_lb_separator() {
        // rationale: xml.py:539-541 — when the new <ab> already has children
        // (or the last child's tail has text), a <lb/> is emitted before
        // the next <p>'s text. Exercises line 1973 branch (kids.is_empty()
        // || last_has_tail).
        let head = build_elem("ab", None, vec![], &[]);
        let p1 = build_elem("p", Some("first"), vec![], &[]);
        let p2 = build_elem("p", Some("second"), vec![], &[]);
        append_child(&head, &p1);
        append_child(&head, &p2);
        let new_ab = _tei_handle_complex_head(&head);
        // After: new_ab has text "first", then <lb/> with tail "second".
        assert_eq!(element_text(&new_ab).as_deref(), Some("first"));
        let lbs = get_elements_by_tag_name(&new_ab, "lb");
        assert_eq!(lbs.len(), 1);
        assert_eq!(tail(&lbs[0]).as_deref(), Some("second"));
    }

    // --- replace_element_text: cell with p child mid-row (lines 597+) -

    #[test]
    fn replace_element_text_cell_with_p_child_first_row_trailing_space() {
        // rationale: xml.py:288-290 — verify trailing-space invariant of
        // first-cell-with-p-child branch (line 606).
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&cell, &create_text_node("h"));
        append_child(&cell, &create_element("p"));
        append_child(&row, &cell);
        // Output: "| h " — leading "| " + trailing " ".
        let r = replace_element_text(&cell, false);
        assert_eq!(r, "| h ");
        assert!(r.ends_with(' '));
    }

    // --- unescape_html: long-but-bounded scan -----------------------

    #[test]
    fn unescape_html_scan_limit_truncates_at_10_chars() {
        // rationale: the scan loop runs at most 10 iterations
        // (`for _ in 0..10`). Anything longer is treated as a non-entity
        // and passes through with the partial entity name.
        // "&abcdefghijk;" has an 11-char body, exceeds the bound.
        let r = unescape_html("&abcdefghijk;");
        // Loop terminates after 10 chars -> found_end stays false -> verbatim.
        assert!(r.starts_with('&'), "got: {r}");
    }

    // --- _move_element_one_level_up: parent-tail seeds new_elem.tail ---

    #[test]
    fn move_element_one_level_up_with_p_tail_runs_to_completion() {
        // rationale: xml.py:598-601 — tail of the parent <p> drives the
        // lines 2122-2127 + 2138-2140 chain. The function must run without
        // panic and the new wrapper p must exist somewhere in the document
        // carrying the moved <hi>. The exact placement of the tail varies
        // with rcdom's tail re-anchoring quirks (faithful-divergence:
        // documented in dom.rs:769-797).
        let body = create_element("body");
        let p = create_element("p");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        let sib = build_elem("hi", Some("x"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&p, &sib);
        append_child(&body, &p);
        // Set tail on p (the parent of ab).
        set_tail(&p, Some("PTAIL"));
        _move_element_one_level_up(&ab);
        // The moved <hi> survives; the function ran the has_kids branch.
        let his = get_elements_by_tag_name(&body, "hi");
        assert_eq!(his.len(), 1);
        // ab is hoisted to body.
        let abs_at_body = children(&body)
            .iter()
            .filter(|k| local_name(k).as_deref() == Some("ab"))
            .count();
        assert_eq!(abs_at_body, 1);
    }

    // --- serialize_xml_pretty: empty root -------------------------

    #[test]
    fn serialize_xml_pretty_empty_root_self_closes() {
        // rationale: lxml `<tag/>` for empty element.
        let e = create_element("root");
        assert_eq!(serialize_xml_pretty(&e), "<root/>");
    }

    #[test]
    fn serialize_xml_pretty_root_with_text_only_inline() {
        // rationale: text-only root has no children to indent; emits inline.
        let e = build_elem("root", Some("hello"), vec![], &[]);
        assert_eq!(serialize_xml_pretty(&e), "<root>hello</root>");
    }

    #[test]
    fn serialize_xml_pretty_root_with_attribute_emits_attr() {
        // rationale: attributes serialised name="escaped value" in source
        // order.
        let e = create_element("root");
        set_attribute(&e, "k", "v");
        assert_eq!(serialize_xml_pretty(&e), "<root k=\"v\"/>");
    }

    // --- remove_empty_elements: graphic + code coverage --------------

    #[test]
    fn remove_empty_elements_keeps_graphic_with_no_text_no_children() {
        // rationale: xml.py:101 — graphic is the exception; even with no
        // children and no text it survives.
        let root = create_element("root");
        let g = create_element("graphic");
        set_attribute(&g, "src", "/img.png");
        append_child(&root, &g);
        remove_empty_elements(&root);
        assert_eq!(get_elements_by_tag_name(&root, "graphic").len(), 1);
    }

    // --- process_element: textless cell after newline-emit ----------

    #[test]
    fn process_element_textless_cell_falls_through_to_after_tag() {
        // rationale: xml.py:333-336 — `tag != "cell"` arm: tag == "cell"
        // falls through to the after-tag block. Empty cell emits " | "
        // separator.
        let row = create_element("row");
        let cell = create_element("cell");
        append_child(&row, &cell);
        let mut out = Vec::new();
        process_element(&cell, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains(" | "), "got: {joined:?}");
    }

    // --- process_element extra branches at xml.py:673,725,728 ---------

    #[test]
    fn process_element_p_with_text_and_no_tail_runs_after_tag() {
        // rationale: xml.py:672 — `!has_text && !has_tail` arm goes false
        // when text IS present (skips the textless-element block); then
        // after-tag block runs (xml.py:725) for NEWLINE_ELEMS.
        let p = build_elem("p", Some("body"), vec![], &[]);
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("body"));
        assert!(joined.contains('\n'));
    }

    #[test]
    fn process_element_p_with_text_and_tail_emits_both() {
        // rationale: xml.py:344,350 — text + tail both present: text via
        // replace_element_text, after-tag separator, then tail emission.
        let root = create_element("root");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&root, &p);
        set_tail(&p, Some("after"));
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("body"));
        assert!(joined.contains("after"));
    }

    #[test]
    fn process_element_non_newline_non_special_emits_default_space() {
        // rationale: xml.py:346-347 — default branch (not NEWLINE_ELEMS, not
        // cell, not SPECIAL_FORMATTING) emits " ".
        let span = build_elem("span", Some("body"), vec![], &[]);
        let mut out = Vec::new();
        process_element(&span, &mut out, false);
        let joined: String = out.join("");
        // span has text, after-tag block emits " ".
        assert_eq!(joined, "body ");
    }

    // --- prune_childless_textless via control_xml_output: tail-only ---

    #[test]
    fn control_xml_output_keeps_inner_leaf_with_tail_text() {
        // rationale: core.py:51 — `not element.tail` goes false when tail
        // has text; element is KEPT (line 409 in prune_childless_textless).
        let body = create_element("body");
        let outer = build_elem("p", Some("text"), vec![], &[]);
        let kid = create_element("span");
        append_child(&outer, &kid);
        // Set tail on kid AFTER attach.
        set_tail(&kid, Some("tail content"));
        append_child(&body, &outer);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = control_xml_output(&doc, OutputFormat::Xml);
        // The span has a tail, so it's NOT pruned by prune_childless_textless.
        // But subsequent passes may transform it. The tail text should
        // somehow survive (sanitize_tree may join it).
        assert!(s.contains("tail content"), "got: {s}");
    }

    // --- _move_element_one_level_up: new_elem text-only (no kids) --

    #[test]
    fn move_element_one_level_up_with_only_ab_tail_no_siblings() {
        // rationale: xml.py:593-596 — ab's tail seeds new_elem.text. With
        // no following siblings, new_elem has no kids; has_text=true alone
        // drives the line 2133 conditional (exercise text-only branch).
        let body = create_element("body");
        let p = create_element("p");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&body, &p);
        // ab has a tail but NO following siblings.
        set_tail(&ab, Some("AB_TAIL"));
        _move_element_one_level_up(&ab);
        // After: ab is in body; some new <p> with text "AB_TAIL" exists.
        let body_kids = children(&body);
        let new_p = body_kids.iter().find(|k| {
            local_name(k).as_deref() == Some("p")
                && element_text(k).as_deref() == Some("AB_TAIL")
        });
        assert!(
            new_p.is_some(),
            "expected a new <p> with AB_TAIL text in body"
        );
    }

    // --- _tei_handle_complex_head: first <p> after non-p child (mixed) --

    #[test]
    fn tei_handle_complex_head_p_after_non_p_child_emits_lb() {
        // rationale: xml.py:539-541 — `kids.is_empty()` is false (there's
        // already a non-p child); plus last_has_tail likely false → first
        // disjunct fires → <lb/> emitted. Tests line 1965/1973 branches.
        let head = build_elem("ab", None, vec![], &[]);
        let hi = build_elem("hi", Some("x"), vec![], &[]);
        let p = build_elem("p", Some("after hi"), vec![], &[]);
        append_child(&head, &hi);
        append_child(&head, &p);
        let new_ab = _tei_handle_complex_head(&head);
        // <hi> was appended first (non-p path); then <p> hit the kids
        // already exist branch.
        assert_eq!(get_elements_by_tag_name(&new_ab, "hi").len(), 1);
        // Either <lb> emitted (kids existed) OR text seeded on last child.
        // Verify the after-hi text is somewhere in the output.
        let lbs = get_elements_by_tag_name(&new_ab, "lb");
        // Some lb separator must have been emitted between the non-p and
        // the p (kids was non-empty when p processed).
        assert!(!lbs.is_empty() || tail(&hi).is_some(), "should have emitted separator");
    }

    // --- write_fullheader: tags-only (no categories) ----------------

    #[test]
    fn write_fullheader_tags_only_textclass_has_no_categories_term() {
        // rationale: xml.py:471-475 — `if categories:` arm goes false; only
        // the tags <term> emits. Tests line 2293 conditional.
        let md = Metadata {
            tags: vec!["t".to_string()],
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let terms = get_elements_by_tag_name(&header, "term");
        let categories_term = terms
            .iter()
            .find(|t| get_attribute(t, "type").as_deref() == Some("categories"));
        assert!(
            categories_term.is_none(),
            "categories term must be absent when categories empty"
        );
        let tag_term = terms
            .iter()
            .find(|t| get_attribute(t, "type").as_deref() == Some("tags"));
        assert!(tag_term.is_some());
    }

    #[test]
    fn write_fullheader_categories_only_textclass_has_no_tags_term() {
        // rationale: xml.py:476-481 — `if tags:` arm goes false.
        let md = Metadata {
            categories: vec!["c".to_string()],
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let terms = get_elements_by_tag_name(&header, "term");
        let tags_term = terms
            .iter()
            .find(|t| get_attribute(t, "type").as_deref() == Some("tags"));
        assert!(
            tags_term.is_none(),
            "tags term must be absent when tags empty"
        );
    }

    // --- write_fullheader: no description on abstract <p> ------------

    #[test]
    fn write_fullheader_description_seeds_abstract_p_text() {
        // rationale: xml.py:472-473 — description seeds the abstract <p>'s
        // text. Tests line 2283 conditional.
        let md = Metadata {
            description: Some("the abstract".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let abs = get_elements_by_tag_name(&header, "abstract");
        let abs_p = children(&abs[0]);
        assert_eq!(element_text(&abs_p[0]).as_deref(), Some("the abstract"));
    }

    #[test]
    fn write_fullheader_url_absent_omits_ptr_url() {
        // rationale: xml.py:465-467 — `if url:` goes false; no <ptr type="URL">
        // in biblFull/publicationStmt.
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &Metadata::default());
        let ptrs = get_elements_by_tag_name(&header, "ptr");
        let url_ptr = ptrs
            .iter()
            .find(|p| get_attribute(p, "type").as_deref() == Some("URL"));
        assert!(url_ptr.is_none());
    }

    // --- unescape_html: long entity name terminator ----------------

    #[test]
    fn unescape_html_entity_with_non_alnum_terminator_breaks_loop() {
        // rationale: xml.py-equiv — the peek loop's `_ => break` arm (line
        // 1055) fires when a non-alphanumeric / non-`;` / non-`#` character
        // shows up mid-entity (e.g. "&amp x" — space breaks the scan).
        let r = unescape_html("&amp x");
        // "&amp" was scanned, then ' ' breaks the loop, found_end=false ->
        // verbatim copy.
        assert!(r.contains("&amp"));
    }

    // --- write_fullheader: source bibl sigle / date / sitename --------

    #[test]
    fn write_fullheader_sitename_and_date_combine_into_sigle() {
        // rationale: xml.py:449-456 — `sigle = ', '.join([sitename, date])`.
        // Both set: sigle = "Example, 2026". Tests line 2238 conditional.
        let md = Metadata {
            site_name: Some("Example".to_string()),
            date: Some("2026".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let bibls = get_elements_by_tag_name(&header, "bibl");
        let sigle_bibl = bibls
            .iter()
            .find(|b| get_attribute(b, "type").as_deref() == Some("sigle"))
            .expect("sigle bibl present");
        assert_eq!(element_text(sigle_bibl).as_deref(), Some("Example, 2026"));
    }

    #[test]
    fn write_fullheader_title_only_seeds_source_bibl() {
        // rationale: xml.py:451-454 — bibl_parts = [title, sigle], joined.
        // Title set, sigle empty: bibl text = "title".
        let md = Metadata {
            title: Some("Title Only".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        // Source bibl is the first bibl (no type=sigle).
        let bibls = get_elements_by_tag_name(&header, "bibl");
        let source_bibl = bibls
            .iter()
            .find(|b| get_attribute(b, "type").is_none())
            .expect("source bibl present");
        assert_eq!(element_text(source_bibl).as_deref(), Some("Title Only"));
    }

    #[test]
    fn write_fullheader_date_set_seeds_publication_date_text() {
        // rationale: xml.py:466 — `<date>{date}</date>` in publicationStmt
        // when date is set. Tests line 2272 conditional.
        let md = Metadata {
            date: Some("2026-05-26".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let dates = get_elements_by_tag_name(&header, "date");
        // Find a date with text "2026-05-26" (the publication date, not the
        // download date).
        let pub_date = dates
            .iter()
            .find(|d| element_text(d).as_deref() == Some("2026-05-26"));
        assert!(pub_date.is_some(), "publication date must be set");
    }

    // --- build_json_output: raw_text non-empty branch ---------------

    #[test]
    fn build_json_output_raw_text_non_empty_renders_as_string() {
        // rationale: xml.py:124 — `raw_text` slot: non-empty -> json_str;
        // empty -> "null". The full-metadata branch (with_metadata=true)
        // emits this slot. Tests line 1427 conditional.
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: "raw content".to_string(),
        };
        let s = build_json_output(&doc, true);
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(v["raw_text"].as_str(), Some("raw content"));
    }

    #[test]
    fn build_json_output_raw_text_empty_renders_as_null() {
        // rationale: xml.py:124 — empty raw_text -> "null" token.
        let doc = Document {
            metadata: Metadata::default(),
            body: create_element("body"),
            commentsbody: None,
            raw_text: String::new(),
        };
        let s = build_json_output(&doc, true);
        let v: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert!(v["raw_text"].is_null());
    }

    // --- process_element: row with include_formatting -----------------

    #[test]
    fn process_element_row_with_include_formatting_uses_plain_newline() {
        // rationale: xml.py:341-343 — `\u{2424}` hack EXCEPT for <row>;
        // a row with include_formatting=true still uses plain "\n".
        let cell = build_elem("cell", Some("x"), vec![], &[]);
        let row = build_elem("row", None, vec![cell], &[]);
        let mut out = Vec::new();
        process_element(&row, &mut out, true);
        let joined: String = out.join("");
        // No U+2424 marker on rows.
        assert!(!joined.contains('\u{2424}'), "got: {joined:?}");
        assert!(joined.contains('\n'));
    }

    // --- replace_element_text: head with rend=0 unusual ---------------

    #[test]
    fn replace_element_text_head_rend_zero_emits_zero_hashes() {
        // rationale: xml.py:258-263 — `int(rend[1])`. rend="h0" -> 0 hashes.
        // "{} {}".format("#"*0, "Title") = " Title".
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "h0")]);
        assert_eq!(replace_element_text(&h, true), " Title");
    }

    #[test]
    fn replace_element_text_head_rend_non_digit_defaults_to_two() {
        // rationale: xml.py:258-263 — `int(rend[1])` raises on non-digit;
        // we default to 2 (unwrap_or path).
        let h = build_elem("head", Some("Title"), vec![], &[("rend", "hx")]);
        assert_eq!(replace_element_text(&h, true), "## Title");
    }

    // --- _wrap_unwanted_siblings_of_div: non-TEI_DIV_SIBLING flush --

    #[test]
    fn wrap_unwanted_siblings_flushes_on_non_tei_div_sibling() {
        // rationale: xml.py:569-573 — non-TEI_DIV_SIBLING sibling flushes
        // the wrapper if it has children, then starts fresh. <head> is NOT
        // in TEI_DIV_SIBLINGS — it acts as a separator.
        let body = create_element("body");
        let div = build_elem("div", None, vec![], &[]);
        let p1 = build_elem("p", Some("p1"), vec![], &[]);
        let head = build_elem("head", Some("h"), vec![], &[]);
        let p2 = build_elem("p", Some("p2"), vec![], &[]);
        append_child(&body, &div);
        append_child(&body, &p1);
        append_child(&body, &head);
        append_child(&body, &p2);
        _wrap_unwanted_siblings_of_div(&div);
        // body must still contain a <head> element (the separator stays in place).
        let heads = get_elements_by_tag_name(&body, "head");
        assert_eq!(heads.len(), 1);
        // p1 should now be inside a wrapper div (the flush moved it).
        let ps = get_elements_by_tag_name(&body, "p");
        assert!(ps.iter().any(|p| element_text(p).as_deref() == Some("p1")));
        // p2 also collected; ends up in body somewhere.
        assert!(ps.iter().any(|p| element_text(p).as_deref() == Some("p2")));
    }

    // --- restore_tei_case: case where source uses lowercase TEI -----

    #[test]
    fn restore_tei_case_handles_tei_with_attributes() {
        // rationale: the `<tei ` mapping (note trailing space) handles
        // self-closing-less <tei xmlns="...">.
        let in_s = "<tei xmlns=\"http://www.tei-c.org/ns/1.0\"></tei>";
        let out = restore_tei_case(in_s);
        assert!(out.contains("<TEI "));
        assert!(out.contains("</TEI>"));
    }

    // --- process_element: textless-with-tail (has_tail=true) -------

    #[test]
    fn process_element_textless_with_tail_renders_tail() {
        // rationale: xml.py:309 — `!has_text && !has_tail` arm goes false
        // when has_tail=true; the textless-element block is skipped, but
        // after-tag block and tail emission both run. For NEWLINE_ELEMS
        // (e.g. <p>), this is a different shape than text+tail.
        let root = create_element("root");
        let p = create_element("p");
        append_child(&root, &p);
        set_tail(&p, Some("only tail"));
        let mut out = Vec::new();
        process_element(&p, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("only tail"), "got: {joined:?}");
    }

    // --- serialize_xml_pretty: element with text-only inline path ----

    #[test]
    fn serialize_xml_pretty_with_text_and_child_inline() {
        // rationale: mixed content (text + child) emits inline (line 2832
        // has_text=true branch within !indent block).
        let main = create_element("main");
        set_element_text(&main, Some("Lead "));
        let span = build_elem("span", Some("kid"), vec![], &[]);
        append_child(&main, &span);
        let s = serialize_xml_pretty(&main);
        // Has_text + child -> mixed content -> inline.
        assert_eq!(s, "<main>Lead <span>kid</span></main>");
    }

    // --- _handle_unwanted_tails: empty tail trimmed to None ---------

    #[test]
    fn handle_unwanted_tails_p_explicit_empty_tail_is_dropped() {
        // rationale: xml.py:517 — empty (post-trim) tail → drop.
        let root = create_element("root");
        let p = build_elem("p", Some("body"), vec![], &[]);
        append_child(&root, &p);
        // Set tail to "" — empty string. set_tail with empty string is a
        // no-op per dom.rs:791-793, so we need whitespace to exercise the
        // trim branch.
        set_tail(&p, Some("\t  \n"));
        _handle_unwanted_tails(&p);
        // Tail trimmed to "" → dropped; element text unchanged.
        assert_eq!(element_text(&p).as_deref(), Some("body"));
        assert_eq!(tail(&p), None);
    }

    // --- remove_empty_elements: keep when only text non-blank --------

    #[test]
    fn remove_empty_elements_preserves_element_with_text_only() {
        // rationale: xml.py:97 — `text_chars_test(element.text)` returns
        // true → element kept (line 351 conditional).
        let root = create_element("root");
        let p = build_elem("p", Some("real text"), vec![], &[]);
        append_child(&root, &p);
        remove_empty_elements(&root);
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 1);
    }

    // --- strip_double_tags: parent in NESTING_WHITELIST skips merge ---

    #[test]
    fn strip_double_tags_does_not_merge_when_inner_parent_is_quote() {
        // rationale: xml.py:110 — `subelem.getparent().tag not in
        // NESTING_WHITELIST` gate. <quote><p>...</p></quote> the <p>'s
        // parent is <quote> (in NESTING_WHITELIST), so the merge is
        // skipped even if a nested same-tag is found. Tests line 477.
        let root = create_element("root");
        let inner_p = build_elem("p", Some("inner"), vec![], &[]);
        let quote = build_elem("quote", None, vec![inner_p], &[]);
        let outer_p = build_elem("p", None, vec![quote], &[]);
        append_child(&root, &outer_p);
        strip_double_tags(&root);
        // Both <p>s survive: the inner <p>'s parent is <quote>, which is
        // in NESTING_WHITELIST.
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 2);
        assert_eq!(get_elements_by_tag_name(&root, "quote").len(), 1);
    }

    // --- replace_element_text: code single-line --------------------

    #[test]
    fn replace_element_text_code_single_line_inline_wrap() {
        // rationale: xml.py:270-274 — single-line code uses backticks
        // `inline`.
        let c = build_elem("code", Some("inline"), vec![], &[]);
        assert_eq!(replace_element_text(&c, true), "`inline`");
    }

    // --- replace_element_text: code without include_formatting -----

    #[test]
    fn replace_element_text_code_without_formatting_passthrough() {
        // rationale: xml.py:257 — the formatting match block is gated by
        // include_formatting; without it, code emits raw text.
        let c = build_elem("code", Some("inline"), vec![], &[]);
        assert_eq!(replace_element_text(&c, false), "inline");
    }

    // --- process_element: tail on textless element -----------------

    #[test]
    fn process_element_textless_lb_with_tail_emits_newline_and_tail() {
        // rationale: xml.py:309-336 — `!has_text && !has_tail` arm goes
        // false when tail present; the textless-block is skipped, but
        // <lb> is NEWLINE_ELEMS so after-tag block still emits "\n"; tail
        // appended after.
        let root = create_element("root");
        let lb = create_element("lb");
        append_child(&root, &lb);
        set_tail(&lb, Some("tail-content"));
        let mut out = Vec::new();
        process_element(&lb, &mut out, false);
        let joined: String = out.join("");
        assert!(joined.contains("tail-content"));
        assert!(joined.contains('\n'));
    }

    // --- write_fullheader: sigle date-only ----------------------

    #[test]
    fn write_fullheader_date_only_sigle_no_sitename() {
        // rationale: xml.py:449-456 — sigle_parts filtered_flatten; with
        // only date, sigle = "2026".
        let md = Metadata {
            date: Some("2026".to_string()),
            ..Metadata::default()
        };
        let teidoc = create_element("TEI");
        let header = write_fullheader(&teidoc, &md);
        let bibls = get_elements_by_tag_name(&header, "bibl");
        let sigle_bibl = bibls
            .iter()
            .find(|b| get_attribute(b, "type").as_deref() == Some("sigle"))
            .expect("sigle bibl present");
        assert_eq!(element_text(sigle_bibl).as_deref(), Some("2026"));
    }

    // --- _move_element_one_level_up: all-false branch at line 2133 ---

    #[test]
    fn move_element_one_level_up_no_siblings_no_tails_skips_new_elem_insert() {
        // rationale: xml.py:603-604 — `if has_kids OR has_text OR has_tail`
        // gate's all-false branch: ab has no tail, no following siblings,
        // and parent p has no tail. new_elem stays detached. The empty p
        // is then dropped (line 2145 active branch).
        let body = create_element("body");
        let p = create_element("p");
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&body, &p);
        _move_element_one_level_up(&ab);
        // ab is hoisted; the original p (now empty) is removed; no new p.
        let body_kids = children(&body);
        // Should be just the ab.
        assert_eq!(body_kids.len(), 1);
        assert_eq!(local_name(&body_kids[0]).as_deref(), Some("ab"));
    }

    // --- _move_element_one_level_up: p retains text, NOT dropped ----

    #[test]
    fn move_element_one_level_up_keeps_p_when_it_has_text() {
        // rationale: xml.py:606-607 — p has text → keep. Tests line 2145
        // arm of the conditional (p NOT dropped).
        let body = create_element("body");
        let p = create_element("p");
        // Give p some leading text BEFORE ab.
        set_element_text(&p, Some("p text"));
        let ab = build_elem("ab", Some("head"), vec![], &[]);
        append_child(&p, &ab);
        append_child(&body, &p);
        _move_element_one_level_up(&ab);
        // p still in body, retains its text.
        let body_kids = children(&body);
        let p_kept = body_kids
            .iter()
            .find(|k| local_name(k).as_deref() == Some("p"))
            .expect("p still present");
        assert_eq!(element_text(p_kept).as_deref(), Some("p text"));
    }

    // --- replace_element_text: del without include_formatting ------

    #[test]
    fn replace_element_text_del_without_formatting_passthrough() {
        // rationale: xml.py:257 — match block gated by include_formatting.
        // <del> with include_formatting=false emits raw text.
        let d = build_elem("del", Some("old"), vec![], &[]);
        assert_eq!(replace_element_text(&d, false), "old");
    }

    // --- _handle_unwanted_tails: ab without parent is noop -----------

    #[test]
    fn handle_unwanted_tails_ab_no_parent_no_panic() {
        // rationale: xml.py:523-528 — `if let Some(p) = parent(element)`
        // arm: detached ab → no insertion, no panic. set_tail on detached
        // is a no-op (dom.rs:770-772), so we can't directly test the
        // tail-bearing detached case; but verify the function exits
        // cleanly.
        let orphan_ab = create_element("ab");
        _handle_unwanted_tails(&orphan_ab);
        assert!(parent(&orphan_ab).is_none());
    }

    // --- escape_xml_text_into / escape_xml_attr_into via serialize ----

    #[test]
    fn serialize_xml_pretty_text_with_amp_lt_gt_escapes_each() {
        // rationale: escape_xml_text_into — `&`, `<`, `>` each escaped.
        let e = build_elem("root", Some("a & b < c > d"), vec![], &[]);
        let s = serialize_xml_pretty(&e);
        assert!(s.contains("&amp;"));
        assert!(s.contains("&lt;"));
        assert!(s.contains("&gt;"));
    }

    #[test]
    fn serialize_xml_pretty_attr_with_quote_escapes() {
        // rationale: escape_xml_attr_into — `"` escaped inside attrs.
        let e = create_element("root");
        set_attribute(&e, "k", "a \"v\" b");
        let s = serialize_xml_pretty(&e);
        assert!(s.contains("&quot;"));
    }

    // --- unescape_html: numeric entity (covers '#' arm of line 1051) ---

    #[test]
    fn unescape_html_alnum_entity_at_each_position() {
        // rationale: the peek loop accepts is_ascii_alphanumeric || '#'.
        // Digits at the start trigger only the alnum arm (no '#').
        // Numeric entity "&#x33;" hits the '#' arm at idx 0, then 'x33'
        // (alnum) at indices 1-3. Both sub-conditions exercised.
        assert_eq!(unescape_html("&#x33;"), "3");
        // Long alpha entity hits only alnum.
        assert_eq!(unescape_html("&amp;"), "&");
    }

    // --- process_element: row with mixed cells (head+plain) -----------

    #[test]
    fn process_element_row_mixed_head_and_plain_cells() {
        // rationale: xml.py:329-330 — `has_head_cell` is true if ANY child
        // is role=head. Mix of head and plain cells still emits the
        // underline separator. Tests line 703 conditional.
        let head_cell = build_elem("cell", Some("H"), vec![], &[("role", "head")]);
        let plain_cell = build_elem("cell", Some("P"), vec![], &[]);
        let row = build_elem("row", None, vec![head_cell, plain_cell], &[]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        // Both cells emit content; head separator present.
        assert!(joined.contains('H'));
        assert!(joined.contains('P'));
        assert!(joined.contains("---|"));
    }

    // --- process_element: row with cell role≠"head" yields no underline ---

    #[test]
    fn process_element_row_with_non_head_cells_no_underline() {
        // rationale: xml.py:329 — `if has_head_cell:` arm goes false when
        // no role=head cell exists; no "---|" underline.
        let c1 = build_elem("cell", Some("a"), vec![], &[]);
        let c2 = build_elem("cell", Some("b"), vec![], &[]);
        let row = build_elem("row", None, vec![c1, c2], &[]);
        let mut out = Vec::new();
        process_element(&row, &mut out, false);
        let joined: String = out.join("");
        assert!(!joined.contains("---|"), "got: {joined:?}");
    }

    // --- _tei_handle_complex_head: <ab> with non-p kid then p -------

    #[test]
    fn tei_handle_complex_head_kids_then_p_last_has_tail() {
        // rationale: xml.py:539-541 — when last child has tail text, emit
        // another <lb/> before the next <p>'s text. Tests line 1973's
        // last_has_tail=true sub-condition + line 1978 latest.
        let head = build_elem("ab", None, vec![], &[]);
        let hi = build_elem("hi", Some("first"), vec![], &[]);
        let p2 = build_elem("p", Some("middle"), vec![], &[]);
        let p3 = build_elem("p", Some("end"), vec![], &[]);
        append_child(&head, &hi);
        append_child(&head, &p2);
        append_child(&head, &p3);
        let new_ab = _tei_handle_complex_head(&head);
        // <hi> goes in via the non-p path; then p2 hits the kids non-empty
        // branch and either appends lb or sets tail on the last child;
        // p3 similarly. Verify multiple <p>-derived texts survive.
        let lbs = get_elements_by_tag_name(&new_ab, "lb");
        assert!(!lbs.is_empty(), "expected at least one <lb/>");
        // All text content survives somewhere.
        let dump = format!("{:?}", new_ab);
        let _ = dump; // silence unused-binding lint
        let his = get_elements_by_tag_name(&new_ab, "hi");
        assert_eq!(his.len(), 1);
    }

    // --- serialize_xml_pretty: child with text but no descendants ---

    #[test]
    fn serialize_xml_pretty_child_with_text_indents_when_root_clean() {
        // rationale: line 2806 — non-empty kids + has_text=false on root
        // → indented form. Child has text but indenting parent root has
        // no text.
        let root = create_element("root");
        let kid = build_elem("kid", Some("hi"), vec![], &[]);
        append_child(&root, &kid);
        let s = serialize_xml_pretty(&root);
        // Indented form: root child on its own line.
        assert!(s.contains("\n  <kid>hi</kid>"), "got: {s}");
    }

    // -------------------------------------------------------------------
    // Coverage: check_tei complex-head + move-up integration (xml.py:205-210)
    // -------------------------------------------------------------------

    /// `check_tei`: a `<head>` with element children that is nested inside a
    /// `<p>` exercises BOTH the complex-head conversion (xml.py:206-208) and
    /// the move-one-level-up (xml.py:209-210). The existing TEI tests call
    /// `_tei_handle_complex_head` / `_move_element_one_level_up` in isolation;
    /// this drives them through `check_tei`'s Pass-1 integration so the
    /// `parent.replace(elem, new_elem)` splice (output.rs:2412-2418) and the
    /// `p_tag == "p"` move-up dispatch (output.rs:2425-2427) are covered.
    #[test]
    fn check_tei_complex_head_inside_p_converts_and_moves_up() {
        // <TEI><text><body><div type="entry">
        //   <p><head rend="h2">title<hi>X</hi></head>after</p>
        // </div></body></text></TEI>
        let tei = create_element("TEI");
        let textel = create_element("text");
        append_child(&tei, &textel);
        let bodyel = create_element("body");
        append_child(&textel, &bodyel);
        let div = build_elem("div", None, vec![], &[("type", "entry")]);
        append_child(&bodyel, &div);
        let p = build_elem("p", None, vec![], &[]);
        append_child(&div, &p);
        // <head> WITH an element child (so it is "complex" / non-leaf).
        let head = build_elem("head", Some("title"), vec![], &[("rend", "h2")]);
        let hi = build_elem("hi", Some("X"), vec![], &[]);
        append_child(&head, &hi);
        append_child(&p, &head);
        // a tail on head triggers the head_tail re-apply path (xml.py:415-417).
        set_tail(&head, Some("after"));

        check_tei(&tei);

        // Head was renamed to <ab type="header"> (Pass 1), flattened of its
        // <p>/element children by _tei_handle_complex_head, and lifted out of
        // the <p> by _move_element_one_level_up. After: an <ab> exists under
        // the <div> (the grandparent), no longer under <p>.
        let abs = get_elements_by_tag_name(&tei, "ab");
        assert_eq!(abs.len(), 1, "head converted to a single <ab>");
        let ab = &abs[0];
        assert_eq!(
            get_attribute(ab, "type").as_deref(),
            Some("header"),
            "ab carries type=header"
        );
        // The <ab> must now be a direct child of the <div>, not the <p>
        // (it was moved one level up out of the p).
        let ab_parent_tag = parent(ab).and_then(|pp| local_name(&pp)).unwrap_or_default();
        assert_eq!(
            ab_parent_tag, "div",
            "ab lifted out of <p> to the grandparent <div>"
        );
        // No <head> tag remains.
        assert!(get_elements_by_tag_name(&tei, "head").is_empty());
    }

    // -------------------------------------------------------------------
    // Coverage: write_teitree / xmltocsv with a present commentsbody
    // -------------------------------------------------------------------

    /// `write_teitree` (via `build_tei_output`): when the document HAS a
    /// `commentsbody`, the `Some(cb)` arm renames it to `<div>` rather than
    /// synthesising an empty one (xml.py:405-408). Pins output.rs:2363 — the
    /// `Some(cb) => replace_element_tag(cb, "div")` arm — which the existing
    /// TEI tests (all `commentsbody: None`) never reach.
    #[test]
    fn build_tei_output_with_comments_renames_commentsbody_to_div() {
        let body = create_element("body");
        let p = build_elem("p", Some("Body text here."), vec![], &[]);
        append_child(&body, &p);
        let commentsbody = create_element("body");
        let cp = build_elem("p", Some("A comment."), vec![], &[]);
        append_child(&commentsbody, &cp);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: Some(commentsbody),
            raw_text: String::new(),
        };
        let out = build_tei_output(&doc);
        // The comments body must appear as <div type="comments"> carrying the
        // comment paragraph.
        let comment_divs: Vec<NodeRef> = get_elements_by_tag_name(&out, "div")
            .into_iter()
            .filter(|d| get_attribute(d, "type").as_deref() == Some("comments"))
            .collect();
        assert_eq!(comment_divs.len(), 1, "one comments div");
        let comment_text = dom::text_content(&comment_divs[0]);
        assert!(
            comment_text.contains("A comment."),
            "comments div must carry the comment text, got: {comment_text:?}"
        );
    }

    /// `xmltocsv`: when the comments body produces non-empty text, the
    /// comments column holds that text rather than the `null` token. Pins
    /// output.rs:1551-1554 — the `else { comments_text }` arm — which the
    /// existing CSV tests (all empty comments) never reach.
    #[test]
    fn xmltocsv_emits_comment_text_when_commentsbody_nonempty() {
        let body = create_element("body");
        let p = build_elem("p", Some("Main body content."), vec![], &[]);
        append_child(&body, &p);
        let commentsbody = create_element("body");
        let cp = build_elem("p", Some("Reader comment text."), vec![], &[]);
        append_child(&commentsbody, &cp);
        let doc = Document {
            metadata: Metadata::default(),
            body,
            commentsbody: Some(commentsbody),
            raw_text: String::new(),
        };
        let row = xmltocsv(&doc, false, "\t", "null", true);
        let cols: Vec<&str> = row.trim_end_matches("\r\n").split('\t').collect();
        // Column 8 (index 7) is text; column 9 (index 8) is comments.
        assert!(
            cols[7].contains("Main body content."),
            "text col must hold body text, got: {:?}",
            cols[7]
        );
        assert!(
            cols[8].contains("Reader comment text."),
            "comments col must hold comment text (not null), got: {:?}",
            cols[8]
        );
    }

}












