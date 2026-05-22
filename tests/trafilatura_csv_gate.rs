//! M7 Stage 3 — corpus-wide CSV equivalence diff harness.
//!
//! Sibling of `trafilatura_txt_gate` / `trafilatura_json_gate`: this gate
//! pins mdrcel's `extract_to_csv` against Python's
//! `trafilatura.extract(raw, output_format="csv")` byte-for-byte.
//!
//! CSV is the **tabular** sibling of txt/json. Python's `core.py` routes the
//! extracted `Document` through `xmltocsv(document, include_formatting)`
//! (`xml.py:366-390`): ONE tab-delimited row of 11 fields — url, id,
//! fingerprint, hostname, title, image, date, `text`, `comments`, license,
//! pagetype — serialised via the stdlib `csv.writer(delimiter="\t",
//! quoting=csv.QUOTE_MINIMAL)` (default `lineterminator="\r\n"`). Empty / None
//! fields render as the literal token `null` (`d if d else null`). The `text`
//! field is the SAME `xmltotxt(body, include_formatting=False)` string the txt
//! and json gates compare, just embedded as a (typically quoted) csv cell, so
//! divergences should track the txt gate closely; if many NEW ones appear,
//! suspect a harness/dialect bug before a real divergence.
//!
//! # Header-row reconciliation (the one csv-specific wrinkle)
//!
//! Python's `xmltocsv` (and `extract(output_format="csv")`) emits ONLY the
//! data row — NO header. mdrcel's public `extract_to_csv` PREPENDS an
//! 11-column header row (`csv_header_row`, an intentional ergonomic feature:
//! a single-call "header + one data row" surface for users who don't run
//! pandas / `csv.DictWriter`). The header is a fixed, deterministic prefix
//! (`url\tid\t…\tpagetype\r\n`). To compare like-for-like with Python's
//! actual output WITHOUT removing the deliberate feature or adding test-only
//! public API, the gate reconstructs that constant prefix locally
//! (`MDRCEL_CSV_HEADER`), asserts mdrcel emitted it verbatim, strips it, and
//! byte-compares the remaining data row against Python. The header column
//! order/names/terminator are themselves pinned by `extract_to_csv`'s own
//! lib-level unit tests; here we only need it to land back at Python's
//! header-less shape.
//!
//! # Comparison shape
//!
//! Both sides emit a `str` (a one-row csv document). The Python pipeline
//! NFC-normalises its text payload upstream (`core.py`); `xmltotxt` (which
//! both the body text and mdrcel's csv path run through) also NFC-normalises.
//! The harness NFC-normalises both whole csv strings ONCE MORE
//! (belt-and-braces) then strict byte-compares the resulting UTF-8.
//!
//! # GREEN criterion
//!
//! GREEN when every fixture lands in exactly one of: `pass` (substantive
//! byte-equivalence), `allowlist_python_bug` (Python is wrong; ADR under
//! `wrk_docs/m7-allowlist/`), or `deferred_known_defect` (mdrcel is wrong but
//! pinned to a future milestone; ADR under `wrk_docs/m7-deferred/`). Any
//! untriaged bucket count > 0 fails the gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use mdrcel::{extract_to_csv, Options};
use unicode_normalization::UnicodeNormalization;

/// The fixed 11-column header row mdrcel's `extract_to_csv` prepends ahead of
/// the data row (`output::csv_header_row("\t")`). Python's `xmltocsv` emits NO
/// header, so the gate strips this constant prefix to compare like-for-like.
/// Tab-delimited, `\r\n`-terminated — must match `csv_header_row`'s output
/// (itself pinned by lib-level unit tests). If `csv_header_row` ever changes,
/// the `assert!` in `mdrcel_data_row` fails loudly rather than silently
/// mis-comparing.
const MDRCEL_CSV_HEADER: &str =
    "url\tid\tfingerprint\thostname\ttitle\timage\tdate\ttext\tcomments\tlicense\tpagetype\r\n";

/// 0-based index of the `fingerprint` column in `xmltocsv`'s 11-field row.
/// Column order (`xml.py:377-389`): url=0, id=1, **fingerprint=2**,
/// hostname=3, title=4, image=5, date=6, text=7, comments=8, license=9,
/// pagetype=10. Verified against the vendored `xml.py` source.
///
/// Python fills this column via `content_fingerprint(title + " " + raw_text)`
/// (`core.py:481-485`) using `blake2b(digest_size=8)`; mdrcel deliberately
/// uses a hand-rolled FNV-1a simhash (and does not wire a fingerprint onto the
/// csv `Document` at all → emits `null`). The value can therefore NEVER match
/// by design. The gate MASKS this single column (blanks it on both sides) and
/// SHAPE-CHECKS Python's value (well-formed lowercase-hex simhash) rather than
/// deferring every non-empty fixture. See
/// `wrk_docs/m7-deferred/fingerprint-blake2b.md` (the established pattern that
/// the Stage 4/5 xml/xmltei gates will reuse).
const FINGERPRINT_COL: usize = 2;

/// Fixtures where Python's `trafilatura.extract(output_format="csv")` is the
/// under-extractor or otherwise anti-inversion-violating in a corpus-specific
/// way. **Each entry MUST have a corresponding ADR** in
/// `wrk_docs/m7-allowlist/`. The csv `text` cell is the same `xmltotxt(body)`
/// string the txt + json gates diff, so these five share their root cause with
/// the txt / json / markdown gates — the divergence is format-independent
/// (selection/parser/decoding, not csv structure). Each cross-references the
/// EXISTING ADR rather than duplicating the analysis.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K — Python's bare_extraction returns empty on this
    // structurally-valid filing (upstream of the csv branch); mdrcel
    // extracts ~75KB. Format-independent. ADR:
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

/// Fixtures where **mdrcel** is the buggy side on the CSV path — divergence
/// is a known mdrcel defect, not an anti-inversion-clean Python bug. Each
/// entry MUST have a corresponding ADR in `wrk_docs/m7-deferred/`. A fixture
/// MUST NOT appear in both lists.
const DEFERRED_KNOWN_DEFECT: &[&str] = &[
    // Apple FR (French Wikipedia) — mdrcel leaks U+2063 INVISIBLE SEPARATOR
    // (Unicode category Cf) that the source HTML literally contains around
    // link text. Python's xmltotxt body text is run through
    // `remove_control_characters` (utils.py:272-300; `char.isprintable() or
    // char.isspace()`); mdrcel's `output::line_processing` deliberately
    // omitted that step pending a real control-character-leak test. The same
    // `xmltotxt(body)` string flows into the csv `text` cell, so the leak
    // re-appears here exactly as on the txt / json paths. mdrcel is the buggy
    // side; a faithful fix needs a Unicode general-category facility (new
    // dependency / vendored table = supervisor-sign-off work), so it is
    // deferred. ADR: wrk_docs/m7-deferred/507b9cdb.md (shared with the txt /
    // json gates; that ADR has an "also affects csv" note).
    "507b9cdbe036bf58.html",
];

/// All 51 corpus snapshots — copied verbatim from the txt / json gate.
/// The gate is corpus-wide by design.
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

/// Bucket classification of a divergence — coarse-grained on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Bucket {
    /// One side empty, the other not. The most severe class.
    EmptyVsNon,
    /// Both non-empty AND identical after collapsing ASCII whitespace runs.
    WhitespaceOnly,
    /// Both non-empty, differ even after whitespace collapse.
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
fn trafilatura_csv_gate() {
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
            "M7 Stage 3 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust csv output — strip the deliberate header prefix so we
        //    compare Python's header-less data row like-for-like.
        let rust_csv_raw = match extract_to_csv(&html, None, &Options::default()) {
            Ok(s) => mdrcel_data_row(&s, fixture_rel),
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_csv returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python csv output (subprocess oracle).
        let python_csv_raw = match python_csv(&path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M7 STAGE 3 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces).
        let rust_nfc: String = rust_csv_raw.as_str().nfc().collect();
        let python_nfc: String = python_csv_raw.as_str().nfc().collect();

        // 4. Mask the fingerprint column (FNV-1a-vs-blake2b deliberate
        //    divergence; ADR wrk_docs/m7-deferred/fingerprint-blake2b.md).
        //    Shape-check Python's value, then blank col 2 on BOTH sides so the
        //    remaining 10 columns are still compared byte-for-byte. A
        //    structurally-malformed Python fingerprint (where both sides DO
        //    parse as 11-field rows) FAILS the gate — the mask must not paper
        //    over a real structural divergence. When a side is NOT a well-formed
        //    11-field row (e.g. Python emits an empty string because it
        //    under-extracted — an allowlist case like 41d2afac), masking is
        //    skipped and the raw NFC strings flow to the divergence triage
        //    below, which routes the fixture to the allowlist / deferred lists.
        let (rust_csv, python_csv) = mask_fingerprint(&rust_nfc, &python_nfc, fixture_rel);

        if rust_csv == python_csv {
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
            "M7 csv gate: fixture {basename} appears in BOTH allowlist and deferred lists; \
             pick one — allowlist = anti-inversion-clean Python bug, deferred = mdrcel defect",
        );

        let bucket = classify(&rust_csv, &python_csv);
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

        let first_diff_byte = first_diff_index(rust_csv.as_bytes(), python_csv.as_bytes());
        let rust_window = window_around(&rust_csv, first_diff_byte, 100);
        let python_window = window_around(&python_csv, first_diff_byte, 100);

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
            rust_csv.chars().count(),
            python_csv.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M7 csv corpus gate verdict (BLOCKER) ===");
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
        "M7 csv gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug}, \
         deferred={deferred_known_defect} sum to {accounted} but total={total}",
    );

    // BLOCKER gate: GREEN when every fixture is pass + allowlist + deferred.
    if pass + allowlist_python_bug + deferred_known_defect != total {
        panic!(
            "M7 csv gate divergence: {pass}/{total} substantive + \
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

fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Strip mdrcel's deliberate header-row prefix so the remaining data row is
/// directly comparable to Python's header-less `xmltocsv` output. Asserts the
/// header is present verbatim (a missing/changed header is a real regression
/// in `extract_to_csv` / `csv_header_row`, surfaced loudly here).
fn mdrcel_data_row(full: &str, fixture_rel: &str) -> String {
    match full.strip_prefix(MDRCEL_CSV_HEADER) {
        Some(rest) => rest.to_string(),
        None => panic!(
            "M7 csv gate: extract_to_csv output for {fixture_rel} did not start with the \
             expected `csv_header_row` prefix; header reconciliation is broken. \
             Got first bytes: {:?}",
            &full[..full.len().min(MDRCEL_CSV_HEADER.len() + 16)],
        ),
    }
}

/// Mask the deliberate FNV-1a-vs-blake2b fingerprint divergence (column
/// [`FINGERPRINT_COL`]) so the remaining 10 columns compare byte-for-byte.
///
/// Steps (ADR `wrk_docs/m7-deferred/fingerprint-blake2b.md`):
/// 1. Parse BOTH single-record CSV strings into their 11 QUOTE_MINIMAL fields
///    (the `text` cell may embed `\t` / `\r` / `\n` and be quoted, so a naive
///    `split('\t')` is wrong — we use a quote-aware reader).
/// 2. SHAPE-CHECK Python's fingerprint field: when both sides ARE well-formed
///    11-field rows, Python's field MUST be a well-formed lowercase-hex simhash
///    (`hex(self.hash)[2:]`, 1–16 chars). A malformed value PANICS — the mask
///    must never hide a real structural divergence.
/// 3. Blank the fingerprint field on BOTH sides, re-serialise, and return the
///    masked pair for byte-comparison.
///
/// When EITHER side fails to parse into exactly 11 fields — e.g. Python emits
/// an empty string because it under-extracted (an allowlist case such as
/// `41d2afac`) — masking is skipped and the ORIGINAL `(rust, python)` strings
/// are returned unchanged, so the caller's existing divergence triage routes
/// the fixture to the allowlist / deferred lists rather than the mask
/// swallowing it. A `null` fingerprint from mdrcel (its honest state) parses
/// fine as field text and is simply blanked alongside Python's hex.
fn mask_fingerprint(rust: &str, python: &str, fixture_rel: &str) -> (String, String) {
    let (Some(mut rust_fields), Some(mut python_fields)) =
        (parse_csv_record(rust), parse_csv_record(python))
    else {
        // One side is not a clean 11-field row (e.g. an empty under-extraction).
        // Fall through with the raw strings; the caller triages the divergence.
        return (rust.to_string(), python.to_string());
    };

    // Both sides are 11-field rows: Python ALWAYS emits a real fingerprint here
    // (core.py:481-485 runs for every non-txt format), so shape-check it. A
    // malformed value is a structural divergence the mask must NOT hide.
    let py_fp = &python_fields[FINGERPRINT_COL];
    assert!(
        is_well_formed_fingerprint(py_fp),
        "M7 csv gate: python fingerprint column (idx {FINGERPRINT_COL}) on {fixture_rel} \
         is not a well-formed lowercase-hex simhash (1-16 chars): {py_fp:?} — the mask \
         must not paper over a structurally-malformed fingerprint",
    );

    // Blank the column on both sides (mdrcel emits `null` here by design).
    rust_fields[FINGERPRINT_COL].clear();
    python_fields[FINGERPRINT_COL].clear();

    (serialise_csv_record(&rust_fields), serialise_csv_record(&python_fields))
}

/// True when `s` is a non-empty lowercase-hex string of length 1–16 — the
/// shape of Python's `Simhash.to_hex()` (`hex(self.hash)[2:]`; leading zeros
/// stripped, so 1–16 hex digits for a 64-bit simhash).
fn is_well_formed_fingerprint(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Parse a single CSV record (one `\r\n`-terminated row) into its fields,
/// honouring Python `csv.writer`'s QUOTE_MINIMAL dialect: tab delimiter,
/// `"`-quoted fields with doubled internal `"`. Returns `None` unless the
/// record contains exactly 11 fields. The trailing `\r\n` (or `\n`) is
/// stripped before parsing.
fn parse_csv_record(record: &str) -> Option<Vec<String>> {
    let body = record
        .strip_suffix("\r\n")
        .or_else(|| record.strip_suffix('\n'))
        .unwrap_or(record);

    let mut fields: Vec<String> = Vec::with_capacity(11);
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = body.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            in_quotes = true;
        } else if c == '\t' {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);

    if fields.len() == 11 {
        Some(fields)
    } else {
        None
    }
}

/// Re-serialise 11 fields back to a `\r\n`-terminated CSV record using the
/// same QUOTE_MINIMAL dialect (tab delimiter; quote a field iff it contains
/// the delimiter, a `"`, `\r`, or `\n`; double internal `"`). Mirrors
/// `output::csv_quote_minimal` so a masked-but-otherwise-identical row
/// round-trips to byte-equality.
fn serialise_csv_record(fields: &[String]) -> String {
    let mut out = String::new();
    for (i, f) in fields.iter().enumerate() {
        if i > 0 {
            out.push('\t');
        }
        let needs_quote =
            f.contains('\t') || f.contains('"') || f.contains('\r') || f.contains('\n');
        if needs_quote {
            out.push('"');
            for c in f.chars() {
                if c == '"' {
                    out.push('"');
                }
                out.push(c);
            }
            out.push('"');
        } else {
            out.push_str(f);
        }
    }
    out.push_str("\r\n");
    out
}

/// Python oracle path: spawn `run.py --csv` and read its stdout as the CSV
/// payload. Bypasses the venv re-exec via `MDRCEL_TRAFILATURA_REEXECED=1`
/// (same trick as the txt / json / markdown gates).
fn python_csv(snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--csv")
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

/// Bucket-classify a divergence.
fn classify(rust: &str, python: &str) -> Bucket {
    if rust.is_empty() != python.is_empty() {
        return Bucket::EmptyVsNon;
    }
    if collapse_ws(rust) == collapse_ws(python) {
        return Bucket::WhitespaceOnly;
    }
    Bucket::ContentMismatch
}

/// Collapse every run of ASCII whitespace to a single space; strip ends.
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

/// Escape control chars + newlines so the per-fixture report is one line.
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
// Self-tests for the harness helpers.
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
    let s = "—abc—def";
    let w = window_around(s, 4, 10);
    assert!(!w.is_empty());
    assert!(s.contains(&w));
}

/// The header-strip helper must round-trip a header+row through to the bare
/// data row, and the constant must match what `extract_to_csv` actually emits
/// (verified end-to-end by the corpus gate; this is the unit-level guard).
#[test]
fn mdrcel_data_row_strips_known_header() {
    let full = format!("{MDRCEL_CSV_HEADER}null\tnull\tbody\r\n");
    assert_eq!(mdrcel_data_row(&full, "synthetic"), "null\tnull\tbody\r\n");
}

/// An 11-field tab record with a plain (unquoted) body round-trips.
#[test]
fn parse_csv_record_plain_11_fields() {
    let row = "null\tnull\tfbe8c3db32b3b7c2\tnull\tnull\tnull\tnull\tbody text\tnull\tnull\tnull\r\n";
    let fields = parse_csv_record(row).expect("11 fields");
    assert_eq!(fields.len(), 11);
    assert_eq!(fields[FINGERPRINT_COL], "fbe8c3db32b3b7c2");
    assert_eq!(fields[7], "body text");
}

/// A quoted `text` cell embedding a tab + newline must NOT be mis-split, and
/// must round-trip through serialise unchanged.
#[test]
fn parse_csv_record_quoted_text_cell_round_trips() {
    let inner = "line1\ttab\r\nline2"; // contains delimiter + CRLF
    let mut fields: Vec<String> = vec!["null".into(); 11];
    fields[7] = inner.to_string();
    let serial = serialise_csv_record(&fields);
    let reparsed = parse_csv_record(&serial).expect("11 fields");
    assert_eq!(reparsed[7], inner, "quoted cell must survive round-trip");
    assert_eq!(reparsed.len(), 11);
}

/// A row with the wrong number of fields is rejected (defensive: a malformed
/// record must not silently pass the mask).
#[test]
fn parse_csv_record_rejects_wrong_field_count() {
    assert!(parse_csv_record("a\tb\tc\r\n").is_none());
}

#[test]
fn fingerprint_shape_check_accepts_real_values() {
    assert!(is_well_formed_fingerprint("fbe8c3db32b3b7c2")); // 16 chars
    assert!(is_well_formed_fingerprint("3f6")); // leading-zero-stripped short
    assert!(is_well_formed_fingerprint("a"));
}

#[test]
fn fingerprint_shape_check_rejects_malformed() {
    assert!(!is_well_formed_fingerprint("")); // empty
    assert!(!is_well_formed_fingerprint("null")); // not hex (l, u)
    assert!(!is_well_formed_fingerprint("ABCD")); // uppercase (Python emits lower)
    assert!(!is_well_formed_fingerprint("0123456789abcdef0")); // 17 chars > 16
    assert!(!is_well_formed_fingerprint("xyz")); // non-hex
}

/// End-to-end: two records identical except for the fingerprint column compare
/// equal AFTER masking; the shape-check accepts Python's value and tolerates
/// mdrcel's `null`.
#[test]
fn mask_fingerprint_blanks_col2_and_compares_rest() {
    let rust = "null\tnull\tnull\tnull\tnull\tnull\tnull\tbody\tnull\tnull\tnull\r\n";
    let python = "null\tnull\tfbe8c3db32b3b7c2\tnull\tnull\tnull\tnull\tbody\tnull\tnull\tnull\r\n";
    let (r, p) = mask_fingerprint(rust, python, "synthetic");
    assert_eq!(r, p, "rows must match once fingerprint col is masked");
}

/// The shape-check is load-bearing: a malformed Python fingerprint (in an
/// otherwise well-formed 11-field row) makes the mask PANIC rather than paper
/// over a structural divergence.
#[test]
#[should_panic(expected = "not a well-formed lowercase-hex simhash")]
fn mask_fingerprint_panics_on_malformed_python_fingerprint() {
    let rust = "null\tnull\tnull\tnull\tnull\tnull\tnull\tbody\tnull\tnull\tnull\r\n";
    // Python col-2 = "GARBAGE" (uppercase, non-hex) → shape-check must fail.
    let python = "null\tnull\tGARBAGE\tnull\tnull\tnull\tnull\tbody\tnull\tnull\tnull\r\n";
    let _ = mask_fingerprint(rust, python, "synthetic");
}

/// When a side is not a clean 11-field row (e.g. Python under-extracted to an
/// empty string), masking is skipped and the originals flow through to the
/// caller's allowlist / deferred triage.
#[test]
fn mask_fingerprint_passes_through_when_python_empty() {
    let rust = "null\tnull\tnull\tnull\tnull\tnull\tnull\tbody\tnull\tnull\tnull\r\n";
    let python = ""; // under-extraction
    let (r, p) = mask_fingerprint(rust, python, "synthetic");
    assert_eq!(r, rust, "rust unchanged when no mask applied");
    assert_eq!(p, python, "python unchanged when no mask applied");
}
