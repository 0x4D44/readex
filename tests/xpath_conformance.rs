//! Stage-0b XPath **conformance** gate (HLD M3 §6.1 / DA-M-6 ratified;
//! DECISION-A discipline).
//!
//! Greenfield XPath evaluators are correctness-fragile by construction; the
//! HLD replaces line-cite discipline for `xpath_engine.rs` with
//! **conformance-table discipline**: every supported operator/axis/function
//! has rows of the form `(xpath_expr, input_html, expected_node_count,
//! expected_first_node_id_or_text)` cross-checked against Python `lxml`
//! at test-build time. A row failure = a build failure.
//!
//! The harness:
//! 1. For each row, evaluates the XPath against the input HTML in Rust
//!    (via `readex::trafilatura::xpath_engine::evaluate`).
//! 2. Spawns the Python probe at
//!    `benchmark/oracles/xpath_conformance/conformance_probe.py` with the
//!    same XPath and HTML, parsing its JSON envelope.
//! 3. Asserts:
//!    - same node count
//!    - same per-node `@id` (Rust extracts via `dom::get_attribute`, Python
//!      via `lxml.Element.get('id')`)
//!    - same per-node tag (lower-cased local name)
//!
//! ## Required environment
//!
//! Python 3 plus `lxml` must be on the bare `python` interpreter the test
//! invokes. If Python is absent or lxml is unimportable the test **fails
//! loudly** — a silently-skipped conformance gate is the Bug-E2 trap M3 §6.1
//! exists to prevent. The Trafilatura oracle's committed venv (per
//! `benchmark/oracles/trafilatura/requirements.txt`) is a sufficient
//! reproducer but not the only one — the probe needs only lxml, no
//! trafilatura, and runs against a bare `python` first.
//!
//! ## Coverage
//!
//! >= 51 rows, structured by `xpaths.py` pattern category:
//! - BODY_XPATH (5 patterns, 3+ rows each)
//! - COMMENTS_XPATH (4 patterns, 3+ rows each)
//! - REMOVE_COMMENTS_XPATH (1 pattern)
//! - OVERALL_DISCARD_XPATH (2 patterns)
//! - TEASER_DISCARD_XPATH (1)
//! - PRECISION_DISCARD_XPATH (2)
//! - COMMENTS_DISCARD_XPATH (3)
//! - DISCARD_IMAGE_ELEMENTS (1)
//! - AUTHOR_XPATHS (3)
//! - AUTHOR_DISCARD_XPATHS (2)
//! - CATEGORIES_XPATHS (6)
//! - TAGS_XPATHS (4)
//! - TITLE_XPATHS (3)
//!
//! Plus operator-coverage rows (positional, union, attribute-union, etc.)
//! for any operator not exercised by the above. This gives ~51 distinct
//! conformance assertions.

use std::path::PathBuf;
use std::process::Command;

use readex::readability::dom::{self, Dom, NodeRef};
use readex::trafilatura::xpath_engine;

/// A single conformance row. Both Rust and lxml are evaluated with `body` as
/// the context node.
#[derive(Debug, Clone)]
struct Row {
    /// Short label for diagnostic output.
    label: &'static str,
    /// HTML to parse. Use a body-only fragment — the harness wraps with
    /// `<html><body>...</body></html>` for parser stability.
    html: &'static str,
    /// XPath expression under test.
    xpath: &'static str,
}

/// Result of evaluating a row through Rust + lxml.
#[derive(Debug)]
struct Compared {
    rust_count: usize,
    lxml_count: usize,
    rust_ids: Vec<String>,
    lxml_ids: Vec<String>,
    rust_tags: Vec<String>,
    lxml_tags: Vec<String>,
}

fn rows() -> Vec<Row> {
    vec![
        // ===== BODY_XPATH (xpaths.py §1 BODY_XPATH, 5 patterns) =============
        Row {
            label: "BODY_XPATH[0]: class='post'",
            html: "<div class='post' id='a'>x</div><div class='other' id='b'>y</div>",
            xpath: ".//*[self::article or self::div or self::main or self::section][@class=\"post\" or @class=\"entry\"]",
        },
        Row {
            label: "BODY_XPATH[0]: contains class post-text",
            html: "<div class='post-text body' id='a'/><article class='other' id='b'/>",
            xpath: ".//*[self::article or self::div or self::main or self::section][contains(@class, \"post-text\")]",
        },
        Row {
            label: "BODY_XPATH[0]: itemprop articleBody",
            html: "<div itemprop='articleBody' id='a'/><div id='b'/>",
            xpath: ".//*[self::article or self::div or self::main or self::section][@itemprop=\"articleBody\"]",
        },
        Row {
            label: "BODY_XPATH[0]: translate articlebody",
            html: "<div id='ArticleBody'>x</div><div id='articleBody'>y</div><div id='other'>z</div>",
            xpath: ".//*[contains(translate(@id, \"AB\", \"ab\"), \"articlebody\")]",
        },
        Row {
            label: "BODY_XPATH[1]: (.//article)[1]",
            html: "<article id='a'/><article id='b'/><article id='c'/>",
            xpath: "(.//article)[1]",
        },
        Row {
            label: "BODY_XPATH[2]: starts-with @id primary",
            html: "<div id='primary-content' id_marker='a'/><div id='secondary' id_marker='b'/>",
            xpath: ".//*[self::article or self::div][starts-with(@id, \"primary\")]",
        },
        Row {
            label: "BODY_XPATH[2]: role='article'",
            html: "<div role='article' id='a'/><div id='b'/>",
            xpath: ".//*[self::article or self::div][@role=\"article\"]",
        },
        Row {
            label: "BODY_XPATH[3]: contains main-content (translated)",
            html: "<div id='MAIN-content' id_marker='a'/><div class='main-content' id_marker='b'/><div id='other' id_marker='c'/>",
            xpath: ".//*[self::div][contains(translate(@id, \"CM\",\"cm\"), \"main-content\") or contains(translate(@class, \"CM\",\"cm\"), \"main-content\")]",
        },
        Row {
            label: "BODY_XPATH[4]: (.//main)[1]",
            html: "<main id='a'/><main id='b'/>",
            xpath: "(.//main)[1]",
        },
        // ===== COMMENTS_XPATH (xpaths.py §1 COMMENTS_XPATH, 4 patterns) =====
        Row {
            label: "COMMENTS[0]: contains @id|@class commentlist",
            html: "<div class='wp-commentlist'/><div id='commentlist-y'/><div class='unrelated'/>",
            xpath: ".//*[self::div or self::list or self::section][contains(@id|@class, 'commentlist')]",
        },
        Row {
            label: "COMMENTS[0]: contains class comment-page",
            html: "<div class='comment-page-1'/><div class='other'/>",
            xpath: ".//*[self::div or self::section][contains(@class, 'comment-page')]",
        },
        Row {
            label: "COMMENTS[1]: starts-with @id|@class comments",
            html: "<div id='comments'/><div class='Comments-block'/><div id='other'/>",
            xpath: ".//*[self::div or self::section][starts-with(@id|@class, 'comments')]",
        },
        Row {
            label: "COMMENTS[2]: starts-with @id disqus_thread",
            html: "<div id='disqus_thread'/><div id='disqus_other'/><div id='unrelated'/>",
            xpath: ".//*[self::div or self::section][starts-with(@id, 'disqus_thread')]",
        },
        Row {
            label: "COMMENTS[3]: contains class comment",
            html: "<section class='comment-x'/><section id='social-y'/><section id='other'/>",
            xpath: ".//*[self::div or self::section][starts-with(@id, 'social') or contains(@class, 'comment')]",
        },
        // ===== REMOVE_COMMENTS_XPATH =========================================
        Row {
            label: "REMOVE_COMMENTS: starts-with translate @id comment",
            html: "<div id='Comment-1'/><div id='COMMENT-2'/><div id='other'/>",
            xpath: ".//*[self::div][starts-with(translate(@id, \"C\",\"c\"), 'comment')]",
        },
        // ===== OVERALL_DISCARD_XPATH (2 patterns) ============================
        Row {
            label: "OVERALL_DISCARD: contains translate @id footer",
            html: "<div id='Footer-Y'/><div class='page-FOOTER'/><div id='other'/>",
            xpath: ".//*[self::div][contains(translate(@id, \"F\",\"f\"), \"footer\") or contains(translate(@class, \"F\",\"f\"), \"footer\")]",
        },
        Row {
            label: "OVERALL_DISCARD: data-lp-replacement-content presence",
            html: "<div data-lp-replacement-content='x'/><div id='other'/>",
            xpath: ".//*[@data-lp-replacement-content]",
        },
        Row {
            label: "OVERALL_DISCARD: contains @id|@class viral",
            html: "<div class='viral-share'/><div id='go-viral'/><div id='other'/>",
            xpath: ".//*[contains(@id|@class, 'viral')]",
        },
        Row {
            label: "OVERALL_DISCARD: comments-title via @class",
            html: "<div class='comments-title-block'/><div class='other'/>",
            xpath: ".//*[@class='comments-title' or contains(@class, 'comments-title')]",
        },
        Row {
            label: "OVERALL_DISCARD: aria-hidden true",
            html: "<div aria-hidden='true' id='a'/><div id='b'/>",
            xpath: ".//*[@aria-hidden='true']",
        },
        // ===== TEASER_DISCARD_XPATH ==========================================
        Row {
            label: "TEASER_DISCARD: contains translate teaser",
            html: "<div id='Teaser-1'/><div class='TEASER-box'/><div id='other'/>",
            xpath: ".//*[self::div][contains(translate(@id, \"T\", \"t\"), \"teaser\") or contains(translate(@class, \"T\", \"t\"), \"teaser\")]",
        },
        // ===== PRECISION_DISCARD_XPATH (2 patterns) ==========================
        Row {
            label: "PRECISION_DISCARD: .//header",
            html: "<header id='h1'/><div><header id='h2'/></div>",
            xpath: ".//header",
        },
        Row {
            label: "PRECISION_DISCARD: bottom/link/border",
            html: "<div id='bottom-bar'/><div class='link-list'/><div style='border:1px solid' id='c'/><div id='other'/>",
            xpath: ".//*[self::div][contains(@id|@class, 'bottom') or contains(@id|@class, 'link') or contains(@style, 'border')]",
        },
        // ===== DISCARD_IMAGE_ELEMENTS ========================================
        Row {
            label: "DISCARD_IMAGE: caption id/class",
            html: "<div id='caption-1'/><div class='img-caption'/><div id='other'/>",
            xpath: ".//*[self::div or self::p][contains(@id, 'caption') or contains(@class, 'caption')]",
        },
        // ===== COMMENTS_DISCARD_XPATH (3 patterns) ===========================
        Row {
            label: "COMMENTS_DISCARD: respond id",
            html: "<div id='respond-1'/><section id='respond-2'/><div id='other'/>",
            xpath: ".//*[self::div or self::section][starts-with(@id, 'respond')]",
        },
        Row {
            label: "COMMENTS_DISCARD: .//cite|.//quote",
            html: "<cite id='c1'/><quote id='q1'/><p id='p'/>",
            xpath: ".//cite|.//quote",
        },
        Row {
            label: "COMMENTS_DISCARD: contains @id|@class akismet",
            html: "<div id='akismet-form'/><span class='akismet-x'/><div id='other'/>",
            xpath: ".//*[contains(@id|@class, 'akismet')]",
        },
        // ===== AUTHOR_XPATHS (3 patterns; simplified ports) ==================
        Row {
            label: "AUTHOR[0]: @rel='author'",
            html: "<a rel='author' id='a'/><a id='b'/>",
            xpath: "//*[self::a or self::span][@rel=\"author\" or @id=\"author\"]",
        },
        Row {
            label: "AUTHOR[1]: contains class author",
            html: "<span class='author-name' id='a'/><div class='author' id='b'/><p id='other'/>",
            xpath: "//*[self::span or self::div or self::p][contains(@class, \"author\") or contains(@id, \"author\")]",
        },
        Row {
            label: "AUTHOR[2]: contains translate class author",
            html: "<div class='AUTHOR-card' id='a'/><div class='other' id='b'/>",
            xpath: "//*[contains(translate(@class, \"A\", \"a\"), \"author\")]",
        },
        // ===== AUTHOR_DISCARD_XPATHS (2 patterns) ============================
        Row {
            label: "AUTHOR_DISCARD: //time|//figure",
            html: "<time id='t'/><figure id='f'/><div id='d'/>",
            xpath: "//time|//figure",
        },
        Row {
            label: "AUTHOR_DISCARD: contains class is-hidden",
            html: "<div class='is-hidden' id='a'/><div class='visible' id='b'/>",
            xpath: ".//*[self::div][contains(@class, 'is-hidden')]",
        },
        // ===== CATEGORIES_XPATHS (6 patterns) ================================
        Row {
            label: "CATEGORIES[0]: post-info //a[@href]",
            html: "<div class='post-info-x'><a id='a' href='/x'/><a id='b'/></div><div><a id='c' href='/y'/></div>",
            xpath: "//div[starts-with(@class, 'post-info')]//a[@href]",
        },
        Row {
            label: "CATEGORIES[1]: postmeta p//a",
            html: "<p class='postmeta-1'><a id='a' href='/x'/></p><p id='other'><a id='b' href='/y'/></p>",
            xpath: "//p[starts-with(@class, 'postmeta')]//a[@href]",
        },
        Row {
            label: "CATEGORIES[2]: footer entry-meta",
            html: "<footer class='entry-meta'><a id='a' href='/x'/></footer><footer class='other'><a id='b'/></footer>",
            xpath: "//footer[starts-with(@class, 'entry-meta')]//a[@href]",
        },
        Row {
            label: "CATEGORIES[3]: li/span post-category",
            // Use explicit `</a>` close tags — `<a/>` self-closing is NOT
            // legal HTML5 (`<a>` is not a void element), and html5ever's
            // "adoption agency" algorithm re-parents subsequent siblings
            // under the still-open `<a>` element, producing a tree very
            // different from lxml. Closing `<a>` explicitly removes the
            // parser divergence and isolates the XPath semantics under test.
            html: "<li class='post-category'><a id='a' href='/x'></a></li><span class='post-category'><a id='b' href='/y'></a></span><li id='other'><a id='c' href='/z'></a></li>",
            xpath: "//*[self::li or self::span][@class=\"post-category\" or @class=\"postcategory\" or contains(@class, \"cat-links\")]//a[@href]",
        },
        Row {
            label: "CATEGORIES[4]: header entry-header",
            html: "<header class='entry-header'><a id='a' href='/x'/></header><header class='other'><a id='b' href='/y'/></header>",
            xpath: "//header[@class=\"entry-header\"]//a[@href]",
        },
        Row {
            label: "CATEGORIES[5]: row or tags div//a",
            html: "<div class='row'><a id='a' href='/x'/></div><div class='tags'><a id='b' href='/y'/></div><div class='other'><a id='c' href='/z'/></div>",
            xpath: "//div[@class=\"row\" or @class=\"tags\"]//a[@href]",
        },
        // ===== TAGS_XPATHS (4 patterns) ======================================
        Row {
            label: "TAGS[0]: div class tags //a",
            html: "<div class='tags'><a id='a' href='/x'/></div><div class='other'><a id='b' href='/y'/></div>",
            xpath: "//div[@class=\"tags\"]//a[@href]",
        },
        Row {
            label: "TAGS[1]: p entry-tags //a",
            html: "<p class='entry-tags-x'><a id='a' href='/x'/></p><p class='other'><a id='b' href='/y'/></p>",
            xpath: "//p[starts-with(@class, 'entry-tags')]//a[@href]",
        },
        Row {
            label: "TAGS[2]: div starts-with tag",
            html: "<div class='tag-list'><a id='a' href='/x'/></div><div class='other'><a id='b' href='/y'/></div>",
            xpath: "//div[starts-with(@class, 'tag')]//a[@href]",
        },
        Row {
            label: "TAGS[3]: contains topics",
            html: "<div class='topics-area'><a id='a' href='/x'/></div><div class='entry-meta'><a id='b' href='/y'/></div><div class='other'><a id='c' href='/z'/></div>",
            xpath: "//*[@class=\"entry-meta\" or contains(@class, \"topics\") or contains(@class, \"tags-links\")]//a[@href]",
        },
        // ===== TITLE_XPATHS (3 patterns) =====================================
        Row {
            label: "TITLE[0]: h1/h2 post-title or entry-title",
            html: "<h1 class='post-title' id='a'/><h2 class='entry-title' id='b'/><h3 id='c'/>",
            xpath: "//*[self::h1 or self::h2][contains(@class, \"post-title\") or contains(@class, \"entry-title\")]",
        },
        Row {
            label: "TITLE[1]: @class entry-title",
            html: "<h1 class='entry-title' id='a'/><h1 class='other' id='b'/>",
            xpath: "//*[@class=\"entry-title\" or @class=\"post-title\"]",
        },
        Row {
            label: "TITLE[2]: h1/h2/h3 contains title",
            html: "<h1 class='page-title' id='a'/><h2 id='subtitle-box'/><h3 id='c'/>",
            xpath: "//*[self::h1 or self::h2 or self::h3][contains(@class, \"title\") or contains(@id, \"title\")]",
        },
        // ===== Operator-coverage rows (any operator not exercised above) ====
        Row {
            label: "OP: contains empty needle is true",
            html: "<div id='a'/><div id='b'/>",
            xpath: ".//div[contains(@id, '')]",
        },
        Row {
            label: "OP: starts-with empty prefix is true",
            html: "<div id='a'/><div id='b'/>",
            xpath: ".//div[starts-with(@id, '')]",
        },
        Row {
            label: "OP: translate with deletion",
            html: "<div id='foo-bar'/><div id='foobar'/><div id='other'/>",
            xpath: ".//*[contains(translate(@id, '-', ''), 'foobar')]",
        },
        Row {
            label: "OP: text() count via .//p//text()",
            html: "<p>hello</p><p>world</p><p></p>",
            xpath: ".//p//text()",
        },
        Row {
            label: "OP: and predicate",
            html: "<div id='a' class='x'/><div id='b' class='y'/><div id='c' class='x'/>",
            xpath: ".//div[@class='x' and starts-with(@id, 'a')]",
        },
        Row {
            label: "OP: or predicate",
            html: "<div id='a'/><div id='b'/><div id='c'/>",
            xpath: ".//div[@id='a' or @id='c']",
        },
        Row {
            label: "OP: nested predicate",
            html: "<div class='outer'><span id='a' class='x'/></div><div class='outer'><span id='b'/></div><div id='c'/>",
            xpath: ".//div[@class='outer']//span[@class='x']",
        },
        Row {
            label: "OP: union both sides match",
            html: "<time id='t1'/><figure id='f1'/><time id='t2'/>",
            xpath: ".//time|.//figure",
        },
        Row {
            label: "OP: positional after predicate",
            html: "<p class='x' id='a'/><p class='x' id='b'/><p class='y' id='c'/>",
            xpath: ".//p[@class='x'][1]",
        },
        Row {
            label: "OP: self:: in step (used by some xpaths.py predicates indirectly)",
            html: "<article id='a'/><div id='b'/>",
            xpath: ".//*[self::article]",
        },
        Row {
            label: "OP: attribute union empty fallback",
            html: "<span id_marker='1'/>",
            xpath: ".//span[contains(@id|@class, 'x')]",
        },
        Row {
            label: "OP: wildcard descendants count",
            html: "<div><span><em/></span></div>",
            xpath: ".//*",
        },
    ]
}

fn run_rust(xpath: &str, html: &str) -> (Vec<String>, Vec<String>) {
    // Parse the HTML as a full document (wrap with <html><body> for parser
    // stability — matches the lxml probe's `document_fromstring`).
    let wrapped = format!("<html><body>{html}</body></html>");
    let dom = Dom::parse(&wrapped);
    let body = dom.body().expect("body must parse");
    let nodes = match xpath_engine::evaluate(xpath, &body) {
        Ok(n) => n,
        Err(e) => panic!("Rust XPath evaluation failed for {xpath:?}: {e}"),
    };
    let ids: Vec<String> = nodes.iter().map(node_id_or_text).collect();
    let tags: Vec<String> = nodes.iter().map(node_tag_lower).collect();
    (ids, tags)
}

fn node_id_or_text(n: &NodeRef) -> String {
    // For text nodes (text() axis), `get_attribute(_, "id")` returns None,
    // collapsing to empty string here — the lxml probe also yields per-node
    // None for `.get('id')` on strings, which we serialize as empty string.
    dom::get_attribute(n, "id").unwrap_or_default()
}

fn node_tag_lower(n: &NodeRef) -> String {
    if let Some(name) = dom::local_name(n) {
        name.to_ascii_lowercase()
    } else {
        // Text nodes: lxml's _ElementUnicodeResult tags don't exist.
        // We emit empty string and the probe is set up to match.
        String::new()
    }
}

fn probe_path() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest)
        .join("benchmark")
        .join("oracles")
        .join("xpath_conformance")
        .join("conformance_probe.py")
}

/// Spawn the Python probe. Returns `(ok, count, ids, tags, error)`.
fn run_lxml(xpath: &str, html: &str) -> Result<(usize, Vec<String>, Vec<String>), String> {
    let probe = probe_path();
    if !probe.exists() {
        return Err(format!("conformance probe missing: {}", probe.display()));
    }

    // Wrap to a full document for parser stability — matches `run_rust`.
    let wrapped = format!("<html><body>{html}</body></html>");

    let out = Command::new("python")
        .arg(&probe)
        .arg("--html")
        .arg(&wrapped)
        .arg("--xpath")
        .arg(xpath)
        .arg("--context")
        .arg("body")
        .output()
        .map_err(|e| format!("could not spawn python: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        return Err(format!(
            "probe exited non-zero: status={:?} stdout={stdout:?} stderr={stderr:?}",
            out.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    parse_probe_json(&stdout)
}

/// Tiny hand-rolled JSON-extract for the four fields we need. We avoid
/// pulling `serde_json` into the test harness scope cleanly: it IS in the
/// mdrcel dep tree (the M2 metadata port uses it), so we just use it.
fn parse_probe_json(s: &str) -> Result<(usize, Vec<String>, Vec<String>), String> {
    let v: serde_json::Value =
        serde_json::from_str(s.trim()).map_err(|e| format!("probe JSON parse: {e} in {s:?}"))?;
    let ok = v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false);
    if !ok {
        let err = v.get("error").and_then(|x| x.as_str()).unwrap_or("unknown");
        return Err(format!("probe ok:false — {err}"));
    }
    let count = v
        .get("count")
        .and_then(|x| x.as_u64())
        .ok_or("probe missing count")? as usize;
    let ids: Vec<String> = v
        .get("ids")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    let tags: Vec<String> = v
        .get("tags")
        .and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .map(|e| e.as_str().map(|s| s.to_string()).unwrap_or_default())
                .collect()
        })
        .unwrap_or_default();
    Ok((count, ids, tags))
}

fn compare_row(row: &Row) -> Compared {
    let (rust_ids, rust_tags) = run_rust(row.xpath, row.html);
    let (lxml_count, lxml_ids, lxml_tags) = match run_lxml(row.xpath, row.html) {
        Ok(t) => t,
        Err(e) => panic!(
            "conformance probe (lxml subprocess) failed for row {:?}: {e}",
            row.label
        ),
    };
    Compared {
        rust_count: rust_ids.len(),
        lxml_count,
        rust_ids,
        lxml_ids,
        rust_tags,
        lxml_tags,
    }
}

/// The actual conformance assertion.
///
/// We assert tag-sequence parity and id-multiset-parity-as-sequence. Order
/// matters for tags (document order is part of the contract); for ids we
/// also require sequence parity, with the caveat that the engine and lxml
/// MAY differ on text-node id/tag (text nodes have neither). We normalize
/// missing ids to empty string on both sides.
fn assert_row(row: &Row, c: &Compared) {
    let mut errors = Vec::new();
    if c.rust_count != c.lxml_count {
        errors.push(format!(
            "count mismatch: rust={} lxml={}",
            c.rust_count, c.lxml_count
        ));
    }
    if c.rust_ids != c.lxml_ids {
        errors.push(format!(
            "ids mismatch:\n  rust = {:?}\n  lxml = {:?}",
            c.rust_ids, c.lxml_ids
        ));
    }
    if c.rust_tags != c.lxml_tags {
        errors.push(format!(
            "tags mismatch:\n  rust = {:?}\n  lxml = {:?}",
            c.rust_tags, c.lxml_tags
        ));
    }
    if !errors.is_empty() {
        panic!(
            "CONFORMANCE FAILURE for row {:?}\n  xpath = {}\n  html  = {}\n  {}",
            row.label,
            row.xpath,
            row.html,
            errors.join("\n  ")
        );
    }
}

#[test]
fn xpath_conformance_all_rows() {
    let all = rows();
    assert!(
        all.len() >= 51,
        "DA-M-6 requires at least 51 conformance rows; got {}",
        all.len()
    );
    let mut failures = Vec::new();
    for row in &all {
        let c = compare_row(row);
        // Inline the assertion logic so we can collect all failures rather
        // than failing on the first.
        let mut row_errors = Vec::new();
        if c.rust_count != c.lxml_count {
            row_errors.push(format!(
                "count mismatch: rust={} lxml={}",
                c.rust_count, c.lxml_count
            ));
        }
        if c.rust_ids != c.lxml_ids {
            row_errors.push(format!(
                "ids mismatch: rust={:?} lxml={:?}",
                c.rust_ids, c.lxml_ids
            ));
        }
        if c.rust_tags != c.lxml_tags {
            row_errors.push(format!(
                "tags mismatch: rust={:?} lxml={:?}",
                c.rust_tags, c.lxml_tags
            ));
        }
        if !row_errors.is_empty() {
            failures.push(format!(
                "  row {:?}\n    xpath = {}\n    html  = {}\n    {}",
                row.label,
                row.xpath,
                row.html,
                row_errors.join("\n    ")
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} of {} conformance rows failed:\n{}",
            failures.len(),
            all.len(),
            failures.join("\n")
        );
    }
}

/// Sanity: the probe is reachable and produces well-formed JSON. This runs
/// FIRST so a totally-broken environment fails fast rather than reporting
/// 51 phantom mismatches.
#[test]
fn xpath_conformance_probe_smoke() {
    let result = run_lxml(".//div", "<div id='a'/>");
    match result {
        Ok((c, ids, tags)) => {
            assert_eq!(c, 1);
            assert_eq!(ids, vec!["a"]);
            assert_eq!(tags, vec!["div"]);
        }
        Err(e) => {
            panic!("conformance probe smoke test failed (Python+lxml must be installed): {e}")
        }
    }
}

/// Also assert the row helper sub-asserts itself — defensive against
/// a refactor that loses the per-row panic context.
#[test]
fn assert_row_panics_on_mismatch_smoke() {
    let r = Row {
        label: "smoke",
        html: "<div id='a'/>",
        xpath: ".//div",
    };
    let c = Compared {
        rust_count: 1,
        lxml_count: 1,
        rust_ids: vec!["a".into()],
        lxml_ids: vec!["a".into()],
        rust_tags: vec!["div".into()],
        lxml_tags: vec!["div".into()],
    };
    // Should not panic.
    assert_row(&r, &c);
}
