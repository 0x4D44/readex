//! Idiomatic Rust port of Mozilla Readability v0.6.0 (HLD
//! `2026.05.18 - HLD - mdrcel Readability Port (M2)`).
//!
//! # Module map (HLD §5)
//!
//! This mirrors `Readability.js` 1:1 so every ported line has an obvious home
//! and a cited spec anchor. **No trait / strategy / plugin scaffolding** — the
//! anti-premature-abstraction rule (CLAUDE.md; HLD §5).
//!
//! | Module | Responsibility | Stage |
//! |---|---|---|
//! | [`dom`] | Facade over `markup5ever_rcdom` — the score-critical DOM primitives | **Stage 0** |
//! | [`regexps`] | `REGEXPS` + the JS-faithful regex dialect (HLD §8) | **Stage 1a** |
//! | [`scoring`] | `_initializeNode` / `_getClassWeight` / `_getLinkDensity` / … | **Stage 1a** |
//! | [`grab_article`] | `_grabArticle` (the algorithmic core), Stage-1a single-pass slice | **Stage 1a** |
//! | [`prep`] | `_prepDocument` / `_removeScripts` / safe `_clean` / empty-`<p>` | **Stage 1a** |
//! | [`helpers`] | `_isProbablyVisible` / `_isPhrasingContent` / `_getNextNode` / … | **Stage 1a** |
//! | [`metadata`] | `_getArticleTitle` + title half of `_getArticleMetadata` | **Stage 1a** |
//!
//! `extract_with` wires → parse →
//! [`Readability::new_from_html`]`.`[`parse`](Readability::parse) →
//! `Option<Article>` → `Result<Extracted, _>` (HLD §7.1/§7.3). Sibling-append
//! (Stage 1b) and the retry/flag-sieve loop (Stage 1c) are ported;
//! `_cleanConditionally`/`_markDataTables` (Stage 2) and full non-body
//! metadata (Stage 4) remain unported and are added in their scheduled stages.

pub mod clean;
pub mod dom;
pub mod grab_article;
pub mod helpers;
pub mod mark_data_tables;
pub mod metadata;
pub mod parse_int;
pub mod prep;
pub mod regexps;
pub mod scoring;

use dom::{Dom, NodeRef, text_content};
use helpers::Flags;

/// The article produced by [`Readability::parse`] — Readability's return
/// object (`Readability.js:2767-2778`), Stage-4 populated.
///
/// Stage 1a populated `title` + `text_content`. Stage 4 (HLD §7.6) adds the
/// remaining non-body metadata (`byline`, `excerpt`, `site_name`,
/// `published_time`, `dir`, `lang`, optionally the serialized `content`) —
/// **NOT scored** (HLD §2 score-invisible partition) but mandatory API per the
/// brief. Every field is `Option<String>` except `title` and `text_content`
/// which are always present (faithful to JS where `_articleTitle` is always a
/// string and `textContent` is always defined).
pub struct Article {
    /// `this._articleTitle` (`Readability.js:2768`). Always present (the
    /// final `_getArticleTitle` fallback at `Readability.js:1815` ensures
    /// non-empty `metadata.title`).
    pub title: String,
    /// `articleContent.textContent` (`Readability.js:2766` / `:2773`) — the
    /// raw WHATWG `Node.textContent` of the final article node. **This is the
    /// field the differential harness scores** (HLD §2).
    pub text_content: String,
    /// `metadata.byline || this._articleByline` (`Readability.js:2769`).
    /// `None` when both are `undefined`.
    pub byline: Option<String>,
    /// `this._articleDir` (`Readability.js:2770`) — the `dir` attribute from
    /// an ancestor of the final top candidate (`Readability.js:1579-1593`).
    /// `None` when none of the ancestors carry `dir`.
    pub dir: Option<String>,
    /// `this._articleLang` (`Readability.js:2771`) — the `<html lang>`
    /// attribute. `None` when absent.
    pub lang: Option<String>,
    /// `metadata.excerpt` (`Readability.js:2775`) with the JS-side
    /// first-`<p>` fallback (`Readability.js:2759-2763`). `None` when both
    /// the metadata source and the first-paragraph fallback yield nothing.
    pub excerpt: Option<String>,
    /// `metadata.siteName || this._articleSiteName` (`Readability.js:2776`).
    /// `_articleSiteName` is constructor-defaulted to `null` and never
    /// reassigned in the spec (`Readability.js:44`), so this is purely the
    /// metadata-side value.
    pub site_name: Option<String>,
    /// `metadata.publishedTime` (`Readability.js:2777`).
    pub published_time: Option<String>,
    /// `<link rel="canonical">` href — not a Readability metadata field per
    /// se but the crate's `Extracted.canonical_url` slot (declared and
    /// previously un-populated; Stage 4 fills it from the obvious source).
    pub canonical_url: Option<String>,
    /// Serialized `articleContent` HTML — populated only when the caller
    /// asked for it via `Options.include_html`. `None` otherwise (faithful
    /// to the opt-in shape).
    pub content_html: Option<String>,
}

/// The `Readability` instance (`Readability.js:27-109` constructor +
/// `Readability.prototype`).
///
/// Holds the **original HTML bytes** because Stage-1c's retry loop re-parses
/// them per attempt (HLD §m-3): the JS resets `page.innerHTML = pageCacheHtml`
/// (`Readability.js:1043`/`1549`, the *post-`_prepDocument`* body) and re-runs
/// `_grabArticle`; re-parsing the original bytes and re-running the
/// deterministic pre-grab pipeline (`_removeScripts` + `_prepDocument`)
/// reconstructs the identical post-prep tree without deep-cloning the
/// `Rc`-keyed score side-tables (a fresh ABA surface — HLD §5.1). `_doc` /
/// `_flags` / `_articleByline` are therefore **per-attempt** state (a fresh
/// `Dom` + freshly-cleared `Flags` each attempt), owned inside the attempt
/// closure, not on this struct.
pub struct Readability {
    /// The original HTML, owned so each retry attempt can re-parse it
    /// (`Readability.js`'s `pageCacheHtml` analogue under the HLD §m-3
    /// re-parse decision).
    html: String,
    /// Stage-4 (HLD §7.6) opt-in: when `true`, eagerly serialize each
    /// attempt's `articleContent` for [`Article::content_html`]. Default
    /// `false` so the harness path and existing consumers see no extra work
    /// (the default-`extract` byte-identity invariant).
    include_html: bool,
}

impl Readability {
    /// `new Readability(doc, options)` (`Readability.js:27-109`).
    ///
    /// Stage 1a/1b took a pre-parsed [`Dom`]; Stage 1c needs to re-parse per
    /// retry attempt (HLD §m-3), so it takes the original HTML and parses
    /// internally (each attempt re-parses it). Kept as the same one-argument
    /// constructor shape via [`Readability::new_from_html`]; the old
    /// `new(Dom)` form is retained for the existing in-crate test call sites
    /// (it re-serialises is unnecessary — those tests parse then hand the
    /// `Dom` straight in; Stage 1c re-parses from the HTML string instead, so
    /// the public crate entry is [`new_from_html`](Self::new_from_html)).
    /// Default `Options` only (`charThreshold` etc. are Stage-4 additive — HLD
    /// §7.6); flags start all-set per attempt (`Readability.js:69-72`).
    pub fn new_from_html(html: &str) -> Self {
        Readability {
            html: html.to_string(),
            include_html: false,
        }
    }

    /// Stage 4 (HLD §7.6) — opt-in `include_html` builder. When `true` the
    /// per-attempt closure eagerly serializes `articleContent` so the
    /// `Article.content_html` can carry the JS `_serializer(articleContent)`
    /// analogue (`Readability.js:2772`). Default is `false`; the harness path
    /// uses the default and is therefore byte-identical to Stage 3.
    pub fn include_html(mut self, on: bool) -> Self {
        self.include_html = on;
        self
    }

    /// `parse()` (`Readability.js:2721-2779`) — **Stage-1a/1b/1c slice**.
    ///
    /// Ported steps, in `Readability.js` order. The pre-grab pipeline
    /// (`_removeScripts` `:2739`, `_prepDocument` `:2741`, the title half of
    /// `_getArticleMetadata` `:2743-2745`) plus one `_grabArticle` attempt
    /// (`:2747`) plus the Stage-1a safe `_prepArticle` slice
    /// (`Readability.js:795-799`, `:835-850`) plus `articleContent.textContent`
    /// (`:2766`) form **one attempt**, run by the [`attempt`](Self::attempt)
    /// closure. Stage-1c's [`grab_article_with_retry`] drives it: the
    /// `textLength < _charThreshold` retry, the
    /// `FLAG_STRIP_UNLIKELYS`→`FLAG_WEIGHT_CLASSES`→`FLAG_CLEAN_CONDITIONALLY`
    /// flag sieve, `_attempts` bookkeeping, and the longest-attempt fallback
    /// (`Readability.js:1043`, `1546-1576`).
    ///
    /// **Re-parse per attempt (HLD §m-3).** Each attempt re-parses
    /// `self.html`; re-running the deterministic pre-grab pipeline
    /// reconstructs the post-`_prepDocument` tree the JS would have via
    /// `page.innerHTML = pageCacheHtml` (`Readability.js:1549`). The title is
    /// recomputed per attempt (it reads `<title>`/`<meta>`, which
    /// `_prepDocument` does not remove — so it is identical every attempt;
    /// recomputing it is simplest and faithful, not a behaviour change).
    ///
    /// **Deliberately NOT ported here** (HLD §7): `_getJSONLD` (`:2736` —
    /// JSON-LD is `{}`), `_postProcessContent` (`:2754` — score-invisible
    /// cosmetics, HLD §2), the excerpt/byline/dir/lang/siteName/serialized
    /// half of the return object (Stage 4, HLD §7.6). Stage 2 ports
    /// `_unwrapNoscriptImages` (`:2733`) — see [`prep::unwrap_noscript_images`]
    /// — and `_cleanConditionally`/`_markDataTables`/`_cleanStyles`/
    /// `_cleanHeaders` (HLD §7.4).
    ///
    /// Returns `None` only when every attempt's `_grabArticle` returns `null`
    /// (no `<body>` — `Readability.js:2748-2750`) or the flag sieve is
    /// exhausted with zero text in every attempt (`Readability.js:1570`). The
    /// caller maps `None` to an empty `Ok` (Bug-E2 — HLD §7.1).
    pub fn parse(self) -> Option<Article> {
        let html = self.html;
        let include_html = self.include_html;

        // Stage 4 (HLD §7.6) — full pre-grab metadata (JSON-LD +
        // `_getArticleMetadata`). The JS does this ONCE before
        // `_grabArticle` (`Readability.js:2733-2745`); the retry loop does
        // not re-compute it (the pre-grab pipeline does not mutate
        // `<title>`/`<meta>`/`<script type="application/ld+json">`, so
        // re-computing each attempt is identical anyway).
        //
        // The same pre-grab pipeline used by each attempt must run BEFORE
        // metadata (RJS line ordering: unwrap noscript → JSON-LD → remove
        // scripts → prep document → metadata). JSON-LD reads `<script
        // type="application/ld+json">` so it MUST happen before
        // `_removeScripts`.
        let metadata_pre = {
            let mut doc = Dom::parse(&html);
            let doc_root = doc.document();
            prep::unwrap_noscript_images(&doc_root);
            // 2736 jsonLd = _getJSONLD(doc) BEFORE _removeScripts at 2739.
            let jsonld = metadata::get_json_ld(&doc_root);
            // 2739 _removeScripts (now safe — JSON-LD already read).
            prep::remove_scripts(&doc_root);
            // 2741 _prepDocument().
            let body = doc.body();
            prep::prep_document(&mut doc, &doc_root, body.as_ref());
            // 2743 metadata = _getArticleMetadata(jsonLd).
            let md = metadata::get_article_metadata(&doc_root, &jsonld);
            // Crate-specific (NOT a Readability return field): canonical
            // URL + html-lang. Read from the same post-prep tree so the
            // single pre-grab pass produces all of them.
            let canonical = metadata::canonical_url(&doc_root);
            let lang = metadata::html_lang(&doc_root);
            PreGrabMetadata {
                title: md.title.clone(),
                byline: md.byline,
                excerpt: md.excerpt,
                site_name: md.site_name,
                published_time: md.published_time,
                canonical_url: canonical,
                lang,
            }
        };
        let article_title = metadata_pre.title.clone();

        // Stage 4 (HLD §7.6): byline detection's JS gate is
        // `!_articleByline && !_metadata.byline` (`Readability.js:1082-1085`).
        // Pre-seeding `byline_found = true` when metadata.byline is already
        // populated SHORT-CIRCUITS the in-tree byline-detect — score-
        // affecting, faithful to the JS double-gate. The same flag is
        // threaded across retry attempts (mirrors `this._articleByline`
        // persisting across the retry — it's instance state, not per-attempt).
        let metadata_byline = metadata_pre.byline.clone();
        let mut byline_found_state = metadata_byline.is_some();
        let mut byline_text_state: Option<String> = metadata_byline.clone();

        // One attempt = the JS `while (true)` body (Readability.js:1045-1545):
        // re-parse → _removeScripts → _prepDocument → title → _grabArticle →
        // _prepArticle → capture textContent + _getInnerText length.
        let attempt = |flags: &Flags,
                       byline_found: &mut bool,
                       byline_text: &mut Option<String>|
         -> Option<grab_article::AttemptOutcome> {
            // re-parse the ORIGINAL bytes (HLD §m-3). A fresh Dom ⇒ fresh
            // tree + fresh empty Rc-keyed side tables (ABA-safe — HLD §5.1).
            let mut doc = Dom::parse(&html);
            let doc_root: NodeRef = doc.document();

            // _unwrapNoscriptImages(this._doc)  (Readability.js:2733).
            // Placeholder-img cull (`:1895-1913`) AND noscript-img unwrap
            // (`:1916-1967`) — must run BEFORE `_removeScripts` drops
            // `<noscript>` (the unwrap reads noscript children) and BEFORE
            // `_prepDocument` / `_grabArticle` so the placeholder-cull's
            // `_cleanConditionally` img-count impact (`Readability.js:2498`)
            // matches RJS.
            prep::unwrap_noscript_images(&doc_root);

            // _removeScripts(this._doc)  (Readability.js:2739)
            prep::remove_scripts(&doc_root);

            // _prepDocument()  (Readability.js:2741). Re-running this on the
            // re-parsed bytes reconstructs the post-prep tree the JS would
            // have after `page.innerHTML = pageCacheHtml` (HLD §m-3).
            let body = doc.body();
            prep::prep_document(&mut doc, &doc_root, body.as_ref());

            // metadata.title (Readability.js:2743-2745) — feeds
            // _headerDuplicatesTitle on the scored path (HLD §7.1 / M-4).
            let article_title_local = metadata::get_article_metadata_title(&doc_root);

            let body = doc.body()?;
            let grab = grab_article::grab_article(
                &mut doc,
                &doc_root,
                &body,
                &article_title_local,
                flags,
                byline_found,
                byline_text,
            )?;
            let article_content = grab.article_content;
            // grab.article_byline is whatever the in-tree detector set THIS
            // attempt; it has already been folded into `*byline_text` via
            // the &mut borrow on the byline-detect arm. We do NOT re-assign
            // here (an attempt's byline-detect may not fire on retry; that
            // is faithful — the JS keeps the first attempt's byline).
            let attempt_dir = grab.article_dir;

            // Readability.js:2754 _postProcessContent(articleContent) — the
            // text-affecting portion ported at Stage 3 (HLD §7.5).
            prep::post_process_content(&article_content);

            // 1545 textLength = _getInnerText(articleContent, true).length;
            // 2766 textContent = articleContent.textContent. Capture
            // eagerly as owned values (this attempt's `doc` drops at closure
            // return; the retry driver must not hold a node from it — HLD
            // §m-3 ABA).
            let inner_text_len = scoring::inner_text_len(&article_content);
            let text_content = text_content(&article_content);

            // Stage 4 metadata captures, eagerly while the tree is alive:
            // (a) first <p>'s textContent.trim() for excerpt fallback
            //     (`Readability.js:2759-2763`);
            // (b) `_serializer(articleContent)` when `include_html`
            //     requested (`Readability.js:2772`).
            let first_paragraph_excerpt = {
                use crate::readability::dom::get_elements_by_tag_name;
                let ps = get_elements_by_tag_name(&article_content, "p");
                ps.first().and_then(|p| {
                    let t = dom::text_content(p);
                    let t = t.trim().to_string();
                    if t.is_empty() { None } else { Some(t) }
                })
            };
            let serialized_html = if include_html {
                Some(dom::serialize_html(&article_content))
            } else {
                None
            };

            Some(grab_article::AttemptOutcome {
                text_content,
                inner_text_len,
                first_paragraph_excerpt,
                serialized_html,
                article_dir: attempt_dir,
            })
        };

        // The Stage-1c retry/flag-sieve/fallback loop (Readability.js:1043,
        // 1546-1576) drives `attempt`. Returns the chosen articleContent's
        // captured metadata, or None (every attempt empty / no <body>).
        let result = grab_article::grab_article_with_retry(|flags| {
            attempt(flags, &mut byline_found_state, &mut byline_text_state)
        })?;

        // 2759-2763 metadata.excerpt = metadata.excerpt || first_paragraph.
        let final_excerpt = metadata_pre.excerpt.or(result.first_paragraph_excerpt);

        // 2769 byline: metadata.byline || this._articleByline.
        // - metadata_byline is the pre-grab metadata byline (if any);
        // - byline_text_state is what the in-tree byline-detect captured
        //   (only fires when metadata.byline was None — gated above);
        //   it is also pre-seeded to metadata_byline so the simpler
        //   "first non-None" reduces to the JS precedence.
        let final_byline = metadata_byline.or(byline_text_state);

        Some(Article {
            title: article_title,
            text_content: result.text_content,
            byline: final_byline,
            dir: result.article_dir,
            lang: metadata_pre.lang,
            excerpt: final_excerpt,
            site_name: metadata_pre.site_name,
            published_time: metadata_pre.published_time,
            canonical_url: metadata_pre.canonical_url,
            content_html: result.serialized_html,
        })
    }
}

/// The pre-grab metadata bundle (`_getArticleMetadata` + crate-specific
/// canonical/lang) computed once before the retry loop.
struct PreGrabMetadata {
    title: String,
    byline: Option<String>,
    excerpt: Option<String>,
    site_name: Option<String>,
    published_time: Option<String>,
    canonical_url: Option<String>,
    lang: Option<String>,
}

#[cfg(test)]
mod tests {
    //! End-to-end `Readability::parse` Stage-1a behaviour, expected values
    //! hand-derived by tracing `Readability.js` (NOT by running an oracle —
    //! inversion, HLD §4).
    use super::*;

    fn parse(html: &str) -> Option<Article> {
        Readability::new_from_html(html).parse()
    }

    #[test]
    fn parse_example_com_real_snapshot_shape_faithful() {
        // The REAL example.com gold snapshot shape (verbatim structure from
        // benchmark/corpus/snapshots/0f115db062b7c0dd.html, NOT copied from
        // any oracle output). Faithful Readability.js: `_articleTitle` resolves
        // to "Example Domain" (via _getArticleTitle: title len 14 < 15 →
        // single-<h1> branch → innerText("Example Domain"); <=4-word guard
        // restores origTitle "Example Domain"). Then `_headerDuplicatesTitle`
        // removes the `<h1>Example Domain</h1>` (similarity 1.0 > 0.75,
        // Readability.js:1112). So a faithful extraction's body is the
        // descriptive sentence + the "Learn more" link text — WITHOUT the h1.
        // The gold expects the h1 present; gold.tsv documents "both oracles
        // drop the <h1>". This asserts the FAITHFUL outcome (HLD §4
        // anti-inversion — NOT tuned to gold; honest-STOP reported upstream).
        let html = "<!doctype html><html lang=\"en\"><head><title>Example Domain</title>\
            <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
            <style>body{background:#eee}</style></head><body><div><h1>Example Domain</h1>\
            <p>This domain is for use in documentation examples without needing permission. Avoid use in operations.</p>\
            <p><a href=\"https://iana.org/domains/example\">Learn more</a></p></div></body></html>";
        let a = parse(html).expect("should produce an article");
        assert_eq!(a.title, "Example Domain");
        assert!(
            !a.text_content.contains("Example Domain"),
            "faithful: title-dup <h1> removed (Readability.js:1112): {:?}",
            a.text_content
        );
        assert!(
            a.text_content.contains("documentation examples"),
            "descriptive sentence must be in body: {:?}",
            a.text_content
        );
    }

    #[test]
    fn parse_scripts_and_styles_removed_from_body_text() {
        let html = "<html><head><title>T Long Enough Title Here</title>\
            <style>.x{color:red}</style></head>\
            <body><div class=content><script>doEvil()</script>\
            <p>The real readable body paragraph is well past twenty-five characters of genuine prose content.</p></div></body></html>";
        let a = parse(html).expect("article");
        assert!(a.text_content.contains("real readable body"));
        assert!(!a.text_content.contains("doEvil"), "script text leaked");
        assert!(!a.text_content.contains("color:red"), "style text leaked");
    }

    #[test]
    fn parse_empty_document_returns_none_faithful_stage1c_retry_exhaustion() {
        // FAITHFUL Stage-1c behaviour (changed from the Stage-1a/1b
        // STOP-before-retry interim). An empty `<body>` → fake-div fallback →
        // articleContent text length 0 < `_charThreshold` (500,
        // `Readability.js:1546`) → push attempt + clear a flag, repeated until
        // the flag sieve is exhausted; then `Readability.js:1564-1571` sorts
        // `_attempts` by `textLength` desc and `if (!_attempts[0].textLength)
        // return null;` — every attempt produced 0 chars, so `_grabArticle`
        // returns **null**, and `parse()` returns null
        // (`Readability.js:2748-2750`). Stage 1a/1b returned `Some("")` ONLY
        // because they deliberately STOPPED before this retry loop; the
        // faithful loop converges to the JS `null`. This is NOT a Bug-E2
        // violation: Bug-E2 ("found nothing is a valid empty Ok, never an
        // error") is defined and preserved at the `lib.rs` layer, which maps
        // `parse() == None` → `Ok(Extracted { text: "" , .. })` (see
        // `lib.rs::extract_with` `None =>` arm and the
        // `extract_empty_extraction_is_ok_not_error_bug_e2` test). The
        // `Article`/`None` boundary is internal; the public `extract` contract
        // is unchanged.
        assert!(
            parse("<html><body>   </body></html>").is_none(),
            "faithful Readability.js: empty doc → all attempts 0 chars → \
             _grabArticle null → parse() None (Readability.js:1569-1571, \
             2748-2750)"
        );
    }
}
