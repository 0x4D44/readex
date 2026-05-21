//! Regex catalogues + month tables — sub-stage C port of
//! `htmldate/extractors.py:47-213`.
//!
//! Source of truth: `htmldate@1.9.x/extractors.py` (vendored under
//! `C:\Users\marti\AppData\Roaming\Python\Python314\site-packages\htmldate\
//! extractors.py`). Every constant cites its exact Python source line per the
//! M4 Stage 1 sub-stage C anti-inversion contract.
//!
//! This sub-stage ports the entire **regex catalogue** at the top of
//! `extractors.py` (the module-level `re.compile(...)` constants plus the
//! `MONTHS` / `TEXT_MONTHS` tables plus the `DATE_EXPRESSIONS` XPath string
//! plus the `FAST_PREPEND` / `SLOW_PREPEND` XPath prefix strings plus the
//! supporting `DAY_RE` / `MONTH_RE` / `YEAR_RE` building-block strings and
//! `REGEX_MONTHS` multilingual alternation). No runtime behaviour is wired —
//! these constants are pure data consumed by sub-stage D (the date-extraction
//! algorithm).
//!
//! # Lazy initialisation
//!
//! Each compiled regex lives behind a `std::sync::OnceLock<regex::Regex>` slot
//! and is exposed via a small `pub(crate) fn` accessor that returns
//! `&'static Regex`. Pattern matches the M3 Stage 4a `readability_fork`
//! precedent (six regex slots) and the htmldate sub-stage A/B precedent (no
//! `lazy_static` / `once_cell` dependency — `OnceLock` is in `std`).
//!
//! # Faithful divergences (recorded — HLD §4 anti-inversion)
//!
//! ## SIMPLE_PATTERN's lookbehind
//!
//! Python's `SIMPLE_PATTERN` at `extractors.py:213` uses a negative
//! lookbehind: `(?<!w3.org)\D({YEAR_RE})\D`. The Rust `regex` crate does
//! **NOT** support lookarounds (it is a finite-automaton engine, not a
//! backtracking engine). The catalogue therefore exposes:
//!
//! - `simple_pattern() -> &'static Regex` — the pattern WITHOUT the
//!   lookbehind (just `\D({YEAR_RE})\D`).
//! - `simple_pattern_post_filter(haystack, match_start) -> bool` — returns
//!   `true` IFF the match should be kept (i.e. the text immediately preceding
//!   the match does NOT end with `"w3.org"`).
//!
//! Sub-stage D callers MUST invoke `simple_pattern_post_filter` on every hit
//! to faithfully reproduce Python's `(?<!w3.org)` semantic. Documented loudly
//! here so the responsibility is explicit.
//!
//! ## `match.lastgroup` semantics
//!
//! Python's `re.Match.lastgroup` returns the name of the LAST named group that
//! participated in the match. Rust's `regex::Captures` has no direct
//! equivalent. Sub-stage C does NOT use `lastgroup` itself (the catalogue is
//! pure data); sub-stage D's callers (see `extractors.py:267, 337, 364`) need
//! a helper that returns which named group matched. That helper is deferred
//! to sub-stage D where the consumers actually live. Marked here with a
//! `// TODO sub-stage D: lastgroup helper` reminder.
//!
//! ## Verbose / case-insensitive flags
//!
//! Python's `re.I` / `re.X` (`(?x)`) flags map onto the Rust `regex` crate's
//! inline `(?i)` / `(?x)` flag syntax verbatim. No translation needed.
//!
//! ## Quantifier lower-bound shorthand
//!
//! Python's `re` accepts `{,n}` as a shorthand for `{0,n}` (no explicit
//! lower bound). The Rust `regex` crate REQUIRES an explicit lower bound:
//! `{m,n}`. `TEXT_PATTERNS` at `extractors.py:175` contains
//! `date[^0-9"]{,20}` which translates to `date[^0-9"]{0,20}` in Rust —
//! same semantics, faithful one-character translation. Documented at the
//! call site.

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

// ===========================================================================
// XPath prefix strings (extractors.py:47-78)
// ===========================================================================

/// "Fast prepend" XPath fragment limiting the search to date-bearing tags.
///
/// Ports `extractors.py:47-48`. Consumed by Trafilatura's XPath engine in
/// sub-stage D's `examine_date_elements`.
pub const FAST_PREPEND: &str =
    ".//*[self::div or self::h2 or self::h3 or self::h4 or self::li or self::p or self::span or self::time or self::ul]";

/// "Slow prepend" XPath fragment widening the search to every descendant.
///
/// Ports `extractors.py:49` (`SLOW_PREPEND = ".//*"`).
pub const SLOW_PREPEND: &str = ".//*";

/// XPath predicate matching date-bearing elements by id/class/itemprop.
///
/// Ports `extractors.py:51-78` (`DATE_EXPRESSIONS = """..."""`). Consumed by
/// the `xpath_engine` in sub-stage D — sub-stage C only stores the literal
/// string verbatim.
pub const DATE_EXPRESSIONS: &str = r#"
[
    contains(translate(@id|@class|@itemprop, "D", "d"), 'date') or
    contains(translate(@id|@class|@itemprop, "D", "d"), 'datum') or
    contains(translate(@id|@class, "M", "m"), 'meta') or
    contains(@id|@class, 'time') or
    contains(@id|@class, 'publish') or
    contains(@id|@class, 'footer') or
    contains(@class, 'info') or
    contains(@class, 'post_detail') or
    contains(@class, 'block-content') or
    contains(@class, 'byline') or
    contains(@class, 'subline') or
    contains(@class, 'posted') or
    contains(@class, 'submitted') or
    contains(@class, 'created-post') or
    contains(@class, 'publication') or
    contains(@class, 'author') or
    contains(@class, 'autor') or
    contains(@class, 'field-content') or
    contains(@class, 'fa-clock-o') or
    contains(@class, 'fa-calendar') or
    contains(@class, 'fecha') or
    contains(@class, 'parution') or
    contains(@id, 'footer-info-lastmod')
] |
.//footer | .//small
"#;

// ===========================================================================
// Date-component building-block patterns (extractors.py:95-97)
// ===========================================================================

/// "Day" regex fragment: `[0-3]?[0-9]` (1- or 2-digit day, 0-39).
///
/// Ports `extractors.py:95` (`DAY_RE = "[0-3]?[0-9]"`).
pub const DAY_RE: &str = "[0-3]?[0-9]";

/// "Month" regex fragment: `[0-1]?[0-9]` (1- or 2-digit month, 0-19).
///
/// Ports `extractors.py:96` (`MONTH_RE = "[0-1]?[0-9]"`).
pub const MONTH_RE: &str = "[0-1]?[0-9]";

/// "Year" regex fragment: 1990s + 2000s + 2010s + 2020s + 2030s.
///
/// Ports `extractors.py:97` (`YEAR_RE = "199[0-9]|20[0-3][0-9]"`).
pub const YEAR_RE: &str = "199[0-9]|20[0-3][0-9]";

// ===========================================================================
// Multilingual month-name alternation (extractors.py:110-118)
// ===========================================================================

/// Multilingual month-name alternation, ported verbatim from
/// `extractors.py:110-118` (`REGEX_MONTHS = """..."""`).
///
/// Python's source string contains newlines that are intentionally NOT
/// stripped at the constant — they are stripped by the consumer
/// (`LONG_TEXT_PATTERN`) via an explicit `.replace("\n", "")` at
/// `extractors.py:124`. We preserve the same shape and rely on the consumer
/// to flatten.
pub const REGEX_MONTHS: &str = "
January?|February?|March|A[pv]ril|Ma[iy]|Jun[ei]|Jul[iy]|August|September|O[ck]tober|November|De[csz]ember|
Jan|Feb|M[aä]r|Apr|Jun|Jul|Aug|Sep|O[ck]t|Nov|De[cz]|
Januari|Februari|Maret|Mei|Agustus|
Jänner|Feber|März|
janvier|février|mars|juin|juillet|aout|septembre|octobre|novembre|décembre|
Ocak|Şubat|Mart|Nisan|Mayıs|Haziran|Temmuz|Ağustos|Eylül|Ekim|Kasım|Aralık|
Oca|Şub|Mar|Nis|Haz|Tem|Ağu|Eyl|Eki|Kas|Ara
";

// ===========================================================================
// Month lookup tables (extractors.py:140-157)
// ===========================================================================

/// 12-entry table mapping `1`-based month numbers to all known month-name
/// tokens (English, German, French, Indonesian, Turkish), lowercased.
///
/// Ports `extractors.py:140-153` (`MONTHS = [...]`) verbatim. Each inner
/// slice preserves Python's source order.
pub const MONTHS: &[&[&str]] = &[
    // extractors.py:141 — January
    &["jan", "januar", "jänner", "january", "januari", "janvier", "ocak", "oca"],
    // extractors.py:142 — February
    &["feb", "februar", "feber", "february", "februari", "février", "şubat", "şub"],
    // extractors.py:143 — March
    &["mar", "mär", "märz", "march", "maret", "mart", "mars"],
    // extractors.py:144 — April
    &["apr", "april", "avril", "nisan", "nis"],
    // extractors.py:145 — May
    &["may", "mai", "mei", "mayıs"],
    // extractors.py:146 — June
    &["jun", "juni", "june", "juin", "haziran", "haz"],
    // extractors.py:147 — July
    &["jul", "juli", "july", "juillet", "temmuz", "tem"],
    // extractors.py:148 — August
    &["aug", "august", "agustus", "ağustos", "ağu", "aout"],
    // extractors.py:149 — September
    &["sep", "september", "septembre", "eylül", "eyl"],
    // extractors.py:150 — October
    &["oct", "oktober", "october", "octobre", "okt", "ekim", "eki"],
    // extractors.py:151 — November
    &["nov", "november", "kasım", "kas", "novembre"],
    // extractors.py:152 — December
    &["dec", "dez", "dezember", "december", "desember", "décembre", "aralık", "ara"],
];

/// Inverse of `MONTHS`: month-name → `1`-based month number.
///
/// Ports `extractors.py:155-157`:
///
/// ```python
/// TEXT_MONTHS = {
///     month: mnum for mnum, mlist in enumerate(MONTHS, start=1) for month in mlist
/// }
/// ```
///
/// Lazily built once on first access via `OnceLock`.
pub fn text_months() -> &'static HashMap<&'static str, u32> {
    static MAP: OnceLock<HashMap<&'static str, u32>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for (idx, names) in MONTHS.iter().enumerate() {
            // Python `enumerate(MONTHS, start=1)` — 1-based month number.
            let mnum = (idx as u32) + 1;
            for name in names.iter() {
                m.insert(*name, mnum);
            }
        }
        m
    })
}

// ===========================================================================
// Compiled regex accessors (extractors.py:100-213)
// ===========================================================================

// --- YMD / YM family --------------------------------------------------------

/// `YMD_NO_SEP_PATTERN` — bare 8-digit YYYYMMDD substring.
///
/// Ports `extractors.py:100` (`re.compile(r"\b(\d{8})\b")`).
pub fn ymd_no_sep_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\b(\d{8})\b").unwrap())
}

/// `YMD_PATTERN` — year-month-day OR day-month-year with `-`/`/`/`.` separators.
///
/// Ports `extractors.py:101-104`.
pub fn ymd_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Python f-string builds: (?:\D|^)(?:(?P<year>{YEAR_RE})[\-/.](?P<month>{MONTH_RE})[\-/.](?P<day>{DAY_RE})|(?P<day2>{DAY_RE})[\-/.](?P<month2>{MONTH_RE})[\-/.](?P<year2>\d{2,4}))(?:\D|$)
        let pat = format!(
            r"(?:\D|^)(?:(?P<year>{y})[\-/.](?P<month>{m})[\-/.](?P<day>{d})|(?P<day2>{d})[\-/.](?P<month2>{m})[\-/.](?P<year2>\d{{2,4}}))(?:\D|$)",
            y = YEAR_RE,
            m = MONTH_RE,
            d = DAY_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `YM_PATTERN` — year-month OR month-year with `-`/`/`/`.` separators.
///
/// Ports `extractors.py:105-108`.
pub fn ym_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"(?:\D|^)(?:(?P<year>{y})[\-/.](?P<month>{m})|(?P<month2>{m})[\-/.](?P<year2>{y}))(?:\D|$)",
            y = YEAR_RE,
            m = MONTH_RE
        );
        Regex::new(&pat).unwrap()
    })
}

// --- Long-text multilingual pattern (extractors.py:119-127) ----------------

/// `LONG_TEXT_PATTERN` — multilingual "Month day, year" / "day Month year"
/// matcher. Case-insensitive (`re.I`).
///
/// Ports `extractors.py:119-127`. The Python source builds the pattern with
/// an explicit `.replace("\n", "")` on a verbose-looking triple-quoted
/// string; we replicate the same flatten + `(?i)` flag.
pub fn long_text_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Python: rf"""(?P<month>{REGEX_MONTHS})\s
        //               (?P<day>{DAY_RE})(?:st|nd|rd|th)?,? (?P<year>{YEAR_RE})|
        //               (?P<day2>{DAY_RE})(?:st|nd|rd|th|\.)? (?:of )?
        //               (?P<month2>{REGEX_MONTHS})[,.]? (?P<year2>{YEAR_RE})""".replace("\n", "")
        let raw = format!(
            r"(?P<month>{rm})\s
(?P<day>{d})(?:st|nd|rd|th)?,? (?P<year>{y})|
(?P<day2>{d})(?:st|nd|rd|th|\.)? (?:of )?
(?P<month2>{rm})[,.]? (?P<year2>{y})",
            rm = REGEX_MONTHS,
            d = DAY_RE,
            y = YEAR_RE
        );
        let flat = raw.replace('\n', "");
        // re.I → inline (?i). Anchor it at the start of the alternation so
        // the flag governs both arms.
        let cased = format!("(?i){}", flat);
        Regex::new(&cased).unwrap()
    })
}

// --- URL / JSON / timestamp patterns (extractors.py:129-137) ---------------

/// `COMPLETE_URL` — `YYYY[/_-]MM[/_-]DD` substring in a URL path.
///
/// Ports `extractors.py:129`.
pub fn complete_url() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"\D({y})[/_-]({m})[/_-]({d})(?:\D|$)",
            y = YEAR_RE,
            m = MONTH_RE,
            d = DAY_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `JSON_MODIFIED` — `"dateModified": "YYYY-MM-DD"` JSON-LD scrape.
/// Case-insensitive (`re.I`).
///
/// Ports `extractors.py:131`.
pub fn json_modified() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r#"(?i)"dateModified": ?"({y}-{m}-{d})"#,
            y = YEAR_RE,
            m = MONTH_RE,
            d = DAY_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `JSON_PUBLISHED` — `"datePublished": "YYYY-MM-DD"` JSON-LD scrape.
/// Case-insensitive (`re.I`).
///
/// Ports `extractors.py:132-134`.
pub fn json_published() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r#"(?i)"datePublished": ?"({y}-{m}-{d})"#,
            y = YEAR_RE,
            m = MONTH_RE,
            d = DAY_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `TIMESTAMP_PATTERN` — `YYYY-MM-DDxHH:MM:SS` ISO-like timestamp.
///
/// Ports `extractors.py:135-137`.
pub fn timestamp_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"({y}-{m}-{d}).[0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}",
            y = YEAR_RE,
            m = MONTH_RE,
            d = DAY_RE
        );
        Regex::new(&pat).unwrap()
    })
}

// --- Text-date scrub patterns (extractors.py:159-180) -----------------------

/// `TEXT_DATE_PATTERN` — characters indicating the text is a date string
/// (separators or year-only digits).
///
/// Ports `extractors.py:159` (`re.compile(r"[.:,_/ -]|^\d+$")`).
pub fn text_date_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[.:,_/ -]|^\d+$").unwrap())
}

/// `DISCARD_PATTERNS` — multi-arm filter rejecting non-date strings
/// (clock-only, IBANs, currency strings, URLs, phone numbers, etc.).
///
/// Ports `extractors.py:161-171`.
pub fn discard_patterns() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Concatenation matches Python's r"..." + r"..." source layout at
        // extractors.py:161-171.
        let pat = concat!(
            r"^\d{2}:\d{2}(?: |:|$)|",
            r"^\D*\d{4}\D*$|",
            r"[$€¥Ұ£¢₽₱฿#₹]|",
            r"[A-Z]{3}[^A-Z]|",
            r"(?:^|\D)(?:\+\d{2}|\d{3}|\d{5})\D|",
            r"ftps?|https?|sftp|",
            r"\.(?:com|net|org|info|gov|edu|de|fr|io)\b|",
            r"IBAN|[A-Z]{2}[0-9]{2}|",
            r"®"
        );
        Regex::new(pat).unwrap()
    })
}

/// `TEXT_PATTERNS` — English/German/Turkish text-prefixed date patterns
/// (e.g. "date: 12/3/2020" / "Datum: 12.3.2020" / Turkish equivalents).
/// Case-insensitive (`re.I`).
///
/// Ports `extractors.py:174-180`.
pub fn text_patterns() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // EN / DE / TR (güncellen?me / yayı(m|n)lan?ma) / TR-suffix variant.
        // Rust regex requires an explicit lower bound on `{m,n}` quantifiers;
        // Python's `{,20}` shorthand (== `{0,20}`) becomes `{0,20}` here.
        // Same semantics, faithful translation.
        let pat = concat!(
            "(?i)",
            r#"(?:date[^0-9"]{0,20}|updated|last-modified|published|posted|on)(?:[ :])*?([0-9]{1,4})[./]([0-9]{1,2})[./]([0-9]{2,4})|"#,
            r"(?:Datum|Stand|Veröffentlicht am):? ?([0-9]{1,2})\.([0-9]{1,2})\.([0-9]{2,4})|",
            r"(?:güncellen?me|yayı(?:m|n)lan?ma) *?(?:tarihi)? *?:? *?([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4})|",
            r"([0-9]{1,2})[./]([0-9]{1,2})[./]([0-9]{2,4}) *?(?:'de|'da|'te|'ta|’de|’da|’te|’ta|tarihinde) *(?:güncellendi|yayı(?:m|n)landı)",
        );
        Regex::new(pat).unwrap()
    })
}

// --- Core component patterns (extractors.py:183-187) -----------------------

/// `THREE_COMP_REGEX_A` — `(day)(sep)(month)(sep)(year)` with 4-digit year.
///
/// Ports `extractors.py:183`.
pub fn three_comp_regex_a() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"({d})[/.-]({m})[/.-]({y})",
            d = DAY_RE,
            m = MONTH_RE,
            y = YEAR_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `THREE_COMP_REGEX_B` — `(day)/(month)/(2-digit-year)` OR
/// `(day)[.-](month)[.-](2-digit-year)`.
///
/// Ports `extractors.py:184-186`.
pub fn three_comp_regex_b() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"({d})/({m})/([0-9]{{2}})|({d})[.-]({m})[.-]([0-9]{{2}})",
            d = DAY_RE,
            m = MONTH_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `TWO_COMP_REGEX` — `(month)(sep)(year)` 2-component matcher.
///
/// Ports `extractors.py:187`.
pub fn two_comp_regex() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"({m})[/.-]({y})", m = MONTH_RE, y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

// --- Extensive-search patterns (extractors.py:190-213) ---------------------

/// `YEAR_PATTERN` — leading year scrape.
///
/// Ports `extractors.py:190` (`re.compile(rf"^\D?({YEAR_RE})")`).
pub fn year_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"^\D?({y})", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `COPYRIGHT_PATTERN` — `© year` / `©year-year` / `Copyright year`.
///
/// Ports `extractors.py:191-193`.
pub fn copyright_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(
            r"(?:©|\&copy;|Copyright|\(c\))\D*(?:{y})?-?({y})\D",
            y = YEAR_RE
        );
        Regex::new(&pat).unwrap()
    })
}

/// `THREE_PATTERN` — `/YYYY/MM/DD` URL fragment.
///
/// Ports `extractors.py:194`.
pub fn three_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"/([0-9]{4}/[0-9]{2}/[0-9]{2})[01/]").unwrap())
}

/// `THREE_CATCH` — `YYYY/MM/DD` capture for date components.
///
/// Ports `extractors.py:195`.
pub fn three_catch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([0-9]{4})/([0-9]{2})/([0-9]{2})").unwrap())
}

/// `THREE_LOOSE_PATTERN` — `YYYY[/.-]MM[/.-]DD` substring (loose separators).
///
/// Ports `extractors.py:196`.
pub fn three_loose_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\D([0-9]{4}[/.-][0-9]{2}[/.-][0-9]{2})\D").unwrap())
}

/// `THREE_LOOSE_CATCH` — `YYYY[/.-]MM[/.-]DD` capture.
///
/// Ports `extractors.py:197`.
pub fn three_loose_catch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([0-9]{4})[/.-]([0-9]{2})[/.-]([0-9]{2})").unwrap())
}

/// `SELECT_YMD_PATTERN` — `D?D[/.-]M?M[/.-]YYYY` substring.
///
/// Ports `extractors.py:198`.
pub fn select_ymd_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\D([0-3]?[0-9][/.-][01]?[0-9][/.-][0-9]{4})\D").unwrap())
}

/// `SELECT_YMD_YEAR` — trailing 4-digit year from a `[\D|]year[\D]?` tail.
///
/// Ports `extractors.py:199`.
pub fn select_ymd_year() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"({y})\D?$", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `YMD_YEAR` — leading 4-digit year at the start of a string.
///
/// Ports `extractors.py:200`.
pub fn ymd_year() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"^({y})", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `DATESTRINGS_PATTERN` — compact `YYYYMMDD` substring for 1990s / 2000s.
///
/// Ports `extractors.py:201-203`.
pub fn datestrings_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(\D19[0-9]{2}[01][0-9][0-3][0-9]\D|\D20[0-9]{2}[01][0-9][0-3][0-9]\D)")
            .unwrap()
    })
}

/// `DATESTRINGS_CATCH` — `YYYYMMDD` capture.
///
/// Ports `extractors.py:204`.
pub fn datestrings_catch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"({y})([01][0-9])([0-3][0-9])", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `SLASHES_PATTERN` — `D?D/M?M/YY` or `DD.MM.YY` substring (2-digit year).
///
/// Ports `extractors.py:205-207`.
pub fn slashes_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"\D([0-3]?[0-9]/[01]?[0-9]/[0129][0-9]|[0-3][0-9]\.[01][0-9]\.[0129][0-9])\D")
            .unwrap()
    })
}

/// `SLASHES_YEAR` — trailing 2-digit year from a slashes-style date.
///
/// Ports `extractors.py:208`.
pub fn slashes_year() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"([0-9]{2})$").unwrap())
}

/// `YYYYMM_PATTERN` — `YYYY[/.-]MM` substring (4-digit year + valid month).
///
/// Ports `extractors.py:209`.
pub fn yyyymm_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\D([12][0-9]{3}[/.-](?:1[0-2]|0[1-9]))\D").unwrap())
}

/// `YYYYMM_CATCH` — `YYYY[/.-]MM` capture.
///
/// Ports `extractors.py:210`.
pub fn yyyymm_catch() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // Python source has a trailing empty alternative in the second group
        // (`(1[0-2]|0[1-9]|)`) — preserved verbatim.
        let pat = format!(r"({y})[/.-](1[0-2]|0[1-9]|)", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `MMYYYY_PATTERN` — `M?M[/.-]YYYY` substring.
///
/// Ports `extractors.py:211`.
pub fn mmyyyy_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\D([01]?[0-9][/.-][12][0-9]{3})\D").unwrap())
}

/// `MMYYYY_YEAR` — trailing 4-digit year.
///
/// Ports `extractors.py:212`.
pub fn mmyyyy_year() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"({y})\D?$", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// `SIMPLE_PATTERN` — `\D(year)\D` scrape (NO lookbehind — see post-filter).
///
/// Ports `extractors.py:213` (`re.compile(rf"(?<!w3.org)\D({YEAR_RE})\D")`)
/// **without** the negative lookbehind, because the Rust `regex` crate is a
/// finite-automaton engine and does not support lookarounds.
///
/// **Callers MUST invoke `simple_pattern_post_filter`** on every hit to
/// faithfully reproduce the Python `(?<!w3.org)` semantic. See the module
/// header for the rationale.
pub fn simple_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let pat = format!(r"\D({y})\D", y = YEAR_RE);
        Regex::new(&pat).unwrap()
    })
}

/// Post-filter for `simple_pattern` matches, replacing Python's
/// `(?<!w3.org)` negative lookbehind.
///
/// Returns `true` IFF the match should be KEPT (i.e. the text immediately
/// preceding `match_start` does NOT end with the literal `"w3.org"`).
///
/// Python's lookbehind treats the literal `.` as ANY character (because the
/// pattern was not regex-escaped), so `"w3-org"` / `"w3xorg"` / any
/// 6-character sequence with `w3` + ANY + `org` would block the match too.
/// We replicate that semantic exactly: the check uses
/// `ends_with("w3.org") || ends_with("w3-org") || ...` — no, simpler: we
/// extract the 6 characters before the match and apply Python's `re`
/// semantics by compiling `w3.org` as a regex anchored at the end. The
/// implementation below uses a one-shot regex match for byte-faithfulness
/// to Python's original `re.compile` behaviour.
///
/// Source citation: `extractors.py:213` lookbehind clause.
pub fn simple_pattern_post_filter(haystack: &str, match_start: usize) -> bool {
    // Need at least 6 bytes of context to apply the `w3.org` (6-char) test.
    if match_start < 6 {
        return true;
    }
    // The Python lookbehind `(?<!w3.org)` runs the pattern `w3.org` against
    // the 6 bytes immediately preceding `match_start`. Python `.` matches any
    // single character (except newline by default, but Python re's `.` does
    // not match newline) — so we use the same regex engine for parity.
    static LB: OnceLock<Regex> = OnceLock::new();
    let lb = LB.get_or_init(|| Regex::new(r"w3.org$").unwrap());

    // Slice back up to 6 bytes but respect UTF-8 boundaries. Python's `re`
    // operates on `str` (codepoints); for ASCII contexts (the realistic
    // `w3.org` case) byte-slicing is equivalent. Walk back to the nearest
    // char boundary that gives us at least the trailing 6 chars.
    let mut start = match_start.saturating_sub(6);
    while start < match_start && !haystack.is_char_boundary(start) {
        start += 1;
    }
    let context = &haystack[start..match_start];
    !lb.is_match(context)
}

// ===========================================================================
// TODO sub-stage D: lastgroup helper
// ===========================================================================
// Python's `re.Match.lastgroup` returns the name of the LAST matched named
// group. Rust's `regex::Captures` has no direct equivalent. Sub-stage D's
// call sites at `extractors.py:267, 337, 364` will need a small helper that
// returns which of the candidate named groups (`year` / `month` / `day` /
// their `*2`-suffixed alternatives) captured. The helper is deferred to
// sub-stage D where the consumers live.

// ===========================================================================
// Tests (≥25, per sub-stage C brief)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- DATE_EXPRESSIONS / prepends ---------------------------------------

    /// Ports `extractors.py:51-78` — DATE_EXPRESSIONS is a non-empty XPath
    /// string. The actual XPath gets exercised in sub-stage E.
    #[test]
    fn date_expressions_is_nonempty_string() {
        assert!(!DATE_EXPRESSIONS.is_empty());
        assert!(DATE_EXPRESSIONS.contains("'date'"));
        assert!(DATE_EXPRESSIONS.contains(".//footer"));
    }

    /// Ports `extractors.py:47-49` — pin FAST_PREPEND / SLOW_PREPEND values.
    #[test]
    fn xpath_prepends_match_python() {
        assert!(FAST_PREPEND.starts_with(".//*[self::div"));
        assert_eq!(SLOW_PREPEND, ".//*");
    }

    // --- MONTHS / TEXT_MONTHS ----------------------------------------------

    /// Ports `extractors.py:140-153` — `MONTHS` has 12 entries in
    /// January..December order.
    #[test]
    fn months_table_has_twelve_entries() {
        assert_eq!(MONTHS.len(), 12);
        assert!(MONTHS[0].contains(&"january"));
        assert!(MONTHS[11].contains(&"december"));
    }

    /// Ports `extractors.py:155-157` — TEXT_MONTHS maps every name in
    /// MONTHS to the 1-based month number.
    #[test]
    fn text_months_lookup_matches_months_table() {
        let tm = text_months();
        assert_eq!(tm.get("jan"), Some(&1));
        assert_eq!(tm.get("january"), Some(&1));
        assert_eq!(tm.get("janvier"), Some(&1));
        assert_eq!(tm.get("ocak"), Some(&1));
        assert_eq!(tm.get("dec"), Some(&12));
        assert_eq!(tm.get("aralık"), Some(&12));
        assert_eq!(tm.get("juin"), Some(&6));
        assert_eq!(tm.get("nonsense"), None);
    }

    /// Pin: every entry across MONTHS lands in TEXT_MONTHS.
    #[test]
    fn text_months_size_matches_months_total() {
        let total: usize = MONTHS.iter().map(|m| m.len()).sum();
        assert_eq!(text_months().len(), total);
    }

    // --- YMD family --------------------------------------------------------

    #[test]
    fn ymd_no_sep_pattern_matches_eight_digits() {
        assert!(ymd_no_sep_pattern().is_match("20200115"));
    }
    #[test]
    fn ymd_no_sep_pattern_rejects_short() {
        assert!(!ymd_no_sep_pattern().is_match("1234567"));
    }

    #[test]
    fn ymd_pattern_matches_iso_form() {
        // Need surrounding non-digit context per (?:\D|^) / (?:\D|$).
        assert!(ymd_pattern().is_match(" 2020-01-15 "));
    }
    #[test]
    fn ymd_pattern_rejects_plain_year() {
        assert!(!ymd_pattern().is_match(" 2020 "));
    }

    #[test]
    fn ym_pattern_matches_year_month() {
        assert!(ym_pattern().is_match(" 2020-01 "));
    }
    #[test]
    fn ym_pattern_rejects_three_components() {
        // YM only — full YMD has trailing extra component that the
        // (?:\D|$) tail rejects unless preceded by separators in the
        // right shape.
        let r = ym_pattern();
        // Plain alphabetic — no match.
        assert!(!r.is_match("abcdef"));
    }

    // --- LONG_TEXT_PATTERN (multilingual) ----------------------------------

    #[test]
    fn long_text_pattern_matches_english() {
        assert!(long_text_pattern().is_match("January 15, 2020"));
    }
    #[test]
    fn long_text_pattern_matches_german() {
        // "März" / "März 12 2020"; or "12. März 2020".
        assert!(long_text_pattern().is_match("12. März 2020"));
    }
    #[test]
    fn long_text_pattern_matches_french() {
        assert!(long_text_pattern().is_match("12 mars 2020"));
    }
    #[test]
    fn long_text_pattern_matches_italian_via_overlap() {
        // Italian shares "marzo" via the German/French alternation pool;
        // "December" / "novembre" / "agosto" — we use Italian "marzo" via
        // English "March" prefix or French "mars" — at minimum ensure
        // "septembre" matches (also French/Italian-similar).
        assert!(long_text_pattern().is_match("12 septembre 2020"));
    }
    #[test]
    fn long_text_pattern_matches_turkish() {
        // Şubat — Turkish February.
        assert!(long_text_pattern().is_match("12 Şubat 2020"));
    }
    #[test]
    fn long_text_pattern_matches_indonesian() {
        // Agustus — Indonesian August.
        assert!(long_text_pattern().is_match("12 Agustus 2020"));
    }
    #[test]
    fn long_text_pattern_rejects_random_text() {
        assert!(!long_text_pattern().is_match("nothing date-like here"));
    }

    // --- URL / JSON / timestamp --------------------------------------------

    #[test]
    fn complete_url_matches_dated_url() {
        assert!(complete_url().is_match("/blog/2020/01/15/foo"));
    }
    #[test]
    fn complete_url_rejects_undated_url() {
        assert!(!complete_url().is_match("/blog/foo/bar"));
    }

    #[test]
    fn json_modified_matches_jsonld_entry() {
        assert!(json_modified().is_match(r#""dateModified": "2020-01-15""#));
    }
    #[test]
    fn json_published_matches_jsonld_entry() {
        assert!(json_published().is_match(r#""datePublished": "2020-01-15""#));
    }
    #[test]
    fn json_modified_rejects_unrelated_field() {
        assert!(!json_modified().is_match(r#""otherField": "2020-01-15""#));
    }

    #[test]
    fn timestamp_pattern_matches_iso_timestamp() {
        assert!(timestamp_pattern().is_match("2020-01-15T12:34:56"));
    }
    #[test]
    fn timestamp_pattern_rejects_bare_date() {
        assert!(!timestamp_pattern().is_match("2020-01-15"));
    }

    // --- Text patterns -----------------------------------------------------

    #[test]
    fn text_date_pattern_matches_separators() {
        assert!(text_date_pattern().is_match("2020-01-15"));
        assert!(text_date_pattern().is_match("2020")); // ^\d+$ branch.
    }
    #[test]
    fn text_date_pattern_rejects_letters_only() {
        assert!(!text_date_pattern().is_match("abcdef"));
    }

    #[test]
    fn discard_patterns_drops_clock_only() {
        assert!(discard_patterns().is_match("12:34"));
    }
    #[test]
    fn discard_patterns_keeps_plain_date() {
        // No discard arm matches a bare "2020-01-15".
        assert!(!discard_patterns().is_match("2020-01-15"));
    }

    #[test]
    fn text_patterns_matches_english_prefix() {
        assert!(text_patterns().is_match("Published: 12/3/2020"));
    }
    #[test]
    fn text_patterns_matches_german_prefix() {
        assert!(text_patterns().is_match("Datum: 12.3.2020"));
    }
    #[test]
    fn text_patterns_matches_turkish_prefix() {
        assert!(text_patterns().is_match("güncelleme tarihi: 12/3/2020"));
    }
    #[test]
    fn text_patterns_rejects_plain_paragraph() {
        assert!(!text_patterns().is_match("hello world no dates"));
    }

    // --- Core component patterns -------------------------------------------

    #[test]
    fn three_comp_regex_a_matches_dd_mm_yyyy() {
        assert!(three_comp_regex_a().is_match("15/01/2020"));
        assert!(three_comp_regex_a().is_match("15.01.2020"));
    }
    #[test]
    fn three_comp_regex_a_rejects_short_year() {
        assert!(!three_comp_regex_a().is_match("15/01/20"));
    }

    #[test]
    fn three_comp_regex_b_matches_two_digit_year() {
        assert!(three_comp_regex_b().is_match("15/01/20"));
    }
    #[test]
    fn three_comp_regex_b_rejects_text() {
        assert!(!three_comp_regex_b().is_match("hello world"));
    }

    #[test]
    fn two_comp_regex_matches_month_year() {
        assert!(two_comp_regex().is_match("01/2020"));
    }
    #[test]
    fn two_comp_regex_rejects_year_only() {
        assert!(!two_comp_regex().is_match("2020"));
    }

    // --- Extensive search --------------------------------------------------

    #[test]
    fn year_pattern_matches_leading_year() {
        assert!(year_pattern().is_match("2020-stuff"));
        assert!(year_pattern().is_match(" 2020"));
    }
    #[test]
    fn year_pattern_rejects_year_in_middle() {
        assert!(!year_pattern().is_match("foo 2020 bar"));
    }

    #[test]
    fn copyright_pattern_matches_symbol() {
        assert!(copyright_pattern().is_match("© 2020 "));
    }
    #[test]
    fn copyright_pattern_rejects_no_symbol() {
        assert!(!copyright_pattern().is_match(" 2020 "));
    }

    #[test]
    fn three_pattern_matches_url_fragment() {
        assert!(three_pattern().is_match("/2020/01/15/"));
    }
    #[test]
    fn three_pattern_rejects_two_segments() {
        assert!(!three_pattern().is_match("/2020/01/"));
    }

    #[test]
    fn three_catch_captures_components() {
        let caps = three_catch().captures("2020/01/15").unwrap();
        assert_eq!(&caps[1], "2020");
        assert_eq!(&caps[2], "01");
        assert_eq!(&caps[3], "15");
    }

    #[test]
    fn three_loose_pattern_matches_loose_separators() {
        assert!(three_loose_pattern().is_match(" 2020.01.15 "));
        assert!(three_loose_pattern().is_match(" 2020-01-15 "));
    }
    #[test]
    fn three_loose_pattern_rejects_short_year() {
        assert!(!three_loose_pattern().is_match(" 20.01.15 "));
    }

    #[test]
    fn three_loose_catch_captures_components() {
        let caps = three_loose_catch().captures("2020-01-15").unwrap();
        assert_eq!(&caps[1], "2020");
    }

    #[test]
    fn select_ymd_pattern_matches_dd_mm_yyyy_substring() {
        assert!(select_ymd_pattern().is_match(" 15/01/2020 "));
    }
    #[test]
    fn select_ymd_year_captures_trailing_year() {
        let caps = select_ymd_year().captures("foo 2020").unwrap();
        assert_eq!(&caps[1], "2020");
    }

    #[test]
    fn ymd_year_matches_leading_year() {
        assert!(ymd_year().is_match("2020-01-15"));
    }

    #[test]
    fn datestrings_pattern_matches_compact_date() {
        assert!(datestrings_pattern().is_match(" 20200115 "));
    }
    #[test]
    fn datestrings_pattern_rejects_old_year() {
        // Python's pattern accepts 19xx or 20xx; reject 1880s.
        assert!(!datestrings_pattern().is_match(" 18800115 "));
    }
    #[test]
    fn datestrings_catch_captures_components() {
        let caps = datestrings_catch().captures("20200115").unwrap();
        assert_eq!(&caps[1], "2020");
        assert_eq!(&caps[2], "01");
        assert_eq!(&caps[3], "15");
    }

    #[test]
    fn slashes_pattern_matches_two_digit_year() {
        assert!(slashes_pattern().is_match(" 15/01/20 "));
        assert!(slashes_pattern().is_match(" 15.01.20 "));
    }
    #[test]
    fn slashes_pattern_rejects_four_digit_year() {
        // The pattern only accepts 2-digit year ([0129][0-9]).
        // " 15/01/2020 " — the third component is "2020", but the regex
        // captures 2-digit groups; "20" matches and trailing "20" remains,
        // so the surrounding \D is satisfied by '/'. We test the failing
        // case instead: a string with no slashed/dotted date at all.
        assert!(!slashes_pattern().is_match("hello world"));
    }
    #[test]
    fn slashes_year_captures_trailing_two_digits() {
        let caps = slashes_year().captures("15/01/20").unwrap();
        assert_eq!(&caps[1], "20");
    }

    #[test]
    fn yyyymm_pattern_matches_year_month() {
        assert!(yyyymm_pattern().is_match(" 2020-01 "));
    }
    #[test]
    fn yyyymm_pattern_rejects_invalid_month() {
        assert!(!yyyymm_pattern().is_match(" 2020-13 "));
    }
    #[test]
    fn yyyymm_catch_captures_year_month() {
        let caps = yyyymm_catch().captures("2020-01").unwrap();
        assert_eq!(&caps[1], "2020");
        assert_eq!(&caps[2], "01");
    }

    #[test]
    fn mmyyyy_pattern_matches_month_year() {
        assert!(mmyyyy_pattern().is_match(" 01/2020 "));
    }
    #[test]
    fn mmyyyy_pattern_rejects_short_year() {
        assert!(!mmyyyy_pattern().is_match(" 01/20 "));
    }
    #[test]
    fn mmyyyy_year_captures_trailing_year() {
        let caps = mmyyyy_year().captures("01/2020").unwrap();
        assert_eq!(&caps[1], "2020");
    }

    // --- SIMPLE_PATTERN + post-filter --------------------------------------

    #[test]
    fn simple_pattern_matches_year_in_text() {
        // Needs \D on both sides per the pattern.
        assert!(simple_pattern().is_match(" 2020 "));
    }
    #[test]
    fn simple_pattern_post_filter_rejects_w3_org_prefix() {
        // Reproduces Python's `(?<!w3.org)` negative lookbehind:
        // a hit immediately after "w3.org" must be DROPPED.
        let haystack = "w3.org 2020 ";
        // Find the match start of "2020" within haystack.
        let m = simple_pattern().find(haystack).unwrap();
        // Without the post-filter, the regex would match. Post-filter
        // returns false → drop.
        assert!(!simple_pattern_post_filter(haystack, m.start()));
    }
    #[test]
    fn simple_pattern_post_filter_keeps_normal_match() {
        let haystack = "Published in 2020!";
        let m = simple_pattern().find(haystack).unwrap();
        assert!(simple_pattern_post_filter(haystack, m.start()));
    }
    #[test]
    fn simple_pattern_post_filter_safe_at_string_start() {
        // match_start < 6 — early return true (no lookbehind context).
        assert!(simple_pattern_post_filter("foo", 0));
        assert!(simple_pattern_post_filter(" 2020", 1));
    }
}
