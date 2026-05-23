//! M7 Stage 1 — corpus-wide TXT equivalence diff harness.
//!
//! Sibling of `trafilatura_markdown_gate`: where that gate pins mdrcel's
//! `extract_to_markdown` against Python's
//! `trafilatura.extract(raw, output_format="markdown")`, this gate pins the
//! plain-TXT path — mdrcel's `extract_to_txt` against Python's
//! `trafilatura.extract(raw, output_format="txt")` byte-for-byte.
//!
//! TXT is the **formatting-off** sibling of markdown (`core.py:71-98`,
//! `settings.py:133`): same pipeline shape, but `formatting=False` so no
//! `**bold**`/`*italic*` markers and the `xmltotxt` formatter emits plain
//! block text. Since markdown is already 51/51 green, TXT is expected to be
//! very close — divergences should be few. If many appear, suspect a harness
//! bug (wrong format string / wrong mdrcel entry point) before assuming a
//! real divergence.
//!
//! # Comparison shape
//!
//! Both sides emit a Python/Rust `str`, both pipelines NFC-normalise their
//! own output; the harness NFC-normalises both ONCE MORE (belt-and-braces)
//! then strict byte-compares the resulting UTF-8.
//!
//! # GREEN criterion
//!
//! GREEN when every fixture lands in exactly one of: `pass` (substantive
//! byte-equivalence), `allowlist_python_bug` (Python is wrong; ADR under
//! `wrk_docs/m7-allowlist/`), or `deferred_known_defect` (mdrcel is wrong but
//! pinned to a future milestone; ADR under `wrk_docs/m7-deferred/`). Any
//! untriaged bucket count > 0 fails the gate.

use mdrcel::{extract_to_txt, Options};

mod common;
use common::{
    classify, escape, first_diff_index, nfc, run_oracle, window_around, workspace_path, Bucket,
};

/// Fixtures where Python's `trafilatura.extract(output_format="txt")` is the
/// under-extractor or otherwise anti-inversion-violating in a corpus-specific
/// way. **Each entry MUST have a corresponding ADR** in
/// `wrk_docs/m7-allowlist/`. Each TXT divergence is triaged into here only
/// after reading the vendored Python source confirms Python is the buggy
/// side. All five entries below share their root cause with the markdown gate
/// (the divergence is format-independent — selection/parser/decoding, not
/// formatting), cross-referenced in each ADR.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K — Python's bare_extraction returns empty on this
    // structurally-valid filing (upstream of the txt/markdown branch); mdrcel
    // extracts ~75KB. Same as markdown allowlist. ADR:
    // wrk_docs/m7-allowlist/41d2afac.md.
    "41d2afac25d46010.html",
    // Hacker News front page — Python over-extracts the `<td class="pagetop">`
    // nav block and emits the story list flat; mdrcel emits a table and omits
    // the nav chrome. Selection/table-walk, format-independent. ADR:
    // wrk_docs/m7-allowlist/0f63a2a5.md.
    "0f63a2a5a5620b74.html",
    // DFIN XBRL 10-K (Apple relative) — single empty table cell drift from
    // html5ever vs lxml XBRL tree construction (>99.95% identical).
    // Parser/table-walk, format-independent. ADR:
    // wrk_docs/m7-allowlist/683d5643.md.
    "683d5643b173c7fd.html",
    // Rust blog index — Python's link_density_test_tables rejects the
    // 76.8%-link-density post-list table that IS the content (161 chars);
    // mdrcel preserves the ~17KB listing. Selection, format-independent. ADR:
    // wrk_docs/m7-allowlist/9c64e8e3.md.
    "9c64e8e3fcd844d4.html",
    // DFIN XBRL 10-K (Berkshire) — `&#153;` HTML5 §13.2.5 CP-1252 remap
    // (0x99 → U+2122 ™); html5ever follows the spec, lxml strips the control
    // char. Character decoding, format-independent. ADR:
    // wrk_docs/m7-allowlist/dc8ba3c0.md.
    "dc8ba3c086153274.html",
];

/// Fixtures where **mdrcel** is the buggy side on the TXT path — divergence
/// is a known mdrcel defect, not an anti-inversion-clean Python bug. Each
/// entry MUST have a corresponding ADR in `wrk_docs/m7-deferred/`. A fixture
/// MUST NOT appear in both lists.
const DEFERRED_KNOWN_DEFECT: &[&str] = &[
    // (M10 Phase 1, 2026-05-23) The 507b9cdb (Apple FR / U+2063) deferral
    // was resolved by porting Python's `remove_control_characters`
    // (utils.py:266-274) into `output::line_processing` and the metadata
    // slot path. ADR `wrk_docs/m7-deferred/507b9cdb.md` carries a closure
    // note. The fixture now passes byte-equality on this gate
    // substantively.
];

/// All 51 corpus snapshots — copied verbatim from the markdown gate
/// (`tests/trafilatura_markdown_gate.rs`). The gate is corpus-wide by design.
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

#[test]
fn trafilatura_txt_gate() {
    let mut pass = 0usize;
    let total = FIXTURES.len();
    let mut report = String::new();
    let mut bucket_empty = 0usize;
    let mut bucket_ws = 0usize;
    let mut bucket_content = 0usize;
    let mut allowlist_python_bug = 0usize;
    let mut deferred_known_defect = 0usize;

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "M7 Stage 1 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust txt output.
        let rust_txt_raw = match extract_to_txt(&html, None, &Options::default()) {
            Ok(s) => s,
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_txt returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python txt output (subprocess oracle).
        let python_txt_raw = match run_oracle("--txt", &path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M7 STAGE 1 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces).
        let rust_txt: String = nfc(&rust_txt_raw);
        let python_txt: String = nfc(&python_txt_raw);

        if rust_txt == python_txt {
            pass += 1;
            continue;
        }

        // Diverged. Check allowlist + deferred lists FIRST.
        let basename = std::path::Path::new(fixture_rel)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let allowlisted = PYTHON_UNDER_EXTRACT_ALLOWLIST.contains(&basename);
        let deferred = DEFERRED_KNOWN_DEFECT.contains(&basename);
        assert!(
            !(allowlisted && deferred),
            "M7 txt gate: fixture {basename} appears in BOTH allowlist and deferred lists; \
             pick one — allowlist = anti-inversion-clean Python bug, deferred = mdrcel defect",
        );

        let bucket = classify(&rust_txt, &python_txt);
        if allowlisted {
            allowlist_python_bug += 1;
        } else if deferred {
            deferred_known_defect += 1;
        } else {
            match bucket {
                Bucket::EmptyVsNon => bucket_empty += 1,
                Bucket::WhitespaceOnly => bucket_ws += 1,
                Bucket::ContentMismatch => bucket_content += 1,
            }
        }

        let first_diff_byte = first_diff_index(rust_txt.as_bytes(), python_txt.as_bytes());
        let rust_window = window_around(&rust_txt, first_diff_byte, 100);
        let python_window = window_around(&python_txt, first_diff_byte, 100);

        let tag = if allowlisted {
            "allowlist_python_bug"
        } else if deferred {
            "deferred_known_defect"
        } else {
            bucket.label()
        };
        report.push_str(&format!(
            "  FAIL  {}  [{}]\n    rust={} chars  python={} chars  first-diff-byte={}\n      rust:   {}\n      python: {}\n",
            fixture_rel,
            tag,
            rust_txt.chars().count(),
            python_txt.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M7 txt corpus gate verdict (BLOCKER) ===");
    eprintln!(
        "GREEN {} = {pass} substantive + {allowlist_python_bug} allowlisted + {deferred_known_defect} deferred / {total}\n",
        pass + allowlist_python_bug + deferred_known_defect,
    );
    if !report.is_empty() {
        eprintln!("Per-fixture failures:\n{report}");
        eprintln!(
            "Bucket totals: empty-vs-non={bucket_empty}  whitespace-only={bucket_ws}  content-mismatch={bucket_content}  allowlist_python_bug={allowlist_python_bug}  deferred_known_defect={deferred_known_defect}",
        );
    }

    // Honest accounting invariant: every fixture lands in exactly one bucket.
    let accounted = pass
        + bucket_empty
        + bucket_ws
        + bucket_content
        + allowlist_python_bug
        + deferred_known_defect;
    assert_eq!(
        accounted, total,
        "M7 txt gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug}, \
         deferred={deferred_known_defect} sum to {accounted} but total={total}",
    );

    // BLOCKER gate: GREEN when every fixture is pass + allowlist + deferred.
    if pass + allowlist_python_bug + deferred_known_defect != total {
        panic!(
            "M7 txt gate divergence: {pass}/{total} substantive + \
             {allowlist_python_bug} allowlisted + {deferred_known_defect} deferred. \
             Untriaged buckets: empty-vs-non={bucket_empty}, whitespace-only={bucket_ws}, \
             content-mismatch={bucket_content}. \
             See per-fixture report above for first-diff windows. \
             Either fix the regression OR triage the new divergence into \
             PYTHON_UNDER_EXTRACT_ALLOWLIST (with a wrk_docs/m7-allowlist/ ADR) \
             or DEFERRED_KNOWN_DEFECT (with a wrk_docs/m7-deferred/ ADR).",
        );
    }
}

