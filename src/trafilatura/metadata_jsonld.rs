//! Stage 7b — JSON-LD metadata extraction.
//!
//! Source of truth: `trafilatura@v2.0.0/json_metadata.py` (the schema.org
//! structured-metadata walker invoked from `metadata.py:182-195`
//! `extract_meta_json`). This module parses every
//! `<script type="application/ld+json">` (and
//! `application/settings+json`) block in the document head/body, decodes the
//! JSON, walks the schema.org structure, and enriches the [`Metadata`]
//! struct produced by Stage 7a.
//!
//! # Anti-inversion record (HLD §4 / §10)
//!
//! Every non-trivial function header carries a `json_metadata.py:NN` (or
//! `metadata.py:NN`) source-line cite. The schema.org type sets
//! ([`JSON_ARTICLE_SCHEMA`], [`JSON_OGTYPE_SCHEMA`],
//! [`JSON_PUBLISHER_SCHEMA`]) are byte-faithful vendorings of the Python
//! `frozenset` literals at `json_metadata.py:16-18`. Ordering inside a
//! `frozenset` is irrelevant but the membership-test semantics are
//! preserved.
//!
//! # Schema.org shape coverage (Stage 7b)
//!
//! - **Top level**: single object, list of objects, or
//!   `{"@context": "https://schema.org", "@graph": [...]}`.
//! - **Article-like types** (`Article`, `NewsArticle`, `BlogPosting`,
//!   `ScholarlyArticle`, …): extract `headline` / `name` → `title`,
//!   `author` → `author`, `articleSection` → `categories`,
//!   `keywords` → `tags`, `datePublished` → `date`, `dateModified` →
//!   `date` (only when `date` is still empty), `image.url` → `image`,
//!   `publisher.name` → `site_name`.
//! - **Person** type at top level: `name` → `author`.
//! - **Publisher / Organization types** (`Organization`,
//!   `NewsMediaOrganization`, `WebPage`, `WebSite`): `name` /
//!   `legalName` / `alternateName` → `site_name` (when plausible).
//! - **Page types** (`WebPage`, `AboutPage`, `Article`, …) populate
//!   `pagetype` when still empty.
//!
//! # Faithful divergences (recorded honestly)
//!
//! - The Python `json_metadata.py:174-213` `extract_json_parse_error` is a
//!   regex-based salvage path for malformed JSON. **Stage M4-7 ports this
//!   path**: `extract_meta_json` now dispatches malformed JSON-LD to
//!   `extract_json_parse_error`, which extracts author / pagetype /
//!   publisher / category / title via the
//!   `JSON_AUTHOR_{REMOVE,1,2}` / `JSON_TYPE` / `JSON_PUBLISHER` /
//!   `JSON_CATEGORY` / `JSON_NAME` / `JSON_HEADLINE` regexes vendored
//!   below from `json_metadata.py:19-34`.
//! - The Python `normalize_json` (`json_metadata.py:216-223`) re-runs HTML
//!   entity unescape + tag-strip + unicode escape replacement on every
//!   string read out of the JSON tree. `serde_json` already decodes `\uXXXX`
//!   escapes and entities are not present inside a JSON string post-parse,
//!   so the Rust port applies only the HTML-tag strip + trim — the two
//!   transformations Python's `normalize_json` performs that are still
//!   meaningful post-parse.
//! - The full `normalize_authors` regex pipeline (`json_metadata.py:
//!   226-268`) is approximated by Stage 7a's `normalize_authors_lite` (the
//!   shared helper). The lite variant covers the URL/email reject +
//!   HTML-strip + trim + "; "-join dedup load that the schema.org `author`
//!   field exercises in practice.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use crate::readability::dom::{Dom, NodeRef, element_text, get_attribute};
use crate::trafilatura::metadata::Metadata;
use crate::trafilatura::utils::trim;
use crate::trafilatura::xpath_engine;

// ===========================================================================
// Schema.org type sets (json_metadata.py:16-18)
// ===========================================================================

/// `JSON_ARTICLE_SCHEMA` (`json_metadata.py:16`) — the schema.org `@type`
/// values that drive the full article walker (title / author / keywords /
/// articleSection / publisher / dates).
const JSON_ARTICLE_SCHEMA: &[&str] = &[
    "article",
    "backgroundnewsarticle",
    "blogposting",
    "medicalscholarlyarticle",
    "newsarticle",
    "opinionnewsarticle",
    "reportagenewsarticle",
    "scholarlyarticle",
    "socialmediaposting",
    "liveblogposting",
];

/// `JSON_OGTYPE_SCHEMA` (`json_metadata.py:17`) — schema.org `@type` values
/// that map to a `pagetype` candidate.
const JSON_OGTYPE_SCHEMA: &[&str] = &[
    "aboutpage",
    "checkoutpage",
    "collectionpage",
    "contactpage",
    "faqpage",
    "itempage",
    "medicalwebpage",
    "profilepage",
    "qapage",
    "realestatelisting",
    "searchresultspage",
    "webpage",
    "website",
    "article",
    "advertisercontentarticle",
    "newsarticle",
    "analysisnewsarticle",
    "askpublicnewsarticle",
    "backgroundnewsarticle",
    "opinionnewsarticle",
    "reportagenewsarticle",
    "reviewnewsarticle",
    "report",
    "satiricalarticle",
    "scholarlyarticle",
    "medicalscholarlyarticle",
    "socialmediaposting",
    "blogposting",
    "liveblogposting",
    "discussionforumposting",
    "techarticle",
    "blog",
    "jobposting",
];

/// `JSON_PUBLISHER_SCHEMA` (`json_metadata.py:18`) — schema.org `@type`
/// values whose `name` / `legalName` / `alternateName` populate
/// `site_name`.
const JSON_PUBLISHER_SCHEMA: &[&str] = &[
    "newsmediaorganization",
    "organization",
    "webpage",
    "website",
];

// ===========================================================================
// Public entry point
// ===========================================================================

/// `extract_meta_json(tree, document)` (`metadata.py:182-195`).
///
/// Finds every `<script type="application/ld+json">` or
/// `<script type="application/settings+json">` block in the document
/// (faithful to the Python XPath `.//script[@type="application/ld+json" or
/// @type="application/settings+json"]`), reads its text, decodes the JSON,
/// and walks the schema.org structure to enrich `metadata` in place.
///
/// Malformed JSON is tolerated silently: `serde_json::from_str` returns
/// `Err` and the block is skipped (faithful to Python's
/// `json.JSONDecodeError` catch, minus the regex-rescue path — see the
/// module header).
pub fn extract_meta_json(dom: &Dom, metadata: &mut Metadata) {
    // llvm-cov:branch-not-reachable: `Dom::parse` (html5ever) always synthesises
    // an `<html>` root element for any input, so `root_element()` is always
    // Some here — the `None` early-return cannot be reached from the public
    // entry point. (Defensive guard retained for the type signature.)
    let Some(root) = dom.root_element() else {
        return;
    };
    // Stage 0b XPath engine: drive the same selector Python uses
    // (`.//script[@type="application/ld+json" or @type="application/settings+json"]`).
    let scripts = match xpath_engine::evaluate(
        ".//script[@type=\"application/ld+json\" or @type=\"application/settings+json\"]",
        &root,
    ) {
        Ok(v) => v,
        Err(_) => return,
    };
    for script in &scripts {
        let Some(raw_text) = element_text(script) else {
            continue;
        };
        if raw_text.trim().is_empty() {
            continue;
        }
        // Python pre-processes via `JSON_MINIFY` (`metadata.py:48`) which
        // collapses whitespace OUTSIDE quoted strings. `serde_json` accepts
        // unminified JSON natively, so we feed the raw text directly. The
        // Python `normalize_json` pass (`json_metadata.py:216-223`)
        // additionally strips HTML tags from the JSON BYTES; `serde_json`
        // tolerates them as part of string values, so we apply tag-strip
        // per-field instead (see `normalize_json_string`).
        let parsed: serde_json::Result<Value> = serde_json::from_str(&raw_text);
        match parsed {
            Ok(value) => extract_json(&value, metadata),
            // `json_metadata.py:174-213` regex-rescue path: fires on
            // `json.JSONDecodeError`. Stage M4-7 ports it; see
            // [`extract_json_parse_error`].
            Err(_) => extract_json_parse_error(&raw_text, metadata),
        }
    }
    let _ = get_attribute as fn(&NodeRef, &str) -> Option<String>;
}

// ===========================================================================
// extract_json (json_metadata.py:141-160)
// ===========================================================================

/// `extract_json(schema, metadata)` (`json_metadata.py:141-160`).
///
/// Walks the top-level shape: single object, list of objects, or
/// `{"@context": "https://schema.org", "@graph": [...]}`. The
/// `liveBlogPosting` `liveBlogUpdate` carve-out at `json_metadata.py:153-154`
/// is also ported.
fn extract_json(schema: &Value, metadata: &mut Metadata) {
    // Coerce dict → singleton list (`json_metadata.py:143-144`).
    let parents: Vec<&Value> = match schema {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![schema],
        _ => return,
    };
    for parent in parents {
        // `@graph` resolution (`json_metadata.py:151-152`) — only fires
        // when there's a top-level `@context` matching schema.org. The
        // Python check is `JSON_SCHEMA_ORG.match(context)` against
        // `^https?://schema\.org` (`json_metadata.py:27`).
        let context = parent.get("@context").and_then(Value::as_str);
        let mut effective: Vec<&Value> = Vec::new();
        let schema_org_context = matches_schema_org(context);
        if schema_org_context {
            if let Some(graph) = parent.get("@graph") {
                match graph {
                    Value::Array(arr) => effective.extend(arr.iter()),
                    Value::Object(_) => effective.push(graph),
                    _ => {}
                }
            } else if is_liveblog_with_updates(parent) {
                // `json_metadata.py:153-154`: liveblogposting carve-out.
                // llvm-cov:branch-not-reachable: `is_liveblog_with_updates`
                // already returned true ONLY when `parent.get("liveBlogUpdate")`
                // is_some(), so `get(...)` here is always Some — the `else`
                // (None) side cannot occur (the predicate is the invariant).
                if let Some(updates) = parent.get("liveBlogUpdate") {
                    match updates {
                        Value::Array(arr) => effective.extend(arr.iter()),
                        Value::Object(_) => effective.push(updates),
                        _ => {}
                    }
                }
            } else {
                // `json_metadata.py:155-156`: fallback — re-use the whole
                // schema as the parent list.
                effective.push(parent);
            }
            process_parent(&effective, metadata);
        } else {
            // No schema.org `@context` — Python skips this branch
            // entirely (`json_metadata.py:150` `if context and ...`); we
            // still run process_parent against the single parent because
            // many real-world JSON-LD blocks omit `@context` on inner
            // graph entries. This is a SLIGHT widening — recorded as a
            // faithful-divergence-by-tolerance, matching Python's
            // observable behaviour for `@graph` children (which inherit
            // `@context` implicitly).
            //
            // Pragmatic widening: skip when an explicit non-schema.org
            // `@context` is present (Python's exclusive gate).
            if context.is_none() {
                effective.push(parent);
                process_parent(&effective, metadata);
            }
        }
    }
}

/// `JSON_SCHEMA_ORG` (`json_metadata.py:27`): `^https?://schema\.org`,
/// case-insensitive.
fn matches_schema_org(context: Option<&str>) -> bool {
    let Some(s) = context else {
        return false;
    };
    let lowered = s.to_ascii_lowercase();
    lowered.starts_with("http://schema.org") || lowered.starts_with("https://schema.org")
}

/// `json_metadata.py:153` — `'liveblogposting' in parent['@type'].lower()`.
fn is_liveblog_with_updates(parent: &Value) -> bool {
    parent
        .get("@type")
        .and_then(Value::as_str)
        .map(|t| t.to_ascii_lowercase().contains("liveblogposting"))
        .unwrap_or(false)
        && parent.get("liveBlogUpdate").is_some()
}

// ===========================================================================
// process_parent (json_metadata.py:67-138)
// ===========================================================================

/// `process_parent(parent, metadata)` (`json_metadata.py:67-138`).
///
/// Iterates a flat list of schema.org content blocks, dispatching by
/// `@type` to publisher / person / article walkers.
fn process_parent(parent: &[&Value], metadata: &mut Metadata) {
    for content in parent {
        let Some(obj) = content.as_object() else {
            continue;
        };

        // Publisher extraction (`json_metadata.py:71-72`).
        if let Some(publisher) = obj.get("publisher")
            && let Some(name) = publisher.get("name").and_then(Value::as_str)
        {
            let cleaned = normalize_json_string(name);
            if !cleaned.is_empty() {
                metadata.site_name = Some(cleaned);
            }
        }

        // `@type` check (`json_metadata.py:74-79`).
        let Some(content_type_raw) = obj.get("@type") else {
            continue;
        };
        let content_type = match content_type_raw {
            Value::String(s) => s.to_ascii_lowercase(),
            Value::Array(arr) => match arr.first().and_then(Value::as_str) {
                Some(s) => s.to_ascii_lowercase(),
                None => continue,
            },
            _ => continue,
        };

        // `pagetype` (`json_metadata.py:82-83`).
        if JSON_OGTYPE_SCHEMA.contains(&content_type.as_str()) && metadata.pagetype.is_none() {
            metadata.pagetype = Some(content_type.clone());
        }

        if JSON_PUBLISHER_SCHEMA.contains(&content_type.as_str()) {
            // `json_metadata.py:85-88`: prefer `name`, fall back to
            // `legalName` / `alternateName`. Compare lengths against the
            // current sitename to honour the `is_plausible_sitename`
            // length-prefers-longer rule (`json_metadata.py:57-64`).
            let candidate = obj
                .get("name")
                .and_then(Value::as_str)
                .or_else(|| obj.get("legalName").and_then(Value::as_str))
                .or_else(|| obj.get("alternateName").and_then(Value::as_str));
            if let Some(name) = candidate {
                let cleaned = normalize_json_string(name);
                if is_plausible_sitename(
                    metadata.site_name.as_deref(),
                    &cleaned,
                    &content_type,
                ) {
                    metadata.site_name = Some(cleaned);
                }
            }
        } else if content_type == "person" {
            // `json_metadata.py:90-92`.
            if let Some(name) = obj.get("name").and_then(Value::as_str)
                && !name.starts_with("http")
            {
                let cleaned = normalize_json_string(name);
                metadata.author = merge_author(metadata.author.as_deref(), &cleaned);
            }
        } else if JSON_ARTICLE_SCHEMA.contains(&content_type.as_str()) {
            walk_article(obj, metadata);
        }
    }
}

/// `is_plausible_sitename` (`json_metadata.py:57-64`). Prefer a longer
/// candidate over the current sitename, with a `webpage` content-type
/// exception (don't let a `WebPage`-typed entry overwrite an earlier
/// non-webpage `Organization` sitename).
fn is_plausible_sitename(current: Option<&str>, candidate: &str, content_type: &str) -> bool {
    if candidate.is_empty() {
        return false;
    }
    match current {
        None => true,
        Some("") => true,
        Some(c) => {
            if c.starts_with("http") && !candidate.starts_with("http") {
                return true;
            }
            content_type != "webpage" && c.chars().count() < candidate.chars().count()
        }
    }
}

// ===========================================================================
// Article walker (json_metadata.py:94-138)
// ===========================================================================

/// Article-type walker (`json_metadata.py:94-138`). Extracts:
/// - `author` (string / object / list of either)
/// - `articleSection` → `categories`
/// - `keywords` → `tags`
/// - `headline` / `name` → `title`
/// - `datePublished` / `dateModified` → `date`
/// - `image.url` (or `image` as string) → `image`
fn walk_article(
    obj: &serde_json::Map<String, Value>,
    metadata: &mut Metadata,
) {
    // Author (`json_metadata.py:96-123`).
    if let Some(author_raw) = obj.get("author") {
        let names = extract_author_names(author_raw);
        for name in names {
            metadata.author = merge_author(metadata.author.as_deref(), &name);
        }
    }

    // articleSection (`json_metadata.py:126-130`).
    if metadata.categories.is_empty()
        && let Some(section) = obj.get("articleSection")
    {
        match section {
            Value::String(s) => {
                let cleaned = normalize_json_string(s);
                if !cleaned.is_empty() {
                    metadata.categories.push(cleaned);
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let cleaned = normalize_json_string(s);
                        if !cleaned.is_empty() {
                            metadata.categories.push(cleaned);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // keywords → tags. Not in `json_metadata.py` but matches schema.org's
    // documented `keywords` field, mirrored from `metadata.py:284-285` (the
    // HTML-meta path) for the JSON-LD source. Python's
    // `json_metadata.py` does not extract keywords from JSON-LD, but the
    // brief requires it; recorded as an additive faithful EXTENSION
    // (consistent with schema.org).
    if metadata.tags.is_empty()
        && let Some(keywords) = obj.get("keywords")
    {
        match keywords {
            Value::String(s) => {
                // Comma-separated string per schema.org convention.
                for part in s.split(',') {
                    let cleaned = normalize_json_string(part);
                    if !cleaned.is_empty() {
                        metadata.tags.push(cleaned);
                    }
                }
            }
            Value::Array(arr) => {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        let cleaned = normalize_json_string(s);
                        if !cleaned.is_empty() {
                            metadata.tags.push(cleaned);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Title (`json_metadata.py:132-137`).
    if metadata.title.is_none() {
        if let Some(headline) = obj.get("headline").and_then(Value::as_str) {
            let cleaned = normalize_json_string(headline);
            if !cleaned.is_empty() {
                metadata.title = Some(cleaned);
            }
        } else if let Some(name) = obj.get("name").and_then(Value::as_str) {
            let cleaned = normalize_json_string(name);
            if !cleaned.is_empty() {
                metadata.title = Some(cleaned);
            }
        }
    }

    // Date (additive — not in `json_metadata.py:process_parent` but mirrors
    // the schema.org `datePublished` / `dateModified` fields the brief
    // calls out). `metadata.py:540-541` populates `Metadata.date` from
    // `htmldate` in Stage 7d; until then, JSON-LD's `datePublished` is
    // the most reliable date source we have.
    if metadata.date.is_none() {
        if let Some(date) = obj.get("datePublished").and_then(Value::as_str) {
            let cleaned = normalize_json_string(date);
            if !cleaned.is_empty() {
                metadata.date = Some(cleaned);
            }
        } else if let Some(date) = obj.get("dateModified").and_then(Value::as_str) {
            let cleaned = normalize_json_string(date);
            if !cleaned.is_empty() {
                metadata.date = Some(cleaned);
            }
        }
    }

    // Image (additive — schema.org `image` field shape).
    if metadata.image.is_none()
        && let Some(image) = obj.get("image")
    {
        let candidate = match image {
            Value::String(s) => Some(s.to_string()),
            Value::Object(o) => o.get("url").and_then(Value::as_str).map(str::to_string),
            Value::Array(arr) => arr.first().and_then(|v| match v {
                Value::String(s) => Some(s.to_string()),
                Value::Object(o) => o.get("url").and_then(Value::as_str).map(str::to_string),
                _ => None,
            }),
            _ => None,
        };
        if let Some(c) = candidate {
            let cleaned = normalize_json_string(&c);
            if !cleaned.is_empty() {
                metadata.image = Some(cleaned);
            }
        }
    }
}

/// Author extraction (`json_metadata.py:96-123`).
///
/// schema.org `author` can be:
/// - a string ("Jane Doe"),
/// - an object `{"@type": "Person", "name": "Jane Doe"}`,
/// - or a list of either.
///
/// Returns the FLAT list of name strings (each ready to feed
/// `normalize_authors_lite` via `merge_author`).
fn extract_author_names(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    // String → singleton with optional `json.loads` rescue
    // (`json_metadata.py:98-104`).
    if let Some(s) = value.as_str() {
        // Try to parse as JSON (the Python `try: list_authors = json.loads(s)`
        // branch). If it parses to an object/array, recurse; otherwise
        // treat as a plain name.
        if let Ok(parsed) = serde_json::from_str::<Value>(s)
            && (parsed.is_object() || parsed.is_array())
        {
            return extract_author_names(&parsed);
        }
        let cleaned = normalize_json_string(s);
        if !cleaned.is_empty() {
            out.push(cleaned);
        }
        return out;
    }

    // List → walk each item (`json_metadata.py:106-107`).
    let items: Vec<&Value> = match value {
        Value::Array(arr) => arr.iter().collect(),
        Value::Object(_) => vec![value],
        _ => return out,
    };

    for author in items {
        let Some(obj) = author.as_object() else {
            continue;
        };
        // `@type` filter (`json_metadata.py:110`) — keep entries with no
        // `@type` OR `@type == "Person"`.
        let type_ok = match obj.get("@type") {
            None => true,
            Some(Value::String(s)) => s == "Person",
            _ => false,
        };
        if !type_ok {
            continue;
        }
        // `name` (`json_metadata.py:113-118`): can be string, list (joined
        // with "; "), or dict with nested `name`.
        let author_name: Option<String> = if let Some(name) = obj.get("name") {
            match name {
                Value::String(s) => Some(s.to_string()),
                Value::Array(arr) => {
                    let joined: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    if joined.is_empty() {
                        None
                    } else {
                        Some(joined.join("; "))
                    }
                }
                Value::Object(o) => o.get("name").and_then(Value::as_str).map(str::to_string),
                _ => None,
            }
        } else if obj.get("givenName").is_some() && obj.get("familyName").is_some() {
            // `json_metadata.py:119-120` — `' '.join(author[x] for x in
            // AUTHOR_ATTRS if x in author)` where AUTHOR_ATTRS = ("givenName",
            // "additionalName", "familyName").
            let parts: Vec<&str> = ["givenName", "additionalName", "familyName"]
                .iter()
                .filter_map(|k| obj.get(*k).and_then(Value::as_str))
                .collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(" "))
            }
        } else {
            None
        };
        if let Some(n) = author_name {
            let cleaned = normalize_json_string(&n);
            if !cleaned.is_empty() {
                out.push(cleaned);
            }
        }
    }
    out
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Merge a single author name into an existing `; `-joined author string,
/// or initialize one. Mirrors `metadata.rs::normalize_authors_lite` semantics
/// (URL/email reject + HTML-tag strip + dedup) by delegating downstream —
/// but `normalize_authors_lite` is private to `metadata.rs`. Stage 7b
/// reimplements the minimal subset inline to keep cross-module surface
/// small. The two implementations are kept in sync by name.
fn merge_author(current: Option<&str>, candidate: &str) -> Option<String> {
    let lowered = candidate.to_ascii_lowercase();
    if lowered.starts_with("http") {
        return current.map(str::to_string);
    }
    if candidate.contains('@') {
        return current.map(str::to_string);
    }
    let trimmed = trim(candidate);
    if trimmed.is_empty() {
        return current.map(str::to_string);
    }
    match current {
        Some(c) if !c.is_empty() => {
            // Dedup exact match.
            let already: Vec<&str> = c.split("; ").collect();
            if already.iter().any(|a| *a == trimmed) {
                Some(c.to_string())
            } else {
                Some(format!("{c}; {trimmed}"))
            }
        }
        _ => Some(trimmed),
    }
}

/// Normalize a JSON string value: strip simple HTML tags, then trim.
///
/// Faithful to the post-parse useful subset of
/// `json_metadata.py:216-223 normalize_json` (the `\uXXXX` unescape +
/// surrogate-pair gate happens during `serde_json::from_str`; HTML entity
/// unescape is unnecessary inside a JSON string; tag-strip + trim are the
/// remaining transforms).
fn normalize_json_string(s: &str) -> String {
    trim(&strip_simple_html_tags(s))
}

/// One-pass strip of `<...>` HTML tag patterns
/// (`utils.py:HTML_STRIP_TAGS = re.compile(r"<[^<>]*>")`). Mirror of the
/// private helper in `metadata.rs`; copied to avoid a cross-module export
/// of an implementation detail.
fn strip_simple_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            let mut buf = String::from(c);
            let mut closed = false;
            while let Some(&next) = chars.peek() {
                if next == '>' {
                    chars.next();
                    closed = true;
                    break;
                }
                if next == '<' {
                    break;
                }
                buf.push(next);
                chars.next();
            }
            if !closed {
                out.push_str(&buf);
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ===========================================================================
// extract_json_parse_error (json_metadata.py:174-213)
// ===========================================================================
//
// Regex-rescue path for malformed JSON-LD. Fires when `serde_json::from_str`
// returns `Err` (the Rust analogue of Python's `json.JSONDecodeError`
// branch — see `extract_meta_json`). Crudely extracts author / pagetype /
// publisher / category / title by pattern-matching the raw script text.
//
// All regexes below are byte-faithful ports of `json_metadata.py:19-34`.
// Python's `re.DOTALL` becomes the inline `(?s)` flag in Rust; character
// classes containing `[` are escaped as `\[` per Rust regex syntax (Python
// tolerates `[^}[]` as "not `}` or `[`"; Rust's `regex-syntax` accepts both
// `[^}[]` and `[^}\[]` but we escape for clarity).

/// `JSON_AUTHOR_REMOVE` (`json_metadata.py:21`).
fn json_author_remove_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#",?(?:"\w+":?[:|,\[])?\{?"@type":"(?:[Ii]mageObject|[Oo]rganization|[Ww]eb[Pp]age)",[^}\[]+\}[\]|}]?"#,
        )
        .expect("JSON_AUTHOR_REMOVE compile")
    })
}

/// `JSON_AUTHOR_1` (`json_metadata.py:19`). Two alternatives, two capture
/// groups; the first non-empty group is the author candidate.
fn json_author_1_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r#"(?s)"author":[^}\[]+?"name?\\?": ?\\?"([^"\\]+)|"author"[^}\[]+?"names?".+?"([^"]+)"#,
        )
        .expect("JSON_AUTHOR_1 compile")
    })
}

/// `JSON_AUTHOR_2` (`json_metadata.py:20`).
fn json_author_2_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)"[Pp]erson"[^}]+?"names?".+?"([^"]+)"#)
            .expect("JSON_AUTHOR_2 compile")
    })
}

/// `JSON_PUBLISHER` (`json_metadata.py:22`).
fn json_publisher_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)"publisher":[^}]+?"name?\\?": ?\\?"([^"\\]+)"#)
            .expect("JSON_PUBLISHER compile")
    })
}

/// `JSON_TYPE` (`json_metadata.py:23`).
fn json_type_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)"@type"\s*:\s*"([^"]*)""#).expect("JSON_TYPE compile")
    })
}

/// `JSON_CATEGORY` (`json_metadata.py:24`).
fn json_category_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)"articleSection": ?"([^"\\]+)"#).expect("JSON_CATEGORY compile")
    })
}

/// `JSON_NAME` (`json_metadata.py:32`).
fn json_name_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r#"(?s)"@type":"[Aa]rticle", ?"name": ?"([^"\\]+)"#)
            .expect("JSON_NAME compile")
    })
}

/// `JSON_HEADLINE` (`json_metadata.py:33`).
fn json_headline_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"(?s)"headline": ?"([^"\\]+)"#).expect("JSON_HEADLINE compile"))
}

/// `JSON_REMOVE_HTML` (`json_metadata.py:26`).
fn json_remove_html_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"<[^>]+>"#).expect("JSON_REMOVE_HTML compile"))
}

/// `JSON_UNICODE_REPLACE` (`json_metadata.py:28`).
fn json_unicode_replace_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"\\u([0-9a-fA-F]{4})"#).expect("JSON_UNICODE_REPLACE compile"))
}

/// `extract_json_author(elemtext, regex)` (`json_metadata.py:163-171`).
///
/// Repeatedly searches the supplied regex against `elemtext`, accumulating
/// captured names containing a space (`' ' in mymatch[1]`), removing each
/// match's text after consumption (`re.sub(r'', elemtext, count=1)`).
///
/// **Anti-inversion note**: the Python source indexes `mymatch[1]` even
/// when the regex has two alternation groups (as in `JSON_AUTHOR_1`),
/// which crashes when only group 2 matched. We mirror Python's lookup
/// order — prefer group 1, fall back to group 2 — and treat absent
/// captures as no-match (faithful behaviour on inputs that don't trip
/// the latent Python crash).
fn extract_json_author(elemtext: &str, regex: &Regex) -> Option<String> {
    let mut authors: Option<String> = None;
    let mut text = elemtext.to_string();
    while let Some(caps) = regex.captures(&text) {
        // Python: `mymatch[1]`. We prefer group 1; if it's absent (the
        // second alternation matched), fall back to group 2.
        let candidate = caps
            .get(1)
            .or_else(|| caps.get(2))
            .map(|m| m.as_str().to_string());
        // llvm-cov:branch-not-reachable: JSON_AUTHOR_1 is a two-alternative
        // pattern where each alternative carries one capture group (group 1 or
        // group 2), and JSON_AUTHOR_2 carries a single mandatory group — so any
        // successful `captures` populates at least one of group 1 / group 2.
        // The `None` (break) side cannot occur.
        let Some(candidate) = candidate else { break };
        // `while mymatch and ' ' in mymatch[1]` — only loop while the
        // candidate contains a space.
        if !candidate.contains(' ') {
            break;
        }
        // Python calls `normalize_authors(authors, mymatch[1])`. The
        // regex-rescue path is rare enough that the Stage 7b
        // `merge_author` semantic (URL/email reject + HTML-tag strip +
        // "; "-join dedup) suffices — same approximation
        // [`crate::trafilatura::metadata::normalize_authors_lite`] uses.
        let cleaned = normalize_json(&candidate);
        if !cleaned.is_empty() {
            authors = merge_author(authors.as_deref(), &cleaned);
        }
        // Consume the match (`re.sub(r'', elemtext, count=1)`).
        let mat = caps.get(0).expect("captures(0) on a successful match");
        let mut next = String::with_capacity(text.len() - (mat.end() - mat.start()));
        next.push_str(&text[..mat.start()]);
        next.push_str(&text[mat.end()..]);
        text = next;
    }
    authors
}

/// `normalize_json(string)` (`json_metadata.py:216-223`).
///
/// Decodes `\uXXXX` escapes, drops `\n` / `\r` / `\t` escape sequences,
/// strips lone surrogates (BMP D800..DFFF), HTML-unescapes (handled here
/// by `strip_simple_html_tags` for the tag form; the entity form is left
/// to `unescape` — we omit the full entity decoder for the regex-rescue
/// path, since the corpus does not exercise it), and trims.
fn normalize_json(s: &str) -> String {
    let processed = if s.contains('\\') {
        let mut t = s.replace("\\n", "").replace("\\r", "").replace("\\t", "");
        t = json_unicode_replace_re()
            .replace_all(&t, |caps: &regex::Captures| {
                let hex = &caps[1];
                // llvm-cov:branch-not-reachable: JSON_UNICODE_REPLACE captures
                // exactly 4 hex digits (`[0-9a-fA-F]{4}`), so the parsed value is
                // at most 0xFFFF — `u32::from_str_radix` on a 4-digit hex string
                // always succeeds. The `Err` (returning the empty fallback) side
                // cannot occur.
                if let Ok(cp) = u32::from_str_radix(hex, 16) {
                    if (0xD800..=0xDFFF).contains(&cp) {
                        // Lone surrogate — drop.
                        return String::new();
                    }
                    // llvm-cov:branch-not-reachable: the regex captures exactly
                    // 4 hex digits, so `cp <= 0xFFFF`; the surrogate range was
                    // already returned above; every remaining BMP scalar value
                    // is a valid `char`, so `char::from_u32` is always Some — the
                    // None side (returning the empty fallback below for this
                    // path) cannot occur.
                    if let Some(c) = char::from_u32(cp) {
                        return c.to_string();
                    }
                }
                String::new()
            })
            .into_owned();
        t
    } else {
        s.to_string()
    };
    let stripped = json_remove_html_re().replace_all(&processed, "");
    trim(&stripped)
}

/// `extract_json_parse_error(elem, metadata)` (`json_metadata.py:174-213`).
///
/// Crudely extracts metadata from malformed JSON-LD blocks. Mirrors the
/// Python ordering (author → pagetype → publisher → category → title)
/// and the early-exit shape (each section gates on a cheap substring
/// check before invoking its regex).
fn extract_json_parse_error(elem: &str, metadata: &mut Metadata) {
    // ── author info (`json_metadata.py:176-181`)
    let element_text_author = json_author_remove_re().replace_all(elem, "").into_owned();
    let author = extract_json_author(&element_text_author, json_author_1_re())
        .or_else(|| extract_json_author(&element_text_author, json_author_2_re()));
    // llvm-cov:branch-not-reachable: `extract_json_author` accumulates names
    // ONLY through `merge_author`, which returns a non-empty `Some` (or `None`),
    // so `author` is never `Some("")` — the `!a.is_empty()` FALSE side cannot
    // occur.
    if let Some(a) = author
        && !a.is_empty()
    {
        metadata.author = Some(a);
    }

    // ── pagetype (`json_metadata.py:183-189`)
    if elem.contains("@type")
        && let Some(caps) = json_type_re().captures(elem)
        // llvm-cov:branch-not-reachable: JSON_TYPE = `"@type"\s*:\s*"([^"]*)"`
        // has a single MANDATORY capture group, so any successful `captures`
        // populates group 1 — `caps.get(1)` is always Some here.
        && let Some(group) = caps.get(1)
    {
        let candidate = normalize_json(&group.as_str().to_ascii_lowercase());
        if JSON_OGTYPE_SCHEMA.contains(&candidate.as_str()) {
            metadata.pagetype = Some(candidate);
        }
    }

    // ── publisher (`json_metadata.py:191-197`)
    if elem.contains("\"publisher\"")
        && let Some(caps) = json_publisher_re().captures(elem)
        // llvm-cov:branch-not-reachable: JSON_PUBLISHER has a single MANDATORY
        // capture group `([^"\\]+)`, so a successful `captures` always
        // populates group 1.
        && let Some(group) = caps.get(1)
        && !group.as_str().contains(',')
    {
        let candidate = normalize_json(group.as_str());
        if is_plausible_sitename(metadata.site_name.as_deref(), &candidate, "") {
            metadata.site_name = Some(candidate);
        }
    }

    // ── category (`json_metadata.py:200-203`)
    if elem.contains("\"articleSection\"")
        && let Some(caps) = json_category_re().captures(elem)
        // llvm-cov:branch-not-reachable: JSON_CATEGORY has a single MANDATORY
        // capture group `([^"\\]+)`, so a successful `captures` always
        // populates group 1.
        && let Some(group) = caps.get(1)
    {
        let cleaned = normalize_json(group.as_str());
        if !cleaned.is_empty() {
            metadata.categories = vec![cleaned];
        }
    }

    // ── title (`json_metadata.py:206-211`). `JSON_SEQ` ordering:
    // (`"name"`, JSON_NAME), (`"headline"`, JSON_HEADLINE).
    if metadata.title.is_none() {
        for (key, regex) in [
            ("\"name\"", json_name_re()),
            ("\"headline\"", json_headline_re()),
        ] {
            if elem.contains(key)
                && let Some(caps) = regex.captures(elem)
                // llvm-cov:branch-not-reachable: both JSON_NAME and JSON_HEADLINE
                // have a single MANDATORY capture group `([^"\\]+)`, so a
                // successful `captures` always populates group 1.
                && let Some(group) = caps.get(1)
            {
                let cleaned = normalize_json(group.as_str());
                if !cleaned.is_empty() {
                    metadata.title = Some(cleaned);
                    break;
                }
            }
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    fn run(html: &str) -> Metadata {
        let dom = Dom::parse(html);
        let mut meta = Metadata::default();
        extract_meta_json(&dom, &mut meta);
        meta
    }

    #[test]
    fn jsonld_extracts_simple_article_metadata() {
        let html = r#"<html><head><script type="application/ld+json">
        {
          "@context": "https://schema.org",
          "@type": "NewsArticle",
          "headline": "Big News Today",
          "author": "Jane Doe",
          "datePublished": "2024-01-15"
        }
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Big News Today"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
        assert_eq!(m.date.as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn jsonld_handles_author_as_string() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article", "author": "Jane Doe"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn jsonld_handles_author_as_object() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": {"@type": "Person", "name": "Jane Doe"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn jsonld_handles_author_as_list() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": [{"name": "Jane"}, {"name": "Joe"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane; Joe"));
    }

    #[test]
    fn jsonld_handles_graph_wrapper() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@graph": [{"@type": "NewsArticle", "headline": "Graph Headline",
                     "author": "Graph Author"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Graph Headline"));
        assert_eq!(m.author.as_deref(), Some("Graph Author"));
    }

    #[test]
    fn jsonld_skips_invalid_json() {
        let html = r#"<html><head><script type="application/ld+json">
        not actually json at all
        </script></head><body></body></html>"#;
        // Must not panic and must not populate any field.
        let m = run(html);
        assert!(m.title.is_none());
        assert!(m.author.is_none());
    }

    #[test]
    fn jsonld_extracts_keywords_as_tags() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": "rust, web, html"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["rust", "web", "html"]);
    }

    #[test]
    fn jsonld_extracts_publisher_name() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "headline": "X",
         "publisher": {"@type": "Organization", "name": "Acme News"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Acme News"));
    }

    #[test]
    fn jsonld_extracts_date_published_to_date_field() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "datePublished": "2024-01-15"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.date.as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn jsonld_handles_multiple_scripts() {
        // First block: title only. Second block: author. The second
        // supplements the first (different fields, no collision).
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article", "headline": "T"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article", "author": "Jane Doe"}
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("T"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn jsonld_extracts_top_level_array() {
        // Some sites emit a top-level JSON ARRAY of objects.
        let html = r#"<html><head><script type="application/ld+json">
        [
          {"@context": "https://schema.org", "@type": "NewsArticle",
           "headline": "Array Headline"},
          {"@context": "https://schema.org", "@type": "Organization",
           "name": "Site Name"}
        ]
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Array Headline"));
        assert_eq!(m.site_name.as_deref(), Some("Site Name"));
    }

    #[test]
    fn jsonld_extracts_articlesection_to_categories() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "articleSection": "Technology"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Technology"]);
    }

    #[test]
    fn jsonld_pagetype_from_article_type() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "headline": "X"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.pagetype.as_deref(), Some("newsarticle"));
    }

    #[test]
    fn jsonld_image_object_url_field() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "headline": "X",
         "image": {"@type": "ImageObject", "url": "https://example.com/hero.jpg"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.image.as_deref(), Some("https://example.com/hero.jpg"));
    }

    #[test]
    fn jsonld_given_and_family_name() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "headline": "X",
         "author": {"@type": "Person",
                    "givenName": "Jane", "familyName": "Doe"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    // ─── M4 Stage 7: extract_json_parse_error regex-rescue tests ───────────
    //
    // Each test feeds intentionally MALFORMED JSON-LD into a script block
    // so `serde_json::from_str` returns `Err`, routing the input through
    // `extract_json_parse_error` (`json_metadata.py:174-213`). The trailing
    // garbage (e.g. an unterminated brace + stray `OOPS`) is what breaks the
    // JSON parser without disturbing the regex matchers.

    /// Brief test 1: author as Person object → recovers author.
    #[test]
    fn parse_error_recovers_person_author() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": {"@type": "Person", "name": "John Doe"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        // JSON_AUTHOR_2 ("[Pp]erson" branch) is what fires here, since
        // JSON_AUTHOR_REMOVE strips the "Person" object's wrapper-less
        // `"@type":"Person"` is preserved (the REMOVE regex targets only
        // ImageObject / Organization / WebPage). Result: author recovered.
        assert_eq!(m.author.as_deref(), Some("John Doe"));
    }

    /// Brief test 2: malformed JSON with headline → recovers title.
    #[test]
    fn parse_error_recovers_headline_as_title() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Article Title" OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Article Title"));
    }

    /// Brief test 3: malformed JSON with `"name"` (no headline) under
    /// `@type=Article` → recovers title via JSON_NAME.
    #[test]
    fn parse_error_recovers_name_as_title_when_no_headline() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type":"Article", "name": "Article Title" OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Article Title"));
    }

    /// Brief test 4: malformed JSON with publisher → recovers sitename.
    #[test]
    fn parse_error_recovers_publisher_as_sitename() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "publisher": {"@type": "Organization", "name": "Acme News"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Acme News"));
    }

    /// Brief test 5: well-formed JSON should NOT invoke the regex-rescue
    /// path — the standard `extract_json` walker handles it. Confirmed by
    /// the structural shape (no `"@type":"Article", "name":` pattern that
    /// JSON_NAME requires — `extract_json` uses `headline` instead).
    #[test]
    fn parse_error_not_invoked_on_well_formed_json() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "NewsArticle",
         "headline": "Article Title",
         "author": "Jane Doe"}
        </script></head><body></body></html>"#;
        let m = run(html);
        // Well-formed → walker path → title and author set.
        assert_eq!(m.title.as_deref(), Some("Article Title"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    /// Brief test 6: malformed JSON with neither author nor title →
    /// metadata unchanged. (We do allow other fields like `articleSection`
    /// to flow through — this test only pins title/author/site_name to
    /// `None` for an input that carries none of those.)
    #[test]
    fn parse_error_no_recoverable_fields_leaves_metadata_unchanged() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org" OOPS no recoverable fields here
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
        assert!(m.title.is_none());
        assert!(m.site_name.is_none());
    }

    /// Brief test 7: multiple JSON-LD blocks, one valid one malformed:
    /// both contribute to metadata.
    #[test]
    fn parse_error_mixed_blocks_each_contributes() {
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Valid Title"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": {"@type": "Person", "name": "Rescue Author"} OOPS
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        // Valid block populates title via the walker.
        assert_eq!(m.title.as_deref(), Some("Valid Title"));
        // Malformed block populates author via the regex-rescue path.
        assert_eq!(m.author.as_deref(), Some("Rescue Author"));
    }

    /// Brief test 8: edge case — whitespace-only / non-JSON garbage.
    /// Must not panic; metadata unchanged.
    #[test]
    fn parse_error_handles_garbage_input_without_panic() {
        let html = r#"<html><head><script type="application/ld+json">
        not actually anything resembling json or json-ld
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
        assert!(m.title.is_none());
        assert!(m.site_name.is_none());
        assert!(m.pagetype.is_none());
        assert!(m.categories.is_empty());
    }

    /// Additional pin: JSON_AUTHOR_1's `"author":..."name"` alternative
    /// fires when the input is a malformed author-object literal (not the
    /// Person-typed shape that JSON_AUTHOR_2 catches).
    #[test]
    fn parse_error_recovers_author_via_author_1_alternative() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": {"name": "Sole Author"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Sole Author"));
    }

    /// Additional pin: JSON_AUTHOR_REMOVE strips an ImageObject wrapper
    /// before author extraction, so a malformed payload mixing image +
    /// author still surfaces the author.
    #[test]
    fn parse_error_strips_imageobject_before_author_extraction() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Article",
         "image":{"@type":"ImageObject","url":"https://x/y.jpg","width":1200},
         "author":{"@type":"Person","name":"Robust Reporter"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Robust Reporter"));
    }

    // ─── Stage 2: JSON-LD shape catalog (M5/coverage push) ─────────────────
    //
    // Tests in this block drive the missed branches inside `walk_article`,
    // `extract_json`, `process_parent`, `extract_author_names`,
    // `extract_json_parse_error`, `is_plausible_sitename`, `merge_author`,
    // `normalize_json_string`, `strip_simple_html_tags`, `matches_schema_org`,
    // and `is_liveblog_with_updates`. Each names the contract it pins.
    //
    // Oracle: `trafilatura@v2.0.0/json_metadata.py:67-223` and the dispatch
    // in `metadata.py:182-195`.

    // ── extract_json top-level dispatch shapes ────────────────────────────

    /// `extract_json` (`json_metadata.py:143-144`): a root that is neither
    /// object nor array (here: bare string) must produce no metadata.
    /// rationale: faithful early-return on non-container roots, not a panic.
    #[test]
    fn jsonld_root_bare_string_yields_no_metadata() {
        let html = r#"<html><head><script type="application/ld+json">
        "just a string"
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
        assert!(m.author.is_none());
        assert!(m.site_name.is_none());
    }

    /// `extract_json` root = `null` triggers the `_ => return` arm.
    /// rationale: JSON null is a valid JSON value but not a schema.org
    /// container — must not surface anything (`json_metadata.py:143-144`).
    #[test]
    fn jsonld_root_null_yields_no_metadata() {
        let html = r#"<html><head><script type="application/ld+json">null</script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `extract_json` root = number triggers the `_ => return` arm.
    /// rationale: same as null — defensive guard on non-object roots.
    #[test]
    fn jsonld_root_number_yields_no_metadata() {
        let html = r#"<html><head><script type="application/ld+json">42</script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `extract_json` `@context` is non-schema.org URL → the explicit
    /// non-schema.org gate (`json_metadata.py:150` `if context and ...`)
    /// skips this entry. Rust port: `else { if context.is_none() ... }`.
    /// rationale: an explicit non-schema-org @context disables walking.
    #[test]
    fn jsonld_non_schema_org_explicit_context_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "http://example.org/ns",
         "@type": "Article", "headline": "Should Be Ignored",
         "author": "Should Also Be Ignored"}
        </script></head><body></body></html>"#;
        let m = run(html);
        // Non-schema.org context blocks the walker. Title/author stay None.
        assert!(m.title.is_none());
        assert!(m.author.is_none());
    }

    /// `extract_json` no `@context` present → faithful-tolerance widening
    /// processes the entry as a fallback.
    /// rationale: pins the documented widening (no @context still walks).
    #[test]
    fn jsonld_missing_context_still_walked_via_widening() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@type": "NewsArticle", "headline": "No Ctx Article",
         "author": "Jane Doe"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("No Ctx Article"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    /// `@graph` is a single object (not an array) under schema.org context.
    /// rationale: `json_metadata.py:151-152`'s `Object` branch of the
    /// `@graph` carrier — must walk it as a singleton.
    #[test]
    fn jsonld_graph_as_single_object() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@graph": {"@type": "NewsArticle",
                    "headline": "Single-Obj Graph",
                    "author": "Graph Solo"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Single-Obj Graph"));
        assert_eq!(m.author.as_deref(), Some("Graph Solo"));
    }

    /// `@graph` carrier is a non-container type (string) → ignored, parent
    /// not re-processed (falls through to no effective entries).
    /// rationale: defensive — schema.org @graph must be array/object.
    #[test]
    fn jsonld_graph_as_string_is_ignored() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@graph": "not a graph"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `@graph` resolution with multi-entity (Article + Organization +
    /// Person + WebPage) cascade — covers `process_parent`'s per-entity
    /// dispatch in one shot.
    /// rationale: pins the multi-entity dispatch ordering.
    #[test]
    fn jsonld_graph_multi_entity_article_org_person_webpage() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@graph": [
            {"@type": "WebPage", "name": "Page"},
            {"@type": "Organization", "name": "Pub Co"},
            {"@type": "Person", "name": "Top-Level Person"},
            {"@type": "NewsArticle", "headline": "The Headline",
             "author": "Article Author"}
         ]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("The Headline"));
        // `Pub Co` is longer than `Page` (the WebPage entry), so it overrides
        // per `is_plausible_sitename`.
        assert_eq!(m.site_name.as_deref(), Some("Pub Co"));
        // Both top-level Person and Article author merge via `merge_author`.
        let a = m.author.as_deref().unwrap();
        assert!(a.contains("Top-Level Person"), "got {a:?}");
        assert!(a.contains("Article Author"), "got {a:?}");
    }

    // ── liveblog carve-out (json_metadata.py:153-154) ──────────────────────

    /// liveblogposting carve-out: `liveBlogUpdate` as ARRAY of update entries
    /// is the effective parent list (`json_metadata.py:153-154`).
    /// rationale: pins the array branch of the liveblog `liveBlogUpdate`
    /// carrier shape.
    #[test]
    fn jsonld_liveblog_updates_as_array() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "LiveBlogPosting",
         "liveBlogUpdate": [
            {"@type": "BlogPosting", "headline": "Latest Update",
             "author": "Live Author"}
         ]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Latest Update"));
        assert_eq!(m.author.as_deref(), Some("Live Author"));
    }

    /// liveblogposting carve-out: `liveBlogUpdate` as a single Object.
    /// rationale: pins the Object branch of the carrier shape match.
    #[test]
    fn jsonld_liveblog_updates_as_single_object() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "LiveBlogPosting",
         "liveBlogUpdate": {"@type": "BlogPosting",
                            "headline": "Solo Update"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Solo Update"));
    }

    /// `@type` contains "liveblogposting" substring but no `liveBlogUpdate`
    /// key → `is_liveblog_with_updates` returns false; entry falls through
    /// to the standard parent processing path.
    /// rationale: pins `is_liveblog_with_updates`'s `&&` gate (both
    /// halves required) at `json_metadata.py:153`.
    #[test]
    fn jsonld_liveblog_no_updates_key_falls_back_to_default_walker() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "LiveBlogPosting",
         "headline": "LB With No Updates",
         "author": "LB Writer"}
        </script></head><body></body></html>"#;
        let m = run(html);
        // Without liveBlogUpdate present, the default path walks the parent
        // as Article-like (LiveBlogPosting is in JSON_ARTICLE_SCHEMA).
        assert_eq!(m.title.as_deref(), Some("LB With No Updates"));
        assert_eq!(m.author.as_deref(), Some("LB Writer"));
    }

    // ── process_parent edge shapes ─────────────────────────────────────────

    /// `process_parent`: an entry whose `@type` is an Object (not str/array)
    /// is skipped (`json_metadata.py:74-79` Python `isinstance(..., (str,
    /// list))` filter). Rust port: `_ => continue`.
    /// rationale: defensive type filter.
    #[test]
    fn jsonld_at_type_as_object_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": {"weird": "shape"},
         "headline": "Should Not Surface"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `process_parent`: a missing `@type` key is silently skipped — the
    /// `continue` arm of the `Option::None` match.
    /// rationale: `json_metadata.py:74` `if "@type" in content` gate.
    #[test]
    fn jsonld_missing_at_type_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "headline": "Should Not Be Title"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `process_parent`: an entry whose `@type` is an empty array → coerces
    /// to no first element and is skipped (the inner `None => continue`).
    /// rationale: `json_metadata.py:76` `parent.get("@type")[0]` first-elem
    /// access on an empty list raises IndexError in Python; we faithfully
    /// skip it instead of panicking.
    #[test]
    fn jsonld_at_type_empty_array_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": [],
         "headline": "Empty-Type Array"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `process_parent`: an entry whose `@type` is `[non-string-value]`
    /// → also skipped (the inner `None` after `arr.first().and_then(as_str)`).
    /// rationale: hardens the @type-array→first-string coercion.
    #[test]
    fn jsonld_at_type_array_first_non_string_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": [42],
         "headline": "Wonky Type Array"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `process_parent`: `@type` as a LIST of strings — uses the first
    /// (`json_metadata.py:76-77`).
    /// rationale: pins the `Array` arm of `@type` coercion when the first
    /// item IS a string (the happy half).
    #[test]
    fn jsonld_at_type_as_list_of_strings_uses_first() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": ["NewsArticle", "BlogPosting"],
         "headline": "Multi-Type Article"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Multi-Type Article"));
        // pagetype derived from first element of the @type list.
        assert_eq!(m.pagetype.as_deref(), Some("newsarticle"));
    }

    /// `process_parent`: publisher present but `name` is NOT a string
    /// (e.g. nested object). The early `and_then(Value::as_str)` returns
    /// None → no site_name updated.
    /// rationale: defensive against `publisher.name` of unexpected shape.
    #[test]
    fn jsonld_publisher_name_non_string_ignored() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "X",
         "publisher": {"name": {"nested": "shape"}}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none());
    }

    /// `process_parent`: publisher with name = empty string → cleaned is
    /// empty → the `!cleaned.is_empty()` gate stops the assignment.
    /// rationale: pins the empty-name early-return.
    #[test]
    fn jsonld_publisher_empty_name_does_not_overwrite() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "X",
         "publisher": {"name": ""}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none());
    }

    /// Publisher schema lookup: `name` absent, `legalName` present → uses
    /// legalName (`json_metadata.py:85-88`).
    /// rationale: pins the legalName fallback.
    #[test]
    fn jsonld_publisher_legalname_fallback() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "Organization",
         "legalName": "Big Media Co Ltd"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Big Media Co Ltd"));
    }

    /// Publisher schema lookup: `name` and `legalName` both absent,
    /// `alternateName` present → uses alternateName.
    /// rationale: pins the final fallback in the OR chain.
    #[test]
    fn jsonld_publisher_alternatename_final_fallback() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "Organization",
         "alternateName": "Alt Co"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Alt Co"));
    }

    /// Person at top level with `name` starting with "http" → skipped
    /// (`json_metadata.py:90-92` `if not name.startswith("http")`).
    /// rationale: pins the URL-as-name reject in the Person branch.
    #[test]
    fn jsonld_person_top_level_with_url_name_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "Person",
         "name": "https://example.com/profile/jane"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// Person at top level with regular `name` → populates author.
    /// rationale: pins the happy path of the Person branch
    /// (`json_metadata.py:90-92`).
    #[test]
    fn jsonld_person_top_level_writes_author() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "Person", "name": "Top Person"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Top Person"));
    }

    /// `process_parent`: non-object entry inside an array (here a string)
    /// is silently skipped.
    /// rationale: pins the `None => continue` of `content.as_object()`.
    #[test]
    fn jsonld_non_object_array_entries_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        ["just a string",
         {"@context": "https://schema.org",
          "@type": "Article", "headline": "Real One"}]
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Real One"));
    }

    // ── walk_article shape catalog ─────────────────────────────────────────

    /// `walk_article` — `articleSection` as an ARRAY of strings appends each
    /// entry to `categories` (`json_metadata.py:128-130`).
    /// rationale: pins the `Value::Array` arm of articleSection.
    #[test]
    fn jsonld_article_section_as_array_of_strings() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": ["Tech", "Science", "Policy"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Tech", "Science", "Policy"]);
    }

    /// `walk_article` — `articleSection` array entries that aren't strings
    /// are silently filtered (no panic).
    /// rationale: pins the `as_str()` filter inside the Array arm.
    #[test]
    fn jsonld_article_section_array_skips_non_strings() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": ["Real", 42, null, "Also Real"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Real", "Also Real"]);
    }

    /// `walk_article` — `articleSection` of `Value::Number` falls through
    /// the `_ => {}` arm; categories stay empty.
    /// rationale: defensive — non-string/list shapes are no-ops.
    #[test]
    fn jsonld_article_section_number_is_noop() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": 42}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.categories.is_empty());
    }

    /// `walk_article` — `articleSection` is processed ONLY if categories is
    /// still empty (the `if metadata.categories.is_empty()` gate). Two
    /// JSON-LD blocks with sections: the second is ignored.
    /// rationale: pins the categories-empty gate ordering at
    /// `json_metadata.py:126`.
    #[test]
    fn jsonld_article_section_skipped_when_already_set() {
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": "First Cat"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": "Second Cat"}
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["First Cat"]);
    }

    /// `walk_article` — `keywords` as ARRAY appends each entry to `tags`.
    /// rationale: pins the Array arm of the keywords carrier (additive
    /// faithful extension noted in the module header).
    #[test]
    fn jsonld_keywords_as_array() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": ["rust", "json", "ld"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["rust", "json", "ld"]);
    }

    /// `walk_article` — keywords with empty entries in comma-string get
    /// filtered out by `!cleaned.is_empty()` after `trim`.
    /// rationale: pins the empty-trim filter inside the comma-split branch.
    #[test]
    fn jsonld_keywords_comma_string_skips_blank_parts() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": "rust, , json,  "}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["rust", "json"]);
    }

    /// `walk_article` — keywords as non-string/non-array (Number) is a no-op.
    /// rationale: pins the `_ => {}` keywords arm.
    #[test]
    fn jsonld_keywords_non_supported_carrier_is_noop() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": 12345}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.tags.is_empty());
    }

    /// `walk_article` — `name` is used as title only when `headline` is
    /// absent (`json_metadata.py:135-137`).
    /// rationale: pins the `else if` cascade in title resolution.
    #[test]
    fn jsonld_title_falls_back_to_name_when_no_headline() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "name": "Name-Only Title"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Name-Only Title"));
    }

    /// `walk_article` — `headline` precedence over `name` when both
    /// present (`json_metadata.py:132-137`).
    /// rationale: pins the `if let Some(headline)` first arm.
    #[test]
    fn jsonld_headline_beats_name_in_title_resolution() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Headline Wins",
         "name": "Name Loses"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Headline Wins"));
    }

    /// `walk_article` — title not overwritten when already set.
    /// rationale: pins the `if metadata.title.is_none()` gate.
    #[test]
    fn jsonld_walk_article_does_not_overwrite_existing_title() {
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "First Title"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Second Title"}
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("First Title"));
    }

    /// `walk_article` — `dateModified` is used only when `datePublished` is
    /// absent.
    /// rationale: pins the `else if` date fallback path.
    #[test]
    fn jsonld_date_modified_used_when_no_published() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "dateModified": "2024-12-31"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.date.as_deref(), Some("2024-12-31"));
    }

    /// `walk_article` — `datePublished` beats `dateModified` when both
    /// present.
    /// rationale: pins the `if let` order: published first, modified
    /// second.
    #[test]
    fn jsonld_date_published_beats_date_modified() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "datePublished": "2024-01-01",
         "dateModified": "2024-12-31"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.date.as_deref(), Some("2024-01-01"));
    }

    /// `walk_article` — date already set by a prior block is not
    /// overwritten.
    /// rationale: pins `if metadata.date.is_none()` gate.
    #[test]
    fn jsonld_walk_article_does_not_overwrite_existing_date() {
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "First", "datePublished": "2024-01-01"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Second", "datePublished": "2024-06-15"}
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.date.as_deref(), Some("2024-01-01"));
    }

    /// `walk_article` — `image` as a bare string (not an ImageObject) is
    /// accepted.
    /// rationale: pins the `Value::String` arm of the image carrier.
    #[test]
    fn jsonld_image_as_bare_string() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "image": "https://example.com/img.jpg"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.image.as_deref(), Some("https://example.com/img.jpg"));
    }

    /// `walk_article` — `image` as an array whose FIRST element is a bare
    /// string.
    /// rationale: pins the array→first→Value::String inner arm.
    #[test]
    fn jsonld_image_as_array_first_string() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "image": ["https://example.com/a.jpg",
                   "https://example.com/b.jpg"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.image.as_deref(), Some("https://example.com/a.jpg"));
    }

    /// `walk_article` — `image` array first elem is an ImageObject with
    /// `url`.
    /// rationale: pins the array→first→Object→url inner arm.
    #[test]
    fn jsonld_image_as_array_first_object_with_url() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "image": [{"@type": "ImageObject", "url": "https://x/y.jpg"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.image.as_deref(), Some("https://x/y.jpg"));
    }

    /// `walk_article` — `image` as ImageObject without `url` → None
    /// candidate, image stays unset.
    /// rationale: pins the `Object` branch returning None when url missing.
    #[test]
    fn jsonld_image_object_without_url_yields_no_image() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "image": {"@type": "ImageObject", "caption": "no url here"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.image.is_none());
    }

    /// `walk_article` — `image` as empty array → `arr.first()` is None →
    /// no image.
    /// rationale: pins the empty-array branch of the Array image arm.
    #[test]
    fn jsonld_image_empty_array_yields_no_image() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "image": []}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.image.is_none());
    }

    /// `walk_article` — `image` as a Number → `_ => None` outer arm.
    /// rationale: defensive type guard on the image carrier.
    #[test]
    fn jsonld_image_as_number_is_noop() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "image": 42}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.image.is_none());
    }

    /// `walk_article` — image already set is not overwritten.
    /// rationale: pins the `if metadata.image.is_none()` gate.
    #[test]
    fn jsonld_walk_article_does_not_overwrite_existing_image() {
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "image": "https://first/a.jpg"}
        </script>
        <script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "y", "image": "https://second/b.jpg"}
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.image.as_deref(), Some("https://first/a.jpg"));
    }

    /// `walk_article` — `image` array first elem is non-string/non-object
    /// (here a Number) → inner `_ => None`.
    /// rationale: pins the inner `_ => None` arm of the array element
    /// dispatch.
    #[test]
    fn jsonld_image_array_first_non_supported_is_noop() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "image": [42]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.image.is_none());
    }

    // ── extract_author_names shape catalog ────────────────────────────────

    /// `extract_author_names` — string-valued author whose VALUE happens to
    /// be JSON-parseable as an object → recurse into it
    /// (`json_metadata.py:98-104`).
    /// rationale: pins the `json.loads(s)` Python rescue path.
    #[test]
    fn jsonld_author_string_is_recursively_parsed_as_object() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": "{\"@type\": \"Person\", \"name\": \"Embedded Author\"}"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Embedded Author"));
    }

    /// `extract_author_names` — string-valued author that parses to an
    /// ARRAY (`json.loads("[{...}]")` rescue) recurses on the array.
    /// rationale: pins the `is_array()` branch of the inner parse rescue.
    #[test]
    fn jsonld_author_string_is_recursively_parsed_as_array() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": "[{\"name\":\"A One\"},{\"name\":\"B Two\"}]"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("A One; B Two"));
    }

    /// `extract_author_names` — string-valued author that parses to a
    /// primitive (e.g. a number) is treated as a plain name string,
    /// reaching `normalize_json_string("42")` → `"42"`.
    /// rationale: pins the fallback from "inner parse succeeded but not
    /// container" through to the literal-string path.
    #[test]
    fn jsonld_author_string_parses_to_primitive_then_treated_as_name() {
        // The string "42" parses to JSON number 42 (not object/array).
        // The rescue branch then falls through to the literal-string
        // handler, which yields "42" as a name.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "author": "42"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("42"));
    }

    /// `extract_author_names` — string-valued author that is empty after
    /// `normalize_json_string` (here: only HTML tags) yields no name.
    /// rationale: pins the `!cleaned.is_empty()` filter on string-author.
    #[test]
    fn jsonld_author_empty_after_tag_strip_yields_no_author() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "author": "<span></span>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — author value is a Number → `_ => return
    /// out` arm with `out` empty.
    /// rationale: pins the catch-all in the carrier match.
    #[test]
    fn jsonld_author_as_number_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "author": 12345}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — list-of-strings as author shape: each
    /// non-object entry is skipped by `author.as_object()` → `None =>
    /// continue`.
    /// rationale: pins the per-item object-only filter (a list of bare
    /// strings is faithfully ignored — Python iterates dicts; bare strings
    /// have no `.get("name")`).
    #[test]
    fn jsonld_author_list_of_strings_is_filtered() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "author": ["Loose A", "Loose B"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        // Bare strings inside the author list have no .name key; the
        // per-item dispatch skips them.
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — list of MIXED objects + strings: strings
    /// are skipped, objects with name are accepted.
    /// rationale: pins the per-item filter when ONE side is valid.
    #[test]
    fn jsonld_author_list_of_mixed_keeps_valid_objects() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": ["Loose String", {"name": "Valid Author"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Valid Author"));
    }

    /// `extract_author_names` — author object with `@type` that is NEITHER
    /// "Person" nor missing (e.g. "Organization") is filtered out by the
    /// `type_ok` gate (`json_metadata.py:110`).
    /// rationale: pins the `Some(Value::String(s)) => s == "Person"` arm
    /// when s != "Person".
    #[test]
    fn jsonld_author_object_with_non_person_type_filtered() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Organization", "name": "Org Author"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — author object with `@type` of non-string
    /// value (e.g. array) is filtered out.
    /// rationale: pins the `_ => false` arm of the @type filter.
    #[test]
    fn jsonld_author_object_with_array_type_filtered() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": ["Person", "Author"], "name": "Multi-Type"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        // @type as array fails type_ok (only String "Person" passes).
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — author object with `name` as ARRAY of
    /// strings is joined with "; " (`json_metadata.py:113-118`).
    /// rationale: pins the Array arm of the name carrier.
    #[test]
    fn jsonld_author_name_as_array_joined_with_semicolons() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person",
                    "name": ["Jane Doe", "Jr."]}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Doe; Jr."));
    }

    /// `extract_author_names` — author object with `name` as EMPTY array
    /// (`json_metadata.py:118`) → `None` → skipped.
    /// rationale: pins the joined-list empty path.
    #[test]
    fn jsonld_author_name_as_empty_array_yields_no_author() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person", "name": []}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — author object with `name` as a NESTED dict
    /// reaches the `o.get("name")` branch (`json_metadata.py:115-118`).
    /// rationale: pins the `Object` arm of the name carrier.
    #[test]
    fn jsonld_author_name_as_nested_object_with_name_field() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person",
                    "name": {"name": "Inner Name"}}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Inner Name"));
    }

    /// `extract_author_names` — author object with `name` of Number type
    /// falls to `_ => None`.
    /// rationale: pins the catch-all of the name carrier shape.
    #[test]
    fn jsonld_author_name_as_number_is_none() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person", "name": 42}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — author with `givenName` only (no
    /// familyName) → the dual-key gate fails and yields no name.
    /// rationale: pins the `&&` gate in the givenName/familyName fallback.
    #[test]
    fn jsonld_author_given_name_only_yields_no_author() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person", "givenName": "Solo"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — givenName + additionalName + familyName
    /// joined with spaces (`json_metadata.py:119-120`).
    /// rationale: pins the AUTHOR_ATTRS triplet join (with middle name).
    #[test]
    fn jsonld_author_given_additional_family_joined_with_spaces() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person",
                    "givenName": "Jane",
                    "additionalName": "Q",
                    "familyName": "Doe"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Q Doe"));
    }

    /// `extract_author_names` — author object with neither `name` nor
    /// givenName/familyName → final `else => None`.
    /// rationale: pins the all-keys-absent fallthrough.
    #[test]
    fn jsonld_author_object_with_no_recognised_keys_skipped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"@type": "Person", "url": "https://x"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `extract_author_names` — multiple Person entries in a list whose
    /// names are dedup'd by `merge_author`.
    /// rationale: pins the dedup arm of `merge_author` reached via the
    /// author-walker.
    #[test]
    fn jsonld_author_list_with_duplicates_is_deduped() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": [{"name": "Jane Doe"}, {"name": "Jane Doe"},
                    {"name": "Joe Bloggs"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Jane Doe; Joe Bloggs"));
    }

    /// `merge_author` — candidate that is itself a URL is rejected.
    /// rationale: pins the `starts_with("http")` reject in merge_author,
    /// reached via a Person object's name field.
    #[test]
    fn jsonld_author_string_url_is_rejected_by_merge() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x",
         "author": {"name": "https://example.com/jane"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    /// `merge_author` — candidate containing `@` (email) is rejected.
    /// rationale: pins the `'@'` reject in merge_author.
    #[test]
    fn jsonld_author_email_address_is_rejected_by_merge() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "x", "author": "jane@example.com"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none());
    }

    // ── Direct unit tests on the small helpers ────────────────────────────

    /// `matches_schema_org` — None returns false (`json_metadata.py:27`).
    /// rationale: pins the None arm.
    #[test]
    fn matches_schema_org_none_is_false() {
        assert!(!matches_schema_org(None));
    }

    /// `matches_schema_org` — http and https variants both accepted, with
    /// or without trailing slash; non-schema-org rejected.
    /// rationale: pins both halves of the `||` predicate.
    #[test]
    fn matches_schema_org_http_and_https_accepted() {
        assert!(matches_schema_org(Some("http://schema.org")));
        assert!(matches_schema_org(Some("https://schema.org/")));
        // Case insensitive.
        assert!(matches_schema_org(Some("HTTP://Schema.org")));
        // Not schema.org.
        assert!(!matches_schema_org(Some("http://example.com")));
        // schema.org without scheme also rejected.
        assert!(!matches_schema_org(Some("schema.org")));
    }

    /// `is_plausible_sitename` — empty candidate always rejected.
    /// rationale: pins the `candidate.is_empty()` guard.
    #[test]
    fn is_plausible_sitename_empty_candidate_rejected() {
        assert!(!is_plausible_sitename(Some("Existing"), "", "organization"));
        assert!(!is_plausible_sitename(None, "", "organization"));
    }

    /// `is_plausible_sitename` — None / empty current always accepts.
    /// rationale: pins the `None`/`Some("")` accept arms.
    #[test]
    fn is_plausible_sitename_no_or_empty_current_accepts() {
        assert!(is_plausible_sitename(None, "New", "organization"));
        assert!(is_plausible_sitename(Some(""), "New", "organization"));
    }

    /// `is_plausible_sitename` — current is http://… and candidate is not
    /// http-prefixed → accept the candidate (preferred over a URL).
    /// rationale: pins the `current.starts_with("http") && !candidate
    /// .starts_with("http")` arm.
    #[test]
    fn is_plausible_sitename_url_current_replaced_by_name_candidate() {
        assert!(is_plausible_sitename(
            Some("https://example.com/site"),
            "Better Name",
            "organization"
        ));
    }

    /// `is_plausible_sitename` — content_type "webpage" never overrides a
    /// non-empty current (`json_metadata.py:63-64`).
    /// rationale: pins the `content_type != "webpage"` arm of the length
    /// compare.
    #[test]
    fn is_plausible_sitename_webpage_type_does_not_overwrite() {
        assert!(!is_plausible_sitename(
            Some("Short"),
            "A Much Longer Candidate Name",
            "webpage"
        ));
    }

    /// `is_plausible_sitename` — current is longer than candidate → reject
    /// (length rule).
    /// rationale: pins the negative branch of `c.chars().count()
    /// < candidate.chars().count()`.
    #[test]
    fn is_plausible_sitename_shorter_candidate_rejected() {
        assert!(!is_plausible_sitename(
            Some("An Already Long Name"),
            "Short",
            "organization"
        ));
    }

    /// `is_plausible_sitename` — current is shorter than candidate AND
    /// content_type != webpage → accept.
    /// rationale: pins the positive branch of the length compare.
    #[test]
    fn is_plausible_sitename_longer_candidate_accepted() {
        assert!(is_plausible_sitename(
            Some("Short"),
            "A Longer Name",
            "organization"
        ));
    }

    /// `merge_author` — None current + valid candidate → init.
    /// rationale: pins the `_ => Some(trimmed)` arm.
    #[test]
    fn merge_author_initializes_from_none() {
        let r = merge_author(None, "Jane Doe");
        assert_eq!(r.as_deref(), Some("Jane Doe"));
    }

    /// `merge_author` — empty-string current + new candidate → init.
    /// rationale: pins the `current.is_empty()` branch of the match guard.
    #[test]
    fn merge_author_initializes_from_empty_string() {
        let r = merge_author(Some(""), "Jane Doe");
        assert_eq!(r.as_deref(), Some("Jane Doe"));
    }

    /// `merge_author` — candidate trimmed to empty → keep current.
    /// rationale: pins the `trimmed.is_empty()` reject.
    #[test]
    fn merge_author_blank_candidate_preserves_current() {
        let r = merge_author(Some("Jane Doe"), "   ");
        assert_eq!(r.as_deref(), Some("Jane Doe"));
    }

    /// `merge_author` — URL-candidate preserves current None.
    /// rationale: pins the URL-reject path returning the existing None.
    #[test]
    fn merge_author_url_candidate_with_none_current_returns_none() {
        assert!(merge_author(None, "http://example.com").is_none());
    }

    /// `merge_author` — same name twice → dedup; current returned
    /// unchanged.
    /// rationale: pins the dedup `already.iter().any(...)` arm.
    #[test]
    fn merge_author_dedupes_exact_match() {
        let r = merge_author(Some("Jane Doe"), "Jane Doe");
        assert_eq!(r.as_deref(), Some("Jane Doe"));
    }

    /// `merge_author` — append distinct candidates with "; ".
    /// rationale: pins the format!("{c}; {trimmed}") branch.
    #[test]
    fn merge_author_appends_distinct_with_semicolon() {
        let r = merge_author(Some("Jane Doe"), "Joe Smith");
        assert_eq!(r.as_deref(), Some("Jane Doe; Joe Smith"));
    }

    /// `strip_simple_html_tags` — basic tag strip.
    /// rationale: pins the standard `<...>` → `""` consumption.
    #[test]
    fn strip_simple_html_tags_removes_basic_tags() {
        assert_eq!(strip_simple_html_tags("hello <b>bold</b> world"), "hello bold world");
    }

    /// `strip_simple_html_tags` — unclosed tag (no `>`) is preserved
    /// verbatim.
    /// rationale: pins the `!closed` branch where `out.push_str(&buf)`.
    #[test]
    fn strip_simple_html_tags_unclosed_tag_preserved() {
        assert_eq!(strip_simple_html_tags("hello <unfinished"), "hello <unfinished");
    }

    /// `strip_simple_html_tags` — nested `<` resets the inner buffer (the
    /// `<` inside an opening "tag" terminates the consumption loop).
    /// rationale: pins the `if next == '<' { break; }` branch.
    #[test]
    fn strip_simple_html_tags_nested_lt_does_not_swallow_text() {
        // Inner `<` triggers `break` on the inner loop; outer loop catches
        // the `<` again as a new tag start. With "abc<x<y>z": first `<x`
        // is preserved (because the inner `<` aborted, no closing `>`),
        // then `<y>` is consumed as a tag.
        let r = strip_simple_html_tags("a<x<y>b");
        assert_eq!(r, "a<xb");
    }

    /// `is_liveblog_with_updates` — @type missing → false.
    /// rationale: pins the `unwrap_or(false)` arm.
    #[test]
    fn is_liveblog_with_updates_no_type_false() {
        let v = serde_json::json!({"liveBlogUpdate": []});
        assert!(!is_liveblog_with_updates(&v));
    }

    /// `is_liveblog_with_updates` — @type missing the substring → false.
    /// rationale: pins the `.contains("liveblogposting")` negative half.
    #[test]
    fn is_liveblog_with_updates_wrong_type_false() {
        let v = serde_json::json!({"@type": "NewsArticle",
                                    "liveBlogUpdate": []});
        assert!(!is_liveblog_with_updates(&v));
    }

    /// `is_liveblog_with_updates` — @type matches but liveBlogUpdate key
    /// missing → false (the `&&` second half fails).
    /// rationale: pins the `parent.get("liveBlogUpdate").is_some()` guard.
    #[test]
    fn is_liveblog_with_updates_no_updates_key_false() {
        let v = serde_json::json!({"@type": "LiveBlogPosting"});
        assert!(!is_liveblog_with_updates(&v));
    }

    /// `is_liveblog_with_updates` — both halves true → true.
    /// rationale: pins the happy path of the `&&` predicate.
    #[test]
    fn is_liveblog_with_updates_both_halves_true() {
        let v = serde_json::json!({"@type": "LiveBlogPosting",
                                    "liveBlogUpdate": []});
        assert!(is_liveblog_with_updates(&v));
    }

    /// `normalize_json_string` — combines tag-strip + trim.
    /// rationale: pins the cleaned-output of the helper used throughout
    /// the walker.
    #[test]
    fn normalize_json_string_strips_tags_and_trims() {
        assert_eq!(normalize_json_string("  <b>Hi</b> there  "), "Hi there");
    }

    // ── extract_json_parse_error: additional shape coverage ───────────────

    /// `extract_json_parse_error` — malformed JSON containing
    /// `"articleSection"` recovers a category.
    /// rationale: pins the `elem.contains("\"articleSection\"")` →
    /// `json_category_re` capture arm.
    #[test]
    fn parse_error_recovers_article_section_as_category() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "articleSection": "Politics" OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Politics"]);
    }

    /// `extract_json_parse_error` — pagetype is set ONLY when @type matches
    /// the OGType list (`json_metadata.py:183-189`). A `@type` that is NOT
    /// in JSON_OGTYPE_SCHEMA (here: "Recipe") is silently dropped.
    /// rationale: pins the `.contains(&candidate.as_str())` negative half.
    #[test]
    fn parse_error_pagetype_non_ogtype_not_assigned() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Recipe" OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.pagetype.is_none());
    }

    /// `extract_json_parse_error` — pagetype matches OGType list (here:
    /// "NewsArticle" → lowered → "newsarticle") → assigned.
    /// rationale: pins the positive half of the pagetype branch.
    #[test]
    fn parse_error_pagetype_ogtype_match_assigned() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "author": {"@type":"Person","name":"Some Author"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.pagetype.as_deref(), Some("newsarticle"));
    }

    /// `extract_json_parse_error` — publisher group containing a comma is
    /// rejected (`json_metadata.py:194` `if "," not in ...`).
    /// rationale: pins the `!group.as_str().contains(',')` guard.
    #[test]
    fn parse_error_publisher_with_comma_rejected() {
        // The regex captures up to the next `"`, so a `,` lurking
        // before the next quote (the regex `[^"\\]+` accepts commas inside
        // the name) gets caught by the `!contains(',')` guard.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "publisher": {"name":"Acme, News, Inc"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none());
    }

    /// `extract_json_parse_error` — title already set is not overwritten.
    /// rationale: pins the `metadata.title.is_none()` gate on the
    /// title-rescue loop.
    #[test]
    fn parse_error_does_not_overwrite_existing_title() {
        // First block: valid JSON sets title. Second block: malformed JSON
        // with a different headline. The second must NOT overwrite.
        let html = r#"<html><head>
        <script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Article",
         "headline":"First Set"}
        </script>
        <script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Article",
         "headline":"Second Should Not Win" OOPS
        </script>
        </head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("First Set"));
    }

    /// `extract_json_parse_error` — input contains neither `"name"` nor
    /// `"headline"` substrings → both inner `contains` gates short-circuit;
    /// no title recovered.
    /// rationale: pins the substring-gate short-circuits in the title
    /// rescue loop.
    #[test]
    fn parse_error_no_name_or_headline_keys_leaves_title_none() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"Article",
         "datePublished":"2024-01-15" OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    /// `extract_json_parse_error` — JSON with single quotes (JS-style)
    /// fails serde_json and falls through to the regex rescue.
    /// rationale: pins the dispatch from `extract_meta_json`'s `Err`
    /// branch into `extract_json_parse_error`, with single-quote JSON.
    #[test]
    fn parse_error_handles_single_quoted_json() {
        // serde_json rejects single quotes outright. But our regexes
        // require double-quoted keys, so this resulting text recovers
        // nothing — confirms that the path is exercised and does NOT
        // panic.
        let html = r#"<html><head><script type="application/ld+json">
        {'@type':'Article','headline':'wont match'}
        </script></head><body></body></html>"#;
        let m = run(html);
        // No double-quoted "headline" → no recovery; metadata unchanged.
        assert!(m.title.is_none());
    }

    /// `extract_json_parse_error` — unterminated string still allows the
    /// regex rescue to surface a publisher when present earlier.
    /// rationale: pins the resilience-to-truncation contract.
    #[test]
    fn parse_error_unterminated_string_still_surfaces_publisher() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "publisher": {"name":"Solid Pub"}, "headline": "trunc
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Solid Pub"));
    }

    /// `extract_json_parse_error` — JS-style comments inside JSON break
    /// serde and route to rescue; an `authorSection` capture survives.
    /// rationale: pins comment-tolerant rescue (no panic).
    #[test]
    fn parse_error_with_js_comments_still_recovers_section() {
        let html = r#"<html><head><script type="application/ld+json">
        // JS comments invalid in JSON
        {"@context":"https://schema.org","@type":"NewsArticle",
         "articleSection": "Sports"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Sports"]);
    }

    // ── Aggregate / cross-cutting tests ───────────────────────────────────

    /// `walk_article` end-to-end with every recognised polymorphic field
    /// shape, asserting all slots populate.
    /// rationale: regression pin against silent loss of any single field
    /// in `walk_article`.
    #[test]
    fn jsonld_full_article_populates_every_slot() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@type": "NewsArticle",
         "headline": "Full Article",
         "author": [{"@type":"Person","name":"Jane Doe"},
                    {"@type":"Person","name":"Joe Smith"}],
         "articleSection": ["Tech", "Policy"],
         "keywords": ["a", "b", "c"],
         "datePublished": "2024-03-04",
         "image": {"@type": "ImageObject",
                   "url": "https://example.com/hero.jpg"},
         "publisher": {"@type": "Organization",
                       "name": "Daily News"}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Full Article"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe; Joe Smith"));
        assert_eq!(m.categories, vec!["Tech", "Policy"]);
        assert_eq!(m.tags, vec!["a", "b", "c"]);
        assert_eq!(m.date.as_deref(), Some("2024-03-04"));
        assert_eq!(m.image.as_deref(), Some("https://example.com/hero.jpg"));
        assert_eq!(m.site_name.as_deref(), Some("Daily News"));
        assert_eq!(m.pagetype.as_deref(), Some("newsarticle"));
    }

    /// `extract_json` (`json_metadata.py:151-152`) — `@graph` is NOT
    /// present and `@context` matches → falls through to the
    /// `else { effective.push(parent); }` fallback (process the parent as
    /// a single article).
    /// rationale: pins the no-@graph-no-liveblog fallback at
    /// `json_metadata.py:155-156`.
    #[test]
    fn jsonld_schema_org_context_without_graph_walks_parent_directly() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "Direct Parent",
         "author": "Single Person"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.title.as_deref(), Some("Direct Parent"));
        assert_eq!(m.author.as_deref(), Some("Single Person"));
    }

    /// `extract_json` — `@graph` value is a `Value::Null` (neither array
    /// nor object) → effective stays empty, process_parent runs with [].
    /// rationale: pins the third arm `_ => {}` of the @graph dispatch.
    #[test]
    fn jsonld_graph_as_null_yields_nothing() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@graph": null}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none());
    }

    // ── extract_meta_json entry-point edges ───────────────────────────────

    /// rationale: `metadata.py:177` — a whitespace-only `<script ld+json>` body
    /// is skipped (`if raw_text.trim().is_empty(): continue`), leaving metadata
    /// untouched.
    #[test]
    fn jsonld_whitespace_only_script_is_skipped() {
        let html = r#"<html><head><script type="application/ld+json">

        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none() && m.author.is_none());
    }

    /// rationale: the script selector matches but the element has no text child
    /// (`element_text` returns None) — the `let Some(raw_text) ... else continue`
    /// arm. A self-shaped empty script contributes nothing.
    #[test]
    fn jsonld_empty_script_element_is_skipped() {
        let html = r#"<html><head>
            <script type="application/ld+json"></script>
            </head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none() && m.author.is_none());
    }

    /// rationale: a document with no `<html>` root element (a bare text
    /// fragment) hits the `let Some(root) = dom.root_element() else return`
    /// guard — extract_meta_json is a no-op.
    #[test]
    fn jsonld_no_root_element_is_noop() {
        let dom = Dom::parse("just a bare text fragment, no elements");
        let mut meta = Metadata::default();
        extract_meta_json(&dom, &mut meta);
        assert!(meta.title.is_none() && meta.author.is_none());
    }

    // ── normalize_json (the \uXXXX-decoding variant) direct coverage ──────

    /// rationale: `json_metadata.py:218-220` — when the string contains a
    /// backslash, `\n`/`\r`/`\t` escape sequences are stripped and `\uXXXX`
    /// escapes are decoded to the literal code point. (This is the regex-rescue
    /// `normalize_json`, distinct from the post-parse `normalize_json_string`.)
    #[test]
    fn normalize_json_decodes_unicode_escape_and_strips_control_escapes() {
        // `A` → 'A'; `\n`/`\t` escape literals are dropped.
        assert_eq!(normalize_json(r"HeAllo\nWorld"), "HeAlloWorld");
    }

    /// rationale: `json_metadata.py:219` lone-surrogate guard — a `\uD800`
    /// (high surrogate, not part of a pair) is dropped rather than emitted.
    #[test]
    fn normalize_json_drops_lone_surrogate_escape() {
        assert_eq!(normalize_json(r"A\uD800B"), "AB");
    }

    /// rationale: the `s.contains('\\')` FALSE side — a plain string with no
    /// backslash bypasses the escape-processing branch and is only tag-stripped
    /// + trimmed.
    #[test]
    fn normalize_json_plain_string_bypasses_escape_branch() {
        assert_eq!(normalize_json("  Plain Title  "), "Plain Title");
    }

    // ── extract_json_author multi-author + no-space break ─────────────────

    /// rationale: `json_metadata.py:178-181` regex-rescue author loop — the
    /// `while ... ' ' in mymatch[1]` loop consumes successive `"author":"..."`
    /// matches while each candidate has a space, merging them with "; ". This
    /// drives the loop body more than once and the consume-and-continue arm.
    #[test]
    fn parse_error_recovers_multiple_spaced_authors() {
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "author":{"name":"Jane Roe"},"author":{"name":"John Doe"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        let author = m.author.expect("recovered authors");
        assert!(author.contains("Jane Roe"), "first author present: {author:?}");
        assert!(author.contains("John Doe"), "second author present: {author:?}");
    }

    // ===================================================================
    // M12 Stage — branch coverage push (metadata_jsonld.rs)
    // Per `wrk_docs/2026.05.26 - CC - Coverage Push Status Report.md`:
    // walk_article / process_parent / extract_author_names residual
    // polymorphic shapes; extract_json_parse_error salvage arms.
    // ===================================================================

    // ---- walk_article — `!cleaned.is_empty()` FALSE sides ----
    // A schema.org value that is ONLY HTML tags normalizes (tag-strip + trim)
    // to "", exercising the `if !cleaned.is_empty()` FALSE side of each field.

    #[test]
    fn walk_article_articlesection_string_empty_after_strip_adds_nothing() {
        // rationale: `json_metadata.py:126-130` String arm — articleSection that
        // is only HTML tags normalizes to "" -> `!cleaned.is_empty()` FALSE ->
        // no category pushed.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": "<b></b>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.categories.is_empty(), "empty section adds nothing");
    }

    #[test]
    fn walk_article_articlesection_array_empty_entry_skipped() {
        // rationale: articleSection Array arm — an entry that strips to "" is
        // skipped (FALSE side) while a real entry is kept.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "articleSection": ["<i></i>", "Real Cat"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Real Cat"]);
    }

    #[test]
    fn walk_article_keywords_string_empty_part_skipped() {
        // rationale: keywords String arm comma-split — an all-tags part strips to
        // "" (FALSE side) and is not pushed; the real keyword survives.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": "<b></b>,realtag"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["realtag"]);
    }

    #[test]
    fn walk_article_keywords_array_empty_entry_skipped() {
        // rationale: keywords Array arm — an all-tags entry strips to "" (FALSE
        // side) and is skipped.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": ["<span></span>", "kept"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["kept"]);
    }

    #[test]
    fn walk_article_headline_empty_after_strip_leaves_title_none() {
        // rationale: `json_metadata.py:132-137` headline arm — a tags-only
        // headline strips to "" (`!cleaned.is_empty()` FALSE) so title is NOT set
        // by the headline branch.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "headline": "<b></b>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none(), "empty headline leaves title None");
    }

    #[test]
    fn walk_article_name_empty_after_strip_leaves_title_none() {
        // rationale: the `else if name` title arm — a tags-only `name` (with no
        // headline) strips to "" (FALSE side) so title stays None.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "name": "<i></i>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none(), "empty name leaves title None");
    }

    #[test]
    fn walk_article_datepublished_empty_after_strip_leaves_date_none() {
        // rationale: datePublished arm — a tags-only value strips to "" (FALSE
        // side) so date is not set.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "datePublished": "<b></b>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.date.is_none(), "empty datePublished leaves date None");
    }

    #[test]
    fn walk_article_datemodified_empty_after_strip_leaves_date_none() {
        // rationale: the `else if dateModified` arm — a tags-only dateModified
        // (with no datePublished) strips to "" (FALSE side) so date stays None.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "dateModified": "<i></i>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.date.is_none(), "empty dateModified leaves date None");
    }

    #[test]
    fn walk_article_image_string_empty_after_strip_leaves_image_none() {
        // rationale: image candidate (String) that strips to "" -> the
        // `!cleaned.is_empty()` FALSE side leaves image None.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "image": "<b></b>"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.image.is_none(), "empty image leaves image None");
    }

    #[test]
    fn walk_article_datemodified_fills_date_when_no_datepublished() {
        // rationale: the `else if dateModified` arm TRUE side — a real
        // dateModified (no datePublished) fills date.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "dateModified": "2024-03-04"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.date.as_deref(), Some("2024-03-04"));
    }

    // ---- process_parent — Person URL-name guard FALSE side ----

    #[test]
    fn process_parent_person_http_name_skipped() {
        // rationale: `json_metadata.py:90-92` inner Person — the
        // `!name.starts_with("http")` FALSE side: a Person nested in process_parent
        // whose `name` is a URL is NOT written as author. Here a Person is a
        // sibling content block (an array at top level keeps process_parent).
        let html = r#"<html><head><script type="application/ld+json">
        [{"@context": "https://schema.org", "@type": "Person",
          "name": "http://example.com/jane"}]
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none(), "http person name is not author");
    }

    // ---- is_plausible_sitename — both-http second operand FALSE ----

    #[test]
    fn is_plausible_sitename_current_http_candidate_also_http_falls_through() {
        // rationale: `json_metadata.py:57-64` — when current starts with "http"
        // AND candidate ALSO starts with "http", the `&& !candidate.starts_with`
        // second operand is FALSE, so the early `return true` is skipped and the
        // length/webpage rule decides. Here candidate is longer -> still true.
        assert!(is_plausible_sitename(
            Some("http://a"),
            "http://longer-name",
            "organization"
        ));
        // And the length-shorter both-http case returns false (webpage rule path
        // with non-webpage type but shorter candidate).
        assert!(!is_plausible_sitename(
            Some("http://longer-current"),
            "http://x",
            "organization"
        ));
    }

    // ---- extract_author_names — givenName/familyName non-string parts empty ----

    #[test]
    fn extract_author_names_given_family_non_string_yields_no_name() {
        // rationale: `json_metadata.py:119-120` — when `givenName` and
        // `familyName` keys exist but are NON-string (numbers), the
        // `filter_map(as_str)` collects nothing and `parts.is_empty()` TRUE side
        // returns None, so no author is produced.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "author": {"@type": "Person", "givenName": 1, "familyName": 2}}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none(), "non-string given/family names yield no author");
    }

    // ---- extract_json_author — no-space candidate breaks the loop ----

    #[test]
    fn parse_error_author_all_tags_after_strip_not_accumulated() {
        // rationale: extract_json_author — a captured author candidate that
        // CONTAINS a space (passes the `' ' in mymatch[1]` gate) but is only HTML
        // tags + whitespace normalizes (tag-strip + trim) to "" -> the
        // `!cleaned.is_empty()` FALSE side does NOT accumulate it. The block is
        // malformed (trailing OOPS) so it routes through the parse-error path.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "author":{"name":"<b> </b>"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none(), "all-tags author not accumulated, got {:?}", m.author);
    }

    #[test]
    fn parse_error_author_without_space_breaks_loop() {
        // rationale: `json_metadata.py:165` `while ... ' ' in mymatch[1]` — a
        // recovered author candidate with NO space breaks the loop immediately
        // (the `if !candidate.contains(' ') { break }` TRUE side), so a
        // single-token author is not accumulated.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "author":{"name":"Solo"} OOPS-malformed
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none(), "single-token author not recovered, got {:?}", m.author);
    }

    // ---- extract_json_parse_error — section-level negative shapes ----

    #[test]
    fn parse_error_publisher_with_comma_in_name_skipped() {
        // rationale: `json_metadata.py:191-197` — the publisher `name` capture
        // containing a ',' fails the `!group.contains(',')` guard (FALSE side),
        // so no sitename is set from the malformed publisher block.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org","@type":"NewsArticle",
         "publisher":{"name":"Acme, Inc"} OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none(), "comma publisher name skipped, got {:?}", m.site_name);
    }

    #[test]
    fn parse_error_type_present_but_regex_no_match_sets_no_pagetype() {
        // rationale: `json_metadata.py:183-189` — `elem.contains("@type")` is TRUE
        // (substring) but the JSON_TYPE regex does not match a quoted value
        // (`captures` None / FALSE side), so pagetype stays None.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context":"https://schema.org" "@type" : broken-no-quotes OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.pagetype.is_none(), "no pagetype from unquoted @type");
    }

    #[test]
    fn parse_error_publisher_substring_present_but_regex_no_match() {
        // rationale: `elem.contains("\"publisher\"")` TRUE but JSON_PUBLISHER
        // regex captures nothing (no `"name":"..."` follows) -> `captures` None
        // FALSE side -> site_name stays None.
        let html = r#"<html><head><script type="application/ld+json">
        {"@type":"NewsArticle","publisher": broken OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none());
    }

    #[test]
    fn parse_error_articlesection_substring_present_but_regex_no_match() {
        // rationale: `elem.contains("\"articleSection\"")` TRUE but JSON_CATEGORY
        // regex captures nothing -> `captures` None FALSE side -> categories
        // stays empty.
        let html = r#"<html><head><script type="application/ld+json">
        {"@type":"NewsArticle","articleSection": broken OOPS
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.categories.is_empty());
    }

    #[test]
    fn parse_error_recovers_articlesection_category() {
        // rationale: `json_metadata.py:200-203` happy path — a well-formed
        // articleSection in an OTHERWISE malformed block is recovered as the sole
        // category (drives the `!cleaned.is_empty()` TRUE side of the category arm).
        let html = r#"<html><head><script type="application/ld+json">
        {"@type":"NewsArticle","articleSection": "Politics" OOPS-trailing
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.categories, vec!["Politics"]);
    }

    // ---- normalize_json — \uXXXX valid escape decode (non-surrogate) ----

    #[test]
    fn normalize_json_decodes_valid_unicode_escape() {
        // rationale: `json_metadata.py:218` JSON_UNICODE_REPLACE — a valid
        // non-surrogate escape (here the six literal chars backslash-u-0-0-4-1)
        // decodes to 'A' via the `char::from_u32` Some arm. The "\\u0041" string
        // literal is the literal backslash sequence (NOT a Rust unicode escape).
        assert_eq!(normalize_json("x\\u0041y"), "xAy");
    }

    // ===================================================================
    // M13 Stage — final branch-coverage push (metadata_jsonld.rs)
    // process_parent publisher/person/article else-if cascade FALSE sides;
    // keywords-array non-string element; extract_author_names empty-after-strip.
    // ===================================================================

    #[test]
    fn process_parent_publisher_type_with_no_name_keys_sets_no_sitename() {
        // rationale: `json_metadata.py:85-88` — a JSON_PUBLISHER_SCHEMA entry
        // (Organization) carrying NONE of name/legalName/alternateName makes the
        // `candidate` OR-chain None, so `if let Some(name) = candidate` takes its
        // FALSE side (metadata_jsonld.rs:341) and no sitename is written.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Organization",
         "url": "https://example.com"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.site_name.is_none(), "publisher with no name keys yields no sitename");
    }

    #[test]
    fn process_parent_publisher_implausible_candidate_does_not_overwrite() {
        // rationale: `json_metadata.py:57-64` is_plausible_sitename — the first
        // (longer, non-webpage) Organization sets site_name; a later WebPage entry
        // with a longer name is rejected by the `content_type != "webpage"` rule,
        // so `if is_plausible_sitename(...)` takes its FALSE side
        // (metadata_jsonld.rs:343) and the original sitename survives.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org",
         "@graph": [
            {"@type": "Organization", "name": "Acme"},
            {"@type": "WebPage", "name": "A Much Longer Page Title Here"}
         ]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.site_name.as_deref(), Some("Acme"));
    }

    #[test]
    fn process_parent_person_type_with_no_name_key_writes_no_author() {
        // rationale: `json_metadata.py:90-92` — a top-level Person entry with NO
        // `name` key makes `obj.get("name").and_then(as_str)` None, so the
        // let-chain `if let Some(name) = ... && !name.starts_with("http")` takes
        // its FALSE side at the binding (metadata_jsonld.rs:353) and no author is
        // produced. (Top-level array keeps process_parent's per-entity dispatch.)
        let html = r#"<html><head><script type="application/ld+json">
        [{"@context": "https://schema.org", "@type": "Person",
          "url": "https://example.com/jane"}]
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.author.is_none(), "Person with no name key yields no author");
    }

    #[test]
    fn process_parent_unrecognised_type_falls_through_cascade() {
        // rationale: `json_metadata.py:74-92` — a well-formed entry whose @type is
        // none of publisher/person/article schema (here "Recipe") fails the
        // publisher `if`, the `person` else-if, AND the JSON_ARTICLE_SCHEMA
        // else-if (metadata_jsonld.rs:359 FALSE side), so walk_article never runs
        // and the headline is NOT surfaced as a title.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Recipe",
         "headline": "Should Not Become Title", "name": "Recipe Name"}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert!(m.title.is_none(), "non-article/publisher/person type does not set title");
    }

    #[test]
    fn walk_article_keywords_array_non_string_element_skipped() {
        // rationale: `walk_article` keywords Array arm — `if let Some(s) =
        // item.as_str()` takes its FALSE side (metadata_jsonld.rs:454) for a
        // non-string array element (number); the real string tag survives.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article",
         "keywords": [42, "kept-tag"]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.tags, vec!["kept-tag"]);
    }

    #[test]
    fn extract_author_names_object_name_empty_after_strip_not_pushed() {
        // rationale: `json_metadata.py:113-118` — an author-list object whose
        // `name` strips (tag-strip + trim) to "" makes `!cleaned.is_empty()` take
        // its FALSE side (metadata_jsonld.rs:610), so that entry is not pushed;
        // the sibling valid author still surfaces.
        let html = r#"<html><head><script type="application/ld+json">
        {"@context": "https://schema.org", "@type": "Article", "headline": "x",
         "author": [{"name": "<b></b>"}, {"name": "Real Author"}]}
        </script></head><body></body></html>"#;
        let m = run(html);
        assert_eq!(m.author.as_deref(), Some("Real Author"));
    }
}
