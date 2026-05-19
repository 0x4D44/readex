//! M3 Stage 0c — Trafilatura-equivalence BLOCKER gate (skeleton).
//!
//! HLD §6.2: this gate is the M3 analogue of M2's `parser_equivalence_gate.rs`.
//! Where M2's gate proves the **DOM substrate** (html5ever + rcdom) is
//! token-sequence-identical to jsdom 29.1.1 before any extraction logic exists,
//! M3's gate proves the **converted-tree substrate** (post-`convert_tags()`)
//! is XML-serialization-identical to Python `lxml`'s equivalent before any
//! M3 extraction logic runs against it.
//!
//! **STATUS at commit time (Stage 0c):** SKELETON ONLY. The gate's assertion
//! body is `#[ignore]`'d with the explicit reason `"convert_tags is Stage 1b;
//! gate activates when Stage 1b lands."`. When Stage 1b ports `convert_tags`,
//! the implementer:
//!
//! 1. Removes the `#[ignore]` attribute on `trafilatura_converted_tree_gate`.
//! 2. Implements `rust_converted_tree(html)` to invoke
//!    `crate::trafilatura::cleaning::convert_tags` (Stage 1b deliverable) on
//!    the parsed `Dom`, then serialize the result via the new
//!    `dom::serialize_converted_tree` helper (also Stage 1b additive surface
//!    on the M2 dom facade, per HLD §5.1 additive-extension discipline).
//! 3. Implements `python_converted_tree(html)` to shell out to
//!    `benchmark/oracles/trafilatura/run.py` with a `--convert-tags-only`
//!    flag that emits the post-`convert_tags()` tree as canonical XML
//!    (the implementer adds this flag to the oracle adapter as a Stage 1b
//!    additive surface — the oracle adapter is in the test-time-only path
//!    and can grow additively without affecting the harness's frozen
//!    `bare_extraction()` contract).
//! 4. Asserts byte-identity on the serialised converted trees, OR a
//!    documented small-whitespace allowance (the HLD permits a documented
//!    delta because lxml's `etree.tostring()` and a Rust XML serializer
//!    are unlikely to match whitespace byte-for-byte; the M2 gate uses
//!    token-sequence-identity rather than byte-identity for the same
//!    reason).
//!
//! After activation, the gate is **frozen** like its M2 sibling: any future
//! commit to `convert_tags` or its dom-facade dependencies must keep the gate
//! green. Divergence is a BLOCKER — fix the port, never re-baseline the gate.
//!
//! **Fixture corpus** (HLD §6.2: "~10 representative URLs"):
//!
//! - 3 from the M2 gold tranche-1 (proven jsdom-equivalent under M2 gate).
//! - 3 from the M2 EDGAR/HMRC tranche (proven table-heavy survives M2 gate).
//! - 4 from `benchmark/corpus/` non-gold URLs covering BBC/news, Wikipedia,
//!   regulator, blog (the four canonical content shapes).
//!
//! At Stage 0c the fixture list is empty (`FIXTURES: &[&str] = &[]`); Stage
//! 1b's activator populates it from the actual on-disk corpus paths.

const FIXTURES: &[&str] = &[
    // Populated by Stage 1b's gate activator. Skeleton intentionally empty.
];

/// Stage 0c skeleton — gate activates at Stage 1b.
///
/// While `#[ignore]`'d, this function exists to:
/// 1. Pin the gate's API shape (signature, fixture-iteration loop, panic
///    diagnostic format) so Stage 1b's activator is a *removal of #[ignore]
///    plus body implementation*, not a re-design of the gate.
/// 2. Prove the gate file compiles cleanly with the test harness (so
///    `cargo test --test trafilatura_equivalence_gate` doesn't fail to
///    discover the test at Stage 0c).
/// 3. Provide a single, greppable "FIXME: STAGE 1B" anchor for the activator.
#[test]
#[ignore = "convert_tags is Stage 1b; gate activates when Stage 1b lands."]
fn trafilatura_converted_tree_gate() {
    // FIXME: STAGE 1B — replace this body with the real gate per the module
    // doc above. The fixture iteration shape is sketched below for the
    // activator's convenience.
    assert!(
        !FIXTURES.is_empty(),
        "Stage 1b activator: populate FIXTURES with ~10 corpus URL paths \
         before removing the #[ignore] attribute on this test."
    );
    for fixture_path in FIXTURES {
        let html = std::fs::read_to_string(fixture_path)
            .unwrap_or_else(|e| panic!("read fixture {fixture_path}: {e}"));
        let rust_xml = rust_converted_tree(&html);
        let python_xml = python_converted_tree(&html);
        assert_eq!(
            rust_xml, python_xml,
            "Trafilatura-equivalence gate divergence on {fixture_path}: \
             Rust converted tree != Python lxml converted tree.\n\
             Rust:\n{rust_xml}\n\nPython:\n{python_xml}",
        );
    }
}

/// Stage 1b activator implements this to invoke
/// `crate::trafilatura::cleaning::convert_tags` then serialize via the
/// dom-facade XML serializer (both Stage 1b deliverables).
fn rust_converted_tree(_html: &str) -> String {
    unimplemented!("Stage 1b activator: invoke convert_tags + serialize")
}

/// Stage 1b activator implements this to shell out to the oracle adapter's
/// `--convert-tags-only` mode (a Stage 1b additive flag on `run.py`).
fn python_converted_tree(_html: &str) -> String {
    unimplemented!(
        "Stage 1b activator: shell out to benchmark/oracles/trafilatura/run.py \
         --convert-tags-only"
    )
}

/// Smoke test: confirms the gate file compiles and is discoverable by the
/// test runner at Stage 0c. Always passes. NOT `#[ignore]`'d — runs as a
/// proof-of-life that `cargo test --test trafilatura_equivalence_gate`
/// reports a non-empty test list at Stage 0c.
#[test]
fn trafilatura_equivalence_gate_skeleton_present() {
    assert_eq!(
        FIXTURES.len(),
        0,
        "Stage 0c skeleton: FIXTURES empty by design"
    );
}
