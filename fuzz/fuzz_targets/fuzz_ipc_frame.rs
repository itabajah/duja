#![no_main]

//! Fuzz the IPC frame decoders **and the full server-side request path**:
//! arbitrary bytes go into `read_request` / `read_response` / the raw
//! `read_frame_bytes`, and — mirroring exactly what the named-pipe server does
//! per connection — through `serve_once`, which must decode, validate, and turn
//! any hostile input into either a handled `Response::Error` or a clean
//! transport error, never a panic and never an allocation past the 64 KiB cap.

use std::io::{Cursor, Read, Write};

use duja_ipc::{Request, Response, read_frame_bytes, read_request, read_response, serve_once};
use libfuzzer_sys::fuzz_target;

/// A one-shot in-memory duplex: reads drain `input`, writes are discarded.
struct Duplex {
    input: Cursor<Vec<u8>>,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.input.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fuzz_target!(|data: &[u8]| {
    let _ = read_request(&mut Cursor::new(data));
    let _ = read_response(&mut Cursor::new(data));
    let _ = read_frame_bytes(&mut Cursor::new(data));

    // The exact per-connection server path: decode → validate → handle/refuse.
    let mut duplex = Duplex {
        input: Cursor::new(data.to_vec()),
    };
    let _ = serve_once(&mut duplex, |req: Request| match req {
        Request::ListDisplays => Response::Displays {
            displays: Vec::new(),
        },
        Request::GetBrightness { id } => Response::Brightness { id, pct: 0 },
        Request::SetBrightness { id, pct } => Response::Brightness { id, pct },
        Request::ShowFlyout => Response::Ok,
    });
});
