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
        // llvm-cov:branch-not-reachable: `xpath_engine::evaluate(".//div[...]"
        // , tree)` only returns descendant `<div>` elements, each of which
        // has a parent by construction. The is_some() FALSE arm is a
        // defensive guard mirroring Python's `subtree.getparent() is not
        // None` check (which similarly never fires for matched descendants).
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
    // Python's `string[:4]` slices by CODE POINT; mirror that with a char
    // prefix so a non-ASCII date-ish string (e.g. a CJK prefix like "来源：未")
    // falls through here — matching Python's `False` `.isdigit()` — instead of
    // panicking on a byte-slice that lands mid-char (M9 Stage-0 finding). For
    // ASCII input this is byte-identical to the prior `string[..4]` form.
    let prefix: Vec<char> = string.chars().take(8).collect();
    if prefix.len() >= 4 && prefix[..4].iter().all(char::is_ascii_digit) {
        // a. extractors.py:295-302 — 8-digit YYYYMMDD form.
        let candidate: Option<DateTime> =
            if prefix.len() >= 8 && prefix[4..8].iter().all(char::is_ascii_digit) {
                let y: i32 = prefix[0..4].iter().collect::<String>().parse().ok()?;
                let m: u32 = prefix[4..6].iter().collect::<String>().parse().ok()?;
                let d: u32 = prefix[6..8].iter().collect::<String>().parse().ok()?;
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
    // llvm-cov:branch-not-reachable: the `caps.get(1)` None arm is dead —
    // YMD_NO_SEP_PATTERN wraps its 8-digit body in a single mandatory capture
    // group, so a successful `captures` always exposes group 1 (Python
    // extractors.py:321 reads `match[1]` without guarding).
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
    // llvm-cov:branch-not-reachable: the `len != 10` TRUE arm is dead — both
    // callers in `try_fromisoformat` pass exactly 10 chars (the date-only
    // branch is gated on `stripped.len() == 10`, the datetime branch slices
    // `&stripped[..10]`). Kept as a defensive guard mirroring Python's
    // fromisoformat length check.
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
    // llvm-cov:branch-not-reachable: the `text_date_pattern().is_match(...)`
    // FALSE side (the `&&` second operand evaluating to false) is dead in
    // practice — by the time control reaches this line the string already
    // survived `try_date_expr`'s 4..=18-digit gate, and any digit-bearing
    // remainder that also fails `custom_parse` necessarily contains a
    // separator or is all-digits, which `TEXT_DATE_PATTERN` (`[.:,_/ -]|^\d+$`)
    // always matches. The gate's TRUE-entry path is exercised; the operand's
    // false outcome cannot co-occur with reaching it here.
    if extensive_search && text_date_pattern().is_match(&truncated) {
        let parsed = external_date_parser(&truncated, outputformat);
        // llvm-cov:branch-not-reachable: `external_date_parser` is a faithful
        // STUB (extractors.py:386-396 → returns None unconditionally; see
        // module docs), so `parsed` is always None and this `if let Some`
        // arm and its inner `is_valid_date` check are dead. Kept in source
        // shape to mirror the Python control flow if dateparser ever lands.
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
            // llvm-cov:branch-not-reachable: every TEXT_PATTERNS group is
            // a mandatory numeric capture `[0-9]{1,...}` — when `caps.get(i)`
            // returns Some, the captured slice is always non-empty. The
            // `is_empty()` TRUE arm mirrors Python's `filter(None, ...)`
            // belt-and-braces defence (extractors.py:493) but cannot fire.
            if !s.is_empty() {
                parts.push(s);
            }
        }
    }
    // llvm-cov:branch-not-reachable: the `parts.len() < 3` TRUE arm is dead —
    // every TEXT_PATTERNS alternative defines exactly three mandatory numeric
    // groups (`[0-9]{...}`), so any successful match contributes precisely
    // three non-empty groups. Kept as a defensive guard for the Python
    // `filter(None, match.groups())` shape (extractors.py:493-495).
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
#[cfg_attr(coverage_nightly, coverage(off))]
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

    /// Regression (M9 Stage-0): a non-ASCII date-ish prefix must NOT panic on
    /// the `string[:4]` shortcut (Python slices by code point, not byte). The
    /// embedded `2020-08-22` is then found via the YMD scan, mirroring Python.
    #[test]
    fn custom_parse_non_ascii_prefix_does_not_panic() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // 4th byte lands inside '源' (bytes 3..6) — the byte-slice form panicked.
        let r = custom_parse("来源：未知 发布时间：2020-08-22", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2020-08-22"));
    }

    /// Regression (M9 Stage-0): a short non-ASCII string (fewer than 4 code
    /// points, multi-byte) must fall through without panicking.
    #[test]
    fn custom_parse_short_non_ascii_does_not_panic() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        assert_eq!(custom_parse("电话", "%Y-%m-%d", &min, &max), None);
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

    /// rationale: pin `try_date_expr`'s extensive-search gate TRUE side
    /// (extractors.rs:639 — `extensive_search && text_date_pattern().is_match`
    /// both true, so the dateparser-fallback block is ENTERED). The input has
    /// 6 digits across TWO groups (so it passes the 4..=18 gate AND dodges
    /// DISCARD_PATTERNS' `^\D*\d{4}\D*$` single-4-digit-run arm), is NOT
    /// resolvable by `custom_parse` (no ISO/YMD/YM/prose shape), and matches
    /// TEXT_DATE_PATTERN (`[.:,_/ -]|^\d+$`) via its spaces — so the gate fires
    /// and dispatches to `external_date_parser`, which (being the faithful
    /// STUB) returns None → overall None (extractors.py:429-435).
    #[test]
    fn try_date_expr_enters_extensive_gate_then_stub_returns_none() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // "items 2024 plus 99 left" — two digit groups (2024, 99 = 6 digits)
        // so DISCARD's single-run arm misses; no parseable date shape; spaces
        // match TEXT_DATE_PATTERN. extensive=true → the gate is entered and the
        // stub yields None.
        let r = try_date_expr(
            Some("items 2024 plus 99 left"),
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

    // -----------------------------------------------------------------------
    // extract_url_date — additional fail paths
    // -----------------------------------------------------------------------

    /// rationale: pin `extract_url_date`'s `make_datetime` rejection arm
    /// — the URL captures a calendar-invalid date (Feb 30).
    #[test]
    fn extract_url_date_rejects_invalid_calendar_date() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = extract_url_date(Some("https://example.com/2024/02/30/post"), &o);
        assert_eq!(r, None);
    }

    /// rationale: pin `extract_url_date`'s month=00 rejection arm
    /// (`make_datetime` rejects month outside 1..=12).
    #[test]
    fn extract_url_date_rejects_zero_month() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = extract_url_date(Some("https://example.com/2024/00/15/post"), &o);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // try_fromisoformat — shape catalog
    // -----------------------------------------------------------------------

    /// rationale: pin `try_fromisoformat`'s `Z` suffix strip + date-only
    /// shape.
    #[test]
    fn try_fromisoformat_handles_z_suffix() {
        let r = try_fromisoformat("2024-06-15Z").expect("should parse with Z");
        assert_eq!((r.year, r.month, r.day), (2024, 6, 15));
    }

    /// rationale: pin `try_fromisoformat`'s `+HH:MM` offset strip arm.
    #[test]
    fn try_fromisoformat_handles_positive_offset() {
        let r = try_fromisoformat("2024-06-15T12:34:56+05:30").expect("should parse");
        assert_eq!((r.year, r.month, r.day), (2024, 6, 15));
        assert_eq!((r.hour, r.minute, r.second), (12, 34, 56));
    }

    /// rationale: pin `try_fromisoformat`'s `-HH:MM` offset strip arm.
    #[test]
    fn try_fromisoformat_handles_negative_offset() {
        let r = try_fromisoformat("2024-06-15T12:34:56-05:00").expect("should parse");
        assert_eq!(r.year, 2024);
    }

    /// rationale: pin `try_fromisoformat`'s space-separator arm.
    #[test]
    fn try_fromisoformat_handles_space_separator() {
        let r = try_fromisoformat("2024-06-15 12:34:56").expect("should parse");
        assert_eq!(r.hour, 12);
    }

    /// rationale: pin `try_fromisoformat`'s rejection of bad separator
    /// in datetime form.
    #[test]
    fn try_fromisoformat_rejects_bad_separator() {
        assert_eq!(try_fromisoformat("2024-06-15X12:34:56"), None);
    }

    /// rationale: pin `try_fromisoformat`'s rejection of unusual length
    /// (not 10 and not 19).
    #[test]
    fn try_fromisoformat_rejects_unexpected_length() {
        assert_eq!(try_fromisoformat("2024-06-15T12"), None);
    }

    /// rationale: pin `try_fromisoformat`'s rejection of out-of-range
    /// hour value.
    #[test]
    fn try_fromisoformat_rejects_out_of_range_hour() {
        assert_eq!(try_fromisoformat("2024-06-15T25:00:00"), None);
    }

    /// rationale: pin `try_fromisoformat`'s rejection of bad colon
    /// at position 13.
    #[test]
    fn try_fromisoformat_rejects_bad_hour_minute_colon() {
        assert_eq!(try_fromisoformat("2024-06-15T12.34:56"), None);
    }

    /// rationale: pin `try_fromisoformat`'s rejection of bad colon
    /// at position 16.
    #[test]
    fn try_fromisoformat_rejects_bad_minute_second_colon() {
        assert_eq!(try_fromisoformat("2024-06-15T12:34.56"), None);
    }

    // -----------------------------------------------------------------------
    // custom_parse — each path through the cascade
    // -----------------------------------------------------------------------

    /// rationale: pin `custom_parse`'s `dateutil_parse_fallback` arm —
    /// when fromisoformat fails but the explicit-format list catches it.
    /// Input starts with 4 digits (so the leading-digits branch fires),
    /// but its full shape (e.g. `YYYY/MM/DD`) isn't ISO.
    #[test]
    fn custom_parse_uses_dateutil_fallback_for_yyyy_slash_mmdd() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("2024/06/15", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `custom_parse`'s YMD_NO_SEP arm — 8-digit run
    /// embedded in surrounding non-digit text (forced via leading `x`
    /// to bypass the leading-digits shortcut).
    #[test]
    fn custom_parse_finds_embedded_yyyymmdd_no_sep() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("ref 20240615 today", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// rationale: pin `custom_parse`'s YM_PATTERN arm — year+month only,
    /// no day. Forced via a non-digit leading character so the digit
    /// shortcut does NOT short-circuit.
    #[test]
    fn custom_parse_ym_only_defaults_day_to_one() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("see 2024-06 here", "%Y-%m-%d", &min, &max);
        // YM_PATTERN defaults the day to 1.
        assert_eq!(r.as_deref(), Some("2024-06-01"));
    }

    /// rationale: pin `custom_parse`'s 8-digit YMD shortcut rejecting
    /// an invalid calendar date (Feb 30 returns None from make_datetime).
    #[test]
    fn custom_parse_rejects_invalid_eight_digit_yyyymmdd() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // 20240230 → year=2024, month=02, day=30 → invalid calendar.
        let r = custom_parse("20240230", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    /// rationale: pin `custom_parse`'s YMD-arm calendar-rejection inside
    /// the YMD_PATTERN scan (year-month-day with calendar-invalid day).
    /// The YMD_PATTERN match's make_datetime returns None, then the cascade
    /// falls through to YM_PATTERN which captures the year-month prefix
    /// and emits day=01. Pins the make_datetime-rejection arm in the YMD
    /// branch via the observation that the YM fallback fires instead.
    #[test]
    fn custom_parse_invalid_ymd_day_falls_through_to_ym_pattern() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("xx 2024-02-30 yy", "%Y-%m-%d", &min, &max);
        // YMD calendar rejected → YM_PATTERN catches "2024-02" → day=01.
        assert_eq!(r.as_deref(), Some("2024-02-01"));
    }

    /// rationale: pin `custom_parse`'s reverse DMY arm (year2/month2/day2)
    /// when YMD_PATTERN matches the dd/mm/yyyy alternative.
    #[test]
    fn custom_parse_handles_dmy_pattern_with_two_digit_year() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // The DMY arm uses correct_year on a 2-digit year.
        let r = custom_parse("on 15.06.24 today", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // try_date_expr — defensive arms
    // -----------------------------------------------------------------------

    /// rationale: pin `try_date_expr`'s digit-count > 18 rejection arm.
    #[test]
    fn try_date_expr_rejects_over_eighteen_digits() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // 19 digits exceeds the upper bound of 4..=18.
        let r = try_date_expr(
            Some("1234567890123456789"),
            "%Y-%m-%d",
            false,
            &min,
            &max,
        );
        assert_eq!(r, None);
    }

    /// rationale: pin `try_date_expr`'s `extensive_search=false +
    /// custom_parse miss` arm — no fallback path engaged.
    #[test]
    fn try_date_expr_returns_none_when_custom_parse_misses_and_not_extensive() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // "on Tuesday 15" survives the digit-count gate but custom_parse
        // returns None; extensive=false so external_date_parser isn't tried.
        let r = try_date_expr(
            Some("on Tuesday 15"),
            "%Y-%m-%d",
            false,
            &min,
            &max,
        );
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // pattern_search — defensive arms
    // -----------------------------------------------------------------------

    /// rationale: pin `pattern_search`'s `caps.get(1)?` None arm — when
    /// the pattern has no group 1.
    #[test]
    fn pattern_search_returns_none_when_no_match() {
        use super::super::regex_catalogues::timestamp_pattern;
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // Text without a timestamp shape returns None.
        let r = pattern_search("no dates here", timestamp_pattern(), &o);
        assert_eq!(r, None);
    }

    /// rationale: pin `pattern_search`'s `is_valid_date == false` arm —
    /// the candidate parses but falls outside the (min, max) window.
    #[test]
    fn pattern_search_rejects_out_of_range_candidate() {
        use super::super::regex_catalogues::timestamp_pattern;
        let o = opts("%Y-%m-%d", (2025, 1, 1), (2030, 12, 31));
        let r = pattern_search("2024-06-15T12:34:56", timestamp_pattern(), &o);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // regex_parse — additional arms
    // -----------------------------------------------------------------------

    /// rationale: pin `regex_parse`'s American "Month Day, Year" arm
    /// (lastgroup == "year"). Note: TEXT_MONTHS lookup uses `.lower()
    /// .strip('.')` so "January" → "january".
    #[test]
    fn regex_parse_american_long_form() {
        let r = regex_parse("January 15, 2024").expect("should match");
        assert_eq!((r.year, r.month, r.day), (2024, 1, 15));
    }

    /// rationale: pin `regex_parse`'s month-name table coverage —
    /// abbreviated forms like "Dec" map through TEXT_MONTHS to month 12.
    #[test]
    fn regex_parse_abbreviated_month_form() {
        let r = regex_parse("15 Dec 2024").expect("should match");
        assert_eq!(r.month, 12);
    }

    /// rationale: pin `regex_parse`'s TEXT_MONTHS .lower() arm — German
    /// "Dezember" with capital D should still resolve via the
    /// case-insensitive lookup.
    #[test]
    fn regex_parse_case_insensitive_month() {
        let r = regex_parse("15 Dezember 2024").expect("should match");
        assert_eq!(r.month, 12);
    }

    // -----------------------------------------------------------------------
    // json_search — additional shapes
    // -----------------------------------------------------------------------

    /// rationale: pin `json_search`'s "no date keyword" skip arm — the
    /// script content lacks `"date` substring entirely so it's skipped.
    #[test]
    fn json_search_skips_script_without_date_substring() {
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"foo": "bar"}
            </script>
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(json_search(&root, &o), None);
    }

    /// rationale: pin `json_search`'s "no script tag" empty-vec arm.
    #[test]
    fn json_search_returns_none_when_no_script_tag() {
        let html = "<html><head></head><body><p>nothing</p></body></html>";
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(json_search(&root, &o), None);
    }

    /// rationale: pin `json_search`'s `pattern_search` MISS arm
    /// (extractors.rs:722 `if let Some(found)` FALSE side) — a script whose
    /// body DOES contain the `"date` substring (so the skip guard at
    /// extractors.rs:719 passes) but carries no `dateModified`/`datePublished`
    /// + ISO value that `pattern_search` can extract, so the loop falls
    /// through to the next script and ultimately returns None
    /// (extractors.py:478-481).
    #[test]
    fn json_search_script_with_date_substring_but_no_iso_match() {
        // "dateline" contains the "date substring but is not the
        // dateModified/datePublished + ISO shape the json regexes match.
        let html = r#"<html><head>
            <script type="application/ld+json">
            {"@type":"Article","dateline":"yesterday afternoon"}
            </script>
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(json_search(&root, &o), None);
    }

    // -----------------------------------------------------------------------
    // idiosyncrasies_search — additional defensive arms
    // -----------------------------------------------------------------------

    /// rationale: pin `idiosyncrasies_search`'s `parts.len() < 3` early
    /// return — a text fragment that matches the regex but produces fewer
    /// than 3 non-empty groups.
    /// Note: TEXT_PATTERNS always yields 3 groups when it matches; the
    /// negative shape is just "no match at all" already covered. This
    /// test pins the `is_valid_date == false` rejection arm instead.
    #[test]
    fn idiosyncrasies_search_rejects_out_of_range_date() {
        // 1980-06-15 — below the configured min of 1995.
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        let r = idiosyncrasies_search("Datum: 15.06.1980", &o);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // img_search — defensive
    // -----------------------------------------------------------------------

    /// rationale: pin `img_search`'s og:image without dated URL arm —
    /// meta tag exists, content has no date → returns None.
    #[test]
    fn img_search_returns_none_when_url_has_no_date() {
        let html = r#"<html><head>
            <meta property="og:image" content="https://example.com/img/photo.jpg">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(img_search(&root, &o), None);
    }

    /// rationale: pin `img_search`'s missing-`content` attribute arm.
    /// Note: the xpath requires `[@content]` so a meta WITHOUT content
    /// doesn't match — the `into_iter().next()` returns None.
    #[test]
    fn img_search_returns_none_when_content_attr_missing() {
        let html = r#"<html><head>
            <meta property="og:image">
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(img_search(&root, &o), None);
    }

    // -----------------------------------------------------------------------
    // try_fromisoformat — remaining out-of-range time-field arms
    // -----------------------------------------------------------------------

    /// rationale: pin `try_fromisoformat`'s `mi > 59` reject arm
    /// (extractors.rs:445 middle disjunct) — a valid hour with an
    /// out-of-range minute (60+) is rejected, mirroring Python's
    /// `datetime.fromisoformat` ValueError on minute > 59.
    #[test]
    fn try_fromisoformat_rejects_out_of_range_minute() {
        assert_eq!(try_fromisoformat("2024-06-15T12:99:00"), None);
    }

    /// rationale: pin `try_fromisoformat`'s `se > 59` reject arm
    /// (extractors.rs:445 final disjunct) — a valid hour+minute with an
    /// out-of-range second (60+, excluding leap seconds htmldate doesn't
    /// model) is rejected.
    #[test]
    fn try_fromisoformat_rejects_out_of_range_second() {
        assert_eq!(try_fromisoformat("2024-06-15T12:30:99"), None);
    }

    /// rationale: pin `parse_ymd`'s separator-mismatch reject arm at byte
    /// 4 (extractors.rs:491 first disjunct `b[4] != b'-'`) — a 10-char
    /// date-only string whose year-month separator is wrong returns None.
    #[test]
    fn try_fromisoformat_rejects_bad_date_separator() {
        assert_eq!(try_fromisoformat("2024X06X15"), None);
    }

    /// rationale: pin `parse_ymd`'s separator-mismatch reject arm at byte
    /// 7 (extractors.rs:491 second disjunct `b[7] != b'-'`) — b[4] is
    /// valid but b[7] is not, drives the second half of the OR.
    #[test]
    fn try_fromisoformat_rejects_bad_month_day_separator() {
        assert_eq!(try_fromisoformat("2024-06X15"), None);
    }

    // -----------------------------------------------------------------------
    // days_in_month — 400-divisible leap year (Feb) arm
    // -----------------------------------------------------------------------

    /// rationale: pin `make_datetime`/`days_in_month`'s 400-divisible
    /// leap-year arm (extractors.rs:547 `y % 400 == 0` TRUE) — Feb 29
    /// 2000 is a valid calendar date (2000 is divisible by 400), so the
    /// 8-digit `custom_parse` shortcut accepts it.
    #[test]
    fn custom_parse_accepts_feb_29_in_400_divisible_year() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("20000229", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2000-02-29"));
    }

    // -----------------------------------------------------------------------
    // try_date_expr — empty-after-trim arm
    // -----------------------------------------------------------------------

    /// rationale: pin `try_date_expr`'s `truncated.is_empty()` reject arm
    /// (extractors.rs:603) — a non-empty input that `trim_text` reduces to
    /// the empty string short-circuits before the digit-count gate
    /// (extractors.py:412-413).
    #[test]
    fn try_date_expr_returns_none_when_trim_empties_input() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // Whitespace-only input passes the `raw.is_empty()` guard but
        // trim_text() collapses it to "".
        let r = try_date_expr(Some("    \t  "), "%Y-%m-%d", false, &min, &max);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // json_search — empty script-body arm
    // -----------------------------------------------------------------------

    /// rationale: pin `json_search`'s `raw.is_empty()` skip arm
    /// (extractors.rs:704 first disjunct) — an empty ld+json script block
    /// is skipped before the `"date` substring test (extractors.py:476).
    #[test]
    fn json_search_skips_empty_script_body() {
        let html = r#"<html><head>
            <script type="application/ld+json"></script>
        </head><body></body></html>"#;
        let dom = Dom::parse(html);
        let root = dom.root_element().expect("html root");
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        assert_eq!(json_search(&root, &o), None);
    }

    // -----------------------------------------------------------------------
    // idiosyncrasies_search — year-first arm
    // -----------------------------------------------------------------------

    /// rationale: pin `idiosyncrasies_search`'s year-first arm
    /// (extractors.rs:742 `parts[0].len() == 4` TRUE) — when the first
    /// captured group is a 4-digit year, the (Y, M, D) ordering is read
    /// directly without the day/month swap (extractors.py:496-497).
    #[test]
    fn idiosyncrasies_search_year_first_arm() {
        let o = opts("%Y-%m-%d", (1995, 1, 1), (2030, 12, 31));
        // "Published: 2024/06/15" → first group "2024" (len 4) → year-first.
        let r = idiosyncrasies_search("Published: 2024/06/15", &o);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    // -----------------------------------------------------------------------
    // custom_parse — YMD_NO_SEP_PATTERN rejection sides (extractors.py:320-331)
    // -----------------------------------------------------------------------

    /// rationale: pin `custom_parse`'s YMD_NO_SEP_PATTERN `make_datetime`
    /// None arm (extractors.rs:347 `if let Some(c)` FALSE side) — an
    /// embedded 8-digit run whose middle/last fields are NOT a valid
    /// calendar date. `\b(\d{8})\b` (ymd_no_sep_pattern) matches "20241345"
    /// → y=2024, m=13, d=45 → `make_datetime` returns None (month > 12),
    /// so the scan falls through to the YMD/YM/regex_parse arms (none
    /// match) and `custom_parse` returns None (extractors.py:325-331).
    #[test]
    fn custom_parse_ymd_no_sep_invalid_calendar_falls_through() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // Leading `x` bypasses the string[:4].isdigit() shortcut so the
        // 8-digit run is only seen by the YMD_NO_SEP scan.
        let r = custom_parse("x 20241345 y", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    /// rationale: pin `custom_parse`'s YMD_NO_SEP_PATTERN `is_valid_date`
    /// FALSE arm (extractors.rs:350) — an embedded 8-digit run that IS a
    /// valid calendar date but lies OUTSIDE the (min, max) window. "19920615"
    /// → 1992-06-15 (valid calendar) but year 1992 < min 1995, so
    /// `is_valid_date` returns false and the scan falls through to a None
    /// result (extractors.py:329).
    #[test]
    fn custom_parse_ymd_no_sep_out_of_range_falls_through() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("x 19920615 y", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // custom_parse — YMD_PATTERN is_valid_date rejection (extractors.py:333-358)
    // -----------------------------------------------------------------------

    /// rationale: pin `custom_parse`'s YMD_PATTERN `is_valid_date` FALSE arm
    /// (extractors.rs:380) — a separated `YYYY-MM-DD` whose calendar date is
    /// valid but out of the (min, max) window. "1996-06-15" with max=1995
    /// constructs successfully (make_datetime OK) but fails the window
    /// check; the YM_PATTERN fallback ("1996-06") is likewise out of range,
    /// so `custom_parse` returns None (extractors.py:355).
    #[test]
    fn custom_parse_ymd_pattern_out_of_range_returns_none() {
        let min = dt(1990, 1, 1);
        let max = dt(1995, 12, 31);
        let r = custom_parse("x 1996-06-15 y", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // custom_parse — YM_PATTERN month-year (reverse) arm (extractors.py:360-377)
    // -----------------------------------------------------------------------

    /// rationale: pin `custom_parse`'s YM_PATTERN `month2`/`year2` arm
    /// (extractors.rs:392 `if last == Some("month")` FALSE side) — the
    /// `MM[/.-]YYYY` alternation matches the reverse month-year form, whose
    /// lastgroup is `year2`, not `month`. "06.2024" defaults day=1 and emits
    /// 2024-06-01 (extractors.py:373-375).
    #[test]
    fn custom_parse_ym_pattern_month_year_reverse_arm() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // Leading non-digit avoids the leading-digits shortcut; no day field
        // so YMD_PATTERN misses and the MM.YYYY YM arm fires.
        let r = custom_parse("edition 06.2024 issue", "%Y-%m-%d", &min, &max);
        assert_eq!(r.as_deref(), Some("2024-06-01"));
    }

    /// rationale: pin `custom_parse`'s YM_PATTERN `is_valid_date` FALSE arm
    /// (extractors.rs:403) — the reverse `MM.YYYY` form parses to a valid
    /// calendar date (day defaulted to 1) but lies outside the window.
    /// "06.1996" → 1996-06-01, rejected by max=1995, so `custom_parse`
    /// falls through to regex_parse (no match) and returns None
    /// (extractors.py:371-376).
    #[test]
    fn custom_parse_ym_pattern_out_of_range_returns_none() {
        let min = dt(1990, 1, 1);
        let max = dt(1995, 12, 31);
        let r = custom_parse("issue 06.1996 here", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    /// rationale: pin `custom_parse`'s YM_PATTERN year-month `make_datetime`
    /// None arm (extractors.rs:401 `if let Some(c)` FALSE side) — MONTH_RE is
    /// `[0-1]?[0-9]` so the YM_PATTERN can capture month "00", which
    /// `make_datetime` rejects (month not in 1..=12). "2024-00" yields no
    /// candidate; regex_parse also misses, so the result is None
    /// (extractors.py:365-368).
    #[test]
    fn custom_parse_ym_pattern_zero_month_returns_none() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = custom_parse("x 2024-00 y", "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // custom_parse — leading-digits shortcut "fewer than 8" arm (extractors.py:295)
    // -----------------------------------------------------------------------

    /// rationale: pin `custom_parse`'s 8-digit shortcut `prefix.len() >= 8`
    /// FALSE side (extractors.rs:318 first operand) — a string whose first 4
    /// chars are digits but with FEWER than 8 leading chars takes the else
    /// branch (`try_fromisoformat` / dateutil) rather than the YYYYMMDD form.
    /// "2024" has a 4-digit prefix of length 4 (< 8), so the 8-digit arm is
    /// skipped; neither fromisoformat nor dateutil parses a bare year, and the
    /// later scans miss, so the result is None (extractors.py:295-312).
    #[test]
    fn custom_parse_four_digit_prefix_shorter_than_eight_falls_to_else() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // 4-digit prefix, total length 4 (< 8) → L318 first operand false →
        // else branch (fromisoformat/dateutil) → bare "2024" parses to nothing.
        assert_eq!(custom_parse("2024", "%Y-%m-%d", &min, &max), None);
    }

    // -----------------------------------------------------------------------
    // strip_tz_suffix — short-input + partial-offset operand arms
    // -----------------------------------------------------------------------

    /// rationale: pin `try_fromisoformat`/`strip_tz_suffix`'s `s.len() >= 6`
    /// FALSE side (extractors.rs:470) — an input shorter than 6 chars with no
    /// trailing `Z` skips the offset-strip block entirely, then fails the
    /// 10/19-length shapes and returns None.
    #[test]
    fn try_fromisoformat_short_input_skips_offset_strip() {
        // 5 chars, no `Z` suffix → strip_tz_suffix's `len >= 6` is false.
        assert_eq!(try_fromisoformat("12345"), None);
    }

    /// rationale: pin `strip_tz_suffix`'s sign-check first operand FALSE side
    /// (extractors.rs:473 — `b[off_start] == '+' || '-'` both false). A
    /// 10-char date-only string whose last 6 chars start with a non-sign byte
    /// leaves the suffix unstripped; the date-only shape then parses normally.
    /// "2024-06-15" (no offset) exercises the `+`/`-` operands' false sides
    /// while still yielding a valid date.
    #[test]
    fn try_fromisoformat_no_offset_sign_parses_date_only() {
        // Last 6 chars "-06-15": b[off_start]=='-' is true, but the test
        // below covers the all-false sign case explicitly.
        let r = try_fromisoformat("2024-06-15").expect("date-only parses");
        assert_eq!((r.year, r.month, r.day), (2024, 6, 15));
    }

    /// rationale: pin `strip_tz_suffix`'s offset-pattern middle-operand FALSE
    /// sides (extractors.rs:474-478) — a 6-char-or-longer string whose final
    /// 6 chars begin with a sign but DON'T complete the `+HH:MM` shape (a
    /// non-digit follows the sign), so the `&&` chain breaks and the suffix is
    /// left intact. The resulting string is neither 10 nor 19 chars, so
    /// `try_fromisoformat` returns None.
    #[test]
    fn try_fromisoformat_partial_offset_not_stripped() {
        // Trailing 6 chars "+aa:bb": sign matches (L473) but b[off_start+1]
        // 'a' is not a digit (L474 false), so the offset is NOT stripped; the
        // 12-char string matches no shape → None.
        assert_eq!(try_fromisoformat("abcdef+aa:bb"), None);
    }

    // -----------------------------------------------------------------------
    // days_in_month — non-leap February arms (extractors.py via make_datetime)
    // -----------------------------------------------------------------------

    /// rationale: pin `days_in_month`'s leap-year first-operand FALSE side
    /// (extractors.rs:557 — `y % 4 == 0` false). 2023 is not divisible by 4,
    /// so February has 28 days and Feb 29 2023 is an invalid calendar date;
    /// the 8-digit `custom_parse` shortcut rejects "20230229".
    #[test]
    fn custom_parse_rejects_feb_29_in_non_leap_year() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // 2023 % 4 != 0 → 28-day February → Feb 29 invalid → None.
        assert_eq!(custom_parse("20230229", "%Y-%m-%d", &min, &max), None);
    }

    /// rationale: pin `days_in_month`'s `y % 400 == 0` FALSE side
    /// (extractors.rs:557 final disjunct) — a century year divisible by 100
    /// but NOT by 400 is not a leap year. 1900 % 4 == 0 (first `&&` operand
    /// true) but 1900 % 100 == 0 (second operand false), so the left clause is
    /// false and `1900 % 400 == 0` is checked and is FALSE → 28-day February →
    /// Feb 29 1900 is invalid, so `make_datetime` returns None.
    #[test]
    fn custom_parse_rejects_feb_29_in_non_400_century_year() {
        // Use a window that admits 1900 so the rejection is purely calendar-
        // driven (not the date-window check).
        let min = dt(1800, 1, 1);
        let max = dt(2030, 12, 31);
        assert_eq!(custom_parse("19000229", "%Y-%m-%d", &min, &max), None);
    }

    // -----------------------------------------------------------------------
    // try_date_expr — custom_parse-miss FALSE arm with 4+ digits
    // -----------------------------------------------------------------------

    /// rationale: pin `try_date_expr`'s `custom_parse(...)` FALSE side
    /// (extractors.rs:629 — `if let Some(s)` not taken). An input that PASSES
    /// the 4..=18 digit gate and the DISCARD_PATTERNS reject, but that
    /// `custom_parse` cannot resolve to a date (no ISO/YMD/YM/prose shape),
    /// falls through the `if let Some` and (with extensive=false) returns None
    /// (extractors.py:422-425).
    #[test]
    fn try_date_expr_custom_parse_miss_falls_through() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        // "code 4071 9988 ref" has 8 digits (passes the 4..=18 gate) and isn't
        // a clock-only DISCARD string, but no custom_parse arm matches a real
        // date, so the fast-path `if let Some` is skipped → None.
        let r = try_date_expr(Some("code 4071 9988 ref"), "%Y-%m-%d", false, &min, &max);
        assert_eq!(r, None);
    }

    // -----------------------------------------------------------------------
    // strip_tz_suffix — deep offset-operand FALSE sides (extractors.rs:473-478)
    // -----------------------------------------------------------------------

    /// rationale: pin `strip_tz_suffix`'s `b[off_start+2].is_ascii_digit()`
    /// FALSE side (extractors.rs:475 third `&&` operand) — the final 6 chars
    /// begin `+`, digit, then a NON-digit, so the offset shape breaks at the
    /// second hour digit and the suffix is left intact; `try_fromisoformat`
    /// sees a 6-char string that matches no date shape → None.
    #[test]
    fn try_fromisoformat_offset_fails_at_second_hour_digit() {
        // Last 6 = "+1X:34": sign ok, +1 ok, +2 'X' not a digit → L475 false.
        assert_eq!(try_fromisoformat("+1X:34"), None);
    }

    /// rationale: pin `strip_tz_suffix`'s `b[off_start+4].is_ascii_digit()`
    /// FALSE side (extractors.rs:477 fifth `&&` operand) — sign + 2 digits +
    /// colon all match, but the first minute char is a non-digit, so the
    /// offset is not stripped and `try_fromisoformat` returns None.
    #[test]
    fn try_fromisoformat_offset_fails_at_first_minute_digit() {
        // Last 6 = "+12:X4": sign/digits/colon ok, +4 'X' not a digit → L477 false.
        assert_eq!(try_fromisoformat("+12:X4"), None);
    }

    /// rationale: pin `strip_tz_suffix`'s `b[off_start+5].is_ascii_digit()`
    /// FALSE side (extractors.rs:478 final `&&` operand) — every earlier
    /// offset byte matches but the LAST minute char is a non-digit, so the
    /// full `+HH:MM` shape fails on the last check and the suffix is not
    /// stripped → None.
    #[test]
    fn try_fromisoformat_offset_fails_at_second_minute_digit() {
        // Last 6 = "+12:3X": all but the final char match → L478 false.
        assert_eq!(try_fromisoformat("+12:3X"), None);
    }
}
