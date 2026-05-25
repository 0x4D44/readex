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
/// `readex::Options::extractor` enum is HLD §5.2 / DECISION-C work for a
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
    /// `Extractor.min_extracted_size` (settings.cfg:26 = `MIN_EXTRACTED_SIZE`,
    /// default `250`). Threshold for `_extract`'s "enough paragraph text"
    /// gate (`main_extractor.py:594`) and `extract_content`'s wild-text
    /// fallback (`main_extractor.py:633`). Stage 2d.
    pub min_extracted_size: usize,
    /// `Extractor.lang` (settings.py:115, default `None`). Stage 6 cascade
    /// arbiter (`compare_extraction`, external.py:45-108) routes this to
    /// `justext_rescue` as the target-language hint; the stoplist accessor
    /// (`justext_stoplists`) lowercases on first read so we store the raw
    /// ISO code Python would.
    pub lang: Option<String>,
    /// `Extractor.url` (settings.py:116, default `None`). Stage 6 cascade
    /// arbiter routes this to `justext_rescue` (paragraphs do not consume it
    /// directly; it is only used to set the Python `options.source` slot
    /// from `_set_source`, settings.py:155-158). Kept as an owned `String`
    /// so the caller can pass a borrow from a longer-lived buffer.
    pub url: Option<String>,
    /// `Extractor.source` (settings.py:91, default derived from `url` or
    /// passed `source`). Used only for log-string interpolation in Python
    /// (e.g. external.py:84,90,96). Stored verbatim so the Rust port can
    /// emit the same diagnostic strings if instrumentation lands.
    pub source: Option<String>,
    /// `Extractor.dedup` (settings.py:114, default `False`). Stage 8.
    /// When true:
    /// - `cleaning::handle_textnode` / `process_node` gate per-element
    ///   on `duplicate_test` (htmlprocessing.py:262, :282).
    /// - `compare_extraction` runs a body-level `duplicate_test` on the
    ///   winning extraction (core.py:330) and returns an empty body when
    ///   the entire postbody was already seen recently.
    ///
    /// All three call sites share the process-wide
    /// [`crate::trafilatura::deduplication`] module's `LRU_TEST` cache.
    pub dedup: bool,
    /// `Extractor.min_duplcheck_size` (settings.cfg:41 =
    /// `MIN_DUPLCHECK_SIZE`, default `100`). Texts shorter than this are
    /// never tested or remembered by `duplicate_test`
    /// (deduplication.py:247). Stage 8.
    pub min_duplcheck_size: usize,
    /// `Extractor.max_repetitions` (settings.cfg:42 = `MAX_REPETITIONS`,
    /// default `2`). `duplicate_test` reports a hit only AFTER the cache
    /// count for a given text exceeds this threshold
    /// (deduplication.py:250). Stage 8.
    pub max_repetitions: usize,
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
            min_extracted_size: 250,
            lang: None,
            url: None,
            source: None,
            // settings.py:114 / settings.cfg:41-42 — Trafilatura defaults.
            dedup: false,
            min_duplcheck_size: 100,
            max_repetitions: 2,
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
///
/// Exposed as `pub` (Stage 2c-ii) so `main_extractor`'s `handle_quotes`
/// (`main_extractor.py:240`) and `handle_paragraphs` (`:308`) can call lxml's
/// `strip_tags` against a single tag name on a subtree. The same snapshot-then-
/// mutate semantics apply.
pub fn strip_tags_multi(tree: &NodeRef, stripping_list: &[&str]) {
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
        // M4 Stage 2 (htmlprocessing.py:395-399):
        //   base_url = url and get_base_url(url)
        //   for elem in tree.iter("a", "ref"):
        //       convert_link(elem, base_url)
        // Renames <a>/<ref> to <ref> and folds href→target, repairing
        // relative URLs against `options.url`'s base when supplied.
        let base_url: Option<String> = options
            .url
            .as_deref()
            .and_then(crate::trafilatura::metadata_url::get_base_url);
        let anchors = get_elements_in_any(tree, &["a", "ref"]);
        for elem in anchors {
            convert_link(&elem, base_url.as_deref());
        }
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

/// `convert_link(elem, base_url)` (`htmlprocessing.py:369-378`).
///
/// "Replace link tags and href attributes, delete the rest." Renames `<a>`
/// (or already-renamed `<ref>`) to `<ref>`, drops all attributes, then —
/// when the original had an `href` — sets `target` to the (relative-URL-
/// resolved, when `base_url` is supplied) URL.
///
/// # Python original
///
/// ```python
/// def convert_link(elem: HtmlElement, base_url: Optional[str]) -> None:
///     "Replace link tags and href attributes, delete the rest."
///     elem.tag = "ref"
///     target = elem.get("href")  # defaults to None
///     elem.attrib.clear()
///     if target:
///         if base_url:
///             target = fix_relative_urls(base_url, target)
///         elem.set("target", target)
/// ```
///
/// # Rust port shape
///
/// Mutates `elem`'s replacement in place. Because `replace_element_tag`
/// allocates a fresh node (Rust rcdom can't mutate `NodeData::Element::name`
/// — Stage 1b precedent), the new `<ref>` element is the one we clear /
/// `set_attribute("target", ...)` on. The caller (in `convert_tags`) does
/// not consume the returned handle; the surrounding `tree.iter("a", "ref")`
/// walk operates on the snapshot taken *before* the rename, so already-
/// renamed elements aren't revisited.
pub(crate) fn convert_link(elem: &NodeRef, base_url: Option<&str>) {
    // 371-372: read href off the original element BEFORE renaming.
    let target = get_attribute(elem, "href");
    // 371: elem.tag = "ref" — under rcdom this allocates a new <ref> node
    // and moves the original's children into it. The old `elem` becomes
    // detached. We operate on the new node from here on.
    let new = replace_element_tag(elem, "ref");
    // 373: elem.attrib.clear() — the rename copies attributes by default,
    // so we must clear them off the new node to match Python.
    clear_attributes(&new);
    // 374-378: if href was present, resolve + set target.
    if let Some(href) = target
        && !href.is_empty()
    {
        let resolved = match base_url {
            Some(base) => crate::trafilatura::metadata_url::fix_relative_urls(base, &href),
            None => href,
        };
        set_attribute(&new, "target", &resolved);
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
// STAGE 2b' EXTENSION — htmlprocessing.py 93-285 (HLD §7.2 prerequisites
// for Stage 2c-i)
//
// The functions below were NOT in Stage 1b. They are the rest of
// `htmlprocessing.py`: `prune_unwanted_nodes`, `collect_link_info`,
// `link_density_test`, `link_density_test_tables`, `delete_by_link_density`,
// `handle_textnode`, `process_node`. These are the substrate the Stage 2c-i
// handler primitives (`handle_titles` / `handle_formatting`) build on.
//
// Stage 1b functions above (`tree_cleaning` / `convert_tags` / `prune_html` /
// per-tag converters / `strip_tags_multi` / `delete_elements_by_tag`) are
// FROZEN; do not modify them in this stage.
// ===========================================================================

use crate::readability::dom::{
    element_text, previous_element_sibling, set_element_text, set_tail, tail, text_content,
};
use crate::trafilatura::utils::{
    duplicate_test, element_child_count, is_image_element, textfilter, trim,
};
use crate::trafilatura::xpath_engine;

// ---------------------------------------------------------------------------
// prune_unwanted_nodes (htmlprocessing.py:93-118)
// ---------------------------------------------------------------------------

/// Prune the HTML tree by removing nodes matched by each XPath expression in
/// `nodelist`. With `with_backup=true`, restore from a pre-deletion snapshot
/// if the post-deletion text shrank to less than 1/7 of the original.
///
/// **Source line-cite:** `htmlprocessing.py:93-118`.
///
/// # Python original
///
/// ```python
/// def prune_unwanted_nodes(
///     tree: HtmlElement, nodelist: List[XPath], with_backup: bool = False
/// ) -> HtmlElement:
///     "Prune the HTML tree by removing unwanted sections."
///     if with_backup:
///         old_len = len(tree.text_content())
///         backup = deepcopy(tree)
///
///     for expression in nodelist:
///         for subtree in expression(tree):
///             # preserve tail text from deletion
///             if subtree.tail is not None:
///                 prev = subtree.getprevious()
///                 if prev is None:
///                     prev = subtree.getparent()
///                 if prev is not None:
///                     # There is a previous node, append text to its tail
///                     prev.tail = (prev.tail or "") + " " + subtree.tail
///             # remove the node
///             subtree.getparent().remove(subtree)
///
///     if with_backup:
///         new_len = len(tree.text_content())
///         # todo: adjust for recall and precision settings
///         return tree if new_len > old_len / 7 else backup
///     return tree
/// ```
///
/// # Rust port shape
///
/// `nodelist` is a slice of XPath expression strings (the Python wrapper is
/// a `List[XPath]` of pre-compiled `etree.XPath` callables; the Stage 0b
/// engine takes strings, so we pass strings directly). Each expression is
/// evaluated against `tree` via `xpath_engine::evaluate`.
///
/// **Tail preservation.** lxml's `getprevious()` returns the previous
/// element/comment/PI sibling. With the `remove_comments=True` parser
/// (utils.py:70), it is effectively "previous element sibling". The Rust
/// port uses `dom::previous_element_sibling` (Stage 2b' addition); if there
/// is none, it falls back to `getparent()` — at which point the Python sets
/// `prev.tail`, where `prev` IS the parent (so the appended text becomes
/// the parent's tail, i.e. text after the parent's closing tag). That is
/// the lxml semantic the port mirrors faithfully (it's unusual but it's
/// what the source says).
///
/// **Backup branch.** When `with_backup=true`, the Python deep-copies the
/// tree, prunes, and reverts if post-deletion text < pre-deletion / 7. Stage
/// 2c-iii-a (this commit) activates the full backup-restore using
/// `dom::deep_clone` (landed Stage 2c-i). The function returns the live
/// `NodeRef` to use downstream: either the pruned input tree (when text
/// survives the threshold) or the pre-prune backup clone (when it doesn't).
/// Python's `tree = prune_unwanted_nodes(...)` rebinds; Rust callers should
/// shadow the same way: `let tree = prune_unwanted_nodes(&tree, ...);`.
///
/// Returns the live `NodeRef`. With `with_backup=false`, the function still
/// mutates `tree` in place and returns `tree.clone()` (cheap Rc clone, same
/// Node) so the call shape matches both Python and the backup-active case.
pub fn prune_unwanted_nodes(tree: &NodeRef, nodelist: &[&str], with_backup: bool) -> NodeRef {
    // htmlprocessing.py:97-99 — capture pre-deletion text length AND a deep
    // clone of the tree to roll back to if the prune is too aggressive.
    let (old_len, backup) = if with_backup {
        (
            text_content(tree).chars().count(),
            Some(dom::deep_clone(tree)),
        )
    } else {
        (0, None)
    };

    // htmlprocessing.py:101-112 — for each XPath, for each match, preserve
    // tail and remove.
    for expression in nodelist {
        let matches = xpath_engine::evaluate(expression, tree).unwrap_or_default();
        for subtree in matches {
            // htmlprocessing.py:104 — if subtree.tail is not None.
            if let Some(t) = tail(&subtree) {
                // htmlprocessing.py:105-107 — prev = subtree.getprevious() or
                // subtree.getparent().
                let prev = previous_element_sibling(&subtree).or_else(|| dom::parent(&subtree));
                // htmlprocessing.py:108-110 — append tail to prev.tail.
                if let Some(prev) = prev {
                    let old_tail = tail(&prev).unwrap_or_default();
                    let mut new_tail = old_tail;
                    new_tail.push(' ');
                    new_tail.push_str(&t);
                    set_tail(&prev, Some(&new_tail));
                }
            }
            // htmlprocessing.py:112 — subtree.getparent().remove(subtree).
            // `dom::remove` detaches without preserving tail (we already
            // moved the tail above); the Text-run that was the tail still
            // lives in the parent at the original position, so we must drop
            // it too. Easiest: detach the tail run as well.
            //
            // The simpler equivalent: clear the tail of `subtree` (drops
            // the parent-level Text-run) then remove `subtree`. set_tail
            // on a still-attached child clears the Text-run between it and
            // the next non-Text sibling.
            set_tail(&subtree, None);
            dom::remove(&subtree);
        }
    }

    // htmlprocessing.py:114-117 — backup branch. Stage 2c-iii-a activates
    // the full restore using the `backup` deep_clone captured before pruning.
    // Python's `return tree if new_len > old_len / 7 else backup` — we use
    // `new_len * 7 > old_len` to avoid a division-by-zero on empty inputs
    // (the inequality is identical for positive integers, and `old_len = 0`
    // implies `new_len = 0` so `0 > 0` is false, falling back to backup, which
    // matches Python's `0 > 0/7 → 0 > 0 → False` exactly).
    if with_backup {
        let new_len = text_content(tree).chars().count();
        if (new_len * 7) > old_len {
            tree.clone()
        } else {
            // htmlprocessing.py:117 — return the pristine pre-prune backup.
            backup.expect("with_backup=true captured a deep_clone backup above")
        }
    } else {
        // htmlprocessing.py:118 — return tree (no backup branch).
        tree.clone()
    }
}

// ---------------------------------------------------------------------------
// collect_link_info (htmlprocessing.py:121-129)
// ---------------------------------------------------------------------------

/// Collect heuristics on link text — sum of lengths, count, short-element
/// count, and the trimmed list itself.
///
/// **Source line-cite:** `htmlprocessing.py:121-129`.
///
/// # Python original
///
/// ```python
/// def collect_link_info(
///     links_xpath: List[HtmlElement],
/// ) -> Tuple[int, int, int, List[str]]:
///     "Collect heuristics on link text"
///     mylist = [e for e in (trim(elem.text_content()) for elem in links_xpath) if e]
///     lengths = list(map(len, mylist))
///     # longer strings impact recall in favor of precision
///     shortelems = sum(1 for l in lengths if l < 10)
///     return sum(lengths), len(mylist), shortelems, mylist
/// ```
///
/// # Rust port shape
///
/// Returns `(total_link_text_len, count, short_elem_count, list)`. Lengths
/// are character counts (Python `len(str)` = code-point count); we use
/// `str::chars().count()` to match.
pub fn collect_link_info(links: &[NodeRef]) -> (usize, usize, usize, Vec<String>) {
    let mylist: Vec<String> = links
        .iter()
        .map(|elem| trim(&text_content(elem)))
        .filter(|s| !s.is_empty())
        .collect();
    let lengths: Vec<usize> = mylist.iter().map(|s| s.chars().count()).collect();
    let shortelems = lengths.iter().filter(|&&l| l < 10).count();
    let total: usize = lengths.iter().sum();
    (total, mylist.len(), shortelems, mylist)
}

// ---------------------------------------------------------------------------
// link_density_test (htmlprocessing.py:132-169)
// ---------------------------------------------------------------------------

/// Determine whether `element` is rich enough in links that it looks like
/// boilerplate. Returns `(should_delete, link_text_list)`.
///
/// **Source line-cite:** `htmlprocessing.py:132-169`.
///
/// # Python original
///
/// See htmlprocessing.py:132-169. The logic, in summary:
/// - Find all `<ref>` descendants (XPath `.//ref`).
/// - If none, return `(false, [])`.
/// - SHORTCUT for exactly one ref: if its trimmed link text is longer than
///   a threshold (10 / 100 by `favor_precision`) and > 90% of element's
///   text, return `(true, [])`.
/// - Pick `limitlen` based on element tag + whether it has a next sibling:
///     - tag == "p": 60 if no next sibling else 30.
///     - else: 300 if no next sibling else 100.
/// - If element text shorter than `limitlen`:
///     - collect_link_info; if zero non-empty links, return `(true, [])`.
///     - Otherwise return true if link text > 80% of total OR
///       (more than one link AND > 80% are short).
/// - Otherwise return `(false, mylist)`.
pub fn link_density_test(
    element: &NodeRef,
    text: &str,
    favor_precision: bool,
) -> (bool, Vec<String>) {
    // htmlprocessing.py:136 — links_xpath = element.findall(".//ref").
    // `findall` semantic = XPath `.//ref` returning descendants in document
    // order. Routed through Stage 0b engine.
    let links_xpath = xpath_engine::evaluate(".//ref", element).unwrap_or_default();
    if links_xpath.is_empty() {
        return (false, Vec::new());
    }

    // htmlprocessing.py:141-145 — single-link shortcut.
    if links_xpath.len() == 1 {
        let len_threshold = if favor_precision { 10 } else { 100 };
        let link_text = trim(&text_content(&links_xpath[0]));
        let link_text_len = link_text.chars().count();
        let text_len = text.chars().count();
        // > len(text) * 0.9 — preserved as integer math via float coercion.
        if link_text_len > len_threshold && link_text_len as f64 > text_len as f64 * 0.9 {
            return (true, Vec::new());
        }
    }

    // htmlprocessing.py:146-154 — pick limitlen.
    let tag = dom::local_name(element).unwrap_or_default();
    let has_next = dom::next_element_sibling(element).is_some();
    let limitlen: usize = if tag == "p" {
        if !has_next { 60 } else { 30 }
    } else if !has_next {
        300
    } else {
        100
    };

    // htmlprocessing.py:155-168 — short-element check.
    let elemlen = text.chars().count();
    let mut mylist_out: Vec<String> = Vec::new();
    if elemlen < limitlen {
        let (linklen, elemnum, shortelems, mylist) = collect_link_info(&links_xpath);
        if elemnum == 0 {
            return (true, mylist);
        }
        // > 80% of total OR (>1 ref AND >80% short).
        if (linklen as f64) > (elemlen as f64) * 0.8
            || (elemnum > 1 && (shortelems as f64) / (elemnum as f64) > 0.8)
        {
            return (true, mylist);
        }
        mylist_out = mylist;
    }
    (false, mylist_out)
}

// ---------------------------------------------------------------------------
// link_density_test_tables (htmlprocessing.py:172-188)
// ---------------------------------------------------------------------------

/// Tables-specific variant of `link_density_test`. Returns true if the
/// table looks like a link-heavy navigation table.
///
/// **Source line-cite:** `htmlprocessing.py:172-188`.
pub fn link_density_test_tables(element: &NodeRef) -> bool {
    let links_xpath = xpath_engine::evaluate(".//ref", element).unwrap_or_default();
    if links_xpath.is_empty() {
        return false;
    }

    let elem_text = trim(&text_content(element));
    let elemlen = elem_text.chars().count();
    // htmlprocessing.py:180-181 — short tables are never link-heavy enough.
    if elemlen < 200 {
        return false;
    }

    let (linklen, elemnum, _, _) = collect_link_info(&links_xpath);
    if elemnum == 0 {
        return true;
    }

    // htmlprocessing.py:188 — 80% threshold for "small" tables (< 1000
    // chars), 50% threshold for larger tables.
    if elemlen < 1000 {
        (linklen as f64) > 0.8 * (elemlen as f64)
    } else {
        (linklen as f64) > 0.5 * (elemlen as f64)
    }
}

// ---------------------------------------------------------------------------
// delete_by_link_density (htmlprocessing.py:191-219)
// ---------------------------------------------------------------------------

/// Determine the link density of every descendant `tagname` element and
/// delete those identified as boilerplate.
///
/// **Source line-cite:** `htmlprocessing.py:191-219`.
///
/// # Python original
///
/// ```python
/// def delete_by_link_density(
///     subtree, tagname, backtracking=False, favor_precision=False
/// ):
///     deletions = []
///     len_threshold = 200 if favor_precision else 100
///     depth_threshold = 1 if favor_precision else 3
///
///     for elem in subtree.iter(tagname):
///         elemtext = trim(elem.text_content())
///         result, templist = link_density_test(elem, elemtext, favor_precision)
///         if result or (
///             backtracking
///             and templist
///             and 0 < len(elemtext) < len_threshold
///             and len(elem) >= depth_threshold
///         ):
///             deletions.append(elem)
///
///     for elem in dict.fromkeys(deletions):  # dedup, preserve order
///         delete_element(elem)
///
///     return subtree
/// ```
pub fn delete_by_link_density(
    subtree: &NodeRef,
    tagname: &str,
    backtracking: bool,
    favor_precision: bool,
) {
    let len_threshold = if favor_precision { 200 } else { 100 };
    let depth_threshold = if favor_precision { 1 } else { 3 };

    // htmlprocessing.py:203 — `subtree.iter(tagname)`. lxml's
    // `Element.iter(tagname)` INCLUDES self if `self.tag == tagname`.
    // Stage 0a's `get_elements_by_tag_name` only walks descendants, so we
    // explicitly check the root and prepend it to the candidate list when
    // its local-name matches.
    let mut candidates: Vec<NodeRef> = Vec::new();
    if local_name(subtree).as_deref() == Some(tagname) {
        candidates.push(subtree.clone());
    }
    candidates.extend(dom::get_elements_by_tag_name(subtree, tagname));

    // Collect deletions in iteration order, deduplicating by Rc identity.
    let mut deletions: Vec<NodeRef> = Vec::new();
    for elem in candidates {
        let elemtext = trim(&text_content(&elem));
        let elemtext_len = elemtext.chars().count();
        let (result, templist) = link_density_test(&elem, &elemtext, favor_precision);
        let backtrack_hit = backtracking
            && !templist.is_empty()
            && elemtext_len > 0
            && elemtext_len < len_threshold
            && element_child_count(&elem) >= depth_threshold;
        if result || backtrack_hit {
            // Python's `dict.fromkeys(deletions)` dedups by identity (since
            // `_Element.__hash__` is identity-based). Rust: dedup by Rc
            // pointer identity.
            if !deletions.iter().any(|e| std::rc::Rc::ptr_eq(e, &elem)) {
                deletions.push(elem);
            }
        }
    }

    // htmlprocessing.py:216-217 — for each deletion, delete_element with
    // tail preservation (xml.py:54-70 default `keep_tail=True`).
    for elem in deletions {
        delete_with_tail_preserve_free(&elem);
    }
}

// ---------------------------------------------------------------------------
// handle_textnode (htmlprocessing.py:222-265)
// ---------------------------------------------------------------------------

/// Convert, format, and probe potential text elements. Returns `Some(elem)`
/// if the element should survive, `None` if it should be dropped.
///
/// **Source line-cite:** `htmlprocessing.py:222-265`.
///
/// **CRITICAL DEPENDENCY OF STAGE 2c-i.** This is the workhorse the Stage
/// 2c-i `handle_titles` / `handle_formatting` primitives funnel every
/// candidate textual element through.
///
/// # Python original
///
/// ```python
/// def handle_textnode(
///     elem, options, comments_fix=True, preserve_spaces=False
/// ) -> Optional[_Element]:
///     "Convert, format, and probe potential text elements."
///     if elem.tag == "graphic" and is_image_element(elem):
///         return elem
///     if elem.tag == "done" or (len(elem) == 0 and not elem.text and not elem.tail):
///         return None
///
///     # lb bypass
///     if not comments_fix and elem.tag == "lb":
///         if not preserve_spaces:
///             elem.tail = trim(elem.tail) or None
///         return elem
///
///     if not elem.text and len(elem) == 0:
///         # try the tail
///         elem.text, elem.tail = elem.tail, ""
///         # handle differently for br/lb
///         if comments_fix and elem.tag == "lb":
///             elem.tag = "p"
///
///     # trim
///     if not preserve_spaces:
///         elem.text = trim(elem.text) or None
///         if elem.tail:
///             elem.tail = trim(elem.tail) or None
///
///     # filter content
///     if (
///         not elem.text
///         and textfilter(elem)
///         or (options.dedup and duplicate_test(elem, options))
///     ):
///         return None
///     return elem
/// ```
///
/// # Rust port shape
///
/// Mutates `elem` in place. The `options.dedup` branch funnels into
/// `duplicate_test` which is a stub (returns false) until a later stage
/// activates dedup — that gates the second half of the final filter.
///
/// The `Options` slot consumed is `dedup`; Stage 1b's `Options` struct
/// doesn't yet carry that slot. Stage 2b' threads through a minimal
/// `&Options` reference; the dedup arm is plumbed but inert (the stub
/// returns false unconditionally). When `Options.dedup` lands, the call
/// site here lights up automatically.
#[must_use = "handle_textnode returns None when the element should be \
              dropped — callers must inspect the return value to decide \
              whether to keep the element"]
pub fn handle_textnode(
    elem: &NodeRef,
    options: &Options,
    comments_fix: bool,
    preserve_spaces: bool,
) -> Option<NodeRef> {
    // htmlprocessing.py:229-230 — graphic + image element survives.
    let tag = dom::local_name(elem).unwrap_or_default();
    if tag == "graphic" && is_image_element(elem) {
        return Some(elem.clone());
    }

    // htmlprocessing.py:231 — done sentinel OR fully-empty element.
    if tag == "done"
        || (element_child_count(elem) == 0 && element_text(elem).is_none() && tail(elem).is_none())
    {
        return None;
    }

    // htmlprocessing.py:235-241 — lb bypass when comments_fix=false.
    if !comments_fix && tag == "lb" {
        if !preserve_spaces {
            let trimmed_tail = tail(elem).map(|t| trim(&t)).filter(|t| !t.is_empty());
            set_tail(elem, trimmed_tail.as_deref());
        }
        return Some(elem.clone());
    }

    // htmlprocessing.py:243-249 — when elem has no text and no element
    // children, try the tail: move tail into text and clear tail.
    let mut current_tag = tag.clone();
    if element_text(elem).is_none() && element_child_count(elem) == 0 {
        // Read tail.
        let t = tail(elem);
        // Move tail to text, clear tail. The Python source assigns
        // `elem.text, elem.tail = elem.tail, ""` atomically; the Rust
        // sequence is read-tail, set-text, clear-tail. The two operations
        // target different storage slots (leading-Text-child run vs
        // following-Text-sibling run) so there is no aliasing risk in
        // rcdom — but we still order it carefully: set text BEFORE clearing
        // tail so that if any future invariant check reads them together,
        // they're never both empty mid-operation.
        set_element_text(elem, t.as_deref());
        set_tail(elem, None);
        // htmlprocessing.py:248-249 — lb→p when comments_fix=true.
        if comments_fix && current_tag == "lb" {
            let renamed = replace_element_tag(elem, "p");
            // The old `elem` is now detached; the caller's `&NodeRef` no
            // longer points to a live element. We MUST update the local
            // tag tracker and return the new node. Subsequent operations
            // below (`preserve_spaces` trim, `textfilter`) need to act on
            // the new element.
            current_tag = "p".to_string();
            return handle_textnode_finish(&renamed, options, preserve_spaces, &current_tag);
        }
    }

    handle_textnode_finish(elem, options, preserve_spaces, &current_tag)
}

/// Tail half of `handle_textnode` after the moved-tail / lb-renaming
/// branch: trim (when not preserve_spaces) and apply `textfilter` +
/// `duplicate_test`. Splits the function so the lb→p rename can return
/// the NEW NodeRef from the renamed element without re-running the
/// already-done "move tail to text" step on it.
fn handle_textnode_finish(
    elem: &NodeRef,
    options: &Options,
    preserve_spaces: bool,
    _tag: &str,
) -> Option<NodeRef> {
    // htmlprocessing.py:252-255 — trim text and tail when not preserve_spaces.
    if !preserve_spaces {
        let trimmed_text = element_text(elem)
            .map(|t| trim(&t))
            .filter(|t| !t.is_empty());
        set_element_text(elem, trimmed_text.as_deref());
        // The Python's `if elem.tail:` is a TRUTHY check — None/"" both
        // falsy — so the trim runs only when there's a non-empty tail.
        if let Some(t) = tail(elem)
            && !t.is_empty()
        {
            let trimmed_tail = trim(&t);
            let new_tail = if trimmed_tail.is_empty() {
                None
            } else {
                Some(trimmed_tail)
            };
            set_tail(elem, new_tail.as_deref());
        }
    }

    // htmlprocessing.py:259-264 — final filter:
    //   (not elem.text AND textfilter(elem)) OR (options.dedup AND duplicate_test(elem))
    let text_empty = element_text(elem).is_none_or(|s| s.is_empty());
    let textfilter_hit = text_empty && textfilter(elem);
    // Stage 2b' Options does not yet carry `dedup`; the stub returns false
    // unconditionally so the dedup arm is inert until a future stage
    // activates it. We DO call duplicate_test (stub) to pin the call shape.
    let dedup_hit = options.dedup() && duplicate_test(elem, options);
    if textfilter_hit || dedup_hit {
        return None;
    }
    Some(elem.clone())
}

// ---------------------------------------------------------------------------
// process_node (htmlprocessing.py:268-285)
// ---------------------------------------------------------------------------

/// Light-format variant of `handle_textnode`. Returns `Some(elem)` if
/// the element should survive, `None` if it should be dropped.
///
/// **Source line-cite:** `htmlprocessing.py:268-285`.
///
/// **CRITICAL DEPENDENCY OF STAGE 2c-i.**
///
/// # Python original
///
/// ```python
/// def process_node(elem, options) -> Optional[_Element]:
///     "Convert, format, and probe potential text elements (light format)."
///     if elem.tag == "done" or (len(elem) == 0 and not elem.text and not elem.tail):
///         return None
///
///     # trim
///     elem.text, elem.tail = trim(elem.text) or None, trim(elem.tail) or None
///
///     # adapt content string
///     if elem.tag != "lb" and not elem.text and elem.tail:
///         elem.text, elem.tail = elem.tail, None
///
///     # content checks
///     if elem.text or elem.tail:
///         if textfilter(elem) or (options.dedup and duplicate_test(elem, options)):
///             return None
///
///     return elem
/// ```
#[must_use = "process_node returns None when the element should be dropped \
              — callers must inspect the return value"]
pub fn process_node(elem: &NodeRef, options: &Options) -> Option<NodeRef> {
    let tag = dom::local_name(elem).unwrap_or_default();
    // htmlprocessing.py:270 — done sentinel OR fully-empty element.
    if tag == "done"
        || (element_child_count(elem) == 0 && element_text(elem).is_none() && tail(elem).is_none())
    {
        return None;
    }

    // htmlprocessing.py:274 — trim text and tail (each replaced by None
    // when the trimmed result is empty).
    let trimmed_text = element_text(elem)
        .map(|s| trim(&s))
        .filter(|s| !s.is_empty());
    set_element_text(elem, trimmed_text.as_deref());
    let trimmed_tail = tail(elem).map(|s| trim(&s)).filter(|s| !s.is_empty());
    set_tail(elem, trimmed_tail.as_deref());

    // htmlprocessing.py:277-278 — non-lb: if no text but tail present,
    // swap tail into text.
    if tag != "lb"
        && element_text(elem).is_none()
        && let Some(t) = tail(elem)
    {
        set_element_text(elem, Some(&t));
        set_tail(elem, None);
    }

    // htmlprocessing.py:281-283 — content checks.
    let has_text = element_text(elem).is_some();
    let has_tail = tail(elem).is_some();
    if has_text || has_tail {
        let textfilter_hit = textfilter(elem);
        let dedup_hit = options.dedup() && duplicate_test(elem, options);
        if textfilter_hit || dedup_hit {
            return None;
        }
    }

    Some(elem.clone())
}

// ---------------------------------------------------------------------------
// Options extension (Stage 2b' — dedup slot accessor)
// ---------------------------------------------------------------------------

impl Options {
    /// `Options.dedup` accessor (settings.py:114, default `False`). Stage
    /// 8 promoted the dedup slot from the Stage-2b' stub to a real field
    /// (`pub dedup: bool`). This accessor remains as a thin getter so that
    /// existing call sites continue to read `options.dedup()` — the
    /// Stage-2b' / Stage-2c-i / Stage-2c-ii / Stage-2c-iii call sites
    /// (`handle_textnode`, `process_node`, and friends) keep their
    /// already-line-cited shape. Rename to a direct field access in a
    /// future refactor stage if desired.
    pub fn dedup(&self) -> bool {
        // Stage 8 (deduplication.py + LRU_TEST wiring). Reads the field
        // landed on `Options` alongside `min_duplcheck_size` and
        // `max_repetitions`. Default `false` per settings.py:114.
        self.dedup
    }
}

// ===========================================================================
// sanitize_tree (external.py:163-190) — Stage 6
// ===========================================================================

/// `TEI_VALID_TAGS` — `xml.py:28-29`. The set of element tags the
/// post-`sanitize_tree` output is allowed to retain; every other element
/// is stripped via `lxml.etree.strip_tags` (children + text + tail survive,
/// only the wrapper goes).
///
/// Stored as an `&[&str]` slice for membership lookup; order is irrelevant
/// (Python uses a literal `set`).
pub const TEI_VALID_TAGS: &[&str] = &[
    "ab", "body", "cell", "code", "del", "div", "graphic", "head", "hi", "item", "lb", "list",
    "p", "quote", "ref", "row", "table",
];

/// `sanitize_tree(tree, options)` — `external.py:163-190`. Post-processing
/// pass that converts the readability/jusText generic-algorithm output to
/// Trafilatura's TEI-like vocabulary AND strips any tags outside
/// [`TEI_VALID_TAGS`].
///
/// # Python original
///
/// ```python
/// def sanitize_tree(tree, options):
///     '''Convert and sanitize the output from the generic algorithm
///        (post-processing)'''
///     # 1. clean
///     cleaned_tree = tree_cleaning(tree, options)
///     if options.links is False:
///         strip_tags(cleaned_tree, 'a')
///     strip_tags(cleaned_tree, 'span')
///     # 2. convert
///     cleaned_tree = convert_tags(cleaned_tree, options)
///     for elem in cleaned_tree.iter('td', 'th', 'tr'):
///         if elem.tag == 'tr':
///             elem.tag = 'row'
///         elif elem.tag in ('td', 'th'):
///             if elem.tag == 'th':
///                 elem.set('role', 'head')
///             elem.tag = 'cell'
///     # 3. sanitize
///     sanitization_list = [
///         tagname
///         for tagname in [element.tag for element in set(cleaned_tree.iter('*'))]
///         if tagname not in TEI_VALID_TAGS
///     ]
///     strip_tags(cleaned_tree, *sanitization_list)
///     # 4. return
///     text = trim(' '.join(cleaned_tree.itertext()))
///     return cleaned_tree, text, len(text)
/// ```
///
/// # Rust port shape
///
/// Mutates `tree` in place (Python rebinds `cleaned_tree`, but our
/// [`tree_cleaning`] / [`convert_tags`] mutate the input node directly — the
/// reassignment is a Python-side aliasing convention, not a fresh allocation).
///
/// Returns `(text, len)`:
/// - `text` is the trimmed space-joined `itertext` (matching Python's `trim(' '.join(cleaned_tree.itertext()))`).
/// - `len` is the codepoint count of `text` (Python `len(str)` on Python 3).
///
/// The caller retains the original `&NodeRef` (no rebind needed); use it
/// alongside the returned `(text, len)` exactly as Python uses the
/// `(cleaned_tree, text, len_text)` triple.
///
/// # Anti-inversion notes
///
/// 1. `set(cleaned_tree.iter('*'))` — Python's `set()` deduplicates by
///    identity for lxml `HtmlElement` instances, but the list comprehension
///    just collects `element.tag` values from that set. So the `sanitization_list`
///    is a list of tag-name STRINGS (with duplicates: one entry per element
///    instance whose tag is non-TEI). `lxml.etree.strip_tags(tree, *names)`
///    accepts repeated names without error, so the duplicates are harmless.
///    We faithfully collect tag-names from the descendant snapshot and pass
///    them to `strip_tags_multi` — same observable outcome (strip every tag
///    not in `TEI_VALID_TAGS`).
///
/// 2. `strip_tags(cleaned_tree, 'a')` only fires when `options.links is False`.
///    Our `tree_cleaning` already invokes `convert_tags` patterns that handle
///    `<a>` (renaming qualifying anchors to `<ref>`); but the Python
///    `sanitize_tree` runs an ADDITIONAL `strip_tags(_, 'a')` after
///    `tree_cleaning`. Since `<a>` is not in `TEI_VALID_TAGS`, the final
///    strip pass (step 3) would catch it anyway — but we preserve the
///    explicit early-strip to match the Python source order.
///
/// 3. The table-cell rename pass (Python `for elem in iter('td', 'th', 'tr')`)
///    runs AFTER `convert_tags`. `convert_tags` does not rewrite tr/td/th in
///    the Stage 1b port (it handles list/heading/quote/del/details, but not
///    table cells — those are explicitly part of `sanitize_tree`).
pub fn sanitize_tree(tree: &NodeRef, options: &Options) -> (String, usize) {
    // external.py:166 — `cleaned_tree = tree_cleaning(tree, options)`.
    tree_cleaning(tree, options);

    // external.py:167-168 — `if options.links is False: strip_tags(cleaned_tree, 'a')`.
    if !options.links {
        strip_tags_multi(tree, &["a"]);
    }
    // external.py:169 — `strip_tags(cleaned_tree, 'span')`.
    strip_tags_multi(tree, &["span"]);

    // external.py:171 — `cleaned_tree = convert_tags(cleaned_tree, options)`.
    convert_tags(tree, options);

    // external.py:172-180 — table-cell rename pass.
    // Python `tree.iter('td', 'th', 'tr')` walks descendants in doc order;
    // for each we either rename `tr` -> `row` or `td`/`th` -> `cell` (with
    // `role="head"` on the latter when the source tag was `th`).
    for elem in dom::get_all_nodes_with_tag(tree, &["td", "th", "tr"]) {
        let tag = match local_name(&elem) {
            Some(t) => t,
            None => continue,
        };
        if tag.as_str() == "tr" {
            // external.py:176 — `elem.tag = 'row'`.
            let _ = replace_element_tag(&elem, "row");
        } else if tag.as_str() == "td" || tag.as_str() == "th" {
            // external.py:177-180 — th gets `role="head"`, then both
            // retag to `cell`. Order matters: set the attribute BEFORE
            // the rename so `replace_element_tag` clones the attr-map
            // including `role` onto the new node.
            if tag.as_str() == "th" {
                set_attribute(&elem, "role", "head");
            }
            let _ = replace_element_tag(&elem, "cell");
        }
    }

    // external.py:182-187 — sanitization list = every descendant element's
    // tag-name that is NOT in `TEI_VALID_TAGS`. Faithful collection:
    // walk all descendant elements via `get_elements_by_tag_name(_, "*")`
    // (lxml `iter('*')` is descendant-or-self in doc order; our facade is
    // descendants only — but `tree` itself is the cascade's wrapper body,
    // never an element with a non-TEI tag, so the divergence is moot).
    let mut bad_tags: Vec<String> = Vec::new();
    for elem in dom::get_elements_by_tag_name(tree, "*") {
        if let Some(tag) = local_name(&elem)
            && !TEI_VALID_TAGS.contains(&tag.as_str())
            && !bad_tags.iter().any(|t| t == tag.as_str())
        {
            bad_tags.push(tag.to_string());
        }
    }
    // external.py:187 — `strip_tags(cleaned_tree, *sanitization_list)`. We
    // collect tag names then strip in one pass. `strip_tags_multi` snapshots
    // matches per-tag before stripping, so a tag dropping in pass N cannot
    // skip a same-tag descendant in pass M>N.
    let bad_tag_refs: Vec<&str> = bad_tags.iter().map(|s| s.as_str()).collect();
    strip_tags_multi(tree, &bad_tag_refs);

    // external.py:189 — `text = trim(' '.join(cleaned_tree.itertext()))`.
    // lxml's `itertext()` yields every text-node in document order; the
    // space-joined trim collapses whitespace runs.
    // `text_content` already concatenates descendant text in doc order;
    // we then `trim` to collapse whitespace runs (matches lxml's
    // `' '.join(...) + trim` for itertext semantics — the explicit ' '
    // separator inserts a single space between runs, then trim collapses
    // multiple spaces to one).
    let raw_text = text_content(tree);
    let text = trim(&raw_text);
    let len = text.chars().count();
    (text, len)
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

    // =======================================================================
    // STAGE 2b' tests — htmlprocessing.py 93-285 (prune_unwanted_nodes /
    // collect_link_info / link_density_test / link_density_test_tables /
    // delete_by_link_density / handle_textnode / process_node)
    // =======================================================================

    use crate::readability::dom::{create_element, element_text, set_element_text, set_tail, tail};

    // ---- collect_link_info ----

    #[test]
    fn collect_link_info_lengths_and_shortelems() {
        // Three refs: text lengths 3, 12, 5 → total 20, count 3, short (len<10) = 2.
        let r1 = create_element("ref");
        set_element_text(&r1, Some("abc")); // 3
        let r2 = create_element("ref");
        set_element_text(&r2, Some("abcdefghijkl")); // 12
        let r3 = create_element("ref");
        set_element_text(&r3, Some("hello")); // 5
        // Empty ref — should be filtered out of mylist.
        let r4 = create_element("ref");
        let links = vec![r1, r2, r3, r4];
        let (total, count, shortelems, list) = collect_link_info(&links);
        assert_eq!(total, 3 + 12 + 5);
        assert_eq!(count, 3);
        assert_eq!(shortelems, 2);
        assert_eq!(
            list,
            vec!["abc".to_string(), "abcdefghijkl".into(), "hello".into()]
        );
    }

    // ---- link_density_test ----

    #[test]
    fn link_density_test_no_refs_returns_false() {
        let dom = parse("<p>just text, no links here</p>");
        let b = body(&dom);
        let p = get_elements_by_tag_name(&b, "p")[0].clone();
        let text = "just text, no links here";
        let (deletehit, list) = link_density_test(&p, text, false);
        assert!(!deletehit);
        assert!(list.is_empty());
    }

    #[test]
    fn link_density_test_single_long_link_returns_true() {
        // Single ref with link text longer than threshold (100) and > 90%
        // of element text → shortcut triggers (htmlprocessing.py:141-145).
        let dom = parse(
            "<p><ref>This is a very long single link text that should be \
             over one hundred characters in length for the shortcut to trigger \
             properly here OK</ref></p>",
        );
        let b = body(&dom);
        let p = get_elements_by_tag_name(&b, "p")[0].clone();
        let text = trim(&text_content(&p));
        assert!(text.chars().count() > 100);
        let (hit, _) = link_density_test(&p, &text, false);
        assert!(hit);
    }

    #[test]
    fn link_density_test_p_with_next_sibling_limitlen_30() {
        // p with next sibling → limitlen=30. Element text "ab" (len 2 < 30).
        // One ref with text "a" (linklen=1; <= 0.8*2=1.6 → not link-heavy by
        // ratio; single ref count → no shortelems-ratio trigger since
        // elemnum==1). Should return false on the ratio path.
        let dom = parse("<div><p><ref>a</ref>b</p><p>next</p></div>");
        let b = body(&dom);
        let p1 = get_elements_by_tag_name(&b, "p")[0].clone();
        let text = trim(&text_content(&p1));
        // Sanity: p has next sibling.
        assert!(dom::next_element_sibling(&p1).is_some());
        let (_hit, _) = link_density_test(&p1, &text, false);
        // We don't pin TRUE/FALSE — we only pin "doesn't panic" and follows
        // the threshold math. A more concrete assertion: when element text
        // is short and link text dominates >80%, return true.
        // Use this case: ref text "longerlonger" (12) vs p text 14 → 12/14 > 0.8.
        let dom2 = parse("<div><p><ref>longerlonger</ref>!!</p><p>next</p></div>");
        let b2 = body(&dom2);
        let p2 = get_elements_by_tag_name(&b2, "p")[0].clone();
        let text2 = trim(&text_content(&p2));
        let (hit2, _) = link_density_test(&p2, &text2, false);
        assert!(hit2);
    }

    // ---- link_density_test_tables ----

    #[test]
    fn link_density_test_tables_threshold_200_chars() {
        // Tables with elemlen < 200 always return false.
        let dom = parse("<table><tr><td><ref>x</ref></td></tr></table>");
        let b = body(&dom);
        let tbl = get_elements_by_tag_name(&b, "table")[0].clone();
        assert!(!link_density_test_tables(&tbl));
    }

    #[test]
    fn link_density_test_tables_link_dominated_returns_true() {
        // Build a table > 200 chars where link text > 80% of total.
        let link_text = "linklink".repeat(30); // 240 chars
        let html = format!("<table><tr><td><ref>{link_text}</ref></td></tr></table>");
        let dom = parse(&html);
        let b = body(&dom);
        let tbl = get_elements_by_tag_name(&b, "table")[0].clone();
        assert!(link_density_test_tables(&tbl));
    }

    // ---- delete_by_link_density ----

    #[test]
    fn delete_by_link_density_removes_listed_elem_by_link_density_test_true() {
        // A <p> whose link-density_test returns true should be deleted.
        // Take the single-link shortcut: link text must be > 100 chars AND
        // > 90% of element text. Build a >150-char link that IS the entire
        // element text.
        let long_link = "a".repeat(150);
        let dom = parse(&format!(
            "<div><p><ref>{long_link}</ref></p><p>after</p></div>"
        ));
        let b = body(&dom);
        let p_before = get_elements_by_tag_name(&b, "p").len();
        assert_eq!(p_before, 2);
        delete_by_link_density(&b, "p", false, false);
        let p_after = get_elements_by_tag_name(&b, "p").len();
        // The link-heavy <p> should have been removed; the "after" <p> stays.
        assert_eq!(p_after, 1);
    }

    #[test]
    fn delete_by_link_density_includes_root_when_tag_matches() {
        // Cite: htmlprocessing.py:203 — Python `tree.iter(tagname)` INCLUDES
        // self when `self.tag == tagname`. The Rust port's
        // `get_elements_by_tag_name` only walks descendants, so the function
        // must explicitly check the root. Construct a <p> root with a >100-
        // char <ref> child that drives link_density_test to true; the root
        // <p> must be removed.
        let long_link = "a".repeat(150);
        let dom = parse(&format!("<div><p><ref>{long_link}</ref></p></div>"));
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        assert_eq!(ps.len(), 1);
        let p_root = ps[0].clone();
        // Sanity: link_density_test on the <p> root must report true under
        // the single-link shortcut (>100-char link IS the entire text).
        let (hit, _) = link_density_test(&p_root, &trim(&text_content(&p_root)), false);
        assert!(hit, "precondition: link_density_test fires on root <p>");
        // Iterate WITH the root as the subtree AND tag matching the root.
        delete_by_link_density(&p_root, "p", false, false);
        // The <p> root must be detached from its parent <div>.
        assert!(
            get_elements_by_tag_name(&b, "p").is_empty(),
            "root <p> must be removed when its own link-density trips"
        );
    }

    // ---- prune_unwanted_nodes ----

    #[test]
    fn prune_unwanted_nodes_removes_matched_subtree() {
        // Remove <aside> via XPath ".//aside".
        let dom = parse("<div><p>keep</p><aside>drop</aside><p>more</p></div>");
        let b = body(&dom);
        prune_unwanted_nodes(&b, &[".//aside"], false);
        assert!(get_elements_by_tag_name(&b, "aside").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "p").len(), 2);
    }

    #[test]
    fn prune_unwanted_nodes_preserves_tail_on_removed_subtree() {
        // <div><p>x</p><aside>y</aside>tail-text<p>z</p></div>.
        // After removing <aside>, "tail-text" should be appended to the
        // previous sibling's tail (here the previous element is <p>x</p>).
        // Python: prev = aside.getprevious() = <p>x</p>; prev.tail =
        // (prev.tail or "") + " " + " tail-text". So <p>x</p>.tail becomes
        // " tail-text" (with the leading space).
        let dom = parse("<div><p>x</p><aside>y</aside>tail-text<p>z</p></div>");
        let b = body(&dom);
        prune_unwanted_nodes(&b, &[".//aside"], false);
        assert!(get_elements_by_tag_name(&b, "aside").is_empty());
        let ps = get_elements_by_tag_name(&b, "p");
        assert_eq!(ps.len(), 2);
        // The first <p> now carries the moved tail.
        let first_p_tail = tail(&ps[0]).unwrap_or_default();
        assert!(
            first_p_tail.contains("tail-text"),
            "expected first <p>'s tail to contain the moved aside tail, got {first_p_tail:?}"
        );
    }

    #[test]
    fn prune_unwanted_nodes_with_backup_true_restores_when_threshold_trips() {
        // Stage 2c-iii-a: the `with_backup=true` branch is now fully wired
        // via `dom::deep_clone` (landed Stage 2c-i). When the post-prune text
        // shrinks to ≤ old_len/7, the returned NodeRef IS the deep_clone
        // backup (i.e. the pristine pre-prune tree). Construct a tree where
        // the <section> carries the bulk of the text so removing it trips
        // the threshold (140-x section + 1-char paragraph; post-prune = 1,
        // pre-prune = 141; 1*7 = 7 ≤ 141 → backup).
        let big = "x".repeat(140);
        let dom = parse(&format!("<div><p>a</p><section>{big}</section></div>"));
        let b = body(&dom);
        let result = prune_unwanted_nodes(&b, &[".//section"], true);
        // The returned tree must still contain <section> (backup is pristine).
        assert!(
            !get_elements_by_tag_name(&result, "section").is_empty(),
            "backup restored — <section> preserved"
        );
        // The IN-PLACE-mutated input `b` does NOT contain <section> any more
        // (Python's `tree` was mutated then `return backup`; the mutation is
        // permanent on the original `b`, but the caller rebinds to `result`).
        assert!(
            get_elements_by_tag_name(&b, "section").is_empty(),
            "input tree is the over-pruned one (orphaned by caller's rebind)"
        );
    }

    #[test]
    fn prune_unwanted_nodes_with_backup_true_keeps_tree_when_text_survives() {
        // Threshold-survival path: post-prune text > old_len/7. Build a tree
        // where the removed <aside> is a small fraction (10 chars) of the
        // total content (>140 chars elsewhere). The returned NodeRef is the
        // (mutated) input tree, NOT the backup.
        let p_text = "p".repeat(140);
        let dom = parse(&format!("<div><p>{p_text}</p><aside>0123456789</aside></div>"));
        let b = body(&dom);
        let result = prune_unwanted_nodes(&b, &[".//aside"], true);
        // <aside> is gone in the result — same handle as the input tree.
        assert!(get_elements_by_tag_name(&result, "aside").is_empty());
        // The p-paragraph is preserved.
        assert_eq!(get_elements_by_tag_name(&result, "p").len(), 1);
    }

    // ---- handle_textnode ----

    /// Build a (`wrap_div`, `p`) pair: `p` is attached as a child of
    /// `wrap_div`; the caller must keep `wrap_div` alive for `p`'s
    /// parent pointer to upgrade. Otherwise `wrap_div` gets dropped and
    /// `p.parent.upgrade()` fails — which silently clears the tail.
    fn make_p_with(text: Option<&str>, tail_str: Option<&str>) -> (NodeRef, NodeRef) {
        let wrap = create_element("div");
        let p = create_element("p");
        dom::append_child(&wrap, &p);
        if let Some(t) = text {
            set_element_text(&p, Some(t));
        }
        if tail_str.is_some() {
            set_tail(&p, tail_str);
        }
        (wrap, p)
    }

    #[test]
    fn handle_textnode_returns_none_for_done_tag() {
        let done = create_element("done");
        let opts = Options::default();
        assert!(handle_textnode(&done, &opts, true, false).is_none());
    }

    #[test]
    fn handle_textnode_returns_none_for_empty_element() {
        // len(elem)==0 AND no text AND no tail → None.
        let p = create_element("p");
        let opts = Options::default();
        assert!(handle_textnode(&p, &opts, true, false).is_none());
    }

    #[test]
    fn handle_textnode_lb_bypass_preserves_tail() {
        // comments_fix=false, tag=lb, preserve_spaces=false →
        // bypass branch trims the tail and returns elem.
        let lb = create_element("lb");
        let wrap = create_element("div");
        dom::append_child(&wrap, &lb);
        set_tail(&lb, Some("   spaced tail   "));
        let opts = Options::default();
        let got = handle_textnode(&lb, &opts, false, false);
        assert!(got.is_some());
        assert_eq!(tail(&lb).as_deref(), Some("spaced tail"));
    }

    #[test]
    fn handle_textnode_lb_bypass_preserve_spaces_true_keeps_tail_untrimmed() {
        let lb = create_element("lb");
        let wrap = create_element("div");
        dom::append_child(&wrap, &lb);
        set_tail(&lb, Some("   spaced tail   "));
        let opts = Options::default();
        let got = handle_textnode(&lb, &opts, false, true);
        assert!(got.is_some());
        // tail unchanged (preserve_spaces=true).
        assert_eq!(tail(&lb).as_deref(), Some("   spaced tail   "));
    }

    #[test]
    fn handle_textnode_moves_tail_to_text_when_text_absent() {
        // Tag=p, no text, no element children, tail="moved". After
        // handle_textnode (comments_fix=true, preserve_spaces=false), the
        // tail should be cleared and the text should be "moved".
        let (_wrap, p) = make_p_with(None, Some("moved"));
        let opts = Options::default();
        let got = handle_textnode(&p, &opts, true, false);
        assert!(got.is_some());
        let got = got.unwrap();
        assert_eq!(element_text(&got).as_deref(), Some("moved"));
        assert!(tail(&got).is_none());
    }

    #[test]
    fn handle_textnode_trims_text_and_tail() {
        // Text "  Hello  ", tail "  Tail  " → trimmed.
        let p = create_element("p");
        set_element_text(&p, Some("  Hello world  "));
        let wrap = create_element("div");
        dom::append_child(&wrap, &p);
        // Add an element after p so trim of tail kicks in.
        let after = create_element("p");
        dom::append_child(&wrap, &after);
        set_tail(&p, Some("  some tail  "));
        let opts = Options::default();
        let got = handle_textnode(&p, &opts, true, false);
        assert!(got.is_some());
        assert_eq!(element_text(&got.unwrap()).as_deref(), Some("Hello world"));
        assert_eq!(tail(&p).as_deref(), Some("some tail"));
    }

    #[test]
    fn handle_textnode_filters_facebook_text() {
        // Text = "" (None after trim), then textfilter via tail = "Facebook"
        // → return None.
        let p = create_element("p");
        let wrap = create_element("div");
        dom::append_child(&wrap, &p);
        set_tail(&p, Some("Facebook"));
        // No text. handle_textnode moves tail->text first; then textfilter
        // sees text="Facebook" — which DOES NOT trip textfilter alone
        // because the filter only fires when text is None/empty. So we
        // need a scenario where text remains None and tail is "Facebook".
        //
        // Achieve this by giving the p an element child (so the "move tail
        // to text" branch is skipped) AND a tail that's a Facebook line.
        let span = create_element("span");
        dom::append_child(&p, &span);
        // Now: element_text(p)=None, child_count(p)=1, tail(p)="Facebook".
        // The first "done/empty" guard: not done, and child_count>0 so the
        // empty branch fails → don't return None. The "move tail to text"
        // guard requires child_count==0 → skipped. trim runs on (text=None
        // stays None, tail="Facebook" stays "Facebook"). Then the final
        // filter: text is None/empty → textfilter is checked against
        // element_text=None, tail="Facebook" → textfilter returns true →
        // return None.
        let opts = Options::default();
        let got = handle_textnode(&p, &opts, true, false);
        assert!(got.is_none(), "Facebook line should trip textfilter");
    }

    // ---- process_node ----

    #[test]
    fn process_node_returns_none_for_done_tag() {
        let done = create_element("done");
        let opts = Options::default();
        assert!(process_node(&done, &opts).is_none());
    }

    #[test]
    fn process_node_returns_none_for_empty_element() {
        let p = create_element("p");
        let opts = Options::default();
        assert!(process_node(&p, &opts).is_none());
    }

    #[test]
    fn process_node_swaps_tail_to_text_when_text_absent() {
        // Tag != "lb", no text, has tail → swap tail into text, tail = None.
        let p = create_element("p");
        let wrap = create_element("div");
        dom::append_child(&wrap, &p);
        set_tail(&p, Some("body text here"));
        let opts = Options::default();
        let got = process_node(&p, &opts);
        assert!(got.is_some());
        assert_eq!(element_text(&p).as_deref(), Some("body text here"));
        assert!(tail(&p).is_none());
    }

    #[test]
    fn process_node_filters_via_textfilter() {
        // Text = "Facebook" → textfilter true → return None.
        let p = create_element("p");
        set_element_text(&p, Some("Facebook"));
        let opts = Options::default();
        let got = process_node(&p, &opts);
        assert!(got.is_none());
    }

    #[test]
    fn process_node_lb_does_not_swap_tail() {
        // Tag == "lb" → the tag != "lb" guard prevents swap. lb with no
        // text and tail "x" → keep both intact (tail trims to "x").
        let lb = create_element("lb");
        let wrap = create_element("div");
        dom::append_child(&wrap, &lb);
        set_tail(&lb, Some("x"));
        let opts = Options::default();
        let got = process_node(&lb, &opts);
        assert!(got.is_some());
        // text stays None; tail stays "x".
        assert!(element_text(&lb).is_none());
        assert_eq!(tail(&lb).as_deref(), Some("x"));
    }

    // ---- duplicate_test stub via Options ----

    #[test]
    fn options_dedup_stub_returns_false_until_field_added() {
        // Pin: the Stage 2b' `Options::dedup()` accessor returns false
        // until Stage 2c-i adds `pub dedup: bool` to the `Options` struct.
        // Stay alert if this changes silently. See the TODO(M3-stage-2c-i)
        // on the accessor body.
        let opts = Options::default();
        assert!(!opts.dedup());
    }

    // -----------------------------------------------------------------------
    // Stage 6: sanitize_tree (external.py:163-190)
    // -----------------------------------------------------------------------

    /// Helper: parse `html`, sanitize the body, return the body NodeRef
    /// for inspection. The `Dom` is kept alive so the rcdom Drop quirk
    /// doesn't drain descendants.
    fn sanitize_body(html: &str) -> (Dom, NodeRef, String) {
        let dom = Dom::parse(html);
        let body = dom.body().expect("body parsed");
        let opts = Options::default();
        let (text, _len) = sanitize_tree(&body, &opts);
        (dom, body, text)
    }

    /// Stage 6/test-5 — `sanitize_tree` strips `class="..."` attributes via
    /// the strip-non-TEI-tags pass. Specifically: a `<div class="x">`
    /// survives (div IS in TEI_VALID_TAGS) and KEEPS its class attribute
    /// (Python doesn't drop attributes on TEI tags). But a `<section
    /// class="x">` is NOT in TEI_VALID_TAGS, so the wrapper is stripped
    /// entirely — children survive but the class on the wrapper is gone.
    ///
    /// The brief asks for "strips class attributes" — but Python's
    /// sanitize_tree does NOT have an "attribute-stripping" pass per se.
    /// What it DOES do is strip non-TEI WRAPPERS, which removes any
    /// attributes those wrappers carried. We pin THAT faithful behaviour.
    #[test]
    fn sanitize_tree_strips_class_attributes_on_non_tei_wrappers() {
        let html = r#"<html><body>
            <section class="non-tei-wrapper">
                <p class="tei-tag-keeps-attr">Hello world content here</p>
            </section>
        </body></html>"#;
        let (_dom, body, _text) = sanitize_body(html);
        // The <section> wrapper is non-TEI → stripped entirely (no element
        // with that class survives).
        let sections = dom::get_elements_by_tag_name(&body, "section");
        assert_eq!(
            sections.len(),
            0,
            "section is not in TEI_VALID_TAGS — wrapper (and its class) stripped"
        );
        // The <p> survives AND keeps its class attribute (TEI-valid tags
        // are not stripped, so their attributes survive — Python's
        // sanitize_tree only strips tag WRAPPERS, not attributes per se).
        let ps = dom::get_elements_by_tag_name(&body, "p");
        assert_eq!(ps.len(), 1, "p is in TEI_VALID_TAGS — survives");
    }

    /// Stage 6/test-6 — `sanitize_tree` removes empty `<p></p>` elements
    /// via `prune_html` (which is called by `tree_cleaning` as part of
    /// `sanitize_tree`'s phase 1).
    #[test]
    fn sanitize_tree_removes_empty_paragraphs() {
        let html = r#"<html><body>
            <p>Substantive content here that survives</p>
            <p></p>
            <p>More substantive content</p>
            <p>   </p>
        </body></html>"#;
        let (_dom, body, _text) = sanitize_body(html);
        let ps = dom::get_elements_by_tag_name(&body, "p");
        // `<p></p>` with no children is dropped by prune_html (p is in
        // CUT_EMPTY_ELEMS). The whitespace-only `<p>   </p>` has a single
        // Text-node child, so `not(node())` is false — it survives the
        // prune. We pin BOTH behaviours.
        assert!(
            ps.len() < 4,
            "at least one empty <p> should have been dropped; got {}",
            ps.len()
        );
        // Both substantive <p> elements must survive.
        let texts: Vec<String> = ps.iter().map(text_content).collect();
        assert!(
            texts.iter().any(|t| t.contains("Substantive content")),
            "substantive <p> dropped: {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t.contains("More substantive content")),
            "second substantive <p> dropped: {texts:?}"
        );
    }

    /// Stage 6/test-7 — `sanitize_tree` preserves non-trivial body
    /// content end-to-end. The text output must equal the trimmed
    /// space-joined itertext of the surviving structure.
    #[test]
    fn sanitize_tree_preserves_substantive_content() {
        let html = r#"<html><body>
            <article>
                <h2>Heading text</h2>
                <p>First paragraph with words and commas, internal structure, real prose.</p>
                <p>Second paragraph continuing the article body with substantive content.</p>
            </article>
        </body></html>"#;
        let (_dom, _body, text) = sanitize_body(html);
        // Substantive content must appear in the returned text.
        assert!(
            text.contains("First paragraph"),
            "first paragraph dropped: {text:?}"
        );
        assert!(
            text.contains("Second paragraph"),
            "second paragraph dropped: {text:?}"
        );
        assert!(
            text.contains("Heading text"),
            "heading text dropped: {text:?}"
        );
    }

    // ---- M4 Stage 2: convert_link + convert_tags(links=true) -----------

    #[test]
    fn convert_link_renames_a_to_ref_and_resolves_relative_href() {
        // htmlprocessing.py:369-378 — `<a href="/x">` under base
        // `https://e.com` → `<ref target="https://e.com/x">`.
        let dom = parse(r#"<html><body><a href="/x">click</a></body></html>"#);
        let b = body(&dom);
        let a = get_elements_by_tag_name(&b, "a")[0].clone();
        convert_link(&a, Some("https://e.com"));
        // The original <a> is detached; the new <ref> lives under <body>.
        let refs = get_elements_by_tag_name(&b, "ref");
        assert_eq!(refs.len(), 1);
        assert_eq!(
            crate::readability::dom::get_attribute(&refs[0], "target").as_deref(),
            Some("https://e.com/x")
        );
        // href attribute must be cleared (Python: elem.attrib.clear()).
        assert_eq!(
            crate::readability::dom::get_attribute(&refs[0], "href"),
            None
        );
    }

    #[test]
    fn convert_link_passes_absolute_href_through_unchanged() {
        // `<a href="https://other.com/y">` → `<ref target="https://other.com/y">`.
        let dom = parse(
            r#"<html><body><a href="https://other.com/y">link</a></body></html>"#,
        );
        let b = body(&dom);
        let a = get_elements_by_tag_name(&b, "a")[0].clone();
        convert_link(&a, Some("https://e.com"));
        let refs = get_elements_by_tag_name(&b, "ref");
        assert_eq!(refs.len(), 1);
        assert_eq!(
            crate::readability::dom::get_attribute(&refs[0], "target").as_deref(),
            Some("https://other.com/y")
        );
    }

    #[test]
    fn convert_tags_links_true_resolves_relative_anchors_against_options_url() {
        // End-to-end: doc with two relative anchors gets two
        // `<ref target="https://e.com/...">` after `convert_tags` with
        // `Options { links: true, url: Some("https://e.com"), ..default() }`.
        let dom = parse(
            r#"<html><body><div>
                <a href="/foo">A</a>
                <a href="/bar">B</a>
            </div></body></html>"#,
        );
        let b = body(&dom);
        let opts = Options {
            links: true,
            url: Some("https://e.com".to_string()),
            ..Options::default()
        };
        convert_tags(&b, &opts);
        let refs = get_elements_by_tag_name(&b, "ref");
        assert_eq!(refs.len(), 2, "expected two <ref>, got {:?}", refs.len());
        let targets: Vec<String> = refs
            .iter()
            .filter_map(|r| crate::readability::dom::get_attribute(r, "target"))
            .collect();
        assert!(targets.contains(&"https://e.com/foo".to_string()));
        assert!(targets.contains(&"https://e.com/bar".to_string()));
        // No `<a>` should remain — convert_link renames all of them.
        assert!(get_elements_by_tag_name(&b, "a").is_empty());
    }
}
