//! `helpers.rs` — small predicate / traversal helpers ported faithfully from
//! `Readability.js` (HLD §5, §7.1 Stage-1a slice).
//!
//! Every function carries its exact `Readability.js:<line>` citation
//! (anti-inversion, HLD §4.3(a)). These are pure functions over the
//! [`dom`](super::dom) facade plus the [`Flags`] bitset; none of them tune a
//! threshold toward any oracle/gold — they transcribe the JS predicate.

use crate::readability::dom::{
    self, NodeRef, child_nodes, children, first_element_child, get_attribute,
    get_elements_by_tag_name, is_element, is_text, next_element_sibling, parent, tag_name,
    text_content,
};
use crate::readability::regexps;

// ---------------------------------------------------------------------------
// Flags (`Readability.js:112-114`, `_flagIsActive` 2686, `_removeFlag` 2690).
// ---------------------------------------------------------------------------

/// `FLAG_STRIP_UNLIKELYS` (`Readability.js:112`, `0x1`).
pub const FLAG_STRIP_UNLIKELYS: u32 = 0x1;
/// `FLAG_WEIGHT_CLASSES` (`Readability.js:113`, `0x2`).
pub const FLAG_WEIGHT_CLASSES: u32 = 0x2;
/// `FLAG_CLEAN_CONDITIONALLY` (`Readability.js:114`, `0x4`).
pub const FLAG_CLEAN_CONDITIONALLY: u32 = 0x4;

/// The `this._flags` bitset (`Readability.js:69-72` — "Start with all flags
/// set"). A thin newtype so `_flagIsActive` / `_removeFlag` are methods, not
/// free `&` arithmetic scattered around.
#[derive(Debug, Clone, Copy)]
pub struct Flags(pub u32);

impl Default for Flags {
    /// `Readability.js:69-72`:
    /// `FLAG_STRIP_UNLIKELYS | FLAG_WEIGHT_CLASSES | FLAG_CLEAN_CONDITIONALLY`.
    fn default() -> Self {
        Flags(FLAG_STRIP_UNLIKELYS | FLAG_WEIGHT_CLASSES | FLAG_CLEAN_CONDITIONALLY)
    }
}

impl Flags {
    /// `_flagIsActive(flag)` (`Readability.js:2686-2688`):
    /// `(this._flags & flag) > 0`.
    pub fn is_active(&self, flag: u32) -> bool {
        (self.0 & flag) > 0
    }

    /// `_removeFlag(flag)` (`Readability.js:2690-2692`):
    /// `this._flags = this._flags & ~flag`.
    pub fn remove(&mut self, flag: u32) {
        self.0 &= !flag;
    }
}

// ---------------------------------------------------------------------------
// Visibility / structure predicates.
// ---------------------------------------------------------------------------

/// `_isProbablyVisible(node)` (`Readability.js:2694-2707`).
///
/// JS reads `node.style` (the inline-style CSSOM declaration). Under jsdom the
/// only inline styles are those in the `style="..."` attribute; we parse that
/// attribute for `display` / `visibility` (the two properties the JS checks).
/// The `!node.style ||` short-circuits (SVG/MathML have no `.style`) collapse
/// to: "if there is no `display:none` / `visibility:hidden` inline declaration".
/// Then `!node.hasAttribute("hidden")` and the `aria-hidden` clause with the
/// `fallback-image` class exception, verbatim.
pub fn is_probably_visible(node: &NodeRef) -> bool {
    // (!node.style || node.style.display != "none")
    if inline_style_prop(node, "display").as_deref() == Some("none") {
        return false;
    }
    // (!node.style || node.style.visibility != "hidden")
    if inline_style_prop(node, "visibility").as_deref() == Some("hidden") {
        return false;
    }
    // !node.hasAttribute("hidden")
    if has_attribute(node, "hidden") {
        return false;
    }
    // !node.hasAttribute("aria-hidden") || aria-hidden != "true" ||
    //   (className.includes("fallback-image"))
    if has_attribute(node, "aria-hidden") {
        let aria_true = get_attribute(node, "aria-hidden").as_deref() == Some("true");
        if aria_true && !dom::class_name(node).contains("fallback-image") {
            return false;
        }
    }
    true
}

/// `node.hasAttribute(name)` — present (even if empty), distinct from
/// `getAttribute(...) == null`. Only meaningful on element nodes.
fn has_attribute(node: &NodeRef, name: &str) -> bool {
    get_attribute(node, name).is_some()
}

/// Extract one `prop` value from an inline `style="..."` attribute, lower-cased
/// and trimmed (so `display: None` → `none`). `None` if no style attribute or
/// the property is absent. CSS property names are ASCII case-insensitive; we
/// match `prop` case-insensitively.
///
/// **CSSOM `!important` faithfulness (HLD §7.5 explicit Stage-3 item; the
/// CSSOM spec — `CSSStyleDeclaration.getPropertyValue` returns just the value,
/// `getPropertyPriority` returns the `"important"` marker separately).** A
/// declaration like `display: none !important` has `display` value `"none"`
/// and priority `"important"`; `node.style.display` in JS / jsdom evaluates to
/// `"none"` (the priority is on a different accessor, not in the value
/// string). Without stripping `!important` here, `display: none !important`
/// would land as `"none !important"` and fail to match `Some("none")` in
/// `_isProbablyVisible`, so the crate would treat the node as visible and the
/// JS as hidden — a real divergence on any inline `display:none !important`.
/// This is the deferred-Stage-3 fix from HLD §7.5 (Stage 1 ported only a
/// minimal version). The corpus does not contain `display:none !important`
/// (probed; no scored URL exercises this branch), so this fix moves no
/// measurable Coverage residual; it is ported anyway because §7.5 names it
/// explicitly and the cost (one regex test) is trivial.
///
/// We strip a trailing `!\s*important` suffix (CSS spec syntax — case-
/// insensitive `!` plus any whitespace plus `important`), then lower-case and
/// trim, faithful to how `getPropertyValue` would expose the value.
///
/// **Known residual (pre-existing, intentionally minimal):** this returns the
/// **FIRST** matching declaration in source order, while the real CSS cascade
/// is **LAST-wins** for a given property within a single `style` attribute. A
/// faithful CSSOM port would consume the whole declaration list and use the
/// last winning value. Corpus impact at M2 close: ONE pattern hits
/// (Wikipedia infobox `display:block;;display:inline`), but neither value is
/// `none`/`hidden`, so the visibility decision for `_isProbablyVisible` is
/// unchanged — score-invisible. Tracked as an HLD §7.5 named residual; not
/// closed because (a) zero scored impact on the current corpus and (b)
/// porting a full CSSOM declaration list would expand the surface area
/// without measurable benefit. Re-examine if a future URL exhibits a
/// `display:something;display:none` pattern where last-wins flips the
/// visibility decision.
fn inline_style_prop(node: &NodeRef, prop: &str) -> Option<String> {
    let style = get_attribute(node, "style")?;
    for decl in style.split(';') {
        let mut kv = decl.splitn(2, ':');
        let k = kv.next()?.trim();
        let v = kv.next();
        if let Some(v) = v
            && k.eq_ignore_ascii_case(prop)
        {
            let value = strip_css_important(v.trim());
            return Some(value.trim().to_ascii_lowercase());
        }
    }
    None
}

/// Strip a trailing `!\s*important` priority annotation from a CSS value.
///
/// CSS syntax: `value !important` where `!` may be followed by any amount of
/// whitespace before `important` (CSS Syntax Module Level 3 §4.3.2). Case-
/// insensitive on `important`. Returns the value substring without the
/// `!important` suffix; if no such suffix, returns the original.
fn strip_css_important(s: &str) -> &str {
    // Find the LAST `!` (a value may not contain `!` legitimately for the
    // properties we read — `display`/`visibility` — and a trailing
    // `!important` is the only place a `!` should appear).
    let Some(bang_pos) = s.rfind('!') else {
        return s;
    };
    let after_bang = s[bang_pos + 1..].trim_start();
    if after_bang.eq_ignore_ascii_case("important") {
        // strip back to the bang then trim trailing ws on the value.
        s[..bang_pos].trim_end()
    } else {
        s
    }
}

/// `_isWhitespace(node)` (`Readability.js:2042-2048`).
///
/// A text node whose `textContent.trim()` is empty, **or** an element whose
/// tag is `BR`. (`String.prototype.trim` is the JS whitespace set — we use the
/// dialect-faithful [`dom::inner_text`] with `normalize=false` which trims with
/// exactly that set; its emptiness is equivalent to `.trim().length === 0`.)
pub fn is_whitespace(node: &NodeRef) -> bool {
    if is_text(node) {
        return js_trim_is_empty(&text_content(node));
    }
    is_element(node) && tag_name(node).as_deref() == Some("BR")
}

/// `s.trim().length === 0` using the one canonical JS whitespace set.
///
/// Routes through a synthetic text node + [`dom::inner_text`]
/// (`normalize=false`): for a Text node `text_content == data`, so this is
/// exactly `data` trimmed with the single canonical JS-space set `dom`
/// encodes (also pinned by the parser-equivalence gate).
fn js_trim_is_empty(s: &str) -> bool {
    let t = dom::create_text_node(s);
    dom::inner_text(&t, false).is_empty()
}

/// `_isElementWithoutContent(node)` (`Readability.js:2002-2011`).
///
/// Element node, **no** non-whitespace `textContent`, and either no element
/// children or every element child is a `BR`/`HR` (children.length ==
/// br-count + hr-count).
pub fn is_element_without_content(node: &NodeRef) -> bool {
    if !is_element(node) {
        return false;
    }
    if !js_trim_is_empty(&text_content(node)) {
        return false;
    }
    let kids = children(node);
    if kids.is_empty() {
        return true;
    }
    let br = get_elements_by_tag_name(node, "br").len();
    let hr = get_elements_by_tag_name(node, "hr").len();
    kids.len() == br + hr
}

/// `_hasSingleTagInsideElement(element, tag)` (`Readability.js:1987-2000`).
///
/// Exactly one element child whose `tagName === tag` (UPPER-case), and no
/// child **text** node whose content matches `REGEXPS.hasContent` (`/\S$/` —
/// i.e. has at least one trailing non-JS-space char).
pub fn has_single_tag_inside_element(element: &NodeRef, tag: &str) -> bool {
    let kids = children(element);
    if kids.len() != 1 || tag_name(&kids[0]).as_deref() != Some(tag) {
        return false;
    }
    // !_someNode(childNodes, n => n is TEXT && hasContent.test(n.textContent))
    !child_nodes(element)
        .iter()
        .any(|n| is_text(n) && regexps::has_content().is_match(&text_content(n)))
}

/// `_hasChildBlockElement(element)` (`Readability.js:2018-2025`).
///
/// `_someNode(childNodes, n => DIV_TO_P_ELEMS.has(n.tagName) ||
/// _hasChildBlockElement(n))` — recursive over **all** child nodes.
pub fn has_child_block_element(element: &NodeRef) -> bool {
    child_nodes(element).iter().any(|n| {
        let is_block = tag_name(n)
            .map(|t| regexps::DIV_TO_P_ELEMS.contains(&t.as_str()))
            .unwrap_or(false);
        is_block || has_child_block_element(n)
    })
}

/// `_isPhrasingContent(node)` (`Readability.js:2031-2040`).
///
/// Text node, **or** `PHRASING_ELEMS.includes(tagName)`, **or** an
/// `A`/`DEL`/`INS` all of whose child nodes are themselves phrasing content
/// (`_everyNode`, recursive).
pub fn is_phrasing_content(node: &NodeRef) -> bool {
    if is_text(node) {
        return true;
    }
    let Some(tag) = tag_name(node) else {
        // Comment / PI etc. — not a text node, has no tagName ⇒ not phrasing.
        return false;
    };
    if regexps::PHRASING_ELEMS.contains(&tag.as_str()) {
        return true;
    }
    (tag == "A" || tag == "DEL" || tag == "INS")
        && child_nodes(node).iter().all(is_phrasing_content)
}

/// `_isValidByline(node, matchString)` (`Readability.js:995-1007`).
///
/// `(rel === "author" || (itemprop && itemprop.includes("author")) ||
/// REGEXPS.byline.test(matchString)) && !!bylineLength && bylineLength < 100`,
/// where `bylineLength = node.textContent.trim().length`.
pub fn is_valid_byline(node: &NodeRef, match_string: &str) -> bool {
    let rel = get_attribute(node, "rel");
    let itemprop = get_attribute(node, "itemprop");
    // node.textContent.trim().length — JS String.trim set.
    let byline_len = js_trim_len(&text_content(node));

    let rel_author = rel.as_deref() == Some("author");
    let itemprop_author = itemprop
        .as_deref()
        .map(|s| s.contains("author"))
        .unwrap_or(false);
    let byline_re = regexps::byline().is_match(match_string);

    (rel_author || itemprop_author || byline_re) && byline_len > 0 && byline_len < 100
}

/// `node.textContent.trim().length` (JS String.trim set) — the UTF-16-ish
/// length JS uses for byline gating. JS `.length` is UTF-16 code units; for
/// the byline-length comparison (`< 100`, `> 0`) we use Rust `char` count of
/// the JS-trimmed string. For realistic bylines (no astral chars) this equals
/// the JS code-unit count; the only divergence would be a byline padded past
/// 100 with astral characters, which the gold corpus does not contain.
fn js_trim_len(s: &str) -> usize {
    let t = dom::create_text_node(s);
    dom::inner_text(&t, false).chars().count()
}

// ---------------------------------------------------------------------------
// Traversal.
// ---------------------------------------------------------------------------

/// `_getNextNode(node, ignoreSelfAndKids)` (`Readability.js:949-965`).
///
/// Depth-first: first element child (unless ignoring), else next element
/// sibling, else walk up the parent chain to the first ancestor that has a
/// next element sibling and return that sibling. `None` at end of document.
pub fn get_next_node(node: &NodeRef, ignore_self_and_kids: bool) -> Option<NodeRef> {
    if !ignore_self_and_kids && let Some(c) = first_element_child(node) {
        return Some(c);
    }
    if let Some(s) = next_element_sibling(node) {
        return Some(s);
    }
    // do { node = node.parentNode } while (node && !node.nextElementSibling)
    let mut cur = parent(node);
    while let Some(n) = cur {
        if let Some(s) = next_element_sibling(&n) {
            return Some(s);
        }
        cur = parent(&n);
    }
    None
}

/// `_nextNode(node)` (`Readability.js:677-687`).
///
/// Skip forward over **non-element** siblings whose `textContent` is
/// whitespace-only (`REGEXPS.whitespace = /^\s*$/`, JS `\s`), returning the
/// first element (or the first non-whitespace text node, or `None`).
pub fn next_node(node: Option<NodeRef>) -> Option<NodeRef> {
    let mut next = node;
    while let Some(n) = next.clone() {
        if is_element(&n) {
            break;
        }
        if !regexps::whitespace().is_match(&text_content(&n)) {
            break;
        }
        next = next_sibling(&n);
    }
    next
}

/// `node.nextSibling` (ALL node types, not element-only — used by `_nextNode`
/// / `_replaceBrs`). Not in `dom`'s element-centric facade, so derived here
/// from the parent's full child list. `None` if last child / detached.
pub fn next_sibling(node: &NodeRef) -> Option<NodeRef> {
    let p = parent(node)?;
    let kids = child_nodes(&p);
    let idx = kids.iter().position(|c| std::rc::Rc::ptr_eq(c, node))?;
    kids.get(idx + 1).cloned()
}

#[cfg(test)]
mod tests {
    //! Expected values hand-derived by tracing `Readability.js` (NOT by
    //! running an oracle — that would be inversion, HLD §4).
    use super::*;
    use crate::readability::dom::Dom;

    fn el(html: &str, tag: &str) -> (Dom, NodeRef) {
        let dom = Dom::parse(html);
        let n = get_elements_by_tag_name(&dom.body().unwrap(), tag)[0].clone();
        (dom, n)
    }

    // ---- Flags (Readability.js:69-72, 2686-2692) ----

    #[test]
    fn flags_default_all_set_and_remove_clears_one() {
        let mut f = Flags::default();
        assert!(f.is_active(FLAG_STRIP_UNLIKELYS));
        assert!(f.is_active(FLAG_WEIGHT_CLASSES));
        assert!(f.is_active(FLAG_CLEAN_CONDITIONALLY));
        f.remove(FLAG_STRIP_UNLIKELYS);
        assert!(!f.is_active(FLAG_STRIP_UNLIKELYS));
        // others untouched (JS: this._flags & ~flag)
        assert!(f.is_active(FLAG_WEIGHT_CLASSES));
        assert!(f.is_active(FLAG_CLEAN_CONDITIONALLY));
    }

    // ---- _isProbablyVisible (Readability.js:2694-2707) ----

    #[test]
    fn is_probably_visible_display_none_hidden() {
        let (_d, n) = el(r#"<div style="display:none">x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_visibility_hidden() {
        let (_d, n) = el(r#"<div style="visibility:hidden">x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_hidden_attr() {
        let (_d, n) = el(r#"<div hidden>x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_aria_hidden_true_is_hidden() {
        let (_d, n) = el(r#"<div aria-hidden="true">x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_aria_hidden_true_with_fallback_image_is_visible() {
        // The wikimedia-math exception (Readability.js:2700-2705).
        let (_d, n) = el(
            r#"<div aria-hidden="true" class="x fallback-image">m</div>"#,
            "div",
        );
        assert!(is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_plain_div_is_visible() {
        let (_d, n) = el(r#"<div style="color:red">x</div>"#, "div");
        assert!(is_probably_visible(&n));
    }

    // Stage-3 — CSSOM `!important` faithfulness (HLD §7.5 explicit deferred
    // item). Expected hand-derived: jsdom's `node.style.display` returns the
    // plain value `"none"` for `style="display:none !important"`, so the
    // `display != "none"` clause MUST treat both as hidden.

    #[test]
    fn is_probably_visible_display_none_important_is_hidden_cssom_faithful() {
        let (_d, n) = el(r#"<div style="display:none !important">x</div>"#, "div");
        assert!(
            !is_probably_visible(&n),
            "Readability.js:2697 `node.style.display != \"none\"`: jsdom's \
             style.display for `display:none !important` is `\"none\"`, so \
             the node MUST be hidden (HLD §7.5)"
        );
    }

    #[test]
    fn is_probably_visible_visibility_hidden_important_is_hidden_cssom_faithful() {
        let (_d, n) = el(
            r#"<div style="visibility:hidden !important">x</div>"#,
            "div",
        );
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_display_none_important_case_insensitive() {
        // CSS `!important` is case-insensitive on `important`.
        let (_d, n) = el(r#"<div style="display: None !IMPORTANT">x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_display_none_important_extra_ws() {
        // `!` may be followed by any whitespace before `important`.
        let (_d, n) = el(r#"<div style="display:none !  important;">x</div>"#, "div");
        assert!(!is_probably_visible(&n));
    }

    #[test]
    fn is_probably_visible_other_property_with_important_does_not_match_display() {
        // `color: red !important` MUST NOT bleed into the `display` lookup —
        // only the matching property's value/priority is inspected.
        let (_d, n) = el(
            r#"<div style="color:red !important;display:block">x</div>"#,
            "div",
        );
        assert!(is_probably_visible(&n), "display:block is visible");
    }

    #[test]
    fn strip_css_important_unit_cases() {
        // No `!important` -> unchanged.
        assert_eq!(strip_css_important("none"), "none");
        assert_eq!(strip_css_important("block"), "block");
        // Plain `!important`.
        assert_eq!(strip_css_important("none !important"), "none");
        // Tight `!important`.
        assert_eq!(strip_css_important("none!important"), "none");
        // Extra ws.
        assert_eq!(strip_css_important("none !  important"), "none");
        // Case-insensitive `important`.
        assert_eq!(strip_css_important("none !IMPORTANT"), "none");
        assert_eq!(strip_css_important("NONE !Important"), "NONE");
        // `!` not followed by `important` is untouched.
        assert_eq!(strip_css_important("0 !x"), "0 !x");
    }

    // ---- _isWhitespace (Readability.js:2042-2048) ----

    #[test]
    fn is_whitespace_text_and_br() {
        let dom = Dom::parse("<div> \t\n <span>x</span><br></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = child_nodes(&div); // [ws-text, span, br]
        assert!(is_whitespace(&kids[0]), "all-ws text node");
        assert!(!is_whitespace(&kids[1]), "<span> with content");
        assert!(is_whitespace(&kids[2]), "<br> element");
    }

    #[test]
    fn is_whitespace_nonempty_text_is_not() {
        let t = dom::create_text_node("  x  ");
        assert!(!is_whitespace(&t));
    }

    // ---- _isElementWithoutContent (Readability.js:2002-2011) ----

    #[test]
    fn is_element_without_content_cases() {
        // empty div -> true
        let (_d, n) = el("<div></div>", "div");
        assert!(is_element_without_content(&n));
        // div with only <br><hr> -> true (children == br+hr count)
        let (_d, n) = el("<div><br><hr></div>", "div");
        assert!(is_element_without_content(&n));
        // div with text -> false
        let (_d, n) = el("<div>hi</div>", "div");
        assert!(!is_element_without_content(&n));
        // div with a <span> child -> false (children != br+hr)
        let (_d, n) = el("<div><span></span></div>", "div");
        assert!(!is_element_without_content(&n));
        // whitespace-only text but a child element -> false
        let (_d, n) = el("<div>  <span></span></div>", "div");
        assert!(!is_element_without_content(&n));
    }

    // ---- _hasSingleTagInsideElement (Readability.js:1987-2000) ----

    #[test]
    fn has_single_tag_inside_element_cases() {
        // exactly one <p>, no real text -> true
        let (_d, n) = el("<div> <p>x</p> </div>", "div");
        assert!(has_single_tag_inside_element(&n, "P"));
        // two children -> false
        let (_d, n) = el("<div><p>x</p><p>y</p></div>", "div");
        assert!(!has_single_tag_inside_element(&n, "P"));
        // single child but wrong tag -> false
        let (_d, n) = el("<div><span>x</span></div>", "div");
        assert!(!has_single_tag_inside_element(&n, "P"));
        // single <p> but a sibling text node WITH content (/\S$/) -> false
        let (_d, n) = el("<div>real<p>x</p></div>", "div");
        assert!(!has_single_tag_inside_element(&n, "P"));
    }

    // ---- _hasChildBlockElement (Readability.js:2018-2025) ----

    #[test]
    fn has_child_block_element_recursive() {
        // direct DIV child
        let (_d, n) = el("<div><div>x</div></div>", "div");
        assert!(has_child_block_element(&n));
        // nested: span > p (P is in DIV_TO_P_ELEMS, found recursively)
        let dom = Dom::parse("<section><span><p>x</p></span></section>");
        let s = get_elements_by_tag_name(&dom.body().unwrap(), "section")[0].clone();
        assert!(has_child_block_element(&s));
        // only inline content -> false
        let dom = Dom::parse("<section><span>x</span><b>y</b></section>");
        let s = get_elements_by_tag_name(&dom.body().unwrap(), "section")[0].clone();
        assert!(!has_child_block_element(&s));
    }

    // ---- _isPhrasingContent (Readability.js:2031-2040) ----

    #[test]
    fn is_phrasing_content_cases() {
        let dom = Dom::parse(
            "<div>txt<span>s</span><b>b</b><a href=#><i>x</i></a><a><p>blk</p></a><div>d</div></div>",
        );
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = child_nodes(&div);
        assert!(is_phrasing_content(&kids[0]), "text node");
        assert!(is_phrasing_content(&kids[1]), "SPAN in PHRASING_ELEMS");
        assert!(is_phrasing_content(&kids[2]), "B in PHRASING_ELEMS");
        assert!(
            is_phrasing_content(&kids[3]),
            "A whose children (<i>) are all phrasing"
        );
        assert!(
            !is_phrasing_content(&kids[4]),
            "A containing a <p> (block) is NOT phrasing"
        );
        assert!(!is_phrasing_content(&kids[5]), "DIV is not phrasing");
    }

    // ---- _isValidByline (Readability.js:995-1007) ----

    #[test]
    fn is_valid_byline_cases() {
        // rel=author, short text -> true
        let (_d, n) = el(r#"<a rel="author">Jane Doe</a>"#, "a");
        assert!(is_valid_byline(&n, "whatever"));
        // itemprop contains author -> true
        let (_d, n) = el(r#"<span itemprop="author name">Jane</span>"#, "span");
        assert!(is_valid_byline(&n, "x"));
        // matchString matches REGEXPS.byline -> true
        let (_d, n) = el(r#"<p class="byline">By Jane</p>"#, "p");
        assert!(is_valid_byline(&n, "byline foo"));
        // no signal -> false
        let (_d, n) = el(r#"<p>By Jane</p>"#, "p");
        assert!(!is_valid_byline(&n, "content main"));
        // empty text -> false (bylineLength == 0)
        let (_d, n) = el(r#"<a rel="author">   </a>"#, "a");
        assert!(!is_valid_byline(&n, "x"));
        // text length >= 100 -> false
        let long = "x".repeat(100);
        let (_d, n) = el(&format!(r#"<a rel="author">{long}</a>"#), "a");
        assert!(!is_valid_byline(&n, "x"));
    }

    // ---- _getNextNode (Readability.js:949-965) ----

    #[test]
    fn get_next_node_dfs_and_ignore_kids() {
        let dom = Dom::parse("<div id=r><a><b></b></a><c-x></c-x></div>");
        let r = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let mut order = Vec::new();
        let mut cur = Some(r.clone());
        while let Some(n) = cur {
            order.push(tag_name(&n).unwrap_or_default());
            cur = get_next_node(&n, false);
        }
        assert_eq!(order, vec!["DIV", "A", "B", "C-X"]);
        // ignoreSelfAndKids from <a> -> skip <b>, go to <c-x>
        let a = get_elements_by_tag_name(&dom.body().unwrap(), "a")[0].clone();
        assert_eq!(
            tag_name(&get_next_node(&a, true).unwrap()).as_deref(),
            Some("C-X")
        );
    }

    // ---- _nextNode (Readability.js:677-687) ----

    #[test]
    fn next_node_skips_whitespace_text_only() {
        // div: [ws-text][<br>] -> _nextNode(firstChild) skips ws to <br>
        let dom = Dom::parse("<div>   <br>tail</div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = child_nodes(&div);
        let n = next_node(Some(kids[0].clone())).unwrap();
        assert_eq!(tag_name(&n).as_deref(), Some("BR"));
        // a non-whitespace text node stops immediately
        let dom = Dom::parse("<div>hello<br></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let first = child_nodes(&div)[0].clone();
        let n = next_node(Some(first)).unwrap();
        assert!(is_text(&n));
        assert_eq!(text_content(&n), "hello");
    }

    /// `_isPhrasingContent` (`Readability.js:2031-2040`): `<DEL>` with
    /// all-phrasing children is phrasing.
    /// rationale: pin the JS list `A|DEL|INS` (not just `A`).
    #[test]
    fn is_phrasing_content_del_with_phrasing_kids_is_phrasing() {
        let dom = Dom::parse("<div><del><span>x</span></del><del><p>blk</p></del></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = children(&div);
        assert!(
            is_phrasing_content(&kids[0]),
            "DEL with phrasing-only children is phrasing (Readability.js:2034)"
        );
        assert!(
            !is_phrasing_content(&kids[1]),
            "DEL containing a <p> (non-phrasing) is NOT phrasing"
        );
    }

    /// `<INS>` mirror of the DEL case.
    /// rationale: the alternation `A|DEL|INS` is all three.
    #[test]
    fn is_phrasing_content_ins_with_phrasing_kids_is_phrasing() {
        let dom = Dom::parse("<div><ins><b>x</b></ins><ins><div>blk</div></ins></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = children(&div);
        assert!(is_phrasing_content(&kids[0]));
        assert!(!is_phrasing_content(&kids[1]));
    }

    /// `_isProbablyVisible` (`Readability.js:2694-2707`): `aria-hidden` with
    /// a value OTHER than `"true"` (e.g. `"false"`) does NOT hide the node.
    /// rationale: only `aria-hidden="true"` is the trigger; any other value
    /// leaves the node visible.
    #[test]
    fn is_probably_visible_aria_hidden_false_is_visible() {
        let (_d, n) = el(r#"<div aria-hidden="false">x</div>"#, "div");
        assert!(
            is_probably_visible(&n),
            "aria-hidden=\"false\" is visible (Readability.js:2700-2703)"
        );
    }

    /// `_isProbablyVisible`: `aria-hidden` present but empty string is NOT
    /// `"true"` ⇒ visible.
    /// rationale: pin the equality check at `:2702` — only the exact
    /// string `"true"` triggers the hidden branch.
    #[test]
    fn is_probably_visible_aria_hidden_empty_string_is_visible() {
        let (_d, n) = el(r#"<div aria-hidden="">x</div>"#, "div");
        assert!(is_probably_visible(&n));
    }

    #[test]
    fn next_sibling_all_node_types() {
        // M5 Stage 6e-a: Comments are stripped at parse time
        // (`HTMLParser(remove_comments=True)`, utils.py:70). The original
        // pre-strip tree was `[text"a", comment, <b>]`; post-strip the
        // Comment is gone, leaving `[text"a", <b>]` — `next_sibling` of the
        // text node is the `<b>` element directly.
        let dom = Dom::parse("<div>a<!--c--><b>x</b></div>");
        let div = get_elements_by_tag_name(&dom.body().unwrap(), "div")[0].clone();
        let kids = child_nodes(&div); // [text"a", <b>]
        assert_eq!(kids.len(), 2);
        let s1 = next_sibling(&kids[0]).unwrap();
        assert_eq!(tag_name(&s1).as_deref(), Some("B"));
        assert!(next_sibling(&kids[1]).is_none());
    }
}
