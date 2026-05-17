//! Pure metrics: tokenization, token-set Jaccard, normalised edit distance.
//!
//! This module is the harness's own **testing oracle** (harness HLD §8): a
//! handful of pure, side-effect-free functions whose algebraic laws are
//! property-tested. Correctness here is load-bearing for every later stage,
//! so the conventions below are first-class and directly tested — not
//! incidental behaviour.
//!
//! # The single tokenizer (HLD §8)
//!
//! `tokens` is applied *identically* to oracle `text`, crate `text`, and gold
//! text. It is the comparability guarantee — there is exactly one tokenizer
//! and exactly one word count in the harness.
//!
//! Pipeline, per HLD §8: split on Unicode `White_Space`, drop empties,
//! lowercase, NFC-normalise.
//!
//! ## Whitespace splitting
//!
//! Rust std `char::is_whitespace()` is defined as the Unicode `White_Space`
//! property, so it is exactly the `\p{White_Space}` the HLD specifies; using
//! it avoids a `regex` runtime dependency for no loss of fidelity. This
//! covers ASCII space/tab/newline, NBSP (U+00A0), the ideographic space
//! (U+3000), the Unicode space separators, etc.
//!
//! ## Normalise / lowercase ORDER (documented, applied consistently)
//!
//! Order is **lowercase → NFC**. `str::to_lowercase` performs full Unicode
//! (locale-independent) case mapping, which can emit sequences that are not
//! in NFC (case mappings are not guaranteed to be normalisation-stable, e.g.
//! some characters lower-case to a base letter plus a combining mark).
//! Applying NFC *after* lowercasing therefore guarantees every emitted token
//! is canonically composed regardless of what case mapping produced. Doing
//! NFC first would not give that guarantee, so the order is not symmetric and
//! is fixed here.

// This module is built ahead of its consumer: per harness HLD §12 the pure
// metrics core (build step 2) lands before `score.rs` (step 6), which is the
// only non-test caller of these functions. Until that stage lands they are
// exercised solely by this module's own test/oracle suite, so each carries a
// scoped `#[allow(dead_code)]` and is NOT dead code in the finished harness.
//
// Deliberately NOT a module-wide `#![allow(dead_code)]`: that would also mask
// genuinely dead code anywhere else in the module. Instead each item with no
// in-crate caller until Stage 6 carries its OWN `#[allow(dead_code)]` plus a
// `TODO(stage-6)` tripwire, so the allow is removed item-by-item as `score.rs`
// starts consuming each function (a module-wide attribute would silently keep
// covering later additions long after it stopped being needed).
//
// HONEST CAVEAT (verified 2026-05-17): for *this* crate the `dead_code` lint
// is currently a no-op regardless of which form the attribute takes. The crate
// is a binary whose only build target under `cargo clippy --all-targets` is
// its own unit-test target; in a `--test` build of a *binary*, rustc's
// dead-code seeding is permissive and does NOT flag items merely unreachable
// from `#[test]` fns (an unguarded unused private fn added here was NOT caught
// by `clippy --workspace --all-targets -D warnings`, plain `cargo check`, or
// `cargo build --bin`). So the per-item attributes are correct *hygiene* and
// become genuinely enforced the moment a non-test caller exists (Stage 6
// `score.rs`, or any non-test bin path) — but do not rely on CI to catch new
// dead code in this module before then; that gap closes with Stage 6.

use std::collections::HashSet;

use unicode_normalization::UnicodeNormalization;

/// Split `text` into normalised tokens (HLD §8 — the single tokenizer).
///
/// Splits on Unicode `White_Space` (via `char::is_whitespace`), drops empty
/// fragments, then for each fragment applies **lowercase then NFC** (see the
/// module docs for why that order). The result is the canonical token
/// sequence used by every comparison in the harness; `word_count`, `jaccard`,
/// `precision` and `edit_similarity` are all defined in terms of it.
///
/// Per-fragment NFC (rather than NFC over the whole string) is safe *only*
/// because Unicode `White_Space` characters are never combining marks and
/// never participate in canonical composition: nothing on either side of a
/// split boundary could have combined with the whitespace, so splitting first
/// cannot change the NFC result versus normalising the whole string. Do not
/// reorder split vs normalise — the equivalence depends on this property.
///
/// # Known limitation (v1, per §8 spec)
///
/// This is **lowercase + NFC**, NOT Unicode caseless matching
/// (`NFKC_Casefold`). Compatibility variants therefore tokenize as *distinct*
/// tokens: U+FB01 `ﬁ` ≠ `fi`, U+00B5 µ (MICRO SIGN) ≠ Greek mu U+03BC, the
/// `ﬀ`/`ﬃ` ligatures ≠ their ASCII expansions, etc. These characters do occur
/// in English / Western-European technical and regulatory pages, so the v1
/// scope only *partially* mitigates the casing/normalisation problem and
/// Coverage/Precision can be artefactually depressed where they appear. The
/// proper resolution (switch to `NFKC_Casefold`) is a **tracked cross-team
/// spec change, not a Stage-2 patch** — it is deliberately out of scope here
/// and pinned by `known_limitation_ligature_not_casefolded` so any future
/// casefold change is an intentional, reviewed test edit.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
pub fn tokens(text: &str) -> Vec<String> {
    text.split(char::is_whitespace)
        .filter(|fragment| !fragment.is_empty())
        // Order: lowercase first, then NFC — see module docs.
        .map(|fragment| fragment.to_lowercase().nfc().collect::<String>())
        .collect()
}

/// Word count = number of tokens (HLD §8).
///
/// The harness **never** trusts an external word count; it is always
/// recomputed from [`tokens`] so there is exactly one word-count definition.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
pub fn word_count(text: &str) -> usize {
    tokens(text).len()
}

/// Token-**set** Jaccard similarity: `|A ∩ B| / |A ∪ B|` (HLD §8 — primary
/// metric).
///
/// `A` and `B` are the *sets* of tokens of `a` and `b` (duplicates collapse),
/// so the metric is robust to reordering and repetition but harsh on
/// omission / boilerplate inclusion — the failure modes the harness cares
/// about.
///
/// # Empty-set conventions (first-class, directly tested)
///
/// The set Jaccard ratio is `0/0` when both inputs are empty, so the boundary
/// is *defined here*, not left to float NaN:
///
/// * `J(∅, ∅) = 1.0` — correct set algebra (two empty sets are identical).
/// * `J(x, ∅) = 0.0` for non-empty `x` — an empty extraction shares none of a
///   non-empty extraction's vocabulary.
///
/// The result is always in `[0.0, 1.0]`.
///
/// # HAZARD — `J(∅, ∅) = 1.0` is meaning-ambiguous
///
/// The value is *mathematically* correct but **semantically blind**: by
/// itself it cannot distinguish
///
/// * "both correctly empty" — e.g. a hub/index page both sides rightly
///   classify as a near-empty body (a genuine 1.0), from
/// * "both empty for the wrong reason" — a crate extraction failure that
///   yielded `""` compared against an empty/failed reference (a *false* 1.0
///   that would launder a broken extraction into a perfect score — exactly
///   the Bug-E2 conflation the contract layer is built to prevent).
///
/// This function therefore **must not be the only thing between a failed
/// extraction and a passing score.** Forward-contract: Stage 6 (`score.rs`)
/// MUST gate Coverage on the crate and reference *STATUS* (per the HLD §5
/// anti-Bug-E2 status taxonomy — `oracle_error` / `oracle_timeout` /
/// `not_implemented` / `crate_error` are NOT `ok`-with-empty-text) **before**
/// trusting any empty-driven `1.0` from this function. An empty-vs-empty
/// `1.0` is only meaningful once both sides are known-`ok`.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
pub fn jaccard(a: &str, b: &str) -> f64 {
    let set_a: HashSet<String> = tokens(a).into_iter().collect();
    let set_b: HashSet<String> = tokens(b).into_iter().collect();

    match (set_a.is_empty(), set_b.is_empty()) {
        // J(∅, ∅) = 1.0 — two empty extractions are identical.
        (true, true) => 1.0,
        // J(x, ∅) = J(∅, x) = 0.0 — no shared vocabulary with an empty side.
        (true, false) | (false, true) => 0.0,
        (false, false) => {
            let intersection = set_a.intersection(&set_b).count();
            // |A ∪ B| = |A| + |B| − |A ∩ B|; both non-empty ⇒ union > 0.
            let union = set_a.len() + set_b.len() - intersection;
            intersection as f64 / union as f64
        }
    }
}

/// Token-**set** precision proxy: `|tokens(extracted) ∩ tokens(reference)| /
/// |tokens(extracted)|` (HLD §8 — "Precision proxy").
///
/// The fraction of the *extracted* vocabulary that also appears in the
/// reference. Low precision means the extraction pulled in tokens the
/// reference does not have — i.e. boilerplate / navigation inclusion. Like
/// [`jaccard`] it operates on token **sets** (duplicates collapse), so it is
/// robust to repetition and asymmetric by construction (the denominator is
/// the extracted side only).
///
/// # Empty convention — DELIBERATELY DIFFERENT from [`jaccard`]'s
///
/// `jaccard` returns `1.0` for `J(∅, ∅)` because that is correct set algebra
/// for a *symmetric* similarity. Precision is **not** symmetric and its
/// boundary is chosen for *meaning*, not algebra:
///
/// * `extracted = ∅` ⇒ **`0.0`** (even if `reference` is also empty). An
///   empty extraction has **not** "perfectly avoided boilerplate" — it
///   produced *nothing*. Returning `1.0` here would re-introduce the exact
///   Bug-E2 vector flagged on [`jaccard`], now on the precision axis: a
///   failed/empty extraction laundered into a perfect precision score. The
///   `0/0` case is therefore pinned to `0.0`, not `1.0`, on purpose.
/// * `reference = ∅` with non-empty `extracted` ⇒ `0.0` — every extracted
///   token is absent from an empty reference (this also falls out of the
///   formula: intersection is 0).
///
/// (Status-gating still applies upstream: as with `jaccard`, Stage 6 must not
/// trust this number until crate/reference STATUS is known-`ok`. The `0.0`
/// floor here just refuses to *flatter* an empty extraction in the meantime.)
///
/// The result is always in `[0.0, 1.0]`, and `precision(x, x) = 1.0` for
/// non-empty `x`.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
pub fn precision(extracted: &str, reference: &str) -> f64 {
    let set_e: HashSet<String> = tokens(extracted).into_iter().collect();

    if set_e.is_empty() {
        // extracted = ∅ ⇒ 0.0 by deliberate convention (NOT 1.0): an empty
        // extraction produced nothing; see the doc comment for why this must
        // differ from jaccard's J(∅, ∅) = 1.0.
        return 0.0;
    }

    let set_r: HashSet<String> = tokens(reference).into_iter().collect();
    // |extracted| > 0 here, so the denominator is non-zero; reference = ∅
    // falls out naturally as intersection = 0 ⇒ 0.0.
    let intersection = set_e.intersection(&set_r).count();
    intersection as f64 / set_e.len() as f64
}

/// Normalised Levenshtein similarity over the token **sequence** (HLD §8 —
/// secondary metric):
/// `1 − lev(seq_a, seq_b) / max(len_a, len_b)`.
///
/// Operates on the ordered token sequences (not sets), so it captures
/// structural / ordering differences that set Jaccard misses. Reported
/// alongside Jaccard; not used for pass/fail.
///
/// # Complexity (and the deliberate absence of an input cap)
///
/// `O(n·m)` time, `O(min(n, m))` space (see [`levenshtein`]). Measured at
/// corpus scale this is ~0.4 s for a 17k-token pair, which is acceptable
/// precisely because `edit_similarity` is the **secondary, non-gating**
/// metric (§8) — it never decides pass/fail, so a slow tail does not block a
/// run. There is **deliberately no input-length cap**: pathological
/// `>50k`-token inputs are a known unbounded-time risk that is *knowingly*
/// not mitigated here, to avoid premature optimisation against inputs the
/// real corpus may never contain. Revisit only if real corpus timings exceed
/// budget (evidence-driven, not predicted).
///
/// # Empty convention (documented, directly tested)
///
/// * Both sequences empty → `1.0` (`max(0, 0)` would divide by zero; two
///   empty extractions are identical, consistent with [`jaccard`]'s
///   `J(∅, ∅) = 1.0`).
///
/// `lev ≤ max(len_a, len_b)` always, so the result is always in `[0.0, 1.0]`.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
pub fn edit_similarity(a: &str, b: &str) -> f64 {
    let seq_a = tokens(a);
    let seq_b = tokens(b);

    let max_len = seq_a.len().max(seq_b.len());
    if max_len == 0 {
        // Both empty — identical. Avoids the 0/0 the formula would give.
        return 1.0;
    }

    let distance = levenshtein(&seq_a, &seq_b);
    1.0 - (distance as f64 / max_len as f64)
}

/// Levenshtein edit distance between two token sequences.
///
/// Standard two-row dynamic-programming implementation, `O(n·m)` time and
/// `O(min(n, m))` space via the rolling rows (no input-length cap — see
/// [`edit_similarity`] for why that is a deliberate, evidence-driven
/// decision). Implemented in-tree deliberately — it is small and standard,
/// and the HLD forbids a new runtime dependency for it.
///
/// The substitution cost is **unit (1)**, and that is load-bearing: the
/// identity property `lev == 0 ⟺ sequences equal` (property-tested) holds
/// only with a non-zero unit substitution cost. Weighted / token-similarity
/// substitution costs would break that equivalence and are intentionally not
/// used.
// TODO(stage-6): remove once score.rs consumes this.
#[allow(dead_code)]
fn levenshtein(a: &[String], b: &[String]) -> usize {
    // Operate on the shorter sequence as the row width to bound memory.
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };

    // Row j holds edit distances from `long[..i]` to `short[..j]`.
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr: Vec<usize> = vec![0; short.len() + 1];

    for (i, long_tok) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, short_tok) in short.iter().enumerate() {
            let substitution_cost = usize::from(long_tok != short_tok);
            curr[j + 1] = (prev[j + 1] + 1) // deletion
                .min(curr[j] + 1) // insertion
                .min(prev[j] + substitution_cost); // substitution / match
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[short.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Epsilon for comparing non-exact f64 ratios. Jaccard/edit values are
    /// small rational numbers (ratios of token counts well under millions),
    /// so the accumulated error is far below 1e-9; this bound is generous.
    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    // ---- Tokenisation: whitespace variety -----------------------------------

    #[test]
    fn splits_on_ascii_whitespace_variety() {
        // space, tab, newline, carriage return, form feed, vertical tab.
        let toks = tokens("a b\tc\nd\re\u{000C}f\u{000B}g");
        assert_eq!(toks, vec!["a", "b", "c", "d", "e", "f", "g"]);
    }

    #[test]
    fn splits_on_unicode_whitespace_nbsp_and_ideographic() {
        // U+00A0 NBSP, U+3000 IDEOGRAPHIC SPACE, U+2003 EM SPACE,
        // U+2028 LINE SEPARATOR — all have the Unicode White_Space property.
        let toks = tokens("alpha\u{00A0}beta\u{3000}gamma\u{2003}delta\u{2028}epsilon");
        assert_eq!(toks, vec!["alpha", "beta", "gamma", "delta", "epsilon"]);
    }

    #[test]
    fn drops_empty_fragments_from_runs_and_edges() {
        // Leading, trailing and repeated whitespace must not yield "" tokens.
        let toks = tokens("  \t leading   and\n\n trailing \t ");
        assert_eq!(toks, vec!["leading", "and", "trailing"]);
    }

    #[test]
    fn empty_and_whitespace_only_yield_no_tokens() {
        assert!(tokens("").is_empty());
        assert!(tokens("   \t\n\u{00A0}\u{3000}  ").is_empty());
    }

    // ---- Tokenisation: casefold --------------------------------------------

    #[test]
    fn casefolds_to_lowercase() {
        assert_eq!(
            tokens("Hello WORLD MixedCase"),
            vec!["hello", "world", "mixedcase"]
        );
    }

    #[test]
    fn casefold_is_unicode_aware() {
        // Greek capital sigma lowercases to lowercase sigma.
        assert_eq!(tokens("\u{03A3}"), vec!["\u{03C3}"]);
        // German sharp-s: uppercase ẞ (U+1E9E) lowercases to ß (U+00DF).
        assert_eq!(tokens("\u{1E9E}"), vec!["\u{00DF}"]);
    }

    // ---- Tokenisation: NFC normalisation ------------------------------------

    #[test]
    fn nfc_makes_composed_and_decomposed_compare_equal() {
        // "café" composed: e-acute as single U+00E9.
        let composed = "caf\u{00E9}";
        // "café" decomposed: 'e' + U+0301 COMBINING ACUTE ACCENT.
        let decomposed = "cafe\u{0301}";
        assert_ne!(composed, decomposed, "inputs must differ at the byte level");
        // After tokens() both must normalise to the identical token.
        assert_eq!(tokens(composed), tokens(decomposed));
        assert_eq!(tokens(composed), vec!["caf\u{00E9}"]);
    }

    #[test]
    fn nfc_and_casefold_compose_for_accented_uppercase() {
        // "ÉCOLE" with composed É vs "e\u{0301}cole" decomposed lowercase
        // must collapse to the same token (lowercase then NFC).
        let a = "\u{00C9}COLE"; // É C O L E
        let b = "e\u{0301}cole"; // e + combining acute, c o l e
        assert_eq!(tokens(a), tokens(b));
        assert_eq!(tokens(a), vec!["\u{00E9}cole"]);
    }

    #[test]
    fn known_limitation_ligature_not_casefolded() {
        // PINNING TEST for the documented v1 limitation (see `tokens` docs):
        // tokens() is lowercase+NFC, NOT NFKC_Casefold, so compatibility
        // variants do NOT fold to their ASCII/canonical forms. This asserts
        // the *current, deliberate* behaviour so that a future switch to
        // NFKC_Casefold is an intentional, reviewed change to THIS test —
        // not a silent regression. DO NOT "fix" this by adding casefold; the
        // resolution is a tracked cross-team spec change, not a Stage-2 patch.
        //
        // U+FB01 LATIN SMALL LIGATURE FI ≠ "fi": NFC keeps the ligature, only
        // NFKC_Casefold would decompose it.
        assert_ne!(tokens("\u{FB01}le"), tokens("file"));
        assert_eq!(tokens("\u{FB01}le"), vec!["\u{FB01}le"]);
        // U+00B5 MICRO SIGN ≠ Greek small letter mu U+03BC under NFC; only
        // NFKC(_Casefold) maps the micro sign to the Greek mu.
        assert_ne!(tokens("\u{00B5}m"), tokens("\u{03BC}m"));
        // ﬀ (U+FB00 LATIN SMALL LIGATURE FF) ≠ "ff" — same family.
        assert_ne!(tokens("\u{FB00}"), tokens("ff"));
    }

    // ---- word_count ---------------------------------------------------------

    #[test]
    fn word_count_is_token_len() {
        assert_eq!(word_count(""), 0);
        assert_eq!(word_count("   "), 0);
        assert_eq!(word_count("one"), 1);
        assert_eq!(word_count("one two\tthree\nfour"), 4);
        // Set-collapse does NOT apply to word_count — repeats are counted.
        assert_eq!(word_count("dup dup dup"), 3);
    }

    // ---- Jaccard: explicit empty conventions (first-class) ------------------

    #[test]
    fn jaccard_empty_empty_is_one() {
        // J(∅, ∅) = 1.0 exactly — two empty extractions are identical.
        assert_eq!(jaccard("", ""), 1.0);
        assert_eq!(jaccard("   \t", "\n\u{00A0}"), 1.0);
    }

    #[test]
    fn jaccard_nonempty_with_empty_is_zero() {
        // J(x, ∅) = J(∅, x) = 0.0 exactly for non-empty x.
        assert_eq!(jaccard("hello world", ""), 0.0);
        assert_eq!(jaccard("", "hello world"), 0.0);
    }

    // ---- Jaccard: known values ---------------------------------------------

    #[test]
    fn jaccard_identical_nonempty_is_one() {
        assert_eq!(jaccard("the quick brown fox", "the quick brown fox"), 1.0);
    }

    #[test]
    fn jaccard_disjoint_is_zero() {
        assert_eq!(jaccard("alpha beta", "gamma delta"), 0.0);
    }

    #[test]
    fn jaccard_partial_overlap_known_ratio() {
        // A = {a, b, c}, B = {b, c, d}; ∩ = {b, c} = 2, ∪ = 4 ⇒ 0.5.
        assert!(approx(jaccard("a b c", "b c d"), 0.5));
    }

    #[test]
    fn jaccard_is_set_based_duplicates_collapse() {
        // {a,b} vs {a,a,b,b}: identical sets ⇒ 1.0 despite different lengths.
        assert_eq!(jaccard("a b", "a a b b"), 1.0);
        // {a} vs {a,b}: ∩=1, ∪=2 ⇒ 0.5 regardless of repetition.
        assert!(approx(jaccard("a a a", "a b"), 0.5));
    }

    #[test]
    fn jaccard_uses_the_shared_tokenizer() {
        // Composed vs decomposed é → same token ⇒ perfect Jaccard.
        assert_eq!(jaccard("caf\u{00E9}", "cafe\u{0301}"), 1.0);
        // Case-insensitive via the shared tokenizer.
        assert_eq!(jaccard("Hello World", "hello world"), 1.0);
    }

    // ---- precision: DELIBERATELY asymmetric ∅-convention -------------------

    #[test]
    fn precision_empty_extracted_is_zero_not_one() {
        // The headline difference from jaccard: extracted = ∅ ⇒ 0.0, even
        // when reference is ALSO empty. An empty extraction produced nothing;
        // returning 1.0 would re-introduce the Bug-E2 vector on the precision
        // axis (see precision/jaccard doc comments). This MUST NOT be 1.0.
        assert_eq!(precision("", ""), 0.0);
        assert_eq!(precision("   \t\n", "\u{00A0}"), 0.0);
        assert_eq!(precision("", "hello world"), 0.0);
    }

    #[test]
    fn precision_empty_reference_with_nonempty_extracted_is_zero() {
        // reference = ∅, extracted non-empty ⇒ 0.0 (no extracted token can
        // be present in an empty reference; falls out of the formula too).
        assert_eq!(precision("hello world", ""), 0.0);
        assert_eq!(precision("hello world", "  \t "), 0.0);
    }

    #[test]
    fn precision_identical_nonempty_is_one() {
        assert_eq!(precision("the quick brown fox", "the quick brown fox"), 1.0);
    }

    #[test]
    fn precision_disjoint_is_zero() {
        // No extracted token appears in the reference.
        assert_eq!(precision("alpha beta", "gamma delta"), 0.0);
    }

    #[test]
    fn precision_known_ratios_are_asymmetric() {
        // extracted = {a,b,c,d}, reference = {a,b}: 2 of 4 extracted tokens
        // are in the reference ⇒ 0.5. Denominator is the EXTRACTED side.
        assert!(approx(precision("a b c d", "a b"), 0.5));
        // Swap: extracted = {a,b}, reference = {a,b,c,d}: both extracted
        // tokens are in the reference ⇒ 1.0. Asymmetry is the point — a
        // small precise extraction scores 1.0 even if it missed coverage.
        assert!(approx(precision("a b", "a b c d"), 1.0));
        // extracted = {a,b,c}, reference = {b,c,d}: 2 of 3 ⇒ ~0.666….
        assert!(approx(precision("a b c", "b c d"), 2.0 / 3.0));
    }

    #[test]
    fn precision_is_set_based_duplicates_collapse() {
        // extracted {a,a,a,b} → set {a,b}; reference {a}: 1 of 2 ⇒ 0.5
        // (repetition in the extraction does not change the set ratio).
        assert!(approx(precision("a a a b", "a"), 0.5));
    }

    #[test]
    fn precision_uses_the_shared_tokenizer() {
        // Composed vs decomposed é collapse via the shared tokenizer ⇒ 1.0.
        assert_eq!(precision("caf\u{00E9}", "cafe\u{0301}"), 1.0);
        // Case-insensitive via the shared tokenizer.
        assert_eq!(precision("Hello World", "hello world"), 1.0);
    }

    // ---- edit_similarity: explicit empty convention ------------------------

    #[test]
    fn edit_similarity_empty_empty_is_one() {
        // Documented: both-empty → 1.0 exactly (identical, no 0/0).
        assert_eq!(edit_similarity("", ""), 1.0);
        assert_eq!(edit_similarity("  \t", "\n "), 1.0);
    }

    #[test]
    fn edit_similarity_nonempty_with_empty_is_zero() {
        // max_len = len(x), lev = len(x) ⇒ 1 - 1 = 0.
        assert_eq!(edit_similarity("a b c", ""), 0.0);
        assert_eq!(edit_similarity("", "a b c"), 0.0);
    }

    // ---- edit_similarity: known values -------------------------------------

    #[test]
    fn edit_similarity_identical_is_one() {
        assert_eq!(
            edit_similarity("the quick brown fox", "the quick brown fox"),
            1.0
        );
    }

    #[test]
    fn edit_similarity_single_substitution_known_ratio() {
        // ["a","b","c","d"] vs ["a","x","c","d"]: lev = 1, max_len = 4
        // ⇒ 1 - 1/4 = 0.75.
        assert!(approx(edit_similarity("a b c d", "a x c d"), 0.75));
    }

    #[test]
    fn edit_similarity_is_sequence_not_set_sensitive() {
        // Same token *set*, different order: Jaccard = 1 but edit_sim < 1.
        assert_eq!(jaccard("a b c d", "d c b a"), 1.0);
        assert!(edit_similarity("a b c d", "d c b a") < 1.0);
    }

    #[test]
    fn edit_similarity_insertion_known_ratio() {
        // ["a","b","c"] vs ["a","b","c","d"]: lev = 1, max_len = 4
        // ⇒ 1 - 1/4 = 0.75.
        assert!(approx(edit_similarity("a b c", "a b c d"), 0.75));
    }

    #[test]
    fn levenshtein_matches_hand_computed_classic_example() {
        // "kitten" → "sitten" → "sittin" → "sitting": distance 3 (on tokens).
        let a: Vec<String> = "k i t t e n".split(' ').map(String::from).collect();
        let b: Vec<String> = "s i t t i n g".split(' ').map(String::from).collect();
        assert_eq!(levenshtein(&a, &b), 3);
        // Symmetric.
        assert_eq!(levenshtein(&b, &a), 3);
    }

    // ---- Property tests (HLD §8 — the harness's own oracle laws) ------------
    //
    // proptest is a dev-dependency. Strategy: short token-ish strings built
    // from a *deliberately tiny* alphabet plus whitespace, so collisions /
    // overlaps / substitutions actually occur (random unicode would almost
    // never share tokens, making the overlap properties vacuous — a wider
    // alphabet was previously ~87% disjoint, exercising mostly the
    // empty-intersection branch).

    use proptest::prelude::*;

    /// Words drawn from a *3-char* alphabet, length 1..=3, so the whole word
    /// space is ≤ 3 + 9 + 27 = 39 distinct tokens. This forces the
    /// intersection / substitution / partial-overlap branches of
    /// jaccard/precision/levenshtein to be hit routinely instead of almost
    /// always falling into the disjoint case.
    fn word() -> impl Strategy<Value = String> {
        prop::collection::vec(prop::sample::select(vec!['a', 'b', 'c']), 1..=3)
            .prop_map(|cs| cs.into_iter().collect())
    }

    /// A whitespace-separated phrase of 0..8 small words.
    fn phrase() -> impl Strategy<Value = String> {
        prop::collection::vec(word(), 0..8).prop_map(|ws| ws.join(" "))
    }

    /// A guaranteed non-empty phrase (1..8 words).
    fn nonempty_phrase() -> impl Strategy<Value = String> {
        prop::collection::vec(word(), 1..8).prop_map(|ws| ws.join(" "))
    }

    /// A phrase strategy that *includes* the empty / whitespace-only cases
    /// (≈⅓ of draws), so empty-inclusive laws are exercised uniformly rather
    /// than only via hand-written unit cases.
    fn maybe_empty_phrase() -> impl Strategy<Value = String> {
        prop_oneof![
            // Whitespace-only / empty fragments → tokenize to ∅.
            prop::sample::select(vec!["", " ", "  \t\n", "\u{00A0}\u{3000}"])
                .prop_map(String::from),
            phrase(),
        ]
    }

    /// Unicode-bearing word fragments: the small alphabet mixed with
    /// normalisation / casing / compatibility hazards. Exercises the
    /// lowercase→NFC pipeline (and its idempotence) on inputs that actually
    /// differ pre-normalisation:
    /// * `é` (U+00E9, composed) vs `e`+U+0301 (decomposed) — NFC must fold,
    /// * `É` (U+00C9) — lowercase then NFC,
    /// * U+00B5 µ MICRO SIGN, U+FB01 ﬁ LATIN SMALL LIGATURE FI — the
    ///   documented v1 non-casefold limitation (must remain distinct),
    /// * NBSP (U+00A0) — a White_Space split point inside a "word".
    fn unicode_word() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop::sample::select(vec![
                "a",
                "b",
                "\u{00E9}",  // é composed
                "e\u{0301}", // e + combining acute (decomposed é)
                "\u{00C9}",  // É
                "\u{00B5}",  // µ MICRO SIGN
                "\u{FB01}",  // ﬁ ligature
                "\u{00A0}",  // NBSP (whitespace inside a fragment)
            ]),
            1..=4,
        )
        .prop_map(|parts| parts.concat())
    }

    /// 0..6 unicode-bearing fragments joined by ASCII spaces.
    fn unicode_phrase() -> impl Strategy<Value = String> {
        prop::collection::vec(unicode_word(), 0..6).prop_map(|ws| ws.join(" "))
    }

    proptest! {
        // jaccard(x, x) == 1 for non-empty x.
        #[test]
        fn prop_jaccard_self_is_one(x in nonempty_phrase()) {
            prop_assert_eq!(jaccard(&x, &x), 1.0);
        }

        // jaccard(x, "") == 0 for non-empty x (explicit convention).
        #[test]
        fn prop_jaccard_nonempty_with_empty_is_zero(x in nonempty_phrase()) {
            prop_assert_eq!(jaccard(&x, ""), 0.0);
            prop_assert_eq!(jaccard("", &x), 0.0);
        }

        // Symmetry: jaccard(a, b) == jaccard(b, a).
        #[test]
        fn prop_jaccard_symmetric(a in phrase(), b in phrase()) {
            prop_assert_eq!(jaccard(&a, &b), jaccard(&b, &a));
        }

        // Range: 0.0 <= jaccard <= 1.0.
        #[test]
        fn prop_jaccard_in_range(a in phrase(), b in phrase()) {
            let j = jaccard(&a, &b);
            prop_assert!((0.0..=1.0).contains(&j), "jaccard out of range: {}", j);
        }

        // Empty-inclusive Jaccard law (HAZARD boundary, exercised uniformly
        // over a strategy that DOES include empties): J(∅,∅)==1.0 and
        // J(x,x)==1.0 must both hold without special-casing in the test.
        #[test]
        fn prop_jaccard_self_is_one_including_empty(x in maybe_empty_phrase()) {
            // Covers the J(∅,∅)=1.0 hazard case and the non-empty self case
            // with one uniform assertion (whenever a draw tokenizes to ∅
            // this is exactly the J(∅,∅) branch).
            prop_assert_eq!(jaccard(&x, &x), 1.0);
        }

        // precision(x, x) == 1.0 for non-empty x.
        #[test]
        fn prop_precision_self_is_one(x in nonempty_phrase()) {
            prop_assert_eq!(precision(&x, &x), 1.0);
        }

        // precision(∅, _) == 0.0 — empty extraction never flatters itself
        // (the deliberate divergence from jaccard's J(∅,∅)=1.0).
        #[test]
        fn prop_precision_empty_extracted_is_zero(r in maybe_empty_phrase()) {
            prop_assert_eq!(precision("", &r), 0.0);
            prop_assert_eq!(precision("  \t\n", &r), 0.0);
        }

        // Range: 0.0 <= precision <= 1.0 (incl. empty-bearing inputs).
        #[test]
        fn prop_precision_in_range(a in maybe_empty_phrase(), b in maybe_empty_phrase()) {
            let p = precision(&a, &b);
            prop_assert!((0.0..=1.0).contains(&p), "precision out of range: {}", p);
        }

        // edit_similarity(x, x) == 1 (non-empty; both-empty also 1 but
        // covered by an explicit unit test).
        #[test]
        fn prop_edit_self_is_one(x in nonempty_phrase()) {
            prop_assert_eq!(edit_similarity(&x, &x), 1.0);
        }

        // Symmetry: edit_similarity(a, b) == edit_similarity(b, a).
        #[test]
        fn prop_edit_symmetric(a in phrase(), b in phrase()) {
            prop_assert_eq!(edit_similarity(&a, &b), edit_similarity(&b, &a));
        }

        // Range: 0.0 <= edit_similarity <= 1.0.
        #[test]
        fn prop_edit_in_range(a in phrase(), b in phrase()) {
            let s = edit_similarity(&a, &b);
            prop_assert!((0.0..=1.0).contains(&s), "edit_similarity out of range: {}", s);
        }

        // Triangle-style sanity for the underlying Levenshtein metric:
        // lev(a, c) <= lev(a, b) + lev(b, c). (Levenshtein is a true metric;
        // this guards the DP implementation against asymmetric/illegal costs.)
        #[test]
        fn prop_levenshtein_triangle(a in phrase(), b in phrase(), c in phrase()) {
            let ta = tokens(&a);
            let tb = tokens(&b);
            let tc = tokens(&c);
            let ac = levenshtein(&ta, &tc);
            let ab = levenshtein(&ta, &tb);
            let bc = levenshtein(&tb, &tc);
            prop_assert!(ac <= ab + bc, "triangle violated: {} > {} + {}", ac, ab, bc);
        }

        // Levenshtein identity of indiscernibles: lev(x, x) == 0, and
        // lev == 0 only when the token sequences are equal.
        #[test]
        fn prop_levenshtein_identity(a in phrase(), b in phrase()) {
            let ta = tokens(&a);
            let tb = tokens(&b);
            prop_assert_eq!(levenshtein(&ta, &ta), 0);
            if ta == tb {
                prop_assert_eq!(levenshtein(&ta, &tb), 0);
            } else {
                prop_assert!(levenshtein(&ta, &tb) > 0);
            }
        }

        // word_count == tokens().len() (the single-word-count guarantee).
        #[test]
        fn prop_word_count_is_token_len(a in phrase()) {
            prop_assert_eq!(word_count(&a), tokens(&a).len());
        }

        // IDEMPOTENCE of the tokenizer over Unicode-bearing input:
        // re-tokenizing the space-joined tokens must yield the same tokens.
        // This is the key oracle law that the lowercase→NFC pipeline reaches
        // a fixed point — a second pass changes nothing. Exercised on the
        // unicode strategy (composed/decomposed é, É, µ, ﬁ, NBSP) so the
        // normalisation/casing branches are actually hit, not just ASCII.
        #[test]
        fn prop_tokens_idempotent_unicode(s in unicode_phrase()) {
            let once = tokens(&s);
            let twice = tokens(&once.join(" "));
            prop_assert_eq!(once, twice);
        }

        // The same idempotence law must also hold on the plain ASCII
        // strategy (cheap, dense coverage of the common path).
        #[test]
        fn prop_tokens_idempotent_ascii(s in phrase()) {
            let once = tokens(&s);
            let twice = tokens(&once.join(" "));
            prop_assert_eq!(once, twice);
        }
    }
}
