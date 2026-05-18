//! `grab_article.rs` — `_grabArticle` (`Readability.js:1031-1597`), the
//! algorithmic core. **Stage-1a slice (HLD §7.1):** the prepping walk +
//! unlikely strip + `div`→`p` + `elementsToScore` scoring + ancestor
//! propagation + **single top-candidate selection only**.
//!
//! STOPS before the sibling-append pass (`Readability.js:1415`) and the
//! `_charThreshold` retry / `FLAG_*` flag-sieve loop (`Readability.js:1546-
//! 1576`) — those are Stage 1b/1c (HLD §7.2/§7.3). Here the outer `while
//! (true)` runs **exactly once** (all flags set, no retry), `articleContent`
//! is built from the chosen top candidate directly, and the fake-`<div>` body
//! fallback (`Readability.js:1314-1327`) is ported (needed for the first
//! `Ok`).
//!
//! Faithful transcription with `Readability.js:<line>` citations
//! (anti-inversion, HLD §4.3(a)). The JS `candidates` array is mirrored by a
//! `Vec<NodeRef>` so candidate **ordering is deterministic** (HLD §5.1 — never
//! from a `HashMap`).

use crate::readability::dom::{
    self, Dom, NodeRef, append_child, children, class_name, create_element, first_element_child,
    get_attribute, id, is_element, parent, replace_child, tag_name,
};
use crate::readability::helpers::{
    FLAG_STRIP_UNLIKELYS, Flags, get_next_node, is_element_without_content, is_phrasing_content,
    is_probably_visible, is_valid_byline, is_whitespace,
};
use crate::readability::regexps;
use crate::readability::scoring::{
    get_link_density, get_node_ancestors, initialize_node, inner_text_len, text_similarity,
};

/// `_nbTopCandidates` default (`Readability.js:125`, `DEFAULT_N_TOP_CANDIDATES
/// = 5`). Stage 1a uses the default (no `Options.nbTopCandidates`).
const NB_TOP_CANDIDATES: usize = 5;

/// Outcome of the Stage-1a `_grabArticle`: the `articleContent` element (a
/// fresh `<div>` holding the selected content), or `None` (`Readability.js`
/// `return null` — only reachable here via the body-fallback producing nothing,
/// faithfully mapped to an empty extraction by the caller, Bug-E2).
pub struct GrabResult {
    /// The `articleContent` div whose `text_content` is the extracted body.
    pub article_content: NodeRef,
}

/// `_removeAndGetNext(node)` (`Readability.js:932-936`):
/// `var n = _getNextNode(node, true); node.remove(); return n;`.
fn remove_and_get_next(node: &NodeRef) -> Option<NodeRef> {
    let next = get_next_node(node, true);
    dom::remove(node);
    next
}

/// `_headerDuplicatesTitle(node)` (`Readability.js:2677-2684`).
///
/// `H1`/`H2` whose `_getInnerText(node,false)` is `_textSimilarity(
/// articleTitle, heading) > 0.75`. On the scored-body path: a true result
/// deletes the heading (`Readability.js:1105-1113`), so this must be faithful.
fn header_duplicates_title(node: &NodeRef, article_title: &str) -> bool {
    match tag_name(node).as_deref() {
        Some("H1") | Some("H2") => {}
        _ => return false,
    }
    // heading = _getInnerText(node, false)  (trim only, no /\s{2,}/ collapse)
    let heading = dom::inner_text(node, false);
    text_similarity(article_title, &heading) > 0.75
}

/// `_hasAncestorTag(node, tagName, maxDepth=3)` (`Readability.js:2217-2235`),
/// no `filterFn` (the two Stage-1a call sites — `"table"`, `"code"` — pass
/// none).
///
/// `maxDepth = maxDepth || 3`; walk `parentNode`; if `maxDepth > 0 && depth >
/// maxDepth` → false; if `parentNode.tagName === tagName` → true; ascend.
fn has_ancestor_tag(node: &NodeRef, tag_name_arg: &str, max_depth: i32) -> bool {
    let want = tag_name_arg.to_ascii_uppercase();
    let max_depth = if max_depth == 0 { 3 } else { max_depth };
    let mut depth = 0_i32;
    let mut cur = node.clone();
    while let Some(p) = parent(&cur) {
        if max_depth > 0 && depth > max_depth {
            return false;
        }
        if tag_name(&p).as_deref() == Some(want.as_str()) {
            return true;
        }
        cur = p;
        depth += 1;
    }
    false
}

/// `node.firstChild` (any node type). `None` if no children.
fn first_child(node: &NodeRef) -> Option<NodeRef> {
    node.children.borrow().first().cloned()
}

/// `node.nextSibling` (any node type) — used by the DIV phrasing-wrap walk.
fn next_sibling(node: &NodeRef) -> Option<NodeRef> {
    let p = parent(node)?;
    let kids = p.children.borrow();
    let idx = kids.iter().position(|c| std::rc::Rc::ptr_eq(c, node))?;
    kids.get(idx + 1).cloned()
}

/// `node.lastChild` (any node type).
fn last_child(node: &NodeRef) -> Option<NodeRef> {
    node.children.borrow().last().cloned()
}

/// `_grabArticle()` — Stage-1a single-pass (`Readability.js:1031-1413` + the
/// fake-div fallback `1314-1327`; STOP before sibling-append `1415` and the
/// retry loop `1546`).
///
/// * `dom` owns the tree + score side-table.
/// * `doc_root` = `this._doc` (document); `body` = `doc.body` (the `page`).
/// * `article_title` = `this._articleTitle` (set before `_grabArticle`, HLD
///   §7.1) — drives `_headerDuplicatesTitle`.
/// * `flags` = `this._flags` (all set at Stage 1a).
///
/// Returns the `articleContent` div, or `None` (faithful `return null` — the
/// caller maps that to an empty `Ok`, Bug-E2).
pub fn grab_article(
    dom: &mut Dom,
    doc_root: &NodeRef,
    body: &NodeRef,
    article_title: &str,
    flags: &Flags,
    article_byline_found: &mut bool,
) -> Option<GrabResult> {
    // var doc = this._doc; page = doc.body (isPaging=false at Stage 1a).
    let page = body.clone();

    // --- The outer `while (true)` runs exactly ONCE at Stage 1a (no retry,
    //     all flags set). ---

    // stripUnlikelyCandidates = _flagIsActive(FLAG_STRIP_UNLIKELYS)
    let strip_unlikely_candidates = flags.is_active(FLAG_STRIP_UNLIKELYS);

    let mut elements_to_score: Vec<NodeRef> = Vec::new();
    // var node = this._doc.documentElement;  (= <html>)
    let mut node_opt = first_element_child(doc_root); // documentElement = <html>
    // documentElement is the root <html>; `doc_root` is the Document. Match
    // `this._doc.documentElement` exactly (the <html> element).
    if node_opt
        .as_ref()
        .map(|n| tag_name(n).as_deref() != Some("HTML"))
        .unwrap_or(false)
    {
        // Defensive: if first element child isn't <html>, find it.
        node_opt = children(doc_root)
            .into_iter()
            .find(|c| tag_name(c).as_deref() == Some("HTML"));
    }

    let mut should_remove_title_header = true;

    while let Some(node) = node_opt.clone() {
        // (this._articleLang assignment at HTML — Stage 4 metadata; skipped:
        // not score-affecting, HLD §2 score-invisible partition.)

        // var matchString = node.className + " " + node.id;
        let match_string = format!("{} {}", class_name(&node), id(&node));

        // if (!_isProbablyVisible(node)) { node = _removeAndGetNext(node); continue; }
        if !is_probably_visible(&node) {
            node_opt = remove_and_get_next(&node);
            continue;
        }

        // aria-modal=="true" && role=="dialog" -> remove
        if get_attribute(&node, "aria-modal").as_deref() == Some("true")
            && get_attribute(&node, "role").as_deref() == Some("dialog")
        {
            node_opt = remove_and_get_next(&node);
            continue;
        }

        // byline detection (Readability.js:1082-1103). Stage 1a has no JSON-LD
        // metadata byline and tracks _articleByline as "not yet found". The
        // node is removed when it IS a valid byline (score-affecting: it
        // deletes a subtree from the scored body), so port the removal
        // faithfully. (The itemprop=name refinement only changes the *stored*
        // byline string — Stage-4 metadata — so it is omitted; the REMOVAL is
        // what changes scored text and that is preserved exactly.)
        if !*article_byline_found && is_valid_byline(&node, &match_string) {
            *article_byline_found = true;
            node_opt = remove_and_get_next(&node);
            continue;
        }

        // if (shouldRemoveTitleHeader && _headerDuplicatesTitle(node))
        if should_remove_title_header && header_duplicates_title(&node, article_title) {
            should_remove_title_header = false;
            node_opt = remove_and_get_next(&node);
            continue;
        }

        // Remove unlikely candidates.
        if strip_unlikely_candidates {
            if regexps::unlikely_candidates().is_match(&match_string)
                && !regexps::ok_maybe_its_a_candidate().is_match(&match_string)
                && !has_ancestor_tag(&node, "table", 3)
                && !has_ancestor_tag(&node, "code", 3)
                && tag_name(&node).as_deref() != Some("BODY")
                && tag_name(&node).as_deref() != Some("A")
            {
                node_opt = remove_and_get_next(&node);
                continue;
            }

            // UNLIKELY_ROLES.includes(node.getAttribute("role"))
            if let Some(role) = get_attribute(&node, "role")
                && regexps::UNLIKELY_ROLES.contains(&role.as_str())
            {
                node_opt = remove_and_get_next(&node);
                continue;
            }
        }

        // Remove DIV/SECTION/HEADER/H1-6 without content.
        let tn = tag_name(&node);
        if matches!(
            tn.as_deref(),
            Some("DIV")
                | Some("SECTION")
                | Some("HEADER")
                | Some("H1")
                | Some("H2")
                | Some("H3")
                | Some("H4")
                | Some("H5")
                | Some("H6")
        ) && is_element_without_content(&node)
        {
            node_opt = remove_and_get_next(&node);
            continue;
        }

        // DEFAULT_TAGS_TO_SCORE.includes(node.tagName) -> elementsToScore.push
        if let Some(t) = tn.as_deref()
            && regexps::DEFAULT_TAGS_TO_SCORE.contains(&t)
        {
            elements_to_score.push(node.clone());
        }

        // Turn DIVs that lack block children into <p>s.
        if tn.as_deref() == Some("DIV") {
            // Put phrasing content into paragraphs.
            let mut p: Option<NodeRef> = None;
            let mut child_node = first_child(&node);
            while let Some(cn) = child_node.clone() {
                let next_s = next_sibling(&cn);
                if is_phrasing_content(&cn) {
                    if let Some(pp) = p.clone() {
                        append_child(&pp, &cn);
                    } else if !is_whitespace(&cn) {
                        let new_p = create_element("p");
                        // node.replaceChild(p, childNode); p.appendChild(childNode);
                        replace_child(&node, &new_p, &cn);
                        append_child(&new_p, &cn);
                        p = Some(new_p);
                    }
                } else if let Some(pp) = p.clone() {
                    // trim trailing whitespace children of p, then p = null
                    while let Some(lc) = last_child(&pp) {
                        if is_whitespace(&lc) {
                            dom::remove(&lc);
                        } else {
                            break;
                        }
                    }
                    p = None;
                }
                child_node = next_s;
            }

            // _hasSingleTagInsideElement(node,"P") && _getLinkDensity(node)<0.25
            //   -> unwrap the single <p>
            if has_single_tag_inside_element(&node, "P") && get_link_density(&node) < 0.25 {
                let new_node = children(&node)[0].clone();
                if let Some(np) = parent(&node) {
                    replace_child(&np, &new_node, &node);
                }
                elements_to_score.push(new_node.clone());
                node_opt = get_next_node(&new_node, false);
                continue;
            } else if !has_child_block_element(&node) {
                // node = _setNodeTag(node, "P")  -> NEW handle
                let new_node = dom.set_node_tag(&node, "P");
                elements_to_score.push(new_node.clone());
                node_opt = get_next_node(&new_node, false);
                continue;
            }
        }

        node_opt = get_next_node(&node, false);
    }

    // --- Score elementsToScore, propagate to ancestors. ---
    // candidates: a Vec mirroring the JS `candidates` array EXACTLY (HLD §5.1
    // — deterministic order, never a HashMap iteration).
    let mut candidates: Vec<NodeRef> = Vec::new();

    for element_to_score in &elements_to_score {
        // if (!parentNode || typeof parentNode.tagName === "undefined") return
        let Some(p) = parent(element_to_score) else {
            continue;
        };
        if !is_element(&p) {
            continue;
        }

        // innerText = _getInnerText(elementToScore); if (length < 25) return
        let inner_len = inner_text_len(element_to_score);
        if inner_len < 25 {
            continue;
        }

        // ancestors = _getNodeAncestors(elementToScore, 5); if (0) return
        let ancestors = get_node_ancestors(element_to_score, 5);
        if ancestors.is_empty() {
            continue;
        }

        // contentScore = 0; +=1 (base)
        let mut content_score = 0.0_f64;
        content_score += 1.0;
        // += innerText.split(REGEXPS.commas).length
        content_score += split_len(&dom::inner_text(element_to_score, true)) as f64;
        // += Math.min(Math.floor(innerText.length / 100), 3)
        content_score += ((inner_len / 100).min(3)) as f64;

        // Initialize & score ancestors.
        for (level, ancestor) in ancestors.iter().enumerate() {
            // if (!ancestor.tagName || !ancestor.parentNode ||
            //     typeof ancestor.parentNode.tagName === "undefined") return
            if tag_name(ancestor).is_none() {
                continue;
            }
            match parent(ancestor) {
                Some(ap) if is_element(&ap) => {}
                _ => continue,
            }

            // if (typeof ancestor.readability === "undefined") {
            //   _initializeNode(ancestor); candidates.push(ancestor); }
            if !dom.has_content_score(ancestor) {
                initialize_node(dom, flags, ancestor);
                candidates.push(ancestor.clone());
            }

            // scoreDivider: level0->1, level1->2, else level*3
            let score_divider = if level == 0 {
                1.0
            } else if level == 1 {
                2.0
            } else {
                (level as f64) * 3.0
            };

            // ancestor.readability.contentScore += contentScore / scoreDivider
            let prev = dom.content_score(ancestor).unwrap_or(0.0);
            dom.set_content_score(ancestor, prev + content_score / score_divider);
        }
    }

    // --- Pick the top candidates (Readability.js:1278-1306). ---
    // topCandidates: ordered Vec, mirroring the JS splice-sorted array.
    let mut top_candidates: Vec<NodeRef> = Vec::new();
    for candidate in &candidates {
        // candidateScore = contentScore * (1 - _getLinkDensity(candidate))
        let cs = dom.content_score(candidate).unwrap_or(0.0);
        let candidate_score = cs * (1.0 - get_link_density(candidate));
        dom.set_content_score(candidate, candidate_score);

        // insertion into the size-bounded topCandidates (JS splice logic).
        let mut inserted = false;
        for t in 0..NB_TOP_CANDIDATES {
            let beats = match top_candidates.get(t) {
                None => true,
                Some(a) => candidate_score > dom.content_score(a).unwrap_or(0.0),
            };
            if beats {
                top_candidates.insert(t, candidate.clone());
                if top_candidates.len() > NB_TOP_CANDIDATES {
                    top_candidates.pop();
                }
                inserted = true;
                break;
            }
        }
        let _ = inserted;
    }

    let mut top_candidate = top_candidates.first().cloned();
    let mut needed_to_create_top_candidate = false;
    let mut parent_of_top_candidate: Option<NodeRef>;

    // If no top candidate, OR it is BODY: build a fake DIV from page children
    // (Readability.js:1314-1327). Needed for the first Ok.
    if top_candidate.is_none()
        || top_candidate
            .as_ref()
            .map(|t| tag_name(t).as_deref() == Some("BODY"))
            .unwrap_or(false)
    {
        let tc = create_element("DIV");
        needed_to_create_top_candidate = true;
        // Move EVERY child (incl. text nodes) of page into tc.
        while let Some(fc) = first_child(&page) {
            append_child(&tc, &fc);
        }
        append_child(&page, &tc);
        initialize_node(dom, flags, &tc);
        top_candidate = Some(tc);
    } else {
        // The alternative-candidate / score-walk / single-child-parent
        // refinements (Readability.js:1328-1413).
        let mut tc = top_candidate.clone().unwrap();

        // (1) Alternative-candidate ancestor unification (1329-1366).
        // JS `for (var i = 1; i < topCandidates.length; i++)` — skip index 0
        // (the top candidate itself), same iteration set.
        let mut alternative_candidate_ancestors: Vec<Vec<NodeRef>> = Vec::new();
        for tc_i in top_candidates.iter().skip(1) {
            let ratio =
                dom.content_score(tc_i).unwrap_or(0.0) / dom.content_score(&tc).unwrap_or(0.0);
            if ratio >= 0.75 {
                alternative_candidate_ancestors.push(get_node_ancestors(tc_i, 0));
            }
        }
        const MINIMUM_TOPCANDIDATES: usize = 3;
        if alternative_candidate_ancestors.len() >= MINIMUM_TOPCANDIDATES {
            let mut potc = parent(&tc);
            while let Some(p) = potc.clone() {
                if tag_name(&p).as_deref() == Some("BODY") {
                    break;
                }
                let mut lists_containing = 0_usize;
                for ancestors in alternative_candidate_ancestors.iter() {
                    if lists_containing >= MINIMUM_TOPCANDIDATES {
                        break;
                    }
                    if ancestors.iter().any(|a| std::rc::Rc::ptr_eq(a, &p)) {
                        lists_containing += 1;
                    }
                }
                if lists_containing >= MINIMUM_TOPCANDIDATES {
                    tc = p.clone();
                    break;
                }
                potc = parent(&p);
            }
        }
        if !dom.has_content_score(&tc) {
            initialize_node(dom, flags, &tc);
        }

        // (2) Walk up while parent score is going up (1371-1398).
        let mut potc = parent(&tc);
        let mut last_score = dom.content_score(&tc).unwrap_or(0.0);
        let score_threshold = last_score / 3.0;
        while let Some(p) = potc.clone() {
            if tag_name(&p).as_deref() == Some("BODY") {
                break;
            }
            if !dom.has_content_score(&p) {
                potc = parent(&p);
                continue;
            }
            let parent_score = dom.content_score(&p).unwrap_or(0.0);
            if parent_score < score_threshold {
                break;
            }
            if parent_score > last_score {
                tc = p.clone();
                break;
            }
            last_score = dom.content_score(&p).unwrap_or(0.0);
            potc = parent(&p);
        }

        // (3) If top candidate is an only child, climb to parent (1400-1409).
        let mut potc2 = parent(&tc);
        while let Some(p) = potc2.clone() {
            if tag_name(&p).as_deref() == Some("BODY") {
                break;
            }
            if children(&p).len() != 1 {
                break;
            }
            tc = p.clone();
            potc2 = parent(&tc);
        }
        if !dom.has_content_score(&tc) {
            initialize_node(dom, flags, &tc);
        }

        top_candidate = Some(tc);
    }

    let top_candidate = top_candidate?;
    parent_of_top_candidate = parent(&top_candidate);

    // --- Build articleContent (Readability.js:1418). ---
    // Stage 1a: NO sibling-append (HLD §7.1 — STOP at 1415). articleContent
    // gets the top candidate only. (isPaging=false so no id set.)
    let article_content = create_element("DIV");

    if needed_to_create_top_candidate {
        // The fake div already holds every page child AND was appended to
        // `page`. JS keeps `articleContent` = the fresh div and (post-prep)
        // wraps. Faithfully: with no sibling loop, articleContent should hold
        // the fake top candidate's content. Move the fake-div into
        // articleContent (the JS sibling loop would have appended exactly the
        // topCandidate==fakeDiv as `sibling===topCandidate` → append=true).
        append_child(&article_content, &top_candidate);
    } else {
        // The JS sibling loop's very first iteration always appends the top
        // candidate itself (`sibling === topCandidate` ⇒ append=true,
        // Readability.js:1447-1448). With sibling-append deferred (Stage 1b),
        // the Stage-1a articleContent is exactly that single append: the top
        // candidate. (Recorded over-inclusion until 1b adds the *other*
        // siblings — HLD §7.1; NOT tuned.)
        append_child(&article_content, &top_candidate);
    }

    let _ = &mut parent_of_top_candidate;

    Some(GrabResult { article_content })
}

/// `_hasChildBlockElement` likewise.
use crate::readability::helpers::has_child_block_element;
/// `_hasSingleTagInsideElement` is in `helpers`; re-export for local call-site
/// readability (the DIV-unwrap branch uses it).
use crate::readability::helpers::has_single_tag_inside_element;

/// `innerText.split(REGEXPS.commas).length` (`Readability.js:1241`). JS
/// `String.split(regex)` length = (number of separator matches) + 1, including
/// for an empty string (`"".split(re)` → `[""]`, length 1). `Regex::split`
/// has the same shape; we count its parts.
fn split_len(s: &str) -> usize {
    regexps::commas().split(s).count()
}

// `_articleByline` "found?" flag (`Readability.js:1083` —
// `!this._articleByline && !this._metadata.byline`): Readability-instance
// state, threaded as `&mut bool` from `mod.rs` (NOT stored on `Dom` — the side
// tables are point-query node maps, HLD §5.1; this is not per-node state). At
// Stage 1a there is no JSON-LD metadata byline, so the guard reduces to
// `!this._articleByline`. The score-affecting effect is the one-shot subtree
// REMOVAL (ported faithfully above); the stored byline *string* is Stage-4
// metadata (HLD §7.6) and is deliberately not produced here.

#[cfg(test)]
mod tests {
    //! Expected selections hand-derived by tracing `Readability.js:1031-1413`
    //! (NOT by running an oracle — inversion, HLD §4).
    use super::*;
    use crate::readability::dom::{Dom, text_content};

    fn grab(html: &str, title: &str) -> Option<(Dom, NodeRef)> {
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        let mut byline_found = false;
        let r = grab_article(&mut dom, &root, &body, title, &flags, &mut byline_found)?;
        Some((dom, r.article_content))
    }

    #[test]
    fn grab_simple_article_selects_content_div() {
        // A clear content div with a long paragraph (>25 chars) vs a short
        // nav. The <p> scores, its ancestors get the score; the content
        // <div> (positive class "content") should win.
        let html = "<html><body>\
            <div class=\"nav\"><a href=/>Home</a><a href=/x>About</a></div>\
            <div class=\"content\"><p>This is a sufficiently long paragraph of real article body text that easily clears the twenty-five character minimum and then some more words.</p>\
            <p>A second paragraph also with plenty of genuine readable prose content for scoring purposes here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("should grab");
        let t = text_content(&ac);
        assert!(t.contains("sufficiently long paragraph"), "got: {t}");
        assert!(t.contains("second paragraph"), "got: {t}");
    }

    #[test]
    fn grab_strips_unlikely_candidate_by_class() {
        // A "comment-area" div (unlikely, not okMaybe) is stripped before
        // scoring; only the article content remains.
        let html = "<html><body>\
            <div class=\"comment-area\"><p>This comment paragraph is long enough to exceed the twenty five char threshold but lives in an unlikely container.</p></div>\
            <article><p>The genuine article paragraph here is also well over twenty-five characters of real readable body prose.</p></article>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(t.contains("genuine article paragraph"), "got: {t}");
        assert!(
            !t.contains("comment paragraph"),
            "unlikely not stripped: {t}"
        );
    }

    #[test]
    fn grab_div_without_block_children_becomes_scored_p() {
        // A bare DIV of phrasing content (no block kids) is retagged P and
        // scored; selected as content.
        let html = "<html><body><div id=\"main\"><div>This is a long enough run of plain inline text inside a div with no block level children at all here.</div></div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        assert!(text_content(&ac).contains("plain inline text"));
    }

    #[test]
    fn grab_header_duplicating_title_is_removed_from_body() {
        // _headerDuplicatesTitle: an <h1> whose text ~= title (>0.75 sim) is
        // removed; it must NOT appear in the scored body.
        let html = "<html><body>\
            <h1>The Exact Article Title</h1>\
            <div class=content><p>Body prose that is comfortably beyond the twenty-five character minimum so it scores as content.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "The Exact Article Title").expect("grab");
        let t = text_content(&ac);
        assert!(t.contains("Body prose"), "got: {t}");
        assert!(
            !t.contains("The Exact Article Title"),
            "title-dup header not removed: {t}"
        );
    }

    #[test]
    fn grab_empty_body_uses_fake_div_fallback() {
        // No scorable content: body fallback creates a DIV from page children.
        // textContent ends up empty -> a valid (Bug-E2) empty extraction
        // upstream; here just assert grab returns Some with empty text.
        let html = "<html><body>   </body></html>";
        let (_d, ac) = grab(html, "").expect("fake-div fallback returns Some");
        assert_eq!(text_content(&ac).trim(), "");
    }

    #[test]
    fn grab_example_com_h1_duplicating_title_is_removed_faithful() {
        // ANTI-INVERSION FINDING (faithful, NOT tuned). The real example.com
        // snapshot's `<title>` is "Example Domain" and its `<h1>` is also
        // "Example Domain", so `_articleTitle == "Example Domain"`. In the
        // `_grabArticle` walk, `_headerDuplicatesTitle(<h1>)` computes
        // `_textSimilarity("Example Domain","Example Domain") == 1.0 > 0.75`
        // (`Readability.js:2683`) ⇒ the `<h1>` is removed at
        // `Readability.js:1112`. A FAITHFUL port therefore drops the `<h1>`
        // exactly as Readability.js does. The gold.tsv itself records that
        // "both oracles drop the <h1>" — so the gold's `<h1>` text is an
        // expected divergence-from-gold, NOT a port bug. We assert the
        // faithful behaviour (h1 absent, body present) and do NOT tune to add
        // the h1 back (HLD §4 anti-inversion; honest-STOP reported upstream).
        let html = "<html><head><title>Example Domain</title></head><body>\
            <div><h1>Example Domain</h1>\
            <p>This domain is for use in illustrative examples in documents. You may use this domain in literature without prior coordination or asking for permission.</p>\
            <p><a href=\"https://www.iana.org/domains/example\">More information...</a></p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "Example Domain").expect("grab");
        let t = text_content(&ac);
        assert!(
            !t.contains("Example Domain"),
            "faithful: title-duplicating <h1> must be removed (Readability.js:1112): {t}"
        );
        assert!(t.contains("illustrative examples"), "body missing: {t}");
    }
}
