#![no_main]

//! Fuzz the quirk-database parser: arbitrary bytes are lossily decoded to a
//! `str` and fed to `QuirkDb::parse`, which must never panic (the 1 MiB cap and
//! schema gate bound the work before any allocation).

use duja_core::quirks::QuirkDb;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = QuirkDb::parse(&text);
});
