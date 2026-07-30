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
    /// down to `GAMMA_FLOOR`), returning whether the ramp is now **live**.
    ///
    /// `false` means the ramp was not written — the OS refused it, or the display
    /// could not be correlated to a gamma device. The coordinator must not then
    /// record the factor as engaged: it never took effect, so there is nothing to
    /// restore later and a retry on the next batch is the only route back.
    fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool;
    /// Restore identity gamma for one display previously engaged.
    fn restore(&mut self, id: &StableDisplayId);
    /// Restore every engaged display and clear the crash marker (clean teardown),
    /// returning whether every restore succeeded (`true` = clean). A `false`
    /// return means at least one ramp could not be reset and the crash marker was
    /// kept, so the caller must not force-remove it.
    fn restore_all(&mut self) -> bool;
}

/// Reconcile a gamma engage map (resolved id → GDI device name) with the outcome
/// of a restore pass: drop every id whose device restored cleanly, and RETAIN any
/// id whose device is still in `failed_devices` — its ramp is still live and the
/// guard still tracks that device in its `touched` set, so the two must not
/// diverge.
///
/// Pure (no OS), so the name→id retention rule is unit-tested on every target
/// without a real GDI guard. Correct under the "device names can change across a
/// hot-plug" caveat: it matches on the exact device name the engage recorded.
fn retain_failed_engagements(
    engaged: &mut BTreeMap<StableDisplayId, String>,
    failed_devices: &BTreeSet<String>,
) {
    engaged.retain(|_id, device| failed_devices.contains(device));
}

/// Once-per-display logging state for a refused gamma engage.
///
/// A slider drag re-plans on every frame, so a display whose ramp the OS refuses
/// emits one warning per apply: the reported log holds **349** identical lines,
/// up to a dozen inside a single 450 ms drag. This tracks which displays are
/// currently refusing so a line is emitted on each *transition* — once when the
/// ramp starts being refused, once if it later succeeds — rather than per frame.
///
/// Pure (a set of ids), so the suppression rule is unit-tested on every target
/// without a failing ramp.
#[derive(Debug, Default)]
pub(crate) struct RefusalLog {
    /// Displays whose most recent engage attempt was refused.
    refusing: BTreeSet<StableDisplayId>,
}

impl RefusalLog {
    /// Record a refused engage for `id`; `true` when this is a *new* refusal and
    /// therefore worth logging.
    pub(crate) fn note_refusal(&mut self, id: &StableDisplayId) -> bool {
        self.refusing.insert(id.clone())
    }

    /// Record a successful engage for `id`; `true` only when it *was* refusing, so
    /// a recovery is reported once and a display that never failed stays silent.
    pub(crate) fn note_success(&mut self, id: &StableDisplayId) -> bool {
        self.refusing.remove(id)
    }
}

/// The pure decision core: tracks which displays currently have gamma engaged
/// (and at what factor) and reconciles that against each apply batch.
#[derive(Debug, Default)]
pub(crate) struct GammaCoordinator {
    /// Resolved id → the factor the sink confirmed is **live**, as raw bits for
    /// exact (lint-free, `NaN`-free — the factor is always a clamped `clamp_gamma`
    /// output) compare. A refused engage is deliberately absent: recording it would
    /// make the coordinator claim a ramp the OS rejected, suppress the retry that
    /// could recover it, and later issue a restore for a ramp that was never set.
    engaged: BTreeMap<StableDisplayId, u32>,
}

impl GammaCoordinator {
    /// Reconcile the gamma channel with one apply batch.
    ///
    /// Engages every command carrying a gamma factor (only when newly present or
    /// the factor changed, so an unchanged ramp is never rewritten), and restores
    /// every previously-engaged display that no longer carries one. Commands with
    /// `gamma: None` (the default overlay path, every HDR/unknown display, and
    /// every factor this platform's OS would refuse — `effective_mode` and
    /// `dimming::plan` force both onto the overlay) never engage the ramp.
    ///
    /// A refused engage is **not** recorded, so the next batch tries again (that is
    /// the only way a display recovers) and no restore is later issued for a ramp
    /// that was never written. It stays in the "asking for gamma" set either way,
    /// so a refusal never tears down a ramp that *is* live at an older factor.
    pub(crate) fn apply(&mut self, commands: &[DimCommand], sink: &mut impl GammaSink) {
        let mut present: BTreeSet<StableDisplayId> = BTreeSet::new();
        for cmd in commands {
            let Some(factor) = cmd.gamma else { continue };
            present.insert(cmd.id.clone());
            let bits = factor.to_bits();
            if self.engaged.get(&cmd.id) != Some(&bits) && sink.engage(&cmd.id, factor) {
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;

    use duja_core::dimmer::{DimCommand, GAMMA_FLOOR};
    use duja_core::id::StableDisplayId;
    use duja_dimmer::{GammaDisplay, ScreenStateGuard};
    use tracing::{info, warn};

    use super::{GammaCoordinator, GammaSink, RefusalLog, retain_failed_engagements};

    /// Resolve a resolved display id to its GDI device name (e.g. `\\.\DISPLAY1`).
    type DeviceResolver = Box<dyn FnMut(&StableDisplayId) -> Option<String>>;

    /// The real gamma sink: correlates ids to GDI devices and drives the
    /// crash-marker-guarded ramp.
    struct GuardSink {
        guard: ScreenStateGuard,
        resolve: DeviceResolver,
        /// Resolved id → the GDI device name engaged for it, so a later restore
        /// targets the exact device the engage used (device names can change
        /// across a hot-plug). Only ids whose ramp write **succeeded** are here:
        /// the guard likewise does not record an untouched display, so the two
        /// stay in step.
        engaged: BTreeMap<StableDisplayId, String>,
        /// Per-display once-only logging for a refused ramp (see [`RefusalLog`]).
        refusals: RefusalLog,
    }

    impl GammaSink for GuardSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool {
            debug_assert!(
                (GAMMA_FLOOR..=1.0).contains(&factor),
                "gamma factor {factor} out of range; HDR/unknown must force overlay"
            );
            let Some(device) = (self.resolve)(id) else {
                if self.refusals.note_refusal(id) {
                    warn!(
                        id = %id.as_str(),
                        "no GDI device for gamma display; skipping ramp (logged once)"
                    );
                }
                return false;
            };
            if let Err(e) = self
                .guard
                .engage_gamma(GammaDisplay::from_device_name(&device), factor)
            {
                // Once per display, not once per frame: a slider drag re-plans every
                // frame, and this warning shipped 349 times in one user's log.
                if self.refusals.note_refusal(id) {
                    warn!(
                        id = %id.as_str(), device, factor, error = %e,
                        "gamma engage refused; falling back to no ramp for this display \
                         (logged once until it succeeds)"
                    );
                }
                return false;
            }
            if self.refusals.note_success(id) {
                info!(id = %id.as_str(), device, "gamma engage recovered");
            }
            self.engaged.insert(id.clone(), device);
            true
        }

        fn restore(&mut self, id: &StableDisplayId) {
            if let Some(device) = self.engaged.remove(id)
                && let Err(e) = self.guard.restore_display(&device)
            {
                warn!(id = %id.as_str(), device, error = %e, "gamma restore failed");
            }
        }

        fn restore_all(&mut self) -> bool {
            // Restore FIRST, then reconcile `engaged` against what actually still
            // holds a ramp: `restore_now` retains any display whose restore failed
            // in the guard's `touched`, so `engaged` must keep those ids too (and
            // drop only the ones that restored cleanly) rather than blanket-clear
            // and diverge from the guard.
            let report = self.guard.restore_now();
            let failed_devices: BTreeSet<String> = report
                .failed
                .iter()
                .map(|(device, _err)| device.clone())
                .collect();
            retain_failed_engagements(&mut self.engaged, &failed_devices);
            if !report.is_clean() {
                warn!(failed = report.failed.len(), "some gamma restores failed");
            }
            report.is_clean()
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
                    refusals: RefusalLog::default(),
                },
            }
        }

        /// Drive the gamma channel for one apply batch.
        pub(crate) fn apply(&mut self, commands: &[DimCommand]) {
            self.coord.apply(commands, &mut self.sink);
        }

        /// Restore every engaged display and clear the crash marker, returning
        /// whether every restore succeeded (`true` = clean).
        ///
        /// A `false` return means a gamma ramp could not be reset; the guard has
        /// KEPT the crash marker so the next launch recovers, and the caller must
        /// not force-remove it.
        pub(crate) fn restore_all(&mut self) -> bool {
            self.coord.forget_all();
            self.sink.restore_all()
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

            let clean = backend.restore_all();
            assert!(
                clean,
                "a restore that leaves nothing engaged must report clean"
            );
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

        /// A GDI device name that does not exist, so `CreateDCW` — and therefore
        /// the ramp write — always fails. Lets the refusal path be exercised
        /// headlessly, with no display and no gamma change on the real screen.
        const BOGUS_DEVICE: &str = r"\\.\DUJA_BOGUS_GAMMA_DEVICE";

        fn bogus_sink(marker: std::path::PathBuf) -> GuardSink {
            GuardSink {
                guard: ScreenStateGuard::new(Some(marker)),
                resolve: Box::new(|_id| Some(BOGUS_DEVICE.to_owned())),
                engaged: BTreeMap::new(),
                refusals: RefusalLog::default(),
            }
        }

        #[test]
        fn a_refused_ramp_reports_failure_and_is_not_recorded_as_engaged() {
            // Bug 1 at the site it shipped from: `GuardSink::engage` logged the
            // failure and then inserted the display into `engaged` anyway, and it
            // returned nothing, so the coordinator believed the ramp was live. It
            // must now report the refusal and record nothing.
            assert!(
                duja_dimmer::set_gamma(&GammaDisplay::from_device_name(BOGUS_DEVICE), 0.6).is_err(),
                "precondition: a bogus GDI device must fail the ramp write"
            );
            let dir = tempfile::tempdir().expect("tempdir");
            let mut sink = bogus_sink(dir.path().join("gamma.dirty"));

            assert!(
                !sink.engage(&id("A"), 0.6),
                "a refused ramp must report that it is not live"
            );
            assert!(
                sink.engaged.is_empty(),
                "a refused ramp must not be tracked as engaged"
            );
        }

        #[test]
        fn a_refused_ramp_is_retried_but_latched_so_it_warns_once() {
            // Bug 3, at the site that emitted the 349 lines. The coordinator retries
            // a refused display on every batch (that is the only way it recovers),
            // so the suppression has to live in the sink. Twelve attempts — the size
            // of one 450 ms drag burst in the report — all report the refusal...
            let dir = tempfile::tempdir().expect("tempdir");
            let mut sink = bogus_sink(dir.path().join("gamma.dirty"));
            for attempt in 0..12 {
                assert!(
                    !sink.engage(&id("A"), 0.6),
                    "attempt {attempt} must still report the refusal"
                );
            }
            // ...but the log-worthy transition was consumed by the FIRST one, so no
            // later attempt is new. Pre-fix, every one of the twelve warned.
            assert!(
                !sink.refusals.note_refusal(&id("A")),
                "the refusal must already be latched after the first attempt"
            );
            // The latch is also what makes a later recovery reportable exactly once.
            assert!(sink.refusals.note_success(&id("A")));
            assert!(!sink.refusals.note_success(&id("A")));
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
    ///
    /// `refuse` makes every engage report "the ramp is not live", which is what a
    /// real OS refusal looks like from the coordinator's side.
    #[derive(Default)]
    struct FakeSink {
        engaged: Vec<(StableDisplayId, f32)>,
        restored: Vec<StableDisplayId>,
        refuse: bool,
    }

    impl GammaSink for FakeSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool {
            self.engaged.push((id.clone(), factor));
            !self.refuse
        }
        fn restore(&mut self, id: &StableDisplayId) {
            self.restored.push(id.clone());
        }
        fn restore_all(&mut self) -> bool {
            true
        }
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

    // --- A refused engage must not be recorded as engaged -------------------

    #[test]
    fn a_refused_engage_is_not_recorded_and_is_retried() {
        // Bug 1's coordinator half: the old code called `sink.engage` and then
        // inserted the factor unconditionally, so a display whose ramp the OS
        // rejected was believed engaged — which also suppressed every retry, since
        // the recorded factor matched the next batch's. RED both ways: pre-fix the
        // second apply issues no engage at all.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink {
            refuse: true,
            ..FakeSink::default()
        };
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        assert_eq!(
            sink.engaged.len(),
            2,
            "a refused ramp must be retried on the next batch, not assumed live"
        );
    }

    #[test]
    fn a_refused_engage_is_never_restored() {
        // Nothing was written, so nothing may be un-written: a restore for a ramp
        // that never took effect is a spurious OS call and, on the guard side, a
        // `touched` entry that never existed. Pre-fix the display was in `engaged`,
        // so dropping it issued a restore.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink {
            refuse: true,
            ..FakeSink::default()
        };
        coord.apply(&[cmd("A", Some(0.6))], &mut sink);
        coord.apply(&[], &mut sink);
        assert!(
            sink.restored.is_empty(),
            "a ramp that was refused must not be restored"
        );
    }

    #[test]
    fn a_refusal_does_not_tear_down_a_ramp_that_is_already_live() {
        // The display keeps asking for gamma, so it stays in the "present" set even
        // while a *new* factor is being refused: the older, live ramp must not be
        // restored out from under it (that would flash the screen bright).
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        coord.apply(&[cmd("A", Some(0.6))], &mut sink); // accepted, now live
        sink.refuse = true;
        coord.apply(&[cmd("A", Some(0.55))], &mut sink); // refused
        assert!(
            sink.restored.is_empty(),
            "a refused change must leave the live ramp alone"
        );
        // ...and when the display finally leaves the gamma path, the ramp that IS
        // live (0.6) is the one restored.
        sink.refuse = false;
        coord.apply(&[cmd("A", None)], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    // --- Once-per-display refusal logging -----------------------------------

    #[test]
    fn refusal_log_reports_only_the_transitions() {
        // Bug 3: 349 identical warnings, a dozen inside one 450 ms drag. Only the
        // edges are worth a line.
        let mut log = RefusalLog::default();
        assert!(log.note_refusal(&id("A")), "the first refusal is news");
        for _ in 0..11 {
            assert!(!log.note_refusal(&id("A")), "a repeat refusal is not");
        }
        assert!(log.note_success(&id("A")), "the recovery is news");
        assert!(
            !log.note_success(&id("A")),
            "a display that was not refusing stays silent"
        );
        // And it latches again after recovering, so a second episode is reported.
        assert!(log.note_refusal(&id("A")));
    }

    #[test]
    fn refusal_log_tracks_displays_independently() {
        let mut log = RefusalLog::default();
        assert!(log.note_refusal(&id("A")));
        assert!(
            log.note_refusal(&id("B")),
            "B's first refusal is news even though A is already refusing"
        );
        assert!(log.note_success(&id("A")));
        assert!(
            !log.note_refusal(&id("B")),
            "A recovering must not un-latch B"
        );
    }

    #[test]
    fn restore_reconciliation_retains_failed_and_drops_restored() {
        // Fix 3: after a restore pass, the sink's engage map (id → device) must
        // mirror the guard's `touched`: an id whose device restore FAILED stays
        // engaged (its ramp is still live), one whose device restored cleanly is
        // dropped. Device names map back to ids via the engage map itself. Pure,
        // so it covers the reconciliation without a real (failing) GDI guard.
        use std::collections::{BTreeMap, BTreeSet};
        let mut engaged: BTreeMap<StableDisplayId, String> = BTreeMap::new();
        engaged.insert(id("A"), r"\\.\DISPLAY1".to_owned()); // restored → drop
        engaged.insert(id("B"), r"\\.\DISPLAY2".to_owned()); // failed → retain
        let failed: BTreeSet<String> = std::iter::once(r"\\.\DISPLAY2".to_owned()).collect();

        retain_failed_engagements(&mut engaged, &failed);

        assert_eq!(engaged.len(), 1, "only the failed id is retained");
        assert!(
            engaged.contains_key(&id("B")),
            "the id whose restore failed stays engaged (mirrors the guard)"
        );
        assert!(
            !engaged.contains_key(&id("A")),
            "the id whose restore succeeded is dropped"
        );
    }

    #[test]
    fn restore_reconciliation_clears_all_when_every_restore_succeeds() {
        // The clean path: no failures ⇒ the engage map empties, matching a guard
        // whose `touched` drained completely.
        use std::collections::{BTreeMap, BTreeSet};
        let mut engaged: BTreeMap<StableDisplayId, String> = BTreeMap::new();
        engaged.insert(id("A"), r"\\.\DISPLAY1".to_owned());
        engaged.insert(id("B"), r"\\.\DISPLAY2".to_owned());

        retain_failed_engagements(&mut engaged, &BTreeSet::new());

        assert!(
            engaged.is_empty(),
            "a clean restore forgets every engagement"
        );
    }
}
