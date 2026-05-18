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

    // `neededToCreateTopCandidate` (`Readability.js:1314`) gates ONLY the
    // score-invisible 1517-1532 page-wrap (assigning `id="readability-page-1"`
    // / `className="page"` and, in the non-fallback arm, wrapping
    // articleContent's children in an extra `<div>`). Both are id/className
    // (score-invisible, HLD §2) and a wrapper DIV adds ZERO `#text`
    // characters, so 1517-1532 is provably `text_content`-invariant and is
    // deliberately NOT ported at Stage 1b (recorded in notes/m2-stage1b.md;
    // the variable returns when Stage 1c needs the `_attempts` bookkeeping
    // context). It therefore intentionally does not exist here.

    // If no top candidate, OR it is BODY: build a fake DIV from page children
    // (Readability.js:1314-1327). Needed for the first Ok.
    if top_candidate.is_none()
        || top_candidate
            .as_ref()
            .map(|t| tag_name(t).as_deref() == Some("BODY"))
            .unwrap_or(false)
    {
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

    // (1508-1510 debug — SKIP. 1512 `this._prepArticle(articleContent)` is
    //  invoked by `mod.rs::parse` AFTER this returns — Stage-1a wiring, kept;
    //  it mirrors the JS call site, which is also after the sibling loop.
    //  1517-1532 the `readability-page-1`/`page` wrap: id/className are
    //  score-invisible (HLD §2) AND wrapping articleContent's children in an
    //  extra DIV adds ZERO `#text` characters, so it is provably
    //  `text_content`-invariant — deliberately NOT implemented here, recorded
    //  in notes/m2-stage1b.md. 1538+ `parseSuccessful`/the
    //  `textLength < charThreshold` retry is Stage 1c — STOP here, exactly as
    //  Stage 1a STOPPED at 1415.)

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
    use crate::readability::dom::{Dom, get_elements_by_tag_name, text_content};

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
        // The SECTION ∈ ALTER_TO_DIV_EXCEPTIONS ⇒ appended as-is (still a
        // SECTION element directly under articleContent).
        let section_kids: Vec<String> = children(&ac)
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
}
