//! Corpus loading: parse `corpus/urls.tsv`, resolve snapshot paths, hashing
//! (harness HLD §6).
//!
//! # Immutability (HLD §6) — this module is READ-ONLY
//!
//! The loader **never** mutates or rewrites the manifest or any snapshot.
//! Snapshots are content-addressed and committed; re-fetching a URL means a
//! new row + new snapshot via the out-of-band `fetch` subcommand, never an
//! in-place edit. Nothing in this module opens the manifest or a snapshot for
//! writing. (The `fetch` path that *does* write lives in `main.rs` and is only
//! reachable via the explicit `fetch` subcommand — it is never on the scoring
//! path.)
//!
//! # Snapshot filename derivation (HLD §6)
//!
//! `snapshot_filename = hex(sha256(url))[:16] + ".html"`. This is a pure
//! function of the URL ([`snapshot_filename`]); the manifest stores the
//! derived value redundantly so a human-editable TSV stays self-describing,
//! and the loader re-derives it to catch hand-edit drift.
//!
//! Content-addressing here is by **URL**, not by a digest of the file
//! contents: the drift check proves the *filename* is the right function of
//! the URL, never that the *bytes* are intact. A byte-corrupted snapshot is
//! therefore NOT detectable by this module — `.gitattributes` `*.html -text`
//! (verbatim, no line-ending normalisation) is the sole guarantee of snapshot
//! byte-stability run-to-run.
//!
//! # Validation split: pure drift check vs. checked existence
//!
//! Two layers, with different guarantees — the doc below states exactly what
//! each one does, no more:
//!
//! * [`parse_manifest`]/[`load`] are **pure and unchecked by design**: they
//!   never touch the filesystem (beyond reading the manifest itself) and so
//!   verify only **filename↔URL drift** — that each row's
//!   `snapshot_filename` equals the value derived from its `url`. They do
//!   **not** verify the snapshot exists. This keeps the parsing/validation
//!   core fully unit-testable without fixtures.
//! * [`load_checked`] layers the **existence** guarantee on top: after
//!   `load`, it `stat`s every entry's snapshot and hard-errors
//!   ([`CorpusError::SnapshotMissing`]) if one is absent. The scoring path
//!   calls `load_checked`, so **both** drift and existence are enforced
//!   before any scoring.
//!
//! # Filename-drift policy: HARD ERROR (decided here)
//!
//! If a manifest row's `snapshot_filename` column does not equal the value
//! derived from its `url`, [`load`] returns an error naming the line, the URL,
//! and expected-vs-actual filename. Rationale: the whole point of
//! content-addressing is that the filename is a checkable function of the URL.
//! A drifted filename means the row is internally inconsistent — a hand-edit
//! mistake, a copy-paste of the wrong hash, or a URL/snapshot mismatch. That
//! would silently feed the *wrong bytes* into scoring while looking
//! superficially valid: exactly the Bug-E2 "broken input laundered as valid"
//! trap the harness exists to prevent. Downgrading this to a warning would let
//! a corrupt corpus flow into a scored run, so it is a hard, fail-closed
//! error — caught at load, before any scoring. The companion *missing-file*
//! case is enforced by [`load_checked`] (above), not [`load`].

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The shape class of a corpus URL (HLD §6) — drives the report's per-class
/// breakdown (HLD §9). Closed set: unknown values in the manifest are a load
/// error, not a silently-accepted new class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeClass {
    Wikipedia,
    SecEdgar,
    Regulator,
    News,
    TechBlog,
    HubIndex,
    EdgeCase,
}

impl ShapeClass {
    /// The exact lowercase manifest token for this class (HLD §6 spelling).
    ///
    /// The inverse of [`ShapeClass::parse`]; the round-trip is the testing
    /// oracle for the closed-set spellings.
    //
    // O4 (Stage 6, `pub`-surface half — genuinely caught): `score::score_corpus`
    // calls `as_str()` to stamp the manifest `shape_class` token into each
    // `results.json` record on the non-test `main` path, so this `pub fn` has
    // a real consumer and the pre-Stage-6 `#[allow(dead_code)]` +
    // `TODO(stage-7)` was removed (no longer dead code by construction). As a
    // `pub` item it is in the half a verification probe shows IS now
    // lint-enforced; private items / never-constructed enum variants in this
    // bin crate remain uncaught (the original Stage-2 O4 caveat persists for
    // those) — so O4 is only PARTIALLY discharged, not blanket-enforced.
    pub fn as_str(self) -> &'static str {
        match self {
            ShapeClass::Wikipedia => "wikipedia",
            ShapeClass::SecEdgar => "sec_edgar",
            ShapeClass::Regulator => "regulator",
            ShapeClass::News => "news",
            ShapeClass::TechBlog => "tech_blog",
            ShapeClass::HubIndex => "hub_index",
            ShapeClass::EdgeCase => "edge_case",
        }
    }

    /// Parse the manifest token. `None` for any unrecognised value — the
    /// caller turns that into a line-numbered [`CorpusError::UnknownShapeClass`]
    /// (closed set, HLD §6).
    fn parse(token: &str) -> Option<ShapeClass> {
        match token {
            "wikipedia" => Some(ShapeClass::Wikipedia),
            "sec_edgar" => Some(ShapeClass::SecEdgar),
            "regulator" => Some(ShapeClass::Regulator),
            "news" => Some(ShapeClass::News),
            "tech_blog" => Some(ShapeClass::TechBlog),
            "hub_index" => Some(ShapeClass::HubIndex),
            "edge_case" => Some(ShapeClass::EdgeCase),
            _ => None,
        }
    }
}

/// One parsed manifest row (HLD §6 columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusEntry {
    pub url: String,
    pub shape_class: ShapeClass,
    /// Snapshot filename exactly as it appears in the manifest. Validated by
    /// [`load`] to equal [`snapshot_filename`] of `url` (drift = hard error),
    /// so by the time an entry is returned this is guaranteed consistent.
    pub snapshot_filename: String,
    pub fetched_date: String,
    pub note: String,
}

impl CorpusEntry {
    /// Absolute-or-relative path to this entry's snapshot under
    /// `corpus/snapshots/`, given the corpus directory the manifest lives in.
    /// Pure path join — does not touch the filesystem.
    ///
    /// Consumed by [`load_checked`] for the snapshot-existence check, and by
    /// the scoring path (HLD §4.2, Stage 6) which reads each snapshot to feed
    /// the oracles and the crate.
    pub fn snapshot_path(&self, corpus_dir: &Path) -> PathBuf {
        corpus_dir.join("snapshots").join(&self.snapshot_filename)
    }
}

/// Errors from loading the corpus manifest. Every variant that can be caused
/// by a bad row carries the 1-based line number so a human editing the TSV
/// can jump straight to it (HLD §6 — human-editable manifest).
#[derive(Debug)]
pub enum CorpusError {
    /// The manifest file could not be read (distinct from "absent": absence is
    /// reported as `Ok(vec![])` by [`load`] so callers can honour the
    /// Stage-1 `no corpus` contract without treating it as an error).
    Io(std::io::Error),
    /// A non-comment, non-blank row did not have exactly 5 tab-separated
    /// fields. Carries the line number and the field count found.
    MalformedRow { line: usize, fields: usize },
    /// The `shape_class` column held a value outside the closed set.
    UnknownShapeClass { line: usize, value: String },
    /// The `snapshot_filename` column did not match the value derived from the
    /// `url` column (hand-edit drift — see module docs; hard error by design).
    FilenameDrift {
        line: usize,
        url: String,
        expected: String,
        actual: String,
    },
    /// A row was internally consistent (filename matched the URL) but the
    /// snapshot file it names does not exist on disk (or is not a regular
    /// file). Surfaced only by [`load_checked`] — [`load`]/[`parse_manifest`]
    /// stay filesystem-free. This is the Bug-E2 backstop: a manifest row that
    /// points at an absent snapshot must fail loudly, not be laundered into a
    /// "loaded N" success.
    SnapshotMissing {
        line: usize,
        url: String,
        path: PathBuf,
    },
}

impl fmt::Display for CorpusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CorpusError::Io(e) => write!(f, "reading corpus manifest: {e}"),
            CorpusError::MalformedRow { line, fields } => write!(
                f,
                "corpus manifest line {line}: expected 5 tab-separated fields \
                 (url, shape_class, snapshot_filename, fetched_date, note), \
                 found {fields}"
            ),
            CorpusError::UnknownShapeClass { line, value } => write!(
                f,
                "corpus manifest line {line}: unknown shape_class {value:?} \
                 (expected one of: wikipedia, sec_edgar, regulator, news, \
                 tech_blog, hub_index, edge_case)"
            ),
            CorpusError::FilenameDrift {
                line,
                url,
                expected,
                actual,
            } => write!(
                f,
                "corpus manifest line {line}: snapshot_filename {actual:?} does \
                 not match the value derived from url {url:?} (expected \
                 {expected:?}). Snapshots are content-addressed and immutable \
                 (HLD §6); fix the row or re-fetch — do not hand-edit the hash."
            ),
            CorpusError::SnapshotMissing { line, url, path } => write!(
                f,
                "corpus manifest line {line}: snapshot for url {url:?} is \
                 missing — expected a file at {}. The row is consistent but \
                 the snapshot is absent; re-fetch it (HLD §6) before scoring \
                 — a missing snapshot must not be laundered as a valid corpus.",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CorpusError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CorpusError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for CorpusError {
    fn from(e: std::io::Error) -> Self {
        CorpusError::Io(e)
    }
}

/// Derive the immutable snapshot filename for a URL (HLD §6, **pure**):
/// `hex(sha256(url))[:16] + ".html"`.
///
/// The 16 hex chars are the first 8 bytes of the SHA-256 digest. This is the
/// single definition of the mapping; both [`load`] (drift check) and the
/// `fetch` subcommand derive the on-disk name through this one function so the
/// content-addressing invariant has exactly one implementation.
///
/// The 64-bit (16-hex) truncation is collision-safe at corpus scale (tens–
/// hundreds of URLs vs. a 2^64 space); a collision is on record as a failure
/// mode because it would manifest as `fetch` silently preserving the *first*
/// URL's bytes for the colliding second URL (the immutable "already present"
/// branch), not as a loud error.
pub fn snapshot_filename(url: &str) -> String {
    let digest = Sha256::digest(url.as_bytes());
    // Lowercase hex of the first 8 bytes == first 16 hex chars of the digest.
    let mut name = String::with_capacity(16 + 5);
    for byte in &digest[..8] {
        // `{:02x}` — fixed two lowercase hex digits per byte.
        use std::fmt::Write as _;
        let _ = write!(name, "{byte:02x}");
    }
    name.push_str(".html");
    name
}

/// Parse the TSV manifest text into entries (HLD §6).
///
/// Split out from [`load`] so the pure parsing/validation logic is unit-tested
/// without touching the filesystem. Lines are 1-based for error messages.
///
/// Rules (HLD §6 — human-editable manifest):
/// * A line whose first non-whitespace char is `#` is a comment → skipped.
/// * A blank / whitespace-only line is skipped.
/// * Every other line MUST have exactly 5 tab-separated fields; the columns
///   are `url`, `shape_class`, `snapshot_filename`, `fetched_date`, `note`,
///   in that order. `note` may itself be empty but the tab must be present.
/// * `shape_class` must be in the closed [`ShapeClass`] set.
/// * `snapshot_filename` must equal [`snapshot_filename`] of `url` (drift =
///   hard error — see module docs).
///
/// The line-stripped pure surface the unit tests assert against; the non-test
/// path goes through [`load_checked`] → [`parse_manifest_with_lines`].
//
// O4 (Stage 6): allow KEPT — Stage 6's `score`/`main` path uses
// `load_checked` (the Bug-E2 backstop), never this unchecked pure helper, so
// it is DELIBERATELY test-only. Scoped (not module-wide) on purpose.
//
// Accuracy caveat: `parse_manifest` is a PRIVATE fn, and a verification probe
// shows unused private items (and never-constructed enum variants) in this
// `benchmark` bin crate are STILL NOT compiler-caught under
// `clippy --workspace --all-targets -- -D warnings` — the original Stage-2 O4
// caveat persists for the non-`pub`-surface case. So this scoped allow is a
// convention marker, NOT a lint that would currently fire if this helper
// became genuinely dead; that protection holds for `pub` items (a non-test
// consumer now exists) but not for private ones like this. The scope is still
// kept tight so the intent is explicit and so the allow becomes load-bearing
// if this item is ever made `pub`.
#[allow(dead_code)]
fn parse_manifest(text: &str) -> Result<Vec<CorpusEntry>, CorpusError> {
    // Public/pure surface: drop the 1-based line numbers the internal parser
    // tracks (only [`load_checked`]'s existence check needs them).
    Ok(parse_manifest_with_lines(text)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// Like [`parse_manifest`] but pairs every entry with its 1-based manifest
/// line, so [`load_checked`] can name the offending line in a
/// [`CorpusError::SnapshotMissing`]. Still pure (no filesystem).
fn parse_manifest_with_lines(text: &str) -> Result<Vec<(usize, CorpusEntry)>, CorpusError> {
    let mut entries = Vec::new();

    for (idx, raw_line) in text.lines().enumerate() {
        let line = idx + 1; // 1-based for humans.

        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Exactly 5 fields. `splitn` would hide extra tabs inside `note`;
        // the spec says exactly 5 columns, so a 6th tab is a malformed row.
        let fields: Vec<&str> = raw_line.split('\t').collect();
        if fields.len() != 5 {
            return Err(CorpusError::MalformedRow {
                line,
                fields: fields.len(),
            });
        }

        let url = fields[0].to_string();

        let shape_class =
            ShapeClass::parse(fields[1]).ok_or_else(|| CorpusError::UnknownShapeClass {
                line,
                value: fields[1].to_string(),
            })?;

        let manifest_filename = fields[2].to_string();
        let derived = snapshot_filename(&url);
        if manifest_filename != derived {
            return Err(CorpusError::FilenameDrift {
                line,
                url,
                expected: derived,
                actual: manifest_filename,
            });
        }

        entries.push((
            line,
            CorpusEntry {
                url,
                shape_class,
                snapshot_filename: manifest_filename,
                fetched_date: fields[3].to_string(),
                note: fields[4].to_string(),
            },
        ));
    }

    Ok(entries)
}

/// Load and validate the corpus manifest at `manifest_path` (HLD §6).
///
/// Returns the parsed, validated entries. **An absent manifest is not an
/// error**: it returns `Ok(vec![])`, so the caller can preserve the Stage-1
/// `no corpus` contract for both "file missing" and "file present but zero
/// entries" with a single `entries.is_empty()` check. Any *other* I/O failure
/// (permission, not-a-file, …) is a real [`CorpusError::Io`].
///
/// Read-only: opens the manifest for reading only and never writes anything
/// (HLD §6 immutability).
///
/// Pure layer, consumed by tests and as the documented unchecked entry; the
/// non-test scoring path uses [`load_checked`] (Bug-E2).
//
// O4 (Stage 6): allow KEPT — Stage 6 landed and the `score`/`main` path uses
// [`load_checked`] (snapshot-existence backstop), NOT this unchecked `load`,
// so `load` stays DELIBERATELY test-only (the documented pure unchecked
// surface). Scoped (not module-wide) on purpose, and here that scoping is
// genuinely load-bearing: `load` is a `pub` fn, and a verification probe
// shows unused `pub` items in this `benchmark` bin crate ARE now caught under
// `clippy --workspace --all-targets -- -D warnings` (a real non-test
// consumer, `score.rs`, now exists, so rustc seeds dead-code analysis from
// the binary root through the `pub` surface). Without this scoped allow the
// lint WOULD flag this test-only `pub` fn. (That `pub`-surface enforcement is
// only the partial O4 discharge: unused PRIVATE items and never-constructed
// ENUM VARIANTS in this bin crate are STILL NOT compiler-caught — the
// original Stage-2 O4 caveat persists for those — so do not over-read this as
// blanket dead-code enforcement.)
#[allow(dead_code)]
pub fn load(manifest_path: &Path) -> Result<Vec<CorpusEntry>, CorpusError> {
    Ok(read_manifest_with_lines(manifest_path)?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// Read + parse the manifest, keeping 1-based line numbers; the shared I/O
/// core of [`load`] and [`load_checked`] so the absent-manifest /
/// `no corpus` semantics live in exactly one place.
///
/// **An absent manifest is not an error**: returns `Ok(vec![])` (Stage-1
/// `no corpus` contract — covers both "file missing" and "file present, zero
/// rows"). Any other I/O failure is a real [`CorpusError::Io`]. Read-only.
fn read_manifest_with_lines(
    manifest_path: &Path,
) -> Result<Vec<(usize, CorpusEntry)>, CorpusError> {
    let text = match fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Absent manifest ⇒ empty corpus (Stage-1 `no corpus` contract).
            return Ok(Vec::new());
        }
        Err(e) => return Err(CorpusError::Io(e)),
    };
    parse_manifest_with_lines(&text)
}

/// [`load`] **plus** an on-disk existence check for every snapshot — the
/// Bug-E2 backstop and the function the scoring path must call.
///
/// [`load`]/[`parse_manifest`] are deliberately pure (filesystem-free, fully
/// unit-testable) and so verify only that each row's `snapshot_filename` is
/// the correct function of its `url`. That alone does **not** prove the named
/// snapshot is actually present: a manifest row pointing at a deleted/absent
/// file is internally consistent yet unscorable. `load_checked` closes that
/// gap — it parses the manifest, then `stat`s every entry's
/// [`CorpusEntry::snapshot_path`] (relative to `corpus_dir`) and returns
/// [`CorpusError::SnapshotMissing`] (line + url + path) for the first one that
/// is absent or not a regular file. An absent *manifest* is still
/// `Ok(vec![])` (the `no corpus` contract is unchanged); only a row that
/// promises a snapshot which isn't there is the hard, fail-closed error.
///
/// Read-only: stats files, never writes (HLD §6 immutability).
pub fn load_checked(
    manifest_path: &Path,
    corpus_dir: &Path,
) -> Result<Vec<CorpusEntry>, CorpusError> {
    let with_lines = read_manifest_with_lines(manifest_path)?;
    for (line, entry) in &with_lines {
        let path = entry.snapshot_path(corpus_dir);
        if !path.is_file() {
            return Err(CorpusError::SnapshotMissing {
                line: *line,
                url: entry.url.clone(),
                path,
            });
        }
    }

    Ok(with_lines.into_iter().map(|(_, e)| e).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- snapshot_filename: known SHA-256 vector ---------------------------

    #[test]
    fn snapshot_filename_matches_known_sha256_prefix() {
        // SHA-256("") =
        //   e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        // First 16 hex chars: e3b0c44298fc1c14.
        assert_eq!(snapshot_filename(""), "e3b0c44298fc1c14.html");

        // SHA-256("abc") =
        //   ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        // First 16 hex chars: ba7816bf8f01cfea.
        assert_eq!(snapshot_filename("abc"), "ba7816bf8f01cfea.html");

        // A realistic URL — this case asserts only the *structure* of the
        // derived name (suffix, length, lowercase-hex stem), not a specific
        // digest value; the exact-value oracle is the two known SHA-256
        // vectors above ("" and "abc").
        let url = "https://example.test/article";
        let name = snapshot_filename(url);
        assert!(name.ends_with(".html"));
        // 16 hex chars + ".html" (5) == 21.
        assert_eq!(name.len(), 21);
        assert!(
            name[..16]
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "filename stem must be 16 lowercase hex digits, got {name:?}"
        );
    }

    #[test]
    fn snapshot_filename_is_deterministic_and_url_sensitive() {
        assert_eq!(
            snapshot_filename("https://a.test/"),
            snapshot_filename("https://a.test/")
        );
        assert_ne!(
            snapshot_filename("https://a.test/"),
            snapshot_filename("https://b.test/")
        );
        // A trailing-slash difference is a different URL ⇒ different snapshot.
        assert_ne!(
            snapshot_filename("https://a.test"),
            snapshot_filename("https://a.test/")
        );
    }

    // ---- ShapeClass round-trip ---------------------------------------------

    #[test]
    fn shape_class_round_trips_through_str() {
        for sc in [
            ShapeClass::Wikipedia,
            ShapeClass::SecEdgar,
            ShapeClass::Regulator,
            ShapeClass::News,
            ShapeClass::TechBlog,
            ShapeClass::HubIndex,
            ShapeClass::EdgeCase,
        ] {
            assert_eq!(ShapeClass::parse(sc.as_str()), Some(sc));
        }
    }

    #[test]
    fn shape_class_rejects_unknown_and_is_case_sensitive() {
        assert_eq!(ShapeClass::parse("blog"), None);
        assert_eq!(ShapeClass::parse(""), None);
        // Closed set is the exact lowercase spelling from HLD §6.
        assert_eq!(ShapeClass::parse("Wikipedia"), None);
        assert_eq!(ShapeClass::parse("SEC_EDGAR"), None);
    }

    // ---- parse_manifest: a helper to build a valid row ---------------------

    /// Build a syntactically valid TSV row with a correctly-derived filename.
    fn row(url: &str, shape: &str, date: &str, note: &str) -> String {
        format!("{url}\t{shape}\t{}\t{date}\t{note}", snapshot_filename(url))
    }

    // ---- parse_manifest: valid rows ----------------------------------------

    #[test]
    fn parses_valid_rows() {
        let text = format!(
            "{}\n{}\n",
            row(
                "https://en.wikipedia.test/Apple",
                "wikipedia",
                "2026-05-17",
                "fruit co"
            ),
            row("https://example.test/", "edge_case", "2026-05-17", ""),
        );
        let entries = parse_manifest(&text).expect("valid manifest");
        assert_eq!(entries.len(), 2);

        assert_eq!(entries[0].url, "https://en.wikipedia.test/Apple");
        assert_eq!(entries[0].shape_class, ShapeClass::Wikipedia);
        assert_eq!(
            entries[0].snapshot_filename,
            snapshot_filename("https://en.wikipedia.test/Apple")
        );
        assert_eq!(entries[0].fetched_date, "2026-05-17");
        assert_eq!(entries[0].note, "fruit co");

        assert_eq!(entries[1].shape_class, ShapeClass::EdgeCase);
        // An empty `note` is valid as long as the tab is present.
        assert_eq!(entries[1].note, "");
    }

    #[test]
    fn snapshot_path_joins_under_snapshots_dir() {
        let entry =
            &parse_manifest(&row("https://example.test/x", "news", "2026-05-17", "")).unwrap()[0];
        let p = entry.snapshot_path(Path::new("/corpus"));
        assert_eq!(
            p,
            Path::new("/corpus")
                .join("snapshots")
                .join(snapshot_filename("https://example.test/x"))
        );
    }

    // ---- parse_manifest: comment / blank skipping --------------------------

    #[test]
    fn skips_comment_and_blank_lines() {
        let text = format!(
            "# this is a comment\n\
             \n\
                \t  \n\
             {}\n\
             #{}\n\
             {}\n",
            row("https://a.test/1", "news", "2026-05-17", "kept"),
            // A commented-out row (leading #) must be ignored even though it
            // is otherwise a well-formed row.
            row("https://a.test/2", "news", "2026-05-17", "commented out"),
            row("https://a.test/3", "tech_blog", "2026-05-17", "kept"),
        );
        let entries = parse_manifest(&text).expect("valid manifest");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].url, "https://a.test/1");
        assert_eq!(entries[1].url, "https://a.test/3");
    }

    #[test]
    fn indented_comment_is_still_a_comment() {
        // First non-whitespace char is `#` ⇒ comment, even if indented.
        let text = format!(
            "   \t # indented comment\n{}\n",
            row("https://a.test/x", "news", "2026-05-17", "")
        );
        let entries = parse_manifest(&text).unwrap();
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn empty_and_comment_only_manifest_is_empty_not_error() {
        assert!(parse_manifest("").unwrap().is_empty());
        assert!(parse_manifest("\n\n   \n").unwrap().is_empty());
        assert!(
            parse_manifest("# header\n# only comments\n")
                .unwrap()
                .is_empty()
        );
    }

    // ---- parse_manifest: malformed row (field count, with line number) -----

    #[test]
    fn rejects_too_few_fields_with_line_number() {
        // Row 3 has only 3 fields (missing fetched_date and note).
        let text = format!(
            "{}\n{}\nhttps://a.test/bad\tnews\t{}\n",
            row("https://a.test/1", "news", "2026-05-17", ""),
            row("https://a.test/2", "news", "2026-05-17", ""),
            snapshot_filename("https://a.test/bad"),
        );
        match parse_manifest(&text) {
            Err(CorpusError::MalformedRow { line, fields }) => {
                assert_eq!(line, 3);
                assert_eq!(fields, 3);
            }
            other => panic!("expected MalformedRow at line 3, got {other:?}"),
        }
    }

    #[test]
    fn rejects_too_many_fields_with_line_number() {
        // 6 fields — an extra tab inside what should have been `note`. The
        // spec is *exactly* 5 columns, so this is malformed (not "note with
        // a tab in it"): caught so a stray tab can't smuggle data.
        let text = "https://a.test/x\tnews\tFILE\t2026-05-17\tnote\textra\n"
            .replace("FILE", &snapshot_filename("https://a.test/x"));
        match parse_manifest(&text) {
            Err(CorpusError::MalformedRow { line, fields }) => {
                assert_eq!(line, 1);
                assert_eq!(fields, 6);
            }
            other => panic!("expected MalformedRow, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_number_counts_comments_and_blanks() {
        // Comments/blanks still advance the line counter so the reported
        // number matches what a human sees in their editor.
        let text = format!(
            "# c1\n\n{}\n\nhttps://a.test/bad\tnews\n",
            row("https://a.test/ok", "news", "2026-05-17", "")
        );
        match parse_manifest(&text) {
            Err(CorpusError::MalformedRow { line, fields }) => {
                assert_eq!(line, 5, "line count must include comments/blanks");
                assert_eq!(fields, 2);
            }
            other => panic!("expected MalformedRow at line 5, got {other:?}"),
        }
    }

    // ---- parse_manifest: unknown shape_class (with line number) ------------

    #[test]
    fn rejects_unknown_shape_class_with_line_number() {
        let text = format!(
            "{}\nhttps://a.test/2\tbloggy\t{}\t2026-05-17\t\n",
            row("https://a.test/1", "news", "2026-05-17", ""),
            snapshot_filename("https://a.test/2"),
        );
        match parse_manifest(&text) {
            Err(CorpusError::UnknownShapeClass { line, value }) => {
                assert_eq!(line, 2);
                assert_eq!(value, "bloggy");
            }
            other => panic!("expected UnknownShapeClass at line 2, got {other:?}"),
        }
    }

    // ---- parse_manifest: filename-drift detection (hard error) -------------

    #[test]
    fn rejects_filename_drift_with_line_number_and_expected() {
        // A hand-edited / wrong snapshot_filename for the URL on the row.
        let url = "https://a.test/article";
        let text = format!(
            "{}\n{url}\tnews\tdeadbeefdeadbeef.html\t2026-05-17\thand-edited\n",
            row("https://a.test/1", "news", "2026-05-17", ""),
        );
        match parse_manifest(&text) {
            Err(CorpusError::FilenameDrift {
                line,
                url: u,
                expected,
                actual,
            }) => {
                assert_eq!(line, 2);
                assert_eq!(u, url);
                assert_eq!(expected, snapshot_filename(url));
                assert_eq!(actual, "deadbeefdeadbeef.html");
            }
            other => panic!("expected FilenameDrift at line 2, got {other:?}"),
        }
    }

    #[test]
    fn drift_detected_even_for_right_hash_wrong_extension() {
        // Correct 16 hex stem but wrong extension is still drift — the
        // derived value includes the ".html" suffix.
        let url = "https://a.test/x";
        let stem = &snapshot_filename(url)[..16];
        let text = format!("{url}\tnews\t{stem}.htm\t2026-05-17\t\n");
        match parse_manifest(&text) {
            Err(CorpusError::FilenameDrift { actual, .. }) => {
                assert_eq!(actual, format!("{stem}.htm"));
            }
            other => panic!("expected FilenameDrift, got {other:?}"),
        }
    }

    // ---- load(): absent file is empty, not an error ------------------------

    #[test]
    fn load_absent_manifest_is_empty_ok() {
        let missing = std::env::temp_dir().join("mdrcel-corpus-does-not-exist-xyz.tsv");
        // Pre-condition: ensure it really is absent.
        let _ = fs::remove_file(&missing);
        let entries = load(&missing).expect("absent manifest ⇒ Ok(empty)");
        assert!(entries.is_empty());
    }

    #[test]
    fn load_reads_and_validates_a_real_file() {
        // Round-trip through the filesystem to cover load() itself (not just
        // parse_manifest): write a tiny valid manifest to a temp file.
        let dir = std::env::temp_dir().join("mdrcel-corpus-load-test");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("urls.tsv");
        let text = format!(
            "# sample\n{}\n",
            row("https://example.test/load", "wikipedia", "2026-05-17", "ok")
        );
        fs::write(&path, &text).unwrap();

        let entries = load(&path).expect("valid manifest file");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].url, "https://example.test/load");
        assert_eq!(entries[0].shape_class, ShapeClass::Wikipedia);

        let _ = fs::remove_file(&path);
    }

    // ---- load_checked(): snapshot existence (Bug-E2 backstop) --------------

    #[test]
    fn load_checked_missing_snapshot_is_hard_error_with_line_and_url() {
        // A row whose filename correctly derives from its URL (so the pure
        // drift check passes) but whose snapshot file is absent: load()
        // accepts it, load_checked() must reject it loudly — Bug-E2.
        let dir = std::env::temp_dir().join("mdrcel-corpus-loadchecked-missing");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("urls.tsv");
        let url = "https://example.test/absent-snapshot";
        // Line 2 is the data row (line 1 is a comment) — assert the number.
        let text = format!(
            "# header comment\n{}\n",
            row(url, "wikipedia", "2026-05-17", "no file on disk")
        );
        fs::write(&manifest, &text).unwrap();
        // Deliberately do NOT create corpus/snapshots/<hash>.html.

        // Pure load() is happy — proves the gap load_checked() must close.
        assert_eq!(load(&manifest).unwrap().len(), 1);

        match load_checked(&manifest, &dir) {
            Err(CorpusError::SnapshotMissing { line, url: u, path }) => {
                assert_eq!(line, 2, "data row is line 2 (line 1 is a comment)");
                assert_eq!(u, url);
                assert_eq!(path, dir.join("snapshots").join(snapshot_filename(url)));
            }
            other => panic!("expected SnapshotMissing at line 2, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_checked_all_snapshots_present_is_ok() {
        // Every row's snapshot exists on disk ⇒ load_checked() returns Ok
        // with the parsed entries (drift + existence both satisfied).
        let dir = std::env::temp_dir().join("mdrcel-corpus-loadchecked-ok");
        let _ = fs::remove_dir_all(&dir);
        let snaps = dir.join("snapshots");
        fs::create_dir_all(&snaps).unwrap();

        let u1 = "https://example.test/present-1";
        let u2 = "https://example.test/present-2";
        // Create the content-addressed snapshot files the rows point at.
        fs::write(snaps.join(snapshot_filename(u1)), b"<html>1</html>").unwrap();
        fs::write(snaps.join(snapshot_filename(u2)), b"<html>2</html>").unwrap();

        let manifest = dir.join("urls.tsv");
        let text = format!(
            "# corpus\n{}\n{}\n",
            row(u1, "wikipedia", "2026-05-17", "present"),
            row(u2, "edge_case", "2026-05-17", ""),
        );
        fs::write(&manifest, &text).unwrap();

        let entries = load_checked(&manifest, &dir).expect("all snapshots present ⇒ Ok");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].url, u1);
        assert_eq!(entries[1].url, u2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_checked_absent_manifest_is_still_empty_ok() {
        // The `no corpus` contract is unchanged: an absent *manifest* (no row
        // promising a snapshot) is Ok(empty), not SnapshotMissing.
        let dir = std::env::temp_dir().join("mdrcel-corpus-loadchecked-absent");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let manifest = dir.join("urls.tsv"); // never created
        let entries =
            load_checked(&manifest, &dir).expect("absent manifest ⇒ Ok(empty), not an error");
        assert!(entries.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
