#!/usr/bin/env python3
"""Committed dev-time self-test for the Trafilatura oracle adapter.

One command (see README). NOT a runtime dependency of run.py. Uses the
dev-time-only `jsonschema` validator installed into the same git-ignored
.venv (HLD section 6); not in the adapter's pinned runtime closure.

Implements the HLD section 7 five-step contract proof against
../contract.schema.json, plus a "found nothing != error" Bug-E2 guard. This
is the 'oracle of the oracles' for the Trafilatura side: it feeds known
inputs and asserts schema validity, the ok/error/exit tri-state, non-empty
text on a content page, the section 3.3 well-formed primitive, and same-machine
run-twice byte-identity.

Exit 0 iff every step passes; non-zero with a stderr reason otherwise.
"""

import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.abspath(os.path.join(HERE, "..", "..", ".."))
RUN_PY = os.path.join(HERE, "run.py")
SCHEMA = os.path.join(HERE, "..", "contract.schema.json")
FIXTURES = os.path.join(HERE, "..", "fixtures")
ARTICLE = os.path.join(FIXTURES, "article.html")
EMPTY = os.path.join(FIXTURES, "empty.html")
UNICODE = os.path.join(FIXTURES, "unicode-article.html")
NONEXISTENT = os.path.join(FIXTURES, "this-path-does-not-exist.html")

# Exact non-Latin-1 code points the unicode-article fixture's MAIN content
# carries; cp1252 (the default Windows console/pipe codepage) cannot encode
# any of them. The HLD §3.3 contract mandates UTF-8 stdout independent of the
# host codepage; this is the permanent regression guard for that defect.
_UNICODE_REQUIRED = ("“", "”", "—", "Beyoncé", "µ", "日本語")


def _fail(step, msg):
    sys.stderr.write(f"[selftest.py] FAIL ({step}): {msg}\n")
    sys.exit(1)


def _invoke(path, *extra_args):
    """Invoke run.py exactly as the harness does: bare `python` + script +
    absolute path, plus any extra CLI args verbatim (e.g.
    ``"--base-url", "..."``). Returns (returncode, stdout_bytes,
    stderr_text)."""
    proc = subprocess.run(
        [sys.executable, RUN_PY, path, *extra_args],
        capture_output=True,
        check=False,
    )
    return proc.returncode, proc.stdout, proc.stderr.decode("utf-8", "replace")


def _load_schema():
    with open(SCHEMA, "r", encoding="utf-8") as fh:
        return json.load(fh)


def _validate(step, schema, obj):
    import jsonschema

    try:
        jsonschema.validate(instance=obj, schema=schema)
    except jsonschema.ValidationError as exc:  # type: ignore[attr-defined]
        _fail(step, f"schema validation failed: {exc.message}")


def _parse_stdout(step, stdout_bytes):
    try:
        return json.loads(stdout_bytes.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        _fail(step, f"stdout is not one valid UTF-8 JSON object: {exc}")


def _has_surrogate(text):
    return any(0xD800 <= ord(ch) <= 0xDFFF for ch in text)


def main():
    schema = _load_schema()

    # --- Step 1: article.html -> schema-valid, ok:true, exit 0, substantive
    #     text, no <head>/<script>/<style> leak, one clean object despite the
    #     fixture's deliberately malformed CSS rule. -----------------------
    rc, out, _err = _invoke(ARTICLE)
    if rc != 0:
        _fail("step1", f"expected exit 0 on article.html, got {rc}")
    obj = _parse_stdout("step1", out)
    _validate("step1", schema, obj)
    if obj.get("ok") is not True or obj.get("error") is not None:
        _fail("step1", f"expected ok:true/error:null, got {obj!r}")
    if not isinstance(obj.get("text"), str) or len(obj["text"].strip()) < 80:
        _fail("step1", "expected substantive `text` on the article fixture")
    leaked = "SCRIPT_SHOULD_NOT_APPEAR_IN_TEXT"
    if leaked in obj["text"]:
        _fail("step1", "<script> content leaked into extracted `text`")
    if "color: #123456" in obj["text"] or "@media screen" in obj["text"]:
        _fail("step1", "<style> content leaked into extracted `text`")
    if obj.get("contract_version") != 1:
        _fail("step1", f"contract_version must be 1, got {obj.get('contract_version')!r}")

    # --- Step 2: empty.html -> schema-valid, ok:true, text:"", exit 0
    #     (the Bug E2 'found nothing' guard). --------------------------------
    rc, out, _err = _invoke(EMPTY)
    if rc != 0:
        _fail("step2", f"expected exit 0 on empty.html, got {rc}")
    obj = _parse_stdout("step2", out)
    _validate("step2", schema, obj)
    if obj.get("ok") is not True or obj.get("error") is not None:
        _fail("step2", f"'found nothing' must be ok:true, got {obj!r}")
    if obj.get("text") != "":
        _fail("step2", f"expected text:'' on empty.html, got {obj.get('text')!r}")

    # --- Step 3: nonexistent path -> schema-valid failure envelope, fully
    #     field-determined per section 3.4: ok:false, error set, exit != 0
    #     (the Bug E2 'blew up — catchable' guard). --------------------------
    rc, out, _err = _invoke(NONEXISTENT)
    if rc == 0:
        _fail("step3", "expected non-zero exit on a nonexistent path")
    obj = _parse_stdout("step3", out)
    _validate("step3", schema, obj)
    if obj.get("ok") is not False or not obj.get("error"):
        _fail("step3", f"expected ok:false + error set, got {obj!r}")
    if obj.get("text") != "" or obj.get("word_count") is not None:
        _fail("step3", "failure envelope must be fully field-determined")
    if obj.get("contract_version") != 1 or obj.get("oracle") != "trafilatura":
        _fail("step3", "failure envelope must set contract_version & oracle")

    # --- Step 4: section 3.3 primitive guard. Construct a lone surrogate via a
    #     SOURCE-LEVEL escape (not a UTF-8 fixture file) and pass it through
    #     the same primitive run.py uses; assert the post-primitive text is
    #     schema-valid and contains NO code point in U+D800..U+DFFF. ---------
    lone = "before\ud800after"  # lone high surrogate via source escape.
    if not _has_surrogate(lone):
        _fail("step4", "test setup error: input had no lone surrogate")
    # Mirror run.py's pinned primitive exactly.
    cleaned = lone.encode("utf-8", "surrogatepass").decode("utf-8", "replace")
    if _has_surrogate(cleaned):
        _fail("step4", "post-primitive text still contains a lone surrogate")
    probe = {
        "contract_version": 1,
        "oracle": "trafilatura",
        "oracle_version": "2.0.0",
        "title": None,
        "text": cleaned,
        "html": None,
        "word_count": len(cleaned.split()),
        "canonical_url": None,
        "language": None,
        "ok": True,
        "error": None,
    }
    _validate("step4", schema, probe)
    # Round-trips through serde-compatible UTF-8 JSON without raising.
    json.loads(json.dumps(probe, ensure_ascii=False).encode("utf-8").decode("utf-8"))

    # --- Step 5: re-run step 1 and diff — byte-identical (same-machine
    #     determinism, section 3.5). -----------------------------------------
    _rc1, out1, _e1 = _invoke(ARTICLE)
    _rc2, out2, _e2 = _invoke(ARTICLE)
    if out1 != out2:
        _fail("step5", "two runs on article.html were NOT byte-identical")

    # --- Step 6: non-Latin-1 / Windows-codepage stdout regression guard.
    #     run.py writes its single JSON object to a PIPE (exactly how the Rust
    #     harness invokes it via Stdio::piped()); on Windows Python then sets
    #     sys.stdout.encoding to the ANSI codepage (cp1252), NOT UTF-8. The
    #     HLD §3.3 contract mandates UTF-8 stdout regardless of the host
    #     codepage. This fixture's MAIN content carries curly quotes, an
    #     em-dash, an accented name (Beyoncé), the micro sign (µ) and a CJK
    #     phrase (日本語) — every one cp1252-unencodable — so a codepage-
    #     dependent writer raises UnicodeEncodeError mid-emit and the whole
    #     contract envelope is lost (the corpus-wide oracle_error defect).
    #     Assert: exit 0, schema-valid, ok:true / error:null, the stdout
    #     bytes decode as UTF-8, and the decoded `text` contains EVERY
    #     required code point verbatim. `_invoke`'s capture_output=True gives
    #     run.py a pipe, faithfully reproducing the harness condition. -------
    rc, out, _err = _invoke(UNICODE)
    if rc != 0:
        _fail(
            "step6",
            f"expected exit 0 on unicode-article.html, got {rc} "
            f"(UTF-8 stdout contract violated; stderr below)\n{_err}",
        )
    # MUST decode as UTF-8 with strict errors (no replacement) — a cp1252 or
    # mojibake stream fails here, which is the precise defect being guarded.
    try:
        decoded = out.decode("utf-8")
    except UnicodeDecodeError as exc:
        _fail("step6", f"stdout is not valid UTF-8: {exc}")
    obj = _parse_stdout("step6", out)
    _validate("step6", schema, obj)
    if obj.get("ok") is not True or obj.get("error") is not None:
        _fail("step6", f"expected ok:true/error:null, got {obj!r}")
    if obj.get("contract_version") != 1:
        _fail(
            "step6",
            f"contract_version must be 1, got {obj.get('contract_version')!r}",
        )
    text = obj.get("text")
    if not isinstance(text, str) or len(text.strip()) < 80:
        _fail("step6", "expected substantive `text` on the unicode fixture")
    missing = [ch for ch in _UNICODE_REQUIRED if ch not in text]
    if missing:
        _fail(
            "step6",
            "extracted `text` is missing required non-Latin-1 code points "
            f"{missing!r} (U+%s) — UTF-8 stdout contract violated"
            % ", U+".join(f"{ord(c):04X}" for c in missing),
        )
    if "SCRIPT_SHOULD_NOT_APPEAR_IN_TEXT" in text:
        _fail("step6", "<script> content leaked into extracted `text`")
    # The decoded-bytes path and the json-parsed path must agree, proving the
    # code points survive the actual stdout byte stream (not just in-memory).
    if json.loads(decoded).get("text") != text:
        _fail("step6", "decoded-bytes `text` disagrees with parsed `text`")

    # --- Step 7: --base-url contract surface. --base-url is AUXILIARY
    #     (canonical/relative-link resolution only), NOT essential to
    #     main-content extraction. Trafilatura already tolerates a malformed
    #     url= and still extracts; this LOCKS that in as a permanent
    #     regression guard and keeps the two adapters symmetric on this
    #     surface (the sibling Readability adapter degrades a malformed
    #     --base-url to the no-base path rather than hard-failing). Assert all
    #     three contract cases: (a) VALID --base-url -> ok:true,
    #     byte-deterministic; (b) ABSENT --base-url -> ok:true (unchanged);
    #     (c) STRUCTURALLY-INVALID --base-url -> ok:true WITH substantive
    #     extracted text (graceful degrade, NOT a hard failure) AND
    #     byte-deterministic. run.py is NOT modified — it is already
    #     tolerant; (c) is expected to pass as-is. ---------------------------
    # (a) valid --base-url.
    rc, out_va, _err = _invoke(ARTICLE, "--base-url", "https://example.com/page")
    if rc != 0:
        _fail("step7", f"valid --base-url: expected exit 0, got {rc}")
    obj = _parse_stdout("step7", out_va)
    _validate("step7", schema, obj)
    if obj.get("ok") is not True or obj.get("error") is not None:
        _fail("step7", f"valid --base-url: expected ok:true, got {obj!r}")
    if not isinstance(obj.get("text"), str) or len(obj["text"].strip()) < 80:
        _fail("step7", "valid --base-url: expected substantive `text`")
    _rc, out_vb, _e = _invoke(ARTICLE, "--base-url", "https://example.com/page")
    if out_va != out_vb:
        _fail("step7", "valid --base-url: two runs were NOT byte-identical")

    # (b) absent --base-url (unchanged no-base behaviour).
    rc, out_ab, _err = _invoke(ARTICLE)
    if rc != 0:
        _fail("step7", f"absent --base-url: expected exit 0, got {rc}")
    obj = _parse_stdout("step7", out_ab)
    _validate("step7", schema, obj)
    if obj.get("ok") is not True or obj.get("error") is not None:
        _fail("step7", f"absent --base-url: expected ok:true, got {obj!r}")

    # (c) structurally-invalid --base-url: MUST still extract (ok:true, text
    #     present), MUST NOT escalate to the failure envelope, MUST be
    #     byte-deterministic. Trafilatura is already tolerant; lock it in.
    for bad in (
        "//x",
        "//proto-relative",
        "http://[invalid",
        "::::not a url::::",
        "http://a b c/x",
    ):
        rc, out_ia, err = _invoke(ARTICLE, "--base-url", bad)
        if rc != 0:
            _fail(
                "step7",
                f"invalid --base-url {bad!r}: expected exit 0 (graceful "
                f"degrade), got {rc}; stderr below\n{err}",
            )
        obj = _parse_stdout("step7", out_ia)
        _validate("step7", schema, obj)
        if obj.get("ok") is not True or obj.get("error") is not None:
            _fail(
                "step7",
                f"invalid --base-url {bad!r}: a malformed base is AUXILIARY "
                f"and must degrade gracefully, not hard-fail; got {obj!r}",
            )
        if not isinstance(obj.get("text"), str) or len(obj["text"].strip()) < 80:
            _fail(
                "step7",
                f"invalid --base-url {bad!r}: expected substantive `text` "
                "(extraction must still occur)",
            )
        _rc, out_ib, _e = _invoke(ARTICLE, "--base-url", bad)
        if out_ia != out_ib:
            _fail(
                "step7",
                f"invalid --base-url {bad!r}: two runs were NOT "
                "byte-identical",
            )

    sys.stderr.write(
        "[selftest.py] PASS — all 7 steps (schema-valid, tri-state, "
        "non-empty text, section 3.3 primitive, byte-identical re-run, "
        "non-Latin-1 UTF-8 stdout regression guard, --base-url "
        "valid/absent/invalid graceful-degrade).\n"
    )
    sys.exit(0)


if __name__ == "__main__":
    main()
