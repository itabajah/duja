//! Monitor quirk database: the TOML schema, the embedded defaults, and the
//! stable-id matcher that resolves a display's effective quirks.
//!
//! Real monitors lie: some need extra pacing between DDC writes, some report a
//! bogus VCP maximum, some advertise features they cannot perform. Duja carries
//! a small, typed database of these workarounds — compiled in from
//! `quirks/quirks.toml` and extensible by the user — and merges every entry
//! that matches a display into one [`ResolvedQuirks`].
//!
//! # Matching
//!
//! An entry's `match` is a [`StableDisplayId`] **prefix** (e.g. `"MSI-30B6"`
//! matches `"MSI-30B6-<serial>"`). A single
//! trailing `*` turns it into a glob over that prefix (`"MSI-*"` matches every
//! MSI display). There are no other wildcards and no regular expressions — the
//! matcher is hand-rolled and bounded.
//!
//! An exact (non-glob) prefix always beats a glob. Among exacts the longest
//! prefix wins; among globs the longest prefix wins. [`QuirkDb::resolve`] then
//! *merges* all matching entries from least to most specific, so a broad
//! `"MSI-*"` default can set fields a specific `"MSI-30B6"` entry leaves alone,
//! and specific entries override broad ones field by field.
//!
//! # Example
//!
//! ```
//! use duja_core::quirks::QuirkDb;
//!
//! let db = QuirkDb::embedded();
//! assert_eq!(db.schema_version, 1);
//! ```

// RATIONALE: as with `caps`, the public vocabulary (`QuirkDb`, `QuirkEntry`,
// `QuirkError`) intentionally shares the module stem and reads best qualified.
#![allow(clippy::module_name_repetitions)]

use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::id::StableDisplayId;

/// The raw embedded quirk database, compiled in from `quirks/quirks.toml`.
pub const EMBEDDED_QUIRKS_TOML: &str = include_str!("../../../quirks/quirks.toml");

/// The largest quirk database [`QuirkDb::parse`] will accept: 1 MiB.
///
/// Checked before parsing so a hostile or corrupt file cannot force large
/// allocations. Real databases are a few kilobytes.
pub const MAX_QUIRKS_LEN: usize = 1024 * 1024;

/// The single quirk-schema version this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// A failure parsing a quirk database.
#[derive(Debug, thiserror::Error)]
pub enum QuirkError {
    /// The input exceeded the [`MAX_QUIRKS_LEN`] safety cap.
    #[error("quirk database exceeds the size limit ({len} bytes)")]
    TooLarge {
        /// The rejected input's length, in bytes.
        len: usize,
    },
    /// The `schema_version` was one this build does not support.
    #[error("unsupported quirk schema version {found} (this build supports {supported})")]
    UnsupportedSchemaVersion {
        /// The version found in the file.
        found: u32,
        /// The version this build supports ([`SUPPORTED_SCHEMA_VERSION`]).
        supported: u32,
    },
    /// The input was not valid TOML, or did not match the schema.
    #[error("quirk database is not valid TOML or violates the schema")]
    Toml(#[from] toml_edit::de::Error),
}

/// A parsed quirk database: a schema version plus a list of entries.
#[derive(Debug, Clone, Deserialize)]
pub struct QuirkDb {
    /// The schema version; must equal [`SUPPORTED_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The quirk entries, in file order (from the `[[quirk]]` tables).
    #[serde(default, rename = "quirk")]
    pub quirks: Vec<QuirkEntry>,
}

/// One quirk entry: a match pattern plus the fields it overrides.
///
/// Every field except [`pattern`](Self::pattern) is optional; an absent field
/// means "this entry says nothing about that setting" and leaves any value from
/// a less-specific matching entry intact.
#[derive(Debug, Clone, Deserialize)]
pub struct QuirkEntry {
    /// The stable-id prefix to match, or a trailing-`*` glob over one
    /// (the TOML key is `match`).
    #[serde(rename = "match")]
    pub pattern: String,
    /// Free-text explanation / source of this entry.
    pub note: Option<String>,
    /// Minimum gap between DDC writes, in milliseconds.
    pub min_write_gap_ms: Option<u64>,
    /// Extra capability-string read attempts before giving up.
    pub caps_retry: Option<u32>,
    /// Whether writes must be read back and verified.
    pub verify_writes: Option<bool>,
    /// The allowed VCP `0x60` input-source values, overriding bogus metadata.
    pub input_source_allowed: Option<Vec<u8>>,
    /// Override for a display that misreports its brightness VCP maximum.
    pub max_brightness: Option<u16>,
    /// Whether VCP `0x60` input switching is advertised but broken.
    pub no_input_switch: Option<bool>,
    /// Whether the capability string is unreliable and should be ignored.
    pub caps_unreliable: Option<bool>,
    /// Whether DDC is broken entirely (force software-only control).
    pub ddc_broken: Option<bool>,
}

/// The effective quirks for one display, merged from every matching entry.
///
/// `Option` fields are `None` when no matching entry set them (the caller
/// applies its own default); `bool` fields default to `false`.
// RATIONALE: these booleans are independent quirk flags that mirror the TOML
// schema one-to-one; folding them into an enum/bitflags would obscure the
// schema mapping and the field-by-field merge, so the `excessive_bools` lint is
// not the right call here.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedQuirks {
    /// Minimum gap between DDC writes, in milliseconds, if overridden.
    pub min_write_gap_ms: Option<u64>,
    /// Extra capability-string read attempts, if overridden.
    pub caps_retry: Option<u32>,
    /// Whether writes must be read back and verified.
    pub verify_writes: bool,
    /// Allowed VCP `0x60` values, if overridden.
    pub input_source_allowed: Option<Vec<u8>>,
    /// Brightness VCP maximum override, if any.
    pub max_brightness: Option<u16>,
    /// Whether input switching is disabled.
    pub no_input_switch: bool,
    /// Whether the capability string should be ignored.
    pub caps_unreliable: bool,
    /// Whether DDC is forced off (software-only).
    pub ddc_broken: bool,
    /// Notes from every matching entry, least-specific first.
    pub notes: Vec<String>,
}

impl ResolvedQuirks {
    /// The minimum write gap as a [`Duration`], falling back to `default` when
    /// no matching quirk overrode it.
    #[must_use]
    pub fn min_write_gap(&self, default: Duration) -> Duration {
        self.min_write_gap_ms.map_or(default, Duration::from_millis)
    }
}

impl QuirkDb {
    /// Parse a quirk database from TOML.
    ///
    /// # Errors
    /// - [`QuirkError::TooLarge`] if the input is longer than [`MAX_QUIRKS_LEN`].
    /// - [`QuirkError::UnsupportedSchemaVersion`] if `schema_version` is not
    ///   [`SUPPORTED_SCHEMA_VERSION`].
    /// - [`QuirkError::Toml`] if the input is not valid TOML or breaks schema.
    pub fn parse(input: &str) -> Result<Self, QuirkError> {
        if input.len() > MAX_QUIRKS_LEN {
            return Err(QuirkError::TooLarge { len: input.len() });
        }
        let db: QuirkDb = toml_edit::de::from_str(input)?;
        if db.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(QuirkError::UnsupportedSchemaVersion {
                found: db.schema_version,
                supported: SUPPORTED_SCHEMA_VERSION,
            });
        }
        Ok(db)
    }

    /// The lazily-parsed embedded quirk database.
    ///
    /// The embedded TOML is verified to parse by a unit test, so this is
    /// effectively infallible; on the impossible parse error it falls back to
    /// an empty database rather than panicking.
    #[must_use]
    pub fn embedded() -> &'static QuirkDb {
        static EMBEDDED: OnceLock<QuirkDb> = OnceLock::new();
        EMBEDDED.get_or_init(|| {
            QuirkDb::parse(EMBEDDED_QUIRKS_TOML).unwrap_or_else(|_| QuirkDb::empty())
        })
    }

    /// An empty database (schema-current, no quirks).
    fn empty() -> QuirkDb {
        QuirkDb {
            schema_version: SUPPORTED_SCHEMA_VERSION,
            quirks: Vec::new(),
        }
    }

    /// Resolve the effective quirks for `id` by merging every matching entry.
    #[must_use]
    pub fn resolve(&self, id: &StableDisplayId) -> ResolvedQuirks {
        self.resolve_id(id.as_str())
    }

    /// [`resolve`](Self::resolve) over the id as a string, for testability.
    fn resolve_id(&self, id: &str) -> ResolvedQuirks {
        // Gather every matching entry with its specificity, then merge from
        // least to most specific so the most specific setter of each field wins.
        let mut matched: Vec<(MatchKind, usize, &QuirkEntry)> = self
            .quirks
            .iter()
            .filter_map(|entry| {
                match_specificity(&entry.pattern, id).map(|(kind, len)| (kind, len, entry))
            })
            .collect();
        // Stable sort keeps file order for equal specificity (later file entry
        // then wins, since it is merged last).
        matched.sort_by_key(|&(kind, len, _)| (kind, len));

        let mut resolved = ResolvedQuirks::default();
        for (_, _, entry) in matched {
            resolved.merge_entry(entry);
        }
        resolved
    }
}

/// How an entry matched: a glob is always less specific than an exact prefix,
/// so the derived ordering (`Glob` < `Exact`) sorts glob matches first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum MatchKind {
    Glob,
    Exact,
}

/// Test whether `pattern` matches `id`, returning its match kind and the length
/// of the matched prefix (longer = more specific), or `None` if it does not.
fn match_specificity(pattern: &str, id: &str) -> Option<(MatchKind, usize)> {
    if let Some(prefix) = pattern.strip_suffix('*') {
        id.starts_with(prefix)
            .then_some((MatchKind::Glob, prefix.len()))
    } else {
        id.starts_with(pattern)
            .then_some((MatchKind::Exact, pattern.len()))
    }
}

impl ResolvedQuirks {
    /// Overlay one entry's set fields onto `self` (used most-specific last).
    fn merge_entry(&mut self, entry: &QuirkEntry) {
        if let Some(value) = entry.min_write_gap_ms {
            self.min_write_gap_ms = Some(value);
        }
        if let Some(value) = entry.caps_retry {
            self.caps_retry = Some(value);
        }
        if let Some(value) = entry.verify_writes {
            self.verify_writes = value;
        }
        if let Some(ref value) = entry.input_source_allowed {
            self.input_source_allowed = Some(value.clone());
        }
        if let Some(value) = entry.max_brightness {
            self.max_brightness = Some(value);
        }
        if let Some(value) = entry.no_input_switch {
            self.no_input_switch = value;
        }
        if let Some(value) = entry.caps_unreliable {
            self.caps_unreliable = value;
        }
        if let Some(value) = entry.ddc_broken {
            self.ddc_broken = value;
        }
        if let Some(ref note) = entry.note {
            self.notes.push(note.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a checksum-valid EDID for an MSI display with product code
    /// `0x30B6` and no serial, so its id is `MSI-30B6-#h<hash>`.
    fn msi_edid() -> Vec<u8> {
        let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.push(0x36); // "MSI" packed into bytes 8..=9, high byte
        e.push(0x69); // "MSI" packed, low byte
        e.push(0xB6); // product 0x30B6, little-endian low byte
        e.push(0x30); // product 0x30B6, little-endian high byte
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        e
    }

    #[test]
    fn quirk_db_parses_embedded_database() {
        let db = QuirkDb::parse(EMBEDDED_QUIRKS_TOML).expect("embedded quirks.toml must parse");
        assert_eq!(db.schema_version, SUPPORTED_SCHEMA_VERSION);
        let msi = db
            .quirks
            .iter()
            .find(|q| q.pattern == "MSI-30B6")
            .expect("embedded MSI-30B6 entry present");
        assert_eq!(msi.min_write_gap_ms, Some(50));
        assert_eq!(msi.caps_retry, Some(3));
        assert_eq!(msi.verify_writes, Some(true));
        assert_eq!(
            msi.input_source_allowed.as_deref(),
            Some(&[17u8, 18, 15][..])
        );
        assert!(msi.note.is_some());
    }

    #[test]
    fn quirk_resolve_applies_msi_quirks_via_embedded_db() {
        let id = StableDisplayId::from_edid(&msi_edid()).expect("valid edid");
        assert!(
            id.as_str().starts_with("MSI-30B6"),
            "id was {}",
            id.as_str()
        );

        let q = QuirkDb::embedded().resolve(&id);
        assert_eq!(q.min_write_gap_ms, Some(50));
        assert_eq!(q.caps_retry, Some(3));
        assert!(q.verify_writes);
        assert_eq!(q.input_source_allowed.as_deref(), Some(&[17u8, 18, 15][..]));
        assert_eq!(
            q.min_write_gap(Duration::from_millis(100)),
            Duration::from_millis(50)
        );
    }

    #[test]
    fn quirk_matching_precedence_exact_over_glob() {
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI-*\"\nmin_write_gap_ms = 200\n",
            "[[quirk]]\nmatch = \"MSI-30B6\"\nmin_write_gap_ms = 50\n",
        ))
        .expect("parses");
        // The exact prefix beats the glob even though both match.
        assert_eq!(db.resolve_id("MSI-30B6-serial").min_write_gap_ms, Some(50));
    }

    #[test]
    fn quirk_longest_exact_prefix_wins() {
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI\"\nmax_brightness = 80\n",
            "[[quirk]]\nmatch = \"MSI-30B6\"\nmax_brightness = 100\n",
        ))
        .expect("parses");
        assert_eq!(db.resolve_id("MSI-30B6-x").max_brightness, Some(100));
    }

    #[test]
    fn quirk_longest_glob_prefix_wins() {
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI-*\"\ncaps_retry = 1\n",
            "[[quirk]]\nmatch = \"MSI-30*\"\ncaps_retry = 5\n",
        ))
        .expect("parses");
        assert_eq!(db.resolve_id("MSI-30B6-x").caps_retry, Some(5));
    }

    #[test]
    fn quirk_merge_overrides_per_field() {
        // The glob sets gap+retry; the exact overrides gap only, retry survives.
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI-*\"\nmin_write_gap_ms = 200\ncaps_retry = 2\n",
            "[[quirk]]\nmatch = \"MSI-30B6\"\nmin_write_gap_ms = 50\n",
        ))
        .expect("parses");
        let q = db.resolve_id("MSI-30B6-x");
        assert_eq!(q.min_write_gap_ms, Some(50)); // exact override
        assert_eq!(q.caps_retry, Some(2)); // inherited from the glob
    }

    #[test]
    fn quirk_explicit_false_overrides_inherited_true() {
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI-*\"\nno_input_switch = true\n",
            "[[quirk]]\nmatch = \"MSI-30B6\"\nno_input_switch = false\n",
        ))
        .expect("parses");
        // The specific entry's explicit `false` wins over the glob's `true`.
        assert!(!db.resolve_id("MSI-30B6-x").no_input_switch);
        // A display that only matches the glob keeps `true`.
        assert!(db.resolve_id("MSI-9999-x").no_input_switch);
    }

    #[test]
    fn quirk_notes_accumulate_least_specific_first() {
        let db = QuirkDb::parse(concat!(
            "schema_version = 1\n",
            "[[quirk]]\nmatch = \"MSI-*\"\nnote = \"broad\"\n",
            "[[quirk]]\nmatch = \"MSI-30B6\"\nnote = \"specific\"\n",
        ))
        .expect("parses");
        assert_eq!(
            db.resolve_id("MSI-30B6-x").notes,
            vec!["broad".to_owned(), "specific".to_owned()]
        );
    }

    #[test]
    fn quirk_resolve_no_match_is_default() {
        let db = QuirkDb::parse(
            "schema_version = 1\n[[quirk]]\nmatch = \"MSI-30B6\"\nddc_broken = true\n",
        )
        .expect("parses");
        assert_eq!(db.resolve_id("DEL-A131-xyz"), ResolvedQuirks::default());
    }

    #[test]
    fn quirk_parse_accepts_empty_database() {
        let db = QuirkDb::parse("schema_version = 1\n").expect("parses");
        assert!(db.quirks.is_empty());
        assert_eq!(db.resolve_id("ANY-1234-x"), ResolvedQuirks::default());
    }

    #[test]
    fn quirk_parse_rejects_oversized_input() {
        let mut big = String::from("schema_version = 1\n");
        while big.len() <= MAX_QUIRKS_LEN {
            big.push_str("# padding padding padding padding padding padding padding\n");
        }
        let len = big.len();
        assert!(len > MAX_QUIRKS_LEN);
        assert!(matches!(QuirkDb::parse(&big), Err(QuirkError::TooLarge { len: l }) if l == len));
    }

    #[test]
    fn quirk_parse_rejects_unsupported_schema_version() {
        let err = QuirkDb::parse("schema_version = 999\n").unwrap_err();
        assert!(matches!(
            err,
            QuirkError::UnsupportedSchemaVersion {
                found: 999,
                supported: 1
            }
        ));
    }

    #[test]
    fn quirk_parse_rejects_malformed_toml() {
        assert!(matches!(
            QuirkDb::parse("this is = = not toml"),
            Err(QuirkError::Toml(_))
        ));
    }

    #[test]
    fn quirk_parse_rejects_entry_without_match() {
        // `match` is required, so its absence is a schema (deserialize) error.
        assert!(matches!(
            QuirkDb::parse("schema_version = 1\n[[quirk]]\nnote = \"x\"\n"),
            Err(QuirkError::Toml(_))
        ));
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Mirrors the fuzz target: arbitrary bytes, lossily decoded, must never
        /// make the parser panic.
        #[test]
        fn quirk_parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
            let s = String::from_utf8_lossy(&bytes);
            let _ = QuirkDb::parse(&s);
        }

        /// Resolution against the embedded DB must never panic on any id string.
        #[test]
        fn quirk_resolve_never_panics(id in "[A-Za-z0-9#*_-]{0,80}") {
            let _ = QuirkDb::embedded().resolve_id(&id);
        }
    }
}
