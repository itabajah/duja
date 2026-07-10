//! Wiring the opt-in gamma sub-floor channel into the app's apply batch.
//!
//! A `dim_mode = "gamma"` display dims below its hardware floor by scaling the
//! GPU gamma ramp instead of stacking an overlay. Gamma is **not** part of the
//! overlay [`Dimmer::apply`](duja_core::dimmer::Dimmer) contract — a Windows
//! gamma ramp persists after the process dies, so it is engaged only through a
//! separate, explicit API guarded by a crash marker (`duja_dimmer`'s
//! `ScreenStateGuard`). This module is that explicit driver.
//!
//! # Split for testability
//!
//! - [`GammaCoordinator`] is the pure decision core: given each apply batch's
//!   [`DimCommand`]s and the set already engaged, it decides which displays to
//!   engage (and at what factor) and which to restore. It never touches the OS —
//!   it drives a [`GammaSink`], so its logic is exhaustively unit-tested against
//!   a fake sink on every target.
//! - `GuardSink` (Windows only) is the real sink: it correlates a resolved
//!   display id to its GDI device name and drives `ScreenStateGuard`'s
//!   `engage_gamma` / `restore_display`, which write and clear the crash marker.
//! - `GammaBackend` (Windows only) bundles the two and is what the tray owns.
//!
//! Before this module existed, `dim_mode = "gamma"` was a silent no-op and the
//! crash-marker machinery was dead code (P4 gate Finding 2): the planner emitted
//! `DimCommand { gamma: Some(_) }` but nothing ever engaged a ramp.

// RATIONALE: the pure coordinator/trait are consumed only by the Windows
// `GammaBackend` (the tray is `cfg(windows)`), but they stay cross-platform so
// their unit tests run on every CI OS; the dead-code allow applies only where no
// consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use duja_core::dimmer::DimCommand;
use duja_core::id::StableDisplayId;

/// A per-display gamma engage/restore executor.
///
/// Abstracts the OS gamma ramp so [`GammaCoordinator`]'s decisions are testable
/// with a fake. The real implementation is Windows' `GuardSink`.
pub(crate) trait GammaSink {
    /// Engage (or re-engage) gamma dimming for `id` at `factor` (`1.0` = identity,
    /// down to `GAMMA_FLOOR`).
    fn engage(&mut self, id: &StableDisplayId, factor: f32);
    /// Restore identity gamma for one display previously engaged.
    fn restore(&mut self, id: &StableDisplayId);
    /// Restore every engaged display and clear the crash marker (clean teardown).
    fn restore_all(&mut self);
}

/// The pure decision core: tracks which displays currently have gamma engaged
/// (and at what factor) and reconciles that against each apply batch.
#[derive(Debug, Default)]
pub(crate) struct GammaCoordinator {
    /// Resolved id → currently-engaged factor, as raw bits for exact (lint-free,
    /// `NaN`-free — the factor is always a clamped `clamp_gamma` output) compare.
    engaged: BTreeMap<StableDisplayId, u32>,
}

impl GammaCoordinator {
    /// Reconcile the gamma channel with one apply batch.
    ///
    /// Engages every command carrying a gamma factor (only when newly present or
    /// the factor changed, so an unchanged ramp is never rewritten), and restores
    /// every previously-engaged display that no longer carries one. Commands with
    /// `gamma: None` (the default overlay path, and every HDR/unknown display —
    /// `effective_mode` forces them to overlay) never engage the ramp.
    pub(crate) fn apply(&mut self, commands: &[DimCommand], sink: &mut impl GammaSink) {
        let mut present: BTreeSet<StableDisplayId> = BTreeSet::new();
        for cmd in commands {
            let Some(factor) = cmd.gamma else { continue };
            present.insert(cmd.id.clone());
            let bits = factor.to_bits();
            if self.engaged.get(&cmd.id) != Some(&bits) {
                sink.engage(&cmd.id, factor);
                self.engaged.insert(cmd.id.clone(), bits);
            }
        }

        let dropped: Vec<StableDisplayId> = self
            .engaged
            .keys()
            .filter(|id| !present.contains(*id))
            .cloned()
            .collect();
        for id in dropped {
            sink.restore(&id);
            self.engaged.remove(&id);
        }
    }

    /// Forget the engaged set without issuing per-display restores (the caller
    /// pairs this with [`GammaSink::restore_all`], which restores everything).
    pub(crate) fn forget_all(&mut self) {
        self.engaged.clear();
    }
}

#[cfg(windows)]
pub(crate) use platform::GammaBackend;

#[cfg(windows)]
mod platform {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use duja_core::dimmer::{DimCommand, GAMMA_FLOOR};
    use duja_core::id::StableDisplayId;
    use duja_dimmer::{GammaDisplay, ScreenStateGuard};
    use tracing::warn;

    use super::{GammaCoordinator, GammaSink};

    /// Resolve a resolved display id to its GDI device name (e.g. `\\.\DISPLAY1`).
    type DeviceResolver = Box<dyn FnMut(&StableDisplayId) -> Option<String>>;

    /// The real gamma sink: correlates ids to GDI devices and drives the
    /// crash-marker-guarded ramp.
    struct GuardSink {
        guard: ScreenStateGuard,
        resolve: DeviceResolver,
        /// Resolved id → the GDI device name engaged for it, so a later restore
        /// targets the exact device the engage used (device names can change
        /// across a hot-plug).
        engaged: BTreeMap<StableDisplayId, String>,
    }

    impl GammaSink for GuardSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) {
            debug_assert!(
                (GAMMA_FLOOR..=1.0).contains(&factor),
                "gamma factor {factor} out of range; HDR/unknown must force overlay"
            );
            let Some(device) = (self.resolve)(id) else {
                warn!(id = %id.as_str(), "no GDI device for gamma display; skipping ramp");
                return;
            };
            if let Err(e) = self
                .guard
                .engage_gamma(GammaDisplay::from_device_name(&device), factor)
            {
                warn!(id = %id.as_str(), device, error = %e, "gamma engage failed");
            }
            self.engaged.insert(id.clone(), device);
        }

        fn restore(&mut self, id: &StableDisplayId) {
            if let Some(device) = self.engaged.remove(id)
                && let Err(e) = self.guard.restore_display(&device)
            {
                warn!(id = %id.as_str(), device, error = %e, "gamma restore failed");
            }
        }

        fn restore_all(&mut self) {
            self.engaged.clear();
            let report = self.guard.restore_now();
            if !report.failed.is_empty() {
                warn!(failed = report.failed.len(), "some gamma restores failed");
            }
        }
    }

    /// The tray-owned gamma channel: the pure coordinator plus the real sink.
    ///
    /// Dropping it restores every engaged display and clears the crash marker
    /// (the [`ScreenStateGuard`]'s `Drop`), so an abnormal teardown still leaves
    /// identity gamma behind.
    pub(crate) struct GammaBackend {
        coord: GammaCoordinator,
        sink: GuardSink,
    }

    impl GammaBackend {
        /// Build a gamma channel whose guard writes/clears its crash marker at
        /// `marker`, using `resolve` to map a resolved display id to its GDI
        /// device name.
        pub(crate) fn new(
            marker: PathBuf,
            resolve: impl FnMut(&StableDisplayId) -> Option<String> + 'static,
        ) -> Self {
            GammaBackend {
                coord: GammaCoordinator::default(),
                sink: GuardSink {
                    guard: ScreenStateGuard::new(Some(marker)),
                    resolve: Box::new(resolve),
                    engaged: BTreeMap::new(),
                },
            }
        }

        /// Drive the gamma channel for one apply batch.
        pub(crate) fn apply(&mut self, commands: &[DimCommand]) {
            self.coord.apply(commands, &mut self.sink);
        }

        /// Restore every engaged display and clear the crash marker.
        pub(crate) fn restore_all(&mut self) {
            self.coord.forget_all();
            self.sink.restore_all();
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use duja_core::dimmer::DisplayBounds;

        fn id(serial: &str) -> StableDisplayId {
            StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
        }

        fn gamma_cmd(serial: &str, factor: f32) -> DimCommand {
            DimCommand::new(
                id(serial),
                DisplayBounds::new(0, 0, 1920, 1080),
                0.0,
                Some(factor),
            )
        }

        #[test]
        fn first_gamma_engage_writes_marker_and_clean_quit_clears_it() {
            // The guard/marker file flow is headless-safe: the resolver hands back
            // a device name so the coordinator's engage reaches the guard, whose
            // `engage_gamma` writes the marker BEFORE it attempts the Win32 ramp
            // write (which fails harmlessly for a bogus device in a disconnected
            // session — the marker is already written). A clean quit clears it.
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("gamma.dirty");
            let mut backend =
                GammaBackend::new(marker.clone(), |_id| Some(r"\\.\DUJA_TEST".to_owned()));

            assert!(!marker.exists(), "no marker before any engage");
            backend.apply(&[gamma_cmd("A", 0.6)]);
            assert!(
                marker.exists(),
                "the first gamma engage must write the crash marker"
            );

            backend.restore_all();
            assert!(!marker.exists(), "a clean quit must clear the crash marker");
        }

        #[test]
        fn missing_device_engages_nothing_and_leaves_no_marker() {
            // A gamma command whose id cannot be correlated to a GDI device must
            // not write a marker (nothing was engaged).
            let dir = tempfile::tempdir().expect("tempdir");
            let marker = dir.path().join("gamma.dirty");
            let mut backend = GammaBackend::new(marker.clone(), |_id| None);

            backend.apply(&[gamma_cmd("A", 0.6)]);
            assert!(
                !marker.exists(),
                "an uncorrelated gamma command must not mark dirty"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::dimmer::DisplayBounds;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    fn cmd(serial: &str, gamma: Option<f32>) -> DimCommand {
        DimCommand::new(id(serial), DisplayBounds::new(0, 0, 1920, 1080), 0.0, gamma)
    }

    /// A fake sink that records every engage/restore call for assertions.
    #[derive(Default)]
    struct FakeSink {
        engaged: Vec<(StableDisplayId, f32)>,
        restored: Vec<StableDisplayId>,
    }

    impl GammaSink for FakeSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) {
            self.engaged.push((id.clone(), factor));
        }
        fn restore(&mut self, id: &StableDisplayId) {
            self.restored.push(id.clone());
        }
        fn restore_all(&mut self) {}
    }

    #[test]
    fn gamma_command_engages_on_the_sink() {
        // Regression for P4 gate Finding 2: a gamma-mode sub-floor plan must reach
        // the gamma engage API. Before the fix, nothing ever called it.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        assert_eq!(sink.engaged.len(), 1);
        let (engaged_id, factor) = sink.engaged.first().expect("one engage");
        assert_eq!(*engaged_id, id("A"));
        assert!((factor - 0.6).abs() < f32::EPSILON);
    }

    #[test]
    fn overlay_only_command_never_engages() {
        // `gamma: None` is the default overlay path AND every HDR/unknown display
        // (forced to overlay by `effective_mode`): none may touch the ramp.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", None)], &mut sink);
        assert!(sink.engaged.is_empty());
        assert!(sink.restored.is_empty());
    }

    #[test]
    fn stable_factor_does_not_re_engage() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        assert_eq!(
            sink.engaged.len(),
            1,
            "unchanged factor must not rewrite the ramp"
        );
    }

    #[test]
    fn changed_factor_re_engages() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[cmd("A", Some(0.4))], &mut sink);
        assert_eq!(sink.engaged.len(), 2);
        let (_, factor) = sink.engaged.get(1).expect("two engages");
        assert!((factor - 0.4).abs() < f32::EPSILON);
    }

    #[test]
    fn dropping_gamma_restores_that_display() {
        // The slider rises above the gamma sub-floor zone: the command now carries
        // no gamma, so the display's ramp must be restored to identity.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[cmd("A", None)], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    #[test]
    fn absent_display_is_restored() {
        // A display that vanishes from the batch entirely (unplugged) is restored.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    #[test]
    fn independent_displays_engage_and_restore_independently() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6)), cmd("B", Some(0.5))], &mut sink);
        assert_eq!(sink.engaged.len(), 2);
        // B drops gamma; A keeps it.
        coord.apply(&[cmd("A", Some(0.6)), cmd("B", None)], &mut sink);
        assert_eq!(sink.restored, vec![id("B")]);
        assert_eq!(sink.engaged.len(), 2, "A must not re-engage on B's change");
    }

    #[test]
    fn forget_all_clears_tracking_without_per_display_restores() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.forget_all();
        // After forgetting, an empty batch issues no restore (the backend pairs
        // this with a whole-guard restore instead).
        coord.apply(&[], &mut sink);
        assert!(sink.restored.is_empty());
    }
}
