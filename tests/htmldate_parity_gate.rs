//! M4 Stage 1 sub-stage H — **5th BLOCKER gate**: byte-equivalent parity
//! between mdrcel's [`mdrcel::htmldate::core::find_date`] and Python's
//! `htmldate.find_date(text, original_date=False, outputformat="%Y-%m-%d")`
//! on a 10-snapshot subset of the M3 Stage 3-B corpus.
//!
//! # Why this gate exists
//!
//! Sub-stages A–G ported the full `htmldate` package (settings, validators,
//! regex catalogues, extractors, core walkers, `search_page` cascade,
//! `find_date` entrypoint, `clean_html`) with unit-level test coverage —
//! 204 new lib tests in the `src/htmldate/**` modules. Unit tests pin
//! per-function semantics but cannot prove the **orchestration** of
//! `find_date` produces byte-identical output to Python on real HTML.
//!
//! This gate closes that loop. Each fixture's expected date was produced
//! by running `python -c "import htmldate; print(htmldate.find_date(open(...).read(), original_date=False))"`
//! against `htmldate==1.9.4` (the version sub-stage A's scoping report
//! pinned). The 10 fixtures cover every major path inside `find_date`:
//!
//! - `<meta property="article:published_time">` — `examine_header`
//! - `<time datetime="...">` — `examine_time_elements`
//! - JSON-LD `datePublished` — `json_search`
//! - URL-embedded date — `extract_url_date` via canonical link
//! - free-text `LONG_TEXT_PATTERN` match — `regex_parse`
//! - `search_page` regex cascade fallback — final 10-arm cascade
//!
//! # Verification (2026-05-21)
//!
//! On commit `3f44874` (sub-stage G landing), this gate passes **10/10**
//! byte-equivalent against Python `htmldate==1.9.4`. The parity probe that
//! produced these expectations lived briefly at
//! `tests/htmldate_parity_probe.rs` and was deleted once the gate file
//! landed.

use std::fs;
use std::path::PathBuf;

use mdrcel::htmldate::{core::find_date, settings::MIN_DATE, utils::Extractor};
use mdrcel::readability::dom::Dom;

/// The 10-fixture parity oracle.
///
/// Format: `(snapshot filename, Python `htmldate.find_date` output)`.
///
/// All snapshots live under `benchmark/corpus/snapshots/`. The expected
/// strings are produced by Python `htmldate==1.9.4` on commit `3f44874`
/// (M4 Stage 1 sub-stage G landing).
const FIXTURES: &[(&str, &str)] = &[
    ("ae2c2184beb6d264.html", "2026-05-16"),
    ("f76ec833b4b5e57d.html", "2026-05-11"),
    ("e1106c5e26712078.html", "2026-05-14"),
    ("9c8f49f04f792f81.html", "2026-05-10"),
    ("86df4d2e654952e4.html", "2026-05-01"),
    ("de79cc5a2c3b5416.html", "2026-05-16"),
    ("d71ec714e950bddf.html", "2026-03-25"),
    ("d159708a94e68ab6.html", "2025-10-15"),
    ("aa562fed8195cd92.html", "2026-05-01"),
    ("9ec7aaf8edb71ac1.html", "2026-03-01"),
];

/// Build the default `Extractor` matching Python's `find_date(text)` defaults
/// (`core.py:810-812`): `extensive_search=True`, no upper bound (Python's
/// `get_max_date(None)` is `datetime.now()`; we use the (9999,12,31) "very
/// future" sentinel that `validators.rs::get_max_date` documents),
/// `min_date=MIN_DATE` (1995-01-01), `original_date=False`,
/// `outputformat="%Y-%m-%d"`.
fn default_options() -> Extractor {
    Extractor::new(
        true,
        (9999, 12, 31),
        MIN_DATE,
        false,
        "%Y-%m-%d".to_string(),
    )
}

/// The BLOCKER gate: every fixture's mdrcel output must byte-equal Python's.
///
/// On regression the assertion message lists every divergent fixture so the
/// fix-cycle has the full picture without a re-run.
#[test]
fn find_date_matches_python_on_corpus_snapshots() {
    let root: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("benchmark/corpus/snapshots");
    let opts = default_options();

    let mut diffs: Vec<String> = Vec::new();

    for (name, expected) in FIXTURES {
        let path = root.join(name);
        let html = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let dom = Dom::parse(&html);
        let tree = dom
            .root_element()
            .unwrap_or_else(|| panic!("no root element in {}", name));
        let actual = find_date(&tree, &opts);
        let actual_str = actual.as_deref().unwrap_or("None");
        if actual_str != *expected {
            diffs.push(format!("  {name}: rust={actual_str} python={expected}"));
        }
    }

    if !diffs.is_empty() {
        panic!(
            "htmldate parity gate: {}/{} fixtures diverged from Python:\n{}",
            diffs.len(),
            FIXTURES.len(),
            diffs.join("\n"),
        );
    }
}
