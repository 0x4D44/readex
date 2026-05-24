//! Content-similarity oracle — M11 Phase B.
//!
//! Two metrics for quantifying the content overlap between mdrcel and oracle
//! extractions, plus a cheap XML tag stripper for normalising xml-format output
//! before comparison.
//!
//! **Primary metric:** character-level Sørensen-Dice coefficient on bigram
//! multisets. Tracks normalised Levenshtein similarity at r ≈ 0.99 but in
//! O(n) time instead of O(n·m).
//!
//! **Secondary metric:** token-set Jaccard. Cheaper than Dice; good
//! "any shared content?" signal but loses granularity on whitespace-only diffs.
//!
//! Design doc: `wrk_docs/2026.05.24 - HLD - M11 Phase B similarity oracle.md`

use std::collections::{HashMap, HashSet};

// -------------------------------------------------------------------------
// Dice bigram similarity
// -------------------------------------------------------------------------

/// Character-level Sørensen-Dice coefficient on bigram multisets.
///
/// Formula: `2 * |A ∩ B|_multiset / (|A| + |B|)`
/// where A, B are character bigram multisets and intersection takes
/// `min(count_a, count_b)` per bigram.
///
/// Returns 1.0 for identical strings (including both empty).
/// Returns 0.0 when one is empty and the other is not.
/// Returns 0.0 for two different single-char strings (no bigrams).
pub fn dice_bigram_similarity(a: &str, b: &str) -> f64 {
    // Both identical (including both empty) => 1.0.
    if a == b {
        return 1.0;
    }
    // One empty, other not => 0.0.
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let bigrams_a = char_bigram_counts(a);
    let bigrams_b = char_bigram_counts(b);

    let size_a: usize = bigrams_a.values().sum();
    let size_b: usize = bigrams_b.values().sum();

    // Both strings are single chars but not equal (handled by a == b above).
    if size_a == 0 && size_b == 0 {
        return 0.0;
    }

    let mut intersection = 0usize;
    for (bigram, &count_a) in &bigrams_a {
        if let Some(&count_b) = bigrams_b.get(bigram) {
            intersection += count_a.min(count_b);
        }
    }

    (2.0 * intersection as f64) / (size_a + size_b) as f64
}

fn char_bigram_counts(s: &str) -> HashMap<(char, char), usize> {
    let mut map = HashMap::new();
    let chars: Vec<char> = s.chars().collect();
    for window in chars.windows(2) {
        *map.entry((window[0], window[1])).or_insert(0) += 1;
    }
    map
}

// -------------------------------------------------------------------------
// Jaccard token similarity
// -------------------------------------------------------------------------

/// Token-set Jaccard similarity.
///
/// Tokens: `\w+` matches (sequences of `[a-zA-Z0-9_]`), lowercased via
/// `to_ascii_lowercase`. SET semantics (not multiset).
///
/// Returns 1.0 when both are empty (no tokens => identical empty sets).
/// Returns 0.0 when one has tokens and the other has none.
pub fn jaccard_token_similarity(a: &str, b: &str) -> f64 {
    let tokens_a = tokenize(a);
    let tokens_b = tokenize(b);

    if tokens_a.is_empty() && tokens_b.is_empty() {
        return 1.0;
    }

    let intersection = tokens_a.intersection(&tokens_b).count();
    let union = tokens_a.union(&tokens_b).count();

    intersection as f64 / union as f64
}

fn tokenize(s: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let mut current = String::new();
    for ch in s.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch.to_ascii_lowercase());
        } else if !current.is_empty() {
            set.insert(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        set.insert(current);
    }
    set
}

// -------------------------------------------------------------------------
// XML tag stripper
// -------------------------------------------------------------------------

/// Strip XML/HTML tags from a string for similarity computation.
///
/// Removes everything between `<` and `>` inclusive. Does NOT handle CDATA,
/// comments, or processing instructions specially — this is a cheap heuristic
/// for the similarity oracle, not a parser.
pub fn strip_xml_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        if ch == '<' {
            in_tag = true;
        } else if ch == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(ch);
        }
    }
    out
}

// =========================================================================
// Unit tests — HLD §6 minimum 8 tests
// =========================================================================

#[test]
fn dice_identical_strings() {
    assert_eq!(dice_bigram_similarity("hello world", "hello world"), 1.0);
    assert_eq!(jaccard_token_similarity("hello world", "hello world"), 1.0);
}

#[test]
fn dice_both_empty() {
    assert_eq!(dice_bigram_similarity("", ""), 1.0);
    assert_eq!(jaccard_token_similarity("", ""), 1.0);
}

#[test]
fn dice_empty_vs_nonempty() {
    assert_eq!(dice_bigram_similarity("", "some content here"), 0.0);
    assert_eq!(dice_bigram_similarity("some content here", ""), 0.0);
    assert_eq!(jaccard_token_similarity("", "some content here"), 0.0);
}

#[test]
fn dice_whitespace_only_diff() {
    let a = "The quick brown fox\njumps over the lazy dog";
    let b = "The quick brown fox\n\njumps over the lazy dog";
    let dice = dice_bigram_similarity(a, b);
    assert!(
        dice > 0.95,
        "whitespace-only diff should score > 0.95, got {dice}"
    );
    // Jaccard should be 1.0 (same word set).
    assert_eq!(jaccard_token_similarity(a, b), 1.0);
}

#[test]
fn dice_partial_overlap() {
    let shared = "This is the main article content about important topics. ";
    let a = format!("{shared}Extra paragraph only in version A.");
    let b = format!("{shared}Different paragraph only in version B.");
    let dice = dice_bigram_similarity(&a, &b);
    assert!(
        dice > 0.70 && dice < 0.95,
        "partial overlap should be content-similar range, got {dice}"
    );
}

#[test]
fn dice_completely_disjoint() {
    let a = "abcdefghijklmnop";
    let b = "qrstuvwxyz123456";
    let dice = dice_bigram_similarity(a, b);
    assert!(
        dice < 0.05,
        "completely disjoint should score near 0, got {dice}"
    );
}

#[test]
fn strip_xml_tags_basic() {
    let xml = "<doc><p>Hello <b>world</b></p></doc>";
    assert_eq!(strip_xml_tags(xml), "Hello world");
}

#[test]
fn dice_xml_tag_stripped_vs_raw() {
    let a = "<doc><p>Hello world</p><p>Second para</p></doc>";
    let b = "<doc><p>Hello world</p><div>Second para</div></doc>";
    let raw_dice = dice_bigram_similarity(a, b);
    let stripped_dice =
        dice_bigram_similarity(&strip_xml_tags(a), &strip_xml_tags(b));
    // Stripped should be >= raw (tag noise removed).
    assert!(
        stripped_dice >= raw_dice,
        "tag-stripped Dice ({stripped_dice}) should be >= raw ({raw_dice})"
    );
    // After stripping, the text is identical.
    assert_eq!(stripped_dice, 1.0);
}

#[test]
fn dice_single_char() {
    // Single chars produce no bigrams — convention: equal strings => 1.0.
    assert_eq!(dice_bigram_similarity("a", "a"), 1.0);
    // Different single chars => 0.0.
    assert_eq!(dice_bigram_similarity("a", "b"), 0.0);
}
