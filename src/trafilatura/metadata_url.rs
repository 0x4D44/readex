//! Stage 7d — minimal URL canonicalization + date stub + deferred
//! category/tag/license extractors.
//!
//! Source of truth: `trafilatura@v2.0.0/metadata.py:389-479` (the URL /
//! sitename / category / license HTML-tag extractors), plus the small subset
//! of `courlan/urlutils.py:14-62` Trafilatura's metadata pipeline calls
//! into (`extract_domain` / `get_base_url`). The full Python `htmldate`
//! package is NOT ported in Stage 7d — see [`extract_date`] for the exact
//! scope vs deferred behaviour.
//!
//! # Anti-inversion (HLD §4 / §10)
//!
//! Every non-trivial function header carries a `metadata.py:NN`,
//! `urlutils.py:NN`, or `clean.py:NN` source-line cite. The four entry points
//! ([`extract_url`], [`extract_domain`], [`extract_date`], plus the private
//! [`extract_catstags`] / [`extract_license`] consumed by
//! [`crate::trafilatura::metadata::extract_metadata`]) are byte-faithful
//! ports of their Python counterparts within the scope each function header
//! documents — divergences (the `htmldate` deferral being the load-bearing
//! one) are recorded inline, never hidden.
//!
//! # Scope (Stage 7d)
//!
//! **In scope** — populates the previously-stubbed `Metadata` fields:
//! - `url` — `<link rel="canonical">` / `<base>` / `<link rel="alternate"
//!   hreflang="x-default">` (`URL_SELECTORS` at `metadata.py:153-157`) with
//!   relative-URL repair via `og:`/`twitter:` content sniffing (also
//!   `metadata.py:397-406`).
//! - `hostname` — `extract_domain(metadata.url, fast=True)`
//!   (`urlutils.py:49-62`) reduced to the regex-fast path the Python
//!   pipeline takes at `metadata.py:542-543`.
//! - `date` — see [`extract_date`] for the documented STUB scope.
//! - `categories` / `tags` — `extract_catstags("category", tree)` /
//!   `extract_catstags("tag", tree)` (`metadata.py:422-446`).
//! - `license` — `extract_license(tree)` (`metadata.py:465-479`).
//!
//! **Deferred** — Stage 7d intentionally STUBS these so the higher-value
//! pieces ship cleanly:
//! - Full `htmldate` port (locale-aware fuzzy date parsing) — Python's
//!   `htmldate` package is ~3000 LOC and uses `dateparser` + `dateutil`. We
//!   ship the obvious-HTML-hints subset (see [`extract_date`]).
//! - `extract_json_parse_error`-style salvage for malformed URLs — Stage 7d
//!   treats invalid URLs the way `metadata.py:408-413` does: drop, fall
//!   back to `default_url`.
//! - The full `normalize_url` query-tracker stripper — Stage 7d implements
//!   the minimal lowercase-scheme + lowercase-netloc + trailing-slash
//!   handling subset (see [`normalize_url`]).
//!
//! All five entry points (`extract_url` / `extract_domain` / `extract_date`
//! / `extract_catstags` / `extract_license`) are wired into
//! [`crate::trafilatura::metadata::extract_metadata`] so the existing
//! `Metadata` struct populates additively — JSON-LD-supplied values
//! (Stage 7b) keep precedence.

use crate::readability::dom::{
    Dom, NodeRef, element_text, get_attribute, get_elements_by_tag_name, local_name, text_content,
};
use crate::trafilatura::utils::trim;
use crate::trafilatura::xpath_engine;
use crate::trafilatura::xpaths_constants::{CATEGORIES_XPATHS, TAGS_XPATHS};

// ===========================================================================
// URL canonicalization (`metadata.py:389-413` + `urlutils.py:49-62` +
// `clean.py:173-207`)
// ===========================================================================

/// `URL_SELECTORS` (`metadata.py:153-157`). Iterated in declaration order
/// by [`extract_url`]; the first matched `<link>`/`<base>` with a non-empty
/// `href` wins.
///
/// The Python list uses lxml `tree.find(selector)` which is the lxml
/// "ElementPath" mini-language (a `.find()`-flavoured XPath subset). We
/// implement the three patterns structurally rather than route through the
/// Stage 0b XPath engine — each is a simple `<head>//link[@rel=X][...]` or
/// `<head>//base` walk, and the structural code is shorter than a string
/// dispatch.
const URL_SELECTORS: &[&str] = &[
    // metadata.py:154 — `.//head//link[@rel="canonical"]`
    "link@rel=canonical",
    // metadata.py:155 — `.//head//base`
    "base",
    // metadata.py:156 — `.//head//link[@rel="alternate"][@hreflang="x-default"]`
    "link@rel=alternate@hreflang=x-default",
];

/// `extract_url(tree, default_url)` (`metadata.py:389-413`).
///
/// Resolution order:
/// 1. Walk `URL_SELECTORS` under `<head>`; take the first matching element's
///    `href` (or `og:url` from the `<base>` element — Python uses the
///    element's `href` attribute uniformly across all three selectors).
/// 2. If the URL starts with `/`, repair it via the `og:`/`twitter:` content
///    of the first `<meta content="..." name="og:..." or property="og:...">`
///    we can extract a base URL from (`metadata.py:397-406`).
/// 3. If the URL fails the minimal validity gate (`http`/`https` scheme +
///    non-empty host), fall through to `default_url`.
///
/// Stage 7d divergence from Python: Python uses `courlan.validate_url` +
/// `courlan.normalize_url`; we implement the minimal validity gate
/// ([`is_valid_url`]) and the minimal normalisation ([`normalize_url`])
/// inline — both faithful within scope, deferring the full query-tracker
/// stripper to a future fold-in.
pub fn extract_url(dom: &Dom, default_url: Option<&str>) -> Option<String> {
    let url = walk_url_selectors(dom);

    // Step 2: relative-URL repair (`metadata.py:397-406`).
    let url = url.and_then(|raw| {
        if raw.starts_with('/') {
            repair_relative_url(dom, &raw).or(Some(raw))
        } else {
            Some(raw)
        }
    });

    // Step 3: validity gate (`metadata.py:408-411`).
    let normalised = url.and_then(|raw| {
        if is_valid_url(&raw) {
            Some(normalize_url(&raw))
        } else {
            None
        }
    });

    normalised.or_else(|| default_url.map(|s| s.to_string()))
}

/// Walk the three `URL_SELECTORS` patterns under `<head>`, returning the
/// first non-empty `href`. The Python source iterates `URL_SELECTORS` and
/// calls `tree.find(selector)` per pattern; we replicate the same shortest-
/// match-wins ordering structurally.
fn walk_url_selectors(dom: &Dom) -> Option<String> {
    let head = find_head(dom)?;
    for selector in URL_SELECTORS {
        let url = match *selector {
            // `metadata.py:154` — link[@rel="canonical"]
            "link@rel=canonical" => find_link_with_rel(&head, "canonical"),
            // `metadata.py:155` — base
            "base" => first_base_href(&head),
            // `metadata.py:156` — link[@rel="alternate"][@hreflang="x-default"]
            "link@rel=alternate@hreflang=x-default" => find_alternate_x_default(&head),
            _ => None,
        };
        if let Some(u) = url
            && !u.trim().is_empty()
        {
            return Some(u);
        }
    }
    None
}

/// Find `<head>` under the document root.
fn find_head(dom: &Dom) -> Option<NodeRef> {
    let html = dom.root_element()?;
    children_of(&html)
        .into_iter()
        .find(|c| local_name(c).as_deref() == Some("head"))
}

/// Children helper that doesn't re-export `dom::children` (we already have
/// `local_name` / `get_elements_by_tag_name` imported, and this keeps the
/// import surface minimal).
fn children_of(node: &NodeRef) -> Vec<NodeRef> {
    crate::readability::dom::children(node)
}

/// Find the first `<link rel="WANT">` descendant of `head` with a `href`.
fn find_link_with_rel(head: &NodeRef, want_rel: &str) -> Option<String> {
    for link in get_elements_by_tag_name(head, "link") {
        if let Some(rel) = get_attribute(&link, "rel")
            && rel.eq_ignore_ascii_case(want_rel)
            && let Some(href) = get_attribute(&link, "href")
        {
            return Some(href);
        }
    }
    None
}

/// Find the first `<base>` descendant with a `href`.
fn first_base_href(head: &NodeRef) -> Option<String> {
    for base in get_elements_by_tag_name(head, "base") {
        if let Some(href) = get_attribute(&base, "href") {
            return Some(href);
        }
    }
    None
}

/// Find the first `<link rel="alternate" hreflang="x-default">`.
fn find_alternate_x_default(head: &NodeRef) -> Option<String> {
    for link in get_elements_by_tag_name(head, "link") {
        let rel_match = get_attribute(&link, "rel")
            .map(|r| r.eq_ignore_ascii_case("alternate"))
            .unwrap_or(false);
        let hreflang_match = get_attribute(&link, "hreflang")
            .map(|h| h.eq_ignore_ascii_case("x-default"))
            .unwrap_or(false);
        if rel_match && hreflang_match
            && let Some(href) = get_attribute(&link, "href")
        {
            return Some(href);
        }
    }
    None
}

/// Repair a relative URL by sniffing `og:`/`twitter:` meta-content for a
/// base URL (`metadata.py:399-406`). Returns `Some(base + relative_url)` if
/// we can extract a base URL from any matching meta element, otherwise
/// `None`.
fn repair_relative_url(dom: &Dom, rel: &str) -> Option<String> {
    let head = find_head(dom)?;
    for elem in get_elements_by_tag_name(&head, "meta") {
        let attrtype = get_attribute(&elem, "name")
            .or_else(|| get_attribute(&elem, "property"))
            .unwrap_or_default();
        if (attrtype.starts_with("og:") || attrtype.starts_with("twitter:"))
            && let Some(content) = get_attribute(&elem, "content")
            && let Some(base) = get_base_url(&content)
            && !base.is_empty()
        {
            return Some(format!("{base}{rel}"));
        }
    }
    None
}

/// `extract_domain(url, fast=True)` (`urlutils.py:49-62`) — minimal regex-fast
/// port.
///
/// Returns the full domain (with subdomain — e.g. `"www.example.com"` for
/// `"https://www.example.com/foo"`). Python's `extract_domain` with
/// `fast=True` runs `DOMAIN_REGEX` (`urlutils.py:14-21`) and returns
/// `STRIP_PORT_REGEX.sub("", domain_match[1].split("@")[-1])` — i.e. the
/// **full domain including subdomain**, port-stripped, after the optional
/// `user@` userinfo. We replicate that minimal scope here.
///
/// **Documented Python behaviour** (verified against the Python source):
/// `extract_domain("https://www.example.com/foo")` returns
/// `"www.example.com"`, NOT `"example.com"` — the `www.` prefix is part of
/// the returned `full_domain`. Python's `urlutils.py:46` applies
/// `CLEAN_FLD_REGEX = re.compile(r"^www[0-9]*\.")` only on the **slow path**
/// (via the `tld` library); the `fast=True` regex path returns
/// `www.example.com` verbatim.
///
/// Stage 7d uses the fast path uniformly (matching `metadata.py:543`'s
/// `extract_domain(metadata.url, fast=True)` call).
pub fn extract_domain(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    // DOMAIN_REGEX shape (`urlutils.py:14-21`):
    //   (?:(?:f|ht)tp)s?:// + (?:[^/?#]{,63}\.)? + (
    //     [^/?#.]{4,63}\.[^/?#]{2,63} | IPv4 | IPv6
    //   ) + (?:/|$)
    //
    // We implement this structurally rather than pull in a regex compile
    // for one pattern: parse the scheme + `://` prefix, capture everything
    // up to the next `/`, `?`, or `#` as the authority, strip optional
    // userinfo + port.

    // Scheme gate.
    let after_scheme = strip_scheme(url)?;
    // Authority = everything up to `/`, `?`, `#` (or end).
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return None;
    }
    // Strip optional `user@` userinfo.
    let host_and_port = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    // Strip optional `:port` (only when port digits follow a non-digit —
    // matches Python `STRIP_PORT_REGEX = r"(?<=\D):\d+"`).
    let host = strip_port(host_and_port);
    if host.is_empty() {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

/// Strip the `http://` / `https://` (or `ftp[s]://`) prefix; return `None` if
/// the URL does not begin with one of these.
fn strip_scheme(url: &str) -> Option<&str> {
    for scheme in &["https://", "http://", "ftps://", "ftp://"] {
        if url.len() >= scheme.len()
            && url[..scheme.len()].eq_ignore_ascii_case(scheme)
        {
            return Some(&url[scheme.len()..]);
        }
    }
    None
}

/// Strip a trailing `:NNN...` port from `authority` when the character before
/// the `:` is a non-digit (mirrors Python `STRIP_PORT_REGEX = r"(?<=\D):\d+"`).
/// For IPv6 addresses in `[...]` notation Python's regex doesn't match
/// because `]` is non-digit; we replicate by only trimming if the segment
/// after `:` is all digits.
fn strip_port(authority: &str) -> &str {
    let Some(colon) = authority.rfind(':') else {
        return authority;
    };
    // Predecessor must be non-digit (Python lookbehind `(?<=\D)`).
    let prev_is_digit = authority[..colon]
        .chars()
        .next_back()
        .map(|c| c.is_ascii_digit())
        .unwrap_or(false);
    if prev_is_digit {
        return authority;
    }
    let port = &authority[colon + 1..];
    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
        return authority;
    }
    &authority[..colon]
}

/// `get_base_url(url)` (`urlutils.py:76-84`) — the `scheme://netloc` prefix.
///
/// Returns `None` for malformed inputs (no recognised scheme), otherwise
/// `Some("https://host[:port]")` etc.
fn get_base_url(url: &str) -> Option<String> {
    let after_scheme_start = url
        .find("://")
        .filter(|i| *i > 0)
        .map(|i| i + 3)?;
    let scheme = &url[..after_scheme_start - 3];
    let after_scheme = &url[after_scheme_start..];
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let netloc = &after_scheme[..authority_end];
    if netloc.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{netloc}"))
}

/// Minimal URL validity gate (`courlan.filters.validate_url` reduced to the
/// load-bearing scheme + host check). Returns `true` when the URL has an
/// `http` or `https` scheme + a non-empty host.
fn is_valid_url(url: &str) -> bool {
    let Some(rest) = strip_scheme(url) else {
        return false;
    };
    // Reject `ftp[s]://` for the validity gate — Python's `validate_url`
    // (`filters.py:253-271`) checks scheme is in `("http", "https")`.
    if !(url.starts_with("http://")
        || url.starts_with("https://")
        || url.to_ascii_lowercase().starts_with("http://")
        || url.to_ascii_lowercase().starts_with("https://"))
    {
        return false;
    }
    let authority_end = rest
        .find(['/', '?', '#'])
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return false;
    }
    // Host must contain at least one `.` (the DOMAIN_REGEX requirement) OR
    // be `localhost` — keep this permissive; the Python source uses the
    // `tld` library for the full check and falls through gracefully.
    let host_and_port = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    let host = strip_port(host_and_port);
    host.contains('.') || host.eq_ignore_ascii_case("localhost")
}

/// Minimal `normalize_url` (`clean.py:173-207`) — lowercase scheme +
/// lowercase netloc only. The full Python implementation strips tracker
/// query parameters, normalises percent-encoded characters, and (when
/// `trailing_slash=False`) trims a trailing `/`; we defer the tracker
/// stripper to a future fold-in (no Stage 3-B fixture exercises it).
///
/// This is the function that `metadata.py:411` calls after
/// `validate_url`: `url = normalize_url(parsed_url) if validation_result
/// else None`.
fn normalize_url(url: &str) -> String {
    let Some(rest) = strip_scheme(url) else {
        return url.to_string();
    };
    // Reconstruct with lowercased scheme + lowercased netloc.
    let scheme_lower = url[..url.len() - rest.len() - 3].to_ascii_lowercase();
    let authority_end = rest
        .find(['/', '?', '#'])
        .unwrap_or(rest.len());
    let netloc_lower = rest[..authority_end].to_ascii_lowercase();
    let tail = &rest[authority_end..];
    format!("{scheme_lower}://{netloc_lower}{tail}")
}

// ===========================================================================
// Date stub (`metadata.py:546-547` calls `find_date(tree, **date_config)`;
// `htmldate` is a separate ~3000 LOC package — see header for scope).
// ===========================================================================

/// Extract a publication / modification date from the document.
///
/// **M4 Stage 1 sub-stage G** wires Trafilatura's `metadata.find_date(tree)`
/// call (`metadata.py:547-550` / `trafilatura/v2.0.0`) into the full
/// `htmldate` port (`htmldate/core.py:808-983`). Where the M3 Stage 7d
/// "obvious HTML hints" stub only knew about `<meta property=
/// "article:published_time">`, `<meta name="date">`, `<meta itemprop=
/// "datePublished">`, and `<time datetime="...">`, the full port runs the
/// complete htmldate cascade: URL canonical-link probe → header walk →
/// JSON-LD search → abbr/time element walks → discard_unwanted +
/// CLEANING_LIST pruning → date-expression XPath + title/h1 fallback →
/// timestamp pattern / img_search / idiosyncrasies_search → extensive
/// free-text + search_page rescue.
///
/// The returned date string is formatted per `"%Y-%m-%d"` — the htmldate
/// default and the format the existing tests pin. This is a **behaviour
/// upgrade** vs the Stage 7d stub: pure-text dates ("Posted: January 15,
/// 2024" in a `<p>`) now succeed instead of falling through to `None`.
/// Stage 7b's JSON-LD `datePublished`/`dateModified` path still runs first
/// in `extract_metadata` and keeps precedence — `extract_date` only fires
/// when JSON-LD didn't supply a date.
pub fn extract_date(dom: &Dom) -> Option<String> {
    let tree = dom.root_element()?;
    let options = crate::htmldate::utils::Extractor::new(
        // Python `extensive_search=True` default (core.py:810).
        true,
        // Python `get_max_date(None)` returns `datetime.now()`; the Rust
        // port pins to a "very future" sentinel for test determinism (see
        // validators.rs::get_max_date). The (9999, 12, 31) tuple gives the
        // same "no upper bound" effect.
        (9999, 12, 31),
        // Python `get_min_date(None)` returns `MIN_DATE = (1995, 1, 1)`.
        crate::htmldate::settings::MIN_DATE,
        // Python `original_date=False` default (core.py:811).
        false,
        // Python `outputformat="%Y-%m-%d"` default (core.py:812).
        "%Y-%m-%d".to_string(),
    );
    crate::htmldate::core::find_date(&tree, &options)
}

// ===========================================================================
// extract_catstags (`metadata.py:422-446`)
// ===========================================================================

/// `extract_catstags(metatype, tree)` (`metadata.py:422-446`).
///
/// Walks `CATEGORIES_XPATHS` (when `metatype = "category"`) or `TAGS_XPATHS`
/// (when `metatype = "tag"`), collecting the `text_content` of every
/// matched `<a href="...">` whose `href` contains `/{metatype}[s|ies]?/`
/// (the `regexpr` filter at `metadata.py:425`). The first XPath that
/// produces ANY results wins (`metadata.py:434-435` — `if results: break`).
///
/// For categories only, the fallback at `metadata.py:437-441` checks
/// `<meta property="article:section">` and `<meta name="*subject*">`.
///
/// Returns a deduplicated list (Python uses `dict.fromkeys(...)` —
/// insertion-order preserving dedup at `metadata.py:446`).
pub(crate) fn extract_catstags(dom: &Dom, metatype: &str) -> Vec<String> {
    let Some(body) = dom.body() else {
        return Vec::new();
    };
    let xpath_list: &[&str] = if metatype == "category" {
        CATEGORIES_XPATHS
    } else {
        TAGS_XPATHS
    };

    let mut results: Vec<String> = Vec::new();

    // `regexpr = "/" + metatype + "[s|ies]?/"` — Python's `re.search` over
    // the `href` attribute. The character class `[s|ies]?` is a Python
    // typo-quirk: it matches a single character from the set
    // `{s, |, i, e}` (i.e. NOT the alternation `s` OR `ies`). We replicate
    // the same membership check verbatim — same chars, same single-char
    // semantics.
    let metatype_lower = metatype.to_ascii_lowercase();
    for expr in xpath_list {
        let Ok(matches) = xpath_engine::evaluate(expr, &body) else {
            continue;
        };
        for elem in &matches {
            let Some(href) = get_attribute(elem, "href") else {
                continue;
            };
            if href_matches_metatype(&href, &metatype_lower) {
                let txt = text_content(elem);
                let t = line_processing(&txt);
                if !t.is_empty() && !results.contains(&t) {
                    results.push(t);
                }
            }
        }
        if !results.is_empty() {
            break;
        }
    }

    // Category fallback (`metadata.py:437-441`).
    if metatype == "category"
        && results.is_empty()
        && let Some(head) = find_head(dom)
    {
        for elem in get_elements_by_tag_name(&head, "meta") {
            let is_section = get_attribute(&elem, "property")
                .map(|p| p.eq_ignore_ascii_case("article:section"))
                .unwrap_or(false);
            let is_subject_name = get_attribute(&elem, "name")
                .map(|n| n.to_ascii_lowercase().contains("subject"))
                .unwrap_or(false);
            if (is_section || is_subject_name)
                && let Some(content) = get_attribute(&elem, "content")
            {
                let t = line_processing(&content);
                if !t.is_empty() && !results.contains(&t) {
                    results.push(t);
                }
            }
        }
    }
    results
}

/// Faithful port of Python's `re.search("/" + metatype + "[s|ies]?/", href)`.
/// As called out inline in [`extract_catstags`], the `[s|ies]?` is a
/// **single-character class** (Python regex quirk) — it matches one
/// character from `{s, |, i, e}`, optional. We replicate exactly that:
/// `href` must contain `/<metatype>/` OR `/<metatype>X/` where X is any
/// one of `s`, `|`, `i`, `e`.
fn href_matches_metatype(href: &str, metatype: &str) -> bool {
    // Plain `/<metatype>/`.
    let needle_bare = format!("/{metatype}/");
    if href.contains(&needle_bare) {
        return true;
    }
    // `/<metatype>X/` for X in {s, |, i, e}.
    for ch in ['s', '|', 'i', 'e'] {
        let needle = format!("/{metatype}{ch}/");
        if href.contains(&needle) {
            return true;
        }
    }
    false
}

/// `line_processing(line)` (`utils.py:283-300`) — minimal port: replace
/// `&#13;`/`&#10;`/`&nbsp;` then trim. The full Python helper additionally
/// strips control characters via `remove_control_characters` and applies a
/// `LINES_TRIMMING` regex; we apply the obvious entity replacements + trim
/// (which collapses internal whitespace) — sufficient for the category/tag
/// link-text path where the input is typically clean.
fn line_processing(line: &str) -> String {
    let replaced = line
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&nbsp;", "\u{00A0}");
    trim(&replaced)
}

// ===========================================================================
// extract_license (`metadata.py:465-479` + `parse_license_element`
// `metadata.py:449-462`)
// ===========================================================================

/// `extract_license(tree)` (`metadata.py:465-479`).
///
/// Resolution order:
/// 1. Walk `<a rel="license" href="...">` anywhere in the document; parse
///    each via [`parse_license_element`] in non-strict mode.
/// 2. Walk `<footer>//a[@href]` and `<div class="*footer*">//a[@href]`;
///    parse each in **strict** mode (requires the TEXT_LICENSE_REGEX
///    match — CC + license-token + optional version).
///
/// Returns the first non-`None` parse result.
pub(crate) fn extract_license(dom: &Dom) -> Option<String> {
    let html = dom.root_element()?;

    // 1. rel="license" links.
    for a in get_elements_by_tag_name(&html, "a") {
        let rel_match = get_attribute(&a, "rel")
            .map(|r| r.eq_ignore_ascii_case("license"))
            .unwrap_or(false);
        if rel_match
            && let Some(result) = parse_license_element(&a, false)
        {
            return Some(result);
        }
    }

    // 2. footer / div.footer / div#footer links (strict mode).
    let body = dom.body()?;
    for elem in get_elements_by_tag_name(&body, "footer") {
        for a in get_elements_by_tag_name(&elem, "a") {
            if get_attribute(&a, "href").is_some()
                && let Some(result) = parse_license_element(&a, true)
            {
                return Some(result);
            }
        }
    }
    for elem in get_elements_by_tag_name(&body, "div") {
        let is_footer_class = get_attribute(&elem, "class")
            .map(|c| c.to_ascii_lowercase().contains("footer"))
            .unwrap_or(false);
        let is_footer_id = get_attribute(&elem, "id")
            .map(|i| i.to_ascii_lowercase().contains("footer"))
            .unwrap_or(false);
        if !(is_footer_class || is_footer_id) {
            continue;
        }
        for a in get_elements_by_tag_name(&elem, "a") {
            if get_attribute(&a, "href").is_some()
                && let Some(result) = parse_license_element(&a, true)
            {
                return Some(result);
            }
        }
    }
    None
}

/// `parse_license_element(element, strict)` (`metadata.py:449-462`).
///
/// Two-arm probe of an `<a>` for license cues:
/// 1. **href arm** — match `LICENSE_REGEX = r"/(by-nc-nd|by-nc-sa|by-nc|
///    by-nd|by-sa|by|zero)/([1-9]\.[0-9])"`. Returns
///    `"CC {license.upper()} {version}"`.
/// 2. **text arm** — if no href match and the link has text:
///    - `strict=false`: return the trimmed text verbatim.
///    - `strict=true`: match `TEXT_LICENSE_REGEX = r"(cc|creative
///      commons) (by-nc-nd|by-nc-sa|by-nc|by-nd|by-sa|by|zero) ?
///      ([1-9]\.[0-9])?"` case-insensitively; return the matched
///      substring or `None`.
fn parse_license_element(elem: &NodeRef, strict: bool) -> Option<String> {
    // Arm 1: href LICENSE_REGEX (`metadata.py:453-455`).
    let href = get_attribute(elem, "href").unwrap_or_default();
    if let Some((token, version)) = match_license_regex(&href) {
        return Some(format!("CC {} {}", token.to_uppercase(), version));
    }
    // Arm 2: link text (`metadata.py:456-461`).
    let text = element_text(elem)?;
    let t = trim(&text);
    if t.is_empty() {
        return None;
    }
    if strict {
        match_text_license_regex(&t)
    } else {
        Some(t)
    }
}

/// Match the Python `LICENSE_REGEX = r"/(by-nc-nd|by-nc-sa|by-nc|by-nd|
/// by-sa|by|zero)/([1-9]\.[0-9])"` structurally.
fn match_license_regex(href: &str) -> Option<(String, String)> {
    // Order matters — longer prefixes first (Python's regex alternation is
    // leftmost-longest? No, it's leftmost-first — but the way the
    // alternation is structured in metadata.py:56-58, the longer prefixes
    // are listed FIRST, so we mirror that ordering).
    const TOKENS: &[&str] = &[
        "by-nc-nd", "by-nc-sa", "by-nc", "by-nd", "by-sa", "by", "zero",
    ];
    for token in TOKENS {
        let prefix = format!("/{token}/");
        if let Some(idx) = href.find(&prefix) {
            // Version must match `[1-9]\.[0-9]` immediately after.
            let after = &href[idx + prefix.len()..];
            let mut chars = after.chars();
            let (Some(major), Some(dot), Some(minor)) = (chars.next(), chars.next(), chars.next())
            else {
                continue;
            };
            if major.is_ascii_digit()
                && major != '0'
                && dot == '.'
                && minor.is_ascii_digit()
            {
                let version = format!("{major}.{minor}");
                return Some(((*token).to_string(), version));
            }
        }
    }
    None
}

/// Match the Python `TEXT_LICENSE_REGEX = r"(cc|creative commons) (by-nc-nd
/// |by-nc-sa|by-nc|by-nd|by-sa|by|zero) ?([1-9]\.[0-9])?"` (case-insensitive).
/// Returns the matched substring (Python: `match[0]`).
fn match_text_license_regex(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    const PREFIXES: &[&str] = &["creative commons", "cc"];
    const TOKENS: &[&str] = &[
        "by-nc-nd", "by-nc-sa", "by-nc", "by-nd", "by-sa", "by", "zero",
    ];
    for prefix in PREFIXES {
        let Some(p_idx) = lower.find(prefix) else {
            continue;
        };
        // Must be followed by ` ` then a TOKEN.
        let after_prefix = &lower[p_idx + prefix.len()..];
        if !after_prefix.starts_with(' ') {
            continue;
        }
        let after_space = &after_prefix[1..];
        for token in TOKENS {
            if let Some(after_token) = after_space.strip_prefix(token) {
                // Optional ` ?([1-9]\.[0-9])?` follows.
                let mut end = p_idx + prefix.len() + 1 + token.len();
                let rest = after_token.trim_start_matches(' ');
                let mut version_chars = rest.chars();
                if let (Some(major), Some(dot), Some(minor)) =
                    (version_chars.next(), version_chars.next(), version_chars.next())
                    && major.is_ascii_digit()
                    && major != '0'
                    && dot == '.'
                    && minor.is_ascii_digit()
                {
                    // Account for the consumed space + version in the
                    // original mixed-case string.
                    let consumed_space = after_token.len() - rest.len();
                    end += consumed_space + 3;
                }
                return Some(text[p_idx..end].to_string());
            }
        }
    }
    None
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trafilatura::metadata::extract_metadata;

    fn parse(html: &str) -> Dom {
        Dom::parse(html)
    }

    // ---- extract_url ------------------------------------------------------

    #[test]
    fn extract_url_from_link_canonical() {
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="https://example.com/a">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/a")
        );
    }

    #[test]
    fn extract_url_falls_back_to_og_url() {
        // No canonical link; only og:url. Python's URL_SELECTORS doesn't
        // include og:url directly, but `examine_meta` populates
        // `Metadata.url` from `og:url`. `extract_url` then guards with
        // `if not metadata.url:` (metadata.py:538-539), so og:url wins
        // before `extract_url` runs in the orchestrator. To pin this
        // *integration* behaviour we test the full extract_metadata path.
        let html = r#"<html><head>
            <meta property="og:url" content="https://example.com/via-og">
            </head><body></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        assert_eq!(m.url.as_deref(), Some("https://example.com/via-og"));
    }

    #[test]
    fn extract_url_uses_default_url_fallback() {
        // Neither canonical link nor og:url present — `default_url` wins.
        let html = r#"<html><head></head><body></body></html>"#;
        let m = extract_metadata(html, Some("https://default.example.com/x"), true, &[]);
        assert_eq!(
            m.url.as_deref(),
            Some("https://default.example.com/x")
        );
    }

    #[test]
    fn extract_url_normalises_uppercase_scheme_and_host() {
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="HTTPS://Example.COM/Path">
                </head><body></body></html>"#,
        );
        // Stage 7d's minimal normalize_url lowercases scheme + netloc.
        // Path case is preserved (Python's normalize_url path is opaque
        // bytes too).
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/Path")
        );
    }

    #[test]
    fn extract_url_repairs_relative_via_og_url() {
        // canonical is relative (`/article/123`); og:url provides the base.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/article/123">
                <meta property="og:url" content="https://example.com/something">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/article/123")
        );
    }

    // ---- extract_domain ---------------------------------------------------

    #[test]
    fn extract_domain_simple() {
        // Python's `extract_domain("https://www.example.com/foo", fast=True)`
        // returns "www.example.com" — the `www.` prefix is part of the
        // fast-path output; only the slow path (via `tld`) strips it via
        // CLEAN_FLD_REGEX. We faithfully match the fast path here.
        assert_eq!(
            extract_domain("https://www.example.com/foo").as_deref(),
            Some("www.example.com")
        );
    }

    #[test]
    fn extract_domain_strips_port() {
        assert_eq!(
            extract_domain("http://example.com:8080/x").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_domain_strips_userinfo() {
        assert_eq!(
            extract_domain("https://user@example.com/x").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_domain_returns_none_for_no_scheme() {
        assert_eq!(extract_domain("example.com/foo"), None);
    }

    #[test]
    fn extract_domain_returns_none_for_empty() {
        assert_eq!(extract_domain(""), None);
    }

    // ---- extract_date -----------------------------------------------------

    #[test]
    fn extract_date_from_article_published_time() {
        let dom = parse(
            r#"<html><head>
                <meta property="article:published_time" content="2024-01-15">
                </head><body></body></html>"#,
        );
        assert_eq!(extract_date(&dom).as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn extract_date_from_time_element() {
        let dom = parse(
            r#"<html><head></head><body>
                <article><time datetime="2024-01-15">Jan 15</time></article>
                </body></html>"#,
        );
        assert_eq!(extract_date(&dom).as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn extract_date_from_meta_name_date() {
        let dom = parse(
            r#"<html><head>
                <meta name="date" content="2024-02-20">
                </head><body></body></html>"#,
        );
        assert_eq!(extract_date(&dom).as_deref(), Some("2024-02-20"));
    }

    #[test]
    fn extract_date_from_pure_text_date_works_with_htmldate() {
        // M4 Stage 1 sub-stage G FLIP: this case was the documented
        // STUB-era miss. The pure-text date "Posted: January 15, 2024"
        // parses via the full htmldate port's `regex_parse` arm
        // (`LONG_TEXT_PATTERN` at extractors.rs:281), which the Stage 7d
        // stub deferred. Now succeeds.
        let dom = parse(
            r#"<html><head></head><body>
                <p>Posted: January 15, 2024</p>
                </body></html>"#,
        );
        assert_eq!(extract_date(&dom).as_deref(), Some("2024-01-15"));
    }

    // ---- extract_catstags -------------------------------------------------

    #[test]
    fn extract_catstags_from_meta_keywords() {
        // Python's `extract_catstags` does NOT read `<meta name="keywords">`
        // directly — that path lives in `examine_meta` and populates
        // `Metadata.tags` via the METANAME_TAG table (Stage 7a). What
        // extract_catstags fills is the **link-based** tag extraction
        // (anchor href containing `/tag/` etc.). Verify the integration:
        // a meta-keywords-only document populates tags via the meta path,
        // and extract_catstags does NOT clobber that pre-populated list.
        let html = r#"<html><head>
            <meta name="keywords" content="rust, web, html">
            </head><body></body></html>"#;
        let m = extract_metadata(html, None, true, &[]);
        // Stage 7a normalize_tags converts ", " separator into the same
        // ", "-joined string — `examine_meta` collects each meta into the
        // tags Vec as ONE element. Verify the tag is present.
        assert!(
            m.tags.iter().any(|t| t.contains("rust") && t.contains("web") && t.contains("html")),
            "tags should contain rust/web/html, got {:?}",
            m.tags
        );
    }

    #[test]
    fn extract_catstags_link_based_tag() {
        // The link-based path (`/tag/` href filter) is what
        // extract_catstags directly tests. Build a div.tags > a[href=
        // /tag/rust].
        let dom = parse(
            r#"<html><head></head><body>
                <div class="tags">
                    <a href="/tag/rust">rust</a>
                    <a href="/tag/web">web</a>
                </div>
                </body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert_eq!(tags, vec!["rust".to_string(), "web".to_string()]);
    }

    // ---- extract_license --------------------------------------------------

    #[test]
    fn extract_license_from_creative_commons_link() {
        let dom = parse(
            r#"<html><head></head><body>
                <a rel="license" href="https://creativecommons.org/licenses/by-sa/4.0/">CC BY-SA 4.0</a>
                </body></html>"#,
        );
        // LICENSE_REGEX matches `/by-sa/4.0` -> "CC BY-SA 4.0"
        assert_eq!(extract_license(&dom).as_deref(), Some("CC BY-SA 4.0"));
    }

    #[test]
    fn extract_license_returns_text_when_no_regex_match() {
        // rel="license" but the href doesn't match LICENSE_REGEX —
        // non-strict mode returns the trimmed link TEXT.
        let dom = parse(
            r#"<html><head></head><body>
                <a rel="license" href="https://example.com/custom">Custom License</a>
                </body></html>"#,
        );
        assert_eq!(
            extract_license(&dom).as_deref(),
            Some("Custom License")
        );
    }

    // ---- Comprehensive e2e ------------------------------------------------

    #[test]
    fn extract_metadata_e2e_populates_all_fields_when_present() {
        // Comprehensive HTML carrying OG + JSON-LD + URL + date + categories +
        // tags + license. Verifies Stage 7d wires every field into
        // `extract_metadata` cleanly without clobbering Stage 7a/7b values.
        let html = r#"<html lang="en"><head>
            <title>Test Article | Example Site</title>
            <link rel="canonical" href="https://example.com/articles/test">
            <meta property="og:title" content="Test Article">
            <meta property="og:description" content="A test article.">
            <meta property="og:type" content="article">
            <meta property="og:image" content="https://example.com/img.jpg">
            <meta property="article:published_time" content="2024-01-15">
            <meta name="author" content="Jane Doe">
            <meta property="article:tag" content="rust, web">
            <script type="application/ld+json">
            {"@context": "https://schema.org",
             "@type": "NewsArticle",
             "headline": "JSON-LD Title",
             "author": {"@type": "Person", "name": "Jane Doe"},
             "publisher": {"@type": "Organization", "name": "Example Publisher"}}
            </script>
            </head><body>
                <div class="tags"><a href="/tag/rust">rust</a></div>
                <footer><a rel="license" href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a></footer>
            </body></html>"#;
        let m = extract_metadata(html, None, true, &[]);

        // Stage 7a: OG title wins over <title>-split + JSON-LD headline.
        assert_eq!(m.title.as_deref(), Some("Test Article"));
        // Stage 7a/7b: author from meta + JSON-LD merged ("Jane Doe").
        assert_eq!(m.author.as_deref(), Some("Jane Doe"));
        // Stage 7a: description + image + pagetype from OG.
        assert_eq!(m.description.as_deref(), Some("A test article."));
        assert_eq!(m.image.as_deref(), Some("https://example.com/img.jpg"));
        assert_eq!(m.pagetype.as_deref(), Some("article"));
        // Stage 7a: language from <html lang>.
        assert_eq!(m.language.as_deref(), Some("en"));
        // Stage 7b: site_name from JSON-LD publisher.
        assert_eq!(m.site_name.as_deref(), Some("Example Publisher"));

        // Stage 7d: URL from <link rel=canonical>.
        assert_eq!(
            m.url.as_deref(),
            Some("https://example.com/articles/test")
        );
        // Stage 7d: hostname extracted from URL.
        assert_eq!(m.hostname.as_deref(), Some("example.com"));
        // Stage 7d: date from <meta property=article:published_time>.
        assert_eq!(m.date.as_deref(), Some("2024-01-15"));
        // Stage 7d: tags include rust from both meta + link-based; in
        // the wiring, examine_meta populates from `article:tag` first
        // ("rust, web"), so extract_catstags doesn't overwrite. Verify a
        // tag is present.
        assert!(
            !m.tags.is_empty(),
            "expected at least one tag, got {:?}",
            m.tags
        );
        // Stage 7d: license from rel=license link.
        assert_eq!(m.license.as_deref(), Some("CC BY 4.0"));
    }

    // ---- Helper / internal tests -----------------------------------------

    #[test]
    fn href_matches_metatype_plain() {
        assert!(href_matches_metatype("/tag/rust", "tag"));
    }

    #[test]
    fn href_matches_metatype_with_s() {
        assert!(href_matches_metatype("/tags/rust", "tag"));
    }

    #[test]
    fn href_matches_metatype_rejects_unrelated() {
        assert!(!href_matches_metatype("/about/", "tag"));
    }

    #[test]
    fn normalize_url_lowercases_scheme_and_netloc() {
        assert_eq!(
            normalize_url("HTTPS://Example.COM/Path"),
            "https://example.com/Path"
        );
    }

    #[test]
    fn is_valid_url_accepts_http() {
        assert!(is_valid_url("http://example.com/x"));
    }

    #[test]
    fn is_valid_url_rejects_no_scheme() {
        assert!(!is_valid_url("example.com/x"));
    }
}
