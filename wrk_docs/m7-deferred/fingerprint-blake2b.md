# M7 Deferred ADR — fingerprint column (FNV-1a vs blake2b): masked + shape-checked, not deferred per-fixture

**Affects:** `tests/trafilatura_csv_gate.rs` (M7 Stage 3) and
`tests/trafilatura_xml_gate.rs` (Stage 4, landed); will affect
`tests/trafilatura_xmltei_gate.rs` (Stage 5), which also serialises
`Document.fingerprint`.

## Also affects xml (Stage 4)

On the XML path the same `core.py:480-485` unconditional fingerprint runs, but
the value surfaces as an ATTRIBUTE on the `<doc>` root rather than a CSV column:
`add_xml_meta` (xml.py:178-183) emits every truthy `META_ATTRIBUTES` value as a
`<doc>` attribute, and with `with_metadata=False` the fingerprint is the ONLY
one present (`record_id` defaults to `None`, so no `id=`). Python therefore
always emits `<doc fingerprint="…">`; mdrcel emits a bare `<doc>`.

`tests/trafilatura_xml_gate.rs` reconciles this the same way the csv gate masks
column 2: it SHAPE-CHECKS Python's `fingerprint` attribute (well-formed
lowercase-hex, 1–16 chars) where present, then STRIPS the `fingerprint="…"`
attribute (and its leading space) from the `<doc …>` start tag on BOTH sides
before byte-comparing the rest of the document. The strip is a no-op when a side
has no `<doc>` root (e.g. Python under-extracted), so the gate's allowlist /
deferred triage still routes those fixtures correctly.
**Scope:** ONE column out of 11 on the csv path. Not a whole-fixture defect.
**Verdict:** deliberate, documented divergence in mdrcel's simhash token hash.
The single fingerprint column is **masked + shape-checked** in the gate; the
blake2b-dependency question is **batched for Arthur at M7 close**.

## The divergence

Python's `core.py:481-485` populates `document.fingerprint` for every
non-`txt` output format (csv / xml / xmltei / json-with-metadata), even when
`with_metadata=False`:

```python
if document.raw_text is not None:
    document.fingerprint = content_fingerprint(
        str(document.title) + " " + str(document.raw_text)
    )
```

`content_fingerprint` (`deduplication.py:141-143`) is `Simhash(content).to_hex()`,
and `Simhash._hash` / `create_hash` (`deduplication.py:72-106`) hash each token
with **`blake2b(digest_size=8)`** (`deduplication.py:11, 75`). `to_hex()` is
`hex(self.hash)[2:]` — a lowercase hex string, 1–16 chars (Python's `hex()`
strips leading zeros, so a 64-bit simhash whose top nibbles are zero renders
shorter than 16 chars). On the corpus, observed values are all 16 lowercase
hex chars (e.g. `fbe8c3db32b3b7c2`, `2bb18a7115211379`, `a8f21620ae23dca5`).

mdrcel diverges on **two** axes here, both deliberate:

1. **mdrcel does not compute a fingerprint on the csv path at all.** The csv
   `Document` carrier has no `fingerprint`/`id` slots (M4 Stage 6 deferred),
   so `xmltocsv` (`src/trafilatura/output.rs`) emits the `null` token in the
   fingerprint column. Python emits a real hex value there.
2. **Even if it did compute one, the value could never match.** mdrcel's
   `src/trafilatura/deduplication.rs` (lines ~42-56) substitutes a hand-rolled
   **FNV-1a 64-bit** token hash for Python's `blake2b(digest_size=8)`. This is
   a recorded honest divergence: the simhash *properties* hold (deterministic,
   similar inputs ⇒ low Hamming distance), but the bit positions — and hence
   the hex value — differ from Python's by construction. The M3 Stage 8 brief
   explicitly authorised a hand-rolled FNV-1a / djb2 token hash in lieu of a
   crypto crate, because no mdrcel consumer depends on byte-identity with
   Python's simhash output.

## Why mask the column, not defer the fixture

Every non-empty fixture has a non-`null` Python fingerprint, so a per-fixture
deferral would push **all ~46 substantive fixtures** into the deferred bucket —
making the csv gate vacuous (it would pin nothing). That is strictly worse than
a precise column-level mask: the mask still byte-compares the other 10 columns
(url, id, hostname, title, image, date, **text**, comments, license, pagetype),
i.e. everything that actually exercises the csv serialiser and the body-text
pipeline, on every fixture.

## What the gate does (`tests/trafilatura_csv_gate.rs`)

For each fixture, before byte-comparison:

1. Parse BOTH sides' data row into the 11 tab-separated, QUOTE_MINIMAL-aware
   fields (the `text` cell may embed `\t`, `\r`, `\n` and is csv-quoted, so a
   naive `split('\t')` is wrong — the gate uses a small quote-aware reader).
2. The fingerprint column is **0-based index 2** (column order from
   `xml.py:377-389`: url=0, id=1, **fingerprint=2**, hostname=3, title=4,
   image=5, date=6, text=7, comments=8, license=9, pagetype=10). Verified
   against the vendored `xml.py` source, not assumed.
3. **Shape-check** Python's field-2: assert it is a non-empty lowercase-hex
   string of length 1–16 (a structurally well-formed simhash hex of the same
   *shape* mdrcel would emit if it computed one). This proves Python's value is
   a real fingerprint — the mask is not hiding a structural divergence, only
   the deliberate value difference. mdrcel's field-2 is `null` (its honest
   state) — the mask tolerates that.
4. **Blank field-2 on BOTH sides**, re-serialise the 11 fields, and byte-compare
   the result (after the gate's existing NFC normalisation).

## The batched decision (M7 close, for Arthur)

Faithfully matching Python's fingerprint needs BOTH:

- a **blake2b dependency** (or a vendored blake2b — DEC-3 new-dependency
  discipline ⇒ supervisor sign-off), AND
- wiring `content_fingerprint` into the csv/xml/xmltei `Document` carrier
  (M4 Stage 6 `set_id` / fingerprint work, currently deferred).

Switching the simhash token hash from FNV-1a to blake2b would also change
mdrcel's dedup fingerprints **globally** (blast radius beyond the csv path).
That is a project-level decision, explicitly batched for Arthur at M7 close —
NOT something to land inside a single output-format gate stage. Until then, the
mask + shape-check is the established pattern, and Stages 4-5 (xml/xmltei) will
reuse it for their own fingerprint serialisation.

## Anti-inversion check

This is NOT an allowlist case (Python is not wrong — its fingerprint is a
legitimate value). It is a deliberate mdrcel divergence with a documented
remediation path. The mask is surgical (one column), the shape-check keeps the
gate honest (it still proves Python emits a real fingerprint), and the other 10
columns are pinned byte-for-byte on every fixture.
