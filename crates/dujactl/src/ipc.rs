//! The IPC path for `dujactl`: connect to the running app, run a command over
//! the pipe, and render its reply.
//!
//! Every command tries this path first. Connecting is cheap and its *failure*
//! (no server listening) is the signal to fall back to the direct backend — see
//! [`try_connect`]. Once connected, the server's answer is authoritative,
//! including its structured errors, which map onto the same exit codes the
//! direct path uses (`3` unknown display, `4` backend error); a transport fault
//! *after* connecting is the new [`EXIT_SERVER`] code.

use std::time::Duration;

use duja_ipc::{DisplayInfo, DisplayKindDto, FeatureDto, Request, Response};
use duja_platform::ipc::{IpcTransportError, PipeClient};

use crate::cli::{EXIT_BACKEND, EXIT_OK, EXIT_SERVER, EXIT_UNKNOWN_DISPLAY, SetTarget};
use crate::fmt::render_table;

/// How long to wait for the running app before falling back to direct access.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);

/// Try to reach the running app. `None` means no server is up (the caller then
/// uses the direct backend); any connect failure degrades to `None` so a
/// missing/overloaded app never blocks a script.
pub fn try_connect() -> Option<PipeClient> {
    PipeClient::connect(CONNECT_TIMEOUT).ok()
}

/// Whether the running app is reachable over IPC (for `doctor`).
pub fn server_reachable() -> bool {
    try_connect().is_some()
}

/// Print the "served over …" note in verbose mode only.
fn note(verbose: bool, path: &str) {
    if verbose {
        eprintln!("dujactl: served over {path}");
    }
}

/// Map a server `Response::Error` code onto a `dujactl` exit code.
fn error_exit(code: &str, message: &str) -> u8 {
    eprintln!("{message}");
    if code == "unknown_display" {
        EXIT_UNKNOWN_DISPLAY
    } else {
        EXIT_BACKEND
    }
}

/// Report a transport fault (reached the app, but the exchange failed).
fn transport_exit(err: &IpcTransportError) -> u8 {
    eprintln!("dujactl: ipc server error: {err}");
    EXIT_SERVER
}

/// `list` over IPC.
pub fn list(client: &mut PipeClient, verbose: bool) -> u8 {
    match client.request(&Request::ListDisplays) {
        Ok(Response::Displays { displays }) => {
            note(verbose, "ipc");
            if displays.is_empty() {
                println!("no displays found");
            } else {
                println!("{}", render_display_table(&displays));
            }
            EXIT_OK
        }
        Ok(Response::Error { code, message }) => error_exit(&code, &message),
        Ok(other) => unexpected(&other),
        Err(err) => transport_exit(&err),
    }
}

/// `get <id>` over IPC.
pub fn get(client: &mut PipeClient, id: &str, verbose: bool) -> u8 {
    match client.request(&Request::GetBrightness { id: id.to_owned() }) {
        Ok(Response::Brightness { pct, .. }) => {
            note(verbose, "ipc");
            println!("{pct}%");
            EXIT_OK
        }
        Ok(Response::Error { code, message }) => error_exit(&code, &message),
        Ok(other) => unexpected(&other),
        Err(err) => transport_exit(&err),
    }
}

/// `set <id|all> brightness <pct>` over IPC.
///
/// `<id>` sends a single `SetBrightness` on the already-connected `client`. `all`
/// is expanded client-side into one `SetBrightness` per listed display (see
/// [`set_all`]).
pub fn set(client: &mut PipeClient, target: &SetTarget, percent: u8, verbose: bool) -> u8 {
    match target {
        SetTarget::One(id) => {
            note(verbose, "ipc");
            match set_brightness(client, id, percent) {
                Ok(code) | Err(code) => code,
            }
        }
        SetTarget::All => set_all(client, percent, verbose, try_connect),
    }
}

/// `set all` over IPC: list the displays on `client`, then drive one
/// `SetBrightness` per display, each on its own fresh connection.
///
/// The IPC server serves exactly one request per connection, so the connection
/// that answered `ListDisplays` (the passed-in `client`) is spent; every
/// `SetBrightness` reconnects via `connect` — the same one-request-per-connect
/// pattern the app itself uses. `connect` is [`try_connect`] in production and a
/// test seam otherwise.
///
/// A failed mid-loop reconnect means the app went away: it is reported as a
/// server error (never a silent success, and never a fall-back to the direct
/// hardware backend, which could put a second uncoordinated writer on the bus).
fn set_all(
    client: &mut PipeClient,
    percent: u8,
    verbose: bool,
    connect: impl Fn() -> Option<PipeClient>,
) -> u8 {
    let ids: Vec<String> = match client.request(&Request::ListDisplays) {
        Ok(Response::Displays { displays }) => displays.into_iter().map(|d| d.id).collect(),
        Ok(Response::Error { code, message }) => return error_exit(&code, &message),
        Ok(other) => return unexpected(&other),
        Err(err) => return transport_exit(&err),
    };

    if ids.is_empty() {
        note(verbose, "ipc");
        println!("no displays found");
        return EXIT_OK;
    }

    note(verbose, "ipc");
    let mut exit = EXIT_OK;
    for id in ids {
        let Some(mut fresh) = connect() else {
            return server_unreachable();
        };
        match set_brightness(&mut fresh, &id, percent) {
            Ok(EXIT_OK) => {}
            Ok(code) => exit = code,
            Err(code) => return code,
        }
    }
    exit
}

/// Report that the running app became unreachable partway through `set all` (a
/// mid-loop reconnect failed). Mirrors [`transport_exit`]'s wording and code.
fn server_unreachable() -> u8 {
    eprintln!("dujactl: ipc server error: the running app became unreachable");
    EXIT_SERVER
}

/// Issue one `SetBrightness` on `client` and render the reply.
///
/// `Ok` carries the per-display exit contribution (`EXIT_OK` on success, else the
/// server's mapped error code); `Err` carries a transport-failure code that must
/// abort the whole `set` (the single-request path returns it directly).
fn set_brightness(client: &mut PipeClient, id: &str, percent: u8) -> Result<u8, u8> {
    match client.request(&Request::SetBrightness {
        id: id.to_owned(),
        pct: percent,
    }) {
        Ok(Response::Ok) => {
            println!("{id}: set {percent}%");
            Ok(EXIT_OK)
        }
        Ok(Response::Error { code, message }) => Ok(error_exit(&code, &message)),
        Ok(other) => Ok(unexpected(&other)),
        Err(err) => Err(transport_exit(&err)),
    }
}

/// A response of the wrong shape for the request (a protocol desync).
fn unexpected(response: &Response) -> u8 {
    eprintln!("dujactl: unexpected server response: {response:?}");
    EXIT_SERVER
}

/// Render the display list the server returned as the `list` table.
fn render_display_table(displays: &[DisplayInfo]) -> String {
    let rows: Vec<Vec<String>> = displays
        .iter()
        .map(|d| {
            vec![
                d.id.clone(),
                kind_label(d.kind).to_owned(),
                mode_label(d.software_only).to_owned(),
                d.name.clone(),
                format!("{}%", d.level_pct),
                features_label(&d.features),
            ]
        })
        .collect();
    render_table(&["id", "kind", "mode", "name", "level", "features"], &rows)
}

/// A short label for a transported display kind (its physical provenance).
fn kind_label(kind: DisplayKindDto) -> &'static str {
    match kind {
        DisplayKindDto::ExternalDdc => "external",
        DisplayKindDto::InternalPanel => "internal",
    }
}

/// The control-mode indicator: `sw` when the display has no working hardware
/// brightness (dimmed purely in software), else `hw`. Kept a separate column so
/// software-only is never folded into the physical `kind`.
fn mode_label(software_only: bool) -> &'static str {
    if software_only { "sw" } else { "hw" }
}

/// A comma-separated label for transported features (`-` when none).
fn features_label(features: &[FeatureDto]) -> String {
    if features.is_empty() {
        return "-".to_owned();
    }
    features
        .iter()
        .map(|f| match f {
            FeatureDto::Brightness => "brightness",
            FeatureDto::Contrast => "contrast",
            FeatureDto::InputSource => "input",
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(all(test, any(windows, unix)))]
mod tests {
    use super::*;

    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

    use duja_platform::PipeServer;

    /// A unique transport name per test so a live server never collides with the
    /// real running app or a parallel test. Windows: a per-process pipe name;
    /// unix: a per-process socket path under a dedicated temp directory.
    fn unique_name(tag: &str) -> String {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\duja-dujactl-{tag}-{pid}-{n}")
        }
        #[cfg(unix)]
        {
            format!("/tmp/duja-dujactl-{tag}-{pid}-{n}/ctl.sock")
        }
    }

    fn display(id: &str) -> DisplayInfo {
        DisplayInfo {
            id: id.to_owned(),
            name: "Fake".to_owned(),
            kind: DisplayKindDto::ExternalDdc,
            software_only: false,
            level_pct: 50,
            features: vec![FeatureDto::Brightness],
        }
    }

    #[test]
    fn kind_and_mode_labels_read_provenance_and_control_mode() {
        assert_eq!(kind_label(DisplayKindDto::ExternalDdc), "external");
        assert_eq!(kind_label(DisplayKindDto::InternalPanel), "internal");
        assert_eq!(mode_label(false), "hw");
        assert_eq!(mode_label(true), "sw");
    }

    #[test]
    fn render_display_table_surfaces_software_only_as_its_own_column() {
        let mut sw = display("GSM-5B09-a");
        sw.software_only = true;
        let table = render_display_table(&[display("GSM-5B09-b"), sw]);
        // Software-only rides its own `mode` column, never folded into `kind`.
        assert!(table.contains("mode"));
        assert!(
            table
                .lines()
                .any(|l| l.contains("GSM-5B09-a") && l.contains("sw"))
        );
        assert!(
            table
                .lines()
                .any(|l| l.contains("GSM-5B09-b") && l.contains("hw"))
        );
        // The kind column stays pure provenance — never the string "software".
        assert!(table.lines().all(|l| !l.contains("software")));
    }

    /// A live-server handler exposing two displays; each `SetBrightness` for a
    /// known id answers `Ok` and bumps `sets`, so the test can assert every
    /// display was actually driven (not merely that the command returned 0).
    fn two_display_handler(
        sets: Arc<AtomicUsize>,
    ) -> impl Fn(Request) -> Response + Send + Sync + 'static {
        move |req| match req {
            Request::ListDisplays => Response::Displays {
                displays: vec![display("GSM-5B09-a"), display("GSM-5B09-b")],
            },
            Request::SetBrightness { id, pct } => {
                if (id == "GSM-5B09-a" || id == "GSM-5B09-b") && pct <= 100 {
                    sets.fetch_add(1, Ordering::SeqCst);
                    Response::Ok
                } else {
                    Response::Error {
                        code: "unknown_display".to_owned(),
                        message: "no such id".to_owned(),
                    }
                }
            }
            _ => Response::Error {
                code: "unexpected".to_owned(),
                message: "unexpected request".to_owned(),
            },
        }
    }

    /// Regression: `set all` reused ONE `PipeClient` for `ListDisplays` and every
    /// `SetBrightness`, but the IPC server serves exactly one request per
    /// connection — so the first `SetBrightness` landed on a closed connection and
    /// the command failed (`EXIT_SERVER`), changing zero displays. Drive the real
    /// `set_all` against a live one-shot `PipeServer` and assert every display is
    /// set, each on its own fresh connection.
    #[test]
    fn set_all_drives_every_display_across_the_one_shot_server() {
        let name = unique_name("setall");
        let sets = Arc::new(AtomicUsize::new(0));
        let server = PipeServer::serve_named(&name, two_display_handler(sets.clone()))
            .expect("live IPC server");

        let mut client =
            PipeClient::connect_named(&name, Duration::from_secs(2)).expect("connect for list");
        let connect = || PipeClient::connect_named(&name, Duration::from_secs(2)).ok();

        let code = set_all(&mut client, 30, false, connect);

        assert_eq!(
            code, EXIT_OK,
            "set all must drive every display over the one-shot server; got exit {code}"
        );
        assert_eq!(
            sets.load(Ordering::SeqCst),
            2,
            "both displays must receive a SetBrightness on their own connection"
        );

        server.shutdown();
    }
}
