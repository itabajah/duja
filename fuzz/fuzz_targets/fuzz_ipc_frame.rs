#![no_main]

//! Fuzz the IPC frame decoders: arbitrary bytes go straight into
//! `read_request`, `read_response`, and the raw `read_frame_bytes`, which must
//! return an `IpcError` (never panic, never allocate past the 64 KiB cap).

use std::io::Cursor;

use duja_ipc::{read_frame_bytes, read_request, read_response};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = read_request(&mut Cursor::new(data));
    let _ = read_response(&mut Cursor::new(data));
    let _ = read_frame_bytes(&mut Cursor::new(data));
});
