"""M10 Phase 2G — Python (lxml + trafilatura) pipeline-locus tracer.

Companion to `examples/m10_phase2g_pipeline_trace.rs`. Same fixture, same
boundaries — instrumented through Python's actual call chain so we can
compare what Python does at each stage versus what mdrcel does.

Usage:
  benchmark/oracles/trafilatura/.venv/Scripts/python.exe \\
    examples/m10_phase2g_pipeline_trace.py <sha-or-path> [<sha-or-path> ...]

Output:
  /tmp/m10_phase2g/<sha>_python_trace.txt
  /tmp/m10_phase2g/<sha>_python_extract_xml.xml
"""

from __future__ import annotations

import os
import sys
from copy import deepcopy
from pathlib import Path

from lxml import etree, html
from lxml.etree import tostring
from lxml.html import HtmlElement

from trafilatura import extract
from trafilatura.core import load_html
from trafilatura.htmlprocessing import (
    convert_tags,
    prune_unwanted_nodes,
    tree_cleaning,
)
from trafilatura.external import compare_extraction, try_readability
from trafilatura.main_extractor import (
    extract_content,
    _extract,
    prune_unwanted_sections,
)
from trafilatura.settings import Extractor, DEFAULT_CONFIG
from trafilatura.xpaths import BODY_XPATH, OVERALL_DISCARD_XPATH


INTERESTING = {
    "td", "tr", "th", "source", "fieldset", "rt", "tfoot",
    "ul", "li", "dfn", "cite", "acronym", "tbody",
}


def spam_in(root) -> list:
    """All elements (self+descendants) whose class contains pl_css_ganrao
    or whose style contains display:none / display: none."""
    found = []
    for elem in root.iter():
        if not isinstance(elem.tag, str):
            continue
        cls = elem.attrib.get("class", "")
        sty = elem.attrib.get("style", "")
        if (
            "pl_css_ganrao" in cls
            or "display:none" in sty
            or "display: none" in sty
        ):
            found.append(elem)
    return found


def count_strays(root) -> dict:
    counts = {}
    for elem in root.iter():
        if not isinstance(elem.tag, str):
            continue
        if elem.tag in INTERESTING:
            counts[elem.tag] = counts.get(elem.tag, 0) + 1
    return dict(sorted(counts.items()))


def id_class(e):
    return f'id={e.attrib.get("id", "")!r} class={e.attrib.get("class", "")!r}'


def report_stage(out: list, stage: str, root, scope_desc: str):
    matches = spam_in(root)
    strays = count_strays(root)
    out.append(f"\n## {stage}")
    out.append(f"  scope: {scope_desc}")
    out.append(f"  spam containers reachable: {len(matches)}")
    for i, m in enumerate(matches):
        out.append(f"    [{i}] <{m.tag}> {id_class(m)}")
    out.append("  stray-tag counts: " + (
        "(none)" if not strays
        else ", ".join(f"{k}={v}" for k, v in strays.items())
    ))


def locate_body(tree):
    for i, expr in enumerate(BODY_XPATH):
        matches = expr(tree)
        first = next((s for s in matches if s is not None), None)
        if first is not None:
            snippet = str(expr).split("\n")[0][:80]
            return (i, snippet, first)
    return None


def process(input_arg: str) -> None:
    if os.path.isabs(input_arg):
        path = Path(input_arg)
    else:
        sha = input_arg.removesuffix(".html")
        path = Path("benchmark/fuzz_corpus") / f"{sha}.html"
    sha_label = path.stem[:12]
    print(f"[py-trace] {sha_label} -> {path}", file=sys.stderr)

    raw = path.read_bytes()
    out: list[str] = []
    out.append(f"# M10 Phase 2G — Python (lxml + trafilatura) pipeline trace")
    out.append(f"# fixture: {path}")
    out.append(f"# sha: {sha_label}")
    out.append(f"# html bytes: {len(raw)}")

    # ----------------------------------------------------------------
    # S0 — load_html (matches core.py:235)
    # ----------------------------------------------------------------
    tree = load_html(raw)
    if tree is None:
        out.append("\n## S0 load_html — returned None; bailing")
        Path("/tmp/m10_phase2g").mkdir(parents=True, exist_ok=True)
        Path(f"/tmp/m10_phase2g/{sha_label}_python_trace.txt").write_text(
            "\n".join(out) + "\n", encoding="utf-8"
        )
        return
    report_stage(out, "S0 load_html (raw lxml tree)", tree, "whole document")

    # The Extractor options used by extract() default path.
    options = Extractor(
        config=DEFAULT_CONFIG,
        output_format="xml",
    )

    # ----------------------------------------------------------------
    # S1 — tree_cleaning (htmlprocessing.py:48) on copy(tree)
    # ----------------------------------------------------------------
    # core.py:280 — cleaned_tree = tree_cleaning(copy(tree), options)
    from copy import copy
    cleaned_tree = tree_cleaning(copy(tree), options)
    report_stage(out, "S1 post-tree_cleaning", cleaned_tree, "cleaned tree (whole)")

    # ----------------------------------------------------------------
    # S2 — cleaned_tree_backup = copy(cleaned_tree)  (core.py:281)
    # ----------------------------------------------------------------
    cleaned_tree_backup = copy(cleaned_tree)
    report_stage(
        out,
        "S2 cleaned_tree_backup = copy(cleaned_tree)",
        cleaned_tree_backup,
        "snapshot of cleaned tree (whole)",
    )

    # ----------------------------------------------------------------
    # S3 — convert_tags (core.py:284)
    # ----------------------------------------------------------------
    cleaned_tree_post_convert = convert_tags(cleaned_tree, options, options.url)
    report_stage(
        out,
        "S3 post-convert_tags",
        cleaned_tree_post_convert,
        "cleaned tree after convert_tags",
    )

    # ----------------------------------------------------------------
    # S4 — BODY_XPATH selection (main_extractor.py:578)
    # ----------------------------------------------------------------
    loc = locate_body(cleaned_tree_post_convert)
    if loc is None:
        out.append("\n## S4 BODY_XPATH selection")
        out.append("  NO MATCH for any of the 5 BODY_XPATH expressions")
    else:
        i, snippet, subtree = loc
        out.append(f"\n## S4 BODY_XPATH selection")
        out.append(f"  matched expression index: {i}")
        out.append(f"  expr snippet: {snippet}…")
        out.append(f"  selected subtree: <{subtree.tag}> {id_class(subtree)}")
        report_stage(
            out,
            "S4 BODY_XPATH subtree (descendant-or-self scope)",
            subtree,
            "subtree _extract's prune_unwanted_sections runs on",
        )

        # S4b prune_unwanted_sections (main_extractor.py:584)
        # Reproduce the potential_tags init from _extract:
        from trafilatura.settings import TAG_CATALOG
        potential_tags = set(TAG_CATALOG)
        if options.tables is True:
            potential_tags.update(["table", "td", "th", "tr"])
        if options.images is True:
            potential_tags.add("graphic")
        if options.links is True:
            potential_tags.add("ref")
        pruned = prune_unwanted_sections(deepcopy(subtree), potential_tags, options)
        report_stage(
            out,
            "S4b post-prune_unwanted_sections on BODY_XPATH subtree",
            pruned,
            "pruned subtree (OVERALL_DISCARD + others)",
        )

    # ----------------------------------------------------------------
    # S5/S6 — extract_content end-to-end (main_extractor.py:620)
    # Fresh copy so we don't share mutations with above.
    # ----------------------------------------------------------------
    cleaned_for_extract = tree_cleaning(copy(tree), options)
    cleaned_for_extract = convert_tags(cleaned_for_extract, options, options.url)
    own_body, own_text, own_len = extract_content(cleaned_for_extract, options)
    report_stage(
        out,
        "S5/S6 extract_content -> own_body",
        own_body,
        "own-arm result body",
    )
    out.append(f"  own_text chars: {own_len}")
    out.append(f"  own_text first 200 chars: {own_text[:200]!r}")

    # ----------------------------------------------------------------
    # S7 — compare_extraction (external.py:45)
    #
    # core.py:296-298: trafilatura_sequence(cleaned_tree, cleaned_tree_backup, tree, options)
    # external.py:45:  compare_extraction(tree, backup_tree, body, text, ...)
    # where 'tree' = cleaned_tree_backup, 'backup_tree' = deepcopy(tree_backup) = deepcopy(tree)
    # ----------------------------------------------------------------
    # Re-take the snapshot taken at core.py:281 from the post-extract cleaned_tree
    # (it would already be there but we cleaned fresh so let's just use the snapshot.
    # The 'tree' arg is just for justext, 'backup_tree' is the readability input.)
    readability_input = deepcopy(tree)  # corresponds to `deepcopy(tree_backup)`
    win_body, win_text, win_len = compare_extraction(
        cleaned_tree_backup,
        readability_input,
        own_body,
        own_text,
        own_len,
        options,
    )
    report_stage(
        out,
        "S7 compare_extraction -> winning_body",
        win_body,
        "cascade-winning body",
    )
    out.append(f"  winning_text chars: {win_len}")
    out.append(f"  winning_text first 200 chars: {win_text[:200]!r}")

    # ----------------------------------------------------------------
    # S7b — call try_readability directly on readability_input to see what
    # the readability arm itself produces (this is what mdrcel's
    # try_readability tries to mirror).
    # ----------------------------------------------------------------
    readability_only = try_readability(deepcopy(tree))
    if readability_only is not None and len(readability_only) > 0:
        report_stage(
            out,
            "S7b try_readability(deepcopy(tree)) — readability arm in isolation",
            readability_only,
            "readability-arm raw output",
        )
        ro_text = tostring(readability_only, method="text", encoding="utf-8").decode("utf-8")
        out.append(f"  readability_text chars: {len(ro_text)}")
        out.append(f"  readability_text first 200 chars: {ro_text.strip()[:200]!r}")
        # Serialised HTML output too
        ro_html = tostring(readability_only, encoding="utf-8").decode("utf-8")
        out.append(f"  readability_html length: {len(ro_html)}")
        out.append(f"  readability_html first 400 chars: {ro_html[:400]!r}")
    else:
        out.append("\n## S7b try_readability returned None / empty")

    # ----------------------------------------------------------------
    # S8 — search for leaked literal substrings in the serialised
    # winning body.
    # ----------------------------------------------------------------
    serialized = tostring(win_body, encoding="utf-8").decode("utf-8")
    out.append("\n## S8 serialised winning body (substring leak markers)")
    markers = [
        "pl_css_ganrao", "display: none", "display:none",
        "<rt ", "<td ", "<tr ", "<th ", "<source ", "<fieldset ",
        "<acronym ", "<dfn ", "<cite ", "<tbody ", "<tfoot ",
    ]
    for m in markers:
        n = serialized.count(m)
        if n > 0:
            out.append(f"  {m!r}: {n}")
    out.append(f"  serialized length: {len(serialized)}")
    out.append(f"  serialized first 400 chars: {serialized[:400]!r}")

    # ----------------------------------------------------------------
    # S9 — full extract() end-to-end (xml output)
    # ----------------------------------------------------------------
    xml_out = extract(raw, output_format="xml")
    out.append("\n## S9 extract(output_format='xml') end-to-end")
    if xml_out is None:
        out.append("  None")
    else:
        out.append(f"  ok, {len(xml_out)} bytes")
        for m in markers:
            n = xml_out.count(m)
            if n > 0:
                out.append(f"  {m!r}: {n}")
        Path("/tmp/m10_phase2g").mkdir(parents=True, exist_ok=True)
        Path(f"/tmp/m10_phase2g/{sha_label}_python_extract.xml").write_text(
            xml_out, encoding="utf-8"
        )

    Path("/tmp/m10_phase2g").mkdir(parents=True, exist_ok=True)
    Path(f"/tmp/m10_phase2g/{sha_label}_python_trace.txt").write_text(
        "\n".join(out) + "\n", encoding="utf-8"
    )
    print(f"[py-trace] wrote /tmp/m10_phase2g/{sha_label}_python_trace.txt", file=sys.stderr)


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print("usage: python examples/m10_phase2g_pipeline_trace.py <sha|path> [...]", file=sys.stderr)
        return 2
    rc = 0
    for a in args:
        try:
            process(a)
        except Exception as e:
            import traceback
            print(f"ERROR processing {a}: {e}", file=sys.stderr)
            traceback.print_exc(file=sys.stderr)
            rc = 1
    return rc


if __name__ == "__main__":
    sys.exit(main())
