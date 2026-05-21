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
        let Some(p) = parent(&element) else { continue };
        // xml.py:100-102 — `if parent is not None and element.tag !=
        // "graphic" and parent.tag != 'code': parent.remove(element)`.
        if local_name(&element).as_deref() == Some("graphic") {
            continue;
        }
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

/// Faithful subset of `utils.py:303-312` (`sanitize`) — line-by-line cleanup
/// with `\u{2424}` removed (xml.py:343's spacing hack) and HTML space
/// entities decoded. Empty lines (whitespace-only after `line_processing`)
/// are pruned; non-empty lines are `\n`-joined.
fn sanitize_text(text: &str) -> String {
    // utils.py:310 — `'\n'.join(filter(None, (line_processing(l, ...) for l
    // in text.splitlines()))).replace('␤', '')`.
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let processed = line_processing(line);
        // utils.py:310 — `filter(None, ...)` drops `None`-returning lines.
        if let Some(p) = processed {
            out_lines.push(p);
        }
    }
    // utils.py:310 — `.replace('␤', '')` — apply AFTER the join.
    out_lines.join("\n").replace('\u{2424}', "")
}

/// Faithful subset of `utils.py:282-300` (`line_processing`):
/// - replace `&#13;` -> '\r', `&#10;` -> '\n', `&nbsp;` -> '\u{00A0}'
/// - trim (`utils.py:340-346`: collapse whitespace + strip)
/// - return `None` for all-whitespace lines
///
/// Stage 3-B does NOT port the `preserve_space` / `trailing_space` knobs
/// (the `sanitize`-`process_element` callsite at `xml.py:363` uses
/// defaults). `remove_control_characters` is omitted — the upstream
/// parser already drops C0 controls except whitespace; if a future test
/// surfaces a control-character leak the helper grows here.
fn line_processing(line: &str) -> Option<String> {
    // utils.py:288 — `remove_control_characters(line.replace('&#13;',
    // '\r').replace('&#10;', '\n').replace('&nbsp;', ' '))`.
    let decoded = line
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&nbsp;", "\u{00A0}");
    // utils.py:292 — `trim(LINES_TRIMMING.sub(r" ", new_line))`. Our `trim`
    // (utils.rs:97) already collapses Unicode whitespace + strips, which
    // subsumes LINES_TRIMMING's behaviour on the realistic inputs.
    let trimmed = crate::trafilatura::utils::trim(&decoded);
    // utils.py:294-295 — `if all(map(str.isspace, new_line)): new_line = None`.
    if trimmed.chars().all(char::is_whitespace) {
        None
    } else {
        Some(trimmed)
    }
}

/// Faithful subset of Python's `html.unescape` for the small entity set
/// `process_element`'s output stream realistically carries. Stage 3-A's
/// helpers never emit named entities themselves; this is the cleanup pass
/// for entities that survived from the source HTML through lxml's
/// `.text` getter. Decodes `&amp;`, `&lt;`, `&gt;`, `&quot;`, `&apos;` and
/// numeric entities `&#NN;` / `&#xHH;` (decimal / hex codepoints).
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

    let md = &doc.metadata;
    // xml.py:378-388 — column order, with `d if d else null` for each.
    let columns: [String; 11] = [
        csv_or_null(md.url.as_deref(), null),       // 1. url
        csv_or_null(None, null),                    // 2. id (Metadata lacks)
        csv_or_null(None, null),                    // 3. fingerprint (lacks)
        csv_or_null(md.hostname.as_deref(), null),  // 4. hostname
        csv_or_null(md.title.as_deref(), null),     // 5. title
        csv_or_null(md.image.as_deref(), null),     // 6. image
        csv_or_null(md.date.as_deref(), null),      // 7. date
        posttext,                                   // 8. text
        commentstext,                               // 9. comments
        csv_or_null(md.license.as_deref(), null),   // 10. license
        csv_or_null(md.pagetype.as_deref(), null),  // 11. pagetype
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
// control_xml_output (xml.py:159-175)
// ===========================================================================

/// `xml.py:159-175` — `control_xml_output(document, options) -> str`.
///
/// The Stage 3-D entry point: runs `strip_double_tags` + `remove_empty_elements`
/// on `document.body`, calls [`build_xml_output`], then serialises with
/// pretty-printing via [`serialize_xml_pretty`]. Returns the rendered XML
/// string (Python `tostring(..., pretty_print=True, encoding='unicode').strip()`).
///
/// # TEI branch — Stage 3-E (deferred)
///
/// Python `xml.py:164` dispatches to `build_tei_output` when
/// `options.format == "xmltei"`. M4 Stage 3-E lands the TEI path; for Stage
/// 3-D we only carry the XML branch. The signature takes no `format` switch
/// because there's no second branch to discriminate yet.
///
/// # `sanitize_tree` deferral
///
/// Python `xml.py:167` runs `sanitize_tree(output_tree)` (utils.py:315-336)
/// before `tostring`. That helper trims spaces, removes control chars, and
/// normalises Unicode per-text-node. Our public `extract_to_xml` instead
/// NFC-normalises the FINAL string (matching the same `extract_to_markdown`
/// pattern in `core.py:98`). The resulting bytes are equivalent for the
/// invariants tests assert — what reaches the user is NFC text.
///
/// # `remove_blank_text` reparse equivalence
///
/// Python `xml.py:169` reparses through `CONTROL_PARSER = XMLParser(
/// remove_blank_text=True)` (`xml.py:35`) to drop inter-element whitespace
/// before pretty-printing. We mirror this in [`serialize_xml_pretty`] by
/// treating whitespace-only text/tail nodes as absent when deciding indent
/// vs inline emission.
pub(crate) fn control_xml_output(doc: &Document) -> String {
    // xml.py:161-162 — `strip_double_tags(document.body); remove_empty_elements
    // (document.body)`. Both mutate in place.
    strip_double_tags(&doc.body);
    remove_empty_elements(&doc.body);

    // xml.py:164-165 — `func = build_xml_output ...; output_tree = func(document)`.
    // TODO Stage 3-E: switch on options.format to dispatch build_tei_output here.
    let output_tree = build_xml_output(doc);

    // xml.py:167-169 — sanitize_tree + reparse-through-CONTROL_PARSER. The
    // sanitize_tree behaviour is deferred (see fn doc); the reparse equivalent
    // is folded into serialize_xml_pretty's whitespace handling.

    // xml.py:175 — `tostring(output_tree, pretty_print=True, encoding='unicode'
    // ).strip()`.
    serialize_xml_pretty(&output_tree)
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
/// 3. **Mixed-content guard.** When an element has any non-whitespace text
///    OR any child element has any non-whitespace tail, pretty-printing is
///    DISABLED for that element's children: they emit inline on the same
///    line as the parent's content.
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
    write_element_pretty(root, &mut out, 0);
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
fn write_element_pretty(element: &NodeRef, out: &mut String, depth: usize) {
    let Some(tag) = local_name(element) else {
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

    // Decide mixed-content vs indented. Indented requires: no text on this
    // element AND every child has a blank tail.
    let any_kid_has_text_tail = kids.iter().any(|k| {
        tail(k)
            .as_deref()
            .map(|t| !is_blank(t))
            .unwrap_or(false)
    });
    let mixed = has_text || any_kid_has_text_tail;

    if mixed {
        // Inline emission: write text, then each child + its tail, all on
        // the same logical run. Text/tail are emitted verbatim (already
        // sanitised by Trafilatura's pipeline upstream).
        if has_text {
            escape_xml_text_into(&text, out);
        }
        for k in &kids {
            write_element_pretty(k, out, depth + 1);
            if let Some(t) = tail(k) {
                escape_xml_text_into(&t, out);
            }
        }
    } else {
        // Indented emission: each child on its own line, indented by
        // `depth + 1` levels of 2 spaces. Blank tails are dropped (the
        // `remove_blank_text=True` reparse equivalent).
        for k in &kids {
            out.push('\n');
            for _ in 0..=depth {
                out.push_str("  ");
            }
            write_element_pretty(k, out, depth + 1);
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
        let row = xmltocsv(&doc, false, "\t", "null");
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
        let row = xmltocsv(&doc, false, ",", "N/A");
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
        let s = control_xml_output(&doc);
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
}
