//! Throwaway M9 Stage 5 tracer — dumps the DOM tree's `<p>` elements at each
//! pipeline stage for a single fixture so we can pin where the trailing-space-
//! before-`<lb/>` divergence is introduced.
//!
//! Usage:
//!   cargo run --release --example m9_stage5_trace -- <path.html>
//!
//! Walks the bare_extraction_with_cascade pipeline manually, dumping at:
//!   STAGE A: after tree_cleaning
//!   STAGE B: after convert_tags
//!   STAGE C: after extract_content (which runs _extract + handle_paragraphs)
//!   STAGE D: post-serialization (the final xml string, around the divergence)
//!
//! For each stage, prints every `<p>` (and `<lb>` parent) element with its
//! `.text` and child summary so we can locate the exact handler that introduces
//! the trailing space before `<br>`/`<lb>`.

use std::fs;

use readex::readability::dom::{
    self, Dom, children, element_text, get_elements_by_tag_name, local_name, tail,
};
use readex::trafilatura::cleaning::{self, Options as CleanOpts};
use readex::trafilatura::main_extractor;

type NodeRef = readex::readability::dom::NodeRef;

/// Render a string showing whitespace explicitly (· for spaces, ⏎ for newlines).
fn vis(s: &str) -> String {
    s.replace('\n', "⏎").replace(' ', "·")
}

fn dump_text_around_folk(label: &str, root: &NodeRef) {
    println!("\n=== {label} ===");
    // Walk every element in the subtree and look for any whose text/tail contains "folk".
    let mut all: Vec<NodeRef> = Vec::new();
    walk_elements(root, &mut all);
    for elem in &all {
        let tag = local_name(elem).unwrap_or_default();
        let text = element_text(elem);
        let tl = tail(elem);
        let text_has = text.as_deref().is_some_and(|t| t.contains("folk"));
        let tail_has = tl.as_deref().is_some_and(|t| t.contains("folk"));
        if text_has || tail_has {
            println!("  <{tag}>");
            if let Some(t) = &text {
                println!("    .text   = {:?}", vis(t));
            }
            if let Some(t) = &tl {
                println!("    .tail   = {:?}", vis(t));
            }
            // Show children with their tail.
            for (i, c) in children(elem).iter().enumerate() {
                if dom::is_element(c) {
                    let ctag = local_name(c).unwrap_or_default();
                    let ctext = element_text(c);
                    let ctail = tail(c);
                    println!(
                        "      child[{i}] <{ctag}> .text={:?} .tail={:?}",
                        ctext.as_deref().map(vis),
                        ctail.as_deref().map(vis),
                    );
                } else if dom::is_text(c) {
                    // Inline text node.
                    let _ = c;
                    // node-level text is exposed via element_text on parent; skip.
                }
            }
        }
    }
}

fn walk_elements(node: &NodeRef, out: &mut Vec<NodeRef>) {
    for c in children(node) {
        if dom::is_element(&c) {
            out.push(c.clone());
            walk_elements(&c, out);
        }
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: m9_stage5_trace <path.html>");
    let bytes = fs::read(&path).expect("read fixture");
    let html = String::from_utf8_lossy(&bytes).to_string();

    let opts = CleanOpts::default();

    // STAGE 0: raw parse, look for the source-side situation.
    let dom = Dom::parse(&html);
    let html_root = dom.root_element().expect("html root");
    let body0 = dom.body().expect("body");
    dump_text_around_folk("STAGE 0 — raw parse (pre-cleaning)", &body0);

    // STAGE A: post tree_cleaning.
    cleaning::tree_cleaning(&html_root, &opts);
    let body_a = dom.body().expect("body");
    dump_text_around_folk("STAGE A — post tree_cleaning", &body_a);

    // STAGE B: post convert_tags.
    cleaning::convert_tags(&html_root, &opts);
    let body_b = dom.body().expect("body");
    dump_text_around_folk("STAGE B — post convert_tags", &body_b);

    // STAGE C: extract_content (this is what _extract+handle_paragraphs produces).
    let (result_body, _temp_text, _len) = main_extractor::extract_content(&body_b, &opts);
    dump_text_around_folk("STAGE C — post extract_content", &result_body);

    // STAGE D: final xml string from public surface for cross-check.
    let xml = readex::extract_to_xml(&html, None, &readex::Options::default())
        .unwrap_or_else(|e| format!("ERR: {e:?}"));
    // Print only the bit around "folk".
    if let Some(pos) = xml.find("folk") {
        let lo = pos.saturating_sub(40);
        let hi = (pos + 80).min(xml.len());
        println!("\n=== STAGE D — final XML excerpt around 'folk' ===");
        println!("  ...{}...", vis(&xml[lo..hi]));
    } else {
        println!("\n=== STAGE D — 'folk' not found in final XML ===");
    }

    // Also pull elements list to ensure we counted any `lb`s.
    let lbs = get_elements_by_tag_name(&result_body, "lb");
    println!("\n(post extract_content: {} <lb> elements)", lbs.len());

    // Keep dom alive (rcdom drop quirk) until end.
    drop(dom);
    drop(result_body);
}
