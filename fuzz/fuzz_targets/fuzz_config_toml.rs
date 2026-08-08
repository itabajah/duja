#![no_main]

//! Fuzz the `config.toml` pipeline: parse, migrate, deserialize.
//!
//! `config.toml` is the one untrusted-parse surface Duja had no fuzz target for,
//! and it is the one a user edits by hand. Unlike the other five targets this is
//! not a single call: a config file goes through three stages that fail in
//! different ways, and driving only the first would have left the interesting
//! two uncovered.
//!
//! 1. **`ConfigDocument::parse`** - `toml_edit` syntax. Reaching stage 2 at all
//!    means the bytes were valid TOML, which arbitrary input rarely is, so this
//!    stage is mostly a filter.
//! 2. **`migrate`**, from *every* version a file could claim. This is the stage
//!    worth fuzzing and the reason the target exists: the migration chain is a
//!    sequence of `DocumentMut` rewrites, each assuming the shape the previous
//!    one left, and a hand-edited file can present any shape at any version. It
//!    is driven from `0..=CURRENT_VERSION` rather than from the version the
//!    document declares, because reading the declared version would let the
//!    fuzzer trivially avoid the multi-step chains by always claiming to be
//!    current - which is exactly the path a corrupted file does not take.
//! 3. **`config()`** - the serde deserialize into the typed schema, where a
//!    well-formed document with an out-of-range or wrong-typed value lands.
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

    // Stage 3 first: it is cheap and independent of the migration chain, so a
    // panic here is not masked by an early `return` from stage 2.
    let _ = document.config();

    // Stage 2. `migrate` consumes the document, hence the clone per step.
    for from in 0..=CURRENT_VERSION {
        let _ = migrate(document.document().clone(), from);
    }
});
