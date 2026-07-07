//! Schema migrations over the format-preserving TOML document.
//!
//! Migrations run on a [`DocumentMut`] (not the typed schema) so that unknown
//! keys and user comments survive the upgrade. [`migrate`] applies steps in
//! order from the document's declared version up to [`CURRENT_VERSION`], then
//! stamps the new version.
//!
//! Only one version exists today, so the sole registered step is a *fake*
//! `v0 -> v1` used to exercise the framework (chain ordering, per-entry
//! preservation, version stamping) ahead of any real future migration.

use toml_edit::DocumentMut;

use crate::config::error::ConfigError;

/// The newest schema version this build can produce.
pub const CURRENT_VERSION: u32 = 1;

/// Migrate `doc`, declared at schema version `from`, up to [`CURRENT_VERSION`].
///
/// Steps are applied in ascending version order; the resulting document is
/// stamped with [`CURRENT_VERSION`]. A document already at the current version
/// is returned unchanged apart from the (idempotent) version stamp.
///
/// # Errors
/// - [`ConfigError::UnsupportedVersion`] if `from` is newer than this build
///   understands (downgrades are refused, never guessed).
/// - [`ConfigError::Migration`] if a required step is not registered or fails to
///   advance the version.
pub fn migrate(mut doc: DocumentMut, from: u32) -> Result<DocumentMut, ConfigError> {
    if from > CURRENT_VERSION {
        return Err(ConfigError::UnsupportedVersion {
            found: from,
            current: CURRENT_VERSION,
        });
    }

    let mut version = from;
    while version < CURRENT_VERSION {
        let (next_doc, next_version) = step(doc, version)?;
        if next_version <= version {
            // Defends the loop against a mis-registered step.
            return Err(ConfigError::Migration {
                from: version,
                to: next_version,
                reason: "migration step did not advance the schema version".to_owned(),
            });
        }
        doc = next_doc;
        version = next_version;
    }

    stamp_version(&mut doc, CURRENT_VERSION);
    Ok(doc)
}

/// Apply the single migration step that starts at version `from`, returning the
/// upgraded document and the version it now conforms to.
fn step(doc: DocumentMut, from: u32) -> Result<(DocumentMut, u32), ConfigError> {
    match from {
        0 => migrate_v0_to_v1(doc),
        other => Err(ConfigError::Migration {
            from: other,
            to: other.saturating_add(1),
            reason: format!("no migration registered from schema v{other}"),
        }),
    }
}

/// `v0 -> v1`: rename each monitor's `min_gap_ms` key to `min_write_gap_ms`,
/// preserving every other key (and comments) in the table.
// RATIONALE: every `migrate_vN_to_vN+1` step shares one fallible signature so
// `step` can dispatch them uniformly and a future step that genuinely fails
// slots in without reshaping the framework; this first step happens not to fail.
#[allow(clippy::unnecessary_wraps)]
fn migrate_v0_to_v1(mut doc: DocumentMut) -> Result<(DocumentMut, u32), ConfigError> {
    rename_in_each_monitor(&mut doc, "min_gap_ms", "min_write_gap_ms");
    Ok((doc, 1))
}

/// Rename `old_key` to `new_key` in every `[monitors.*]` sub-table, leaving the
/// value (and the rest of the table) untouched. A no-op where the key is absent.
fn rename_in_each_monitor(doc: &mut DocumentMut, old_key: &str, new_key: &str) {
    let Some(monitors) = doc.get_mut("monitors").and_then(|m| m.as_table_mut()) else {
        return;
    };
    for (_, entry) in monitors.iter_mut() {
        let Some(table) = entry.as_table_mut() else {
            continue;
        };
        if let Some(value) = table.remove(old_key) {
            table.insert(new_key, value);
        }
    }
}

/// Set the top-level `schema_version` scalar.
fn stamp_version(doc: &mut DocumentMut, version: u32) {
    doc.as_table_mut()
        .insert("schema_version", toml_edit::value(i64::from(version)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> DocumentMut {
        s.parse().expect("valid toml fixture")
    }

    /// A config as a hypothetical pre-versioning `v0` build would have written
    /// it: no `schema_version`, the old `min_gap_ms` key, user comments, and an
    /// unknown future section — all of which must survive the upgrade.
    const V0_DOC: &str = "\
# Duja config written by a hypothetical v0 build (no schema_version).
[general]
autostart = true

# Left monitor — note the old key name `min_gap_ms`.
[monitors.\"GSM-5B09-312NTAB1C234\"]
name = \"Left LG\"
hw_floor_pct = 10
min_gap_ms = 120

[monitors.\"DEL-A131-s12345\"]
name = \"Right Dell\"
min_gap_ms = 100

[future_section]
experimental = true
";

    #[test]
    fn v0_to_v1_migration_preserves_per_monitor_entries() {
        let out = migrate(parse(V0_DOC), 0)
            .expect("migration succeeds")
            .to_string();

        // Both per-monitor tables survive with their identifying data.
        assert!(out.contains("GSM-5B09-312NTAB1C234"), "{out}");
        assert!(out.contains("DEL-A131-s12345"), "{out}");
        assert!(out.contains("name = \"Left LG\""));
        assert!(out.contains("name = \"Right Dell\""));

        // The v0 key was renamed in every monitor; no `min_gap_ms = …`
        // assignment remains (the string still appears inside the preserved
        // comment below, which is exactly the point).
        assert!(!out.contains("min_gap_ms ="), "old key leaked: {out}");
        assert!(out.contains("min_write_gap_ms = 120"));
        assert!(out.contains("min_write_gap_ms = 100"));

        // Version stamped; comments (including the one mentioning the old key)
        // and unknown sections preserved verbatim.
        assert!(out.contains("schema_version = 1"));
        assert!(out.contains("# Duja config written by a hypothetical v0 build"));
        assert!(out.contains("note the old key name `min_gap_ms`"));
        assert!(out.contains("[future_section]"));
        assert!(out.contains("experimental = true"));

        // The migrated document is valid TOML and loads under the v1 schema.
        let cfg: crate::config::Config =
            toml_edit::de::from_str(&out).expect("migrated doc is a valid v1 config");
        assert_eq!(cfg.schema_version, 1);
        assert_eq!(cfg.monitors.len(), 2);

        // Golden snapshot of the exact migrated document.
        insta::assert_snapshot!("v0_to_v1_migration", out);
    }

    #[test]
    fn step_advances_exactly_one_version() {
        // Chain-order proof: the v0 step lands on v1 and applies its rename.
        let (doc, version) =
            step(parse("[monitors.\"X\"]\nmin_gap_ms = 50\n"), 0).expect("step runs");
        assert_eq!(version, 1);
        assert!(doc.to_string().contains("min_write_gap_ms = 50"));
    }

    #[test]
    fn migrate_at_current_version_only_stamps() {
        let doc = parse("schema_version = 1\n\n[general]\nautostart = false\n");
        let out = migrate(doc, 1).expect("noop migration").to_string();
        assert!(out.contains("schema_version = 1"));
        assert!(out.contains("autostart = false"));
        // Idempotent: migrating the result again changes nothing.
        let again = migrate(parse(&out), 1).expect("idempotent").to_string();
        assert_eq!(out, again);
    }

    #[test]
    fn migrate_rejects_future_version() {
        let err = migrate(parse("schema_version = 99\n"), 99).expect_err("must reject");
        assert!(matches!(
            err,
            ConfigError::UnsupportedVersion {
                found: 99,
                current: 1
            }
        ));
    }

    #[test]
    fn unregistered_step_is_a_typed_migration_error() {
        // A gap in the step chain reports a Migration error rather than looping.
        let err = step(parse(""), 7).expect_err("no v7 step");
        assert!(matches!(err, ConfigError::Migration { from: 7, .. }));
    }
}
