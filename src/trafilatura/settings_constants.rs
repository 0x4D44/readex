//! `settings_constants` — verbatim Rust vendoring of the catalog constants
//! Stage 1b depends on (HLD M3 §7.2).
//!
//! Source of truth: `trafilatura@v2.0.0` Python source under
//! `site-packages/trafilatura/`. Every constant in this file traces, line by
//! line, to a Python literal at the cited line range. No reordering, no
//! "looks-nice-in-Rust" decisions — the lists are kept as `&[&str]` (not
//! `HashSet`) because Trafilatura's own comment at `settings.py:348`
//! ("order could matter, using lists to keep extraction deterministic")
//! makes determinism load-bearing. Membership tests over the small (≤50)
//! constant tables are linear scans of `&[&str]`; this is faster than a
//! `HashSet<&str>` lookup at this scale and avoids importing `std::collections`
//! at module load.
//!
//! # Anti-inversion (HLD §10)
//!
//! Every entry here is one of:
//! - a `MANUALLY_CLEANED` literal from `settings.py:349-404`;
//! - a `MANUALLY_STRIPPED` literal from `settings.py:407-429`;
//! - a `REND_TAG_MAPPING` literal from `htmlprocessing.py:29-41`;
//! - a `CUT_EMPTY_ELEMS` literal from `settings.py:320-343` (used by
//!   `prune_html` per `htmlprocessing.py:87-89`);
//! - a `PRESERVE_IMG_CLEANING` literal from `htmlprocessing.py:45`.
//!
//! No entry exists that does not have a corresponding Python literal at the
//! cited line. The conformance discipline (Stage 0c gate) cross-checks the
//! resulting post-`convert_tags` tree byte-for-byte against Trafilatura's own
//! output, which is the empirical anchor.

/// `MANUALLY_CLEANED` (settings.py:349-404).
///
/// Tags whose **entire subtree** is dropped by `tree_cleaning` (the Python
/// `delete_element` call at `htmlprocessing.py:77-78`). Order preserved
/// verbatim (Trafilatura's `# order could matter` comment at line 348).
///
/// The trailing comment at `settings.py:405`
/// (`# 'meta', 'hr', 'img', 'data', 'details', 'summary'`) is a record of
/// tags that were *considered* but excluded — they are NOT in the list and
/// not added here either.
pub(crate) const MANUALLY_CLEANED: &[&str] = &[
    // important (settings.py:350-359)
    "aside", "embed", "footer", "form", "head", "iframe", "menu", "object", "script",
    // other content (settings.py:360-368)
    "applet", "audio", "canvas", "figure", "map", "picture", "svg", "video",
    // secondary (settings.py:369-404)
    "area", "blink", "button", "datalist", "dialog", "frame", "frameset", "fieldset", "link",
    "input", "ins", "label", "legend", "marquee", "math", "menuitem", "nav", "noindex", "noscript",
    "optgroup", "option", "output", "param", "progress", "rp", "rt", "rtc", "select", "source",
    "style", "track", "textarea", "time", "use",
];

/// `MANUALLY_STRIPPED` (settings.py:407-429).
///
/// Tags whose **element wrapper** is removed by `tree_cleaning` but whose
/// children + tail text are preserved (Trafilatura's `strip_tags(tree,
/// stripping_list)` at `htmlprocessing.py:64` — the lxml `strip_tags`
/// semantic that unwraps an element in place).
///
/// The trailing comment at `settings.py:430`
/// (`# 'center', 'rb', 'wbr'`) records tags considered but not stripped.
pub(crate) const MANUALLY_STRIPPED: &[&str] = &[
    "abbr", "acronym", "address", "bdi", "bdo", "big", "cite", "data", "dfn", "font", "hgroup",
    "img", "ins", "mark", "meta", "ruby", "small", "tbody", "template", "tfoot", "thead",
];

/// `CUT_EMPTY_ELEMS` (settings.py:320-343).
///
/// Tags whose **empty** instances (no element children, no text) are dropped
/// by `prune_html` (`htmlprocessing.py:87-89`). In Trafilatura this is a
/// `set`; we store as `&[&str]` for the same determinism reason as the other
/// catalogs (membership is `.contains(&tag)`, O(n) over ≤22 entries — cheap).
///
/// Trailing comments at `settings.py:344-346` record tags considered but not
/// in the set ("'meta', 'td', 'a', 'caption', 'dl', 'header', 'colgroup',
/// 'col'") plus an alternative narrow set ("CUT_EMPTY_ELEMS = {'div', 'span'}").
/// Neither is part of the runtime set.
pub(crate) const CUT_EMPTY_ELEMS: &[&str] = &[
    "article",
    "b",
    "blockquote",
    "dd",
    "div",
    "dt",
    "em",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "i",
    "li",
    "main",
    "p",
    "pre",
    "q",
    "section",
    "span",
    "strong",
];

/// `PRESERVE_IMG_CLEANING` (htmlprocessing.py:45).
///
/// Tags removed from the cleaning list when `options.images` is true — these
/// are the wrappers that commonly contain `<img>` and so must NOT be cleaned
/// when image extraction is enabled (`htmlprocessing.py:58-61`).
///
/// At Stage 1b the public Options surface does not yet expose `images`; the
/// Stage 1b convert_tags path runs with the Trafilatura defaults
/// (`images=false`), so this constant is wired but exercised only via the
/// `Options::images` toggle in the implementation. Vendored verbatim now to
/// avoid a later additive churn.
pub(crate) const PRESERVE_IMG_CLEANING: &[&str] = &["figure", "picture", "source"];

/// `REND_TAG_MAPPING` (htmlprocessing.py:29-41).
///
/// HTML tag → TEI `rend` value. Trafilatura's `convert_tags` (htmlprocessing.py:402-405)
/// iterates this dict, clears the element's attributes, sets `rend = mapping[tag]`,
/// and renames the element to `hi`. We store as a slice of `(from, rend)` pairs:
/// linear scan over 11 entries is faster than a `HashMap` at this size and
/// preserves the deterministic ordering Trafilatura's `# order could matter`
/// doctrine pins for other catalogs (REND_TAG_MAPPING is a dict in Python and
/// dict iteration order is insertion order under CPython 3.7+; we preserve
/// that order here).
///
/// **Verbatim verification (Python source vs this constant):**
///
/// | Python line | Python literal     | Rust entry          |
/// |-------------|--------------------|---------------------|
/// | 30          | `"em": "#i"`       | `("em", "#i")`      |
/// | 31          | `"i": "#i"`        | `("i", "#i")`       |
/// | 32          | `"b": "#b"`        | `("b", "#b")`       |
/// | 33          | `"strong": "#b"`   | `("strong", "#b")`  |
/// | 34          | `"u": "#u"`        | `("u", "#u")`       |
/// | 35          | `"kbd": "#t"`      | `("kbd", "#t")`     |
/// | 36          | `"samp": "#t"`     | `("samp", "#t")`    |
/// | 37          | `"tt": "#t"`       | `("tt", "#t")`      |
/// | 38          | `"var": "#t"`      | `("var", "#t")`     |
/// | 39          | `"sub": "#sub"`    | `("sub", "#sub")`   |
/// | 40          | `"sup": "#sup"`    | `("sup", "#sup")`   |
///
/// The task brief enumerated `<i>` → `<hi rend="#i">` and `<em>` →
/// `<hi rend="#em">` "respectively"; the Python source disagrees — `em` maps
/// to `#i`, not `#em`. The Python source is the algorithm spec source of truth
/// (HLD §1); this constant matches the Python source verbatim. The Stage 0c
/// gate then catches any divergence empirically.
pub(crate) const REND_TAG_MAPPING: &[(&str, &str)] = &[
    ("em", "#i"),
    ("i", "#i"),
    ("b", "#b"),
    ("strong", "#b"),
    ("u", "#u"),
    ("kbd", "#t"),
    ("samp", "#t"),
    ("tt", "#t"),
    ("var", "#t"),
    ("sub", "#sub"),
    ("sup", "#sup"),
];

/// Lookup helper: tag name → TEI `rend` value for `REND_TAG_MAPPING`.
/// Returns `None` if `tag` is not in the mapping.
pub(crate) fn rend_of(tag: &str) -> Option<&'static str> {
    REND_TAG_MAPPING
        .iter()
        .find(|(t, _)| *t == tag)
        .map(|(_, r)| *r)
}

/// All keys of `REND_TAG_MAPPING` (in declared order). Convenience accessor
/// for callers that need the tag list (e.g. `convert_tags`'s `tree.iter(
/// REND_TAG_MAPPING.keys())` shape at `htmlprocessing.py:402`).
pub(crate) const REND_TAG_NAMES: &[&str] = &[
    "em", "i", "b", "strong", "u", "kbd", "samp", "tt", "var", "sub", "sup",
];

#[cfg(test)]
mod tests {
    use super::*;

    // ---- catalog cardinalities (sanity tripwires) -------------------------
    //
    // Numbers below are derived from a hand-count of the Python source line
    // ranges cited in each constant's doc comment. Any future edit to the
    // Python source that changes the count must be reflected here and in the
    // line-range citations above.

    #[test]
    fn manually_cleaned_has_51_entries() {
        // Authoritative count from `python -c 'from trafilatura.settings import
        // MANUALLY_CLEANED; print(len(MANUALLY_CLEANED))'` -> 51.
        // settings.py:349-404 declares 51 literals across the three section
        // groups (important / other content / secondary); the trailing
        // comment at :405 is not part of the list.
        assert_eq!(MANUALLY_CLEANED.len(), 51);
    }

    #[test]
    fn manually_stripped_has_21_entries() {
        // settings.py:408-428 = 21 literals.
        assert_eq!(MANUALLY_STRIPPED.len(), 21);
    }

    #[test]
    fn cut_empty_elems_has_22_entries() {
        // settings.py:321-342 = 22 literals.
        assert_eq!(CUT_EMPTY_ELEMS.len(), 22);
    }

    #[test]
    fn preserve_img_cleaning_has_3_entries() {
        assert_eq!(PRESERVE_IMG_CLEANING.len(), 3);
    }

    #[test]
    fn rend_tag_mapping_has_11_entries() {
        // htmlprocessing.py:30-40 = 11 mappings.
        assert_eq!(REND_TAG_MAPPING.len(), 11);
        assert_eq!(REND_TAG_NAMES.len(), 11);
    }

    // ---- REND_TAG_MAPPING content vs source (anti-inversion anchor) -------

    #[test]
    fn rend_of_b_is_hash_b() {
        // htmlprocessing.py:32: "b": "#b"
        assert_eq!(rend_of("b"), Some("#b"));
    }

    #[test]
    fn rend_of_strong_is_hash_b() {
        // htmlprocessing.py:33: "strong": "#b"
        assert_eq!(rend_of("strong"), Some("#b"));
    }

    #[test]
    fn rend_of_em_is_hash_i_not_hash_em() {
        // htmlprocessing.py:30: "em": "#i"  (NOT "#em" as the brief speculated)
        assert_eq!(rend_of("em"), Some("#i"));
    }

    #[test]
    fn rend_of_i_is_hash_i() {
        // htmlprocessing.py:31: "i": "#i"
        assert_eq!(rend_of("i"), Some("#i"));
    }

    #[test]
    fn rend_of_kbd_samp_tt_var_all_hash_t() {
        // htmlprocessing.py:35-38: monospace family all collapse to "#t"
        assert_eq!(rend_of("kbd"), Some("#t"));
        assert_eq!(rend_of("samp"), Some("#t"));
        assert_eq!(rend_of("tt"), Some("#t"));
        assert_eq!(rend_of("var"), Some("#t"));
    }

    #[test]
    fn rend_of_sub_sup_distinct() {
        assert_eq!(rend_of("sub"), Some("#sub"));
        assert_eq!(rend_of("sup"), Some("#sup"));
    }

    #[test]
    fn rend_of_unmapped_is_none() {
        assert_eq!(rend_of("p"), None);
        assert_eq!(rend_of("div"), None);
        assert_eq!(rend_of("hi"), None); // hi is the TARGET, not a source
    }

    #[test]
    fn rend_tag_names_match_mapping_keys() {
        let from_mapping: Vec<&str> = REND_TAG_MAPPING.iter().map(|(t, _)| *t).collect();
        assert_eq!(from_mapping, REND_TAG_NAMES);
    }

    // ---- spot-checks on cleaning catalogs ---------------------------------

    #[test]
    fn manually_cleaned_contains_canonical_boilerplate_tags() {
        // The "important" group must include the canonical chrome.
        //
        // Branch contract: the `||` short-circuits on a per-tag basis. To
        // pin BOTH halves of the OR, the iterated set MUST include:
        //   1. tags only in MANUALLY_CLEANED (`script` etc.) — LHS=True path,
        //   2. tags only in MANUALLY_STRIPPED (`abbr`, `tbody` etc.) — LHS=False
        //      forces the RHS evaluation, RHS=True closes the loop.
        // Without (2), the RHS `contains` is never reached. settings.py:407-429
        // (MANUALLY_STRIPPED) and settings.py:349-404 (MANUALLY_CLEANED)
        // together must cover every boilerplate tag the cleaner handles.
        for tag in [
            // cleaned-only (LHS True)
            "aside", "footer", "form", "head", "iframe", "nav", "script", "style",
            // stripped-only (LHS False, RHS True) — exercises the RHS half.
            "abbr", "cite", "font", "tbody", "thead", "tfoot",
        ] {
            assert!(
                MANUALLY_CLEANED.contains(&tag) || MANUALLY_STRIPPED.contains(&tag),
                "{tag} should be in MANUALLY_CLEANED or MANUALLY_STRIPPED",
            );
        }
        // "style" specifically is in MANUALLY_CLEANED, not stripped.
        assert!(MANUALLY_CLEANED.contains(&"style"));
        assert!(MANUALLY_CLEANED.contains(&"nav"));
        assert!(MANUALLY_CLEANED.contains(&"footer"));
        assert!(MANUALLY_CLEANED.contains(&"aside"));
    }

    #[test]
    fn manually_stripped_contains_typography_unwrappers() {
        // The "strip" list unwraps presentational containers (children stay).
        for tag in [
            "abbr", "cite", "font", "img", "meta", "tbody", "thead", "tfoot",
        ] {
            assert!(
                MANUALLY_STRIPPED.contains(&tag),
                "{tag} should be in MANUALLY_STRIPPED"
            );
        }
    }

    #[test]
    fn cut_empty_elems_contains_block_carriers() {
        for tag in [
            "article",
            "blockquote",
            "div",
            "h1",
            "h2",
            "h3",
            "p",
            "section",
            "span",
        ] {
            assert!(
                CUT_EMPTY_ELEMS.contains(&tag),
                "{tag} should be in CUT_EMPTY_ELEMS"
            );
        }
    }

    #[test]
    fn cleaning_and_stripping_are_disjoint() {
        // A tag can't be both removed-entirely and unwrapped-in-place;
        // Trafilatura's source treats them as disjoint operations.
        // Exception: "ins" appears in BOTH (settings.py:381 "ins" in
        // MANUALLY_CLEANED, settings.py:420 "ins" in MANUALLY_STRIPPED).
        // That is faithful to upstream — strip_tags is applied FIRST
        // (htmlprocessing.py:64) so `<ins>` is unwrapped, then the cleaning
        // pass over the now-absent `<ins>` is a no-op. Vendored verbatim.
        let mut overlap = Vec::new();
        for c in MANUALLY_CLEANED {
            if MANUALLY_STRIPPED.contains(c) {
                overlap.push(*c);
            }
        }
        assert_eq!(
            overlap,
            vec!["ins"],
            "Only 'ins' is expected to appear in both lists per Trafilatura source"
        );
    }

    // ---- short-circuit OR branch coverage (settings.py:407-429 STRIPPED-but-
    //      not-CLEANED tags exercise the right-hand `contains` arm) ----------

    #[test]
    fn manually_stripped_only_tag_lights_or_right_arm() {
        // rationale: the canonical-boilerplate-tags assertion uses
        //   `MANUALLY_CLEANED.contains(&tag) || MANUALLY_STRIPPED.contains(&tag)`
        // (settings_constants.rs:285). For tags that are in MANUALLY_STRIPPED
        // but NOT in MANUALLY_CLEANED (settings.py:407-429 — `abbr`, `cite`,
        // `font`, `tbody`, etc.) the short-circuit forces the right-hand
        // `MANUALLY_STRIPPED.contains` arm to evaluate True. This pins the
        // contract that BOTH catalogs participate in the "is this a cleaning
        // tag?" check.
        for tag in ["abbr", "cite", "font", "tbody", "thead", "tfoot", "small"] {
            // Sanity check the asymmetric membership before exercising the OR.
            assert!(
                !MANUALLY_CLEANED.contains(&tag),
                "{tag} unexpectedly in MANUALLY_CLEANED",
            );
            assert!(
                MANUALLY_STRIPPED.contains(&tag),
                "{tag} should be in MANUALLY_STRIPPED",
            );
            // The OR expression: first half False, second half True.
            let in_either = MANUALLY_CLEANED.contains(&tag) || MANUALLY_STRIPPED.contains(&tag);
            assert!(in_either, "{tag} should be matched by either catalog");
        }
    }

    #[test]
    fn unknown_tag_lights_or_both_false_arm() {
        // rationale: the OR at settings_constants.rs:285 still needs the
        // "neither catalog matches" case for the False-side of the second
        // `contains` to be observed. Tags that Trafilatura intentionally
        // leaves OUT of both catalogs (e.g. plain `<p>`, `<div>`, `<span>`,
        // `<h1>`) flow through `tree_cleaning` untouched. Per settings.py:344
        // these are the "considered-but-excluded" tags — `<p>` and `<div>`
        // specifically appear in the trailing comments and the surrounding
        // discussion.
        for tag in ["p", "div", "span", "h1", "article", "section"] {
            assert!(
                !MANUALLY_CLEANED.contains(&tag),
                "{tag} should NOT be in MANUALLY_CLEANED",
            );
            assert!(
                !MANUALLY_STRIPPED.contains(&tag),
                "{tag} should NOT be in MANUALLY_STRIPPED",
            );
            // The OR expression: both halves False.
            let in_either = MANUALLY_CLEANED.contains(&tag) || MANUALLY_STRIPPED.contains(&tag);
            assert!(!in_either, "{tag} should not be matched by either catalog");
        }
    }

    #[test]
    fn cleaned_only_tag_short_circuits_or_left_arm() {
        // rationale: pins the LEFT-half True path of the OR at
        // settings_constants.rs:285 — tags in MANUALLY_CLEANED (and NOT in
        // MANUALLY_STRIPPED) short-circuit on the first `contains` and never
        // consult MANUALLY_STRIPPED. settings.py:349-404 lists the cleaned-
        // only tags.
        for tag in ["script", "style", "iframe", "nav", "footer", "aside", "form"] {
            assert!(
                MANUALLY_CLEANED.contains(&tag),
                "{tag} should be in MANUALLY_CLEANED",
            );
            assert!(
                !MANUALLY_STRIPPED.contains(&tag),
                "{tag} should NOT be in MANUALLY_STRIPPED",
            );
            let in_either = MANUALLY_CLEANED.contains(&tag) || MANUALLY_STRIPPED.contains(&tag);
            assert!(in_either, "{tag} should match via MANUALLY_CLEANED");
        }
    }
}
