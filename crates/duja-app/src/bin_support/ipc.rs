//! The app side of the local IPC server: the narrow [`IpcBridge`] the transport
//! calls, the request→response mapping, and the concrete tray/headless bridges.
//!
//! # Where the mapping lives
//!
//! [`handle_request`] is a pure function of an [`IpcBridge`], so it is unit
//! tested with a fake bridge and never needs a real pipe. The OS transport
//! ([`duja_platform::ipc`]) owns threads and `unsafe`; this module owns only the
//! translation from a [`Request`] to an [`EngineCommand`] / UI action.
//!
//! # Consistency choice (plan §6)
//!
//! `list`/`get` are answered from a fresh engine [`Snapshot`](EngineCommand::Snapshot),
//! read straight off the engine thread — no main-thread hop. `set` is different:
//! to keep the persisted user level and the overlay/gamma batch consistent with
//! the flyout, the tray bridge routes it through the **same** main-thread
//! `set_user_level` path a slider drag uses (via [`slint::invoke_from_event_loop`]).
//! The headless bridge, which owns no overlay/state, forwards `set` straight to
//! the engine.

use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Sender, bounded};
use tracing::{debug, info, warn};

use duja_app::EngineCommand;
use duja_core::model::DisplaySnapshot;
use duja_ipc::{DisplayInfo, Request, Response};
use duja_platform::PipeServer;
use duja_platform::ipc::PipeClient;

/// How long the IPC handler waits for the engine to answer a snapshot request
/// before giving up (and returning an empty list).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a second instance waits to reach the running server before giving
/// up on the show-flyout handshake.
const SECOND_INSTANCE_TIMEOUT: Duration = Duration::from_millis(500);

/// The narrow app-facing capability set the IPC request handler needs.
///
/// Kept deliberately small: the transport only ever asks for a snapshot, a level
/// change, or a flyout nudge. Implementors decide how each maps onto the running
/// app (engine actor for headless; main-thread UI path for the tray).
pub(crate) trait IpcBridge: Send + Sync + 'static {
    /// The current UI-facing display snapshots.
    fn snapshot(&self) -> Vec<DisplaySnapshot>;
    /// Apply a user level to the display with id string `id`. Returns `false`
    /// when no such display is currently known.
    fn set_level(&self, id: &str, pct: u8) -> bool;
    /// Surface the app's flyout (a no-op where there is no UI).
    fn show_flyout(&self);
}

/// Map one [`Request`] onto `bridge`, producing the [`Response`] to send back.
///
/// Pure with respect to `bridge`, so it is exhaustively unit-testable.
pub(crate) fn handle_request(bridge: &dyn IpcBridge, request: Request) -> Response {
    match request {
        Request::ListDisplays => Response::Displays {
            displays: bridge
                .snapshot()
                .iter()
                .map(DisplayInfo::from_snapshot)
                .collect(),
        },
        Request::GetBrightness { id } => bridge
            .snapshot()
            .iter()
            .find(|snap| snap.id.as_str() == id)
            .map_or_else(
                || unknown_display(&id),
                |snap| Response::Brightness {
                    id: id.clone(),
                    pct: snap.user_level_pct,
                },
            ),
        Request::SetBrightness { id, pct } => {
            if bridge.set_level(&id, pct) {
                Response::Ok
            } else {
                unknown_display(&id)
            }
        }
        Request::ShowFlyout => {
            bridge.show_flyout();
            Response::Ok
        }
    }
}

/// The stable error for a request naming a display the app does not know.
fn unknown_display(id: &str) -> Response {
    Response::Error {
        code: "unknown_display".to_owned(),
        message: format!("no display with id `{id}`"),
    }
}

/// Ask the engine for a fresh snapshot, tolerating a slow/absent engine.
fn engine_snapshot(engine_tx: &Sender<EngineCommand>) -> Vec<DisplaySnapshot> {
    let (reply_tx, reply_rx) = bounded(1);
    if engine_tx
        .send(EngineCommand::Snapshot { reply: reply_tx })
        .is_err()
    {
        return Vec::new();
    }
    reply_rx.recv_timeout(SNAPSHOT_TIMEOUT).unwrap_or_default()
}

/// The headless bridge: everything goes straight to the engine actor; there is
/// no UI to surface and no overlay/state book to keep consistent.
pub(crate) struct HeadlessBridge {
    engine_tx: Sender<EngineCommand>,
}

impl HeadlessBridge {
    pub(crate) fn new(engine_tx: Sender<EngineCommand>) -> Self {
        HeadlessBridge { engine_tx }
    }
}

impl IpcBridge for HeadlessBridge {
    fn snapshot(&self) -> Vec<DisplaySnapshot> {
        engine_snapshot(&self.engine_tx)
    }

    fn set_level(&self, id: &str, pct: u8) -> bool {
        let Some(target) = self
            .snapshot()
            .into_iter()
            .find(|snap| snap.id.as_str() == id)
        else {
            return false;
        };
        self.engine_tx
            .send(EngineCommand::SetUserLevel { id: target.id, pct })
            .is_ok()
    }

    fn show_flyout(&self) {
        // No UI in headless mode; ShowFlyout is a documented no-op.
    }
}

/// Start the IPC server for `bridge`, returning the handle to keep alive (or
/// `None` when the transport is unavailable — the app still runs).
pub(crate) fn start(bridge: Arc<dyn IpcBridge>) -> Option<PipeServer> {
    match PipeServer::serve(move |request| handle_request(bridge.as_ref(), request)) {
        Ok(server) => {
            info!(
                pipe = %duja_platform::ipc::default_pipe_name(),
                "ipc server listening"
            );
            Some(server)
        }
        Err(err) => {
            warn!(error = %err, "ipc server unavailable; control API disabled");
            None
        }
    }
}

/// Best-effort: connect to the already-running instance and ask it to show its
/// flyout. Returns whether the handshake succeeded.
// RATIONALE: only the (Windows-only) tray second-instance path calls this; the
// non-Windows build has no tray, so keep that lane dead-code clean.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn show_running_instance() -> bool {
    match PipeClient::connect(SECOND_INSTANCE_TIMEOUT) {
        Ok(mut client) => match client.request(&Request::ShowFlyout) {
            Ok(_) => true,
            Err(err) => {
                warn!(error = %err, "could not ask the running instance to show its flyout");
                false
            }
        },
        Err(err) => {
            debug!(error = %err, "no running instance reachable over ipc");
            false
        }
    }
}

/// The tray bridge: `set`/`show_flyout` hop onto the Slint main thread so the
/// persisted level and the overlay/gamma batch stay consistent with the flyout;
/// `snapshot` reads the engine directly.
#[cfg(windows)]
pub(crate) struct TrayBridge {
    engine_tx: Sender<EngineCommand>,
}

#[cfg(windows)]
impl TrayBridge {
    pub(crate) fn new(engine_tx: Sender<EngineCommand>) -> Self {
        TrayBridge { engine_tx }
    }
}

#[cfg(windows)]
impl IpcBridge for TrayBridge {
    fn snapshot(&self) -> Vec<DisplaySnapshot> {
        engine_snapshot(&self.engine_tx)
    }

    fn set_level(&self, id: &str, pct: u8) -> bool {
        // Resolve the stable id off the engine snapshot (also the existence
        // check), then apply on the main thread through the flyout's own path.
        let Some(target) = self
            .snapshot()
            .into_iter()
            .find(|snap| snap.id.as_str() == id)
        else {
            return false;
        };
        crate::bin_support::tray::ipc_apply_set_level(target.id, pct);
        true
    }

    fn show_flyout(&self) {
        crate::bin_support::tray::ipc_show_flyout();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind};

    fn snap(serial: &str, level: u8) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x5B09, Some(serial)).unwrap(),
            name: "Panel".to_owned(),
            kind: DisplayKind::InternalPanel,
            software_only: false,
            user_level_pct: level,
            capabilities: Capabilities::default(),
        }
    }

    /// A fake bridge over a fixed table; records `set_level` calls.
    struct FakeBridge {
        displays: Vec<DisplaySnapshot>,
        sets: Mutex<Vec<(String, u8)>>,
        flyouts: AtomicU32,
    }

    impl FakeBridge {
        fn new(displays: Vec<DisplaySnapshot>) -> Self {
            FakeBridge {
                displays,
                sets: Mutex::new(Vec::new()),
                flyouts: AtomicU32::new(0),
            }
        }
    }

    impl IpcBridge for FakeBridge {
        fn snapshot(&self) -> Vec<DisplaySnapshot> {
            self.displays.clone()
        }
        fn set_level(&self, id: &str, pct: u8) -> bool {
            if self.displays.iter().any(|s| s.id.as_str() == id) {
                self.sets.lock().unwrap().push((id.to_owned(), pct));
                true
            } else {
                false
            }
        }
        fn show_flyout(&self) {
            self.flyouts.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn list_projects_every_snapshot() {
        let bridge = FakeBridge::new(vec![snap("A", 40), snap("B", 70)]);
        let resp = handle_request(&bridge, Request::ListDisplays);
        match resp {
            Response::Displays { displays } => {
                assert_eq!(displays.len(), 2);
                assert_eq!(displays.first().unwrap().level_pct, 40);
            }
            other => panic!("expected Displays, got {other:?}"),
        }
    }

    #[test]
    fn get_known_and_unknown() {
        let bridge = FakeBridge::new(vec![snap("A", 40)]);
        let id = snap("A", 40).id.as_str().to_owned();
        let resp = handle_request(&bridge, Request::GetBrightness { id: id.clone() });
        assert_eq!(resp, Response::Brightness { id, pct: 40 });

        let resp = handle_request(
            &bridge,
            Request::GetBrightness {
                id: "GSM-5B09-nope".to_owned(),
            },
        );
        assert!(matches!(resp, Response::Error { code, .. } if code == "unknown_display"));
    }

    #[test]
    fn set_routes_through_the_bridge_and_flags_unknown() {
        let bridge = FakeBridge::new(vec![snap("A", 40)]);
        let id = snap("A", 40).id.as_str().to_owned();
        let resp = handle_request(
            &bridge,
            Request::SetBrightness {
                id: id.clone(),
                pct: 25,
            },
        );
        assert_eq!(resp, Response::Ok);
        assert_eq!(bridge.sets.lock().unwrap().as_slice(), &[(id, 25)]);

        let resp = handle_request(
            &bridge,
            Request::SetBrightness {
                id: "GSM-5B09-nope".to_owned(),
                pct: 25,
            },
        );
        assert!(matches!(resp, Response::Error { code, .. } if code == "unknown_display"));
    }

    #[test]
    fn show_flyout_is_ok_and_calls_the_bridge() {
        let bridge = FakeBridge::new(vec![]);
        assert_eq!(handle_request(&bridge, Request::ShowFlyout), Response::Ok);
        assert_eq!(bridge.flyouts.load(Ordering::Relaxed), 1);
    }
}
