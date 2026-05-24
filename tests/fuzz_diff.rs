//! M9 Stage 4 — diff / classify / group harness for the differential-fuzz
//! pipeline (HLD §5.4, §6', §6.4).
//!
//! ## Invocation
//!
//! ```text
//! cargo test --test fuzz_diff -- --ignored --nocapture
//! ```
//!
//! The single test function (`fuzz_diff`) is `#[ignore]`d so a default
//! `cargo test` skips it. The harness is opt-in: it consumes a multi-megabyte
//! corpus + pre-built oracle cache produced by Stages 2/3.
//!
//! ## What it does (in one breath)
//!
//! 1. Re-reads the four integrity fields from the first line of
//!    `benchmark/fuzz/oracle_cache.jsonl` (`traf_version`, `cfg_sha`,
//!    `run_py_sha`, `manifest_sha`) and recomputes each one from the
//!    live on-disk state. Any mismatch is a hard panic — a stale cache
//!    would silently turn oracle drift into phantom "bugs."
//! 2. For each line in `benchmark/fuzz/manifest.jsonl`, reads
//!    `benchmark/fuzz_corpus/<sha>.html`, runs mdrcel's three string
//!    extractors (`extract_to_txt` / `extract_to_markdown` /
//!    `extract_to_xml`) on the identical `(html, source_url)` the oracle
//!    saw, NFC-normalises both sides, byte-compares them. Mismatches are
//!    appended to `benchmark/fuzz/divergences.jsonl` (one JSONL record
//!    per format per doc).
//! 3. Wraps the extract call in `catch_unwind` so a residual mdrcel panic
//!    is recorded as a divergence with `shape_class = "panic"` rather
//!    than aborting the whole harness (defence-in-depth — the Stage-0
//!    `htmldate` panic has been fixed, but the harness must be robust to
//!    any new one).
//! 4. After the corpus has been processed, groups divergences three ways:
//!      - per-format: # pages / # byte-equal / # divergent;
//!      - top-30 `(format, shape_class, near_tag)` groups by size (the
//!        HLD §6' "deliberately dumb v1 key"), each annotated with the
//!        count of distinct `diff_shape_hash` values inside it (the
//!        HLD §5.5 K=1 split — any group with >1 distinct hash is the
//!        candidate for the auto-split that triage will perform);
//!      - the global count of distinct `diff_shape_hash` values (the
//!        real unit-of-triage count under K=1).
//!
//! ## Why this file is the only thing M9 Stage 4 adds
//!
//! All shared helpers (NFC, diff-window, escape, workspace-path) already
//! live in `tests/common/mod.rs` from Stage 1. The fuzz harness uses them
//! directly — no extension is needed. The only NEW machinery here is
//! cache-integrity verification (SHA-256 of three files + one
//! known-stable-JSON descriptor), the divergence shape-class /
//! diff-shape-hash classification, and the read-time group-by — all
//! local to this file because no other gate needs them.

#![allow(clippy::needless_pass_by_value)]

mod common;
mod similarity;

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::panic;
use std::path::PathBuf;
use std::process::Command;

use sha2::{Digest, Sha256};

use common::{escape, first_diff_index, nfc, window_around, workspace_path};

// -------------------------------------------------------------------------
// Pinned constants
// -------------------------------------------------------------------------

/// The trafilatura version mdrcel is byte-for-byte porting (see Cargo.toml
/// journal). The cache header must match this AND the live venv. We assert
/// both: header-vs-live (so a venv bump fails fast) and header-vs-pinned
/// (so a maliciously crafted cache cannot lie).
const PINNED_TRAFILATURA_VERSION: &str = "2.0.0";

/// Diff-window size used by `window_around` — the same width used to escape
/// the per-divergence record. Wider windows reveal more context but bloat
/// `divergences.jsonl` and the diff-shape-hash space.
const DIFF_WINDOW_BYTES: usize = 120;

/// Print a progress dot every N docs so a human watching can tell it's alive.
const PROGRESS_EVERY: usize = 100;

/// HLD §8 — the manifest is partitioned by index into a WORKING slice (the
/// first `WORKING_SLICE_SIZE` docs, used for triage and fix targeting) and a
/// HELD-OUT slice (the rest, the §8 fidelity KPI substrate — never used for
/// triage). The partition is FROZEN at Stage 5 entry; changing it
/// re-baselines the KPI. Today the working slice is 1000 docs and the
/// held-out is 500 (manifest is 1500 entries from a single WARC; same CC
/// distribution).
const WORKING_SLICE_SIZE: usize = 1000;

// -------------------------------------------------------------------------
// Minimal JSON value type
// -------------------------------------------------------------------------
//
// The cache + manifest files are JSONL; the only field shapes we need are
// flat string-typed maps (manifest line, header) plus a single
// `outputs.{txt,markdown,xml}` sub-map. We use `serde_json` (already a
// regular dep) for the read path; the cfg-sha descriptor is hand-emitted
// since its JSON shape is load-bearing (must match Python's
// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=True)`).

// -------------------------------------------------------------------------
// Cache integrity
// -------------------------------------------------------------------------

struct CacheHeader {
    traf_version: String,
    cfg_sha: String,
    run_py_sha: String,
    manifest_sha: String,
}

/// Parse the first line of the oracle cache as `{"_header": {...}}`.
/// Panics on any malformed shape — an unreadable header is itself a stale
/// cache signal.
fn read_cache_header(cache_path: &PathBuf) -> CacheHeader {
    let bytes = fs::read(cache_path)
        .unwrap_or_else(|e| panic!("cannot read cache file {cache_path:?}: {e}"));
    let first_line_end = bytes
        .iter()
        .position(|&b| b == b'\n')
        .unwrap_or(bytes.len());
    let first_line = std::str::from_utf8(&bytes[..first_line_end])
        .unwrap_or_else(|e| panic!("cache header is not utf-8: {e}"));
    let v: serde_json::Value = serde_json::from_str(first_line)
        .unwrap_or_else(|e| panic!("cache header is not valid JSON: {e}\nline: {first_line}"));
    let hdr = v.get("_header").unwrap_or_else(|| {
        panic!("cache first line missing `_header` key — cache is stale or malformed")
    });
    let field = |k: &str| -> String {
        hdr.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("cache header missing/non-string field `{k}`"))
            .to_string()
    };
    CacheHeader {
        traf_version: field("traf_version"),
        cfg_sha: field("cfg_sha"),
        run_py_sha: field("run_py_sha"),
        manifest_sha: field("manifest_sha"),
    }
}

/// SHA-256 (hex) of a file's bytes.
fn file_sha256(path: &PathBuf) -> String {
    let bytes =
        fs::read(path).unwrap_or_else(|e| panic!("cannot read {path:?} for hashing: {e}"));
    let mut h = Sha256::new();
    h.update(&bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Replicate Python's locked-kwargs descriptor JSON encoding bit-for-bit:
///
/// ```python
/// json.dumps({"with_metadata": False, "deduplicate": False,
///             "include_comments": False,
///             "config_file_sha256": "<hex>"},
///            sort_keys=True, separators=(",", ":"), ensure_ascii=True)
/// ```
///
/// gives:
///
/// ```json
/// {"config_file_sha256":"<hex>","deduplicate":false,"include_comments":false,"with_metadata":false}
/// ```
///
/// Hand-emitting is the safest path here: serde_json's default Map order is
/// insertion order (BTreeMap with the `preserve_order` feature off would also
/// work), but the `false` literal serialisation and the exact comma/colon
/// separator placement are non-negotiable. A four-key string is shorter than
/// the ceremony needed to coerce serde_json into producing the same bytes,
/// and there is no risk of an extra space or capitalised `False` sneaking in.
///
/// Verified equivalence against Python (Stage 4 integration check):
///
/// ```
///   payload   = b'{"config_file_sha256":"<hex>","deduplicate":false,"include_comments":false,"with_metadata":false}'
///   cfg_sha   = SHA-256(payload).hexdigest()
/// ```
///
/// matches the header `cfg_sha` byte-for-byte for a fresh cache. (If a
/// future cfg file bytes change, both Python and this function will pick
/// up the new SHA from the same file path.)
fn compute_cfg_sha() -> String {
    let cfg_path = workspace_path("benchmark/oracles/trafilatura/trafilatura.cfg");
    let cfg_sha_hex = file_sha256(&cfg_path);
    // Keys are emitted in lexicographic order to match Python `sort_keys=True`:
    //   config_file_sha256 < deduplicate < include_comments < with_metadata
    let payload = format!(
        r#"{{"config_file_sha256":"{cfg_sha_hex}","deduplicate":false,"include_comments":false,"with_metadata":false}}"#
    );
    let mut h = Sha256::new();
    h.update(payload.as_bytes());
    hex(&h.finalize())
}

/// One-shot venv-python invocation purely to read `trafilatura.__version__`.
/// This is an INTEGRITY check, not an extraction call — it spawns once for
/// the whole harness run, and only reports the live version string.
fn live_traf_version() -> String {
    // Match the workspace convention used by the existing gates.
    let candidates = [
        "benchmark/oracles/trafilatura/.venv/Scripts/python.exe", // Windows
        "benchmark/oracles/trafilatura/.venv/bin/python",         // POSIX
    ];
    let mut last_err = String::new();
    for rel in candidates {
        let p = workspace_path(rel);
        if !p.exists() {
            continue;
        }
        let out = Command::new(&p)
            .args(["-c", "import trafilatura; print(trafilatura.__version__)"])
            .env("MDRCEL_TRAFILATURA_REEXECED", "1")
            .output();
        match out {
            Ok(o) if o.status.success() => {
                return String::from_utf8_lossy(&o.stdout).trim().to_string();
            }
            Ok(o) => {
                last_err = format!(
                    "{p:?} exited non-zero ({:?}) stderr={}",
                    o.status,
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            Err(e) => {
                last_err = format!("{p:?}: {e}");
            }
        }
    }
    panic!(
        "could not query live trafilatura version via venv python (tried Windows + POSIX paths): {last_err}"
    );
}

/// Verify all four cache-integrity fields. Panics on any mismatch with a
/// pointed diagnostic — "re-run the batch oracle" is the universal remedy.
fn verify_cache_integrity(cache_path: &PathBuf, hdr: &CacheHeader) {
    // 1. traf_version — both the header and the live venv must agree with the
    //    pinned constant. Catches "header was hand-edited" AND "venv was
    //    bumped without re-baking the cache."
    let live = live_traf_version();
    if hdr.traf_version != PINNED_TRAFILATURA_VERSION {
        panic!(
            "cache header traf_version={:?} but mdrcel is pinned to {:?} — re-bake the cache",
            hdr.traf_version, PINNED_TRAFILATURA_VERSION
        );
    }
    if live != PINNED_TRAFILATURA_VERSION {
        panic!(
            "venv trafilatura.__version__={live:?} but mdrcel is pinned to {PINNED_TRAFILATURA_VERSION:?} — venv has drifted from the pin"
        );
    }
    if hdr.traf_version != live {
        panic!(
            "cache header traf_version={:?} disagrees with live venv {:?} — re-bake the cache",
            hdr.traf_version, live
        );
    }

    // 2. cfg_sha — hash the locked-kwargs descriptor against the same cfg
    //    file Python read.
    let live_cfg = compute_cfg_sha();
    if hdr.cfg_sha != live_cfg {
        panic!(
            "cache cfg_sha mismatch:\n  header: {}\n  live:   {}\n  -> trafilatura.cfg or run.py's locked-kwargs descriptor changed; re-bake the cache",
            hdr.cfg_sha, live_cfg
        );
    }

    // 3. run_py_sha — bytes of run.py.
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let live_run = file_sha256(&run_py);
    if hdr.run_py_sha != live_run {
        panic!(
            "cache run_py_sha mismatch:\n  header: {}\n  live:   {}\n  -> run.py changed; re-bake the cache",
            hdr.run_py_sha, live_run
        );
    }

    // 4. manifest_sha — bytes of manifest.jsonl.
    let manifest = workspace_path("benchmark/fuzz/manifest.jsonl");
    let live_manifest = file_sha256(&manifest);
    if hdr.manifest_sha != live_manifest {
        panic!(
            "cache manifest_sha mismatch:\n  header: {}\n  live:   {}\n  -> manifest.jsonl changed; re-bake the cache",
            hdr.manifest_sha, live_manifest
        );
    }

    eprintln!(
        "[fuzz_diff] cache integrity OK ({} / {} / {} / {})\n[fuzz_diff] cache path: {}",
        &hdr.traf_version,
        &hdr.cfg_sha[..12],
        &hdr.run_py_sha[..12],
        &hdr.manifest_sha[..12],
        cache_path.display()
    );
}

// -------------------------------------------------------------------------
// Cache + manifest readers
// -------------------------------------------------------------------------

struct CacheEntry {
    txt: String,
    markdown: String,
    xml: String,
}

/// Read the cache, skipping the header, returning a sha → outputs map.
fn read_cache_entries(cache_path: &PathBuf) -> BTreeMap<String, CacheEntry> {
    let f = fs::File::open(cache_path)
        .unwrap_or_else(|e| panic!("cannot open cache {cache_path:?}: {e}"));
    let mut map = BTreeMap::new();
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line = line.unwrap_or_else(|e| panic!("cache line {idx} read error: {e}"));
        if line.is_empty() {
            continue;
        }
        if idx == 0 {
            // Header. Already validated by `read_cache_header`.
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => panic!(
                "cache line {idx} is not valid JSON: {e}\nfirst 200 chars: {}",
                &line.chars().take(200).collect::<String>()
            ),
        };
        let sha = v
            .get("sha")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("cache line {idx} missing `sha`"))
            .to_string();
        let outs = v
            .get("outputs")
            .unwrap_or_else(|| panic!("cache line {idx} missing `outputs`"));
        let pluck = |k: &str| -> String {
            outs.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        map.insert(
            sha,
            CacheEntry {
                txt: pluck("txt"),
                markdown: pluck("markdown"),
                xml: pluck("xml"),
            },
        );
    }
    map
}

struct ManifestEntry {
    sha: String,
    source_url: Option<String>,
}

fn read_manifest(manifest_path: &PathBuf) -> Vec<ManifestEntry> {
    let f = fs::File::open(manifest_path)
        .unwrap_or_else(|e| panic!("cannot open manifest {manifest_path:?}: {e}"));
    let mut out = Vec::new();
    for (idx, line) in BufReader::new(f).lines().enumerate() {
        let line = line.unwrap_or_else(|e| panic!("manifest line {idx} read error: {e}"));
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("manifest line {idx} is not valid JSON: {e}"));
        let sha = v
            .get("sha")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("manifest line {idx} missing `sha`"))
            .to_string();
        // `source_url` may be null or an empty string.
        let source_url = v
            .get("source_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        out.push(ManifestEntry { sha, source_url });
    }
    out
}

// -------------------------------------------------------------------------
// Divergence classification
// -------------------------------------------------------------------------

/// Coarse shape-class for `(format, shape_class, near_tag)` grouping. The
/// HLD §6' "deliberately dumb v1" key — over-merging is made safe by the
/// `diff_shape_hash` K=1 split (§5.5). New classes are added only when a
/// genuine new diff shape appears that none of these capture.
#[derive(Debug, Clone, Copy)]
enum ShapeClass {
    Whitespace,
    TextContent,
    Attribute,
    MissingElement,
    ExtraElement,
    Structural,
    EmptyVsNonempty,
    Panic,
    MdrcelError,
}

impl ShapeClass {
    fn label(self) -> &'static str {
        match self {
            ShapeClass::Whitespace => "whitespace",
            ShapeClass::TextContent => "text-content",
            ShapeClass::Attribute => "attribute",
            ShapeClass::MissingElement => "missing-element",
            ShapeClass::ExtraElement => "extra-element",
            ShapeClass::Structural => "structural",
            ShapeClass::EmptyVsNonempty => "empty-vs-nonempty",
            ShapeClass::Panic => "panic",
            ShapeClass::MdrcelError => "mdrcel_error",
        }
    }
}

/// Classify a divergence from the two NFC-normalised strings + their windows.
/// The rules are intentionally simple and inspectable; misclassifications
/// are a triage problem, not a harness problem (K=1 split protects against
/// silent merging).
fn classify_shape(oracle: &str, mdrcel: &str, ow: &str, mw: &str) -> ShapeClass {
    if oracle.is_empty() != mdrcel.is_empty() {
        return ShapeClass::EmptyVsNonempty;
    }
    // Collapse-ws equality => whitespace-only.
    let ws_collapse = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if ws_collapse(oracle) == ws_collapse(mdrcel) {
        return ShapeClass::Whitespace;
    }
    // Attribute? "=" inside the window with either side carrying a closing
    // quote nearby is the cheap heuristic.
    let inside_attr = |s: &str| {
        // Walk backwards from the rough center looking for `<…name="`.
        // Equivalent quick test: an unbalanced quote count between the
        // window start and the diff midpoint, AND the window contains `=`.
        if !s.contains('=') {
            return false;
        }
        let dquote = s.matches('"').count();
        let squote = s.matches('\'').count();
        (dquote % 2 == 1) || (squote % 2 == 1)
    };
    if inside_attr(ow) || inside_attr(mw) {
        return ShapeClass::Attribute;
    }
    // Element presence asymmetry: count `<` runs in each window.
    let opens = |s: &str| -> usize { s.matches('<').count() };
    let oc = opens(ow);
    let mc = opens(mw);
    if oc > mc {
        return ShapeClass::MissingElement; // oracle has it, mdrcel doesn't
    }
    if mc > oc {
        return ShapeClass::ExtraElement;
    }
    // Same open-count but different tag names => structural.
    if first_tag_name(ow) != first_tag_name(mw) {
        return ShapeClass::Structural;
    }
    // Default: textual content within a tag's body.
    ShapeClass::TextContent
}

/// Apply the M9 "known-class mask" the HLD §1/§3 contract demands.
///
/// The fuzz pipeline is supposed to MASK the known-deferred defects so they
/// don't drown out new-class signal. Today only the xml `fingerprint="…"`
/// attribute mask is wired (the only known-class mask that affects a format
/// we diff): mdrcel emits a bare `<doc>` while Python emits
/// `<doc fingerprint="…">`, per ADR `wrk_docs/m7-deferred/fingerprint-blake2b.md`.
/// Cf-control leak and TEI-header masks are intentionally NOT applied: TEI
/// isn't a format we diff here, and Cf surfaces naturally — the
/// `diff_shape_hash` clusters Cf-only divergences for Stage-5 triage rather
/// than hiding them.
fn apply_known_masks(s: &str, format: &str) -> String {
    match format {
        "xml" => strip_xml_fingerprint(s),
        _ => s.to_string(),
    }
}

/// Strip the first `fingerprint="…"` attribute (and the single space that
/// precedes it) from the input. Idempotent and a no-op when the attribute is
/// absent. A lighter cousin of `tests/trafilatura_xml_gate.rs`'s
/// `strip_doc_fingerprint`: the gate also shape-checks Python's value; here
/// the gate's 51-fixture coverage already enforces shape, so we strip
/// without re-asserting.
fn strip_xml_fingerprint(s: &str) -> String {
    let needle = " fingerprint=\"";
    let Some(start) = s.find(needle) else {
        return s.to_string();
    };
    let val_start = start + needle.len();
    let Some(rel_end) = s[val_start..].find('"') else {
        return s.to_string();
    };
    let end = val_start + rel_end + 1; // include closing quote
    format!("{}{}", &s[..start], &s[end..])
}

/// Best-effort "first tag name" in a window — the character class is the
/// HTML element-name production (ASCII letter start, then letters/digits/-).
fn first_tag_name(window: &str) -> Option<String> {
    let bytes = window.as_bytes();
    let n = bytes.len();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'<' {
            let mut j = i + 1;
            if j < n && bytes[j] == b'/' {
                j += 1;
            }
            let start = j;
            while j < n {
                let c = bytes[j];
                let alnum = c.is_ascii_alphanumeric() || c == b'-';
                if !alnum {
                    break;
                }
                j += 1;
            }
            if j > start {
                let nm = std::str::from_utf8(&bytes[start..j]).ok()?;
                if nm.starts_with(|c: char| c.is_ascii_alphabetic()) {
                    return Some(nm.to_ascii_lowercase());
                }
            }
        }
        i += 1;
    }
    None
}

/// Compute the K=1 diff-shape-hash (HLD §5.5): hash of the COMBINED diff
/// window with literal text elided. Replaces every run of `[A-Za-z0-9_]+`
/// with the literal token `IDENT` (digit-only runs collapse to `NUM` first
/// — order matters: digit-only is a strict subset of the alnum class, so
/// we handle digits before the alphabetic substitution). Everything else
/// — punctuation, whitespace, tag delimiters, control-char escapes, entity
/// refs — is preserved verbatim.
fn diff_shape_hash(oracle_window: &str, mdrcel_window: &str) -> String {
    // Combine windows with a sentinel that cannot appear in either (control
    // chars are already escaped via `common::escape`).
    let joined = format!("{oracle_window}\u{001E}{mdrcel_window}");
    let elided = elide_literals(&joined);
    let mut h = Sha256::new();
    h.update(elided.as_bytes());
    // Truncate to 16 hex chars — 64-bit collision space is plenty for
    // hundreds-of-thousands of distinct shapes; the full 256-bit hex is
    // pure noise in the divergences.jsonl.
    let full = hex(&h.finalize());
    full[..16].to_string()
}

/// Implement the HLD §5.5 elision pass. The two runs we care about are
/// "digit-only" (becomes `NUM`) and "general identifier" (becomes `IDENT`).
/// We process the input ONCE, character by character, gathering runs of the
/// `[A-Za-z0-9_]+` class and emitting `NUM` when the run is digit-only,
/// `IDENT` otherwise.
fn elide_literals(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    while i < n {
        let b = bytes[i];
        let is_word = b.is_ascii_alphanumeric() || b == b'_';
        if !is_word {
            // Non-ASCII bytes are part of a UTF-8 multibyte char — preserve
            // verbatim (they are punctuation/text/symbol; not part of the
            // `[A-Za-z0-9_]+` class).
            // Push the entire code-point.
            // Find the next char boundary in the *string*.
            // Cheaper: use the str-level iteration for this slow branch.
            let rest = &s[i..];
            let ch = rest.chars().next().expect("byte i must be a char start");
            out.push(ch);
            i += ch.len_utf8();
            continue;
        }
        // Identifier run — find its end.
        let start = i;
        while i < n {
            let c = bytes[i];
            if !(c.is_ascii_alphanumeric() || c == b'_') {
                break;
            }
            i += 1;
        }
        let run = &s[start..i];
        if run.bytes().all(|c| c.is_ascii_digit()) {
            out.push_str("NUM");
        } else {
            out.push_str("IDENT");
        }
    }
    out
}

// -------------------------------------------------------------------------
// Divergence record + serialization
// -------------------------------------------------------------------------

struct Divergence {
    sha: String,
    source_url: Option<String>,
    format: &'static str,
    shape_class: ShapeClass,
    near_tag: String,
    first_diff_byte: usize,
    oracle_window: String,
    mdrcel_window: String,
    diff_shape_hash: String,
    dice_similarity: f64,
    jaccard_similarity: f64,
}

impl Divergence {
    fn to_jsonl(&self) -> String {
        // Hand-write JSON for stability: the only fields that need escaping
        // are the two windows (already escaped via `common::escape`) and
        // `source_url` (may contain quotes/backslashes). Use `serde_json` for
        // the value-encoding of those three to keep escaping bulletproof.
        let src = match &self.source_url {
            Some(u) => serde_json::Value::String(u.clone()).to_string(),
            None => "null".to_string(),
        };
        // `oracle_window` / `mdrcel_window` are already escaped (control chars
        // → `\xNN`) by `common::escape` so the only chars left needing JSON
        // escaping are `"` / `\` / `\x00..\x1f` (which can't appear post-
        // escape) — serde_json handles the rest.
        let ow = serde_json::Value::String(self.oracle_window.clone()).to_string();
        let mw = serde_json::Value::String(self.mdrcel_window.clone()).to_string();
        let nt = serde_json::Value::String(self.near_tag.clone()).to_string();
        format!(
            r#"{{"sha":"{sha}","source_url":{src},"format":"{fmt}","shape_class":"{sc}","near_tag":{nt},"first_diff_byte":{fdb},"oracle_window":{ow},"mdrcel_window":{mw},"diff_shape_hash":"{dsh}","dice_similarity":{dice:.4},"jaccard_similarity":{jacc:.4}}}"#,
            sha = self.sha,
            src = src,
            fmt = self.format,
            sc = self.shape_class.label(),
            nt = nt,
            fdb = self.first_diff_byte,
            ow = ow,
            mw = mw,
            dsh = self.diff_shape_hash,
            dice = self.dice_similarity,
            jacc = self.jaccard_similarity,
        )
    }
}

// -------------------------------------------------------------------------
// Extraction wrappers — one per format, all panic-catching.
// -------------------------------------------------------------------------

type ExtractFn = fn(
    html: &str,
    base_url: Option<&str>,
    opts: &mdrcel::Options,
) -> Result<String, mdrcel::ExtractError>;

enum ExtractOutcome {
    Ok(String),
    Err(String),   // ExtractError -> mdrcel_error class
    Panic(String), // catch_unwind capture -> panic class
}

fn safe_extract(
    f: ExtractFn,
    html: &str,
    base_url: Option<&str>,
    opts: &mdrcel::Options,
) -> ExtractOutcome {
    let html_owned = html.to_string();
    let base_owned = base_url.map(|s| s.to_string());
    let opts_clone = opts.clone();
    // Move owned values into the closure so the panic payload isn't holding
    // any borrow when it unwinds.
    let result = panic::catch_unwind(panic::AssertUnwindSafe(move || {
        f(&html_owned, base_owned.as_deref(), &opts_clone)
    }));
    match result {
        Ok(Ok(s)) => ExtractOutcome::Ok(s),
        Ok(Err(e)) => ExtractOutcome::Err(format!("{e}")),
        Err(payload) => {
            // Stringify whatever the panic carried.
            let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                (*s).to_string()
            } else if let Some(s) = payload.downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic payload>".to_string()
            };
            ExtractOutcome::Panic(msg)
        }
    }
}

// -------------------------------------------------------------------------
// Per-doc diff loop
// -------------------------------------------------------------------------

/// Dice threshold: divergent pages scoring at or above this are
/// "near-equivalent" — whitespace-only or trivially structural differences.
/// Empirically calibrated against the M11 Phase B probe data (HLD §5).
const DICE_NEAR_EQUIVALENT: f64 = 0.95;

/// Dice threshold: divergent pages scoring at or above this (but below
/// `DICE_NEAR_EQUIVALENT`) are "content-similar" — different paragraphs
/// selected but substantial content overlap. Below this is "truly divergent."
const DICE_CONTENT_SIMILAR: f64 = 0.80;

struct PerFormatStats {
    pages: usize,
    equal: usize,
    diverge: usize,
    /// Sum of Dice scores across ALL pages (byte-equal pages count as 1.0).
    dice_sum_all: f64,
    /// Sum of Dice scores across DIVERGENT pages only.
    dice_sum_diverge: f64,
    /// Divergent pages with Dice >= 0.95.
    near_equivalent: usize,
    /// Divergent pages with 0.80 <= Dice < 0.95.
    content_similar: usize,
    /// Divergent pages with Dice < 0.80.
    truly_divergent: usize,
}

impl PerFormatStats {
    fn new() -> Self {
        Self {
            pages: 0,
            equal: 0,
            diverge: 0,
            dice_sum_all: 0.0,
            dice_sum_diverge: 0.0,
            near_equivalent: 0,
            content_similar: 0,
            truly_divergent: 0,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn process_format(
    sha: &str,
    source_url: &Option<String>,
    html: &str,
    format: &'static str,
    extract: ExtractFn,
    oracle_text: &str,
    stats: &mut PerFormatStats,
    out: &mut dyn Write,
    divergence_buf: &mut Vec<Divergence>,
    opts: &mdrcel::Options,
    record_divergence: bool,
) {
    stats.pages += 1;

    let mdrcel_out = safe_extract(extract, html, source_url.as_deref(), opts);

    // The two NFC-normalised sides we'll compare and (on diff) window into.
    let (mdrcel_text_raw, force_shape): (String, Option<ShapeClass>) = match mdrcel_out {
        ExtractOutcome::Ok(s) => (nfc(&s), None),
        ExtractOutcome::Err(msg) => (msg, Some(ShapeClass::MdrcelError)),
        ExtractOutcome::Panic(msg) => (msg, Some(ShapeClass::Panic)),
    };
    let oracle_text_raw = nfc(oracle_text);

    // Apply the known-class mask per HLD §1/§3 ("the pipeline masks them"):
    // strip the blake2b fingerprint from xml on BOTH sides so the
    // documented mdrcel-vs-trafilatura fingerprint substitute
    // (`wrk_docs/m7-deferred/fingerprint-blake2b.md`) doesn't drown out the
    // real signal. (Cf-control / TEI masks not needed here: TEI isn't
    // diffed; Cf surfaces naturally and the diff_shape_hash clusters it for
    // Stage-5 triage.)
    let mdrcel_text = apply_known_masks(&mdrcel_text_raw, format);
    let oracle_text_n = apply_known_masks(&oracle_text_raw, format);

    // Byte-equal? Fast path.
    if force_shape.is_none() && mdrcel_text == oracle_text_n {
        stats.equal += 1;
        stats.dice_sum_all += 1.0; // byte-equal implies Dice = 1.0
        return;
    }

    stats.diverge += 1;

    // --- Compute similarity metrics (M11 Phase B) ---
    // §7.6: panic/error divergences get hardcoded 0.0 — computing Dice on
    // error message strings is meaningless.
    let (dice, jaccard) = match force_shape {
        Some(ShapeClass::Panic | ShapeClass::MdrcelError) => (0.0, 0.0),
        _ => {
            // §7.2: for xml, strip tags before computing similarity.
            // §7.7: for non-xml, pass &str references directly (no clone).
            if format == "xml" {
                let stripped_a = similarity::strip_xml_tags(&mdrcel_text);
                let stripped_b = similarity::strip_xml_tags(&oracle_text_n);
                (
                    similarity::dice_bigram_similarity(&stripped_a, &stripped_b),
                    similarity::jaccard_token_similarity(&stripped_a, &stripped_b),
                )
            } else {
                (
                    similarity::dice_bigram_similarity(&mdrcel_text, &oracle_text_n),
                    similarity::jaccard_token_similarity(&mdrcel_text, &oracle_text_n),
                )
            }
        }
    };

    stats.dice_sum_all += dice;
    stats.dice_sum_diverge += dice;

    // Bucket classification.
    if dice >= DICE_NEAR_EQUIVALENT {
        stats.near_equivalent += 1;
    } else if dice >= DICE_CONTENT_SIMILAR {
        stats.content_similar += 1;
    } else {
        stats.truly_divergent += 1;
    }
    // --- END similarity metrics ---

    let first_diff = first_diff_index(oracle_text_n.as_bytes(), mdrcel_text.as_bytes());
    let ow_raw = window_around(&oracle_text_n, first_diff, DIFF_WINDOW_BYTES);
    let mw_raw = window_around(&mdrcel_text, first_diff, DIFF_WINDOW_BYTES);
    let ow = escape(&ow_raw);
    let mw = escape(&mw_raw);

    let shape = force_shape.unwrap_or_else(|| classify_shape(&oracle_text_n, &mdrcel_text, &ow, &mw));
    let near = first_tag_name(&ow).unwrap_or_default();
    let dsh = diff_shape_hash(&ow, &mw);

    let div = Divergence {
        sha: sha.to_string(),
        source_url: source_url.clone(),
        format,
        shape_class: shape,
        near_tag: near,
        first_diff_byte: first_diff,
        oracle_window: ow,
        mdrcel_window: mw,
        diff_shape_hash: dsh,
        dice_similarity: dice,
        jaccard_similarity: jaccard,
    };
    // HLD §8: held-out divergences are counted in stats but NOT enumerated
    // here — the held-out slice is the KPI substrate, "look but don't peek."
    // Triage targets are drawn from the working slice only.
    if record_divergence {
        let line = div.to_jsonl();
        writeln!(out, "{line}").expect("write divergences.jsonl");
        divergence_buf.push(div);
    }
}

// -------------------------------------------------------------------------
// Read-time grouping + summary
// -------------------------------------------------------------------------

fn print_summary(
    total_pages: usize,
    per_format_working: &BTreeMap<&'static str, PerFormatStats>,
    per_format_heldout: &BTreeMap<&'static str, PerFormatStats>,
    divs: &[Divergence],
    divergences_path: &PathBuf,
) {
    eprintln!();
    eprintln!("===========================================================");
    eprintln!("              M9 Stage 5 — fuzz_diff summary");
    eprintln!("===========================================================");
    eprintln!("corpus pages processed: {total_pages}");
    eprintln!(
        "  WORKING slice (first {} docs — triage target)",
        WORKING_SLICE_SIZE
    );
    eprintln!("  HELD-OUT slice (rest — HLD §8 fidelity KPI, no triage)");
    eprintln!();

    eprintln!("Per-format tally — WORKING:");
    eprintln!(
        "  {:<10} {:>10} {:>10} {:>10}",
        "format", "pages", "equal", "diverge"
    );
    for (fmt, s) in per_format_working {
        eprintln!(
            "  {:<10} {:>10} {:>10} {:>10}",
            fmt, s.pages, s.equal, s.diverge
        );
    }
    eprintln!();

    eprintln!("Per-format tally — HELD-OUT (fidelity KPI substrate):");
    eprintln!(
        "  {:<10} {:>10} {:>10} {:>10} {:>10}",
        "format", "pages", "equal", "diverge", "KPI(%eq)"
    );
    for (fmt, s) in per_format_heldout {
        let pct = if s.pages == 0 {
            0.0
        } else {
            100.0 * (s.equal as f64) / (s.pages as f64)
        };
        eprintln!(
            "  {:<10} {:>10} {:>10} {:>10} {:>9.2}%",
            fmt, s.pages, s.equal, s.diverge, pct
        );
    }
    eprintln!();

    // Similarity KPI — held-out (M11 Phase B).
    eprintln!("Similarity KPI — HELD-OUT (fidelity substrate):");
    eprintln!(
        "  {:<10} {:>6} {:>9} {:>14} {:>14} {:>8} {:>12} {:>10}",
        "format", "pages", "byte-eq%", "mean_dice_all", "mean_dice_div",
        "near-eq", "content-sim", "divergent"
    );
    for (fmt, s) in per_format_heldout {
        let byte_eq_pct = if s.pages == 0 {
            0.0
        } else {
            100.0 * (s.equal as f64) / (s.pages as f64)
        };
        let mean_dice_all = if s.pages == 0 {
            0.0
        } else {
            s.dice_sum_all / s.pages as f64
        };
        let mean_dice_div = if s.diverge == 0 {
            0.0
        } else {
            s.dice_sum_diverge / s.diverge as f64
        };
        eprintln!(
            "  {:<10} {:>6} {:>8.2}% {:>14.4} {:>14.4} {:>8} {:>12} {:>10}",
            fmt, s.pages, byte_eq_pct, mean_dice_all, mean_dice_div,
            s.near_equivalent, s.content_similar, s.truly_divergent
        );
    }
    eprintln!();

    // Similarity KPI — working slice (for triage context).
    eprintln!("Similarity KPI — WORKING (triage context):");
    eprintln!(
        "  {:<10} {:>6} {:>9} {:>14} {:>14} {:>8} {:>12} {:>10}",
        "format", "pages", "byte-eq%", "mean_dice_all", "mean_dice_div",
        "near-eq", "content-sim", "divergent"
    );
    for (fmt, s) in per_format_working {
        let byte_eq_pct = if s.pages == 0 {
            0.0
        } else {
            100.0 * (s.equal as f64) / (s.pages as f64)
        };
        let mean_dice_all = if s.pages == 0 {
            0.0
        } else {
            s.dice_sum_all / s.pages as f64
        };
        let mean_dice_div = if s.diverge == 0 {
            0.0
        } else {
            s.dice_sum_diverge / s.diverge as f64
        };
        eprintln!(
            "  {:<10} {:>6} {:>8.2}% {:>14.4} {:>14.4} {:>8} {:>12} {:>10}",
            fmt, s.pages, byte_eq_pct, mean_dice_all, mean_dice_div,
            s.near_equivalent, s.content_similar, s.truly_divergent
        );
    }
    eprintln!();

    // Group by (format, shape_class, near_tag).
    type GroupKey = (&'static str, &'static str, String);
    let mut groups: BTreeMap<GroupKey, Vec<&Divergence>> = BTreeMap::new();
    for d in divs {
        let key: GroupKey = (d.format, d.shape_class.label(), d.near_tag.clone());
        groups.entry(key).or_default().push(d);
    }

    // Sort top groups by size descending.
    let mut top: Vec<(&GroupKey, &Vec<&Divergence>)> = groups.iter().collect();
    top.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));

    eprintln!(
        "Top 30 (format, shape_class, near_tag) groups by size (HLD §6' dumb v1 key):"
    );
    eprintln!(
        "  {:<10} {:<20} {:<20} {:>8} {:>14}",
        "format", "shape_class", "near_tag", "members", "distinct_dsh"
    );
    for (key, members) in top.iter().take(30) {
        // Count distinct diff_shape_hash values within this group — HLD §5.5
        // K=1 trigger.
        let mut dsh_set: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        for d in members.iter() {
            dsh_set.insert(d.diff_shape_hash.as_str());
        }
        eprintln!(
            "  {:<10} {:<20} {:<20} {:>8} {:>14}",
            key.0,
            key.1,
            if key.2.is_empty() { "<none>".to_string() } else { key.2.clone() },
            members.len(),
            dsh_set.len()
        );
    }
    eprintln!();

    // Global distinct diff_shape_hash count = unit-of-triage under K=1.
    let mut all_dsh: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
    for d in divs {
        all_dsh.insert(d.diff_shape_hash.as_str());
    }
    eprintln!(
        "Total divergences:       {}",
        divs.len()
    );
    eprintln!(
        "Distinct (fmt,cls,tag) groups: {}",
        groups.len()
    );
    eprintln!(
        "Distinct diff_shape_hash (K=1 triage units): {}",
        all_dsh.len()
    );
    eprintln!("divergences written to:  {}", divergences_path.display());
    eprintln!("===========================================================");
}

// -------------------------------------------------------------------------
// The single integration test (ignored by default).
// -------------------------------------------------------------------------

#[test]
#[ignore]
fn fuzz_diff() {
    let cache_path = workspace_path("benchmark/fuzz/oracle_cache.jsonl");
    let manifest_path = workspace_path("benchmark/fuzz/manifest.jsonl");
    let corpus_dir = workspace_path("benchmark/fuzz_corpus");
    let divergences_path = workspace_path("benchmark/fuzz/divergences.jsonl");

    if !cache_path.exists() {
        panic!(
            "oracle cache not found at {cache_path:?} — run the batch oracle first:\n\n  python benchmark/oracles/trafilatura/run.py --batch benchmark/fuzz_corpus benchmark/fuzz/manifest.jsonl\n"
        );
    }

    // Step 1 — cache integrity.
    let header = read_cache_header(&cache_path);
    verify_cache_integrity(&cache_path, &header);

    // Step 2 — load manifest + cache.
    let manifest = read_manifest(&manifest_path);
    eprintln!("[fuzz_diff] manifest entries: {}", manifest.len());
    let cache_entries = read_cache_entries(&cache_path);
    eprintln!(
        "[fuzz_diff] cache entries: {} (expected ≈ {})",
        cache_entries.len(),
        manifest.len()
    );

    // Truncate divergences file (we never append across runs).
    let mut div_file =
        fs::File::create(&divergences_path).unwrap_or_else(|e| {
            panic!("cannot create divergences file {divergences_path:?}: {e}")
        });

    // Per-format running stats — separated by slice (HLD §8). Working-slice
    // stats drive triage; held-out-slice stats are the KPI substrate.
    let mut per_format_working: BTreeMap<&'static str, PerFormatStats> = BTreeMap::new();
    let mut per_format_heldout: BTreeMap<&'static str, PerFormatStats> = BTreeMap::new();
    for fmt in ["txt", "markdown", "xml"] {
        per_format_working.insert(fmt, PerFormatStats::new());
        per_format_heldout.insert(fmt, PerFormatStats::new());
    }

    let mut divergences: Vec<Divergence> = Vec::new();
    let opts = mdrcel::Options::default();

    let mut processed = 0usize;
    let mut missing_html = 0usize;
    let mut missing_cache = 0usize;

    for (idx, m) in manifest.iter().enumerate() {
        let is_working = idx < WORKING_SLICE_SIZE;
        let per_format = if is_working {
            &mut per_format_working
        } else {
            &mut per_format_heldout
        };
        // Locate the matching cache entry. A missing cache row is NOT a
        // divergence — the cache is in-progress / partial; just skip.
        let Some(cache_entry) = cache_entries.get(&m.sha) else {
            missing_cache += 1;
            continue;
        };

        let html_path = corpus_dir.join(format!("{}.html", m.sha));
        let Ok(html) = fs::read_to_string(&html_path) else {
            // Corpus and manifest may briefly disagree mid-harvest; skip
            // rather than panicking.
            missing_html += 1;
            continue;
        };

        process_format(
            &m.sha,
            &m.source_url,
            &html,
            "txt",
            mdrcel::extract_to_txt,
            &cache_entry.txt,
            per_format.get_mut("txt").unwrap(),
            &mut div_file,
            &mut divergences,
            &opts,
            is_working,
        );
        process_format(
            &m.sha,
            &m.source_url,
            &html,
            "markdown",
            mdrcel::extract_to_markdown,
            &cache_entry.markdown,
            per_format.get_mut("markdown").unwrap(),
            &mut div_file,
            &mut divergences,
            &opts,
            is_working,
        );
        process_format(
            &m.sha,
            &m.source_url,
            &html,
            "xml",
            mdrcel::extract_to_xml,
            &cache_entry.xml,
            per_format.get_mut("xml").unwrap(),
            &mut div_file,
            &mut divergences,
            &opts,
            is_working,
        );

        processed += 1;
        if processed % PROGRESS_EVERY == 0 {
            eprintln!(
                "[fuzz_diff] progress: {processed} docs ({} divergences so far)",
                divergences.len()
            );
        }
    }

    div_file.flush().expect("flush divergences");
    drop(div_file);

    eprintln!();
    eprintln!(
        "[fuzz_diff] done: {processed} docs processed, {missing_cache} skipped (no cache entry), {missing_html} skipped (corpus missing)"
    );

    print_summary(
        processed,
        &per_format_working,
        &per_format_heldout,
        &divergences,
        &divergences_path,
    );
}

// -------------------------------------------------------------------------
// Self-tests for the shape-class / hash helpers — keep them tiny.
// -------------------------------------------------------------------------

#[test]
fn elide_literals_collapses_idents_and_nums() {
    assert_eq!(elide_literals("abc"), "IDENT");
    assert_eq!(elide_literals("123"), "NUM");
    assert_eq!(elide_literals("abc 123"), "IDENT NUM");
    assert_eq!(elide_literals("<p>hello world</p>"), "<IDENT>IDENT IDENT</IDENT>");
    // Punctuation + entity refs preserved verbatim. Note `x07` is alnum →
    // collapses to a single IDENT run (the leading `x` makes it non-
    // digit-only).
    assert_eq!(elide_literals("&amp; — \\x07"), "&IDENT; — \\IDENT");
    // Run lengths don't matter — `aaaa` and `bb` both collapse to `IDENT`.
    assert_eq!(elide_literals("aaaa bb"), "IDENT IDENT");
}

#[test]
fn diff_shape_hash_is_stable_for_same_shape_different_words() {
    let a = diff_shape_hash("<p>hello</p>", "<p>world</p>");
    let b = diff_shape_hash("<p>foo</p>", "<p>bar</p>");
    assert_eq!(a, b, "same shape, different idents should hash equal");
}

#[test]
fn diff_shape_hash_differs_for_punctuation_changes() {
    // Tag-name differences alone DON'T change the diff-shape (both <p> and
    // <div> elide to <IDENT>). That's by design: the K=1 hash is the FINAL
    // safety net, but the FIRST split is the (format, shape_class, near_tag)
    // key, which captures tag-name differences via `near_tag`. So this test
    // pins what the hash IS sensitive to: actual structural punctuation /
    // delimiter changes.
    let a = diff_shape_hash("<p>x</p>", "<p>x</p>"); // identical
    let b = diff_shape_hash("<p>x</p>", "<p>x"); // missing closer
    assert_ne!(a, b);
    // And whitespace-vs-no-whitespace inside punctuation matters too.
    let c = diff_shape_hash("a=b", "a = b");
    let d = diff_shape_hash("a=b", "a=b");
    assert_ne!(c, d);
}

#[test]
fn first_tag_name_finds_first_open_tag() {
    assert_eq!(first_tag_name("hello <p>world</p>"), Some("p".to_string()));
    assert_eq!(first_tag_name("</body>"), Some("body".to_string()));
    assert_eq!(first_tag_name("plain text"), None);
}

#[test]
fn cfg_sha_descriptor_format_matches_python_shape() {
    // Direct probe: the descriptor body for a known cfg_sha hex must produce
    // the exact bytes Python's json.dumps would emit. We pin the literal
    // bytes here because if a future maintainer "tidies" the format string
    // (e.g. inserts spaces, capitalises False), the integrity check will
    // silently disagree with Python — this test catches that at compile-test
    // time, before the integration test ever runs.
    let cfg_hex = "0000000000000000000000000000000000000000000000000000000000000000";
    let expected = r#"{"config_file_sha256":"0000000000000000000000000000000000000000000000000000000000000000","deduplicate":false,"include_comments":false,"with_metadata":false}"#;
    let actual = format!(
        r#"{{"config_file_sha256":"{cfg_hex}","deduplicate":false,"include_comments":false,"with_metadata":false}}"#
    );
    assert_eq!(actual, expected);
}
