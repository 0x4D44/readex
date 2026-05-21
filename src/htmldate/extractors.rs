//! `extractors` — sub-stage D port of the non-dateparser parsing layer of
//! `htmldate/extractors.py`.
//!
//! Source of truth: `htmldate@1.9.x/extractors.py` (vendored under
//! `C:\Users\marti\AppData\Roaming\Python\Python314\site-packages\htmldate\
//! extractors.py`). Every function cites its exact Python source line range per
//! the M4 Stage 1 sub-stage D anti-inversion contract.
//!
//! Sub-stages A (settings + Extractor + trim_text), B (validators), and C
//! (regex catalogues + month tables) supply the building blocks; this
//! sub-stage wires them into the post-line-216 algorithm:
//!
//! - `discard_unwanted`        (extractors.py:216-222)
//! - `extract_url_date`        (extractors.py:225-242)
//! - `correct_year`            (extractors.py:245-249)
//! - `try_swap_values`         (extractors.py:252-254)
//! - `regex_parse`             (extractors.py:257-283)
//! - `custom_parse`            (extractors.py:286-383)
//! - `external_date_parser`    (extractors.py:386-396) — STUB
//! - `try_date_expr`           (extractors.py:399-437)
//! - `img_search`              (extractors.py:440-451)
//! - `pattern_search`          (extractors.py:454-466)
//! - `json_search`             (extractors.py:469-483)
//! - `idiosyncrasies_search`   (extractors.py:486-508)
//!
//! # Faithful divergences (recorded — HLD §4 anti-inversion)
//!
//! ## `dateutil.parser.parse(fuzzy=False)` cascade
//!
//! Python's `extractors.py:310` falls through to `dateutil.parser.parse(string,
//! fuzzy=False)` after the `datetime.fromisoformat` branch fails. The Rust
//! `regex`-only ecosystem has no direct equivalent. Strategy: cover the common
//! `dateutil` shapes via a small explicit format list driven through sub-stage
//! B's `format_parse`. If none match, return `None`. Documented at the call
//! site; if a corpus regression surfaces, extend the format list.
//!
//! ## `datetime.fromisoformat`
//!
//! Python 3.11+ accepts a fairly liberal ISO 8601 grammar (including `"Z"`
//! and `"+00:00"` offsets). Implemented as `try_fromisoformat` covering the
//! shapes htmldate's `custom_parse` actually feeds into `fromisoformat`:
//! `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS`, `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`, `...Z`,
//! and `YYYY-MM-DD HH:MM:SS`.
//!
//! ## `external_date_parser` (dateparser)
//!
//! The Python `external_date_parser` calls `dateparser.DateDataParser`
//! (~10k LOC + 200 locale YAML files). Per the M4 Stage 1 scoping report,
//! `dateparser` is deferred indefinitely; the Rust stub returns `None`
//! unconditionally. The downstream gate in `try_date_expr`
//! (extractors.py:429) requires `extensive_search=True` AND `TEXT_DATE_PATTERN`
//! match before reaching it, so the dateparser fallback rarely fires.
//!
//! ## `@lru_cache` on `try_date_expr`
//!
//! Python `extractors.py:399` decorates `try_date_expr` with
//! `@lru_cache(maxsize=CACHE_SIZE)`. The Rust port does NOT cache —
//! every call recomputes. Pure perf optimisation, observable result is
//! identical. Matches sub-stage B's `@lru_cache`-deferral precedent.
//!
//! ## `match.lastgroup` semantics
//!
//! Python `re.Match.lastgroup` returns the name of the LAST named group that
//! participated in the match. Rust's `regex::Captures` has no direct
//! equivalent; this module ships a small `last_named_group` helper deferred
//! from sub-stage C.

use regex::{Captures, Regex};

use super::regex_catalogues::{
    complete_url, discard_patterns, json_modified, json_published, long_text_pattern,
    text_date_pattern, text_months, text_patterns, ym_pattern, ymd_no_sep_pattern, ymd_pattern,
};
use super::utils::{Extractor, trim_text};
use super::validators::{
    DateInput, DateTime, format_emit, is_valid_date, validate_and_convert,
};

use crate::readability::dom::{
    self, NodeRef, attributes_in_source_order, children, get_attribute, parent, tag_name,
    text_content,
};
use crate::trafilatura::xpath_engine;

// ===========================================================================
// MAX_SEGMENT_LEN (extractors.py:86)
// ===========================================================================

/// Maximum length of a candidate date string. Per `extractors.py:86`
/// (`MAX_SEGMENT_LEN = 52`). Consumed by `try_date_expr` to trim long
/// strings before regex evaluation.
pub const MAX_SEGMENT_LEN: usize = 52;

// ===========================================================================
// `lastgroup` helper (deferred from sub-stage C)
// ===========================================================================

/// Returns the name of the LAST matched named group, mimicking Python's
/// `re.Match.lastgroup`.
///
/// Walks the candidate `group_names` slice in REVERSE registration order and
/// returns the LAST non-None group. This mirrors `re.Match.lastgroup`'s
/// "last named group that participated in the match" semantic for the
/// patterns htmldate actually consumes (`YMD_PATTERN`, `YM_PATTERN`,
/// `LONG_TEXT_PATTERN` — each defines pairs of named groups where exactly
/// one arm of the alternation matches).
///
/// Deferred from sub-stage C; consumers are `custom_parse` (extractors.py:337,
/// :364) and `regex_parse` (extractors.py:267).
pub(crate) fn last_named_group<'a>(caps: &Captures<'_>, group_names: &[&'a str]) -> Option<&'a str> {
    group_names
        .iter()
        .rev()
        .find(|name| caps.name(name).is_some())
        .copied()
}

// ===========================================================================
// discard_unwanted (extractors.py:216-222)
// ===========================================================================

/// Delete unwanted sections of an HTML document and return them as a list.
///
/// Ports `extractors.py:216-222`:
///
/// ```python
/// def discard_unwanted(tree):
///     my_discarded = []
///     for subtree in DISCARD_EXPRESSIONS(tree):
///         my_discarded.append(subtree)
///         subtree.getparent().remove(subtree)
///     return tree, my_discarded
/// ```
///
/// `DISCARD_EXPRESSIONS` at `extractors.py:90` is the literal XPath
/// `.//div[@id="wm-ipp-base" or @id="wm-ipp"]` (archive.org banner inserts).
/// Implemented via the Stage 0b XPath engine.
///
/// Returns the list of removed subtree references (the input `tree` is
/// mutated in place — like the Python original).
pub fn discard_unwanted(tree: &NodeRef) -> Vec<NodeRef> {
    let mut discarded = Vec::new();
    let matches = xpath_engine::evaluate(
        r#".//div[@id="wm-ipp-base" or @id="wm-ipp"]"#,
        tree,
    )
    .unwrap_or_default();
    for subtree in matches {
        // `subtree.getparent().remove(subtree)` — drop the subtree from its
        // parent; the dom facade's `remove` does both halves in one call.
        if parent(&subtree).is_some() {
            dom::remove(&subtree);
            discarded.push(subtree);
        }
    }
    discarded
}

// ===========================================================================
// extract_url_date (extractors.py:225-242)
// ===========================================================================

/// Extract the date out of a URL string complying with the Y-M-D format.
///
/// Ports `extractors.py:225-242` verbatim. `COMPLETE_URL` at
/// `extractors.py:129` matches `YYYY[/_-]MM[/_-]DD` with three capture
/// groups (year, month, day). Returns the formatted date string on success
/// or `None` (URL is `None`, no match, or out-of-range date).
pub fn extract_url_date(testurl: Option<&str>, options: &Extractor) -> Option<String> {
    let url = testurl?;
    let caps = complete_url().captures(url)?;
    // extractors.py:235 — datetime(int(match[1]), int(match[2]), int(match[3])).
    let year: i32 = caps.get(1)?.as_str().parse().ok()?;
    let month: u32 = caps.get(2)?.as_str().parse().ok()?;
    let day: u32 = caps.get(3)?.as_str().parse().ok()?;
    let dt = make_datetime(year, month, day)?;
    // extractors.py:236-238 — is_valid_date guard.
    let di = DateInput::DateTime(dt);
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    if !is_valid_date(Some(&di), &options.format, &earliest, &latest) {
        return None;
    }
    // extractors.py:239 — dateobject.strftime(options.format).
    format_emit(&dt, &options.format).ok()
}

// ===========================================================================
// correct_year (extractors.py:245-249)
// ===========================================================================

/// Adapt year from YY to YYYY format.
///
/// Ports `extractors.py:245-249` verbatim:
///
/// ```python
/// def correct_year(year: int) -> int:
///     if year < 100:
///         year += 1900 if year >= 90 else 2000
///     return year
/// ```
///
/// Examples:
/// - `98` → `1998` (90 <= 98 < 100, +1900)
/// - `23` → `2023` (23 < 90, +2000)
/// - `00` → `2000` (00 < 90, +2000)
/// - `2024` → `2024` (>= 100, untouched)
pub fn correct_year(year: i32) -> i32 {
    if year < 100 {
        if year >= 90 {
            year + 1900
        } else {
            year + 2000
        }
    } else {
        year
    }
}

// ===========================================================================
// try_swap_values (extractors.py:252-254)
// ===========================================================================

/// Swap day and month values if it seems feasible.
///
/// Ports `extractors.py:252-254` verbatim:
///
/// ```python
/// def try_swap_values(day: int, month: int) -> Tuple[int, int]:
///     return (month, day) if month > 12 and day <= 12 else (day, month)
/// ```
///
/// Used by `custom_parse` and `regex_parse` to repair ambiguous DD/MM
/// orderings (e.g. a 15-as-month would swap with a 03-as-day).
pub fn try_swap_values(day: u32, month: u32) -> (u32, u32) {
    if month > 12 && day <= 12 {
        (month, day)
    } else {
        (day, month)
    }
}

// ===========================================================================
// regex_parse (extractors.py:257-283)
// ===========================================================================

/// Try full-text parse for date elements using `LONG_TEXT_PATTERN`
/// (multilingual day-month-year + American English patterns).
///
/// Ports `extractors.py:257-283`. Uses `last_named_group` to detect which
/// arm of the pattern's alternation matched (`year` vs `year2`).
pub fn regex_parse(string: &str) -> Option<DateTime> {
    let pat = long_text_pattern();
    let caps = pat.captures(string)?;
    // extractors.py:265-269 — `("day","month","year")` if lastgroup=="year"
    // else `("day2","month2","year2")`.
    let last = last_named_group(&caps, &["month", "day", "year", "day2", "month2", "year2"]);
    let groups: (&str, &str, &str) = if last == Some("year") {
        ("day", "month", "year")
    } else {
        ("day2", "month2", "year2")
    };
    // extractors.py:271-276 — int(...) + TEXT_MONTHS lookup.
    let day_str = caps.name(groups.0)?.as_str();
    let month_token = caps.name(groups.1)?.as_str();
    let year_str = caps.name(groups.2)?.as_str();
    let day: u32 = day_str.parse().ok()?;
    // Python: TEXT_MONTHS[match.group(groups[1]).lower().strip(".")]
    let month_key = month_token.to_lowercase();
    let month_key = month_key.trim_matches('.');
    let month: u32 = *text_months().get(month_key)?;
    let year_raw: i32 = year_str.parse().ok()?;
    // extractors.py:277-278 — correct_year + try_swap_values.
    let year = correct_year(year_raw);
    let (day, month) = try_swap_values(day, month);
    // extractors.py:279 — datetime(year, month, day) (any ValueError -> None).
    make_datetime(year, month, day)
}

// ===========================================================================
// custom_parse (extractors.py:286-383)
// ===========================================================================

/// Try to bypass the slow dateparser via fast custom heuristics.
///
/// Ports `extractors.py:286-383`. Faithful five-step cascade:
///
/// 1. **Year-starts-with-digits shortcut**: if `string[:4]` is digits, try
///    (a) the `YYYYMMDD` 8-digit form, (b) `fromisoformat`, (c) a fallback
///    `dateutil_parse`-equivalent format list. Validate + emit.
/// 2. **`YMD_NO_SEP_PATTERN`** scan (8 consecutive digits anywhere).
/// 3. **`YMD_PATTERN`** scan (year-month-day OR day-month-year with `-/.`).
/// 4. **`YM_PATTERN`** scan (year-month OR month-year). Defaults day to 1.
/// 5. Falls through to `regex_parse` + `validate_and_convert`.
pub fn custom_parse(
    string: &str,
    outputformat: &str,
    min_date: &DateTime,
    max_date: &DateTime,
) -> Option<String> {
    // ----------------------------------------------------------------------
    // 1. extractors.py:292-318 — `string[:4].isdigit()` shortcut
    // ----------------------------------------------------------------------
    if string.len() >= 4 && string[..4].chars().all(|c| c.is_ascii_digit()) {
        // a. extractors.py:295-302 — 8-digit YYYYMMDD form.
        let candidate: Option<DateTime> =
            if string.len() >= 8 && string[4..8].chars().all(|c| c.is_ascii_digit()) {
                let y: i32 = string[..4].parse().ok()?;
                let m: u32 = string[4..6].parse().ok()?;
                let d: u32 = string[6..8].parse().ok()?;
                make_datetime(y, m, d)
            } else {
                // b. extractors.py:305-312 — fromisoformat, then dateutil
                // fallback.
                try_fromisoformat(string).or_else(|| dateutil_parse_fallback(string))
            };
        // c. extractors.py:313-318 — plausibility test + emit.
        if let Some(c) = candidate {
            let di = DateInput::DateTime(c);
            if is_valid_date(Some(&di), outputformat, min_date, max_date) {
                return format_emit(&c, outputformat).ok();
            }
        }
    }

    // ----------------------------------------------------------------------
    // 2. extractors.py:320-331 — YMD_NO_SEP_PATTERN scan.
    // ----------------------------------------------------------------------
    if let Some(caps) = ymd_no_sep_pattern().captures(string)
        && let Some(m) = caps.get(1)
    {
        let s = m.as_str();
        let y: i32 = s[..4].parse().ok().unwrap_or(0);
        let mo: u32 = s[4..6].parse().ok().unwrap_or(0);
        let d: u32 = s[6..8].parse().ok().unwrap_or(0);
        if let Some(c) = make_datetime(y, mo, d) {
            let di = DateInput::DateTime(c);
            // extractors.py:329 — `is_valid_date(candidate, "%Y-%m-%d", ...)`.
            if is_valid_date(Some(&di), "%Y-%m-%d", min_date, max_date) {
                return format_emit(&c, outputformat).ok();
            }
        }
    }

    // ----------------------------------------------------------------------
    // 3. extractors.py:333-358 — YMD_PATTERN scan.
    // ----------------------------------------------------------------------
    if let Some(caps) = ymd_pattern().captures(string) {
        // extractors.py:337 — `match.lastgroup == "day"` selects YMD arm.
        let last = last_named_group(
            &caps,
            &["year", "month", "day", "day2", "month2", "year2"],
        );
        let candidate = if last == Some("day") {
            let y: i32 = caps.name("year")?.as_str().parse().ok()?;
            let mo: u32 = caps.name("month")?.as_str().parse().ok()?;
            let d: u32 = caps.name("day")?.as_str().parse().ok()?;
            make_datetime(y, mo, d)
        } else {
            let d: u32 = caps.name("day2")?.as_str().parse().ok()?;
            let mo: u32 = caps.name("month2")?.as_str().parse().ok()?;
            let y_raw: i32 = caps.name("year2")?.as_str().parse().ok()?;
            let y = correct_year(y_raw);
            let (d, mo) = try_swap_values(d, mo);
            make_datetime(y, mo, d)
        };
        if let Some(c) = candidate {
            let di = DateInput::DateTime(c);
            if is_valid_date(Some(&di), "%Y-%m-%d", min_date, max_date) {
                return format_emit(&c, outputformat).ok();
            }
        }
    }

    // ----------------------------------------------------------------------
    // 4. extractors.py:360-377 — YM_PATTERN scan.
    // ----------------------------------------------------------------------
    if let Some(caps) = ym_pattern().captures(string) {
        // extractors.py:364 — `match.lastgroup == "month"` selects YM arm.
        let last = last_named_group(&caps, &["year", "month", "month2", "year2"]);
        let candidate = if last == Some("month") {
            let y: i32 = caps.name("year")?.as_str().parse().ok()?;
            let mo: u32 = caps.name("month")?.as_str().parse().ok()?;
            make_datetime(y, mo, 1)
        } else {
            let y: i32 = caps.name("year2")?.as_str().parse().ok()?;
            let mo: u32 = caps.name("month2")?.as_str().parse().ok()?;
            make_datetime(y, mo, 1)
        };
        if let Some(c) = candidate {
            let di = DateInput::DateTime(c);
            if is_valid_date(Some(&di), "%Y-%m-%d", min_date, max_date) {
                return format_emit(&c, outputformat).ok();
            }
        }
    }

    // ----------------------------------------------------------------------
    // 5. extractors.py:379-383 — regex_parse fallback.
    // ----------------------------------------------------------------------
    let dateobject = regex_parse(string)?;
    let di = DateInput::DateTime(dateobject);
    validate_and_convert(Some(&di), outputformat, min_date, max_date)
}

/// Mirrors Python `datetime.fromisoformat` for the shapes htmldate's
/// `custom_parse` feeds in. Covers the bare `YYYY-MM-DD`,
/// `YYYY-MM-DDTHH:MM:SS`, `YYYY-MM-DDTHH:MM:SS+ZZ:ZZ`, `...Z`, and
/// `YYYY-MM-DD HH:MM:SS` shapes. Time-zone offsets are accepted and
/// ignored (Python returns timezone-aware datetimes; the validators only
/// use Y/M/D so the truncation is observation-equivalent here).
///
/// Returns `None` for unrecognised shapes (Python raises `ValueError`).
pub fn try_fromisoformat(s: &str) -> Option<DateTime> {
    // Strip a trailing `Z` or `+HH:MM`/`-HH:MM` offset; leave the rest for
    // the explicit-form match.
    let stripped = strip_tz_suffix(s);

    // Date-only shape.
    if stripped.len() == 10 {
        return parse_ymd(stripped);
    }
    // Date + time shape.
    if stripped.len() == 19 {
        let b = stripped.as_bytes();
        if b[10] != b'T' && b[10] != b' ' {
            return None;
        }
        let date = parse_ymd(&stripped[..10])?;
        let h: u32 = stripped[11..13].parse().ok()?;
        if b[13] != b':' {
            return None;
        }
        let mi: u32 = stripped[14..16].parse().ok()?;
        if b[16] != b':' {
            return None;
        }
        let se: u32 = stripped[17..19].parse().ok()?;
        if h > 23 || mi > 59 || se > 59 {
            return None;
        }
        return Some(DateTime {
            year: date.year,
            month: date.month,
            day: date.day,
            hour: h,
            minute: mi,
            second: se,
        });
    }
    None
}

fn strip_tz_suffix(s: &str) -> &str {
    if let Some(stripped) = s.strip_suffix('Z') {
        return stripped;
    }
    // `+HH:MM` / `-HH:MM` — 6 trailing chars.
    if s.len() >= 6 {
        let b = s.as_bytes();
        let off_start = s.len() - 6;
        if (b[off_start] == b'+' || b[off_start] == b'-')
            && b[off_start + 1].is_ascii_digit()
            && b[off_start + 2].is_ascii_digit()
            && b[off_start + 3] == b':'
            && b[off_start + 4].is_ascii_digit()
            && b[off_start + 5].is_ascii_digit()
        {
            return &s[..off_start];
        }
    }
    s
}

fn parse_ymd(s: &str) -> Option<DateTime> {
    if s.len() != 10 {
        return None;
    }
    let b = s.as_bytes();
    if b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s[..4].parse().ok()?;
    let mo: u32 = s[5..7].parse().ok()?;
    let d: u32 = s[8..10].parse().ok()?;
    make_datetime(y, mo, d)
}

/// Stand-in for Python's `dateutil.parser.parse(string, fuzzy=False)`.
///
/// Rust has no direct `dateutil` equivalent. Strategy: try a small explicit
/// format list covering the common shapes that fall through Python's
/// `fromisoformat` (which the prior branch already handled). If none match,
/// return `None`. Documented divergence; extend if a corpus regression
/// surfaces.
fn dateutil_parse_fallback(string: &str) -> Option<DateTime> {
    const FORMATS: &[&str] = &[
        "%Y-%m-%d",
        "%Y/%m/%d",
        "%Y.%m.%d",
        "%d.%m.%Y",
        "%d/%m/%Y",
        "%d-%m-%Y",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%d %H:%M:%S",
    ];
    for fmt in FORMATS {
        if let Ok(dt) = super::validators::format_parse(string, fmt) {
            return Some(dt);
        }
    }
    None
}

/// Constructs a `DateTime` at midnight from `(year, month, day)`, returning
/// `None` for invalid calendar dates. Mirrors Python `datetime(y, m, d)`
/// which raises `ValueError` on invalid input.
fn make_datetime(year: i32, month: u32, day: u32) -> Option<DateTime> {
    if !(1..=12).contains(&month) {
        return None;
    }
    let max_d = days_in_month(year, month);
    if !(1..=max_d).contains(&day) {
        return None;
    }
    Some(DateTime {
        year,
        month,
        day,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

// ===========================================================================
// external_date_parser (extractors.py:386-396) — STUB
// ===========================================================================

/// Ports `extractors.py:386-396` (`external_date_parser`).
///
/// **STUB** — the real implementation calls `dateparser.DateDataParser`
/// (~10k LOC + 200 locale YAML files). Per the M4 Stage 1 scoping report,
/// dateparser is deferred indefinitely. This function returns `None`
/// unconditionally. The downstream gate in `try_date_expr` (extractors.py:429)
/// requires `extensive_search=True` AND `TEXT_DATE_PATTERN` match before
/// reaching us, so the dateparser fallback rarely fires.
pub fn external_date_parser(_string: &str, _outputformat: &str) -> Option<String> {
    None
}

// ===========================================================================
// try_date_expr (extractors.py:399-437)
// ===========================================================================

/// Use a series of heuristics and rules to parse a potential date expression.
///
/// Ports `extractors.py:399-437`. LEAF orchestrator that:
/// 1. trims + caps length at `MAX_SEGMENT_LEN`
/// 2. checks digit-count is in `[4, 18]`
/// 3. rejects via `DISCARD_PATTERNS`
/// 4. tries `custom_parse` (fast path)
/// 5. falls back to `external_date_parser` (STUB) when
///    `extensive_search && TEXT_DATE_PATTERN.matches(string)`
///
/// Python `@lru_cache(maxsize=CACHE_SIZE)` is NOT ported — see module docs.
pub fn try_date_expr(
    string: Option<&str>,
    outputformat: &str,
    extensive_search: bool,
    min_date: &DateTime,
    max_date: &DateTime,
) -> Option<String> {
    let raw = string?;
    if raw.is_empty() {
        return None;
    }

    // extractors.py:412 — string = trim_text(string)[:MAX_SEGMENT_LEN].
    let trimmed = trim_text(raw);
    let truncated: String = trimmed.chars().take(MAX_SEGMENT_LEN).collect();
    if truncated.is_empty() {
        return None;
    }

    // extractors.py:415 — `not 4 <= sum(map(str.isdigit, string)) <= 18`.
    let digit_count = truncated.chars().filter(|c| c.is_ascii_digit()).count();
    if !(4..=18).contains(&digit_count) {
        return None;
    }

    // extractors.py:419 — DISCARD_PATTERNS reject.
    if discard_patterns().is_match(&truncated) {
        return None;
    }

    // extractors.py:422-425 — fast path via custom_parse.
    if let Some(s) = custom_parse(&truncated, outputformat, min_date, max_date) {
        return Some(s);
    }

    // extractors.py:429-435 — extensive_search + TEXT_DATE_PATTERN gated
    // dateparser fallback (Rust stub returns None).
    if extensive_search && text_date_pattern().is_match(&truncated) {
        let parsed = external_date_parser(&truncated, outputformat);
        if let Some(ref p) = parsed {
            let di = DateInput::Str(p);
            if is_valid_date(Some(&di), outputformat, min_date, max_date) {
                return parsed;
            }
        }
    }

    None
}

// ===========================================================================
// img_search (extractors.py:440-451)
// ===========================================================================

/// Skim through image elements for an `og:image` URL carrying a date.
///
/// Ports `extractors.py:440-451`. Python uses
/// `tree.find('.//meta[@property="og:image"][@content]')`; the Rust port
/// uses the Stage 0b XPath engine and then reads the `content` attribute
/// via the dom facade.
pub fn img_search(tree: &NodeRef, options: &Extractor) -> Option<String> {
    // Stage 0b xpath_engine supports `[a][b]` predicate-list shape.
    let matches = xpath_engine::evaluate(
        r#".//meta[@property="og:image"][@content]"#,
        tree,
    )
    .ok()?;
    let element = matches.into_iter().next()?;
    let content = get_attribute(&element, "content")?;
    extract_url_date(Some(&content), options)
}

// ===========================================================================
// pattern_search (extractors.py:454-466)
// ===========================================================================

/// Look for date expressions using a regular expression on a string of text.
///
/// Ports `extractors.py:454-466`. The Python source captures group 1 as the
/// pre-formatted `%Y-%m-%d` candidate. Validates it against the options'
/// min/max window, then converts to the requested output format.
pub fn pattern_search(text: &str, date_pattern: &Regex, options: &Extractor) -> Option<String> {
    let caps = date_pattern.captures(text)?;
    let candidate = caps.get(1)?.as_str();
    let di = DateInput::Str(candidate);
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    if !is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest) {
        return None;
    }
    super::validators::convert_date(candidate, "%Y-%m-%d", &options.format).ok()
}

// ===========================================================================
// json_search (extractors.py:469-483)
// ===========================================================================

/// Look for JSON time patterns in `application/ld+json` /
/// `application/settings+json` script blocks.
///
/// Ports `extractors.py:469-483`. Walks the matching `<script>` elements in
/// document order, skipping any whose text doesn't contain `"date`. Returns
/// the first successful match.
pub fn json_search(tree: &NodeRef, options: &Extractor) -> Option<String> {
    let pattern = if options.original {
        json_published()
    } else {
        json_modified()
    };
    let scripts = xpath_engine::evaluate(
        r#".//script[@type="application/ld+json" or @type="application/settings+json"]"#,
        tree,
    )
    .unwrap_or_default();
    for elem in &scripts {
        let raw = text_content(elem);
        if raw.is_empty() || !raw.contains("\"date") {
            continue;
        }
        if let Some(found) = pattern_search(&raw, pattern, options) {
            return Some(found);
        }
    }
    None
}

// ===========================================================================
// idiosyncrasies_search (extractors.py:486-508)
// ===========================================================================

/// Look for author-written dates throughout the web page (multilingual
/// `TEXT_PATTERNS` scan).
///
/// Ports `extractors.py:486-508`. After a successful match, partitions the
/// non-empty groups into (year, month, day) following Python's
/// `len(parts[0]) == 4` first-position heuristic.
pub fn idiosyncrasies_search(htmlstring: &str, options: &Extractor) -> Option<String> {
    let pat = text_patterns();
    let caps = pat.captures(htmlstring)?;
    // extractors.py:493 — list(filter(None, match.groups())).
    // Python's `match.groups()` returns ALL groups; `filter(None, ...)`
    // drops empties / Nones. Walk groups 1..N and collect the non-empty.
    let mut parts: Vec<&str> = Vec::new();
    for i in 1..caps.len() {
        if let Some(m) = caps.get(i) {
            let s = m.as_str();
            if !s.is_empty() {
                parts.push(s);
            }
        }
    }
    if parts.len() < 3 {
        return None;
    }
    let candidate: Option<DateTime> = if parts[0].len() == 4 {
        // extractors.py:496-497 — year in first position.
        let y: i32 = parts[0].parse().ok()?;
        let mo: u32 = parts[1].parse().ok()?;
        let d: u32 = parts[2].parse().ok()?;
        make_datetime(y, mo, d)
    } else {
        // extractors.py:498-501 — DD/MM/YY arm.
        let d_raw: u32 = parts[0].parse().ok()?;
        let mo_raw: u32 = parts[1].parse().ok()?;
        let y_raw: i32 = parts[2].parse().ok()?;
        let (d, mo) = try_swap_values(d_raw, mo_raw);
        let y = correct_year(y_raw);
        make_datetime(y, mo, d)
    };
    let c = candidate?;
    let di = DateInput::DateTime(c);
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    if !is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest) {
        return None;
    }
    format_emit(&c, &options.format).ok()
}

// ===========================================================================
// Internal helper: silence the unused-imports warning for `attributes_in_source_order`
// + `children` + `tag_name`. They are re-exported in the dom facade but the
// extractors module doesn't currently consume them directly — they're kept on
// the use line so callers can lean on this module's named imports without
// re-importing the dom facade. Suppress via `#[allow(dead_code)]`-style
// fake-use.
// ===========================================================================

#[allow(dead_code)]
fn _silence_unused() {
    let _ = attributes_in_source_order;
    let _ = children;
    let _ = tag_name;
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

    fn dt(y: i32, m: u32, d: u32) -> DateTime {
        DateTime::from_ymd((y, m, d))
    }

    // -----------------------------------------------------------------------
    // discard_unwanted
    // -----------------------------------------------------------------------

    /// Ports extractors.py:216-222 — DISCARD_EXPRESSIONS removes archive.org
    /// banner divs.
    #[test]
    fn discard_unwanted_removes_archive_org_banner() {
        let html = r#"<html><body>
            <div id="wm-ipp-base">Archive banner</div>
            <p>Article body</p>
            <div id="wm-ipp">Old banner</div>
        </body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let discarded = discard_unwanted(&root);
        assert_eq!(
            discarded.len(),
            2,
            "should drop both wm-ipp-base and wm-ipp divs"
        );
        // The article body must survive.
        let remaining = text_content(&root);
        assert!(remaining.contains("Article body"));
        assert!(!remaining.contains("Archive banner"));
        assert!(!remaining.contains("Old banner"));
    }

    /// Ports extractors.py:216-222 — an unmatched tree returns an empty
    /// discard list.
    #[test]
    fn discard_unwanted_no_match_returns_empty() {
        let dom = Dom::parse("<html><body><p>Hello</p></body></html>");
        let root = dom.root_element().expect("html root");
        let discarded = discard_unwanted(&root);
        assert!(discarded.is_empty());
    }

    // -----------------------------------------------------------------------
    // extract_url_date
    // -----------------------------------------------------------------------

    /// Ports extractors.py:225-242 — happy path: a YYYY/MM/DD URL fragment.
    #[test]
    fn extract_url_date_finds_dated_url() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = extract_url_date(Some("https://example.com/2024/06/15/post"), &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:225-242 — no date in URL returns None.
    #[test]
    fn extract_url_date_no_match_returns_none() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(
            extract_url_date(Some("https://example.com/about"), &o),
            None
        );
    }

    /// Ports extractors.py:225-242 — `None` input short-circuits to None.
    #[test]
    fn extract_url_date_none_input() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(extract_url_date(None, &o), None);
    }

    /// Ports extractors.py:225-242 — multiple dates: regex matches first
    /// plausible (left-most).
    #[test]
    fn extract_url_date_multiple_dates_picks_first() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // The COMPLETE_URL regex finds the leftmost plausible date.
        let r = extract_url_date(
            Some("https://example.com/2024/06/15/and/2023/01/01/post"),
            &o,
        );
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:236-238 — out-of-window date is rejected.
    #[test]
    fn extract_url_date_out_of_range_rejected() {
        let o = opts("%Y-%m-%d", (2024, 1, 1), (2024, 12, 31));
        // 2020 is below the configured floor.
        let r = extract_url_date(Some("https://example.com/2020/06/15/post"), &o);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // correct_year
    // -----------------------------------------------------------------------

    /// Ports extractors.py:245-249 — `98` becomes 1998 (>= 90).
    #[test]
    fn correct_year_98_becomes_1998() {
        assert_eq!(correct_year(98), 1998);
    }

    /// Ports extractors.py:245-249 — `23` becomes 2023 (< 90).
    #[test]
    fn correct_year_23_becomes_2023() {
        assert_eq!(correct_year(23), 2023);
    }

    /// Ports extractors.py:245-249 — `00` becomes 2000 (< 90).
    #[test]
    fn correct_year_00_becomes_2000() {
        assert_eq!(correct_year(0), 2000);
    }

    /// Ports extractors.py:245-249 — already 4-digit untouched.
    #[test]
    fn correct_year_2024_unchanged() {
        assert_eq!(correct_year(2024), 2024);
    }

    // -----------------------------------------------------------------------
    // try_swap_values
    // -----------------------------------------------------------------------

    /// Ports extractors.py:252-254 — month > 12 and day <= 12 triggers swap.
    #[test]
    fn try_swap_values_swaps_when_month_oversized() {
        // 03/15 = day=3, month=15 -> swap to day=15, month=3.
        assert_eq!(try_swap_values(3, 15), (15, 3));
    }

    /// Ports extractors.py:252-254 — when both within range, no swap.
    #[test]
    fn try_swap_values_no_swap_when_both_in_range() {
        assert_eq!(try_swap_values(5, 6), (5, 6));
    }

    /// Ports extractors.py:252-254 — month > 12 but day > 12 (e.g. 20/15):
    /// no swap (Python condition is `month > 12 AND day <= 12`).
    #[test]
    fn try_swap_values_no_swap_when_day_too_large() {
        assert_eq!(try_swap_values(20, 15), (20, 15));
    }

    // -----------------------------------------------------------------------
    // custom_parse
    // -----------------------------------------------------------------------

    /// Ports extractors.py:286-318 — ISO `YYYY-MM-DD` via fromisoformat.
    #[test]
    fn custom_parse_iso_date() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("2024-06-15", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:295-302 — 8-digit YYYYMMDD shortcut.
    #[test]
    fn custom_parse_eight_digit_yyyymmdd() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("20240615", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:313-318 — out-of-range date rejected.
    #[test]
    fn custom_parse_out_of_range_returns_none() {
        let min = dt(1995, 1, 1);
        let max = dt(2020, 12, 31);
        // 2050 is above the max — fails is_valid_date.
        let r = custom_parse("2050-06-15", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    /// Ports extractors.py:286-383 — fallback through YMD_PATTERN finds a
    /// date embedded in surrounding text.
    #[test]
    fn custom_parse_finds_ymd_in_text() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // Leading non-digit context for the `(?:\D|^)` anchor; the year-leading
        // shortcut would short-circuit before YMD_PATTERN scans, so use a
        // string that does NOT start with 4 digits.
        let r = custom_parse("Published 2024-06-15 today", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // external_date_parser (STUB)
    // -----------------------------------------------------------------------

    /// Ports extractors.py:386-396 — the stub returns None for every input.
    #[test]
    fn external_date_parser_stub_returns_none() {
        assert_eq!(external_date_parser("anything goes", "%Y-%m-%d"), None);
        assert_eq!(
            external_date_parser("15. Januar 2024", "%Y-%m-%d"),
            None
        );
        assert_eq!(external_date_parser("", "%Y-%m-%d"), None);
    }

    // -----------------------------------------------------------------------
    // regex_parse
    // -----------------------------------------------------------------------

    /// Ports extractors.py:257-283 — English "15 January 2024".
    #[test]
    fn regex_parse_english_long_form() {
        // The LONG_TEXT_PATTERN alternation accepts either the "month day,
        // year" arm or the "day month year" arm. English-style "15 January
        // 2024" matches the day-month-year arm (lastgroup="year2").
        let r = regex_parse("15 January 2024").expect("should match");
        assert_eq!((r.year, r.month, r.day), (2024, 1, 15));
    }

    /// Ports extractors.py:257-283 — German "15. Januar 2024".
    #[test]
    fn regex_parse_german_long_form() {
        let r = regex_parse("15. Januar 2024").expect("should match");
        assert_eq!((r.year, r.month, r.day), (2024, 1, 15));
    }

    /// Ports extractors.py:257-283 — French "15 janvier 2024".
    #[test]
    fn regex_parse_french_long_form() {
        let r = regex_parse("15 janvier 2024").expect("should match");
        assert_eq!((r.year, r.month, r.day), (2024, 1, 15));
    }

    /// Ports extractors.py:262-264 — no match returns None.
    #[test]
    fn regex_parse_no_match_returns_none() {
        assert_eq!(regex_parse("not a date string"), None);
    }

    // -----------------------------------------------------------------------
    // try_date_expr (orchestration)
    // -----------------------------------------------------------------------

    /// Ports extractors.py:422-425 — custom_parse hit short-circuits.
    #[test]
    fn try_date_expr_returns_via_custom_parse() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = try_date_expr(Some("2024-06-15"), "%Y-%m-%d", false, &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:408-409 — empty / None input returns None.
    #[test]
    fn try_date_expr_returns_none_for_empty_input() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        assert_eq!(
            try_date_expr(None, "%Y-%m-%d", false, &min, &max),
            None
        );
        assert_eq!(
            try_date_expr(Some(""), "%Y-%m-%d", false, &min, &max),
            None
        );
    }

    /// Ports extractors.py:415 — digit-count below 4 fails the gate.
    #[test]
    fn try_date_expr_rejects_under_four_digits() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // "ab12" has only 2 digits — under the 4..=18 window.
        let r = try_date_expr(Some("ab12"), "%Y-%m-%d", false, &min, &max);
        assert_eq!(r, None);
    }

    /// Ports extractors.py:419-420 — DISCARD_PATTERNS rejects clock-only
    /// strings ahead of custom_parse.
    #[test]
    fn try_date_expr_rejects_via_discard_patterns() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // "12:34" matches the clock-only arm of DISCARD_PATTERNS.
        let r = try_date_expr(Some("12:34"), "%Y-%m-%d", false, &min, &max);
        assert_eq!(r, None);
    }

    /// Ports extractors.py:431-435 — dateparser fallback (Rust STUB)
    /// returns None even when extensive_search=true.
    #[test]
    fn try_date_expr_stub_external_returns_none() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // A free-text date that bypasses custom_parse but matches
        // TEXT_DATE_PATTERN — Python would dispatch to dateparser; the Rust
        // stub returns None.
        let r = try_date_expr(
            Some("on Tuesday 15"),
            "%Y-%m-%d",
            true,
            &min,
            &max,
        );
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // img_search
    // -----------------------------------------------------------------------

    /// Ports extractors.py:440-451 — `<meta property="og:image">` with a
    /// dated URL is parsed.
    #[test]
    fn img_search_finds_og_image_url() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/img/2024/06/15/photo.jpg">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = img_search(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:440-451 — no og:image meta returns None.
    #[test]
    fn img_search_no_match_returns_none() {
        let html = "<html><head></head><body></body></html>";
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(img_search(&root, &o), None);
    }

    // -----------------------------------------------------------------------
    // pattern_search
    // -----------------------------------------------------------------------

    /// Ports extractors.py:454-466 — TIMESTAMP_PATTERN against a valid
    /// timestamp returns the parsed YYYY-MM-DD prefix.
    #[test]
    fn pattern_search_matches_timestamp_pattern() {
        use super::super::regex_catalogues::timestamp_pattern;
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = pattern_search("2024-06-15T12:34:56", timestamp_pattern(), &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // json_search
    // -----------------------------------------------------------------------

    /// Ports extractors.py:469-483 — JSON-LD `datePublished` extraction.
    #[test]
    fn json_search_finds_date_published() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "datePublished":"2024-06-15"}
            </script>
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        // original=true selects JSON_PUBLISHED.
        let o = Extractor::new(
            false,
            (2030, 12, 31),
            (1995, 1, 1),
            true,
            "%Y-%m-%d".into(),
        );
        let r = json_search(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:469-483 — JSON-LD `dateModified` extraction.
    #[test]
    fn json_search_finds_date_modified() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@context":"https://schema.org","@type":"Article",
             "dateModified":"2024-06-15"}
            </script>
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        // original=false selects JSON_MODIFIED.
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = json_search(&root, &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // idiosyncrasies_search
    // -----------------------------------------------------------------------

    /// Ports extractors.py:486-508 — German "Datum: DD.MM.YYYY" prefix.
    #[test]
    fn idiosyncrasies_search_matches_german_prefix() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = idiosyncrasies_search("Datum: 15.06.2024", &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports extractors.py:486-508 — no TEXT_PATTERN match returns None.
    #[test]
    fn idiosyncrasies_search_no_match_returns_none() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(
            idiosyncrasies_search("nothing date-like here", &o),
            None
        );
    }
}
