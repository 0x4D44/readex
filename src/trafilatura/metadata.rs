//! `metadata` — Stage 7a: HTML-based metadata extraction (OG / meta-name /
//! XPath fallbacks).
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7 (the
//! metadata-extraction phase). Source of truth:
//! `trafilatura@v2.0.0/metadata.py:1-589` plus the XPath constants in
//! `trafilatura@v2.0.0/xpaths.py:214-265` (already vendored in
//! [`crate::trafilatura::xpaths_constants`] at Stage 2a).
//!
//! # Scope of this file (Stage 7a)
//!
//! Stage 7a ports the **HTML-tag** sources only — OpenGraph `<meta
//! property="og:*">` tags, `<meta name="...">` tags, `<meta itemprop="...">`,
//! `<html lang="...">`, plus the XPath fallbacks for title / author
//! (`TITLE_XPATHS` / `AUTHOR_XPATHS`). These are the meat of `metadata.py`'s
//! `examine_meta` (`:221-315`), `extract_title` (`:351-376`), `extract_author`
//! (`:379-386`), and `extract_metadata`'s HTML-side orchestration (`:482-589`).
//!
//! **Out of scope (deferred to later sub-stages):**
//! - **7b** JSON-LD parsing (`metadata.py:182-195` `extract_meta_json` +
//!   `json_metadata.py` schema walker). Requires a JSON parser + a substantial
//!   schema interpreter; deserves its own iteration.
//! - **7d** URL canonicalization (`metadata.py:389-413` `extract_url` —
//!   needs `courlan` analogue) and date extraction (`metadata.py:546-547`
//!   `find_date` from `htmldate`). Stubbed: `Metadata.url` / `Metadata.date`
//!   remain `None` from this module.
//! - License extraction (`metadata.py:465-479` `extract_license`) and
//!   categories/tags (`metadata.py:422-446` `extract_catstags`) — possible
//!   future fold-ins; for now `Metadata.categories` / `tags` / `license` are
//!   shaped but populated as empty / `None`.
//!
//! # Anti-inversion (HLD §4 / §10)
//!
//! Every non-trivial function header carries a `metadata.py:NN` source-line
//! cite. The OG / meta-name / property tables (`METANAME_AUTHOR`,
//! `METANAME_TITLE`, etc.) are byte-faithful vendorings of the Python
//! `frozenset` literals; ordering inside a `frozenset` is irrelevant but the
//! membership-test semantics are preserved. The XPath constants are consumed
//! verbatim from `xpaths_constants` via the Stage 0b XPath engine.
//!
//! # Faithful divergences (recorded)
//!
//! - `normalize_authors`: Python's `json_metadata.py:226-268` runs a full
//!   regex-driven name-splitter / emoji-stripper / Twitter-handle-stripper /
//!   nickname-stripper / title-case heuristic. Stage 7a ships a **lite**
//!   variant ([`normalize_authors`]) that strips HTML tags, decodes
//!   entities, trims, and applies the AUTHOR_EMAIL / URL-prefix rejection
//!   gate from the Python source. The full normaliser arrives when 7b /
//!   downstream JSON-LD wiring needs it (currently no test in our corpus
//!   demands the heavy normaliser; the simple "Jane Doe" / "Jane Author"
//!   cases pass cleanly with the lite variant).
//! - `extract_metainfo` `len_limit` test (`metadata.py:328`) is implemented
//!   faithfully but uses [`text_content`] for the element text rather than
//!   lxml's `itertext()` joined on `" "`. The two produce the same text for
//!   single-line title/author elements (the only realistic case for an
//!   `h1`/`address`/`<span class="author">`); deeper nesting with
//!   intervening Element nodes is exceedingly rare for these XPath
//!   selectors and any future-discovered divergence is a one-shot patch.

use crate::readability::dom::{
    Dom, NodeRef, children, element_text, get_attribute, get_elements_by_tag_name, local_name,
    text_content,
};
use crate::trafilatura::output::strip_control_chars;
use crate::trafilatura::utils::trim;
use crate::trafilatura::xpath_engine;
use crate::trafilatura::xpaths_constants::{AUTHOR_DISCARD_XPATHS, AUTHOR_XPATHS, TITLE_XPATHS};
use regex::Regex;
use std::borrow::Cow;
use std::sync::OnceLock;

// ===========================================================================
// Metadata struct (metadata.py:Document analogue, settings.py:Document)
// ===========================================================================

/// `Document` dataclass — the metadata payload returned by
/// `extract_metadata` (`metadata.py:482-589`).
///
/// In Python this is `trafilatura.settings.Document`, a dataclass-style
/// container the metadata pipeline populates incrementally. The Rust port
/// uses the same field names where they match (`title`, `author`,
/// `url`, `hostname`, `description`, `date`, `categories`, `tags`,
/// `language`, `image`, `pagetype`, `license`) plus the `sitename` ->
/// `site_name` rename to match the M2 `Extracted.site_name` field already
/// in the public API (rust snake-case preference; the Python `sitename`
/// is an unconventional single-word elision).
///
/// Every field is `Option`/`Vec` and defaults to `None`/empty so
/// `Metadata::default()` returns the "no data" sentinel
/// (`metadata.py:508` `return Document()` on `load_html` failure).
///
/// Stage 7a populates `title`, `author`, `description`, `site_name`,
/// `language`, `image`, `pagetype`, `tags` (from `article:tag`). Stage 7b
/// (JSON-LD) overrides any of these when JSON-LD wins precedence. Stage 7d
/// fills `url`, `hostname`, `date`. License + categories remain unwired
/// at Stage 7a.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Metadata {
    /// `Document.title` — document title.
    pub title: Option<String>,
    /// `Document.author` — author / byline string (multiple authors are
    /// `"; "`-joined per `normalize_authors`'s output convention).
    pub author: Option<String>,
    /// `Document.url` — Stage 7d stub (`None` at Stage 7a).
    pub url: Option<String>,
    /// `Document.hostname` — Stage 7d stub.
    pub hostname: Option<String>,
    /// `Document.description` — short summary (`og:description` /
    /// `<meta name="description">` / `dcterms.abstract` / etc.).
    pub description: Option<String>,
    /// `Document.sitename` (renamed to `site_name` to match the M2
    /// `Extracted.site_name` snake-case) — site / publisher name.
    pub site_name: Option<String>,
    /// `Document.date` — Stage 7d stub.
    pub date: Option<String>,
    /// `Document.categories` — empty at Stage 7a (Stage 7d / future).
    pub categories: Vec<String>,
    /// `Document.tags` — populated from `<meta property="article:tag">`
    /// and `<meta name="keywords">` etc. at Stage 7a.
    pub tags: Vec<String>,
    /// `Document.language` — populated from `<html lang="...">`.
    pub language: Option<String>,
    /// `Document.image` — from `og:image` / `twitter:image` / etc.
    pub image: Option<String>,
    /// `Document.pagetype` — `og:type`.
    pub pagetype: Option<String>,
    /// `Document.license` — empty at Stage 7a (future fold-in).
    pub license: Option<String>,
    /// `Document.filedate` — the download/extraction date. Python sets this
    /// to `date_config["max_date"]` = `datetime.now().strftime("%Y-%m-%d")`
    /// (`metadata.py:586` + `settings.py:202`), i.e. *today*. M8 added this
    /// slot (M4 Stage 6 had deferred it). Rendered as `<date type="download">`
    /// in the TEI header.
    pub filedate: Option<String>,
}

// ===========================================================================
// OG / meta-name lookup tables (metadata.py:64-149)
// ===========================================================================

/// `METANAME_AUTHOR` (`metadata.py:64-82`) — the `<meta name="...">` set
/// recognised as author-bearing.
const METANAME_AUTHOR: &[&str] = &[
    "article:author",
    "atc-metaauthor",
    "author",
    "authors",
    "byl",
    "citation_author",
    "creator",
    "dc.creator",
    "dc.creator.aut",
    "dc:creator",
    "dcterms.creator",
    "dcterms.creator.aut",
    "dcsext.author",
    "parsely-author",
    "rbauthors",
    "sailthru.author",
    "shareaholic:article_author_name",
];

/// `METANAME_DESCRIPTION` (`metadata.py:83-91`).
const METANAME_DESCRIPTION: &[&str] = &[
    "dc.description",
    "dc:description",
    "dcterms.abstract",
    "dcterms.description",
    "description",
    "sailthru.description",
    "twitter:description",
];

/// `METANAME_PUBLISHER` (`metadata.py:92-103`).
const METANAME_PUBLISHER: &[&str] = &[
    "article:publisher",
    "citation_journal_title",
    "copyright",
    "dc.publisher",
    "dc:publisher",
    "dcterms.publisher",
    "publisher",
    "sailthru.publisher",
    "rbpubname",
    "twitter:site",
];

/// `METANAME_TAG` (`metadata.py:104-111`).
const METANAME_TAG: &[&str] = &[
    "citation_keywords",
    "dcterms.subject",
    "keywords",
    "parsely-tags",
    "shareaholic:keywords",
    "tags",
];

/// `METANAME_TITLE` (`metadata.py:112-124`).
const METANAME_TITLE: &[&str] = &[
    "citation_title",
    "dc.title",
    "dcterms.title",
    "fb_title",
    "headline",
    "parsely-title",
    "sailthru.title",
    "shareaholic:title",
    "rbtitle",
    "title",
    "twitter:title",
];

/// `METANAME_IMAGE` (`metadata.py:126-133`).
const METANAME_IMAGE: &[&str] = &[
    "image",
    "og:image",
    "og:image:url",
    "og:image:secure_url",
    "twitter:image",
    "twitter:image:src",
];

/// `PROPERTY_AUTHOR` (`metadata.py:134`) — the `<meta property="...">` set
/// recognised as author-bearing (smaller than the name-attr table).
const PROPERTY_AUTHOR: &[&str] = &["author", "article:author"];

/// `TWITTER_ATTRS` (`metadata.py:135`) — the `<meta name="...">` values that
/// hold a *backup* sitename (twitter handle / app-name).
const TWITTER_ATTRS: &[&str] = &["twitter:site", "application-name"];

/// `OG_AUTHOR` (`metadata.py:151`) — `og:` properties carrying author info.
const OG_AUTHOR: &[&str] = &["og:author", "og:article:author"];

/// OG property -> target Metadata field. Maps `metadata.py:141-149`'s
/// `OG_PROPERTIES` dict. The match expression at the call site translates
/// each hit into the relevant `Metadata` field write (Rust has no clean
/// "field-name from string" mechanism, so the Python dict becomes a
/// `match`).
///
/// `og:image:url` and `og:image:secure_url` both map to `image`
/// (`metadata.py:146-148`).
fn assign_og_property(metadata: &mut Metadata, property_name: &str, content: &str) {
    match property_name {
        "og:title" if metadata.title.is_none() => {
            metadata.title = Some(content.to_string());
        }
        "og:description" if metadata.description.is_none() => {
            metadata.description = Some(content.to_string());
        }
        "og:site_name" if metadata.site_name.is_none() => {
            metadata.site_name = Some(content.to_string());
        }
        "og:image" | "og:image:url" | "og:image:secure_url"
            if metadata.image.is_none() =>
        {
            metadata.image = Some(content.to_string());
        }
        "og:type" if metadata.pagetype.is_none() => {
            metadata.pagetype = Some(content.to_string());
        }
        _ => {}
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// `normalize_authors(current_authors, author_string)` (`json_metadata.py:226-268`).
///
/// Full port (M8 — was a "lite" partial). Splits the candidate on
/// `AUTHOR_SPLIT`, runs each piece through the regex-cleaning pipeline (emoji /
/// `@handle` / `._+`→space / nickname-parenthetical / special chars / "by"
/// prefix / digits-to-end / trailing preposition), applies the empty/too-long
/// skip, title-cases ALL-CAPS or no-cap names, then dedup-merges into the
/// existing `; `-joined author list and returns `'; '.join(...).strip('; ')`.
fn normalize_authors(current: Option<&str>, author_string: &str) -> Option<String> {
    // `if author_string.lower().startswith('http') or AUTHOR_EMAIL.match(...)`.
    if author_string.to_ascii_lowercase().starts_with("http")
        || author_email_re()
            .find(author_string)
            .is_some_and(|m| m.start() == 0)
    {
        return current.map(str::to_string);
    }

    // `new_authors = current_authors.split('; ')` (else []).
    let mut new_authors: Vec<String> = match current {
        Some(c) => c.split("; ").map(str::to_string).collect(),
        None => Vec::new(),
    };

    // NOTE: the `'\\u' in author_string` unicode-escape branch (json_metadata.py:234)
    // is not ported — no corpus author carries a literal backslash-u; would need
    // Python `unicode_escape` decoding. Documented gap.
    // `if '&#' in s or '&amp;' in s: s = unescape(s)` (json_metadata.py:237-238).
    let unescaped = if author_string.contains("&#") || author_string.contains("&amp;") {
        crate::readability::metadata::unescape_html_entities(author_string)
    } else {
        author_string.to_string()
    };
    // `author_string = HTML_STRIP_TAGS.sub('', author_string)`.
    let stripped = html_strip_tags_re().replace_all(&unescaped, "").into_owned();

    for piece in author_split_re().split(&stripped) {
        let mut author = trim(piece);
        author = author_emoji_re().replace_all(&author, "").into_owned();
        author = author_twitter_re().replace_all(&author, "").into_owned();
        author = trim(&author_replace_join_re().replace_all(&author, " "));
        author = author_nickname_re().replace_all(&author, "").into_owned();
        author = author_special_re().replace_all(&author, "").into_owned();
        author = author_prefix_re().replace_all(&author, "").into_owned();
        author = author_numbers_re().replace_all(&author, "").into_owned();
        author = author_preposition_re().replace_all(&author, "").into_owned();

        // `if not author or (len(author) >= 50 and ' ' not in author and '-' not in author): continue`.
        let len = author.chars().count();
        if author.is_empty()
            || (len >= 50 && !author.contains(' ') && !author.contains('-'))
        {
            continue;
        }

        // `if not author[0].isupper() or sum(c.isupper()) < 1: author = author.title()`.
        let first_upper = author.chars().next().is_some_and(char::is_uppercase);
        let any_upper = author.chars().any(char::is_uppercase);
        if !first_upper || !any_upper {
            author = python_title_case(&author);
        }

        // `if author not in new_authors and (len==0 or all(na not in author))`.
        if !new_authors.iter().any(|na| na == &author)
            && (new_authors.is_empty() || new_authors.iter().all(|na| !author.contains(na.as_str())))
        {
            new_authors.push(author);
        }
    }

    if new_authors.is_empty() {
        return current.map(str::to_string);
    }
    // `'; '.join(new_authors).strip('; ')`.
    Some(
        new_authors
            .join("; ")
            .trim_matches(|c| c == ';' || c == ' ')
            .to_string(),
    )
}

// ---- AUTHOR_* regexes (json_metadata.py:21-54, utils.py:66) ----------------

fn author_email_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b").unwrap())
}
fn html_strip_tags_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?s)(<!--.*?-->|<[^>]*>)").unwrap())
}
fn author_split_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"(?i)/|;|,|\||&|(?:^|\W)[u|a]nd(?:$|\W)").unwrap())
}
fn author_emoji_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            "[\u{2700}-\u{27BE}\u{1F600}-\u{1F64F}\u{2600}-\u{26FF}\u{1F300}-\u{1F5FF}\
             \u{1F900}-\u{1F9FF}\u{1FA70}-\u{1FAFF}\u{1F680}-\u{1F6FF}]+",
        )
        .unwrap()
    })
}
fn author_twitter_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"@\w+").unwrap())
}
fn author_replace_join_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[._+]").unwrap())
}
fn author_nickname_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r#"["‘({\[’'][^"]+?[‘’"')\]}]"#).unwrap())
}
fn author_special_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[^\w]+$|[:()?*$#!%/<>{}~¿]").unwrap())
}
fn author_prefix_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)^([a-zäöüß]+(ed|t))? ?(written by|words by|words|by|von|from) ").unwrap()
    })
}
fn author_numbers_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\d.+?$").unwrap())
}
fn author_preposition_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)\b\s+(am|on|for|at|in|to|from|of|via|with|—|-|–)\s+(.*)").unwrap()
    })
}

/// One-pass strip of `<...>` HTML tag patterns (`utils.py:HTML_STRIP_TAGS =
/// re.compile(r"<[^<>]*>")`). Pure ASCII scan; no regex dependency.
fn strip_simple_html_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            // Scan forward until '>' (inclusive). If we hit another '<'
            // first, abort the scan: the `<` we saw was a literal, push it.
            // (Faithful to the regex `<[^<>]*>` — match fails on a nested
            // `<`, so the outer `<` is not part of any tag.)
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
                // Not a tag — push the buffered chars verbatim.
                out.push_str(&buf);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `normalize_tags` (`metadata.py:160-166`).
///
/// Strips `"` and `'` from the tag string, splits on `", "`, filters out
/// empties, rejoins with `", "`. Decoded entities are passed through
/// `trim` first.
fn normalize_tags(tags: &str) -> String {
    let trimmed = trim(tags);
    if trimmed.is_empty() {
        return String::new();
    }
    let cleaned: String = trimmed.chars().filter(|c| *c != '"' && *c != '\'').collect();
    let parts: Vec<&str> = cleaned
        .split(", ")
        .filter(|p| !p.is_empty())
        .collect();
    parts.join(", ")
}

/// `check_authors` (`metadata.py:169-179`). Filter `authors` against
/// `blacklist`, keeping any author whose lowercased trim is not in the
/// blacklist. Returns `None` if every author was filtered.
fn check_authors(authors: &str, blacklist: &[String]) -> Option<String> {
    let lowered_blacklist: Vec<String> =
        blacklist.iter().map(|s| s.to_ascii_lowercase()).collect();
    let mut kept: Vec<String> = Vec::new();
    for author in authors.split(';') {
        let candidate = author.trim();
        if candidate.is_empty() {
            continue;
        }
        let candidate_lower = candidate.to_ascii_lowercase();
        if lowered_blacklist.contains(&candidate_lower) {
            continue;
        }
        kept.push(candidate.to_string());
    }
    if kept.is_empty() {
        None
    } else {
        let joined = kept.join("; ");
        Some(joined.trim_matches(|c: char| c == ';' || c == ' ').to_string())
    }
}

/// Find the `<head>` element under the document root, if any.
///
/// html5ever always synthesises `<head>` for a real parse, but our DOM
/// facade does not expose it directly (only [`Dom::body`]). This helper
/// walks the `<html>` children for the first `<head>` child.
fn find_head(doc: &Dom) -> Option<NodeRef> {
    let html = doc.root_element()?;
    children(&html)
        .into_iter()
        .find(|c| local_name(c).as_deref() == Some("head"))
}

/// Find every `<meta>` element under `head` (immediate descendants — but
/// html5ever's parse may nest meta under template-like containers; for
/// metadata extraction the realistic shape is `<head>><meta>` as direct
/// children, matching the Python XPath `.//head/meta[...]`).
fn meta_elements(head: &NodeRef) -> Vec<NodeRef> {
    get_elements_by_tag_name(head, "meta")
}

/// `extract_opengraph(tree)` (`metadata.py:198-218`) — walk
/// `<meta property="og:*">` tags and populate the OG fields.
fn examine_opengraph(head: &NodeRef, metadata: &mut Metadata) {
    for elem in meta_elements(head) {
        let Some(property_attr) = get_attribute(&elem, "property") else {
            continue;
        };
        let property_lower = property_attr.to_ascii_lowercase();
        if !property_lower.starts_with("og:") {
            continue;
        }
        let Some(content) = get_attribute(&elem, "content") else {
            continue;
        };
        if content.trim().is_empty() {
            continue;
        }
        // OG_PROPERTIES dict membership (`metadata.py:141-149`).
        assign_og_property(metadata, &property_lower, &content);
        // og:author / og:article:author (`metadata.py:213-214`).
        if OG_AUTHOR.contains(&property_lower.as_str()) {
            metadata.author = normalize_authors(metadata.author.as_deref(), &content);
        }
        // og:url -> `Metadata.url` (Stage 7a stub: we DO accept the og:url
        // value here even though full URL canonicalization is Stage 7d.
        // The Python source at `metadata.py:211-212` calls `is_valid_url`
        // before assigning; we keep it permissive at Stage 7a — Stage 7d
        // will tighten with the courlan analogue).
        if property_lower == "og:url" && metadata.url.is_none() {
            metadata.url = Some(content);
        }
    }
}

// ===========================================================================
// examine_meta (`metadata.py:221-315`)
// ===========================================================================

/// `examine_meta(tree, document)` (`metadata.py:221-315`).
///
/// Walks the OG tags first, then iterates every `<meta content="...">`
/// element under `<head>`, dispatching by `property` / `name` / `itemprop`
/// attribute. Backup site name from twitter handles is applied last
/// (`metadata.py:310-311`).
fn examine_meta(doc: &Dom, document: &mut Metadata) {
    let Some(head) = find_head(doc) else {
        return;
    };
    // First pass: OG tags (`metadata.py:223-224` bootstrap).
    examine_opengraph(&head, document);

    let mut tags: Vec<String> = Vec::new();
    let mut backup_sitename: Option<String> = None;

    for elem in meta_elements(&head) {
        let Some(content_raw) = get_attribute(&elem, "content") else {
            continue;
        };
        // content stripped of HTML tags and whitespace (`metadata.py:244`).
        let content = trim(&strip_simple_html_tags(&content_raw));
        if content.is_empty() {
            continue;
        }

        // ---- property attr branch (`metadata.py:250-262`) ---------------
        if let Some(property_raw) = get_attribute(&elem, "property") {
            let property_attr = property_raw.to_ascii_lowercase();
            // OG done in examine_opengraph (`metadata.py:253-254`).
            if property_attr.starts_with("og:") {
                continue;
            }
            if property_attr == "article:tag" {
                let normalized = normalize_tags(&content);
                if !normalized.is_empty() {
                    tags.push(normalized);
                }
            } else if PROPERTY_AUTHOR.contains(&property_attr.as_str()) {
                document.author = normalize_authors(document.author.as_deref(), &content);
            } else if property_attr == "article:publisher" && document.site_name.is_none() {
                document.site_name = Some(content);
            } else if METANAME_IMAGE.contains(&property_attr.as_str())
                && document.image.is_none()
            {
                document.image = Some(content);
            }
            continue;
        }

        // ---- name attr branch (`metadata.py:264-290`) -------------------
        if let Some(name_raw) = get_attribute(&elem, "name") {
            let name_attr = name_raw.to_ascii_lowercase();
            if METANAME_AUTHOR.contains(&name_attr.as_str()) {
                document.author = normalize_authors(document.author.as_deref(), &content);
            } else if METANAME_TITLE.contains(&name_attr.as_str()) {
                if document.title.is_none() {
                    document.title = Some(content);
                }
            } else if METANAME_DESCRIPTION.contains(&name_attr.as_str()) {
                if document.description.is_none() {
                    document.description = Some(content);
                }
            } else if METANAME_PUBLISHER.contains(&name_attr.as_str()) {
                if document.site_name.is_none() {
                    document.site_name = Some(content);
                }
            } else if TWITTER_ATTRS.contains(&name_attr.as_str())
                || name_attr.contains("twitter:app:name")
            {
                backup_sitename = Some(content);
            } else if name_attr == "twitter:url" && document.url.is_none() {
                // Stage 7d will gate this through URL validation; Stage 7a
                // accepts it permissively.
                document.url = Some(content);
            } else if METANAME_TAG.contains(&name_attr.as_str()) {
                let normalized = normalize_tags(&content);
                if !normalized.is_empty() {
                    tags.push(normalized);
                }
            }
            continue;
        }

        // ---- itemprop attr branch (`metadata.py:291-298`) ---------------
        if let Some(itemprop_raw) = get_attribute(&elem, "itemprop") {
            let itemprop_attr = itemprop_raw.to_ascii_lowercase();
            if itemprop_attr == "author" {
                document.author = normalize_authors(document.author.as_deref(), &content);
            } else if itemprop_attr == "description" {
                if document.description.is_none() {
                    document.description = Some(content);
                }
            } else if itemprop_attr == "headline" && document.title.is_none() {
                document.title = Some(content);
            }
        }
    }

    // Backup sitename (`metadata.py:310-311`).
    if document.site_name.is_none() {
        document.site_name = backup_sitename;
    }
    if !tags.is_empty() {
        document.tags = tags;
    }
}

// ===========================================================================
// Title extraction (`metadata.py:337-376`)
// ===========================================================================

/// `HTMLTITLE_REGEX` source pattern (`metadata.py:50-52`).
///
/// `^(.+)?\s+[–•·—|⁄*⋆~‹«<›»>:-]\s+(.+)$`
///
/// Stage 7a implements the predicate **structurally** rather than pulling
/// in a regex dependency for one pattern: split the title on the
/// separator-character set when surrounded by whitespace; return the two
/// halves. The character class is the same set as the Python regex
/// (`-`, `–`, `•`, `·`, `—`, `|`, `⁄`, `*`, `⋆`, `~`, `‹`, `«`, `<`, `›`,
/// `»`, `>`, `:`). When multiple separators occur, group 1 `(.+)?` is
/// **greedy**, so it maximises the LEFT half — i.e. the regex splits on the
/// LAST separator (rightmost), leaving the smallest non-empty right half.
/// (Verified: `"A - B - C"` → `("A - B", "C")`, not `("A", "B - C")`.)
fn split_html_title(title: &str) -> Option<(String, String)> {
    const SEPS: &[char] = &[
        '–', '•', '·', '—', '|', '⁄', '*', '⋆', '~', '‹', '«', '<', '›', '»', '>', ':', '-',
    ];
    let chars: Vec<char> = title.chars().collect();
    if chars.len() < 3 {
        return None;
    }
    // Greedy group 1 → scan from the RIGHT for the last index `i` such that
    // chars[i-1] is whitespace, chars[i] is in SEPS, chars[i+1] is whitespace,
    // and both trimmed halves are non-empty.
    for i in (1..chars.len() - 1).rev() {
        if !SEPS.contains(&chars[i]) {
            continue;
        }
        if !chars[i - 1].is_whitespace() || !chars[i + 1].is_whitespace() {
            continue;
        }
        let left: String = chars[..i - 1].iter().collect();
        let right: String = chars[i + 2..].iter().collect();
        if left.trim().is_empty() || right.trim().is_empty() {
            continue;
        }
        return Some((left.trim().to_string(), right.trim().to_string()));
    }
    None
}

/// Title text-split heuristic (`metadata.py:50-52` + `metadata.py:337-348`'s
/// `examine_title_element`). Strips a trailing "site name" suffix
/// (e.g. `"My Site | My Article"` -> `"My Site"` or `"My Article"`,
/// per the leftmost-match rule).
///
/// Returns the **first** half of the title when a separator is found;
/// otherwise returns the input verbatim.
pub fn split_title_on_separators(title: &str) -> String {
    if let Some((first, _second)) = split_html_title(title) {
        first
    } else {
        title.to_string()
    }
}

/// `examine_title_element(tree)` (`metadata.py:337-348`). Returns
/// `(raw_title, first_half, second_half)` where halves are `Some` iff the
/// title matched `HTMLTITLE_REGEX`.
fn examine_title_element(doc: &Dom) -> (String, Option<String>, Option<String>) {
    let Some(head) = find_head(doc) else {
        return (String::new(), None, None);
    };
    let titles = get_elements_by_tag_name(&head, "title");
    let Some(title_elem) = titles.first() else {
        return (String::new(), None, None);
    };
    let raw = trim(&text_content(title_elem));
    let halves = split_html_title(&raw);
    match halves {
        Some((a, b)) => (raw, Some(a), Some(b)),
        None => (raw, None, None),
    }
}

/// `extract_metainfo(tree, expressions, len_limit)` (`metadata.py:318-334`).
///
/// Walk the XPath expressions; for each result, take `trim(" ".join(
/// elem.itertext()))` (metadata.py:327) — itertext joined on a SPACE, NOT a
/// bare `text_content` concat: a `<div>X<a>Y</a><a>Z</a>` element yields
/// `"X Y Z"`, not `"XYZ"`. Return the FIRST result whose length is strictly
/// between 2 and `len_limit`.
fn extract_metainfo(
    tree: &NodeRef,
    expressions: &[&str],
    len_limit: usize,
) -> Option<String> {
    for expr in expressions {
        let Ok(results) = xpath_engine::evaluate(expr, tree) else {
            continue;
        };
        for elem in &results {
            let raw = crate::trafilatura::baseline::itertext(elem).join(" ");
            // `trim` = `" ".join(s.split())` (utils.py:340-346) collapses the
            // join's whitespace runs to single spaces.
            let content = trim(&raw);
            if content.chars().count() > 2 && content.chars().count() < len_limit {
                return Some(content);
            }
        }
    }
    None
}

/// `extract_title(tree)` (`metadata.py:351-376`).
///
/// Resolution order:
/// 1. Single `<h1>` if it is the **only** `<h1>` in the document — use its
///    text.
/// 2. `TITLE_XPATHS` walk via `extract_metainfo`.
/// 3. `<title>` tag with HTMLTITLE_REGEX split — prefer either half that
///    does NOT contain a `.` character.
/// 4. First `<h1>` (if there were multiple).
/// 5. First `<h2>`.
fn extract_title(doc: &Dom) -> Option<String> {
    let body = doc.body()?;
    // h1 collection
    let h1_results = get_elements_by_tag_name(&body, "h1");
    // 1. Single-h1 rule.
    if h1_results.len() == 1 {
        let title = trim(&text_content(&h1_results[0]));
        if !title.is_empty() {
            return Some(title);
        }
    }
    // 2. TITLE_XPATHS walk.
    if let Some(title) = extract_metainfo(&body, TITLE_XPATHS, 200) {
        return Some(title);
    }
    // 3. <title> tag with separator-split (`metadata.py:364-367`).
    let (raw, first, second) = examine_title_element(doc);
    for half in [first.as_deref(), second.as_deref()].into_iter().flatten() {
        if !half.contains('.') {
            return Some(half.to_string());
        }
    }
    // 4. First h1 fallback (`metadata.py:368-370`).
    if let Some(h1) = h1_results.first() {
        let txt = trim(&text_content(h1));
        if !txt.is_empty() {
            return Some(txt);
        }
    }
    // 5. First h2 fallback (`metadata.py:371-376`).
    let h2_results = get_elements_by_tag_name(&body, "h2");
    if let Some(h2) = h2_results.first() {
        let txt = trim(&text_content(h2));
        if !txt.is_empty() {
            return Some(txt);
        }
    }
    // Final fallback: the raw <title> text (Python's variable `title` at
    // line 376 ends up here when no split matched and no h2 existed; if
    // raw is also empty, return None to keep `Metadata.title = None`).
    if !raw.is_empty() {
        return Some(raw);
    }
    None
}

/// `extract_author(tree)` (`metadata.py:379-386`).
///
/// Python: `subtree = prune_unwanted_nodes(deepcopy(tree), AUTHOR_DISCARD_XPATHS);
/// author = extract_metainfo(subtree, AUTHOR_XPATHS, len_limit=120)`. M8 wires
/// the discard-prune (was skipped): it removes comment/sidebar/title/date/
/// `//time`/`//figure` blocks before AUTHOR_XPATHS runs, so spurious matches
/// (a headline or nav text mistaken for a byline) are pruned — Python returns
/// no author there, and mdrcel now matches (5f27add4, eceb9608). We `deep_clone`
/// the body so the shared metadata `Dom` (reused for categories/tags/license)
/// is not mutated.
fn extract_author(doc: &Dom, blacklist: &[String]) -> Option<String> {
    let body = doc.body()?;
    let subtree = crate::readability::dom::deep_clone(&body);
    crate::trafilatura::cleaning::prune_unwanted_nodes(&subtree, AUTHOR_DISCARD_XPATHS, false);
    let raw = extract_metainfo(&subtree, AUTHOR_XPATHS, 120)?;
    let normalized = normalize_authors(None, &raw)?;
    if !blacklist.is_empty() {
        check_authors(&normalized, blacklist)
    } else {
        Some(normalized)
    }
}

/// `extract_description(tree)` — currently a no-op (Stage 7a relies on
/// `examine_meta`'s OG / meta-name pass to fill `Metadata.description`).
///
/// The Python source does not expose a standalone `extract_description`;
/// description is populated EXCLUSIVELY from the meta-tag pass at
/// `metadata.py:243-296`. The function exists in our public API to match
/// the brief's signature contract and to provide a hook point for a
/// future XPath-based description rescue (none in `metadata.py` today).
fn extract_description(_doc: &Dom) -> Option<String> {
    None
}

// ===========================================================================
// "today" source (metadata.py:586 / settings.py:202)
// ===========================================================================

/// Today's date as `(year, month, day)` in UTC.
///
/// Python's `set_date_params` (`settings.py:202`) computes
/// `datetime.now().strftime("%Y-%m-%d")` — *local* civil date — and uses it
/// for BOTH `filedate` (`metadata.py:586`) and htmldate's `max_date` upper
/// bound (`metadata.py:546`). mdrcel is otherwise fully deterministic, but
/// reproducing Python's "today" is the ONLY way to byte-match the
/// `<date type="download">` filedate and to reject post-today garbage dates in
/// htmldate. We compute the UTC civil date from the system clock via Howard
/// Hinnant's `civil_from_days` algorithm (no new dependency).
///
/// Caveat (documented in the M8 journal): we use UTC, Python uses local. On
/// this UTC+1 host the two civil dates agree except in the ~1h window around
/// local midnight (UTC 23:00–00:00), where mdrcel would be one day behind. The
/// live TEI gate runs both sides within milliseconds, so this only flakes if a
/// run straddles that window — an accepted residual versus adding a timezone
/// dependency.
fn today_utc() -> (i32, u32, u32) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    civil_from_days(days)
}

/// Howard Hinnant's `civil_from_days`: convert a count of days since the Unix
/// epoch (1970-01-01) to a `(year, month, day)` Gregorian civil date.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    let year = (y + i64::from(m <= 2)) as i32;
    (year, m, d)
}

/// Format a `(year, month, day)` tuple as Python's `%Y-%m-%d`.
fn ymd_to_iso((y, m, d): (i32, u32, u32)) -> String {
    format!("{y:04}-{m:02}-{d:02}")
}

// ===========================================================================
// extract_metadata orchestrator (metadata.py:482-589)
// ===========================================================================

/// `extract_metadata(filecontent, default_url, date_config, extensive,
/// author_blacklist)` (`metadata.py:482-589`).
///
/// Stage 7a's scope: parse the HTML, run `examine_meta`, then fall back
/// to the XPath / `<title>` / `<h1>` / `<h2>` extractors for any field
/// still empty. JSON-LD (`extract_meta_json`), URL canonicalization, and
/// date extraction are stubbed (see the module header).
///
/// `extensive` is accepted but currently unused (Stage 7d's date extractor
/// will consume it via the `htmldate` analogue). `default_url` is also
/// stubbed at Stage 7a — Stage 7d will use it when the document carries
/// no canonical URL.
pub fn extract_metadata(
    html: &str,
    default_url: Option<&str>,
    extensive: bool,
    author_blacklist: &[String],
) -> Metadata {
    let _ = extensive;

    let dom = Dom::parse(html);
    let mut metadata = Metadata::default();

    // Python's `set_date_params` (settings.py:197-203) computes
    // `max_date = datetime.now().strftime("%Y-%m-%d")` ONCE and uses it for
    // both htmldate's upper bound (metadata.py:546) and `filedate`
    // (metadata.py:586). We mirror that: compute today once and share it.
    let today = today_utc();

    // <html lang="..."> (`metadata.py:Document.language` — populated by
    // json_metadata typically; Stage 7a takes it from the html element
    // directly, which is the simpler/equivalent path).
    if let Some(html_elem) = dom.root_element()
        && let Some(lang) = get_attribute(&html_elem, "lang")
    {
        let t = trim(&lang);
        if !t.is_empty() {
            metadata.language = Some(t);
        }
    }

    // 1. examine_meta — OG + meta-name + itemprop walk.
    examine_meta(&dom, &mut metadata);

    // 2. Python's `if metadata.author and " " not in metadata.author:
    //     metadata.author = None` (`metadata.py:514-515`).
    //    Drops single-word author candidates (one-name twitter handles
    //    etc.) before the XPath fallback runs.
    if let Some(ref author) = metadata.author
        && !author.contains(' ')
    {
        metadata.author = None;
    }

    // 2b. JSON-LD enrichment (Stage 7b — `metadata.py:519-520`
    //     `extract_meta_json` after the meta-tag walk but before the
    //     XPath fallbacks). JSON-LD fields overlay onto any still-empty
    //     `Metadata` field; populated HTML-tag fields are NOT
    //     overwritten (e.g. `metadata.title.is_none()` gate inside the
    //     walker). Always-populated fields per `json_metadata.py:67-138`:
    //     `site_name` (publisher.name unconditional overwrite at
    //     `json_metadata.py:72`), `author` (`merge_author` extends),
    //     `categories`/`tags` (extend when current empty), `date`
    //     (Stage 7b additive — Stage 7d will refine via htmldate),
    //     `image` (when empty), `pagetype` (when empty).
    crate::trafilatura::metadata_jsonld::extract_meta_json(&dom, &mut metadata);

    // 3. Title XPath fallback (`metadata.py:523-525`).
    if metadata.title.is_none() {
        metadata.title = extract_title(&dom);
    }

    // 4. Author blacklist re-check (`metadata.py:527-529`).
    if let Some(ref author) = metadata.author.clone()
        && !author_blacklist.is_empty()
    {
        metadata.author = check_authors(author, author_blacklist);
    }

    // 5. Author XPath fallback (`metadata.py:530-532`).
    if metadata.author.is_none() {
        metadata.author = extract_author(&dom, author_blacklist);
    }

    // 6. Re-check author against blacklist after XPath fallback
    //    (`metadata.py:534-535`).
    if let Some(ref author) = metadata.author.clone()
        && !author_blacklist.is_empty()
    {
        metadata.author = check_authors(author, author_blacklist);
    }

    // 7. Description XPath stub — not used at Stage 7a (the meta-tag pass
    //    already populates `description`). Kept here to match the brief's
    //    signature surface; future fold-in lands here.
    if metadata.description.is_none() {
        metadata.description = extract_description(&dom);
    }

    // 8. URL fallback (`metadata.py:538-539`). Only fires when earlier passes
    //    (og:url / twitter:url / JSON-LD) didn't already populate `metadata.url`.
    if metadata.url.is_none() {
        metadata.url = crate::trafilatura::metadata_url::extract_url(&dom, default_url);
    }

    // 9. Hostname from URL (`metadata.py:542-543`). Always fires when a URL is
    //    present (Python overwrites unconditionally). `extract_domain` returns
    //    the registered domain (leading subdomain / `www.` stripped).
    if let Some(ref url) = metadata.url {
        metadata.hostname = crate::trafilatura::metadata_url::extract_domain(url);
    }

    // 10. Date (`metadata.py:546-547`). Python assigns `metadata.date =
    //     find_date(tree, **date_config)` UNCONDITIONALLY — overwriting any
    //     JSON-LD `datePublished` set by `extract_meta_json` — so the final
    //     date is always htmldate's output (`%Y-%m-%d`, date-only, bounded at
    //     today). M8 fix: match that (was a `if date.is_none()` gate that let
    //     the raw JSON-LD timestamp win, e.g. `2002-06-06T01:53:27Z`).
    metadata.date = crate::trafilatura::metadata_url::extract_date(&dom, today);

    // 11. Sitename (`metadata.py:549-572`). If still empty, try the `<title>`
    //     separator-split (`extract_sitename`, metadata.py:550). Then normalise
    //     a present sitename (strip a leading `@`; title-case a dot-less,
    //     lower-initial name); ELSE derive it from the URL host via `META_URL`.
    if metadata.site_name.is_none() {
        let (_raw, first, second) = examine_title_element(&dom);
        for half in [first.as_deref(), second.as_deref()].into_iter().flatten() {
            if half.contains('.') {
                metadata.site_name = Some(half.to_string());
                break;
            }
        }
    }
    if let Some(sn) = metadata.site_name.take() {
        // `metadata.sitename.lstrip("@")` (metadata.py:560).
        let sn = sn.trim_start_matches('@').to_string();
        // `if "." not in sitename and not sitename[0].isupper(): .title()`
        // (metadata.py:562-567).
        let sn = if !sn.is_empty()
            && !sn.contains('.')
            && !sn.chars().next().is_some_and(|c| c.is_uppercase())
        {
            python_title_case(&sn)
        } else {
            sn
        };
        metadata.site_name = Some(sn);
    } else if let Some(url) = metadata.url.as_deref() {
        // `mymatch = META_URL.match(url); sitename = mymatch[1]`
        // (metadata.py:569-572).
        metadata.site_name = crate::trafilatura::metadata_url::meta_url_sitename(url);
    }

    // 12. Categories fallback (Stage 7d, `metadata.py:575-576`).
    if metadata.categories.is_empty() {
        metadata.categories = crate::trafilatura::metadata_url::extract_catstags(&dom, "category");
    }

    // 13. Tags fallback (Stage 7d, `metadata.py:579-580`).
    if metadata.tags.is_empty() {
        metadata.tags = crate::trafilatura::metadata_url::extract_catstags(&dom, "tag");
    }

    // 14. License (Stage 7d, `metadata.py:583`). Unconditional in Python
    //     (the assignment always runs); Stage 7d preserves that — license
    //     is always re-derived from the document, since it's not populated
    //     anywhere else.
    metadata.license = crate::trafilatura::metadata_url::extract_license(&dom);

    // 15. filedate (`metadata.py:586`): `metadata.filedate =
    //     date_config["max_date"]` = today (`%Y-%m-%d`). M8 added the slot.
    metadata.filedate = Some(ymd_to_iso(today));

    // 16. M10 Phase 1 + Phase 2E: faithful port of Python's
    //     `metadata.clean_and_trim()` (`metadata.py:587` →
    //     `settings.py:289-299`). Applies 10,000-char length cap →
    //     `html.unescape` → control-char strip to every str-typed metadata
    //     slot in Python's exact order. Phase 1 ported the strip; Phase 2E
    //     closed out the cap + unescape (HLD §1.2 / §3 / §4).
    clean_and_trim_metadata(&mut metadata);

    metadata
}

/// Faithful port of Python's `metadata.clean_and_trim()`
/// (`metadata.py:587` → `settings.py:289-299`). Applies the 10,000-char
/// length cap with U+2026 ellipsis truncation, `html.unescape` (full
/// HTML5 entity table via `web_atoms::NAMED_ENTITIES`), and
/// `strip_control_chars` to every str-typed metadata slot in Python's
/// exact order (cap → unescape → strip). Phase 1 ported the strip;
/// Phase 2E (HLD §1.2 / §3 / §4) closed out the cap + unescape.
fn clean_and_trim_metadata(m: &mut Metadata) {
    fn process_slot(slot: &mut Option<String>) {
        if let Some(v) = slot.take() {
            // Step 1 — settings.py:295-296 — 10_000-char cap.
            let capped = cap_at_python_length(&v);
            // Step 2 — settings.py:298 — html.unescape.
            let unescaped = python_html_unescape(&capped);
            // Step 3 — line_processing's strip half (Phase 1).
            *slot = Some(strip_control_chars(&unescaped));
        }
    }
    process_slot(&mut m.title);
    process_slot(&mut m.author);
    process_slot(&mut m.url);
    process_slot(&mut m.hostname);
    process_slot(&mut m.description);
    process_slot(&mut m.site_name);
    process_slot(&mut m.date);
    process_slot(&mut m.language);
    process_slot(&mut m.image);
    process_slot(&mut m.pagetype);
    process_slot(&mut m.license);
    process_slot(&mut m.filedate);
    for c in m.categories.iter_mut() {
        let capped = cap_at_python_length(c);
        let unescaped = python_html_unescape(&capped);
        *c = strip_control_chars(&unescaped);
    }
    for t in m.tags.iter_mut() {
        let capped = cap_at_python_length(t);
        let unescaped = python_html_unescape(&capped);
        *t = strip_control_chars(&unescaped);
    }
}

// ===========================================================================
// M10 Phase 2E — html.unescape + 10,000-char cap helpers
// ===========================================================================

/// Python `value[:9999] + "…"` cap (`settings.py:295-296`).
///
/// Char count, NOT byte count: Python `len(str)` returns the number of
/// codepoints, so a 5_000-`'中'` string (15_000 bytes / 5_000 chars) does
/// NOT trigger truncation. The threshold is strict `>` — exactly 10_000
/// chars passes through unchanged.
///
/// When truncation fires: takes the first 9_999 chars, appends a single
/// `'\u{2026}'` (HORIZONTAL ELLIPSIS — NOT three ASCII dots), yielding a
/// string of exactly 10_000 chars.
fn cap_at_python_length(v: &str) -> Cow<'_, str> {
    if v.chars().count() <= 10_000 {
        return Cow::Borrowed(v);
    }
    let mut out: String = v.chars().take(9999).collect();
    out.push('\u{2026}');
    Cow::Owned(out)
}

/// Faithful port of CPython `html.unescape` (`html/__init__.py:91-132`).
///
/// Implements the regex `&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\f <&#;]{1,32};?)`
/// (`html/__init__.py:118-120`) as a hand-written scanner state machine for
/// `Cow`-friendly fast-path support. Each match is processed by an inline
/// equivalent of `_replace_charref` (`html/__init__.py:91-115`):
///
/// - **Numeric** (decimal `&#NN;` or hex `&#xHH;`): parse the integer, then
///   apply, in order, `_invalid_charrefs` (35-entry Windows-1252 substitution
///   table; e.g. `0x80` → `U+20AC` €), surrogate / overflow guard
///   (`0xD800..=0xDFFF` or `> 0x10FFFF` → `U+FFFD`), `_invalid_codepoints`
///   (~80-entry empty-string substitution set; e.g. `0x0B` → ""), then
///   `char::from_u32`.
/// - **Named**: look up the full body in `web_atoms::NAMED_ENTITIES` (2231
///   entries; byte-equal to Python `html.entities.html5`). If absent, peel
///   chars off the right and retry until length 2 — the longest-prefix
///   fallback that turns `&notreal;` into `¬real;` per the HTML5 standard.
///   If no prefix matches, return `&` + body verbatim.
///
/// Fast path: returns `Cow::Borrowed(s)` when the input contains no `&`
/// (`html/__init__.py:130` `if '&' not in s: return s`).
pub(crate) fn python_html_unescape(s: &str) -> Cow<'_, str> {
    if !s.contains('&') {
        return Cow::Borrowed(s);
    }

    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'&' {
            // Push a single char (UTF-8 safe).
            let ch_start = i;
            // Find char boundary: scan past this UTF-8 sequence.
            i += 1;
            while i < bytes.len() && (bytes[i] & 0xC0) == 0x80 {
                i += 1;
            }
            out.push_str(&s[ch_start..i]);
            continue;
        }

        // Try to match the `_charref` regex starting at i.
        // (`html/__init__.py:118-120`)
        match scan_charref(bytes, i) {
            Some((body_start, body_end_excl)) => {
                // body is s[body_start..body_end_excl], match consumed
                // i..body_end_excl (`body_start` is i+1).
                let body = &s[body_start..body_end_excl];
                replace_charref(body, &mut out);
                i = body_end_excl;
            }
            None => {
                // Bare `&` that did not match `_charref` — copy verbatim.
                out.push('&');
                i += 1;
            }
        }
    }

    Cow::Owned(out)
}

/// Scan a `_charref` match starting at `i` (the `&` byte). Returns the
/// inclusive-start / exclusive-end byte indices of the captured group
/// `body` (everything after the leading `&`, including the optional `;`).
///
/// Mirrors the alternation order of the regex
/// `&(#[0-9]+;?|#[xX][0-9a-fA-F]+;?|[^\t\n\f <&#;]{1,32};?)`
/// (`html/__init__.py:118-120`).
fn scan_charref(bytes: &[u8], i: usize) -> Option<(usize, usize)> {
    debug_assert_eq!(bytes[i], b'&');
    let body_start = i + 1;
    if body_start >= bytes.len() {
        return None;
    }

    if bytes[body_start] == b'#' {
        // Numeric branch.
        // Need at least one digit after `#` (decimal) or `#x` / `#X` (hex).
        let after_hash = body_start + 1;
        if after_hash >= bytes.len() {
            return None;
        }
        let (digits_start, is_hex) = if bytes[after_hash] == b'x' || bytes[after_hash] == b'X' {
            (after_hash + 1, true)
        } else {
            (after_hash, false)
        };
        let mut p = digits_start;
        while p < bytes.len()
            && if is_hex {
                bytes[p].is_ascii_hexdigit()
            } else {
                bytes[p].is_ascii_digit()
            }
        {
            p += 1;
        }
        if p == digits_start {
            // No digit followed — not a numeric charref.
            return None;
        }
        // Optional trailing `;`.
        if p < bytes.len() && bytes[p] == b';' {
            p += 1;
        }
        Some((body_start, p))
    } else {
        // Named branch: `[^\t\n\f <&#;]{1,32};?`.
        let mut p = body_start;
        let max_end = (body_start + 32).min(bytes.len());
        while p < max_end {
            let b = bytes[p];
            if matches!(b, b'\t' | b'\n' | 0x0C | b' ' | b'<' | b'&' | b'#' | b';') {
                break;
            }
            p += 1;
        }
        if p == body_start {
            // Zero chars matched.
            return None;
        }
        // Optional trailing `;`.
        if p < bytes.len() && bytes[p] == b';' {
            p += 1;
        }
        Some((body_start, p))
    }
}

/// Inline equivalent of Python's `_replace_charref(s)`
/// (`html/__init__.py:91-115`). `body` is the captured group (no leading
/// `&`). Appends the decoded replacement to `out`.
fn replace_charref(body: &str, out: &mut String) {
    debug_assert!(!body.is_empty());
    let first = body.as_bytes()[0];
    if first == b'#' {
        // Numeric charref. `html/__init__.py:93-105`.
        // Strip the trailing `;` (if any) then parse.
        let mut digits_part = &body[1..];
        if digits_part.ends_with(';') {
            digits_part = &digits_part[..digits_part.len() - 1];
        }
        let num = if let Some(rest) = digits_part
            .strip_prefix('x')
            .or_else(|| digits_part.strip_prefix('X'))
        {
            u32::from_str_radix(rest, 16).ok()
        } else {
            digits_part.parse::<u32>().ok()
        };
        // Python's int() does not overflow; on Rust an out-of-range
        // value would not fit in u32. Treat as undecodable -> verbatim.
        let Some(num) = num else {
            out.push('&');
            out.push_str(body);
            return;
        };
        if let Some(rep) = invalid_charref_replacement(num) {
            out.push_str(rep);
            return;
        }
        if (0xD800..=0xDFFF).contains(&num) || num > 0x10FFFF {
            out.push('\u{FFFD}');
            return;
        }
        if is_invalid_codepoint(num) {
            // Empty-string substitution (`html/__init__.py:103-104`).
            return;
        }
        // Should always succeed given the guards above.
        if let Some(ch) = char::from_u32(num) {
            out.push(ch);
        } else {
            // Defensive: shouldn't fire — guards above cover surrogates
            // and >0x10FFFF, the only `char::from_u32` failure modes.
            out.push('\u{FFFD}');
        }
    } else {
        // Named charref. `html/__init__.py:106-115`.
        //
        // NOTE on `web_atoms::NAMED_ENTITIES` vs Python's `html5`: the
        // PHF map has 9854 entries vs Python's 2231 because web_atoms
        // also stores **prefix sentinels** (e.g. `"a"`, `"am"`, `"AM"`,
        // any partial path along the HTML5 entity-trie) with the
        // value `(0, 0)`. Python's table has no such sentinels —
        // `html5.get("am")` is `None`. We treat `(cp1=0, cp2=0)` as
        // "not a real entity" (no real HTML5 entity decodes to U+0000,
        // and the only `_invalid_charrefs[0x00]` substitution is on the
        // NUMERIC path, not the named one). This restores Python's
        // semantics for both the direct lookup AND the longest-prefix
        // descent (which would otherwise hit a sentinel and emit
        // garbage instead of falling through to the next prefix).
        if let Some(decoded) = lookup_named_entity(body) {
            out.push_str(&decoded);
            return;
        }
        // Longest-prefix descent (`html/__init__.py:110-113`):
        //   for x in range(len(s)-1, 1, -1):
        //       if s[:x] in _html5: return _html5[s[:x]] + s[x:]
        // x iterates as char-count len-1 .. 2 (Python's `len(str)` is
        // char count). For HTML5 named entities the body is pure ASCII
        // (entity-name alphabet is alphanumerics + optional trailing
        // `;`), so byte-count == char-count and slicing by byte index
        // is equivalent for ASCII bodies; non-ASCII bodies fall through
        // to a char-by-char peel.
        if body.is_ascii() {
            let len = body.len();
            // Python `range(len(s)-1, 1, -1)` yields len-1, len-2, …, 2.
            for x in (2..len).rev() {
                let prefix = &body[..x];
                if let Some(decoded) = lookup_named_entity(prefix) {
                    out.push_str(&decoded);
                    out.push_str(&body[x..]);
                    return;
                }
            }
        } else {
            // Non-ASCII body: char-by-char peel.
            let chars: Vec<char> = body.chars().collect();
            let n = chars.len();
            for x in (2..n).rev() {
                let prefix: String = chars[..x].iter().collect();
                if let Some(decoded) = lookup_named_entity(&prefix) {
                    out.push_str(&decoded);
                    let tail: String = chars[x..].iter().collect();
                    out.push_str(&tail);
                    return;
                }
            }
        }
        // No prefix matched — return `&` + body (`html/__init__.py:115`).
        out.push('&');
        out.push_str(body);
    }
}

/// Look up a candidate name in `web_atoms::NAMED_ENTITIES`, treating the
/// table's `(0, 0)` prefix sentinels as "not a real entity" (see the
/// extended comment in `replace_charref`'s named branch). Returns the
/// decoded 1- or 2-codepoint replacement string, or `None` if the name
/// is absent or is a prefix sentinel.
fn lookup_named_entity(name: &str) -> Option<String> {
    let (cp1, cp2) = web_atoms::NAMED_ENTITIES.get(name).copied()?;
    if cp1 == 0 && cp2 == 0 {
        // Prefix sentinel — not a real entity in Python's html5 table.
        return None;
    }
    let mut s = String::with_capacity(8);
    if let Some(ch) = char::from_u32(cp1) {
        s.push(ch);
    }
    if cp2 != 0
        && let Some(ch) = char::from_u32(cp2)
    {
        s.push(ch);
    }
    Some(s)
}

/// Python `_invalid_charrefs` (`html/__init__.py:30-65`). 35 entries —
/// Windows-1252 punctuation substitutions for the C1 range plus a few
/// oddballs (`0x00` → `U+FFFD`, `0x0D` → `\r`).
fn invalid_charref_replacement(num: u32) -> Option<&'static str> {
    match num {
        0x00 => Some("\u{FFFD}"), // REPLACEMENT CHARACTER
        0x0D => Some("\r"),       // CARRIAGE RETURN
        0x80 => Some("\u{20AC}"), // EURO SIGN
        0x81 => Some("\u{0081}"), // <control>
        0x82 => Some("\u{201A}"), // SINGLE LOW-9 QUOTATION MARK
        0x83 => Some("\u{0192}"), // LATIN SMALL LETTER F WITH HOOK
        0x84 => Some("\u{201E}"), // DOUBLE LOW-9 QUOTATION MARK
        0x85 => Some("\u{2026}"), // HORIZONTAL ELLIPSIS
        0x86 => Some("\u{2020}"), // DAGGER
        0x87 => Some("\u{2021}"), // DOUBLE DAGGER
        0x88 => Some("\u{02C6}"), // MODIFIER LETTER CIRCUMFLEX ACCENT
        0x89 => Some("\u{2030}"), // PER MILLE SIGN
        0x8A => Some("\u{0160}"), // LATIN CAPITAL LETTER S WITH CARON
        0x8B => Some("\u{2039}"), // SINGLE LEFT-POINTING ANGLE QUOTATION MARK
        0x8C => Some("\u{0152}"), // LATIN CAPITAL LIGATURE OE
        0x8D => Some("\u{008D}"), // <control>
        0x8E => Some("\u{017D}"), // LATIN CAPITAL LETTER Z WITH CARON
        0x8F => Some("\u{008F}"), // <control>
        0x90 => Some("\u{0090}"), // <control>
        0x91 => Some("\u{2018}"), // LEFT SINGLE QUOTATION MARK
        0x92 => Some("\u{2019}"), // RIGHT SINGLE QUOTATION MARK
        0x93 => Some("\u{201C}"), // LEFT DOUBLE QUOTATION MARK
        0x94 => Some("\u{201D}"), // RIGHT DOUBLE QUOTATION MARK
        0x95 => Some("\u{2022}"), // BULLET
        0x96 => Some("\u{2013}"), // EN DASH
        0x97 => Some("\u{2014}"), // EM DASH
        0x98 => Some("\u{02DC}"), // SMALL TILDE
        0x99 => Some("\u{2122}"), // TRADE MARK SIGN
        0x9A => Some("\u{0161}"), // LATIN SMALL LETTER S WITH CARON
        0x9B => Some("\u{203A}"), // SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
        0x9C => Some("\u{0153}"), // LATIN SMALL LIGATURE OE
        0x9D => Some("\u{009D}"), // <control>
        0x9E => Some("\u{017E}"), // LATIN SMALL LETTER Z WITH CARON
        0x9F => Some("\u{0178}"), // LATIN CAPITAL LETTER Y WITH DIAERESIS
        _ => None,
    }
}

/// Python `_invalid_codepoints` (`html/__init__.py:67-88`). Dense ranges;
/// `matches!` keeps the spec literal.
fn is_invalid_codepoint(num: u32) -> bool {
    matches!(
        num,
        // 0x0001..=0x0008
        0x01..=0x08
        // 0x000B (note: 0x09/0x0A/0x0C are whitespace, NOT in the invalid set)
        | 0x0B
        // 0x000E..=0x001F
        | 0x0E..=0x1F
        // 0x007F..=0x009F
        | 0x7F..=0x9F
        // 0xFDD0..=0xFDEF
        | 0xFDD0..=0xFDEF
        // plane-end non-characters: 0xnFFFE / 0xnFFFF for n = 0..=16
        | 0xFFFE | 0xFFFF
        | 0x1FFFE | 0x1FFFF
        | 0x2FFFE | 0x2FFFF
        | 0x3FFFE | 0x3FFFF
        | 0x4FFFE | 0x4FFFF
        | 0x5FFFE | 0x5FFFF
        | 0x6FFFE | 0x6FFFF
        | 0x7FFFE | 0x7FFFF
        | 0x8FFFE | 0x8FFFF
        | 0x9FFFE | 0x9FFFF
        | 0xAFFFE | 0xAFFFF
        | 0xBFFFE | 0xBFFFF
        | 0xCFFFE | 0xCFFFF
        | 0xDFFFE | 0xDFFFF
        | 0xEFFFE | 0xEFFFF
        | 0xFFFFE | 0xFFFFF
        | 0x10FFFE | 0x10FFFF
    )
}

/// Python `str.title()` for ASCII-ish sitenames (`metadata.py:567`).
///
/// Uppercases the first cased character of each run of cased characters and
/// lowercases the rest; non-cased characters (digits, `-`, `.`, spaces) act as
/// word boundaries. E.g. `"rustlang"` → `"Rustlang"`, `"rust-lang"` →
/// `"Rust-Lang"`. Faithful enough for the sitename normalisation, which only
/// reaches this for dot-less, lower-initial names.
fn python_title_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_cased = false;
    for ch in s.chars() {
        let is_cased = ch.is_alphabetic();
        if is_cased && !prev_cased {
            out.extend(ch.to_uppercase());
        } else if is_cased {
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
        prev_cased = is_cased;
    }
    out
}

// ===========================================================================
// Re-exports kept narrow (per brief): only `Metadata` and `extract_metadata`
// need to be public to satisfy the dispatcher contract. The other XPath
// helpers stay private — Stage 7b can flip them to `pub(crate)` if it
// needs cross-module access.
// ===========================================================================

// Silence dead-code warnings for the unused-at-Stage-7a `element_text`
// import that future sub-stages will consume (Stage 7b's JSON-LD walker
// needs it for `<script type="application/ld+json">` text reads).
#[allow(dead_code)]
fn _stage7a_reserved_imports() {
    let _ = element_text as fn(&NodeRef) -> Option<String>;
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_extracts_og_title() {
        let html = r#"<html><head>
            <meta property="og:title" content="My Article">
            <title>Site</title>
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.title.as_deref(), Some("My Article"));
    }

    #[test]
    fn metadata_extracts_meta_author() {
        let html = r#"<html><head>
            <meta name="author" content="Jane Doe">
            <title>x</title>
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn metadata_falls_back_to_title_tag() {
        // No OG tag; just <title>My Site | My Article</title>. The HTMLTITLE
        // separator regex matches " | ", splits into ("My Site", "My
        // Article"). The pick rule prefers a half WITHOUT "." — neither
        // contains ".", so the FIRST half "My Site" is returned (per the
        // for-loop's iteration order at `metadata.py:365-367`).
        let html = r#"<html><head>
            <title>My Site | My Article</title>
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        // The title is set from the <title>-split (not OG) at the
        // examine_title_element / extract_title step. The Python source
        // takes the first non-"." half it iterates; we faithfully return
        // "My Site".
        assert_eq!(m.title.as_deref(), Some("My Site"));
    }

    #[test]
    fn metadata_extracts_h1_when_no_title_or_meta() {
        // No <title>, no OG, just <h1>Heading</h1>.
        let html = r#"<html><head></head><body><h1>Heading</h1></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        // Single-h1 rule (extract_title step 1).
        assert_eq!(m.title.as_deref(), Some("Heading"));
    }

    #[test]
    fn metadata_extracts_description_from_og() {
        let html = r#"<html><head>
            <meta property="og:description" content="A short summary of the article.">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(
            m.description.as_deref(),
            Some("A short summary of the article.")
        );
    }

    #[test]
    fn metadata_extracts_site_name() {
        let html = r#"<html><head>
            <meta property="og:site_name" content="Example Site">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.site_name.as_deref(), Some("Example Site"));
    }

    #[test]
    fn metadata_respects_author_blacklist() {
        let html = r#"<html><head>
            <meta name="author" content="Spam Site">
            </head><body><p>x</p></body></html>"#;
        let blacklist = vec!["Spam Site".to_string()];
        let m = extract_metadata(html, None, true, &blacklist);
        assert!(
            m.author.is_none(),
            "blacklisted author should be filtered, got {:?}",
            m.author
        );
    }

    #[test]
    fn metadata_extracts_language() {
        let html = r#"<html lang="en"><head><title>x</title></head>
            <body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.language.as_deref(), Some("en"));
    }

    #[test]
    fn metadata_extracts_image_from_og() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/cover.jpg">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(
            m.image.as_deref(),
            Some("https://example.com/cover.jpg")
        );
    }

    #[test]
    fn metadata_normalizes_whitespace_in_author() {
        // The Python source's `examine_meta` calls `HTML_STRIP_TAGS.sub("",
        // ...).strip()` on the content attribute — extra whitespace is
        // collapsed by `trim`.
        let html = r#"<html><head>
            <meta name="author" content="  Jane    Doe  ">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    // ---- Internal helper tests --------------------------------------------

    #[test]
    fn split_title_on_separators_strips_trailing_site_name() {
        // " | " is the separator; leftmost match wins.
        assert_eq!(
            split_title_on_separators("My Article | My Site"),
            "My Article"
        );
    }

    #[test]
    fn split_title_on_separators_returns_input_when_no_separator() {
        assert_eq!(split_title_on_separators("Untitled"), "Untitled");
    }

    #[test]
    fn split_title_on_separators_recognises_em_dash() {
        assert_eq!(
            split_title_on_separators("Article Headline – Publisher Name"),
            "Article Headline"
        );
    }

    #[test]
    fn normalize_authors_rejects_urls() {
        let out = normalize_authors(None, "https://example.com/by/jane");
        assert_eq!(out, None);
    }

    #[test]
    fn normalize_authors_rejects_emails() {
        let out = normalize_authors(None, "jane@example.com");
        assert_eq!(out, None);
    }

    #[test]
    fn normalize_authors_strips_html_tags() {
        let out = normalize_authors(None, "<span>Jane Doe</span>");
        assert_eq!(out.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn normalize_authors_joins_multiple_with_semicolon() {
        let out = normalize_authors(Some("Jane Doe"), "John Smith");
        assert_eq!(out.as_deref(), Some("Jane Doe; John Smith"));
    }

    #[test]
    fn normalize_authors_dedupes_exact_match() {
        let out = normalize_authors(Some("Jane Doe"), "Jane Doe");
        assert_eq!(out.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn normalize_tags_strips_quotes() {
        assert_eq!(normalize_tags(r#""rust", "trafilatura""#), "rust, trafilatura");
    }

    #[test]
    fn check_authors_keeps_non_blacklisted() {
        let kept = check_authors("Jane Doe; Spam Site", &["Spam Site".to_string()]);
        assert_eq!(kept.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn check_authors_returns_none_when_all_blacklisted() {
        let out = check_authors(
            "Spam Site",
            &["Spam Site".to_string(), "Other".to_string()],
        );
        assert_eq!(out, None);
    }

    #[test]
    fn examine_meta_handles_itemprop_headline_and_author() {
        let html = r#"<html><head>
            <meta itemprop="headline" content="Itemprop Title">
            <meta itemprop="author" content="Itemprop Author">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.title.as_deref(), Some("Itemprop Title"));
        assert_eq!(m.author.as_deref(), Some("Itemprop Author"));
    }

    #[test]
    fn examine_meta_article_tag_populates_tags() {
        let html = r#"<html><head>
            <meta property="article:tag" content="rust, web, html">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.tags, vec!["rust, web, html".to_string()]);
    }

    #[test]
    fn examine_meta_og_type_populates_pagetype() {
        let html = r#"<html><head>
            <meta property="og:type" content="article">
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.pagetype.as_deref(), Some("article"));
    }

    #[test]
    fn extract_metadata_combines_html_and_jsonld() {
        // OG provides the title; JSON-LD provides the author.
        // Both populated post-extract — verifies the Stage 7b wiring
        // does not clobber Stage 7a's OG title.
        let html = r#"<html><head>
            <meta property="og:title" content="OG Title Wins">
            <script type="application/ld+json">
            {"@context": "https://schema.org", "@type": "NewsArticle",
             "headline": "JSON-LD Title Loses",
             "author": "Jane Doe"}
            </script>
            </head><body><p>x</p></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.title.as_deref(), Some("OG Title Wins"));
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
    }

    #[test]
    fn metadata_default_is_all_none() {
        let m = Metadata::default();
        assert!(m.title.is_none());
        assert!(m.author.is_none());
        assert!(m.url.is_none());
        assert!(m.hostname.is_none());
        assert!(m.description.is_none());
        assert!(m.site_name.is_none());
        assert!(m.date.is_none());
        assert!(m.categories.is_empty());
        assert!(m.tags.is_empty());
        assert!(m.language.is_none());
        assert!(m.image.is_none());
        assert!(m.pagetype.is_none());
        assert!(m.license.is_none());
    }

    // -------------------------------------------------------------------
    // clean_and_trim_metadata (M10 Phase 1, HLD §6a-bis) — 5 tests
    // -------------------------------------------------------------------

    #[test]
    fn clean_and_trim_metadata_strips_cf_from_title() {
        let mut m = Metadata {
            title: Some("Hello\u{200B}World".into()),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(m.title.as_deref(), Some("HelloWorld"));
    }

    #[test]
    fn clean_and_trim_metadata_strips_invisible_separator_from_description() {
        // The 507b9cdb fixture's exact pattern, applied to a description slot.
        let mut m = Metadata {
            description: Some("iPadOS 15\u{2063}\u{2063}, il".into()),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(m.description.as_deref(), Some("iPadOS 15, il"));
    }

    #[test]
    fn clean_and_trim_metadata_strips_per_category_and_tag_entry() {
        // Belt-and-braces over JSON-LD-sourced categories that bypass
        // extract_catstags's line_processing.
        let mut m = Metadata {
            categories: vec!["news".into(), "sport\u{00AD}s".into()],
            tags: vec!["foo\u{2063}".into(), "bar".into()],
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(m.categories, vec!["news".to_string(), "sports".to_string()]);
        assert_eq!(m.tags, vec!["foo".to_string(), "bar".to_string()]);
    }

    #[test]
    fn clean_and_trim_metadata_leaves_none_slots_none() {
        let mut m = Metadata::default();
        clean_and_trim_metadata(&mut m);
        assert!(m.title.is_none());
        assert!(m.author.is_none());
        assert!(m.url.is_none());
        assert!(m.hostname.is_none());
        assert!(m.description.is_none());
        assert!(m.site_name.is_none());
        assert!(m.date.is_none());
        assert!(m.language.is_none());
        assert!(m.image.is_none());
        assert!(m.pagetype.is_none());
        assert!(m.license.is_none());
        assert!(m.filedate.is_none());
        assert!(m.categories.is_empty());
        assert!(m.tags.is_empty());
    }

    #[test]
    fn clean_and_trim_metadata_preserves_clean_slots_byte_equal() {
        let mut m = Metadata {
            title: Some("My Article".into()),
            author: Some("Jane Doe".into()),
            url: Some("https://example.com/post".into()),
            hostname: Some("example.com".into()),
            description: Some("A short summary.".into()),
            site_name: Some("Example Site".into()),
            date: Some("2026-05-23".into()),
            language: Some("en".into()),
            image: Some("https://example.com/cover.jpg".into()),
            pagetype: Some("article".into()),
            license: Some("CC-BY-4.0".into()),
            filedate: Some("2026-05-23".into()),
            categories: vec!["news".into()],
            tags: vec!["rust".into(), "café".into()],
        };
        let before = m.clone();
        clean_and_trim_metadata(&mut m);
        assert_eq!(m, before);
    }

    // -------------------------------------------------------------------
    // M10 Phase 2E — cap_at_python_length (HLD §6.1)
    // -------------------------------------------------------------------

    #[test]
    fn cap_at_python_length_passes_short_input_unchanged() {
        let v = "hello world";
        let out = cap_at_python_length(v);
        assert_eq!(out, "hello world");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn cap_at_python_length_passes_exactly_10000_chars_unchanged() {
        // Strict `>` boundary per Python `if len(value) > 10000`.
        let v: String = "a".repeat(10_000);
        let out = cap_at_python_length(&v);
        assert_eq!(out.chars().count(), 10_000);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.as_ref(), v.as_str());
    }

    #[test]
    fn cap_at_python_length_truncates_at_9999_with_ellipsis() {
        let v: String = "a".repeat(10_001);
        let out = cap_at_python_length(&v);
        // Final length exactly 10_000 chars: 9999 a's + 1 ellipsis.
        assert_eq!(out.chars().count(), 10_000);
        assert!(matches!(out, Cow::Owned(_)));
        // Last char is U+2026, NOT three ASCII dots.
        let last = out.chars().last().expect("non-empty");
        assert_eq!(last, '\u{2026}');
        // First 9999 chars are 'a's.
        let prefix: String = out.chars().take(9999).collect();
        assert_eq!(prefix, "a".repeat(9999));
    }

    #[test]
    fn cap_at_python_length_uses_char_count_not_byte_count() {
        // 5_000 `'中'` is 15_000 bytes but only 5_000 chars; must pass
        // through (≤ 10_000 chars).
        let v: String = "中".repeat(5_000);
        assert_eq!(v.len(), 15_000); // 3-byte UTF-8
        assert_eq!(v.chars().count(), 5_000);
        let out = cap_at_python_length(&v);
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out.chars().count(), 5_000);
    }

    // -------------------------------------------------------------------
    // M10 Phase 2E — python_html_unescape (HLD §6.2)
    // -------------------------------------------------------------------

    #[test]
    fn python_html_unescape_passthrough_no_ampersand() {
        let v = "hello world";
        let out = python_html_unescape(v);
        assert_eq!(out, "hello world");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn python_html_unescape_named_with_semicolon() {
        // Headline base case: the XML-mandatory five.
        let out = python_html_unescape("&amp; &lt; &gt; &quot; &apos;");
        assert_eq!(out, "& < > \" '");
    }

    #[test]
    fn python_html_unescape_named_without_semicolon_legacy() {
        // Python regex `;?` allows missing `;` on legacy entities.
        // `&amp` (no semicolon) is one of the 106 legacy entries.
        let out = python_html_unescape("AT&amp T");
        assert_eq!(out, "AT& T");
    }

    #[test]
    fn python_html_unescape_numeric_decimal() {
        let out = python_html_unescape("&#8230;");
        assert_eq!(out, "\u{2026}");
    }

    #[test]
    fn python_html_unescape_numeric_hex() {
        let out = python_html_unescape("&#x2026;");
        assert_eq!(out, "\u{2026}");
    }

    #[test]
    fn python_html_unescape_longest_prefix_fallback() {
        // `&notreal;` is not in _html5; peel chars to find `not`
        // (which decodes to U+00AC NOT SIGN). Remainder `real;` is
        // appended verbatim.
        let out = python_html_unescape("&notreal;");
        assert_eq!(out, "\u{00AC}real;");
    }

    #[test]
    fn python_html_unescape_windows_1252_substitution() {
        // `_invalid_charrefs[0x80]` -> U+20AC EURO SIGN.
        let out = python_html_unescape("&#x80;");
        assert_eq!(out, "\u{20AC}");
    }

    #[test]
    fn python_html_unescape_surrogate_yields_fffd() {
        let out = python_html_unescape("&#xD800;");
        assert_eq!(out, "\u{FFFD}");
    }

    #[test]
    fn python_html_unescape_invalid_codepoint_yields_empty_string() {
        // `0x0B` is in _invalid_codepoints -> empty string substitution.
        // Surrounding text remains.
        let out = python_html_unescape("&#x000B;x");
        assert_eq!(out, "x");
    }

    #[test]
    fn python_html_unescape_bare_ampersand_no_match() {
        // Bare `&` followed by space doesn't match `_charref` (space is
        // in the excluded set for named-entity char class).
        let out = python_html_unescape("a & b");
        assert_eq!(out, "a & b");
    }

    #[test]
    fn python_html_unescape_empty_input() {
        let out = python_html_unescape("");
        assert_eq!(out, "");
        assert!(matches!(out, Cow::Borrowed(_)));
    }

    #[test]
    fn python_html_unescape_two_codepoint_entity() {
        // `&acE;` decodes to U+223E U+0333.
        let out = python_html_unescape("&acE;");
        assert_eq!(out, "\u{223E}\u{0333}");
    }

    #[test]
    fn python_html_unescape_longest_named_entity() {
        // The 32-char ceiling case.
        let out = python_html_unescape("&CounterClockwiseContourIntegral;");
        assert_eq!(out, "\u{2233}");
    }

    // -------------------------------------------------------------------
    // M10 Phase 2E — clean_and_trim_metadata combined behaviour (HLD §6.3)
    // -------------------------------------------------------------------

    #[test]
    fn clean_and_trim_metadata_unescapes_title_entity() {
        // Step 2 in isolation.
        let mut m = Metadata {
            title: Some("AT&amp;T News".into()),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(m.title.as_deref(), Some("AT&T News"));
    }

    #[test]
    fn clean_and_trim_metadata_caps_long_description() {
        // Step 1 in isolation: 10_001 chars truncates to 9999 + U+2026.
        let long: String = "a".repeat(10_001);
        let mut m = Metadata {
            description: Some(long),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        let desc = m.description.expect("set");
        assert_eq!(desc.chars().count(), 10_000);
        assert_eq!(desc.chars().last(), Some('\u{2026}'));
    }

    #[test]
    fn clean_and_trim_metadata_order_cap_before_unescape_truncates_entity() {
        // Order keystone (HLD §1.2 / §3): cap precedes unescape. A title
        // of 10_002 chars (`"x" * 9996` + `"&amp;Y"`) truncates to the
        // first 9999 chars (= "x" * 9996 + "&am") then appends ellipsis,
        // leaving "&am" as a dangling entity prefix. unescape then sees
        // "&am" (no semicolon, no longest-prefix down to length 2
        // matches) so returns it verbatim — exactly what Python does at
        // settings.py:295-298.
        let mut title = "x".repeat(9996);
        title.push_str("&amp;Y");
        assert_eq!(title.chars().count(), 10_002);
        let mut m = Metadata {
            title: Some(title),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        let out = m.title.expect("set");
        // 9999 chars before ellipsis: "x" * 9996 + "&am" then '\u{2026}'.
        assert_eq!(out.chars().count(), 10_000);
        assert_eq!(out.chars().last(), Some('\u{2026}'));
        let prefix: String = out.chars().take(9999).collect();
        let mut expected = "x".repeat(9996);
        expected.push_str("&am");
        assert_eq!(prefix, expected);
    }

    #[test]
    fn clean_and_trim_metadata_order_unescape_before_strip() {
        // Order keystone: unescape -> strip. `&amp;` decodes to `&`,
        // then the ZWSP (U+200B, Cf) is stripped.
        let mut m = Metadata {
            title: Some("&amp;\u{200B}".into()),
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(m.title.as_deref(), Some("&"));
    }

    #[test]
    fn clean_and_trim_metadata_applies_to_categories_and_tags() {
        // List-entry coverage: cap+unescape applies to every list slot too.
        let mut m = Metadata {
            categories: vec!["AT&amp;T".into(), "news".into()],
            tags: vec!["rust&lt;3".into(), "café".into()],
            ..Default::default()
        };
        clean_and_trim_metadata(&mut m);
        assert_eq!(
            m.categories,
            vec!["AT&T".to_string(), "news".to_string()]
        );
        assert_eq!(
            m.tags,
            vec!["rust<3".to_string(), "café".to_string()]
        );
    }

    #[test]
    fn clean_and_trim_metadata_idempotent_on_clean_input() {
        // Clean ASCII input with no entities and no control chars round-
        // trips through cap+unescape+strip byte-equal.
        let mut m = Metadata {
            title: Some("Clean Title".into()),
            description: Some("A clean description, no entities.".into()),
            categories: vec!["news".into(), "tech".into()],
            tags: vec!["rust".into()],
            ..Default::default()
        };
        let before = m.clone();
        clean_and_trim_metadata(&mut m);
        // Second pass — confirms idempotence.
        clean_and_trim_metadata(&mut m);
        assert_eq!(m, before);
    }
}
