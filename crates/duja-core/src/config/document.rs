//! A format-preserving wrapper over `toml_edit::DocumentMut`.
//!
//! [`ConfigDocument`] is the bridge between the typed schema ([`Config`]) and
//! the on-disk file. Reads go through serde
//! ([`config`](ConfigDocument::config)); edits go
//! through typed setters that write *into* the underlying document, changing
//! only the keys they touch. Because the document model preserves unknown keys,
//! comments and whitespace, a `load -> set -> save` cycle keeps everything the
//! user (or a future Duja version) put in the file that this build doesn't know
//! about — the forward-compatibility guarantee from the plan (§7).

use std::path::Path;

use toml_edit::{DocumentMut, Item, Table};

use crate::config::error::ConfigError;
use crate::config::migrate;
use crate::config::persist;
use crate::config::schema::{Config, DimMode, Theme};

/// The commented `[hotkeys]` example prepended to a freshly emitted default
/// config. All bindings are optional and unbound by default; these lines
/// document the format and the recognised actions without binding anything.
const HOTKEYS_EXAMPLE_COMMENT: &str = "\
# Global hotkeys (optional; none bound by default).
#
# Each value is an accelerator: modifiers (Ctrl / Alt / Shift / Super, any
# order, case-insensitive) joined by '+' to exactly one key (an arrow, F1-F24,
# a letter, a digit, or a named key such as Space). Actions:
#   brightness_up    - raise every display's brightness by 5%
#   brightness_down  - lower every display's brightness by 5%
#   toggle_flyout    - show / hide the brightness flyout
#
# Uncomment and edit to enable:
# [hotkeys]
# brightness_up = \"Ctrl+Alt+Up\"
# brightness_down = \"Ctrl+Alt+Down\"
# toggle_flyout = \"Ctrl+Alt+B\"

";

/// A configuration file held as an editable, format-preserving TOML document.
#[derive(Debug, Clone)]
pub struct ConfigDocument {
    doc: DocumentMut,
}

impl ConfigDocument {
    /// Parse TOML `text` into a document, without migrating it.
    ///
    /// Use [`load`](Self::load) to read from disk with version handling; this is
    /// the lower-level building block.
    ///
    /// # Errors
    /// [`ConfigError::Parse`] if `text` is not valid TOML.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let doc = text.parse::<DocumentMut>().map_err(ConfigError::Parse)?;
        Ok(ConfigDocument { doc })
    }

    /// Build a document from a typed [`Config`].
    ///
    /// # Errors
    /// [`ConfigError::Serialize`] if the config holds a value TOML cannot
    /// represent (e.g. a `u64` beyond the signed 64-bit range), or
    /// [`ConfigError::Parse`] on the (theoretically impossible) failure to
    /// re-parse the serialized form.
    pub fn from_config(config: &Config) -> Result<Self, ConfigError> {
        let text = toml_edit::ser::to_string(config).map_err(ConfigError::Serialize)?;
        Self::parse(&text)
    }

    /// A document representing the full default configuration.
    ///
    /// The emitted TOML carries a commented `[hotkeys]` example block. Duja
    /// binds no hotkeys out of the box (the `[hotkeys]` table is empty), so the
    /// examples are commented out; the format-preserving document layer keeps
    /// them across a later `load -> edit -> save`.
    #[must_use]
    pub fn defaults() -> Self {
        // Serializing the defaults cannot fail (every value is in range), but
        // an empty document is an equally correct fallback: it deserializes to
        // `Config::default()` too.
        let body = toml_edit::ser::to_string(&Config::default()).unwrap_or_default();
        Self::parse(&format!("{HOTKEYS_EXAMPLE_COMMENT}{body}")).unwrap_or_else(|_| {
            ConfigDocument {
                doc: DocumentMut::new(),
            }
        })
    }

    /// Load configuration from `path`.
    ///
    /// A missing file yields [`defaults`](Self::defaults) (a normal first run).
    /// An existing file is parsed and migrated up to [`CURRENT_VERSION`](migrate::CURRENT_VERSION). The
    /// file is never rewritten here — the caller decides when to save.
    ///
    /// # Errors
    /// - [`ConfigError::Io`] if the file exists but cannot be read.
    /// - [`ConfigError::Parse`] if the file is not valid TOML.
    /// - [`ConfigError::UnsupportedVersion`] if it was written by a newer build.
    /// - [`ConfigError::Migration`] if an upgrade step fails.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match persist::read_to_string_opt(path)? {
            None => Ok(Self::defaults()),
            Some(text) => {
                let doc = text.parse::<DocumentMut>().map_err(ConfigError::Parse)?;
                let found = read_schema_version(&doc);
                let doc = migrate::migrate(doc, found)?;
                Ok(ConfigDocument { doc })
            }
        }
    }

    /// Atomically write the document to `path` (see [`persist::write_atomic`]).
    ///
    /// # Errors
    /// [`ConfigError::Io`] if the crash-safe write fails.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        persist::write_atomic(path, &self.to_toml_string())
    }

    /// Deserialize the current document into the typed [`Config`].
    ///
    /// Reflects any edits made since load. Unknown keys are ignored here (they
    /// remain in the document); missing keys fall back to schema defaults.
    ///
    /// # Errors
    /// [`ConfigError::Deserialize`] if the document does not match the schema
    /// (a known key has the wrong type, an unknown enum variant, and so on).
    pub fn config(&self) -> Result<Config, ConfigError> {
        toml_edit::de::from_str(&self.to_toml_string()).map_err(ConfigError::Deserialize)
    }

    /// Render the document to TOML text.
    #[must_use]
    pub fn to_toml_string(&self) -> String {
        self.doc.to_string()
    }

    /// Borrow the underlying editable document for advanced, structured edits.
    #[must_use]
    pub fn document(&self) -> &DocumentMut {
        &self.doc
    }

    /// Mutably borrow the underlying document for advanced, structured edits.
    pub fn document_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    // --- typed setters: each changes only the key it names ---

    /// Set `general.autostart`.
    pub fn set_autostart(&mut self, enabled: bool) {
        self.set_general("autostart", toml_edit::value(enabled));
    }

    /// Set `general.update_check`.
    pub fn set_update_check(&mut self, enabled: bool) {
        self.set_general("update_check", toml_edit::value(enabled));
    }

    /// Set `general.theme`.
    pub fn set_theme(&mut self, theme: Theme) {
        self.set_general("theme", toml_edit::value(theme_token(theme)));
    }

    /// Set a hotkey binding for `action`, inserting or replacing it.
    pub fn set_hotkey(&mut self, action: &str, binding: &str) {
        if let Some(hotkeys) = ensure_table(self.doc.as_table_mut(), "hotkeys") {
            hotkeys.insert(action, toml_edit::value(binding));
        }
    }

    /// Remove the hotkey binding for `action`; returns whether one was present.
    pub fn remove_hotkey(&mut self, action: &str) -> bool {
        self.doc
            .as_table_mut()
            .get_mut("hotkeys")
            .and_then(Item::as_table_mut)
            .and_then(|hotkeys| hotkeys.remove(action))
            .is_some()
    }

    /// Set a monitor's display `name`.
    pub fn set_monitor_name(&mut self, id: &str, name: &str) {
        self.with_monitor(id, |monitor| {
            monitor.insert("name", toml_edit::value(name));
        });
    }

    /// Set a monitor's `hw_floor_pct`.
    pub fn set_monitor_hw_floor_pct(&mut self, id: &str, pct: u8) {
        self.with_monitor(id, |monitor| {
            monitor.insert("hw_floor_pct", toml_edit::value(i64::from(pct)));
        });
    }

    /// Set a monitor's `dim_mode`.
    pub fn set_monitor_dim_mode(&mut self, id: &str, mode: DimMode) {
        self.with_monitor(id, |monitor| {
            monitor.insert("dim_mode", toml_edit::value(dim_mode_token(mode)));
        });
    }

    /// Set a monitor's `min_write_gap_ms`.
    ///
    /// Values beyond TOML's signed 64-bit integer range are clamped to
    /// [`i64::MAX`] so the document always stays serializable.
    pub fn set_monitor_min_write_gap_ms(&mut self, id: &str, ms: u64) {
        let value = i64::try_from(ms).unwrap_or(i64::MAX);
        self.with_monitor(id, |monitor| {
            monitor.insert("min_write_gap_ms", toml_edit::value(value));
        });
    }

    /// Set (or, with `None`, clear) a monitor's `sync_group`.
    pub fn set_monitor_sync_group(&mut self, id: &str, group: Option<&str>) {
        self.with_monitor(id, |monitor| match group {
            Some(name) => {
                monitor.insert("sync_group", toml_edit::value(name));
            }
            None => {
                monitor.remove("sync_group");
            }
        });
    }

    /// Set a monitor's `sync_offset` (percentage points against the group
    /// master; only meaningful alongside a `sync_group`).
    pub fn set_monitor_sync_offset(&mut self, id: &str, offset: i8) {
        self.with_monitor(id, |monitor| {
            monitor.insert("sync_offset", toml_edit::value(i64::from(offset)));
        });
    }

    /// Set a monitor's `excluded` flag.
    pub fn set_monitor_excluded(&mut self, id: &str, excluded: bool) {
        self.with_monitor(id, |monitor| {
            monitor.insert("excluded", toml_edit::value(excluded));
        });
    }

    /// Set a named DDC input source (VCP `0x60` code) for a monitor.
    pub fn set_monitor_input(&mut self, id: &str, name: &str, code: u16) {
        self.with_monitor(id, |monitor| {
            if let Some(inputs) = ensure_table(monitor, "inputs") {
                inputs.insert(name, toml_edit::value(i64::from(code)));
            }
        });
    }

    // --- private helpers ---

    /// Insert `value` under `general.<key>`, creating `[general]` if needed.
    fn set_general(&mut self, key: &str, value: Item) {
        if let Some(general) = ensure_table(self.doc.as_table_mut(), "general") {
            general.insert(key, value);
        }
    }

    /// Run `edit` against the `[monitors."<id>"]` table, creating it (and the
    /// implicit parent `[monitors]` table) if absent.
    fn with_monitor<F: FnOnce(&mut Table)>(&mut self, id: &str, edit: F) {
        let Some(monitors) = ensure_table(self.doc.as_table_mut(), "monitors") else {
            return;
        };
        // Keep `[monitors]` implicit so it never renders as a bare, empty header
        // above its per-monitor sub-tables.
        monitors.set_implicit(true);
        if let Some(monitor) = ensure_table(monitors, id) {
            edit(monitor);
        }
    }
}

/// Read the top-level `schema_version`, defaulting to `0` (pre-versioning).
///
/// An absent (or non-integer / out-of-range) version is treated as version 0
/// and migrated forward, per ADR-0007: only pre-versioning builds — and hand-
/// written files — omit the stamp, and every stamped build writes its version
/// on save. Migration steps are therefore written to be **shape-tolerant**: a
/// step that renames a key is a no-op when the key is absent, so migrating an
/// unstamped file that already has the current shape merely stamps it. This is
/// what keeps future migrations safe for hand-written files; treating unstamped
/// as "current" would silently skip every migration step for them instead.
fn read_schema_version(doc: &DocumentMut) -> u32 {
    doc.as_table()
        .get("schema_version")
        .and_then(Item::as_integer)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0)
}

/// Ensure `parent[key]` is a table, replacing a non-table item if necessary, and
/// return it. Returns `None` only if the item could not be coerced (it always
/// can after the assignment, so callers treat `None` as a no-op).
fn ensure_table<'a>(parent: &'a mut Table, key: &str) -> Option<&'a mut Table> {
    let item = parent.entry(key).or_insert_with(toml_edit::table);
    if !item.is_table() {
        *item = toml_edit::table();
    }
    item.as_table_mut()
}

/// The TOML token for a [`Theme`], matching the serde `rename_all = "lowercase"`.
fn theme_token(theme: Theme) -> &'static str {
    match theme {
        Theme::System => "system",
        Theme::Light => "light",
        Theme::Dark => "dark",
    }
}

/// The TOML token for a [`DimMode`], matching the serde `rename_all =
/// "lowercase"`.
fn dim_mode_token(mode: DimMode) -> &'static str {
    match mode {
        DimMode::Overlay => "overlay",
        DimMode::Gamma => "gamma",
        DimMode::Off => "off",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::MonitorConfig;

    #[test]
    fn defaults_document_matches_default_config() {
        let doc = ConfigDocument::defaults();
        assert_eq!(doc.config().expect("typed"), Config::default());
    }

    #[test]
    fn defaults_document_carries_commented_hotkey_examples() {
        let doc = ConfigDocument::defaults();
        let toml = doc.to_toml_string();
        // The example block documents the actions and the accelerator format...
        assert!(toml.contains("# brightness_up = \"Ctrl+Alt+Up\""), "{toml}");
        assert!(toml.contains("# toggle_flyout = \"Ctrl+Alt+B\""), "{toml}");
        // ...but binds nothing: the typed view still has an empty hotkeys table.
        assert!(doc.config().expect("typed").hotkeys.is_empty());
    }

    #[test]
    fn unknown_fields_preserved_across_load_save() {
        // A file carrying a user comment and a section this build knows nothing
        // about, plus a normal key we will edit.
        let original = "\
# hand-written by the user — keep me!
schema_version = 1

[general]
autostart = true

[future_section]
experimental = 1
";
        let mut doc = ConfigDocument::parse(original).expect("parse");

        // Touch exactly one value.
        doc.set_autostart(false);

        let saved = doc.to_toml_string();
        // The unknown section, its value, and the comment all survive.
        assert!(saved.contains("[future_section]"), "{saved}");
        assert!(saved.contains("experimental = 1"), "{saved}");
        assert!(saved.contains("# hand-written by the user"), "{saved}");
        // The edit landed.
        assert!(saved.contains("autostart = false"), "{saved}");
        // And the typed view agrees.
        assert!(!doc.config().expect("typed").general.autostart);
    }

    #[test]
    fn setting_one_value_leaves_other_keys_byte_identical() {
        let original = "\
[general]
autostart = true
theme = \"dark\"
";
        let mut doc = ConfigDocument::parse(original).expect("parse");
        doc.set_autostart(false);
        let saved = doc.to_toml_string();
        // theme line is untouched; only autostart changed.
        assert!(saved.contains("theme = \"dark\""));
        assert!(saved.contains("autostart = false"));
        assert!(!saved.contains("autostart = true"));
    }

    #[test]
    fn monitor_and_general_setters_reflect_in_typed_config() {
        let mut doc = ConfigDocument::defaults();
        doc.set_update_check(true);
        doc.set_theme(Theme::Light);
        doc.set_hotkey("brightness-up", "Ctrl+Alt+Up");

        let id = "GSM-5B09-312NTAB1C234";
        doc.set_monitor_name(id, "Left LG");
        doc.set_monitor_hw_floor_pct(id, 10);
        doc.set_monitor_dim_mode(id, DimMode::Gamma);
        doc.set_monitor_min_write_gap_ms(id, 200);
        doc.set_monitor_sync_group(id, Some("desk"));
        doc.set_monitor_excluded(id, false);
        doc.set_monitor_input(id, "hdmi1", 17);

        let cfg = doc.config().expect("typed");
        assert!(cfg.general.update_check);
        assert_eq!(cfg.general.theme, Theme::Light);
        assert_eq!(
            cfg.hotkeys.get("brightness-up").map(String::as_str),
            Some("Ctrl+Alt+Up")
        );

        let monitor = cfg.monitors.get(id).expect("monitor entry");
        assert_eq!(monitor.name.as_deref(), Some("Left LG"));
        assert_eq!(monitor.hw_floor_pct, 10);
        assert_eq!(monitor.dim_mode, DimMode::Gamma);
        assert_eq!(monitor.min_write_gap_ms, 200);
        assert_eq!(monitor.sync_group.as_deref(), Some("desk"));
        assert!(!monitor.excluded);
        assert_eq!(monitor.inputs.get("hdmi1").copied(), Some(17));
    }

    #[test]
    fn clearing_sync_group_removes_the_key() {
        let id = "X";
        let mut doc = ConfigDocument::defaults();
        doc.set_monitor_sync_group(id, Some("desk"));
        let cfg = doc.config().expect("typed");
        assert_eq!(
            cfg.monitors.get(id).expect("entry").sync_group.as_deref(),
            Some("desk")
        );
        doc.set_monitor_sync_group(id, None);
        let cfg = doc.config().expect("typed");
        assert_eq!(cfg.monitors.get(id).expect("entry").sync_group, None);
    }

    #[test]
    fn removing_a_hotkey_reports_presence() {
        let mut doc = ConfigDocument::defaults();
        doc.set_hotkey("toggle", "Ctrl+Space");
        assert!(doc.remove_hotkey("toggle"));
        assert!(!doc.remove_hotkey("toggle"));
        assert!(doc.config().expect("typed").hotkeys.is_empty());
    }

    #[test]
    fn dim_mode_setter_round_trips_every_variant() {
        // Guards the setter's token map against drift from the serde schema.
        for mode in [DimMode::Overlay, DimMode::Gamma, DimMode::Off] {
            let mut doc = ConfigDocument::defaults();
            doc.set_monitor_dim_mode("X", mode);
            let cfg = doc.config().expect("typed");
            assert_eq!(cfg.monitors.get("X").expect("entry").dim_mode, mode);
        }
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nonexistent-config.toml");
        let doc = ConfigDocument::load(&path).expect("load missing");
        assert_eq!(doc.config().expect("typed"), Config::default());
    }

    #[test]
    fn corrupt_file_yields_typed_error_not_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        // Broken TOML: an unterminated table header.
        persist::write_atomic(&path, "[general\nautostart = true\n").expect("write garbage");
        let result = ConfigDocument::load(&path);
        assert!(
            matches!(result, Err(ConfigError::Parse(_))),
            "expected a typed parse error, got {result:?}"
        );
    }

    #[test]
    fn valid_toml_wrong_types_is_a_deserialize_error() {
        // Parses as TOML, but `autostart` is the wrong type for the schema.
        let doc = ConfigDocument::parse("[general]\nautostart = \"yes\"\n").expect("parse");
        assert!(matches!(doc.config(), Err(ConfigError::Deserialize(_))));
    }

    #[test]
    fn load_migrates_an_older_versioned_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        persist::write_atomic(
            &path,
            "schema_version = 0\n\n[monitors.\"X\"]\nmin_gap_ms = 150\n",
        )
        .expect("write v0");

        let doc = ConfigDocument::load(&path).expect("load + migrate");
        let cfg = doc.config().expect("typed");
        assert_eq!(cfg.schema_version, migrate::CURRENT_VERSION);
        // The v0 key was migrated into the v1 field.
        assert_eq!(cfg.monitors.get("X").expect("entry").min_write_gap_ms, 150);
    }

    #[test]
    fn load_refuses_a_future_version() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        persist::write_atomic(&path, "schema_version = 999\n").expect("write future");
        assert!(matches!(
            ConfigDocument::load(&path),
            Err(ConfigError::UnsupportedVersion {
                found: 999,
                current: _
            })
        ));
    }

    #[test]
    fn unversioned_file_is_migrated_from_v0_shape_tolerantly() {
        // No schema_version is treated as v0 and migrated forward (ADR-0007).
        // The current-named key wins over a stray legacy key (shape-tolerant
        // rename, no clobber), and the document is stamped to the current
        // version on load.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        persist::write_atomic(
            &path,
            "[monitors.\"X\"]\nmin_write_gap_ms = 150\nmin_gap_ms = 999\n",
        )
        .expect("write");
        let doc = ConfigDocument::load(&path).expect("load");
        // The stale legacy key is dropped; the explicit current value is kept.
        assert!(!doc.to_toml_string().contains("999"));
        assert!(doc.to_toml_string().contains("schema_version = 1"));
        let cfg = doc.config().expect("typed");
        assert_eq!(cfg.monitors.get("X").expect("entry").min_write_gap_ms, 150);
    }

    #[test]
    fn unstamped_legacy_key_is_renamed_on_load() {
        // A genuine pre-versioning file (only the old key) is upgraded in place.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        persist::write_atomic(&path, "[monitors.\"X\"]\nmin_gap_ms = 120\n").expect("write");
        let cfg = ConfigDocument::load(&path)
            .expect("load")
            .config()
            .expect("typed");
        assert_eq!(cfg.monitors.get("X").expect("entry").min_write_gap_ms, 120);
    }

    #[test]
    fn save_then_load_round_trips_edits() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");

        let mut doc = ConfigDocument::defaults();
        doc.set_monitor_name("GSM-5B09-312NTAB1C234", "Left LG");
        doc.set_autostart(false);
        doc.save(&path).expect("save");

        let reloaded = ConfigDocument::load(&path).expect("load");
        let cfg = reloaded.config().expect("typed");
        assert!(!cfg.general.autostart);
        assert_eq!(
            cfg.monitors
                .get("GSM-5B09-312NTAB1C234")
                .expect("entry")
                .name
                .as_deref(),
            Some("Left LG")
        );
    }

    #[test]
    fn setter_repairs_a_non_table_monitors_value() {
        // If the file has a scalar where a table belongs, a setter coerces it
        // rather than panicking (defensive: the file was already off-schema).
        let mut doc = ConfigDocument::parse("monitors = 5\n").expect("parse");
        doc.set_monitor_name("X", "Repaired");
        let cfg = doc.config().expect("typed");
        assert_eq!(
            cfg.monitors.get("X").expect("entry").name.as_deref(),
            Some("Repaired")
        );
    }

    #[test]
    fn default_monitor_config_helper_is_stable() {
        // Sanity: the schema default used across tests is the documented one.
        assert_eq!(MonitorConfig::default().min_write_gap_ms, 100);
    }
}
