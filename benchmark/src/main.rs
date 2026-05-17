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
            println!(
                "scored {} urls ({}) | crate {} | trafilatura {} | \
                 readability {} | trusted-scores {} | results: {}",
                results.corpus_size,
                gold_summary,
                fmt_counts(&results.status_counts.crate_status),
                fmt_counts(&results.status_counts.trafilatura_status),
                fmt_counts(&results.status_counts.readability_status),
                scored,
                path.display()
            );
            ExitCode::SUCCESS
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
}
