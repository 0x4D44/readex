//! Stage-0 parser-equivalence **BLOCKER** gate (HLD §6.1, supervisor M-1/m-4).
//!
//! > Require post-tokenizer (`metrics.rs::tokens`) token-sequence identity
//! > between jsdom-29.1.1 `body.textContent` and `html5ever + rcdom` for ALL
//! > 7 gold snapshots AND the table-heavy non-gold EDGAR / HMRC snapshots,
//! > BEFORE Stage 1a may begin.
//!
//! This is the concrete realisation of architect risk #1 (the highest). A
//! token mismatch at the substrate level is unfixable downstream and would
//! silently corrupt every later Coverage number, so a failure here is a
//! **Stage-0 design-decision trigger** — the parser/DOM choice changes
//! (rcdom → kuchikiki, HLD §3), never a Stage-1 workaround. The honest
//! outcome on divergence is to STOP with evidence (raw char-diff + token
//! diff), not to weaken the gate (project `honest-failure-over-synthesis`
//! doctrine / Bug-E2).
//!
//! ## Mechanism
//!
//! For each named snapshot:
//! 1. run `benchmark/oracles/readability-js/body_text.mjs` (the Stage-0 probe)
//!    under **the oracle's own jsdom 29.1.1** to get raw
//!    `document.body.textContent`;
//! 2. parse the same bytes with the `mdrcel` html5ever+rcdom facade and take
//!    `dom.body()` `text_content` (the thing being gated);
//! 3. push both through [`tokens`] (a **byte-exact mirror** of
//!    `benchmark/src/metrics.rs::tokens`, self-checked below);
//! 4. assert the two token **sequences** are identical (not set, not Jaccard
//!    — sequence);
//! 5. on any divergence, print the first differing token index, a window of
//!    surrounding tokens, and the first raw `textContent` character
//!    difference, then fail.
//!
//! ## Scope of the equivalence claim (honest, not over-claimed)
//!
//! Passing proves token-sequence equivalence **for this snapshot corpus**,
//! which the per-snapshot guard proves contains ZERO non-whitespace stray
//! text directly inside table parts. html5ever and jsdom are **known to
//! diverge** on that class (witness:
//! `known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent`);
//! the guard self-polices it so any future corpus addition in that class
//! re-triggers the HLD §6.1 rcdom → kuchikiki design decision. This is
//! **not** a blanket "the DOM substrate is faithful for all inputs" claim —
//! it is bounded, named, and regression-pinned.
//!
//! Run: `cargo test --test parser_equivalence_gate`
//! (use the DEC-G5 MSVC env: `CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0`).
//!
//! Node is required (the gate spawns it exactly as `oracle.rs` spawns the
//! adapter). If `node` is absent the test **fails loudly** rather than
//! skipping — a parser-equivalence gate that silently no-ops would be the
//! Bug-E2 conflation this whole HLD exists to prevent.

use std::path::{Path, PathBuf};
use std::process::Command;

use mdrcel::readability::dom::{self, Dom, NodeRef};
use unicode_normalization::UnicodeNormalization;

/// **Byte-exact mirror of `benchmark/src/metrics.rs::tokens`** (HLD §8 / §6.1
/// — the gate must use *the harness tokenizer semantics*).
///
/// `benchmark` is a binary-only crate, so its `metrics` module cannot be
/// imported from here; reproducing the (tiny, fully-specified) pipeline is
/// the faithful option. The pipeline, verbatim from `metrics.rs`:
///
/// ```text
/// text.split(char::is_whitespace)
///     .filter(|fragment| !fragment.is_empty())
///     .map(|fragment| fragment.to_lowercase().nfc().collect::<String>())
///     .collect()
/// ```
///
/// `unicode-normalization` is pinned to the same `"0.1"` as
/// `benchmark/Cargo.toml`, and [`tokenizer_mirror_is_faithful`] pins this
/// against hand-derived vectors so any drift from the harness fails loudly.
fn tokens(text: &str) -> Vec<String> {
    text.split(char::is_whitespace)
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| fragment.to_lowercase().nfc().collect::<String>())
        .collect()
}

/// One snapshot under the gate: content-addressed filename + a human label +
/// whether it is gold (all 7) or a table-heavy non-gold EDGAR/HMRC doc.
struct GateSnapshot {
    file: &'static str,
    label: &'static str,
    gold: bool,
}

/// The exact §6.1 snapshot set: **all 7 gold** (from
/// `benchmark/corpus/gold/gold.tsv`) **AND the table-heavy non-gold EDGAR /
/// HMRC** snapshots (from `benchmark/corpus/urls.tsv`).
///
/// "Table-heavy non-gold EDGAR/HMRC" = the EDGAR 10-K/10-Q financial filings
/// and the gov.uk HMRC rate pages that carry many tables and are NOT in the
/// gold set. The EDGAR *filing-index* pages are **included too** (MINOR-1):
/// they were once excluded as "EDGAR listing shape", but that was inaccurate —
/// the filing index is rendered as HTML tables (~2–3 tables / ~90–185 cells),
/// so they are squarely inside the §6.1 / M-1 stress intent (implied-`<tbody>`
/// / foster-parenting under tables) and carry no extra exclusion rationale.
const GATE_SNAPSHOTS: &[GateSnapshot] = &[
    // ---- the 7 gold (gold.tsv) ----
    GateSnapshot {
        file: "0f115db062b7c0dd.html",
        label: "GOLD example.com",
        gold: true,
    },
    GateSnapshot {
        file: "a604eb8a03efa82d.html",
        label: "GOLD gov.uk hub",
        gold: true,
    },
    GateSnapshot {
        file: "ae2c2184beb6d264.html",
        label: "GOLD Apple Wikipedia",
        gold: true,
    },
    GateSnapshot {
        file: "9c8f49f04f792f81.html",
        label: "GOLD Wm Morrison Wikipedia",
        gold: true,
    },
    GateSnapshot {
        file: "9a1590d0917107a7.html",
        label: "GOLD Apple FY2025 10-K EDGAR (62 tables)",
        gold: true,
    },
    GateSnapshot {
        file: "9ec7aaf8edb71ac1.html",
        label: "GOLD gov.uk HMRC employer rates 2025-26 (23 tables)",
        gold: true,
    },
    GateSnapshot {
        file: "577e61856ca2770d.html",
        label: "GOLD Apple FY2019 10-K EDGAR (124 tables)",
        gold: true,
    },
    // ---- table-heavy NON-GOLD EDGAR / HMRC (urls.tsv) ----
    GateSnapshot {
        file: "41d2afac25d46010.html",
        label: "EDGAR IBM FY2008 10-K (legacy, ~1500 <font>)",
        gold: false,
    },
    GateSnapshot {
        file: "dc8ba3c086153274.html",
        label: "EDGAR Berkshire FY2024 10-K (very large, table-heavy)",
        gold: false,
    },
    GateSnapshot {
        file: "683d5643b173c7fd.html",
        label: "EDGAR Microsoft FY2025 10-K (table-heavy)",
        gold: false,
    },
    GateSnapshot {
        file: "340e6571c584979a.html",
        label: "EDGAR Apple Q2-FY2026 10-Q (table-heavy)",
        gold: false,
    },
    GateSnapshot {
        file: "803b534a50a3f584.html",
        label: "HMRC income tax rates & allowances (7 tables)",
        gold: false,
    },
    GateSnapshot {
        file: "d159708a94e68ab6.html",
        label: "HMRC employer rates 2024-25 (22 tables)",
        gold: false,
    },
    // ---- EDGAR filing-index pages (urls.tsv) — tabular, NOT link-lists ----
    // Earlier excluded as "EDGAR listing shape"; that rationale was wrong —
    // these pages ARE table-heavy (the filing index is rendered as HTML
    // tables), so they belong in the §6.1 stress set (MINOR-1).
    GateSnapshot {
        file: "e6037cf1c861d089.html",
        label: "EDGAR filing index (~3 tables / ~185 cells)",
        gold: false,
    },
    GateSnapshot {
        file: "6c688ba250fbc628.html",
        label: "EDGAR filing index (~2 tables / ~90 cells)",
        gold: false,
    },
];

/// Workspace root. `CARGO_MANIFEST_DIR` for the `mdrcel` crate IS the
/// workspace root in this layout (`Cargo.toml` + `src/lib.rs` at root), so the
/// corpus / probe are reached relative to it (working-directory independent,
/// same technique as the harness's `corpus_dir`).
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn snapshot_path(file: &str) -> PathBuf {
    workspace_root()
        .join("benchmark")
        .join("corpus")
        .join("snapshots")
        .join(file)
}

fn probe_script() -> PathBuf {
    workspace_root()
        .join("benchmark")
        .join("oracles")
        .join("readability-js")
        .join("body_text.mjs")
}

/// Run the jsdom probe and return raw `document.body.textContent`.
///
/// Spawns `node body_text.mjs <abs-snapshot>` exactly as `oracle.rs` spawns
/// the adapter (bare `node`, absolute snapshot path, stdout piped, stderr
/// inherited for diagnostics). A spawn failure or non-zero exit is a hard
/// `Err` — the gate never treats "no parseable jsdom output" as success.
fn jsdom_body_text(snapshot_abs: &Path) -> Result<String, String> {
    let script = probe_script();
    assert!(
        script.is_file(),
        "Stage-0 probe missing: {} (HLD §6.1 aid)",
        script.display()
    );
    let out = Command::new("node")
        .arg(&script)
        .arg(snapshot_abs)
        .output()
        .map_err(|e| {
            format!(
                "failed to spawn `node` for the parser-equivalence probe: {e} \
                 (Node >=20 must be installed and on PATH — the gate spawns it \
                 exactly as the readability-js oracle adapter does; a \
                 silently-skipped gate is the Bug-E2 conflation this gate \
                 exists to prevent)"
            )
        })?;
    if !out.status.success() {
        return Err(format!(
            "jsdom probe exited {:?} for {} (stderr: {})",
            out.status.code(),
            snapshot_abs.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout)
        .map_err(|e| format!("jsdom probe stdout was not valid UTF-8: {e}"))
}

/// First char-level difference between two strings, as a human-readable
/// window (HLD §6.1 — "raw `textContent` char-level diff is reported for
/// diagnosis"). Reports the char index and a context window from each side.
fn first_char_diff(a: &str, b: &str) -> String {
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let n = av.len().min(bv.len());
    let mut i = 0;
    while i < n && av[i] == bv[i] {
        i += 1;
    }
    if i == n && av.len() == bv.len() {
        return "(* strings are byte-identical — divergence is tokenizer-only, \
                which is impossible for this pipeline; investigate *)"
            .to_string();
    }
    let lo = i.saturating_sub(40);
    let win = |v: &[char]| -> String {
        let hi = (i + 40).min(v.len());
        v[lo..hi]
            .iter()
            .collect::<String>()
            .escape_debug()
            .to_string()
    };
    format!(
        "first char diff at index {i} (jsdom len {} vs rcdom len {}):\n  \
         jsdom  …{}…\n  rcdom  …{}…\n  jsdom[{i}]={:?}  rcdom[{i}]={:?}",
        av.len(),
        bv.len(),
        win(&av),
        win(&bv),
        av.get(i),
        bv.get(i),
    )
}

/// First differing token index + a window, for the post-tokenizer diff.
fn first_token_diff(a: &[String], b: &[String]) -> String {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n && a[i] == b[i] {
        i += 1;
    }
    let lo = i.saturating_sub(6);
    let win = |v: &[String]| -> String {
        let hi = (i + 6).min(v.len());
        format!("{:?}", &v[lo..hi])
    };
    format!(
        "first token diff at index {i} (jsdom {} tokens vs rcdom {} tokens):\n  \
         jsdom  …{}…\n  rcdom  …{}…\n  jsdom[{i}]={:?}  rcdom[{i}]={:?}",
        a.len(),
        b.len(),
        win(a),
        win(b),
        a.get(i),
        b.get(i),
    )
}

/// `true` iff `s` contains at least one non-whitespace char (Unicode
/// `White_Space`, the same class the harness tokenizer splits on — so "stray
/// text" here means text the tokenizer would actually keep, exactly the text
/// whose foster-parent position differs between html5ever and jsdom).
fn has_non_whitespace(s: &str) -> bool {
    s.chars().any(|c| !c.is_whitespace())
}

/// First non-whitespace `#text` node whose **direct parent element** is a
/// table part (`table`/`tbody`/`thead`/`tfoot`/`tr`), as
/// `(parent_tag_lowercase, offending_text)` — or `None` if the subtree has
/// none (the all-current-snapshots case).
///
/// This is the html5ever ≢ jsdom foster-parent witness class (see
/// `known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent`):
/// the two parsers place such stray text in *different* positions, so its
/// presence means the gate can no longer certify equivalence for that
/// snapshot. Walks the tree with the facade's own primitives only.
fn first_stray_table_part_text(node: &NodeRef) -> Option<(String, String)> {
    // Element-tag check: `dom::tag_name` is UPPER-cased; compare upper-case.
    if let Some(tag) = dom::tag_name(node)
        && matches!(tag.as_str(), "TABLE" | "TBODY" | "THEAD" | "TFOOT" | "TR")
    {
        for child in dom::child_nodes(node) {
            if dom::is_text(&child) {
                // text_content of a Text node is its own `data`.
                let data = dom::text_content(&child);
                if has_non_whitespace(&data) {
                    return Some((tag.to_ascii_lowercase(), data));
                }
            }
        }
    }
    for child in dom::child_nodes(node) {
        if let Some(hit) = first_stray_table_part_text(&child) {
            return Some(hit);
        }
    }
    None
}

/// THE BLOCKER GATE (HLD §6.1). Token-sequence identity, all 15 snapshots.
///
/// One `#[test]` over the whole set so a single `cargo test` reports the
/// complete per-snapshot verdict (PASS/FAIL with evidence) in one place — the
/// reviewable artefact the HLD asks for. The first divergence STOPs the test
/// with full evidence (honest-failure doctrine).
#[test]
fn parser_equivalence_blocker_gate() {
    // Self-check the tokenizer mirror FIRST: if it has drifted from
    // metrics.rs the whole gate is meaningless, so fail before doing work.
    tokenizer_mirror_is_faithful();

    let mut pass = 0usize;
    let total = GATE_SNAPSHOTS.len();
    let mut report = String::new();

    for s in GATE_SNAPSHOTS {
        let path = snapshot_path(s.file);
        assert!(
            path.is_file(),
            "gate snapshot missing: {} ({})",
            path.display(),
            s.label
        );

        // 1. jsdom 29.1.1 raw body.textContent (the oracle's parser).
        let jsdom_text = match jsdom_body_text(&path) {
            Ok(t) => t,
            Err(e) => panic!(
                "PARSER-EQUIVALENCE GATE: probe failure on [{}] {} — {e}\n\
                 (This is a hard STOP, not a skip: the gate cannot certify \
                 the substrate without the oracle's jsdom.)",
                s.file, s.label
            ),
        };

        // 2. html5ever + rcdom facade text_content on the SAME bytes.
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));

        // 2a. GUARD (MINOR-5, folded into the per-snapshot guard): reject any
        // gate snapshot that is not valid UTF-8. The lossy decode below would
        // silently substitute U+FFFD for invalid bytes, and the jsdom probe
        // (its own lenient TextDecoder) may substitute *differently* — so
        // decode-parity between the two parsers is only PROVEN for valid
        // UTF-8. A non-UTF-8 snapshot must STOP the gate, not be laundered
        // through two independent lossy decoders.
        if let Err(e) = std::str::from_utf8(&bytes) {
            panic!(
                "PARSER-EQUIVALENCE GATE: snapshot [{}] {} is not valid UTF-8 \
                 ({e}). Decode-parity (and hence token-sequence equivalence) \
                 is only proven for valid UTF-8: the lossy rcdom decode and \
                 jsdom's lenient TextDecoder may substitute U+FFFD \
                 differently. STOP — do not certify the substrate on a \
                 non-UTF-8 input.",
                s.file, s.label
            );
        }

        // run.mjs:139 / body_text.mjs: utf-8 decode of the raw bytes, lossy
        // (jsdom's TextDecoder is lenient); decode the same way so the parse
        // inputs are identical. (Validity already asserted just above, so this
        // is a no-substitution decode here — the lossy form is kept only to
        // mirror the probe's decoder exactly.)
        let html = String::from_utf8_lossy(&bytes);
        let parsed = Dom::parse(&html);
        let body = parsed
            .body()
            .unwrap_or_else(|| panic!("html5ever produced no <body> for {}", s.file));

        // 2b. GUARD (MAJOR-2b): scan the parsed rcdom tree for non-whitespace
        // `#text` directly under a table part (`table`/`tbody`/`thead`/
        // `tfoot`/`tr`). Such stray text is the html5ever ≢ jsdom
        // foster-parent class (witness:
        // `known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent`)
        // — the two parsers place it in DIFFERENT positions, so the gate can
        // no longer certify equivalence for that snapshot. Fail loudly: this
        // re-triggers the HLD §6.1 design decision (rcdom → kuchikiki), it is
        // NOT a gate to weaken. (All current snapshots pass this guard.)
        if let Some(witness) = first_stray_table_part_text(&body) {
            panic!(
                "PARSER-EQUIVALENCE GATE: snapshot [{}] {} contains \
                 non-whitespace stray text directly inside a <{}> \
                 (offending text: {:?}). This is the KNOWN html5ever ≢ jsdom \
                 foster-parent divergence class (see \
                 `known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent`): \
                 html5ever foster-parents it before the first cell, jsdom \
                 places it after — the gate can NO LONGER certify equivalence \
                 for this snapshot. Per HLD §6.1 this is the rcdom → kuchikiki \
                 design-decision trigger. STOP — do NOT weaken this guard.",
                s.file, s.label, witness.0, witness.1
            );
        }

        let rcdom_text = dom::text_content(&body);

        // 3. tokenize both with the harness tokenizer semantics.
        let jt = tokens(&jsdom_text);
        let rt = tokens(&rcdom_text);

        // 3a. Close the empty/whitespace laundering path (MAJOR-1): a
        // whitespace-only body yields NON-empty stdout but an EMPTY token
        // vector, and `jt == rt` would then be `[] == []` — a *false*
        // equivalence that certifies the substrate on no text. Assert on the
        // TOKEN vectors (not stdout bytes): degenerate empty-vs-empty is never
        // equivalence evidence (Bug-E2 honest-failure doctrine).
        assert!(
            !jt.is_empty() && !rt.is_empty(),
            "PARSER-EQUIVALENCE GATE: degenerate empty token vector for [{}] {} \
             (jsdom {} tok, rcdom {} tok) — empty-vs-empty is not equivalence \
             evidence; the gate must never certify the substrate on no text \
             (Bug-E2 honest-failure doctrine).",
            s.file,
            s.label,
            jt.len(),
            rt.len()
        );

        // 4. assert token-SEQUENCE identity.
        if jt == rt {
            pass += 1;
            report.push_str(&format!(
                "  PASS  [{}] {}  ({} tokens)\n",
                if s.gold { "gold" } else { "non-gold" },
                s.label,
                jt.len()
            ));
        } else {
            // 5. honest STOP with full evidence (char-diff + token-diff).
            report.push_str(&format!("  FAIL  {}\n", s.label));
            eprintln!("{report}");
            panic!(
                "\n================ PARSER-EQUIVALENCE BLOCKER GATE: FAIL \
                 ================\n\
                 Snapshot : {} ({})\n\
                 Gold     : {}\n\
                 jsdom body.textContent vs html5ever+rcdom text_content \
                 DIVERGED after the harness tokenizer.\n\n\
                 {}\n\n\
                 {}\n\n\
                 Per HLD §6.1 divergence policy this is a STAGE-0 \
                 DESIGN-DECISION TRIGGER (parser/DOM choice changes: \
                 rcdom → kuchikiki, HLD §3), NOT a downstream workaround. \
                 STOP — do not weaken this gate, do not proceed to Stage 1a.\n\
                 ============================================================\
                 ====\n",
                s.file,
                s.label,
                s.gold,
                first_token_diff(&jt, &rt),
                first_char_diff(&jsdom_text, &rcdom_text),
            );
        }
    }

    eprintln!(
        "\n===== PARSER-EQUIVALENCE BLOCKER GATE: PASS =====\n{report}\
         {pass}/{total} snapshots: jsdom-29.1.1 body.textContent and \
         html5ever+rcdom text_content are post-tokenizer SEQUENCE-IDENTICAL.\n\
         SCOPE (honest, not over-claimed): equivalence is proven for THIS \
         {total}-snapshot corpus, which the per-snapshot guard proves contains \
         ZERO non-whitespace stray text in table parts. html5ever and jsdom \
         are KNOWN to diverge on that class (witness: \
         `known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent`); \
         the guard self-polices it, so any future corpus addition in that \
         class re-triggers the HLD §6.1 design decision (rcdom → kuchikiki). \
         This is NOT a blanket \"the DOM substrate is faithful\" claim.\n\
         Stage 1a is unblocked (HLD §6.1/§6.2).\n\
         =================================================\n"
    );
    assert_eq!(pass, total, "all gate snapshots must pass (HLD §6.1)");
}

/// Pin the [`tokens`] mirror against hand-derived expected output so any
/// drift from `benchmark/src/metrics.rs::tokens` fails loudly (HLD §6.1
/// requires *the harness tokenizer semantics*, so the mirror's fidelity is
/// itself load-bearing). Called at the top of the gate and as its own test.
fn tokenizer_mirror_is_faithful() {
    // split(char::is_whitespace) -> drop empty -> lowercase -> NFC.
    assert_eq!(tokens(""), Vec::<String>::new());
    assert_eq!(tokens("   \t\n  "), Vec::<String>::new());
    assert_eq!(tokens("Hello   World"), vec!["hello", "world"]);
    // NBSP (U+00A0) and ideographic space (U+3000) ARE Unicode White_Space ->
    // split points (metrics.rs relies on `char::is_whitespace` == \p{White_Space}).
    assert_eq!(tokens("a\u{00A0}b\u{3000}c"), vec!["a", "b", "c"]);
    // lowercase THEN NFC: É (U+00C9) lowercases to é then NFC-composes to the
    // single code point U+00E9.
    assert_eq!(tokens("\u{00C9}"), vec!["\u{00E9}"]);
    // Decomposed e + combining acute -> NFC single U+00E9 (post-lowercase NFC).
    assert_eq!(tokens("E\u{0301}"), vec!["\u{00E9}"]);
    // Mixed case + punctuation stays attached (no punctuation splitting — the
    // tokenizer only splits on White_Space).
    assert_eq!(tokens("Foo, Bar."), vec!["foo,", "bar."]);
}

/// Standalone form of the self-check (also runs the assertions in isolation
/// for a focused failure locus).
#[test]
fn tokenizer_mirror_matches_metrics_rs() {
    tokenizer_mirror_is_faithful();
}

/// **Pin the known html5ever ≢ jsdom foster-parent divergence as an explicit,
/// regression-locked WITNESS (MAJOR-2a).**
///
/// The gate certifies equivalence only for the *current* corpus, which
/// provably contains ZERO non-whitespace stray text directly inside table
/// parts (the per-snapshot guard in [`parser_equivalence_blocker_gate`]
/// enforces that). html5ever and jsdom are **known to diverge** on that class,
/// and over-claiming "the DOM substrate is faithful" without naming it would
/// be exactly the Bug-E2 conflation this gate exists to prevent. So the
/// divergence is made explicit here rather than hidden:
///
/// For `"<table>FOSTER<tr><td>cell</td></tr></table>"`, html5ever (rcdom)
/// **foster-parents** the stray non-whitespace `FOSTER` text *before* the
/// cell, so `body.text_content()` is `"FOSTERcell"`. **jsdom (the oracle)
/// places it *after* the cell**, yielding `"cellFOSTER"`. The two parsers
/// genuinely disagree on this input class; the corpus simply never hits it,
/// and the gate guard re-triggers HLD §6.1 (rcdom → kuchikiki) if a future
/// snapshot ever does.
///
/// The expected `"FOSTERcell"` was **verified by running rcdom**, not
/// invented; if html5ever's behaviour ever changes, this fails loudly (the
/// honest-failure outcome) rather than silently masking the divergence.
#[test]
fn known_html5ever_vs_jsdom_foster_divergence_is_documented_not_silent() {
    let html = "<table>FOSTER<tr><td>cell</td></tr></table>";
    let parsed = Dom::parse(html);
    let body = parsed.body().expect("html5ever always synthesises <body>");
    let rcdom_text = dom::text_content(&body);
    // html5ever foster-parents non-whitespace stray table text BEFORE the
    // cell (verified by running rcdom). jsdom (the oracle) yields the OTHER
    // order, "cellFOSTER" — i.e. the stray text AFTER the cell. This asymmetry
    // is the named, regression-pinned html5ever ≢ jsdom divergence; the gate
    // guard rejects any corpus snapshot that would enter this class.
    assert_eq!(
        rcdom_text, "FOSTERcell",
        "html5ever foster-parent behaviour changed: expected \"FOSTERcell\" \
         (stray text BEFORE the cell); jsdom yields \"cellFOSTER\" (stray text \
         AFTER the cell). This is the named HLD §6.1 divergence class — STOP \
         and re-evaluate the rcdom→kuchikiki trigger, do NOT edit the expected \
         value to make this pass."
    );
}

/// Pinned FNV-1a-64 of the normalised `benchmark/src/metrics.rs::tokens`
/// function body (see [`metrics_tokens_mirror_coupling_tripwire`]). The gate's
/// in-file [`tokens`] mirror is only valid while it is byte-identical to the
/// harness tokenizer; this constant fingerprints that harness function so any
/// drift in it fails the build loudly (MAJOR-3).
///
/// **Computed from the current `metrics.rs::tokens` (verified by running the
/// exact extraction+normalisation+hash this test uses).** On a deliberate
/// `metrics.rs::tokens` change: re-verify the in-file mirror is still
/// byte-identical to it, THEN update this constant — never the reverse.
const METRICS_TOKENS_FN_FNV1A64: u64 = 0x162b_75a3_d886_41b3;

/// FNV-1a 64-bit. Self-contained + deterministic **by construction** (no
/// external crate — `mdrcel`'s manifest is frozen at Stage 0 and may not gain
/// `sha2`; a `std::hash` SipHash is NOT stable across toolchains so is
/// unusable for a pinned constant). FNV-1a is a fixed, public algorithm whose
/// output cannot drift across toolchains/platforms, which is all a drift
/// tripwire needs (this is change-detection, not a security boundary).
fn fnv1a64(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// **Tokenizer-mirror coupling tripwire (MAJOR-3).**
///
/// The HLD §6.1 gate is only valid while the in-file [`tokens`] mirror is
/// byte-identical to the harness tokenizer `benchmark/src/metrics.rs::tokens`.
/// [`tokenizer_mirror_is_faithful`] pins the mirror's *observable behaviour*;
/// this test additionally pins the *source it mirrors*, so a future edit to
/// `metrics.rs::tokens` cannot silently desynchronise the two.
///
/// At test time it READS `benchmark/src/metrics.rs`, extracts the body of the
/// `tokens` function (brace-matched from its signature to the matching
/// closing brace), **normalises** it (trim each line, drop empty lines, join
/// with `\n`) so formatting-only churn — a rustfmt reflow / indentation /
/// blank-line change — does NOT trip the wire while any change to the actual
/// fn-body tokens DOES, then asserts a stable [`fnv1a64`] content hash equals
/// [`METRICS_TOKENS_FN_FNV1A64`]. (Hash choice: FNV-1a over the normalised
/// text — the simplest *robust* zero-dependency option, since `mdrcel`'s
/// manifest is frozen and `std::hash` is not toolchain-stable.)
///
/// Multi-agent-safe: it only READS the concurrently-edited `benchmark/src`
/// and only WRITES this Stage-0 test file's expectation.
#[test]
fn metrics_tokens_mirror_coupling_tripwire() {
    let path = workspace_root()
        .join("benchmark")
        .join("src")
        .join("metrics.rs");
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "cannot read {} for the mirror tripwire: {e}",
            path.display()
        )
    });

    // Extract `pub fn tokens(...) { .. }` brace-matched: signature line
    // through the matching closing brace (inclusive).
    let sig = "pub fn tokens(text: &str) -> Vec<String> {";
    let start = src.find(sig).unwrap_or_else(|| {
        panic!(
            "metrics.rs::tokens signature not found (expected `{sig}`) — the \
             harness tokenizer was renamed/resignatured; re-verify the in-file \
             mirror against it, then update this tripwire."
        )
    });
    let bytes = src.as_bytes();
    let mut depth = 1usize; // the '{' at the end of `sig` is already open
    let mut i = start + sig.len();
    while i < bytes.len() && depth > 0 {
        match bytes[i] {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    assert_eq!(
        depth, 0,
        "unbalanced braces extracting metrics.rs::tokens — refusing to certify \
         a partial mirror"
    );
    let fn_src = &src[start..i];

    // Normalise: trim each line, drop empties, join with '\n'. Robust to
    // formatting-only churn; sensitive to any fn-body token change.
    let normalised = fn_src
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    let got = fnv1a64(normalised.as_bytes());
    assert_eq!(
        got, METRICS_TOKENS_FN_FNV1A64,
        "\nbenchmark/src/metrics.rs::tokens CHANGED (normalised FNV-1a-64 \
         0x{got:016x} != pinned 0x{:016x}).\n\
         The HLD §6.1 gate is only valid while the in-file `tokens` mirror is \
         byte-identical to the harness tokenizer. ACTION: re-verify the mirror \
         in this file still reproduces `metrics.rs::tokens` exactly \
         (split(char::is_whitespace) -> drop-empty -> to_lowercase -> NFC), \
         and ONLY THEN update METRICS_TOKENS_FN_FNV1A64 to the new hash. Do \
         NOT update the constant without re-verifying the mirror.\n\
         Extracted (normalised) fn:\n{normalised}\n",
        METRICS_TOKENS_FN_FNV1A64
    );
}
