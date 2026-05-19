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
