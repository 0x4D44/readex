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
//! # Out of scope (NOT ported, recorded per HLD §10)
//!
//! - `is_similar_domain` (lines 27-32): not in any call path the M3 gate
//!   exercises. Domain similarity is courlan / metadata-pipeline turf;
//!   defer until Stage 7-rev needs it.
//! - `Simhash` + `content_fingerprint` (lines 58-143): used only by
//!   `meta.py:11,29` (which clears the LRU cache); not on the
//!   `bare_extraction` path. The simhash port is a self-contained piece
//!   that can land in a later stage without changing the LRU shape.
//! - `sample_tokens` + `generate_bow_hash` (lines 35-55): support for
//!   `Simhash`; defer with it.
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
}
