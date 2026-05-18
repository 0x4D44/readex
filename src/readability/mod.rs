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

/// The article produced by [`Readability::parse`] — the Stage-1a subset of
/// Readability's return object (`Readability.js:2767-2778`).
///
/// Stage 1a populates `title` (`this._articleTitle`) and `text_content`
/// (`articleContent.textContent`, the harness-scored field — HLD §2). The
/// other metadata fields (`byline`, `dir`, `lang`, `excerpt`, `siteName`,
/// `publishedTime`, serialized `content`) are **Stage 4** (HLD §7.6) and are
/// deliberately absent here, not stubbed with speculative values.
pub struct Article {
    /// `this._articleTitle` (`Readability.js:2768`).
    pub title: String,
    /// `articleContent.textContent` (`Readability.js:2766` / `:2773`) — the
    /// raw WHATWG `Node.textContent` of the final article node. **This is the
    /// field the differential harness scores** (HLD §2).
    pub text_content: String,
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
        }
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

        // The metadata title is **attempt-invariant**: `_getArticleMetadata`/
        // `_getArticleTitle` read `<title>`/`<meta>`, which the pre-grab
        // pipeline does NOT remove (`_removeScripts` drops `<script>`/
        // `<noscript>`; `_prepDocument` strips `<style>`, retags `<font>`,
        // replaces `<br>` runs — none touch `<head>`/`<title>`/`<meta>`).
        // Re-parse is deterministic, so every attempt computes the SAME
        // title. Compute it once here for `Article.title`, running the SAME
        // pre-title pipeline the JS runs before `_getArticleMetadata`
        // (`Readability.js:2739` `_removeScripts` → `:2741` `_prepDocument`
        // → `:2743` `_getArticleMetadata`) so this is byte-identical to each
        // attempt's internal title BY CONSTRUCTION, not merely by argument.
        // Each attempt still recomputes it (identical value) for
        // `_headerDuplicatesTitle` on its own fresh tree.
        let article_title = {
            let mut doc = Dom::parse(&html);
            let doc_root = doc.document();
            // _unwrapNoscriptImages(this._doc)  (Readability.js:2733) — must
            // run BEFORE _removeScripts (`:2739`) drops `<noscript>` and
            // BEFORE _prepDocument (`:2741`). The title path runs the same
            // pre-grab pipeline as the attempt closure (HLD §m-3); inserting
            // this call here is title-invariant (the function only touches
            // `<img>`/`<noscript>` subtrees, never `<title>`/`<meta>`) but
            // kept BY CONSTRUCTION identical to the attempt closure so any
            // future divergence is structurally visible.
            prep::unwrap_noscript_images(&doc_root);
            prep::remove_scripts(&doc_root);
            let body = doc.body();
            prep::prep_document(&mut doc, &doc_root, body.as_ref());
            metadata::get_article_metadata_title(&doc_root)
        };

        // One attempt = the JS `while (true)` body (Readability.js:1045-1545):
        // re-parse → _removeScripts → _prepDocument → title → _grabArticle →
        // _prepArticle → capture textContent + _getInnerText length.
        let attempt = |flags: &Flags| -> Option<grab_article::AttemptOutcome> {
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
            let article_title = metadata::get_article_metadata_title(&doc_root);

            // articleContent = this._grabArticle()  (Readability.js:2747).
            // No <body> ⇒ _grabArticle returns null ⇒ parse() returns null
            // (Readability.js:2748-2750) — propagated as None (NOT an
            // _attempts push; 1551 is only reached when articleContent exists
            // but is too short).
            //
            // **Stage 2 ORDER**: `grab_article` now runs `_prepArticle`
            // INTERNALLY (Readability.js:1512) **before** the page-wrap
            // (`Readability.js:1517-1532`) — the JS order. Stage 1c's swap
            // (page-wrap → `_prepArticle` in this closure) was retired
            // because the full Stage-2 `_cleanConditionally`'s
            // `_hasAncestorTag(node, "code", maxDepth=3)` is no longer
            // ancestor-level-invariant under the extra page-wrap div (see
            // the order-fidelity note in `grab_article`).
            let body = doc.body()?;
            let mut byline_found = false;
            let grab = grab_article::grab_article(
                &mut doc,
                &doc_root,
                &body,
                &article_title,
                flags,
                &mut byline_found,
            )?;
            let article_content = grab.article_content;

            // (Readability.js:2754 _postProcessContent — score-invisible
            // cosmetics, HLD §2 — NOT run here.)

            // 1545 textLength = _getInnerText(articleContent, true).length;
            // 2766 textContent = articleContent.textContent. Capture BOTH
            // eagerly as owned values (this attempt's `doc` drops at closure
            // return; the retry driver must not hold a node from it — HLD
            // §m-3 ABA).
            let inner_text_len = scoring::inner_text_len(&article_content);
            let text_content = text_content(&article_content);
            Some(grab_article::AttemptOutcome {
                text_content,
                inner_text_len,
            })
        };

        // The Stage-1c retry/flag-sieve/fallback loop (Readability.js:1043,
        // 1546-1576) drives `attempt`. Returns the chosen articleContent's
        // textContent, or None (every attempt empty / no <body>).
        let result = grab_article::grab_article_with_retry(attempt)?;

        Some(Article {
            title: article_title,
            text_content: result.text_content,
        })
    }
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
