//! `core` — sub-stage E port of `htmldate/core.py` (header walker, element
//! walkers, candidate selection).
//!
//! Source of truth: `htmldate@1.9.x/core.py` (vendored under
//! `C:\Users\marti\AppData\Roaming\Python\Python314\site-packages\htmldate\
//! core.py`). Every function / constant cites its exact Python source line
//! range per the M4 Stage 1 sub-stage E anti-inversion contract.
//!
//! Sub-stages A (settings + Extractor + trim_text), B (validators + DateTime),
//! C (regex catalogues), and D (extractors) supply the building blocks; this
//! sub-stage wires them into the meta-/abbr-/time-element walkers and the
//! candidate-selection heuristic:
//!
//! - `DATE_ATTRIBUTES` / `NAME_MODIFIED` / `PROPERTY_MODIFIED` /
//!   `ITEMPROP_ATTRS_ORIGINAL` / `ITEMPROP_ATTRS_MODIFIED` / `ITEMPROP_ATTRS` /
//!   `CLASS_ATTRS`                                                  (core.py:80-189)
//! - `NON_DIGITS_REGEX`                                             (core.py:191)
//! - `THREE_COMP_PATTERNS` table                                    (core.py:193-196)
//! - `examine_text`                                                 (core.py:199-212)
//! - `examine_date_elements`                                        (core.py:215-232)
//! - `examine_header`                                               (core.py:235-352)
//! - `select_candidate`                                             (core.py:355-407)
//! - `search_pattern`                                               (core.py:410-425)
//! - `compare_reference`                                            (core.py:428-440)
//! - `examine_abbr_elements`                                        (core.py:443-497)
//! - `examine_time_elements`                                        (core.py:500-562)
//! - `normalize_match`                                              (core.py:565-571)
//!
//! `search_page` and `find_date` (core.py:574-983) are deferred to sub-stage F
//! (the orchestrator-level work).
//!
//! # Faithful divergences (recorded — HLD §4 anti-inversion)
//!
//! ## `@lru_cache` on `compare_reference`
//!
//! Python `core.py:428` decorates `compare_reference` with
//! `@lru_cache(maxsize=CACHE_SIZE)`. The Rust port does NOT cache — every call
//! recomputes. Pure perf optimisation, observable result identical. Matches
//! sub-stage B/D's `@lru_cache`-deferral precedent.
//!
//! ## Counter -> HashMap<String, usize>
//!
//! Python `Counter[str]` becomes Rust `HashMap<String, usize>` (see sub-stage
//! B's `plausible_year_filter`). `Counter.most_common(N)` returns descending
//! order by count; ties broken by insertion order in CPython 3.7+ (dict
//! ordering). The Rust port iterates and sorts explicitly — see
//! `select_candidate`'s implementation comments.
//!
//! ## XPath axes used here
//!
//! `examine_header` iterates `.//meta` via the Stage 0b XPath engine
//! (`xpath_engine::evaluate(".//meta", tree)`). `examine_abbr_elements` and
//! `examine_time_elements` use `.//abbr` / `.//time` likewise. Every path is
//! within the Stage 0b operator catalog (descendant-or-self + bare tag); no
//! engine extension required.
//!
//! ## `examine_text` MAX_SEGMENT_LEN trimming
//!
//! Python `core.py:209` does `text = NON_DIGITS_REGEX.sub("", text[:MAX_SEGMENT_LEN])`.
//! `text[:MAX_SEGMENT_LEN]` is a CODEPOINT slice in Python 3. The Rust port
//! uses `chars().take(MAX_SEGMENT_LEN)` for the same codepoint semantics.

use std::collections::HashMap;

use regex::Regex;
use std::sync::OnceLock;

use super::extractors::{
    MAX_SEGMENT_LEN, extract_url_date, try_date_expr,
};
use super::regex_catalogues::{
    three_catch, three_loose_catch, three_loose_pattern, three_pattern,
};
use super::settings::MAX_POSSIBLE_CANDIDATES;
use super::utils::Extractor;
use super::validators::{
    DateInput, DateTime, check_extracted_reference, compare_values, is_valid_date,
    plausible_year_filter,
};

use crate::readability::dom::{
    NodeRef, get_attribute, get_elements_by_tag_name, text_content,
};
use crate::trafilatura::xpath_engine;

// ===========================================================================
// DATE_ATTRIBUTES (core.py:80-152) — meta-tag attribute values that signal
// a publication date
// ===========================================================================

/// Lowercased meta-tag attribute values signalling a publication date.
///
/// Ports `htmldate/core.py:80-152` verbatim. Python uses a `set[str]`; the
/// Rust port uses a `&[&str]` slice with a `contains` helper. All entries
/// already-lowercased to match Python's `elem.get("name", "").lower()` lookup
/// shape at `core.py:270` / `:287` / `:301`.
pub const DATE_ATTRIBUTES: &[&str] = &[
    "analyticsattributes.articledate",
    "article.created",
    "article_date_original",
    "article:post_date",
    "article.published",
    "article:published",
    "article:published_date",
    "article:published_time",
    "article:publicationdate",
    "bt:pubdate",
    "citation_date",
    "citation_publication_date",
    "content_create_date",
    "created",
    "cxenseparse:recs:publishtime",
    "date",
    "date_created",
    "date_published",
    "datecreated",
    "dateposted",
    "datepublished",
    // Dublin Core
    "dc.date",
    "dc.created",
    "dc.date.created",
    "dc.date.issued",
    "dc.date.publication",
    "dcsext.articlefirstpublished",
    "dcterms.created",
    "dcterms.date",
    "dcterms.issued",
    "dc:created",
    "dc:date",
    "displaydate",
    "doc_date",
    "field-name-post-date",
    "gentime",
    "mediator_published_time",
    "meta",
    // Open Graph
    "og:article:published",
    "og:article:published_time",
    "og:datepublished",
    "og:pubdate",
    "og:publish_date",
    "og:published_time",
    "og:question:published_time",
    "og:regdate",
    "originalpublicationdate",
    "parsely-pub-date",
    "pdate",
    "ptime",
    "pubdate",
    "publishdate",
    "publish_date",
    "publish_time",
    "publish-date",
    "published-date",
    "published_date",
    "published_time",
    "publisheddate",
    "publication_date",
    "rbpubdate",
    "release_date",
    "rnews:datepublished",
    "sailthru.date",
    "shareaholic:article_published_time",
    "timestamp",
    "twt-published-at",
    "video:release_date",
    "vr:published_time",
];

/// Lowercased meta-tag `name` attribute values that signal a *last-modified*
/// date (rather than a publication date).
///
/// Ports `htmldate/core.py:155-162` verbatim.
pub const NAME_MODIFIED: &[&str] = &[
    "lastdate",
    "lastmod",
    "lastmodified",
    "last-modified",
    "modified",
    "utime",
];

/// Lowercased meta-tag `property` attribute values that signal a
/// *last-modified* date.
///
/// Ports `htmldate/core.py:165-183` verbatim.
pub const PROPERTY_MODIFIED: &[&str] = &[
    "article:modified",
    "article:modified_date",
    "article:modified_time",
    "article:post_modified",
    "bt:moddate",
    "datemodified",
    "dc.modified",
    "dcterms.modified",
    "lastmodified",
    "modified_time",
    "modificationdate",
    "og:article:modified_time",
    "og:modified_time",
    "og:updated_time",
    "release_date",
    "revision_date",
    "updated_time",
];

/// Lowercased meta-tag `itemprop` values that signal a publication
/// (original) date.
///
/// Ports `htmldate/core.py:186` (`ITEMPROP_ATTRS_ORIGINAL =
/// {"datecreated", "datepublished", "pubyear"}`).
pub const ITEMPROP_ATTRS_ORIGINAL: &[&str] = &["datecreated", "datepublished", "pubyear"];

/// Lowercased meta-tag `itemprop` values that signal a modification date.
///
/// Ports `htmldate/core.py:187` (`ITEMPROP_ATTRS_MODIFIED =
/// {"datemodified", "dateupdate"}`).
pub const ITEMPROP_ATTRS_MODIFIED: &[&str] = &["datemodified", "dateupdate"];

/// Union of `ITEMPROP_ATTRS_ORIGINAL` and `ITEMPROP_ATTRS_MODIFIED`.
///
/// Ports `htmldate/core.py:188` (`ITEMPROP_ATTRS =
/// ITEMPROP_ATTRS_ORIGINAL.union(ITEMPROP_ATTRS_MODIFIED)`).
pub const ITEMPROP_ATTRS: &[&str] = &[
    "datecreated",
    "datepublished",
    "pubyear",
    "datemodified",
    "dateupdate",
];

/// Lowercased `class` attribute values signalling a date-bearing `<abbr>`
/// element.
///
/// Ports `htmldate/core.py:189` (`CLASS_ATTRS =
/// {"date-published", "published", "time published"}`).
pub const CLASS_ATTRS: &[&str] = &["date-published", "published", "time published"];

// ===========================================================================
// NON_DIGITS_REGEX (core.py:191)
// ===========================================================================

/// Regex matching one-or-more trailing non-digits at the end of a string.
///
/// Ports `htmldate/core.py:191` (`re.compile(r"\D+$")`). Consumed by
/// `examine_text` to scrub trailing non-digit cruft after the
/// `MAX_SEGMENT_LEN` truncation.
fn non_digits_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\D+$").unwrap())
}

// ===========================================================================
// examine_text (core.py:199-212)
// ===========================================================================

/// Prepare text and try to extract a date.
///
/// Ports `core.py:199-212`:
///
/// ```python
/// def examine_text(text: str, options: Extractor) -> Optional[str]:
///     text = trim_text(text)
///     if len(text) <= MIN_SEGMENT_LEN:
///         return None
///     text = NON_DIGITS_REGEX.sub("", text[:MAX_SEGMENT_LEN])
///     return try_date_expr(text, options.format, options.extensive,
///                          options.min, options.max)
/// ```
///
/// `MIN_SEGMENT_LEN` is imported from `extractors.py:86` (= 6 in current
/// htmldate). We pin the value locally to keep sub-stage E self-contained;
/// see the constant's doc-comment for the line citation.
pub fn examine_text(text: &str, options: &Extractor) -> Option<String> {
    let trimmed = super::utils::trim_text(text);

    // core.py:206 — `len(text) <= MIN_SEGMENT_LEN` short-circuits. Python
    // `len` on a `str` counts codepoints in Python 3; mirror via `chars()`.
    if trimmed.chars().count() <= MIN_SEGMENT_LEN {
        return None;
    }

    // core.py:209 — `text[:MAX_SEGMENT_LEN]` then strip trailing non-digits.
    let truncated: String = trimmed.chars().take(MAX_SEGMENT_LEN).collect();
    let scrubbed = non_digits_regex().replace(&truncated, "");

    // core.py:210-212 — try_date_expr.
    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);
    try_date_expr(
        Some(scrubbed.as_ref()),
        &options.format,
        options.extensive,
        &min,
        &max,
    )
}

/// `MIN_SEGMENT_LEN` — minimum candidate string length (exclusive).
///
/// Ports `htmldate/extractors.py:85` (`MIN_SEGMENT_LEN = 6`). Pinned locally
/// so `examine_text` self-cites without reaching into extractors.rs's private
/// surface. If extractors.rs exports a public `MIN_SEGMENT_LEN` constant in a
/// later sub-stage, this file should consume that.
pub const MIN_SEGMENT_LEN: usize = 6;

// ===========================================================================
// examine_date_elements (core.py:215-232)
// ===========================================================================

/// Check HTML elements one by one for date expressions.
///
/// Ports `core.py:215-232`:
///
/// ```python
/// def examine_date_elements(tree, expression, options):
///     elements = tree.xpath(expression)
///     if not elements or len(elements) > MAX_POSSIBLE_CANDIDATES:
///         return None
///     for elem in elements:
///         for text in [elem.text_content(), elem.get("title", "")]:
///             attempt = examine_text(text, options)
///             if attempt:
///                 return attempt
///     return None
/// ```
///
/// `tree.xpath(expression)` is dispatched through the Stage 0b XPath engine.
/// Sub-stage F's caller will pass FAST_PREPEND/SLOW_PREPEND + DATE_EXPRESSIONS
/// here; sub-stage E only ports the helper itself.
pub fn examine_date_elements(
    tree: &NodeRef,
    expression: &str,
    options: &Extractor,
) -> Option<String> {
    let elements = xpath_engine::evaluate(expression, tree).ok()?;
    if elements.is_empty() || elements.len() > MAX_POSSIBLE_CANDIDATES {
        return None;
    }
    for elem in &elements {
        // core.py:227 — `[elem.text_content(), elem.get("title", "")]`.
        let title = get_attribute(elem, "title").unwrap_or_default();
        for text in [text_content(elem), title] {
            if let Some(attempt) = examine_text(&text, options) {
                return Some(attempt);
            }
        }
    }
    None
}

// ===========================================================================
// examine_header (core.py:235-352)
// ===========================================================================

/// Parse header elements to find date cues.
///
/// Ports `core.py:235-352`. The 117-line beast that dispatches on every meta
/// element's `name` / `property` / `itemprop` / `pubdate` / `http-equiv`
/// attribute. Returns the first successful date string, or `None`.
///
/// Faithful early-return ladder: the dispatch chain is `name` → `property` →
/// `itemprop` → `pubdate` → `http-equiv`, with each branch being an
/// `else if` in Python (so each meta element matches at most one branch).
/// The Rust port preserves that strict elif structure.
pub fn examine_header(tree: &NodeRef, options: &Extractor) -> Option<String> {
    let mut headerdate: Option<String> = None;
    let mut reserve: Option<String> = None;

    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);

    // Local helper mirroring Python's `partial(try_date_expr, ...)` at
    // core.py:252-258. Returns Option<String>.
    let tryfunc = |s: Option<&str>| -> Option<String> {
        try_date_expr(s, &options.format, options.extensive, &min, &max)
    };

    // core.py:260 — `for elem in tree.iterfind(".//meta")`. Stage 0b engine
    // supports `.//meta`; consumed verbatim.
    let metas = get_elements_by_tag_name(tree, "meta");

    for elem in &metas {
        // core.py:262-267 — "safeguard": attrib non-empty AND ("content" in
        // attrib OR "datetime" in attrib).
        // Python's precedence: `not A and B and C` is `not (A) and (B and C)`,
        // but the actual Python source reads:
        //     not elem.attrib
        //     or "content" not in elem.attrib
        //     and "datetime" not in elem.attrib
        // Per Python operator precedence: `not in`/`in` bind tighter than
        // `and`/`or`, so this is:
        //     (not elem.attrib) OR ((content NOT IN) AND (datetime NOT IN))
        // i.e. skip when "neither content nor datetime is present" (or attrib
        // is empty — which subsumes the prior).
        let has_content = get_attribute(elem, "content").is_some();
        let has_datetime = get_attribute(elem, "datetime").is_some();
        if !has_content && !has_datetime {
            continue;
        }

        let content_val = get_attribute(elem, "content");
        let content_ref = content_val.as_deref();

        // core.py:269 — `if "name" in elem.attrib`.
        if let Some(name_raw) = get_attribute(elem, "name") {
            let attribute = name_raw.to_lowercase();
            // core.py:272-273 — og:url -> extract_url_date(content, options).
            if attribute == "og:url" {
                reserve = extract_url_date(content_ref, options);
            }
            // core.py:275-277 — DATE_ATTRIBUTES.
            else if DATE_ATTRIBUTES.contains(&attribute.as_str()) {
                headerdate = tryfunc(content_ref);
            }
            // core.py:279-284 — NAME_MODIFIED.
            else if NAME_MODIFIED.contains(&attribute.as_str()) {
                if !options.original {
                    headerdate = tryfunc(content_ref);
                } else {
                    reserve = tryfunc(content_ref);
                }
            }
        }
        // core.py:286-298 — `property` attribute branch.
        else if let Some(prop_raw) = get_attribute(elem, "property") {
            let attribute = prop_raw.to_lowercase();
            let is_date = DATE_ATTRIBUTES.contains(&attribute.as_str());
            let is_mod = PROPERTY_MODIFIED.contains(&attribute.as_str());
            if is_date || is_mod {
                let attempt = tryfunc(content_ref);
                if let Some(att) = attempt {
                    if (is_date && options.original) || (is_mod && !options.original) {
                        headerdate = Some(att);
                    } else {
                        // core.py:296-298 — "hurts precision"; stash as reserve.
                        reserve = Some(att);
                    }
                }
            }
        }
        // core.py:300-323 — `itemprop` attribute branch.
        else if let Some(itemprop_raw) = get_attribute(elem, "itemprop") {
            let attribute = itemprop_raw.to_lowercase();
            // core.py:303 — `if attribute in ITEMPROP_ATTRS`.
            if ITEMPROP_ATTRS.contains(&attribute.as_str()) {
                // core.py:305 — `elem.get("datetime") or elem.get("content")`.
                let datetime_val = get_attribute(elem, "datetime");
                let candidate_str = datetime_val.as_deref().or(content_ref);
                let attempt = tryfunc(candidate_str);
                if let Some(att) = attempt {
                    let is_orig = ITEMPROP_ATTRS_ORIGINAL.contains(&attribute.as_str());
                    let is_mod = ITEMPROP_ATTRS_MODIFIED.contains(&attribute.as_str());
                    if (is_orig && options.original) || (is_mod && !options.original) {
                        headerdate = Some(att);
                    }
                    // Python's "put on hold: hurts precision" comment at
                    // core.py:312-314 deliberately drops the else branch.
                }
            }
            // core.py:316-323 — copyrightyear.
            else if attribute == "copyrightyear"
                && let Some(content) = content_ref
            {
                let attempt = format!("{}-01-01", content);
                let di = DateInput::Str(&attempt);
                if is_valid_date(Some(&di), "%Y-%m-%d", &min, &max) {
                    reserve = Some(attempt);
                }
            }
        }
        // core.py:325-328 — `pubdate` attribute branch.
        else if let Some(pubdate_raw) = get_attribute(elem, "pubdate") {
            if pubdate_raw.to_lowercase() == "pubdate" {
                headerdate = tryfunc(content_ref);
            }
        }
        // core.py:330-343 — `http-equiv` attribute branch.
        else if let Some(httpeq_raw) = get_attribute(elem, "http-equiv") {
            let attribute = httpeq_raw.to_lowercase();
            if attribute == "date" {
                if options.original {
                    headerdate = tryfunc(content_ref);
                } else {
                    reserve = tryfunc(content_ref);
                }
            } else if attribute == "last-modified" {
                if !options.original {
                    headerdate = tryfunc(content_ref);
                } else {
                    reserve = tryfunc(content_ref);
                }
            }
        }

        // core.py:345-346 — `if headerdate is not None: break`.
        if headerdate.is_some() {
            break;
        }
    }

    // core.py:348-350 — lower-granularity fallback.
    if headerdate.is_none() && reserve.is_some() {
        headerdate = reserve;
    }
    headerdate
}

// ===========================================================================
// select_candidate (core.py:355-407)
// ===========================================================================

/// Select a candidate among the most frequent matches.
///
/// Ports `core.py:355-407`. Returns the best-matching string (Python returns
/// `re.Match`; we return the substring captured by the `catch` pattern, which
/// is what every caller actually consumes). Selection logic:
///
/// 1. Empty / over-capacity input → `None`.
/// 2. Single candidate → `catch.search(it)` on that candidate.
/// 3. Top-10 by frequency, sorted (descending unless `options.original`),
///    take top 2.
/// 4. Year-validity per `is_valid_date(year, "%Y", earliest, latest)`.
/// 5. Tie / >50%-frequent newer candidate / fallback.
///
/// Returns the matched group-0 substring as `Option<String>` — sufficient for
/// every Python call site (`bestmatch[0]` / `bestmatch[1]` consumers operate on
/// the underlying string, not the re.Match opaque type).
pub fn select_candidate(
    occurrences: &HashMap<String, usize>,
    catch: &Regex,
    yearpat: &Regex,
    options: &Extractor,
) -> Option<String> {
    // core.py:362 — empty or > MAX_POSSIBLE_CANDIDATES → None.
    if occurrences.is_empty() || occurrences.len() > MAX_POSSIBLE_CANDIDATES {
        return None;
    }

    // core.py:365-366 — single-element shortcut.
    if occurrences.len() == 1 {
        let only = occurrences.keys().next()?;
        let cap = catch.find(only)?;
        return Some(cap.as_str().to_string());
    }

    // core.py:369 — most_common(10).
    let mut firstselect: Vec<(String, usize)> = occurrences
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    // Python's Counter.most_common: sort by count DESC, then by insertion
    // order ASC (CPython dict ordering). The Rust port has no insertion
    // order on HashMap, so we sort by count DESC then by key ASC for a
    // deterministic tie-break.
    firstselect.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    firstselect.truncate(10);

    // core.py:372 — `sorted(firstselect, reverse=not options.original)[:2]`.
    // Python `sorted` on a list of (str, int) tuples compares lexicographically
    // on (str, int). `reverse=True` for !original (newest first); `False` for
    // original (oldest first).
    let mut bestones = firstselect.clone();
    bestones.sort_by(|a, b| {
        let primary = a.0.cmp(&b.0).then(a.1.cmp(&b.1));
        if options.original {
            primary
        } else {
            primary.reverse()
        }
    });
    bestones.truncate(2);

    // core.py:376 — patterns, counts = zip(*bestones).
    let patterns: Vec<&str> = bestones.iter().map(|(s, _)| s.as_str()).collect();
    let counts: Vec<usize> = bestones.iter().map(|(_, c)| *c).collect();

    // core.py:378-382 — extract year from each pattern via yearpat.
    let mut years: Vec<String> = Vec::new();
    for p in &patterns {
        if let Some(c) = yearpat.captures(p)
            && let Some(g) = c.get(1)
        {
            years.push(g.as_str().to_string());
        }
    }
    if years.len() < 2 {
        // Python zips assume both arms have 2 entries; if not, fall through
        // to the "any(validation)" branch defensively.
        if let Some(first) = years.first()
            && let Ok(y) = first.parse::<i32>()
        {
            let dt = DateTime::from_ymd((y, 1, 1));
            let di = DateInput::DateTime(dt);
            let earliest = DateTime::from_ymd(options.min);
            let latest = DateTime::from_ymd(options.max);
            if is_valid_date(Some(&di), "%Y", &earliest, &latest) {
                let pat = patterns[0];
                if let Some(m) = catch.find(pat) {
                    return Some(m.as_str().to_string());
                }
            }
        }
        return None;
    }

    // core.py:384-389 — validation per year.
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    let validation: Vec<bool> = years
        .iter()
        .map(|y| {
            let yi: i32 = y.parse().unwrap_or(0);
            let dt = DateTime::from_ymd((yi, 1, 1));
            let di = DateInput::DateTime(dt);
            is_valid_date(Some(&di), "%Y", &earliest, &latest)
        })
        .collect();

    let result_pattern: Option<&str> = if validation.iter().all(|v| *v) {
        // core.py:393-401 — both valid.
        if counts[0] == counts[1] {
            Some(patterns[0])
        } else if years[1] != years[0] && (counts[1] as f64 / counts[0] as f64) > 0.5 {
            Some(patterns[1])
        } else {
            Some(patterns[0])
        }
    } else if validation.iter().any(|v| *v) {
        // core.py:402-403 — pick the one that validated.
        let idx = validation.iter().position(|v| *v)?;
        Some(patterns[idx])
    } else {
        // core.py:404-406 — both invalid.
        None
    };

    let pat = result_pattern?;
    let m = catch.find(pat)?;
    Some(m.as_str().to_string())
}

// ===========================================================================
// search_pattern (core.py:410-425)
// ===========================================================================

/// Chained candidate filtering and selection.
///
/// Ports `core.py:410-425`. Bridges `plausible_year_filter` (sub-stage B)
/// and `select_candidate`.
pub fn search_pattern(
    htmlstring: &str,
    pattern: &Regex,
    catch: &Regex,
    yearpat: &Regex,
    options: &Extractor,
) -> Option<String> {
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    let candidates = plausible_year_filter(htmlstring, pattern, yearpat, &earliest, &latest, false);
    select_candidate(&candidates, catch, yearpat, options)
}

// ===========================================================================
// compare_reference (core.py:428-440)
// ===========================================================================

/// Compare candidate to current date reference (includes date validation and
/// older/newer test).
///
/// Ports `core.py:428-440` — the `@lru_cache(maxsize=CACHE_SIZE)` decorator is
/// deliberately NOT ported (see module docs).
pub fn compare_reference(reference: i64, expression: &str, options: &Extractor) -> i64 {
    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);
    let attempt = try_date_expr(
        Some(expression),
        &options.format,
        options.extensive,
        &min,
        &max,
    );
    match attempt {
        Some(s) => compare_values(reference, &s, options),
        None => reference,
    }
}

// ===========================================================================
// examine_abbr_elements (core.py:443-497)
// ===========================================================================

/// Scan the page for `<abbr>` elements and check if their content contains an
/// eligible date.
///
/// Ports `core.py:443-497`. Three sources of dates on an `<abbr>`:
/// - `data-utime` (Facebook timestamp integer)
/// - `class` matching `CLASS_ATTRS` plus a `title` attribute
/// - `class` matching `CLASS_ATTRS` plus text content (length > 10)
///
/// Returns the converted date string, or `None`.
pub fn examine_abbr_elements(tree: &NodeRef, options: &Extractor) -> Option<String> {
    let elements = get_elements_by_tag_name(tree, "abbr");
    if elements.is_empty() || elements.len() >= MAX_POSSIBLE_CANDIDATES {
        return None;
    }

    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);

    let mut reference: i64 = 0;
    for elem in &elements {
        // core.py:453-464 — data-utime branch.
        if let Some(utime_raw) = get_attribute(elem, "data-utime") {
            let candidate: i64 = match utime_raw.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            // core.py:460-464 — original-vs-newest split. Python's
            // `options.original and (reference == 0 or candidate < reference)`
            // (pick oldest non-zero) vs `not options.original and candidate
            // > reference` (pick newest) have identical bodies; collapse to a
            // single boolean expression so clippy stops complaining without
            // losing the faithful Python semantics.
            let take_original =
                options.original && (reference == 0 || candidate < reference);
            let take_newest = !options.original && candidate > reference;
            if take_original || take_newest {
                reference = candidate;
            }
        }
        // core.py:466-490 — class-based branches.
        else {
            let class_val = get_attribute(elem, "class");
            let class_str = class_val.as_deref().unwrap_or("");
            if CLASS_ATTRS.contains(&class_str) {
                // core.py:468-486 — title-attr branch.
                if let Some(trytext) = get_attribute(elem, "title") {
                    if options.original {
                        let attempt = try_date_expr(
                            Some(&trytext),
                            &options.format,
                            options.extensive,
                            &min,
                            &max,
                        );
                        if attempt.is_some() {
                            return attempt;
                        }
                    } else {
                        reference = compare_reference(reference, &trytext, options);
                        if reference > 0 {
                            break;
                        }
                    }
                }
                // core.py:488-490 — text-content branch.
                else {
                    // Python `elem.text` returns the lxml `.text` (leading
                    // child Text node). The Rust dom facade exposes this as
                    // `element_text`; use it directly to mirror Python.
                    let text = crate::readability::dom::element_text(elem);
                    if let Some(t) = text
                        && t.chars().count() > 10
                    {
                        reference = compare_reference(reference, &t, options);
                    }
                }
            }
        }
    }

    // core.py:492-496 — return check_extracted_reference OR
    // examine_date_elements(tree, ".//abbr", options).
    check_extracted_reference(reference, options)
        .or_else(|| examine_date_elements(tree, ".//abbr", options))
}

// ===========================================================================
// examine_time_elements (core.py:500-562)
// ===========================================================================

/// Scan the page for `<time>` elements and check if their content contains an
/// eligible date.
///
/// Ports `core.py:500-562`. Cascades through:
/// 1. `datetime` attribute (with `pubdate=pubdate` / `class=entry-{date,time}` /
///    `class=updated` shortcuts that bypass the reference accumulator).
/// 2. Bare element text content.
///
/// Returns the converted date string (or `None`) via
/// `check_extracted_reference`.
pub fn examine_time_elements(tree: &NodeRef, options: &Extractor) -> Option<String> {
    let elements = get_elements_by_tag_name(tree, "time");
    if elements.is_empty() || elements.len() >= MAX_POSSIBLE_CANDIDATES {
        return None;
    }

    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);

    let mut reference: i64 = 0;
    for elem in &elements {
        let mut shortcut_flag = false;
        let datetime_attr = get_attribute(elem, "datetime").unwrap_or_default();

        if datetime_attr.len() > 6 {
            // core.py:514-520 — `pubdate` shortcut.
            let pubdate_attr = get_attribute(elem, "pubdate");
            if pubdate_attr.is_some()
                && pubdate_attr.as_deref() == Some("pubdate")
                && options.original
            {
                shortcut_flag = true;
            }
            // core.py:522-538 — class-attribute shortcuts. The Python source
            // is an if/elif with identical bodies; we collapse to a single
            // boolean expression preserving the same observable semantics
            // (both arms set `shortcut_flag = true`).
            else if let Some(class_raw) = get_attribute(elem, "class") {
                let entry_date_arm = options.original
                    && (class_raw.starts_with("entry-date")
                        || class_raw.starts_with("entry-time"));
                let updated_arm = !options.original && class_raw == "updated";
                if entry_date_arm || updated_arm {
                    shortcut_flag = true;
                }
            }

            // core.py:543-554 — shortcut vs accumulator dispatch.
            if shortcut_flag {
                let attempt = try_date_expr(
                    Some(&datetime_attr),
                    &options.format,
                    options.extensive,
                    &min,
                    &max,
                );
                if attempt.is_some() {
                    return attempt;
                }
            } else {
                reference = compare_reference(reference, &datetime_attr, options);
            }
        }
        // core.py:556-558 — bare text content with length > 6.
        else {
            // Python uses `elem.text` (lxml's `.text` accessor); mirror.
            if let Some(t) = crate::readability::dom::element_text(elem)
                && t.chars().count() > 6
            {
                reference = compare_reference(reference, &t, options);
            }
        }
    }

    check_extracted_reference(reference, options)
}

// ===========================================================================
// normalize_match (core.py:565-571)
// ===========================================================================

/// Normalize string output by adding `"0"` if necessary, and optionally
/// expand the year from two to four digits.
///
/// Ports `core.py:565-571`:
///
/// ```python
/// def normalize_match(match: Optional[Match[str]]) -> str:
///     day, month, year = (g.zfill(2) for g in match.groups() if g)
///     if len(year) == 2:
///         year = f"19{year}" if year[0] == "9" else f"20{year}"
///     return f"{year}-{month}-{day}"
/// ```
///
/// Input is the THREE non-empty groups (in declaration order) of a
/// `THREE_COMP_REGEX_A` / `THREE_COMP_REGEX_B`-like match. Python's
/// `match.groups()` includes ALL groups; `if g` filters empties. The Rust
/// port accepts `(day, month, year)` directly — callers extract non-empty
/// groups before invoking.
pub fn normalize_match(day: &str, month: &str, year: &str) -> String {
    let day_padded = zfill2(day);
    let month_padded = zfill2(month);
    let year_padded = zfill2(year);
    let year_final = if year_padded.len() == 2 {
        if year_padded.starts_with('9') {
            format!("19{year_padded}")
        } else {
            format!("20{year_padded}")
        }
    } else {
        year_padded
    };
    format!("{year_final}-{month_padded}-{day_padded}")
}

fn zfill2(s: &str) -> String {
    if s.len() >= 2 {
        s.to_string()
    } else {
        format!("{:0>2}", s)
    }
}

// ===========================================================================
// THREE_COMP_PATTERNS (core.py:193-196)
// ===========================================================================

/// Pairs of `(pattern, catch)` regexes used by `search_page`'s three-component
/// pass.
///
/// Ports `core.py:193-196` — kept as a small public helper so sub-stage F's
/// `search_page` can iterate. Returns owned `&'static Regex` references.
pub fn three_comp_patterns() -> [(&'static Regex, &'static Regex); 2] {
    [
        (three_pattern(), three_catch()),
        (three_loose_pattern(), three_loose_catch()),
    ]
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::Dom;

    fn opts(format: &str, min: (i32, u32, u32), max: (i32, u32, u32)) -> Extractor {
        Extractor::new(false, max, min, false, format.into())
    }

    fn opts_orig(format: &str, min: (i32, u32, u32), max: (i32, u32, u32)) -> Extractor {
        Extractor::new(false, max, min, true, format.into())
    }

    // -----------------------------------------------------------------------
    // DATE_ATTRIBUTES table content
    // -----------------------------------------------------------------------

    /// Ports core.py:80-152 — DATE_ATTRIBUTES contains the headline meta
    /// names htmldate dispatches on.
    #[test]
    fn date_attributes_contains_known_keys() {
        assert!(DATE_ATTRIBUTES.contains(&"datepublished"));
        assert!(DATE_ATTRIBUTES.contains(&"article:published_time"));
        assert!(DATE_ATTRIBUTES.contains(&"og:published_time"));
        assert!(DATE_ATTRIBUTES.contains(&"date"));
        assert!(DATE_ATTRIBUTES.contains(&"timestamp"));
        // Sanity — table is non-trivial in size (>60 entries in Python).
        assert!(DATE_ATTRIBUTES.len() > 60);
    }

    /// Ports core.py:186-188 — ITEMPROP_ATTRS is the union of the two sub-tables.
    #[test]
    fn itemprop_attrs_is_union() {
        for s in ITEMPROP_ATTRS_ORIGINAL {
            assert!(ITEMPROP_ATTRS.contains(s));
        }
        for s in ITEMPROP_ATTRS_MODIFIED {
            assert!(ITEMPROP_ATTRS.contains(s));
        }
        assert_eq!(
            ITEMPROP_ATTRS.len(),
            ITEMPROP_ATTRS_ORIGINAL.len() + ITEMPROP_ATTRS_MODIFIED.len()
        );
    }

    // -----------------------------------------------------------------------
    // examine_header — five distinct meta-tag shapes
    // -----------------------------------------------------------------------

    /// Ports core.py:275-277 — `<meta name="datePublished" content="...">`.
    #[test]
    fn examine_header_meta_name_date_published() {
        let html = r#"<html><head>
            <meta name="datePublished" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:286-295 — `<meta property="article:published_time">`.
    #[test]
    fn examine_header_meta_property_published() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15T10:00:00">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:300-314 — `<meta itemprop="datePublished">`.
    #[test]
    fn examine_header_meta_itemprop_date_published() {
        let html = r#"<html><head>
            <meta itemprop="datePublished" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:330-343 — `<meta http-equiv="last-modified">` with
    /// original=false (looking for latest).
    #[test]
    fn examine_header_meta_http_equiv_last_modified() {
        let html = r#"<html><head>
            <meta http-equiv="last-modified" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        // original=false -> last-modified populates headerdate directly.
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:325-328 — `<meta pubdate="pubdate" content="...">`.
    #[test]
    fn examine_header_meta_pubdate_attr() {
        let html = r#"<html><head>
            <meta pubdate="pubdate" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:316-323 — itemprop=copyrightyear falls into `reserve`,
    /// surfaced when no headerdate is found.
    #[test]
    fn examine_header_copyrightyear_used_as_reserve() {
        let html = r#"<html><head>
            <meta itemprop="copyrightYear" content="2020">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_header(&root, &o);
        // Reserve fallback yields the synthesised "2020-01-01".
        assert_eq!(r.as_deref(), Some("2020-01-01"));
    }

    // -----------------------------------------------------------------------
    // select_candidate
    // -----------------------------------------------------------------------

    /// Ports core.py:365-366 — single-candidate shortcut.
    #[test]
    fn select_candidate_single_entry_short_circuits() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        m.insert(" 2024-06-15 ".to_string(), 1usize);
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        assert!(r.is_some(), "single-entry candidate should match");
    }

    /// Ports core.py:362 — empty Counter returns None.
    #[test]
    fn select_candidate_empty_returns_none() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let m: HashMap<String, usize> = HashMap::new();
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        assert_eq!(r, None);
    }

    /// Ports core.py:394-395 — tie-break: equal counts → take FIRST entry.
    #[test]
    fn select_candidate_tie_break_takes_first() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        m.insert(" 2020-06-15 ".to_string(), 3usize);
        m.insert(" 2024-06-15 ".to_string(), 3usize);
        // original=false ⇒ reverse=true ⇒ newer first
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        // With equal counts, take patterns[0] of the post-sort window.
        // Newer first under reverse=true → first is 2024-06-15.
        assert!(r.as_deref().unwrap().contains("2024"));
    }

    /// Ports core.py:372 — `options.original=true` flips sort direction
    /// (oldest first).
    #[test]
    fn select_candidate_original_picks_older() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        m.insert(" 2020-06-15 ".to_string(), 3usize);
        m.insert(" 2024-06-15 ".to_string(), 3usize);
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        // original=true ⇒ reverse=false ⇒ ascending; first → 2020.
        assert!(r.as_deref().unwrap().contains("2020"));
    }

    /// Ports core.py:362 — over-MAX_POSSIBLE_CANDIDATES returns None.
    #[test]
    fn select_candidate_over_max_returns_none() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        for i in 0..(MAX_POSSIBLE_CANDIDATES + 1) {
            m.insert(format!("k{i}"), 1usize);
        }
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // search_pattern
    // -----------------------------------------------------------------------

    /// Ports core.py:410-425 — chained plausible_year_filter +
    /// select_candidate over a THREE_LOOSE_PATTERN scan (the same usage
    /// the Python `search_page` consumes via the THREE_COMP_PATTERNS table).
    #[test]
    fn search_pattern_matches_iso_date_in_text() {
        use super::super::regex_catalogues::{
            three_loose_catch, three_loose_pattern, year_pattern,
        };
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // THREE_LOOSE_PATTERN expects \D-anchored YYYY[/-.]MM[/-.]DD.
        let r = search_pattern(
            "published on 2024-06-15 by an author",
            three_loose_pattern(),
            three_loose_catch(),
            year_pattern(),
            &o,
        );
        assert!(r.is_some(), "expected a match, got None");
        assert!(r.unwrap().contains("2024"));
    }

    // -----------------------------------------------------------------------
    // compare_reference
    // -----------------------------------------------------------------------

    /// Ports core.py:428-440 — successful parse returns updated reference.
    #[test]
    fn compare_reference_returns_updated_timestamp() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = compare_reference(0, "2024-06-15", &o);
        assert!(r > 0, "successful parse should return positive timestamp");
    }

    /// Ports core.py:439-440 — failed parse returns reference unchanged.
    #[test]
    fn compare_reference_returns_reference_on_failure() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = compare_reference(42, "not-a-date-string", &o);
        assert_eq!(r, 42);
    }

    // -----------------------------------------------------------------------
    // examine_abbr_elements
    // -----------------------------------------------------------------------

    /// Ports core.py:466-486 — `<abbr class="published" title="2024-06-15">`.
    #[test]
    fn examine_abbr_elements_finds_class_title() {
        let html = r#"<html><body>
            <abbr class="published" title="2024-06-15">June 15, 2024</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_abbr_elements(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // examine_time_elements
    // -----------------------------------------------------------------------

    /// Ports core.py:510-554 — `<time datetime="...">`. datetime attr is ISO.
    #[test]
    fn examine_time_elements_uses_datetime_attribute() {
        let html = r#"<html><body>
            <time datetime="2024-06-15T10:00:00">June 15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_time_elements(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:514-520 — `<time pubdate="pubdate" datetime="...">`
    /// shortcut under original=true.
    #[test]
    fn examine_time_elements_pubdate_shortcut() {
        let html = r#"<html><body>
            <time pubdate="pubdate" datetime="2024-06-15">Yesterday</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_time_elements(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:556-558 — fallback to bare text content when
    /// datetime attr is short / absent.
    #[test]
    fn examine_time_elements_uses_text_content_fallback() {
        let html = r#"<html><body>
            <time>2024-06-15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_time_elements(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // normalize_match
    // -----------------------------------------------------------------------

    /// Ports core.py:565-571 — YMD with already-4-digit year.
    #[test]
    fn normalize_match_ymd_four_digit_year() {
        assert_eq!(normalize_match("15", "06", "2024"), "2024-06-15");
    }

    /// Ports core.py:568-569 — 2-digit year starting with 9 → 19xx.
    #[test]
    fn normalize_match_two_digit_year_19xx() {
        assert_eq!(normalize_match("15", "06", "98"), "1998-06-15");
    }

    /// Ports core.py:568-570 — 2-digit year not starting with 9 → 20xx.
    #[test]
    fn normalize_match_two_digit_year_20xx() {
        assert_eq!(normalize_match("15", "06", "24"), "2024-06-15");
    }

    /// Ports core.py:568 — single-digit day/month gets zfilled.
    #[test]
    fn normalize_match_zfills_single_digit_components() {
        assert_eq!(normalize_match("5", "6", "2024"), "2024-06-05");
    }

    // -----------------------------------------------------------------------
    // examine_text
    // -----------------------------------------------------------------------

    /// Ports core.py:206 — `len(text) <= MIN_SEGMENT_LEN` short-circuits.
    #[test]
    fn examine_text_rejects_too_short() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // 6 chars or less hits the floor.
        assert_eq!(examine_text("abcde", &o), None);
        assert_eq!(examine_text("abcdef", &o), None);
    }

    /// Ports core.py:209-212 — happy path on a long-enough string.
    #[test]
    fn examine_text_extracts_iso_date() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_text("on 2024-06-15 today", &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }
}
