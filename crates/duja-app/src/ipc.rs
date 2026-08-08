//! The app side of the local IPC control API: the narrow [`IpcBridge`] the
//! transport calls, the request→response mapping, and the headless bridge.
//!
//! # Where the mapping lives
//!
//! [`handle_request`] is a pure function of an [`IpcBridge`], so it is unit
//! tested with a fake bridge and never needs a real pipe. The OS transport
//! (`duja_platform::ipc`) owns threads and `unsafe`; this module owns only the
//! translation from a [`Request`] to an [`EngineCommand`] / UI action.
//!
//! It lives in the **library** rather than the binary so the whole assembly seam
//! — transport → bridge → engine actor → worker → controller — can be driven
//! end-to-end by an integration test against fakes (`tests/e2e_smoke.rs`). The
//! binary keeps only what needs the tray: the Windows `TrayBridge` (which hops
//! `set` onto the Slint main thread) and the server start/handshake helpers.
//!
//! # Consistency choice (plan §6)
//!
//! `list`/`get` are answered from a fresh engine [`Snapshot`](EngineCommand::Snapshot),
//! read straight off the engine thread — no main-thread hop. `set` is different:
//! to keep the persisted user level and the overlay/gamma batch consistent with
//! the flyout, the tray bridge routes it through the **same** main-thread
//! `set_user_level` path a slider drag uses. The [`HeadlessBridge`] here, which
//! owns no overlay/state, forwards `set` straight to the engine.

use std::time::Duration;

use crossbeam_channel::{Sender, bounded};

use duja_core::model::DisplaySnapshot;
use duja_ipc::{DisplayInfo, Request, Response};

use crate::EngineCommand;

/// How long the IPC handler waits for the engine to answer a snapshot request
/// before giving up (and returning an empty list).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(500);

/// The narrow app-facing capability set the IPC request handler needs.
///
/// Kept deliberately small: the transport only ever asks for a snapshot, a level
/// change, or a flyout nudge. Implementors decide how each maps onto the running
/// app ([`HeadlessBridge`] for the console pipeline; a main-thread UI path for
/// the tray).
pub trait IpcBridge: Send + Sync + 'static {
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
#[must_use]
pub fn handle_request(bridge: &dyn IpcBridge, request: Request) -> Response {
    match request {
        Request::ListDisplays => Response::Displays {
            displays: bridge
                .snapshot()
                .iter()
                .map(DisplayInfo::from_snapshot)
                .collect(),
        },
        Request::GetBrightness { id } => {
            let snapshot = bridge.snapshot();
            find_by_id(&snapshot, &id).map_or_else(
                || unknown_display(&id),
                |snap| Response::Brightness {
                    id: id.clone(),
                    pct: snap.user_level_pct,
                },
            )
        }
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

/// Find the snapshot whose stable id renders as `id`.
///
/// One function rather than the three copies of
/// `find(|snap| snap.id.as_str() == id)` that used to sit in [`handle_request`]'s
/// `GetBrightness` arm, in [`HeadlessBridge::set_level`], and in the binary's
/// `TrayBridge::set_level`. The three were identical and only two of them were
/// ever reached by a test — the binary's copy measured **0 %** of regions on
/// 2026-08-08 — so a change to the matching rule in one place would have left
/// the tray path silently disagreeing with the headless one about which display
/// a `dujactl set` names.
///
/// Public because the third caller lives in the binary crate, which is the whole
/// reason the duplication existed.
///
/// **This is not the only id-matching rule in the codebase, and it should not
/// become one.** [`duja_core::id::select_slot_match`] resolves a `-slot<n>`
/// twin against the hardware and is what `backend.rs` and `dujactl` use. The
/// two are deliberately different: a snapshot id already carries its slot
/// suffix, so the IPC surface wants exact equality and nothing cleverer.
/// Matching becomes wrong here the moment it starts guessing.
///
/// Ties are resolved first-wins. The manager gives twins distinct ids, so a
/// duplicate is a defect upstream rather than a case to define policy for —
/// but the behaviour is pinned by a test so a later `.rev()` or `.filter().last()`
/// is a decision rather than an accident.
#[must_use]
pub fn find_by_id<'a>(snapshot: &'a [DisplaySnapshot], id: &str) -> Option<&'a DisplaySnapshot> {
    snapshot.iter().find(|snap| snap.id.as_str() == id)
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
#[derive(Debug)]
pub struct HeadlessBridge {
    engine_tx: Sender<EngineCommand>,
}

impl HeadlessBridge {
    /// Bridge onto the engine reachable through `engine_tx`.
    #[must_use]
    pub fn new(engine_tx: Sender<EngineCommand>) -> Self {
        HeadlessBridge { engine_tx }
    }
}

impl IpcBridge for HeadlessBridge {
    fn snapshot(&self) -> Vec<DisplaySnapshot> {
        engine_snapshot(&self.engine_tx)
    }

    fn set_level(&self, id: &str, pct: u8) -> bool {
        let snapshot = self.snapshot();
        let Some(target) = find_by_id(&snapshot, id) else {
            return false;
        };
        self.engine_tx
            .send(EngineCommand::SetUserLevel {
                id: target.id.clone(),
                pct,
            })
            .is_ok()
    }

    fn show_flyout(&self) {
        // No UI in headless mode; ShowFlyout is a documented no-op.
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

    #[test]
    fn find_by_id_matches_on_the_rendered_stable_id() {
        let (first, second) = (snap("AAA", 40), snap("BBB", 70));
        let wanted = second.id.as_str().to_owned();
        let table = vec![first, second];

        let hit = find_by_id(&table, &wanted).expect("the id is in the table");
        assert_eq!(hit.id.as_str(), wanted);
        // The *right* one, not merely one: both rows share a manufacturer and a
        // product code and differ only by serial, which is the case a lookup that
        // compared the wrong field would still pass.
        assert_eq!(hit.user_level_pct, 70);
    }

    #[test]
    fn find_by_id_rejects_an_unknown_id_rather_than_falling_back() {
        let only = snap("AAA", 40);
        let known = only.id.as_str().to_owned();
        let table = vec![only];

        assert!(find_by_id(&table, "not-a-display").is_none());
        // Empty is the shape `engine_snapshot` returns when the engine is gone or
        // slow, and it must not resolve to a first element that is not there.
        assert!(find_by_id(&[], &known).is_none());
    }

    #[test]
    fn find_by_id_is_exact_rather_than_a_prefix_or_substring_match() {
        // The guard that matters for an IPC surface: `dujactl set <id>` must not
        // reach a *different* monitor because one id is a prefix of another. A
        // `starts_with`/`contains` regression reds here and nowhere else.
        let only = snap("AAA", 40);
        let full = only.id.as_str().to_owned();
        let table = vec![only];

        // `pop` rather than a range index: `indexing_slicing` is a workspace lint
        // and tests are not exempt from it, and a byte range would be wrong on a
        // multi-byte id anyway.
        let mut truncated = full.clone();
        truncated.pop();

        assert!(find_by_id(&table, &truncated).is_none());
        assert!(find_by_id(&table, &format!("{full}X")).is_none());
        assert!(find_by_id(&table, &full).is_some());
    }

    #[test]
    fn a_duplicate_id_resolves_to_the_first_row() {
        // The one behavioural property the hoist froze into a *public* function
        // and that nothing else pinned: which row wins a tie. The manager gives
        // twins distinct `-slot<n>` ids, so a duplicate here is an upstream
        // defect rather than a supported case - but "first wins" is now the
        // documented answer, and `.rev()` or `.filter(..).last()` would stay
        // green without this.
        let (first, shadow) = (snap("AAA", 11), snap("AAA", 99));
        let id = first.id.as_str().to_owned();
        assert_eq!(id, shadow.id.as_str(), "the fixture must actually collide");
        let table = vec![first, shadow];

        let hit = find_by_id(&table, &id).expect("the id is in the table");
        assert_eq!(hit.user_level_pct, 11, "first match wins, not last");
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
