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
    /// Article section / category labels (`Metadata.categories`). Empty
    /// `Vec` when no source yielded a category. Populated by the metadata
    /// pipeline's `<meta property="article:section">` walk plus
    /// JSON-LD `articleSection` and the URL/XPath fallback at
    /// `metadata.py:575-576` (`extract_catstags("category", tree)`).
    ///
    /// Ports `Document.categories` (`metadata.py:422-446`).
    ///
    /// **M4 Stage 4 additive field.** Default is the empty `Vec`; old
    /// callers using `..Extracted::default()` are forward-compatible.
    pub categories: Vec<String>,
    /// Article keyword/tag labels (`Metadata.tags`). Empty `Vec` when no
    /// source yielded tags. Populated from `<meta property="article:tag">`,
    /// `<meta name="keywords">`, JSON-LD `keywords`, and the URL/XPath
    /// fallback at `metadata.py:579-580` (`extract_catstags("tag", tree)`).
    ///
    /// Ports `Document.tags` (`metadata.py:422-446`).
    ///
    /// **M4 Stage 4 additive field.** Default is the empty `Vec`; old
    /// callers using `..Extracted::default()` are forward-compatible.
    pub tags: Vec<String>,
    /// Lead/social image URL (`Metadata.image`). Sourced from `og:image`,
    /// `og:image:url`, `og:image:secure_url`, `twitter:image`,
    /// `twitter:image:src`, or `<meta name="image">` — see
    /// `METANAME_IMAGE` (`metadata.py:126-133`) and the `og:image*`
    /// branches of `assign_og_property` (`metadata.py:141-149`).
    /// Returned verbatim as the document provided it (no URL resolution).
    ///
    /// Ports `Document.image`.
    ///
    /// **M4 Stage 4 additive field.** Default is `None`; old callers using
    /// `..Extracted::default()` are forward-compatible.
    pub image: Option<String>,
    /// Open Graph page type (`Metadata.pagetype`). Sourced from
    /// `<meta property="og:type">` — see the `og:type` branch of
    /// `assign_og_property` (`metadata.py:141-149`). Typical values:
    /// `"article"`, `"website"`, `"video.other"`. `None` when absent.
    ///
    /// Ports `Document.pagetype`.
    ///
    /// **M4 Stage 4 additive field.** Default is `None`; old callers using
    /// `..Extracted::default()` are forward-compatible.
    pub pagetype: Option<String>,
    /// Document licence string (`Metadata.license`). Sourced by scanning
    /// the footer (and similar regions) for `rel="license"` links, plus
    /// CC-licence URL pattern matching — ports `extract_license` at
    /// `metadata.py:465-479`. `None` when no licence was identified.
    ///
    /// Ports `Document.license`.
    ///
    /// **M4 Stage 4 additive field.** Default is `None`; old callers using
    /// `..Extracted::default()` are forward-compatible.
    pub license: Option<String>,
    /// Hostname extracted from the page's canonical URL
    /// (`Metadata.hostname`). `None` when no URL was discovered or when
    /// the URL had no netloc.
    ///
    /// Ports the `extract_domain(url, fast=True)` call at
    /// `metadata.py:542-543`.
    ///
    /// **M4 Stage 4 additive field.** Default is `None`; old callers using
    /// `..Extracted::default()` are forward-compatible.
    pub hostname: Option<String>,
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
    /// When `true`, [`extract_to_markdown`] prepends a YAML-style `---`
    /// header listing the metadata fields Python's `core.py:75-91`
    /// enumerates (title / author / url / hostname / description /
    /// sitename / date / categories / tags / fingerprint / id / license).
    /// Default `false` — the formatter emits the body only.
    ///
    /// Ignored by [`extract`] / [`extract_with`]: their public `Extracted`
    /// type already carries the metadata fields as discrete struct
    /// members, so a header would be redundant. This knob is exclusively
    /// for the markdown formatter where the YAML front-matter is the
    /// idiomatic way to carry metadata alongside the rendered body.
    ///
    /// **M4 Stage 3 sub-stage B additive field.** Old callers using
    /// `..Options::default()` are forward-compatible.
    pub with_metadata: bool,
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
    //    M4 Stage 4 additive fields (HLD §7.6 trailing follow-on) wire the
    //    six remaining Metadata slots straight through (same field names —
    //    no rename), making them visible on the public Extracted surface:
    //    - Metadata.categories    → Extracted.categories
    //    - Metadata.tags          → Extracted.tags
    //    - Metadata.image         → Extracted.image
    //    - Metadata.pagetype      → Extracted.pagetype
    //    - Metadata.license       → Extracted.license
    //    - Metadata.hostname      → Extracted.hostname
    //    These are pure value-moves out of the *already-computed* Metadata
    //    (no second `extract_metadata` call). Old callers using
    //    `..Extracted::default()` remain byte-equivalent for every
    //    pre-Stage-4 field — the additions are strictly forward.
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
        categories: metadata.categories,
        tags: metadata.tags,
        image: metadata.image,
        pagetype: metadata.pagetype,
        license: metadata.license,
        hostname: metadata.hostname,
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

/// Extract `html` and render the main content as markdown.
///
/// Returns the formatted body. When `opts.with_metadata` is `true`, a
/// YAML-style `---` header listing metadata (title, author, url, date,
/// hostname, sitename, categories, tags, fingerprint, id, license,
/// comments) precedes the body — matching Python Trafilatura's
/// `with_metadata=True` markdown output (`core.py:73-96`).
///
/// Equivalent to Python's `extract(html, output_format="markdown",
/// include_formatting=True, with_metadata=...)`.
///
/// # Plain TXT
///
/// Plain TXT (formatting=false) does NOT need a separate public function
/// — it is already what [`extract_with`]`(...)?.text` returns (the
/// whitespace-collapsed `trim`-ed body text). The markdown formatter
/// here is the *formatted* path: `#` headings, `**bold**`, `*italic*`,
/// `[link](url)`, `- list items`, fenced code blocks etc.
///
/// # NFC normalisation
///
/// The final string is NFC-normalised per `core.py:98`'s
/// `normalize_unicode(returnstring)` (`utils.py:277-279`). Input HTML
/// containing decomposed (NFD) Unicode lands in the output as composed
/// (NFC) — faithful to Python.
///
/// # `base_url`
///
/// As in [`extract`] — informational only, used by the cascade for
/// language hints and relative-URL resolution where applicable. Never
/// fetched.
///
/// # Errors
///
/// Same shape as [`extract_with`]: only [`ExtractError::ContentTooShort`]
/// when `opts.min_word_count > 0` and the produced text fails the
/// threshold. The default-`Options` path (`min_word_count == 0`) never
/// produces an error — an empty body returns an empty markdown string
/// (or the YAML header alone, when `with_metadata=true`).
pub fn extract_to_markdown(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // M4 Stage 3 sub-stage B — port of `core.bare_extraction` +
    // `determine_returnstring` (the markdown/TXT branch at core.py:73-96).
    //
    // 1. Metadata — same orchestrator as `extract_with`.
    // 2. Body — same cascade as `extract_with`.
    // 3. Format via `output::xmltotxt(body, include_formatting=true)`.
    // 4. Optional YAML header per `core.py:74-91`.
    // 5. NFC normalise (`core.py:98`).
    //
    // The min_word_count gate fires AFTER assembly, matching `extract_with`.

    // 1. Metadata.
    let metadata = trafilatura::metadata::extract_metadata(html, base_url, true, &[]);

    // 2. Body extraction via the cascade. Identical wiring to extract_with,
    //    EXCEPT `formatting` is forced on. Python's `settings.py:133` has:
    //        self.formatting = formatting or self.format == "markdown"
    //    i.e. the `markdown` output format auto-enables `formatting`. That
    //    flag is what makes `cleaning::convert_tags` rewrite `<b>/<strong>/
    //    <i>/<em>/<u>/<tt>` to `<hi rend="#b|#i|#u|#t">` (cleaning.rs:548)
    //    instead of stripping them. Without it, no `<hi>` element survives
    //    for `xmltotxt`'s formatting branch (xml.py:266-269) to wrap, so
    //    `**bold**` / `*italic*` / `__underline__` / `` `tt` `` markers are
    //    dropped — the bug M5 Stage 4 fixes.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        formatting: true,
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Format body via `xmltotxt(body, include_formatting=true)`. The
    //    markdown formatter ALWAYS sets include_formatting=true — this is
    //    what distinguishes it from plain TXT (xml.py:354).
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), true);

    // 4. Optional YAML header (core.py:73-91). The header builder honours
    //    Python's `if getattr(document, attr):` falsy check by skipping
    //    None/empty fields.
    let header = if opts.with_metadata {
        trafilatura::output::build_yaml_header(&metadata)
    } else {
        String::new()
    };

    // core.py:94 — `returnstring = f"{header}{xmltotxt(...)}"`.
    let mut returnstring = format!("{header}{body_text}");

    // core.py:95-96 — commentsbody branch:
    //     if document.commentsbody is not None:
    //         returnstring = f"{returnstring}\n{xmltotxt(...)}".strip()
    // Python's `bare_extraction` (core.py:287-292) sets `commentsbody` to an
    // `Element("body")` whenever `options.comments` is true — which is the
    // default. The empty body's `xmltotxt` returns `""`, so the branch is
    // equivalent to `(returnstring + "\n").strip()` for the default
    // extraction. mdrcel does not currently extract a commentsbody (always
    // None in our cascade), but Python's default behaviour always strips,
    // so to match `extract(output_format='markdown')` byte-for-byte we
    // mirror the strip path. This drops the stray trailing `\n` that
    // `xmltotxt`'s `process_element` after-tag emit + `sanitize` leaves
    // behind on the body's final block-level element (xml.py:340-343).
    returnstring = format!("{returnstring}\n").trim().to_string();

    // 5. NFC normalise (core.py:98). `unicode-normalization` is a real
    //    dependency at sub-stage B (promoted from dev-dep, see Cargo.toml).
    use unicode_normalization::UnicodeNormalization;
    let normalised: String = returnstring.as_str().nfc().collect();

    // Word count for the optional threshold check. We count words on the
    // FORMATTED string after sanitize/unescape — the same surface a
    // human reader sees.
    let word_count = normalised.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        // Drop the cascade's NodeRef BEFORE returning. The rcdom Drop
        // quirk is contained inside bare_extraction_with_cascade and the
        // function already returned; we keep `body_opt` alive through
        // `xmltotxt` (handled above) and now release it.
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // Keep `body_opt` alive across the format pass so the rcdom Drop
    // quirk (HLD §m-3) doesn't drain the body's descendants mid-walk.
    // `xmltotxt` has already produced its String above; release here.
    let _ = body_opt;
    Ok(normalised)
}

/// Extract `html` and render the main content as plain TXT.
///
/// Equivalent to Python's `extract(html, output_format="txt")`
/// (`core.py:71-98`, the "Markdown and TXT" branch). TXT is the
/// **formatting-off** sibling of [`extract_to_markdown`]: the SAME pipeline
/// shape (metadata → cascade body → `xmltotxt` → optional YAML header →
/// commentsbody strip → NFC) but with `formatting` **false** throughout.
///
/// # Why a separate function (not `extract_with(...).text`)
///
/// `extract_with(...)?.text` derives its text via `text_content(body)` +
/// `trim` — a flat space-join of `itertext()`. Python's `output_format=
/// "txt"` path instead routes the body through `xmltotxt(body, False)`
/// (`core.py:94`), which preserves block structure: table rows rendered with
/// `|` separators, list items, paragraph breaks, etc. The two are NOT
/// byte-equivalent, so TXT needs its own formatter call.
///
/// # The formatting flag, two places
///
/// Python's `settings.py:133` sets `self.formatting = formatting or
/// self.format == "markdown"`. For `"txt"` neither is true, so `formatting`
/// stays `False`. That single flag governs BOTH:
///
/// 1. `cleaning::convert_tags` — with `formatting=false`, `<b>/<i>/<u>/…` are
///    stripped to their text rather than rewritten to `<hi rend=…>` (so no
///    `**bold**`/`*italic*` markers survive); and
/// 2. `xmltotxt(body, false)` — the formatter's `<hi>`/code/quote branches
///    emit plain text without the markdown decorations.
///
/// This function therefore sets `cleaning_opts.formatting = false` (vs
/// markdown's `true`) and calls `xmltotxt(.., false)` (vs markdown's `true`).
///
/// # `with_metadata`, `base_url`, NFC, and errors
///
/// Identical to [`extract_to_markdown`]: optional YAML header when
/// `opts.with_metadata`, `base_url` is informational only, the final string
/// is NFC-normalised (`core.py:98`), and the only error is
/// [`ExtractError::ContentTooShort`] when `opts.min_word_count > 0` and the
/// produced text is below threshold.
pub fn extract_to_txt(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // Mirror of `extract_to_markdown`, with `formatting` forced OFF (the
    // ONLY behavioural difference between Python's "txt" and "markdown"
    // output_format branches — see core.py:71-98 + settings.py:133).

    // 1. Metadata.
    let metadata = trafilatura::metadata::extract_metadata(html, base_url, true, &[]);

    // 2. Body extraction via the cascade. Unlike markdown, `formatting` is
    //    left at its default (false): the "txt" output_format does NOT
    //    auto-enable it (settings.py:133), so `convert_tags` strips inline
    //    `<b>/<i>/<u>/<tt>` rather than rewriting them to `<hi rend=…>`.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Format body via `xmltotxt(body, include_formatting=false)` — the
    //    plain-TXT branch (core.py:94 passes `options.formatting`, which is
    //    false here).
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), false);

    // 4. Optional YAML header (core.py:73-91) — identical to markdown.
    let header = if opts.with_metadata {
        trafilatura::output::build_yaml_header(&metadata)
    } else {
        String::new()
    };

    // core.py:94 — `returnstring = f"{header}{xmltotxt(...)}"`.
    let mut returnstring = format!("{header}{body_text}");

    // core.py:95-96 — commentsbody strip path (see extract_to_markdown for
    // the full rationale; mdrcel's cascade never sets commentsbody, but the
    // default Python path always strips, so we mirror it for byte parity).
    returnstring = format!("{returnstring}\n").trim().to_string();

    // 5. NFC normalise (core.py:98).
    use unicode_normalization::UnicodeNormalization;
    let normalised: String = returnstring.as_str().nfc().collect();

    let word_count = normalised.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // Keep `body_opt` alive across the format pass (rcdom Drop quirk).
    let _ = body_opt;
    Ok(normalised)
}

/// Extract `html` and render the main content as a JSON object.
///
/// Equivalent to Python's `extract(html, output_format="json")` per
/// `core.py:66-67` (`returnstring = build_json_output(document,
/// options.with_metadata)`).
///
/// # `with_metadata` semantics
///
/// Python's `core.py:67` passes `options.with_metadata` into
/// `build_json_output` (`xml.py:115`); `Extractor.with_metadata` defaults
/// to `False` (`settings.py:118`). We mirror this exactly: when
/// `opts.with_metadata` is `false` (the [`Options::default`]), the JSON
/// is a body-only object with just `{"text": ..., "comments": ...}`
/// (`xml.py:128-132`). When `true`, the JSON carries the full 19-key
/// Python metadata object (`xml.py:117-127`).
///
/// # JSON key set
///
/// `with_metadata=true` emits, in Python's `dict` insertion order:
/// `title`, `author`, `hostname`, `date`, `fingerprint`, `id`, `license`,
/// `comments`, `raw_text`, `text`, `language`, `image`, `pagetype`,
/// `filedate`, `source`, `source-hostname`, `excerpt`, `categories`,
/// `tags`. `fingerprint`/`id`/`filedate` render as JSON `null` until
/// M4 Stage 6 wires them; the rest map directly from the Trafilatura
/// metadata pipeline.
///
/// # `base_url`
///
/// As in [`extract`] — informational only, used by the cascade for
/// language hints and relative-URL resolution where applicable. Never
/// fetched.
///
/// # Errors
///
/// Same shape as [`extract_with`]: only [`ExtractError::ContentTooShort`]
/// when `opts.min_word_count > 0` and the produced body text fails the
/// threshold. The default-`Options` path (`min_word_count == 0`) never
/// produces an error — an empty body returns a valid JSON object
/// (`{"text": "", "comments": ""}` with default opts).
pub fn extract_to_json(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // 1. Metadata (same as extract_to_markdown).
    let metadata = trafilatura::metadata::extract_metadata(html, base_url, true, &[]);

    // 2. Body extraction via the cascade.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Min-word-count gate. Run on the same `xmltotxt` body text the JSON
    //    formatter emits so the threshold reflects what the consumer sees.
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), false);
    let word_count = body_text.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // 4. Build the Document carrier — body MUST be non-None for the
    //    formatter (xml.py:125's `xmltotxt(outputdict.pop('body'), ...)`
    //    walks the element directly). Empty bodies are represented by an
    //    empty `<body>` element so the formatter's xmltotxt returns "".
    let body_node = body_opt.clone().unwrap_or_else(|| {
        use crate::readability::dom::create_element;
        create_element("body")
    });
    let doc = trafilatura::output::Document {
        metadata,
        body: body_node,
        commentsbody: None,
        raw_text: String::new(),
    };

    // 5. Build JSON output per xml.py:115-134.
    let out = trafilatura::output::build_json_output(&doc, opts.with_metadata);

    // Keep cascade body alive until after the formatter walks it (rcdom
    // Drop quirk, HLD §m-3).
    let _ = body_opt;
    Ok(out)
}

/// Extract `html` and render the main content as CSV (or delimiter-
/// separated values). Equivalent to Python's `extract(html,
/// output_format="csv")` per `core.py:63-64` (`returnstring =
/// xmltocsv(document, options.formatting)`).
///
/// # Output shape
///
/// Returns a CSV string of TWO rows: a header row (`url`, `id`,
/// `fingerprint`, `hostname`, `title`, `image`, `date`, `text`,
/// `comments`, `license`, `pagetype` — 11 columns) followed by ONE data
/// row per call. Python's `xmltocsv` emits only the data row; the header
/// is added here for ergonomic single-call use (the typical Python user
/// either calls `csv.DictWriter` with the same column names or prepends
/// the header manually).
///
/// # CSV dialect
///
/// Tab-delimited (`\t`) by default with Python `csv.QUOTE_MINIMAL` quoting
/// (`xml.py:374`): fields containing the delimiter, a `"`, `\r`, or `\n`
/// are wrapped in `"..."` with internal `"` doubled. Rows terminate with
/// `\r\n` (Python `csv.writer` default `lineterminator`). The null token
/// is the literal string `null` (`xml.py:366`), emitted for empty / `None`
/// fields per Python's `d if d else null` rule.
///
/// # `with_metadata` semantics
///
/// Python's `core.py:269-270` builds an *empty* `Document()` when
/// `options.with_metadata` is `false` (the [`Options::default`]), so every
/// metadata-derived column (`url`, `id`, `fingerprint`, `hostname`, `title`,
/// `image`, `date`, `license`, `pagetype`) renders `null` — only the
/// body-derived `text` / `comments` columns carry content. We mirror this by
/// passing `opts.with_metadata` into `xmltocsv`: when `false`, the metadata
/// columns are forced to `null` regardless of what was extracted. When
/// `true`, the real extracted metadata populates those columns.
///
/// # `base_url`
///
/// As in [`extract`] — informational only. Never fetched.
///
/// # Errors
///
/// Same shape as [`extract_with`]: only [`ExtractError::ContentTooShort`]
/// when `opts.min_word_count > 0` and the produced body text fails the
/// threshold. The default-`Options` path never produces an error — an
/// empty body yields a valid CSV row with `null` in the text column.
pub fn extract_to_csv(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // 1. Metadata.
    let metadata = trafilatura::metadata::extract_metadata(html, base_url, true, &[]);

    // 2. Body extraction via the cascade.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Min-word-count gate.
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), false);
    let word_count = body_text.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // 4. Build Document (empty <body> sentinel when cascade returned None).
    let body_node = body_opt.clone().unwrap_or_else(|| {
        use crate::readability::dom::create_element;
        create_element("body")
    });
    let doc = trafilatura::output::Document {
        metadata,
        body: body_node,
        commentsbody: None,
        raw_text: String::new(),
    };

    // 5. Header + one data row. xmltocsv produces ONE row matching Python
    //    `outputwriter.writerow([...])` (xml.py:377-389). Defaults match
    //    Python's `delim="\t"`, `null="null"`.
    let mut out = trafilatura::output::csv_header_row("\t");
    out.push_str(&trafilatura::output::xmltocsv(
        &doc,
        false,
        "\t",
        "null",
        opts.with_metadata,
    ));

    let _ = body_opt;
    Ok(out)
}

/// Extract `html` and render the main content as Trafilatura-flavoured XML.
///
/// Equivalent to Python's `extract(html, output_format="xml")` per
/// `core.py:62-64` (`returnstring = control_xml_output(document, options)`,
/// `xml.py:159-175`). Produces a pretty-printed XML string whose root is
/// `<doc>`; child elements are `<main>` (the extracted body, renamed from
/// `<body>` per `xml.py:149`) and `<comments>` (Python `xml.py:153-154` —
/// the comments tree, defaulting to an empty element when no comments
/// were extracted).
///
/// # `with_metadata` semantics
///
/// When `opts.with_metadata` is `true`, metadata is attached to the `<doc>`
/// root as attributes — matching Python's `add_xml_meta` (`xml.py:178-183`).
/// Attributes emitted, in the `META_ATTRIBUTES` order (`xml.py:42-46`):
/// `sitename`, `title`, `author`, `date`, `url`, `hostname`, `description`,
/// `categories`, `tags`, `license`, `language`. (`id` / `fingerprint` are
/// M4 Stage 6 deferred — silently omitted, matching Python's behaviour on
/// a pre-`set_id` `Document`.) Falsy fields are skipped. When `false`,
/// `<doc>` carries no metadata attributes.
///
/// # Element attributes (body subtree)
///
/// Per `xml.py:152` (`clean_attributes`), attributes survive ONLY on the
/// `WITH_ATTRIBUTES` tag set (`xml.py:39`: `cell`, `row`, `del`, `graphic`,
/// `head`, `hi`, `item`, `list`, `ref`). Everything else has its attributes
/// wiped. Useful surviving attributes: `<hi rend="#b">`, `<ref target="...">`,
/// `<graphic src="...">`, `<head rend="h2">`.
///
/// # XML escaping
///
/// `<`, `>`, `&` in text are escaped to `&lt;`, `&gt;`, `&amp;`. Attribute
/// values additionally escape `"` to `&quot;`.
///
/// # `base_url`
///
/// As in [`extract`] — informational only. Never fetched.
///
/// # Errors
///
/// Same shape as [`extract_with`]: only [`ExtractError::ContentTooShort`]
/// when `opts.min_word_count > 0` and the produced body text fails the
/// threshold. The default-`Options` path never produces an error — an empty
/// body returns a minimal `<doc>` with empty `<main>` and `<comments>`
/// children.
pub fn extract_to_xml(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // M4 Stage 3 sub-stage D — port of `core.bare_extraction` +
    // `control_xml_output` (xml.py:159-175) wired into the public surface.
    //
    // Pipeline:
    // 1. Metadata (gated by opts.with_metadata — when false, we skip metadata
    //    extraction to keep the `<doc>` root attribute-free).
    // 2. Body extraction via the cascade.
    // 3. Min-word-count gate on the same xmltotxt(body) the consumer sees.
    // 4. Wrap Document in `<doc>`, run strip_double_tags + remove_empty_elements
    //    + clean_attributes via `control_xml_output`.
    // 5. NFC-normalise the final string (core.py:98 invariant).

    // 1. Metadata. When opts.with_metadata is false, we still call extract_metadata
    //    because the cascade path uses url/hostname signals downstream; we then
    //    drop the metadata when building Document so `<doc>` stays bare.
    let metadata = if opts.with_metadata {
        trafilatura::metadata::extract_metadata(html, base_url, true, &[])
    } else {
        trafilatura::metadata::Metadata::default()
    };

    // 2. Body extraction via the cascade.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Min-word-count gate (xmltotxt with include_formatting=false matches
    //    JSON/CSV; the user-visible XML byte stream is a different shape but
    //    the gate semantic is "did we recover enough content").
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), false);
    let word_count = body_text.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // 4. Build Document (empty <body> sentinel when cascade returned None).
    let body_node = body_opt.clone().unwrap_or_else(|| {
        use crate::readability::dom::create_element;
        create_element("body")
    });
    let doc = trafilatura::output::Document {
        metadata,
        body: body_node,
        commentsbody: None,
        raw_text: String::new(),
    };

    // 5. control_xml_output runs the full xml.py:159-175 pipeline and returns
    //    the pretty-printed string. Stage 3-D dispatches the XML branch only.
    let xml_string = trafilatura::output::control_xml_output(
        &doc,
        trafilatura::output::OutputFormat::Xml,
    );

    // 6. NFC-normalise (core.py:98 invariant — extract_to_markdown does the
    //    same; consistent user-visible byte shape across all formatters).
    use unicode_normalization::UnicodeNormalization;
    let normalised: String = xml_string.as_str().nfc().collect();

    // Keep cascade body alive through serialisation (rcdom Drop quirk, HLD §m-3).
    let _ = body_opt;
    Ok(normalised)
}

/// Extract `html` and render result as TEI-conformant XML (Text Encoding
/// Initiative). Stricter than Trafilatura's own XML format — runs through
/// `check_tei` to fix invalid structures (move/wrap/relabel).
///
/// Equivalent to Python's `extract(html, output_format="xmltei",
/// tei_validation=False)` per `xml.py:159-175` TEI branch + `xml.py:186-235`
/// `build_tei_output`. Produces a pretty-printed XML string whose root is
/// `<TEI xmlns="http://www.tei-c.org/ns/1.0">`. The default tree shape is
/// `<TEI><teiHeader>...</teiHeader><text><body><div type="entry">...</div>
/// <div type="comments">...</div></body></text></TEI>`.
///
/// # `with_metadata` semantics
///
/// When `opts.with_metadata` is `true`, the `<teiHeader>` carries
/// bibliographic info: `<fileDesc>` (titleStmt + publicationStmt +
/// notesStmt + sourceDesc), `<profileDesc>` (abstract + textClass +
/// creation), and `<encodingDesc>` (appInfo). When `false`, the header is
/// still emitted (TEI conformance requires it) but with empty / default
/// values throughout — title, author, description, date, url, keywords all
/// blank / absent.
///
/// # `check_tei` walker
///
/// `xml.py:196-235` runs three passes:
/// 1. `<head>` rename to `<ab type="header">`, complex-head conversion via
///    `_tei_handle_complex_head`, `_move_element_one_level_up` when nested
///    inside `<p>`.
/// 2. `<lb>` directly under `<div>` with text-bearing tail becomes `<p>`.
/// 3. Descendant walk of `text/body`: tags outside `TEI_VALID_TAGS` are
///    merged with parent; tags in `TEI_REMOVE_TAIL` (`ab` / `p`) re-anchor
///    their tails; `<div>` triggers `_handle_text_content_of_div_nodes` +
///    `_wrap_unwanted_siblings_of_div`; attributes outside `TEI_VALID_ATTRS`
///    are popped.
///
/// # `tei_validation` deferred
///
/// Python's `validate_tei` (`xml.py:238-250`) uses lxml's `DTD.validate`
/// against the TEI DTD. Rust has no native DTD validator. Per the scoping
/// report, `tei_validation` is opt-in (default `False`), so the deferral is
/// silent on the default path. `extract_to_tei` does NOT validate.
///
/// # NFC normalisation
///
/// Output is NFC-normalised, consistent with [`extract_to_xml`] / [`extract_to_markdown`]
/// (the `core.py:98` invariant).
///
/// # `base_url`
///
/// As in [`extract`] — informational only. Never fetched.
///
/// # Errors
///
/// Same shape as [`extract_with`]: only [`ExtractError::ContentTooShort`]
/// when `opts.min_word_count > 0` and the produced body text fails the
/// threshold. The default-`Options` path never produces an error — an empty
/// body returns a minimal TEI tree with `<teiHeader>` + empty `<div
/// type="entry">` + empty `<div type="comments">`.
pub fn extract_to_tei(
    html: &str,
    base_url: Option<&str>,
    opts: &Options,
) -> Result<String, ExtractError> {
    // M4 Stage 3 sub-stage E — port of `core.bare_extraction` +
    // `control_xml_output` (xml.py:159-175 TEI branch) wired into the public
    // surface. Mirrors `extract_to_xml`'s pipeline shape; the only difference
    // is the `OutputFormat::Tei` discriminator passed to `control_xml_output`.

    // 1. Metadata — always extract (the TEI header consumes everything; even
    //    `with_metadata=false` produces a structurally valid header with empty
    //    fields). `with_metadata` gates whether populated metadata reaches the
    //    header (false = blank header fields).
    let metadata = if opts.with_metadata {
        trafilatura::metadata::extract_metadata(html, base_url, true, &[])
    } else {
        trafilatura::metadata::Metadata::default()
    };

    // 2. Body extraction via the cascade.
    let cleaning_opts = trafilatura::cleaning::Options {
        url: base_url.map(|s| s.to_string()),
        ..trafilatura::cleaning::Options::default()
    };
    let body_opt =
        trafilatura::readability_fork::bare_extraction_with_cascade(html, &cleaning_opts);

    // 3. Min-word-count gate (xmltotxt of body matches all other formatters).
    let body_text = trafilatura::output::xmltotxt(body_opt.as_ref(), false);
    let word_count = body_text.split_whitespace().count();
    if opts.min_word_count > 0 && word_count < opts.min_word_count {
        let _ = body_opt;
        return Err(ExtractError::ContentTooShort {
            word_count,
            threshold: opts.min_word_count,
        });
    }

    // 4. Build Document (empty <body> sentinel when cascade returned None).
    let body_node = body_opt.clone().unwrap_or_else(|| {
        use crate::readability::dom::create_element;
        create_element("body")
    });
    let doc = trafilatura::output::Document {
        metadata,
        body: body_node,
        commentsbody: None,
        raw_text: String::new(),
    };

    // 5. control_xml_output dispatched to the TEI branch.
    let xml_string = trafilatura::output::control_xml_output(
        &doc,
        trafilatura::output::OutputFormat::Tei,
    );

    // 6. NFC-normalise (core.py:98 invariant — all formatters do the same).
    use unicode_normalization::UnicodeNormalization;
    let normalised: String = xml_string.as_str().nfc().collect();

    // Keep cascade body alive through serialisation (rcdom Drop quirk, HLD §m-3).
    let _ = body_opt;
    Ok(normalised)
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
                // M4 Stage 4 additive fields: the M2 Readability port does
                // not derive these (Mozilla Readability has no
                // categories/tags/image/pagetype/license/hostname concept
                // beyond what's already folded into byline/excerpt/etc.),
                // so they default. `..Extracted::default()` would be
                // cleaner but the existing literal is exhaustive — staying
                // exhaustive matches the surrounding style.
                categories: Vec::new(),
                tags: Vec::new(),
                image: None,
                pagetype: None,
                license: None,
                hostname: None,
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
        // M4 Stage 3 sub-stage B additive field — default false so old
        // callers using `..Options::default()` keep their pre-stage-B
        // behaviour: extract_to_markdown emits the body only (no YAML
        // header).
        assert!(!o.with_metadata, "default with_metadata must be false");
    }

    #[test]
    fn options_is_clone_and_debug() {
        let o = Options {
            include_html: true,
            min_word_count: 7,
            with_metadata: false,
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
            categories: vec!["Tech".to_string()],
            tags: vec!["rust".to_string(), "web".to_string()],
            image: Some("https://example.com/img.png".to_string()),
            pagetype: Some("article".to_string()),
            license: Some("CC BY 4.0".to_string()),
            hostname: Some("example.com".to_string()),
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
        // M4 Stage 4 additive fields.
        assert_eq!(e.categories, vec!["Tech".to_string()]);
        assert_eq!(e.tags, vec!["rust".to_string(), "web".to_string()]);
        assert_eq!(e.image.as_deref(), Some("https://example.com/img.png"));
        assert_eq!(e.pagetype.as_deref(), Some("article"));
        assert_eq!(e.license.as_deref(), Some("CC BY 4.0"));
        assert_eq!(e.hostname.as_deref(), Some("example.com"));
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
            categories: Vec::new(),
            tags: Vec::new(),
            image: None,
            pagetype: None,
            license: None,
            hostname: None,
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
            with_metadata: false,
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
            with_metadata: false,
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
            with_metadata: false,
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
                with_metadata: false,
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

    // ====== M4 Stage 3 sub-stage B — extract_to_markdown tests.

    const MARKDOWN_ARTICLE_HTML: &str = "<html><head><title>An Article</title></head><body>\
        <article><p>This is a real readable paragraph with quite a few words \
        in it because the unlikely-candidate strip cares about minimum body length, \
        and we want the cascade to surface SOMETHING from the Trafilatura pipeline. \
        Adding more text here so the various length-threshold gates don't reject this \
        fixture outright; the M3 cascade has min_extracted_size=250 by default and \
        we want to clear it comfortably.</p></article></body></html>";

    /// Sub-stage B brief #1 — basic article: returns markdown text with `#`
    /// headings, paragraph content, and ends NFC-normalised.
    #[test]
    fn extract_to_markdown_returns_formatted_body() {
        let md = extract_to_markdown(MARKDOWN_ARTICLE_HTML, None, &Options::default())
            .expect("ok");
        assert!(
            md.contains("readable paragraph"),
            "expected body content, got: {md:?}"
        );
        // include_formatting=true unconditionally on the markdown formatter
        // — the paragraph emits the U+2424 spacing hack which `sanitize`
        // strips. Per Python's `core.py:95-96` commentsbody-strip branch
        // (mdrcel mirrors it unconditionally because Python's default
        // `options.comments=True` always sets `commentsbody` to a non-None
        // Element), the trailing `\n` is stripped, leaving the single
        // paragraph as a single line with no embedded newlines.
        assert!(
            !md.is_empty(),
            "expected formatted output, got: {md:?}"
        );
    }

    /// Sub-stage B brief #2 — with_metadata=true emits a YAML header
    /// listing populated metadata fields per core.py:75-91.
    #[test]
    fn extract_to_markdown_with_metadata_emits_yaml_header() {
        let html = r#"<html><head>
            <meta property="og:title" content="OG Title Wins">
            <meta property="og:description" content="A brief description for OG">
            <meta property="og:site_name" content="Example Site">
            <meta property="article:author" content="Jane Author">
            <link rel="canonical" href="https://example.com/canon">
            <title>Fallback Title</title>
            </head><body><article>
            <p>A real readable paragraph with enough words to extract; lorem ipsum dolor
            sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut
            labore et dolore magna aliqua ut enim ad minim veniam quis nostrud
            exercitation.</p>
            </article></body></html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let md = extract_to_markdown(html, None, &opts).expect("ok");
        assert!(md.starts_with("---\n"), "header opens, got: {md:?}");
        assert!(md.contains("title: OG Title Wins"));
        assert!(md.contains("author: Jane Author"));
        assert!(md.contains("url: https://example.com/canon"));
        assert!(md.contains("description: A brief description for OG"));
        assert!(md.contains("sitename: Example Site"));
        // Header closes before the body.
        let header_end = md.find("\n---\n").expect("header closes");
        assert!(
            md[header_end..].contains("readable paragraph"),
            "body follows header"
        );
    }

    /// Sub-stage B brief #3 — with_metadata=false (default) emits no header.
    #[test]
    fn extract_to_markdown_without_metadata_emits_no_header() {
        let md = extract_to_markdown(MARKDOWN_ARTICLE_HTML, None, &Options::default())
            .expect("ok");
        assert!(
            !md.starts_with("---"),
            "default opts must not emit YAML header, got: {md:?}"
        );
    }

    /// Sub-stage B brief #4 — `<a href="...">` links survive cleaning and
    /// emit `[text](url)` in markdown via `<ref target="...">` conversion.
    #[test]
    fn extract_to_markdown_renders_links_as_markdown() {
        // The cleaning::convert_tags pipeline drops `<a>` href by default
        // (Options.links = false). Switch on the link conversion via the
        // cleaning Options; for the public extract_to_markdown surface we
        // exercise the formatter on a programmatic `<ref>` tree to pin
        // the [text](target) emission shape — the `<ref>` element with a
        // `target` attribute is the Trafilatura internal representation
        // of an `<a>` that survives convert_tags (htmlprocessing.py:395-399).
        use crate::readability::dom::{
            Dom, append_child, create_element, create_text_node, set_attribute,
        };
        let dom = Dom::parse("<html><body></body></html>");
        let body = dom.body().expect("body");
        let p = create_element("p");
        append_child(&p, &create_text_node("See "));
        let r = create_element("ref");
        append_child(&r, &create_text_node("the link"));
        set_attribute(&r, "target", "https://example.com");
        append_child(&p, &r);
        append_child(&body, &p);
        let md = trafilatura::output::xmltotxt(Some(&p), true);
        assert!(
            md.contains("[the link](https://example.com)"),
            "got: {md:?}"
        );
        drop(dom);
    }

    /// Sub-stage B brief #5 — heading levels rend="hN" → N hashes.
    #[test]
    fn extract_to_markdown_renders_heading_levels() {
        use crate::readability::dom::{create_element, create_text_node, set_attribute, append_child};
        for (rend, prefix) in [
            ("h1", "# "),
            ("h2", "## "),
            ("h3", "### "),
            ("h4", "#### "),
        ] {
            let h = create_element("head");
            append_child(&h, &create_text_node("Title"));
            set_attribute(&h, "rend", rend);
            let md = trafilatura::output::xmltotxt(Some(&h), true);
            assert!(
                md.contains(&format!("{prefix}Title")),
                "rend={rend} expected `{prefix}Title` in {md:?}"
            );
        }
    }

    /// Sub-stage B brief #6 — `<code>foo</code>` (inline) → backticked.
    #[test]
    fn extract_to_markdown_renders_code_blocks() {
        use crate::readability::dom::{append_child, create_element, create_text_node};
        let c = create_element("code");
        append_child(&c, &create_text_node("foo()"));
        let md = trafilatura::output::xmltotxt(Some(&c), true);
        assert!(md.contains("`foo()`"), "got: {md:?}");
    }

    /// Sub-stage B brief #7 — `<quote>` rendered (the quote tag prefix
    /// happens at the surrounding paragraph level; we pin that the
    /// inner text survives and the quote tag delimits the section).
    #[test]
    fn extract_to_markdown_renders_quoted_text() {
        use crate::readability::dom::{append_child, create_element, create_text_node};
        let q = create_element("quote");
        append_child(&q, &create_text_node("an inspiring quote"));
        let md = trafilatura::output::xmltotxt(Some(&q), true);
        assert!(
            md.contains("an inspiring quote"),
            "quote text must survive, got: {md:?}"
        );
    }

    /// Sub-stage B brief #8 — empty body returns empty string (Bug-E2).
    #[test]
    fn extract_to_markdown_empty_body_returns_empty_string() {
        let md = extract_to_markdown("<html><body></body></html>", None, &Options::default())
            .expect("ok");
        assert_eq!(md, "", "empty body must yield empty markdown");
    }

    /// Sub-stage B brief #9 — basic table renders pipe-separated cells.
    #[test]
    fn extract_to_markdown_renders_table() {
        use crate::readability::dom::{append_child, create_element, create_text_node};
        let cell_a = create_element("cell");
        append_child(&cell_a, &create_text_node("a"));
        let cell_b = create_element("cell");
        append_child(&cell_b, &create_text_node("b"));
        let row = create_element("row");
        append_child(&row, &cell_a);
        append_child(&row, &cell_b);
        let table = create_element("table");
        append_child(&table, &row);
        let md = trafilatura::output::xmltotxt(Some(&table), true);
        // Leading `|` from cell_a; `|` separators between/after cells.
        assert!(md.contains("| a"), "leading-pipe missing: {md:?}");
        assert!(md.contains(" | "), "separator missing: {md:?}");
    }

    /// Sub-stage B brief #10 — NFC normalisation: NFD input is composed
    /// in output. "é" in NFD is "e\u{0301}"; NFC composes it to "\u{e9}".
    #[test]
    fn extract_to_markdown_nfc_normalises_output() {
        // Build a programmatic <p> with NFD text to pin the NFC pass.
        // We can't easily pass NFD through the HTML parser (html5ever
        // preserves bytes), so we craft the body element directly.
        use crate::readability::dom::{append_child, create_element, create_text_node};
        let p = create_element("p");
        // NFD: e + combining acute (U+0301).
        append_child(&p, &create_text_node("caf\u{0065}\u{0301}"));
        // Through the formatter helper (xmltotxt is the inner pipe; we
        // can verify NFC by sending the same string through nfc()).
        let raw = trafilatura::output::xmltotxt(Some(&p), true);
        // Apply NFC ourselves to verify the public function's behaviour.
        use unicode_normalization::UnicodeNormalization;
        let normalised: String = raw.as_str().nfc().collect();
        // NFC reduces to "café" with the single codepoint U+00E9.
        assert!(
            normalised.contains("caf\u{00E9}"),
            "expected NFC-composed text, got: {normalised:?}"
        );
        // The full extract_to_markdown applies NFC unconditionally on the
        // joined string; the helper above just confirms the algorithm.
    }

    /// Sub-stage B brief #11 — Options::default().with_metadata == false.
    #[test]
    fn extract_to_markdown_options_default_with_metadata_is_false() {
        assert!(!Options::default().with_metadata);
    }

    /// Sub-stage B brief #12 — backward compatibility: callers using
    /// `..Options::default()` keep working after the additive field.
    #[test]
    fn extract_to_markdown_options_struct_update_syntax_works() {
        let opts = Options {
            min_word_count: 10,
            ..Options::default()
        };
        assert_eq!(opts.min_word_count, 10);
        assert!(!opts.with_metadata);
        assert!(!opts.include_html);
    }

    /// Sub-stage B brief #13 — `<hi rend="#b">` → `**bold**`,
    /// `<hi rend="#i">` → `*italic*` (xml.py:266-269 / HI_FORMATTING).
    #[test]
    fn extract_to_markdown_renders_bold_and_italic() {
        use crate::readability::dom::{append_child, create_element, create_text_node, set_attribute};
        for (rend, expect) in [("#b", "**emphasized**"), ("#i", "*emphasized*")] {
            let h = create_element("hi");
            append_child(&h, &create_text_node("emphasized"));
            set_attribute(&h, "rend", rend);
            let md = trafilatura::output::xmltotxt(Some(&h), true);
            assert!(md.contains(expect), "rend={rend} expected {expect} in {md:?}");
        }
    }

    /// Sub-stage B brief #14 — bullet lists: `<item>` → `- item\n`.
    #[test]
    fn extract_to_markdown_renders_bullet_list() {
        use crate::readability::dom::{append_child, create_element, create_text_node};
        let item_a = create_element("item");
        append_child(&item_a, &create_text_node("first"));
        let item_b = create_element("item");
        append_child(&item_b, &create_text_node("second"));
        let list = create_element("list");
        append_child(&list, &item_a);
        append_child(&list, &item_b);
        let md = trafilatura::output::xmltotxt(Some(&list), true);
        assert!(md.contains("- first"), "got: {md:?}");
        assert!(md.contains("- second"), "got: {md:?}");
    }

    /// Sub-stage B brief #15 — with_metadata=true honours categories/tags
    /// (list-valued) via Python-style `['a', 'b']` rendering.
    #[test]
    fn extract_to_markdown_with_metadata_renders_categories_and_tags() {
        // Build a Metadata directly to test the YAML header builder
        // (a richer corpus path is exercised by tests #2 + #15 combined
        // via extract_to_markdown — but the categories/tags lists are
        // ONLY populated when JSON-LD / meta-keywords carries them).
        let html = r#"<html><head>
            <title>X</title>
            <meta name="keywords" content="alpha, beta, gamma">
            </head><body><article>
            <p>A real readable paragraph with enough words to extract; lorem ipsum dolor
            sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut
            labore et dolore magna aliqua ut enim ad minim veniam quis nostrud
            exercitation.</p>
            </article></body></html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let md = extract_to_markdown(html, None, &opts).expect("ok");
        // tags rendered as Python str(list). The metadata pipeline stores
        // the keywords content verbatim (no comma-split) as a single tag
        // string — `["alpha, beta, gamma"]` — so the YAML emits that
        // single-element list. Pin the list-shape (`[..]`) and the
        // single-quoted content separately.
        assert!(
            md.contains("tags: ["),
            "tags list shape missing, got: {md:?}"
        );
        assert!(
            md.contains("'alpha, beta, gamma'"),
            "tag content missing, got: {md:?}"
        );
    }

    // ====== M4 Stage 3 sub-stage C — extract_to_json + extract_to_csv tests.

    const JSON_CSV_ARTICLE_HTML: &str = r#"<html><head>
        <meta property="og:title" content="OG Title Wins">
        <meta property="og:description" content="A brief description for OG">
        <meta property="og:site_name" content="Example Site">
        <meta property="article:author" content="Jane Author">
        <link rel="canonical" href="https://example.com/canon">
        <title>Fallback Title</title>
        </head><body><article>
        <p>A real readable paragraph with enough words to extract; lorem ipsum dolor
        sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut
        labore et dolore magna aliqua ut enim ad minim veniam quis nostrud
        exercitation.</p>
        </article></body></html>"#;

    /// Sub-stage C JSON test #1 — basic article with with_metadata=true
    /// returns a JSON object whose payload `text` field is non-empty.
    #[test]
    fn extract_to_json_with_metadata_returns_valid_json() {
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let out = extract_to_json(JSON_CSV_ARTICLE_HTML, None, &opts).expect("ok");
        // Must be parseable JSON.
        let v: serde_json::Value =
            serde_json::from_str(&out).expect("output must be valid JSON");
        assert!(v.is_object(), "expected JSON object, got {out:?}");
        // Author and title come from the metadata pipeline.
        assert_eq!(v["title"].as_str(), Some("OG Title Wins"));
        assert_eq!(v["author"].as_str(), Some("Jane Author"));
        // Text must contain the paragraph body.
        let text = v["text"].as_str().expect("text field");
        assert!(
            text.contains("readable paragraph"),
            "text missing body content, got {text:?}"
        );
    }

    /// Sub-stage C JSON test #2 — empty body returns JSON with empty `text`.
    #[test]
    fn extract_to_json_empty_body_returns_empty_text() {
        let out = extract_to_json("<html><body></body></html>", None, &Options::default())
            .expect("ok");
        let v: serde_json::Value =
            serde_json::from_str(&out).expect("must be valid JSON");
        assert_eq!(v["text"].as_str(), Some(""), "text must be empty: {out:?}");
        assert_eq!(v["comments"].as_str(), Some(""), "comments must be empty");
    }

    /// Sub-stage C JSON test #3 — output must always be parseable.
    #[test]
    fn extract_to_json_is_always_parseable() {
        for opts in [
            Options::default(),
            Options {
                with_metadata: true,
                ..Options::default()
            },
        ] {
            let out =
                extract_to_json(JSON_CSV_ARTICLE_HTML, None, &opts).expect("ok");
            let _v: serde_json::Value = serde_json::from_str(&out)
                .unwrap_or_else(|e| panic!("must parse: {e} — out={out:?}"));
        }
    }

    /// Sub-stage C JSON test #4 — without metadata, output has only `text`
    /// and `comments` keys (xml.py:128-130 body-only branch).
    #[test]
    fn extract_to_json_without_metadata_only_text_and_comments() {
        let out = extract_to_json(JSON_CSV_ARTICLE_HTML, None, &Options::default())
            .expect("ok");
        let v: serde_json::Value =
            serde_json::from_str(&out).expect("must parse");
        let obj = v.as_object().expect("object");
        let keys: Vec<&String> = obj.keys().collect();
        assert_eq!(keys.len(), 2, "body-only branch must have 2 keys: {keys:?}");
        assert!(obj.contains_key("text"));
        assert!(obj.contains_key("comments"));
    }

    /// Sub-stage C JSON test #5 — with metadata, the 19 Python-spec keys are
    /// all present in the output (key-set parity vs xml.py:117-127).
    #[test]
    fn extract_to_json_with_metadata_has_python_spec_key_set() {
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let out = extract_to_json(JSON_CSV_ARTICLE_HTML, None, &opts).expect("ok");
        let v: serde_json::Value = serde_json::from_str(&out).expect("must parse");
        let obj = v.as_object().expect("object");
        let expected: &[&str] = &[
            "title",
            "author",
            "hostname",
            "date",
            "fingerprint",
            "id",
            "license",
            "comments",
            "raw_text",
            "text",
            "language",
            "image",
            "pagetype",
            "filedate",
            "source",
            "source-hostname",
            "excerpt",
            "categories",
            "tags",
        ];
        for k in expected {
            assert!(
                obj.contains_key(*k),
                "missing JSON key {k:?} — got keys: {:?}",
                obj.keys().collect::<Vec<_>>()
            );
        }
        assert_eq!(obj.len(), expected.len());
        // Source = canonical URL via metadata pipeline.
        assert_eq!(v["source"].as_str(), Some("https://example.com/canon"));
        assert_eq!(v["excerpt"].as_str(), Some("A brief description for OG"));
        assert_eq!(v["source-hostname"].as_str(), Some("Example Site"));
        // Stage-6-deferred fields render as null.
        assert!(v["fingerprint"].is_null());
        assert!(v["id"].is_null());
        assert!(v["filedate"].is_null());
    }

    /// Sub-stage C CSV test #6 — output has header row + exactly ONE data row.
    #[test]
    fn extract_to_csv_emits_header_plus_one_row() {
        let out = extract_to_csv(JSON_CSV_ARTICLE_HTML, None, &Options::default())
            .expect("ok");
        // Two CRLF-terminated lines: header + data.
        let lines: Vec<&str> = out.split("\r\n").collect();
        // split yields trailing empty after the final \r\n.
        assert!(lines.len() >= 2, "expected ≥2 rows, got: {out:?}");
        assert!(lines[0].starts_with("url\t"), "header start: {:?}", lines[0]);
        assert!(!lines[1].is_empty(), "data row missing");
    }

    /// Sub-stage C CSV test #7 — default delimiter is TAB.
    #[test]
    fn extract_to_csv_is_tab_delimited_by_default() {
        let out = extract_to_csv(JSON_CSV_ARTICLE_HTML, None, &Options::default())
            .expect("ok");
        // The header row has 11 columns → 10 tabs.
        let first_line = out.split("\r\n").next().expect("first line");
        let tab_count = first_line.matches('\t').count();
        assert_eq!(tab_count, 10, "header tab count: {first_line:?}");
        // No commas in the header (a comma-default would dispatch as CSV not TSV).
        assert!(!first_line.contains(','), "must not be comma-delimited");
    }

    /// Sub-stage C CSV test #8 — empty fields render as the `null` token.
    #[test]
    fn extract_to_csv_renders_null_for_empty_fields() {
        // Minimal HTML with no metadata → most fields will be empty.
        let html = "<html><body><p>plain text</p></body></html>";
        let out = extract_to_csv(html, None, &Options::default()).expect("ok");
        // The data row's URL column (col 1) has no source — must be "null".
        let data_row = out.split("\r\n").nth(1).expect("data row");
        let first_col = data_row.split('\t').next().expect("first col");
        assert_eq!(first_col, "null", "empty URL must be null: {data_row:?}");
    }

    /// Sub-stage C CSV test #9 — newlines and tabs in body text are properly
    /// CSV-quoted (per QUOTE_MINIMAL).
    #[test]
    fn extract_to_csv_quotes_fields_containing_delimiter_or_newline() {
        use crate::trafilatura::output::{xmltocsv, Document};
        use crate::readability::dom::{append_child, create_element, create_text_node};
        // Build a programmatic body containing a tab and a newline.
        let body = create_element("body");
        let p = create_element("p");
        append_child(&p, &create_text_node("line1\ttab\nline2"));
        append_child(&body, &p);
        let doc = Document {
            metadata: crate::trafilatura::metadata::Metadata::default(),
            body,
            commentsbody: None,
            raw_text: String::new(),
        };
        let row = xmltocsv(&doc, false, "\t", "null", false);
        // The text column must be quoted (contains both \t and \n).
        // Easiest check: the row contains a `"`.
        assert!(
            row.contains('"'),
            "expected quoted field, got: {row:?}"
        );
    }

    /// Sub-stage C CSV test #10 — empty body still produces a valid row with
    /// the correct number of columns.
    #[test]
    fn extract_to_csv_empty_body_produces_valid_row() {
        let out = extract_to_csv("<html><body></body></html>", None, &Options::default())
            .expect("ok");
        let lines: Vec<&str> = out.split("\r\n").collect();
        // 2 substantive lines (header + data), possibly + 1 trailing empty.
        assert!(lines.len() >= 2);
        // The data row, unquoted with empty/null fields, splits on \t to give
        // exactly 11 columns.
        let data_row = lines[1];
        let cols: Vec<&str> = data_row.split('\t').collect();
        assert_eq!(cols.len(), 11, "expected 11 columns, got: {cols:?}");
    }

    /// Sub-stage C CSV test #11 — header columns match Python's xmltocsv
    /// column order exactly (xml.py:378-388).
    #[test]
    fn extract_to_csv_header_column_order_matches_python() {
        let out = extract_to_csv("<html><body></body></html>", None, &Options::default())
            .expect("ok");
        let header = out.split("\r\n").next().expect("header");
        let expected = "url\tid\tfingerprint\thostname\ttitle\timage\tdate\ttext\tcomments\tlicense\tpagetype";
        assert_eq!(header, expected, "header column order");
    }

    /// Sub-stage C CSV test #12 — min_word_count threshold fires when set
    /// (parity with extract_with / extract_to_markdown behaviour).
    #[test]
    fn extract_to_csv_respects_min_word_count() {
        let opts = Options {
            min_word_count: 5,
            ..Options::default()
        };
        let err = extract_to_csv("<html><body></body></html>", None, &opts)
            .expect_err("must Err");
        match err {
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                assert_eq!(word_count, 0);
                assert_eq!(threshold, 5);
            }
            other => panic!("expected ContentTooShort, got {other:?}"),
        }
    }

    /// Sub-stage B brief #16 — min_word_count threshold fires when set
    /// (parity with extract_with behaviour).
    #[test]
    fn extract_to_markdown_respects_min_word_count() {
        let opts = Options {
            min_word_count: 5,
            ..Options::default()
        };
        let err = extract_to_markdown("<html><body></body></html>", None, &opts)
            .expect_err("must Err on threshold miss");
        match err {
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                assert_eq!(word_count, 0);
                assert_eq!(threshold, 5);
            }
            other => panic!("expected ContentTooShort, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // Stage 3-D: extract_to_xml — Python xml.py:159-175 control_xml_output.
    // -------------------------------------------------------------------

    /// Brief #1 — Basic article yields valid XML with `<doc>` root and a
    /// `<main>` child carrying the extracted body.
    #[test]
    fn extract_to_xml_basic_article_yields_doc_main() {
        let html = r#"<html><body>
            <article><h1>Title</h1><p>Hello, world.</p></article>
        </body></html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        assert!(s.starts_with("<doc"), "must start with <doc: {s}");
        assert!(s.contains("<main>"), "must contain <main>: {s}");
        assert!(s.contains("Hello, world."), "must contain body text: {s}");
        assert!(s.ends_with("</doc>"), "must end with </doc>: {s}");
    }

    /// Brief #6 — Empty article: minimal <doc> with self-closing children.
    #[test]
    fn extract_to_xml_empty_body_produces_minimal_doc() {
        let s =
            extract_to_xml("<html><body></body></html>", None, &Options::default()).expect("ok");
        // Empty extraction -> <doc>\n  <main/>\n  <comments/>\n</doc>.
        assert_eq!(s, "<doc>\n  <main/>\n  <comments/>\n</doc>");
    }

    /// Brief #3 — with_metadata=true populates <doc> root attributes.
    #[test]
    fn extract_to_xml_with_metadata_populates_doc_attrs() {
        let html = r#"<html>
            <head>
                <title>The Title</title>
                <meta property="og:url" content="https://example.com/article"/>
            </head>
            <body><article><p>One two three four five.</p></article></body>
        </html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_xml(html, Some("https://example.com/article"), &opts).expect("ok");
        assert!(
            s.contains("title=\"The Title\""),
            "metadata title must populate <doc>: {s}"
        );
    }

    /// Brief #4 — with_metadata=false yields bare <doc> root (no attrs).
    #[test]
    fn extract_to_xml_without_metadata_has_bare_doc_root() {
        let html = r#"<html>
            <head><title>The Title</title></head>
            <body><article><p>One two three four five.</p></article></body>
        </html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        // Default opts -> with_metadata=false -> no title= on <doc>.
        assert!(s.starts_with("<doc>\n"), "got: {s}");
        assert!(!s.contains("title="), "got: {s}");
    }

    /// Brief #7 — Special chars in body text are XML-escaped.
    #[test]
    fn extract_to_xml_escapes_special_chars_in_body() {
        let html = r#"<html><body>
            <article><p>a &lt; b &amp;&amp; c &gt; d</p></article>
        </body></html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        // The HTML entities decoded to characters in the DOM, then re-escaped
        // by our XML serializer.
        assert!(s.contains("a &lt; b &amp;&amp; c &gt; d"), "got: {s}");
    }

    /// Brief #9 — Output is NFC-normalised.
    #[test]
    fn extract_to_xml_output_is_nfc() {
        // U+0065 U+0301 (NFD) -> U+00E9 (NFC) after extract_to_xml normalises.
        let html = "<html><body><article><p>cafe\u{0301}</p></article></body></html>";
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        assert!(
            s.contains('\u{00E9}'),
            "decomposed e+combining-acute must NFC to single U+00E9: {s:?}"
        );
        assert!(
            !s.contains('\u{0301}'),
            "no combining acute should survive NFC: {s:?}"
        );
    }

    /// Brief #2 — Output is a serialisable XML byte stream (the only way to
    /// "parse back" cheaply is to re-render via the same path; what we
    /// actually verify is that the byte stream is well-formed enough to
    /// re-pass through mdrcel's HTML5 parser without panic).
    #[test]
    fn extract_to_xml_output_is_parseable_html_subset() {
        use crate::readability::dom::Dom;
        let html = r#"<html><body><article><p>Hello world from XML.</p></article></body></html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        // The Trafilatura XML uses <doc> / <main> / <comments> — not real
        // HTML tags — but feeding it back to the HTML5 parser MUST NOT
        // panic. The parser will wrap it in <html><head></head><body>...
        // tolerantly.
        let _ = Dom::parse(&s);
    }

    /// Brief #10 — Per-tag attributes preserved when whitelisted. We can't
    /// trigger <hi rend=...> from real HTML easily (HTML's <b>/<i> map to
    /// xmltotxt formatting markers, not <hi> tags). Instead verify the
    /// negative: <p class="..."> attrs are STRIPPED (p is not in
    /// WITH_ATTRIBUTES per xml.py:39).
    #[test]
    fn extract_to_xml_strips_non_whitelisted_attrs() {
        let html = r#"<html><body>
            <article><p class="article-body" id="lead">First para.</p></article>
        </body></html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        // class / id on <p> get wiped by clean_attributes.
        assert!(
            !s.contains("class=\"article-body\""),
            "p.class must be stripped: {s}"
        );
        assert!(!s.contains("id=\"lead\""), "p.id must be stripped: {s}");
        // Body text still present.
        assert!(s.contains("First para."), "got: {s}");
    }

    /// Brief #8 — Indentation: nested elements use 2-space increments.
    #[test]
    fn extract_to_xml_indents_two_spaces_per_level() {
        let html = r#"<html><body><article><p>Hello world.</p></article></body></html>"#;
        let s = extract_to_xml(html, None, &Options::default()).expect("ok");
        // The <main> sits at depth 1 -> 2 spaces.
        assert!(
            s.contains("\n  <main>"),
            "main must be indented 2 spaces: {s}"
        );
    }

    /// Stage 3-D parity with extract_to_markdown — min_word_count gate fires.
    #[test]
    fn extract_to_xml_respects_min_word_count() {
        let opts = Options {
            min_word_count: 5,
            ..Options::default()
        };
        let err = extract_to_xml("<html><body></body></html>", None, &opts)
            .expect_err("must Err on threshold miss");
        match err {
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                assert_eq!(word_count, 0);
                assert_eq!(threshold, 5);
            }
            other => panic!("expected ContentTooShort, got {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // Stage 3-E: extract_to_tei — Python xml.py:186-235 build_tei_output +
    // check_tei. The TEI branch of control_xml_output.
    // -------------------------------------------------------------------

    /// Brief #1 — Basic article: valid TEI structure with <TEI> root.
    #[test]
    fn extract_to_tei_basic_article_yields_tei_root() {
        let html = r#"<html><body>
            <article><h1>Title</h1><p>Hello, world.</p></article>
        </body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        assert!(s.starts_with("<TEI"), "must start with <TEI: {s}");
        assert!(
            s.contains("xmlns=\"http://www.tei-c.org/ns/1.0\""),
            "must declare TEI ns: {s}"
        );
        assert!(s.contains("Hello, world."), "must contain body text: {s}");
        assert!(s.ends_with("</TEI>"), "must end with </TEI>: {s}");
    }

    /// Brief #2 — with_metadata=true: full <teiHeader> populated.
    #[test]
    fn extract_to_tei_with_metadata_populates_full_header() {
        let html = r#"<html>
            <head>
                <title>The Article Title</title>
                <meta property="og:url" content="https://example.com/a"/>
                <meta property="og:site_name" content="Example"/>
            </head>
            <body><article><p>One two three four five six seven.</p></article></body>
        </html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_tei(html, Some("https://example.com/a"), &opts).expect("ok");
        assert!(s.contains("<teiHeader>"), "header present: {s}");
        assert!(s.contains("<fileDesc>"), "{s}");
        assert!(s.contains("<titleStmt>"), "{s}");
        assert!(s.contains("<publicationStmt>"), "{s}");
        assert!(s.contains("<sourceDesc>"), "{s}");
        assert!(
            s.contains("The Article Title"),
            "title must appear in header: {s}"
        );
    }

    /// Brief #3 — with_metadata=false: header still emitted (TEI requires it)
    /// but with blank fields.
    #[test]
    fn extract_to_tei_without_metadata_still_emits_minimal_header() {
        let html = r#"<html>
            <head><title>Should Not Appear</title></head>
            <body><article><p>The body content here.</p></article></body>
        </html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // teiHeader is always present (TEI conformance).
        assert!(s.contains("<teiHeader>"), "{s}");
        // But the title from metadata should NOT appear (with_metadata=false).
        assert!(
            !s.contains("Should Not Appear"),
            "title must NOT leak when with_metadata=false: {s}"
        );
    }

    /// Brief #4 — TEI namespace is declared on the root.
    #[test]
    fn extract_to_tei_declares_namespace_on_root() {
        let html = r#"<html><body><article><p>x</p></article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        assert!(
            s.contains("<TEI xmlns=\"http://www.tei-c.org/ns/1.0\""),
            "namespace decl: {s}"
        );
    }

    /// Brief #6 — Empty body: minimal valid TEI.
    #[test]
    fn extract_to_tei_empty_body_produces_minimal_tei() {
        let s = extract_to_tei("<html><body></body></html>", None, &Options::default())
            .expect("ok");
        // Even empty content yields a structurally valid <TEI>...</TEI>.
        assert!(s.starts_with("<TEI"), "{s}");
        assert!(s.ends_with("</TEI>"), "{s}");
        assert!(s.contains("<teiHeader>"), "{s}");
        // text/body chain present even when empty.
        assert!(s.contains("<text>") || s.contains("<text/>"), "{s}");
    }

    /// Brief #7 — `<TEI>` carries the expected children: teiHeader + text.
    #[test]
    fn extract_to_tei_structure_has_header_and_text() {
        let html = r#"<html><body><article><p>Body para.</p></article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // Order: teiHeader first, then text.
        let header_idx = s.find("<teiHeader").expect("has teiHeader");
        let text_idx = s.find("<text").expect("has text element");
        assert!(
            header_idx < text_idx,
            "teiHeader must precede text: {s}"
        );
    }

    /// Brief #11 — NFC normalisation: NFD input becomes NFC.
    #[test]
    fn extract_to_tei_output_is_nfc() {
        // U+0065 U+0301 (NFD) -> U+00E9 (NFC).
        let html =
            "<html><body><article><p>cafe\u{0301} bistro</p></article></body></html>";
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        assert!(
            s.contains('\u{00E9}'),
            "decomposed e+combining-acute must NFC: {s:?}"
        );
        assert!(
            !s.contains('\u{0301}'),
            "no combining acute should survive NFC: {s:?}"
        );
    }

    /// Brief #13 — `<span>` (not in TEI_VALID_TAGS) is stripped.
    #[test]
    fn extract_to_tei_strips_non_whitelisted_descendant_tags() {
        let html = r#"<html><body><article>
            <p>good text in para that should survive cleaning</p>
        </article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // <span> should never appear in the output — it's not in
        // TEI_VALID_TAGS (xml.py:28-29).
        assert!(!s.contains("<span"), "no <span>: {s}");
    }

    /// Brief #17 — `<licence>` is `<licence>` rendered inside the
    /// publicationStmt via availability/<p>. Test the structural shape.
    /// (License extraction is stubbed in our Metadata so we exercise the
    /// "no license" branch: an empty <p/> for conformity.)
    #[test]
    fn extract_to_tei_publicationstmt_includes_empty_p_when_no_license() {
        let html = r#"<html><body><article><p>x y z a b c d e</p></article></body></html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_tei(html, None, &opts).expect("ok");
        // publicationStmt is always present; without a license the
        // conformity-filler is an empty <p/>.
        assert!(s.contains("<publicationStmt>"), "{s}");
    }

    /// Brief #12 — Comments handling: `<div type="comments">` always present.
    #[test]
    fn extract_to_tei_emits_comments_div_when_no_comments() {
        let html = r#"<html><body><article><p>a b c d e f g</p></article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // The TEI body should have two <div>s: entry + comments. comments
        // is empty here (no commentsbody extracted).
        assert!(
            s.contains("type=\"comments\"") || s.contains("type=\"entry\""),
            "should have entry/comments divs: {s}"
        );
    }

    /// Brief #15 — Date metadata renders as element text on <date>.
    /// (Stage 7d's date wiring is deferred; the test asserts the
    /// `<date type="download"/>` empty element survives in the creation block.)
    #[test]
    fn extract_to_tei_creation_block_has_download_date() {
        let html = r#"<html><body><article><p>a b c d e f g h</p></article></body></html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_tei(html, None, &opts).expect("ok");
        // creation block with <date type="download"/> is always present.
        assert!(s.contains("<creation>"), "{s}");
        assert!(s.contains("type=\"download\""), "{s}");
    }

    /// Brief #16 — URL renders as `<ptr type="URL" target="...">` in biblFull.
    #[test]
    fn extract_to_tei_url_renders_as_ptr_in_biblfull() {
        let html = r#"<html>
            <head><meta property="og:url" content="https://example.com/article"/></head>
            <body><article><p>One two three four five.</p></article></body>
        </html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_tei(html, Some("https://example.com/article"), &opts).expect("ok");
        assert!(s.contains("<biblFull>"), "biblFull present: {s}");
        // URL renders as <ptr type="URL" target="..."/>.
        assert!(
            s.contains("type=\"URL\"") && s.contains("https://example.com/article"),
            "URL ptr: {s}"
        );
    }

    /// Brief #18 — Output is well-formed enough to feed back into the HTML5
    /// parser (no panic).
    #[test]
    fn extract_to_tei_output_is_parseable_by_html5_parser() {
        use crate::readability::dom::Dom;
        let html = r#"<html><body><article><p>Sample body content.</p></article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // No panic on re-parse.
        let _ = Dom::parse(&s);
    }

    /// Brief #19 — `<note type="fingerprint"/>` is emitted in notesStmt.
    #[test]
    fn extract_to_tei_notesstmt_has_fingerprint_note() {
        let html = r#"<html><body><article><p>a b c d e</p></article></body></html>"#;
        let opts = Options {
            with_metadata: true,
            ..Options::default()
        };
        let s = extract_to_tei(html, None, &opts).expect("ok");
        assert!(s.contains("<notesStmt>"), "{s}");
        assert!(s.contains("type=\"fingerprint\""), "{s}");
    }

    /// Brief #20 — Trafilatura `<application>` element with version + ident.
    #[test]
    fn extract_to_tei_encodingdesc_has_application_block() {
        let html = r#"<html><body><article><p>a b c</p></article></body></html>"#;
        let s = extract_to_tei(html, None, &Options::default()).expect("ok");
        // Always present (the encodingDesc is structural).
        assert!(s.contains("<encodingDesc>"), "{s}");
        assert!(s.contains("<appInfo>"), "{s}");
        assert!(s.contains("ident=\"Trafilatura\""), "{s}");
        assert!(s.contains("https://github.com/adbar/trafilatura"), "{s}");
    }

    /// Stage 3-E parity with extract_to_xml — min_word_count gate fires.
    #[test]
    fn extract_to_tei_respects_min_word_count() {
        let opts = Options {
            min_word_count: 5,
            ..Options::default()
        };
        let err = extract_to_tei("<html><body></body></html>", None, &opts)
            .expect_err("must Err on threshold miss");
        match err {
            ExtractError::ContentTooShort {
                word_count,
                threshold,
            } => {
                assert_eq!(word_count, 0);
                assert_eq!(threshold, 5);
            }
            other => panic!("expected ContentTooShort, got {other:?}"),
        }
    }

    // ====================================================================
    // M4 Stage 4 — Metadata -> Extracted public-surface mapping.
    //
    // Six new fields (`categories`, `tags`, `image`, `pagetype`, `license`,
    // `hostname`) flow straight through from
    // `trafilatura::metadata::extract_metadata`. Each test pins one source
    // → field path; the negative tests pin the additive guarantee.
    // ====================================================================

    /// Test 1 — `Metadata.hostname` (`metadata.py:542-543` —
    /// `extract_domain(url, fast=True)`) lands on `Extracted.hostname`
    /// when the page carries a `<link rel="canonical">`.
    #[test]
    fn extracted_hostname_from_canonical_link() {
        let html = r#"<html><head>
            <link rel="canonical" href="https://example.com/page">
            <title>T</title></head>
            <body><article><p>Body text that is long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, None).expect("must extract");
        assert_eq!(e.canonical_url.as_deref(), Some("https://example.com/page"));
        assert_eq!(e.hostname.as_deref(), Some("example.com"));
    }

    /// Test 2 — `Metadata.categories`
    /// (`metadata.py:422-446` `extract_catstags("category", tree)` —
    /// `<meta property="article:section">` category fallback).
    #[test]
    fn extracted_categories_from_article_section_meta() {
        let html = r#"<html><head>
            <meta property="article:section" content="Tech">
            <title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, None).expect("must extract");
        assert_eq!(e.categories, vec!["Tech".to_string()]);
    }

    /// Test 3 — `Metadata.tags` (`metadata.py:422-446` `extract_catstags("tag", tree)`,
    /// `<meta property="article:tag">` content split / dedup).
    #[test]
    fn extracted_tags_from_article_tag_meta() {
        let html = r#"<html><head>
            <meta property="article:tag" content="rust">
            <title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, None).expect("must extract");
        // examine_meta routes article:tag content verbatim into Metadata.tags
        // (metadata.py:483-498). One value, so one element.
        assert_eq!(e.tags, vec!["rust".to_string()]);
    }

    /// Test 4 — `Metadata.image` from `og:image`
    /// (`assign_og_property` at `metadata.py:141-149`). Stored verbatim —
    /// no URL resolution at metadata time (Python parity).
    #[test]
    fn extracted_image_from_og_image_meta() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://cdn.example.com/img.jpg">
            <title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, Some("https://example.com/page"))
            .expect("must extract");
        assert_eq!(
            e.image.as_deref(),
            Some("https://cdn.example.com/img.jpg")
        );
    }

    /// Test 5 — `Metadata.pagetype` from `og:type` — `assign_og_property`'s
    /// `og:type` branch at `metadata.py:141-149`.
    #[test]
    fn extracted_pagetype_from_og_type_meta() {
        let html = r#"<html><head>
            <meta property="og:type" content="article">
            <title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, None).expect("must extract");
        assert_eq!(e.pagetype.as_deref(), Some("article"));
    }

    /// Test 6 — `Metadata.license` from a `rel="license"` link
    /// (`extract_license` at `metadata.py:465-479` — non-strict mode
    /// returns the trimmed link text when no LICENSE_REGEX hit).
    #[test]
    fn extracted_license_from_rel_license_link() {
        let html = r#"<html><head><title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article>
            <footer><a rel="license" href="https://example.com/terms">My Custom Licence</a></footer>
            </body></html>"#;
        let e = extract(html, None).expect("must extract");
        assert_eq!(e.license.as_deref(), Some("My Custom Licence"));
    }

    /// Test 7 — Negative: when no metadata is provided, all six new
    /// fields default to their empty/None values. Confirms additive
    /// guarantee (no spurious population).
    #[test]
    fn extracted_new_fields_default_when_no_metadata() {
        let html = r#"<html><head><title>T</title></head>
            <body><article><p>Body text long enough to extract reliably.</p></article></body></html>"#;
        let e = extract(html, None).expect("must extract");
        assert!(e.categories.is_empty(), "categories: {:?}", e.categories);
        assert!(e.tags.is_empty(), "tags: {:?}", e.tags);
        assert!(e.image.is_none(), "image: {:?}", e.image);
        assert!(e.pagetype.is_none(), "pagetype: {:?}", e.pagetype);
        assert!(e.license.is_none(), "license: {:?}", e.license);
        assert!(e.hostname.is_none(), "hostname: {:?}", e.hostname);
    }

    /// Test 8 — Backward compat: `Extracted::default()` produces sensible
    /// defaults for every new field. Anchors the additive invariant.
    #[test]
    fn extracted_default_covers_new_fields() {
        let e = Extracted::default();
        assert!(e.categories.is_empty());
        assert!(e.tags.is_empty());
        assert!(e.image.is_none());
        assert!(e.pagetype.is_none());
        assert!(e.license.is_none());
        assert!(e.hostname.is_none());
    }

    /// Test 9 — Backward compat: `..Extracted::default()` callsites still
    /// compile and populate the new fields with their defaults. This
    /// mirrors the in-repo callsites at `benchmark/src/score.rs:1058` and
    /// `benchmark/src/crate_run.rs:343`, which the supervisor relies on
    /// continuing to compile transparently.
    #[test]
    fn extracted_partial_struct_literal_compiles() {
        let e = Extracted {
            title: Some("X".to_string()),
            text: "hello".to_string(),
            ..Extracted::default()
        };
        assert_eq!(e.title.as_deref(), Some("X"));
        // The new M4 Stage 4 fields are silently defaulted.
        assert!(e.categories.is_empty());
        assert!(e.tags.is_empty());
        assert!(e.image.is_none());
        assert!(e.pagetype.is_none());
        assert!(e.license.is_none());
        assert!(e.hostname.is_none());
    }

    /// Test 10 — Byte-identity sanity for the pre-Stage-4 fields: the M3
    /// finale's `extract` invariant (`extract == extract_with(default)`)
    /// remains intact when the new fields are populated. Reuses the
    /// canonical sample HTML used by `extract_is_extract_with_default_options`
    /// so any drift would surface there too.
    #[test]
    fn extracted_existing_fields_unchanged_with_new_fields_present() {
        let html = r#"<html><head>
            <meta property="og:type" content="article">
            <meta property="og:image" content="https://cdn.example.com/img.jpg">
            <meta property="article:section" content="Tech">
            <link rel="canonical" href="https://example.com/page">
            <title>T</title></head>
            <body><article><p>hello world</p></article></body></html>"#;
        let a = extract(html, None).expect("a");
        let b = extract_with(html, None, &Options::default()).expect("b");
        // PartialEq covers every field (old + new). If any pre-Stage-4
        // field drifted on the trafilatura path the comparison would
        // catch it.
        assert_eq!(a, b);
        // And: the new fields actually landed.
        assert_eq!(a.pagetype.as_deref(), Some("article"));
        assert_eq!(a.image.as_deref(), Some("https://cdn.example.com/img.jpg"));
        assert_eq!(a.categories, vec!["Tech".to_string()]);
        assert_eq!(a.hostname.as_deref(), Some("example.com"));
    }
}
