//! `metadata.rs` — title resolution, pulled forward to Stage 1a (HLD §7.1 /
//! supervisor M-4) because `_grabArticle`'s `_headerDuplicatesTitle`
//! (`Readability.js:1105`) deletes headings via
//! `_textSimilarity(this._articleTitle, heading) > 0.75` and so the article
//! title **directly changes the scored body text** (`_articleTitle` is set at
//! `Readability.js:2745`, before `_grabArticle` at `2747`).
//!
//! Stage-1a scope: `_getArticleTitle` (`Readability.js:572-651`) + the
//! **title half** of `_getArticleMetadata` (`Readability.js:1803-1816`). The
//! rest of `_getArticleMetadata` / `_getJSONLD` is Stage 4 (HLD §7.6) — JSON-LD
//! is `{}` here (Stage-1a `parse()` does not call `_getJSONLD`), so the
//! `jsonld.title` term is absent.
//!
//! Faithful transcription with `Readability.js:<line>` citations
//! (anti-inversion, HLD §4.3(a)).

use crate::readability::dom::{
    NodeRef, get_all_nodes_with_tag, get_attribute, get_elements_by_tag_name, inner_text,
    is_js_space, text_content,
};
use crate::readability::regexps;

/// `document.title` (WHATWG): the child text content of the **first** `<title>`
/// element (in tree order, anywhere — normally `<head>`), with the HTML
/// "strip and collapse ASCII whitespace" algorithm applied (strip leading/
/// trailing ASCII whitespace, collapse interior runs to one U+0020). Returns
/// `""` if there is no `<title>` (matching jsdom).
///
/// ASCII whitespace per WHATWG = TAB U+0009, LF U+000A, FF U+000C, CR U+000D,
/// SPACE U+0020 (note: **not** the JS-`\s` set — `document.title` uses the
/// HTML ASCII-whitespace set, distinct from `String.prototype.trim`).
fn document_title(doc_root: &NodeRef) -> String {
    let titles = get_elements_by_tag_name(doc_root, "title");
    let Some(t) = titles.first() else {
        return String::new();
    };
    strip_and_collapse_ascii_whitespace(&text_content(t))
}

/// WHATWG "strip and collapse ASCII whitespace".
fn strip_and_collapse_ascii_whitespace(s: &str) -> String {
    fn is_ascii_ws(c: char) -> bool {
        matches!(
            c,
            '\u{0009}' | '\u{000A}' | '\u{000C}' | '\u{000D}' | '\u{0020}'
        )
    }
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    let mut started = false;
    for c in s.chars() {
        if is_ascii_ws(c) {
            in_run = true;
        } else {
            if in_run && started {
                out.push(' ');
            }
            out.push(c);
            started = true;
            in_run = false;
        }
    }
    out
}

/// JS `String.prototype.trim`: strip leading/trailing JS-`\s`
/// (the ECMAScript whitespace set — NBSP, U+FEFF, …; the canonical
/// [`is_js_space`] predicate, **not** the ASCII-whitespace set
/// `document.title` uses), as used by `_getArticleTitle`.
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_space)
}

/// JS `str.split(/\s+/).length` (the `wordCount` inner function,
/// `Readability.js:591-593`).
///
/// `String.prototype.split(/\s+/)`: a leading whitespace run yields a leading
/// `""` element; otherwise splits on runs of JS-`\s`. `"".split(/\s+/)` →
/// `[""]` (length 1). `" a".split(/\s+/)` → `["", "a"]` (length 2). We
/// replicate JS `split` semantics exactly (NOT "count words").
fn word_count(s: &str) -> usize {
    // Regex split mirrors JS String.split(regex): produces leading/trailing
    // "" for boundary matches; we DON'T filter them (JS .length counts them).
    let parts: Vec<&str> = regexps::ws_plus().split(s).collect();
    parts.len()
}

/// `_getArticleTitle()` (`Readability.js:572-651`). Faithful transcription.
///
/// `doc_root` is `this._doc` (the document; `_getAllNodesWithTag`/
/// `getElementsByTagName` search its descendants). `doc.title` is resolved via
/// [`document_title`] (WHATWG semantics — jsdom).
pub fn get_article_title(doc_root: &NodeRef) -> String {
    // curTitle = origTitle = doc.title.trim();
    let orig_title = js_trim(&document_title(doc_root)).to_string();
    let mut cur_title = orig_title.clone();
    // (the `typeof curTitle !== "string"` branch is unreachable: doc.title is
    // always a string in jsdom — faithfully a no-op here.)

    let mut title_had_hierarchical_separators = false;

    // if (/ [\|\-\\\/>»] /.test(curTitle))
    if regexps::title_separator().is_match(&cur_title) {
        // titleHadHierarchicalSeparators = / [\\\/>»] /.test(curTitle)
        title_had_hierarchical_separators = regexps::title_hier_separator().is_match(&cur_title);
        // allSeparators = Array.from(origTitle.matchAll(/ [\|\-\\\/>»] /gi));
        // curTitle = origTitle.substring(0, allSeparators.pop().index);
        if let Some(last) = regexps::title_separator().find_iter(&orig_title).last() {
            cur_title = byte_substring(&orig_title, 0, last.start());
        }
        // if (wordCount(curTitle) < 3)
        if word_count(&cur_title) < 3 {
            // curTitle = origTitle.replace(/^[^\|\-\\\/>»]*[\|\-\\\/>»]/gi, "")
            cur_title = regexps::title_lead_separator()
                .replace(&orig_title, "")
                .into_owned();
        }
    } else if cur_title.contains(": ") {
        // headings = _getAllNodesWithTag(doc, ["h1","h2"]);
        let headings = get_all_nodes_with_tag(doc_root, &["h1", "h2"]);
        let trimmed_title = js_trim(&cur_title).to_string();
        // match = _someNode(headings, h => h.textContent.trim() === trimmedTitle)
        let matched = headings
            .iter()
            .any(|h| js_trim(&text_content(h)) == trimmed_title);
        if !matched {
            // curTitle = origTitle.substring(origTitle.lastIndexOf(":") + 1)
            if let Some(pos) = orig_title.rfind(':') {
                cur_title = byte_substring(&orig_title, pos + ':'.len_utf8(), orig_title.len());
            }
            if word_count(&cur_title) < 3 {
                // curTitle = origTitle.substring(origTitle.indexOf(":") + 1)
                if let Some(pos) = orig_title.find(':') {
                    cur_title = byte_substring(&orig_title, pos + ':'.len_utf8(), orig_title.len());
                }
            } else if word_count(before_first_colon(&orig_title)) > 5 {
                // wordCount(origTitle.substr(0, origTitle.indexOf(":"))) > 5
                cur_title = orig_title.clone();
            }
        }
    } else if cur_title.chars().count() > 150 || cur_title.chars().count() < 15 {
        // hOnes = doc.getElementsByTagName("h1");
        let h_ones = get_elements_by_tag_name(doc_root, "h1");
        if h_ones.len() == 1 {
            // curTitle = _getInnerText(hOnes[0])  (normalizeSpaces default true)
            cur_title = inner_text(&h_ones[0], true);
        }
    }

    // curTitle = curTitle.trim().replace(REGEXPS.normalize, " ");
    cur_title = regexps::normalize()
        .replace_all(js_trim(&cur_title), " ")
        .into_owned();

    // if (curTitleWordCount <= 4 && (!titleHadHierarchicalSeparators ||
    //     curTitleWordCount != wordCount(origTitle.replace(/[\|\-\\\/>»]+/g,""))-1))
    let cur_title_word_count = word_count(&cur_title);
    if cur_title_word_count <= 4 {
        let orig_no_sep = regexps::title_separators_run().replace_all(&orig_title, "");
        let cond = !title_had_hierarchical_separators
            || cur_title_word_count != word_count(&orig_no_sep).wrapping_sub(1);
        if cond {
            cur_title = orig_title.clone();
        }
    }

    cur_title
}

/// `origTitle.substr(0, origTitle.indexOf(":"))` — the substring before the
/// first `:` (or the whole string if none, since `indexOf` returns -1 and
/// `substr(0,-1)` in JS → `""`; but JS `substr(0, -1)` actually yields `""`).
/// Faithfully: if there is no `:`, JS `indexOf` is `-1` and
/// `"abc".substr(0,-1)` === `""`.
fn before_first_colon(s: &str) -> &str {
    match s.find(':') {
        Some(p) => &s[..p],
        None => "", // substr(0, -1) === ""
    }
}

/// JS `String.prototype.substring(start, end)` over **UTF-16-ish** indices.
/// Readability only ever slices at byte offsets we computed from the same
/// string (regex match index / `indexOf`), so byte slicing on the original
/// `&str` is the faithful operation here (the offsets are UTF-8 byte offsets
/// from `find`/`Regex::find` on this exact string, which land on char
/// boundaries). Clamps defensively.
fn byte_substring(s: &str, start: usize, end: usize) -> String {
    let (a, b) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    let a = a.min(s.len());
    let b = b.min(s.len());
    // a,b are derived from char-boundary-safe sources; guard anyway.
    if s.is_char_boundary(a) && s.is_char_boundary(b) {
        s[a..b].to_string()
    } else {
        // Defensive fallback (never hit on real inputs): char-walk.
        s.chars().skip(a).take(b.saturating_sub(a)).collect()
    }
}

/// The title half of `_getArticleMetadata(jsonld)` (`Readability.js:1803-1816`).
///
/// `metadata.title = jsonld.title || values["dc:title"] || … ||
/// values["parsely-title"]`; if still falsy, `_getArticleTitle()`. At Stage 1a
/// `jsonld` is `{}` (no `_getJSONLD`), so `jsonld.title` is absent. `values`
/// is built from `<meta>` tags exactly as `Readability.js:1771-1800`.
///
/// This is the value assigned to `this._articleTitle` (`Readability.js:2745`)
/// and consumed by `_headerDuplicatesTitle` on the scored path.
pub fn get_article_metadata_title(doc_root: &NodeRef) -> String {
    let values = collect_meta_values(doc_root);

    // metadata.title precedence (Readability.js:1803-1812), jsonld.title absent.
    for key in [
        "dc:title",
        "dcterm:title",
        "og:title",
        "weibo:article:title",
        "weibo:webpage:title",
        "title",
        "twitter:title",
        "parsely-title",
    ] {
        if let Some(v) = values.get(key)
            && !v.is_empty()
        {
            return v.clone();
        }
    }
    // if (!metadata.title) metadata.title = this._getArticleTitle();
    get_article_title(doc_root)
}

/// Build the `values` map from `<meta>` elements
/// (`Readability.js:1760-1800`), restricted to the **title-relevant** keys
/// (the only ones Stage 1a reads — Stage 4 widens this to author/excerpt/etc).
///
/// `propertyPattern` = `/\s*(article|dc|dcterm|og|twitter)\s*:\s*(author|
/// creator|description|published_time|title|site_name)\s*/gi` — for
/// `<meta property>`. `namePattern` = `/^\s*(?:(dc|dcterm|og|twitter|parsely|
/// weibo:(article|webpage))\s*[-\.:]\s*)?(author|creator|pub-date|description|
/// title|site_name)\s*$/i` — for `<meta name>`. We faithfully apply both, then
/// keep title keys.
fn collect_meta_values(doc_root: &NodeRef) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut values: HashMap<String, String> = HashMap::new();

    for element in get_elements_by_tag_name(doc_root, "meta") {
        let element_name = get_attribute(&element, "name");
        let element_property = get_attribute(&element, "property");
        let content = match get_attribute(&element, "content") {
            Some(c) if !c.is_empty() => c,
            // `if (!content) return;` — empty/missing content skips.
            _ => continue,
        };

        let mut matched = false;
        if let Some(prop) = element_property.as_deref()
            && let Some(m) = regexps::meta_property_pattern().find(prop)
        {
            // name = matches[0].toLowerCase().replace(/\s/g, "")
            let name = strip_js_ws(&m.as_str().to_lowercase());
            values.insert(name, js_trim(&content).to_string());
            matched = true;
        }

        if !matched
            && let Some(en) = element_name.as_deref()
            && regexps::meta_name_pattern().is_match(en)
        {
            // name = name.toLowerCase().replace(/\s/g,"").replace(/\./g,":")
            let name = strip_js_ws(&en.to_lowercase()).replace('.', ":");
            values.insert(name, js_trim(&content).to_string());
        }
    }
    values
}

/// `str.replace(/\s/g, "")` with the JS `\s` set — remove **all** JS
/// whitespace (not just trim).
fn strip_js_ws(s: &str) -> String {
    regexps::js_space_any().replace_all(s, "").into_owned()
}

#[cfg(test)]
mod tests {
    //! Expected titles hand-derived by tracing `Readability.js:572-651` /
    //! `:1803-1816` (NOT by running an oracle — inversion, HLD §4).
    use super::*;
    use crate::readability::dom::Dom;

    fn doc(html: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let root = dom.root_element().unwrap();
        (dom, root)
    }

    // ---- document.title / strip-and-collapse ----

    #[test]
    fn document_title_strips_and_collapses_ascii_ws() {
        let (_d, r) =
            doc("<html><head><title>  Hello   World \n </title></head><body></body></html>");
        assert_eq!(document_title(&r), "Hello World");
        let (_d, r) = doc("<html><head></head><body></body></html>");
        assert_eq!(document_title(&r), "");
    }

    // ---- wordCount = str.split(/\s+/).length (Readability.js:591-593) ----

    #[test]
    fn word_count_matches_js_split_semantics() {
        assert_eq!(word_count("a b c"), 3);
        assert_eq!(word_count(""), 1); // "".split(/\s+/) -> [""]
        assert_eq!(word_count(" a"), 2); // ["","a"]
        assert_eq!(word_count("a "), 2); // ["a",""]
        assert_eq!(word_count("one"), 1);
    }

    // ---- _getArticleTitle (Readability.js:572-651) ----

    #[test]
    fn article_title_plain_no_separator() {
        // No separator, length in [15,150], not ": " -> returned trimmed-norm.
        // But wordCount<=4 fallback: "A Reasonable Page Title" = 4 words,
        // titleHadHierarchicalSeparators=false -> cond true -> origTitle.
        let (_d, r) =
            doc("<html><head><title>A Reasonable Page Title</title></head><body></body></html>");
        assert_eq!(get_article_title(&r), "A Reasonable Page Title");
    }

    #[test]
    fn article_title_pipe_separator_drops_last_part() {
        // "Great Article Name | Site Name" : has " | " separator.
        // allSeparators last at the " | "; curTitle = substring before it =
        // "Great Article Name". wordCount=3 (>=3, no first-part fallback).
        // titleHadHierarchicalSeparators = / [\\\/>»] /.test -> false (pipe).
        // final: wordCount=3 >4? no, <=4 -> cond (!hier || ...) = (!false)=true
        //   -> curTitle = origTitle?? WAIT: trace carefully.
        // curTitleWordCount = 3 <= 4 ; titleHadHierarchicalSeparators=false
        //   so (!false)=true -> curTitle = origTitle = full string.
        let (_d, r) = doc(
            "<html><head><title>Great Article Name | Site Name</title></head><body></body></html>",
        );
        // Per faithful trace the <=4-word guard restores origTitle.
        assert_eq!(get_article_title(&r), "Great Article Name | Site Name");
    }

    #[test]
    fn article_title_long_pipe_separator_keeps_shortened() {
        // Long enough first part (>4 words) so the <=4 guard does NOT restore.
        // "The Quick Brown Fox Jumps Over | Site" -> sep " | " ;
        // curTitle = "The Quick Brown Fox Jumps Over" (6 words, >=3).
        // hier seps? / [\\\/>»] / -> false. trim+normalize unchanged.
        // curTitleWordCount=6 > 4 -> guard skipped -> result stays.
        let (_d, r) = doc(
            "<html><head><title>The Quick Brown Fox Jumps Over | Site</title></head><body></body></html>",
        );
        assert_eq!(get_article_title(&r), "The Quick Brown Fox Jumps Over");
    }

    #[test]
    fn article_title_short_title_single_h1_used() {
        // curTitle.length < 15 and exactly one <h1> -> use h1 inner text.
        // "Hi" (2 chars <15), no separator, not ": ". hOnes length 1.
        // curTitle = innerText(h1) = "The Real Heading Of The Page".
        // Then <=4 guard: wordCount=6 >4 -> kept.
        let (_d, r) = doc(
            "<html><head><title>Hi</title></head><body><h1>The Real Heading Of The Page</h1></body></html>",
        );
        assert_eq!(get_article_title(&r), "The Real Heading Of The Page");
    }

    #[test]
    fn article_title_colon_space_no_heading_match_takes_after_last_colon() {
        // 'Site: A Fine Long Article Title Here' contains ": ", no h1/h2
        // exactly equal -> curTitle = substring after lastIndexOf(":")+1 =
        // " A Fine Long Article Title Here". wordCount=6 (>=3) so not the
        // first-colon branch; wordCount(before first colon)= 'Site' =1, not >5.
        // trim+normalize -> "A Fine Long Article Title Here". 6 words >4 kept.
        let (_d, r) = doc(
            "<html><head><title>Site: A Fine Long Article Title Here</title></head><body><h2>Unrelated</h2></body></html>",
        );
        assert_eq!(get_article_title(&r), "A Fine Long Article Title Here");
    }

    #[test]
    fn article_title_colon_with_matching_heading_uses_full() {
        // ": " present AND an <h1> whose trimmed textContent === trimmed
        // curTitle -> match true -> the colon-splitting is skipped, curTitle
        // stays the full title (then trim/normalize, >4 words kept).
        let (_d, r) = doc(
            "<html><head><title>Brand: The Whole Real Long Title</title></head>\
             <body><h1>Brand: The Whole Real Long Title</h1></body></html>",
        );
        assert_eq!(get_article_title(&r), "Brand: The Whole Real Long Title");
    }

    // ---- _getArticleMetadata title half (Readability.js:1803-1816) ----

    #[test]
    fn metadata_title_prefers_og_title_meta() {
        let (_d, r) = doc(
            r#"<html><head><meta property="og:title" content="OG Title Wins"><title>Doc Title</title></head><body></body></html>"#,
        );
        assert_eq!(get_article_metadata_title(&r), "OG Title Wins");
    }

    #[test]
    fn metadata_title_name_twitter_title() {
        let (_d, r) = doc(
            r#"<html><head><meta name="twitter:title" content="Tw Title"><title>Doc</title></head><body></body></html>"#,
        );
        assert_eq!(get_article_metadata_title(&r), "Tw Title");
    }

    #[test]
    fn metadata_title_falls_back_to_get_article_title() {
        // No usable meta -> _getArticleTitle(); single long title, >4 words.
        let (_d, r) = doc(
            "<html><head><title>A Plain Old Document Heading</title></head><body></body></html>",
        );
        assert_eq!(
            get_article_metadata_title(&r),
            "A Plain Old Document Heading"
        );
    }

    #[test]
    fn metadata_title_ignores_empty_content_meta() {
        // empty content -> skipped (Readability.js:1775 `if (!content) return`)
        let (_d, r) = doc(
            r#"<html><head><meta property="og:title" content=""><title>Real Title Goes Here</title></head><body></body></html>"#,
        );
        assert_eq!(get_article_metadata_title(&r), "Real Title Goes Here");
    }
}
