//! Length-prefixed JSON framing and the crate-wide [`IpcError`].
//!
//! Each frame on the wire is a little-endian `u32` byte-length prefix followed
//! by that many bytes of UTF-8 JSON. The length is checked against
//! [`MAX_FRAME_LEN`] **before** any body buffer is allocated, so a hostile or
//! corrupt 4-byte header can never drive an allocation larger than the cap —
//! the single most important property of this module (SECURITY.md §IPC).

use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// The maximum size, in bytes, of a single frame body.
///
/// Enforced against the length prefix before allocation on read, and against
/// the serialized body on write. Control-plane messages are tiny; 64 KiB is a
/// generous ceiling that still bounds a single malicious frame's memory cost.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// A failure encountered while framing, unframing, or validating an IPC
/// message.
///
/// The variants are deliberately specific so callers (and, later, the P5
/// transport) can distinguish a recoverable stream boundary
/// ([`IpcError::UnexpectedEof`]) from a protocol violation
/// ([`IpcError::FrameTooLarge`], [`IpcError::UnsupportedVersion`],
/// [`IpcError::InvalidField`]) or a codec fault ([`IpcError::Json`]).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IpcError {
    /// The length prefix (or a serialized body) exceeded [`MAX_FRAME_LEN`].
    ///
    /// On read this is raised from the 4-byte header alone — before the body is
    /// touched — so the oversized body is never allocated or consumed.
    #[error("frame length {len} exceeds the {} byte maximum", MAX_FRAME_LEN)]
    FrameTooLarge {
        /// The rejected length, as read from the prefix or measured on write.
        len: usize,
    },
    /// The stream ended part-way through a frame (a short read).
    #[error("unexpected end of stream while reading a frame")]
    UnexpectedEof,
    /// An underlying reader/writer I/O error that was not a clean EOF.
    #[error("i/o error: {0}")]
    Io(#[source] io::Error),
    /// The body was not valid JSON for the target type.
    #[error("json codec error: {0}")]
    Json(#[from] serde_json::Error),
    /// The envelope carried a protocol version this build does not speak.
    #[error("unsupported protocol version {found}, expected {expected}")]
    UnsupportedVersion {
        /// The version seen on the wire.
        found: u16,
        /// The version this build accepts.
        expected: u16,
    },
    /// A decoded field failed validation (range, charset, or length).
    #[error("invalid field `{field}`: {reason}")]
    InvalidField {
        /// The offending field's name.
        field: &'static str,
        /// A human-readable description of why it was rejected.
        reason: String,
    },
}

/// Map an I/O error, folding a short read into [`IpcError::UnexpectedEof`].
fn map_read_io(err: io::Error) -> IpcError {
    if err.kind() == io::ErrorKind::UnexpectedEof {
        IpcError::UnexpectedEof
    } else {
        IpcError::Io(err)
    }
}

/// Serialize `value` to JSON and write it as one length-prefixed frame.
///
/// # Errors
/// - [`IpcError::Json`] if `value` cannot be serialized.
/// - [`IpcError::FrameTooLarge`] if the JSON body exceeds [`MAX_FRAME_LEN`].
/// - [`IpcError::Io`] if the writer fails.
pub fn write_frame<T: Serialize>(writer: &mut impl Write, value: &T) -> Result<(), IpcError> {
    let body = serde_json::to_vec(value)?;
    if body.len() > MAX_FRAME_LEN {
        return Err(IpcError::FrameTooLarge { len: body.len() });
    }
    // The cap check above guarantees the length fits in u32.
    let len = u32::try_from(body.len()).map_err(|_| IpcError::FrameTooLarge { len: body.len() })?;
    writer.write_all(&len.to_le_bytes()).map_err(IpcError::Io)?;
    writer.write_all(&body).map_err(IpcError::Io)?;
    Ok(())
}

/// Read one length-prefixed frame's raw body bytes, enforcing the cap on the
/// prefix before allocating.
///
/// # Errors
/// - [`IpcError::UnexpectedEof`] on a stream that ends inside the header or
///   body.
/// - [`IpcError::FrameTooLarge`] if the prefix declares more than
///   [`MAX_FRAME_LEN`] bytes — raised **before** the body buffer is allocated.
/// - [`IpcError::Io`] on any other reader failure.
pub fn read_frame_bytes(reader: &mut impl Read) -> Result<Vec<u8>, IpcError> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).map_err(map_read_io)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    // Enforce the ceiling on the declared length BEFORE allocating anything.
    if len > MAX_FRAME_LEN {
        return Err(IpcError::FrameTooLarge { len });
    }
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).map_err(map_read_io)?;
    Ok(body)
}

/// Read one length-prefixed frame and deserialize its body as `T`.
///
/// This is the generic primitive; [`crate::read_request`] /
/// [`crate::read_response`] layer version and field validation on top.
///
/// # Errors
/// Any [`IpcError`] from [`read_frame_bytes`], plus [`IpcError::Json`] if the
/// body is not valid JSON for `T`.
pub fn read_frame<T: DeserializeOwned>(reader: &mut impl Read) -> Result<T, IpcError> {
    let body = read_frame_bytes(reader)?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Prepend a little-endian `u32` length prefix to `body`.
    fn framed(body: &[u8]) -> Vec<u8> {
        let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(body);
        frame
    }

    #[test]
    fn roundtrips_a_plain_value() {
        let mut buf = Vec::new();
        write_frame(&mut buf, &"hello").unwrap();
        let mut cursor = Cursor::new(buf);
        let got: String = read_frame(&mut cursor).unwrap();
        assert_eq!(got, "hello");
    }

    #[test]
    fn oversized_length_prefix_rejected_before_body() {
        // A header claiming 100 KiB, followed by only a couple of body bytes.
        // FrameTooLarge must fire from the header alone — never UnexpectedEof,
        // which is what a post-allocation read of the missing body would give.
        let mut frame = Vec::new();
        let claimed: u32 = u32::try_from(MAX_FRAME_LEN).unwrap() + 1;
        frame.extend_from_slice(&claimed.to_le_bytes());
        frame.extend_from_slice(b"{}");
        let mut cursor = Cursor::new(frame);
        let err = read_frame_bytes(&mut cursor).unwrap_err();
        assert!(
            matches!(err, IpcError::FrameTooLarge { len } if len == MAX_FRAME_LEN + 1),
            "expected FrameTooLarge, got {err:?}"
        );
    }

    #[test]
    fn exactly_max_len_is_allowed_by_the_cap_check() {
        // A body of exactly MAX_FRAME_LEN passes the length gate (it is the
        // boundary), so the failure is the honest short read of the body.
        let claimed: u32 = u32::try_from(MAX_FRAME_LEN).unwrap();
        let mut cursor = Cursor::new(claimed.to_le_bytes().to_vec());
        assert!(matches!(
            read_frame_bytes(&mut cursor),
            Err(IpcError::UnexpectedEof)
        ));
    }

    #[test]
    fn truncated_header_is_unexpected_eof() {
        let mut cursor = Cursor::new(vec![0x01, 0x02]); // only 2 of 4 length bytes
        assert!(matches!(
            read_frame_bytes(&mut cursor),
            Err(IpcError::UnexpectedEof)
        ));
    }

    #[test]
    fn empty_stream_is_unexpected_eof() {
        let mut cursor = Cursor::new(Vec::new());
        assert!(matches!(
            read_frame_bytes(&mut cursor),
            Err(IpcError::UnexpectedEof)
        ));
    }

    #[test]
    fn truncated_body_is_unexpected_eof() {
        let mut frame = 8u32.to_le_bytes().to_vec(); // claims 8 bytes
        frame.extend_from_slice(b"ab"); // supplies 2
        let mut cursor = Cursor::new(frame);
        assert!(matches!(
            read_frame_bytes(&mut cursor),
            Err(IpcError::UnexpectedEof)
        ));
    }

    #[test]
    fn garbage_json_body_is_a_json_error() {
        let mut cursor = Cursor::new(framed(b"not json"));
        let err = read_frame::<String>(&mut cursor).unwrap_err();
        assert!(matches!(err, IpcError::Json(_)), "got {err:?}");
    }

    #[test]
    fn write_rejects_a_body_over_the_cap() {
        // A string long enough that its JSON body clears MAX_FRAME_LEN.
        let huge = "x".repeat(MAX_FRAME_LEN + 10);
        let mut buf = Vec::new();
        let err = write_frame(&mut buf, &huge).unwrap_err();
        assert!(matches!(err, IpcError::FrameTooLarge { .. }), "got {err:?}");
        assert!(
            buf.is_empty(),
            "nothing should be written for an oversized frame"
        );
    }
}
