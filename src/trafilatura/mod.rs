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
