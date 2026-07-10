//! The request→response *exchange* helpers that sit on top of the codec.
//!
//! These are pure and transport-agnostic: they operate on any `Read + Write`
//! stream, so the OS-specific transport (a Windows named pipe in
//! `duja-platform`, a unix socket later) only has to supply a byte stream. The
//! server side ([`serve_once`]) reads one request, invokes a handler, and writes
//! one response; the client side ([`exchange`]) writes one request and reads one
//! response.
//!
//! # Structured refusals
//!
//! A hostile or buggy client must get a *structured* answer, not a silent
//! disconnect: [`serve_once`] maps a decode/validation failure to a
//! [`Response::Error`] (via [`error_response`]) and writes it back before
//! returning. Only an unrecoverable stream fault (`UnexpectedEof`/`Io`) — where
//! there is no live pipe to answer on — is surfaced as an [`IpcError`] with
//! nothing written.

use std::io::{Read, Write};

use crate::frame::IpcError;
use crate::protocol::{Request, Response};
use crate::{read_request, read_response, write_request, write_response};

/// Map a decode/framing failure to the [`Response::Error`] a server should send
/// back, or `None` when the stream is too broken to answer on.
///
/// Protocol violations (`FrameTooLarge`, `UnsupportedVersion`, `InvalidField`,
/// malformed JSON) each get a stable machine `code` so a client can react
/// programmatically. Transport faults (`UnexpectedEof`, `Io`) return `None`:
/// the peer is gone, so writing a reply is pointless.
#[must_use]
pub fn error_response(err: &IpcError) -> Option<Response> {
    let code = match err {
        IpcError::FrameTooLarge { .. } => "frame_too_large",
        IpcError::UnsupportedVersion { .. } => "unsupported_version",
        IpcError::InvalidField { .. } => "invalid_field",
        IpcError::Json(_) => "bad_request",
        IpcError::UnexpectedEof | IpcError::Io(_) => return None,
    };
    Some(Response::Error {
        code: code.to_owned(),
        message: err.to_string(),
    })
}

/// Serve exactly one request→response exchange over `stream`.
///
/// Reads and validates one [`Request`], runs `handler`, and writes its
/// [`Response`]. A decode/validation failure is answered with the structured
/// [`error_response`] instead of the handler being called; the exchange then
/// completes normally (`Ok(())`).
///
/// # Errors
/// - The underlying [`IpcError`] on an unrecoverable read fault
///   (`UnexpectedEof`/`Io`), where no error frame could be written.
/// - Any [`IpcError`] from writing the response frame (a broken pipe mid-write).
pub fn serve_once<S, H>(stream: &mut S, handler: H) -> Result<(), IpcError>
where
    S: Read + Write,
    H: FnOnce(Request) -> Response,
{
    match read_request(stream) {
        Ok(request) => {
            let response = handler(request);
            write_response(stream, &response)
        }
        Err(err) => match error_response(&err) {
            Some(response) => write_response(stream, &response),
            None => Err(err),
        },
    }
}

/// Perform one client-side exchange: write `request`, read the [`Response`].
///
/// # Errors
/// Any [`IpcError`] from framing the request or decoding/validating the reply.
pub fn exchange<S: Read + Write>(stream: &mut S, request: &Request) -> Result<Response, IpcError> {
    write_request(stream, request)?;
    read_response(stream)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A one-shot in-memory duplex: reads drain `input`, writes accumulate in
    /// `output`. Sufficient for the read-then-write shape of both helpers.
    struct Duplex {
        input: Cursor<Vec<u8>>,
        output: Vec<u8>,
    }

    impl Duplex {
        fn new(input: Vec<u8>) -> Self {
            Duplex {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.output.flush()
        }
    }

    /// Frame a request into raw bytes an exchange partner would send.
    fn framed_request(req: &Request) -> Vec<u8> {
        let mut buf = Vec::new();
        write_request(&mut buf, req).unwrap();
        buf
    }

    #[test]
    fn serve_once_maps_a_request_through_the_handler() {
        let req = Request::SetBrightness {
            id: "GSM-5B09-x".to_owned(),
            pct: 42,
        };
        let mut duplex = Duplex::new(framed_request(&req));
        serve_once(&mut duplex, |got| {
            assert_eq!(got, req);
            Response::Ok
        })
        .unwrap();
        let echoed = read_response(&mut Cursor::new(duplex.output)).unwrap();
        assert_eq!(echoed, Response::Ok);
    }

    #[test]
    fn serve_once_answers_a_malformed_frame_with_an_error_response() {
        // A validly-framed but out-of-range request: decode succeeds, validation
        // rejects it, and the server must answer with a structured error rather
        // than dropping the connection.
        let body = br#"{"v":1,"request":{"set_brightness":{"id":"GSM-5B09-x","pct":200}}}"#;
        let mut frame = u32::try_from(body.len()).unwrap().to_le_bytes().to_vec();
        frame.extend_from_slice(body);

        let mut duplex = Duplex::new(frame);
        // The handler must NOT run for an invalid request.
        serve_once(&mut duplex, |_| panic!("handler ran on an invalid request")).unwrap();

        let reply = read_response(&mut Cursor::new(duplex.output)).unwrap();
        assert!(
            matches!(reply, Response::Error { ref code, .. } if code == "invalid_field"),
            "got {reply:?}"
        );
    }

    #[test]
    fn serve_once_surfaces_a_truncated_stream_as_an_error() {
        // Only two of the four length-prefix bytes: an UnexpectedEof that cannot
        // be answered on, so it propagates.
        let mut duplex = Duplex::new(vec![0x01, 0x02]);
        let err = serve_once(&mut duplex, |_| Response::Ok).unwrap_err();
        assert!(matches!(err, IpcError::UnexpectedEof), "got {err:?}");
        assert!(duplex.output.is_empty(), "nothing should be written");
    }

    #[test]
    fn exchange_round_trips_request_and_response() {
        // Seed the reply the "server" would send, then check exchange writes the
        // request and returns the decoded response.
        let mut reply_bytes = Vec::new();
        write_response(
            &mut reply_bytes,
            &Response::Brightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 50,
            },
        )
        .unwrap();

        let mut duplex = Duplex::new(reply_bytes);
        let req = Request::GetBrightness {
            id: "GSM-5B09-x".to_owned(),
        };
        let resp = exchange(&mut duplex, &req).unwrap();
        assert_eq!(
            resp,
            Response::Brightness {
                id: "GSM-5B09-x".to_owned(),
                pct: 50
            }
        );
        // The request the server would have read is in `output`.
        let sent = read_request(&mut Cursor::new(duplex.output)).unwrap();
        assert_eq!(sent, req);
    }

    #[test]
    fn error_response_none_for_transport_faults() {
        assert!(error_response(&IpcError::UnexpectedEof).is_none());
        assert!(
            error_response(&IpcError::FrameTooLarge { len: 1 << 20 }).is_some_and(
                |r| matches!(r, Response::Error { code, .. } if code == "frame_too_large")
            )
        );
    }
}
