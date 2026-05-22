# DEFERRED — 8d5cc524 htmldate extensive-search date discovery

**Fixture:** `8d5cc5247b273722.html` (SEC press release, "Mark Uyeda Sworn in for
Second Term as SEC Commissioner")
**Gate:** `tests/trafilatura_tei_gate.rs` (M8 — `<teiHeader>` `<date>`/`<bibl>`).
**Bucket:** `DEFERRED_KNOWN_DEFECT` — **mdrcel is the wrong side**; fix is blocked
on a deep, under-gated htmldate-port change.

## Divergence

| field | Python | mdrcel |
|---|---|---|
| `<date>` (and the date in `<bibl>`/`<bibl type="sigle">`) | `2024-01-03` | `2009-12-16` |

Everything else in the `<teiHeader>` (and the whole `<text>` body) is byte-identical.

## Root cause (verified against vendored htmldate 1.9.4)

The page contains the article date only as the text `Jan. 3, 2024` (in
`<p>Washington D.C., Jan. 3, 2024 — </p>` and `<span class="nowrap">Jan. 3,
2024</span>`). It also contains a string `…/numeric-2009-12-16.xsd` inside a
`<script>` JSON config blob (`"extExclude":"…|http://www.xbrl.org/dtr/type/numeric-2009-12-16.xsd|…"`).

Confirmed against the live oracle:
- `find_date(html, extensive_search=False)` → `None` (both `original_date` values).
- `find_date(html, extensive_search=True, original_date=True)` → `2024-01-03`.
- `find_date("<p>…numeric-2009-12-16.xsd</p>", extensive_search=True)` → `2009-12-16`
  (so Python's regex *does* match the XBRL string in isolation).

So both dates are reachable only in htmldate's **extensive** path, and Python's
cascade returns `2024-01-03` from an *earlier* extensive sub-stage (the
`SLOW_PREPEND + DATE_EXPRESSIONS` element walk, `core.py:922-941`, which finds
`Jan. 3, 2024` in the lead-in `<p>`/`<span>`), returning **before** the
whole-page free-text scan (`search_page`/`select_candidate`, `core.py:970-981`)
that would surface `2009-12-16`. mdrcel's port fails to extract `Jan. 3, 2024`
in that earlier element-walk stage and falls through to its `extensive_search`
last resort, which matches the XBRL `2009-12-16` in the serialized page string.

`htmldate`'s `CLEANING_LIST` (settings.py:21-39) does **not** include `<script>`,
so script-blob removal is not the lever; the divergence is in the extensive
**element-walk vs whole-page-scan ordering / month-abbreviation extraction**.

## Why deferred (not fixed here)

1. **mdrcel is wrong**, so this is not an allowlist case.
2. The fix lives in the htmldate port's extensive `examine_date_elements`
   (`SLOW_PREPEND + DATE_EXPRESSIONS` selection + month-abbreviation text
   parsing) — a deep change in a ~3000 LOC port.
3. The only htmldate safety net, `tests/htmldate_parity_gate.rs`, exercises
   **`original_date=False`** over 10 fixtures; the metadata path uses
   `original_date=True`, which is comparatively under-tested. A blind change to
   the extensive stage risks silent regressions there.
4. It is **one fixture out of 51**. The M8 general-rule fixes brought every other
   substantive fixture's `<teiHeader>` to byte-parity; deferring this single
   date-discovery edge keeps the gate substantive and GREEN.

## What would unblock it

A focused htmldate-port hardening pass: broaden the htmldate parity gate to cover
`original_date=True` + extensive search, then align mdrcel's
`examine_date_elements` extensive element-walk (and its month-abbreviation
`LONG_TEXT_PATTERN` application order) with Python so the article's `Jan. 3, 2024`
is found before the whole-page scan. Track as an htmldate-parity follow-up.
