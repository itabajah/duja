#![no_main]

//! Fuzz the MCCS capability-string parser: arbitrary bytes are lossily decoded
//! to a `str` and fed to `ParsedCaps::parse`, which must never panic.

use duja_core::caps::ParsedCaps;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = ParsedCaps::parse(&text);
});
