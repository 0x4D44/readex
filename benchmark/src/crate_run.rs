//! Crate adapter: call `mdrcel::extract` **in-process** and map the outcome
//! onto the harness's first-class status taxonomy.
//!
//! This is the crate-under-test side of the differential harness. Unlike the
//! two oracles (subprocesses, different runtimes — see `oracle.rs`), the crate
//! is a Rust workspace dependency and is called directly in-process (harness
//! HLD §4.2/§5 — "The crate is called **in-process**... This asymmetry is
//! intentional and simpler than uniform subprocessing").
//!
//! # The tri-state (harness HLD §5) — consumer mapping
//!
//! Mirrors the oracle tri-state philosophy, the anti-Bug-E2 doctrine:
//!
//! | `mdrcel::extract` result                       | [`CrateStatus`]                                         |
//! |------------------------------------------------|---------------------------------------------------------|
//! | `Ok(Extracted)` — **even if `text` is `""`**   | [`Ok`](CrateStatus::Ok) (carries it)                    |
//! | `Err(ExtractError::NotImplemented)`            | [`NotImplemented`](CrateStatus::NotImplemented)         |
//! | `Err(ExtractError::ContentTooShort{..})` (NEW) | [`CrateError`](CrateStatus::CrateError) (`"content_too_short: …"`) |
//! | a **panic** inside `extract`                   | [`CrateError`](CrateStatus::CrateError) (`"panic: …"`)  |
//!
//! At Stage 4 (M2, HLD §7.6) the M1 `ExtractError::NotImplemented` is **no
//! longer** the only variant: the long-anticipated
//! [`ExtractError::ContentTooShort`](mdrcel::ExtractError::ContentTooShort)
//! variant has landed and **the compile-fence below fired** — a conscious
//! match arm was added per its doctrine. The match remains **exhaustive,
//! WITHOUT a wildcard**, so the next future variant will fire the fence
//! again. `ContentTooShort` is mapped to `CrateError` with the reason string
//! `content_too_short: <wc> < <threshold>` because (a) it is a CONFIGURED
//! threshold failure — the harness path uses `Options::default()` so
//! `min_word_count == 0` and this variant CANNOT fire on the harness corpus
//! run; the mapping exists for forward-callers that pass non-zero, not the
//! harness; (b) folding it into the existing `CrateError` channel keeps the
//! Bug-E2 status partition stable: error reason is what differentiates,
//! never the status discriminant. (Future-proofing: if a corpus run ever
//! configures `min_word_count > 0`, the per-status BTreeMap surfaces
//! `crate_error` with the reason intact — exactly the anti-Bug-E2 design.)
//!
//! `Ok` with empty `text` is **success**, not an error and not
//! `not_implemented`: "found little" is a valid extraction (the exact
//! distinction Bug E2 collapsed; harness HLD §5 — "distinct from `crate_error`
//! and from `ok` with empty text"). [`NotImplemented`](CrateStatus::NotImplemented)
//! is the **Milestone-1 floor** and a **distinct first-class status** — it is
//! never folded into [`CrateError`](CrateStatus::CrateError) nor laundered into
//! an empty-`Ok`; the baseline report counts it separately so "the algorithm
//! does not exist yet" is visibly different from "the algorithm ran and
//! failed/found nothing".
//!
//! # `ExtractError` compile-fence — **FIRED AND RESOLVED at M2 Stage 4**
//!
//! `ExtractError` is matched **exhaustively, WITHOUT a wildcard**. At M1 it
//! had only `NotImplemented`. At M2 Stage 4 (HLD §7.6) `ContentTooShort`
//! arrived as the *first* deliberate non-floor error variant, the build
//! broke RIGHT HERE on purpose, and the resolution was a conscious arm
//! added below (with this comment as the audit trail). `ExtractError` is
//! still deliberately **not** `#[non_exhaustive]` (that would defeat the
//! fence). When the NEXT variant lands, this match will stop being
//! exhaustive again and the harness build will break RIGHT HERE again —
//! that is the feature, do **not** reintroduce a wildcard to "unbreak" it.
//! Add the new arm deliberately. (The `panic` arm is the *outer*
//! `catch_unwind` `Result`, not an `ExtractError` variant — it is unrelated
//! to this fence.)
//!
//! # Panic isolation (forward-looking robustness)
//!
//! At M1 `mdrcel::extract` only returns `NotImplemented` and cannot panic. But
//! once the extraction algorithm lands (later milestones) a single pathological
//! corpus URL could panic (slicing a non-char-boundary, an `unwrap` on a
//! malformed DOM, recursion depth, …). Because the crate runs **in-process**,
//! an unguarded panic would unwind through the harness and **abort the entire
//! corpus run**, destroying the differential signal for every *other* URL —
//! the opposite of what a differential harness is for. So the actual call is
//! wrapped in [`std::panic::catch_unwind`]: a caught panic becomes
//! [`CrateStatus::CrateError`] (`"panic: …"`) for *that one URL* and the run
//! continues. This is the in-process analogue of the oracle side recording a
//! crashed subprocess as `oracle_error` rather than letting it kill the run.
//!
//! ## `panic = "abort"` caveat (documented limitation)
//!
//! [`catch_unwind`](std::panic::catch_unwind) only works under the **unwinding**
//! panic strategy. Under `panic = "abort"` a panic terminates the process
//! immediately and **cannot** be caught — this isolation would be silently
//! ineffective. This workspace uses Cargo's **default** profiles, which use
//! `panic = "unwind"` for both `dev` and `test` (no `panic = "abort"` override
//! in any `Cargo.toml`), so the isolation is effective as built. This is a
//! deliberately recorded constraint, not a defect: if a future profile sets
//! `panic = "abort"`, this guard becomes a no-op and a panicking URL would
//! again take down the whole run. A unit test exercises the catch path under
//! the test profile to keep the guarantee honest for the configuration we ship.
//!
//! # Testable seam (mirrors `oracle.rs`'s minimal approach)
//!
//! The status-mapping + panic-isolation logic must be unit-testable **now**,
//! even though the real `mdrcel::extract` only ever returns `NotImplemented`
//! at M1 (so the `Ok`, empty-`Ok`, other-`Err`, and panic arms would otherwise
//! be unreachable from tests). Following the established `oracle.rs` pattern —
//! a single function taking the variable part as a parameter, **not** a
//! trait/plugin tower (no premature abstraction, harness HLD §3) — the seam is
//! [`run_extraction`]: it takes an injectable `FnOnce() -> Result<Extracted,
//! ExtractError>` and owns *only* the catch-unwind + tri-state mapping.
//! Production goes through the thin [`run_crate`] wrapper, whose closure simply
//! calls `mdrcel::extract`. Tests inject closures that return each variant (and
//! one that `panic!`s) to exercise every arm without the algorithm existing.

// O4 status (Stage 6, 2026-05-17). `score.rs` (reachable from `main`'s
// no-subcommand path) constructs/inspects `CrateStatus` (all three variants)
// and calls `run_crate`, which reaches `run_extraction` — every pre-Stage-6
// per-item `#[allow(dead_code)]` + `TODO(stage-6)` tripwire in this module
// was REMOVED (no longer dead code by construction: every item here now has a
// real consumer, so none of them depends on the lint to stay non-dead).
//
// O4 is only PARTIALLY discharged here, NOT proven fully enforcing for this
// bin crate. A verification probe under
// `clippy --workspace --all-targets -- -D warnings` establishes only that
// unused `pub` items in the `benchmark` bin crate ARE now caught (a real
// non-test consumer, `score.rs`, exists, so rustc seeds dead-code analysis
// from the binary root through the `pub` surface). It does NOT establish
// enforcement for unused PRIVATE items or never-constructed ENUM VARIANTS in
// this bin crate — the original Stage-2 O4 caveat persists there unchanged
// (notably: `CrateStatus`'s variants are kept non-dead by `score.rs`
// constructing them, NOT by the lint flagging an unconstructed variant). No
// module-wide `#![allow]` was ever added (deliberate), so the `pub`-surface
// half of the enforcement is genuine; the private / enum-variant half remains
// convention + review, not a proven guarantee.

use std::panic::{AssertUnwindSafe, catch_unwind};

use mdrcel::{ExtractError, Extracted};

/// First-class outcome of one in-process `mdrcel::extract` call (harness
/// HLD §5 status taxonomy — the anti-Bug-E2 tri-state, crate side).
///
/// Deliberately a sibling of `oracle::OracleStatus`, not a shared type: the
/// crate has *no* timeout (it is in-process, not a killable subprocess) and
/// adds [`NotImplemented`](Self::NotImplemented) (the M1 floor, which has no
/// oracle analogue). Forcing the two onto one enum would be exactly the kind
/// of premature unification the harness HLD §3 warns against.
///
/// The `Ok` payload is `Box`ed so the large [`Extracted`] does not bloat every
/// `CrateStatus` value (mirrors `OracleStatus::Ok(Box<…>)` and silences
/// clippy's `large_enum_variant`). [`CrateError`](Self::CrateError) carries a
/// human-readable reason so the Stage-7 report can surface *why* (a hard error
/// message, or `"panic: …"`) without re-deriving it.
//
// O4 status (Stage 6): `score.rs` constructs/inspects `CrateStatus`
// (matching all three variants) on the non-test `main` path; the pre-Stage-6
// `#[allow(dead_code)]` + `TODO(stage-6)` was removed because every variant
// now has a real constructor — NOT because the lint would otherwise flag an
// unconstructed variant. A verification probe confirms unused PRIVATE items
// and never-constructed ENUM VARIANTS in this `benchmark` bin crate are
// STILL NOT compiler-caught (the original Stage-2 O4 caveat persists for the
// non-`pub`-surface case); these variants stay non-dead by convention +
// `score.rs` actually constructing them, not by the dead-code lint.
#[derive(Debug)]
pub enum CrateStatus {
    /// `mdrcel::extract` returned `Ok` — **even if `text` is `""`** ("found
    /// little" is a valid result, NOT an error and NOT `not_implemented`).
    /// Carries the parsed [`Extracted`].
    ///
    /// Bug-E2 hazard forwarded to the consumer: Stage 6 (`score.rs`) MUST
    /// gate on this `Ok` discriminant before trusting any empty-text-driven
    /// metric — an `Ok` whose `text` is empty, scored against an empty
    /// reference, yields `jaccard == 1.0` (a perfect score for "extracted
    /// nothing"; see the `metrics.rs` `# HAZARD — J(∅, ∅) = 1.0` block).
    /// That `1.0` is only meaningful when the empty came from a *known* `Ok`
    /// (a deliberate empty extraction), never from `NotImplemented` or
    /// `CrateError` laundered into emptiness.
    Ok(Box<Extracted>),
    /// `mdrcel::extract` returned [`ExtractError::NotImplemented`] — the
    /// Milestone-1 floor. A **distinct** first-class status: never folded into
    /// [`CrateError`](Self::CrateError), never laundered into an empty `Ok`.
    NotImplemented,
    /// `mdrcel::extract` **panicked** — the catch_unwind layer recovered it
    /// (`"panic: …"`). Never silently treated as empty content (the Bug-E2
    /// lesson, crate side). Carries a human-readable reason.
    ///
    /// At M1 this is reached *only* via a caught panic: the `ExtractError`
    /// match is an exhaustive no-wildcard compile-fence (see the module docs),
    /// so a future error variant does **not** silently arrive here — the build
    /// breaks instead, forcing a deliberate decision about whether that
    /// variant should map to `CrateError` or elsewhere.
    CrateError(String),
}

/// Run `mdrcel::extract` for `html` / `base_url` and map the outcome onto
/// [`CrateStatus`] (the production entry point).
///
/// Thin wrapper over the [`run_extraction`] seam: it only supplies the
/// closure that calls `mdrcel::extract`, so the catch-unwind + tri-state
/// mapping is exercised by tests **without** depending on the algorithm
/// existing (at M1 the real call only ever yields `NotImplemented`). An
/// end-to-end test asserts this production path yields
/// [`CrateStatus::NotImplemented`] at M1 (the documented floor).
// O4 (Stage 6, `pub`-surface half — genuinely caught): `score::score_corpus`
// calls `run_crate` for every URL on the non-test `main` path, so this `pub`
// fn has a real consumer and the pre-Stage-6 allow was removed. As a `pub`
// item it is in the half a verification probe shows IS now lint-enforced
// (unused `pub` items in this bin crate are caught once a non-test consumer
// exists); the private / enum-variant half remains uncaught (see the
// module-level O4 status note).
pub fn run_crate(html: &str, base_url: Option<&str>) -> CrateStatus {
    run_extraction(|| mdrcel::extract(html, base_url))
}

/// The testable seam: invoke `f` (an extraction call), isolating any panic,
/// and map the outcome onto [`CrateStatus`] per the tri-state.
///
/// Kept **minimal on purpose** — a single function taking the extraction as a
/// `FnOnce` parameter, *not* a trait/strategy tower (no premature abstraction,
/// harness HLD §3; same shape as `oracle::run_command_with_timeout`).
/// Production callers go through [`run_crate`]; tests inject closures returning
/// each `Result` variant (and one that `panic!`s) so every mapping arm —
/// `Ok`-with-text, `Ok`-with-empty-text, `NotImplemented`, other-`Err`, and
/// panic — is unit-testable at M1.
///
/// # Panic isolation
///
/// `f` is run inside [`catch_unwind`]. A caught panic maps to
/// [`CrateStatus::CrateError`] prefixed `"panic: "` (so the report can tell a
/// crash from a returned error) and the caller's corpus loop continues —
/// **one bad URL must not lose the whole differential signal**. See the
/// module-level `panic = "abort"` caveat: this guard is effective only under
/// the unwinding strategy, which is what this workspace's default profiles
/// use.
///
/// [`AssertUnwindSafe`] is required because an arbitrary `FnOnce` is not
/// `UnwindSafe` (e.g. the production closure captures `&str` by reference).
/// It is **sound here**: `f` is consumed exactly once and nothing it might
/// have mutated is observed after a panic — on the panic path we return a
/// fresh `CrateError` and touch none of `f`'s captures, so there is no
/// broken-invariant hazard the `UnwindSafe` bound exists to prevent.
// O4 (Stage 6, `pub`-surface half — genuinely caught): reached via `run_crate`
// from the non-test `score::score_corpus` → `main` path, so this `pub` fn has
// a real consumer and the pre-Stage-6 allow was removed. `pub`-surface
// enforcement is the half a verification probe shows IS now real; private /
// enum-variant items remain uncaught (see the module-level O4 status note).
pub fn run_extraction<F>(f: F) -> CrateStatus
where
    F: FnOnce() -> Result<Extracted, ExtractError>,
{
    match catch_unwind(AssertUnwindSafe(f)) {
        // The closure returned normally — apply the tri-state mapping.
        Ok(Ok(extracted)) => {
            // Ok regardless of whether `text` is empty: an empty body is a
            // valid extraction, NOT an error and NOT not_implemented
            // (Bug-E2 doctrine; harness HLD §5).
            CrateStatus::Ok(Box::new(extracted))
        }
        Ok(Err(ExtractError::NotImplemented)) => {
            // The M1 floor — a DISTINCT first-class status, never folded into
            // CrateError nor an empty Ok.
            //
            // The INTENTIONAL COMPILE-FENCE (anti-Bug-E2 — see module-level
            // doc) keeps this match exhaustive WITHOUT a wildcard. M2 Stage 4
            // ADDED a conscious arm below for the new `ContentTooShort`
            // variant — that was the documented fence-firing event (HLD
            // §7.6 / brief Stage-4 §C). The fence persists; the *next* new
            // variant will break the build right here again on purpose.
            // Do NOT reintroduce a wildcard.
            CrateStatus::NotImplemented
        }
        Ok(Err(ExtractError::ContentTooShort {
            word_count,
            threshold,
        })) => {
            // M2 Stage 4 (HLD §7.6) — **DELIBERATE FENCE-FIRING ARM**.
            //
            // The new `ContentTooShort` variant arrived; the no-wildcard match
            // above stopped being exhaustive; the harness build broke right
            // here on purpose; this arm is the conscious resolution
            // documented in the brief (Stage-4 §C — "Resolve the fence BY
            // ADDING A CONSCIOUS, REVIEWED ARM").
            //
            // **Tri-state decision: map to CrateError with a precise reason
            // string, NOT to NotImplemented and NOT to a new CrateStatus.**
            //
            // - NOT NotImplemented: the algorithm DID run successfully — a
            //   threshold check failed, not the floor; conflating them would
            //   relitigate exactly the M1-floor distinction.
            // - NOT a new CrateStatus variant: the harness uses
            //   `Options::default()` (min_word_count == 0) on every URL of
            //   the corpus run, so this arm CANNOT fire on the harness path;
            //   creating a first-class status the harness never observes
            //   would be premature abstraction (the reason channel already
            //   surfaces *why* via the per-URL `crate_error_reason` field).
            // - The reason string `"content_too_short: <wc> < <threshold>"`
            //   gives downstream consumers (`results.json`,
            //   `crate_status_detail`) the numbers without re-derivation.
            //
            // **No harness behaviour change at M2 Stage 4**: every default-
            // Options corpus call still maps to `CrateStatus::Ok(_)`;
            // `crate ok:51` is preserved exactly. This arm exists for
            // (a) the fence's documented resolution; (b) consumers that
            // configure `min_word_count > 0` (out-of-harness use), so the
            // mapping is correct for them too.
            CrateStatus::CrateError(format!("content_too_short: {word_count} < {threshold}"))
        }
        // A panic unwound out of `f`. This is the *outer* catch_unwind
        // `Result`, NOT an `ExtractError` variant — it is unrelated to the
        // compile-fence above and is intentionally a catch-all over panic
        // payloads. catch_unwind gives us the panic payload; recover the
        // message for the report (the two standard payload shapes are
        // `&'static str` and `String`; anything else is summarised).
        Err(payload) => CrateStatus::CrateError(format!("panic: {}", panic_message(&payload))),
    }
}

/// Best-effort human-readable text from a [`catch_unwind`] panic payload.
///
/// `panic!` payloads are `Box<dyn Any + Send>`; in practice they are either a
/// `&'static str` (string-literal panic) or a `String` (formatted panic /
/// `unwrap`/`expect`). Anything else (a non-string panic value) cannot be
/// stringified generically, so it is summarised as `<non-string panic
/// payload>` rather than dropped — the report still records *that* it panicked.
fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a fully-populated [`Extracted`] with a given body text, so the
    /// `Ok` arms can assert the payload is carried through verbatim.
    ///
    /// M2 Stage 4: `..Extracted::default()` fills the new metadata fields
    /// added to the public API (HLD §7.6) — same shape as the score.rs test
    /// helper; only the struct-literal grew. The mapping logic in this file
    /// is unchanged.
    fn extracted_with_text(text: &str) -> Extracted {
        Extracted {
            title: Some("Title".to_string()),
            text: text.to_string(),
            html: None,
            word_count: text.split_whitespace().count(),
            canonical_url: None,
            language: Some("en".to_string()),
            ..Extracted::default()
        }
    }

    #[test]
    fn ok_with_text_maps_to_ok_carrying_the_extracted() {
        let status = run_extraction(|| Ok(extracted_with_text("hello world")));
        match status {
            CrateStatus::Ok(e) => {
                assert_eq!(e.text, "hello world");
                assert_eq!(e.word_count, 2);
                assert_eq!(e.title.as_deref(), Some("Title"));
            }
            other => panic!("expected Ok carrying the Extracted, got {other:?}"),
        }
    }

    #[test]
    fn ok_with_empty_text_is_ok_not_error_not_not_implemented() {
        // THE Bug-E2 distinction (crate side): empty extraction is a VALID
        // result, never crate_error and never not_implemented.
        let status = run_extraction(|| Ok(extracted_with_text("")));
        match status {
            CrateStatus::Ok(e) => assert!(e.text.is_empty(), "text must be the empty string"),
            other => panic!("empty text must be Ok (valid), got {other:?}"),
        }
    }

    #[test]
    fn not_implemented_maps_to_distinct_not_implemented_status() {
        let status = run_extraction(|| Err(ExtractError::NotImplemented));
        // Must be the dedicated variant — NOT CrateError, NOT an empty Ok.
        assert!(
            matches!(status, CrateStatus::NotImplemented),
            "NotImplemented must map to the distinct first-class status, got {status:?}"
        );
    }

    #[test]
    fn caught_panic_maps_to_crate_error_and_process_survives() {
        // A panicking extraction (the real risk once the algorithm lands)
        // must NOT abort the run: catch_unwind turns it into CrateError and
        // execution continues past this call (the assertion below runs, and
        // so do all subsequent tests — proof the process survived).
        let status = run_extraction(|| -> Result<Extracted, ExtractError> {
            panic!("boom in extract");
        });
        match status {
            CrateStatus::CrateError(msg) => {
                assert!(
                    msg.starts_with("panic: "),
                    "panic must be tagged so the report distinguishes it from a \
                     returned error; got {msg:?}"
                );
                assert!(
                    msg.contains("boom in extract"),
                    "the panic message should be surfaced; got {msg:?}"
                );
            }
            other => panic!("a panic must map to CrateError, got {other:?}"),
        }
        // Reaching here at all proves the panic did not unwind out of
        // run_extraction and kill the test process.
    }

    #[test]
    fn string_payload_panic_message_is_recovered() {
        // `panic!("{}", ..)` / unwrap-style panics carry a String payload (a
        // different downcast arm than a &'static str literal).
        let status = run_extraction(|| -> Result<Extracted, ExtractError> {
            panic!("formatted {}", 42);
        });
        match status {
            CrateStatus::CrateError(msg) => {
                assert!(msg.starts_with("panic: "), "got {msg:?}");
                assert!(msg.contains("formatted 42"), "got {msg:?}");
            }
            other => panic!("expected CrateError, got {other:?}"),
        }
    }

    /// End-to-end: the **production** path (real `mdrcel::extract`, not an
    /// injected closure) on a page that yields a readable article surfaces
    /// as [`CrateStatus::Ok`] carrying the extracted body.
    ///
    /// **History.** This test used to assert `CrateStatus::NotImplemented` —
    /// the M1 floor every URL produced. The floor was retired at M2 Stage 1a
    /// (`d426df1`): `mdrcel::extract` now runs a real Readability port and
    /// returns a populated `Ok` for an article-bearing page. The original
    /// "yields_not_implemented_at_m1" assertion has been faithfully retired
    /// at M2 Stage 4 in line with that reality.
    #[test]
    fn production_run_crate_yields_ok_for_real_article_post_m1_floor() {
        let html = "<html><body><article><p>hello world</p></article></body></html>";
        let status = run_crate(html, Some("https://example.com/"));
        match status {
            CrateStatus::Ok(e) => {
                assert!(
                    e.text.contains("hello world"),
                    "production path must extract the article body now that the \
                     M1 NotImplemented floor was retired at M2 Stage 1a (`d426df1`); \
                     got text {:?}",
                    e.text
                );
            }
            other => panic!(
                "production path on an article-bearing page must be Ok post-M1; \
                 got {other:?} — see new tests in this module for the M1-floor \
                 retirement"
            ),
        }
    }

    /// Production path on an EMPTY document remains an `Ok` with empty text
    /// — Bug-E2 doctrine (an empty extraction is a valid result, not an
    /// error and not `NotImplemented`). Replaces the M1-era
    /// `..._not_implemented_regardless_of_base_url` test (which asserted
    /// `NotImplemented` for any base_url and is no longer true post-Stage-1a).
    #[test]
    fn production_run_crate_ok_with_empty_text_on_empty_document_bug_e2() {
        // base_url None: an empty document → faithful Readability null →
        // crate maps to empty Ok per Bug-E2 (lib.rs::extract_with `None =>`
        // arm).
        match run_crate("", None) {
            CrateStatus::Ok(e) => assert_eq!(
                e.text, "",
                "empty document ⇒ Ok with empty text (Bug-E2); got {:?}",
                e.text
            ),
            other => panic!("empty doc must be Ok(empty) per Bug-E2, got {other:?}"),
        }
        // base_url Some: the base_url is informational and must not change
        // the outcome. A tiny page with under-25-char paragraphs yields an
        // empty extraction (none qualify), also Ok(empty) per Bug-E2.
        match run_crate("<p>x</p>", Some("https://x/")) {
            CrateStatus::Ok(e) => {
                // The text may be empty or contain "x"; either way it is
                // OK and not NotImplemented — that is the point.
                assert!(
                    e.text.len() < 100,
                    "tiny doc ⇒ short Ok body; got {:?}",
                    e.text
                );
            }
            other => panic!("tiny doc must be Ok per Bug-E2, got {other:?}"),
        }
    }

    /// M2 Stage 4 (HLD §7.6) — the deliberate-fence-firing arm.
    /// `ExtractError::ContentTooShort` maps to `CrateError` with a
    /// well-formed reason string the report can surface.
    #[test]
    fn content_too_short_maps_to_crate_error_with_reason_string() {
        let status = run_extraction(|| {
            Err(mdrcel::ExtractError::ContentTooShort {
                word_count: 3,
                threshold: 50,
            })
        });
        match status {
            CrateStatus::CrateError(reason) => {
                assert!(
                    reason.starts_with("content_too_short: "),
                    "reason must be prefixed for the report; got {reason:?}"
                );
                assert!(
                    reason.contains('3') && reason.contains("50"),
                    "reason must carry both numbers; got {reason:?}"
                );
            }
            other => panic!("ContentTooShort must map to CrateError, got {other:?}"),
        }
    }

    /// The harness's actual corpus path (`Options::default()`, so
    /// `min_word_count == 0`) never fires `ContentTooShort` — confirming
    /// the "no harness-observable behaviour change" Stage-4 invariant
    /// (HLD §7.6 exit condition).
    #[test]
    fn run_crate_default_options_never_produces_content_too_short() {
        // A wholly empty document: at default options this is the canonical
        // Bug-E2 empty Ok, NOT ContentTooShort (the threshold is 0).
        let status = run_crate("", None);
        assert!(
            !matches!(status, CrateStatus::CrateError(ref r) if r.starts_with("content_too_short:")),
            "default-Options path must NEVER produce ContentTooShort; got {status:?}"
        );
    }
}
