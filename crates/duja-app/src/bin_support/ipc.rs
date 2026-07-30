//! The binary's IPC wiring: starting the OS transport over an [`IpcBridge`],
//! the second-instance handshake, and the tray bridge.
//!
//! The transport-agnostic half — the [`IpcBridge`] trait, the
//! [`handle_request`] request→response mapping and the [`HeadlessBridge`] —
//! lives in the **library** (`duja_app::ipc`) so an integration test can drive
//! the whole transport → bridge → engine seam against fakes. Only what needs
//! the tray (or the process-wide default pipe name) stays here.
//!
//! # Consistency choice (plan §6)
//!
//! `list`/`get` are answered from a fresh engine
//! [`Snapshot`](duja_app::EngineCommand::Snapshot), read straight off the engine
//! thread — no main-thread hop. `set` is different: to keep the persisted user
//! level and the overlay/gamma batch consistent with the flyout, the (Windows
//! only, so deliberately not intra-doc-linked here) `TrayBridge` routes it
//! through the **same** main-thread `set_user_level` path a slider drag uses,
//! via [`slint::invoke_from_event_loop`].

use std::sync::Arc;
use std::time::Duration;

use tracing::{debug, info, warn};

use duja_ipc::Request;
use duja_platform::PipeServer;
use duja_platform::ipc::PipeClient;

pub(crate) use duja_app::ipc::{HeadlessBridge, IpcBridge, handle_request};

#[cfg(any(windows, target_os = "macos"))]
use crossbeam_channel::Sender;
#[cfg(any(windows, target_os = "macos"))]
use duja_app::EngineCommand;
#[cfg(any(windows, target_os = "macos"))]
use duja_core::model::DisplaySnapshot;

/// How long a second instance waits to reach the running server before giving
/// up on the show-flyout handshake.
const SECOND_INSTANCE_TIMEOUT: Duration = Duration::from_millis(500);

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
// RATIONALE: only the tray second-instance path calls this, and the tray does not
// exist on Linux yet, so keep that lane dead-code clean.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
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
#[cfg(any(windows, target_os = "macos"))]
pub(crate) struct TrayBridge {
    /// A **fully functional** headless bridge, embedded only for its
    /// [`snapshot`](IpcBridge::snapshot) (a plain engine read, identical in both
    /// bridges).
    ///
    /// Never call `self.bridge.set_level(..)` or `self.bridge.show_flyout()`
    /// from the tray path. Both compile and both look correct; they are wrong
    /// here for two reasons:
    ///
    /// - `HeadlessBridge::set_level` sends `SetUserLevel` **straight to the
    ///   engine**, bypassing the main-thread `AppState::set_user_level` that
    ///   owns the persisted user level, the overlay/gamma batch and the flyout
    ///   row. Hardware would move while state, overlay and slider stayed stale.
    /// - It runs on the IPC server's own thread, so it also skips the
    ///   [`slint::invoke_from_event_loop`] hop that puts the work on the Slint
    ///   thread — the precondition of the tray's `ReentrantCell` re-entrancy
    ///   rule (`AppState` is reachable only through `with_app`).
    ///
    /// The correct routes are [`set_level`](IpcBridge::set_level) and
    /// [`show_flyout`](IpcBridge::show_flyout) below, which hop first.
    bridge: HeadlessBridge,
}

#[cfg(any(windows, target_os = "macos"))]
impl TrayBridge {
    pub(crate) fn new(engine_tx: Sender<EngineCommand>) -> Self {
        TrayBridge {
            bridge: HeadlessBridge::new(engine_tx),
        }
    }
}

#[cfg(any(windows, target_os = "macos"))]
impl IpcBridge for TrayBridge {
    fn snapshot(&self) -> Vec<DisplaySnapshot> {
        // Same engine read as the headless bridge; only `set` differs.
        self.bridge.snapshot()
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
