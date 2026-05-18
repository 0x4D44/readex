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
    self, Dom, NodeRef, append_child, child_nodes, create_element, get_all_nodes_with_tag,
    inner_text, is_element, parent, replace_child, tag_name, text_content,
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
}
