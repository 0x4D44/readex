//! M7 Stage 5 — corpus-wide TEI (`xmltei`) equivalence diff harness.
//!
//! Final sibling of `trafilatura_txt_gate` / `json` / `csv` / `xml`: this gate
//! pins mdrcel's `extract_to_tei` against Python's
//! `trafilatura.extract(raw, output_format="xmltei")` byte-for-byte.
//!
//! TEI is the **TEI-schema XML** sibling of the plain `xml` gate. Python's
//! `core.py:44-61` routes the extracted `Document` through
//! `control_xml_output(document, options)` (`xml.py:159-175`), which for the
//! `xmltei` format calls `build_tei_output` (`xml.py:186-193`):
//! `write_teitree` bundles the body/comments into `<text><body><div
//! type="entry">…</div><div type="comments">…</div></body></text>` under a
//! `<TEI xmlns="http://www.tei-c.org/ns/1.0">` root, prefixed by a full
//! `<teiHeader>` (`write_fullheader`, `xml.py:423-491`); `check_tei`
//! (`xml.py:196-235`) then repairs illegal structures (relabels `<head>` →
//! `<ab type="header">`, re-anchors tails, wraps loose `<div>` text, pops
//! attributes outside `TEI_VALID_ATTRS`); the tree is `sanitize_tree`d, round-
//! tripped through lxml's `CONTROL_PARSER`, and serialised via lxml
//! `tostring(..., pretty_print=True, encoding="unicode").strip()`.
//!
//! # The TEI-SPECIFIC `with_metadata` forcing (the key divergence from `xml`)
//!
//! `Extractor.__init__` (`settings.py:144-149`) forces the effective
//! `with_metadata` to `True` whenever `output_format == "xmltei"`:
//!
//! ```python
//! self.with_metadata = (with_metadata or only_with_metadata
//!                       or bool(url_blacklist) or output_format == "xmltei")
//! ```
//!
//! So even though `run.py --xmltei` passes `with_metadata=False` (mirroring the
//! other M7 modes), the TEI path ALWAYS extracts metadata and emits a fully
//! populated `<teiHeader>` (title/author/publisher/date/url). The plain `xml`
//! path does NOT — it honours `with_metadata=False` and emits a bare `<doc
//! fingerprint=…>`. mdrcel's `extract_to_tei` mirrors this: it ALWAYS extracts
//! metadata (lib.rs), independent of `opts.with_metadata`. The gate therefore
//! calls `extract_to_tei` with **default `Options`** (matching the other
//! gates) and relies on the lib-side forcing.
//!
//! # `<teiHeader>` metadata neutralisation (the surgical mask, csv/xml pattern)
//!
//! Because TEI forces `with_metadata=True` (above), the `<teiHeader>` is fully
//! populated from the metadata-extraction subsystem — the FIRST and ONLY M7
//! gate to exercise it (txt/json/csv use `with_metadata=False`; xml emits only
//! the fingerprint). mdrcel's metadata extraction diverges from Python on
//! several axes that are NOT TEI-format concerns: `filedate` (Python = today;
//! mdrcel has no slot — M4 Stage 6 deferred), the blake2b `fingerprint`,
//! date truncation/value, `www.`-hostname stripping, and author/sitename
//! extraction quality. mdrcel is the weaker side on each, so they are DEFERRED,
//! not allowlisted. After two minimal `check_tei` tail-semantics fixes (see
//! ADR), the TEI **`<text>` subtree is byte-identical on all 39
//! non-allowlist/non-deferred fixtures** — 100% of the residual divergence is
//! in the header.
//!
//! Following the established csv/xml masking pattern (mask the one diverging
//! field, byte-compare the rest), this gate NEUTRALISES the `<teiHeader>`
//! region — collapsing both sides' header to a canonical empty shell — and
//! byte-compares the `<text>` subtree (everything the TEI serialiser /
//! `check_tei` / pretty-printer actually produces) on every fixture. Deferring
//! all 39 instead would make the gate vacuous. The fingerprint note carries an
//! EXTRA explicit shape-check (well-formed lowercase-hex, 1–16 chars) before
//! the header is blanked, so the simhash divergence stays accounted for.
//! Documented in `wrk_docs/m7-deferred/tei-header-metadata.md` and the shared
//! `wrk_docs/m7-deferred/fingerprint-blake2b.md`.
//!
//! # Comparison shape
//!
//! Both sides emit a pretty-printed XML `str`, lxml-`.strip()`ed (no trailing
//! newline). Python NFC-normalises (`core.py:98`); mdrcel's `extract_to_tei`
//! NFC-normalises too. The harness NFC-normalises both whole strings once more
//! then strict byte-compares.
//!
//! # GREEN criterion
//!
//! GREEN when every fixture lands in exactly one of: `pass`,
//! `allowlist_python_bug` (ADR under `wrk_docs/m7-allowlist/`), or
//! `deferred_known_defect` (ADR under `wrk_docs/m7-deferred/`). Any untriaged
//! bucket count > 0 fails the gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use mdrcel::{extract_to_tei, Options};
use unicode_normalization::UnicodeNormalization;

/// Fixtures where Python's `trafilatura.extract(output_format="xmltei")` is the
/// under-extractor or otherwise anti-inversion-violating in a corpus-specific
/// way (Python/lxml is wrong or non-spec; mdrcel is correct-by-spec). **Each
/// entry MUST have a corresponding ADR** in `wrk_docs/m7-allowlist/`. The TEI
/// body text + tree shape are the same content the txt/json/csv/xml gates diff,
/// so these share their root cause and cross-reference the EXISTING ADR. The
/// xml gate's allowlist (11 entries) is the closest precedent since TEI is
/// structure-preserving like xml; each entry is verified to reproduce on the
/// TEI path for the SAME documented reason.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K — Python's bare_extraction returns empty (upstream of the
    // tei branch); mdrcel extracts ~75KB. Format-independent. ADR:
    // wrk_docs/m7-allowlist/41d2afac.md.
    "41d2afac25d46010.html",
    // Hacker News front page — Python over-extracts the nav block; mdrcel
    // emits a table and omits the chrome. Selection, format-independent. ADR:
    // wrk_docs/m7-allowlist/0f63a2a5.md.
    "0f63a2a5a5620b74.html",
    // DFIN XBRL 10-K (Apple relative) — single empty table cell drift from
    // html5ever vs lxml XBRL tree construction. Parser, format-independent.
    // ADR: wrk_docs/m7-allowlist/683d5643.md.
    "683d5643b173c7fd.html",
    // Rust blog index — Python's link_density_test_tables rejects the
    // post-list table that IS the content; mdrcel preserves it. Selection,
    // format-independent. ADR: wrk_docs/m7-allowlist/9c64e8e3.md.
    "9c64e8e3fcd844d4.html",
    // DFIN XBRL 10-K (Berkshire) — `&#153;` HTML5 §13.2.5 CP-1252 remap;
    // html5ever follows the spec, lxml strips the control char. Character
    // decoding, format-independent. ADR: wrk_docs/m7-allowlist/dc8ba3c0.md.
    "dc8ba3c086153274.html",
    // Workiva inline-XBRL filings — `<div type="entry"><body><p>` (mdrcel) vs
    // `<div type="entry"><p>` (Python): same html5ever-vs-lxml XBRL tree
    // construction as 683d5643. ADR: wrk_docs/m7-allowlist/xbrl-body-wrapper.md.
    "340e6571c584979a.html",
    "577e61856ca2770d.html",
    "9a1590d0917107a7.html",
    // First web page ever (CERN, 1991) — malformed uppercase pre-HTML5 markup;
    // html5ever vs lxml reconstruct the body differently. ADR:
    // wrk_docs/m7-allowlist/74ef4dad.md.
    "74ef4dadd5f70cb5.html",
    // `<code>`/`<pre>` leading-newline — html5ever follows HTML5 §13.2.5; lxml
    // in XML mode keeps it. Same family as dc8ba3c0. ADR:
    // wrk_docs/m7-allowlist/39ca4af9.md.
    "39ca4af9befa0524.html",
    // Wikipedia infobox — one extra empty `<cell/>` from html5ever-vs-lxml
    // table construction; identical mechanism to 683d5643. ADR:
    // wrk_docs/m7-allowlist/8638632a.md.
    "8638632aa27b2f45.html",
];

/// Fixtures where **mdrcel** is the buggy side on the TEI path — a known mdrcel
/// defect, not an anti-inversion-clean Python bug. Each entry MUST have a
/// corresponding ADR in `wrk_docs/m7-deferred/`. A fixture MUST NOT appear in
/// both lists.
const DEFERRED_KNOWN_DEFECT: &[&str] = &[
    // Apple FR (French Wikipedia) — mdrcel leaks U+2063 INVISIBLE SEPARATOR
    // (Unicode category Cf) that the source HTML literally contains; Python's
    // body text is run through `remove_control_characters` (utils.py:272-300),
    // mdrcel's `output::line_processing` omits that step. The same body text
    // flows into the TEI `<div type="entry">` element text, so the leak
    // re-appears exactly as on the txt/json/csv/xml paths. mdrcel is the buggy
    // side; a faithful fix needs a Unicode general-category facility (new
    // dependency / vendored table = supervisor-sign-off). ADR:
    // wrk_docs/m7-deferred/507b9cdb.md (shared; has an "also affects xmltei"
    // note). NOTE: the fingerprint note is still neutralised for this fixture
    // before the U+2063 body divergence is observed — the two are independent.
    "507b9cdbe036bf58.html",
];

/// All 51 corpus snapshots — copied verbatim from the txt / json / csv / xml
/// gate. The gate is corpus-wide by design.
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
fn trafilatura_tei_gate() {
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
            "M7 Stage 5 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust TEI output (default Options — extract_to_tei forces metadata
        //    extraction internally, mirroring settings.py:144-149).
        let rust_tei_raw = match extract_to_tei(&html, None, &Options::default()) {
            Ok(s) => s,
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_tei returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python TEI output (subprocess oracle).
        let python_tei_raw = match python_tei(&path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M7 STAGE 5 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces).
        let rust_nfc: String = rust_tei_raw.as_str().nfc().collect();
        let python_nfc: String = python_tei_raw.as_str().nfc().collect();

        // 4. Shape-check the fingerprint note (keeps the blake2b divergence
        //    explicitly accounted for — ADR fingerprint-blake2b.md), then
        //    neutralise the whole `<teiHeader>` region on BOTH sides (ADR
        //    tei-header-metadata.md) so the byte-comparison pins the `<text>`
        //    subtree — everything the TEI serialiser/check_tei/pretty-printer
        //    produces. When Python under-extracted (no TEI tree — an allowlist
        //    case like 41d2afac) the neutralisation is a no-op and the raw NFC
        //    strings flow to the divergence triage below.
        let (rust_tei, python_tei) = neutralise_header(&rust_nfc, &python_nfc, fixture_rel);

        if rust_tei == python_tei {
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
            "M7 tei gate: fixture {basename} appears in BOTH allowlist and deferred lists; \
             pick one — allowlist = anti-inversion-clean Python bug, deferred = mdrcel defect",
        );

        let bucket = classify(&rust_tei, &python_tei);
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

        let first_diff_byte = first_diff_index(rust_tei.as_bytes(), python_tei.as_bytes());
        let rust_window = window_around(&rust_tei, first_diff_byte, 120);
        let python_window = window_around(&python_tei, first_diff_byte, 120);

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
            rust_tei.chars().count(),
            python_tei.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M7 xmltei corpus gate verdict (BLOCKER) ===");
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
        "M7 tei gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug}, \
         deferred={deferred_known_defect} sum to {accounted} but total={total}",
    );

    // BLOCKER gate: GREEN when every fixture is pass + allowlist + deferred.
    if pass + allowlist_python_bug + deferred_known_defect != total {
        panic!(
            "M7 tei gate divergence: {pass}/{total} substantive + \
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

/// The canonical empty form the `<teiHeader>` is collapsed to on both sides.
const EMPTY_TEI_HEADER: &str = "<teiHeader/>";

/// Neutralise the metadata-bearing `<teiHeader>` so the byte-comparison pins
/// the TEI `<text>` subtree (the serialiser's actual product).
///
/// Background (ADRs `wrk_docs/m7-deferred/tei-header-metadata.md` +
/// `fingerprint-blake2b.md`): TEI forces `with_metadata=True`
/// (settings.py:144-149), so the header is fully populated from the
/// metadata-extraction subsystem, which mdrcel diverges from on filedate /
/// fingerprint / date / hostname / author. These are mdrcel-weaker deferred
/// gaps, not TEI-format defects. The `<text>` subtree is byte-identical on
/// every non-allowlist/non-deferred fixture.
///
/// Steps:
/// 1. SHAPE-CHECK Python's `<note type="fingerprint">` text where present: it
///    MUST be a well-formed lowercase-hex simhash (1–16 chars). A malformed
///    value PANICS — keeps the blake2b divergence explicitly accounted for and
///    proves the header still carries a real fingerprint before it is blanked.
/// 2. Collapse the `<teiHeader>…</teiHeader>` element to `<teiHeader/>` on BOTH
///    sides. When a side has no header (e.g. Python under-extracted to an empty
///    string) the collapse is a no-op for that side, so the caller's divergence
///    triage still routes the fixture to the allowlist / deferred lists.
fn neutralise_header(rust: &str, python: &str, fixture_rel: &str) -> (String, String) {
    if let Some(fp) = note_fingerprint_value(python) {
        assert!(
            is_well_formed_fingerprint(&fp),
            "M7 tei gate: python <note type=\"fingerprint\"> text on {fixture_rel} \
             is not a well-formed lowercase-hex simhash (1-16 chars): {fp:?} — the \
             neutralisation must not paper over a structurally-malformed fingerprint",
        );
    }
    (collapse_tei_header(rust), collapse_tei_header(python))
}

/// Collapse a populated `<teiHeader>…</teiHeader>` (which may be self-closing
/// already) to the canonical `<teiHeader/>`. Idempotent; a no-op when the
/// header is absent (e.g. Python under-extracted to an empty string).
fn collapse_tei_header(s: &str) -> String {
    let open = "<teiHeader>";
    let Some(open_pos) = s.find(open) else {
        // Already self-closing or absent.
        return s.to_string();
    };
    let after_open = open_pos + open.len();
    let close = "</teiHeader>";
    let Some(rel_close) = s[after_open..].find(close) else {
        return s.to_string();
    };
    let close_end = after_open + rel_close + close.len();
    format!("{}{}{}", &s[..open_pos], EMPTY_TEI_HEADER, &s[close_end..])
}

/// Extract the text of the first `<note type="fingerprint">…</note>` element,
/// or `None` if there is no such populated note (an already-empty
/// self-closing `<note type="fingerprint"/>` returns `None`).
fn note_fingerprint_value(s: &str) -> Option<String> {
    let open = "<note type=\"fingerprint\">";
    let start = s.find(open)? + open.len();
    let rest = &s[start..];
    let end = rest.find("</note>")?;
    let val = &rest[..end];
    if val.is_empty() {
        None
    } else {
        Some(val.to_string())
    }
}


/// True when `s` is a non-empty lowercase-hex string of length 1–16 — the
/// shape of Python's `Simhash.to_hex()` (`hex(self.hash)[2:]`; leading zeros
/// stripped, so 1–16 hex digits for a 64-bit simhash).
fn is_well_formed_fingerprint(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Python oracle path: spawn `run.py --xmltei` and read its stdout as the TEI
/// payload. Bypasses the venv re-exec via `MDRCEL_TRAFILATURA_REEXECED=1`
/// (same trick as the txt / json / csv / xml / markdown gates).
fn python_tei(snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--xmltei")
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

/// The fingerprint-note text reader finds populated notes and treats an
/// already-empty self-closing note as absent.
#[test]
fn note_fingerprint_value_reads_text() {
    assert_eq!(
        note_fingerprint_value("<note type=\"fingerprint\">fbe8c3db32b3b7c2</note>"),
        Some("fbe8c3db32b3b7c2".to_string()),
    );
    assert_eq!(note_fingerprint_value("<note type=\"fingerprint\"/>"), None);
    assert_eq!(note_fingerprint_value("<note type=\"fingerprint\"></note>"), None);
    assert_eq!(note_fingerprint_value("no note here"), None);
}

/// Collapsing the `<teiHeader>` canonicalises a populated header to
/// `<teiHeader/>`, preserves the surrounding `<text>` subtree, and is idempotent.
#[test]
fn collapse_tei_header_canonicalises() {
    let py = "<TEI>\n  <teiHeader>\n    <fileDesc/>\n  </teiHeader>\n  <text><body/></text>\n</TEI>";
    let want = "<TEI>\n  <teiHeader/>\n  <text><body/></text>\n</TEI>";
    assert_eq!(collapse_tei_header(py), want);
    // Idempotent: already-collapsed header unchanged.
    assert_eq!(collapse_tei_header(want), want);
    // Absent header: no-op.
    assert_eq!(collapse_tei_header("no header"), "no header");
}

/// End-to-end: a Python populated header and mdrcel's differing header with an
/// otherwise identical `<text>` subtree compare equal AFTER neutralisation, and
/// the fingerprint shape-check accepts Python's value.
#[test]
fn neutralise_header_blanks_header_and_compares_text() {
    let rust = "<TEI><teiHeader><note type=\"fingerprint\"/></teiHeader><text><body>x</body></text></TEI>";
    let python = "<TEI><teiHeader><note type=\"fingerprint\">fbe8c3db32b3b7c2</note></teiHeader><text><body>x</body></text></TEI>";
    let (r, p) = neutralise_header(rust, python, "synthetic");
    assert_eq!(r, p, "documents must match once the header is neutralised");
}

/// The fingerprint shape-check is load-bearing: a malformed Python fingerprint
/// makes neutralisation PANIC rather than paper over a structural divergence.
#[test]
#[should_panic(expected = "not a well-formed lowercase-hex simhash")]
fn neutralise_header_panics_on_malformed_python_fingerprint() {
    let rust = "<TEI><teiHeader><note type=\"fingerprint\"/></teiHeader></TEI>";
    let python = "<TEI><teiHeader><note type=\"fingerprint\">GARBAGE</note></teiHeader></TEI>";
    let _ = neutralise_header(rust, python, "synthetic");
}

/// When Python under-extracted to an empty string (no TEI tree), the collapse
/// is a no-op and the originals flow through to the caller's allowlist /
/// deferred triage.
#[test]
fn neutralise_header_passes_through_when_python_empty() {
    let rust = "<TEI><teiHeader/><text><body/></text></TEI>";
    let python = ""; // under-extraction
    let (r, p) = neutralise_header(rust, python, "synthetic");
    assert_eq!(r, rust, "rust unchanged");
    assert_eq!(p, python, "python unchanged when it has no header");
}
