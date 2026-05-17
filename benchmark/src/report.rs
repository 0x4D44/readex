//! Report emission: `report.md`, generated **from** a [`score::RunResults`]
//! (harness HLD §9). The single source of truth is the same in-memory
//! `RunResults` that produced `results.json`; this module never reparses
//! `results.json` and never recomputes any metric — it consumes the already
//! status-gated [`score::ScoreOutcome`]s and shapes them into markdown.
//!
//! # THE critical requirement — Bug-E2 at the report/aggregation layer
//!
//! This module **mirrors the score-layer gate** (HLD §5 doctrine). Every
//! aggregate (mean Coverage, mean Precision, the per-`shape_class` means) is
//! computed **only** over URLs whose [`score::ScoreOutcome`] is
//! [`Scored`](score::ScoreOutcome::Scored). [`NotScored`](score::ScoreOutcome::NotScored)
//! URLs are **excluded** from the mean and are **never** coerced to `0.0`/`1.0`
//! — see [`mean_of_scored`] / [`MeanCell`]. Every mean is rendered **with its
//! sample N** (`mean Coverage: 0.83 (N=12 of 50 scored)`); if **zero** URLs
//! are `Scored` (the M1 floor) the mean renders as `n/a (0 of N scored)`,
//! **never** `0.00` / `100%` / a blank that reads as a value. A failed/empty
//! run is visibly a non-result, not a fake perfect/zero score. The per-status
//! counts are shown explicitly so the reader sees how many URLs were excluded
//! and why.
//!
//! # FORWARD CONTRACT — agreement-on-disagreement must carry its sample N
//!
//! (Now discharged.) The agreement-on-disagreement distribution
//! ([`score::Agreement`]) is rendered **with its sample size** (`N = k of m`)
//! and **flagged non-representative when N is below
//! [`AGREEMENT_MIN_SAMPLES`]**. It is **never** rendered as a bare "crate sides
//! with Trafilatura X%" without the accompanying `(N=k of m)`: that signal is
//! `Some` only on the subset of URLs where all three sides are valid *and* the
//! two oracles genuinely disagree, which is often a handful. A percentage over
//! a handful presented as a population statistic is exactly the laundered,
//! misleading number the harness doctrine (HLD §5; the Bug-E2 lesson) forbids.
//! At M1 N=0 → rendered explicitly as `no agreement samples (N=0)`, never a
//! percentage.
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
//!
//! # No premature abstraction (HLD §3 / §10)
//!
//! The report is plain string formatting — **no template engine**, no
//! reporting framework. Markdown is assembled with `writeln!` into a `String`.
//! [`render_report`] is **pure** given a `&RunResults` + the gold word-bands,
//! so the entire aggregation (and the Bug-E2 / O8 gates) is unit-testable by
//! synthesising inputs, with no run/spawn/filesystem.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::oracle::OracleKind;
use crate::score::{Agreement, GoldSet, RunResults, ScoreOutcome};

/// Minimum sample size for the agreement-on-disagreement distribution to be
/// presented without a non-representative caveat (HLD §8 / the O8 forward
/// contract). A documented **code constant** (HLD §10 — no env / no flags):
/// the agreement signal is `Some` only where all three sides are valid *and*
/// the two oracles genuinely disagree (often a handful), so any distribution
/// over fewer than this many samples is flagged non-representative and is
/// **never** presented as a bare population percentage. Revisitable once real
/// corpus evidence exists (evidence-driven, not predicted).
const AGREEMENT_MIN_SAMPLES: usize = 10;

/// Number of worst-scoring disagreement rows to surface (HLD §9 — the
/// candidate-bug queue; the full ranking is in `results.json`). A documented
/// code constant.
const TOP_DISAGREEMENTS: usize = 25;

/// Minimum fraction of the corpus that must be `Scored` for the headline
/// `mean Coverage` / `mean Precision` to stand **without** a
/// low-scored-fraction caveat (HLD §5 doctrine — the same non-representative
/// discipline the agreement distribution already obeys via
/// [`AGREEMENT_MIN_SAMPLES`], applied to the *most-read* numbers).
///
/// A documented **code constant** (HLD §10 — no env / no flags). The headline
/// means are computed only over `Scored` URLs and always carry `(N=k of m
/// scored)`; but a mean over a small *minority* of the corpus, while correctly
/// annotated, is still read at a glance as "the corpus mean" — exactly the
/// laundered-by-omission number the harness doctrine forbids. **0.5** (a
/// strict majority) is the threshold: at/above it the mean covers a majority
/// of the corpus and the `(N=k of m)` annotation suffices; below it the
/// headline is over a minority and is explicitly flagged
/// non-representative. (`AGREEMENT_MIN_SAMPLES` is an absolute count because
/// that signal's denominator is itself a sub-subset; this is a *fraction*
/// because the headline's denominator is the whole corpus.) The arithmetic
/// and the `n/a`-at-zero behaviour are unchanged — this is an **additional**
/// caveat for the low-fraction case only. Revisitable once real corpus
/// evidence exists (evidence-driven, not predicted).
const SCORED_FRACTION_MIN: f64 = 0.5;

/// A mean over the `Scored` subset, carrying its sample size so it can **never**
/// be rendered as a bare number (the Bug-E2 report-layer gate).
///
/// `value` is `None` iff `n == 0` (zero `Scored` URLs in scope) — the M1
/// floor. It is **never** a coerced `0.0`/`1.0` for an empty sample: a
/// non-result is structurally distinct from a real mean, exactly as
/// [`score::ScoreOutcome`] keeps `NotScored` distinct from a real `0.0`.
#[derive(Debug, Clone, Copy, PartialEq)]
struct MeanCell {
    /// `Some(mean)` over the `Scored` URLs in scope; `None` iff `n == 0`.
    value: Option<f64>,
    /// Number of `Scored` URLs the mean was computed over (the numerator of
    /// "N of total scoped").
    n: usize,
    /// Total URLs in scope (overall: corpus size; per shape: that shape's
    /// URL count) — the denominator, so the reader sees the exclusion.
    total: usize,
}

impl MeanCell {
    /// Render as `0.83 (N=12 of 50 scored)`, or — when **no** URL in scope was
    /// `Scored` — exactly `n/a (0 of 50 scored)`. NEVER `0.00`/`100%`/blank
    /// for an empty sample: a failed/empty run must read as a non-result, not
    /// a fake perfect/zero score (HLD §5 Bug-E2 doctrine, report layer).
    fn render(&self) -> String {
        match self.value {
            Some(m) => format!("{m:.4} (N={} of {} scored)", self.n, self.total),
            None => format!("n/a (0 of {} scored)", self.total),
        }
    }
}

/// Mean of `f` applied to every `Scored` URL in `records`, **excluding** every
/// `NotScored` URL (the Bug-E2 report-layer gate — a non-scored URL is never
/// coerced into the average as `0.0`/`1.0`). `total` is the in-scope URL count
/// (so the rendered cell shows how many were excluded).
///
/// Returns a [`MeanCell`] whose `value` is `None` iff **no** in-scope URL was
/// `Scored` — structurally a non-result, never a fake `0.0`.
fn mean_of_scored<'a, I, F>(records: I, total: usize, f: F) -> MeanCell
where
    I: IntoIterator<Item = &'a crate::score::UrlRecord>,
    F: Fn(f64, f64, f64) -> f64,
{
    let mut sum = 0.0;
    let mut n = 0usize;
    for r in records {
        if let ScoreOutcome::Scored {
            coverage,
            precision,
            edit_sim,
        } = r.score
        {
            // Cheap invariant guard: Coverage/Precision are [0,1] by metrics
            // construction (`metrics::jaccard`/`precision` over the single
            // tokenizer), so a NaN/inf here means a metrics-layer defect — it
            // must never silently propagate into a rendered mean (a Bug-E2
            // class laundering). Debug-only; the release path is unchanged.
            debug_assert!(
                coverage.is_finite(),
                "coverage must be finite (got {coverage}) — a NaN/inf must \
                 never reach the report mean"
            );
            debug_assert!(
                precision.is_finite(),
                "precision must be finite (got {precision}) — a NaN/inf must \
                 never reach the report mean"
            );
            sum += f(coverage, precision, edit_sim);
            n += 1;
        }
    }
    MeanCell {
        value: if n == 0 { None } else { Some(sum / n as f64) },
        n,
        total,
    }
}

/// Gold word-bands (HLD §7 `gold.tsv` columns `min_words` / `max_words`) — a
/// **report-layer** input, not scoring data.
///
/// The scoring hierarchy ([`score::GoldSet`]) deliberately models only the
/// URL→expected-text mapping it needs; the word band is purely for the §9 gold
/// **report** (pass/fail vs `min_words..max_words`), so it is loaded here, not
/// there (no premature abstraction — HLD §3; `score.rs` is untouched). This is
/// **not** reparsing `results.json` and **not** reimplementing scoring — it is
/// a distinct report-only concern.
///
/// Keyed by URL → `(min_words, max_words)`. `BTreeMap` so iteration is
/// deterministic (the report must be byte-stable for a fixed input).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GoldBands {
    by_url: BTreeMap<String, (usize, usize)>,
}

impl GoldBands {
    /// Whether no gold bands are loaded (the M1 / pre-freeze state).
    fn is_empty(&self) -> bool {
        self.by_url.is_empty()
    }

    /// Load the word-bands from `corpus_dir/gold/gold.tsv` (HLD §7). An absent
    /// `gold/` dir or `gold.tsv` ⇒ an **empty** set (`Ok`, not an error — the
    /// M1 / pre-freeze state, mirroring [`score::GoldSet::load`]'s
    /// absent-manifest contract). A *present* row with a non-numeric or
    /// inverted (`min > max`) band is a hard error: a malformed gold band must
    /// fail loudly, never silently disable the §9 gold pass/fail check.
    pub fn load(corpus_dir: &Path) -> Result<GoldBands, GoldBandError> {
        let manifest = corpus_dir.join("gold").join("gold.tsv");
        let text = match fs::read_to_string(&manifest) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(GoldBands::default());
            }
            Err(e) => return Err(GoldBandError::Io(e)),
        };

        let mut by_url = BTreeMap::new();
        for (idx, raw) in text.lines().enumerate() {
            let line = idx + 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // url, snapshot_filename, expected_text_file, min_words,
            // max_words, why_critical — exactly 6 columns (HLD §7), same shape
            // score::GoldSet::load validates (kept consistent on purpose).
            let fields: Vec<&str> = raw.split('\t').collect();
            if fields.len() != 6 {
                return Err(GoldBandError::MalformedRow {
                    line,
                    fields: fields.len(),
                });
            }
            let url = fields[0].to_string();
            let min = parse_band(fields[3], line, "min_words")?;
            let max = parse_band(fields[4], line, "max_words")?;
            if min > max {
                return Err(GoldBandError::InvertedBand { line, min, max });
            }
            by_url.insert(url, (min, max));
        }
        Ok(GoldBands { by_url })
    }
}

/// Parse a `gold.tsv` word-band column to `usize`, attributing a parse failure
/// to the 1-based line + column name so a human editing the TSV can jump to it.
fn parse_band(raw: &str, line: usize, col: &'static str) -> Result<usize, GoldBandError> {
    raw.trim()
        .parse::<usize>()
        .map_err(|_| GoldBandError::NonNumericBand {
            line,
            col,
            value: raw.to_string(),
        })
}

/// Errors from loading the gold word-bands (HLD §7). Rows carry the 1-based
/// line so a human editing `gold.tsv` can jump straight to it. Mirrors the
/// fail-loud doctrine of [`score::GoldError`] — a malformed gold band must
/// never be silently dropped (which would disable the §9 pass/fail check).
#[derive(Debug)]
pub enum GoldBandError {
    /// `gold.tsv` could not be read (distinct from absent: absence ⇒ empty).
    Io(std::io::Error),
    /// A non-comment, non-blank row did not have exactly 6 tab-separated
    /// fields (HLD §7 columns).
    MalformedRow { line: usize, fields: usize },
    /// A `min_words` / `max_words` column was not a non-negative integer.
    NonNumericBand {
        line: usize,
        col: &'static str,
        value: String,
    },
    /// `min_words > max_words` — an impossible band; a curation defect that
    /// must fail loudly, not silently pass/fail-nothing.
    InvertedBand { line: usize, min: usize, max: usize },
}

impl std::fmt::Display for GoldBandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GoldBandError::Io(e) => write!(f, "reading gold/gold.tsv: {e}"),
            GoldBandError::MalformedRow { line, fields } => write!(
                f,
                "gold.tsv line {line}: expected 6 tab-separated fields (url, \
                 snapshot_filename, expected_text_file, min_words, max_words, \
                 why_critical), found {fields}"
            ),
            GoldBandError::NonNumericBand { line, col, value } => write!(
                f,
                "gold.tsv line {line}: {col} {value:?} is not a non-negative \
                 integer — a malformed gold band must fail loudly (HLD §7), \
                 not silently disable the gold pass/fail check."
            ),
            GoldBandError::InvertedBand { line, min, max } => write!(
                f,
                "gold.tsv line {line}: min_words ({min}) > max_words ({max}) — \
                 an impossible word band; fix the gold row."
            ),
        }
    }
}

impl std::error::Error for GoldBandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GoldBandError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Render the human-readable `report.md` from a [`RunResults`] (HLD §9).
///
/// **Pure** given `results`, `gold_set` (the authoritative URL→gold-text
/// hierarchy input — see the gold/band cross-check below), and `bands` (the
/// §7 word-bands): no I/O, no spawn, no clock — so the whole aggregation, the
/// Bug-E2 mean gate, and the O8 agreement gate are unit-tested by synthesising
/// inputs. Deterministic for a fixed input: the only iteration orders are the
/// manifest order of `results.urls`, `BTreeMap`s, and explicit sorts — **no**
/// `HashMap` and no nondeterministic map iteration, so the bytes are stable
/// run-to-run (regression-diff friendly). `results.urls` is in manifest order
/// **because `score::score_corpus` populates it sequentially** in the corpus
/// manifest order; if that loop is ever parallelised it MUST re-sort to a
/// deterministic key (e.g. manifest index) before this report consumes it, or
/// every section that relies on `results.urls` order (the coverage table, the
/// guardrail queue) becomes run-to-run unstable and breaks the regression
/// diff. (`BTreeMap`/explicit-sort sections are already order-independent.)
///
/// `gold_set` is passed (not just `bands`) so the gold section can
/// **cross-check the two independent `gold.tsv` parsers** (`GoldSet` vs
/// `GoldBands`): a count-preserving column swap — a post-freeze human edit
/// §7/§2.7 explicitly anticipates — would silently desync band-vs-text on the
/// §7 highest-authority signal with **no** parse error in either parser. The
/// report asserts the two URL key sets are equal and emits a loud FAIL row on
/// any disagreement (the differential-oracle principle applied to the parsers
/// themselves; see [`write_gold`]).
///
/// Section order is HLD §9: summary (corpus size, per-status counts, means
/// overall + per `shape_class`), the coverage table, disagreements ranked
/// worst-first, the guardrail-flagged queue, then the gold-set section.
pub fn render_report(results: &RunResults, gold_set: &GoldSet, bands: &GoldBands) -> String {
    let mut md = String::new();

    write_header(&mut md, results);
    write_summary(&mut md, results);
    write_agreement(&mut md, results);
    write_coverage_table(&mut md, results);
    write_disagreements(&mut md, results);
    write_guardrail(&mut md, results);
    write_gold(&mut md, results, gold_set, bands);

    md
}

/// Write `report.md` into `run_dir` (the **same** `runs/<ts>/` directory that
/// holds `results.json` — HLD §9). Returns the path written.
///
/// The caller passes the run directory (the parent of the `results.json` path
/// [`score::write_results`] returned), so the report lands beside the results
/// it was generated from — one run, one directory.
pub fn write_report(markdown: &str, run_dir: &Path) -> std::io::Result<PathBuf> {
    let path = run_dir.join("report.md");
    fs::write(&path, markdown)?;
    Ok(path)
}

/// Run header (HLD §9 — the host identity / timestamp / corpus size, so the
/// report states the provenance the regression check pins on).
fn write_header(md: &mut String, r: &RunResults) {
    let _ = writeln!(md, "# mdrcel differential test report");
    let _ = writeln!(md);
    let _ = writeln!(md, "- host: `{}`", r.host);
    let _ = writeln!(md, "- utc_timestamp: `{}`", r.utc_timestamp);
    let _ = writeln!(md, "- corpus size: {}", r.corpus_size);
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "> Means are computed **only** over URLs that were actually *scored* \
         (a real crate `ok` against a valid non-empty reference). Not-scored \
         URLs are excluded and never counted as 0 or 1; every mean shows its \
         sample N so an empty/failed run reads as a non-result, not a fake \
         perfect/zero score."
    );
    let _ = writeln!(md);
}

/// Summary: per-status counts (every producer) + mean Coverage/Precision
/// overall and per `shape_class` (HLD §9.1). Each mean is a [`MeanCell`] so it
/// always carries its N and renders `n/a` at zero scored.
fn write_summary(md: &mut String, r: &RunResults) {
    let _ = writeln!(md, "## Summary");
    let _ = writeln!(md);

    // Per-status counts — shown explicitly so the reader sees how many URLs
    // were excluded from the means and why (HLD §9, the Bug-E2 visibility
    // requirement). Source maps are BTreeMaps ⇒ deterministic order.
    let _ = writeln!(md, "### Status counts");
    let _ = writeln!(md);
    write_status_line(md, "crate", &r.status_counts.crate_status);
    write_status_line(
        md,
        &format!("{} (oracle #1)", OracleKind::Trafilatura.wire_name()),
        &r.status_counts.trafilatura_status,
    );
    write_status_line(
        md,
        &format!(
            "{} (oracle #2, guardrail)",
            OracleKind::ReadabilityJs.wire_name()
        ),
        &r.status_counts.readability_status,
    );
    let _ = writeln!(md);

    let scored = r
        .urls
        .iter()
        .filter(|u| matches!(u.score, ScoreOutcome::Scored { .. }))
        .count();
    let _ = writeln!(
        md,
        "**Scored: {scored} of {} URLs.**{}",
        r.corpus_size,
        if scored == 0 {
            " (No URL produced a trusted score — the means below are `n/a`. \
             This is the expected Milestone-1 floor, not a failure laundered \
             into a passing number.)"
        } else {
            ""
        }
    );
    let _ = writeln!(md);

    // Low-scored-fraction caveat (HLD §5 — the same non-representative
    // discipline the agreement distribution obeys, applied to the most-read
    // numbers). Only when SOME URL is scored but it is a minority of the
    // corpus: scored == 0 keeps the existing M1 `n/a` path (handled above and
    // by MeanCell::render) — this banner is the additional low-fraction case,
    // never a replacement for the zero case. Mirrors the agreement blockquote.
    if scored > 0
        && r.corpus_size > 0
        && (scored as f64) < SCORED_FRACTION_MIN * r.corpus_size as f64
    {
        let _ = writeln!(
            md,
            "> **Low scored fraction: N={scored} of {} — the means below are \
             over a minority of the corpus and may not be representative.**",
            r.corpus_size
        );
        let _ = writeln!(md);
    }

    // Overall means — Scored-only, with N (the Bug-E2 report-layer gate).
    let cov = mean_of_scored(&r.urls, r.corpus_size, |c, _, _| c);
    let prec = mean_of_scored(&r.urls, r.corpus_size, |_, p, _| p);
    let _ = writeln!(md, "### Means (overall)");
    let _ = writeln!(md);
    let _ = writeln!(md, "- mean Coverage: {}", cov.render());
    let _ = writeln!(md, "- mean Precision: {}", prec.render());
    let _ = writeln!(md);

    // Per-shape_class means — same Scored-only gate; a shape with 0 scored
    // renders `n/a` (never 0.0). BTreeMap keyed by the shape token ⇒ stable.
    let _ = writeln!(md, "### Means (per shape_class)");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "| shape_class | URLs | mean Coverage | mean Precision |"
    );
    let _ = writeln!(md, "|---|---|---|---|");
    let mut by_shape: BTreeMap<&str, Vec<&crate::score::UrlRecord>> = BTreeMap::new();
    for u in &r.urls {
        by_shape.entry(u.shape_class.as_str()).or_default().push(u);
    }
    for (shape, recs) in &by_shape {
        let total = recs.len();
        let c = mean_of_scored(recs.iter().copied(), total, |c, _, _| c);
        let p = mean_of_scored(recs.iter().copied(), total, |_, p, _| p);
        let _ = writeln!(
            md,
            "| {shape} | {total} | {} | {} |",
            c.render(),
            p.render()
        );
    }
    let _ = writeln!(md);
}

/// One `- producer: ok:1, oracle_error:2` status line (deterministic — the
/// source is a `BTreeMap`). An empty map renders `- producer: (none)`.
fn write_status_line(md: &mut String, label: &str, counts: &BTreeMap<String, usize>) {
    if counts.is_empty() {
        let _ = writeln!(md, "- {label}: (none)");
        return;
    }
    let body = counts
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(", ");
    let _ = writeln!(md, "- {label}: {body}");
}

/// Agreement-on-disagreement distribution (HLD §8 / the O8 forward contract).
///
/// The signal is `Some` only where all three sides are valid **and** the two
/// oracles genuinely disagree (often a handful). It is rendered **with its
/// sample N** (`N=k of m`, m = corpus size), **flagged non-representative**
/// below [`AGREEMENT_MIN_SAMPLES`], and at **N=0** rendered explicitly as
/// `no agreement samples (N=0)` — **never** a bare "crate sides with
/// Trafilatura X%". A percentage over a handful read as a population statistic
/// is exactly the laundered number the harness doctrine forbids.
fn write_agreement(md: &mut String, r: &RunResults) {
    let _ = writeln!(md, "## Agreement on disagreement");
    let _ = writeln!(md);

    let (mut to_traf, mut to_read, mut tie) = (0usize, 0usize, 0usize);
    for u in &r.urls {
        match u.agreement {
            Some(Agreement::CloserToTrafilatura) => to_traf += 1,
            Some(Agreement::CloserToReadability) => to_read += 1,
            Some(Agreement::Tie) => tie += 1,
            None => {}
        }
    }
    let n = to_traf + to_read + tie;
    let m = r.corpus_size;

    if n == 0 {
        // O8 / Bug-E2: NEVER a percentage at N=0. Explicit non-result.
        let _ = writeln!(
            md,
            "_No agreement samples (N=0 of {m})._ The agreement signal only \
             exists where the crate and **both** oracles produced a valid \
             non-empty extraction *and* the two oracles genuinely disagree; \
             at the Milestone-1 floor there are none, so no distribution is \
             reported (a percentage over zero samples would be meaningless)."
        );
        let _ = writeln!(md);
        return;
    }

    if n < AGREEMENT_MIN_SAMPLES {
        let _ = writeln!(
            md,
            "> **Non-representative: N={n} (< {AGREEMENT_MIN_SAMPLES}).** The \
             distribution below is over too few URLs to read as a population \
             statistic; it is shown for completeness only, never as \"the \
             crate sides with X %\"."
        );
        let _ = writeln!(md);
    }

    // Always paired with (N=k of m) — never a bare percentage (O8).
    let pct = |k: usize| 100.0 * k as f64 / n as f64;
    let _ = writeln!(
        md,
        "- closer to Trafilatura: {to_traf} ({:.1}%) (N={to_traf} of {n})",
        pct(to_traf)
    );
    let _ = writeln!(
        md,
        "- closer to Readability: {to_read} ({:.1}%) (N={to_read} of {n})",
        pct(to_read)
    );
    let _ = writeln!(md, "- tie: {tie} ({:.1}%) (N={tie} of {n})", pct(tie));
    let _ = writeln!(md, "- agreement samples: N={n} of {m} corpus URLs");
    let _ = writeln!(md);
}

/// Coverage table: one row per URL (manifest order — deterministic) ×
/// {Trafilatura wc, Readability wc, crate wc, Coverage, Precision, flags,
/// statuses} (HLD §9.2). A `NotScored` URL shows its reason in the Coverage
/// cell, never a fake number.
fn write_coverage_table(md: &mut String, r: &RunResults) {
    let _ = writeln!(md, "## Coverage table");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "| URL | shape | {} wc | {} wc | crate wc | Coverage | Precision | \
         edit_sim | guardrail | crate / traf / read status |",
        OracleKind::Trafilatura.wire_name(),
        OracleKind::ReadabilityJs.wire_name()
    );
    let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|---|");
    for u in &r.urls {
        let (cov, prec, edit) = match u.score {
            ScoreOutcome::Scored {
                coverage,
                precision,
                ..
            } => (
                format!("{coverage:.4}"),
                format!("{precision:.4}"),
                u.edit_sim
                    .map(|e| format!("{e:.4}"))
                    .unwrap_or_else(|| "-".into()),
            ),
            // NotScored: the Coverage cell is the *reason*, never a number —
            // the Bug-E2 gate carried into the per-URL table.
            ScoreOutcome::NotScored { reason } => {
                let why = format!("not scored: {}", not_scored_reason_str(reason));
                (why, "-".into(), "-".into())
            }
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} / {} / {} |",
            // URL is free text — escape so a legal `|`/newline cannot split
            // the row or shift every later column (see `md_cell`).
            md_cell(&u.url),
            u.shape_class,
            opt_usize(u.word_counts.trafilatura_wc),
            opt_usize(u.word_counts.readability_wc),
            opt_usize(u.word_counts.crate_wc),
            cov,
            prec,
            edit,
            if u.guardrail_flag { "yes" } else { "" },
            u.crate_status,
            u.trafilatura_status,
            u.readability_status,
        );
    }
    let _ = writeln!(md);
}

/// Disagreements ranked by severity (HLD §9.3): a URL where the two oracles
/// genuinely disagree (the recorded [`Agreement`] is `Some` — by construction
/// their token-set Jaccard was `< 0.5`, see `score::agreement`) **and** the
/// crate-to-reference Coverage is low is a **candidate bug**. Ranked
/// worst-first by ascending Coverage, then URL (a deterministic tiebreak so
/// the ordering is stable for a fixed input).
///
/// Only `Scored` URLs have a Coverage to rank by; a `NotScored` URL has no
/// trusted similarity number, so it cannot be ranked here (it is visible in
/// the status counts / coverage table instead — never invented into a 0.0).
///
/// **Known blind spot (documented in the preamble):** this ranking keys on
/// oracle↔oracle disagreement, so it **cannot** surface a URL where *both*
/// oracles are wrong the same way and the crate faithfully matches them —
/// Coverage is high and there is no disagreement, so it never ranks. That
/// failure mode is the gold set's and the guardrail queue's job, not this
/// ranking's; the preamble states this so an empty/short list is not
/// mis-read as "no candidate bugs."
fn write_disagreements(md: &mut String, r: &RunResults) {
    let _ = writeln!(md, "## Disagreements ranked by severity");
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "_Candidate bugs: the two oracles disagree (their Jaccard < 0.5) **and** \
         the crate's Coverage to the reference is low. Worst (lowest Coverage) \
         first. Only scored URLs are rankable._"
    );
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "_Limitation: this ranking cannot surface cases where **both** oracles \
         are wrong the same way and the crate faithfully matches them (high \
         Coverage, no oracle disagreement) — those rely on the guardrail queue \
         and the gold set, not this list._"
    );
    let _ = writeln!(md);

    let mut ranked: Vec<(&str, f64, Option<Agreement>)> = r
        .urls
        .iter()
        .filter_map(|u| match (u.agreement, &u.score) {
            (Some(a), ScoreOutcome::Scored { coverage, .. }) => {
                Some((u.url.as_str(), *coverage, Some(a)))
            }
            _ => None,
        })
        .collect();
    // Worst-first by Coverage asc; URL asc as the deterministic tiebreak so
    // the report is byte-stable for a fixed RunResults.
    ranked.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(b.0))
    });

    if ranked.is_empty() {
        let _ = writeln!(
            md,
            "_None: no scored URL has the two oracles in genuine disagreement \
             (at the Milestone-1 floor nothing is scored)._"
        );
        let _ = writeln!(md);
        return;
    }

    let _ = writeln!(md, "| rank | URL | Coverage | crate is closer to |");
    let _ = writeln!(md, "|---|---|---|---|");
    for (i, (url, cov, agree)) in ranked.iter().take(TOP_DISAGREEMENTS).enumerate() {
        let closer = match agree {
            Some(Agreement::CloserToTrafilatura) => "trafilatura",
            Some(Agreement::CloserToReadability) => "readability-js",
            Some(Agreement::Tie) => "tie",
            None => "-",
        };
        // URL is free text — escape (see `md_cell`).
        let _ = writeln!(
            md,
            "| {} | {} | {:.4} | {} |",
            i + 1,
            md_cell(url),
            cov,
            closer
        );
    }
    let _ = writeln!(md);
}

/// Guardrail-flagged URLs (HLD §9.4): the suspected-Trafilatura-truncation
/// queue — the gold-set candidate list. Manifest order (deterministic).
fn write_guardrail(md: &mut String, r: &RunResults) {
    let _ = writeln!(
        md,
        "## Guardrail-flagged URLs (suspected Trafilatura truncation)"
    );
    let _ = writeln!(md);
    let _ = writeln!(
        md,
        "_Readability extracted > 1.25× Trafilatura's words on a non-hub page \
         — suspected Trafilatura truncation; the gold-set candidate queue._"
    );
    let _ = writeln!(md);

    let flagged: Vec<&crate::score::UrlRecord> =
        r.urls.iter().filter(|u| u.guardrail_flag).collect();
    if flagged.is_empty() {
        let _ = writeln!(md, "_None flagged._");
        let _ = writeln!(md);
        return;
    }
    let _ = writeln!(
        md,
        "| URL | shape | {} wc | {} wc |",
        OracleKind::Trafilatura.wire_name(),
        OracleKind::ReadabilityJs.wire_name()
    );
    let _ = writeln!(md, "|---|---|---|---|");
    for u in flagged {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} |",
            // URL is free text — escape (see `md_cell`).
            md_cell(&u.url),
            u.shape_class,
            opt_usize(u.word_counts.trafilatura_wc),
            opt_usize(u.word_counts.readability_wc),
        );
    }
    let _ = writeln!(md);
}

/// Gold-set section (HLD §9.5): per gold URL, pass/fail vs `min_words..
/// max_words` and Coverage vs the gold text.
///
/// The Coverage for a gold URL **is** the recorded [`ScoreOutcome`] coverage —
/// `score::score_url` uses the gold text as the reference whenever a gold
/// entry exists (HLD §7), so no recomputation is needed or done here. Pass =
/// the crate was `Scored` **and** its recomputed word count is within the
/// band. A `NotScored` gold URL is an explicit **FAIL (not scored)** — never
/// laundered into a pass. Iterated over the `BTreeMap` of bands ⇒ deterministic.
///
/// # Gold/band cross-check — the two `gold.tsv` parsers must agree
///
/// `gold_set` (the authoritative URL→text hierarchy input) and `bands` (the
/// §7 word-bands) are parsed from the **same** `gold.tsv` by two **independent**
/// parsers ([`GoldSet::load`] / [`GoldBands::load`]). A count-preserving column
/// swap — a post-freeze human edit §7/§2.7 explicitly anticipates — keeps both
/// at 6 fields with **no** parse error in either, yet would silently bind a
/// band to one URL and the gold text to another: a desync on the §7
/// highest-authority signal. Before rendering, the two URL **key sets** are
/// asserted equal; any URL present in one parser's view but not the other's is
/// a **loud FAIL row** (`**FAIL — gold/band manifest disagree …**`), never a
/// silent skip. This is the differential-oracle principle turned on the
/// parsers themselves. A loud row (not a hard error / panic) is deliberate:
/// `render_report` is pure and returns the whole report; aborting here would
/// suppress the status counts / means that make the M1 floor honest, which is
/// itself a Bug-E2-class failure (hide the evidence). The loud row is
/// maximally visible *and* keeps the rest of the report readable, consistent
/// with how this section already renders inline `**FAIL …**` rows rather than
/// aborting.
fn write_gold(md: &mut String, r: &RunResults, gold_set: &GoldSet, bands: &GoldBands) {
    let _ = writeln!(md, "## Gold set");
    let _ = writeln!(md);

    // Cross-check the two independent gold.tsv parsers FIRST, regardless of
    // emptiness: "one parser sees gold rows, the other sees none" is itself a
    // disagreement that must be loud, not silently collapsed into "_No gold
    // set_". Both-empty (the genuine pre-freeze state) ⇒ no disagreement.
    let band_urls: BTreeSet<&str> = bands.by_url.keys().map(String::as_str).collect();
    let gold_urls: BTreeSet<&str> = gold_set.urls().collect();
    if band_urls != gold_urls {
        let _ = writeln!(
            md,
            "> **FAIL — gold/band manifest disagree. The gold expected-text \
             parser (`GoldSet`) and the word-band parser (`GoldBands`) read \
             different URL sets from the *same* `gold.tsv`. A count-preserving \
             column swap (an anticipated post-freeze human edit, HLD §7/§2.7) \
             desyncs band-vs-text on the §7 highest-authority signal with no \
             parse error — fix `gold.tsv` before trusting any gold result \
             below.**"
        );
        let _ = writeln!(md);
        // One explicit FAIL row per offending URL, deterministically ordered
        // (BTreeSet). Banded-but-not-gold AND gold-but-not-banded are both
        // surfaced — neither direction is a silent skip.
        for url in band_urls.difference(&gold_urls) {
            let _ = writeln!(
                md,
                "- **FAIL — gold/band manifest disagree: {} (has a word band \
                 but no gold expected text)**",
                md_cell(url)
            );
        }
        for url in gold_urls.difference(&band_urls) {
            let _ = writeln!(
                md,
                "- **FAIL — gold/band manifest disagree: {} (has gold expected \
                 text but no word band)**",
                md_cell(url)
            );
        }
        let _ = writeln!(md);
    }

    if bands.is_empty() {
        let _ = writeln!(
            md,
            "_No gold set (the §2.7 gold-set freeze happens before crate \
             tuning — later than the Milestone-1 floor). Nothing to check._"
        );
        let _ = writeln!(md);
        return;
    }

    // Index URL → record once (manifest order is irrelevant here; we iterate
    // the BTreeMap of bands for determinism).
    let by_url: BTreeMap<&str, &crate::score::UrlRecord> =
        r.urls.iter().map(|u| (u.url.as_str(), u)).collect();

    let _ = writeln!(
        md,
        "| gold URL | band (min..max words) | crate wc | Coverage vs gold | \
         result |"
    );
    let _ = writeln!(md, "|---|---|---|---|---|");
    for (url, (min, max)) in &bands.by_url {
        let band = format!("{min}..{max}");
        // Free-text cell — escape so a legal `|`/newline in the URL cannot
        // split the row or shift the columns a human reads (see `md_cell`).
        let url_cell = md_cell(url);
        match by_url.get(url.as_str()) {
            None => {
                // A gold URL not in the corpus run — loud, never a silent pass.
                let _ = writeln!(
                    md,
                    "| {url_cell} | {band} | - | - | **FAIL (gold URL absent \
                     from this run)** |"
                );
            }
            Some(rec) => match rec.score {
                ScoreOutcome::Scored { coverage, .. } => {
                    let wc = rec.word_counts.crate_wc;
                    let in_band = wc.map(|w| w >= *min && w <= *max).unwrap_or(false);
                    let _ = writeln!(
                        md,
                        "| {url_cell} | {band} | {} | {coverage:.4} | {} |",
                        opt_usize(wc),
                        if in_band {
                            "**PASS**"
                        } else {
                            "**FAIL (word count out of band)**"
                        }
                    );
                }
                ScoreOutcome::NotScored { reason } => {
                    // Bug-E2: a not-scored gold URL is an explicit FAIL with
                    // its reason — never a pass, never an invented Coverage.
                    let _ = writeln!(
                        md,
                        "| {url_cell} | {band} | {} | n/a | **FAIL (not \
                         scored: {})** |",
                        opt_usize(rec.word_counts.crate_wc),
                        not_scored_reason_str(reason),
                    );
                }
            },
        }
    }
    let _ = writeln!(md);
}

/// Escape a free-text value for safe interpolation into a markdown pipe table
/// cell. A raw `|` inside a `| … |` row terminates the cell early, splitting
/// one logical cell into two (or shifting every later column) — a legal `|` in
/// a URL or reason string would otherwise **corrupt or inject** the
/// human-read table. `|` is escaped to `\|` (the GFM cell-literal pipe), and
/// any newline / carriage-return / other ASCII control char (which would break
/// the row across lines, again de-aligning the table) is replaced by a single
/// space. Pure and allocation-cheap; applied to every free-text cell (URLs,
/// not-scored reasons) across **all** tables so no single cell can desync the
/// columns a human reads.
fn md_cell(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '|' => out.push_str("\\|"),
            // Newlines/CR and any other control char would break the row onto
            // a new line or render invisibly; collapse to a single space so
            // the row stays one well-formed line.
            '\n' | '\r' => out.push(' '),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// `Some(n)` → `n`, `None` → `-` (an absent count is unambiguously distinct
/// from a real `0`, mirroring `score::WordCounts`' `Option` doctrine).
fn opt_usize(v: Option<usize>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "-".to_string(),
    }
}

/// Wire spelling of a [`score::NotScoredReason`] for the report (so a
/// not-scored URL stays *explained*, never a bare blank — the Bug-E2 lesson).
fn not_scored_reason_str(reason: crate::score::NotScoredReason) -> &'static str {
    use crate::score::NotScoredReason as N;
    match reason {
        N::CrateNotImplemented => "crate not_implemented",
        N::CrateError => "crate_error",
        N::ReferenceUnavailable => "reference unavailable (oracle error/timeout, no gold)",
        N::ReferenceEmpty => "reference empty",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::score::{
        Agreement, NotScoredReason, RunResults, ScoreOutcome, StatusCounts, StatusDetail,
        UrlRecord, WordCounts,
    };

    // ---- Builders: synthesize a RunResults without scoring anything --------

    /// A `Scored` URL record with the given coverage/precision.
    fn scored(url: &str, shape: &str, coverage: f64, precision: f64) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            shape_class: shape.to_string(),
            crate_status: "ok".to_string(),
            trafilatura_status: "ok".to_string(),
            readability_status: "ok".to_string(),
            status_detail: StatusDetail::default(),
            word_counts: WordCounts {
                crate_wc: Some(100),
                trafilatura_wc: Some(110),
                readability_wc: Some(105),
            },
            score: ScoreOutcome::Scored {
                coverage,
                precision,
                edit_sim: 0.5,
            },
            edit_sim: Some(0.5),
            guardrail_flag: false,
            agreement: None,
        }
    }

    /// A `NotScored` URL record (the M1 floor shape by default).
    fn not_scored(url: &str, shape: &str, reason: NotScoredReason) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            shape_class: shape.to_string(),
            crate_status: "not_implemented".to_string(),
            trafilatura_status: "oracle_error".to_string(),
            readability_status: "oracle_error".to_string(),
            status_detail: StatusDetail::default(),
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

    fn run(urls: Vec<UrlRecord>) -> RunResults {
        let mut sc = StatusCounts::default();
        for u in &urls {
            *sc.crate_status.entry(u.crate_status.clone()).or_insert(0) += 1;
            *sc.trafilatura_status
                .entry(u.trafilatura_status.clone())
                .or_insert(0) += 1;
            *sc.readability_status
                .entry(u.readability_status.clone())
                .or_insert(0) += 1;
        }
        RunResults {
            host: "test-host".to_string(),
            utc_timestamp: "2026-05-17T12-00-00Z".to_string(),
            corpus_size: urls.len(),
            status_counts: sc,
            urls,
        }
    }

    /// Build a real [`GoldSet`] over `urls` via the **public** `GoldSet::load`
    /// (no `score.rs` test back-door — the only added `score.rs` surface is the
    /// read-only `urls()` accessor). Writes a well-formed temp `gold/gold.tsv`
    /// plus one non-empty `.txt` per URL so `load` succeeds; the caller passes
    /// a unique `tag` so concurrent tests do not share a temp dir. The
    /// returned `GoldSet`'s URL key set is exactly `urls`.
    fn gold_set_with_urls(tag: &str, urls: &[&str]) -> GoldSet {
        let dir = std::env::temp_dir().join(format!("mdrcel-report-goldset-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        std::fs::create_dir_all(&gold_dir).unwrap();
        let mut tsv = String::new();
        for (i, u) in urls.iter().enumerate() {
            let txt = format!("g{i}.txt");
            std::fs::write(gold_dir.join(&txt), "gold body text").unwrap();
            // url, snapshot_filename, expected_text_file, min, max, why
            let _ = writeln!(tsv, "{u}\ts{i}.html\t{txt}\t1\t9\twhy");
        }
        std::fs::write(gold_dir.join("gold.tsv"), tsv).unwrap();
        let g = GoldSet::load(&dir).expect("well-formed temp gold.tsv loads");
        let _ = std::fs::remove_dir_all(&dir);
        g
    }

    /// Epsilon for non-exact f64 mean comparisons (same rationale as
    /// `metrics.rs`/`score.rs`: a mean of small ratios accumulates error
    /// far below 1e-9; the rendered `{:.4}` string is the real contract).
    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // ---- Bug-E2 report-layer gate: means over Scored ONLY, with N ----------

    #[test]
    fn mean_is_over_scored_only_with_correct_n() {
        // 2 Scored (cov 0.4, 0.8 ⇒ mean 0.6 over N=2) + 1 NotScored excluded.
        // total scope = 3 (corpus size) so the cell shows "N=2 of 3 scored".
        let r = run(vec![
            scored("https://a.test/1", "news", 0.4, 0.6),
            scored("https://a.test/2", "news", 0.8, 0.9),
            not_scored(
                "https://a.test/3",
                "news",
                NotScoredReason::CrateNotImplemented,
            ),
        ]);
        let cell = mean_of_scored(&r.urls, r.corpus_size, |c, _, _| c);
        assert!(
            approx(cell.value.expect("2 scored ⇒ a mean exists"), 0.6),
            "mean must average ONLY the 2 Scored (got {:?})",
            cell.value
        );
        assert_eq!(cell.n, 2, "N must be the Scored count, not 3");
        assert_eq!(cell.total, 3);
        // The rendered string is the actual report-layer contract.
        assert_eq!(cell.render(), "0.6000 (N=2 of 3 scored)");
    }

    #[test]
    fn not_scored_never_coerced_into_the_mean_as_zero_or_one() {
        // If NotScored were (wrongly) folded in as 0.0 the mean would be 0.4;
        // as 1.0 it would be ~0.66. The correct Scored-only mean is 0.6.
        let r = run(vec![
            scored("https://a.test/1", "news", 0.4, 0.5),
            scored("https://a.test/2", "news", 0.8, 0.5),
            not_scored("https://a.test/3", "news", NotScoredReason::ReferenceEmpty),
            not_scored("https://a.test/4", "news", NotScoredReason::CrateError),
        ]);
        let cell = mean_of_scored(&r.urls, r.corpus_size, |c, _, _| c);
        assert!(approx(cell.value.expect("2 scored"), 0.6));
        assert_eq!(cell.n, 2);
        // And the excluded ones are still *counted* in the status summary.
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(
            md.contains("not_implemented") || md.contains("crate_error"),
            "excluded URLs must still appear in the per-status counts"
        );
        assert!(md.contains("Scored: 2 of 4 URLs"));
    }

    /// THE key Bug-E2 test: an all-`NotScored` run (the M1 floor) must render
    /// every mean as `n/a (0 of N scored)` — NEVER `0.00`, `100%`, or a blank
    /// that reads as a value. A failed/empty run is visibly a non-result.
    #[test]
    fn all_not_scored_m1_floor_means_render_n_a_not_zero_or_hundred() {
        let r = run(vec![
            not_scored(
                "https://a.test/1",
                "wikipedia",
                NotScoredReason::CrateNotImplemented,
            ),
            not_scored(
                "https://a.test/2",
                "hub_index",
                NotScoredReason::CrateNotImplemented,
            ),
            not_scored(
                "https://a.test/3",
                "edge_case",
                NotScoredReason::CrateNotImplemented,
            ),
        ]);
        let cov = mean_of_scored(&r.urls, r.corpus_size, |c, _, _| c);
        let prec = mean_of_scored(&r.urls, r.corpus_size, |_, p, _| p);
        assert_eq!(cov.value, None, "0 scored ⇒ no mean value (NOT 0.0)");
        assert_eq!(prec.value, None);
        assert_eq!(cov.render(), "n/a (0 of 3 scored)");
        assert_eq!(prec.render(), "n/a (0 of 3 scored)");

        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(
            md.contains("mean Coverage: n/a (0 of 3 scored)"),
            "M1 mean Coverage must render n/a, not a number:\n{md}"
        );
        assert!(md.contains("mean Precision: n/a (0 of 3 scored)"));
        // Must NOT have laundered a fake perfect/zero score anywhere in the
        // means section.
        assert!(
            !md.contains("mean Coverage: 0.00") && !md.contains("mean Coverage: 1.00"),
            "M1 floor must never show a fake 0.00/1.00 mean Coverage:\n{md}"
        );
        assert!(
            !md.contains("100%") || !md.contains("mean"),
            "no mean may be laundered into 100%:\n{md}"
        );
        assert!(md.contains("Scored: 0 of 3 URLs"));
    }

    // ---- #4 (review): headline means obey the non-representative discipline -

    #[test]
    fn low_scored_fraction_is_flagged_non_representative() {
        // 1 Scored of 4 (25% < SCORED_FRACTION_MIN=0.5) ⇒ the headline means
        // must carry the low-scored-fraction blockquote, mirroring the
        // agreement non-representative pattern. Arithmetic unchanged: the mean
        // is still the real Scored-only value with its (N=k of m).
        let r = run(vec![
            scored("https://a.test/1", "news", 0.8, 0.8),
            not_scored("https://a.test/2", "news", NotScoredReason::CrateError),
            not_scored("https://a.test/3", "news", NotScoredReason::CrateError),
            not_scored("https://a.test/4", "news", NotScoredReason::CrateError),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(
            md.contains(
                "> **Low scored fraction: N=1 of 4 — the means below are over \
                 a minority of the corpus and may not be representative.**"
            ),
            "a minority scored fraction must be flagged non-representative:\n{md}"
        );
        // The mean itself is still the honest Scored-only value (unchanged).
        assert!(md.contains("mean Coverage: 0.8000 (N=1 of 4 scored)"));
    }

    #[test]
    fn healthy_scored_fraction_is_not_flagged() {
        // 3 Scored of 4 (75% ≥ 0.5) ⇒ NO low-fraction banner; the (N=k of m)
        // annotation alone suffices for a majority.
        let r = run(vec![
            scored("https://a.test/1", "news", 0.8, 0.8),
            scored("https://a.test/2", "news", 0.6, 0.6),
            scored("https://a.test/3", "news", 0.7, 0.7),
            not_scored("https://a.test/4", "news", NotScoredReason::CrateError),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(
            !md.contains("Low scored fraction"),
            "a majority scored fraction must NOT be flagged:\n{md}"
        );
    }

    #[test]
    fn zero_scored_keeps_the_n_a_path_not_the_low_fraction_banner() {
        // The M1 floor (0 scored) must stay on the EXISTING `n/a` path — the
        // low-fraction banner is the *additional low-fraction* case only, not
        // a replacement for the zero case (which is already handled and tested
        // by all_not_scored_m1_floor_…). 0% must not emit the new banner.
        let r = run(vec![
            not_scored(
                "https://a.test/1",
                "news",
                NotScoredReason::CrateNotImplemented,
            ),
            not_scored(
                "https://a.test/2",
                "news",
                NotScoredReason::CrateNotImplemented,
            ),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(
            !md.contains("Low scored fraction"),
            "0 scored must use the existing n/a path, NOT the low-fraction \
             banner (which is the >0-but-minority case):\n{md}"
        );
        assert!(
            md.contains("mean Coverage: n/a (0 of 2 scored)"),
            "0 scored must still render the existing n/a mean:\n{md}"
        );
    }

    // ---- Per-shape_class means (Bug-E2 gate applies per shape) -------------

    #[test]
    fn per_shape_means_correct_and_zero_scored_shape_is_n_a() {
        // wikipedia: 2 Scored (0.6, 0.8 ⇒ mean 0.7). news: 1 NotScored ⇒ the
        // news row must be n/a (0 of 1), NEVER 0.0.
        let r = run(vec![
            scored("https://w.test/1", "wikipedia", 0.6, 0.7),
            scored("https://w.test/2", "wikipedia", 0.8, 0.9),
            not_scored("https://n.test/1", "news", NotScoredReason::CrateError),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        // Deterministic BTreeMap order: news before wikipedia.
        assert!(
            md.contains("| news | 1 | n/a (0 of 1 scored) | n/a (0 of 1 scored) |"),
            "a shape with 0 scored must be n/a, not 0.0:\n{md}"
        );
        assert!(
            md.contains("| wikipedia | 2 | 0.7000 (N=2 of 2 scored) | 0.8000 (N=2 of 2 scored) |"),
            "wikipedia per-shape mean wrong:\n{md}"
        );
    }

    // ---- Disagreement ranking: worst-first, deterministic ------------------

    #[test]
    fn disagreements_ranked_worst_first_known_order() {
        // Three Scored URLs with agreement set (oracles disagree). Coverage
        // 0.2, 0.5, 0.9 ⇒ ranked 0.2, 0.5, 0.9 (worst first). A 4th URL with
        // agreement=None must NOT appear; a NotScored one must NOT appear.
        let mut u_bad = scored("https://z.test/bad", "news", 0.2, 0.3);
        u_bad.agreement = Some(Agreement::CloserToReadability);
        let mut u_mid = scored("https://a.test/mid", "news", 0.5, 0.4);
        u_mid.agreement = Some(Agreement::CloserToTrafilatura);
        let mut u_good = scored("https://m.test/good", "news", 0.9, 0.9);
        u_good.agreement = Some(Agreement::Tie);
        let u_noagree = scored("https://q.test/noagree", "news", 0.1, 0.1); // agreement None
        let mut u_ns = not_scored("https://q.test/ns", "news", NotScoredReason::CrateError);
        u_ns.agreement = Some(Agreement::CloserToTrafilatura); // still unrankable

        let r = run(vec![u_good, u_noagree, u_bad, u_mid, u_ns]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());

        // Scope the ordering check to the disagreement SECTION only — URLs
        // also appear earlier in the manifest-order coverage table, so a
        // whole-document `find` would match the wrong occurrence.
        let sec =
            &md[md.find("## Disagreements").unwrap()..md.find("## Guardrail-flagged").unwrap()];
        let bad = sec.find("https://z.test/bad").expect("bad ranked");
        let mid = sec.find("https://a.test/mid").expect("mid ranked");
        let good = sec.find("https://m.test/good").expect("good ranked");
        assert!(bad < mid && mid < good, "must be worst-first (0.2,0.5,0.9)");
        // The disagreement section must not rank the agreement=None URL nor
        // the NotScored one.
        assert!(
            !sec.contains("https://q.test/noagree"),
            "agreement=None URL must not be ranked as a disagreement"
        );
        assert!(
            !sec.contains("https://q.test/ns"),
            "a NotScored URL has no trusted Coverage and must not be ranked"
        );
        // Ranks are 1,2,3 in order.
        assert!(sec.contains("| 1 | https://z.test/bad | 0.2000 | readability-js |"));
        assert!(sec.contains("| 2 | https://a.test/mid | 0.5000 | trafilatura |"));
        assert!(sec.contains("| 3 | https://m.test/good | 0.9000 | tie |"));
    }

    #[test]
    fn disagreement_tiebreak_is_url_for_determinism() {
        // Two disagreements with the SAME coverage must order by URL asc so
        // the report is byte-stable for a fixed input.
        let mut a = scored("https://b.test/x", "news", 0.5, 0.5);
        a.agreement = Some(Agreement::Tie);
        let mut b = scored("https://a.test/x", "news", 0.5, 0.5);
        b.agreement = Some(Agreement::Tie);
        let r = run(vec![a, b]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        // Scope to the disagreement section (the coverage table lists both in
        // manifest order, which would mask the ranking tiebreak).
        let sec =
            &md[md.find("## Disagreements").unwrap()..md.find("## Guardrail-flagged").unwrap()];
        let first = sec.find("https://a.test/x").unwrap();
        let second = sec.find("https://b.test/x").unwrap();
        assert!(first < second, "equal Coverage ⇒ URL-ascending tiebreak");
    }

    #[test]
    fn disagreements_preamble_states_the_both_oracles_wrong_blind_spot() {
        // #3b (review): the ranking cannot surface a URL where BOTH oracles
        // are wrong the same way and the crate faithfully matches them (high
        // Coverage, no oracle disagreement). The preamble must state this so
        // an empty/short list is not mis-read as "no candidate bugs".
        let r = run(vec![scored("https://a.test/1", "news", 0.9, 0.9)]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec =
            &md[md.find("## Disagreements").unwrap()..md.find("## Guardrail-flagged").unwrap()];
        assert!(
            sec.contains("both** oracles")
                && sec.contains("wrong the same way")
                && sec.contains("guardrail queue and the gold set"),
            "the disagreement preamble must document the both-oracles-wrong \
             blind spot:\n{sec}"
        );
    }

    // ---- Guardrail-flagged section -----------------------------------------

    #[test]
    fn guardrail_section_lists_exactly_the_flagged_urls() {
        let mut flagged = scored("https://g.test/flag", "news", 0.5, 0.5);
        flagged.guardrail_flag = true;
        flagged.word_counts = WordCounts {
            crate_wc: Some(50),
            trafilatura_wc: Some(40),
            readability_wc: Some(80),
        };
        let unflagged = scored("https://g.test/clean", "news", 0.9, 0.9);
        let r = run(vec![flagged, unflagged]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec = &md[md.find("## Guardrail-flagged").unwrap()..md.find("## Gold set").unwrap()];
        assert!(
            sec.contains("https://g.test/flag"),
            "flagged URL must be listed"
        );
        assert!(
            !sec.contains("https://g.test/clean"),
            "an unflagged URL must NOT be in the guardrail queue"
        );
    }

    #[test]
    fn guardrail_section_says_none_when_empty() {
        let r = run(vec![scored("https://g.test/clean", "news", 0.9, 0.9)]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec = &md[md.find("## Guardrail-flagged").unwrap()..md.find("## Gold set").unwrap()];
        assert!(sec.contains("_None flagged._"));
    }

    // ---- Gold-set section: pass/fail vs band + Coverage --------------------

    #[test]
    fn gold_section_pass_fail_vs_word_band_and_coverage() {
        let mut g_pass = scored("https://gold.test/pass", "wikipedia", 0.95, 0.9);
        g_pass.word_counts.crate_wc = Some(2600); // within 2500..3000
        let mut g_fail_band = scored("https://gold.test/short", "wikipedia", 0.80, 0.7);
        g_fail_band.word_counts.crate_wc = Some(100); // below 2500..3000
        let g_fail_ns = not_scored(
            "https://gold.test/ns",
            "sec_edgar",
            NotScoredReason::CrateNotImplemented,
        );
        let r = run(vec![g_pass, g_fail_band, g_fail_ns]);

        let mut bands = GoldBands::default();
        bands
            .by_url
            .insert("https://gold.test/pass".into(), (2500, 3000));
        bands
            .by_url
            .insert("https://gold.test/short".into(), (2500, 3000));
        bands
            .by_url
            .insert("https://gold.test/ns".into(), (1000, 5000));

        // Matching GoldSet (same URL key set) so the gold/band cross-check
        // passes — this test exercises PASS/FAIL-vs-band, not the disagreement.
        let gs = gold_set_with_urls(
            "passfail",
            &[
                "https://gold.test/pass",
                "https://gold.test/short",
                "https://gold.test/ns",
            ],
        );
        let md = render_report(&r, &gs, &bands);
        let sec = &md[md.find("## Gold set").unwrap()..];
        assert!(
            !sec.contains("gold/band manifest disagree"),
            "matching gold/band URL sets must NOT trip the cross-check:\n{sec}"
        );
        assert!(
            sec.contains("| https://gold.test/pass | 2500..3000 | 2600 | 0.9500 | **PASS** |"),
            "in-band scored gold URL must PASS with its Coverage:\n{sec}"
        );
        assert!(
            sec.contains("**FAIL (word count out of band)**"),
            "out-of-band gold URL must FAIL:\n{sec}"
        );
        assert!(
            sec.contains("**FAIL (not scored: crate not_implemented)**"),
            "a NotScored gold URL must be an explicit FAIL, never a pass:\n{sec}"
        );
    }

    #[test]
    fn gold_section_says_no_gold_when_bands_empty() {
        let r = run(vec![not_scored(
            "https://a.test/1",
            "news",
            NotScoredReason::CrateNotImplemented,
        )]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert!(md.contains("_No gold set"));
        // Both parsers empty (the genuine pre-freeze state) ⇒ NO disagreement.
        assert!(
            !md.contains("gold/band manifest disagree"),
            "both-empty is the pre-freeze state, not a parser disagreement:\n{md}"
        );
    }

    // ---- #1: the two gold.tsv parsers must agree (differential oracle) ------

    #[test]
    fn gold_band_manifest_disagree_banded_url_not_in_goldset_is_loud_fail() {
        // A URL has a word band but the authoritative GoldSet has no gold
        // text for it — a count-preserving column swap (HLD §7/§2.7) desyncs
        // band-vs-text with NO parse error. Must be a LOUD visible FAIL row,
        // never a silent skip.
        let r = run(vec![scored("https://gold.test/a", "wikipedia", 0.9, 0.9)]);
        let mut bands = GoldBands::default();
        bands.by_url.insert("https://gold.test/a".into(), (1, 9));
        // GoldSet does NOT contain that URL (it has a different one).
        let gs = gold_set_with_urls("banded-not-gold", &["https://gold.test/OTHER"]);

        let md = render_report(&r, &gs, &bands);
        let sec = &md[md.find("## Gold set").unwrap()..];
        assert!(
            sec.contains("**FAIL — gold/band manifest disagree."),
            "a banded-but-not-gold URL must trip a LOUD manifest FAIL:\n{sec}"
        );
        assert!(
            sec.contains(
                "gold/band manifest disagree: https://gold.test/a (has a word \
                 band but no gold expected text)"
            ),
            "the offending URL + direction must be named explicitly:\n{sec}"
        );
    }

    #[test]
    fn gold_band_manifest_disagree_goldset_url_not_banded_is_loud_fail() {
        // Symmetric direction: the GoldSet has gold text for a URL that has
        // no word band. Equally a loud FAIL, never a silent skip.
        let r = run(vec![scored("https://gold.test/a", "wikipedia", 0.9, 0.9)]);
        let mut bands = GoldBands::default();
        bands
            .by_url
            .insert("https://gold.test/BANDED".into(), (1, 9));
        let gs = gold_set_with_urls("gold-not-banded", &["https://gold.test/a"]);

        let md = render_report(&r, &gs, &bands);
        let sec = &md[md.find("## Gold set").unwrap()..];
        assert!(
            sec.contains("**FAIL — gold/band manifest disagree."),
            "a gold-but-not-banded URL must trip a LOUD manifest FAIL:\n{sec}"
        );
        assert!(
            sec.contains(
                "gold/band manifest disagree: https://gold.test/a (has gold \
                 expected text but no word band)"
            ),
            "the offending URL + direction must be named explicitly:\n{sec}"
        );
    }

    #[test]
    fn gold_band_manifest_agree_does_not_trip_the_cross_check() {
        // Identical URL key sets in both parsers ⇒ NO disagreement banner;
        // the normal per-band pass/fail table renders.
        let mut g = scored("https://gold.test/a", "wikipedia", 0.95, 0.9);
        g.word_counts.crate_wc = Some(5); // within 1..9
        let r = run(vec![g]);
        let mut bands = GoldBands::default();
        bands.by_url.insert("https://gold.test/a".into(), (1, 9));
        let gs = gold_set_with_urls("agree", &["https://gold.test/a"]);

        let md = render_report(&r, &gs, &bands);
        let sec = &md[md.find("## Gold set").unwrap()..];
        assert!(
            !sec.contains("gold/band manifest disagree"),
            "matching URL sets must NOT trip the cross-check:\n{sec}"
        );
        assert!(
            sec.contains("| https://gold.test/a | 1..9 | 5 | 0.9500 | **PASS** |"),
            "the normal per-band row must still render when the sets agree:\n{sec}"
        );
    }

    // ---- #4: markdown cell injection — a `|`/newline in a URL is escaped ----

    #[test]
    fn md_cell_escapes_pipe_and_newline_so_a_url_cannot_inject_a_cell() {
        // A URL containing a literal `|` and a newline must not split the row
        // or shift columns: `|` → `\|`, newline → space. Checked on the
        // coverage table (URL is the free-text injection vector).
        let nasty = "https://e.test/a|b\nc";
        let r = run(vec![scored(nasty, "news", 0.5, 0.5)]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());

        // The raw, unescaped URL must NOT appear verbatim anywhere…
        assert!(
            !md.contains(nasty),
            "the raw `|`/newline URL must never reach the markdown verbatim:\n{md}"
        );
        // …it must appear escaped, on a single well-formed line.
        let escaped = "https://e.test/a\\|b c";
        assert!(
            md.contains(escaped),
            "URL must be escaped (| -> \\|, newline -> space):\n{md}"
        );
        // The coverage-table row for it stays a single line with the SAME
        // number of *cell delimiters* as the real header row. An escaped `\|`
        // is NOT a delimiter (markdown renders it literally inside the cell),
        // so strip `\|` before counting — counting raw `|` would wrongly treat
        // the escaped pipe as a column break (the bug this test guards is a
        // split row, not the presence of a backslash-pipe).
        let row = md
            .lines()
            .find(|l| l.contains(escaped))
            .expect("escaped URL row present");
        let header = md
            .lines()
            .find(|l| l.starts_with("| URL | shape |"))
            .expect("coverage table header present");
        let delims = |s: &str| s.replace("\\|", "").matches('|').count();
        assert_eq!(
            delims(row),
            delims(header),
            "escaped URL row must keep the coverage table's column count \
             (escaped \\| is not a delimiter):\nrow:    {row}\nheader: {header}"
        );
    }

    // ---- O8: agreement rendered with N + non-representative flag -----------

    #[test]
    fn o8_agreement_zero_samples_is_explicit_not_a_percentage() {
        // M1 floor: every agreement None ⇒ N=0 ⇒ "no agreement samples (N=0)"
        // and NEVER an "X%" population statistic.
        let r = run(vec![
            not_scored(
                "https://a.test/1",
                "news",
                NotScoredReason::CrateNotImplemented,
            ),
            not_scored(
                "https://a.test/2",
                "news",
                NotScoredReason::CrateNotImplemented,
            ),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec = &md[md.find("## Agreement on disagreement").unwrap()
            ..md.find("## Coverage table").unwrap()];
        assert!(
            sec.contains("No agreement samples (N=0 of 2)"),
            "N=0 must be explicit:\n{sec}"
        );
        assert!(
            !sec.contains('%'),
            "a percentage must NEVER be printed at N=0 (O8):\n{sec}"
        );
    }

    #[test]
    fn o8_agreement_below_threshold_is_flagged_non_representative_with_n() {
        // A handful of disagreement samples (< AGREEMENT_MIN_SAMPLES) must be
        // flagged non-representative AND every line carries (N=k of m) — never
        // a bare "crate sides with Trafilatura X%".
        let mk = |url: &str, a: Agreement| {
            let mut u = scored(url, "news", 0.3, 0.3);
            u.agreement = Some(a);
            u
        };
        let r = run(vec![
            mk("https://a.test/1", Agreement::CloserToTrafilatura),
            mk("https://a.test/2", Agreement::CloserToTrafilatura),
            mk("https://a.test/3", Agreement::CloserToReadability),
        ]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec = &md[md.find("## Agreement on disagreement").unwrap()
            ..md.find("## Coverage table").unwrap()];
        assert!(
            sec.contains("Non-representative: N=3"),
            "below-threshold N must be flagged non-representative:\n{sec}"
        );
        // Every distribution line is paired with (N=k of m) (O8).
        assert!(sec.contains("closer to Trafilatura: 2") && sec.contains("(N=2 of 3)"));
        assert!(sec.contains("closer to Readability: 1") && sec.contains("(N=1 of 3)"));
        assert!(
            sec.contains("agreement samples: N=3 of 3 corpus URLs"),
            "the sample size must be stated as N=k of m:\n{sec}"
        );
    }

    #[test]
    fn o8_agreement_at_or_above_threshold_not_flagged_but_still_has_n() {
        // >= AGREEMENT_MIN_SAMPLES (10) ⇒ NOT flagged non-representative, but
        // still always carries its N (never a bare percentage).
        let urls: Vec<UrlRecord> = (0..AGREEMENT_MIN_SAMPLES)
            .map(|i| {
                let mut u = scored(&format!("https://a.test/{i}"), "news", 0.3, 0.3);
                u.agreement = Some(Agreement::CloserToTrafilatura);
                u
            })
            .collect();
        let r = run(urls);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let sec = &md[md.find("## Agreement on disagreement").unwrap()
            ..md.find("## Coverage table").unwrap()];
        assert!(
            !sec.contains("Non-representative"),
            "N >= threshold must not be flagged non-representative:\n{sec}"
        );
        assert!(
            sec.contains(&format!(
                "agreement samples: N={AGREEMENT_MIN_SAMPLES} of {AGREEMENT_MIN_SAMPLES}"
            )),
            "N must still be stated even when representative:\n{sec}"
        );
    }

    // ---- Determinism + well-formed markdown --------------------------------

    #[test]
    fn report_is_deterministic_for_a_fixed_run_results() {
        // Mixed Scored / NotScored / agreement / guardrail, several shapes —
        // rendering twice must be byte-identical (no HashMap iteration; sorts
        // + BTreeMaps + manifest order only).
        let mut g = scored("https://m.test/g", "news", 0.4, 0.5);
        g.guardrail_flag = true;
        let mut d = scored("https://m.test/d", "tech_blog", 0.3, 0.3);
        d.agreement = Some(Agreement::CloserToReadability);
        let r = run(vec![
            scored("https://m.test/a", "wikipedia", 0.9, 0.9),
            not_scored(
                "https://m.test/b",
                "hub_index",
                NotScoredReason::ReferenceEmpty,
            ),
            g,
            d,
            scored("https://m.test/c", "wikipedia", 0.7, 0.6),
        ]);
        let a = render_report(&r, &GoldSet::default(), &GoldBands::default());
        let b = render_report(&r, &GoldSet::default(), &GoldBands::default());
        assert_eq!(
            a, b,
            "render_report must be deterministic for a fixed input"
        );
    }

    #[test]
    fn markdown_is_well_formed_all_sections_present() {
        let r = run(vec![scored("https://a.test/1", "news", 0.5, 0.5)]);
        let md = render_report(&r, &GoldSet::default(), &GoldBands::default());
        for h in [
            "# mdrcel differential test report",
            "## Summary",
            "### Status counts",
            "### Means (overall)",
            "### Means (per shape_class)",
            "## Agreement on disagreement",
            "## Coverage table",
            "## Disagreements ranked by severity",
            "## Guardrail-flagged URLs",
            "## Gold set",
        ] {
            assert!(md.contains(h), "missing required section/heading: {h:?}");
        }
        // Table header + separator rows are present and pipe-delimited.
        assert!(md.contains("| URL | shape |"));
        assert!(md.contains("|---|---|---|---|---|---|---|---|---|---|"));
        // No accidental empty mean value (would read as a blank "value").
        assert!(!md.contains("mean Coverage: \n"));
    }

    // ---- GoldBands::load ---------------------------------------------------

    #[test]
    fn gold_bands_load_absent_is_empty_ok_not_error() {
        let dir = std::env::temp_dir().join("mdrcel-goldbands-absent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // No gold/ dir at all ⇒ Ok(empty), mirroring score::GoldSet::load.
        let g = GoldBands::load(&dir).expect("absent gold ⇒ Ok(empty)");
        assert!(g.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn gold_bands_load_parses_min_max_and_rejects_malformed() {
        let dir = std::env::temp_dir().join("mdrcel-goldbands-parse");
        let _ = std::fs::remove_dir_all(&dir);
        let gold_dir = dir.join("gold");
        std::fs::create_dir_all(&gold_dir).unwrap();
        // Valid: 6 columns, numeric band.
        std::fs::write(
            gold_dir.join("gold.tsv"),
            "# gold\nhttps://g.test/a\tf.html\ta.txt\t2500\t3000\twhy\n",
        )
        .unwrap();
        let g = GoldBands::load(&dir).expect("valid gold.tsv");
        assert_eq!(
            g.by_url.get("https://g.test/a").copied(),
            Some((2500, 3000))
        );

        // Inverted band ⇒ hard error (must fail loudly, not silently skip).
        std::fs::write(
            gold_dir.join("gold.tsv"),
            "https://g.test/a\tf.html\ta.txt\t3000\t2500\twhy\n",
        )
        .unwrap();
        assert!(matches!(
            GoldBands::load(&dir),
            Err(GoldBandError::InvertedBand { .. })
        ));

        // Non-numeric band ⇒ hard error.
        std::fs::write(
            gold_dir.join("gold.tsv"),
            "https://g.test/a\tf.html\ta.txt\tlots\t3000\twhy\n",
        )
        .unwrap();
        assert!(matches!(
            GoldBands::load(&dir),
            Err(GoldBandError::NonNumericBand { .. })
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
