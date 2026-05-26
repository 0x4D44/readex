//! `xpaths_constants` — verbatim Rust vendoring of `trafilatura/xpaths.py`
//! (HLD M3 §7, Stage 2a).
//!
//! Source of truth: `trafilatura@v2.0.0/xpaths.py` (267 lines). Every constant
//! here is byte-equivalent to a Python `XPath(...)` literal at the cited line
//! range. The Python file wraps each string in `lxml.etree.XPath(x)`; in our
//! port we store the **raw XPath source string** and let
//! `crate::trafilatura::xpath_engine` parse + evaluate it on demand. The wrapper
//! is a Python-side compile-cache concern, not part of the algorithm spec.
//!
//! # Anti-inversion (HLD §10)
//!
//! Each `&[&str]` entry is a Rust raw-string literal copy of the
//! corresponding Python triple-/single-/double-quoted string at the cited
//! `xpaths.py` line range — same characters, same whitespace, same single-
//! vs double-quote convention inside the string. NO XPath was rewritten to
//! dodge an engine gap; the Stage 2a engine-gap survey
//! (`tests/xpath_constants_engine_coverage.rs`) tracks which XPaths the Stage
//! 0b engine accepts verbatim vs which require Stage 2b engine extension.
//!
//! # Shape conventions
//!
//! - Python `[XPath(x) for x in (...)]` -> Rust `pub static FOO: &[&str]`
//!   containing each tuple element as a `&str`.
//! - Python `[XPath("""...""")]` (a one-element list) -> Rust `pub static FOO:
//!   &[&str]` with a single entry (we keep the list shape so callers can
//!   `for x in FOO` uniformly, matching Trafilatura's own usage e.g.
//!   `trafilatura/main_extractor.py` iterates these lists).
//! - Single-line XPaths use `r#"..."#` raw strings; multi-line XPaths use a
//!   raw string spanning the original Python source lines verbatim.

// ---------------------------------------------------------------------------
// 1. CONTENT XPaths (xpaths.py §1)
// ---------------------------------------------------------------------------

/// `BODY_XPATH` (xpaths.py:13-54).
///
/// Five XPaths that locate the article body. Order is load-bearing — the
/// extractor walks them in declaration order and takes the first match
/// (Trafilatura `main_extractor.py` iterates `for expr in BODY_XPATH`).
///
/// Inline source notes:
/// - Entry 0 (xpaths.py:14-31): the canonical "class/id contains post / entry /
///   article-body / page-content / ..." disjunction with the `[1]` positional
///   first-match suffix.
/// - The `# (…)[1] = first occurrence` comment at xpaths.py:32 documents the
///   `[1]` postfix as a positional predicate (not a wrapper).
/// - Entry 1 (xpaths.py:33): trivial `(.//article)[1]`.
/// - Entry 2 (xpaths.py:34-46): the "story-content / theme-content / blog-
///   content" disjunction.
/// - Entry 3 (xpaths.py:47-52): the "content-main / content-body /
///   page-content" disjunction.
/// - Entry 4 (xpaths.py:53): the "starts-with main" / "main element" union.
pub static BODY_XPATH: &[&str] = &[
    // xpaths.py:14-31
    r#".//*[self::article or self::div or self::main or self::section][
    @class="post" or @class="entry" or
    contains(@class, "post-text") or contains(@class, "post_text") or
    contains(@class, "post-body") or contains(@class, "post-entry") or contains(@class, "postentry") or
    contains(@class, "post-content") or contains(@class, "post_content") or
    contains(@class, "postcontent") or contains(@class, "postContent") or contains(@class, "post_inner_wrapper") or
    contains(@class, "article-text") or contains(@class, "articletext") or contains(@class, "articleText")
    or contains(@id, "entry-content") or
    contains(@class, "entry-content") or contains(@id, "article-content") or
    contains(@class, "article-content") or contains(@id, "article__content") or
    contains(@class, "article__content") or contains(@id, "article-body") or
    contains(@class, "article-body") or contains(@id, "article__body") or
    contains(@class, "article__body") or @itemprop="articleBody" or
    contains(translate(@id, "B", "b"), "articlebody") or contains(translate(@class, "B", "b"), "articlebody")
    or @id="articleContent" or contains(@class, "ArticleContent") or
    contains(@class, "page-content") or contains(@class, "text-content") or
    contains(@id, "body-text") or contains(@class, "body-text") or
    contains(@class, "article__container") or contains(@id, "art-content") or contains(@class, "art-content")][1]"#,
    // xpaths.py:32 source comment: `# (…)[1] = first occurrence`
    // xpaths.py:33
    r#"(.//article)[1]"#,
    // xpaths.py:34-46
    r#"(.//*[self::article or self::div or self::main or self::section][
    contains(@class, 'post-bodycopy') or
    contains(@class, 'storycontent') or contains(@class, 'story-content') or
    @class='postarea' or @class='art-postcontent' or
    contains(@class, 'theme-content') or contains(@class, 'blog-content') or
    contains(@class, 'section-content') or contains(@class, 'single-content') or
    contains(@class, 'single-post') or
    contains(@class, 'main-column') or contains(@class, 'wpb_text_column') or
    starts-with(@id, 'primary') or starts-with(@class, 'article ') or @class="text" or
    @id="article" or @class="cell" or @id="story" or @class="story" or
    contains(@class, "story-body") or contains(@id, "story-body") or contains(@class, "field-body") or
    contains(translate(@class, "FULTEX","fultex"), "fulltext")
    or @role='article'])[1]"#,
    // xpaths.py:47-52
    r#"(.//*[self::article or self::div or self::main or self::section][
    contains(@id, "content-main") or contains(@class, "content-main") or contains(@class, "content_main") or
    contains(@id, "content-body") or contains(@class, "content-body") or contains(@id, "contentBody")
    or contains(@class, "content__body") or contains(translate(@id, "CM","cm"), "main-content") or contains(translate(@class, "CM","cm"), "main-content")
    or contains(translate(@class, "CP","cp"), "page-content") or
    @id="content" or @class="content"])[1]"#,
    // xpaths.py:53
    r#"(.//*[self::article or self::div or self::section][starts-with(@class, "main") or starts-with(@id, "main") or starts-with(@role, "main")])[1]|(.//main)[1]"#,
];
// xpaths.py:55-63 trailing source comments (recorded for anti-inversion
// audit — these are tags Trafilatura considered but did NOT include):
//   # starts-with(@id, "article") or
//   # or starts-with(@id, "story") or contains(@class, "story")
//   # starts-with(@class, "content ") or contains(@class, " content")
//   # '//div[contains(@class, "text") or contains(@class, "article-wrapper") or contains(@class, "content-wrapper")]',
//   # '//div[contains(@class, "article-wrapper") or contains(@class, "content-wrapper")]',
//   # |//*[self::article or self::div or self::main or self::section][contains(@class, "article") or contains(@class, "Article")]
//   # @id="content"or @class="content" or @class="Content"
//   # or starts-with(@class, 'post ')
//   # './/span[@class=""]', # instagram?

/// `COMMENTS_XPATH` (xpaths.py:66-78).
///
/// Four XPaths locating user-comment sections. Iterated in declaration order
/// by the comments-extraction pass.
pub static COMMENTS_XPATH: &[&str] = &[
    // xpaths.py:67-70
    r#".//*[self::div or self::list or self::section][contains(@id|@class, 'commentlist')
    or contains(@class, 'comment-page') or
    contains(@id|@class, 'comment-list') or
    contains(@class, 'comments-content') or contains(@class, 'post-comments')]"#,
    // xpaths.py:71-74
    r#".//*[self::div or self::section or self::list][starts-with(@id|@class, 'comments')
    or starts-with(@class, 'Comments') or
    starts-with(@id|@class, 'comment-') or
    contains(@class, 'article-comments')]"#,
    // xpaths.py:75-76
    r#".//*[self::div or self::section or self::list][starts-with(@id, 'comol') or
    starts-with(@id, 'disqus_thread') or starts-with(@id, 'dsq-comments')]"#,
    // xpaths.py:77
    r#".//*[self::div or self::section][starts-with(@id, 'social') or contains(@class, 'comment')]"#,
];
// xpaths.py:79 trailing comment: `# or contains(@class, 'Comments')`

/// `REMOVE_COMMENTS_XPATH` (xpaths.py:82-90).
///
/// Single-XPath list (Python wraps the single literal in `[XPath(...)]`).
/// Stored as `&[&str]` of length 1 to preserve the iterable-callsite shape.
pub static REMOVE_COMMENTS_XPATH: &[&str] = &[
    // xpaths.py:83-89
    r#".//*[self::div or self::list or self::section][
    starts-with(translate(@id, "C","c"), 'comment') or
    starts-with(translate(@class, "C","c"), 'comment') or
    contains(@class, 'article-comments') or contains(@class, 'post-comments')
    or starts-with(@id, 'comol') or starts-with(@id, 'disqus_thread')
    or starts-with(@id, 'dsq-comments')
    ]"#,
];
// xpaths.py:91-92 trailing comments (considered but NOT in the runtime list):
//   # or self::span
//   # or contains(@class, 'comment') or contains(@id, 'comment')

/// `OVERALL_DISCARD_XPATH` (xpaths.py:95-156).
///
/// Two giant XPaths that strip navigation / footers / shares / cookies /
/// newsletter widgets / bylines / hidden parts. Order load-bearing (entry 0 is
/// the structural-chrome strip; entry 1 is the comment-debris/hidden strip).
pub static OVERALL_DISCARD_XPATH: &[&str] = &[
    // xpaths.py:96-97 source comment: `# navigation + footers, news outlets
    // related posts, sharing, jp-post-flair jp-relatedposts` and `# paywalls`.
    // xpaths.py:98-145
    r#".//*[self::div or self::item or self::list
            or self::p or self::section or self::span][
    contains(translate(@id, "F","f"), "footer") or contains(translate(@class, "F","f"), "footer")
    or contains(@id, "related") or contains(@class, "elated") or
    contains(@id|@class, "viral") or
    starts-with(@id|@class, "shar") or
    contains(@class, "share-") or
    contains(translate(@id, "S", "s"), "share") or
    contains(@id|@class, "social") or contains(@class, "sociable") or
    contains(@id|@class, "syndication") or
    starts-with(@id, "jp-") or starts-with(@id, "dpsp-content") or
    contains(@class, "embedded") or contains(@class, "embed") or
    contains(@id|@class, "newsletter") or
    contains(@class, "subnav") or
    contains(@id|@class, "cookie") or
    contains(@id|@class, "tags") or contains(@class, "tag-list") or
    contains(@id|@class, "sidebar") or
    contains(@id|@class, "banner") or contains(@class, "bar") or
    contains(@class, "meta") or contains(@id, "menu") or contains(@class, "menu") or
    contains(translate(@id, "N", "n"), "nav") or contains(translate(@role, "N", "n"), "nav")
    or starts-with(@class, "nav") or contains(@class, "avigation") or
    contains(@class, "navbar") or contains(@class, "navbox") or starts-with(@class, "post-nav")
    or contains(@id|@class, "breadcrumb") or
    contains(@id|@class, "bread-crumb") or
    contains(@id|@class, "author") or
    contains(@id|@class, "button")
    or contains(translate(@class, "B", "b"), "byline")
    or contains(@class, "rating") or contains(@class, "widget") or
    contains(@class, "attachment") or contains(@class, "timestamp") or
    contains(@class, "user-info") or contains(@class, "user-profile") or
    contains(@class, "-ad-") or contains(@class, "-icon")
    or contains(@class, "article-infos") or
    contains(@class, "nfoline")
    or contains(@data-component, "MostPopularStories")
    or contains(@class, "outbrain") or contains(@class, "taboola")
    or contains(@class, "criteo") or contains(@class, "options") or contains(@class, "expand")
    or contains(@class, "consent") or contains(@class, "modal-content")
    or contains(@class, " ad ") or contains(@class, "permission")
    or contains(@class, "next-") or contains(@class, "-stories")
    or contains(@class, "most-popular") or contains(@class, "mol-factbox")
    or starts-with(@class, "ZendeskForm") or contains(@id|@class, "message-container")
    or contains(@class, "yin") or contains(@class, "zlylin")
    or contains(@class, "xg1") or contains(@id, "bmdh")
    or contains(@class, "slide") or contains(@class, "viewport")
    or @data-lp-replacement-content
    or contains(@id, "premium") or contains(@class, "overlay")
    or contains(@class, "paid-content") or contains(@class, "paidcontent")
    or contains(@class, "obfuscated") or contains(@class, "blurred")]"#,
    // xpaths.py:147 source comment: `# comment debris + hidden parts`
    // xpaths.py:148-155
    r#".//*[@class="comments-title" or contains(@class, "comments-title") or
    contains(@class, "nocomments") or starts-with(@id|@class, "reply-") or
    contains(@class, "-reply-") or contains(@class, "message") or contains(@id, "reader-comments")
    or contains(@id, "akismet") or contains(@class, "akismet") or contains(@class, "suggest-links") or
    starts-with(@class, "hide-") or contains(@class, "-hide-") or contains(@class, "hide-print") or
    contains(@id|@style, "hidden") or contains(@class, " hidden") or contains(@class, " hide")
    or contains(@class, "noprint") or contains(@style, "display:none") or contains(@style, "display: none")
    or @aria-hidden="true" or contains(@class, "notloaded")]"#,
];
// xpaths.py:157-165 trailing comments (recorded for anti-inversion audit —
// patterns Trafilatura considered but rejected):
//   # conflicts:
//   # contains(@id, "header") or contains(@class, "header") or
//   # class contains "cats" (categories, also tags?)
//   # or contains(@class, "hidden ")  or contains(@class, "-hide")
//   # or contains(@class, "paywall")
//   # contains(@class, "content-info") or contains(@class, "content-title")
//   # contains(translate(@class, "N", "n"), "nav") or
//   # contains(@class, "panel") or
//   # or starts-with(@id, "comment-")

/// `TEASER_DISCARD_XPATH` (xpaths.py:169-174).
///
/// Single-element list — drops elements whose id or class contains "teaser"
/// (case-insensitive via `translate`). Used in extraction-precision mode.
pub static TEASER_DISCARD_XPATH: &[&str] = &[
    // xpaths.py:170-173
    r#".//*[self::div or self::item or self::list
             or self::p or self::section or self::span][
        contains(translate(@id, "T", "t"), "teaser") or contains(translate(@class, "T", "t"), "teaser")
    ]"#,
];

/// `PRECISION_DISCARD_XPATH` (xpaths.py:177-185).
///
/// Two XPaths for the precision-mode extra strip: all `<header>` elements,
/// then containers whose id/class/style contains "bottom" / "link" / "border".
pub static PRECISION_DISCARD_XPATH: &[&str] = &[
    // xpaths.py:178
    r#".//header"#,
    // xpaths.py:179-184
    r#".//*[self::div or self::item or self::list
             or self::p or self::section or self::span][
        contains(@id|@class, "bottom") or
        contains(@id|@class, "link") or
        contains(@style, "border")
    ]"#,
];
// xpaths.py:186 trailing comment:
//   # or contains(@id, "-comments") or contains(@class, "-comments")

/// `DISCARD_IMAGE_ELEMENTS` (xpaths.py:189-195).
///
/// Single-element list — drops image-caption containers when image
/// extraction is disabled.
pub static DISCARD_IMAGE_ELEMENTS: &[&str] = &[
    // xpaths.py:190-194
    r#".//*[self::div or self::item or self::list
             or self::p or self::section or self::span][
             contains(@id, "caption") or contains(@class, "caption")
            ]
    "#,
];

/// `COMMENTS_DISCARD_XPATH` (xpaths.py:198-206).
///
/// Three XPaths applied to the comments subtree to drop reply forms, cited
/// quotes, akismet wrappers etc.
pub static COMMENTS_DISCARD_XPATH: &[&str] = &[
    // xpaths.py:199
    r#".//*[self::div or self::section][starts-with(@id, "respond")]"#,
    // xpaths.py:200
    r#".//cite|.//quote"#,
    // xpaths.py:201-205
    r#".//*[@class="comments-title" or contains(@class, "comments-title") or
    contains(@class, "nocomments") or starts-with(@id|@class, "reply-") or
    contains(@class, "-reply-") or contains(@class, "message")
    or contains(@class, "signin") or
    contains(@id|@class, "akismet") or contains(@style, "display:none")]"#,
];

// ---------------------------------------------------------------------------
// 2. METADATA XPaths (xpaths.py §2, lines 210-265)
// ---------------------------------------------------------------------------

/// `AUTHOR_XPATHS` (xpaths.py:214-221).
///
/// Three XPaths from specific -> generic -> last-resort that locate the
/// author element. Iterated in declaration order; the first hit wins.
pub static AUTHOR_XPATHS: &[&str] = &[
    // xpaths.py:215 source comment: `# specific and almost specific`
    // xpaths.py:216
    r#"//*[self::a or self::address or self::div or self::link or self::p or self::span or self::strong][@rel="author" or @id="author" or @class="author" or @itemprop="author name" or rel="me" or contains(@class, "author-name") or contains(@class, "AuthorName") or contains(@class, "authorName") or contains(@class, "author name") or @data-testid="AuthorCard" or @data-testid="AuthorURL"]|//author"#,
    // xpaths.py:217 source comment: `# almost generic and generic, last ones not common`
    // xpaths.py:218
    r#"//*[self::a or self::div or self::h3 or self::h4 or self::p or self::span][contains(@class, "author") or contains(@id, "author") or contains(@itemprop, "author") or @class="byline" or contains(@class, "channel-name") or contains(@id, "zuozhe") or contains(@class, "zuozhe") or contains(@id, "bianji") or contains(@class, "bianji") or contains(@id, "xiaobian") or contains(@class, "xiaobian") or contains(@class, "submitted-by") or contains(@class, "posted-by") or @class="username" or @class="byl" or @class="BBL" or contains(@class, "journalist-name")]"#,
    // xpaths.py:219 source comment: `# last resort: any element`
    // xpaths.py:220
    r#"//*[contains(translate(@id, "A", "a"), "author") or contains(translate(@class, "A", "a"), "author") or contains(@class, "screenname") or contains(@data-component, "Byline") or contains(@itemprop, "author") or contains(@class, "writer") or contains(translate(@class, "B", "b"), "byline")]"#,
];

/// `AUTHOR_DISCARD_XPATHS` (xpaths.py:224-232).
///
/// Two XPaths that strip author-candidates known to be bad (comment lists,
/// hidden blocks, embedded social cards, time/figure elements).
pub static AUTHOR_DISCARD_XPATHS: &[&str] = &[
    // xpaths.py:225-230
    r#".//*[self::a or self::div or self::section or self::span][@id='comments' or @class='comments' or @class='title' or @class='date' or
    contains(@id, 'commentlist') or contains(@class, 'commentlist') or contains(@class, 'sidebar') or contains(@class, 'is-hidden') or contains(@class, 'quote')
    or contains(@id, 'comment-list') or contains(@class, 'comments-list') or contains(@class, 'embedly-instagram') or contains(@id, 'ProductReviews') or
    starts-with(@id, 'comments') or contains(@data-component, "Figure") or contains(@class, "article-share") or contains(@class, "article-support") or contains(@class, "print") or contains(@class, "category") or contains(@class, "meta-date") or contains(@class, "meta-reviewer")
    or starts-with(@class, 'comments') or starts-with(@class, 'Comments')
    ]"#,
    // xpaths.py:231
    r#"//time|//figure"#,
];

/// `CATEGORIES_XPATHS` (xpaths.py:235-245).
///
/// Six XPaths locating category-link containers (`//a[@href]` inside
/// post-info, postmeta, entry-meta, footer, header, row/tags wrappers).
pub static CATEGORIES_XPATHS: &[&str] = &[
    // xpaths.py:236-239
    r#"//div[starts-with(@class, 'post-info') or starts-with(@class, 'postinfo') or
    starts-with(@class, 'post-meta') or starts-with(@class, 'postmeta') or
    starts-with(@class, 'meta') or starts-with(@class, 'entry-meta') or starts-with(@class, 'entry-info') or
    starts-with(@class, 'entry-utility') or starts-with(@id, 'postpath')]//a[@href]"#,
    // xpaths.py:240
    r#"//p[starts-with(@class, 'postmeta') or starts-with(@class, 'entry-categories') or @class='postinfo' or @id='filedunder']//a[@href]"#,
    // xpaths.py:241
    r#"//footer[starts-with(@class, 'entry-meta') or starts-with(@class, 'entry-footer')]//a[@href]"#,
    // xpaths.py:242
    r#"//*[self::li or self::span][@class="post-category" or @class="postcategory" or @class="entry-category" or contains(@class, "cat-links")]//a[@href]"#,
    // xpaths.py:243
    r#"//header[@class="entry-header"]//a[@href]"#,
    // xpaths.py:244
    r#"//div[@class="row" or @class="tags"]//a[@href]"#,
];
// xpaths.py:246 trailing comment:
//   # "//*[self::div or self::p][contains(@class, 'byline')]",

/// `TAGS_XPATHS` (xpaths.py:249-256).
///
/// Four XPaths locating tag-link containers.
pub static TAGS_XPATHS: &[&str] = &[
    // xpaths.py:250
    r#"//div[@class="tags"]//a[@href]"#,
    // xpaths.py:251
    r#"//p[starts-with(@class, 'entry-tags')]//a[@href]"#,
    // xpaths.py:252-254
    r#"//div[@class="row" or @class="jp-relatedposts" or
    @class="entry-utility" or starts-with(@class, 'tag') or
    starts-with(@class, 'postmeta') or starts-with(@class, 'meta')]//a[@href]"#,
    // xpaths.py:255
    r#"//*[@class="entry-meta" or contains(@class, "topics") or contains(@class, "tags-links")]//a[@href]"#,
];
// xpaths.py:257-258 trailing comments:
//   # "related-topics"
//   # https://github.com/grangier/python-goose/blob/develop/goose/extractors/tags.py

/// `TITLE_XPATHS` (xpaths.py:261-265).
///
/// Three XPaths for title extraction: h1/h2 with `post-title`/`entry-title`
/// class, then `@class="entry-title"` exact, then generic h1/h2/h3 with
/// `title` in class or id.
pub static TITLE_XPATHS: &[&str] = &[
    // xpaths.py:262
    r#"//*[self::h1 or self::h2][contains(@class, "post-title") or contains(@class, "entry-title") or contains(@class, "headline") or contains(@id, "headline") or contains(@itemprop, "headline") or contains(@class, "post__title") or contains(@class, "article-title")]"#,
    // xpaths.py:263
    r#"//*[@class="entry-title" or @class="post-title"]"#,
    // xpaths.py:264
    r#"//*[self::h1 or self::h2 or self::h3][contains(@class, "title") or contains(@id, "title")]"#,
];
// xpaths.py:266-267 trailing comments:
//   # json-ld headline
//   # '//header/h1',

// ---------------------------------------------------------------------------
// Aggregate accessor (convenience for the engine gap survey)
// ---------------------------------------------------------------------------

/// One labelled `(constant_name, source_line_range, expressions)` tuple per
/// vendored constant. The Stage 2a engine-gap survey integration test iterates
/// this list to attempt parsing every vendored XPath through the Stage 0b
/// engine and report which ones the engine rejects.
///
/// The line ranges are the Python-source ranges (xpaths.py LL-MM) — they are
/// informational only (used in the survey's diagnostic output).
pub static ALL_XPATHS: &[(&str, &str, &[&str])] = &[
    ("BODY_XPATH", "xpaths.py:13-54", BODY_XPATH),
    ("COMMENTS_XPATH", "xpaths.py:66-78", COMMENTS_XPATH),
    (
        "REMOVE_COMMENTS_XPATH",
        "xpaths.py:82-90",
        REMOVE_COMMENTS_XPATH,
    ),
    (
        "OVERALL_DISCARD_XPATH",
        "xpaths.py:95-156",
        OVERALL_DISCARD_XPATH,
    ),
    (
        "TEASER_DISCARD_XPATH",
        "xpaths.py:169-174",
        TEASER_DISCARD_XPATH,
    ),
    (
        "PRECISION_DISCARD_XPATH",
        "xpaths.py:177-185",
        PRECISION_DISCARD_XPATH,
    ),
    (
        "DISCARD_IMAGE_ELEMENTS",
        "xpaths.py:189-195",
        DISCARD_IMAGE_ELEMENTS,
    ),
    (
        "COMMENTS_DISCARD_XPATH",
        "xpaths.py:198-206",
        COMMENTS_DISCARD_XPATH,
    ),
    ("AUTHOR_XPATHS", "xpaths.py:214-221", AUTHOR_XPATHS),
    (
        "AUTHOR_DISCARD_XPATHS",
        "xpaths.py:224-232",
        AUTHOR_DISCARD_XPATHS,
    ),
    ("CATEGORIES_XPATHS", "xpaths.py:235-245", CATEGORIES_XPATHS),
    ("TAGS_XPATHS", "xpaths.py:249-256", TAGS_XPATHS),
    ("TITLE_XPATHS", "xpaths.py:261-265", TITLE_XPATHS),
];

// ---------------------------------------------------------------------------
// Tests — byte-equivalence + cardinality vs the Python source.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    // ---- Cardinalities (anti-inversion tripwires) -------------------------
    //
    // Each `assert_eq!` below is hand-derived from a count of the tuple
    // elements at the cited Python source range. Any future edit to xpaths.py
    // that adds/removes an entry must be reflected here.

    #[test]
    fn body_xpath_has_5_entries() {
        // xpaths.py:13-54 contains five tuple elements (lines 14-31, 33,
        // 34-46, 47-52, 53).
        assert_eq!(BODY_XPATH.len(), 5);
    }

    #[test]
    fn comments_xpath_has_4_entries() {
        // xpaths.py:66-78 contains four tuple elements (67-70, 71-74, 75-76, 77).
        assert_eq!(COMMENTS_XPATH.len(), 4);
    }

    #[test]
    fn remove_comments_xpath_has_1_entry() {
        // xpaths.py:82-90 contains one XPath wrapped in `[XPath(...)]`.
        assert_eq!(REMOVE_COMMENTS_XPATH.len(), 1);
    }

    #[test]
    fn overall_discard_xpath_has_2_entries() {
        // xpaths.py:95-156 contains two tuple elements (98-145, 148-155).
        assert_eq!(OVERALL_DISCARD_XPATH.len(), 2);
    }

    #[test]
    fn teaser_discard_xpath_has_1_entry() {
        // xpaths.py:169-174.
        assert_eq!(TEASER_DISCARD_XPATH.len(), 1);
    }

    #[test]
    fn precision_discard_xpath_has_2_entries() {
        // xpaths.py:177-185: .//header and the big disjunction.
        assert_eq!(PRECISION_DISCARD_XPATH.len(), 2);
    }

    #[test]
    fn discard_image_elements_has_1_entry() {
        // xpaths.py:189-195.
        assert_eq!(DISCARD_IMAGE_ELEMENTS.len(), 1);
    }

    #[test]
    fn comments_discard_xpath_has_3_entries() {
        // xpaths.py:198-206: respond, cite|quote, comments-title disjunction.
        assert_eq!(COMMENTS_DISCARD_XPATH.len(), 3);
    }

    #[test]
    fn author_xpaths_has_3_entries() {
        // xpaths.py:214-221.
        assert_eq!(AUTHOR_XPATHS.len(), 3);
    }

    #[test]
    fn author_discard_xpaths_has_2_entries() {
        // xpaths.py:224-232.
        assert_eq!(AUTHOR_DISCARD_XPATHS.len(), 2);
    }

    #[test]
    fn categories_xpaths_has_6_entries() {
        // xpaths.py:235-245.
        assert_eq!(CATEGORIES_XPATHS.len(), 6);
    }

    #[test]
    fn tags_xpaths_has_4_entries() {
        // xpaths.py:249-256.
        assert_eq!(TAGS_XPATHS.len(), 4);
    }

    #[test]
    fn title_xpaths_has_3_entries() {
        // xpaths.py:261-265.
        assert_eq!(TITLE_XPATHS.len(), 3);
    }

    #[test]
    fn all_xpaths_total_count() {
        // 5+4+1+2+1+2+1+3+3+2+6+4+3 = 37 vendored XPath expressions.
        let total: usize = ALL_XPATHS.iter().map(|(_, _, v)| v.len()).sum();
        assert_eq!(total, 37);
        // And the labelled-list cardinality matches the per-constant set.
        assert_eq!(ALL_XPATHS.len(), 13);
    }

    // ---- Byte-equivalence spot-checks ------------------------------------
    //
    // For each vendored constant, verify the first entry's first ~30
    // characters byte-for-byte against the Python source (the load-bearing
    // discipline — see HLD §10 anti-inversion). The full byte-for-byte
    // verification is the human-eyeball pass at vendoring time + the
    // diff against xpaths.py in code review; these tests are tripwires
    // against accidental edits.

    #[test]
    fn body_xpath_first_prefix() {
        // xpaths.py:14 begins: `.//*[self::article or self::div or self::main`
        assert!(BODY_XPATH[0].starts_with(".//*[self::article or self::div or self::main"));
    }

    #[test]
    fn body_xpath_second_is_first_article() {
        // xpaths.py:33: '(.//article)[1]'
        assert_eq!(BODY_XPATH[1], "(.//article)[1]");
    }

    #[test]
    fn body_xpath_last_is_starts_with_main_union() {
        // xpaths.py:53 (single-line):
        let last = BODY_XPATH[4];
        assert!(last.starts_with("(.//*[self::article or self::div or self::section]"));
        assert!(last.ends_with("|(.//main)[1]"));
    }

    #[test]
    fn comments_xpath_first_prefix() {
        // xpaths.py:67: `.//*[self::div or self::list or self::section][contains(@id|@class, 'commentlist')`
        assert!(COMMENTS_XPATH[0].starts_with(
            ".//*[self::div or self::list or self::section][contains(@id|@class, 'commentlist')"
        ));
    }

    #[test]
    fn remove_comments_xpath_prefix() {
        // xpaths.py:83.
        assert!(
            REMOVE_COMMENTS_XPATH[0].starts_with(
                ".//*[self::div or self::list or self::section][\n    starts-with(translate(@id, \"C\",\"c\"), 'comment')"
            )
        );
    }

    #[test]
    fn overall_discard_xpath_first_prefix() {
        // xpaths.py:98.
        assert!(
            OVERALL_DISCARD_XPATH[0].starts_with(
                ".//*[self::div or self::item or self::list\n            or self::p or self::section or self::span]"
            )
        );
    }

    #[test]
    fn overall_discard_xpath_second_prefix() {
        // xpaths.py:148.
        assert!(
            OVERALL_DISCARD_XPATH[1].starts_with(
                ".//*[@class=\"comments-title\" or contains(@class, \"comments-title\")"
            )
        );
    }

    #[test]
    fn teaser_discard_xpath_prefix() {
        // xpaths.py:170.
        assert!(
            TEASER_DISCARD_XPATH[0].starts_with(
                ".//*[self::div or self::item or self::list\n             or self::p or self::section or self::span]"
            )
        );
    }

    #[test]
    fn precision_discard_xpath_first_is_header() {
        // xpaths.py:178.
        assert_eq!(PRECISION_DISCARD_XPATH[0], ".//header");
    }

    #[test]
    fn precision_discard_xpath_second_prefix() {
        // xpaths.py:179.
        assert!(
            PRECISION_DISCARD_XPATH[1].starts_with(
                ".//*[self::div or self::item or self::list\n             or self::p or self::section or self::span]"
            )
        );
    }

    #[test]
    fn discard_image_elements_prefix() {
        // xpaths.py:190.
        assert!(
            DISCARD_IMAGE_ELEMENTS[0].starts_with(
                ".//*[self::div or self::item or self::list\n             or self::p or self::section or self::span]"
            )
        );
    }

    #[test]
    fn comments_discard_xpath_first_is_respond() {
        // xpaths.py:199.
        assert_eq!(
            COMMENTS_DISCARD_XPATH[0],
            ".//*[self::div or self::section][starts-with(@id, \"respond\")]"
        );
    }

    #[test]
    fn comments_discard_xpath_second_is_cite_union_quote() {
        // xpaths.py:200.
        assert_eq!(COMMENTS_DISCARD_XPATH[1], ".//cite|.//quote");
    }

    #[test]
    fn author_xpaths_first_prefix() {
        // xpaths.py:216.
        assert!(
            AUTHOR_XPATHS[0].starts_with(
                "//*[self::a or self::address or self::div or self::link or self::p or self::span or self::strong]"
            )
        );
    }

    #[test]
    fn author_discard_xpaths_second_is_time_union_figure() {
        // xpaths.py:231.
        assert_eq!(AUTHOR_DISCARD_XPATHS[1], "//time|//figure");
    }

    #[test]
    fn categories_xpaths_last_is_row_or_tags() {
        // xpaths.py:244.
        assert_eq!(
            CATEGORIES_XPATHS[5],
            "//div[@class=\"row\" or @class=\"tags\"]//a[@href]"
        );
    }

    #[test]
    fn tags_xpaths_first_is_div_class_tags() {
        // xpaths.py:250.
        assert_eq!(TAGS_XPATHS[0], "//div[@class=\"tags\"]//a[@href]");
    }

    #[test]
    fn title_xpaths_second_is_entry_or_post_title() {
        // xpaths.py:263.
        assert_eq!(
            TITLE_XPATHS[1],
            "//*[@class=\"entry-title\" or @class=\"post-title\"]"
        );
    }

    // ---- Structural invariants (defensive) -------------------------------

    #[test]
    fn no_xpath_is_empty() {
        for (name, _src, exprs) in ALL_XPATHS {
            for (i, e) in exprs.iter().enumerate() {
                assert!(
                    !e.trim().is_empty(),
                    "{name}[{i}] must not be empty / whitespace-only"
                );
            }
        }
    }

    #[test]
    fn all_xpaths_table_matches_per_constant_lengths() {
        // The aggregator table must list every constant with its real length.
        // (Anti-inversion: catches a future PR that adds an entry to one of
        // the constants without updating ALL_XPATHS.)
        let pairs: &[(&str, usize)] = &[
            ("BODY_XPATH", BODY_XPATH.len()),
            ("COMMENTS_XPATH", COMMENTS_XPATH.len()),
            ("REMOVE_COMMENTS_XPATH", REMOVE_COMMENTS_XPATH.len()),
            ("OVERALL_DISCARD_XPATH", OVERALL_DISCARD_XPATH.len()),
            ("TEASER_DISCARD_XPATH", TEASER_DISCARD_XPATH.len()),
            ("PRECISION_DISCARD_XPATH", PRECISION_DISCARD_XPATH.len()),
            ("DISCARD_IMAGE_ELEMENTS", DISCARD_IMAGE_ELEMENTS.len()),
            ("COMMENTS_DISCARD_XPATH", COMMENTS_DISCARD_XPATH.len()),
            ("AUTHOR_XPATHS", AUTHOR_XPATHS.len()),
            ("AUTHOR_DISCARD_XPATHS", AUTHOR_DISCARD_XPATHS.len()),
            ("CATEGORIES_XPATHS", CATEGORIES_XPATHS.len()),
            ("TAGS_XPATHS", TAGS_XPATHS.len()),
            ("TITLE_XPATHS", TITLE_XPATHS.len()),
        ];
        for (name, expected_len) in pairs {
            let entry = ALL_XPATHS
                .iter()
                .find(|(n, _, _)| n == name)
                .unwrap_or_else(|| panic!("ALL_XPATHS missing {name}"));
            assert_eq!(
                entry.2.len(),
                *expected_len,
                "ALL_XPATHS length mismatch for {name}"
            );
        }
    }
}
