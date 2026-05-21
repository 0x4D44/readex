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
