#![no_main]

//! Fuzz the `config.toml` pipeline: parse, migrate, deserialize.
//!
//! `config.toml` is the one untrusted-parse surface Duja had no fuzz target for,
//! and it is the one a user edits by hand. Unlike the other five targets this is
//! not a single call: a config file fails in three different places and only the
//! first is a parser.
//!
//! 1. **`ConfigDocument::parse`** - `toml_edit` syntax. Reaching stage 2 at all
//!    means the bytes were valid TOML, which arbitrary input rarely is, so this
//!    stage is mostly a filter.
//! 2. **`migrate`**, driven over `0..=CURRENT_VERSION + 1`. Every arm of that
//!    function is reached: the no-op when the document already claims the
//!    current version, the one registered upgrade step, and the
//!    `ConfigError::UnsupportedVersion` refusal for a file written by a newer
//!    build. The last is the reason the range runs one past the end.
//! 3. **`config()`** - the serde deserialize into the typed schema, where a
//!    well-formed document with an out-of-range or wrong-typed value lands. Run
//!    on the *migrated* document as well as the raw one, because
//!    `ConfigDocument::load` migrates before it deserializes and that ordering
//!    is the one production actually uses.
//!
//! # What this does not do, said because the shape invites over-claiming
//!
//! **There is no migration *chain* to exercise yet.** `CURRENT_VERSION` is 1 and
//! `migrate`'s own header says the single registered step is a *fake* `v0 -> v1`
//! that exists to exercise the framework. So stage 2 is one real step, one
//! no-op and one refusal - not a sequence of rewrites each assuming the shape
//! the last one left. It will become that, and this target is what will be
//! waiting.
//!
//! **The version is not taken from the input.** On the real path
//! `ConfigDocument::load` reads it out of the document with
//! `read_schema_version`, which is private and unreachable from here. Sweeping
//! the range is the closest available approximation and it is strictly broader
//! for the values it covers, but it does not fuzz the *reading* of the version.
//!
//! Every stage is contractually total: `Err` is a fine outcome and a panic is a
//! bug. Nothing here writes to disk, so a fuzz iteration costs no I/O.

use duja_core::config::{CURRENT_VERSION, ConfigDocument, migrate};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let Ok(document) = ConfigDocument::parse(&text) else {
        return;
    };

    // Stage 3 on the raw document: a file that fails to migrate never reaches
    // the typed schema on the real path, so this is the only way those bytes
    // get deserialized at all.
    let _ = document.config();

    // Stage 2, plus stage 3 on what it produced. `migrate` consumes the
    // document, hence the clone per step.
    for from in 0..=CURRENT_VERSION.saturating_add(1) {
        if let Ok(migrated) = migrate(document.document().clone(), from) {
            let _ = ConfigDocument::parse(&migrated.to_string()).map(|d| d.config());
        }
    }
});
