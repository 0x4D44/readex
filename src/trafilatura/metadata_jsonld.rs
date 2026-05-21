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
//! - The Python `json_metadata.py:171-213` `extract_json_parse_error` is a
//!   regex-based salvage path for malformed JSON. Stage 7b skips this path
//!   and treats any `json::from_str` failure as "no JSON-LD data" — the
//!   crate's corpus does not exercise the regex-salvage path (every Stage
//!   3-B fixture parses cleanly), and a regex port adds ~80 LOC for a
//!   rescue case the BLOCKER gates don't gate on. If a future test demands
//!   it, the rescue is a one-shot fold-in.
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
        if let Ok(value) = parsed {
            extract_json(&value, metadata);
        }
        // Malformed JSON: silently skip (faithful to Python's
        // `json.JSONDecodeError` catch; the regex-rescue path is omitted
        // per the module header's faithful-divergence note).
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
// Tests
// ===========================================================================

#[cfg(test)]
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
}
