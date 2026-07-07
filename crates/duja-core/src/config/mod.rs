//! Configuration schema, persistence and migration.
//!
//! This is the one module in `duja-core` that performs filesystem I/O. It owns
//! several concerns, kept in separate submodules:
//!
//! - [`schema`] — the typed, serde-derived config schema (version 1) and its
//!   defaults.
//! - [`migrate`](mod@migrate) — chained, version-stamped migrations over the
//!   document.
//!
//! All fallible operations surface [`ConfigError`]; nothing here panics, and a
//! file that cannot be understood is reported, never silently overwritten.

mod error;
pub mod migrate;
pub mod schema;

pub use error::ConfigError;
pub use migrate::{CURRENT_VERSION, migrate};
pub use schema::{Config, DimMode, General, MonitorConfig, Theme};
