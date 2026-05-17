# benchmark — mdrcel differential test harness

Scaffolding (not shipped to crates.io). For each URL in a fixed corpus it runs
the two oracle adapters and the `mdrcel` crate, scores their outputs against
the locked oracle hierarchy, and emits a markdown report plus a regression
check against a committed baseline. See
`wrk_docs/2026.05.17 - HLD - mdrcel Differential Test Harness.md`.

Run:

```
cargo run -p benchmark
```

For each corpus URL it spawns both oracle adapters, calls `mdrcel` in-process,
scores against the locked hierarchy, writes `runs/<UTC-ts>/{results.json,
report.md}`, then **regression-gates** the run against the committed baseline
(below). An absent manifest (or zero entries) still prints exactly `no corpus`
and exits 0.

Exit code: non-zero **only** for a *gated* regression (a run on the declared
host that regressed vs the committed baseline), a malformed corpus, a
host-detection failure, or a broken/unreadable baseline. A clean run, an
advisory run (off the declared host), or no committed baseline all exit 0.

## Declared reproducibility host

Pinning cannot pin native `libxml2`/ICU, so cross-machine byte-identity is
impossible without a container (deferred for v1, HLD §2.9). The committed
baseline is valid **only on one named host**; runs on any other machine are
advisory, not regression-gating.

Declared reproducibility host: Anvil (Windows 11 Pro 10.0.26200)

Host identity is compared in **canonical form** (trimmed, lowercased, short
hostname — FQDN domain stripped), so `ANVIL`, `anvil`, and `anvil.corp.local`
are the same host. A run whose canonical host matches the baseline's stamped
host is **regression-gating**; any other host is **advisory only** (the
comparison still runs and is shown in `report.md`, but it never fails CI).

## The committed baseline and how to update it (HLD §9 / §2.7 — manual, deliberate)

`benchmark/baseline/results.json` is the **only** persisted run state in git
(distinct from `benchmark/runs/`, which is gitignored scratch). The harness
**only ever reads** it. There is **deliberately no tooling** to write it — no
`set-baseline` subcommand, no migration. Updating the baseline is a deliberate
manual act, by design (HLD §9: "a file copy under version control is the
entire mechanism"):

1. Run the harness **on the declared reproducibility host** and confirm the
   run is what you intend to bless (review `runs/<latest>/report.md` — the
   `REGRESSIONS` block is at the top).
2. Copy that run's results over the baseline:

   ```
   cp benchmark/runs/<UTC-timestamp>/results.json benchmark/baseline/results.json
   ```

3. Commit **only** that file, with a message that **states why** the baseline
   moved (the first baseline is the documented Milestone-1 floor; every later
   move must justify the new numbers — this is the §2.7 gold-set-freeze
   discipline applied to the baseline):

   ```
   git add benchmark/baseline/results.json
   git commit -m "Baseline: <what changed and why these numbers are now correct>"
   ```

The very first commit of `benchmark/baseline/results.json` establishes the
floor; until it exists, every run prints `no baseline committed … this run is
a baseline candidate` and exits 0 (an honest *skipped*, never a false
"no regressions"). A baseline that is present but unreadable/malformed is a
**loud, non-zero** failure — it is never silently treated as "no regressions".

**What the gate does NOT detect — re-bless after ANY gold-set change (HLD
§2.7/§9).** The regression check compares **Coverage / word_count magnitudes
only**. It does **not** — and at M1 deliberately *cannot* — detect a change in
the *reference basis* behind otherwise-equal numbers: if a URL's reference
switches (Trafilatura → a newly-frozen gold entry) but the resulting Coverage
is numerically unchanged, the gate sees no regression even though "0.92 vs
Trafilatura" and "0.92 vs gold" are different claims. Storing the reference
kind in `results.json` is intentionally out of scope at M1 (no premature
mechanism, HLD §3). The compensating control is procedural and load-bearing:
**after ANY gold-set change the committed baseline MUST be re-blessed** by the
`cp` + commit ritual above, with the commit message stating why the numbers
(and now the basis) moved. Relatedly, a previously-`Scored` URL that becomes
`not_scored` for a **reference/oracle** reason (`reference_unavailable` /
`reference_empty`) is **listed** in the `REGRESSIONS` block as signal but is
**NOT** regression-gating (#2c — the oracle/reference moved under us; the crate
is not implicated, so it is never laundered into a crate red-CI). Only a
**crate**-owned loss (`not_implemented` / `crate_error`) gates. If the
reference environment legitimately changed, re-bless the baseline.

A gating run in which **every** URL is `not_scored` on **both** the baseline
and the current run (the Milestone-1 floor) is reported as **VACUOUS**: it is
correctly *not* a regression (exit 0) but the gate compared **no trusted
numbers**, so it is explicitly flagged as **NOT a substantive pass** (the same
honesty as the *skipped* no-baseline line — absence of a real comparison must
never read as a real pass).

## Conventions (no config, no env vars, no flags — HLD §10)

- Oracle subprocess timeout: 180 s wall-clock (`oracle_timeout` status).
- Regression threshold: a documented `REGRESSION_DROP = 0.05`. A URL regresses
  (on the declared host) iff, for a `Scored → Scored` transition, Coverage
  dropped by **more than 0.05 absolute** *or* crate word count shrank by
  **more than 5 % relative**; a previously-`Scored` URL now `not_scored` for a
  **crate**-owned reason (`not_implemented`/`crate_error`), or a baseline URL
  absent from the run, is also a (gating) regression. A previously-`Scored` URL
  now `not_scored` for a **reference/oracle**-owned reason
  (`reference_unavailable`/`reference_empty`) is **listed but NON-gating**
  (#2c — the comparison basis moved under us; not a crate regression — re-bless
  the baseline if the reference environment changed). A
  `not_scored → not_scored` URL is never a regression (absent numbers are
  never compared); `not_scored → Scored` is an improvement. The run timestamp
  is ignored (it always differs).
- Known coarseness (accepted at M1): a single flat absolute `0.05` Coverage
  band cannot distinguish a 0.04 drop from 0.97 (significant) vs from 0.50
  (likely noise). HLD §9 mandates the constant and there is no corpus evidence
  to tune a magnitude-relative or shape-aware band against yet; revisit once
  real corpus evidence exists (evidence-driven, not predicted).
- Guardrail ratio: Readability word count > Trafilatura × 1.25 on a non-hub
  page flags suspected Trafilatura truncation.
- Corpus path, oracle entrypoints, and the above are conventions/constants,
  not configuration.

## Snapshot capture (`fetch`) — developer-only, out-of-band (HLD §6)

`cargo run -p benchmark -- fetch <url>` captures a snapshot. It is **not**
part of the scoring run (which never touches the network). It shells out to
the system **`curl`**, which must be installed and on `PATH`; the harness
links no HTTP client of its own. If `curl` is absent or the request fails,
`fetch` errors and leaves no partial snapshot behind.
