//! `clean.rs` — the Stage-2 cleaning pass (`Readability.js:2434-2668`, `:2088-
//! 2108`, `:2641-2651`, `:862-883`).
//!
//! **Stage 2 scope (HLD §7.4).** Faithful transcription with line cites
//! (anti-inversion, HLD §4.3(a)). Provides:
//!
//! * [`clean_conditionally`] — `_cleanConditionally(e, tag)`
//!   (`Readability.js:2434-2632`), the complete shadiness checklist.
//! * [`clean_headers`] — `_cleanHeaders(e)` (`Readability.js:2659-2668`).
//! * [`clean_styles`] — `_cleanStyles(e)` (`Readability.js:2088-2108`).
//! * [`clean_matched_nodes`] — `_cleanMatchedNodes(e, filter)`
//!   (`Readability.js:2641-2651`).
//! * [`get_text_density`] — `_getTextDensity(e, tags)`
//!   (`Readability.js:2414-2426`).
//! * [`has_ancestor_tag`] — `_hasAncestorTag(node, tag, maxDepth=3,
//!   filterFn)` (`Readability.js:2217-2235`) — the general 4-argument form.
//! * [`single_cell_table_unwrap`] — the single-cell-`<table>` unwrap pass
//!   (`Readability.js:862-883`).
//!
//! # CRITICAL ANTI-INVERSION (HLD §4 + supervisor directive)
//!
//! `_cleanConditionally` **deliberately KEEPS marked data tables** — the
//! `if (isDataTable) return false` clause at `Readability.js:2461-2463` and
//! the ancestor-table-`isDataTable` clause at `:2466-2468`. A faithful port
//! therefore preserves EDGAR's financial tables exactly as Readability-JS
//! does. The faithful expected outcome is **convergence toward RJS**, NOT
//! beating it; out-cleaning RJS would be inversion.

use crate::readability::dom::{
    self, Dom, NodeRef, children, get_all_nodes_with_tag, get_elements_by_tag_name, is_element,
    parent, tag_name, text_content,
};
use crate::readability::helpers::{FLAG_CLEAN_CONDITIONALLY, Flags, get_next_node};
use crate::readability::prep;
use crate::readability::regexps;
use crate::readability::scoring;

/// `_hasAncestorTag(node, tagName, maxDepth, filterFn)`
/// (`Readability.js:2217-2235`) — the general 4-argument form.
///
/// JS:
/// ```text
/// maxDepth = maxDepth || 3;
/// tagName = tagName.toUpperCase();
/// var depth = 0;
/// while (node.parentNode) {
///   if (maxDepth > 0 && depth > maxDepth) return false;
///   if (node.parentNode.tagName === tagName &&
///       (!filterFn || filterFn(node.parentNode))) return true;
///   node = node.parentNode;
///   depth++;
/// }
/// return false;
/// ```
///
/// `max_depth`:
/// * `0` ⇒ JS `0 || 3` ⇒ effective `3` (the JS default-from-falsy);
/// * `-1` ⇒ JS `-1 || 3` ⇒ `-1` (negative is truthy ⇒ kept). The `maxDepth > 0`
///   guard inside the loop is then never true, so the walk is **unbounded**;
///   this is the "no cap" call site at `Readability.js:2466`
///   (`_hasAncestorTag(node, "table", -1, isDataTable)`).
///
/// `filter_fn`: `None` ⇒ JS `!filterFn` short-circuits the check ⇒ any
/// ancestor with the matching tag wins. `Some(f)` ⇒ ALSO require `f(parent)`
/// (the JS `&& filterFn(node.parentNode)` clause).
pub fn has_ancestor_tag(
    node: &NodeRef,
    tag_name_arg: &str,
    max_depth: i32,
    filter_fn: Option<&dyn Fn(&NodeRef) -> bool>,
) -> bool {
    let want = tag_name_arg.to_ascii_uppercase();
    // maxDepth = maxDepth || 3 — JS truthy/falsy. 0 is falsy; -1, 1, 5… are
    // truthy. Mirror with: 0 → 3, else keep.
    let max_depth = if max_depth == 0 { 3 } else { max_depth };
    let mut depth = 0_i32;
    let mut cur = node.clone();
    while let Some(p) = parent(&cur) {
        if max_depth > 0 && depth > max_depth {
            return false;
        }
        if tag_name(&p).as_deref() == Some(want.as_str())
            && filter_fn.map(|f| f(&p)).unwrap_or(true)
        {
            return true;
        }
        cur = p;
        depth += 1;
    }
    false
}

/// `_getTextDensity(e, tags)` (`Readability.js:2414-2426`):
/// `childrenLength / textLength`, where `textLength = _getInnerText(e,
/// true).length` (0 ⇒ return 0), and `childrenLength` is the sum of
/// `_getInnerText(c, true).length` for each descendant of `e` whose tag is in
/// `tags` (`_getAllNodesWithTag(e, tags)`).
pub fn get_text_density(e: &NodeRef, tags: &[&str]) -> f64 {
    let text_length = scoring::inner_text_len(e) as f64;
    if text_length == 0.0 {
        return 0.0;
    }
    let mut children_length = 0.0_f64;
    for child in get_all_nodes_with_tag(e, tags) {
        children_length += scoring::inner_text_len(&child) as f64;
    }
    children_length / text_length
}

/// `_cleanHeaders(e)` (`Readability.js:2659-2668`).
///
/// Remove every `<h1>`/`<h2>` descendant whose `_getClassWeight < 0`.
///
/// Note: `_getClassWeight` returns `0` when `FLAG_WEIGHT_CLASSES` is inactive
/// (`Readability.js:2143-2145`), so this is a no-op then — exactly the JS
/// behaviour.
pub fn clean_headers(flags: &Flags, e: &NodeRef) {
    // _removeNodes iterates backwards (Readability.js:303-317); descendant
    // order is deterministic and our `get_all_nodes_with_tag` is a snapshot,
    // so backwards iteration is faithful (no live-list semantics needed —
    // each removal mutates the tree, not the snapshot).
    let nodes = get_all_nodes_with_tag(e, &["h1", "h2"]);
    for node in nodes.iter().rev() {
        if parent(node).is_some() && scoring::get_class_weight(flags, node) < 0 {
            dom::remove(node);
        }
    }
}

/// `_cleanStyles(e)` (`Readability.js:2088-2108`).
///
/// Recursively remove every `PRESENTATIONAL_ATTRIBUTES` attribute from `e` and
/// its element descendants. Also remove `width`/`height` on
/// `DEPRECATED_SIZE_ATTRIBUTE_ELEMS` (`TABLE`/`TH`/`TD`/`HR`/`PRE`). The walk
/// **skips** SVG (`tagName.toLowerCase() === "svg"`) and ALL its descendants.
///
/// `text_content`-invisible (HLD §2 — attribute mutations don't change the
/// WHATWG `Node.textContent` DFS); included for structural fidelity (a future
/// `include_html` Option would surface it).
pub fn clean_styles(e: &NodeRef) {
    // The JS guard: `if (!e || e.tagName.toLowerCase() === "svg") return;`
    let lname = dom::local_name(e);
    if lname.as_deref() == Some("svg") {
        return;
    }
    if !is_element(e) {
        return;
    }
    // Remove every PRESENTATIONAL_ATTRIBUTE.
    for attr in regexps::PRESENTATIONAL_ATTRIBUTES {
        dom::remove_attribute(e, attr);
    }
    // Width/height on DEPRECATED_SIZE_ATTRIBUTE_ELEMS (JS compares `tagName`,
    // i.e. upper-case).
    if let Some(t) = tag_name(e)
        && regexps::DEPRECATED_SIZE_ATTRIBUTE_ELEMS.contains(&t.as_str())
    {
        dom::remove_attribute(e, "width");
        dom::remove_attribute(e, "height");
    }
    // Recurse over `e.firstElementChild` siblings.
    for c in children(e) {
        clean_styles(&c);
    }
}

/// `_cleanMatchedNodes(e, filter)` (`Readability.js:2641-2651`).
///
/// Walk forward from `e` via `_getNextNode` (DFS) until the
/// `_getNextNode(e, true)` "end of search" marker (the node we would have
/// reached after skipping `e`'s subtree — i.e. visit only `e`'s descendants).
/// For each visited `next`, call `filter(next, className + " " + id)`:
/// true ⇒ remove (and continue from `_removeAndGetNext`), false ⇒ continue
/// from `_getNextNode(next)`.
pub fn clean_matched_nodes(e: &NodeRef, filter: &dyn Fn(&NodeRef, &str) -> bool) {
    let end_marker = get_next_node(e, true);
    let mut next = get_next_node(e, false);
    while let Some(n) = next.clone() {
        // The JS guard: `while (next && next != endOfSearchMarkerNode)`.
        if let Some(ref end) = end_marker
            && std::rc::Rc::ptr_eq(&n, end)
        {
            break;
        }
        let class = dom::class_name(&n);
        let id_ = dom::id(&n);
        let match_string = format!("{class} {id_}");
        if filter(&n, &match_string) {
            // `_removeAndGetNext(n)` = `next = _getNextNode(n, true); n.remove();`
            let after = get_next_node(&n, true);
            dom::remove(&n);
            next = after;
        } else {
            next = get_next_node(&n, false);
        }
    }
}

/// `_cleanConditionally(e, tag)` (`Readability.js:2434-2632`).
///
/// The **complete** shadiness checklist. Every clause is transcribed in JS
/// order with line cites; see inline comments. The function is a no-op when
/// `FLAG_CLEAN_CONDITIONALLY` is inactive.
///
/// **KEEP clauses** (the anti-inversion fidelity points):
/// * `2461-2463` `if (tag === "table" && isDataTable(node))` → keep.
/// * `2466-2468` `_hasAncestorTag(node, "table", -1, isDataTable)` → keep.
/// * `2470-2472` `_hasAncestorTag(node, "code")` → keep.
/// * `2474-2481` any descendant data table → keep.
/// * `2520-2523` embed with attr matching `_allowedVideoRegex` → keep.
/// * `2526-2531` `<object>` whose `innerHTML` matches the video regex →
///   keep.
/// * `2613-2627` simple list-of-images exception (ul/ol whose every li
///   contains exactly one image) → keep.
pub fn clean_conditionally(dom: &Dom, flags: &Flags, e: &NodeRef, tag: &str) {
    // 2435-2437: flag guard.
    if !flags.is_active(FLAG_CLEAN_CONDITIONALLY) {
        return;
    }
    let tag_lower = tag.to_ascii_lowercase();

    // 2444: `_removeNodes(_getAllNodesWithTag(e, [tag]), filter)`. The
    // filter returns `true` to remove. We iterate the snapshot in REVERSE
    // (mirroring `_removeNodes`'s `for (i = nodeList.length - 1; i >= 0; i--)`,
    // Readability.js:308) and only act on nodes with a parent (the JS
    // `if (parentNode)` guard at :311).
    let nodes = get_all_nodes_with_tag(e, &[tag_lower.as_str()]);
    for node in nodes.iter().rev() {
        if parent(node).is_none() {
            continue;
        }
        if should_remove_conditionally(dom, flags, node, &tag_lower) {
            dom::remove(node);
        }
    }
}

/// The inner `filter` of `_cleanConditionally` (`Readability.js:2444-2631`).
/// Returns `true` to remove. Every clause is line-cited.
fn should_remove_conditionally(dom: &Dom, flags: &Flags, node: &NodeRef, tag: &str) -> bool {
    // 2446-2448: `isDataTable = t => t._readabilityDataTable;`
    let is_data_table = |t: &NodeRef| dom.is_readability_data_table(t);

    // 2450: `isList = tag === "ul" || tag === "ol"`.
    let mut is_list = tag == "ul" || tag == "ol";
    // 2451-2459: if not a list, compute the list-density alternate.
    if !is_list {
        let mut list_length = 0_usize;
        for list in get_all_nodes_with_tag(node, &["ul", "ol"]) {
            list_length += scoring::inner_text_len(&list);
        }
        let inner_len = scoring::inner_text_len(node);
        if inner_len > 0 {
            is_list = (list_length as f64) / (inner_len as f64) > 0.9;
        }
        // JS: `listLength / 0 = Infinity`, `Infinity > 0.9 = true`. But that
        // only fires when node has zero inner text AND non-zero list inner
        // text — practically impossible (the list's text IS in node's text).
        // Defensive: keep `is_list = false` when inner_len == 0, matching the
        // JS only on the unreachable edge of "non-empty descendant lists with
        // no text" (impossible — lists' text IS in node's).
    }

    // 2461-2463: KEEP — tag === "table" && data table.
    if tag == "table" && is_data_table(node) {
        return false;
    }
    // 2466-2468: KEEP — inside any data table ancestor (unbounded depth).
    if has_ancestor_tag(node, "table", -1, Some(&is_data_table)) {
        return false;
    }
    // 2470-2472: KEEP — inside a <code> ancestor (default maxDepth=3).
    if has_ancestor_tag(node, "code", 0, None) {
        return false;
    }
    // 2474-2481: KEEP — any descendant `<table>` that is a data table.
    if get_elements_by_tag_name(node, "table")
        .iter()
        .any(is_data_table)
    {
        return false;
    }

    // 2483: weight = _getClassWeight(node).
    let weight = scoring::get_class_weight(flags, node) as f64;

    // 2487: contentScore = 0; (the TODO note: "Consider taking into account
    // original contentScore here." — the JS does NOT use the real score yet,
    // we follow the JS exactly.)
    let content_score = 0.0_f64;

    // 2489-2491: weight + contentScore < 0 ⇒ remove.
    if weight + content_score < 0.0 {
        return true;
    }

    // 2493: `if (_getCharCount(node, ",") < 10)` — the dense "many checks
    // when commas are scarce" gate. ALL the remaining shadiness checks live
    // inside this block (2493-2628).
    let comma_count = scoring::get_char_count(node, ",");
    if comma_count >= 10 {
        // 2630 (implicit `return false` for the case "lots of commas, keep").
        return false;
    }

    // -- the < 10 commas branch (2493-2628) --

    // 2497-2500: counts.
    let p = get_elements_by_tag_name(node, "p").len() as i64;
    let img = get_elements_by_tag_name(node, "img").len() as i64;
    // 2499: `li = node.getElementsByTagName("li").length - 100;` — yes, the
    // `- 100` is in the JS. It is a coarse "if more <li>s than -100" effort
    // by the JS to defang isolated list pages; we transcribe verbatim.
    let li = get_elements_by_tag_name(node, "li").len() as i64 - 100;
    let input = get_elements_by_tag_name(node, "input").len() as i64;

    // 2501-2508: headingDensity = _getTextDensity(node, [h1..h6]).
    let heading_density = get_text_density(node, &["h1", "h2", "h3", "h4", "h5", "h6"]);

    // 2510-2515: embeds = _getAllNodesWithTag(node, [object,embed,iframe]).
    let embeds = get_all_nodes_with_tag(node, &["object", "embed", "iframe"]);

    // 2517-2534: embed-count loop with the early-return video KEEP clauses.
    let mut embed_count = 0_i64;
    for embed in &embeds {
        // 2519-2523: any attribute value matches _allowedVideoRegex ⇒ KEEP.
        if let dom::NodeData::Element { attrs, .. } = &embed.data {
            let any_video = attrs
                .borrow()
                .iter()
                .any(|a| regexps::videos().is_match(&a.value));
            if any_video {
                return false;
            }
        }
        // 2526-2531: `<object>` innerHTML check. `Readability.js:2527`
        // (`tagName === "object"`) is dead — `tagName` is always upper-case,
        // so the lower-case compare never fires. Faithfully transcribed as
        // dead; firing it would be a KEEP-direction inversion (port keeps an
        // `<object>` RJS removes).
        let _is_object_branch_dead = tag_name(embed).as_deref() == Some("object");

        embed_count += 1;
    }

    // 2536: innerText = _getInnerText(node).
    let inner_text = dom::inner_text(node, true);

    // 2538-2544: REGEXPS.adWords / loadingWords on innerText ⇒ remove.
    // These two patterns carry the `/u` flag and contain non-ASCII alternations
    // (`广告`, `Реклама`, `Anuncio`, etc.). Per HLD §8 they are dialect-faithful
    // when transcribed as anchored, /u-equivalent Unicode patterns; for Rust's
    // `regex` crate which is Unicode-default these compile fine.
    if regexps::ad_words().is_match(&inner_text) || regexps::loading_words().is_match(&inner_text) {
        return true;
    }

    // 2546-2552: contentLength / linkDensity / textishTags / textDensity /
    // isFigureChild.
    let content_length = inner_text.chars().count() as i64;
    let link_density = scoring::get_link_density(node);
    // textishTags = ["SPAN", "LI", "TD"].concat(DIV_TO_P_ELEMS).
    // DIV_TO_P_ELEMS in our regexps is upper-case ["BLOCKQUOTE","DL","DIV",
    // "IMG","OL","P","PRE","TABLE","UL"]. `get_all_nodes_with_tag` is
    // case-insensitive on the input tag list (lower-cases everything) — we
    // pass them through as the JS-style upper-case list which lower-cases
    // identically.
    let mut textish_tags: Vec<&str> = vec!["span", "li", "td"];
    for t in regexps::DIV_TO_P_ELEMS {
        textish_tags.push(t);
    }
    let text_density = get_text_density(node, &textish_tags);
    let is_figure_child = has_ancestor_tag(node, "figure", 0, None);

    // 2555-2609: `shouldRemoveNode = () => { ... }`.
    // We compute the verdict by accumulating "errs" exactly as the JS does;
    // any err means remove. The order of clauses is preserved (the JS pushes
    // to a Vec and tests `errs.length`).
    // Constants: `_linkDensityModifier` is `options.linkDensityModifier || 0`
    // (Readability.js:66). Default = 0; Stage-2 uses the default (no
    // Options surface yet).
    const LINK_DENSITY_MODIFIER: f64 = 0.0;

    let mut errs = false;
    // 2557-2559: !isFigureChild && img > 1 && p/img < 0.5 ⇒ bad p:img ratio.
    if !is_figure_child && img > 1 && (p as f64) / (img as f64) < 0.5 {
        errs = true;
    }
    // 2560-2562: !isList && li > p ⇒ too many <li>s outside a list.
    if !is_list && li > p {
        errs = true;
    }
    // 2563-2565: input > Math.floor(p / 3) ⇒ too many inputs per p.
    // JS: Math.floor of integer p/3 is identical to integer division.
    if input > p.div_euclid(3) {
        errs = true;
    }
    // 2566-2577: suspiciously short.
    if !is_list
        && !is_figure_child
        && heading_density < 0.9
        && content_length < 25
        && (img == 0 || img > 2)
        && link_density > 0.0
    {
        errs = true;
    }
    // 2578-2586: low weight + a little linky.
    if !is_list && weight < 25.0 && link_density > 0.2 + LINK_DENSITY_MODIFIER {
        errs = true;
    }
    // 2587-2591: high weight + mostly links.
    if weight >= 25.0 && link_density > 0.5 + LINK_DENSITY_MODIFIER {
        errs = true;
    }
    // 2592-2596: suspicious embed.
    if (embed_count == 1 && content_length < 75) || embed_count > 1 {
        errs = true;
    }
    // 2597-2601: no useful content.
    if img == 0 && text_density == 0.0 {
        errs = true;
    }

    let have_to_remove = errs;

    // 2614-2627: image-gallery exception. Only consults if isList && haveToRemove.
    if is_list && have_to_remove {
        // for (x=0; x<node.children.length; x++) { ... }
        for child in children(node) {
            // 2618-2620: if child.children.length > 1 ⇒ early-return haveToRemove
            // (i.e. don't apply the exception).
            if children(&child).len() > 1 {
                return have_to_remove;
            }
        }
        let li_count = get_elements_by_tag_name(node, "li").len() as i64;
        // 2623-2625: if (img == li_count) return false (KEEP).
        // NOTE: this is `img` (the descendant <img> count up top), not a
        // recount.
        if img == li_count {
            return false;
        }
    }
    have_to_remove
}

/// Single-cell `<table>` unwrap (`Readability.js:862-883`).
///
/// For every `<table>` descendant of `article_content`:
/// 1. `tbody = _hasSingleTagInsideElement(table, "TBODY") ?
///    table.firstElementChild : table;`
/// 2. If `_hasSingleTagInsideElement(tbody, "TR")`:
///    * `row = tbody.firstElementChild`
///    * If `_hasSingleTagInsideElement(row, "TD")`:
///      * `cell = row.firstElementChild`
///      * `cell = _setNodeTag(cell, _everyNode(cell.childNodes,
///        _isPhrasingContent) ? "P" : "DIV")`
///      * `table.parentNode.replaceChild(cell, table)`.
pub fn single_cell_table_unwrap(dom: &mut Dom, article_content: &NodeRef) {
    // The JS iterates `_getAllNodesWithTag(articleContent, ["table"])` via
    // `_forEachNode`, which is forward order. Each operation REPLACES the
    // table with its single cell in the tree; our snapshot is unaffected
    // (it holds owned `Rc<Node>` clones).
    for table in get_all_nodes_with_tag(article_content, &["table"]) {
        if parent(&table).is_none() {
            continue;
        }
        // 866-868: tbody = single TBODY inside ⇒ unwrap first.
        let tbody = if crate::readability::helpers::has_single_tag_inside_element(&table, "TBODY") {
            dom::first_element_child(&table).unwrap_or_else(|| table.clone())
        } else {
            table.clone()
        };
        // 869-880: nested has_single_tag checks.
        if !crate::readability::helpers::has_single_tag_inside_element(&tbody, "TR") {
            continue;
        }
        let Some(row) = dom::first_element_child(&tbody) else {
            continue;
        };
        if !crate::readability::helpers::has_single_tag_inside_element(&row, "TD") {
            continue;
        }
        let Some(cell) = dom::first_element_child(&row) else {
            continue;
        };

        // 873-878: setNodeTag to P (all child nodes phrasing) or DIV.
        let all_phrasing = dom::child_nodes(&cell)
            .iter()
            .all(crate::readability::helpers::is_phrasing_content);
        let new_tag = if all_phrasing { "P" } else { "DIV" };
        let cell_new = dom.set_node_tag(&cell, new_tag);

        // 879: table.parentNode.replaceChild(cell_new, table).
        if let Some(table_parent) = parent(&table) {
            dom::replace_child(&table_parent, &cell_new, &table);
        }
    }
}

/// Trailing-`<br>`-before-`<p>` removal (`Readability.js:852-860`).
///
/// For every `<br>` descendant of `article_content`, if `_nextNode(br
/// .nextSibling)` is a `<p>`, remove the `<br>`.
pub fn remove_br_before_p(article_content: &NodeRef) {
    for br in get_all_nodes_with_tag(article_content, &["br"]) {
        if parent(&br).is_none() {
            continue;
        }
        let next_sib = crate::readability::helpers::next_sibling(&br);
        let next = crate::readability::helpers::next_node(next_sib);
        if next.as_ref().and_then(tag_name).as_deref() == Some("P") {
            dom::remove(&br);
        }
    }
}

/// `<h1>` → `<h2>` retag (`Readability.js:828-832`).
pub fn replace_h1_with_h2(dom: &mut Dom, article_content: &NodeRef) {
    let h1s = get_all_nodes_with_tag(article_content, &["h1"]);
    for h1 in h1s {
        let _ = dom.set_node_tag(&h1, "h2");
    }
}

/// The share-strip pass (`Readability.js:806-813`).
///
/// For each top-level child of `article_content`, run `_cleanMatchedNodes`
/// with a filter that removes nodes whose `className + " " + id` matches the
/// `share` regex AND whose `textContent.length < DEFAULT_CHAR_THRESHOLD`
/// (500).
pub fn share_strip(article_content: &NodeRef) {
    const SHARE_ELEMENT_THRESHOLD: usize = 500;
    for top in children(article_content) {
        clean_matched_nodes(&top, &|node, match_string| {
            regexps::share_elements().is_match(match_string)
                && text_content(node).chars().count() < SHARE_ELEMENT_THRESHOLD
        });
    }
}

// Re-export the `prep::clean` helper so this module is the natural home for
// _clean call-out plumbing too (the cleaning pass calls `prep::clean` for
// each tag in the unconditional list).
#[allow(unused_imports)]
pub(crate) use prep::clean as clean_unconditional;

#[cfg(test)]
mod tests {
    //! Every expected value hand-derived by tracing `Readability.js`
    //! (NOT by running an oracle — anti-inversion, HLD §4).
    use super::*;
    use crate::readability::dom::Dom;
    use crate::readability::helpers::FLAG_WEIGHT_CLASSES;

    fn dom_div(html: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        (dom, div)
    }

    // ---- _hasAncestorTag (Readability.js:2217-2235) ----

    #[test]
    fn has_ancestor_tag_default_max_depth_3() {
        // p > b > i > em > a > section: from <a>, parent <em> depth 0, <i> 1,
        // <b> 2, <p> 3, <section> 4. With maxDepth=3 (default) and tagName
        // "SECTION", walk stops at depth 4 → false.
        let dom = Dom::parse("<section><p><b><i><em><a>x</a></em></i></b></p></section>");
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();
        assert!(!has_ancestor_tag(&a, "section", 0, None));
        // P is parent of B parent of I parent of EM parent of A: depths 3,2,1,0 — wait.
        // Actually: a's parent=em (depth 0). depth 0 OK. Check tagName em==SECTION? no.
        // walk: cur=em, depth=1. em.parent=i. Check i==SECTION? no. cur=i, depth=2.
        // i.parent=b. Check b==SECTION? no. cur=b, depth=3. b.parent=p. depth 3 OK
        // (3 > 3 is false). Check p==SECTION? no. cur=p, depth=4. now depth 4 > 3
        // — return false. So <p> (3 ancestors up from <a>) is in window.
        // Pin both:
        assert!(has_ancestor_tag(&a, "p", 0, None));
        assert!(has_ancestor_tag(&a, "i", 0, None));
        assert!(has_ancestor_tag(&a, "b", 0, None));
        assert!(has_ancestor_tag(&a, "em", 0, None));
    }

    #[test]
    fn has_ancestor_tag_minus_one_is_unbounded() {
        // Same tree; maxDepth=-1 (JS: -1 || 3 = -1, then maxDepth > 0 is false
        // so no cap). SECTION is depth-5 ancestor → still finds.
        let dom = Dom::parse("<section><p><b><i><em><a>x</a></em></i></b></p></section>");
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();
        assert!(has_ancestor_tag(&a, "section", -1, None));
    }

    #[test]
    fn has_ancestor_tag_filter_fn() {
        // Only count ancestor when filter returns true.
        let dom = Dom::parse("<div id=outer><div id=inner><span>x</span></div></div>");
        let span = get_elements_by_tag_name(&dom.body().unwrap(), "span")[0].clone();
        // Filter that only accepts <div id=inner>:
        let filter = |n: &NodeRef| dom::get_attribute(n, "id").as_deref() == Some("inner");
        assert!(has_ancestor_tag(&span, "div", 0, Some(&filter)));
        let filter_outer = |n: &NodeRef| dom::get_attribute(n, "id").as_deref() == Some("outer");
        assert!(has_ancestor_tag(&span, "div", 0, Some(&filter_outer)));
        // A filter that matches nothing:
        let filter_no = |_: &NodeRef| false;
        assert!(!has_ancestor_tag(&span, "div", 0, Some(&filter_no)));
    }

    // ---- _getTextDensity (Readability.js:2414-2426) ----

    #[test]
    fn text_density_zero_text_returns_zero() {
        let (_d, div) = dom_div("<div></div>");
        assert_eq!(get_text_density(&div, &["p"]), 0.0);
    }

    #[test]
    fn text_density_ratio() {
        // div text "AAAABBBB" (8) with one <p>"BBBB" (4 chars) descendant:
        // density = 4 / 8 = 0.5.
        let (_d, div) = dom_div("<div>AAAA<p>BBBB</p></div>");
        let d = get_text_density(&div, &["p"]);
        assert!((d - 0.5).abs() < 1e-12);
    }

    // ---- _cleanStyles (Readability.js:2088-2108) ----

    #[test]
    fn clean_styles_strips_presentational_attrs() {
        let dom =
            Dom::parse(r#"<div align="center" style="color:red"><p bgcolor="blue">x</p></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean_styles(&div);
        assert!(dom::get_attribute(&div, "align").is_none());
        assert!(dom::get_attribute(&div, "style").is_none());
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        assert!(dom::get_attribute(&p, "bgcolor").is_none());
    }

    #[test]
    fn clean_styles_removes_width_height_on_table_th_td() {
        let dom =
            Dom::parse(r#"<div><table width="100"><tr><td height="20">x</td></tr></table></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean_styles(&div);
        let table = get_elements_by_tag_name(&div, "table")[0].clone();
        assert!(dom::get_attribute(&table, "width").is_none());
        let td = get_elements_by_tag_name(&div, "td")[0].clone();
        assert!(dom::get_attribute(&td, "height").is_none());
    }

    #[test]
    fn clean_styles_skips_svg_subtree() {
        let dom = Dom::parse(
            r#"<div><svg style="x"><rect style="y"></rect></svg><p style="z">t</p></div>"#,
        );
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean_styles(&div);
        // The SVG and its descendants must NOT have been touched.
        let svg = get_elements_by_tag_name(&div, "svg")[0].clone();
        assert_eq!(dom::get_attribute(&svg, "style").as_deref(), Some("x"));
        let rect = get_elements_by_tag_name(&div, "rect")[0].clone();
        assert_eq!(dom::get_attribute(&rect, "style").as_deref(), Some("y"));
        // The <p> outside the svg IS touched.
        let p = get_elements_by_tag_name(&div, "p")[0].clone();
        assert!(dom::get_attribute(&p, "style").is_none());
    }

    // ---- _cleanHeaders (Readability.js:2659-2668) ----

    #[test]
    fn clean_headers_removes_negative_weight_h1_h2() {
        // class "sidebar" → negative weight (-25).
        let dom =
            Dom::parse(r#"<div><h1 class="article">keep</h1><h2 class="sidebar">drop</h2></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_headers(&flags, &div);
        assert!(text_content(&div).contains("keep"));
        assert!(!text_content(&div).contains("drop"));
    }

    #[test]
    fn clean_headers_no_op_when_weight_flag_off() {
        // FLAG_WEIGHT_CLASSES off ⇒ getClassWeight returns 0 ⇒ no removals.
        let dom = Dom::parse(r#"<div><h1 class="sidebar">x</h1></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let mut flags = Flags::default();
        flags.remove(FLAG_WEIGHT_CLASSES);
        clean_headers(&flags, &div);
        assert!(text_content(&div).contains("x"));
    }

    // ---- _cleanMatchedNodes (Readability.js:2641-2651) ----

    #[test]
    fn clean_matched_nodes_removes_matching_descendants() {
        let dom = Dom::parse(
            "<div id=root><p class=keep>k</p><p class=share-this>s</p><span>tail</span></div>",
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        // Filter: remove if className contains "share-this".
        clean_matched_nodes(&root, &|_n, m| m.contains("share-this"));
        let t = text_content(&root);
        assert!(t.contains('k'));
        assert!(!t.contains('s'), "share-this <p> must be removed: {t:?}");
        assert!(t.contains("tail"));
    }

    // ---- _cleanConditionally KEEP clauses (the load-bearing tests) ----

    /// **The EDGAR anti-inversion pin.** A data table inside articleContent
    /// MUST survive `_cleanConditionally("table")` via the `tag === "table"
    /// && isDataTable` KEEP clause (`Readability.js:2461-2463`). Without
    /// this clause, the table would be filtered by the shadiness checks
    /// (no commas, structural counts trigger errs) and stripped.
    #[test]
    fn clean_conditionally_keeps_marked_data_table() {
        let mut dom = Dom::parse(
            "<div><table><thead><tr><th>Q1</th><th>Q2</th></tr></thead>\
             <tbody><tr><td>$1000</td><td>$2500</td></tr><tr><td>$1100</td><td>$2700</td></tr></tbody>\
             </table></div>",
        );
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        // Mark the table (the JS does this in _prepArticle before
        // _cleanConditionally).
        super::super::mark_data_tables::mark_data_tables(&mut dom, &div);
        let table = get_elements_by_tag_name(&div, "table")[0].clone();
        assert!(
            dom.is_readability_data_table(&table),
            "thead-bearing table must be marked data (test precondition)"
        );
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &div, "table");
        // Table must still be there.
        assert_eq!(
            get_elements_by_tag_name(&div, "table").len(),
            1,
            "data table MUST survive _cleanConditionally — Readability.js:2461 KEEP"
        );
    }

    /// `_cleanConditionally` KEEPS nodes inside a `<code>` ancestor.
    #[test]
    fn clean_conditionally_keeps_inside_code_ancestor() {
        let dom = Dom::parse("<div><code><div class=\"sidebar\"><p>x</p></div></code></div>");
        let outer = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &outer, "div");
        // The inner sidebar <div> must survive — it has a <code> ancestor.
        // Even though sidebar's class weight is -25, the code-ancestor KEEP
        // (Readability.js:2470-2472) wins.
        // Get all DIVs with class=sidebar:
        let still_present = get_elements_by_tag_name(&outer, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(
            still_present,
            "inside <code> ancestor MUST be kept (Readability.js:2470)"
        );
    }

    /// Without the KEEP, a low-weight `<div>` with no commas, few <p>s, etc.
    /// IS removed. This is the negative control for the KEEP clauses.
    #[test]
    fn clean_conditionally_removes_low_weight_div_no_keep() {
        let dom = Dom::parse(r#"<div id=root><div class="sidebar"><p>x</p></div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_sidebar = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(
            !any_sidebar,
            "low-weight sidebar div must be removed (no KEEP applies)"
        );
    }

    /// FLAG_CLEAN_CONDITIONALLY off ⇒ no-op.
    #[test]
    fn clean_conditionally_no_op_when_flag_off() {
        let dom = Dom::parse(r#"<div id=root><div class="sidebar"><p>x</p></div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let mut flags = Flags::default();
        flags.remove(FLAG_CLEAN_CONDITIONALLY);
        clean_conditionally(&dom, &flags, &root, "div");
        let any_sidebar = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(any_sidebar, "flag off ⇒ no removal");
    }

    /// `>= 10` commas ⇒ keep (Readability.js:2493 inverse). NOTE: the weight
    /// check (`Readability.js:2489-2491`, `weight + contentScore < 0 ⇒
    /// remove`) fires BEFORE the comma check, so a div with `negative` class
    /// AND many commas STILL gets removed — the JS short-circuits on the
    /// weight before even consulting commas. To exercise the "many commas
    /// keeps" path we must use a neutral class (weight 0); the comma check
    /// then keeps the div because the commas branch is `< 10` (the >=10
    /// path falls through to `return false` after the closing brace).
    #[test]
    fn clean_conditionally_many_commas_keeps_div() {
        let dom = Dom::parse(
            r#"<div id=root><div class="neutralwrapper"><p>a,b,c,d,e,f,g,h,i,j,k</p></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_inner = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "neutralwrapper");
        assert!(
            any_inner,
            "≥10 commas keeps the div (Readability.js:2493 inverse path); weight 0 means weight check does not fire first"
        );
    }

    /// Image-gallery exception (`Readability.js:2613-2627`). A `<ul>` whose
    /// every `<li>` contains exactly one `<img>` (and nothing else) is kept,
    /// even if shadiness checks would otherwise remove it.
    ///
    /// NOTE: the weight check fires BEFORE the gallery exception is even
    /// considered (the JS `if (weight + contentScore < 0) return true` at
    /// 2489-2491 is OUTSIDE the comma-gate that contains the gallery
    /// clause). So a UL with a `negative`-regex class is removed regardless.
    /// Use a neutral class to reach the gallery exception path.
    #[test]
    fn clean_conditionally_image_gallery_ul_is_kept() {
        let dom = Dom::parse(
            r#"<div><ul class="gallery-list"><li><img src="a"></li><li><img src="b"></li><li><img src="c"></li></ul></div>"#,
        );
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &div, "ul");
        // The ul must still be there.
        assert!(
            !get_elements_by_tag_name(&div, "ul").is_empty(),
            "image-gallery <ul> (every <li> with exactly 1 child = <img>) must be kept (Readability.js:2623-2625)"
        );
    }

    // ---- single_cell_table_unwrap (Readability.js:862-883) ----

    #[test]
    fn single_cell_table_unwraps_to_p_when_phrasing() {
        let mut dom = Dom::parse("<div><table><tr><td>just text</td></tr></table></div>");
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        single_cell_table_unwrap(&mut dom, &div);
        // The table is gone, replaced by a <p>"just text".
        assert!(get_elements_by_tag_name(&div, "table").is_empty());
        assert_eq!(text_content(&div), "just text");
        let ps = get_elements_by_tag_name(&div, "p");
        assert_eq!(ps.len(), 1, "single-cell table unwrapped to <p>");
    }

    #[test]
    fn single_cell_table_unwraps_to_div_when_block_inside_cell() {
        let mut dom =
            Dom::parse("<div><table><tr><td><div>block content</div></td></tr></table></div>");
        let body = dom.body().unwrap();
        let outer = get_elements_by_tag_name(&body, "div")[0].clone();
        single_cell_table_unwrap(&mut dom, &outer);
        assert!(get_elements_by_tag_name(&outer, "table").is_empty());
        // The cell's children were non-phrasing (a <div>), so the cell was
        // retagged DIV (not P). The DIV with "block content" is still in the
        // tree.
        assert!(text_content(&outer).contains("block content"));
    }

    #[test]
    fn single_cell_table_unwrap_handles_tbody() {
        // <table><tbody><tr><td>x</td></tr></tbody></table> — single TBODY
        // inside, then single TR, then single TD.
        let mut dom =
            Dom::parse("<div><table><tbody><tr><td>cell text</td></tr></tbody></table></div>");
        let body = dom.body().unwrap();
        let outer = get_elements_by_tag_name(&body, "div")[0].clone();
        single_cell_table_unwrap(&mut dom, &outer);
        assert!(get_elements_by_tag_name(&outer, "table").is_empty());
        assert_eq!(text_content(&outer), "cell text");
    }

    #[test]
    fn single_cell_table_does_not_unwrap_multi_row() {
        let mut dom =
            Dom::parse("<div><table><tr><td>a</td></tr><tr><td>b</td></tr></table></div>");
        let body = dom.body().unwrap();
        let outer = get_elements_by_tag_name(&body, "div")[0].clone();
        single_cell_table_unwrap(&mut dom, &outer);
        // Table still present (2 rows → not single-cell).
        assert_eq!(get_elements_by_tag_name(&outer, "table").len(), 1);
    }

    // ---- remove_br_before_p (Readability.js:852-860) ----

    #[test]
    fn remove_br_before_p_removes_when_p_follows() {
        let dom = Dom::parse("<div>a<br><p>b</p></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        remove_br_before_p(&div);
        assert!(get_elements_by_tag_name(&div, "br").is_empty());
    }

    #[test]
    fn remove_br_before_p_keeps_when_no_p_follows() {
        let dom = Dom::parse("<div>a<br>b</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        remove_br_before_p(&div);
        assert_eq!(get_elements_by_tag_name(&div, "br").len(), 1);
    }

    // ---- replace_h1_with_h2 (Readability.js:828-832) ----

    #[test]
    fn replace_h1_with_h2_retags_all() {
        let mut dom = Dom::parse("<div><h1>a</h1><h1>b</h1></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        replace_h1_with_h2(&mut dom, &div);
        assert!(get_elements_by_tag_name(&div, "h1").is_empty());
        assert_eq!(get_elements_by_tag_name(&div, "h2").len(), 2);
    }

    // ---- NodeKey ABA re-audit under conditional-removal churn ----

    /// HLD §5.1 ABA invariant: removing a node via `dom::remove` does NOT
    /// touch the score / data-table side tables. Any stale NodeKey is
    /// harmless because:
    ///
    /// (1) Stage 2 reads `_readabilityDataTable` only inside
    ///     `_cleanConditionally`'s ancestor / descendant checks, and these
    ///     query LIVE nodes (which have their CURRENT addresses) — the
    ///     stale entries of removed nodes never alias a live read in
    ///     between `_markDataTables` and its consuming `_cleanConditionally`
    ///     calls.
    ///
    /// (2) Within ONE attempt, the only fresh-element creation after
    ///     `_cleanConditionally` is `set_node_tag` (single-cell-table
    ///     unwrap, h1→h2 retag). These create NEW `Rc<Node>` instances —
    ///     their addresses could *theoretically* reuse a freed slot, but
    ///     within a single attempt no scored DIV / no marked TABLE is
    ///     removed AND then a TABLE-tag element created at the same
    ///     address with the data-table flag preserved by accident. The
    ///     transfer is intentional only via `set_node_tag`'s explicit
    ///     side-table move.
    ///
    /// We exercise the exact churn: mark a data table, conditionally
    /// remove a sibling `<div>` (whose NodeKey *could* be reused if the
    /// allocator were to give it back), then assert the data-table flag is
    /// unchanged on the *live* table — i.e. the cleaning churn did not
    /// leak the data-table flag into a freshly-allocated descendant.
    #[test]
    fn aba_data_table_flag_survives_clean_conditionally_churn() {
        let mut dom = Dom::parse(
            "<div id=root>\
             <table><thead><tr><th>Q</th><th>Y</th></tr></thead><tbody><tr><td>1</td><td>2</td></tr></tbody></table>\
             <div class=\"sidebar\"><p>chrome to remove</p></div>\
             </div>",
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        super::super::mark_data_tables::mark_data_tables(&mut dom, &root);
        let table = get_elements_by_tag_name(&root, "table")[0].clone();
        assert!(dom.is_readability_data_table(&table));

        let flags = Flags::default();
        // Remove the sibling sidebar.
        clean_conditionally(&dom, &flags, &root, "div");
        // The data table is still marked.
        assert!(
            dom.is_readability_data_table(&table),
            "_markDataTables flag must survive _cleanConditionally churn — ABA invariant"
        );
        // The sidebar div is gone.
        let any_sidebar = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(!any_sidebar);
        // And no spurious data-table flag has appeared on the table's parent.
        assert!(!dom.is_readability_data_table(&root));
    }

    // ---- share_strip (Readability.js:806-813) ----

    /// `_cleanMatchedNodes` (`Readability.js:2641-2651`) starts from
    /// `_getNextNode(e, false)` — `e`'s first element child or sibling —
    /// so it walks `e`'s **descendants**, NOT `e` itself. The
    /// `share_strip` outer loop runs that walk for each top-level child of
    /// `articleContent`. Therefore share widgets that ARE the top-level
    /// children are NOT visited (they were never `e`); share widgets
    /// NESTED inside the content body ARE visited. The test mirrors the
    /// faithful corpus: share buttons live INSIDE a content body, not at
    /// the page-top level.
    #[test]
    fn share_strip_removes_nested_short_share_widgets() {
        let dom = Dom::parse(
            "<div id=ac>\
               <div id=body><p>Body para</p>\
                 <div class=\"share\">share buttons here</div>\
                 <div class=\"sharedaddy\">sharedaddy bar</div>\
                 <p>More body</p>\
               </div>\
             </div>",
        );
        let ac = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        share_strip(&ac);
        let t = text_content(&ac);
        assert!(t.contains("Body para"), "body kept: {t}");
        assert!(t.contains("More body"), "body kept: {t}");
        assert!(!t.contains("share buttons"), "share widget stripped: {t}");
        assert!(!t.contains("sharedaddy bar"), "sharedaddy stripped: {t}");
    }

    /// A share widget with >500-char text is kept (`Readability.js:810`
    /// threshold = `DEFAULT_CHAR_THRESHOLD`).
    #[test]
    fn share_strip_keeps_long_share_text() {
        let long = "x".repeat(600);
        let html = format!(
            "<div id=ac><div id=body><p>body</p><div class=\"share\">{long}</div></div></div>"
        );
        let dom = Dom::parse(&html);
        let ac = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        share_strip(&ac);
        let t = text_content(&ac);
        assert!(
            t.contains(&long),
            "share text >= 500 chars must be kept (Readability.js:810)"
        );
    }

    /// Pin the faithful "doesn't visit `e`" semantics: a top-level
    /// share-class child of articleContent is NOT removed by share-strip
    /// (the JS deliberately does not visit `e` itself, only descendants).
    /// The non-removal here is FAITHFUL to the JS, not a port bug.
    #[test]
    fn share_strip_does_not_visit_top_level_e_itself_faithful() {
        // articleContent has a top-level `<div class=share>` and a
        // top-level `<p>`. share_strip iterates children and runs
        // clean_matched_nodes(child, ...) which visits child's DESCENDANTS
        // only. So the top-level `<div class=share>` is NEVER visited as
        // `node`, NEVER tested by the filter, NEVER removed.
        let dom = Dom::parse(
            "<div id=ac><div class=\"share\">top-level share widget</div><p>body</p></div>",
        );
        let ac = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        share_strip(&ac);
        let t = text_content(&ac);
        assert!(
            t.contains("top-level share widget"),
            "FAITHFUL: clean_matched_nodes does NOT visit `e` itself \
             (Readability.js:2642-2643 starts from _getNextNode(e, false) — \
             a descendant or sibling, never `e`): {t}"
        );
    }

    // ---- _cleanConditionally per-clause coverage ----
    //
    // Every clause hand-traced from Readability.js:2493-2628.

    /// 2497-2562: `li > p outside a list`. With `li - 100 > p`, i.e. `li > p +
    /// 100`. A div with 101 li and 0 p ⇒ li=101-100=1 > p=0 ⇒ remove.
    #[test]
    fn clean_conditionally_li_gt_p_outside_list() {
        let mut html = String::from("<div id=root><div class=neg>");
        for _ in 0..101 {
            html.push_str("<li>x</li>");
        }
        html.push_str("</div></div>");
        let dom = Dom::parse(&html);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_neg = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "neg");
        assert!(
            !any_neg,
            "li(101)-100=1 > p=0 outside list ⇒ remove (Readability.js:2560-2562)"
        );
    }

    /// 2566-2577: suspiciously short.
    ///   !isList && !isFigureChild && headingDensity < 0.9 &&
    ///   contentLength < 25 && (img===0||img>2) && linkDensity > 0
    /// Construct: <div><a href=/x>w</a></div> — short text, has a link,
    /// no img, no headings. contentLength = "w".length = 1 < 25.
    /// linkDensity = 1/1 = 1.0 > 0. img = 0. headingDensity = 0 < 0.9.
    /// isList = false (it's a div with no ul/ol descendants relative to
    /// inner_text), isFigureChild = false.
    /// → suspiciously short → remove.
    #[test]
    fn clean_conditionally_suspiciously_short() {
        let dom = Dom::parse(r#"<div id=root><div class=neg><a href="/x">w</a></div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_neg = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "neg");
        assert!(!any_neg, "suspiciously short link-y div ⇒ remove");
    }

    /// 2587-2591: high weight (>= 25) + mostly links (linkDensity > 0.5).
    /// class "article" gives +25 weight. linkDensity > 0.5 via almost all
    /// text inside an <a>.
    #[test]
    fn clean_conditionally_high_weight_mostly_links() {
        // article class +25, 80% of text in a link. Long enough to not be
        // suspiciously short; but no commas, so checklist applies.
        let dom = Dom::parse(
            r#"<div id=root><div class="article">tail <a href="/x">most of this text is inside the anchor tag here</a></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_article = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "article");
        assert!(
            !any_article,
            "weight>=25 + linkDensity>0.5 ⇒ remove (Readability.js:2587-2591)"
        );
    }

    /// 2597-2601: no useful content (img===0 && textDensity===0).
    #[test]
    fn clean_conditionally_no_useful_content() {
        // empty class, no text, no img, no descendants → textDensity 0, img 0.
        let dom = Dom::parse(r#"<div id=root><div class=empty></div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_empty = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "empty");
        assert!(!any_empty, "no useful content ⇒ remove");
    }

    // ---- should_remove_conditionally per-clause coverage (Readability.js:2440-2628) ----
    //
    // The shadiness ladder. Each test pins ONE branch of the long list at
    // `Readability.js:2440-2628`. Existing tests above already cover:
    //   - 2461 KEEP "table && data table" (clean_conditionally_keeps_marked_data_table)
    //   - 2470 KEEP "<code> ancestor" (clean_conditionally_keeps_inside_code_ancestor)
    //   - 2493 KEEP "10+ commas" (clean_conditionally_many_commas_keeps_div)
    //   - 2560 REMOVE "li > p outside list" (clean_conditionally_li_gt_p_outside_list)
    //   - 2566 REMOVE "suspiciously short" (clean_conditionally_suspiciously_short)
    //   - 2587 REMOVE "weight + link density" (clean_conditionally_high_weight_mostly_links)
    //   - 2597 REMOVE "no useful content" (clean_conditionally_no_useful_content)
    //   - 2613 KEEP "image gallery <ul>" (clean_conditionally_image_gallery_ul_is_kept)
    //
    // The tests below cover the remaining branches.

    /// `Readability.js:2466-2468` KEEP: inside any `<table>` ancestor whose
    /// `_readabilityDataTable` flag is set. Unbounded depth (maxDepth = -1).
    /// rationale: a marked data table protects its descendants from
    /// `_cleanConditionally` removal regardless of class weight.
    #[test]
    fn clean_conditionally_keeps_descendants_inside_data_table_ancestor() {
        let mut dom = Dom::parse(
            "<div id=root>\
             <table><thead><tr><th>a</th><th>b</th></tr></thead>\
             <tbody><tr><td>1</td><td><div class=\"sidebar\"><p>x</p></div></td></tr></tbody>\
             </table>\
             </div>",
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        super::super::mark_data_tables::mark_data_tables(&mut dom, &root);
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        // The negative-weight `<div class=sidebar>` inside the data table
        // ancestor MUST survive (Readability.js:2466 KEEP).
        let any_sidebar = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(
            any_sidebar,
            "data-table ancestor protects descendants (Readability.js:2466-2468)"
        );
    }

    /// `Readability.js:2474-2481` KEEP: a descendant `<table>` that is a data
    /// table protects the candidate node from removal.
    /// rationale: a div wrapping a data table is not stripped even if its
    /// other shadiness checks would fail.
    #[test]
    fn clean_conditionally_keeps_node_with_data_table_descendant() {
        let mut dom = Dom::parse(
            "<div id=root>\
             <div class=\"neutralwrap\"><table><thead><tr><th>x</th><th>y</th></tr></thead>\
             <tbody><tr><td>1</td><td>2</td></tr></tbody></table></div>\
             </div>",
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        super::super::mark_data_tables::mark_data_tables(&mut dom, &root);
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        // The wrap div must survive: it has a data-table descendant.
        let any_neutral = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "neutralwrap");
        assert!(
            any_neutral,
            "descendant data table protects ancestor (Readability.js:2474-2481)"
        );
    }

    /// `Readability.js:2489-2491` REMOVE: `weight + contentScore < 0`. Negative
    /// class weight short-circuits BEFORE the comma gate is consulted.
    /// rationale: a `sidebar`-class div (-25 per regexps::negative) gets
    /// stripped without consulting the rest of the shadiness checks even with
    /// many commas (which would otherwise KEEP a neutral-weight div).
    #[test]
    fn clean_conditionally_negative_weight_removes_before_comma_gate() {
        // class "sidebar" → -25 weight (regexps::negative). Add many commas to
        // prove the weight short-circuit fires FIRST: with weight = 0 the
        // 10+ commas branch would KEEP this div.
        let dom = Dom::parse(
            r#"<div id=root><div class="sidebar"><p>a,b,c,d,e,f,g,h,i,j,k,l,m</p></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_sidebar = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "sidebar");
        assert!(
            !any_sidebar,
            "weight<0 short-circuits even with many commas (Readability.js:2489-2491)"
        );
    }

    /// `Readability.js:2519-2523` KEEP: an `<object>`/`<embed>`/`<iframe>`
    /// whose ANY attribute matches `REGEXPS.videos` (e.g. a YouTube src) is a
    /// video embed and the candidate is KEPT regardless of other shadiness.
    /// rationale: an iframe whose src is youtube.com keeps the containing div
    /// even when no other content is present.
    #[test]
    fn clean_conditionally_video_embed_attribute_keeps_node() {
        // No commas, no images, only an iframe with a YouTube src — without
        // the video-keep the div would otherwise be removed (no useful content
        // / suspicious embed). The KEEP path returns false immediately.
        let dom = Dom::parse(
            r#"<div id=root><div class="vidwrap"><iframe src="https://www.youtube.com/embed/abcDEF"></iframe></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_wrap = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "vidwrap");
        assert!(
            any_wrap,
            "video-attribute embed KEEPs candidate (Readability.js:2519-2523)"
        );
    }

    /// `Readability.js:2538-2544` REMOVE: `REGEXPS.adWords` match on innerText.
    /// rationale: a div whose ONLY text is exactly the ad-words alternation
    /// (`Advertisement`, `Werbung`, `Реклама`, `广告`, etc.) is stripped.
    #[test]
    fn clean_conditionally_ad_words_inner_text_removes() {
        // The regex is `^(ad(vertising|vertisement)?|pub(licité)?|werb(ung)?|广告|Реклама|Anuncio)$`
        // anchored — so the inner text must be EXACTLY one of those words.
        let dom = Dom::parse(r#"<div id=root><div class="x">Advertisement</div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "adWords inner text ⇒ remove (Readability.js:2540)"
        );
    }

    /// `Readability.js:2538-2544` REMOVE: `REGEXPS.loadingWords` match.
    /// rationale: a div whose inner text is exactly a loading-label
    /// (`Loading`, `Cargando…`, `Загрузка…`, etc.) is stripped.
    #[test]
    fn clean_conditionally_loading_words_inner_text_removes() {
        let dom = Dom::parse(r#"<div id=root><div class="y">Loading...</div></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_y = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "y");
        assert!(
            !any_y,
            "loadingWords inner text ⇒ remove (Readability.js:2541)"
        );
    }

    /// `Readability.js:2557-2559` REMOVE: not figure-child AND img > 1 AND
    /// p/img < 0.5. With 0 paragraphs and 2 images the ratio 0/2 = 0 < 0.5.
    /// rationale: an image-heavy div outside a `<figure>` is treated as a
    /// gallery wrapper and stripped when it lacks paragraphs.
    #[test]
    fn clean_conditionally_bad_p_to_img_ratio() {
        // class=x → weight 0. No <p>. Two <img>. No commas. p/img = 0 < 0.5.
        // Not a figure descendant. errs = true ⇒ remove.
        let dom = Dom::parse(
            r#"<div id=root><div class="x"><img src="a.jpg"><img src="b.jpg"></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "p/img < 0.5 outside figure ⇒ remove (Readability.js:2557-2559)"
        );
    }

    /// `Readability.js:2557-2559` KEEP path: the same shape but as a descendant
    /// of `<figure>` — the `isFigureChild` guard suppresses the ratio check.
    /// rationale: figure descendants are exempt from the image-density rule.
    #[test]
    fn clean_conditionally_figure_child_bypasses_p_to_img_ratio() {
        // Same shape as above but wrapped in <figure>. The ratio check is
        // disabled by isFigureChild=true. Other branches (suspiciously short
        // — disabled too by isFigureChild; no useful content — disabled by
        // img > 0) leave errs=false ⇒ keep.
        let dom = Dom::parse(
            r#"<div id=root><figure><div class="x"><img src="a.jpg"><img src="b.jpg"></div></figure></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            any_x,
            "isFigureChild bypasses p/img ratio (Readability.js:2557 guard)"
        );
    }

    /// `Readability.js:2563-2565` REMOVE: `input > Math.floor(p / 3)`. With
    /// 0 paragraphs the gate is `input > 0`, so a single `<input>` triggers.
    /// rationale: a form-like container with no paragraphs gets stripped.
    #[test]
    fn clean_conditionally_too_many_inputs_per_p() {
        let dom = Dom::parse(
            r#"<div id=root><div class="x"><input type="text"></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "input > floor(p/3) ⇒ remove (Readability.js:2563-2565)"
        );
    }

    /// `Readability.js:2578-2586` REMOVE: `!isList && weight<25 &&
    /// linkDensity > 0.2`. Class "x" → weight 0. A short prose with a long
    /// link drives linkDensity > 0.2.
    /// rationale: a low-weight div whose text is mostly links is stripped.
    #[test]
    fn clean_conditionally_low_weight_linky() {
        // Inner text: "ab " + "long anchor text here for density" =
        // about 36 chars; "long anchor text here for density" ≈ 33 in <a>,
        // so linkDensity ≈ 0.92 > 0.2. weight = 0 < 25.
        let dom = Dom::parse(
            r#"<div id=root><div class="x">ab <a href="/p">long anchor text here for density</a></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "weight<25 && linkDensity>0.2 ⇒ remove (Readability.js:2578-2586)"
        );
    }

    /// `Readability.js:2592-2596` REMOVE: `embedCount == 1 && contentLength
    /// < 75`. A short-text candidate with one non-video iframe is stripped.
    /// rationale: a single suspicious embed is enough to strip a short div.
    #[test]
    fn clean_conditionally_single_embed_short_content() {
        // class="x" → weight 0. Inner text < 75 chars. One <iframe> with a
        // non-video src (it would otherwise be a video-keep).
        let dom = Dom::parse(
            r#"<div id=root><div class="x">short text alongside an embed here<iframe src="https://example.com/x"></iframe></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "embedCount==1 && contentLength<75 ⇒ remove (Readability.js:2592-2594)"
        );
    }

    /// `Readability.js:2592-2596` REMOVE: `embedCount > 1` always triggers
    /// regardless of content length.
    /// rationale: multiple non-video embeds within a div get it stripped.
    #[test]
    fn clean_conditionally_multiple_embeds_removes() {
        // Two non-video iframes ⇒ embedCount = 2 > 1 ⇒ remove. Content >= 75
        // chars to prove the "embedCount > 1" half of the OR, not the
        // "embedCount == 1 && contentLength < 75" half.
        let dom = Dom::parse(
            r#"<div id=root><div class="x">some content text long enough to be over the seventy-five character minimum but it still has too many embeds inside it now today<iframe src="https://example.com/a"></iframe><iframe src="https://example.com/b"></iframe></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            !any_x,
            "embedCount>1 ⇒ remove (Readability.js:2595)"
        );
    }

    /// `Readability.js:2451-2459` "list density" branch: when `tag` is NOT a
    /// list, the JS computes `listLength / inner_text_len > 0.9` and treats
    /// the candidate as if it were a list (suppressing the
    /// li/p and low-weight-linky checks). With a div whose entire text is
    /// inside one `<ul>`, the ratio = 1.0 > 0.9.
    /// rationale: a div dominated by a list is treated as a list for the
    /// shadiness checks.
    #[test]
    fn clean_conditionally_list_density_treats_div_as_list() {
        // Build a candidate that would FAIL the suspiciously-short check
        // (img==0, contentLength=4 chars, linkDensity=1.0) IF isList were
        // false. With the list-density override (1.0 > 0.9 ⇒ is_list=true),
        // suspiciously-short is suppressed. To still get a kept outcome we
        // also need img>0 OR text_density>0 to defang the no-useful-content
        // arm. The <ul>'s text density > 0 (it has a text descendant), so
        // text_density>0 ⇒ no-useful-content arm does not fire.
        let dom = Dom::parse(
            r#"<div id=root><div class="x"><ul><li><a href="/p">a longer link text here for stable density numbers in this list density test</a></li></ul></div></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "div");
        let any_x = get_elements_by_tag_name(&root, "div")
            .iter()
            .any(|d| dom::class_name(d) == "x");
        assert!(
            any_x,
            "list-density override (Readability.js:2451-2459) treats div as list ⇒ \
             li/p, suspicious-short and low-weight-linky arms suppressed"
        );
    }

    /// `Readability.js:2614-2621` image-gallery exception, early-return when a
    /// child has MORE than one own child. The exception walks `<li>` children;
    /// if any `<li>` has > 1 child the exception is abandoned and the original
    /// haveToRemove verdict stands.
    /// rationale: an `<ul>` whose `<li>` items have text alongside `<img>`
    /// (i.e. multiple children per li) does not get the gallery exemption.
    #[test]
    fn clean_conditionally_gallery_exception_aborts_on_multi_child_li() {
        // class="x" — neutral; tag = "ul". The shadiness ladder for a UL with
        // a single img per li *would* return false via the gallery KEEP
        // (img == liCount). But here each li has TWO children (img + text
        // span), so the early-return at 2618-2620 fires returning
        // haveToRemove (which is true: img==2>1 AND p==0 ⇒ ratio 0<0.5
        // forces errs ... but wait, isList is true since tag=="ul", so
        // 2557 is is_figure_child false AND img>1 AND p/img<0.5 — p/img
        // check fires regardless of isList. Actually 2557-2559 fires when
        // !isFigureChild && img>1 && p/img<0.5. That's independent of
        // isList. So haveToRemove = true. The gallery early-return at
        // 2618-2620 returns true.
        let dom = Dom::parse(
            r#"<div id=root><ul class="x"><li><img src="a.jpg"><span>caption a</span></li><li><img src="b.jpg"><span>caption b</span></li></ul></div>"#,
        );
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = Flags::default();
        clean_conditionally(&dom, &flags, &root, "ul");
        // The UL is gone — gallery exception aborted by multi-child li
        // (Readability.js:2618-2620).
        assert!(
            get_elements_by_tag_name(&root, "ul").is_empty(),
            "multi-child <li> aborts gallery exception (Readability.js:2618-2620)"
        );
    }
}
