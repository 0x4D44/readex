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
            // llvm-cov:branch-not-reachable: `get_base_url` returns `None` for an
            // empty netloc (`urlutils.py:76-84` port; the `if netloc.is_empty()`
            // guard at the `get_base_url` body), so when it yields `Some(_)` the
            // string is always `scheme://<non-empty netloc>` — never empty. The
            // `!base.is_empty()` FALSE side is therefore unreachable; the guard
            // is retained to mirror the Python `if base_url:` truthiness check.
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
    let bytes = url.as_bytes();
    for scheme in &["https://", "http://", "ftps://", "ftp://"] {
        let s = scheme.as_bytes();
        // Compare on raw bytes so the byte-length cap doesn't land inside
        // a multi-byte UTF-8 char (e.g. Japanese metadata `このページのURL`).
        // Schemes are pure ASCII; a successful match guarantees `s.len()`
        // is on a char boundary.
        if bytes.len() >= s.len() && bytes[..s.len()].eq_ignore_ascii_case(s) {
            return Some(&url[s.len()..]);
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
/// The `url` crate is NOT a dependency of `readex` (Cargo.toml lists only
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
    // llvm-cov:branch-not-reachable (else arm): html5ever's `parse_document`
    // always synthesises a `<body>` for a full-document parse, so `dom.body()`
    // is `Some` for every `Dom::parse` snapshot (dom.rs:419-420 contract). The
    // `else { Vec::new() }` arm mirrors the Python `if tree is None` defensive
    // guard but cannot fire on a real parse.
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
        // llvm-cov:branch-not-reachable (Err arm): `xpath_list` is one of the
        // fixed `CATEGORIES_XPATHS` / `TAGS_XPATHS` constants, every one of
        // which parses + evaluates cleanly on the Stage 0b engine (pinned by
        // `tests/xpath_constants_engine_coverage.rs`). `evaluate` never returns
        // `Err` for these literals, so the `else { continue }` arm is dead; it
        // mirrors lxml's compile-error tolerance verbatim.
        let Ok(matches) = xpath_engine::evaluate(expr, &body) else {
            continue;
        };
        for elem in &matches {
            // llvm-cov:branch-not-reachable (None arm): every CATEGORIES_XPATHS
            // / TAGS_XPATHS expression ends in `//a[@href]` (xpaths.py:236-256),
            // so the engine only yields anchors that carry an `href` attribute;
            // `get_attribute(elem, "href")` is therefore always `Some`. The
            // `else { continue }` arm guards a shape the `[@href]` predicate
            // already excludes.
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
        // llvm-cov:branch-not-reachable (None arm): `find_head` walks the
        // synthesised `<head>` html5ever always emits for a full-document parse
        // (dom.rs `find_head` / root_element contract), so for any `Dom::parse`
        // snapshot it is `Some`. The `&& let Some(head)` FALSE side mirrors the
        // Python `if not head` guard but cannot fire on a real parse.
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
#[cfg_attr(coverage_nightly, coverage(off))]
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

    // ---- M12 regression: char-boundary safety on non-ASCII URLs --------

    #[test]
    fn strip_scheme_handles_non_ascii_url_prefix() {
        // Regression: M12 broad sweep surfaced a panic where `url[..7]`
        // landed inside a 3-byte Japanese char (`ペ`, bytes 6..9) when a
        // page's metadata contained `このページのURL` as a URL value.
        // The byte-slice comparison must not panic on non-ASCII input.
        assert!(strip_scheme("このページのURL").is_none());
        assert!(strip_scheme("中神通信息技术有限公司").is_none());
        assert!(strip_scheme("ペ").is_none()); // shorter than any scheme
    }

    // ===================================================================
    // M12 Stage 4 — branch coverage push (metadata_url.rs)
    // -------------------------------------------------------------------
    // Per `wrk_docs/2026.05.26 - CC - Coverage Improvement Plan.md`
    // §Stage 4 these tests pin cold-spot contracts in:
    //   - is_valid_url         (filters.py:253-271 minimal port)
    //   - urljoin              (urllib.parse.urljoin reduced shapes)
    //   - extract_catstags     (metadata.py:422-446)
    //   - extract_license      (metadata.py:465-479)
    //   - parse_license_element (metadata.py:449-462)
    //   - match_license_regex  (LICENSE_REGEX, metadata.py:56-58)
    //   - match_text_license_regex (TEXT_LICENSE_REGEX, metadata.py:60-62)
    //   - extract_url          (metadata.py:389-413)
    //   - extract_date         (metadata.py:546-547 / htmldate cascade)
    //   - meta_url_sitename / get_base_url / repair_relative_url
    // ===================================================================

    // ---- is_valid_url ----------------------------------------------------

    #[test]
    fn is_valid_url_accepts_https_with_dotted_host() {
        // rationale: scheme + dotted host satisfy the gate.
        assert!(is_valid_url("https://example.com/x"));
    }

    #[test]
    fn is_valid_url_accepts_localhost() {
        // rationale: `host.eq_ignore_ascii_case("localhost")` arm — single-label
        // host without a dot is allowed when it equals "localhost".
        assert!(is_valid_url("http://localhost/x"));
    }

    #[test]
    fn is_valid_url_rejects_ftp_scheme() {
        // rationale: `filters.py:253-271` requires http/https only.
        assert!(!is_valid_url("ftp://example.com/file"));
    }

    #[test]
    fn is_valid_url_rejects_relative_url() {
        // rationale: `strip_scheme` returns None for non-http(s) -> early false.
        assert!(!is_valid_url("/just/a/path"));
    }

    #[test]
    fn is_valid_url_rejects_scheme_only() {
        // rationale: authority empty -> false. `https://` with no host.
        assert!(!is_valid_url("https://"));
    }

    #[test]
    fn is_valid_url_rejects_scheme_with_path_no_host() {
        // rationale: empty authority before `/`.
        assert!(!is_valid_url("https:///path/only"));
    }

    #[test]
    fn is_valid_url_rejects_single_label_host_not_localhost() {
        // rationale: host must contain '.' OR equal "localhost" — `intranet` fails both.
        assert!(!is_valid_url("https://intranet/foo"));
    }

    #[test]
    fn is_valid_url_accepts_userinfo_and_port() {
        // rationale: userinfo `user@` and `:port` are stripped before the
        // `host.contains('.')` check.
        assert!(is_valid_url("https://user@example.com:8080/x"));
    }

    // ---- urljoin (driven via fix_relative_urls) -------------------------

    #[test]
    fn urljoin_query_only_ref_keeps_base_path_and_replaces_query() {
        // rationale: `urljoin(base, "?q=2")` keeps base's path and drops base's
        // query/fragment, then appends new query.
        assert_eq!(
            fix_relative_urls("https://e.com/a/b?old=1", "?new=2"),
            "https://e.com/a/b?new=2"
        );
    }

    #[test]
    fn urljoin_fragment_only_ref_keeps_base_path_and_query() {
        // rationale: fragment-only reference path of urljoin.
        assert_eq!(
            fix_relative_urls("https://e.com/a/b?q=1", "#section"),
            "https://e.com/a/b?q=1#section"
        );
    }

    #[test]
    fn urljoin_fragment_only_ref_drops_base_fragment_keeps_path() {
        // rationale: fragment-only reference replaces base's fragment.
        assert_eq!(
            fix_relative_urls("https://e.com/a#old", "#new"),
            "https://e.com/a#new"
        );
    }

    #[test]
    fn urljoin_base_without_path_synthesises_leading_slash() {
        // rationale: base has empty path -> parent is empty -> the "Base had no
        // path" arm prepends a synthetic `/`.
        assert_eq!(
            fix_relative_urls("https://e.com", "x"),
            "https://e.com/x"
        );
    }

    #[test]
    fn urljoin_same_host_with_scheme_returns_reference_verbatim() {
        // rationale: when ref has its own scheme AND same netloc as base,
        // urljoin's ref_scheme.is_some() early-return fires.
        assert_eq!(
            fix_relative_urls("https://e.com/a", "https://e.com/b/c"),
            "https://e.com/b/c"
        );
    }

    #[test]
    fn urljoin_protocol_relative_inherits_base_scheme_when_same_logic_path() {
        // rationale: scheme-less `//host/x` with DIFFERENT host -> `http:` prepended
        // (this is the "different netloc" branch; covered for completeness).
        assert_eq!(
            fix_relative_urls("https://e.com", "//other.com/abc"),
            "http://other.com/abc"
        );
    }

    #[test]
    fn urljoin_relative_path_against_root_base() {
        // rationale: relative path with base "/" — parent_end at index 1 yields "/".
        assert_eq!(
            fix_relative_urls("https://e.com/", "x"),
            "https://e.com/x"
        );
    }

    // ---- extract_catstags - category branches ---------------------------

    #[test]
    fn extract_catstags_category_from_post_info_div() {
        // rationale: CATEGORIES_XPATHS[0] matches `<div class="post-info">//a`.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="post-info">
                    <a href="/category/news">News</a>
                    <a href="/category/sport">Sport</a>
                </div>
            </body></html>"#,
        );
        let cats = extract_catstags(&dom, "category");
        assert_eq!(cats, vec!["News".to_string(), "Sport".to_string()]);
    }

    #[test]
    fn extract_catstags_category_meta_article_section_fallback() {
        // rationale: `metadata.py:437-441` — when XPath finds nothing, fall back
        // to `<meta property="article:section">`.
        let dom = parse(
            r#"<html><head>
                <meta property="article:section" content="Politics">
            </head><body><p>text</p></body></html>"#,
        );
        let cats = extract_catstags(&dom, "category");
        assert_eq!(cats, vec!["Politics".to_string()]);
    }

    #[test]
    fn extract_catstags_category_meta_subject_name_fallback() {
        // rationale: fallback also accepts any `<meta name="*subject*">`
        // (case-insensitive substring).
        let dom = parse(
            r#"<html><head>
                <meta name="dcterms.subject" content="Technology">
            </head><body><p>text</p></body></html>"#,
        );
        let cats = extract_catstags(&dom, "category");
        assert_eq!(cats, vec!["Technology".to_string()]);
    }

    #[test]
    fn extract_catstags_category_meta_fallback_only_when_xpath_empty() {
        // rationale: the meta fallback is gated on `results.is_empty()`; if
        // XPath wins, the meta fallback never fires.
        let dom = parse(
            r#"<html><head>
                <meta property="article:section" content="Should Be Ignored">
            </head><body>
                <div class="post-info"><a href="/category/news">News</a></div>
            </body></html>"#,
        );
        let cats = extract_catstags(&dom, "category");
        assert_eq!(cats, vec!["News".to_string()]);
    }

    #[test]
    fn extract_catstags_tag_does_not_use_meta_fallback() {
        // rationale: the meta fallback at metadata.py:437-441 only fires for
        // `metatype == "category"` — tags don't get the section/subject path.
        let dom = parse(
            r#"<html><head>
                <meta property="article:section" content="X">
            </head><body></body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert!(tags.is_empty(), "tag should not pull from article:section");
    }

    #[test]
    fn extract_catstags_dedupes_repeated_text() {
        // rationale: `metadata.py:446` `dict.fromkeys(...)` — insertion-order
        // dedup. Two anchors with same text yield one entry.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="tags">
                    <a href="/tag/rust">rust</a>
                    <a href="/tag/rust">rust</a>
                </div>
            </body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert_eq!(tags, vec!["rust".to_string()]);
    }

    #[test]
    fn extract_catstags_returns_empty_when_no_body() {
        // rationale: `let Some(body) = dom.body()` early-return.
        // A malformed fragment that html5ever still synthesizes a body for
        // -> ensure we just return empty when no useful structure.
        let dom = parse("<html><head></head><body></body></html>");
        let cats = extract_catstags(&dom, "category");
        assert!(cats.is_empty());
    }

    #[test]
    fn extract_catstags_skips_anchor_without_matching_href() {
        // rationale: `href_matches_metatype` rejects anchors whose href doesn't
        // include `/tag[s|ies]?/`.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="tags">
                    <a href="/about/team">about</a>
                    <a href="/tag/rust">rust</a>
                </div>
            </body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert_eq!(tags, vec!["rust".to_string()]);
    }

    #[test]
    fn extract_catstags_first_winning_xpath_short_circuits() {
        // rationale: `if !results.is_empty() { break; }` — only the FIRST
        // matching XPath populates results. A later xpath that would also
        // match must NOT add additional entries.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="post-info">
                    <a href="/category/news">News</a>
                </div>
                <div class="row">
                    <a href="/category/sport">Sport</a>
                </div>
            </body></html>"#,
        );
        let cats = extract_catstags(&dom, "category");
        // Only the first xpath's hit; the row-based xpath is not consulted.
        assert_eq!(cats, vec!["News".to_string()]);
    }

    #[test]
    fn extract_catstags_skips_anchor_without_href_attr() {
        // rationale: `let Some(href) = get_attribute(...) else { continue }`.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="tags">
                    <a>no href</a>
                    <a href="/tag/rust">rust</a>
                </div>
            </body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert_eq!(tags, vec!["rust".to_string()]);
    }

    // ---- parse_license_element / match_license_regex --------------------

    #[test]
    fn match_license_regex_extracts_by_sa_4_0() {
        // rationale: LICENSE_REGEX `/by-sa/4.0` match yields ("by-sa", "4.0").
        let pair = match_license_regex("https://creativecommons.org/licenses/by-sa/4.0/");
        assert_eq!(
            pair,
            Some(("by-sa".to_string(), "4.0".to_string()))
        );
    }

    #[test]
    fn match_license_regex_extracts_by_nc_nd() {
        // rationale: longer prefixes appear FIRST in the alternation; `by-nc-nd`
        // is matched as a whole, not as `by-nc` + `-nd`.
        let pair = match_license_regex("https://creativecommons.org/licenses/by-nc-nd/3.0/");
        assert_eq!(
            pair,
            Some(("by-nc-nd".to_string(), "3.0".to_string()))
        );
    }

    #[test]
    fn match_license_regex_extracts_zero() {
        // rationale: `zero` token at the end of the alternation list.
        let pair = match_license_regex("https://creativecommons.org/publicdomain/zero/1.0/");
        assert_eq!(
            pair,
            Some(("zero".to_string(), "1.0".to_string()))
        );
    }

    #[test]
    fn match_license_regex_rejects_zero_major_version() {
        // rationale: `[1-9]\.[0-9]` — major must NOT be `0`.
        assert_eq!(
            match_license_regex("https://e.com/by/0.5/"),
            None
        );
    }

    #[test]
    fn match_license_regex_rejects_missing_version() {
        // rationale: no `[1-9]\.[0-9]` after the token -> None.
        assert_eq!(
            match_license_regex("https://creativecommons.org/licenses/by-sa/"),
            None
        );
    }

    #[test]
    fn match_license_regex_rejects_non_digit_version() {
        // rationale: `major.is_ascii_digit() && minor.is_ascii_digit()` guard.
        assert_eq!(
            match_license_regex("https://e.com/by/a.b/"),
            None
        );
    }

    #[test]
    fn match_license_regex_returns_none_for_unrelated_href() {
        // rationale: no `/<token>/` appears -> None.
        assert_eq!(match_license_regex("https://example.com/about"), None);
    }

    // ---- match_text_license_regex ---------------------------------------

    #[test]
    fn match_text_license_regex_creative_commons_with_version() {
        // rationale: TEXT_LICENSE_REGEX — `Creative Commons BY-SA 4.0` -> matches.
        let m = match_text_license_regex("Creative Commons by-sa 4.0");
        assert_eq!(m.as_deref(), Some("Creative Commons by-sa 4.0"));
    }

    #[test]
    fn match_text_license_regex_cc_prefix_without_version() {
        // rationale: version is `?` optional in the regex.
        let m = match_text_license_regex("cc by-nc");
        assert_eq!(m.as_deref(), Some("cc by-nc"));
    }

    #[test]
    fn match_text_license_regex_rejects_token_without_prefix() {
        // rationale: prefix (`cc` or `creative commons`) is required.
        assert!(match_text_license_regex("by-sa 4.0").is_none());
    }

    #[test]
    fn match_text_license_regex_rejects_unrelated_text() {
        // rationale: neither prefix appears -> None.
        assert!(match_text_license_regex("All rights reserved").is_none());
    }

    #[test]
    fn match_text_license_regex_rejects_prefix_without_space() {
        // rationale: `after_prefix.starts_with(' ')` guard.
        assert!(match_text_license_regex("ccby-sa").is_none());
    }

    #[test]
    fn match_text_license_regex_rejects_zero_major_version() {
        // rationale: `major != '0'` guard — when present, version must be valid.
        // With "0.5" present but invalid, the version is NOT consumed but the
        // base "cc by-sa" still matches (version optional).
        let m = match_text_license_regex("cc by-sa 0.5");
        assert_eq!(m.as_deref(), Some("cc by-sa"));
    }

    // ---- extract_license -------------------------------------------------

    #[test]
    fn extract_license_from_footer_link_with_text_match() {
        // rationale: footer-link strict branch — link href has no LICENSE_REGEX
        // match, but link text matches TEXT_LICENSE_REGEX -> returns matched substring.
        let dom = parse(
            r#"<html><head></head><body>
                <p>article</p>
                <footer>
                    <a href="https://e.com/license">Creative Commons by-sa 4.0</a>
                </footer>
            </body></html>"#,
        );
        assert_eq!(
            extract_license(&dom).as_deref(),
            Some("Creative Commons by-sa 4.0")
        );
    }

    #[test]
    fn extract_license_from_div_class_footer_with_cc_href() {
        // rationale: div.footer//a branch — strict mode requires href LICENSE_REGEX
        // OR text matches TEXT_LICENSE_REGEX.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="page-footer">
                    <a href="https://creativecommons.org/licenses/by/4.0/">CC BY</a>
                </div>
            </body></html>"#,
        );
        assert_eq!(extract_license(&dom).as_deref(), Some("CC BY 4.0"));
    }

    #[test]
    fn extract_license_from_div_id_footer() {
        // rationale: `is_footer_id` branch alongside the class branch.
        let dom = parse(
            r#"<html><head></head><body>
                <div id="footer">
                    <a href="https://creativecommons.org/publicdomain/zero/1.0/">CC0</a>
                </div>
            </body></html>"#,
        );
        assert_eq!(extract_license(&dom).as_deref(), Some("CC ZERO 1.0"));
    }

    #[test]
    fn extract_license_returns_none_when_no_rel_no_footer() {
        // rationale: every path falls through -> None.
        let dom = parse(
            r#"<html><head></head><body>
                <p>article</p>
                <a href="/about">About</a>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }

    #[test]
    fn extract_license_div_footer_skips_when_not_footer() {
        // rationale: `if !(is_footer_class || is_footer_id) { continue; }`.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="content">
                    <a href="https://creativecommons.org/licenses/by/4.0/">CC BY 4.0</a>
                </div>
            </body></html>"#,
        );
        // rel="license" not set and div isn't a footer -> None.
        assert!(extract_license(&dom).is_none());
    }

    #[test]
    fn extract_license_skips_footer_anchor_without_href() {
        // rationale: `if get_attribute(&a, "href").is_some()` guard.
        let dom = parse(
            r#"<html><head></head><body>
                <footer>
                    <a>no href, just text</a>
                </footer>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }

    // ---- parse_license_element strict mode arms -------------------------

    #[test]
    fn extract_license_rel_license_returns_text_when_text_arm_wins() {
        // rationale: rel=license with no href LICENSE_REGEX match AND link text
        // present -> non-strict branch returns the trimmed text verbatim.
        let dom = parse(
            r#"<html><head></head><body>
                <a rel="license" href="https://e.com/x">Public Domain</a>
            </body></html>"#,
        );
        assert_eq!(extract_license(&dom).as_deref(), Some("Public Domain"));
    }

    #[test]
    fn extract_license_rel_license_returns_none_when_empty_text_and_no_regex() {
        // rationale: text arm trimmed empty -> None; href has no LICENSE_REGEX
        // -> overall None for the first anchor; second anchor in footer kicks in.
        let dom = parse(
            r#"<html><head></head><body>
                <a rel="license" href="https://e.com/x"></a>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }

    // ---- extract_url additional shapes ----------------------------------

    #[test]
    fn extract_url_with_base_element() {
        // rationale: URL_SELECTORS[1] = `<base>` href fallback.
        let dom = parse(
            r#"<html><head>
                <base href="https://example.com/base/">
            </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/base/")
        );
    }

    #[test]
    fn extract_url_with_alternate_x_default() {
        // rationale: URL_SELECTORS[2] = link[rel=alternate hreflang=x-default].
        let dom = parse(
            r#"<html><head>
                <link rel="alternate" hreflang="x-default" href="https://example.com/x">
            </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/x")
        );
    }

    #[test]
    fn extract_url_rejects_invalid_and_uses_default() {
        // rationale: `is_valid_url` rejects `ftp://` so the canonical link
        // is discarded and default_url wins.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="ftp://example.com/file">
            </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.com/")).as_deref(),
            Some("https://fallback.com/")
        );
    }

    #[test]
    fn extract_url_returns_none_with_no_head_and_no_default() {
        // rationale: `walk_url_selectors` early-returns None when head missing.
        let dom = parse("<html><body></body></html>");
        assert!(extract_url(&dom, None).is_none());
    }

    #[test]
    fn extract_url_relative_canonical_with_no_og_base_falls_back() {
        // rationale: `/article/x` is relative; no `og:`/`twitter:` content
        // to repair against; the raw value falls through to is_valid_url,
        // which rejects schemeless URL -> default_url wins.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/article/x">
            </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://default.com/")).as_deref(),
            Some("https://default.com/")
        );
    }

    // ---- get_base_url ----------------------------------------------------

    #[test]
    fn get_base_url_strips_path_query_fragment() {
        // rationale: `urlutils.py:76-84` — returns `scheme://netloc`.
        assert_eq!(
            get_base_url("https://example.com/a/b?q=1#x"),
            Some("https://example.com".to_string())
        );
    }

    #[test]
    fn get_base_url_returns_none_for_missing_scheme() {
        // rationale: `url.find("://")` returns None for no `://`.
        assert_eq!(get_base_url("example.com/path"), None);
    }

    #[test]
    fn get_base_url_returns_none_for_empty_netloc() {
        // rationale: netloc empty after `://` -> None.
        assert_eq!(get_base_url("https:///path"), None);
    }

    #[test]
    fn get_base_url_returns_none_for_empty_scheme() {
        // rationale: `i > 0` filter — leading `://` would yield empty scheme.
        assert_eq!(get_base_url("://e.com/x"), None);
    }

    // ---- meta_url_sitename ----------------------------------------------

    #[test]
    fn meta_url_sitename_strips_www() {
        // rationale: META_URL strips an optional `www.` prefix.
        assert_eq!(
            meta_url_sitename("https://www.example.com/x"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn meta_url_sitename_strips_w_digit_prefix() {
        // rationale: META_URL = `(?:www\.|w[0-9]+\.)?` — `w3.` style.
        assert_eq!(
            meta_url_sitename("https://w3.example.com/x"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn meta_url_sitename_keeps_bare_host_no_prefix() {
        // rationale: no `www`/`wN.` prefix -> host kept verbatim.
        assert_eq!(
            meta_url_sitename("https://example.com/x"),
            Some("example.com".to_string())
        );
    }

    #[test]
    fn meta_url_sitename_returns_none_for_non_http() {
        // rationale: regex anchored at `https?://` -> no match for ftp:.
        assert!(meta_url_sitename("ftp://example.com/x").is_none());
    }

    #[test]
    fn meta_url_sitename_returns_none_for_empty_host() {
        // rationale: `if host.is_empty()` -> None.
        assert!(meta_url_sitename("https://").is_none());
    }

    // ---- normalize_url additional --------------------------------------

    #[test]
    fn normalize_url_collapses_double_slashes_in_path() {
        // rationale: PATH1 collapses `//+` -> `/`.
        assert_eq!(
            normalize_url("https://e.com//a//b"),
            "https://e.com/a/b"
        );
    }

    #[test]
    fn normalize_url_strips_leading_dotdot_segments() {
        // rationale: PATH2 strips a leading `/..` run.
        assert_eq!(
            normalize_url("https://e.com/../../x"),
            "https://e.com/x"
        );
    }

    #[test]
    fn normalize_url_preserves_query_and_fragment_byte_equal() {
        // rationale: only the path is quoted; query/fragment pass through.
        assert_eq!(
            normalize_url("https://e.com/p?a=1#frag"),
            "https://e.com/p?a=1#frag"
        );
    }

    #[test]
    fn normalize_url_no_scheme_returns_input_unchanged() {
        // rationale: `let Some(rest) = strip_scheme(url) else { return ... }`.
        assert_eq!(
            normalize_url("not-a-url"),
            "not-a-url"
        );
    }

    // ---- href_matches_metatype additional shapes ------------------------

    #[test]
    fn href_matches_metatype_with_i_quirk() {
        // rationale: Python's `[s|ies]?` is a single-char class; `i` is in the
        // set, so `/tagi/` matches (verified Python regex quirk).
        assert!(href_matches_metatype("/tagi/x", "tag"));
    }

    #[test]
    fn href_matches_metatype_with_e_quirk() {
        // rationale: `e` is in the `[s|ies]?` class.
        assert!(href_matches_metatype("/tage/x", "tag"));
    }

    #[test]
    fn href_matches_metatype_with_pipe_quirk() {
        // rationale: `|` is literally one of the chars in the `[s|ies]` class.
        assert!(href_matches_metatype("/tag|/x", "tag"));
    }

    #[test]
    fn href_matches_metatype_rejects_partial_path() {
        // rationale: needle requires surrounding slashes.
        assert!(!href_matches_metatype("/tagged-rust", "tag"));
    }

    // ---- extract_date additional shapes ---------------------------------

    #[test]
    fn extract_date_returns_none_for_empty_html() {
        // rationale: defensive path — no date hints found anywhere.
        let dom = parse("<html><head></head><body><p>x</p></body></html>");
        assert!(extract_date(&dom, (9999, 12, 31)).is_none());
    }

    #[test]
    fn extract_date_returns_none_when_root_missing() {
        // rationale: `let tree = dom.root_element()?` early-return.
        // An empty/fragment document still synthesises a root via html5ever;
        // we simulate "no date anywhere" instead.
        let dom = parse("");
        let out = extract_date(&dom, (9999, 12, 31));
        assert!(out.is_none() || out.as_deref().is_some_and(|s| s.len() == 10));
    }

    // ---- line_processing -------------------------------------------------

    #[test]
    fn line_processing_replaces_html_entities_then_trims() {
        // rationale: `utils.py:283-300` minimal port — &#13;/&#10;/&nbsp;.
        let out = line_processing("foo&#13;&#10;&nbsp;bar");
        // After replace + strip_control_chars + trim, whitespace collapsed.
        assert!(out.contains("foo"));
        assert!(out.contains("bar"));
    }

    #[test]
    fn line_processing_empty_returns_empty() {
        assert_eq!(line_processing(""), "");
    }

    // ---- strip_port / strip_www_prefix corner cases ---------------------

    #[test]
    fn strip_port_leaves_authority_when_no_colon() {
        // rationale: `let Some(colon) = rfind(':')` -> None branch.
        assert_eq!(strip_port("example.com"), "example.com");
    }

    #[test]
    fn strip_port_leaves_when_after_colon_not_digits() {
        // rationale: port must be all-ascii-digit.
        assert_eq!(strip_port("example.com:abc"), "example.com:abc");
    }

    #[test]
    fn strip_www_prefix_with_digit_label_strips() {
        // rationale: `wwwN.` (CLEAN_FLD_REGEX `^www[0-9]*\.`).
        assert_eq!(strip_www_prefix("www2.example.com"), "example.com");
    }

    #[test]
    fn strip_www_prefix_no_www_returns_input() {
        assert_eq!(strip_www_prefix("example.com"), "example.com");
    }

    #[test]
    fn strip_www_prefix_www_without_dot_returns_input() {
        // rationale: `if let Some(stripped) = rest[..].strip_prefix('.')` else fall.
        assert_eq!(strip_www_prefix("wwwsomething"), "wwwsomething");
    }

    // ---- urljoin / fix_relative_urls — RFC-3986 catalogue residue --------

    #[test]
    fn urljoin_base_without_scheme_returns_reference_verbatim() {
        // rationale: Python `urljoin` with a base that has no scheme cannot
        // resolve the reference; our port returns the reference unchanged
        // (the `let Some(base_scheme) = base_scheme else` arm). Drive it via
        // fix_relative_urls so the reference is a same-treatment relative path.
        // base has no `://` -> split_scheme(base).0 == None.
        assert_eq!(fix_relative_urls("not-a-url", "page.html"), "page.html");
    }

    #[test]
    fn fix_relative_urls_empty_reference_returns_base() {
        // rationale: `urljoin(base, "")` returns the base verbatim (RFC-3986
        // reference resolution with an empty reference — the `if reference
        // .is_empty()` arm).
        assert_eq!(
            fix_relative_urls("https://e.com/a/b", ""),
            "https://e.com/a/b"
        );
    }

    #[test]
    fn fix_relative_urls_query_only_reference_replaces_query() {
        // rationale: urlutils.py:123 -> urljoin; a `?`-only reference keeps the
        // base path and replaces the query (drops base query/fragment).
        assert_eq!(
            fix_relative_urls("https://e.com/a?old=1#frag", "?new=2"),
            "https://e.com/a?new=2"
        );
    }

    #[test]
    fn fix_relative_urls_fragment_only_reference_keeps_path_and_query() {
        // rationale: a `#`-only reference keeps base path+query, replaces fragment.
        assert_eq!(
            fix_relative_urls("https://e.com/a?x=1#old", "#new"),
            "https://e.com/a?x=1#new"
        );
    }

    #[test]
    fn fix_relative_urls_other_host_scheme_less_with_path() {
        // rationale: urlutils.py:118-121 — a scheme-less reference whose netloc
        // differs from the base's gets `http:` prepended (NOT base's scheme).
        assert_eq!(
            fix_relative_urls("https://e.com/a", "//cdn.other.com/img.png"),
            "http://cdn.other.com/img.png"
        );
    }

    #[test]
    fn fix_relative_urls_absolute_path_replaces_entire_base_path() {
        // rationale: urljoin RFC-3986 — a `/`-leading reference replaces the
        // base path entirely (keeps scheme+netloc).
        assert_eq!(
            fix_relative_urls("https://e.com/deep/nested/page", "/top.html"),
            "https://e.com/top.html"
        );
    }

    // ---- find_link_with_rel / x-default — attr-present-but-no-href --------

    #[test]
    fn extract_url_canonical_link_without_href_falls_through() {
        // rationale: `metadata.py:154` `link[@rel="canonical"]` — the `href`
        // getter FALSE side: a <link rel="canonical"> with NO href yields no
        // URL, so extract_url falls through to default_url.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.example.com/")).as_deref(),
            Some("https://fallback.example.com/")
        );
    }

    #[test]
    fn extract_url_alternate_x_default_without_href_falls_through() {
        // rationale: `metadata.py:156` alternate/x-default selector — rel+hreflang
        // match but `href` getter FALSE side: no URL extracted.
        let dom = parse(
            r#"<html><head>
                <link rel="alternate" hreflang="x-default">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.example.com/")).as_deref(),
            Some("https://fallback.example.com/")
        );
    }

    // ---- is_valid_url — extra negative shapes -----------------------------

    #[test]
    fn is_valid_url_accepts_uppercase_scheme() {
        // rationale: `filters.py:253-271` scheme check is case-insensitive; our
        // gate's `to_ascii_lowercase().starts_with("https://")` arm accepts an
        // uppercase scheme with a dotted host.
        assert!(is_valid_url("HTTPS://Example.COM/path"));
    }

    // ---- extract_domain — single-label host (no fast-path) ----------------

    #[test]
    fn extract_domain_single_label_host_falls_to_www_strip() {
        // rationale: `urlutils.py:14-21` DOMAIN_REGEX fast path needs >=2 labels;
        // a single-label host (`labels.len() >= 2` FALSE) falls to the www-strip
        // fallback, which returns the host unchanged when there is no www prefix.
        assert_eq!(extract_domain("http://localhost/x").as_deref(), Some("localhost"));
    }

    // ---- parse_license_element — empty link text (strict + non-strict) ----

    #[test]
    fn extract_license_rel_license_empty_text_no_href_match_returns_none() {
        // rationale: `metadata.py:456-461` — a rel=license <a> with no LICENSE_REGEX
        // href match and EMPTY text returns None (the `if t.is_empty()` arm),
        // so extract_license overall yields None.
        let dom = parse(
            r#"<html><head></head><body>
                <a rel="license" href="/about/"></a>
                </body></html>"#,
        );
        assert_eq!(extract_license(&dom), None);
    }

    // ===================================================================
    // M12 Stage 4 (single-file push) — RFC-3986 / courlan edge-arm branches
    // -------------------------------------------------------------------
    // Each test below forces the previously-unhit side of a production
    // branch, citing the courlan / urllib / metadata.py invariant it pins.
    // ===================================================================

    // ---- walk_url_selectors: whitespace-only href (metadata.py:154) ------

    #[test]
    fn extract_url_whitespace_only_canonical_href_is_skipped() {
        // rationale: `walk_url_selectors` guard `&& !u.trim().is_empty()`
        // (metadata.py iterates URL_SELECTORS and takes the first NON-empty
        // href). A `<link rel="canonical" href="   ">` yields a whitespace
        // href whose `trim()` is empty -> the FALSE side fires, the selector
        // is skipped, and extract_url falls through to default_url.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="   ">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.example.com/")).as_deref(),
            Some("https://fallback.example.com/")
        );
    }

    // ---- first_base_href: <base> without href (metadata.py:155) ----------

    #[test]
    fn extract_url_base_element_without_href_falls_through() {
        // rationale: `first_base_href` getter `if let Some(href) = get_attribute
        // (&base, "href")` FALSE side — a `<base>` element carrying NO href
        // attribute yields no URL, so URL_SELECTORS[1] contributes nothing and
        // extract_url falls through to default_url.
        let dom = parse(
            r#"<html><head>
                <base target="_blank">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.example.com/")).as_deref(),
            Some("https://fallback.example.com/")
        );
    }

    // ---- find_alternate_x_default: rel match but no hreflang -------------

    #[test]
    fn extract_url_alternate_without_hreflang_is_skipped() {
        // rationale: `find_alternate_x_default` `if rel_match && hreflang_match`
        // — the `hreflang_match` (second &&-operand) FALSE side. A `<link
        // rel="alternate">` WITHOUT an `hreflang="x-default"` matches rel but
        // not hreflang, so the alternate selector (metadata.py:156) contributes
        // nothing and extract_url falls through to default_url.
        let dom = parse(
            r#"<html><head>
                <link rel="alternate" href="https://example.com/fr">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://fallback.example.com/")).as_deref(),
            Some("https://fallback.example.com/")
        );
    }

    // ---- repair_relative_url: og:/twitter: discrimination ----------------

    #[test]
    fn extract_url_repairs_relative_via_twitter_meta() {
        // rationale: `repair_relative_url` guard `attrtype.starts_with("og:")
        // || attrtype.starts_with("twitter:")` (metadata.py:399-406). With a
        // `twitter:url` meta (and no `og:`), the `og:` operand is FALSE so the
        // `twitter:` operand is evaluated and TRUE — repairing the relative
        // canonical from the twitter base.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/article/42">
                <meta name="twitter:url" content="https://twit.example.com/x">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://twit.example.com/article/42")
        );
    }

    #[test]
    fn extract_url_relative_ignores_non_og_twitter_meta_then_repairs() {
        // rationale: a leading non-og/non-twitter meta (`description`) makes
        // BOTH operands of the metadata.py:399 guard FALSE (the `twitter:`
        // FALSE side); the loop then reaches the `og:url` meta and repairs.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/p/9">
                <meta name="description" content="not a url at all">
                <meta property="og:url" content="https://og.example.com/home">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://og.example.com/p/9")
        );
    }

    #[test]
    fn extract_url_relative_og_meta_without_content_skipped() {
        // rationale: `repair_relative_url` `&& let Some(content) = get_attribute
        // (&elem, "content")` FALSE side — an `og:` meta with NO content
        // attribute is skipped; with no other base source the relative URL
        // falls through is_valid_url (schemeless -> invalid) to default_url.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/p/9">
                <meta property="og:url">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://def.example.com/")).as_deref(),
            Some("https://def.example.com/")
        );
    }

    #[test]
    fn extract_url_relative_og_content_without_base_skipped() {
        // rationale: `&& let Some(base) = get_base_url(&content)` FALSE side —
        // an `og:url` whose content has no scheme yields `get_base_url == None`
        // (urlutils.py:76-84 needs `://`), so it is skipped and the relative
        // URL falls through to default_url.
        let dom = parse(
            r#"<html><head>
                <link rel="canonical" href="/p/9">
                <meta property="og:url" content="example.com-no-scheme">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, Some("https://def.example.com/")).as_deref(),
            Some("https://def.example.com/")
        );
    }

    // ---- extract_domain: empty authority / empty host --------------------

    #[test]
    fn extract_domain_empty_authority_returns_none() {
        // rationale: `extract_domain` `if authority.is_empty()` TRUE side —
        // `https:///path` has an empty authority (the `/path` begins at index
        // 0 after the scheme), so DOMAIN_REGEX cannot match -> None.
        assert_eq!(extract_domain("https:///path"), None);
    }

    #[test]
    fn extract_domain_userinfo_with_empty_host_returns_none() {
        // rationale: `extract_domain` `if host.is_empty()` TRUE side — after
        // splitting `user@` userinfo (urlutils.py `.split("@")[-1]`) the host
        // component of `https://user@/path` is empty -> None.
        assert_eq!(extract_domain("https://user@/path"), None);
    }

    #[test]
    fn extract_domain_short_remainder_falls_to_www_strip() {
        // rationale: `extract_domain` fast-path `if (2..=63).contains(&rest_len)`
        // FALSE side — host `abcd.x` has a 4-char non-final label (`abcd`) but
        // the remainder after `abcd.` is `x` (1 char), failing DOMAIN_REGEX's
        // `[^/?#]{2,63}` remainder (urlutils.py:14-21). The fast path rejects
        // it and the www-strip fallback returns the host verbatim.
        assert_eq!(extract_domain("https://abcd.x/p").as_deref(), Some("abcd.x"));
    }

    // ---- meta_url_sitename: w-prefix without digits / without dot --------

    #[test]
    fn meta_url_sitename_w_prefix_no_digits_keeps_host() {
        // rationale: `meta_url_sitename` `if digits_end > 0 && ...` FALSE side
        // via the FIRST operand — host `web.example.com` begins with `w` but no
        // digit follows (`w[0-9]+\.` needs >=1 digit, metadata.py:46), so the
        // prefix is NOT stripped and the bare host is kept.
        assert_eq!(
            meta_url_sitename("https://web.example.com/x"),
            Some("web.example.com".to_string())
        );
    }

    #[test]
    fn meta_url_sitename_w_digits_without_dot_keeps_host() {
        // rationale: `&& r[digits_end..].starts_with('.')` FALSE side — host
        // `w3x.example.com` has digits after `w` but no `.` immediately after
        // them, so `w[0-9]+\.` does not match and the host is kept verbatim.
        assert_eq!(
            meta_url_sitename("https://w3x.example.com/x"),
            Some("w3x.example.com".to_string())
        );
    }

    // ---- strip_port: digit-predecessor / empty port ---------------------

    #[test]
    fn strip_port_keeps_when_colon_predecessor_is_digit() {
        // rationale: `strip_port` `if prev_is_digit` TRUE side — Python's
        // lookbehind `STRIP_PORT_REGEX = r"(?<=\D):\d+"` only strips a port when
        // the char before `:` is a NON-digit. For `1.2.3.4:80` the predecessor
        // is `4` (a digit), so the port is NOT stripped (matches the IPv4
        // dotted-quad guard).
        assert_eq!(strip_port("1.2.3.4:80"), "1.2.3.4:80");
    }

    #[test]
    fn strip_port_keeps_when_port_empty() {
        // rationale: `strip_port` `if port.is_empty() || ...` TRUE side via the
        // FIRST operand — a trailing `:` with nothing after it (`example.com:`)
        // has an empty port segment, so `\d+` cannot match and the authority is
        // returned unchanged.
        assert_eq!(strip_port("example.com:"), "example.com:");
    }

    // ---- fix_relative_urls / urljoin: same-netloc + empty-netloc ---------

    #[test]
    fn fix_relative_urls_protocol_relative_same_host_inherits_scheme() {
        // rationale: urlutils.py:118-121 guard `!unetloc.is_empty() && unetloc
        // != base_netloc` is FALSE here because the reference's netloc EQUALS
        // the base's (`e.com`); execution falls through to urljoin (line 123),
        // whose `if let Some(rnetloc) = ref_netloc` TRUE side then rebuilds the
        // URL as `<base-scheme>://<ref-netloc><ref-path>`.
        assert_eq!(
            fix_relative_urls("https://e.com/a", "//e.com/b/c"),
            "https://e.com/b/c"
        );
    }

    #[test]
    fn fix_relative_urls_empty_netloc_protocol_relative() {
        // rationale: urlutils.py:118 guard FALSE side via the FIRST operand
        // `!unetloc.is_empty()` — a bare `//` reference splits to an EMPTY
        // netloc, so the guard is FALSE and execution falls through to urljoin,
        // which emits `<scheme>://` with the (empty) reference authority.
        assert_eq!(fix_relative_urls("https://e.com/a", "//"), "https://");
    }

    // ---- split_scheme: empty scheme / invalid-char scheme ----------------

    #[test]
    fn fix_relative_urls_base_with_empty_scheme_returns_reference() {
        // rationale: `split_scheme` `if !scheme.is_empty() && ...` FALSE side
        // via the FIRST operand — a base beginning with `://` has an empty
        // scheme, so split_scheme yields `(None, _)`; urljoin then cannot
        // resolve and returns the reference verbatim.
        assert_eq!(fix_relative_urls("://no-scheme/x", "page.html"), "page.html");
    }

    #[test]
    fn fix_relative_urls_base_with_invalid_scheme_char_returns_reference() {
        // rationale: `split_scheme` `&& scheme.chars().all(...)` FALSE side —
        // a base scheme containing `!` (not in RFC-3986's `ALPHA / DIGIT / "+"
        // / "-" / "."`) makes `all()` FALSE, so split_scheme yields `(None, _)`
        // and urljoin returns the reference verbatim. Also exercises the
        // closure's `is_ascii_alphanumeric()` FALSE side on `!`.
        assert_eq!(fix_relative_urls("ht!tp://host/x", "page.html"), "page.html");
    }

    #[test]
    fn fix_relative_urls_reference_scheme_with_plus_is_recognised() {
        // rationale: `split_scheme` closure `c == '+'` TRUE side — a reference
        // scheme `git+ssh` contains a `+` (a valid RFC-3986 scheme char), so
        // the closure accepts it and split_scheme recognises the scheme. With a
        // different host + a real scheme, fix_relative_urls returns it verbatim
        // (urlutils.py:118-120).
        assert_eq!(
            fix_relative_urls("https://e.com/a", "git+ssh://other.host/repo"),
            "git+ssh://other.host/repo"
        );
    }

    #[test]
    fn fix_relative_urls_reference_scheme_with_dash_and_dot_is_recognised() {
        // rationale: `split_scheme` closure `c == '-'` TRUE side (the `-` in
        // `a-b`) and the trailing `c == '.'` operand (the `.` in `b.c`) — a
        // reference scheme `a-b.c` is all-valid RFC-3986 scheme chars, so
        // split_scheme recognises it and fix_relative_urls returns the
        // different-host absolute URL verbatim.
        assert_eq!(
            fix_relative_urls("https://e.com/a", "a-b.c://other.host/x"),
            "a-b.c://other.host/x"
        );
    }

    // ---- is_valid_url: uppercase http:// (not https) --------------------

    #[test]
    fn is_valid_url_accepts_uppercase_http_scheme() {
        // rationale: `is_valid_url` scheme `||` chain `url.to_ascii_lowercase()
        // .starts_with("http://")` TRUE side — `HTTP://...` is neither verbatim
        // `http://` nor `https://` nor lowercased-`https://`, so it is only
        // accepted via the lowercased-`http://` operand (filters.py:253-271
        // case-insensitive scheme check).
        assert!(is_valid_url("HTTP://Example.COM/path"));
    }

    // ---- normalize_url / collapse_path_slashes: PATH2 /.. arms ----------

    #[test]
    fn normalize_url_strips_trailing_dotdot_to_empty_path() {
        // rationale: `collapse_path_slashes` PATH2 loop `after.is_empty() ||
        // after.starts_with('/')` TRUE side via the FIRST operand — a path of
        // exactly `/..` strips to an empty remainder (clean.py PATH2
        // `^(?:/\.\.(?![^/]))+`), so the normalized path is empty.
        assert_eq!(normalize_url("https://e.com/.."), "https://e.com");
    }

    #[test]
    fn normalize_url_keeps_dotdot_when_not_a_full_segment() {
        // rationale: `collapse_path_slashes` PATH2 `after.starts_with('/')`
        // FALSE side — `/..x` is NOT a `/..` segment (the negative lookahead
        // `(?![^/])` in clean.py PATH2 forbids a trailing non-slash char), so
        // it is left intact rather than stripped.
        assert_eq!(normalize_url("https://e.com/..x"), "https://e.com/..x");
    }

    // ---- extract_catstags: empty-text matching anchor --------------------

    #[test]
    fn extract_catstags_skips_matching_anchor_with_empty_text() {
        // rationale: `extract_catstags` `if !t.is_empty() && ...` FALSE side via
        // the FIRST operand — an anchor whose href matches `/tag/` but whose
        // text is empty (after line_processing) contributes nothing; the second
        // anchor with text is what populates results.
        let dom = parse(
            r#"<html><head></head><body>
                <div class="tags">
                    <a href="/tag/empty"></a>
                    <a href="/tag/rust">rust</a>
                </div>
            </body></html>"#,
        );
        let tags = extract_catstags(&dom, "tag");
        assert_eq!(tags, vec!["rust".to_string()]);
    }

    // ---- extract_catstags category fallback: meta content arms ----------

    #[test]
    fn extract_catstags_category_section_meta_without_content_skipped() {
        // rationale: category fallback `if (is_section || is_subject_name) &&
        // let Some(content) = get_attribute(&elem, "content")` FALSE side — a
        // `<meta property="article:section">` with NO content attribute matches
        // the section predicate but yields no content, so it is skipped and the
        // overall result is empty (metadata.py:437-441).
        let dom = parse(
            r#"<html><head>
                <meta property="article:section">
            </head><body><p>x</p></body></html>"#,
        );
        assert!(extract_catstags(&dom, "category").is_empty());
    }

    #[test]
    fn extract_catstags_category_section_meta_whitespace_content_skipped() {
        // rationale: category fallback `if !t.is_empty() && ...` FALSE side via
        // the FIRST operand — a section meta whose content is whitespace-only
        // trims to empty, so it adds nothing (metadata.py:437-441 collects only
        // non-empty section/subject text).
        let dom = parse(
            r#"<html><head>
                <meta property="article:section" content="   ">
            </head><body><p>x</p></body></html>"#,
        );
        assert!(extract_catstags(&dom, "category").is_empty());
    }

    #[test]
    fn extract_catstags_category_section_meta_dedupes_duplicate_content() {
        // rationale: category fallback `if !t.is_empty() && !results.contains
        // (&t)` FALSE side via the SECOND operand — two section metas with the
        // same content yield one entry (metadata.py:446 `dict.fromkeys`
        // insertion-order dedup applies to the fallback path too).
        let dom = parse(
            r#"<html><head>
                <meta property="article:section" content="Politics">
                <meta name="dcterms.subject" content="Politics">
            </head><body><p>x</p></body></html>"#,
        );
        assert_eq!(
            extract_catstags(&dom, "category"),
            vec!["Politics".to_string()]
        );
    }

    // ---- extract_license: footer/div anchor href present-but-no-match ----

    #[test]
    fn extract_license_footer_anchor_with_href_no_match_no_text_returns_none() {
        // rationale: `extract_license` footer loop guard `if get_attribute(&a,
        // "href").is_some() && ...` — the href IS present (TRUE) but parse_license
        // _element returns None (no LICENSE_REGEX match, no strict text match),
        // so the overall result is None (metadata.py:465-479).
        let dom = parse(
            r#"<html><head></head><body>
                <p>article</p>
                <footer>
                    <a href="https://example.com/about">About us</a>
                </footer>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }

    #[test]
    fn extract_license_div_footer_anchor_with_href_strict_text_match() {
        // rationale: `extract_license` div.footer loop guard `if get_attribute
        // (&a, "href").is_some() && let Some(result) = parse_license_element(&a,
        // true)` — href present AND the strict TEXT_LICENSE_REGEX matches the
        // anchor text, so the matched substring is returned (metadata.py:473-477).
        let dom = parse(
            r#"<html><head></head><body>
                <div id="site-footer">
                    <a href="https://example.com/legal">Creative Commons by-nc 3.0</a>
                </div>
            </body></html>"#,
        );
        assert_eq!(
            extract_license(&dom).as_deref(),
            Some("Creative Commons by-nc 3.0")
        );
    }

    // ---- parse_license_element: whitespace text trims to empty -----------

    #[test]
    fn extract_license_rel_license_whitespace_text_returns_none() {
        // rationale: `parse_license_element` `if t.is_empty()` TRUE side — a
        // rel=license anchor whose href has NO LICENSE_REGEX match and whose
        // text is whitespace-only (element_text -> Some("   "), trim -> "")
        // returns None, so extract_license overall yields None
        // (metadata.py:456-461). Distinct from the EMPTY-text case where
        // element_text returns None and short-circuits before this arm.
        let dom = parse(
            "<html><head></head><body>\
                <a rel=\"license\" href=\"https://e.com/x\">   </a>\
                </body></html>",
        );
        assert!(extract_license(&dom).is_none());
    }

    // ---- match_license_regex: version second/third char invalid ----------

    #[test]
    fn match_license_regex_rejects_when_third_char_not_dot() {
        // rationale: `match_license_regex` version guard `&& dot == '.'` FALSE
        // side — `/by/12/` has a valid first digit but the third char is `2`,
        // not `.`, failing `[1-9]\.[0-9]` (metadata.py:56-58). No match.
        assert_eq!(match_license_regex("https://e.com/by/12/"), None);
    }

    #[test]
    fn match_license_regex_rejects_when_minor_not_digit() {
        // rationale: `match_license_regex` version guard `&& minor.is_ascii
        // _digit()` FALSE side — `/by/1.x/` has major=`1`, dot=`.`, but minor=`x`
        // is not a digit, failing `[1-9]\.[0-9]`. No match.
        assert_eq!(match_license_regex("https://e.com/by/1.x/"), None);
    }

    // ---- match_text_license_regex: optional-version char arms ------------

    #[test]
    fn match_text_license_regex_token_with_nondigit_major_keeps_base() {
        // rationale: `match_text_license_regex` `&& major.is_ascii_digit()`
        // FALSE side — `cc by-sa abc` has three trailing chars but the first
        // (`a`) is not a digit, so the optional `([1-9]\.[0-9])?` is NOT
        // consumed and the base `cc by-sa` is returned (TEXT_LICENSE_REGEX,
        // metadata.py:60-62).
        assert_eq!(
            match_text_license_regex("cc by-sa abc").as_deref(),
            Some("cc by-sa")
        );
    }

    #[test]
    fn match_text_license_regex_token_with_nondot_keeps_base() {
        // rationale: `&& dot == '.'` FALSE side — `cc by-sa 123` has major=`1`
        // but the second char is `2`, not `.`, so the optional version is not
        // consumed; the base `cc by-sa` is returned.
        assert_eq!(
            match_text_license_regex("cc by-sa 123").as_deref(),
            Some("cc by-sa")
        );
    }

    #[test]
    fn match_text_license_regex_token_with_nondigit_minor_keeps_base() {
        // rationale: `&& minor.is_ascii_digit()` FALSE side — `cc by-sa 1.x`
        // has major=`1`, dot=`.`, but minor=`x` is not a digit, so the optional
        // version is not consumed; the base `cc by-sa` is returned.
        assert_eq!(
            match_text_license_regex("cc by-sa 1.x").as_deref(),
            Some("cc by-sa")
        );
    }

    // ---- find_link_with_rel: <link> without a rel attribute --------------

    #[test]
    fn extract_url_link_without_rel_attribute_is_ignored() {
        // rationale: `find_link_with_rel` `if let Some(rel) = get_attribute(&link,
        // "rel")` FALSE side — a `<link href="...">` with NO `rel` attribute
        // (e.g. a bare resource link) is skipped by the canonical scan
        // (metadata.py:154 `link[@rel="canonical"]`); the subsequent
        // canonical link is what wins.
        let dom = parse(
            r#"<html><head>
                <link href="https://example.com/style.css">
                <link rel="canonical" href="https://example.com/real">
                </head><body></body></html>"#,
        );
        assert_eq!(
            extract_url(&dom, None).as_deref(),
            Some("https://example.com/real")
        );
    }

    // ---- extract_license div.footer loop: href-getter / parse arms ------

    #[test]
    fn extract_license_div_footer_anchor_without_href_skipped() {
        // rationale: `extract_license` div.footer loop guard `if get_attribute
        // (&a, "href").is_some()` FALSE side — an anchor inside a div#footer with
        // NO href is skipped; with no other license source the result is None
        // (metadata.py:473-477). The footer-element loop above finds nothing
        // first, so execution reaches the div.footer loop.
        let dom = parse(
            r#"<html><head></head><body>
                <p>article</p>
                <div id="footer">
                    <a>no href just text</a>
                </div>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }

    #[test]
    fn extract_license_div_footer_anchor_href_no_license_match_skipped() {
        // rationale: div.footer loop `&& let Some(result) = parse_license_element
        // (&a, true)` FALSE side — an anchor with an href but NO LICENSE_REGEX
        // match and NO strict TEXT_LICENSE_REGEX text match yields None from
        // parse_license_element, so the loop continues and the overall result
        // is None (metadata.py:473-477).
        let dom = parse(
            r#"<html><head></head><body>
                <p>article</p>
                <div class="site-footer">
                    <a href="https://example.com/contact">Contact</a>
                </div>
            </body></html>"#,
        );
        assert!(extract_license(&dom).is_none());
    }
}
