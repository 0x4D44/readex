//! Oracle invocation: spawn the two oracle subprocesses, parse the contract
//! JSON, and map the result onto the harness's first-class status taxonomy.
//!
//! This is the **consumer** side of the oracle contract. The normative source
//! of truth is `benchmark/oracles/contract.schema.json` (owned by the oracle
//! team); harness HLD §5 is a non-normative restatement and the sibling Oracle
//! Adapters HLD §3.2/§3.3/§3.4 fixes the exact JSON shape, serialization
//! guarantee, and the `ok`/`error`/exit tri-state. On any discrepancy the
//! schema governs; [`schema_conformance`](tests) cross-checks our serde types
//! against the committed schema *when it exists* (it auto-activates the moment
//! the oracle team commits it — see that test). That cross-check is a
//! deliberate **structural probe**, not a full JSON-Schema validator (no
//! validator crate by HLD §3 design): it spot-checks the load-bearing
//! keys/types/nullability the harness depends on. Even when green, full
//! type/nullability conformance to the NORMATIVE schema remains formally
//! **UNVERIFIED until O1 is discharged**.
//!
//! # What the harness reads, and what it deliberately ignores
//!
//! The adapter emits **10** fields (sibling §3.2 — note: this build's sibling
//! revision lists `html`/`word_count` as captured-but-unscored; an earlier
//! draft mentioned an 11th `contract_version` which the reconciled harness §5
//! does not enumerate, so it is simply tolerated as an unknown field — see the
//! `deny_unknown_fields` note below). [`OracleResult`] captures all 10 so the
//! type is a faithful mirror of the wire object, but two are **captured yet
//! unused** by the harness:
//!
//! * `html` — never scored (harness HLD §5; sibling §3.2 "never scored").
//! * `word_count` — the adapter's own count is **informational only**. The
//!   harness recomputes word count itself via the single tokenizer in
//!   `metrics.rs` (harness HLD §8) in Stage 6, so there is exactly one
//!   word-count definition in the whole harness. We keep the field (so the
//!   wire object round-trips and the schema cross-check sees every property)
//!   but the value is *never read by scoring*. **This inertness is an
//!   obligation on Stage 6, not a property this module can enforce:** it
//!   holds *iff* `score.rs` never reads `OracleResult::word_count` and always
//!   recomputes via `metrics.rs`. Consequently this module deliberately does
//!   **not** validate/clamp the value — it parses any JSON integer verbatim
//!   (incl. negative or huge); a pinning test asserts that carry-through so a
//!   future change that starts trusting it is caught here.
//!
//! Both are retained rather than `#[serde(skip)]`'d so the serialized form in
//! the schema-conformance test is the *true* wire shape (every schema property
//! present), and so a future need (e.g. surfacing the adapter's own count for
//! diagnostics) needs no type change. They carry `#[allow(dead_code)]` until a
//! consumer exists — matching the established `metrics.rs`/`corpus.rs`
//! pre-consumer convention (a per-item allow, never a module-wide one, so an
//! unused `pub` item added later is still caught now that a non-test consumer
//! of this bin crate exists — see the O4 status note below for the precise,
//! partial scope of that enforcement; private items and never-constructed
//! enum variants remain uncaught and rely on convention).
//!
//! # Why no `#[serde(deny_unknown_fields)]`
//!
//! Mandated by harness HLD §5 and sibling §3.4/O3: the adapter may emit fields
//! the harness does not model (the sibling explicitly forbids the schema
//! setting `additionalProperties:false` against the consumer's narrower read
//! view). `deny_unknown_fields` would turn a *valid* envelope carrying an
//! extra field into a parse failure — a Bug-E2-class trap (a correct result
//! rejected as unparseable). So unknown fields are silently ignored.
//!
//! # Why `text` cannot make `serde_json` reject a valid extraction
//!
//! The adapter guarantees `text` is **valid UTF-8 before serialization**
//! (sibling §3.3: lone surrogates are replaced via a pinned never-raising
//! primitive). `serde_json` therefore cannot reject an otherwise-valid
//! extraction on account of `text`; a parse failure here always means a
//! genuinely malformed/truncated/absent stdout, which maps to
//! [`OracleStatus::OracleError`] (never silently to empty content — the
//! Bug-E2 lesson, consumer side).
//!
//! # The tri-state (harness HLD §5, sibling §3.4) — consumer mapping
//!
//! | Subprocess outcome                                   | [`OracleStatus`]              |
//! |------------------------------------------------------|-------------------------------|
//! | exit 0 **and** `ok:true` **and** `error == null` (even if `text:""`) | [`Ok`](OracleStatus::Ok)      |
//! | non-zero exit **or** `ok:false` **or** unparseable / absent stdout **or** `ok:true` with non-null `error` | [`OracleError`](OracleStatus::OracleError) |
//! | subprocess exceeded [`ORACLE_TIMEOUT`]               | [`OracleTimeout`](OracleStatus::OracleTimeout) |
//!
//! `text:""` with `ok:true` is **success** ("found little" is a valid result,
//! not an error — the exact distinction Bug E2 collapsed). `ok:false` and a
//! non-zero exit always co-occur on the adapter's failure path (sibling §3.2);
//! the consumer treats *either alone* as failure regardless. The harness is
//! the **last line of defense** (§5 doctrine) and does **not** trust the
//! sibling §3.2 invariant `ok == (error == null)`: an `ok:true` envelope that
//! *also* carries a non-null `error` is internally contradictory and is mapped
//! to [`OracleError`] (a "contract violation"), never laundered to
//! [`Ok`](OracleStatus::Ok) on the strength of `ok` alone. **Timeout is a
//! first-class status, never folded into `OracleError`** — conflating a
//! legitimately-slow large SEC filing with a hard error would be a
//! Bug-E2-class loss of information on exactly the named acceptance-critical cases.

// O4 status (Stage 6, 2026-05-17). `score.rs` (reachable from `main`'s
// no-subcommand path) now constructs `OracleKind` and calls `run_oracle`,
// which transitively reaches `oracle_command`, `run_command_with_timeout` and
// `ORACLE_TIMEOUT` — their pre-Stage-6 `#[allow(dead_code)]` + `TODO(stage-6)`
// tripwires were REMOVED (no longer dead code by construction: each now has a
// real consumer, so none depends on the lint to stay non-dead).
//
// O4 is only PARTIALLY discharged for this `benchmark` bin crate, NOT proven
// fully enforcing — state exactly what a verification probe under
// `clippy --workspace --all-targets -- -D warnings` shows:
//   * Unused `pub` items in this bin crate ARE now caught, because a real
//     non-test consumer (`score.rs`, reachable from `main`) exists, so rustc
//     seeds dead-code analysis from the binary root through the `pub` surface.
//   * Unused PRIVATE items and never-constructed ENUM VARIANTS in this bin
//     crate are STILL NOT compiler-caught — the original Stage-2 O4 caveat
//     persists there unchanged (notably: `OracleKind`'s variants are kept
//     non-dead by `score.rs` constructing them, NOT by the lint flagging an
//     unconstructed variant). These rely on convention + review.
// No module-wide `#![allow]` was ever added (deliberate), so the `pub`-surface
// half of the enforcement is genuine; the private / enum-variant half remains
// a convention, not a proven guarantee.
//
// Three items KEEP a scoped allow because Stage 6 deliberately does NOT
// consume them: `OracleKind::wire_name` (retargeted `TODO(stage-7)` — a
// report-label candidate, still test-only) and the `OracleResult.html` /
// `OracleResult.word_count` fields (INTENTIONALLY never scored — HLD §5/§8;
// kept purely for true 10-field wire fidelity, not a pending consumer). These
// are `pub`, so they sit in the genuinely-enforced half above: the scoped
// allow is load-bearing precisely because the lint WOULD otherwise flag them.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Wall-clock timeout for a single oracle subprocess (harness HLD §5,
/// sibling §3 open item).
///
/// A documented **code constant**, not configuration (HLD §10 — no env / no
/// flags). 180 s is deliberately generous: a legitimately slow large SEC
/// filing must get a fair chance before being recorded as
/// [`OracleStatus::OracleTimeout`]. Revisitable once real corpus timings
/// exist (evidence-driven, not predicted) — changing it is a one-line edit
/// here, by design.
//
// O4 (Stage 6, `pub`-surface half — genuinely caught): consumed by
// `run_oracle` on the non-test `score::score_corpus` → `main` path, so this
// `pub const` has a real consumer and the pre-Stage-6 `#[allow(dead_code)]` +
// `TODO(stage-6)` was removed. As a `pub` item it is in the half a probe
// shows IS now lint-enforced; see the module-level O4 status note for the
// private / enum-variant half that remains uncaught.
pub const ORACLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Which oracle to invoke. Exactly two, hardcoded by name (harness HLD §3 —
/// **no** plugin/registry abstraction; the M8-ring-road antipattern).
///
/// O4 status (Stage 6): `score::score_corpus` constructs both
/// `OracleKind::Trafilatura` and `OracleKind::ReadabilityJs` on the non-test
/// `main` path, so the pre-Stage-6 per-item `#[allow(dead_code)]` +
/// `TODO(stage-6)` was removed because every variant now has a real
/// constructor — NOT because the lint would otherwise flag an unconstructed
/// variant. A verification probe confirms never-constructed ENUM VARIANTS
/// (and unused private items) in this `benchmark` bin crate are STILL NOT
/// compiler-caught under `clippy --workspace --all-targets -- -D warnings`:
/// the original Stage-2 O4 caveat persists for the non-`pub`-surface case.
/// These variants stay non-dead by convention + `score.rs` actually
/// constructing them, not by the dead-code lint (contrast the `pub`-surface
/// items, which the same probe shows ARE now caught).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleKind {
    /// Trafilatura (#1 in the asymmetric hierarchy) — `python run.py`.
    Trafilatura,
    /// Readability-JS (#2, the guardrail) — `node run.mjs`.
    ReadabilityJs,
}

impl OracleKind {
    /// The exact `oracle` string this kind's adapter stamps into its JSON
    /// (sibling §3.2 hardcoded literal). The inverse pairing is asserted in
    /// tests so the wire spelling is pinned.
    // NOT consumed by Stage 6: `score.rs` derives status/attribution from the
    // parsed `OracleResult.oracle` + `OracleStatus`, never via this helper, so
    // the allow is KEPT (still test-only). TODO retargeted to Stage 7 (the
    // report may use it for per-oracle column labels).
    #[allow(dead_code)] // TODO(stage-7): candidate for report.rs oracle labels.
    pub fn wire_name(self) -> &'static str {
        match self {
            OracleKind::Trafilatura => "trafilatura",
            OracleKind::ReadabilityJs => "readability-js",
        }
    }
}

/// The single JSON object an oracle adapter prints on stdout (sibling §3.2 —
/// the non-normative restatement; the committed schema governs on any
/// discrepancy).
///
/// **No `#[serde(deny_unknown_fields)]`** — see the module docs (mandated by
/// harness HLD §5; an extra field must not fail the parse). `oracle_version`
/// is `Option<String>` because a failed import still emits a valid envelope
/// with `oracle_version: null` (typing it non-null would be the Bug-E2 trap
/// of a valid failure report rejected as unparseable). `text` is **never
/// null** (`""` when none) and is guaranteed valid UTF-8 by the adapter
/// (sibling §3.3), so deserialization cannot reject an otherwise-valid result
/// on its account.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct OracleResult {
    /// `"trafilatura"` | `"readability-js"` (sibling §3.2 hardcoded literal).
    pub oracle: String,
    /// `null` iff the library could not be imported / its version was
    /// unreadable — MUST be `Option` (sibling §3.2, O3; harness HLD §5).
    pub oracle_version: Option<String>,
    /// Document title, or `null`.
    pub title: Option<String>,
    /// The primary comparison surface. **Never `null`** — `""` when nothing
    /// was extracted (sibling §3.2). Guaranteed valid UTF-8 by the adapter
    /// (sibling §3.3).
    pub text: String,
    /// Extracted HTML where the tool produces it for free. **Captured but
    /// never scored** (harness HLD §5; sibling §3.2 "never scored").
    //
    // O4: allow KEPT — Stage 6 (`score.rs`) confirmed this is deliberately
    // never read by scoring (HLD §5). It is wire-fidelity only (so the
    // serialized form is the true 10-field shape and the schema cross-check
    // sees every property), NOT a pending future-stage consumer.
    #[allow(dead_code)] // INTENTIONALLY never scored; kept for true wire shape.
    pub html: Option<String>,
    /// The adapter's *own* word count — **informational only**. The harness
    /// recomputes word count via the single tokenizer (`metrics.rs`, HLD §8);
    /// this value is captured for wire fidelity but **never read by scoring**.
    /// `i64` (signed) because JSON has no unsigned integers and the schema
    /// types it as a plain integer; the harness never arithmetically depends
    /// on it so the (impossible-in-practice) negative is harmless here.
    //
    // O4: allow KEPT — Stage 6 (`score.rs`) recomputes word count via
    // `metrics::word_count` on `.text` and DELIBERATELY never reads this
    // field (HLD §8 — exactly one word-count definition). Wire-fidelity only,
    // NOT a pending future-stage consumer; the inertness is now an enforced
    // Stage-6 fact, not a forward promise.
    #[allow(dead_code)] // INTENTIONALLY unused by scoring; kept for wire shape.
    pub word_count: Option<i64>,
    /// The tool's reported canonical URL, or `null`. Not scored.
    pub canonical_url: Option<String>,
    /// Best-effort detected language, or `null`.
    pub language: Option<String>,
    /// `true` iff the adapter ran and produced a (possibly empty) result;
    /// `false` on the adapter's own failure path (co-occurs with a non-zero
    /// exit — sibling §3.2).
    pub ok: bool,
    /// Human-readable failure message on the failure path; `null` when
    /// `ok:true` (sibling §3.2 invariant `ok == (error == null)`).
    pub error: Option<String>,
}

/// First-class outcome of one oracle invocation (harness HLD §5 status
/// taxonomy — the anti-Bug-E2 tri-state).
///
/// [`OracleTimeout`](Self::OracleTimeout) is **deliberately distinct** from
/// [`OracleError`](Self::OracleError): a legitimately slow large filing must
/// be distinguishable from a hard failure (load-bearing for the consumer-critical
/// slow filings). The error/timeout variants carry a human-readable reason so
/// the report (Stage 7) can surface *why* without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleStatus {
    /// exit 0 **and** `ok:true` — even if `text:""` ("found little" is a
    /// valid result, NOT an error). Carries the parsed [`OracleResult`].
    Ok(Box<OracleResult>),
    /// Non-zero exit, **or** `ok:false`, **or** stdout was absent / not a
    /// single valid contract JSON object. Never silently treated as empty
    /// content (the Bug-E2 lesson). Carries a human-readable reason.
    OracleError(String),
    /// The subprocess exceeded [`ORACLE_TIMEOUT`] and was killed. A distinct
    /// status — never folded into [`OracleError`](Self::OracleError). Carries
    /// the timeout that was exceeded (for the report).
    OracleTimeout(String),
}

/// Build the **exact** `(program, args)` to invoke an oracle adapter
/// (harness HLD §5 / sibling §3.1 — the fixed, unconfigurable convention).
///
/// This is a **pure** function (no I/O, no process spawn) so the argv is unit
/// tested directly:
///
/// * Trafilatura → program `python`, args
///   `["<repo>/benchmark/oracles/trafilatura/run.py", "<abs-snapshot>"]`
/// * ReadabilityJs → program `node`, args
///   `["<repo>/benchmark/oracles/readability-js/run.mjs", "<abs-snapshot>"]`
///
/// `["--base-url", "<URL>"]` is appended (in that order, as two separate
/// argv entries) iff `base_url` is `Some`. Interpreter literals are bare
/// `python` / `node` exactly as the sibling expects (the Trafilatura adapter
/// makes bare `python` correct *by construction* via its venv re-exec,
/// sibling §4; the Node side's lack of an equivalent is the cross-team open
/// item §11.1 — not the harness's concern at the invocation seam).
///
/// `snapshot_abs` MUST already be absolute (the caller resolves it; the
/// adapter MUST NOT chdir or resolve it relative to its own dir — sibling
/// §3.1). It is passed through **verbatim**. This precondition is no longer
/// doc-only: a [`debug_assert!`] fails loudly on a relative path in dev/test
/// builds (zero release cost), and a test pins both the panic and the
/// verbatim pass-through of an absolute path.
///
/// The script path is rooted at this crate's `oracles/` tree via
/// `CARGO_MANIFEST_DIR` (compile-time, working-directory independent — same
/// technique as `main.rs::corpus_dir`).
// O4 (Stage 6, `pub`-surface half — genuinely caught): reached via
// `run_oracle` from the non-test `score::score_corpus` → `main` path, so this
// `pub fn` has a real consumer and the pre-Stage-6 allow was removed. As a
// `pub` item it is in the half a verification probe shows IS now lint-enforced
// (unused `pub` items in this bin crate are caught once a non-test consumer
// exists); the private / enum-variant half remains uncaught — see the
// module-level O4 status note.
pub fn oracle_command(
    kind: OracleKind,
    snapshot_abs: &Path,
    base_url: Option<&str>,
) -> (String, Vec<String>) {
    // Enforce the sibling §3.1 precondition the harness owns: the snapshot
    // path passed to the adapter MUST be absolute (the adapter MUST NOT
    // resolve it relative to its own dir). debug_assert: caught in dev/test
    // builds, zero release cost — this is an internal harness invariant (the
    // caller resolves the path), not untrusted external input.
    debug_assert!(
        snapshot_abs.is_absolute(),
        "snapshot path passed to the oracle adapter must be absolute \
         (sibling §3.1 — the adapter must not resolve it relative to its own \
         dir); got: {}",
        snapshot_abs.display()
    );

    // CARGO_MANIFEST_DIR == benchmark/, so the oracle tree is always
    // benchmark/oracles/ regardless of the process working directory
    // (fixed convention, not configuration — HLD §10).
    let oracles = Path::new(env!("CARGO_MANIFEST_DIR")).join("oracles");

    let (program, script) = match kind {
        OracleKind::Trafilatura => ("python", oracles.join("trafilatura").join("run.py")),
        OracleKind::ReadabilityJs => ("node", oracles.join("readability-js").join("run.mjs")),
    };

    let mut args = vec![
        script.to_string_lossy().into_owned(),
        snapshot_abs.to_string_lossy().into_owned(),
    ];
    if let Some(url) = base_url {
        args.push("--base-url".to_string());
        args.push(url.to_string());
    }

    (program.to_string(), args)
}

/// Run an oracle adapter for `snapshot_abs` and map the outcome onto
/// [`OracleStatus`] (the production entry point — uses the fixed
/// [`oracle_command`] convention).
///
/// Thin wrapper over [`run_command_with_timeout`] (the testable seam): it
/// only chooses the fixed argv and the [`ORACLE_TIMEOUT`] constant, so the
/// spawn / timeout / parse / tri-state logic is exercised by tests **without**
/// the real adapters (which the oracle team has not yet delivered).
// O4 (Stage 6, `pub`-surface half — genuinely caught): `score::score_corpus`
// calls this for both oracles on the non-test `main` path, so this `pub fn`
// has a real consumer and the pre-Stage-6 allow was removed. As a `pub` item
// it is in the half a verification probe shows IS now lint-enforced; the
// private / enum-variant half remains uncaught — see the module-level O4
// status note.
pub fn run_oracle(kind: OracleKind, snapshot_abs: &Path, base_url: Option<&str>) -> OracleStatus {
    let (program, args) = oracle_command(kind, snapshot_abs, base_url);
    run_command_with_timeout(&program, &args, ORACLE_TIMEOUT)
}

/// The testable seam: spawn `program` with `args`, enforce a real wall-clock
/// `timeout`, and map the outcome onto [`OracleStatus`] per the tri-state.
///
/// Kept **minimal on purpose** — a single function taking the command as
/// parameters, *not* a trait/plugin tower (no premature abstraction, HLD §3).
/// Production callers go through [`run_oracle`] (fixed [`oracle_command`]);
/// tests point this at a trivial cross-platform helper (a file-cat for the
/// JSON-shape cases, a blocking child for the timeout case) so the spawn /
/// timeout / parse logic is fully unit-testable on the Windows dev box
/// without the real adapters.
///
/// # Timeout (real wall clock, sync, std only — no async, no new deps)
///
/// `spawn` the child with piped stdout, then **poll** `try_wait` against an
/// [`Instant`] deadline with a short sleep between polls. On expiry the child
/// is `kill`'d (and reaped) and the result is
/// [`OracleStatus::OracleTimeout`]. The poll granularity is small (well under
/// a second) so the timeout *test* completes in well under a second with a
/// short injected `timeout` — it never has to wait the production 180 s.
///
/// stdout is drained **concurrently** by a dedicated reader thread spawned
/// the instant the child is, *not* after the poll loop observes exit. This is
/// mandatory, not an optimisation: the OS pipe buffer is finite (~64 KB).
/// Sibling §3.3's "single write then flush" guarantees no *interleaved*
/// stdout, but a single `write()` of more than the buffer still **blocks
/// part-way** until a reader consumes it — write atomicity does not exempt a
/// large write from back-pressure. So a non-draining poll loop would let any
/// child whose stdout exceeds the buffer (every real SEC 10-K — text alone is
/// far over 64 KB) block in `write()`, never exit, and be falsely recorded as
/// [`OracleStatus::OracleTimeout`] — a Bug-E2-class conflation on exactly the
/// the consumer-critical large filings. The reader `read_to_end`s into a `Vec<u8>`
/// while the poll loop independently watches the deadline. On the **success**
/// path the child has exited and closed its write end, so the reader is at
/// EOF and `join()` returns promptly with the full stdout; bytes → `String`
/// happens *after* that join, preserving the non-UTF-8 → [`OracleError`]
/// mapping (never empty content — the Bug-E2 lesson, consumer side). On the
/// **timeout/error** path the reader is **detached, not joined**: `kill()`
/// only closes *our direct child's* write end, but an orphaned grandchild
/// (the documented "# Known limitation" — e.g. the Trafilatura venv worker)
/// can keep the inherited write end open, so joining could re-stall the
/// harness for the orphan's full runtime, defeating the timeout. The detached
/// reader is a harmless short-lived daemon that ends when the pipe finally
/// closes; stdout is discarded on those paths anyway. stderr is left
/// inherited: the adapters send all logs/parser noise there (sibling §3.2)
/// and the harness does not parse it (so it cannot itself deadlock us).
///
/// # Known limitation — `kill()` on timeout can orphan the real worker
///
/// [`Child::kill`] terminates **only the direct child**. The Trafilatura
/// adapter self-corrects its interpreter by **re-execing into its venv
/// interpreter as a subprocess child** (sibling §4 — a subprocess-proxy, not
/// `os.execv`, because Windows `os.execv` corrupts the exit code and argv).
/// So on the timeout path we kill the launcher `python`, but the venv worker
/// it spawned is **orphaned** and runs to completion in the background — a
/// resource leak across a long corpus run. The recorded verdict is still
/// **correctly** [`OracleStatus::OracleTimeout`]: the launcher is dead, no
/// parseable stdout reaches us, and this is a slow-extraction signal, not a
/// correctness/Bug-E2 issue. Tree-kill (Windows Job Object / POSIX process
/// group) is **deliberately DEFERRED** for the v1 single-host sandbox: it
/// would need platform-specific code/deps, which HLD §3 ("no new deps",
/// sync/std only) forbids here. Documented, not fixed, by design.
///
/// # Tri-state mapping (harness HLD §5 / sibling §3.4)
///
/// 1. spawn fails → [`OracleError`](OracleStatus::OracleError) (no parseable
///    stdout — e.g. interpreter absent).
/// 2. exceeded `timeout` → [`OracleTimeout`](OracleStatus::OracleTimeout)
///    (distinct; never folded into the error case).
/// 3. exited, but non-zero status → `OracleError` (the failure path; `ok` is
///    not even consulted — a non-zero exit alone is failure).
/// 4. exited 0, stdout not a single valid [`OracleResult`] → `OracleError`
///    (absent / truncated / malformed — never laundered into empty content).
/// 5. exited 0, parsed, `ok:false` → `OracleError` (carries `error`).
/// 6. exited 0, parsed, `ok:true` **but** `error` non-null → `OracleError`
///    (a "contract violation": violates sibling §3.2 `ok == (error == null)`;
///    the harness is the last line of defense and refuses to trust `ok`
///    alone — §5 doctrine).
/// 7. exited 0, parsed, `ok:true`, `error == null` → [`Ok`](OracleStatus::Ok)
///    (**even if `text:""`** — "found little" is success).
// O4 (Stage 6, `pub`-surface half — genuinely caught): reached via
// `run_oracle` from the non-test `score::score_corpus` → `main` path, so this
// `pub fn` has a real consumer and the pre-Stage-6 allow was removed. As a
// `pub` item it is in the half a verification probe shows IS now lint-enforced;
// the private / enum-variant half remains uncaught — see the module-level O4
// status note.
pub fn run_command_with_timeout(program: &str, args: &[String], timeout: Duration) -> OracleStatus {
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        // stderr inherited: adapters log there (sibling §3.2); not parsed.
        .spawn()
    {
        Ok(c) => c,
        // Spawn failure (e.g. `python`/`node` absent) is "no parseable
        // stdout" → OracleError, NOT a timeout (HLD §5 / sibling §3.4).
        Err(e) => {
            return OracleStatus::OracleError(format!(
                "failed to spawn oracle process `{program}`: {e} \
                 (is the interpreter installed and on PATH?)"
            ));
        }
    };

    // Drain stdout on a DEDICATED THREAD started immediately — see the doc:
    // the OS pipe buffer is finite (~64 KB), so a child writing more than
    // that blocks in `write()` until someone reads, regardless of sibling
    // §3.3 single-write atomicity. The poll loop below must NOT be the only
    // reader or a large-but-valid stdout deadlocks → false OracleTimeout. The
    // reader `read_to_end`s raw bytes; UTF-8 validation is deferred until
    // after the join so the non-UTF-8 → OracleError mapping is preserved.
    // `stdout` is `Some` here (we requested `Stdio::piped()`).
    let mut stdout_pipe = child
        .stdout
        .take()
        .expect("child stdout is piped, so take() yields Some");
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        // The io::Result is carried out so a pipe read error still maps to
        // OracleError (never silently to empty content).
        stdout_pipe.read_to_end(&mut buf).map(|_| buf)
    });

    // Poll for completion against a wall-clock deadline. 25 ms granularity:
    // negligible CPU, yet the timeout TEST (short injected `timeout`) still
    // finishes in well under a second — it never waits the real 180 s.
    let deadline = Instant::now() + timeout;
    let poll = Duration::from_millis(25);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    // Exceeded the wall clock → kill, reap, and report the
                    // DISTINCT timeout status (never OracleError — HLD §5).
                    // DETACH the reader (drop the handle) rather than join:
                    // `kill()` only closes OUR direct child's write end, but
                    // an orphaned grandchild (cmd→ping in the test; the
                    // Trafilatura launcher→venv worker in production, the
                    // documented "# Known limitation" above) inherited the
                    // pipe and keeps the write end OPEN, so the reader would
                    // NOT get EOF until that orphan exits. Joining here would
                    // re-stall the harness for the orphan's full runtime —
                    // exactly the timeout this path exists to bound. The
                    // detached reader is a short-lived daemon: it ends on its
                    // own when the pipe finally closes, holds nothing the
                    // harness needs (we discard stdout on timeout), and never
                    // blocks our return.
                    let _ = child.kill();
                    let _ = child.wait(); // reap; ignore the (killed) status.
                    drop(reader); // detach; do NOT join (see above).
                    return OracleStatus::OracleTimeout(format!(
                        "oracle process `{program}` exceeded the {} s wall-clock \
                         timeout and was killed",
                        timeout.as_secs()
                    ));
                }
                std::thread::sleep(poll);
            }
            Err(e) => {
                // Cannot determine child state — treat as a hard error (not a
                // timeout): we have no parseable result. Kill+reap, then
                // DETACH the reader (same reasoning as the timeout path: an
                // orphaned grandchild may keep the write end open, so joining
                // could stall; we discard stdout on this error path anyway).
                let _ = child.kill();
                let _ = child.wait();
                drop(reader); // detach; do NOT join (orphan may hold the pipe).
                return OracleStatus::OracleError(format!(
                    "failed while waiting on oracle process `{program}`: {e}"
                ));
            }
        }
    };

    // Success/clean-exit path: our direct child has exited. Joining is prompt
    // here (unlike the timeout path) because the only grandchild in scope is
    // the Trafilatura venv worker, and the launcher PROPAGATES its exit code
    // via `sys.exit(child.returncode)` (sibling §4) — i.e. it waits for that
    // worker before exiting, so by the time we observe the launcher's exit
    // the worker has already closed the inherited write end and the reader is
    // at EOF. (The test seam has no grandchild at all.) Join to collect the
    // full stdout.
    let stdout = match reader.join() {
        // Bytes → String AFTER the join. A non-UTF-8 stdout is itself a
        // contract violation → unparseable (OracleError), never empty content
        // (preserves the Bug-E2 consumer-side mapping).
        Ok(Ok(bytes)) => match String::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                return OracleStatus::OracleError(format!(
                    "oracle process `{program}` stdout was not valid UTF-8: {e}"
                ));
            }
        },
        Ok(Err(e)) => {
            return OracleStatus::OracleError(format!(
                "oracle process `{program}` stdout was unreadable: {e}"
            ));
        }
        Err(_) => {
            return OracleStatus::OracleError(format!(
                "oracle process `{program}` stdout reader thread panicked"
            ));
        }
    };

    // Non-zero exit is the failure path (ok:false co-occurs, but a non-zero
    // exit ALONE is failure — sibling §3.2; do not even consult `ok`).
    if !status.success() {
        return OracleStatus::OracleError(format!(
            "oracle process `{program}` exited unsuccessfully ({status}); \
             stdout was: {}",
            truncate_for_msg(&stdout)
        ));
    }

    // Exited 0: stdout MUST be a single valid contract object. An absent /
    // truncated / malformed stdout is a hard error, NEVER laundered into
    // empty content (the Bug-E2 lesson, consumer side).
    let result: OracleResult = match serde_json::from_str(stdout.trim()) {
        Ok(r) => r,
        Err(e) => {
            return OracleStatus::OracleError(format!(
                "oracle process `{program}` exited 0 but stdout was not a \
                 single valid contract JSON object: {e}; stdout was: {}",
                truncate_for_msg(&stdout)
            ));
        }
    };

    // Parsed. `ok:false` is the adapter's own failure path → OracleError
    // carrying its message. `ok:true` (even with text:"") is success — UNLESS
    // it also carries a non-null `error`, which violates the sibling §3.2
    // invariant `ok == (error == null)`. The §5 doctrine is that the harness
    // is the LAST line of defense and must NOT trust upstream invariants: an
    // `ok:true`+non-null-`error` envelope is internally contradictory, so we
    // refuse to launder it to Ok and surface it as a hard error (never
    // silently trusting `ok` alone — the Bug-E2 lesson, consumer side).
    if result.ok {
        if let Some(err) = &result.error {
            return OracleStatus::OracleError(format!(
                "oracle `{}` contract violation: ok:true with non-null error: {err}",
                result.oracle
            ));
        }
        OracleStatus::Ok(Box::new(result))
    } else {
        let detail = result
            .error
            .clone()
            .unwrap_or_else(|| "(no error message provided)".to_string());
        OracleStatus::OracleError(format!(
            "oracle `{}` reported ok:false: {detail}",
            result.oracle
        ))
    }
}

/// Clamp an arbitrarily large stdout to a bounded snippet for an error
/// message (a 50 MB EDGAR filing's stdout must not be inlined whole into the
/// report). Pure helper; not a contract surface.
fn truncate_for_msg(s: &str) -> String {
    const MAX: usize = 300;
    let trimmed = s.trim();
    if trimmed.len() <= MAX {
        trimmed.to_string()
    } else {
        // Respect char boundaries: take whole chars up to the budget.
        let cut: String = trimmed.chars().take(MAX).collect();
        format!("{cut}… [truncated, {} bytes total]", trimmed.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- A representative, fully-populated wire object ----------------------

    /// A valid Trafilatura envelope with **all 10** contract fields present
    /// and `ok:true`. The shared fixture for the deserialize / status / schema
    /// tests so "the wire shape" is defined in exactly one place.
    fn full_json_ok() -> String {
        r#"{
            "oracle": "trafilatura",
            "oracle_version": "2.0.0",
            "title": "An Example Article",
            "text": "The body text of the article.",
            "html": "<p>The body text of the article.</p>",
            "word_count": 6,
            "canonical_url": "https://example.test/article",
            "language": "en",
            "ok": true,
            "error": null
        }"#
        .to_string()
    }

    /// A platform-appropriate **absolute** snapshot path for the argv tests.
    /// `oracle_command` now enforces (debug_assert, review #7) that the
    /// snapshot is absolute per sibling §3.1; a Unix-style `/abs/...` literal
    /// is NOT absolute on Windows (needs a drive/UNC prefix), so the fixture
    /// must match the host. Defined once so the argv tests share one notion
    /// of "an absolute snapshot".
    fn abs_snapshot(rel: &str) -> std::path::PathBuf {
        let rel = rel.trim_start_matches('/');
        if cfg!(windows) {
            Path::new(r"C:\abs").join(rel)
        } else {
            Path::new("/abs").join(rel)
        }
    }

    // ---- oracle_command: exact argv, both kinds, ±base_url -----------------

    #[test]
    fn oracle_command_trafilatura_no_base_url() {
        let snap = abs_snapshot("corpus/snapshots/deadbeef.html");
        let (program, args) = oracle_command(OracleKind::Trafilatura, &snap, None);

        assert_eq!(program, "python");
        // [script, snapshot] — exactly two args, no --base-url.
        assert_eq!(args.len(), 2);
        assert!(
            args[0]
                .replace('\\', "/")
                .ends_with("benchmark/oracles/trafilatura/run.py"),
            "script path was {:?}",
            args[0]
        );
        // Snapshot forwarded verbatim (exactly the bytes the caller passed).
        assert_eq!(args[1], snap.to_string_lossy());
    }

    #[test]
    fn oracle_command_trafilatura_with_base_url() {
        let snap = abs_snapshot("snap.html");
        let (program, args) = oracle_command(
            OracleKind::Trafilatura,
            &snap,
            Some("https://example.test/a"),
        );

        assert_eq!(program, "python");
        // [script, snapshot, "--base-url", "<URL>"] — base-url is two argv
        // entries, in that order, appended last.
        assert_eq!(args.len(), 4);
        assert!(args[0].replace('\\', "/").ends_with("trafilatura/run.py"));
        assert_eq!(args[1], snap.to_string_lossy());
        assert_eq!(args[2], "--base-url");
        assert_eq!(args[3], "https://example.test/a");
    }

    #[test]
    fn oracle_command_readability_no_base_url() {
        let snap = abs_snapshot("snap.html");
        let (program, args) = oracle_command(OracleKind::ReadabilityJs, &snap, None);

        assert_eq!(program, "node");
        assert_eq!(args.len(), 2);
        assert!(
            args[0]
                .replace('\\', "/")
                .ends_with("benchmark/oracles/readability-js/run.mjs"),
            "script path was {:?}",
            args[0]
        );
        assert_eq!(args[1], snap.to_string_lossy());
    }

    #[test]
    fn oracle_command_readability_with_base_url() {
        let snap = abs_snapshot("snap.html");
        let (program, args) =
            oracle_command(OracleKind::ReadabilityJs, &snap, Some("https://x.test/"));

        assert_eq!(program, "node");
        assert_eq!(args.len(), 4);
        assert!(
            args[0]
                .replace('\\', "/")
                .ends_with("readability-js/run.mjs")
        );
        assert_eq!(args[1], snap.to_string_lossy());
        assert_eq!(args[2], "--base-url");
        assert_eq!(args[3], "https://x.test/");
    }

    #[test]
    fn oracle_command_script_path_is_absolute() {
        // CARGO_MANIFEST_DIR is absolute, so the derived script path must be
        // absolute on every platform (the adapter must not resolve it
        // relative to its own dir — sibling §3.1).
        let snap = abs_snapshot("s.html");
        for kind in [OracleKind::Trafilatura, OracleKind::ReadabilityJs] {
            let (_p, args) = oracle_command(kind, &snap, None);
            assert!(
                Path::new(&args[0]).is_absolute(),
                "{kind:?} script path not absolute: {:?}",
                args[0]
            );
        }
    }

    #[test]
    fn oracle_command_passes_absolute_snapshot_through_verbatim() {
        // Review #7: the absolute-path precondition is the harness's to own
        // (sibling §3.1 — the adapter must NOT resolve relative paths). The
        // happy path: an already-absolute snapshot is forwarded BYTE-FOR-BYTE
        // as argv[1], for both kinds, ± base_url (no normalisation/rewrite).
        let abs = if cfg!(windows) {
            r"C:\abs\corpus\snapshots\deadbeef.html"
        } else {
            "/abs/corpus/snapshots/deadbeef.html"
        };
        for kind in [OracleKind::Trafilatura, OracleKind::ReadabilityJs] {
            let (_p, args) = oracle_command(kind, Path::new(abs), Some("https://x.test/"));
            assert_eq!(
                args[1], abs,
                "{kind:?}: absolute snapshot must pass through verbatim"
            );
            assert!(Path::new(&args[1]).is_absolute());
        }
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "must be absolute")]
    fn oracle_command_relative_snapshot_trips_debug_assert() {
        // Review #7: a RELATIVE snapshot path is a harness bug (the caller
        // must resolve it before the seam — sibling §3.1). The doc-only
        // precondition is now enforced by a debug_assert!; this pins that it
        // fires in dev/test builds. Gated on `debug_assertions` because the
        // assert is (correctly) compiled out of release builds — running the
        // test suite in release must not spuriously fail here.
        let _ = oracle_command(
            OracleKind::Trafilatura,
            Path::new("relative/snap.html"),
            None,
        );
    }

    #[test]
    fn oracle_kind_wire_names_are_pinned() {
        assert_eq!(OracleKind::Trafilatura.wire_name(), "trafilatura");
        assert_eq!(OracleKind::ReadabilityJs.wire_name(), "readability-js");
    }

    // ---- OracleResult deserialization --------------------------------------

    #[test]
    fn deserializes_full_ten_field_object() {
        let r: OracleResult = serde_json::from_str(&full_json_ok()).unwrap();
        assert_eq!(r.oracle, "trafilatura");
        assert_eq!(r.oracle_version.as_deref(), Some("2.0.0"));
        assert_eq!(r.title.as_deref(), Some("An Example Article"));
        assert_eq!(r.text, "The body text of the article.");
        assert_eq!(
            r.html.as_deref(),
            Some("<p>The body text of the article.</p>")
        );
        assert_eq!(r.word_count, Some(6));
        assert_eq!(
            r.canonical_url.as_deref(),
            Some("https://example.test/article")
        );
        assert_eq!(r.language.as_deref(), Some("en"));
        assert!(r.ok);
        assert_eq!(r.error, None);
    }

    #[test]
    fn deserializes_with_unknown_extra_field_no_deny_unknown() {
        // An extra field the harness does not model (e.g. a future
        // `contract_version`, or the sibling's `dir`/`byline`) MUST NOT fail
        // the parse — no #[serde(deny_unknown_fields)] (HLD §5 / sibling O3).
        let json = r#"{
            "oracle": "readability-js",
            "oracle_version": "0.6.0",
            "title": null,
            "text": "body",
            "html": null,
            "word_count": 1,
            "canonical_url": null,
            "language": null,
            "ok": true,
            "error": null,
            "contract_version": 1,
            "some_future_field": {"nested": true}
        }"#;
        let r: OracleResult =
            serde_json::from_str(json).expect("unknown fields must be ignored, not rejected");
        assert_eq!(r.oracle, "readability-js");
        assert_eq!(r.text, "body");
    }

    #[test]
    fn oracle_version_null_deserializes_to_none() {
        // A failed import still emits a valid envelope with
        // oracle_version: null → MUST map to None (sibling O3 / HLD §5).
        let json = r#"{
            "oracle": "trafilatura",
            "oracle_version": null,
            "title": null,
            "text": "",
            "html": null,
            "word_count": null,
            "canonical_url": null,
            "language": null,
            "ok": false,
            "error": "ImportError: trafilatura not installed"
        }"#;
        let r: OracleResult = serde_json::from_str(json).unwrap();
        assert_eq!(r.oracle_version, None);
        assert_eq!(r.word_count, None);
        assert!(!r.ok);
    }

    #[test]
    fn word_count_is_carried_through_untouched_never_clamped() {
        // Review #5: `word_count` is informational ONLY — the harness
        // recomputes via metrics.rs (HLD §8) and this module deliberately
        // neither validates nor clamps it. Its inertness is a Stage-6
        // obligation (score.rs must never read it). Pin the carry-through so a
        // regression that starts clamping/rejecting/panicking on the adapter's
        // count is caught here: a negative AND an i64-max value must both
        // deserialize to a valid `ok:true` envelope with the value verbatim.
        for wc in [-5_i64, i64::MAX] {
            let json = format!(
                r#"{{
                    "oracle": "trafilatura",
                    "oracle_version": "2.0.0",
                    "title": null,
                    "text": "body",
                    "html": null,
                    "word_count": {wc},
                    "canonical_url": null,
                    "language": null,
                    "ok": true,
                    "error": null
                }}"#
            );
            let r: OracleResult =
                serde_json::from_str(&json).expect("any JSON integer word_count must parse");
            assert_eq!(
                r.word_count,
                Some(wc),
                "word_count must be carried through untouched (not clamped), wc={wc}"
            );
            assert!(r.ok);
            // And it must flow through the seam to Ok with the value intact —
            // proving nothing on the status path inspects/clamps it either.
            let (p, a, path) = emit_file_command(&json);
            let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
            let _ = std::fs::remove_file(&path);
            match status {
                OracleStatus::Ok(r) => assert_eq!(r.word_count, Some(wc)),
                other => panic!("expected Ok with word_count={wc}, got {other:?}"),
            }
        }
    }

    #[test]
    fn utf8_text_round_trips_including_non_ascii() {
        // text is guaranteed valid UTF-8 by the adapter (sibling §3.3);
        // non-ASCII (incl. astral) must round-trip through serde unchanged.
        let json = r#"{
            "oracle": "trafilatura",
            "oracle_version": "2.0.0",
            "title": "Café — résumé",
            "text": "Crème brûlée, naïve façade. 日本語のテキスト. Emoji: 🦀.",
            "html": null,
            "word_count": 9,
            "canonical_url": null,
            "language": "fr",
            "ok": true,
            "error": null
        }"#;
        let r: OracleResult = serde_json::from_str(json).unwrap();
        assert_eq!(
            r.text,
            "Crème brûlée, naïve façade. 日本語のテキスト. Emoji: 🦀."
        );
        assert_eq!(r.title.as_deref(), Some("Café — résumé"));
        // Serialize → deserialize must preserve the bytes exactly.
        let reserialized = serde_json::to_string(&r).unwrap();
        let r2: OracleResult = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(r, r2);
    }

    // ---- Tri-state mapping via the testable seam ---------------------------
    //
    // The seam is exercised WITHOUT the real adapters using only
    // cross-platform std primitives: a temp file `cat`/`type`'d to stdout for
    // the JSON-shape cases, and a blocking child for the timeout case. No
    // assumption of `python`/`node`/`sh`/`sleep` being installed.

    /// `(program, args)` that writes `content` verbatim to stdout and exits 0,
    /// on both Windows and Unix, using only the platform's always-present
    /// shell + a file the test wrote. Robust to any bytes in `content`
    /// (quotes/braces/newlines) because the content travels via a file, never
    /// the command line.
    fn emit_file_command(content: &str) -> (String, Vec<String>, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "mdrcel-oracle-seam-{}-{n}.json",
            std::process::id()
        ));
        std::fs::write(&path, content).unwrap();
        let p = path.to_string_lossy().into_owned();
        let (program, args) = if cfg!(windows) {
            // `cmd /C type <file>` — `type` echoes the file verbatim.
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "type".to_string(), p],
            )
        } else {
            ("cat".to_string(), vec![p])
        };
        (program, args, path)
    }

    /// `(program, args)` for a child that **blocks for ~5 s** (longer than
    /// the short injected test timeout) so the timeout path is exercised
    /// without the test itself sleeping. Cross-platform, std-only, no
    /// dependence on a `sleep` binary: Windows `ping -n 5 127.0.0.1` blocks
    /// ~4 s; Unix `sh -c 'sleep 5'`.
    fn slow_command() -> (String, Vec<String>) {
        if cfg!(windows) {
            (
                "cmd".to_string(),
                vec![
                    "/C".to_string(),
                    "ping".to_string(),
                    "-n".to_string(),
                    "5".to_string(),
                    "127.0.0.1".to_string(),
                ],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "sleep 5".to_string()],
            )
        }
    }

    #[test]
    fn status_ok_when_exit0_and_ok_true() {
        let (p, a, path) = emit_file_command(&full_json_ok());
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::Ok(r) => {
                assert_eq!(r.oracle, "trafilatura");
                assert!(r.ok);
                assert_eq!(r.text, "The body text of the article.");
            }
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn status_ok_when_text_empty_but_ok_true() {
        // "found little" (text:"") with ok:true is SUCCESS, not an error —
        // the exact distinction Bug E2 collapsed (HLD §5 / sibling §3.4).
        let json = r#"{
            "oracle": "readability-js",
            "oracle_version": "0.6.0",
            "title": null,
            "text": "",
            "html": null,
            "word_count": 0,
            "canonical_url": null,
            "language": null,
            "ok": true,
            "error": null
        }"#;
        let (p, a, path) = emit_file_command(json);
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::Ok(r) => {
                assert_eq!(r.text, "");
                assert!(r.ok);
            }
            other => panic!("expected Ok (empty text is valid), got {other:?}"),
        }
    }

    #[test]
    fn status_error_when_ok_false_with_message() {
        // ok:false (the adapter's own catchable-failure path) → OracleError
        // carrying the message; NOT silently treated as empty content.
        let json = r#"{
            "oracle": "trafilatura",
            "oracle_version": null,
            "title": null,
            "text": "",
            "html": null,
            "word_count": null,
            "canonical_url": null,
            "language": null,
            "ok": false,
            "error": "could not read input file: /abs/missing.html"
        }"#;
        let (p, a, path) = emit_file_command(json);
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::OracleError(msg) => {
                assert!(
                    msg.contains("could not read input file"),
                    "error must carry the adapter message, got: {msg}"
                );
            }
            other => panic!("expected OracleError, got {other:?}"),
        }
    }

    #[test]
    fn status_error_on_contract_violation_ok_true_with_nonnull_error() {
        // Review #3 / §5 last-defense doctrine: exit 0 + `ok:true` but a
        // NON-NULL `error` violates the sibling §3.2 invariant
        // `ok == (error == null)`. The harness must NOT trust the upstream
        // invariant and launder this to Ok on the strength of `ok` alone —
        // it is internally contradictory and must surface as a hard error.
        let json = r#"{
            "oracle": "trafilatura",
            "oracle_version": "2.0.0",
            "title": "Contradictory envelope",
            "text": "some text was produced",
            "html": null,
            "word_count": 4,
            "canonical_url": null,
            "language": "en",
            "ok": true,
            "error": "but an error was also reported"
        }"#;
        let (p, a, path) = emit_file_command(json);
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::OracleError(msg) => {
                assert!(
                    msg.contains("contract violation")
                        && msg.contains("ok:true with non-null error")
                        && msg.contains("but an error was also reported"),
                    "must report the contradiction and carry the error, got: {msg}"
                );
            }
            other => panic!(
                "ok:true + non-null error must be OracleError (contract \
                 violation), never Ok; got {other:?}"
            ),
        }
    }

    #[test]
    fn status_error_on_unparseable_stdout() {
        // Exit 0 but stdout is not a single valid contract object → hard
        // error, NEVER laundered into empty content (Bug-E2, consumer side).
        let (p, a, path) = emit_file_command("this is not json at all <<>>");
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::OracleError(msg) => {
                assert!(
                    msg.contains("not a single valid contract JSON object"),
                    "got: {msg}"
                );
            }
            other => panic!("expected OracleError, got {other:?}"),
        }
    }

    #[test]
    fn status_error_on_empty_stdout() {
        // Absent stdout (exit 0, nothing written) is also unparseable →
        // OracleError, not Ok-with-empty-text.
        let (p, a, path) = emit_file_command("");
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let _ = std::fs::remove_file(&path);
        assert!(
            matches!(status, OracleStatus::OracleError(_)),
            "empty stdout must be OracleError, got {status:?}"
        );
    }

    #[test]
    fn status_error_on_nonzero_exit() {
        // Non-zero exit ALONE is the failure path — `ok` is not even
        // consulted (sibling §3.2). `exit`/`false` are shell built-ins/std.
        let (program, args) = if cfg!(windows) {
            (
                "cmd".to_string(),
                vec!["/C".to_string(), "exit 7".to_string()],
            )
        } else {
            (
                "sh".to_string(),
                vec!["-c".to_string(), "exit 7".to_string()],
            )
        };
        match run_command_with_timeout(&program, &args, ORACLE_TIMEOUT) {
            OracleStatus::OracleError(msg) => {
                assert!(msg.contains("exited unsuccessfully"), "got: {msg}");
            }
            other => panic!("expected OracleError on non-zero exit, got {other:?}"),
        }
    }

    #[test]
    fn status_error_when_spawn_fails() {
        // A program that cannot be spawned (interpreter absent) is "no
        // parseable stdout" → OracleError, NOT OracleTimeout.
        let status =
            run_command_with_timeout("mdrcel-no-such-program-xyz-123", &[], ORACLE_TIMEOUT);
        match status {
            OracleStatus::OracleError(msg) => {
                assert!(msg.contains("failed to spawn"), "got: {msg}");
            }
            other => panic!("expected OracleError on spawn failure, got {other:?}"),
        }
    }

    #[test]
    fn status_timeout_when_subprocess_exceeds_injected_timeout() {
        // THE timeout path. The seam takes the timeout as a parameter, so the
        // test injects a SHORT one (150 ms) against a child that blocks ~5 s.
        // The test therefore completes in well under a second and NEVER waits
        // the production ORACLE_TIMEOUT (180 s). Timeout is a DISTINCT status,
        // never folded into OracleError (HLD §5 — load-bearing for slow
        // the consumer filings).
        let (program, args) = slow_command();
        let started = Instant::now();
        let status = run_command_with_timeout(&program, &args, Duration::from_millis(150));
        let elapsed = started.elapsed();

        match status {
            OracleStatus::OracleTimeout(msg) => {
                assert!(
                    msg.contains("exceeded") && msg.contains("timeout"),
                    "timeout reason should explain itself, got: {msg}"
                );
            }
            other => panic!("expected OracleTimeout, got {other:?}"),
        }
        // The whole call must return promptly after the injected deadline —
        // proves we don't wait for the child's full ~5 s (let alone 180 s).
        // Generous 4 s bound: kill+reap + CI jitter, still ≪ the 5 s child.
        assert!(
            elapsed < Duration::from_secs(4),
            "timeout path must return promptly after the injected deadline, \
             took {elapsed:?}"
        );
    }

    #[test]
    fn large_valid_stdout_is_ok_not_false_timeout() {
        // REGRESSION (review #1): the OS pipe buffer is ~64 KB. If the poll
        // loop does not drain stdout concurrently, a child writing more than
        // that blocks in `write()` (full-buffer writes block regardless of the
        // single-write atomicity in sibling §3.3), never exits, and the harness
        // hits the wall-clock timeout — a FALSE `OracleTimeout` on every real
        // SEC 10-K (large-but-VALID output; the named acceptance-critical case Bug E2
        // warns about). This test pushes ≥256 KB of VALID contract JSON through
        // the cross-platform seam with the FULL production ORACLE_TIMEOUT and
        // asserts the verdict is `Ok` — NOT `OracleTimeout`. It deadlocks (and
        // thus times out / fails) against the pre-fix post-exit-drain code and
        // passes once stdout is drained on a concurrent reader thread.
        let big_text = "lorem ipsum dolor sit amet ".repeat(12_000); // ≈324 KB
        let json = format!(
            r#"{{
                "oracle": "trafilatura",
                "oracle_version": "2.0.0",
                "title": "A very large SEC 10-K filing",
                "text": {},
                "html": null,
                "word_count": 60000,
                "canonical_url": null,
                "language": "en",
                "ok": true,
                "error": null
            }}"#,
            serde_json::to_string(&big_text).unwrap()
        );
        assert!(
            json.len() >= 256 * 1024,
            "fixture must exceed the ~64 KB pipe buffer by a wide margin; was {} bytes",
            json.len()
        );
        let (p, a, path) = emit_file_command(&json);
        let started = Instant::now();
        let status = run_command_with_timeout(&p, &a, ORACLE_TIMEOUT);
        let elapsed = started.elapsed();
        let _ = std::fs::remove_file(&path);
        match status {
            OracleStatus::Ok(r) => {
                assert!(r.ok);
                assert_eq!(r.text, big_text);
            }
            other => panic!(
                "≥256 KB of valid JSON must be Ok, not a false timeout; \
                 got {other:?} after {elapsed:?}"
            ),
        }
        // It must also return promptly (it drains while the child runs, not
        // after a 180 s stall): a concurrently-drained child finishes in well
        // under a second, so anything near the timeout would prove a deadlock.
        assert!(
            elapsed < Duration::from_secs(10),
            "draining must be concurrent; a ≥256 KB valid write took {elapsed:?}"
        );
    }

    // ---- Schema-conformance: R1-gated, AUTO-ACTIVATING ---------------------

    /// Structurally cross-check our serde [`OracleResult`] against the
    /// oracle-team-owned `benchmark/oracles/contract.schema.json` **iff it
    /// exists at test runtime**.
    ///
    /// AUTO-ACTIVATION: the schema file is owned by the oracle team and is
    /// absent until they commit it (this build runs before that delivery).
    /// Rather than `#[ignore]` (which would silently stay off forever and
    /// need a human to remember to flip it), this test *passes with a clear
    /// skip notice* when the file is absent and *starts enforcing
    /// automatically* the moment the file appears on disk — strictly better
    /// for a cross-team gate.
    ///
    /// It loads the schema as `serde_json::Value` only — deliberately **no**
    /// json-schema validator crate (that would be a speculative heavy
    /// dependency for one structural check; HLD §3 minimal-deps). It asserts
    /// the *structural* contract the harness depends on:
    ///   * every key the schema marks `required` is present in our serialized
    ///     representative object (we can satisfy the schema);
    ///   * our field names are a subset of the schema's `properties` (we
    ///     model nothing the schema does not declare);
    ///   * `oracle_version` is nullable in the schema (sibling O3 — the one
    ///     nullability the harness structurally depends on);
    ///   * the *types* of the load-bearing fields match what the harness's
    ///     serde types assume, when the schema declares them: `text` is a
    ///     `string` **and not nullable** (the never-null comparison surface —
    ///     a nullable `text` would silently break the `String` field), `ok`
    ///     is a `boolean`, `oracle` is a `string`; and per-field nullability
    ///     for the `Option` fields is cross-checked (flagging, e.g., a schema
    ///     that wrongly marks `text` nullable).
    ///
    /// **Caveat (deliberately a probe, not a validator).** Even when this
    /// test is green it is a *structural probe only*: it spot-checks the
    /// load-bearing keys/types/nullability, NOT full JSON-Schema conformance
    /// of `OracleResult` to the NORMATIVE `contract.schema.json` (no full
    /// validator crate by HLD §3 design). The contract therefore remains
    /// formally **UNVERIFIED until O1 is discharged** — a green probe lowers
    /// risk but does not close O1.
    //
    // TODO(oracle-schema, ref O1): when the schema lands, the oracle team
    // MUST have committed it under `.gitattributes` `-text` (byte-stable;
    // their obligation per sibling §2.1/§8, NOT ours — this harness task does
    // not create anything under benchmark/oracles/).
    #[test]
    fn schema_conformance_when_schema_present() {
        // CARGO_MANIFEST_DIR == benchmark/.
        let schema_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("oracles")
            .join("contract.schema.json");

        if !schema_path.is_file() {
            // AUTO-ACTIVATING skip: not #[ignore]. This becomes a real,
            // enforcing test automatically once the oracle team commits the
            // schema — no code change, no human flipping a flag.
            eprintln!(
                "[schema_conformance] SKIPPED — {} not present yet \
                 (oracle-team-owned; this test AUTO-ACTIVATES the moment it \
                 is committed — ref TODO(oracle-schema, O1)).",
                schema_path.display()
            );
            return;
        }

        let schema_txt =
            std::fs::read_to_string(&schema_path).expect("schema present but unreadable");
        let schema: serde_json::Value =
            serde_json::from_str(&schema_txt).expect("contract.schema.json is not valid JSON");

        // Our representative serialized object = the exact set of field names
        // the harness emits/reads.
        let sample: OracleResult = serde_json::from_str(&full_json_ok()).unwrap();
        let sample_val = serde_json::to_value(&sample).unwrap();
        let our_fields: std::collections::BTreeSet<&str> = sample_val
            .as_object()
            .expect("OracleResult serializes to an object")
            .keys()
            .map(String::as_str)
            .collect();

        // 1. Every schema-`required` key must be present in our output.
        if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
            for req in required {
                let key = req.as_str().expect("`required` entries are strings");
                assert!(
                    our_fields.contains(key),
                    "schema requires `{key}` but OracleResult does not emit \
                     it; our fields = {our_fields:?}"
                );
            }
        }

        // 2. Our field names ⊆ schema `properties` (we model nothing the
        //    schema does not declare).
        let props = schema
            .get("properties")
            .and_then(|p| p.as_object())
            .expect("schema must declare `properties`");
        for f in &our_fields {
            assert!(
                props.contains_key(*f),
                "OracleResult emits `{f}` which is not a declared schema \
                 property; schema properties = {:?}",
                props.keys().collect::<Vec<_>>()
            );
        }

        // 3. `oracle_version` must be nullable in the schema (sibling O3 —
        //    the harness types it Option and structurally depends on this).
        //    Accept any of the common JSON-Schema nullable spellings without
        //    pulling a schema crate: `"type": [..,"null"]`, an `anyOf`/`oneOf`
        //    containing a null type, or an explicit `"nullable": true`.
        let ov = props
            .get("oracle_version")
            .expect("schema must declare `oracle_version`");
        assert!(
            json_type_is_nullable(ov),
            "schema must allow `oracle_version` to be null (sibling O3; the \
             harness types it Option<String>); declaration was: {ov}"
        );

        // 4. TYPES of the load-bearing fields must match the serde types the
        //    harness assumes — checked only where the schema declares the
        //    field (a structural probe, NOT full schema validation; see the
        //    caveat on this test). A type mismatch here is a latent Bug-E2:
        //    e.g. a nullable `text` would silently defeat the never-null
        //    comparison surface.
        if let Some(text_decl) = props.get("text") {
            assert!(
                json_type_admits(text_decl, "string"),
                "schema must type `text` as string (the primary comparison \
                 surface; harness types it `String`); declaration was: {text_decl}"
            );
            assert!(
                !json_type_is_nullable(text_decl),
                "schema must NOT mark `text` nullable — the harness types it \
                 non-Option `String` and relies on the never-null guarantee \
                 (sibling §3.2/§3.3); a nullable `text` is a Bug-E2 trap; \
                 declaration was: {text_decl}"
            );
        }
        if let Some(ok_decl) = props.get("ok") {
            assert!(
                json_type_admits(ok_decl, "boolean"),
                "schema must type `ok` as boolean (the harness types it \
                 `bool` and branches the tri-state on it); declaration \
                 was: {ok_decl}"
            );
            assert!(
                !json_type_is_nullable(ok_decl),
                "schema must NOT mark `ok` nullable — the harness types it \
                 non-Option `bool`; declaration was: {ok_decl}"
            );
        }
        if let Some(oracle_decl) = props.get("oracle") {
            assert!(
                json_type_admits(oracle_decl, "string"),
                "schema must type `oracle` as string (the harness types it \
                 `String` for result attribution); declaration was: {oracle_decl}"
            );
        }

        // 5. Cross-check per-field nullability for the harness's Option
        //    fields: each MUST be schema-nullable (the harness types them
        //    `Option<_>` and structurally relies on `null` being legal).
        //    `oracle_version` is already asserted above (sibling O3); the
        //    rest are the remaining Option fields on `OracleResult`.
        for opt_field in [
            "title",
            "html",
            "word_count",
            "canonical_url",
            "language",
            "error",
        ] {
            if let Some(decl) = props.get(opt_field) {
                assert!(
                    json_type_is_nullable(decl),
                    "schema must allow `{opt_field}` to be null — the harness \
                     types it `Option<_>`; declaration was: {decl}"
                );
            }
        }
    }

    /// True if a JSON-Schema property declaration permits `null`, across the
    /// common spellings (no schema crate — a tiny structural probe):
    /// `"type":"null"`, `"type":[...,"null"]`, `"nullable":true`, or an
    /// `anyOf`/`oneOf`/`allOf` whose members include any of those.
    fn json_type_is_nullable(decl: &serde_json::Value) -> bool {
        // `"nullable": true` (OpenAPI-flavoured schemas).
        if decl.get("nullable").and_then(|n| n.as_bool()) == Some(true) {
            return true;
        }
        // `"type": "null"` or `"type": ["string","null"]`.
        match decl.get("type") {
            Some(serde_json::Value::String(s)) if s == "null" => return true,
            Some(serde_json::Value::Array(types))
                if types.iter().any(|t| t.as_str() == Some("null")) =>
            {
                return true;
            }
            _ => {}
        }
        // anyOf / oneOf / allOf containing a nullable member.
        for combinator in ["anyOf", "oneOf", "allOf"] {
            if decl
                .get(combinator)
                .and_then(|c| c.as_array())
                .is_some_and(|arr| arr.iter().any(json_type_is_nullable))
            {
                return true;
            }
        }
        false
    }

    /// True if a JSON-Schema property declaration permits the given JSON
    /// primitive type `want` (e.g. `"string"`, `"boolean"`), across the same
    /// spellings `json_type_is_nullable` tolerates (no schema crate — a tiny
    /// structural probe): `"type":"<want>"`, `"type":[...,"<want>"]`, or an
    /// `anyOf`/`oneOf`/`allOf` whose members include any of those. A missing
    /// `type`/combinator is treated as **permissive** (an unconstrained
    /// schema fragment does not by itself contradict the harness's type) so
    /// the probe only fails on a declaration that actively excludes `want`.
    fn json_type_admits(decl: &serde_json::Value, want: &str) -> bool {
        match decl.get("type") {
            Some(serde_json::Value::String(s)) => return s == want,
            Some(serde_json::Value::Array(types)) => {
                return types.iter().any(|t| t.as_str() == Some(want));
            }
            _ => {}
        }
        let mut saw_combinator = false;
        for combinator in ["anyOf", "oneOf", "allOf"] {
            if let Some(arr) = decl.get(combinator).and_then(|c| c.as_array()) {
                saw_combinator = true;
                if arr.iter().any(|m| json_type_admits(m, want)) {
                    return true;
                }
            }
        }
        // No `type` and no matching combinator member: permissive only if the
        // declaration imposed no type constraint at all (e.g. `{}` / a pure
        // `$ref` we do not resolve) — do NOT pass a declaration that listed
        // combinators none of which admit `want`.
        !saw_combinator
    }
}
