//! Throwaway M9 Stage 5 Round 2 tracer — dumps cell/head/p structure for the
//! `3adc0f3100a84aea` sports-league fixture so we can pin where the
//! `Forest\n|\n` (mdrcel) vs `Forest |\n` (oracle) divergence is introduced.
//!
//! Usage:
//!   cargo run --release --example m9_stage5_round2_trace -- <path.html>

use std::fs;

use readex::readability::dom::{self, Dom, children, element_text, local_name, tail};
use readex::trafilatura::cleaning::{self, Options as CleanOpts};
use readex::trafilatura::main_extractor;

type NodeRef = readex::readability::dom::NodeRef;

fn vis(s: &str) -> String {
    s.replace('\n', "⏎").replace(' ', "·")
}

fn dump_cells(label: &str, root: &NodeRef) {
    println!("\n=== {label} ===");
    let mut all: Vec<NodeRef> = Vec::new();
    walk_elements(root, &mut all);
    for elem in &all {
        let tag = local_name(elem).unwrap_or_default();
        if tag != "cell" && tag != "td" && tag != "th" {
            continue;
        }
        let text = element_text(elem);
        let has_forest = text.as_deref().is_some_and(|t| t.contains("Forest"))
            || children(elem).iter().any(|c| {
                element_text(c)
                    .as_deref()
                    .is_some_and(|t| t.contains("Forest"))
            });
        if !has_forest {
            continue;
        }
        println!("  <{tag}>");
        println!("    .text = {:?}", text.as_deref().map(vis));
        println!("    .tail = {:?}", tail(elem).as_deref().map(vis));
        for (i, c) in children(elem).iter().enumerate() {
            if dom::is_element(&c) {
                let ctag = local_name(&c).unwrap_or_default();
                let ctext = element_text(&c);
                let ctail = tail(&c);
                println!(
                    "      child[{i}] <{ctag}> .text={:?} .tail={:?} children={}",
                    ctext.as_deref().map(vis),
                    ctail.as_deref().map(vis),
                    children(&c).len(),
                );
                // One level deeper too — head may have <hi> or text only.
                for (j, gc) in children(&c).iter().enumerate() {
                    if dom::is_element(&gc) {
                        let gctag = local_name(&gc).unwrap_or_default();
                        println!(
                            "         gchild[{j}] <{gctag}> .text={:?} .tail={:?}",
                            element_text(&gc).as_deref().map(vis),
                            tail(&gc).as_deref().map(vis),
                        );
                    }
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
        .expect("usage: m9_stage5_round2_trace <path.html>");
    let bytes = fs::read(&path).expect("read fixture");
    let html = String::from_utf8_lossy(&bytes).to_string();

    let opts = CleanOpts::default();

    let dom = Dom::parse(&html);
    let html_root = dom.root_element().expect("html root");

    // STAGE A
    cleaning::tree_cleaning(&html_root, &opts);
    let body_a = dom.body().expect("body");
    dump_cells("STAGE A — post tree_cleaning", &body_a);

    // STAGE B
    cleaning::convert_tags(&html_root, &opts);
    let body_b = dom.body().expect("body");
    dump_cells("STAGE B — post convert_tags", &body_b);

    // STAGE C
    let (result_body, _temp_text, _len) = main_extractor::extract_content(&body_b, &opts);
    dump_cells("STAGE C — post extract_content", &result_body);

    // STAGE D — full markdown output, print first 400 bytes.
    let md = readex::extract_to_markdown(&html, None, &readex::Options::default())
        .unwrap_or_else(|e| format!("ERR: {e:?}"));
    println!("\n=== STAGE D — first 400 bytes of markdown ===");
    let n = md.len().min(400);
    println!("{}", vis(&md[..n]));

    drop(dom);
    drop(result_body);
}
