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
use crate::trafilatura::output::strip_control_chars;
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
/// **Documented Python behaviour** (verified against the live oracle):
/// `extract_domain("https://www.example.com/foo")` returns `"example.com"`
/// (NOT `"www.example.com"`), `"https://en.wikipedia.org/x"` → `"wikipedia.org"`,
/// `"https://www.gov.uk/"` → `"gov.uk"`. The leading subdomain is stripped.
///
/// Implementation replicates `DOMAIN_REGEX` (`urlutils.py:14-21`)'s match
/// semantics for the fast path, plus a `www.`-stripping approximation of the
/// `get_tld` PSL fallback:
///
/// 1. **Fast path** (`DOMAIN_REGEX` group 1): the optional subdomain prefix
///    `(?:[^/?#]{,63}\.)?` is greedy, so it strips as many leading labels as
///    possible while leaving `([^/?#.]{4,63}\.[^/?#]{2,63})` — a label of
///    length 4–63 followed by `.` and a 2–63-char remainder (which may contain
///    dots). Equivalently: the domain begins at the RIGHTMOST non-final label
///    whose length is 4–63 (`news.bbc.co.uk` → `news.bbc.co.uk`;
///    `blog.example.co.uk` → `example.co.uk`). The IPv4/IPv6 alternatives need
///    no special-casing — a dotted-quad has no ≥4-char non-final label, so it
///    falls through to the fallback and is returned verbatim.
/// 2. **PSL fallback** when no non-final label qualifies (all <4 chars, e.g.
///    `www.gov.uk`, `www.sec.gov`): Python calls `get_tld(...).fld` (public
///    suffix list) then `CLEAN_FLD_REGEX = ^www[0-9]*\.`. The `tld` PSL is NOT
///    vendored (a data dependency); we approximate with the `www`-strip alone,
///    which reproduces every corpus fallback case (`gov.uk`, `sec.gov`,
///    `bbc.com`, `w3.org`). A host that genuinely needs the PSL (e.g.
///    `m.bbc.com`) would diverge — a documented gap, not in the corpus.
///
/// Matches `metadata.py:543`'s `extract_domain(metadata.url, fast=True)`.
pub fn extract_domain(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    // Scheme gate (DOMAIN_REGEX requires a protocol).
    let after_scheme = strip_scheme(url)?;
    // Authority = everything up to `/`, `?`, `#` (or end).
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    let authority = &after_scheme[..authority_end];
    if authority.is_empty() {
        return None;
    }
    // Strip optional `user@` userinfo (Python `.split("@")[-1]`).
    let host_and_port = authority.rsplit_once('@').map(|(_, h)| h).unwrap_or(authority);
    // Strip optional `:port` (Python `STRIP_PORT_REGEX = r"(?<=\D):\d+"`).
    let host = strip_port(host_and_port).to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }

    // Fast path: DOMAIN_REGEX group 1 = from the rightmost non-final label of
    // length 4..=63 to the end, with the remainder 2..=63 chars.
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() >= 2 {
        let last_non_final = labels.len() - 2;
        for i in (0..=last_non_final).rev() {
            let label_len = labels[i].len();
            if (4..=63).contains(&label_len) {
                let candidate = labels[i..].join(".");
                let rest_len = candidate.len() - label_len - 1; // strip "label."
                if (2..=63).contains(&rest_len) {
                    return Some(candidate);
                }
            }
        }
    }

    // PSL fallback approximation: strip a leading `www[0-9]*.` prefix.
    Some(strip_www_prefix(&host).to_string())
}

/// `CLEAN_FLD_REGEX = re.compile(r"^www[0-9]*\.")` (`urlutils.py:23`) — strip a
/// leading `www`/`wwwN` label and its dot.
fn strip_www_prefix(host: &str) -> &str {
    let Some(rest) = host.strip_prefix("www") else {
        return host;
    };
    let digits_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if let Some(stripped) = rest[digits_end..].strip_prefix('.') {
        stripped
    } else {
        host
    }
}

/// `META_URL = re.compile(r"https?://(?:www\.|w[0-9]+\.)?([^/]+)")`
/// (`metadata.py:46`) — the sitename URL fallback (`metadata.py:569-572`).
///
/// Anchored at the start; returns group 1 = the host (optional `www.`/`wN.`
/// prefix stripped), up to the first `/`. `None` when the URL does not begin
/// with an `http(s)://` authority.
pub(crate) fn meta_url_sitename(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    // Optional `www.` or `wN.` prefix (`w[0-9]+\.` requires ≥1 digit).
    let rest = if let Some(r) = rest.strip_prefix("www.") {
        r
    } else if let Some(r) = rest.strip_prefix('w') {
        let digits_end = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        if digits_end > 0 && r[digits_end..].starts_with('.') {
            &r[digits_end + 1..]
        } else {
            rest
        }
    } else {
        rest
    };
    let host_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_end];
    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
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
///
/// Visibility: `pub(crate)` so the `cleaning::convert_tags` `links=true` branch
/// (M4 Stage 2; `htmlprocessing.py:397`) can derive `base_url` from
/// `options.url` without duplicating the parsing logic.
pub(crate) fn get_base_url(url: &str) -> Option<String> {
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

/// `fix_relative_urls(baseurl, url)` (`courlan/urlutils.py:110-123`).
///
/// "Prepend protocol and host information to relative links." Used by
/// `convert_tags` when `options.links = true` to rewrite the `href`
/// attribute on `<a>`/`<ref>` elements into an absolute URL.
///
/// # Python original
///
/// ```python
/// def fix_relative_urls(baseurl: str, url: str) -> str:
///     "Prepend protocol and host information to relative links."
///     if url.startswith("{"):
///         return url
///     base_netloc = urlsplit(baseurl).netloc
///     split_url = urlsplit(url)
///     if split_url.netloc not in (base_netloc, ""):
///         if split_url.scheme:
///             return url
///         return urlunsplit(split_url._replace(scheme="http"))
///     return urljoin(baseurl, url)
/// ```
///
/// # Hand-rolled scope
///
/// The `url` crate is NOT a dependency of `mdrcel` (Cargo.toml lists only
/// `html5ever` / `markup5ever_rcdom` / `tendril` / `regex` / `serde_json`;
/// DEC-3 "deferred until the algorithm needs it" — relative-URL resolution
/// at `convert_tags`'s anchor branch is too small a surface to justify a
/// new dependency). The covered cases mirror Python's `urljoin` for the
/// shapes Trafilatura encounters in real HTML anchors:
///
/// - `url` starts with `{`  → return unchanged (template-literal guard,
///   `urlutils.py:112-113`).
/// - `url` has a scheme + netloc differing from baseurl's → return unchanged.
/// - `url` has a netloc but no scheme (`//other.com/x`) → prepend `http:`.
/// - `url` starts with `//` (protocol-relative) → `<scheme>://<...>`.
/// - `url` starts with `/` (absolute path) → `<scheme>://<netloc><url>`.
/// - `url` starts with `?` or `#` → splice the query/fragment onto the base
///   (matches RFC 3986's reference-resolution algorithm).
/// - Otherwise relative path → strip the basename from baseurl's path,
///   append the relative url.
pub(crate) fn fix_relative_urls(baseurl: &str, url: &str) -> String {
    // 112-113: template-literal escape (e.g. Jinja2 / Mustache `{{...}}`).
    if url.starts_with('{') {
        return url.to_string();
    }

    let (url_scheme, url_after_scheme) = split_scheme(url);
    let (url_netloc, url_path_etc) = split_netloc(url_after_scheme, url_scheme.is_some());

    // 118-121: url has a netloc differing from baseurl's netloc.
    if let Some(unetloc) = url_netloc {
        let base_netloc = split_netloc(split_scheme(baseurl).1, true).0.unwrap_or("");
        if !unetloc.is_empty() && unetloc != base_netloc {
            if url_scheme.is_some() {
                // Different host with a real scheme — leave as-is.
                return url.to_string();
            }
            // Scheme-less `//other.com/x` — Python `urlunsplit(_replace(
            // scheme="http"))` returns `http://other.com/x`.
            return format!("http:{url}");
        }
        // Same netloc as base — fall through to urljoin behaviour.
    }

    // 123: urljoin(baseurl, url) — RFC 3986 reference resolution.
    urljoin(baseurl, url, url_scheme, url_netloc, url_path_etc)
}

/// Split a URL into `(Some(scheme), rest_after_colon_slash_slash)` when it
/// has a recognised scheme followed by `://`, or `(None, whole_input)`
/// otherwise. The Python `urlsplit` is more permissive (it accepts
/// `mailto:foo`); `fix_relative_urls` only sees `http(s)`-shaped URLs in
/// Trafilatura's `<a>` anchors, so we restrict to the `<scheme>://` form.
fn split_scheme(url: &str) -> (Option<&str>, &str) {
    if let Some(idx) = url.find("://") {
        // Scheme must be non-empty + alphanumeric/`+`/`-`/`.` (RFC 3986).
        let scheme = &url[..idx];
        if !scheme.is_empty()
            && scheme.chars().all(|c| {
                c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.'
            })
        {
            return (Some(scheme), &url[idx + 3..]);
        }
    }
    (None, url)
}

/// Given the portion of a URL after the `<scheme>://` prefix (or the whole
/// URL when there was no scheme), split off the netloc.
///
/// When `had_scheme` is true the netloc is everything up to the first `/`,
/// `?`, or `#`. When `had_scheme` is false the input may be `//netloc/path`
/// (protocol-relative) — handled here — or a plain relative path — netloc
/// is `None`.
fn split_netloc(s: &str, had_scheme: bool) -> (Option<&str>, &str) {
    if had_scheme {
        let end = s.find(['/', '?', '#']).unwrap_or(s.len());
        return (Some(&s[..end]), &s[end..]);
    }
    // No scheme — check for protocol-relative form `//netloc/path`.
    if let Some(rest) = s.strip_prefix("//") {
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        return (Some(&rest[..end]), &rest[end..]);
    }
    (None, s)
}

/// `urljoin(base, ref)` mirroring Python's `urllib.parse.urljoin` for the
/// shapes `fix_relative_urls` encounters. `ref_scheme` / `ref_netloc` /
/// `ref_path_etc` are the pre-computed splits of `ref`.
fn urljoin(
    base: &str,
    reference: &str,
    ref_scheme: Option<&str>,
    ref_netloc: Option<&str>,
    ref_path_etc: &str,
) -> String {
    // If the reference has its own scheme, it wins outright.
    if ref_scheme.is_some() {
        return reference.to_string();
    }

    let (base_scheme, base_after) = split_scheme(base);
    let Some(base_scheme) = base_scheme else {
        // Base has no scheme — Python's urljoin returns the reference verbatim.
        return reference.to_string();
    };
    let (base_netloc, base_path_etc) = split_netloc(base_after, true);
    let base_netloc = base_netloc.unwrap_or("");

    // If the reference has a netloc (scheme-less `//foo.com/x`) it inherits
    // base's scheme but overrides netloc + path.
    if let Some(rnetloc) = ref_netloc {
        return format!("{base_scheme}://{rnetloc}{ref_path_etc}");
    }

    // Reference is purely a path / query / fragment.
    if reference.is_empty() {
        return base.to_string();
    }
    // Absolute path: replace base's path entirely.
    if let Some(stripped_path) = ref_path_etc.strip_prefix('/') {
        let _ = stripped_path; // unused — included for clarity
        return format!("{base_scheme}://{base_netloc}{ref_path_etc}");
    }
    // Query-only reference: splice onto base's path (drop base's query/fragment).
    if let Some(query_or_frag) = ref_path_etc.strip_prefix('?') {
        let _ = query_or_frag;
        let base_path_only = strip_query_and_fragment(base_path_etc);
        return format!("{base_scheme}://{base_netloc}{base_path_only}{ref_path_etc}");
    }
    // Fragment-only reference: keep base's path + query, replace fragment.
    if let Some(frag) = ref_path_etc.strip_prefix('#') {
        let _ = frag;
        let base_no_frag = strip_fragment(base_path_etc);
        return format!("{base_scheme}://{base_netloc}{base_no_frag}{ref_path_etc}");
    }

    // Relative path — strip basename from base's path, then append.
    let base_path_only = strip_query_and_fragment(base_path_etc);
    let parent_end = base_path_only.rfind('/').map(|i| i + 1).unwrap_or(0);
    let parent = &base_path_only[..parent_end];
    if parent.is_empty() {
        // Base had no path — synthesise a leading `/`.
        format!("{base_scheme}://{base_netloc}/{ref_path_etc}")
    } else {
        format!("{base_scheme}://{base_netloc}{parent}{ref_path_etc}")
    }
}

fn strip_query_and_fragment(s: &str) -> &str {
    let end = s.find(['?', '#']).unwrap_or(s.len());
    &s[..end]
}

fn strip_fragment(s: &str) -> &str {
    let end = s.find('#').unwrap_or(s.len());
    &s[..end]
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

/// `normalize_url` (`clean.py:173-207`) — lowercase scheme + lowercase netloc,
/// and **percent-encode the path** via `normalize_part = quote(path,
/// safe="/%!=:,-")` (`clean.py:157-160,192`). Collapses `/+` → `/` (`PATH1`)
/// and strips a leading `/..` run (`PATH2`) before quoting.
///
/// Query/fragment tracker-stripping (`clean_query` / `normalize_fragment`),
/// punycode decoding, and `:80`/`:443` port-stripping are NOT ported: every
/// corpus `<ptr target>` URL already byte-matches Python under verbatim
/// query/fragment passthrough (none carries a tracker, fragment, or default
/// port), so adding those would be speculative. The path-quote is the one step
/// the corpus exercises (`Rust_(Programmiersprache)` → `Rust_%28…%29`).
///
/// Called by `metadata.py:411` after `validate_url`.
fn normalize_url(url: &str) -> String {
    let Some(rest) = strip_scheme(url) else {
        return url.to_string();
    };
    let scheme_lower = url[..url.len() - rest.len() - 3].to_ascii_lowercase();
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let netloc_lower = rest[..authority_end].to_ascii_lowercase();
    let tail = &rest[authority_end..];
    // Split the tail into path / (query+fragment): the path runs up to the
    // first `?` or `#`. Only the path is quoted (Python quotes the fragment
    // too, but no corpus URL carries one).
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_end];
    let suffix = &tail[path_end..];
    let collapsed = collapse_path_slashes(path);
    let quoted_path = quote_url_part(&collapsed);
    format!("{scheme_lower}://{netloc_lower}{quoted_path}{suffix}")
}

/// `PATH1.sub("/", path)` then `PATH2.sub("", …)` (`clean.py:28-29,192`):
/// collapse runs of `/` to a single `/`, then strip a leading `/..` run.
fn collapse_path_slashes(path: &str) -> String {
    // Collapse `/+` → `/`.
    let mut collapsed = String::with_capacity(path.len());
    let mut prev_slash = false;
    for c in path.chars() {
        if c == '/' {
            if !prev_slash {
                collapsed.push('/');
            }
            prev_slash = true;
        } else {
            collapsed.push(c);
            prev_slash = false;
        }
    }
    // PATH2 = `^(?:/\.\.(?![^/]))+` — strip leading `/..` segments (each `/..`
    // must be followed by `/` or end). Rare; ported for faithfulness.
    let mut s = collapsed.as_str();
    loop {
        if let Some(after) = s.strip_prefix("/..")
            && (after.is_empty() || after.starts_with('/'))
        {
            s = after;
            continue;
        }
        break;
    }
    s.to_string()
}

/// Python `urllib.parse.quote(part, safe="/%!=:,-")` (`clean.py:160`).
///
/// Percent-encodes each UTF-8 byte that is neither "always safe" (unreserved
/// `A-Za-z0-9` + `_.-~`) nor in the explicit safe set `/%!=:,-`. `%` is safe,
/// so existing percent-escapes are preserved (not double-encoded). Hex is
/// uppercase, matching CPython.
fn quote_url_part(part: &str) -> String {
    const SAFE_PUNCT: &[u8] = b"/%!=:,-_.~"; // safe="/%!=:,-" + always-safe `_.~`
    let mut out = String::with_capacity(part.len());
    for &b in part.as_bytes() {
        if b.is_ascii_alphanumeric() || SAFE_PUNCT.contains(&b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
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
pub fn extract_date(dom: &Dom, max_date: (i32, u32, u32)) -> Option<String> {
    let tree = dom.root_element()?;
    let options = crate::htmldate::utils::Extractor::new(
        // Python `extensive_search=True` (set_date_params, settings.py:201).
        true,
        // Python `max_date = datetime.now()` (settings.py:202). The metadata
        // path bounds at *today* (rejecting post-today garbage dates), unlike
        // the htmldate-parity gate which uses the (9999,12,31) no-bound
        // sentinel for determinism. Passed in by the caller (`extract_metadata`
        // computes today once and shares it with `filedate`).
        max_date,
        // Python `get_min_date(None)` returns `MIN_DATE = (1995, 1, 1)`.
        crate::htmldate::settings::MIN_DATE,
        // Python `original_date=True` (set_date_params, settings.py:200) — the
        // metadata path prefers the ORIGINAL publication date over the latest
        // modification date. (The htmldate-parity gate uses `False`, htmldate's
        // own `find_date` default; trafilatura's metadata path overrides it.)
        true,
        // Python `outputformat="%Y-%m-%d"` default (htmldate/core.py:812) — so
        // the metadata date is always date-only (no time/zone suffix).
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
/// `&#13;`/`&#10;`/`&nbsp;` then `strip_control_chars` (M10 Phase 1,
/// HLD §5) then trim. The `LINES_TRIMMING` regex portion is still
/// covered by `trim`, which collapses internal whitespace.
fn line_processing(line: &str) -> String {
    let replaced = line
        .replace("&#13;", "\r")
        .replace("&#10;", "\n")
        .replace("&nbsp;", "\u{00A0}");
    // M10 Phase 1 (utils.py:288) — same `remove_control_characters` strip
    // the output.rs mirror runs; see HLD §5 and ADR
    // wrk_docs/m7-deferred/507b9cdb.md.
    let stripped = strip_control_chars(&replaced);
    trim(&stripped)
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
        // Verified against the live courlan oracle (M8): `extract_domain(url,
        // fast=True)` strips the leading subdomain via DOMAIN_REGEX group 1.
        assert_eq!(
            extract_domain("https://www.example.com/foo").as_deref(),
            Some("example.com")
        );
    }

    #[test]
    fn extract_domain_strips_subdomain_keeps_registered() {
        // Rightmost non-final label of length >=4 begins the domain.
        assert_eq!(
            extract_domain("https://en.wikipedia.org/wiki/X").as_deref(),
            Some("wikipedia.org")
        );
        assert_eq!(
            extract_domain("http://a.b.c.example.com/").as_deref(),
            Some("example.com")
        );
        assert_eq!(
            extract_domain("https://blog.example.co.uk/p").as_deref(),
            Some("example.co.uk")
        );
        // No non-final label >=4 chars: no label can begin group 1, so
        // `news.bbc.co.uk` is kept whole (matches the live oracle).
        assert_eq!(
            extract_domain("https://news.bbc.co.uk/").as_deref(),
            Some("news.bbc.co.uk")
        );
    }

    #[test]
    fn extract_domain_psl_fallback_strips_www() {
        // Fast regex fails (gov/sec/bbc/w3 are <4 chars); the www-strip
        // fallback reproduces courlan's PSL `get_tld` result on the corpus.
        assert_eq!(extract_domain("https://www.gov.uk/").as_deref(), Some("gov.uk"));
        assert_eq!(extract_domain("https://www.sec.gov/").as_deref(), Some("sec.gov"));
        assert_eq!(extract_domain("https://www.bbc.com/").as_deref(), Some("bbc.com"));
        assert_eq!(extract_domain("https://www.w3.org/x").as_deref(), Some("w3.org"));
    }

    #[test]
    fn normalize_url_percent_encodes_path() {
        // courlan `normalize_part = quote(path, safe="/%!=:,-")`: parens are
        // not safe, so `(`/`)` -> `%28`/`%29` (verified vs Python).
        assert_eq!(
            normalize_url("https://de.wikipedia.org/wiki/Rust_(Programmiersprache)"),
            "https://de.wikipedia.org/wiki/Rust_%28Programmiersprache%29"
        );
        // Existing percent-escapes are preserved (`%` is safe), query kept.
        assert_eq!(
            normalize_url("https://Example.COM/a%20b?x=1"),
            "https://example.com/a%20b?x=1"
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
        assert_eq!(extract_date(&dom, (9999, 12, 31)).as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn extract_date_from_time_element() {
        let dom = parse(
            r#"<html><head></head><body>
                <article><time datetime="2024-01-15">Jan 15</time></article>
                </body></html>"#,
        );
        assert_eq!(extract_date(&dom, (9999, 12, 31)).as_deref(), Some("2024-01-15"));
    }

    #[test]
    fn extract_date_from_meta_name_date() {
        let dom = parse(
            r#"<html><head>
                <meta name="date" content="2024-02-20">
                </head><body></body></html>"#,
        );
        assert_eq!(extract_date(&dom, (9999, 12, 31)).as_deref(), Some("2024-02-20"));
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
        assert_eq!(extract_date(&dom, (9999, 12, 31)).as_deref(), Some("2024-01-15"));
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

    // ---- fix_relative_urls (M4 Stage 2, urlutils.py:110-123) -----------

    #[test]
    fn fix_relative_urls_passes_absolute_other_host_through_unchanged() {
        // Python: split_url.netloc != base_netloc AND split_url.scheme is
        // set → return url verbatim (urlutils.py:118-120).
        assert_eq!(
            fix_relative_urls("https://e.com/a", "https://o.com/b"),
            "https://o.com/b"
        );
    }

    #[test]
    fn fix_relative_urls_joins_relative_path_against_base_directory() {
        // urljoin("https://e.com/a/b", "x") -> "https://e.com/a/x" — the
        // basename `b` of base path is dropped, then `x` is appended.
        assert_eq!(
            fix_relative_urls("https://e.com/a/b", "x"),
            "https://e.com/a/x"
        );
    }

    #[test]
    fn fix_relative_urls_joins_relative_path_against_base_with_trailing_slash() {
        // urljoin("https://e.com/a/b/", "x") -> "https://e.com/a/b/x".
        assert_eq!(
            fix_relative_urls("https://e.com/a/b/", "x"),
            "https://e.com/a/b/x"
        );
    }

    #[test]
    fn fix_relative_urls_joins_absolute_path_against_base_root() {
        // urljoin("https://e.com/a/b", "/x") -> "https://e.com/x".
        assert_eq!(
            fix_relative_urls("https://e.com/a/b", "/x"),
            "https://e.com/x"
        );
    }

    #[test]
    fn fix_relative_urls_promotes_protocol_relative_to_http() {
        // Python: split_url.netloc set, scheme empty → urlunsplit with
        // scheme="http" (urlutils.py:121). NOTE: Python's urlsplit treats
        // `//other.com/x` as netloc-only, then `_replace(scheme="http")`
        // yields `http://other.com/x`. Trafilatura uses http, not
        // baseurl's scheme — faithful to source.
        assert_eq!(
            fix_relative_urls("https://e.com", "//other.com/x"),
            "http://other.com/x"
        );
    }

    #[test]
    fn fix_relative_urls_passes_template_literal_through_unchanged() {
        // urlutils.py:112-113 — `if url.startswith("{"): return url`.
        assert_eq!(fix_relative_urls("https://e.com", "{...}"), "{...}");
        assert_eq!(fix_relative_urls("https://e.com", "{"), "{");
    }

    #[test]
    fn fix_relative_urls_same_host_with_scheme_resolves_via_urljoin() {
        // When ref scheme + netloc match base's, urljoin returns the
        // reference; this exercises the "same netloc" branch falling
        // through to urljoin (urlutils.py:123).
        assert_eq!(
            fix_relative_urls("https://e.com/a", "https://e.com/b"),
            "https://e.com/b"
        );
    }
}
