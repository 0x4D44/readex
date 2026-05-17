//! Scoring: apply the oracle hierarchy → coverage / precision / agreement
//! (harness HLD §2/§5/§8/§9). This is the **integrative anti-Bug-E2 stage**.
//!
//! # The hierarchy (HLD §2.1/§2.4, locked)
//!
//! ```text
//! gold set  >  Trafilatura (#1)  >  Readability-JS (#2, guardrail only)
//! ```
//!
//! Per URL the scoring **reference** is:
//! * the **gold** expected text, *iff* a gold entry exists for that URL
//!   (HLD §7 — authoritative there, curated non-empty by construction); else
//! * **Trafilatura**'s [`OracleResult::text`] (HLD §2.2 — #1 is the target
//!   on a disagreement with no gold entry).
//!
//! **Readability is NEVER the reference** (HLD §2.3 — it is the guardrail: it
//! raises a flag when it extracts substantially more than Trafilatura on a
//! structured page, but it never *defines* truth).
//!
//! # THE critical requirement — Bug-E2 status-gating (HLD §5 doctrine)
//!
//! Coverage/Precision are **trusted scores only when BOTH** the crate produced
//! a real [`CrateStatus::Ok`] **AND** the chosen reference is itself a valid
//! non-empty extraction. This module **must not launder** any failure into a
//! passing number. Concretely (see `metrics.rs`'s `# HAZARD — J(∅, ∅) = 1.0`):
//! `jaccard(∅,∅)=1.0` and `jaccard(x,∅)=0.0` are *mathematically* correct but
//! *meaning-ambiguous*; a crate `Ok("")` scored against an empty/failed
//! reference must **never** surface as Coverage=1.0 "perfect". The gate is on
//! **STATUS before the metric**, encoded as the explicit [`ScoreOutcome`] enum
//! (`Scored` vs `NotScored{reason}`) — **not** an in-band sentinel float. This
//! is the entire reason the tri-state status types exist.
//!
//! [`score_url`] is the single gating decision; it is pure given its inputs
//! (statuses + optional gold text) and is exhaustively unit-tested as a matrix.
//!
//! # Metrics — recomputed via `metrics.rs`, never the wire values (HLD §8)
//!
//! Every number is recomputed from the `.text` fields with the single
//! tokenizer in `metrics.rs`. The harness **never** trusts
//! [`OracleResult::word_count`] or [`mdrcel::Extracted::word_count`] (HLD §8 —
//! "The harness never trusts an external word count"); there is exactly one
//! word-count definition in the whole harness.
//!
//! # No premature abstraction (HLD §3)
//!
//! The hierarchy is **concrete**: there is no scoring-strategy plugin/registry.
//! Exactly two oracles + an optional gold map, hardcoded — the M8-ring-road
//! antipattern the brief warns against.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::corpus::CorpusEntry;
use crate::corpus::ShapeClass;
use crate::crate_run::{CrateStatus, run_crate};
use crate::metrics::{edit_similarity, jaccard, precision, tokens, word_count};
use crate::oracle::{OracleKind, OracleStatus, run_oracle};

/// Guardrail ratio (HLD §8): flag suspected Trafilatura truncation when
/// Readability's word count exceeds Trafilatura's by more than this factor on
/// a non-`hub_index` page. A documented **code constant** (HLD §10 — no env /
/// no flags), revisitable once real corpus evidence exists.
const GUARDRAIL_RATIO: f64 = 1.25;

/// Agreement-on-disagreement threshold (HLD §8): Trafilatura and Readability
/// are "far apart" when their token-set Jaccard is **below** this. A
/// documented code constant.
const AGREEMENT_DISAGREE: f64 = 0.5;

/// First-class scoring outcome for one URL — the explicit anti-Bug-E2 gate
/// (HLD §5). Either a **trusted** score, or an explicitly **not-scored**
/// record carrying the reason. This is deliberately an enum, **not** an
/// in-band sentinel float: a `NotScored` is *unambiguously* distinguishable
/// from a real `0.0`/`1.0`, so a failed/empty extraction can never be
/// laundered into a passing Coverage (the `metrics.rs` `# HAZARD` vector).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ScoreOutcome {
    /// Both the crate produced a real [`CrateStatus::Ok`] **and** the chosen
    /// reference is a valid non-empty extraction. These numbers are trusted.
    ///
    /// `coverage` = `jaccard(crate, reference)`, `precision` =
    /// `metrics::precision(crate, reference)`, `edit_sim` =
    /// `edit_similarity(crate, reference)` — all over the single tokenizer.
    /// `coverage`/`precision` may legitimately be `0.0` here (crate produced
    /// `Ok("")` against a real non-empty reference — a *trusted* zero, the
    /// crate genuinely extracted nothing; meaningfully different from the
    /// `NotScored` cases below).
    Scored {
        coverage: f64,
        precision: f64,
        edit_sim: f64,
    },
    /// The score is **not trusted** and was deliberately not computed as a
    /// number (HLD §5). The `reason` says why; it is never a passing score.
    NotScored { reason: NotScoredReason },
}

/// Why a URL was not scored (HLD §5 status taxonomy → scoring side). Each
/// variant is a distinct, recorded reason — never collapsed into a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotScoredReason {
    /// The crate returned [`CrateStatus::NotImplemented`] — the Milestone-1
    /// floor. Coverage/Precision are NOT computed; the status is still
    /// recorded so "the algorithm does not exist yet" is visible.
    CrateNotImplemented,
    /// The crate returned [`CrateStatus::CrateError`] (a hard error / a caught
    /// panic). Not laundered into an empty-`Ok` score.
    CrateError,
    /// The reference is unavailable: no gold entry **and** Trafilatura was
    /// [`OracleStatus::OracleError`] / [`OracleStatus::OracleTimeout`]. There
    /// is nothing trustworthy to score against (HLD §5 — a failed reference
    /// must never yield a trusted metric).
    ReferenceUnavailable,
    /// The reference text is **empty** (tokenizes to ∅). `jaccard(x,∅)=0.0`
    /// and `jaccard(∅,∅)=1.0` are meaning-ambiguous (`metrics.rs` `# HAZARD`),
    /// so an empty reference can never yield a trusted Coverage — gated here
    /// **before** the metric, regardless of the crate's own text.
    ReferenceEmpty,
}

/// Which oracle the crate is closer to, when Trafilatura and Readability
/// **disagree** (their Jaccard `< AGREEMENT_DISAGREE`) — HLD §8
/// "agreement-on-disagreement". Distribution reported (Stage 7), not scored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Agreement {
    /// Crate text is closer (higher Jaccard) to Trafilatura's than to
    /// Readability's.
    CloserToTrafilatura,
    /// Crate text is closer to Readability's than to Trafilatura's.
    CloserToReadability,
    /// Equidistant from both (exact Jaccard tie).
    Tie,
}

/// The wire spelling of a [`CrateStatus`] for `results.json` (HLD §9 —
/// per-status counts use these exact tokens).
fn crate_status_str(s: &CrateStatus) -> &'static str {
    match s {
        CrateStatus::Ok(_) => "ok",
        CrateStatus::NotImplemented => "not_implemented",
        CrateStatus::CrateError(_) => "crate_error",
    }
}

/// The wire spelling of an [`OracleStatus`] for `results.json` (HLD §9).
fn oracle_status_str(s: &OracleStatus) -> &'static str {
    match s {
        OracleStatus::Ok(_) => "ok",
        OracleStatus::OracleError(_) => "oracle_error",
        OracleStatus::OracleTimeout(_) => "oracle_timeout",
    }
}

/// The human-readable reason a [`CrateStatus`] is **not** `Ok`, if any
/// (HLD §5 — the variant "carries a human-readable reason so the report can
/// surface *why*"). `None` for `Ok`/`NotImplemented` (no free-text reason).
///
/// Surfacing this in `results.json` is deliberate anti-Bug-E2 hygiene: a bare
/// `crate_error` token with no *why* is exactly the silent information loss
/// the doctrine warns against. (It also legitimately consumes the
/// `CrateError` payload on the non-test path — not an artificial use.)
fn crate_status_detail(s: &CrateStatus) -> Option<&str> {
    match s {
        CrateStatus::CrateError(reason) => Some(reason.as_str()),
        CrateStatus::Ok(_) | CrateStatus::NotImplemented => None,
    }
}

/// The human-readable reason an [`OracleStatus`] is **not** `Ok`, if any
/// (HLD §5 — the error/timeout variants carry a reason so the report can
/// surface *why* without re-deriving it). `None` for `Ok`.
///
/// As with [`crate_status_detail`], recording this in `results.json` keeps a
/// failed reference *explained*, not silently reduced to a bare token (the
/// Bug-E2 lesson — never lose the failure information).
fn oracle_status_detail(s: &OracleStatus) -> Option<&str> {
    match s {
        OracleStatus::OracleError(r) | OracleStatus::OracleTimeout(r) => Some(r.as_str()),
        OracleStatus::Ok(_) => None,
    }
}

/// Borrow the extracted body text iff the crate produced a real `Ok`.
///
/// The gate's crate side: `NotImplemented` / `CrateError` yield `None` — they
/// are **never** treated as `Ok("")` (the exact Bug-E2 conflation).
fn crate_text(s: &CrateStatus) -> Option<&str> {
    match s {
        CrateStatus::Ok(e) => Some(e.text.as_str()),
        CrateStatus::NotImplemented | CrateStatus::CrateError(_) => None,
    }
}

/// Borrow an oracle's extracted text iff it produced a real `Ok`.
///
/// `OracleError` / `OracleTimeout` yield `None` — a failed oracle is never
/// treated as empty content (the Bug-E2 lesson, consumer side; HLD §5).
fn oracle_text(s: &OracleStatus) -> Option<&str> {
    match s {
        OracleStatus::Ok(r) => Some(r.text.as_str()),
        OracleStatus::OracleError(_) | OracleStatus::OracleTimeout(_) => None,
    }
}

/// Word count of an oracle's text via the **single tokenizer** (HLD §8),
/// `None` unless the oracle produced a real `Ok`. The adapter's own
/// `word_count` is deliberately **never** consulted.
fn oracle_word_count(s: &OracleStatus) -> Option<usize> {
    oracle_text(s).map(word_count)
}

/// Apply the hierarchy + the Bug-E2 status gate for one URL (HLD §5/§8).
///
/// **Pure** given its inputs, so the entire gating matrix is unit-tested
/// without spawning anything. The decision order is load-bearing:
///
/// 1. **Crate gate.** If `crate_status` is not [`CrateStatus::Ok`] → return
///    [`ScoreOutcome::NotScored`] with [`NotScoredReason::CrateNotImplemented`]
///    or [`NotScoredReason::CrateError`]. Coverage/Precision are **not**
///    computed as a trusted number; the status is recorded by the caller.
/// 2. **Reference resolution.** `gold_text` (if `Some`) is the reference
///    (HLD §7 — gold wins, curated non-empty by construction). Otherwise the
///    reference is Trafilatura's text — and Trafilatura **must** be
///    [`OracleStatus::Ok`]; an `OracleError`/`OracleTimeout` reference →
///    [`NotScoredReason::ReferenceUnavailable`]. Readability is **never**
///    consulted as the reference.
/// 3. **Empty-reference gate.** If the resolved reference tokenizes to ∅ →
///    [`NotScoredReason::ReferenceEmpty`], **regardless of the crate text**.
///    This is the exact `metrics.rs` `# HAZARD` cut: `jaccard(∅,∅)=1.0` /
///    `jaccard(x,∅)=0.0` are meaning-ambiguous, so an empty reference can
///    never yield a trusted Coverage.
/// 4. Crate `Ok` **and** a non-empty reference → [`ScoreOutcome::Scored`]
///    with `coverage`/`precision`/`edit_sim` recomputed via `metrics.rs`.
///    Note the crate text itself **may** be empty here — `Ok("")` vs a real
///    non-empty reference yields a *trusted* `coverage = 0.0` (the crate
///    genuinely extracted nothing), which is meaningfully different from any
///    `NotScored` case and is therefore correctly a real score, not laundered.
pub fn score_url(
    crate_status: &CrateStatus,
    trafilatura: &OracleStatus,
    gold_text: Option<&str>,
) -> ScoreOutcome {
    // 1. Crate gate — a non-Ok crate is NEVER scored (no laundering of
    //    NotImplemented/CrateError into an empty-Ok metric).
    let ctext = match crate_text(crate_status) {
        Some(t) => t,
        None => {
            let reason = match crate_status {
                CrateStatus::NotImplemented => NotScoredReason::CrateNotImplemented,
                CrateStatus::CrateError(_) => NotScoredReason::CrateError,
                // Unreachable: crate_text returned None ⇒ not Ok.
                CrateStatus::Ok(_) => unreachable!("crate_text(None) implies not Ok"),
            };
            return ScoreOutcome::NotScored { reason };
        }
    };

    // 2. Reference resolution: gold wins (HLD §7); else Trafilatura, which
    //    must itself be Ok. Readability is never the reference (HLD §2.3).
    let reference: &str = match gold_text {
        Some(g) => g,
        None => match oracle_text(trafilatura) {
            Some(t) => t,
            None => {
                return ScoreOutcome::NotScored {
                    reason: NotScoredReason::ReferenceUnavailable,
                };
            }
        },
    };

    // 3. Empty-reference gate — the metrics.rs # HAZARD cut. An empty
    //    reference can never yield a trusted Coverage, whatever the crate did.
    if tokens(reference).is_empty() {
        return ScoreOutcome::NotScored {
            reason: NotScoredReason::ReferenceEmpty,
        };
    }

    // 4. Trusted score. crate Ok + non-empty reference. crate text MAY be ""
    //    here ⇒ coverage 0.0, a TRUSTED zero (genuinely extracted nothing
    //    against a real reference), distinct from every NotScored case.
    ScoreOutcome::Scored {
        coverage: jaccard(ctext, reference),
        precision: precision(ctext, reference),
        edit_sim: edit_similarity(ctext, reference),
    }
}

/// Guardrail flag (HLD §8): `true` iff `word_count(Readability) >
/// word_count(Trafilatura) * GUARDRAIL_RATIO` **and** the page is not
/// `hub_index` **and** both oracles produced a real `Ok` (need both texts to
/// compare). A hub/index page legitimately has Readability ≫ Trafilatura, so
/// it is excluded by design. The boundary is strict `>`: exactly `1.25×` does
/// **not** fire.
fn guardrail_flag(
    trafilatura: &OracleStatus,
    readability: &OracleStatus,
    shape: ShapeClass,
) -> bool {
    if shape == ShapeClass::HubIndex {
        return false;
    }
    match (
        oracle_word_count(trafilatura),
        oracle_word_count(readability),
    ) {
        (Some(tw), Some(rw)) => (rw as f64) > (tw as f64) * GUARDRAIL_RATIO,
        // Need BOTH oracles' real text to compare — anything else: no flag.
        _ => false,
    }
}

/// Agreement-on-disagreement (HLD §8): only meaningful when **all three** of
/// crate / Trafilatura / Readability produced a real, non-empty `Ok` text.
/// When Trafilatura and Readability disagree (their Jaccard
/// `< AGREEMENT_DISAGREE`), record which one the crate's text is closer to (by
/// Jaccard to each). `None` when not applicable (any side missing/empty, or
/// the oracles do **not** disagree).
///
/// # FORWARD CONTRACT — Stage-7 reporting (binding on `report.rs`)
///
/// This signal is `Some` only on the (often small) subset of URLs where all
/// three sides are valid **and** the two oracles genuinely disagree. The
/// Stage-7 report **MUST** render this distribution **with its sample size**
/// (`N = k of m` URLs) and **flag it non-representative when N is below a
/// documented threshold**. It must **never** state a bare "crate sides with
/// Trafilatura X%" without the accompanying `(N=k of m)` — a percentage over a
/// handful of URLs read as a population statistic is exactly the kind of
/// laundered, misleading number the harness doctrine forbids. (Mirrored in
/// `report.rs`.)
fn agreement(
    crate_status: &CrateStatus,
    trafilatura: &OracleStatus,
    readability: &OracleStatus,
) -> Option<Agreement> {
    let c = crate_text(crate_status)?;
    let t = oracle_text(trafilatura)?;
    let r = oracle_text(readability)?;
    // All three must be non-empty for the comparison to be meaningful.
    if tokens(c).is_empty() || tokens(t).is_empty() || tokens(r).is_empty() {
        return None;
    }
    // Only meaningful where the two oracles genuinely disagree.
    if jaccard(t, r) >= AGREEMENT_DISAGREE {
        return None;
    }
    let to_t = jaccard(c, t);
    let to_r = jaccard(c, r);
    Some(if to_t > to_r {
        Agreement::CloserToTrafilatura
    } else if to_r > to_t {
        Agreement::CloserToReadability
    } else {
        Agreement::Tie
    })
}

/// Word counts for the report (HLD §9), each recomputed via the single
/// tokenizer (`metrics.rs`, HLD §8) — **never** the wire `word_count`. `None`
/// where that producer did not yield a real `Ok` (so an absent count is
/// unambiguously distinct from a real `0`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordCounts {
    /// `metrics::word_count(crate text)`; `None` unless crate `Ok`.
    pub crate_wc: Option<usize>,
    /// `metrics::word_count(Trafilatura text)`; `None` unless Trafilatura `Ok`.
    pub trafilatura_wc: Option<usize>,
    /// `metrics::word_count(Readability text)`; `None` unless Readability `Ok`.
    pub readability_wc: Option<usize>,
}

/// The human-readable *why* behind any non-`Ok` producer status (HLD §5/§9).
/// Each is `None` when that producer was `Ok` (or, for the crate, a
/// reason-less status like `not_implemented`). Recorded so a failed
/// crate/oracle is **explained** in `results.json`, never silently reduced to
/// a bare status token (the Bug-E2 lesson — do not lose failure information).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusDetail {
    pub crate_reason: Option<String>,
    pub trafilatura_reason: Option<String>,
    pub readability_reason: Option<String>,
}

/// One per-URL record in `results.json` (HLD §9). Harness-internal serde —
/// **not** the oracle contract. Statuses are the wire-spelling strings; the
/// score is the explicit [`ScoreOutcome`] (never a sentinel float).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UrlRecord {
    pub url: String,
    pub shape_class: String,
    pub crate_status: String,
    pub trafilatura_status: String,
    pub readability_status: String,
    /// The *why* for any non-`Ok` status (HLD §5) — keeps a failure explained.
    pub status_detail: StatusDetail,
    pub word_counts: WordCounts,
    pub score: ScoreOutcome,
    /// Secondary, non-gating (HLD §8). `Some` only when the same gate as
    /// [`ScoreOutcome::Scored`] holds (so it is never a meaning-ambiguous
    /// empty-driven value); `None` otherwise.
    pub edit_sim: Option<f64>,
    pub guardrail_flag: bool,
    /// `None` when agreement-on-disagreement is not applicable (HLD §8).
    pub agreement: Option<Agreement>,
}

/// Per-status counts for the run header (HLD §9 summary). One map per producer
/// keyed by the wire-spelling status token (e.g. `ok`, `oracle_error`,
/// `oracle_timeout`, `not_implemented`, `crate_error`). `BTreeMap` so the
/// serialized order is stable run-to-run (regression-diff friendly).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusCounts {
    pub crate_status: BTreeMap<String, usize>,
    pub trafilatura_status: BTreeMap<String, usize>,
    pub readability_status: BTreeMap<String, usize>,
}

/// The `results.json` run header + per-URL records (HLD §9) — the single
/// source of truth for a run. Harness-internal serde (the report, Stage 7, is
/// generated *from* this; this module does not build the report).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunResults {
    /// Machine-readable host identity in **canonical form** (HLD §2.9 / O2) —
    /// the baseline is valid only on the named host; stamped here so the
    /// Stage-8 regression check can compare. Always the [`canonical_host`]
    /// value (trimmed, lowercased, short hostname); a detection-failure run is
    /// never written at all ([`HostDetectionFailed`]) so this is never a
    /// shared sentinel a `host == host` check could spuriously equate.
    pub host: String,
    /// UTC run timestamp, `YYYY-MM-DDTHH-MM-SSZ` (filesystem-safe — `-`, not
    /// `:`, so it is also the `runs/<ts>/` directory name).
    pub utc_timestamp: String,
    /// Number of corpus URLs scored this run.
    pub corpus_size: usize,
    /// Per-producer per-status counts (HLD §9 summary).
    pub status_counts: StatusCounts,
    /// One record per corpus URL, in manifest order.
    pub urls: Vec<UrlRecord>,
}

/// Failure to determine **any** real host identity (HLD §2.9 / O2). This is a
/// hard, fail-closed error — *not* a sentinel string — by the same doctrine as
/// [`crate::corpus::CorpusError::SnapshotMissing`] and
/// [`GoldError::ExpectedTextMissing`]: a results.json with no real provenance
/// must **never** be written.
///
/// # Why a previous `"unknown-host"` sentinel was wrong (the collision vector)
///
/// HLD §2.9/§9: the committed baseline is valid **only on one named host**, and
/// the Stage-8 regression check compares the running host against that name.
/// If total detection failure stamped a *literal* (`"unknown-host"`), two
/// **different** unidentifiable machines would both stamp the identical token,
/// and a `host == host` comparison would treat them as the **same declared
/// host** — a *false* baseline validity that silently defeats the single-host
/// reproducibility guarantee §2.9 exists to provide. A loud, non-comparable
/// hard error is the only fail-closed answer: a run with no host provenance is
/// unscorable / never baseline-eligible.
///
/// # FORWARD CONTRACT — Stage-8 host comparison (binding on the regression code)
///
/// The Stage-8 regression check **MUST** compare hosts using the canonical
/// form produced by [`canonical_host`] (trim, lowercase, short hostname — FQDN
/// domain stripped), applied to **both** the running host and the baseline's
/// stamped `host`. It must **never** raw-string-compare un-normalised hosts
/// (`ANVIL` vs `anvil` vs `anvil.corp.local` are the *same* host and must
/// match; this is why Stage 6 stamps the canonical form, not the raw value).
/// A detection-failure run produces **no** results.json (this error), so it can
/// never become a baseline and never matches any host — there is deliberately
/// no `"unknown-host"` value for a `host == host` check to spuriously equate.
#[derive(Debug)]
pub struct HostDetectionFailed;

impl fmt::Display for HostDetectionFailed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "could not determine any host identity (COMPUTERNAME / HOSTNAME \
             env, and the `hostname` / `uname -n` commands all failed). A run \
             with no host provenance is unscorable: the baseline is valid only \
             on one named host (HLD §2.9), so results.json is NOT written — a \
             missing identity must fail loudly, never be laundered into a \
             shared sentinel that a host check would treat as a real host."
        )
    }
}

impl std::error::Error for HostDetectionFailed {}

/// Normalise a detected host string to its **canonical form** (Stage 6 owns
/// this contract — HLD §2.9): trim surrounding whitespace, lowercase, and take
/// the **short** hostname (strip any FQDN domain, i.e. everything from the
/// first `.`). Returns `None` if nothing non-empty remains.
///
/// This is the single definition of host identity equality. `ANVIL.corp.local`,
/// `  Anvil `, and `anvil` all canonicalise to `anvil` so the Stage-8
/// host-pinning check (which MUST use this form on both sides — see
/// [`HostDetectionFailed`]) treats them as the same host. Pure; unit-tested.
fn canonical_host(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    // Short hostname: everything before the first '.' (FQDN domain stripped).
    let short = trimmed.split('.').next().unwrap_or(trimmed).trim();
    if short.is_empty() {
        None
    } else {
        Some(short.to_lowercase())
    }
}

/// Resolve the canonical host from already-collected detection candidates —
/// the **pure testable seam** for [`host_identity`]. First candidate that
/// canonicalises to a non-empty value wins; if *every* candidate is
/// absent/blank this returns [`HostDetectionFailed`] (the fail-closed state).
///
/// Taking the env/command results as an argument is what makes the
/// total-failure path unit-testable **without** actually stripping the test
/// process's machine environment (which is global, racy under parallel tests,
/// and would corrupt sibling tests).
fn resolve_host(candidates: &[Option<String>]) -> Result<String, HostDetectionFailed> {
    candidates
        .iter()
        .flatten()
        .find_map(|c| canonical_host(c))
        .ok_or(HostDetectionFailed)
}

/// Machine-readable host identity in **canonical form** (HLD §2.9 / O2),
/// discharged **dependency-free by inspecting the runtime environment** (HLD
/// §2.9 explicitly permits the env reads as runtime-environment inspection, not
/// configuration; HLD §3 — no new crate, sync/std only).
///
/// Detection order, first that canonicalises non-empty wins:
/// 1. `COMPUTERNAME` env (Windows sets this);
/// 2. `HOSTNAME` env (commonly exported on Unix shells);
/// 3. the `hostname` command (Windows/macOS/most Linux);
/// 4. `uname -n` (POSIX fallback).
///
/// **Total failure is a hard error** ([`HostDetectionFailed`]), *not* a
/// sentinel string — see that type for the collision-vector rationale and the
/// binding Stage-8 forward contract. The returned value is already canonical
/// ([`canonical_host`]): trimmed, lowercased, short hostname.
///
/// Justification for the env-first order: it is the cheapest correct signal
/// (no subprocess) and is exactly what HLD §2.9 wants — *identity for baseline
/// pinning*, not byte-stability. The subprocess fallbacks make it robust on
/// hosts that do not export the env var. The pure resolution lives in
/// [`resolve_host`] so the failure path is testable without touching the real
/// process environment.
pub fn host_identity() -> Result<String, HostDetectionFailed> {
    resolve_host(&[
        env_nonempty("COMPUTERNAME"),
        env_nonempty("HOSTNAME"),
        command_first_line("hostname", &[]),
        command_first_line("uname", &["-n"]),
    ])
}

/// A non-empty, trimmed environment variable, or `None`. Pure `std::env`.
fn env_nonempty(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) => {
            let v = v.trim();
            if v.is_empty() {
                None
            } else {
                Some(v.to_string())
            }
        }
        Err(_) => None,
    }
}

/// First non-empty trimmed line of a command's stdout, or `None` if the
/// command cannot be spawned / exits non-zero / prints nothing. Pure
/// `std::process` (no new dependency — HLD §3).
fn command_first_line(program: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(program).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// UTC timestamp `YYYY-MM-DDTHH-MM-SSZ` — filesystem-safe (uses `-` instead of
/// `:` so it doubles as the `runs/<ts>/` directory name on every OS).
///
/// Computed from [`SystemTime`] with the same self-contained civil-date
/// algorithm `main.rs` uses for `fetched_date` (Howard Hinnant's
/// `civil_from_days`), so **no date crate** is pulled in for one stamp
/// (HLD §3). `SystemTime::now()` is always post-epoch (the error path clamps
/// to 0), so the pre-epoch era branch is unreachable and omitted.
pub fn utc_timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let tod = secs % 86_400; // seconds into the day (UTC).
    let (hh, mm, ss) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    // civil_from_days (Hinnant) — post-epoch only (see doc).
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hh:02}-{mm:02}-{ss:02}Z")
}

/// The gold set (HLD §7) — hand-curated ground truth for the ~6–10 URLs where
/// neither oracle is trustworthy. Keyed by URL → expected body text.
///
/// **Minimal by design.** Only the URL→expected-text mapping the scoring
/// hierarchy needs is loaded; the `min_words`/`max_words`/`why_critical`
/// columns (HLD §7) are for the Stage-7 gold report, not the reference
/// resolution, so they are deliberately not modelled here (no premature
/// abstraction — HLD §3). An **absent** `gold/` dir or `gold.tsv` is *not* an
/// error: it yields an empty set (M1 has no gold yet — the gold-set freeze,
/// HLD §2.7, happens before crate tuning, later than M1).
#[derive(Debug, Clone, Default)]
pub struct GoldSet {
    by_url: BTreeMap<String, String>,
}

impl GoldSet {
    /// The gold expected text for `url`, if this URL is a gold member.
    pub fn text_for(&self, url: &str) -> Option<&str> {
        self.by_url.get(url).map(String::as_str)
    }

    /// Number of gold members (for the run summary / tests).
    pub fn len(&self) -> usize {
        self.by_url.len()
    }

    /// The gold member URLs (the authoritative key set), in deterministic
    /// `BTreeMap` order. **Read-only**: this exposes the existing key set for
    /// the Stage-7 report's gold/band cross-check (the report's `GoldBands`
    /// independently re-parses `gold.tsv`, so a count-preserving column swap —
    /// which §7/§2.7 explicitly anticipates as a post-freeze human edit — could
    /// silently desync band-vs-text with no parse error; the report asserts
    /// these two URL sets are equal). It does **not** touch the reference
    /// resolution / scoring / `load` logic in any way and changes no behaviour.
    pub fn urls(&self) -> impl Iterator<Item = &str> {
        self.by_url.keys().map(String::as_str)
    }

    /// Whether the gold set is empty (no curated members yet — the M1 state).
    pub fn is_empty(&self) -> bool {
        self.by_url.is_empty()
    }

    /// Load `gold/gold.tsv` + its referenced `gold/<hash>.txt` files from
    /// `corpus_dir` (HLD §7). An absent `gold/` or `gold.tsv` ⇒ an **empty**
    /// set (`Ok`, not an error — the M1 / pre-freeze state, mirroring the
    /// corpus loader's absent-manifest contract). A *present* manifest that
    /// references a **missing** expected-text file is a hard error
    /// ([`GoldError::ExpectedTextMissing`]) — the Bug-E2 backstop: a gold row
    /// that promises text which isn't there must fail loudly, never be
    /// silently dropped (which would demote a gold URL to the Trafilatura
    /// reference and *hide* a divergence the gold set exists to catch).
    pub fn load(corpus_dir: &Path) -> Result<GoldSet, GoldError> {
        let gold_dir = corpus_dir.join("gold");
        let manifest = gold_dir.join("gold.tsv");
        let text = match fs::read_to_string(&manifest) {
            Ok(t) => t,
            // Absent gold.tsv (or absent gold/ dir) ⇒ empty set, not an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GoldSet::default());
            }
            Err(e) => return Err(GoldError::Io(e)),
        };

        let mut by_url = BTreeMap::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // url, snapshot_filename, expected_text_file, min_words,
            // max_words, why_critical — exactly 6 columns (HLD §7).
            let fields: Vec<&str> = raw.split('\t').collect();
            if fields.len() != 6 {
                return Err(GoldError::MalformedRow {
                    line,
                    fields: fields.len(),
                });
            }
            let url = fields[0].to_string();
            let expected_file = fields[2];
            let expected_path = gold_dir.join(expected_file);
            let expected_text = match fs::read_to_string(&expected_path) {
                Ok(t) => t,
                Err(_) => {
                    return Err(GoldError::ExpectedTextMissing {
                        line,
                        url,
                        path: expected_path,
                    });
                }
            };
            // A present-but-empty/whitespace-only gold body is a curation
            // defect on the §7 highest-authority signal. Reject it HERE, at
            // the load boundary, symmetric with ExpectedTextMissing — never
            // let it load as Some("") and be silently demoted downstream to
            // NotScored(ReferenceEmpty), which would launder the defect (and
            // mis-attribute it to a "failed oracle reference"). This uses the
            // exact same `tokens()`-to-∅ test score_url uses for the empty
            // gate, so the two stay consistent.
            if tokens(&expected_text).is_empty() {
                return Err(GoldError::ExpectedTextEmpty {
                    line,
                    url,
                    path: expected_path,
                });
            }
            by_url.insert(url, expected_text);
        }
        Ok(GoldSet { by_url })
    }
}

/// Errors from loading the gold set (HLD §7). Rows carry the 1-based line so a
/// human editing `gold.tsv` can jump straight to it.
#[derive(Debug)]
pub enum GoldError {
    /// `gold.tsv` could not be read (distinct from absent: absence ⇒ empty set).
    Io(std::io::Error),
    /// A non-comment, non-blank row did not have exactly 6 tab-separated
    /// fields (HLD §7 columns).
    MalformedRow { line: usize, fields: usize },
    /// A row referenced a `gold/<file>.txt` that does not exist. Hard error —
    /// a gold promise that isn't kept must fail loudly (Bug-E2 backstop), not
    /// silently demote the URL to the Trafilatura reference.
    ExpectedTextMissing {
        line: usize,
        url: String,
        path: std::path::PathBuf,
    },
    /// A row's `gold/<file>.txt` **is present but empty / whitespace-only**
    /// (tokenizes to ∅). Symmetric with [`GoldError::ExpectedTextMissing`]:
    /// the §7 highest-authority signal must be non-empty by construction. An
    /// empty gold body is a *curation defect*, not a valid reference, and must
    /// fail loudly at load — never load as `Some("")` to be silently demoted
    /// downstream to `NotScored(ReferenceEmpty)` (which would launder the
    /// defect and mis-attribute it to a failed oracle reference).
    ExpectedTextEmpty {
        line: usize,
        url: String,
        path: std::path::PathBuf,
    },
}

impl fmt::Display for GoldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GoldError::Io(e) => write!(f, "reading gold/gold.tsv: {e}"),
            GoldError::MalformedRow { line, fields } => write!(
                f,
                "gold.tsv line {line}: expected 6 tab-separated fields (url, \
                 snapshot_filename, expected_text_file, min_words, max_words, \
                 why_critical), found {fields}"
            ),
            GoldError::ExpectedTextMissing { line, url, path } => write!(
                f,
                "gold.tsv line {line}: expected-text file for url {url:?} is \
                 missing — expected {}. A gold entry that promises text which \
                 is absent must fail loudly (HLD §7), not silently fall back \
                 to the Trafilatura reference.",
                path.display()
            ),
            GoldError::ExpectedTextEmpty { line, url, path } => write!(
                f,
                "gold.tsv line {line}: expected-text file for url {url:?} is \
                 present but empty / whitespace-only ({}). The gold set is the \
                 highest-authority signal and is non-empty by construction \
                 (HLD §7); an empty gold body is a curation defect and must \
                 fail loudly at load, not be silently demoted to an \
                 empty-reference non-score.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for GoldError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GoldError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Score the whole corpus into a [`RunResults`] (HLD §5/§8/§9).
///
/// For each entry, reads its snapshot, runs **both** oracles
/// ([`run_oracle`] — at M1 the adapters do not exist yet, so a spawn-fail is
/// recorded **honestly** as [`OracleStatus::OracleError`], never laundered),
/// runs the crate in-process ([`run_crate`] → [`CrateStatus::NotImplemented`]
/// at M1), and applies the [`score_url`] gate. A snapshot that cannot be read
/// is recorded as a [`CrateStatus::CrateError`] for that URL (the run
/// continues — one bad URL must not lose the differential signal for the rest;
/// mirrors the in-process panic-isolation doctrine).
///
/// At M1 every record is `crate=not_implemented`, both oracles
/// `oracle_error`, and every `score` is `NotScored` — the documented M1 floor,
/// with **nothing** laundered into a passing number.
///
/// **Fail-closed on host provenance (HLD §2.9):** the host identity is
/// resolved **first**; if no real host can be determined this returns
/// [`HostDetectionFailed`] and the caller writes **no** results.json. A run
/// with no provenance is unscorable / not baseline-eligible (see
/// [`HostDetectionFailed`] for the collision-vector rationale) — it must never
/// produce a poisoned results.json stamped with a shared sentinel.
pub fn score_corpus(
    entries: &[CorpusEntry],
    corpus_dir: &Path,
    gold: &GoldSet,
) -> Result<RunResults, HostDetectionFailed> {
    // Resolve provenance up front and fail closed: no point scoring a run that
    // can never be written (HLD §2.9 — the baseline is host-pinned). The
    // resolution is delegated to the testable inner so the no-write failure
    // path is exercisable WITHOUT stripping the real machine env (which on
    // Windows is also undefeatable via PATH — `hostname.exe` resolves from the
    // system dir regardless; the seam is the only honest test of this path).
    score_corpus_with_host(host_identity(), entries, corpus_dir, gold)
}

/// Inner of [`score_corpus`] taking the **already-resolved** host result — the
/// pure seam that makes the fail-closed "no results.json on host-detection
/// failure" path testable without touching the process environment. A
/// `Err(HostDetectionFailed)` host short-circuits **before** any scoring (and
/// therefore before the caller could ever write results.json); otherwise the
/// canonical host is stamped verbatim into the run header.
fn score_corpus_with_host(
    host: Result<String, HostDetectionFailed>,
    entries: &[CorpusEntry],
    corpus_dir: &Path,
    gold: &GoldSet,
) -> Result<RunResults, HostDetectionFailed> {
    let host = host?;

    let mut urls = Vec::with_capacity(entries.len());
    let mut status_counts = StatusCounts::default();

    for entry in entries {
        let snapshot = entry.snapshot_path(corpus_dir);
        let base_url = entry.url.as_str();

        // Read the snapshot once; feed the same bytes to both oracles (by
        // path) and the crate (in-process). An unreadable snapshot ⇒ a
        // CrateError for THIS url only (run continues).
        let html = fs::read_to_string(&snapshot).ok();

        let crate_status = match &html {
            Some(h) => run_crate(h, Some(base_url)),
            None => CrateStatus::CrateError(format!("snapshot unreadable: {}", snapshot.display())),
        };
        let trafilatura = run_oracle(OracleKind::Trafilatura, &snapshot, Some(base_url));
        let readability = run_oracle(OracleKind::ReadabilityJs, &snapshot, Some(base_url));

        let gold_text = gold.text_for(&entry.url);
        let outcome = score_url(&crate_status, &trafilatura, gold_text);
        let edit_sim = match &outcome {
            ScoreOutcome::Scored { edit_sim, .. } => Some(*edit_sim),
            ScoreOutcome::NotScored { .. } => None,
        };

        let guardrail = guardrail_flag(&trafilatura, &readability, entry.shape_class);
        let agree = agreement(&crate_status, &trafilatura, &readability);

        let cs = crate_status_str(&crate_status);
        let ts = oracle_status_str(&trafilatura);
        let rs = oracle_status_str(&readability);
        *status_counts
            .crate_status
            .entry(cs.to_string())
            .or_insert(0) += 1;
        *status_counts
            .trafilatura_status
            .entry(ts.to_string())
            .or_insert(0) += 1;
        *status_counts
            .readability_status
            .entry(rs.to_string())
            .or_insert(0) += 1;

        urls.push(UrlRecord {
            url: entry.url.clone(),
            shape_class: entry.shape_class.as_str().to_string(),
            crate_status: cs.to_string(),
            trafilatura_status: ts.to_string(),
            readability_status: rs.to_string(),
            // The *why* for any non-Ok status (HLD §5) — a failed crate /
            // oracle stays explained, never a bare token (Bug-E2 lesson).
            status_detail: StatusDetail {
                crate_reason: crate_status_detail(&crate_status).map(str::to_string),
                trafilatura_reason: oracle_status_detail(&trafilatura).map(str::to_string),
                readability_reason: oracle_status_detail(&readability).map(str::to_string),
            },
            word_counts: WordCounts {
                crate_wc: crate_text(&crate_status).map(word_count),
                trafilatura_wc: oracle_word_count(&trafilatura),
                readability_wc: oracle_word_count(&readability),
            },
            score: outcome,
            edit_sim,
            guardrail_flag: guardrail,
            agreement: agree,
        });
    }

    Ok(RunResults {
        // Canonical host resolved at the top (fail-closed); stamped verbatim
        // so the Stage-8 host-pin compare sees the canonical form (HLD §2.9).
        host,
        utc_timestamp: utc_timestamp(),
        corpus_size: entries.len(),
        status_counts,
        urls,
    })
}

/// Write `results.json` under a **unique** `runs/<utc_timestamp>[-N]/`
/// directory (HLD §9 / §4.1 — that directory is gitignored scratch). Returns
/// the file path written.
///
/// `runs_root` is `benchmark/runs/` (the caller resolves it relative to the
/// crate manifest dir — fixed convention, HLD §10).
///
/// # Collision handling — never silently overwrite a prior run
///
/// `utc_timestamp` is 1-second resolution, so two runs in the same wall-clock
/// second would otherwise resolve to the **same** directory and the second
/// `fs::write` would clobber the first run's `results.json` (silent data loss
/// for a developer comparing the last two runs). To prevent that, the run
/// directory is created with [`fs::create_dir`] — which is atomic and **fails
/// if the directory already exists** (`AlreadyExists`) rather than reusing it.
/// On a collision a numeric suffix (`-2`, `-3`, …) is appended until a fresh
/// directory is created. The bare timestamp is used for the common (no
/// collision) case, so the directory name stays human-sortable and the unique
/// suffix only appears when two runs genuinely land in the same second. The
/// atomic `create_dir` also makes the suffix search safe under the (unlikely)
/// concurrent-run race — each loser just retries the next suffix.
///
/// `runs_root` itself is created with `create_dir_all` (it is allowed to
/// pre-exist — only the per-run directory must be fresh).
pub fn write_results(
    results: &RunResults,
    runs_root: &Path,
) -> std::io::Result<std::path::PathBuf> {
    fs::create_dir_all(runs_root)?;

    // Find a fresh per-run directory. Attempt the bare timestamp first; on a
    // same-second collision append -2, -3, … . `fs::create_dir` is atomic and
    // errors `AlreadyExists` for an extant dir (it never reuses one), so this
    // can never silently overwrite a prior run and is race-safe.
    let mut suffix: u32 = 1;
    let run_dir = loop {
        let name = if suffix == 1 {
            results.utc_timestamp.clone()
        } else {
            format!("{}-{suffix}", results.utc_timestamp)
        };
        let candidate = runs_root.join(&name);
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                suffix += 1;
            }
            Err(e) => return Err(e),
        }
    };

    let path = run_dir.join("results.json");
    let json = serde_json::to_string_pretty(results)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    fs::write(&path, json)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use mdrcel::Extracted;

    /// Epsilon for non-exact f64 ratio comparisons (same rationale as
    /// `metrics.rs`: ratios of small token counts, error ≪ 1e-9).
    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // ---- Builders: synthesize statuses without spawning anything -----------

    /// An [`Extracted`] whose body is `text`. `word_count` is set to a
    /// **deliberate lie** (a huge value) so any test asserting the recomputed
    /// count proves the harness ignores the wire value (HLD §8).
    fn extracted(text: &str) -> Extracted {
        Extracted {
            title: Some("T".to_string()),
            text: text.to_string(),
            html: None,
            word_count: 999_999, // wire LIE — must never be trusted.
            canonical_url: None,
            language: Some("en".to_string()),
        }
    }

    fn crate_ok(text: &str) -> CrateStatus {
        CrateStatus::Ok(Box::new(extracted(text)))
    }

    /// An oracle `OracleResult` whose body is `text`, `ok:true`, with the
    /// wire `word_count` set to a lie (must never be trusted — HLD §8).
    fn oracle_result(oracle: &str, text: &str) -> crate::oracle::OracleResult {
        crate::oracle::OracleResult {
            oracle: oracle.to_string(),
            oracle_version: Some("9.9.9".to_string()),
            title: None,
            text: text.to_string(),
            html: None,
            word_count: Some(-12345), // wire LIE — must never be trusted.
            canonical_url: None,
            language: None,
            ok: true,
            error: None,
        }
    }

    fn oracle_ok(oracle: &str, text: &str) -> OracleStatus {
        OracleStatus::Ok(Box::new(oracle_result(oracle, text)))
    }

    // ---- Reference selection (gold present vs absent) ----------------------

    #[test]
    fn reference_is_trafilatura_when_no_gold() {
        // No gold ⇒ reference = Trafilatura's text. Crate identical to Traf ⇒
        // coverage 1.0; identical to Readability would NOT matter (Readability
        // is never the reference).
        let c = crate_ok("alpha beta gamma");
        let t = oracle_ok("trafilatura", "alpha beta gamma");
        match score_url(&c, &t, None) {
            ScoreOutcome::Scored { coverage, .. } => assert!(approx(coverage, 1.0)),
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    #[test]
    fn reference_is_gold_when_present_not_trafilatura() {
        // Gold present ⇒ reference = gold text, NOT Trafilatura. Crate matches
        // gold exactly but is DISJOINT from Trafilatura: a gold-driven 1.0
        // proves gold (not Traf) was the reference.
        let c = crate_ok("gold one gold two");
        let t = oracle_ok("trafilatura", "completely different trafilatura body");
        match score_url(&c, &t, Some("gold one gold two")) {
            ScoreOutcome::Scored {
                coverage,
                precision,
                ..
            } => {
                assert!(approx(coverage, 1.0), "coverage vs gold must be 1.0");
                assert!(approx(precision, 1.0), "precision vs gold must be 1.0");
            }
            other => panic!("expected Scored against gold, got {other:?}"),
        }
    }

    #[test]
    fn gold_url_uses_gold_even_when_trafilatura_failed() {
        // Gold present + Trafilatura OracleError: gold is still the reference
        // (gold is authoritative and valid by construction — HLD §7), so this
        // is Scored, NOT ReferenceUnavailable.
        let c = crate_ok("gold body text");
        let t = OracleStatus::OracleError("traf blew up".to_string());
        match score_url(&c, &t, Some("gold body text")) {
            ScoreOutcome::Scored { coverage, .. } => assert!(approx(coverage, 1.0)),
            other => panic!("gold must override a failed Trafilatura, got {other:?}"),
        }
    }

    // ---- Happy-path metrics with known token sets --------------------------

    #[test]
    fn scored_known_coverage_precision_edit_sim() {
        // crate = {a,b,c}, reference(Traf) = {b,c,d}.
        // coverage  = J = |∩|/|∪| = 2/4 = 0.5
        // precision = |∩|/|crate| = 2/3
        // edit_sim over sequences ["a","b","c"] vs ["b","c","d"]: lev = 2,
        //   max_len = 3 ⇒ 1 - 2/3 = 1/3.
        let c = crate_ok("a b c");
        let t = oracle_ok("trafilatura", "b c d");
        match score_url(&c, &t, None) {
            ScoreOutcome::Scored {
                coverage,
                precision,
                edit_sim,
            } => {
                assert!(approx(coverage, 0.5), "coverage was {coverage}");
                assert!(approx(precision, 2.0 / 3.0), "precision was {precision}");
                assert!(approx(edit_sim, 1.0 / 3.0), "edit_sim was {edit_sim}");
            }
            other => panic!("expected Scored, got {other:?}"),
        }
    }

    // ---- THE Bug-E2 gating matrix ------------------------------------------

    #[test]
    fn bug_e2_crate_ok_nonempty_traf_ok_nonempty_is_scored() {
        let c = crate_ok("shared words here");
        let t = oracle_ok("trafilatura", "shared words here");
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::Scored { .. }
        ));
    }

    #[test]
    fn bug_e2_crate_ok_empty_vs_traf_ok_empty_is_not_scored_not_one() {
        // THE key test. crate Ok("") vs Trafilatura Ok(""): jaccard(∅,∅)=1.0
        // is mathematically correct but MEANING-AMBIGUOUS (metrics.rs
        // # HAZARD). It MUST surface as NotScored(ReferenceEmpty), NEVER as
        // Coverage=1.0 "perfect". This is the entire reason ScoreOutcome
        // exists.
        let c = crate_ok("");
        let t = oracle_ok("trafilatura", "");
        match score_url(&c, &t, None) {
            ScoreOutcome::NotScored {
                reason: NotScoredReason::ReferenceEmpty,
            } => {}
            ScoreOutcome::Scored { coverage, .. } => panic!(
                "Bug-E2 LAUNDERING: crate Ok(\"\") vs Traf Ok(\"\") surfaced \
                 as Scored coverage={coverage} — must be NotScored, never 1.0"
            ),
            other => panic!("expected NotScored(ReferenceEmpty), got {other:?}"),
        }
    }

    #[test]
    fn bug_e2_crate_ok_nonempty_vs_traf_ok_empty_is_not_scored() {
        // Non-empty crate against an EMPTY (but ok) Trafilatura reference:
        // jaccard(x,∅)=0.0 is also meaning-ambiguous ⇒ NotScored, not a
        // trusted 0.0.
        let c = crate_ok("the crate found plenty");
        let t = oracle_ok("trafilatura", "   \t\n  "); // tokenizes to ∅
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::ReferenceEmpty
            }
        ));
    }

    #[test]
    fn bug_e2_crate_ok_vs_traf_oracle_error_is_not_scored() {
        let c = crate_ok("crate text");
        let t = OracleStatus::OracleError("spawn failed: python not found".to_string());
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::ReferenceUnavailable
            }
        ));
    }

    #[test]
    fn bug_e2_crate_ok_vs_traf_oracle_timeout_is_not_scored() {
        // Timeout is a DISTINCT status (not folded into error) but it is still
        // an unavailable reference ⇒ NotScored(ReferenceUnavailable).
        let c = crate_ok("crate text");
        let t = OracleStatus::OracleTimeout("exceeded 180 s".to_string());
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::ReferenceUnavailable
            }
        ));
    }

    #[test]
    fn bug_e2_crate_not_implemented_is_not_scored_status_recorded() {
        // The M1 floor. Crate NotImplemented ⇒ NotScored(CrateNotImplemented).
        // Even against a perfectly good Trafilatura reference — a missing
        // algorithm is NEVER laundered into a score.
        let c = CrateStatus::NotImplemented;
        let t = oracle_ok("trafilatura", "a real reference body");
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateNotImplemented
            }
        ));
    }

    #[test]
    fn bug_e2_crate_error_is_not_scored() {
        let c = CrateStatus::CrateError("panic: boom".to_string());
        let t = oracle_ok("trafilatura", "a real reference body");
        assert!(matches!(
            score_url(&c, &t, None),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateError
            }
        ));
    }

    #[test]
    fn bug_e2_crate_ok_empty_vs_traf_nonempty_is_trusted_zero_not_not_scored() {
        // The DISTINCTION that proves we do not over-gate: crate Ok("")
        // against a REAL non-empty reference is a TRUSTED coverage = 0.0 (the
        // crate genuinely extracted nothing vs a valid reference), NOT
        // NotScored. This must be a real, meaningful 0.0 — different from the
        // empty-reference laundering case above.
        let c = crate_ok("");
        let t = oracle_ok("trafilatura", "a genuine non empty reference");
        match score_url(&c, &t, None) {
            ScoreOutcome::Scored {
                coverage,
                precision,
                ..
            } => {
                assert!(approx(coverage, 0.0), "trusted zero coverage");
                assert!(approx(precision, 0.0), "trusted zero precision");
            }
            other => panic!(
                "crate Ok(\"\") vs a REAL reference must be a trusted 0.0, \
                 not NotScored; got {other:?}"
            ),
        }
    }

    #[test]
    fn bug_e2_gold_url_crate_not_implemented_still_not_scored() {
        // Gold does NOT bypass the crate gate: a gold URL with the crate at
        // the M1 floor is still NotScored(CrateNotImplemented). (Reference
        // resolution never even runs — the crate gate is first.)
        let c = CrateStatus::NotImplemented;
        let t = oracle_ok("trafilatura", "irrelevant");
        assert!(matches!(
            score_url(&c, &t, Some("gold expected text")),
            ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateNotImplemented
            }
        ));
    }

    // ---- Word counts come from metrics, NOT the wire values ----------------

    #[test]
    fn word_counts_recomputed_via_metrics_not_wire() {
        // Builders set wire word_count to lies (Extracted 999_999, OracleResult
        // -12345). The recomputed counts must be the true token counts.
        let c = crate_ok("one two three"); // 3 tokens
        let t = oracle_ok("trafilatura", "alpha beta"); // 2 tokens
        let r = oracle_ok("readability-js", "x y z w"); // 4 tokens

        assert_eq!(crate_text(&c).map(word_count), Some(3));
        assert_eq!(oracle_word_count(&t), Some(2));
        assert_eq!(oracle_word_count(&r), Some(4));
        // And explicitly NOT the wire lies.
        assert_ne!(crate_text(&c).map(word_count), Some(999_999));
        assert_ne!(oracle_word_count(&t), Some(0)); // -12345 i64 never surfaces
    }

    #[test]
    fn word_counts_are_none_when_producer_not_ok() {
        let ni = CrateStatus::NotImplemented;
        let err = OracleStatus::OracleError("x".to_string());
        let to = OracleStatus::OracleTimeout("x".to_string());
        assert_eq!(crate_text(&ni).map(word_count), None);
        assert_eq!(oracle_word_count(&err), None);
        assert_eq!(oracle_word_count(&to), None);
    }

    // ---- Guardrail flag ----------------------------------------------------

    #[test]
    fn guardrail_fires_when_readability_exceeds_1_25x_on_non_hub() {
        // Traf 4 words, Readability 6 words. 6 > 4 * 1.25 (= 5.0) ⇒ fire on a
        // non-hub page.
        let t = oracle_ok("trafilatura", "a b c d");
        let r = oracle_ok("readability-js", "a b c d e f");
        assert!(guardrail_flag(&t, &r, ShapeClass::News));
        assert!(guardrail_flag(&t, &r, ShapeClass::Wikipedia));
    }

    #[test]
    fn guardrail_does_not_fire_on_hub_index_even_when_exceeded() {
        // Same ratio that fires above, but hub_index is excluded by design
        // (a hub legitimately has Readability ≫ Trafilatura — HLD §8).
        let t = oracle_ok("trafilatura", "a b c d");
        let r = oracle_ok("readability-js", "a b c d e f g h i j");
        assert!(
            !guardrail_flag(&t, &r, ShapeClass::HubIndex),
            "hub_index must never raise the guardrail"
        );
    }

    #[test]
    fn guardrail_boundary_is_strict_greater_than() {
        // Exactly 1.25×: Traf 4 → threshold 5.0; Readability exactly 5 words
        // is NOT > 5.0 ⇒ must NOT fire (documented strict `>`).
        let t = oracle_ok("trafilatura", "a b c d");
        let r5 = oracle_ok("readability-js", "a b c d e");
        assert!(
            !guardrail_flag(&t, &r5, ShapeClass::News),
            "exactly 1.25× must not fire (strict >)"
        );
        // One more word does fire.
        let r6 = oracle_ok("readability-js", "a b c d e f");
        assert!(guardrail_flag(&t, &r6, ShapeClass::News));
    }

    #[test]
    fn guardrail_requires_both_oracles_ok() {
        let r = oracle_ok("readability-js", "a b c d e f g h");
        let t_err = OracleStatus::OracleError("x".to_string());
        let t_to = OracleStatus::OracleTimeout("x".to_string());
        assert!(!guardrail_flag(&t_err, &r, ShapeClass::News));
        assert!(!guardrail_flag(&t_to, &r, ShapeClass::News));
        // Readability failed instead.
        let t = oracle_ok("trafilatura", "a b");
        let r_err = OracleStatus::OracleError("x".to_string());
        assert!(!guardrail_flag(&t, &r_err, ShapeClass::News));
    }

    // ---- Agreement-on-disagreement -----------------------------------------

    #[test]
    fn agreement_only_when_all_three_valid_and_oracles_disagree() {
        // Traf = {a,b,c,d}, Read = {w,x,y,z}: jaccard = 0 < 0.5 ⇒ disagree.
        // Crate = {a,b,c,e}: closer to Traf (J=3/5) than Read (J=0) ⇒
        // CloserToTrafilatura.
        let c = crate_ok("a b c e");
        let t = oracle_ok("trafilatura", "a b c d");
        let r = oracle_ok("readability-js", "w x y z");
        assert_eq!(agreement(&c, &t, &r), Some(Agreement::CloserToTrafilatura));

        // Crate closer to Readability.
        let c2 = crate_ok("w x y v");
        assert_eq!(agreement(&c2, &t, &r), Some(Agreement::CloserToReadability));
    }

    #[test]
    fn agreement_none_when_oracles_agree() {
        // Traf and Read nearly identical ⇒ jaccard ≥ 0.5 ⇒ not applicable.
        let c = crate_ok("a b c");
        let t = oracle_ok("trafilatura", "a b c d");
        let r = oracle_ok("readability-js", "a b c d");
        assert_eq!(agreement(&c, &t, &r), None);
    }

    #[test]
    fn agreement_none_when_any_side_missing_or_empty() {
        let t = oracle_ok("trafilatura", "a b c d");
        let r = oracle_ok("readability-js", "w x y z");
        // Crate not Ok.
        assert_eq!(agreement(&CrateStatus::NotImplemented, &t, &r), None);
        // An oracle not Ok.
        assert_eq!(
            agreement(
                &crate_ok("a b"),
                &OracleStatus::OracleError("x".to_string()),
                &r
            ),
            None
        );
        // Crate text empty.
        assert_eq!(agreement(&crate_ok(""), &t, &r), None);
        // An oracle text empty.
        let t_empty = oracle_ok("trafilatura", "");
        assert_eq!(agreement(&crate_ok("a b"), &t_empty, &r), None);
    }

    #[test]
    fn agreement_tie_when_equidistant() {
        // Traf = {a,b}, Read = {c,d}: disjoint ⇒ disagree. Crate = {a,c}:
        // J(crate,Traf) = 1/3, J(crate,Read) = 1/3 ⇒ Tie.
        let c = crate_ok("a c");
        let t = oracle_ok("trafilatura", "a b");
        let r = oracle_ok("readability-js", "c d");
        assert_eq!(agreement(&c, &t, &r), Some(Agreement::Tie));
    }

    // ---- Status string projection ------------------------------------------

    #[test]
    fn status_strings_are_the_wire_spellings() {
        assert_eq!(crate_status_str(&crate_ok("x")), "ok");
        assert_eq!(
            crate_status_str(&CrateStatus::NotImplemented),
            "not_implemented"
        );
        assert_eq!(
            crate_status_str(&CrateStatus::CrateError("e".to_string())),
            "crate_error"
        );
        assert_eq!(oracle_status_str(&oracle_ok("trafilatura", "x")), "ok");
        assert_eq!(
            oracle_status_str(&OracleStatus::OracleError("e".to_string())),
            "oracle_error"
        );
        assert_eq!(
            oracle_status_str(&OracleStatus::OracleTimeout("t".to_string())),
            "oracle_timeout"
        );
    }

    #[test]
    fn status_detail_surfaces_the_non_ok_reason_not_a_bare_token() {
        // Anti-Bug-E2: a failed crate/oracle must stay EXPLAINED in
        // results.json, never silently reduced to a bare status token. The
        // reason String is carried through verbatim.
        assert_eq!(crate_status_detail(&crate_ok("x")), None);
        assert_eq!(crate_status_detail(&CrateStatus::NotImplemented), None);
        assert_eq!(
            crate_status_detail(&CrateStatus::CrateError(
                "panic: boom in extract".to_string()
            )),
            Some("panic: boom in extract")
        );
        assert_eq!(oracle_status_detail(&oracle_ok("trafilatura", "x")), None);
        assert_eq!(
            oracle_status_detail(&OracleStatus::OracleError(
                "failed to spawn `python`".to_string()
            )),
            Some("failed to spawn `python`")
        );
        assert_eq!(
            oracle_status_detail(&OracleStatus::OracleTimeout(
                "exceeded the 180 s wall-clock timeout".to_string()
            )),
            Some("exceeded the 180 s wall-clock timeout")
        );
    }

    // ---- Host identity + timestamp -----------------------------------------

    #[test]
    fn host_identity_is_ok_and_canonical_on_this_host() {
        // O2: on the dev/CI host at least one of the env vars or the
        // `hostname`/`uname -n` fallbacks resolves, so this is Ok(_). The
        // returned value is already canonical: non-empty, trimmed, lowercase,
        // and the short hostname (no FQDN domain).
        let h = host_identity().expect("dev/CI host always has an identity");
        assert!(!h.is_empty(), "host identity must never be empty");
        assert_eq!(h, h.trim(), "host identity must be trimmed");
        assert_eq!(h, h.to_lowercase(), "host identity must be lowercase");
        assert!(
            !h.contains('.'),
            "host identity must be the short hostname (no FQDN): {h:?}"
        );
    }

    #[test]
    fn resolve_host_total_detection_failure_is_hard_error_not_sentinel() {
        // THE Risk-1 test. Every detection candidate absent/blank ⇒ a hard,
        // non-comparable error — NEVER a shared `"unknown-host"` sentinel that
        // a Stage-8 `host == host` check would treat as a real, equal host
        // (the collision vector that defeats §2.9 single-host repro).
        //
        // Injected via the pure `resolve_host` seam so the failure path is
        // exercised WITHOUT stripping this process's real machine env (which
        // is global and would corrupt sibling tests under parallel runs).
        let all_absent: [Option<String>; 4] = [None, None, None, None];
        assert!(
            matches!(resolve_host(&all_absent), Err(HostDetectionFailed)),
            "all candidates absent must be the fail-closed hard error"
        );
        // Blank / whitespace-only candidates are equivalent to absent (they
        // canonicalise to nothing): still the hard error, never a sentinel.
        let all_blank = [
            Some(String::new()),
            Some("   ".to_string()),
            Some("\t\n".to_string()),
            Some(".".to_string()), // FQDN-strips to empty
        ];
        assert!(
            matches!(resolve_host(&all_blank), Err(HostDetectionFailed)),
            "blank/whitespace candidates must also fail closed"
        );
        // And the Display is loud + explains why no results.json is written.
        let msg = HostDetectionFailed.to_string();
        assert!(msg.contains("host"), "message must mention host: {msg}");
        assert!(
            msg.contains("results.json"),
            "message must say results.json is not written: {msg}"
        );
    }

    #[test]
    fn resolve_host_first_canonicalisable_candidate_wins() {
        // First candidate that canonicalises non-empty wins; earlier blank
        // ones are skipped (mirrors the env-first precedence in host_identity).
        let got = resolve_host(&[
            Some("   ".to_string()), // blank ⇒ skipped
            None,                    // absent ⇒ skipped
            Some("ANVIL.corp.local".to_string()),
            Some("ignored-host".to_string()),
        ])
        .expect("a real candidate is present");
        assert_eq!(got, "anvil", "must take + canonicalise the first non-blank");
    }

    #[test]
    fn canonical_host_normalises_case_whitespace_and_strips_fqdn() {
        // Stage-6 owns this contract (HLD §2.9): trim, lowercase, short
        // hostname. These three MUST collapse to the same canonical identity
        // so the Stage-8 host-pin compare treats them as one host.
        assert_eq!(canonical_host("ANVIL.corp.local").as_deref(), Some("anvil"));
        assert_eq!(canonical_host("  Anvil ").as_deref(), Some("anvil"));
        assert_eq!(canonical_host("anvil").as_deref(), Some("anvil"));
        // Mixed case + multi-label FQDN + surrounding space — all → "anvil".
        assert_eq!(
            canonical_host("  AnViL.eng.example.COM  ").as_deref(),
            Some("anvil")
        );
        // Nothing real ⇒ None (the resolve_host fail-closed feeder).
        assert_eq!(canonical_host(""), None);
        assert_eq!(canonical_host("   \t "), None);
        assert_eq!(canonical_host("."), None); // FQDN-strips to empty
        assert_eq!(canonical_host("  .  "), None);
    }

    #[test]
    fn host_detection_failure_yields_hard_error_and_writes_no_results_json() {
        // End-to-end fail-closed contract (Risk 1), exercised via the
        // `score_corpus_with_host` seam so the run-level path is tested
        // WITHOUT stripping the machine env (on Windows the env/PATH cannot
        // even simulate this — `hostname.exe` resolves from the system dir
        // regardless). Drive the exact sequence main.rs uses: score_corpus →
        // (only on Ok) write_results. A failed host must short-circuit with
        // the hard error and NEVER reach write_results ⇒ no results.json.
        let dir = std::env::temp_dir().join("mdrcel-host-fail-nowrite");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("snapshots")).unwrap();
        let url = "https://example.test/hostfail";
        let entry = CorpusEntry {
            url: url.to_string(),
            shape_class: ShapeClass::Wikipedia,
            snapshot_filename: crate::corpus::snapshot_filename(url),
            fetched_date: "2026-05-17".to_string(),
            note: String::new(),
        };
        let runs_root = dir.join("runs");

        // Inject a total-detection-failure host (the resolve_host fail-closed
        // state) and run the SAME pipeline main.rs runs.
        let scored = score_corpus_with_host(
            Err(HostDetectionFailed),
            std::slice::from_ref(&entry),
            &dir,
            &GoldSet::default(),
        );
        assert!(
            matches!(scored, Err(HostDetectionFailed)),
            "a detection-failure host must be the hard error, never an Ok run"
        );
        // main.rs returns FAILURE here and NEVER calls write_results — so no
        // runs/ directory and no results.json is ever produced. Assert that.
        if let Ok(results) = &scored {
            let _ = write_results(results, &runs_root); // unreachable by contract
        }
        assert!(
            !runs_root.exists(),
            "a poisoned results.json must NEVER be written on host-detection \
             failure (runs/ must not exist)"
        );

        // Sanity: the SAME pipeline with a real resolved host DOES write a
        // results.json (proves the no-write above is the host gate, not a
        // broken harness).
        let ok = score_corpus_with_host(
            Ok("anvil".to_string()),
            std::slice::from_ref(&entry),
            &dir,
            &GoldSet::default(),
        )
        .expect("a real host ⇒ Ok run");
        let p = write_results(&ok, &runs_root).expect("real host ⇒ results.json written");
        assert!(p.is_file());
        assert_eq!(ok.host, "anvil", "the canonical host is stamped verbatim");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn utc_timestamp_is_filesystem_safe_and_well_formed() {
        let ts = utc_timestamp();
        // YYYY-MM-DDTHH-MM-SSZ — 20 chars, no ':' (dir-name safe).
        assert!(!ts.contains(':'), "timestamp must be filesystem-safe: {ts}");
        assert!(ts.ends_with('Z'), "timestamp must end Z: {ts}");
        assert_eq!(ts.len(), 20, "unexpected timestamp shape: {ts}");
        let (date, rest) = ts.split_once('T').expect("has a T separator");
        let dparts: Vec<&str> = date.split('-').collect();
        assert_eq!(dparts.len(), 3);
        let y: i64 = dparts[0].parse().unwrap();
        assert!((2020..=2100).contains(&y), "year out of range: {y}");
        // HH-MM-SSZ
        let t = rest.trim_end_matches('Z');
        let tparts: Vec<&str> = t.split('-').collect();
        assert_eq!(tparts.len(), 3);
        let hh: i64 = tparts[0].parse().unwrap();
        let mm: i64 = tparts[1].parse().unwrap();
        let ss: i64 = tparts[2].parse().unwrap();
        assert!((0..24).contains(&hh), "hour out of range: {hh}");
        assert!((0..60).contains(&mm), "minute out of range: {mm}");
        assert!((0..60).contains(&ss), "second out of range: {ss}");
    }

    // ---- results.json serialization round-trip -----------------------------

    #[test]
    fn run_results_round_trips_through_serde() {
        let results = RunResults {
            host: "test-host".to_string(),
            utc_timestamp: "2026-05-17T12-00-00Z".to_string(),
            corpus_size: 2,
            status_counts: {
                let mut sc = StatusCounts::default();
                sc.crate_status.insert("not_implemented".to_string(), 2);
                sc.trafilatura_status.insert("oracle_error".to_string(), 2);
                sc.readability_status.insert("oracle_error".to_string(), 2);
                sc
            },
            urls: vec![
                UrlRecord {
                    url: "https://example.test/a".to_string(),
                    shape_class: "wikipedia".to_string(),
                    crate_status: "not_implemented".to_string(),
                    trafilatura_status: "oracle_error".to_string(),
                    readability_status: "oracle_error".to_string(),
                    status_detail: StatusDetail {
                        crate_reason: None, // not_implemented carries no reason
                        trafilatura_reason: Some("spawn failed".to_string()),
                        readability_reason: Some("spawn failed".to_string()),
                    },
                    word_counts: WordCounts {
                        crate_wc: None,
                        trafilatura_wc: None,
                        readability_wc: None,
                    },
                    score: ScoreOutcome::NotScored {
                        reason: NotScoredReason::CrateNotImplemented,
                    },
                    edit_sim: None,
                    guardrail_flag: false,
                    agreement: None,
                },
                UrlRecord {
                    url: "https://example.test/b".to_string(),
                    shape_class: "news".to_string(),
                    crate_status: "ok".to_string(),
                    trafilatura_status: "ok".to_string(),
                    readability_status: "ok".to_string(),
                    status_detail: StatusDetail::default(), // all Ok ⇒ no reasons
                    word_counts: WordCounts {
                        crate_wc: Some(10),
                        trafilatura_wc: Some(12),
                        readability_wc: Some(11),
                    },
                    score: ScoreOutcome::Scored {
                        coverage: 0.5,
                        precision: 0.75,
                        edit_sim: 0.6,
                    },
                    edit_sim: Some(0.6),
                    guardrail_flag: true,
                    agreement: Some(Agreement::CloserToTrafilatura),
                },
            ],
        };

        let json = serde_json::to_string_pretty(&results).expect("serialize");
        let back: RunResults = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(results, back, "results.json must round-trip exactly");

        // The tagged ScoreOutcome must be unambiguous in the JSON: a
        // NotScored is never confusable with a Scored 0.0/1.0.
        assert!(
            json.contains("\"outcome\": \"not_scored\""),
            "NotScored must serialize with an explicit discriminant: {json}"
        );
        assert!(json.contains("\"outcome\": \"scored\""));
        assert!(json.contains("\"reason\": \"crate_not_implemented\""));
    }

    // ---- GoldSet loading ---------------------------------------------------

    #[test]
    fn gold_absent_dir_is_empty_set_not_error() {
        // M1 / pre-freeze: no gold/ dir ⇒ empty set, Ok (mirrors the corpus
        // loader's absent-manifest contract).
        let dir = std::env::temp_dir().join("mdrcel-gold-absent-xyz");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let g = GoldSet::load(&dir).expect("absent gold ⇒ Ok(empty)");
        assert!(g.is_empty());
        assert_eq!(g.len(), 0);
        assert_eq!(g.text_for("anything"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gold_loads_url_to_text_mapping() {
        let dir = std::env::temp_dir().join("mdrcel-gold-load-ok");
        let _ = fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        fs::create_dir_all(&gold_dir).unwrap();
        fs::write(gold_dir.join("apple.txt"), "the apple gold body").unwrap();
        // url, snapshot_filename, expected_text_file, min, max, why
        let manifest = "# gold\n\
            https://en.wiki.test/Apple\tdeadbeef.html\tapple.txt\t10\t99\tconsumer\n";
        fs::write(gold_dir.join("gold.tsv"), manifest).unwrap();

        let g = GoldSet::load(&dir).expect("valid gold.tsv");
        assert_eq!(g.len(), 1);
        assert_eq!(
            g.text_for("https://en.wiki.test/Apple"),
            Some("the apple gold body")
        );
        assert_eq!(g.text_for("https://other.test/"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn goldset_urls_iterates_the_key_set_deterministically() {
        // The read-only accessor used by the Stage-7 gold/band cross-check:
        // it yields exactly the loaded URL keys, in BTreeMap (deterministic)
        // order, and does not alter the URL→text mapping.
        let dir = std::env::temp_dir().join("mdrcel-goldset-urls");
        let _ = fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        fs::create_dir_all(&gold_dir).unwrap();
        fs::write(gold_dir.join("b.txt"), "beta body").unwrap();
        fs::write(gold_dir.join("a.txt"), "alpha body").unwrap();
        // Inserted b-then-a; BTreeMap must yield a-then-b.
        let manifest = "https://z.test/b\tx.html\tb.txt\t1\t9\twhy\n\
            https://z.test/a\ty.html\ta.txt\t1\t9\twhy\n";
        fs::write(gold_dir.join("gold.tsv"), manifest).unwrap();

        let g = GoldSet::load(&dir).expect("valid gold.tsv");
        let urls: Vec<&str> = g.urls().collect();
        assert_eq!(urls, vec!["https://z.test/a", "https://z.test/b"]);
        // Accessor is purely a view: the text mapping is unchanged.
        assert_eq!(g.text_for("https://z.test/a"), Some("alpha body"));
        assert_eq!(g.text_for("https://z.test/b"), Some("beta body"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gold_missing_expected_text_is_hard_error() {
        // A gold row that references an absent <hash>.txt is a hard error —
        // never silently dropped (that would demote the URL to the Traf
        // reference and HIDE the divergence gold exists to catch). Bug-E2.
        let dir = std::env::temp_dir().join("mdrcel-gold-missing-txt");
        let _ = fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        fs::create_dir_all(&gold_dir).unwrap();
        let manifest = "https://x.test/a\tdeadbeef.html\tabsent.txt\t1\t9\twhy\n";
        fs::write(gold_dir.join("gold.tsv"), manifest).unwrap();
        // Deliberately do NOT create absent.txt.
        match GoldSet::load(&dir) {
            Err(GoldError::ExpectedTextMissing { line, url, .. }) => {
                assert_eq!(line, 1);
                assert_eq!(url, "https://x.test/a");
            }
            other => panic!("expected ExpectedTextMissing, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gold_present_but_empty_expected_text_is_hard_error() {
        // Symmetric with gold_missing_expected_text_is_hard_error: a present
        // but empty / whitespace-only gold body is ALSO a hard error at the
        // load boundary — never loaded as Some("") and silently demoted
        // downstream to NotScored(ReferenceEmpty). The §7 highest-authority
        // signal is non-empty by construction; an empty gold body is a
        // curation defect that must fail loudly, not be laundered (and
        // mis-attributed to a failed oracle reference).
        let dir = std::env::temp_dir().join("mdrcel-gold-empty-txt");
        let _ = fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        fs::create_dir_all(&gold_dir).unwrap();
        // Whitespace-only body: present (so NOT ExpectedTextMissing), but
        // tokenizes to ∅ (the exact score_url empty-gate predicate).
        fs::write(gold_dir.join("blank.txt"), "  \t\n  \r\n ").unwrap();
        let manifest = "https://x.test/empty\tdeadbeef.html\tblank.txt\t1\t9\twhy\n";
        fs::write(gold_dir.join("gold.tsv"), manifest).unwrap();
        match GoldSet::load(&dir) {
            Err(GoldError::ExpectedTextEmpty { line, url, path }) => {
                assert_eq!(line, 1);
                assert_eq!(url, "https://x.test/empty");
                assert!(path.ends_with("blank.txt"));
            }
            other => panic!("expected ExpectedTextEmpty, got {other:?}"),
        }
        // Truly-empty (zero-byte) file is the same defect, same hard error.
        fs::write(gold_dir.join("blank.txt"), "").unwrap();
        assert!(
            matches!(
                GoldSet::load(&dir),
                Err(GoldError::ExpectedTextEmpty { .. })
            ),
            "a zero-byte gold body must also be a hard error"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn gold_malformed_row_is_error_with_line() {
        let dir = std::env::temp_dir().join("mdrcel-gold-malformed");
        let _ = fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        fs::create_dir_all(&gold_dir).unwrap();
        // 3 fields, not 6.
        fs::write(gold_dir.join("gold.tsv"), "u\tf\tt\n").unwrap();
        match GoldSet::load(&dir) {
            Err(GoldError::MalformedRow { line, fields }) => {
                assert_eq!(line, 1);
                assert_eq!(fields, 3);
            }
            other => panic!("expected MalformedRow, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&dir);
    }

    // ---- score_corpus end-to-end (M1 floor — nothing laundered) ------------

    #[test]
    fn score_corpus_m1_floor_nothing_laundered() {
        // Build a tiny real corpus on disk: one snapshot file, one entry.
        // At M1: crate = NotImplemented (real mdrcel::extract), oracles =
        // OracleError (python/node adapters absent ⇒ spawn-fail, recorded
        // HONESTLY). The record MUST be NotScored with NOTHING laundered to a
        // passing number. This is the M1 baseline floor.
        let dir = std::env::temp_dir().join("mdrcel-score-corpus-m1");
        let _ = fs::remove_dir_all(&dir);
        let snaps = dir.join("snapshots");
        fs::create_dir_all(&snaps).unwrap();
        let url = "https://example.test/m1";
        let fname = crate::corpus::snapshot_filename(url);
        fs::write(
            snaps.join(&fname),
            "<html><body><article><p>hello world</p></article></body></html>",
        )
        .unwrap();

        let entry = CorpusEntry {
            url: url.to_string(),
            shape_class: ShapeClass::Wikipedia,
            snapshot_filename: fname,
            fetched_date: "2026-05-17".to_string(),
            note: String::new(),
        };
        let gold = GoldSet::default();
        // The dev/CI host always resolves, so score_corpus is Ok here; the
        // fail-closed host-detection path is unit-tested via `resolve_host`.
        let results = score_corpus(std::slice::from_ref(&entry), &dir, &gold)
            .expect("dev/CI host always resolves");

        assert_eq!(results.corpus_size, 1);
        assert_eq!(results.urls.len(), 1);
        assert!(!results.host.is_empty());
        // The stamped host is the canonical form (HLD §2.9): lowercase, short.
        assert_eq!(results.host, results.host.to_lowercase());
        assert!(!results.host.contains('.'), "host must be the short name");
        let rec = &results.urls[0];
        assert_eq!(rec.crate_status, "not_implemented");
        // The oracle adapters (benchmark/oracles/**) do not exist at M1, so
        // `run_oracle` either fails to spawn `python`/`node` OR they exit
        // non-zero — EITHER way the honest record is `oracle_error` (never
        // laundered into ok-with-empty). Timeout is also acceptable in
        // principle but not expected here; assert it is one of the non-ok
        // statuses and specifically not `ok`.
        assert_ne!(rec.trafilatura_status, "ok");
        assert_ne!(rec.readability_status, "ok");
        assert_eq!(rec.trafilatura_status, "oracle_error");
        assert_eq!(rec.readability_status, "oracle_error");
        // ANTI-LAUNDERING: the failure stays EXPLAINED — a non-empty reason
        // is recorded, not a bare token (HLD §5; Bug-E2 lesson).
        assert!(
            rec.status_detail
                .trafilatura_reason
                .as_deref()
                .is_some_and(|r| !r.is_empty()),
            "a failed oracle must carry a non-empty reason, got {:?}",
            rec.status_detail.trafilatura_reason
        );
        assert!(
            rec.status_detail
                .readability_reason
                .as_deref()
                .is_some_and(|r| !r.is_empty())
        );
        // The crate is `not_implemented` (a reason-less status) ⇒ no
        // free-text crate reason (distinct from a `crate_error`, which would).
        assert_eq!(rec.status_detail.crate_reason, None);
        // NOTHING laundered: the score is NotScored, the crate gate fired
        // FIRST (NotImplemented), so the reason is CrateNotImplemented — never
        // a Scored 0.0/1.0.
        match &rec.score {
            ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateNotImplemented,
            } => {}
            other => panic!("M1 floor must be NotScored(CrateNotImplemented), got {other:?}"),
        }
        assert_eq!(rec.edit_sim, None);
        assert!(!rec.guardrail_flag);
        assert_eq!(rec.agreement, None);
        // Word counts: crate is not Ok ⇒ None; oracles failed ⇒ None. No wire
        // value ever surfaces.
        assert_eq!(rec.word_counts.crate_wc, None);
        assert_eq!(rec.word_counts.trafilatura_wc, None);
        assert_eq!(rec.word_counts.readability_wc, None);
        // Status counts reflect exactly the M1 floor.
        assert_eq!(
            results.status_counts.crate_status.get("not_implemented"),
            Some(&1)
        );
        assert_eq!(
            results.status_counts.trafilatura_status.get("oracle_error"),
            Some(&1)
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn score_corpus_unreadable_snapshot_is_crate_error_run_continues() {
        // A manifest entry whose snapshot is absent on disk: score_corpus must
        // record a CrateError for THAT url (not panic, not skip) and continue.
        // (load_checked normally prevents this upstream, but score_corpus must
        // be robust if called with a stale entry — one bad URL must not lose
        // the whole differential signal.)
        let dir = std::env::temp_dir().join("mdrcel-score-corpus-badsnap");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("snapshots")).unwrap();
        let url = "https://example.test/missing-snap";
        let entry = CorpusEntry {
            url: url.to_string(),
            shape_class: ShapeClass::EdgeCase,
            snapshot_filename: crate::corpus::snapshot_filename(url),
            fetched_date: "2026-05-17".to_string(),
            note: String::new(),
        };
        let results =
            score_corpus(&[entry], &dir, &GoldSet::default()).expect("dev/CI host always resolves");
        assert_eq!(results.urls[0].crate_status, "crate_error");
        match &results.urls[0].score {
            ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateError,
            } => {}
            other => panic!("expected NotScored(CrateError), got {other:?}"),
        }
        // The crate error stays EXPLAINED — the unreadable-snapshot reason is
        // surfaced, not a bare `crate_error` (anti-Bug-E2; HLD §5).
        assert!(
            results.urls[0]
                .status_detail
                .crate_reason
                .as_deref()
                .is_some_and(|r| r.contains("snapshot unreadable")),
            "crate_error must carry the unreadable-snapshot reason, got {:?}",
            results.urls[0].status_detail.crate_reason
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_results_writes_under_runs_timestamp_dir() {
        let tmp = std::env::temp_dir().join("mdrcel-write-results");
        let _ = fs::remove_dir_all(&tmp);
        let results = RunResults {
            host: "h".to_string(),
            utc_timestamp: "2026-05-17T01-02-03Z".to_string(),
            corpus_size: 0,
            status_counts: StatusCounts::default(),
            urls: vec![],
        };
        let path = write_results(&results, &tmp).expect("write");
        assert!(path.is_file());
        assert!(path.ends_with("results.json"));
        assert_eq!(
            path.parent().unwrap().file_name().unwrap(),
            std::ffi::OsStr::new("2026-05-17T01-02-03Z")
        );
        // It must be valid JSON that round-trips.
        let txt = fs::read_to_string(&path).unwrap();
        let back: RunResults = serde_json::from_str(&txt).unwrap();
        assert_eq!(back, results);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn write_results_same_second_runs_do_not_overwrite() {
        // Risk-7a: two runs in the SAME wall-clock second (identical
        // utc_timestamp) must NOT collide — the second write must land in a
        // DISTINCT directory, never silently clobber the first run's
        // results.json (data loss for a developer comparing the last two
        // runs). We force the collision by reusing the exact same timestamp.
        let tmp = std::env::temp_dir().join("mdrcel-write-results-collision");
        let _ = fs::remove_dir_all(&tmp);
        let ts = "2026-05-17T09-09-09Z";

        let r1 = RunResults {
            host: "host-a".to_string(),
            utc_timestamp: ts.to_string(),
            corpus_size: 1,
            status_counts: StatusCounts::default(),
            urls: vec![],
        };
        let mut r2 = r1.clone();
        r2.host = "host-b".to_string(); // distinguishable payload
        r2.corpus_size = 2;

        let p1 = write_results(&r1, &tmp).expect("first write");
        let p2 = write_results(&r2, &tmp).expect("second write, same second");

        // Distinct directories — no silent overwrite.
        assert_ne!(
            p1.parent().unwrap(),
            p2.parent().unwrap(),
            "two same-second runs must not share a run directory"
        );
        // The first run's file is intact (NOT clobbered by the second).
        let back1: RunResults = serde_json::from_str(&fs::read_to_string(&p1).unwrap()).unwrap();
        let back2: RunResults = serde_json::from_str(&fs::read_to_string(&p2).unwrap()).unwrap();
        assert_eq!(back1, r1, "first run's results.json must survive intact");
        assert_eq!(back2, r2);

        // First (no collision) dir is the bare, human-sortable timestamp; the
        // second carries the numeric uniquifier suffix.
        assert_eq!(
            p1.parent().unwrap().file_name().unwrap(),
            std::ffi::OsStr::new(ts),
            "the first run keeps the bare, sortable timestamp dir"
        );
        let d2 = p2
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            d2.starts_with(ts) && d2 != ts,
            "the second run dir must be a suffixed variant of the timestamp: {d2}"
        );

        // A third same-second run still gets its own distinct directory.
        let mut r3 = r1.clone();
        r3.host = "host-c".to_string();
        let p3 = write_results(&r3, &tmp).expect("third write, same second");
        assert_ne!(p3.parent().unwrap(), p1.parent().unwrap());
        assert_ne!(p3.parent().unwrap(), p2.parent().unwrap());

        let _ = fs::remove_dir_all(&tmp);
    }
}
