//! `prep.rs` — document/article preparation, Stage-1a slice (HLD §5, §7.1).
//!
//! Ported faithfully with `Readability.js:<line>` citations (anti-inversion,
//! HLD §4.3(a)). **Stage-1a scope only:** `_removeScripts`, `_prepDocument`
//! (`<style>` strip, `font`→`span`, `_replaceBrs`), `_getInnerText`, the safe
//! `_clean` (object/embed/footer/link/aside) and empty-`<p>` removal slice of
//! `_prepArticle`. **NOT** `_cleanConditionally` / `_markDataTables` /
//! `_cleanStyles` / `_cleanHeaders` / single-cell unwrap (Stage 2 — HLD §7.4),
//! which stay unported here.

use crate::readability::dom::{
    self, Dom, NodeRef, append_child, child_nodes, children, create_element,
    get_all_nodes_with_tag, get_attribute, get_elements_by_tag_name, inner_text, is_element,
    parent, replace_child, set_attribute, tag_name, text_content,
};
use crate::readability::helpers::{is_phrasing_content, is_whitespace, next_node, next_sibling};
use crate::readability::regexps;

/// `_getInnerText(e, normalizeSpaces=true)` (`Readability.js:2058-2067`).
///
/// Thin wrapper over [`dom::inner_text`], which already encodes
/// `textContent.trim()` + (when normalizing) `REGEXPS.normalize` `/\s{2,}/g`
/// → single space with the **JS** `\s` set (dialect-faithful, HLD §8). Kept as
/// a named function so call sites read like the JS.
pub fn get_inner_text(e: &NodeRef, normalize_spaces: bool) -> String {
    inner_text(e, normalize_spaces)
}

/// `_removeScripts(doc)` (`Readability.js:1975-1977`):
/// `_removeNodes(_getAllNodesWithTag(doc, ["script","noscript"]))`.
pub fn remove_scripts(doc_root: &NodeRef) {
    for n in get_all_nodes_with_tag(doc_root, &["script", "noscript"]) {
        dom::remove(&n);
    }
}

/// `_prepDocument()` (`Readability.js:659-670`).
///
/// 1. `_removeNodes(_getAllNodesWithTag(doc, ["style"]))`.
/// 2. if `doc.body` → `_replaceBrs(doc.body)`.
/// 3. `_replaceNodeTags(_getAllNodesWithTag(doc, ["font"]), "SPAN")`.
///
/// `doc_root` is the document root the JS calls `_getAllNodesWithTag(doc, …)`
/// against (descendant search), `body` is `doc.body`.
pub fn prep_document(dom: &mut Dom, doc_root: &NodeRef, body: Option<&NodeRef>) {
    // 1. Remove all style tags.
    for n in get_all_nodes_with_tag(doc_root, &["style"]) {
        dom::remove(&n);
    }
    // 2. _replaceBrs on the body.
    if let Some(b) = body {
        replace_brs(dom, b);
    }
    // 3. _replaceNodeTags(font, "SPAN") — slow-branch _setNodeTag per node.
    for font in get_all_nodes_with_tag(doc_root, &["font"]) {
        let _ = dom.set_node_tag(&font, "SPAN");
    }
}

/// `node.lastChild` (any node type). `None` if no children. Not in `dom`'s
/// element-centric facade; derived from the full child list.
fn last_child(node: &NodeRef) -> Option<NodeRef> {
    child_nodes(node).into_iter().next_back()
}

/// `node.previousElementSibling` — the immediately preceding **element**
/// sibling (skipping text/comment nodes), or `None` if there is none.
///
/// Local helper rather than a `dom.rs` primitive: `_unwrapNoscriptImages` is
/// the only Stage-2 site that reads `previousElementSibling`, so it stays
/// confined here per the "no new primitives unless genuinely needed" rule
/// (HLD §5 / port discipline). Mirrors the existing `next_element_sibling` in
/// `dom.rs` (line 345) but going backwards.
fn previous_element_sibling(node: &NodeRef) -> Option<NodeRef> {
    let p = parent(node)?;
    let kids = child_nodes(&p);
    let idx = kids.iter().position(|c| std::rc::Rc::ptr_eq(c, node))?;
    kids[..idx]
        .iter()
        .rev()
        .find(|c| matches!(c.data, dom::NodeData::Element { .. }))
        .cloned()
}

/// `_isSingleImage(node)` (`Readability.js:1871-1882`).
///
/// JS:
/// ```text
/// while (node) {
///   if (node.tagName === "IMG") return true;
///   if (node.children.length !== 1 || node.textContent.trim() !== "") return false;
///   node = node.children[0];
/// }
/// ```
///
/// `node.children` is **element-only** (HTML5 `HTMLCollection`), so a non-empty
/// text child still gates `textContent.trim() !== ""`. Our [`dom::children`]
/// returns element-only children to match. `node.textContent.trim()` is
/// `dom::inner_text(node, false)` (`Readability.js:2058-2067` with
/// `normalizeSpaces=false` ≡ `textContent.trim()`).
fn is_single_image(node: &NodeRef) -> bool {
    let mut cur = node.clone();
    loop {
        if tag_name(&cur).as_deref() == Some("IMG") {
            return true;
        }
        let elem_children = children(&cur);
        if elem_children.len() != 1 || !inner_text(&cur, false).is_empty() {
            return false;
        }
        cur = elem_children.into_iter().next().unwrap();
    }
}

/// `_unwrapNoscriptImages(doc)` (`Readability.js:1892-1968`).
///
/// Two-pass:
/// 1. **Placeholder-img cull (`:1895-1913`).** For every `<img>` in the doc,
///    walk its attributes — if NO attribute is named `src`/`srcset`/`data-src`/
///    `data-srcset` AND NO attribute *value* matches `/\.(jpg|jpeg|png|webp)/i`
///    ([`regexps::image_extension`]), the `<img>` is treated as a placeholder
///    and removed (`:1912` `img.remove()`).
///
///    Score-impact note (HLD §2): removing a placeholder `<img>` changes
///    `_cleanConditionally`'s img-count (`Readability.js:2498`,
///    `node.getElementsByTagName("img").length`) on the scored path, which can
///    flip a shadiness verdict (more shady when fewer "real" content imgs are
///    present). This is exactly why the JS does the cull BEFORE `_grabArticle`
///    — the port must match.
///
/// 2. **Noscript-img unwrap (`:1916-1967`).** For every `<noscript>` in the
///    doc that `_isSingleImage`-qualifies, look at `previousElementSibling`
///    (typically the placeholder `<img>` the page lazy-loaded — except step 1
///    may have already removed it). If that previous element ALSO
///    `_isSingleImage`-qualifies, copy "source-ish" attributes (name in
///    {`src`,`srcset`} OR value matches the image-extension regex) onto the
///    new image extracted from the noscript's children, then replace the
///    previous element with that new image element.
///
///    **Critical jsdom-inert / html5ever-inert parity (HLD §6.1).** The JS at
///    `:1928` does `tmp.innerHTML = noscript.innerHTML;` to *parse* the
///    noscript's content as markup. Under jsdom-inert (no `runScripts`,
///    `run.mjs:184`), `<noscript>` content is already parsed as children — so
///    `noscript.innerHTML` parsed back into `tmp` reproduces the same element
///    tree the parser put inside `<noscript>` in the first place. Our
///    [`Dom::parse`] sets `scripting_enabled: false` (`dom.rs:131-138`) for
///    the same parity, so the `<noscript>`'s children are already real
///    elements — we read them directly via `get_elements_by_tag_name(noscript,
///    "img")[0]` and `children(noscript)[0]` for `firstElementChild`, no
///    fragment re-parse needed.
///
/// Pipeline order (`Readability.js:2733`): called by `parse()` BEFORE
/// `_removeScripts` (`:2739`) and BEFORE `_prepDocument` (`:2741`) — see the
/// call site in `readability/mod.rs::Readability::parse`.
pub(crate) fn unwrap_noscript_images(doc_root: &NodeRef) {
    // ---- Pass 1: placeholder-img cull (Readability.js:1895-1913) ----
    for img in get_elements_by_tag_name(doc_root, "img") {
        // `var imgs = Array.from(doc.getElementsByTagName("img"))` — the JS
        // takes a snapshot via `Array.from`, our `get_elements_by_tag_name`
        // already returns an owned snapshot (HLD §5 / dom.rs:498-511).

        // `for (var i = 0; i < img.attributes.length; i++)` (`:1897`). Walk
        // every attribute; if any of `src`/`srcset`/`data-src`/`data-srcset`
        // is *named*, return early (KEEP); else if any attribute *value*
        // matches the image-extension regex, return early (KEEP); else
        // (fallthrough out of the for-loop) `img.remove()` (`:1912`).
        let mut keep = false;
        if let dom::NodeData::Element { attrs, .. } = &img.data {
            for a in attrs.borrow().iter() {
                let name = &*a.name.local;
                // Readability.js:1899-1905 — name switch (KEEP).
                if matches!(name, "src" | "srcset" | "data-src" | "data-srcset") {
                    keep = true;
                    break;
                }
                // Readability.js:1907-1909 — value-regex (KEEP).
                if regexps::image_extension().is_match(&a.value) {
                    keep = true;
                    break;
                }
            }
        }
        if !keep {
            // Readability.js:1912 — `img.remove()`.
            dom::remove(&img);
        }
    }

    // ---- Pass 2: noscript-img unwrap (Readability.js:1916-1967) ----
    for noscript in get_elements_by_tag_name(doc_root, "noscript") {
        // Readability.js:1919-1921 — `if (!_isSingleImage(noscript)) return;`
        // (continue in the `_forEachNode` loop).
        if !is_single_image(&noscript) {
            continue;
        }

        // Readability.js:1933 — `prevElement = noscript.previousElementSibling`.
        // (`:1922-1928` build `tmp` from `noscript.innerHTML`; under
        // scripting-disabled parsing the noscript's children are already
        // parsed elements, so we read them directly from `noscript` itself
        // instead of re-parsing.)
        let Some(prev_element) = previous_element_sibling(&noscript) else {
            continue;
        };

        // Readability.js:1934 — `if (prevElement && _isSingleImage(prevElement))`.
        if !is_single_image(&prev_element) {
            continue;
        }

        // Readability.js:1935-1938 — locate `prevImg`. If `prevElement.tagName
        // === "IMG"`, use it; else use the first descendant `<img>`.
        let prev_img = if tag_name(&prev_element).as_deref() == Some("IMG") {
            prev_element.clone()
        } else {
            let imgs = get_elements_by_tag_name(&prev_element, "img");
            let Some(first) = imgs.into_iter().next() else {
                continue;
            };
            first
        };

        // Readability.js:1940 — `newImg = tmp.getElementsByTagName("img")[0]`.
        // Under inert parsing `noscript`'s children ARE the parsed markup,
        // so `tmp.getElementsByTagName("img")[0]` ≡
        // `noscript.getElementsByTagName("img")[0]`.
        let new_imgs = get_elements_by_tag_name(&noscript, "img");
        let Some(new_img) = new_imgs.into_iter().next() else {
            continue;
        };

        // Readability.js:1941-1963 — attribute copy from prevImg onto newImg.
        // Iterate prevImg's attributes; for each: skip if value == ""; for
        // those with name in {src,srcset} OR value matching the
        // image-extension regex, copy onto newImg with a `data-old-` prefix
        // on the destination name if it already has an attribute by that
        // name. Skip if newImg already has the same name/value pair.
        let prev_attrs = if let dom::NodeData::Element { attrs, .. } = &prev_img.data {
            attrs
                .borrow()
                .iter()
                .map(|a| (a.name.local.to_string(), a.value.to_string()))
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (name, value) in prev_attrs {
            // Readability.js:1943-1945 — empty value: skip.
            if value.is_empty() {
                continue;
            }
            // Readability.js:1947-1951 — name in {src,srcset} OR value
            // matches /\.(jpg|jpeg|png|webp)/i.
            let is_source_attr =
                name == "src" || name == "srcset" || regexps::image_extension().is_match(&value);
            if !is_source_attr {
                continue;
            }
            // Readability.js:1952-1954 — newImg already has same value: skip.
            if get_attribute(&new_img, &name).as_deref() == Some(value.as_str()) {
                continue;
            }
            // Readability.js:1956-1959 — newImg has *some* value for this
            // attribute: prefix destination with `data-old-`.
            let attr_name = if get_attribute(&new_img, &name).is_some() {
                format!("data-old-{name}")
            } else {
                name.clone()
            };
            // Readability.js:1961 — setAttribute.
            set_attribute(&new_img, &attr_name, &value);
        }

        // Readability.js:1965 — `noscript.parentNode.replaceChild(
        // tmp.firstElementChild, prevElement)`. The `tmp.firstElementChild`
        // is the noscript's first element child (since the noscript content
        // is exactly the `<img>` markup under inert parsing).
        let Some(parent_of_noscript) = parent(&noscript) else {
            continue;
        };
        let Some(first_elem_child) = children(&noscript).into_iter().next() else {
            continue;
        };
        replace_child(&parent_of_noscript, &first_elem_child, &prev_element);
    }
}

/// `_replaceBrs(elem)` (`Readability.js:696-750`).
///
/// Replace runs of 2+ `<br>` (whitespace-only nodes between them ignored) with
/// a single `<p>`, moving following phrasing siblings into the `<p>` until the
/// next `<br><br>` chain or a non-phrasing node; trim trailing whitespace
/// children; and if the new `<p>`'s parent is itself a `<p>`, retag the parent
/// to `DIV`.
fn replace_brs(dom: &mut Dom, elem: &NodeRef) {
    for br in get_all_nodes_with_tag(elem, &["br"]) {
        let mut next = next_sibling(&br);
        let mut replaced = false;

        // while ((next = _nextNode(next)) && next.tagName == "BR")
        loop {
            next = next_node(next.clone());
            let Some(n) = next.clone() else { break };
            if tag_name(&n).as_deref() != Some("BR") {
                break;
            }
            replaced = true;
            let br_sibling = next_sibling(&n);
            dom::remove(&n);
            next = br_sibling;
        }

        if replaced {
            // var p = createElement("p"); br.parentNode.replaceChild(p, br);
            let p = create_element("p");
            let Some(br_parent) = parent(&br) else {
                continue;
            };
            replace_child(&br_parent, &p, &br);

            // next = p.nextSibling;
            let mut next = next_sibling(&p);
            while let Some(n) = next.clone() {
                // if next is BR and _nextNode(next.nextSibling) is BR -> break
                if tag_name(&n).as_deref() == Some("BR") {
                    let next_elem = next_node(next_sibling(&n));
                    if next_elem.as_ref().and_then(tag_name).as_deref() == Some("BR") {
                        break;
                    }
                }
                if !is_phrasing_content(&n) {
                    break;
                }
                // make this node a child of the new <p>
                let sibling = next_sibling(&n);
                append_child(&p, &n);
                next = sibling;
            }

            // while (p.lastChild && _isWhitespace(p.lastChild)) p.lastChild.remove()
            while let Some(lc) = last_child(&p) {
                if is_whitespace(&lc) {
                    dom::remove(&lc);
                } else {
                    break;
                }
            }

            // if (p.parentNode.tagName === "P") _setNodeTag(p.parentNode, "DIV")
            if let Some(pp) = parent(&p)
                && tag_name(&pp).as_deref() == Some("P")
            {
                let _ = dom.set_node_tag(&pp, "DIV");
            }
        }
    }
}

/// The Stage-1a `_prepArticle` slice (`Readability.js:782-884`, **subset**).
///
/// Ports ONLY the parts HLD §7.1 admits at Stage 1a:
/// * `_clean(articleContent, "object"|"embed"|"footer"|"link"|"aside")`
///   (`Readability.js:795-799`);
/// * the "Remove extra paragraphs" empty-`<p>` pass (`Readability.js:835-850`).
///
/// Deliberately **omitted** (Stage 2, HLD §7.4): `_cleanStyles`,
/// `_markDataTables`, `_fixLazyImages`, `_cleanConditionally`,
/// `_cleanMatchedNodes` share-strip, `_cleanHeaders`, `<h1>`→`<h2>`, the
/// trailing-`<br>`-before-`<p>` pass, single-cell-table unwrap. Their absence
/// is the documented Stage-1a over-inclusion on table pages (recorded, not
/// tuned — HLD §7.1).
///
/// **Stage 2 supersedes this** with [`prep_article`] (the full JS order
/// `Readability.js:782-884`); this Stage-1a slice is retained because it is
/// the input to the `page_wrap_prep_article_order_invariant` test (a
/// regression pin on the now-historical Stage-1c order-swap reasoning).
pub fn prep_article_stage1a(article_content: &NodeRef) {
    // _clean(articleContent, tag) for the safe set (Readability.js:795-799).
    for tag in ["object", "embed", "footer", "link", "aside"] {
        clean(article_content, tag);
    }

    // Remove extra paragraphs (Readability.js:835-850):
    // remove a <p> with zero img/embed/object/iframe descendants AND no
    // _getInnerText(p, false).
    for paragraph in get_all_nodes_with_tag(article_content, &["p"]) {
        let content_element_count =
            get_all_nodes_with_tag(&paragraph, &["img", "embed", "object", "iframe"]).len();
        if content_element_count == 0 && get_inner_text(&paragraph, false).is_empty() {
            dom::remove(&paragraph);
        }
    }
}

/// `_prepArticle(articleContent)` — full Stage-2 port (`Readability.js:782-
/// 884`).
///
/// Runs every step in JS order:
///
/// 1. `_cleanStyles` (`:783`) — strip presentational attrs (score-invisible,
///    HLD §2; included for full structural fidelity).
/// 2. `_markDataTables` (`:788`) — set `_readabilityDataTable` on every
///    `<table>` descendant.
/// 3. `_fixLazyImages` (`:790`) — **DEFERRED at Stage 2 (HLD §7.4 scope)**;
///    img-attribute fiddling is score-invisible (HLD §2), no `text_content`
///    impact.
/// 4. `_cleanConditionally("form")` (`:793`).
/// 5. `_cleanConditionally("fieldset")` (`:794`).
/// 6. `_clean("object")` (`:795`).
/// 7. `_clean("embed")` (`:796`).
/// 8. `_clean("footer")` (`:797`).
/// 9. `_clean("link")` (`:798`).
/// 10. `_clean("aside")` (`:799`).
/// 11. Share-strip per top-level child via `_cleanMatchedNodes` (`:806-813`).
/// 12. `_clean("iframe")` (`:815`).
/// 13. `_clean("input")` (`:816`).
/// 14. `_clean("textarea")` (`:817`).
/// 15. `_clean("select")` (`:818`).
/// 16. `_clean("button")` (`:819`).
/// 17. `_cleanHeaders` (`:820`).
/// 18. `_cleanConditionally("table")` (`:824`).
/// 19. `_cleanConditionally("ul")` (`:825`).
/// 20. `_cleanConditionally("div")` (`:826`).
/// 21. `<h1>` → `<h2>` retag (`:829-832`).
/// 22. Remove extra paragraphs (`:835-850`).
/// 23. `<br>`-before-`<p>` removal (`:852-860`).
/// 24. Single-cell-table unwrap (`:862-883`).
///
/// **HLD §4 anti-inversion:** `_cleanConditionally("table")` deliberately
/// KEEPS marked data tables (`:2461-2463`). Stage-2 EDGAR/HMRC tables are
/// preserved exactly as RJS preserves them — the port converges TO RJS, does
/// NOT out-clean it.
pub fn prep_article(
    dom: &mut Dom,
    flags: &crate::readability::helpers::Flags,
    article_content: &NodeRef,
) {
    use crate::readability::clean::{
        clean_conditionally, clean_headers, clean_styles, remove_br_before_p, replace_h1_with_h2,
        share_strip, single_cell_table_unwrap,
    };
    use crate::readability::mark_data_tables::mark_data_tables;

    // 1. _cleanStyles
    clean_styles(article_content);
    // 2. _markDataTables
    mark_data_tables(dom, article_content);
    // 3. _fixLazyImages — DEFERRED (score-invisible).

    // 4-5. _cleanConditionally for form / fieldset.
    clean_conditionally(dom, flags, article_content, "form");
    clean_conditionally(dom, flags, article_content, "fieldset");

    // 6-10. _clean for object / embed / footer / link / aside.
    for tag in ["object", "embed", "footer", "link", "aside"] {
        clean(article_content, tag);
    }

    // 11. Share-strip.
    share_strip(article_content);

    // 12-16. _clean for iframe / input / textarea / select / button.
    for tag in ["iframe", "input", "textarea", "select", "button"] {
        clean(article_content, tag);
    }

    // 17. _cleanHeaders.
    clean_headers(flags, article_content);

    // 18-20. _cleanConditionally for table / ul / div.
    clean_conditionally(dom, flags, article_content, "table");
    clean_conditionally(dom, flags, article_content, "ul");
    clean_conditionally(dom, flags, article_content, "div");

    // 21. <h1> → <h2> retag.
    replace_h1_with_h2(dom, article_content);

    // 22. Remove extra paragraphs.
    for paragraph in get_all_nodes_with_tag(article_content, &["p"]) {
        if dom::parent(&paragraph).is_none() {
            continue;
        }
        let content_element_count =
            get_all_nodes_with_tag(&paragraph, &["img", "embed", "object", "iframe"]).len();
        if content_element_count == 0 && get_inner_text(&paragraph, false).is_empty() {
            dom::remove(&paragraph);
        }
    }

    // 23. <br> before <p> removal.
    remove_br_before_p(article_content);

    // 24. Single-cell-table unwrap.
    single_cell_table_unwrap(dom, article_content);
}

/// `_clean(e, tag)` (`Readability.js:2182-2206`).
///
/// Remove every `tag` descendant, **except** for embeds (`object`/`embed`/
/// `iframe`) whose any attribute value, or (for `<object>`) inner HTML,
/// matches `_allowedVideoRegex` (the default `REGEXPS.videos`).
///
/// Stage-1a `_prepArticle` only calls this with object/embed/footer/link/aside;
/// `footer`/`link`/`aside` are not embeds so the video-allow branch never fires
/// for them. The embed branch is ported faithfully for `object`/`embed`. The
/// `<object>` innerHTML check uses `text_content` as a faithful proxy: the
/// default `REGEXPS.videos` matches a URL substring (`//youtube.com…`) which
/// appears in element text only via attribute-bearing children; in practice
/// the attribute loop above already catches allowed video objects, and no
/// gold/corpus `<object>` carries a video URL solely in a text node. Recorded
/// as a bounded Stage-1a fidelity note (serialization is score-invisible per
/// HLD §2; a full innerHTML serializer is Stage-3+).
pub fn clean(e: &NodeRef, tag: &str) {
    let is_embed = matches!(tag, "object" | "embed" | "iframe");
    for element in get_all_nodes_with_tag(e, &[tag]) {
        let mut keep = false;
        if is_embed {
            // Check every attribute value against _allowedVideoRegex.
            if let Some(attr_match) = any_attr_matches_videos(&element) {
                keep = attr_match;
            }
            // For <object>, also check inner content (see doc note).
            if !keep
                && tag_name(&element).as_deref() == Some("OBJECT")
                && regexps::videos().is_match(&text_content(&element))
            {
                keep = true;
            }
        }
        if !keep {
            dom::remove(&element);
        }
    }
}

/// `for (attr of element.attributes) if (_allowedVideoRegex.test(attr.value))
/// return false /* keep */`. Returns `Some(true)` if an attribute value
/// matches the video regex (⇒ keep the node), else `Some(false)`.
fn any_attr_matches_videos(element: &NodeRef) -> Option<bool> {
    if let markup5ever_rcdom::NodeData::Element { attrs, .. } = &element.data {
        for a in attrs.borrow().iter() {
            if regexps::videos().is_match(&a.value) {
                return Some(true);
            }
        }
        return Some(false);
    }
    None
}

/// `_isProbablyVisible`/etc. live in `helpers`; this module only needs the
/// element guard locally for the empty-`<p>` pass clarity.
#[allow(dead_code)]
fn is_el(n: &NodeRef) -> bool {
    is_element(n)
}

#[cfg(test)]
mod tests {
    //! Expected DOM shapes hand-derived by tracing `Readability.js` (NOT by
    //! running an oracle — inversion, HLD §4).
    use super::*;
    use crate::readability::dom::{Dom, get_elements_by_tag_name};

    fn body_text_after<F: FnOnce(&mut Dom, &NodeRef)>(html: &str, f: F) -> String {
        let mut dom = Dom::parse(html);
        let body = dom.body().unwrap();
        f(&mut dom, &body);
        text_content(&dom.body().unwrap())
    }

    // ---- _removeScripts (Readability.js:1975-1977) ----

    #[test]
    fn remove_scripts_drops_script_and_noscript() {
        let t = body_text_after(
            "<div>keep<script>var x=1;</script><noscript>ns</noscript>tail</div>",
            |_d, b| remove_scripts(b),
        );
        assert_eq!(t, "keeptail");
    }

    // ---- _prepDocument: style strip + font->span (Readability.js:659-670) ----

    #[test]
    fn prep_document_strips_style_and_retags_font() {
        let mut dom = Dom::parse(
            "<html><head><style>.a{}</style></head><body><font color=red>hi</font> there</body></html>",
        );
        let root = dom.root_element().unwrap();
        let body = dom.body();
        prep_document(&mut dom, &root, body.as_ref());
        let b = dom.body().unwrap();
        // style content gone, font replaced by span (text preserved)
        assert_eq!(text_content(&b), "hi there");
        assert!(get_elements_by_tag_name(&b, "style").is_empty());
        assert!(get_elements_by_tag_name(&b, "font").is_empty());
        assert_eq!(get_elements_by_tag_name(&b, "span").len(), 1);
    }

    // ---- _replaceBrs (Readability.js:696-750) ----

    #[test]
    fn replace_brs_double_br_becomes_p() {
        // Faithful trace of `Readability.js:696-750` on the doc-comment input
        // `<div>foo<br>bar<br> <br><br>abc</div>`:
        //   * br[0] (after "foo"): next is text "bar", not a <br> chain → skip.
        //   * br[1] (after "bar"): _nextNode skips the " " text to br[2]
        //     (chain); br[2], br[3] removed; <p> replaces br[1]. The walk then
        //     appends p.nextSibling nodes that are phrasing: the leftover " "
        //     text node, then "abc". The trailing-whitespace trim only strips
        //     from `p.lastChild` (the END), so the **leading " " survives**.
        // ⇒ p.textContent == " abc" (NOT "abc": the JS doc-comment
        //   "<p>abc</p>" is an *illustration*, not exact — the code does not
        //   left-trim). div text = "foo"+"bar"+" abc" = "foobar abc".
        let mut dom = Dom::parse("<div>foo<br>bar<br> <br><br>abc</div>");
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        replace_brs(&mut dom, &body);
        let ps = get_elements_by_tag_name(&div, "p");
        assert_eq!(ps.len(), 1, "exactly one <p> created");
        assert_eq!(
            text_content(&ps[0]),
            " abc",
            "leading ws NOT trimmed (faithful)"
        );
        assert_eq!(text_content(&div), "foobar abc");
    }

    #[test]
    fn replace_brs_single_br_untouched() {
        // A lone <br> (no chain) -> no <p>, nothing removed.
        let mut dom = Dom::parse("<div>a<br>b</div>");
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        replace_brs(&mut dom, &body);
        assert!(get_elements_by_tag_name(&div, "p").is_empty());
        assert_eq!(text_content(&div), "ab");
    }

    #[test]
    fn replace_brs_parent_p_retagged_to_div() {
        // <p>x<br><br>y</p> : the new <p> for "y" has parent <p> -> parent
        // retagged DIV. Net: text preserved, no nested <p> under <p>.
        let mut dom = Dom::parse("<p>x<br><br>y</p>");
        let body = dom.body().unwrap();
        replace_brs(&mut dom, &body);
        assert_eq!(text_content(&body), "xy");
        // the original <p> became a <div>
        assert!(
            !get_elements_by_tag_name(&body, "div").is_empty(),
            "parent <p> retagged to <div>"
        );
    }

    // ---- _clean (Readability.js:2182-2206) ----

    #[test]
    fn clean_removes_tag_and_keeps_video_embed() {
        let dom = Dom::parse(
            r#"<div><object data="https://www.youtube.com/embed/x">v</object><object>plain</object>txt</div>"#,
        );
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean(&div, "object");
        let objs = get_elements_by_tag_name(&div, "object");
        // youtube object kept; plain object removed
        assert_eq!(objs.len(), 1);
        assert_eq!(text_content(&objs[0]), "v");
    }

    #[test]
    fn clean_footer_is_removed_unconditionally() {
        let dom = Dom::parse("<div>body<footer>foot</footer></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean(&div, "footer");
        assert!(get_elements_by_tag_name(&div, "footer").is_empty());
        assert_eq!(text_content(&div), "body");
    }

    // ---- prep_article_stage1a: empty <p> removal (Readability.js:835-850) ----

    #[test]
    fn prep_article_removes_empty_p_keeps_content_and_img_p() {
        let dom = Dom::parse(r#"<div id=a><p>real text</p><p>   </p><p><img src=x></p></div>"#);
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        prep_article_stage1a(&art);
        let ps = get_elements_by_tag_name(&art, "p");
        // empty whitespace <p> removed; text <p> + img <p> kept (2)
        assert_eq!(ps.len(), 2);
        assert_eq!(text_content(&art), "real text");
    }

    #[test]
    fn prep_article_clean_set_removes_aside_link_object() {
        let dom = Dom::parse(r#"<div id=a>keep<aside>side</aside><link><object>o</object></div>"#);
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        prep_article_stage1a(&art);
        assert!(get_elements_by_tag_name(&art, "aside").is_empty());
        assert!(get_elements_by_tag_name(&art, "link").is_empty());
        assert!(get_elements_by_tag_name(&art, "object").is_empty());
        assert_eq!(text_content(&art), "keep");
    }

    // ---- _unwrapNoscriptImages (Readability.js:1892-1968) ----

    #[test]
    fn unwrap_noscript_images_keeps_img_with_src() {
        // Readability.js:1899-1904 — name in {src,srcset,data-src,data-srcset}
        // → KEEP (return early). An `<img src=...>` survives the cull.
        let dom = Dom::parse(r#"<html><body><div><img src="photo.jpg"></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        let imgs = get_elements_by_tag_name(&dom.document(), "img");
        assert_eq!(imgs.len(), 1, "img with src must be kept");
        assert_eq!(get_attribute(&imgs[0], "src").as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn unwrap_noscript_images_removes_img_without_attrs() {
        // Readability.js:1897-1912 — no qualifying attribute name, no value
        // matching `/\.(jpg|jpeg|png|webp)/i`. The empty `<img>` has zero
        // attributes ⇒ for-loop body never runs ⇒ fallthrough to remove().
        let dom = Dom::parse(r#"<html><body><div><img></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        let imgs = get_elements_by_tag_name(&dom.document(), "img");
        assert!(
            imgs.is_empty(),
            "placeholder img (no attributes) must be removed (Readability.js:1912)"
        );
    }

    #[test]
    fn unwrap_noscript_images_keeps_img_with_data_src() {
        // Readability.js:1902 — `case "data-src": return;` (KEEP). The
        // lazy-loaded placeholder pattern: `<img data-src="...">` survives.
        let dom = Dom::parse(r#"<html><body><div><img data-src="lazy.png"></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        let imgs = get_elements_by_tag_name(&dom.document(), "img");
        assert_eq!(imgs.len(), 1, "img with data-src must be kept");
    }

    #[test]
    fn unwrap_noscript_images_keeps_img_with_srcset() {
        // Readability.js:1901 — `case "srcset": return;` (KEEP).
        let dom =
            Dom::parse(r#"<html><body><div><img srcset="a.png 1x, b.png 2x"></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        assert_eq!(
            get_elements_by_tag_name(&dom.document(), "img").len(),
            1,
            "img with srcset must be kept"
        );
    }

    #[test]
    fn unwrap_noscript_images_keeps_img_with_data_srcset() {
        // Readability.js:1903 — `case "data-srcset": return;` (KEEP).
        let dom =
            Dom::parse(r#"<html><body><div><img data-srcset="x.jpg 1x"></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        assert_eq!(
            get_elements_by_tag_name(&dom.document(), "img").len(),
            1,
            "img with data-srcset must be kept"
        );
    }

    #[test]
    fn unwrap_noscript_images_keeps_img_with_image_extension_value() {
        // Readability.js:1907-1909 — attribute *value* contains `.png` etc.
        // ⇒ KEEP. Here the attribute is `alt`, not a source-named one, but
        // its value matches the image-extension regex.
        let dom = Dom::parse(r#"<html><body><div><img alt="photo.png"></div></body></html>"#);
        unwrap_noscript_images(&dom.document());
        assert_eq!(
            get_elements_by_tag_name(&dom.document(), "img").len(),
            1,
            "img with image-extension value must be kept (Readability.js:1907)"
        );
    }

    #[test]
    fn unwrap_noscript_images_removes_img_with_non_image_attrs() {
        // Readability.js:1899-1909 — none of the attribute *names* are in the
        // src-name set AND none of the *values* match `.(jpg|jpeg|png|webp)`.
        // Fallthrough to `:1912` remove.
        let dom = Dom::parse(
            r#"<html><body><div><img class="foo" id="bar" alt="text"></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        assert!(
            get_elements_by_tag_name(&dom.document(), "img").is_empty(),
            "img with only non-source attrs must be removed (Readability.js:1912)"
        );
    }

    #[test]
    fn unwrap_noscript_images_unwraps_prev_img_with_noscript_img() {
        // The full :1916-1967 second-loop trace.
        //
        // Input: <div><img class="lazy"><noscript><img src="real.jpg"></noscript></div>
        //
        // Pass 1 (:1895-1913): the bare `<img class="lazy">` has only a
        //   `class` attribute (no src/srcset/data-src/data-srcset, no value
        //   matching the image-extension regex). Per :1912 the JS REMOVES
        //   it. So at the start of pass 2 the placeholder is already gone.
        //
        //   The noscript's previousElementSibling is then nothing ⇒ the
        //   second-loop guard at :1934 `if (prevElement && _isSingleImage...)`
        //   fails ⇒ no unwrap. This is the *faithful* trace — the placeholder
        //   img must use a src-like attribute or an image-extension value
        //   to survive pass 1 and be replaced. We verify the structural
        //   outcome.
        let dom = Dom::parse(
            r#"<html><body><div><img class="lazy"><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        // Lazy placeholder removed by pass 1.
        let imgs = get_elements_by_tag_name(&dom.document(), "img");
        // The `<img src="real.jpg">` INSIDE the noscript still has src — it
        // survives pass 1 (it qualifies on src). Pass 2 sees the noscript
        // (single-image qualifies) but its prevElement is now gone (the
        // bare placeholder was removed), so no replacement happens. The
        // `<img src="real.jpg">` remains inside the noscript.
        assert_eq!(imgs.len(), 1);
        assert_eq!(get_attribute(&imgs[0], "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn unwrap_noscript_images_unwraps_prev_img_when_placeholder_survives_pass1() {
        // The unwrap-fires case: prev `<img>` has a `data-src` attribute, so
        // pass 1 KEEPS it (Readability.js:1902). Pass 2 sees the noscript
        // (single-image) AND the prev img (single-image) ⇒ copies the
        // src-like attributes from prevImg onto the noscript's img, then
        // replaces prevImg with that img.
        //
        // Input:
        //   <div><img data-src="lazy.png" class="placeholder">
        //        <noscript><img src="real.jpg"></noscript></div>
        //
        // Trace of :1941-1963 attribute copy onto newImg ("real.jpg"):
        //   - "data-src"="lazy.png": value matches `.png` ⇒ source-ish.
        //     newImg has no "data-src" ⇒ set newImg["data-src"]="lazy.png".
        //   - "class"="placeholder": value doesn't match img regex, name
        //     not in {src,srcset} ⇒ skip.
        // Then :1965 replaces prevImg with newImg in the DOM.
        let dom = Dom::parse(
            r#"<html><body><div><img data-src="lazy.png" class="placeholder"><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        // After replaceChild :1965, prevImg is detached, newImg ("real.jpg")
        // is in its place. The noscript still exists (it's removed later by
        // _removeScripts). The doc-tree img count: newImg (in the div) PLUS
        // the original img inside noscript (still there since replaceChild
        // moves newImg out of noscript, but the noscript's children are
        // updated — actually replaceChild on jsdom moves newImg, so it's
        // GONE from inside noscript). Verify by checking the div's img
        // directly.
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let div_imgs = get_elements_by_tag_name(&div, "img");
        // Exactly one img directly under div (the moved newImg);
        // plus, since `noscript` is still in the tree (until _removeScripts),
        // the noscript element is present but its `<img>` child was moved.
        // `getElementsByTagName` is descendants in document order — so:
        // - if there's still an img inside noscript, count == 2;
        // - if the noscript's img was moved, count == 1.
        // The JS semantics of replaceChild: the inserted node is detached
        // from its prior parent first (DOM move semantics). So count == 1.
        assert_eq!(
            div_imgs.len(),
            1,
            "exactly one img under div after unwrap (newImg moved out of noscript)"
        );
        let new_img = &div_imgs[0];
        assert_eq!(
            get_attribute(new_img, "src").as_deref(),
            Some("real.jpg"),
            "the noscript's <img src=\"real.jpg\"> is now in the div"
        );
        assert_eq!(
            get_attribute(new_img, "data-src").as_deref(),
            Some("lazy.png"),
            "data-src attribute copied from prevImg (Readability.js:1961)"
        );
    }

    #[test]
    fn unwrap_noscript_images_copies_attribute_with_data_old_prefix_when_collision() {
        // Readability.js:1957-1959 — if newImg already has an attribute by
        // the same name, the destination name becomes `data-old-<name>`.
        //
        // Prev img has `src="placeholder.png"` (survives pass 1 by name).
        // Noscript img has `src="real.jpg"` (survives pass 1 by name and
        // by value).
        // Pass 2 copies prev's "src"="placeholder.png" onto newImg which
        // ALREADY has src="real.jpg" ⇒ destination becomes "data-old-src".
        let dom = Dom::parse(
            r#"<html><body><div><img src="placeholder.png"><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(imgs.len(), 1, "exactly one img under div");
        let img = &imgs[0];
        assert_eq!(
            get_attribute(img, "src").as_deref(),
            Some("real.jpg"),
            "newImg retains its own src"
        );
        assert_eq!(
            get_attribute(img, "data-old-src").as_deref(),
            Some("placeholder.png"),
            "prev's src was copied to data-old-src (Readability.js:1957-1959)"
        );
    }

    #[test]
    fn unwrap_noscript_images_skips_noscript_without_single_image() {
        // Readability.js:1919-1921 — `_isSingleImage(noscript)` must be true.
        // A noscript with text alongside an img fails (textContent.trim() != "").
        let dom = Dom::parse(
            r#"<html><body><div><img data-src="x.png"><noscript>extra text<img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        // Prev img was kept (data-src). Noscript fails _isSingleImage ⇒ no
        // replacement. Both the prev img and the noscript subtree's img are
        // present.
        let div_imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(div_imgs.len(), 2);
    }

    #[test]
    fn unwrap_noscript_images_skips_when_prev_not_single_image() {
        // Readability.js:1934 — `prevElement && _isSingleImage(prevElement)`.
        // Prev is a <div> with text → _isSingleImage returns false (children
        // length is 0, textContent.trim() != "") ⇒ no replacement.
        let dom = Dom::parse(
            r#"<html><body><div><div>some text</div><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        // No replacement; the noscript content remains in place.
        let body = dom.body().unwrap();
        let outer_div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&outer_div, "img");
        assert_eq!(imgs.len(), 1, "real img still inside noscript");
    }

    #[test]
    fn unwrap_noscript_images_clean_conditionally_img_count_pin() {
        // FIX-1 pin: the placeholder-img cull (`Readability.js:1895-1913`)
        // changes `_cleanConditionally`'s img count (`Readability.js:2498`),
        // which can flip a shadiness verdict. This test verifies the
        // STRUCTURAL placeholder removal — the upstream effect on
        // _cleanConditionally is then matched-by-construction.
        //
        // Construct a fixture where the SAME element has:
        //   - one real `<img src="real.jpg">` (kept)
        //   - one placeholder `<img>` with non-source attrs (removed)
        // After unwrap_noscript_images, the img-count is 1, not 2. The
        // downstream `_cleanConditionally` sees 1 img (whichever shadiness
        // verdict that produces).
        let dom = Dom::parse(
            r#"<html><body><div id="t"><img src="real.jpg"><img class="placeholder" alt="text"></div></body></html>"#,
        );
        let body = dom.body().unwrap();
        let target = get_elements_by_tag_name(&body, "div")[0].clone();
        // Before: 2 imgs.
        assert_eq!(get_elements_by_tag_name(&target, "img").len(), 2);
        unwrap_noscript_images(&dom.document());
        // After: only the real img survives — img count flips from 2 → 1,
        // which is the upstream `_cleanConditionally` img-count input.
        let imgs_after = get_elements_by_tag_name(&target, "img");
        assert_eq!(imgs_after.len(), 1);
        assert_eq!(
            get_attribute(&imgs_after[0], "src").as_deref(),
            Some("real.jpg")
        );
    }

    #[test]
    fn is_single_image_recursive_descent() {
        // Readability.js:1871-1882 trace.
        //   <a><img src=x></a>  : a has 1 child (img), textContent.trim()=="" ⇒
        //     recurse into img ⇒ tagName=="IMG" ⇒ true.
        //   <a>txt<img></a>     : a's textContent.trim()=="txt" ⇒ false.
        //   <a><b><img></b></a> : a → b → img → IMG ⇒ true.
        //   <img>               : direct IMG ⇒ true.
        //   <div></div>         : children length 0 ⇒ false.
        let dom1 = Dom::parse(r#"<a><img src=x></a>"#);
        let a = get_elements_by_tag_name(&dom1.body().unwrap(), "a")[0].clone();
        assert!(is_single_image(&a));

        let dom2 = Dom::parse(r#"<a>txt<img src=x></a>"#);
        let a2 = get_elements_by_tag_name(&dom2.body().unwrap(), "a")[0].clone();
        assert!(!is_single_image(&a2));

        let dom3 = Dom::parse(r#"<a><b><img src=x></b></a>"#);
        let a3 = get_elements_by_tag_name(&dom3.body().unwrap(), "a")[0].clone();
        assert!(is_single_image(&a3));

        let dom4 = Dom::parse(r#"<div><img src=x></div>"#);
        let img = get_elements_by_tag_name(&dom4.body().unwrap(), "img")[0].clone();
        assert!(is_single_image(&img));

        let dom5 = Dom::parse(r#"<div></div>"#);
        let div = get_elements_by_tag_name(&dom5.body().unwrap(), "div")[0].clone();
        assert!(!is_single_image(&div));
    }
}
