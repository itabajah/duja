#![no_main]

//! Fuzz the DDC/CI reply decoders: arbitrary bytes go straight into
//! `decode_get_vcp_reply` and `decode_caps_reply`, which must never panic,
//! index out of bounds, or loop unboundedly on a corrupt reply frame.

use duja_ddc::ddcci::{decode_caps_reply, decode_get_vcp_reply};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The VCP opcode is part of the reply contract; fuzz a couple of codes so
    // the opcode-echo check is exercised for both matches and mismatches.
    let _ = decode_get_vcp_reply(data, 0x10);
    let _ = decode_get_vcp_reply(data, 0x60);
    let _ = decode_caps_reply(data);
});
