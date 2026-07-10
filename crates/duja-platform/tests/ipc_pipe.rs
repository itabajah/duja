//! Loopback end-to-end tests for the Windows named-pipe IPC transport.
//!
//! Each test spins up a real [`PipeServer`] on a unique pipe name with a fake
//! handler, drives it with a real [`PipeClient`] over the OS pipe, and asserts
//! the security-checklist behaviour (SECURITY.md §IPC / plan §6). The tests are
//! Windows-only; on other targets there is no pipe transport yet.
#![cfg(windows)]
// RATIONALE: integration tests are a separate crate and use unwrap/expect for
// brevity; they never ship. The security-inspection helpers below drive raw
// Win32 FFI whose idiomatic call shapes trip the confined-unsafe lints the
// library proper documents per-block; in a throwaway test we relax them wholesale.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::undocumented_unsafe_blocks,
    clippy::borrow_as_ptr,
    clippy::cast_possible_truncation,
    clippy::cast_lossless,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::unnested_or_patterns
)]

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use duja_ipc::{Request, Response};
use duja_platform::PipeServer;
use duja_platform::ipc::{IpcTransportError, PipeClient};

/// A unique pipe name per test so parallel tests never collide.
fn unique_name(tag: &str) -> String {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!(r"\\.\pipe\duja-test-{}-{tag}-{n}", std::process::id())
}

/// A fake in-app bridge: answers list/get/set from a fixed table.
fn fake_handler(req: Request) -> Response {
    match req {
        Request::ListDisplays => Response::Displays {
            displays: vec![duja_ipc::DisplayInfo {
                id: "GSM-5B09-abc".to_owned(),
                name: "Fake".to_owned(),
                kind: duja_ipc::DisplayKindDto::ExternalDdc,
                level_pct: 55,
                features: vec![duja_ipc::FeatureDto::Brightness],
            }],
        },
        Request::GetBrightness { id } => {
            if id == "GSM-5B09-abc" {
                Response::Brightness { id, pct: 55 }
            } else {
                Response::Error {
                    code: "unknown_display".to_owned(),
                    message: "no such id".to_owned(),
                }
            }
        }
        Request::SetBrightness { id, pct } => {
            if id == "GSM-5B09-abc" && pct <= 100 {
                Response::Ok
            } else {
                Response::Error {
                    code: "unknown_display".to_owned(),
                    message: "no such id".to_owned(),
                }
            }
        }
        Request::ShowFlyout => Response::Ok,
    }
}

fn short() -> Duration {
    Duration::from_secs(2)
}

#[test]
fn list_get_set_round_trip() {
    let name = unique_name("rt");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");

    // list
    let mut c = PipeClient::connect_named(&name, short()).expect("connect list");
    let resp = c.request(&Request::ListDisplays).expect("list");
    assert!(
        matches!(resp, Response::Displays { ref displays } if displays.len() == 1),
        "got {resp:?}"
    );
    drop(c);

    // get
    let mut c = PipeClient::connect_named(&name, short()).expect("connect get");
    let resp = c
        .request(&Request::GetBrightness {
            id: "GSM-5B09-abc".to_owned(),
        })
        .expect("get");
    assert_eq!(
        resp,
        Response::Brightness {
            id: "GSM-5B09-abc".to_owned(),
            pct: 55
        }
    );
    drop(c);

    // set
    let mut c = PipeClient::connect_named(&name, short()).expect("connect set");
    let resp = c
        .request(&Request::SetBrightness {
            id: "GSM-5B09-abc".to_owned(),
            pct: 30,
        })
        .expect("set");
    assert_eq!(resp, Response::Ok);
    drop(c);

    server.shutdown();
}

#[test]
fn same_process_client_passes_pid_session_check() {
    // A client from THIS process is necessarily in the same session, so the
    // server's PID/session verification must let the exchange proceed.
    let name = unique_name("session");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");
    let mut c = PipeClient::connect_named(&name, short()).expect("connect");
    let resp = c.request(&Request::ShowFlyout).expect("show flyout");
    assert_eq!(resp, Response::Ok);
    server.shutdown();
}

#[test]
fn malformed_frame_gets_an_error_response_not_a_disconnect() {
    // Send a validly-framed but semantically invalid request (pct out of range):
    // the server must answer with a structured error, not just drop us.
    let name = unique_name("malformed");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");
    let mut c = PipeClient::connect_named(&name, short()).expect("connect");
    // pct=200 is a valid u8 but fails validation on the server.
    let resp = c
        .request(&Request::SetBrightness {
            id: "GSM-5B09-abc".to_owned(),
            pct: 200,
        })
        .expect("still get a response frame");
    assert!(
        matches!(resp, Response::Error { ref code, .. } if code == "invalid_field"),
        "got {resp:?}"
    );
    server.shutdown();
}

#[test]
fn oversized_length_prefix_is_refused() {
    // Write a raw 4-byte length prefix that exceeds the 64 KiB cap, then nothing.
    // The server must reject it on the header (never allocating the body) and
    // answer with a `frame_too_large` error.
    use std::io::{Read, Write};

    let name = unique_name("oversize");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");

    // Connect with the raw platform client and hand-write a hostile prefix.
    let mut raw = RawPipe::connect(&name, short()).expect("raw connect");
    let claimed: u32 = (duja_ipc::MAX_FRAME_LEN as u32) + 1;
    raw.write_all(&claimed.to_le_bytes()).expect("write prefix");
    raw.write_all(b"{}").expect("write partial body");

    // The server answers with an error frame (or, if it decides to drop, the
    // read returns EOF). We assert we can at least read a length-prefixed error.
    let mut len_buf = [0u8; 4];
    let read = read_exact_timeout(&mut raw, &mut len_buf, Duration::from_secs(6));
    assert!(
        read,
        "server should answer the oversized prefix with an error frame"
    );
    let len = u32::from_le_bytes(len_buf) as usize;
    assert!(
        len > 0 && len <= duja_ipc::MAX_FRAME_LEN,
        "reply len = {len}"
    );
    let mut body = vec![0u8; len];
    assert!(raw.read_exact(&mut body).is_ok(), "read error body");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("frame_too_large"), "reply = {text}");

    server.shutdown();
}

#[test]
fn slow_writer_hits_the_read_timeout() {
    // Connect but never send a full frame; the server must drop the connection
    // after its read timeout (5 s) rather than pinning a handler forever.
    use std::io::{Read, Write};

    let name = unique_name("slow");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");

    let mut raw = RawPipe::connect(&name, short()).expect("raw connect");
    // Send a length prefix promising 8 bytes, then never send the body.
    raw.write_all(&8u32.to_le_bytes()).expect("write prefix");

    let start = Instant::now();
    // A blocking read now waits until the server times out and closes: it then
    // returns 0 (EOF), some bytes (an error frame), or an error (broken pipe) —
    // in every case the exchange is over, which is all we assert.
    let mut buf = [0u8; 16];
    let _ = raw.read(&mut buf);
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4),
        "server closed too early ({elapsed:?}); the 5 s read timeout was not honoured"
    );
    server.shutdown();
}

#[test]
fn fifth_concurrent_connection_is_refused() {
    // A handler that blocks until released, so connections stay open and occupy
    // instances. Four should connect; the fifth must be refused (ERROR_PIPE_BUSY
    // → Busy) within a short window.
    let name = unique_name("cap");

    // The handler sleeps so the connection (and its instance) stays occupied for
    // the duration of the test window.
    let server = PipeServer::serve_named(&name, |req| {
        std::thread::sleep(Duration::from_secs(3));
        fake_handler(req)
    })
    .expect("server up");

    // Open four raw connections and immediately send a request each (occupying a
    // handler / instance). Keep them alive in a vec.
    let mut held = Vec::new();
    for _ in 0..duja_platform::ipc::MAX_CONNECTIONS {
        match RawPipe::connect(&name, Duration::from_secs(2)) {
            Ok(mut p) => {
                use std::io::Write;
                // Send a full ShowFlyout frame so the handler starts (and sleeps).
                let mut buf = Vec::new();
                duja_ipc::write_request(&mut buf, &Request::ShowFlyout).unwrap();
                let _ = p.write_all(&buf);
                held.push(p);
            }
            Err(e) => panic!(
                "expected the first {} to connect, got {e:?}",
                duja_platform::ipc::MAX_CONNECTIONS
            ),
        }
    }

    // Give the listener a moment to have all instances occupied.
    std::thread::sleep(Duration::from_millis(300));

    // The 5th connect must be refused quickly (Busy or Timeout), not accepted.
    let fifth = PipeClient::connect_named(&name, Duration::from_millis(500));
    assert!(
        matches!(
            fifth,
            Err(IpcTransportError::Busy) | Err(IpcTransportError::Timeout)
        ),
        "the 5th concurrent connection should be refused, got {:?}",
        fifth.map(|_| "connected")
    );

    drop(held);
    server.shutdown();
}

#[test]
fn dacl_grants_owner_only_no_everyone() {
    // Open the pipe as a client (same user → allowed by the DACL) and inspect
    // its security: the owner must be the current user and the DACL must carry
    // no `Everyone` (S-1-1-0) ACE.
    let name = unique_name("dacl");
    let server = PipeServer::serve_named(&name, fake_handler).expect("server up");

    let sids = query_pipe_security(&name).expect("read pipe security");
    // Owner must be the current user SID.
    let me = current_user_sid_string().expect("current sid");
    assert_eq!(sids.owner, me, "pipe owner should be the current user");
    assert!(
        !sids.dacl_sids.contains(&"S-1-1-0".to_owned()),
        "DACL must not contain an Everyone ACE; got {:?}",
        sids.dacl_sids
    );
    assert!(
        sids.dacl_sids.contains(&me),
        "DACL should grant the current user; got {:?}",
        sids.dacl_sids
    );

    server.shutdown();
}

// --- raw pipe + security helpers (Win32) ---------------------------------

use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, HLOCAL,
    LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetSecurityInfo, SE_KERNEL_OBJECT,
};
use windows::Win32::Security::{ACL, DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSID};
use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::core::{PCWSTR, PWSTR};

struct RawPipe {
    handle: HANDLE,
}

impl RawPipe {
    fn connect(name: &str, timeout: Duration) -> Result<Self, String> {
        let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let deadline = Instant::now() + timeout;
        loop {
            let opened = unsafe {
                CreateFileW(
                    PCWSTR(wide.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match opened {
                Ok(handle) if !handle.is_invalid() => return Ok(RawPipe { handle }),
                _ => {
                    let code = unsafe { GetLastError() };
                    if code != ERROR_PIPE_BUSY {
                        return Err(format!("win32 error {}", code.0));
                    }
                    if Instant::now() >= deadline {
                        return Err("busy".to_owned());
                    }
                    let _ = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 200) };
                }
            }
        }
    }
}

impl std::io::Read for RawPipe {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut read_n = 0u32;
        unsafe { ReadFile(self.handle, Some(buf), Some(&mut read_n), None) }
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(read_n as usize)
    }
}

impl std::io::Write for RawPipe {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0u32;
        unsafe { WriteFile(self.handle, Some(buf), Some(&mut written), None) }
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(written as usize)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for RawPipe {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Read exactly `buf.len()` bytes, or give up after `budget`. Returns whether it
/// filled the buffer.
fn read_exact_timeout(pipe: &mut RawPipe, buf: &mut [u8], budget: Duration) -> bool {
    use std::io::Read;
    let start = Instant::now();
    let mut filled = 0;
    while filled < buf.len() {
        if start.elapsed() > budget {
            return false;
        }
        match pipe.read(&mut buf[filled..]) {
            Ok(n) if n > 0 => filled += n,
            // EOF or error: the server answered nothing more.
            _ => return false,
        }
    }
    true
}

struct PipeSids {
    owner: String,
    dacl_sids: Vec<String>,
}

/// Open the pipe read-control and read its owner + DACL SIDs as strings.
fn query_pipe_security(name: &str) -> Option<PipeSids> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_READ.0,
            FILE_SHARE_MODE(0),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(0),
            None,
        )
    }
    .ok()?;

    let mut owner_sid = PSID::default();
    let mut dacl: *mut ACL = std::ptr::null_mut();
    let mut psd = windows::Win32::Security::PSECURITY_DESCRIPTOR::default();
    let rc = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner_sid),
            None,
            Some(&mut dacl),
            None,
            Some(&mut psd),
        )
    };
    let result = if rc.is_ok() {
        let owner = unsafe { sid_to_string(owner_sid) };
        let dacl_sids = unsafe { dacl_sid_strings(dacl) };
        owner.map(|owner| PipeSids { owner, dacl_sids })
    } else {
        None
    };
    unsafe {
        if !psd.0.is_null() {
            let _ = LocalFree(Some(HLOCAL(psd.0)));
        }
        let _ = CloseHandle(handle);
    }
    result
}

/// Walk a DACL's ACEs and return each trustee SID as a string.
unsafe fn dacl_sid_strings(dacl: *mut ACL) -> Vec<String> {
    use windows::Win32::Security::ACCESS_ALLOWED_ACE;
    let mut out = Vec::new();
    if dacl.is_null() {
        return out;
    }
    let count = unsafe { (*dacl).AceCount };
    for i in 0..count as u32 {
        let mut ace_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
        if unsafe { windows::Win32::Security::GetAce(dacl, i, &mut ace_ptr) }.is_err() {
            continue;
        }
        // Allow/deny ACEs share the ACCESS_ALLOWED_ACE layout, with the trustee
        // SID beginning at the SidStart field.
        let ace = ace_ptr.cast::<ACCESS_ALLOWED_ACE>();
        let sid = PSID(unsafe { std::ptr::addr_of_mut!((*ace).SidStart) }.cast());
        if let Some(s) = unsafe { sid_to_string(sid) } {
            out.push(s);
        }
    }
    out
}

unsafe fn sid_to_string(sid: PSID) -> Option<String> {
    if sid.is_invalid() {
        return None;
    }
    let mut raw = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut raw) }.ok()?;
    if raw.is_null() {
        return None;
    }
    let s = unsafe { raw.to_string() }.ok();
    unsafe {
        let _ = LocalFree(Some(HLOCAL(raw.0.cast())));
    }
    s
}

fn current_user_sid_string() -> Option<String> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).ok()?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        if len == 0 {
            let _ = CloseHandle(token);
            return None;
        }
        let words = (len as usize).div_ceil(8);
        let mut buf = vec![0u64; words];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !ok {
            return None;
        }
        let sid: PSID = (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid;
        sid_to_string(sid)
    }
}
