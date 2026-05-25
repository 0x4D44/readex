//! Throwaway M10 Phase 2F parser probe — dump mdrcel's post-html5ever-parse
//! tree shape around the SEO-spam containers, BEFORE any cleaning runs.
//!
//! Compared offline against the lxml-side dump produced by the sibling Python
//! script `examples/m10_phase2f_parser_probe.py`. The diff answers Phase 2F's
//! gating question: do html5ever and lxml agree at the spam-div subtree, or
//! does one of them foster-parent / drop / re-interpret the stray
//! `<td>`/`<tr>`/`<source>` tokens?
//!
//! Usage:
//!   cargo run --release --example m10_phase2f_parser_probe -- <sha_or_path>
//!     [<sha_or_path> ...]
//!
//! For each input, looks up `benchmark/fuzz_corpus/<sha>.html` (or, if the
//! arg is an absolute path, reads that file directly), parses via the same
//! `parse_document(RcDom, ParseOpts { scripting_enabled: false, .. })`
//! invocation as `src/readability/dom.rs:127-138`, and:
//!
//! 1. Locates every element whose `class` attribute contains `pl_css_ganrao`
//!    OR whose `style` attribute contains `display: none` /  `display:none`.
//!    These are the candidate spam containers.
//! 2. For each found container, dumps the entire descendant subtree (depth
//!    ≤ 8) with tag + attrs.
//! 3. Also dumps a depth-≤5 outline of `<body>` so we can see whether stray
//!    `<td>`/`<tr>`/`<source>` tokens were foster-parented elsewhere.
//!
//! Output goes to `/tmp/m10_phase2f/<sha>_mdrcel_pre_clean.txt`.

use std::fs;
use std::path::{Path, PathBuf};

use readex::readability::dom::{
    self, Dom, NodeData, attributes_in_source_order, child_nodes, is_element, local_name,
};
use readex::trafilatura::cleaning::{Options as CleanOpts, prune_unwanted_nodes, tree_cleaning};
use readex::trafilatura::xpaths_constants::OVERALL_DISCARD_XPATH;

type NodeRef = readex::readability::dom::NodeRef;

fn vis(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            _ => out.push(c),
        }
    }
    out
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        vis(s)
    } else {
        let t: String = s.chars().take(max).collect();
        format!("{}…(+{} chars)", vis(&t), s.chars().count() - max)
    }
}

fn attrs_brief(node: &NodeRef) -> String {
    let attrs = attributes_in_source_order(node);
    if attrs.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for (name, value) in attrs {
        parts.push(format!("{}={:?}", name, truncate_for_display(&value, 60)));
    }
    format!(" [{}]", parts.join(" "))
}

fn dump_subtree(node: &NodeRef, depth: usize, max_depth: usize, out: &mut String) {
    if depth > max_depth {
        return;
    }
    let indent = "  ".repeat(depth);
    for child in child_nodes(node) {
        match &child.data {
            NodeData::Element { .. } => {
                let tag = local_name(&child).unwrap_or_else(|| "?".to_string());
                let attrs = attrs_brief(&child);
                out.push_str(&format!("{indent}<{tag}>{attrs}\n"));
                dump_subtree(&child, depth + 1, max_depth, out);
            }
            NodeData::Text { contents } => {
                let data = contents.borrow();
                let s = data.trim();
                if !s.is_empty() {
                    out.push_str(&format!("{indent}#text {:?}\n", truncate_for_display(s, 80)));
                }
            }
            NodeData::Comment { contents } => {
                out.push_str(&format!(
                    "{indent}#comment {:?}\n",
                    truncate_for_display(contents, 60)
                ));
            }
            _ => {}
        }
    }
}

/// Walk every element under `root` and collect those whose class contains
/// `pl_css_ganrao` or whose style contains `display:none` / `display: none`.
fn find_spam_containers(root: &NodeRef) -> Vec<NodeRef> {
    let mut found = Vec::new();
    walk(root, &mut |n| {
        let mut class_val = String::new();
        let mut style_val = String::new();
        for (name, value) in attributes_in_source_order(n) {
            match name.as_str() {
                "class" => class_val = value,
                "style" => style_val = value,
                _ => {}
            }
        }
        let has_ganrao = class_val.contains("pl_css_ganrao");
        let has_hidden = style_val.contains("display:none") || style_val.contains("display: none");
        if has_ganrao || has_hidden {
            found.push(n.clone());
        }
    });
    found
}

fn walk<F: FnMut(&NodeRef)>(node: &NodeRef, f: &mut F) {
    for c in child_nodes(node) {
        if is_element(&c) {
            f(&c);
            walk(&c, f);
        }
    }
}

/// Count occurrences of stray tags (`td`, `tr`, `th`, `source`, `fieldset`,
/// `rt`, `tfoot`, `ul`, `li`, `dfn`, `cite`, `acronym`, `tbody`) anywhere in
/// the tree, regardless of container. Used to spot foster-parented tokens.
fn count_stray_tags(root: &NodeRef) -> Vec<(String, usize)> {
    let interesting = [
        "td", "tr", "th", "source", "fieldset", "rt", "tfoot", "ul", "li", "dfn", "cite",
        "acronym", "tbody",
    ];
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    walk(root, &mut |n| {
        if let Some(tag) = local_name(n) {
            if interesting.contains(&tag.as_str()) {
                *counts.entry(tag).or_insert(0) += 1;
            }
        }
    });
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort();
    out
}

fn process(input: &str) -> std::io::Result<()> {
    // Resolve sha or path.
    let path = if Path::new(input).is_absolute() {
        PathBuf::from(input)
    } else {
        PathBuf::from(format!(
            "benchmark/fuzz_corpus/{}.html",
            input.trim_end_matches(".html")
        ))
    };

    let sha_label = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.chars().take(12).collect::<String>())
        .unwrap_or_else(|| "unknown".to_string());

    eprintln!("[mdrcel] {} -> {}", sha_label, path.display());
    let html = fs::read_to_string(&path)?;
    let dom = Dom::parse(&html);
    let doc = dom.document();

    let mut out = String::new();
    out.push_str(&format!("# mdrcel parser probe (pre-clean)\n"));
    out.push_str(&format!("# fixture: {}\n", path.display()));
    out.push_str(&format!("# sha (head): {}\n", sha_label));
    out.push_str(&format!("# parse: html5ever via parse_document(RcDom, ParseOpts {{ scripting_enabled: false, .. }})\n"));
    out.push_str(&format!("# source bytes: {}\n", html.len()));
    out.push_str("\n");

    // Spam containers.
    let spam = find_spam_containers(&doc);
    out.push_str(&format!(
        "## Spam-candidate containers (class~pl_css_ganrao OR style~display:none): {}\n\n",
        spam.len()
    ));
    for (i, container) in spam.iter().enumerate() {
        let tag = local_name(container).unwrap_or_else(|| "?".to_string());
        let attrs = attrs_brief(container);
        out.push_str(&format!("### Container #{i}: <{tag}>{attrs}\n"));
        // Parent context for one ancestor.
        if let Some(parent) = dom::parent(container) {
            if let Some(ptag) = local_name(&parent) {
                let pattrs = attrs_brief(&parent);
                out.push_str(&format!("  (parent: <{ptag}>{pattrs})\n"));
            }
        }
        out.push_str("  --- subtree dump (depth <= 8) ---\n");
        dump_subtree(container, 1, 8, &mut out);
        out.push_str("\n");
    }

    // Stray-tag counts across the WHOLE tree (whether or not foster-parented).
    let strays = count_stray_tags(&doc);
    out.push_str("## Stray-tag total counts across full document tree\n");
    if strays.is_empty() {
        out.push_str("  (none of {td, tr, th, source, fieldset, rt, tfoot, ul, li, dfn, cite, acronym, tbody})\n");
    } else {
        for (tag, n) in &strays {
            out.push_str(&format!("  <{}>: {}\n", tag, n));
        }
    }
    out.push_str("\n");

    // Body outline (depth <= 5) — to see if elements got foster-parented OUT
    // of the spam div.
    out.push_str("## <body> outline (depth <= 5)\n");
    if let Some(body) = dom.body() {
        out.push_str(&format!("<body>{}\n", attrs_brief(&body)));
        dump_subtree(&body, 1, 5, &mut out);
    } else {
        out.push_str("  (no body)\n");
    }

    // Write to /tmp/m10_phase2f/<sha>_mdrcel_pre_clean.txt
    let out_dir = Path::new("/tmp/m10_phase2f");
    fs::create_dir_all(out_dir)?;
    let out_path = out_dir.join(format!("{}_mdrcel_pre_clean.txt", sha_label));
    fs::write(&out_path, &out)?;
    eprintln!("[mdrcel] wrote {}", out_path.display());

    // === Post-tree_cleaning dump ===
    let body_for_clean = dom.body();
    let opts = CleanOpts::default();
    let mut post = String::new();
    post.push_str(&format!("# mdrcel parser probe (POST tree_cleaning)\n"));
    post.push_str(&format!("# fixture: {}\n", path.display()));
    post.push_str(&format!("# Options: {:?}\n\n", opts));
    if let Some(body) = body_for_clean.clone() {
        tree_cleaning(&body, &opts);
        let strays_after = count_stray_tags(&body);
        post.push_str(
            "## Stray-tag counts across <body> after tree_cleaning + prune_html (default Options)\n",
        );
        if strays_after.is_empty() {
            post.push_str("  (none)\n");
        } else {
            for (tag, n) in &strays_after {
                post.push_str(&format!("  <{}>: {}\n", tag, n));
            }
        }
        let spam_after = find_spam_containers(&body);
        post.push_str(&format!(
            "\n## Spam-candidate containers AFTER tree_cleaning: {}\n",
            spam_after.len()
        ));
        for (i, c) in spam_after.iter().enumerate() {
            let tag = local_name(c).unwrap_or_else(|| "?".to_string());
            let ab = attrs_brief(c);
            post.push_str(&format!("### Survivor #{i}: <{tag}>{ab}\n"));
            // count direct element children
            let n_children = child_nodes(c).iter().filter(|n| is_element(n)).count();
            post.push_str(&format!("  direct element children: {n_children}\n"));
        }
        post.push_str("\n## <body> outline AFTER tree_cleaning (depth <= 5)\n");
        post.push_str(&format!("<body>{}\n", attrs_brief(&body)));
        dump_subtree(&body, 1, 5, &mut post);
    } else {
        post.push_str("  (no body)\n");
    }
    let post_path = out_dir.join(format!("{}_mdrcel_post_clean.txt", sha_label));
    fs::write(&post_path, &post)?;
    eprintln!("[mdrcel] wrote {}", post_path.display());

    // === Post-OVERALL_DISCARD_XPATH dump (separate fresh parse so we
    // observe the prune effect in isolation, not on top of tree_cleaning) ===
    let dom2 = Dom::parse(&html);
    let body2 = dom2.body().expect("body");
    // NB: with_backup=true to match prune_unwanted_sections's actual call
    // (main_extractor.py:537). Returned handle may be the BACKUP (pristine
    // pre-prune copy) if the prune shrank text_content by > 1/7.
    let pruned = prune_unwanted_nodes(&body2, OVERALL_DISCARD_XPATH, true);
    // Use the returned handle (could be backup) to reflect real semantics.
    let spam_after_overall = find_spam_containers(&pruned);
    let backup_used = !std::rc::Rc::ptr_eq(&pruned, &body2);
    let mut overall = String::new();
    overall.push_str("# mdrcel parser probe (post OVERALL_DISCARD_XPATH only)\n");
    overall.push_str(&format!("# fixture: {}\n", path.display()));
    overall.push_str(&format!(
        "# with_backup=true semantics; backup_used={}\n\n",
        backup_used
    ));
    overall.push_str(&format!(
        "## Spam-candidate containers AFTER prune_unwanted_nodes(OVERALL_DISCARD_XPATH, true): {}\n",
        spam_after_overall.len()
    ));
    for (i, c) in spam_after_overall.iter().enumerate() {
        let tag = local_name(c).unwrap_or_else(|| "?".to_string());
        overall.push_str(&format!("### Survivor #{i}: <{tag}>{}\n", attrs_brief(c)));
    }
    overall.push_str("\n## Stray-tag counts AFTER OVERALL_DISCARD_XPATH prune\n");
    let strays3 = count_stray_tags(&pruned);
    if strays3.is_empty() {
        overall.push_str("  (none)\n");
    } else {
        for (tag, n) in &strays3 {
            overall.push_str(&format!("  <{}>: {}\n", tag, n));
        }
    }
    let overall_path = out_dir.join(format!("{}_mdrcel_post_overall_discard.txt", sha_label));
    fs::write(&overall_path, &overall)?;
    eprintln!("[mdrcel] wrote {}", overall_path.display());
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: cargo run --release --example m10_phase2f_parser_probe -- <sha|path> [...]"
        );
        std::process::exit(2);
    }
    let mut had_err = false;
    for input in &args {
        if let Err(e) = process(input) {
            eprintln!("ERROR processing {input}: {e}");
            had_err = true;
        }
    }
    if had_err {
        std::process::exit(1);
    }
}
