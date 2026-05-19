//! `main_extractor` — Stage 2c-i: handler primitives.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.4 (the
//! `main_extractor.py` handler dispatch). Source of truth:
//! `trafilatura@v2.0.0/main_extractor.py:30-160`.
//!
//! # Scope
//!
//! Stage 2c-i ports the **handler-primitives** half of `main_extractor.py`:
//! the small reusable functions that Stage 2c-ii's block handlers
//! (`handle_paragraphs` / `handle_lists` / `handle_quotes` / etc.) consume.
//! Nothing in this file dispatches an extraction by itself — these are
//! leaf-level building blocks.
//!
//! Functions ported here (Python line ranges from `main_extractor.py@v2.0.0`):
//!
//! - Module-level constants (lines 30-35): `P_FORMATTING`, `TABLE_ELEMS`,
//!   `TABLE_ALL`, `FORMATTING`, `CODES_QUOTES`, `NOT_AT_THE_END`.
//! - `handle_titles` (lines 43-66) — process `<head>`-tagged title nodes.
//! - `handle_formatting` (lines 69-116) — process out-of-paragraph
//!   formatting (`<hi>`, `<ref>`, `<span>`).
//! - `add_sub_element` (lines 119-124) — append a processed child onto a
//!   new-tree parent, copying text/tail and the ORIGINAL element's attrs.
//! - `process_nested_elements` (lines 127-140) — walk an element's
//!   descendants, dispatching list children through `handle_lists` and
//!   non-list children through `handle_textnode` + `add_sub_element`.
//! - `update_elem_rendition` (lines 143-147) — copy the `rend` attribute.
//! - `is_text_element` (lines 149-151) — does this element have any
//!   `text_chars_test`-passing text?
//! - `define_newelem` (lines 154-158) — append a stub sub-element if a
//!   processed result exists.
//!
//! # Forward stub: `handle_lists`
//!
//! `process_nested_elements` calls `handle_lists`, which is Stage 2c-ii's
//! responsibility. This file defines a panicking stub so that any code path
//! that *actually* reaches the list branch surfaces loudly until Stage 2c-ii
//! replaces it. Tests at this stage exercise the dispatch (the call shape)
//! without forcing the stub to execute (they assert the renamed-to-"done"
//! side effect instead).
//!
//! # `_log_event` (lines 38-40): SKIPPED
//!
//! The Python helper is a debug-logging façade (`LOGGER.debug(...)`).
//! Observability, not algorithm — recording the skip per HLD §10 (every
//! Python line is either ported, stubbed-with-reason, or skipped-with-
//! reason). No call site outside this file consumes `_log_event` as a
//! value; Trafilatura's other modules log directly.
//!
//! # itertext (locally replicated)
//!
//! `is_text_element` and `handle_titles` use `''.join(elem.itertext())`.
//! `baseline.rs` defines an `itertext` helper privately at Stage 1c; rather
//! than de-duplicate now (which would touch frozen Stage 1c surface), Stage
//! 2c-i replicates the same ~20 LOC locally and a future refactor stage may
//! consolidate. The local copy is line-cited to lxml docs and mirrors the
//! `baseline.rs` version exactly.
//!
//! # Anti-inversion (HLD §4 / §10)
//!
//! Every function carries a `main_extractor.py:NN-MM` source-line cite. No
//! "looks-nice" decisions. Every test traces to a Python line, not a fixture
//! shape. The forward stub `handle_lists` panics — never silently returns
//! `None` — so a Stage 2c-ii regression cannot mask itself behind this file.

use crate::readability::dom::{
    NodeData, NodeRef, append_child, attributes_in_source_order, create_element, deep_clone,
    element_text, get_attribute, local_name, parent, previous_element_sibling, replace_element_tag,
    set_attribute, set_element_text, set_tail, tail,
};
use crate::trafilatura::cleaning::{Options, handle_textnode, process_node};
use crate::trafilatura::utils::{FORMATTING_PROTECTED, text_chars_test};

// ===========================================================================
// Module constants (main_extractor.py:30-35)
// ===========================================================================

/// `P_FORMATTING` — `{'hi', 'ref'}` (main_extractor.py:30). Tag-name set of
/// formatting elements that may appear inside a `<p>`-converted block.
/// Membership-test only — order does not matter.
pub const P_FORMATTING: &[&str] = &["hi", "ref"];

/// `TABLE_ELEMS` — `{'td', 'th'}` (main_extractor.py:31). The two cell-level
/// element types in a TEI-converted table.
pub const TABLE_ELEMS: &[&str] = &["td", "th"];

/// `TABLE_ALL` — `{'td', 'th', 'hi'}` (main_extractor.py:32). The catalog of
/// element types `handle_table` may pull through verbatim (cells plus inline
/// formatting).
pub const TABLE_ALL: &[&str] = &["td", "th", "hi"];

/// `FORMATTING` — `{'hi', 'ref', 'span'}` (main_extractor.py:33). The
/// elements `handle_formatting` is dispatched on (and which downstream
/// `<p>` wrapping treats as inline atoms).
pub const FORMATTING: &[&str] = &["hi", "ref", "span"];

/// `CODES_QUOTES` — `{'code', 'quote'}` (main_extractor.py:34). The two
/// block elements whose `process_node` short-circuits the dispatch sieve
/// (Stage 2c-iii's responsibility — referenced here for parity).
pub const CODES_QUOTES: &[&str] = &["code", "quote"];

/// `NOT_AT_THE_END` — `{'head', 'ref'}` (main_extractor.py:35). Elements
/// that must not be left as the final element of an extracted body
/// (Stage 2c-iii prune logic). Stored here for parity.
pub const NOT_AT_THE_END: &[&str] = &["head", "ref"];

// ===========================================================================
// _log_event (main_extractor.py:38-40) — SKIPPED
// ===========================================================================
//
// Python source (NOT PORTED):
// ```python
// def _log_event(msg, tag, text):
//     "Format extraction event for debugging purposes."
//     LOGGER.debug("%s: %s %s", msg, tag, trim(text or "") or "None")
// ```
//
// Observability helper, not algorithm. Trafilatura's call sites pass
// through `LOGGER.debug` only; no downstream caller consumes the return
// value (`None`). HLD §10 documents this as a deliberate skip — the Rust
// port emits no extraction debug logs at Stage 2c-i.

// ===========================================================================
// handle_lists — FORWARD STUB (Stage 2c-ii will replace this)
// ===========================================================================

/// `handle_lists(element, options)` — **STAGE 2c-ii FORWARD STUB**.
///
/// `process_nested_elements` (`main_extractor.py:131-134`) dispatches to
/// `handle_lists` when it encounters a descendant whose tag is `"list"`.
/// The full implementation is Stage 2c-ii's responsibility (`main_extractor.py
/// :161-205`). To keep Stage 2c-i compilable and to **surface loudly** if a
/// caller actually exercises the list branch before Stage 2c-ii lands, this
/// stub panics with a Stage-citing message rather than silently returning
/// `None` (which would hide a regression).
///
/// Stage 2c-ii will replace this `unimplemented!` with the full port.
fn handle_lists(_elem: &NodeRef, _opts: &Options) -> Option<NodeRef> {
    unimplemented!("Stage 2c-ii: handle_lists (main_extractor.py:161-205)")
}

// ===========================================================================
// handle_titles (main_extractor.py:43-66)
// ===========================================================================

/// `handle_titles(element, options)` — `main_extractor.py:43-66`.
///
/// Process a `<head>` (title) element: when the element has no child
/// elements, run it through `process_node` directly; otherwise deep-copy
/// it, then iterate the original's children running each through
/// `handle_textnode` and appending survivors onto the copy. The returned
/// node survives only if its concatenated `itertext` passes
/// `text_chars_test`.
///
/// # Python original
///
/// ```python
/// def handle_titles(element, options):
///     '''Process head elements (titles)'''
///     if len(element) == 0:
///         title = process_node(element, options)
///     else:
///         title = deepcopy(element)
///         for child in list(element):
///             processed_child = handle_textnode(child, options, comments_fix=False)
///             if processed_child is not None:
///                 title.append(processed_child)
///             child.tag = 'done'
///     if title is not None and text_chars_test(''.join(title.itertext())) is True:
///         return title
///     return None
/// ```
///
/// # Faithfulness notes
///
/// 1. `len(element) == 0` — lxml `len(elem)` is the *element* child count
///    (not all nodes). Use `element_child_count` (which excludes Text /
///    Comment / PI siblings).
/// 2. `deepcopy(element)` — `dom::deep_clone` (added Stage 2c-i).
/// 3. `title.append(processed_child)` — lxml `Element.append` **moves**
///    the appended node from its current parent. `dom::append_child` does
///    the same (it calls `remove(child)` first). The original `element`'s
///    child list shrinks as siblings are moved out into `title`.
/// 4. `child.tag = 'done'` — renames the (now-moved-into-`title`) child
///    via `dom::replace_element_tag`. The Python rename happens AFTER the
///    move; we mirror that ordering. `replace_element_tag` returns the
///    new `NodeRef` — we discard it because the rename's only purpose is
///    to mark the descendant as processed for downstream sieve passes,
///    and `itertext` on `title` re-walks the tree at the end (so it sees
///    whatever is in the tree NOW, regardless of which NodeRef we hold).
/// 5. `''.join(title.itertext())` — pre-order Text-data concatenation
///    over `title`'s subtree (see `itertext` helper below).
pub fn handle_titles(element: &NodeRef, options: &Options) -> Option<NodeRef> {
    let title = if element_child_count(element) == 0 {
        // main_extractor.py:50 — process_node mutates in place and returns
        // the same NodeRef (or None).
        process_node(element, options)?
    } else {
        // main_extractor.py:53 — deepcopy first.
        let title = deep_clone(element);
        // main_extractor.py:56 — `list(element)` snapshots the element-child
        // list (Python's `list()` materialises the generator).
        let children_snapshot: Vec<NodeRef> = element_children(element);
        for child in children_snapshot {
            // main_extractor.py:60 — comments_fix=False; Stage 2c-i has no
            // preserve_spaces toggle to surface here, so we pass the
            // documented Trafilatura default (false) for that slot too.
            let processed_child = handle_textnode(&child, options, false, false);
            if let Some(p) = processed_child {
                // main_extractor.py:62 — title.append(processed_child)
                // MOVES the node from `element` into `title`.
                append_child(&title, &p);
            }
            // main_extractor.py:63 — child.tag = 'done' AFTER the (maybe-no-op)
            // move. `replace_element_tag` is the rcdom-equivalent rename;
            // it returns the new NodeRef which we discard (see fn docs).
            let _renamed = replace_element_tag(&child, "done");
        }
        title
    };

    // main_extractor.py:64-65 — text_chars_test gate on the joined itertext.
    let joined: String = itertext(&title).concat();
    if text_chars_test(Some(&joined)) {
        Some(title)
    } else {
        None
    }
}

// ===========================================================================
// handle_formatting (main_extractor.py:69-116)
// ===========================================================================

/// `handle_formatting(element, options)` — `main_extractor.py:69-116`.
///
/// Process a formatting element (`<hi>`, `<ref>`, `<span>`) found outside
/// of a paragraph: if the parent (or, if no parent, the previous sibling)
/// is in `FORMATTING_PROTECTED`, return the formatting result naked;
/// otherwise wrap it in a fresh `<p>`.
///
/// # Python original (Stage 2c-i ports ONLY the active lines)
///
/// Lines 76-106 of the Python source are commented-out alternatives —
/// they are NOT ported. The active body is:
///
/// ```python
/// formatting = process_node(element, options)
/// if formatting is None:
///     return None
/// parent = element.getparent()
/// if parent is None:
///     parent = element.getprevious()
/// if parent is None or parent.tag not in FORMATTING_PROTECTED:
///     processed_element = Element('p')
///     processed_element.insert(0, formatting)
/// else:
///     processed_element = formatting
/// return processed_element
/// ```
///
/// # Faithfulness notes
///
/// 1. `process_node` mutates `element` in place and returns the same
///    `NodeRef` (or `None`). The returned `formatting` IS `element`.
/// 2. `element.getprevious()` is lxml's previous-sibling that includes
///    Comments / PIs (anything non-Text). Stage 2b' added
///    `previous_element_sibling`, which is the **element-only** subset.
///    For HTML inputs parsed by `html5ever` the only difference is whether
///    a Comment / PI sits between the element and its previous element
///    sibling; in practice Trafilatura's preceding cleaning pass strips
///    Comments and PIs are exceedingly rare in HTML — so the element-only
///    subset is functionally identical on the Stage 2c-i call paths. The
///    same trade-off is documented at the previous_element_sibling docsite
///    (Stage 2b' note).
/// 3. `processed_element.insert(0, formatting)` — lxml `Element.insert(0,
///    child)` **moves** `child` to position 0 (detaches from current
///    parent first). We use `append_child`, which appends at the END;
///    but the fresh `<p>` has no other children, so position-0-append
///    and position-0-insert are identical on an empty parent.
pub fn handle_formatting(element: &NodeRef, options: &Options) -> Option<NodeRef> {
    // main_extractor.py:72-74 — process_node short-circuit.
    let formatting = process_node(element, options)?;

    // main_extractor.py:108-110 — getparent() or getprevious().
    let parent_or_prev = parent(element).or_else(|| previous_element_sibling(element));

    // main_extractor.py:111-115 — wrap in <p> unless parent.tag in
    // FORMATTING_PROTECTED.
    let parent_protected = match parent_or_prev.as_ref() {
        Some(p) => {
            let tag = local_name(p).unwrap_or_default();
            FORMATTING_PROTECTED.contains(&tag.as_str())
        }
        None => false,
    };

    if parent_protected {
        // Naked formatting.
        Some(formatting)
    } else {
        // Wrap in <p>.
        let processed_element = create_element("p");
        // insert(0, formatting) — append into the empty <p> (move
        // semantics; `append_child` detaches from current parent first).
        append_child(&processed_element, &formatting);
        Some(processed_element)
    }
}

// ===========================================================================
// add_sub_element (main_extractor.py:119-124)
// ===========================================================================

/// `add_sub_element(new_child_elem, subelem, processed_subchild)` —
/// `main_extractor.py:119-124`.
///
/// Append a fresh sub-element onto `new_child_elem`, with:
/// - **tag** taken from `processed_subchild`,
/// - **text** + **tail** taken from `processed_subchild`,
/// - **attributes** taken from `subelem` (NOT `processed_subchild`).
///
/// # Python original
///
/// ```python
/// def add_sub_element(new_child_elem, subelem, processed_subchild):
///     sub_child_elem = SubElement(new_child_elem, processed_subchild.tag)
///     sub_child_elem.text, sub_child_elem.tail = processed_subchild.text, processed_subchild.tail
///     for attr in subelem.attrib:
///         sub_child_elem.set(attr, subelem.attrib[attr])
/// ```
///
/// # Faithfulness notes
///
/// `SubElement(parent, tag)` is lxml's "create AND attach" combinator —
/// equivalent to `create_element(tag) + append_child(parent, new)`.
///
/// The attribute-source split (attrs from `subelem`, payload from
/// `processed_subchild`) is **deliberate** in Trafilatura: `process_node`
/// may strip attributes during its processing, so the original `subelem`'s
/// attrib snapshot is the canonical source.
pub fn add_sub_element(new_child_elem: &NodeRef, subelem: &NodeRef, processed_subchild: &NodeRef) {
    let tag = local_name(processed_subchild).unwrap_or_default();
    let sub_child_elem = create_element(&tag);
    append_child(new_child_elem, &sub_child_elem);
    // .text / .tail copied from processed_subchild.
    set_element_text(&sub_child_elem, element_text(processed_subchild).as_deref());
    set_tail(&sub_child_elem, tail(processed_subchild).as_deref());
    // Attributes copied from the ORIGINAL subelem, in source order.
    for (name, value) in attributes_in_source_order(subelem) {
        set_attribute(&sub_child_elem, &name, &value);
    }
}

// ===========================================================================
// process_nested_elements (main_extractor.py:127-140)
// ===========================================================================

/// `process_nested_elements(child, new_child_elem, options)` —
/// `main_extractor.py:127-140`.
///
/// Walk `child`'s descendants (element-only, document order, excluding
/// self): dispatch `<list>` descendants through `handle_lists` (Stage
/// 2c-ii); for all other descendants, run `handle_textnode` then
/// `add_sub_element(new_child_elem, subelem, processed_subchild)`. Rename
/// every visited descendant's tag to `"done"`.
///
/// # Python original
///
/// ```python
/// def process_nested_elements(child, new_child_elem, options):
///     "Iterate through an element child and rewire its descendants."
///     new_child_elem.text = child.text
///     for subelem in child.iterdescendants("*"):
///         if subelem.tag == "list":
///             processed_subchild = handle_lists(subelem, options)
///             if processed_subchild is not None:
///                 new_child_elem.append(processed_subchild)
///         else:
///             processed_subchild = handle_textnode(subelem, options, comments_fix=False)
///             if processed_subchild is not None:
///                 add_sub_element(new_child_elem, subelem, processed_subchild)
///         subelem.tag = "done"
///         #subelem.getparent().remove(subelem)
/// ```
///
/// # Faithfulness notes
///
/// 1. `iterdescendants("*")` — element-only descendants in document order,
///    **excluding** `child` itself. We snapshot the descendant list up
///    front (Python's iterdescendants would re-walk during mutation;
///    snapshotting is the safe documented Stage-1b pattern, see
///    `cleaning.rs::delete_elements_by_tag` for the same Trafilatura-iter
///    parity discussion).
/// 2. `subelem.tag = "done"` runs **regardless** of whether the descendant
///    was kept — the Python writes the rename unconditionally at the end
///    of the loop body.
pub fn process_nested_elements(child: &NodeRef, new_child_elem: &NodeRef, options: &Options) {
    // main_extractor.py:129 — new_child_elem.text = child.text.
    set_element_text(new_child_elem, element_text(child).as_deref());

    // main_extractor.py:130 — child.iterdescendants("*") snapshot.
    let descendants = descendant_elements(child);

    for subelem in descendants {
        let tag = local_name(&subelem).unwrap_or_default();
        if tag == "list" {
            // main_extractor.py:131-134 — list branch.
            if let Some(processed) = handle_lists(&subelem, options) {
                append_child(new_child_elem, &processed);
            }
        } else {
            // main_extractor.py:135-138 — non-list branch.
            // handle_textnode with comments_fix=false, preserve_spaces=false
            // (Trafilatura default — no caller of process_nested_elements
            // passes a non-default preserve_spaces).
            if let Some(processed) = handle_textnode(&subelem, options, false, false) {
                add_sub_element(new_child_elem, &subelem, &processed);
            }
        }
        // main_extractor.py:139 — subelem.tag = "done", unconditionally.
        let _renamed = replace_element_tag(&subelem, "done");
    }
}

// ===========================================================================
// update_elem_rendition (main_extractor.py:143-147)
// ===========================================================================

/// `update_elem_rendition(elem, new_elem)` — `main_extractor.py:143-147`.
///
/// Copy `elem.get("rend")` onto `new_elem.set("rend", ...)` if present.
/// No-op when the source has no `rend` attribute (Python's `:=` walrus is
/// only true when the value is truthy; an absent attr → `None` → falsy).
///
/// # Python original
///
/// ```python
/// def update_elem_rendition(elem, new_elem):
///     "Copy the rend attribute from an existing element to a new one."
///     if rend_attr := elem.get("rend"):
///         new_elem.set("rend", rend_attr)
/// ```
///
/// Note: Python's truthy check on a string means **empty string `""` is
/// ALSO a no-op**, matching `bool("") == False`. The Rust port uses
/// `Some(s)` with `!s.is_empty()` to match that.
pub fn update_elem_rendition(elem: &NodeRef, new_elem: &NodeRef) {
    if let Some(rend) = get_attribute(elem, "rend")
        && !rend.is_empty()
    {
        set_attribute(new_elem, "rend", &rend);
    }
}

// ===========================================================================
// is_text_element (main_extractor.py:149-151)
// ===========================================================================

/// `is_text_element(elem)` — `main_extractor.py:149-151`.
///
/// `True` iff `elem` is non-`None` AND its concatenated `itertext` passes
/// `text_chars_test` (utils.py:452-456 — not empty AND not all whitespace).
///
/// # Python original
///
/// ```python
/// def is_text_element(elem):
///     "Find if the element contains text."
///     return elem is not None and text_chars_test(''.join(elem.itertext())) is True
/// ```
///
/// The Rust signature takes `Option<&NodeRef>` to mirror the `elem is not
/// None` Python guard at the type level.
pub fn is_text_element(elem: Option<&NodeRef>) -> bool {
    let Some(elem) = elem else {
        return false;
    };
    let joined: String = itertext(elem).concat();
    text_chars_test(Some(&joined))
}

// ===========================================================================
// define_newelem (main_extractor.py:154-158)
// ===========================================================================

/// `define_newelem(processed_elem, orig_elem)` — `main_extractor.py:154-158`.
///
/// If `processed_elem` is non-`None`, append a fresh sub-element onto
/// `orig_elem` carrying the same tag / text / tail as `processed_elem`.
/// (Attributes are NOT copied — this is the **stub** form; the with-attrs
/// variant lives in `add_sub_element`.)
///
/// # Python original
///
/// ```python
/// def define_newelem(processed_elem, orig_elem):
///     "Create a new sub-element if necessary."
///     if processed_elem is not None:
///         childelem = SubElement(orig_elem, processed_elem.tag)
///         childelem.text, childelem.tail = processed_elem.text, processed_elem.tail
/// ```
pub fn define_newelem(processed_elem: Option<&NodeRef>, orig_elem: &NodeRef) {
    let Some(processed_elem) = processed_elem else {
        return;
    };
    let tag = local_name(processed_elem).unwrap_or_default();
    let childelem = create_element(&tag);
    append_child(orig_elem, &childelem);
    set_element_text(&childelem, element_text(processed_elem).as_deref());
    set_tail(&childelem, tail(processed_elem).as_deref());
}

// ===========================================================================
// Local helpers (Stage 2c-i internal)
// ===========================================================================

/// Element-only child snapshot of `node`. Mirrors `list(elem)` in lxml
/// (which iterates Element children only — the Python `list(elem)` over an
/// `_Element` returns the element children, not all child nodes).
fn element_children(node: &NodeRef) -> Vec<NodeRef> {
    node.children
        .borrow()
        .iter()
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .cloned()
        .collect()
}

/// Element-only child count. Mirrors lxml's `len(elem)`.
///
/// Stage 2b's `utils.rs::element_child_count` already exposes this; Stage
/// 2c-i routes through that public helper to avoid a private duplicate.
fn element_child_count(node: &NodeRef) -> usize {
    crate::trafilatura::utils::element_child_count(node)
}

/// Element-only descendants of `node` in document order, **excluding**
/// `node` itself. Mirrors lxml's `Element.iterdescendants("*")`.
fn descendant_elements(node: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    for child in node.children.borrow().iter() {
        if matches!(child.data, NodeData::Element { .. }) {
            out.push(child.clone());
            // Recurse — the snapshot is depth-first / pre-order.
            collect_descendant_elements(child, &mut out);
        }
    }
    out
}

fn collect_descendant_elements(node: &NodeRef, out: &mut Vec<NodeRef>) {
    for child in node.children.borrow().iter() {
        if matches!(child.data, NodeData::Element { .. }) {
            out.push(child.clone());
            collect_descendant_elements(child, out);
        }
    }
}

/// `Element.itertext()` from lxml — pre-order Text-node `data`
/// concatenation over `elem`'s subtree.
///
/// Locally replicated from `baseline.rs::itertext` (Stage 1c, frozen). A
/// future refactor stage may consolidate to a shared `dom::itertext`; until
/// then Stage 2c-i carries its own copy to avoid touching the Stage 1c
/// frozen surface. The two implementations are byte-identical by design.
///
/// Returns each Text run as a separate `String` in document order. The
/// concrete representation matches `baseline.rs`'s shape: a `Vec<String>`
/// that `concat()`s to the lxml `''.join(elem.itertext())` string.
fn itertext(elem: &NodeRef) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_itertext(elem, &mut out);
    out
}

fn collect_itertext(node: &NodeRef, out: &mut Vec<String>) {
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let data = contents.borrow().to_string();
                if !data.is_empty() {
                    out.push(data);
                }
            }
            NodeData::Element { .. } => {
                collect_itertext(child, out);
            }
            _ => {}
        }
    }
}

// ===========================================================================
// Tests (Stage 2c-i unit tests — each traces to a Python line)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::{
        Dom, append_child as dom_append_child, create_element as dom_create_element,
        create_text_node, get_elements_by_tag_name,
    };

    fn parse(html: &str) -> Dom {
        Dom::parse(html)
    }

    fn body(dom: &Dom) -> NodeRef {
        dom.body().expect("html5ever synthesises <body>")
    }

    // -------------------------------------------------------------------
    // module constants (main_extractor.py:30-35)
    // -------------------------------------------------------------------

    #[test]
    fn module_constants_match_python() {
        // main_extractor.py:30 — P_FORMATTING = {'hi', 'ref'}
        assert_eq!(P_FORMATTING.len(), 2);
        assert!(P_FORMATTING.contains(&"hi"));
        assert!(P_FORMATTING.contains(&"ref"));

        // main_extractor.py:31 — TABLE_ELEMS = {'td', 'th'}
        assert_eq!(TABLE_ELEMS.len(), 2);
        assert!(TABLE_ELEMS.contains(&"td"));
        assert!(TABLE_ELEMS.contains(&"th"));

        // main_extractor.py:32 — TABLE_ALL = {'td', 'th', 'hi'}
        assert_eq!(TABLE_ALL.len(), 3);
        for t in ["td", "th", "hi"] {
            assert!(TABLE_ALL.contains(&t), "TABLE_ALL missing {t}");
        }

        // main_extractor.py:33 — FORMATTING = {'hi', 'ref', 'span'}
        assert_eq!(FORMATTING.len(), 3);
        for t in ["hi", "ref", "span"] {
            assert!(FORMATTING.contains(&t), "FORMATTING missing {t}");
        }

        // main_extractor.py:34 — CODES_QUOTES = {'code', 'quote'}
        assert_eq!(CODES_QUOTES.len(), 2);
        assert!(CODES_QUOTES.contains(&"code"));
        assert!(CODES_QUOTES.contains(&"quote"));

        // main_extractor.py:35 — NOT_AT_THE_END = {'head', 'ref'}
        assert_eq!(NOT_AT_THE_END.len(), 2);
        assert!(NOT_AT_THE_END.contains(&"head"));
        assert!(NOT_AT_THE_END.contains(&"ref"));
    }

    // -------------------------------------------------------------------
    // handle_titles (main_extractor.py:43-66)
    // -------------------------------------------------------------------

    #[test]
    fn handle_titles_no_children_goes_via_process_node() {
        // <head>Title text</head> — len(element) == 0 (no element kids),
        // so handle_titles delegates to process_node.
        let head = dom_create_element("head");
        dom_append_child(&head, &create_text_node("Title text"));
        // Attach to a parent so process_node can read .tail context.
        let parent_div = dom_create_element("div");
        dom_append_child(&parent_div, &head);
        let opts = Options::default();
        let out = handle_titles(&head, &opts).expect("process_node keeps the title");
        // The returned node should still hold the text.
        let joined: String = itertext(&out).concat();
        assert_eq!(joined.trim(), "Title text");
    }

    #[test]
    fn handle_titles_with_children_uses_deepcopy_path() {
        // <head>pre<span>inner</span>post</head> — has an element child,
        // so the deepcopy branch fires.
        let head = dom_create_element("head");
        dom_append_child(&head, &create_text_node("pre"));
        let span = dom_create_element("span");
        dom_append_child(&span, &create_text_node("inner"));
        dom_append_child(&head, &span);
        dom_append_child(&head, &create_text_node("post"));
        // Attach to a parent so the moved children have a "valid" original parent.
        let parent_div = dom_create_element("div");
        dom_append_child(&parent_div, &head);
        let opts = Options::default();
        let out = handle_titles(&head, &opts).expect("itertext non-empty -> Some");
        // The returned node's itertext should contain "inner" (the span content
        // survives via the deep-clone path + the move-into-title pass).
        let joined: String = itertext(&out).concat();
        assert!(joined.contains("inner"), "joined={joined:?}");
    }

    #[test]
    fn handle_titles_returns_none_when_no_text_chars() {
        // <head></head> — no children, no text, no tail → process_node
        // returns None → handle_titles propagates None.
        let head = dom_create_element("head");
        let opts = Options::default();
        assert!(handle_titles(&head, &opts).is_none());
    }

    // -------------------------------------------------------------------
    // handle_formatting (main_extractor.py:69-116)
    // -------------------------------------------------------------------

    #[test]
    fn handle_formatting_returns_none_when_process_node_none() {
        // Empty <hi/> — process_node sees no text/tail/children → None.
        let hi = dom_create_element("hi");
        let opts = Options::default();
        assert!(handle_formatting(&hi, &opts).is_none());
    }

    #[test]
    fn handle_formatting_wraps_in_p_when_parent_not_protected() {
        // Parent <div> — "div" is NOT in FORMATTING_PROTECTED, so wrap in <p>.
        let div = dom_create_element("div");
        let hi = dom_create_element("hi");
        dom_append_child(&hi, &create_text_node("bold"));
        dom_append_child(&div, &hi);
        let opts = Options::default();
        let out = handle_formatting(&hi, &opts).expect("formatting kept");
        // Output is a <p> wrapping the <hi>.
        assert_eq!(local_name(&out).as_deref(), Some("p"));
        let inner_hi = get_elements_by_tag_name(&out, "hi");
        assert_eq!(inner_hi.len(), 1);
    }

    #[test]
    fn handle_formatting_returns_naked_when_parent_protected() {
        // Parent <p> — "p" IS in FORMATTING_PROTECTED, so return the
        // naked formatting result.
        let p = dom_create_element("p");
        let hi = dom_create_element("hi");
        dom_append_child(&hi, &create_text_node("bold"));
        dom_append_child(&p, &hi);
        let opts = Options::default();
        let out = handle_formatting(&hi, &opts).expect("formatting kept");
        // Output IS the <hi> itself — naked.
        assert_eq!(local_name(&out).as_deref(), Some("hi"));
    }

    #[test]
    fn handle_formatting_wraps_when_no_parent_and_no_previous() {
        // Detached <hi> with no parent and no previous-sibling-element.
        // Both getparent() and getprevious() return None → "parent is None"
        // branch (main_extractor.py:111) fires → wrap in <p>.
        let hi = dom_create_element("hi");
        dom_append_child(&hi, &create_text_node("x"));
        let opts = Options::default();
        let out = handle_formatting(&hi, &opts).expect("formatting kept");
        // Wrap path fires (parent None, not protected).
        assert_eq!(local_name(&out).as_deref(), Some("p"));
    }

    #[test]
    fn handle_formatting_previous_element_sibling_facade_smoke() {
        // The Python branch `parent = element.getprevious()` (main_extractor.py:
        // 109-110) fires when `element.getparent()` is None. In lxml, an
        // element detached via Element.remove() retains a usable
        // getprevious() pointer; in rcdom, `remove()` clears the weak parent
        // pointer, so a detached NodeRef has no reachable previous sibling.
        // This unreachability is a documented divergence (see Stage 2b' docs
        // on `previous_element_sibling`). We therefore pin the FACADE
        // behaviour the production path depends on: when a node IS still
        // attached, `previous_element_sibling` returns the prior element
        // sibling, even when a Text node sits between them.
        //
        // The actual Python branch (parent=None, previous=Some) is reachable
        // only through Stage 2c-iii's recover_wild_text path; a behavioural
        // pin lives there.
        let root = dom_create_element("root");
        let div = dom_create_element("div");
        let hi = dom_create_element("hi");
        dom_append_child(&hi, &create_text_node("x"));
        dom_append_child(&root, &div);
        // Intersperse a Text node — previous_element_sibling must skip it.
        dom_append_child(&root, &create_text_node("between"));
        dom_append_child(&root, &hi);
        // Probe BEFORE handle_formatting (which would move <hi>).
        let prev = previous_element_sibling(&hi).expect("prev exists");
        assert_eq!(local_name(&prev).as_deref(), Some("div"));
        // Now run handle_formatting and verify the parent-branch is taken
        // (root.tag = "root", NOT protected → wrap in <p>).
        let opts = Options::default();
        let out = handle_formatting(&hi, &opts).expect("formatting kept");
        assert_eq!(local_name(&out).as_deref(), Some("p"));
    }

    #[test]
    fn handle_formatting_previous_element_sibling_finds_protected_p() {
        // Build `<root><p>prev</p>" between "<hi>x</hi></root>` and verify
        // previous_element_sibling(&hi) finds the <p> (which IS in
        // FORMATTING_PROTECTED). This pins the precondition that, when the
        // production path eventually reaches a (parent=None, previous=<p>)
        // shape via Stage 2c-iii, the previous-element fallback would
        // correctly identify a protected tag.
        let root = dom_create_element("root");
        let p_prev = dom_create_element("p");
        dom_append_child(&p_prev, &create_text_node("prev"));
        let hi = dom_create_element("hi");
        dom_append_child(&hi, &create_text_node("x"));
        dom_append_child(&root, &p_prev);
        dom_append_child(&root, &create_text_node(" between "));
        dom_append_child(&root, &hi);
        let prev = previous_element_sibling(&hi).expect("prev exists");
        assert_eq!(local_name(&prev).as_deref(), Some("p"));
        // Document the unreachability of the (parent=None, previous=Some)
        // case via this test's intentional non-call to handle_formatting.
        let _ = root;
    }

    // -------------------------------------------------------------------
    // add_sub_element (main_extractor.py:119-124)
    // -------------------------------------------------------------------

    #[test]
    fn add_sub_element_copies_text_tail_and_attrs() {
        // new_child_elem = <list/>
        let new_child_elem = dom_create_element("list");
        // subelem = <span class="A" id="B">[ignored payload]</span>
        // We need subelem to live in a parent so .tail can be read; for
        // attribute-copy testing the parent attachment is irrelevant.
        let subelem = dom_create_element("span");
        set_attribute(&subelem, "class", "A");
        set_attribute(&subelem, "id", "B");
        // processed_subchild = <item>text<tail-not-shown></item>  (text via
        // direct construction; tail via attachment under a temporary parent).
        let processed_subchild = dom_create_element("item");
        set_element_text(&processed_subchild, Some("PAYLOAD-TEXT"));
        // Attach to a temp parent so we can set .tail on processed_subchild.
        let tmp = dom_create_element("tmp");
        dom_append_child(&tmp, &processed_subchild);
        set_tail(&processed_subchild, Some("PAYLOAD-TAIL"));

        add_sub_element(&new_child_elem, &subelem, &processed_subchild);

        // new_child_elem now has one child: an <item> with the copied payload
        // text + tail, and class="A" id="B" attrs from subelem.
        let items = get_elements_by_tag_name(&new_child_elem, "item");
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(element_text(item).as_deref(), Some("PAYLOAD-TEXT"));
        assert_eq!(tail(item).as_deref(), Some("PAYLOAD-TAIL"));
        // Attributes from subelem (NOT from processed_subchild).
        assert_eq!(get_attribute(item, "class").as_deref(), Some("A"));
        assert_eq!(get_attribute(item, "id").as_deref(), Some("B"));
    }

    // -------------------------------------------------------------------
    // process_nested_elements (main_extractor.py:127-140)
    // -------------------------------------------------------------------

    #[test]
    #[should_panic(expected = "Stage 2c-ii: handle_lists")]
    fn process_nested_elements_dispatches_list_to_handle_lists_stub() {
        // Confirm the dispatch routes <list> descendants through
        // handle_lists — the Stage 2c-ii stub panics with the documented
        // message. This pins the dispatch shape; the actual return-value
        // semantics are Stage 2c-ii's responsibility.
        let child = dom_create_element("div");
        let list_elem = dom_create_element("list");
        dom_append_child(&child, &list_elem);
        let new_child_elem = dom_create_element("div");
        let opts = Options::default();
        process_nested_elements(&child, &new_child_elem, &opts);
    }

    #[test]
    fn process_nested_elements_renames_subelems_to_done() {
        // child = <div>x<span>y</span>z</div> — no <list> descendants;
        // process_nested_elements should walk the <span>, run it through
        // handle_textnode + add_sub_element, then rename to "done".
        let child = dom_create_element("div");
        dom_append_child(&child, &create_text_node("x"));
        let span = dom_create_element("span");
        dom_append_child(&span, &create_text_node("y"));
        dom_append_child(&child, &span);
        dom_append_child(&child, &create_text_node("z"));
        let new_child_elem = dom_create_element("div");
        let opts = Options::default();
        process_nested_elements(&child, &new_child_elem, &opts);
        // After the call, the original <span> has been renamed: no <span>
        // descendant remains in `child`. The new tag is "done".
        let spans = get_elements_by_tag_name(&child, "span");
        assert!(
            spans.is_empty(),
            "after process_nested_elements, no <span> should remain"
        );
        let dones = get_elements_by_tag_name(&child, "done");
        assert_eq!(dones.len(), 1, "exactly one descendant renamed to <done>");
        // new_child_elem.text was set to child.text ("x").
        assert_eq!(element_text(&new_child_elem).as_deref(), Some("x"));
    }

    // -------------------------------------------------------------------
    // update_elem_rendition (main_extractor.py:143-147)
    // -------------------------------------------------------------------

    #[test]
    fn update_elem_rendition_copies_rend_attr() {
        let elem = dom_create_element("hi");
        set_attribute(&elem, "rend", "#b");
        let new_elem = dom_create_element("hi");
        update_elem_rendition(&elem, &new_elem);
        assert_eq!(get_attribute(&new_elem, "rend").as_deref(), Some("#b"));
    }

    #[test]
    fn update_elem_rendition_no_op_when_no_rend() {
        let elem = dom_create_element("hi");
        // no rend attr
        let new_elem = dom_create_element("hi");
        update_elem_rendition(&elem, &new_elem);
        assert!(get_attribute(&new_elem, "rend").is_none());
    }

    // -------------------------------------------------------------------
    // is_text_element (main_extractor.py:149-151)
    // -------------------------------------------------------------------

    #[test]
    fn is_text_element_true_when_text_present() {
        let dom = parse("<p>hello</p>");
        let p = get_elements_by_tag_name(&body(&dom), "p")[0].clone();
        assert!(is_text_element(Some(&p)));
    }

    #[test]
    fn is_text_element_false_for_whitespace_only() {
        let dom = parse("<p>   \t\n</p>");
        let p = get_elements_by_tag_name(&body(&dom), "p")[0].clone();
        assert!(!is_text_element(Some(&p)));
    }

    #[test]
    fn is_text_element_false_for_none() {
        // The Python `elem is not None` guard.
        assert!(!is_text_element(None));
    }

    // -------------------------------------------------------------------
    // define_newelem (main_extractor.py:154-158)
    // -------------------------------------------------------------------

    #[test]
    fn define_newelem_no_op_when_processed_elem_is_none() {
        let orig = dom_create_element("div");
        define_newelem(None, &orig);
        // No child appended.
        assert_eq!(orig.children.borrow().len(), 0);
    }

    #[test]
    fn define_newelem_copies_tag_text_tail() {
        let orig = dom_create_element("div");
        let processed = dom_create_element("p");
        set_element_text(&processed, Some("HELLO"));
        // To set .tail on `processed`, it must be attached to a parent.
        let tmp = dom_create_element("tmp");
        dom_append_child(&tmp, &processed);
        set_tail(&processed, Some("TAIL"));

        define_newelem(Some(&processed), &orig);

        // orig now has a <p> child with text "HELLO" and tail "TAIL".
        let kids = get_elements_by_tag_name(&orig, "p");
        assert_eq!(kids.len(), 1);
        let p = &kids[0];
        assert_eq!(element_text(p).as_deref(), Some("HELLO"));
        assert_eq!(tail(p).as_deref(), Some("TAIL"));
    }
}
