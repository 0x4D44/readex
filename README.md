# readex

[![Crates.io](https://img.shields.io/crates/v/readex.svg)](https://crates.io/crates/readex)
[![docs.rs](https://img.shields.io/docsrs/readex)](https://docs.rs/readex)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

**HTML main-content extraction for Rust.** Give `readex` a `&str` of HTML and
it returns the article body, title, byline, publish date, language, and
~15 other metadata fields — no network I/O, no JavaScript rendering, no
encoding detection. Pure synchronous string and DOM work, suitable for
embedding anywhere from a desktop tool to a server pipeline.

---

## Quick start

```toml
[dependencies]
readex = "0.19"
```

```rust
use readex::{extract, Extracted};

let html = r#"
    <html>
      <head><title>Hello readex</title></head>
      <body>
        <article>
          <h1>Hello readex</h1>
          <p>This is the body of an article. It contains enough words
             that the extractor will consider it substantive content.</p>
          <p>A second paragraph adds more text so the scorer has signal.</p>
        </article>
      </body>
    </html>
"#;

let Extracted { title, text, .. } = extract(html, None).expect("extraction failed");

assert_eq!(title.as_deref(), Some("Hello readex"));
assert!(text.contains("body of an article"));
```

## A more representative example

Real web pages come wrapped in navigation, cookie banners, share widgets, and
comment sections. `readex` strips the chrome and returns just the body and
the metadata it can recover:

```rust
use readex::extract;

let html = r#"
    <html lang="en">
      <head>
        <title>Why the bridge collapsed — The Daily Example</title>
        <meta property="og:site_name" content="The Daily Example">
        <meta name="author" content="Jane Reporter">
        <meta property="article:published_time" content="2026-05-24T09:30:00Z">
      </head>
      <body>
        <nav><a href="/">Home</a> <a href="/news">News</a></nav>
        <aside class="cookie-banner">We use cookies. <button>OK</button></aside>
        <article>
          <h1>Why the bridge collapsed</h1>
          <p class="byline">By Jane Reporter, 24 May 2026</p>
          <p>Investigators arrived on site shortly after dawn and began
             sampling the steelwork for fatigue cracks.</p>
          <p>The bridge, opened in 1972, had been scheduled for inspection
             next month. Engineers say the failure mode is consistent with
             corrosion at the western anchorage.</p>
        </article>
        <section class="comments">
          <h3>Comments (412)</h3>
          <p>"Knew this would happen" — anonymous</p>
        </section>
        <footer>© 2026 The Daily Example</footer>
      </body>
    </html>
"#;

let result = extract(html, Some("https://example.com/news/bridge")).unwrap();

assert_eq!(result.title.as_deref(), Some("Why the bridge collapsed"));
assert_eq!(result.byline.as_deref(), Some("Jane Reporter"));
assert_eq!(result.site_name.as_deref(), Some("The Daily Example"));
assert_eq!(result.language.as_deref(), Some("en"));
assert!(result.published_time.is_some());
assert!(result.text.contains("Investigators arrived on site"));
assert!(!result.text.contains("cookie"));          // banner stripped
assert!(!result.text.contains("Home"));            // nav stripped
assert!(!result.text.contains("Knew this would")); // comments stripped
```

`readex` carries the lineage of three well-validated extractors:

| Origin | Role inside `readex` |
| --- | --- |
| [Mozilla Readability](https://github.com/mozilla/readability) (JS) | Article-scoring core — the M2 port preserves the full `_grabArticle` / `_prepArticle` / flag-sieve pipeline. |
| [Trafilatura](https://trafilatura.readthedocs.io) (Python) | The M3 cascade — own → readability fork → jusText — with the 7-branch arbiter, dedup gate, and sanitize post-pass. |
| [htmldate](https://htmldate.readthedocs.io) (Python) | Publication-date extraction with the same precedence rules as upstream. |

Each is a clean-room reimplementation in Rust; the upstream Python and
JavaScript projects are the differential-test oracles, not vendored code.

---

## API reference (cheat sheet)

| Function | Purpose |
| --- | --- |
| [`extract`] | Default extraction. Returns an [`Extracted`] with title, body text, canonical URL, language, byline, excerpt, site name, published time, categories, tags, image, license, hostname, and (optionally) sanitised HTML. |
| [`extract_with`] | `extract(html, base_url)` plus a third `&Options` parameter (so `extract_with(html, base_url, &Options::default())` is exactly equivalent to `extract(html, base_url)`). Lets you opt into sanitised HTML output, set a minimum word-count threshold, or request a YAML metadata header. |
| [`extract_to_markdown`] | Body as Markdown — Trafilatura's `output_format="markdown"`. |
| [`extract_to_txt`] | Plain-text body — Trafilatura's `output_format="txt"`. |
| `extract_to_json` / `extract_to_csv` / `extract_to_xml` / `extract_to_tei` | Structured output formats. |
| [`extract_via_readability`] | Forces the M2 Mozilla-Readability path (older, simpler, no Trafilatura cascade). Useful when you specifically need that algorithm's output shape. |

`extract` and `extract_with(.., .., &Options::default())` are byte-identical by
construction — `extract` is literally a one-line delegate, so the two cannot
drift apart.

---

## Why readex?

There are already a handful of HTML-extraction crates on crates.io. Honest
positioning vs. the obvious alternatives:

| | `readex` | [`readability`](https://crates.io/crates/readability) | [`dom_content_extraction`](https://crates.io/crates/dom_content_extraction) |
| --- | --- | --- | --- |
| Algorithms | Readability + Trafilatura cascade + htmldate | Readability only | DOM-centric (different family) |
| Metadata fields | ~15 (title, byline, language, dates, OG/Twitter/JSON-LD, categories, tags, image, license, hostname …) | Title + summary | Body text only |
| Output formats | text, sanitised HTML, Markdown, TXT, JSON, CSV, XML, TEI | text only | text only |
| Differential parity testing | Yes — 51-URL corpus + 50K broad sweep, every release | No | No |
| Hard pin on parser versions | Yes (`html5ever 0.39.0`, plus a documented "parser-equivalence fence") | No | No |
| Edition / MSRV | 2024 / 1.85 | 2018 / older | 2021 / older |
| Comments extraction (Reddit/vBulletin/etc.) | Yes (via Trafilatura) | No | No |
| Date extraction | Yes (via htmldate) | No | No |

If your input is well-structured English-language articles and you want one
algorithm with no extra moving parts, `readability` may be all you need.
`readex` exists because real-world corpora (SEC filings, regulator
publications, multilingual news, low-template blogs, hub/index pages) defeat
single-algorithm extractors — the Trafilatura cascade was designed
specifically for that long tail.

---

## Quality & differential testing

`readex` is developed against a differential-test harness that runs every
benchmark URL through three extractors in parallel — `readex`, Mozilla
Readability (via Node), and Trafilatura (via Python) — and scores agreement
across token sequences and metadata fields. The harness lives in
`benchmark/` in the repo (not published as part of the crate) and is
re-run on every release.

Latest verdicts (as of 0.19.0):

| Gate | Corpus | Result |
| --- | --- | --- |
| Trafilatura `extract_content` (Markdown path) | 51 URLs | **48 / 51** byte-equivalent (41 substantive + 7 documented allowlist) |
| Trafilatura plain-text (TXT) path | 51 URLs | **45 / 51** substantive + 5 allowlist + 1 deferred |
| Trafilatura TEI structured output | 51 URLs | **51 / 51** (39 substantive + 12 allowlist) |
| Mozilla Readability `textContent` | 51 URLs | **50 / 51** byte-equivalent vs. jsdom |
| Parser equivalence (rcdom vs. jsdom) | 51 URLs | **51 / 51** byte-equivalent DOM |
| Broad-sweep confidence (Common Crawl) | **50,000 pages** | Tail-distribution scan vs. Python Trafilatura |

The "allowlist" entries are documented per-page divergences where `readex`
and upstream genuinely disagree for traced reasons (e.g. upstream emits a
cookie banner the page lacks chrome-class hints for; or upstream skips a
table the data-table heuristic rescues). They live under
`wrk_docs/m{5,7}-allowlist/` in the repo with one Markdown file per
fixture.

If you find a page where `readex` disagrees with both Readability and
Trafilatura in a way that matters, please file an issue with the URL or
HTML — the harness will pick it up.

---

## What is out of scope

- **Network fetching.** `readex` takes a `&str`. The caller owns HTTP, redirects, SSRF guarding, and encoding detection.
- **JavaScript rendering.** `readex` parses the bytes as given. Pages that need JS to render their body need a headless browser upstream.
- **PDF extraction.** HTML only.
- **Streaming.** The whole document is parsed at once.

These boundaries keep the crate sync, dependency-light, and easy to embed.

---

## Status

`0.19.0` is the first public crates.io release. The API surface is:

- Stable: `extract`, `extract_with`, `extract_to_markdown`, `extract_to_txt`,
  `extract_to_json`, `extract_to_csv`, `extract_to_xml`, `extract_to_tei`,
  `extract_via_readability`, plus the `Extracted`, `Options`, and
  `ExtractError` types they use.
- `#[doc(hidden)]` internals: `readability::*`, `trafilatura::*`,
  `htmldate::*`. These are reachable but **explicitly not part of the
  semver contract** — they exist for the in-workspace differential test
  harness. Treat them as private; they can change at any time.

`readex` is at 0.x — additive minor bumps may add fields to `Extracted` or
`Options`; breaking changes (renames, signature changes) will only land on
a 0.X.0 boundary with a clear changelog entry.

### Minimum supported Rust version (MSRV)

`readex` targets **Rust 1.85+** (Rust 2024 edition).

---

## Contributing

Issues and PRs welcome at <https://github.com/0x4D44/readex>. For
non-trivial changes, please open an issue first so we can discuss the
approach — `readex` is gated by a parity-test harness against Readability
and Trafilatura, and the cheapest path through that gate is usually a
quick sketch of intent before code.

---

## License

Licensed under either of:

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT License ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in `readex` by you, as defined in the Apache-2.0
license, shall be dual-licensed as above, without any additional terms or
conditions.

See [NOTICE](NOTICE) for attribution to the upstream Readability,
Trafilatura, and htmldate projects whose algorithms `readex` ports.

[`extract`]: https://docs.rs/readex/latest/readex/fn.extract.html
[`extract_with`]: https://docs.rs/readex/latest/readex/fn.extract_with.html
[`extract_to_markdown`]: https://docs.rs/readex/latest/readex/fn.extract_to_markdown.html
[`extract_to_txt`]: https://docs.rs/readex/latest/readex/fn.extract_to_txt.html
[`extract_via_readability`]: https://docs.rs/readex/latest/readex/fn.extract_via_readability.html
[`Extracted`]: https://docs.rs/readex/latest/readex/struct.Extracted.html
[`Options`]: https://docs.rs/readex/latest/readex/struct.Options.html
