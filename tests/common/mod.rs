//! Shared, FORMAT-AGNOSTIC helpers for the six trafilatura format-gate
//! integration tests (`trafilatura_{markdown,txt,json,csv,xml,tei}_gate.rs`).
//!
//! Cargo does NOT treat files under a `tests/` subdirectory as their own test
//! binary, so this module is included by each gate via `mod common;` and is
//! never run as a standalone gate. Each gate uses a different subset of these
//! helpers, hence the crate-wide `dead_code` allow.
//!
//! # What lives here (and why)
//!
//! ONLY the helpers that were byte-for-byte identical across the gates that
//! defined them: the divergence `Bucket` enum + classifier, the diff-window /
//! escape utilities, the workspace-path resolver, the unified Python oracle
//! spawn, and an NFC normaliser. Format-SPECIFIC masking (csv fingerprint
//! column, xml `<doc>` fingerprint attribute, tei `<teiHeader>` /
//! `<note type="fingerprint">`) is assertion-bearing and STAYS in each gate.
//!
//! This is the shared seam the M9 fuzz harness will reuse.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;

use unicode_normalization::UnicodeNormalization;

/// Bucket classification of a divergence — coarse-grained on purpose so the
/// triage step can pick the highest-value fix target at a glance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Bucket {
    /// One side empty, the other not. The most severe class.
    EmptyVsNon,
    /// Both non-empty AND identical after collapsing ASCII whitespace runs.
    WhitespaceOnly,
    /// Both non-empty, differ even after whitespace collapse.
    ContentMismatch,
}

impl Bucket {
    pub fn label(self) -> &'static str {
        match self {
            Bucket::EmptyVsNon => "empty-vs-non",
            Bucket::WhitespaceOnly => "whitespace-only",
            Bucket::ContentMismatch => "content-mismatch",
        }
    }
}

/// Resolve a workspace-relative path against `CARGO_MANIFEST_DIR`.
pub fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// NFC-normalise a string (belt-and-braces: both pipelines already NFC-normalise
/// their own output; the gates normalise once more to make the contract explicit
/// at gate level).
pub fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Unified Python oracle spawn: run `run.py <format_flag> <snapshot_path>` and
/// return its stdout. `format_flag` is the per-format selector (e.g. `"--txt"`,
/// `"--markdown"`, `"--json"`, `"--csv"`, `"--xml"`, `"--xmltei"`). Bypasses the
/// venv re-exec by setting `MDRCEL_TRAFILATURA_REEXECED=1`.
pub fn run_oracle(format_flag: &str, snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg(format_flag)
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

/// Bucket-classify a divergence. Coarse on purpose — sub-buckets are derived
/// from the per-fixture window listing as needed.
pub fn classify(rust: &str, python: &str) -> Bucket {
    if rust.is_empty() != python.is_empty() {
        return Bucket::EmptyVsNon;
    }
    if collapse_ws(rust) == collapse_ws(python) {
        return Bucket::WhitespaceOnly;
    }
    Bucket::ContentMismatch
}

/// Collapse every run of ASCII whitespace to a single space; strip ends.
pub fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First byte index where two byte slices differ; min(len) if one is a
/// prefix of the other.
pub fn first_diff_index(a: &[u8], b: &[u8]) -> usize {
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
pub fn window_around(s: &str, byte_idx: usize, n: usize) -> String {
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
pub fn escape(s: &str) -> String {
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
// Self-tests for the shared helpers — they live with the code they test.
// These run once per gate crate that includes `common`; they are microsecond
// unit tests so the duplication is harmless.
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
