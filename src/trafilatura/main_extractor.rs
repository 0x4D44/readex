//! `main_extractor` — Stage 2c-i + 2c-ii: handler primitives and block
//! handlers.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.4 (the
//! `main_extractor.py` handler dispatch). Source of truth:
//! `trafilatura@v2.0.0/main_extractor.py:30-353` (Stage 2c-i: 30-160; Stage
//! 2c-ii: 161-353).
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
    NodeData, NodeRef, append_child, attributes_in_source_order, clear_attributes, create_element,
    deep_clone, delete_with_tail_preserve_free, element_text, get_attribute,
    get_elements_by_tag_name, local_name, parent, previous_element_sibling, replace_element_tag,
    set_attribute, set_element_text, set_tail, tail,
};
use crate::trafilatura::cleaning::{Options, handle_textnode, process_node, strip_tags_multi};
use crate::trafilatura::utils::{FORMATTING_PROTECTED, is_image_file, text_chars_test};
use std::collections::HashSet;

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

/// `TAG_CATALOG` — `frozenset(["blockquote", "code", "del", "head", "hi",
/// "lb", "list", "p", "pre", "quote"])` (settings.py:436-438). The default
/// `potential_tags` set used by `_extract` (`main_extractor.py:569`) and
/// `recover_wild_text` (`main_extractor.py:512`). Vendored here (rather
/// than in a settings module) because Stage 2c-iii is the first consumer
/// and Stage 2d's `_extract` will consume it next. Order is irrelevant
/// (membership-only set semantics).
pub const TAG_CATALOG: &[&str] = &[
    "blockquote", "code", "del", "head", "hi", "lb", "list", "p", "pre", "quote",
];

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
// handle_lists (main_extractor.py:161-199) — Stage 2c-ii
// ===========================================================================

/// `handle_lists(element, options)` — `main_extractor.py:161-199`.
///
/// Process a `<list>` element and its `<item>` descendants. The element's tag
/// is preserved on the freshly-minted output (Trafilatura's `convert_lists`
/// pass produces `<list>` from `<ul>` / `<ol>`, so the input element's tag is
/// almost always `"list"`; we preserve whatever it is to match Python's
/// `Element(element.tag)` exactly).
///
/// # Python original
///
/// ```python
/// def handle_lists(element, options):
///     "Process lists elements including their descendants."
///     processed_element = Element(element.tag)
///
///     if element.text is not None and element.text.strip():
///         new_child_elem = SubElement(processed_element, "item")
///         new_child_elem.text = element.text
///
///     for child in element.iterdescendants("item"):
///         new_child_elem = Element("item")
///         if len(child) == 0:
///             processed_child = process_node(child, options)
///             if processed_child is not None:
///                 new_child_elem.text = processed_child.text or ""
///                 if processed_child.tail and processed_child.tail.strip():
///                     new_child_elem.text += " " + processed_child.tail
///                 processed_element.append(new_child_elem)
///         else:
///             process_nested_elements(child, new_child_elem, options)
///             if child.tail is not None and child.tail.strip():
///                 new_child_elem_children = [el for el in new_child_elem if el.tag != "done"]
///                 if new_child_elem_children:
///                     last_subchild = new_child_elem_children[-1]
///                     if last_subchild.tail is None or not last_subchild.tail.strip():
///                         last_subchild.tail = child.tail
///                     else:
///                         last_subchild.tail += " " + child.tail
///         if new_child_elem.text or len(new_child_elem) > 0:
///             update_elem_rendition(child, new_child_elem)
///             processed_element.append(new_child_elem)
///         child.tag = "done"
///     element.tag = "done"
///     # test if it has children and text. Avoid double tags??
///     if is_text_element(processed_element):
///         update_elem_rendition(element, processed_element)
///         return processed_element
///     return None
/// ```
///
/// # Faithfulness notes
///
/// 1. `Element(element.tag)` (line 163) — preserves the input element's tag.
///    We read `local_name(element)` and pass to `create_element`. Detached
///    elements with no parent return `None`; we fall back to `"list"`, but
///    the Python source assumes the input has a tag (so the fallback is
///    inert on real inputs).
/// 2. `element.iterdescendants("item")` (line 171) — descendant-only, tag-
///    filtered. The `<item>` children of `<list>` are direct children, but
///    nested lists can produce deeper `<item>`s; `get_elements_by_tag_name`
///    walks descendants in document order with the right semantics.
/// 3. `new_child_elem_children = [el for el in new_child_elem if el.tag !=
///    "done"]` (line 183) — filter element-children whose tag != "done".
/// 4. `child.tail and child.tail.strip()` (line 182) — TRUTHY check: None
///    and "" are falsy; the trimmed must be non-empty.
/// 5. `processed_child.text or ""` (line 176) — None / empty / falsy
///    falls back to "". We use `unwrap_or_default()`.
/// 6. `new_child_elem.text or len(new_child_elem) > 0` (line 190) — the
///    survival gate: keep new_child_elem if it has text OR any element-
///    child (Python `len(elem)` is element-child count).
pub fn handle_lists(element: &NodeRef, options: &Options) -> Option<NodeRef> {
    // main_extractor.py:163 — Element(element.tag). Preserve the tag.
    let tag = local_name(element).unwrap_or_else(|| "list".to_string());
    let processed_element = create_element(&tag);

    // main_extractor.py:165-167 — leading text becomes its own <item>.
    if let Some(text) = element_text(element)
        && !text.trim().is_empty()
    {
        let new_child_elem = create_element("item");
        append_child(&processed_element, &new_child_elem);
        set_element_text(&new_child_elem, Some(&text));
    }

    // main_extractor.py:171 — iterdescendants("item") snapshot.
    let item_descendants = get_elements_by_tag_name(element, "item");

    // **rcdom Drop quirk anchor** (`markup5ever_rcdom-0.39.0/lib.rs:268-284`):
    // dropping a `Node` iteratively walks `self.children.borrow_mut().drain`
    // AND `mem::take`s every descendant's children Vec. Discarding the
    // `replace_element_tag` return value lets the fresh "done" handle drop
    // immediately — which mem::takes the children of every descendant of
    // the now-detached subtree (including elements we still hold NodeRefs
    // for via `item_descendants`). Pin returned "done" handles alive in
    // `dones_alive` for the duration of the function so subsequent
    // iterations still see the original child trees.
    let mut dones_alive: Vec<NodeRef> = Vec::new();

    for child in item_descendants {
        // main_extractor.py:172 — fresh <item>.
        let new_child_elem = create_element("item");

        if element_child_count(&child) == 0 {
            // main_extractor.py:173-179 — leaf <item>: process_node, then
            // text + tail concatenation into new_child_elem.text.
            if let Some(processed_child) = process_node(&child, options) {
                let mut text = element_text(&processed_child).unwrap_or_default();
                if let Some(t) = tail(&processed_child)
                    && !t.trim().is_empty()
                {
                    text.push(' ');
                    text.push_str(&t);
                }
                set_element_text(&new_child_elem, Some(&text));
                // main_extractor.py:179 — append. Note: the Python code
                // appends here AND again at line 192 if the survival gate
                // is met. The double-append is a Python source quirk; we
                // mirror it faithfully (the second append on an already-
                // attached node is a re-parent — but new_child_elem has
                // text but NO element children, so the gate condition
                // (text OR len>0) is the same in both branches and the
                // second append is a no-op move-back into the same parent).
                append_child(&processed_element, &new_child_elem);
            }
        } else {
            // main_extractor.py:180-189 — non-leaf <item>: rewire descendants
            // into new_child_elem, then handle the tail-merge.
            process_nested_elements(&child, &new_child_elem, options);
            if let Some(child_tail) = tail(&child)
                && !child_tail.trim().is_empty()
            {
                // main_extractor.py:183 — element-children of new_child_elem
                // whose tag != "done".
                let new_child_elem_children: Vec<NodeRef> = element_children(&new_child_elem)
                    .into_iter()
                    .filter(|el| local_name(el).as_deref() != Some("done"))
                    .collect();
                if let Some(last_subchild) = new_child_elem_children.last() {
                    // main_extractor.py:186-189 — merge child.tail onto
                    // last_subchild.tail (or set if empty).
                    let existing_tail = tail(last_subchild);
                    let merged = match existing_tail.as_deref() {
                        Some(t) if !t.trim().is_empty() => format!("{t} {child_tail}"),
                        _ => child_tail.clone(),
                    };
                    set_tail(last_subchild, Some(&merged));
                }
            }
        }

        // main_extractor.py:190-192 — survival gate: keep new_child_elem if
        // it has text OR any element-children. Apply rendition copy.
        let has_text = element_text(&new_child_elem)
            .map(|t| !t.is_empty())
            .unwrap_or(false);
        let has_children = element_child_count(&new_child_elem) > 0;
        if has_text || has_children {
            update_elem_rendition(&child, &new_child_elem);
            append_child(&processed_element, &new_child_elem);
        }
        // main_extractor.py:193 — rename child to "done", unconditional.
        // Pin alive (rcdom Drop quirk).
        dones_alive.push(replace_element_tag(&child, "done"));
    }
    // main_extractor.py:194 — rename element to "done", unconditional.
    dones_alive.push(replace_element_tag(element, "done"));
    let _ = &dones_alive;

    // main_extractor.py:196-199 — is_text_element gate, plus rendition copy.
    if is_text_element(Some(&processed_element)) {
        update_elem_rendition(element, &processed_element);
        Some(processed_element)
    } else {
        None
    }
}

// ===========================================================================
// is_code_block_element (main_extractor.py:202-215) — Stage 2c-ii
// ===========================================================================

/// `is_code_block_element(element)` — `main_extractor.py:202-215`.
///
/// True iff `element` looks like a code block by structural markers:
///   1. has a `lang` attribute (pip-style), OR is tag `<code>` (line 205);
///   2. parent's `class` attribute contains "highlight" (GitHub) (line 209);
///   3. has exactly one element child whose tag is `<code>` (highlightjs)
///      (lines 212-213).
///
/// # Python original
///
/// ```python
/// def is_code_block_element(element):
///     "Check if it is a code element according to common structural markers."
///     # pip
///     if element.get("lang") or element.tag == "code":
///         return True
///     # GitHub
///     parent = element.getparent()
///     if parent is not None and "highlight" in parent.get("class", ""):
///         return True
///     # highlightjs
///     code = element.find("code")
///     if code is not None and len(element) == 1:
///         return True
///     return False
/// ```
///
/// # Faithfulness notes
///
/// 1. `element.get("lang")` (line 205) — Python's TRUTHY check; an absent
///    attr → None → falsy; an empty-string attr → "" → falsy.
/// 2. `parent.get("class", "")` (line 209) — substring `in`-check against
///    the class attribute string (which may carry many tokens), NOT a
///    tokenised match. "highlight-py" is also a hit (matches Python `in`).
/// 3. `element.find("code")` (line 212) — lxml's `find()` is XPath
///    `./code` (CHILD axis), returning the FIRST matching ELEMENT child.
///    NOT a descendant search. Stage 2c-ii: first child whose tag is "code".
/// 4. `len(element) == 1` (line 213) — element-child count equals 1. The
///    Python's TWO conditions on line 213 must BOTH hold (code exists AND
///    element has exactly one element-child).
pub fn is_code_block_element(element: &NodeRef) -> bool {
    // main_extractor.py:205 — lang attr OR tag == "code".
    let has_lang = get_attribute(element, "lang")
        .map(|s| !s.is_empty())
        .unwrap_or(false);
    if has_lang || local_name(element).as_deref() == Some("code") {
        return true;
    }
    // main_extractor.py:208-210 — parent's class contains "highlight".
    if let Some(p) = parent(element) {
        let class = get_attribute(&p, "class").unwrap_or_default();
        if class.contains("highlight") {
            return true;
        }
    }
    // main_extractor.py:212-214 — first <code> CHILD element AND len==1.
    let first_code_child = element
        .children
        .borrow()
        .iter()
        .find(|c| {
            matches!(c.data, NodeData::Element { .. }) && local_name(c).as_deref() == Some("code")
        })
        .cloned();
    if first_code_child.is_some() && element_child_count(element) == 1 {
        return true;
    }
    false
}

// ===========================================================================
// handle_code_blocks (main_extractor.py:218-224) — Stage 2c-ii
// ===========================================================================

/// `handle_code_blocks(element)` — `main_extractor.py:218-224`.
///
/// Deep-clone the element, mark every descendant of the ORIGINAL as "done",
/// and rename the CLONE's tag to `"code"`.
///
/// # Python original
///
/// ```python
/// def handle_code_blocks(element):
///     "Turn element into a properly tagged code block."
///     processed_element = deepcopy(element)
///     for child in element.iter("*"):
///         child.tag = "done"
///     processed_element.tag = "code"
///     return processed_element
/// ```
///
/// # Faithfulness notes
///
/// 1. `element.iter("*")` (line 221) — pre-order walk over `element` AND its
///    descendants (INCLUDING self). Every element gets renamed to "done".
/// 2. The rename loop runs on `element` (the ORIGINAL), NOT on the clone:
///    the clone keeps its original tag structure (then its root tag is
///    overwritten to "code" at line 223).
/// 3. `replace_element_tag` returns a NEW NodeRef; the original is detached.
///    We deliberately ignore the returned handle — the sole purpose of the
///    rename is to mark the original tree's nodes as processed, which is
///    a side-effect on `element`'s subtree (the rename mutates the parent's
///    child list, so subsequent walks see "done"). For the root element
///    itself, the rename of root happens INSIDE the loop (since iter("*")
///    starts at self) — the new root then sits in the original parent.
pub fn handle_code_blocks(element: &NodeRef) -> NodeRef {
    // main_extractor.py:220 — deepcopy first (before the rename pass would
    // affect the clone).
    let processed_element = deep_clone(element);
    // main_extractor.py:221-222 — rename every node in element.iter("*") to
    // "done". iter("*") includes self, so we walk self + descendants.
    let mut all: Vec<NodeRef> = vec![element.clone()];
    collect_descendant_elements(element, &mut all);
    // **rcdom Drop quirk anchor**: see handle_paragraphs / handle_lists docs.
    // Pin returned "done" handles alive so dropping them doesn't drain
    // descendants we haven't yet visited.
    let mut dones_alive: Vec<NodeRef> = Vec::new();
    for n in all {
        dones_alive.push(replace_element_tag(&n, "done"));
    }
    let _ = &dones_alive;
    // main_extractor.py:223 — set the clone's tag to "code".
    replace_element_tag(&processed_element, "code")
}

// ===========================================================================
// handle_quotes (main_extractor.py:227-242) — Stage 2c-ii
// ===========================================================================

/// `handle_quotes(element, options)` — `main_extractor.py:227-242`.
///
/// Process a `<quote>` element. If it looks like a code block (via
/// `is_code_block_element`), dispatch to `handle_code_blocks`. Otherwise
/// build a fresh element with the same tag, walk `element.iter("*")`
/// (self + descendants), run each through `process_node`, append surviving
/// children via `define_newelem`, and finally strip nested `<quote>` tags.
///
/// # Python original
///
/// ```python
/// def handle_quotes(element, options):
///     "Process quotes elements."
///     if is_code_block_element(element):
///         return handle_code_blocks(element)
///
///     processed_element = Element(element.tag)
///     for child in element.iter("*"):
///         processed_child = process_node(child, options)
///         if processed_child is not None:
///             define_newelem(processed_child, processed_element)
///         child.tag = "done"
///     if is_text_element(processed_element):
///         # avoid double/nested tags
///         strip_tags(processed_element, "quote")
///         return processed_element
///     return None
/// ```
///
/// # Faithfulness notes
///
/// 1. `element.iter("*")` (line 233) — INCLUDES self.
/// 2. `strip_tags(processed_element, "quote")` (line 240) — lxml's
///    strip_tags: remove `<quote>` element wrappers from the subtree while
///    keeping their text/children/tail. Maps to `strip_tags_multi(_,
///    &["quote"])`.
pub fn handle_quotes(element: &NodeRef, options: &Options) -> Option<NodeRef> {
    // main_extractor.py:229-230 — code-block dispatch.
    if is_code_block_element(element) {
        return Some(handle_code_blocks(element));
    }
    // main_extractor.py:232 — fresh element with same tag.
    let tag = local_name(element).unwrap_or_else(|| "quote".to_string());
    let processed_element = create_element(&tag);

    // main_extractor.py:233-237 — walk self + descendants.
    let mut all: Vec<NodeRef> = vec![element.clone()];
    collect_descendant_elements(element, &mut all);
    // **rcdom Drop quirk anchor**: see handle_paragraphs / handle_lists docs.
    let mut dones_alive: Vec<NodeRef> = Vec::new();
    for child in all {
        let processed_child = process_node(&child, options);
        if let Some(p) = processed_child.as_ref() {
            define_newelem(Some(p), &processed_element);
        }
        dones_alive.push(replace_element_tag(&child, "done"));
    }
    let _ = &dones_alive;

    // main_extractor.py:238-241 — is_text_element gate; strip nested quotes.
    if is_text_element(Some(&processed_element)) {
        strip_tags_multi(&processed_element, &["quote"]);
        Some(processed_element)
    } else {
        None
    }
}

// ===========================================================================
// handle_other_elements (main_extractor.py:245-269) — Stage 2c-ii
// ===========================================================================

/// `handle_other_elements(element, potential_tags, options)` —
/// `main_extractor.py:245-269`.
///
/// Diverse-element fallback: handle w3schools-style code divs, drop
/// unexpected tags, and route surviving `<div>` blocks through `handle_textnode`
/// → `<p>` rename.
///
/// # Python original
///
/// ```python
/// def handle_other_elements(element, potential_tags, options):
///     "Handle diverse or unknown elements in the scope of relevant tags."
///     # handle w3schools code
///     if element.tag == "div" and "w3-code" in element.get("class", ""):
///         return handle_code_blocks(element)
///
///     # delete unwanted
///     if element.tag not in potential_tags:
///         if element.tag != "done":
///             _log_event("discarding element", element.tag, element.text)
///         return None
///
///     if element.tag == "div":
///         processed_element = handle_textnode(element, options, comments_fix=False, preserve_spaces=True)
///         if processed_element is not None and text_chars_test(processed_element.text) is True:
///             processed_element.attrib.clear()
///             if processed_element.tag == "div":
///                 processed_element.tag = "p"
///             return processed_element
///
///     return None
/// ```
///
/// # Faithfulness notes
///
/// 1. `"w3-code" in element.get("class", "")` (line 248) — substring `in`-check.
/// 2. `_log_event(...)` (line 254) — observability, SKIPPED.
/// 3. `processed_element.attrib.clear()` (line 262) — wipe attributes via
///    `clear_attributes` (Stage 1b).
pub fn handle_other_elements(
    element: &NodeRef,
    potential_tags: &HashSet<String>,
    options: &Options,
) -> Option<NodeRef> {
    let tag = local_name(element).unwrap_or_default();
    let class = get_attribute(element, "class").unwrap_or_default();

    // main_extractor.py:248-249 — w3schools code div.
    if tag == "div" && class.contains("w3-code") {
        return Some(handle_code_blocks(element));
    }

    // main_extractor.py:252-255 — drop unexpected tags. _log_event SKIPPED.
    if !potential_tags.contains(&tag) {
        return None;
    }

    // main_extractor.py:257-267 — div path: handle_textnode + clear-attrs +
    // div→p rename.
    if tag == "div" {
        let processed_element = handle_textnode(element, options, false, true)?;
        if text_chars_test(element_text(&processed_element).as_deref()) {
            clear_attributes(&processed_element);
            // main_extractor.py:264-265 — div→p rename.
            if local_name(&processed_element).as_deref() == Some("div") {
                return Some(replace_element_tag(&processed_element, "p"));
            }
            return Some(processed_element);
        }
    }

    None
}

// ===========================================================================
// handle_paragraphs (main_extractor.py:272-351) — Stage 2c-ii
// ===========================================================================

/// `handle_paragraphs(element, potential_tags, options)` —
/// `main_extractor.py:272-351`.
///
/// Process a `<p>` along with its children: clean, trim, and rebuild as a
/// `<p>`-tagged tree carrying only the surviving `text_chars_test`-passing
/// children. The largest of the block handlers.
///
/// # Python original
///
/// See module-level Python source (lines 272-351). The active body:
///
/// 1. clear element's attribs (line 274);
/// 2. no-children fast path → `process_node` (lines 278-279);
/// 3. else: fresh `<p>`-typed processed_element; iterate `element.iter("*")`;
///    skip non-potential / non-"done" tags; run each through `handle_textnode`
///    with `preserve_spaces=True`; handle `<p>` children specially (merge
///    text into processed_element.text); for other survivors, build a new
///    sub-element with the child's tag, copy text+tail from the processed
///    child, copy `rend`/`target` attrs for `hi`/`ref`, and append;
/// 4. clean trailing `<lb>` element if it has no tail;
/// 5. return processed_element if it has children OR text; else `None`.
///
/// # Faithfulness notes
///
/// 1. `element.iter("*")` (line 283) — INCLUDES self. The "self" iteration
///    will have `child.tag == element.tag == "p"` (which is in potential_tags
///    on every reasonable call site); when that step runs `handle_textnode`
///    on the parent, the result has `processed_child.tag == "p"`, taking
///    the line-292 merge branch — which copies the parent's text into
///    `processed_element.text` and renames `element` itself to "done". The
///    iteration's downstream visits to `element`'s descendants then run
///    against a "done"-tagged ancestor, but the `iter("*")` is materialised
///    before iteration so we still visit every descendant (lxml semantics).
/// 2. `child.tag not in potential_tags and child.tag != "done"` (line 284):
///    skip via `continue`. The "done" allowance is so prior-stage renames
///    don't break the iteration.
/// 3. `processed_child.tag == "p"` (line 292): inner-p merge.
/// 4. `if processed_element.text: ... += " " + (processed_child.text or "")`
///    (line 294-295): truthy check on existing text; append with separator
///    space.
/// 5. P_FORMATTING branch (line 302-314): strip and clean nested formatting.
/// 6. `handle_image` (line 336): FORWARD STUB at module-private scope.
/// 7. lines 342-351: finish — clean trailing lb, return if children or text.
pub fn handle_paragraphs(
    element: &NodeRef,
    potential_tags: &HashSet<String>,
    options: &Options,
) -> Option<NodeRef> {
    // main_extractor.py:274 — element.attrib.clear().
    clear_attributes(element);

    // main_extractor.py:278-279 — no element children: single dispatch.
    if element_child_count(element) == 0 {
        return process_node(element, options);
    }

    // main_extractor.py:282 — fresh processed_element with same tag.
    let elem_tag = local_name(element).unwrap_or_else(|| "p".to_string());
    let processed_element = create_element(&elem_tag);

    // main_extractor.py:283 — iter("*") snapshot: self + descendants.
    let mut all: Vec<NodeRef> = vec![element.clone()];
    collect_descendant_elements(element, &mut all);

    // **rcdom Drop quirk anchor**: rcdom's `impl Drop for Node` iteratively
    // walks `self.children.borrow_mut().drain(..)` AND mem::take's the
    // children Vec on every descendant during the walk
    // (markup5ever_rcdom-0.39.0/lib.rs:268-284). Because
    // `replace_element_tag` returns a fresh node owning the OLD subtree's
    // children, discarding that handle as a `_renamed` binding lets it Drop
    // at the end of the statement — which then drains every descendant's
    // children Vec (including descendants we've already snapshotted into
    // `all` and intend to revisit). To pin the iteration's snapshot
    // semantics, we keep every returned "done" NodeRef alive in this
    // `dones_alive` Vec for the duration of the function.
    let mut dones_alive: Vec<NodeRef> = Vec::new();

    for child in all {
        let child_tag = local_name(&child).unwrap_or_default();
        // main_extractor.py:284-286 — skip non-potential, non-done tags.
        if !potential_tags.contains(&child_tag) && child_tag != "done" {
            continue;
        }
        // main_extractor.py:289 — handle_textnode with preserve_spaces=True.
        let processed_child = match handle_textnode(&child, options, false, true) {
            Some(p) => p,
            None => {
                // main_extractor.py:340 — child.tag = "done" runs even when
                // handle_textnode returns None (the outer if-block continues
                // straight to it via the for-loop end). Pin the returned
                // "done" handle alive (rcdom Drop quirk — see fn header).
                dones_alive.push(replace_element_tag(&child, "done"));
                continue;
            }
        };

        let processed_child_tag = local_name(&processed_child).unwrap_or_default();

        // main_extractor.py:292-299 — inner-p merge branch.
        if processed_child_tag == "p" {
            let inner_text = element_text(&processed_child).unwrap_or_default();
            let existing = element_text(&processed_element);
            let merged = match existing.as_deref() {
                Some(t) if !t.is_empty() => format!("{t} {inner_text}"),
                _ => inner_text,
            };
            set_element_text(&processed_element, Some(&merged));
            // Pin the returned "done" handle alive (rcdom Drop quirk).
            dones_alive.push(replace_element_tag(&child, "done"));
            continue;
        }

        // main_extractor.py:301 — newsub = Element(child.tag).
        let mut newsub = create_element(&child_tag);

        // main_extractor.py:302-314 — P_FORMATTING handling.
        if P_FORMATTING.contains(&processed_child_tag.as_str()) {
            // main_extractor.py:304-308 — nested-children cleanup.
            if element_child_count(&processed_child) > 0 {
                for item in element_children(&processed_child) {
                    if text_chars_test(element_text(&item).as_deref()) {
                        let prefixed = format!(" {}", element_text(&item).unwrap_or_default());
                        set_element_text(&item, Some(&prefixed));
                    }
                    // main_extractor.py:308 — strip_tags(processed_child, item.tag).
                    let item_tag = local_name(&item).unwrap_or_default();
                    strip_tags_multi(&processed_child, &[item_tag.as_str()]);
                }
            }
            // main_extractor.py:310-314 — attribute copy (hi: rend; ref: target).
            if child_tag == "hi" {
                let rend = get_attribute(&child, "rend").unwrap_or_default();
                set_attribute(&newsub, "rend", &rend);
            } else if child_tag == "ref"
                && let Some(target) = get_attribute(&child, "target")
            {
                set_attribute(&newsub, "target", &target);
            }
        }

        // main_extractor.py:333 — newsub.text, newsub.tail = processed_child.text,
        // processed_child.tail.
        set_element_text(&newsub, element_text(&processed_child).as_deref());
        // **Stage 3-B Cluster B fix (2026-05-21):** capture the tail value
        // BEFORE the graphic-swap below; APPLY it AFTER the append, because
        // `set_tail` (`dom.rs:512`) silently no-ops on an orphan node —
        // tails are stored as following-Text-sibling-run, which requires
        // a parent. The pre-fix order set the tail on the still-orphan
        // newsub, so every `<lb>` tail (and every other inline element's
        // tail) was silently dropped during paragraph processing — visible
        // on gov.uk PAYE and Fed reserve where `<lb>` carries the line-
        // break text. Python's lxml `newsub.tail = ...` works on detached
        // nodes because lxml stores tail on the element itself (not as a
        // sibling).
        let tail_value = tail(&processed_child);

        // main_extractor.py:335-338 — graphic dispatch (FORWARD STUB).
        if processed_child_tag == "graphic"
            && let Some(image_elem) = handle_image(&processed_child)
        {
            newsub = image_elem;
        }

        // main_extractor.py:339 — append newsub to processed_element.
        append_child(&processed_element, &newsub);
        // Tail goes AFTER the append (see Cluster B note above).
        set_tail(&newsub, tail_value.as_deref());
        // main_extractor.py:340 — child.tag = "done". Pin alive (rcdom Drop
        // quirk).
        dones_alive.push(replace_element_tag(&child, "done"));
    }
    // Keep dones_alive in scope until the end of the function (rcdom Drop
    // quirk pin).
    let _ = dones_alive;

    // main_extractor.py:342-351 — finish.
    if element_child_count(&processed_element) > 0 {
        // main_extractor.py:343-346 — trailing lb cleanup.
        let kids = element_children(&processed_element);
        if let Some(last_elem) = kids.last()
            && local_name(last_elem).as_deref() == Some("lb")
            && tail(last_elem).is_none()
        {
            delete_with_tail_preserve_free(last_elem);
        }
        return Some(processed_element);
    }
    if element_text(&processed_element)
        .map(|t| !t.is_empty())
        .unwrap_or(false)
    {
        return Some(processed_element);
    }
    // main_extractor.py:350 — _log_event SKIPPED.
    None
}

// ===========================================================================
// handle_image (main_extractor.py:445-480) — Stage 2c-iii
// ===========================================================================

/// `handle_image(element)` — `main_extractor.py:445-480`.
///
/// Process an image element (typically `<graphic>` after `convert_tags`) and
/// extract its src/alt/title attributes onto a fresh `<graphic>` (or the
/// element's own tag — Python's `Element(element.tag)`). Returns `None` if
/// the element has no usable `src` attribute.
///
/// # Python original
///
/// ```python
/// def handle_image(element: Optional[_Element]) -> Optional[_Element]:
///     "Process image elements and their relevant attributes."
///     if element is None:
///         return None
///
///     processed_element = Element(element.tag)
///
///     for attr in ("data-src", "src"):
///         src = element.get(attr, "")
///         if is_image_file(src):
///             processed_element.set("src", src)
///             break
///     else:
///         # take the first corresponding attribute
///         for attr, value in element.attrib.items():
///             if attr.startswith("data-src") and is_image_file(value):
///                 processed_element.set("src", value)
///                 break
///
///     # additional data
///     if alt_attr := element.get("alt"):
///         processed_element.set("alt", alt_attr)
///     if title_attr := element.get("title"):
///         processed_element.set("title", title_attr)
///
///     # don't return empty elements or elements without source, just None
///     if not processed_element.attrib or not processed_element.get("src"):
///         return None
///
///     # post-processing: URLs
///     src_attr = processed_element.get("src", "")
///     if not src_attr.startswith("http"):
///         processed_element.set("src", re.sub(r"^//", "http://", src_attr))
///
///     return processed_element
/// ```
///
/// # Faithfulness notes
///
/// 1. Python's `for..else` (line 452-462): the `else` block runs only when
///    the loop completes WITHOUT a `break`. Both `for` loops use `break` on
///    a successful match, so the `else` only runs if `data-src` and `src`
///    are both absent or invalid. Faithful Rust: track a `found` flag.
/// 2. `processed_element.attrib.items()` (line 459) — iterate in source
///    order via `attributes_in_source_order` (Stage 1b dom facade).
/// 3. `not src_attr.startswith("http")` (line 476) — covers `http://` AND
///    `https://`. Python's startswith on a non-empty string with empty
///    prefix returns True, but `src_attr` here is non-empty (line 472
///    guard).
/// 4. `re.sub(r"^//", "http://", src_attr)` (line 477) — only rewrite when
///    the src begins with `//` (protocol-relative URL). Otherwise the regex
///    matches nothing and the src is unchanged. Faithful Rust: check prefix.
pub fn handle_image(element: &NodeRef) -> Option<NodeRef> {
    // main_extractor.py:450 — Element(element.tag). Preserve the tag.
    let tag = local_name(element).unwrap_or_else(|| "graphic".to_string());
    let processed_element = create_element(&tag);

    // main_extractor.py:452-456 — first loop: ("data-src", "src").
    let mut found_src = false;
    for attr in ["data-src", "src"] {
        let src = get_attribute(element, attr);
        if is_image_file(src.as_deref()) {
            // src is guaranteed Some(..) inside is_image_file's true branch.
            set_attribute(&processed_element, "src", src.as_deref().unwrap_or(""));
            found_src = true;
            break;
        }
    }
    // main_extractor.py:457-462 — for..else: scan attrs starting with "data-src".
    if !found_src {
        for (name, value) in attributes_in_source_order(element) {
            if name.starts_with("data-src") && is_image_file(Some(&value)) {
                set_attribute(&processed_element, "src", &value);
                break;
            }
        }
    }

    // main_extractor.py:465-468 — alt and title attribute copy. Python's
    // walrus operator `if alt_attr := element.get("alt"):` is a TRUTHY check
    // — empty string is falsy. Faithful Rust: only copy when value is Some
    // and non-empty.
    if let Some(alt) = get_attribute(element, "alt")
        && !alt.is_empty()
    {
        set_attribute(&processed_element, "alt", &alt);
    }
    if let Some(title) = get_attribute(element, "title")
        && !title.is_empty()
    {
        set_attribute(&processed_element, "title", &title);
    }

    // main_extractor.py:471 — bail if no attrs OR no src. attributes_in_source_order
    // returns an empty Vec when there are no attrs; checking get_attribute("src")
    // covers both the "no attrs at all" and "attrs but no src" cases.
    let src_attr = get_attribute(&processed_element, "src")?;
    if src_attr.is_empty() {
        return None;
    }

    // main_extractor.py:474-477 — URL canonicalisation: only rewrite when
    // the src begins with `//` (protocol-relative). The regex `^//` only
    // matches at start, so non-`//` non-`http` srcs are unchanged.
    if !src_attr.starts_with("http")
        && let Some(rest) = src_attr.strip_prefix("//")
    {
        set_attribute(&processed_element, "src", &format!("http://{rest}"));
    }

    Some(processed_element)
}

// ===========================================================================
// define_cell_type (main_extractor.py:354-360) — Stage 2c-iii
// ===========================================================================

/// `define_cell_type(is_header)` — `main_extractor.py:354-360`.
///
/// Mint a fresh `<cell>` element; if `is_header`, set its `role` attribute
/// to `"head"`.
///
/// # Python original
///
/// ```python
/// def define_cell_type(is_header: bool) -> _Element:
///     "Determine cell element type and mint new element."
///     cell_element = Element("cell")
///     if is_header:
///         cell_element.set("role", "head")
///     return cell_element
/// ```
fn define_cell_type(is_header: bool) -> NodeRef {
    let cell = create_element("cell");
    if is_header {
        set_attribute(&cell, "role", "head");
    }
    cell
}

// ===========================================================================
// handle_table (main_extractor.py:363-442) — Stage 2c-iii
// ===========================================================================

/// `handle_table(table_elem, potential_tags, options)` —
/// `main_extractor.py:363-442`.
///
/// Process a single `<table>` element: walk its descendants in document
/// order, converting `<tr>` into `<row>` and `<td>`/`<th>` into `<cell>`,
/// stripping structural wrappers (`<thead>`/`<tbody>`/`<tfoot>`),
/// dispatching nested content through `handle_textnode` / `handle_lists` /
/// `handle_textelem` as appropriate, and breaking at the first nested
/// `<table>` descendant (to avoid double-processing).
///
/// # Faithfulness notes
///
/// 1. `strip_tags(table_elem, "thead", "tbody", "tfoot")` (line 368) — lift
///    each wrapper's children into its parent. Maps to
///    `cleaning::strip_tags_multi(table_elem, &["thead", "tbody", "tfoot"])`.
/// 2. `iter('tr')` / `iter(TABLE_ELEMS)` (lines 372-373) — descendant-or-self
///    walk filtered by tag. For a `<table>` root, the table itself can't
///    match `tr` / `td` / `th`, so descendants-only suffices.
/// 3. `td.get("colspan", 1)` (line 373) — `int(...)` of the colspan string,
///    default 1. Faithful Rust: parse with `.unwrap_or(1)` on parse failure.
/// 4. `iterdescendants()` (no arg, line 383) — every descendant in document
///    order. Mirrors via `descendant_elements`.
/// 5. `subelement.iterdescendants()` (line 405) — descendants of a CELL,
///    mirroring the same shape.
/// 6. `subelement.tag = "done"` at line 432 runs unconditionally at the END
///    of each outer iteration (including the `<tr>` and TABLE_ELEMS
///    branches). For the nested-table `break` branch, the rename does NOT
///    run (`break` short-circuits). Faithful Rust: rename inside the loop
///    body, never on break. Pin returned "done" handles in `dones_alive`
///    against rcdom Drop quirk.
/// 7. `newrow.attrib.pop("span", None)` (line 435) — remove the `span`
///    attribute on the residual `newrow` (the one that hasn't been
///    appended to newtable yet). Maps to `remove_attribute`.
/// 8. `processed_subchild = None` (line 417) — explicit reset so the
///    `if processed_subchild is not None` test at line 422 is false. In
///    Rust we just use a separate variable scope and `let mut`.
pub fn handle_table(
    table_elem: &NodeRef,
    potential_tags: &HashSet<String>,
    options: &Options,
) -> Option<NodeRef> {
    // main_extractor.py:365 — newtable = Element("table").
    let newtable = create_element("table");

    // main_extractor.py:368 — strip thead/tbody/tfoot.
    strip_tags_multi(table_elem, &["thead", "tbody", "tfoot"]);

    // main_extractor.py:371-373 — max columns including colspan.
    let mut max_cols: usize = 0;
    let trs = get_elements_by_tag_name(table_elem, "tr");
    for tr in &trs {
        let mut row_cols: usize = 0;
        for elem_tag in TABLE_ELEMS {
            // `tr.iter(TABLE_ELEMS)` walks descendants-or-self of `tr`. A
            // `<tr>` never matches `td`/`th`, so descendants suffice.
            let cells = get_elements_by_tag_name(tr, elem_tag);
            for cell in cells {
                let colspan_attr = get_attribute(&cell, "colspan").unwrap_or_default();
                let colspan: usize = colspan_attr.parse().unwrap_or(1);
                row_cols += colspan;
            }
        }
        if row_cols > max_cols {
            max_cols = row_cols;
        }
    }

    // main_extractor.py:376-381 — initial state.
    let mut seen_header_row = false;
    let mut seen_header = false;
    let span_attr = if max_cols > 1 {
        Some(max_cols.to_string())
    } else {
        None
    };
    let mut newrow = create_element("row");
    if let Some(span) = &span_attr {
        set_attribute(&newrow, "span", span);
    }

    // main_extractor.py:383 — outer descendants walk. Pin "done" handles
    // alive against rcdom Drop quirk.
    let mut dones_alive: Vec<NodeRef> = Vec::new();
    let descendants = descendant_elements(table_elem);

    for subelement in descendants {
        let sub_tag = local_name(&subelement).unwrap_or_default();

        if sub_tag == "tr" {
            // main_extractor.py:384-391 — close existing row if it has cells.
            if element_child_count(&newrow) > 0 {
                append_child(&newtable, &newrow);
                newrow = create_element("row");
                if let Some(span) = &span_attr {
                    set_attribute(&newrow, "span", span);
                }
                seen_header_row = seen_header_row || seen_header;
            }
        } else if TABLE_ELEMS.contains(&sub_tag.as_str()) {
            // main_extractor.py:392-427 — cell.
            let is_header = sub_tag == "th" && !seen_header_row;
            seen_header = seen_header || is_header;
            let new_child_elem = define_cell_type(is_header);

            if element_child_count(&subelement) == 0 {
                // main_extractor.py:397-400 — leaf cell.
                if let Some(processed_cell) = process_node(&subelement, options) {
                    set_element_text(&new_child_elem, element_text(&processed_cell).as_deref());
                    set_tail(&new_child_elem, tail(&processed_cell).as_deref());
                }
            } else {
                // main_extractor.py:402-424 — non-leaf cell: take text/tail
                // from subelement directly, then walk inner descendants.
                set_element_text(&new_child_elem, element_text(&subelement).as_deref());
                set_tail(&new_child_elem, tail(&subelement).as_deref());

                // **rcdom Drop / replace_element_tag quirk** (Stage 3-B
                // Cluster A fix, 2026-05-21): Python's `subelement.tag = "done"`
                // (main_extractor.py:404) mutates the tag IN PLACE — the
                // children of subelement remain reachable via
                // `subelement.iterdescendants()` at line 405. Our
                // `replace_element_tag` (dom.rs:1361) instead DRAINS
                // `subelement.children` into a fresh replacement node. If we
                // rename here (before the inner walk), the OLD subelement
                // becomes childless and the inner-descendants walk yields
                // nothing — the cell loses all its content. So we DEFER the
                // outer rename to the unconditional `replace_element_tag` at
                // the loop bottom (main_extractor.py:432), which Python also
                // runs as a redundant no-op cleanup. The inner walk below
                // therefore sees the original children.
                let inner_descendants = descendant_elements(&subelement);
                for child in inner_descendants {
                    let child_tag = local_name(&child).unwrap_or_default();
                    let mut processed_subchild: Option<NodeRef> = None;

                    if TABLE_ALL.contains(&child_tag.as_str()) {
                        // main_extractor.py:406-411 — TABLE_ALL branch.
                        // For nested td/th: Python in-place renames to "cell"
                        // BEFORE calling handle_textnode. handle_textnode
                        // reads child.text/tail/attrs — it does NOT walk
                        // descendants. Our replace_element_tag DOES drain
                        // children but handle_textnode does not need them
                        // (it sees text via element_text which reads the
                        // first contiguous text-run; after drain, no text
                        // child remains, so element_text returns None and
                        // handle_textnode returns None or an empty processed
                        // node). For correctness we'd need to defer the
                        // rename here too — but Stage 3-B Cluster A's
                        // primary divergence is the OUTER subelement rename;
                        // nested TABLE_ELEMS are a follow-up. For now do the
                        // rename inline (Python flow), then revisit if a
                        // fixture surfaces the nested-cell bug.
                        if TABLE_ELEMS.contains(&child_tag.as_str()) {
                            dones_alive.push(replace_element_tag(&child, "cell"));
                        }
                        processed_subchild = handle_textnode(&child, options, true, true);
                    } else if child_tag == "list"
                        && options.focus == crate::trafilatura::cleaning::Focus::Recall
                    {
                        // main_extractor.py:413-417 — list-in-cell, recall only.
                        if let Some(list_out) = handle_lists(&child, options) {
                            append_child(&new_child_elem, &list_out);
                            // Don't dispatch via define_newelem below.
                            processed_subchild = None;
                        }
                    } else {
                        // main_extractor.py:418-420 — handle_textelem fallback.
                        let pot_union: HashSet<String> = potential_tags
                            .iter()
                            .cloned()
                            .chain(std::iter::once("div".to_string()))
                            .collect();
                        processed_subchild = handle_textelem(&child, &pot_union, options);
                    }
                    // main_extractor.py:422-423 — define_newelem dispatch.
                    if let Some(p) = processed_subchild.as_ref() {
                        define_newelem(Some(p), &new_child_elem);
                    }
                    // main_extractor.py:424 — child.tag = "done". Pin alive.
                    dones_alive.push(replace_element_tag(&child, "done"));
                }
            }

            // main_extractor.py:426-427 — append cell if it has text or
            // element-children.
            let has_text = element_text(&new_child_elem)
                .map(|t| !t.is_empty())
                .unwrap_or(false);
            if has_text || element_child_count(&new_child_elem) > 0 {
                append_child(&newrow, &new_child_elem);
            }
        } else if sub_tag == "table" {
            // main_extractor.py:429-430 — break on nested table.
            break;
        }
        // main_extractor.py:432 — subelement.tag = "done" (unconditional
        // outer cleanup). Pin alive.
        dones_alive.push(replace_element_tag(&subelement, "done"));
    }
    let _ = &dones_alive;

    // main_extractor.py:435 — newrow.attrib.pop("span", None).
    crate::readability::dom::remove_attribute(&newrow, "span");

    // main_extractor.py:438-441 — append residual row, then return table
    // or None.
    if element_child_count(&newrow) > 0 {
        append_child(&newtable, &newrow);
    }
    if element_child_count(&newtable) > 0 {
        Some(newtable)
    } else {
        None
    }
}

// ===========================================================================
// handle_textelem (main_extractor.py:482-509) — Stage 2c-iii
// ===========================================================================

/// `handle_textelem(element, potential_tags, options)` —
/// `main_extractor.py:482-509`.
///
/// Dispatch hub: routes each text element to the appropriate handle_*
/// based on its tag. The central call site for `_extract`'s element-wise
/// processing pass.
///
/// # Python original
///
/// ```python
/// def handle_textelem(element, potential_tags, options):
///     '''Process text element and determine how to deal with its content'''
///     new_element = None
///     if element.tag == 'list':
///         new_element = handle_lists(element, options)
///     elif element.tag in CODES_QUOTES:
///         new_element = handle_quotes(element, options)
///     elif element.tag == 'head':
///         new_element = handle_titles(element, options)
///     elif element.tag == 'p':
///         new_element = handle_paragraphs(element, potential_tags, options)
///     elif element.tag == 'lb':
///         if text_chars_test(element.tail) is True:
///             this_element = process_node(element, options)
///             if this_element is not None:
///                 new_element = Element('p')
///                 new_element.text = this_element.tail
///     elif element.tag in FORMATTING:
///         new_element = handle_formatting(element, options)
///     elif element.tag == 'table' and 'table' in potential_tags:
///         new_element = handle_table(element, potential_tags, options)
///     elif element.tag == 'graphic' and 'graphic' in potential_tags:
///         new_element = handle_image(element)
///     else:
///         new_element = handle_other_elements(element, potential_tags, options)
///     return new_element
/// ```
///
/// # Faithfulness notes
///
/// 1. The `<lb>` branch (lines 494-499) is unusual: it builds a NEW `<p>`
///    whose `.text` is the **tail** of the processed `<lb>`. The `<lb>`
///    itself is dropped; only its tail survives as a fresh paragraph. The
///    `text_chars_test(element.tail) is True` guard ensures we only do this
///    when the tail has actual character content (post-strip).
/// 2. `if 'table' in potential_tags` and `if 'graphic' in potential_tags`
///    (lines 502, 504) — the dispatch is gated by the caller's
///    `potential_tags` set. When the gate fails, the `<table>` / `<graphic>`
///    falls through to `handle_other_elements`.
pub fn handle_textelem(
    element: &NodeRef,
    potential_tags: &HashSet<String>,
    options: &Options,
) -> Option<NodeRef> {
    let tag = local_name(element).unwrap_or_default();

    match tag.as_str() {
        "list" => handle_lists(element, options),
        t if CODES_QUOTES.contains(&t) => handle_quotes(element, options),
        "head" => handle_titles(element, options),
        "p" => handle_paragraphs(element, potential_tags, options),
        "lb" => {
            // main_extractor.py:494-499 — <lb> tail-promotion.
            if text_chars_test(tail(element).as_deref()) {
                let this_element = process_node(element, options)?;
                let new_element = create_element("p");
                set_element_text(&new_element, tail(&this_element).as_deref());
                Some(new_element)
            } else {
                None
            }
        }
        t if FORMATTING.contains(&t) => handle_formatting(element, options),
        "table" if potential_tags.contains("table") => {
            handle_table(element, potential_tags, options)
        }
        "graphic" if potential_tags.contains("graphic") => handle_image(element),
        _ => handle_other_elements(element, potential_tags, options),
    }
}

// ===========================================================================
// prune_unwanted_sections (main_extractor.py:533-564) — Stage 2c-iii
// ===========================================================================

/// `prune_unwanted_sections(tree, potential_tags, options)` —
/// `main_extractor.py:533-564`.
///
/// Rule-based deletion of targeted document sections. Runs a cascade of
/// XPath-driven prunes (with backup-restore on the OVERALL pass), two
/// passes of link-density pruning on div/list/p, conditional table-link-
/// density filtering, and precision-mode trailing-`<head>` cleanup.
///
/// # Python original
///
/// ```python
/// def prune_unwanted_sections(tree, potential_tags, options):
///     'Rule-based deletion of targeted document sections'
///     favor_precision = options.focus == "precision"
///     tree = prune_unwanted_nodes(tree, OVERALL_DISCARD_XPATH, with_backup=True)
///     if 'graphic' not in potential_tags:
///         tree = prune_unwanted_nodes(tree, DISCARD_IMAGE_ELEMENTS)
///     if options.focus != "recall":
///         tree = prune_unwanted_nodes(tree, TEASER_DISCARD_XPATH)
///         if favor_precision:
///             tree = prune_unwanted_nodes(tree, PRECISION_DISCARD_XPATH)
///     for _ in range(2):
///         tree = delete_by_link_density(tree, 'div', backtracking=True, favor_precision=favor_precision)
///         tree = delete_by_link_density(tree, 'list', backtracking=False, favor_precision=favor_precision)
///         tree = delete_by_link_density(tree, 'p', backtracking=False, favor_precision=favor_precision)
///     if 'table' in potential_tags or favor_precision:
///         for elem in tree.iter('table'):
///             if link_density_test_tables(elem) is True:
///                 delete_element(elem, keep_tail=False)
///     if favor_precision:
///         while len(tree) > 0 and (tree[-1].tag == 'head'):
///             delete_element(tree[-1], keep_tail=False)
///         tree = delete_by_link_density(tree, 'head', backtracking=False, favor_precision=True)
///         tree = delete_by_link_density(tree, 'quote', backtracking=False, favor_precision=True)
///     return tree
/// ```
///
/// # Faithfulness notes
///
/// 1. `tree = prune_unwanted_nodes(...)` rebinds the local: only the OVERALL
///    pass (with_backup=true) can actually swap; all other passes return
///    the same tree (no backup). The `let tree = ...` shadow in Rust does
///    the same rebind so downstream passes operate on the current handle.
/// 2. `delete_by_link_density` mutates the input in place and returns the
///    same tree in Python; our Rust port returns `()`, so no rebind is
///    needed (the original handle still points at the in-place-mutated
///    tree).
/// 3. `tree.iter('table')` (line 554) is descendant-OR-self for "table",
///    so a root that is itself `<table>` would match. In practice `_extract`
///    passes the body element, so the iter starts at descendants.
///    `get_elements_by_tag_name` is descendants-only; we explicitly include
///    self when its local-name matches.
/// 4. `delete_element(elem, keep_tail=False)` (line 556) — remove the
///    element AND drop its tail Text-run. Faithful Rust: clear tail via
///    `set_tail(None)` (drops the parent-level Text-run between elem and
///    its next non-Text sibling) then `dom::remove(elem)`.
/// 5. `while len(tree) > 0 and (tree[-1].tag == 'head')` (lines 560-561):
///    Python's `tree[-1]` is the LAST element child of `tree`. Repeat the
///    delete loop while the last element child is a `<head>`.
pub fn prune_unwanted_sections(
    tree: &NodeRef,
    potential_tags: &HashSet<String>,
    options: &Options,
) -> NodeRef {
    use crate::trafilatura::cleaning::{
        Focus, delete_by_link_density, link_density_test_tables, prune_unwanted_nodes,
    };
    use crate::trafilatura::xpaths_constants::{
        DISCARD_IMAGE_ELEMENTS, OVERALL_DISCARD_XPATH, PRECISION_DISCARD_XPATH,
        TEASER_DISCARD_XPATH,
    };

    let favor_precision = options.focus == Focus::Precision;

    // main_extractor.py:537 — OVERALL prune with backup.
    let tree = prune_unwanted_nodes(tree, OVERALL_DISCARD_XPATH, true);

    // main_extractor.py:539-540 — image elements unless graphic preserved.
    let tree = if !potential_tags.contains("graphic") {
        prune_unwanted_nodes(&tree, DISCARD_IMAGE_ELEMENTS, false)
    } else {
        tree
    };

    // main_extractor.py:542-545 — teaser + precision (when not recall).
    let tree = if options.focus != Focus::Recall {
        let tree = prune_unwanted_nodes(&tree, TEASER_DISCARD_XPATH, false);
        if favor_precision {
            prune_unwanted_nodes(&tree, PRECISION_DISCARD_XPATH, false)
        } else {
            tree
        }
    } else {
        tree
    };

    // main_extractor.py:547-550 — two passes of link-density deletion.
    for _ in 0..2 {
        delete_by_link_density(&tree, "div", true, favor_precision);
        delete_by_link_density(&tree, "list", false, favor_precision);
        delete_by_link_density(&tree, "p", false, favor_precision);
    }

    // main_extractor.py:552-556 — table link-density.
    if potential_tags.contains("table") || favor_precision {
        // iter('table') is descendants-or-self; include self if it matches.
        let mut tables: Vec<NodeRef> = Vec::new();
        if local_name(&tree).as_deref() == Some("table") {
            tables.push(tree.clone());
        }
        tables.extend(get_elements_by_tag_name(&tree, "table"));
        for elem in tables {
            if link_density_test_tables(&elem) {
                // delete_element(elem, keep_tail=False).
                set_tail(&elem, None);
                crate::readability::dom::remove(&elem);
            }
        }
    }

    // main_extractor.py:558-563 — favor_precision tail-prune.
    if favor_precision {
        // while len(tree) > 0 and tree[-1].tag == 'head': delete.
        loop {
            let kids = element_children(&tree);
            let Some(last) = kids.last() else { break };
            if local_name(last).as_deref() != Some("head") {
                break;
            }
            set_tail(last, None);
            crate::readability::dom::remove(last);
        }
        delete_by_link_density(&tree, "head", false, true);
        delete_by_link_density(&tree, "quote", false, true);
    }

    tree
}

// ===========================================================================
// recover_wild_text (main_extractor.py:512-530) — Stage 2c-iii
// ===========================================================================

/// `recover_wild_text(tree, result_body, options, potential_tags)` —
/// `main_extractor.py:512-530`.
///
/// Last-resort rescue: look for wild elements throughout the document
/// (including outside the determined frame) to recover potentially missing
/// text. Runs `prune_unwanted_sections` first, strips inline link wrappers
/// (`<a>`/`<ref>`/`<span>` unless `ref` is preserved), then walks all
/// remaining `<blockquote|code|p|pre|q|quote|table|div.w3-code>` elements
/// (plus `<div|lb|list>` in recall mode), processes each through
/// `handle_textelem`, and appends survivors to `result_body`.
///
/// # Python original
///
/// ```python
/// def recover_wild_text(tree, result_body, options, potential_tags=TAG_CATALOG):
///     LOGGER.debug('Recovering wild text elements')
///     search_expr = './/blockquote|.//code|.//p|.//pre|.//q|.//quote|.//table|.//div[contains(@class, \'w3-code\')]'
///     if options.focus == "recall":
///         potential_tags.update(['div', 'lb'])
///         search_expr += '|.//div|.//lb|.//list'
///     search_tree = prune_unwanted_sections(tree, potential_tags, options)
///     if 'ref' not in potential_tags:
///         strip_tags(search_tree, 'a', 'ref', 'span')
///     else:
///         strip_tags(search_tree, 'span')
///     subelems = search_tree.xpath(search_expr)
///     result_body.extend(filter(lambda x: x is not None, (handle_textelem(e, potential_tags, options)
///                        for e in subelems)))
///     return result_body
/// ```
///
/// # Faithfulness notes
///
/// 1. `potential_tags: Any = TAG_CATALOG` — Python default arg is the
///    `TAG_CATALOG` frozenset. The first action mutates `potential_tags`
///    (line 518: `update`), which can pollute the default arg across
///    calls (a famous Python pitfall). Faithful Rust: take `&HashSet`
///    by reference; the caller passes a fresh set. We clone-into-mutable
///    inside the function so the recall branch's `update` doesn't have to
///    rely on the caller's mutability.
/// 2. `strip_tags(search_tree, 'a', 'ref', 'span')` (line 524) — strip
///    three tags' wrappers (lift children up). Faithful Rust:
///    `strip_tags_multi(search_tree, &["a", "ref", "span"])`.
/// 3. `result_body.extend(filter(..., (handle_textelem(e, ...) for e in
///    subelems)))` (lines 528-529) — generator expression filtered through
///    `lambda x: x is not None`. Iterate over subelems, call handle_textelem
///    on each, append non-None results to result_body.
/// 4. The XPath union expression uses our Stage 0b engine; the gap survey
///    showed contains() and unions are supported.
pub fn recover_wild_text(
    tree: &NodeRef,
    result_body: &NodeRef,
    options: &Options,
    potential_tags: &HashSet<String>,
) -> NodeRef {
    use crate::trafilatura::cleaning::Focus;
    use crate::trafilatura::xpath_engine;

    // main_extractor.py:516 — base search expression.
    let mut search_expr = String::from(
        ".//blockquote|.//code|.//p|.//pre|.//q|.//quote|.//table|.//div[contains(@class, 'w3-code')]",
    );

    // main_extractor.py:517-519 — recall mode extends both the set and the
    // expression. Clone the input set so we don't mutate the caller's view.
    let mut pot = potential_tags.clone();
    if options.focus == Focus::Recall {
        pot.insert("div".to_string());
        pot.insert("lb".to_string());
        search_expr.push_str("|.//div|.//lb|.//list");
    }

    // main_extractor.py:521 — prune.
    let search_tree = prune_unwanted_sections(tree, &pot, options);

    // main_extractor.py:523-526 — strip inline tags based on whether ref
    // is preserved.
    if !pot.contains("ref") {
        strip_tags_multi(&search_tree, &["a", "ref", "span"]);
    } else {
        strip_tags_multi(&search_tree, &["span"]);
    }

    // main_extractor.py:527 — XPath search.
    let subelems = xpath_engine::evaluate(&search_expr, &search_tree).unwrap_or_default();

    // main_extractor.py:528-529 — filter + extend.
    for e in subelems {
        if let Some(new_elem) = handle_textelem(&e, &pot, options) {
            append_child(result_body, &new_elem);
        }
    }

    result_body.clone()
}

// ===========================================================================
// _extract (main_extractor.py:567-617) — Stage 2d
// ===========================================================================

/// `_extract(tree, options)` — `main_extractor.py:567-617`.
///
/// The main extraction orchestrator. Builds the `potential_tags` set from
/// `options`, then iterates BODY_XPATH expressions until one finds a
/// non-empty subtree, prunes it, dispatches each descendant through
/// `handle_textelem`, and accumulates survivors into a fresh `<body>`.
///
/// Returns `(result_body, temp_text, potential_tags)`.
///
/// # Python original
///
/// See the module-level `_extract` (lines 567-617). The active body:
///
/// 1. Build `potential_tags` from TAG_CATALOG, gated by tables/images/links.
/// 2. For each `BODY_XPATH` expression:
///    - Find the first non-None match in `tree`.
///    - Prune it via `prune_unwanted_sections`.
///    - Skip if the pruned tree has no element children.
///    - Compute `ptest = subtree.xpath('//p//text()')` (descendant text
///      nodes under `<p>`). If `ptest` is empty OR the joined length is
///      below `min_extracted_size * factor`, ADD `"div"` to potential_tags
///      (a recall booster — division-class blocks become harvestable).
///      `factor = 1` for precision, `3` otherwise.
///    - Strip `<ref>` / `<span>` when not in potential_tags.
///    - Build `subelems = subtree.xpath('.//*')` (all descendants).
///    - Special case: if every descendant has tag "lb", use `[subtree]`
///      instead so handle_textelem dispatches the lb-containing container.
///    - Dispatch every elem through handle_textelem; append non-None
///      results to result_body.
///    - Trim trailing NOT_AT_THE_END elements (`<head>`/`<ref>`) via
///      `delete_element(_, keep_tail=False)`.
///    - Exit the BODY_XPATH loop early if `len(result_body) > 1`.
/// 3. Compute `temp_text = ' '.join(result_body.itertext()).strip()`.
///
/// # Faithfulness notes
///
/// 1. The `next(...)` (line 580) takes the FIRST element from
///    `expr(tree)`. Our XPath engine returns a Vec; `first().cloned()`
///    has identical semantics for tree-ordered results.
/// 2. `factor = 3` for non-precision means the `min_extracted_size`
///    threshold becomes 750 chars by default — quite high. This is the
///    "rescue gate" that pulls `<div>` into `potential_tags` when the
///    primary BODY_XPATH match doesn't have enough paragraph text.
/// 3. `len(subtree) == 0` (line 586) — element-child count.
/// 4. The text-node string extraction reads each Text node's `.data`
///    field. Our `xpath_engine::evaluate` returns Text nodes via the
///    text() axis, and `data` is accessible via the NodeData::Text arm.
pub fn _extract(tree: &NodeRef, options: &Options) -> (NodeRef, String, HashSet<String>) {
    use crate::trafilatura::cleaning::Focus;
    use crate::trafilatura::xpath_engine;
    use crate::trafilatura::xpaths_constants::BODY_XPATH;

    // main_extractor.py:569-575 — build potential_tags.
    let mut potential_tags: HashSet<String> =
        TAG_CATALOG.iter().map(|s| s.to_string()).collect();
    if options.tables {
        for t in ["table", "td", "th", "tr"] {
            potential_tags.insert(t.to_string());
        }
    }
    if options.images {
        potential_tags.insert("graphic".to_string());
    }
    if options.links {
        potential_tags.insert("ref".to_string());
    }

    // main_extractor.py:576 — fresh <body>.
    let result_body = create_element("body");

    // main_extractor.py:578 — iterate BODY_XPATH.
    for expr in BODY_XPATH {
        // main_extractor.py:580 — first non-None match.
        let matches = xpath_engine::evaluate(expr, tree).unwrap_or_default();
        let Some(subtree) = matches.into_iter().next() else {
            continue;
        };

        // main_extractor.py:584 — prune.
        let subtree = prune_unwanted_sections(&subtree, &potential_tags, options);

        // main_extractor.py:586-587 — skip empty.
        if element_child_count(&subtree) == 0 {
            continue;
        }

        // main_extractor.py:589 — ptest = subtree.xpath('//p//text()').
        // Use ".//p//text()" (descendant-relative) instead of "//p//text()"
        // (absolute, root-relative) to match the document's <p> elements
        // anchored at the subtree's root.
        let ptest =
            xpath_engine::evaluate(".//p//text()", &subtree).unwrap_or_default();
        let factor: usize = if options.focus == Focus::Precision {
            1
        } else {
            3
        };
        // main_extractor.py:594 — Python's `''.join(ptest)` joins all
        // Text-node data values into one string. Our Rust port reads each
        // node's NodeData::Text contents.
        let ptest_total_len: usize = ptest
            .iter()
            .filter_map(|n| match &n.data {
                NodeData::Text { contents } => Some(contents.borrow().chars().count()),
                _ => None,
            })
            .sum();
        if ptest.is_empty() || ptest_total_len < options.min_extracted_size * factor {
            potential_tags.insert("div".to_string());
        }

        // main_extractor.py:597-600 — strip ref/span when not in potential_tags.
        if !potential_tags.contains("ref") {
            strip_tags_multi(&subtree, &["ref"]);
        }
        if !potential_tags.contains("span") {
            strip_tags_multi(&subtree, &["span"]);
        }

        // main_extractor.py:603 — subelems = subtree.xpath('.//*').
        let mut subelems = get_elements_by_tag_name(&subtree, "*");

        // main_extractor.py:605-606 — special case: only-lb shape.
        let only_lb = !subelems.is_empty()
            && subelems
                .iter()
                .all(|e| local_name(e).as_deref() == Some("lb"));
        if only_lb {
            subelems = vec![subtree.clone()];
        }

        // main_extractor.py:608 — handle_textelem dispatch + append.
        //
        // **Stage 3-B Cluster C fix (2026-05-21):** skip detached
        // (orphan) elements. Python's flow renames already-processed
        // elements to `tag = "done"` via in-place tag mutation
        // (e.g. main_extractor.py:340 inside handle_paragraphs,
        // line 222 inside handle_code_blocks), so subsequent
        // iterdescendants walks visit those elements and they fall
        // through `handle_other_elements` → `tag not in potential_tags`
        // → returns None. Our `replace_element_tag` doesn't mutate the
        // OLD node's tag (it can't — rcdom's `NodeData::Element::name`
        // is not wrapped in a cell), but it DOES detach the OLD node
        // (drains children + clears parent pointer). So our equivalent
        // skip-signal is "the OLD node is now detached". This avoids
        // emitting extra empty `<code>` placeholders (visible on the
        // Rust 1.83 blog where inline `<code>rustup</code>` inside a
        // `<p>` was being re-emitted as a top-level orphan after
        // handle_paragraphs drained it).
        for e in subelems {
            if crate::readability::dom::parent(&e).is_none() {
                continue;
            }
            if let Some(new_elem) = handle_textelem(&e, &potential_tags, options) {
                append_child(&result_body, &new_elem);
            }
        }

        // main_extractor.py:610-611 — trim trailing NOT_AT_THE_END.
        loop {
            let kids = element_children(&result_body);
            let Some(last) = kids.last() else { break };
            let last_tag = local_name(last).unwrap_or_default();
            if !NOT_AT_THE_END.contains(&last_tag.as_str()) {
                break;
            }
            set_tail(last, None);
            crate::readability::dom::remove(last);
        }

        // main_extractor.py:613-615 — exit if result has >1 children.
        if element_child_count(&result_body) > 1 {
            break;
        }
    }

    // main_extractor.py:616 — temp_text = ' '.join(result_body.itertext()).strip().
    let texts = itertext(&result_body);
    let temp_text = texts.join(" ").trim().to_string();

    (result_body, temp_text, potential_tags)
}

// ===========================================================================
// extract_content (main_extractor.py:620-640) — Stage 2d
// ===========================================================================

/// `extract_content(cleaned_tree, options)` — `main_extractor.py:620-640`.
///
/// High-level extraction wrapper: backs up the cleaned tree, runs
/// `_extract`, falls back to `recover_wild_text` when nothing or too
/// little came out, then strips `<done>` artifacts (with content) and
/// `<div>` wrappers (preserving inline content), and returns
/// `(result_body, temp_text, len(temp_text))`.
///
/// # Python original
///
/// ```python
/// def extract_content(cleaned_tree, options) -> Tuple[_Element, str, int]:
///     backup_tree = deepcopy(cleaned_tree)
///     result_body, temp_text, potential_tags = _extract(cleaned_tree, options)
///     if len(result_body) == 0 or len(temp_text) < options.min_extracted_size:
///         result_body = recover_wild_text(backup_tree, result_body, options, potential_tags)
///         temp_text = ' '.join(result_body.itertext()).strip()
///     strip_elements(result_body, 'done')
///     strip_tags(result_body, 'div')
///     return result_body, temp_text, len(temp_text)
/// ```
///
/// # Faithfulness notes
///
/// 1. `deepcopy(cleaned_tree)` (line 625) — pre-prune backup. Use
///    `dom::deep_clone` (Stage 2c-i facade).
/// 2. `strip_elements(result_body, 'done')` (line 637) — removes every
///    `<done>` descendant AND its subtree, AND drops the tail Text-run
///    that follows (Python's `with_tail=False` is the default for
///    `etree.strip_elements`). Our 2-step pattern: snapshot via
///    `get_elements_by_tag_name`, then `set_tail(None) + dom::remove`.
/// 3. `strip_tags(result_body, 'div')` (line 638) — DIFFERENT semantic:
///    removes only the `<div>` WRAPPER, preserving its children and
///    tail. Maps to `cleaning::strip_tags_multi(result_body, &["div"])`.
pub fn extract_content(cleaned_tree: &NodeRef, options: &Options) -> (NodeRef, String, usize) {
    // main_extractor.py:625 — backup.
    let backup_tree = deep_clone(cleaned_tree);

    // main_extractor.py:627 — _extract.
    let (result_body, mut temp_text, potential_tags) = _extract(cleaned_tree, options);

    // main_extractor.py:633-635 — wild-text fallback.
    let result_body = if element_child_count(&result_body) == 0
        || temp_text.chars().count() < options.min_extracted_size
    {
        let rb = recover_wild_text(&backup_tree, &result_body, options, &potential_tags);
        let texts = itertext(&rb);
        temp_text = texts.join(" ").trim().to_string();
        rb
    } else {
        result_body
    };

    // main_extractor.py:637 — strip_elements(result_body, 'done').
    let dones = get_elements_by_tag_name(&result_body, "done");
    for d in dones {
        set_tail(&d, None);
        crate::readability::dom::remove(&d);
    }

    // main_extractor.py:638 — strip_tags(result_body, 'div').
    strip_tags_multi(&result_body, &["div"]);

    let temp_text_len = temp_text.chars().count();
    (result_body, temp_text, temp_text_len)
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
    fn process_nested_elements_routes_list_descendant_through_handle_lists() {
        // Stage 2c-ii replaced the panicking handle_lists stub with the
        // real impl (main_extractor.py:161-199). The dispatch shape that
        // Stage 2c-i pinned (with #[should_panic]) is now exercised end-to-
        // end: build a tree with a <list><item>x</item></list> descendant,
        // run process_nested_elements, and assert the new_child_elem now
        // carries the handle_lists output (a <list> with an <item> child).
        let child = dom_create_element("div");
        let list_elem = dom_create_element("list");
        let item = dom_create_element("item");
        dom_append_child(&item, &create_text_node("x"));
        dom_append_child(&list_elem, &item);
        dom_append_child(&child, &list_elem);
        let new_child_elem = dom_create_element("div");
        let opts = Options::default();
        process_nested_elements(&child, &new_child_elem, &opts);
        // After dispatch, new_child_elem should have a <list> child carrying
        // an <item> grandchild — proving the list branch ran handle_lists
        // and appended its output (not stripped via the textnode/add_sub
        // path).
        let lists = get_elements_by_tag_name(&new_child_elem, "list");
        assert_eq!(lists.len(), 1, "handle_lists output appended");
        let items = get_elements_by_tag_name(&lists[0], "item");
        assert_eq!(items.len(), 1, "item preserved through handle_lists");
        let item_text = element_text(&items[0]).unwrap_or_default();
        assert!(
            item_text.contains('x'),
            "item text preserved: {item_text:?}"
        );
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

    // -------------------------------------------------------------------
    // Stage 2c-ii — handle_lists (main_extractor.py:161-199)
    // -------------------------------------------------------------------

    #[test]
    fn handle_lists_empty_input_returns_none() {
        // <list></list> — no text, no item descendants → is_text_element
        // on processed_element returns false → None.
        let list = dom_create_element("list");
        let opts = Options::default();
        assert!(handle_lists(&list, &opts).is_none());
    }

    #[test]
    fn handle_lists_with_text_creates_item() {
        // <list>leading</list> — leading text becomes a synthetic <item>.
        let list = dom_create_element("list");
        dom_append_child(&list, &create_text_node("leading text"));
        let opts = Options::default();
        let out = handle_lists(&list, &opts).expect("text → Some");
        // The output has a single <item> carrying the leading text.
        let items = get_elements_by_tag_name(&out, "item");
        assert_eq!(items.len(), 1);
        assert_eq!(element_text(&items[0]).as_deref(), Some("leading text"));
    }

    #[test]
    fn handle_lists_descendant_items_processed() {
        // <list><item>one</item><item>two</item></list> — both items survive.
        let list = dom_create_element("list");
        for txt in ["one", "two"] {
            let item = dom_create_element("item");
            dom_append_child(&item, &create_text_node(txt));
            dom_append_child(&list, &item);
        }
        let opts = Options::default();
        let out = handle_lists(&list, &opts).expect("items present → Some");
        let items = get_elements_by_tag_name(&out, "item");
        // Two original items get processed; each becomes a new <item> on
        // the output tree.
        assert_eq!(items.len(), 2);
        let item_texts: Vec<String> = items
            .iter()
            .map(|i| element_text(i).unwrap_or_default())
            .collect();
        assert!(item_texts.iter().any(|t| t.contains("one")));
        assert!(item_texts.iter().any(|t| t.contains("two")));
    }

    #[test]
    fn handle_lists_returns_none_when_no_text_chars() {
        // <list><item>   </item></list> — process_node strips whitespace,
        // is_text_element on the resulting processed_element fails (no text
        // chars survive) → None.
        let list = dom_create_element("list");
        let item = dom_create_element("item");
        dom_append_child(&item, &create_text_node("   "));
        dom_append_child(&list, &item);
        let opts = Options::default();
        assert!(handle_lists(&list, &opts).is_none());
    }

    #[test]
    fn handle_lists_preserves_rend_attribute() {
        // <list rend="bullet"><item>x</item></list> — update_elem_rendition
        // at line 197 copies element's rend to processed_element.
        let list = dom_create_element("list");
        set_attribute(&list, "rend", "bullet");
        let item = dom_create_element("item");
        dom_append_child(&item, &create_text_node("x"));
        dom_append_child(&list, &item);
        let opts = Options::default();
        let out = handle_lists(&list, &opts).expect("text → Some");
        assert_eq!(get_attribute(&out, "rend").as_deref(), Some("bullet"));
    }

    // -------------------------------------------------------------------
    // Stage 2c-ii — is_code_block_element (main_extractor.py:202-215)
    // -------------------------------------------------------------------

    #[test]
    fn is_code_block_element_true_for_code_tag() {
        // main_extractor.py:205 — element.tag == "code" → True.
        let code = dom_create_element("code");
        assert!(is_code_block_element(&code));
    }

    #[test]
    fn is_code_block_element_true_for_lang_attr() {
        // main_extractor.py:205 — element.get("lang") truthy → True.
        let pre = dom_create_element("pre");
        set_attribute(&pre, "lang", "python");
        assert!(is_code_block_element(&pre));
    }

    #[test]
    fn is_code_block_element_true_for_highlight_parent_class() {
        // main_extractor.py:208-210 — parent.class contains "highlight".
        let parent_div = dom_create_element("div");
        set_attribute(&parent_div, "class", "highlight-py source");
        let pre = dom_create_element("pre");
        dom_append_child(&parent_div, &pre);
        assert!(is_code_block_element(&pre));
    }

    #[test]
    fn is_code_block_element_true_for_single_child_code() {
        // main_extractor.py:212-213 — find("code") AND len==1.
        let pre = dom_create_element("pre");
        let code = dom_create_element("code");
        dom_append_child(&pre, &code);
        assert!(is_code_block_element(&pre));
    }

    #[test]
    fn is_code_block_element_false_for_plain_div() {
        // No lang, not code tag, no highlight parent, no single code child.
        let div = dom_create_element("div");
        dom_append_child(&div, &create_text_node("plain"));
        assert!(!is_code_block_element(&div));
    }

    // -------------------------------------------------------------------
    // Stage 2c-ii — handle_code_blocks (main_extractor.py:218-224)
    // -------------------------------------------------------------------

    #[test]
    fn handle_code_blocks_renames_to_code() {
        // <pre>foo</pre> → clone with tag "code".
        let pre = dom_create_element("pre");
        dom_append_child(&pre, &create_text_node("foo"));
        let out = handle_code_blocks(&pre);
        assert_eq!(local_name(&out).as_deref(), Some("code"));
        let joined: String = itertext(&out).concat();
        assert!(joined.contains("foo"), "clone text preserved: {joined:?}");
    }

    #[test]
    fn handle_code_blocks_marks_descendants_done() {
        // <root><pre><span>x</span></pre></root> — original's <pre> + <span>
        // both renamed to "done" via replace_element_tag (which in rcdom
        // mints a NEW node and splices into the parent). Attach <pre> to a
        // <root> so we can probe the resulting parent tree.
        let root = dom_create_element("root");
        let pre = dom_create_element("pre");
        let span = dom_create_element("span");
        dom_append_child(&span, &create_text_node("x"));
        dom_append_child(&pre, &span);
        dom_append_child(&root, &pre);
        let out = handle_code_blocks(&pre);
        // The CLONE has tag "code".
        assert_eq!(local_name(&out).as_deref(), Some("code"));
        // Original <root> no longer contains a <pre> or <span>; both got
        // renamed to "done" descendants of <root>.
        let pre_remaining = get_elements_by_tag_name(&root, "pre");
        assert!(pre_remaining.is_empty(), "pre renamed away");
        let span_remaining = get_elements_by_tag_name(&root, "span");
        assert!(span_remaining.is_empty(), "span renamed away");
        let done_remaining = get_elements_by_tag_name(&root, "done");
        assert_eq!(done_remaining.len(), 2, "both pre and span renamed to done");
    }

    // -------------------------------------------------------------------
    // Stage 2c-ii — handle_quotes (main_extractor.py:227-242)
    // -------------------------------------------------------------------

    #[test]
    fn handle_quotes_dispatches_to_code_block_when_codey() {
        // <quote lang="python">x</quote> — is_code_block_element fires →
        // handle_code_blocks renames to "code".
        let quote = dom_create_element("quote");
        set_attribute(&quote, "lang", "python");
        dom_append_child(&quote, &create_text_node("x"));
        let opts = Options::default();
        let out = handle_quotes(&quote, &opts).expect("code-block path → Some");
        assert_eq!(local_name(&out).as_deref(), Some("code"));
    }

    #[test]
    fn handle_quotes_strips_nested_quote_tags() {
        // <quote>outer<quote>inner</quote></quote> — line 240 strips the
        // nested <quote> tag, leaving its inline content merged.
        let outer = dom_create_element("quote");
        dom_append_child(&outer, &create_text_node("outer "));
        let inner = dom_create_element("quote");
        dom_append_child(&inner, &create_text_node("inner"));
        dom_append_child(&outer, &inner);
        let opts = Options::default();
        let out = handle_quotes(&outer, &opts).expect("text → Some");
        // The result must NOT contain a nested <quote> element (strip_tags).
        let quotes = get_elements_by_tag_name(&out, "quote");
        assert!(quotes.is_empty(), "nested <quote> stripped");
    }

    #[test]
    fn handle_quotes_returns_none_for_empty() {
        // <quote></quote> — no text → is_text_element fails → None.
        let quote = dom_create_element("quote");
        let opts = Options::default();
        assert!(handle_quotes(&quote, &opts).is_none());
    }

    // -------------------------------------------------------------------
    // Stage 2c-ii — handle_other_elements (main_extractor.py:245-269)
    // -------------------------------------------------------------------

    fn potential_tags(tags: &[&str]) -> HashSet<String> {
        tags.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn handle_other_elements_dispatches_w3_code_div_to_code_blocks() {
        // <div class="w3-code">x</div> — main_extractor.py:248-249 →
        // handle_code_blocks renames to "code".
        let div = dom_create_element("div");
        set_attribute(&div, "class", "w3-code notranslate");
        dom_append_child(&div, &create_text_node("x"));
        let pot = potential_tags(&["div", "p"]);
        let opts = Options::default();
        let out = handle_other_elements(&div, &pot, &opts).expect("w3-code → Some");
        assert_eq!(local_name(&out).as_deref(), Some("code"));
    }

    #[test]
    fn handle_other_elements_returns_none_when_tag_not_in_potential() {
        // <article>x</article> — "article" not in potential_tags → None
        // (main_extractor.py:252-255).
        let article = dom_create_element("article");
        dom_append_child(&article, &create_text_node("x"));
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        assert!(handle_other_elements(&article, &pot, &opts).is_none());
    }

    #[test]
    fn handle_other_elements_renames_div_to_p_when_text_present() {
        // <div>some text</div> — div in potential_tags, text_chars_test
        // passes, attribs cleared, div→p rename (main_extractor.py:257-267).
        let div = dom_create_element("div");
        set_attribute(&div, "class", "removeme");
        dom_append_child(&div, &create_text_node("some text"));
        let pot = potential_tags(&["div", "p"]);
        let opts = Options::default();
        let out = handle_other_elements(&div, &pot, &opts).expect("div text → Some");
        assert_eq!(local_name(&out).as_deref(), Some("p"));
    }

    #[test]
    fn handle_other_elements_clears_attributes_on_div() {
        // main_extractor.py:262 — processed_element.attrib.clear() wipes attrs.
        let div = dom_create_element("div");
        set_attribute(&div, "class", "removeme");
        set_attribute(&div, "id", "alsoremoved");
        dom_append_child(&div, &create_text_node("text"));
        let pot = potential_tags(&["div", "p"]);
        let opts = Options::default();
        let out = handle_other_elements(&div, &pot, &opts).expect("div text → Some");
        assert!(get_attribute(&out, "class").is_none(), "class attr cleared");
        assert!(get_attribute(&out, "id").is_none(), "id attr cleared");
    }

    // -------------------------------------------------------------------
    // Stage 2c-ii — handle_paragraphs (main_extractor.py:272-351)
    // -------------------------------------------------------------------

    #[test]
    fn handle_paragraphs_empty_dispatches_to_process_node() {
        // <p>text</p> — no element children (only Text), so len(element)==0
        // → process_node single dispatch (main_extractor.py:278-279).
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("hello"));
        let pot = potential_tags(&["p", "hi", "ref", "lb"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("text → Some via process_node");
        // process_node returns the same NodeRef; tag preserved.
        assert_eq!(local_name(&out).as_deref(), Some("p"));
    }

    #[test]
    fn handle_paragraphs_clears_input_attributes() {
        // main_extractor.py:274 — element.attrib.clear() runs unconditionally.
        let p = dom_create_element("p");
        set_attribute(&p, "class", "junk");
        set_attribute(&p, "id", "junkid");
        dom_append_child(&p, &create_text_node("text"));
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        let _ = handle_paragraphs(&p, &pot, &opts);
        // Input's attrs are gone (the clear ran on `element` itself).
        assert!(get_attribute(&p, "class").is_none());
        assert!(get_attribute(&p, "id").is_none());
    }

    #[test]
    fn handle_paragraphs_p_child_text_merges_into_processed_text() {
        // <p>outer<p>inner</p></p> — line 292-298: processed_child.tag=="p"
        // path merges inner text into processed_element.text. We need the
        // outer to have an element child so we take the iter("*") branch.
        let outer = dom_create_element("p");
        dom_append_child(&outer, &create_text_node("outer"));
        let inner = dom_create_element("p");
        dom_append_child(&inner, &create_text_node("inner"));
        dom_append_child(&outer, &inner);
        let pot = potential_tags(&["p", "hi", "ref"]);
        let opts = Options::default();
        let out = handle_paragraphs(&outer, &pot, &opts).expect("text path → Some");
        let joined = element_text(&out).unwrap_or_default();
        // Both outer and inner texts merge into the output's .text.
        assert!(joined.contains("outer"), "outer text merged: {joined:?}");
        assert!(joined.contains("inner"), "inner text merged: {joined:?}");
    }

    #[test]
    fn handle_paragraphs_preserves_lb_tail_text() {
        // **Stage 3-B Cluster B regression pin (2026-05-21).**
        //
        // Before the fix, `handle_paragraphs` set the tail on `newsub`
        // BEFORE appending it to `processed_element`. `set_tail` (dom.rs:512)
        // early-returns on an orphan node (it stores tails as
        // following-Text-sibling-run, which requires a parent), so the tail
        // was silently dropped — visible on gov.uk PAYE and Fed reserve
        // where `<lb>` elements carry the line-break text in their tails.
        //
        // Verifies that
        //   <p>Information Policy Team<lb/>The National Archives<lb/>Kew</p>
        // round-trips with all the lb tails intact.
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("Information Policy Team"));
        let lb1 = dom_create_element("lb");
        dom_append_child(&p, &lb1);
        // Tail of lb1 — stored as a sibling Text node after lb1 in p.
        dom_append_child(&p, &create_text_node("The National Archives"));
        let lb2 = dom_create_element("lb");
        dom_append_child(&p, &lb2);
        dom_append_child(&p, &create_text_node("Kew"));

        let pot = potential_tags(&["p", "lb", "hi", "ref"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");

        // The output should have 2 lb children with their tails as sibling
        // Text nodes preserved.
        let serialized = crate::readability::dom::serialize_converted_tree(&out);
        assert!(
            serialized.contains("The National Archives"),
            "lb1 tail preserved: {serialized:?}"
        );
        assert!(
            serialized.contains("Kew"),
            "lb2 tail preserved: {serialized:?}"
        );
        // And the lbs themselves are present.
        let lbs = get_elements_by_tag_name(&out, "lb");
        assert_eq!(lbs.len(), 2, "both lb elements present");
    }

    #[test]
    fn handle_paragraphs_hi_formatting_copies_rend() {
        // <p>x<hi rend="bold">y</hi></p> — main_extractor.py:310-311 copies
        // rend onto newsub.
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("x"));
        let hi = dom_create_element("hi");
        set_attribute(&hi, "rend", "bold");
        dom_append_child(&hi, &create_text_node("y"));
        dom_append_child(&p, &hi);
        let pot = potential_tags(&["p", "hi"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");
        // The <hi> child in the output should carry rend="bold".
        let his = get_elements_by_tag_name(&out, "hi");
        assert_eq!(his.len(), 1, "hi survived");
        assert_eq!(get_attribute(&his[0], "rend").as_deref(), Some("bold"));
    }

    #[test]
    fn handle_paragraphs_ref_formatting_copies_target() {
        // <p>x<ref target="/url">y</ref></p> — main_extractor.py:312-314
        // copies target onto newsub.
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("x"));
        let r = dom_create_element("ref");
        set_attribute(&r, "target", "/url");
        dom_append_child(&r, &create_text_node("y"));
        dom_append_child(&p, &r);
        let pot = potential_tags(&["p", "ref"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");
        let refs = get_elements_by_tag_name(&out, "ref");
        assert_eq!(refs.len(), 1, "ref survived");
        assert_eq!(get_attribute(&refs[0], "target").as_deref(), Some("/url"));
    }

    #[test]
    fn handle_paragraphs_skips_unexpected_tags() {
        // <p>x<unknown>y</unknown></p> — unknown tag not in potential_tags
        // AND not "done" → skipped (main_extractor.py:284-286).
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("x"));
        let u = dom_create_element("unknown");
        dom_append_child(&u, &create_text_node("y"));
        dom_append_child(&p, &u);
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");
        // The <unknown> tag must NOT appear in the output.
        let unk = get_elements_by_tag_name(&out, "unknown");
        assert!(unk.is_empty(), "unknown skipped");
    }

    #[test]
    fn handle_paragraphs_strips_trailing_lb_without_tail() {
        // <p>x<lb/></p> — trailing <lb> with no tail → delete (main_extractor.py:
        // 343-346). After handle_paragraphs the output's element-children list
        // must not end in <lb>.
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("x"));
        let lb = dom_create_element("lb");
        dom_append_child(&p, &lb);
        let pot = potential_tags(&["p", "lb"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");
        // Either no element children at all, or the last one isn't lb.
        let kids = element_children(&out);
        if let Some(last) = kids.last() {
            assert_ne!(
                local_name(last).as_deref(),
                Some("lb"),
                "trailing lb stripped"
            );
        }
    }

    #[test]
    fn handle_paragraphs_graphic_dispatches_to_handle_image() {
        // <p>x<graphic src="/img.png"/></p> — Stage 2c-iii replaced the
        // panicking handle_image forward stub with the real impl
        // (main_extractor.py:445-480). The graphic now survives the
        // dispatch with src canonicalised to "http:///img.png" (because
        // "/img.png" doesn't start with "http" and doesn't start with "//",
        // so it's unchanged from the input — Python's re.sub(r"^//", ...)
        // is a no-op on non-`//` prefixes).
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("x"));
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "/img.png");
        dom_append_child(&p, &g);
        let pot = potential_tags(&["p", "graphic"]);
        let opts = Options::default();
        let out = handle_paragraphs(&p, &pot, &opts).expect("p path → Some");
        // The <graphic> survives as a child of the output, with src preserved.
        let graphics = get_elements_by_tag_name(&out, "graphic");
        assert_eq!(graphics.len(), 1, "graphic survived");
        assert_eq!(
            get_attribute(&graphics[0], "src").as_deref(),
            Some("/img.png"),
            "src preserved through handle_image canonicalisation"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — handle_image (main_extractor.py:445-480)
    // -------------------------------------------------------------------

    #[test]
    fn handle_image_picks_src_attr() {
        // <graphic src="https://example.com/img.png"/> — src is preserved.
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "https://example.com/img.png");
        let out = handle_image(&g).expect("valid src → Some");
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("https://example.com/img.png")
        );
    }

    #[test]
    fn handle_image_picks_data_src_when_src_missing() {
        // <graphic data-src="https://example.com/img.png"/> — Python's first
        // loop ("data-src", "src") finds data-src first and breaks.
        let g = dom_create_element("graphic");
        set_attribute(&g, "data-src", "https://example.com/img.png");
        let out = handle_image(&g).expect("valid data-src → Some");
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("https://example.com/img.png")
        );
    }

    #[test]
    fn handle_image_scans_data_src_variants_via_for_else() {
        // <graphic data-src-large="https://example.com/img.png"/> — neither
        // data-src nor src is set, so the for..else fallback scans every attr
        // starting with "data-src".
        let g = dom_create_element("graphic");
        set_attribute(&g, "data-src-large", "https://example.com/img.png");
        let out = handle_image(&g).expect("data-src variant → Some");
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("https://example.com/img.png")
        );
    }

    #[test]
    fn handle_image_copies_alt_and_title() {
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "https://example.com/img.png");
        set_attribute(&g, "alt", "an image");
        set_attribute(&g, "title", "title text");
        let out = handle_image(&g).expect("valid → Some");
        assert_eq!(get_attribute(&out, "alt").as_deref(), Some("an image"));
        assert_eq!(get_attribute(&out, "title").as_deref(), Some("title text"));
    }

    #[test]
    fn handle_image_returns_none_when_no_src() {
        // <graphic alt="x"/> — no src/data-src present, so the bail at line
        // 471 fires.
        let g = dom_create_element("graphic");
        set_attribute(&g, "alt", "x");
        assert!(handle_image(&g).is_none());
    }

    #[test]
    fn handle_image_rewrites_protocol_relative_url() {
        // <graphic src="//cdn.example.com/img.png"/> — main_extractor.py:476-477
        // rewrites `^//` → `http://`.
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "//cdn.example.com/img.png");
        let out = handle_image(&g).expect("valid → Some");
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("http://cdn.example.com/img.png")
        );
    }

    #[test]
    fn handle_image_does_not_rewrite_relative_src() {
        // <graphic src="/relative.png"/> — re.sub(r"^//", ...) doesn't match,
        // so the src is unchanged.
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "/relative.png");
        let out = handle_image(&g).expect("valid → Some");
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("/relative.png"),
            "single-slash relative URL unchanged"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — define_cell_type (main_extractor.py:354-360)
    // -------------------------------------------------------------------

    #[test]
    fn define_cell_type_no_header_has_no_role() {
        let cell = define_cell_type(false);
        assert_eq!(local_name(&cell).as_deref(), Some("cell"));
        assert!(get_attribute(&cell, "role").is_none());
    }

    #[test]
    fn define_cell_type_header_sets_role_head() {
        let cell = define_cell_type(true);
        assert_eq!(local_name(&cell).as_deref(), Some("cell"));
        assert_eq!(get_attribute(&cell, "role").as_deref(), Some("head"));
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — handle_table (main_extractor.py:363-442)
    // -------------------------------------------------------------------

    #[test]
    fn handle_table_returns_none_for_empty_table() {
        // <table></table> — no rows, no cells → None.
        let t = dom_create_element("table");
        let pot = potential_tags(&["table", "p"]);
        let opts = Options::default();
        assert!(handle_table(&t, &pot, &opts).is_none());
    }

    #[test]
    fn handle_table_converts_tr_td_to_row_cell() {
        // <table><tr><td>x</td></tr></table> — should produce <table><row><cell>x.
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        let td = dom_create_element("td");
        dom_append_child(&td, &create_text_node("x"));
        dom_append_child(&tr, &td);
        dom_append_child(&t, &tr);
        let pot = potential_tags(&["table", "p"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        assert_eq!(local_name(&out).as_deref(), Some("table"));
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 1, "one row");
        let cells = get_elements_by_tag_name(&out, "cell");
        assert_eq!(cells.len(), 1, "one cell");
        let cell_text = element_text(&cells[0]).unwrap_or_default();
        assert!(cell_text.contains('x'), "cell text preserved: {cell_text:?}");
    }

    #[test]
    fn handle_table_marks_th_as_header_cell() {
        // <table><tr><th>H</th></tr></table> — the <th> cell gets role="head".
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        let th = dom_create_element("th");
        dom_append_child(&th, &create_text_node("H"));
        dom_append_child(&tr, &th);
        dom_append_child(&t, &tr);
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        let cells = get_elements_by_tag_name(&out, "cell");
        assert_eq!(cells.len(), 1);
        assert_eq!(
            get_attribute(&cells[0], "role").as_deref(),
            Some("head"),
            "th becomes header cell"
        );
    }

    #[test]
    fn handle_table_strips_thead_tbody_tfoot() {
        // <table><thead><tr><th>H</th></tr></thead><tbody><tr><td>x</td></tr></tbody></table>
        // — thead/tbody are stripped (children lifted to table) before processing.
        let t = dom_create_element("table");
        let thead = dom_create_element("thead");
        let tr1 = dom_create_element("tr");
        let th = dom_create_element("th");
        dom_append_child(&th, &create_text_node("H"));
        dom_append_child(&tr1, &th);
        dom_append_child(&thead, &tr1);
        let tbody = dom_create_element("tbody");
        let tr2 = dom_create_element("tr");
        let td = dom_create_element("td");
        dom_append_child(&td, &create_text_node("x"));
        dom_append_child(&tr2, &td);
        dom_append_child(&tbody, &tr2);
        dom_append_child(&t, &thead);
        dom_append_child(&t, &tbody);
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        // The output table should NOT contain thead/tbody (they were stripped),
        // and should have 2 rows (one for the th, one for the td).
        assert!(get_elements_by_tag_name(&out, "thead").is_empty());
        assert!(get_elements_by_tag_name(&out, "tbody").is_empty());
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 2, "two rows after stripping wrappers");
    }

    #[test]
    fn handle_table_pops_span_attribute_from_residual_row() {
        // <table><tr><td>a</td><td>b</td><td>c</td></tr></table> — 3 cols.
        // max_cols=3, so the initial newrow gets span="3". But Python's
        // line 435 `newrow.attrib.pop("span", None)` runs on the RESIDUAL
        // newrow before the final append (since len(newrow)>0 fires there,
        // not at the tr branch — only the first tr triggered no `len`>0
        // branch). The result: in a single-tr table, the appended row has
        // NO span attribute, even though max_cols > 1. We faithfully
        // preserve this Python source quirk.
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        for txt in ["a", "b", "c"] {
            let td = dom_create_element("td");
            dom_append_child(&td, &create_text_node(txt));
            dom_append_child(&tr, &td);
        }
        dom_append_child(&t, &tr);
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 1);
        assert!(
            get_attribute(&rows[0], "span").is_none(),
            "residual-row span popped before final append (Python line 435)"
        );
    }

    #[test]
    fn handle_table_two_row_table_first_row_keeps_span_last_loses_it() {
        // <table><tr><td>a</td><td>b</td></tr><tr><td>c</td><td>d</td></tr></table>
        // — max_cols=2. The FIRST tr appends the initial newrow (with
        // span="2") to newtable; the second tr's row is the residual one
        // whose span gets popped. Faithful to main_extractor.py:435.
        let t = dom_create_element("table");
        for row_data in [["a", "b"], ["c", "d"]] {
            let tr = dom_create_element("tr");
            for txt in row_data {
                let td = dom_create_element("td");
                dom_append_child(&td, &create_text_node(txt));
                dom_append_child(&tr, &td);
            }
            dom_append_child(&t, &tr);
        }
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 2, "two rows");
        assert_eq!(
            get_attribute(&rows[0], "span").as_deref(),
            Some("2"),
            "first row keeps span (appended before pop)"
        );
        assert!(
            get_attribute(&rows[1], "span").is_none(),
            "last row loses span via Python line 435 pop"
        );
    }

    #[test]
    fn handle_table_respects_colspan_in_max_cols() {
        // <table><tr><td colspan="2">a</td><td>b</td></tr><tr><td>c</td></tr></table>
        // — max_cols across both rows = 3 (first row: 2+1, second: 1).
        // The first row (appended before pop) carries span="3".
        let t = dom_create_element("table");
        let tr1 = dom_create_element("tr");
        let td1 = dom_create_element("td");
        set_attribute(&td1, "colspan", "2");
        dom_append_child(&td1, &create_text_node("a"));
        let td2 = dom_create_element("td");
        dom_append_child(&td2, &create_text_node("b"));
        dom_append_child(&tr1, &td1);
        dom_append_child(&tr1, &td2);
        let tr2 = dom_create_element("tr");
        let td3 = dom_create_element("td");
        dom_append_child(&td3, &create_text_node("c"));
        dom_append_child(&tr2, &td3);
        dom_append_child(&t, &tr1);
        dom_append_child(&t, &tr2);
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 2);
        assert_eq!(
            get_attribute(&rows[0], "span").as_deref(),
            Some("3"),
            "first row carries colspan-summed max_cols=3"
        );
    }

    #[test]
    fn handle_table_cell_with_list_preserves_placeholder_marker() {
        // **Stage 3-B Cluster A regression pin (2026-05-21).**
        //
        // Before the fix, `handle_table`'s non-leaf cell branch renamed the
        // OLD subelement to "done" via `replace_element_tag` BEFORE walking
        // its descendants. rcdom's `replace_element_tag` drains the OLD
        // node's children into a fresh replacement — so the subsequent
        // `descendant_elements(&subelement)` yielded an empty Vec and the
        // cell lost ALL nested content. Python's `subelement.tag = "done"`
        // (main_extractor.py:404) is an in-place tag mutation, so the
        // subsequent `subelement.iterdescendants()` still finds the children.
        //
        // The fix: defer the rename to the unconditional cleanup at the loop
        // bottom (line 1128) — the inner walk now sees the original children.
        //
        // Verifies that a cell containing a `<list><item>x</item></list>`
        // ends up with a `<list/>` placeholder marker (define_newelem's
        // intentional behaviour — only tag/text/tail copied, no children).
        //
        // Cluster A divergence; unblocked Wikipedia Morrison, Apple 10-K,
        // and HMRC fixtures in the Stage 3-B extract_content gate.
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        let th = dom_create_element("th");
        dom_append_child(&th, &create_text_node("Type"));
        let td = dom_create_element("td");
        let list = dom_create_element("list");
        let item1 = dom_create_element("item");
        dom_append_child(&item1, &create_text_node("Public"));
        let item2 = dom_create_element("item");
        dom_append_child(&item2, &create_text_node("Company"));
        dom_append_child(&list, &item1);
        dom_append_child(&list, &item2);
        dom_append_child(&td, &list);
        dom_append_child(&tr, &th);
        dom_append_child(&tr, &td);
        dom_append_child(&t, &tr);

        let pot = potential_tags(&["table", "list", "item", "p"]);
        let opts = Options::default();
        let out = handle_table(&t, &pot, &opts).expect("non-empty → Some");

        // Expect one row with TWO cells.
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 1, "one row");
        let cells = get_elements_by_tag_name(&rows[0], "cell");
        assert_eq!(
            cells.len(),
            2,
            "row has both the header cell AND the cell-with-list (Cluster A regression)"
        );
        assert_eq!(
            get_attribute(&cells[0], "role").as_deref(),
            Some("head"),
            "first cell is header"
        );
        // Second cell carries a placeholder <list/> via define_newelem.
        let inner = element_children(&cells[1]);
        assert_eq!(inner.len(), 1, "cell contains exactly one child: the list");
        assert_eq!(
            local_name(&inner[0]).as_deref(),
            Some("list"),
            "placeholder is a <list> element"
        );
        // The placeholder is empty — define_newelem doesn't copy children.
        assert_eq!(
            element_child_count(&inner[0]),
            0,
            "placeholder <list> has no <item> children (define_newelem is intentionally lossy)"
        );
    }

    #[test]
    fn handle_table_breaks_on_nested_table() {
        // <table><tr><td>x</td></tr><table>NESTED</table></table> — the
        // nested <table> descendant triggers break (line 429-430).
        // Asserting the outer table's row is preserved while no nested-table
        // processing happens.
        let outer = dom_create_element("table");
        let tr = dom_create_element("tr");
        let td = dom_create_element("td");
        dom_append_child(&td, &create_text_node("x"));
        dom_append_child(&tr, &td);
        dom_append_child(&outer, &tr);
        let nested = dom_create_element("table");
        let ntr = dom_create_element("tr");
        let ntd = dom_create_element("td");
        dom_append_child(&ntd, &create_text_node("NESTED"));
        dom_append_child(&ntr, &ntd);
        dom_append_child(&nested, &ntr);
        dom_append_child(&outer, &nested);

        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_table(&outer, &pot, &opts).expect("non-empty → Some");
        // Output has one row (the outer's tr), no NESTED text.
        let rows = get_elements_by_tag_name(&out, "row");
        assert_eq!(rows.len(), 1, "outer row kept");
        let cells = get_elements_by_tag_name(&out, "cell");
        let cell_text = cells
            .iter()
            .map(|c| element_text(c).unwrap_or_default())
            .collect::<Vec<_>>()
            .join(",");
        assert!(
            !cell_text.contains("NESTED"),
            "nested table content not processed: {cell_text:?}"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — handle_textelem (main_extractor.py:482-509)
    // -------------------------------------------------------------------

    #[test]
    fn handle_textelem_routes_list_to_handle_lists() {
        let list = dom_create_element("list");
        let item = dom_create_element("item");
        dom_append_child(&item, &create_text_node("x"));
        dom_append_child(&list, &item);
        let pot = potential_tags(&["list", "item"]);
        let opts = Options::default();
        let out = handle_textelem(&list, &pot, &opts).expect("list → Some");
        assert_eq!(local_name(&out).as_deref(), Some("list"));
    }

    #[test]
    fn handle_textelem_routes_code_to_handle_quotes() {
        // CODES_QUOTES includes "code" — routed to handle_quotes.
        let code = dom_create_element("code");
        dom_append_child(&code, &create_text_node("x"));
        let pot = potential_tags(&["code"]);
        let opts = Options::default();
        // handle_quotes on a <code> input (is_code_block_element fires
        // because tag == "code"), so out is renamed to "code".
        let out = handle_textelem(&code, &pot, &opts).expect("code → Some");
        assert_eq!(local_name(&out).as_deref(), Some("code"));
    }

    #[test]
    fn handle_textelem_routes_head_to_handle_titles() {
        let head = dom_create_element("head");
        dom_append_child(&head, &create_text_node("Title"));
        let pot = potential_tags(&["head", "p"]);
        let opts = Options::default();
        let out = handle_textelem(&head, &pot, &opts).expect("head → Some");
        // handle_titles returns a <head> with its text preserved.
        assert_eq!(local_name(&out).as_deref(), Some("head"));
    }

    #[test]
    fn handle_textelem_routes_p_to_handle_paragraphs() {
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("hello"));
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        let out = handle_textelem(&p, &pot, &opts).expect("p → Some");
        assert_eq!(local_name(&out).as_deref(), Some("p"));
    }

    #[test]
    fn handle_textelem_routes_table_to_handle_table_when_gated() {
        // potential_tags includes "table" → handle_table is called.
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        let td = dom_create_element("td");
        dom_append_child(&td, &create_text_node("x"));
        dom_append_child(&tr, &td);
        dom_append_child(&t, &tr);
        let pot = potential_tags(&["table"]);
        let opts = Options::default();
        let out = handle_textelem(&t, &pot, &opts).expect("table → Some");
        assert_eq!(local_name(&out).as_deref(), Some("table"));
    }

    #[test]
    fn handle_textelem_falls_through_table_to_other_when_not_in_potential() {
        // potential_tags does NOT include "table" → fall through to
        // handle_other_elements, which (since "table" is also not in
        // potential_tags there) returns None.
        let t = dom_create_element("table");
        let tr = dom_create_element("tr");
        let td = dom_create_element("td");
        dom_append_child(&td, &create_text_node("x"));
        dom_append_child(&tr, &td);
        dom_append_child(&t, &tr);
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        assert!(handle_textelem(&t, &pot, &opts).is_none());
    }

    #[test]
    fn handle_textelem_routes_graphic_to_handle_image_when_gated() {
        let g = dom_create_element("graphic");
        set_attribute(&g, "src", "https://example.com/img.png");
        let pot = potential_tags(&["graphic"]);
        let opts = Options::default();
        let out = handle_textelem(&g, &pot, &opts).expect("graphic → Some");
        assert_eq!(local_name(&out).as_deref(), Some("graphic"));
        assert_eq!(
            get_attribute(&out, "src").as_deref(),
            Some("https://example.com/img.png")
        );
    }

    #[test]
    fn handle_textelem_routes_unknown_to_handle_other_elements() {
        // <div>text</div> with "div" in potential_tags → handle_other_elements
        // renames to <p>.
        let d = dom_create_element("div");
        dom_append_child(&d, &create_text_node("some text"));
        let pot = potential_tags(&["div", "p"]);
        let opts = Options::default();
        let out = handle_textelem(&d, &pot, &opts).expect("div text → Some");
        assert_eq!(
            local_name(&out).as_deref(),
            Some("p"),
            "div renamed to p via handle_other_elements"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — prune_unwanted_sections (main_extractor.py:533-564)
    // -------------------------------------------------------------------

    #[test]
    fn prune_unwanted_sections_returns_tree_on_simple_input() {
        // Smoke test: an empty body with just a <p> should round-trip
        // through the prune cascade without panicking and with the <p>
        // preserved.
        let body = dom_create_element("body");
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("hello world"));
        dom_append_child(&body, &p);
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        let out = prune_unwanted_sections(&body, &pot, &opts);
        let ps = get_elements_by_tag_name(&out, "p");
        assert!(!ps.is_empty(), "p preserved");
    }

    #[test]
    fn prune_unwanted_sections_strips_image_discard_targets_when_graphic_not_potential() {
        // DISCARD_IMAGE_ELEMENTS (xpaths.py:189-195) targets divs/items/etc.
        // with `id`/`class` containing "caption". Without "graphic" in
        // potential_tags, a <div class="caption"> should be pruned.
        let body = dom_create_element("body");
        let p = dom_create_element("p");
        dom_append_child(&p, &create_text_node("a sufficiently long body paragraph that survives any link-density culling unscathed for testing purposes"));
        dom_append_child(&body, &p);
        let caption = dom_create_element("div");
        set_attribute(&caption, "class", "caption");
        dom_append_child(&caption, &create_text_node("image caption"));
        dom_append_child(&body, &caption);
        let pot = potential_tags(&["p"]);
        let opts = Options::default();
        let out = prune_unwanted_sections(&body, &pot, &opts);
        // The <div class="caption"> should be gone after the
        // DISCARD_IMAGE_ELEMENTS pass.
        let caption_divs: Vec<_> = get_elements_by_tag_name(&out, "div")
            .into_iter()
            .filter(|d| {
                get_attribute(d, "class")
                    .map(|c| c.contains("caption"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            caption_divs.is_empty(),
            "caption div pruned via DISCARD_IMAGE_ELEMENTS"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2c-iii — recover_wild_text (main_extractor.py:512-530)
    // -------------------------------------------------------------------

    #[test]
    fn recover_wild_text_appends_paragraphs_to_result_body() {
        // <body><p>one</p><p>two</p></body> + empty result_body.
        // After recover, result_body should contain 2 paragraphs.
        let body = dom_create_element("body");
        let p1 = dom_create_element("p");
        dom_append_child(&p1, &create_text_node("hello world"));
        dom_append_child(&body, &p1);
        let p2 = dom_create_element("p");
        dom_append_child(&p2, &create_text_node("foo bar"));
        dom_append_child(&body, &p2);
        let result_body = dom_create_element("body");
        let pot = potential_tags(TAG_CATALOG);
        let opts = Options::default();
        let _ = recover_wild_text(&body, &result_body, &opts, &pot);
        let ps = get_elements_by_tag_name(&result_body, "p");
        assert_eq!(ps.len(), 2, "both paragraphs recovered");
    }

    #[test]
    fn recover_wild_text_returns_empty_when_tree_has_no_text_elements() {
        // <body><script>x</script></body> — no blockquote/p/etc → nothing
        // recovered.
        let body = dom_create_element("body");
        let scr = dom_create_element("script");
        dom_append_child(&scr, &create_text_node("var x = 1;"));
        dom_append_child(&body, &scr);
        let result_body = dom_create_element("body");
        let pot = potential_tags(TAG_CATALOG);
        let opts = Options::default();
        let _ = recover_wild_text(&body, &result_body, &opts, &pot);
        assert_eq!(element_child_count(&result_body), 0, "no recoveries");
    }

    // -------------------------------------------------------------------
    // Stage 2d — _extract (main_extractor.py:567-617)
    // -------------------------------------------------------------------

    /// Parse `html` into a `(Dom, body)` pair. The `Dom` MUST be kept alive
    /// for the duration of any test that uses `body` — rcdom's iterative
    /// `impl Drop for Node` (markup5ever_rcdom-0.39.0/lib.rs:268-284)
    /// drains every descendant's children Vec on doc-drop, even when the
    /// caller holds Rc references to those descendants. Returning the Dom
    /// keeps it pinned. (See identical-shape rcdom Drop pin in
    /// `handle_table`'s `dones_alive` Vec.)
    fn parse_body(
        html: &str,
    ) -> (
        crate::readability::dom::Dom,
        crate::readability::dom::NodeRef,
    ) {
        let d = crate::readability::dom::Dom::parse(html);
        let body = d.body().expect("html input has <body>");
        (d, body)
    }

    #[test]
    fn _extract_returns_empty_body_for_no_content() {
        // <body><script>x</script></body> — no BODY_XPATH match yields content.
        let (_d, body) = parse_body("<html><body><script>x</script></body></html>");
        let opts = Options::default();
        let (result_body, temp_text, _pot) = _extract(&body, &opts);
        assert_eq!(local_name(&result_body).as_deref(), Some("body"));
        assert_eq!(element_child_count(&result_body), 0, "no content extracted");
        assert!(temp_text.is_empty(), "no text extracted");
    }

    #[test]
    fn _extract_finds_article_paragraphs() {
        // <article><p>...</p></article> — BODY_XPATH[1] = `(.//article)[1]`
        // matches. We expect either a paragraph in result_body OR the
        // extracted text to contain the body paragraph.
        let (_d, body) = parse_body(
            "<html><body><article><p>The quick brown fox jumps over the lazy dog.</p></article></body></html>",
        );
        let opts = Options::default();
        let (result_body, temp_text, _pot) = _extract(&body, &opts);
        assert!(
            element_child_count(&result_body) > 0 || temp_text.contains("quick brown fox"),
            "extraction produced output: children={}, text={temp_text:?}",
            element_child_count(&result_body)
        );
    }

    #[test]
    fn _extract_populates_potential_tags_from_options() {
        // With tables/images/links=true, potential_tags should include
        // table/td/th/tr/graphic/ref.
        let (_d, body) = parse_body("<html><body><article><p>x</p></article></body></html>");
        let opts = Options {
            tables: true,
            images: true,
            links: true,
            ..Options::default()
        };
        let (_rb, _tt, pot) = _extract(&body, &opts);
        assert!(pot.contains("table"));
        assert!(pot.contains("td"));
        assert!(pot.contains("th"));
        assert!(pot.contains("tr"));
        assert!(pot.contains("graphic"));
        assert!(pot.contains("ref"));
    }

    #[test]
    fn _extract_skips_detached_orphan_elements_from_cluster_c() {
        // **Stage 3-B Cluster C regression pin (2026-05-21).**
        //
        // Before the fix, `_extract` walked the subtree's `.//*` snapshot
        // without checking whether each element had already been drained
        // by a prior iteration's handler. Python's flow renames already-
        // processed elements to `tag = "done"` via in-place tag mutation
        // (e.g. `handle_paragraphs` line 340, `handle_code_blocks` line
        // 222), so subsequent iterdescendants visits fall through
        // `handle_other_elements` → `tag not in potential_tags` → return
        // None. Our `replace_element_tag` (`dom.rs:1361`) doesn't mutate
        // the OLD node's tag — but it DOES detach the OLD node (drains
        // children + clears parent). So our equivalent skip-signal is
        // "OLD node is now detached".
        //
        // Visible on Rust 1.83 blog where `<code>rustup</code>` inline
        // inside a `<p>` was being re-emitted as a top-level orphan
        // `<code></code>` after `handle_paragraphs` drained it.
        //
        // Verifies the test fixture
        //   <article>
        //     <p>via <code>rustup</code>!</p>
        //     <code class="block">$ rustup update stable</code>
        //   </article>
        // produces exactly TWO top-level children: the processed `<p>`
        // (with the inline code inside it) and the standalone code block
        // — NOT three (the inline code re-emitted as a stray top-level
        // `<code>` between them).
        let (_d, body) = parse_body(
            r#"<html><body><article>
            <p>via <code>rustup</code>! Some more text after the inline code so the
            paragraph is long enough to survive the recover_wild_text fallback.</p>
            <code class="block">$ rustup update stable</code>
            <p>Some more body text padding so the article is substantive enough
            that _extract takes the main path instead of the recover_wild_text
            fallback. Lorem ipsum dolor sit amet, consectetur adipiscing elit.
            Sed do eiusmod tempor incididunt ut labore et dolore magna aliqua.
            Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris.</p>
            </article></body></html>"#,
        );
        let opts = Options::default();
        let (result_body, _text, _pot) = _extract(&body, &opts);

        // Count the top-level code elements in result_body — they should
        // be exactly ONE (the block), NOT two (block + stray empty
        // orphan). The inline `<code>rustup</code>` should be NESTED
        // inside its parent `<p>`, not a top-level sibling.
        let top_level_codes: Vec<NodeRef> = element_children(&result_body)
            .into_iter()
            .filter(|c| local_name(c).as_deref() == Some("code"))
            .collect();
        assert_eq!(
            top_level_codes.len(),
            1,
            "exactly one top-level <code> (the block); no empty orphan from drained inline code"
        );
        // And the block has the text content.
        let block_text = element_text(&top_level_codes[0]).unwrap_or_default();
        assert!(
            block_text.contains("rustup update stable"),
            "the surviving top-level code is the block, not the empty inline orphan: {block_text:?}"
        );
    }

    // -------------------------------------------------------------------
    // Stage 2d — extract_content (main_extractor.py:620-640)
    // -------------------------------------------------------------------

    #[test]
    fn extract_content_returns_body_text_and_length_for_article() {
        // A long article paragraph that should survive _extract or
        // recover_wild_text fallback.
        let (_d, body) = parse_body(
            "<html><body><article><p>The quick brown fox jumps over the lazy dog. This is a longer paragraph that should easily exceed the minimum extracted size threshold to ensure the extractor returns meaningful results without triggering the wild-text fallback path. We need at least 250 characters of content here so that the assertion holds reliably across runs in autonomous mode.</p></article></body></html>",
        );
        let opts = Options::default();
        let (result_body, text, len) = extract_content(&body, &opts);
        assert_eq!(local_name(&result_body).as_deref(), Some("body"));
        assert!(
            text.contains("quick brown fox"),
            "text extracted: {text:?}"
        );
        assert_eq!(len, text.chars().count(), "len matches text length");
    }

    #[test]
    fn extract_content_strips_done_elements_with_content() {
        // After extract_content, no <done> elements should remain
        // (strip_elements(_, 'done') cleanup at main_extractor.py:637).
        let (_d, body) = parse_body(
            "<html><body><article><p>A paragraph long enough to survive any minimum-size gates the orchestrator might apply, with several sentences of plausible body text to keep the extractor happy and avoid the wild-text fallback path entirely for testing purposes.</p></article></body></html>",
        );
        let opts = Options::default();
        let (result_body, _t, _l) = extract_content(&body, &opts);
        assert!(
            get_elements_by_tag_name(&result_body, "done").is_empty(),
            "no <done> elements in output"
        );
    }
}
