//! `htmldate` — port of the Python `htmldate` package (M4 Stage 1, sub-stage A).
//!
//! Source of truth: `htmldate@1.9.x` (vendored under
//! `C:\Users\marti\AppData\Roaming\Python\Python314\site-packages\htmldate\`).
//!
//! This is the in-tree **internal infrastructure surface** for the htmldate
//! port, exposed via the lib.rs `#[doc(hidden)] pub mod htmldate;` declaration
//! so later sub-stages (B/C/D/...) and consumers (M4 metadata wiring) can
//! drive sub-modules without leaking them onto the stable public contract.
//!
//! **Sub-stage A** (this commit): `settings` + `utils::Extractor` +
//! `utils::trim_text` only. Sub-stage B onwards adds the date-parsing
//! algorithm itself (XPath sources, regex extractors, validation, the
//! `find_date` orchestrator).

// Sub-stage A — module-level constants ported verbatim from
// `htmldate/settings.py` (MIN_DATE, MAX_FILE_SIZE, CACHE_SIZE,
// MAX_POSSIBLE_CANDIDATES, CLEANING_LIST).
pub mod settings;

// Sub-stage A — small utility surface ported from `htmldate/utils.py`:
// the `Extractor` options struct (lines 47-65) and the `trim_text` helper
// (lines 258-260). Other `utils.py` helpers (`load_html`, `clean_html`,
// `fetch_url`, encoding detection) are deferred to sub-stage G.
pub mod utils;

// Sub-stage B — date validators / plausibility / format parsing — verbatim
// port of `htmldate/validators.py:1-216`. Adds an internal `DateTime` type
// (year/month/day/hour/minute/second tuple) because Python `datetime`'s
// `timestamp()` comparison at validators.py:53 requires hour-precision.
// `chrono` is still NOT a crate dependency — see `validators.rs`'s
// "Date typing" doc-comment.
pub mod validators;

// Sub-stage C — regex catalogues + month tables — verbatim port of
// `htmldate/extractors.py:47-213`. Pure-data constants: every regex lives
// behind a `std::sync::OnceLock<Regex>` slot. Sub-stage C does NOT wire
// runtime behaviour; the catalogues are consumed by sub-stage D's
// date-extraction algorithm. SIMPLE_PATTERN's `(?<!w3.org)` negative
// lookbehind is preserved as a separate `simple_pattern_post_filter`
// helper (Rust `regex` is finite-automaton; no lookarounds). See
// `regex_catalogues.rs`'s module-level docs for the divergence rationale.
pub mod regex_catalogues;

// Sub-stage D — non-dateparser parsing layer — verbatim port of
// `htmldate/extractors.py:216-508`. Wires sub-stages A/B/C into the
// `discard_unwanted` / `extract_url_date` / `correct_year` /
// `try_swap_values` / `regex_parse` / `custom_parse` / `try_date_expr` /
// `img_search` / `pattern_search` / `json_search` /
// `idiosyncrasies_search` algorithm. The `external_date_parser`
// function is shipped as a STUB returning `None` (Python's call into
// `dateparser.DateDataParser` is deferred indefinitely — see the
// `extractors.rs` module header for the rationale).
pub mod extractors;

// Sub-stage E — header walker + element walkers + candidate selection
// — verbatim port of `htmldate/core.py:80-571`. Adds the
// `DATE_ATTRIBUTES` / `NAME_MODIFIED` / `PROPERTY_MODIFIED` /
// `ITEMPROP_ATTRS` tables, `examine_text` / `examine_date_elements` /
// `examine_header` / `select_candidate` / `search_pattern` /
// `compare_reference` / `examine_abbr_elements` /
// `examine_time_elements` / `normalize_match`. `search_page` and
// `find_date` (the orchestrators at core.py:574-983) are deferred to
// sub-stage F.
pub mod core;
