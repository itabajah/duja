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

/// `set <id|all> brightness <pct>` over IPC. `all` is expanded client-side into
/// one `SetBrightness` per listed display.
pub fn set(client: &mut PipeClient, target: &SetTarget, percent: u8, verbose: bool) -> u8 {
    let ids = match target {
        SetTarget::One(id) => vec![id.clone()],
        SetTarget::All => match client.request(&Request::ListDisplays) {
            Ok(Response::Displays { displays }) => {
                displays.into_iter().map(|d| d.id).collect::<Vec<_>>()
            }
            Ok(Response::Error { code, message }) => return error_exit(&code, &message),
            Ok(other) => return unexpected(&other),
            Err(err) => return transport_exit(&err),
        },
    };

    if ids.is_empty() {
        note(verbose, "ipc");
        println!("no displays found");
        return EXIT_OK;
    }

    note(verbose, "ipc");
    let mut exit = EXIT_OK;
    for id in ids {
        match client.request(&Request::SetBrightness {
            id: id.clone(),
            pct: percent,
        }) {
            Ok(Response::Ok) => println!("{id}: set {percent}%"),
            Ok(Response::Error { code, message }) => {
                exit = error_exit(&code, &message);
            }
            Ok(other) => {
                exit = unexpected(&other);
            }
            Err(err) => return transport_exit(&err),
        }
    }
    exit
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
                d.name.clone(),
                format!("{}%", d.level_pct),
                features_label(&d.features),
            ]
        })
        .collect();
    render_table(&["id", "kind", "name", "level", "features"], &rows)
}

/// A short label for a transported display kind.
fn kind_label(kind: DisplayKindDto) -> &'static str {
    match kind {
        DisplayKindDto::ExternalDdc => "ddc",
        DisplayKindDto::InternalPanel => "panel",
        DisplayKindDto::SoftwareOnly => "software",
    }
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
