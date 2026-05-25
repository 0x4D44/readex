"""M10 Phase 2F — post-clean probe (Python side).

Runs trafilatura's `tree_cleaning` + `prune_unwanted_nodes(OVERALL_DISCARD_XPATH,
with_backup=True)` on the lxml tree, then reports how many spam containers
survive. Counterpart to the mdrcel example's post-clean dumps.

Usage:
  benchmark/oracles/trafilatura/.venv/Scripts/python.exe \
    examples/m10_phase2f_parser_probe_postclean.py <sha> [...]
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

from lxml import html

from trafilatura.htmlprocessing import prune_unwanted_nodes, tree_cleaning
from trafilatura.settings import Extractor
from trafilatura.xpaths import OVERALL_DISCARD_XPATH


SPAM_INTERESTING = {
    "td", "tr", "th", "source", "fieldset", "rt", "tfoot", "ul",
    "li", "dfn", "cite", "acronym", "tbody",
}


def find_spam(root) -> list:
    out = []
    for e in root.iter():
        if not isinstance(e.tag, str):
            continue
        cls = e.attrib.get("class", "")
        sty = e.attrib.get("style", "")
        if "pl_css_ganrao" in cls or "display:none" in sty or "display: none" in sty:
            out.append(e)
    return out


def count_strays(root) -> list[tuple[str, int]]:
    c: dict[str, int] = {}
    for e in root.iter():
        if isinstance(e.tag, str) and e.tag in SPAM_INTERESTING:
            c[e.tag] = c.get(e.tag, 0) + 1
    return sorted(c.items())


def process(arg: str) -> None:
    if os.path.isabs(arg):
        path = Path(arg)
    else:
        path = Path("benchmark/fuzz_corpus") / f"{arg.removesuffix('.html')}.html"
    sha = path.stem[:12]
    print(f"[python-post] {sha} -> {path}", file=sys.stderr)

    raw = path.read_bytes()
    tree = html.fromstring(raw)

    # Default Extractor (trafilatura.extract defaults).
    options = Extractor()  # defaults

    # Step 1: tree_cleaning (drops MANUALLY_CLEANED, strips MANUALLY_STRIPPED).
    cleaned = tree_cleaning(tree, options)

    out: list[str] = []
    out.append("# python (lxml) post-clean probe")
    out.append(f"# fixture: {path}")
    out.append("# pipeline: tree_cleaning + prune_unwanted_nodes(OVERALL_DISCARD_XPATH, with_backup=True)\n")

    after_clean_spam = find_spam(cleaned)
    out.append(f"## After tree_cleaning: spam containers = {len(after_clean_spam)}")
    for i, c in enumerate(after_clean_spam):
        out.append(f"  #{i} <{c.tag} class={c.attrib.get('class','')!r} style={c.attrib.get('style','')!r}>")
    out.append("")

    after_clean_strays = count_strays(cleaned)
    out.append("## After tree_cleaning: stray-tag counts")
    if not after_clean_strays:
        out.append("  (none)")
    for tag, n in after_clean_strays:
        out.append(f"  <{tag}>: {n}")
    out.append("")

    # Step 2: OVERALL_DISCARD_XPATH with backup.
    pruned = prune_unwanted_nodes(cleaned, OVERALL_DISCARD_XPATH, with_backup=True)
    after_overall_spam = find_spam(pruned)
    out.append(f"## After OVERALL_DISCARD_XPATH(with_backup=True): spam containers = {len(after_overall_spam)}")
    for i, c in enumerate(after_overall_spam):
        out.append(f"  #{i} <{c.tag} class={c.attrib.get('class','')!r} style={c.attrib.get('style','')!r}>")
    out.append("")

    after_overall_strays = count_strays(pruned)
    out.append("## After OVERALL_DISCARD_XPATH(with_backup=True): stray-tag counts")
    if not after_overall_strays:
        out.append("  (none)")
    for tag, n in after_overall_strays:
        out.append(f"  <{tag}>: {n}")

    out_dir = Path("/tmp/m10_phase2f")
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{sha}_python_post_clean.txt"
    out_path.write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"[python-post] wrote {out_path}", file=sys.stderr)


def main() -> int:
    if not sys.argv[1:]:
        print("usage: python m10_phase2f_parser_probe_postclean.py <sha> [...]", file=sys.stderr)
        return 2
    rc = 0
    for arg in sys.argv[1:]:
        try:
            process(arg)
        except Exception as e:
            import traceback
            traceback.print_exc()
            rc = 1
    return rc


if __name__ == "__main__":
    sys.exit(main())
