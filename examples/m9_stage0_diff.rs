//! M9 Stage 0 — throwaway differential diff harness (falsification spike).
//!
//! Reads `benchmark/fuzz/stage0/manifest.jsonl`, runs mdrcel's
//! `extract_to_txt/markdown/xml` over each corpus page (passing the page's own
//! source_url as base_url), NFC-normalises BOTH mdrcel output and the matching
//! Python-oracle output, byte-compares, and on mismatch records the first
//! differing byte, a ~120-char window on each side, a coarse `shape_class`, and
//! the nearest enclosing tag. Appends one JSON line per mismatch to
//! `divergences.jsonl` and prints a per-format summary plus distinct-signature
//! count.
//!
//! Run: `cargo run --release --example m9_stage0_diff`
//!
//! Self-contained: only depends on mdrcel + serde_json + unicode-normalization
//! (all regular deps). Does NOT import any tests/common module.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

const FORMATS: [&str; 3] = ["txt", "markdown", "xml"];

/// Strip the `<doc fingerprint="...">` attribute (the KNOWN blake2b/FNV
/// fingerprint locus) so the xml re-diff can reveal any DEEPER divergence the
/// byte-4 fingerprint mismatch would otherwise mask.
fn strip_fingerprint(s: &str) -> String {
    // Only affects the opening <doc ...> tag. Replace ` fingerprint="HEX"`
    // with nothing.
    if let Some(start) = s.find("<doc fingerprint=\"") {
        if let Some(close) = s[start..].find('>') {
            let head = &s[start..start + close];
            // head looks like: <doc fingerprint="abcd1234"
            if let Some(q1) = head.find('"') {
                if let Some(q2rel) = head[q1 + 1..].find('"') {
                    let q2 = q1 + 1 + q2rel;
                    // rebuild: "<doc" + rest after the closing quote
                    let rebuilt = format!("<doc{}", &head[q2 + 1..]);
                    let mut out = String::with_capacity(s.len());
                    out.push_str(&s[..start]);
                    out.push_str(&rebuilt);
                    out.push_str(&s[start + close..]);
                    return out;
                }
            }
        }
    }
    s.to_string()
}

fn stage0_dir() -> PathBuf {
    // examples/ run from the crate root → benchmark/fuzz/stage0
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("benchmark")
        .join("fuzz")
        .join("stage0")
}

fn nfc(s: &str) -> String {
    s.nfc().collect::<String>()
}

/// Snap a byte index to the nearest char boundary at or below `i`.
fn snap_down(s: &str, mut i: usize) -> usize {
    if i > s.len() {
        i = s.len();
    }
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Snap a byte index to the nearest char boundary at or above `i`.
fn snap_up(s: &str, mut i: usize) -> usize {
    if i > s.len() {
        i = s.len();
    }
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// ~120-char window (≈60 bytes either side, snapped to char boundaries) around
/// byte index `at`.
fn window(s: &str, at: usize) -> String {
    let start = snap_down(s, at.saturating_sub(60));
    let end = snap_up(s, (at + 60).min(s.len()));
    s[start..end].to_string()
}

/// First differing BYTE index between two strings (length of common prefix).
fn first_diff_byte(a: &str, b: &str) -> usize {
    let ab = a.as_bytes();
    let bb = b.as_bytes();
    let n = ab.len().min(bb.len());
    let mut i = 0;
    while i < n && ab[i] == bb[i] {
        i += 1;
    }
    i
}

/// Nearest enclosing tag name discernible from the oracle/mdrcel windows:
/// scan backwards from the diff for the last `<tagname` opener.
fn near_tag(win_a: &str, win_b: &str) -> String {
    for w in [win_a, win_b] {
        if let Some(t) = last_open_tag(w) {
            return t;
        }
    }
    String::new()
}

fn last_open_tag(w: &str) -> Option<String> {
    let bytes = w.as_bytes();
    let mut best: Option<String> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            // skip a leading '/' for close tags but still record the name
            if j < bytes.len() && bytes[j] == b'/' {
                j += 1;
            }
            let name_start = j;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'-' || bytes[j] == b'_')
            {
                j += 1;
            }
            if j > name_start {
                best = Some(w[name_start..j].to_string());
            }
        }
        i += 1;
    }
    best
}

/// Coarse divergence shape classification.
fn classify(oracle: &str, mdr: &str, at: usize) -> &'static str {
    if oracle.is_empty() != mdr.is_empty() {
        return "empty-vs-nonempty";
    }
    // chars around the diff on each side
    let oc = oracle[snap_down(oracle, at)..].chars().next();
    let mc = mdr[snap_down(mdr, at)..].chars().next();

    let is_ws = |c: Option<char>| c.is_some_and(|c| c.is_whitespace());
    let is_invisible = |c: Option<char>| {
        c.is_some_and(|c| {
            // Cf-category / zero-width / invisible separators commonly leaked.
            matches!(
                c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}'
                    | '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{FEFF}'
                    | '\u{00AD}' | '\u{200E}' | '\u{200F}'
            ) || c.is_control()
        })
    };

    if is_invisible(oc) || is_invisible(mc) {
        return "invisible-control-char";
    }
    if is_ws(oc) && is_ws(mc) {
        return "whitespace";
    }
    // structural cues based on the byte at the diff
    let ob = oracle.as_bytes().get(at).copied();
    let mb = mdr.as_bytes().get(at).copied();
    // inside an attribute? look back for '=' before the next '>' or '<'
    if in_attribute(oracle, at) || in_attribute(mdr, at) {
        return "attribute";
    }
    match (ob, mb) {
        (Some(b'<'), _) | (_, Some(b'<')) => {
            // one side opens/closes a tag the other doesn't
            "structural"
        }
        (Some(b'>'), _) | (_, Some(b'>')) => "structural",
        _ => {
            // length-only or whitespace-only?
            if is_ws(oc) || is_ws(mc) {
                "whitespace"
            } else {
                "text-content"
            }
        }
    }
}

/// Remove Cf-category / zero-width / soft-hyphen / BOM chars (the KNOWN-1
/// "invisible/Cf leak" class). Used to decide whether a divergence is FULLY
/// explained by that known class (strip both sides → equal) or whether a
/// genuine NEW residual remains.
fn strip_invisible(s: &str) -> String {
    s.chars()
        .filter(|c| {
            !matches!(
                *c,
                '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{2060}'
                    | '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}'
                    | '\u{FEFF}' | '\u{00AD}' | '\u{200E}' | '\u{200F}'
                    | '\u{2066}' | '\u{2067}' | '\u{2068}' | '\u{2069}'
            )
        })
        .collect()
}

/// Heuristic: is byte index `at` inside an HTML attribute value/region?
/// Scan back to the nearest `<` or `>`; if we hit `<` first and saw an `=`
/// after a tag name, we're plausibly in attribute territory.
fn in_attribute(s: &str, at: usize) -> bool {
    let bytes = s.as_bytes();
    let at = at.min(bytes.len());
    let mut i = at;
    let mut saw_eq = false;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b'>' => return false,
            b'<' => return saw_eq,
            b'=' => saw_eq = true,
            _ => {}
        }
    }
    false
}

fn main() {
    let dir = stage0_dir();
    let manifest_path = dir.join("manifest.jsonl");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read manifest {}: {e}", manifest_path.display()));

    let rows: Vec<Value> = manifest
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("parse manifest line"))
        .collect();

    let opts = readex::Options::default();

    let div_path = dir.join("divergences.jsonl");
    let mut div_out = fs::File::create(&div_path).expect("create divergences.jsonl");

    // Fingerprint-masked xml re-diff: reveals divergences hidden behind the
    // known byte-4 `<doc fingerprint="...">` mismatch.
    let div_masked_path = dir.join("divergences_xml_fpmasked.jsonl");
    let mut div_masked_out =
        fs::File::create(&div_masked_path).expect("create divergences_xml_fpmasked.jsonl");
    let mut xml_fpmasked_pages = 0usize;
    let mut xml_fpmasked_equal = 0usize;
    let mut xml_fpmasked_divergent = 0usize;
    let mut xml_fpmasked_sigs: std::collections::BTreeSet<(String, String)> =
        std::collections::BTreeSet::new();

    // per-format tallies
    let mut pages = [0usize; 3];
    let mut equal = [0usize; 3];
    let mut divergent = [0usize; 3];
    let mut signatures: std::collections::BTreeSet<(String, String, String)> =
        std::collections::BTreeSet::new();

    for row in &rows {
        let sha = row["sha"].as_str().expect("sha");
        let url = row["source_url"].as_str().unwrap_or("");
        let html_path = dir.join("corpus").join(format!("{sha}.html"));
        let html = match fs::read(&html_path) {
            Ok(b) => String::from_utf8_lossy(&b).into_owned(),
            Err(e) => {
                eprintln!("skip {sha}: read html: {e}");
                continue;
            }
        };
        let base = if url.is_empty() { None } else { Some(url) };

        for (fi, fmt) in FORMATS.iter().enumerate() {
            pages[fi] += 1;
            // Wrap the call in catch_unwind: a real-world page can trigger a
            // PANIC inside mdrcel (e.g. byte-index slicing of a multi-byte
            // char). That is itself a Stage-0 finding — we record it as a
            // `mdrcel-panic` divergence rather than aborting the whole spike.
            let html_ref = &html;
            let call = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match *fmt {
                "txt" => readex::extract_to_txt(html_ref, base, &opts),
                "markdown" => readex::extract_to_markdown(html_ref, base, &opts),
                "xml" => readex::extract_to_xml(html_ref, base, &opts),
                _ => unreachable!(),
            }));
            let panicked = call.is_err();
            let mdr_str = match call {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    // Treat an extraction error as an empty string for the diff
                    // (the oracle collapses None -> "").
                    eprintln!("{sha} {fmt}: mdrcel Err: {e:?}");
                    String::new()
                }
                Err(_) => {
                    eprintln!("{sha} {fmt}: mdrcel PANIC");
                    String::new()
                }
            };

            if panicked {
                // Record the panic as its own divergence signature so it shows
                // up in the report regardless of how the empty-string diff
                // classifies.
                divergent[fi] += 1;
                let oracle_path = dir.join("oracle").join(format!("{sha}.{fmt}.txt"));
                let oracle_raw = fs::read_to_string(&oracle_path).unwrap_or_default();
                let o = nfc(&oracle_raw);
                let ow = window(&o, 0);
                signatures.insert((fmt.to_string(), "mdrcel-panic".to_string(), String::new()));
                let rec = serde_json::json!({
                    "sha": sha,
                    "format": fmt,
                    "first_diff_byte": 0,
                    "shape_class": "mdrcel-panic",
                    "near_tag": "",
                    "known1_invisible_only": false,
                    "oracle_window": ow,
                    "mdrcel_window": "<PANIC: extraction aborted>",
                });
                writeln!(div_out, "{rec}").expect("write divergence");
                continue;
            }

            let oracle_path = dir.join("oracle").join(format!("{sha}.{fmt}.txt"));
            let oracle_raw = fs::read_to_string(&oracle_path).unwrap_or_default();

            let o = nfc(&oracle_raw);
            let m = nfc(&mdr_str);

            // Fingerprint-masked xml re-diff (xml only).
            if *fmt == "xml" {
                xml_fpmasked_pages += 1;
                let om = strip_fingerprint(&o);
                let mm = strip_fingerprint(&m);
                if om.as_bytes() == mm.as_bytes() {
                    xml_fpmasked_equal += 1;
                } else {
                    xml_fpmasked_divergent += 1;
                    let at = first_diff_byte(&om, &mm);
                    let ow = window(&om, at);
                    let mw = window(&mm, at);
                    let shape = classify(&om, &mm, at);
                    let tag = near_tag(&ow, &mw);
                    // Exact KNOWN-1 test on the FULL fp-masked strings.
                    let known1_only = strip_invisible(&om) == strip_invisible(&mm);
                    xml_fpmasked_sigs.insert((shape.to_string(), tag.clone()));
                    let rec = serde_json::json!({
                        "sha": sha,
                        "format": "xml_fpmasked",
                        "first_diff_byte": at,
                        "shape_class": shape,
                        "near_tag": tag,
                        "known1_invisible_only": known1_only,
                        "oracle_window": ow,
                        "mdrcel_window": mw,
                    });
                    writeln!(div_masked_out, "{rec}").expect("write masked divergence");
                }
            }

            if o.as_bytes() == m.as_bytes() {
                equal[fi] += 1;
                continue;
            }
            divergent[fi] += 1;

            let at = first_diff_byte(&o, &m);
            let ow = window(&o, at);
            let mw = window(&m, at);
            let shape = classify(&o, &m, at);
            let tag = near_tag(&ow, &mw);

            signatures.insert((fmt.to_string(), shape.to_string(), tag.clone()));

            // Exact KNOWN-1 test on the FULL strings: does removing invisible/
            // Cf chars from both sides make them byte-equal?
            let known1_only = strip_invisible(&o) == strip_invisible(&m);

            let rec = serde_json::json!({
                "sha": sha,
                "format": fmt,
                "first_diff_byte": at,
                "shape_class": shape,
                "near_tag": tag,
                "known1_invisible_only": known1_only,
                "oracle_window": ow,
                "mdrcel_window": mw,
            });
            writeln!(div_out, "{rec}").expect("write divergence");
        }
    }
    div_out.flush().ok();
    div_masked_out.flush().ok();

    println!("=== M9 Stage 0 diff summary ===");
    for (fi, fmt) in FORMATS.iter().enumerate() {
        println!(
            "{fmt:>13}: pages={} byte-equal={} divergent={}",
            pages[fi], equal[fi], divergent[fi]
        );
    }
    println!(
        "{:>13}: pages={} byte-equal={} divergent={}",
        "xml_fpmasked", xml_fpmasked_pages, xml_fpmasked_equal, xml_fpmasked_divergent
    );
    println!(
        "distinct (format, shape_class, near_tag) signatures: {}",
        signatures.len()
    );
    println!(
        "distinct xml_fpmasked (shape_class, near_tag) signatures: {}",
        xml_fpmasked_sigs.len()
    );
    println!("divergences written to: {}", div_path.display());
    println!("fp-masked xml divergences written to: {}", div_masked_path.display());
}
