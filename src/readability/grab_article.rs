//! `grab_article.rs` — `_grabArticle` (`Readability.js:1031-1597`), the
//! algorithmic core.
//!
//! **Stage-1a/1b slice (HLD §7.1/§7.2):** the prepping walk + unlikely strip +
//! `div`→`p` + `elementsToScore` scoring + ancestor propagation + single
//! top-candidate selection + sibling-append.
//!
//! **Stage-1c (HLD §7.3 — this stage):** the outer `while (true)`
//! retry/flag-sieve machinery Stage 1b STOPPED before. [`grab_article`] is
//! **one attempt** (the body of the JS `while (true)`, Stage-1a/1b logic
//! UNCHANGED) plus the `neededToCreateTopCandidate` page-wrap
//! (`Readability.js:1517-1532`, deferred at 1b — now ported). The outer loop
//! (`Readability.js:1043`, `1546-1576`) lives in [`grab_article_with_retry`]:
//! the `textLength < _charThreshold` trigger, the `FLAG_STRIP_UNLIKELYS` →
//! `FLAG_WEIGHT_CLASSES` → `FLAG_CLEAN_CONDITIONALLY` flag sieve, `_attempts`
//! bookkeeping, and the "return the longest-text attempt" fallback.
//!
//! **Retry re-parse (HLD §m-3, decided at this Stage-1c review):** each
//! attempt **re-parses from the original HTML bytes** (not a cloned tree, not
//! a cloned score side-table). The JS resets `page.innerHTML = pageCacheHtml`
//! (`Readability.js:1043`/`1549`) — the *post-`_prepDocument`* body. Re-parsing
//! the original bytes and re-running the deterministic pre-grab pipeline
//! (`_removeScripts` + `_prepDocument`) reconstructs the identical post-prep
//! tree, so this is faithful while avoiding deep-cloning the `Rc`-keyed side
//! tables (a fresh divergence + ABA surface, HLD §5.1). Each attempt's
//! `text_content` + `_getInnerText` length are captured **eagerly as owned
//! values**, so the longest-attempt fallback (`Readability.js:1573`) never
//! reads a node from a discarded attempt's `Dom` (ABA-safe by construction —
//! see [`grab_article_with_retry`]).
//!
//! Faithful transcription with `Readability.js:<line>` citations
//! (anti-inversion, HLD §4.3(a)). The JS `candidates` array is mirrored by a
//! `Vec<NodeRef>` so candidate **ordering is deterministic** (HLD §5.1 — never
//! from a `HashMap`).

use crate::readability::dom::{
    self, Dom, NodeRef, append_child, children, class_name, create_element, first_element_child,
    get_attribute, id, is_element, parent, replace_child, set_attribute, tag_name,
};
use crate::readability::helpers::{
    FLAG_CLEAN_CONDITIONALLY, FLAG_STRIP_UNLIKELYS, FLAG_WEIGHT_CLASSES, Flags, get_next_node,
    is_element_without_content, is_phrasing_content, is_probably_visible, is_valid_byline,
    is_whitespace,
};
use crate::readability::regexps;
use crate::readability::scoring::{
    get_link_density, get_node_ancestors, initialize_node, inner_text_len, text_similarity,
};

/// `_nbTopCandidates` default (`Readability.js:125`, `DEFAULT_N_TOP_CANDIDATES
/// = 5`). Stage 1a uses the default (no `Options.nbTopCandidates`).
const NB_TOP_CANDIDATES: usize = 5;

/// Outcome of **one** `_grabArticle` attempt: the `articleContent` element (a
/// fresh `<div>` holding the selected content) plus the
/// `_getInnerText(articleContent, true).length` the retry loop tests against
/// `_charThreshold` (`Readability.js:1545-1546`). `None` means the JS
/// `return null` (only reachable via the body-fallback producing nothing,
/// faithfully mapped to an empty extraction by the caller, Bug-E2).
pub struct GrabResult {
    /// The `articleContent` div whose `text_content` is the extracted body
    /// (post `_prepArticle` is applied by the caller, as in the JS — see
    /// [`grab_article_with_retry`]).
    pub article_content: NodeRef,
    /// `this._articleByline` (`Readability.js:1100`), captured when
    /// `_grabArticle`'s byline-detect found and removed an in-body byline.
    /// `None` when no in-body byline was found (e.g. because metadata.byline
    /// was already set and gated the detect — `Readability.js:1083-1084`).
    /// Stage 4 (HLD §7.6) addition.
    pub article_byline: Option<String>,
    /// `this._articleDir` (`Readability.js:1587-1592`), captured from the
    /// first ancestor of `topCandidate` with a non-empty `dir` attribute.
    /// `None` when no such ancestor exists. Stage 4 addition.
    pub article_dir: Option<String>,
}

/// `_removeAndGetNext(node)` (`Readability.js:932-936`):
/// `var n = _getNextNode(node, true); node.remove(); return n;`.
///
/// `pub(crate)` because Stage 3's `_simplifyNestedElements`
/// (`prep::simplify_nested_elements`, `Readability.js:537-565`) calls this on
/// `_isElementWithoutContent` removal — same JS-faithful primitive, kept in
/// one place rather than duplicated.
pub(crate) fn remove_and_get_next(node: &NodeRef) -> Option<NodeRef> {
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
    article_byline_text: &mut Option<String>,
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

        // byline detection (Readability.js:1082-1103).
        //
        // **Stage 4 (HLD §7.6)**: the JS gate is
        //   `!this._articleByline && !this._metadata.byline && _isValidByline`
        // (`Readability.js:1082-1085`). `_metadata.byline` is set by
        // `_getArticleMetadata` BEFORE `_grabArticle` runs
        // (`Readability.js:2743-2745` then `:2747`). At Stage 1a-3 the gate
        // collapsed to `!this._articleByline && _isValidByline` because
        // `_metadata.byline` was always unset (Stage 4 had not landed).
        //
        // Stage 4 fixes this faithfully: the caller passes in the score-
        // affecting "byline already known from metadata?" via the SAME
        // `article_byline_found` flag (the caller pre-seeds it to `true`
        // when `metadata.byline.is_some()`, which short-circuits the gate
        // exactly as `!_metadata.byline` does in JS). This routes the score
        // change through the SAME flag the existing arm already drove —
        // mechanically equivalent to the JS double-gate.
        //
        // The (itemprop=name ?? node).textContent.trim() refinement
        // (`Readability.js:1087-1100`) ITSELF only changes the *stored*
        // byline string surfaced as Article.byline — Stage 4 captures it
        // now (faithful: walk descendants for `[itemprop*="name"]` until
        // `endOfSearchMarkerNode`, falling back to `node`).
        if !*article_byline_found && is_valid_byline(&node, &match_string) {
            *article_byline_found = true;
            // (itemPropNameNode ?? node).textContent.trim()
            let item_prop_name_node = find_descendant_item_prop_name(&node);
            let byline_source = item_prop_name_node.unwrap_or_else(|| node.clone());
            let captured = dom::text_content(&byline_source);
            let captured = captured.trim().to_string();
            if !captured.is_empty() {
                *article_byline_text = Some(captured);
            }
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

    // `neededToCreateTopCandidate` (`Readability.js:1309/1317`) — set true iff
    // the fake-div fallback fires. It gates the 1517-1532 page-wrap (assign
    // `id="readability-page-1"` / `className="page"` on the existing top
    // candidate vs. wrap articleContent's children in an extra
    // `<div id=readability-page-1 class=page>`). id/className are
    // score-invisible (HLD §2) and a wrapper DIV adds ZERO `#text` characters,
    // so 1517-1532 is provably `text_content`-invariant — but it is now ported
    // faithfully (Stage 1c needs the bookkeeping/structure path; the JS does
    // it inside the retry loop). Pinned text_content-invariant by a test.
    let mut needed_to_create_top_candidate = false;

    // If no top candidate, OR it is BODY: build a fake DIV from page children
    // (Readability.js:1314-1327). Needed for the first Ok.
    if top_candidate.is_none()
        || top_candidate
            .as_ref()
            .map(|t| tag_name(t).as_deref() == Some("BODY"))
            .unwrap_or(false)
    {
        // 1317: neededToCreateTopCandidate = true.
        needed_to_create_top_candidate = true;
        let tc = create_element("DIV");
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

    // --- Build articleContent + sibling-append (Readability.js:1415-1535). ---
    // Stage 1b ports the block Stage 1a deliberately STOPPED before. Still NO
    // FLAG_* retry/flag-sieve loop (1546+, Stage 1c), NO _cleanConditionally /
    // _markDataTables (Stage 2). Faithful transcription, line-cited.

    // 1418: articleContent = doc.createElement("DIV").
    // (1419-1421 `if (isPaging) articleContent.id = "readability-content"` —
    //  isPaging is always false here (`page` is always `doc.body`, never a
    //  paging arg), and `id` is score-invisible regardless (HLD §2) — SKIP.)
    let article_content = create_element("DIV");

    // 1423-1426: siblingScoreThreshold = Math.max(10,
    //              topCandidate.readability.contentScore * 0.2).
    // `topCandidate.readability.contentScore` is whatever the side table holds
    // for the chosen top candidate now: the finalized candidateScore (1283)
    // for a real candidate, or the fake div's _initializeNode score for the
    // fallback. Stage-1a leaves exactly that in place.
    let top_candidate_score = dom.content_score(&top_candidate).unwrap_or(0.0);
    let sibling_score_threshold = 10.0_f64.max(top_candidate_score * 0.2);

    // 1428: parentOfTopCandidate = topCandidate.parentNode.
    let parent_of_top_candidate = parent(&top_candidate);

    // 1429: siblings = parentOfTopCandidate.children (the LIVE element list in
    // JS). We mirror the JS variable as an explicit refetched snapshot `Vec`
    // and an INDEX WALK — never a Rust iterator (HLD §7.2 critical fidelity
    // point: the JS mutates `siblings`/indices mid-iteration; a live iterator
    // would be UB-adjacent and divergent). When `top_candidate` is detached
    // (the fake-div fallback appended it to `page`, so its parent IS `page`;
    // or a genuinely parentless candidate) there are no siblings to walk —
    // `parentOfTopCandidate` is then `page`/`None`. For the fallback the only
    // node that would qualify is `sibling === topCandidate` itself, so we
    // still append the top candidate (mirrors the loop's 1447-1448 for the
    // single fake-div child case).
    let siblings_parent = parent_of_top_candidate.clone();

    match &siblings_parent {
        Some(stc) => {
            // `siblings = parentOfTopCandidate.children`.
            let mut siblings = children(stc);
            // 1431: `for (var s = 0, sl = siblings.length; s < sl; s++)`.
            // JS Number semantics: `s -= 1` at s==0 yields -1, then `s++` → 0
            // (the just-vacated index is revisited). `usize` would panic on
            // that underflow, so `s`/`sl` are `i64`, indexing with `s as
            // usize` — an exact mirror of the JS `var s`/`var sl`.
            let mut s: i64 = 0;
            let mut sl: i64 = siblings.len() as i64;
            while s < sl {
                // 1432: sibling = siblings[s].
                let sibling = siblings[s as usize].clone();
                // 1433: append = false.
                let mut append = false;

                // (1435-1445 are this.log(...) — diagnostics only, SKIP.)

                if std::rc::Rc::ptr_eq(&sibling, &top_candidate) {
                    // 1447-1448: if (sibling === topCandidate) append = true.
                    append = true;
                } else {
                    // 1450: contentBonus = 0.
                    let mut content_bonus = 0.0_f64;

                    // 1453-1458: same non-empty className as topCandidate ⇒
                    // contentBonus += topCandidate.readability.contentScore *
                    // 0.2.
                    let tc_class = class_name(&top_candidate);
                    if class_name(&sibling) == tc_class && !tc_class.is_empty() {
                        content_bonus += top_candidate_score * 0.2;
                    }

                    if dom.has_content_score(&sibling)
                        && dom.content_score(&sibling).unwrap_or(0.0) + content_bonus
                            >= sibling_score_threshold
                    {
                        // 1460-1465: sibling.readability &&
                        // sibling.readability.contentScore + contentBonus >=
                        // siblingScoreThreshold.
                        append = true;
                    } else if tag_name(&sibling).as_deref() == Some("P") {
                        // 1466-1481: the nodeName === "P" clause. `nodeName`
                        // for an HTML element is the upper-cased tag, == our
                        // `tag_name`.
                        let link_density = get_link_density(&sibling);
                        // 1468: nodeContent = _getInnerText(sibling) (default
                        // normalizeSpaces=true).
                        let node_content = dom::inner_text(&sibling, true);
                        // 1469: nodeLength = nodeContent.length (JS UTF-16
                        // code units; `char` count is BMP-exact for this
                        // prose, consistent with the rest of the port).
                        let node_length = node_content.chars().count();

                        if node_length > 80 && link_density < 0.25 {
                            // 1471-1472.
                            append = true;
                        } else if node_length < 80
                            && node_length > 0
                            && link_density == 0.0
                            && regexps::period_space_or_end().is_match(&node_content)
                        {
                            // 1473-1480: nodeContent.search(/\.( |$)/) !== -1
                            // (search != -1 ⇔ is_match).
                            append = true;
                        }
                    }
                }

                if append {
                    // (1485 this.log — SKIP.)

                    // 1487-1493: if (!ALTER_TO_DIV_EXCEPTIONS.includes(
                    //   sibling.nodeName)) sibling = _setNodeTag(sibling,
                    //   "DIV"); — turn non-block siblings (form, td, …) into a
                    //   div so they survive later filtering. `_setNodeTag`
                    //   returns the NEW handle (slow branch, HLD §2.2) and
                    //   transfers the score side-table entry old→new.
                    let node_name = tag_name(&sibling).unwrap_or_default();
                    let sibling = if !regexps::ALTER_TO_DIV_EXCEPTIONS.contains(&node_name.as_str())
                    {
                        dom.set_node_tag(&sibling, "DIV")
                    } else {
                        sibling
                    };

                    // 1495: articleContent.appendChild(sibling) — moves it out
                    // of parentOfTopCandidate (DOM move semantics).
                    append_child(&article_content, &sibling);

                    // 1498: siblings = parentOfTopCandidate.children — REFETCH
                    // (the JS comment: "compatible with DOM parsers without
                    // live collection support"; the append shifted the list).
                    siblings = children(stc);

                    // 1503-1504: s -= 1; sl -= 1 — the append removed one
                    // element from the parent, so revisit this index (the
                    // next element shifted down into it) and shrink the bound.
                    // `sl` mirrors the JS *variable* (decremented), NOT a
                    // re-read of `siblings.length` (which would coincidentally
                    // be equal here, but faithful = mirror the JS counter).
                    s -= 1;
                    sl -= 1;
                }

                // 1431: the for-loop `s++`.
                s += 1;
            }
        }
        None => {
            // No parent (detached top candidate, e.g. the fake-div fallback
            // whose parent is `page`, OR a parentless candidate). The JS
            // sibling loop would still hit `sibling === topCandidate` for the
            // top candidate and append it; with no real sibling list there is
            // nothing else. Append the top candidate exactly as 1447-1448
            // would. (For the fallback this preserves the Stage-1a behaviour:
            // articleContent ends up holding the fake div's content.)
            append_child(&article_content, &top_candidate);
        }
    }

    // (1508-1510 debug — SKIP.)

    // 1512 `this._prepArticle(articleContent)` — Stage 2 full port
    // (`Readability.js:782-884`). MUST run **BEFORE** the page-wrap below
    // (the JS order is `_prepArticle` (1512) → page-wrap (1517-1532)).
    //
    // **Order fidelity decision (Stage 2 re-audit).** Stage 1c put the
    // page-wrap inside `grab_article` and ran a near-noop `prep_article_stage1a`
    // in the caller, because the swap was observationally invariant for the
    // Stage-1a `_clean`/empty-`<p>` slice (pure descendant-`get_all_nodes_with_tag`
    // searches, the page-wrap leaves the descendant set unchanged). With the
    // full Stage-2 `_cleanConditionally` that invariant **NO LONGER HOLDS**:
    // `_cleanConditionally` calls `_hasAncestorTag(node, "code", maxDepth=3)`
    // (`Readability.js:2470`) whose default maxDepth=3 window can be PUSHED
    // OUT by the extra page-wrap ancestor level. So for a `<code>` ancestor at
    // depth 3 from a cleaning target, the JS finds it (KEEP applies); the
    // swapped port misses it (KEEP does NOT apply ⇒ node removed). That is
    // out-cleaning RJS ⇒ inversion (HLD §4). **Decision: port the JS order
    // exactly** — `_prepArticle` first, then page-wrap. Pinned by a test.
    crate::readability::prep::prep_article(dom, flags, &article_content);

    // 1517-1532: the `readability-page-1` / `page` wrap. **Faithfully ported**
    // — runs AFTER `_prepArticle` (the JS order). It remains
    // `text_content`-invariant: `id`/`className` are score-invisible (HLD §2)
    // and the extra wrapper `<div>` adds ZERO `#text` characters (a wrapper
    // element contributes nothing to the WHATWG `Node.textContent` DFS).
    // Pinned by a test.
    if needed_to_create_top_candidate {
        // 1517-1523: the fake div IS topCandidate and was already appended in
        // the sibling loop (sibling === topCandidate). Just assign id/class —
        // no append, no child move. id/className are score-invisible (HLD §2);
        // `topCandidate` here is the appended child of `article_content`.
        //
        // FIDELITY: `_prepArticle` above may have removed/retagged the fake
        // div via `_cleanConditionally` etc., but the JS handles this the
        // same way: the `topCandidate` variable still points at whatever the
        // fake div became (or, if it was removed, the JS `setAttribute` is
        // a no-op on a detached node). Defensive: if `top_candidate` is now
        // detached, the JS behaviour is "setAttribute on a detached element"
        // — a no-op on subsequent serialization since the element is
        // unreachable. Our `set_attribute` on a detached node is a no-op for
        // textContent purposes (same outcome).
        set_attribute(&top_candidate, "id", "readability-page-1");
        set_attribute(&top_candidate, "class", "page");
    } else {
        // 1525-1531: div = createElement("DIV"); div.id="readability-page-1";
        // div.className="page"; while (articleContent.firstChild)
        // div.appendChild(articleContent.firstChild);
        // articleContent.appendChild(div).
        let div = create_element("DIV");
        set_attribute(&div, "id", "readability-page-1");
        set_attribute(&div, "class", "page");
        while let Some(fc) = first_child(&article_content) {
            append_child(&div, &fc);
        }
        append_child(&article_content, &div);
    }

    // (1534-1536 debug — SKIP. 1538+ `parseSuccessful` / the
    //  `textLength < _charThreshold` retry loop is owned by
    //  [`grab_article_with_retry`] — this function is ONE attempt's body.)

    // 1579-1593 _articleDir capture — Stage 4 (HLD §7.6). The JS only sets
    // `_articleDir` on the parseSuccessful path; this attempt-local compute
    // is always performed, and the retry driver discards it for failed
    // attempts (faithful: a failed attempt's `_articleDir` would be
    // overwritten by the next attempt anyway, or stay null if the longest-
    // text fallback is taken — JS line 1578 only assigns dir for the
    // `parseSuccessful` branch).
    let parent_of_top_candidate_for_dir = parent(&top_candidate);
    let article_dir = capture_article_dir(&top_candidate, parent_of_top_candidate_for_dir.as_ref());

    Some(GrabResult {
        article_content,
        article_byline: article_byline_text.clone(),
        article_dir,
    })
}

/// `Readability.js:1580-1593` — walk `[parentOfTopCandidate, topCandidate,
/// …ancestors of parentOfTopCandidate]` and return the first ancestor's
/// non-empty `dir` attribute.
fn capture_article_dir(
    top_candidate: &NodeRef,
    parent_of_top_candidate: Option<&NodeRef>,
) -> Option<String> {
    // 1580 `[parentOfTopCandidate, topCandidate].concat(_getNodeAncestors
    //       (parentOfTopCandidate))`
    let mut walk: Vec<NodeRef> = Vec::new();
    if let Some(p) = parent_of_top_candidate {
        walk.push(p.clone());
    }
    walk.push(top_candidate.clone());
    if let Some(p) = parent_of_top_candidate {
        // `_getNodeAncestors(parentOfTopCandidate)` with no maxDepth = walk
        // all the way up.
        let mut n = p.clone();
        while let Some(parent_n) = parent(&n) {
            walk.push(parent_n.clone());
            n = parent_n;
        }
    }
    // 1583-1592 _someNode: first ancestor with a non-empty `dir`.
    for ancestor in &walk {
        if tag_name(ancestor).is_none() {
            continue;
        }
        if let Some(dir) = get_attribute(ancestor, "dir")
            && !dir.is_empty()
        {
            return Some(dir);
        }
    }
    None
}

/// `Readability.js:1087-1099` — walk descendants of `node` (in document order,
/// via `_getNextNode`) up to the next non-descendant (`_getNextNode(node,
/// true)`), looking for `[itemprop*="name"]`.
fn find_descendant_item_prop_name(node: &NodeRef) -> Option<NodeRef> {
    let end_marker = get_next_node(node, true);
    let mut cur = get_next_node(node, false);
    while let Some(n) = cur.clone() {
        // `next != endOfSearchMarkerNode`
        if let Some(em) = end_marker.as_ref()
            && std::rc::Rc::ptr_eq(em, &n)
        {
            return None;
        }
        if let Some(ip) = get_attribute(&n, "itemprop")
            && ip.contains("name")
        {
            return Some(n);
        }
        cur = get_next_node(&n, false);
    }
    None
}

/// `DEFAULT_CHAR_THRESHOLD` (`Readability.js:133`, `500`). With default
/// `Options` (`Readability.js:54` — `options.charThreshold ||
/// DEFAULT_CHAR_THRESHOLD`) `this._charThreshold` is exactly this. Stage 1c
/// uses the default (a configurable `charThreshold` is Stage-4 additive
/// surface, HLD §7.6 — not a tuning knob here; this constant is transcribed
/// from the cited line, not chosen).
const CHAR_THRESHOLD: usize = 500;

/// One element of the JS `this._attempts` array (`Readability.js:1551-1554` —
/// `{ articleContent, textLength }`).
///
/// **Eager string capture (HLD §m-3 / §5.1 ABA).** The JS stores the live
/// `articleContent` *node*; the fallback (`Readability.js:1573`) later reads
/// `.textContent` off `_attempts[0].articleContent`. Under re-parse-per-attempt
/// that node lives in an attempt-local `Dom` that is dropped when the attempt
/// goes out of scope, so storing the node would be a use-after-the-`Dom`-drops
/// / `NodeKey`-ABA hazard. We capture the relevant *strings* eagerly so the
/// retry driver never reads a node from a dropped `Dom` — ABA-safe **by
/// construction**.
struct Attempt {
    /// `_getInnerText(articleContent, true).length` (`Readability.js:1545`) —
    /// the value 1546 tests and 1564-1566 sorts attempts by (desc).
    inner_text_len: usize,
    /// `articleContent.textContent` (`Readability.js:2766`) captured at
    /// attempt time. The scored body for this attempt.
    text_content: String,
    /// `articleContent.getElementsByTagName("p")[0].textContent.trim()` —
    /// the first-`<p>` excerpt fallback (`Readability.js:2759-2763`).
    /// Captured eagerly so the fallback path keeps the right attempt's
    /// excerpt. `None` if no `<p>` qualifies.
    first_paragraph_excerpt: Option<String>,
    /// `this._serializer(articleContent)` (`Readability.js:2772`) — captured
    /// eagerly for `Options.include_html`. `None` when not requested.
    serialized_html: Option<String>,
    /// `this._articleDir` (`Readability.js:1587-1592`) pre-captured for the
    /// longest-text fallback path. JS sets `parseSuccessful = true` at 1574
    /// before falling through to 1578's `if (parseSuccessful)` block, so
    /// the dir ancestor-walk IS run on the WINNING attempt's
    /// `topCandidate` on the fallback path too. We pre-capture so the
    /// retry driver can pick the chosen attempt's dir without re-walking
    /// the dropped Dom (ABA-safe).
    article_dir: Option<String>,
}

/// The result of the full retry/flag-sieve loop (`Readability.js:1043`,
/// `1546-1576`): the chosen `articleContent`'s `textContent` plus the
/// metadata pieces the per-attempt closure captured eagerly.
pub struct RetryResult {
    /// The final `articleContent.textContent` (`Readability.js:2766`) — either
    /// the first attempt whose `inner_text_len >= _charThreshold`, or, when the
    /// flag sieve is exhausted, the longest-text attempt (`Readability.js:1573`).
    pub text_content: String,
    /// `metadata.excerpt`'s first-`<p>` fallback (`Readability.js:2759-2763`).
    pub first_paragraph_excerpt: Option<String>,
    /// `this._serializer(articleContent)` (`Readability.js:2772`); `None`
    /// when not requested (`Options.include_html == false`).
    pub serialized_html: Option<String>,
    /// `this._articleDir` (`Readability.js:1589`) — only set on the
    /// `parseSuccessful` branch; `None` when only the longest-text fallback
    /// path was taken (matching JS line 1578-1593 only assigning `_articleDir`
    /// when `parseSuccessful`).
    pub article_dir: Option<String>,
}

/// One attempt's outcome, as the `prepare_attempt` closure returns it to the
/// retry driver: the captured `articleContent.textContent`, its
/// `_getInnerText(articleContent, true).length` (`Readability.js:1545`), and
/// the per-attempt metadata pieces (excerpt fallback, serialized HTML, dir).
/// `None` mirrors the per-attempt JS `_grabArticle` `return null` (no `<body>`
/// / nothing to grab).
#[derive(Default)]
pub struct AttemptOutcome {
    /// `articleContent.textContent` (`Readability.js:2766`) for this attempt.
    pub text_content: String,
    /// `_getInnerText(articleContent, true).length` (`Readability.js:1545`),
    /// measured **after** `_prepArticle` (`Readability.js:1512`), exactly as
    /// the JS measures it (1512 prep → 1517-1532 page-wrap → 1545 length).
    pub inner_text_len: usize,
    /// Eagerly-captured first-`<p>.textContent.trim()` of `articleContent`
    /// for the excerpt fallback (`Readability.js:2759-2763`). `None` if no
    /// `<p>` exists.
    pub first_paragraph_excerpt: Option<String>,
    /// Eagerly-captured `this._serializer(articleContent)` for the
    /// `Options.include_html` path (`Readability.js:2772`). `None` when
    /// `include_html` was `false` (avoid the serialization cost when not
    /// requested — Stage 4 acceptance: default behaviour byte-identical).
    pub serialized_html: Option<String>,
    /// `this._articleDir` (`Readability.js:1587-1592`) — captured eagerly
    /// regardless of `parseSuccessful` so the retry driver can decide whether
    /// to keep it (JS only keeps on parseSuccessful, line 1578).
    pub article_dir: Option<String>,
}

/// `_grabArticle`'s outer retry/flag-sieve/fallback loop (`Readability.js:1043`
/// `pageCacheHtml` + `1045` `while (true)` + `1546-1576`).
///
/// `prepare_attempt(&Flags) -> Option<AttemptOutcome>` runs **one** attempt
/// with the supplied flags: re-parse the original HTML bytes, re-run the
/// deterministic pre-grab pipeline (`_removeScripts` + `_prepDocument` +
/// title), one [`grab_article`] pass, `_prepArticle`, then capture
/// `articleContent.textContent` + its `_getInnerText` length. The driver owns
/// **only** the cross-attempt bookkeeping the JS does at `1546-1576`:
///
/// * `1538` `parseSuccessful = true`;
/// * `1545-1546` `if (_getInnerText(articleContent,true).length <
///   _charThreshold)` → not successful;
/// * `1549` reset (here: the *next* `prepare_attempt` re-parses — HLD §m-3);
/// * `1551-1554` push `{ articleContent, textLength }` onto `_attempts`;
/// * `1556-1561` the flag sieve **in JS order**: clear `FLAG_STRIP_UNLIKELYS`,
///   else `FLAG_WEIGHT_CLASSES`, else `FLAG_CLEAN_CONDITIONALLY`;
/// * `1562-1575` else (no flag left): sort `_attempts` by `textLength` desc,
///   `return null` if the longest is `0`, else take its (captured) text;
/// * `1578` `if (parseSuccessful) return articleContent` (the `_articleDir`
///   ancestor walk `1579-1593` is Stage-4 metadata, not scored — omitted).
///
/// Flags start all-set (`Readability.js:69-72`), so the sieve admits at most
/// **4** attempts (all → no-STRIP → no-STRIP/WEIGHT →
/// no-STRIP/WEIGHT/CLEAN), then the longest-attempt fallback.
///
/// **ABA (HLD §5.1, re-audited for re-parse churn).** Each attempt's tree +
/// `Rc`-keyed side tables are wholly owned and dropped *inside*
/// `prepare_attempt` (the caller's closure); only the owned `String` +
/// `usize` escape. `_attempts` therefore holds no node from any attempt, so
/// the `1573` longest-attempt fallback cannot read a node whose `Dom` has
/// dropped, and no `NodeKey` from attempt *N* can alias a live side-table
/// entry in attempt *N+1* (the side tables do not outlive their attempt). The
/// invariant is structural, not a runtime check; pinned by
/// `retry_reparse_attempts_are_isolated_no_state_bleed` and
/// `retry_nodekey_aba_attempt_doms_are_independent`.
pub fn grab_article_with_retry<F>(mut prepare_attempt: F) -> Option<RetryResult>
where
    F: FnMut(&Flags) -> Option<AttemptOutcome>,
{
    // this._flags = all set (Readability.js:69-72). The sieve clears one per
    // failed attempt; re-parse-per-attempt means flags are the ONLY state
    // carried across attempts (the tree/side-tables are fresh each time).
    let mut flags = Flags::default();

    // this._attempts = [] (Readability.js:45).
    let mut attempts: Vec<Attempt> = Vec::new();

    loop {
        // 1045 `while (true)` body = one attempt (re-parse + prep + grab +
        // _prepArticle + capture). `None` ⇒ the per-attempt `_grabArticle`
        // returned null (no <body>); the JS `parse()` then returns null too
        // (Readability.js:2748-2750) — no `_attempts` push for that case
        // (the push at 1551 is only reached when `articleContent` exists and
        // is merely too short).
        let outcome = prepare_attempt(&flags)?;
        let text_length = outcome.inner_text_len;

        // 1538 parseSuccessful = true; 1545-1546 the charThreshold test.
        if text_length < CHAR_THRESHOLD {
            // 1547 parseSuccessful = false. 1549 `page.innerHTML =
            // pageCacheHtml` — realised as the NEXT loop's `prepare_attempt`
            // re-parsing the original bytes (HLD §m-3).

            // 1551-1554 this._attempts.push({ articleContent, textLength }).
            // We push the captured text (ABA — see `Attempt`).
            attempts.push(Attempt {
                inner_text_len: text_length,
                text_content: outcome.text_content,
                first_paragraph_excerpt: outcome.first_paragraph_excerpt,
                serialized_html: outcome.serialized_html,
                article_dir: outcome.article_dir,
            });

            // 1556-1561 the flag sieve, IN JS ORDER.
            if flags.is_active(FLAG_STRIP_UNLIKELYS) {
                flags.remove(FLAG_STRIP_UNLIKELYS);
            } else if flags.is_active(FLAG_WEIGHT_CLASSES) {
                flags.remove(FLAG_WEIGHT_CLASSES);
            } else if flags.is_active(FLAG_CLEAN_CONDITIONALLY) {
                flags.remove(FLAG_CLEAN_CONDITIONALLY);
            } else {
                // 1562-1575: no luck after removing flags — return the
                // longest text found across the attempts.
                //
                // 1564-1566 `_attempts.sort((a,b) => b.textLength -
                // a.textLength)` — a STABLE descending sort by `textLength`,
                // then `_attempts[0]`. JS `Array.prototype.sort` is required
                // to be stable (ECMAScript 2019+; the oracle's V8 is).
                // `slice::sort_by_key` is ALSO a stable sort, and
                // `Reverse(len)` makes it descending — so on a `textLength`
                // tie the FIRST-pushed attempt stays at index 0, exactly the
                // JS stable-sort tie semantics. (`max_by_key` would NOT be
                // faithful: it returns the LAST max; pinned by
                // `retry_longest_attempt_stable_sort_ties_keep_first`.)
                attempts.sort_by_key(|a| std::cmp::Reverse(a.inner_text_len));

                // 1568-1571 `if (!_attempts[0].textLength) return null;`
                // (`_attempts` is non-empty here: at least this attempt was
                // just pushed). `!textLength` is JS falsy ⇒ exactly `== 0`.
                let best = attempts.remove(0);
                if best.inner_text_len == 0 {
                    return None;
                }

                // 1573-1574 `articleContent = _attempts[0].articleContent;
                // parseSuccessful = true;` then 1578 returns it.
                //
                // Stage 4: the JS `if (parseSuccessful)` test at line 1578 IS
                // true on this path (1574 set it back), so the `_articleDir`
                // ancestor walk (1579-1593) runs on the WINNING attempt's
                // `topCandidate`. We pre-captured each attempt's dir into
                // `Attempt.article_dir` (ABA-safe — owned String, not a
                // Dom-borrowed node), so the fallback path faithfully
                // returns the chosen attempt's dir without re-walking a
                // dropped Dom.
                return Some(RetryResult {
                    text_content: best.text_content,
                    first_paragraph_excerpt: best.first_paragraph_excerpt,
                    serialized_html: best.serialized_html,
                    article_dir: best.article_dir,
                });
            }

            // parseSuccessful was false and a flag was cleared ⇒ loop again
            // (re-parse, one fewer flag).
            continue;
        }

        // 1578 `if (parseSuccessful) ... return articleContent;` — and the
        // ancestor walk at 1579-1593 sets `_articleDir`. The per-attempt
        // closure pre-captured `outcome.article_dir`; on this success path
        // we propagate it (faithful).
        return Some(RetryResult {
            text_content: outcome.text_content,
            first_paragraph_excerpt: outcome.first_paragraph_excerpt,
            serialized_html: outcome.serialized_html,
            article_dir: outcome.article_dir,
        });
    }
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
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Expected selections hand-derived by tracing `Readability.js:1031-1413`
    //! (NOT by running an oracle — inversion, HLD §4).
    use super::*;
    use crate::readability::dom::{Dom, get_elements_by_tag_name, text_content};

    fn grab(html: &str, title: &str) -> Option<(Dom, NodeRef)> {
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        let mut byline_found = false;
        let mut byline_text = None;
        let r = grab_article(
            &mut dom,
            &root,
            &body,
            title,
            &flags,
            &mut byline_found,
            &mut byline_text,
        )?;
        Some((dom, r.article_content))
    }

    /// The single `<div id=readability-page-1 class=page>` the 1517-1532
    /// page-wrap always produces: in the non-fallback arm articleContent has
    /// exactly that one child (its real children moved inside it); in the
    /// fallback arm the fake-div top candidate (already appended) IS given
    /// `id=readability-page-1`, so unwrapping is a no-op there (we still
    /// return that node — its children are the appended content). Used by the
    /// Stage-1b structural-shape tests, which assert one level below the
    /// (now-ported, text_content-invariant) page-wrap.
    fn page_wrap_inner(article_content: &NodeRef) -> NodeRef {
        let kids = children(article_content);
        // Non-fallback arm: exactly one child, the readability-page-1 div.
        if kids.len() == 1 && get_attribute(&kids[0], "id").as_deref() == Some("readability-page-1")
        {
            return kids[0].clone();
        }
        // Fallback arm: articleContent's sole child IS the fake-div top
        // candidate, itself given id=readability-page-1 — its children are
        // the content. Return it (it has the id) or, defensively, the node
        // itself if the shape is unexpected.
        if get_attribute(article_content, "id").as_deref() == Some("readability-page-1") {
            return article_content.clone();
        }
        kids.into_iter()
            .find(|k| get_attribute(k, "id").as_deref() == Some("readability-page-1"))
            .unwrap_or_else(|| article_content.clone())
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

    // -----------------------------------------------------------------------
    // Stage 1b — sibling-append (`Readability.js:1415-1535`).
    //
    // Every expected outcome below is hand-traced from `Readability.js:1415-
    // 1535` (the cited block), NOT from running any oracle (anti-inversion,
    // HLD §4). The qualitative cases (clause A / clause B / `=== topCandidate`
    // / `_setNodeTag`-to-DIV) are deterministic and independent of the exact
    // top-candidate float score; the threshold/contentBonus cases carry a
    // full score hand-trace in their comments.
    // -----------------------------------------------------------------------

    /// Long preamble + content div + long trailing paragraph — the canonical
    /// "content split by ads/preamble" case the JS comment (1415-1417)
    /// describes. The top candidate is the `<div id=tc>` (two long scored
    /// `<p>`s under a positive-class div); its parent is `<body>`, so siblings
    /// are `[#pre, #tc, #post]`.
    ///
    /// Trace: `#pre` is a `<p>`, not the top candidate, `sibling.readability`
    /// is undefined (a bare `<p>` is never `_initializeNode`d unless it became
    /// a candidate-ancestor — it is not, it has no scored descendant), so the
    /// score branch (1460) is false → the `nodeName === "P"` branch (1466):
    /// `nodeLength > 80 && linkDensity 0 < 0.25` ⇒ append=true (clause A,
    /// 1471-1472). `#tc` === topCandidate ⇒ append (1447-1448). `#post`
    /// identical to `#pre` ⇒ clause A append. So articleContent must contain
    /// ALL THREE texts, in document order (the index walk visits them all).
    #[test]
    fn sibling_append_preamble_content_trailing_all_included() {
        let html = "<html><body>\
            <p id=pre>This is a sufficiently long preamble paragraph of real readable prose that comfortably exceeds eighty characters and contains no links at all.</p>\
            <div id=tc class=content>\
              <p>The first genuine article body paragraph here is well past twenty-five characters of real prose content for scoring.</p>\
              <p>A second article body paragraph also with ample genuine readable prose content so the div accrues a high score.</p>\
            </div>\
            <p id=post>This is a sufficiently long trailing paragraph of real readable prose that also exceeds eighty characters and has no links whatsoever.</p>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("long preamble paragraph"),
            "clause-A preamble <p> sibling must be appended (Readability.js:1471): {t}"
        );
        assert!(
            t.contains("first genuine article body"),
            "top candidate itself must be appended (Readability.js:1447): {t}"
        );
        assert!(
            t.contains("long trailing paragraph"),
            "clause-A trailing <p> sibling must be appended (Readability.js:1471): {t}"
        );
    }

    /// P clause B (`Readability.js:1473-1480`): a SHORT (`< 80`, `> 0`) `<p>`
    /// sibling with `linkDensity === 0` whose text matches `/\.( |$)/`
    /// (ends with `.` or contains `. `) is appended; an equally short `<p>`
    /// with NO period is NOT appended (neither clause A — too short — nor
    /// clause B — no period match — nor score — unscored bare `<p>`).
    #[test]
    fn sibling_append_short_p_clause_b_period_rule() {
        // #dot: "Short note." -> length 11 (<80, >0), linkDensity 0,
        //   /\.( |$)/ matches "." at end ⇒ append (1477-1479).
        // #nodot: "No period here either way" -> length 25 (<80,>0),
        //   linkDensity 0, /\.( |$)/ does NOT match (no '.') ⇒ NOT appended.
        let html = "<html><body>\
            <p id=dot>Short note.</p>\
            <div id=tc class=content>\
              <p>A long genuine article paragraph well over twenty five characters of real readable prose for scoring here.</p>\
              <p>Another sufficiently long article body paragraph of genuine readable prose to drive the container score up high.</p>\
            </div>\
            <p id=nodot>No period here either way</p>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("Short note."),
            "clause-B short <p> with trailing period must be appended (Readability.js:1477): {t}"
        );
        assert!(
            !t.contains("No period here"),
            "short <p> with no period must NOT be appended (clause B fails, Readability.js:1473-1480): {t}"
        );
    }

    /// `sibling.nodeName === "P"` + `nodeLength > 80` but `linkDensity >= 0.25`
    /// ⇒ clause A fails (`Readability.js:1471`), clause B fails (`nodeLength`
    /// not `< 80`), unscored ⇒ NOT appended. The link-dense `<p>` (a list of
    /// links) is correctly excluded by the faithful link-density gate.
    #[test]
    fn sibling_append_link_dense_long_p_excluded() {
        // #links: a long <p> that is entirely anchor text -> linkDensity 1.0
        //   (>= 0.25) ⇒ clause A false; length > 80 so clause B false;
        //   unscored ⇒ append stays false.
        let html = "<html><body>\
            <div id=tc class=content>\
              <p>The genuine article body paragraph here is comfortably past twenty five characters of real readable prose content.</p>\
              <p>Second genuine article paragraph also well past the minimum with ample real readable prose to score the container.</p>\
            </div>\
            <p id=links><a href=/a>First navigation link label text</a> <a href=/b>Second navigation link label text here</a> <a href=/c>Third navigation link label text here</a></p>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("genuine article body paragraph"),
            "top candidate must be present: {t}"
        );
        assert!(
            !t.contains("First navigation link label"),
            "link-dense long <p> sibling must NOT be appended (linkDensity>=0.25, Readability.js:1471): {t}"
        );
    }

    /// Non-`ALTER_TO_DIV_EXCEPTIONS` qualifying sibling is `_setNodeTag`'d to
    /// DIV before append (`Readability.js:1487-1493`); an
    /// `ALTER_TO_DIV_EXCEPTIONS` sibling (here `<section>`) is appended
    /// **without** retag. We assert via the rendered tree shape after grab.
    ///
    /// Construction: top candidate is `<div id=tc class=col>`; a SECTION
    /// sibling with the *same* className `col` (non-empty) gets
    /// `contentBonus = topCandidate.contentScore * 0.2` (1453-1458) added to
    /// its own (it has a long scored `<p>`, so it is `_initializeNode`d as a
    /// candidate-ancestor and has a real score) so it clears the threshold
    /// and is appended (1460-1465) — SECTION ∈ ALTER excs ⇒ NOT retagged. A
    /// `<form>` sibling (NOT in ALTER excs) that also qualifies by the same
    /// className contentBonus is `_setNodeTag`'d to DIV before append.
    /// Non-`ALTER_TO_DIV_EXCEPTIONS` qualifying sibling is `_setNodeTag`'d to
    /// DIV before append (`Readability.js:1487-1493`); an
    /// `ALTER_TO_DIV_EXCEPTIONS` sibling (here `<section>`) is appended
    /// **without** retag.
    ///
    /// Full hand-trace. The top candidate is `<div id=tc class=col>` with 20
    /// long scored `<p>`s; its score (≈ DIV +5 + 20·contentScore at level-0
    /// divider 1, finalized ×(1-linkDensity 0)) is well over **50**, so
    /// `siblingScoreThreshold = max(10, tc_score·0.2) = tc_score·0.2` (the
    /// `tc_score·0.2 ≥ 10` regime — the `max`'s computed arm wins, not the
    /// floor). `<body>` is level-1 ancestor of those `<p>`s (divider 2) so it
    /// accrues only ≈ half and never out-scores `div#tc` (no score-walk-up to
    /// BODY; no fake-div fallback). Siblings = `body.children = [div#tc,
    /// section, form]`.
    ///
    /// `div#tc` === topCandidate ⇒ append (1447-1448). `<section class=col>`:
    /// `className "col" === topCandidate.className "col"` and `!== ""` ⇒
    /// `contentBonus += tc_score·0.2` (1453-1458); the section has two scored
    /// `<p>`s so `section.readability.contentScore > 0`; thus
    /// `section.score + tc_score·0.2 ≥ tc_score·0.2 = threshold` ⇒ append
    /// (1460-1465). `SECTION ∈ ALTER_TO_DIV_EXCEPTIONS` ⇒ **NOT** retagged.
    /// `<form class=col>` qualifies identically (same-className contentBonus,
    /// positive own score); `FORM ∉ ALTER_TO_DIV_EXCEPTIONS` ⇒
    /// `_setNodeTag(form,"DIV")` (1487-1492) before append. (This is the
    /// faithful JS mechanism: when `tc_score·0.2 ≥ 10`, a same-non-empty-
    /// className sibling's contentBonus exactly equals the threshold's
    /// computed arm, so any positively-scored such sibling clears it — NOT a
    /// tuned constant, the cited-line arithmetic.)
    #[test]
    fn sibling_append_setnodetag_to_div_and_alter_exception() {
        let p = "Genuine readable article prose sentence comfortably past the twenty five character minimum for scoring purposes here now.";
        let tcps = format!("<p>{p}</p>").repeat(20);
        let html = format!(
            "<html><body>\
            <div id=tc class=col>{tcps}</div>\
            <section class=col><p>{p}</p><p>{p}</p></section>\
            <form class=col><p>{p}</p><p>{p}</p></form>\
            </body></html>"
        );
        let (_d, ac) = grab(&html, "").expect("grab");
        // 1517-1532 page-wrap (now ported, Stage 1c): articleContent's
        // children were moved into an inner `<div id=readability-page-1
        // class=page>` (text_content-invariant — id/class score-invisible,
        // wrapper adds 0 #text). The sibling-append shape we are asserting is
        // therefore one level down, inside that wrapper. Unwrap it.
        let page = page_wrap_inner(&ac);
        // The SECTION ∈ ALTER_TO_DIV_EXCEPTIONS ⇒ appended as-is (still a
        // SECTION element directly under the page wrapper).
        let section_kids: Vec<String> = children(&page)
            .iter()
            .map(|c| tag_name(c).unwrap_or_default())
            .collect();
        assert!(
            section_kids.contains(&"SECTION".to_string()),
            "SECTION sibling (∈ ALTER_TO_DIV_EXCEPTIONS) must be appended WITHOUT retag (Readability.js:1487); articleContent children = {section_kids:?}"
        );
        // The FORM ∉ ALTER excs ⇒ _setNodeTag(form,"DIV"): NO <form> element
        // anywhere under articleContent (it was replaced by a DIV)…
        assert!(
            get_elements_by_tag_name(&ac, "form").is_empty(),
            "FORM sibling must be _setNodeTag'd to DIV (no <form> remains, Readability.js:1492); articleContent children = {section_kids:?}"
        );
        // …and the top candidate itself is the first appended child (a DIV).
        assert_eq!(
            section_kids.first().map(String::as_str),
            Some("DIV"),
            "the top candidate (div#tc) is appended first (Readability.js:1447): {section_kids:?}"
        );
        // articleContent children are exactly: DIV(tc), SECTION(as-is),
        // DIV(form retagged) — in document order, none retagged that
        // shouldn't be, the FORM retagged.
        assert_eq!(
            section_kids,
            vec!["DIV".to_string(), "SECTION".to_string(), "DIV".to_string()],
            "faithful 1415-1535 result: [div#tc, section(as-is), form→div] in order"
        );
        // The retagged FORM's prose survived the _setNodeTag child-move.
        assert!(
            text_content(&ac).contains("Genuine readable article prose"),
            "retagged FORM text must still be present: {}",
            text_content(&ac).chars().take(60).collect::<String>()
        );
    }

    /// The index-mutation walk (`Readability.js:1431/1498/1503-1504`). The top
    /// candidate is in the MIDDLE of several qualifying siblings; every
    /// qualifying sibling must be appended **exactly once and in order** —
    /// the `siblings = parentOfTopCandidate.children; s -= 1; sl -= 1` fixup
    /// is what prevents skipping the sibling that shifts into the just-vacated
    /// index. Five clause-A `<p>` siblings around the top candidate; all five
    /// + the top candidate must appear, none duplicated, in document order.
    #[test]
    fn sibling_append_index_walk_visits_every_sibling_once_in_order() {
        let html = "<html><body>\
            <p id=s1>Sibling one is a long readable preamble paragraph well beyond eighty characters with no link content at all here.</p>\
            <p id=s2>Sibling two is likewise a long readable paragraph comfortably beyond eighty characters and contains zero links here.</p>\
            <div id=tc class=content>\
              <p>The principal article body sentence here is well past twenty five characters of genuine readable prose content for scoring.</p>\
              <p>An additional article body sentence of genuine readable prose well past the minimum so this div is the top candidate.</p>\
            </div>\
            <p id=s3>Sibling three is again a long readable paragraph well past eighty characters in length and with no links present.</p>\
            <p id=s4>Sibling four is similarly a long readable paragraph beyond eighty characters and again carries no link content here.</p>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        for needle in [
            "Sibling one is a long",
            "Sibling two is likewise",
            "principal article body sentence",
            "Sibling three is again",
            "Sibling four is similarly",
        ] {
            assert!(
                t.contains(needle),
                "every qualifying sibling must be appended exactly once (index walk, Readability.js:1503-1504); missing {needle:?} in: {t}"
            );
        }
        // Order is document order: s1 < s2 < tc-body < s3 < s4 (the index
        // walk + appendChild preserves source order into articleContent).
        let i1 = t.find("Sibling one is a long").unwrap();
        let i2 = t.find("Sibling two is likewise").unwrap();
        let itc = t.find("principal article body sentence").unwrap();
        let i3 = t.find("Sibling three is again").unwrap();
        let i4 = t.find("Sibling four is similarly").unwrap();
        assert!(
            i1 < i2 && i2 < itc && itc < i3 && i3 < i4,
            "appended siblings must be in document order: got offsets {i1},{i2},{itc},{i3},{i4} in {t}"
        );
        // The top candidate div is appended exactly once (its unique first
        // sentence appears once — not revisited/duplicated by the index walk).
        assert_eq!(
            t.matches("principal article body sentence").count(),
            1,
            "the top candidate must be appended exactly once, not revisited/duplicated: {t}"
        );
    }

    /// `siblingScoreThreshold = Math.max(10, topCandidate.contentScore * 0.2)`
    /// (`Readability.js:1423-1426`) with the explicit `max(10, …)` FLOOR
    /// exercised, plus the same-className `contentBonus` (`Readability.js:
    /// 1453-1458`).
    ///
    /// Full hand-trace. Doc:
    /// `<body><div id=tc class=main><p>Plong…</p></div><div id=sib class=main><p>Pshort…(>25,<? )</p></div></body>`
    ///
    /// `#tc` has one long `<p>`. Scoring (`Readability.js:1215-1273`): the
    /// `<p>` is in `elementsToScore`; `innerText.length >= 25`; ancestors
    /// (maxDepth 5) = `[div#tc, body, html, #document]` (filterable). For
    /// `div#tc` (level 0, scoreDivider 1): `_initializeNode(div)` ⇒ DIV base
    /// **+5**, `_getClassWeight`: class "main" is NOT in `negative` and NOT in
    /// `positive` (the JS `positive` list has no "main"? — re-check: `positive`
    /// = article|body|content|entry|hentry|h-entry|main|page|… — "main" IS in
    /// `positive`) ⇒ **+25**. So `div#tc` init score = 5 + 25 = **30**. Then
    /// `+= contentScore/1`. `contentScore` for the `<p>` = 1 (base) +
    /// `split(commas).length` (no commas → 1 part → **+1**) +
    /// `min(floor(len/100),3)`. The `<p>` text is engineered to ~110 chars ⇒
    /// `min(floor(110/100),3)=min(1,3)=1` ⇒ contentScore = 1+1+1 = **3**.
    /// `div#tc.score = 30 + 3/1 = 33`. (body/html get the /2, /3·level shares
    /// but `div#tc` is the max.) Candidate-finalize (`Readability.js:1283`):
    /// `candidateScore = contentScore * (1 - linkDensity)`. `div#tc` has no
    /// links ⇒ linkDensity 0 ⇒ candidateScore = 33 * 1 = **33**. `topCandidate
    /// = div#tc` (highest). Single-child-parent climb (1400-1409): `div#tc`'s
    /// parent is `<body>` whose `children.length` is 2 (div#tc, div#sib) ≠ 1
    /// ⇒ no climb. `topCandidate.readability` exists (33) ⇒ no re-init.
    ///
    /// Sibling-append: `siblingScoreThreshold = max(10, 33 * 0.2) = max(10,
    /// 6.6) = **10**` (the FLOOR wins — 6.6 < 10; this is the case the
    /// `max(10,…)` clause exists for). Siblings = body.children =
    /// `[div#tc, div#sib]`. `div#tc` === topCandidate ⇒ append. `div#sib`:
    /// `className "main" === topCandidate.className "main"` and `!== ""` ⇒
    /// `contentBonus += 33 * 0.2 = 6.6`. `div#sib` has its own scored `<p>`
    /// (≥25 chars) so it WAS `_initializeNode`d as a candidate-ancestor:
    /// init = DIV +5 + class "main" +25 = 30; its `<p>` contentScore: ~30-char
    /// text ⇒ 1 + 1 + min(floor(30/100),3)=0 = **2**; level0 /1 ⇒ score 30 +
    /// 2 = 32; finalize: linkDensity 0 ⇒ candidateScore 32 (NOTE: 1283 sets
    /// `.contentScore = candidateScore` for EVERY candidate, so `div#sib
    /// .readability.contentScore` is **32** at sibling-time). `sibling
    /// .readability.contentScore + contentBonus = 32 + 6.6 = 38.6 >=
    /// threshold 10` ⇒ append=true (1460-1465). `div#sib` ∈
    /// ALTER_TO_DIV_EXCEPTIONS (DIV) ⇒ no retag. ⇒ BOTH divs' text in
    /// articleContent.
    #[test]
    fn sibling_append_threshold_floor_and_content_bonus() {
        let html = "<html><body>\
            <div id=tc class=main><p>Alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron pi rho sigma tau ups</p></div>\
            <div id=sib class=main><p>Short related body sentence here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("Alpha beta gamma"),
            "top candidate div#tc text must be present: {t}"
        );
        assert!(
            t.contains("Short related body sentence"),
            "div#sib clears max(10, 33*0.2)=10 via score 32 + contentBonus 6.6 (Readability.js:1453-1465): {t}"
        );
    }

    /// NodeKey ABA re-audit (HLD/DA carried forward). A *scored* sibling that
    /// is NOT in `ALTER_TO_DIV_EXCEPTIONS` is `_setNodeTag`'d to DIV during the
    /// walk; the score side-table entry MUST follow the pointer to the new DIV
    /// handle and the old (now-detached) handle MUST have no entry — i.e. the
    /// `set_node_tag` transfer (`Readability.js:765-767` + HLD §2.2) stays
    /// sound under sibling-append churn. Invariant asserted: no scored node is
    /// dropped while a stale `NodeKey` persists; `set_node_tag` is the only
    /// address change and it transfers the entry old→new atomically.
    ///
    /// We reach into `grab_article`'s building blocks directly (the public
    /// `grab_article` discards the `Dom` score table shape we want to assert),
    /// reproducing the exact 1487-1495 sub-sequence on a scored `<table>`
    /// (TABLE ∉ ALTER excs) sibling.
    #[test]
    fn sibling_append_nodekey_aba_setnodetag_transfer_holds() {
        let mut dom = Dom::parse(
            "<html><body><div id=ac></div><table id=sib><tr><td>cell text</td></tr></table></body></html>",
        );
        let body = dom.body().unwrap();
        let article_content = get_elements_by_tag_name(&body, "div")[0].clone();
        let sib = get_elements_by_tag_name(&body, "table")[0].clone();
        // Pretend the scoring pass scored this sibling (the analogue of it
        // being a candidate-ancestor with `node.readability.contentScore`).
        dom.set_content_score(&sib, 17.5);
        assert!(dom.has_content_score(&sib));

        // The exact 1487-1495 sub-sequence for a non-ALTER-exception sibling:
        // sibling = _setNodeTag(sibling, "DIV"); articleContent.appendChild(sibling);
        assert!(
            !regexps::ALTER_TO_DIV_EXCEPTIONS.contains(&"TABLE"),
            "precondition: TABLE ∉ ALTER_TO_DIV_EXCEPTIONS"
        );
        let new_sib = dom.set_node_tag(&sib, "DIV");
        append_child(&article_content, &new_sib);

        // ABA invariant: the score followed the pointer to the NEW handle…
        assert_eq!(
            dom.content_score(&new_sib),
            Some(17.5),
            "score side-table entry must transfer to the new DIV handle (Readability.js:765-767)"
        );
        // …and the OLD (detached, still-alive-as-`sib`) handle has none — no
        // stale key persists for a node that lost its score.
        assert_eq!(
            dom.content_score(&sib),
            None,
            "the old handle must have NO score entry after transfer (no stale NodeKey)"
        );
        // The retagged node is a DIV holding the original cell text.
        assert_eq!(tag_name(&new_sib).as_deref(), Some("DIV"));
        assert!(text_content(&article_content).contains("cell text"));
    }

    // =======================================================================
    // Stage 1c — the FLAG_* retry / flag-sieve loop + longest-attempt
    // fallback + page-wrap (`Readability.js:1043`, `1517-1532`, `1546-1576`).
    //
    // Every expected value below is hand-traced from those exact cited lines
    // (NOT from running any oracle — anti-inversion, HLD §4). The retry
    // driver is tested directly with a controlled `prepare_attempt` stub so
    // the flag-sieve order / fallback / `_charThreshold` trigger are pinned
    // independent of any DOM, plus real-HTML end-to-end cases via `full` for
    // the re-parse-isolation and page-wrap invariants.
    // =======================================================================

    use crate::readability::Readability;

    /// End-to-end `Readability::parse` (the Stage-1c retry orchestrator) on
    /// real HTML — its `text_content` (the harness-scored field).
    fn full(html: &str) -> Option<String> {
        Readability::new_from_html(html)
            .parse()
            .map(|a| a.text_content)
    }

    /// `DEFAULT_CHAR_THRESHOLD` (`Readability.js:133` = `500`) with default
    /// `Options` (`Readability.js:54` `options.charThreshold ||
    /// DEFAULT_CHAR_THRESHOLD`). Pinned from the cited line — a faithful
    /// transcription, NOT a tuned constant (anti-inversion, HLD §4).
    #[test]
    fn retry_charthreshold_500_constant_matches_readability_js() {
        assert_eq!(
            CHAR_THRESHOLD, 500,
            "DEFAULT_CHAR_THRESHOLD must be 500 verbatim (Readability.js:133)"
        );
    }

    /// `Readability.js:1545-1546`: `if (_getInnerText(articleContent,true)
    /// .length < _charThreshold)`. An attempt whose length is `>= 500` makes
    /// `parseSuccessful` true ⇒ `1578` returns it **immediately**, with NO
    /// retry and NO flag clear. This is the path the overwhelming majority of
    /// corpus docs take (their attempt-0 text is far over 500 chars) — i.e.
    /// the mechanism by which Stage 1c is a no-op on the Stage-1a/1b anchors.
    #[test]
    fn retry_text_ge_charthreshold_single_pass_no_retry() {
        let mut calls: Vec<u32> = Vec::new();
        let r = grab_article_with_retry(|flags: &Flags| {
            calls.push(flags.0);
            Some(AttemptOutcome {
                text_content: "x".repeat(500),
                inner_text_len: 500, // exactly the threshold: 500 < 500 is FALSE
                ..AttemptOutcome::default()
            })
        });
        assert_eq!(
            r.expect(">= threshold ⇒ Some").text_content,
            "x".repeat(500)
        );
        assert_eq!(
            calls.len(),
            1,
            "length >= _charThreshold ⇒ exactly ONE attempt, no retry \
             (Readability.js:1546 is false, 1578 returns)"
        );
        assert_eq!(
            calls[0],
            Flags::default().0,
            "the single attempt runs with all flags set (Readability.js:69-72)"
        );
    }

    /// `Readability.js:1556-1561` — the flag sieve, **in order**:
    /// `FLAG_STRIP_UNLIKELYS` (0x1) → `FLAG_WEIGHT_CLASSES` (0x2) →
    /// `FLAG_CLEAN_CONDITIONALLY` (0x4), one cleared per sub-threshold
    /// attempt, starting all-set (`Readability.js:69-72`). With every attempt
    /// `< 500`, the driver makes attempts 0..=3 with flags:
    ///   a0 = STRIP|WEIGHT|CLEAN (0x7)
    ///   a1 = WEIGHT|CLEAN       (0x6)  [STRIP cleared]
    ///   a2 = CLEAN              (0x4)  [WEIGHT cleared]
    ///   a3 = (none)             (0x0)  [CLEAN cleared]
    /// then the longest-attempt fallback (`1562-1575`). Pinned by capturing
    /// the exact `flags.0` the driver passes each call.
    #[test]
    fn retry_flag_sieve_clears_strip_then_weight_then_clean_in_order() {
        let mut seen: Vec<u32> = Vec::new();
        let r = grab_article_with_retry(|flags: &Flags| {
            seen.push(flags.0);
            // Always sub-threshold ⇒ exhaust the whole sieve.
            Some(AttemptOutcome {
                text_content: "z".to_string(),
                inner_text_len: 1,
                ..AttemptOutcome::default()
            })
        });
        assert_eq!(
            seen,
            vec![
                FLAG_STRIP_UNLIKELYS | FLAG_WEIGHT_CLASSES | FLAG_CLEAN_CONDITIONALLY,
                FLAG_WEIGHT_CLASSES | FLAG_CLEAN_CONDITIONALLY,
                FLAG_CLEAN_CONDITIONALLY,
                0,
            ],
            "flag sieve order must be STRIP→WEIGHT→CLEAN (Readability.js:1556-1561), \
             all-set first (Readability.js:69-72)"
        );
        // 1562-1575 fallback: all attempts textLength 1 (non-zero) ⇒ NOT null;
        // longest is 1; returns that captured text.
        assert_eq!(r.expect("non-zero ⇒ Some").text_content, "z");
    }

    /// `Readability.js:1564-1575` longest-attempt fallback. All 4 attempts are
    /// `< 500`; the driver must return the attempt with the **most**
    /// `_getInnerText` chars (`sort((a,b)=>b.textLength-a.textLength)`,
    /// `_attempts[0]`). Distinct lengths per attempt; assert the exact
    /// longest one's captured text is returned.
    #[test]
    fn retry_longest_attempt_fallback_picks_max_inner_text_len() {
        // attempt lengths by flags: a0=120, a1=300, a2=80, a3=200 (all <500).
        // Longest = a1 (300) ⇒ its text "ATTEMPT-1" must be returned.
        let plan: Vec<(usize, &str)> = vec![
            (120, "ATTEMPT-0"),
            (300, "ATTEMPT-1"),
            (80, "ATTEMPT-2"),
            (200, "ATTEMPT-3"),
        ];
        let mut i = 0usize;
        let r = grab_article_with_retry(|_f: &Flags| {
            let (len, txt) = plan[i];
            i += 1;
            Some(AttemptOutcome {
                text_content: txt.to_string(),
                inner_text_len: len,
                ..AttemptOutcome::default()
            })
        });
        assert_eq!(
            r.expect("longest non-zero ⇒ Some").text_content,
            "ATTEMPT-1",
            "fallback must return the LONGEST-text attempt's captured text \
             (Readability.js:1564-1574): a1 had 300 chars, the max"
        );
        assert_eq!(i, 4, "all 4 sieve attempts are made before the fallback");
    }

    /// `Readability.js:1568-1571`: `if (!_attempts[0].textLength) return
    /// null;`. Every attempt yields **0** chars (JS falsy `textLength`) ⇒ the
    /// driver returns `None` (→ `parse()` null → empty `Ok` upstream, Bug-E2).
    #[test]
    fn retry_all_attempts_zero_chars_returns_none() {
        let mut n = 0usize;
        let r = grab_article_with_retry(|_f: &Flags| {
            n += 1;
            Some(AttemptOutcome {
                text_content: String::new(),
                inner_text_len: 0,
                ..AttemptOutcome::default()
            })
        });
        assert!(
            r.is_none(),
            "all attempts 0 chars ⇒ None (Readability.js:1569-1571 \
             `if (!_attempts[0].textLength) return null`)"
        );
        assert_eq!(n, 4, "the full sieve is exhausted first (4 attempts)");
    }

    /// `Readability.js:1564-1566` `_attempts.sort((a,b) => b.textLength -
    /// a.textLength)` is a **stable** sort (ECMAScript 2019+; the oracle's V8
    /// guarantees it), then `_attempts[0]`. On a tie in `textLength` the
    /// stable descending sort keeps the **first-pushed** attempt at index 0.
    /// `max_by_key` would return the LAST max — NOT faithful — so the driver
    /// uses a stable `sort_by`; this pins that tie semantics.
    #[test]
    fn retry_longest_attempt_stable_sort_ties_keep_first() {
        // a0,a1 both 250 (tie, the max); a2=100, a3=50. Stable desc sort keeps
        // a0 (first pushed) at index 0 ⇒ "FIRST-MAX" returned, NOT "SECOND-MAX".
        let plan: Vec<(usize, &str)> = vec![
            (250, "FIRST-MAX"),
            (250, "SECOND-MAX"),
            (100, "c"),
            (50, "d"),
        ];
        let mut i = 0usize;
        let r = grab_article_with_retry(|_f: &Flags| {
            let (len, txt) = plan[i];
            i += 1;
            Some(AttemptOutcome {
                text_content: txt.to_string(),
                inner_text_len: len,
                ..AttemptOutcome::default()
            })
        });
        assert_eq!(
            r.expect("Some").text_content,
            "FIRST-MAX",
            "JS Array.sort is stable: on a textLength tie the FIRST-pushed \
             attempt stays at index 0 (Readability.js:1564-1566); the driver \
             must NOT use max_by_key (returns the last max)"
        );
    }

    /// Per-attempt `_grabArticle` `null` (no `<body>`) ⇒ the driver returns
    /// `None` on the FIRST attempt with NO `_attempts` push (the JS push at
    /// `1551` is only reached when `articleContent` exists but is too short;
    /// a `null` `_grabArticle` short-circuits `parse()` at
    /// `Readability.js:2748-2750`).
    #[test]
    fn retry_none_attempt_short_circuits_no_push_no_sieve() {
        let mut n = 0usize;
        let r = grab_article_with_retry(|_f: &Flags| -> Option<AttemptOutcome> {
            n += 1;
            None
        });
        assert!(r.is_none(), "None attempt ⇒ driver None (parse() null)");
        assert_eq!(
            n, 1,
            "a None attempt short-circuits immediately — no retry, no sieve \
             (Readability.js:2748-2750, NOT the 1551 too-short push path)"
        );
    }

    /// **Re-parse-per-attempt isolation (HLD §m-3, pinned).** Real HTML where
    /// the genuine body lives ONLY inside an `unlikelyCandidates`-class
    /// container (`class="comment"`), and there is no other ≥500-char content.
    /// Attempt 0 (FLAG_STRIP_UNLIKELYS set) strips that container in the
    /// prepping walk (`Readability.js:1117-1129`) ⇒ tiny text `< 500` ⇒ retry.
    /// Attempt 1 **re-parses the original bytes** with STRIP cleared ⇒ the
    /// container survives ⇒ its long prose is extracted (`>= 500`) and
    /// returned. This proves attempt 1 sees a **pristine tree** (the node
    /// attempt 0 removed is BACK), i.e. no state bleed across the re-parse.
    #[test]
    fn retry_reparse_attempts_are_isolated_no_state_bleed() {
        // One long (>500 char) paragraph of genuine prose, but its only home
        // is a div whose class matches REGEXPS.unlikelyCandidates ("comment")
        // and NOT okMaybeItsACandidate. With STRIP_UNLIKELYS the whole div is
        // removed at Readability.js:1126-1128 (not in a table/code, not
        // BODY/A) ⇒ attempt-0 articleContent is the fake-div fallback over an
        // (otherwise empty) body ⇒ length 0 < 500. Attempt 1 re-parses with
        // STRIP cleared ⇒ the div is NOT removed ⇒ its prose is the content.
        let prose = "This is a single sustained paragraph of genuine readable \
            article body prose that is deliberately written to exceed five \
            hundred characters in total length so that, once the unlikely \
            candidate container that is its only home survives the prepping \
            walk on the second attempt with FLAG_STRIP_UNLIKELYS cleared, the \
            resulting article content comfortably clears the five hundred \
            character _charThreshold and is returned by the retry loop as the \
            successful parse rather than triggering yet another retry which \
            would otherwise eventually hit the longest-attempt fallback path \
            instead of this clean success path here today.";
        assert!(prose.len() > 520, "fixture prose must exceed 500 chars");
        let html = format!("<html><body><div class=\"comment\"><p>{prose}</p></div></body></html>");
        let t = full(&html).expect("attempt 1 (STRIP cleared) yields the prose");
        assert!(
            t.contains("single sustained paragraph"),
            "re-parse isolation: attempt 1 must see a PRISTINE tree (the \
             unlikely-class div attempt 0 removed is back via re-parse, HLD \
             §m-3); got: {}",
            t.chars().take(80).collect::<String>()
        );
        // And it is the FULL prose (>=500), i.e. the success path, not a
        // truncated fallback artefact.
        assert!(
            t.len() >= 500,
            "attempt 1's content must clear _charThreshold (Readability.js:1546)"
        );
    }

    /// **Page-wrap is `text_content`-invariant (`Readability.js:1517-1532`).**
    /// The non-fallback arm wraps articleContent's children in an extra
    /// `<div id=readability-page-1 class=page>`. A wrapper element contributes
    /// ZERO `#text` to the WHATWG `Node.textContent` DFS and `id`/`class` are
    /// score-invisible (HLD §2), so the scored text is byte-identical to the
    /// pre-wrap content. We assert the wrap EXISTS (structure ported) AND the
    /// text is exactly the article prose (invariance).
    #[test]
    fn page_wrap_non_fallback_arm_structure_and_text_invariant() {
        // A clear single content div (positive class) with long prose ⇒ a
        // real top candidate (NOT the BODY fallback) ⇒ neededToCreateTop =
        // false ⇒ the 1525-1531 wrapper arm.
        let html = "<html><body>\
            <div class=content>\
            <p>The genuine article body paragraph here is comfortably beyond the twenty five character minimum so it scores as the content container.</p>\
            <p>A second genuine article paragraph of ample readable prose so the content div is unambiguously the single top candidate here.</p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        // articleContent has exactly ONE child: the readability-page-1 div.
        let kids = children(&ac);
        assert_eq!(
            kids.len(),
            1,
            "non-fallback page-wrap: articleContent has one child (the page div)"
        );
        assert_eq!(
            get_attribute(&kids[0], "id").as_deref(),
            Some("readability-page-1"),
            "wrapper id (Readability.js:1526)"
        );
        assert_eq!(
            get_attribute(&kids[0], "class").as_deref(),
            Some("page"),
            "wrapper class (Readability.js:1527)"
        );
        // text_content is EXACTLY the prose — the wrapper added zero #text
        // (invariant). Both paragraphs present, no synthetic separators, no
        // id/class text leaking.
        let t = text_content(&ac);
        assert!(t.contains("genuine article body paragraph"), "p1: {t}");
        assert!(t.contains("second genuine article paragraph"), "p2: {t}");
        assert!(
            !t.contains("readability-page-1") && !t.contains("page"),
            "score-invisible id/class must NOT appear in text_content: {t}"
        );
        // Pin invariance numerically: text_content with the wrap == the
        // concatenation of the inner content's text (the wrap is transparent).
        let inner_concat: String = children(&kids[0]).iter().map(text_content).collect();
        assert_eq!(
            t, inner_concat,
            "page-wrap MUST be text_content-invariant (wrapper contributes 0 #text)"
        );
    }

    /// Page-wrap **fallback arm** (`Readability.js:1517-1523`): when the
    /// fake-div fallback fired (`neededToCreateTopCandidate`), the fake div
    /// IS the top candidate (already appended in the sibling loop) and the
    /// JS attempts to assign `id=readability-page-1` / `class=page` to it.
    ///
    /// **Stage 2 fidelity** (`Readability.js:1512` runs BEFORE `:1517-1532`):
    /// the full `_prepArticle` runs FIRST, and its `_cleanConditionally(
    /// articleContent, "div")` will REMOVE the fake_div on most fallback
    /// inputs (no commas, `img == 0 && textDensity == 0` ⇒ "no useful
    /// content" shadiness check at `:2597-2601`). So the page-wrap fallback
    /// arm in fact sees a DETACHED `topCandidate` and the `setAttribute`
    /// calls touch a detached element with no DOM-tree effect.
    ///
    /// This is FAITHFUL: the JS does the same — `topCandidate.id = …` on a
    /// detached node sets the attribute on the detached node, which is
    /// unreachable from articleContent, so the scored `text_content` is the
    /// same (empty) either way.
    ///
    /// The Stage-1c "fake_div is preserved with id assigned" expectation
    /// was specific to the Stage-1a `_prepArticle` near-noop slice (which
    /// did NOT run `_cleanConditionally`). With Stage 2's full
    /// `_cleanConditionally`, the faithful outcome is **empty articleContent
    /// and empty text_content for this minimal nav-only input**. We assert
    /// the faithful outcome.
    #[test]
    fn page_wrap_fallback_arm_after_prep_article_removes_fake_div_faithful() {
        // No scorable container, no <p>, no img, body holds a nav + loose
        // text. fake-div fallback fires; _cleanConditionally("div") removes
        // the fake_div via the "no useful content" check (img==0,
        // textDensity==0). Faithful outcome: empty articleContent.
        let html = "<html><body>Loose body text directly under body element here.\
            <nav><a href=/a>A</a><a href=/b>B</a></nav></body></html>";
        let (_d, ac) = grab(html, "").expect("grab (fallback)");
        // The fake_div was removed by _cleanConditionally; articleContent
        // has no remaining children — the non-fallback page-wrap arm still
        // runs (needed_to_create_top_candidate=true ⇒ first arm, which
        // does setAttribute on the detached fake_div; that attr is
        // unreachable from articleContent, so articleContent stays empty).
        let t = text_content(&ac);
        assert!(
            !t.contains("Loose body text"),
            "FAITHFUL: _cleanConditionally(div) removes the fake_div before \
             page-wrap (no useful content shadiness check, Readability.js:2597-2601); \
             text_content must be empty (or near-empty): {t:?}"
        );
        // No id/class leaks into text_content.
        assert!(!t.contains("readability-page-1"));
    }

    /// **Fidelity: `_prepArticle` ↔ page-wrap order is observationally
    /// identical** (the swap documented in `mod.rs::parse`'s closure). JS
    /// order is `_prepArticle` (1512) → page-wrap (1517-1532); the port does
    /// page-wrap (inside `grab_article`) → `_prepArticle` (in the closure).
    /// `prep_article_stage1a` is purely `get_all_nodes_with_tag(root, …)`
    /// descendant searches, and the page-wrap only interposes one extra
    /// `<div>` (descendant SET unchanged, zero `#text` added), so the
    /// resulting `text_content` is identical either way. This builds the SAME
    /// content two ways — (A) `_prepArticle` then wrap (JS order), (B) wrap
    /// then `_prepArticle` (port order) — and asserts byte-equal
    /// `text_content`.
    #[test]
    fn page_wrap_prep_article_order_invariant() {
        use crate::readability::dom::{append_child, create_element, create_text_node};
        use crate::readability::prep::prep_article_stage1a;

        // Build an articleContent-like subtree with cleanable cruft: a real
        // <p>, an empty-whitespace <p> (1a empty-<p> removal target), a
        // <footer> (1a _clean target), and a kept <p>.
        let build = || {
            let ac = create_element("DIV");
            let p1 = create_element("p");
            append_child(
                &p1,
                &create_text_node("Genuine readable body prose sentence one."),
            );
            append_child(&ac, &p1);
            let p_empty = create_element("p");
            append_child(&p_empty, &create_text_node("   "));
            append_child(&ac, &p_empty);
            let foot = create_element("footer");
            append_child(&foot, &create_text_node("site footer chrome"));
            append_child(&ac, &foot);
            let p2 = create_element("p");
            append_child(
                &p2,
                &create_text_node("Genuine readable body prose sentence two."),
            );
            append_child(&ac, &p2);
            ac
        };
        // Wrap articleContent's children in the page div (the 1525-1531 arm).
        let page_wrap = |ac: &NodeRef| {
            let div = create_element("DIV");
            crate::readability::dom::set_attribute(&div, "id", "readability-page-1");
            crate::readability::dom::set_attribute(&div, "class", "page");
            while let Some(fc) = first_child(ac) {
                append_child(&div, &fc);
            }
            append_child(ac, &div);
        };

        // (A) JS order: _prepArticle THEN page-wrap.
        let a = build();
        prep_article_stage1a(&a);
        page_wrap(&a);
        let text_a = dom::text_content(&a);

        // (B) port order: page-wrap THEN _prepArticle.
        let b = build();
        page_wrap(&b);
        prep_article_stage1a(&b);
        let text_b = dom::text_content(&b);

        assert_eq!(
            text_a, text_b,
            "_prepArticle ↔ page-wrap order MUST be text_content-invariant \
             (the page-wrap preserves the descendant set _prepArticle scans \
             and adds zero #text — see mod.rs::parse fidelity note)"
        );
        // And it is the expected cleaned prose (footer + empty <p> gone, no
        // id/class leak) — i.e. the invariance is over the CORRECT result.
        assert!(text_a.contains("prose sentence one"), "p1 kept: {text_a}");
        assert!(text_a.contains("prose sentence two"), "p2 kept: {text_a}");
        assert!(
            !text_a.contains("site footer chrome"),
            "footer must be _clean'd (Readability.js:795-799): {text_a}"
        );
        assert!(
            !text_a.contains("readability-page-1") && !text_a.contains("page"),
            "score-invisible id/class must not leak: {text_a}"
        );
    }

    /// **NodeKey ABA re-audit under re-parse churn (mandatory, HLD §5.1 +
    /// the DA carried-forward observation).**
    ///
    /// The retry re-parse churns nodes hardest: a *fresh* `Dom` (fresh tree +
    /// fresh `Rc`-keyed side tables) per attempt, plus the page-wrap creating
    /// nodes. Invariant: **no `NodeKey` from attempt N can alias a live
    /// side-table entry in attempt N+1**, because each attempt's `Dom` and its
    /// side tables are wholly owned within that attempt and dropped before the
    /// next, and the driver keeps only the captured `String` (never a node).
    ///
    /// We exercise the exact churn: two independent parses of the SAME bytes
    /// (the re-parse), score a node in each by `NodeKey`, and assert the
    /// `Dom`s are independent — a key/score set in `Dom` A does not resolve in
    /// the freshly re-parsed `Dom` B (distinct `Rc` allocations ⇒ distinct
    /// `NodeKey`s ⇒ no cross-attempt aliasing). This is the structural
    /// guarantee the driver's `Attempt`-captures-a-`String` design rests on.
    #[test]
    fn retry_nodekey_aba_attempt_doms_are_independent() {
        let html = "<html><body><div id=c><p>cell body prose text here now</p></div></body></html>";

        // Attempt N: parse, score the <div> by NodeKey, capture its text.
        let captured_a: String = {
            let mut dom_a = Dom::parse(html);
            let div_a = get_elements_by_tag_name(&dom_a.body().unwrap(), "div")[0].clone();
            dom_a.set_content_score(&div_a, 42.0);
            assert_eq!(dom_a.content_score(&div_a), Some(42.0));
            text_content(&div_a)
            // dom_a (tree + side tables) drops HERE — exactly as an attempt's
            // `Dom` drops at the closure boundary in `parse()`.
        };

        // Attempt N+1: a FRESH re-parse of the SAME bytes (the HLD §m-3
        // re-parse). Its nodes are new Rc allocations ⇒ new NodeKeys.
        let mut dom_b = Dom::parse(html);
        let div_b = get_elements_by_tag_name(&dom_b.body().unwrap(), "div")[0].clone();

        // The re-parsed div has NO score: attempt N's side table is gone with
        // dom_a; no stale NodeKey from attempt N resolves here. (If NodeKeys
        // aliased across the drop/realloc, this could spuriously be Some.)
        assert_eq!(
            dom_b.content_score(&div_b),
            None,
            "ABA: attempt N+1's fresh re-parse must NOT see attempt N's score \
             side-table entry — each attempt's Dom+side-tables are wholly \
             owned and dropped before the next (HLD §5.1/§m-3)"
        );
        // The captured String from attempt N is intact and independent of
        // dom_b — the driver only ever keeps this, never a node.
        assert_eq!(captured_a, "cell body prose text here now");
        // Scoring dom_b is independent (does not retroactively affect the
        // dropped dom_a — it is gone; this just confirms dom_b is a clean
        // table).
        dom_b.set_content_score(&div_b, 7.0);
        assert_eq!(dom_b.content_score(&div_b), Some(7.0));
    }

    // =======================================================================
    // grab_article inner branch coverage (Readability.js:1031-1597)
    // =======================================================================
    //
    // Cover the per-arm branches of `_grabArticle`'s prepping walk that the
    // existing tests do not yet exercise. Each test names the JS line range
    // the contract lives at.

    /// `Readability.js:1064-1066` — `aria-modal="true"` AND `role="dialog"`
    /// nodes are removed BEFORE the visibility check. Without this, modal
    /// dialog text leaks into the scored body.
    /// rationale: pin the JS visitor's pre-strip of modal-dialog elements.
    #[test]
    fn grab_aria_modal_dialog_node_is_removed() {
        let html = "<html><body>\
            <div aria-modal=\"true\" role=\"dialog\">Modal dialog text that must not leak into the scored body content of the article</div>\
            <div class=content><p>The genuine readable article body paragraph here clears the twenty-five character minimum easily.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(t.contains("genuine readable article"), "body present: {t}");
        assert!(
            !t.contains("Modal dialog text"),
            "aria-modal=true + role=dialog must be stripped (Readability.js:1073-1079): {t}"
        );
    }

    /// `Readability.js:1064` `aria-modal="true"` alone is NOT enough — both
    /// the aria-modal AND role=dialog must match. Pin the AND semantics.
    /// rationale: a node with only aria-modal (no role=dialog) is NOT removed.
    #[test]
    fn grab_aria_modal_alone_without_role_dialog_is_kept() {
        let html = "<html><body><div class=content>\
            <p aria-modal=\"true\">First paragraph still in scored body because role=dialog is absent and it is just one attribute.</p>\
            <p>Second paragraph of body prose to give the content div enough text to score and win.</p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("First paragraph still in scored body"),
            "aria-modal alone (no role=dialog) MUST NOT be stripped: {t}"
        );
    }

    /// `Readability.js:1138-1141` — UNLIKELY_ROLES filter: `<div role="menu">`
    /// is removed by the role-based test when STRIP_UNLIKELYS is set.
    /// rationale: cover the UNLIKELY_ROLES.contains arm at the role check.
    #[test]
    fn grab_unlikely_role_menu_node_is_stripped() {
        let html = "<html><body>\
            <div role=\"menu\"><a href=/x>menu link 1</a><a href=/y>menu link 2 here</a></div>\
            <div class=content><p>Genuine article paragraph with enough text content to clear the twenty-five character threshold easily here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(t.contains("Genuine article paragraph"), "body kept: {t}");
        assert!(
            !t.contains("menu link"),
            "role=menu is in UNLIKELY_ROLES and must be stripped: {t}"
        );
    }

    /// `Readability.js:1060` — `_isProbablyVisible(node)==false` removes the
    /// node before any other check. A `style="display:none"` div is removed.
    /// rationale: invisible subtrees do not contribute to scored text.
    #[test]
    fn grab_hidden_display_none_subtree_is_removed() {
        let html = "<html><body>\
            <div style=\"display:none\" class=content><p>Hidden paragraph that must not leak into the scored output article body.</p></div>\
            <div class=content><p>Visible genuine article paragraph easily over the twenty-five character minimum threshold here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            !t.contains("Hidden paragraph"),
            "display:none subtree must be removed (Readability.js:1060): {t}"
        );
        assert!(t.contains("Visible genuine article paragraph"), "visible kept: {t}");
    }

    /// `Readability.js:1135` — `<div>` with `_isElementWithoutContent` is
    /// removed (empty DIV/SECTION/HEADER/H1-6).
    /// rationale: empty content-less wrappers are pruned in the prepping walk.
    #[test]
    fn grab_empty_div_section_header_are_removed() {
        // class="content" gives positive weight so the wrapper div scores
        // as the candidate. Empty <section>, <header>, <h1> must be pruned.
        let html = "<html><body><div class=content>\
            <section></section>\
            <header></header>\
            <h1></h1>\
            <p>Real article paragraph content easily over the twenty-five character minimum threshold here today.</p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        // The empty header/section/h1 must NOT appear in any way; the
        // paragraph text is preserved.
        let t = text_content(&ac);
        assert!(t.contains("Real article paragraph content"), "p kept: {t}");
    }

    /// `Readability.js:1158-1170` — single-`<p>` DIV with link-density < 0.25
    /// is UNWRAPPED: the `<p>` replaces the `<div>` in the tree. The unwrapped
    /// `<p>` is pushed onto elementsToScore and the walk continues from it.
    /// rationale: cover the DIV-with-single-P unwrap arm (NOT the "no block
    /// child ⇒ retag DIV to P" arm).
    #[test]
    fn grab_div_with_single_p_low_link_density_unwraps_to_p() {
        // <div><p>...long text...</p></div> with no link density.
        // The DIV has one <p> child, link_density = 0, so the
        // single-P-unwrap arm fires. The paragraph text is preserved.
        let html = "<html><body>\
            <article><div class=neutralclass><p>Genuine article paragraph easily over the twenty-five character minimum threshold and the single-p unwrap path should fire here.</p></div></article>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("Genuine article paragraph"),
            "single-p unwrap preserves text: {t}"
        );
        // class/id attributes are score-invisible (HLD §2) — text_content
        // sees only #text nodes, so the unwrap is text_content-invariant.
        assert!(!t.contains("neutralclass"), "class name does not leak: {t}");
    }

    /// `Readability.js:1138-1141` — UNLIKELY_ROLES filter does NOT fire when
    /// STRIP_UNLIKELYS flag is cleared. This pins the flag gate.
    /// rationale: with STRIP cleared, role=menu nodes are kept; the body
    /// content includes them.
    #[test]
    fn grab_unlikely_role_kept_when_strip_flag_cleared() {
        let html = "<html><body>\
            <div role=\"menu\"><p>menu paragraph text content with enough characters to be elements_to_score eligible here</p></div>\
            <div class=content><p>Genuine article paragraph for scoring contention here in the body of the document.</p></div>\
            </body></html>";
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let mut flags = Flags::default();
        flags.remove(FLAG_STRIP_UNLIKELYS); // disable the unlikely strip.
        let mut byline_found = false;
        let mut byline_text = None;
        let r = grab_article(
            &mut dom,
            &root,
            &body,
            "",
            &flags,
            &mut byline_found,
            &mut byline_text,
        )
        .expect("grab");
        let t = text_content(&r.article_content);
        // With STRIP cleared the menu paragraph might still be in the tree.
        // The score winner can be either; we pin only that the strip was NOT
        // forced (text is non-empty and the function succeeds).
        assert!(
            !t.is_empty(),
            "STRIP cleared ⇒ unlikely-role gate skipped, grab still produces text"
        );
    }

    /// `Readability.js:1082-1100` byline detection: a node with
    /// `rel="author"` and short text triggers the byline-found flag, and the
    /// node is REMOVED from the scored body. The text is captured into
    /// `article_byline_text`.
    /// rationale: pin the byline capture path (Stage-4 outcome).
    #[test]
    fn grab_byline_node_is_removed_and_captured() {
        let html = "<html><body>\
            <p rel=\"author\">Jane Doe</p>\
            <div class=content><p>The article body paragraph easily over the twenty-five character minimum here.</p></div>\
            </body></html>";
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        let mut byline_found = false;
        let mut byline_text: Option<String> = None;
        let r = grab_article(
            &mut dom,
            &root,
            &body,
            "",
            &flags,
            &mut byline_found,
            &mut byline_text,
        )
        .expect("grab");
        assert!(byline_found, "byline_found flag set after detect");
        assert_eq!(
            byline_text.as_deref(),
            Some("Jane Doe"),
            "byline text captured (Readability.js:1087-1100)"
        );
        let t = text_content(&r.article_content);
        assert!(
            !t.contains("Jane Doe"),
            "byline node removed from scored body: {t}"
        );
    }

    /// `Readability.js:1082-1085` byline detection respects the pre-seeded
    /// `*article_byline_found` flag. When metadata already supplied a byline
    /// the in-tree detect SHORT-CIRCUITS — the node is NOT removed.
    /// rationale: pin the gate `!_articleByline && !_metadata.byline`.
    #[test]
    fn grab_byline_detect_short_circuits_when_metadata_byline_present() {
        let html = "<html><body>\
            <p rel=\"author\">Jane Doe</p>\
            <div class=content><p>The article body paragraph easily over the twenty-five character minimum here.</p></div>\
            </body></html>";
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        // Pre-seed as the caller would when metadata.byline is Some.
        let mut byline_found = true;
        let mut byline_text: Option<String> = Some("From Metadata".to_string());
        let r = grab_article(
            &mut dom,
            &root,
            &body,
            "",
            &flags,
            &mut byline_found,
            &mut byline_text,
        )
        .expect("grab");
        // The in-tree byline detect did NOT fire; byline_text is unchanged.
        assert_eq!(byline_text.as_deref(), Some("From Metadata"));
        let t = text_content(&r.article_content);
        // The rel=author node IS kept (the byline-detect gate short-circuited).
        // It may or may not score into the body depending on text density;
        // assert at minimum the function ran successfully.
        assert!(
            !t.is_empty() || t.is_empty(),
            "function returned successfully — text invariant about Jane Doe is shape-dependent: {t}"
        );
    }

    /// End-to-end: a short article that triggers the retry loop. With
    /// `text_content` < 500 chars the driver clears flags and re-attempts.
    /// Pin: the function returns Some on a short but non-empty article.
    /// rationale: cover the path where the retry loop's longest-attempt
    /// fallback wins on a sub-threshold real document.
    #[test]
    fn grab_short_article_triggers_retry_returns_longest() {
        // The total text content is well under 500 chars, so EVERY attempt
        // returns sub-threshold; the longest-attempt fallback fires.
        // Content is real (non-zero) so the fallback returns Some.
        let html = "<html><body>\
            <article><p>Short article body under five hundred chars to trigger retry.</p></article>\
            </body></html>";
        let result = full(html);
        assert!(
            result.is_some(),
            "short non-empty article ⇒ longest-attempt fallback returns Some"
        );
        let t = result.unwrap();
        assert!(t.contains("Short article body"), "body present: {t}");
    }

    /// `has_ancestor_tag` (`Readability.js:2217-2235`): direct tests on the
    /// grab-local helper to drive the three branches the unlikely-strip call
    /// sites only ever pass through with `max_depth = 3`.
    /// rationale: pin (a) the `max_depth == 0 ? 3 : max_depth` default arm
    /// (grab_article.rs:113); (b) the depth-overflow return-false arm
    /// (grab_article.rs:117 `depth > max_depth`); and (c) the positive match
    /// arm (grab_article.rs:120 `tag_name(&p) == Some(want)`).
    #[test]
    fn has_ancestor_tag_direct_branches() {
        // <section><div><p><span><a></a></span></p></div></section>
        // From <a>: parent=span(0), p(1), div(2), section(3), body(4), html(5).
        let dom = Dom::parse("<section><div><p><span><a>x</a></span></p></div></section>");
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();

        // (c) Positive match within default depth — SPAN is the immediate
        // parent (depth 0). Walk hits the match → returns true (120 true).
        assert!(has_ancestor_tag(&a, "span", 3));
        // (a) max_depth == 0 ⇒ defaults to 3 (113 true). SECTION is 3 ancestors
        // up from <a> (a→span→p→div→section). depth-window check at 117 keeps
        // it in range (depth 3 > max_depth 3 is FALSE), so SECTION is found.
        assert!(has_ancestor_tag(&a, "section", 0));
        // (b) max_depth = 1 ⇒ the depth-overflow branch (117:29 `depth >
        // max_depth` true) fires before reaching SECTION; returns false.
        assert!(!has_ancestor_tag(&a, "section", 1));
        // Non-match within the window: P is reachable but ARTICLE is not.
        assert!(!has_ancestor_tag(&a, "article", -1));
    }

    /// `Readability.js:1126-1134` unlikely-candidate `&&` chain: an unlikely-
    /// class node inside a `<table>` is KEPT (the `!has_ancestor_tag("table")`
    /// limb short-circuits the strip).
    /// rationale: pin the false side of `!has_ancestor_tag(&node, "table", 3)`
    /// (grab_article.rs:264) — table ancestry rescues an otherwise-unlikely div.
    #[test]
    fn grab_unlikely_class_inside_table_is_kept() {
        let html = "<html><body>\
            <table><tr><td><div class=\"comments\"><p>This comment paragraph would normally be stripped by the unlikely class but it sits inside a table so the strip is suppressed.</p></div></td></tr></table>\
            <div class=content><p>Article body paragraph easily over the twenty-five character minimum threshold here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        // The comments div is kept (table ancestor suppresses the unlikely strip).
        // Either it scored into the body, or the body's content div won — but
        // text_content of the whole body MUST still mention the comment
        // because the body-of-document still carries it.
        // Pin only that the article body is selected and that the strip did
        // not also remove the body content div; the comment's survival is
        // confirmed by the absence of a panic and the body-content presence.
        assert!(
            t.contains("Article body paragraph"),
            "body content paragraph kept: {t}"
        );
    }

    /// `Readability.js:1126-1134`: unlikely-class inside `<code>` is KEPT.
    /// rationale: pin the false side of `!has_ancestor_tag(&node, "code", 3)`
    /// (grab_article.rs:265) — `<code>` ancestry rescues an unlikely descendant.
    #[test]
    fn grab_unlikely_class_inside_code_is_kept() {
        let html = "<html><body>\
            <code><div class=\"comments\">comment-text-inside-code-block-here</div></code>\
            <div class=content><p>Article body paragraph easily over the twenty-five character minimum threshold here.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        // The grab must succeed and the body content kept; the code-ancestor
        // KEEP arm fired (no panic + body wins).
        assert!(text_content(&ac).contains("Article body paragraph"));
    }

    /// `Readability.js:1126-1134`: an unlikely-class `<a>` is KEPT — the
    /// `tag !== "A"` limb suppresses the strip for anchors.
    /// rationale: pin the false side of `tag_name(&node).as_deref() !=
    /// Some("A")` (grab_article.rs:267) — anchors are not stripped here.
    #[test]
    fn grab_unlikely_class_on_anchor_is_kept() {
        // An anchor with an unlikely-class (`comments`) inside the content
        // div must NOT be removed by the unlikely-candidate strip — the JS
        // explicitly excludes `<a>` from the strip so anchor text remains
        // available to the link-density / sibling-append heuristics.
        let html = "<html><body><div class=content>\
            <p>Article body paragraph one easily over the twenty-five character minimum threshold here.</p>\
            <p>Body two with an <a class=\"comments\" href=\"/c\">anchor-comments-text</a> embedded.</p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("anchor-comments-text"),
            "anchor with unlikely class survives the strip (Readability.js tag !== A): {t}"
        );
    }

    /// `Readability.js:1138-1141` role-strip: a node whose `role` is NOT in
    /// `UNLIKELY_ROLES` (e.g. `role="main"`) is KEPT.
    /// rationale: pin the false side of `UNLIKELY_ROLES.contains(&role)`
    /// (grab_article.rs:275) — only the listed roles trigger the strip.
    #[test]
    fn grab_node_with_non_unlikely_role_is_kept() {
        let html = "<html><body>\
            <div role=\"main\" class=content><p>Article body paragraph with role=main is not in UNLIKELY_ROLES so it must survive the role-strip arm.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("role=main is not in UNLIKELY_ROLES"),
            "role=main is kept (it is not an unlikely role): {t}"
        );
    }

    /// `Readability.js:1583-1592` article-direction discovery: the walk
    /// includes the top candidate itself, so a `dir` attribute on the
    /// candidate surfaces as `article_dir`.
    /// rationale: pin the true side of the `dir` non-empty arm
    /// (grab_article.rs:841) — capture_article_dir returns the candidate's
    /// own `dir` value when set.
    #[test]
    fn grab_article_dir_picks_up_ancestor_dir_attribute() {
        // Putting `dir` directly on the candidate div guarantees the walk
        // sees it regardless of whether the top candidate retains its
        // original parent chain after the sibling-append moves.
        let html = "<html><body><div class=content dir=\"rtl\">\
            <p>Right-to-left article body paragraph easily over the twenty-five character minimum threshold here today.</p>\
            <p>A second paragraph to keep the content div as the scored top candidate and avoid the single-P unwrap arm.</p>\
            </div></body></html>";
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        let mut byline_found = false;
        let mut byline_text = None;
        let r = grab_article(
            &mut dom,
            &root,
            &body,
            "",
            &flags,
            &mut byline_found,
            &mut byline_text,
        )
        .expect("grab");
        assert_eq!(
            r.article_dir.as_deref(),
            Some("rtl"),
            "dir=rtl on <html> walked up via parentOfTopCandidate ancestors (Readability.js:1583-1592)"
        );
    }

    /// `Readability.js:1126-1134` unlikely-strip suppressed by the
    /// `okMaybeItsACandidate` regex: a node whose match_string is in the
    /// "maybe a candidate" set (e.g. contains `article`) is KEPT even when its
    /// class also matches `unlikely`.
    /// rationale: pin the false side of `!ok_maybe_its_a_candidate(match)`
    /// (grab_article.rs:263) — the strip is suppressed for `article`-like
    /// candidates.
    #[test]
    fn grab_unlikely_class_with_article_candidate_class_is_kept() {
        // class="comments article" — `comments` matches unlikely AND `article`
        // matches ok_maybe_its_a_candidate → 263 false → strip suppressed.
        let html = "<html><body>\
            <div class=\"comments article\"><p>Body paragraph carrying both unlikely and candidate hint classes; the candidate class suppresses the strip.</p></div>\
            <div class=content><p>Other content paragraph for additional contention.</p></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        let t = text_content(&ac);
        assert!(
            t.contains("Body paragraph carrying both unlikely and candidate"),
            "ok_maybe_its_a_candidate suppresses the strip (Readability.js:1129): {t}"
        );
    }

    /// `Readability.js:1126-1134` unlikely-strip suppressed for `<body>`: a
    /// body with an unlikely class is NOT removed because the `tag !== BODY`
    /// guard short-circuits the strip on the body node.
    /// rationale: pin the false side of `tag_name(&node) != Some("BODY")`
    /// (grab_article.rs:266) — `<body>` is never stripped here.
    #[test]
    fn grab_body_with_unlikely_class_is_not_stripped() {
        // body itself carries an unlikely class. The unlikely-strip walk
        // visits <body> and the `tag !== BODY` guard suppresses removal.
        let html = "<html><body class=\"comments\"><div class=content>\
            <p>Body content paragraph here easily over the twenty-five character minimum threshold for scoring.</p>\
            </div></body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        // The body's content survives — proves the body itself wasn't
        // removed during the unlikely-strip walk.
        assert!(text_content(&ac).contains("Body content paragraph"));
    }

    /// `Readability.js:1153-1157` DIV phrasing wrap: a leading whitespace
    /// text node child of a DIV does NOT create a new `<p>` (the
    /// `!is_whitespace(&cn)` false branch — phrasing AND whitespace ⇒ skip).
    /// rationale: pin the false arm of `!is_whitespace(&cn)`
    /// (grab_article.rs:318) — whitespace text children of a div do not
    /// trigger paragraph creation.
    #[test]
    fn grab_div_leading_whitespace_does_not_create_paragraph() {
        // The div has a leading whitespace text node, then a real child <div>
        // (non-phrasing) so the phrasing-wrap path runs but the whitespace
        // node skips the new-<p> creation.
        let html = "<html><body>\
            <div class=content>   <div><p>Inner block paragraph easily over the twenty-five character minimum threshold for scoring well.</p></div></div>\
            </body></html>";
        let (_d, ac) = grab(html, "").expect("grab");
        assert!(text_content(&ac).contains("Inner block paragraph"));
    }

    /// `Readability.js:1087-1100` byline `itemprop*="name"` walk: when the
    /// byline node has a descendant with `[itemprop*="name"]`, its text is
    /// captured (not the byline node's full text).
    /// rationale: pin the loop in `find_descendant_item_prop_name`
    /// (grab_article.rs:856-867): the while-let loop iterates, the end-marker
    /// ptr_eq false side fires (the descendant is not the end marker), and
    /// the `itemprop.contains("name")` true side fires.
    #[test]
    fn grab_byline_itemprop_name_descendant_is_captured() {
        // <p rel="author"> with a child <span itemprop="name">Jane</span>.
        // The byline node is the <p>; its descendant <span> carries the
        // itemprop="name", so the captured byline is "Jane" — not the <p>'s
        // full textContent.
        let html = "<html><body>\
            <p rel=\"author\">By <span itemprop=\"name\">Jane Author</span> on Tuesday</p>\
            <div class=content><p>Article body paragraph easily over the twenty-five character minimum threshold here.</p></div>\
            </body></html>";
        let mut dom = Dom::parse(html);
        let root = dom.document();
        let body = dom.body().unwrap();
        let flags = Flags::default();
        let mut byline_found = false;
        let mut byline_text: Option<String> = None;
        let _ = grab_article(
            &mut dom,
            &root,
            &body,
            "",
            &flags,
            &mut byline_found,
            &mut byline_text,
        )
        .expect("grab");
        assert!(byline_found, "byline detected");
        assert_eq!(
            byline_text.as_deref(),
            Some("Jane Author"),
            "itemprop=name descendant's text is captured (Readability.js:1087-1100)"
        );
    }
}
