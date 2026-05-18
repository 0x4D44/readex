//! `scoring.rs` — Readability's content-scoring primitives, ported faithfully
//! with the JS float operation order preserved exactly (HLD §5, §7.1).
//!
//! Each function cites its `Readability.js:<line>` (anti-inversion, HLD
//! §4.3(a)). The scoring state (`node.readability.contentScore`) lives in the
//! [`Dom`](super::dom::Dom) point-query side table (HLD §5.1), never in a
//! field on the node, so these take `&Dom`/`&mut Dom`.
//!
//! **Numeric fidelity (HLD §9):** the in-scope scoring path has no
//! `Math.round` and no `parseInt` (those are Stage-2 `_getRowAndColumnCount`);
//! the only arithmetic is `f64` `+`/`-`/`*`/`/` and `Math.min`/`Math.floor`,
//! all of which IEEE-754 `f64` reproduces bit-for-bit in the same evaluation
//! order. The order is therefore transcribed verbatim, not "equivalently".

use crate::readability::dom::{
    self, Dom, NodeRef, class_name, get_attribute, get_elements_by_tag_name, id, inner_text,
    parent, tag_name,
};
use crate::readability::helpers::{FLAG_WEIGHT_CLASSES, Flags};
use crate::readability::regexps;

/// `_initializeNode(node)` (`Readability.js:893-930`).
///
/// Sets `node.readability = { contentScore: 0 }`, adds the per-tag base score,
/// then adds `_getClassWeight(node)`.
pub fn initialize_node(dom: &mut Dom, flags: &Flags, node: &NodeRef) {
    // node.readability = { contentScore: 0 };
    let mut score = 0.0_f64;
    // switch (node.tagName) — tag_name is UPPER-cased (matches JS tagName).
    match tag_name(node).as_deref() {
        Some("DIV") => score += 5.0,
        Some("PRE") | Some("TD") | Some("BLOCKQUOTE") => score += 3.0,
        Some("ADDRESS") | Some("OL") | Some("UL") | Some("DL") | Some("DD") | Some("DT")
        | Some("LI") | Some("FORM") => score -= 3.0,
        Some("H1") | Some("H2") | Some("H3") | Some("H4") | Some("H5") | Some("H6")
        | Some("TH") => score -= 5.0,
        _ => {}
    }
    // node.readability.contentScore += this._getClassWeight(node);
    score += get_class_weight(flags, node) as f64;
    dom.set_content_score(node, score);
}

/// `_getClassWeight(e)` (`Readability.js:2142-2172`). Returns an **integer**
/// (JS comment says "number (Integer)"); the only values are 0, ±25, ±50.
///
/// `0` if `FLAG_WEIGHT_CLASSES` inactive; else `-25`/`+25` for negative/
/// positive class regex, same again for the id regex (independently summed).
pub fn get_class_weight(flags: &Flags, e: &NodeRef) -> i32 {
    if !flags.is_active(FLAG_WEIGHT_CLASSES) {
        return 0;
    }
    let mut weight = 0_i32;

    // className (typeof string && !== "") — dom::class_name is "" if absent,
    // matching `(getAttribute("class") || "")`.
    let cls = class_name(e);
    if !cls.is_empty() {
        if regexps::negative().is_match(&cls) {
            weight -= 25;
        }
        if regexps::positive().is_match(&cls) {
            weight += 25;
        }
    }

    // id (typeof string && !== "")
    let id_str = id(e);
    if !id_str.is_empty() {
        if regexps::negative().is_match(&id_str) {
            weight -= 25;
        }
        if regexps::positive().is_match(&id_str) {
            weight += 25;
        }
    }

    weight
}

/// `_getInnerText(e, normalizeSpaces=true)` length helper. JS `.length` is
/// UTF-16 code units; Readability compares it against small integer thresholds
/// (25, 80, 100, charThreshold). We use Rust `char` count, which equals the
/// UTF-16 length for the BMP text these comparisons see; an astral-char
/// divergence would require padding text past a threshold purely with astral
/// codepoints, which the corpus does not do. `inner_text` already encodes the
/// JS trim+`/\s{2,}/g` collapse (dialect-faithful, HLD §8).
pub fn inner_text_len(node: &NodeRef) -> usize {
    inner_text(node, true).chars().count()
}

/// `_getLinkDensity(element)` (`Readability.js:2117-2133`).
///
/// `linkLength / textLength` where `textLength = _getInnerText(element).length`
/// (0 ⇒ return 0), and each descendant `<a>` contributes
/// `_getInnerText(a).length * (hashUrl.test(href) ? 0.3 : 1)`.
pub fn get_link_density(element: &NodeRef) -> f64 {
    let text_length = inner_text_len(element) as f64;
    if text_length == 0.0 {
        return 0.0;
    }
    let mut link_length = 0.0_f64;
    for link_node in get_elements_by_tag_name(element, "a") {
        let href = get_attribute(&link_node, "href");
        let coefficient = match href.as_deref() {
            Some(h) if regexps::hash_url().is_match(h) => 0.3,
            _ => 1.0,
        };
        link_length += inner_text_len(&link_node) as f64 * coefficient;
    }
    link_length / text_length
}

/// `_getCharCount(e, s=",")` (`Readability.js:2076-2079`):
/// `_getInnerText(e).split(s).length - 1`. JS `String.split(string)` with a
/// non-empty separator yields `occurrences + 1` parts, so this is the count of
/// `s` occurrences. (Used by Stage-2 `_cleanConditionally`; provided here with
/// the rest of the scoring family, exercised by a unit test.)
pub fn get_char_count(e: &NodeRef, s: &str) -> usize {
    debug_assert!(!s.is_empty(), "JS default separator is ',' (non-empty)");
    inner_text(e, true).matches(s).count()
}

/// `_getNodeAncestors(node, maxDepth=0)` (`Readability.js:1009-1021`).
///
/// Walk `parentNode` upward collecting ancestors; if `maxDepth > 0` stop after
/// `maxDepth` of them. `maxDepth = maxDepth || 0` so `0`/`None` ⇒ unbounded.
pub fn get_node_ancestors(node: &NodeRef, max_depth: usize) -> Vec<NodeRef> {
    let mut ancestors = Vec::new();
    let mut cur = node.clone();
    let mut i = 0_usize;
    while let Some(p) = parent(&cur) {
        ancestors.push(p.clone());
        if max_depth > 0 {
            i += 1;
            if i == max_depth {
                break;
            }
        }
        cur = p;
    }
    ancestors
}

/// `_textSimilarity(textA, textB)` (`Readability.js:971-986`).
///
/// Tokenize both (lowercase → split `REGEXPS.tokenize` `/\W+/g` ASCII →
/// drop-empty). Empty either side ⇒ 0. `distanceB = uniqTokensB.join(" ").length
/// / tokensB.join(" ").length`; return `1 - distanceB`. The `.length`s are JS
/// UTF-16 code units; for the title/heading text this gates (`> 0.75`) the
/// inputs are ordinary prose — `char` count matches UTF-16 for the BMP and the
/// ratio is robust to the rare astral char. **Float order verbatim.**
pub fn text_similarity(text_a: &str, text_b: &str) -> f64 {
    let tokens_a = tokenize_lower(text_a);
    let tokens_b = tokenize_lower(text_b);
    if tokens_a.is_empty() || tokens_b.is_empty() {
        return 0.0;
    }
    // uniqTokensB = tokensB.filter(t => !tokensA.includes(t))
    let uniq_b: Vec<&str> = tokens_b
        .iter()
        .filter(|t| !tokens_a.iter().any(|a| a == *t))
        .map(|s| s.as_str())
        .collect();
    // distanceB = uniqB.join(" ").length / tokensB.join(" ").length
    let uniq_join_len = join_space_len(&uniq_b);
    let all_join_len = join_space_len(&tokens_b.iter().map(|s| s.as_str()).collect::<Vec<_>>());
    let distance_b = uniq_join_len as f64 / all_join_len as f64;
    1.0 - distance_b
}

/// `s.toLowerCase().split(REGEXPS.tokenize).filter(Boolean)`.
///
/// JS `String.prototype.split(regex)` with `/\W+/g` plus `.filter(Boolean)`
/// (drop empty strings). A leading match yields a leading "" which `filter`
/// drops; consecutive non-word chars collapse (the `+`). Equivalent to
/// "split on runs of ASCII non-word chars, discard empties" — exactly Rust
/// `Regex::split` with the same pattern then dropping empties.
fn tokenize_lower(s: &str) -> Vec<String> {
    let lower = s.to_lowercase();
    regexps::tokenize()
        .split(&lower)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string())
        .collect()
}

/// `arr.join(" ").length` in JS UTF-16 code units, approximated by Rust
/// `char` count (BMP-exact; see `_textSimilarity` note). `join(" ")` places a
/// single space between elements: `sum(len) + (n-1)` for `n>0`, else `0`.
fn join_space_len(parts: &[&str]) -> usize {
    if parts.is_empty() {
        return 0;
    }
    let chars: usize = parts.iter().map(|p| p.chars().count()).sum();
    chars + (parts.len() - 1)
}

/// `dom.createElement`/text-node helper kept here so callers in this module
/// can synthesize without importing `dom` twice. (No behaviour — re-export.)
#[allow(unused_imports)]
pub(crate) use dom::create_text_node;

#[cfg(test)]
mod tests {
    //! Every expected number hand-derived by tracing `Readability.js`
    //! arithmetic (NOT by running an oracle — inversion, HLD §4).
    use super::*;
    use crate::readability::dom::Dom;

    fn el(html: &str, tag: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let n = get_elements_by_tag_name(&dom.body().unwrap(), tag)[0].clone();
        (dom, n)
    }

    // ---- _getClassWeight (Readability.js:2142-2172) ----

    #[test]
    fn class_weight_zero_when_flag_off() {
        let mut f = Flags::default();
        f.remove(FLAG_WEIGHT_CLASSES);
        let (_d, n) = el(r#"<div class="article content">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), 0);
    }

    #[test]
    fn class_weight_positive_negative_class_and_id() {
        let f = Flags::default();
        // positive class only -> +25
        let (_d, n) = el(r#"<div class="article">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), 25);
        // negative class only -> -25
        let (_d, n) = el(r#"<div class="sidebar">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), -25);
        // negative + positive class -> -25 + 25 = 0
        let (_d, n) = el(r#"<div class="sidebar article">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), 0);
        // positive class + positive id -> 25 + 25 = 50
        let (_d, n) = el(r#"<div class="article" id="post">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), 50);
        // negative class + negative id -> -50
        let (_d, n) = el(r#"<div class="footer" id="comment">x</div>"#, "div");
        assert_eq!(get_class_weight(&f, &n), -50);
        // no class/id -> 0
        let (_d, n) = el("<div>x</div>", "div");
        assert_eq!(get_class_weight(&f, &n), 0);
    }

    // ---- _initializeNode (Readability.js:893-930) ----

    #[test]
    fn initialize_node_base_scores_plus_class_weight() {
        let f = Flags::default();
        // DIV +5, positive class +25 -> 30
        let (mut d, n) = el(r#"<div class="content">x</div>"#, "div");
        initialize_node(&mut d, &f, &n);
        assert_eq!(d.content_score(&n), Some(30.0));
        // TD +3, no class -> 3
        let (mut d, n) = el("<table><tr><td>x</td></tr></table>", "td");
        initialize_node(&mut d, &f, &n);
        assert_eq!(d.content_score(&n), Some(3.0));
        // H2 -5, no class -> -5
        let (mut d, n) = el("<h2>x</h2>", "h2");
        initialize_node(&mut d, &f, &n);
        assert_eq!(d.content_score(&n), Some(-5.0));
        // UL -3, negative class -25 -> -28
        let (mut d, n) = el(r#"<ul class="sidebar"><li>x</li></ul>"#, "ul");
        initialize_node(&mut d, &f, &n);
        assert_eq!(d.content_score(&n), Some(-28.0));
        // unlisted tag (P) -> 0 base + 0 class = 0
        let (mut d, n) = el("<p>x</p>", "p");
        initialize_node(&mut d, &f, &n);
        assert_eq!(d.content_score(&n), Some(0.0));
    }

    // ---- _getLinkDensity (Readability.js:2117-2133) ----

    #[test]
    fn link_density_zero_text_is_zero() {
        let (_d, n) = el("<div></div>", "div");
        assert_eq!(get_link_density(&n), 0.0);
    }

    #[test]
    fn link_density_half_text_in_link() {
        // text "AAAABBBB" (8 chars); one <a> with "BBBB" (4), non-hash href.
        // density = 4*1 / 8 = 0.5
        let (_d, n) = el(r#"<div>AAAA<a href="/x">BBBB</a></div>"#, "div");
        assert!((get_link_density(&n) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn link_density_hash_href_coefficient_0_3() {
        // text "AAAABBBB" (8); <a href="#sec">BBBB</a> -> 4*0.3 / 8 = 0.15
        let (_d, n) = el(r##"<div>AAAA<a href="#sec">BBBB</a></div>"##, "div");
        assert!((get_link_density(&n) - 0.15).abs() < 1e-12);
    }

    #[test]
    fn link_density_no_href_uses_coefficient_1() {
        // <a> with no href -> coefficient 1. "AAAABBBB" -> 4/8 = 0.5
        let (_d, n) = el(r#"<div>AAAA<a>BBBB</a></div>"#, "div");
        assert!((get_link_density(&n) - 0.5).abs() < 1e-12);
    }

    // ---- _getCharCount (Readability.js:2076-2079) ----

    #[test]
    fn char_count_commas() {
        let (_d, n) = el("<p>a,b,c,d</p>", "p");
        assert_eq!(get_char_count(&n, ","), 3); // "a,b,c,d".split(",").length-1 = 3
        let (_d, n) = el("<p>no commas here</p>", "p");
        assert_eq!(get_char_count(&n, ","), 0);
    }

    // ---- _getNodeAncestors (Readability.js:1009-1021) ----

    #[test]
    fn node_ancestors_bounded_and_unbounded() {
        // body > div > section > p. JS `_getNodeAncestors` walks
        // `while (node.parentNode)` and pushes parentNode each step. For
        // `<html>`, `html.parentNode` is the *document* (a truthy object in
        // jsdom), so the document IS pushed; then `document.parentNode` is
        // null and the loop stops. So the full ancestor list (filter_map drops
        // the Document, which has no tagName) is:
        //   [SECTION, DIV, BODY, HTML]  (+ Document, tagName-less)
        // and the *length* including Document is 5.
        let dom = Dom::parse("<div><section><p>x</p></section></div>");
        let p = get_elements_by_tag_name(&dom.body().unwrap(), "p")[0].clone();
        let all = get_node_ancestors(&p, 0);
        let tags: Vec<String> = all.iter().filter_map(tag_name).collect();
        assert_eq!(tags, vec!["SECTION", "DIV", "BODY", "HTML"]);
        // 5 raw ancestors incl. the tagName-less Document (faithful to JS:
        // SECTION, DIV, BODY, HTML, #document).
        assert_eq!(get_node_ancestors(&p, 0).len(), 5);
        // maxDepth=2 -> first 2 only
        let two = get_node_ancestors(&p, 2);
        let t2: Vec<String> = two.iter().filter_map(tag_name).collect();
        assert_eq!(t2, vec!["SECTION", "DIV"]);
        // maxDepth=5 caps at 5 (SECTION,DIV,BODY,HTML,#document) — exactly the
        // `_getNodeAncestors(elementToScore, 5)` call in `_grabArticle`.
        assert_eq!(get_node_ancestors(&p, 5).len(), 5);
    }

    // ---- _textSimilarity (Readability.js:971-986) ----

    #[test]
    fn text_similarity_identical_is_one() {
        // tokensA == tokensB ; uniqB empty -> distance 0 -> 1.0
        assert!((text_similarity("Hello World", "hello world") - 1.0).abs() < 1e-12);
    }

    #[test]
    fn text_similarity_completely_different() {
        // A="abc", B="xyz qrs". tokensB=["xyz","qrs"]; uniqB=["xyz","qrs"].
        // uniqB.join(" ")="xyz qrs" len 7; tokensB.join(" ")="xyz qrs" len 7.
        // distance 7/7=1 -> 1-1 = 0.
        assert!((text_similarity("abc", "xyz qrs")).abs() < 1e-12);
    }

    #[test]
    fn text_similarity_partial_overlap_exact_ratio() {
        // A = "the apple"  -> tokensA=["the","apple"]
        // B = "the apple pie shop" -> tokensB=["the","apple","pie","shop"]
        // uniqB = ["pie","shop"]; join=" " -> "pie shop" length 8
        // tokensB.join(" ") = "the apple pie shop" length 18
        // distanceB = 8/18 ; result = 1 - 8/18
        let got = text_similarity("the apple", "the apple pie shop");
        let want = 1.0 - (8.0 / 18.0);
        assert!((got - want).abs() < 1e-12, "got {got}, want {want}");
    }

    #[test]
    fn text_similarity_empty_side_is_zero() {
        assert_eq!(text_similarity("", "anything"), 0.0);
        assert_eq!(text_similarity("anything", ""), 0.0);
        // all-punct tokenizes to nothing -> empty -> 0
        assert_eq!(text_similarity("...", "abc"), 0.0);
    }

    #[test]
    fn text_similarity_header_dup_threshold_case() {
        // A header that is the title plus a couple extra words: the JS
        // _headerDuplicatesTitle gate is `> 0.75`. Trace:
        // title A = "Apple Inc"  tokensA=["apple","inc"]
        // heading B = "Apple Inc"  -> identical -> sim 1.0 > 0.75 (removed)
        assert!(text_similarity("Apple Inc", "Apple Inc") > 0.75);
        // B = "Apple Inc Wikipedia" -> uniqB=["wikipedia"](9);
        //   tokensB.join=" " "apple inc wikipedia"(19); 1-9/19 ≈ 0.526 < 0.75
        assert!(text_similarity("Apple Inc", "Apple Inc Wikipedia") < 0.75);
    }
}
