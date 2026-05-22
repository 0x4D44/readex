# M7 XML Allowlist ADR — Workiva XBRL `<main><body>` wrapper drift

**Fixtures:** `340e6571c584979a.html`, `577e61856ca2770d.html`,
`9a1590d0917107a7.html` (Workiva-platform inline-XBRL filings).
**Gate:** `tests/trafilatura_xml_gate.rs` (`--xml` / `output_format="xml"`).
**Bucket pre-allowlist:** `content-mismatch` (first-diff at byte ~20: mdrcel
emits `<main>\n    <body>\n      <p>…`, Python emits `<main>\n    <p>…`).
**Verdict:** anti-inversion-clean — same html5ever-vs-lxml XBRL tree-construction
root cause as the already-allowlisted `683d5643b173c7fd.html`; format-independent.

## Why this is the same divergence as 683d5643

These three are Workiva-platform inline-XBRL documents — direct relatives of the
DFIN XBRL filing already allowlisted at `wrk_docs/m7-allowlist/683d5643.md`. They
declare `xmlns="http://www.w3.org/1999/xhtml"` plus a dozen XBRL namespaces
(`ixt-sec`, `dei`, `us-gaap`, …) and are served as `<?xml …?>`-prologued
XHTML. lxml parses them in lenient-XML mode; mdrcel's html5ever parses them as
HTML5. The two parsers build different trees around the XBRL-namespaced wrapper
elements, and mdrcel retains one extra `<body>`-tagged container that lxml
collapses, so after `docmeta.body.tag = 'main'` (xml.py:149) mdrcel's `<main>`
holds a nested `<body>` where Python's holds the `<p>` directly.

The body **text** is byte-identical: the txt, json, csv, and markdown gates are
all GREEN on these three fixtures (verified). `output_format` only governs
serialization — the divergence is purely the XML tree shape produced upstream by
the parser, exactly as for 683d5643.

## Anti-inversion check

Closing this would require re-implementing lxml's lenient-XML construction of
XBRL-namespaced XHTML inside html5ever (faithfulness-violating), or
special-casing a `<body>`-unwrap that risks corrupting legitimately-nested
content on other fixtures. Neither is a clean win; allowlisting documents the
trade-off honestly. Cross-reference: `wrk_docs/m7-allowlist/683d5643.md`,
`wrk_docs/m5-allowlist/683d5643.md`.
