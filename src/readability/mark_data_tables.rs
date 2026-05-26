//! `mark_data_tables.rs` — `_markDataTables` (`Readability.js:2271-2329`) and
//! `_getRowAndColumnCount` (`Readability.js:2240-2264`).
//!
//! **Stage 2 scope (HLD §7.4).** Faithful transcription with line cites
//! (anti-inversion, HLD §4.3(a)). Sets the `_readabilityDataTable` flag on
//! every `<table>` descendant of `root` (which is `articleContent` per
//! `Readability.js:788` `this._markDataTables(articleContent)`).
//!
//! The flag is consumed by `_cleanConditionally("table")` and the
//! `_hasAncestorTag(node, "table", -1, isDataTable)` guard — the **two
//! KEEP clauses** that make `_cleanConditionally` deliberately preserve data
//! tables (`Readability.js:2461-2468`). A faithful port that marks data
//! tables faithfully therefore preserves EDGAR/HMRC financial tables exactly
//! as Readability-JS does (HLD anti-inversion: the port converges TO RJS,
//! does NOT out-clean it).

use crate::readability::dom::{Dom, NodeRef, child_nodes, get_attribute, get_elements_by_tag_name};
use crate::readability::parse_int::row_or_col_span_or_one;

/// `_getRowAndColumnCount(table)` (`Readability.js:2240-2264`).
///
/// Returns `(rows, columns)` where:
///
/// * `rows` is the sum over every `<tr>` descendant of `(rowspan || 1)` with
///   the JS-faithful `parseInt`/falsy coercions (HLD §9 / M-6 — see
///   [`row_or_col_span_or_one`]);
/// * `columns` is the `Math.max` over every `<tr>` descendant of the sum of
///   `(colspan || 1)` for that row's `<td>` descendants — i.e. the **maximum
///   column count of any row**.
///
/// Returned as `i64` to mirror the JS `Number` arithmetic exactly (rowspan
/// can be negative via `parseInt("-3", 10)`; the downstream comparisons in
/// `_markDataTables` are `== 1`/`>= 10`/`> 10`/`> 4` and behave consistently
/// for negative inputs — a negative would never match any of the data-table
/// branches that look for "big" tables, so it falls through to the final
/// `rows * columns > 10` test, which is `false` for a negative).
pub fn get_row_and_column_count(table: &NodeRef) -> (i64, i64) {
    let mut rows: i64 = 0;
    let mut columns: i64 = 0;

    // `var trs = table.getElementsByTagName("tr");` — JS `getElementsByTagName`
    // on an HTMLTableElement is a live HTMLCollection over the subtree (every
    // descendant `<tr>`). Our snapshot getter is equivalent: a document-order
    // descendant `Vec`.
    for tr in get_elements_by_tag_name(table, "tr") {
        // 2245-2249: rowspan default 0 / parseInt / `|| 1`.
        let rowspan_attr = get_attribute(&tr, "rowspan");
        let rowspan = row_or_col_span_or_one(rowspan_attr.as_deref());
        rows = rows.saturating_add(rowspan);

        // 2252: columnsInThisRow = 0.
        let mut columns_in_this_row: i64 = 0;

        // 2253-2260: per-cell colspan accumulation.
        for cell in get_elements_by_tag_name(&tr, "td") {
            let colspan_attr = get_attribute(&cell, "colspan");
            let colspan = row_or_col_span_or_one(colspan_attr.as_deref());
            columns_in_this_row = columns_in_this_row.saturating_add(colspan);
        }

        // 2261: columns = Math.max(columns, columnsInThisRow).
        if columns_in_this_row > columns {
            columns = columns_in_this_row;
        }
    }
    (rows, columns)
}

/// `_markDataTables(root)` (`Readability.js:2271-2329`).
///
/// Walks every `<table>` descendant of `root` (in document order) and sets
/// `table._readabilityDataTable` to either `true` (a "data" table per the
/// checklist) or `false` (a "layout" table). Faithful, with cited branches:
///
/// * 2275-2278 `role == "presentation"` → false (every other check skipped).
/// * 2280-2283 `datatable == "0"` → false.
/// * 2285-2288 non-empty `summary` attribute → true.
/// * 2291-2294 `<caption>` with at least one child node → true.
/// * 2298-2305 any `col`/`colgroup`/`tfoot`/`thead`/`th` descendant → true.
/// * 2309-2311 any nested `<table>` descendant → false ("layout table").
/// * 2314-2319 `rows == 1` or `columns == 1` → false (single row/column =
///   layout).
/// * 2322-2324 `rows >= 10` or `columns > 4` → true ("big" table).
/// * 2327 else `rows * columns > 10` → true, otherwise false (final
///   size-only fallthrough).
///
/// Each branch terminates the per-table loop iteration (`continue`); the
/// flag is set exactly once per table.
pub fn mark_data_tables(dom: &mut Dom, root: &NodeRef) {
    for table in get_elements_by_tag_name(root, "table") {
        // 2275-2278: role=presentation
        if get_attribute(&table, "role").as_deref() == Some("presentation") {
            dom.set_readability_data_table(&table, false);
            continue;
        }
        // 2280-2283: datatable="0"
        if get_attribute(&table, "datatable").as_deref() == Some("0") {
            dom.set_readability_data_table(&table, false);
            continue;
        }
        // 2285-2288: summary non-empty.
        // JS: `var summary = table.getAttribute("summary"); if (summary)`.
        // A non-empty string is truthy; an empty string OR null is falsy.
        if let Some(s) = get_attribute(&table, "summary")
            && !s.is_empty()
        {
            dom.set_readability_data_table(&table, true);
            continue;
        }
        // 2291-2294: caption with childNodes.length > 0.
        if let Some(caption) = get_elements_by_tag_name(&table, "caption")
            .into_iter()
            .next()
            && !child_nodes(&caption).is_empty()
        {
            dom.set_readability_data_table(&table, true);
            continue;
        }
        // 2298-2305: any col / colgroup / tfoot / thead / th descendant.
        let data_table_descendants = ["col", "colgroup", "tfoot", "thead", "th"];
        let has_data_y_descendant = data_table_descendants
            .iter()
            .any(|t| !get_elements_by_tag_name(&table, t).is_empty());
        if has_data_y_descendant {
            dom.set_readability_data_table(&table, true);
            continue;
        }
        // 2309-2311: nested table.
        if !get_elements_by_tag_name(&table, "table").is_empty() {
            dom.set_readability_data_table(&table, false);
            continue;
        }
        // 2314-2319: single-row / single-column = layout.
        let (rows, columns) = get_row_and_column_count(&table);
        if columns == 1 || rows == 1 {
            dom.set_readability_data_table(&table, false);
            continue;
        }
        // 2322-2324: ≥10 rows OR >4 columns ⇒ data.
        if rows >= 10 || columns > 4 {
            dom.set_readability_data_table(&table, true);
            continue;
        }
        // 2327: final size-only fallthrough.
        dom.set_readability_data_table(&table, rows.saturating_mul(columns) > 10);
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Every expected value hand-derived by tracing `Readability.js:2271-2329`
    //! (NOT by running an oracle — anti-inversion, HLD §4).
    use super::*;
    use crate::readability::dom::{Dom, get_elements_by_tag_name};

    fn first_table(dom: &Dom) -> NodeRef {
        get_elements_by_tag_name(&dom.body().unwrap(), "table")[0].clone()
    }

    // ---- _getRowAndColumnCount (Readability.js:2240-2264) ----

    #[test]
    fn rowcol_simple_2x3() {
        let dom = Dom::parse(
            "<table>\
             <tr><td>a</td><td>b</td><td>c</td></tr>\
             <tr><td>d</td><td>e</td><td>f</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        assert_eq!(get_row_and_column_count(&t), (2, 3));
    }

    #[test]
    fn rowcol_rowspan_2_adds_2_to_rows() {
        // tr1 with rowspan="2" cell + tr2 = (1+2)+ ... wait, the JS only counts
        // rowspans on the <tr> itself (`trs[i].getAttribute("rowspan")`), NOT on
        // <td>. So the <tr>'s own `rowspan` attr (rare but valid HTML) is what
        // matters. Test with that:
        let dom = Dom::parse(
            "<table>\
             <tr rowspan=\"2\"><td>a</td><td>b</td></tr>\
             <tr><td>c</td><td>d</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        // rows = 2 (first tr) + 1 (second tr) = 3; columns max = 2.
        assert_eq!(get_row_and_column_count(&t), (3, 2));
    }

    #[test]
    fn rowcol_colspan_widens_row() {
        let dom = Dom::parse(
            "<table>\
             <tr><td colspan=\"3\">wide</td></tr>\
             <tr><td>a</td><td>b</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        // rows=2; row 1 has 3 columns (colspan 3), row 2 has 2 → max 3.
        assert_eq!(get_row_and_column_count(&t), (2, 3));
    }

    #[test]
    fn rowcol_rowspan_zero_string_is_one() {
        // rowspan="0" → JS: truthy string, parseInt → 0, `0 || 1 = 1`.
        let dom = Dom::parse(
            "<table>\
             <tr rowspan=\"0\"><td>a</td></tr>\
             <tr><td>b</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        // both rows contribute 1: rows=2, columns max=1.
        assert_eq!(get_row_and_column_count(&t), (2, 1));
    }

    #[test]
    fn rowcol_rowspan_garbage_is_one() {
        let dom = Dom::parse(
            "<table>\
             <tr rowspan=\"abc\"><td>a</td><td>b</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        // rows=1 (NaN || 1), columns=2.
        assert_eq!(get_row_and_column_count(&t), (1, 2));
    }

    #[test]
    fn rowcol_rowspan_leading_whitespace() {
        let dom = Dom::parse(
            "<table>\
             <tr rowspan=\"  3  \"><td>a</td></tr>\
             </table>",
        );
        let t = first_table(&dom);
        // parseInt strips leading whitespace; "  3  " → 3.
        assert_eq!(get_row_and_column_count(&t), (3, 1));
    }

    #[test]
    fn rowcol_empty_table() {
        let dom = Dom::parse("<table></table>");
        let t = first_table(&dom);
        // No rows → rows=0, columns=0.
        assert_eq!(get_row_and_column_count(&t), (0, 0));
    }

    // ---- _markDataTables (Readability.js:2271-2329) ----

    /// Helper: parse, run mark_data_tables on body, return `is_data_table`
    /// for the (sole) table.
    fn mark_first_table(html: &str) -> bool {
        let mut dom = Dom::parse(html);
        let body = dom.body().unwrap();
        mark_data_tables(&mut dom, &body);
        let t = first_table(&dom);
        dom.is_readability_data_table(&t)
    }

    #[test]
    fn role_presentation_is_layout() {
        assert!(!mark_first_table(
            "<table role=\"presentation\"><tr><th>a</th><th>b</th></tr><tr><td>1</td><td>2</td></tr></table>"
        ));
    }

    #[test]
    fn datatable_zero_is_layout() {
        // datatable="0" → false even though there's a <th> descendant.
        assert!(!mark_first_table(
            "<table datatable=\"0\"><tr><th>a</th></tr><tr><td>1</td></tr></table>"
        ));
    }

    #[test]
    fn summary_present_is_data() {
        assert!(mark_first_table(
            "<table summary=\"Q3 finance\"><tr><td>a</td></tr></table>"
        ));
    }

    #[test]
    fn summary_empty_is_not_data_via_summary_branch() {
        // summary="" — JS `if (summary)` is false (empty string falsy), so the
        // summary branch is NOT taken; falls through to later checks. A 1x1
        // table with no other signals → single col/row → false.
        assert!(!mark_first_table(
            "<table summary=\"\"><tr><td>a</td></tr></table>"
        ));
    }

    #[test]
    fn caption_with_children_is_data() {
        assert!(mark_first_table(
            "<table><caption>Q4 report</caption><tr><td>a</td></tr></table>"
        ));
    }

    #[test]
    fn caption_empty_is_not_data_via_caption_branch() {
        // <caption></caption> with childNodes.length===0 → not data via caption.
        // Falls through; 1x1 table → false.
        assert!(!mark_first_table(
            "<table><caption></caption><tr><td>a</td></tr></table>"
        ));
    }

    #[test]
    fn data_y_descendant_th_is_data() {
        assert!(mark_first_table(
            "<table><tr><th>h</th></tr><tr><td>1</td></tr></table>"
        ));
    }

    #[test]
    fn data_y_descendant_thead_is_data() {
        assert!(mark_first_table(
            "<table><thead><tr><td>h</td></tr></thead><tbody><tr><td>1</td></tr></tbody></table>"
        ));
    }

    #[test]
    fn data_y_descendant_colgroup_is_data() {
        assert!(mark_first_table(
            "<table><colgroup><col></colgroup><tr><td>1</td></tr></table>"
        ));
    }

    #[test]
    fn nested_table_outer_is_layout_inner_is_data_or_not() {
        // The OUTER table has a nested table descendant ⇒ outer = layout.
        // The INNER table 1x1 → single col/row → false.
        let mut dom = Dom::parse(
            "<table id=outer><tr><td><table id=inner><tr><td>x</td></tr></table></td></tr></table>",
        );
        let body = dom.body().unwrap();
        mark_data_tables(&mut dom, &body);
        let tables = get_elements_by_tag_name(&body, "table");
        // outer
        assert!(!dom.is_readability_data_table(&tables[0]));
        // inner (1x1)
        assert!(!dom.is_readability_data_table(&tables[1]));
    }

    #[test]
    fn single_row_is_layout() {
        // 1 row, 3 columns → rows == 1 → false.
        assert!(!mark_first_table(
            "<table><tr><td>a</td><td>b</td><td>c</td></tr></table>"
        ));
    }

    #[test]
    fn single_col_is_layout() {
        // 3 rows, 1 column → columns == 1 → false.
        assert!(!mark_first_table(
            "<table><tr><td>a</td></tr><tr><td>b</td></tr><tr><td>c</td></tr></table>"
        ));
    }

    #[test]
    fn ten_rows_is_data() {
        // 10 rows × 2 columns → rows >= 10 → true.
        let rows = "<tr><td>a</td><td>b</td></tr>".repeat(10);
        let html = format!("<table>{rows}</table>");
        assert!(mark_first_table(&html));
    }

    #[test]
    fn five_cols_is_data() {
        // columns > 4 → 5 → true.
        assert!(mark_first_table(
            "<table>\
             <tr><td>a</td><td>b</td><td>c</td><td>d</td><td>e</td></tr>\
             <tr><td>1</td><td>2</td><td>3</td><td>4</td><td>5</td></tr>\
             </table>"
        ));
    }

    #[test]
    fn four_cols_below_threshold_uses_final_size_check() {
        // 3 rows × 4 cols = 12 > 10 → true (final size check, NOT the
        // rows>=10/columns>4 branch since rows<10 and columns==4).
        assert!(mark_first_table(
            "<table>\
             <tr><td>a</td><td>b</td><td>c</td><td>d</td></tr>\
             <tr><td>1</td><td>2</td><td>3</td><td>4</td></tr>\
             <tr><td>x</td><td>y</td><td>z</td><td>w</td></tr>\
             </table>"
        ));
    }

    #[test]
    fn small_table_below_size_threshold_is_layout() {
        // 2 rows × 3 cols = 6 < 10 → false.
        assert!(!mark_first_table(
            "<table>\
             <tr><td>a</td><td>b</td><td>c</td></tr>\
             <tr><td>1</td><td>2</td><td>3</td></tr>\
             </table>"
        ));
    }

    /// Real-EDGAR-style hand-derived case: a financial table with a `<thead>`
    /// must be marked DATA (preserved by `_cleanConditionally`'s KEEP clause).
    /// This is the EDGAR/HMRC anti-inversion pin (HLD §7.4): the port matches
    /// RJS exactly — KEEPS data tables — never strips them.
    #[test]
    fn edgar_style_thead_table_marked_data_table() {
        let html = "<table>\
             <thead><tr><th>Q1</th><th>Q2</th></tr></thead>\
             <tbody><tr><td>$1,000</td><td>$2,500</td></tr><tr><td>$1,200</td><td>$2,700</td></tr></tbody>\
             </table>";
        assert!(
            mark_first_table(html),
            "EDGAR-style financial table with <thead>/<th> MUST be marked _readabilityDataTable=true \
             (Readability.js:2302) so _cleanConditionally KEEPS it (Readability.js:2461)."
        );
    }
}
