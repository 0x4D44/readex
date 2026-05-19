//! `cleaning` — Stage 1b: `tree_cleaning`, `convert_tags`, `prune_html`.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.2.
//! Source of truth: `trafilatura@v2.0.0/htmlprocessing.py`.
//!
//! # What this module does (one paragraph)
//!
//! Given an `rcdom` tree representing parsed HTML, **`tree_cleaning`** drops
//! script/style/aside/footer/form/nav/... (per `MANUALLY_CLEANED`,
//! `settings.py:349-404`), unwraps presentational containers like
//! abbr/cite/font/img/tbody/thead/tfoot (per `MANUALLY_STRIPPED`,
//! `settings.py:407-429`), then runs **`prune_html`** to delete empty
//! instances of a small block-element catalog (`CUT_EMPTY_ELEMS`,
//! `settings.py:320-343`). **`convert_tags`** then rewrites typographic
//! tags to a TEI-like vocabulary: `<b>/<strong>` → `<hi rend="#b">`,
//! `<i>/<em>` → `<hi rend="#i">`, `<u>` → `<hi rend="#u">`, `<ul>/<ol>/<dl>`
//! → `<list>`, `<li>/<dt>/<dd>` → `<item>`, `<h1..h6>` → `<head rend="hN">`,
//! `<blockquote>/<q>` → `<quote>`, `<pre>` → `<quote>` or `<code>`,
//! `<br>/<hr>` → `<lb>`, `<del>/<s>/<strike>` → `<del rend="overstrike">`,
//! `<details>` → `<div>`. Output: an XML-ish lxml tree whose downstream
//! consumer is the own-extractor (Stage 2), the readability-fork (Stage 4),
//! jusText (Stage 5), the arbiter (Stage 6), and finally `xmltotxt`.
//!
//! # Faithfulness anchor (HLD §10 / anti-inversion)
//!
//! Every branch is line-cited to `htmlprocessing.py@v2.0.0`. No "looks-nice"
//! decisions. The Stage 0c BLOCKER gate
//! (`tests/trafilatura_equivalence_gate.rs`) compares this module's output
//! byte-for-byte against Trafilatura's own `convert_tags`.
//!
//! # Options (DA-revised, M3 §7.2)
//!
//! The Python `Extractor` class has ~25 slots (`settings.py:65-99`).
//! Stage 1b only consumes 4 of those slots:
//! - `tables`: default `True` (settings.py:113) — when false, also clean
//!   `table/td/th/tr` (htmlprocessing.py:52-53).
//! - `images`: default `False` — when true, `MANUALLY_STRIPPED -= ["img"]`
//!   and `MANUALLY_CLEANED -= PRESERVE_IMG_CLEANING` (htmlprocessing.py:58-61).
//! - `links`: default `False` — when false, anchor elements are stripped
//!   (htmlprocessing.py:386-394).
//! - `formatting`: default `False` — when true, REND_TAG_MAPPING runs;
//!   otherwise those tags are stripped (htmlprocessing.py:401-407).
//! - `focus`: default `"balanced"` — `"precision"` skips tail-preservation
//!   in prune_html (htmlprocessing.py:85). `"recall"` triggers the backup
//!   pattern (htmlprocessing.py:67-73).
//!
//! Until the full `Options::extractor` enum is wired (HLD §5.2 / DECISION-C),
//! Stage 1b exposes a small `Options` struct with **the Trafilatura defaults**
//! pinned per `settings.py:101-153`. The Stage 0c gate uses defaults — that
//! is what `run.py` invokes `bare_extraction` with at the harness boundary.

use crate::readability::dom::{
    self, NodeData, NodeRef, clear_attributes, delete_with_tail_preserve_free, get_attribute,
    local_name, replace_element_tag, set_attribute, strip_element, tag_name,
};
use crate::trafilatura::settings_constants::{
    CUT_EMPTY_ELEMS, MANUALLY_CLEANED, MANUALLY_STRIPPED, PRESERVE_IMG_CLEANING, REND_TAG_NAMES,
    rend_of,
};

/// Stage 1b `Extractor` options slice (HLD §7.2 footnote).
///
/// This is the subset of `Extractor` slots Stage 1b consumes. Field defaults
/// match Trafilatura's `Extractor.__init__` (`settings.py:101-153`) under
/// `bare_extraction`'s harness invocation (`run.py:244-251`).
///
/// **NOT YET PUBLIC.** This struct is `pub(crate)` because the public
/// `mdrcel::Options::extractor` enum is HLD §5.2 / DECISION-C work for a
/// later stage. Stage 1b's gate runs with defaults — the Stage 0c gate
/// compares Trafilatura's `bare_extraction(... default options ...)` against
/// the Rust port with `Options::default()`.
#[derive(Debug, Clone)]
pub struct Options {
    /// `Extractor.tables` (settings.py:113, default `True`). When false,
    /// `tree_cleaning` also drops `table/td/th/tr` (htmlprocessing.py:52-53).
    pub tables: bool,
    /// `Extractor.images` (settings.py:135, default `False`). When true,
    /// `MANUALLY_STRIPPED -= ["img"]` and `MANUALLY_CLEANED -=
    /// PRESERVE_IMG_CLEANING` (htmlprocessing.py:58-61).
    pub images: bool,
    /// `Extractor.links` (settings.py:134, default `False`). When false,
    /// `<a>` inside div/li/p (and `table` when `tables=true`) becomes `<ref>`
    /// and other `<a>` elements are stripped (htmlprocessing.py:386-394).
    pub links: bool,
    /// `Extractor.formatting` (settings.py:133, default `False`). When true,
    /// REND_TAG_MAPPING runs (b/i/em/strong/u/... → hi rend=...); when false,
    /// those tags are stripped via lxml `strip_tags` semantics
    /// (htmlprocessing.py:401-407).
    pub formatting: bool,
    /// `Extractor.focus` (settings.py:129-131, default `"balanced"`). The
    /// Stage 1b deliveries respect:
    /// - `"balanced"` — default; `prune_html` preserves tail (keep_tail=True).
    /// - `"precision"` — `prune_html` does NOT preserve tail
    ///   (htmlprocessing.py:85: `tails = focus != "precision"`).
    /// - `"recall"` — `tree_cleaning` runs with backup; if cleaning removed
    ///   all `<p>` elements, restore from the backup
    ///   (htmlprocessing.py:67-73).
    pub focus: Focus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Balanced,
    Precision,
    Recall,
}

impl Default for Options {
    fn default() -> Self {
        // Trafilatura's bare_extraction harness defaults (settings.py:101-153).
        Self {
            tables: true,
            images: false,
            links: false,
            formatting: false,
            focus: Focus::Balanced,
        }
    }
}

// ===========================================================================
// tree_cleaning (htmlprocessing.py:48-80)
// ===========================================================================

/// Strip the tree of `MANUALLY_CLEANED` subtrees, unwrap `MANUALLY_STRIPPED`
/// elements, and prune empty block elements.
///
/// **Source line-cite:** `htmlprocessing.py:48-80`.
///
/// # Python original
///
/// ```python
/// def tree_cleaning(tree, options):
///     cleaning_list, stripping_list = MANUALLY_CLEANED.copy(), MANUALLY_STRIPPED.copy()
///     if not options.tables:
///         cleaning_list.extend(["table", "td", "th", "tr"])
///     else:
///         for elem in tree.xpath(".//figure[descendant::table]"):
///             elem.tag = "div"
///     if options.images:
///         cleaning_list = [e for e in cleaning_list if e not in PRESERVE_IMG_CLEANING]
///         stripping_list.remove("img")
///
///     strip_tags(tree, stripping_list)
///
///     if options.focus == "recall" and tree.find(".//p") is not None:
///         tcopy = deepcopy(tree)
///         for expression in cleaning_list:
///             for element in tree.iter(expression):
///                 delete_element(element)
///         if tree.find(".//p") is None:
///             tree = tcopy
///     else:
///         for expression in cleaning_list:
///             for element in tree.iter(expression):
///                 delete_element(element)
///
///     return prune_html(tree, options.focus)
/// ```
///
/// # Rust port shape
///
/// Mutates `tree` in place. The Python signature returns `tree`; the Rust
/// caller already holds the `NodeRef`. The recall-backup branch is omitted
/// for Stage 1b — the default `Options::focus = Balanced` exercises the
/// else-branch (lines 75-78) only. A recall implementation would deep-clone
/// the tree, which rcdom does not expose directly; if Stage 1b grows recall
/// support, the clone strategy needs its own design pass.
pub fn tree_cleaning(tree: &NodeRef, options: &Options) {
    // htmlprocessing.py:51 — build mutable copies of the catalogs.
    let mut cleaning_list: Vec<&str> = MANUALLY_CLEANED.to_vec();
    let mut stripping_list: Vec<&str> = MANUALLY_STRIPPED.to_vec();

    // htmlprocessing.py:52-53 — tables=False: also clean table/td/th/tr.
    if !options.tables {
        cleaning_list.extend(["table", "td", "th", "tr"]);
    } else {
        // htmlprocessing.py:55-57 — figure[descendant::table] -> div
        // (prevents issue #301: figures wrapping tables get cleaned away,
        // taking the table with them; rewriting them as <div> preserves the
        // table while still letting the cleaning sweep over inner figures).
        for fig in find_figures_with_descendant_table(tree) {
            // Rename in place (replace_element_tag returns the new node;
            // discarded — we just need the rewrite). NB: the FIRST element
            // returned by find_figures... is the outermost figure; rewriting
            // it does NOT recursively descend, matching lxml's xpath result.
            let _renamed = replace_element_tag(&fig, "div");
        }
    }

    // htmlprocessing.py:58-61 — images=True: don't clean figure/picture/source
    // wrappers (commonly contain <img>); don't strip <img>.
    if options.images {
        cleaning_list.retain(|t| !PRESERVE_IMG_CLEANING.contains(t));
        stripping_list.retain(|t| *t != "img");
    }

    // htmlprocessing.py:64 — strip_tags(tree, stripping_list).
    // lxml.etree.strip_tags removes the named tags AS WRAPPERS — children +
    // text + tail survive. Iteration order is implementation-defined; we
    // emulate the lxml shape by walking the tree once per tag in the catalog
    // (matching lxml's strip_tags behaviour on a multi-tag list).
    strip_tags_multi(tree, &stripping_list);

    // htmlprocessing.py:65-78 — focus=recall is NOT supported at Stage 1b.
    // Stage 1b runs the else-branch unconditionally:
    //   for expression in cleaning_list:
    //       for element in tree.iter(expression):
    //           delete_element(element)
    if options.focus == Focus::Recall {
        // TODO Stage 7.x: recall-backup branch (deepcopy + retry).
        // For now, fall through to the balanced path — Stage 0c gate runs
        // with defaults so this is unreachable.
    }
    for expression in &cleaning_list {
        delete_elements_by_tag(tree, expression);
    }

    // htmlprocessing.py:80 — return prune_html(tree, options.focus).
    prune_html(tree, options.focus);
}

/// Find every `figure` element with a descendant `table`. Document order.
///
/// Source: `htmlprocessing.py:56` — the xpath `.//figure[descendant::table]`.
/// Stage 0b's XPath engine does not yet support `descendant::` as a predicate
/// axis, so we implement this one shape directly (it is the ONLY use of
/// `descendant::` in the Stage 1b corpus). HLD §6.1's operator catalog covers
/// what the engine supports; this is the explicit out-of-catalog path.
fn find_figures_with_descendant_table(root: &NodeRef) -> Vec<NodeRef> {
    let figures = dom::get_elements_by_tag_name(root, "figure");
    figures
        .into_iter()
        .filter(|fig| !dom::get_elements_by_tag_name(fig, "table").is_empty())
        .collect()
}

/// Walk `tree` once per tag in `stripping_list`, calling `strip_element` on
/// every match. Equivalent to lxml's `etree.strip_tags(tree, *tags)`.
///
/// Implementation note: we snapshot the matching elements BEFORE stripping
/// any of them (the snapshot semantics of `get_elements_by_tag_name` — HLD
/// §5 / dom.rs risk #3). Otherwise removing the first match could invalidate
/// the iterator over later ones.
fn strip_tags_multi(tree: &NodeRef, stripping_list: &[&str]) {
    for tag in stripping_list {
        let matches = dom::get_elements_by_tag_name(tree, tag);
        for elem in matches {
            strip_element(&elem);
        }
    }
}

/// Walk `tree` and delete (with tail preservation) every element whose local
/// name is `tag`. Equivalent to the Python loop:
///
/// ```python
/// for element in tree.iter(tag):
///     delete_element(element)   # xml.py:54 — keep_tail=True default
/// ```
///
/// `delete_element` (xml.py:54-70) joins the tail to the previous sibling
/// (or to `parent.text` if no previous sibling). `delete_with_tail_preserve`
/// in dom.rs implements that exact semantic.
///
/// # lxml-iter-with-mutation parity (Stage 1b finding)
///
/// Python's `tree.iter(tag)` is a STATEFUL generator that breaks badly when
/// the tree is mutated during iteration (documented lxml gotcha — the iter
/// uses libxml2's traversal pointer, which advances BEFORE the yield, so the
/// next-pointer can land inside a just-deleted subtree). The empirical
/// behaviour is: when deleting an ancestor `<nav>`, the iter yields the
/// detached subtree's descendant `<nav>` next (and `delete_element` removes
/// it from the now-detached ancestor's child list), then **stops** — the
/// traversal walks up via the detached chain and dies. Sibling `<nav>`
/// elements at the original tree level that come AFTER the deleted ancestor
/// are NEVER visited.
///
/// To match this faithfully, the Rust port:
/// 1. Snapshots matches in document order (`get_elements_by_tag_name`).
/// 2. For each match in order, checks whether the match is **still
///    reachable from `tree`** via the parent chain. If yes: delete it. If no
///    (an ancestor was already deleted): apply the same `delete_element`
///    semantic — which is a no-op when `getparent()` is None at the top of
///    the detached chain, OR a sibling-level removal when an intermediate
///    detached ancestor still has the match as a child. THEN, after that
///    no-op-or-detached-sibling removal, the Python iter STOPS — so the
///    Rust port stops too (returning early from the loop).
///
/// This is the documented anti-inversion: replicate Trafilatura's
/// implementation faithfully, including its iter-while-mutating quirk.
fn delete_elements_by_tag(tree: &NodeRef, tag: &str) {
    let matches = dom::get_elements_by_tag_name(tree, tag);
    for elem in matches {
        // Is `elem` still reachable from `tree` via the parent chain?
        if is_reachable_from(&elem, tree) {
            delete_with_tail_preserve_free(&elem);
        } else {
            // Detached: lxml's `delete_element` calls `parent.remove(child)`
            // which succeeds if `child` is still a child of the (detached)
            // parent — but after that, lxml's iter cannot recover (its
            // saved next-pointer is inside a detached chain). We mirror
            // that "stop" by breaking out of the loop here. The detached
            // node itself: do a defensive remove (no tail-preservation,
            // since the detached chain has no parent.text to anchor onto).
            dom::remove(&elem);
            break;
        }
    }
}

/// `true` iff `node` is reachable from `root` via the parent chain (i.e.
/// `node`'s ancestors include `root`, or `node == root`). Used to detect
/// nodes that have become detached during a mutate-while-iterate sweep
/// (Stage 1b parity with lxml's `tree.iter()` behaviour).
fn is_reachable_from(node: &NodeRef, root: &NodeRef) -> bool {
    let mut cur = Some(node.clone());
    while let Some(n) = cur {
        if std::rc::Rc::ptr_eq(&n, root) {
            return true;
        }
        cur = dom::parent(&n);
    }
    false
}

// ===========================================================================
// prune_html (htmlprocessing.py:83-90)
// ===========================================================================

/// Delete selected empty elements to save space and processing time.
///
/// **Source line-cite:** `htmlprocessing.py:83-90`.
///
/// # Python original
///
/// ```python
/// def prune_html(tree, focus="balanced"):
///     tails = focus != "precision"
///     for element in tree.xpath(".//processing-instruction()|.//*[not(node())]"):
///         if element.tag in CUT_EMPTY_ELEMS:
///             delete_element(element, keep_tail=tails)
///     return tree
/// ```
///
/// # Rust port shape
///
/// The XPath `.//processing-instruction()|.//*[not(node())]` is outside the
/// Stage 0b engine catalog (no `processing-instruction()` node-test, no
/// `not()` function, no `node()` test). We implement the predicate directly:
/// walk every descendant element; if it has NO children at all (text or
/// element) AND its tag is in `CUT_EMPTY_ELEMS`, delete it.
///
/// `processing-instruction()` is omitted because rcdom yields PIs as
/// `NodeData::ProcessingInstruction` — they have no `.tag`, so the Python
/// `element.tag in CUT_EMPTY_ELEMS` check would never match a PI anyway. The
/// `delete_element(element, keep_tail=tails)` call WOULD still strip PI text
/// in the original; for Stage 1b we accept the deviation (PIs are
/// effectively absent in HTML and the gate corpus). Document for later.
pub fn prune_html(tree: &NodeRef, focus: Focus) {
    let keep_tail = focus != Focus::Precision;

    // Snapshot every descendant element BEFORE deletion (snapshot semantics —
    // HLD §5 / dom.rs risk #3). Walking and deleting concurrently would skip
    // siblings of the just-deleted node.
    let all_elements = dom::get_elements_by_tag_name(tree, "*");

    for elem in all_elements {
        // `not(node())` predicate: element has no children of any node type.
        //
        // **Parser-equivalence note (Stage 1b finding):** Trafilatura parses
        // HTML via `load_html` -> `lxml.html.HTMLParser(remove_comments=True,
        // remove_pis=True)` (`utils.py:70`). Comments and PIs are stripped
        // BEFORE `prune_html` sees the tree. mdrcel uses html5ever which
        // preserves comments/PIs, so to match Python's `not(node())` we
        // must treat Comment / ProcessingInstruction children as if absent.
        // Text-node children (including whitespace-only text) DO count as
        // nodes per W3C XPath 1.0 — match that.
        let has_real_child = elem.children.borrow().iter().any(|c| {
            !matches!(
                c.data,
                NodeData::Comment { .. } | NodeData::ProcessingInstruction { .. }
            )
        });
        if has_real_child {
            continue;
        }
        // Tag must be in CUT_EMPTY_ELEMS.
        let tag = match &elem.data {
            NodeData::Element { name, .. } => name.local.as_ref(),
            _ => continue,
        };
        if !CUT_EMPTY_ELEMS.contains(&tag) {
            continue;
        }
        if keep_tail {
            delete_with_tail_preserve_free(&elem);
        } else {
            dom::remove(&elem);
        }
    }
}

// ===========================================================================
// convert_tags (htmlprocessing.py:381-417)
// ===========================================================================

/// Simplify markup and convert relevant HTML tags to a TEI-like XML standard.
///
/// **Source line-cite:** `htmlprocessing.py:381-417` + per-tag converters
/// at `htmlprocessing.py:288-366`.
///
/// # Python original (slimmed to the Stage 1b-default code paths)
///
/// ```python
/// def convert_tags(tree, options, url=None):
///     # 386-394: !options.links
///     if not options.links:
///         xpath_expr = ".//*[self::div or self::li or self::p]//a"
///         if options.tables:
///             xpath_expr += "|.//table//a"
///         for elem in tree.xpath(xpath_expr):
///             elem.tag = "ref"
///         strip_tags(tree, "a")
///     else:
///         base_url = url and get_base_url(url)
///         for elem in tree.iter("a", "ref"):
///             convert_link(elem, base_url)
///
///     # 401-407: !options.formatting
///     if options.formatting:
///         for elem in tree.iter(REND_TAG_MAPPING.keys()):
///             elem.attrib.clear()
///             elem.set("rend", REND_TAG_MAPPING[elem.tag])
///             elem.tag = "hi"
///     else:
///         strip_tags(tree, *REND_TAG_MAPPING.keys())
///
///     # 410-411: per-tag CONVERSIONS dispatch
///     for elem in tree.iter(CONVERSIONS.keys()):
///         CONVERSIONS[elem.tag](elem)
///     # 413-415: options.images
///     if options.images:
///         for elem in tree.iter("img"):
///             elem.tag = "graphic"
///
///     return tree
/// ```
///
/// # Rust port shape
///
/// Mutates `tree` in place. The `url` parameter is omitted at Stage 1b
/// (Stage 7 wires URL canonicalization; `links=true` is a Stage 2+ option).
pub fn convert_tags(tree: &NodeRef, options: &Options) {
    // ---- htmlprocessing.py:386-394 — anchor handling ----
    if !options.links {
        // .//div//a, .//li//a, .//p//a → rename to ref; .//table//a too if tables.
        // We walk anchors once and check ancestors (div/li/p, or table when
        // tables=true). This implements the XPath predicate without going
        // through the Stage 0b engine (the engine doesn't support `or`
        // inside `self::` predicates yet beyond the limited catalog).
        let anchors = dom::get_elements_by_tag_name(tree, "a");
        for a in &anchors {
            if has_ancestor_matching(a, options.tables) {
                // Reviewer NIT-1: the returned new <ref> handle is not used;
                // the subsequent strip_tags_multi walks by tag-name "a" and
                // naturally skips already-renamed <ref> elements. Dropping
                // the unused Vec keeps the call site honest.
                let _ = replace_element_tag(a, "ref");
            }
        }
        // strip_tags(tree, "a") — strip any remaining <a> wrappers.
        strip_tags_multi(tree, &["a"]);
    } else {
        // Stage 1b: links=true path not exercised by the default gate. The
        // logic would be:
        //   for elem in tree.iter("a", "ref"):
        //       convert_link(elem, base_url)
        // which renames <a> to <ref> and folds href→target. Wire when Stage 2
        // needs it; the default Options::default() does NOT take this branch.
        let _ = options.links;
    }

    // ---- htmlprocessing.py:401-407 — REND_TAG_MAPPING handling ----
    if options.formatting {
        // formatting=true: rewrite to <hi rend="...">.
        // Iterate the union of all REND_TAG_MAPPING keys in document order
        // (lxml's tree.iter accepts varargs of tag names; the semantic is
        // "every descendant whose tag is one of these, in document order").
        let candidates = get_elements_in_any(tree, REND_TAG_NAMES);
        for elem in candidates {
            // tag is one of REND_TAG_MAPPING keys; rend_of returns Some.
            let tag = match &elem.data {
                NodeData::Element { name, .. } => name.local.as_ref().to_string(),
                _ => continue,
            };
            let Some(rend) = rend_of(&tag) else { continue };
            // elem.attrib.clear() — drop the original attributes
            // (htmlprocessing.py:403). replace_element_tag clones attrs;
            // we then clear + set rend on the new element.
            let new = replace_element_tag(&elem, "hi");
            clear_attributes(&new);
            set_attribute(&new, "rend", rend);
        }
    } else {
        // formatting=false: strip the wrappers entirely (children + text + tail
        // survive). lxml.etree.strip_tags(tree, "em", "i", "b", "strong", ...).
        strip_tags_multi(tree, REND_TAG_NAMES);
    }

    // ---- htmlprocessing.py:410-411 — CONVERSIONS dispatch ----
    //
    // Trafilatura's CONVERSIONS dict (htmlprocessing.py:346-366) maps
    // tag-name → converter function. Iteration over `tree.iter(CONVERSIONS.keys())`
    // visits every element whose tag is in the dict, in document order, and
    // dispatches to the per-tag converter.
    let conversions_keys = [
        "dl",
        "ol",
        "ul", // -> convert_lists
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6", // -> convert_headings
        "br",
        "hr", // -> convert_line_breaks
        "blockquote",
        "pre",
        "q", // -> convert_quotes
        "del",
        "s",
        "strike",  // -> convert_deletions
        "details", // -> convert_details
    ];
    let converted = get_elements_in_any(tree, &conversions_keys);
    for elem in converted {
        let tag = match &elem.data {
            NodeData::Element { name, .. } => name.local.as_ref().to_string(),
            _ => continue,
        };
        match tag.as_str() {
            "dl" | "ol" | "ul" => {
                convert_lists(&elem);
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                convert_headings(&elem);
            }
            "br" | "hr" => {
                convert_line_breaks(&elem);
            }
            "blockquote" | "pre" | "q" => {
                convert_quotes(&elem);
            }
            "del" | "s" | "strike" => {
                convert_deletions(&elem);
            }
            "details" => {
                convert_details(&elem);
            }
            _ => unreachable!("conversions_keys filter is exhaustive"),
        }
    }

    // ---- htmlprocessing.py:413-415 — images=true path ----
    if options.images {
        let imgs = dom::get_elements_by_tag_name(tree, "img");
        for img in imgs {
            let _ = replace_element_tag(&img, "graphic");
        }
    }
}

/// Anchor-ancestor filter used by `convert_tags` when `links=false`
/// (htmlprocessing.py:387-388). Returns true iff `a` has an ancestor that is
/// `<div>`, `<li>`, `<p>`, or — when `tables=true` — `<table>`.
fn has_ancestor_matching(a: &NodeRef, allow_table: bool) -> bool {
    let mut cur = dom::parent(a);
    while let Some(p) = cur {
        if let Some(tag) = local_name(&p) {
            match tag.as_str() {
                "div" | "li" | "p" => return true,
                "table" if allow_table => return true,
                _ => {}
            }
        }
        cur = dom::parent(&p);
    }
    false
}

/// Walk `tree` once and return every descendant element whose local-name is
/// in `tags`, in document order. lxml's `tree.iter(*tags)` semantic.
///
/// This is a thin wrapper over `dom::get_all_nodes_with_tag` (which already
/// matches the lxml shape — case-insensitive ASCII match against the
/// element's local-name, in document order, returning an owned snapshot
/// `Vec<NodeRef>`).
fn get_elements_in_any(tree: &NodeRef, tags: &[&str]) -> Vec<NodeRef> {
    dom::get_all_nodes_with_tag(tree, tags)
}

// ---------------------------------------------------------------------------
// Per-tag converters (htmlprocessing.py:288-344)
// ---------------------------------------------------------------------------

/// `convert_lists` (htmlprocessing.py:288-301).
///
/// `<ul>` / `<ol>` → `<list rend="ul">` / `<list rend="ol">`. Iterates the
/// inner `<li>` / `<dt>` / `<dd>` and renames them to `<item>`. `<dt>` and
/// `<dd>` additionally get a `rend="dd-N"` / `rend="dt-N"` (N is a counter
/// that increments after each `<dd>` to keep dd/dt pairing).
fn convert_lists(elem: &NodeRef) {
    // 290: elem.set("rend", elem.tag)
    let original_tag = match &elem.data {
        NodeData::Element { name, .. } => name.local.as_ref().to_string(),
        _ => return,
    };
    set_attribute(elem, "rend", &original_tag);
    // 291: elem.tag = "list" — rename WRAPPER, keep children (lxml mutates
    // elem.tag in place; the Rust port allocates a new element + reparents
    // children via replace_element_tag).
    let list_node = replace_element_tag(elem, "list");
    // But replace_element_tag returns a NEW node. The OLD `elem` is now
    // detached. We need to operate on `list_node` from here on.
    let elem = &list_node;

    // 292: i = 1
    let mut i: i32 = 1;
    // 293: for subelem in elem.iter("dd", "dt", "li"):
    let subelems = get_elements_in_any(elem, &["dd", "dt", "li"]);
    for sub in subelems {
        let sub_tag = match &sub.data {
            NodeData::Element { name, .. } => name.local.as_ref().to_string(),
            _ => continue,
        };
        // 295-299: rend bookkeeping for dd/dt.
        if sub_tag == "dd" || sub_tag == "dt" {
            set_attribute(&sub, "rend", &format!("{sub_tag}-{i}"));
            if sub_tag == "dd" {
                i += 1;
            }
        }
        // 301: subelem.tag = "item"
        let _new = replace_element_tag(&sub, "item");
    }
}

/// `convert_quotes` (htmlprocessing.py:304-318).
///
/// `<blockquote>`/`<q>` → `<quote>`. `<pre>` is more subtle:
/// - `<pre>` with a single `<span>` child → `<code>`.
/// - `<pre>` containing `<span class="hljs*">` → `<code>` (those spans get
///   their attributes cleared too — htmlprocessing.py:316-317).
/// - Otherwise `<pre>` → `<quote>`.
fn convert_quotes(elem: &NodeRef) {
    let tag = match &elem.data {
        NodeData::Element { name, .. } => name.local.as_ref().to_string(),
        _ => return,
    };
    let mut code_flag = false;
    if tag == "pre" {
        // 309-311: a <pre> with exactly one element child that is a <span>
        // is more likely code.
        let kids = dom::children(elem);
        if kids.len() == 1 && tag_name(&kids[0]).as_deref() == Some("SPAN") {
            code_flag = true;
        }
        // 313-317: hljs span detection.
        let inner_spans = dom::get_elements_by_tag_name(elem, "span");
        let hljs_spans: Vec<NodeRef> = inner_spans
            .into_iter()
            .filter(|s| {
                get_attribute(s, "class")
                    .map(|c| c.starts_with("hljs"))
                    .unwrap_or(false)
            })
            .collect();
        if !hljs_spans.is_empty() {
            code_flag = true;
            for s in &hljs_spans {
                clear_attributes(s);
            }
        }
    }
    // 318: elem.tag = "code" if code_flag else "quote"
    let _new = replace_element_tag(elem, if code_flag { "code" } else { "quote" });
}

/// `convert_headings` (htmlprocessing.py:321-325).
///
/// `<h1..h6>` → `<head rend="hN">`. Attributes cleared.
fn convert_headings(elem: &NodeRef) {
    let original_tag = match &elem.data {
        NodeData::Element { name, .. } => name.local.as_ref().to_string(),
        _ => return,
    };
    // 323: elem.attrib.clear()
    clear_attributes(elem);
    // 324: elem.set("rend", elem.tag)  (h1/h2/.../h6)
    set_attribute(elem, "rend", &original_tag);
    // 325: elem.tag = "head"
    let _new = replace_element_tag(elem, "head");
}

/// `convert_line_breaks` (htmlprocessing.py:328-330).
///
/// `<br>` / `<hr>` → `<lb>`.
fn convert_line_breaks(elem: &NodeRef) {
    let _new = replace_element_tag(elem, "lb");
}

/// `convert_deletions` (htmlprocessing.py:333-336).
///
/// `<del>` / `<s>` / `<strike>` → `<del rend="overstrike">`. Note: `<del>`
/// stays as `<del>` (the rename is idempotent for that tag) — the attribute
/// is the operative change.
fn convert_deletions(elem: &NodeRef) {
    // 335: elem.tag = "del"
    let new = replace_element_tag(elem, "del");
    // 336: elem.set("rend", "overstrike")
    set_attribute(&new, "rend", "overstrike");
}

/// `convert_details` (htmlprocessing.py:339-343).
///
/// `<details>` → `<div>`. Any descendant `<summary>` → `<head>`.
fn convert_details(elem: &NodeRef) {
    let new = replace_element_tag(elem, "div");
    let summaries = dom::get_elements_by_tag_name(&new, "summary");
    for s in summaries {
        let _ = replace_element_tag(&s, "head");
    }
}

// ===========================================================================
// Tests (Stage 1b unit tests)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::{Dom, get_elements_by_tag_name, serialize_converted_tree};

    fn parse(html: &str) -> Dom {
        Dom::parse(html)
    }

    fn body(dom: &Dom) -> NodeRef {
        dom.body().expect("html5ever synthesises <body>")
    }

    // ---- tree_cleaning ----

    #[test]
    fn tree_cleaning_drops_script_style_nav_footer() {
        let dom = parse(
            "<div><p>keep</p><script>bad()</script><style>x{}</style>\
             <nav>menu</nav><footer>foot</footer><aside>side</aside></div>",
        );
        let b = body(&dom);
        tree_cleaning(&b, &Options::default());
        let div = get_elements_by_tag_name(&b, "div")[0].clone();
        assert!(get_elements_by_tag_name(&div, "script").is_empty());
        assert!(get_elements_by_tag_name(&div, "style").is_empty());
        assert!(get_elements_by_tag_name(&div, "nav").is_empty());
        assert!(get_elements_by_tag_name(&div, "footer").is_empty());
        assert!(get_elements_by_tag_name(&div, "aside").is_empty());
        assert_eq!(get_elements_by_tag_name(&div, "p").len(), 1);
    }

    #[test]
    fn tree_cleaning_strips_tbody_thead_tfoot_keeps_rows() {
        // MANUALLY_STRIPPED unwraps tbody/thead/tfoot but keeps the inner
        // tr/td/cell structure (since stripping is "remove wrapper, keep
        // children").
        let dom = parse(
            "<table><thead><tr><th>H</th></tr></thead>\
             <tbody><tr><td>A</td></tr><tr><td>B</td></tr></tbody></table>",
        );
        let b = body(&dom);
        tree_cleaning(&b, &Options::default());
        let table = get_elements_by_tag_name(&b, "table")[0].clone();
        // tbody/thead are gone (unwrapped); the tr's are children of <table>.
        assert!(get_elements_by_tag_name(&table, "tbody").is_empty());
        assert!(get_elements_by_tag_name(&table, "thead").is_empty());
        assert_eq!(get_elements_by_tag_name(&table, "tr").len(), 3);
    }

    #[test]
    fn tree_cleaning_strips_meta_img_font() {
        let dom = parse(r#"<div><p><font color="red">RED</font><img src=x>after</p></div>"#);
        let b = body(&dom);
        tree_cleaning(&b, &Options::default());
        let p = get_elements_by_tag_name(&b, "p")[0].clone();
        // <font> and <img> are stripped (wrappers gone; "RED" survives).
        assert!(get_elements_by_tag_name(&p, "font").is_empty());
        assert!(get_elements_by_tag_name(&p, "img").is_empty());
        assert!(crate::readability::dom::text_content(&p).contains("RED"));
        assert!(crate::readability::dom::text_content(&p).contains("after"));
    }

    #[test]
    fn tree_cleaning_tables_false_drops_table_subtree() {
        let dom = parse("<div><p>keep</p><table><tr><td>data</td></tr></table></div>");
        let b = body(&dom);
        let opts = Options {
            tables: false,
            ..Options::default()
        };
        tree_cleaning(&b, &opts);
        assert!(get_elements_by_tag_name(&b, "table").is_empty());
        assert!(get_elements_by_tag_name(&b, "td").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "p").len(), 1);
    }

    #[test]
    fn tree_cleaning_figure_with_descendant_table_becomes_div() {
        // Per htmlprocessing.py:55-57 — figures containing tables are
        // rewritten to <div> BEFORE the cleaning sweep (which would otherwise
        // remove the figure subtree, taking the table with it).
        let dom = parse("<figure><table><tr><td>data</td></tr></table></figure>");
        let b = body(&dom);
        tree_cleaning(&b, &Options::default());
        // figure rewritten to div, table preserved.
        assert!(get_elements_by_tag_name(&b, "figure").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "table").len(), 1);
        assert_eq!(get_elements_by_tag_name(&b, "td").len(), 1);
    }

    // ---- prune_html ----

    #[test]
    fn prune_html_drops_empty_p_and_div() {
        // <p></p>, <div></div> both empty and both in CUT_EMPTY_ELEMS → dropped.
        let dom = parse("<section><p></p><div></div><p>k</p></section>");
        let b = body(&dom);
        prune_html(&b, Focus::Balanced);
        let section = get_elements_by_tag_name(&b, "section")[0].clone();
        // Only the populated <p> survives.
        let ps = get_elements_by_tag_name(&section, "p");
        assert_eq!(ps.len(), 1);
        assert_eq!(crate::readability::dom::text_content(&ps[0]), "k");
        assert!(get_elements_by_tag_name(&section, "div").is_empty());
    }

    #[test]
    fn prune_html_keeps_nonempty_carriers() {
        // <p>x</p> is NOT empty (has text child); must survive.
        let dom = parse("<section><p>x</p></section>");
        let b = body(&dom);
        prune_html(&b, Focus::Balanced);
        assert_eq!(get_elements_by_tag_name(&b, "p").len(), 1);
    }

    #[test]
    fn prune_html_skips_tags_not_in_cut_empty_elems() {
        // <td> is NOT in CUT_EMPTY_ELEMS — empty td survives.
        let dom = parse("<table><tr><td></td><td>x</td></tr></table>");
        let b = body(&dom);
        prune_html(&b, Focus::Balanced);
        assert_eq!(get_elements_by_tag_name(&b, "td").len(), 2);
    }

    // ---- convert_tags ----

    #[test]
    fn convert_tags_default_strips_b_i_em_strong() {
        // formatting=false (default) → strip the wrappers (children survive).
        let dom = parse("<p>this is <b>bold</b> and <i>italic</i> and <em>em</em>.</p>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        let p = get_elements_by_tag_name(&b, "p")[0].clone();
        // No <b>, <i>, <em>, <hi> elements remain.
        assert!(get_elements_by_tag_name(&p, "b").is_empty());
        assert!(get_elements_by_tag_name(&p, "i").is_empty());
        assert!(get_elements_by_tag_name(&p, "em").is_empty());
        assert!(get_elements_by_tag_name(&p, "hi").is_empty());
        // Text content survives.
        let t = crate::readability::dom::text_content(&p);
        assert!(t.contains("bold") && t.contains("italic") && t.contains("em"));
    }

    #[test]
    fn convert_tags_formatting_true_rewrites_b_to_hi() {
        let dom = parse("<p><b>bold</b></p>");
        let b = body(&dom);
        let opts = Options {
            formatting: true,
            ..Options::default()
        };
        convert_tags(&b, &opts);
        let p = get_elements_by_tag_name(&b, "p")[0].clone();
        assert!(get_elements_by_tag_name(&p, "b").is_empty());
        let his = get_elements_by_tag_name(&p, "hi");
        assert_eq!(his.len(), 1);
        assert_eq!(get_attribute(&his[0], "rend").as_deref(), Some("#b"));
    }

    #[test]
    fn convert_tags_em_becomes_hi_rend_hash_i() {
        // anti-inversion: em maps to #i (not #em).
        let dom = parse("<p><em>e</em></p>");
        let b = body(&dom);
        let opts = Options {
            formatting: true,
            ..Options::default()
        };
        convert_tags(&b, &opts);
        let his = get_elements_by_tag_name(&b, "hi");
        assert_eq!(his.len(), 1);
        assert_eq!(get_attribute(&his[0], "rend").as_deref(), Some("#i"));
    }

    #[test]
    fn convert_tags_ul_ol_become_list_li_become_item() {
        let dom = parse("<ul><li>a</li><li>b</li></ul>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "ul").is_empty());
        assert!(get_elements_by_tag_name(&b, "li").is_empty());
        let lists = get_elements_by_tag_name(&b, "list");
        assert_eq!(lists.len(), 1);
        assert_eq!(get_attribute(&lists[0], "rend").as_deref(), Some("ul"));
        assert_eq!(get_elements_by_tag_name(&lists[0], "item").len(), 2);
    }

    #[test]
    fn convert_tags_h1_through_h6_become_head_rend_h_n() {
        let dom = parse("<div><h1>A</h1><h2>B</h2><h3>C</h3><h4>D</h4><h5>E</h5><h6>F</h6></div>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        for h in ["h1", "h2", "h3", "h4", "h5", "h6"] {
            assert!(
                get_elements_by_tag_name(&b, h).is_empty(),
                "{h} should be gone"
            );
        }
        let heads = get_elements_by_tag_name(&b, "head");
        assert_eq!(heads.len(), 6);
        let rends: Vec<String> = heads
            .iter()
            .map(|h| get_attribute(h, "rend").unwrap_or_default())
            .collect();
        assert_eq!(rends, vec!["h1", "h2", "h3", "h4", "h5", "h6"]);
    }

    #[test]
    fn convert_tags_blockquote_and_q_become_quote() {
        let dom = parse("<div><blockquote>x</blockquote><q>y</q></div>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "blockquote").is_empty());
        assert!(get_elements_by_tag_name(&b, "q").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "quote").len(), 2);
    }

    #[test]
    fn convert_tags_pre_with_single_span_becomes_code() {
        let dom = parse("<pre><span>codey</span></pre>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "pre").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "code").len(), 1);
    }

    #[test]
    fn convert_tags_pre_with_hljs_span_becomes_code_attrs_cleared() {
        let dom = parse(r#"<pre><span class="hljs-keyword">if</span> x</pre>"#);
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        let codes = get_elements_by_tag_name(&b, "code");
        assert_eq!(codes.len(), 1);
        let spans = get_elements_by_tag_name(&codes[0], "span");
        // span attributes cleared.
        for s in &spans {
            assert_eq!(get_attribute(s, "class"), None);
        }
    }

    #[test]
    fn convert_tags_plain_pre_becomes_quote() {
        // <pre> with multi-element non-span content — not code-like.
        let dom = parse("<pre>line1\nline2</pre>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "pre").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "quote").len(), 1);
    }

    #[test]
    fn convert_tags_br_hr_become_lb() {
        let dom = parse("<p>a<br>b<hr>c</p>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "br").is_empty());
        assert!(get_elements_by_tag_name(&b, "hr").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "lb").len(), 2);
    }

    #[test]
    fn convert_tags_del_s_strike_become_del_overstrike() {
        let dom = parse("<p><del>a</del><s>b</s><strike>c</strike></p>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "s").is_empty());
        assert!(get_elements_by_tag_name(&b, "strike").is_empty());
        let dels = get_elements_by_tag_name(&b, "del");
        assert_eq!(dels.len(), 3);
        for d in &dels {
            assert_eq!(get_attribute(d, "rend").as_deref(), Some("overstrike"));
        }
    }

    #[test]
    fn convert_tags_details_becomes_div_summary_becomes_head() {
        let dom = parse("<details><summary>S</summary><p>body</p></details>");
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(get_elements_by_tag_name(&b, "details").is_empty());
        assert!(get_elements_by_tag_name(&b, "summary").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "div").len(), 1);
        assert_eq!(get_elements_by_tag_name(&b, "head").len(), 1);
    }

    #[test]
    fn convert_tags_links_false_anchors_in_div_become_ref_others_stripped() {
        // Default options: links=false. <a> inside <div>/<li>/<p> -> <ref>;
        // other <a> stripped.
        let dom = parse(
            r#"<section><div><a href="/x">in-div</a></div>\
               <span><a href="/y">in-span</a></span></section>"#,
        );
        let b = body(&dom);
        convert_tags(&b, &Options::default());
        assert!(
            get_elements_by_tag_name(&b, "a").is_empty(),
            "no <a> remains"
        );
        // The one in <div> should be <ref>; the one in <span> should be stripped.
        let refs = get_elements_by_tag_name(&b, "ref");
        assert_eq!(refs.len(), 1);
        assert_eq!(crate::readability::dom::text_content(&refs[0]), "in-div");
    }

    // ---- integrated tree_cleaning + convert_tags ----

    #[test]
    fn cleaning_then_convert_produces_tei_like_tree() {
        let dom = parse(
            "<html><body>\
                <script>x</script>\
                <article><h2>Title</h2>\
                  <p>This is <b>bold</b> text.</p>\
                  <ul><li>one</li><li>two</li></ul>\
                  <footer>foot</footer>\
                </article>\
             </body></html>",
        );
        let b = body(&dom);
        let opts = Options::default();
        tree_cleaning(&b, &opts);
        convert_tags(&b, &opts);
        // No script, footer.
        assert!(get_elements_by_tag_name(&b, "script").is_empty());
        assert!(get_elements_by_tag_name(&b, "footer").is_empty());
        // <h2> -> <head rend="h2">
        assert!(get_elements_by_tag_name(&b, "h2").is_empty());
        let heads = get_elements_by_tag_name(&b, "head");
        assert_eq!(heads.len(), 1);
        // <ul>/<li> -> <list>/<item>
        let lists = get_elements_by_tag_name(&b, "list");
        assert_eq!(lists.len(), 1);
        assert_eq!(get_elements_by_tag_name(&lists[0], "item").len(), 2);
        // <b> stripped (formatting=false default), text preserved.
        assert!(get_elements_by_tag_name(&b, "b").is_empty());
        assert!(crate::readability::dom::text_content(&b).contains("bold"));
    }

    #[test]
    fn cleaning_then_convert_serializes_to_xml_for_gate() {
        // Smoke test for the Stage 0c gate: serialize_converted_tree on the
        // post-convert_tags <body> produces a deterministic XML string.
        let dom = parse("<article><p>hi <b>there</b>!</p></article>");
        let b = body(&dom);
        let opts = Options::default();
        tree_cleaning(&b, &opts);
        convert_tags(&b, &opts);
        let xml = serialize_converted_tree(&b);
        // Whatever the exact string, it should start with <body> and contain
        // the surviving paragraph content.
        assert!(xml.starts_with("<body>"));
        assert!(xml.ends_with("</body>"));
        assert!(xml.contains("there"));
    }
}
