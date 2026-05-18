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
//! Stage 1a wires `extract_with` → parse → [`Readability::new`]`.`[`parse`](Readability::parse)
//! → `Option<Article>` → `Result<Extracted, _>` (HLD §7.1). Later-stage
//! functions (sibling-append, `_cleanConditionally`, full metadata, …) remain
//! unported and are added in their scheduled stages.

pub mod dom;
pub mod grab_article;
pub mod helpers;
pub mod metadata;
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
/// `Readability.prototype`). Stage-1a fields only.
///
/// `_doc` is the parsed [`Dom`]; `_articleTitle` is resolved before
/// `_grabArticle` (HLD §7.1 / supervisor M-4). `_flags` starts with all flags
/// set (`Readability.js:69-72`). The Stage-1a `parse()` runs **one** grab pass
/// (no retry/flag-sieve loop — that is Stage 1c, HLD §7.3).
pub struct Readability {
    doc: Dom,
    article_title: String,
    flags: Flags,
    /// `this._articleByline` "found?" — Readability-instance state
    /// (`Readability.js:42`), threaded into `_grabArticle` (the byline-node
    /// removal is score-affecting; the stored string is Stage-4 metadata).
    article_byline_found: bool,
}

impl Readability {
    /// `new Readability(doc, options)` (`Readability.js:27-109`).
    ///
    /// Stage 1a uses default options only (`Options` knobs like
    /// `nbTopCandidates` / `charThreshold` are Stage-4 additive — HLD §7.6),
    /// so this takes just the parsed document. `_flags` = all set
    /// (`Readability.js:69-72`).
    pub fn new(doc: Dom) -> Self {
        Readability {
            doc,
            article_title: String::new(),
            flags: Flags::default(),
            article_byline_found: false,
        }
    }

    /// `parse()` (`Readability.js:2721-2779`) — **Stage-1a slice (HLD §7.1)**.
    ///
    /// Ported steps, in `Readability.js` order:
    /// * `_removeScripts(this._doc)` (`:2739`);
    /// * `_prepDocument()` (`:2741`) — `<style>` strip, `font`→`span`,
    ///   `_replaceBrs`;
    /// * `this._articleTitle = metadata.title` (`:2745`) via the title half of
    ///   `_getArticleMetadata` (`:2743`) — pulled forward (supervisor M-4)
    ///   because it feeds `_headerDuplicatesTitle` on the scored path;
    /// * `articleContent = this._grabArticle()` (`:2747`), Stage-1a single-pass
    ///   (no sibling-append, no retry loop);
    /// * the safe `_prepArticle` slice on `articleContent`
    ///   (`Readability.js:795-799`, `:835-850` — object/embed/footer/link/
    ///   aside `_clean` + empty-`<p>`);
    /// * `textContent = articleContent.textContent` (`:2766`).
    ///
    /// **Deliberately NOT ported at Stage 1a** (HLD §7): `_unwrapNoscriptImages`
    /// (`:2733`), `_getJSONLD` (`:2736` — so JSON-LD is `{}`),
    /// `_postProcessContent` (`:2754` — score-invisible cosmetics, HLD §2),
    /// the excerpt/byline/dir/lang/siteName/serialized-content half of the
    /// return object (Stage 4, HLD §7.6).
    ///
    /// Returns `None` only when `_grabArticle` returns `null`
    /// (`Readability.js:2748-2750`). The caller maps `None` to an empty `Ok`
    /// extraction (Bug-E2: "found nothing" is success, not error — HLD §7.1).
    pub fn parse(mut self) -> Option<Article> {
        // documentElement guard: `Readability.js` constructor requires
        // `doc.documentElement`. html5ever always synthesises <html> for a
        // full-document parse, so this is Some for every real input.
        let doc_root: NodeRef = self.doc.document();

        // _removeScripts(this._doc)  (Readability.js:2739)
        prep::remove_scripts(&doc_root);

        // _prepDocument()  (Readability.js:2741)
        let body = self.doc.body();
        prep::prep_document(&mut self.doc, &doc_root, body.as_ref());

        // metadata = _getArticleMetadata(jsonLd={}); this._articleTitle =
        // metadata.title  (Readability.js:2743-2745). Title pulled forward
        // (HLD §7.1 / M-4): it feeds _headerDuplicatesTitle on the scored path.
        self.article_title = metadata::get_article_metadata_title(&doc_root);

        // articleContent = this._grabArticle()  (Readability.js:2747)
        let body = self.doc.body()?; // no <body> ⇒ no article (return null)
        let grab = grab_article::grab_article(
            &mut self.doc,
            &doc_root,
            &body,
            &self.article_title,
            &self.flags,
            &mut self.article_byline_found,
        )?;
        let article_content = grab.article_content;

        // _prepArticle(articleContent) — Stage-1a safe slice only
        // (Readability.js:795-799 + 835-850; NOT _cleanConditionally /
        // _markDataTables / _cleanStyles / _cleanHeaders — Stage 2, HLD §7.4).
        prep::prep_article_stage1a(&article_content);

        // (Readability.js:2754 _postProcessContent is score-invisible cosmetics
        // — HLD §2 score-invisible partition — and is NOT run at Stage 1a.)

        // textContent = articleContent.textContent  (Readability.js:2766)
        let tc = text_content(&article_content);

        Some(Article {
            title: self.article_title,
            text_content: tc,
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
        Readability::new(Dom::parse(html)).parse()
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
    fn parse_empty_document_returns_some_with_empty_text() {
        // Bug-E2: a document with no content still parses to an Article whose
        // text is empty (the body-fallback path) — "found nothing" is a valid
        // result, NOT None/error. `parse()` returns None only on a genuine
        // `_grabArticle` null (no <body>); a present-but-empty <body> yields
        // the fake-div fallback ⇒ Some with empty text.
        let a = parse("<html><body>   </body></html>").expect("Some (empty ok)");
        assert_eq!(a.text_content.trim(), "");
    }
}
