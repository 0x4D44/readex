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
