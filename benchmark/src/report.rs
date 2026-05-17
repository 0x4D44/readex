//! Report emission: `report.md` + `results.json`.
//! Logic arrives in a later stage (harness HLD §9).
//!
//! # FORWARD CONTRACT — agreement-on-disagreement must carry its sample N
//!
//! When this stub is implemented (Stage 7), the agreement-on-disagreement
//! distribution (`score::Agreement` — see the matching forward-contract note
//! on `score::agreement`) **MUST** be rendered **with its sample size**
//! (`N = k of m` URLs) and **flagged non-representative when N is below a
//! documented threshold**. It must **never** report a bare "crate sides with
//! Trafilatura X%" without the accompanying `(N=k of m)`: that signal is
//! `Some` only on the subset of URLs where all three sides are valid *and* the
//! two oracles genuinely disagree, which is often a handful. A percentage over
//! a handful presented as a population statistic is exactly the laundered,
//! misleading number the harness doctrine (HLD §5; the Bug-E2 lesson) forbids.
//!
//! # FORWARD CONTRACT — host pinning uses the canonical form (Stage-8)
//!
//! The Stage-8 regression check (`regression.rs`) compares the running host
//! against the baseline's stamped `host`. It **MUST** canonicalise **both**
//! sides via `score::canonical_host`'s contract (trim, lowercase, short
//! hostname — FQDN domain stripped) before comparing; a raw string compare
//! would treat `ANVIL`, `anvil`, and `anvil.corp.local` as different hosts.
//! A detection-failure run is never written at all (`score::HostDetectionFailed`
//! — no results.json), so it can never become a baseline and never matches any
//! host; there is deliberately no shared `"unknown-host"` sentinel for a
//! `host == host` check to spuriously equate (HLD §2.9). The advisory-vs-gating
//! message on a host mismatch is per HLD §9; this note pins only the
//! comparison form, not the surrounding regression policy.
