//! Assembly of the real Duja pipeline for the `--once` and `--headless` modes,
//! plus the shared platform-event forwarding used by the stress harness too.

use std::io::BufRead;
use std::process::ExitCode;
use std::thread::{self, JoinHandle};

use anyhow::Context;
use crossbeam_channel::Receiver;

use duja_app::{EngineNotification, Enumeration};
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
        Box::new(backend::open_controller),
        tick_rx,
    );

    let notif_join = spawn_notification_printer(notifications);

    eprintln!("duja headless: pipeline running. type `q` then Enter to quit.");
    wait_for_quit();

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

/// `--restore`: P4 stub.
pub(crate) fn restore() -> ExitCode {
    println!("nothing to restore (no gamma/overlay state yet)");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::{format_notification, summarize_snapshots};
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
}
