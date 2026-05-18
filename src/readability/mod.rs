//! Idiomatic Rust port of Mozilla Readability v0.6.0 (HLD
//! `2026.05.18 - HLD - mdrcel Readability Port (M2)`).
//!
//! # Module map (HLD §5)
//!
//! This mirrors `Readability.js` 1:1 so every ported line has an obvious home
//! and a cited spec anchor. **No trait / strategy / plugin scaffolding** — the
//! anti-premature-abstraction rule (CLAUDE.md; HLD §5).
//!
//! | Module | Responsibility | Stage |
//! |---|---|---|
//! | [`dom`] | Facade over `markup5ever_rcdom` — the score-critical DOM primitives (WHATWG `Node.textContent`, snapshots, `set_node_tag` slow branch, the score / data-table side-tables) | **Stage 0 (this)** |
//! | [`regexps`] | `REGEXPS` + the JS-faithful regex dialect (HLD §8) | Stage 1a |
//! | [`scoring`] | `_initializeNode` / `_getClassWeight` / `_getLinkDensity` / … | Stage 1a |
//! | [`grab_article`] | `_grabArticle` (the algorithmic core) | Stage 1a |
//! | [`prep`] | `_prepDocument` / `_prepArticle` / `_cleanConditionally` / `_markDataTables` / … | Stage 1a/2 |
//! | [`helpers`] | `_isProbablyVisible` / `_isPhrasingContent` / `_getNextNode` / … | Stage 1a |
//! | [`metadata`] | `_getArticleTitle` / `_getArticleMetadata` / `_getJSONLD` | Stage 1a/4 |
//!
//! Stage 0 implements **only** [`dom`]; every other module is a declared stub
//! so the module tree compiles (HLD §6 — "others are declared stubs"). The
//! `Readability` struct + `parse()` workflow land in Stage 1a.

pub mod dom;

// --- Stage-1a+ stubs (HLD §5/§6) ----------------------------------------
//
// Declared now so the module tree compiles as the HLD §5 layout dictates;
// each is implemented in its scheduled stage (table above). They are
// intentionally empty — a stub with speculative contents would be the
// premature-abstraction antipattern the brief/CLAUDE.md forbid.

/// `REGEXPS` (`Readability.js:137-175`) + the JS-faithful regex dialect
/// (HLD §8). Stub until Stage 1a.
pub mod regexps {}

/// Scoring primitives (`_initializeNode` / `_getClassWeight` /
/// `_getLinkDensity` / `_getTextDensity` / `_textSimilarity` /
/// `_getCharCount` / `_getNodeAncestors`). Stub until Stage 1a.
pub mod scoring {}

/// `_grabArticle` — the algorithmic core (`Readability.js:1031-1597`).
/// Stub until Stage 1a.
pub mod grab_article {}

/// `_prepDocument` / `_prepArticle` / `_cleanConditionally` /
/// `_markDataTables` / … (`Readability.js:776-884`, `2240-2263`).
/// Stub until Stage 1a/2.
pub mod prep {}

/// `_isProbablyVisible` / `_isPhrasingContent` / `_isWhitespace` /
/// `_getNextNode` / `_removeAndGetNext` / … . Stub until Stage 1a.
pub mod helpers {}

/// `_getArticleTitle` (`Readability.js:572-651`) / `_getArticleMetadata` /
/// `_getJSONLD`. Stub until Stage 1a/4.
pub mod metadata {}
