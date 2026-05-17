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

At this stage the CLI prints `no corpus` and exits 0; the full pipeline
(corpus, oracles, scoring, report, regression) arrives in later stages.

## Declared reproducibility host

Pinning cannot pin native `libxml2`/ICU, so cross-machine byte-identity is
impossible without a container (deferred for v1, HLD §2.9). The committed
baseline is valid **only on one named host**; runs on any other machine are
advisory, not regression-gating.

Declared reproducibility host: Anvil (Windows 11 Pro 10.0.26200)

## Conventions (no config, no env vars, no flags — HLD §10)

- Oracle subprocess timeout: 180 s wall-clock (`oracle_timeout` status).
- Regression threshold: 5 % drop in word count or coverage-to-reference.
- Guardrail ratio: Readability word count > Trafilatura × 1.25 on a non-hub
  page flags suspected Trafilatura truncation.
- Corpus path, oracle entrypoints, and the above are conventions/constants,
  not configuration.
