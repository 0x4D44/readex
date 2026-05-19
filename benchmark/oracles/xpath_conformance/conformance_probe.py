#!/usr/bin/env python3
"""mdrcel XPath conformance probe — ORACLE (M3 Stage 0b).

Runs a single XPath expression against a single HTML fragment under Python
`lxml` and writes a JSON envelope to stdout for the Rust conformance harness
(`tests/xpath_conformance.rs`) to assert parity against. **NOT** a substitute
for the Trafilatura oracle adapter (`benchmark/oracles/trafilatura/run.py`):
this probe answers exactly one question per invocation — "what does lxml's
.xpath() do on this HTML when evaluated from the <body> context?" — and is
used only by the Stage-0b conformance table.

Invocation:

    python conformance_probe.py --html '<body>...</body>' --xpath './/div'

Or from a file (preferred for HTML containing the shell-fragile characters):

    python conformance_probe.py --html-file path.html --xpath './/div'

Output (single JSON object on stdout, newline-terminated):

    {
      "ok": true,
      "count": 3,
      "tags": ["div", "div", "div"],
      "ids": ["a", "b", "c"],
      "first_text": "the trimmed text-content of the first match, or null",
      "error": null
    }

On any error (parse, lxml import, missing args) emits ok:false and a
non-zero exit. The Rust harness asserts `ok:true` plus `count`/`tags`/
`ids` parity.

Why a separate probe and not the trafilatura oracle?
- The trafilatura oracle runs the full Trafilatura algorithm against an
  entire snapshot file. The conformance harness needs to evaluate an
  arbitrary XPath against a small synthetic HTML fragment, hundreds of
  times. A tightly scoped probe is faster, more diagnostic, and removes
  trafilatura's many transitive dependencies from the test surface (HLD
  M3 §3.1 / DA-M-4: keep the test-time Python dep surface small).
"""

import argparse
import json
import sys


def _emit(obj):
    sys.stdout.write(json.dumps(obj, ensure_ascii=False))
    sys.stdout.write("\n")
    sys.stdout.flush()


def _emit_failure(message, exit_code=1):
    _emit({
        "ok": False,
        "count": None,
        "tags": None,
        "ids": None,
        "first_text": None,
        "error": str(message),
    })
    sys.exit(exit_code)


def _node_tag(node):
    """Local-name of an lxml element (without namespace), lower-cased."""
    tag = getattr(node, "tag", None)
    if tag is None:
        return None
    if isinstance(tag, str):
        # Strip an `{ns}` prefix if present (HTML in lxml does not normally
        # carry namespaces, but be defensive).
        if "}" in tag:
            tag = tag.split("}", 1)[1]
        return tag.lower()
    # Some node types (Comment, ProcessingInstruction) have callable tag.
    try:
        return tag().lower() if callable(tag) else str(tag).lower()
    except Exception:  # noqa: BLE001
        return None


def _node_id(node):
    """`@id` attribute if `node` is an element; None otherwise."""
    try:
        return node.get("id")
    except Exception:  # noqa: BLE001
        return None


def _first_text(nodes):
    """text_content() of the first node, trimmed. None if list is empty."""
    if not nodes:
        return None
    first = nodes[0]
    # lxml elements have .text_content(); strings (from text() axis) are
    # already strings; attribute results (from @ axis) are also strings.
    if isinstance(first, str):
        return first.strip() or first  # preserve a pure-whitespace string
    text_fn = getattr(first, "text_content", None)
    if text_fn is None:
        # _ElementStringResult etc.
        return str(first)
    try:
        return text_fn().strip()
    except Exception:  # noqa: BLE001
        return None


def main():
    parser = argparse.ArgumentParser(description="mdrcel XPath conformance probe.")
    parser.add_argument("--html", help="HTML source (string)")
    parser.add_argument("--html-file", help="Path to a file containing the HTML source")
    parser.add_argument("--xpath", required=True, help="XPath expression to evaluate")
    parser.add_argument(
        "--context",
        choices=["body", "root"],
        default="body",
        help="Evaluation context: body (default) starts from <body>; "
             "root starts from the document root.",
    )
    args = parser.parse_args()

    if args.html is None and args.html_file is None:
        _emit_failure("either --html or --html-file is required")

    if args.html_file is not None:
        try:
            with open(args.html_file, "r", encoding="utf-8") as fh:
                html = fh.read()
        except OSError as exc:
            _emit_failure(f"could not read --html-file: {exc}")
    else:
        html = args.html

    try:
        from lxml import html as lxml_html
    except ImportError as exc:
        _emit_failure(f"lxml not installed: {exc}", exit_code=2)

    try:
        # Use lxml's html parser via document_fromstring: this forces a full
        # <html><body>...</body></html> wrap, so the body is always findable.
        # `html.fromstring` alone would unwrap a body-only fragment and (worse)
        # synthesise a single-root wrapper when multiple body children are
        # present, which would silently change the answer to `.//div`.
        tree = lxml_html.document_fromstring(html)
    except Exception as exc:  # noqa: BLE001
        _emit_failure(f"lxml parse error: {exc}")

    # Choose context. `body` finds the <body>; `root` uses the document root.
    if args.context == "body":
        body = tree.find("body")
        if body is None:
            # document_fromstring always synthesises body; this is defensive.
            ctx = tree
        else:
            ctx = body
    else:
        ctx = tree

    try:
        result = ctx.xpath(args.xpath)
    except Exception as exc:  # noqa: BLE001
        _emit_failure(f"xpath evaluation error: {exc}")

    # Normalise the result: lxml returns a list of elements / strings.
    nodes = list(result) if isinstance(result, list) else [result]

    tags = [_node_tag(n) for n in nodes]
    node_ids = [_node_id(n) for n in nodes]
    first_text = _first_text(nodes)

    _emit({
        "ok": True,
        "count": len(nodes),
        "tags": tags,
        "ids": node_ids,
        "first_text": first_text,
        "error": None,
    })


if __name__ == "__main__":
    main()
