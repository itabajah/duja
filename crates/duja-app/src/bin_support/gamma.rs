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
//! - the real sink is per-platform, and both correlate a resolved display id to
//!   its display-surface token before driving the OS: Windows' `GuardSink` turns
//!   the token into a GDI device name and drives `ScreenStateGuard`'s
//!   `engage_gamma` / `restore_display`, which write and clear the crash marker;
//!   macOS' `MacSink` parses the token as a `CGDirectDisplayID` and calls
//!   `duja_dimmer`'s `set_gamma` / `restore_identity` directly, with **no** guard
//!   and no marker (see the crash-safety note below).
//! - `GammaBackend` bundles the two and is what the tray owns. Both platforms'
//!   versions expose the same constructor and methods, so the tray wires the
//!   gamma channel without a `cfg`.
//!
//! # Why macOS has no crash marker — and how well that is actually established
//!
//! A Windows gamma ramp survives the process that set it, so a crash leaves the
//! screen dimmed with nothing left to undo it — hence `ScreenStateGuard`, the
//! marker file, and the recover-on-launch path. macOS is believed not to need
//! that: the window server is widely observed to restore a process's transfer
//! tables when the process exits, so a crash self-heals. The macOS sink therefore
//! holds no guard, and `GammaBackend::new`'s marker path is accepted and ignored
//! there — keeping one signature rather than pushing a `cfg` into the tray.
//!
//! **"Widely observed", not "documented".** Worth stating plainly, because this
//! assumption is the sole reason a never-brick net is absent. Apple's
//! `CGDirectDisplay.h` says nothing about restore-on-exit for
//! `CGSetDisplayTransferByFormula`; the nearest official sentence — *"When your
//! application terminates, the display arrangement returns to the current settings
//! in Displays preferences"* — is about display **configuration**, not transfer
//! tables. Apple's own `MacGamma` sample saves and restores explicitly, and Gamma
//! Control's changelog records moving to reset on quit *"instead of counting on
//! macOS to do it for us"*. Nothing here contradicts the belief; nothing confirms
//! it either, and Duja has no Mac to settle it on.
//!
//! What that costs if the assumption is wrong on some configuration: because the
//! marker is never written, `startup::recover_from_crash_marker` can never fire on
//! macOS, so there is **no automatic recovery** — only `duja --restore`, run by a
//! user on a screen they may not be able to read. Tracked in `docs/debt.md`; the
//! hard-kill (SIGKILL / force-quit) case in particular is assumed, not verified.
//!
//! Before this module existed, `dim_mode = "gamma"` was a silent no-op and the
//! crash-marker machinery was dead code (P4 gate Finding 2): the planner emitted
//! `DimCommand { gamma: Some(_) }` but nothing ever engaged a ramp.
//!
//! # Add the new dimming before removing the old
//!
//! One apply batch drives **two** mechanisms, and on Windows a display can switch
//! between them mid-drag: `dimming::plan` substitutes an overlay for any gamma
//! factor the OS would refuse, so crossing that threshold means one channel takes
//! over from the other. Doing that in the wrong order is visible.
//!
//! The overlay backend's `apply` **blocks** and turns alpha 0 into a window
//! `Destroy`, so "overlay first, then gamma" would tear the overlay down *to
//! completion* and only then engage the ramp — on a floor-5/anchor-40 display,
//! nudging the slider 21 → 22 would flash the screen to 43 % (twice the requested
//! brightness) on every drag through the middle of the sub-floor zone. Microsoft
//! documents `SetDeviceGammaRamp` as taking up to 200 ms on some hardware, so the
//! gap is perceptible.
//!
//! [`apply_dimming_batch`] therefore sequences the batch as **engage new ramps →
//! overlay diff → restore stale ramps**: whichever direction the threshold is
//! crossed, the two dims briefly overlap instead of briefly vanishing, so the
//! artifact is a short dip rather than a bright spike. The overlay diff is one
//! blocking call that both creates and destroys, so the dip cannot be removed from
//! this layer — but a screen that is momentarily too dark is the right failure
//! direction for a dimmer.

// RATIONALE: the pure coordinator/trait stay cross-platform so their unit tests
// run on every CI OS, and the whole channel — the macOS `GammaBackend` included —
// has exactly one consumer: the tray, which is still `cfg(windows)`. So on macOS
// the sink built in this file is written but not yet called, which is also why the
// re-export needs `unused_imports` and not just `dead_code`.
//
// Drop BOTH allows for macOS in the PR that un-gates the tray, when the consumer
// actually appears — the tightened form is
// `not(any(windows, target_os = "macos"))`. Leaving them on past that point would
// hide a genuinely unwired sink, which is the failure this file already had once
// (the P4 gate found `dim_mode = "gamma"` was a silent no-op).
#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::{BTreeMap, BTreeSet};

use duja_core::dimmer::{DimCommand, Dimmer, DimmerError};
use duja_core::id::StableDisplayId;

/// A per-display gamma engage/restore executor.
///
/// Abstracts the OS gamma ramp so [`GammaCoordinator`]'s decisions are testable
/// with a fake. The real implementations are Windows' `GuardSink` and macOS'
/// `MacSink`.
pub(crate) trait GammaSink {
    /// Engage (or re-engage) gamma dimming for `id` at `factor` (`1.0` = identity,
    /// down to `GAMMA_FLOOR`), returning whether the OS **reported the write as
    /// accepted**.
    ///
    /// `false` means the write was not accepted — the OS refused it, or the display
    /// could not be correlated to a gamma device. The coordinator must not then
    /// record the factor as engaged: as far as anything observable goes it never
    /// took effect, so there is nothing to restore later and a retry on the next
    /// batch is the only route back.
    ///
    /// # This is *reported* acceptance, not observed liveness
    ///
    /// `true` deliberately claims only what the OS said, because on **both**
    /// platforms that is all that is knowable from the write alone. This was
    /// originally written as a Windows property; it is not.
    ///
    /// - **Windows.** Microsoft documents `SetDeviceGammaRamp` as being able to
    ///   *"fail silently (that is, it returns TRUE, but it doesn't set your
    ///   ramp)"* when a ramp violates its heuristics.
    /// - **macOS.** `CGSetDisplayTransferByFormula` has the same shape, and it is
    ///   not hypothetical: Apple's own forum threads carry a DTS-acknowledged
    ///   report that the call returns `kCGErrorSuccess` while *"the display's
    ///   actual gamma curve remains unchanged despite the API reporting successful
    ///   completion"*, with a reproduction as ordinary as leaving **"Automatically
    ///   adjust brightness" on** (the default on Apple Silicon laptops). f.lux,
    ///   Lunar, `BetterDisplay` and `MonitorControl` are all affected.
    ///
    /// The Windows cure — a `GetDeviceGammaRamp` read-back comparison, ADR-0002's
    /// verify-by-readback idiom — **does not transfer**: on the reported macOS
    /// hardware `CGGetDisplayTransferByTable` reads back exactly the values just
    /// written while nothing changes on screen, so a readback would confirm a ramp
    /// that is not live. See `docs/debt.md`.
    fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool;
    /// Restore one display previously engaged, back to whatever this platform
    /// treats as "not dimmed by Duja".
    ///
    /// Deliberately not "restore identity gamma": the two implementations differ
    /// and it is user-visible. Windows writes the identity ramp; macOS writes
    /// identity through `CGSetDisplayTransferByFormula`, which on a **calibrated**
    /// display is *not* the same as its `ColorSync` profile — see `MacSink::restore`.
    fn restore(&mut self, id: &StableDisplayId);
    /// Restore every engaged display for a clean teardown, returning whether the
    /// restore was clean (`true`).
    ///
    /// On Windows this also clears the crash marker, and a `false` return means at
    /// least one ramp could not be reset and the marker was **kept**, so the caller
    /// must not force-remove it. macOS has no marker and cannot report a failure —
    /// its `false` is unreachable (see `MacSink::restore_all`), so a caller must
    /// not read `true` there as evidence that anything was verified.
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

/// Recover a `CGDirectDisplayID` from a macOS **gamma** token.
///
/// The token is `backend::DisplayGeom`'s `gamma_token`: on macOS this display's
/// own `CGDirectDisplayID` rendered in decimal. Emphatically **not** the
/// `surface_token` next to it, which names the mirror-set master and may belong to
/// a display Duja never enumerated — `BoundsMap` keeps the two behind separately
/// named accessors precisely so this sink cannot take the wrong one.
///
/// This is the whole of the macOS sink's correlation step, isolated so the
/// rejection rule is pinned on every CI lane rather than only where a Mac can run
/// it.
///
/// Returns `None` for anything that is not a plain decimal `u32`, and the caller
/// treats that exactly as "this display has no gamma device": it refuses the
/// engage rather than dimming something else. Three ways that matters:
///
/// - the *other* platform's token is a GDI device name (`\.\DISPLAY1`), so a
///   wrong-platform token must fail closed rather than parse to a plausible number;
/// - a lenient parse of a leading-digits string (`"1abc"` → `1`) would silently
///   address display 1;
/// - `0` is `kCGNullDirectDisplay`, never a valid display, so it is rejected too.
///   That one is not hypothetical hygiene: it is what a swapped or unset id looks
///   like, and Core Graphics would otherwise be handed the null display.
#[cfg(any(test, target_os = "macos"))]
fn display_id_from_token(token: &str) -> Option<u32> {
    match token.parse::<u32>() {
        // `kCGNullDirectDisplay`: a real display never has id 0.
        Ok(0) | Err(_) => None,
        Ok(id) => Some(id),
    }
}

/// Once-per-reason logging state for a refused gamma engage.
///
/// A slider drag re-plans on every frame, so a display whose ramp the OS refuses
/// emits one warning per apply: the reported log holds **349** identical lines,
/// up to a dozen inside a single 450 ms drag. This remembers *why* each display was
/// last refused, so a line is emitted when that changes — the first refusal, a
/// refusal for a **different** reason, and a recovery — rather than per frame.
///
/// Keying on the reason and not just the id matters because a display has more than
/// one way to fail (no GDI device at all, versus a ramp the driver rejects) and an
/// id-only latch would swallow the second one entirely, hiding a genuinely
/// different fault behind an already-latched one.
///
/// Pure (a map of ids to reasons), so the suppression rule is unit-tested on every
/// target without a failing ramp.
#[derive(Debug, Default)]
pub(crate) struct RefusalLog {
    /// Display → the reason its most recent engage attempt was refused.
    refusing: BTreeMap<StableDisplayId, String>,
}

impl RefusalLog {
    /// Record a refused engage for `id` with reason `reason`; `true` when that is
    /// news — a first refusal, or a *different* reason than last time.
    pub(crate) fn note_refusal(&mut self, id: &StableDisplayId, reason: &str) -> bool {
        match self.refusing.get(id) {
            Some(previous) if previous == reason => false,
            _ => {
                self.refusing.insert(id.clone(), reason.to_owned());
                true
            }
        }
    }

    /// Record a successful engage for `id`; `true` only when it *was* refusing, so
    /// a recovery is reported once and a display that never failed stays silent.
    pub(crate) fn note_success(&mut self, id: &StableDisplayId) -> bool {
        self.refusing.remove(id).is_some()
    }

    /// The reason `id` is currently refusing, if it is. Lets a test assert on the
    /// latch without reconstructing an OS error string it cannot know.
    #[cfg(test)]
    pub(crate) fn reason_for(&self, id: &StableDisplayId) -> Option<&str> {
        self.refusing.get(id).map(String::as_str)
    }
}

/// Drive one apply batch across **both** dimming mechanisms, in the order that
/// never brightens the screen mid-transition.
///
/// New ramps are engaged first, the overlay diff runs second, and stale ramps are
/// restored last — see the [module docs](self) for why that ordering is the whole
/// point, and what it costs. `overlays` is `None` when no dimmer backend could be
/// started (a documented degradation), in which case the gamma channel still runs.
///
/// Returns the overlay backend's own result; the gamma phases report their failures
/// through the sink's logging, since a refused ramp is per-display and must not
/// abort the batch.
pub(crate) fn apply_dimming_batch(
    commands: &[DimCommand],
    coord: &mut GammaCoordinator,
    sink: &mut impl GammaSink,
    overlays: Option<&mut dyn Dimmer>,
) -> Result<(), DimmerError> {
    coord.engage_phase(commands, sink);
    let outcome = match overlays {
        Some(dimmer) => dimmer.apply(commands),
        None => Ok(()),
    };
    coord.restore_phase(commands, sink);
    outcome
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
    /// Phase 1 of a batch: engage every ramp the batch asks for.
    ///
    /// Engages every command carrying a gamma factor (only when newly present or
    /// the factor changed, so an unchanged ramp is never rewritten). Commands with
    /// `gamma: None` (the default overlay path, every HDR/unknown display, and
    /// every factor this platform's OS would refuse — `effective_mode` and
    /// `dimming::plan` force both onto the overlay) never engage the ramp.
    ///
    /// A refused engage is **not** recorded, so the next batch tries again (that is
    /// the only way a display recovers) and no restore is later issued for a ramp
    /// that was never written. The display is still counted as *asking* for gamma
    /// (see [`restore_phase`](Self::restore_phase)), so a refusal never tears down a
    /// ramp that is live at an older factor.
    ///
    /// Runs **before** the overlay diff so the two dims briefly overlap rather than
    /// briefly vanish — see the [module docs](self).
    pub(crate) fn engage_phase(&mut self, commands: &[DimCommand], sink: &mut impl GammaSink) {
        for cmd in commands {
            let Some(factor) = cmd.gamma else { continue };
            let bits = factor.to_bits();
            if self.engaged.get(&cmd.id) != Some(&bits) && sink.engage(&cmd.id, factor) {
                self.engaged.insert(cmd.id.clone(), bits);
            }
        }
    }

    /// Phase 2 of a batch: restore every ramp the batch no longer asks for.
    ///
    /// A previously-engaged display whose command has dropped its gamma factor (it
    /// moved to the overlay, or above the sub-floor zone) or which has left the
    /// batch entirely (unplugged) is reset to identity gamma.
    ///
    /// Runs **after** the overlay diff, so the replacement dim is already on screen
    /// before this one is removed.
    pub(crate) fn restore_phase(&mut self, commands: &[DimCommand], sink: &mut impl GammaSink) {
        let present: BTreeSet<&StableDisplayId> = commands
            .iter()
            .filter(|cmd| cmd.gamma.is_some())
            .map(|cmd| &cmd.id)
            .collect();
        let dropped: Vec<StableDisplayId> = self
            .engaged
            .keys()
            .filter(|id| !present.contains(id))
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
    use duja_core::dimmer::{Dimmer, DimmerError};
    use duja_core::id::StableDisplayId;
    use duja_dimmer::{GammaDisplay, ScreenStateGuard};
    use tracing::{debug, warn};

    use super::{
        GammaCoordinator, GammaSink, RefusalLog, apply_dimming_batch, retain_failed_engagements,
    };

    /// Resolve a resolved display id to its GDI device name (e.g. `\\.\DISPLAY1`).
    type DeviceResolver = Box<dyn FnMut(&StableDisplayId) -> Option<String>>;

    /// The [`RefusalLog`] reason for "this id has no GDI device", kept distinct from
    /// any OS error text so the two failure shapes each get their own log line.
    const NO_DEVICE_REASON: &str = "no GDI device for this display";

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
                if self.refusals.note_refusal(id, NO_DEVICE_REASON) {
                    warn!(
                        id = %id.as_str(),
                        "no GDI device for gamma display; skipping ramp \
                         (logged once until the reason changes)"
                    );
                }
                return false;
            };
            if let Err(e) = self
                .guard
                .engage_gamma(GammaDisplay::from_device_name(&device), factor)
            {
                // Once per reason, not once per frame: a slider drag re-plans every
                // frame, and this warning shipped 349 times in one user's log.
                let reason = e.to_string();
                if self.refusals.note_refusal(id, &reason) {
                    warn!(
                        id = %id.as_str(), device, factor, error = %reason,
                        "gamma engage refused; no ramp for this display \
                         (logged once until the reason changes)"
                    );
                }
                return false;
            }
            if self.refusals.note_success(id) {
                // `debug`, not `info`: a driver that flaps would otherwise emit two
                // lines per frame where the pre-fix code emitted one. The refusal
                // above is the line that matters at WARN.
                debug!(id = %id.as_str(), device, "gamma engage accepted again");
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

        /// Drive one apply batch across the gamma channel **and** `overlays`, in the
        /// order that never brightens the screen mid-transition.
        ///
        /// Delegates to the cross-platform [`apply_dimming_batch`], which owns the
        /// sequencing (and its tests) — so the ordering is pinned on every CI lane
        /// rather than only where a real `GuardSink` can be built.
        pub(crate) fn apply_batch(
            &mut self,
            commands: &[DimCommand],
            overlays: Option<&mut dyn Dimmer>,
        ) -> Result<(), DimmerError> {
            apply_dimming_batch(commands, &mut self.coord, &mut self.sink, overlays)
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
            backend
                .apply_batch(&[gamma_cmd("A", 0.6)], None)
                .expect("no overlay backend ⇒ no failure");
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

            backend
                .apply_batch(&[gamma_cmd("A", 0.6)], None)
                .expect("no overlay backend ⇒ no failure");
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
            // later attempt with the SAME reason is new. Pre-fix, every one of the
            // twelve warned. The reason is the real `CreateDCW` failure text, so it
            // has to be read back out of the log rather than reconstructed here.
            let reason = sink
                .refusals
                .reason_for(&id("A"))
                .expect("the refusal is latched")
                .to_owned();
            assert!(
                !sink.refusals.note_refusal(&id("A"), &reason),
                "the refusal must already be latched after the first attempt"
            );
            // ...but a DIFFERENT failure on the same display is still news, so one
            // fault can never hide behind another.
            assert!(
                sink.refusals.note_refusal(&id("A"), NO_DEVICE_REASON),
                "a different reason on the same display must be logged"
            );
            // The latch is also what makes a later recovery reportable exactly once.
            assert!(sink.refusals.note_success(&id("A")));
            assert!(!sink.refusals.note_success(&id("A")));
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use platform::GammaBackend;

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use duja_core::dimmer::{DimCommand, Dimmer, DimmerError, GAMMA_FLOOR};
    use duja_core::id::StableDisplayId;
    use duja_dimmer::GammaDisplay;
    use tracing::{debug, warn};

    use super::{
        GammaCoordinator, GammaSink, RefusalLog, apply_dimming_batch, display_id_from_token,
    };

    /// Resolve a resolved display id to its **gamma** token — on macOS this
    /// display's own `CGDirectDisplayID` in decimal (see `backend::DisplayGeom`).
    /// The tray wires this from `BoundsMap::gamma_token_for`, never
    /// `surface_token_for`.
    type DeviceResolver = Box<dyn FnMut(&StableDisplayId) -> Option<String>>;

    /// [`RefusalLog`] reason: this id carries no gamma token at all — a
    /// `DisplayServices` panel, which has none by construction.
    ///
    /// Deliberately **separate** from [`BAD_TOKEN_REASON`]: `RefusalLog` latches per
    /// (id, reason), so folding the two correlation failures into one string would
    /// let an already-latched "no token" swallow a later "the token is garbage" on
    /// the same id — which is exactly the hiding-a-second-fault behaviour the latch
    /// documents itself as preventing.
    const NO_TOKEN_REASON: &str = "no gamma token for this display";

    /// [`RefusalLog`] reason: a token was present but is not a `CGDirectDisplayID`.
    ///
    /// Should be unreachable — `backend::discover_ddc` stamps
    /// `cg_display_id.to_string()` — so seeing this in a log means the wrong value
    /// reached the gamma channel, e.g. a surface token or a wrong-platform string.
    const BAD_TOKEN_REASON: &str = "gamma token is not a CoreGraphics display id";

    /// The real macOS gamma sink: correlates ids to `CGDirectDisplayID`s and drives
    /// Core Graphics' transfer formula.
    ///
    /// The Windows twin wraps every write in a `ScreenStateGuard` because a Windows
    /// ramp outlives the process. Here there is no guard and no marker: the window
    /// server restores this process's transfer tables when it exits, so the only
    /// state worth tracking is what *this* run engaged, for the in-process restore.
    struct MacSink {
        resolve: DeviceResolver,
        /// Resolved id → the `CGDirectDisplayID` engaged for it, so a later restore
        /// targets the exact display the engage used (ids are re-issued across a
        /// hot-plug). Only ids whose write **succeeded** are here.
        engaged: BTreeMap<StableDisplayId, u32>,
        /// Per-display once-only logging for a refused ramp (see [`RefusalLog`]).
        refusals: RefusalLog,
    }

    impl MacSink {
        /// Resolve `id` to a live `CGDirectDisplayID`, logging (once per reason) and
        /// returning `None` when it has no usable one.
        fn gamma_display_for(&mut self, id: &StableDisplayId) -> Option<u32> {
            let Some(token) = (self.resolve)(id) else {
                if self.refusals.note_refusal(id, NO_TOKEN_REASON) {
                    warn!(
                        id = %id.as_str(),
                        "no gamma token for this display; skipping ramp \
                         (logged once until the reason changes)"
                    );
                }
                return None;
            };
            let cg_id = display_id_from_token(&token);
            if cg_id.is_none() && self.refusals.note_refusal(id, BAD_TOKEN_REASON) {
                // The token is in the line: this reason means the *wrong value*
                // reached the gamma channel, and which value it was is the whole
                // diagnostic.
                warn!(
                    id = %id.as_str(), token,
                    "gamma token is not a CoreGraphics display id; skipping ramp \
                     (logged once until the reason changes)"
                );
            }
            cg_id
        }
    }

    impl GammaSink for MacSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool {
            debug_assert!(
                (GAMMA_FLOOR..=1.0).contains(&factor),
                "gamma factor {factor} out of range; HDR/unknown must force overlay"
            );
            let Some(cg_id) = self.gamma_display_for(id) else {
                return false;
            };
            if let Err(e) = duja_dimmer::set_gamma(&GammaDisplay::from_display_id(cg_id), factor) {
                // Once per reason, not once per frame: a slider drag re-plans every
                // frame, and the Windows twin of this warning shipped 349 times in
                // one user's log before it was latched.
                let reason = e.to_string();
                if self.refusals.note_refusal(id, &reason) {
                    warn!(
                        id = %id.as_str(), cg_id, factor, error = %reason,
                        "gamma engage refused; no ramp for this display \
                         (logged once until the reason changes)"
                    );
                }
                return false;
            }
            if self.refusals.note_success(id) {
                debug!(id = %id.as_str(), cg_id, "gamma engage accepted again");
            }
            self.engaged.insert(id.clone(), cg_id);
            true
        }

        /// Write identity gamma to the one display leaving the sub-floor zone.
        ///
        /// # This is not the same end state as [`Self::restore_all`]
        ///
        /// `restore_all` goes through `CGDisplayRestoreColorSyncSettings`, which
        /// returns every display to the **user's `ColorSync` profile**. This path
        /// writes a linear identity transfer function instead, which on a
        /// *calibrated* display is a different thing: the display comes back
        /// un-dimmed but also un-calibrated, and stays that way until Duja quits or
        /// anything triggers a global restore.
        ///
        /// There are **three** options, and all three are worth naming, because the
        /// obvious second one is bad enough to make it look like there are only two:
        ///
        /// 1. What this does. Cheap, per-display, no flicker — loses calibration.
        /// 2. Call the **global** `CGDisplayRestoreColorSyncSettings` and re-engage
        ///    every other display Duja still has dimmed. Honours the profile, but
        ///    momentarily brightens displays the user did not touch — the exact
        ///    artifact [`apply_dimming_batch`]'s ordering exists to avoid.
        /// 3. **Snapshot and restore**, which is what Apple's own `MacGamma` sample
        ///    does: `CGGetDisplayTransferByTable` once, before Duja's first write,
        ///    and `CGSetDisplayTransferByTable` to put it back. Per-display, no
        ///    flicker, preserves calibration — better than 1 on the axis that
        ///    matters, and both symbols are already in the bound `objc2` surface.
        ///
        /// So 3 is the right answer and this is not it. Deferred, not dismissed, and
        /// the costs are real rather than rhetorical: two new `unsafe` blocks with
        /// pointer out-params in `duja-dimmer`'s macOS backend (which today has
        /// exactly one), a `CGDisplayGammaTableCapacity` query plus a 3×N
        /// `CGGammaValue` buffer per engaged display — and that query can answer
        /// **0**, for a display exposing no gamma table, so option 3 needs option 1
        /// as its own fallback rather than replacing it. It also carries a residual:
        /// another app changing the table mid-session gets Duja's older snapshot
        /// written back, still strictly better than identity, which clobbers it
        /// unconditionally.
        ///
        /// And it narrows the divergence rather than removing it. `restore_all`
        /// would still reload the *profile* while this path writes the *snapshot*,
        /// and those differ whenever anything altered the table before Duja started;
        /// a snapshot keyed by `CGDirectDisplayID` also inherits the id-reuse hazard
        /// noted on `engaged`. Writing that FFI blind, for a path unreachable on any
        /// Mac with an EDR panel, is not a trade to make without hardware. See
        /// `docs/debt.md`.
        fn restore(&mut self, id: &StableDisplayId) {
            if let Some(cg_id) = self.engaged.remove(id)
                && let Err(e) = duja_dimmer::restore_identity(&GammaDisplay::from_display_id(cg_id))
            {
                warn!(id = %id.as_str(), cg_id, error = %e, "gamma restore failed");
            }
        }

        /// Reset every display to its `ColorSync` profile and forget the engaged set.
        ///
        /// **Always returns `true`**, and the signature cannot say so. macOS'
        /// `RestoreReport::failed` is hardcoded empty (`CGDisplayRestoreColorSyncSettings`
        /// returns `void` — there is nothing to report), so unlike Windows there is
        /// no per-display outcome to reconcile and no failure to propagate. Clearing
        /// `engaged` unconditionally is therefore correct here and would be a **bug**
        /// on Windows, where a ramp that failed to reset must stay tracked so the
        /// crash marker is kept.
        ///
        /// A caller must not read the `true` as evidence of anything: it means the
        /// call was made. There is deliberately no `if !report.is_clean()` warning
        /// here — that branch is unreachable, and dead code that looks like a guard
        /// is worse than no guard.
        ///
        /// Note the blast radius, which is wider than the Windows twin's: this
        /// resets **every** display, including ones Duja never engaged and whatever
        /// another app (f.lux, a calibration loader) had set. Windows' `restore_now`
        /// touches only what it recorded. Pre-existing — `tray/state.rs` already
        /// calls the same global `duja_dimmer::restore_all()` on quit and on
        /// "Restore screen" — but it is a real difference, not a simplification.
        fn restore_all(&mut self) -> bool {
            // The report is inspected nowhere on purpose: its `failed` list is
            // hardcoded empty upstream, so there is nothing to branch on.
            let _report = duja_dimmer::restore_all();
            self.engaged.clear();
            true
        }
    }

    /// The tray-owned gamma channel: the pure coordinator plus the real sink.
    ///
    /// Unlike the Windows twin, dropping this does **not** restore anything — there
    /// is no guard to run on `Drop`, on the belief that the window server restores
    /// this process's transfer tables when it exits however that happens. See the
    /// module docs for how well that is actually established; it is the assumption
    /// the whole no-marker design rests on. A clean teardown still calls
    /// [`Self::restore_all`] so the screen is right *before* the process goes away.
    pub(crate) struct GammaBackend {
        coord: GammaCoordinator,
        sink: MacSink,
    }

    impl GammaBackend {
        /// Build a gamma channel using `resolve` to map a resolved display id to its
        /// **gamma** token (`BoundsMap::gamma_token_for`).
        ///
        /// `_marker` is the crash-marker path, accepted and **ignored**. It exists so
        /// this constructor is signature-identical to the Windows one and the tray
        /// can wire the gamma channel without a `cfg`; macOS has nothing to record
        /// because a dirty exit is believed to leave no ramp behind (see the module
        /// docs for the strength of that belief). The consequence of ignoring it is
        /// that `startup::recover_from_crash_marker` can never fire on macOS.
        pub(crate) fn new(
            _marker: PathBuf,
            resolve: impl FnMut(&StableDisplayId) -> Option<String> + 'static,
        ) -> Self {
            GammaBackend {
                coord: GammaCoordinator::default(),
                sink: MacSink {
                    resolve: Box::new(resolve),
                    engaged: BTreeMap::new(),
                    refusals: RefusalLog::default(),
                },
            }
        }

        /// Drive one apply batch across the gamma channel **and** `overlays`, in the
        /// order that never brightens the screen mid-transition.
        ///
        /// Delegates to the cross-platform [`apply_dimming_batch`], which owns the
        /// sequencing (and its tests) — so the ordering is pinned on every CI lane
        /// rather than only where a real sink can be built.
        pub(crate) fn apply_batch(
            &mut self,
            commands: &[DimCommand],
            overlays: Option<&mut dyn Dimmer>,
        ) -> Result<(), DimmerError> {
            apply_dimming_batch(commands, &mut self.coord, &mut self.sink, overlays)
        }

        /// Reset every display to its `ColorSync` profile and forget what was engaged.
        ///
        /// Always `true` — see [`MacSink::restore_all`].
        pub(crate) fn restore_all(&mut self) -> bool {
            self.coord.forget_all();
            self.sink.restore_all()
        }
    }

    /// # What these tests do **not** cover — read before trusting the count
    ///
    /// Six tests, and they pin the **refusal** half. Three paths are unpinned, and
    /// naming them is the point: a count that reads as coverage it does not have is
    /// the false assurance this project rates worse than an admitted gap.
    ///
    /// - **The accept path.** Nothing here ever reaches a successful `set_gamma`,
    ///   so `engaged.insert`, `note_success` and the `true` return are unpinned. An
    ///   `engage` that writes the ramp and then returns `false` would pass — and its
    ///   real shape is nasty: a live ramp the coordinator never records, rewritten
    ///   every batch and never restored. This is at **parity with Windows**, whose
    ///   four sink tests all use a bogus device that fails the write.
    /// - **`restore`'s OS call.** `restoring_one_display_forgets_only_that_display`
    ///   pins the bookkeeping; a `restore` that drops the entry and never resets the
    ///   ramp still passes, and the user sees a display that stays dim after the
    ///   slider leaves the sub-floor zone. Also at parity — no Windows test calls
    ///   `GuardSink::restore` either.
    /// - **`restore_all`'s OS call.** Same shape, and this one is *not* at parity:
    ///   Windows pins it through a side effect, because the marker clear happens
    ///   inside `ScreenStateGuard::restore_now`, so skipping the OS restore reds a
    ///   test. macOS has no marker, so the call has no headless observable at all.
    ///
    /// A live smoke would not close the first: on a virtualized runner
    /// `CGSetDisplayTransferByFormula` returning success is *exactly* the
    /// success-without-effect case this module warns about, so the test would pin
    /// "the FFI returned `Ok`" and nothing about a live ramp — strictly weaker than
    /// `an_os_refusal_is_not_recorded_as_engaged`, whose discriminator is an
    /// **error** and therefore positive evidence the call was evaluated. The only
    /// construction that closes all three deterministically is injecting
    /// `set_gamma`/`restore_identity` behind a seam, the same split this module
    /// already uses one level up at [`GammaSink`] — a design change, not a test.
    #[cfg(test)]
    mod tests {
        use super::*;
        use duja_core::dimmer::DisplayBounds;

        /// A `CGDirectDisplayID` that cannot be attached: `CGGetActiveDisplayList`
        /// only ever reports real ids, so Core Graphics rejects a write to this one
        /// with `kCGErrorIllegalArgument`. The macOS twin of the Windows tests'
        /// bogus `\\.\DUJA_TEST` device name — it makes the *real* OS call fail
        /// deterministically, on a runner with no displays at all.
        const UNATTACHED_DISPLAY_ID: &str = "4294967295";

        /// [`UNATTACHED_DISPLAY_ID`] as the `u32` the engaged map stores, so the
        /// seeded tests below cannot drift from the token the others parse.
        const UNATTACHED_ID: u32 = u32::MAX;
        /// A second unattached id, for asserting one display's restore leaves its
        /// siblings alone.
        const OTHER_UNATTACHED_ID: u32 = u32::MAX - 1;

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

        /// A display the resolver cannot map is refused, and nothing is recorded as
        /// engaged — so the next batch retries rather than believing a ramp is live.
        ///
        /// Reds an `engage` that returns `true` without reaching Core Graphics.
        #[test]
        fn a_display_with_no_token_engages_nothing() {
            let mut sink = MacSink {
                resolve: Box::new(|_id| None),
                engaged: BTreeMap::new(),
                refusals: RefusalLog::default(),
            };
            assert!(
                !sink.engage(&id("A"), 0.6),
                "no token ⇒ the write cannot have happened"
            );
            assert!(
                sink.engaged.is_empty(),
                "nothing may be recorded as engaged"
            );
        }

        /// A token that is not a `CGDirectDisplayID` is refused rather than coerced.
        /// `0` is `kCGNullDirectDisplay`; the device name is the other platform's
        /// token, which must never reach this sink.
        #[test]
        fn a_token_that_is_not_a_display_id_engages_nothing() {
            for token in ["0", r"\\.\DISPLAY1", "1abc", ""] {
                let mut sink = MacSink {
                    resolve: Box::new(move |_id| Some(token.to_owned())),
                    engaged: BTreeMap::new(),
                    refusals: RefusalLog::default(),
                };
                assert!(
                    !sink.engage(&id("A"), 0.6),
                    "token {token:?} must be refused"
                );
                assert!(
                    sink.engaged.is_empty(),
                    "token {token:?} recorded an engage"
                );
            }
        }

        /// A well-formed token for a display that is not attached reaches Core
        /// Graphics and is refused **by the OS**, which is the half the two tests
        /// above cannot cover: they short-circuit before the FFI.
        ///
        /// Reds an `engage` that reports success without checking `set_gamma`'s
        /// result — the failure mode that would leave the coordinator believing a
        /// ramp is live and never planning an overlay instead.
        #[test]
        fn an_os_refusal_is_not_recorded_as_engaged() {
            // Precondition, asserted rather than assumed — the Windows twin does the
            // same. Two ways this test could silently become vacuous: Core Graphics
            // accepting the unattached id (and this PR's own headline finding is
            // that it reports success it should not), or `display_id_from_token`
            // tightening until `u32::MAX` is rejected, which would route the engage
            // through the short-circuit path instead of the FFI and still pass.
            assert_eq!(
                display_id_from_token(UNATTACHED_DISPLAY_ID),
                Some(u32::MAX),
                "the probe id must survive the token parse, or this test never \
                 reaches Core Graphics at all"
            );
            assert!(
                duja_dimmer::set_gamma(&GammaDisplay::from_display_id(u32::MAX), 0.6).is_err(),
                "precondition: an unattached display id must fail the ramp write"
            );

            let mut sink = MacSink {
                resolve: Box::new(|_id| Some(UNATTACHED_DISPLAY_ID.to_owned())),
                engaged: BTreeMap::new(),
                refusals: RefusalLog::default(),
            };
            assert!(
                !sink.engage(&id("A"), 0.6),
                "Core Graphics refuses an unattached display id"
            );
            assert!(
                sink.engaged.is_empty(),
                "a refused write must not be recorded as engaged"
            );
        }

        /// The whole channel is inert for a display it cannot address: the batch
        /// still succeeds (a refused ramp is never fatal) and engages nothing.
        ///
        /// Deliberately does **not** assert `restore_all()` is `true`: that is a
        /// documented constant on this platform, so asserting it would pin the doc
        /// rather than the behaviour. What `restore_all` must actually do is
        /// covered below.
        #[test]
        fn a_batch_for_an_unaddressable_display_succeeds_and_engages_nothing() {
            let mut backend = GammaBackend::new(PathBuf::from("ignored"), |_id| None);
            backend
                .apply_batch(&[gamma_cmd("A", 0.6)], None)
                .expect("no overlay backend ⇒ no failure");
            assert!(backend.sink.engaged.is_empty());
        }

        /// Restoring one display drops **that** display from the engaged set and
        /// leaves the others alone.
        ///
        /// The engaged map is seeded directly, because every path that populates it
        /// honestly needs a real display to accept a ramp. That is the point: with
        /// only the refusal tests above, `restore` could be an empty function body
        /// and nothing would notice — the coordinator never asks it to restore
        /// something it never recorded as engaged.
        #[test]
        fn restoring_one_display_forgets_only_that_display() {
            // The resolver is never consulted: `restore` reads the engaged map,
            // not the resolver, which is itself part of the contract.
            let mut sink = MacSink {
                resolve: Box::new(|_id| None),
                engaged: BTreeMap::new(),
                refusals: RefusalLog::default(),
            };
            // Two unattached ids: the Core Graphics write will fail and be logged,
            // which is irrelevant here — the bookkeeping is what is under test.
            sink.engaged.insert(id("A"), UNATTACHED_ID);
            sink.engaged.insert(id("B"), OTHER_UNATTACHED_ID);

            sink.restore(&id("A"));

            assert!(!sink.engaged.contains_key(&id("A")), "A must be forgotten");
            assert!(
                sink.engaged.contains_key(&id("B")),
                "B is still engaged and must not be dropped by A's restore"
            );
        }

        /// A full restore forgets **everything**, so nothing is left believing a
        /// ramp of its own is live.
        ///
        /// Seeded for the same reason as above. Reds a `restore_all` that skips its
        /// `engaged.clear()` — which would strand entries the coordinator has
        /// already forgotten, so a later `restore` for one of them would write to a
        /// display Duja no longer tracks.
        #[test]
        fn a_full_restore_forgets_every_engaged_display() {
            let mut sink = MacSink {
                resolve: Box::new(|_id| None),
                engaged: BTreeMap::new(),
                refusals: RefusalLog::default(),
            };
            sink.engaged.insert(id("A"), UNATTACHED_ID);
            sink.engaged.insert(id("B"), OTHER_UNATTACHED_ID);

            sink.restore_all();

            assert!(
                sink.engaged.is_empty(),
                "a full restore must leave nothing engaged"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duja_core::dimmer::DisplayBounds;
    use std::sync::{Arc, Mutex};

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    fn cmd(serial: &str, gamma: Option<f32>) -> DimCommand {
        DimCommand::new(id(serial), DisplayBounds::new(0, 0, 1920, 1080), 0.0, gamma)
    }

    /// A decimal token recovers its `CGDirectDisplayID` across the valid range.
    #[test]
    fn a_decimal_token_recovers_its_coregraphics_display_id() {
        use super::display_id_from_token as decode;
        assert_eq!(decode("1"), Some(1));
        assert_eq!(decode("724059720"), Some(724_059_720));
        assert_eq!(decode(&u32::MAX.to_string()), Some(u32::MAX));
    }

    /// Anything that is not a plain decimal `u32` must fail **closed**, so the sink
    /// refuses the engage instead of driving some other display's gamma.
    ///
    /// The Windows device name is the case that matters: it is the other platform's
    /// token for the same field, so a lenient parse is how a wrong-platform value
    /// would quietly become a real display id.
    #[test]
    fn a_token_that_is_not_a_display_id_is_rejected_rather_than_coerced() {
        use super::display_id_from_token as decode;
        assert_eq!(decode(r"\\.\DISPLAY1"), None, "a Windows GDI device name");
        // Leading digits must NOT win: `"1abc"` parsing to 1 would address display 1.
        assert_eq!(decode("1abc"), None);
        assert_eq!(decode(""), None);
        assert_eq!(decode(" 7"), None);
        assert_eq!(decode("-1"), None);
        // Wider than a CGDirectDisplayID: truncating would address a real display.
        assert_eq!(decode("4294967296"), None);
        // `kCGNullDirectDisplay`. Parses fine as a `u32`, so only an explicit
        // rejection keeps it out — and it is exactly what an unset or swapped id
        // looks like, which is the reason to fail closed rather than hand Core
        // Graphics the null display.
        assert_eq!(decode("0"), None);
    }

    /// An ordered log of what one batch did, shared by the fake sink and the fake
    /// dimmer so the **interleaving** of the two mechanisms is observable.
    ///
    /// `Arc<Mutex<_>>` rather than `Rc<RefCell<_>>` because [`Dimmer`] requires
    /// [`Send`] (a real backend owns a worker thread).
    type Trace = Arc<Mutex<Vec<String>>>;

    fn note(trace: &Trace, step: String) {
        if let Ok(mut log) = trace.lock() {
            log.push(step);
        }
    }

    /// A fake sink that records every engage/restore call for assertions.
    ///
    /// `refuse` makes every engage report "the OS did not accept the write", which
    /// is what a real refusal looks like from the coordinator's side.
    #[derive(Default)]
    struct FakeSink {
        engaged: Vec<(StableDisplayId, f32)>,
        restored: Vec<StableDisplayId>,
        refuse: bool,
        trace: Trace,
    }

    impl GammaSink for FakeSink {
        fn engage(&mut self, id: &StableDisplayId, factor: f32) -> bool {
            note(&self.trace, format!("engage {}", id.as_str()));
            self.engaged.push((id.clone(), factor));
            !self.refuse
        }
        fn restore(&mut self, id: &StableDisplayId) {
            note(&self.trace, format!("restore {}", id.as_str()));
            self.restored.push(id.clone());
        }
        fn restore_all(&mut self) -> bool {
            true
        }
    }

    /// A fake overlay backend that records only that it ran, so the batch ordering
    /// can be asserted against the gamma phases around it.
    #[derive(Debug)]
    struct FakeDimmer {
        trace: Trace,
    }

    impl Dimmer for FakeDimmer {
        fn apply(&mut self, commands: &[DimCommand]) -> Result<(), DimmerError> {
            note(&self.trace, format!("overlays({})", commands.len()));
            Ok(())
        }
        fn clear(&mut self) -> Result<(), DimmerError> {
            note(&self.trace, "clear".to_owned());
            Ok(())
        }
    }

    /// Drive a batch through the production entry point with **no** overlay backend
    /// (the gamma channel alone) — the shape every coordinator test below wants.
    /// Cannot fail: the `None` arm has nothing to report.
    fn gamma_only(coord: &mut GammaCoordinator, commands: &[DimCommand], sink: &mut FakeSink) {
        apply_dimming_batch(commands, coord, sink, None).expect("no dimmer ⇒ no failure");
    }

    #[test]
    fn gamma_command_engages_on_the_sink() {
        // Regression for P4 gate Finding 2: a gamma-mode sub-floor plan must reach
        // the gamma engage API. Before the fix, nothing ever called it.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", None)], &mut sink);
        assert!(sink.engaged.is_empty());
        assert!(sink.restored.is_empty());
    }

    #[test]
    fn stable_factor_does_not_re_engage() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[cmd("A", Some(0.4))], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[cmd("A", None)], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    #[test]
    fn absent_display_is_restored() {
        // A display that vanishes from the batch entirely (unplugged) is restored.
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    #[test]
    fn independent_displays_engage_and_restore_independently() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(
            &mut coord,
            &[cmd("A", Some(0.6)), cmd("B", Some(0.5))],
            &mut sink,
        );
        assert_eq!(sink.engaged.len(), 2);
        // B drops gamma; A keeps it.
        gamma_only(
            &mut coord,
            &[cmd("A", Some(0.6)), cmd("B", None)],
            &mut sink,
        );
        assert_eq!(sink.restored, vec![id("B")]);
        assert_eq!(sink.engaged.len(), 2, "A must not re-engage on B's change");
    }

    #[test]
    fn forget_all_clears_tracking_without_per_display_restores() {
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        coord.forget_all();
        // After forgetting, an empty batch issues no restore (the backend pairs
        // this with a whole-guard restore instead).
        gamma_only(&mut coord, &[], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink);
        gamma_only(&mut coord, &[], &mut sink);
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
        gamma_only(&mut coord, &[cmd("A", Some(0.6))], &mut sink); // accepted, now live
        sink.refuse = true;
        gamma_only(&mut coord, &[cmd("A", Some(0.55))], &mut sink); // refused
        assert!(
            sink.restored.is_empty(),
            "a refused change must leave the live ramp alone"
        );
        // ...and when the display finally leaves the gamma path, the ramp that IS
        // live (0.6) is the one restored.
        sink.refuse = false;
        gamma_only(&mut coord, &[cmd("A", None)], &mut sink);
        assert_eq!(sink.restored, vec![id("A")]);
    }

    // --- Add the new dimming before removing the old ------------------------

    #[test]
    fn a_batch_engages_new_ramps_before_the_overlay_diff_and_restores_after() {
        // Blocker: the overlay backend's `apply` BLOCKS and turns alpha 0 into a
        // window Destroy, so running it before the gamma engage tears the old dim
        // down to completion and flashes the screen bright (on a floor-5/anchor-40
        // display, slider 21 → 22 would flash to 43 % — twice what was asked for).
        // The batch must therefore add the new dim first and remove the old last.
        // A dims via gamma, B is leaving the gamma path (it crossed onto the
        // overlay), so one batch does both an engage and a restore.
        let trace: Trace = Trace::default();
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink {
            trace: Arc::clone(&trace),
            ..FakeSink::default()
        };
        // Seed: B already has a live ramp.
        gamma_only(&mut coord, &[cmd("B", Some(0.6))], &mut sink);
        if let Ok(mut log) = trace.lock() {
            log.clear();
        }

        let mut dimmer = FakeDimmer {
            trace: Arc::clone(&trace),
        };
        apply_dimming_batch(
            &[cmd("A", Some(0.7)), cmd("B", None)],
            &mut coord,
            &mut sink,
            Some(&mut dimmer),
        )
        .expect("the fake dimmer succeeds");

        let steps = trace.lock().map(|log| log.clone()).unwrap_or_default();
        assert_eq!(
            steps,
            vec![
                format!("engage {}", id("A").as_str()),
                "overlays(2)".to_owned(),
                format!("restore {}", id("B").as_str()),
            ],
            "the new ramp must be engaged before the overlay diff and the stale one \
             restored after it"
        );
    }

    #[test]
    fn a_batch_without_an_overlay_backend_still_drives_both_gamma_phases() {
        // The dimmer is `Option` because spawning it can fail (a documented
        // degradation); gamma must not be skipped along with it.
        let trace: Trace = Trace::default();
        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink {
            trace: Arc::clone(&trace),
            ..FakeSink::default()
        };
        gamma_only(&mut coord, &[cmd("B", Some(0.6))], &mut sink);
        gamma_only(
            &mut coord,
            &[cmd("A", Some(0.7)), cmd("B", None)],
            &mut sink,
        );
        let steps = trace.lock().map(|log| log.clone()).unwrap_or_default();
        assert_eq!(
            steps,
            vec![
                format!("engage {}", id("B").as_str()),
                format!("engage {}", id("A").as_str()),
                format!("restore {}", id("B").as_str()),
            ]
        );
    }

    #[test]
    fn the_overlay_backends_failure_is_returned_but_does_not_skip_the_restores() {
        // A wedged overlay worker must not leave a stale ramp engaged forever: the
        // restore phase runs regardless, and the error still reaches the caller.
        #[derive(Debug)]
        struct FailingDimmer;
        impl Dimmer for FailingDimmer {
            fn apply(&mut self, _commands: &[DimCommand]) -> Result<(), DimmerError> {
                Err(DimmerError::Backend)
            }
            fn clear(&mut self) -> Result<(), DimmerError> {
                Ok(())
            }
        }

        let mut coord = GammaCoordinator::default();
        let mut sink = FakeSink::default();
        gamma_only(&mut coord, &[cmd("B", Some(0.6))], &mut sink);
        let outcome = apply_dimming_batch(
            &[cmd("B", None)],
            &mut coord,
            &mut sink,
            Some(&mut FailingDimmer),
        );
        assert!(outcome.is_err(), "the overlay failure must be reported");
        assert_eq!(
            sink.restored,
            vec![id("B")],
            "the stale ramp must be restored even when the overlay diff failed"
        );
    }

    // --- Once-per-reason refusal logging ------------------------------------

    /// Two distinct refusal reasons, as the real sink would supply them.
    const REASON_A: &str = "SetDeviceGammaRamp refused the ramp";
    const REASON_B: &str = "no GDI device for this display";

    #[test]
    fn refusal_log_reports_only_the_transitions() {
        // Bug 3: 349 identical warnings, a dozen inside one 450 ms drag. Only the
        // edges are worth a line.
        let mut log = RefusalLog::default();
        assert!(
            log.note_refusal(&id("A"), REASON_A),
            "the first refusal is news"
        );
        for _ in 0..11 {
            assert!(
                !log.note_refusal(&id("A"), REASON_A),
                "a repeat of the same refusal is not"
            );
        }
        assert!(log.note_success(&id("A")), "the recovery is news");
        assert!(
            !log.note_success(&id("A")),
            "a display that was not refusing stays silent"
        );
        // And it latches again after recovering, so a second episode is reported.
        assert!(log.note_refusal(&id("A"), REASON_A));
    }

    #[test]
    fn refusal_log_reports_a_different_reason_on_the_same_display() {
        // An id-only latch would swallow this entirely: a display that stops being
        // resolvable to a GDI device after its ramp was refused is a *different*
        // fault, and hiding it behind the first one loses the diagnosis.
        let mut log = RefusalLog::default();
        assert!(log.note_refusal(&id("A"), REASON_A));
        assert!(
            log.note_refusal(&id("A"), REASON_B),
            "a different reason for the same display must be logged"
        );
        assert!(
            !log.note_refusal(&id("A"), REASON_B),
            "...and then latch on the new reason"
        );
        assert_eq!(log.reason_for(&id("A")), Some(REASON_B));
    }

    #[test]
    fn refusal_log_tracks_displays_independently() {
        let mut log = RefusalLog::default();
        assert!(log.note_refusal(&id("A"), REASON_A));
        assert!(
            log.note_refusal(&id("B"), REASON_A),
            "B's first refusal is news even though A is already refusing"
        );
        assert!(log.note_success(&id("A")));
        assert!(
            !log.note_refusal(&id("B"), REASON_A),
            "A recovering must not un-latch B"
        );
        assert_eq!(log.reason_for(&id("A")), None);
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
