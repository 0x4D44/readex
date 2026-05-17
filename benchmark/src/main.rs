//! `benchmark` — the mdrcel differential test harness CLI entrypoint.
//!
//! Stage 6 (hierarchy scoring + `results.json`). Subcommand dispatch:
//!
//! * `cargo run -p benchmark` (no subcommand) — load + validate the corpus
//!   manifest **and** assert every snapshot exists (`corpus::load_checked` —
//!   the Bug-E2 backstop). Absent manifest **or** zero entries ⇒ prints
//!   exactly `no corpus` (preserves the Stage-1 contract); a row pointing at
//!   an absent snapshot is a hard error, not laundered into success.
//!   Otherwise it **runs the differential pass**: for every URL it spawns both
//!   oracle adapters (`oracle::run_oracle`), calls the crate in-process
//!   (`crate_run::run_crate`), applies the anti-Bug-E2 status gate
//!   (`score::score_url`), writes `runs/<UTC-ts>/results.json`
//!   (`score::write_results` — gitignored scratch, HLD §4.1/§9), and prints a
//!   one-line per-status summary. At M1 the adapters do not exist yet, so
//!   every URL is `crate=not_implemented`, both oracles `oracle_error`
//!   (a spawn-fail recorded **honestly**), and every score `NotScored` —
//!   nothing laundered into a passing number (the M1 floor). The Stage-7
//!   report is generated *from* `results.json` and is deliberately not here.
//! * `cargo run -p benchmark -- fetch <url>` — one-shot, **out-of-band**
//!   snapshot capture (HLD §6). Shells out to the system `curl` to GET the URL
//!   straight into the content-addressed snapshot path, then echoes a
//!   ready-to-paste `urls.tsv` row. This is the ONLY path that performs
//!   network I/O or writes into `corpus/`; the no-subcommand scoring path
//!   never reaches it and links no HTTP stack at all.
//!
//! The corpus directory is a fixed convention (HLD §10 — no config / env /
//! flags): `benchmark/corpus/`, resolved relative to this crate's manifest
//! dir so it is independent of the process working directory.

mod corpus;
mod crate_run;
mod metrics;
mod oracle;
mod regression;
mod report;
mod score;

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

/// The corpus directory, resolved at compile time relative to this crate.
///
/// `CARGO_MANIFEST_DIR` is `benchmark/`, so the corpus is always
/// `benchmark/corpus/` regardless of the process working directory. Fixed
/// convention, not configuration (HLD §10).
fn corpus_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

fn main() -> ExitCode {
    // args[0] is the binary; args[1] (if any) is the subcommand.
    let args: Vec<String> = std::env::args().collect();

    match args.get(1).map(String::as_str) {
        None => run_no_subcommand(),
        Some("fetch") => match args.get(2) {
            Some(url) => run_fetch(url),
            None => {
                eprintln!("usage: cargo run -p benchmark -- fetch <url>");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!(
                "unknown subcommand {other:?}\n\
                 usage:\n  \
                 cargo run -p benchmark              # load corpus / summary\n  \
                 cargo run -p benchmark -- fetch <url>   # capture a snapshot"
            );
            ExitCode::FAILURE
        }
    }
}

/// No-subcommand path: load the corpus and **run the differential pass**.
///
/// Preserves the Stage-1 contract exactly: an absent manifest or a manifest
/// with zero entries prints `no corpus`. Otherwise it scores every URL
/// (`score::score_corpus` — spawns both oracles, calls the crate in-process,
/// applies the anti-Bug-E2 status gate), writes `runs/<UTC-ts>/results.json`
/// (gitignored scratch, HLD §4.1/§9), and prints a one-line per-status
/// summary. Never touches the network — `fetch` is the only path that does;
/// the scoring path only spawns the (local) oracle adapters and reads
/// committed snapshots.
///
/// At M1 the oracle adapters do not exist yet, so every URL is
/// `crate=not_implemented`, both oracles `oracle_error` (a spawn-fail recorded
/// honestly), and every score `NotScored` — the documented M1 floor with
/// nothing laundered into a passing number. The summary line is built from the
/// recorded statuses, so the floor is visible at a glance.
fn run_no_subcommand() -> ExitCode {
    let dir = corpus_dir();
    let manifest = dir.join("urls.tsv");
    // `load_checked` (not `load`): a manifest row pointing at an absent
    // snapshot must fail loudly here, BEFORE any scoring — the Bug-E2
    // backstop. An absent *manifest* is still Ok(empty) ⇒ `no corpus`.
    let entries = match corpus::load_checked(&manifest, &dir) {
        Ok(entries) if entries.is_empty() => {
            // Stage-1 contract: absent OR zero entries ⇒ exactly this string.
            println!("no corpus");
            return ExitCode::SUCCESS;
        }
        Ok(entries) => entries,
        Err(e) => {
            // A malformed/inconsistent manifest is a hard, loud failure (not
            // silently treated as "no corpus"): the Bug-E2 lesson — a broken
            // input must never be laundered into a passing/empty state.
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Gold set (HLD §7): an absent gold/ dir ⇒ empty set (the M1 / pre-freeze
    // state). A *present* gold.tsv referencing a missing expected-text file is
    // a hard error — a broken gold promise must fail loudly (Bug-E2 backstop),
    // never silently demote a gold URL to the Trafilatura reference.
    let gold = match score::GoldSet::load(&dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Gold word-bands (HLD §7 `min_words`/`max_words`) — a *report-layer*
    // input (the §9 gold pass/fail), not scoring data, so it is loaded
    // separately from `GoldSet` (which models only the URL→text the hierarchy
    // needs). Same absent-is-empty contract; a malformed band fails loudly.
    let gold_bands = match report::GoldBands::load(&dir) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Run the differential pass and persist results.json (the single source
    // of truth for the run; the Stage-7 report is generated from it).
    //
    // Fail-closed on host provenance (HLD §2.9): if no real host identity can
    // be determined the run is unscorable — print a clear message, exit
    // non-zero, and write NO results.json. A run with no provenance must never
    // produce a poisoned baseline candidate (see `score::HostDetectionFailed`).
    let results = match score::score_corpus(&entries, &dir, &gold) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runs_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("runs");
    match score::write_results(&results, &runs_root) {
        Ok(path) => {
            // One-line per-status summary (HLD §9). The M1 floor is visible
            // here: crate=not_implemented:N, oracles oracle_error:N — and
            // every score NotScored (nothing laundered).
            let scored = results
                .urls
                .iter()
                .filter(|u| matches!(u.score, score::ScoreOutcome::Scored { .. }))
                .count();
            // "no gold (M1/pre-freeze)" vs "N gold" — `is_empty` distinguishes
            // the deliberate pre-freeze state (HLD §2.7/§7) from a curated set.
            let gold_summary = if gold.is_empty() {
                "no gold (pre-freeze)".to_string()
            } else {
                format!("{} gold", gold.len())
            };

            // Generate report.md *from the same in-memory RunResults* (the
            // single source of truth — never reparse results.json) into the
            // SAME runs/<ts>/ directory results.json was just written to
            // (HLD §9). `write_results` returns the results.json path, so its
            // parent IS the unique per-run directory. The host-detection
            // failure path short-circuits BEFORE write_results, so a poisoned
            // run produces neither results.json NOR report.md (contract
            // preserved). report generation is pure given the RunResults +
            // the gold word-bands; only the final write touches disk.
            let run_dir = path.parent().unwrap_or(&runs_root);
            // Pass the authoritative `GoldSet` (the URL→gold-text hierarchy
            // input) alongside the word-bands so the report can cross-check
            // the two independent `gold.tsv` parsers (a count-preserving
            // column swap desyncs them silently — HLD §7/§2.7). Both are
            // already loaded above; nothing is re-parsed.
            let markdown = report::render_report(&results, &gold, &gold_bands);
            let report_path = match report::write_report(&markdown, run_dir) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: writing report.md: {e}");
                    return ExitCode::FAILURE;
                }
            };

            // Stage 8 (HLD §9 + §2.9): regression-gate against the committed
            // baseline. This prepends the REGRESSIONS / ADVISORY / skipped
            // block to the TOP of report.md ON DISK (so a human/CI reads the
            // verdict first) and returns the process exit code: non-zero ONLY
            // for a gated (same-canonical-host) regression; advisory /
            // no-baseline / clean ⇒ 0. A malformed baseline is a LOUD distinct
            // error (Bug-E2 — a broken baseline must NEVER be silently treated
            // as "no regressions"), non-zero, with NO laundering.
            let gate_exit = run_regression_gate(&results, &report_path);

            println!(
                "scored {} urls ({}) | crate {} | trafilatura {} | \
                 readability {} | trusted-scores {} | results: {} | report: {}",
                results.corpus_size,
                gold_summary,
                fmt_counts(&results.status_counts.crate_status),
                fmt_counts(&results.status_counts.trafilatura_status),
                fmt_counts(&results.status_counts.readability_status),
                scored,
                path.display(),
                report_path.display()
            );
            gate_exit
        }
        Err(e) => {
            eprintln!("error: writing results.json: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Render a per-status count map as a compact `a:1, b:2` string for the
/// one-line run summary (HLD §9). Deterministic order (`BTreeMap`).
fn fmt_counts(counts: &std::collections::BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "-".to_string();
    }
    counts
        .iter()
        .map(|(k, v)| format!("{k}:{v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The committed baseline `results.json` (HLD §9 / §4.1).
///
/// `benchmark/baseline/results.json` is the **only** persisted run state in
/// git (distinct from the gitignored `benchmark/runs/`). The harness only ever
/// **reads** it; updating it is a deliberate manual `cp` + commit (documented
/// in `benchmark/README.md`) — there is deliberately **no** baseline-writer
/// here (HLD §9: "No tooling, no migration — a file copy under version control
/// is the entire mechanism"). Fixed convention, not configuration (HLD §10).
fn baseline_results_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("baseline")
        .join("results.json")
}

/// Stage-8 regression gate (HLD §9 + §2.9). Compares `current` against the
/// committed baseline and **prepends** the verdict block to the TOP of
/// `report.md` on disk (so a human / CI sees it before the summary). Returns
/// the process exit code.
///
/// Exit semantics (HLD §9):
/// * **No baseline committed** (e.g. first ever run) ⇒ prepend an explicit
///   *skipped (baseline candidate)* line — never silence, never a false
///   "no regressions" — and exit **0** (this run is a baseline candidate).
/// * **Malformed / unreadable baseline** ⇒ a **loud, distinct** error and exit
///   **non-zero** (Bug-E2: a broken baseline must NOT be silently treated as
///   "no regressions"). The report is left as written (no laundered block).
/// * **Valid baseline** ⇒ `regression::compare`; prepend the
///   `REGRESSIONS` / `BASELINE ADVISORY` block; exit **non-zero ONLY** for a
///   gated (same-canonical-host) regression — advisory / clean ⇒ **0**.
///
/// A failure to prepend the block to `report.md` is itself a loud non-zero
/// error: the report missing its gate verdict must not pass silently.
///
/// Thin orchestrator: it resolves the **fixed** committed-baseline path and
/// delegates the decision to the pure-seam [`regression_gate_outcome`], then
/// maps the structured [`GateOutcome`] to the process [`ExitCode`] + the
/// human-facing stderr line. The seam (path injected) is what makes every
/// branch — absent / unreadable / malformed / gating / advisory — unit-testable
/// without touching the real `benchmark/baseline/` location (mirrors the
/// established `score::score_corpus_with_host` testable-seam convention).
fn run_regression_gate(current: &score::RunResults, report_path: &std::path::Path) -> ExitCode {
    let baseline_path = baseline_results_path();
    match regression_gate_outcome(&baseline_path, current, report_path) {
        GateOutcome::NoBaseline => {
            // Honest: the check was SKIPPED (no baseline yet), not passed.
            eprintln!(
                "no baseline committed ({}); regression check skipped — this \
                 run is a baseline candidate (see benchmark/README.md to \
                 promote it).",
                baseline_path.display()
            );
            ExitCode::SUCCESS
        }
        GateOutcome::Clean { host, vacuous } => {
            if vacuous {
                // #4b: honest — correctly not a regression (exit 0) but the
                // gate compared NO trusted numbers (every URL not_scored on
                // both sides — the M1 floor). It must NOT read as a real pass.
                eprintln!(
                    "no regressions vs the committed baseline on host `{host}`, \
                     but VACUOUS — every URL is not_scored on both the baseline \
                     and this run (the Milestone-1 floor); this gate compared \
                     no trusted numbers and is NOT a substantive pass."
                );
            } else {
                eprintln!(
                    "no regressions vs the committed baseline on host `{host}` \
                     (clean — ≥1 trusted score compared)."
                );
            }
            ExitCode::SUCCESS
        }
        GateOutcome::ReferenceLostNonGating { host, count } => {
            // #2c: honest — exit 0 (the crate is not implicated) but NOT a
            // clean pass; the reference/oracle moved under us.
            eprintln!(
                "no crate regressions vs the committed baseline on host \
                 `{host}`, but {count} reference/oracle loss(es) listed in \
                 report.md (NOT crate regressions — the comparison basis \
                 changed under us; CI exits 0). Re-bless the baseline if the \
                 reference environment legitimately changed (see \
                 benchmark/README.md)."
            );
            ExitCode::SUCCESS
        }
        GateOutcome::Advisory {
            current_host,
            baseline_host,
            count,
        } => {
            eprintln!(
                "BASELINE ADVISORY: ran on `{current_host}`, baseline from \
                 `{baseline_host}` — {count} difference(s) listed in \
                 report.md, NOT regression-gating (HLD §2.9)."
            );
            ExitCode::SUCCESS
        }
        GateOutcome::Gated { count } => {
            eprintln!(
                "REGRESSIONS: {count} offending URL(s) on the declared host — \
                 regression-gating, exiting non-zero (see report.md)."
            );
            ExitCode::FAILURE
        }
        GateOutcome::BaselineBroken { message } => {
            // Loud, distinct, non-zero. A broken baseline is NEVER laundered
            // into "no regressions" (Bug-E2).
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
        GateOutcome::ReportWriteFailed { message } => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The structured result of the regression gate — the **testable** outcome of
/// [`regression_gate_outcome`], decoupled from `ExitCode` / stderr so every
/// branch is unit-asserted directly. `BaselineBroken` is a single loud
/// non-zero outcome covering *unreadable* and *malformed* (both Bug-E2: a
/// broken baseline must never read as "no regressions"); the carried message
/// names which.
#[derive(Debug, PartialEq)]
enum GateOutcome {
    /// No baseline committed (NotFound). Exit 0; the skipped line is prepended.
    NoBaseline,
    /// Same canonical host, zero regressions. Exit 0. `vacuous` is `true` when
    /// every URL was `not_scored` on **both** the baseline and this run (the
    /// M1 floor) — the gate compared no trusted numbers, so it is correctly
    /// not a regression but is **NOT a substantive pass** (#4b); the stderr
    /// line says so explicitly (mirrors the honest no-baseline line).
    Clean { host: String, vacuous: bool },
    /// Same canonical host, ≥1 offender, but **every** offender is non-gating
    /// ([`regression::RegressionKind::ReferenceLost`] — the oracle/reference
    /// changed under us, #2c). Exit **0** (the crate is not implicated; never
    /// laundered into a crate red-CI), but it is **NOT** "clean": the offenders
    /// are still listed in `report.md` as signal, and stderr says so honestly
    /// (re-bless the baseline if the reference environment legitimately
    /// changed).
    ReferenceLostNonGating { host: String, count: usize },
    /// Different host (or unparseable baseline host) — advisory only, exit 0
    /// even with differences (HLD §2.9).
    Advisory {
        current_host: String,
        baseline_host: String,
        count: usize,
    },
    /// Same canonical host **and** ≥1 regression — regression-gating, exit
    /// non-zero (HLD §9).
    Gated { count: usize },
    /// The baseline exists but is unreadable or malformed — loud, non-zero,
    /// NOT laundered into a pass (Bug-E2).
    BaselineBroken { message: String },
    /// The verdict block could not be prepended to `report.md` — loud,
    /// non-zero (the report missing its verdict must not pass silently).
    ReportWriteFailed { message: String },
}

/// Pure-seam regression gate (HLD §9 + §2.9) — the testable core of
/// [`run_regression_gate`] with the baseline path **injected** so every branch
/// is exercisable without touching the fixed `benchmark/baseline/` location.
///
/// Side effect (the one this stage owns): on every non-`BaselineBroken`
/// outcome it **prepends** the appropriate block to `report_path` so the gate
/// verdict is the first thing in `report.md`. A prepend failure is its own
/// loud `ReportWriteFailed` outcome (the report missing its verdict must not
/// pass silently). `BaselineBroken` deliberately does **not** prepend — there
/// is no trustworthy verdict to write, and the report is left as the report
/// layer wrote it (no laundered block).
fn regression_gate_outcome(
    baseline_path: &std::path::Path,
    current: &score::RunResults,
    report_path: &std::path::Path,
) -> GateOutcome {
    // Absent baseline ⇒ NOT an error: the honest skipped block, exit 0. Any
    // OTHER read error (permission, not-a-file, …) is a real, loud failure —
    // a baseline that exists but cannot be read must never be laundered into
    // "no regressions" (Bug-E2).
    let raw = match fs::read_to_string(baseline_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Err(pe) = prepend_to_report(report_path, &regression::no_baseline_block()) {
                return GateOutcome::ReportWriteFailed {
                    message: format!("prepending no-baseline notice to report.md: {pe}"),
                };
            }
            return GateOutcome::NoBaseline;
        }
        Err(e) => {
            return GateOutcome::BaselineBroken {
                message: format!(
                    "the committed baseline {} exists but could not be read: \
                     {e}. A broken baseline must fail loudly — it is NOT \
                     silently treated as 'no regressions' (Bug-E2).",
                    baseline_path.display()
                ),
            };
        }
    };

    // Malformed JSON / wrong shape ⇒ loud, distinct, non-zero. A baseline that
    // does not deserialize to RunResults must NEVER be laundered into a pass.
    let baseline: score::RunResults = match serde_json::from_str(&raw) {
        Ok(b) => b,
        Err(e) => {
            return GateOutcome::BaselineBroken {
                message: format!(
                    "the committed baseline {} is malformed (not a valid \
                     results.json / RunResults): {e}. A broken baseline must \
                     fail loudly and is NOT treated as 'no regressions' \
                     (Bug-E2). Fix or re-capture the baseline (see \
                     benchmark/README.md).",
                    baseline_path.display()
                ),
            };
        }
    };

    // Pure comparator (HLD §9). Timestamp ignored; per-URL keyed by url+host;
    // host-pin gate decides gating vs advisory (HLD §2.9).
    let cmp = regression::compare(&baseline, current);
    let block = regression::render_block(&cmp);
    if let Err(e) = prepend_to_report(report_path, &block) {
        return GateOutcome::ReportWriteFailed {
            message: format!("prepending REGRESSIONS block to report.md: {e}"),
        };
    }

    if cmp.should_fail() {
        GateOutcome::Gated {
            count: cmp.regressions.len(),
        }
    } else {
        match cmp.gate {
            regression::Gate::Advisory => GateOutcome::Advisory {
                current_host: cmp.current_host.clone(),
                baseline_host: cmp
                    .baseline_host
                    .clone()
                    .unwrap_or_else(|| "<absent/unparseable>".to_string()),
                count: cmp.regressions.len(),
            },
            regression::Gate::Gating => {
                if cmp.regressions.is_empty() {
                    GateOutcome::Clean {
                        host: cmp.current_host.clone(),
                        // #4b: a gating run with zero offenders is only a
                        // *substantive* pass if a trusted number was actually
                        // compared. If every URL was not_scored on both sides
                        // (the M1 floor) it is VACUOUS — surfaced honestly,
                        // not as a real pass.
                        vacuous: cmp.is_vacuous_clean(),
                    }
                } else {
                    // !should_fail() yet offenders present on the gating host
                    // ⇒ every offender is non-gating (#2c — `ReferenceLost`):
                    // the oracle/reference moved under us. Exit 0, but NOT
                    // "clean" — the offenders are real listed signal.
                    GateOutcome::ReferenceLostNonGating {
                        host: cmp.current_host.clone(),
                        count: cmp.regressions.len(),
                    }
                }
            }
        }
    }
}

/// Prepend `block` to the **front** of `report.md` (HLD §9 — the gate verdict
/// must be the first thing a human / CI sees, before the report header). Reads
/// the just-written report, writes `block` followed by the original content.
/// A single read+write; the report is small and this is the final step.
///
/// **Intentionally NON-atomic (#5 — no atomic-rename machinery, premature per
/// HLD §3).** This operates only on the **disposable, gitignored per-run
/// scratch** `runs/<ts>/report.md` — regenerated *wholesale* from
/// `results.json` on the next run, and **never** the committed
/// `baseline/results.json` (the gate only ever reads the baseline). A torn
/// write therefore corrupts at most one throwaway scratch file, and any write
/// failure is surfaced **loudly** as a non-zero
/// [`GateOutcome::ReportWriteFailed`] by the caller — **never** swallowed. A
/// temp-file + atomic-rename would add machinery for a failure mode whose blast
/// radius is a single regenerated scratch file: deliberately not built.
fn prepend_to_report(report_path: &std::path::Path, block: &str) -> std::io::Result<()> {
    let existing = fs::read_to_string(report_path)?;
    fs::write(report_path, format!("{block}{existing}"))
}

/// `fetch <url>` — one-shot, out-of-band snapshot capture (HLD §6).
///
/// GETs `url` (via system `curl`) straight into the content-addressed snapshot
/// path under `corpus/snapshots/`, and prints a ready-to-paste `urls.tsv` row
/// (the operator fills in `shape_class` and adjusts the note).
///
/// Immutability (HLD §6): if a snapshot already exists for this URL the bytes
/// are **not** overwritten — re-fetching is a new row + new snapshot by
/// design, and a content-addressed name only collides when the URL is
/// identical, in which case the existing committed bytes are authoritative.
/// This path is unreachable from the scoring run.
fn run_fetch(url: &str) -> ExitCode {
    let filename = corpus::snapshot_filename(url);
    let snapshots = corpus_dir().join("snapshots");
    let dest = snapshots.join(&filename);

    if let Err(e) = fs::create_dir_all(&snapshots) {
        eprintln!("error: creating {}: {e}", snapshots.display());
        return ExitCode::FAILURE;
    }

    if dest.exists() {
        // Immutable (HLD §6): the snapshot is already present and committed.
        // Its bytes may have been fetched long ago, so we must NOT emit a
        // fresh manifest row stamped with today's date — that would launder a
        // stale snapshot as freshly fetched. Keep the original row's
        // `fetched_date`; nothing to add.
        eprintln!(
            "snapshot already present (immutable, HLD §6): {}",
            dest.display()
        );
        eprintln!(
            "not overwritten and no new row emitted — keep the existing \
             urls.tsv row (its original fetched_date stands)."
        );
        return ExitCode::SUCCESS;
    }

    if let Err(e) = curl_download(url, &dest) {
        // Never leave a truncated/partial snapshot behind: a half-written
        // file would later be laundered into scoring as a valid fixture.
        let _ = fs::remove_file(&dest);
        eprintln!("error: GET {url} failed: {e}");
        return ExitCode::FAILURE;
    }
    let len = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
    eprintln!("wrote {len} bytes -> {}", dest.display());

    // Ready-to-paste manifest row. shape_class is left as a placeholder for
    // the operator to fill from the closed set; today's date is stamped
    // because these bytes were genuinely fetched just now.
    let today = today_utc_date();
    eprintln!("\nadd this row to benchmark/corpus/urls.tsv (set <shape_class>):");
    println!("{url}\t<shape_class>\t{filename}\t{today}\tfetched via `fetch`");
    ExitCode::SUCCESS
}

/// One-shot HTTP GET via the system `curl`, written directly to `dest`.
///
/// Deliberately shells out instead of linking an HTTP client: `fetch` is an
/// out-of-band, developer-only step (HLD §6) that must never put an HTTP stack
/// on the scoring path (HLD §3 — minimal deps, sync, no async). Reachable ONLY
/// from `run_fetch`, i.e. only via the explicit `fetch` subcommand.
///
/// `curl` writes the body straight to `dest` (`--output`); `--fail` makes a
/// non-2xx response a non-zero exit (no error page masquerading as a
/// snapshot), `--location` follows redirects, `--max-time 60` bounds the call.
/// Returns an error if `curl` is absent (spawn fails) or exits non-zero; the
/// caller removes any partial `dest` so no truncated snapshot survives.
fn curl_download(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let status = std::process::Command::new("curl")
        .arg("--fail")
        .arg("--silent")
        .arg("--show-error")
        .arg("--location")
        .arg("--max-time")
        .arg("60")
        .arg("--output")
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("could not run `curl` (is it installed and on PATH?): {e}"))?;

    if !status.success() {
        return Err(format!("`curl` exited unsuccessfully: {status}"));
    }
    Ok(())
}

/// Today's date as `YYYY-MM-DD` (UTC), for the `fetched_date` column.
///
/// Computed from `SystemTime` with the civil-date algorithm (Howard Hinnant's
/// `days_from_civil` inverse) so no date crate is pulled in for one stamp —
/// the only consumer is a human-pasted manifest row, not scored logic.
fn today_utc_date() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64; // days since 1970-01-01 (UTC).

    // civil_from_days (Hinnant). `SystemTime::now()` is always post-epoch
    // (and the error path clamps to 0), so `days >= 0` and `z >= 719_468 > 0`
    // — the pre-epoch (`z < 0`) era adjustment is unreachable here and is
    // therefore omitted; `era = z / 146_097` directly.
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_dir_is_under_this_crate() {
        let d = corpus_dir();
        assert!(d.ends_with("corpus"));
        // Parent is the crate manifest dir (benchmark/).
        assert_eq!(
            d.parent().unwrap(),
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        );
    }

    #[test]
    fn today_utc_date_is_well_formed() {
        let s = today_utc_date();
        // YYYY-MM-DD
        let parts: Vec<&str> = s.split('-').collect();
        assert_eq!(parts.len(), 3, "date {s:?} not YYYY-MM-DD");
        assert_eq!(parts[0].len(), 4);
        assert_eq!(parts[1].len(), 2);
        assert_eq!(parts[2].len(), 2);
        let y: i64 = parts[0].parse().unwrap();
        let m: i64 = parts[1].parse().unwrap();
        let d: i64 = parts[2].parse().unwrap();
        assert!((2020..=2100).contains(&y), "year out of sane range: {y}");
        assert!((1..=12).contains(&m), "month out of range: {m}");
        assert!((1..=31).contains(&d), "day out of range: {d}");
    }

    /// Civil-date conversion spot-checks against known UNIX-epoch day counts
    /// (the algorithm is the testing oracle for the inline date math).
    #[test]
    fn civil_date_known_vectors() {
        // We can't inject time into today_utc_date(); instead re-derive the
        // pure conversion here from a fixed day count and assert known dates.
        // Mirrors production: post-epoch only (all vectors below have
        // `days >= 0`), so no pre-epoch era adjustment.
        fn civil(days: i64) -> (i64, i64, i64) {
            let z = days + 719_468;
            let era = z / 146_097;
            let doe = z - era * 146_097;
            let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
            let y = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let d = doy - (153 * mp + 2) / 5 + 1;
            let m = if mp < 10 { mp + 3 } else { mp - 9 };
            (if m <= 2 { y + 1 } else { y }, m, d)
        }
        assert_eq!(civil(0), (1970, 1, 1)); // UNIX epoch.
        assert_eq!(civil(18_628), (2021, 1, 1)); // 2021-01-01.
        assert_eq!(civil(19_723), (2024, 1, 1)); // 2024-01-01 (leap year).
        assert_eq!(civil(20_589), (2026, 5, 16)); // 2026-05-16.
    }

    // ---- Stage 8: regression-gate wiring (the testable seam) ---------------
    //
    // `regression_gate_outcome` takes the baseline path as an argument so
    // every branch is exercised against a temp dir, never the committed
    // `benchmark/baseline/` location (the established score.rs seam pattern).
    // The pure transition matrix itself is exhaustively tested in
    // `regression`'s own module; these assert the I/O wiring + exit mapping +
    // the Bug-E2 "broken baseline is loud, not laundered" contract.

    use crate::score::{
        NotScoredReason, RunResults, ScoreOutcome, StatusCounts, UrlRecord, WordCounts,
    };

    fn rec_scored(url: &str, coverage: f64, wc: usize) -> UrlRecord {
        UrlRecord {
            url: url.to_string(),
            shape_class: "news".to_string(),
            crate_status: "ok".to_string(),
            trafilatura_status: "ok".to_string(),
            readability_status: "ok".to_string(),
            status_detail: Default::default(),
            word_counts: WordCounts {
                crate_wc: Some(wc),
                trafilatura_wc: Some(wc),
                readability_wc: Some(wc),
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

    fn rec_not_scored(url: &str) -> UrlRecord {
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
            score: ScoreOutcome::NotScored {
                reason: NotScoredReason::CrateNotImplemented,
            },
            edit_sim: None,
            guardrail_flag: false,
            agreement: None,
        }
    }

    fn run(host: &str, ts: &str, urls: Vec<UrlRecord>) -> RunResults {
        RunResults {
            host: host.to_string(),
            utc_timestamp: ts.to_string(),
            corpus_size: urls.len(),
            status_counts: StatusCounts::default(),
            urls,
        }
    }

    /// A fresh temp dir + a stub `report.md` (the report layer always writes
    /// one before the gate runs). Returns `(dir, report_path, baseline_path)`.
    fn scratch(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("mdrcel-reg-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let report = dir.join("report.md");
        fs::write(&report, "# mdrcel differential test report\n\nbody\n").unwrap();
        let baseline = dir.join("results.json");
        (dir, report, baseline)
    }

    fn write_baseline(path: &std::path::Path, r: &RunResults) {
        fs::write(path, serde_json::to_string_pretty(r).unwrap()).unwrap();
    }

    #[test]
    fn gate_absent_baseline_is_skipped_not_a_false_pass_and_block_prepended() {
        // No baseline committed (the first-ever-run / current M1 state):
        // NoBaseline (exit 0), and the report gets an explicit *skipped
        // (baseline candidate)* block at the TOP — never silence, never a
        // false "no regressions" (Bug-E2).
        let (_d, report, baseline) = scratch("absent");
        // baseline path deliberately does NOT exist.
        let cur = run("anvil", "t", vec![rec_not_scored("u")]);
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(out, GateOutcome::NoBaseline);
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            body.starts_with("# REGRESSIONS"),
            "skipped notice must be prepended at the TOP: {body:?}"
        );
        assert!(
            body.contains("No baseline committed") && body.contains("baseline candidate"),
            "must be the honest skipped line, not a false pass: {body}"
        );
        // The original report content is preserved AFTER the block.
        assert!(body.contains("# mdrcel differential test report"));
    }

    #[test]
    fn gate_malformed_baseline_is_loud_not_laundered_into_no_regressions() {
        // THE Bug-E2 wiring test. A baseline that exists but does not
        // deserialize to RunResults MUST be a loud, distinct, non-zero
        // BaselineBroken — NEVER silently "no regressions".
        let (_d, report, baseline) = scratch("malformed");
        fs::write(&baseline, "{ this is not valid results json ]").unwrap();
        let cur = run("anvil", "t", vec![rec_scored("u", 0.9, 100)]);
        let out = regression_gate_outcome(&baseline, &cur, &report);
        match &out {
            GateOutcome::BaselineBroken { message } => {
                assert!(
                    message.contains("malformed") && message.contains("Bug-E2"),
                    "broken-baseline message must be loud + explain Bug-E2: {message}"
                );
            }
            other => panic!("malformed baseline must be BaselineBroken, got {other:?}"),
        }
        // run_regression_gate maps BaselineBroken → non-zero exit.
        // And the report is NOT given a laundered "clean" block.
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            !body.contains("clean") && !body.contains("No baseline committed"),
            "a broken baseline must NOT prepend a clean/skipped block: {body}"
        );
    }

    #[test]
    fn gate_host_match_with_regression_is_gated_non_zero_and_block_at_top() {
        // Same canonical host + a coverage drop ⇒ Gated (run_regression_gate
        // maps this to a NON-ZERO exit), with the REGRESSIONS block at the TOP.
        let (_d, report, baseline) = scratch("gated");
        write_baseline(
            &baseline,
            &run(
                "ANVIL.corp.local",
                "old",
                vec![rec_scored("https://x/1", 0.95, 100)],
            ),
        );
        let cur = run("anvil", "new", vec![rec_scored("https://x/1", 0.10, 100)]);
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(out, GateOutcome::Gated { count: 1 });
        let body = fs::read_to_string(&report).unwrap();
        assert!(body.starts_with("# REGRESSIONS"), "block at TOP: {body:?}");
        assert!(body.contains("FAILS") && body.contains("https://x/1"));
    }

    #[test]
    fn gate_host_mismatch_is_advisory_exit_zero_even_with_regressions() {
        // Different host ⇒ Advisory even with a blatant regression (HLD §2.9).
        // run_regression_gate maps Advisory → exit 0.
        let (_d, report, baseline) = scratch("advisory");
        write_baseline(
            &baseline,
            &run("anvil", "old", vec![rec_scored("https://x/1", 0.95, 100)]),
        );
        let cur = run("borg", "new", vec![rec_scored("https://x/1", 0.01, 1)]);
        let out = regression_gate_outcome(&baseline, &cur, &report);
        match &out {
            GateOutcome::Advisory {
                current_host,
                baseline_host,
                count,
            } => {
                assert_eq!(current_host, "borg");
                assert_eq!(baseline_host, "anvil");
                assert_eq!(*count, 1, "the delta is still listed as advisory signal");
            }
            other => panic!("host mismatch must be Advisory, got {other:?}"),
        }
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            body.starts_with("# BASELINE ADVISORY"),
            "advisory block at TOP: {body:?}"
        );
        assert!(body.contains("not regression-gating"));
    }

    #[test]
    fn gate_same_host_no_regression_is_clean_exit_zero() {
        // Same host, identical scored records, only the timestamp differs ⇒
        // Clean (exit 0), timestamp ignored, explicit clean block prepended.
        let (_d, report, baseline) = scratch("clean");
        write_baseline(
            &baseline,
            &run(
                "anvil",
                "2026-01-01T00-00-00Z",
                vec![rec_scored("u", 0.80, 100)],
            ),
        );
        let cur = run(
            "anvil",
            "2026-12-31T23-59-59Z",
            vec![rec_scored("u", 0.80, 100)],
        );
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(
            out,
            GateOutcome::Clean {
                host: "anvil".to_string(),
                // A real Scored→Scored pair ⇒ substantive, NOT vacuous (#4b).
                vacuous: false,
            }
        );
        let body = fs::read_to_string(&report).unwrap();
        assert!(body.starts_with("# REGRESSIONS") && body.contains("clean"));
        assert!(
            !body.contains("VACUOUS"),
            "a substantive clean must NOT be VACUOUS: {body}"
        );
    }

    #[test]
    fn gate_m1_floor_self_compare_is_clean_but_vacuous_exit_zero() {
        // The end-to-end M1 floor (#4b): a baseline that is itself the
        // all-NotScored floor vs the same floor now ⇒ Clean but **VACUOUS**,
        // exit 0. It is correctly NOT a regression (nothing laundered into a
        // fake delta), but it compared NO trusted numbers, so it must be
        // surfaced as NOT a substantive pass (not laundered into a fake pass
        // either) — and the prepended block says VACUOUS.
        let (_d, report, baseline) = scratch("m1floor");
        write_baseline(
            &baseline,
            &run(
                "anvil",
                "t1",
                vec![rec_not_scored("u1"), rec_not_scored("u2")],
            ),
        );
        let cur = run(
            "anvil",
            "t2",
            vec![rec_not_scored("u1"), rec_not_scored("u2")],
        );
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(
            out,
            GateOutcome::Clean {
                host: "anvil".to_string(),
                vacuous: true,
            },
            "M1 floor self-compare must be Clean{{vacuous:true}} / exit 0"
        );
        // run_regression_gate maps Clean{vacuous:true} → exit 0, and the
        // prepended block must honestly say it is NOT a substantive pass.
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            body.starts_with("# REGRESSIONS") && body.contains("VACUOUS"),
            "the M1-floor self-compare block must be prepended and say VACUOUS: {body}"
        );
        assert!(
            body.contains("NOT a substantive pass"),
            "vacuous block must say it is NOT a substantive pass: {body}"
        );
    }

    #[test]
    fn gate_substantive_clean_is_not_vacuous_exit_zero() {
        // A gating run with ≥1 Scored→Scored within-threshold pair is a
        // SUBSTANTIVE clean: Clean{vacuous:false}, exit 0, block says clean
        // and NOT vacuous.
        let (_d, report, baseline) = scratch("substantive");
        write_baseline(
            &baseline,
            &run("anvil", "t1", vec![rec_scored("https://x/1", 0.80, 100)]),
        );
        let cur = run("anvil", "t2", vec![rec_scored("https://x/1", 0.80, 100)]);
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(
            out,
            GateOutcome::Clean {
                host: "anvil".to_string(),
                vacuous: false,
            },
            "a real Scored→Scored pair ⇒ substantive Clean, NOT vacuous"
        );
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            body.contains("clean") && !body.contains("VACUOUS"),
            "substantive clean must say clean, not VACUOUS: {body}"
        );
    }

    #[test]
    fn gate_reference_loss_on_declared_host_is_non_gating_exit_zero_but_listed() {
        // #2c end-to-end: a Scored→NotScored(ReferenceUnavailable) on the
        // DECLARED host. The crate is NOT implicated ⇒ exit 0
        // (ReferenceLostNonGating, NOT Gated, NOT Clean), but the offender is
        // still LISTED in the prepended block as signal.
        let (_d, report, baseline) = scratch("reflost");
        write_baseline(
            &baseline,
            &run(
                "ANVIL.corp.local",
                "old",
                vec![rec_scored("https://x/1", 0.95, 100)],
            ),
        );
        let mut cur = run("anvil", "new", vec![rec_scored("https://x/1", 0.95, 100)]);
        // Flip the single URL to a reference/oracle loss in the current run.
        cur.urls[0].score = ScoreOutcome::NotScored {
            reason: NotScoredReason::ReferenceUnavailable,
        };
        cur.urls[0].crate_status = "ok".to_string();
        cur.urls[0].trafilatura_status = "oracle_error".to_string();
        let out = regression_gate_outcome(&baseline, &cur, &report);
        assert_eq!(
            out,
            GateOutcome::ReferenceLostNonGating {
                host: "anvil".to_string(),
                count: 1,
            },
            "a reference/oracle loss on the declared host MUST be \
             ReferenceLostNonGating (exit 0), never Gated and never Clean"
        );
        let body = fs::read_to_string(&report).unwrap();
        assert!(
            body.starts_with("# REGRESSIONS") && body.contains("https://x/1"),
            "the reference loss MUST still be listed in the block as signal: {body}"
        );
        assert!(
            body.contains("does NOT fail") && !body.contains("FAILS"),
            "the block must say it does NOT fail (not laundered into a crate \
             red-CI): {body}"
        );
    }
}
