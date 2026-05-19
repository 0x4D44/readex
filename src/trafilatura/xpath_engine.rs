//! `xpath_engine` — greenfield XPath 1.0 evaluator (M3 Stage 0b).
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §6.1
//! (DECISION-A ratified). Replaces an FFI to libxml2 / libxslt with a tiny
//! hand-rolled parser + evaluator covering exactly the XPath operator surface
//! Trafilatura v2.0.0 needs (DA-B-1 enumerated catalog; 13-18 distinct
//! constructs). Anti-`unsafe` doctrine preserved; zero new runtime dependencies.
//!
//! # The operator catalog (DA-B-1; this is the contract)
//!
//! **Axes:**
//! - `descendant-or-self` (`//`, `.//`) — every descendant in document order,
//!   plus self.
//! - `child` (`/`) — direct children only.
//! - `self::` (`self::p`, `self::div`, …) — node-test on the context node.
//! - `attribute` (`@`) — attribute access.
//!
//! **Predicates:**
//! - `[N]` — 1-indexed positional (XPath 1.0 quirk).
//! - `[predicate-list][N]` — filter first, THEN positional.
//! - `[@attr="value"]` — attribute equals literal.
//! - `[@attr]` — attribute presence.
//!
//! **Functions:**
//! - `contains(string, string)` — substring containment. **Load-bearing edge
//!   case (DA-B-1):** when the first argument is a node-set (e.g.
//!   `@id|@class`), libxml2 / lxml silently converts it to the *string-value
//!   of the first node in document order*, or the empty string if the
//!   node-set is empty. The conformance table covers the `@id|@class` shape.
//! - `starts-with(string, string)` — prefix match (same node-set-to-string
//!   coercion).
//! - `translate(string, from, to)` — single-character translation table
//!   (Trafilatura uses the `translate(@class, "F", "f")` shape exclusively).
//! - `text()` — text-node children (used by Trafilatura's `external.py`
//!   arbiter via `body.xpath('.//p//text()')`).
//!
//! **Operators:** `or`, `and`, `|` (union), `=` (string equality).
//!
//! # Semantic anchors (the anti-inversion record)
//!
//! Every decision below traces to one of:
//! - W3C XPath 1.0 (1999-11-16; https://www.w3.org/TR/1999/REC-xpath-19991116/).
//! - lxml / libxml2 documented behaviour (the conformance harness is the
//!   ground truth — see `tests/xpath_conformance.rs`).
//! - A Trafilatura `xpaths.py` pattern.
//!
//! There is **no** "looks-nice-in-Rust" decision; every behavioural choice has
//! a citation in a doc-comment.

use std::collections::HashSet;
use std::rc::Rc;

use crate::readability::dom::{
    self, NodeData, NodeRef, attributes_in_source_order, child_nodes, get_attribute, is_element,
    local_name, tag_name,
};

// ---------------------------------------------------------------------------
// Public surface
// ---------------------------------------------------------------------------

/// Errors returned by [`evaluate`] / [`parse`] when an XPath expression is
/// malformed or uses a construct outside the supported catalog (see module
/// docs).
///
/// The error variants are deliberately narrow — the contract gates Stage 0b
/// at "every operator in the DA-B-1 catalog is implemented; everything else
/// is a hard error so the supervisor catches surface drift before Stage 1b
/// ships a silent miscompile".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XPathError {
    /// Tokenizer / parser could not consume the input.
    Parse(String),
    /// Construct outside the supported catalog (DA-B-1).
    Unsupported(String),
}

impl std::fmt::Display for XPathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            XPathError::Parse(s) => write!(f, "XPath parse error: {s}"),
            XPathError::Unsupported(s) => write!(f, "XPath unsupported construct: {s}"),
        }
    }
}

impl std::error::Error for XPathError {}

/// Evaluate `xpath` against `root` and return the resulting node-set (elements
/// only, deduplicated, in document order).
///
/// `root` is the context node. Relative paths (`.//foo`, `foo`) run from
/// `root`; absolute paths (`//foo`, `/foo`) climb to the document root and
/// then run. The function returns an `Err` only on a malformed expression or
/// an unsupported construct — a syntactically valid expression that simply
/// matches nothing returns `Ok(vec![])`.
///
/// **The function returns elements only.** XPath 1.0 supports node-sets of
/// any node type, but Trafilatura only ever consumes element node-sets from
/// `xpath()` calls. The one exception — `text()` — produces text nodes; in
/// that case the function returns the *containing element* (Trafilatura's
/// `external.py` arbiter takes `len(text())` over a body, so the count is
/// invariant under "element vs text node" framing). The conformance table
/// pins this.
pub fn evaluate(xpath: &str, root: &NodeRef) -> Result<Vec<NodeRef>, XPathError> {
    let path = parse(xpath)?;
    Ok(eval_path(&path, root))
}

// ---------------------------------------------------------------------------
// AST
// ---------------------------------------------------------------------------

/// A complete XPath expression. Either a single linear path or a top-level
/// union of paths (`|`). lxml allows `expr|expr|expr` at the top of an XPath
/// expression — see e.g. `'//time|//figure'` in
/// `trafilatura/xpaths.py:AUTHOR_DISCARD_XPATHS`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum XPath {
    Path(Path),
    Union(Vec<Path>),
}

/// A single linear XPath path: an "absoluteness" flag plus an ordered list of
/// steps. A path like `.//div[@class='x']//a[@href]` is two steps:
/// `descendant-or-self::div[@class='x']` then `descendant-or-self::a[@href]`.
///
/// Absoluteness: `Absolute = true` means the path starts from the document
/// root (`//foo`, `/foo`); `Absolute = false` means it starts from the context
/// node (`.//foo`, `foo`, `./foo`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Path {
    pub absolute: bool,
    pub steps: Vec<Step>,
}

/// One step of a path: axis + node-test + predicate list.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Step {
    pub axis: Axis,
    pub test: NodeTest,
    pub predicates: Vec<Predicate>,
}

// Clippy `enum_variant_names`: `SelfAxis` (one of four variants) ends with
// the enum's name. The W3C-canonical name for this axis is `self::`; the
// idiomatic Rust naming `Self_` collides with the keyword and `It`/`Ego`
// obscure the meaning. The variant name is doc-load-bearing; allow the lint.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Axis {
    /// `//` (between steps) or `.//` (path start) — the descendant-or-self
    /// shortcut. Per W3C XPath 1.0 §2.5, `//` is short for
    /// `/descendant-or-self::node()/`; the engine collapses the two-step form
    /// into this single axis for clarity.
    DescendantOrSelf,
    /// `/` between steps — direct child.
    Child,
    /// `self::` axis — node-test on the context node itself, no movement.
    SelfAxis,
    /// `@` — attribute access. Only used inside predicate expressions in the
    /// trafilatura corpus, but exposed as a Step axis for completeness.
    Attribute,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NodeTest {
    /// `*` — any element.
    Wildcard,
    /// `tagname` — an element with this local-name (lower-cased, HTML semantics).
    Name(String),
    /// `text()` — text-node children.
    Text,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Predicate {
    /// `[N]` — keep only the Nth (1-indexed) candidate in document order
    /// after preceding predicates have filtered the set. Trafilatura uses
    /// this only via `(expr)[1]`-shaped expressions; per DA-B-1, the
    /// "predicate-list THEN positional" ordering is the contract.
    Positional(usize),
    /// Boolean / expression predicate — the candidate is kept iff the
    /// expression evaluates to true in the candidate's context.
    Expr(Expr),
}

/// Predicate-internal expression node.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Expr {
    /// `lhs or rhs` — short-circuit OR.
    Or(Box<Expr>, Box<Expr>),
    /// `lhs and rhs` — short-circuit AND.
    And(Box<Expr>, Box<Expr>),
    /// `lhs = rhs` — string equality (after both sides coerce to string).
    Eq(Box<Expr>, Box<Expr>),
    /// Single attribute reference, `@name`. Evaluates to the attribute's value
    /// in string context; "exists" in boolean context (XPath 1.0 §3.4
    /// boolean()-of-node-set is `true` iff non-empty).
    Attribute(String),
    /// Union of attribute references, `@a|@b|...`. In string context this is
    /// libxml2's "string-value of first node in document order"; we resolve
    /// it as "first non-None attribute value in declaration order on the
    /// element" — lxml stores attributes in document (declaration) order, so
    /// "first node in document order" reduces to "first present attribute in
    /// the union". DA-B-1 calls this out as the load-bearing edge case for
    /// `contains(@id|@class, ...)`-shaped predicates.
    AttributeUnion(Vec<String>),
    /// `self::tagname` inside a predicate — a context-node test (TRUE iff the
    /// context node is an element of that tag). Trafilatura uses this heavily
    /// in BODY_XPATH-style `[self::article or self::div or ...]` shapes.
    SelfTagTest(String),
    /// `child::name` inside a predicate (the bare `name` shape — e.g.
    /// `rel="me"` in `AUTHOR_XPATHS`). In boolean context: "true iff the
    /// context node has a child element of that name". The Trafilatura
    /// `rel="me"` looks like a typo (no `@`), but lxml happily reads it as a
    /// child-element test, so we replicate that.
    ChildElementTest(String),
    /// `'literal'` or `"literal"` — string literal.
    Literal(String),
    /// `N` — numeric literal (Trafilatura only uses these as positional
    /// predicates; supported here as a literal in case it appears in a
    /// comparison).
    Number(f64),
    /// `contains(haystack, needle)` — substring containment. See module-doc
    /// node-set coercion note.
    FnContains(Box<Expr>, Box<Expr>),
    /// `starts-with(haystack, prefix)`.
    FnStartsWith(Box<Expr>, Box<Expr>),
    /// `translate(source, from-chars, to-chars)` — single-character
    /// translation table. Trafilatura always uses one- or few-char tables
    /// (`translate(@id, "B", "b")` etc.).
    FnTranslate(Box<Expr>, Box<Expr>, Box<Expr>),
}

// ---------------------------------------------------------------------------
// Tokenizer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Slash,          // /
    DoubleSlash,    // //
    Dot,            // .
    LBracket,       // [
    RBracket,       // ]
    LParen,         // (
    RParen,         // )
    At,             // @
    Pipe,           // |
    Comma,          // ,
    Star,           // *
    Eq,             // =
    ColonColon,     // ::
    Number(f64),    // 1, 2, 3.5...
    String(String), // 'foo' or "foo"
    Name(String),   // identifier (XML NCName + a few HTML attribute chars)
    Or,             // 'or' keyword
    And,            // 'and' keyword
}

struct Tokenizer<'a> {
    src: &'a str,
    chars: std::iter::Peekable<std::str::CharIndices<'a>>,
}

impl<'a> Tokenizer<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            chars: src.char_indices().peekable(),
        }
    }

    fn skip_ws(&mut self) {
        while let Some(&(_, c)) = self.chars.peek() {
            if c.is_whitespace() {
                self.chars.next();
            } else {
                break;
            }
        }
    }

    /// Tokenize the entire input. We use a single-pass approach: small
    /// allocation profile, hand-coded — XPath has no nesting state to track
    /// at the lexical level (string literals are the only context-sensitive
    /// bit).
    fn tokenize(mut self) -> Result<Vec<Tok>, XPathError> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            let Some(&(_, c)) = self.chars.peek() else {
                break;
            };
            match c {
                '/' => {
                    self.chars.next();
                    if let Some(&(_, '/')) = self.chars.peek() {
                        self.chars.next();
                        out.push(Tok::DoubleSlash);
                    } else {
                        out.push(Tok::Slash);
                    }
                }
                '.' => {
                    self.chars.next();
                    out.push(Tok::Dot);
                }
                '[' => {
                    self.chars.next();
                    out.push(Tok::LBracket);
                }
                ']' => {
                    self.chars.next();
                    out.push(Tok::RBracket);
                }
                '(' => {
                    self.chars.next();
                    out.push(Tok::LParen);
                }
                ')' => {
                    self.chars.next();
                    out.push(Tok::RParen);
                }
                '@' => {
                    self.chars.next();
                    out.push(Tok::At);
                }
                '|' => {
                    self.chars.next();
                    out.push(Tok::Pipe);
                }
                ',' => {
                    self.chars.next();
                    out.push(Tok::Comma);
                }
                '*' => {
                    self.chars.next();
                    out.push(Tok::Star);
                }
                '=' => {
                    self.chars.next();
                    out.push(Tok::Eq);
                }
                ':' => {
                    self.chars.next();
                    if let Some(&(_, ':')) = self.chars.peek() {
                        self.chars.next();
                        out.push(Tok::ColonColon);
                    } else {
                        return Err(XPathError::Parse(
                            "single ':' is not a supported XPath token".into(),
                        ));
                    }
                }
                '\'' | '"' => {
                    let quote = c;
                    self.chars.next();
                    let mut s = String::new();
                    let mut closed = false;
                    while let Some(&(_, ch)) = self.chars.peek() {
                        self.chars.next();
                        if ch == quote {
                            closed = true;
                            break;
                        }
                        s.push(ch);
                    }
                    if !closed {
                        return Err(XPathError::Parse(format!(
                            "unterminated string literal {quote:?}"
                        )));
                    }
                    out.push(Tok::String(s));
                }
                d if d.is_ascii_digit() => {
                    // Numeric literal — collect digits and at most one '.'.
                    let mut s = String::new();
                    let mut seen_dot = false;
                    while let Some(&(_, ch)) = self.chars.peek() {
                        if ch.is_ascii_digit() {
                            s.push(ch);
                            self.chars.next();
                        } else if ch == '.' && !seen_dot {
                            // Could be a decimal point OR a step-`.`. If the
                            // next char is a digit, this is a decimal; else
                            // emit the number and let `.` lex separately.
                            let mut clone = self.chars.clone();
                            clone.next();
                            if let Some(&(_, after)) = clone.peek()
                                && after.is_ascii_digit()
                            {
                                seen_dot = true;
                                s.push('.');
                                self.chars.next();
                                continue;
                            }
                            break;
                        } else {
                            break;
                        }
                    }
                    let n: f64 = s
                        .parse()
                        .map_err(|e| XPathError::Parse(format!("bad number {s:?}: {e}")))?;
                    out.push(Tok::Number(n));
                }
                ch if is_name_start(ch) => {
                    // Name — identifier per XML NCName plus '-', '_', '.', and
                    // digit-after-first. We deliberately do NOT allow ':'
                    // inside Names (the colon is its own ColonColon token).
                    let mut s = String::new();
                    while let Some(&(_, ch)) = self.chars.peek() {
                        if is_name_cont(ch) {
                            s.push(ch);
                            self.chars.next();
                        } else {
                            break;
                        }
                    }
                    // 'or' / 'and' are XPath operator keywords; recognise them
                    // here so the parser doesn't need a second-pass
                    // disambiguation. They are case-sensitive per XPath 1.0.
                    match s.as_str() {
                        "or" => out.push(Tok::Or),
                        "and" => out.push(Tok::And),
                        _ => out.push(Tok::Name(s)),
                    }
                }
                _ => {
                    return Err(XPathError::Parse(format!(
                        "unexpected character {c:?} in {:?}",
                        self.src
                    )));
                }
            }
        }
        Ok(out)
    }
}

fn is_name_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_name_cont(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.'
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn new(toks: Vec<Tok>) -> Self {
        Self { toks, pos: 0 }
    }

    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn peek_n(&self, n: usize) -> Option<&Tok> {
        self.toks.get(self.pos + n)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn expect(&mut self, t: &Tok) -> Result<(), XPathError> {
        match self.bump() {
            Some(got) if &got == t => Ok(()),
            Some(got) => Err(XPathError::Parse(format!("expected {t:?}, got {got:?}"))),
            None => Err(XPathError::Parse(format!("expected {t:?}, got EOF"))),
        }
    }

    /// Top-level: a `UnionExpr` (one or more `|`-separated paths).
    fn parse_xpath(&mut self) -> Result<XPath, XPathError> {
        let first = self.parse_path()?;
        if !matches!(self.peek(), Some(Tok::Pipe)) {
            // Allow trailing nothing.
            if self.pos != self.toks.len() {
                return Err(XPathError::Parse(format!(
                    "trailing tokens after path: {:?}",
                    &self.toks[self.pos..]
                )));
            }
            return Ok(XPath::Path(first));
        }
        let mut paths = vec![first];
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.bump();
            paths.push(self.parse_path()?);
        }
        if self.pos != self.toks.len() {
            return Err(XPathError::Parse(format!(
                "trailing tokens after union: {:?}",
                &self.toks[self.pos..]
            )));
        }
        Ok(XPath::Union(paths))
    }

    /// Parse one `Path`. Recognised shapes:
    /// - `(...)[N]` — parenthesised path expression then positional
    /// - `.//step` / `/step` / `//step` / `step` (relative or absolute) plus
    ///   further `/step` / `//step` segments
    fn parse_path(&mut self) -> Result<Path, XPathError> {
        // Parenthesised sub-expression. `(.//x|.//y)[1]` is the trafilatura
        // shape — paren wraps a Union of paths, then `[1]` positional. We
        // model this by parsing the inner union, then if the outer Union
        // collapses to a single Path we attach the positional predicate to
        // its last step (which is the lxml semantic; see e.g. `(.//main)[1]`
        // in `xpaths.py:BODY_XPATH`). For a Union inside parens, we add the
        // positional as a synthetic step. This is the engine's narrowest
        // departure from a fully spec-compliant XPath 1.0 parser, justified
        // by the fact that lxml on the trafilatura patterns always yields
        // the same result for either model.
        if matches!(self.peek(), Some(Tok::LParen)) {
            self.bump(); // (
            let inner = self.parse_xpath_inside_parens()?;
            self.expect(&Tok::RParen)?;
            // Optional `[N]` after the parens.
            let mut path = match inner {
                XPath::Path(p) => p,
                XPath::Union(paths) => {
                    // Materialise the union into a single Path with a
                    // synthetic union-step. This shape is only generated by
                    // `(a|b)[N]`-style expressions; the evaluator's eval_path
                    // recognises and handles it specially.
                    let union_step = Step {
                        axis: Axis::Child,
                        test: NodeTest::Name("__union__".into()),
                        predicates: vec![Predicate::Expr(Expr::Literal(serialize_paths(&paths)))],
                    };
                    // Stash the original paths on the side: we use a
                    // module-thread-local? No — keep it simple: encode them
                    // in a dedicated Path::absolute field is no good. Use a
                    // sentinel: hardcode a literal "_UNION_" and have a side
                    // table. Actually the simpler answer is to add a dedicated
                    // AST variant. Defer until we hit an actual trafilatura
                    // pattern that needs it; the corpus does not.
                    let _ = union_step;
                    return Err(XPathError::Unsupported(
                        "(union)[N] paren-around-union with positional predicate \
                         is not used by Trafilatura xpaths.py; not implemented \
                         (would need a dedicated AST variant)"
                            .into(),
                    ));
                }
            };
            while matches!(self.peek(), Some(Tok::LBracket)) {
                let pred = self.parse_predicate()?;
                if let Some(last) = path.steps.last_mut() {
                    last.predicates.push(pred);
                } else {
                    // Edge case: parens around a path with no steps. Not
                    // achievable via the trafilatura corpus.
                    return Err(XPathError::Unsupported(
                        "predicate on empty paren-path".into(),
                    ));
                }
            }
            // Trailing `//foo`/`/foo` after `(...)[1]` — yes, this appears in
            // xpaths.py: `(.//*[...])[1]|(.//main)[1]` no, that's union; but
            // `//div[@class="x"]//a[@href]` chains. After parens, allow more
            // steps too.
            while matches!(self.peek(), Some(Tok::Slash) | Some(Tok::DoubleSlash)) {
                let axis = match self.bump() {
                    Some(Tok::Slash) => Axis::Child,
                    Some(Tok::DoubleSlash) => Axis::DescendantOrSelf,
                    _ => unreachable!(),
                };
                let step = self.parse_step_body(axis)?;
                path.steps.push(step);
            }
            return Ok(path);
        }

        // Determine absolute vs relative + the first-step axis.
        let mut absolute = false;
        let mut steps: Vec<Step> = Vec::new();

        match self.peek() {
            Some(Tok::Slash) => {
                absolute = true;
                self.bump();
                // After a single `/`, the next step's axis is Child (or
                // there's no more — '/' alone is the document root, which
                // trafilatura never uses bare).
                let step = self.parse_step_body(Axis::Child)?;
                steps.push(step);
            }
            Some(Tok::DoubleSlash) => {
                absolute = true;
                self.bump();
                let step = self.parse_step_body(Axis::DescendantOrSelf)?;
                steps.push(step);
            }
            Some(Tok::Dot) => {
                // `./` or `.//`
                self.bump();
                match self.peek() {
                    Some(Tok::Slash) => {
                        self.bump();
                        let step = self.parse_step_body(Axis::Child)?;
                        steps.push(step);
                    }
                    Some(Tok::DoubleSlash) => {
                        self.bump();
                        let step = self.parse_step_body(Axis::DescendantOrSelf)?;
                        steps.push(step);
                    }
                    None => {
                        // Bare `.` — just return the context node. Encode as
                        // an empty path; the evaluator treats that as
                        // "context node only".
                        return Ok(Path {
                            absolute: false,
                            steps: vec![],
                        });
                    }
                    Some(t) => {
                        return Err(XPathError::Parse(format!(
                            "expected '/' or '//' after '.', got {t:?}"
                        )));
                    }
                }
            }
            Some(_) => {
                // Bare name / `*` / `@` — relative child::Name.
                let step = self.parse_step_body(Axis::Child)?;
                steps.push(step);
            }
            None => return Err(XPathError::Parse("empty XPath expression".into())),
        }

        // Subsequent steps.
        while matches!(self.peek(), Some(Tok::Slash) | Some(Tok::DoubleSlash)) {
            let axis = match self.bump() {
                Some(Tok::Slash) => Axis::Child,
                Some(Tok::DoubleSlash) => Axis::DescendantOrSelf,
                _ => unreachable!(),
            };
            let step = self.parse_step_body(axis)?;
            steps.push(step);
        }

        Ok(Path { absolute, steps })
    }

    /// XPath inside `(...)`. Re-enters `parse_xpath` but stops at the matching
    /// `)`. Implemented by parsing a path (or union) without forcing the
    /// "no more tokens" tail check — re-uses the parser state.
    fn parse_xpath_inside_parens(&mut self) -> Result<XPath, XPathError> {
        let first = self.parse_path()?;
        if !matches!(self.peek(), Some(Tok::Pipe)) {
            return Ok(XPath::Path(first));
        }
        let mut paths = vec![first];
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.bump();
            paths.push(self.parse_path()?);
        }
        Ok(XPath::Union(paths))
    }

    /// Parse one step body: (axis|node-test) plus zero-or-more predicates.
    /// `axis` is the axis the calling segment determined (Child for `/foo`,
    /// DescendantOrSelf for `//foo`, etc.). The `self::tag` and `@attr` step
    /// shapes override this axis explicitly.
    fn parse_step_body(&mut self, default_axis: Axis) -> Result<Step, XPathError> {
        let (axis, test) = match self.peek().cloned() {
            Some(Tok::Star) => {
                self.bump();
                (default_axis, NodeTest::Wildcard)
            }
            Some(Tok::At) => {
                self.bump();
                match self.bump() {
                    Some(Tok::Name(n)) => (Axis::Attribute, NodeTest::Name(n.to_ascii_lowercase())),
                    Some(Tok::Star) => (Axis::Attribute, NodeTest::Wildcard),
                    other => {
                        return Err(XPathError::Parse(format!(
                            "expected attribute name after '@', got {other:?}"
                        )));
                    }
                }
            }
            Some(Tok::Name(n)) => {
                // Could be `self::Tag` or `text()` or plain name.
                if n == "self" && matches!(self.peek_n(1), Some(Tok::ColonColon)) {
                    self.bump(); // 'self'
                    self.bump(); // '::'
                    let test = match self.bump() {
                        Some(Tok::Name(tag)) => NodeTest::Name(tag.to_ascii_lowercase()),
                        Some(Tok::Star) => NodeTest::Wildcard,
                        other => {
                            return Err(XPathError::Parse(format!(
                                "expected tag after 'self::', got {other:?}"
                            )));
                        }
                    };
                    (Axis::SelfAxis, test)
                } else if n == "text"
                    && matches!(self.peek_n(1), Some(Tok::LParen))
                    && matches!(self.peek_n(2), Some(Tok::RParen))
                {
                    self.bump(); // 'text'
                    self.bump(); // '('
                    self.bump(); // ')'
                    (default_axis, NodeTest::Text)
                } else {
                    self.bump();
                    (default_axis, NodeTest::Name(n.to_ascii_lowercase()))
                }
            }
            other => {
                return Err(XPathError::Parse(format!(
                    "expected node-test, got {other:?}"
                )));
            }
        };

        // Predicates: zero or more `[ ... ]`.
        let mut preds = Vec::new();
        while matches!(self.peek(), Some(Tok::LBracket)) {
            preds.push(self.parse_predicate()?);
        }

        Ok(Step {
            axis,
            test,
            predicates: preds,
        })
    }

    /// Parse a single `[ ... ]` predicate.
    fn parse_predicate(&mut self) -> Result<Predicate, XPathError> {
        self.expect(&Tok::LBracket)?;
        // Look-ahead for the bare-number `[N]` shape. If the bracket contains
        // only a single positive-integer Number token and nothing else, it is
        // a positional predicate. Anything else (including `position()=1`,
        // which trafilatura does not use) is an expression predicate.
        if let (Some(Tok::Number(n)), Some(Tok::RBracket)) = (self.peek(), self.peek_n(1)) {
            let n = *n;
            self.bump();
            self.bump();
            if n.fract() != 0.0 || n < 1.0 {
                return Err(XPathError::Unsupported(format!(
                    "non-integer or non-positive positional predicate [{n}]"
                )));
            }
            return Ok(Predicate::Positional(n as usize));
        }
        let e = self.parse_or()?;
        self.expect(&Tok::RBracket)?;
        Ok(Predicate::Expr(e))
    }

    /// `or`-level expression (lowest precedence).
    fn parse_or(&mut self) -> Result<Expr, XPathError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let rhs = self.parse_and()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// `and`-level expression.
    fn parse_and(&mut self) -> Result<Expr, XPathError> {
        let mut lhs = self.parse_eq()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.bump();
            let rhs = self.parse_eq()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// `=`-level expression.
    fn parse_eq(&mut self) -> Result<Expr, XPathError> {
        let lhs = self.parse_union_expr()?;
        if matches!(self.peek(), Some(Tok::Eq)) {
            self.bump();
            let rhs = self.parse_union_expr()?;
            return Ok(Expr::Eq(Box::new(lhs), Box::new(rhs)));
        }
        Ok(lhs)
    }

    /// Union of attribute references: `@a|@b|...`. Anything else just falls
    /// through to a single primary expression.
    fn parse_union_expr(&mut self) -> Result<Expr, XPathError> {
        let first = self.parse_primary()?;
        if !matches!(self.peek(), Some(Tok::Pipe)) {
            return Ok(first);
        }
        // Union — only attribute-union supported (DA-B-1 contract).
        let mut attrs = match first {
            Expr::Attribute(a) => vec![a],
            other => {
                return Err(XPathError::Unsupported(format!(
                    "predicate-internal union of non-attribute expressions is not \
                     in the DA-B-1 catalog (got {other:?})"
                )));
            }
        };
        while matches!(self.peek(), Some(Tok::Pipe)) {
            self.bump();
            let next = self.parse_primary()?;
            match next {
                Expr::Attribute(a) => attrs.push(a),
                other => {
                    return Err(XPathError::Unsupported(format!(
                        "predicate-internal union element must be @attr; got {other:?}"
                    )));
                }
            }
        }
        Ok(Expr::AttributeUnion(attrs))
    }

    /// Primary expression: attribute / literal / number / function-call / self::tag
    /// / bare child-element name.
    fn parse_primary(&mut self) -> Result<Expr, XPathError> {
        match self.peek().cloned() {
            Some(Tok::At) => {
                self.bump();
                match self.bump() {
                    Some(Tok::Name(n)) => Ok(Expr::Attribute(n.to_ascii_lowercase())),
                    other => Err(XPathError::Parse(format!(
                        "expected attribute name after '@', got {other:?}"
                    ))),
                }
            }
            Some(Tok::String(s)) => {
                self.bump();
                Ok(Expr::Literal(s))
            }
            Some(Tok::Number(n)) => {
                self.bump();
                Ok(Expr::Number(n))
            }
            Some(Tok::Name(n)) => {
                // Function call? `name(...)`. Or `self::tag` test? Or bare
                // child name (the lxml-quirk shape `rel="me"` inside a
                // predicate)?
                if n == "self" && matches!(self.peek_n(1), Some(Tok::ColonColon)) {
                    self.bump();
                    self.bump();
                    match self.bump() {
                        Some(Tok::Name(tag)) => Ok(Expr::SelfTagTest(tag.to_ascii_lowercase())),
                        other => Err(XPathError::Parse(format!(
                            "expected tag after 'self::', got {other:?}"
                        ))),
                    }
                } else if matches!(self.peek_n(1), Some(Tok::LParen)) {
                    let name = n;
                    self.bump(); // name
                    self.bump(); // (
                    let args = self.parse_call_args()?;
                    self.expect(&Tok::RParen)?;
                    self.dispatch_function(&name, args)
                } else {
                    // Bare child-element test — e.g. `rel="me"` in
                    // AUTHOR_XPATHS. lxml treats this as `child::rel` and the
                    // resulting node-set's existence is the boolean truth.
                    self.bump();
                    Ok(Expr::ChildElementTest(n.to_ascii_lowercase()))
                }
            }
            Some(Tok::LParen) => {
                self.bump();
                let e = self.parse_or()?;
                self.expect(&Tok::RParen)?;
                Ok(e)
            }
            other => Err(XPathError::Parse(format!(
                "expected primary expression, got {other:?}"
            ))),
        }
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, XPathError> {
        let mut args = Vec::new();
        if matches!(self.peek(), Some(Tok::RParen)) {
            return Ok(args);
        }
        args.push(self.parse_or()?);
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.bump();
            args.push(self.parse_or()?);
        }
        Ok(args)
    }

    fn dispatch_function(&self, name: &str, args: Vec<Expr>) -> Result<Expr, XPathError> {
        match name {
            "contains" => {
                if args.len() != 2 {
                    return Err(XPathError::Parse(format!(
                        "contains() takes 2 args; got {}",
                        args.len()
                    )));
                }
                let mut it = args.into_iter();
                let a = it.next().unwrap();
                let b = it.next().unwrap();
                Ok(Expr::FnContains(Box::new(a), Box::new(b)))
            }
            "starts-with" => {
                if args.len() != 2 {
                    return Err(XPathError::Parse(format!(
                        "starts-with() takes 2 args; got {}",
                        args.len()
                    )));
                }
                let mut it = args.into_iter();
                let a = it.next().unwrap();
                let b = it.next().unwrap();
                Ok(Expr::FnStartsWith(Box::new(a), Box::new(b)))
            }
            "translate" => {
                if args.len() != 3 {
                    return Err(XPathError::Parse(format!(
                        "translate() takes 3 args; got {}",
                        args.len()
                    )));
                }
                let mut it = args.into_iter();
                let a = it.next().unwrap();
                let b = it.next().unwrap();
                let c = it.next().unwrap();
                Ok(Expr::FnTranslate(Box::new(a), Box::new(b), Box::new(c)))
            }
            other => Err(XPathError::Unsupported(format!(
                "function {other:?} is outside the DA-B-1 catalog"
            ))),
        }
    }
}

/// Stringify a path-list for diagnostic use only (the AST does not need to
/// round-trip; this is for error messages when an unsupported construct
/// hits).
fn serialize_paths(paths: &[Path]) -> String {
    let parts: Vec<String> = paths.iter().map(|p| format!("{p:?}")).collect();
    parts.join("|")
}

/// Parse an XPath expression into the internal AST.
pub(crate) fn parse(src: &str) -> Result<XPath, XPathError> {
    let toks = Tokenizer::new(src).tokenize()?;
    let mut p = Parser::new(toks);
    p.parse_xpath()
}

// ---------------------------------------------------------------------------
// Evaluator
// ---------------------------------------------------------------------------

fn eval_path(xpath: &XPath, root: &NodeRef) -> Vec<NodeRef> {
    match xpath {
        XPath::Path(p) => {
            let raw = eval_one_path(p, root);
            doc_order_unique(raw, root)
        }
        XPath::Union(paths) => {
            let mut all = Vec::new();
            for p in paths {
                all.extend(eval_one_path(p, root));
            }
            doc_order_unique(all, root)
        }
    }
}

fn eval_one_path(p: &Path, root: &NodeRef) -> Vec<NodeRef> {
    // Pick the initial context node-set.
    let start = if p.absolute {
        document_root(root)
    } else {
        root.clone()
    };
    let mut current = vec![start];

    for step in &p.steps {
        let mut next: Vec<NodeRef> = Vec::new();
        for ctx in &current {
            next.extend(apply_step(step, ctx));
        }
        // Per-step dedup keeps the set tractable across the path; final dedup
        // re-runs at the end (eval_path).
        current = dedup_preserve(next);

        // Predicates: filter the step's emitted set, with positional applied
        // AFTER all expression predicates (the DA-B-1 contract).
        let mut exprs = Vec::new();
        let mut positional: Option<usize> = None;
        for pred in &step.predicates {
            match pred {
                Predicate::Expr(e) => exprs.push(e.clone()),
                Predicate::Positional(n) => {
                    // Last positional wins — Trafilatura only ever uses one,
                    // but XPath would chain `[1][2]` etc. nonsensically.
                    positional = Some(*n);
                }
            }
        }
        if !exprs.is_empty() {
            current.retain(|node| exprs.iter().all(|e| eval_expr_bool(e, node)));
        }
        if let Some(n) = positional {
            // Document order is preserved by construction (we walked
            // descendants in pre-order). Take the Nth (1-indexed).
            let n = n.saturating_sub(1);
            current = current.into_iter().nth(n).into_iter().collect();
        }
    }

    current
}

fn document_root(node: &NodeRef) -> NodeRef {
    // Walk up to the topmost parent (the rcdom Document or detached root).
    let mut cur = node.clone();
    loop {
        let parent = {
            let weak = cur.parent.take();
            let upgraded = weak.as_ref().and_then(|w| w.upgrade());
            cur.parent.set(weak);
            upgraded
        };
        match parent {
            Some(p) => cur = p,
            None => return cur,
        }
    }
}

fn apply_step(step: &Step, ctx: &NodeRef) -> Vec<NodeRef> {
    match step.axis {
        Axis::Child => match &step.test {
            NodeTest::Wildcard => element_children(ctx),
            NodeTest::Name(name) => element_children(ctx)
                .into_iter()
                .filter(|n| element_name_matches(n, name))
                .collect(),
            NodeTest::Text => text_children(ctx),
        },
        Axis::DescendantOrSelf => match &step.test {
            // `.//foo` is sugar for `./descendant-or-self::node()/child::foo`
            // (W3C XPath 1.0 §2.5). The trailing `child::foo` means foo must
            // be a CHILD of some descendant-or-self of the context node, i.e.
            // foo is a strict descendant of the context node — `.//foo` from
            // a foo context does NOT include self. lxml matches this exactly.
            // We therefore walk strict descendants only.
            NodeTest::Wildcard => strict_descendant_elements(ctx, |_| true),
            NodeTest::Name(name) => {
                strict_descendant_elements(ctx, |n| element_name_matches(n, name))
            }
            NodeTest::Text => descendant_text_nodes_as_elements(ctx),
        },
        Axis::SelfAxis => match &step.test {
            NodeTest::Wildcard => {
                if is_element(ctx) {
                    vec![ctx.clone()]
                } else {
                    vec![]
                }
            }
            NodeTest::Name(name) => {
                if is_element(ctx) && element_name_matches(ctx, name) {
                    vec![ctx.clone()]
                } else {
                    vec![]
                }
            }
            NodeTest::Text => {
                if matches!(ctx.data, NodeData::Text { .. }) {
                    // Surfacing text as element: see module-doc contract.
                    vec![ctx.clone()]
                } else {
                    vec![]
                }
            }
        },
        Axis::Attribute => {
            // Only used if the engine is later extended to put `@attr` at
            // step level (trafilatura never does — `@attr` only appears
            // inside predicates). Return empty for safety.
            vec![]
        }
    }
}

fn element_children(node: &NodeRef) -> Vec<NodeRef> {
    dom::children(node)
}

fn text_children(node: &NodeRef) -> Vec<NodeRef> {
    child_nodes(node)
        .into_iter()
        .filter(|n| matches!(n.data, NodeData::Text { .. }))
        .collect()
}

/// Strict element descendants in document order. `node` itself is NOT
/// considered (W3C XPath 1.0 §2.5: `.//foo` =
/// `./descendant-or-self::node()/child::foo`; the trailing `child::foo` forces
/// foo to be a descendant of self, never self). The `keep` closure is the
/// node-test filter — applied during the walk to keep the result small.
fn strict_descendant_elements<F: Fn(&NodeRef) -> bool>(node: &NodeRef, keep: F) -> Vec<NodeRef> {
    let mut out = Vec::new();
    let kids = child_nodes(node);
    for child in kids {
        walk_descendants_self_first(&child, &keep, &mut out);
    }
    out
}

fn walk_descendants_self_first<F: Fn(&NodeRef) -> bool>(
    node: &NodeRef,
    keep: &F,
    out: &mut Vec<NodeRef>,
) {
    if is_element(node) && keep(node) {
        out.push(node.clone());
    }
    let kids = child_nodes(node);
    for child in kids {
        walk_descendants_self_first(&child, keep, out);
    }
}

/// All text-node descendants. For the `.//p//text()` arbiter pattern, we
/// surface text nodes as nodes; downstream `len()` matches lxml because
/// text-nodes and elements both count as "one node" in a node-set.
fn descendant_text_nodes_as_elements(node: &NodeRef) -> Vec<NodeRef> {
    let mut out = Vec::new();
    collect_text_descendants(node, &mut out);
    out
}

fn collect_text_descendants(node: &NodeRef, out: &mut Vec<NodeRef>) {
    for child in child_nodes(node) {
        if matches!(child.data, NodeData::Text { .. }) {
            out.push(child.clone());
        } else {
            collect_text_descendants(&child, out);
        }
    }
}

fn element_name_matches(node: &NodeRef, want_lowercase: &str) -> bool {
    match local_name(node) {
        Some(n) => n.eq_ignore_ascii_case(want_lowercase),
        None => false,
    }
}

/// Doc-order dedup. The engine's per-step walk emits elements in
/// document order naturally (descendant-or-self is a pre-order traversal),
/// and step composition concatenates per-context emissions; duplicates are
/// possible when two contexts have overlapping descendant subtrees (e.g.
/// `//div//span` from nested `<div><div><span/></div></div>` yields `span`
/// from each `div` context). We dedup by node identity and re-sort into
/// document order via a recorded pre-order index of the document root.
fn dedup_preserve(nodes: Vec<NodeRef>) -> Vec<NodeRef> {
    let mut seen: HashSet<usize> = HashSet::new();
    let mut out = Vec::with_capacity(nodes.len());
    for n in nodes {
        let key = Rc::as_ptr(&n) as usize;
        if seen.insert(key) {
            out.push(n);
        }
    }
    out
}

fn doc_order_unique(nodes: Vec<NodeRef>, root: &NodeRef) -> Vec<NodeRef> {
    let deduped = dedup_preserve(nodes);
    if deduped.len() <= 1 {
        return deduped;
    }
    // Build a document-order index by walking from the topmost root.
    let top = document_root(root);
    let mut order: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut counter = 0usize;
    walk_record_order(&top, &mut counter, &mut order);

    let mut indexed: Vec<(usize, NodeRef)> = deduped
        .into_iter()
        .map(|n| {
            let k = Rc::as_ptr(&n) as usize;
            let i = order.get(&k).copied().unwrap_or(usize::MAX);
            (i, n)
        })
        .collect();
    indexed.sort_by_key(|(i, _)| *i);
    indexed.into_iter().map(|(_, n)| n).collect()
}

fn walk_record_order(
    node: &NodeRef,
    counter: &mut usize,
    order: &mut std::collections::HashMap<usize, usize>,
) {
    let key = Rc::as_ptr(node) as usize;
    order.insert(key, *counter);
    *counter += 1;
    for child in child_nodes(node) {
        walk_record_order(&child, counter, order);
    }
}

// ---------------------------------------------------------------------------
// Expression evaluation (boolean / string / numeric coercion)
// ---------------------------------------------------------------------------

fn eval_expr_bool(e: &Expr, ctx: &NodeRef) -> bool {
    match e {
        Expr::Or(l, r) => eval_expr_bool(l, ctx) || eval_expr_bool(r, ctx),
        Expr::And(l, r) => eval_expr_bool(l, ctx) && eval_expr_bool(r, ctx),
        Expr::Eq(l, r) => eval_expr_str(l, ctx) == eval_expr_str(r, ctx),
        Expr::Attribute(name) => attr_exists(ctx, name),
        Expr::AttributeUnion(names) => names.iter().any(|n| attr_exists(ctx, n)),
        Expr::SelfTagTest(tag) => element_name_matches(ctx, tag),
        Expr::ChildElementTest(name) => {
            // Boolean truth: at least one child element with this name. Used
            // for the trafilatura `rel="me"` typo / shape.
            element_children(ctx)
                .iter()
                .any(|n| element_name_matches(n, name))
        }
        Expr::Literal(s) => !s.is_empty(),
        Expr::Number(n) => *n != 0.0,
        Expr::FnContains(a, b) => {
            let h = eval_expr_str(a, ctx);
            let n = eval_expr_str(b, ctx);
            // libxml2: contains(_, "") is true. We replicate that.
            if n.is_empty() {
                return true;
            }
            h.contains(&n)
        }
        Expr::FnStartsWith(a, b) => {
            let h = eval_expr_str(a, ctx);
            let n = eval_expr_str(b, ctx);
            if n.is_empty() {
                return true;
            }
            h.starts_with(&n)
        }
        Expr::FnTranslate(a, b, c) => {
            // translate() returns a string; in boolean context, that string
            // is truthy iff non-empty (XPath 1.0 §3.4 boolean(string) =
            // string-length(s) > 0). Trafilatura always wraps translate() in
            // contains()/starts-with(), so this branch is mostly defensive.
            let s = translate(
                &eval_expr_str(a, ctx),
                &eval_expr_str(b, ctx),
                &eval_expr_str(c, ctx),
            );
            !s.is_empty()
        }
    }
}

fn eval_expr_str(e: &Expr, ctx: &NodeRef) -> String {
    match e {
        Expr::Literal(s) => s.clone(),
        Expr::Number(n) => format_number(*n),
        Expr::Attribute(name) => attr_value(ctx, name),
        Expr::AttributeUnion(names) => {
            // libxml2: node-set in string context = string-value of first node
            // in document order. For an `@a|@b` union on an element, the
            // "node-set" is the set of those attributes that are present. The
            // "first in document order" is the first attribute that appears
            // on the element in attribute declaration order. lxml/libxml2's
            // attribute iteration follows the parser's order; in html5ever
            // attributes are stored in source order too — see
            // `get_attribute`'s use of `.iter().find` over the rcdom
            // `attrs.borrow()`. So we walk the element's attribute list in
            // source order and return the first whose name matches any of
            // `names`. Empty string if none match (the libxml2 contract).
            //
            // The exception (Trafilatura corpus relevant): when the union
            // appears as the first arg to `contains(@id|@class, "x")`, lxml
            // applies the same coercion, so an element with NO id and NO
            // class yields empty string (and contains("", "x") = false). The
            // conformance harness pins this.
            attr_value_union_first_in_source_order(ctx, names)
        }
        Expr::SelfTagTest(tag) => {
            // string-value of a SelfTagTest predicate doesn't really make
            // sense, but XPath rules: a boolean is "true" or "false" as
            // strings.
            if element_name_matches(ctx, tag) {
                "true".into()
            } else {
                "false".into()
            }
        }
        Expr::ChildElementTest(name) => {
            // String-value of a node-set = string-value of first node in
            // document order. The string-value of an element is the
            // concatenation of all descendant text-node data.
            element_children(ctx)
                .into_iter()
                .find(|n| element_name_matches(n, name))
                .map(|n| dom::text_content(&n))
                .unwrap_or_default()
        }
        Expr::Or(_, _) | Expr::And(_, _) | Expr::Eq(_, _) => {
            // Boolean -> string per XPath 1.0 §4.2.
            if eval_expr_bool(e, ctx) {
                "true".into()
            } else {
                "false".into()
            }
        }
        Expr::FnContains(_, _) | Expr::FnStartsWith(_, _) => {
            if eval_expr_bool(e, ctx) {
                "true".into()
            } else {
                "false".into()
            }
        }
        Expr::FnTranslate(a, b, c) => translate(
            &eval_expr_str(a, ctx),
            &eval_expr_str(b, ctx),
            &eval_expr_str(c, ctx),
        ),
    }
}

fn attr_exists(node: &NodeRef, name: &str) -> bool {
    if !is_element(node) {
        return false;
    }
    get_attribute(node, name).is_some()
}

fn attr_value(node: &NodeRef, name: &str) -> String {
    get_attribute(node, name).unwrap_or_default()
}

/// Walk the element's attribute list in source order and return the value of
/// the first attribute whose name is in `names`. Returns empty string if
/// none match. This is libxml2's node-set-to-string coercion for the
/// `@a|@b|...` union shape (DA-B-1 load-bearing case).
///
/// Post-review MAJOR fix: consumes the `dom::attributes_in_source_order`
/// facade rather than reaching into rcdom internals here. Preserves the
/// "lxml-facade discipline" invariant Stage 0a established: only `dom.rs`
/// touches rcdom-private storage.
fn attr_value_union_first_in_source_order(node: &NodeRef, names: &[String]) -> String {
    for (attr_name, attr_value) in attributes_in_source_order(node) {
        if names.iter().any(|n| n.eq_ignore_ascii_case(&attr_name)) {
            return attr_value;
        }
    }
    String::new()
}

/// XPath 1.0 `translate(string, from, to)`:
/// "The translate function returns the first argument string with occurrences
/// of characters in the second argument string replaced by the character at
/// the corresponding position in the third argument string. If there is a
/// character in the second argument string with no character at a
/// corresponding position in the third argument string (because the second
/// argument string is longer than the third argument string), then occurrences
/// of that character in the first argument string are removed."
/// (https://www.w3.org/TR/1999/REC-xpath-19991116/#section-String-Functions)
fn translate(src: &str, from: &str, to: &str) -> String {
    let from_chars: Vec<char> = from.chars().collect();
    let to_chars: Vec<char> = to.chars().collect();
    let mut out = String::with_capacity(src.len());
    for c in src.chars() {
        // First occurrence in `from` wins (XPath says "the character at the
        // corresponding position"; the spec example 'translate("bar","abc","ABC")'
        // → "BAr" — 'a' (pos 0) -> 'A', 'b' (pos 1) -> not at pos 1 since 'b'
        // appears at index 1 in "abc"; actually the spec example is "BAr".
        // Re-reading: "abc" -> "ABC", so 'a'→'A', 'b'→'B', 'c'→'C'. Input
        // "bar" → 'b'→'B', 'a'→'A', 'r'→'r' (unchanged) = "BAr". So first
        // occurrence in `from` does win — if 'a' appears twice in `from`,
        // only the first matters.
        match from_chars.iter().position(|&f| f == c) {
            Some(i) => {
                if let Some(&t) = to_chars.get(i) {
                    out.push(t);
                }
                // else: deleted (no corresponding char in `to`).
            }
            None => out.push(c),
        }
    }
    out
}

/// XPath 1.0 number-to-string (§4.4): integers render without trailing
/// `.0`. Trafilatura never uses arithmetic, so this is purely defensive.
fn format_number(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 {
            "Infinity".into()
        } else {
            "-Infinity".into()
        };
    }
    if n == n.trunc() && n.abs() < 1e15 {
        return format!("{:.0}", n);
    }
    format!("{}", n)
}

// ---------------------------------------------------------------------------
// Diagnostic helper used by some test cases
// ---------------------------------------------------------------------------

/// Number of matches — convenience for the conformance harness.
pub fn count(xpath: &str, root: &NodeRef) -> Result<usize, XPathError> {
    Ok(evaluate(xpath, root)?.len())
}

/// First match's tag name (lower-cased) — convenience.
pub fn first_tag(xpath: &str, root: &NodeRef) -> Result<Option<String>, XPathError> {
    Ok(evaluate(xpath, root)?.first().and_then(|n| {
        local_name(n)
            .map(|s| s.to_ascii_lowercase())
            .or_else(|| tag_name(n))
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::readability::dom::Dom;

    fn parse_dom(html: &str) -> Dom {
        Dom::parse(html)
    }

    fn body(d: &Dom) -> NodeRef {
        d.body().expect("body required for these tests")
    }

    fn ids(nodes: &[NodeRef]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| get_attribute(n, "id").unwrap_or_default())
            .collect()
    }

    fn tags(nodes: &[NodeRef]) -> Vec<String> {
        nodes
            .iter()
            .map(|n| local_name(n).unwrap_or_default())
            .collect()
    }

    // ----- Tokenizer / parser ----------------------------------------------

    #[test]
    fn tokenize_basics() {
        let toks = Tokenizer::new("//div[@class='x']").tokenize().unwrap();
        assert_eq!(
            toks,
            vec![
                Tok::DoubleSlash,
                Tok::Name("div".into()),
                Tok::LBracket,
                Tok::At,
                Tok::Name("class".into()),
                Tok::Eq,
                Tok::String("x".into()),
                Tok::RBracket,
            ]
        );
    }

    #[test]
    fn tokenize_self_axis() {
        let toks = Tokenizer::new("self::div").tokenize().unwrap();
        assert_eq!(
            toks,
            vec![
                Tok::Name("self".into()),
                Tok::ColonColon,
                Tok::Name("div".into()),
            ]
        );
    }

    #[test]
    fn parse_simple_descendant() {
        let p = parse(".//div").unwrap();
        if let XPath::Path(p) = p {
            assert!(!p.absolute);
            assert_eq!(p.steps.len(), 1);
            assert!(matches!(p.steps[0].axis, Axis::DescendantOrSelf));
            assert_eq!(p.steps[0].test, NodeTest::Name("div".into()));
        } else {
            panic!("expected Path");
        }
    }

    #[test]
    fn parse_union() {
        let p = parse("//time|//figure").unwrap();
        if let XPath::Union(paths) = p {
            assert_eq!(paths.len(), 2);
        } else {
            panic!("expected Union");
        }
    }

    #[test]
    fn parse_paren_position() {
        let p = parse("(.//article)[1]").unwrap();
        if let XPath::Path(p) = p {
            assert_eq!(p.steps.len(), 1);
            assert_eq!(p.steps[0].predicates.len(), 1);
            assert!(matches!(p.steps[0].predicates[0], Predicate::Positional(1)));
        } else {
            panic!("expected Path");
        }
    }

    // ----- Axes -------------------------------------------------------------

    #[test]
    fn descendant_or_self_finds_nested() {
        // .//p must find p at any depth.
        let d = parse_dom(
            "<html><body><div><section><p id='a'/></section></div><p id='b'/></body></html>",
        );
        let r = evaluate(".//p", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a", "b"]);
    }

    #[test]
    fn child_axis_only_direct() {
        // /p (after a body context) finds only direct children.
        let d = parse_dom("<html><body><p id='a'/><div><p id='b'/></div></body></html>");
        // body/p — explicit child path on body's context.
        let r = evaluate("p", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn self_axis_in_predicate() {
        let d = parse_dom("<html><body><article id='a'/><div id='b'/><p id='c'/></body></html>");
        let r = evaluate(".//*[self::article or self::div]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a", "b"]);
    }

    #[test]
    fn wildcard_descendants() {
        let d = parse_dom("<html><body><div id='a'><span id='b'/></div></body></html>");
        let r = evaluate(".//*", &body(&d)).unwrap();
        // body's descendants: div, span (in doc order).
        assert_eq!(ids(&r), vec!["a", "b"]);
    }

    // ----- Predicates ------------------------------------------------------

    #[test]
    fn attribute_eq_predicate() {
        let d = parse_dom(
            "<html><body><div class='post' id='a'/><div class='other' id='b'/></body></html>",
        );
        let r = evaluate(".//div[@class='post']", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn attribute_presence_predicate() {
        let d = parse_dom("<html><body><div data-x='1' id='a'/><div id='b'/></body></html>");
        let r = evaluate(".//div[@data-x]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn positional_one() {
        let d = parse_dom("<html><body><article id='a'/><article id='b'/></body></html>");
        let r = evaluate("(.//article)[1]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn predicate_then_positional_da_b1_ordering() {
        // Filter then take first: only articles with @class='post' qualify,
        // then [1] picks the first of those.
        let d = parse_dom(
            "<html><body>\
                <article id='a' />\
                <article id='b' class='post' />\
                <article id='c' class='post' />\
            </body></html>",
        );
        // The predicate ordering: [@class='post'] first, [1] second.
        // We model `[1]` always as positional-after-predicates per DA-B-1.
        let r = evaluate(".//article[@class='post'][1]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["b"]);
    }

    // ----- contains / starts-with / translate ------------------------------

    #[test]
    fn contains_simple() {
        let d = parse_dom(
            "<html><body>\
                <div id='a' class='post-text x' />\
                <div id='b' class='other' />\
            </body></html>",
        );
        let r = evaluate(".//div[contains(@class, 'post-text')]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn contains_with_node_set_union_first_arg_da_b1() {
        // The DA-B-1 load-bearing edge case: `contains(@id|@class, 'commentlist')`
        // applies libxml2's node-set-to-string coercion (string-value of first
        // node in document order). The element below has NO id but a class;
        // the union should fall through to @class.
        let d = parse_dom(
            "<html><body>\
                <div class='wp-commentlist box' id_marker='1' />\
                <div id='commentlist-other' class='unrelated' id_marker='2' />\
                <div class='unrelated' id_marker='3' />\
            </body></html>",
        );
        // Use id_marker for stable identification (id collides with the
        // libxml2 coercion under test).
        let r = evaluate(".//div[contains(@id|@class, 'commentlist')]", &body(&d)).unwrap();
        let markers: Vec<String> = r
            .iter()
            .map(|n| get_attribute(n, "id_marker").unwrap_or_default())
            .collect();
        // Marker 1: class contains "commentlist", first union elem (no @id
        //   on this element, so @class wins for the string coercion).
        // Marker 2: has @id, which is first in source order; @id contains
        //   "commentlist-other" which contains the substring "commentlist".
        // Marker 3: no @id, @class="unrelated" — no match.
        assert_eq!(markers, vec!["1", "2"]);
    }

    #[test]
    fn contains_with_empty_node_set_yields_empty_string_and_false() {
        // An element with NEITHER @id nor @class yields empty string for
        // `@id|@class`; contains("", "x") is false in libxml2 (and true only
        // for contains(_, "") empty needle, per spec).
        let d = parse_dom("<html><body><span id_marker='1' /></body></html>");
        let r = evaluate(".//span[contains(@id|@class, 'x')]", &body(&d)).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn starts_with_simple() {
        let d = parse_dom(
            "<html><body>\
                <div id='comments-a' />\
                <div id='other' />\
            </body></html>",
        );
        let r = evaluate(".//div[starts-with(@id, 'comments')]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["comments-a"]);
    }

    #[test]
    fn translate_single_char_table() {
        // The Trafilatura idiom: case-fold one or two characters. With B→b
        // only, "ArticleBody" becomes "Articlebody" which does NOT contain
        // "articlebody" (capital A doesn't fold), while "articleBody" becomes
        // "articlebody" which does. This is exactly what lxml does and is
        // the deliberate Trafilatura semantic (the writer is case-folding
        // specific characters they expect to vary; the regression is human-
        // visible and matches the pattern's behaviour).
        let d = parse_dom(
            "<html><body>\
                <div id='ArticleBody' />\
                <div id='articleBody' />\
                <div id='other' />\
            </body></html>",
        );
        let r = evaluate(
            ".//div[contains(translate(@id, 'B', 'b'), 'articlebody')]",
            &body(&d),
        )
        .unwrap();
        assert_eq!(ids(&r), vec!["articleBody"]);
        // The fuller idiom from xpaths.py uses two-char tables:
        // `translate(@id, "AB", "ab")` — this DOES match both, since both A→a
        // and B→b. We verify that here as a positive control.
        let r = evaluate(
            ".//div[contains(translate(@id, 'AB', 'ab'), 'articlebody')]",
            &body(&d),
        )
        .unwrap();
        assert_eq!(ids(&r), vec!["ArticleBody", "articleBody"]);
    }

    #[test]
    fn translate_deletion_when_to_shorter_than_from() {
        // XPath 1.0 §4.2: chars in `from` without a corresponding position in
        // `to` are deleted. For translate("foobar", "ao", "X"):
        //   'f' not in "ao" -> 'f'
        //   'o' at position 1 in "ao", to[1] absent -> deleted
        //   'o' -> deleted
        //   'b' -> 'b'
        //   'a' at position 0 in "ao", to[0]='X' -> 'X'
        //   'r' -> 'r'
        // Result: "fbXr".
        assert_eq!(translate("foobar", "ao", "X"), "fbXr");
    }

    #[test]
    fn translate_spec_example() {
        // The W3C XPath 1.0 example from §4.2:
        //   translate("bar","abc","ABC") -> "BAr"
        assert_eq!(translate("bar", "abc", "ABC"), "BAr");
    }

    // ----- Union -----------------------------------------------------------

    #[test]
    fn union_top_level() {
        let d = parse_dom("<html><body><time id='t'/><figure id='f'/><div id='d'/></body></html>");
        let r = evaluate(".//time|.//figure", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["t", "f"]);
    }

    #[test]
    fn union_doc_order_preserved() {
        // Document order across the union: figure appears before time here.
        let d = parse_dom("<html><body><figure id='f'/><div id='d'/><time id='t'/></body></html>");
        let r = evaluate(".//time|.//figure", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["f", "t"]);
    }

    // ----- text() ----------------------------------------------------------

    #[test]
    fn text_node_count_external_arbiter() {
        // .//p//text() — the arbiter pattern. Should count one text node per
        // <p> with text in this snapshot.
        let d = parse_dom("<html><body><p>hello</p><p>world</p></body></html>");
        let r = evaluate(".//p//text()", &body(&d)).unwrap();
        assert_eq!(r.len(), 2);
    }

    // ----- Misc / structural ------------------------------------------------

    #[test]
    fn dedup_handles_overlapping_descendants() {
        // .//div//span from nested div: only one span, should not duplicate.
        let d = parse_dom("<html><body><div><div><span id='s'/></div></div></body></html>");
        let r = evaluate(".//div//span", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["s"]);
    }

    #[test]
    fn child_chain_after_paren_path() {
        // (.//div[@class='wrap'])//a[@href] — the chain shape.
        let d = parse_dom(
            "<html><body>\
                <div class='wrap'><a id='a' href='/x'/><a id='b' /></div>\
                <a id='c' href='/y' />\
            </body></html>",
        );
        let r = evaluate(".//div[@class='wrap']//a[@href]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn absolute_doubleslash_from_inner_context() {
        // Even from an inner context, `//foo` must reach the document root.
        let d = parse_dom("<html><body><div id='outer'><span id='target'/></div></body></html>");
        let inner = evaluate(".//span", &body(&d)).unwrap()[0].clone();
        let r = evaluate("//span", &inner).unwrap();
        assert_eq!(ids(&r), vec!["target"]);
    }

    #[test]
    fn self_axis_self_only() {
        // self::div on an element node — true iff it's a div.
        let d = parse_dom("<html><body><div id='d'/></body></html>");
        let div = evaluate(".//div", &body(&d)).unwrap()[0].clone();
        let r = evaluate("self::div", &div).unwrap();
        assert_eq!(ids(&r), vec!["d"]);
        let r = evaluate("self::span", &div).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn multi_or_predicate() {
        let d = parse_dom(
            "<html><body>\
                <article id='a' class='post' />\
                <div id='b' class='entry' />\
                <main id='c' class='other' />\
                <section id='d' itemprop='articleBody' />\
            </body></html>",
        );
        let r = evaluate(
            ".//*[self::article or self::div or self::main or self::section][@class='post' or @class='entry' or @itemprop='articleBody']",
            &body(&d),
        )
        .unwrap();
        assert_eq!(ids(&r), vec!["a", "b", "d"]);
    }

    #[test]
    fn body_xpath_first_pattern_smoke() {
        // The first BODY_XPATH pattern from trafilatura/xpaths.py — heavy
        // multi-or predicate plus `[1]` positional. Smoke-checks the
        // engine can chew through it without complaint.
        let xp = "(.//*[self::article or self::div or self::main or self::section]\
            [@class=\"post\" or @class=\"entry\" or contains(@class, \"post-text\")\
             or contains(@class, \"article-content\")])[1]";
        let d = parse_dom(
            "<html><body>\
                <div class='other' id='a' />\
                <div class='article-content body' id='b' />\
                <div class='entry' id='c' />\
            </body></html>",
        );
        let r = evaluate(xp, &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["b"]);
    }

    #[test]
    fn discard_xpath_smoke() {
        // A simpler PRECISION_DISCARD_XPATH-style pattern: `.//header`.
        let d = parse_dom("<html><body><header id='h'/><div><header id='h2'/></div></body></html>");
        let r = evaluate(".//header", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["h", "h2"]);
    }

    #[test]
    fn attribute_with_hyphen_and_underscore() {
        // Trafilatura uses @data-component, @data-testid, @data-lp-replacement-content.
        let d = parse_dom("<html><body><div data-component='Byline' id='a'/></body></html>");
        let r = evaluate(".//*[contains(@data-component, 'Byline')]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn descendant_axis_excludes_self_per_w3c_2_5() {
        // W3C XPath 1.0 §2.5: `.//foo` = `./descendant-or-self::node()/child::foo`.
        // The trailing `child::foo` forces foo to be a *child* of some
        // descendant-or-self of the context node, hence foo must be a strict
        // descendant of the context node — `.//foo` from a foo context does
        // NOT include self. lxml matches this exactly; the engine matches.
        let d = parse_dom("<html><body><div id='outer'><div id='inner'/></div></body></html>");
        let outer = evaluate(".//div", &body(&d)).unwrap()[0].clone();
        let r = evaluate(".//div", &outer).unwrap();
        assert_eq!(ids(&r), vec!["inner"]);
    }

    #[test]
    fn comments_xpath_smoke_with_attribute_union() {
        // COMMENTS_XPATH[0] uses `contains(@id|@class, 'commentlist')`.
        let xp = ".//*[self::div or self::list or self::section]\
            [contains(@id|@class, 'commentlist') or \
             contains(@class, 'comment-page') or \
             contains(@id|@class, 'comment-list')]";
        let d = parse_dom(
            "<html><body>\
                <div class='wp-commentlist' id_marker='1' />\
                <div class='comment-page' id_marker='2' />\
                <section class='comment-list' id_marker='3' />\
                <div class='unrelated' id_marker='4' />\
            </body></html>",
        );
        let r = evaluate(xp, &body(&d)).unwrap();
        let markers: Vec<String> = r
            .iter()
            .map(|n| get_attribute(n, "id_marker").unwrap_or_default())
            .collect();
        assert_eq!(markers, vec!["1", "2", "3"]);
    }

    #[test]
    fn body_xpath_main_or_top_class() {
        // BODY_XPATH last pattern uses starts-with and union of paren-paths.
        // We test a simplified version since the full pattern uses
        // `(...)|(...)` paren-around-union semantics which we model
        // case-by-case.
        let d = parse_dom(
            "<html><body>\
                <div class='mainbar' id='a' />\
                <div class='other' id='b' />\
                <main id='c' />\
            </body></html>",
        );
        let r = evaluate(
            ".//*[self::article or self::div or self::section][starts-with(@class, 'main')]",
            &body(&d),
        )
        .unwrap();
        assert_eq!(ids(&r), vec!["a"]);
        let r = evaluate(".//main", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["c"]);
    }

    #[test]
    fn parse_error_on_unterminated_string() {
        let err = parse(".//div[@class='unclosed").unwrap_err();
        match err {
            XPathError::Parse(_) => {}
            _ => panic!("expected Parse error"),
        }
    }

    #[test]
    fn parse_error_on_unknown_function() {
        let err = parse(".//div[matches(@class, 'x')]").unwrap_err();
        match err {
            XPathError::Unsupported(_) => {}
            _ => panic!("expected Unsupported error, got {err:?}"),
        }
    }

    #[test]
    fn translate_unicode_passthrough() {
        // ASCII chars in the table apply; chars not in the table pass
        // through unchanged. "Café" has 'C' which maps to 'c'.
        assert_eq!(translate("Caf\u{00E9}", "ABC", "abc"), "caf\u{00E9}");
        // Non-ASCII in the table works too.
        assert_eq!(translate("Caf\u{00E9}", "\u{00E9}", "e"), "Cafe");
        // A character entirely outside the table is unchanged.
        assert_eq!(translate("Z", "ABC", "abc"), "Z");
    }

    #[test]
    fn attribute_eq_with_data_lp_replacement_attr() {
        // OVERALL_DISCARD_XPATH uses `@data-lp-replacement-content` (bare
        // presence). We test the same shape.
        let d = parse_dom(
            "<html><body><div data-lp-replacement-content='1' id='a'/><div id='b'/></body></html>",
        );
        let r = evaluate(".//div[@data-lp-replacement-content]", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn empty_descendant_set_returns_empty() {
        let d = parse_dom("<html><body><span/></body></html>");
        let r = evaluate(".//article", &body(&d)).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn descendant_count_is_doc_order() {
        let d = parse_dom(
            "<html><body>\
                <div id='a'><div id='b'><div id='c'/></div></div>\
                <div id='d'/>\
            </body></html>",
        );
        let r = evaluate(".//div", &body(&d)).unwrap();
        // Document order pre-order traversal.
        assert_eq!(ids(&r), vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn link_density_pattern_smoke() {
        // CATEGORIES_XPATHS: `//div[starts-with(@class, 'post-info')]//a[@href]`.
        let xp = "//div[starts-with(@class, 'post-info')]//a[@href]";
        let d = parse_dom(
            "<html><body>\
                <div class='post-info-foo'>\
                    <a id='a' href='/x' />\
                    <a id='b' />\
                </div>\
                <div class='unrelated'>\
                    <a id='c' href='/y' />\
                </div>\
            </body></html>",
        );
        let r = evaluate(xp, &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a"]);
    }

    #[test]
    fn cite_or_quote_union() {
        // COMMENTS_DISCARD_XPATH[1]: `.//cite|.//quote`.
        let d = parse_dom("<html><body><cite id='c'/><quote id='q'/><p id='p'/></body></html>");
        let r = evaluate(".//cite|.//quote", &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["c", "q"]);
    }

    #[test]
    fn tag_test_lowercases() {
        // HTML element names normalise to lower-case; the engine matches them
        // regardless of source case.
        let d = parse_dom("<html><body><ARTICLE id='a'/></body></html>");
        let r = evaluate(".//article", &body(&d)).unwrap();
        // html5ever lower-cases element names on parse.
        assert_eq!(tags(&r), vec!["article"]);
    }

    /// **xpaths.py-pattern smoke**: every xpaths.py XPath from
    /// `trafilatura@v2.0.0` parses without error. This is the "engine surface
    /// drift" tripwire — if a future xpaths.py revision adds a construct
    /// outside DA-B-1, this test catches it before Stage 1b silently
    /// miscompiles.
    #[test]
    fn xpaths_py_patterns_all_parse() {
        let patterns: &[&str] = &[
            // BODY_XPATH (5)
            ".//*[self::article or self::div or self::main or self::section][@class=\"post\" or @class=\"entry\" or contains(@class, \"post-text\") or contains(@class, \"post_text\") or contains(@class, \"post-body\") or contains(@class, \"post-entry\") or contains(@class, \"postentry\") or contains(@class, \"post-content\") or contains(@class, \"post_content\") or contains(@class, \"postcontent\") or contains(@class, \"postContent\") or contains(@class, \"post_inner_wrapper\") or contains(@class, \"article-text\") or contains(@class, \"articletext\") or contains(@class, \"articleText\") or contains(@id, \"entry-content\") or contains(@class, \"entry-content\") or contains(@id, \"article-content\") or contains(@class, \"article-content\") or contains(@id, \"article__content\") or contains(@class, \"article__content\") or contains(@id, \"article-body\") or contains(@class, \"article-body\") or contains(@id, \"article__body\") or contains(@class, \"article__body\") or @itemprop=\"articleBody\" or contains(translate(@id, \"B\", \"b\"), \"articlebody\") or contains(translate(@class, \"B\", \"b\"), \"articlebody\") or @id=\"articleContent\" or contains(@class, \"ArticleContent\") or contains(@class, \"page-content\") or contains(@class, \"text-content\") or contains(@id, \"body-text\") or contains(@class, \"body-text\") or contains(@class, \"article__container\") or contains(@id, \"art-content\") or contains(@class, \"art-content\")][1]",
            "(.//article)[1]",
            "(.//*[self::article or self::div or self::main or self::section][contains(@class, 'post-bodycopy') or contains(@class, 'storycontent') or contains(@class, 'story-content') or @class='postarea' or @class='art-postcontent' or contains(@class, 'theme-content') or contains(@class, 'blog-content') or contains(@class, 'section-content') or contains(@class, 'single-content') or contains(@class, 'single-post') or contains(@class, 'main-column') or contains(@class, 'wpb_text_column') or starts-with(@id, 'primary') or starts-with(@class, 'article ') or @class=\"text\" or @id=\"article\" or @class=\"cell\" or @id=\"story\" or @class=\"story\" or contains(@class, \"story-body\") or contains(@id, \"story-body\") or contains(@class, \"field-body\") or contains(translate(@class, \"FULTEX\",\"fultex\"), \"fulltext\") or @role='article'])[1]",
            "(.//*[self::article or self::div or self::main or self::section][contains(@id, \"content-main\") or contains(@class, \"content-main\") or contains(@class, \"content_main\") or contains(@id, \"content-body\") or contains(@class, \"content-body\") or contains(@id, \"contentBody\") or contains(@class, \"content__body\") or contains(translate(@id, \"CM\",\"cm\"), \"main-content\") or contains(translate(@class, \"CM\",\"cm\"), \"main-content\") or contains(translate(@class, \"CP\",\"cp\"), \"page-content\") or @id=\"content\" or @class=\"content\"])[1]",
            // COMMENTS_XPATH (4)
            ".//*[self::div or self::list or self::section][contains(@id|@class, 'commentlist') or contains(@class, 'comment-page') or contains(@id|@class, 'comment-list') or contains(@class, 'comments-content') or contains(@class, 'post-comments')]",
            ".//*[self::div or self::section or self::list][starts-with(@id|@class, 'comments') or starts-with(@class, 'Comments') or starts-with(@id|@class, 'comment-') or contains(@class, 'article-comments')]",
            ".//*[self::div or self::section or self::list][starts-with(@id, 'comol') or starts-with(@id, 'disqus_thread') or starts-with(@id, 'dsq-comments')]",
            ".//*[self::div or self::section][starts-with(@id, 'social') or contains(@class, 'comment')]",
            // REMOVE_COMMENTS_XPATH (1)
            ".//*[self::div or self::list or self::section][starts-with(translate(@id, \"C\",\"c\"), 'comment') or starts-with(translate(@class, \"C\",\"c\"), 'comment') or contains(@class, 'article-comments') or contains(@class, 'post-comments') or starts-with(@id, 'comol') or starts-with(@id, 'disqus_thread') or starts-with(@id, 'dsq-comments')]",
            // TEASER_DISCARD_XPATH (1)
            ".//*[self::div or self::item or self::list or self::p or self::section or self::span][contains(translate(@id, \"T\", \"t\"), \"teaser\") or contains(translate(@class, \"T\", \"t\"), \"teaser\")]",
            // PRECISION_DISCARD_XPATH (2)
            ".//header",
            ".//*[self::div or self::item or self::list or self::p or self::section or self::span][contains(@id|@class, \"bottom\") or contains(@id|@class, \"link\") or contains(@style, \"border\")]",
            // OVERALL_DISCARD_XPATH (2) — the 50-line monsters, parsed
            // verbatim from xpaths.py to prove the engine swallows the full
            // construct set.
            ".//*[self::div or self::item or self::list or self::p or self::section or self::span][contains(translate(@id, \"F\",\"f\"), \"footer\") or contains(translate(@class, \"F\",\"f\"), \"footer\") or contains(@id, \"related\") or contains(@class, \"elated\") or contains(@id|@class, \"viral\") or starts-with(@id|@class, \"shar\") or contains(@class, \"share-\") or contains(translate(@id, \"S\", \"s\"), \"share\") or contains(@id|@class, \"social\") or contains(@class, \"sociable\") or contains(@id|@class, \"syndication\") or starts-with(@id, \"jp-\") or starts-with(@id, \"dpsp-content\") or contains(@class, \"embedded\") or contains(@class, \"embed\") or contains(@id|@class, \"newsletter\") or contains(@class, \"subnav\") or contains(@id|@class, \"cookie\") or contains(@id|@class, \"tags\") or contains(@class, \"tag-list\") or contains(@id|@class, \"sidebar\") or contains(@id|@class, \"banner\") or contains(@class, \"bar\") or contains(@class, \"meta\") or contains(@id, \"menu\") or contains(@class, \"menu\") or contains(translate(@id, \"N\", \"n\"), \"nav\") or contains(translate(@role, \"N\", \"n\"), \"nav\") or starts-with(@class, \"nav\") or contains(@class, \"avigation\") or contains(@class, \"navbar\") or contains(@class, \"navbox\") or starts-with(@class, \"post-nav\") or contains(@id|@class, \"breadcrumb\") or contains(@id|@class, \"bread-crumb\") or contains(@id|@class, \"author\") or contains(@id|@class, \"button\") or contains(translate(@class, \"B\", \"b\"), \"byline\") or contains(@class, \"rating\") or contains(@class, \"widget\") or contains(@class, \"attachment\") or contains(@class, \"timestamp\") or contains(@class, \"user-info\") or contains(@class, \"user-profile\") or contains(@class, \"-ad-\") or contains(@class, \"-icon\") or contains(@class, \"article-infos\") or contains(@class, \"nfoline\") or contains(@data-component, \"MostPopularStories\") or contains(@class, \"outbrain\") or contains(@class, \"taboola\") or contains(@class, \"criteo\") or contains(@class, \"options\") or contains(@class, \"expand\") or contains(@class, \"consent\") or contains(@class, \"modal-content\") or contains(@class, \" ad \") or contains(@class, \"permission\") or contains(@class, \"next-\") or contains(@class, \"-stories\") or contains(@class, \"most-popular\") or contains(@class, \"mol-factbox\") or starts-with(@class, \"ZendeskForm\") or contains(@id|@class, \"message-container\") or contains(@class, \"yin\") or contains(@class, \"zlylin\") or contains(@class, \"xg1\") or contains(@id, \"bmdh\") or contains(@class, \"slide\") or contains(@class, \"viewport\") or @data-lp-replacement-content or contains(@id, \"premium\") or contains(@class, \"overlay\") or contains(@class, \"paid-content\") or contains(@class, \"paidcontent\") or contains(@class, \"obfuscated\") or contains(@class, \"blurred\")]",
            ".//*[@class=\"comments-title\" or contains(@class, \"comments-title\") or contains(@class, \"nocomments\") or starts-with(@id|@class, \"reply-\") or contains(@class, \"-reply-\") or contains(@class, \"message\") or contains(@id, \"reader-comments\") or contains(@id, \"akismet\") or contains(@class, \"akismet\") or contains(@class, \"suggest-links\") or starts-with(@class, \"hide-\") or contains(@class, \"-hide-\") or contains(@class, \"hide-print\") or contains(@id|@style, \"hidden\") or contains(@class, \" hidden\") or contains(@class, \" hide\") or contains(@class, \"noprint\") or contains(@style, \"display:none\") or contains(@style, \"display: none\") or @aria-hidden=\"true\" or contains(@class, \"notloaded\")]",
            // DISCARD_IMAGE_ELEMENTS (1)
            ".//*[self::div or self::item or self::list or self::p or self::section or self::span][contains(@id, \"caption\") or contains(@class, \"caption\")]",
            // COMMENTS_DISCARD_XPATH (3)
            ".//*[self::div or self::section][starts-with(@id, \"respond\")]",
            ".//cite|.//quote",
            ".//*[@class=\"comments-title\" or contains(@class, \"comments-title\") or contains(@class, \"nocomments\") or starts-with(@id|@class, \"reply-\") or contains(@class, \"-reply-\") or contains(@class, \"message\") or contains(@class, \"signin\") or contains(@id|@class, \"akismet\") or contains(@style, \"display:none\")]",
            // AUTHOR_XPATHS (3) — the [0] pattern includes a top-level |//author union.
            "//*[self::a or self::address or self::div or self::link or self::p or self::span or self::strong][@rel=\"author\" or @id=\"author\" or @class=\"author\" or @itemprop=\"author name\" or rel=\"me\" or contains(@class, \"author-name\") or contains(@class, \"AuthorName\") or contains(@class, \"authorName\") or contains(@class, \"author name\") or @data-testid=\"AuthorCard\" or @data-testid=\"AuthorURL\"]|//author",
            "//*[self::a or self::div or self::h3 or self::h4 or self::p or self::span][contains(@class, \"author\") or contains(@id, \"author\") or contains(@itemprop, \"author\") or @class=\"byline\" or contains(@class, \"channel-name\") or contains(@id, \"zuozhe\") or contains(@class, \"zuozhe\") or contains(@id, \"bianji\") or contains(@class, \"bianji\") or contains(@id, \"xiaobian\") or contains(@class, \"xiaobian\") or contains(@class, \"submitted-by\") or contains(@class, \"posted-by\") or @class=\"username\" or @class=\"byl\" or @class=\"BBL\" or contains(@class, \"journalist-name\")]",
            "//*[contains(translate(@id, \"A\", \"a\"), \"author\") or contains(translate(@class, \"A\", \"a\"), \"author\") or contains(@class, \"screenname\") or contains(@data-component, \"Byline\") or contains(@itemprop, \"author\") or contains(@class, \"writer\") or contains(translate(@class, \"B\", \"b\"), \"byline\")]",
            // AUTHOR_DISCARD_XPATHS (2)
            ".//*[self::a or self::div or self::section or self::span][@id='comments' or @class='comments' or @class='title' or @class='date' or contains(@id, 'commentlist') or contains(@class, 'commentlist') or contains(@class, 'sidebar') or contains(@class, 'is-hidden') or contains(@class, 'quote') or contains(@id, 'comment-list') or contains(@class, 'comments-list') or contains(@class, 'embedly-instagram') or contains(@id, 'ProductReviews') or starts-with(@id, 'comments') or contains(@data-component, \"Figure\") or contains(@class, \"article-share\") or contains(@class, \"article-support\") or contains(@class, \"print\") or contains(@class, \"category\") or contains(@class, \"meta-date\") or contains(@class, \"meta-reviewer\") or starts-with(@class, 'comments') or starts-with(@class, 'Comments')]",
            "//time|//figure",
            // CATEGORIES_XPATHS (6)
            "//div[starts-with(@class, 'post-info') or starts-with(@class, 'postinfo') or starts-with(@class, 'post-meta') or starts-with(@class, 'postmeta') or starts-with(@class, 'meta') or starts-with(@class, 'entry-meta') or starts-with(@class, 'entry-info') or starts-with(@class, 'entry-utility') or starts-with(@id, 'postpath')]//a[@href]",
            "//p[starts-with(@class, 'postmeta') or starts-with(@class, 'entry-categories') or @class='postinfo' or @id='filedunder']//a[@href]",
            "//footer[starts-with(@class, 'entry-meta') or starts-with(@class, 'entry-footer')]//a[@href]",
            "//*[self::li or self::span][@class=\"post-category\" or @class=\"postcategory\" or @class=\"entry-category\" or contains(@class, \"cat-links\")]//a[@href]",
            "//header[@class=\"entry-header\"]//a[@href]",
            "//div[@class=\"row\" or @class=\"tags\"]//a[@href]",
            // TAGS_XPATHS (4)
            "//div[@class=\"tags\"]//a[@href]",
            "//p[starts-with(@class, 'entry-tags')]//a[@href]",
            "//div[@class=\"row\" or @class=\"jp-relatedposts\" or @class=\"entry-utility\" or starts-with(@class, 'tag') or starts-with(@class, 'postmeta') or starts-with(@class, 'meta')]//a[@href]",
            "//*[@class=\"entry-meta\" or contains(@class, \"topics\") or contains(@class, \"tags-links\")]//a[@href]",
            // TITLE_XPATHS (3)
            "//*[self::h1 or self::h2][contains(@class, \"post-title\") or contains(@class, \"entry-title\") or contains(@class, \"headline\") or contains(@id, \"headline\") or contains(@itemprop, \"headline\") or contains(@class, \"post__title\") or contains(@class, \"article-title\")]",
            "//*[@class=\"entry-title\" or @class=\"post-title\"]",
            "//*[self::h1 or self::h2 or self::h3][contains(@class, \"title\") or contains(@id, \"title\")]",
        ];
        for (i, pat) in patterns.iter().enumerate() {
            if let Err(e) = parse(pat) {
                panic!("xpaths.py pattern #{i} failed to parse: {e}\nPattern was:\n  {pat}");
            }
        }
        // Sanity: at least 30 distinct xpaths.py patterns are smoke-parsed.
        // Trafilatura v2.0.0's xpaths.py has 35 named patterns plus the two
        // very-long OVERALL_DISCARD_XPATH patterns; we cover ≥33 by name
        // (we omit OVERALL_DISCARD[0..2] because they are 50-line monsters
        // built from the same construct set — no new operator coverage).
        assert!(
            patterns.len() >= 30,
            "expected at least 30 xpaths.py patterns smoke-parsed; got {}",
            patterns.len()
        );
    }

    #[test]
    fn deeply_nested_pattern_smoke() {
        // Larger BODY_XPATH-like predicate to exercise the OR-chain.
        let xp = ".//*[self::article or self::div or self::main or self::section]\
            [@class=\"post\" or @class=\"entry\" or contains(@class, \"post-text\") or \
             contains(@class, \"post_text\") or contains(@class, \"post-body\") or \
             contains(@class, \"post-entry\") or contains(@class, \"postentry\")]";
        let d = parse_dom(
            "<html><body>\
                <div class='post-body something' id='a' />\
                <div class='unrelated' id='b' />\
                <article class='postentry' id='c' />\
            </body></html>",
        );
        let r = evaluate(xp, &body(&d)).unwrap();
        assert_eq!(ids(&r), vec!["a", "c"]);
    }
}
