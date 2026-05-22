# M7 TXT Allowlist ADR — `41d2afac25d46010.html` (EDGAR SEC 10-K, Python returns empty)

**Gate:** `tests/trafilatura_txt_gate.rs` (`--txt` / `output_format="txt"`).
**Bucket pre-allowlist:** `empty-vs-non` (Rust = 74,475 chars, Python = 0 chars).
**Verdict:** anti-inversion-clean — identical root cause to the markdown gate, format-independent.

## Why this is the same divergence as markdown

Python's `bare_extraction` returns `None` (→ `""`) on this structurally-valid
legacy-SGML-wrapped EDGAR filing on BOTH the markdown and txt paths. The
divergence is in the **extraction/selection** stage (the cascade decides there
is nothing to extract), which runs identically regardless of `output_format` —
the format string only governs the final serialization of an already-selected
body. mdrcel's cascade selects the ~75 KB of substantive 10-K content the rest
of the trafilatura pipeline would emit.

Because the empty result is produced upstream of the markdown/txt branch
(`core.py:71-98`), the txt output is empty for exactly the same reason the
markdown output was empty.

## Cross-reference

Full diagnosis + anti-inversion analysis: `wrk_docs/m5-allowlist/41d2afac.md`
(markdown gate). This M7 ADR records that the SAME Python under-extraction
manifests on the txt path; no new mechanism.

## Anti-inversion check

Matching Python would mean deliberately discarding ~75 KB of correctly-extracted
filing content to emit an empty string — textbook inversion (introducing data
loss to match a buggy oracle). Allowlisting is the honest call.
