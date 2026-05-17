//! Regression: diff the current run against the **committed baseline**,
//! host-pinned (harness HLD §9 + §2.9). The final harness stage.
//!
//! # What this is (and is not)
//!
//! A **pure comparator**: [`compare`] takes a baseline [`RunResults`] and the
//! current [`RunResults`] and decides, per URL, whether the run regressed —
//! and whether the result is **regression-gating** (same canonical host) or
//! merely **advisory** (different host — HLD §2.9 single-host repro). It is
//! pure given its two inputs, so the entire transition matrix + the host-pin
//! gate is unit-tested by synthesising `RunResults`, with no run / spawn / FS.
//!
//! There is **no baseline-management tooling** here (HLD §9 is explicit:
//! "copy `runs/<latest>/results.json` over `baseline/results.json` and commit
//! … No tooling, no migration — a file copy under version control is the
//! entire mechanism"). This module only ever **reads** the committed
//! `baseline/results.json`; the deliberate manual update ritual is documented
//! in `benchmark/README.md`.
//!
//! # THE Bug-E2 requirement at the regression layer (mirrors §5 doctrine)
//!
//! Regression is decided by the **(baseline_outcome → current_outcome)
//! transition**, encoded as the explicit exhaustively-tested [`RegressionKind`]
//! — **not** ad-hoc float math. A [`ScoreOutcome::NotScored`] is **never**
//! coerced to `0.0` to manufacture or hide a delta (the exact `metrics.rs`
//! `# HAZARD` / §5 laundering the harness exists to prevent). Concretely:
//!
//! | baseline → current             | result                                 |
//! |--------------------------------|----------------------------------------|
//! | `Scored → Scored`              | regression **iff** Coverage dropped by |
//! |                                | more than [`REGRESSION_DROP`] (abs)    |
//! |                                | **or** crate `word_count` shrank by    |
//! |                                | more than [`REGRESSION_DROP`] (rel);   |
//! |                                | else no — **gating**                   |
//! | `Scored → NotScored` —         | a trusted score was lost; the **crate**|
//! | crate-owned: `CrateNotImplemented` | genuinely got worse →              |
//! | / `CrateError`                 | [`RegressionKind::ScoreLost`] —        |
//! |                                | **GATING** (counts toward `should_fail`)|
//! | `Scored → NotScored` —         | the oracle/reference changed *under    |
//! | reference/oracle-owned:        | us*; the crate is **NOT** implicated → |
//! | `ReferenceUnavailable` /       | [`RegressionKind::ReferenceLost`] —    |
//! | `ReferenceEmpty`               | listed as signal but **NON-gating**    |
//! |                                | (excluded from `should_fail`; re-bless |
//! |                                | the baseline if the ref env changed)   |
//! | `NotScored → NotScored`        | no change (absent numbers never cmp'd) |
//! | `NotScored → Scored`           | improvement (not a regression)         |
//! | in baseline, **absent**        | **REGRESSION** / anomaly (lost a URL's |
//! | in current                     | coverage entirely) — **gating**        |
//! | **new** in current             | not a regression (noted)               |
//!
//! `NotScored → NotScored` deliberately does **not** compare any absent
//! number; `Scored → NotScored` is **always flagged/listed** (never silently
//! dropped) but the *owner class* of the not-scored reason decides whether it
//! **gates**: a **crate**-owned loss (`CrateNotImplemented`/`CrateError`) is a
//! real crate regression and gates; a **reference/oracle**-owned loss
//! (`ReferenceUnavailable`/`ReferenceEmpty`) means the comparison basis moved
//! under us and the crate is not implicated — it is **listed as signal but
//! NON-gating** (#2c — an oracle failure is never laundered into a crate
//! red-CI; re-bless the baseline if the reference environment legitimately
//! changed). [`NotScoredReason`] is matched **exhaustively** (no wildcard) so a
//! future reason must force a conscious crate-vs-reference classification.
//! The M1 floor (baseline all-`NotScored` vs current all-`NotScored`)
//! is therefore **zero regressions** by construction — the key floor invariant.
//!
//! # What this gate does NOT detect (#2a — re-bless after a gold-set change)
//!
//! The comparator diffs **Coverage / word_count magnitudes only**. It does
//! **not** — and at M1 deliberately *cannot* — detect a change in the
//! **reference basis** behind otherwise-equal numbers: if the reference for a
//! URL switches (Trafilatura → a newly-frozen gold entry, HLD §7) but the
//! resulting Coverage is numerically unchanged, the gate sees no regression
//! even though "0.92 vs Trafilatura" and "0.92 vs gold" are different claims.
//! Storing the reference kind in `results.json` is intentionally **out of
//! scope at M1** (no premature mechanism, HLD §3). The compensating control is
//! procedural and load-bearing: **after ANY gold-set change the committed
//! baseline MUST be re-blessed** via the HLD §2.7/§9 `cp` + commit ritual (a
//! deliberate manual act whose commit message states why the numbers moved).
//! See `benchmark/README.md`.
//!
//! # Host pinning (HLD §2.9 / the Stage-6 canonical contract)
//!
//! The baseline is valid **only on the host named in its metadata**. Hosts are
//! compared via [`score::canonical_host_of`] — the single host-identity
//! equality definition (`ANVIL` / `anvil` / `anvil.corp.local` are the same
//! host), reused, **never** reimplemented (the binding forward contract on
//! `score::HostDetectionFailed` / `report.rs`). If the canonical hosts match
//! the run is **gating**: a `REGRESSIONS` block is prepended to `report.md`
//! and the process exits non-zero on any regression. If they differ (or the
//! baseline's host is absent/unparseable — belt-and-suspenders for the
//! fail-closed Stage-6 contract) the result is **advisory only**: a
//! `BASELINE ADVISORY` block is prepended and the process does **not** exit
//! non-zero (HLD §2.9 — runs off the declared host are advisory).
//!
//! # No premature abstraction (HLD §3)
//!
//! One concrete comparator + one block renderer. No diff framework, no
//! pluggable policy — the M8-ring-road antipattern the brief warns against.

use std::fmt::Write as _;

use crate::score::{NotScoredReason, RunResults, ScoreOutcome, UrlRecord, canonical_host_of};

/// Regression threshold (HLD §9 / §10 — "more than a documented constant,
/// proposed 5%"). A documented **code constant** (HLD §10 — no env / no
/// flags), revisitable once real corpus evidence exists (evidence-driven, not
/// predicted).
///
/// Applied two ways, each the *interpretable* one for its quantity:
///
/// * **Coverage** drop is **absolute**: Coverage is already a `0.0..=1.0`
///   ratio (token-set Jaccard, `metrics.rs`), so a flat `0.05` band is the
///   natural, directly-interpretable "dropped by 5 percentage-points" cut. A
///   *relative* test on a small ratio would amplify noise near zero (a
///   `0.02 → 0.018` wobble is not a regression).
/// * Crate **`word_count`** shrink is **relative**: word count is an unbounded
///   count with no natural scale, so "shrank by more than 5%" only means
///   anything relative to the baseline count (`current < baseline * (1 -
///   REGRESSION_DROP)`).
///
/// Both boundaries are "more than", evaluated with an epsilon tolerance
/// ([`EPS`]) so a drop that is *mathematically* **exactly** `REGRESSION_DROP`
/// (Coverage) or a shrink to **exactly** `baseline * (1 - REGRESSION_DROP)`
/// (word count) is treated as **within** threshold — **not** a regression
/// (HLD §9 "more than"). The tolerance is required because f64 subtraction of
/// the ratio at the exact boundary carries representation error far larger
/// than any meaningful Coverage delta (e.g. `0.90 - 0.85` is not bit-exactly
/// `0.05`); a bare `>` would spuriously flag the exact boundary. Same `1e-9`
/// epsilon rationale as `metrics.rs` / `score.rs` ratio comparisons.
pub const REGRESSION_DROP: f64 = 0.05;

/// Epsilon for the boundary comparisons (same rationale + value as
/// `metrics.rs` / `score.rs`: these are ratios of small token counts, the
/// representation error is `≪ 1e-9`, and a delta below this is f64 noise, not
/// a real regression). A drop is "more than" the threshold only if it exceeds
/// it by **more than** `EPS`, so the exact boundary reads as within-threshold.
const EPS: f64 = 1e-9;

/// The per-URL regression decision — the explicit transition outcome (the
/// anti-Bug-E2 gate at the regression layer). Every variant is a distinct,
/// exhaustively-tested transition cell; regression-ness is **never** inferred
/// from ad-hoc float math, and a `NotScored` is **never** coerced to a number.
#[derive(Debug, Clone, PartialEq)]
pub enum RegressionKind {
    /// `Scored → Scored`, Coverage dropped by more than [`REGRESSION_DROP`]
    /// (absolute). Carries both values so the report can show the delta.
    CoverageDrop { baseline: f64, current: f64 },
    /// `Scored → Scored`, crate `word_count` shrank by more than
    /// [`REGRESSION_DROP`] (relative to the baseline count).
    WordCountShrank { baseline: usize, current: usize },
    /// `Scored → NotScored` for a **crate-owned** reason
    /// ([`NotScoredReason::CrateNotImplemented`] / [`NotScoredReason::CrateError`]):
    /// the crate itself genuinely got worse and a previously-trusted score was
    /// lost. **Gating** — counts toward [`Comparison::should_fail`]; never
    /// silently dropped (HLD §5/§9). Carries the current reason token so the
    /// report explains *why* it was lost.
    ScoreLost { reason: String },
    /// `Scored → NotScored` for a **reference/oracle-owned** reason
    /// ([`NotScoredReason::ReferenceUnavailable`] / [`NotScoredReason::ReferenceEmpty`]):
    /// the oracle/reference changed *under us*, so there is nothing trustworthy
    /// to compare — **the crate is NOT implicated**. This is still listed as an
    /// offender row (it is real signal: the comparison basis moved), but it is
    /// **NON-gating** — explicitly **excluded** from [`Comparison::should_fail`]
    /// so an oracle/reference failure is never laundered into a crate
    /// "regression" that red-CIs the declared host. The fix is to re-bless the
    /// baseline if the reference environment legitimately changed (HLD §2.7/§9).
    /// Carries the current reason token so the report explains the loss.
    ReferenceLost { reason: String },
    /// The URL was present in the baseline but is **absent** from the current
    /// run (manifest order, a removed/renamed URL) — lost coverage of a URL
    /// entirely. **Gating** — flagged as a regression/anomaly (HLD §9).
    UrlMissing,
}

impl RegressionKind {
    /// Whether this offender kind is **regression-gating** (counts toward
    /// [`Comparison::should_fail`] / red-CI on the declared host) or merely a
    /// **non-gating** listed signal.
    ///
    /// The split exists so an **oracle/reference** failure is never laundered
    /// into a **crate** regression (the #2c cut): only
    /// [`RegressionKind::ReferenceLost`] (the reference/oracle changed under
    /// us — the crate is not implicated) is non-gating. Every crate-owned
    /// transition ([`CoverageDrop`](RegressionKind::CoverageDrop) /
    /// [`WordCountShrank`](RegressionKind::WordCountShrank) /
    /// [`ScoreLost`](RegressionKind::ScoreLost)) and
    /// [`UrlMissing`](RegressionKind::UrlMissing) (a URL we lost coverage of
    /// entirely) **is** gating. Exhaustive (no wildcard) so a new kind forces a
    /// conscious gating decision here.
    fn is_gating(&self) -> bool {
        match self {
            RegressionKind::CoverageDrop { .. }
            | RegressionKind::WordCountShrank { .. }
            | RegressionKind::ScoreLost { .. }
            | RegressionKind::UrlMissing => true,
            RegressionKind::ReferenceLost { .. } => false,
        }
    }

    /// A one-line human reason for the `REGRESSIONS` block (HLD §9 — the
    /// offender list a human/CI reads first).
    fn describe(&self) -> String {
        match self {
            RegressionKind::CoverageDrop { baseline, current } => format!(
                "Coverage dropped {baseline:.4} → {current:.4} (> {REGRESSION_DROP} absolute)"
            ),
            RegressionKind::WordCountShrank { baseline, current } => format!(
                "crate word_count shrank {baseline} → {current} (> {}% relative)",
                REGRESSION_DROP * 100.0
            ),
            RegressionKind::ScoreLost { reason } => {
                format!("a trusted score was LOST (now not_scored: {reason})")
            }
            RegressionKind::ReferenceLost { reason } => {
                format!(
                    "reference/oracle LOST under us — NOT a crate regression \
                     (now not_scored: {reason}); re-bless the baseline if the \
                     reference environment changed (NON-gating)"
                )
            }
            RegressionKind::UrlMissing => {
                "URL present in baseline but ABSENT from the current run \
                 (lost coverage of a URL)"
                    .to_string()
            }
        }
    }
}

/// One flagged offender (URL + why) for the `REGRESSIONS` / advisory block.
#[derive(Debug, Clone, PartialEq)]
pub struct Regression {
    /// The offending URL (verbatim from the baseline record).
    pub url: String,
    /// Which transition cell fired.
    pub kind: RegressionKind,
}

/// Whether the regression result **gates** (fails CI / non-zero exit) or is
/// **advisory** only — the HLD §2.9 single-host-reproducibility decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Current run's canonical host **equals** the baseline's: the baseline is
    /// valid here, so any regression **gates** (block prepended + non-zero
    /// exit). `host` is the shared canonical host (for the block header).
    Gating,
    /// Current host **differs** from the baseline's (or the baseline host is
    /// absent/unparseable — the fail-closed never-matches case). Per HLD §2.9
    /// the result is **advisory only**: block prepended, but exit stays 0.
    Advisory,
}

/// The outcome of comparing the current run against the committed baseline.
/// Carries the offender list **and** the gating decision, so `main.rs` can
/// prepend the right block and set the exit code without re-deriving anything.
#[derive(Debug, Clone, PartialEq)]
pub struct Comparison {
    /// Gating (same canonical host) vs advisory (HLD §2.9).
    pub gate: Gate,
    /// Current run's canonical host (or the raw stamped value if it somehow
    /// fails to canonicalise — only for the human-readable block header; the
    /// gate decision already accounts for an unparseable host).
    pub current_host: String,
    /// Baseline run's canonical host, or `None` if the baseline's stamped
    /// `host` is absent/unparseable (the fail-closed never-matches case — the
    /// Stage-6 contract belt-and-suspenders; such a baseline can only ever be
    /// [`Gate::Advisory`]).
    pub baseline_host: Option<String>,
    /// Every flagged URL, in baseline manifest order. Empty ⇒ a clean run.
    pub regressions: Vec<Regression>,
    /// `true` iff **≥1** URL pair (same URL in both runs) was `Scored` on
    /// **both** the baseline and the current run — i.e. at least one *trusted
    /// number was actually compared* (#4b). When this is `false` *and* the run
    /// is gating with zero regressions, the gate is **vacuous**: every URL is
    /// `NotScored` on both sides (the Milestone-1 floor) so the comparison
    /// proved nothing — it must NOT read as a substantive pass (mirrors
    /// [`no_baseline_block`]'s honesty), even though it is correctly *not* a
    /// regression (exit 0).
    pub compared_trusted_pair: bool,
}

impl Comparison {
    /// `true` iff the process must exit **non-zero**: the run is
    /// regression-[`Gate::Gating`] (same canonical host — HLD §9/§2.9; advisory
    /// never fails CI) **and** at least one offender is a **gating** kind
    /// ([`RegressionKind::is_gating`]).
    ///
    /// A [`RegressionKind::ReferenceLost`] offender (the oracle/reference
    /// changed under us — #2c) is **excluded**: it is still listed in the block
    /// as signal, but on its own it must never red-CI the declared host (it is
    /// not a crate regression). So a gating run whose *only* offenders are
    /// `ReferenceLost` is `should_fail() == false` even though the offender list
    /// is non-empty.
    pub fn should_fail(&self) -> bool {
        self.gate == Gate::Gating && self.regressions.iter().any(|r| r.kind.is_gating())
    }

    /// `true` iff this is a **vacuous** gating-clean result (#4b): the run is
    /// regression-[`Gate::Gating`], has **zero** regressions, **and** not a
    /// single trusted number was compared ([`compared_trusted_pair`] is
    /// `false`) — every URL is `NotScored` on **both** sides (the M1 floor).
    /// Such a run is correctly *not* a regression (exit 0) but compared nothing
    /// trustworthy, so it must be reported as **NOT a substantive pass** — it
    /// must never read like a real "clean" (the Bug-E2 / `no_baseline_block`
    /// honesty principle).
    ///
    /// [`compared_trusted_pair`]: Comparison::compared_trusted_pair
    pub fn is_vacuous_clean(&self) -> bool {
        self.gate == Gate::Gating && self.regressions.is_empty() && !self.compared_trusted_pair
    }
}

/// Decide the per-URL regression for a `(baseline → current)` pair where the
/// URL exists in **both** runs — the explicit transition function (the
/// anti-Bug-E2 core). `None` ⇒ not a regression (within threshold, an
/// improvement, or `NotScored → NotScored`).
///
/// This deliberately switches on the **outcome variants**, never on coerced
/// numbers: a `NotScored` is matched as a variant, so an absent score can
/// never be turned into a `0.0` that manufactures or hides a delta (the §5
/// doctrine at the regression layer).
fn classify(baseline: &UrlRecord, current: &UrlRecord) -> Option<RegressionKind> {
    match (&baseline.score, &current.score) {
        // Scored → Scored: the ONLY cell that compares numbers. Coverage
        // (absolute) OR crate word_count (relative). Coverage is checked
        // first so its (more interpretable) message wins when both fired.
        (
            ScoreOutcome::Scored {
                coverage: base_cov, ..
            },
            ScoreOutcome::Scored {
                coverage: cur_cov, ..
            },
        ) => {
            // Absolute Coverage drop, "more than" with an EPS tolerance so the
            // EXACT boundary (drop == REGRESSION_DROP, within f64 noise) is
            // within threshold — HLD §9 "more than". A bare `>` would flag the
            // exact boundary because the subtraction is not bit-exact.
            if (base_cov - cur_cov) - REGRESSION_DROP > EPS {
                return Some(RegressionKind::CoverageDrop {
                    baseline: *base_cov,
                    current: *cur_cov,
                });
            }
            // Relative crate word_count shrink. Both counts are recomputed via
            // the single tokenizer (HLD §8) and are `Some` whenever the crate
            // was `Ok` — which it must have been for `Scored` — so a `Scored`
            // record always has a crate_wc. Guard defensively anyway: a
            // missing count is treated as "cannot prove a shrink" (no false
            // regression), never as 0 (which would manufacture a 100% shrink).
            match (current.word_counts.crate_wc, baseline.word_counts.crate_wc) {
                (Some(cur_wc), Some(base_wc)) => {
                    // current < baseline * (1 - DROP), with an EPS-scaled
                    // tolerance so EXACTLY baseline*(1-DROP) (within f64 noise)
                    // is within threshold — HLD §9 "more than". The tolerance
                    // is scaled by the baseline count because the multiply
                    // amplifies representation error with magnitude.
                    let floor = (base_wc as f64) * (1.0 - REGRESSION_DROP);
                    if floor - (cur_wc as f64) > EPS * (base_wc as f64).max(1.0) {
                        return Some(RegressionKind::WordCountShrank {
                            baseline: base_wc,
                            current: cur_wc,
                        });
                    }
                    None
                }
                _ => None,
            }
        }
        // Scored → NotScored: a previously-trusted score was LOST. WHICH owner
        // class lost it decides gating-ness (#2c — never launder an
        // oracle/reference failure into a crate "regression"). Match
        // NotScoredReason EXHAUSTIVELY (no wildcard): a future reason MUST force
        // a conscious crate-vs-reference classification here (the Stage-5
        // Bug-E2 compile-fence). Both arms still list the offender (real
        // signal) and carry the reason; only the KIND (and thus its
        // `is_gating`) differs.
        (ScoreOutcome::Scored { .. }, ScoreOutcome::NotScored { reason }) => Some(match reason {
            // Crate-owned: the crate itself genuinely got worse ⇒ a trusted
            // score was lost ⇒ gating ScoreLost (counts toward should_fail).
            NotScoredReason::CrateNotImplemented | NotScoredReason::CrateError => {
                RegressionKind::ScoreLost {
                    reason: format!("{reason:?}"),
                }
            }
            // Reference/oracle-owned: the comparison basis moved under us; the
            // crate is NOT implicated ⇒ non-gating ReferenceLost (listed as
            // signal, but EXCLUDED from should_fail so it never red-CIs the
            // declared host — re-bless the baseline if the reference env
            // legitimately changed).
            NotScoredReason::ReferenceUnavailable | NotScoredReason::ReferenceEmpty => {
                RegressionKind::ReferenceLost {
                    reason: format!("{reason:?}"),
                }
            }
        }),
        // NotScored → NotScored: NO change. Deliberately compares NOTHING —
        // absent numbers are never diffed (the §5 anti-laundering cut).
        // NotScored → Scored: an improvement, not a regression.
        (ScoreOutcome::NotScored { .. }, _) => None,
    }
}

/// Compare the current run against the committed `baseline` (HLD §9). **Pure**
/// — the entire transition matrix + host-pin gate is exercised by synthesising
/// `RunResults`.
///
/// Records are keyed by **URL + canonical host**: a baseline record is only
/// ever compared to a current record with the **same URL** *and* the **same
/// canonical host**. (Within one `RunResults` the host is uniform — it is the
/// run header — but keying on both is the defensive contract: a baseline can
/// only be regression-gating on its own host.)
///
/// `utc_timestamp` is **completely ignored** — it always differs run-to-run
/// and is never a regression signal (the explicit HLD §9 requirement).
///
/// The gate:
/// * `canonical_host_of(current.host) == canonical_host_of(baseline.host)`
///   (both `Some` and equal) ⇒ [`Gate::Gating`].
/// * otherwise — different hosts, **or** the baseline host absent/unparseable
///   (fail-closed never-matches, the Stage-6 contract) ⇒ [`Gate::Advisory`]
///   (HLD §2.9 — advisory off the declared host, never gating).
pub fn compare(baseline: &RunResults, current: &RunResults) -> Comparison {
    // Host pinning (HLD §2.9). Reuse the SINGLE canonicalisation (score.rs) —
    // never reimplement it (the binding Stage-6 forward contract). A baseline
    // host that does not canonicalise (absent/blank) can never match ⇒ the
    // run is advisory, belt-and-suspenders for the fail-closed contract.
    let cur_canon = canonical_host_of(&current.host);
    let base_canon = canonical_host_of(&baseline.host);
    let gate = match (&cur_canon, &base_canon) {
        (Some(c), Some(b)) if c == b => Gate::Gating,
        _ => Gate::Advisory,
    };

    let mut regressions = Vec::new();
    // #4b: did we actually compare ≥1 trusted number? `true` once a URL is
    // `Scored` on BOTH sides (the only cell that diffs real numbers). If this
    // stays `false` on an otherwise-clean gating run, every URL was `NotScored`
    // on both sides (the M1 floor) ⇒ the gate is VACUOUS, not a real pass.
    let mut compared_trusted_pair = false;

    // Walk baseline records in their (manifest) order so the offender list is
    // deterministic and stable run-to-run (regression-diff friendly). For each
    // baseline URL find the current record with the SAME url; absent ⇒ the URL
    // was lost (a regression/anomaly). A URL NEW in current is intentionally
    // NOT iterated here (it cannot be a regression — there is no baseline to
    // drop from); the report notes new URLs elsewhere.
    for base_rec in &baseline.urls {
        match current.urls.iter().find(|c| c.url == base_rec.url) {
            Some(cur_rec) => {
                // A trusted number is compared iff BOTH sides are `Scored`
                // (matched as variants — a `NotScored` is never coerced, the §5
                // cut). This is exactly the cell `classify` diffs numbers in.
                if matches!(
                    (&base_rec.score, &cur_rec.score),
                    (ScoreOutcome::Scored { .. }, ScoreOutcome::Scored { .. })
                ) {
                    compared_trusted_pair = true;
                }
                if let Some(kind) = classify(base_rec, cur_rec) {
                    regressions.push(Regression {
                        url: base_rec.url.clone(),
                        kind,
                    });
                }
            }
            None => {
                regressions.push(Regression {
                    url: base_rec.url.clone(),
                    kind: RegressionKind::UrlMissing,
                });
            }
        }
    }

    Comparison {
        gate,
        current_host: cur_canon.unwrap_or_else(|| current.host.clone()),
        baseline_host: base_canon,
        regressions,
        compared_trusted_pair,
    }
}

/// Render the `REGRESSIONS` / `BASELINE ADVISORY` block that is **prepended**
/// to the top of `report.md` (HLD §9 — a human / CI sees the gate verdict
/// *first*, before the summary).
///
/// * [`Gate::Gating`] + ≥1 **gating** offender ⇒ a `# REGRESSIONS` block
///   listing every offender (this run **fails**: non-zero exit, see
///   [`Comparison::should_fail`]).
/// * [`Gate::Gating`] + offenders that are **all non-gating**
///   ([`RegressionKind::ReferenceLost`]) ⇒ the offenders are still listed
///   (signal) but the block states the run does **not** fail (the oracle /
///   reference moved under us — #2c — not a crate regression; re-bless the
///   baseline). Exit stays 0.
/// * [`Gate::Gating`] + none + **a trusted pair was compared** ⇒ an explicit,
///   honest *clean* line (never silence — silence reads as "not checked").
/// * [`Gate::Gating`] + none + **no trusted pair compared** ⇒ an explicit
///   **VACUOUS** line (#4b): every URL is `not_scored` on both sides (the M1
///   floor) so the gate compared no trusted numbers — it is **not** a
///   substantive pass (mirrors [`no_baseline_block`]'s honesty). Still exit 0.
/// * [`Gate::Advisory`] ⇒ a `# BASELINE ADVISORY (ran on X, baseline from Y)`
///   block; offenders are still listed (useful signal) but it is **not**
///   regression-gating (HLD §2.9 — exit stays 0).
///
/// A trailing `\n` separates the block from the existing report header so the
/// prepend is a clean markdown boundary.
pub fn render_block(cmp: &Comparison) -> String {
    let mut b = String::new();
    match cmp.gate {
        Gate::Gating => {
            if cmp.regressions.is_empty() {
                if cmp.compared_trusted_pair {
                    // Honest substantive clean: ≥1 trusted number compared.
                    let _ = writeln!(
                        b,
                        "# REGRESSIONS\n\n_None — regression-gating on host \
                         `{}` (matches the committed baseline); this run is \
                         clean (≥1 trusted score compared)._",
                        cmp.current_host
                    );
                } else {
                    // #4b: VACUOUS — every URL is not_scored on BOTH sides (the
                    // M1 floor). Correctly not a regression (exit 0) but it
                    // compared no trusted numbers, so it must NOT read as a
                    // real pass (mirrors no_baseline_block's honesty).
                    let _ = writeln!(
                        b,
                        "# REGRESSIONS\n\n_No regressions, but **VACUOUS** — \
                         every URL is `not_scored` on **both** the committed \
                         baseline and this run (the Milestone-1 floor) on host \
                         `{}`. This gate compared **no trusted numbers** and is \
                         **NOT a substantive pass** (it is correctly not a \
                         regression, so CI exits 0, but nothing trustworthy was \
                         verified)._",
                        cmp.current_host
                    );
                }
            } else if cmp.should_fail() {
                // ≥1 gating offender ⇒ this run FAILS (non-zero exit).
                let _ = writeln!(
                    b,
                    "# REGRESSIONS\n\n**This run is regression-gating** on host \
                     `{}` (matches the committed baseline) and **FAILS**: \
                     {} offending URL(s) — CI exits non-zero.\n",
                    cmp.current_host,
                    cmp.regressions.len()
                );
                for r in &cmp.regressions {
                    let _ = writeln!(b, "- `{}` — {}", r.url, r.kind.describe());
                }
            } else {
                // Offenders present but ALL non-gating (#2c —
                // `ReferenceLost`): the oracle/reference moved under us, the
                // crate is not implicated. List them as signal but state
                // clearly the run does NOT fail (exit 0; re-bless the baseline
                // if the reference environment legitimately changed).
                let _ = writeln!(
                    b,
                    "# REGRESSIONS\n\nRegression-gating on host `{}` (matches \
                     the committed baseline) but **does NOT fail**: the {} \
                     listed offender(s) are all **reference/oracle losses, NOT \
                     crate regressions** (#2c) — the comparison basis changed \
                     under us, so this is NOT laundered into a crate red-CI. \
                     CI exits 0; re-bless the committed baseline if the \
                     reference environment legitimately changed (see \
                     benchmark/README.md).\n",
                    cmp.current_host,
                    cmp.regressions.len()
                );
                for r in &cmp.regressions {
                    let _ = writeln!(b, "- `{}` — {}", r.url, r.kind.describe());
                }
            }
        }
        Gate::Advisory => {
            let base = cmp
                .baseline_host
                .as_deref()
                .unwrap_or("<absent/unparseable>");
            let _ = writeln!(
                b,
                "# BASELINE ADVISORY (ran on `{}`, baseline from `{}`) — \
                 not regression-gating\n\n_The committed baseline is valid only \
                 on its declared host (HLD §2.9). This run is on a different \
                 host, so the comparison below is **advisory only** and does \
                 **not** fail CI._",
                cmp.current_host, base
            );
            if cmp.regressions.is_empty() {
                let _ = writeln!(b, "\n_No differences flagged (advisory)._");
            } else {
                let _ = writeln!(
                    b,
                    "\n{} URL(s) would be flagged if this were the declared \
                     host (advisory):\n",
                    cmp.regressions.len()
                );
                for r in &cmp.regressions {
                    let _ = writeln!(b, "- `{}` — {}", r.url, r.kind.describe());
                }
            }
        }
    }
    // Blank line between the block and the existing report `# ...` header.
    b.push('\n');
    b
}

/// The block printed when **no baseline is committed** (e.g. the first ever
/// run) — prepended to `report.md` in place of the regression block.
///
/// This is an explicit, honest line: **not** silence and **not** a false "no
/// regressions" (the Bug-E2 lesson — absence of a check must never read as a
/// passing check). The run is a baseline *candidate*; the manual ritual to
/// promote it is in `benchmark/README.md`.
pub fn no_baseline_block() -> String {
    "# REGRESSIONS\n\n_No baseline committed — regression check skipped (this \
     run is a baseline candidate)._\n\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::{
        NotScoredReason, RunResults, ScoreOutcome, StatusCounts, UrlRecord, WordCounts,
    };

    // ---- Builders: synthesize RunResults without spawning anything ---------

    fn rec_scored(url: &str, coverage: f64, crate_wc: usize) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            shape_class: "news".to_string(),
            crate_status: "ok".to_string(),
            trafilatura_status: "ok".to_string(),
            readability_status: "ok".to_string(),
            status_detail: Default::default(),
            word_counts: WordCounts {
                crate_wc: Some(crate_wc),
                trafilatura_wc: Some(crate_wc),
                readability_wc: Some(crate_wc),
            },
            score: ScoreOutcome::Scored {
                coverage,
                precision: coverage,
                edit_sim: coverage,
            },
            edit_sim: Some(coverage),
            guardrail_flag: false,
            agreement: None,
        }
    }

    fn rec_not_scored(url: &str, reason: NotScoredReason) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            shape_class: "news".to_string(),
            crate_status: "not_implemented".to_string(),
            trafilatura_status: "oracle_error".to_string(),
            readability_status: "oracle_error".to_string(),
            status_detail: Default::default(),
            word_counts: WordCounts {
                crate_wc: None,
                trafilatura_wc: None,
                readability_wc: None,
            },
            score: ScoreOutcome::NotScored { reason },
            edit_sim: None,
            guardrail_flag: false,
            agreement: None,
        }
    }

    /// A `RunResults` on `host` at `ts`, with the given records. `ts` is varied
    /// in tests to prove it is ignored.
    fn run(host: &str, ts: &str, urls: Vec<UrlRecord>) -> RunResults {
        RunResults {
            host: host.to_string(),
            utc_timestamp: ts.to_string(),
            corpus_size: urls.len(),
            status_counts: StatusCounts::default(),
            urls,
        }
    }

    // ---- THE M1-floor self-comparison (the key floor test) -----------------

    #[test]
    fn m1_floor_all_not_scored_vs_all_not_scored_is_zero_regressions_exit_zero() {
        // THE floor invariant. Baseline is the documented M1 floor (every URL
        // NotScored) and the current run is the SAME floor. NotScored →
        // NotScored compares NOTHING (absent numbers never diffed), so this is
        // ZERO regressions and the process exits 0 — the floor self-compares
        // clean, nothing laundered into a fake delta or a fake pass.
        let urls_b = vec![
            rec_not_scored("https://a.test/1", NotScoredReason::CrateNotImplemented),
            rec_not_scored("https://a.test/2", NotScoredReason::CrateNotImplemented),
        ];
        let urls_c = vec![
            rec_not_scored("https://a.test/1", NotScoredReason::CrateNotImplemented),
            rec_not_scored("https://a.test/2", NotScoredReason::CrateNotImplemented),
        ];
        let base = run("anvil", "2026-05-17T00-00-00Z", urls_b);
        let cur = run("anvil", "2026-05-18T09-30-00Z", urls_c); // different ts
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Gating, "same host ⇒ gating");
        assert!(
            cmp.regressions.is_empty(),
            "M1 floor self-compare MUST be zero regressions, got {:?}",
            cmp.regressions
        );
        assert!(
            !cmp.should_fail(),
            "M1 floor self-compare must exit 0 (nothing laundered)"
        );
    }

    // ---- Scored → Scored: Coverage threshold (boundary exactly 5%) ---------

    #[test]
    fn scored_coverage_drop_over_threshold_is_regression() {
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/x", 0.90, 100)],
        );
        // 0.90 → 0.80 = 0.10 absolute drop > 0.05 ⇒ regression.
        let cur = run(
            "anvil",
            "t2",
            vec![rec_scored("https://a.test/x", 0.80, 100)],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1);
        assert!(matches!(
            cmp.regressions[0].kind,
            RegressionKind::CoverageDrop { .. }
        ));
        assert!(cmp.should_fail(), "gated coverage drop must fail CI");
    }

    #[test]
    fn scored_coverage_drop_exactly_threshold_is_not_regression() {
        // Boundary: drop of EXACTLY REGRESSION_DROP is WITHIN threshold (HLD
        // §9 "more than"), strict `>`. 0.90 → 0.85 == 0.05 ⇒ NOT a regression.
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/x", 0.90, 100)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_scored("https://a.test/x", 0.90 - REGRESSION_DROP, 100)],
        );
        let cmp = compare(&base, &cur);
        assert!(
            cmp.regressions.is_empty(),
            "exactly 5% drop is within threshold, got {:?}",
            cmp.regressions
        );
    }

    #[test]
    fn scored_coverage_drop_within_threshold_is_not_regression() {
        // 0.90 → 0.87 = 0.03 < 0.05 ⇒ not a regression.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.90, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.87, 100)]);
        assert!(compare(&base, &cur).regressions.is_empty());
    }

    #[test]
    fn scored_coverage_improvement_is_not_regression() {
        // Coverage went UP — never a regression.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.50, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.95, 100)]);
        assert!(compare(&base, &cur).regressions.is_empty());
    }

    // ---- Scored → Scored: word_count shrink (>5% relative) -----------------

    #[test]
    fn scored_word_count_shrink_over_threshold_is_regression() {
        // Coverage unchanged; crate wc 1000 → 900 = 10% shrink > 5% ⇒ regression.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.80, 1000)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.80, 900)]);
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1);
        assert!(matches!(
            cmp.regressions[0].kind,
            RegressionKind::WordCountShrank {
                baseline: 1000,
                current: 900
            }
        ));
    }

    #[test]
    fn scored_word_count_shrink_exactly_threshold_is_not_regression() {
        // Boundary: current == baseline*(1-DROP) is WITHIN threshold (strict
        // `<`). 1000 * 0.95 = 950 exactly ⇒ NOT a regression.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.80, 1000)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.80, 950)]);
        assert!(
            compare(&base, &cur).regressions.is_empty(),
            "exactly 5% wc shrink is within threshold"
        );
    }

    #[test]
    fn scored_word_count_growth_is_not_regression() {
        // Crate extracted MORE — not a regression.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.80, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.80, 5000)]);
        assert!(compare(&base, &cur).regressions.is_empty());
    }

    // ---- #2c: Scored → NotScored split by reason owner --------------------
    //
    // CRATE-owned (CrateNotImplemented / CrateError): the crate genuinely got
    // worse ⇒ gating `ScoreLost`, counts toward should_fail (red-CI on the
    // declared host). REFERENCE/ORACLE-owned (ReferenceUnavailable /
    // ReferenceEmpty): the comparison basis moved under us, the crate is NOT
    // implicated ⇒ non-gating `ReferenceLost` — still LISTED as signal but
    // EXCLUDED from should_fail (never laundered into a crate regression).

    #[test]
    fn scored_to_not_scored_crate_error_is_gating_score_lost() {
        // CrateError is crate-owned: a previously-trusted score was genuinely
        // lost ⇒ gating ScoreLost, fails CI on the declared host. The reason
        // is carried so the report explains the loss.
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/y", 0.95, 200)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_not_scored(
                "https://a.test/y",
                NotScoredReason::CrateError,
            )],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1);
        match &cmp.regressions[0].kind {
            RegressionKind::ScoreLost { reason } => assert!(
                reason.contains("CrateError"),
                "the lost-score reason must be carried, got {reason:?}"
            ),
            other => panic!("CrateError must be gating ScoreLost, got {other:?}"),
        }
        assert!(
            cmp.should_fail(),
            "a crate-owned lost score on the declared host MUST fail CI (non-zero)"
        );
    }

    #[test]
    fn scored_to_not_scored_crate_not_implemented_is_gating_score_lost() {
        // CrateNotImplemented is also crate-owned (the crate regressed back to
        // the M1 floor for this URL) ⇒ gating ScoreLost, fails CI.
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/y", 0.80, 120)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_not_scored(
                "https://a.test/y",
                NotScoredReason::CrateNotImplemented,
            )],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1);
        assert!(
            matches!(cmp.regressions[0].kind, RegressionKind::ScoreLost { .. }),
            "CrateNotImplemented must be gating ScoreLost, got {:?}",
            cmp.regressions[0].kind
        );
        assert!(
            cmp.should_fail(),
            "a crate-owned lost score on the declared host MUST be non-zero"
        );
    }

    #[test]
    fn scored_to_not_scored_reference_unavailable_is_listed_but_non_gating() {
        // ReferenceUnavailable is reference/oracle-owned: the oracle/reference
        // changed under us, the CRATE is not implicated. It MUST still be
        // LISTED as an offender (real signal) but should_fail() MUST be false
        // — exit 0 on the declared host even though it is an offender row.
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/y", 0.95, 200)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_not_scored(
                "https://a.test/y",
                NotScoredReason::ReferenceUnavailable,
            )],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Gating, "same host ⇒ gating");
        assert_eq!(
            cmp.regressions.len(),
            1,
            "the loss MUST still be listed as signal (never silently dropped)"
        );
        match &cmp.regressions[0].kind {
            RegressionKind::ReferenceLost { reason } => assert!(
                reason.contains("ReferenceUnavailable"),
                "the reference-loss reason must be carried, got {reason:?}"
            ),
            other => panic!("ReferenceUnavailable must be non-gating ReferenceLost, got {other:?}"),
        }
        assert!(
            !cmp.should_fail(),
            "a reference/oracle loss MUST NOT red-CI the crate (#2c): should_fail==false"
        );
        // And the rendered block lists it but says it does NOT fail.
        let block = render_block(&cmp);
        assert!(
            block.contains("https://a.test/y"),
            "offender listed as signal"
        );
        assert!(
            block.contains("does NOT fail") && block.contains("reference/oracle"),
            "block must say it does NOT fail and name reference/oracle: {block}"
        );
        assert!(
            !block.contains("FAILS"),
            "a non-gating-only block must NOT say FAILS: {block}"
        );
    }

    #[test]
    fn scored_to_not_scored_reference_empty_is_listed_but_non_gating() {
        // ReferenceEmpty is also reference/oracle-owned (the resolved reference
        // tokenised to ∅ under us) ⇒ listed but non-gating, exit 0.
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/y", 0.70, 90)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_not_scored(
                "https://a.test/y",
                NotScoredReason::ReferenceEmpty,
            )],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1, "still listed as signal");
        assert!(
            matches!(
                cmp.regressions[0].kind,
                RegressionKind::ReferenceLost { .. }
            ),
            "ReferenceEmpty must be non-gating ReferenceLost, got {:?}",
            cmp.regressions[0].kind
        );
        assert!(
            !cmp.should_fail(),
            "a reference/oracle loss never red-CIs the crate (#2c)"
        );
    }

    #[test]
    fn mixed_crate_and_reference_loss_gates_on_the_crate_one_only() {
        // A crate-owned loss AND a reference-owned loss in the same run: the
        // run gates (the crate one is gating) but BOTH are listed. This proves
        // the reference loss is not silently dropped just because another
        // offender already gates.
        let base = run(
            "anvil",
            "t1",
            vec![
                rec_scored("https://a.test/crate", 0.90, 100),
                rec_scored("https://a.test/ref", 0.90, 100),
            ],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![
                rec_not_scored("https://a.test/crate", NotScoredReason::CrateError),
                rec_not_scored("https://a.test/ref", NotScoredReason::ReferenceUnavailable),
            ],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 2, "BOTH listed (order preserved)");
        assert!(matches!(
            cmp.regressions[0].kind,
            RegressionKind::ScoreLost { .. }
        ));
        assert!(matches!(
            cmp.regressions[1].kind,
            RegressionKind::ReferenceLost { .. }
        ));
        assert!(
            cmp.should_fail(),
            "the crate-owned loss gates the run (non-zero) even though the \
             reference loss alone would not"
        );
    }

    // ---- NotScored → Scored: improvement, NOT a regression -----------------

    #[test]
    fn not_scored_to_scored_is_not_regression() {
        // The crate started working for this URL — an improvement, never a
        // regression. Critically: the absent baseline score is NOT coerced to
        // 0.0 and diffed (that would be the §5 laundering inverted).
        let base = run(
            "anvil",
            "t1",
            vec![rec_not_scored("u", NotScoredReason::CrateNotImplemented)],
        );
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.10, 5)]);
        assert!(
            compare(&base, &cur).regressions.is_empty(),
            "NotScored→Scored is an improvement, never a regression"
        );
    }

    // ---- URL removed-in-current → flagged; new-in-current → not ------------

    #[test]
    fn url_in_baseline_absent_in_current_is_flagged() {
        let base = run(
            "anvil",
            "t1",
            vec![
                rec_scored("https://a.test/keep", 0.80, 100),
                rec_scored("https://a.test/gone", 0.80, 100),
            ],
        );
        // "gone" dropped from the current run entirely.
        let cur = run(
            "anvil",
            "t2",
            vec![rec_scored("https://a.test/keep", 0.80, 100)],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 1);
        assert_eq!(cmp.regressions[0].url, "https://a.test/gone");
        assert_eq!(cmp.regressions[0].kind, RegressionKind::UrlMissing);
    }

    #[test]
    fn url_new_in_current_is_not_a_regression() {
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/old", 0.80, 100)],
        );
        // A brand-new URL appears; the old one is unchanged.
        let cur = run(
            "anvil",
            "t2",
            vec![
                rec_scored("https://a.test/old", 0.80, 100),
                rec_scored("https://a.test/new", 0.10, 5),
            ],
        );
        assert!(
            compare(&base, &cur).regressions.is_empty(),
            "a NEW url cannot be a regression (no baseline to drop from)"
        );
    }

    // ---- Timestamp is ignored ----------------------------------------------

    #[test]
    fn timestamp_differs_everything_else_identical_is_zero_regressions() {
        // utc_timestamp ALWAYS differs run-to-run and must NEVER be a
        // regression signal (explicit HLD §9). Identical records, different
        // timestamps ⇒ zero regressions.
        let urls_b = vec![rec_scored("u1", 0.80, 100), rec_scored("u2", 0.40, 50)];
        let urls_c = vec![rec_scored("u1", 0.80, 100), rec_scored("u2", 0.40, 50)];
        let base = run("anvil", "2026-01-01T00-00-00Z", urls_b);
        let cur = run("anvil", "2026-12-31T23-59-59Z", urls_c);
        assert!(
            compare(&base, &cur).regressions.is_empty(),
            "timestamp must be ignored — identical records ⇒ no regression"
        );
    }

    // ---- Host pinning: match ⇒ gating + non-zero exit on a regression ------

    #[test]
    fn host_match_is_gating_and_fails_on_regression() {
        // Canonical host match (ANVIL.corp.local vs anvil → both "anvil").
        // A regression ⇒ gating + should_fail (non-zero exit).
        let base = run("ANVIL.corp.local", "t1", vec![rec_scored("u", 0.95, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.50, 100)]);
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Gating, "ANVIL.corp.local ≡ anvil ⇒ gating");
        assert!(!cmp.regressions.is_empty());
        assert!(cmp.should_fail(), "gated regression ⇒ non-zero exit");
    }

    // ---- Host pinning: mismatch ⇒ advisory + exit 0 even WITH deltas -------

    #[test]
    fn host_mismatch_is_advisory_and_does_not_fail_even_with_regressions() {
        // THE host-mismatch-advisory test. Different hosts: even a blatant
        // regression is ADVISORY ONLY (HLD §2.9 — runs off the declared host
        // never gate). Offenders are still listed (signal) but exit stays 0.
        let base = run("anvil", "t1", vec![rec_scored("u", 0.95, 100)]);
        let cur = run("borg", "t2", vec![rec_scored("u", 0.01, 1)]);
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Advisory, "different hosts ⇒ advisory");
        assert!(
            !cmp.regressions.is_empty(),
            "the delta is still computed/listed as advisory signal"
        );
        assert!(
            !cmp.should_fail(),
            "advisory MUST NOT fail CI even with a blatant regression (HLD §2.9)"
        );
    }

    #[test]
    fn baseline_host_unparseable_is_advisory_never_gates() {
        // Belt-and-suspenders for the fail-closed Stage-6 contract: a baseline
        // whose stamped host does not canonicalise (blank/garbage) can NEVER
        // match any host ⇒ advisory, never gating, even with a regression.
        let base = run("   ", "t1", vec![rec_scored("u", 0.95, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.01, 1)]);
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Advisory);
        assert_eq!(cmp.baseline_host, None, "unparseable baseline host ⇒ None");
        assert!(!cmp.should_fail(), "unparseable baseline host never gates");
    }

    // ---- The block is prepended at the TOP, and is honest ------------------

    #[test]
    fn regressions_block_starts_with_the_regressions_heading() {
        let base = run(
            "anvil",
            "t1",
            vec![rec_scored("https://a.test/z", 0.95, 100)],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_scored("https://a.test/z", 0.10, 100)],
        );
        let cmp = compare(&base, &cur);
        let block = render_block(&cmp);
        assert!(
            block.starts_with("# REGRESSIONS"),
            "the gating block must start with the REGRESSIONS heading so a \
             human/CI sees the verdict FIRST; got: {:?}",
            &block[..block.len().min(40)]
        );
        assert!(block.contains("https://a.test/z"), "offender URL listed");
        assert!(
            block.contains("FAILS"),
            "a gated regression block says FAILS"
        );
    }

    #[test]
    fn gating_clean_block_is_explicit_not_silent() {
        // Same host, no regressions, a REAL Scored→Scored within-threshold
        // pair (≥1 trusted number compared): an EXPLICIT, substantive clean
        // line — never silence (Bug-E2) and NOT vacuous (#4b).
        let base = run("anvil", "t1", vec![rec_scored("u", 0.80, 100)]);
        let cur = run("anvil", "t2", vec![rec_scored("u", 0.80, 100)]);
        let cmp = compare(&base, &cur);
        assert!(
            cmp.compared_trusted_pair,
            "a Scored→Scored pair means a trusted number WAS compared"
        );
        assert!(!cmp.is_vacuous_clean(), "≥1 trusted pair ⇒ NOT vacuous");
        let block = render_block(&cmp);
        assert!(block.starts_with("# REGRESSIONS"));
        assert!(
            block.contains("clean"),
            "a substantive clean gated run must say so explicitly: {block}"
        );
        assert!(
            !block.contains("VACUOUS"),
            "a run that compared a trusted pair must NOT be VACUOUS: {block}"
        );
    }

    // ---- #4b: a VACUOUS gate must NOT present as a substantive pass --------

    #[test]
    fn vacuous_gate_all_not_scored_both_sides_says_not_a_substantive_pass() {
        // THE #4b case: same host, zero regressions, but EVERY URL is
        // NotScored on BOTH the baseline and this run (the M1 floor). This is
        // correctly NOT a regression (exit 0) — but it compared NO trusted
        // numbers, so the block MUST say it is VACUOUS / not a substantive
        // pass (mirrors no_baseline_block's honesty), never a real "clean".
        let base = run(
            "anvil",
            "t1",
            vec![
                rec_not_scored("https://a.test/1", NotScoredReason::CrateNotImplemented),
                rec_not_scored("https://a.test/2", NotScoredReason::CrateNotImplemented),
            ],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![
                rec_not_scored("https://a.test/1", NotScoredReason::CrateNotImplemented),
                rec_not_scored("https://a.test/2", NotScoredReason::CrateNotImplemented),
            ],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.gate, Gate::Gating, "same host ⇒ gating");
        assert!(
            cmp.regressions.is_empty(),
            "M1 floor self-compare ⇒ zero regressions"
        );
        assert!(
            !cmp.compared_trusted_pair,
            "no Scored→Scored pair ⇒ no trusted number compared"
        );
        assert!(
            cmp.is_vacuous_clean(),
            "all-NotScored-both-sides gating ⇒ vacuous"
        );
        assert!(
            !cmp.should_fail(),
            "vacuous is NOT a regression — still exit 0"
        );
        let block = render_block(&cmp);
        assert!(block.starts_with("# REGRESSIONS"));
        assert!(
            block.contains("VACUOUS") && block.contains("NOT a substantive pass"),
            "the vacuous block must say VACUOUS / not a substantive pass: {block}"
        );
        assert!(
            !block.contains("this run is clean"),
            "a vacuous gate must NOT read as a real clean pass: {block}"
        );
    }

    #[test]
    fn non_vacuous_if_at_least_one_scored_pair_even_among_not_scored() {
        // A single Scored→Scored within-threshold pair among otherwise
        // all-NotScored URLs is enough to make the gate SUBSTANTIVE (not
        // vacuous): ≥1 trusted number was actually compared.
        let base = run(
            "anvil",
            "t1",
            vec![
                rec_not_scored("https://a.test/floor", NotScoredReason::CrateNotImplemented),
                rec_scored("https://a.test/real", 0.80, 100),
            ],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![
                rec_not_scored("https://a.test/floor", NotScoredReason::CrateNotImplemented),
                rec_scored("https://a.test/real", 0.80, 100),
            ],
        );
        let cmp = compare(&base, &cur);
        assert!(
            cmp.regressions.is_empty(),
            "within-threshold ⇒ no regression"
        );
        assert!(
            cmp.compared_trusted_pair,
            "the one Scored→Scored pair counts as a trusted comparison"
        );
        assert!(
            !cmp.is_vacuous_clean(),
            "≥1 trusted pair ⇒ substantive, NOT vacuous"
        );
        let block = render_block(&cmp);
        assert!(
            block.contains("clean") && !block.contains("VACUOUS"),
            "{block}"
        );
    }

    #[test]
    fn advisory_block_names_both_hosts_and_says_not_gating() {
        let base = run("anvil", "t1", vec![rec_scored("u", 0.95, 100)]);
        let cur = run("borg", "t2", vec![rec_scored("u", 0.01, 1)]);
        let block = render_block(&compare(&base, &cur));
        assert!(
            block.starts_with("# BASELINE ADVISORY"),
            "advisory block heading first: {block}"
        );
        assert!(
            block.contains("borg") && block.contains("anvil"),
            "both hosts named"
        );
        assert!(
            block.contains("not regression-gating"),
            "advisory must state it does not gate: {block}"
        );
    }

    #[test]
    fn no_baseline_block_is_honest_not_a_false_no_regressions() {
        // No baseline ⇒ an explicit "skipped (baseline candidate)" line, NOT
        // silence and NOT a false "no regressions" (the Bug-E2 lesson —
        // absence of a check must never read as a passing check).
        let b = no_baseline_block();
        assert!(b.starts_with("# REGRESSIONS"));
        assert!(
            b.contains("No baseline committed") && b.contains("baseline candidate"),
            "must explicitly say the check was skipped: {b}"
        );
    }

    // ---- Mixed corpus: order preserved, multiple offenders -----------------

    #[test]
    fn multiple_offenders_listed_in_baseline_order() {
        let base = run(
            "anvil",
            "t1",
            vec![
                rec_scored("https://a.test/1", 0.90, 100), // will drop coverage
                rec_scored("https://a.test/2", 0.80, 1000), // will shrink wc
                rec_scored("https://a.test/3", 0.80, 100), // unchanged
                rec_scored("https://a.test/4", 0.90, 100), // will be lost
            ],
        );
        let cur = run(
            "anvil",
            "t2",
            vec![
                rec_scored("https://a.test/1", 0.50, 100),
                rec_scored("https://a.test/2", 0.80, 100),
                rec_scored("https://a.test/3", 0.80, 100),
                rec_not_scored("https://a.test/4", NotScoredReason::CrateError),
            ],
        );
        let cmp = compare(&base, &cur);
        assert_eq!(cmp.regressions.len(), 3, "1,2,4 regress; 3 does not");
        // Order is baseline manifest order (deterministic).
        assert_eq!(cmp.regressions[0].url, "https://a.test/1");
        assert!(matches!(
            cmp.regressions[0].kind,
            RegressionKind::CoverageDrop { .. }
        ));
        assert_eq!(cmp.regressions[1].url, "https://a.test/2");
        assert!(matches!(
            cmp.regressions[1].kind,
            RegressionKind::WordCountShrank { .. }
        ));
        assert_eq!(cmp.regressions[2].url, "https://a.test/4");
        assert!(matches!(
            cmp.regressions[2].kind,
            RegressionKind::ScoreLost { .. }
        ));
    }
}
