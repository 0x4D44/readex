//! `baseline` — Stage 1c: `baseline()` rescue extractor + `html2txt()` last
//! resort + `basic_cleaning()` pre-strip.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.3.
//! Source of truth: `trafilatura@v2.0.0/baseline.py` (lines 18-123) plus the
//! `BASIC_CLEAN_XPATH` literal at `settings.py:432-434`, and the `trim`
//! helper at `utils.py:340-346`.
//!
//! # What this module does (one paragraph)
//!
//! Given a `&str` of HTML, **`baseline`** returns a `(<body>, text, length)`
//! triplet for use as the *first* rescue answer when the own-extractor (Stage
//! 2+) and the readability-fork (Stage 4+) both come up empty. It walks four
//! extraction paths in source order, each a faithful port of
//! `baseline.py:25-101`:
//!
//! 1. **JSON-LD `articleBody`** (lines 43-59): scan every `<script type=
//!    "application/ld+json">` for `articleBody`; concatenate the matches; if
//!    the total exceeds 100 chars, return it.
//! 2. **`<article>` text** (lines 65-72): post-`basic_cleaning`, every
//!    `<article>` element's `text_content` over 100 chars is collected.
//! 3. **Tag-set tree iteration** (lines 75-86): `tree.iter('blockquote',
//!    'code', 'p', 'pre', 'q', 'quote')` — every matching element's
//!    `text_content`, deduped via a `HashSet`.
//! 4. **`<body>` itertext fallback** (lines 88-96): `'\n'.join(trim(t) for t
//!    in body.itertext() if trim(t))`.
//! 5. **`html2txt` last resort** (lines 98-101): for the rare case `<body>` is
//!    None (a malformed parse), run `html2txt(tree, clean=False)`.
//!
//! **`basic_cleaning`** (lines 18-22) deletes every `<aside>`, footer-class /
//! footer-id `<div>`, `<footer>`, `<script>`, and `<style>` in the tree, using
//! the Stage 0b XPath engine on the literal `BASIC_CLEAN_XPATH` string from
//! `settings.py:432-434`. It MUST execute between paths 1 and 2 — DA-B-2
//! anti-inversion: even when JSON-LD yielded short text, the side effect of
//! `basic_cleaning` is needed before path 2 (`<article>`) sees the tree.
//!
//! **`html2txt`** (lines 104-123) parses `content`, finds `<body>`, optionally
//! runs `basic_cleaning` on it (clean=true; the default — but
//! `baseline.py:99` calls it with `clean=false`), then returns
//! `" ".join(body.text_content().split()).strip()`. Reachable as the very last
//! fallback inside `baseline` AND as a public entry point in its own right
//! (Trafilatura's `core.bare_extraction` uses it as a final rescue).
//!
//! # Faithfulness anchor (HLD §10 / anti-inversion)
//!
//! Every non-trivial function carries a `// trafilatura@v2.0.0/baseline.py:NN-MM`
//! cite. The four extraction paths execute **in source order** with full side
//! effects — no short-circuit-after-JSON-LD optimisation (DA-B-2). The
//! `trim` helper open-codes `utils.py:340-346` without the Python `lru_cache`
//! (a cache is irrelevant in Rust here).
//!
//! # Output shape
//!
//! `BaselineOutput { postbody, text, length }` mirrors Python's
//! `(body_element, text, len(text))`. `postbody` is an owned `<body>`
//! `NodeRef` belonging to a **freshly-constructed** detached element graph —
//! it is NOT a node lifted out of the input tree. The Stage 2+ pipeline
//! consumes `text`; `postbody` exists for `xmltotxt` parity with
//! Trafilatura's pipeline.
//!
//! # Empty / malformed input (Bug-E2 doctrine)
//!
//! `baseline("")` and any input that fails to parse to a usable tree returns
//! `BaselineOutput { postbody: <empty body>, text: "", length: 0 }`. No
//! `Result`, no `Err` — Trafilatura's `load_html` returning `None` is a
//! happy-path `(<body>, "", 0)` per `baseline.py:38-39`.

use std::collections::HashSet;

use crate::readability::dom::{
    self, Dom, NodeData, NodeRef, append_child, create_element, create_text_node,
    delete_with_tail_preserve_free, text_content,
};
use crate::trafilatura::xpath_engine;

/// Output of [`baseline`] — `(body_element, text, len(text))` from
/// `baseline.py:25` translated to a struct.
///
/// `postbody` is the freshly-created `<body>` element with one or more `<p>`
/// children carrying the extracted text. It is owned by a detached element
/// graph (NOT borrowed from the input tree), so the caller may further
/// manipulate it freely.
#[derive(Debug, Clone)]
pub struct BaselineOutput {
    /// The synthetic `<body>` element. Always non-null; on empty input it is
    /// an empty `<body>` with no children (`baseline.py:38-39`).
    pub postbody: NodeRef,
    /// The extracted text. `String::new()` on empty / malformed input
    /// (Bug-E2 doctrine — never `Err`).
    pub text: String,
    /// `text.chars().count()` is **NOT** what we want — Python's `len(text)`
    /// on a `str` is `len-in-codepoints` only for Python 3 `str`; the public
    /// contract here is "string length as Trafilatura measures it", which is
    /// `len(text)` on a Python 3 `str` → number of **codepoints** (because
    /// `str` in Py3 is already decoded). Bytes vs chars only diverges on
    /// non-ASCII input; we follow Python's `len(str)` semantic — chars.
    pub length: usize,
}

// ===========================================================================
// trim (utils.py:340-346)
// ===========================================================================

/// `trim(s)` from `utils.py:340-346`.
///
/// Python:
/// ```python
/// def trim(string: str) -> str:
///     try:
///         return " ".join(string.split()).strip()
///     except (AttributeError, TypeError):
///         return ""
/// ```
///
/// The Python `str.split()` (no arg) splits on **any** Unicode whitespace and
/// drops empty parts. We follow Rust's `str::split_whitespace`, which
/// implements the Unicode `White_Space` property — the **same set** Python's
/// no-arg `split()` uses (both delegate to Unicode whitespace, including
/// `\t \n \r \x0b \x0c` plus `\xa0`, ` `, ` `, the Zs class, …).
/// The `.strip()` is a no-op after `" ".join(...)` on a list of non-empty
/// trimmed pieces, but Python keeps it; we faithfully append it for behaviour
/// parity.
///
/// `AttributeError`/`TypeError` branch (None / non-str) is dead in Rust —
/// `&str` is the only input shape.
fn trim(s: &str) -> String {
    let joined: Vec<&str> = s.split_whitespace().collect();
    let out = joined.join(" ");
    // .strip() — also strip any residual leading/trailing whitespace. After
    // split_whitespace + join(" "), the only way this can have edge whitespace
    // is on an empty/whitespace-only input (which produces ""), so this is a
    // belt-and-braces faithful mirror.
    out.trim().to_string()
}

// ===========================================================================
// basic_cleaning (baseline.py:18-22)
// ===========================================================================

/// Remove a few section types from the document.
///
/// Source: `baseline.py:18-22` (the Python function name is `basic_cleaning`):
/// ```python
/// def basic_cleaning(tree: HtmlElement) -> HtmlElement:
///     for elem in BASIC_CLEAN_XPATH(tree):
///         delete_element(elem)
///     return tree
/// ```
///
/// The `BASIC_CLEAN_XPATH` literal at `settings.py:432-434`:
/// ```python
/// BASIC_CLEAN_XPATH = XPath(
///     ".//aside|.//div[contains(@class|@id, 'footer')]|.//footer|.//script|.//style"
/// )
/// ```
///
/// is fed verbatim to the Stage 0b XPath engine; `delete_element` is the
/// `dom::delete_with_tail_preserve_free` Stage 1b additive symbol, which open-
/// codes `xml.py:54-70`'s tail-preserving deletion exactly.
///
/// Mutates `tree` in place. Python returns `tree`; the Rust caller already
/// holds the `NodeRef`.
pub fn basic_cleaning(tree: &NodeRef) {
    // settings.py:432-434 — fed literally to xpath_engine.
    const BASIC_CLEAN_XPATH: &str =
        ".//aside|.//div[contains(@class|@id, 'footer')]|.//footer|.//script|.//style";
    // baseline.py:20-21
    let matches = match xpath_engine::evaluate(BASIC_CLEAN_XPATH, tree) {
        Ok(v) => v,
        // The XPath is a compile-time constant; an Err here is a Stage 0b
        // engine regression, not a runtime user error. Surface it as "no
        // matches" (empty sweep) so the caller still gets a valid tree; the
        // engine's own conformance harness pins the contract.
        Err(_) => return,
    };
    for elem in matches {
        // xml.py:54-70 delete_element with keep_tail=True (the default).
        delete_with_tail_preserve_free(&elem);
    }
}

// ===========================================================================
// baseline (baseline.py:25-101)
// ===========================================================================

/// Use baseline extraction targeting text paragraphs and/or JSON metadata.
///
/// Source: `baseline.py:25-101`. Returns a [`BaselineOutput`] containing the
/// synthetic `<body>`, the extracted text, and its length. Bug-E2 doctrine
/// holds — empty or malformed input is a valid `BaselineOutput { ..., length:
/// 0 }`, never an `Err`.
///
/// # The four-path order is load-bearing
///
/// 1. JSON-LD `articleBody` scan (43-59). Returns early if `>100` chars.
/// 2. `basic_cleaning(tree)` — DA-B-2: this MUST run regardless of path 1
///    result; subsequent paths operate on the cleaned tree.
/// 3. `<article>` text scan (65-72). Returns early if any `<article>` text
///    was added to postbody.
/// 4. Tag-set tree iteration (75-86): `blockquote`/`code`/`p`/`pre`/`q`/
///    `quote`. Returns early if combined text `>100` chars.
/// 5. `<body>` itertext join (88-96). Returns if `<body>` exists.
/// 6. `html2txt(tree, clean=False)` last resort (98-101).
pub fn baseline(filecontent: &str) -> BaselineOutput {
    // baseline.py:36 — tree = load_html(filecontent)
    // baseline.py:37 — postbody = Element('body')
    let mut postbody = create_element("body");

    // baseline.py:38-39 — if tree is None: return postbody, '', 0
    // In Rust we treat empty input AND any "no parseable tree" case as the
    // None branch. html5ever produces a tree even for "", but the body
    // will be empty and the rest of the paths produce nothing — but to be
    // faithful to the early-return branch and to avoid running the XPath
    // engine over a synthetic empty <html><head></head><body></body> we
    // short-circuit on empty input.
    if filecontent.is_empty() {
        return BaselineOutput {
            postbody,
            text: String::new(),
            length: 0,
        };
    }

    let dom_handle = Dom::parse(filecontent);
    // tree_root is what Python calls `tree` — the lxml HtmlElement at the
    // root of the parsed document, typically `<html>`. We use Dom's root
    // element. If html5ever failed to synthesise <html> (effectively
    // impossible for a real parse) we fall through to the load_html-None
    // branch.
    let Some(tree_root) = dom_handle.root_element() else {
        return BaselineOutput {
            postbody,
            text: String::new(),
            length: 0,
        };
    };

    // ----------------------------------------------------------------------
    // baseline.py:42-59 — JSON-LD articleBody scan
    // ----------------------------------------------------------------------
    let mut temp_text = String::new();
    // baseline.py:43 — for elem in tree.iterfind('.//script[@type="application/ld+json"]')
    let scripts = xpath_engine::evaluate(".//script[@type=\"application/ld+json\"]", &tree_root)
        .unwrap_or_default();
    for elem in &scripts {
        // baseline.py:44 — if elem.text and 'articleBody' in elem.text
        // lxml's `elem.text` is the leading-text run of the script element's
        // children. In our facade, since rcdom parses the contents of a
        // <script> as a single Text node, dom::text_content(elem) is
        // identical to lxml's `elem.text` here.
        let raw = text_content(elem);
        if raw.is_empty() || !raw.contains("articleBody") {
            continue;
        }
        // baseline.py:45-48 — try: json_body = json.loads(...).get("articleBody", "")
        //                      except Exception:
        //                          json_body = ""
        //
        // The catch-all swallows JSONDecodeError AND the AttributeError that
        // arises when the JSON root is an array (`[1,2,3].get(...)` fails on
        // lists). We faithfully mirror by treating ANY non-object root or
        // missing key as an empty body.
        let parsed: Option<serde_json::Value> = serde_json::from_str(&raw).ok();
        let json_body: String = parsed
            .as_ref()
            .and_then(|v| v.as_object())
            .and_then(|o| o.get("articleBody"))
            .and_then(|b| b.as_str())
            .map(String::from)
            .unwrap_or_default();
        // baseline.py:49 — if json_body:
        if json_body.is_empty() {
            continue;
        }
        // baseline.py:50-54
        let text = if json_body.contains("<p>") {
            // parsed = load_html(json_body); text = trim(parsed.text_content())
            //                                       if parsed is not None else ""
            // Parse the json_body as a fresh HTML document and take its
            // text_content. html5ever wraps the fragment in <html><body>...;
            // text_content over the root yields the same characters lxml
            // would after `load_html` + `text_content`.
            let inner_dom = Dom::parse(&json_body);
            match inner_dom.root_element() {
                Some(r) => trim(&text_content(&r)),
                None => String::new(),
            }
        } else {
            // baseline.py:54 — text = trim(json_body)
            trim(&json_body)
        };
        // baseline.py:55 — SubElement(postbody, 'p').text = text
        append_text_paragraph(&postbody, &text);
        // baseline.py:56 — temp_text += " " + text if temp_text else text
        if temp_text.is_empty() {
            temp_text = text;
        } else {
            temp_text.push(' ');
            temp_text.push_str(&text);
        }
    }
    // baseline.py:58-59 — if len(temp_text) > 100: return postbody, temp_text, len
    if temp_text.chars().count() > 100 {
        let length = temp_text.chars().count();
        return BaselineOutput {
            postbody,
            text: temp_text,
            length,
        };
    }

    // ----------------------------------------------------------------------
    // baseline.py:61 — tree = basic_cleaning(tree)
    //
    // DA-B-2 anti-inversion: the side effect of basic_cleaning is part of
    // the contract even when JSON-LD yielded short text. The <article> and
    // tag-set passes below run on the post-cleaning tree.
    // ----------------------------------------------------------------------
    basic_cleaning(&tree_root);

    // ----------------------------------------------------------------------
    // baseline.py:63-72 — <article> tag scan
    // ----------------------------------------------------------------------
    let mut temp_text = String::new();
    // baseline.py:65 — for article_elem in tree.iterfind('.//article')
    let articles = xpath_engine::evaluate(".//article", &tree_root).unwrap_or_default();
    for article_elem in &articles {
        // baseline.py:66 — text = trim(article_elem.text_content())
        let text = trim(&text_content(article_elem));
        // baseline.py:67 — if len(text) > 100
        if text.chars().count() > 100 {
            // 68 — SubElement(postbody, 'p').text = text
            append_text_paragraph(&postbody, &text);
            // 69 — temp_text += " " + text if temp_text else text
            if temp_text.is_empty() {
                temp_text.clone_from(&text);
            } else {
                temp_text.push(' ');
                temp_text.push_str(&text);
            }
        }
    }
    // baseline.py:70-72 — if len(postbody) > 0: return postbody, temp_text, len
    //
    // lxml's `len(postbody)` is the count of element children of `postbody`.
    // We count <p> children directly (postbody's only child shape).
    if element_child_count(&postbody) > 0 {
        let length = temp_text.chars().count();
        return BaselineOutput {
            postbody,
            text: temp_text,
            length,
        };
    }

    // ----------------------------------------------------------------------
    // baseline.py:74-86 — tag-set tree iteration
    // ----------------------------------------------------------------------
    // baseline.py:75 — results = set()
    let mut results: HashSet<String> = HashSet::new();
    // baseline.py:76 — temp_text = ""
    let mut temp_text = String::new();
    // baseline.py:78 — for element in tree.iter('blockquote','code','p','pre','q','quote'):
    //
    // This is lxml's `Element.iter(*tags)` — a document-order pre-order walk
    // over ALL descendants (and self) yielding only those whose local-name
    // is in the tag set. Stage 0a's `Dom::document_order_triplets` walks
    // elements in pre-order with `(elem, .text, .tail)`; we use it here as
    // the Rust-equivalent of `tree.iter(...)` filtered by lowercase
    // local-name in the catalog.
    let tag_set: &[&str] = &["blockquote", "code", "p", "pre", "q", "quote"];
    let candidates = iter_elements_by_tags(&dom_handle, &tree_root, tag_set);
    for element in &candidates {
        // baseline.py:79 — entry = trim(element.text_content())
        let entry = trim(&text_content(element));
        // baseline.py:80 — if entry not in results
        if !results.contains(&entry) {
            // 81 — SubElement(postbody, 'p').text = entry
            append_text_paragraph(&postbody, &entry);
            // 82 — temp_text += " " + entry if temp_text else entry
            if temp_text.is_empty() {
                temp_text.clone_from(&entry);
            } else {
                temp_text.push(' ');
                temp_text.push_str(&entry);
            }
            // 83 — results.add(entry)
            results.insert(entry);
        }
    }
    // baseline.py:85-86 — if len(temp_text) > 100: return postbody, temp_text, len
    if temp_text.chars().count() > 100 {
        let length = temp_text.chars().count();
        return BaselineOutput {
            postbody,
            text: temp_text,
            length,
        };
    }

    // ----------------------------------------------------------------------
    // baseline.py:88-96 — default strategy: clean the tree and take everything
    //
    // NB: baseline.py:89 re-assigns `postbody = Element('body')`, discarding
    // any <p> children appended by the tag-set pass above. We mirror that
    // RESET — the path-4 output replaces (not appends to) prior path output.
    // ----------------------------------------------------------------------
    postbody = create_element("body");
    // baseline.py:90 — body_elem = tree.find('.//body')
    let body_elem = xpath_engine::evaluate(".//body", &tree_root)
        .unwrap_or_default()
        .into_iter()
        .next();
    if let Some(body_elem) = body_elem {
        // baseline.py:92 — p_elem = SubElement(postbody, 'p')
        let p_elem = create_element("p");
        // baseline.py:94 — text_elems = [trim(e) for e in body_elem.itertext()]
        let text_elems: Vec<String> = itertext(&body_elem).iter().map(|s| trim(s)).collect();
        // baseline.py:95 — p_elem.text = '\n'.join([e for e in text_elems if e])
        let joined = text_elems
            .into_iter()
            .filter(|e| !e.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        // Set p_elem.text by appending a Text child (the lxml way to set
        // Element.text is to mutate `_element.text = "..."`; in rcdom we add
        // a single leading Text child).
        if !joined.is_empty() {
            let txt = create_text_node(&joined);
            append_child(&p_elem, &txt);
        }
        append_child(&postbody, &p_elem);
        // baseline.py:96 — return postbody, p_elem.text, len(p_elem.text)
        //
        // p_elem.text is `joined` (the same string). len() is char-count.
        let length = joined.chars().count();
        return BaselineOutput {
            postbody,
            text: joined,
            length,
        };
    }

    // ----------------------------------------------------------------------
    // baseline.py:98-101 — new fallback: html2txt(tree, clean=False)
    // ----------------------------------------------------------------------
    // baseline.py:99 — text = html2txt(tree, clean=False)
    //
    // At this point Python has a `tree` HtmlElement but no `<body>` child;
    // html2txt(tree, ...) calls `load_html(content)` on its argument. When
    // the argument is already a parsed tree, lxml's `load_html` returns it
    // as-is; for our purposes we already hold the same tree, so we call the
    // internal helper that takes the parsed tree.
    let text = html2txt_from_tree(&tree_root, false);
    // baseline.py:100 — SubElement(postbody, 'p').text = text
    append_text_paragraph(&postbody, &text);
    let length = text.chars().count();
    // baseline.py:101 — return postbody, text, len(text)
    BaselineOutput {
        postbody,
        text,
        length,
    }
}

// ===========================================================================
// html2txt (baseline.py:104-123)
// ===========================================================================

/// Run basic html2txt on a document.
///
/// Source: `baseline.py:104-123`. Public entry point. Reachable from
/// [`baseline`] at the path-5 last-resort step (with `clean=false`) AND as a
/// standalone "give me the text" helper.
///
/// ```python
/// def html2txt(content: Any, clean: bool = True) -> str:
///     tree = load_html(content)
///     if tree is None:
///         return ""
///     body = tree.find(".//body")
///     if body is None:
///         return ""
///     if clean:
///         body = basic_cleaning(body)
///     return " ".join(body.text_content().split()).strip()
/// ```
///
/// The final `" ".join(...).split().strip()` is exactly [`trim`].
pub fn html2txt(content: &str, clean: bool) -> String {
    // baseline.py:115 — tree = load_html(content)
    if content.is_empty() {
        // baseline.py:116-117 — if tree is None: return ""
        return String::new();
    }
    let dom_handle = Dom::parse(content);
    let Some(root) = dom_handle.root_element() else {
        return String::new();
    };
    html2txt_from_tree(&root, clean)
}

/// Internal `html2txt` variant that takes an already-parsed tree root.
/// Equivalent to `html2txt(content, clean)` when `content` has already been
/// `load_html`'d — the path-5 invocation inside [`baseline`] uses this so it
/// does NOT re-parse the input HTML for the last-resort step (Python's
/// `load_html` is a no-op when given an already-parsed tree).
fn html2txt_from_tree(tree_root: &NodeRef, clean: bool) -> String {
    // baseline.py:118 — body = tree.find(".//body")
    let body = xpath_engine::evaluate(".//body", tree_root)
        .unwrap_or_default()
        .into_iter()
        .next();
    // baseline.py:119-120 — if body is None: return ""
    let Some(body) = body else {
        return String::new();
    };
    // baseline.py:121-122 — if clean: body = basic_cleaning(body)
    if clean {
        basic_cleaning(&body);
    }
    // baseline.py:123 — return " ".join(body.text_content().split()).strip()
    trim(&text_content(&body))
}

// ===========================================================================
// Helpers — itertext, append_text_paragraph, iter_elements_by_tags
// ===========================================================================

/// `Element.itertext()` from lxml — yields, in document order:
///
/// 1. `elem.text` if non-`None` (the leading Text-child run);
/// 2. Recursively, for each child element `c`: `c.itertext()` followed by
///    `c.tail` if non-`None`.
///
/// Critically, the element's **own** `.tail` is NOT emitted (that belongs to
/// `elem`'s parent's `itertext`, not `elem`'s).
///
/// In rcdom terms: do a pre-order walk that emits each Text-node's `data`
/// in the order encountered, with one exception — the very last Text-run
/// AFTER `elem`'s last element-child (which would be `elem`'s child's
/// `.tail` if seen as a child; or `elem`'s OWN `.tail` if the run sits
/// outside `elem`). We achieve this by walking `elem`'s `children` only,
/// recursing into every element child, and emitting Text-node `data` as we
/// hit it.
pub(crate) fn itertext(elem: &NodeRef) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    collect_itertext(elem, &mut out);
    out
}

fn collect_itertext(node: &NodeRef, out: &mut Vec<String>) {
    // Walk `node`'s children in document order. Text children contribute
    // their data; element children recurse (which emits the element's own
    // descendant text AND, on returning, the element's `.tail` Text-run
    // — which is captured by the parent walk that handles the sibling Text
    // nodes right after the element).
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let data = contents.borrow().to_string();
                if !data.is_empty() {
                    out.push(data);
                }
            }
            NodeData::Element { .. } => {
                collect_itertext(child, out);
            }
            // Comments / PIs / Doctype contribute no text (matching lxml's
            // itertext which skips non-Text non-Element children).
            _ => {}
        }
    }
}

/// `SubElement(parent, 'p').text = text` — the `baseline.py` shape at lines
/// 55, 68, 81, 92, 100. Create a `<p>`, hang it under `parent`, set its
/// leading text by appending a single Text child.
///
/// Faithful mirror of lxml's `Element.text = ...` semantic via rcdom: lxml
/// stores `.text` as a virtual "leading-text-child run"; in rcdom we
/// materialise the same shape as one Text child.
fn append_text_paragraph(parent: &NodeRef, text: &str) {
    let p = create_element("p");
    if !text.is_empty() {
        let txt = create_text_node(text);
        append_child(&p, &txt);
    }
    append_child(parent, &p);
}

/// Count of **element** children of `node`. Matches lxml's `len(elem)` which
/// counts element children only (lxml's Element is element-only;
/// `len(elem)` reports `len(elem.iterchildren())` over elements).
fn element_child_count(node: &NodeRef) -> usize {
    node.children
        .borrow()
        .iter()
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .count()
}

/// lxml `tree.iter(*tags)` — document-order pre-order walk yielding every
/// descendant element whose lowercase local-name is in `tags`, **plus
/// `root` itself if its tag matches**.
///
/// This is the `tree.iter('blockquote','code','p','pre','q','quote')` shape
/// at `baseline.py:78`. The substrate is `Dom::document_order_triplets`
/// (Stage 0a — HLD §5.1 / §6.0), filtered by lowercase local-name set
/// membership.
fn iter_elements_by_tags(dom: &Dom, root: &NodeRef, tags: &[&str]) -> Vec<NodeRef> {
    let want: HashSet<&str> = tags.iter().copied().collect();
    let triplets = dom.document_order_triplets(root);
    triplets
        .into_iter()
        .filter_map(|(elem, _t, _tl)| {
            let tag = dom::local_name(&elem)?;
            if want.contains(tag.as_str()) {
                Some(elem)
            } else {
                None
            }
        })
        .collect()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::{get_attribute, get_elements_by_tag_name, tag_name};

    // --- XPath engine sanity checks (per Stage 1c brief) ------------------

    #[test]
    fn xpath_engine_handles_jsonld_script_selector() {
        // Sanity check — Stage 1c relies on this XPath shape working.
        let dom = Dom::parse(
            r#"<html><body><script type="application/ld+json">{}</script></body></html>"#,
        );
        let root = dom.root_element().unwrap();
        let r = xpath_engine::evaluate(".//script[@type=\"application/ld+json\"]", &root)
            .expect("XPath should parse");
        assert_eq!(r.len(), 1);
        assert_eq!(tag_name(&r[0]).as_deref(), Some("SCRIPT"));
    }

    #[test]
    fn xpath_engine_handles_basic_clean_xpath() {
        // Sanity check — the BASIC_CLEAN_XPATH literal must select an
        // <aside>, a footer-class <div>, and a <footer>.
        let dom = Dom::parse(
            r#"<html><body>
                <aside>side</aside>
                <div class="footer">f1</div>
                <footer>f2</footer>
                <p>keep</p>
              </body></html>"#,
        );
        let root = dom.root_element().unwrap();
        let r = xpath_engine::evaluate(
            ".//aside|.//div[contains(@class|@id, 'footer')]|.//footer|.//script|.//style",
            &root,
        )
        .expect("XPath should parse");
        // Expect: aside, div.footer, footer (3 matches).
        let tags: Vec<String> = r.iter().filter_map(dom::local_name).collect();
        // Order-independent assertion (XPath unions are document-ordered,
        // but the test only cares about presence).
        assert!(tags.contains(&"aside".to_string()));
        assert!(tags.contains(&"div".to_string()));
        assert!(tags.contains(&"footer".to_string()));
        assert_eq!(r.len(), 3);
    }

    // --- baseline tests (per Stage 1c brief) ------------------------------

    #[test]
    fn empty_input_yields_empty_baseline_output() {
        let out = baseline("");
        assert_eq!(out.length, 0);
        assert_eq!(out.text, "");
        // postbody is the empty <body> Element.
        assert_eq!(tag_name(&out.postbody).as_deref(), Some("BODY"));
        // No <p> children.
        assert_eq!(get_elements_by_tag_name(&out.postbody, "p").len(), 0);
    }

    #[test]
    fn jsonld_articlebody_string_path() {
        // articleBody is a single string of >100 chars (path 1 short-circuits).
        let body = "Lorem ipsum dolor sit amet ".repeat(10); // ~270 chars
        let raw_json = format!(r#"{{"articleBody":"{}"}}"#, body.trim_end());
        let html = format!(
            r#"<html><body><script type="application/ld+json">{}</script></body></html>"#,
            raw_json
        );
        let out = baseline(&html);
        assert!(out.length > 100, "expected >100 chars, got {}", out.length);
        assert!(out.text.contains("Lorem ipsum"));
        // postbody has one <p>.
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn jsonld_articlebody_html_path() {
        // articleBody contains "<p>" — parsed via fresh load_html.
        let inner = "<p>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam.</p>";
        // Serialize "<p>" via JSON escaping — produces "<p>..." or
        // "<p>..." per serde_json defaults; serde_json::to_string emits "<p>"
        // (no escape). Either way the JSON's articleBody string includes "<p>".
        let raw_json = serde_json::json!({ "articleBody": inner }).to_string();
        let html = format!(
            r#"<html><body><script type="application/ld+json">{}</script></body></html>"#,
            raw_json
        );
        let out = baseline(&html);
        // Path 1 returns >100 chars after trim. Trim turns the inner HTML's
        // text content (still over 100 chars after stripping <p>) into a
        // single-spaced string.
        assert!(
            out.length > 100,
            "expected >100 chars after JSON-LD HTML path, got {}",
            out.length
        );
        assert!(out.text.contains("Lorem ipsum"));
    }

    #[test]
    fn jsonld_array_root_swallowed() {
        // articleBody-named-but-not-present scenario: JSON root is an array.
        // Path 1 yields nothing; falls through to paths 2-5.
        let inner_html = "<article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim.</article>";
        let html = format!(
            r#"<html><body><script type="application/ld+json">[1,2,"articleBody",3]</script>{}</body></html>"#,
            inner_html
        );
        let out = baseline(&html);
        // Path 2 (<article>) wins.
        assert!(out.length > 100);
        assert!(out.text.contains("Lorem ipsum"));
        // postbody has at least one <p> (path 2 appends, NOT path 5).
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn article_tag_path() {
        // JSON-LD absent; one <article> with >100 chars.
        let html = "<html><body><article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim.</article></body></html>";
        let out = baseline(html);
        assert!(out.length > 100);
        assert!(out.text.contains("Lorem ipsum"));
        // postbody has one <p>.
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
    }

    #[test]
    fn tag_set_path_dedupes() {
        // JSON-LD absent, <article> absent; multiple <p> with the SAME trimmed
        // text — dedup via HashSet per baseline.py:75.
        // Each <p>'s text is short; total temp_text must exceed 100 chars to
        // exercise path 3 return. We use distinct <p>s plus repeats.
        let html = "<html><body>\
            <p>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</p>\
            <p>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</p>\
            <p>aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa</p>\
            <p>bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb</p>\
            <p>cccccccccccccccccccccccccccccccccccccccccccccc</p>\
        </body></html>";
        let out = baseline(html);
        assert!(
            out.length > 100,
            "expected >100 chars from path 3 (got {})",
            out.length
        );
        // postbody has 3 <p> children (the unique trimmed texts).
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 3, "dedup should produce 3 unique <p>s");
    }

    #[test]
    fn body_fallback_path() {
        // No JSON-LD, no <article>, no qualifying <p>/<blockquote>/etc with
        // >100-chars combined text. The body still has some text.
        // path 4 is exercised when temp_text from path 3 is <= 100 chars.
        let html = "<html><body>short<div>some text<span>tail</span>more</div></body></html>";
        let out = baseline(html);
        // Path 4 runs: postbody has one <p> whose text is '\n'-joined
        // trimmed itertext pieces.
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
        // The text contains the non-empty pieces, '\n'-joined.
        assert!(out.text.contains("short"));
        assert!(out.text.contains("some text"));
        assert!(out.text.contains("tail"));
        assert!(out.text.contains("more"));
        // '\n' separators are present (multiple distinct itertext pieces).
        assert!(out.text.contains('\n'));
    }

    #[test]
    fn html2txt_fallback_when_body_missing() {
        // Direct test of html2txt — confirms path 5 last-resort behaviour.
        // Empty input: returns "".
        assert_eq!(html2txt("", true), "");
        // No <body> case is hard to construct from html5ever (it always
        // synthesises <body>); we test html2txt's direct behaviour on a
        // minimal HTML with text content.
        let s = html2txt("<html><body>  hello   world  </body></html>", false);
        assert_eq!(s, "hello world");
    }

    #[test]
    fn itertext_text_tail_interleaving() {
        // Direct unit test of our itertext implementation.
        // <body>A<p>B</p>C<p>D</p>E</body> -> ["A", "B", "C", "D", "E"]
        let dom = Dom::parse("<html><body>A<p>B</p>C<p>D</p>E</body></html>");
        let body = dom.body().unwrap();
        let pieces = itertext(&body);
        assert_eq!(pieces, vec!["A", "B", "C", "D", "E"]);
    }

    #[test]
    fn itertext_nested_text_tail_interleaving() {
        // Nested: <body>A<p>B<i>X</i>Y</p>C<p>D</p>E</body> -> A,B,X,Y,C,D,E
        let dom = Dom::parse("<html><body>A<p>B<i>X</i>Y</p>C<p>D</p>E</body></html>");
        let body = dom.body().unwrap();
        let pieces = itertext(&body);
        assert_eq!(pieces, vec!["A", "B", "X", "Y", "C", "D", "E"]);
    }

    #[test]
    fn basic_cleaning_drops_footer_aside_script_style() {
        // Round-trip test for basic_cleaning. After cleaning, the named
        // elements are gone from the tree; siblings + non-targeted text
        // survive.
        let dom = Dom::parse(
            r#"<html><body>
                <aside>aside-text</aside>
                <div class="footer">footer-div</div>
                <footer>real-footer</footer>
                <script>js code</script>
                <style>css code</style>
                <p>keep me</p>
              </body></html>"#,
        );
        let root = dom.root_element().unwrap();
        basic_cleaning(&root);
        assert!(get_elements_by_tag_name(&root, "aside").is_empty());
        assert!(get_elements_by_tag_name(&root, "footer").is_empty());
        assert!(get_elements_by_tag_name(&root, "script").is_empty());
        assert!(get_elements_by_tag_name(&root, "style").is_empty());
        // The footer-class <div> is gone — but other <div> elements would
        // survive. Confirm the surviving <p>.
        let ps = get_elements_by_tag_name(&root, "p");
        assert_eq!(ps.len(), 1);
        assert!(text_content(&ps[0]).contains("keep me"));
        // The class="footer" <div> specifically is gone.
        let divs = get_elements_by_tag_name(&root, "div");
        for d in &divs {
            let cls = get_attribute(d, "class").unwrap_or_default();
            assert!(
                !cls.contains("footer"),
                "footer-class div should be removed"
            );
        }
    }

    #[test]
    fn basic_cleaning_drops_footer_id_div() {
        // contains(@class|@id, 'footer') matches by id too.
        let dom = Dom::parse(r#"<html><body><div id="footer">bye</div><p>hi</p></body></html>"#);
        let root = dom.root_element().unwrap();
        basic_cleaning(&root);
        // The footer-id div is gone.
        let divs = get_elements_by_tag_name(&root, "div");
        for d in &divs {
            let id = get_attribute(d, "id").unwrap_or_default();
            assert_ne!(id, "footer");
        }
    }

    #[test]
    fn do_not_shortcircuit_basic_cleaning() {
        // DA-B-2 regression: JSON-LD has articleBody="short" (<= 100 chars)
        // AND an <aside> with text that must be removed by basic_cleaning
        // before subsequent paths see the tree.
        //
        // We assemble HTML where: (a) JSON-LD path runs but produces only
        // ~5 chars (under 100 → no early return); (b) basic_cleaning is
        // required to strip the <aside>; (c) we add an <article> with
        // >100 chars so path 2 fires AFTER basic_cleaning. The assertion:
        // path 2's text does NOT include the <aside>'s content (because
        // basic_cleaning DID run).
        let json_short = r#"{"articleBody":"short"}"#;
        let html = format!(
            r#"<html><body>
                <script type="application/ld+json">{}</script>
                <aside>POISON-FOOTER-CONTENT-DO-NOT-LEAK</aside>
                <article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim.</article>
            </body></html>"#,
            json_short
        );
        let out = baseline(&html);
        // Path 2 should win.
        assert!(out.length > 100);
        // The aside's text MUST NOT leak into the output (basic_cleaning
        // dropped it).
        assert!(
            !out.text.contains("POISON-FOOTER-CONTENT-DO-NOT-LEAK"),
            "basic_cleaning failed to run: aside text leaked into baseline output"
        );
        // Sanity: the article text IS present.
        assert!(out.text.contains("Lorem ipsum"));
    }

    #[test]
    fn html2txt_collapses_whitespace() {
        let s = html2txt("<html><body>   foo \t\n  bar  </body></html>", false);
        assert_eq!(s, "foo bar");
    }

    #[test]
    fn html2txt_clean_true_strips_script() {
        // With clean=true, basic_cleaning runs on body before text_content.
        // We need a footer-class div or aside that would be included
        // otherwise.
        // Note: html2txt feeds .//body to xpath then runs basic_cleaning on
        // body. basic_cleaning then evaluates .//aside on body — which finds
        // body's descendants. Confirm the cleaning works.
        let html = r#"<html><body>main<aside>aside</aside></body></html>"#;
        let with_clean = html2txt(html, true);
        let without_clean = html2txt(html, false);
        // with_clean drops the aside contribution.
        assert_eq!(with_clean, "main");
        // without_clean keeps it.
        assert!(without_clean.contains("main"));
        assert!(without_clean.contains("aside"));
    }

    #[test]
    fn baseline_returns_empty_on_only_whitespace_input() {
        // " \t\n" — Dom::parse accepts it; tree exists, but body is empty.
        // No JSON-LD, no <article>, no <p>; <body> exists but its
        // text_content is "\n" or " \t\n". Path 4 fires; itertext gives
        // one piece (the whitespace) which trim → "". Filter removes
        // empty pieces. p_elem.text = "" → length 0.
        let out = baseline("   \n\t  ");
        // length 0, text "" — path 4 with empty joined string.
        assert_eq!(out.text, "");
        assert_eq!(out.length, 0);
    }

    #[test]
    fn baseline_postbody_is_owned_not_aliased_with_input_tree() {
        // The struct doc says postbody is owned by a freshly-constructed
        // detached element graph. Confirm: mutate the input HTML's body
        // node (we'd have to re-parse to get a handle anyway). Direct check:
        // postbody returned from baseline("") has no parent.
        let out = baseline("");
        assert!(
            dom::parent(&out.postbody).is_none(),
            "postbody must be detached (no parent)"
        );
    }

    #[test]
    fn trim_helper_matches_python_semantics() {
        // " ".join(s.split()).strip()
        assert_eq!(trim("  hello   world  "), "hello world");
        assert_eq!(trim(""), "");
        assert_eq!(trim("   "), "");
        assert_eq!(trim("hello"), "hello");
        // Unicode whitespace handling (NBSP, tab, newline mix).
        assert_eq!(trim("a\u{00A0}b\tc\nd"), "a b c d");
    }

    #[test]
    fn iter_elements_by_tags_walks_document_order() {
        let dom = Dom::parse(
            "<html><body><p>1</p><div><p>2</p><blockquote>3</blockquote></div><pre>4</pre></body></html>",
        );
        let root = dom.root_element().unwrap();
        let r = iter_elements_by_tags(&dom, &root, &["blockquote", "p", "pre"]);
        // Document order: p(1), p(2), blockquote(3), pre(4).
        let tags: Vec<String> = r.iter().filter_map(dom::local_name).collect();
        assert_eq!(tags, vec!["p", "p", "blockquote", "pre"]);
    }

    #[test]
    fn baseline_path4_resets_postbody_then_writes_one_p() {
        // Path 3 may write multiple <p>s, but path 4 RESETS postbody and
        // writes exactly one <p>. We construct an input that fails path 3
        // (no <p>/<blockquote>/.. with usable text totalling >100 chars)
        // and lands in path 4.
        let html = "<html><body>only-body-text-no-paragraph</body></html>";
        let out = baseline(html);
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        // Exactly one <p> from path 4.
        assert_eq!(ps.len(), 1);
        assert!(out.text.contains("only-body-text-no-paragraph"));
    }

    #[test]
    fn jsonld_missing_articlebody_key_falls_through() {
        // JSON-LD is well-formed object but no articleBody key.
        // Path 1 yields nothing.
        let html = r#"<html><body>
            <script type="application/ld+json">{"@type":"Article","headline":"foo"}</script>
            <p>only short</p>
          </body></html>"#;
        let out = baseline(html);
        // Path 4 wins (path 3's text "only short" is <= 100 chars).
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
        // Text contains the body text.
        assert!(out.text.contains("only short"));
    }

    #[test]
    fn jsonld_invalid_json_falls_through() {
        // Malformed JSON — JSONDecodeError swallowed.
        let html = r#"<html><body>
            <script type="application/ld+json">{not json articleBody</script>
            <article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim.</article>
          </body></html>"#;
        let out = baseline(html);
        assert!(out.length > 100);
        assert!(out.text.contains("Lorem ipsum"));
    }

    #[test]
    fn baseline_post_body_is_detached_body_element() {
        // baseline returns a synthetic <body> not the input's <body>.
        let html = "<html><body><p>hello</p></body></html>";
        let out = baseline(html);
        // postbody is a <body> element with no parent.
        assert_eq!(tag_name(&out.postbody).as_deref(), Some("BODY"));
        assert!(dom::parent(&out.postbody).is_none());
    }

    // ===========================================================================
    // Stage 10 — branch-coverage closers for each of the 5 baseline paths
    // ===========================================================================

    // ---- Path 1 multi-script accumulation (baseline.py:55-58) -------------

    #[test]
    fn jsonld_two_scripts_concatenate_temp_text() {
        // rationale: pin the False side of `if temp_text.is_empty()` at
        // baseline.rs:295 — Python's `temp_text += " " + text if temp_text
        // else text` (baseline.py:56). Two JSON-LD scripts, each with a
        // short articleBody (~60 chars), combined to > 100 chars so the
        // > 100 gate at baseline.rs:303 fires. The second iteration sees
        // `temp_text` already populated → enters the else arm
        // (`push(' '); push_str(&text)`).
        let body_a = "x".repeat(60);
        let body_b = "y".repeat(60);
        let html = format!(
            r#"<html><body>
                <script type="application/ld+json">{{"articleBody":"{}"}}</script>
                <script type="application/ld+json">{{"articleBody":"{}"}}</script>
            </body></html>"#,
            body_a, body_b
        );
        let out = baseline(&html);
        // Combined length > 100 ⇒ path 1 returns early.
        assert!(out.length > 100);
        // Both texts are present, separated by a single space (the else-arm
        // accumulator at baseline.py:56).
        assert!(out.text.contains(&body_a));
        assert!(out.text.contains(&body_b));
        assert!(
            out.text.contains(&format!("{} {}", body_a, body_b)),
            "expected single-space separator between the two articleBody texts"
        );
    }

    // ---- Path 1 short articleBody adds a <p> but does NOT short-circuit ---

    #[test]
    fn jsonld_short_articlebody_adds_p_then_falls_through_to_path2_return() {
        // rationale: pin the True side of `element_child_count(&postbody)
        // > 0` at baseline.rs:347 — Python `len(postbody) > 0`
        // (baseline.py:70). When JSON-LD produces a short (<100 char) `<p>`
        // that lands in postbody, and the `<article>` scan finds nothing,
        // path 2 STILL returns because `len(postbody)` is non-zero (the
        // JSON-LD <p> is there). The returned `text` is the article-path
        // temp_text (empty).
        let short = "x".repeat(30); // < 100 chars
        let html = format!(
            r#"<html><body>
                <script type="application/ld+json">{{"articleBody":"{}"}}</script>
            </body></html>"#,
            short
        );
        let out = baseline(&html);
        // Path 2 returns with the JSON-LD `<p>` already in postbody. The
        // returned text is the article-temp_text (empty string).
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1, "JSON-LD added one <p>; no article was found");
        assert_eq!(out.text, "", "path 2 temp_text is empty when no article matched");
        assert_eq!(out.length, 0);
    }

    // ---- Path 2 multi-article accumulation (baseline.py:69) ----------------

    #[test]
    fn article_path_two_articles_concatenate_temp_text() {
        // rationale: pin the False side of `if temp_text.is_empty()` at
        // baseline.rs:335 — same shape as the JSON-LD accumulator
        // (baseline.py:69). Two `<article>` elements, each > 100 chars,
        // produce a temp_text containing both, space-separated.
        let body_a = "alpha ".repeat(20); // ~120 chars
        let body_b = "bravo ".repeat(20); // ~120 chars
        let html = format!(
            "<html><body><article>{}</article><article>{}</article></body></html>",
            body_a, body_b
        );
        let out = baseline(&html);
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 2, "two articles → two <p>s in postbody");
        // The combined text contains BOTH article contents.
        assert!(out.text.contains("alpha"));
        assert!(out.text.contains("bravo"));
        // Path 2 returns with non-empty temp_text and length > 100.
        assert!(out.length > 100);
    }

    // ---- Path 2 short-article fall-through (baseline.py:67 False side) -----

    #[test]
    fn article_path_short_article_does_not_register() {
        // rationale: pin the False side of `if text.chars().count() > 100`
        // at baseline.rs:331 — short `<article>` text is NOT promoted to
        // postbody. The article scan completes with no `<p>` added; path 2's
        // post-loop guard `element_child_count > 0` is False → fall through
        // to path 3.
        let short_article = "abc"; // 3 chars
        // Path 3 should win with sufficient <p> text.
        let html = format!(
            "<html><body><article>{}</article>\
             <p>{}</p></body></html>",
            short_article,
            "tag-set ".repeat(20) // ~160 chars in <p>
        );
        let out = baseline(&html);
        // Path 3 wins: postbody has the <p>'s text.
        assert!(out.length > 100);
        assert!(out.text.contains("tag-set"));
        // The 3-char article was NOT added to postbody.
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        // path 3 added only the <p> (with the long text); the <article>'s
        // text didn't make it because it was below the 100-char gate.
        assert_eq!(ps.len(), 1);
    }

    // ---- Path 3 dedup HIT arm (baseline.py:80 False side) ------------------

    #[test]
    fn tag_set_dedup_hit_skips_duplicate_entry() {
        // rationale: pin the False side of `if !results.contains(&entry)`
        // at baseline.rs:377 — when a tag-set element's trimmed text already
        // appears in the dedup set, the element is SKIPPED. We construct
        // two `<p>`s with identical >100-char text; only ONE postbody <p>
        // should result. (Companion to `tag_set_path_dedupes`, which uses
        // shorter text and 5 distinct entries.)
        let body = "duplicate-text ".repeat(10); // ~150 chars
        let html = format!(
            "<html><body><p>{}</p><p>{}</p></body></html>",
            body, body
        );
        let out = baseline(&html);
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        // Dedup: only ONE <p> survives.
        assert_eq!(ps.len(), 1, "dedup HashSet collapses identical entries");
        assert!(out.length > 100);
    }

    // ---- Path 4 body present but emits only whitespace pieces --------------

    #[test]
    fn path4_body_only_whitespace_yields_empty_joined() {
        // rationale: pin the False side of `if !joined.is_empty()` at
        // baseline.rs:428 — when path 4's itertext produces only
        // whitespace pieces (all filtered to "" by `trim`), `joined` is the
        // empty string, no Text child is appended to the `<p>`, and
        // `text=""` / `length=0` is returned. The `<p>` is still present
        // (path 4 always appends a `<p>` per baseline.py:92-96).
        let html = "<html><body>   \t   \n   </body></html>";
        let out = baseline(html);
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1, "path 4 always appends exactly one <p>");
        // The <p> has NO text child (empty joined string).
        assert_eq!(out.text, "");
        assert_eq!(out.length, 0);
    }

    // ---- html2txt direct entry — empty input and clean=false branches ----

    #[test]
    fn html2txt_direct_empty_input_is_empty_string() {
        // rationale: pin the empty-input early return at baseline.rs:492.
        // Mirrors `baseline.py:116-117` (`if tree is None: return ""`).
        assert_eq!(html2txt("", true), "");
        assert_eq!(html2txt("", false), "");
    }

    #[test]
    fn html2txt_clean_false_preserves_script_text() {
        // rationale: pin the False side of `if clean` at baseline.rs:519 —
        // when `clean=false`, `basic_cleaning` is NOT called and `<aside>`/
        // `<script>` content WILL leak into the joined body text. This is
        // the path-5 invocation contract inside `baseline` (it calls
        // `html2txt_from_tree(&root, false)`).
        let html = r#"<html><body>visible<aside>aside-text</aside></body></html>"#;
        let out = html2txt(html, false);
        assert!(out.contains("visible"));
        assert!(
            out.contains("aside-text"),
            "with clean=false the aside content is preserved"
        );
    }

    // ---- baseline path-4 reset preserves freshly-created body element -----

    #[test]
    fn path4_postbody_is_fresh_after_path3_partial_population() {
        // rationale: pin baseline.py:89 — `postbody = Element('body')` is
        // RE-ASSIGNED inside path 4, dropping any path-3 `<p>` children.
        // We construct an input that adds path-3 <p>s with combined text <=
        // 100 chars (so path 3 doesn't return), then path 4 takes over and
        // emits exactly ONE <p>.
        let html = "<html><body>\
            <p>aa</p>\
            <p>bb</p>\
            <p>cc</p>\
            <p>dd</p>\
            (other body text)\
        </body></html>";
        let out = baseline(html);
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        // Path 4 reset means EXACTLY one <p> survives in the synthetic
        // postbody (not the 4 path-3 might have added).
        assert_eq!(ps.len(), 1, "path 4 reset drops path-3 <p>s");
        // The text contains content from the body itertext.
        assert!(out.text.contains("aa"));
        assert!(out.text.contains("bb"));
        assert!(out.text.contains("(other body text)"));
    }

    // ---- append_text_paragraph empty-text arm (baseline.rs:585) -----------

    #[test]
    fn baseline_empty_articlebody_string_still_adds_p() {
        // rationale: when JSON-LD `articleBody` is the empty string after
        // trim (e.g. just punctuation/whitespace), Python's
        // `SubElement(postbody, 'p').text = ""` still APPENDS a `<p>`
        // (with no leading-text-child run). Pins the False side of
        // `if !text.is_empty()` inside `append_text_paragraph`
        // (baseline.rs:585).
        //
        // We engineer this via path 4: a body with whitespace-only content
        // routes through `append_text_paragraph(&postbody, "")` at
        // baseline.rs:456 (the path-5 fall-through) — wait, that's path 5.
        // Actually baseline.py:92 (`p_elem = SubElement(postbody, 'p')`)
        // always creates the <p>; the text is set conditionally.
        //
        // Direct exercise: call append_text_paragraph indirectly via path 5
        // (html2txt fallback) where the html2txt returns "".
        // path 5 only fires when <body> is absent — html5ever always
        // synthesises <body>, so path 5 is effectively unreachable in
        // baseline. Instead, exercise path 4 with whitespace-only body,
        // which lands in baseline.rs:428's False branch: no Text child is
        // appended to the path-4 <p> (the `append_text_paragraph` arm at
        // baseline.rs:585 False side fires via the path-1/2/3 callers when
        // a trimmed JSON-LD body collapses to "").
        //
        // Direct path 1: empty articleBody → loop iteration sees
        // `json_body=""` and `continue`s (baseline.rs:272 True), so
        // append_text_paragraph isn't called for "". This branch is hit
        // only via the JSON-LD html-parse path when the inner trim
        // collapses to "" — engineer that.
        let html = r#"<html><body>
            <script type="application/ld+json">{"articleBody":"<p>   </p>"}</script>
        </body></html>"#;
        let out = baseline(html);
        // The inner load_html sees "<p>   </p>" → text_content "   " →
        // trim() → "". `json_body` is non-empty (contains "<p>"), so the
        // `if json_body.is_empty()` continue is NOT taken. The
        // `append_text_paragraph(&postbody, "")` runs with text="" — the
        // <p> is appended without a Text child.
        // Path 1 temp_text accumulates "" → length 0 → does NOT return.
        // Then path 2: no <article>; element_child_count is 1 (from the
        // JSON-LD <p>) → path 2 returns with temp_text="".
        // Postbody has exactly one <p>, and that <p> has no Text child.
        let ps = get_elements_by_tag_name(&out.postbody, "p");
        assert_eq!(ps.len(), 1);
        // The <p> exists but has no children (the empty-text branch).
        assert_eq!(ps[0].children.borrow().len(), 0);
        assert_eq!(out.text, "");
    }

    // ---- baseline articleBody key but value is non-string (None/Number/Array)

    #[test]
    fn jsonld_articlebody_non_string_value_is_ignored() {
        // rationale: pin the swallow-by-`as_str` arm. Python catches the
        // `.get("articleBody", "")` returning a non-str (int/list/None) via
        // the same try/except — Trafilatura's `json_body` ends up "" and
        // the `if json_body:` check (baseline.rs:272 True side) skips. The
        // Rust port routes through `.and_then(|b| b.as_str())` which is
        // None for non-string values; `.unwrap_or_default()` makes it "".
        let html = r#"<html><body>
            <script type="application/ld+json">{"articleBody":123}</script>
            <article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim.</article>
        </body></html>"#;
        let out = baseline(html);
        // Path 1 yields nothing (articleBody is a number → as_str returns
        // None → json_body = "" → continue). Path 2 wins via the long
        // <article>.
        assert!(out.length > 100);
        assert!(out.text.contains("Lorem ipsum"));
    }

    // ---- baseline path-1 articleBody-key-absent fast skip ------------------

    #[test]
    fn jsonld_script_without_articlebody_substring_is_skipped() {
        // rationale: pin the True side of `if raw.is_empty() ||
        // !raw.contains("articleBody")` (baseline.rs:252) — when a script
        // body lacks the substring "articleBody" entirely, the loop
        // continues without parsing. Faithful to baseline.py:44.
        let html = r#"<html><body>
            <script type="application/ld+json">{"@type":"Organization","name":"Foo"}</script>
            <article>Lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim.</article>
        </body></html>"#;
        let out = baseline(html);
        // Path 2 wins (Organization JSON has no articleBody → JSON-LD
        // contributes nothing → article path takes over).
        assert!(out.length > 100);
        assert!(out.text.contains("Lorem ipsum"));
    }
}
