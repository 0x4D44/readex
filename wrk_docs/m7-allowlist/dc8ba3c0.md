# M7 TXT Allowlist ADR — `dc8ba3c086153274.html` (DFIN XBRL 10-K, `&#153;` HTML5/CP-1252 divergence)

**Gate:** `tests/trafilatura_txt_gate.rs` (`--txt` / `output_format="txt"`).
**Bucket pre-allowlist:** `content-mismatch` (Rust = 467,056 chars, Python = 467,044 chars; first-diff at byte ~84,001).
**Verdict:** anti-inversion-clean — identical root cause to the markdown gate, format-independent.

## Why this is the same divergence as markdown

The source contains `CrossMod&#153;, town homes …`. The numeric character
reference `&#153;` is in the CP-1252 high-control range 0x80–0x9F, which
HTML5 §13.2.5 ("Numeric character reference end state") MUST remap to a
printable codepoint: 0x99 → U+2122 ™.

```
rust:   …CrossMod™, town homes…
python: …CrossMod, town homes…
```

html5ever (mdrcel) applies the spec remap → U+2122 ™. lxml (Python) treats
`&#153;` literally as U+0099 (a C1 control) and strips it during
normalization. This is a **character-decoding** divergence rooted in the HTML
parser, entirely independent of `output_format` — so the txt path diverges by
the same trademark glyph as the markdown path.

12-char delta in a 467 KB document (0.0026%).

## Cross-reference

Full diagnosis + HTML5 spec citation + anti-inversion analysis:
`wrk_docs/m5-allowlist/dc8ba3c0.md` (markdown gate). This M7 ADR records that
the SAME CP-1252 remap divergence manifests on the txt path; no new mechanism.

## Anti-inversion check

Matching Python would mean deliberately stripping trademark glyphs to mirror
lxml's loss-of-information — inversion in the worst sense (introducing data
loss to match a buggy oracle). Allowlisting is correct.
