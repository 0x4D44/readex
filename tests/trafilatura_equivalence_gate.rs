//! M3 Stage 0c — Trafilatura-equivalence BLOCKER gate (**ACTIVATED at Stage 1b**).
//!
//! HLD §6.2: this gate is the M3 analogue of M2's `parser_equivalence_gate.rs`.
//! Where M2's gate proves the **DOM substrate** (html5ever + rcdom) is
//! token-sequence-identical to jsdom 29.1.1 before any extraction logic exists,
//! M3's gate proves the **converted-tree substrate** (post-`convert_tags()`)
//! is XML-serialization-equivalent to Python `lxml`'s equivalent before any
//! M3 extraction logic runs against it.
//!
//! **STATUS at this commit (Stage 1b):** ACTIVATED. The gate runs on every
//! `cargo test --test trafilatura_equivalence_gate` against 10 fixture URLs
//! drawn from `benchmark/corpus/snapshots/`. The gate compares the Rust port's
//! post-`convert_tags` output (via `cleaning::tree_cleaning` +
//! `cleaning::convert_tags` + `dom::serialize_converted_tree`) to Trafilatura's
//! own (via a Python subprocess invoking
//! `benchmark/oracles/trafilatura/run.py --convert-tags-only`).
//!
//! # The comparison shape
//!
//! Both sides emit XML strings; the gate compares them via a **structural
//! XPath-like token list** rather than raw bytes. This is the documented
//! whitespace-only delta the HLD §6.2 permits: lxml's `etree.tostring()` and
//! the Rust serializer (`dom::serialize_converted_tree`) are unlikely to
//! match attribute order, whitespace placement, or empty-element form
//! byte-for-byte — but the post-`convert_tags` TREE STRUCTURE (element
//! tags + the rend / target / class attribute *values*, in document order)
//! is what's load-bearing for every downstream stage.
//!
//! The comparison extracts a sequence of `(open_or_close, tag, [rend])`
//! tokens from each XML string and asserts equality of the two sequences.
//! Inline text content is intentionally NOT compared at this stage — the
//! text-level equivalence is the Stage 2+ scope (when `xmltotxt` lands and
//! the gate's text-output comparison becomes byte-meaningful).
//!
//! # Fixture corpus (HLD §6.2: "~10 representative URLs")
//!
//! 10 fixtures drawn from `benchmark/corpus/snapshots/`, chosen for shape
//! coverage:
//!
//! - 3 M2 gold tranche-1 (example.com, Apple/Wikipedia, govuk-hub)
//! - 3 M2 EDGAR / HMRC tranche (Apple FY2025 10-K, HMRC 2025-26 rates,
//!   Apple Wikipedia again for the long-prose shape)
//! - 4 non-gold (BBC tech, Wikipedia German, regulator Fed-OMO, Rust blog)
//!
//! # On divergence
//!
//! The gate is **frozen** like its M2 sibling: any future commit to
//! `cleaning::tree_cleaning` / `cleaning::convert_tags` /
//! `dom::serialize_converted_tree` or their dependencies must keep the gate
//! green. Divergence is a BLOCKER — fix the port, never re-baseline the gate.

use std::path::{Path, PathBuf};
use std::process::Command;

use readex::readability::dom::{self, Dom};
use readex::trafilatura::cleaning;

/// Stage 1b activator: 10 corpus URL paths the gate runs against on every
/// `cargo test`. Paths are RELATIVE to the workspace root (the test runner's
/// CWD); the test resolves each against `CARGO_MANIFEST_DIR`.
///
/// Each entry's shape class is in its trailing comment. Selection rationale:
/// shape coverage across the corpus, not random sampling — the gate must
/// see at minimum one representative per (wiki, sec_edgar, regulator, news,
/// tech_blog, hub_index, edge_case).
const FIXTURES: &[&str] = &[
    // ---- gold tranche-1 (3) — proven jsdom-equivalent under M2 gate ----
    "benchmark/corpus/snapshots/0f115db062b7c0dd.html", // example.com (edge_case, minimal)
    "benchmark/corpus/snapshots/ae2c2184beb6d264.html", // en.wikipedia.org Apple_Inc.
    "benchmark/corpus/snapshots/9c8f49f04f792f81.html", // en.wikipedia.org Wm_Morrison
    // ---- EDGAR / HMRC tranche (3) — table-heavy survival check ----
    "benchmark/corpus/snapshots/9a1590d0917107a7.html", // Apple FY2025 10-K (62 tables)
    "benchmark/corpus/snapshots/9ec7aaf8edb71ac1.html", // HMRC 2025-26 (23 tables)
    "benchmark/corpus/snapshots/803b534a50a3f584.html", // gov.uk income tax (7 tables)
    // ---- non-gold (4) — shape-class diversity ----
    "benchmark/corpus/snapshots/f405a9e3314e15da.html", // BBC News tech (news)
    "benchmark/corpus/snapshots/5714710c8c9a3e8a.html", // de.wikipedia Rust
    "benchmark/corpus/snapshots/e339ce76eb1cba73.html", // Fed reserve open-market (regulator)
    "benchmark/corpus/snapshots/65e1c5b5502a5c81.html", // Rust 1.83 release blog (tech_blog)
];

/// THE BLOCKER GATE (HLD §6.2). One `#[test]` over the whole fixture set so
/// a single `cargo test --test trafilatura_equivalence_gate` reports the
/// per-fixture verdict in one place — the reviewable artefact the HLD asks
/// for. The first divergence STOPs the test with full evidence (honest-
/// failure doctrine).
#[test]
fn trafilatura_converted_tree_gate() {
    let mut pass = 0usize;
    let mut report = String::new();
    let total = FIXTURES.len();

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "Stage 1b gate fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust converted tree.
        let rust_tree = rust_converted_tree(&html);
        // 2. Python converted tree (subprocess to trafilatura oracle).
        let python_tree = match python_converted_tree(&path) {
            Ok(t) => t,
            Err(e) => panic!(
                "TRAFILATURA-EQUIVALENCE GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. Tokenize both into structural sequences and compare.
        let rust_tokens = structural_tokens(&rust_tree);
        let python_tokens = structural_tokens(&python_tree);

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

        // Divergence: find first differing index and emit a window.
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
            "TRAFILATURA-EQUIVALENCE GATE divergence on {}\n\n\
             Per-fixture report so far:\n{}\n\n\
             Full Rust XML (truncated to 2KB):\n{}\n\n\
             Full Python XML (truncated to 2KB):\n{}\n",
            fixture_rel,
            report,
            truncate(&rust_tree, 2000),
            truncate(&python_tree, 2000),
        );
    }

    eprintln!("\n=== Trafilatura-equivalence gate verdict ===");
    eprintln!("{report}");
    eprintln!("PASS {pass}/{total}\n");
    assert_eq!(
        pass, total,
        "Trafilatura-equivalence gate must pass on every fixture"
    );
}

/// Resolve `rel` relative to the workspace root (the directory of
/// `Cargo.toml`). Tests run with CWD = manifest dir, so this just joins.
fn workspace_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(rel)
}

/// Truncate `s` to at most `n` bytes, appending `…` if truncated.
fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        // Snap to a UTF-8 boundary to avoid panicking on a multi-byte split.
        let mut i = n;
        while !s.is_char_boundary(i) && i > 0 {
            i -= 1;
        }
        format!("{}…", &s[..i])
    }
}

/// Rust port path: parse, run tree_cleaning + convert_tags with defaults,
/// then serialize the post-convert_tags <html> root via the dom-facade XML
/// serializer.
///
/// **Cleaning runs on the `<html>` root, NOT `<body>`** — Trafilatura's Python
/// flow at `core.py:235,280` calls `load_html(filecontent)` (returns the
/// `<html>` root) then `tree_cleaning(copy(tree), options)`. `<head>` is in
/// MANUALLY_CLEANED so the head subtree is dropped during cleaning. Running
/// cleaning on `<body>` only would leave the head's `<title>` / `<meta>` /
/// `<style>` intact and diverge from the Python port.
fn rust_converted_tree(html: &str) -> String {
    let dom = Dom::parse(html);
    let html_root = dom.root_element().expect("html5ever synthesises <html>");
    let opts = cleaning::Options::default();
    cleaning::tree_cleaning(&html_root, &opts);
    cleaning::convert_tags(&html_root, &opts);
    dom::serialize_converted_tree(&html_root)
}

/// Python oracle path: spawn the trafilatura adapter in `--convert-tags-only`
/// mode and read its stdout as XML.
///
/// **Bypassing the venv-reexec:** the adapter normally re-execs into a
/// committed `.venv` (HLD §4 / B2 spike), but for the Stage 0c gate we need
/// a Python with `trafilatura==2.0.0` installed. We set the re-exec sentinel
/// pre-spawn so the adapter runs DIRECTLY under whatever `python` PATH
/// resolves to (the test harness's own Python, which must have trafilatura
/// importable for the gate to be meaningful).
fn python_converted_tree(snapshot_path: &Path) -> Result<String, String> {
    // The adapter relays sys.stdout.buffer.write of the XML string — we
    // capture stdout as bytes and decode as UTF-8.
    let run_py = workspace_path("benchmark/oracles/trafilatura/run.py");
    let output = Command::new("python")
        .arg(&run_py)
        .arg("--convert-tags-only")
        .arg(snapshot_path)
        .env("MDRCEL_TRAFILATURA_REEXECED", "1") // bypass venv re-exec
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

/// Tokenize an XML string into a `Vec<String>` of structural tokens:
///
/// - `"<tag>"` for each open tag, optionally followed by ` rend=VALUE` /
///   ` target=VALUE` / ` class=VALUE` (the three attributes Stage 1b's
///   `convert_tags` actually emits — all others are discarded; the
///   post-convert_tags surface deliberately strips most attributes).
/// - `"</tag>"` for each close tag.
/// - Text content (whitespace-collapsed, NFC-normalised) NOT included.
///
/// This is the "documented whitespace-only delta" the HLD §6.2 permits: we
/// compare TREE STRUCTURE + the rend/target/class attribute values, not
/// inter-element whitespace, not text byte-equality, and not attribute
/// ordering of *other* attributes.
///
/// The tokenizer is a tiny, deliberately-stupid XML reader — it does NOT
/// implement full XML; it implements "post-trafilatura-convert_tags-tree
/// XML", which is a constrained subset (no namespaces, no PIs, no CDATA,
/// no doctype, no comments, attributes are simple `name="value"` with the
/// five-char escape set). If a future Stage emits a more complex shape, the
/// tokenizer grows additively.
fn structural_tokens(xml: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let bytes = xml.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Find the matching '>'. Quoted attribute values may contain '>',
        // but the XML spec forbids it inside attributes; both sides emit
        // standard XML so we can scan for '>' directly with care to skip
        // chars inside double-quoted attribute values.
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
        // Skip comments / PIs / doctype / CDATA — none should appear in
        // post-convert_tags output but defensive.
        if inner.starts_with('!') || inner.starts_with('?') {
            i = j + 1;
            continue;
        }
        if let Some(stripped) = inner.strip_prefix('/') {
            tokens.push(format!("</{}>", stripped.trim()));
        } else {
            // Open tag (possibly self-closing). Parse out (tag, attrs).
            let (tag, attrs_str) = split_tag_attrs(inner);
            let mut tok = String::from("<");
            tok.push_str(&tag.to_ascii_lowercase());
            // Pick out the three load-bearing attrs in a canonical order.
            let attrs = parse_attrs(attrs_str);
            for key in ["rend", "target", "class", "href"] {
                if let Some(v) = attrs.iter().find(|(k, _)| k == key) {
                    tok.push_str(&format!(" {}=\"{}\"", v.0, decode_xml_entities(&v.1)));
                }
            }
            tok.push('>');
            tokens.push(tok);
            // Self-closing form `<tag/>` — emit the matching close too so
            // the two sides agree regardless of long/short form choice.
            if inner.trim_end().ends_with('/') {
                tokens.push(format!("</{}>", tag.to_ascii_lowercase()));
            }
        }
        i = j + 1;
    }
    tokens
}

/// Split `<tag ...attrs...>` content (excluding the angle brackets) into
/// `(tag_name, attrs_str)`. Handles `<tag>`, `<tag attr=val>`, `<tag/>`.
fn split_tag_attrs(inner: &str) -> (String, &str) {
    let trimmed = inner.trim_end_matches('/');
    let (tag, rest) = match trimmed.find(|c: char| c.is_ascii_whitespace()) {
        Some(idx) => (&trimmed[..idx], &trimmed[idx..]),
        None => (trimmed, ""),
    };
    (tag.to_string(), rest.trim_start())
}

/// Parse `attr1="v1" attr2='v2' attr3=v3` into `Vec<(key, value)>`. Returns
/// attributes in source order. Values are XML-unescaped (the five-char set).
fn parse_attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // Skip whitespace.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        // Read key up to '=' or whitespace.
        let key_start = i;
        while i < bytes.len() && bytes[i] != b'=' && !(bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        let key = &s[key_start..i];
        if key.is_empty() {
            break;
        }
        // Skip '=' and surrounding spaces.
        while i < bytes.len() && (bytes[i] as char).is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            // attribute with no value — record as empty.
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
        // Value: quoted or bare.
        let value = if bytes[i] == b'"' || bytes[i] == b'\'' {
            let q = bytes[i];
            i += 1;
            let v_start = i;
            while i < bytes.len() && bytes[i] != q {
                i += 1;
            }
            let v = &s[v_start..i];
            if i < bytes.len() {
                i += 1; // skip closing quote
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

/// Decode the five core XML entities used by both sides' serializers.
fn decode_xml_entities(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

// ===========================================================================
// Tokenizer self-tests (the gate's tokenizer is load-bearing — if it loses a
// real divergence we cannot certify the substrate).
// ===========================================================================

#[test]
fn tokenizer_simple_open_close() {
    let toks = structural_tokens("<p>x</p>");
    assert_eq!(toks, vec!["<p>", "</p>"]);
}

#[test]
fn tokenizer_keeps_rend_attribute() {
    let toks = structural_tokens(r##"<hi rend="#b">x</hi>"##);
    assert_eq!(toks, vec![r##"<hi rend="#b">"##, "</hi>"]);
}

#[test]
fn tokenizer_keeps_href_and_target() {
    let toks = structural_tokens(r#"<ref target="/x" href="/y">k</ref>"#);
    assert_eq!(toks, vec![r#"<ref target="/x" href="/y">"#, "</ref>"]);
}

#[test]
fn tokenizer_discards_other_attributes() {
    // Stage 1b convert_tags clears most attributes; if any leak through (e.g.
    // a stray `id`/`data-x`/`style`) we don't want to fail the gate on it.
    let toks = structural_tokens(r#"<p id="x" data-k="v">x</p>"#);
    assert_eq!(toks, vec!["<p>", "</p>"]);
}

#[test]
fn tokenizer_handles_self_closing() {
    let toks = structural_tokens(r#"<lb/>"#);
    assert_eq!(toks, vec!["<lb>", "</lb>"]);
}

#[test]
fn tokenizer_handles_nested_with_text_between() {
    let toks = structural_tokens("<div>a<p>b</p>c</div>");
    assert_eq!(toks, vec!["<div>", "<p>", "</p>", "</div>"]);
}

#[test]
fn tokenizer_canonical_attr_order_independent_of_source_order() {
    // rend comes BEFORE target in our canonical order regardless of source.
    let toks_a = structural_tokens(r#"<ref target="/x" rend="r">k</ref>"#);
    let toks_b = structural_tokens(r#"<ref rend="r" target="/x">k</ref>"#);
    assert_eq!(toks_a, toks_b);
    assert_eq!(toks_a[0], r#"<ref rend="r" target="/x">"#);
}
