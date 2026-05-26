//! `metadata.rs` — title resolution + non-body metadata (HLD §7.1 / §7.6).
//!
//! **Stage 1a** pulled forward title resolution because `_grabArticle`'s
//! `_headerDuplicatesTitle` (`Readability.js:1105`) deletes headings via
//! `_textSimilarity(this._articleTitle, heading) > 0.75` — so the article
//! title **directly changes the scored body text**. The Stage-1a slice was
//! `_getArticleTitle` (`Readability.js:572-651`) + the title-only half of
//! `_getArticleMetadata` (`Readability.js:1803-1816`); JSON-LD was deferred
//! (`jsonld = {}` per `Readability.js:2736` when `_disableJSONLD = true`).
//!
//! **Stage 4** (this file's extension; HLD §7.6) ports the *rest* of
//! `_getArticleMetadata` (`Readability.js:1757-1863`) — byline, excerpt, site
//! name, published time — and the full `_getJSONLD`
//! (`Readability.js:1632-1747`, **including** the `@graph` resolution
//! (`:1674-1678`) that Stage 1a deferred). These fields are **NOT scored**
//! (HLD §2 score-invisible partition), so they cannot move corpus-level
//! Coverage/Precision; their inclusion is API-completeness ahead of the M5
//! the consumer shim (supervisor M-4) and is byte-additive — every previously-
//! `None` field stays `None` on every input that did not produce a hit
//! (faithful to JS `undefined`).
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
        // llvm-cov:branch-not-reachable: the enclosing `if` fired because
        // `title_separator().is_match(cur_title)` was true, and at this point
        // `cur_title == orig_title` (an unmodified clone), so `find_iter` on the
        // same pattern over `orig_title` always yields ≥1 match — the `else`
        // (None) side cannot occur (Readability.js:599-601 invariant).
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
            // llvm-cov:branch-not-reachable: this whole block is gated by the
            // `else if cur_title.contains(": ")` arm and `cur_title == orig_title`
            // here, so `orig_title` is guaranteed to contain a ':' — `rfind`
            // always returns Some (Readability.js:609-612 invariant).
            if let Some(pos) = orig_title.rfind(':') {
                cur_title = byte_substring(&orig_title, pos + ':'.len_utf8(), orig_title.len());
            }
            if word_count(&cur_title) < 3 {
                // curTitle = origTitle.substring(origTitle.indexOf(":") + 1)
                // llvm-cov:branch-not-reachable: same `contains(": ")` gate —
                // `orig_title` always contains ':', so `find` is always Some
                // (Readability.js:613-615 invariant).
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
    // llvm-cov:branch-not-reachable: every caller derives `start`/`end` from
    // `Regex::find`/`str::find`/`rfind`/`indexOf` indices on THIS exact `&str`,
    // which always land on UTF-8 char boundaries — so both `is_char_boundary`
    // checks are always true and the `else` char-walk fallback cannot fire
    // (documented invariant in this fn's own doc-comment).
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
/// and consumed by `_headerDuplicatesTitle` on the scored path. **Stage 4**
/// still calls this from the pre-grab pipeline (title alone is sufficient to
/// drive `_headerDuplicatesTitle` — the score-affecting hook), so this
/// title-only entry point is preserved. Stage 4's [`get_article_metadata`]
/// computes the same title plus the other (non-scored) metadata fields; the
/// titles ARE byte-identical by construction (the title precedence in
/// `Readability.js:1803-1816` is unchanged when `jsonld.title` is unset).
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

// ---------------------------------------------------------------------------
// Stage 4 (HLD §7.6) — full `_getArticleMetadata` + `_getJSONLD`.
// ---------------------------------------------------------------------------

/// `metadata = {...}` (`Readability.js:1757-1863` + JSON-LD merge),
/// **score-invisible** non-body metadata.
///
/// Fields:
/// * `title` — the same value `get_article_metadata_title` returns (the
///   title-only precedence; JSON-LD `headline`/`name` wins if present, exactly
///   as `Readability.js:1690-1713` decides).
/// * `byline` — author (`jsonld.byline || values["dc:creator"] || …`,
///   `Readability.js:1825-1831`).
/// * `excerpt` — description (`jsonld.excerpt || values["og:description"] ||
///   …`, `Readability.js:1834-1842`).
/// * `site_name` — publisher (`jsonld.siteName || values["og:site_name"]`,
///   `Readability.js:1845`).
/// * `published_time` — date (`jsonld.datePublished ||
///   values["article:published_time"] || values["parsely-pub-date"]`,
///   `Readability.js:1848-1852`).
///
/// All non-title fields default to `None` (JS `undefined`), faithfully — the
/// JS `metadata.byline = ... || ... || articleAuthor` may land on `undefined`
/// when every alternative is undefined.
///
/// Every string is `_unescapeHtmlEntities`-decoded
/// (`Readability.js:1856-1860`) faithfully via [`unescape_html_entities`].
#[derive(Debug, Default, Clone)]
pub struct Metadata {
    /// `metadata.title` — never empty after this function (final fallback is
    /// `_getArticleTitle()` per `Readability.js:1814-1816`).
    pub title: String,
    /// `metadata.byline` — `undefined` ⇒ `None`. Note the JS `parse()`
    /// (`Readability.js:2769`) falls back to `this._articleByline` (set by
    /// `_grabArticle` when it finds a `<address>` / `[rel=author]` etc. in
    /// the tree); that fallback happens in the caller (`lib.rs` /
    /// `Readability::parse`), not here.
    pub byline: Option<String>,
    /// `metadata.excerpt` — JS path `Readability.js:2759-2763` ALSO has a
    /// final fallback to the first `<p>` of `articleContent`. The caller
    /// (`Readability::parse`) applies that fallback because it has the
    /// articleContent in hand; this function returns only the metadata-only
    /// excerpt.
    pub excerpt: Option<String>,
    /// `metadata.siteName` — falls back to `this._articleSiteName` in JS
    /// `parse()` (`Readability.js:2776`); we keep the metadata-only value
    /// here.
    pub site_name: Option<String>,
    /// `metadata.publishedTime` — JS lets this be `null` (`:1852`); `None`
    /// here means "no metadata source set it" (faithfully equivalent: a
    /// `null` JS value flows to the return object's `publishedTime` as
    /// `null`).
    pub published_time: Option<String>,
}

/// Full `_getArticleMetadata(jsonld)` (`Readability.js:1757-1863`) — Stage 4
/// (HLD §7.6).
///
/// `jsonld` is the value returned by [`get_json_ld`]; pass an empty
/// [`JsonLd`] to faithfully reproduce the `_disableJSONLD = true` branch.
pub fn get_article_metadata(doc_root: &NodeRef, jsonld: &JsonLd) -> Metadata {
    let values = collect_meta_values(doc_root);

    // 1803-1812 metadata.title precedence (jsonld.title wins if present).
    let mut title = jsonld
        .title
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
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
            String::new()
        });
    if title.is_empty() {
        // 1814-1816 if (!metadata.title) metadata.title = this._getArticleTitle()
        title = get_article_title(doc_root);
    }

    // 1818-1822 articleAuthor — the article:author meta value, BUT only if
    // it does not look like a URL.
    let article_author = values
        .get("article:author")
        .filter(|s| !is_url_like(s))
        .cloned();

    // 1824-1831 metadata.byline precedence.
    let byline = jsonld
        .byline
        .clone()
        .or_else(|| values.get("dc:creator").cloned())
        .or_else(|| values.get("dcterm:creator").cloned())
        .or_else(|| values.get("author").cloned())
        .or_else(|| values.get("parsely-author").cloned())
        .or(article_author);

    // 1833-1842 metadata.excerpt precedence.
    let excerpt = jsonld
        .excerpt
        .clone()
        .or_else(|| values.get("dc:description").cloned())
        .or_else(|| values.get("dcterm:description").cloned())
        .or_else(|| values.get("og:description").cloned())
        .or_else(|| values.get("weibo:article:description").cloned())
        .or_else(|| values.get("weibo:webpage:description").cloned())
        .or_else(|| values.get("description").cloned())
        .or_else(|| values.get("twitter:description").cloned());

    // 1844-1845 metadata.siteName.
    let site_name = jsonld
        .site_name
        .clone()
        .or_else(|| values.get("og:site_name").cloned());

    // 1847-1852 metadata.publishedTime (defaults to null in JS; None here).
    let published_time = jsonld
        .date_published
        .clone()
        .or_else(|| values.get("article:published_time").cloned())
        .or_else(|| values.get("parsely-pub-date").cloned());

    // 1854-1860 entity-unescape every string.
    Metadata {
        title: unescape_html_entities(&title),
        byline: byline.map(|s| unescape_html_entities(&s)),
        excerpt: excerpt.map(|s| unescape_html_entities(&s)),
        site_name: site_name.map(|s| unescape_html_entities(&s)),
        published_time: published_time.map(|s| unescape_html_entities(&s)),
    }
}

/// The JSON-LD subset `_getJSONLD` (`Readability.js:1632-1747`) lifts off the
/// `<script type="application/ld+json">` payload.
///
/// All fields are `Option<String>` mirroring JS `undefined` exactly.
#[derive(Debug, Default, Clone)]
pub struct JsonLd {
    /// `metadata.title` — `:1690-1713`. Note JS sometimes chooses `name` vs
    /// `headline` based on `_textSimilarity` to the article title; that
    /// choice is made INSIDE [`get_json_ld`] because the JS path has
    /// `this._getArticleTitle()` available.
    pub title: Option<String>,
    /// `metadata.byline` — `:1714-1731`.
    pub byline: Option<String>,
    /// `metadata.excerpt` — `:1732-1734`.
    pub excerpt: Option<String>,
    /// `metadata.siteName` — `:1735-1737`.
    pub site_name: Option<String>,
    /// `metadata.datePublished` — `:1738-1740`.
    pub date_published: Option<String>,
}

/// `_getJSONLD(doc)` (`Readability.js:1632-1747`) — Stage 4 (HLD §7.6),
/// including the `@graph` resolution (`:1674-1678`) that Stage 1a deferred.
///
/// Walks every `<script type="application/ld+json">` in tree order; the
/// **first** that parses to a Schema.org Article-class object wins (JS uses
/// `if (!metadata)` — first hit, then later hits are ignored). Returns an
/// empty [`JsonLd`] if nothing qualifies (JS `metadata ? metadata : {}` —
/// `:1746`).
///
/// `JSON.parse` failures are silently swallowed (JS `catch (err)
/// this.log(err.message)` — `:1741-1743`); a single malformed JSON-LD does
/// not stop later scripts from being considered.
pub fn get_json_ld(doc_root: &NodeRef) -> JsonLd {
    // 1633 var scripts = this._getAllNodesWithTag(doc, ["script"]);
    let scripts = get_all_nodes_with_tag(doc_root, &["script"]);

    for script in &scripts {
        // 1639-1641 if (!metadata && type === "application/ld+json")
        if get_attribute(script, "type").as_deref() != Some("application/ld+json") {
            continue;
        }

        // 1644-1647 Strip CDATA markers, faithfully.
        let raw = text_content(script);
        let stripped = strip_cdata_markers(&raw);

        // 1648 JSON.parse — failure ⇒ catch ⇒ continue to next script.
        let Ok(parsed_val): serde_json::Result<serde_json::Value> = serde_json::from_str(&stripped)
        else {
            continue;
        };

        // 1650-1660 if Array.isArray: pick the first @type matching
        // jsonLdArticleTypes. If none, skip (`return` in JS, meaning continue
        // to next script — `forEachNode` callback).
        let parsed = match parsed_val {
            serde_json::Value::Array(arr) => {
                let Some(found) = arr.into_iter().find(|it| {
                    it.get("@type")
                        .and_then(|t| t.as_str())
                        .map(json_ld_article_type_matches)
                        .unwrap_or(false)
                }) else {
                    continue;
                };
                found
            }
            other => other,
        };

        // 1662-1672 @context must match schema.org (string OR object with
        // @vocab string), else skip.
        let context_matches = match parsed.get("@context") {
            Some(serde_json::Value::String(s)) => schema_dot_org_matches(s),
            Some(serde_json::Value::Object(obj)) => obj
                .get("@vocab")
                .and_then(|v| v.as_str())
                .map(schema_dot_org_matches)
                .unwrap_or(false),
            _ => false,
        };
        if !context_matches {
            continue;
        }

        // 1674-1678 `@graph` resolution: if no top-level @type but a @graph
        // array, scan the graph for the first Article-type entry.
        let parsed = if parsed.get("@type").is_none() {
            if let Some(graph) = parsed.get("@graph").and_then(|g| g.as_array()) {
                match graph.iter().find(|it| {
                    it.get("@type")
                        .map(json_ld_at_type_matches_loose)
                        .unwrap_or(false)
                }) {
                    Some(found) => found.clone(),
                    // 1678 If find returns undefined ⇒ `parsed` becomes
                    // undefined ⇒ the next `if (!parsed || …)` returns. We
                    // continue to next script (faithful to forEachNode's
                    // `return`).
                    None => continue,
                }
            } else {
                parsed
            }
        } else {
            parsed
        };

        // 1680-1686 if (!parsed || !parsed["@type"] || !parsed["@type"].match
        //   (jsonLdArticleTypes)) return;
        let at_type_ok = parsed
            .get("@type")
            .and_then(|t| t.as_str())
            .map(json_ld_article_type_matches)
            .unwrap_or(false);
        if !at_type_ok {
            continue;
        }

        // 1688 metadata = {} — start a fresh JsonLd to populate.
        let mut md = JsonLd::default();

        // 1690-1713 title resolution (name vs headline, with similarity
        // tie-break against _getArticleTitle()).
        let parsed_name = parsed.get("name").and_then(|v| v.as_str());
        let parsed_headline = parsed.get("headline").and_then(|v| v.as_str());
        match (parsed_name, parsed_headline) {
            // both present AND differ ⇒ similarity tie-break (`:1690-1708`).
            (Some(n), Some(h)) if n != h => {
                // `_getArticleTitle` is what JS calls; we already have a
                // ported version, so use it.
                let title = get_article_title(doc_root);
                let name_matches = crate::readability::scoring::text_similarity(n, &title) > 0.75;
                let headline_matches =
                    crate::readability::scoring::text_similarity(h, &title) > 0.75;
                if headline_matches && !name_matches {
                    md.title = Some(h.to_string());
                } else {
                    md.title = Some(n.to_string());
                }
            }
            // only `name`.
            (Some(n), _) => md.title = Some(js_trim(n).to_string()),
            // only `headline`.
            (_, Some(h)) => md.title = Some(js_trim(h).to_string()),
            _ => {}
        }

        // 1714-1731 author/byline.
        if let Some(author) = parsed.get("author") {
            if let Some(name) = author.get("name").and_then(|v| v.as_str()) {
                md.byline = Some(js_trim(name).to_string());
            } else if let Some(arr) = author.as_array()
                && let Some(first) = arr.first()
                && first.get("name").and_then(|v| v.as_str()).is_some()
            {
                // .filter(author => typeof author.name === "string")
                // .map(author => author.name.trim()).join(", ")
                let names: Vec<String> = arr
                    .iter()
                    .filter_map(|a| a.get("name").and_then(|v| v.as_str()))
                    .map(|n| js_trim(n).to_string())
                    .collect();
                md.byline = Some(names.join(", "));
            }
        }

        // 1732-1734 metadata.excerpt = parsed.description.trim().
        if let Some(desc) = parsed.get("description").and_then(|v| v.as_str()) {
            md.excerpt = Some(js_trim(desc).to_string());
        }

        // 1735-1737 metadata.siteName = parsed.publisher.name.trim().
        if let Some(name) = parsed
            .get("publisher")
            .and_then(|p| p.get("name"))
            .and_then(|v| v.as_str())
        {
            md.site_name = Some(js_trim(name).to_string());
        }

        // 1738-1740 metadata.datePublished = parsed.datePublished.trim().
        if let Some(dp) = parsed.get("datePublished").and_then(|v| v.as_str()) {
            md.date_published = Some(js_trim(dp).to_string());
        }

        // 1746 return metadata ? metadata : {} — first script with a metadata
        // object wins.
        return md;
    }

    // No qualifying JSON-LD ⇒ {}.
    JsonLd::default()
}

/// `/^\s*<!\[CDATA\[|\]\]>\s*$/g` (`Readability.js:1645`) — strip a leading
/// `<![CDATA[` (after JS-whitespace) and a trailing `]]>` (before JS-
/// whitespace). The `/g` form would replace **every** match anywhere; in
/// practice the only sane JSON-LD wrapping is one leading + one trailing
/// marker, and the JS regex is anchored at both ends per occurrence (the
/// alternation matches either anchor).
fn strip_cdata_markers(s: &str) -> String {
    static R: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = R.get_or_init(|| {
        regex::Regex::new(&format!(
            "(?:^[{cls}]*<!\\[CDATA\\[)|(?:\\]\\]>[{cls}]*$)",
            cls = regexps::JS_SPACE_CLASS
        ))
        .expect("cdata regex")
    });
    re.replace_all(s, "").into_owned()
}

/// `REGEXPS.jsonLdArticleTypes` (`Readability.js:168-169`):
/// `/^Article|AdvertiserContentArticle|…|APIReference$/` — note the JS regex
/// **only anchors the first alternative `Article` and the last alternative
/// `APIReference`**; every middle alternative is unanchored, so they match
/// anywhere in the string. This is a JS regex quirk (precedence: `^A|B|C$`
/// parses as `(^A)|B|(C$)`), and Readability sees it as faithfully matching
/// any of the listed types either anchored or as substrings. We replicate
/// EXACTLY: try each alternative, anchored or substring per the JS parse.
fn json_ld_article_type_matches(s: &str) -> bool {
    // The first alternative `Article` is anchored at start; the last
    // `APIReference` is anchored at end; everything in between is unanchored.
    // We reproduce that exactly to be JS-faithful.
    if s.starts_with("Article") {
        return true;
    }
    if s.ends_with("APIReference") {
        return true;
    }
    for t in [
        "AdvertiserContentArticle",
        "NewsArticle",
        "AnalysisNewsArticle",
        "AskPublicNewsArticle",
        "BackgroundNewsArticle",
        "OpinionNewsArticle",
        "ReportageNewsArticle",
        "ReviewNewsArticle",
        "Report",
        "SatiricalArticle",
        "ScholarlyArticle",
        "MedicalScholarlyArticle",
        "SocialMediaPosting",
        "BlogPosting",
        "LiveBlogPosting",
        "DiscussionForumPosting",
        "TechArticle",
    ] {
        if s.contains(t) {
            return true;
        }
    }
    false
}

/// The `@graph` filter at `Readability.js:1676` uses `(it["@type"] || "")
/// .match(...)`. `it["@type"]` may be a string OR an array of strings — JS
/// coerces with `||` (only "" string is falsy among those shapes, but a
/// missing key is `undefined` so `|| ""`). If it is an array, `Array.match`
/// is undefined and would throw — Readability silently accepts that as a
/// no-match. Faithful: only treat as a match when `@type` is a string that
/// passes [`json_ld_article_type_matches`].
fn json_ld_at_type_matches_loose(v: &serde_json::Value) -> bool {
    v.as_str().is_some_and(json_ld_article_type_matches)
}

/// `Readability.js:1662` `schemaDotOrgRegex = /^https?\:\/\/schema\.org\/?$/`.
fn schema_dot_org_matches(s: &str) -> bool {
    // Tiny pattern; no regex needed.
    let s = s.strip_suffix('/').unwrap_or(s);
    s == "http://schema.org" || s == "https://schema.org"
}

/// `_isUrl(str)` (`Readability.js:441-448`): `try new URL(str); return true;
/// catch return false;`.
///
/// JS `new URL(str)` is the **WHATWG URL parser**; absolute URLs only (no
/// base). Faithful predicate: accept the very narrow shape Readability cares
/// about — `<scheme>:<scheme-specific-part>` where scheme starts with an
/// ASCII letter then `[A-Za-z0-9+\-.]*`. The only context that calls this is
/// `metadata.byline`'s `article:author` ladder (`Readability.js:1818-1822`):
/// if the meta value looks like a URL, skip it (a profile URL is not a name).
/// A tighter parser is unnecessary — a benign false-negative would just keep
/// a value JS would have rejected; a false-positive would only happen on a
/// string with a scheme prefix, which is exactly what the JS check rejects.
fn is_url_like(s: &str) -> bool {
    // WHATWG URL parser requires `<scheme>:<rest>` with the scheme matching
    // `[A-Za-z][A-Za-z0-9+\-.]*`. That is what `new URL(str)` accepts as
    // absolute; relative URLs without a base throw. So `_isUrl` is roughly
    // "starts with a scheme:".
    let bytes = s.as_bytes();
    let Some(colon) = bytes.iter().position(|&b| b == b':') else {
        return false;
    };
    if colon == 0 {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    bytes[1..colon]
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
}

/// `_unescapeHtmlEntities(str)` (`Readability.js:1605-1625`).
///
/// Faithful decoding for the **named** entities `&quot;`/`&amp;`/`&apos;`/
/// `&lt;`/`&gt;` (Readability's HTML_ESCAPE_MAP — `:267-273`) plus numeric
/// `&#NN;` / `&#xHH;`. Code points 0, > 0x10FFFF, or in the surrogate range
/// 0xD800..=0xDFFF are replaced by U+FFFD (`:1619-1620`). Returns the input
/// unchanged when it is empty (JS `if (!str) return str;` — `:1606`).
///
/// Operates over `char` iteration via `.chars()` so multi-byte UTF-8 is
/// preserved verbatim outside entity sequences.
pub fn unescape_html_entities(s: &str) -> String {
    if s.is_empty() {
        return s.to_string();
    }

    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;

    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Step by one UTF-8 char. `s.as_bytes()` is valid UTF-8 because
            // `s: &str`, so the leading byte tells us the char length.
            let len = utf8_char_len(bytes[i]);
            out.push_str(&s[i..i + len]);
            i += len;
            continue;
        }

        // Try named entities (`Readability.js:1612` — the HTML_ESCAPE_MAP).
        if let Some(hit) = try_named_entity(&bytes[i..]) {
            out.push(hit.ch);
            i += hit.len;
            continue;
        }

        // Numeric entity `&#NN;` / `&#xHH;` (`Readability.js:1615-1623`).
        if let Some(hit) = try_numeric_entity(&bytes[i..]) {
            out.push(hit.ch);
            i += hit.len;
            continue;
        }

        // Not a recognised entity — copy `&` verbatim and continue.
        out.push('&');
        i += 1;
    }
    out
}

struct EntityHit {
    ch: char,
    len: usize,
}

fn try_named_entity(b: &[u8]) -> Option<EntityHit> {
    for (name, ch) in [
        ("&quot;", '"'),
        ("&amp;", '&'),
        ("&apos;", '\''),
        ("&lt;", '<'),
        ("&gt;", '>'),
    ] {
        if b.starts_with(name.as_bytes()) {
            return Some(EntityHit {
                ch,
                len: name.len(),
            });
        }
    }
    None
}

fn try_numeric_entity(b: &[u8]) -> Option<EntityHit> {
    if b.len() < 4 || b[0] != b'&' || b[1] != b'#' {
        return None;
    }
    let (radix, digit_start) = if b[2] == b'x' || b[2] == b'X' {
        (16u32, 3usize)
    } else {
        (10u32, 2usize)
    };

    // Consume the digit run.
    let mut p = digit_start;
    let is_digit = |c: u8| match radix {
        16 => c.is_ascii_hexdigit(),
        _ => c.is_ascii_digit(),
    };
    while p < b.len() && is_digit(b[p]) {
        p += 1;
    }
    if p == digit_start || p >= b.len() || b[p] != b';' {
        return None;
    }

    // Parse the digit prefix.
    let digits = std::str::from_utf8(&b[digit_start..p]).ok()?;
    let mut num = u32::from_str_radix(digits, radix).unwrap_or(0xFFFD);

    // `:1619-1620` invalid code points -> U+FFFD.
    if num == 0 || num > 0x10FFFF || (0xD800..=0xDFFF).contains(&num) {
        num = 0xFFFD;
    }
    let ch = char::from_u32(num).unwrap_or('\u{FFFD}');
    Some(EntityHit {
        ch,
        len: p + 1, // include the trailing ';'
    })
}

/// UTF-8 character byte-length from the leading byte. Assumes `b` is the
/// first byte of a valid UTF-8 code point (true when iterating over the
/// bytes of a `&str` at code-point boundaries).
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b < 0xC0 {
        // Continuation byte — should not appear at a code-point boundary in
        // valid UTF-8; defensively step by 1.
        // llvm-cov:branch-not-reachable: `b` is always the LEADING byte of a
        // code point (the caller only advances by full `utf8_char_len` strides
        // over a valid `&str`), so a continuation byte (0x80..=0xBF) never
        // reaches this arm.
        1
    } else if b < 0xE0 {
        2
    } else if b < 0xF0 {
        3
    } else {
        4
    }
}

/// HTML `<html lang="...">` for `_articleLang` (`Readability.js:1060-1062`).
/// `None` when missing.
pub fn html_lang(doc_root: &NodeRef) -> Option<String> {
    let html = get_elements_by_tag_name(doc_root, "html");
    html.first().and_then(|h| get_attribute(h, "lang"))
}

/// `<link rel="canonical" href=...>` href, if any (the canonical URL
/// surfaced to consumers — not a Readability-spec field per se but an
/// `Extracted.canonical_url` slot that was always declared and never
/// populated; Stage 4 finally fills it from the obvious metadata source).
///
/// **NOT a `Readability.js` line cite** — this is `Extracted.canonical_url`
/// (the crate's API), not Readability metadata. Readability does not surface
/// a canonical URL; the crate's brief declares the field (and `language`)
/// as best-effort metadata the library decoded itself.
pub fn canonical_url(doc_root: &NodeRef) -> Option<String> {
    let links = get_elements_by_tag_name(doc_root, "link");
    for link in &links {
        // Match `<link rel="canonical">` (case-insensitive `rel`).
        if let Some(rel) = get_attribute(link, "rel")
            && rel.eq_ignore_ascii_case("canonical")
        {
            return get_attribute(link, "href").filter(|s| !s.is_empty());
        }
    }
    None
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
#[cfg_attr(coverage_nightly, coverage(off))]
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

    // ====== Stage 4 tests (HLD §7.6 — full `_getArticleMetadata` + JSON-LD).
    // Expected values hand-derived from `Readability.js:1632-1863`.

    // ---- full `_getArticleMetadata` (Readability.js:1757-1863) ----

    #[test]
    fn metadata_full_collects_og_byline_excerpt_site_name_published_time() {
        // <meta property> matches `propertyPattern`; values map keys are the
        // matched substring lowercased and stripped of JS-`\s`. Faithful keys:
        //   "article:author", "og:description", "og:site_name",
        //   "article:published_time"
        let (_d, r) = doc(r#"<html><head>
                <meta property="og:title" content="The Headline">
                <meta property="article:author" content="Jane Doe">
                <meta property="og:description" content="A short excerpt.">
                <meta property="og:site_name" content="Example News">
                <meta property="article:published_time" content="2024-01-02T03:04:05Z">
                <title>Doc</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.title, "The Headline");
        assert_eq!(md.byline.as_deref(), Some("Jane Doe"));
        assert_eq!(md.excerpt.as_deref(), Some("A short excerpt."));
        assert_eq!(md.site_name.as_deref(), Some("Example News"));
        assert_eq!(md.published_time.as_deref(), Some("2024-01-02T03:04:05Z"));
    }

    #[test]
    fn metadata_full_byline_article_author_url_is_rejected() {
        // `Readability.js:1818-1822`: articleAuthor only set when article:author
        // is NOT a URL. A URL-shaped article:author is skipped and the byline
        // ladder falls through to the next alternative; with no others present
        // the byline ends up `undefined` (None).
        let (_d, r) = doc(r#"<html><head>
                <meta property="article:author" content="https://example.com/jane">
                <title>X</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &JsonLd::default());
        assert!(md.byline.is_none(), "got {:?}", md.byline);
    }

    #[test]
    fn metadata_full_byline_dc_creator_precedence() {
        // Readability.js:1825-1831: byline = dc:creator || dcterm:creator ||
        // author || parsely-author || articleAuthor.
        let (_d, r) = doc(r#"<html><head>
                <meta name="dc.creator" content="DC Creator">
                <meta name="author" content="Plain Author">
                <meta property="article:author" content="Article Author">
                <title>X</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &JsonLd::default());
        // dc:creator wins (`.` → `:` per `:1796`).
        assert_eq!(md.byline.as_deref(), Some("DC Creator"));
    }

    #[test]
    fn metadata_full_jsonld_byline_beats_meta() {
        // JsonLd.byline wins over every <meta> alternative.
        let jsonld = JsonLd {
            byline: Some("JSON Author".to_string()),
            ..JsonLd::default()
        };
        let (_d, r) = doc(r#"<html><head>
                <meta name="author" content="Plain Author">
                <title>X</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &jsonld);
        assert_eq!(md.byline.as_deref(), Some("JSON Author"));
    }

    #[test]
    fn metadata_full_excerpt_precedence_og_description_beats_description() {
        // og:description should win over plain description.
        let (_d, r) = doc(r#"<html><head>
                <meta name="description" content="Plain description">
                <meta property="og:description" content="OG description wins">
                <title>X</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &JsonLd::default());
        // dc:description / dcterm:description absent; og:description wins.
        assert_eq!(md.excerpt.as_deref(), Some("OG description wins"));
    }

    #[test]
    fn metadata_full_unescapes_html_entities_on_strings() {
        // Readability.js:1856-1860 _unescapeHtmlEntities on every metadata
        // string field.
        let (_d, r) = doc(r#"<html><head>
                <meta property="og:title" content="Tom &amp; Jerry">
                <meta name="author" content="A &lt;B&gt;">
                <meta property="og:description" content="A &quot;quote&quot;">
                <meta property="og:site_name" content="Foo &#38; Bar">
                <title>X</title>
               </head><body></body></html>"#);
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.title, "Tom & Jerry");
        assert_eq!(md.byline.as_deref(), Some("A <B>"));
        assert_eq!(md.excerpt.as_deref(), Some(r#"A "quote""#));
        assert_eq!(md.site_name.as_deref(), Some("Foo & Bar"));
    }

    #[test]
    fn metadata_full_default_path_returns_only_title_when_no_meta() {
        // No <meta>; title falls back to _getArticleTitle. Other fields stay
        // None — faithful "undefined".
        let (_d, r) = doc(
            "<html><head><title>A Plain Doc Heading Word Five</title></head>\
             <body></body></html>",
        );
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.title, "A Plain Doc Heading Word Five");
        assert!(md.byline.is_none());
        assert!(md.excerpt.is_none());
        assert!(md.site_name.is_none());
        assert!(md.published_time.is_none());
    }

    // ---- `_getJSONLD` (Readability.js:1632-1747) ----

    #[test]
    fn jsonld_picks_first_article_object() {
        // First script that parses to an Article-class object wins; later
        // scripts are ignored.
        let html = r#"<html><head>
            <script type="application/ld+json">
                {"@context":"https://schema.org","@type":"Article",
                 "name":"First","author":{"name":"Alice"},
                 "description":"desc",
                 "publisher":{"name":"Pub"},
                 "datePublished":"2024-01-01"}
            </script>
            <script type="application/ld+json">
                {"@context":"https://schema.org","@type":"Article",
                 "name":"Second"}
            </script>
            </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("First"));
        assert_eq!(ld.byline.as_deref(), Some("Alice"));
        assert_eq!(ld.excerpt.as_deref(), Some("desc"));
        assert_eq!(ld.site_name.as_deref(), Some("Pub"));
        assert_eq!(ld.date_published.as_deref(), Some("2024-01-01"));
    }

    #[test]
    fn jsonld_at_graph_resolution_finds_article_in_graph() {
        // The @graph case: top-level has no @type but a @graph array — find
        // the first Article in the graph (`Readability.js:1674-1678`).
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org",
             "@graph":[
                {"@type":"WebSite","name":"site"},
                {"@type":"NewsArticle","name":"From Graph",
                 "author":{"name":"Bob"}}
             ]}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("From Graph"));
        assert_eq!(ld.byline.as_deref(), Some("Bob"));
    }

    #[test]
    fn jsonld_at_graph_no_article_skips_to_next_script() {
        // First script's @graph has no Article-type entry — JS would `return`
        // from the forEachNode callback (continue to next script). Next script
        // provides an Article.
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@graph":[{"@type":"WebSite","name":"site"}]}
            </script>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"BlogPosting","name":"Recovered"}
            </script>
            </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Recovered"));
    }

    #[test]
    fn jsonld_context_object_with_at_vocab_schema_org() {
        // @context can be an object with @vocab: schema.org — accepted per
        // Readability.js:1666-1668.
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":{"@vocab":"https://schema.org/"},"@type":"Article",
             "name":"Vocab-Ctx Article"}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Vocab-Ctx Article"));
    }

    #[test]
    fn jsonld_array_root_finds_article_inside() {
        // Top-level array — Readability.js:1650-1660. find() picks the first
        // Article-class object.
        let html = r#"<html><head>
            <script type="application/ld+json">
            [{"@type":"WebSite","@context":"https://schema.org"},
             {"@context":"https://schema.org","@type":"Article","name":"In Array"}]
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("In Array"));
    }

    #[test]
    fn jsonld_non_schema_org_context_skipped() {
        // Readability.js:1670-1672: non-schema.org context => return (skip).
        // No qualifying JSON-LD => empty JsonLd.
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"http://example.com/ns","@type":"Article","name":"Should be ignored"}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none(), "got {:?}", ld.title);
    }

    #[test]
    fn jsonld_malformed_json_swallowed_does_not_block_subsequent() {
        // Readability.js:1741-1743 catches errors silently. Malformed JSON-LD
        // in one script does not prevent a valid one later from winning.
        let html = r#"<html><head>
            <script type="application/ld+json">{not valid json</script>
            <script type="application/ld+json">
              {"@context":"https://schema.org","@type":"Article","name":"Recovered"}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Recovered"));
    }

    #[test]
    fn jsonld_strips_cdata_markers() {
        // Readability.js:1644-1647 — strip `<![CDATA[` / `]]>` markers.
        let html = "<html><head><script type=\"application/ld+json\"><![CDATA[\n\
            {\"@context\":\"https://schema.org\",\"@type\":\"Article\",\"name\":\"CDATAd\"}\n\
            ]]></script></head><body></body></html>";
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("CDATAd"));
    }

    #[test]
    fn jsonld_authors_array_joined_with_comma() {
        // Readability.js:1717-1729: author array of {name} -> "n1, n2, n3".
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "author":[{"name":"Alice"},{"name":"Bob"},{"name":"Carol"}],
             "name":"Co-Authored"}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.byline.as_deref(), Some("Alice, Bob, Carol"));
    }

    #[test]
    fn jsonld_name_and_headline_differ_similarity_picks_one() {
        // Readability.js:1690-1708: when both present and differ, do
        // similarity tie-break vs _getArticleTitle. doc.title is "Real
        // Headline Goes Long Enough" (5 words, >4). With the headline JSON-LD
        // matching it closely and name differing, we expect headline wins.
        let html = r#"<html><head>
            <title>Real Headline Goes Long Enough</title>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"Site Brand Name Here Now",
             "headline":"Real Headline Goes Long Enough"}
            </script></head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        // Both present, differ. Similarity(name, title) low; similarity
        // (headline, title) 1.0 > 0.75; so headlineMatches && !nameMatches
        // => metadata.title = parsed.headline (`:1704-1705`).
        assert_eq!(
            ld.title.as_deref(),
            Some("Real Headline Goes Long Enough"),
            "got {:?}",
            ld.title
        );
    }

    #[test]
    fn jsonld_empty_or_no_script_returns_empty() {
        let (_d, r) = doc("<html><head></head><body></body></html>");
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
        assert!(ld.byline.is_none());
    }

    // ---- `_unescapeHtmlEntities` (Readability.js:1605-1625) ----

    #[test]
    fn unescape_named_entities() {
        assert_eq!(unescape_html_entities("a &amp; b"), "a & b");
        assert_eq!(unescape_html_entities("&lt;tag&gt;"), "<tag>");
        assert_eq!(unescape_html_entities("&quot;hi&quot;"), r#""hi""#);
        assert_eq!(unescape_html_entities("&apos;"), "'");
    }

    #[test]
    fn unescape_numeric_decimal_and_hex() {
        assert_eq!(unescape_html_entities("&#65;"), "A"); // 65 = 'A'
        assert_eq!(unescape_html_entities("&#x41;"), "A");
        assert_eq!(unescape_html_entities("&#x1F600;"), "\u{1F600}"); // 😀
    }

    #[test]
    fn unescape_invalid_codepoints_replaced_by_fffd() {
        // 0, surrogate, and >0x10FFFF map to U+FFFD per :1619-1620.
        assert_eq!(unescape_html_entities("&#0;"), "\u{FFFD}");
        assert_eq!(unescape_html_entities("&#xD800;"), "\u{FFFD}");
        assert_eq!(unescape_html_entities("&#x110000;"), "\u{FFFD}");
    }

    #[test]
    fn unescape_lone_ampersand_left_alone() {
        // No recognised name/numeric form -> verbatim '&'.
        assert_eq!(unescape_html_entities("a & b"), "a & b");
        assert_eq!(unescape_html_entities("&notreal;"), "&notreal;");
    }

    #[test]
    fn unescape_empty_input_unchanged() {
        assert_eq!(unescape_html_entities(""), "");
    }

    #[test]
    fn unescape_preserves_utf8_multibyte() {
        // Naïve byte iteration would corrupt multibyte UTF-8. Test passes
        // through verbatim.
        assert_eq!(unescape_html_entities("héllo &amp; wörld"), "héllo & wörld");
        assert_eq!(
            unescape_html_entities("中文 &lt;a&gt; 中文"),
            "中文 <a> 中文"
        );
    }

    // ---- `_isUrl` (Readability.js:441-448) ----

    #[test]
    fn is_url_like_recognises_schemes() {
        assert!(is_url_like("https://example.com"));
        assert!(is_url_like("http://example.com"));
        assert!(is_url_like("mailto:a@b"));
        // No scheme -> false.
        assert!(!is_url_like("just a name"));
        assert!(!is_url_like("Jane Doe"));
        // Leading non-letter -> false (URL scheme must start with letter).
        assert!(!is_url_like("1http://x"));
    }

    // ---- canonical_url ----

    #[test]
    fn canonical_url_picks_link_rel_canonical_href() {
        let (_d, r) = doc(r#"<html><head>
                <link rel="canonical" href="https://example.com/canon">
               </head><body></body></html>"#);
        assert_eq!(
            canonical_url(&r).as_deref(),
            Some("https://example.com/canon")
        );
    }

    #[test]
    fn canonical_url_none_when_absent_or_empty() {
        let (_d, r) = doc("<html><head></head><body></body></html>");
        assert!(canonical_url(&r).is_none());

        let (_d, r) =
            doc(r#"<html><head><link rel="canonical" href=""></head><body></body></html>"#);
        assert!(canonical_url(&r).is_none());
    }

    // ---- html_lang ----

    #[test]
    fn html_lang_returns_html_element_lang_attr() {
        // Production calls `html_lang(&doc.document())` (the Document, not
        // the <html> element — see `Readability::parse`); `doc()` returns
        // `root_element()` = the <html> itself, so a descendant search for
        // "html" returns []. Use `dom.document()` for the production-shape
        // root in this test.
        let dom = Dom::parse(r#"<html lang="en-GB"><head></head><body></body></html>"#);
        assert_eq!(html_lang(&dom.document()).as_deref(), Some("en-GB"));
    }

    #[test]
    fn html_lang_none_when_attr_absent() {
        let dom = Dom::parse("<html><head></head><body></body></html>");
        assert!(html_lang(&dom.document()).is_none());
    }

    // ====== Stage 2: JSON-LD shape catalog for `get_json_ld` ===============
    //
    // Tests below drive the missed branches inside `get_json_ld`
    // (`Readability.js:1632-1747`). Each pins the JS-source-line contract
    // it covers.

    /// `get_json_ld` — a script element with `type != application/ld+json`
    /// is skipped (`Readability.js:1639-1641`).
    /// rationale: pins the `continue` arm on the type-attribute filter.
    #[test]
    fn jsonld_skips_script_with_wrong_type() {
        let html = r#"<html><head>
            <script type="text/javascript">
            {"@context":"https://schema.org","@type":"Article","name":"Ignored"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none(), "got {:?}", ld.title);
    }

    /// `get_json_ld` — a script with no `type` attribute is also skipped.
    /// rationale: pins the `None` comparison of `get_attribute("type")`.
    #[test]
    fn jsonld_skips_script_with_no_type_attribute() {
        let html = r#"<html><head>
            <script>
            {"@context":"https://schema.org","@type":"Article","name":"Ignored"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — top-level array with NO Article-type entry → the
    /// `find` returns None, we continue to the next script
    /// (`Readability.js:1650-1660`).
    /// rationale: pins the `None => continue` arm of the array find.
    #[test]
    fn jsonld_array_with_no_article_entry_falls_through() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            [{"@type":"WebSite","name":"Just a site"},
             {"@type":"Organization","name":"Just an org"}]
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
        assert!(ld.byline.is_none());
    }

    /// `get_json_ld` — array entry's `@type` is NOT a string (e.g. an
    /// array of strings) — `as_str()` returns None, `unwrap_or(false)`
    /// short-circuits → entry skipped.
    /// rationale: pins the `as_str().is_some_and(...)` shape on array
    /// entries.
    #[test]
    fn jsonld_array_entry_with_non_string_type_skipped() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            [{"@type":["NewsArticle","BlogPosting"],"name":"Multi-Type"}]
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        // JS would have `Array.prototype.match` undefined → no match.
        // Faithful: no qualifying article in the array → continue to next
        // script (none here) → empty JsonLd.
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@context` is an Object WITHOUT a `@vocab` key →
    /// fails the inner `obj.get("@vocab")` lookup → context_matches false.
    /// rationale: pins the `unwrap_or(false)` half of the Object match.
    #[test]
    fn jsonld_context_object_no_vocab_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":{"locale":"en-US"},
             "@type":"Article","name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@context` object with `@vocab` that is NOT
    /// schema.org → also rejected.
    /// rationale: pins the negative side of `schema_dot_org_matches` on
    /// the vocab path.
    #[test]
    fn jsonld_context_object_vocab_non_schema_org_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":{"@vocab":"https://example.com/ns"},
             "@type":"Article","name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@context` object whose `@vocab` is non-string
    /// (number) → `as_str()` returns None → unwrap_or(false).
    /// rationale: pins the `and_then(as_str)` failure path.
    #[test]
    fn jsonld_context_object_vocab_non_string_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":{"@vocab":42},
             "@type":"Article","name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@context` of unsupported type (Number) → outer
    /// `_ => false` arm.
    /// rationale: pins the catch-all of the context match.
    #[test]
    fn jsonld_context_as_number_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":42,"@type":"Article","name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@context` absent → outer `_ => false` arm.
    /// rationale: pins the no-@context guard.
    #[test]
    fn jsonld_missing_context_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@type":"Article","name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@type` present at top-level but NOT an article
    /// type → `at_type_ok` false → continue (`Readability.js:1680-1686`).
    /// rationale: pins the type-mismatch reject.
    #[test]
    fn jsonld_non_article_top_type_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"WebSite","name":"Site"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — top-level `@type` is non-string (e.g. array) → the
    /// `as_str()` returns None → continue.
    /// rationale: pins the post-@graph `at_type_ok` guard.
    #[test]
    fn jsonld_top_type_as_array_rejected() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org",
             "@type":["NewsArticle","BlogPosting"],
             "name":"Should Skip"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `name` only (no `headline`) populates title via the
    /// `(Some(n), _)` arm (`Readability.js:1709-1710`).
    /// rationale: pins the name-only path.
    #[test]
    fn jsonld_title_from_name_only() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"Name Only"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Name Only"));
    }

    /// `get_json_ld` — `headline` only (no `name`) populates title via the
    /// `(_, Some(h))` arm.
    /// rationale: pins the headline-only path.
    #[test]
    fn jsonld_title_from_headline_only() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "headline":"Headline Only"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Headline Only"));
    }

    /// `get_json_ld` — `name` and `headline` identical → falls through to
    /// the `(Some(n), _)` arm (the `n != h` guard fails)
    /// (`Readability.js:1690`).
    /// rationale: pins the `if n != h` short-circuit.
    #[test]
    fn jsonld_title_when_name_equals_headline_uses_name_arm() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"Same Title", "headline":"Same Title"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Same Title"));
    }

    /// `get_json_ld` — name & headline differ, but neither matches doc
    /// title (similarity ≤ 0.75) → the `else` arm chooses `name`
    /// (`Readability.js:1707-1708`).
    /// rationale: pins the `name` fallback when similarity rule doesn't
    /// elect `headline`.
    #[test]
    fn jsonld_title_diff_neither_matches_falls_back_to_name() {
        let html = r#"<html><head>
            <title>Completely Unrelated Document Heading</title>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"Brand Site Header XYZ",
             "headline":"Something Else Entirely Yes"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        // Neither name nor headline is similar enough to the doc title;
        // headlineMatches && !nameMatches = false; else → name wins.
        assert_eq!(ld.title.as_deref(), Some("Brand Site Header XYZ"));
    }

    /// `get_json_ld` — neither `name` nor `headline` present → title
    /// remains None.
    /// rationale: pins the `_ => {}` arm of the title resolution match.
    #[test]
    fn jsonld_title_missing_when_neither_name_nor_headline() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "description":"Has only description"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
        assert_eq!(ld.excerpt.as_deref(), Some("Has only description"));
    }

    /// `get_json_ld` — `author` is an object with no `name` field, also
    /// not an array → the entire byline assignment is skipped.
    /// rationale: pins the `if let Some(name)` failure with non-array
    /// author.
    #[test]
    fn jsonld_author_object_no_name_field_yields_no_byline() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "author":{"url":"https://x"}}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.byline.is_none());
    }

    /// `get_json_ld` — `author` as array where first element has no
    /// `name` → the `first.get("name").as_str().is_some()` guard fails →
    /// no byline.
    /// rationale: pins the `first has no name` short-circuit.
    #[test]
    fn jsonld_author_array_first_has_no_name_yields_no_byline() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x",
             "author":[{"url":"https://x"},{"name":"Late Arrival"}]}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        // First-element guard fails → entire array path is skipped.
        assert!(ld.byline.is_none());
    }

    /// `get_json_ld` — empty author array → `arr.first()` is None → no
    /// byline.
    /// rationale: pins the empty-array guard.
    #[test]
    fn jsonld_author_empty_array_yields_no_byline() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "author":[]}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.byline.is_none());
    }

    /// `get_json_ld` — `author.name` present as STRING → byline set.
    /// rationale: pins the happy path of the byline.name arm.
    #[test]
    fn jsonld_author_object_with_name_string() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "author":{"name":"Solo Writer"}}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.byline.as_deref(), Some("Solo Writer"));
    }

    /// `get_json_ld` — `publisher` without `.name` → no site_name.
    /// rationale: pins the `and_then(p.get("name"))` failure path.
    #[test]
    fn jsonld_publisher_without_name_yields_no_site_name() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "publisher":{"url":"https://x"}}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.site_name.is_none());
    }

    /// `get_json_ld` — `datePublished` populates the slot.
    /// rationale: pins the date_published assignment.
    #[test]
    fn jsonld_date_published_populates_slot() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "datePublished":"2024-12-01"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.date_published.as_deref(), Some("2024-12-01"));
    }

    /// `get_json_ld` — `description` populates the excerpt slot.
    /// rationale: pins the excerpt assignment branch.
    #[test]
    fn jsonld_description_populates_excerpt() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "name":"x", "description":"A short blurb."}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.excerpt.as_deref(), Some("A short blurb."));
    }

    /// `get_json_ld` — `@graph` field present but is NOT an array (here:
    /// an Object) AND top-level `@type` is missing → JS faithful path:
    /// `parsed.get("@graph").and_then(|g| g.as_array())` returns None →
    /// the `else { parsed }` branch keeps `parsed` (which lacks @type), so
    /// the subsequent `at_type_ok` check fails → continue.
    /// rationale: pins the `@graph` non-array shape.
    #[test]
    fn jsonld_graph_as_non_array_with_no_at_type_skipped() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org",
             "@graph":{"@type":"Article","name":"In Object Graph"}}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        // @graph as object is not unwrapped (JS coerces only arrays); the
        // top-level has no @type → at_type_ok false → skip.
        assert!(ld.title.is_none());
    }

    /// `get_json_ld` — `@graph` array entries with no Article-type
    /// → continues to next script (covers the `None => continue` arm of
    /// the @graph find).
    /// rationale: complementary to existing
    /// `jsonld_at_graph_no_article_skips_to_next_script` — additionally
    /// pins the "no second script" → empty JsonLd path.
    #[test]
    fn jsonld_graph_no_article_and_no_next_script_returns_empty() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org",
             "@graph":[{"@type":"WebSite","name":"S"},
                       {"@type":"Organization","name":"O"}]}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
        assert!(ld.byline.is_none());
        assert!(ld.site_name.is_none());
    }

    /// `get_json_ld` — no scripts in document → empty JsonLd.
    /// rationale: pins the outer "no qualifying JSON-LD" return at
    /// `Readability.js:1746`.
    #[test]
    fn jsonld_no_scripts_returns_default() {
        let (_d, r) = doc("<html><body><p>no scripts here</p></body></html>");
        let ld = get_json_ld(&r);
        assert!(ld.title.is_none());
        assert!(ld.byline.is_none());
        assert!(ld.excerpt.is_none());
        assert!(ld.site_name.is_none());
        assert!(ld.date_published.is_none());
    }

    /// `get_json_ld` — top-level array find succeeds on FIRST entry but
    /// that first entry's @context fails the schema.org check → continue
    /// to next script.
    /// rationale: pins the array-find-then-context-fail interaction
    /// (covers the path where the array's chosen entry STILL fails the
    /// context filter at `:1662-1672`).
    #[test]
    fn jsonld_array_find_first_then_context_fail_continues() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            [{"@context":"http://example.org/ns","@type":"Article",
              "name":"Wrong Context"}]
            </script>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article","name":"Right"}
            </script>
        </head><body></body></html>"#;
        let (_d, r) = doc(html);
        let ld = get_json_ld(&r);
        assert_eq!(ld.title.as_deref(), Some("Right"));
    }

    // ---- get_article_title — separator/colon cascade residue ----

    #[test]
    fn article_title_separator_short_first_part_strips_leading_segment() {
        // rationale: `Readability.js:582-585` — when the substring before the
        // last separator has wordCount < 3, curTitle is recomputed as
        // `origTitle.replace(/^[^\|\-\\\/>»]*[\|\-\\\/>»]/, "")` (strip up to and
        // including the FIRST separator). Title "X | A Real Article Headline":
        // before-last-sep is "X" (1 word < 3) -> lead-strip yields
        // " A Real Article Headline" -> trim/normalize -> 4 words, hier=false so
        // the <=4 guard restores origTitle.
        let (_d, r) = doc(
            "<html><head><title>X | A Real Article Headline Here</title></head><body></body></html>",
        );
        // First part "X" has <3 words -> lead-separator strip path taken;
        // 5-word result > 4 so the <=4 guard does not restore origTitle.
        assert_eq!(get_article_title(&r), "A Real Article Headline Here");
    }

    #[test]
    fn article_title_colon_after_last_short_falls_back_to_first_colon() {
        // rationale: `Readability.js:606-612` — ": " present, no heading match;
        // after-last-colon substring has wordCount < 3, so curTitle falls back to
        // `origTitle.substring(indexOf(":")+1)` (after the FIRST colon).
        // "Brand: Section: Hi" -> after last colon = " Hi" (1 word < 3) ->
        // after first colon = " Section: Hi".
        let (_d, r) = doc(
            "<html><head><title>Brand: Section: Hi</title></head><body><p>x</p></body></html>",
        );
        // 2 words after normalize -> <=4 guard with hier=false restores origTitle.
        assert_eq!(get_article_title(&r), "Brand: Section: Hi");
    }

    #[test]
    fn article_title_colon_long_prefix_keeps_full_title() {
        // rationale: `Readability.js:613-615` — ": " present, no heading match,
        // after-last-colon has >=3 words, AND the prefix before the first colon
        // has wordCount > 5 -> curTitle = origTitle (keep the whole thing).
        let (_d, r) = doc(
            "<html><head><title>One Two Three Four Five Six: After The Colon Part Here</title></head>\
             <body><p>x</p></body></html>",
        );
        assert_eq!(
            get_article_title(&r),
            "One Two Three Four Five Six: After The Colon Part Here"
        );
    }

    #[test]
    fn article_title_very_long_no_separator_multiple_h1_keeps_long_title() {
        // rationale: `Readability.js:617-624` — no separator, no ": ", length
        // > 150; the single-h1 rule needs EXACTLY one <h1>. With two <h1> the
        // `hOnes.length === 1` FALSE side leaves the long curTitle in place.
        let long = "Word ".repeat(40); // ~200 chars, 40 words, no separator/colon
        let long = long.trim();
        let html = format!(
            "<html><head><title>{long}</title></head><body><h1>A</h1><h1>B</h1></body></html>"
        );
        let (_d, r) = doc(&html);
        // >150 chars, two h1 -> curTitle unchanged; 40 words > 4 -> kept.
        assert_eq!(get_article_title(&r), long);
    }

    // ---- byte_substring — defensive start>end swap ----

    #[test]
    fn byte_substring_swaps_when_start_exceeds_end() {
        // rationale: JS `substring(a, b)` swaps the arguments when a > b
        // (`:197-201`). byte_substring(s, 5, 2) must yield the same slice as
        // byte_substring(s, 2, 5).
        assert_eq!(byte_substring("abcdefg", 5, 2), "cde");
    }

    // ---- is_url_like — scheme-prefix predicate negative sides ----

    #[test]
    fn is_url_like_rejects_leading_colon() {
        // rationale: `Readability.js:441-448` _isUrl — a colon at position 0
        // (no scheme name) is not a valid absolute URL (`if colon == 0` arm).
        assert!(!is_url_like(":nonsense"));
    }

    #[test]
    fn is_url_like_rejects_non_alpha_scheme_start() {
        // rationale: WHATWG scheme must start with an ASCII letter; "1http:" has
        // a leading digit (`!bytes[0].is_ascii_alphabetic()` TRUE side).
        assert!(!is_url_like("1http://example.com"));
    }

    #[test]
    fn is_url_like_rejects_invalid_scheme_char() {
        // rationale: scheme chars must be `[A-Za-z0-9+\-.]`; an underscore in the
        // scheme fails the `.all(...)` predicate.
        assert!(!is_url_like("ht_tp://example.com"));
    }

    #[test]
    fn is_url_like_rejects_no_colon() {
        // rationale: no colon at all -> `position(|b| b == b':')` None arm ->
        // not a URL.
        assert!(!is_url_like("just-a-plain-byline-name"));
    }

    // ---- unescape_html_entities — numeric edge shapes ----

    #[test]
    fn unescape_numeric_entity_without_semicolon_left_alone() {
        // rationale: `Readability.js:1615-1623` numeric form requires a trailing
        // ';'. `&#65` (no ';') fails `try_numeric_entity` (the `b[p] != b';'`
        // arm) and the bare '&' is copied verbatim.
        assert_eq!(unescape_html_entities("A&#65 B"), "A&#65 B");
    }

    #[test]
    fn unescape_numeric_entity_empty_digit_run_left_alone() {
        // rationale: `&#;` has no digits (`p == digit_start`) -> not a numeric
        // entity -> '&' copied verbatim.
        assert_eq!(unescape_html_entities("x&#;y"), "x&#;y");
    }

    #[test]
    fn unescape_hex_entity_decoded() {
        // rationale: `&#x41;` -> 'A' via the hex radix arm (`b[2] == b'x'`).
        assert_eq!(unescape_html_entities("&#x41;"), "A");
    }

    // ===================================================================
    // M12 Stage — branch coverage push (readability/metadata.rs)
    // Per `wrk_docs/2026.05.26 - CC - Coverage Push Status Report.md`:
    // get_article_title separator/heading arms, get_json_ld @type/@context
    // residual, numeric-entity scanner edge arms, collect_meta_values
    // negative shapes.
    // ===================================================================

    // ---- get_article_metadata_title / get_article_metadata — empty meta value

    #[test]
    fn metadata_title_skips_whitespace_only_meta_value() {
        // rationale: `Readability.js:1803-1812` precedence loop — `values[key]`
        // exists but `js_trim` reduced its content to "" (a `content=" "` meta).
        // The `&& !v.is_empty()` guard FALSE side must skip it, falling through
        // to the NEXT precedence key (`title`). collect_meta_values inserts the
        // empty-after-trim value (content " " is non-empty pre-trim, so it is
        // stored), exercising the empty-value skip at the precedence loop.
        let (_d, r) = doc(
            "<html><head><meta property=\"og:title\" content=\" \">\
             <title>Real Doc Title Goes Here</title></head><body></body></html>",
        );
        // og:title is empty-after-trim -> skipped -> "title" key (document.title)
        // wins via get_article_title fallback.
        assert_eq!(get_article_metadata_title(&r), "Real Doc Title Goes Here");
    }

    #[test]
    fn get_article_metadata_skips_whitespace_only_og_title() {
        // rationale: same `!v.is_empty()` FALSE side inside get_article_metadata's
        // own title precedence loop (`Readability.js:1803-1812`) — an empty
        // og:title is skipped and the `title` key resolves to document.title.
        let (_d, r) = doc(
            "<html><head><meta property=\"og:title\" content=\" \">\
             <title>Another Long Article Title Value</title></head><body></body></html>",
        );
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.title, "Another Long Article Title Value");
    }

    // ---- get_article_title — `: ` colon branch when ALSO has a separator-less
    //      short curTitle that requires the lead-separator replace path.

    #[test]
    fn article_title_separator_word_count_lt3_uses_lead_separator_replace() {
        // rationale: `Readability.js:602-608` — when a hierarchical separator is
        // present BUT the substring before the last separator has <3 words, the
        // code falls to `origTitle.replace(REGEXPS.titleLeadSeparator, "")`.
        // "A | Real Long Article Heading Title": last sep gives curTitle "A"
        // (1 word, <3) -> lead-separator replace drops "A |", leaving
        // " Real Long Article Heading Title" -> normalised.
        let (_d, r) = doc(
            "<html><head><title>A | Real Long Article Heading Title</title></head><body></body></html>",
        );
        // After lead-separator strip + normalize: leading space trimmed.
        assert_eq!(
            get_article_title(&r),
            "Real Long Article Heading Title"
        );
    }

    // ---- get_article_title — `cond` second operand (L168/L170) ----

    #[test]
    fn article_title_hier_separator_keeps_shortened_when_word_count_minus_one_matches() {
        // rationale: `Readability.js:617-619` final guard
        //   if (curTitleWordCount <= 4 &&
        //       (!titleHadHierarchicalSeparators ||
        //        curTitleWordCount != wordCount(origTitle.replace(/sep+/g,"")) - 1))
        //       curTitle = origTitle;
        // Trace "Foo > Bar":
        //   sep " > " present (hier=true). curTitle = "Foo" (before last sep),
        //   wordCount("Foo")=1 < 3 -> lead-separator replace strips "Foo >" ->
        //   " Bar" -> normalize/trim -> "Bar" (wordCount 1, <= 4).
        //   orig without sep runs = "Foo  Bar" -> wordCount(/\s+/) = 2.
        //   cond = !true || (1 != 2-1=1) = false || false = FALSE.
        // So the `||` second operand is FALSE (this is the L168/L170 cond=FALSE
        // path) and the shortened "Bar" is KEPT (NOT restored to origTitle).
        let (_d, r) = doc("<html><head><title>Foo > Bar</title></head><body></body></html>");
        assert_eq!(get_article_title(&r), "Bar");
    }

    // ---- get_json_ld — @type/@context residual arms ----

    #[test]
    fn json_ld_article_type_matches_api_reference_suffix() {
        // rationale: `Readability.js:168-169` jsonLdArticleTypes — `APIReference`
        // is the LAST alternative, anchored at END (`...|APIReference$`). A type
        // ending in "APIReference" matches via the `s.ends_with` arm.
        assert!(json_ld_article_type_matches("FooAPIReference"));
    }

    #[test]
    fn json_ld_article_type_matches_middle_substring() {
        // rationale: middle alternatives are UNANCHORED (JS `^A|B|C$` precedence)
        // — `NewsArticle` matches as a substring anywhere.
        assert!(json_ld_article_type_matches("xxNewsArticleyy"));
    }

    #[test]
    fn json_ld_article_type_rejects_non_article() {
        // rationale: the final `false` — a type that matches no alternative.
        assert!(!json_ld_article_type_matches("Recipe"));
    }

    #[test]
    fn schema_dot_org_matches_http_variant() {
        // rationale: `Readability.js:1662` schemaDotOrgRegex — the
        // `s == "http://schema.org"` arm (TRUE side) accepts the http scheme.
        assert!(schema_dot_org_matches("http://schema.org"));
    }

    #[test]
    fn schema_dot_org_matches_https_with_trailing_slash() {
        // rationale: `/^https?...schema\.org\/?$/` — the optional trailing slash
        // is stripped then compared against "https://schema.org".
        assert!(schema_dot_org_matches("https://schema.org/"));
    }

    #[test]
    fn schema_dot_org_rejects_other_host() {
        assert!(!schema_dot_org_matches("https://example.org"));
    }

    #[test]
    fn json_ld_title_prefers_headline_when_only_headline_similar() {
        // rationale: `Readability.js:1690-1708` — both `name` and `headline`
        // present AND differ -> similarity tie-break. When headline ~ article
        // title (>0.75) but name does NOT, the `headline_matches && !name_matches`
        // TRUE side picks `headline`. The article <title> equals the headline so
        // text_similarity(headline, title) ~ 1.0; name is a distinct short brand.
        let html = "<html><head>\
            <title>The Quick Brown Fox Jumps Over The Lazy Dog Today</title>\
            <script type=\"application/ld+json\">\
            {\"@context\":\"https://schema.org\",\"@type\":\"Article\",\
             \"name\":\"Brand\",\
             \"headline\":\"The Quick Brown Fox Jumps Over The Lazy Dog Today\"}\
            </script></head><body></body></html>";
        let (_d, r) = doc(html);
        let jl = get_json_ld(&r);
        assert_eq!(
            jl.title.as_deref(),
            Some("The Quick Brown Fox Jumps Over The Lazy Dog Today")
        );
    }

    // ---- canonical_url — rel match arms ----

    #[test]
    fn canonical_url_returns_href_for_rel_canonical() {
        // rationale: `<link rel="canonical">` rel matches (TRUE side of
        // `rel.eq_ignore_ascii_case("canonical")`) and a non-empty href returns.
        let (_d, r) = doc(
            "<html><head><link rel=\"canonical\" href=\"https://e.com/x\"></head><body></body></html>",
        );
        assert_eq!(canonical_url(&r).as_deref(), Some("https://e.com/x"));
    }

    #[test]
    fn canonical_url_skips_non_canonical_rel() {
        // rationale: the `rel.eq_ignore_ascii_case("canonical")` FALSE side — a
        // `rel="stylesheet"` link is skipped, returning None when no canonical.
        let (_d, r) = doc(
            "<html><head><link rel=\"stylesheet\" href=\"a.css\"></head><body></body></html>",
        );
        assert!(canonical_url(&r).is_none());
    }

    #[test]
    fn canonical_url_none_when_no_link() {
        // rationale: the loop exhausts with no rel attribute present at all
        // (`get_attribute(link, "rel")` None -> `if let` FALSE side).
        let (_d, r) = doc(
            "<html><head><link href=\"a.css\"></head><body></body></html>",
        );
        assert!(canonical_url(&r).is_none());
    }

    // ---- collect_meta_values — negative-shape arms (via get_article_metadata) ----

    #[test]
    fn collect_meta_values_ignores_unmatched_property() {
        // rationale: `Readability.js:1771-1779` — a `<meta property>` whose value
        // does NOT match propertyPattern (the `find()` None / FALSE side) is not
        // stored; an unrelated property leaves title to the document.title path.
        let (_d, r) = doc(
            "<html><head><meta property=\"fb:app_id\" content=\"123\">\
             <title>Fallback Document Title Here</title></head><body></body></html>",
        );
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.title, "Fallback Document Title Here");
        assert!(md.site_name.is_none(), "no site_name from fb:app_id");
    }

    #[test]
    fn collect_meta_values_ignores_unmatched_name() {
        // rationale: `Readability.js:1781-1799` — a `<meta name>` not matching
        // namePattern (`is_match` FALSE side) is dropped; a `name="viewport"`
        // produces no byline/excerpt.
        let (_d, r) = doc(
            "<html><head><meta name=\"viewport\" content=\"width=device-width\">\
             <title>Doc Title</title></head><body></body></html>",
        );
        let md = get_article_metadata(&r, &JsonLd::default());
        assert!(md.byline.is_none() && md.excerpt.is_none());
    }

    #[test]
    fn collect_meta_values_name_author_populates_byline() {
        // rationale: namePattern is_match TRUE side — `name="author"` matches and
        // is stored under "author", feeding the byline precedence.
        let (_d, r) = doc(
            "<html><head><meta name=\"author\" content=\"Jane Doe\">\
             <title>Doc Title</title></head><body></body></html>",
        );
        let md = get_article_metadata(&r, &JsonLd::default());
        assert_eq!(md.byline.as_deref(), Some("Jane Doe"));
    }

    // ---- try_numeric_entity — radix / boundary arms ----

    #[test]
    fn unescape_uppercase_hex_entity_decoded() {
        // rationale: `Readability.js:1615` — `&#X41;` uses the `b[2] == b'X'`
        // (uppercase) operand of the radix selector. Decodes to 'A'.
        assert_eq!(unescape_html_entities("&#X41;"), "A");
    }

    #[test]
    fn unescape_numeric_entity_runs_to_end_without_semicolon() {
        // rationale: try_numeric_entity `p >= b.len()` middle operand of the
        // `p == digit_start || p >= b.len() || b[p] != b';'` reject — a digit run
        // that hits end-of-string with no ';' is not an entity.
        assert_eq!(unescape_html_entities("&#65"), "&#65");
    }

    #[test]
    fn unescape_four_byte_codepoint_entity() {
        // rationale: utf8_char_len `b < 0xF0` FALSE side — a decoded 4-byte UTF-8
        // codepoint (U+1F600 grinning face) is emitted then the scanner advances
        // by its full 4-byte length. `&#128512;` = U+1F600.
        let out = unescape_html_entities("a&#128512;b");
        assert_eq!(out, "a\u{1F600}b");
        assert!(out.contains('\u{1F600}'));
    }

    #[test]
    fn unescape_too_short_numeric_entity_left_alone() {
        // rationale: try_numeric_entity `b.len() < 4` TRUE side early-return —
        // `&#x` (3 bytes after the &) is too short to be a numeric entity.
        assert_eq!(unescape_html_entities("&#x"), "&#x");
    }
}
