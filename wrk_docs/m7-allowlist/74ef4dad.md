# M7 XML Allowlist ADR — `74ef4dadd5f70cb5.html` (1991 WWW page, `<body>` vs `<div>`)

**Gate:** `tests/trafilatura_xml_gate.rs` (`--xml` / `output_format="xml"`).
**Bucket pre-allowlist:** `content-mismatch` (Rust = 1,582 chars, Python = 1,580
chars; first-diff at byte 20: mdrcel `<main>\n    <body>The World Wide Web…`,
Python `<main>\n    <div>The World Wide Web…`).
**Verdict:** anti-inversion-clean — html5ever-vs-lxml tree construction on
malformed pre-HTML5 markup; format-independent.

## Diagnosis

This is the first web page ever published (CERN, 1991): uppercase tags
(`<HEADER>`, `<TITLE>`, `<NEXTID>`, `<BODY>`, `<H1>`), no closing `</P>` tags,
text floating directly inside `<BODY>`. There is no DOCTYPE and the structure
predates HTML 2.0.

mdrcel's html5ever and Python's lxml reconstruct this malformed tree
differently around the document `<body>`: after `docmeta.body.tag = 'main'`
(xml.py:149) renames the extracted root, mdrcel's `<main>` contains a nested
`<body>` element while lxml's contains a `<div>`. The text content is identical
(2-char delta is the `body`/`div` tag-name length difference); the txt, json,
csv, and markdown gates are all GREEN on this fixture.

## Anti-inversion check

Closing this would require matching lxml's exact recovery heuristics for
malformed 1991-era markup inside html5ever — a faithfulness-violating parser
rewrite. A `<body>`→`<div>` rename special-case would be a fragile hack keyed to
this one fixture. Allowlisting documents the trade-off honestly.
