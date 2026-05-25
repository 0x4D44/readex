"""M10 Phase 2F parser probe — lxml side.

Counterpart to `examples/m10_phase2f_parser_probe.rs`. Same fixtures, same
"find spam containers, dump subtrees, count strays" probe — but parsing via
`lxml.html.fromstring` (the trafilatura entry point).

Usage:
  benchmark/oracles/trafilatura/.venv/Scripts/python.exe \
    examples/m10_phase2f_parser_probe.py <sha> [<sha> ...]

Output: /tmp/m10_phase2f/<sha>_python_pre_clean.txt
"""

from __future__ import annotations

import os
import sys
from pathlib import Path

from lxml import etree, html


SPAM_TAG_INTERESTING = {
    "td", "tr", "th", "source", "fieldset", "rt", "tfoot", "ul",
    "li", "dfn", "cite", "acronym", "tbody",
}


def vis(s: str) -> str:
    return s.replace("\n", "\\n").replace("\t", "\\t").replace("\r", "\\r")


def trunc(s: str, max_chars: int = 60) -> str:
    if len(s) <= max_chars:
        return vis(s)
    return f"{vis(s[:max_chars])}…(+{len(s) - max_chars} chars)"


def attrs_brief(elem) -> str:
    if not elem.attrib:
        return ""
    parts = [f'{k}="{trunc(v, 60)}"' for k, v in elem.attrib.items()]
    return " [" + " ".join(parts) + "]"


def dump_subtree(elem, depth: int, max_depth: int, out: list[str]) -> None:
    if depth > max_depth:
        return
    indent = "  " * depth
    # lxml: leading text of elem is elem.text (BEFORE any child).
    # That belongs in the *parent*'s rendering (since we're called for kids
    # already), so we render child elements + their tails here.
    for child in elem.iterchildren():
        if isinstance(child.tag, str):
            tag = child.tag
            ab = attrs_brief(child)
            out.append(f"{indent}<{tag}>{ab}")
            # element.text (text inside the element, before first child)
            if child.text and child.text.strip():
                inner_indent = "  " * (depth + 1)
                out.append(f'{inner_indent}#text {trunc(child.text.strip(), 80)!r}')
            dump_subtree(child, depth + 1, max_depth, out)
            if child.tail and child.tail.strip():
                out.append(f"{indent}#tail {trunc(child.tail.strip(), 80)!r}")
        elif child.tag is etree.Comment:
            out.append(f"{indent}#comment {trunc(child.text or '', 60)!r}")


def find_spam_containers(root) -> list:
    """All elements whose class contains 'pl_css_ganrao' or style contains
    'display:none' / 'display: none'."""
    found = []
    for elem in root.iter():
        if not isinstance(elem.tag, str):
            continue
        cls = elem.attrib.get("class", "")
        style = elem.attrib.get("style", "")
        if "pl_css_ganrao" in cls or "display:none" in style or "display: none" in style:
            found.append(elem)
    return found


def count_stray_tags(root) -> list[tuple[str, int]]:
    counts: dict[str, int] = {}
    for elem in root.iter():
        if isinstance(elem.tag, str) and elem.tag in SPAM_TAG_INTERESTING:
            counts[elem.tag] = counts.get(elem.tag, 0) + 1
    return sorted(counts.items())


def process(input_arg: str) -> None:
    if os.path.isabs(input_arg):
        path = Path(input_arg)
    else:
        sha = input_arg.removesuffix(".html")
        path = Path("benchmark/fuzz_corpus") / f"{sha}.html"

    sha_label = path.stem[:12]
    print(f"[python] {sha_label} -> {path}", file=sys.stderr)
    raw = path.read_bytes()

    # The trafilatura entry point. core.py:241 calls load_html which calls
    # html.fromstring on bytes. The default parser is lxml's HTMLParser
    # (permissive HTML).
    tree = html.fromstring(raw)

    out: list[str] = []
    out.append("# python (lxml) parser probe (pre-clean)")
    out.append(f"# fixture: {path}")
    out.append(f"# sha (head): {sha_label}")
    out.append("# parse: lxml.html.fromstring (matches trafilatura's load_html entry point)")
    out.append(f"# source bytes: {len(raw)}")
    out.append("")

    # Spam containers.
    spam = find_spam_containers(tree)
    out.append(f"## Spam-candidate containers (class~pl_css_ganrao OR style~display:none): {len(spam)}")
    out.append("")
    for i, container in enumerate(spam):
        tag = container.tag
        ab = attrs_brief(container)
        out.append(f"### Container #{i}: <{tag}>{ab}")
        parent = container.getparent()
        if parent is not None and isinstance(parent.tag, str):
            out.append(f"  (parent: <{parent.tag}>{attrs_brief(parent)})")
        out.append("  --- subtree dump (depth <= 8) ---")
        if container.text and container.text.strip():
            out.append(f'  #text {trunc(container.text.strip(), 80)!r}')
        dump_subtree(container, 1, 8, out)
        out.append("")

    strays = count_stray_tags(tree)
    out.append("## Stray-tag total counts across full document tree")
    if not strays:
        out.append("  (none of {td, tr, th, source, fieldset, rt, tfoot, ul, li, dfn, cite, acronym, tbody})")
    else:
        for tag, n in strays:
            out.append(f"  <{tag}>: {n}")
    out.append("")

    # Body outline.
    body = tree.find(".//body")
    if body is None and tree.tag == "body":
        body = tree
    out.append("## <body> outline (depth <= 5)")
    if body is not None:
        out.append(f"<body>{attrs_brief(body)}")
        if body.text and body.text.strip():
            out.append(f'  #text {trunc(body.text.strip(), 80)!r}')
        dump_subtree(body, 1, 5, out)
    else:
        out.append("  (no body)")

    out_dir = Path("/tmp/m10_phase2f")
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"{sha_label}_python_pre_clean.txt"
    out_path.write_text("\n".join(out) + "\n", encoding="utf-8")
    print(f"[python] wrote {out_path}", file=sys.stderr)


def main() -> int:
    args = sys.argv[1:]
    if not args:
        print(
            "usage: python examples/m10_phase2f_parser_probe.py <sha|path> [...]",
            file=sys.stderr,
        )
        return 2
    rc = 0
    for arg in args:
        try:
            process(arg)
        except Exception as e:
            print(f"ERROR processing {arg}: {e}", file=sys.stderr)
            rc = 1
    return rc


if __name__ == "__main__":
    sys.exit(main())
