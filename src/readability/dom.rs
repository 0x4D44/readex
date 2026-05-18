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
        let dom = parse_document(RcDom::default(), opts).one(html);
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

    // --- score side-table (HLD §5 / §5.1 — point-query-only) -------------

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
        let dom = Dom::parse("<div><!--x--></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")
            .into_iter()
            .next()
            .unwrap();
        let comment = child_nodes(&div).into_iter().next().unwrap();
        assert!(matches!(comment.data, NodeData::Comment { .. }));
        assert_eq!(text_content(&comment), "");
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
        // child_nodes: t1, span, comment, t2, b = 5
        assert_eq!(child_nodes(&div).len(), 5);
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
}
