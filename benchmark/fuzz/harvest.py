#!/usr/bin/env python
"""M9 Stage 3 — canonical WARC harvester for the fuzz corpus.

Streams ONE Common Crawl WARC over HTTPS (no full download), applies the FROZEN
content filter spec'd in the M9 HLD §5.2, decodes each kept page to UTF-8, and
writes:

    <output_dir>/<sha256>.html         (UTF-8 decoded HTML; one file per page)
    <manifest>                         (JSONL; one object per page, fixed schema)

Manifest schema (HLD §5.2 — EXACTLY these keys):
    {
      "sha":         "<sha256 hex of the file's UTF-8 bytes on disk; also the filename>",
      "source_url":  "<value of WARC-Target-URI>",
      "warc_path":   "<path-only portion of the WARC URL, no host>",
      "warc_offset": <int byte offset of the record's start within the gzipped WARC>,
      "length":      <int byte length of the DECODED UTF-8 HTML file>
    }

The corpus is self-verifying: sha256(<sha>.html bytes) == <sha>, and
len(<sha>.html) == length. For latin-1-fallback pages this means `sha` will
NOT equal the sha of the raw WARC payload — by design, since the file on
disk is the canonical UTF-8 form both oracle and mdrcel will see.

FROZEN filter (HLD §5.2 — _FILTER_FROZEN_AT below).
DO NOT tune this filter during triage. Tweaking it re-baselines the KPI (HLD §8)
and must be a deliberate, separate PR.

  EXCLUDE only non-content pages:
    * decoded UTF-8 size < 1 KB or > 2 MB
    * fewer than 3 `<p` tags AND fewer than 200 chars of visible text
      (both clauses required to drop)
    * obvious binary mislabeled as HTML (control-char ratio > 2 %)
  KEEP:
    * malformed / exotic markup (broken tags, weird whitespace, exotic entities)
    * unusual charset/encoding cases — decode best-effort utf-8 -> latin-1
      (the manifest does NOT record encoding; once we write the file, both
      oracle and mdrcel see identical UTF-8 bytes per the founding-brief
      contract).

Light stratification (target distribution, NOT a strict quota):
    ~30 % non-English (<html lang="..."> heuristic), ~20 % table-heavy
    (>=2 <table> tags). Pages are never rejected for bucket reasons; the
    harvester just reports the achieved mix.

Recommended invocation (uses the oracle venv that already pins warcio/requests):

    "benchmark/oracles/trafilatura/.venv/Scripts/python.exe" \
        benchmark/fuzz/harvest.py [--target-count 1500]

CLI:
    --crawl-id      Common Crawl id (default: auto-discover via collinfo.json;
                    fall back to CC-MAIN-2026-17).
    --warc-index    0-based index into warc.paths.gz (default: 0). Use a
                    different index for the held-out slice in a later stage.
    --target-count  How many pages to keep (default: 1500).
    --output-dir    Where to write <sha>.html files (default: benchmark/fuzz_corpus).
    --manifest      Path to the JSONL manifest (default: benchmark/fuzz/manifest.jsonl).
    --verbose       Print per-record progress to stderr.

Constraints:
    * stdlib + warcio + requests only (no new pip deps).
    * streams the WARC; stops the moment target_count is reached.
    * idempotent: overwrites manifest.jsonl and replaces output_dir contents.
    * deterministic JSONL ordering (the order records emerge from the WARC).
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import shutil
import sys
from typing import Optional, Tuple

import requests
from warcio.archiveiterator import ArchiveIterator

# When this filter changes, the KPI baseline re-baselines too. Bump the date
# and update the M9 journal with the rationale.
_FILTER_FROZEN_AT = "M9 Stage 3 — 2026-05-23"

PROJECT_ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", ".."))
DEFAULT_OUTPUT_DIR = os.path.join(PROJECT_ROOT, "benchmark", "fuzz_corpus")
DEFAULT_MANIFEST = os.path.join(PROJECT_ROOT, "benchmark", "fuzz", "manifest.jsonl")
FALLBACK_CRAWL = "CC-MAIN-2026-17"
CC_BASE = "https://data.commoncrawl.org/"

# Cheap byte-level regexes — applied to the raw HTTP body before decoding.
P_RE = re.compile(rb"<p[\s>]", re.I)
TABLE_RE = re.compile(rb"<table[\s>]", re.I)
TAG_RE = re.compile(rb"<[^>]+>")
LANG_RE = re.compile(rb"""\blang\s*=\s*["']?([a-zA-Z\-]+)""", re.I)


# ---------------------------------------------------------------------------
# Frozen content filter
# ---------------------------------------------------------------------------


def is_content(raw: bytes) -> bool:
    """Return True iff `raw` (undecoded HTTP body) passes the FROZEN filter.

    See module docstring for the literal spec. This function MUST NOT be tuned
    during triage; that would re-baseline the KPI.
    """
    n = len(raw)
    if n < 1024 or n > 2 * 1024 * 1024:
        return False
    if _control_char_ratio(raw) > 0.02:
        return False
    n_p = len(P_RE.findall(raw))
    if n_p < 3 and _visible_text_len(raw) < 200:
        return False
    return True


def _visible_text_len(raw: bytes) -> int:
    stripped = TAG_RE.sub(b" ", raw)
    try:
        txt = stripped.decode("utf-8", "ignore")
    except Exception:  # noqa: BLE001
        txt = stripped.decode("latin-1", "ignore")
    return len(re.sub(r"\s+", " ", txt).strip())


def _control_char_ratio(raw: bytes) -> float:
    sample = raw[:65536]
    if not sample:
        return 1.0
    bad = sum(1 for b in sample if b < 9 or (13 < b < 32))
    return bad / len(sample)


def _decode_best_effort(raw: bytes) -> Optional[str]:
    """Decode utf-8, falling back to latin-1. Returns None on total failure."""
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        pass
    try:
        return raw.decode("latin-1")
    except Exception:  # noqa: BLE001
        return None


# ---------------------------------------------------------------------------
# Common Crawl discovery
# ---------------------------------------------------------------------------


def discover_crawl_id() -> str:
    """Return the newest CC crawl id, or the FALLBACK on any failure."""
    try:
        r = requests.get("https://index.commoncrawl.org/collinfo.json", timeout=60)
        r.raise_for_status()
        info = r.json()
        cid = info[0]["id"]
        print(f"[discover] latest crawl = {cid}", file=sys.stderr)
        return cid
    except Exception as e:  # noqa: BLE001
        print(
            f"[discover] failed ({e!r}); falling back to {FALLBACK_CRAWL}",
            file=sys.stderr,
        )
        return FALLBACK_CRAWL


def warc_path_at(crawl_id: str, index: int) -> str:
    """Return the WARC path at `index` in warc.paths.gz (0-based)."""
    url = f"{CC_BASE}crawl-data/{crawl_id}/warc.paths.gz"
    r = requests.get(url, timeout=120)
    r.raise_for_status()
    data = gzip.decompress(r.content).decode("utf-8")
    paths = [ln.strip() for ln in data.splitlines() if ln.strip()]
    if index < 0 or index >= len(paths):
        raise IndexError(
            f"warc-index {index} out of range (0..{len(paths) - 1}) for {crawl_id}"
        )
    pth = paths[index]
    print(f"[warc.paths] {crawl_id}[{index}] = {pth}", file=sys.stderr)
    return pth


# ---------------------------------------------------------------------------
# Harvest
# ---------------------------------------------------------------------------


def _detect_lang(raw: bytes) -> str:
    m = LANG_RE.search(raw[:8192])
    return m.group(1).decode("ascii", "ignore").lower() if m else ""


def _is_non_english(lang: str) -> bool:
    """True if <html lang> is present AND does not start with 'en'.

    If lang is absent we treat the page as English (the safer default, per the
    HLD spec: 'if absent or starts with en, treat as English')."""
    return bool(lang) and not lang.startswith("en")


def harvest(
    warc_url: str,
    warc_path: str,
    target_count: int,
    output_dir: str,
    manifest_path: str,
    verbose: bool,
) -> dict:
    """Stream `warc_url`, keep up to `target_count` content pages."""
    # Idempotency: wipe and recreate the output dir; truncate the manifest.
    if os.path.isdir(output_dir):
        shutil.rmtree(output_dir)
    os.makedirs(output_dir, exist_ok=True)
    os.makedirs(os.path.dirname(manifest_path) or ".", exist_ok=True)

    scanned = 0
    kept = 0
    bucket_non_english = 0
    bucket_table_heavy = 0
    seen_sha: set[str] = set()
    manifest_lines: list[str] = []

    resp = requests.get(warc_url, stream=True, timeout=300)
    resp.raise_for_status()
    resp.raw.decode_content = False

    iterator = ArchiveIterator(resp.raw)
    try:
        for record in iterator:
            if record.rec_type != "response":
                continue
            scanned += 1

            # Capture the record start offset *before* we consume the body —
            # warcio's iterator.offset is updated AFTER read_to_end completes,
            # so reading it here gives us the current record's start offset
            # in the (gzipped) WARC stream.
            warc_offset = iterator.offset

            ct = (
                record.http_headers.get_header("Content-Type")
                if record.http_headers
                else None
            )
            if not ct or "text/html" not in ct.lower():
                continue

            raw = record.content_stream().read()
            if not is_content(raw):
                continue

            decoded = _decode_best_effort(raw)
            if decoded is None:
                continue

            encoded = decoded.encode("utf-8")
            # `sha` hashes the file-on-disk bytes (canonical UTF-8 form) so the
            # corpus is self-verifying: sha256(<sha>.html) == <sha>. This may
            # differ from sha256(raw) for latin-1-fallback pages — by design.
            sha = hashlib.sha256(encoded).hexdigest()
            if sha in seen_sha:
                continue
            seen_sha.add(sha)

            uri = record.rec_headers.get_header("WARC-Target-URI") or ""

            # Stratification counters (observational only — never reject).
            lang = _detect_lang(raw)
            n_table = len(TABLE_RE.findall(raw))
            if _is_non_english(lang):
                bucket_non_english += 1
            if n_table >= 2:
                bucket_table_heavy += 1

            # Write the decoded UTF-8 HTML file.
            html_path = os.path.join(output_dir, f"{sha}.html")
            with open(html_path, "wb") as fh:
                fh.write(encoded)

            rec = {
                "sha": sha,
                "source_url": uri,
                "warc_path": warc_path,
                "warc_offset": int(warc_offset) if warc_offset is not None else None,
                "length": len(encoded),
            }
            manifest_lines.append(json.dumps(rec, ensure_ascii=False))
            kept += 1

            if verbose and kept % 50 == 0:
                print(
                    f"[progress] scanned={scanned} kept={kept} "
                    f"non_en={bucket_non_english} table>={bucket_table_heavy}",
                    file=sys.stderr,
                )

            if kept >= target_count:
                break
    finally:
        try:
            resp.close()
        except Exception:  # noqa: BLE001
            pass

    with open(manifest_path, "w", encoding="utf-8", newline="\n") as fh:
        if manifest_lines:
            fh.write("\n".join(manifest_lines) + "\n")

    return {
        "scanned_response_records": scanned,
        "kept": kept,
        "non_english_count": bucket_non_english,
        "table_heavy_count": bucket_table_heavy,
        "filter_frozen_at": _FILTER_FROZEN_AT,
    }


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _parse_args(argv: Optional[list[str]] = None) -> argparse.Namespace:
    p = argparse.ArgumentParser(
        description="M9 Stage 3 WARC harvester (FROZEN content filter).",
    )
    p.add_argument("--crawl-id", default=None,
                   help="Common Crawl id (default: auto-discover).")
    p.add_argument("--warc-index", type=int, default=0,
                   help="0-based index into warc.paths.gz (default: 0).")
    p.add_argument("--target-count", type=int, default=1500,
                   help="Pages to keep (default: 1500).")
    p.add_argument("--output-dir", default=DEFAULT_OUTPUT_DIR,
                   help=f"HTML output dir (default: {DEFAULT_OUTPUT_DIR}).")
    p.add_argument("--manifest", default=DEFAULT_MANIFEST,
                   help=f"Manifest JSONL path (default: {DEFAULT_MANIFEST}).")
    p.add_argument("--verbose", action="store_true",
                   help="Print per-50-record progress to stderr.")
    return p.parse_args(argv)


def main(argv: Optional[list[str]] = None) -> int:
    args = _parse_args(argv)

    crawl_id = args.crawl_id or discover_crawl_id()
    try:
        warc_rel = warc_path_at(crawl_id, args.warc_index)
    except Exception as e:  # noqa: BLE001
        if crawl_id != FALLBACK_CRAWL:
            print(
                f"[warc.paths] {crawl_id} failed ({e!r}); falling back to "
                f"{FALLBACK_CRAWL}",
                file=sys.stderr,
            )
            crawl_id = FALLBACK_CRAWL
            warc_rel = warc_path_at(crawl_id, args.warc_index)
        else:
            raise

    warc_url = f"{CC_BASE}{warc_rel}"
    print(f"[stream] {warc_url}", file=sys.stderr)

    summary = harvest(
        warc_url=warc_url,
        warc_path=warc_rel,
        target_count=args.target_count,
        output_dir=os.path.abspath(args.output_dir),
        manifest_path=os.path.abspath(args.manifest),
        verbose=args.verbose,
    )
    summary["crawl_id"] = crawl_id
    summary["warc_path"] = warc_rel
    summary["warc_url"] = warc_url
    summary["output_dir"] = os.path.abspath(args.output_dir)
    summary["manifest"] = os.path.abspath(args.manifest)
    print(json.dumps(summary, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
