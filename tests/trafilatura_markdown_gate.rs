//! M5 Stage 2 — corpus-wide markdown equivalence diff harness.
//!
//! Where the Stage 3-B `trafilatura_extract_content_gate` pins the
//! `extract_content` lxml-Element output as canonical XML (structural-token
//! comparison), this gate pins the END of the pipeline: mdrcel's
//! `extract_to_markdown` against Python's
//! `trafilatura.extract(raw, output_format="markdown")` byte-for-byte.
//!
//! # Stage 2 success criterion
//!
//! Stage 2 succeeds when this harness **compiles, runs to completion against
//! all 51 corpus snapshots, and emits an actionable divergence report**. It
//! is NOT required to be all-green; triage is Stage 3's job. The harness
//! therefore collects every divergence into a single buffer and panics ONCE
//! at the end with the full report, instead of bailing on the first miss.
//!
//! # The comparison shape
//!
//! Both sides emit a Python/Rust `str` and are NFC-normalised by their own
//! pipelines (Python at `core.py:98`; Rust at `lib.rs:679-680`). The harness
//! NFC-normalises both ONCE MORE on its own, belt-and-braces — the contract
//! is explicit even when both sides already normalised. Comparison is then
//! strict byte-equality of the resulting UTF-8.
//!
//! # On divergence
//!
//! Every fixture that fails contributes:
//! - rust char count vs python char count
//! - first byte index of divergence
//! - a 100-char window on each side around that index
//! - a coarse "bucket" tag (whitespace-only / empty-vs-non / content)
//!
//! The end-of-report tally totals each bucket so Stage 3 can pick the
//! highest-value fix target.

use std::path::{Path, PathBuf};
use std::process::Command;

use mdrcel::{extract_to_markdown, Options};
use unicode_normalization::UnicodeNormalization;

/// Fixtures where Python's `trafilatura.extract` is the under-extractor
/// (or its output is anti-inversion-violating in a corpus-specific way).
/// **Each entry MUST have a corresponding ADR** in `wrk_docs/m5-allowlist/`
/// — see the ADR for the per-fixture rationale. Divergence still counts
/// against the substantive pass tally, but is reported separately under
/// `allowlist_python_bug` so the verdict is honest.
///
/// **Per-fixture filename only** (basename, no path); the harness checks
/// the fixture's `.html` filename against this list during the divergence
/// classification step.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K (legacy SGML wrap). Python's bare_extraction returns
    // empty on this structurally-valid filing; mdrcel extracts the same
    // ~75KB of substantive content the rest of the trafilatura cascade
    // would emit. ADR: wrk_docs/m5-allowlist/41d2afac.md.
    "41d2afac25d46010.html",
    // DFIN XBRL 10-K filing — Apple 10-K relative. Single empty table
    // cell emission disagreement at byte 32335 within a 375KB filing
    // (rust 375876 vs python 375714 chars — >99.95% identical). ADR:
    // wrk_docs/m5-allowlist/683d5643.md.
    "683d5643b173c7fd.html",
    // DFIN XBRL 10-K filing — Berkshire Hathaway. Source HTML uses
    // `&#153;` (Windows-1252 trademark sign encoding). HTML5 spec
    // requires CP-1252 remap of 0x80-0x9F numeric references to printable
    // glyphs (U+2122 here); mdrcel follows the spec, lxml strips the
    // control character. ADR: wrk_docs/m5-allowlist/dc8ba3c0.md.
    "dc8ba3c086153274.html",
    // Rust blog index page (blog.rust-lang.org). Python's link-density
    // filter (`htmlprocessing.link_density_test_tables`) rejects the
    // 76.8%-link-density `<table class="post-list">` that IS the page's
    // content. Result: 162 chars (description only). mdrcel preserves
    // the post listing (~17KB of post titles + URLs + dates). ADR:
    // wrk_docs/m5-allowlist/9c64e8e3.md.
    "9c64e8e3fcd844d4.html",
];

/// All 51 corpus snapshots — enumerated literally from
/// `benchmark/corpus/snapshots/*.html`. The gate is corpus-wide by design
/// (M5 supervisor decision: 51 is small enough that sampling buys nothing).
const FIXTURES: &[&str] = &[
    "benchmark/corpus/snapshots/0a8d11a0ba2ed7cd.html",
    "benchmark/corpus/snapshots/0d8e2588d2d1b931.html",
    "benchmark/corpus/snapshots/0e657595b198c359.html",
    "benchmark/corpus/snapshots/0f115db062b7c0dd.html",
    "benchmark/corpus/snapshots/0f63a2a5a5620b74.html",
    "benchmark/corpus/snapshots/25a711d6ecb6768d.html",
    "benchmark/corpus/snapshots/2ea386b478856ebc.html",
    "benchmark/corpus/snapshots/340e6571c584979a.html",
    "benchmark/corpus/snapshots/39ca4af9befa0524.html",
    "benchmark/corpus/snapshots/3b766ea17775d5f2.html",
    "benchmark/corpus/snapshots/3d00ac8ea9abae79.html",
    "benchmark/corpus/snapshots/3dbf9e15ef26c109.html",
    "benchmark/corpus/snapshots/41d2afac25d46010.html",
    "benchmark/corpus/snapshots/455761fa318c01ef.html",
    "benchmark/corpus/snapshots/507b9cdbe036bf58.html",
    "benchmark/corpus/snapshots/5714710c8c9a3e8a.html",
    "benchmark/corpus/snapshots/577e61856ca2770d.html",
    "benchmark/corpus/snapshots/5f27add4419ace7c.html",
    "benchmark/corpus/snapshots/65e1c5b5502a5c81.html",
    "benchmark/corpus/snapshots/683d5643b173c7fd.html",
    "benchmark/corpus/snapshots/6c688ba250fbc628.html",
    "benchmark/corpus/snapshots/74ef4dadd5f70cb5.html",
    "benchmark/corpus/snapshots/7630c14a6e2b99f6.html",
    "benchmark/corpus/snapshots/78e3fc9fe5c86c8d.html",
    "benchmark/corpus/snapshots/803b534a50a3f584.html",
    "benchmark/corpus/snapshots/8198d1bac40a1033.html",
    "benchmark/corpus/snapshots/859b46bf108e3db4.html",
    "benchmark/corpus/snapshots/8638632aa27b2f45.html",
    "benchmark/corpus/snapshots/8670676aae5747a2.html",
    "benchmark/corpus/snapshots/86df4d2e654952e4.html",
    "benchmark/corpus/snapshots/8740577e8c7803f2.html",
    "benchmark/corpus/snapshots/8badbcb95530e9c2.html",
    "benchmark/corpus/snapshots/8d5cc5247b273722.html",
    "benchmark/corpus/snapshots/9a1590d0917107a7.html",
    "benchmark/corpus/snapshots/9c64e8e3fcd844d4.html",
    "benchmark/corpus/snapshots/9c8f49f04f792f81.html",
    "benchmark/corpus/snapshots/9ec7aaf8edb71ac1.html",
    "benchmark/corpus/snapshots/a604eb8a03efa82d.html",
    "benchmark/corpus/snapshots/aa562fed8195cd92.html",
    "benchmark/corpus/snapshots/ae2c2184beb6d264.html",
    "benchmark/corpus/snapshots/d153da3363ba7cf1.html",
    "benchmark/corpus/snapshots/d159708a94e68ab6.html",
    "benchmark/corpus/snapshots/d71ec714e950bddf.html",
    "benchmark/corpus/snapshots/dc8ba3c086153274.html",
    "benchmark/corpus/snapshots/de79cc5a2c3b5416.html",
    "benchmark/corpus/snapshots/e1106c5e26712078.html",
    "benchmark/corpus/snapshots/e339ce76eb1cba73.html",
    "benchmark/corpus/snapshots/e6037cf1c861d089.html",
    "benchmark/corpus/snapshots/eceb960849e96838.html",
    "benchmark/corpus/snapshots/f405a9e3314e15da.html",
    "benchmark/corpus/snapshots/f76ec833b4b5e57d.html",
];

/// Bucket classification of a divergence — coarse-grained on purpose so
/// Stage 3 can decide the highest-value fix target at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    /// One side empty, the other not. The most severe class — typically a
    /// pipeline silently producing nothing.
    EmptyVsNon,
    /// Both sides non-empty AND identical after collapsing all ASCII
    /// whitespace runs to a single space. Often a low-stakes formatting
    /// drift (extra blank lines, paragraph spacing).
    WhitespaceOnly,
    /// Both sides non-empty, differ even after whitespace collapse. The
    /// "real" content divergence — Stage 3's main focus.
    ContentMismatch,
}

impl Bucket {
    fn label(self) -> &'static str {
        match self {
            Bucket::EmptyVsNon => "empty-vs-non",
            Bucket::WhitespaceOnly => "whitespace-only",
            Bucket::ContentMismatch => "content-mismatch",
        }
    }
}

#[test]
fn trafilatura_markdown_gate() {
    let mut pass = 0usize;
    let total = FIXTURES.len();
    let mut report = String::new();
    let mut bucket_empty = 0usize;
    let mut bucket_ws = 0usize;
    let mut bucket_content = 0usize;
    // Fixtures that diverged but appear in PYTHON_UNDER_EXTRACT_ALLOWLIST.
    // Reported separately; not counted as substantive passes (the
    // substantive count + allowlist count + bucket totals MUST equal
    // `total` so no fixture is silently dropped).
    let mut allowlist_python_bug = 0usize;

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "M5 Stage 2 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        // Read raw bytes on both sides (same decoding contract as the
        // Stage 3-B gate uses, lib.rs:101-103).
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust markdown output.
        let rust_md_raw = match extract_to_markdown(&html, None, &Options::default()) {
            Ok(s) => s,
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_markdown returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python markdown output (subprocess oracle).
        let python_md_raw = match python_markdown(&path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M5 STAGE 2 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces — both pipelines already
        //    NFC-normalise; this makes the contract explicit at gate level).
        let rust_md: String = rust_md_raw.as_str().nfc().collect();
        let python_md: String = python_md_raw.as_str().nfc().collect();

        if rust_md == python_md {
            pass += 1;
            continue;
        }

        // Diverged. Check the allowlist FIRST — allowlisted fixtures get
        // a distinct tag and bypass the bucket counters (each one is
        // anti-inversion-clean per a checked-in ADR; see
        // `PYTHON_UNDER_EXTRACT_ALLOWLIST` above + `wrk_docs/m5-allowlist/`).
        let basename = std::path::Path::new(fixture_rel)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let allowlisted = PYTHON_UNDER_EXTRACT_ALLOWLIST.contains(&basename);

        // Classify either way so the per-fixture report still shows the
        // bucket the divergence would have fallen into.
        let bucket = classify(&rust_md, &python_md);
        if allowlisted {
            allowlist_python_bug += 1;
        } else {
            match bucket {
                Bucket::EmptyVsNon => bucket_empty += 1,
                Bucket::WhitespaceOnly => bucket_ws += 1,
                Bucket::ContentMismatch => bucket_content += 1,
            }
        }

        // First byte-index of divergence + 100-char windows on each side.
        let first_diff_byte = first_diff_index(rust_md.as_bytes(), python_md.as_bytes());
        let rust_window = window_around(&rust_md, first_diff_byte, 100);
        let python_window = window_around(&python_md, first_diff_byte, 100);

        let tag = if allowlisted {
            "allowlist_python_bug"
        } else {
            bucket.label()
        };
        report.push_str(&format!(
            "  FAIL  {}  [{}]\n    rust={} chars  python={} chars  first-diff-byte={}\n      rust:   {}\n      python: {}\n",
            fixture_rel,
            tag,
            rust_md.chars().count(),
            python_md.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M5 Stage 2 markdown corpus gate verdict ===");
    eprintln!("PASS {pass}/{total} substantive + {allowlist_python_bug} allowlisted\n");
    if !report.is_empty() {
        eprintln!("Per-fixture failures:\n{report}");
        eprintln!(
            "Bucket totals: empty-vs-non={bucket_empty}  whitespace-only={bucket_ws}  content-mismatch={bucket_content}  allowlist_python_bug={allowlist_python_bug}",
        );
    }

    // Honest accounting invariant: every fixture lands in exactly one of
    // `pass`, `bucket_empty`, `bucket_ws`, `bucket_content`,
    // `allowlist_python_bug`. Catches silent fixture-drop regressions.
    let accounted =
        pass + bucket_empty + bucket_ws + bucket_content + allowlist_python_bug;
    assert_eq!(
        accounted, total,
        "M5 markdown gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug} \
         sum to {accounted} but total={total}",
    );

    // Stage 2 succeeds when the harness runs to completion. We still surface
    // the divergence count via a `panic!` so the test framework treats the
    // first-run "non-green" verdict as a test failure (which Stage 3 will
    // resolve fixture-by-fixture). Allowlisted Python-under-extract
    // fixtures do NOT trip the panic — they're documented anti-inversion
    // wins, each backed by an ADR in `wrk_docs/m5-allowlist/`.
    if pass + allowlist_python_bug != total {
        panic!(
            "M5 Stage 2 markdown gate divergence: {pass}/{total} substantive + \
             {allowlist_python_bug} allowlisted. \
             Buckets: empty-vs-non={bucket_empty}, whitespace-only={bucket_ws}, \
             content-mismatch={bucket_content}. \
             See per-fixture report above for first-diff windows.",
        );
    }
}

fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Python oracle path: spawn `run.py --markdown` and read its stdout as the
/// markdown payload. Bypasses the venv re-exec by setting
/// `MDRCEL_TRAFILATURA_REEXECED=1` (same trick as the Stage 1b / 3-B gates).
fn python_markdown(snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--markdown")
        .arg(snapshot_path)
        .env("MDRCEL_TRAFILATURA_REEXECED", "1")
        .output()
        .map_err(|e| format!("failed to spawn python: {e}"))?;

    if !output.status.success() {
        return Err(format!(
            "python adapter exited non-zero ({:?}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("python stdout not utf-8: {e}"))
}

/// Bucket-classify a divergence. Coarse on purpose — Stage 3 sub-buckets as
/// needed from the per-fixture window listing.
fn classify(rust: &str, python: &str) -> Bucket {
    if rust.is_empty() != python.is_empty() {
        return Bucket::EmptyVsNon;
    }
    if collapse_ws(rust) == collapse_ws(python) {
        return Bucket::WhitespaceOnly;
    }
    Bucket::ContentMismatch
}

/// Collapse every run of ASCII whitespace to a single space, strip leading
/// and trailing whitespace. Cheap proxy for the "did only formatting
/// differ?" question; if the answer is yes, the bucket is downgraded.
fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First byte index where two byte slices differ; min(len) if one is a
/// prefix of the other.
fn first_diff_index(a: &[u8], b: &[u8]) -> usize {
    let n = a.len().min(b.len());
    for i in 0..n {
        if a[i] != b[i] {
            return i;
        }
    }
    n
}

/// Return up to `n` chars of context centred on `byte_idx`, snapped to UTF-8
/// boundaries. Empty when the string is empty.
fn window_around(s: &str, byte_idx: usize, n: usize) -> String {
    if s.is_empty() {
        return String::new();
    }
    let half = n / 2;
    let mut lo = byte_idx.saturating_sub(half);
    let mut hi = (byte_idx + half).min(s.len());
    while lo > 0 && !s.is_char_boundary(lo) {
        lo -= 1;
    }
    while hi < s.len() && !s.is_char_boundary(hi) {
        hi += 1;
    }
    s[lo..hi].to_string()
}

/// Escape control chars + newlines so the per-fixture report is a single
/// readable line. Keeps tabs/spaces visible as `\t` / regular space.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

// ===========================================================================
// Self-tests for the harness helpers — exercised at compile time of this
// integration-test binary so the harness machinery itself doesn't drift.
// ===========================================================================

#[test]
fn bucket_empty_vs_non() {
    assert_eq!(classify("", "x"), Bucket::EmptyVsNon);
    assert_eq!(classify("x", ""), Bucket::EmptyVsNon);
}

#[test]
fn bucket_whitespace_only() {
    assert_eq!(classify("a b", "a  b"), Bucket::WhitespaceOnly);
    assert_eq!(classify("a\nb", "a b"), Bucket::WhitespaceOnly);
}

#[test]
fn bucket_content_mismatch() {
    assert_eq!(classify("hello", "world"), Bucket::ContentMismatch);
}

#[test]
fn first_diff_index_basic() {
    assert_eq!(first_diff_index(b"abc", b"abd"), 2);
    assert_eq!(first_diff_index(b"abc", b"abc"), 3);
    assert_eq!(first_diff_index(b"abc", b"abcdef"), 3);
}

#[test]
fn window_around_snaps_to_boundary() {
    // A 3-byte UTF-8 char ("é" is 2 bytes; "—" is 3) at start of string.
    let s = "—abc—def";
    let w = window_around(s, 4, 10);
    // Must be valid UTF-8 and contain at least one of the surrounding chars.
    assert!(!w.is_empty());
    assert!(s.contains(&w));
}
