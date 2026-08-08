//! The [`ConfigError`] type shared by every operation in the [`config`] module.
//!
//! [`config`]: crate::config

/// A failure while loading, parsing, migrating or persisting configuration.
///
/// Every fallible entry point in the [`config`](crate::config) module surfaces
/// one of these. Loading a *missing* file is **not** an error (it yields
/// defaults); only genuine failures — unreadable files, malformed TOML,
/// unknown future schema versions, or a migration that could not be applied —
/// become a `ConfigError`. The module never panics and never silently discards
/// a file it could not understand.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The underlying file could not be read or written.
    #[error("config file I/O error: {0}")]
    Io(#[source] std::io::Error),

    /// The file is larger than [`crate::config::persist::MAX_CONFIG_LEN`].
    ///
    /// Its own error rather than an [`Io`](Self::Io): a caller logging "config
    /// unreadable" for a 4 GB `config.toml` sends the reader looking for a
    /// permissions problem.
    ///
    /// `at_least` rather than an exact size, because the two ways this is
    /// reached know different amounts. The metadata pre-check knows the real
    /// length; the bounded read only knows it stopped one byte past the cap and
    /// cannot say how much more there was. A field called `bytes` carrying
    /// `1048577` for a 4 GB file would read as "one byte over", so it says what
    /// it can stand behind.
    #[error("config file is at least {at_least} bytes, over the {max} byte cap")]
    TooLarge {
        /// A lower bound on the file's size, exact when it came from metadata.
        at_least: u64,
        /// The cap it exceeded.
        max: usize,
    },

    /// The file was not syntactically valid TOML.
    ///
    /// The wrapped [`toml_edit::TomlError`] carries a human-readable message
    /// with line/column span information when the parser could locate the
    /// offending token.
    #[error("config file is not valid TOML: {0}")]
    Parse(#[source] toml_edit::TomlError),

    /// The file parsed as TOML but did not match the typed schema (a value had
    /// the wrong type, an enum variant was unknown, and so on).
    #[error("config does not match the expected schema: {0}")]
    Deserialize(#[source] toml_edit::de::Error),

    /// A typed value could not be serialized to TOML (e.g. a `u64` larger than
    /// TOML's signed 64-bit integer range).
    #[error("config could not be serialized to TOML: {0}")]
    Serialize(#[source] toml_edit::ser::Error),

    /// The file declared a `schema_version` newer than this build understands.
    ///
    /// Downgrading is refused rather than guessed at, so a newer config written
    /// by a future build is never silently rewritten and truncated.
    #[error("config schema version {found} is newer than this build supports (max {current})")]
    UnsupportedVersion {
        /// The version stamped in the file.
        found: u32,
        /// The newest version this build can produce.
        current: u32,
    },

    /// A migration step from one schema version to the next failed.
    #[error("failed to migrate config from schema v{from} to v{to}: {reason}")]
    Migration {
        /// The version being migrated *from*.
        from: u32,
        /// The version being migrated *to*.
        to: u32,
        /// A human-readable explanation of what went wrong.
        reason: String,
    },
}

impl From<std::io::Error> for ConfigError {
    fn from(err: std::io::Error) -> Self {
        ConfigError::Io(err)
    }
}

impl From<toml_edit::TomlError> for ConfigError {
    fn from(err: toml_edit::TomlError) -> Self {
        ConfigError::Parse(err)
    }
}

impl From<toml_edit::de::Error> for ConfigError {
    fn from(err: toml_edit::de::Error) -> Self {
        ConfigError::Deserialize(err)
    }
}

impl From<toml_edit::ser::Error> for ConfigError {
    fn from(err: toml_edit::ser::Error) -> Self {
        ConfigError::Serialize(err)
    }
}
