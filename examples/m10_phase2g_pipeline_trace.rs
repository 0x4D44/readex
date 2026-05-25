//! M10 Phase 2G — pipeline-locus tracer for SEO-spam leak.
//!
//! Goal: trace mdrcel's `extract_to_xml` pipeline on ONE canonical SEO-spam
//! fixture, instrumenting EVERY major boundary to find the first stage at
//! which the spam div (`pl_css_ganrao` / `display:none`) is missing — OR
//! confirm it survives all the way to output (which means OVERALL_DISCARD
//! never fires on a tree that contains it in the actual pipeline).
//!
//! Companion script:
//!   examples/m10_phase2g_pipeline_trace.py    (lxml side, same boundaries).
//!
//! Reproduction:
//!   cargo run --release --example m10_phase2g_pipeline_trace -- <sha>
//!
//! Output: /tmp/m10_phase2g/<sha>_mdrcel_trace.txt
//!
//! Stages instrumented (in mdrcel's pipeline order, matching
//! `bare_extraction_with_cascade` + `extract_content` + `_extract`):
//!
//!   S0  post-Dom::parse (raw html5ever tree)
//!   S1  post-tree_cleaning (Options::default())
//!   S2  post-deep_clone for cleaned_body_backup
//!   S3  post-convert_tags
//!   S4  per-BODY_XPATH expression: subtree picked? Does it CONTAIN the spam?
//!   S4b post-prune_unwanted_sections(subtree, ...) — including OVERALL_DISCARD
//!   S5  post-_extract result_body
//!   S6  post-extract_content (after strip_elements(done) + strip_tags(div))
//!   S7  post-compare_extraction winning body
//!   S8  final serialised XML (substring search for the leaked-tag literals)

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use readex::readability::dom::{
    self, Dom, NodeRef, attributes_in_source_order, child_nodes, deep_clone, is_element,
    local_name, serialize_html,
};
use readex::trafilatura::cleaning::{Options as CleanOpts, tree_cleaning, convert_tags};
use readex::trafilatura::main_extractor;
use readex::trafilatura::readability_fork;
use readex::trafilatura::xpath_engine;
use readex::trafilatura::xpaths_constants::BODY_XPATH;

fn walk<F: FnMut(&NodeRef)>(node: &NodeRef, f: &mut F) {
    for c in child_nodes(node) {
        if is_element(&c) {
            f(&c);
            walk(&c, f);
        }
    }
}

fn find_spam(root: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    walk(root, &mut |n| {
        let mut cls = String::new();
        let mut sty = String::new();
        for (k, v) in attributes_in_source_order(n) {
            match k.as_str() {
                "class" => cls = v,
                "style" => sty = v,
                _ => {}
            }
        }
        if cls.contains("pl_css_ganrao")
            || sty.contains("display:none")
            || sty.contains("display: none")
        {
            out.push(n.clone());
        }
    });
    out
}

/// Does the subtree rooted at `root` (descendant-or-self) contain a spam
/// container? Returns the list of matches found in self+descendants.
fn spam_in_subtree(root: &NodeRef) -> Vec<NodeRef> {
    let mut matches = Vec::new();
    // self
    let mut cls = String::new();
    let mut sty = String::new();
    for (k, v) in attributes_in_source_order(root) {
        match k.as_str() {
            "class" => cls = v,
            "style" => sty = v,
            _ => {}
        }
    }
    if cls.contains("pl_css_ganrao")
        || sty.contains("display:none")
        || sty.contains("display: none")
    {
        matches.push(root.clone());
    }
    matches.extend(find_spam(root));
    matches
}

fn id_or_class(n: &NodeRef) -> String {
    let mut id = String::new();
    let mut cls = String::new();
    for (k, v) in attributes_in_source_order(n) {
        match k.as_str() {
            "id" => id = v,
            "class" => cls = v,
            _ => {}
        }
    }
    format!("id={id:?} class={cls:?}")
}

fn count_stray_tags(root: &NodeRef) -> Vec<(String, usize)> {
    let interesting = [
        "td", "tr", "th", "source", "fieldset", "rt", "tfoot", "ul", "li", "dfn", "cite",
        "acronym", "tbody",
    ];
    let mut counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
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

fn report_stage(out: &mut String, stage: &str, root: &NodeRef, scope_desc: &str) {
    let matches = spam_in_subtree(root);
    let strays = count_stray_tags(root);
    out.push_str(&format!("\n## {stage}\n"));
    out.push_str(&format!("  scope: {scope_desc}\n"));
    out.push_str(&format!("  spam containers reachable: {}\n", matches.len()));
    for (i, m) in matches.iter().enumerate() {
        let tag = local_name(m).unwrap_or_default();
        out.push_str(&format!("    [{i}] <{tag}> {}\n", id_or_class(m)));
    }
    out.push_str(&format!("  stray-tag counts: "));
    if strays.is_empty() {
        out.push_str("(none of {td, tr, th, source, fieldset, rt, tfoot, ul, li, dfn, cite, acronym, tbody})\n");
    } else {
        let s: Vec<String> = strays.iter().map(|(k, n)| format!("{k}={n}")).collect();
        out.push_str(&s.join(", "));
        out.push('\n');
    }
}

/// Mimic main_extractor::_extract's BODY_XPATH loop without running the
/// full handle_textelem dispatch — we just want to see which subtree it
/// picks (first match for the first xpath expression that yields one).
fn locate_body_subtree(tree: &NodeRef) -> Option<(usize, String, NodeRef)> {
    for (i, expr) in BODY_XPATH.iter().enumerate() {
        let matches = xpath_engine::evaluate(expr, tree).unwrap_or_default();
        if let Some(first) = matches.into_iter().next() {
            let snippet: String = expr.chars().take(80).collect();
            return Some((i, snippet, first));
        }
    }
    None
}

fn process(input: &str) -> std::io::Result<()> {
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

    eprintln!("[mdrcel-trace] {} -> {}", sha_label, path.display());
    let html = fs::read_to_string(&path)?;

    let mut out = String::new();
    out.push_str(&format!(
        "# M10 Phase 2G — mdrcel pipeline trace\n# fixture: {}\n# sha: {}\n# html bytes: {}\n",
        path.display(),
        sha_label,
        html.len()
    ));

    // -----------------------------------------------------------------
    // S0 — post-Dom::parse
    // -----------------------------------------------------------------
    let dom = Dom::parse(&html);
    let root = dom.document();
    let body = dom.body().expect("body");
    report_stage(&mut out, "S0 post-Dom::parse <body>", &body, "<body> subtree");
    report_stage(&mut out, "S0b post-Dom::parse <root>", &root, "Document root");

    // -----------------------------------------------------------------
    // S1 — post-tree_cleaning
    //
    // bare_extraction_with_cascade does:
    //   tree_cleaning(&html_root, opts)
    //   deep_clone(body) -> cleaned_body_backup
    //   convert_tags(&html_root, opts)
    //   body = dom.body()
    //   extract_content(body, opts)
    //   compare_extraction(cleaned_body_backup, html, own_body, ...)
    // -----------------------------------------------------------------
    let opts = CleanOpts::default();
    let html_root = dom.root_element().expect("root_element");
    tree_cleaning(&html_root, &opts);
    let body_after_clean = dom.body().expect("body");
    report_stage(
        &mut out,
        "S1 post-tree_cleaning <body>",
        &body_after_clean,
        "<body> subtree after tree_cleaning",
    );

    // -----------------------------------------------------------------
    // S2 — deep_clone for cleaned_body_backup (taken BEFORE convert_tags)
    // -----------------------------------------------------------------
    let cleaned_body_backup = deep_clone(&body_after_clean);
    report_stage(
        &mut out,
        "S2 deep_clone -> cleaned_body_backup",
        &cleaned_body_backup,
        "deep-cloned <body> (snapshot for compare_extraction)",
    );

    // -----------------------------------------------------------------
    // S3 — post-convert_tags
    // -----------------------------------------------------------------
    convert_tags(&html_root, &opts);
    let body_after_convert = dom.body().expect("body");
    report_stage(
        &mut out,
        "S3 post-convert_tags <body>",
        &body_after_convert,
        "<body> subtree after convert_tags",
    );

    // -----------------------------------------------------------------
    // S4 — BODY_XPATH match + scope check
    //
    // Inside _extract, the loop calls `xpath_engine::evaluate(expr, tree)`
    // where `tree` is the cleaned body. We replicate that and report what
    // subtree gets picked + whether it contains the spam div as a
    // descendant.
    // -----------------------------------------------------------------
    let body_for_xpath = dom.body().expect("body");
    match locate_body_subtree(&body_for_xpath) {
        Some((i, snippet, subtree)) => {
            out.push_str(&format!(
                "\n## S4 BODY_XPATH selection\n  matched expression index: {i}\n  expr (first 80 chars): {snippet}…\n",
            ));
            let tag = local_name(&subtree).unwrap_or_default();
            out.push_str(&format!(
                "  selected subtree: <{tag}> {}\n",
                id_or_class(&subtree)
            ));
            report_stage(
                &mut out,
                "S4 BODY_XPATH subtree (descendant-or-self scope)",
                &subtree,
                "the subtree _extract's prune_unwanted_sections runs on",
            );

            // S4b — apply prune_unwanted_sections on this subtree (the SAME
            // call _extract makes at main_extractor.py:584).
            let mut potential_tags: HashSet<String> = HashSet::new();
            for t in ["p", "head", "ref", "hi", "lb", "list", "item", "cell", "quote", "code"] {
                potential_tags.insert(t.to_string());
            }
            // table tags added per options.tables (true by default)
            if opts.tables {
                for t in ["table", "td", "th", "tr"] {
                    potential_tags.insert(t.to_string());
                }
            }
            let pruned =
                main_extractor::prune_unwanted_sections(&subtree, &potential_tags, &opts);
            report_stage(
                &mut out,
                "S4b post-prune_unwanted_sections on BODY_XPATH subtree",
                &pruned,
                "pruned subtree (after OVERALL_DISCARD + others)",
            );
        }
        None => {
            out.push_str(
                "\n## S4 BODY_XPATH selection\n  NO MATCH for any of the 5 BODY_XPATH expressions\n",
            );
        }
    }

    // -----------------------------------------------------------------
    // S5/S6 — Run the actual extract_content end-to-end (uses a fresh
    // parse so we don't double-prune the in-place tree above).
    // -----------------------------------------------------------------
    let dom2 = Dom::parse(&html);
    let root2 = dom2.root_element().expect("root_element");
    let body2_pre = dom2.body().expect("body");
    tree_cleaning(&root2, &opts);
    let backup2 = deep_clone(&dom2.body().expect("body"));
    convert_tags(&root2, &opts);
    let body2 = dom2.body().expect("body");

    let (own_body, own_text, own_len) =
        main_extractor::extract_content(&body2, &opts);
    let _ = body2_pre;
    report_stage(
        &mut out,
        "S5/S6 extract_content -> own_body (post strip_elements(done) + strip_tags(div))",
        &own_body,
        "own-arm result body",
    );
    out.push_str(&format!("  own_text chars: {}\n", own_len));
    out.push_str(&format!(
        "  own_text first 200 chars: {:?}\n",
        own_text.chars().take(200).collect::<String>()
    ));

    // -----------------------------------------------------------------
    // S6b — try_readability(html) in isolation.
    // -----------------------------------------------------------------
    let readability_only = readability_fork::try_readability(&html);
    match &readability_only {
        Some(b) => {
            report_stage(
                &mut out,
                "S6b try_readability(html) — readability arm in isolation",
                b,
                "readability arm raw output",
            );
            let ro_text = dom::text_content(b);
            out.push_str(&format!("  readability_text chars: {}\n", ro_text.chars().count()));
            out.push_str(&format!(
                "  readability_text first 200 chars: {:?}\n",
                ro_text.chars().take(200).collect::<String>()
            ));
            let ro_html = serialize_html(b);
            out.push_str(&format!("  readability_html length: {}\n", ro_html.len()));
            out.push_str(&format!(
                "  readability_html first 400 chars: {:?}\n",
                ro_html.chars().take(400).collect::<String>()
            ));
        }
        None => out.push_str("\n## S6b try_readability returned None\n"),
    }
    let _ = readability_only;

    // -----------------------------------------------------------------
    // S7 — compare_extraction winning body (full cascade)
    // -----------------------------------------------------------------
    let (winning_body, winning_text, winning_len) =
        readability_fork::compare_extraction(&backup2, &html, &own_body, own_text.clone(), own_len, &opts);
    report_stage(
        &mut out,
        "S7 compare_extraction -> winning_body",
        &winning_body,
        "cascade-winning body",
    );
    out.push_str(&format!("  winning_text chars: {}\n", winning_len));
    out.push_str(&format!(
        "  winning_text first 200 chars: {:?}\n",
        winning_text.chars().take(200).collect::<String>()
    ));

    // -----------------------------------------------------------------
    // S8 — Serialize winning body. Search for leaked literal substrings
    // (the canonical Phase 2F leak markers).
    // -----------------------------------------------------------------
    let serialized = serialize_html(&winning_body);
    out.push_str("\n## S8 serialised winning body (substring leak markers)\n");
    let markers = [
        "pl_css_ganrao",
        "display: none",
        "display:none",
        "<rt ",
        "<td ",
        "<tr ",
        "<th ",
        "<source ",
        "<fieldset ",
        "<acronym ",
        "<dfn ",
        "<cite ",
        "<tbody ",
        "<tfoot ",
    ];
    for m in &markers {
        let n = serialized.matches(m).count();
        if n > 0 {
            out.push_str(&format!("  {:?}: {n}\n", m));
        }
    }
    out.push_str(&format!("  serialized length: {}\n", serialized.len()));
    out.push_str(&format!(
        "  serialized first 400 chars: {:?}\n",
        serialized.chars().take(400).collect::<String>()
    ));

    // Also call extract_to_xml end-to-end and substring-check.
    let xml_opts = readex::Options::default();
    let xml_out = readex::extract_to_xml(&html, None, &xml_opts);
    out.push_str("\n## S9 extract_to_xml end-to-end\n");
    match xml_out {
        Ok(s) => {
            out.push_str(&format!("  ok, {} bytes\n", s.len()));
            for m in &markers {
                let n = s.matches(m).count();
                if n > 0 {
                    out.push_str(&format!("  {:?}: {n}\n", m));
                }
            }
            // Save the actual XML too for direct inspection.
            let xml_path =
                Path::new("/tmp/m10_phase2g").join(format!("{}_mdrcel_extract_to_xml.xml", sha_label));
            fs::create_dir_all("/tmp/m10_phase2g")?;
            fs::write(&xml_path, &s)?;
            eprintln!("[mdrcel-trace] wrote {}", xml_path.display());
        }
        Err(e) => out.push_str(&format!("  err: {:?}\n", e)),
    }

    let out_dir = Path::new("/tmp/m10_phase2g");
    fs::create_dir_all(out_dir)?;
    let out_path = out_dir.join(format!("{}_mdrcel_trace.txt", sha_label));
    fs::write(&out_path, &out)?;
    eprintln!("[mdrcel-trace] wrote {}", out_path.display());
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        eprintln!(
            "usage: cargo run --release --example m10_phase2g_pipeline_trace -- <sha|path>"
        );
        std::process::exit(2);
    }
    let mut err = false;
    for a in &args {
        if let Err(e) = process(a) {
            eprintln!("ERROR processing {a}: {e}");
            err = true;
        }
    }
    if err {
        std::process::exit(1);
    }
}
