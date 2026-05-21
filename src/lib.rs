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
//! **M3 Stage 9** (HLD `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.6,
//! THE M3 FINALE): the public [`extract`] / [`extract_with`] functions now
//! drive the **full Trafilatura cascade** (`core.bare_extraction`,
//! `core.py:130-358`) — parse + `tree_cleaning` + `convert_tags` +
//! `bare_extraction_with_cascade` (own → readability_fork → jusText, with the
//! 7-branch arbiter + dedup gate + sanitize post-pass) +
//! `metadata::extract_metadata` (OG / meta-name / itemprop / JSON-LD / URL /
//! date) + `extract_comments`. The M2 Readability port is preserved verbatim
//! under [`extract_via_readability`] for callers who want the older path.
//! Every public type and signature is byte-unchanged from M2 except for ONE
//! additive field on [`Extracted`] (`comments: String`, defaulting to `""`)
//! — additive only, exhaustive struct-literal callers upgrade via
//! `..Extracted::default()` (the M2 Stage 4 pattern).
//!
//! **M2 Stage 1a/1b/1c/2** (HLD `2026.05.18 - HLD - mdrcel Readability Port
//! (M2)` §7.1–§7.4): the public API is unchanged but [`extract`] /
//! [`extract_with`] now run an idiomatic Rust port of Mozilla Readability
//! v0.6.0 — the parse spine (`_removeScripts` / `_prepDocument`), title
//! resolution, scoring, single top-candidate selection, sibling-append, the
//! `FLAG_*` retry / flag-sieve / longest-text fallback, the `readability-
//! page-1` page-wrap, AND (Stage 2) the full faithful `_prepArticle`:
//! `_markDataTables` (with the JS-faithful `parse_int_js` rowspan/colspan
//! coercion), `_cleanConditionally` (the complete shadiness checklist incl.
//! the data-table KEEP, ancestor-table KEEP, ancestor-code KEEP, and
//! image-gallery exception), `_cleanHeaders`, `_cleanStyles`,
//! `_cleanMatchedNodes` (share-strip), single-cell-`<table>` unwrap,
//! `<h1>`→`<h2>` retag, `<br>`-before-`<p>` removal. A page yielding an
//! article returns a populated `Ok`; a genuinely-empty extraction is a valid
//! empty `Ok` (the Bug-E2 doctrine — "found little" is success, never an
//! error and never [`ExtractError::NotImplemented`]). Full non-body metadata
//! is the **last stage** (HLD §7.6) and is deliberately not yet ported. The
//! [`ExtractError::NotImplemented`] variant is retained but is no longer
//! returned on the happy path.
//!
//! **HLD §4 anti-inversion (Stage 2 anchor).** `_cleanConditionally`
//! deliberately KEEPS marked data tables (`Readability.js:2461-2463` and the
//! ancestor-data-table check `:2466-2468`); the port faithfully preserves
//! EDGAR/HMRC financial tables exactly as Readability-JS does. The faithful
//! port converges TO Readability-JS — it does NOT out-clean it. Word-count
//! gaps versus a "narrative-only" human gold on table-heavy pages are
//! therefore the documented diagnostic residual, never a tuning signal.
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

// M3 Stage 0b (HLD `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §6.1,
// DECISION-A ratified). The greenfield XPath evaluator + conformance harness
// lands here under the same `#[doc(hidden)] pub` infrastructure surface that
// `readability` uses: in-workspace consumers (the `tests/xpath_conformance.rs`
// harness; later M3 stages: `cleaning`, `main_extractor`, `baseline`) can
// drive `trafilatura::xpath_engine::evaluate` against a Python `lxml`
// subprocess, but the external crate API is unchanged. Subsequent M3 stages
// fold in more sub-modules (`cleaning`, `main_extractor`, etc.) — Stage 0b is
// the XPath floor only.
#[doc(hidden)]
pub mod trafilatura;

// M4 Stage 1 sub-stage A (HLD M4 — htmldate port). New `#[doc(hidden)] pub
// mod htmldate` infrastructure surface mirroring the M3 `trafilatura` shape.
// Sub-stage A lands the module-level settings constants (`MIN_DATE`,
// `MAX_FILE_SIZE`, `CACHE_SIZE`, `MAX_POSSIBLE_CANDIDATES`, `CLEANING_LIST`)
// from `htmldate/settings.py` and the `Extractor` + `trim_text` helpers from
// `htmldate/utils.py`. Sub-stages B onwards add the date-parsing algorithm
// itself; the public `extract` / `extract_with` surface is byte-unchanged.
#[doc(hidden)]
pub mod htmldate;

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
#[derive(Debug, Clone, PartialEq, Default)]
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
    /// Article author / byline (`metadata.byline || this._articleByline` —
    /// `Readability.js:2769`). `None` when absent.
    ///
    /// Populated by M2 **Stage 4** (HLD §7.6); previously always `None`. Not
    /// scored by the differential harness — this is API-completeness ahead
    /// of the M5 the consumer shim.
    pub byline: Option<String>,
    /// Brief excerpt of the article — `og:description` / `<meta name=
    /// "description">` / the first `<p>` of the article body, in
    /// `_getArticleMetadata` precedence order (`Readability.js:2775`). `None`
    /// only when no source yielded a value.
    ///
    /// Populated by M2 **Stage 4**; not scored.
    pub excerpt: Option<String>,
    /// Site name (`og:site_name`) when present. `None` otherwise.
    ///
    /// Populated by M2 **Stage 4**; not scored.
    pub site_name: Option<String>,
    /// Article publication time (`article:published_time` / parsely date /
    /// JSON-LD `datePublished`). Returned verbatim as the source provided it
    /// (typically ISO-8601, but not validated). `None` when absent.
    ///
    /// Populated by M2 **Stage 4**; not scored.
    pub published_time: Option<String>,
    /// Text direction (`dir="rtl"` / `dir="ltr"` etc.) from an ancestor of
    /// the top candidate (`Readability.js:1587-1592`). `None` when no
    /// ancestor carried `dir`.
    ///
    /// Populated by M2 **Stage 4**; not scored.
    pub dir: Option<String>,
    /// Reader comments extracted by the Trafilatura comments pipeline
    /// (`main_extractor.extract_comments`, `main_extractor.py:657-688`).
    /// Empty `""` when the document carried no recognised comment section
    /// (Reddit-style `commentlist`, vBulletin `comment-list`, etc.) — every
    /// non-comment page therefore lands here with `comments == ""`, which
    /// is also the value M2-Readability-path callers see via
    /// [`extract_via_readability`] (the M2 port has no comments concept).
    ///
    /// **M3 Stage 9 additive field.** Default is `""` (empty); old callers
    /// using `..Extracted::default()` are forward-compatible.
    pub comments: String,
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
/// **M1**: only `NotImplemented`. **M2 Stage 4** (this version, HLD §7.6) adds
/// [`ContentTooShort`](Self::ContentTooShort) — the deliberately-anticipated
/// new variant whose introduction fires the documented harness compile-fence
/// in `benchmark/src/crate_run.rs`. The enum is deliberately **not**
/// `#[non_exhaustive]`: the in-workspace differential harness matches it
/// *exhaustively without a wildcard* on purpose, so that adding a variant
/// breaks the harness build and forces a conscious decision at the Bug-E2
/// site rather than silently laundering the new variant into `crate_error`.
///
/// DEC-3: the `Display`/`Error` impls below remain **hand-written** rather
/// than derived via `thiserror`. With two variants and a single dynamic value
/// to render, `thiserror` is still ~10 lines of code-saved at the cost of a
/// proc-macro dependency, so the deferral persists (mirrors how the
/// dependency is still under review for "when does it actually pay back").
#[derive(Debug)]
pub enum ExtractError {
    /// The extraction algorithm is not implemented yet (Milestone-1 floor).
    /// The differential harness maps this to a first-class `not_implemented`
    /// status, distinct from a crate error and from an empty-but-ok result
    /// (harness HLD §5).
    ///
    /// **As of M2 Stage 1a** the production happy path no longer returns
    /// this. The variant is preserved so consumers that match it explicitly
    /// (the harness `crate_run.rs` did so by intention) still compile, and
    /// to leave a clean upgrade door if some future degraded mode wants it
    /// back. Stage 4 introduces [`ContentTooShort`](Self::ContentTooShort)
    /// as the FIRST genuinely-returned error variant on a successful parse.
    NotImplemented,
    /// The extraction completed (`Ok` would have produced a real article)
    /// but the **word count was strictly below `Options.min_word_count`**.
    /// Fired ONLY when `min_word_count > 0`; the default-Options path
    /// (`min_word_count == 0`) never produces this — `extract` /
    /// `extract_with(default)` therefore remain byte-identical-observable
    /// to the pre-Stage-4 surface.
    ///
    /// Carries both the actual word count and the threshold so consumers
    /// can surface a precise reason in their telemetry. **Distinct from
    /// `Ok(text: "")`** (Bug-E2: an empty extraction is a valid `Ok`, not
    /// an error) and from `NotImplemented` (the M1 floor).
    ///
    /// M2 Stage 4 (HLD §7.6) — the fence-firing event the harness's
    /// `crate_run.rs:240-259` doctrine anticipates.
    ContentTooShort {
        /// `metrics::word_count`-style count over the produced text (counted
        /// inside the crate using `split_whitespace`; the harness recomputes
        /// with its own tokenizer and does not trust this value, per the
        /// crate-level docs).
        word_count: usize,
        /// The threshold the caller passed in [`Options::min_word_count`].
        /// Always `>= 1` when this variant is produced (since
        /// `min_word_count == 0` short-circuits the test).
        threshold: usize,
    },
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::NotImplemented => {
                f.write_str("content extraction is not implemented yet (Milestone 1 floor)")
            }
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                write!(
                    f,
                    "extracted content too short: {word_count} words \
                     (threshold: {threshold})"
                )
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
/// M2 Stage 4: the default path **cannot** return an error
/// ([`ExtractError::ContentTooShort`] only fires when `min_word_count > 0`,
/// which the default path leaves at `0`). A genuinely-empty extraction is a
/// valid `Ok` per Bug-E2 (HLD §7.1).
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
/// * [`ExtractError::ContentTooShort`] — only when `opts.min_word_count > 0`
///   and the extracted text's word count is strictly less than that
///   threshold. The default-Options path (`min_word_count == 0`) **never**
///   produces this — `extract == extract_with(default)` remains
///   byte-identical-observable to the M1/M2-Stage-3 surface.
pub fn extract_with(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<Extracted, ExtractError> {
    // M3 Stage 9 (HLD §7.6, THE M3 FINALE) — `extract_with` is now the public
    // entry-point into the **full Trafilatura cascade**. Mirrors Python's
    // `core.bare_extraction` (core.py:130-358):
    //
    //   1. metadata = extract_metadata(html, base_url, extensive=True, [])
    //   2. body     = bare_extraction_with_cascade(html, &cleaning_opts)
    //                  // = tree_cleaning + convert_tags + own arm
    //                  //   (extract_content) + compare_extraction
    //                  //   (readability_fork + jusText cascade) +
    //                  //   sanitize_tree (post-pass)
    //   3. comments = extract_comments on a separately-cleaned tree
    //   4. assemble Extracted (mapping Metadata.* → Extracted.* fields)
    //
    // The M2 Readability port is preserved verbatim under
    // `extract_via_readability` — it is no longer the default but remains
    // available for callers who depend on that specific extraction shape.
    //
    // `base_url` plumbs through cleaning::Options.url so the cascade's jusText
    // arm can use it as a language/source hint (settings.py:91/155-158).
    // Relative-URL resolution proper is HLD §7.7 deferred (out of M3 scope).

    // 1. Metadata — Trafilatura's extract_metadata orchestrator
    //    (metadata.py:482-589). Parses internally, walks OG / meta-name /
    //    itemprop / JSON-LD / canonical-URL / date / cats-tags / license.
    let metadata =
        trafilatura::metadata::extract_metadata(html, base_url, true, &[]);

    // 2. Body extraction via the cascade. Mirrors core.bare_extraction's
    //    own → readability → jusText path (`trafilatura_sequence`,
    //    core.py:101-127) plus the `compare_extraction` arbiter
    //    (external.py:45-108) plus the `sanitize_tree` post-pass
    //    (external.py:163-190). Stage 4d landed this entry-point; Stage 9
    //    consumes it from the public API.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // The cascade's `sanitize_tree` already trimmed + collapsed the text
    // via `' '.join(itertext()) + trim()` (external.py:189). We re-derive
    // the final `text` by walking the returned `<body>` here — same
    // semantics, no second-pass mutation. An empty body → empty text
    // (Bug-E2: `Ok` with `text == ""` is the valid outcome, never an
    // error).
    let text = match &body_opt {
        Some(body) => {
            let raw = readability::dom::text_content(body);
            trafilatura::utils::trim(&raw)
        }
        None => String::new(),
    };

    // `opts.include_html` (M2 Stage 4): when true, serialise the body's
    // sanitised tree into the `html` field. M2 used Readability's eager
    // serializer; Stage 9 uses `dom::serialize_converted_tree` (the
    // Stage-1b facade) on the cascade's body. Skipped when extraction
    // failed (Bug-E2: an empty body has no useful HTML to surface).
    let html_field = if opts.include_html {
        body_opt.as_ref().map(readability::dom::serialize_converted_tree)
    } else {
        None
    };

    // 3. Comments extraction (M3 Stage 8: `extract_comments`,
    //    main_extractor.py:657-688). Re-parses + re-cleans the original
    //    HTML — necessary because Stage 2 cascade above CONSUMED its DOM
    //    (the `rcdom` Drop quirk, HLD §m-3). The double-parse cost is the
    //    documented Stage-9 simplicity tradeoff; a future stage can lift
    //    the comments call into the cascade orchestrator if perf demands
    //    it.
    let comments = extract_comments_from_html(html, &cleaning_opts);

    // 4. Assemble the public Extracted. Mapping:
    //    - Metadata.title         → Extracted.title
    //    - Metadata.author        → Extracted.byline
    //    - Metadata.description   → Extracted.excerpt
    //    - Metadata.site_name     → Extracted.site_name
    //    - Metadata.date          → Extracted.published_time
    //    - Metadata.url           → Extracted.canonical_url
    //    - Metadata.language      → Extracted.language
    //
    //    Metadata fields without a 1:1 Extracted slot
    //    (categories / tags / image / pagetype / license / hostname) are
    //    still computed and remain accessible via the
    //    `trafilatura::metadata::extract_metadata` infrastructure surface;
    //    they are intentionally NOT added to the public Extracted struct
    //    here to keep the M3 finale backward-compatible (only `comments`
    //    is added). Future MAJOR versions may surface them directly.
    let word_count = text.split_whitespace().count();
    let extracted = Extracted {
        title: metadata.title,
        text,
        html: html_field,
        word_count,
        canonical_url: metadata.url,
        language: metadata.language,
        byline: metadata.author,
        excerpt: metadata.description,
        site_name: metadata.site_name,
        published_time: metadata.date,
        // `dir` (text direction) is a Mozilla-Readability concept that
        // Trafilatura's metadata pipeline does not extract. Until a Stage-9
        // follow-on wires `<html dir="...">` lookup into Metadata, this
        // remains `None` for the Trafilatura path. Callers needing `dir`
        // can opt into the M2 path via `extract_via_readability`.
        dir: None,
        comments,
    };

    // M2 Stage 4 (HLD §7.6) — `min_word_count`. The check fires AFTER the
    // extraction succeeds; an empty `Ok` (Bug-E2) becomes `ContentTooShort`
    // when the caller demanded a positive minimum, NOT silent emptiness.
    // This is the documented harness compile-fence event (the new variant
    // breaks `crate_run.rs`'s exhaustive no-wildcard match — by design).
    if opts.min_word_count > 0 && extracted.word_count < opts.min_word_count {
        return Err(ExtractError::ContentTooShort {
            word_count: extracted.word_count,
            threshold: opts.min_word_count,
        });
    }

    Ok(extracted)
}

/// Helper: parse + tree_cleaning + convert_tags + extract_comments on a
/// fresh DOM. Returns the joined trimmed comments text (`""` when no
/// comment section was found). Mirrors Python's
/// `extract_comments(cleaned_tree, options)` callsite at
/// `core.py:288-290`, but on a freshly-parsed DOM rather than sharing the
/// cascade's tree (the rcdom Drop quirk forbids cross-fn sharing of a
/// `Dom`).
fn extract_comments_from_html(
    html: &str,
    cleaning_opts: &trafilatura::cleaning::Options,
) -> String {
    let dom = readability::dom::Dom::parse(html);
    let Some(html_root) = dom.root_element() else {
        return String::new();
    };
    trafilatura::cleaning::tree_cleaning(&html_root, cleaning_opts);
    trafilatura::cleaning::convert_tags(&html_root, cleaning_opts);
    let Some(body) = dom.body() else {
        return String::new();
    };
    let (cbody, ctext, _) =
        trafilatura::main_extractor::extract_comments(&body, cleaning_opts);
    // Keep `cbody` and `dom` alive until the function exits — rcdom Drop
    // quirk: dropping `dom` iteratively drains every descendant's children
    // Vec, even when the caller still holds a NodeRef. We need only the
    // text, which is already a fresh String, so `cbody`/`dom` can drop
    // here cleanly.
    let _ = cbody;
    drop(dom);
    ctext
}

/// Extract via the **M2 Mozilla Readability port** (the previous default).
///
/// This is the pre-Stage-9 extraction path preserved verbatim. The M3
/// Stage 9 finale shifts the default of [`extract`] / [`extract_with`] to
/// the Trafilatura pipeline; callers depending on the M2 Readability
/// shape — Mozilla `_grabArticle` + `_prepArticle` + the JSON-LD title
/// rescue — can opt back in here without behavioural drift versus the
/// M2 0.4.x / 0.5.x / 0.6.x / 0.7.x / 0.8.x / 0.9.x line.
///
/// Honors `opts.include_html` and `opts.min_word_count` identically to
/// the pre-Stage-9 `extract_with`. `base_url` remains informational only.
///
/// # Errors
///
/// Same as [`extract_with`]: only [`ExtractError::ContentTooShort`] when
/// `opts.min_word_count > 0` and the produced text fails the threshold.
pub fn extract_via_readability(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<Extracted, ExtractError> {
    let _ = base_url;

    let extracted = match readability::Readability::new_from_html(html)
        .include_html(opts.include_html)
        .parse()
    {
        Some(article) => {
            let text = article.text_content;
            let title = if article.title.is_empty() {
                None
            } else {
                Some(article.title)
            };
            let word_count = text.split_whitespace().count();
            Extracted {
                title,
                text,
                html: article.content_html,
                word_count,
                canonical_url: article.canonical_url,
                language: article.lang,
                byline: article.byline,
                excerpt: article.excerpt,
                site_name: article.site_name,
                published_time: article.published_time,
                dir: article.dir,
                comments: String::new(),
            }
        }
        None => Extracted::default(),
    };

    if opts.min_word_count > 0 && extracted.word_count < opts.min_word_count {
        return Err(ExtractError::ContentTooShort {
            word_count: extracted.word_count,
            threshold: opts.min_word_count,
        });
    }

    Ok(extracted)
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
        // empty `Ok` on the DEFAULT path — never `NotImplemented`, never an
        // error, never fabricated. (Stage 4 retains the default-Options
        // empty-Ok behaviour; `min_word_count == 0` short-circuits the
        // threshold test even on an empty extraction.)
        let e = extract_with("<html><body>   </body></html>", None, &Options::default())
            .expect("empty extraction is a valid Ok on default path");
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
            byline: Some("Author Name".to_string()),
            excerpt: Some("a short excerpt".to_string()),
            site_name: Some("Example Site".to_string()),
            published_time: Some("2024-01-02".to_string()),
            dir: Some("ltr".to_string()),
            comments: String::new(),
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
        assert_eq!(e.byline.as_deref(), Some("Author Name"));
        assert_eq!(e.excerpt.as_deref(), Some("a short excerpt"));
        assert_eq!(e.site_name.as_deref(), Some("Example Site"));
        assert_eq!(e.published_time.as_deref(), Some("2024-01-02"));
        assert_eq!(e.dir.as_deref(), Some("ltr"));
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
            byline: None,
            excerpt: None,
            site_name: None,
            published_time: None,
            dir: None,
            comments: String::new(),
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

    // ====== M2 Stage 4 (HLD §7.6) — new public API behaviour tests.

    #[test]
    fn min_word_count_fires_content_too_short_when_text_under_threshold() {
        // A genuinely-empty page → empty Ok at default path; with
        // `min_word_count = 1` the empty text fails the threshold and the
        // new ExtractError::ContentTooShort variant fires.
        let opts = Options {
            include_html: false,
            min_word_count: 1,
        };
        let err = extract_with("<html><body>   </body></html>", None, &opts).expect_err("must Err");
        match err {
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                assert_eq!(word_count, 0);
                assert_eq!(threshold, 1);
            }
            other => panic!("expected ContentTooShort, got {other:?}"),
        }
    }

    #[test]
    fn min_word_count_does_not_fire_when_threshold_zero() {
        // The default-Options invariant: min_word_count=0 must NEVER produce
        // ContentTooShort regardless of extracted text length. An empty
        // extraction stays Ok with "" — Bug-E2.
        let opts = Options::default();
        let e = extract_with("<html><body></body></html>", None, &opts)
            .expect("default path must always Ok");
        assert_eq!(e.text, "");
    }

    #[test]
    fn min_word_count_passes_when_text_meets_threshold() {
        // A real article well past the threshold: Ok, no error.
        let html = "<html><head><title>Title Word Five Six Seven</title></head>\
            <body><article><p>This is a real readable paragraph with quite a few words \
            in it because the unlikely-candidate strip cares about minimum body length.</p>\
            </article></body></html>";
        let opts = Options {
            include_html: false,
            min_word_count: 5,
        };
        let e = extract_with(html, None, &opts).expect("threshold must be met");
        assert!(e.word_count >= 5);
    }

    #[test]
    fn include_html_populates_html_field_when_true() {
        // A page that should extract. With include_html=true the html field
        // is populated with serialized articleContent; the text field is
        // unchanged from the default.
        let html = "<html><head><title>Title Word Five Six Seven</title></head>\
            <body><article><p>This is a real readable paragraph with quite a few words \
            in it because the unlikely-candidate strip cares about minimum body length.</p>\
            </article></body></html>";

        let default = extract_with(html, None, &Options::default()).expect("default extracts");
        assert!(default.html.is_none(), "default include_html=false ⇒ None");

        let opts = Options {
            include_html: true,
            min_word_count: 0,
        };
        let with_html = extract_with(html, None, &opts).expect("extracts");
        assert!(
            with_html.html.is_some(),
            "include_html=true ⇒ html field populated"
        );
        // The text MUST be unchanged: the html serialization is additive and
        // does not feed back into the scored text path.
        assert_eq!(default.text, with_html.text);
    }

    #[test]
    fn include_html_false_is_byte_identical_to_default_path() {
        // The "include_html=false is the harness's path" invariant: an
        // include_html=false call must equal an Options::default() call
        // exactly. (The harness path is Options::default() via extract().)
        let html = "<html><head><title>Sample Doc Long Enough</title></head>\
            <body><div><p>A real readable paragraph well past twenty-five characters \
            of genuine prose content.</p></div></body></html>";
        let a = extract_with(html, None, &Options::default()).expect("ok");
        let b = extract_with(
            html,
            None,
            &Options {
                include_html: false,
                min_word_count: 0,
            },
        )
        .expect("ok");
        assert_eq!(a, b, "include_html=false ≡ default");
    }

    #[test]
    fn content_too_short_display_carries_numbers() {
        let err = ExtractError::ContentTooShort {
            word_count: 3,
            threshold: 10,
        };
        let msg = err.to_string();
        // Must include both numbers so a consumer can diagnose without
        // re-deriving anything.
        assert!(msg.contains('3') && msg.contains("10"), "got {msg:?}");
        assert!(
            msg.to_lowercase().contains("too short") || msg.to_lowercase().contains("threshold"),
            "got {msg:?}"
        );
    }

    #[test]
    fn metadata_byline_populates_extracted_byline_field() {
        // Stage 4: a page with an og/article author meta yields a
        // populated `Extracted.byline` (previously always None).
        let html = r#"<html><head>
            <meta property="og:title" content="Real Article Title Goes Here">
            <meta property="article:author" content="Jane Author">
            <title>X</title>
            </head><body><article><p>A real readable paragraph with enough \
            text to extract.</p></article></body></html>"#;
        let e = extract(html, None).expect("ok");
        assert_eq!(e.byline.as_deref(), Some("Jane Author"));
    }

    #[test]
    fn metadata_lang_populates_extracted_language_field() {
        let html = "<html lang=\"en-GB\"><head><title>X</title></head>\
            <body><p>some text</p></body></html>";
        let e = extract(html, None).expect("ok");
        assert_eq!(e.language.as_deref(), Some("en-GB"));
    }

    #[test]
    fn metadata_canonical_populates_extracted_canonical_url_field() {
        let html = r#"<html><head>
            <title>X</title>
            <link rel="canonical" href="https://example.com/x">
            </head><body><p>some text</p></body></html>"#;
        let e = extract(html, None).expect("ok");
        assert_eq!(e.canonical_url.as_deref(), Some("https://example.com/x"));
    }

    // ====== M3 Stage 9 (HLD §7.6) — Trafilatura pipeline public-surface tests.

    /// Stage 9 brief test #1 — a minimal article HTML yields an `Ok` with
    /// non-empty text. Drives the full cascade through the public
    /// `extract` entry point. The cascade's own-arm `extract_content` may
    /// fall back to readability / jusText on tiny inputs; we only assert
    /// that *some* text was extracted (the Trafilatura cascade is
    /// non-strict in scope but never returns an error for a well-formed
    /// short article).
    #[test]
    fn extract_returns_ok_for_simple_article() {
        let html = "<html><head><title>An Article</title></head><body>\
            <article><p>This is a real readable paragraph with quite a few words \
            in it because the unlikely-candidate strip cares about minimum body length, \
            and we want the cascade to surface SOMETHING from the Trafilatura pipeline. \
            Adding more text here so the various length-threshold gates don't reject this \
            fixture outright; the M3 cascade has min_extracted_size=250 by default and \
            we want to clear it comfortably.</p></article></body></html>";
        let e = extract(html, None).expect("simple article must extract");
        assert!(!e.text.is_empty(), "expected non-empty text, got {:?}", e.text);
        assert_eq!(e.title.as_deref(), Some("An Article"));
    }

    /// Stage 9 brief test #2 — OG / meta-name tags drive the populated
    /// metadata fields. Pins the Metadata→Extracted mapping documented in
    /// `extract_with`.
    #[test]
    fn extract_populates_metadata_fields_from_og_tags() {
        let html = r#"<html><head>
            <meta property="og:title" content="OG Title Wins Over Title Element">
            <meta property="og:description" content="A brief description for OG">
            <meta property="og:site_name" content="Example Site">
            <meta property="article:author" content="Jane Author">
            <title>Fallback Title</title>
            </head><body><article>
            <p>A real readable paragraph with enough words to extract; lorem ipsum dolor
            sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut
            labore et dolore magna aliqua ut enim ad minim veniam quis nostrud
            exercitation.</p>
            </article></body></html>"#;
        let e = extract(html, None).expect("ok");
        assert_eq!(
            e.title.as_deref(),
            Some("OG Title Wins Over Title Element"),
            "og:title beats <title>"
        );
        assert_eq!(e.byline.as_deref(), Some("Jane Author"));
        assert_eq!(e.excerpt.as_deref(), Some("A brief description for OG"));
        assert_eq!(e.site_name.as_deref(), Some("Example Site"));
    }

    /// Stage 9 brief test #3 — JSON-LD drives metadata when OG / meta-name
    /// tags are ABSENT. Mirrors `metadata.py:519-520` `extract_meta_json`
    /// orchestration position.
    #[test]
    fn extract_uses_jsonld_when_og_absent() {
        let html = r#"<html><head>
            <title>Fallback Title</title>
            <script type="application/ld+json">
            {
                "@context": "https://schema.org",
                "@type": "Article",
                "headline": "JSON-LD Headline Wins",
                "author": {"@type": "Person", "name": "Alice JSONLD"},
                "datePublished": "2024-06-01"
            }
            </script>
            </head><body><article>
            <p>A real readable paragraph with enough words to extract; lorem ipsum dolor
            sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut
            labore et dolore magna aliqua ut enim ad minim veniam quis nostrud
            exercitation.</p>
            </article></body></html>"#;
        let e = extract(html, None).expect("ok");
        assert_eq!(
            e.title.as_deref(),
            Some("JSON-LD Headline Wins"),
            "JSON-LD headline beats <title>"
        );
        assert_eq!(e.byline.as_deref(), Some("Alice JSONLD"));
        assert_eq!(
            e.published_time.as_deref(),
            Some("2024-06-01"),
            "JSON-LD datePublished -> published_time"
        );
    }

    /// Stage 9 brief test #4 — Bug-E2 preserved on the new default path.
    /// `<html><body></body></html>` → `Ok(Extracted)` with empty text;
    /// NEVER an error (mirrors the M2 contract the parent brief pins).
    #[test]
    fn extract_handles_empty_html() {
        let e = extract("<html><body></body></html>", None)
            .expect("empty body must be Ok per Bug-E2");
        assert_eq!(e.text, "");
        assert_eq!(e.word_count, 0);
        // `comments` is the new Stage-9 additive field — must default to "".
        assert_eq!(e.comments, "");
    }

    /// Stage 9 brief test #5 — pin the parent-brief invariant: `extract`
    /// must be byte-for-byte equivalent to `extract_with(default)` on
    /// every input.
    #[test]
    fn extract_invariant_default_options_match_no_options() {
        let cases = [
            "",
            "<html><body></body></html>",
            "<html><head><title>T</title></head><body><p>hello world</p></body></html>",
            r#"<html><head><meta property="og:title" content="X"></head>
               <body><article><p>body body body body body body body body body body body body
               body body body body body body body body body body body body body body body body
               body body body body body body body body body body body body body body body</p>
               </article></body></html>"#,
        ];
        for html in cases {
            let a = extract(html, None);
            let b = extract_with(html, None, &Options::default());
            assert_eq!(
                a.is_ok(),
                b.is_ok(),
                "Ok-ness diverged for {html:?}"
            );
            if let (Ok(a), Ok(b)) = (a, b) {
                assert_eq!(a, b, "Extracted diverged for {html:?}");
            }
        }
    }

    /// Stage 9 brief test #6 — when the own arm yields a short
    /// extraction, the cascade's readability / jusText arms can rescue.
    /// We don't pin WHICH arm wins (that's an implementation detail of
    /// `compare_extraction`'s arbiter); we only pin that *some* text comes
    /// out of a page with abundant paragraph content. Documents that
    /// Stage 9 is wired into the full cascade, not the own-arm only.
    #[test]
    fn extract_falls_back_to_readability_on_short_own_extraction() {
        // A page with many <p> elements but no single <article> — the
        // own arm's BODY_XPATH may yield a thin result; readability picks
        // up the bulk paragraphs via div-to-p transformation.
        let mut html = String::from("<html><head><title>Multi-P Page</title></head><body>");
        for _ in 0..20 {
            html.push_str(
                "<p>This is a long readable paragraph with substantial text content so the \
                cascade's classifier can reliably pull it through. Lorem ipsum dolor sit amet, \
                consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et \
                dolore magna aliqua.</p>",
            );
        }
        html.push_str("</body></html>");
        let e = extract(&html, None).expect("ok");
        assert!(
            e.text.len() > 200,
            "expected substantive cascade extraction, got {} chars: {:?}",
            e.text.len(),
            e.text
        );
    }

    /// Stage 9 brief test #7 — `<html lang="...">` populates
    /// Extracted.language via Trafilatura's metadata pipeline.
    #[test]
    fn extract_populates_language_from_html_lang() {
        let html = "<html lang=\"en\"><head><title>X</title></head>\
            <body><p>some text</p></body></html>";
        let e = extract(html, None).expect("ok");
        assert_eq!(e.language.as_deref(), Some("en"));
    }

    /// Stage 9 brief test #8 — malformed HTML must not panic. The
    /// html5ever parser handles malformed input by inserting implied
    /// elements; the cascade must consume that without unimplemented! /
    /// panic / infinite loop. Empty extraction is a valid `Ok` (Bug-E2).
    #[test]
    fn extract_doesnt_panic_on_malformed_html() {
        let cases = [
            "<html><body><div><p>unclosed",
            "<<<>><body><p>nested broken<<<</p></body>",
            "<html><body><a href=\"unclosed quote</body>",
            "<!DOCTYPE garbage><html><body>",
            "",
        ];
        for html in cases {
            let result = extract(html, None);
            assert!(
                result.is_ok(),
                "malformed HTML must still be Ok, got Err for {html:?}: {:?}",
                result.err()
            );
        }
    }

    /// Stage 9 sanity — the M2 Readability path is still reachable via
    /// `extract_via_readability`. Pin the byte-faithful old default's
    /// availability so the M3 finale doesn't silently DROP the previous
    /// extraction shape (which would be a major regression for any
    /// caller pinned to M2's `_grabArticle` semantics).
    #[test]
    fn extract_via_readability_remains_available_for_m2_callers() {
        const SAMPLE_HTML: &str =
            "<html><head><title>T</title></head>\
             <body><article><p>hello world</p></article></body></html>";
        let e = extract_via_readability(SAMPLE_HTML, None, &Options::default())
            .expect("M2 Readability path must remain available");
        assert!(e.text.contains("hello world"), "M2 path body text: {:?}", e.text);
        assert_eq!(e.title.as_deref(), Some("T"));
        // The M2 path has no comments concept.
        assert_eq!(e.comments, "");
    }
}
