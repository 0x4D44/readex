//! `mdrcel` — main-content extraction for arbitrary HTML.
//!
//! `mdrcel` takes a `&str` of HTML plus an optional base URL and returns the
//! page's main textual content together with a little metadata. It performs
//! **no** network I/O, **no** JavaScript rendering, and **no** encoding
//! detection — the caller owns all of that (parent brief
//! `2026.05.16 - BRIEF - Rust Content Extraction Library.md`, "What is
//! explicitly OUT of scope"). The crate is pure, synchronous, `std`-only
//! string/DOM work; a caller that needs it off the async hot path wraps it in
//! `spawn_blocking`.
//!
//! # Milestone status
//!
//! **M2 Stage 1a** (HLD `2026.05.18 - HLD - mdrcel Readability Port (M2)`
//! §7.1): the public API is unchanged but [`extract`] / [`extract_with`] now
//! run an idiomatic Rust port of Mozilla Readability v0.6.0 — the parse spine
//! (`_removeScripts` / `_prepDocument`), title resolution, scoring, and
//! **single top-candidate** selection. A page yielding an article returns a
//! populated `Ok`; a genuinely-empty extraction is a valid empty `Ok` (the
//! Bug-E2 doctrine — "found little" is success, never an error and never
//! [`ExtractError::NotImplemented`]). Sibling-append, the `_cleanConditionally`
//! / `_markDataTables` table passes, the retry/flag-sieve loop, and full
//! non-body metadata are **later stages** (HLD §7.2–§7.6) and are deliberately
//! not yet ported — so structured/nav/table pages legitimately over-include
//! versus the prose-only gold until those stages land. The
//! [`ExtractError::NotImplemented`] variant is retained but is no longer
//! returned on the happy path.
//!
//! There is intentionally **no** trait / strategy / plugin scaffolding here.
//! The parent brief explicitly warns against premature abstraction (the "M8
//! Glasgow ring road" antipattern — on-ramps built to nowhere). The dispatcher
//! between extraction strategies is a later-milestone concern and is added
//! when the strategies actually exist, not speculatively now.
//!
//! # The `extract` / `extract_with` invariant
//!
//! The parent brief mandates: *"Keep the default-`Options` path the same as
//! `extract()`."* That invariant is guaranteed **by construction** rather than
//! by parallel maintenance: [`extract`] is literally
//! `extract_with(html, base_url, &Options::default())`. The two entry points
//! therefore cannot diverge — there is only one code path. A unit test pins
//! the equivalence so a future refactor that breaks it fails loudly.
//!
//! # Word count
//!
//! [`Extracted::word_count`] is the **library's own** count over its own
//! extracted text. The differential test harness deliberately does **not**
//! trust it: the harness recomputes word count with its single canonical
//! tokenizer (harness HLD §8 — "The harness never trusts an external word
//! count"), exactly as it ignores each oracle's self-reported count. The field
//! is provided for direct library consumers (e.g. the consumer) as a convenience;
//! it is informational, not the harness's comparability surface.

// M2 Readability port (HLD `2026.05.18 - HLD - mdrcel Readability Port (M2)`).
//
// `#[doc(hidden)] pub`: this is **internal infrastructure + in-workspace
// verification surface only**, NOT part of the stable public contract. It is
// `pub` purely so the in-workspace differential harness (the `benchmark`
// path-dependency crate) and the Stage-0 parser-equivalence BLOCKER gate
// (`tests/parser_equivalence_gate.rs`, HLD §6.1) can drive
// `readability::dom::text_content` against jsdom — exactly the role the
// `benchmark` crate already plays as an in-tree consumer. It is `#[doc(hidden)]`
// so it does NOT appear in the crate's rendered API and external consumers get
// no stability promise on it.
//
// The **frozen extraction surface** the parent brief pins —
// `extract` / `extract_with` / `Extracted` / `Options` / `ExtractError` — is
// **signature-unchanged**, but as of M2 **Stage 1a** `extract_with` is wired
// to the port (parse → `Readability::new(doc).parse()` → `Result<Extracted,
// _>`): a page yielding an article now returns a real populated `Ok`, and a
// genuinely-empty extraction is a valid empty `Ok` (Bug-E2). The
// `ExtractError` enum is unchanged (`NotImplemented` is retained as a variant
// but is no longer returned on the happy path). This is the **0.3.0 MINOR**
// bump (first real extraction behind the frozen surface — see `Cargo.toml`);
// the public *types/signatures* are byte-for-byte unchanged.
#[doc(hidden)]
pub mod readability;

/// The extracted main content of an HTML document, plus light metadata.
///
/// Every field is owned so the result outlives the input `&str`. `title`,
/// `html`, `canonical_url` and `language` are best-effort and may be `None`;
/// `text` is always present (`""` if nothing was extracted — an empty body is
/// a *valid* result, not an error, mirroring the harness/oracle Bug-E2
/// doctrine that "found little" must not be conflated with failure).
///
/// `Eq` is deliberately **omitted** at M1: a future field (e.g. a
/// confidence/quality score, which extraction algorithms commonly carry as a
/// `f32`/`f64`) would make a `#[derive(Eq)]` impossible without a breaking
/// derive change, since floats are `PartialEq` but not `Eq`. The only
/// consumer (the differential harness) needs just `PartialEq` (to compare),
/// plus `Clone`/`Box`, so omitting `Eq` now costs nothing and forecloses a
/// future breaking decision.
#[derive(Debug, Clone, PartialEq)]
pub struct Extracted {
    /// The document title, if one could be determined. `None` when absent.
    pub title: Option<String>,
    /// The extracted main body text, whitespace-normalised. Never `None`;
    /// `""` when nothing qualified as content (a valid, non-error outcome).
    pub text: String,
    /// Sanitised, content-only HTML. `None` unless [`Options::include_html`]
    /// was set (opt-in — most consumers want only `text`).
    pub html: Option<String>,
    /// The library's own word count over [`text`](Self::text). Informational;
    /// the differential harness recomputes this with its own tokenizer and
    /// does **not** trust this value (see the crate-level docs).
    pub word_count: usize,
    /// The page's canonical URL (`<link rel="canonical">` / `og:url`), if one
    /// was found. `None` when absent.
    pub canonical_url: Option<String>,
    /// Best-effort content language (e.g. `"en"`). May be `None`.
    pub language: Option<String>,
}

/// Tuning knobs for [`extract_with`].
///
/// `Options` is **additive in v1.x**: new fields may be appended in a minor
/// release, but the [`Default`] surface is never widened without a major
/// version bump (parent brief: *"Add new options additively; never widen the
/// default surface without a major version."*). Speculative fields are
/// deliberately **not** added now — only what Milestone 1 needs to define the
/// frozen surface (no premature abstraction).
///
/// `Options::default()` MUST produce behaviour identical to [`extract`]; that
/// is guaranteed because [`extract`] delegates to [`extract_with`] with
/// exactly `Options::default()`.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// When `true`, populate [`Extracted::html`] with sanitised content-only
    /// HTML. Default `false` — most consumers want only the text and the HTML
    /// pass is extra work.
    pub include_html: bool,
    /// Minimum acceptable word count. An extraction below this threshold is a
    /// `ContentTooShort`-style error in a later milestone. Default `0` (no
    /// floor) so the default path never rejects on length — keeping the
    /// `Default` surface as permissive as `extract`.
    pub min_word_count: usize,
}

/// Errors returned by [`extract`] / [`extract_with`].
///
/// At Milestone 1 the only variant is [`NotImplemented`](Self::NotImplemented)
/// — the documented floor. Further variants (e.g. a content-too-short signal)
/// are **additive in later milestones**; they are intentionally **not**
/// declared now, because a variant with no behaviour behind it is premature
/// abstraction. The enum is deliberately **not** `#[non_exhaustive]`: the
/// in-workspace differential harness matches it *exhaustively without a
/// wildcard* on purpose, so that adding a variant breaks the harness build
/// and forces a conscious decision at the Bug-E2 site (see
/// `benchmark/src/crate_run.rs`) rather than silently laundering the new
/// variant into `crate_error`.
///
/// DEC-3: the `Display`/`Error` impls below are **hand-written** rather than
/// derived via `thiserror`. At M1 there is one variant with a static message
/// and no `#[from]`/`#[source]`, so `thiserror` would add a proc-macro
/// dependency (and `proc-macro2`/`quote`/`syn`) to what is otherwise a
/// **zero-dependency** library — for ~5 lines it does not save. `thiserror`
/// is therefore *deferred* until `ExtractError` actually grows variants /
/// `#[from]` / `#[source]` (mirrors how `scraper`/`html5ever` are deferred
/// until the algorithm needs them). This decision is fully reversible: re-add
/// the dependency and the derive at that point.
#[derive(Debug)]
pub enum ExtractError {
    /// The extraction algorithm is not implemented yet (Milestone-1 floor).
    /// The differential harness maps this to a first-class `not_implemented`
    /// status, distinct from a crate error and from an empty-but-ok result
    /// (harness HLD §5).
    NotImplemented,
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::NotImplemented => {
                f.write_str("content extraction is not implemented yet (Milestone 1 floor)")
            }
        }
    }
}

impl std::error::Error for ExtractError {}

/// Extract the main content of `html`.
///
/// `base_url`, when `Some`, is used to resolve relative links/URLs during
/// extraction (e.g. for the canonical URL). It is informational only — the
/// crate never fetches it.
///
/// Equivalent to `extract_with(html, base_url, &Options::default())`. This
/// delegation is the mechanism that guarantees the default path and `extract`
/// can never diverge (see the crate-level docs).
///
/// # Errors
///
/// At Milestone 1 this **always** returns [`ExtractError::NotImplemented`].
pub fn extract(html: &str, base_url: Option<&str>) -> Result<Extracted, ExtractError> {
    extract_with(html, base_url, &Options::default())
}

/// Extract the main content of `html` with explicit [`Options`].
///
/// `base_url` behaves as in [`extract`]. `opts` tunes the extraction;
/// `&Options::default()` reproduces [`extract`] exactly.
///
/// # Errors
///
/// At Milestone 1 this **always** returns [`ExtractError::NotImplemented`],
/// regardless of `html`, `base_url`, or `opts`. No parsing is performed — the
/// algorithm arrives in a later milestone behind this unchanged signature.
pub fn extract_with(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<Extracted, ExtractError> {
    // M2 Stage 1a (HLD §7.1): parse → Readability::new(doc).parse() → map
    // Option<Article> to Result<Extracted, _>.
    //
    // `base_url` / `opts` are intentionally still unused: relative-URL
    // resolution and `include_html` / `min_word_count` are Stage-4 additive
    // surface (HLD §7.6) — wiring them now would be behaviour ahead of its
    // contract. The frozen signature is unchanged; only the body grows.
    let _ = (base_url, opts);

    let doc = readability::dom::Dom::parse(html);
    match readability::Readability::new(doc).parse() {
        // An article was produced — a real `Ok`. `text` is the article node's
        // raw `text_content` (`Readability.js:2766` `articleContent.textContent`
        // — the field the differential harness scores, HLD §2; the harness
        // tokenizer does its own whitespace handling, so no pre-normalization
        // here, keeping the scored text byte-faithful to the JS return object).
        // `title` is `this._articleTitle`. Other metadata stays `None` until
        // Stage 4 (HLD §7.6 — not scored, deliberately deferred).
        Some(article) => {
            let text = article.text_content;
            let title = if article.title.is_empty() {
                None
            } else {
                Some(article.title)
            };
            let word_count = text.split_whitespace().count();
            Ok(Extracted {
                title,
                text,
                html: None,
                word_count,
                canonical_url: None,
                language: None,
            })
        }
        // `_grabArticle` returned `null` (no `<body>` to grab —
        // `Readability.js:2748-2750`). Per Bug-E2 (HLD §7.1) "found nothing"
        // is a **valid empty `Ok`**, NOT an error and NOT `NotImplemented`:
        // never fabricate, never regress a real extraction to the M1 floor.
        None => Ok(Extracted {
            title: None,
            text: String::new(),
            html: None,
            word_count: 0,
            canonical_url: None,
            language: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_HTML: &str = "<html><head><title>T</title></head>\
                               <body><article><p>hello world</p></article></body></html>";

    #[test]
    fn extract_returns_ok_with_body_text_at_stage1a() {
        // M2 Stage 1a: a page yielding an article now returns a real `Ok`
        // (no longer the M1 `NotImplemented` floor — HLD §7.1). The sample's
        // sole content is the article paragraph; its title is the <title>.
        let e = extract(SAMPLE_HTML, None).expect("Stage 1a must extract");
        assert!(e.text.contains("hello world"), "body text: {:?}", e.text);
        // <title>T</title>: len 1 (<15), no separator/colon -> single-h1
        // branch finds no <h1> (length != 1) so curTitle stays "T"; the
        // <=4-word guard (1 word, no hierarchical sep) restores origTitle "T".
        assert_eq!(e.title.as_deref(), Some("T"));
    }

    #[test]
    fn extract_with_returns_ok_at_stage1a() {
        let e = extract_with(
            SAMPLE_HTML,
            Some("https://example.com/"),
            &Options::default(),
        )
        .expect("Stage 1a must extract");
        assert!(e.text.contains("hello world"));
    }

    #[test]
    fn extract_empty_extraction_is_ok_not_error_bug_e2() {
        // Bug-E2 (HLD §7.1): a document that yields no content is a VALID
        // empty `Ok`, never `NotImplemented`, never an error, never
        // fabricated. (Non-default Options do not change this at Stage 1a —
        // include_html / min_word_count are Stage-4 additive, HLD §7.6.)
        let opts = Options {
            include_html: true,
            min_word_count: 999,
        };
        let e = extract_with("<html><body>   </body></html>", None, &opts)
            .expect("empty extraction is a valid Ok");
        assert!(
            e.text.trim().is_empty(),
            "expected empty text, got {:?}",
            e.text
        );
        assert!(e.title.is_none());
    }

    /// The documented invariant: `extract(h,b)` ≡
    /// `extract_with(h,b,&Options::default())`. Now that `Extracted` is really
    /// produced this is exercised over the **`Ok` arm** (`Extracted:
    /// PartialEq`), exactly as the original M1 tripwire anticipated ("keeps
    /// holding once `Extracted` is actually produced").
    #[test]
    fn extract_is_extract_with_default_options() {
        for (html, base) in [
            ("", None),
            (SAMPLE_HTML, None),
            (SAMPLE_HTML, Some("https://example.com/page")),
            (
                "<html><body><div><p>A genuine readable paragraph well over the twenty-five character minimum.</p></div></body></html>",
                None,
            ),
        ] {
            let a = extract(html, base);
            let b = extract_with(html, base, &Options::default());
            assert_eq!(
                a.is_ok(),
                b.is_ok(),
                "extract/extract_with Ok-ness diverged for {html:?}"
            );
            if let (Ok(a), Ok(b)) = (a, b) {
                assert_eq!(a, b, "extract/extract_with Extracted diverged for {html:?}");
            }
        }
    }

    #[test]
    fn options_default_field_values() {
        let o = Options::default();
        assert!(!o.include_html, "default include_html must be false");
        assert_eq!(o.min_word_count, 0, "default min_word_count must be 0");
    }

    #[test]
    fn options_is_clone_and_debug() {
        let o = Options {
            include_html: true,
            min_word_count: 7,
        };
        let c = o.clone();
        assert_eq!(c.include_html, o.include_html);
        assert_eq!(c.min_word_count, o.min_word_count);
        // Debug is derived; just exercise it.
        assert!(format!("{o:?}").contains("Options"));
    }

    #[test]
    fn extracted_constructs_with_all_fields() {
        let e = Extracted {
            title: Some("Title".to_string()),
            text: "body text".to_string(),
            html: Some("<p>body text</p>".to_string()),
            word_count: 2,
            canonical_url: Some("https://example.com/canon".to_string()),
            language: Some("en".to_string()),
        };
        assert_eq!(e.title.as_deref(), Some("Title"));
        assert_eq!(e.text, "body text");
        assert_eq!(e.html.as_deref(), Some("<p>body text</p>"));
        assert_eq!(e.word_count, 2);
        assert_eq!(
            e.canonical_url.as_deref(),
            Some("https://example.com/canon")
        );
        assert_eq!(e.language.as_deref(), Some("en"));
        // Clone + PartialEq are part of the public contract (the harness
        // boxes and compares Extracted).
        assert_eq!(e.clone(), e);
    }

    #[test]
    fn extracted_optional_fields_can_be_none_and_text_empty_is_valid() {
        // An empty extraction is a *valid* Extracted, not an error (Bug-E2
        // doctrine). This pins that the type can represent it.
        let e = Extracted {
            title: None,
            text: String::new(),
            html: None,
            word_count: 0,
            canonical_url: None,
            language: None,
        };
        assert!(e.text.is_empty());
        assert!(e.title.is_none());
    }

    #[test]
    fn not_implemented_display_is_sensible() {
        let msg = ExtractError::NotImplemented.to_string();
        // Must be human-readable and clearly signal the unimplemented floor
        // (the harness/report surfaces this; an empty/garbled message would
        // make the M1 baseline unreadable).
        assert!(!msg.is_empty());
        let lower = msg.to_lowercase();
        assert!(
            lower.contains("not implemented") || lower.contains("not implemented yet"),
            "Display should mention it is not implemented: {msg:?}"
        );
    }
}
