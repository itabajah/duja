#![no_main]

//! Fuzz EDID parsing and stable-id derivation: arbitrary bytes go straight into
//! `EdidInfo::parse` and `StableDisplayId::from_edid`, which must never panic.

use duja_core::id::{EdidInfo, StableDisplayId};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = EdidInfo::parse(data);
    let _ = StableDisplayId::from_edid(data);
});
