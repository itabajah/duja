//! Pure DDC/CI (VESA MCCS) wire-packet codec and the I2C-bus seam the macOS
//! transport is built on.
//!
//! Unlike Windows — where the OS dxva2 layer frames DDC/CI packets for us — a
//! macOS backend must build the raw I2C payloads itself and talk to the display
//! at I2C slave address `0x37`. This module owns that framing as **total,
//! side-effect-free** functions over byte buffers:
//!
//! - [`DdcWire`] captures the two request framings Duja must emit. The Intel
//!   `IOI2CSendRequest` path uses the standard MCCS framing; Apple Silicon's
//!   private `IOAVServiceWriteI2C` path wants MonitorControl's slightly
//!   different framing (an extra length byte and a different checksum seed).
//!   Both are encoded here, per-arm, with cited constants.
//! - [`decode_get_vcp_reply`] and [`decode_caps_reply`] parse the display's
//!   replies, which are the *same* standard DDC/CI reply frame on both arms.
//! - [`DdcCiTransport`] turns an [`I2cBus`] (a couple of raw byte operations)
//!   into a [`VcpTransport`], so **all** controller policy (pacing, retry,
//!   verify, quirks) is reused unchanged. The real buses live in the macOS-only
//!   `mac::sys` module; a scriptable fake drives the codec in host tests.
//!
//! The whole module is safe, cross-platform Rust and is unit-tested on every OS
//! in CI — the wire protocol is identical regardless of who carries the bytes,
//! so the framing is proved without macOS hardware. Only the concrete
//! [`I2cBus`] implementations are platform- and hardware-specific.
//!
//! # Protocol references
//! Byte layouts, the two framings, and the checksum convention are quoted from
//! the two mature open-source implementations Duja cross-checked: MonitorControl
//! (`MonitorControl/Support/Arm64DDC.swift` and `IntelDDC.swift`) and the
//! `ddc-macos` crate. Every magic constant below cites its role; see ADR-0013.

// RATIONALE: the codec's public vocabulary (`DdcCiError`, `DdcCiTransport`,
// `DdcWire`) shares the `ddcci` module stem; the qualified names read best at
// call sites and the surface is small and frozen by the protocol.
#![allow(clippy::module_name_repetitions)]
// RATIONALE: the docs cite framework/protocol identifiers by name
// (MonitorControl, Arm64DDC, IOAVServiceWriteI2C, ddc-macos, MCCS) in running
// prose where backticking every mention would hurt readability; the ones that
// are Rust items are already backticked.
#![allow(clippy::doc_markdown)]

use std::fmt;

use crate::transport::{TransportError, VcpReading, VcpTransport};

/// The DDC/CI display I2C slave address (7-bit). Both bus backends pass this as
/// the chip address; the wire read/write addresses are `0x6F`/`0x6E`.
///
/// Source: MonitorControl `Arm64DDC.swift` `ARM64_DDC_7BIT_ADDRESS = 0x37`.
pub const DDC_I2C_ADDRESS: u8 = 0x37;

/// Source/sub-address a host stamps for a request. On Intel it is the first
/// packet byte; on Apple Silicon it is the `dataAddress` argument.
///
/// Source: standard DDC/CI host address `0x50 | write` = `0x51`; MonitorControl
/// `ARM64_DDC_DATA_ADDRESS = 0x51`.
const HOST_SOURCE_ADDRESS: u8 = 0x51;

/// The display's 8-bit write address (`0x37 << 1`). It seeds a request's
/// checksum even though the I2C layer carries the address out of band.
const DISPLAY_WRITE_ADDRESS: u8 = 0x6E;

/// The virtual host address that seeds a display→host **reply** checksum (VESA
/// DDC/CI: the receiver's own address, `0x50`).
const HOST_RECEIVE_ADDRESS: u8 = 0x50;

/// The source-address byte a display stamps into its replies (its own write
/// address, `0x6E`).
const DISPLAY_SOURCE_ADDRESS: u8 = 0x6E;

/// High bit OR-ed into the length byte of every DDC/CI message.
const LENGTH_FLAG: u8 = 0x80;

/// Op-code: "Get VCP Feature" request.
const OP_GET_VCP: u8 = 0x01;
/// Op-code: "Get VCP Feature" reply (display → host).
const OP_GET_VCP_REPLY: u8 = 0x02;
/// Op-code: "Set VCP Feature" request (no reply).
const OP_SET_VCP: u8 = 0x03;
/// Op-code: "Capabilities Request".
const OP_CAPS_REQUEST: u8 = 0xF3;
/// Op-code: "Capabilities Reply" (display → host).
const OP_CAPS_REPLY: u8 = 0xE3;

/// DDC/CI result code meaning the requested VCP opcode is unsupported.
const RESULT_UNSUPPORTED_VCP: u8 = 0x01;

/// Bytes to request from the bus for a Get-VCP reply: 1 source + 1 length + 8
/// data + 1 checksum. A few real panels pad the reply, so the bus may return
/// more; the decoder locates the frame by its length byte.
pub const GET_VCP_REPLY_LEN: usize = 11;

/// Bytes to request from the bus for one capabilities-reply fragment: header
/// (source + length + op + 2 offset) + up to 32 payload bytes + checksum.
pub const CAPS_REPLY_LEN: usize = 64;

/// The most capability-reply fragments to read before giving up (a runaway or
/// non-terminating display cannot loop us forever). 64 KiB / 32-byte fragments.
const MAX_CAPS_FRAGMENTS: usize = 2048;

/// Which framing a request is encoded in — the two macOS I2C paths disagree on
/// the header and checksum seed, and getting it wrong yields a display that
/// never answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdcWire {
    /// Intel Macs via `IOI2CSendRequest`: the standard MCCS framing
    /// `[0x51, 0x80|len, body…, checksum]`, checksum seeded with the display
    /// write address `0x6E`. Source: MonitorControl `IntelDDC.swift`
    /// ("Get VCP: `0x51, 0x82, 0x01, <vcp>, <checksum>`").
    Intel,
    /// Apple Silicon via the private `IOAVServiceWriteI2C`: MonitorControl's
    /// framing `[0x80|(len+1), len, body…, checksum]` with the `0x51` source
    /// carried as the call's `dataAddress`, checksum seeded `0x6E` for a
    /// one-byte body else `0x6E ^ 0x51`. Source: `Arm64DDC.swift`.
    AppleSilicon,
}

/// XOR-fold `bytes` into `seed` — the DDC/CI checksum. XOR cannot overflow, so
/// this is total. Source: `Arm64DDC.swift::checksum`.
fn checksum(seed: u8, bytes: &[u8]) -> u8 {
    bytes.iter().fold(seed, |acc, &b| acc ^ b)
}

impl DdcWire {
    /// Frame a request `body` (the op-code and its arguments) into the full I2C
    /// payload for this wire style.
    ///
    /// `body` is a tiny compile-time-bounded slice (≤ 4 bytes, fixed by the
    /// op-code), so the `as u8` narrowing of its length cannot truncate.
    fn frame_request(self, body: &[u8]) -> Vec<u8> {
        // RATIONALE: request bodies are ≤ 4 bytes (fixed by op-code), so the low
        // 7 bits hold the length exactly — no truncation is possible.
        #[allow(clippy::cast_possible_truncation)]
        let len = body.len() as u8;
        match self {
            DdcWire::Intel => {
                let mut packet = Vec::with_capacity(body.len().saturating_add(3));
                packet.push(HOST_SOURCE_ADDRESS);
                packet.push(LENGTH_FLAG | (len & 0x7F));
                packet.extend_from_slice(body);
                packet.push(checksum(DISPLAY_WRITE_ADDRESS, &packet));
                packet
            }
            DdcWire::AppleSilicon => {
                let mut packet = Vec::with_capacity(body.len().saturating_add(3));
                packet.push(LENGTH_FLAG | (len.saturating_add(1) & 0x7F));
                packet.push(len);
                packet.extend_from_slice(body);
                // Seed: 0x6E for a single-byte body, else 0x6E ^ 0x51.
                let seed = if body.len() == 1 {
                    DISPLAY_WRITE_ADDRESS
                } else {
                    DISPLAY_WRITE_ADDRESS ^ HOST_SOURCE_ADDRESS
                };
                packet.push(checksum(seed, &packet));
                packet
            }
        }
    }

    /// Build a "Get VCP Feature" request payload for VCP `code`.
    #[must_use]
    pub fn encode_get_vcp(self, code: u8) -> Vec<u8> {
        self.frame_request(&[OP_GET_VCP, code])
    }

    /// Build a "Set VCP Feature" request payload writing `value` (big-endian,
    /// per DDC/CI) to VCP `code`.
    #[must_use]
    pub fn encode_set_vcp(self, code: u8, value: u16) -> Vec<u8> {
        let [hi, lo] = value.to_be_bytes();
        self.frame_request(&[OP_SET_VCP, code, hi, lo])
    }

    /// Build a "Capabilities Request" payload for the fragment at `offset`.
    #[must_use]
    pub fn encode_caps_request(self, offset: u16) -> Vec<u8> {
        let [hi, lo] = offset.to_be_bytes();
        self.frame_request(&[OP_CAPS_REQUEST, hi, lo])
    }
}

/// A failure decoding a DDC/CI reply. Kept separate from [`TransportError`] so
/// the codec stays pure and host-testable; the transport maps these onto the
/// classified transport error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DdcCiError {
    /// The reply was shorter than its own length byte requires, had the wrong
    /// source address or op-code, or failed its XOR checksum. Transient on real
    /// hardware (DDC/CI is flaky); the transport retries it.
    #[error("malformed or corrupt DDC/CI reply")]
    Malformed,
    /// The display answered that the requested VCP opcode is unsupported.
    #[error("the display reports VCP feature {code:#04x} as unsupported")]
    Unsupported {
        /// The VCP opcode the display rejected.
        code: u8,
    },
}

/// Locate and validate a reply frame in `buf`, returning its data bytes (the
/// op-code and everything up to, but not including, the checksum).
///
/// A DDC/CI reply is `[source, 0x80|len, data(len)…, checksum]` on both arms.
/// The frame is validated against the expected source address, the declared
/// length, and the XOR checksum (seeded with the receiving host address).
/// Trailing padding some panels append is ignored — only `len` data bytes are
/// consumed.
fn parse_reply(buf: &[u8]) -> Result<&[u8], DdcCiError> {
    let source = buf.first().copied().ok_or(DdcCiError::Malformed)?;
    if source != DISPLAY_SOURCE_ADDRESS {
        return Err(DdcCiError::Malformed);
    }
    let length_byte = buf.get(1).copied().ok_or(DdcCiError::Malformed)?;
    if length_byte & LENGTH_FLAG == 0 {
        return Err(DdcCiError::Malformed);
    }
    let data_len = usize::from(length_byte & 0x7F);
    // The checksum sits immediately after the `data_len` data bytes (which
    // start at index 2). Everything up to it is covered by the checksum.
    let checksum_idx = data_len.saturating_add(2);
    let stated = buf
        .get(checksum_idx)
        .copied()
        .ok_or(DdcCiError::Malformed)?;
    let covered = buf.get(..checksum_idx).ok_or(DdcCiError::Malformed)?;
    if checksum(HOST_RECEIVE_ADDRESS, covered) != stated {
        return Err(DdcCiError::Malformed);
    }
    buf.get(2..checksum_idx).ok_or(DdcCiError::Malformed)
}

/// Decode a "Get VCP Feature" reply for VCP `code` into a [`VcpReading`].
///
/// The 8-byte reply data is `[op, result, code, type, max_hi, max_lo, cur_hi,
/// cur_lo]`. A result code of "unsupported VCP" yields
/// [`DdcCiError::Unsupported`]; any structural or checksum fault yields
/// [`DdcCiError::Malformed`].
///
/// # Errors
/// [`DdcCiError`] if the reply is malformed or the feature is unsupported.
pub fn decode_get_vcp_reply(buf: &[u8], code: u8) -> Result<VcpReading, DdcCiError> {
    let data = parse_reply(buf)?;
    if data.first().copied() != Some(OP_GET_VCP_REPLY) {
        return Err(DdcCiError::Malformed);
    }
    let result = data.get(1).copied().ok_or(DdcCiError::Malformed)?;
    if result == RESULT_UNSUPPORTED_VCP {
        return Err(DdcCiError::Unsupported { code });
    }
    if result != 0x00 {
        return Err(DdcCiError::Malformed);
    }
    // data.get(2) echoes the VCP opcode; a mismatch means a stale/late reply.
    if data.get(2).copied() != Some(code) {
        return Err(DdcCiError::Malformed);
    }
    let max_hi = data.get(4).copied().ok_or(DdcCiError::Malformed)?;
    let max_lo = data.get(5).copied().ok_or(DdcCiError::Malformed)?;
    let cur_hi = data.get(6).copied().ok_or(DdcCiError::Malformed)?;
    let cur_lo = data.get(7).copied().ok_or(DdcCiError::Malformed)?;
    Ok(VcpReading {
        max: u16::from_be_bytes([max_hi, max_lo]),
        current: u16::from_be_bytes([cur_hi, cur_lo]),
    })
}

/// One decoded capabilities-reply fragment: the payload bytes and the offset the
/// display echoed for them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapsFragment {
    /// The byte offset into the capability string this fragment starts at.
    pub offset: u16,
    /// The fragment's payload bytes (empty terminates the read).
    pub data: Vec<u8>,
}

/// Decode a "Capabilities Reply" fragment.
///
/// The reply data is `[op(0xE3), offset_hi, offset_lo, payload…]`. An empty
/// payload signals the end of the capability string.
///
/// # Errors
/// [`DdcCiError::Malformed`] if the reply is structurally invalid or fails its
/// checksum.
pub fn decode_caps_reply(buf: &[u8]) -> Result<CapsFragment, DdcCiError> {
    let data = parse_reply(buf)?;
    if data.first().copied() != Some(OP_CAPS_REPLY) {
        return Err(DdcCiError::Malformed);
    }
    let offset_hi = data.get(1).copied().ok_or(DdcCiError::Malformed)?;
    let offset_lo = data.get(2).copied().ok_or(DdcCiError::Malformed)?;
    let payload = data.get(3..).unwrap_or(&[]).to_vec();
    Ok(CapsFragment {
        offset: u16::from_be_bytes([offset_hi, offset_lo]),
        data: payload,
    })
}

/// The raw per-display I2C bus a [`DdcCiTransport`] drives: report the wire
/// style, write a framed request, then read a reply. Implementations own the
/// chip address ([`DDC_I2C_ADDRESS`]) and any hardware reply delay; the
/// transport above is timing-agnostic.
///
/// `&mut self` matches the transport's exclusive ownership (never shared, no
/// interior locking). `Send` lets the owning controller move onto a per-monitor
/// worker thread; `Debug` lets it be logged.
pub trait I2cBus: Send + fmt::Debug {
    /// The framing this bus requires (Intel vs Apple Silicon).
    fn wire(&self) -> DdcWire;

    /// Write a framed DDC/CI request payload to the display.
    ///
    /// # Errors
    /// A [`TransportError`] if the I2C write is not acknowledged.
    fn write(&mut self, data: &[u8]) -> Result<(), TransportError>;

    /// Read up to `len` reply bytes from the display, after the bus's own
    /// mandatory post-request delay.
    ///
    /// # Errors
    /// A [`TransportError`] if the I2C read fails.
    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError>;
}

/// A [`VcpTransport`] built from an [`I2cBus`]: it frames requests, parses
/// replies, and hands every policy decision (pacing, retry, verify, quirks) up
/// to the [`DdcController`](crate::controller::DdcController). This is the macOS
/// backend's transport, generic over the bus so the same logic is exercised by
/// a fake bus on every OS.
#[derive(Debug)]
pub struct DdcCiTransport<B: I2cBus> {
    bus: B,
}

impl<B: I2cBus> DdcCiTransport<B> {
    /// Wrap an [`I2cBus`] as a transport.
    #[must_use]
    pub fn new(bus: B) -> Self {
        DdcCiTransport { bus }
    }

    /// Borrow the underlying bus (for inspection in tests).
    #[must_use]
    pub fn bus(&self) -> &B {
        &self.bus
    }
}

/// Map a codec error onto the classified transport error. A malformed reply is
/// the common transient DDC no-reply the controller retries; an unsupported
/// feature is reported as a timeout so probe-by-reads treats it as absent
/// (there is no "unsupported" at the transport layer — the controller owns that
/// decision via the capability set).
fn codec_error(err: DdcCiError) -> TransportError {
    match err {
        DdcCiError::Malformed | DdcCiError::Unsupported { .. } => TransportError::Timeout,
    }
}

impl<B: I2cBus> VcpTransport for DdcCiTransport<B> {
    fn read_vcp(&mut self, code: u8) -> Result<VcpReading, TransportError> {
        let request = self.bus.wire().encode_get_vcp(code);
        self.bus.write(&request)?;
        let reply = self.bus.read(GET_VCP_REPLY_LEN)?;
        decode_get_vcp_reply(&reply, code).map_err(codec_error)
    }

    fn write_vcp(&mut self, code: u8, value: u16) -> Result<(), TransportError> {
        // "Set VCP Feature" has no reply; the controller's pacing enforces the
        // post-write settle time before the next operation.
        let request = self.bus.wire().encode_set_vcp(code, value);
        self.bus.write(&request)
    }

    fn read_capabilities(&mut self) -> Result<String, TransportError> {
        let wire = self.bus.wire();
        let mut raw: Vec<u8> = Vec::new();
        let mut offset: u16 = 0;
        for _ in 0..MAX_CAPS_FRAGMENTS {
            self.bus.write(&wire.encode_caps_request(offset))?;
            let reply = self.bus.read(CAPS_REPLY_LEN)?;
            let fragment = decode_caps_reply(&reply).map_err(codec_error)?;
            // A well-behaved display echoes the requested offset; a mismatch is
            // a desync we treat as a transient fault.
            if fragment.offset != offset {
                return Err(TransportError::Timeout);
            }
            if fragment.data.is_empty() {
                break;
            }
            let advance = u16::try_from(fragment.data.len()).unwrap_or(u16::MAX);
            offset = offset.saturating_add(advance);
            raw.extend_from_slice(&fragment.data);
            if raw.len() >= duja_core::caps::MAX_CAPS_LEN {
                break;
            }
        }
        if raw.is_empty() {
            return Err(TransportError::Timeout);
        }
        Ok(String::from_utf8_lossy(&raw).into_owned())
    }
}

/// Frame a display→host reply `body` (op-code and arguments) into a full reply
/// payload: `[source, 0x80|len, body…, checksum]`, checksum seeded with the
/// receiving host address. Shared by the codec tests and the scriptable fake
/// I2C bus so the decoder is always tested against a real encoder.
#[cfg(test)]
pub(crate) fn frame_reply(body: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(body.len().saturating_add(3));
    packet.push(DISPLAY_SOURCE_ADDRESS);
    // RATIONALE: reply bodies built in tests are small and fixed-shape; the low
    // 7 bits hold the length exactly, so no truncation is possible.
    #[allow(clippy::cast_possible_truncation)]
    let len_byte = LENGTH_FLAG | (body.len() as u8 & 0x7F);
    packet.push(len_byte);
    packet.extend_from_slice(body);
    packet.push(checksum(HOST_RECEIVE_ADDRESS, &packet));
    packet
}

/// Build a "Get VCP Feature" reply payload reporting `reading` for VCP `code`
/// with the given result code (0 = success, 1 = unsupported).
#[cfg(test)]
pub(crate) fn encode_get_vcp_reply(code: u8, reading: VcpReading, result: u8) -> Vec<u8> {
    let [max_hi, max_lo] = reading.max.to_be_bytes();
    let [cur_hi, cur_lo] = reading.current.to_be_bytes();
    frame_reply(&[
        OP_GET_VCP_REPLY,
        result,
        code,
        0x00, // VCP type: "Set parameter" (continuous)
        max_hi,
        max_lo,
        cur_hi,
        cur_lo,
    ])
}

/// Build a "Capabilities Reply" payload carrying `data` at `offset`.
#[cfg(test)]
pub(crate) fn encode_caps_reply(offset: u16, data: &[u8]) -> Vec<u8> {
    let [off_hi, off_lo] = offset.to_be_bytes();
    let mut body = vec![OP_CAPS_REPLY, off_hi, off_lo];
    body.extend_from_slice(data);
    frame_reply(&body)
}

#[cfg(test)]
mod tests;
