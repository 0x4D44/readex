# Trafilatura oracle adapter

`run.py` runs one **pinned** Trafilatura extraction with no runtime network
and prints exactly one JSON object (shape governed by
`../contract.schema.json`; behaviour by HLD *mdrcel Oracle Adapters* §3).

## Pinned versions (declared reproducibility host)

| Component | Pin | Source of truth |
|---|---|---|
| Trafilatura | **2.0.0** (latest 2.x as of 2026-05-17) | `requirements.txt` (exact `==`) |
| Full transitive closure | frozen, incl. the fallback cascade (jusText, lxml, htmldate, …) — the Barbaresi-2021 SOTA *is* Trafilatura + fallbacks | `requirements.txt` |
| Python | **3.12.10** | this file |
| OS | Windows 11 (win32) | this file |
| `libxml2` (via `lxml==6.1.0`) | the wheel-bundled libxml2 for the CPython 3.12 ABI on win32 | `lxml` wheel |
| Frozen on | 2026-05-17 | this file |

`oracle_version` is emitted at runtime (`importlib.metadata.version`) so
version skew shows in the consumer's baselines. `lxml` ships per-CPython-ABI
wheels (a different interpreter ⇒ a different native libxml2 ⇒ a different
`text`); `requirements.txt` therefore reproduces **only** under the matching
interpreter. `run.py` guarantees that by construction: it re-runs itself as a
child under `./.venv` (subprocess-proxy, HLD §4), so bare `python run.py` is
correct regardless of system packages.

**Cross-machine byte-identity is NOT guaranteed** (native libxml2/ICU are not
pinned by `pip freeze`); the document-and-lock floor + a single declared CI
host is the v1 contract (HLD §2/§3.5). The container is the only true
cross-machine fix and is deferred.

## One-time bootstrap (needs network; run once per clone)

The `.venv` is git-ignored and **absent on a fresh clone**. From the repo root:

```sh
python -m venv benchmark/oracles/trafilatura/.venv
benchmark/oracles/trafilatura/.venv/Scripts/python -m pip install -r benchmark/oracles/trafilatura/requirements.txt
# dev-time-only validator for selftest.py (NOT in the runtime closure, HLD §6):
benchmark/oracles/trafilatura/.venv/Scripts/python -m pip install "jsonschema>=4,<5"
```

(POSIX: use `.venv/bin/python` instead of `.venv/Scripts/python`.)

## One-command smoke

```sh
python benchmark/oracles/trafilatura/run.py <absolute-path-to-an-html-file>
```

Expected on a content page: one JSON line, `"ok": true`, non-empty `"text"`,
`"oracle_version": "2.0.0"`, exit 0.

## Self-test (HLD §7 contract proof)

`selftest.py` is a **dev-time tool**, not part of the adapter's runtime
closure and not invoked by the harness (the harness only runs `run.py`). It
imports the **dev-only** `jsonschema` validator installed into `./.venv`, so
it must be run with the **venv** interpreter shown below — NOT bare `python`.
(`run.py` self-re-execs into the venv and so is correct under bare `python`;
`selftest.py` does not, by design — it asserts the contract directly. A bare
`python selftest.py` on a system without `jsonschema` fails with
`ModuleNotFoundError: jsonschema` — that is the missing one-time bootstrap,
not an adapter defect.)

```sh
benchmark/oracles/trafilatura/.venv/Scripts/python benchmark/oracles/trafilatura/selftest.py
```

(POSIX: `.venv/bin/python` instead of `.venv/Scripts/python`.)

Asserts, against `../contract.schema.json` and the committed `../fixtures/`:
(1) `article.html` ⇒ schema-valid, `ok:true`, exit 0, substantive `text`, no
`<head>/<script>/<style>` leak, one clean object despite a malformed CSS rule;
(2) `empty.html` ⇒ schema-valid, `ok:true`, `text:""`, exit 0 (Bug-E2 "found
nothing"); (3) nonexistent path ⇒ schema-valid failure envelope, `ok:false`,
`error` set, exit ≠ 0 (Bug-E2 "blew up — catchable"); (4) §3.3 lone-surrogate
primitive guard via a source-level escape; (5) re-run step 1 byte-identical
(same-machine determinism); (6) non-Latin-1 / Windows-codepage UTF-8 stdout
regression guard; (7) `--base-url` contract surface — valid ⇒ `ok:true` &
deterministic, absent ⇒ `ok:true` (unchanged), structurally-invalid ⇒
`ok:true` with substantive `text` (Trafilatura already tolerates a malformed
`url=`; locked in here, keeping the two adapters symmetric on this auxiliary
surface). Exit 0 iff all pass.

## Pin-bump checklist

On any upstream bump, re-verify and re-record here:

1. `requirements.txt` re-frozen via `pip freeze` under the matching Python
   ABI; the fallback cascade still pinned.
2. `bare_extraction(..., with_metadata=True)` still returns a `Document` whose
   `.as_dict()` **method** exists (the `as_dict=` *parameter* is deprecated in
   2.x and must not be relied on).
3. `bare_extraction` returning `None` is still the "found nothing" signal
   (mapped to `ok:true`, `text:""`).
4. `trafilatura.cfg` keys/defaults still match the packaged config (re-read
   `use_config()['DEFAULT']`); the dedup mirror still makes the size-gated
   dedup check unreachable.
5. Python / OS / `lxml` (libxml2) versions in the table above re-recorded.
