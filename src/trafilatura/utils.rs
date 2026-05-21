//! `utils` — Stage 2b': small text/image/regex helpers from
//! `trafilatura@v2.0.0/utils.py`.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.2 (the
//! `htmlprocessing.py` ports under Stage 2b' depend on these utilities).
//! Source of truth: `trafilatura@v2.0.0/utils.py`.
//!
//! # Scope
//!
//! This module collects the **small** utility helpers Stage 2b' (and the
//! upcoming Stage 2c-i handler primitives) reach for:
//!
//! - `FORMATTING_PROTECTED` / `SPACING_PROTECTED` — tag-name sets that gate
//!   text trimming in `handle_textnode` / `process_node` / the block
//!   handlers (utils.py:79-80).
//! - `IMAGE_EXTENSION` regex + `is_image_file` / `is_image_element` — used
//!   by `handle_textnode` (htmlprocessing.py:229) and by the image handlers
//!   in Stage 2c-iii.
//! - `RE_FILTER` + `textfilter` — the "social-media line filter" used by
//!   `handle_textnode` / `process_node` to drop boilerplate share buttons.
//! - `text_chars_test` — used widely to short-circuit "is this string any
//!   actual content?" (utils.py:452-456).
//! - `trim` — the canonical `" ".join(s.split())` wrapper (utils.py:340-346).
//!   A private copy already lives in `baseline.rs`; that copy is frozen as
//!   part of Stage 1c, so Stage 2b' adds a parallel pub(crate) version here
//!   for the Stage 2b'/2c module surface to share. A future stage may
//!   consolidate them.
//!
//! # Anti-inversion
//!
//! Every fn / const carries a `utils.py:NN-MM` source-line cite. The regex
//! literals are byte-identical to the Python source (the IMAGE_EXTENSION /
//! RE_FILTER patterns are case-insensitive ASCII; Rust's `regex` accepts the
//! same alternation/group syntax verbatim).

use std::sync::OnceLock;

use regex::Regex;

use crate::readability::dom::{
    NodeData, NodeRef, attributes_in_source_order, element_text, get_attribute, tail,
};

// ===========================================================================
// FORMATTING_PROTECTED / SPACING_PROTECTED (utils.py:79-80)
// ===========================================================================

/// `FORMATTING_PROTECTED` (utils.py:79). Tag-name set: text within these
/// elements is not aggressively whitespace-collapsed during handler
/// processing. Membership-test only — order does not matter.
///
/// Python source:
/// ```python
/// FORMATTING_PROTECTED = {'cell', 'head', 'hi', 'item', 'p', 'quote', 'ref', 'td'}
/// ```
pub const FORMATTING_PROTECTED: &[&str] =
    &["cell", "head", "hi", "item", "p", "quote", "ref", "td"];

/// `SPACING_PROTECTED` (utils.py:80). Tag-name set: text within these
/// elements is **never** whitespace-collapsed (code blocks preserve
/// indentation; `<pre>` likewise). Exported for parity with Python; not
/// referenced in Stage 2b' itself (it lights up in Stage 2c block handlers).
///
/// Python source:
/// ```python
/// SPACING_PROTECTED = {'code', 'pre'}
/// ```
pub const SPACING_PROTECTED: &[&str] = &["code", "pre"];

/// `tag in FORMATTING_PROTECTED` set-membership helper.
#[inline]
pub fn formatting_protected(tag: &str) -> bool {
    FORMATTING_PROTECTED.contains(&tag)
}

/// `tag in SPACING_PROTECTED` set-membership helper.
#[inline]
pub fn spacing_protected(tag: &str) -> bool {
    SPACING_PROTECTED.contains(&tag)
}

// ===========================================================================
// trim (utils.py:340-346)
// ===========================================================================

/// `trim(string)` — `" ".join(string.split()).strip()` (utils.py:340-346).
///
/// Python `str.split()` (no arg) splits on **any** Unicode whitespace and
/// drops empty parts. Rust `str::split_whitespace` implements the same
/// Unicode `White_Space` property, so this is faithful. The final `.strip()`
/// is belt-and-braces — already a no-op after `" ".join` over non-empty
/// trimmed pieces, but Python writes it.
///
/// A private copy of this function exists in `baseline.rs` (Stage 1c,
/// frozen). Stage 2b' adds a parallel `pub(crate)` version here so the
/// Stage 2b'/2c module surface shares one trim implementation.
pub fn trim(s: &str) -> String {
    let joined: Vec<&str> = s.split_whitespace().collect();
    joined.join(" ").trim().to_string()
}

// ===========================================================================
// IMAGE_EXTENSION regex + is_image_file (utils.py:77, 363-368)
// ===========================================================================

/// `IMAGE_EXTENSION` regex (utils.py:77).
///
/// Python source:
/// ```python
/// IMAGE_EXTENSION = re.compile(r'[^\s]+\.(avif|bmp|gif|hei[cf]|jpe?g|png|webp)(\b|$)')
/// ```
///
/// Matches a non-whitespace prefix ending in `.ext` where `ext` is one of
/// the listed image extensions, followed by a word boundary or end of input.
fn image_extension_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[^\s]+\.(avif|bmp|gif|hei[cf]|jpe?g|png|webp)(\b|$)").expect(
            "trafilatura utils.py:77 IMAGE_EXTENSION regex compiles \
             (literal known-good pattern)",
        )
    })
}

/// `is_image_file(imagesrc)` — utils.py:363-368.
///
/// Python source:
/// ```python
/// def is_image_file(imagesrc: Optional[str]) -> bool:
///     '''Check if the observed string corresponds to a valid image extension.
///        Use a length threshold and apply a regex on the content.'''
///     if imagesrc is None or len(imagesrc) > 8192:
///         return False
///     return bool(IMAGE_EXTENSION.search(imagesrc))
/// ```
pub fn is_image_file(imagesrc: Option<&str>) -> bool {
    match imagesrc {
        None => false,
        // Python `len(imagesrc)` on a str = code-point count. Use chars().
        Some(s) if s.chars().count() > 8192 => false,
        Some(s) => image_extension_re().is_match(s),
    }
}

/// `is_image_element(element)` — utils.py:349-360.
///
/// Python source:
/// ```python
/// def is_image_element(element: _Element) -> bool:
///     '''Check if an element is a valid img element'''
///     for attr in ("data-src", "src"):
///         src = element.get(attr, "")
///         if is_image_file(src):
///             return True
///     else:
///         # take the first corresponding attribute
///         for attr, value in element.attrib.items():
///             if attr.startswith("data-src") and is_image_file(value):
///                 return True
///     return False
/// ```
///
/// Note the Python `for..else`: the `else` block runs only if the `for`
/// completed without `break`. There is no `break` in the body, so the
/// `else` always runs after the first loop falls through. The faithful
/// Rust translation is: check `data-src` and `src` first; if neither
/// matches, fall through to scanning *every* attribute that starts with
/// `data-src` (e.g. `data-src-large`, `data-src-set`).
pub fn is_image_element(element: &NodeRef) -> bool {
    // First loop: ("data-src", "src") — fixed attribute names.
    for attr in ["data-src", "src"] {
        let src = get_attribute(element, attr);
        if is_image_file(src.as_deref()) {
            return true;
        }
    }
    // for..else: the loop completed without break, so the else runs.
    // Scan every attribute whose name starts with "data-src".
    for (name, value) in attributes_in_source_order(element) {
        if name.starts_with("data-src") && is_image_file(Some(&value)) {
            return true;
        }
    }
    false
}

// ===========================================================================
// RE_FILTER + textfilter (utils.py:87-90, 445-449)
// ===========================================================================

/// `RE_FILTER` regex (utils.py:87-90).
///
/// Python source (case-insensitive):
/// ```python
/// RE_FILTER = re.compile(r'\W*(Drucken|E-?Mail|Facebook|Flipboard|Google|Instagram|'
///                         'Linkedin|Mail|PDF|Pinterest|Pocket|Print|QQ|Reddit|Twitter|'
///                         'WeChat|WeiBo|Whatsapp|Xing|Mehr zum Thema:?|More on this.{,8}$)$',
///                        flags=re.IGNORECASE)
/// ```
///
/// Note the Python literal `.{,8}` — that is `.{0,8}` (Python accepts the
/// elided lower bound). Rust's `regex` crate requires the explicit `{0,8}`.
/// This is the only literal-syntax adaptation; the alternation set and the
/// `\W*` prefix anchor are byte-identical.
fn re_filter() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Python's adjacent-string-literal concatenation joins the three
        // source string fragments verbatim — no whitespace is inserted
        // between them. The resulting single pattern is reproduced here as
        // one Rust raw-string literal. `(?i)` corresponds to Python's
        // `re.IGNORECASE` flag. The only literal-syntax adaptation from
        // Python is `.{,8}` → `.{0,8}` (Rust requires an explicit lower
        // bound; Python permits it elided).
        Regex::new(
            r"(?i)\W*(Drucken|E-?Mail|Facebook|Flipboard|Google|Instagram|Linkedin|Mail|PDF|Pinterest|Pocket|Print|QQ|Reddit|Twitter|WeChat|WeiBo|Whatsapp|Xing|Mehr zum Thema:?|More on this.{0,8}$)$",
        )
        .expect("utils.py:87-90 RE_FILTER compiles")
    })
}

/// `RE_FILTER.match(line)` — Python `re.match` anchors at the start (Python
/// `re.match`), not the whole string. This helper exposes that "anchored at
/// start" semantic. Returns true iff the pattern matches starting at index 0
/// of `s`.
fn re_filter_match_start(s: &str) -> bool {
    let re = re_filter();
    re.find(s).is_some_and(|m| m.start() == 0)
}

/// Mirrors Python `str.splitlines()` — splits on the full set of Unicode
/// line-boundary characters that Python recognises:
/// `\n`, `\r`, `\r\n`, `\v` (U+000B), `\f` (U+000C),
/// `\x1c`–`\x1e`, `\x85`, `\u{2028}`, `\u{2029}`.
/// Cite: CPython `Objects/unicodeobject.c` `splitlines_keep_newline` /
/// `_PyUnicode_IsLineBreak`. Rust's `str::lines` only handles `\n` /
/// `\r\n`, which is too narrow for the share-button line filter
/// (`textfilter`) where unusual separators like U+000B (vertical tab)
/// or U+2028 (line separator) can appear in the wild.
fn splitlines_python(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = s.char_indices().collect();
    let mut start = 0;
    let mut i = 0;
    while i < chars.len() {
        let (idx, ch) = chars[i];
        let is_break = matches!(
            ch,
            '\n' | '\r'
                | '\u{000B}'
                | '\u{000C}'
                | '\u{001C}'
                | '\u{001D}'
                | '\u{001E}'
                | '\u{0085}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if is_break {
            out.push(&s[start..idx]);
            // CRLF: collapse the trailing \n into the same line-break.
            if ch == '\r' && i + 1 < chars.len() && chars[i + 1].1 == '\n' {
                i += 1;
            }
            i += 1;
            start = if i < chars.len() { chars[i].0 } else { s.len() };
        } else {
            i += 1;
        }
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// `textfilter(element)` — utils.py:445-449.
///
/// Python source:
/// ```python
/// def textfilter(element: _Element) -> bool:
///     '''Filter out unwanted text'''
///     testtext = element.tail if element.text is None else element.text
///     # to check: line len → continue if len(line) <= 5
///     return not testtext or testtext.isspace() or any(map(RE_FILTER.match, testtext.splitlines()))
/// ```
///
/// Returns true if the element's text-or-tail looks like boilerplate
/// (Facebook / Reddit / "More on this …" share-line). `element.text is None`
/// in lxml maps to `element_text(elem) == None` in dom.rs — they both mean
/// "no leading-text-child run".
pub fn textfilter(element: &NodeRef) -> bool {
    let text = element_text(element);
    let testtext = if text.is_none() { tail(element) } else { text };
    let Some(s) = testtext else {
        // `not testtext` — None ≈ falsy.
        return true;
    };
    if s.is_empty() {
        // `not testtext` — empty string ≈ falsy.
        return true;
    }
    // `testtext.isspace()` — Python returns False for the empty string, so
    // we keep the empty-string check above separate. For non-empty, this is
    // "every char is Unicode whitespace".
    if s.chars().all(|c| c.is_whitespace()) {
        return true;
    }
    // any(map(RE_FILTER.match, testtext.splitlines()))
    // Python `str.splitlines()` splits on `\n`, `\r`, `\r\n`, `\v`, `\f`,
    // `\x1c`–`\x1e`, `\x85`, `\u{2028}`, `\u{2029}`. Rust's `str::lines`
    // is narrower (`\n` / `\r\n` only), which would silently fail to
    // match a share-button line separated by, e.g., U+000B. Route
    // through the explicit `splitlines_python` helper instead.
    for line in splitlines_python(&s) {
        if re_filter_match_start(line) {
            return true;
        }
    }
    false
}

// ===========================================================================
// text_chars_test (utils.py:452-456)
// ===========================================================================

/// `text_chars_test(string)` — utils.py:452-456.
///
/// Python source:
/// ```python
/// def text_chars_test(string: Optional[str]) -> bool:
///     '''Determine if a string is only composed of spaces and/or control characters'''
///     return bool(string) and not string.isspace()
/// ```
///
/// Returns true iff `string` is `Some` AND non-empty AND not all-whitespace.
/// Python's `str.isspace()` returns False for the empty string, so we keep
/// the empty-string short-circuit explicit. Whitespace is Unicode (NBSP /
/// U+00A0 etc. count as whitespace under Python's `isspace`; `char::
/// is_whitespace` matches).
pub fn text_chars_test(string: Option<&str>) -> bool {
    let Some(s) = string else { return false };
    if s.is_empty() {
        return false;
    }
    !s.chars().all(|c| c.is_whitespace())
}

// ===========================================================================
// duplicate_test (deduplication.py:243-254) — Stage 8 (live)
// ===========================================================================

/// `duplicate_test(element, options)` — deduplication.py:243-254.
///
/// **Stage 8** activates this surface: the call sites in
/// `cleaning::handle_textnode` (htmlprocessing.py:262) and
/// `cleaning::process_node` (htmlprocessing.py:282) now route into the
/// real LRU-cache port at [`crate::trafilatura::deduplication`]. The
/// Stage 2b' stub returned `false` unconditionally because
/// `Options.dedup` defaulted to `false` and had no field backing; both
/// of those gaps closed in Stage 8 (the field landed on `Options` and
/// the LRU port landed in `deduplication.rs`).
///
/// This thin wrapper preserves the per-element call shape every Stage
/// 2b' caller already writes — `duplicate_test(elem, options)` — by
/// forwarding to [`crate::trafilatura::deduplication::duplicate_test_node`].
pub fn duplicate_test(element: &NodeRef, options: &crate::trafilatura::cleaning::Options) -> bool {
    // Stage 8 wiring — forward into the real LRU-backed implementation.
    // The Python source signature `duplicate_test(element, options) -> bool`
    // is preserved at every call site in cleaning.rs.
    crate::trafilatura::deduplication::duplicate_test_node(element, options)
}

// ===========================================================================
// Internal: element_child_count (lxml `len(elem)` shape)
// ===========================================================================

/// Count of **element** children of `node`. Matches lxml's `len(elem)`
/// (which counts *element* children only; lxml's Element is element-only
/// for indexing).
///
/// `handle_textnode` (htmlprocessing.py:231, 243) and `process_node`
/// (:270) test `len(elem) == 0`. We provide it here so Stage 2b' callers
/// don't reach into rcdom internals. A private copy already lives in
/// `baseline.rs` (Stage 1c, frozen) and `cleaning.rs` is expected to share
/// this one.
pub fn element_child_count(node: &NodeRef) -> usize {
    node.children
        .borrow()
        .iter()
        .filter(|c| matches!(c.data, NodeData::Element { .. }))
        .count()
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::{
        Dom, create_element, create_text_node, get_elements_by_tag_name, set_attribute,
    };

    fn parse(html: &str) -> Dom {
        Dom::parse(html)
    }

    fn body(dom: &Dom) -> NodeRef {
        dom.body().expect("html5ever synthesises <body>")
    }

    // ---- FORMATTING_PROTECTED / SPACING_PROTECTED ----

    #[test]
    fn formatting_protected_membership() {
        // utils.py:79 — {'cell', 'head', 'hi', 'item', 'p', 'quote', 'ref', 'td'}
        for tag in ["cell", "head", "hi", "item", "p", "quote", "ref", "td"] {
            assert!(formatting_protected(tag), "{tag} should be protected");
        }
        for tag in ["div", "span", "a", ""] {
            assert!(!formatting_protected(tag), "{tag} should NOT be protected");
        }
    }

    #[test]
    fn spacing_protected_membership() {
        // utils.py:80 — {'code', 'pre'}
        assert!(spacing_protected("code"));
        assert!(spacing_protected("pre"));
        assert!(!spacing_protected("p"));
        assert!(!spacing_protected("div"));
    }

    // ---- text_chars_test ----

    #[test]
    fn text_chars_test_smoke() {
        // Empty / whitespace / NBSP — all false.
        assert!(!text_chars_test(None));
        assert!(!text_chars_test(Some("")));
        assert!(!text_chars_test(Some("   ")));
        assert!(!text_chars_test(Some("\t \n")));
        // U+00A0 NBSP — Python `str.isspace()` returns True; Rust `char::
        // is_whitespace` matches (NBSP has the Unicode White_Space property).
        assert!(!text_chars_test(Some("\u{00A0}")));
        // Real content — true.
        assert!(text_chars_test(Some("a")));
        assert!(text_chars_test(Some("hello world")));
        // Trailing/leading whitespace with content — true (not ALL space).
        assert!(text_chars_test(Some(" x ")));
    }

    // ---- textfilter ----

    fn p_with_text(text: &str) -> NodeRef {
        // Build a <p> with a single leading Text child = `text`.
        let p = create_element("p");
        if !text.is_empty() {
            let t = create_text_node(text);
            // append_child (faithful link).
            crate::readability::dom::append_child(&p, &t);
        }
        p
    }

    #[test]
    fn textfilter_blocks_facebook_line() {
        let p = p_with_text("Facebook");
        assert!(textfilter(&p));
    }

    #[test]
    fn textfilter_blocks_pinterest_with_punctuation() {
        // \W*Pinterest$ — leading non-word chars allowed (the \W* prefix in
        // the Python regex). " Pinterest" matches at start with \W*=" ".
        let p = p_with_text(" Pinterest");
        assert!(textfilter(&p));
    }

    #[test]
    fn textfilter_passes_normal_text() {
        let p = p_with_text("Hello world. This is real content.");
        assert!(!textfilter(&p));
    }

    #[test]
    fn textfilter_blocks_empty_and_whitespace() {
        let empty = create_element("p"); // no text child
        assert!(textfilter(&empty));
        let ws = p_with_text("   \t\n");
        assert!(textfilter(&ws));
    }

    #[test]
    fn textfilter_uses_tail_when_text_is_none() {
        // Build <div><span></span>Facebook</div>. The <span>.text is None,
        // its .tail is "Facebook". textfilter on <span> should return true
        // via the tail branch.
        let dom = parse("<div><span></span>Facebook</div>");
        let b = body(&dom);
        let spans = get_elements_by_tag_name(&b, "span");
        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        // Sanity: element_text(span) returns None (no leading Text child).
        assert!(element_text(span).is_none());
        // tail(span) returns "Facebook".
        assert_eq!(tail(span).as_deref(), Some("Facebook"));
        assert!(textfilter(span));
    }

    // ---- splitlines_python (Python str.splitlines parity) ----

    #[test]
    fn splitlines_python_matches_lf_crlf_cr() {
        // Basic cases — LF, CR, CRLF all split a line.
        assert_eq!(splitlines_python("a\nb"), vec!["a", "b"]);
        assert_eq!(splitlines_python("a\rb"), vec!["a", "b"]);
        assert_eq!(splitlines_python("a\r\nb"), vec!["a", "b"]);
        // Trailing terminator yields no trailing empty element (matches
        // Python: `"a\n".splitlines() == ["a"]`).
        assert_eq!(splitlines_python("a\n"), vec!["a"]);
    }

    #[test]
    fn splitlines_python_splits_on_vertical_tab() {
        // U+000B (vertical tab) is a Python line-break but NOT a Rust
        // `str::lines` break — pins the Stage 2b' review fix.
        assert_eq!(
            splitlines_python("foo\u{000B}Facebook"),
            vec!["foo", "Facebook"]
        );
    }

    #[test]
    fn splitlines_python_splits_on_u2028() {
        // U+2028 (LINE SEPARATOR) is a Python line-break but NOT a Rust
        // `str::lines` break — pins the Stage 2b' review fix.
        assert_eq!(
            splitlines_python("foo\u{2028}Facebook"),
            vec!["foo", "Facebook"]
        );
    }

    // ---- is_image_file ----

    #[test]
    fn is_image_file_case_sensitive_per_python_default() {
        // utils.py:77 has no re.IGNORECASE, so "FOO.PNG" should NOT match
        // (the alternation lists lowercase extensions only).
        assert!(!is_image_file(Some("FOO.PNG")));
        assert!(is_image_file(Some("FOO.png")));
    }

    #[test]
    fn is_image_file_accepts_png_jpg_jpeg_webp_avif_heic_heif() {
        for ext in [
            "png", "jpg", "jpeg", "webp", "avif", "heic", "heif", "gif", "bmp",
        ] {
            let s = format!("/path/to/x.{ext}");
            assert!(is_image_file(Some(&s)), "{ext} should match");
        }
    }

    #[test]
    fn is_image_file_rejects_none_and_oversized() {
        assert!(!is_image_file(None));
        let huge = "x".repeat(8193) + ".png";
        assert!(!is_image_file(Some(&huge)));
    }

    #[test]
    fn is_image_file_rejects_javascript_url() {
        // No matching extension at end-or-word-boundary.
        assert!(!is_image_file(Some("javascript:void(0)")));
        assert!(!is_image_file(Some("/foo/bar")));
    }

    #[test]
    fn is_image_file_rejects_query_appended_garbage() {
        // ".pngmore" — no word boundary after png, so the (\b|$) anchor
        // doesn't match. Python regex agrees.
        assert!(!is_image_file(Some("x.pngmore")));
    }

    // ---- is_image_element ----

    #[test]
    fn is_image_element_accepts_data_src_first() {
        // utils.py:351 — for attr in ("data-src", "src") — data-src is
        // checked FIRST. If data-src holds an image filename and src is
        // empty/missing, return True via data-src.
        let img = create_element("img");
        set_attribute(&img, "data-src", "y.jpg");
        assert!(is_image_element(&img));
    }

    #[test]
    fn is_image_element_uses_src_when_data_src_absent() {
        let img = create_element("img");
        set_attribute(&img, "src", "x.png");
        assert!(is_image_element(&img));
    }

    #[test]
    fn is_image_element_falls_through_to_data_src_variants() {
        // The Python for..else: after ("data-src", "src") loop completes
        // without break (which it always does, since there's no break in
        // the body), scan every attribute starting with "data-src".
        let img = create_element("img");
        set_attribute(&img, "data-src-large", "huge.webp");
        assert!(is_image_element(&img));
    }

    #[test]
    fn is_image_element_rejects_non_image() {
        let img = create_element("img");
        set_attribute(&img, "src", "javascript:0");
        assert!(!is_image_element(&img));
    }

    // ---- duplicate_test (Stage 8 — live via deduplication::duplicate_test_node)

    #[test]
    fn duplicate_test_empty_node_returns_false() {
        // Pin: a `<p>` with no text gives an empty `itertext` join, which
        // is below any sensible `min_duplcheck_size` (default 100) — so
        // `duplicate_test` records the empty string but never returns
        // true. Stage 2b' returned false unconditionally via a stub;
        // Stage 8 now returns false because the text gate (Python
        // `len(teststring) > min_duplcheck_size`) does not trip on empty
        // input. Same observable result on the default-Options path.
        use crate::trafilatura::cleaning::Options;
        use crate::trafilatura::deduplication::clear_lru_test;
        clear_lru_test();
        let p = create_element("p");
        let opts = Options::default();
        assert!(!duplicate_test(&p, &opts));
    }

    // ---- trim ----

    #[test]
    fn trim_smoke() {
        assert_eq!(trim("  hello  world\t\n"), "hello world");
        assert_eq!(trim(""), "");
        assert_eq!(trim("   "), "");
        assert_eq!(trim("a"), "a");
    }

    // ---- element_child_count ----

    #[test]
    fn element_child_count_counts_only_elements() {
        let dom = parse("<div>text<p>a</p><!--c--><span>b</span>tail</div>");
        let b = body(&dom);
        let divs = get_elements_by_tag_name(&b, "div");
        assert_eq!(divs.len(), 1);
        // <div> has children: Text, <p>, Comment, <span>, Text — 2 elements.
        assert_eq!(element_child_count(&divs[0]), 2);
    }

    // ---- element_text / set_element_text / set_tail (dom.rs facade smoke) --

    #[test]
    fn element_text_reads_leading_text_run() {
        let dom = parse("<p>hello<span>x</span></p>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        assert_eq!(ps.len(), 1);
        assert_eq!(element_text(&ps[0]).as_deref(), Some("hello"));
    }

    #[test]
    fn element_text_returns_none_when_first_child_is_element() {
        let dom = parse("<p><span>x</span></p>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        assert!(element_text(&ps[0]).is_none());
    }

    #[test]
    fn set_element_text_replaces_leading_run() {
        use crate::readability::dom::set_element_text;
        let dom = parse("<p>old<span>x</span></p>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        set_element_text(&ps[0], Some("new"));
        assert_eq!(element_text(&ps[0]).as_deref(), Some("new"));
        // <span> child still present.
        let spans = get_elements_by_tag_name(&ps[0], "span");
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn set_element_text_none_clears_leading_run() {
        use crate::readability::dom::set_element_text;
        let dom = parse("<p>old<span>x</span></p>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        set_element_text(&ps[0], None);
        assert!(element_text(&ps[0]).is_none());
    }

    #[test]
    fn set_tail_replaces_tail_run() {
        use crate::readability::dom::set_tail;
        let dom = parse("<div><p>x</p>old<span>z</span></div>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        set_tail(&ps[0], Some("new"));
        assert_eq!(tail(&ps[0]).as_deref(), Some("new"));
    }

    #[test]
    fn set_tail_none_clears_tail_run() {
        use crate::readability::dom::set_tail;
        let dom = parse("<div><p>x</p>tail-text<span>z</span></div>");
        let b = body(&dom);
        let ps = get_elements_by_tag_name(&b, "p");
        set_tail(&ps[0], None);
        assert!(tail(&ps[0]).is_none());
    }
}
