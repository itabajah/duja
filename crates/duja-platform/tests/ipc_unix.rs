//! Loopback end-to-end tests for the unix-domain-socket IPC transport.
//!
//! Each test spins up a real [`PipeServer`] on a unique socket path with a fake
//! handler, drives it with a real [`PipeClient`] (or a raw [`UnixStream`]) over
//! the OS socket, and asserts the security-checklist behaviour (SECURITY.md §IPC
//! / plan §6). These tests are unix-only and run on the ubuntu **and** macos CI
//! lanes; on Windows there is the named-pipe transport instead (see
//! `ipc_pipe.rs`).
#![cfg(unix)]
// RATIONALE: integration tests are a separate crate and use unwrap/expect for
// brevity; they never ship.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_lossless
)]

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use duja_ipc::{Request, Response};
use duja_platform::PipeServer;
use duja_platform::ipc::{MAX_CONNECTIONS, MAX_HANDLER_THREADS, PipeClient};

/// A unique socket path per test so parallel tests never collide. All paths nest
/// under one per-process directory the server creates and `chmod 0700`s.
fn unique_socket(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    PathBuf::from(format!(
        "/tmp/duja-it-{}/{tag}-{n}.sock",
        std::process::id()
    ))
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("utf-8 socket path")
}

/// A fake in-app bridge: answers list/get/set from a fixed table.
fn fake_handler(req: Request) -> Response {
    match req {
        Request::ListDisplays => Response::Displays {
            displays: vec![duja_ipc::DisplayInfo {
                id: "GSM-5B09-abc".to_owned(),
                name: "Fake".to_owned(),
                kind: duja_ipc::DisplayKindDto::ExternalDdc,
                software_only: false,
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
    let path = unique_socket("rt");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let mut c = PipeClient::connect_named(path_str(&path), short()).expect("connect list");
    let resp = c.request(&Request::ListDisplays).expect("list");
    assert!(
        matches!(resp, Response::Displays { ref displays } if displays.len() == 1),
        "got {resp:?}"
    );
    drop(c);

    let mut c = PipeClient::connect_named(path_str(&path), short()).expect("connect get");
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

    let mut c = PipeClient::connect_named(path_str(&path), short()).expect("connect set");
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
fn same_process_client_passes_peer_check() {
    // A client from THIS process necessarily shares our uid, so the server's
    // peer-credential check must let the exchange proceed.
    let path = unique_socket("peer");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");
    let mut c = PipeClient::connect_named(path_str(&path), short()).expect("connect");
    let resp = c.request(&Request::ShowFlyout).expect("show flyout");
    assert_eq!(resp, Response::Ok);
    server.shutdown();
}

#[test]
fn malformed_frame_gets_an_error_response_not_a_disconnect() {
    // A validly-framed but semantically invalid request (pct out of range): the
    // server must answer with a structured error, not just drop us.
    let path = unique_socket("malformed");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");
    let mut c = PipeClient::connect_named(path_str(&path), short()).expect("connect");
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
fn oversized_length_prefix_is_refused_before_allocation() {
    // Write a raw 4-byte length prefix exceeding the 64 KiB cap, then nothing.
    // The server must reject it on the header (never allocating the body) and
    // answer with a `frame_too_large` error.
    let path = unique_socket("oversize");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let mut raw = UnixStream::connect(path_str(&path)).expect("raw connect");
    let claimed: u32 = (duja_ipc::MAX_FRAME_LEN as u32) + 1;
    raw.write_all(&claimed.to_le_bytes()).expect("write prefix");
    raw.write_all(b"{}").expect("write partial body");

    let body = read_frame(&mut raw, Duration::from_secs(6)).expect("server answers an error frame");
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("frame_too_large"), "reply = {text}");

    server.shutdown();
}

#[test]
fn slow_writer_hits_the_read_timeout() {
    // Connect but never send a full frame; the server must drop the connection
    // after its read timeout (~5 s) rather than pinning a handler forever.
    let path = unique_socket("slow");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let mut raw = UnixStream::connect(path_str(&path)).expect("raw connect");
    // Promise 8 bytes, then never send the body.
    raw.write_all(&8u32.to_le_bytes()).expect("write prefix");
    raw.set_read_timeout(Some(Duration::from_secs(9)))
        .expect("set read timeout");

    let start = Instant::now();
    // A read now blocks until the server times out and closes: it returns 0 (EOF)
    // or an error — either way the exchange is over.
    let mut buf = [0u8; 16];
    let _ = raw.read(&mut buf);
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_secs(4),
        "server closed too early ({elapsed:?}); the 5 s read timeout was not honoured"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "server never closed ({elapsed:?}); the read timeout did not fire"
    );
    server.shutdown();
}

#[test]
fn dribbling_writer_cannot_renew_the_read_timeout() {
    // P5 gate finding C1, unix edition: SO_RCVTIMEO renews per syscall, so a naive
    // per-read timeout would let a peer trickling one byte at a time renew the
    // budget forever and pin a handler (the frame never completes). The deadline
    // is armed once per exchange, so the connection must die ~READ_TIMEOUT after
    // the FIRST read regardless of dribbled bytes.
    let path = unique_socket("dribble");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let mut raw = UnixStream::connect(path_str(&path)).expect("raw connect");
    // Promise 64 bytes, then dribble one byte at a time past the budget.
    raw.write_all(&64u32.to_le_bytes()).expect("write prefix");

    let start = Instant::now();
    let mut dribbled = 0usize;
    let mut died_at = None;
    while start.elapsed() < Duration::from_secs(12) {
        // A write to a server-closed socket fails (EPIPE / broken pipe).
        if raw.write_all(b"x").is_err() {
            died_at = Some(start.elapsed());
            break;
        }
        dribbled += 1;
        std::thread::sleep(Duration::from_millis(1500));
    }

    let died_at = died_at.unwrap_or_else(|| {
        panic!(
            "server still accepted writes after {dribbled} dribbled bytes over {:?}: \
             the read deadline is being renewed per syscall (finding C1)",
            start.elapsed()
        )
    });
    assert!(
        died_at < Duration::from_secs(10),
        "connection survived {died_at:?}, well past the 5 s exchange budget"
    );

    // And the server is still healthy: a well-behaved client is served at once.
    let mut good =
        PipeClient::connect_named(path_str(&path), short()).expect("connect after dribble");
    assert_eq!(
        good.request(&Request::ShowFlyout).expect("served"),
        Response::Ok
    );

    server.shutdown();
}

#[test]
fn handler_pool_bounds_concurrency() {
    // Unix has no connection-time `ERROR_PIPE_BUSY` analogue; the connection cap
    // manifests as bounded concurrency. Fire more clients than handlers at a
    // handler that sleeps, and prove (a) all are served correctly and (b) at no
    // point do more than MAX_HANDLER_THREADS run concurrently — a flood cannot
    // grow the server's worker pool.
    let path = unique_socket("cap");
    let concurrent = Arc::new(AtomicUsize::new(0));
    let max_seen = Arc::new(AtomicUsize::new(0));

    let server = {
        let concurrent = concurrent.clone();
        let max_seen = max_seen.clone();
        PipeServer::serve_named(path_str(&path), move |req| {
            let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            max_seen.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(200));
            concurrent.fetch_sub(1, Ordering::SeqCst);
            fake_handler(req)
        })
        .expect("server up")
    };

    let clients = (MAX_CONNECTIONS as usize) + 2;
    let mut handles = Vec::new();
    for _ in 0..clients {
        let path = path.clone();
        handles.push(std::thread::spawn(move || {
            let mut c = PipeClient::connect_named(path_str(&path), Duration::from_secs(5))
                .expect("connect");
            c.request(&Request::ShowFlyout).expect("served")
        }));
    }
    for h in handles {
        assert_eq!(h.join().expect("client thread"), Response::Ok);
    }

    let peak = max_seen.load(Ordering::SeqCst);
    assert!(
        peak <= MAX_HANDLER_THREADS,
        "peak concurrent handlers {peak} exceeded the pool of {MAX_HANDLER_THREADS}"
    );
    assert!(peak >= 1, "no handler ever ran");

    server.shutdown();
}

#[test]
fn silent_readonly_client_cannot_pin_a_handler() {
    // A peer that connects and never writes must not pin a handler past the stop
    // flag: a well-behaved client is still served by a free handler, and shutdown
    // does not hang joining the handler blocked reading from the silent peer.
    let path = unique_socket("silent");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    // The silent client stays open (never writing) for the whole test.
    let silent = UnixStream::connect(path_str(&path)).expect("silent connect");
    std::thread::sleep(Duration::from_millis(250));

    let start = Instant::now();
    let mut c =
        PipeClient::connect_named(path_str(&path), short()).expect("second client connects");
    let resp = c
        .request(&Request::ShowFlyout)
        .expect("second client answered");
    assert_eq!(resp, Response::Ok, "the second client must be served");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "second client took too long ({:?}); a handler looks pinned",
        start.elapsed()
    );
    drop(c);

    // Shutdown must not hang joining the handler blocked reading the silent peer.
    with_watchdog(Duration::from_secs(3), move || server.shutdown());

    // Keep the silent client alive until after shutdown so the handler unblocks
    // via the stop flag, not because the peer disconnected.
    drop(silent);
}

#[test]
fn shutdown_completes_promptly_with_idle_connection() {
    // Connect, send nothing, then immediately shut down: the handler parked in
    // its first read must be cancelled by the stop flag well under 3 s.
    let path = unique_socket("idle");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let idle = UnixStream::connect(path_str(&path)).expect("raw connect");
    std::thread::sleep(Duration::from_millis(250));

    with_watchdog(Duration::from_secs(3), move || server.shutdown());

    drop(idle);
}

#[test]
fn stale_socket_is_taken_over() {
    // A leftover socket inode with no live listener (e.g. after a crash) must not
    // block a fresh server: it detects the stale inode, unlinks it, and rebinds.
    let path = unique_socket("stale");
    std::fs::create_dir_all(path.parent().unwrap()).expect("mk dir");

    // Leave a stale socket file: std does NOT unlink a listener's path on drop.
    {
        let stale = UnixListener::bind(path_str(&path)).expect("bind stale");
        drop(stale);
    }
    assert!(path.exists(), "the stale socket inode should persist");

    // Takeover: bind fails AddrInUse, the connect-probe is refused (no listener),
    // so the server unlinks and rebinds.
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("takeover");
    let mut c =
        PipeClient::connect_named(path_str(&path), short()).expect("connect after takeover");
    assert_eq!(
        c.request(&Request::ShowFlyout).expect("served"),
        Response::Ok
    );
    server.shutdown();
}

#[test]
fn live_server_refuses_a_second_instance() {
    // While a server is live, binding a second one on the same path must fail
    // (the connect-probe succeeds, so it is not treated as stale) — the
    // single-instance answer.
    let path = unique_socket("second");
    let first = PipeServer::serve_named(path_str(&path), fake_handler).expect("first server");

    let second = PipeServer::serve_named(path_str(&path), fake_handler);
    assert!(
        second.is_err(),
        "a second server on a live socket must be refused"
    );

    first.shutdown();
}

#[test]
fn socket_and_dir_have_owner_only_permissions() {
    // The socket is chmod 0600 and its parent directory 0700 (the real barrier).
    let path = unique_socket("perms");
    let server = PipeServer::serve_named(path_str(&path), fake_handler).expect("server up");

    let sock_mode = std::fs::metadata(&path)
        .expect("stat socket")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(sock_mode, 0o600, "socket must be user-only 0600");

    let dir_mode = std::fs::metadata(path.parent().unwrap())
        .expect("stat dir")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(dir_mode, 0o700, "socket dir must be user-only 0700");

    server.shutdown();
}

#[test]
fn connect_to_absent_socket_reports_not_running() {
    // No server: the client must report NotRunning so dujactl falls back to the
    // direct backend rather than hanging.
    let path = unique_socket("absent");
    let err = PipeClient::connect_named(path_str(&path), Duration::from_millis(300))
        .err()
        .expect("no server should refuse the connect");
    assert!(
        matches!(err, duja_platform::ipc::IpcTransportError::NotRunning),
        "got {err:?}"
    );
}

// --- helpers -------------------------------------------------------------

/// Read one length-prefixed frame body, or `None` on timeout/EOF/oversize.
fn read_frame(stream: &mut UnixStream, budget: Duration) -> Option<Vec<u8>> {
    stream.set_read_timeout(Some(budget)).ok()?;
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).ok()?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len == 0 || len > duja_ipc::MAX_FRAME_LEN {
        return None;
    }
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;
    Some(body)
}

/// Run `body` on a worker thread and fail the test if it does not finish within
/// `budget` — so a re-introduced hang panics promptly instead of stalling CI.
fn with_watchdog<F: FnOnce() + Send + 'static>(budget: Duration, body: F) {
    let done = Arc::new(AtomicBool::new(false));
    let flag = done.clone();
    let worker = std::thread::spawn(move || {
        body();
        flag.store(true, Ordering::SeqCst);
    });
    let start = Instant::now();
    while start.elapsed() < budget {
        if done.load(Ordering::SeqCst) {
            worker.join().expect("watchdogged body panicked");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("watchdog: operation did not complete within {budget:?}");
}
