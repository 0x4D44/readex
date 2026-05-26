//! `parse_int.rs` — JS-faithful `parseInt(s, 10)` (HLD §9 / M-6).
//!
//! Used by `_getRowAndColumnCount` (`Readability.js:2240-2264`) to coerce
//! `rowspan` / `colspan` attribute strings into integers exactly as the JS
//! `parseInt(s, 10)` does. The JS path is:
//!
//! ```text
//! var rowspan = trs[i].getAttribute("rowspan") || 0;
//! if (rowspan) { rowspan = parseInt(rowspan, 10); }
//! rows += rowspan || 1;
//! ```
//!
//! `getAttribute("rowspan") || 0` is `"<value>"` if present (a truthy string
//! even if e.g. `"0"`, because non-empty strings are truthy in JS) else the
//! integer `0`. The `if (rowspan)` then guards `parseInt`; an empty string is
//! falsy so it is NOT parsed (stays the integer `0`); a `0` literal is also
//! falsy so it is also NOT parsed. After parsing, `rowspan || 1` uses `1`
//! when `rowspan` is `0`, `NaN`, or `""` (all JS-falsy).
//!
//! [`parse_int_js`] returns `Option<i64>` — `Some(n)` for a successful parse
//! (the `parseInt` numeric result), `None` for JS `NaN` (no digits in the
//! valid prefix). Callers reproduce the JS `|| 0` / `|| 1` defaults explicitly
//! via [`row_or_col_span_or_one`].

use crate::readability::dom::is_js_space;

/// JS `parseInt(s, 10)` (ECMA-262 §21.1.3.18 — `Number.parseInt`).
///
/// Algorithm:
/// 1. **Skip leading whitespace** — every leading `is_js_space` char is
///    discarded (the ECMA-262 `StrWhiteSpace` set: same as JS `\s`, includes
///    NBSP / ZWNBSP / `\v`).
/// 2. **Optional sign** — a single `+` or `-` is consumed.
/// 3. **Longest valid digit prefix** with radix 10 — `[0-9]+`. Hex prefixes
///    (`0x`/`0X`) are NOT special here because the explicit radix is `10`;
///    `0x10` parses as `"0"` → `0` (the `x` is non-digit, ends the prefix).
/// 4. **Empty prefix ⇒ NaN** — zero leading digit chars (after sign) ⇒
///    `None`. Otherwise the digit-prefix is parsed as a `i64` with the sign
///    applied.
///
/// Trailing garbage is ignored (`"12abc"` → `12`). Underflow/overflow is
/// saturating (an attribute value `"99999999999999999999"` would exceed
/// `i64::MAX`; `_getRowAndColumnCount` compares the result against small
/// constants like `1`/`4`/`10`, so the column/row count saturating at
/// `i64::MAX` is sound — the test outcome is identical, the cell never wins
/// the "single col/row" or "rows*columns > 10" branches with anything less
/// than a fantastical value).
///
/// Pinned by an exhaustive table in this module's tests.
pub fn parse_int_js(s: &str) -> Option<i64> {
    let mut it = s.chars().peekable();

    // (1) Skip leading whitespace.
    while let Some(&c) = it.peek() {
        if is_js_space(c) {
            it.next();
        } else {
            break;
        }
    }

    // (2) Optional sign.
    let sign: i64 = match it.peek() {
        Some('+') => {
            it.next();
            1
        }
        Some('-') => {
            it.next();
            -1
        }
        _ => 1,
    };

    // (3) Longest valid digit prefix (radix 10).
    let mut value: i64 = 0;
    let mut has_digit = false;
    while let Some(&c) = it.peek() {
        let Some(d) = c.to_digit(10) else { break };
        it.next();
        has_digit = true;
        // Saturating to i64::MAX — the JS would return a Number losing
        // precision past 2^53, then comparisons against small ints still
        // succeed in the same direction. Saturating is the simplest, most
        // faithful behavior on the comparison axis _getRowAndColumnCount
        // uses (the result is only compared against 1/4/10 etc.).
        value = value.saturating_mul(10).saturating_add(d as i64);
    }

    // (4) No digits ⇒ NaN ⇒ None.
    if !has_digit {
        return None;
    }
    Some(sign.saturating_mul(value))
}

/// `attr_or_zero || 0` then `if (truthy) parseInt(_, 10)` then `_ || 1` —
/// the EXACT JS `rowspan`/`colspan` coercion (`Readability.js:2244-2249` and
/// `:2254-2259`). Returns the integer to add as rows / columns-in-this-row.
///
/// JS reference:
/// ```text
/// var rowspan = trs[i].getAttribute("rowspan") || 0;
/// if (rowspan) { rowspan = parseInt(rowspan, 10); }
/// rows += rowspan || 1;
/// ```
///
/// Equivalences:
/// - attribute absent ⇒ `getAttribute` returns `null`, `null || 0 = 0`,
///   `if (0)` is false (no parseInt), then `0 || 1 = 1`.
/// - attribute `""` (empty) ⇒ truthy as a string only if non-empty;
///   `"" || 0 = 0` (empty string is falsy in JS) ⇒ no parseInt, `0 || 1 = 1`.
/// - attribute `"0"` ⇒ truthy as a string (non-empty), parsed → `0`,
///   `0 || 1 = 1`.
/// - attribute `"2"` ⇒ truthy, parsed → `2`, `2 || 1 = 2`.
/// - attribute `"abc"` ⇒ truthy, parsed → `NaN`, `NaN || 1 = 1`.
/// - attribute `"-3"` ⇒ truthy, parsed → `-3`, `-3 || 1 = -3` (negative is
///   truthy in JS).
pub fn row_or_col_span_or_one(attr: Option<&str>) -> i64 {
    // attr_or_zero: empty string AND absent both collapse to "no parseInt"
    // because JS `"" || 0 = 0` and `null || 0 = 0` are both 0 (a number,
    // falsy ⇒ no parseInt), and the JS-style `rowspan || 1` is `0 || 1 = 1`.
    let Some(s) = attr.filter(|s| !s.is_empty()) else {
        return 1;
    };

    // parseInt(rowspan, 10). NaN result ⇒ rowspan stays NaN ⇒ `NaN || 1 = 1`.
    let parsed = parse_int_js(s);
    match parsed {
        Some(0) | None => 1,
        Some(n) => n,
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    /// Every expected value hand-derived from the ECMA-262 `parseInt`
    /// algorithm + the JS truthy/falsy rules (NOT by running JS — that would
    /// be an oracle dependency the unit test must NOT have).
    #[test]
    fn parse_int_js_conformance_table() {
        // (input, expected) — `None` ≡ NaN.
        let rows: &[(&str, Option<i64>)] = &[
            // empty / whitespace only
            ("", None),
            (" ", None),
            ("  \t\n", None),
            ("\u{FEFF}", None),
            // single digits and small ints
            ("0", Some(0)),
            ("1", Some(1)),
            ("9", Some(9)),
            ("12", Some(12)),
            ("123", Some(123)),
            // signs
            ("+5", Some(5)),
            ("-3", Some(-3)),
            ("+0", Some(0)),
            ("-0", Some(0)),
            ("--3", None), // second '-' is non-digit; sign already consumed, no digit yet
            ("++3", None),
            // leading whitespace
            ("  42", Some(42)),
            ("\t-7", Some(-7)),
            // leading whitespace + sign + digits
            ("\u{00A0}+11", Some(11)), // NBSP is JS \s
            ("\u{FEFF}9", Some(9)),    // ZWNBSP is JS \s
            // trailing garbage ignored
            ("12abc", Some(12)),
            ("3 spaces", Some(3)),
            ("9.5", Some(9)), // '.' stops the prefix at radix 10
            // pure garbage
            ("abc", None),
            ("+abc", None),
            ("-abc", None),
            // hex prefix is NOT special at radix 10
            ("0x10", Some(0)),
            ("0X10", Some(0)),
            // very large ints — saturating to i64::MAX (fine for the
            // downstream comparisons against 1/4/10).
            ("99999999999999999999999", Some(i64::MAX)),
            // edge: leading zeros
            ("00042", Some(42)),
            ("-007", Some(-7)),
        ];
        for (s, want) in rows {
            assert_eq!(parse_int_js(s), *want, "parse_int_js({s:?})");
        }
    }

    /// `row_or_col_span_or_one` matches the EXACT JS three-step coercion in
    /// `Readability.js:2244-2249`. Hand-derived per case.
    #[test]
    fn row_or_col_span_or_one_conformance() {
        // (attr, want)
        let rows: &[(Option<&str>, i64)] = &[
            // absent ⇒ getAttribute null ⇒ null || 0 = 0 ⇒ no parseInt ⇒ 0 || 1 = 1
            (None, 1),
            // empty ⇒ "" || 0 = 0 ⇒ no parseInt ⇒ 0 || 1 = 1
            (Some(""), 1),
            // "0" ⇒ truthy string ⇒ parsed 0 ⇒ 0 || 1 = 1
            (Some("0"), 1),
            // "1"/"2"/... ⇒ parsed value
            (Some("1"), 1),
            (Some("2"), 2),
            (Some("10"), 10),
            // garbage ⇒ NaN ⇒ NaN || 1 = 1
            (Some("abc"), 1),
            (Some("nan"), 1),
            // signed
            (Some("+5"), 5),
            // negative ⇒ truthy in JS ⇒ used as-is (the `|| 1` doesn't fire)
            (Some("-3"), -3),
            // trailing garbage
            (Some("4 spans"), 4),
            // leading whitespace ok
            (Some("   7"), 7),
        ];
        for (attr, want) in rows {
            assert_eq!(
                row_or_col_span_or_one(*attr),
                *want,
                "row_or_col_span_or_one({attr:?})"
            );
        }
    }

    /// The exact path `_getRowAndColumnCount` follows for one `<tr>`'s
    /// `rowspan`. Pinned with `parse_int_js` + the `|| 1` default in series,
    /// since this is the load-bearing combination.
    #[test]
    fn rowspan_zero_string_treated_as_one() {
        // attr "0" must become 1 (the `0 || 1` step).
        assert_eq!(row_or_col_span_or_one(Some("0")), 1);
    }

    #[test]
    fn rowspan_nan_string_treated_as_one() {
        assert_eq!(row_or_col_span_or_one(Some("nope")), 1);
    }
}
