//! Stage 2a engine-gap survey (HLD M3 §7).
//!
//! For every XPath expression vendored in
//! `mdrcel::trafilatura::xpaths_constants`, this test attempts to evaluate the
//! expression through the Stage 0b engine
//! (`mdrcel::trafilatura::xpath_engine::evaluate`) against a trivial DOM. The
//! engine has three possible outcomes per expression:
//!
//! 1. **Accept (Ok)** — the expression parsed AND evaluated cleanly. The
//!    result may be an empty node-set (no match against the trivial DOM); that
//!    is fine. Parse + eval surface is the only thing this test gates.
//!
//! 2. **Reject (Err)** — `evaluate` returned `XPathError::Parse(...)` or
//!    `XPathError::Unsupported(...)`. This is the gap signal Stage 2b consumes
//!    — the supervisor reads the failing list, scopes the engine extension,
//!    and we revisit each entry.
//!
//! 3. **Panic** — defensive only. The Stage 0b engine is Err-only by contract
//!    (no `panic!` in the parse or eval paths under the supported catalog),
//!    but the test wraps every call in `catch_unwind` so a regression doesn't
//!    crash the entire test binary; a panic is treated identically to an Err
//!    for survey purposes.
//!
//! ## Always passes
//!
//! This integration test ALWAYS passes — it is a **survey**, not a gate. The
//! whole point is to land Stage 2a as a working snapshot even with engine
//! gaps. The test's only job is to:
//!
//! - Run every vendored XPath through the engine,
//! - Collect accept/reject counts,
//! - Print a summary,
//! - Write a Markdown report to
//!   `wrk_journals/2026.05.19 - JRN - xpath_engine_gap_survey.md` for the
//!   supervisor to read without re-running tests.
//!
//! The "no XPath was silently rewritten to make it pass" anti-inversion
//! guarantee lives in `xpaths_constants.rs` (the vendored literals are
//! byte-equivalent to the Python source) — this test exercises THOSE
//! literals, so a green pass here against an honest gap list IS the
//! attestation.

use std::fs;
use std::panic;
use std::path::PathBuf;

use mdrcel::readability::dom::Dom;
use mdrcel::trafilatura::xpath_engine;
use mdrcel::trafilatura::xpaths_constants::ALL_XPATHS;

/// One row of the survey: every vendored XPath gets one of these.
struct SurveyRow {
    constant_name: &'static str,
    source_range: &'static str,
    /// 0-based index into the constant's `&[&str]`.
    entry_index: usize,
    xpath: &'static str,
    /// `None` = engine accepted; `Some(reason)` = engine rejected with reason
    /// (XPathError display, or panic message string).
    rejection_reason: Option<String>,
}

/// Run the engine against `xpath` with a trivial body context. Treats panic as
/// rejection (defensively — the Stage 0b engine is Err-only by contract, but
/// we don't want a regression to crash the test binary).
fn try_engine(xpath: &str, body: &mdrcel::readability::dom::NodeRef) -> Option<String> {
    let body_clone = body.clone();
    let xpath_owned: String = xpath.to_string();
    let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
        xpath_engine::evaluate(&xpath_owned, &body_clone)
    }));
    match result {
        Ok(Ok(_nodes)) => None,
        Ok(Err(e)) => Some(format!("XPathError::{e}")),
        Err(panic_payload) => {
            let msg = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<unknown panic payload>".to_string()
            };
            Some(format!("panic: {msg}"))
        }
    }
}

#[test]
fn xpath_engine_gap_survey() {
    // Trivial DOM — we only care about parse/evaluate surface, not match
    // counts. `<html><body></body></html>` exercises the engine's path
    // walker against an empty body context.
    let dom = Dom::parse("<html><body></body></html>");
    let body = dom.body().expect("trivial DOM should always have a body");

    // Run every vendored XPath through the engine, collecting rows.
    let mut rows: Vec<SurveyRow> = Vec::new();
    for (constant_name, source_range, exprs) in ALL_XPATHS {
        for (entry_index, expr) in exprs.iter().enumerate() {
            let rejection_reason = try_engine(expr, &body);
            rows.push(SurveyRow {
                constant_name,
                source_range,
                entry_index,
                xpath: expr,
                rejection_reason,
            });
        }
    }

    // Partition into accept / reject sets.
    let total = rows.len();
    let failing: Vec<&SurveyRow> = rows
        .iter()
        .filter(|r| r.rejection_reason.is_some())
        .collect();
    let passing_count = total - failing.len();

    // ----- stdout summary ----------------------------------------------
    println!();
    println!("===== Stage 2a XPath engine-gap survey =====");
    println!(
        "{} / {} XPaths accepted by engine ({} rejected)",
        passing_count,
        total,
        failing.len(),
    );
    if !failing.is_empty() {
        println!();
        println!("Engine REJECTED the following {} XPaths:", failing.len());
        for row in &failing {
            println!(
                "  - {}[{}] ({}): {}",
                row.constant_name,
                row.entry_index,
                row.source_range,
                row.rejection_reason.as_deref().unwrap_or("<unknown>"),
            );
        }
        println!();
    }

    // ----- write the journal report ------------------------------------
    //
    // The supervisor reads this without re-running the test, so the journal
    // file is the durable artefact of the survey. Path is anchored on
    // `CARGO_MANIFEST_DIR` so the test is hermetic relative to the crate
    // root regardless of where `cargo test` is invoked from.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let journal_path: PathBuf = PathBuf::from(manifest_dir)
        .join("wrk_journals")
        .join("2026.05.19 - JRN - xpath_engine_gap_survey.md");

    let report = render_report(passing_count, total, &failing, &rows);
    fs::write(&journal_path, report)
        .unwrap_or_else(|e| panic!("failed to write journal {}: {e}", journal_path.display()));
    println!("Wrote survey to {}", journal_path.display());

    // Sanity check — the survey must have run against every vendored XPath.
    // (Anti-inversion: a future regression that empties one of the constants
    // would otherwise silently produce a zero-row survey.)
    assert!(
        total > 0,
        "survey saw zero XPaths — vendored constants empty?"
    );
}

/// Render the survey as a Markdown report. The format is:
///
/// - Top-level summary (counts).
/// - Per-rejection table: `constant_name | entry | source | reason | xpath`.
/// - Full vendored list (status per row).
fn render_report(
    passing_count: usize,
    total: usize,
    failing: &[&SurveyRow],
    all_rows: &[SurveyRow],
) -> String {
    let mut out = String::new();
    out.push_str("# JRN — Stage 2a XPath engine gap survey\n\n");
    out.push_str(
        "Generated by `tests/xpath_constants_engine_coverage.rs` against the Stage 0b XPath engine\n",
    );
    out.push_str("(`src/trafilatura/xpath_engine.rs`) and the vendored constants from\n");
    out.push_str("`src/trafilatura/xpaths_constants.rs`.\n\n");
    out.push_str(
        "Engine entry point: `xpath_engine::evaluate(xpath, &body)` against the trivial DOM\n",
    );
    out.push_str(
        "`<html><body></body></html>`. **Empty-match-set is an ACCEPT**; only parse/unsupported\n",
    );
    out.push_str("errors count as rejections.\n\n");
    out.push_str("## Summary\n\n");
    out.push_str(&format!(
        "- **{passing_count} / {total}** XPaths accepted by the Stage 0b engine.\n"
    ));
    out.push_str(&format!(
        "- **{}** XPaths rejected (require Stage 2b engine extension or a Stage 0b-equivalent rewrite).\n\n",
        failing.len()
    ));

    if failing.is_empty() {
        out.push_str(
            "The engine accepts every vendored XPath verbatim — Stage 2b has no surface to\n",
        );
        out.push_str("extend. (Unexpected but green.)\n\n");
    } else {
        out.push_str("## Engine gaps (the input for Stage 2b)\n\n");
        out.push_str("Each row below is an XPath the engine could NOT parse or evaluate.\n");
        out.push_str(
            "The supervisor scopes Stage 2b by reading these reasons and deciding per row whether\n",
        );
        out.push_str(
            "to extend the engine (preferred — keeps the XPath byte-equivalent to the Python source)\n",
        );
        out.push_str(
            "or rewrite the XPath to a Stage 0b-equivalent expression (only if the engine extension\n",
        );
        out.push_str("is disproportionate; the rewrite must preserve lxml semantics exactly).\n\n");
        for row in failing {
            out.push_str(&format!(
                "### `{}[{}]` ({})\n\n",
                row.constant_name, row.entry_index, row.source_range
            ));
            out.push_str(&format!(
                "**Engine rejection:** `{}`\n\n",
                row.rejection_reason.as_deref().unwrap_or("<unknown>")
            ));
            out.push_str("**XPath verbatim:**\n\n");
            out.push_str("```xpath\n");
            out.push_str(row.xpath);
            if !row.xpath.ends_with('\n') {
                out.push('\n');
            }
            out.push_str("```\n\n");
        }
    }

    out.push_str("## Full vendored-XPath status table\n\n");
    out.push_str("| Constant | Entry | Source | Status |\n");
    out.push_str("|----------|-------|--------|--------|\n");
    for row in all_rows {
        let status = match &row.rejection_reason {
            None => "ACCEPT".to_string(),
            Some(r) => format!("REJECT: {}", r.replace('|', "\\|").replace('\n', " ")),
        };
        out.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            row.constant_name, row.entry_index, row.source_range, status
        ));
    }
    out.push('\n');
    out
}
