//! Configuration schema, persistence and migration.
//!
//! This is the one module in `duja-core` that performs filesystem I/O. It owns
//! several concerns, kept in separate submodules:
//!
//! - [`schema`] — the typed, serde-derived config schema (version 1) and its
//!   defaults.
//! - [`document`] — a format-preserving wrapper over `toml_edit::DocumentMut`
//!   so unknown keys and user comments survive a load -> edit -> save cycle.
//! - [`migrate`](mod@migrate) — chained, version-stamped migrations over the
//!   document.
//! - [`persist`] — crash-safe atomic writes (temp file in the same directory,
//!   then rename) and tolerant reads.
//! - [`state`] — a small, volatile side file for per-display last-levels, so
//!   frequent level writes never churn the user's config.
//!
//! All fallible operations surface [`ConfigError`]; nothing here panics, and a
//! file that cannot be understood is reported, never silently overwritten.

pub mod document;
mod error;
pub mod migrate;
pub mod persist;
pub mod schema;
pub mod state;

pub use document::ConfigDocument;
pub use error::ConfigError;
pub use migrate::{CURRENT_VERSION, migrate};
pub use schema::{Accent, Config, DimMode, General, MonitorConfig, Theme};
pub use state::{STATE_WRITE_DEBOUNCE, StateFile, should_write};
