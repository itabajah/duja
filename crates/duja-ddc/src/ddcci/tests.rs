//! Host-run unit tests for the pure DDC/CI codec and the bus-backed transport.
//!
//! These run on every OS in CI — the wire protocol is hardware-independent, so
//! the framing, checksums and reply parsing are proved without a Mac. The
//! exact-byte vectors act as a regression corpus: any drift in the framing or
//! checksum seed changes the bytes and fails a test.

use std::collections::VecDeque;

use super::{
    CAPS_REPLY_LEN, CapsFragment, DdcCiError, DdcCiTransport, DdcWire, GET_VCP_REPLY_LEN, I2cBus,
    decode_caps_reply, decode_get_vcp_reply, encode_caps_reply, encode_get_vcp_reply,
};
use crate::transport::{TransportError, VcpReading, VcpTransport};

/// Independent XOR check used to validate a packet's own checksum byte, seeded
/// with `seed` over every byte but the last.
fn trailing_checksum_ok(packet: &[u8], seed: u8) -> bool {
    let Some((last, covered)) = packet.split_last() else {
        return false;
    };
    covered.iter().fold(seed, |acc, &b| acc ^ b) == *last
}

// --- exact-byte regression corpus ----------------------------------------

#[test]
fn intel_get_vcp_brightness_matches_monitorcontrol() {
    // IntelDDC.swift "Get VCP: 0x51, 0x82, 0x01, <vcp>, <checksum>".
    let packet = DdcWire::Intel.encode_get_vcp(0x10);
    assert_eq!(packet, vec![0x51, 0x82, 0x01, 0x10, 0xAC]);
    // Checksum seed is the display write address 0x6E.
    assert!(trailing_checksum_ok(&packet, 0x6E));
}

#[test]
fn intel_set_vcp_brightness_matches_monitorcontrol() {
    // IntelDDC.swift "Set VCP: 0x51, 0x84, 0x03, <vcp>, <MSB>, <LSB>, <chk>".
    let packet = DdcWire::Intel.encode_set_vcp(0x10, 0x0032);
    assert_eq!(packet, vec![0x51, 0x84, 0x03, 0x10, 0x00, 0x32, 0x9A]);
    assert!(trailing_checksum_ok(&packet, 0x6E));
}

#[test]
fn apple_silicon_get_vcp_matches_arm64ddc_framing() {
    // Arm64DDC.swift: [0x80|(len+1), len, body…, checksum]; body=[0x01,0x10]
    // (len 2), so byte0=0x83, byte1=0x02; seed 0x6E^0x51 = 0x3F.
    let packet = DdcWire::AppleSilicon.encode_get_vcp(0x10);
    assert_eq!(packet, vec![0x83, 0x02, 0x01, 0x10, 0xAF]);
    assert!(trailing_checksum_ok(&packet, 0x6E ^ 0x51));
}

#[test]
fn apple_silicon_and_intel_framings_differ() {
    // The whole reason DdcWire exists: the two arms are not interchangeable.
    assert_ne!(
        DdcWire::Intel.encode_get_vcp(0x10),
        DdcWire::AppleSilicon.encode_get_vcp(0x10)
    );
}

#[test]
fn apple_silicon_single_byte_body_seeds_with_write_address() {
    // A hypothetical one-byte body exercises the `send.count == 1` seed branch
    // (Arm64DDC.swift): seed is 0x6E, not 0x6E ^ 0x51.
    let packet = DdcWire::AppleSilicon.frame_request(&[0xAA]);
    // byte0 = 0x80 | 2 = 0x82, byte1 = 1, body 0xAA, checksum seeded 0x6E.
    assert_eq!(packet.get(..3), Some(&[0x82u8, 0x01, 0xAA][..]));
    assert!(trailing_checksum_ok(&packet, 0x6E));
}

// --- get-VCP reply round trips -------------------------------------------

#[test]
fn get_vcp_reply_round_trips() {
    let reply = encode_get_vcp_reply(
        0x10,
        VcpReading {
            current: 50,
            max: 100,
        },
        0x00,
    );
    let reading = decode_get_vcp_reply(&reply, 0x10).expect("valid reply decodes");
    assert_eq!(
        reading,
        VcpReading {
            current: 50,
            max: 100
        }
    );
}

#[test]
fn get_vcp_reply_decodes_wide_values() {
    // Non-trivial big-endian values catch byte-order bugs.
    let reply = encode_get_vcp_reply(
        0x12,
        VcpReading {
            current: 0x1234,
            max: 0xABCD,
        },
        0x00,
    );
    let reading = decode_get_vcp_reply(&reply, 0x12).expect("decodes");
    assert_eq!(
        reading,
        VcpReading {
            current: 0x1234,
            max: 0xABCD
        }
    );
}

#[test]
fn get_vcp_reply_unsupported_result_is_reported() {
    let reply = encode_get_vcp_reply(0x60, VcpReading { current: 0, max: 0 }, 0x01);
    assert_eq!(
        decode_get_vcp_reply(&reply, 0x60),
        Err(DdcCiError::Unsupported { code: 0x60 })
    );
}

#[test]
fn get_vcp_reply_rejects_bad_checksum() {
    let mut reply = encode_get_vcp_reply(
        0x10,
        VcpReading {
            current: 50,
            max: 100,
        },
        0x00,
    );
    if let Some(last) = reply.last_mut() {
        *last = last.wrapping_add(1);
    }
    assert_eq!(
        decode_get_vcp_reply(&reply, 0x10),
        Err(DdcCiError::Malformed)
    );
}

#[test]
fn get_vcp_reply_rejects_opcode_echo_mismatch() {
    // A reply for 0x10 must not satisfy a request for 0x12 (stale/late reply).
    let reply = encode_get_vcp_reply(
        0x10,
        VcpReading {
            current: 50,
            max: 100,
        },
        0x00,
    );
    assert_eq!(
        decode_get_vcp_reply(&reply, 0x12),
        Err(DdcCiError::Malformed)
    );
}

#[test]
fn get_vcp_reply_rejects_wrong_source_and_truncation() {
    assert_eq!(decode_get_vcp_reply(&[], 0x10), Err(DdcCiError::Malformed));
    // Wrong source address in byte 0.
    let mut reply = encode_get_vcp_reply(0x10, VcpReading { current: 1, max: 2 }, 0x00);
    if let Some(first) = reply.first_mut() {
        *first = 0x00;
    }
    assert_eq!(
        decode_get_vcp_reply(&reply, 0x10),
        Err(DdcCiError::Malformed)
    );
    // Truncated below the declared length.
    let full = encode_get_vcp_reply(0x10, VcpReading { current: 1, max: 2 }, 0x00);
    let short = full.get(..4).unwrap_or(&[]).to_vec();
    assert_eq!(
        decode_get_vcp_reply(&short, 0x10),
        Err(DdcCiError::Malformed)
    );
}

#[test]
fn get_vcp_reply_ignores_trailing_padding() {
    // Some panels pad the reply; the decoder consumes only `len` data bytes.
    let mut reply = encode_get_vcp_reply(
        0x10,
        VcpReading {
            current: 40,
            max: 80,
        },
        0x00,
    );
    reply.extend_from_slice(&[0x00, 0xFF, 0xAB]);
    let reading = decode_get_vcp_reply(&reply, 0x10).expect("padded reply decodes");
    assert_eq!(
        reading,
        VcpReading {
            current: 40,
            max: 80
        }
    );
}

// --- capabilities reply ---------------------------------------------------

#[test]
fn caps_reply_round_trips() {
    let reply = encode_caps_reply(0, b"(vcp(10 12))");
    let fragment = decode_caps_reply(&reply).expect("decodes");
    assert_eq!(
        fragment,
        CapsFragment {
            offset: 0,
            data: b"(vcp(10 12))".to_vec()
        }
    );
}

#[test]
fn caps_reply_empty_payload_is_terminator() {
    let reply = encode_caps_reply(42, b"");
    let fragment = decode_caps_reply(&reply).expect("decodes");
    assert_eq!(fragment.offset, 42);
    assert!(fragment.data.is_empty());
}

// --- decode is total (fuzz-style) ----------------------------------------

use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4_000))]

    /// The reply decoders must never panic on arbitrary bytes.
    #[test]
    fn decoders_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        let _ = decode_get_vcp_reply(&bytes, 0x10);
        let _ = decode_caps_reply(&bytes);
    }

    /// Any value round-trips through the get-VCP encode/decode pair.
    #[test]
    fn any_reading_round_trips(current in any::<u16>(), max in any::<u16>()) {
        let reply = encode_get_vcp_reply(0x10, VcpReading { current, max }, 0x00);
        let decoded = decode_get_vcp_reply(&reply, 0x10).expect("decodes");
        prop_assert_eq!(decoded, VcpReading { current, max });
    }
}

// --- transport over a scripted bus ---------------------------------------

/// A minimal [`I2cBus`] that logs writes and serves queued replies in order.
#[derive(Debug)]
struct ScriptBus {
    wire: DdcWire,
    replies: VecDeque<Vec<u8>>,
    writes: Vec<Vec<u8>>,
}

impl ScriptBus {
    fn new(wire: DdcWire, replies: Vec<Vec<u8>>) -> Self {
        ScriptBus {
            wire,
            replies: replies.into(),
            writes: Vec::new(),
        }
    }
}

impl I2cBus for ScriptBus {
    fn wire(&self) -> DdcWire {
        self.wire
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        self.writes.push(data.to_vec());
        Ok(())
    }

    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        assert!(len == GET_VCP_REPLY_LEN || len == CAPS_REPLY_LEN);
        self.replies.pop_front().ok_or(TransportError::Timeout)
    }
}

#[test]
fn transport_reads_vcp_through_the_bus() {
    let reply = encode_get_vcp_reply(
        0x10,
        VcpReading {
            current: 55,
            max: 90,
        },
        0x00,
    );
    let bus = ScriptBus::new(DdcWire::AppleSilicon, vec![reply]);
    let mut transport = DdcCiTransport::new(bus);
    let reading = transport.read_vcp(0x10).expect("read succeeds");
    assert_eq!(
        reading,
        VcpReading {
            current: 55,
            max: 90
        }
    );
    // The framed request the transport wrote is the Apple Silicon get packet.
    assert_eq!(
        transport.bus().writes.first().map(Vec::as_slice),
        Some(&[0x83u8, 0x02, 0x01, 0x10, 0xAF][..])
    );
}

#[test]
fn transport_reassembles_multi_fragment_capabilities() {
    // Two data fragments then an empty terminator reassemble to the full string.
    let replies = vec![
        encode_caps_reply(0, b"(vcp(10 "),
        encode_caps_reply(8, b"12))"),
        encode_caps_reply(12, b""),
    ];
    let bus = ScriptBus::new(DdcWire::Intel, replies);
    let mut transport = DdcCiTransport::new(bus);
    let caps = transport.read_capabilities().expect("caps read succeeds");
    assert_eq!(caps, "(vcp(10 12))");
    // One caps request per fragment (three total).
    assert_eq!(transport.bus().writes.len(), 3);
}

#[test]
fn transport_maps_bus_disconnect_through() {
    #[derive(Debug)]
    struct DeadBus;
    impl I2cBus for DeadBus {
        fn wire(&self) -> DdcWire {
            DdcWire::Intel
        }
        fn write(&mut self, _data: &[u8]) -> Result<(), TransportError> {
            Err(TransportError::Disconnected)
        }
        fn read(&mut self, _len: usize) -> Result<Vec<u8>, TransportError> {
            Err(TransportError::Disconnected)
        }
    }
    let mut transport = DdcCiTransport::new(DeadBus);
    assert!(matches!(
        transport.read_vcp(0x10),
        Err(TransportError::Disconnected)
    ));
}
