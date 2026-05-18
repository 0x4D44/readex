//! `regexps.rs` — the `Readability.js` `REGEXPS` table + constant lists
//! (`Readability.js:137-264`), ported with **explicit JS-compatible character
//! classes** (HLD §8).
//!
//! # Why this module is delicate (HLD §8, supervisor M-5)
//!
//! JS `RegExp` and Rust `regex` are **different dialects**; a silent class
//! mismatch corrupts scoring invisibly. The binding rules (HLD §8), applied to
//! every pattern below:
//!
//! - **ASCII `\W`** is written `(?-u:\W)` — never bare Rust `\W` (which is
//!   Unicode-aware and matches differently on non-ASCII). JS `\W` is ASCII.
//! - **JS `\s`** is written as the explicit class
//!   [`JS_SPACE_CLASS`] — never bare Rust `\s`. JS `\s` **includes U+FEFF**,
//!   which Rust `regex`'s `\s` **excludes**; that one character is the exact
//!   trap HLD §8 documents. `\v` = U+000B is included explicitly too.
//! - JS `\S` becomes `[^<JS_SPACE_CLASS>]` for the same reason.
//! - The `/i` flag → Rust `(?i)`. JS `/i` is Unicode-simple-case-folding;
//!   every `REGEXPS` `/i` pattern is ASCII-keyword-only so Rust `(?i)` (which
//!   *adds* Unicode case folding) is behaviourally identical **on the inputs
//!   these patterns see** (class/id strings, titles). `adWords`/`loadingWords`
//!   carry `/u` in JS and contain non-ASCII alternatives — not in Stage-1a
//!   scope, so deferred (declared, not ported, below).
//!
//! **Verified (HLD §8):** there are **no backreferences and no lookaround**
//! anywhere in `REGEXPS`, so Rust `regex` is expressively sufficient (no
//! `fancy-regex`); and there is **no `Math.round`** on the scored path.
//!
//! A per-regex conformance test table (`#[cfg(test)]` below) pins each ported
//! pattern against hand-derived JS-match expectations — the HLD §8 Stage-1a
//! oracle. It is hand-derived by reading the JS regex semantics, **not** by
//! running Readability (that would be inversion, HLD §4).

use std::sync::OnceLock;

use regex::Regex;

/// The explicit ECMAScript `\s` class body (HLD §8).
///
/// ECMA-262 `WhiteSpace` ∪ `LineTerminator`, as a character-class **body**
/// (no surrounding `[]`), so it can be spliced into both `[...]` and `[^...]`.
/// Members: TAB U+0009, LF U+000A, **VT U+000B** (`\v`), FF U+000C, CR U+000D,
/// SPACE U+0020, NBSP U+00A0, **ZWNBSP/BOM U+FEFF** (the Rust-`\s` trap),
/// LS U+2028, PS U+2029, and every `Zs`: U+1680, U+2000–U+200A, U+202F,
/// U+205F, U+3000. This mirrors `dom.rs::is_js_space` exactly (same set, one
/// in a `matches!`, one as a regex class) — Stage 0 introduced the predicate;
/// Stage 1a formalises the class + its conformance table per HLD §8.
pub const JS_SPACE_CLASS: &str = "\u{0009}\u{000A}\u{000B}\u{000C}\u{000D}\u{0020}\u{00A0}\u{FEFF}\u{2028}\u{2029}\
     \u{1680}\u{2000}-\u{200A}\u{202F}\u{205F}\u{3000}";

/// Compile `pattern` or panic with the offending pattern (these are
/// compile-time-constant patterns authored here; a failure is a port bug, not
/// a runtime input error — failing loudly at first use is correct).
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|e| panic!("mdrcel regexps: bad pattern {pattern:?}: {e}"))
}

/// `REGEXPS.unlikelyCandidates` (`Readability.js:140-141`, `/…/i`).
pub fn unlikely_candidates() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(
            "(?i)-ad-|ai2html|banner|breadcrumbs|combx|comment|community|cover-wrap|disqus|\
             extra|footer|gdpr|header|legends|menu|related|remark|replies|rss|shoutbox|\
             sidebar|skyscraper|social|sponsor|supplemental|ad-break|agegate|pagination|\
             pager|popup|yom-remote",
        )
    })
}

/// `REGEXPS.okMaybeItsACandidate` (`Readability.js:142`, `/…/i`).
pub fn ok_maybe_its_a_candidate() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("(?i)and|article|body|column|content|main|shadow"))
}

/// `REGEXPS.positive` (`Readability.js:144-145`, `/…/i`).
pub fn positive() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(
            "(?i)article|body|content|entry|hentry|h-entry|main|page|pagination|post|text|\
             blog|story",
        )
    })
}

/// `REGEXPS.negative` (`Readability.js:146-147`, `/…/i`).
///
/// Note the literal-space alternatives ` hid$| hid |^hid ` — these are plain
/// U+0020 spaces in the JS source (a class/id match string is
/// `className + " " + id`), ported verbatim as literal spaces (NOT `\s`).
pub fn negative() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(
            "(?i)-ad-|hidden|^hid$| hid$| hid |^hid |banner|combx|comment|com-|contact|\
             footer|gdpr|masthead|media|meta|outbrain|promo|related|scroll|share|shoutbox|\
             sidebar|skyscraper|sponsor|shopping|tags|widget",
        )
    })
}

/// `REGEXPS.extraneous` (`Readability.js:148-149`, `/…/i`). Declared for
/// completeness of the table; first *used* in Stage 2/3. Ported now so the
/// conformance table is the single Stage-1a oracle for the whole table the
/// HLD §8 asks to freeze.
pub fn extraneous() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // JS: /print|archive|comment|discuss|e[\-]?mail|share|reply|all|login|sign|single|utility/i
        compile("(?i)print|archive|comment|discuss|e[\\-]?mail|share|reply|all|login|sign|single|utility")
    })
}

/// `REGEXPS.byline` (`Readability.js:150`, `/…/i`).
pub fn byline() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("(?i)byline|author|dateline|writtenby|p-author"))
}

/// `REGEXPS.normalize` (`Readability.js:152`, `/\s{2,}/g`) — JS `\s`, so the
/// explicit class. Used by `_getInnerText` / `_getArticleTitle`.
pub fn normalize() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!("[{JS_SPACE_CLASS}]{{2,}}")))
}

/// `REGEXPS.tokenize` (`Readability.js:158`, `/\W+/g`) — JS `\W` is **ASCII**.
///
/// HLD §8 specifies "ASCII `\W` written `(?-u:\W)`". On `&str` searches the
/// `regex` crate **rejects** `(?-u:\W)` ("pattern can match invalid UTF-8" —
/// a byte class on a `&str` matcher). The exactly-equivalent construction
/// that *does* compile on `&str` is the **explicit ECMAScript `\W` class**:
/// ECMA-262 defines `\w` as `[A-Za-z0-9_]`, so `\W` ≡ `[^A-Za-z0-9_]`. This
/// is **identical match semantics** to JS `\W` (and to `(?-u:\W)` on valid
/// UTF-8) — a literal transcription of the ECMA-262 class, the same
/// dialect-faithful "spell the class out" technique HLD §8 already applies to
/// JS `\s` via [`JS_SPACE_CLASS`]. (Recovery correction: the interrupted
/// partial port used the non-compiling `(?-u:\W)+`. The conformance row
/// `tokenize "café" → true` — `é` is ASCII-`\W` — pins that this stays the
/// ASCII, not Unicode, definition.) Used by `_textSimilarity`.
pub fn tokenize() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("[^A-Za-z0-9_]+"))
}

/// `REGEXPS.whitespace` (`Readability.js:159`, `/^\s*$/`) — JS `\s` class.
/// Used by `_nextNode`.
pub fn whitespace() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!("^[{JS_SPACE_CLASS}]*$")))
}

/// `REGEXPS.hasContent` (`Readability.js:160`, `/\S$/`) — JS `\S`, so
/// `[^<JS_SPACE_CLASS>]` anchored at end. Used by `_hasSingleTagInsideElement`.
///
/// Rust `regex` `$` is end-of-text (no `m` flag), and matches before a final
/// `\n` — but a final `\n` *is* a `JS_SPACE_CLASS` char so `[^…]` cannot match
/// it anyway; behaviourally identical to JS `/\S$/` (JS `$` without `/m` is
/// also end-of-input). `.` is not used here so the `\n`-dotall nuance is moot.
pub fn has_content() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!("[^{JS_SPACE_CLASS}]$")))
}

/// `REGEXPS.hashUrl` (`Readability.js:161`, `/^#.+/`). Used by
/// `_getLinkDensity`. JS `.` excludes line terminators; an `href` realistically
/// never contains a raw newline, and Rust `regex` `.` also excludes `\n` by
/// default — identical here.
pub fn hash_url() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("^#.+"))
}

/// `REGEXPS.commas` (`Readability.js:166`, global). The Latin/Sindhi/Chinese/…
/// comma variants. Used by `_grabArticle`'s comma scoring
/// (`innerText.split(REGEXPS.commas).length`).
///
/// JS: `/,|،|﹐|︐|︑|⹁|⸴|⸲|，/g`.
pub fn commas() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile("\u{002C}|\u{060C}|\u{FE50}|\u{FE10}|\u{FE11}|\u{2E41}|\u{2E34}|\u{2E32}|\u{FF0C}")
    })
}

/// `/\.( |$)/` — the inline regex in `_grabArticle`'s sibling-append `<p>`
/// short-content clause (`Readability.js:1477`,
/// `nodeContent.search(/\.( |$)/) !== -1`). NOT a `REGEXPS`-table entry (it is
/// an inline literal at the call site), but it is load-bearing on the scored
/// path (it decides whether a short `<p>` sibling is appended), so it lives
/// here with the rest of the patterns and the §8 conformance table.
///
/// Semantics: a literal `.` immediately followed by **either** a single ASCII
/// U+0020 space **or** end-of-input (`$`). The space is a literal U+0020 in
/// the JS source (NOT `\s`), ported verbatim as a literal space. JS `String
/// .prototype.search` returns the match index or `-1`; the caller's
/// `!== -1` is exactly "this pattern matches somewhere", i.e.
/// [`Regex::is_match`].
///
/// **Dialect note (HLD §8):** JS `$` without `/m` is end-of-input and also
/// matches immediately before a final `\n`; Rust `regex` `$` without `(?m)`
/// behaves identically. The input here is `_getInnerText(sibling)` (already
/// JS-trimmed + `/\s{2,}/`-collapsed), so it never ends in `\n` and the
/// nuance is moot — behaviourally identical to JS `/\.( |$)/`.
pub fn period_space_or_end() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("\\.( |$)"))
}

/// `/\.(jpg|jpeg|png|webp)/i` (`Readability.js:1907`, `:1950`) — the inline
/// image-extension probe used by `_unwrapNoscriptImages` to decide whether
/// an `<img>` attribute *value* looks like an image source (and so the `<img>`
/// is NOT a placeholder to be removed at `:1912`, or the `prevImg` attribute
/// IS worth copying onto the noscript-extracted `newImg` at `:1947-1951`).
///
/// Not a `REGEXPS`-table entry (inline literal at two call sites in the JS
/// function body), but ported here so the §8 conformance table covers every
/// regex on the Stage-2 pre-grab path.
///
/// **Dialect note (HLD §8):** the JS `.` operator is escaped (`\\.`), so this
/// is a literal `.` followed by one of `jpg`/`jpeg`/`png`/`webp`. `/i` is
/// keyword-only (ASCII alternations), so Rust `(?i)` is identical here. The
/// pattern is **unanchored** — it matches anywhere in the attribute value
/// (`"foo.jpg?x=1"` matches; the `?` query-string suffix does not break it).
pub fn image_extension() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("(?i)\\.(jpg|jpeg|png|webp)"))
}

/// `REGEXPS.b64DataUrl` (`Readability.js:163`, `/^data:\s*([^\s;,]+)\s*;\s*base64\s*,/i`).
///
/// Used by `_fixLazyImages` (`:2338-2340`) to (a) detect a base64-encoded
/// data: URI in an `<img>`'s `src` attribute, and (b) capture the mediatype
/// (the first capture group) so the SVG carve-out (`:2341-2343`) can fire.
/// The `parts[0].length` JS expression is the full prefix length up to and
/// including `";base64,"` — used to subtract from the total `src.length` to
/// estimate the base64 payload size (the `<133` placeholder check at
/// `:2365`).
///
/// **Dialect note (HLD §8):** JS `\s` ⇒ JS-space class (`JS_SPACE_CLASS`).
/// The pattern is anchored at `^`, case-insensitive on `data:` / `base64`.
pub fn b64_data_url() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(&format!(
            "(?i)^data:[{JS_SPACE_CLASS}]*([^{JS_SPACE_CLASS};,]+)\
             [{JS_SPACE_CLASS}]*;[{JS_SPACE_CLASS}]*base64[{JS_SPACE_CLASS}]*,"
        ))
    })
}

/// `/\.(jpg|jpeg|png|webp)/i` reused as "any attribute value contains an
/// image extension" — `Readability.js:2354`'s inline literal inside
/// `_fixLazyImages`. **Identical** pattern to [`image_extension`] (also at
/// `:1907`/`:1950`); kept as a separate name to make the call sites
/// readable. (Aliasing the function rather than recompiling the pattern.)
pub fn image_ext_anywhere() -> &'static Regex {
    // Reuses the same compiled regex — `image_extension` is already memoised.
    image_extension()
}

/// `/\.(jpg|jpeg|png|webp)\s+\d/` (`Readability.js:2389`) — inline in
/// `_fixLazyImages`. Detects a srcset-style attribute value: an image
/// extension followed by JS whitespace and a digit (the density / width
/// descriptor, e.g. `"foo.jpg 2x"` or `"foo.jpg 800w"`).
///
/// **Dialect note (HLD §8):** JS `\s` ⇒ JS-space class. JS `\d` is ASCII
/// `[0-9]` — explicit here so we match the JS dialect exactly.
pub fn image_srcset_value() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(&format!(
            "(?i)\\.(jpg|jpeg|png|webp)[{JS_SPACE_CLASS}]+[0-9]"
        ))
    })
}

/// `/^\s*\S+\.(jpg|jpeg|png|webp)\S*\s*$/` (`Readability.js:2391`) — inline
/// in `_fixLazyImages`. Detects a plain image-URL attribute value: optional
/// leading JS-whitespace, one or more non-whitespace chars (the path
/// portion before the extension), the image extension, optional
/// non-whitespace suffix (query string), optional trailing JS-whitespace.
///
/// Anchored `^…$`. **Dialect note (HLD §8):** JS `\s` ⇒ JS-space class; JS
/// `\S` ⇒ `[^<JS-space-class>]`.
pub fn image_src_value() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(&format!(
            "(?i)^[{JS_SPACE_CLASS}]*[^{JS_SPACE_CLASS}]+\\.(jpg|jpeg|png|webp)\
             [^{JS_SPACE_CLASS}]*[{JS_SPACE_CLASS}]*$"
        ))
    })
}

/// `REGEXPS.videos` (`Readability.js:153-154`, `/…/i`). The default
/// `_allowedVideoRegex`. Used by `_clean` for embed allow-listing. Stage 1a's
/// `_clean` targets object/embed/footer/link/aside — `isEmbed` is true for
/// object/embed so this is on the Stage-1a path.
pub fn videos() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        // JS: /\/\/(www\.)?((dailymotion|youtube|youtube-nocookie|player\.vimeo|v\.qq)\.com|(archive|upload\.wikimedia)\.org|player\.twitch\.tv)/i
        compile(
            "(?i)//(www\\.)?((dailymotion|youtube|youtube-nocookie|player\\.vimeo|v\\.qq)\\.com|\
             (archive|upload\\.wikimedia)\\.org|player\\.twitch\\.tv)",
        )
    })
}

/// `REGEXPS.shareElements` (`Readability.js:155`, `/(\b|_)(share|sharedaddy)(\b|_)/i`).
/// Used by `_prepArticle`'s `_cleanMatchedNodes` share-strip — **not** in the
/// Stage-1a near-noop `_prepArticle` slice, but ported now so the §8 table is
/// the single frozen oracle.
pub fn share_elements() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("(?i)(\\b|_)(share|sharedaddy)(\\b|_)"))
}

/// `REGEXPS.adWords` (`Readability.js:171-172`,
/// `/^(ad(vertising|vertisement)?|pub(licité)?|werb(ung)?|广告|Реклама|Anuncio)$/iu`).
///
/// Anchored alternation of "ad words" used by `_cleanConditionally` to detect
/// inner text that is just an ad label (`Readability.js:2540`). The JS pattern
/// carries `/u` to enable Unicode case folding for the non-ASCII alternatives
/// (`广告`, `Реклама`, `Anuncio`); Rust `regex` defaults to Unicode mode (and
/// `(?i)` is Unicode-aware), so it is dialect-faithful for these character
/// classes (no ASCII-only `(?-u:..)` opt-out here). Pinned by the
/// conformance table.
pub fn ad_words() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(
            "(?i)^(ad(vertising|vertisement)?|pub(licit\u{00E9})?|werb(ung)?|\u{5E7F}\u{544A}|\
             \u{0420}\u{0435}\u{043A}\u{043B}\u{0430}\u{043C}\u{0430}|Anuncio)$",
        )
    })
}

/// `REGEXPS.loadingWords` (`Readability.js:173-174`,
/// `/^((loading|正在加载|Загрузка|chargement|cargando)(…|\.\.\.)?)$/iu`).
///
/// Anchored alternation of "loading" words used by `_cleanConditionally`
/// (`Readability.js:2541`). Same `/u`/Unicode-default dialect note as
/// [`ad_words`].
pub fn loading_words() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(
            "(?i)^((loading|\u{6B63}\u{5728}\u{52A0}\u{8F7D}|\u{0417}\u{0430}\u{0433}\u{0440}\
             \u{0443}\u{0437}\u{043A}\u{0430}|chargement|cargando)(\u{2026}|\\.\\.\\.)?)$",
        )
    })
}

// ---------------------------------------------------------------------------
// Inline regexes used by `_getArticleTitle` (`Readability.js:572-651`) and the
// title half of `_getArticleMetadata` (`Readability.js:1757-1816`). These are
// NOT in the `REGEXPS` table but are load-bearing on the Stage-1a title path
// (title feeds `_headerDuplicatesTitle` → scored body, HLD §7.1). Same dialect
// rules (HLD §8): JS `\s` → [`JS_SPACE_CLASS`]; `/i` → `(?i)`; no backrefs.
// ---------------------------------------------------------------------------

/// `wordCount`'s split pattern (`Readability.js:592`, `/\s+/`). JS `\s`, so
/// the explicit class. Used via `Regex::split` to mirror `String.split(/\s+/)`.
pub fn ws_plus() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!("[{JS_SPACE_CLASS}]+")))
}

/// `/ [\|\-\\\/>»] /` (`Readability.js:596` test; `:598` `matchAll` w/ `/gi`).
/// A single space, one of `| - \ / > »`, a single space. Literal U+0020
/// spaces (NOT `\s`). Case-insensitive in the `matchAll` use (`/gi`); the
/// class has no letters so `(?i)` is inert — applied uniformly here so one
/// compiled pattern serves both the `.test` and `.matchAll` call sites
/// (identical match set).
pub fn title_separator() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    // Char class: space | - \ / > » space  → escape \ / ] - inside the class.
    R.get_or_init(|| compile("(?i) [\\|\\-\\\\/>\u{00BB}] "))
}

/// `/ [\\\/>»] /` (`Readability.js:597`) — the *hierarchical* separators only
/// (`\ / > »`), space-delimited. Drives `titleHadHierarchicalSeparators`.
pub fn title_hier_separator() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(" [\\\\/>\u{00BB}] "))
}

/// `/^[^\|\-\\\/>»]*[\|\-\\\/>»]/gi` (`Readability.js:603`). From start: any
/// run of non-separator chars, then one separator — i.e. strip up to and
/// including the first separator. (`/g` ⇒ use `replace` once is enough since
/// it is anchored at `^`; `/i` inert — no letters.)
pub fn title_lead_separator() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("(?i)^[^\\|\\-\\\\/>\u{00BB}]*[\\|\\-\\\\/>\u{00BB}]"))
}

/// `/[\|\-\\\/>»]+/g` (`Readability.js:645`) — runs of separator chars,
/// removed wholesale (used in the final `<=4`-word guard's word recount).
pub fn title_separators_run() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile("[\\|\\-\\\\/>\u{00BB}]+"))
}

/// `propertyPattern` (`Readability.js:1763-1764`):
/// `/\s*(article|dc|dcterm|og|twitter)\s*:\s*(author|creator|description|
/// published_time|title|site_name)\s*/gi`. JS `\s` → explicit class; `/i`
/// keyword-only so `(?i)` is identical on these inputs. `/g` ⇒ `find` (JS uses
/// `String.match` w/o capturing the global list meaningfully — it reads
/// `matches[0]`, the first match).
pub fn meta_property_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(&format!(
            "(?i)[{JS_SPACE_CLASS}]*(article|dc|dcterm|og|twitter)[{JS_SPACE_CLASS}]*:\
             [{JS_SPACE_CLASS}]*(author|creator|description|published_time|title|site_name)\
             [{JS_SPACE_CLASS}]*"
        ))
    })
}

/// `namePattern` (`Readability.js:1767-1768`):
/// `/^\s*(?:(dc|dcterm|og|twitter|parsely|weibo:(article|webpage))\s*[-\.:]\s*)?
/// (author|creator|pub-date|description|title|site_name)\s*$/i`. JS `\s` →
/// explicit class; `/i` keyword-only. Anchored `^…$` (no `/g`); used with
/// `.test`.
pub fn meta_name_pattern() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        compile(&format!(
            "(?i)^[{JS_SPACE_CLASS}]*(?:(dc|dcterm|og|twitter|parsely|weibo:(article|webpage))\
             [{JS_SPACE_CLASS}]*[-\\.:][{JS_SPACE_CLASS}]*)?\
             (author|creator|pub-date|description|title|site_name)[{JS_SPACE_CLASS}]*$"
        ))
    })
}

/// `/\s/g` (`Readability.js:1786`/`1796`) — match a **single** JS-`\s` char
/// (used with `replace(/\s/g,"")` to delete all whitespace).
pub fn js_space_any() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| compile(&format!("[{JS_SPACE_CLASS}]")))
}

// ---------------------------------------------------------------------------
// Constant lists (`Readability.js:177-264`).
// ---------------------------------------------------------------------------

/// `UNLIKELY_ROLES` (`Readability.js:177-185`).
pub const UNLIKELY_ROLES: &[&str] = &[
    "menu",
    "menubar",
    "complementary",
    "navigation",
    "alert",
    "alertdialog",
    "dialog",
];

/// `DIV_TO_P_ELEMS` (`Readability.js:187-197`) — a `Set` in JS; membership is
/// the only operation, so a slice + `.contains` is faithful.
pub const DIV_TO_P_ELEMS: &[&str] = &[
    "BLOCKQUOTE",
    "DL",
    "DIV",
    "IMG",
    "OL",
    "P",
    "PRE",
    "TABLE",
    "UL",
];

/// `ALTER_TO_DIV_EXCEPTIONS` (`Readability.js:199`). Used by the sibling-append
/// pass (Stage 1b) — declared here with the rest of the table.
pub const ALTER_TO_DIV_EXCEPTIONS: &[&str] = &["DIV", "ARTICLE", "SECTION", "P", "OL", "UL"];

/// `PRESENTATIONAL_ATTRIBUTES` (`Readability.js:201-214`). Used by
/// `_cleanStyles` (Stage 2). Declared with the table.
pub const PRESENTATIONAL_ATTRIBUTES: &[&str] = &[
    "align",
    "background",
    "bgcolor",
    "border",
    "cellpadding",
    "cellspacing",
    "frame",
    "hspace",
    "rules",
    "style",
    "valign",
    "vspace",
];

/// `DEPRECATED_SIZE_ATTRIBUTE_ELEMS` (`Readability.js:216`). Stage 2.
pub const DEPRECATED_SIZE_ATTRIBUTE_ELEMS: &[&str] = &["TABLE", "TH", "TD", "HR", "PRE"];

/// `PHRASING_ELEMS` (`Readability.js:220-261`). The commented-out
/// CANVAS/IFRAME/SVG/VIDEO are intentionally **excluded** (the JS comment
/// explains: they qualify as phrasing but Readability removes them from
/// paragraphs, so they are omitted here too — faithful to the JS array).
pub const PHRASING_ELEMS: &[&str] = &[
    "ABBR", "AUDIO", "B", "BDO", "BR", "BUTTON", "CITE", "CODE", "DATA", "DATALIST", "DFN", "EM",
    "EMBED", "I", "IMG", "INPUT", "KBD", "LABEL", "MARK", "MATH", "METER", "NOSCRIPT", "OBJECT",
    "OUTPUT", "PROGRESS", "Q", "RUBY", "SAMP", "SCRIPT", "SELECT", "SMALL", "SPAN", "STRONG",
    "SUB", "SUP", "TEXTAREA", "TIME", "VAR", "WBR",
];

/// `DEFAULT_TAGS_TO_SCORE` (`Readability.js:128-130`):
/// `"section,h2,h3,h4,h5,h6,p,td,pre".toUpperCase().split(",")`.
pub const DEFAULT_TAGS_TO_SCORE: &[&str] =
    &["SECTION", "H2", "H3", "H4", "H5", "H6", "P", "TD", "PRE"];

/// `CLASSES_TO_PRESERVE` (`Readability.js:264`). Used by `_cleanClasses`
/// (Stage 3). Declared with the table.
pub const CLASSES_TO_PRESERVE: &[&str] = &["page"];

#[cfg(test)]
mod tests {
    //! HLD §8 per-regex conformance table — the Stage-1a oracle.
    //!
    //! Each case's expected `bool` is **hand-derived from the JS regex
    //! semantics** (reading `Readability.js:137-175`), NOT by running
    //! Readability (that would be inversion, HLD §4). The cases deliberately
    //! probe the dialect traps the HLD §8 names: JS-`\s` vs Rust-`\s`
    //! (U+FEFF, U+00A0, U+000B), ASCII-`\W` vs Unicode-`\W`, `/i`
    //! case-insensitivity, anchors, and the literal-space alternatives in
    //! `negative`.

    use super::*;

    /// `(pattern_fn, haystack, expected_is_match)` rows.
    type Row = (fn() -> &'static Regex, &'static str, bool);

    #[test]
    fn regexps_conformance_table() {
        let rows: &[Row] = &[
            // --- unlikelyCandidates (/i) ---
            (unlikely_candidates, "main-footer", true),
            (unlikely_candidates, "comment-list", true),
            (unlikely_candidates, "COMMENT", true), // /i
            (unlikely_candidates, "article-body", false),
            (unlikely_candidates, "", false),
            // --- okMaybeItsACandidate (/i) ---
            (ok_maybe_its_a_candidate, "main", true),
            (ok_maybe_its_a_candidate, "ARTICLE", true),
            (ok_maybe_its_a_candidate, "footer", false),
            // --- positive (/i) ---
            (positive, "article", true),
            (positive, "Story", true),
            (positive, "nav", false),
            // --- negative (/i) incl. literal-space alternatives ---
            (negative, "sidebar", true),
            (negative, "comment", true),
            (negative, "hid", true),         // ^hid$
            (negative, "foo hid", true),     // " hid$"
            (negative, "foo hid bar", true), // " hid "
            (negative, "hid bar", true),     // "^hid "
            (negative, "hidden", true),      // "hidden"
            (negative, "hideous", false), // 'hid' only matches the anchored/spaced alts, not substring
            (negative, "main", false),
            // --- byline (/i) ---
            (byline, "byline", true),
            (byline, "p-author", true),
            (byline, "AUTHOR", true),
            (byline, "content", false),
            // --- normalize: JS \s{2,} (the U+FEFF / U+00A0 / U+000B trap) ---
            (normalize, "a  b", true),               // 2 ASCII spaces
            (normalize, "a b", false),               // single space: NOT matched
            (normalize, "a\u{00A0}\u{00A0}b", true), // 2 NBSP
            (normalize, "a\u{FEFF}\u{FEFF}b", true), // 2 ZWNBSP — Rust \s would MISS this
            (normalize, "a\u{000B}\u{000B}b", true), // 2 VT (\v)
            (normalize, "a\u{00A0}b", false),        // single NBSP: not a run of 2
            // --- tokenize: ASCII \W+ ---
            (tokenize, "a b", true),  // space is \W
            (tokenize, "a.b", true),  // '.' is \W
            (tokenize, "abc", false), // all word chars
            (tokenize, "a_b", false), // '_' is a word char in \w
            // ASCII-\W trap: 'é' is a WORD char under Unicode \W (so a bare
            // Rust \W would NOT match it) but a NON-word char under JS ASCII
            // \W (so (?-u:\W) MUST match it). This row is the HLD §8 proof.
            (tokenize, "café", true),
            // --- whitespace: ^\s*$ (JS \s) ---
            (whitespace, "", true),
            (whitespace, "   ", true),
            (whitespace, "\u{FEFF}", true), // ZWNBSP is JS \s
            (whitespace, " \t\n ", true),
            (whitespace, " x ", false),
            // --- hasContent: \S$ (JS \S) ---
            (has_content, "abc", true),          // ends non-space
            (has_content, "abc ", false),        // ends space
            (has_content, "abc\u{FEFF}", false), // ends ZWNBSP (JS \s) -> NOT \S
            (has_content, "", false),
            // --- hashUrl: ^#.+ ---
            (hash_url, "#section", true),
            (hash_url, "#", false),          // needs at least one char after #
            (hash_url, "http://x#y", false), // must be anchored at start
            // --- commas: comma variants ---
            (commas, "a,b", true),        // U+002C
            (commas, "a\u{FF0C}b", true), // fullwidth comma U+FF0C
            (commas, "a\u{060C}b", true), // Arabic comma U+060C
            (commas, "a;b", false),
            // --- image_extension (/i) — Readability.js:1907, :1950 ---
            (image_extension, "photo.jpg", true),
            (image_extension, "photo.JPG", true),    // /i
            (image_extension, "img.jpeg?v=2", true), // unanchored
            (image_extension, "x.png", true),
            (image_extension, "x.webp", true),
            (image_extension, "x.gif", false), // not in alternation
            (image_extension, "jpg", false),   // literal `.` required
            (image_extension, "", false),
            // --- b64_data_url: ^data:\s*([^\s;,]+)\s*;\s*base64\s*, — Readability.js:163 ---
            (b64_data_url, "data:image/png;base64,iVBOR", true),
            (b64_data_url, "data:image/svg+xml;base64,PHN2", true), // svg too
            (b64_data_url, "data:image/png ;base64 ,iVBOR", true),  // \s allowed
            (b64_data_url, "Data:image/png;base64,iVBOR", true),    // /i
            (b64_data_url, "data:image/png;base32,iVBOR", false),   // not base64
            (b64_data_url, "data:;base64,iVBOR", false),            // empty mediatype excluded
            (b64_data_url, "https://x/y", false),
            (b64_data_url, "data:image/png,iVBOR", false), // no `;base64,`
            // --- image_srcset_value: /\.(jpg|jpeg|png|webp)\s+\d/ — Readability.js:2389 ---
            (image_srcset_value, "foo.jpg 2x", true),
            (image_srcset_value, "foo.JPG  800w", true),
            (image_srcset_value, "foo.webp\t1x", true), // \s
            (image_srcset_value, "foo.jpg", false),     // missing srcset descriptor
            (image_srcset_value, "foo.jpg 2", true),    // \d (any digit)
            (image_srcset_value, "foo.gif 2x", false),  // not in alternation
            // --- image_src_value: /^\s*\S+\.(jpg|jpeg|png|webp)\S*\s*$/ — Readability.js:2391 ---
            (image_src_value, "foo.jpg", true),
            (image_src_value, "  foo.jpg  ", true), // \s* leading + trailing
            (image_src_value, "/path/to/x.png?v=1", true), // \S* suffix (query string)
            (image_src_value, "image_srcset_value", false), // no extension
            (image_src_value, "a.jpg b.jpg", false), // contains a space → \S+ doesn't span
            (image_src_value, "", false),
            // --- videos (/i) ---
            (videos, "https://www.youtube.com/watch?v=x", true),
            (videos, "//player.vimeo.com/video/1", true),
            (videos, "https://example.com/v", false),
            // --- shareElements (/i) ---
            (share_elements, "share", true),          // \b boundaries
            (share_elements, "post_share_box", true), // _ delimiters
            (share_elements, "sharedaddy", true),
            (share_elements, "shared", false), // 'share' not at \b|_ boundary on the right
            // --- extraneous (/i) ---
            (extraneous, "e-mail", true),
            (extraneous, "email", true), // e[\-]?mail -> '-' optional
            (extraneous, "archive", true),
            (extraneous, "BodyText", false),
            // --- period_space_or_end: /\.( |$)/ (Stage-1b <p> clause) ---
            (period_space_or_end, "end.", true), // '.' then end-of-input ($)
            (period_space_or_end, "a. b", true), // '.' then a literal U+0020
            (period_space_or_end, "Mr. Smith ran.", true), // matches first ". "
            (period_space_or_end, "no period here", false),
            (period_space_or_end, "ends with dot then more.x", false), // '.' then 'x' (not space/end)
            (period_space_or_end, "a.b. ", true), // second '.' is followed by ' '
            (period_space_or_end, "", false),
            (period_space_or_end, ".", true), // '.' immediately at end
            // --- ad_words (/iu) ---
            (ad_words, "ad", true),
            (ad_words, "advertising", true),
            (ad_words, "advertisement", true),
            (ad_words, "AD", true), // /i
            (ad_words, "Anuncio", true),
            (ad_words, "\u{5E7F}\u{544A}", true),   // 广告
            (ad_words, "advertising stuff", false), // anchored ^..$
            (ad_words, "ads", false),               // not in alternation
            // --- loading_words (/iu) ---
            (loading_words, "loading", true),
            (loading_words, "Loading", true),         // /i
            (loading_words, "loading...", true),      // optional "..." or "…"
            (loading_words, "loading\u{2026}", true), // ellipsis char
            (loading_words, "cargando", true),
            (loading_words, "loading bar", false), // anchored
        ];

        for (i, (f, hay, expect)) in rows.iter().enumerate() {
            let got = f().is_match(hay);
            assert_eq!(
                got,
                *expect,
                "row {i}: pattern {:?} on input {hay:?}: expected is_match={expect}, got {got}",
                f().as_str()
            );
        }
    }

    #[test]
    fn js_space_class_membership_matches_dom_is_js_space_set() {
        // FIX-2 single-source-of-truth pin: `JS_SPACE_CLASS` (the regex
        // character-class literal — a fn cannot be spliced into a pattern, so
        // it must stay a literal) is mechanically pinned EQUAL to the
        // canonical `dom::is_js_space` predicate over the **full relevant
        // codepoint set**. `dom::is_js_space` is the one definition every
        // JS-`\s` site is verified against (`metadata::js_trim` now calls it
        // directly; `dom`'s own trim/normalise use it); this exhaustive sweep
        // means ANY future drift in EITHER the literal OR the predicate fails
        // the build. The set is entirely within the BMP, so U+0000..=U+FFFF is
        // exhaustive; a few astral boundary points are added for completeness.
        use crate::readability::dom::is_js_space;
        let re = compile(&format!("^[{JS_SPACE_CLASS}]$"));
        let check = |c: char| {
            assert_eq!(
                re.is_match(&c.to_string()),
                is_js_space(c),
                "JS-`\\s` divergence at U+{:04X}: JS_SPACE_CLASS regex \
                 says {}, canonical dom::is_js_space says {} — the single \
                 source of truth and the regex literal have drifted apart \
                 (FIX-2 pin).",
                c as u32,
                re.is_match(&c.to_string()),
                is_js_space(c),
            );
        };
        for cp in 0u32..=0xFFFF {
            if let Some(c) = char::from_u32(cp) {
                check(c);
            }
        }
        // Astral boundary points (none are JS `\s`; both sides must agree).
        for &cp in &[0x1_0000u32, 0x1_F600, 0x10_FFFF] {
            check(char::from_u32(cp).unwrap());
        }
    }
}
