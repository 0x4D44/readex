# Readability-JS oracle adapter

`run.mjs` runs one **pinned** Mozilla Readability extraction (jsdom inert &
offline) and prints exactly one JSON object (shape governed by
`../contract.schema.json`; behaviour by HLD *mdrcel Oracle Adapters* §3).

## Pinned versions (declared reproducibility host)

| Component | Pin | Source of truth |
|---|---|---|
| `@mozilla/readability` | **0.6.0** (latest as of 2026-05-17) | `package.json` (exact) + `package-lock.json` |
| `jsdom` | **29.1.1** (latest as of 2026-05-17) | `package.json` (exact) + `package-lock.json` |
| Full dependency tree | locked | `package-lock.json` (committed) |
| Node | **24.12.0** | `.nvmrc` + `package.json` `engines` (`>=20` for `String.prototype.toWellFormed`) |
| OS | Windows 11 (win32) | this file |
| Resolved on | 2026-05-17 | this file |

`oracle_version` is emitted at runtime
(`createRequire(...)('@mozilla/readability/package.json').version`) so version
skew shows in the consumer's baselines. Node auto-resolves
`./node_modules` relative to the script, so no venv/re-exec is needed (only
the Trafilatura adapter self-bootstraps).

**Cross-machine byte-identity is NOT guaranteed** (host ICU is not pinned by
`package-lock.json`); the document-and-lock floor + a single declared CI host
is the v1 contract (HLD §2/§3.5). The container is the only true cross-machine
fix and is deferred.

## One-time bootstrap (needs network; run once per clone)

`node_modules/` is git-ignored and **absent on a fresh clone**. From this
directory (`benchmark/oracles/readability-js`):

```sh
npm ci   # reproduces EXACTLY from the committed package-lock.json
```

`npm ci` installs `devDependencies` too, which includes the **dev-time-only**
schema validator `ajv` used by `selftest.mjs` — it is NOT in the adapter's
runtime closure (HLD §6) and is never imported by `run.mjs`.

## One-command smoke

```sh
node benchmark/oracles/readability-js/run.mjs <absolute-path-to-an-html-file>
```

Expected on a content page: one JSON line, `"ok": true`, non-empty `"text"`,
`"oracle_version": "0.6.0"`, exit 0.

## Self-test (HLD §7 contract proof)

`selftest.mjs` is a **dev-time tool**, not part of the adapter's runtime
closure and not invoked by the harness (the harness only runs `run.mjs`). It
imports the **dev-only** `ajv` schema validator, so run it with the same Node
whose `./node_modules` has the dev dependencies installed — i.e. after
`npm ci` (above) in `benchmark/oracles/readability-js`, plain `node` resolves
`ajv` correctly. (`run.mjs` itself never imports `ajv`; a fresh clone that has
not run `npm ci` will fail the selftest with a `Cannot find module 'ajv'` —
that is the missing one-time bootstrap, not an adapter defect.)

```sh
node benchmark/oracles/readability-js/selftest.mjs
```

Asserts, against `../contract.schema.json` and the committed `../fixtures/`:
(1) `article.html` ⇒ schema-valid, `ok:true`, exit 0, substantive `text`, no
`<head>/<script>/<style>` leak, one clean object despite a malformed CSS rule
(exercises the §5 stderr-only virtual console); (2) `empty.html` ⇒
schema-valid, `ok:true`, `text:""`, exit 0 (Bug-E2 "found nothing");
(3) nonexistent path ⇒ schema-valid failure envelope, `ok:false`, `error` set,
exit ≠ 0 (Bug-E2 "blew up — catchable"); (4) §3.3 lone-surrogate
`toWellFormed()` guard via a source-level escape; (5) re-run step 1
byte-identical (same-machine determinism); (6) `--base-url` contract surface —
valid ⇒ `ok:true` & deterministic, absent ⇒ `ok:true` (unchanged), and
structurally-invalid ⇒ `ok:true` with substantive `text` (a malformed base URL
is AUXILIARY and degrades gracefully to the no-base path — byte-identical to
it — rather than hard-failing the extraction). Exit 0 iff all pass.

## Pin-bump checklist (HLD §5/§6)

On any upstream bump, re-verify and re-record here:

1. `package.json` re-pinned exact; `package-lock.json` regenerated and
   committed; re-installed via `npm ci`.
2. **`@mozilla/readability` still ships NO `exports` map** — so
   `require('@mozilla/readability/package.json')` subpath resolves for the
   `oracle_version` read. (0.6.0: verified no `exports`.)
3. **A bare `new jsdom.VirtualConsole()` still installs a no-op `error`
   listener** (so §5 stdout hygiene holds without `forwardTo`/`sendTo`).
   (jsdom 29.1.1: verified — 1 `error` listener, `emit('error',…)` does not
   throw.)
4. `.parse()` still returns `{title, lang, content, textContent, …}` and
   `null` is still the "found nothing" signal.
5. `String.prototype.toWellFormed` still available (Node `engines` floor).
6. Node / OS versions in the table above re-recorded.
