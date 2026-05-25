//! M3 Stage 3 — `extract_content` smoke gate.
//!
//! Stage 2d landed the central `_extract` + `extract_content` orchestrators.
//! This smoke gate runs `extract_content` against the same 10 corpus fixtures
//! the converted-tree BLOCKER gate uses, asserting:
//!
//! 1. The full pipeline does NOT panic. No `unimplemented!()` from a missing
//!    Stage 2c handler. No infinite loop. No `unwrap()` on a malformed input.
//! 2. The pipeline returns *something* — for at least 7/10 fixtures, the
//!    extracted text length must be > 200 characters (the canonical corpus
//!    is article-shaped HTML; we expect substantive extractions, not empty
//!    bodies). Allow up to 3 "low yield" cases for now — small fixtures like
//!    example.com (528 bytes) genuinely have very little extractable content.
//!
//! This is a SMOKE gate, not a BLOCKER. A failure here means the pipeline
//! has regressed on a real-world input, but it does NOT pin byte-equivalence
//! to Python (Stage 3-B will handle that with a fresh Python oracle mode).

use std::path::{Path, PathBuf};

use readex::readability::dom::Dom;
use readex::trafilatura::cleaning;
use readex::trafilatura::main_extractor;

/// Same 10 fixtures the converted-tree gate uses (HLD §6.2).
const FIXTURES: &[&str] = &[
    "benchmark/corpus/snapshots/0f115db062b7c0dd.html", // example.com (edge_case, minimal)
    "benchmark/corpus/snapshots/ae2c2184beb6d264.html", // en.wikipedia.org Apple_Inc.
    "benchmark/corpus/snapshots/9c8f49f04f792f81.html", // en.wikipedia.org Wm_Morrison
    "benchmark/corpus/snapshots/9a1590d0917107a7.html", // Apple FY2025 10-K (62 tables)
    "benchmark/corpus/snapshots/9ec7aaf8edb71ac1.html", // HMRC 2025-26 (23 tables)
    "benchmark/corpus/snapshots/803b534a50a3f584.html", // gov.uk income tax (7 tables)
    "benchmark/corpus/snapshots/f405a9e3314e15da.html", // BBC News tech (news)
    "benchmark/corpus/snapshots/5714710c8c9a3e8a.html", // de.wikipedia Rust
    "benchmark/corpus/snapshots/e339ce76eb1cba73.html", // Fed reserve open-market (regulator)
    "benchmark/corpus/snapshots/65e1c5b5502a5c81.html", // Rust 1.83 release blog (tech_blog)
];

/// Per-fixture smoke verdict. A fixture passes if:
/// - extract_content does not panic
/// - the returned text length is > 200 chars (substantive output)
///
/// We collect verdicts across all fixtures and require ≥ 7/10 substantive
/// outputs (allowing 3 "low yield" cases for tiny inputs like example.com).
#[test]
fn extract_content_smoke_gate_runs_full_pipeline() {
    let mut substantive = 0usize;
    let mut report = String::new();

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "smoke gate fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // Run the full pipeline. The `Dom` must stay alive for the
        // duration of the extraction (rcdom Drop quirk — Stage 2d
        // discovery, pinned via `dones_alive` Vec semantics).
        let dom = Dom::parse(&html);
        let html_root = dom
            .root_element()
            .expect("html5ever synthesises <html>");
        let opts = cleaning::Options::default();
        cleaning::tree_cleaning(&html_root, &opts);
        cleaning::convert_tags(&html_root, &opts);

        // After cleaning + convert_tags, run extract_content on the
        // cleaned tree's body.
        let body = dom.body().expect("body present after cleaning");

        let (_result_body, text, text_len) = main_extractor::extract_content(&body, &opts);

        if text_len > 200 {
            substantive += 1;
            report.push_str(&format!(
                "  SUBST  {}  ({} chars)\n",
                fixture_rel, text_len
            ));
        } else {
            report.push_str(&format!(
                "  LOW    {}  ({} chars, sample: {:?})\n",
                fixture_rel,
                text_len,
                truncate(&text, 80)
            ));
        }
    }

    eprintln!("\n=== extract_content smoke gate verdict ===");
    eprintln!("{report}");
    eprintln!("SUBSTANTIVE {substantive}/{}\n", FIXTURES.len());

    // Threshold: ≥ 7 substantive extractions (allow 3 low-yield cases).
    assert!(
        substantive >= 7,
        "smoke gate: expected ≥ 7 substantive extractions, got {substantive}/{} — pipeline regression?",
        FIXTURES.len()
    );
}

fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        let mut i = n;
        while !s.is_char_boundary(i) && i > 0 {
            i -= 1;
        }
        format!("{}…", &s[..i])
    }
}
