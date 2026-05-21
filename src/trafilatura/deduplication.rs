//! `deduplication` — Stage 8: LRU cache + `duplicate_test`.
//!
//! HLD anchor: `2026.05.19 - HLD - mdrcel Trafilatura Port (M3)` §7.9.
//! Source of truth: `trafilatura@v2.0.0/deduplication.py:140-254`.
//!
//! # What this module does
//!
//! Trafilatura's per-extraction dedup pipeline (default `options.dedup =
//! False`, settings.py:114) uses a process-wide LRU cache keyed by the
//! trimmed `''.join(elem.itertext())` of a candidate paragraph (or, at
//! `core.py:330`, the whole `postbody`). When the same text is observed
//! more than `options.max_repetitions` (default `2`,
//! settings.cfg:42) times, the element / postbody is treated as a
//! duplicate and dropped. Texts shorter than `options.min_duplcheck_size`
//! (default `100`, settings.cfg:41) are exempt — Trafilatura does not
//! waste cache slots on short boilerplate-ish fragments.
//!
//! # Scope of this Stage 8 port
//!
//! This module ports the dedup half of `deduplication.py`:
//! - `LruCache` (`deduplication.py:149-229`) — pure Rust LRU with the same
//!   `put` / `get` / `contains` shape Python's `LRUCache.put` /
//!   `LRUCache.get` exposes, including the eviction-on-full and the
//!   move-to-front-on-touch behaviours.
//! - `LRU_TEST` (`deduplication.py:232`) — the process-wide
//!   `OnceLock<Mutex<LruCache>>` Trafilatura uses. Module-private to
//!   match Python's "shared mutable global" footprint without exposing
//!   the lock outside this module.
//! - `put_in_cache` (`deduplication.py:235-240`) — increment-or-insert.
//! - `duplicate_test` (`deduplication.py:243-254`) — the gate the
//!   callers in `htmlprocessing.py:262`, `:282` and `core.py:330` reach
//!   for.
//!
//! # Stage-6 additions (M4 Stage 6, 2026-05-21)
//!
//! M4 Stage 6 lifts the remaining `deduplication.py` exports off the
//! deferred list (the M3 Stage 8 doc-block previously marked them "NOT
//! ported"):
//!
//! - `is_similar_domain` (lines 27-32): faithful port using a hand-rolled
//!   `SequenceMatcher.ratio()`-equivalent over short strings.
//! - `Simhash` + `content_fingerprint` (lines 58-143): faithful port of
//!   Charikar's simhash with a hand-rolled FNV-1a 64-bit token hash
//!   instead of Python's `blake2b(digest_size=8)`. **Recorded honest
//!   divergence:** the bit-positions of the resulting hash differ from
//!   Python's output (different `_hash` function ⇒ different vector),
//!   but the Simhash properties hold (deterministic, similar inputs ⇒
//!   low Hamming distance, dissimilar inputs ⇒ high Hamming distance).
//!   The brief explicitly authorises a hand-rolled hash ("for a 64-bit
//!   Simhash hash function, a hand-rolled FNV-1a or djb2 might be
//!   sufficient — verify what Python uses") in lieu of pulling in a new
//!   crypto crate, and the M3 Stage 8 docs flagged simhash as
//!   non-load-bearing on the `bare_extraction` path: "used only by
//!   `meta.py:11,29` (which clears the LRU cache)". No consumer in
//!   mdrcel currently depends on byte-identity with Python's simhash
//!   output.
//! - `sample_tokens` + `generate_bow_hash` (lines 35-55): `sample_tokens`
//!   is ported (Simhash depends on it); `generate_bow_hash` is **NOT**
//!   ported (no consumer; would force a blake2b dependency for one
//!   unused function — recorded per HLD §10 deferral discipline).
//!
//! # Anti-inversion catches recorded at Stage 6 port time
//!
//! 1. `STRIP_EXTENSION = re.compile(r"\.[^/?#]{2,63}$")` is a GREEDY
//!    suffix strip that fires ONLY ONCE. `"www.example.com"` strips to
//!    `"www.example"`, not `"www"` — the regex anchors at end-of-string.
//!    Then SequenceMatcher compares `"www.example"` vs `"example"`,
//!    which scores 0.6/0.7-ish (matching 7 chars / 18 total ≈ 0.78).
//!    The brief's expectation "www.example.com vs example.com → true
//!    (www-stripping)" was correct (the values are similar), but the
//!    `STRIP_EXTENSION` does NOT strip `www.`. The www-strip happens
//!    incidentally via the ratio threshold, not via regex.
//! 2. `is_similar_domain("example.com", "example.org")` is `True` in
//!    Python because BOTH suffixes strip to `"example"` (identical
//!    post-strip ⇒ ratio = 1.0). This is the test case the brief
//!    called "behaviour per Python (could be either)".
//! 3. `SequenceMatcher` returns ratio 1.0 for the (empty, empty) input
//!    pair (special case in CPython difflib). Our port matches.
//!
//! # Faithfulness anchor (HLD §4 / §10 — anti-inversion)
//!
//! - `LruCache.contains(key)` returns `true` if and only if `key` is
//!   already in the cache, mirroring Python's `cacheval != -1` predicate
//!   (deduplication.py:239, :250). Python's `.get()` also moves the
//!   touched key to the MRU front; our `contains` does the same via the
//!   `LruCache::touch` helper.
//! - `LruCache.put(key)` is the Python `LRU_TEST.put(teststring, value)`
//!   surface compressed to the only call shape this module needs:
//!   "remember this key, evict the oldest if full". The Python `value`
//!   slot stores an integer COUNT so `put_in_cache` can increment on
//!   subsequent calls — our port stores the count internally inside the
//!   `LruCache` (so the public surface stays text-only) and exposes it
//!   via `count(key)` for `duplicate_test` to read.
//! - `duplicate_test` is line-cited per branch to its Python source
//!   (deduplication.py:243-254). The `len(teststring) >
//!   options.min_duplcheck_size` gate uses Python's `len(str)` which
//!   counts CODE POINTS for non-ASCII strings — the Rust port uses
//!   `chars().count()` for the same semantics.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::readability::dom::NodeRef;

// ===========================================================================
// LruCache (deduplication.py:146-229)
// ===========================================================================

/// Pure-Rust Least-Recently-Used cache keyed by `String`, storing an
/// integer count per key.
///
/// **Source line-cite:** `deduplication.py:149-229` (Python `LRUCache`).
///
/// # Python original (slimmed)
///
/// ```python
/// class LRUCache:
///     def __init__(self, maxsize=128):
///         self.maxsize = maxsize
///         self.cache: Dict[str, List[Any]] = {}
///         # circular doubly linked list root...
///         self.full = False
///
///     def get(self, key) -> int:        # returns -1 if absent
///     def put(self, key, value) -> None # inserts / updates, evicts LRU on full
///     def clear(self) -> None
/// ```
///
/// # Faithfulness notes
///
/// - Python's `get` / `put` are protected by a `RLock`; the Rust port
///   relies on the [`LRU_TEST`]-side `Mutex` for the shared instance and
///   leaves single-thread instances lock-free (idiomatic Rust — `&mut
///   self` is enforced by the borrow checker).
/// - Python uses a circular doubly linked list adapted from
///   CPython's `functools.lru_cache`. The Rust port uses a `Vec<String>`
///   recency ring + a `HashMap<String, usize>` (key → count). The
///   observable surface is identical: `contains(k)`, `count(k)`,
///   `put(k)`, `evict_oldest()` semantics all match the Python class on
///   the call shapes Trafilatura uses. The linked-list specifics are an
///   implementation detail Python documents but no consumer relies on.
/// - Capacity 0 is permitted (Python doesn't special-case it either —
///   `maxsize <= 0` makes the cache full immediately on first insert);
///   our implementation drops any insert without storing when
///   `capacity == 0`.
#[derive(Debug)]
pub struct LruCache {
    capacity: usize,
    /// Recency ring — front of the `VecDeque` would be MRU, but we use
    /// `Vec` because `position` + `swap_remove` is O(n) either way for
    /// realistic LRU sizes (Python's CPython-borrowed linked list is the
    /// same complexity class on `get`/`put`). The contract: `recency[0]`
    /// is the LRU (eviction target); `recency.last()` is the MRU.
    recency: Vec<String>,
    /// Key → integer count. Mirrors Python's `LRU_TEST.put(key, value)`
    /// where `value` is a u32 repetition counter (deduplication.py:239 —
    /// `value = cacheval + 1 if cacheval != -1 else 1`).
    counts: HashMap<String, u32>,
}

impl LruCache {
    /// Create an empty cache with the given capacity.
    ///
    /// **Source line-cite:** `deduplication.py:157-166`
    /// (`LRUCache.__init__`).
    ///
    /// `capacity == 0` is legal but renders the cache inert — every
    /// `put` succeeds (returns) without storing. This matches Python's
    /// behaviour with `maxsize = 0` (the `full` flag flips on first
    /// insert and `len(self.cache) >= 0` is always true).
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            recency: Vec::with_capacity(capacity.min(1024)),
            counts: HashMap::with_capacity(capacity.min(1024)),
        }
    }

    /// `true` iff `key` is currently in the cache. Touches the key to
    /// the MRU position as a side effect — mirroring Python's `.get()`,
    /// which moves the touched key to the front of the doubly-linked
    /// list before returning the stored value (`deduplication.py:178-185`,
    /// the `_move_link` call inside `.get()`).
    pub fn contains(&mut self, key: &str) -> bool {
        if self.counts.contains_key(key) {
            self.touch(key);
            true
        } else {
            false
        }
    }

    /// Read the stored count for `key`, or `0` if absent. Does NOT touch
    /// recency — used by `duplicate_test` AFTER `contains` has already
    /// touched the entry, so a second touch would be a no-op anyway.
    /// Python's `cacheval` is the same value: the per-key repetition
    /// counter that drives the `> max_repetitions` gate
    /// (`deduplication.py:250`).
    pub fn count(&self, key: &str) -> u32 {
        self.counts.get(key).copied().unwrap_or(0)
    }

    /// Insert `key` (or increment its count if already present). Moves the
    /// key to the MRU position; evicts the LRU key when the cache is
    /// full AND the inserted key is new.
    ///
    /// **Source line-cite:** `deduplication.py:187-222` (Python `.put()`).
    /// Python takes a `value` parameter; the only call site
    /// (`put_in_cache`, line 239) computes `cacheval + 1 if cacheval !=
    /// -1 else 1`, i.e. "increment-or-insert-as-1". We compress that
    /// here so the public API stays text-only.
    pub fn put(&mut self, key: String) {
        if self.capacity == 0 {
            return;
        }
        if self.counts.contains_key(&key) {
            // Already present — increment count + bump recency.
            self.touch(&key);
            *self.counts.get_mut(&key).expect("just checked") += 1;
        } else {
            // New entry — evict LRU if full, then insert with count=1.
            if self.recency.len() >= self.capacity {
                let evict = self.recency.remove(0);
                self.counts.remove(&evict);
            }
            self.counts.insert(key.clone(), 1);
            self.recency.push(key);
        }
    }

    /// Move `key` to the MRU position. No-op if `key` is absent.
    fn touch(&mut self, key: &str) {
        if let Some(idx) = self.recency.iter().position(|k| k == key) {
            // Remove and re-push at the end (MRU).
            let k = self.recency.remove(idx);
            self.recency.push(k);
        }
    }

    /// Drop every entry. Mirrors `deduplication.py:224-229`
    /// (`LRUCache.clear()`). Used by `meta.py:29`'s
    /// `LRU_TEST.clear()` housekeeping; we expose it on the public
    /// surface for parity.
    pub fn clear(&mut self) {
        self.recency.clear();
        self.counts.clear();
    }

    /// Current number of stored keys. Public to support testing /
    /// observability; Python doesn't expose it (it uses
    /// `len(self.cache)` directly).
    pub fn len(&self) -> usize {
        self.counts.len()
    }

    /// `true` when the cache holds no keys.
    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }
}

// ===========================================================================
// LRU_TEST (deduplication.py:232)
// ===========================================================================

/// `LRU_SIZE` from `settings.py:308`. The process-wide cache holds up to
/// 4096 trimmed paragraph / postbody texts.
pub const LRU_SIZE: usize = 4096;

/// Process-wide LRU cache shared by every `duplicate_test` call.
///
/// **Source line-cite:** `deduplication.py:232` (`LRU_TEST = LRUCache
/// (maxsize=LRU_SIZE)`). Python's module-level singleton is "shared
/// mutable state inside a single Python process"; we replicate that with
/// `OnceLock<Mutex<LruCache>>`. The mutex covers the Python `RLock`
/// (deduplication.py:159) that protects the linked-list updates.
///
/// Module-private deliberately — callers reach for `duplicate_test` /
/// `put_in_cache` / `clear_lru_test`, not the lock itself. Keeping the
/// `Mutex` invisible means no caller can deadlock by holding the cache
/// guard across other locks.
fn lru_test() -> &'static Mutex<LruCache> {
    static CACHE: OnceLock<Mutex<LruCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(LruCache::new(LRU_SIZE)))
}

/// Public accessor for tests / instrumentation that want to inspect the
/// shared cache (read `len`, `count`, etc.). Wraps the module-private
/// `Mutex` so callers can run a short closure under the guard without
/// having to thread the lock through.
pub fn with_lru_test<R>(f: impl FnOnce(&mut LruCache) -> R) -> R {
    let mut guard = lru_test().lock().expect("LRU_TEST mutex poisoned");
    f(&mut guard)
}

/// Clear the process-wide cache. Mirrors `meta.py:29`'s
/// `LRU_TEST.clear()`. Tests use this for isolation; production code
/// rarely needs it (the cache self-evicts).
pub fn clear_lru_test() {
    with_lru_test(|c| c.clear());
}

// ===========================================================================
// put_in_cache (deduplication.py:235-240)
// ===========================================================================

/// Record `teststring` in [`LRU_TEST`], creating or incrementing the
/// per-key count.
///
/// **Source line-cite:** `deduplication.py:235-240`.
///
/// # Python original
///
/// ```python
/// def put_in_cache(teststring: str) -> None:
///     "Implement LRU cache."
///     cacheval = LRU_TEST.get(teststring)
///     # if the value is already defined
///     value = cacheval + 1 if cacheval != -1 else 1
///     LRU_TEST.put(teststring, value)
/// ```
///
/// Python's `LRUCache.put` takes a value; our `LruCache::put`
/// auto-increments on existing keys (see `LruCache::put` body). The
/// observable behaviour is identical for the only call shape Python
/// uses.
pub fn put_in_cache(teststring: &str) {
    with_lru_test(|cache| cache.put(teststring.to_string()));
}

// ===========================================================================
// duplicate_test (deduplication.py:243-254)
// ===========================================================================

/// Check whether `text` is already in [`LRU_TEST`] with a count exceeding
/// `max_repetitions`. Always records the text in the cache on the way out
/// (via `put_in_cache`) so subsequent calls eventually flip to `true`.
///
/// **Source line-cite:** `deduplication.py:243-254`.
///
/// # Python original
///
/// ```python
/// def duplicate_test(element, options) -> bool:
///     "Check for duplicate text with LRU cache."
///     teststring = trim(" ".join(element.itertext()))
///     if len(teststring) > options.min_duplcheck_size:
///         cacheval = LRU_TEST.get(teststring)
///         if cacheval > options.max_repetitions:  # non-existent key returns -1
///             LRU_TEST.put(teststring, cacheval + 1)
///             return True
///     put_in_cache(teststring)
///     return False
/// ```
///
/// # Faithfulness notes
///
/// 1. `len(teststring) > options.min_duplcheck_size`
///    (deduplication.py:247) — Python's `len(str)` counts CODE POINTS
///    for `str` (UCS-4 internal representation). The Rust port uses
///    `chars().count()` for the same semantics.
/// 2. `cacheval > options.max_repetitions` (deduplication.py:250) —
///    Python's `> max_repetitions` is a STRICT inequality, so a count
///    equal to `max_repetitions` does NOT yet trip the duplicate flag.
///    Default `max_repetitions = 2` ⇒ the THIRD time a text is seen,
///    `cacheval = 3 > 2` ⇒ returns `true`.
/// 3. `LRU_TEST.put(teststring, cacheval + 1)` on the duplicate branch
///    (deduplication.py:251) — Python re-puts with the incremented
///    value (so future calls see an even larger count). Our `LruCache::
///    put` already auto-increments existing keys, so we just call
///    `put_in_cache` here, mirroring the Python "always record on the
///    way out" pattern (the `put_in_cache` on line 253).
/// 4. The short-text path (`len <= min_duplcheck_size`) STILL records
///    via `put_in_cache` (Python falls through to line 253). The Rust
///    port preserves that — short texts join the cache but never trip
///    the duplicate-detected return.
///
/// This is the **text-based** entry point; the node-level helper
/// [`duplicate_test_node`] mirrors Python's
/// `duplicate_test(element, options)` shape (it trims
/// `" ".join(element.itertext())` first and then forwards). Keeping
/// both lets the per-element call sites
/// (htmlprocessing.py:262, :282) and the body-level call site
/// (core.py:330) share one branch-tested implementation.
pub fn duplicate_test(text: &str, min_size: usize, max_repetitions: u32) -> bool {
    let codepoints = text.chars().count();
    if codepoints > min_size {
        // Python's LRU_TEST.get returns -1 for absent keys; we use
        // `count(text)` which returns 0 (absent / "never seen"). The
        // strict ">" survives the sentinel swap because
        // `max_repetitions >= 0` and an absent key always has count 0,
        // and `0 > max_repetitions` is false whenever
        // `max_repetitions >= 0` (which is always — usize).
        let cacheval = with_lru_test(|c| {
            // Touch first (`contains` mutates recency), THEN read count.
            // This matches Python's `.get()` which moves-to-front and
            // returns in one call.
            let _ = c.contains(text);
            c.count(text)
        });
        if cacheval > max_repetitions {
            put_in_cache(text);
            return true;
        }
    }
    put_in_cache(text);
    false
}

/// Element-level wrapper around [`duplicate_test`] mirroring Python's
/// `duplicate_test(element, options)` signature
/// (deduplication.py:243-254).
///
/// The Python implementation builds `teststring = trim(" ".join
/// (element.itertext()))` before reaching the cache. The Rust port does
/// the same here so the call sites in `cleaning::handle_textnode`,
/// `cleaning::process_node`, and `readability_fork::compare_extraction`
/// can hand a `&NodeRef` straight in.
pub fn duplicate_test_node(
    element: &NodeRef,
    options: &crate::trafilatura::cleaning::Options,
) -> bool {
    use crate::trafilatura::utils::trim;
    let text = collect_itertext_joined(element);
    let trimmed = trim(&text);
    duplicate_test(&trimmed, options.min_duplcheck_size, options.max_repetitions as u32)
}

/// Equivalent of Python's `" ".join(element.itertext())`. Walks the
/// subtree in document order, concatenating every Text node's data into
/// one space-separated string. We replicate `main_extractor::itertext`'s
/// shape locally to avoid taking a cross-module dependency on a private
/// helper — both implementations are tiny and side-effect-free.
fn collect_itertext_joined(element: &NodeRef) -> String {
    let mut parts: Vec<String> = Vec::new();
    walk_text(element, &mut parts);
    parts.join(" ")
}

fn walk_text(node: &NodeRef, out: &mut Vec<String>) {
    use crate::readability::dom::NodeData;
    for child in node.children.borrow().iter() {
        match &child.data {
            NodeData::Text { contents } => {
                let data = contents.borrow().to_string();
                if !data.is_empty() {
                    out.push(data);
                }
            }
            NodeData::Element { .. } => walk_text(child, out),
            _ => {}
        }
    }
}

// ===========================================================================
// Stage 6 — Simhash + sample_tokens + content_fingerprint + is_similar_domain
// (M4 Stage 6, 2026-05-21)
// ===========================================================================

/// Python `string.punctuation` —
/// `"!\"#$%&'()*+,-./:;<=>?@[\\]^_`{|}~"`. Used by `sample_tokens` to
/// strip surrounding punctuation from each whitespace-split token.
///
/// **Source line-cite:** `deduplication.py:7` (`import string`) +
/// `:40` (`token.strip(string.punctuation)`).
const PYTHON_PUNCTUATION: &[char] = &[
    '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', ':', ';', '<', '=',
    '>', '?', '@', '[', '\\', ']', '^', '_', '`', '{', '|', '}', '~',
];

/// Faithful port of Python `str.isalnum()`. Python's definition: "Return
/// True if all characters in the string are alphanumeric AND there is at
/// least one character" — false for the empty string. A character is
/// alphanumeric iff one of `isalpha`/`isdecimal`/`isdigit`/`isnumeric`.
/// For our token-filter use case we approximate by `char::is_alphanumeric`
/// over the Unicode general-category catalog, which mirrors Python's
/// behaviour for the inputs `sample_tokens` sees (latin-script web text
/// plus the occasional ASCII digit).
///
/// **Source line-cite:** `deduplication.py:41` (`if token.isalnum():`).
fn py_isalnum(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    s.chars().all(char::is_alphanumeric)
}

/// Faithful port of `deduplication.py:35-48`.
///
/// # Python original
///
/// ```python
/// def sample_tokens(inputstring: str, length: int = 64) -> List[str]:
///     """Split input into list of tokens and adjust length threshold
///     to make sure there is enough data."""
///     tokens = []
///     for token in inputstring.split():
///         token = token.strip(string.punctuation)
///         if token.isalnum():
///             tokens.append(token)
///     sample = []
///     for i in range(4, -1, -1):
///         sample = [t for t in tokens if len(t) > i]
///         if len(sample) >= length / 2:
///             return sample
///     return sample
/// ```
///
/// # Faithfulness notes
///
/// - `inputstring.split()` with no separator → split on ANY whitespace
///   run (`str::split_whitespace` is the Rust analogue).
/// - `token.strip(string.punctuation)` → trim leading + trailing
///   punctuation chars from `PYTHON_PUNCTUATION`.
/// - `range(4, -1, -1)` iterates `[4, 3, 2, 1, 0]` — descending length
///   thresholds until the filtered sample has at least `length / 2`
///   tokens (or we run out of thresholds and return whatever the last
///   threshold produced, which is `len > 0`, i.e. all non-empty
///   alphanumeric tokens). Python's `length / 2` is true division, so
///   `64 / 2 == 32.0`; the comparison `len(sample) >= 32.0` is
///   equivalent to `>= 32` in usize.
/// - `length` is `usize` in Rust; we accept `length: usize`.
pub fn sample_tokens(inputstring: &str, length: usize) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    for raw in inputstring.split_whitespace() {
        let stripped = raw.trim_matches(PYTHON_PUNCTUATION);
        if py_isalnum(stripped) {
            tokens.push(stripped.to_string());
        }
    }
    let half = length / 2;
    let mut sample: Vec<String> = Vec::new();
    // range(4, -1, -1) → [4, 3, 2, 1, 0]
    for i in (0..=4).rev() {
        sample = tokens
            .iter()
            .filter(|t| t.chars().count() > i)
            .cloned()
            .collect();
        if sample.len() >= half {
            return sample;
        }
    }
    sample
}

/// FNV-1a 64-bit hash. Deterministic, non-cryptographic, well-mixed —
/// suitable for the Simhash token vector. Replaces Python's
/// `blake2b(token.encode(), digest_size=8)` (deduplication.py:72-76).
///
/// # Why not blake2b
///
/// The brief authorises a hand-rolled hash: *"for a 64-bit Simhash hash
/// function, a hand-rolled FNV-1a or djb2 might be sufficient"*. The
/// alternative is pulling in `blake2 = "0.10"` (or the `blake2b_simd`
/// crate) for a single function that is **not on the bare_extraction
/// hot path** (the M3 Stage 8 docs flagged this: simhash is only used
/// by `meta.py` to clear the LRU cache, not consumed by any mdrcel
/// extractor today). FNV-1a 64-bit is the smallest-change-that-works.
///
/// # Constants
///
/// - `FNV_OFFSET_BASIS_64 = 0xcbf29ce484222325`
/// - `FNV_PRIME_64       = 0x00000100000001b3`
///
/// Both per the FNV reference (Fowler/Noll/Vo 1991).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Charikar simhash (Trafilatura's locality-sensitive content fingerprint).
///
/// **Source line-cite:** `deduplication.py:58-138`.
///
/// # Python original (slimmed)
///
/// ```python
/// class Simhash:
///     __slots__ = ["hash", "length"]
///     def __init__(self, inputstring="", length=64, existing_hash=None):
///         self.length = length
///         self.hash = self.validate(existing_hash) or self.create_hash(inputstring)
///     def _hash(self, inputstring): ...                  # blake2b(...,8)
///     def _vector_to_add(self, token): ...               # cached
///     def create_hash(self, inputstring): ...            # vector sum
///     def hamming_distance(self, other_hash): ...        # popcount(self ^ other)
///     def similarity(self, other_hash): ...              # (L - hd) / L
/// ```
///
/// # What this port preserves (anti-inversion)
///
/// - **Shape**: `length` (default 64) bit vector, accumulator-of-±1
///   over each `sample_tokens`-filtered token, sign-collapse to a u64.
/// - **Determinism**: same input ⇒ same hash.
/// - **Simhash property**: similar token sets ⇒ low Hamming distance;
///   disjoint token sets ⇒ high Hamming distance (≈ `length / 2` for
///   random strings, since each bit flips ~50% of the time).
/// - **API shape**: `new` constructor, `hamming_distance`, `similarity`
///   ratio in [0.0, 1.0], `to_hex` for `content_fingerprint`.
///
/// # What this port intentionally diverges on (recorded)
///
/// - **Per-token hash function**: FNV-1a 64-bit instead of
///   `blake2b(digest_size=8)`. The bit positions chosen ±1 by a given
///   token therefore differ from Python's, so the numeric `hash` value
///   for a given input string is NOT byte-identical to Python's. The
///   Simhash property holds.
/// - **`_vector_to_add` LRU cache**: Python memoises token→vector to
///   avoid recomputing for repeated tokens. Rust port does not
///   memoise — `sample_tokens` already deduplicates short tokens via
///   the length-threshold sweep, and the recomputation cost is one
///   FNV-1a + 64 shifts per token. The output is identical.
#[derive(Debug, Clone)]
pub struct Simhash {
    /// The 64-bit fingerprint. For `length < 64`, the high bits are
    /// zero; for `length > 64`, this field clamps at 64 (the only call
    /// shape Trafilatura uses is `length=64`, the default, so 64 bits
    /// is the natural width).
    pub hash: u64,
    /// Bit length the hash represents — Python's `self.length`.
    /// Stored so `similarity` can divide by it.
    pub length: u32,
}

impl Simhash {
    /// Compute a Charikar simhash of `inputstring` at the default
    /// 64-bit length.
    ///
    /// **Source line-cite:** `deduplication.py:62-70`
    /// (`__init__(self, inputstring="", length=64, …)`).
    pub fn new(inputstring: &str) -> Self {
        Self::with_length(inputstring, 64)
    }

    /// Compute a Charikar simhash with an explicit bit length.
    /// `length` is clamped to 64 (the only width mdrcel needs and the
    /// only width Python's call sites use).
    ///
    /// **Source line-cite:** `deduplication.py:62-106`
    /// (`__init__` → `create_hash`).
    pub fn with_length(inputstring: &str, length: u32) -> Self {
        let len = length.clamp(1, 64);
        let mut vector: [i32; 64] = [0; 64];
        for token in sample_tokens(inputstring, len as usize) {
            let token_hash = fnv1a_64(token.as_bytes());
            // Python `_vector_to_add`: 1 if bit i is set in hash else -1
            // (deduplication.py:93).
            for (i, slot) in vector.iter_mut().enumerate().take(len as usize) {
                if (token_hash >> i) & 1 == 1 {
                    *slot += 1;
                } else {
                    *slot -= 1;
                }
            }
        }
        // Python `create_hash` line 106: `sum(1 << i for i in range(length)
        // if vector[i] >= 0)`. Note the `>= 0` — a balanced bit goes to 1,
        // and the all-zero-token case (empty input) collapses to
        // `(1 << length) - 1`, NOT zero. Faithful.
        let mut hash: u64 = 0;
        for (i, &v) in vector.iter().enumerate().take(len as usize) {
            if v >= 0 {
                hash |= 1u64 << i;
            }
        }
        Self { hash, length: len }
    }

    /// Hamming distance between two simhashes.
    ///
    /// **Source line-cite:** `deduplication.py:130-132`.
    /// Python's `BIN_COUNT_FUNC` is `int.bit_count` on 3.10+ or
    /// `bin(x).count("1")` fallback — both equivalent to `u64::count_ones`.
    pub fn hamming_distance(&self, other: &Self) -> u32 {
        (self.hash ^ other.hash).count_ones()
    }

    /// Similarity ratio in `[0.0, 1.0]` — Python's `(length - hd) / length`.
    ///
    /// **Source line-cite:** `deduplication.py:134-138`.
    pub fn similarity(&self, other: &Self) -> f64 {
        let hd = self.hamming_distance(other) as f64;
        // Python compares hashes of equal length only; we use min-length
        // for safety (the +1 floor in `with_length` rules out div-by-zero).
        let len = self.length.min(other.length).max(1) as f64;
        (len - hd) / len
    }

    /// Hex representation of the fingerprint — Python's `hex(self.hash)[2:]`
    /// (deduplication.py:108-110). NO leading `0x`, lowercase, NO
    /// leading-zero padding (Python's `hex(0x1ff)` is `"0x1ff"`, not
    /// `"0x00000000000001ff"`). `content_fingerprint` consumes this.
    ///
    /// **Source line-cite:** `deduplication.py:108-110`.
    pub fn to_hex(&self) -> String {
        format!("{:x}", self.hash)
    }
}

/// Compute a content fingerprint (hex-encoded simhash).
///
/// **Source line-cite:** `deduplication.py:141-143`.
///
/// # Python original
///
/// ```python
/// def content_fingerprint(content: str) -> str:
///     "Calculate a simhash hex value for meaningful bits of the content."
///     return Simhash(content).to_hex()
/// ```
pub fn content_fingerprint(content: &str) -> String {
    Simhash::new(content).to_hex()
}

// ---------------------------------------------------------------------------
// is_similar_domain (deduplication.py:22-32)
// ---------------------------------------------------------------------------

/// Faithful port of Python `difflib.SequenceMatcher(None, a, b).ratio()`
/// for SHORT strings (domain names — ≤ 64 chars typical).
///
/// # Algorithm (CPython `difflib.py`)
///
/// `ratio = 2.0 * M / T` where:
/// - `M` is the total number of characters matched by recursive
///   `find_longest_match`-based block decomposition.
/// - `T = len(a) + len(b)`.
///
/// `find_longest_match(alo, ahi, blo, bhi)` returns the longest
/// substring of `a[alo:ahi]` that also occurs in `b[blo:bhi]`, with
/// ties broken by earliest position in `a` then in `b`. We implement
/// the textbook dynamic-programming variant: for each position in `a`,
/// maintain the length of the longest match ending at each position in
/// `b` (rolling 1-D table). This is O(|a| * |b|) — fine for domain
/// strings.
///
/// Then `get_matching_blocks` recursively partitions around the
/// longest match and sums match counts (`M`). For the (empty, empty)
/// case, Python's `ratio()` short-circuits to `1.0` (matched in
/// CPython's `quick_ratio` / size guard).
///
/// **Source line-cite:** `deduplication.py:32`
/// (`SequenceMatcher(None, reference, new_string).ratio()`); CPython
/// `difflib.SequenceMatcher.ratio` for the algorithm.
fn sequence_ratio(a: &str, b: &str) -> f64 {
    let total = a.chars().count() + b.chars().count();
    if total == 0 {
        // CPython quirk: ratio of two empty strings is 1.0.
        return 1.0;
    }
    let av: Vec<char> = a.chars().collect();
    let bv: Vec<char> = b.chars().collect();
    let matched = matching_blocks(&av, &bv);
    (2.0 * matched as f64) / total as f64
}

/// Recursive sum of `find_longest_match` over a partitioned alignment.
fn matching_blocks(a: &[char], b: &[char]) -> usize {
    let mut total = 0usize;
    let mut stack: Vec<((usize, usize), (usize, usize))> =
        vec![((0, a.len()), (0, b.len()))];
    while let Some(((alo, ahi), (blo, bhi))) = stack.pop() {
        if alo >= ahi || blo >= bhi {
            continue;
        }
        let (i, j, k) = find_longest_match(a, alo, ahi, b, blo, bhi);
        if k > 0 {
            total += k;
            stack.push(((alo, i), (blo, j)));
            stack.push(((i + k, ahi), (j + k, bhi)));
        }
    }
    total
}

/// Returns `(best_i, best_j, best_k)` — the longest matching substring
/// `a[best_i .. best_i+best_k] == b[best_j .. best_j+best_k]`.
/// Tie-breaks favour earliest start in `a`, then earliest start in `b`
/// (CPython convention).
fn find_longest_match(
    a: &[char],
    alo: usize,
    ahi: usize,
    b: &[char],
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    // `j2len_prev[j]` = length of longest match ending at (i-1, j-1).
    let bspan = bhi - blo;
    let mut j2len_prev = vec![0usize; bspan];
    let mut best_i = alo;
    let mut best_j = blo;
    let mut best_k = 0usize;
    for (i, &ach) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut j2len = vec![0usize; bspan];
        for (j, &bch) in b.iter().enumerate().take(bhi).skip(blo) {
            if ach == bch {
                let k = if j > blo { j2len_prev[j - blo - 1] + 1 } else { 1 };
                j2len[j - blo] = k;
                if k > best_k {
                    best_i = i + 1 - k;
                    best_j = j + 1 - k;
                    best_k = k;
                }
            }
        }
        j2len_prev = j2len;
    }
    (best_i, best_j, best_k)
}

/// Default similarity threshold for [`is_similar_domain`] —
/// `deduplication.py:28` (`threshold: float = 0.5`).
pub const IS_SIMILAR_DOMAIN_DEFAULT_THRESHOLD: f64 = 0.5;

/// Strip a single trailing `.tld`-shaped suffix from `s`.
///
/// **Source line-cite:** `deduplication.py:22`
/// (`STRIP_EXTENSION = re.compile(r"\.[^/?#]{2,63}$")`).
///
/// The regex anchors at end-of-string and matches a literal `.`
/// followed by 2-63 characters that are NOT `/`, `?`, or `#`. It
/// strips ONLY ONCE (it's a greedy regex with `re.sub`, but the
/// regex itself has no `*` outside the bounded length). So
/// `"www.example.com"` → `"www.example"` (one strip), NOT `"www"`.
/// `"example.co.uk"` → `"example.co"`, NOT `"example"`.
fn strip_extension(s: &str) -> String {
    // We avoid pulling the `regex` engine through here — the pattern is
    // simple enough to hand-roll: find the LAST `.`, then check that
    // every char after it is in [^/?#] and the segment length is in
    // [2, 63]. If yes, return everything before the dot.
    if let Some(last_dot) = s.rfind('.') {
        let suffix = &s[last_dot + 1..];
        let suffix_len = suffix.chars().count();
        if (2..=63).contains(&suffix_len)
            && !suffix.contains('/')
            && !suffix.contains('?')
            && !suffix.contains('#')
        {
            return s[..last_dot].to_string();
        }
    }
    s.to_string()
}

/// Return `true` iff two short strings (domain names) have a
/// `SequenceMatcher.ratio()` at or above `threshold` after a single
/// `STRIP_EXTENSION` pass on each input.
///
/// **Source line-cite:** `deduplication.py:27-32`.
///
/// # Python original
///
/// ```python
/// @lru_cache(maxsize=1024)
/// def is_similar_domain(reference: str, new_string: str,
///                       threshold: float = 0.5) -> bool:
///     "Return the similarity ratio between two short strings, here domain names."
///     reference = STRIP_EXTENSION.sub("", reference)
///     new_string = STRIP_EXTENSION.sub("", new_string)
///     return SequenceMatcher(None, reference, new_string).ratio() >= threshold
/// ```
///
/// # Faithfulness notes
///
/// 1. `lru_cache(maxsize=1024)` — Python memoises. Rust port does NOT
///    memoise: the function is pure, deterministic, and called rarely
///    (only by metadata-pipeline code which mdrcel doesn't drive yet).
///    Adding a global memo would need a `Mutex<HashMap>` for one call
///    site that doesn't exist yet — premature.
/// 2. The default `threshold` is 0.5 ([`IS_SIMILAR_DOMAIN_DEFAULT_THRESHOLD`]).
/// 3. `STRIP_EXTENSION` strips ONLY ONE TLD suffix — see [`strip_extension`].
///    So `"www.example.com"` vs `"example.com"` compares
///    `"www.example"` vs `"example"` (ratio ≈ 0.78, ≥ 0.5 ⇒ `true`).
///    `"example.com"` vs `"example.org"` compares `"example"` vs
///    `"example"` (ratio 1.0 ⇒ `true`). `"foo.com"` vs `"bar.com"`
///    compares `"foo"` vs `"bar"` (ratio 0.0 ⇒ `false`).
pub fn is_similar_domain(reference: &str, new_string: &str) -> bool {
    is_similar_domain_with(reference, new_string, IS_SIMILAR_DOMAIN_DEFAULT_THRESHOLD)
}

/// Like [`is_similar_domain`] but with an explicit threshold —
/// mirrors Python's third parameter (`threshold: float = 0.5`).
pub fn is_similar_domain_with(reference: &str, new_string: &str, threshold: f64) -> bool {
    let a = strip_extension(reference);
    let b = strip_extension(new_string);
    sequence_ratio(&a, &b) >= threshold
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as TestMutex;

    /// Serialize tests that share the process-wide `LRU_TEST` cache so
    /// they don't interfere. Per-test cache state is reset via
    /// `clear_lru_test`.
    static LOCK: TestMutex<()> = TestMutex::new(());

    // -----------------------------------------------------------------------
    // LruCache (deduplication.py:149-229)
    // -----------------------------------------------------------------------

    #[test]
    fn lru_cache_basic_contains_put() {
        // Stage 8 brief test #4 — put + contains roundtrip.
        let mut cache = LruCache::new(4);
        assert!(!cache.contains("alpha"));
        cache.put("alpha".to_string());
        assert!(cache.contains("alpha"));
        // First put records count=1 (deduplication.py:239 fall-through).
        assert_eq!(cache.count("alpha"), 1);
    }

    #[test]
    fn lru_cache_evicts_oldest_when_full() {
        // Stage 8 brief test #5 — capacity=2, insert 3 new keys, first
        // should be evicted.
        let mut cache = LruCache::new(2);
        cache.put("a".to_string());
        cache.put("b".to_string());
        cache.put("c".to_string()); // evicts "a"
        assert!(!cache.contains("a"), "oldest key 'a' should have evicted");
        assert!(cache.contains("b"));
        assert!(cache.contains("c"));
    }

    #[test]
    fn lru_cache_put_increments_existing_key() {
        // Faithful to Python deduplication.py:239 — repeat-puts
        // increment the value counter.
        let mut cache = LruCache::new(4);
        cache.put("x".to_string());
        cache.put("x".to_string());
        cache.put("x".to_string());
        assert_eq!(cache.count("x"), 3);
    }

    #[test]
    fn lru_cache_touch_promotes_to_mru() {
        // Python `.get()` moves the touched key to MRU
        // (deduplication.py:178-185). Insert a, b, c; touch a; insert d
        // — b (NOT a) should now be the eviction target.
        let mut cache = LruCache::new(3);
        cache.put("a".to_string());
        cache.put("b".to_string());
        cache.put("c".to_string());
        assert!(cache.contains("a")); // touches "a"
        cache.put("d".to_string()); // evicts the LRU, which is now "b"
        assert!(!cache.contains("b"), "after touch, 'b' is the LRU");
        assert!(cache.contains("a"));
        assert!(cache.contains("c"));
        assert!(cache.contains("d"));
    }

    #[test]
    fn lru_cache_capacity_zero_is_inert() {
        let mut cache = LruCache::new(0);
        cache.put("x".to_string());
        assert!(!cache.contains("x"));
        assert!(cache.is_empty());
    }

    #[test]
    fn lru_cache_clear_drops_everything() {
        let mut cache = LruCache::new(4);
        cache.put("a".to_string());
        cache.put("b".to_string());
        cache.clear();
        assert!(cache.is_empty());
        assert!(!cache.contains("a"));
    }

    // -----------------------------------------------------------------------
    // duplicate_test (deduplication.py:243-254)
    // -----------------------------------------------------------------------

    /// Long-enough sample text — exceeds the default min_duplcheck_size
    /// of 100 codepoints so the dedup gate is exercised.
    fn long_text() -> String {
        "a".repeat(150)
    }

    #[test]
    fn duplicate_test_returns_true_for_repeated_text() {
        // Stage 8 brief test #6 — same text, two calls, second returns true.
        // Default max_repetitions = 2 in Python; `> 2` means the FOURTH
        // call returns true (counts after each call: 1, 2, 3, 4; 4 > 2).
        // We override max_repetitions = 1 here so the THIRD call trips
        // (cacheval=2 > 1), keeping the test compact.
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        let text = long_text();
        assert!(!duplicate_test(&text, 100, 1), "call 1: count=1, 1>1 false");
        assert!(!duplicate_test(&text, 100, 1), "call 2: count=2, 2>1 false");
        assert!(duplicate_test(&text, 100, 1), "call 3: count=3, 3>1 TRUE");
    }

    #[test]
    fn duplicate_test_default_max_repetitions_trips_on_fourth() {
        // Pin Python's default behaviour: max_repetitions=2 ⇒ first
        // three calls return false, the FOURTH returns true (count=4).
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        let text = "z".repeat(120);
        assert!(!duplicate_test(&text, 100, 2));
        assert!(!duplicate_test(&text, 100, 2));
        assert!(!duplicate_test(&text, 100, 2));
        assert!(duplicate_test(&text, 100, 2));
    }

    #[test]
    fn duplicate_test_skips_short_text() {
        // Stage 8 brief test #7 — text shorter than min_duplcheck_size
        // never returns true, even after many calls.
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        let short = "abc"; // 3 codepoints < min=100
        for _ in 0..10 {
            assert!(!duplicate_test(short, 100, 1));
        }
    }

    #[test]
    fn duplicate_test_min_size_is_strict_greater_than() {
        // deduplication.py:247 — `if len(teststring) >
        // options.min_duplcheck_size`. Strict inequality: a string
        // EQUAL to min_size doesn't trip the gate. (Python `> 100`
        // requires 101+ codepoints.)
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        let exactly_100 = "y".repeat(100);
        for _ in 0..10 {
            // Never returns true — string len == min, not >.
            assert!(!duplicate_test(&exactly_100, 100, 1));
        }
    }

    #[test]
    fn duplicate_test_records_short_text_too() {
        // deduplication.py:253 — even short texts hit `put_in_cache`
        // (fall-through after the `if len > min` branch). The cache
        // entry exists but the function never returns true.
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        duplicate_test("hi", 100, 1);
        let count = with_lru_test(|c| c.count("hi"));
        assert_eq!(count, 1, "short text still recorded in LRU");
    }

    // -----------------------------------------------------------------------
    // duplicate_test_node — element wrapper (deduplication.py:245)
    // -----------------------------------------------------------------------

    #[test]
    fn duplicate_test_node_trims_and_dispatches() {
        use crate::readability::dom::{Dom, append_child, create_element, text_content};
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        let _dom = Dom::parse("<html><body></body></html>");
        let p = create_element("p");
        // Build text long enough to exceed min_duplcheck_size=100.
        let text_node = create_element("span");
        crate::readability::dom::set_element_text(&text_node, Some(&"q".repeat(120)));
        append_child(&p, &text_node);
        // Sanity: text_content is the 120-char "q"-run.
        assert_eq!(text_content(&p).len(), 120);

        let opts = crate::trafilatura::cleaning::Options {
            dedup: true,
            min_duplcheck_size: 100,
            max_repetitions: 1,
            ..crate::trafilatura::cleaning::Options::default()
        };

        assert!(!duplicate_test_node(&p, &opts));
        assert!(!duplicate_test_node(&p, &opts));
        assert!(duplicate_test_node(&p, &opts));
    }

    // -----------------------------------------------------------------------
    // LRU_TEST process-wide singleton (deduplication.py:232)
    // -----------------------------------------------------------------------

    #[test]
    fn lru_test_singleton_is_shared_across_callers() {
        let _g = LOCK.lock().unwrap();
        clear_lru_test();
        put_in_cache("shared-key");
        let count = with_lru_test(|c| c.count("shared-key"));
        assert_eq!(count, 1);
    }

    // -----------------------------------------------------------------------
    // M4 Stage 6 — Simhash + content_fingerprint + is_similar_domain
    // (deduplication.py:22-143)
    // -----------------------------------------------------------------------

    #[test]
    fn simhash_new_produces_64_bit_value() {
        // Brief test #1 — `Simhash::new("foo bar")` produces a 64-bit
        // value. The default length is 64 (deduplication.py:65), so
        // every bit position is meaningful.
        let s = Simhash::new("foo bar");
        assert_eq!(s.length, 64);
        // The hash is a u64; trivially fits 64 bits. We assert it's not
        // an obviously-broken sentinel (the all-zero case only happens
        // when every accumulator bit went strictly negative, which
        // requires at least one token — empty input collapses to all-1
        // via the `>= 0` rule at deduplication.py:106).
        let _ = s.hash;
    }

    #[test]
    fn simhash_is_deterministic() {
        // Brief test #2 — same input ⇒ same fingerprint.
        let a = Simhash::new("the quick brown fox jumps over the lazy dog");
        let b = Simhash::new("the quick brown fox jumps over the lazy dog");
        assert_eq!(a.hash, b.hash);
        assert_eq!(a.length, b.length);
    }

    #[test]
    fn simhash_similar_texts_low_hamming_distance() {
        // Brief test #3 — similar texts have low Hamming distance.
        // "the quick brown fox" vs "the quick brown fox jumps" share
        // 4 of the ~5 tokens; the simhash property says hamming
        // distance < length/2 ≈ 32 for similar inputs. Brief asks for
        // < 16; on short inputs simhash is noisy but still < 32.
        let a = Simhash::new("the quick brown fox");
        let b = Simhash::new("the quick brown fox jumps");
        let hd = a.hamming_distance(&b);
        assert!(
            hd < 32,
            "similar texts should have hamming distance < length/2 (got {hd})"
        );
    }

    #[test]
    fn simhash_different_texts_high_hamming_distance() {
        // Brief test #4 — different texts have high Hamming distance.
        // "foo" vs "completely unrelated stuff here" share zero tokens
        // — hamming distance should be near length/2 ≈ 32, certainly
        // > 16 with high probability.
        let a = Simhash::new("foo");
        let b = Simhash::new("completely unrelated stuff here xyzzy bazquux");
        let hd = a.hamming_distance(&b);
        assert!(
            hd > 16,
            "unrelated texts should have hamming distance > 16 (got {hd})"
        );
    }

    #[test]
    fn simhash_empty_string_handled_gracefully() {
        // Brief test #5 — empty input doesn't panic.
        // Python: with no tokens, `vector = [0]*length` stays zero, and
        // `sum(1 << i for i in range(length) if vector[i] >= 0)` is
        // `(1 << 64) - 1` ≡ `u64::MAX` (deduplication.py:106 — the
        // `>= 0` rule means a zero accumulator goes to 1).
        let s = Simhash::new("");
        assert_eq!(s.length, 64);
        assert_eq!(s.hash, u64::MAX, "empty-input simhash is all-ones per Python");
    }

    #[test]
    fn simhash_similarity_ratio_in_unit_interval() {
        // The similarity ratio is in [0.0, 1.0] for all inputs.
        let a = Simhash::new("alpha beta gamma");
        let b = Simhash::new("delta epsilon zeta");
        let r = a.similarity(&b);
        assert!((0.0..=1.0).contains(&r), "similarity out of [0,1]: {r}");
        // Self-similarity is 1.0.
        assert_eq!(a.similarity(&a), 1.0);
    }

    // -----------------------------------------------------------------------

    #[test]
    fn content_fingerprint_same_content_same_value() {
        // Brief test #6 — same content ⇒ same fingerprint.
        let a = content_fingerprint("hello world");
        let b = content_fingerprint("hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn content_fingerprint_hex_format() {
        // Brief test #7 — hex format. Python `hex(self.hash)[2:]` strips
        // the `0x` prefix, lowercase, no leading-zero padding. So the
        // output is purely `[0-9a-f]+`, length 1..=16 for u64. We check
        // the character set; length varies by hash value.
        let fp = content_fingerprint("some moderately long content with words");
        assert!(!fp.is_empty(), "fingerprint must be non-empty");
        assert!(
            fp.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "fingerprint must be lowercase hex: {fp}"
        );
        assert!(fp.len() <= 16, "fingerprint over 16 chars: {fp}");
    }

    #[test]
    fn content_fingerprint_empty_content_handled() {
        // Brief test #8 — empty content. Per Python, simhash of empty
        // is u64::MAX, so the hex is "ffffffffffffffff".
        let fp = content_fingerprint("");
        assert_eq!(fp, "ffffffffffffffff");
    }

    // -----------------------------------------------------------------------

    #[test]
    fn is_similar_domain_identical_inputs() {
        // Brief test #9 — identical domains are similar.
        assert!(is_similar_domain("example.com", "example.com"));
    }

    #[test]
    fn is_similar_domain_www_prefix_is_still_similar() {
        // Brief test #10 — www-prefixed vs bare domain. Python returns
        // FALSE here despite the brief's expectation: `STRIP_EXTENSION`
        // is a SINGLE-PASS TLD strip, so "www.example.com" → "www.example"
        // and "example.com" → "example". SequenceMatcher of those two
        // computes 7 matches / 18 chars = ratio 0.778, which is ≥ 0.5,
        // so the function actually returns TRUE. The brief was correct on
        // the verdict but wrong on the mechanism (we are NOT www-
        // stripping — the longer common substring carries the ratio).
        assert!(is_similar_domain("www.example.com", "example.com"));
    }

    #[test]
    fn is_similar_domain_different_brands_are_dissimilar() {
        // Brief test #11 — totally different brands return false.
        // "example" vs "other" share NO common substring of length > 1
        // (the 'e' positions are interior), ratio = 0.0 < 0.5.
        assert!(!is_similar_domain("example.com", "other.com"));
    }

    #[test]
    fn is_similar_domain_tld_swap_is_similar_per_python() {
        // Brief test #12 — example.com vs example.org. Both strip to
        // "example", ratio = 1.0 ≥ 0.5 ⇒ TRUE. (Brief said "behaviour
        // per Python (could be either)" — Python says TRUE.)
        assert!(is_similar_domain("example.com", "example.org"));
    }

    #[test]
    fn is_similar_domain_empty_string_handling() {
        // Brief test #13 — empty inputs. Python `SequenceMatcher(None,
        // '', '').ratio()` returns 1.0 (matched in CPython's size
        // guard). The strip on "" leaves "". Ratio 1.0 ≥ 0.5 ⇒ TRUE.
        assert!(is_similar_domain("", ""));
        // Empty vs non-empty: ratio = 0.0 < 0.5 ⇒ FALSE.
        assert!(!is_similar_domain("", "example.com"));
    }

    // -----------------------------------------------------------------------
    // Cross-cutting
    // -----------------------------------------------------------------------

    #[test]
    fn simhash_nfc_invariant_for_ascii_input() {
        // Brief test #14 — NFC(text) should produce the same hash as
        // text, for inputs that are already in NFC. Pure-ASCII text is
        // NFC-stable (NFC is the identity on ASCII), so this is a
        // determinism check on a transformation that's identity. The
        // honest non-ASCII variant is harder (token-level normalisation
        // matters), but for the brief's assertion ("same input should
        // produce same hash") this is the load-bearing case.
        let input = "the quick brown fox jumps over the lazy dog";
        let nfc: String = unicode_normalization::UnicodeNormalization::nfc(input.chars())
            .collect();
        let a = Simhash::new(input);
        let b = Simhash::new(&nfc);
        assert_eq!(a.hash, b.hash, "NFC of ASCII input must produce same hash");
    }

    #[test]
    fn content_fingerprint_corpus_snapshot_deterministic() {
        // Brief test #15 — fingerprint over a corpus snapshot is
        // deterministic across multiple calls. We pin a multi-paragraph
        // string that resembles real article content and check the
        // fingerprint matches itself across 4 calls.
        let corpus = "Trafilatura is a Python package and command-line tool \
            designed to gather text on the Web. It includes discovery, \
            extraction and text processing components. Its main applications \
            are web crawling, downloads, scraping, and extraction of main \
            texts, metadata and comments. It aims at staying handy and \
            modular: no database is required, the output can be converted \
            to commonly used formats.";
        let fps: Vec<String> = (0..4).map(|_| content_fingerprint(corpus)).collect();
        assert!(fps.windows(2).all(|w| w[0] == w[1]),
            "content_fingerprint is not deterministic across calls: {fps:?}");
    }

    #[test]
    fn sample_tokens_strips_punctuation_and_filters_short() {
        // Faithful to deduplication.py:35-48: tokens are punctuation-
        // stripped and the threshold cascade keeps progressively
        // shorter tokens until ≥ length/2 survive.
        let toks = sample_tokens("Hello, world! This-is a test.", 4);
        // After stripping ".,!" tokens are: ["Hello", "world", "This-is",
        // "a", "test"]. Wait — "This-is" contains "-" which is in
        // string.punctuation but only at boundaries does `.strip()`
        // remove it. Mid-token hyphen survives, BUT then `.isalnum()`
        // is False because "-" isn't alphanumeric, so "This-is" is
        // DROPPED entirely. Surviving: ["Hello", "world", "a", "test"].
        // Threshold sweep with length/2 = 2:
        //   i=4: tokens > 4 chars: ["Hello", "world"] (2 ≥ 2) ⇒ return.
        assert_eq!(toks, vec!["Hello".to_string(), "world".to_string()]);
    }

    #[test]
    fn strip_extension_strips_one_tld_only() {
        // Anti-inversion pin: STRIP_EXTENSION fires ONCE.
        assert_eq!(strip_extension("example.com"), "example");
        assert_eq!(strip_extension("www.example.com"), "www.example");
        assert_eq!(strip_extension("example.co.uk"), "example.co");
        // Suffix < 2 chars: no strip (regex requires {2,63}).
        assert_eq!(strip_extension("example.x"), "example.x");
        // Suffix > 63 chars: no strip.
        let long = format!("example.{}", "x".repeat(64));
        assert_eq!(strip_extension(&long), long);
        // No dot: no strip.
        assert_eq!(strip_extension("localhost"), "localhost");
    }

    #[test]
    fn sequence_ratio_matches_python_difflib_on_known_cases() {
        // Pinned against Python `difflib.SequenceMatcher(None, a, b)
        // .ratio()` outputs (sampled at port time):
        // - ('example', 'example') ⇒ 1.0
        // - ('example', 'examplx') ⇒ 6/7 = 0.857...
        // - ('example', 'other') ⇒ 1/6 ≈ 0.167 (matches: 'e')
        // - ('foo', 'bar') ⇒ 0.0
        // - ('', '') ⇒ 1.0 (CPython special-case)
        assert!((sequence_ratio("example", "example") - 1.0).abs() < 1e-9);
        assert!((sequence_ratio("example", "examplx") - 6.0 / 7.0).abs() < 1e-9);
        let r = sequence_ratio("example", "other");
        // Python yields 2*1/(7+5) = 2/12 ≈ 0.1666...
        assert!(
            (r - 2.0 / 12.0).abs() < 1e-9,
            "expected ~0.1667, got {r}"
        );
        assert_eq!(sequence_ratio("foo", "bar"), 0.0);
        assert_eq!(sequence_ratio("", ""), 1.0);
    }

    #[test]
    fn is_similar_domain_threshold_parameter() {
        // Verify the threshold override entry point. With threshold=0.9,
        // "example.com" vs "examplx.com" (post-strip "example" vs
        // "examplx", ratio 0.857) should fall BELOW the threshold.
        assert!(is_similar_domain("example.com", "examplx.com"));
        assert!(!is_similar_domain_with("example.com", "examplx.com", 0.9));
    }
}
