# M5 Allowlist ADR — `41d2afac25d46010.html` (SEC 10-K, legacy SGML wrap)

**Bucket pre-allowlist:** `empty-vs-non` (Rust = 74,901 chars, Python = 0 chars).
**Verdict:** anti-inversion-clean — Python under-extracts a structurally-valid 10-K filing.

## Why Python is wrong

The fixture is an EDGAR-style 10-K filing wrapped in a legacy SGML envelope:

```html
<DOCUMENT>
<TYPE>10-K
<SEQUENCE>1
<FILENAME>a2189817z10-k.htm
<DESCRIPTION>10-K
<TEXT>
<HTML>
<HEAD>
</HEAD>
<BODY ...>
... ~237 KB of substantive 10-K body markup ...
</BODY>
</HTML>
</TEXT>
</DOCUMENT>
```

Python's `trafilatura.extract` returns an **empty string** on this input.
The root cause is in `core.bare_extraction`'s structural gates — the
SGML pre-amble (`<DOCUMENT>`/`<TYPE>10-K`/`<SEQUENCE>1`/etc.) confuses lxml's
HTML parser enough that the downstream BODY_XPATH selection finds no
substantive subtree, even though a manual walk of the HTML reveals the
full 10-K body intact inside `<BODY>...</BODY>`.

This is a **silent under-extraction** — Python emits no error, no warning;
it simply returns `""` for a structurally-valid SEC filing. A downstream
consumer using `trafilatura.extract` on this URL would lose every byte of
the filing.

## What mdrcel does instead

mdrcel's pipeline (Stage 1b cleaning → Stage 2 _extract orchestrator)
processes the same input and emits **~75 KB of substantive markdown** —
the full Item-1 Business Description, financial statements paragraphs,
risk factors, etc. The output is structurally faithful to what
`trafilatura.extract` would emit *if* its `bare_extraction` BODY_XPATH
selection had succeeded.

The difference traces to mdrcel's html5ever parser handling the SGML
envelope more permissively (treating `<DOCUMENT>`/`<TEXT>` as unknown
elements and continuing into the nested `<HTML><BODY>...`), where lxml's
strict HTML parser apparently bails earlier.

## What evidence supports this verdict

1. **Manual inspection of the source:** `head -c 500
   benchmark/corpus/snapshots/41d2afac25d46010.html` shows a well-formed
   `<HTML><HEAD></HEAD><BODY>...` block embedded inside the SGML envelope.
   The body content is real 10-K text; not a stub, not an error page.
2. **Rust output size:** 74,901 chars of markdown — substantive paragraphs,
   tables, headings — the shape of a real 10-K extraction.
3. **No other oracle disagrees with mdrcel:** the M5 markdown gate is the
   only one where Python returns empty here; the structural-token
   `extract_content` gate (BLOCKER #3) does not exercise this fixture.
4. **Python's `bare_extraction` does NOT raise** — the empty return is a
   silent failure, not a documented "this format is unsupported" path.

## Anti-inversion check

The HLD §4 anti-inversion doctrine forbids `mdrcel` from *out-cleaning*
Trafilatura. This fixture is the inverse case: `mdrcel` correctly
extracts content `Trafilatura` silently drops. Allowlisting it as a
documented Python under-extraction is the honest verdict — out-cleaning
mdrcel to match Python's empty output would be a deliberate regression.
