//! Local IPC between `dujactl` (and second app instances) and the running app.
//!
//! Protocol: length-prefixed JSON, 64 KiB max frame enforced **before**
//! allocation, versioned envelope, strict parameter validation. Transports
//! (P5): Windows named pipe with explicit user-only DACL and anti-squatting
//! flags; unix sockets with 0600 perms + peer-uid checks. See SECURITY.md.
//!
//! # Layers
//!
//! - [`frame`] is the generic length-prefixed JSON codec ([`write_frame`],
//!   [`read_frame`]) and the crate error [`IpcError`]. The frame length is
//!   validated against [`MAX_FRAME_LEN`] before any body allocation.
//! - [`protocol`] is the versioned [`Request`]/[`Response`] message set, the
//!   wire envelope, and on-decode validation.
//! - The [`write_request`]/[`read_request`]/[`write_response`]/[`read_response`]
//!   helpers below tie the two together: they wrap a message in the `{"v":…}`
//!   envelope, frame it, and — on read — check the version and run field
//!   validation before returning. Prefer them over the raw codec.
//!
//! ```
//! use duja_ipc::{Request, read_request, write_request};
//!
//! let mut buf = Vec::new();
//! write_request(&mut buf, &Request::GetBrightness { id: "GSM-5B09-x".into() }).unwrap();
//! let echoed = read_request(&mut std::io::Cursor::new(buf)).unwrap();
//! assert_eq!(echoed, Request::GetBrightness { id: "GSM-5B09-x".into() });
//! ```

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod endpoint;
pub mod frame;
pub mod protocol;

use std::io::{Read, Write};

pub use endpoint::{error_response, exchange, serve_once};
pub use frame::{IpcError, MAX_FRAME_LEN, read_frame, read_frame_bytes, write_frame};
pub use protocol::{
    DisplayInfo, DisplayKindDto, FeatureDto, ID_MAX_LEN, PROTOCOL_VERSION, Request, Response,
};

use protocol::{RequestEnvelope, ResponseEnvelope, VersionPeek, check_version};

/// Frame a [`Request`] inside the current-version envelope and write it.
///
/// # Errors
/// Any [`IpcError`] from [`write_frame`] (serialization, oversized body, I/O).
pub fn write_request(writer: &mut impl Write, request: &Request) -> Result<(), IpcError> {
    let envelope = RequestEnvelope {
        v: PROTOCOL_VERSION,
        request: request.clone(),
    };
    write_frame(writer, &envelope)
}

/// Read one frame, check its version, decode it as a [`Request`], and validate
/// its fields.
///
/// Decoding is strict: unknown envelope keys, unknown variants, and unknown
/// variant fields are all rejected (server-side strictness).
///
/// # Errors
/// - [`IpcError::UnsupportedVersion`] if the envelope's version is not
///   [`PROTOCOL_VERSION`].
/// - [`IpcError::Json`] for malformed or non-conforming JSON.
/// - [`IpcError::InvalidField`] if a decoded id or percentage is out of range.
/// - [`IpcError::FrameTooLarge`] / [`IpcError::UnexpectedEof`] / [`IpcError::Io`]
///   from the underlying codec.
pub fn read_request(reader: &mut impl Read) -> Result<Request, IpcError> {
    let body = read_frame_bytes(reader)?;
    let peek: VersionPeek = serde_json::from_slice(&body)?;
    check_version(peek.v)?;
    let envelope: RequestEnvelope = serde_json::from_slice(&body)?;
    envelope.request.validate()?;
    Ok(envelope.request)
}

/// Frame a [`Response`] inside the current-version envelope and write it.
///
/// # Errors
/// Any [`IpcError`] from [`write_frame`] (serialization, oversized body, I/O).
pub fn write_response(writer: &mut impl Write, response: &Response) -> Result<(), IpcError> {
    let envelope = ResponseEnvelope {
        v: PROTOCOL_VERSION,
        response: response.clone(),
    };
    write_frame(writer, &envelope)
}

/// Read one frame, check its version, decode it as a [`Response`], and validate
/// its fields.
///
/// Decoding is lenient about unknown fields (forward compatibility with a newer
/// server) but still validates ids and percentages that are present.
///
/// # Errors
/// As [`read_request`], but for the response envelope.
pub fn read_response(reader: &mut impl Read) -> Result<Response, IpcError> {
    let body = read_frame_bytes(reader)?;
    let peek: VersionPeek = serde_json::from_slice(&body)?;
    check_version(peek.v)?;
    let envelope: ResponseEnvelope = serde_json::from_slice(&body)?;
    envelope.response.validate()?;
    Ok(envelope.response)
}

/// The crate version, as compiled in.
#[must_use]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Prepend a little-endian `u32` length prefix to a raw JSON body.
    fn framed(body: &[u8]) -> Vec<u8> {
        let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(body);
        frame
    }

    fn sample_display() -> DisplayInfo {
        DisplayInfo {
            id: "GSM-5B09-312NTAB1C234".to_owned(),
            name: "LG UltraGear".to_owned(),
            kind: DisplayKindDto::ExternalDdc,
            software_only: false,
            level_pct: 73,
            features: vec![FeatureDto::Brightness, FeatureDto::Contrast],
        }
    }

    #[test]
    fn links_against_core() {
        assert_eq!(version(), duja_core::version());
    }

    #[test]
    fn every_request_variant_roundtrips() {
        for req in [
            Request::ListDisplays,
            Request::ShowFlyout,
            Request::GetBrightness {
                id: "DEL-A131-s12345".to_owned(),
            },
            Request::SetBrightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 0,
            },
            Request::SetBrightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 100,
            },
        ] {
            let mut buf = Vec::new();
            write_request(&mut buf, &req).unwrap();
            let got = read_request(&mut Cursor::new(buf)).unwrap();
            assert_eq!(got, req);
        }
    }

    #[test]
    fn every_response_variant_roundtrips() {
        for resp in [
            Response::Displays {
                displays: vec![sample_display()],
            },
            Response::Brightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 42,
            },
            Response::Ok,
            Response::Error {
                code: "unknown_display".to_owned(),
                message: "no such id".to_owned(),
            },
        ] {
            let mut buf = Vec::new();
            write_response(&mut buf, &resp).unwrap();
            let got = read_response(&mut Cursor::new(buf)).unwrap();
            assert_eq!(got, resp);
        }
    }

    #[test]
    fn display_info_projects_a_snapshot() {
        use duja_core::id::StableDisplayId;
        use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot, Feature};

        let snapshot = DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x5B09, Some("312NTAB1C234")).unwrap(),
            name: "LG".to_owned(),
            kind: DisplayKind::InternalPanel,
            // Software-only AND internal at once: the projection must carry both,
            // never fold the flag back into the kind.
            software_only: true,
            user_level_pct: 60,
            capabilities: Capabilities {
                features: [Feature::Brightness].into_iter().collect(),
                hardware_range: true,
                raw_capabilities: None,
                allowed_inputs: Vec::new(),
            },
        };
        let info = DisplayInfo::from_snapshot(&snapshot);
        assert_eq!(info.id, "GSM-5B09-312NTAB1C234");
        assert_eq!(info.kind, DisplayKindDto::InternalPanel);
        assert!(info.software_only);
        assert_eq!(info.level_pct, 60);
        assert_eq!(info.features, vec![FeatureDto::Brightness]);
    }

    #[test]
    fn request_rejects_unknown_fields() {
        // Extra key inside the variant object must be rejected (strict server).
        let body = br#"{"v":1,"request":{"set_brightness":{"id":"GSM-5B09-x","pct":5,"evil":1}}}"#;
        let frame = framed(body);
        let err = read_request(&mut Cursor::new(frame)).unwrap_err();
        assert!(matches!(err, IpcError::Json(_)), "got {err:?}");
    }

    #[test]
    fn request_rejects_unknown_top_level_key() {
        let body = br#"{"v":1,"request":"list_displays","extra":true}"#;
        let frame = framed(body);
        let err = read_request(&mut Cursor::new(frame)).unwrap_err();
        assert!(matches!(err, IpcError::Json(_)), "got {err:?}");
    }

    #[test]
    fn response_ignores_unknown_fields() {
        // A newer server adds a field; an older client must still decode.
        let body = br#"{"v":1,"response":{"kind":"ok"},"trailer":"future"}"#;
        let frame = framed(body);
        let got = read_response(&mut Cursor::new(frame)).unwrap();
        assert_eq!(got, Response::Ok);
    }

    #[test]
    fn wrong_version_is_typed_error() {
        let body = br#"{"v":99,"request":"list_displays"}"#;
        let frame = framed(body);
        let err = read_request(&mut Cursor::new(frame)).unwrap_err();
        assert!(
            matches!(
                err,
                IpcError::UnsupportedVersion {
                    found: 99,
                    expected: 1
                }
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_out_of_range_pct() {
        // pct=200 is a valid u8 and valid JSON, but validation must reject it.
        let body = br#"{"v":1,"request":{"set_brightness":{"id":"GSM-5B09-x","pct":200}}}"#;
        let frame = framed(body);
        let err = read_request(&mut Cursor::new(frame)).unwrap_err();
        assert!(
            matches!(err, IpcError::InvalidField { field: "pct", .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn decode_rejects_bad_id_charset() {
        let body = br#"{"v":1,"request":{"get_brightness":{"id":"bad id!"}}}"#;
        let frame = framed(body);
        let err = read_request(&mut Cursor::new(frame)).unwrap_err();
        assert!(
            matches!(err, IpcError::InvalidField { field: "id", .. }),
            "got {err:?}"
        );
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;
    use std::io::Cursor;

    /// A generator for ids in the exact charset `StableDisplayId` emits.
    fn valid_id() -> impl Strategy<Value = String> {
        proptest::collection::vec(
            proptest::sample::select(
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789#-".to_vec(),
            ),
            1..=ID_MAX_LEN,
        )
        .prop_map(|bytes| String::from_utf8(bytes).unwrap())
    }

    proptest! {
        /// Any valid id + percentage round-trips through the request codec.
        #[test]
        fn request_roundtrip(id in valid_id(), pct in 0u8..=100) {
            let req = Request::SetBrightness { id, pct };
            let mut buf = Vec::new();
            write_request(&mut buf, &req).unwrap();
            let got = read_request(&mut Cursor::new(buf)).unwrap();
            prop_assert_eq!(got, req);
        }

        /// Arbitrary bytes fed to the decoders never panic — they return an
        /// `IpcError` (this is the shape of the `fuzz_ipc_frame` target).
        #[test]
        fn arbitrary_bytes_never_panic(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
            let _ = read_request(&mut Cursor::new(bytes.clone()));
            let _ = read_response(&mut Cursor::new(bytes.clone()));
            let _ = read_frame::<serde_json::Value>(&mut Cursor::new(bytes));
        }

        /// A well-formed 4-byte oversized header is always rejected as
        /// FrameTooLarge, regardless of trailing bytes — never a panic or an
        /// allocation of the declared size.
        #[test]
        fn oversized_header_always_rejected(trailer in prop::collection::vec(any::<u8>(), 0..64)) {
            let claimed = u32::try_from(MAX_FRAME_LEN).unwrap().saturating_add(1);
            let mut frame = claimed.to_le_bytes().to_vec();
            frame.extend_from_slice(&trailer);
            let err = read_frame_bytes(&mut Cursor::new(frame)).unwrap_err();
            let is_too_large = matches!(err, IpcError::FrameTooLarge { .. });
            prop_assert!(is_too_large, "expected FrameTooLarge, got {:?}", err);
        }
    }
}
