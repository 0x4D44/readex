#!/usr/bin/env python3
"""mdrcel Trafilatura oracle adapter.

Invocation (HLD 'mdrcel Oracle Adapters' section 3.1):

    python <repo>/benchmark/oracles/trafilatura/run.py <abs.html> [--base-url <URL>]

Writes EXACTLY ONE JSON object to stdout (single write + flush) and nothing
else; all logs/warnings/parser noise go to stderr. The output shape is
governed by ../contract.schema.json; the behavioural contract (the
ok/error/exit tri-state, same-machine determinism, the well-formed `text`
primitive) is HLD section 3.

Three additive oracle modes (mutually exclusive) bypass the JSON envelope
and emit a raw payload directly on stdout for use by per-stage equivalence
gates:

  --convert-tags-only   M3 Stage 1b — post-`convert_tags` tree as XML.
  --extract-content     M3 Stage 3-B — `extract_content` result body as XML.
  --markdown            M5 Stage 2  — `extract(output_format="markdown")`
                                       as raw UTF-8 (None → "").
  --txt                 M7 Stage 1  — `extract(output_format="txt")`.
  --json                M7 Stage 2  — `extract(output_format="json")`.
  --csv                 M7 Stage 3  — `extract(output_format="csv")`.
  --xml                 M7 Stage 4  — `extract(output_format="xml")`.
  --xmltei              M7 Stage 5  — `extract(output_format="xmltei")`.

The five M7 modes mirror --markdown EXACTLY, changing only the
`output_format=` string. Each emits the returned string (None → "") as raw
UTF-8 bytes on stdout (no JSON envelope). The format strings are validated
against trafilatura's `bare_extraction` accepted set (core.py:168 /
`{"csv", "html", "json", "markdown", "txt", "xml", "xmltei"}`); TEI is the
literal string `"xmltei"`. All six format modes share a single
`run_oracle(...)` (the M9 Stage 2 config-lock invariant — one place builds
the `extract` kwargs, every call path goes through it).

M9 Stage 2 also adds an additive batch mode:

  --batch <corpus_dir> <manifest_path>
                        Loop over a JSONL manifest, read each
                        `<corpus_dir>/<sha>.html`, call `run_oracle` for
                        each of {"txt", "markdown", "xml"}, and emit one
                        JSONL line per doc to stdout. The first stdout
                        line is a `{"_header": {...}}` cache-integrity
                        header (`traf_version`, `cfg_sha`, `run_py_sha`,
                        `manifest_sha`) so the Rust diff harness can
                        refuse a stale cache. One Python process / one
                        trafilatura import / many docs (the per-fixture
                        spawn cost is the harvester's bottleneck).

Self-correcting interpreter (HLD section 4, B2 spike resolution): the harness
invokes BARE `python` from PATH and activates no venv. `requirements.txt` only
reproduces under the matching interpreter (lxml ships per-CPython-ABI wheels
=> a different native libxml2 => different `text`). So this script resolves
the venv interpreter relative to __file__ and, if the running interpreter is
not it (compared via resolved real paths), re-runs itself as a CHILD under the
venv interpreter using subprocess, relays the child's stdout/stderr verbatim,
and propagates the child's exit code. It MUST NOT use os.execv (the B2 spike
proved Windows os.execv returns exit 0 regardless of the child and shreds a
spaced --base-url). Re-exec happens AT MOST ONCE, enforced by one tightly
scoped internal env sentinel (the sole, justified exception to 'no env vars').
"""

import hashlib
import json
import os
import sys

# --- The single internal re-exec sentinel (HLD section 4) ------------------
# Set by the parent before spawning the venv child; the child sees it and runs
# the adapter directly, so re-exec can happen AT MOST ONCE (fork-bomb guard).
_REEXEC_SENTINEL = "MDRCEL_TRAFILATURA_REEXECED"


def _emit_json(obj):
    """The SOLE stdout writer: serialize `obj` and emit it as UTF-8 in one
    atomic write + flush, independent of the host console/pipe codepage.

    The harness invokes this adapter with stdout = an OS pipe (Stdio::piped()).
    On Windows, Python then sets ``sys.stdout.encoding`` to the ANSI codepage
    (cp1252), so a text-layer ``sys.stdout.write`` of the contract object
    raises ``UnicodeEncodeError`` on any non-Latin-1 code point real pages
    carry (curly quotes, em-dashes, accents, CJK) and the whole envelope is
    lost. HLD §3.3 mandates UTF-8 stdout regardless of the host codepage, so
    we serialize with ``ensure_ascii=False`` and write the UTF-8 BYTES to the
    raw binary buffer (``sys.stdout.buffer``), bypassing the text encoder
    entirely. Still EXACTLY one write + one flush and nothing else on stdout
    (HLD §3.3 no partial/interleaved output); ``json.dumps`` is deterministic
    for our fixed-shape dict, preserving same-machine byte-identity (§3.5).
    """
    sys.stdout.buffer.write(json.dumps(obj, ensure_ascii=False).encode("utf-8"))
    sys.stdout.buffer.flush()


def _venv_python_path():
    """Absolute path to the committed venv's interpreter, relative to __file__.

    Platform-correct: Windows venvs put python.exe under Scripts/, POSIX under
    bin/. This does NOT chdir and does NOT resolve the snapshot path.
    """
    here = os.path.dirname(os.path.abspath(__file__))
    if os.name == "nt":
        return os.path.join(here, ".venv", "Scripts", "python.exe")
    return os.path.join(here, ".venv", "bin", "python")


def _same_interpreter(a, b):
    """True iff two interpreter paths are the same file by RESOLVED real path.

    realpath + normcase so symlink / Windows short-path (8.3) / case
    differences cannot produce a spurious mismatch (HLD section 4).
    """
    try:
        ra = os.path.normcase(os.path.realpath(a))
        rb = os.path.normcase(os.path.realpath(b))
        return ra == rb
    except OSError:
        return False


def _emit_failure(message, oracle_version=None):
    """Emit the fully field-determined section 3.4 failure envelope and exit !=0.

    `ok:false` and a non-zero exit ALWAYS co-occur (the consumer treats either
    alone as failure). Imports/version reads happen inside the guarded block so
    an ImportError still yields this envelope.
    """
    obj = {
        "contract_version": 1,
        "oracle": "trafilatura",
        "oracle_version": oracle_version,
        "title": None,
        "text": "",
        "html": None,
        "word_count": None,
        "canonical_url": None,
        "language": None,
        "ok": False,
        "error": str(message),
    }
    _emit_json(obj)
    sys.exit(1)


def _reexec_into_venv():
    """If not already running under the venv interpreter, re-run as a child.

    subprocess-proxy, NOT os.execv (HLD section 4 / B2 spike): spawn the venv
    interpreter as a child with the SAME argv, relay stdout/stderr verbatim,
    and propagate the child's exit code. A corrupt/unspawnable venv is a
    CATCHABLE failure that still emits the section 3.4 envelope. A genuinely
    ABSENT venv (fresh clone — git-ignored) emits the section 3.4 envelope
    whose `error` names the one-time bootstrap command.
    """
    if os.environ.get(_REEXEC_SENTINEL) == "1":
        return  # Already the venv child — run directly (at-most-once guard).

    venv_py = _venv_python_path()
    if _same_interpreter(sys.executable, venv_py):
        return  # Bare `python` already IS the venv interpreter.

    if not os.path.isfile(venv_py):
        _emit_failure(
            "Trafilatura venv not bootstrapped (expected interpreter "
            f"missing: {venv_py}). One-time bootstrap (see "
            "benchmark/oracles/trafilatura/README.md):  python -m venv "
            "benchmark/oracles/trafilatura/.venv  &&  "
            "benchmark/oracles/trafilatura/.venv/Scripts/python -m pip "
            "install -r benchmark/oracles/trafilatura/requirements.txt"
        )

    import subprocess

    child_env = dict(os.environ)
    child_env[_REEXEC_SENTINEL] = "1"
    try:
        completed = subprocess.run(
            [venv_py, os.path.abspath(__file__)] + sys.argv[1:],
            env=child_env,
            stdout=sys.stdout,
            stderr=sys.stderr,
            check=False,
        )
    except OSError as exc:
        _emit_failure(
            f"Trafilatura venv interpreter unspawnable ({venv_py}): {exc}"
        )
        return  # unreachable; _emit_failure exits.
    sys.exit(completed.returncode)


def _to_well_formed(text):
    """HLD section 3.3 pinned, never-raising primitive (Python side).

    Replace lone surrogates so `text` is valid UTF-8 before serialization
    (a serde_json reject of an otherwise-valid extraction is a Bug-E2 trap).
    Identity on well-formed input (verified incl. astral/BOM/NUL).
    """
    return text.encode("utf-8", "surrogatepass").decode("utf-8", "replace")


def _word_count(text):
    """Informational only (HLD section 3.2). The consumer recomputes and
    ignores this; a simple whitespace split is sufficient and deterministic."""
    return len(text.split())


# --- M9 Stage 2: single config-locked oracle surface -----------------------
# Every extract-mode branch (--markdown / --txt / --json / --csv / --xml /
# --xmltei) AND the --batch mode go through `run_oracle`. There is EXACTLY
# ONE place in this file that constructs the extract() kwargs (the
# `_LOCKED_EXTRACT_KWARGS` dict below) — that is the anti-drift invariant the
# M9 HLD §5.3 demands. The five M7 per-mode branches used to inline their own
# near-identical extract() call; today they all funnel through here.
#
# The locked posture matches what the per-mode branches had before refactor:
#   with_metadata=False, deduplicate=False, include_comments=False
# `--xmltei` does NOT need a special with_metadata=True override here:
# Trafilatura's Extractor.__init__ (settings.py:144-149) already promotes
# with_metadata to True whenever output_format == "xmltei", so passing
# with_metadata=False is harmless and identical to today's behavior.

def _trafilatura_cfg_path():
    """Absolute path to the committed `trafilatura.cfg` next to this file.

    Single source of truth for both `run_oracle` and the `cfg_sha` header
    hash.
    """
    return os.path.join(
        os.path.dirname(os.path.abspath(__file__)), "trafilatura.cfg"
    )


def _locked_kwargs_descriptor():
    """Stable dict describing the locked extract() posture for hashing.

    The kwargs we pin to `trafi_extract` are not all JSON-serializable
    (`config` is a `configparser.ConfigParser` instance). So for the
    `cfg_sha` header we encode the SHAPE of the posture: the literal
    keyword booleans we pass, PLUS the SHA-256 of the trafilatura.cfg file
    bytes (since that file's contents are part of the posture — change it
    and the oracle's behavior changes, and the Rust diff harness should
    refuse a stale cache).
    """
    with open(_trafilatura_cfg_path(), "rb") as fh:
        cfg_bytes = fh.read()
    return {
        "with_metadata": False,
        "deduplicate": False,
        "include_comments": False,
        "config_file_sha256": hashlib.sha256(cfg_bytes).hexdigest(),
    }


def _cfg_sha():
    """SHA-256 (hex) of the locked-kwargs descriptor.

    Stable JSON encoding: sort_keys=True, separators=(",", ":") — no
    whitespace, deterministic key order, ASCII-only escaping (the
    descriptor is ASCII anyway). Documented here so a future maintainer
    sees why the encoding is what it is.
    """
    payload = json.dumps(
        _locked_kwargs_descriptor(),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=True,
    ).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def _file_sha256(path):
    """SHA-256 (hex) of a file's bytes. Used for run_py_sha / manifest_sha."""
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()


def run_oracle(raw, base_url, output_format):
    """THE single config-locked entry point. Every extract-mode call goes here.

    Returns the trafilatura.extract() string (None → "") for the given
    output_format. Imports trafilatura lazily so the cost is paid once per
    Python process; `use_config` is also called once per call (cheap; it just
    re-reads the committed cfg file). For --batch we'd ideally cache the
    `cfg` object across calls, but `use_config` is fast enough on a 51-doc
    corpus that the simpler "one cfg per call" shape is the right call —
    keeps the surface tiny and avoids a module-global mutable.

    `base_url` may be None (the Python `extract()` signature accepts that;
    it disables URL-derived metadata).
    """
    from trafilatura import extract as trafi_extract
    from trafilatura.settings import use_config

    cfg = use_config(_trafilatura_cfg_path())
    payload = trafi_extract(
        raw,
        url=base_url,
        output_format=output_format,
        with_metadata=False,
        deduplicate=False,
        include_comments=False,
        config=cfg,
    )
    return "" if payload is None else payload


# Output formats the --batch mode emits per doc. Stage 0 showed signal in
# all three; xmltei/json/csv are scaffolding consumed only by per-doc gates
# (and the noisier xmltei carries the documented BST/local-time residual).
_BATCH_OUTPUT_FORMATS = ("txt", "markdown", "xml")


def _emit_batch_line(obj):
    """Single-line JSONL writer for --batch.

    Same UTF-8 byte-discipline as `_emit_json` (raw `sys.stdout.buffer`
    write so non-Latin-1 code points survive a cp1252 console), but
    appends a `\\n` so the result is one valid JSONL record. We flush
    after every record so a partial run is salvageable mid-pipe.
    `ensure_ascii=False` so the payload's UTF-8 round-trips bit-for-bit
    (HLD section 3.3 / M9 HLD §5.2 cache-integrity contract).
    """
    line = json.dumps(obj, ensure_ascii=False)
    sys.stdout.buffer.write(line.encode("utf-8"))
    sys.stdout.buffer.write(b"\n")
    sys.stdout.buffer.flush()


def _run_batch(corpus_dir, manifest_path):
    """--batch mode entry point. See module docstring for the contract.

    Reads the manifest line-by-line (skipping blank lines), for each entry
    reads `<corpus_dir>/<sha>.html` as bytes, calls `run_oracle` for each
    output format in `_BATCH_OUTPUT_FORMATS`, and emits one JSONL line per
    doc. The FIRST stdout line is the cache-integrity header so the Rust
    diff harness can refuse a stale cache before reading any per-doc data.
    """
    if not os.path.isdir(corpus_dir):
        _emit_failure(
            f"--batch corpus_dir not found or not a directory: {corpus_dir!r}"
        )
    if not os.path.isfile(manifest_path):
        _emit_failure(
            f"--batch manifest_path not found or not a regular file: "
            f"{manifest_path!r}"
        )

    # trafilatura version for the header (best-effort; None if unreadable).
    import importlib.metadata

    try:
        traf_version = importlib.metadata.version("trafilatura")
    except Exception:  # noqa: BLE001
        traf_version = None

    header = {
        "_header": {
            "traf_version": traf_version,
            "cfg_sha": _cfg_sha(),
            "run_py_sha": _file_sha256(os.path.abspath(__file__)),
            "manifest_sha": _file_sha256(manifest_path),
        }
    }
    _emit_batch_line(header)

    # Stream the manifest line-by-line. Blank lines are tolerated (harvest
    # tooling may emit a trailing newline). Each non-blank line must parse
    # as a JSON object with a `sha` key; `source_url` is optional (missing
    # / empty / null → pass base_url=None to run_oracle).
    with open(manifest_path, "r", encoding="utf-8") as mfh:
        for line_no, raw_line in enumerate(mfh, start=1):
            line = raw_line.strip()
            if not line:
                continue
            try:
                entry = json.loads(line)
            except json.JSONDecodeError as exc:
                _emit_failure(
                    f"--batch manifest line {line_no} is not valid JSON: "
                    f"{exc}"
                )
            sha = entry.get("sha")
            if not isinstance(sha, str) or not sha:
                _emit_failure(
                    f"--batch manifest line {line_no} missing/invalid 'sha'"
                )
            source_url = entry.get("source_url")
            if not isinstance(source_url, str) or not source_url:
                source_url = None

            html_path = os.path.join(corpus_dir, f"{sha}.html")
            if not os.path.isfile(html_path):
                _emit_failure(
                    f"--batch corpus file missing for sha {sha!r}: "
                    f"{html_path!r}"
                )
            with open(html_path, "rb") as fh:
                raw = fh.read()

            outputs = {}
            for fmt in _BATCH_OUTPUT_FORMATS:
                outputs[fmt] = run_oracle(raw, source_url, fmt)

            _emit_batch_line(
                {"sha": sha, "source_url": source_url, "outputs": outputs}
            )


def _parse_args(argv):
    """Positional <abs.html> plus optional --base-url <URL> plus optional
    --convert-tags-only / --extract-content / --markdown (mutually exclusive).

    No argparse: a fixed CLI; argparse would print to stdout on error.

    Returns (path, base_url, convert_tags_only, extract_content_only,
             markdown_only, extra_format, batch, err).

    `batch` is `None` unless `--batch <corpus_dir> <manifest_path>` is set,
    in which case it is the tuple `(corpus_dir, manifest_path)` and the
    `path` positional is unused (the manifest supplies per-doc shas). The
    --batch mode is mutually exclusive with every other mode flag and with
    --base-url (per-doc source_urls come from the manifest).

    `extra_format` is `None` unless exactly one of the M7 output-format
    flags (--txt / --json / --csv / --xml / --xmltei) is set, in which case
    it carries the matching `output_format=` string. These mirror
    --markdown's full-`extract` shape, differing only in the format string,
    and are mutually exclusive with each other AND with --convert-tags-only
    / --extract-content / --markdown.

    `--convert-tags-only` (M3 Stage 1b additive — HLD §6.2 / Trafilatura-
    equivalence BLOCKER gate): when set, run.py SKIPS the full
    `bare_extraction` pipeline and instead emits the post-`tree_cleaning`
    + post-`convert_tags` tree as canonical XML on stdout (NOT the contract
    JSON envelope). This is the gate's Python-side oracle: Trafilatura's
    own htmlprocessing.tree_cleaning + htmlprocessing.convert_tags run with
    DEFAULT options (matching Rust Options::default()), output serialized
    via lxml etree.tostring(method='xml', encoding='unicode'). The mode is
    a Stage 1b additive surface — the harness's `bare_extraction` contract
    is unchanged (no flag set ⇒ identical behaviour to pre-Stage-1b).

    `--extract-content` (M3 Stage 3-B additive — HLD §6.2 follow-on /
    extract_content equivalence gate): when set, run.py SKIPS the full
    `bare_extraction` pipeline and instead runs Trafilatura's own
    `tree_cleaning` + `convert_tags` + `main_extractor.extract_content`
    against the snapshot and emits the returned `result_body` lxml Element
    as canonical XML on stdout (NOT the contract JSON envelope). This is
    the Stage 3-B gate's Python-side oracle. As with --convert-tags-only,
    Options are at DEFAULT (matching Rust `cleaning::Options::default()`).
    Mutually exclusive with --convert-tags-only — if both are passed, this
    returns an error; the gate only ever needs one.

    `--markdown` (M5 Stage 2 additive — corpus-wide markdown equivalence
    gate): when set, run.py runs the full `trafilatura.extract(raw,
    output_format="markdown")` and emits the returned string (or `""` when
    Python returns `None`) as raw UTF-8 bytes on stdout — NOT the contract
    JSON envelope. This is the markdown corpus-diff gate's Python-side
    oracle. Mutually exclusive with --convert-tags-only and
    --extract-content.
    """
    # The M7 output-format flags map 1:1 onto trafilatura's accepted
    # `output_format=` strings (core.py:168). --xmltei maps to the literal
    # "xmltei" (the string trafilatura validates against for TEI).
    _EXTRA_FORMAT_FLAGS = {
        "--txt": "txt",
        "--json": "json",
        "--csv": "csv",
        "--xml": "xml",
        "--xmltei": "xmltei",
    }
    path = None
    base_url = None
    convert_tags_only = False
    extract_content_only = False
    markdown_only = False
    extra_format = None
    batch = None
    i = 0
    while i < len(argv):
        a = argv[i]
        if a == "--base-url":
            if i + 1 >= len(argv):
                return None, None, False, False, False, None, None, (
                    "--base-url requires a URL argument"
                )
            base_url = argv[i + 1]
            i += 2
            continue
        if a == "--batch":
            if i + 2 >= len(argv):
                return None, None, False, False, False, None, None, (
                    "--batch requires <corpus_dir> <manifest_path>"
                )
            batch = (argv[i + 1], argv[i + 2])
            i += 3
            continue
        if a == "--convert-tags-only":
            convert_tags_only = True
            i += 1
            continue
        if a == "--extract-content":
            extract_content_only = True
            i += 1
            continue
        if a == "--markdown":
            markdown_only = True
            i += 1
            continue
        if a in _EXTRA_FORMAT_FLAGS:
            if extra_format is not None:
                return None, None, False, False, False, None, None, (
                    "--txt / --json / --csv / --xml / --xmltei are "
                    "mutually exclusive"
                )
            extra_format = _EXTRA_FORMAT_FLAGS[a]
            i += 1
            continue
        if path is None:
            path = a
            i += 1
            continue
        return None, None, False, False, False, None, None, (
            f"unexpected extra argument: {a!r}"
        )
    # --batch is mutually exclusive with EVERY other mode flag, with
    # --base-url (per-doc source_urls come from the manifest), and with the
    # <abs.html> positional (the manifest supplies the per-doc shas).
    if batch is not None:
        if (
            convert_tags_only
            or extract_content_only
            or markdown_only
            or extra_format is not None
            or base_url is not None
            or path is not None
        ):
            return None, None, False, False, False, None, None, (
                "--batch is mutually exclusive with --base-url, "
                "--convert-tags-only, --extract-content, --markdown, "
                "--txt, --json, --csv, --xml, --xmltei, and the "
                "<abs.html> positional"
            )
        return (
            None,
            None,
            False,
            False,
            False,
            None,
            batch,
            None,
        )
    if path is None:
        return None, None, False, False, False, None, None, (
            "missing required <abs.html> argument"
        )
    exclusive_count = sum(
        (
            convert_tags_only,
            extract_content_only,
            markdown_only,
            extra_format is not None,
        )
    )
    if exclusive_count > 1:
        return None, None, False, False, False, None, None, (
            "--convert-tags-only, --extract-content, --markdown, --txt, "
            "--json, --csv, --xml, and --xmltei are mutually exclusive"
        )
    return (
        path,
        base_url,
        convert_tags_only,
        extract_content_only,
        markdown_only,
        extra_format,
        None,
        None,
    )


def main():
    # Self-correct the interpreter FIRST (HLD section 4). On return we are
    # guaranteed to be the venv child (or bare python already was the venv).
    _reexec_into_venv()

    # Everything that can fail at the tool/import layer is inside this guard so
    # an ImportError / version-read failure still emits the section 3.4
    # envelope (the Bug-E2 'adapter blew up — catchable' guard).
    oracle_version = None
    try:
        (
            snapshot_path,
            base_url,
            convert_tags_only,
            extract_content_only,
            markdown_only,
            extra_format,
            batch,
            arg_err,
        ) = _parse_args(sys.argv[1:])
        if arg_err is not None:
            _emit_failure(arg_err)

        # Import + version read INSIDE the guard (HLD section 3.4).
        import importlib.metadata

        from trafilatura import bare_extraction
        from trafilatura.settings import use_config

        try:
            oracle_version = importlib.metadata.version("trafilatura")
        except Exception:  # noqa: BLE001 — null on any unreadable version.
            oracle_version = None

        # --- M9 Stage 2: --batch <corpus_dir> <manifest_path> -------------
        # One Python process, one trafilatura import, loops over the
        # manifest JSONL. Emits a single `{"_header": {...}}` line first
        # (cache-integrity contract — see _cfg_sha / _file_sha256 / the
        # M9 HLD §5.3); then one JSONL line per doc:
        #   {"sha": ..., "source_url": ..., "outputs": {"txt": ..., ...}}
        # By invariant, each `outputs[<fmt>]` is byte-identical to running
        # `python run.py <abs> --<fmt>` on the same doc — both go through
        # `run_oracle`.
        if batch is not None:
            _run_batch(batch[0], batch[1])
            sys.exit(0)
        # --- end --batch branch -------------------------------------------

        if not os.path.isfile(snapshot_path):
            _emit_failure(
                f"snapshot file not found or not a regular file: "
                f"{snapshot_path!r}",
                oracle_version,
            )

        # Bytes read RAW and handed to the library unmodified; the library
        # does whatever decoding it does — part of the pinned algorithm
        # (HLD section 3.1, honest framing).
        with open(snapshot_path, "rb") as fh:
            raw = fh.read()

        # --- M3 Stage 1b: --convert-tags-only mode (HLD §6.2) -------------
        # Emit the post-tree_cleaning + post-convert_tags tree as canonical
        # XML directly to stdout (no JSON envelope) and exit. This is the
        # Stage 0c gate's Python-side oracle.
        if convert_tags_only:
            # All imports needed for this branch are documented at use-site
            # so a future maintainer sees why each is here:
            #   - load_html: Trafilatura's HTML parser front door
            #     (trafilatura/utils.py); same parser bare_extraction uses
            #     (core.py:235), so the parsed tree matches gate semantics.
            #   - tree_cleaning, convert_tags: the two Stage 1b functions
            #     under test (trafilatura/htmlprocessing.py).
            #   - Extractor: the options dataclass that controls
            #     tables/images/links/formatting/focus (trafilatura/settings.py).
            #     We instantiate with DEFAULTS (matching mdrcel Rust
            #     cleaning::Options::default()).
            #   - copy.copy: tree_cleaning mutates in place; in
            #     bare_extraction the call is `tree_cleaning(copy(tree),
            #     options)` (core.py:280) so we follow that convention.
            #   - lxml.etree.tostring: canonical XML serialization.
            from copy import copy
            from trafilatura.utils import load_html
            from trafilatura.htmlprocessing import tree_cleaning, convert_tags
            from trafilatura.settings import Extractor
            from lxml.etree import tostring

            tree = load_html(raw)
            if tree is None:
                _emit_failure(
                    "load_html returned None on the snapshot bytes "
                    "(empty/unparsable HTML)",
                    oracle_version,
                )

            options = Extractor(url=base_url)  # All other knobs at default.
            cleaned_tree = tree_cleaning(copy(tree), options)
            cleaned_tree = convert_tags(
                cleaned_tree, options, options.url or None
            )

            # tostring with method='xml' + encoding='unicode' yields a `str`
            # (NOT bytes); pretty_print=False keeps it byte-stable.
            xml_str = tostring(
                cleaned_tree,
                method="xml",
                encoding="unicode",
                pretty_print=False,
            )
            # Single UTF-8 byte write + flush, mirroring _emit_json's
            # stdout discipline (HLD §3.3).
            sys.stdout.buffer.write(xml_str.encode("utf-8"))
            sys.stdout.buffer.flush()
            sys.exit(0)
        # --- end --convert-tags-only branch -------------------------------

        # --- M3 Stage 3-B: --extract-content mode (HLD §6.2 follow-on) ----
        # Run the full Trafilatura tree_cleaning + convert_tags +
        # main_extractor.extract_content pipeline and emit the returned
        # result_body lxml Element as canonical XML directly to stdout (no
        # JSON envelope). This is the Stage 3-B gate's Python-side oracle —
        # mirror image of --convert-tags-only but one stage deeper into the
        # pipeline.
        if extract_content_only:
            # All imports needed for this branch are documented at use-site:
            #   - load_html / tree_cleaning / convert_tags / Extractor:
            #     same rationale as --convert-tags-only above; we MUST run
            #     the same upstream cleaning + tag-conversion so the input
            #     to extract_content matches what bare_extraction would feed
            #     it (trafilatura.core.py:280 onward).
            #   - extract_content: the Stage 2d entry point under test
            #     (trafilatura/main_extractor.py:620). Returns
            #     (result_body, temp_text, len_text); we serialize
            #     result_body (the lxml <body> element).
            #   - copy.copy: tree_cleaning mutates in place — mirror
            #     bare_extraction's `tree_cleaning(copy(tree), options)`.
            #   - lxml.etree.tostring: canonical XML serialization.
            from copy import copy
            from trafilatura.utils import load_html
            from trafilatura.htmlprocessing import tree_cleaning, convert_tags
            from trafilatura.main_extractor import extract_content
            from trafilatura.settings import Extractor
            from lxml.etree import tostring

            tree = load_html(raw)
            if tree is None:
                _emit_failure(
                    "load_html returned None on the snapshot bytes "
                    "(empty/unparsable HTML)",
                    oracle_version,
                )

            options = Extractor(url=base_url)  # All other knobs at default.
            cleaned_tree = tree_cleaning(copy(tree), options)
            cleaned_tree = convert_tags(
                cleaned_tree, options, options.url or None
            )

            # extract_content returns (result_body, temp_text, len_text);
            # we only need the lxml Element for the gate. The text + length
            # would let the gate cross-check our Rust text-extraction too,
            # but Stage 3-B is structural-XML only — text byte-equivalence
            # is Stage 3-C scope.
            result_body, _temp_text, _len_text = extract_content(
                cleaned_tree, options
            )

            xml_str = tostring(
                result_body,
                method="xml",
                encoding="unicode",
                pretty_print=False,
            )
            sys.stdout.buffer.write(xml_str.encode("utf-8"))
            sys.stdout.buffer.flush()
            sys.exit(0)
        # --- end --extract-content branch ---------------------------------

        # --- M5 Stage 2 + M7 Stages 1-5: shared per-doc extract-mode branch
        # All six extract modes (--markdown / --txt / --json / --csv /
        # --xml / --xmltei) share the SINGLE config-locked surface
        # `run_oracle(raw, base_url, output_format)`. The per-mode branches
        # used to inline near-duplicate `trafilatura.extract(...)` calls;
        # M9 Stage 2 collapsed them into one. The returned string (None →
        # "") is emitted as raw UTF-8 bytes on stdout — no JSON envelope —
        # so the Rust gates can strict byte-compare unconditionally.
        #
        # `--xmltei` does NOT need a special with_metadata=True override
        # here: Trafilatura's `Extractor.__init__` (settings.py:144-149)
        # already promotes with_metadata to True whenever output_format ==
        # "xmltei", so the locked posture (`with_metadata=False`) is
        # behavior-identical to the pre-refactor inline call.
        _mode_format = "markdown" if markdown_only else extra_format
        if _mode_format is not None:
            payload = run_oracle(raw, base_url, _mode_format)
            sys.stdout.buffer.write(payload.encode("utf-8"))
            sys.stdout.buffer.flush()
            sys.exit(0)
        # --- end extract-mode branch --------------------------------------

        # Explicit committed config, never ambient (HLD section 4).
        # EXTRACTION_TIMEOUT=0 disables the signal-based timeout: SIGALRM is
        # POSIX-only (unusable on the Windows CI host) and a wall-clock cutoff
        # would make `text` non-deterministic. deduplicate=False is passed
        # explicitly below (Trafilatura's module-level dedup LRU is stateful
        # and non-deterministic).
        cfg = use_config(
            os.path.join(
                os.path.dirname(os.path.abspath(__file__)), "trafilatura.cfg"
            )
        )

        doc = bare_extraction(
            raw,
            url=base_url,
            with_metadata=True,
            deduplicate=False,
            include_comments=False,
            config=cfg,
        )

        if doc is None:
            # 'Found nothing' is a VALID ok:true result, NOT an error — the
            # exact distinction Bug E2 collapsed (HLD section 3.4).
            result = {
                "contract_version": 1,
                "oracle": "trafilatura",
                "oracle_version": oracle_version,
                "title": None,
                "text": "",
                "html": None,
                "word_count": 0,
                "canonical_url": None,
                "language": None,
                "ok": True,
                "error": None,
            }
        else:
            d = doc.as_dict()  # the .as_dict() METHOD (as_dict= param is
            #                    deprecated in Trafilatura 2.x — HLD section 4).
            title = d.get("title")
            raw_text = d.get("text") or ""
            text = _to_well_formed(raw_text)
            language = d.get("language")  # None unless py3langid present
            #                                (null-acceptable per HLD section 4).
            canonical = d.get("url")  # tool's source/canonical URL or None.
            result = {
                "contract_version": 1,
                "oracle": "trafilatura",
                "oracle_version": oracle_version,
                "title": title if title else None,
                "text": text,
                "html": None,  # v1: the body is an lxml element, not a
                #                 string; not serialized (HLD section 4).
                "word_count": _word_count(text),
                "canonical_url": canonical if canonical else None,
                "language": language if language else None,
                "ok": True,
                "error": None,
            }
    except SystemExit:
        raise  # _emit_failure's sys.exit must propagate unaltered.
    except BaseException as exc:  # noqa: BLE001 — any catchable tool/runtime
        #                            error still emits the section 3.4 envelope.
        _emit_failure(f"{type(exc).__name__}: {exc}", oracle_version)
        return  # unreachable; _emit_failure exits.

    # Build the COMPLETE object in memory, then a SINGLE UTF-8 write + flush;
    # never begin emitting until the object is complete (HLD section 3.3 — no
    # partial/interleaved stdout). _emit_json serializes with
    # ensure_ascii=False and writes the UTF-8 bytes to the raw buffer, so the
    # contract holds regardless of the host console/pipe codepage.
    _emit_json(result)
    sys.exit(0)


if __name__ == "__main__":
    main()
