//! M7 Stage 4 — corpus-wide XML equivalence diff harness.
//!
//! Sibling of `trafilatura_txt_gate` / `trafilatura_json_gate` /
//! `trafilatura_csv_gate`: this gate pins mdrcel's `extract_to_xml` against
//! Python's `trafilatura.extract(raw, output_format="xml")` byte-for-byte.
//!
//! XML is the **structured tree** sibling of csv/json. Python's `core.py:44-61`
//! routes the extracted `Document` through `control_xml_output(document,
//! options)` (`xml.py:159-175`): the body tree is renamed `<body>` → `<main>`,
//! the comments tree → `<comments>`, both wrapped in a `<doc>` root, run through
//! `clean_attributes` (only the `WITH_ATTRIBUTES` tag set keeps attrs),
//! `sanitize_tree`, a round-trip through lxml's `CONTROL_PARSER`
//! (`remove_blank_text=True`), then serialised via lxml `tostring(...,
//! pretty_print=True, encoding="unicode").strip()`. The body text inside the
//! tree is the SAME content the txt/json/csv gates compare, just embedded as
//! XML element text, so divergences should track those gates closely; if many
//! NEW ones appear, suspect a serialiser/harness bug before a real divergence.
//!
//! # Fingerprint-attribute reconciliation (the one xml-specific wrinkle)
//!
//! For EVERY non-TXT format (xml included), `core.py:480-485` sets
//! `document.fingerprint = content_fingerprint(title + " " + raw_text)`
//! UNCONDITIONALLY — even when `with_metadata=False` and the `Document` is
//! otherwise empty. `add_xml_meta` (`xml.py:178-183`) then emits any truthy
//! `META_ATTRIBUTES` value as a `<doc>` attribute, so Python's root is always
//! `<doc fingerprint="…">` (and, were `record_id` non-None, an `id=` too — but
//! `extract` defaults `record_id=None`, so only `fingerprint` appears).
//!
//! `content_fingerprint` is `Simhash(content).to_hex()` = `hex(self.hash)[2:]`
//! — a blake2b-seeded 64-bit simhash rendered as lowercase hex, 1–16 chars.
//! mdrcel deliberately does NOT reproduce this value (no crypto dependency; it
//! emits no fingerprint at all → bare `<doc>`). This is the documented
//! divergence in `wrk_docs/m7-deferred/fingerprint-blake2b.md` (shared with the
//! csv gate's column-2 mask; that ADR has an "Also affects xml" note). The gate
//! SHAPE-CHECKS Python's `fingerprint` attribute value (well-formed
//! lowercase-hex, 1–16 chars) then NEUTRALISES that single attribute on BOTH
//! sides (strips it from the `<doc …>` start tag) before byte-comparing the
//! rest of the document.
//!
//! # Comparison shape
//!
//! Both sides emit a `str` (one pretty-printed XML document, lxml-`.strip()`ed
//! so there is no leading/trailing whitespace and no trailing newline). The
//! Python pipeline NFC-normalises (`core.py:98`); mdrcel's `extract_to_xml`
//! NFC-normalises too. The harness NFC-normalises both whole XML strings ONCE
//! MORE (belt-and-braces) then strict byte-compares the resulting UTF-8.
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

use mdrcel::{extract_to_xml, Options};
use unicode_normalization::UnicodeNormalization;

/// Fixtures where Python's `trafilatura.extract(output_format="xml")` is the
/// under-extractor or otherwise anti-inversion-violating in a corpus-specific
/// way. **Each entry MUST have a corresponding ADR** in
/// `wrk_docs/m7-allowlist/`. The XML body text is the same content the
/// txt/json/csv gates diff, so these five share their root cause with the
/// txt/json/csv/markdown gates — the divergence is format-independent
/// (selection/parser/decoding, not XML structure). Each cross-references the
/// EXISTING ADR rather than duplicating the analysis.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K — Python's bare_extraction returns empty on this
    // structurally-valid filing (upstream of the xml branch); mdrcel
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
    // ---- xml-structure-specific (Stage 4) — all anti-inversion-clean
    // html5ever-vs-lxml tree-construction divergences; the txt/json/csv/
    // markdown gates are GREEN on every one (content byte-identical, only the
    // XML tree SHAPE differs). ----
    // Workiva inline-XBRL filings — `<main><body><p>` (mdrcel) vs `<main><p>`
    // (Python): same html5ever-vs-lxml XBRL tree construction as the
    // allowlisted 683d5643. ADR: wrk_docs/m7-allowlist/xbrl-body-wrapper.md.
    "340e6571c584979a.html",
    "577e61856ca2770d.html",
    "9a1590d0917107a7.html",
    // First web page ever (CERN, 1991) — malformed uppercase pre-HTML5 markup;
    // html5ever vs lxml reconstruct the document body differently (`<main>
    // <body>` vs `<main><div>`). 2-char delta. ADR:
    // wrk_docs/m7-allowlist/74ef4dad.md.
    "74ef4dadd5f70cb5.html",
    // `<code>`/`<pre>` leading-newline — html5ever follows HTML5 §13.2.5
    // (drop the newline right after a pre/listing/textarea start tag); lxml in
    // XML mode keeps it. Same spec-compliant-mdrcel family as dc8ba3c0. ADR:
    // wrk_docs/m7-allowlist/39ca4af9.md.
    "39ca4af9befa0524.html",
    // Wikipedia infobox — one extra empty `<cell/>` (mdrcel=1, Python=0) from
    // html5ever-vs-lxml table construction; identical mechanism to 683d5643's
    // "single empty-cell drift". 7-char delta. ADR:
    // wrk_docs/m7-allowlist/8638632a.md.
    "8638632aa27b2f45.html",
];

/// Fixtures where **mdrcel** is the buggy side on the XML path — divergence
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
    // body text flows into the xml `<main>` element text, so the leak
    // re-appears here exactly as on the txt/json/csv paths. mdrcel is the
    // buggy side; a faithful fix needs a Unicode general-category facility
    // (new dependency / vendored table = supervisor-sign-off work), so it is
    // deferred. ADR: wrk_docs/m7-deferred/507b9cdb.md (shared with the
    // txt/json/csv gates; that ADR has an "also affects xml" note). NOTE: the
    // fingerprint attribute is still neutralised for this fixture before the
    // U+2063 body divergence is observed — the two are independent.
    "507b9cdbe036bf58.html",
];

/// All 51 corpus snapshots — copied verbatim from the txt / json / csv gate.
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
fn trafilatura_xml_gate() {
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
            "M7 Stage 4 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust xml output.
        let rust_xml_raw = match extract_to_xml(&html, None, &Options::default()) {
            Ok(s) => s,
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_xml returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python xml output (subprocess oracle).
        let python_xml_raw = match python_xml(&path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M7 STAGE 4 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces).
        let rust_nfc: String = rust_xml_raw.as_str().nfc().collect();
        let python_nfc: String = python_xml_raw.as_str().nfc().collect();

        // 4. Neutralise the deliberate blake2b-vs-(absent) fingerprint
        //    attribute on the `<doc>` root (ADR
        //    wrk_docs/m7-deferred/fingerprint-blake2b.md). Shape-check Python's
        //    value where present, then strip the `fingerprint="…"` attribute
        //    from BOTH `<doc …>` start tags so the rest of the document is still
        //    compared byte-for-byte. When Python under-extracted (no `<doc>`
        //    root at all — an allowlist case like 41d2afac) the strip is a
        //    no-op and the raw NFC strings flow to the divergence triage below.
        let (rust_xml, python_xml) = neutralise_fingerprint(&rust_nfc, &python_nfc, fixture_rel);

        if rust_xml == python_xml {
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
            "M7 xml gate: fixture {basename} appears in BOTH allowlist and deferred lists; \
             pick one — allowlist = anti-inversion-clean Python bug, deferred = mdrcel defect",
        );

        let bucket = classify(&rust_xml, &python_xml);
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

        let first_diff_byte = first_diff_index(rust_xml.as_bytes(), python_xml.as_bytes());
        let rust_window = window_around(&rust_xml, first_diff_byte, 100);
        let python_window = window_around(&python_xml, first_diff_byte, 100);

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
            rust_xml.chars().count(),
            python_xml.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M7 xml corpus gate verdict (BLOCKER) ===");
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
        "M7 xml gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug}, \
         deferred={deferred_known_defect} sum to {accounted} but total={total}",
    );

    // BLOCKER gate: GREEN when every fixture is pass + allowlist + deferred.
    if pass + allowlist_python_bug + deferred_known_defect != total {
        panic!(
            "M7 xml gate divergence: {pass}/{total} substantive + \
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

/// Strip the deliberate blake2b-vs-(absent) `fingerprint` attribute from the
/// `<doc …>` root start tag on BOTH sides so the rest of the document compares
/// byte-for-byte.
///
/// Background (ADR `wrk_docs/m7-deferred/fingerprint-blake2b.md`): for every
/// non-TXT format `core.py:480-485` sets `document.fingerprint =
/// content_fingerprint(...)` UNCONDITIONALLY, and `add_xml_meta`
/// (`xml.py:178-183`) emits it as a `<doc>` attribute. With `with_metadata=False`
/// it is the ONLY attribute on the root (`record_id` defaults to `None`).
/// mdrcel emits no fingerprint (no crypto dependency) → bare `<doc>`.
///
/// Steps:
/// 1. SHAPE-CHECK Python's `fingerprint` attribute value where present: it MUST
///    be a well-formed lowercase-hex simhash (`hex(self.hash)[2:]`, 1–16 chars).
///    A malformed value PANICS — the neutralisation must never hide a real
///    structural divergence.
/// 2. Strip the `fingerprint="…"` attribute from the FIRST `<doc …>` start tag
///    on each side (only the root carries it; `clean_attributes` wipes it from
///    everything else, and mdrcel never emits it anywhere). When a side has no
///    `<doc>` root (e.g. Python under-extracted to an empty string) the strip is
///    a no-op for that side, so the caller's divergence triage still routes the
///    fixture to the allowlist / deferred lists.
fn neutralise_fingerprint(rust: &str, python: &str, fixture_rel: &str) -> (String, String) {
    if let Some(fp) = doc_fingerprint_value(python) {
        assert!(
            is_well_formed_fingerprint(&fp),
            "M7 xml gate: python <doc> fingerprint attribute on {fixture_rel} \
             is not a well-formed lowercase-hex simhash (1-16 chars): {fp:?} — the \
             neutralisation must not paper over a structurally-malformed fingerprint",
        );
    }
    (strip_doc_fingerprint(rust), strip_doc_fingerprint(python))
}

/// Extract the value of the `fingerprint="…"` attribute from the first
/// `<doc …>` start tag, or `None` if there is no such root / attribute.
fn doc_fingerprint_value(s: &str) -> Option<String> {
    let tag = doc_start_tag(s)?;
    let key = "fingerprint=\"";
    let start = tag.find(key)? + key.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Remove the `fingerprint="…"` attribute (and the single space that precedes
/// it) from the first `<doc …>` start tag. Idempotent and a no-op when the
/// attribute / root is absent.
fn strip_doc_fingerprint(s: &str) -> String {
    let Some(tag) = doc_start_tag(s) else {
        return s.to_string();
    };
    let key = "fingerprint=\"";
    let Some(attr_pos) = tag.find(key) else {
        return s.to_string();
    };
    // Find end of the quoted value.
    let val_start = attr_pos + key.len();
    let Some(rel_end) = tag[val_start..].find('"') else {
        return s.to_string();
    };
    let attr_end = val_start + rel_end + 1; // include closing quote
    // Eat one leading space if present (attrs are space-separated after `<doc`).
    let strip_start = if attr_pos > 0 && tag.as_bytes()[attr_pos - 1] == b' ' {
        attr_pos - 1
    } else {
        attr_pos
    };
    let new_tag = format!("{}{}", &tag[..strip_start], &tag[attr_end..]);
    // Replace the first occurrence of the original tag with the stripped one.
    s.replacen(tag, &new_tag, 1)
}

/// Return the first `<doc …>` start tag substring (from `<doc` to the matching
/// `>`), or `None` if the string does not start such a tag. Honours the two
/// shapes mdrcel/Python emit: bare `<doc>` and `<doc attr="…">`. (lxml never
/// puts `>` inside an attribute value — it escapes it to `&gt;` — so a simple
/// scan to the first `>` is safe for the root tag.)
fn doc_start_tag(s: &str) -> Option<&str> {
    let start = s.find("<doc")?;
    // Guard against `<document` etc.: the char after `<doc` must be `>` or
    // whitespace.
    let after = s.as_bytes().get(start + 4)?;
    if *after != b'>' && !after.is_ascii_whitespace() {
        return None;
    }
    let rest = &s[start..];
    let end = rest.find('>')? + 1;
    Some(&rest[..end])
}

/// True when `s` is a non-empty lowercase-hex string of length 1–16 — the
/// shape of Python's `Simhash.to_hex()` (`hex(self.hash)[2:]`; leading zeros
/// stripped, so 1–16 hex digits for a 64-bit simhash).
fn is_well_formed_fingerprint(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 16
        && s.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Python oracle path: spawn `run.py --xml` and read its stdout as the XML
/// payload. Bypasses the venv re-exec via `MDRCEL_TRAFILATURA_REEXECED=1`
/// (same trick as the txt / json / csv / markdown gates).
fn python_xml(snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--xml")
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

/// The `<doc>` start-tag scanner finds bare and attributed roots, and rejects
/// look-alike tags (`<document>`).
#[test]
fn doc_start_tag_variants() {
    assert_eq!(doc_start_tag("<doc>\n  <main/>"), Some("<doc>"));
    assert_eq!(
        doc_start_tag("<doc fingerprint=\"abc\">\n  <main/>"),
        Some("<doc fingerprint=\"abc\">"),
    );
    assert_eq!(doc_start_tag("<document>x</document>"), None);
    assert_eq!(doc_start_tag("no root here"), None);
}

/// The fingerprint value is read out of the root start tag.
#[test]
fn doc_fingerprint_value_reads_root_attr() {
    assert_eq!(
        doc_fingerprint_value("<doc fingerprint=\"fbe8c3db32b3b7c2\">\n  <main/>\n</doc>"),
        Some("fbe8c3db32b3b7c2".to_string()),
    );
    assert_eq!(doc_fingerprint_value("<doc>\n  <main/>\n</doc>"), None);
}

/// Stripping the fingerprint attribute leaves a bare `<doc>` and is idempotent.
#[test]
fn strip_doc_fingerprint_neutralises_attr() {
    let py = "<doc fingerprint=\"fbe8c3db32b3b7c2\">\n  <main/>\n</doc>";
    assert_eq!(strip_doc_fingerprint(py), "<doc>\n  <main/>\n</doc>");
    // Idempotent: already-bare root unchanged.
    let bare = "<doc>\n  <main/>\n</doc>";
    assert_eq!(strip_doc_fingerprint(bare), bare);
}

/// End-to-end: a Python `<doc fingerprint=…>` and mdrcel's bare `<doc>` with an
/// otherwise identical body compare equal AFTER neutralisation, and the
/// shape-check accepts Python's value.
#[test]
fn neutralise_fingerprint_blanks_root_attr_and_compares_rest() {
    let rust = "<doc>\n  <main><p>Hello.</p></main>\n  <comments/>\n</doc>";
    let python =
        "<doc fingerprint=\"fbe8c3db32b3b7c2\">\n  <main><p>Hello.</p></main>\n  <comments/>\n</doc>";
    let (r, p) = neutralise_fingerprint(rust, python, "synthetic");
    assert_eq!(r, p, "roots must match once fingerprint attr is neutralised");
}

/// The shape-check is load-bearing: a malformed Python fingerprint makes the
/// neutralisation PANIC rather than paper over a structural divergence.
#[test]
#[should_panic(expected = "not a well-formed lowercase-hex simhash")]
fn neutralise_fingerprint_panics_on_malformed_python_fingerprint() {
    let rust = "<doc>\n  <main/>\n</doc>";
    let python = "<doc fingerprint=\"GARBAGE\">\n  <main/>\n</doc>";
    let _ = neutralise_fingerprint(rust, python, "synthetic");
}

/// When Python under-extracted to an empty string (no `<doc>` root), the strip
/// is a no-op and the originals flow through to the caller's allowlist /
/// deferred triage.
#[test]
fn neutralise_fingerprint_passes_through_when_python_empty() {
    let rust = "<doc>\n  <main><p>body</p></main>\n  <comments/>\n</doc>";
    let python = ""; // under-extraction
    let (r, p) = neutralise_fingerprint(rust, python, "synthetic");
    assert_eq!(r, rust, "rust unchanged when python has no root");
    assert_eq!(p, python, "python unchanged when it has no root");
}
