//! M5 Stage 2 — corpus-wide markdown equivalence diff harness.
//!
//! Where the Stage 3-B `trafilatura_extract_content_gate` pins the
//! `extract_content` lxml-Element output as canonical XML (structural-token
//! comparison), this gate pins the END of the pipeline: mdrcel's
//! `extract_to_markdown` against Python's
//! `trafilatura.extract(raw, output_format="markdown")` byte-for-byte.
//!
//! # Stage 2 success criterion
//!
//! Stage 2 succeeds when this harness **compiles, runs to completion against
//! all 51 corpus snapshots, and emits an actionable divergence report**. It
//! is NOT required to be all-green; triage is Stage 3's job. The harness
//! therefore collects every divergence into a single buffer and panics ONCE
//! at the end with the full report, instead of bailing on the first miss.
//!
//! # The comparison shape
//!
//! Both sides emit a Python/Rust `str` and are NFC-normalised by their own
//! pipelines (Python at `core.py:98`; Rust at `lib.rs:679-680`). The harness
//! NFC-normalises both ONCE MORE on its own, belt-and-braces — the contract
//! is explicit even when both sides already normalised. Comparison is then
//! strict byte-equality of the resulting UTF-8.
//!
//! # On divergence
//!
//! Every fixture that fails contributes:
//! - rust char count vs python char count
//! - first byte index of divergence
//! - a 100-char window on each side around that index
//! - a coarse "bucket" tag (whitespace-only / empty-vs-non / content)
//!
//! The end-of-report tally totals each bucket so Stage 3 can pick the
//! highest-value fix target.

use mdrcel::{extract_to_markdown, Options};

mod common;
use common::{
    classify, escape, first_diff_index, nfc, run_oracle, window_around, workspace_path, Bucket,
};

/// Fixtures where Python's `trafilatura.extract` is the under-extractor
/// (or its output is anti-inversion-violating in a corpus-specific way).
/// **Each entry MUST have a corresponding ADR** in `wrk_docs/m5-allowlist/`
/// — see the ADR for the per-fixture rationale. Divergence still counts
/// against the substantive pass tally, but is reported separately under
/// `allowlist_python_bug` so the verdict is honest.
///
/// **Per-fixture filename only** (basename, no path); the harness checks
/// the fixture's `.html` filename against this list during the divergence
/// classification step.
const PYTHON_UNDER_EXTRACT_ALLOWLIST: &[&str] = &[
    // EDGAR SEC 10-K (legacy SGML wrap). Python's bare_extraction returns
    // empty on this structurally-valid filing; mdrcel extracts the same
    // ~75KB of substantive content the rest of the trafilatura cascade
    // would emit. ADR: wrk_docs/m5-allowlist/41d2afac.md.
    "41d2afac25d46010.html",
    // DFIN XBRL 10-K filing — Apple 10-K relative. Single empty table
    // cell emission disagreement at byte 32335 within a 375KB filing
    // (rust 375876 vs python 375714 chars — >99.95% identical). ADR:
    // wrk_docs/m5-allowlist/683d5643.md.
    "683d5643b173c7fd.html",
    // DFIN XBRL 10-K filing — Berkshire Hathaway. Source HTML uses
    // `&#153;` (Windows-1252 trademark sign encoding). HTML5 spec
    // requires CP-1252 remap of 0x80-0x9F numeric references to printable
    // glyphs (U+2122 here); mdrcel follows the spec, lxml strips the
    // control character. ADR: wrk_docs/m5-allowlist/dc8ba3c0.md.
    "dc8ba3c086153274.html",
    // Rust blog index page (blog.rust-lang.org). Python's link-density
    // filter (`htmlprocessing.link_density_test_tables`) rejects the
    // 76.8%-link-density `<table class="post-list">` that IS the page's
    // content. Result: 162 chars (description only). mdrcel preserves
    // the post listing (~17KB of post titles + URLs + dates). ADR:
    // wrk_docs/m5-allowlist/9c64e8e3.md.
    "9c64e8e3fcd844d4.html",
    // Hacker News front page (news.ycombinator.com). Python over-extracts
    // the `<td class="pagetop">` site-nav block ("Hacker News | new | past
    // | comments | ask | show | jobs | submit | login") as the opening
    // ~215 chars of output and emits the story listing flat (one cell per
    // line, literal `|` pipes between). mdrcel emits the listing as a
    // proper markdown table and omits the nav chrome. ADR:
    // wrk_docs/m5-allowlist/0f63a2a5.md.
    "0f63a2a5a5620b74.html",
    // Wikipedia article (M5 Stage 6j-b, 2026-05-22). Python's
    // `extract(output_format='markdown')` re-runs `handle_formatting`
    // during `xmltotxt(body, formatting=True)` and STRUCTURALLY
    // FRACTURES the source `<sup>[<i><a><span>citation needed</span></a></i>]</sup>`
    // into multiple sibling `<hi>` elements at different tree positions
    // (closing `]` ends up as orphan-tail at body level; opening `[` ends
    // up in a fresh `<p>`). Python's XML output preserves the citation
    // intact; mdrcel matches Python's XML (the faithful body). 3-byte
    // diff at byte 30291 in a 65,906-char fixture. ADR:
    // wrk_docs/m5-allowlist/86df4d2e.md.
    "86df4d2e654952e4.html",
    // Rust blog post (M5 Stage 6j-c, 2026-05-22). html5ever follows
    // HTML5 §13.2.5.51 (in-body start-tag dispatch for `<pre>`):
    // "If the next token is a U+000A LINE FEED (LF) character token,
    // then ignore that token..." lxml's HTMLParser does NOT implement
    // this spec rule and preserves the leading `\n` as `<pre>.text`.
    // Downstream, `xmltotxt`'s `<code>` formatting branch emits an
    // empty ```` ```\n``` ```` fence pair from the preserved `\n` —
    // a 7-byte content-free artefact. mdrcel parses per spec, so no
    // empty fence is emitted. Replicating the artefact would require
    // patching html5ever (invasive) or injecting leading `\n` into
    // every `<pre>` (destructive). 7-byte diff at byte 1106 of a
    // 20,602-char fixture. ADR: wrk_docs/m5-allowlist/39ca4af9.md.
    "39ca4af9befa0524.html",
    // Apple FR — French Wikipedia Apple Inc. article (M6 Stage 1 pivot,
    // 2026-05-22). The M5 deferred ADR claimed Python's `xmltotxt` has a
    // `<sup class="reference">` paragraph-break branch and mdrcel should
    // mirror it. M6 Stage 1 verified Python source and found NO such
    // branch: `htmlprocessing.convert_tags` clears the `class` attribute
    // and renames `<sup>` → `<hi rend="#sup">` BEFORE `xmltotxt` sees
    // the tree (`htmlprocessing.py:402-407`); `xml.py:process_element`
    // (`xml.py:253-351`) has no `<sup>`-class branch at all. The
    // paragraph break is a side-effect of `handle_paragraphs`/
    // `handle_formatting` (`main_extractor.py:108-115, 272-351`) running
    // on inline `<hi rend="#sup">` when `formatting=True` — identical
    // pattern to the already-allowlisted 86df4d2e fracture. Smoking gun:
    // `trafilatura.extract(output_format='txt')` on the same fixture
    // produces inline references matching mdrcel exactly; only the
    // markdown path diverges. Mirroring Python's markdown bug would be
    // anti-inversion-violating. ~145 bytes of structural divergence over
    // ~30 reference markers, net ~1 byte in a 122,740-char fixture.
    // ADR: wrk_docs/m5-allowlist/507b9cdb.md (supersedes
    // wrk_docs/m5-deferred/507b9cdb.md).
    "507b9cdbe036bf58.html",
];

/// Fixtures where **mdrcel** is the buggy side — divergence is a known
/// extraction defect on the Rust port, not an anti-inversion-clean
/// Python bug. Each entry MUST have a corresponding ADR in
/// `wrk_docs/m5-deferred/` describing the diagnosis AND the deferred
/// remediation milestone (typically M6 or later).
///
/// **Semantic distinction from `PYTHON_UNDER_EXTRACT_ALLOWLIST`:**
/// allowlist says "Python is wrong; matching it would be anti-inversion-
/// violating bug-for-bug replication" — deferred says "mdrcel is wrong;
/// fixing it is real port work scoped to a future milestone, but the
/// defect is documented and the gate should not silently regress on
/// other fixtures while we live with this one."
///
/// A fixture **MUST NOT** appear in both lists. The harness enforces
/// nothing structural here — supervisor discipline is the only safety
/// rail — but the per-fixture ADRs cite each other when the choice was
/// non-obvious.
const DEFERRED_KNOWN_DEFECT: &[&str] = &[
    // FRED (St. Louis Fed) economic-data page (`e339ce76eb1cba73.html`)
    // — FORMERLY deferred under the misdiagnosed "jusText classifier
    // returns 487 chars" hypothesis. M6 Stage 3 anti-inversion
    // verification: the classifier itself is structurally faithful, but
    // TWO independent surface bugs combined to truncate FRED's
    // markdown output to a 5187-char noscript stub:
    //   (1) Threshold mismatch — `try_justext` was calling
    //       `classify_and_revise` with jusText's library defaults
    //       (length_low=70, stopwords_high=0.32, no_headings=False) when
    //       trafilatura's `custom_justext` (external.py:121-126)
    //       overrides them to (50, 150, 0.1, 0.2, 0.25, no_headings=True).
    //       The wider net is what classifies English narrative paragraphs
    //       at 0.10-0.30 stopword density as `good` instead of `bad`.
    //   (2) `cleaned_tree_backup` missing — Python (core.py:281,297)
    //       deep-copies the post-`tree_cleaning` body BEFORE
    //       `convert_tags`/`extract_content` mutate it, then hands the
    //       copy to `compare_extraction` so `justext_rescue` sees the
    //       full un-extracted tree. mdrcel passed the same `body`
    //       NodeRef that `extract_content` had stripped down to ~10% of
    //       its element content, so jusText saw a truncated tree.
    // Fix landed in `justext_core::custom_justext` (new helper) plus
    // `bare_extraction_with_cascade::cleaned_body_backup` (deep_clone
    // before convert_tags). Fixture now byte-equivalent to Python.
    // (No allowlist entry — mdrcel was the buggy side on both surfaces.)
    // PBS (CNN-lite) news article (`e1106c5e26712078.html`) — FORMERLY
    // deferred under the misdiagnosed `BODY_XPATH selection divergence`
    // hypothesis. M6 Stage 2 anti-inversion verification: BOTH engines
    // select the same `<ul>` of stories; the divergence was in mdrcel's
    // readability_fork `Document::summary` retry loop, which re-parsed
    // `self.html` on every attempt (an M2 Mozilla flag-sieve pattern,
    // HLD §m-3) instead of mutating `self.doc` in place across attempts
    // like Python's `readability_lxml.Document.summary`. Fix landed in
    // `src/trafilatura/readability_fork.rs::Document::summary` + body-
    // fallback detachment into a fresh wrapper to survive the rcdom
    // Drop quirk. Fixture is now byte-equivalent to Python and counted
    // as substantive. (No allowlist entry — mdrcel was the buggy side.)
];

/// All 51 corpus snapshots — enumerated literally from
/// `benchmark/corpus/snapshots/*.html`. The gate is corpus-wide by design
/// (M5 supervisor decision: 51 is small enough that sampling buys nothing).
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
fn trafilatura_markdown_gate() {
    let mut pass = 0usize;
    let total = FIXTURES.len();
    let mut report = String::new();
    let mut bucket_empty = 0usize;
    let mut bucket_ws = 0usize;
    let mut bucket_content = 0usize;
    // Fixtures that diverged but appear in PYTHON_UNDER_EXTRACT_ALLOWLIST.
    // Reported separately; not counted as substantive passes (the
    // substantive count + allowlist count + deferred count + bucket
    // totals MUST equal `total` so no fixture is silently dropped).
    let mut allowlist_python_bug = 0usize;
    // Fixtures that diverged AND appear in DEFERRED_KNOWN_DEFECT. mdrcel
    // is the buggy side; the divergence is pinned to a future-milestone
    // remediation ADR rather than allowlisted as anti-inversion-clean.
    let mut deferred_known_defect = 0usize;

    for fixture_rel in FIXTURES {
        let path = workspace_path(fixture_rel);
        assert!(
            path.is_file(),
            "M5 Stage 2 fixture missing: {} (expected at {})",
            fixture_rel,
            path.display(),
        );

        // Read raw bytes on both sides (same decoding contract as the
        // Stage 3-B gate uses, lib.rs:101-103).
        let bytes = std::fs::read(&path)
            .unwrap_or_else(|e| panic!("cannot read fixture {}: {e}", path.display()));
        let html = String::from_utf8_lossy(&bytes);

        // 1. Rust markdown output.
        let rust_md_raw = match extract_to_markdown(&html, None, &Options::default()) {
            Ok(s) => s,
            Err(e) => {
                report.push_str(&format!(
                    "  ERR   {} — extract_to_markdown returned Err: {e:?}\n",
                    fixture_rel,
                ));
                bucket_content += 1;
                continue;
            }
        };
        // 2. Python markdown output (subprocess oracle).
        let python_md_raw = match run_oracle("--markdown", &path) {
            Ok(s) => s,
            Err(e) => panic!(
                "M5 STAGE 2 GATE: Python oracle failure on {} — {e}",
                fixture_rel,
            ),
        };

        // 3. NFC-normalise both (belt-and-braces — both pipelines already
        //    NFC-normalise; this makes the contract explicit at gate level).
        let rust_md: String = nfc(&rust_md_raw);
        let python_md: String = nfc(&python_md_raw);

        if rust_md == python_md {
            pass += 1;
            continue;
        }

        // Diverged. Check the allowlist + deferred lists FIRST — each
        // gets a distinct tag and bypasses the bucket counters
        // (allowlist = Python-side bug per ADR in `wrk_docs/m5-allowlist/`;
        // deferred = mdrcel-side bug per ADR in `wrk_docs/m5-deferred/`).
        // A fixture must not be in both lists; the gate assertion below
        // catches any accounting drift.
        let basename = std::path::Path::new(fixture_rel)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        let allowlisted = PYTHON_UNDER_EXTRACT_ALLOWLIST.contains(&basename);
        let deferred = DEFERRED_KNOWN_DEFECT.contains(&basename);
        assert!(
            !(allowlisted && deferred),
            "M5 gate: fixture {basename} appears in BOTH allowlist and deferred lists; \
             pick one — allowlist = anti-inversion-clean Python bug, deferred = mdrcel defect",
        );

        // Classify either way so the per-fixture report still shows the
        // bucket the divergence would have fallen into.
        let bucket = classify(&rust_md, &python_md);
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

        // First byte-index of divergence + 100-char windows on each side.
        let first_diff_byte = first_diff_index(rust_md.as_bytes(), python_md.as_bytes());
        let rust_window = window_around(&rust_md, first_diff_byte, 100);
        let python_window = window_around(&python_md, first_diff_byte, 100);

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
            rust_md.chars().count(),
            python_md.chars().count(),
            first_diff_byte,
            escape(&rust_window),
            escape(&python_window),
        ));
    }

    eprintln!("\n=== M5 markdown corpus gate verdict (BLOCKER) ===");
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

    // Honest accounting invariant: every fixture lands in exactly one of
    // `pass`, `bucket_empty`, `bucket_ws`, `bucket_content`,
    // `allowlist_python_bug`, `deferred_known_defect`. Catches silent
    // fixture-drop regressions.
    let accounted = pass
        + bucket_empty
        + bucket_ws
        + bucket_content
        + allowlist_python_bug
        + deferred_known_defect;
    assert_eq!(
        accounted, total,
        "M5 markdown gate accounting drift: pass={pass}, empty={bucket_empty}, \
         ws={bucket_ws}, content={bucket_content}, allowlist={allowlist_python_bug}, \
         deferred={deferred_known_defect} sum to {accounted} but total={total}",
    );

    // M5 BLOCKER gate: GREEN when every fixture lands in exactly one of
    // `pass` (substantive byte-equivalence), `allowlist_python_bug`
    // (Python is wrong; ADR under `wrk_docs/m5-allowlist/`), or
    // `deferred_known_defect` (mdrcel is wrong but pinned to a future
    // milestone; ADR under `wrk_docs/m5-deferred/`). Any other bucket
    // count > 0 indicates either a regression on a previously-passing
    // fixture OR a brand-new divergence that has not yet been triaged
    // into one of the two ADR lists.
    if pass + allowlist_python_bug + deferred_known_defect != total {
        panic!(
            "M5 markdown gate divergence: {pass}/{total} substantive + \
             {allowlist_python_bug} allowlisted + {deferred_known_defect} deferred. \
             Untriaged buckets: empty-vs-non={bucket_empty}, whitespace-only={bucket_ws}, \
             content-mismatch={bucket_content}. \
             See per-fixture report above for first-diff windows. \
             Either fix the regression OR triage the new divergence into \
             PYTHON_UNDER_EXTRACT_ALLOWLIST (with a wrk_docs/m5-allowlist/ ADR) \
             or DEFERRED_KNOWN_DEFECT (with a wrk_docs/m5-deferred/ ADR).",
        );
    }
}

