//! Phase 2H minimal probe: how html5ever handles stray `</br>` end-tag.
//!
//! Compares to lxml (which drops `</br>` outright).

use readex::readability::dom::{self, Dom, children, element_text, local_name, tail, text_content};

type NodeRef = readex::readability::dom::NodeRef;

fn walk(node: &NodeRef, depth: usize) {
    for c in children(node) {
        if dom::is_element(&c) {
            let tag = local_name(&c).unwrap_or_default();
            let t = element_text(&c);
            let tl = tail(&c);
            let indent = "  ".repeat(depth);
            println!(
                "{indent}<{tag}> text={:?} tail={:?}",
                t.as_deref(),
                tl.as_deref()
            );
            walk(&c, depth + 1);
        }
    }
}

fn main() {
    for snippet in &[
        "<html><body><div><p>before</br>after</p></div></body></html>",
        "<html><body><div>before</br>after</div></body></html>",
        // The actual fixture pattern
        "<html><body><div><div><p>some Chinese text。</br>LEGAL DISCLAIMER WARNING</p></div></div></body></html>",
    ] {
        println!("\n=== HTML snippet ===");
        println!("{snippet}");
        let dom = Dom::parse(snippet);
        let root = dom.root_element().expect("root");
        walk(&root, 0);
        println!("--- text_content of root: ---");
        println!("{:?}", text_content(&root));
    }
}
