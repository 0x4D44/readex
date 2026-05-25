//! Throwaway M10 Phase 2B tracer — pin where the markdown blank-line / structural
//! divergence is introduced for G5 representatives.
//!
//! Usage:
//!   cargo run --release --example m10_phase2b_trace -- <path.html>
//!
//! Dumps, in order, the state of mdrcel's pipeline (own-arm vs readability-arm
//! vs jusText-arm via compare_extraction), and shows the final markdown / txt /
//! xml outputs around the divergence point.
//!
//! For each fixture we want to know:
//!   - did mdrcel's own arm produce any text?
//!   - did readability win in compare_extraction?
//!   - what does the winning body look like just before serialization?
//!   - what does the final output look like?

use std::fs;

use readex::readability::dom::{self, Dom, children, element_text, local_name, tail, text_content};
use readex::trafilatura::cleaning::{self, Options as CleanOpts};
use readex::trafilatura::{main_extractor, readability_fork};

type NodeRef = readex::readability::dom::NodeRef;

fn vis(s: &str) -> String {
    s.replace('\n', "\\n").replace('\t', "\\t")
}

fn walk_elements(node: &NodeRef, out: &mut Vec<NodeRef>) {
    for c in children(node) {
        if dom::is_element(&c) {
            out.push(c.clone());
            walk_elements(&c, out);
        }
    }
}

fn dump_tree_brief(label: &str, root: &NodeRef, needle: &str) {
    println!("\n=== {label} ===");
    let mut all: Vec<NodeRef> = Vec::new();
    walk_elements(root, &mut all);
    let mut found = false;
    for elem in &all {
        let tag = local_name(elem).unwrap_or_default();
        let text = element_text(elem);
        let tl = tail(elem);
        let text_has = text.as_deref().is_some_and(|t| t.contains(needle));
        let tail_has = tl.as_deref().is_some_and(|t| t.contains(needle));
        if text_has || tail_has {
            found = true;
            println!("  <{tag}> .text={:?} .tail={:?}", text.as_deref().map(vis), tl.as_deref().map(vis));
            // show one level of children for context
            for (i, c) in children(elem).iter().enumerate() {
                if dom::is_element(&c) {
                    let ctag = local_name(&c).unwrap_or_default();
                    let ctext = element_text(&c);
                    let ctail = tail(&c);
                    println!(
                        "    child[{i}] <{ctag}> .text={:?} .tail={:?}",
                        ctext.as_deref().map(vis),
                        ctail.as_deref().map(vis),
                    );
                }
            }
        }
    }
    if !found {
        // dump a summary if not found
        let raw = text_content(root);
        let idx = raw.find(needle);
        if let Some(i) = idx {
            let lo = i.saturating_sub(40);
            let hi = (i + 80).min(raw.len());
            println!("  (needle in text_content): ...{}...", vis(&raw[lo..hi]));
        } else {
            println!("  (needle '{needle}' not found in tree)");
        }
    }
}

fn dump_final_around(label: &str, s: &str, needle: &str) {
    println!("\n=== {label} ===");
    if let Some(pos) = s.find(needle) {
        let mut lo = pos.saturating_sub(60);
        while lo < s.len() && !s.is_char_boundary(lo) {
            lo -= 1;
        }
        let mut hi = (pos + needle.len() + 200).min(s.len());
        while hi > 0 && !s.is_char_boundary(hi) {
            hi -= 1;
        }
        println!("  ...{}...", vis(&s[lo..hi]));
    } else {
        println!("  ('{needle}' not found in output)");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).expect("usage: m10_phase2b_trace <path.html> [needle]");
    let needle = args.get(2).map(|s| s.as_str()).unwrap_or("");

    let bytes = fs::read(path).expect("read fixture");
    let html = String::from_utf8_lossy(&bytes).to_string();

    let opts = CleanOpts::default();

    // STAGE 0: raw parse
    let dom = Dom::parse(&html);
    let html_root = dom.root_element().expect("html root");

    // STAGE A: tree_cleaning + take backup
    cleaning::tree_cleaning(&html_root, &opts);
    let cleaned_body_backup = {
        let body = dom.body().expect("body");
        dom::deep_clone(&body)
    };
    if !needle.is_empty() {
        dump_tree_brief("STAGE A — post tree_cleaning (backup snapshot)", &cleaned_body_backup, needle);
    }

    // STAGE B: convert_tags
    cleaning::convert_tags(&html_root, &opts);
    let body_b = dom.body().expect("body");
    if !needle.is_empty() {
        dump_tree_brief("STAGE B — post convert_tags (extract_content input)", &body_b, needle);
    }

    // STAGE C: own arm extract_content
    let (own_body, own_text, own_len) = main_extractor::extract_content(&body_b, &opts);
    println!("\n=== STAGE C — extract_content own arm ===");
    println!("  own_len = {own_len}");
    println!("  own_text snippet (first 200): {:?}", vis(&own_text.chars().take(200).collect::<String>()));
    if !needle.is_empty() {
        dump_tree_brief("STAGE C tree — own arm body", &own_body, needle);
    }

    // STAGE C.5 — pure try_readability (no compare_extraction wrapper) to
    // see what readability arm produces before any TEI-sanitize is applied
    // by compare_extraction's post-pass.
    let try_ready = readability_fork::try_readability(&html);
    println!("\n=== STAGE C.5 — try_readability raw output ===");
    match &try_ready {
        Some(node) => {
            let raw = text_content(node);
            println!("  text_content len = {}", raw.chars().count());
            if !needle.is_empty() {
                dump_tree_brief("STAGE C.5 tree — try_readability raw", node, needle);
            }
        }
        None => println!("  try_readability returned None"),
    }

    // STAGE D: compare_extraction (which may swap in readability)
    let (winning_body, winning_text, winning_len) =
        readability_fork::compare_extraction(&cleaned_body_backup, &html, &own_body, own_text.clone(), own_len, &opts);
    println!("\n=== STAGE D — compare_extraction result ===");
    println!("  winning_len = {winning_len}");
    // NodeRef is Rc<Node> (Handle); same-pointer test:
    let same = std::rc::Rc::ptr_eq(&winning_body, &own_body);
    println!("  winning_body is_same_as_own = {same}");
    println!("  winning_text snippet (first 200): {:?}", vis(&winning_text.chars().take(200).collect::<String>()));
    if !needle.is_empty() {
        dump_tree_brief("STAGE D tree — winning body (post compare_extraction)", &winning_body, needle);
    }

    // STAGE D.5: dump empty <p> elements and their neighbours
    println!("\n=== STAGE D.5 — empty-element survey ===");
    {
        let mut all: Vec<NodeRef> = Vec::new();
        walk_elements(&winning_body, &mut all);
        let mut empties = 0;
        for elem in &all {
            let tag = local_name(elem).unwrap_or_default();
            if tag == "p" || tag == "head" || tag == "list" || tag == "lb" {
                let text = element_text(elem);
                let tl = tail(elem);
                let kids = children(elem);
                let is_empty = text.as_deref().map(|t| t.is_empty()).unwrap_or(true) && kids.is_empty();
                if is_empty {
                    empties += 1;
                    println!("  empty <{tag}> .tail={:?}", tl.as_deref().map(vis));
                }
            }
        }
        println!("  total empty-NEWLINE-elems: {empties}");
    }

    // STAGE E: full pipeline outputs
    let md = readex::extract_to_markdown(&html, None, &readex::Options::default())
        .unwrap_or_else(|e| format!("ERR: {e:?}"));
    let txt = readex::extract_to_txt(&html, None, &readex::Options::default())
        .unwrap_or_else(|e| format!("ERR: {e:?}"));
    let xml = readex::extract_to_xml(&html, None, &readex::Options::default())
        .unwrap_or_else(|e| format!("ERR: {e:?}"));

    if !needle.is_empty() {
        dump_final_around("STAGE E — final MARKDOWN around needle", &md, needle);
        dump_final_around("STAGE E — final TXT around needle", &txt, needle);
        dump_final_around("STAGE E — final XML around needle", &xml, needle);
    }

    drop(dom);
}
