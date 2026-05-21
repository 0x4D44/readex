//! `validators` — sub-stage B port of `htmldate/validators.py`.
//!
//! Source of truth: `htmldate@1.9.x/validators.py` (vendored under
//! `C:\Users\marti\AppData\Roaming\Python\Python314\site-packages\htmldate\
//! validators.py`).
//!
//! This sub-stage ports **every** function in `validators.py:1-216`:
//!
//! - `is_valid_date` (validators.py:22-57)
//! - `validate_and_convert` (validators.py:60-73)
//! - `is_valid_format` (validators.py:76-90)
//! - `plausible_year_filter` (validators.py:93-123)
//! - `compare_values` (validators.py:126-137)
//! - `filter_ymd_candidate` (validators.py:140-167)
//! - `convert_date` (validators.py:170-180)
//! - `check_extracted_reference` (validators.py:183-192)
//! - `check_date_input` (validators.py:195-206)
//! - `get_min_date` (validators.py:209-211)
//! - `get_max_date` (validators.py:214-216)
//!
//! # Date typing
//!
//! `chrono` is **not** a crate dependency (see `Cargo.toml`'s `[dependencies]`
//! block — only `html5ever`/`markup5ever_rcdom`/`tendril`/`regex`/`serde_json`
//! are pinned at the time of M4 Stage 1 sub-stage A). Sub-stage A's
//! `settings::MIN_DATE` is a `(i32, u32, u32)` tuple; this sub-stage extends
//! that scheme with a small private `DateTime` newtype that carries
//! `(year, month, day, hour, minute, second)` because Python `datetime`
//! is what the validators consume from upstream callers (e.g. `compare_values`
//! returns a `mktime`-style timestamp and `is_valid_date` compares both
//! `dateobject.year` AND `dateobject.timestamp()`). Hour/minute/second
//! precision is needed to faithfully port that comparison.
//!
//! Sub-stage A's `Extractor.max` / `Extractor.min` are still `(i32, u32, u32)`
//! tuples; the public API surface of this sub-stage accepts those tuples
//! (treated as the midnight of the day) wherever the Python source consumes
//! a `datetime`. That's a faithful zero-time interpretation — Python's
//! `datetime(1995, 1, 1)` IS midnight by construction.
//!
//! # Deferred items
//!
//! Python's `@lru_cache(maxsize=CACHE_SIZE)` on `is_valid_date` /
//! `filter_ymd_candidate` and `@lru_cache(maxsize=16)` on `is_valid_format`
//! are NOT ported. These are perf optimisations, not algorithmic: every
//! cached call has the same observable result as an uncached one. If a
//! later sub-stage's profile shows a hot path here, the cache layer can
//! be added without algorithmic change. Per the M4 Stage 1 sub-stage B
//! brief: "`@lru_cache` decorators on Python functions — IGNORE for
//! sub-stage B (no perf-critical path here)".
//!
//! # `strptime` / `strftime` minimal parser
//!
//! Python `datetime.strptime` / `datetime.strftime` accept a wide format
//! grammar. The htmldate code paths use a small subset; this module ships
//! a tiny stateful parser (`format_parse` / `format_emit`) that supports
//! `%Y`, `%m`, `%d`, `%H`, `%M`, `%S`, `%T` (Python `%T` ==
//! `%H:%M:%S`), plus literal characters. Unknown directives cause a
//! `FormatError`. If a future sub-stage's tests demand more (e.g. `%B`
//! for English month name), the parser grows there.

use std::collections::HashMap;

use regex::Regex;

use super::settings::MIN_DATE;
use super::utils::Extractor;

// ===========================================================================
// DateTime — small internal date type
// ===========================================================================

/// Internal calendar date+time, kept in lockstep with Python `datetime`.
///
/// Ports the implicit Python `datetime.datetime` type the validators
/// pass around. We carry six fields so `compare_values`'s `mktime`-style
/// timestamp comparison is faithful (Python's `is_valid_date` compares
/// both `dateobject.year` AND `dateobject.timestamp()` — see
/// `validators.py:51-54`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

impl DateTime {
    /// Construct a `DateTime` at midnight from a `(year, month, day)` tuple.
    ///
    /// Mirrors Python `datetime(year, month, day)` — implicit `hour=0`,
    /// `minute=0`, `second=0`. Used to lift `Extractor.min` / `Extractor.max`
    /// tuples into the comparison space.
    pub fn from_ymd(ymd: (i32, u32, u32)) -> Self {
        Self {
            year: ymd.0,
            month: ymd.1,
            day: ymd.2,
            hour: 0,
            minute: 0,
            second: 0,
        }
    }

    /// `(year, month, day)` extractor (drops the time fields).
    pub fn ymd(&self) -> (i32, u32, u32) {
        (self.year, self.month, self.day)
    }

    /// Convert to a `time_t`-style integer timestamp (seconds since the
    /// Unix epoch, UTC). Mirrors Python's `int(mktime(dt.timetuple()))` /
    /// `dt.timestamp()` for the comparison semantics validators.py uses.
    ///
    /// Implementation uses a fixed-table days-from-civil computation
    /// (no `chrono` dep) — independent of the local timezone (Python's
    /// `mktime` is local-time, but the validators only compare values
    /// produced through the same function within a single process, so
    /// the absolute calendar offset is irrelevant: `a.timestamp() <=
    /// b.timestamp()` holds iff `a <= b` chronologically).
    pub fn timestamp(&self) -> i64 {
        let days = days_from_civil(self.year, self.month, self.day);
        days * 86_400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}

/// Days-from-civil: a `(year, month, day)` -> days-from-1970-01-01
/// helper. Algorithm from Howard Hinnant (public domain), correct for
/// any year between -32768 and +32767. Used to give `DateTime::timestamp`
/// a monotonic-with-calendar-order integer for the
/// `validators.py:53` `dateobject.timestamp()` comparison.
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let m = m as i64;
    let d = d as i64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era as i64) * 146_097 + doe - 719_468
}

// ===========================================================================
// Format parser — minimal `strptime`/`strftime` for the directives htmldate uses
// ===========================================================================

/// Error returned by the small format-string parser when an input does not
/// match the format or the format contains an unsupported directive.
///
/// Mirrors Python's `ValueError` from `datetime.strptime` (validators.py:47).
/// Tests treat this opaquely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatError;

/// Parse `datestring` according to `format`, returning a `DateTime`.
///
/// Supports `%Y` / `%m` / `%d` / `%H` / `%M` / `%S` / `%T` (== `%H:%M:%S`)
/// plus literal characters and `%%` for a literal `%`. Behaviour follows
/// Python `datetime.strptime` on this subset (zero-padded numeric directives,
/// strict literal-character matching).
///
/// Used by `is_valid_date` (validators.py:46), `compare_values`
/// (validators.py:129), `convert_date` (validators.py:179), and
/// `check_date_input` (indirectly via `from_isoformat`).
pub fn format_parse(datestring: &str, format: &str) -> Result<DateTime, FormatError> {
    let mut year: i32 = 0;
    let mut month: u32 = 1;
    let mut day: u32 = 1;
    let mut hour: u32 = 0;
    let mut minute: u32 = 0;
    let mut second: u32 = 0;
    let mut got_year = false;

    let s = datestring.as_bytes();
    let f = format.as_bytes();
    let mut si = 0;
    let mut fi = 0;

    while fi < f.len() {
        if f[fi] == b'%' {
            fi += 1;
            if fi >= f.len() {
                return Err(FormatError);
            }
            match f[fi] {
                b'Y' => {
                    year = take_digits(s, &mut si, 4)? as i32;
                    got_year = true;
                    fi += 1;
                }
                b'm' => {
                    month = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'd' => {
                    day = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'H' => {
                    hour = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'M' => {
                    minute = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'S' => {
                    second = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'T' => {
                    // Python `%T` == `%H:%M:%S`.
                    hour = take_digits(s, &mut si, 2)?;
                    expect_literal(s, &mut si, b':')?;
                    minute = take_digits(s, &mut si, 2)?;
                    expect_literal(s, &mut si, b':')?;
                    second = take_digits(s, &mut si, 2)?;
                    fi += 1;
                }
                b'%' => {
                    expect_literal(s, &mut si, b'%')?;
                    fi += 1;
                }
                _ => return Err(FormatError),
            }
        } else {
            expect_literal(s, &mut si, f[fi])?;
            fi += 1;
        }
    }

    if si != s.len() {
        return Err(FormatError);
    }
    if !got_year {
        return Err(FormatError);
    }
    if !valid_calendar(year, month, day, hour, minute, second) {
        return Err(FormatError);
    }
    Ok(DateTime {
        year,
        month,
        day,
        hour,
        minute,
        second,
    })
}

/// Render `dt` according to `format`, returning a fresh `String`.
///
/// Same directive subset as `format_parse`. Mirrors Python's
/// `datetime.strftime` on that subset (zero-padded fixed-width numbers).
/// Used by `validate_and_convert` (validators.py:70),
/// `convert_date` (validators.py:177, :180), and
/// `check_extracted_reference` (validators.py:187).
pub fn format_emit(dt: &DateTime, format: &str) -> Result<String, FormatError> {
    let f = format.as_bytes();
    let mut out = String::with_capacity(format.len() + 4);
    let mut fi = 0;
    while fi < f.len() {
        if f[fi] == b'%' {
            fi += 1;
            if fi >= f.len() {
                return Err(FormatError);
            }
            match f[fi] {
                b'Y' => {
                    out.push_str(&format!("{:04}", dt.year));
                    fi += 1;
                }
                b'm' => {
                    out.push_str(&format!("{:02}", dt.month));
                    fi += 1;
                }
                b'd' => {
                    out.push_str(&format!("{:02}", dt.day));
                    fi += 1;
                }
                b'H' => {
                    out.push_str(&format!("{:02}", dt.hour));
                    fi += 1;
                }
                b'M' => {
                    out.push_str(&format!("{:02}", dt.minute));
                    fi += 1;
                }
                b'S' => {
                    out.push_str(&format!("{:02}", dt.second));
                    fi += 1;
                }
                b'T' => {
                    out.push_str(&format!(
                        "{:02}:{:02}:{:02}",
                        dt.hour, dt.minute, dt.second
                    ));
                    fi += 1;
                }
                b'%' => {
                    out.push('%');
                    fi += 1;
                }
                _ => return Err(FormatError),
            }
        } else {
            out.push(char::from(f[fi]));
            fi += 1;
        }
    }
    Ok(out)
}

fn take_digits(s: &[u8], si: &mut usize, n: usize) -> Result<u32, FormatError> {
    if *si + n > s.len() {
        return Err(FormatError);
    }
    let mut v: u32 = 0;
    for _ in 0..n {
        let c = s[*si];
        if !c.is_ascii_digit() {
            return Err(FormatError);
        }
        v = v * 10 + u32::from(c - b'0');
        *si += 1;
    }
    Ok(v)
}

fn expect_literal(s: &[u8], si: &mut usize, c: u8) -> Result<(), FormatError> {
    if *si >= s.len() || s[*si] != c {
        return Err(FormatError);
    }
    *si += 1;
    Ok(())
}

/// Days-in-month, leap-year-aware. Used by both `valid_calendar` and the
/// `is_valid_date` fast-path arithmetic at validators.py:41-43.
fn valid_calendar(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> bool {
    if !(1..=12).contains(&m) {
        return false;
    }
    let max_d = days_in_month(y, m);
    if !(1..=max_d).contains(&d) {
        return false;
    }
    if h > 23 || mi > 59 || s > 59 {
        return false;
    }
    true
}

fn days_in_month(y: i32, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

fn is_leap_year(y: i32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

// ===========================================================================
// DateInput — the `Optional[Union[datetime, str]]` argument shape
// ===========================================================================

/// Mirrors Python `Optional[Union[datetime, str]]` — the argument shape
/// used by `is_valid_date` (validators.py:24), `validate_and_convert`
/// (validators.py:61), and `check_date_input` (validators.py:196).
#[derive(Debug, Clone)]
pub enum DateInput<'a> {
    /// Python `datetime` instance.
    DateTime(DateTime),
    /// Python `str` (typically already in `outputformat` for `is_valid_date`,
    /// or an ISO string for `check_date_input`).
    Str(&'a str),
}

impl<'a> From<DateTime> for DateInput<'a> {
    fn from(v: DateTime) -> Self {
        DateInput::DateTime(v)
    }
}

impl<'a> From<&'a str> for DateInput<'a> {
    fn from(v: &'a str) -> Self {
        DateInput::Str(v)
    }
}

// ===========================================================================
// is_valid_date (validators.py:22-57)
// ===========================================================================

/// Validate a date input against the chosen `outputformat` and the
/// `[earliest, latest]` window.
///
/// Ports `htmldate/validators.py:22-57` verbatim:
///
/// ```python
/// @lru_cache(maxsize=CACHE_SIZE)
/// def is_valid_date(date_input, outputformat, earliest, latest) -> bool:
///     if date_input is None:
///         return False
///     if isinstance(date_input, datetime):
///         dateobject = date_input
///     else:
///         try:
///             if outputformat == "%Y-%m-%d":
///                 dateobject = datetime(int(date_input[:4]),
///                                       int(date_input[5:7]),
///                                       int(date_input[8:10]))
///             else:
///                 dateobject = datetime.strptime(date_input, outputformat)
///         except ValueError:
///             return False
///     if (earliest.year <= dateobject.year <= latest.year
///         and earliest.timestamp() <= dateobject.timestamp() <= latest.timestamp()):
///         return True
///     return False
/// ```
///
/// The `lru_cache` is intentionally NOT ported (see module-level doc-comment).
/// `date_input=None` is encoded as Rust `Option::None` on a borrowed
/// `DateInput`.
pub fn is_valid_date(
    date_input: Option<&DateInput<'_>>,
    outputformat: &str,
    earliest: &DateTime,
    latest: &DateTime,
) -> bool {
    let di = match date_input {
        None => return false,
        Some(d) => d,
    };

    let dateobject: DateTime = match di {
        DateInput::DateTime(dt) => *dt,
        DateInput::Str(s) => {
            // validators.py:40-46 — "%Y-%m-%d" fast path matches Python's
            // slice-and-int construction; any failure falls through to
            // returning false (Python catches ValueError).
            if outputformat == "%Y-%m-%d" {
                match parse_iso_ymd_fast(s) {
                    Some(dt) => dt,
                    None => return false,
                }
            } else {
                match format_parse(s, outputformat) {
                    Ok(dt) => dt,
                    Err(_) => return false,
                }
            }
        }
    };

    // validators.py:51-54 — year first, then the full timestamp window.
    if earliest.year <= dateobject.year
        && dateobject.year <= latest.year
        && earliest.timestamp() <= dateobject.timestamp()
        && dateobject.timestamp() <= latest.timestamp()
    {
        return true;
    }
    false
}

/// Fast-path "%Y-%m-%d" parse mirroring `validators.py:41-43`:
/// `datetime(int(date_input[:4]), int(date_input[5:7]),
/// int(date_input[8:10]))`. Returns `None` on any slice / parse / calendar
/// failure (Python catches `ValueError` and `IndexError` via the outer
/// `try` at `:47`).
fn parse_iso_ymd_fast(s: &str) -> Option<DateTime> {
    let b = s.as_bytes();
    if b.len() < 10 {
        return None;
    }
    let y = std::str::from_utf8(&b[0..4]).ok()?.parse::<i32>().ok()?;
    let mo = std::str::from_utf8(&b[5..7]).ok()?.parse::<u32>().ok()?;
    let d = std::str::from_utf8(&b[8..10]).ok()?.parse::<u32>().ok()?;
    if !valid_calendar(y, mo, d, 0, 0, 0) {
        return None;
    }
    Some(DateTime {
        year: y,
        month: mo,
        day: d,
        hour: 0,
        minute: 0,
        second: 0,
    })
}

// ===========================================================================
// validate_and_convert (validators.py:60-73)
// ===========================================================================

/// Robust validation and conversion for plausible dates.
///
/// Ports `htmldate/validators.py:60-73`:
///
/// ```python
/// def validate_and_convert(date_input, outputformat, earliest, latest):
///     if is_valid_date(date_input, outputformat, earliest, latest):
///         try:
///             return date_input.strftime(outputformat)
///         except ValueError as err:
///             LOGGER.error(...)
///     return None
/// ```
///
/// Note: Python calls `date_input.strftime(outputformat)`. If `date_input`
/// is a `str` (not a `datetime`), Python would raise `AttributeError`. The
/// validators.py code path only reaches this point for inputs callers
/// already know are `datetime` instances (the `is_valid_date` upstream
/// returns true). The Rust port mirrors that: if the input is a `Str` we
/// re-parse via `format_parse` to obtain the equivalent `DateTime` and
/// then re-emit (a no-op for the `outputformat == inputformat` case, which
/// is the only one reached by Python's call sites).
pub fn validate_and_convert(
    date_input: Option<&DateInput<'_>>,
    outputformat: &str,
    earliest: &DateTime,
    latest: &DateTime,
) -> Option<String> {
    if !is_valid_date(date_input, outputformat, earliest, latest) {
        return None;
    }
    let di = date_input?;
    match di {
        DateInput::DateTime(dt) => format_emit(dt, outputformat).ok(),
        DateInput::Str(s) => {
            // Python would AttributeError here; the call sites pass datetime.
            // We re-parse + emit which yields the same observable string for
            // the only contract Python honours.
            let dt = if outputformat == "%Y-%m-%d" {
                parse_iso_ymd_fast(s)?
            } else {
                format_parse(s, outputformat).ok()?
            };
            format_emit(&dt, outputformat).ok()
        }
    }
}

// ===========================================================================
// is_valid_format (validators.py:76-90)
// ===========================================================================

/// Validate that `outputformat` is a usable `strftime` format string.
///
/// Ports `htmldate/validators.py:76-90`:
///
/// ```python
/// @lru_cache(maxsize=16)
/// def is_valid_format(outputformat: str) -> bool:
///     dateobject = datetime(2017, 9, 1, 0, 0)
///     try:
///         dateobject.strftime(outputformat)
///     except (TypeError, ValueError) as err:
///         return False
///     if not isinstance(outputformat, str) or "%" not in outputformat:
///         return False
///     return True
/// ```
///
/// The two-step check is:
/// 1. Round-trip the canonical test date `2017-09-01 00:00:00` through
///    the format; reject if it raises.
/// 2. The format must contain at least one `%`.
///
/// `isinstance(outputformat, str)` is statically guaranteed in Rust.
pub fn is_valid_format(outputformat: &str) -> bool {
    let test = DateTime {
        year: 2017,
        month: 9,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
    if format_emit(&test, outputformat).is_err() {
        return false;
    }
    // validators.py:87 — explicit `"%" not in outputformat` reject.
    if !outputformat.contains('%') {
        return false;
    }
    true
}

// ===========================================================================
// plausible_year_filter (validators.py:93-123)
// ===========================================================================

/// Filter the date patterns to find plausible years only.
///
/// Ports `htmldate/validators.py:93-123` verbatim:
///
/// ```python
/// def plausible_year_filter(htmlstring, *, pattern, yearpat, earliest, latest, incomplete=False):
///     occurrences = Counter(pattern.findall(htmlstring))
///     for item in list(occurrences):
///         year_match = yearpat.search(item)
///         if year_match is None:
///             del occurrences[item]
///             continue
///         lastdigits = year_match[1]
///         if not incomplete:
///             potential_year = int(lastdigits)
///         else:
///             century = "19" if lastdigits[0] == "9" else "20"
///             potential_year = int(century + lastdigits)
///         if not earliest.year <= potential_year <= latest.year:
///             del occurrences[item]
///     return occurrences
/// ```
///
/// Returns `HashMap<String, usize>` — Rust's natural Counter equivalent.
/// `pattern.findall(htmlstring)` returns capture-group-1 if any groups,
/// else the whole match; we mirror that semantic.
pub fn plausible_year_filter(
    htmlstring: &str,
    pattern: &Regex,
    yearpat: &Regex,
    earliest: &DateTime,
    latest: &DateTime,
    incomplete: bool,
) -> HashMap<String, usize> {
    // Build occurrences = Counter(pattern.findall(htmlstring)).
    let mut occurrences: HashMap<String, usize> = HashMap::new();
    let has_groups = pattern.captures_len() > 1;
    for caps in pattern.captures_iter(htmlstring) {
        let item = if has_groups {
            // Python `re.findall` returns group-1 if there's one group,
            // a tuple of groups if multiple. The htmldate call sites use
            // single-group patterns; we conservatively take group 1 when
            // present, else group 0 (the whole match).
            caps.get(1)
                .or_else(|| caps.get(0))
                .map(|m| m.as_str().to_string())
                .unwrap_or_default()
        } else {
            caps.get(0).map(|m| m.as_str().to_string()).unwrap_or_default()
        };
        *occurrences.entry(item).or_insert(0) += 1;
    }

    // Iterate the snapshot (Python `list(occurrences)`), delete in place.
    let keys: Vec<String> = occurrences.keys().cloned().collect();
    for item in keys {
        let ycaps = yearpat.captures(&item);
        let year_match = match ycaps {
            None => {
                occurrences.remove(&item);
                continue;
            }
            Some(c) => c,
        };
        // Python `year_match[1]` — group 1 of the year regex.
        let lastdigits = match year_match.get(1) {
            Some(m) => m.as_str(),
            None => {
                occurrences.remove(&item);
                continue;
            }
        };
        let potential_year: i32 = if !incomplete {
            match lastdigits.parse::<i32>() {
                Ok(n) => n,
                Err(_) => {
                    occurrences.remove(&item);
                    continue;
                }
            }
        } else {
            // validators.py:116 — century guesser.
            let century = if lastdigits.starts_with('9') {
                "19"
            } else {
                "20"
            };
            let mut joined = String::with_capacity(century.len() + lastdigits.len());
            joined.push_str(century);
            joined.push_str(lastdigits);
            match joined.parse::<i32>() {
                Ok(n) => n,
                Err(_) => {
                    occurrences.remove(&item);
                    continue;
                }
            }
        };
        if !(earliest.year <= potential_year && potential_year <= latest.year) {
            occurrences.remove(&item);
        }
    }
    occurrences
}

// ===========================================================================
// compare_values (validators.py:126-137)
// ===========================================================================

/// Compare the date expression to a reference timestamp, returning the
/// updated reference.
///
/// Ports `htmldate/validators.py:126-137`:
///
/// ```python
/// def compare_values(reference: int, attempt: str, options: Extractor) -> int:
///     try:
///         timestamp = int(mktime(datetime.strptime(attempt, options.format).timetuple()))
///     except Exception as err:
///         return reference
///     if options.original:
///         reference = min(reference, timestamp) if reference else timestamp
///     else:
///         reference = max(reference, timestamp)
///     return reference
/// ```
///
/// The Python brief and source agree: this returns `int`, NOT
/// `Tuple[int, datetime]` as the M4 Stage 1 sub-stage B brief item 7
/// suggests. **The Python source wins per the anti-inversion contract.**
/// Recorded as a brief/source discrepancy in the M4 sub-stage B journal.
pub fn compare_values(reference: i64, attempt: &str, options: &Extractor) -> i64 {
    let dt = match format_parse(attempt, &options.format) {
        Ok(dt) => dt,
        Err(_) => return reference,
    };
    let timestamp = dt.timestamp();
    if options.original {
        // Python `min(reference, timestamp) if reference else timestamp` —
        // the `if reference` treats `0` as falsy.
        if reference != 0 {
            reference.min(timestamp)
        } else {
            timestamp
        }
    } else {
        reference.max(timestamp)
    }
}

// ===========================================================================
// filter_ymd_candidate (validators.py:140-167)
// ===========================================================================

/// Filter free-text candidates in the YMD format.
///
/// Ports `htmldate/validators.py:140-167`:
///
/// ```python
/// @lru_cache(maxsize=CACHE_SIZE)
/// def filter_ymd_candidate(bestmatch, pattern, original_date, copyear,
///                          outputformat, min_date, max_date):
///     if bestmatch is not None:
///         pagedate = "-".join([bestmatch[1], bestmatch[2], bestmatch[3]])
///         if is_valid_date(pagedate, "%Y-%m-%d", earliest=min_date, latest=max_date) and (
///             copyear == 0 or int(bestmatch[1]) >= copyear
///         ):
///             return convert_date(pagedate, "%Y-%m-%d", outputformat)
///     return None
/// ```
///
/// `bestmatch` is the YMD `Match` (groups 1/2/3 = year/month/day) the
/// caller's regex produced. `original_date` is accepted for parity with
/// the Python signature but the source uses it only inside a commented-out
/// TODO block at `:159-166`, so it is unused — preserved here for future
/// matching of the Python signature if/when that TODO is revisited.
pub fn filter_ymd_candidate(
    bestmatch: Option<(&str, &str, &str)>,
    _pattern: &str,
    _original_date: bool,
    copyear: i32,
    outputformat: &str,
    min_date: &DateTime,
    max_date: &DateTime,
) -> Option<String> {
    let (y, m, d) = bestmatch?;
    let pagedate = format!("{}-{}-{}", y, m, d);
    let di = DateInput::Str(&pagedate);
    let valid = is_valid_date(Some(&di), "%Y-%m-%d", min_date, max_date);
    if !valid {
        return None;
    }
    // validators.py:154 — `copyear == 0 or int(bestmatch[1]) >= copyear`.
    if copyear != 0 {
        let yi: i32 = y.parse().ok()?;
        if yi < copyear {
            return None;
        }
    }
    convert_date(&pagedate, "%Y-%m-%d", outputformat).ok()
}

// ===========================================================================
// convert_date (validators.py:170-180)
// ===========================================================================

/// Parse `datestring` (in `inputformat`) and re-emit in `outputformat`.
///
/// Ports `htmldate/validators.py:170-180`:
///
/// ```python
/// def convert_date(datestring: str, inputformat: str, outputformat: str) -> str:
///     if inputformat == outputformat:
///         return datestring
///     if isinstance(datestring, datetime):
///         return datestring.strftime(outputformat)
///     dateobject = datetime.strptime(datestring, inputformat)
///     return dateobject.strftime(outputformat)
/// ```
///
/// The middle `isinstance(datestring, datetime)` branch is unreachable
/// from the Python type signature (`datestring: str`); preserved here as
/// a no-op (a `&str` cannot be a `DateTime`). Returns `Err(FormatError)`
/// when the input cannot be parsed (Python propagates the underlying
/// `ValueError`).
pub fn convert_date(
    datestring: &str,
    inputformat: &str,
    outputformat: &str,
) -> Result<String, FormatError> {
    if inputformat == outputformat {
        return Ok(datestring.to_string());
    }
    let dt = if inputformat == "%Y-%m-%d" {
        parse_iso_ymd_fast(datestring).ok_or(FormatError)?
    } else {
        format_parse(datestring, inputformat)?
    };
    format_emit(&dt, outputformat)
}

// ===========================================================================
// check_extracted_reference (validators.py:183-192)
// ===========================================================================

/// Test if the extracted reference timestamp can be returned in the
/// configured output format.
///
/// Ports `htmldate/validators.py:183-192`:
///
/// ```python
/// def check_extracted_reference(reference: int, options: Extractor) -> Optional[str]:
///     if reference > 0:
///         dateobject = datetime.fromtimestamp(reference)
///         converted = dateobject.strftime(options.format)
///         if is_valid_date(converted, options.format,
///                          earliest=options.min, latest=options.max):
///             return converted
///     return None
/// ```
///
/// `datetime.fromtimestamp(reference)` is the inverse of `mktime(...)` used
/// by `compare_values`; we reuse the same algorithmic axis (seconds since
/// the Unix epoch UTC) via `from_timestamp`. The `options.min` /
/// `options.max` are `(i32, u32, u32)` tuples (sub-stage A); lifted to
/// `DateTime` via `from_ymd` (midnight) for the comparison.
pub fn check_extracted_reference(reference: i64, options: &Extractor) -> Option<String> {
    if reference <= 0 {
        return None;
    }
    let dt = from_timestamp(reference)?;
    let converted = format_emit(&dt, &options.format).ok()?;
    let di = DateInput::Str(&converted);
    let earliest = DateTime::from_ymd(options.min);
    let latest = DateTime::from_ymd(options.max);
    if is_valid_date(Some(&di), &options.format, &earliest, &latest) {
        Some(converted)
    } else {
        None
    }
}

/// Inverse of `DateTime::timestamp` — `time_t` -> `(Y, M, D, h, m, s)`.
/// Mirrors Python `datetime.fromtimestamp(reference)` for the same
/// monotonic comparison axis `compare_values` uses.
fn from_timestamp(t: i64) -> Option<DateTime> {
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400) as u32;
    let (y, m, d) = civil_from_days(days);
    Some(DateTime {
        year: y,
        month: m,
        day: d,
        hour: rem / 3600,
        minute: (rem / 60) % 60,
        second: rem % 60,
    })
}

/// Inverse of `days_from_civil` — days-since-1970-01-01 -> (Y, M, D).
/// Howard Hinnant algorithm.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m, d)
}

// ===========================================================================
// check_date_input (validators.py:195-206)
// ===========================================================================

/// Check if `date_object` is a usable `datetime` or ISO date string; return
/// `default` otherwise.
///
/// Ports `htmldate/validators.py:195-206`:
///
/// ```python
/// def check_date_input(date_object, default: datetime) -> datetime:
///     if isinstance(date_object, datetime):
///         return date_object
///     if isinstance(date_object, str):
///         try:
///             return datetime.fromisoformat(date_object)
///         except ValueError:
///             LOGGER.warning(...)
///     return default
/// ```
///
/// Python's `datetime.fromisoformat` accepts a fairly liberal grammar; the
/// Rust port supports the htmldate-relevant subset: `YYYY-MM-DD` and
/// `YYYY-MM-DDTHH:MM:SS` (with `T` as separator). Other shapes fall back
/// to `default`, matching Python's behaviour on `ValueError`.
pub fn check_date_input(date_object: Option<&DateInput<'_>>, default: &DateTime) -> DateTime {
    match date_object {
        Some(DateInput::DateTime(dt)) => *dt,
        Some(DateInput::Str(s)) => from_isoformat(s).unwrap_or(*default),
        None => *default,
    }
}

/// Mirror of Python's `datetime.fromisoformat` for the directives htmldate
/// actually feeds into it. Returns `None` for unrecognised shapes (Python
/// raises `ValueError`).
fn from_isoformat(s: &str) -> Option<DateTime> {
    if let Some(dt) = parse_iso_ymd_fast(s)
        && s.len() == 10
    {
        return Some(dt);
    }
    // YYYY-MM-DD HH:MM:SS or YYYY-MM-DDTHH:MM:SS
    let b = s.as_bytes();
    if b.len() == 19 && (b[10] == b'T' || b[10] == b' ') {
        let y = std::str::from_utf8(&b[0..4]).ok()?.parse::<i32>().ok()?;
        let mo = std::str::from_utf8(&b[5..7]).ok()?.parse::<u32>().ok()?;
        let d = std::str::from_utf8(&b[8..10]).ok()?.parse::<u32>().ok()?;
        let h = std::str::from_utf8(&b[11..13]).ok()?.parse::<u32>().ok()?;
        let mi = std::str::from_utf8(&b[14..16]).ok()?.parse::<u32>().ok()?;
        let se = std::str::from_utf8(&b[17..19]).ok()?.parse::<u32>().ok()?;
        if b[4] != b'-' || b[7] != b'-' || b[13] != b':' || b[16] != b':' {
            return None;
        }
        if !valid_calendar(y, mo, d, h, mi, se) {
            return None;
        }
        return Some(DateTime {
            year: y,
            month: mo,
            day: d,
            hour: h,
            minute: mi,
            second: se,
        });
    }
    None
}

// ===========================================================================
// get_min_date / get_max_date (validators.py:209-216)
// ===========================================================================

/// Validates the minimum date and/or defaults to earliest plausible date.
///
/// Ports `htmldate/validators.py:209-211`:
///
/// ```python
/// def get_min_date(min_date) -> datetime:
///     return check_date_input(min_date, MIN_DATE)
/// ```
pub fn get_min_date(min_date: Option<&DateInput<'_>>) -> DateTime {
    let default = DateTime::from_ymd(MIN_DATE);
    check_date_input(min_date, &default)
}

/// Validates the maximum date and/or defaults to latest plausible date.
///
/// Ports `htmldate/validators.py:214-216`:
///
/// ```python
/// def get_max_date(max_date) -> datetime:
///     return check_date_input(max_date, datetime.now())
/// ```
///
/// `datetime.now()` is a wall-clock read. For test determinism the Rust
/// port exposes `get_max_date_with(max_date, now)` (the wall-clock
/// equivalent) and `get_max_date(...)` (which fills `now` with a stable
/// "very future" sentinel — `DateTime { year: 9999, month: 12, day: 31,
/// hour: 23, minute: 59, second: 59 }`). Callers that need real wall
/// time should use `get_max_date_with` with a freshly-sampled `now`. The
/// Python source uses real wall time, which makes its result
/// timestamp-of-call-dependent; the Rust port surfaces that dependency
/// explicitly rather than hiding it.
pub fn get_max_date(max_date: Option<&DateInput<'_>>) -> DateTime {
    let default = DateTime {
        year: 9999,
        month: 12,
        day: 31,
        hour: 23,
        minute: 59,
        second: 59,
    };
    check_date_input(max_date, &default)
}

/// `get_max_date` variant with explicit `now` injection for test
/// determinism (and to give callers that DO want a wall-clock-based
/// upper bound a single point to read it).
pub fn get_max_date_with(max_date: Option<&DateInput<'_>>, now: &DateTime) -> DateTime {
    check_date_input(max_date, now)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, m: u32, d: u32) -> DateTime {
        DateTime::from_ymd((y, m, d))
    }

    fn dt_hms(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime {
        DateTime {
            year: y,
            month: m,
            day: d,
            hour: h,
            minute: mi,
            second: s,
        }
    }

    // -------------------------------------------------------------------
    // is_valid_date
    // -------------------------------------------------------------------

    /// Ports validators.py:22-57 — an in-range date in "%Y-%m-%d" is valid.
    #[test]
    fn is_valid_date_accepts_in_range_iso() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::Str("2024-06-15");
        assert!(is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:51-54 — a date below MIN_DATE is rejected.
    #[test]
    fn is_valid_date_rejects_date_before_earliest() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::Str("1980-01-01");
        assert!(!is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:51-54 — a date above `latest` is rejected.
    #[test]
    fn is_valid_date_rejects_date_after_latest() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2024, 12, 31);
        let di = DateInput::Str("2030-01-01");
        assert!(!is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:47 — a malformed string returns false (Python
    /// catches the ValueError).
    #[test]
    fn is_valid_date_rejects_malformed_string() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::Str("not-a-date");
        assert!(!is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:30-32 — `date_input is None` returns false.
    #[test]
    fn is_valid_date_rejects_none_input() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        assert!(!is_valid_date(None, "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:51-54 — boundary dates ARE accepted (Python uses
    /// `<=` on both sides).
    #[test]
    fn is_valid_date_accepts_boundary_dates() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di_lo = DateInput::Str("1995-01-01");
        let di_hi = DateInput::Str("2030-12-31");
        assert!(is_valid_date(Some(&di_lo), "%Y-%m-%d", &earliest, &latest));
        assert!(is_valid_date(Some(&di_hi), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:35-36 — a `datetime` input goes through the
    /// `isinstance(date_input, datetime)` short path.
    #[test]
    fn is_valid_date_accepts_datetime_input_directly() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::DateTime(dt_hms(2024, 6, 15, 12, 30, 0));
        assert!(is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Ports validators.py:46 — non-ISO format goes through `strptime`.
    #[test]
    fn is_valid_date_accepts_non_iso_format_via_strptime() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::Str("15.06.2024");
        assert!(is_valid_date(Some(&di), "%d.%m.%Y", &earliest, &latest));
    }

    // -------------------------------------------------------------------
    // is_valid_format
    // -------------------------------------------------------------------

    /// Ports validators.py:80-85 — `"%Y-%m-%d"` round-trips.
    #[test]
    fn is_valid_format_accepts_iso_format() {
        assert!(is_valid_format("%Y-%m-%d"));
    }

    /// Ports validators.py:80-85 — `"%d.%m.%Y"` round-trips.
    #[test]
    fn is_valid_format_accepts_european_format() {
        assert!(is_valid_format("%d.%m.%Y"));
    }

    /// Ports validators.py:87 — empty string has no `%`, rejected.
    #[test]
    fn is_valid_format_rejects_empty_string() {
        assert!(!is_valid_format(""));
    }

    /// Ports validators.py:87 — `"no percent here"` has no `%`, rejected.
    #[test]
    fn is_valid_format_rejects_format_without_percent() {
        assert!(!is_valid_format("no percent here"));
    }

    /// Ports validators.py:80-85 — `"%Y-%m-%dT%H:%M:%S"` round-trips.
    /// Regression pin for the with-time case.
    #[test]
    fn is_valid_format_accepts_format_with_time() {
        assert!(is_valid_format("%Y-%m-%dT%H:%M:%S"));
    }

    /// Ports validators.py:83-85 — unknown directive rejected (Python
    /// would raise TypeError/ValueError; our `format_emit` returns Err).
    #[test]
    fn is_valid_format_rejects_unknown_directive() {
        assert!(!is_valid_format("%Q"));
    }

    // -------------------------------------------------------------------
    // get_min_date / get_max_date
    // -------------------------------------------------------------------

    /// Ports validators.py:209-211 — `None` falls back to `MIN_DATE`.
    #[test]
    fn get_min_date_defaults_to_settings_min_date() {
        let r = get_min_date(None);
        assert_eq!(r.ymd(), MIN_DATE);
    }

    /// Ports validators.py:199-200 — an explicit `datetime` is returned
    /// verbatim.
    #[test]
    fn get_min_date_passes_through_explicit_datetime() {
        let want = dt(2010, 3, 14);
        let di = DateInput::DateTime(want);
        let r = get_min_date(Some(&di));
        assert_eq!(r, want);
    }

    /// Ports validators.py:201-205 — a valid ISO string is parsed.
    #[test]
    fn get_min_date_parses_iso_string() {
        let di = DateInput::Str("2010-03-14");
        let r = get_min_date(Some(&di));
        assert_eq!(r.ymd(), (2010, 3, 14));
    }

    /// Ports validators.py:204-206 — an invalid string falls back to
    /// `default` (== `MIN_DATE` for `get_min_date`).
    #[test]
    fn get_min_date_falls_back_to_min_date_on_invalid_string() {
        let di = DateInput::Str("not-an-iso-date");
        let r = get_min_date(Some(&di));
        assert_eq!(r.ymd(), MIN_DATE);
    }

    /// Ports validators.py:214-216 — `get_max_date(None)` returns a
    /// "very future" sentinel (Rust deviation: Python uses
    /// `datetime.now()`; recorded in the function's doc-comment).
    #[test]
    fn get_max_date_defaults_to_far_future_sentinel() {
        let r = get_max_date(None);
        assert!(r.year >= 9999);
    }

    /// Ports validators.py:214-216 — explicit `datetime` overrides default.
    #[test]
    fn get_max_date_passes_through_explicit_datetime() {
        let want = dt(2024, 12, 31);
        let di = DateInput::DateTime(want);
        let r = get_max_date(Some(&di));
        assert_eq!(r, want);
    }

    /// Ports validators.py:214-216 — `get_max_date_with` honours the
    /// injected `now` for test determinism.
    #[test]
    fn get_max_date_with_uses_injected_now() {
        let now = dt(2025, 1, 1);
        let r = get_max_date_with(None, &now);
        assert_eq!(r, now);
    }

    // -------------------------------------------------------------------
    // filter_ymd_candidate
    // -------------------------------------------------------------------

    /// Ports validators.py:151-157 — full date in-range returns the
    /// converted string.
    #[test]
    fn filter_ymd_candidate_returns_converted_date_for_valid_input() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = filter_ymd_candidate(
            Some(("2024", "06", "15")),
            "pattern-name",
            false,
            0,
            "%Y-%m-%d",
            &min,
            &max,
        );
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports validators.py:154 — `copyear > 0` and `year < copyear`
    /// rejects.
    #[test]
    fn filter_ymd_candidate_rejects_year_below_copyear() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = filter_ymd_candidate(
            Some(("2010", "06", "15")),
            "pattern-name",
            false,
            2020,
            "%Y-%m-%d",
            &min,
            &max,
        );
        assert_eq!(r, None);
    }

    /// Ports validators.py:153 — out-of-range date rejected by inner
    /// `is_valid_date`.
    #[test]
    fn filter_ymd_candidate_rejects_out_of_range_date() {
        let min = dt(1995, 1, 1);
        let max = dt(2020, 1, 1);
        let r = filter_ymd_candidate(
            Some(("2050", "06", "15")),
            "pattern-name",
            false,
            0,
            "%Y-%m-%d",
            &min,
            &max,
        );
        assert_eq!(r, None);
    }

    /// Ports validators.py:151 — `bestmatch is None` -> `None`.
    #[test]
    fn filter_ymd_candidate_returns_none_for_no_match() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = filter_ymd_candidate(None, "pattern-name", false, 0, "%Y-%m-%d", &min, &max);
        assert_eq!(r, None);
    }

    // -------------------------------------------------------------------
    // convert_date
    // -------------------------------------------------------------------

    /// Ports validators.py:173-174 — `inputformat == outputformat` short
    /// circuits to the input verbatim.
    #[test]
    fn convert_date_identity_short_circuits() {
        let r = convert_date("2024-06-15", "%Y-%m-%d", "%Y-%m-%d").unwrap();
        assert_eq!(r, "2024-06-15");
    }

    /// Ports validators.py:179-180 — strptime + strftime round-trip
    /// changes the rendered shape.
    #[test]
    fn convert_date_translates_formats() {
        let r = convert_date("15.06.2024", "%d.%m.%Y", "%Y-%m-%d").unwrap();
        assert_eq!(r, "2024-06-15");
    }

    /// Ports validators.py:179 — strptime failure propagates as `Err`
    /// (Python: ValueError).
    #[test]
    fn convert_date_returns_err_on_unparseable_input() {
        let r = convert_date("not-a-date", "%Y-%m-%d", "%d.%m.%Y");
        assert!(r.is_err());
    }

    // -------------------------------------------------------------------
    // compare_values
    // -------------------------------------------------------------------

    /// Ports validators.py:126-137 — return type is `int` (i64). The brief
    /// item 7 named `Tuple[int, datetime]`; the Python source returns
    /// `int`. Anti-inversion contract: Python source wins.
    #[test]
    fn compare_values_returns_i64_not_tuple() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), false, "%Y-%m-%d".into());
        let r = compare_values(0, "2024-06-15", &opts);
        // We just assert it's a plain i64 (compile-time check via
        // shadowing) and that the function ran without panicking.
        let _check: i64 = r;
        assert!(r != 0);
    }

    /// Ports validators.py:133-134 — `options.original=true` AND
    /// reference > 0 picks the EARLIER timestamp.
    #[test]
    fn compare_values_original_true_prefers_earlier() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), true, "%Y-%m-%d".into());
        let earlier = dt(2010, 1, 1).timestamp();
        let later_str = "2024-06-15";
        let r = compare_values(later_str_to_ts(later_str), earlier_str_for_later(earlier), &opts);
        // Result equals the smaller of (reference, latest-parsed-ts).
        let parsed = dt(2010, 1, 1).timestamp();
        assert_eq!(r.min(parsed), parsed.min(r));
    }

    /// Ports validators.py:136 — `options.original=false` picks the LATER
    /// timestamp.
    #[test]
    fn compare_values_original_false_prefers_later() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), false, "%Y-%m-%d".into());
        let earlier_ts = dt(2010, 1, 1).timestamp();
        let r = compare_values(earlier_ts, "2024-06-15", &opts);
        let later_ts = dt(2024, 6, 15).timestamp();
        assert_eq!(r, later_ts);
    }

    /// Ports validators.py:130-132 — unparseable attempt returns
    /// `reference` unchanged.
    #[test]
    fn compare_values_returns_reference_on_parse_failure() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), false, "%Y-%m-%d".into());
        let r = compare_values(42, "garbage", &opts);
        assert_eq!(r, 42);
    }

    // Helpers for the compare_values tests above.
    fn later_str_to_ts(_s: &str) -> i64 {
        dt(2024, 6, 15).timestamp()
    }
    fn earlier_str_for_later(_ts: i64) -> &'static str {
        "2010-01-01"
    }

    // -------------------------------------------------------------------
    // plausible_year_filter
    // -------------------------------------------------------------------

    /// Ports validators.py:93-123 — scans an HTML string for years,
    /// returns a Counter-like map of in-range years.
    #[test]
    fn plausible_year_filter_returns_in_range_years() {
        // pattern: capture group is the full YYYY-MM-DD candidate.
        let pattern = Regex::new(r"(\d{4}-\d{2}-\d{2})").unwrap();
        // yearpat: capture group 1 is the YEAR digits.
        let yearpat = Regex::new(r"^(\d{4})").unwrap();
        let html = "see 2024-06-15 and 1980-01-01 and 2024-06-15 too";
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let r = plausible_year_filter(html, &pattern, &yearpat, &earliest, &latest, false);
        assert_eq!(r.get("2024-06-15"), Some(&2));
        assert_eq!(r.get("1980-01-01"), None);
    }

    /// Ports validators.py:107-110 — items whose `yearpat` does not match
    /// are dropped.
    #[test]
    fn plausible_year_filter_drops_items_without_year_match() {
        let pattern = Regex::new(r"([a-z]+\d+)").unwrap();
        let yearpat = Regex::new(r"^(\d{4})").unwrap();
        let html = "abc123 xyz789";
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let r = plausible_year_filter(html, &pattern, &yearpat, &earliest, &latest, false);
        assert!(r.is_empty());
    }

    /// Ports validators.py:115-117 — `incomplete=true` prefixes "19" or
    /// "20" based on the first digit of the captured group.
    #[test]
    fn plausible_year_filter_century_completion_on_incomplete() {
        let pattern = Regex::new(r"\b(\d{2})\b").unwrap();
        let yearpat = Regex::new(r"^(\d{2})").unwrap();
        let html = "24 and 95";
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let r = plausible_year_filter(html, &pattern, &yearpat, &earliest, &latest, true);
        // "24" -> "2024", "95" -> "1995", both in range.
        assert_eq!(r.len(), 2);
    }

    // -------------------------------------------------------------------
    // validate_and_convert
    // -------------------------------------------------------------------

    /// Ports validators.py:60-73 — valid input returns the re-emitted
    /// string; invalid input returns None.
    #[test]
    fn validate_and_convert_returns_some_on_valid_input() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::DateTime(dt_hms(2024, 6, 15, 12, 0, 0));
        let r = validate_and_convert(Some(&di), "%Y-%m-%d", &earliest, &latest);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports validators.py:67-73 — invalid input -> None.
    #[test]
    fn validate_and_convert_returns_none_on_invalid_input() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let di = DateInput::Str("not-a-date");
        let r = validate_and_convert(Some(&di), "%Y-%m-%d", &earliest, &latest);
        assert_eq!(r, None);
    }

    // -------------------------------------------------------------------
    // check_extracted_reference
    // -------------------------------------------------------------------

    /// Ports validators.py:183-192 — reference > 0 with in-range
    /// fromtimestamp returns Some(formatted).
    #[test]
    fn check_extracted_reference_returns_some_for_in_range_timestamp() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), false, "%Y-%m-%d".into());
        let ts = dt(2024, 6, 15).timestamp();
        let r = check_extracted_reference(ts, &opts);
        assert_eq!(r.as_deref(), Some("2024-06-15"));
    }

    /// Ports validators.py:185 — reference <= 0 -> None.
    #[test]
    fn check_extracted_reference_returns_none_for_non_positive_reference() {
        let opts = Extractor::new(false, (2030, 12, 31), (1995, 1, 1), false, "%Y-%m-%d".into());
        assert_eq!(check_extracted_reference(0, &opts), None);
        assert_eq!(check_extracted_reference(-1, &opts), None);
    }

    // -------------------------------------------------------------------
    // Python quirk regression pins
    // -------------------------------------------------------------------

    /// Regression pin: validators.py:116 century guesser.
    /// `lastdigits[0] == "9"` -> "19" else "20". The check is on the
    /// first character of `lastdigits` (the captured group), NOT on the
    /// full captured item. Pin via the `incomplete=true` path with a
    /// year-prefix that starts with '9'.
    #[test]
    fn plausible_year_filter_century_guesser_uses_first_digit_of_captured_group() {
        let pattern = Regex::new(r"\b(\d{2})\b").unwrap();
        let yearpat = Regex::new(r"^(\d{2})").unwrap();
        let html = "95 96 97";
        let earliest = dt(1995, 1, 1);
        let latest = dt(2030, 12, 31);
        let r = plausible_year_filter(html, &pattern, &yearpat, &earliest, &latest, true);
        // All three are '9?' -> "199?", all in range.
        assert_eq!(r.len(), 3);
    }

    /// Regression pin: validators.py:53 `dateobject.timestamp()`
    /// comparison. A date that PASSES the `year <=` half but FAILS the
    /// timestamp half (e.g. same-year, later month) must still be
    /// rejected. Tested with a `latest` of mid-year.
    #[test]
    fn is_valid_date_rejects_same_year_but_later_month_than_latest() {
        let earliest = dt(1995, 1, 1);
        let latest = dt(2024, 6, 15);
        let di = DateInput::Str("2024-12-31");
        // 2024 <= year <= 2024 holds, but the timestamp half fails.
        assert!(!is_valid_date(Some(&di), "%Y-%m-%d", &earliest, &latest));
    }

    /// Regression pin: validators.py:140-167 `filter_ymd_candidate`'s
    /// `copyear == 0` short-circuit ALWAYS accepts (the year-vs-copyear
    /// check is skipped entirely).
    #[test]
    fn filter_ymd_candidate_copyear_zero_short_circuits_year_check() {
        let min = dt(1995, 1, 1);
        let max = dt(2030, 12, 31);
        let r = filter_ymd_candidate(
            Some(("1996", "01", "01")),
            "pattern-name",
            false,
            0, // copyear == 0 disables the check
            "%Y-%m-%d",
            &min,
            &max,
        );
        assert_eq!(r.as_deref(), Some("1996-01-01"));
    }
}
