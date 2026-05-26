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
//! - `search_page` (sub-stage F)                                    (core.py:574-805)
//!
//! `find_date` (core.py:808-983) remains deferred to a later sub-stage.
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
    MAX_SEGMENT_LEN, extract_url_date, idiosyncrasies_search, img_search, json_search,
    pattern_search, regex_parse, try_date_expr,
};
use super::regex_catalogues::{
    DATE_EXPRESSIONS, FAST_PREPEND, SLOW_PREPEND, copyright_pattern, datestrings_catch,
    datestrings_pattern, mmyyyy_pattern, mmyyyy_year, select_ymd_pattern, select_ymd_year,
    simple_pattern, simple_pattern_post_filter, slashes_pattern, slashes_year, three_catch,
    three_comp_regex_a, three_comp_regex_b, three_loose_catch, three_loose_pattern, three_pattern,
    timestamp_pattern, two_comp_regex, year_pattern, ymd_pattern, ymd_year, yyyymm_catch,
    yyyymm_pattern,
};
use super::settings::{CLEANING_LIST, MAX_POSSIBLE_CANDIDATES};
use super::utils::{Extractor, clean_html};
use super::validators::{
    DateInput, DateTime, check_extracted_reference, compare_values, filter_ymd_candidate,
    is_valid_date, is_valid_format, plausible_year_filter, validate_and_convert,
};

use crate::htmldate::extractors::discard_unwanted;
use crate::readability::dom::{
    NodeData, NodeRef, child_nodes, deep_clone, get_attribute, get_elements_by_tag_name,
    serialize_html, text_content,
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
// search_page (core.py:574-805) — sub-stage F
// ===========================================================================

/// Re-capture (year, month, day) from a `select_candidate` substring using
/// the supplied catch regex.
///
/// Python's `select_candidate` (core.py:355-407) returns a `re.Match` whose
/// groups `[1]`/`[2]`/`[3]` `filter_ymd_candidate` reads directly. Our Rust
/// `select_candidate` returns the matched substring instead (see core.rs's
/// sub-stage E module header divergence note). To bridge to the Rust
/// `filter_ymd_candidate(Option<(&str, &str, &str)>, ...)` contract, we
/// re-run the catch regex on the substring and pull groups 1/2/3 in YMD
/// order. Returns `None` if the recapture fails (regex anchored mismatch),
/// matching the Python `bestmatch is None` short-circuit at
/// `validators.py:144`.
fn recapture_ymd_groups(s: &str, catch: &Regex) -> Option<(String, String, String)> {
    let caps = catch.captures(s)?;
    let y = caps.get(1)?.as_str().to_string();
    let m = caps.get(2)?.as_str().to_string();
    let d = caps.get(3)?.as_str().to_string();
    Some((y, m, d))
}

/// Opportunistically search the HTML text for common date text patterns.
///
/// Ports `htmldate/core.py:574-805` — the final regex cascade fallback
/// inside `find_date`. Runs the arms in **Python source order** (verbatim,
/// per the M4 Stage 1 sub-stage F anti-inversion contract):
///
/// 1. **COPYRIGHT_PATTERN** (`core.py:589-605`) — sets `copyear`, does not
///    return.
/// 2. **THREE_COMP_PATTERNS** loop (`core.py:607-629`) — URL `/YYYY/MM/DD`
///    + loose-separator `YYYY[/.-]MM[/.-]DD`.
/// 3. **SELECT_YMD_PATTERN** (`core.py:631-658`) — `D?D[/.-]M?M[/.-]YYYY`
///    normalised through `THREE_COMP_REGEX_A` + `normalize_match`.
/// 4. **DATESTRINGS_PATTERN** (`core.py:660-678`) — compact `YYYYMMDD`.
/// 5. **SLASHES_PATTERN** (`core.py:680-707`) — `D?D/M?M/YY` normalised
///    through `THREE_COMP_REGEX_B` + `normalize_match` (incomplete=true).
/// 6. **YYYYMM_PATTERN** (`core.py:709-732`) — `YYYY[/.-]MM` two-component.
/// 7. **MMYYYY_PATTERN** (`core.py:734-765`) — `M?M[/.-]YYYY` two-component
///    normalised through `TWO_COMP_REGEX` (incomplete=options.original).
/// 8. **regex_parse** (`core.py:767-775`) — multilingual `LONG_TEXT_PATTERN`.
/// 9. **Copyright catchall** (`core.py:777-781`) — if `copyear != 0`,
///    return `copyear-01-01`.
/// 10. **SIMPLE_PATTERN** (`core.py:783-803`) — year-only last resort
///     with the `(?<!w3.org)` lookbehind reproduced via
///     `simple_pattern_post_filter` (the Rust `regex` crate has no
///     lookarounds — see `regex_catalogues.rs` module header).
///
/// Returns the first arm to produce a valid date, formatted per
/// `options.format`. Returns `None` only if every arm misses.
pub fn search_page(htmlstring: &str, options: &Extractor) -> Option<String> {
    let min = DateTime::from_ymd(options.min);
    let max = DateTime::from_ymd(options.max);

    // -----------------------------------------------------------------------
    // 1. core.py:589-605 — copyright sets `copyear`.
    // -----------------------------------------------------------------------
    let mut copyear: i32 = 0;
    let bestmatch = search_pattern(
        htmlstring,
        copyright_pattern(),
        year_pattern(),
        year_pattern(),
        options,
    );
    if let Some(s) = bestmatch
        && let Ok(year) = s.parse::<i32>()
    {
        // core.py:601-605 — is_valid_date(datetime(year, 1, 1), "%Y", ...).
        let dt = DateTime::from_ymd((year, 1, 1));
        let di = DateInput::DateTime(dt);
        if is_valid_date(Some(&di), "%Y", &min, &max) {
            copyear = year;
        }
    }

    // -----------------------------------------------------------------------
    // 2. core.py:607-629 — THREE_COMP_PATTERNS loop.
    // -----------------------------------------------------------------------
    for (pattern, catch) in three_comp_patterns() {
        let bestmatch = search_pattern(htmlstring, pattern, catch, year_pattern(), options);
        // core.py:619-627 — filter_ymd_candidate. Recapture groups via catch.
        let groups = bestmatch.as_deref().and_then(|s| recapture_ymd_groups(s, catch));
        let result = filter_ymd_candidate(
            groups.as_ref().map(|(y, m, d)| (y.as_str(), m.as_str(), d.as_str())),
            "", // pattern name only used for logging in Python.
            options.original,
            copyear,
            &options.format,
            &min,
            &max,
        );
        if result.is_some() {
            return result;
        }
    }

    // -----------------------------------------------------------------------
    // 3. core.py:631-658 — SELECT_YMD_PATTERN with THREE_COMP_REGEX_A
    // normalisation.
    // -----------------------------------------------------------------------
    let candidates = plausible_year_filter(
        htmlstring,
        select_ymd_pattern(),
        select_ymd_year(),
        &min,
        &max,
        false,
    );
    // core.py:639-645 — replace each candidate with normalize_match output.
    let mut replacement: HashMap<String, usize> = HashMap::new();
    for (item, count) in &candidates {
        if let Some(caps) = three_comp_regex_a().captures(item) {
            // THREE_COMP_REGEX_A groups: (day, month, year) per
            // extractors.py:183. Python `match.groups()` includes ALL groups;
            // `if g` filters empties. The regex has exactly 3 groups so we
            // pull 1/2/3.
            let day = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let month = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let year = caps.get(3).map(|m| m.as_str()).unwrap_or("");
            if !day.is_empty() && !month.is_empty() && !year.is_empty() {
                let key = normalize_match(day, month, year);
                replacement.insert(key, *count);
            }
        }
    }
    let bestmatch = select_candidate(&replacement, ymd_pattern(), ymd_year(), options);
    let groups = bestmatch
        .as_deref()
        .and_then(|s| recapture_ymd_groups(s, ymd_pattern()));
    let result = filter_ymd_candidate(
        groups.as_ref().map(|(y, m, d)| (y.as_str(), m.as_str(), d.as_str())),
        "",
        options.original,
        copyear,
        &options.format,
        &min,
        &max,
    );
    if result.is_some() {
        return result;
    }

    // -----------------------------------------------------------------------
    // 4. core.py:660-678 — DATESTRINGS_PATTERN.
    // -----------------------------------------------------------------------
    let bestmatch = search_pattern(
        htmlstring,
        datestrings_pattern(),
        datestrings_catch(),
        year_pattern(),
        options,
    );
    let groups = bestmatch
        .as_deref()
        .and_then(|s| recapture_ymd_groups(s, datestrings_catch()));
    let result = filter_ymd_candidate(
        groups.as_ref().map(|(y, m, d)| (y.as_str(), m.as_str(), d.as_str())),
        "",
        options.original,
        copyear,
        &options.format,
        &min,
        &max,
    );
    if result.is_some() {
        return result;
    }

    // -----------------------------------------------------------------------
    // 5. core.py:680-707 — SLASHES_PATTERN with THREE_COMP_REGEX_B
    // normalisation (incomplete=true).
    // -----------------------------------------------------------------------
    let candidates = plausible_year_filter(
        htmlstring,
        slashes_pattern(),
        slashes_year(),
        &min,
        &max,
        true,
    );
    let mut replacement: HashMap<String, usize> = HashMap::new();
    for (item, count) in &candidates {
        // THREE_COMP_REGEX_B has 6 groups: two alternatives sharing
        // (day, month, 2-digit-year). Python's `match.groups()` returns ALL
        // groups; `if g` filters None entries. The Rust port iterates
        // captures and takes the first three non-empty groups in declaration
        // order, mirroring Python's filter-by-truthiness.
        if let Some(caps) = three_comp_regex_b().captures(item) {
            let mut parts: Vec<&str> = Vec::new();
            for i in 1..caps.len() {
                if let Some(m) = caps.get(i)
                    && !m.as_str().is_empty()
                {
                    parts.push(m.as_str());
                    if parts.len() == 3 {
                        break;
                    }
                }
            }
            if parts.len() == 3 {
                let key = normalize_match(parts[0], parts[1], parts[2]);
                replacement.insert(key, *count);
            }
        }
    }
    let bestmatch = select_candidate(&replacement, ymd_pattern(), ymd_year(), options);
    let groups = bestmatch
        .as_deref()
        .and_then(|s| recapture_ymd_groups(s, ymd_pattern()));
    let result = filter_ymd_candidate(
        groups.as_ref().map(|(y, m, d)| (y.as_str(), m.as_str(), d.as_str())),
        "",
        options.original,
        copyear,
        &options.format,
        &min,
        &max,
    );
    if result.is_some() {
        return result;
    }

    // -----------------------------------------------------------------------
    // 6. core.py:709-732 — YYYYMM_PATTERN (two-component, first option).
    // -----------------------------------------------------------------------
    let bestmatch = search_pattern(
        htmlstring,
        yyyymm_pattern(),
        yyyymm_catch(),
        year_pattern(),
        options,
    );
    if let Some(s) = bestmatch.as_deref()
        && let Some(caps) = yyyymm_catch().captures(s)
    {
        // YYYYMM_CATCH groups: (year, month). Python `bestmatch[1]`/`[2]`.
        let year: i32 = caps.get(1).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        let month: u32 = caps.get(2).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
        if let Some(dt) = make_date(year, month, 1)
            && (copyear == 0 || dt.year >= copyear)
        {
            let di = DateInput::DateTime(dt);
            if let Some(r) = validate_and_convert(Some(&di), &options.format, &min, &max) {
                return Some(r);
            }
        }
    }

    // -----------------------------------------------------------------------
    // 7. core.py:734-765 — MMYYYY_PATTERN (two-component, second option,
    // incomplete=options.original).
    // -----------------------------------------------------------------------
    let candidates = plausible_year_filter(
        htmlstring,
        mmyyyy_pattern(),
        mmyyyy_year(),
        &min,
        &max,
        options.original,
    );
    let mut replacement: HashMap<String, usize> = HashMap::new();
    for (item, count) in &candidates {
        // TWO_COMP_REGEX: (month, year). Python builds `"-".join([year,
        // zfilled_month, "01"])` per core.py:746-751.
        if let Some(caps) = two_comp_regex().captures(item)
            && let (Some(month_m), Some(year_m)) = (caps.get(1), caps.get(2))
        {
            let month_raw = month_m.as_str();
            let year_s = year_m.as_str();
            let month_padded = if month_raw.len() == 1 {
                format!("0{}", month_raw)
            } else {
                month_raw.to_string()
            };
            let key = format!("{}-{}-01", year_s, month_padded);
            replacement.insert(key, *count);
        }
    }
    let bestmatch = select_candidate(&replacement, ymd_pattern(), ymd_year(), options);
    let groups = bestmatch
        .as_deref()
        .and_then(|s| recapture_ymd_groups(s, ymd_pattern()));
    let result = filter_ymd_candidate(
        groups.as_ref().map(|(y, m, d)| (y.as_str(), m.as_str(), d.as_str())),
        "",
        options.original,
        copyear,
        &options.format,
        &min,
        &max,
    );
    if result.is_some() {
        return result;
    }

    // -----------------------------------------------------------------------
    // 8. core.py:767-775 — full-blown text regex (regex_parse / LONG_TEXT_PATTERN).
    // -----------------------------------------------------------------------
    let dateobject = regex_parse(htmlstring);
    if (copyear == 0 || dateobject.map(|d| d.year >= copyear).unwrap_or(false))
        && let Some(dt) = dateobject
    {
        let di = DateInput::DateTime(dt);
        if let Some(r) = validate_and_convert(Some(&di), &options.format, &min, &max) {
            return Some(r);
        }
    }

    // -----------------------------------------------------------------------
    // 9. core.py:777-781 — copyright catchall.
    // -----------------------------------------------------------------------
    if copyear != 0 {
        let dt = DateTime::from_ymd((copyear, 1, 1));
        // Python: `dateobject.strftime(options.format)`. Use validate_and_convert
        // for format emission — copyear was already validated by is_valid_date.
        let di = DateInput::DateTime(dt);
        if let Some(r) = validate_and_convert(Some(&di), &options.format, &min, &max) {
            return Some(r);
        }
    }

    // -----------------------------------------------------------------------
    // 10. core.py:783-803 — SIMPLE_PATTERN (one component, last resort).
    // The (?<!w3.org) Python lookbehind is reproduced via
    // simple_pattern_post_filter, per regex_catalogues.rs module header.
    // -----------------------------------------------------------------------
    // Manually iterate matches so we can apply the post-filter before
    // feeding plausible_year_filter / select_candidate.
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    for caps in simple_pattern().captures_iter(htmlstring) {
        if let Some(m0) = caps.get(0) {
            // The lookbehind in Python sits BEFORE the `\D` byte at
            // simple_pattern's start: `(?<!w3.org)\D({YEAR_RE})\D`. The
            // post-filter checks the 6 bytes preceding the WHOLE match
            // (m0.start()), which is exactly what Python's lookbehind sees.
            if !simple_pattern_post_filter(htmlstring, m0.start()) {
                continue;
            }
        }
        if let Some(g1) = caps.get(1) {
            *occurrences.entry(g1.as_str().to_string()).or_insert(0) += 1;
        }
    }
    // Apply plausible_year_filter's year-range filter manually since we
    // already populated occurrences with the post-filtered results. The
    // year_pattern() yearpat is anchored at the start so it works on bare
    // 4-digit candidates.
    let keys: Vec<String> = occurrences.keys().cloned().collect();
    for k in keys {
        if let Some(caps) = year_pattern().captures(&k)
            && let Some(g) = caps.get(1)
            && let Ok(y) = g.as_str().parse::<i32>()
        {
            if !(min.year <= y && y <= max.year) {
                occurrences.remove(&k);
            }
        } else {
            occurrences.remove(&k);
        }
    }
    if let Some(bestmatch) = select_candidate(&occurrences, year_pattern(), year_pattern(), options)
        && let Some(caps) = year_pattern().captures(&bestmatch)
        && let Some(year_m) = caps.get(1)
        && let Ok(year) = year_m.as_str().parse::<i32>()
        && let Some(dt) = make_date(year, 1, 1)
        && year >= copyear
    {
        // core.py:794-797 — is_valid_date(dateobject, "%Y-%m-%d", ...).
        let synth = format!("{:04}-01-01", year);
        let di = DateInput::Str(&synth);
        if is_valid_date(Some(&di), "%Y-%m-%d", &min, &max) {
            let di2 = DateInput::DateTime(dt);
            if let Some(r) = validate_and_convert(Some(&di2), &options.format, &min, &max) {
                return Some(r);
            }
        }
    }

    None
}

/// Tiny helper: construct a `DateTime` for the (Y, M, D) tuple, returning
/// `None` if the calendar values are invalid. Mirrors Python's
/// `datetime(year, month, day)` raising `ValueError`.
fn make_date(year: i32, month: u32, day: u32) -> Option<DateTime> {
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(DateTime::from_ymd((year, month, day)))
}

// ===========================================================================
// find_date (core.py:808-983) — sub-stage G entrypoint
// ===========================================================================

/// Extract dates from HTML documents using markup analysis and text patterns.
///
/// Ports `htmldate/core.py:808-983`. Wires every sub-stage A..F port into the
/// full htmldate algorithm. Returns a date string formatted per
/// `options.format`, or `None`.
///
/// # Signature divergence from Python (faithful)
///
/// Python accepts `htmlobject: Union[bytes, str, HtmlElement]` and runs
/// `load_html` (encoding sniff + URL fetch + lxml parse). The Rust port
/// takes a **pre-parsed `&NodeRef`** instead — every caller in the
/// Trafilatura pipeline (specifically `metadata_url::extract_date` and
/// `metadata::extract_metadata`) already holds a parsed `Dom`, so the
/// `load_html` branch is dead weight. `load_html` remains deferred per the
/// sub-stage A module header.
///
/// Python's `extensive_search` / `original_date` / `outputformat` /
/// `url` / `min_date` / `max_date` parameters fold into the existing
/// [`Extractor`] options struct sub-stages A-F already consume. The
/// remaining `verbose` (a logging knob) is dropped — Rust callers can
/// configure log levels separately. `deferred_url_extractor` defaults to
/// `false` in Python and is currently never set true by any in-tree
/// caller, so the Rust port pins it `false` for sub-stage G simplicity;
/// the wiring can grow there if a future use case appears.
///
/// # Algorithm (cited line-by-line vs core.py:861-983)
///
/// 1. **core.py:866-867** — outputformat validity gate. Reject any
///    non-default format that `is_valid_format` rejects.
/// 2. **core.py:881-891** — URL handling. If `url` is `None`, probe
///    `.//link[@rel="canonical"]` for an `href`. Then call
///    `extract_url_date(url, options)`. If hit and not deferred, return.
/// 3. **core.py:895** — `examine_header(tree, options) or
///    json_search(tree, options)`.
/// 4. **core.py:900-901** — deferred URL fallback (no-op when
///    deferred=false).
/// 5. **core.py:904-909** — `examine_abbr_elements(tree, options)`.
/// 6. **core.py:912-919** — prune tree: `discard_unwanted(clean_html(
///    deepcopy(tree), CLEANING_LIST))`. The `try / except ValueError`
///    Python branch (a defensive lxml-NULL-byte rescue) has no Rust
///    counterpart — `clean_html` / `discard_unwanted` cannot raise.
/// 7. **core.py:922-925** — `date_expr = (SLOW_PREPEND if extensive
///    else FAST_PREPEND) + DATE_EXPRESSIONS`.
/// 8. **core.py:929-941** — `examine_date_elements(search_tree, date_expr,
///    options) or examine_date_elements(search_tree, ".//title|.//h1",
///    options) or examine_time_elements(search_tree, options)`.
/// 9. **core.py:953-956** — serialize search_tree to a string for
///    string-pattern arms.
/// 10. **core.py:961-965** — `pattern_search(htmlstring, TIMESTAMP_PATTERN,
///     options) or img_search(search_tree, options) or
///     idiosyncrasies_search(htmlstring, options)`.
/// 11. **core.py:970-981** — extensive last resort: iterate
///     `FREE_TEXT_EXPRESSIONS` (FAST_PREPEND + `/text()`), accumulate
///     `compare_reference` over each segment, then
///     `check_extracted_reference(reference, options) or
///     search_page(htmlstring, options)`.
///
/// Returns the formatted date string on success, or `None`.
pub fn find_date(tree: &NodeRef, options: &Extractor) -> Option<String> {
    // core.py:866-867 — outputformat validity gate.
    if options.format != "%Y-%m-%d" && !is_valid_format(&options.format) {
        return None;
    }

    // The Python signature exposes `url` + `deferred_url_extractor` as
    // function-level arguments. The Rust port currently calls find_date
    // without external URL context (the Trafilatura caller already routes
    // URL handling through `metadata_url::extract_url`), and the
    // deferred_url_extractor path is dead code in every in-tree caller.
    let deferred_url_extractor = false;

    // core.py:881-891 — URL probe. If we don't have a url, try the
    // canonical link; then call extract_url_date.
    let canonical_url: Option<String> = find_canonical_url(tree);
    let url_result = extract_url_date(canonical_url.as_deref(), options);
    // llvm-cov:branch-not-reachable: the `!deferred_url_extractor` FALSE side
    // is dead — `deferred_url_extractor` is pinned `false` above, so the `&&`
    // second operand always evaluates to `true` (we never skip a hit URL).
    if url_result.is_some() && !deferred_url_extractor {
        return url_result;
    }

    // core.py:895 — examine_header then json_search.
    let result = examine_header(tree, options).or_else(|| json_search(tree, options));
    if result.is_some() {
        return result;
    }

    // llvm-cov:branch-not-reachable: `deferred_url_extractor` is pinned
    // `false` (see L1398), so the entire deferred-URL fallback block is
    // dead in every in-tree caller. Preserved verbatim from core.py:900-901
    // for forward-compatibility if a deferred caller ever appears.
    if deferred_url_extractor && url_result.is_some() {
        return url_result;
    }

    // core.py:904-909 — abbr elements.
    let abbr_result = examine_abbr_elements(tree, options);
    if abbr_result.is_some() {
        return abbr_result;
    }

    // core.py:912-919 — prune tree: deepcopy + clean_html + discard_unwanted.
    // Python wraps in `try / except ValueError` (lxml NULL-byte rescue);
    // neither Rust port can raise, so no try/except equivalent needed.
    let search_tree = deep_clone(tree);
    clean_html(&search_tree, CLEANING_LIST);
    let _discarded = discard_unwanted(&search_tree);

    // core.py:922-925 — choose prepend by extensive flag.
    let prepend = if options.extensive {
        SLOW_PREPEND
    } else {
        FAST_PREPEND
    };
    let date_expr = format!("{}{}", prepend, DATE_EXPRESSIONS);

    // core.py:929-941 — date_elements → title/h1 → time elements.
    let result = examine_date_elements(&search_tree, &date_expr, options)
        .or_else(|| examine_date_elements(&search_tree, ".//title|.//h1", options))
        .or_else(|| examine_time_elements(&search_tree, options));
    if result.is_some() {
        return result;
    }

    // core.py:953-956 — robust conversion to string. Rust's serialize_html
    // is UTF-8 throughout; no UnicodeDecodeError rescue branch needed.
    let htmlstring = serialize_html(&search_tree);

    // core.py:961-965 — timestamp pattern_search, img_search, idiosyncrasies.
    let result = pattern_search(&htmlstring, timestamp_pattern(), options)
        .or_else(|| img_search(&search_tree, options))
        .or_else(|| idiosyncrasies_search(&htmlstring, options));
    if result.is_some() {
        return result;
    }

    // core.py:970-981 — extensive_search last resort.
    if options.extensive {
        let mut reference: i64 = 0;
        for segment in free_text_segments(&search_tree) {
            let stripped = segment.trim();
            // core.py:976 — `if not MIN_SEGMENT_LEN < len(segment) <
            // MAX_SEGMENT_LEN: continue`. Python's `<` is strict on both
            // ends. Python `len(str)` counts codepoints; mirror via chars().
            let n = stripped.chars().count();
            if !(MIN_SEGMENT_LEN < n && n < MAX_SEGMENT_LEN) {
                continue;
            }
            reference = compare_reference(reference, stripped, options);
        }
        let converted = check_extracted_reference(reference, options);
        // core.py:981 — `return converted or search_page(htmlstring, options)`.
        return converted.or_else(|| search_page(&htmlstring, options));
    }

    None
}

/// Probe the tree for a `<link rel="canonical" href="...">` and return its
/// `href` attribute.
///
/// Ports `core.py:884-886`:
///
/// ```python
/// urlelem = tree.find('.//link[@rel="canonical"]')
/// if urlelem is not None:
///     url = urlelem.get("href")
/// ```
///
/// Returns the href value (which may be relative — `extract_url_date`'s
/// regex is permissive enough to handle both).
fn find_canonical_url(tree: &NodeRef) -> Option<String> {
    for link in get_elements_by_tag_name(tree, "link") {
        if let Some(rel) = get_attribute(&link, "rel")
            && rel.eq_ignore_ascii_case("canonical")
            && let Some(href) = get_attribute(&link, "href")
        {
            return Some(href);
        }
    }
    None
}

/// Yield each direct-text-child string of every element matching
/// `FAST_PREPEND`'s self-tag filter, in document order.
///
/// Ports the lxml `XPath(FAST_PREPEND + "/text()")` consumed by
/// `core.py:974` (`for segment in FREE_TEXT_EXPRESSIONS(search_tree)`).
/// Python yields each direct text child of every matching element as a
/// separate string (lxml `_ElementUnicodeResult`). The Rust port does the
/// same: collect text-typed `child_nodes` of each FAST_PREPEND-matched
/// element and return their string data.
///
/// We resolve `FAST_PREPEND` via the Stage 0b XPath engine and then walk
/// each match's direct children for `NodeData::Text` siblings — mirroring
/// the `/text()` step semantically without extending the engine to yield
/// text nodes directly (which would touch a hot conformance harness).
fn free_text_segments(tree: &NodeRef) -> Vec<String> {
    let elements = xpath_engine::evaluate(FAST_PREPEND, tree).unwrap_or_default();
    let mut out = Vec::new();
    for elem in &elements {
        for child in child_nodes(elem) {
            if let NodeData::Text { contents } = &child.data {
                let data = contents.borrow().to_string();
                out.push(data);
            }
        }
    }
    out
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
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

    /// rationale: pin `select_candidate`'s "patterns[1] wins on >50%
    /// ratio" arm (core.rs:633 — `years[1] != years[0] &&
    /// counts[1]/counts[0] > 0.5` TRUE → bestmatch[1]). The second
    /// candidate's count is at least half of the first's, so it wins
    /// (core.py:395-398). With original=false (reverse sort, newer is
    /// patterns[0]), this lets a slightly-more-popular OLDER year beat
    /// the newer one.
    #[test]
    fn select_candidate_patterns_one_wins_on_majority_ratio() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        // original=false → reverse sort puts " 2024-06-15 " first.
        // counts = [3 (newer), 4 (older)] → ratio 4/3 > 0.5 → patterns[1]
        // (the older " 2020-06-15 ") wins.
        m.insert(" 2020-06-15 ".to_string(), 4usize);
        m.insert(" 2024-06-15 ".to_string(), 3usize);
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        // The older year wins because count[1]/count[0] = 4/3 > 0.5.
        assert!(r.as_deref().unwrap().contains("2020"));
    }

    /// rationale: pin `select_candidate`'s "years.len() < 2" recovery
    /// arm (core.rs:596 TRUE → fall through to single-year validation at
    /// L599-L612) — when yearpat captures fewer than 2 years among the
    /// top-2 patterns, the function attempts to salvage the first
    /// captured year (faithful divergence from Python's `zip(*bestones)`
    /// which would panic on a length mismatch).
    #[test]
    fn select_candidate_falls_back_when_only_one_year_captured() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        // One entry has a 4-digit year; the other doesn't. yearpat only
        // captures from the first → years.len() == 1 < 2 → recovery arm.
        // original=true → ascending sort → " 2024..." (lex smaller) sits at
        // patterns[0], so the recovery's `catch.find(patterns[0])` succeeds.
        m.insert(" 2024-06-15 ".to_string(), 2usize);
        m.insert(" no-year-here ".to_string(), 1usize);
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        // The recovery arm catches the lone year and returns the YMD match.
        assert!(r.as_deref().unwrap().contains("2024"));
    }

    /// rationale: pin `select_candidate`'s "patterns[0] wins when ratio
    /// <= 0.5" arm (core.rs:633 TRUE+ `>0.5` FALSE → bestmatch[0]).
    /// With original=true (ascending sort, older is patterns[0]), the
    /// older year wins when the newer's count is <= half (core.py:395-400).
    #[test]
    fn select_candidate_patterns_zero_wins_when_minority_ratio() {
        use super::super::regex_catalogues::{year_pattern, ymd_pattern};
        let mut m = HashMap::new();
        // original=true → ascending sort → " 2020-06-15 " is patterns[0].
        // counts = [10 (older), 1 (newer)] → ratio 1/10 = 0.1 <= 0.5 →
        // falls through to else → patterns[0] (older) wins.
        m.insert(" 2020-06-15 ".to_string(), 10usize);
        m.insert(" 2024-06-15 ".to_string(), 1usize);
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = select_candidate(&m, ymd_pattern(), year_pattern(), &o);
        assert!(r.as_deref().unwrap().contains("2020"));
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

    // -----------------------------------------------------------------------
    // search_page (sub-stage F — core.py:574-805)
    // -----------------------------------------------------------------------

    /// Ports core.py:607-629 — THREE_COMP_PATTERNS arm A (THREE_PATTERN,
    /// URL-style `/YYYY/MM/DD/` fragment).
    #[test]
    fn search_page_three_pattern_url_form_match() {
        let html = "<html><body><a href=\"/blog/2024/03/15/article\">Read more</a></body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:607-629 — THREE_COMP_PATTERNS arm B (THREE_LOOSE_PATTERN,
    /// loose-separator `YYYY-MM-DD` substring).
    #[test]
    fn search_page_three_loose_pattern_match() {
        let html = "<html><body><p>Published 2024.03.15 by an author</p></body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:631-658 — SELECT_YMD_PATTERN arm. Input is
    /// `DD/MM/YYYY` which select_ymd_pattern catches, then normalised
    /// to YYYY-MM-DD via THREE_COMP_REGEX_A.
    #[test]
    fn search_page_select_ymd_pattern_match() {
        // Use a form that doesn't trip the THREE_LOOSE_PATTERN arm first.
        // The SLASHES arm has 2-digit years; SELECT_YMD has 4-digit years.
        // DD/MM/YYYY with separators not matching THREE_LOOSE (which is
        // YYYY[/.-]MM[/.-]DD only) — confirmed by surrounding non-digits.
        let html = "<html><body>Date posted: 15/03/2024 today!</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:660-678 — DATESTRINGS_PATTERN arm: compact `YYYYMMDD`.
    #[test]
    fn search_page_datestrings_pattern_match() {
        let html = "<html><body>archive id 20240315 reference</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:680-707 — SLASHES_PATTERN arm: `DD/MM/YY` two-digit-year
    /// rescue normalised via THREE_COMP_REGEX_B + century guesser.
    #[test]
    fn search_page_slashes_pattern_match() {
        // 2-digit year 24 (not 9-prefixed) -> 20xx century per
        // plausible_year_filter (incomplete=true).
        let html = "<html><body>posted on 15/03/24 evening</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:709-732 — YYYYMM_PATTERN arm (defaults day=1).
    #[test]
    fn search_page_yyyymm_pattern_match() {
        // YYYYMM only — no full YMD anywhere. The arms above ALL miss
        // (THREE_COMP wants D component; SELECT_YMD wants D; DATESTRINGS
        // wants 8 digits; SLASHES wants 2-digit year), so flow reaches
        // YYYYMM.
        let html = "<html><body>archive bucket 2024/03 entries</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-03-01"));
    }

    /// Ports core.py:734-765 — MMYYYY_PATTERN arm. Pattern requires no
    /// YYYYMM elsewhere, so use `03/2024` form (matches MMYYYY but not
    /// YYYYMM since the 4-digit segment isn't the first).
    #[test]
    fn search_page_mmyyyy_pattern_match() {
        let html = "<html><body>edition 03/2024 catalogue</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        // MMYYYY defaults day=1; output ISO is 2024-03-01.
        assert_eq!(r.as_deref(), Some("2024-03-01"));
    }

    /// Ports core.py:783-803 — SIMPLE_PATTERN year-only last resort.
    #[test]
    fn search_page_simple_pattern_year_only_match() {
        // Bare year, no other dates anywhere; copyright catchall must not
        // fire (no © / Copyright symbols) so SIMPLE arm gets reached.
        let html = "<html><body>archive year 2024 catalog</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-01-01"));
    }

    /// Ports core.py:783-803 + regex_catalogues.rs `simple_pattern_post_filter`
    /// — Python's `(?<!w3.org)` lookbehind: a year preceded by `w3.org` MUST
    /// be rejected. With ONLY a w3.org-prefixed year and no copyright, the
    /// SIMPLE arm must reject, and search_page must return None.
    #[test]
    fn search_page_simple_pattern_rejects_w3_org_prefix() {
        // The w3.org token blocks SIMPLE_PATTERN's match; no other arm
        // can extract a date.
        let html = "<html><body>see w3.org 2024 specification</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        // The w3.org year is filtered out; SIMPLE arm leaves no candidates.
        // (Copyright catchall is gated on copyear != 0, which it isn't.)
        assert_eq!(r, None);
    }

    /// Ports core.py:589-605 + core.py:777-781 — COPYRIGHT_PATTERN extracts
    /// `copyear`; downstream arms all miss (only the © year is on the page),
    /// then the copyright catchall returns `copyear-01-01`.
    #[test]
    fn search_page_copyright_catchall_match() {
        let html = "<html><body><footer>© 2024 Acme Inc.</footer></body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("2024-01-01"));
    }

    /// Ports core.py:805 — no date anywhere ⇒ None.
    #[test]
    fn search_page_no_date_returns_none() {
        let html = "<html><body>Just some text without any date markers</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r, None);
    }

    /// Ports core.py:601-602 + validators.py:51-54 — min_date filter rejects
    /// pages whose only date is below the configured floor.
    #[test]
    fn search_page_min_date_filter_rejects_old_year() {
        // 2024 page but min_date = 2030 ⇒ every arm's is_valid_date rejects.
        // No © so the catchall doesn't engage either.
        let html = "<html><body>published 2024-03-15 today</body></html>";
        let o = opts("%Y-%m-%d", (2030, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r, None);
    }

    /// Ports core.py:733 + validate_and_convert outputformat application —
    /// custom outputformat (`%d.%m.%Y`) honoured on the YYYYMM rescue arm
    /// (the only arm that goes through validate_and_convert with a
    /// constructed DateTime — exercises format_emit path).
    #[test]
    fn search_page_respects_custom_outputformat() {
        let html = "<html><body><p>Published 2024-03-15</p></body></html>";
        let o = opts("%d.%m.%Y", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        assert_eq!(r.as_deref(), Some("15.03.2024"));
    }

    // -----------------------------------------------------------------------
    // find_date (sub-stage G — core.py:808-983)
    // -----------------------------------------------------------------------

    /// Ports core.py:895 + examine_header — `<meta property=
    /// "article:published_time">` resolves via the header walk.
    #[test]
    fn find_date_from_meta_article_published_time() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15T10:00:00Z">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:940 + examine_time_elements — fallback to `<time
    /// datetime="...">` once the header / json / abbr / date_elements
    /// cascade all miss.
    #[test]
    fn find_date_from_time_element() {
        let html = r#"<html><head></head><body>
            <article><time datetime="2024-06-15">Jun 15</time></article>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:895 + json_search — JSON-LD `datePublished` populates
    /// via the JSON scan when header doesn't fire.
    #[test]
    fn find_date_from_jsonld_date_published() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article","datePublished":"2024-06-15T10:00:00Z"}
            </script>
        </head><body><p>An article.</p></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        // original=true so json_search picks `datePublished` (json_published).
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:970-981 — extensive_search free-text + search_page
    /// rescue. With no markup-level signals and only "Last updated: ..."
    /// in body text, the FREE_TEXT_EXPRESSIONS walk + search_page
    /// fallback finds the ISO date.
    #[test]
    fn find_date_from_search_page_cascade() {
        // Pure body text, no <meta>, <time>, <abbr>, JSON-LD. The
        // FREE_TEXT_EXPRESSIONS iter feeds segments to compare_reference,
        // and search_page's THREE_LOOSE_PATTERN arm picks up the ISO date.
        let html = r#"<html><head></head><body>
            <p>Last updated: 2024-03-15 for clarity.</p>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        // extensive=true is required to reach the search_page fallback.
        let o = Extractor::new(
            true,
            (2030, 12, 31),
            (1995, 1, 1),
            false,
            "%Y-%m-%d".to_string(),
        );
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// Ports core.py:970-981 — pure-text English-prose date ("Posted:
    /// January 15, 2024") parses via the FREE_TEXT_EXPRESSIONS +
    /// `compare_reference` -> `try_date_expr` -> `regex_parse`
    /// LONG_TEXT_PATTERN arm.
    #[test]
    fn find_date_from_pure_english_text_date() {
        let html = r#"<html><head></head><body>
            <p>Posted: January 15, 2024 by the editor.</p>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = Extractor::new(
            true,
            (2030, 12, 31),
            (1995, 1, 1),
            false,
            "%Y-%m-%d".to_string(),
        );
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-01-15"));
    }

    /// Ports core.py:895 — with `original_date=true`, examine_header picks
    /// the publication date (article:published_time, EARLIER) over any
    /// modification date.
    #[test]
    fn find_date_original_picks_publication_date() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-01-15">
            <meta property="article:modified_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-01-15"));
    }

    /// Ports core.py:895 — with `original_date=false` (default),
    /// examine_header prefers the modification date (latest).
    #[test]
    fn find_date_default_picks_modification_date() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-01-15">
            <meta property="article:modified_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports core.py:983 — HTML with no plausible date returns `None`.
    #[test]
    fn find_date_returns_none_for_dateless_html() {
        let html = r#"<html><head><title>About</title></head><body>
            <p>Just a paragraph with no date markers anywhere.</p>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = Extractor::new(
            true,
            (2030, 12, 31),
            (1995, 1, 1),
            false,
            "%Y-%m-%d".to_string(),
        );
        let r = find_date(&root, &o);
        assert_eq!(r, None);
    }

    /// Ports core.py:881-891 — URL canonical link's date wins when
    /// present (the URL probe runs BEFORE header/json/abbr/time).
    #[test]
    fn find_date_from_canonical_url_date() {
        let html = r#"<html><head>
            <link rel="canonical" href="https://example.com/blog/2024/03/15/post">
        </head><body><p>Some content.</p></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = find_date(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-03-15"));
    }

    /// rationale: pin `find_date`'s outputformat-validity gate
    /// (core.rs:1389 — `format != "%Y-%m-%d" && !is_valid_format`).
    /// A non-default, invalid format (no `%` directive) is rejected up
    /// front returning None (core.py:866-867).
    #[test]
    fn find_date_rejects_invalid_outputformat() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        // "nodirective" has no '%', so is_valid_format rejects it; combined
        // with format != "%Y-%m-%d" the gate fires and find_date bails.
        let o = opts_orig("nodirective", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o), None);
    }

    /// rationale: pin `find_date`'s outputformat gate pass-through
    /// (core.rs:1389 FALSE — a non-default but VALID format proceeds) so
    /// the full pipeline emits in the requested format (core.py:866-867
    /// then the normal walk).
    #[test]
    fn find_date_honours_valid_custom_outputformat() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%d.%m.%Y", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o).as_deref(), Some("15.06.2024"));
    }

    /// rationale: pin `find_date`'s abbr-elements fallback arm
    /// (core.rs:1420-1423) — when header/json miss but an `<abbr
    /// class="published" title=...>` carries the date, the abbr walk
    /// resolves it (core.py:904-909).
    #[test]
    fn find_date_from_abbr_element() {
        let html = r#"<html><head></head><body>
            <abbr class="published" title="2024-06-15">Jun 15</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `find_date`'s date-elements walk arm
    /// (core.rs:1441 — `examine_date_elements(search_tree, date_expr)`)
    /// where a date-bearing element in the body (not header/json/abbr/time)
    /// resolves via the FAST_PREPEND + DATE_EXPRESSIONS xpath
    /// (core.py:929-941).
    #[test]
    fn find_date_from_body_date_element() {
        let html = r#"<html><head></head><body>
            <span class="date">2024-06-15</span>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `find_date`'s `extensive=false` exit arm
    /// (core.rs:1461 FALSE — the function returns None when every earlier
    /// arm misses and extensive_search is off, mirroring Python's "no
    /// fallback" terminal at core.py:983).
    #[test]
    fn find_date_returns_none_when_not_extensive_and_no_signals() {
        let html = r#"<html><head><title>Hi</title></head><body>
            <p>Just plain text with no date markers.</p>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        // extensive=false → reaches L1461 with the FALSE side and returns None.
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o), None);
    }

    /// rationale: pin `find_canonical_url`'s non-canonical `<link>` skip
    /// arm (core.rs:1497 FALSE — `rel.eq_ignore_ascii_case("canonical")`
    /// is false for stylesheets / icons). The function continues iterating
    /// and returns None when no canonical link exists.
    #[test]
    fn find_date_ignores_non_canonical_link_elements() {
        let html = r#"<html><head>
            <link rel="stylesheet" href="https://example.com/css/2024/03/15/main.css">
            <link rel="icon" href="https://example.com/icon/2024/03/15/fav.png">
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // The stylesheet/icon URLs are NOT consumed by find_canonical_url;
        // the meta tag's date wins.
        assert_eq!(find_date(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `find_canonical_url`'s missing-`href` skip arm
    /// (core.rs:1499 FALSE — `get_attribute(&link, "href")` is None). A
    /// `rel="canonical"` link without an `href` attribute is skipped and
    /// the function returns None.
    #[test]
    fn find_date_skips_canonical_link_without_href() {
        let html = r#"<html><head>
            <link rel="canonical">
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(find_date(&root, &o).as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // examine_header — additional arm coverage (core.py:235-352)
    // -----------------------------------------------------------------------

    /// rationale: pin `examine_header`'s `og:url` arm (core.py:272-273)
    /// — `name="og:url"` content goes through `extract_url_date` and
    /// lands in `reserve`, surfacing as the final headerdate.
    #[test]
    fn examine_header_og_url_populates_reserve() {
        let html = r#"<html><head>
            <meta name="og:url" content="https://example.com/2024/06/15/post">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `NAME_MODIFIED` arm with
    /// `options.original=false` — `name="lastmod"` populates headerdate
    /// directly (core.py:279-282).
    #[test]
    fn examine_header_name_lastmod_populates_headerdate_when_not_original() {
        let html = r#"<html><head>
            <meta name="lastmod" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `NAME_MODIFIED` arm with
    /// `options.original=true` — `name="lastmod"` lands in `reserve`
    /// (core.py:283-284).
    #[test]
    fn examine_header_name_lastmod_populates_reserve_when_original() {
        let html = r#"<html><head>
            <meta name="lastmod" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // With `original=true` and no publish date, reserve surfaces.
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `property=` DATE_ATTRIBUTES arm
    /// where original=true and the property is a publication marker
    /// (core.py:294 — `(is_date && original) → headerdate`).
    #[test]
    fn examine_header_property_published_with_original_populates_headerdate() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `property=` PROPERTY_MODIFIED arm
    /// where original=false (core.py:294 — `(is_mod && !original) →
    /// headerdate`).
    #[test]
    fn examine_header_property_modified_with_not_original_populates_headerdate() {
        let html = r#"<html><head>
            <meta property="article:modified_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `property=` "hurts precision"
    /// arm (core.py:296-298) — DATE attr with original=FALSE goes to
    /// reserve (surfaces when no other headerdate is found).
    #[test]
    fn examine_header_property_published_with_not_original_goes_to_reserve() {
        let html = r#"<html><head>
            <meta property="article:published_time" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // No modified-time present so the published-time reserve surfaces.
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s itemprop ITEMPROP_ATTRS_MODIFIED
    /// arm (`dateModified`) with `original=false` → headerdate.
    #[test]
    fn examine_header_itemprop_date_modified_populates_headerdate() {
        let html = r#"<html><head>
            <meta itemprop="dateModified" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s itemprop `datetime`-attribute
    /// preference arm — `elem.get("datetime") or elem.get("content")`
    /// at core.py:305 reads `datetime` first.
    #[test]
    fn examine_header_itemprop_prefers_datetime_attr_over_content() {
        let html = r#"<html><head>
            <meta itemprop="datePublished" datetime="2024-06-15" content="2020-01-01">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // datetime attr (2024-06-15) wins over content (2020-01-01).
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s itemprop `copyrightyear`
    /// rejection arm — the synthesised "<year>-01-01" fails is_valid_date
    /// (year below min), so reserve is NOT set.
    #[test]
    fn examine_header_copyrightyear_invalid_year_not_used() {
        let html = r#"<html><head>
            <meta itemprop="copyrightYear" content="1980">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // 1980 is below the 1995 minimum → reserve stays None → headerdate=None.
        assert_eq!(examine_header(&root, &o), None);
    }

    /// rationale: pin `examine_header`'s `http-equiv="date"` arm
    /// with `original=true` (core.py:331 → headerdate).
    #[test]
    fn examine_header_http_equiv_date_original_populates_headerdate() {
        let html = r#"<html><head>
            <meta http-equiv="date" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `http-equiv="date"` arm
    /// with `original=false` (core.py:333 → reserve).
    #[test]
    fn examine_header_http_equiv_date_not_original_goes_to_reserve() {
        let html = r#"<html><head>
            <meta http-equiv="date" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s `http-equiv="last-modified"`
    /// arm with `original=true` → reserve (core.py:341-343).
    #[test]
    fn examine_header_http_equiv_last_modified_original_goes_to_reserve() {
        let html = r#"<html><head>
            <meta http-equiv="last-modified" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // reserve surfaces because no headerdate was set.
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_header`'s skip-meta-without-content arm
    /// (core.py:262-267 — meta with NEITHER content nor datetime is
    /// skipped entirely).
    #[test]
    fn examine_header_skips_meta_without_content_or_datetime() {
        let html = r#"<html><head>
            <meta name="datePublished">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o), None);
    }

    /// rationale: pin `examine_header`'s pubdate-attribute non-"pubdate"
    /// value arm — `pubdate="off"` (or anything other than literal
    /// "pubdate") is silently skipped (core.py:325-327).
    #[test]
    fn examine_header_pubdate_attr_non_pubdate_value_ignored() {
        let html = r#"<html><head>
            <meta pubdate="off" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o), None);
    }

    /// rationale: pin `examine_header`'s `name=` unknown-name arm — a
    /// `name` attribute neither in DATE_ATTRIBUTES nor NAME_MODIFIED is
    /// silently skipped, leaving headerdate=None.
    #[test]
    fn examine_header_name_unknown_value_skipped() {
        let html = r#"<html><head>
            <meta name="totally-unrelated" content="2024-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_header(&root, &o), None);
    }

    /// rationale: pin `examine_header`'s early-break (`headerdate.is_some()
    /// break`) — multiple matching metas should stop at the first.
    #[test]
    fn examine_header_breaks_on_first_headerdate() {
        let html = r#"<html><head>
            <meta name="datePublished" content="2024-01-15">
            <meta name="datePublished" content="2020-06-15">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // The first meta wins.
        assert_eq!(examine_header(&root, &o).as_deref(), Some("2024-01-15"));
    }

    // -----------------------------------------------------------------------
    // examine_abbr_elements — additional shape coverage (core.py:443-497)
    // -----------------------------------------------------------------------

    /// rationale: pin `examine_abbr_elements`'s `data-utime` numeric arm
    /// (core.py:453-464) — Unix timestamp on an <abbr> populates reference.
    #[test]
    fn examine_abbr_data_utime_numeric_arm() {
        // 1718409600 = 2024-06-15 00:00:00 UTC
        let html = r#"<html><body>
            <abbr data-utime="1718409600">Jun 15, 2024</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_abbr_elements`'s `data-utime` parse-error
    /// continue arm (core.py:456-459) — non-numeric data-utime is skipped.
    #[test]
    fn examine_abbr_data_utime_non_numeric_continues() {
        let html = r#"<html><body>
            <abbr data-utime="not-a-number">Garbage</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o), None);
    }

    /// rationale: pin `examine_abbr_elements`'s class+title arm with
    /// `original=false` — uses `compare_reference` to accumulate timestamps
    /// (core.py:478-486).
    #[test]
    fn examine_abbr_class_title_not_original_uses_compare_reference() {
        let html = r#"<html><body>
            <abbr class="published" title="2024-06-15">Jun 15</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_abbr_elements`'s class+text-content arm
    /// (no title) — falls back to elem.text when title is absent
    /// (core.py:488-490).
    #[test]
    fn examine_abbr_class_text_content_arm() {
        let html = r#"<html><body>
            <abbr class="published">15 June 2024 published</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_abbr_elements`'s short-text rejection arm
    /// — text <= 10 chars is dropped (core.py:488-490 `if len(text) > 10`).
    #[test]
    fn examine_abbr_class_short_text_dropped() {
        let html = r#"<html><body>
            <abbr class="published">Jun 15</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o), None);
    }

    /// rationale: pin `examine_abbr_elements`'s empty-list arm — no
    /// <abbr> elements → early `None` (core.py:447).
    #[test]
    fn examine_abbr_no_abbr_elements_returns_none() {
        let html = "<html><body><p>no abbrs here</p></body></html>";
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o), None);
    }

    /// rationale: pin `examine_abbr_elements`'s class+title with
    /// `original=true` and `try_date_expr` returning None — invalid title
    /// (e.g. "not a date" with no digit content) leaves headerdate as
    /// None (core.py:470-475).
    #[test]
    fn examine_abbr_class_title_invalid_returns_none() {
        let html = r#"<html><body>
            <abbr class="published" title="not a date">June</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // title parse fails, no text → returns None.
        assert_eq!(examine_abbr_elements(&root, &o), None);
    }

    /// rationale: pin `examine_abbr_elements`'s class+no-title+empty-text
    /// arm (core.rs:772 `if let Some(t) = text` FALSE side) — an `<abbr
    /// class="published">` with no title AND no text child means
    /// `element_text` returns None, so the loop iteration is a no-op.
    #[test]
    fn examine_abbr_class_without_title_or_text_returns_none() {
        let html = r#"<html><body>
            <abbr class="published"></abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_abbr_elements(&root, &o), None);
    }

    /// rationale: pin `examine_abbr_elements`'s class+title with
    /// `original=false` + parse failure — compare_reference returns 0,
    /// so the `if reference > 0 { break }` FALSE side (core.rs:761) is
    /// taken (don't break, keep iterating).
    #[test]
    fn examine_abbr_class_title_unparseable_does_not_break() {
        let html = r#"<html><body>
            <abbr class="published" title="not parseable">Junk</abbr>
            <abbr class="published" title="2024-06-15">Jun 15</abbr>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // First abbr's title fails to parse (reference stays 0, no break),
        // second abbr provides the valid date.
        assert_eq!(examine_abbr_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // examine_time_elements — additional shape coverage (core.py:500-562)
    // -----------------------------------------------------------------------

    /// rationale: pin `examine_time_elements`'s `pubdate` shortcut WITHOUT
    /// the `pubdate=pubdate` attribute value — `pubdate="off"` doesn't
    /// engage the shortcut and falls into the accumulator.
    #[test]
    fn examine_time_pubdate_non_match_uses_accumulator() {
        let html = r#"<html><body>
            <time pubdate="off" datetime="2024-06-15">Jun 15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // pubdate="off" misses the shortcut; accumulator picks 2024-06-15.
        assert_eq!(examine_time_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_time_elements`'s `class="entry-date"`
    /// shortcut with `original=true` (core.py:522-538).
    #[test]
    fn examine_time_class_entry_date_shortcut() {
        let html = r#"<html><body>
            <time class="entry-date" datetime="2024-06-15">Jun 15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_time_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_time_elements`'s `class="entry-time"`
    /// shortcut arm (also core.py:522-538).
    #[test]
    fn examine_time_class_entry_time_shortcut() {
        let html = r#"<html><body>
            <time class="entry-time" datetime="2024-06-15">Jun 15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_time_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_time_elements`'s `class="updated"`
    /// shortcut arm (with `original=false`, core.py:533).
    #[test]
    fn examine_time_class_updated_shortcut_when_not_original() {
        let html = r#"<html><body>
            <time class="updated" datetime="2024-06-15">Jun 15</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_time_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_time_elements`'s "no <time> elements" arm
    /// (core.py:504-505).
    #[test]
    fn examine_time_no_elements_returns_none() {
        let html = "<html><body><p>no time tags</p></body></html>";
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(examine_time_elements(&root, &o), None);
    }

    /// rationale: pin `examine_time_elements`'s short-datetime-attr arm
    /// — `datetime.len() <= 6` falls into the text-content branch
    /// (core.py:556-558).
    #[test]
    fn examine_time_short_datetime_falls_to_text_content() {
        let html = r#"<html><body>
            <time datetime="2024">June 15, 2024</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // datetime="2024" has len 4 ≤ 6 → text content "June 15, 2024" is used.
        assert_eq!(examine_time_elements(&root, &o).as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `examine_time_elements`'s shortcut-with-failed-
    /// parse arm — datetime attr looks valid but parses to nothing,
    /// then falls back to the accumulator (which is still 0 → None).
    #[test]
    fn examine_time_shortcut_failed_parse_returns_none() {
        let html = r#"<html><body>
            <time pubdate="pubdate" datetime="not-a-real-date">June</time>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts_orig("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // try_date_expr returns None → falls through, no other shortcut, accumulator=0 → None.
        assert_eq!(examine_time_elements(&root, &o), None);
    }

    // -----------------------------------------------------------------------
    // search_page — additional deep-cascade coverage (core.py:574-805)
    // -----------------------------------------------------------------------

    /// rationale: pin `search_page`'s deep-cascade no-match arm — a page
    /// whose only date-like content is the bare 2-digit year fails every
    /// arm (THREE_COMP needs 4-digit year, SELECT_YMD needs 4-digit year,
    /// DATESTRINGS needs 8 digits, SLASHES needs DD/MM/YY, YYYYMM needs
    /// 4-digit year, MMYYYY needs 4-digit year, copyright text patterns
    /// need explicit ©/Copyright, SIMPLE needs 4-digit year).
    #[test]
    fn search_page_all_arms_miss_returns_none() {
        // Just a 2-digit year fragment with no surrounding date context.
        let html = "<html><body>see 24 things</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(search_page(html, &o), None);
    }

    /// rationale: pin `search_page`'s YYYYMM_PATTERN copyear gate
    /// — when copyear is set above the YYYY/MM year, the YYYYMM arm
    /// is skipped (core.py:728 — `if (copyear == 0 || dt.year >= copyear)`).
    #[test]
    fn search_page_yyyymm_below_copyear_falls_through() {
        // Copyright 2024, but YYYYMM only is 2020/06. The 2020 year is
        // below copyear 2024 → YYYYMM arm skipped, copyright catchall fires.
        let html = "<html><body>archive 2020/06 contents © 2024 Acme</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = search_page(html, &o);
        // The copyright year catchall returns 2024-01-01.
        assert_eq!(r.as_deref(), Some("2024-01-01"));
    }

    /// rationale: pin `search_page`'s copyright-only-page arm — © year
    /// is the only signal.
    #[test]
    fn search_page_copyright_text_only_returns_copyear() {
        let html = "<html><body>Copyright 2024 by Acme.</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(search_page(html, &o).as_deref(), Some("2024-01-01"));
    }

    /// rationale: pin `search_page`'s copyright-rejection arm — when
    /// the © year is below the min-date window, `is_valid_date` rejects
    /// the year and copyear stays 0.
    #[test]
    fn search_page_copyright_below_min_year_not_set_as_copyear() {
        // © 1980 with min_date=1995 → copyear stays 0.
        // No other date → SIMPLE arm finds "1980" but min filter drops it.
        let html = "<html><body>© 1980 Old.</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(search_page(html, &o), None);
    }

    /// rationale: pin `search_page`'s `simple_pattern` rejection by the
    /// min-date filter (year < min.year is removed from occurrences).
    #[test]
    fn search_page_simple_pattern_rejects_year_below_min() {
        // bare year 1980, no copyright. SIMPLE arm catches 1980 but
        // out-of-range → drops it.
        let html = "<html><body>see 1980 archives</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(search_page(html, &o), None);
    }

    /// rationale: pin `search_page`'s simple_pattern + copyear gate
    /// — when SIMPLE finds year < copyear, it's rejected
    /// (core.py:794-797 — `year >= copyear`). The copyright catchall
    /// then fires and returns `copyear-01-01`.
    #[test]
    fn search_page_simple_year_below_copyear_uses_copyear_catchall() {
        // © 2024, plus a bare "2020" elsewhere. SIMPLE finds 2020 but
        // 2020 < copyear 2024 → drops it. Catchall returns 2024-01-01.
        let html = "<html><body>info 2020 © 2024 Inc.</body></html>";
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(search_page(html, &o).as_deref(), Some("2024-01-01"));
    }

    // -----------------------------------------------------------------------
    // make_date — calendar boundary arms
    // -----------------------------------------------------------------------

    /// rationale: pin `make_date`'s month-zero rejection arm.
    #[test]
    fn make_date_rejects_zero_month() {
        assert_eq!(make_date(2024, 0, 15), None);
    }

    /// rationale: pin `make_date`'s month-13 rejection arm.
    #[test]
    fn make_date_rejects_month_over_twelve() {
        assert_eq!(make_date(2024, 13, 15), None);
    }

    /// rationale: pin `make_date`'s day-zero rejection arm.
    #[test]
    fn make_date_rejects_zero_day() {
        assert_eq!(make_date(2024, 6, 0), None);
    }

    /// rationale: pin `make_date`'s day-32 rejection arm.
    #[test]
    fn make_date_rejects_day_over_thirty_one() {
        assert_eq!(make_date(2024, 6, 32), None);
    }

    /// rationale: pin `make_date`'s happy path.
    #[test]
    fn make_date_accepts_valid_date() {
        assert!(make_date(2024, 6, 15).is_some());
    }

    // -----------------------------------------------------------------------
    // recapture_ymd_groups — None arm coverage
    // -----------------------------------------------------------------------

    /// rationale: pin `recapture_ymd_groups`'s captures-None arm — when
    /// the catch regex doesn't match at all, returns None.
    #[test]
    fn recapture_ymd_groups_returns_none_for_non_matching_string() {
        use super::super::regex_catalogues::ymd_pattern;
        let r = recapture_ymd_groups("no date here", ymd_pattern());
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // examine_text — additional arm coverage
    // -----------------------------------------------------------------------

    /// rationale: pin `examine_text`'s scrub arm — trailing non-digits
    /// are removed after MAX_SEGMENT_LEN truncation.
    #[test]
    fn examine_text_scrubs_trailing_non_digits() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // "2024-06-15abcdef" — the trailing letters are scrubbed by
        // NON_DIGITS_REGEX before try_date_expr fires.
        let r = examine_text("2024-06-15abcdef", &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // examine_date_elements — element-bound arms
    // -----------------------------------------------------------------------

    /// rationale: pin `examine_date_elements`'s "no matches" arm.
    #[test]
    fn examine_date_elements_no_match_returns_none() {
        let html = "<html><body><p>no dates</p></body></html>";
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_date_elements(&root, ".//time", &o);
        assert_eq!(r, None);
    }

    /// rationale: pin `examine_date_elements`'s title-attribute arm
    /// (core.py:227 — iterate over `[text_content, title]`).
    #[test]
    fn examine_date_elements_uses_title_attribute() {
        let html = r#"<html><body>
            <span title="published 2024-06-15 today">June</span>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = examine_date_elements(&root, ".//span", &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }
}
