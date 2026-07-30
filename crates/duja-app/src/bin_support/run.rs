//! Assembly of the real Duja pipeline for the `--once` and `--headless` modes,
//! plus the shared platform-event forwarding used by the stress harness too.

use std::io::BufRead;
use std::process::ExitCode;
use std::thread::{self, JoinHandle};

use anyhow::Context;
use crossbeam_channel::Receiver;

use duja_app::{EngineNotification, Enumeration};
use duja_core::id::StableDisplayId;
use duja_core::manager::DiscoveredDisplay;
use duja_core::model::DisplaySnapshot;
use duja_platform::{EventPump, PlatformEvent};

use crate::bin_support::backend;
use crate::bin_support::fmt::{features_label, kind_label, render_table};

/// Build the engine's enumerator: a closure that runs one real enumeration.
pub(crate) fn enumerator() -> duja_app::Enumerator {
    Box::new(|| Enumeration {
        displays: backend::discover(),
    })
}

/// Build the engine's controller factory: each call returns a deferred opener
/// that re-enumerates and opens the display **on the worker thread** (so any
/// thread-affine backend resource, e.g. a WMI COM apartment, is created and
/// used on one thread).
pub(crate) fn controller_factory() -> duja_app::ControllerFactory {
    Box::new(|id: &StableDisplayId| {
        let id = id.clone();
        Box::new(move || backend::open_controller(&id)) as duja_app::ControllerOpener
    })
}

/// Owns the platform event pump and the thread that forwards its events into
/// the engine's `()`-tick channel. Shut down explicitly (or on drop).
pub(crate) struct PlatformForwarder {
    pump: Option<EventPump>,
    join: Option<JoinHandle<()>>,
}

impl PlatformForwarder {
    /// Stop the pump and join the forwarding thread. Idempotent.
    pub(crate) fn shutdown(&mut self) {
        // Dropping the pump closes its sender, which ends the forwarding loop.
        if let Some(pump) = self.pump.take() {
            pump.shutdown();
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for PlatformForwarder {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn the platform event pump and a thread that maps
/// `DisplaysChanged` / `Resumed` / `SessionUnlocked` to `()` ticks
/// (`Suspending` is ignored for now).
///
/// Returns the tick receiver to hand to `Engine::spawn`, and the forwarder
/// handle to keep alive for the engine's lifetime.
///
/// # Errors
/// Propagates any [`duja_platform::PlatformError`] from starting the pump.
pub(crate) fn start_platform() -> anyhow::Result<(Receiver<()>, PlatformForwarder)> {
    let (pump, pump_rx) = EventPump::spawn().context("starting the platform event pump")?;
    let (tick_tx, tick_rx) = crossbeam_channel::unbounded::<()>();

    let join = thread::spawn(move || {
        while let Ok(event) = pump_rx.recv() {
            match event {
                PlatformEvent::DisplaysChanged
                | PlatformEvent::Resumed
                | PlatformEvent::SessionUnlocked => {
                    if tick_tx.send(()).is_err() {
                        break; // engine gone; stop forwarding.
                    }
                }
                PlatformEvent::Suspending => {}
            }
        }
    });

    Ok((
        tick_rx,
        PlatformForwarder {
            pump: Some(pump),
            join: Some(join),
        },
    ))
}

/// `--once`: one enumeration, print a table, exit 0 (also when empty).
pub(crate) fn once() -> ExitCode {
    let displays = backend::discover();
    if displays.is_empty() {
        println!("no displays");
        return ExitCode::SUCCESS;
    }
    println!("{}", once_table(&displays));
    ExitCode::SUCCESS
}

/// Build the `--once` table, reading each display's current level through a
/// freshly-opened controller (shown as `?` when it cannot be read).
fn once_table(displays: &[DiscoveredDisplay]) -> String {
    let rows: Vec<Vec<String>> = displays
        .iter()
        .map(|d| {
            vec![
                d.id.as_str().to_owned(),
                kind_label(d.kind).to_owned(),
                d.name.clone().unwrap_or_else(|| "-".to_owned()),
                read_level_label(d),
                features_label(&d.capabilities),
            ]
        })
        .collect();
    render_table(&["id", "kind", "name", "level", "features"], &rows)
}

/// Open a controller for `display` and read its brightness as a percent label.
fn read_level_label(display: &DiscoveredDisplay) -> String {
    let Some(mut controller) = backend::open_controller(&display.id) else {
        return "?".to_owned();
    };
    match controller.get(duja_core::model::Feature::Brightness) {
        Ok(range) => format!(
            "{}%",
            crate::bin_support::num::raw_to_pct(range.current, range.max)
        ),
        Err(_) => "?".to_owned(),
    }
}

/// `--headless`: assemble the full pipeline and run until `q<Enter>` (or EOF).
///
/// # Errors
/// Propagates a failure to start the platform event pump.
pub(crate) fn headless() -> anyhow::Result<ExitCode> {
    let (tick_rx, mut forwarder) = start_platform()?;

    let (engine, notifications) = duja_app::Engine::spawn(
        duja_app::EngineConfig::default(),
        enumerator(),
        controller_factory(),
        tick_rx,
    );

    // IPC control server so dujactl can drive the headless pipeline too.
    let ipc_server = crate::bin_support::ipc::start(std::sync::Arc::new(
        crate::bin_support::ipc::HeadlessBridge::new(engine.sender()),
    ));

    let notif_join = spawn_notification_printer(notifications);

    eprintln!("duja headless: pipeline running. type `q` then Enter to quit.");
    wait_for_quit();

    if let Some(server) = ipc_server {
        server.shutdown();
    }
    engine.shutdown();
    forwarder.shutdown();
    let _ = notif_join.join();
    Ok(ExitCode::SUCCESS)
}

/// Print engine notifications to stderr, one readable line each, until the
/// channel closes.
fn spawn_notification_printer(notifications: Receiver<EngineNotification>) -> JoinHandle<()> {
    thread::spawn(move || {
        while let Ok(notification) = notifications.recv() {
            eprintln!("{}", format_notification(&notification));
        }
    })
}

/// Render one [`EngineNotification`] as a single readable line.
fn format_notification(notification: &EngineNotification) -> String {
    match notification {
        EngineNotification::DisplaysChanged(snaps) => {
            format!("displays-changed: {}", summarize_snapshots(snaps))
        }
        EngineNotification::DisplayUnresponsive(id) => {
            format!("display-unresponsive: {}", id.as_str())
        }
        EngineNotification::DisplayResponsive(id) => {
            format!("display-responsive: {}", id.as_str())
        }
        EngineNotification::LevelRead { id, hw_pct } => {
            format!("level-read: {}={hw_pct}%", id.as_str())
        }
    }
}

/// A compact one-line summary of a snapshot list.
fn summarize_snapshots(snaps: &[DisplaySnapshot]) -> String {
    if snaps.is_empty() {
        return "(no displays)".to_owned();
    }
    snaps
        .iter()
        .map(|s| format!("{}={}%", s.id.as_str(), s.user_level_pct))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Block until the user types a line beginning with `q`, or stdin reaches EOF.
fn wait_for_quit() {
    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        match stdin.lock().read_line(&mut line) {
            // EOF (piped/closed stdin) or a read error: treat as quit so the
            // harness still exits cleanly.
            Ok(0) | Err(_) => break,
            Ok(_) => {
                if line.trim_start().starts_with('q') {
                    break;
                }
            }
        }
    }
}

/// What `--restore` resets the gamma ramp *to*, which is not the same thing on
/// both platforms — so the summary line must not claim it is.
///
/// Windows writes a linear ramp per display (`restore_identity`); macOS asks the
/// window server to reload each display's `ColorSync` profile
/// (`CGDisplayRestoreColorSyncSettings`), which for a calibrated display is by
/// definition *not* identity.
#[cfg(windows)]
const RESTORE_SUMMARY: &str = "restored identity gamma on";
#[cfg(target_os = "macos")]
const RESTORE_SUMMARY: &str = "reset gamma to the ColorSync profile on";

/// `--restore`: reset any persisted screen state.
///
/// Overlay windows die with their owning process, so a separate `--restore`
/// invocation cannot touch a running tray instance's overlays; what it *can*
/// undo is the one piece of screen state that outlives a process — the gamma
/// ramp — via `duja_dimmer::restore_all`. Exit is non-zero if any display
/// could not be reset.
///
/// # What this actually rescues on macOS
///
/// This section previously said `duja-app` had no gamma-engage path on macOS and
/// pre-registered its own rewrite for "the moment the tray is un-gated". That is
/// this commit, so here is the rewrite.
///
/// **`--restore` on macOS now does two jobs at once**, and they are worth keeping
/// apart:
///
/// - It can undo **Duja's own** ramp. [`gamma::GammaBackend`]'s macOS sink and its
///   only consumer, the tray, both exist now, so a `dim_mode = "gamma"` display
///   engaged by a previous run is genuinely something this can be reversing.
/// - It remains a **general screen rescue**: `CGDisplayRestoreColorSyncSettings`
///   reloads *every* display's `ColorSync` profile, clearing a ramp left by any
///   process (f.lux, a calibration loader, a crashed tool), whether or not Duja
///   put it there.
///
/// The second is why the blast radius is wider than the Windows path's, which
/// touches only the displays it recorded. It is safe — it restores the user's own
/// calibration rather than flattening it to identity — but it is a different
/// promise from "undo Duja's leftovers", and the command keeps the wider one.
///
/// One asymmetry survives from before: a *dirty exit* on macOS is believed to need
/// no rescue at all, because the window server restores a process's transfer tables
/// when it exits, which is why the mac dimmer carries no crash-marker machinery.
/// How well established that belief is — widely observed, undocumented by Apple —
/// is set out in `gamma.rs`. If it is ever found to be wrong, this command is the
/// only recovery macOS has, since nothing writes a marker for
/// `startup::recover_from_crash_marker` to find.
///
/// [`gamma::GammaBackend`]: super::gamma
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn restore() -> ExitCode {
    let report = duja_dimmer::restore_all();
    let (lines, ok) = restore_outcome(report.restored.len(), &report.failed, RESTORE_SUMMARY);
    for line in lines {
        println!("{line}");
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Pure core of [`restore`]: the lines to print and whether the run succeeded.
///
/// Split out because the interesting part is a four-way decision (nothing to do
/// / summary / per-failure detail / exit code) over a report, and folding it into
/// the `println!` path would leave the user-visible strings untested on every
/// platform. Takes primitives rather than `&RestoreReport` so it stays
/// cross-platform: the report type is per-backend, but its shape is not, and
/// these tests then run on all three CI lanes.
///
/// The empty-report branch is reachable on Windows only. On macOS
/// `restore_all` reports every enumerated display (falling back to the main
/// display when enumeration is empty), so `restored` is never empty there and
/// the "nothing to restore" line cannot fire — noted so nobody reads it as a
/// macOS "Duja had nothing to clean up" signal.
// RATIONALE (dead_code): the pure decision stays cross-platform so its tests run
// on every CI OS, but it is only *called* from the gamma-capable arm above.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn restore_outcome(
    restored: usize,
    failed: &[(String, String)],
    summary: &str,
) -> (Vec<String>, bool) {
    if restored == 0 && failed.is_empty() {
        return (
            vec!["nothing to restore (no displays with a resettable gamma ramp)".to_owned()],
            true,
        );
    }
    let mut lines = vec![format!("{summary} {restored} display(s)")];
    lines.extend(
        failed
            .iter()
            .map(|(name, err)| format!("  failed: {name}: {err}")),
    );
    (lines, failed.is_empty())
}

/// `--restore` where no dimmer backend exists (currently Linux): there is no
/// gamma ramp Duja could have left behind, so there is nothing to undo.
///
/// Deliberately narrow. This arm used to cover macOS too and told the user
/// "software dimming is Windows-only in this build", which stopped being true
/// when the macOS dimmer landed in P6 wave 1 — a stub that had quietly become a
/// false statement about the user's own screen.
#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) fn restore() -> ExitCode {
    println!("nothing to restore (no software dimming backend on this platform)");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{format_notification, restore_outcome, summarize_snapshots};
    use duja_app::EngineNotification;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x5B09, Some(serial)).unwrap()
    }

    fn snap(serial: &str, level: u8) -> DisplaySnapshot {
        DisplaySnapshot {
            id: id(serial),
            name: "Panel".to_owned(),
            kind: DisplayKind::InternalPanel,
            software_only: false,
            user_level_pct: level,
            capabilities: Capabilities::default(),
        }
    }

    #[test]
    fn summarize_handles_empty_and_populated() {
        assert_eq!(summarize_snapshots(&[]), "(no displays)");
        let line = summarize_snapshots(&[snap("A", 40), snap("B", 70)]);
        assert!(line.contains("40%"));
        assert!(line.contains("70%"));
    }

    #[test]
    fn notification_lines_are_readable() {
        assert!(
            format_notification(&EngineNotification::DisplaysChanged(vec![snap("A", 50)]))
                .starts_with("displays-changed:")
        );
        assert_eq!(
            format_notification(&EngineNotification::DisplayUnresponsive(id("A"))),
            format!("display-unresponsive: {}", id("A").as_str())
        );
        assert!(
            format_notification(&EngineNotification::DisplayResponsive(id("A")))
                .starts_with("display-responsive:")
        );
    }

    #[test]
    fn restore_reports_nothing_to_do_only_when_the_report_is_wholly_empty() {
        let (lines, ok) = restore_outcome(0, &[], "restored identity gamma on");
        assert_eq!(
            lines,
            ["nothing to restore (no displays with a resettable gamma ramp)"]
        );
        assert!(ok);
    }

    #[test]
    fn restore_summary_uses_the_platform_wording_it_is_given() {
        // The summary is a parameter precisely because Windows writes a linear
        // ramp while macOS reloads the ColorSync profile; "identity" is wrong on
        // macOS, so the caller supplies the verb rather than this deciding.
        let (win, _) = restore_outcome(2, &[], "restored identity gamma on");
        assert_eq!(win, ["restored identity gamma on 2 display(s)"]);
        let (mac, _) = restore_outcome(2, &[], "reset gamma to the ColorSync profile on");
        assert_eq!(
            mac,
            ["reset gamma to the ColorSync profile on 2 display(s)"]
        );
    }

    #[test]
    fn restore_lists_each_failure_and_fails_the_exit_code() {
        let failed = [
            (
                "\\\\.\\DISPLAY1".to_owned(),
                "SetDeviceGammaRamp failed".to_owned(),
            ),
            ("\\\\.\\DISPLAY2".to_owned(), "access denied".to_owned()),
        ];
        let (lines, ok) = restore_outcome(1, &failed, "restored identity gamma on");
        assert_eq!(
            lines,
            [
                "restored identity gamma on 1 display(s)",
                "  failed: \\\\.\\DISPLAY1: SetDeviceGammaRamp failed",
                "  failed: \\\\.\\DISPLAY2: access denied",
            ]
        );
        assert!(!ok, "any failure must make --restore exit non-zero");
    }

    #[test]
    fn restore_reports_failures_even_when_nothing_was_restored() {
        // Not the empty-report case: a run where every display failed must still
        // say so and exit non-zero, rather than printing "nothing to restore".
        let failed = [("\\\\.\\DISPLAY1".to_owned(), "access denied".to_owned())];
        let (lines, ok) = restore_outcome(0, &failed, "restored identity gamma on");
        assert_eq!(
            lines.first().map(String::as_str),
            Some("restored identity gamma on 0 display(s)")
        );
        assert!(!ok);
    }
}
