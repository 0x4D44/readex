//! M3 Stage 3-B — `extract_content` Trafilatura-equivalence BLOCKER gate.
//!
//! HLD §6.2 follow-on: where Stage 0c / 1b's
//! `trafilatura_equivalence_gate.rs` proves the post-`convert_tags` substrate
//! is XML-serialization-equivalent to Trafilatura's, this gate proves the
//! ONE STAGE DEEPER `extract_content` output is also equivalent. The
//! Python-side oracle is `benchmark/oracles/trafilatura/run.py
//! --extract-content` (added in the same commit as this gate).
//!
//! # Why a separate gate from Stage 3-A's smoke test
//!
//! `trafilatura_extract_smoke.rs` only asserts the Rust pipeline returns
//! ≥ 200 chars of text on ≥ 7/10 fixtures — a sanity floor, not equivalence.
//! This gate is the byte-meaningful pin: structural-token equality against
//! the Python oracle, fixture by fixture. A failure here is a faithfulness
//! regression and must be fixed in the port, never re-baselined (same
//! discipline as `parser_equivalence_gate.rs` and the Stage 1b gate).
//!
//! # The comparison shape
//!
//! Both sides emit XML strings; the gate compares them via a structural
//! token list (same approach as `trafilatura_equivalence_gate.rs`). The
//! attribute set tracked here is wider than the 1b gate's, because
//! `extract_content` emits more attribute-bearing elements:
//!
//! - `rend` — `<head>` heading levels, `<hi>` formatting, `<list>` type
//! - `target` / `href` — `<ref>` hyperlinks
//! - `src` / `alt` / `title` — `<graphic>` images (Stage 2c-iii-a)
//! - `role` / `span` — `<cell>` head-row marker and column span
//!   (Stage 2c-iii-b)
//! - `class` — defensive; Stage 1b convert_tags strips most class attrs
//!   but a few survive into extracted output
//!
//! Text content + inter-element whitespace + attribute ordering of *other*
//! attributes are NOT compared (same documented whitespace-only delta the
//! HLD §6.2 permits — see `trafilatura_equivalence_gate.rs` rationale).
//!
//! # Fixture corpus — phased rollout
//!
//! This gate is **born small**. The first commit pins ONLY example.com —
//! the smallest fixture in the corpus (528 bytes, edge_case shape). Each
//! subsequent commit expands the fixture set as we resolve per-fixture
//! divergences and pin the port one fixture at a time. The end goal is the
//! same 10-fixture set the Stage 1b gate uses (HLD §6.2 "~10 representative
//! URLs"); the phased rollout is operationally honest — extraction
//! equivalence is harder than substrate equivalence and we should not
//! pretend otherwise by activating all 10 fixtures and burying a long list
//! of "expected failures".
//!
//! # On divergence
//!
//! The gate is frozen like its sibling. Any future commit to
//! `main_extractor::extract_content` or its dependencies must keep the gate
//! green on every active fixture. Divergence is a BLOCKER — fix the port.

use std::path::{Path, PathBuf};
use std::process::Command;

use readex::readability::dom::{self, Dom};
use readex::trafilatura::{cleaning, main_extractor};

/// Stage 3-B activator: corpus paths the gate runs against. **Born small** —
/// see file-level docs for the phased rollout rationale.
///
/// To add a fixture: pin the divergence (run the gate, capture the first
/// diff index), fix the port, re-run until token-identical, then add the
/// fixture here in a dedicated commit.
const FIXTURES: &[&str] = &[
    // ---- phase 1 (commit 64d8814) ----
    "benchmark/corpus/snapshots/0f115db062b7c0dd.html", // example.com (edge_case)
    // ---- phase 2 (Cluster A fix, commit 8bf2e15) ----
    "benchmark/corpus/snapshots/9c8f49f04f792f81.html", // en.wikipedia Wm_Morrison
    "benchmark/corpus/snapshots/9a1590d0917107a7.html", // Apple FY2025 10-K
    "benchmark/corpus/snapshots/9ec7aaf8edb71ac1.html", // HMRC 2025-26
    "benchmark/corpus/snapshots/f405a9e3314e15da.html", // BBC News tech
    // ---- phase 3 (Cluster B fix, commit 6462060) ----
    "benchmark/corpus/snapshots/803b534a50a3f584.html", // gov.uk income tax (PAYE)
    "benchmark/corpus/snapshots/e339ce76eb1cba73.html", // Fed reserve open-market
    // ---- phase 4 (Cluster C fix, commit b54e02d) ----
    "benchmark/corpus/snapshots/5714710c8c9a3e8a.html", // de.wikipedia Rust
    "benchmark/corpus/snapshots/65e1c5b5502a5c81.html", // Rust 1.83 release blog
    // ---- phase 4 (Cluster D fix, commit TBD) ----
    "benchmark/corpus/snapshots/ae2c2184beb6d264.html", // en.wikipedia Apple_Inc
];

#[test]
fn trafilatura_extract_content_gate() {
    let mut pass = 0usize;
    let mut report = String::new();
    let total = FIXTURES.len();

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "Stage 3-B gate fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust extract_content output as XML.
        let rust_xml = rust_extract_content_xml(&html);
        // 2. Python extract_content output (subprocess oracle).
        let python_xml = match python_extract_content_xml(&path) {
            Ok(t) => t,
            Err(e) => panic!(
                "STAGE 3-B GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. Tokenize both into structural sequences and compare.
        let rust_tokens = structural_tokens(&rust_xml);
        let python_tokens = structural_tokens(&python_xml);

        if rust_tokens == python_tokens {
            pass += 1;
            report.push_str(&format!(
                "  PASS  {}  (rust={} tokens; python={} tokens)\n",
                fixture_rel,
                rust_tokens.len(),
                python_tokens.len(),
            ));
            continue;
        }

        let first_diff = rust_tokens
            .iter()
            .zip(python_tokens.iter())
            .position(|(a, b)| a != b)
            .unwrap_or_else(|| rust_tokens.len().min(python_tokens.len()));
        let lo = first_diff.saturating_sub(5);
        let hi_r = (first_diff + 5).min(rust_tokens.len());
        let hi_p = (first_diff + 5).min(python_tokens.len());

        report.push_str(&format!(
            "  FAIL  {}\n    rust tokens: {} python tokens: {}\n    first diff at index {}:\n      rust:   {:?}\n      python: {:?}\n",
            fixture_rel,
            rust_tokens.len(),
            python_tokens.len(),
            first_diff,
            &rust_tokens[lo..hi_r],
            &python_tokens[lo..hi_p],
        ));

        panic!(
            "STAGE 3-B GATE divergence on {}\n\n\
             Per-fixture report so far:\n{}\n\n\
             Full Rust XML (truncated to 2KB):\n{}\n\n\
             Full Python XML (truncated to 2KB):\n{}\n",
            fixture_rel,
            report,
            truncate(&rust_xml, 2000),
            truncate(&python_xml, 2000),
        );
    }

    eprintln!("\n=== Stage 3-B extract_content gate verdict ===");
    eprintln!("{report}");
    eprintln!("PASS {pass}/{total}\n");
    assert_eq!(
        pass, total,
        "Stage 3-B extract_content gate must pass on every active fixture"
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

/// Rust port path: parse, tree_cleaning, convert_tags, then extract_content;
/// serialize the returned `result_body` via the dom-facade XML serializer.
///
/// The `Dom` must stay alive through the whole pipeline — rcdom's iterative
/// Drop (Stage 2d discovery, pinned in `trafilatura_extract_smoke.rs`)
/// would drain every descendant if the Dom went out of scope early.
fn rust_extract_content_xml(html: &str) -> String {
    let dom = Dom::parse(html);
    let html_root = dom.root_element().expect("html5ever synthesises <html>");
    let opts = cleaning::Options::default();
    cleaning::tree_cleaning(&html_root, &opts);
    cleaning::convert_tags(&html_root, &opts);
    let body = dom.body().expect("body present after cleaning");
    let (result_body, _text, _len) = main_extractor::extract_content(&body, &opts);
    dom::serialize_converted_tree(&result_body)
}

/// Python oracle path: spawn the trafilatura adapter in `--extract-content`
/// mode and read its stdout as XML. Bypasses the venv re-exec by setting
/// `MDRCEL_TRAFILATURA_REEXECED=1` (same trick as the Stage 1b gate).
fn python_extract_content_xml(snapshot_path: &Path) -> Result<String, String> {
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--extract-content")
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

/// Tokenize an XML string into a `Vec<String>` of structural tokens.
///
/// **Attribute set tracked here is wider than the Stage 1b gate's** — see
/// file-level docs for the rationale. The canonical attribute order in the
/// emitted token (regardless of source order) is:
///
///   `rend, target, href, src, alt, title, role, span, class`
///
/// All other attributes are discarded. Text content + whitespace are NOT
/// included (Stage 3-C will pin those when we have an isolated text-extraction
/// gate).
///
/// This is structurally identical to the tokenizer in
/// `trafilatura_equivalence_gate.rs` — the duplication is intentional. The
/// tracked-attribute SET is a per-stage decision (Stage 1b emits a narrower
/// surface than Stage 3-B), so factoring the tokenizer into a shared module
/// would force the two stages to share a constant that should not be shared.
fn structural_tokens(xml: &str) -> Vec<String> {
    const TRACKED_ATTRS: &[&str] = &[
        "rend", "target", "href", "src", "alt", "title", "role", "span", "class",
    ];

    let mut tokens = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut in_quote = false;
        let mut quote_ch = b'"';
        while j < bytes.len() {
            let c = bytes[j];
            if in_quote {
                if c == quote_ch {
                    in_quote = false;
                }
            } else if c == b'"' || c == b'\'' {
                in_quote = true;
                quote_ch = c;
            } else if c == b'>' {
                break;
            }
            j += 1;
        }
        if j >= bytes.len() {
            break;
        }
        let inner = &xml[i + 1..j];
        if inner.starts_with('!') || inner.starts_with('?') {
            i = j + 1;
            continue;
        }
        if let Some(stripped) = inner.strip_prefix('/') {
            tokens.push(format!("</{}>", stripped.trim()));
        } else {
            let (tag, attrs_str) = split_tag_attrs(inner);
            let mut tok = String::from("<");
            tok.push_str(&tag.to_ascii_lowercase());
            let attrs = parse_attrs(attrs_str);
            for key in TRACKED_ATTRS {
                if let Some(v) = attrs.iter().find(|(k, _)| k == key) {
                    tok.push_str(&format!(" {}=\"{}\"", v.0, decode_xml_entities(&v.1)));
                }
            }
            tok.push('>');
            tokens.push(tok);
            if inner.trim_end().ends_with('/') {
                tokens.push(format!("</{}>", tag.to_ascii_lowercase()));
            }
        }
        i = j + 1;
    }
    tokens
}

fn split_tag_attrs(inner: &str) -> (String, &str) {
    let trimmed = inner.trim_end_matches('/');
    let (tag, rest) = match trimmed.find(|c: char| c.is_ascii_whitespace()) {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, ""),
    };
    (tag.to_string(), rest.trim_start())
}

fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !(bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        let key = &s[key_start..i];
        if key.is_empty() {
            break;
        }
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            out.push((key.to_string(), String::new()));
            continue;
        }
        i += 1;
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i];
            i += 1;
            let v_start = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let v = &s[v_start..i];
            if i < bytes.len() {
                i += 1;
            }
            v.to_string()
        } else {
            let v_start = i;
            while i < bytes.len() && !(bytes[i] as char).is_ascii_whitespace() {
                i += 1;
            }
            s[v_start..i].to_string()
        };
        out.push((key.to_string(), value));
    }
    out
}

fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

// ===========================================================================
// Tokenizer self-tests — confirm the wider tracked-attr set works correctly.
// (Shared shape with the Stage 1b gate's self-tests, but the wider attr set
// means we re-prove the canonical-order invariant for our specific list.)
// ===========================================================================

#[test]
fn tokenizer_tracks_src_alt_title_for_graphic() {
    let toks = structural_tokens(r#"<graphic src="/x.png" alt="cat" title="a cat">x</graphic>"#);
    assert_eq!(
        toks,
        vec![
            r#"<graphic src="/x.png" alt="cat" title="a cat">"#,
            "</graphic>",
        ]
    );
}

#[test]
fn tokenizer_tracks_role_and_span_for_cell() {
    let toks = structural_tokens(r#"<cell role="head" span="3">x</cell>"#);
    assert_eq!(
        toks,
        vec![r#"<cell role="head" span="3">"#, "</cell>"]
    );
}

#[test]
fn tokenizer_canonical_order_independent_of_source() {
    let a = structural_tokens(r#"<graphic title="t" src="/x" alt="a">k</graphic>"#);
    let b = structural_tokens(r#"<graphic alt="a" src="/x" title="t">k</graphic>"#);
    assert_eq!(a, b);
    assert_eq!(a[0], r#"<graphic src="/x" alt="a" title="t">"#);
}

#[test]
fn tokenizer_drops_unknown_attrs() {
    let toks = structural_tokens(r#"<p id="x" data-k="v" style="color:red">x</p>"#);
    assert_eq!(toks, vec!["<p>", "</p>"]);
}
