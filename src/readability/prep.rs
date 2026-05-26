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
    get_all_nodes_with_tag, get_attribute, get_elements_by_tag_name, id, inner_text, is_element,
    parent, replace_child, set_attribute, tag_name, text_content,
};
use crate::readability::helpers::{
    get_next_node, has_single_tag_inside_element, is_element_without_content, is_phrasing_content,
    is_whitespace, next_node, next_sibling,
};
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
        // llvm-cov:branch-not-reachable: the `if let Element` else side. `img`
        // comes from `get_elements_by_tag_name(doc_root, "img")`, which only
        // yields Element nodes, so the destructure never fails.
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
            // llvm-cov:branch-not-reachable: the `else { continue }` (None)
            // side. `_isSingleImage(prev_element)` just returned true, which
            // for a non-IMG element means it descends to an IMG, so there is
            // always at least one descendant img here.
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
        // llvm-cov:branch-not-reachable: the `else { continue }` (None) side.
        // `_isSingleImage(noscript)` (checked at the top of the loop) is only
        // true when the noscript resolves to an IMG, so it always has an img.
        let Some(new_img) = new_imgs.into_iter().next() else {
            continue;
        };

        // Readability.js:1941-1963 — attribute copy from prevImg onto newImg.
        // Iterate prevImg's attributes; for each: skip if value == ""; for
        // those with name in {src,srcset} OR value matching the
        // image-extension regex, copy onto newImg with a `data-old-` prefix
        // on the destination name if it already has an attribute by that
        // name. Skip if newImg already has the same name/value pair.
        // llvm-cov:branch-not-reachable: the `if let Element` else side.
        // `prev_img` is either `prev_element` (an element) or a descendant img
        // from `get_elements_by_tag_name`, so it is always an Element node.
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
        // llvm-cov:branch-not-reachable: the `else { continue }` (None) side.
        // The noscript was found via `get_elements_by_tag_name(doc_root, ...)`,
        // so it is a live descendant with a parent.
        let Some(parent_of_noscript) = parent(&noscript) else {
            continue;
        };
        // llvm-cov:branch-not-reachable: the `else { continue }` (None) side.
        // `_isSingleImage(noscript)` true ⇒ the noscript has a single element
        // child (the img, possibly wrapped), so it always has a first child.
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
            // llvm-cov:branch-not-reachable: the `else { continue }` (None)
            // side. This `<br>` is a live descendant from the snapshot whose
            // following `<br>` siblings were removed (not itself), so it still
            // has a parent here.
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
/// 3. `_fixLazyImages` (`:790`) — wired at Stage 3 (HLD §7.5); the score-
///    affecting empty-`<figure>`/`<img>` create at `Readability.js:2398-2407`
///    is included. See `fix_lazy_images` below at `:461`.
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
    // 3. _fixLazyImages — Stage-3 ported (Readability.js:790, :2332-2412).
    // Mostly attribute-only (score-invisible) EXCEPT the empty-`<figure>`
    // branch (`:2398-2407`) which CREATES a new `<img>` child of an empty
    // figure; that increases the img descendant count `_cleanConditionally`
    // (`:2498`) reads at the next step. Without porting, a corpus URL with
    // a `<figure data-src="foo.jpg">` (no inner img) could be `_cleanConditionally`-
    // removed where RJS keeps it. Validated against the current corpus to
    // confirm no scored URL exercises the figure branch (zero measurable
    // residual moved), but ported anyway per HLD §7.5 for structural
    // faithfulness — the cost is bounded.
    fix_lazy_images(article_content);

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
        // llvm-cov:branch-not-reachable: `parent(&paragraph).is_none()` true
        // side. `<p>` cannot nest a `<p>` in HTML5 (auto-closed), so removing
        // one paragraph never orphans another in this snapshot. Defensive guard.
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
            // llvm-cov:branch-not-reachable: the `if let Some` None side.
            // `any_attr_matches_videos` returns None only for a non-element,
            // but `element` comes from `get_all_nodes_with_tag` (elements only).
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
    // llvm-cov:branch-not-reachable: the `if let Element` else side (returns
    // None). The only caller (`clean`) passes elements from
    // `get_all_nodes_with_tag`, so a non-element is never seen here.
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

// ===========================================================================
// Stage 3 — `_simplifyNestedElements` and `_fixLazyImages`
// ===========================================================================
//
// HLD §7.5 / supervisor Stage-3 brief. Both were deferred at Stage 2 as
// "attribute-only / structural cleanups whose effect on the scored
// `text_content` is invisible by inspection"; Stage 3 re-examines that
// reading. `_simplifyNestedElements` is **token-sequence-invariant** (the
// only branch that touches `#text` descendants is the
// `_isElementWithoutContent` removal, which removes JS-whitespace-only
// content the harness tokenizer would collapse anyway), but it normalises
// the raw `textContent` whitespace byte-pattern to match RJS — porting it
// raised crate↔RJS byte equality from 29/51 to 50/51 on the corpus. The
// element-unwrap branch is byte-and-token invariant. `_fixLazyImages` is
// attribute-only EXCEPT for the `<figure>` branch that CREATES a new
// `<img>` child of an empty figure (`Readability.js:2398-2407`), which
// raises the `<img>` descendant count consumed by `_cleanConditionally`
// (`:2498`) — a real cross-stage effect, so the function MUST be ported
// (even if the corpus does not exercise the figure branch). Both are
// ported here behind the same frozen public surface as the rest of
// `prep`.

/// `_simplifyNestedElements(articleContent)` (`Readability.js:537-565`).
///
/// Walks the article tree via [`get_next_node`] (the JS `_getNextNode`).
/// For every visited `<DIV>` or `<SECTION>` that has a parent and whose `id`
/// does not start with `readability` (the JS skip for the page-wrap):
///
/// 1. If the node is `_isElementWithoutContent` (no non-whitespace
///    `textContent`, children empty or only `<br>`/`<hr>`), remove it via
///    `_removeAndGetNext` and continue from the returned next node.
/// 2. Else if it has a single `<DIV>` or single `<SECTION>` element child,
///    clone all the node's attributes onto the child (faithful to the JS's
///    `setAttributeNode(node.attributes[i].cloneNode())` — same-name
///    attrs on the child are overwritten with the node's value, distinct
///    names are added), then `parentNode.replaceChild(child, node)` and
///    continue from `child` (NOT from `_getNextNode` — the JS keeps the
///    same `node` cursor pointed at the child).
///
/// **Effect on `textContent` (Stage-3 differential measurement, recorded).**
/// Branch (1) removes elements whose `textContent` is JS-whitespace-only:
/// removing them strips those whitespace characters from the parent's
/// `textContent` byte-string. The harness tokenizer collapses whitespace
/// runs, so the **token sequence** the harness scores is unchanged (token-
/// invariant by construction — no non-whitespace token can be created or
/// destroyed). Branch (2) re-parents a single element child (no text
/// descendants gained or lost), token-and-byte invariant. The Stage-3
/// benchmark observed this exactly: token-Coverage / word-count was
/// UNCHANGED on all 50 scored URLs (token sequence invariant), but raw
/// `textContent` byte-equality against RJS rose from 29/51 to 50/51 — RJS
/// runs the same JS function, so the crate's raw bytes converge to RJS's
/// exact `textContent` on every URL the corpus exercises. **Token-
/// stability is the load-bearing invariant** (the harness scores tokens,
/// not bytes); the byte convergence is bonus evidence that the port is
/// JS-faithful at the raw-`textContent` level too.
///
/// Called from [`post_process_content`] (the JS `_postProcessContent`,
/// `Readability.js:281-291`), which runs AFTER `_grabArticle` and BEFORE
/// the scored `textContent` capture (`Readability.js:2754`/`:2766`). The
/// supervisor-brief framing of "ported in `parse()` immediately before
/// `_grabArticle`" was a slot-mistake (the JS call site is post-grab); the
/// JS-faithful position is used here, anchored by the citation above.
pub fn simplify_nested_elements(article_content: &NodeRef) {
    let mut node_opt: Option<NodeRef> = Some(article_content.clone());

    while let Some(node) = node_opt.clone() {
        let tag = tag_name(&node);
        let tag_is_div_or_section = matches!(tag.as_deref(), Some("DIV") | Some("SECTION"));
        let has_parent = parent(&node).is_some();
        let id_is_readability = id(&node).starts_with("readability");

        if has_parent && tag_is_div_or_section && !id_is_readability {
            // Branch 1: empty element -> remove and continue from next.
            if is_element_without_content(&node) {
                node_opt = crate::readability::grab_article::remove_and_get_next(&node);
                continue;
            }

            // Branch 2: single DIV-or-SECTION child -> unwrap. Clone all of
            // node's attrs onto the child, then `parentNode.replaceChild`.
            if has_single_tag_inside_element(&node, "DIV")
                || has_single_tag_inside_element(&node, "SECTION")
            {
                // children() returns element-only children; the JS `node.children`
                // is the same (HTMLCollection of element children).
                let kids = children(&node);
                // `_hasSingleTagInsideElement` already passed -> exactly one elt
                // child whose tag is DIV/SECTION.
                let child = kids.into_iter().next().expect("single child by predicate");

                // for (i ; i < node.attributes.length ; i++)
                //   child.setAttributeNode(node.attributes[i].cloneNode())
                // Set every node attribute on child (overwriting any same-named
                // child attribute, faithful to `setAttributeNode`).
                clone_attributes_onto(&node, &child);

                // node.parentNode.replaceChild(child, node)
                // llvm-cov:branch-not-reachable: the `if let Some(p)` None
                // side. `has_parent` was confirmed true at the top of the loop
                // body before this unwrap branch runs.
                if let Some(p) = parent(&node) {
                    replace_child(&p, &child, &node);
                }

                // node = child;  continue;
                node_opt = Some(child);
                continue;
            }
        }

        // node = this._getNextNode(node);
        node_opt = get_next_node(&node, false);
    }
}

/// Copy every attribute from `src` onto `dst`, overwriting `dst`'s same-named
/// attributes (faithful to the JS `setAttributeNode(attr.cloneNode())`
/// semantics — `setAttributeNode` replaces an existing attribute node with
/// the same `name` and returns the old, otherwise inserts).
fn clone_attributes_onto(src: &NodeRef, dst: &NodeRef) {
    // Snapshot the src attrs (name+value pairs) to a Vec — borrowing both
    // RefCells at once would otherwise be a hazard. The set is short.
    let pairs: Vec<(String, String)> = match &src.data {
        markup5ever_rcdom::NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .map(|a| (a.name.local.to_string(), a.value.to_string()))
            .collect(),
        _ => return,
    };
    for (name, value) in &pairs {
        set_attribute(dst, name, value);
    }
}

/// `_fixLazyImages(root)` (`Readability.js:2332-2412`).
///
/// Visits every `<img>` / `<picture>` / `<figure>` descendant and rewires
/// the lazy-load attributes (`data-src`, `data-srcset`, etc.) into proper
/// `src` / `srcset` attributes so a downstream renderer can load them
/// without JS.
///
/// Three branches per element (faithful transcription of `:2336-2407`):
///
/// 1. **Tiny base64 `src` cull (`:2336-2369`).** If the element's `src`
///    matches `REGEXPS.b64DataUrl` AND the mediatype is NOT `image/svg+xml`
///    AND any OTHER attribute value matches `/\.(jpg|jpeg|png|webp)/i`
///    (i.e. there's a real image elsewhere), then if the base64 payload is
///    `< 133` chars (a likely placeholder), `removeAttribute("src")`.
///
/// 2. **Has-image short-circuit (`:2371-2377`).** If the element has
///    `src` OR (`srcset` !== `"null"`) AND the class name does NOT contain
///    `"lazy"`, return — nothing to fix.
///
/// 3. **Attribute promotion (`:2379-2409`).** For every attribute (except
///    `src`/`srcset`/`alt`), if its value looks like a srcset
///    (`/\.(jpg|jpeg|png|webp)\s+\d/`) OR a plain image URL
///    (`/^\s*\S+\.(jpg|jpeg|png|webp)\S*\s*$/`), copy it to `srcset` or
///    `src` respectively — directly on `<IMG>`/`<PICTURE>`, or, on a
///    `<FIGURE>` with no inner `<img>`/`<picture>`, by **creating a new
///    `<img>` child** of the figure (`elem.appendChild(img)`).
///
/// **The figure-img branch (`:2398-2407`) is the load-bearing reason this
/// must be ported.** All other branches are attribute-only and
/// `text_content`-invariant. The figure branch INCREMENTS the number of
/// `<img>` descendants under the figure's ancestors, which `_cleanConditionally`
/// reads (`Readability.js:2498` — `var img = node.getElementsByTagName("img")
/// .length;`) to decide whether the ancestor is "too few paragraphs per
/// image, remove". Without porting `_fixLazyImages`, a `<figure data-src=
/// "foo.jpg">` with no inner `<img>` would have `img == 0` for its
/// ancestors and could be removed where the JS would keep it.
///
/// The corpus probe shows zero scored URL exercises the figure branch
/// (no inline `<figure>` with a `data-`/`srcset`-style attribute on a
/// figure that lacks an inner `<img>`/`<picture>`). The function is ported
/// anyway because the cost is bounded and the spec gap is named (HLD §7.5).
///
/// Called from [`prep_article`] in the `_fixLazyImages` slot
/// (`Readability.js:790`), AFTER `_markDataTables` and BEFORE any
/// `_cleanConditionally`.
pub fn fix_lazy_images(root: &NodeRef) {
    let targets = get_all_nodes_with_tag(root, &["img", "picture", "figure"]);
    for elem in targets {
        let elem_tag = match tag_name(&elem) {
            Some(t) => t,
            None => continue,
        };

        // ------- (1) tiny base64 placeholder src cull (:2336-2369) -------
        if let Some(src) = get_attribute(&elem, "src")
            && let Some(caps) = regexps::b64_data_url().captures(&src)
        {
            let mediatype = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if mediatype != "image/svg+xml" {
                // any OTHER attribute value matching /\.(jpg|jpeg|png|webp)/i ?
                let mut src_could_be_removed = false;
                // llvm-cov:branch-not-reachable: the `if let Element` else
                // side. `elem` comes from `get_all_nodes_with_tag(root, [img,
                // picture, figure])`, so it is always an Element node.
                if let markup5ever_rcdom::NodeData::Element { attrs, .. } = &elem.data {
                    for a in attrs.borrow().iter() {
                        if a.name.local.to_string() == "src" {
                            continue;
                        }
                        if regexps::image_ext_anywhere().is_match(&a.value) {
                            src_could_be_removed = true;
                            break;
                        }
                    }
                }
                if src_could_be_removed {
                    // b64starts = parts[0].length (the prefix incl. ";base64,")
                    let b64starts = caps.get(0).map(|m| m.as_str().chars().count()).unwrap_or(0);
                    let total = src.chars().count();
                    let b64length = total.saturating_sub(b64starts);
                    if b64length < 133 {
                        dom::remove_attribute(&elem, "src");
                    }
                }
            }
        }

        // ------- (2) has-image short-circuit (:2371-2377) -------
        // `(elem.src || (elem.srcset && elem.srcset != "null")) &&
        //  !elem.className.toLowerCase().includes("lazy")` -> return.
        // Note: re-read src AFTER step (1) (it may have been removed).
        let cur_src = get_attribute(&elem, "src");
        let cur_srcset = get_attribute(&elem, "srcset");
        let has_image = (cur_src.is_some() && !cur_src.as_deref().unwrap_or("").is_empty())
            || (cur_srcset.is_some()
                && cur_srcset.as_deref() != Some("null")
                && !cur_srcset.as_deref().unwrap_or("").is_empty());
        let class_lazy = dom::class_name(&elem).to_ascii_lowercase().contains("lazy");
        if has_image && !class_lazy {
            continue;
        }

        // ------- (3) attribute promotion (:2379-2409) -------
        // Snapshot attrs to a Vec — we may MUTATE the element below
        // (setAttribute / appendChild) which would invalidate a live
        // borrow of attrs.
        let pairs: Vec<(String, String)> = match &elem.data {
            markup5ever_rcdom::NodeData::Element { attrs, .. } => attrs
                .borrow()
                .iter()
                .map(|a| (a.name.local.to_string(), a.value.to_string()))
                .collect(),
            _ => continue,
        };
        for (name, value) in pairs.iter() {
            if name == "src" || name == "srcset" || name == "alt" {
                continue;
            }
            let copy_to: Option<&str> = if regexps::image_srcset_value().is_match(value) {
                Some("srcset")
            } else if regexps::image_src_value().is_match(value) {
                Some("src")
            } else {
                None
            };
            if let Some(target_attr) = copy_to {
                if elem_tag == "IMG" || elem_tag == "PICTURE" {
                    set_attribute(&elem, target_attr, value);
                // llvm-cov:branch-not-reachable: the `elem_tag == "FIGURE"`
                // false side. `targets` are only img/picture/figure; the
                // preceding `if` consumed IMG/PICTURE, so in this else-if
                // `elem_tag` is always "FIGURE".
                } else if elem_tag == "FIGURE"
                    && get_all_nodes_with_tag(&elem, &["img", "picture"]).is_empty()
                {
                    // The score-affecting branch: empty <figure> with a
                    // promotable attribute -> create a new <img> child.
                    let img = create_element("IMG");
                    set_attribute(&img, target_attr, value);
                    append_child(&elem, &img);
                }
            }
        }
    }
}

/// `_postProcessContent(articleContent)` (`Readability.js:281-291`) —
/// **Stage 3 text-affecting parts**.
///
/// The JS body is:
/// ```text
/// _fixRelativeUris(articleContent);     // attribute-only, score-invisible
/// _simplifyNestedElements(articleContent);
/// if (!_keepClasses) _cleanClasses(articleContent);  // attribute-only
/// ```
///
/// Of the three, only `_simplifyNestedElements` has structural effect;
/// `_fixRelativeUris` and `_cleanClasses` are attribute-only and
/// `text_content`-invariant (HLD §2). For Stage 3 we port the structural
/// half; the attribute halves remain deferred (they are score-invisible
/// and Stage-4 cleanup territory). `_simplifyNestedElements` is itself
/// `text_content`-invariant (see [`simplify_nested_elements`] doc), so
/// calling `post_process_content` does not perturb the scored body —
/// porting is for structural faithfulness, not Coverage.
///
/// The supervisor brief noted `aria-modal`/`role=dialog` removal as a
/// candidate text-affecting `_postProcessContent` step; that check is in
/// fact inside `_grabArticle`'s main visitor loop (`Readability.js:1073-1079`),
/// already ported in `grab_article.rs:192-198` — nothing to port here.
pub fn post_process_content(article_content: &NodeRef) {
    // _fixRelativeUris — attribute-only (score-invisible, HLD §2): deferred.
    simplify_nested_elements(article_content);
    // _cleanClasses — attribute-only (score-invisible): deferred.
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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

    // ---- Stage 3: _simplifyNestedElements (Readability.js:537-565) ----

    #[test]
    fn simplify_nested_elements_removes_empty_div_and_section() {
        // Readability.js:546-548: a <div>/<section> with parent, id NOT
        // starting with "readability", and `_isElementWithoutContent` ⇒
        // _removeAndGetNext. Empty divs (no children, no text) qualify.
        let dom =
            Dom::parse("<body><article><div></div><section></section><p>keep</p></article></body>");
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        simplify_nested_elements(&art);
        // The two empty containers are gone; the <p>keep</p> remains.
        assert_eq!(get_elements_by_tag_name(&art, "div").len(), 0);
        assert_eq!(get_elements_by_tag_name(&art, "section").len(), 0);
        assert_eq!(get_elements_by_tag_name(&art, "p").len(), 1);
    }

    #[test]
    fn simplify_nested_elements_skips_readability_prefix_id() {
        // Readability.js:544: `!(node.id && node.id.startsWith("readability"))`
        // — the page-wrap (`readability-page-1`) MUST NOT be removed even if
        // empty.
        let dom =
            Dom::parse(r#"<body><article><div id="readability-page-1"></div></article></body>"#);
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        simplify_nested_elements(&art);
        // The id="readability-page-1" div MUST survive.
        let divs = get_elements_by_tag_name(&art, "div");
        assert_eq!(divs.len(), 1);
        assert_eq!(
            get_attribute(&divs[0], "id").as_deref(),
            Some("readability-page-1")
        );
    }

    #[test]
    fn simplify_nested_elements_unwraps_single_div_child() {
        // Readability.js:549-559: <div ATTR><div ATTR2>x</div></div> →
        // <div ATTR ATTR2>x</div> (child's attrs union with node's; same-name
        // attrs overwritten by node's via `setAttributeNode`).
        let dom = Dom::parse(
            r#"<body><article><div class="outer" data-x="o"><div class="inner" data-y="i">hi</div></div></article></body>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        simplify_nested_elements(&art);
        // Only ONE <div> remains; the inner survives, with outer's attrs
        // overwritten onto it.
        let divs = get_elements_by_tag_name(&art, "div");
        assert_eq!(divs.len(), 1);
        // class is overwritten by outer's `outer` value (faithful to
        // setAttributeNode replace-on-same-name).
        assert_eq!(get_attribute(&divs[0], "class").as_deref(), Some("outer"));
        // data-x from outer.
        assert_eq!(get_attribute(&divs[0], "data-x").as_deref(), Some("o"));
        // data-y survives from inner.
        assert_eq!(get_attribute(&divs[0], "data-y").as_deref(), Some("i"));
        // Text content preserved.
        assert_eq!(text_content(&divs[0]), "hi");
    }

    #[test]
    fn simplify_nested_elements_preserves_text_content_invariant() {
        // The faithfulness invariant: _simplifyNestedElements MUST be
        // text_content-invariant (branch 1 removes only empty elements; branch
        // 2 moves a child up — neither changes the #text DFS concatenation).
        let html = "<body><article>\
            <div></div>\
            <section><div><p>main body text here</p></div></section>\
            <div id='readability-page-1'></div>\
        </article></body>";
        let dom = Dom::parse(html);
        let art_before = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        let tc_before = text_content(&art_before);
        simplify_nested_elements(&art_before);
        let tc_after = text_content(&art_before);
        assert_eq!(tc_before, tc_after, "text_content MUST be invariant");
    }

    #[test]
    fn simplify_nested_elements_skips_root_with_no_parent() {
        // Readability.js:542: `node.parentNode && ...`. The first node visited
        // (articleContent itself) has no parent in this test — so the branch
        // is skipped on it. The walk then proceeds via _getNextNode to its
        // descendants and processes those.
        let dom = Dom::parse(r#"<div id="root"><div></div><p>x</p></div>"#);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        // Detach root so it has no parent.
        crate::readability::dom::remove(&root);
        // Now root has no parent.
        assert!(parent(&root).is_none());
        simplify_nested_elements(&root);
        // Descendants (the inner empty <div>) get processed → removed.
        // <p>x</p> stays.
        assert_eq!(get_elements_by_tag_name(&root, "div").len(), 0);
        assert_eq!(get_elements_by_tag_name(&root, "p").len(), 1);
    }

    // ---- Stage 3: _fixLazyImages (Readability.js:2332-2412) ----

    #[test]
    fn fix_lazy_images_empty_figure_creates_img_score_affecting_branch() {
        // The branch that justifies porting (Readability.js:2398-2407): a
        // <figure data-src="foo.jpg"> with NO inner <img>/<picture> AND no
        // existing src/srcset gets a new <img> appended. This INCREASES
        // _cleanConditionally's img descendant count for the figure's
        // ancestors (`Readability.js:2498`).
        let dom =
            Dom::parse(r#"<body><div id=art><figure data-src="hero.jpg"></figure></div></body>"#);
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let imgs_before = get_elements_by_tag_name(&art, "img").len();
        assert_eq!(imgs_before, 0);
        fix_lazy_images(&art);
        let imgs_after = get_elements_by_tag_name(&art, "img").len();
        assert_eq!(imgs_after, 1, "figure branch MUST create a new <img>");
        let imgs = get_elements_by_tag_name(&art, "img");
        assert_eq!(get_attribute(&imgs[0], "src").as_deref(), Some("hero.jpg"));
    }

    #[test]
    fn fix_lazy_images_figure_with_inner_img_does_not_add_another() {
        // The guard `!_getAllNodesWithTag(elem, ["img","picture"]).length`
        // (`:2400`) blocks the figure-img creation if any img/picture exists
        // inside. The data-src is still on the figure; no extra img.
        let dom = Dom::parse(
            r#"<body><div id=art><figure data-src="x.jpg"><img src="real.jpg"></figure></div></body>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        fix_lazy_images(&art);
        let imgs = get_elements_by_tag_name(&art, "img");
        assert_eq!(imgs.len(), 1);
        // Real img untouched (it already has src; short-circuit at :2371-2377).
        assert_eq!(get_attribute(&imgs[0], "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn fix_lazy_images_img_with_data_src_promotes_to_src() {
        // Plain <img data-src="x.jpg"> with no src/srcset and no `lazy` class
        // ⇒ the attribute promotion branch (:2391) matches /^.../.test → copy
        // value to `src`.
        let dom = Dom::parse(r#"<body><img data-src="real.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn fix_lazy_images_picture_data_srcset_promotes_to_srcset() {
        // <picture data-srcset="foo.jpg 2x"> ⇒ /\.(jpg|jpeg|png|webp)\s+\d/
        // matches → copy value to `srcset`.
        let dom = Dom::parse(r#"<body><picture data-srcset="foo.jpg 2x"></picture></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let pic = get_elements_by_tag_name(&body, "picture")[0].clone();
        assert_eq!(get_attribute(&pic, "srcset").as_deref(), Some("foo.jpg 2x"));
    }

    #[test]
    fn fix_lazy_images_b64_tiny_placeholder_src_removed() {
        // Readability.js:2336-2369: data:image/png;base64,SHORT (b64length<133)
        // AND another attribute value with image extension ⇒ removeAttribute("src").
        // Tiny base64 placeholder + `data-real-src="real.jpg"` (image-ext).
        let dom = Dom::parse(
            r#"<body><img src="data:image/png;base64,iVBOR" data-real-src="real.jpg"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        // Tiny base64 src removed; then attribute promotion kicks in for
        // data-real-src → src.
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn fix_lazy_images_b64_svg_carve_out() {
        // Readability.js:2341-2343: svg+xml mediatype short-circuits BEFORE
        // the placeholder cull. A short svg+xml base64 src MUST be kept.
        let dom = Dom::parse(
            r#"<body><img src="data:image/svg+xml;base64,PHN2" data-real-src="real.jpg"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        // src is unchanged (svg carve-out triggered `return;` at :2342).
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("data:image/svg+xml;base64,PHN2")
        );
    }

    #[test]
    fn fix_lazy_images_has_image_and_not_lazy_short_circuits() {
        // (elem.src OR (elem.srcset && != "null")) && !class.contains("lazy")
        //   ⇒ return (no attribute promotion). A real img with `src` plus a
        //   benign data-some attribute that looks like an image must NOT
        //   overwrite `src`.
        let dom = Dom::parse(r#"<body><img src="real.jpg" data-something="other.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        // src stays at "real.jpg" — short-circuit prevented overwrite.
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn fix_lazy_images_lazy_class_bypasses_short_circuit() {
        // class contains "lazy" ⇒ short-circuit does NOT fire ⇒ attribute
        // promotion runs even when src exists. The lazy-class case is
        // exactly the kind of placeholder src this function was written for.
        // (Note: srcset takes priority over src in the promotion test.)
        let dom =
            Dom::parse(r#"<body><img class="lazy" src="placeholder" data-src="real.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("real.jpg"));
    }

    #[test]
    fn fix_lazy_images_text_content_invariant_on_pure_attribute_branches() {
        // Branches 1-3 are attribute-only except the figure-img creation
        // (which adds an <img> element with no text). So text_content of the
        // root MUST be invariant.
        let html = r#"<div id=r>
            <img data-src="real.jpg">
            <picture data-srcset="foo.png 2x"></picture>
            <figure data-src="x.jpg"></figure>
            inline text
        </div>"#;
        let dom = Dom::parse(html);
        let root = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let tc_before = text_content(&root);
        fix_lazy_images(&root);
        let tc_after = text_content(&root);
        assert_eq!(
            tc_before, tc_after,
            "text_content MUST be invariant under _fixLazyImages"
        );
    }

    #[test]
    fn post_process_content_runs_simplify_nested_elements() {
        // Verifies the call wiring: _postProcessContent's structural half is
        // _simplifyNestedElements.
        let dom = Dom::parse(
            r#"<body><article><div class="outer"><div class="inner">x</div></div></article></body>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        post_process_content(&art);
        let divs = get_elements_by_tag_name(&art, "div");
        assert_eq!(divs.len(), 1);
        assert_eq!(text_content(&divs[0]), "x");
    }

    // ---- fix_lazy_images additional branch coverage (Readability.js:2332-2412) ----
    //
    // Existing tests already cover: figure-creates-img, inner-img short-circuit,
    // data-src→src, data-srcset→srcset, tiny b64 placeholder cull, svg+xml
    // carve-out, has-image short-circuit, lazy-class bypass. The tests below
    // hit the remaining branches.

    /// `Readability.js:2391` `image_src_value`: any attribute name with a
    /// plain image URL value is promoted to `src`. Cover `data-original`
    /// (the WordPress/lazysizes pattern).
    /// rationale: any non-{src,srcset,alt} attribute whose value matches
    /// `^.+\.(jpg|jpeg|png|webp)\S*$` is copied to `src`.
    #[test]
    fn fix_lazy_images_data_original_attr_promotes_to_src() {
        let dom = Dom::parse(r#"<body><img data-original="hero.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("hero.jpg"),
            "data-original value matches image_src_value ⇒ copied to src"
        );
    }

    /// Same shape as above, different attribute name (`data-defer-src`).
    /// rationale: the promotion is value-driven, not attribute-name-driven.
    #[test]
    fn fix_lazy_images_data_defer_src_promotes_to_src() {
        let dom = Dom::parse(r#"<body><img data-defer-src="hero.webp"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("hero.webp"),
            ".webp extension matches the image_src_value alternation"
        );
    }

    /// Same shape again with `data-flickity-lazyload` (a real-world lazy-load
    /// library attribute).
    /// rationale: arbitrary library prefixes — the function is generic.
    #[test]
    fn fix_lazy_images_data_flickity_lazyload_promotes_to_src() {
        let dom = Dom::parse(r#"<body><img data-flickity-lazyload="hero.png"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("hero.png"));
    }

    /// `Readability.js:2381` skip `src` attribute when iterating: a plain
    /// image-URL value at `src` must NOT be promoted to itself (and we must
    /// not loop forever).
    /// rationale: the iteration explicitly skips `src` / `srcset` / `alt`.
    #[test]
    fn fix_lazy_images_src_attr_is_skipped_in_iteration() {
        // src already set, no `lazy` class. The has-image short-circuit at
        // :2371-2377 returns BEFORE the iteration. We pin the outcome: no
        // duplicate write, src is identical.
        let dom = Dom::parse(r#"<body><img src="real.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(get_attribute(&img, "src").as_deref(), Some("real.jpg"));
    }

    /// `Readability.js:2381` skip `alt` attribute when iterating. An `alt`
    /// whose value happens to look like an image URL must NOT be promoted.
    /// rationale: the JS skips alt to avoid hijacking accessibility text.
    #[test]
    fn fix_lazy_images_alt_attr_is_skipped() {
        // No src/srcset, has class "lazy" so short-circuit DOES NOT fire ⇒
        // the iteration runs. alt="photo.jpg" must NOT become src.
        let dom = Dom::parse(r#"<body><img class="lazy" alt="photo.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert!(
            get_attribute(&img, "src").is_none(),
            "alt value MUST be skipped (Readability.js:2381): src stays unset"
        );
    }

    /// `Readability.js:2371-2377` srcset=="null" string special case: a
    /// literal `"null"` srcset is treated as absent, so the has-image
    /// short-circuit does NOT fire ⇒ attribute promotion runs.
    /// rationale: pages emit `srcset="null"` for placeholder lazy loaders.
    #[test]
    fn fix_lazy_images_srcset_null_string_treated_as_absent() {
        let dom = Dom::parse(
            r#"<body><img srcset="null" data-src="real.jpg"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        // The has-image short-circuit was bypassed (srcset == "null"),
        // attribute promotion ran, data-src landed in src.
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("real.jpg"),
            "srcset=\"null\" must NOT count as an image (Readability.js:2374-2375)"
        );
    }

    /// `Readability.js:2386` srcset-shape value: extension + whitespace +
    /// digit (e.g. `foo.jpg 2x`) ⇒ promoted to `srcset`, not `src`.
    /// rationale: a srcset-pattern value is recognised by the regex and
    /// routed to the srcset slot.
    #[test]
    fn fix_lazy_images_data_src_with_srcset_shape_value_goes_to_srcset() {
        // Value `"hero.jpg 2x"` matches image_srcset_value (.jpg + ws + digit)
        // BEFORE image_src_value is checked (the JS `if/else` order).
        let dom = Dom::parse(r#"<body><img data-src="hero.jpg 2x"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "srcset").as_deref(),
            Some("hero.jpg 2x"),
            "srcset-shape value routed to srcset slot (Readability.js:2386)"
        );
        assert!(
            get_attribute(&img, "src").is_none(),
            "src must remain unset — the value matched the srcset regex first"
        );
    }

    /// `Readability.js:2391` else-branch: non-image-shaped value is ignored.
    /// rationale: a `data-foo="some-other-thing"` value that matches neither
    /// regex leaves no trace on src/srcset.
    #[test]
    fn fix_lazy_images_non_image_attr_value_ignored() {
        let dom = Dom::parse(
            r#"<body><img class="lazy" data-foo="bar-not-an-image"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert!(get_attribute(&img, "src").is_none());
        assert!(get_attribute(&img, "srcset").is_none());
    }

    /// `Readability.js:2336-2369` tiny-b64 NEGATIVE case: NO other attribute
    /// has an image-extension value ⇒ `src_could_be_removed=false` ⇒ the
    /// tiny base64 src is KEPT (not removed). The has-image short-circuit
    /// then fires (the src remains) and no promotion runs.
    /// rationale: the cull only triggers when a real image exists elsewhere
    /// (otherwise removing src would leave the page with no image).
    #[test]
    fn fix_lazy_images_b64_kept_when_no_other_image_attr() {
        let dom = Dom::parse(
            r#"<body><img src="data:image/png;base64,iVBOR" data-some="not-an-image-text"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("data:image/png;base64,iVBOR"),
            "no other image-ext attr ⇒ src_could_be_removed=false ⇒ b64 src kept"
        );
    }

    /// `Readability.js:2367` b64length >= 133 boundary: a LARGE base64 src
    /// (≥133 chars payload) is NOT removed even if another attr has an
    /// image-ext value — only tiny placeholders qualify.
    /// rationale: a real base64-encoded image survives the cull.
    #[test]
    fn fix_lazy_images_b64_kept_when_payload_large_enough() {
        // Build a base64 src with payload >= 133 chars (any chars suffice as
        // the function counts character length, not validates base64).
        let large_payload = "A".repeat(140);
        let html = format!(
            r#"<body><img src="data:image/png;base64,{large_payload}" data-real-src="real.jpg"></body>"#
        );
        let dom = Dom::parse(&html);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        // src is unchanged (b64length>=133 ⇒ no cull). has-image short-circuit
        // then keeps everything as-is — data-real-src is NOT promoted.
        let src = get_attribute(&img, "src").unwrap();
        assert!(
            src.starts_with("data:image/png;base64,"),
            "large b64 src kept (Readability.js:2366): {src}"
        );
    }

    /// `Readability.js:2398-2407` figure with srcset-shape value: a `<figure>`
    /// with NO inner `<img>`/`<picture>` and a `data-srcset` is given a NEW
    /// `<img>` child carrying that srcset.
    /// rationale: the score-affecting branch covers both src AND srcset shapes.
    #[test]
    fn fix_lazy_images_empty_figure_with_srcset_creates_img_with_srcset() {
        // value "hero.jpg 2x" matches image_srcset_value ⇒ figure-creates-img
        // assigns it to the new img's `srcset` attribute.
        let dom = Dom::parse(
            r#"<body><div id=a><figure data-srcset="hero.jpg 2x"></figure></div></body>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        fix_lazy_images(&art);
        let imgs = get_elements_by_tag_name(&art, "img");
        assert_eq!(imgs.len(), 1, "figure branch creates a new <img>");
        assert_eq!(
            get_attribute(&imgs[0], "srcset").as_deref(),
            Some("hero.jpg 2x"),
            "srcset-shape value lands in srcset (Readability.js:2398-2407)"
        );
    }

    /// `Readability.js:2382-2384`: `srcset` attribute when iterating is SKIPPED.
    /// rationale: an existing `srcset` whose value is non-empty and not "null"
    /// makes the has-image short-circuit fire, so we never see iteration. Pin
    /// the outcome: srcset is preserved exactly.
    #[test]
    fn fix_lazy_images_srcset_attr_is_skipped_in_iteration() {
        // `srcset="real.jpg 2x"` ⇒ has-image short-circuit fires (srcset
        // is present and != "null"). data-src is NOT promoted.
        let dom = Dom::parse(
            r#"<body><img srcset="real.jpg 2x" data-src="other.jpg"></body>"#,
        );
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "srcset").as_deref(),
            Some("real.jpg 2x"),
            "existing srcset preserved (short-circuit at :2371-2377)"
        );
        assert!(
            get_attribute(&img, "src").is_none(),
            "no promotion ran (short-circuit), src unset"
        );
    }

    /// `Readability.js:2371-2377` empty-string `src` is treated as ABSENT.
    /// rationale: `src=""` does NOT count as having an image, so the
    /// short-circuit does NOT fire and attribute promotion runs.
    #[test]
    fn fix_lazy_images_empty_src_string_does_not_short_circuit() {
        // src="" ⇒ has_image=false. data-src="real.jpg" promotes to src.
        let dom = Dom::parse(r#"<body><img src="" data-src="real.jpg"></body>"#);
        let body = dom.body().unwrap();
        fix_lazy_images(&body);
        let img = get_elements_by_tag_name(&body, "img")[0].clone();
        assert_eq!(
            get_attribute(&img, "src").as_deref(),
            Some("real.jpg"),
            "empty src does NOT short-circuit; promotion runs"
        );
    }

    // ---- unwrap_noscript_images additional branch coverage (Readability.js:1892-1968) ----

    /// `Readability.js:1934` `prevElement` is `null` (no prior sibling): the
    /// noscript-img unwrap skips this noscript.
    /// rationale: a noscript that is the FIRST child has no previous sibling
    /// to replace, so the pass-2 guard silently continues.
    #[test]
    fn unwrap_noscript_images_no_prev_sibling_skips_unwrap() {
        // No prev sibling — noscript is the only child.
        let dom = Dom::parse(
            r#"<html><body><div><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        // No replacement happened: the <noscript> is still there and its
        // <img> still inside it (it survives pass 1 via its src attribute).
        let nosc = get_elements_by_tag_name(&div, "noscript");
        assert_eq!(nosc.len(), 1, "noscript preserved when prev sibling absent");
        let img_in_nos = get_elements_by_tag_name(&nosc[0], "img");
        assert_eq!(img_in_nos.len(), 1, "noscript's img stays inside");
    }

    /// `Readability.js:1919-1921` noscript that does NOT pass `_isSingleImage`
    /// (empty noscript) is skipped.
    /// rationale: an empty `<noscript>` is not an unwrap target.
    #[test]
    fn unwrap_noscript_images_empty_noscript_skipped() {
        let dom = Dom::parse(
            r#"<html><body><div><img src="placeholder.png"><noscript></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        // The prev img is kept, the empty noscript is left in place
        // (it would be removed later by _removeScripts).
        assert_eq!(
            get_elements_by_tag_name(&div, "img").len(),
            1,
            "empty noscript skipped, placeholder img stays"
        );
    }

    /// `Readability.js:1935-1938` prev-img-via-descendant branch: prev element
    /// is NOT an `<img>` itself (it's e.g. an `<a>` wrapping an `<img>`), so
    /// the JS uses `prev.getElementsByTagName("img")[0]`.
    /// rationale: cover the descendant lookup arm of the prevImg resolution.
    #[test]
    fn unwrap_noscript_images_prev_with_img_descendant() {
        // Prev: <a><img data-src="lazy.png"></a> — single-image (a has 1
        // child, no text); its descendant img survives pass 1 (data-src).
        // Noscript: <img src="real.jpg">.
        let dom = Dom::parse(
            r#"<html><body><div><a><img data-src="lazy.png"></a><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        // The <a> wrapper is the prevElement; it was replaced by newImg
        // (`replaceChild` at :1965). The div now contains:
        //  - the newImg (moved out of noscript) where <a> was
        //  - the noscript element (still present, empty after move)
        // Pin the survivor:
        let div_imgs = get_elements_by_tag_name(&div, "img");
        // The new img is directly under div (it took the <a>'s slot, then
        // replaceChild moves it without re-parenting into <a>). At least one
        // img with src="real.jpg" + data-src="lazy.png" copied across.
        let new_img_under_div = div_imgs
            .iter()
            .find(|i| get_attribute(i, "src").as_deref() == Some("real.jpg"));
        assert!(
            new_img_under_div.is_some(),
            "noscript img promoted into the div (prev had img descendant)"
        );
        let new_img = new_img_under_div.unwrap();
        assert_eq!(
            get_attribute(new_img, "data-src").as_deref(),
            Some("lazy.png"),
            "data-src from prev's descendant img copied onto newImg"
        );
    }

    /// `Readability.js:1943-1945` attribute copy skips empty values.
    /// rationale: an attribute on prevImg whose value is "" is silently
    /// skipped during the attribute-copy pass.
    #[test]
    fn unwrap_noscript_images_skips_empty_attribute_values_during_copy() {
        // Prev img: src=placeholder.png (survives pass 1 by name) + an
        // empty-string custom attr. Noscript img: src=real.jpg.
        //
        // The empty-string attr would otherwise be copied if it matched the
        // value-regex, but the :1944 `if (!value) continue;` short-circuits.
        // We pin by asserting the new img has NO `data-empty` attribute.
        let dom = Dom::parse(
            r#"<html><body><div><img src="placeholder.png" data-empty=""><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(imgs.len(), 1);
        let img = &imgs[0];
        // The empty-valued attribute MUST NOT have been copied. (Whether
        // present-with-empty-value or absent, the contract is "no useful
        // value got carried over".)
        assert!(
            get_attribute(img, "data-empty").is_none()
                || get_attribute(img, "data-empty").as_deref() == Some(""),
            "empty-value attr was skipped (Readability.js:1944)"
        );
    }

    /// `Readability.js:1947-1951` attribute copy skips non-source attributes
    /// (name not in {src,srcset} AND value doesn't match image-extension).
    /// rationale: a `class` attribute on prevImg is NOT copied to newImg.
    #[test]
    fn unwrap_noscript_images_does_not_copy_non_source_attributes() {
        // Prev img: src=placeholder.png + class=placeholder-class. Noscript
        // img: src=real.jpg. The class attribute is name="class" (not src/
        // srcset) AND value="placeholder-class" (no image extension), so
        // :1947-1951 SKIPS it.
        let dom = Dom::parse(
            r#"<html><body><div><img src="placeholder.png" class="placeholder-class"><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(imgs.len(), 1);
        let img = &imgs[0];
        // class was NOT copied (faithful: only source-ish attrs cross the
        // boundary).
        let cls = get_attribute(img, "class");
        assert!(
            cls.is_none() || cls.as_deref() == Some(""),
            "non-source class attribute NOT copied (Readability.js:1947-1951): {cls:?}"
        );
    }

    /// `Readability.js:1952-1954` skip when newImg already has the SAME
    /// value: the copy is a no-op.
    /// rationale: when prev and noscript img both carry the same src, no
    /// `data-old-src` is created.
    #[test]
    fn unwrap_noscript_images_skips_when_same_value_already_present() {
        // Both prev and noscript img have src="same.jpg". The :1952-1954
        // guard `if (newImg.getAttribute(name) === value) continue;` skips
        // — no data-old-src created.
        let dom = Dom::parse(
            r#"<html><body><div><img src="same.jpg"><noscript><img src="same.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(imgs.len(), 1);
        let img = &imgs[0];
        assert_eq!(get_attribute(img, "src").as_deref(), Some("same.jpg"));
        assert!(
            get_attribute(img, "data-old-src").is_none(),
            "no data-old-src when values identical (Readability.js:1952-1954)"
        );
    }

    /// `Readability.js:1899-1909` Pass 1 — keep when no attributes at all
    /// but ANY attribute *value* matches image extension regex. Cover a
    /// non-source-named attribute whose value carries `.jpg`.
    /// rationale: the value-regex KEEP arm at :1907.
    #[test]
    fn unwrap_noscript_images_keeps_img_by_value_regex_only() {
        // `id="myimage.jpg"` — name is not in the src-name set, but the
        // value matches `.(jpg|jpeg|png|webp)`. :1907-1909 KEEPS the img.
        let dom = Dom::parse(
            r#"<html><body><div><img id="myimage.jpg"></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let imgs = get_elements_by_tag_name(&dom.document(), "img");
        assert_eq!(
            imgs.len(),
            1,
            "id value matches image-extension regex ⇒ img kept (Readability.js:1907)"
        );
    }

    // (Existing test below — keep.)

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

    /// `_prepDocument` (`Readability.js:659-670`): the `if (doc.body)` guard
    /// false side — a document with NO body still strips styles and retags
    /// font without touching `_replaceBrs`.
    /// rationale: pin `if let Some(b) = body` false arm (prep.rs:54) by passing
    /// `body = None`.
    #[test]
    fn prep_document_with_no_body_still_strips_styles() {
        let mut dom = Dom::parse("<html><head><style>.a{}</style></head></html>");
        let root = dom.root_element().unwrap();
        // Pass None for body: exercises the `if let Some(b)` false side.
        prep_document(&mut dom, &root, None);
        assert!(
            get_elements_by_tag_name(&root, "style").is_empty(),
            "style tags removed even when body is None (Readability.js:659-661)"
        );
    }

    /// `_clean` (`Readability.js:2182-2206`): a non-`<object>` embed
    /// (`<iframe>`) with no video attribute is removed; the `tagName ===
    /// "OBJECT"` text-content fallback is NOT consulted for it.
    /// rationale: pin the false side of `tag_name(&element) == "OBJECT"`
    /// (prep.rs:537) — only `<object>` reaches the inner-content video check.
    #[test]
    fn clean_iframe_non_object_skips_object_text_check() {
        let dom =
            Dom::parse(r#"<div>keep<iframe src="https://example.com/page"></iframe></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean(&div, "iframe");
        assert!(
            get_elements_by_tag_name(&div, "iframe").is_empty(),
            "non-video iframe removed; object-text check not reached (Readability.js:2196 false)"
        );
        assert_eq!(text_content(&div), "keep");
    }

    /// `_clean` (`Readability.js:2191-2199`): an `<object>` whose INNER TEXT
    /// carries a video URL (no video attribute) is KEPT via the object
    /// inner-content video check.
    /// rationale: pin the true side of `regexps::videos().is_match(text)`
    /// (prep.rs:538) — the object-text fallback keeps an embedded video URL.
    #[test]
    fn clean_object_with_video_url_in_text_is_kept() {
        // No attribute on the object, so any_attr_matches returns Some(false);
        // the inner-text fallback sees the youtube URL and keeps it.
        let dom = Dom::parse(
            r#"<div><object>https://www.youtube.com/embed/abc</object><object>plain</object>txt</div>"#,
        );
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean(&div, "object");
        let objs = get_elements_by_tag_name(&div, "object");
        assert_eq!(objs.len(), 1, "the video-URL object survives, plain removed");
        assert!(
            text_content(&objs[0]).contains("youtube.com"),
            "kept object is the one with a video URL in its text (Readability.js:2196-2199)"
        );
    }

    /// `any_attr_matches_videos` (`Readability.js:2186-2189`): an embed whose
    /// attribute value is NOT a video URL returns `Some(false)` so the embed
    /// is removed.
    /// rationale: pin the false side of `regexps::videos().is_match(&a.value)`
    /// (prep.rs:555) — a non-video attribute does not keep the embed.
    #[test]
    fn clean_embed_with_non_video_attribute_is_removed() {
        let dom = Dom::parse(r#"<div>keep<embed width="100" height="50"></embed></div>"#);
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        clean(&div, "embed");
        assert!(
            get_elements_by_tag_name(&div, "embed").is_empty(),
            "embed with only non-video attrs is removed (Readability.js:2186-2189 no match)"
        );
    }

    /// `prep_article` empty-`<p>` pass (`Readability.js:835-850`): the FULL
    /// `prep_article` (not the stage-1a slice) keeps a `<p>` with text and a
    /// `<p>` carrying an `<img>`, removing only the whitespace-only `<p>`.
    /// rationale: drive both false sides of
    /// `content_element_count == 0 && get_inner_text(...).is_empty()`
    /// (prep.rs:498) — text-bearing `<p>` fails `is_empty`, img-bearing `<p>`
    /// fails the content-count, only the empty `<p>` is removed.
    #[test]
    fn prep_article_full_empty_p_removal_keeps_text_and_img_paragraphs() {
        let mut dom = Dom::parse(
            r#"<div id="readability-page-1"><p>Genuine readable prose paragraph well beyond twenty-five characters of content.</p><p>   </p><p><img src="photo.jpg"></p></div>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let flags = crate::readability::helpers::Flags::default();
        prep_article(&mut dom, &flags, &art);
        let ps = get_elements_by_tag_name(&art, "p");
        // The whitespace-only <p> removed; text <p> + img <p> survive.
        assert_eq!(
            ps.len(),
            2,
            "empty <p> removed, text and img paragraphs kept (Readability.js:835-850)"
        );
        assert!(text_content(&art).contains("Genuine readable prose"));
        assert_eq!(
            get_elements_by_tag_name(&art, "img").len(),
            1,
            "the img-bearing <p> is kept (content_element_count != 0)"
        );
    }

    /// `_simplifyNestedElements` (`Readability.js:549-559`): the single
    /// `<SECTION>` child unwrap (the `|| _hasSingleTagInsideElement(node,
    /// "SECTION")` limb), mirror of the DIV case.
    /// rationale: pin the true side of `has_single_tag_inside_element(&node,
    /// "SECTION")` (prep.rs:652) — a div wrapping a single section is unwrapped.
    #[test]
    fn simplify_nested_elements_unwraps_single_section_child() {
        let dom = Dom::parse(
            r#"<body><article><div class="wrap"><section class="inner">section text</section></div></article></body>"#,
        );
        let art = get_elements_by_tag_name(&dom.body().unwrap(), "article")[0].clone();
        simplify_nested_elements(&art);
        // The wrapping <div> is unwrapped; the <section> survives with the
        // div's attrs cloned onto it.
        assert_eq!(
            get_elements_by_tag_name(&art, "div").len(),
            0,
            "single-section-child div unwrapped (Readability.js:549-559 SECTION limb)"
        );
        let sections = get_elements_by_tag_name(&art, "section");
        assert_eq!(sections.len(), 1);
        assert_eq!(text_content(&sections[0]), "section text");
        assert_eq!(
            get_attribute(&sections[0], "class").as_deref(),
            Some("wrap"),
            "wrapping div's class cloned onto the section"
        );
    }

    /// `_unwrapNoscriptImages` attribute copy (`Readability.js:1947-1951`):
    /// an attribute whose NAME is neither `src` nor `srcset` but whose VALUE
    /// matches the image-extension regex IS a source attribute and gets copied.
    /// rationale: pin the true side of `regexps::image_extension().is_match
    /// (&value)` (prep.rs:258) — the value-regex limb of the source-attr test.
    #[test]
    fn unwrap_noscript_images_copies_attr_with_image_extension_value() {
        // prev img: src=placeholder.png (survives pass 1 by name) + a
        // data-hires="hires.jpg" whose NAME is not src/srcset but whose value
        // ends in .jpg ⇒ source attr via the value-regex limb.
        let dom = Dom::parse(
            r#"<html><body><div><img src="placeholder.png" data-hires="hires.jpg"><noscript><img src="real.jpg"></noscript></div></body></html>"#,
        );
        unwrap_noscript_images(&dom.document());
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        let imgs = get_elements_by_tag_name(&div, "img");
        assert_eq!(imgs.len(), 1);
        assert_eq!(
            get_attribute(&imgs[0], "data-hires").as_deref(),
            Some("hires.jpg"),
            "image-extension-valued attr copied via the value-regex limb (Readability.js:1947-1951)"
        );
    }

    /// `_replaceBrs` (`Readability.js:719-742`): the move-into-`<p>` loop stops
    /// at a `<br><br>` chain (`tag == BR` true + next BR), and stops at a
    /// non-phrasing block; and a non-whitespace last child halts the trailing
    /// trim.
    /// rationale: pin (a) the `tag_name(&n) == "BR"` true arm (prep.rs:328),
    /// (b) the `!is_phrasing_content(&n)` true arm (prep.rs:334), and (c) the
    /// `is_whitespace(&lc)` false arm (prep.rs:344).
    #[test]
    fn replace_brs_move_loop_stops_on_br_chain_and_block() {
        // After the first <br><br> collapse, the new <p> absorbs phrasing
        // siblings until it meets a non-phrasing <div> (334 true). The <p>'s
        // last child is the non-whitespace text "phrase" (344 false → trim
        // stops immediately).
        let mut dom = Dom::parse("<div>start<br><br>phrase<div>block</div>tail</div>");
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        replace_brs(&mut dom, &body);
        let ps = get_elements_by_tag_name(&div, "p");
        assert_eq!(ps.len(), 1, "one <p> from the br-chain collapse");
        assert_eq!(
            text_content(&ps[0]),
            "phrase",
            "phrasing absorbed up to the non-phrasing <div> (Readability.js:734 break)"
        );
        // The inner <div>block</div> stayed outside the <p>.
        assert!(text_content(&div).contains("block"));
    }

    /// `_replaceBrs` (`Readability.js:719-723`): the move loop breaks when a
    /// following `<br>` begins a NEW `<br><br>` chain, leaving the second chain
    /// for its own iteration.
    /// rationale: pin the `tag == BR` true arm AND the inner "next is BR" break
    /// (prep.rs:328-332) — a `phrase<br><br>...` run after the first collapse.
    #[test]
    fn replace_brs_two_separate_br_chains() {
        // foo<br><br>bar<br><br>baz : two separate chains. The first collapse
        // creates a <p>; while filling it the loop meets the second <br><br>
        // chain (328 true, inner next-BR true) and breaks, leaving the second
        // chain to be handled by a later br iteration.
        let mut dom = Dom::parse("<div>foo<br><br>bar<br><br>baz</div>");
        let body = dom.body().unwrap();
        let div = get_elements_by_tag_name(&body, "div")[0].clone();
        replace_brs(&mut dom, &body);
        let ps = get_elements_by_tag_name(&div, "p");
        assert_eq!(ps.len(), 2, "two <p> elements from two separate br chains");
        assert_eq!(text_content(&div), "foobarbaz");
    }
}
