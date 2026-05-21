//! `trafilatura` — port of Trafilatura v2.0.0 (HLD M3
//! `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)`).
//!
//! This is the in-tree **internal infrastructure surface** for the M3 port,
//! exposed via the lib.rs `#[doc(hidden)] pub mod trafilatura;` declaration so
//! the conformance harness (`tests/xpath_conformance.rs`) and later stages can
//! drive sub-modules without leaking them onto the stable public contract.
//!
//! Stage 0b (this commit): `xpath_engine` only. Later stages add
//! `xpaths`, `cleaning`, `main_extractor`, `baseline`, `external`, etc. —
//! see HLD §5.

// Stage 0b — greenfield XPath evaluator + conformance table (HLD §6.1,
// DECISION-A). Operator catalog is DA-B-1 revised; see the module docs of
// `xpath_engine` for the exact contract.
pub mod xpath_engine;

// Stage 1b — tree_cleaning + convert_tags + prune_html (HLD §7.2, DECISION-F).
// Source of truth: `trafilatura@v2.0.0/htmlprocessing.py`. The Stage 0c
// Trafilatura-equivalence BLOCKER gate (`tests/trafilatura_equivalence_gate.rs`)
// activates against this module's output.
pub mod cleaning;

// Stage 1b — vendored constants for tree_cleaning + convert_tags. Each entry
// traces verbatim to a `trafilatura@v2.0.0/settings.py` or `.../htmlprocessing.py`
// line. Membership-test arrays, not HashSets — order is load-bearing per
// Trafilatura's `# order could matter` comment at `settings.py:348`.
pub mod settings_constants;

// Stage 1c — `baseline()` rescue extractor + `html2txt()` last resort +
// `basic_cleaning()` pre-strip (HLD §7.3). Source of truth:
// `trafilatura@v2.0.0/baseline.py:18-123` plus `settings.py:432-434`
// (BASIC_CLEAN_XPATH literal) and `utils.py:340-346` (trim).
pub mod baseline;

// Stage 2a — verbatim Rust vendoring of `trafilatura@v2.0.0/xpaths.py`
// (HLD M3 §7). Stores the 13 XPath constants (BODY_XPATH, COMMENTS_XPATH,
// OVERALL_DISCARD_XPATH, etc.) as `&[&str]` so callers iterate them and pass
// each expression to `xpath_engine::evaluate`. The Python `XPath(...)` wrapper
// is a Python-side compile cache and is not vendored. Gap survey for which
// XPaths the Stage 0b engine accepts vs needs Stage 2b extension lives in
// `tests/xpath_constants_engine_coverage.rs`.
pub mod xpaths_constants;

// Stage 2b' — small utility helpers ported from `trafilatura@v2.0.0/utils.py`
// (HLD §7.2 prerequisites for Stage 2c-i): `FORMATTING_PROTECTED`,
// `SPACING_PROTECTED`, `IMAGE_EXTENSION`, `RE_FILTER`, `is_image_file`,
// `is_image_element`, `textfilter`, `text_chars_test`, `trim`. Plus the
// `duplicate_test` stub from `deduplication.py:243-254` (the full LRU port
// is deferred until a future stage activates `Options.dedup`).
pub mod utils;

// Stage 2c-i — handler primitives ported from
// `trafilatura@v2.0.0/main_extractor.py:30-160` (HLD §7.4): module constants
// (P_FORMATTING / TABLE_ELEMS / TABLE_ALL / FORMATTING / CODES_QUOTES /
// NOT_AT_THE_END), `handle_titles`, `handle_formatting`, `add_sub_element`,
// `process_nested_elements`, `update_elem_rendition`, `is_text_element`,
// `define_newelem`. The `process_nested_elements` list dispatch routes
// through a forward-stub `handle_lists` that panics with a Stage 2c-ii
// citation; Stage 2c-ii replaces that stub with the full port
// (`main_extractor.py:161-205`).
pub mod main_extractor;

// Stage 4a — Trafilatura's INTERNAL FORK of readability-lxml: data
// structures + scoring primitives. Source of truth:
// `trafilatura@v2.0.0/readability_lxml.py:42-303`. Ports the module-level
// constants (DIV_SCORES / BLOCK_SCORES / BAD_ELEM_SCORES /
// STRUCTURE_SCORES / FRAME_TAGS / LIST_TAGS / TEXT_CLEAN_ELEMS /
// REGEXES dict), the `Candidate` dataclass, and the five leaf scoring
// primitives (`text_length`, `class_weight`, `score_node`,
// `score_paragraph_text`, `link_density`). NO orchestration logic — that
// arrives in Stage 4b (`Document::summary()` core), Stage 4c (`sanitize`
// + ruthless/lenient retry), Stage 4d (`is_probably_readerable` +
// cascade integration). Distinct from `crate::readability` (which is
// the M2 port of Mozilla Readability.js — different algorithm, different
// scoring constants).
pub mod readability_fork;

// Stage 5a — vendored jusText language stoplists (100 languages). Source
// of truth: `justext/utils.py:51-63` (`get_stoplist`) and
// `justext/utils.py:37-48` (`get_stoplists`). The newline-delimited
// word lists at `justext/stoplists/*.txt` are vendored verbatim under
// `justext_stoplists/`; per-language lazy `OnceLock<Vec<String>>`
// accessors lowercase + cache on first access (matching the Python
// `.lower()` step that owned strings would re-do on every call).
// Consumed by Stage 5c's `classify_paragraphs` port.
pub mod justext_stoplists;

// Stage 5b/5c/5d — jusText paragraph segmentation + classifier + cascade
// wrappers. Source of truth: `justext/core.py:28-371` + `justext/
// paragraph.py:14-66` + `trafilatura/external.py:121-160`. Stage 5b
// landed segmentation (`Paragraph` + `ParagraphMaker` + `make_paragraphs`);
// Stage 5c landed the context-free + context-sensitive classifier
// (`classify_paragraphs` + `revise_paragraph_classification`); Stage 5d
// landed the cascade wrappers (`try_justext` + `justext_rescue` +
// `JUSTEXT_LANGUAGES`) plus the 3-arm cascade integration in
// `readability_fork::bare_extraction_with_cascade`.
pub mod justext_core;

// Stage 7a — HTML-based metadata extraction (`metadata.py:198-589` HTML-tag
// sources only). OpenGraph `<meta property="og:*">`, `<meta name="...">`,
// `<meta itemprop="...">`, `<html lang="...">`, plus XPath fallbacks for
// title / author via `TITLE_XPATHS` / `AUTHOR_XPATHS`. JSON-LD parsing
// (7b), URL canonicalization + date extraction (7d), and license + cat/tag
// XPath extraction are deferred to later sub-stages — `Metadata::url` /
// `hostname` / `date` / `license` / `categories` remain `None`/empty at
// Stage 7a. NO public API change to `extract` / `Extracted` — `Metadata`
// is a NEW public type living under the same `#[doc(hidden)] pub mod
// trafilatura` infrastructure surface as every other M3 stage.
pub mod metadata;

// Stage 7b — JSON-LD metadata extraction (`json_metadata.py:1-268` + the
// `metadata.py:182-195` `extract_meta_json` orchestrator). Parses
// `<script type="application/ld+json">` blocks for schema.org-shaped
// structured metadata (Article / NewsArticle / Organization / Person /
// WebPage etc.), then enriches the Stage-7a `Metadata` struct in place.
// Article-like types contribute title / author / categories / tags /
// date / image; Person types contribute author; Organization /
// NewsMediaOrganization / WebPage / WebSite types contribute site_name
// (with a length-prefers-longer rule). Wired into
// `metadata::extract_metadata` between the OG/meta walk and the title
// XPath fallback.
pub mod metadata_jsonld;

// Stage 7d — minimal URL canonicalization (`metadata.py:389-413` +
// `courlan/urlutils.py:14-62`) + date stub (subset of `htmldate`'s easy
// path: <meta property="article:published_time">, <meta name="date">,
// <time datetime="...">) + the previously-deferred Stage 7a extractors
// `extract_catstags` (`metadata.py:422-446`) and `extract_license`
// (`metadata.py:465-479`). Wired into `metadata::extract_metadata` so
// `Metadata::url` / `hostname` / `date` / `categories` / `tags` / `license`
// populate additively (JSON-LD values from Stage 7b keep precedence).
pub mod metadata_url;

// Stage 8 — LRU cache + `duplicate_test` (`deduplication.py:146-254`).
// Lands the LRU dedup the Stage 2b' stub deferred. New public surface:
// `LruCache` + `duplicate_test` (text-level) + `duplicate_test_node`
// (element wrapper) + `put_in_cache` + `with_lru_test` + `clear_lru_test`.
// Wired into `cleaning::handle_textnode` / `cleaning::process_node`
// (per-element gate, htmlprocessing.py:262,:282) and into
// `readability_fork::compare_extraction` (body-level gate, core.py:330).
// The shared `LRU_TEST` singleton matches Python's process-wide cache
// (deduplication.py:232) with capacity `LRU_SIZE = 4096`
// (settings.py:308).
pub mod deduplication;
