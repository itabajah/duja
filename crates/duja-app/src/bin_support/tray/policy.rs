//! The tray app's pure decision rules.
//!
//! Every function here is a free function over plain values — no [`AppState`],
//! no Win32, no Slint — so the adoption/reflection/toggle/throttle policy is
//! unit-tested directly, away from the event loop it runs inside.
//!
//! [`AppState`]: super::state::AppState

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use duja_core::continuum::{ContinuumConfig, map_user_level, reverse_map};
use duja_core::manager::DEFAULT_USER_LEVEL_PCT;
use duja_core::model::DisplaySnapshot;

use crate::bin_support::state_store::StateStore;

use super::FLYOUT_MIN_LOGICAL_HEIGHT;

/// How long after the flyout is hidden a tray-icon click is treated as the same
/// dismissing gesture (rather than a fresh open), closing the click-outside race.
pub(super) const TOGGLE_GUARD: Duration = Duration::from_millis(200);

/// How long a probed HDR gamma verdict is trusted before the app re-probes DXGI.
///
/// The verdict is refreshed from
/// [`AppState::on_displays_changed`](super::state::AppState::on_displays_changed),
/// which also fires on every `SetUserLevel` echo (a slider drag) — re-probing
/// DXGI there unconditionally would put a factory-create on the drag hot path.
/// This TTL bounds the probe rate to at most once per window while still
/// picking up a live HDR on/off change well inside human reaction time.
pub(super) const GAMMA_VERDICT_TTL: Duration = Duration::from_secs(1);

/// What a tray-icon click resolves to, given flyout visibility + recency of hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ToggleDecision {
    /// Open the flyout.
    Show,
    /// Hide the visible flyout.
    Hide,
    /// Swallow the click (it is the tail of the gesture that just dismissed the
    /// flyout via focus-loss; re-opening would fight the user).
    Ignore,
}

/// The perceived slider level to adopt for a freshly-sighted display the user has
/// not yet taken control of, choosing between the live hardware reading and the
/// persisted value.
///
/// Item 5 — Duja adopts the monitor's CURRENT brightness on launch. The engine's
/// reading (`reading_pct`, from the initial Get) is a **hardware** percentage,
/// but the slider is **perceived** (ADR-0014), so a real reading is reflected
/// through [`reverse_map`] to its slider position — the slider then mirrors
/// reality and the first interaction causes no jump. It falls back to the
/// persisted last-known (already a perceived level) only when the reading is
/// still the pre-probe placeholder ([`DEFAULT_USER_LEVEL_PCT`] — before the Get
/// lands, or when it fails) and a persisted value exists. It never itself writes
/// to hardware — adoption reflects reality, it does not restore a saved level.
pub(super) fn adopt_position(reading_pct: u8, persisted: Option<u8>, cfg: ContinuumConfig) -> u8 {
    match persisted {
        Some(saved) if reading_pct == DEFAULT_USER_LEVEL_PCT => saved,
        _ => reverse_map(reading_pct, &cfg),
    }
}

/// The perceptual gate for a poll reading: `Some(new_slider)` to reflect a
/// genuine external change, or `None` when the reading matches what the current
/// slider position already drives.
///
/// The `None` case covers our own state and, crucially, the pinned-floor/overlay
/// case: below the transition the hardware sits at the floor and the reading
/// matches it, so the reflection never yanks the thumb up to the transition even
/// though `reverse_map` (alpha-agnostic) would map the floor reading there. A
/// software-only display (no hardware channel) never reflects.
pub(super) fn reflected_level(
    current_perceived: u8,
    hw_pct: u8,
    cfg: ContinuumConfig,
) -> Option<u8> {
    match map_user_level(current_perceived, &cfg).hardware_pct {
        None => None,
        Some(intended) if intended.abs_diff(hw_pct) <= 1 => None,
        Some(_) => Some(reverse_map(hw_pct, &cfg)),
    }
}

/// Adopt a fresh enumeration into the state book: record each display's adopted
/// user level (see [`adopt_position`]) for every display the user has **not** taken
/// control of this session.
///
/// This is the startup/hot-plug "adopt reality" step (item 5). It has **no engine
/// channel by construction** — adoption records the level for the UI and persists
/// it, but pushes NOTHING to the hardware, so a launch can never move the
/// brightness. (The pre-fix code sent an `EngineCommand::SetUserLevel` for the
/// persisted level here, which dimmed the monitor to the last-saved level on every
/// launch.) A user-controlled display is skipped so a late enumeration echo cannot
/// overwrite the user's chosen value.
pub(super) fn adopt_enumeration(
    snapshots: &[DisplaySnapshot],
    user_controlled: &BTreeSet<String>,
    cfgs: &[ContinuumConfig],
    state: &mut StateStore,
    now_unix: i64,
) {
    for (snap, cfg) in snapshots.iter().zip(cfgs) {
        if user_controlled.contains(snap.id.as_str()) {
            continue;
        }
        let level = adopt_position(snap.user_level_pct, state.level(snap.id.as_str()), *cfg);
        state.record(snap.id.as_str(), level, now_unix);
    }
}

/// Decide what a tray-icon click should do.
///
/// A visible flyout hides. An already-hidden flyout normally re-opens — *unless*
/// it was hidden within [`TOGGLE_GUARD`] of this click, which means focus-loss
/// dismissal already fired for this same click; then the click is swallowed so
/// the flyout does not immediately re-open (P0 live-QA bug 5 follow-up: clicking
/// the tray icon while the flyout is open toggles it closed, not re-open).
pub(super) fn toggle_decision(
    visible: bool,
    since_hidden: Option<Duration>,
    guard: Duration,
) -> ToggleDecision {
    if visible {
        ToggleDecision::Hide
    } else if since_hidden.is_some_and(|elapsed| elapsed < guard) {
        ToggleDecision::Ignore
    } else {
        ToggleDecision::Show
    }
}

/// Clamp a flyout's content-driven height to the work-area `cap` while keeping
/// the minimum window height. Pure and shared by
/// [`AppState::show_flyout`](super::state::AppState::show_flyout) and the
/// open-flyout resize in
/// [`AppState::on_displays_changed`](super::state::AppState::on_displays_changed)
/// so both apply the same cap; unit-tested independently of the Win32
/// work-area query.
pub(super) fn clamp_flyout_height(content: f32, cap: f32) -> f32 {
    content.min(cap).max(FLYOUT_MIN_LOGICAL_HEIGHT)
}

/// Whether a background update check is due: never checked before, at least
/// `interval_secs` have passed since `last_check_unix`, **or** that timestamp is
/// in the future.
///
/// A future `last` means a backward wall-clock correction or a bad persisted
/// value; treat it as due so the check can never be wedged off forever (a plain
/// `now.saturating_sub(last)` would clamp to 0 and report "not due" until real
/// time overtook the stale future stamp). Uses saturating subtraction so the
/// non-monotonic case cannot panic under the arithmetic-side-effects lint.
pub(super) fn due_for_check(
    now_unix: i64,
    last_check_unix: Option<i64>,
    interval_secs: i64,
) -> bool {
    match last_check_unix {
        None => true,
        Some(last) => now_unix < last || now_unix.saturating_sub(last) >= interval_secs,
    }
}

/// Whether the cached HDR verdict is stale enough to re-probe DXGI: never probed
/// (`None`), or at least `ttl` has elapsed since the last probe.
///
/// Pure and monotonic-clock-safe ([`Instant::duration_since`] saturates), so the
/// throttle that keeps the probe off the slider-drag hot path is unit-tested
/// without the FFI probe itself.
pub(super) fn verdict_probe_due(last: Option<Instant>, now: Instant, ttl: Duration) -> bool {
    match last {
        None => true,
        Some(last) => now.duration_since(last) >= ttl,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ContinuumConfig, DEFAULT_USER_LEVEL_PCT, GAMMA_VERDICT_TTL, TOGGLE_GUARD, ToggleDecision,
        adopt_enumeration, adopt_position, clamp_flyout_height, due_for_check, reflected_level,
        toggle_decision, verdict_probe_due,
    };
    use crate::bin_support::state_store::StateStore;
    use crate::bin_support::tray::update_flow::UPDATE_CHECK_INTERVAL_SECS;
    use duja_core::id::StableDisplayId;
    use duja_core::model::{Capabilities, DimMode, DisplayKind, DisplaySnapshot};
    use std::collections::BTreeSet;
    use std::time::{Duration, Instant};

    // --- Item 5: launching Duja must NOT change the monitor brightness ---------
    //
    // The bug (confirmed on the live box: floor 20, overlay, persisted 48): the
    // first enumeration pushed the PERSISTED level to the engine, dimming the
    // monitor to the last-saved level on every launch, and seeded the UI from the
    // persisted file. The fix ADOPTS the monitor's current hardware reading
    // (`snap.user_level_pct`) and writes nothing. `adopt_enumeration` has no engine
    // channel by construction, so adoption structurally cannot push a level; the
    // live 20×-cycle probe confirms the brightness stays put on launch.

    fn adopt_snap(serial: &str, reading_pct: u8) -> DisplaySnapshot {
        DisplaySnapshot {
            id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap(),
            name: format!("Monitor {serial}"),
            kind: DisplayKind::ExternalDdc,
            software_only: false,
            user_level_pct: reading_pct,
            capabilities: Capabilities::default(),
        }
    }

    #[test]
    fn update_check_due_only_after_the_interval() {
        let day = UPDATE_CHECK_INTERVAL_SECS;
        // Never checked before ⇒ always due.
        assert!(due_for_check(1_000, None, day));
        // Just checked ⇒ not due.
        assert!(!due_for_check(1_000, Some(1_000), day));
        // Less than a day later ⇒ not due.
        assert!(!due_for_check(1_000 + day - 1, Some(1_000), day));
        // Exactly a day later ⇒ due.
        assert!(due_for_check(1_000 + day, Some(1_000), day));
        // More than a day later ⇒ due.
        assert!(due_for_check(1_000 + day * 2, Some(1_000), day));
        // Non-monotonic clock (last in the FUTURE: a backward clock correction or
        // a bad persisted value) ⇒ DUE, so the check can never be wedged off
        // forever (and still no panic).
        assert!(due_for_check(1_000, Some(5_000), day));
        // One second before `last` is still in the future ⇒ due.
        assert!(due_for_check(4_999, Some(5_000), day));
    }

    #[test]
    fn verdict_probe_is_due_only_after_the_ttl() {
        // Fix 1: the HDR verdict re-probe is throttled off the slider-drag hot
        // path. `on_displays_changed` (which drives it) also fires on a
        // SetUserLevel echo, so the probe must run at most once per TTL while
        // still refreshing within it.
        let ttl = GAMMA_VERDICT_TTL;
        let t0 = Instant::now();
        // Never probed ⇒ always due (the field starts unset / first enumeration).
        assert!(verdict_probe_due(None, t0, ttl));
        // Just probed ⇒ not due.
        assert!(!verdict_probe_due(Some(t0), t0, ttl));
        // Within the TTL ⇒ not due (a drag echo re-uses the cached verdict).
        let mid = t0.checked_add(ttl / 2).expect("instant in range");
        assert!(!verdict_probe_due(Some(t0), mid, ttl));
        // Exactly at the TTL ⇒ due.
        let at = t0.checked_add(ttl).expect("instant in range");
        assert!(verdict_probe_due(Some(t0), at, ttl));
        // Past the TTL ⇒ due.
        let past = t0.checked_add(ttl * 2).expect("instant in range");
        assert!(verdict_probe_due(Some(t0), past, ttl));
    }

    #[test]
    fn flyout_height_is_clamped_to_the_work_area_cap() {
        use super::FLYOUT_MIN_LOGICAL_HEIGHT;
        // Cap below content ⇒ clamp DOWN to the cap so rows scroll instead of
        // overflowing off-screen. This is the on_displays_changed regression: an
        // open flyout dropped this cap and grew back to its full content height.
        assert!((clamp_flyout_height(620.0, 300.0) - 300.0).abs() < f32::EPSILON);
        // Cap above content ⇒ keep the content's own layout height.
        assert!((clamp_flyout_height(420.0, 620.0) - 420.0).abs() < f32::EPSILON);
        // A cap tighter than the floor still yields at least the minimum height.
        assert!(
            (clamp_flyout_height(620.0, 100.0) - FLYOUT_MIN_LOGICAL_HEIGHT).abs() < f32::EPSILON
        );
    }

    fn temp_state() -> (StateStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        (StateStore::load(dir.path().join("state.toml")), dir)
    }

    /// An identity continuum (m=0, floor=0): `reverse_map` is the identity, so the
    /// adoption tests below stay focused on the reading-vs-persisted logic.
    fn identity_cfg() -> ContinuumConfig {
        ContinuumConfig::hardware(0, 0, DimMode::Overlay)
    }

    #[test]
    fn adopt_position_prefers_the_live_hardware_reading_over_persisted() {
        // A real reading is adopted, even against a different (low — the bug's
        // trigger) persisted level: the slider mirrors reality, nothing moves.
        assert_eq!(adopt_position(70, Some(20), identity_cfg()), 70);
        assert_eq!(adopt_position(70, None, identity_cfg()), 70);
    }

    #[test]
    fn adopt_position_falls_back_to_persisted_only_for_the_pre_probe_placeholder() {
        // While the reading is still the pre-probe placeholder, the persisted
        // last-known seeds the UI (the documented fallback for a failed/pending read)…
        assert_eq!(
            adopt_position(DEFAULT_USER_LEVEL_PCT, Some(20), identity_cfg()),
            20
        );
        // …but with nothing persisted there is nothing better than the placeholder.
        assert_eq!(
            adopt_position(DEFAULT_USER_LEVEL_PCT, None, identity_cfg()),
            DEFAULT_USER_LEVEL_PCT
        );
    }

    #[test]
    fn adopt_position_reflects_a_real_reading_through_the_perceptual_scale() {
        // The engine's reading is a hardware %, but the slider is perceived
        // (ADR-0014): a real reading is reflected through reverse_map so the slider
        // shows the true perceived position and the first interaction never jumps.
        // hardware 70 with anchor 25 ⇒ pos(70) = 25 + 75·0.7 = 77.5 → 78.
        let cfg = ContinuumConfig::hardware(0, 25, DimMode::Overlay);
        assert_eq!(adopt_position(70, None, cfg), 78);
        // A low persisted value must not be preferred over a real reading.
        assert_eq!(adopt_position(70, Some(20), cfg), 78);
    }

    #[test]
    fn adoption_seeds_from_the_reading_and_pushes_no_level() {
        // The old code pushed the persisted 48 to hardware here (dimming on launch);
        // adoption takes the live reading (70) into state and — since this fn has no
        // engine channel — sends ZERO SetUserLevel. The seed is the reading.
        let snap = adopt_snap("A", 70);
        let id = snap.id.as_str().to_owned();
        let (mut state, _dir) = temp_state();
        state.record(&id, 48, 1); // low persisted level, as on the live box
        adopt_enumeration(&[snap], &BTreeSet::new(), &[identity_cfg()], &mut state, 2);
        assert_eq!(
            state.level(&id),
            Some(70),
            "must adopt the live hardware reading, not the persisted 48"
        );
    }

    // --- external-change reflection: the perceptual gate ---

    #[test]
    fn reflected_level_ignores_a_reading_matching_the_current_slider() {
        // Identity continuum: current slider 50 drives hardware 50; a reading of 50
        // (± rounding) is our own state, not an external change.
        let cfg = ContinuumConfig::hardware(0, 0, DimMode::Overlay);
        assert_eq!(reflected_level(50, 50, cfg), None);
        assert_eq!(reflected_level(50, 51, cfg), None); // within tolerance
    }

    #[test]
    fn reflected_level_reflects_a_genuine_external_change() {
        // A reading that differs from what the slider drives is reflected via
        // reverse_map (identity here ⇒ 80).
        let cfg = ContinuumConfig::hardware(0, 0, DimMode::Overlay);
        assert_eq!(reflected_level(50, 80, cfg), Some(80));
    }

    #[test]
    fn reflected_level_does_not_jump_when_pinned_below_the_floor() {
        // floor 30, anchor 25 ⇒ transition B = 47.5. At slider 10 (below B) the
        // hardware is pinned at the floor 30; a reading of 30 matches, so the gate
        // returns None — the thumb must NOT jump up to pos(30) = 47.5 even though
        // reverse_map(30) would map there.
        let cfg = ContinuumConfig::hardware(30, 25, DimMode::Overlay);
        assert_eq!(reflected_level(10, 30, cfg), None);
        // But a reading well above the floor is a real external change.
        assert!(reflected_level(10, 70, cfg).is_some());
    }

    #[test]
    fn reflected_level_never_reflects_on_software_only() {
        let cfg = ContinuumConfig::software_only(DimMode::Overlay);
        assert_eq!(reflected_level(50, 80, cfg), None);
    }

    #[test]
    fn adoption_never_clobbers_a_user_controlled_display() {
        // After a genuine user change, a later enumeration echo must not re-adopt
        // (overwrite) the user's chosen level.
        let snap = adopt_snap("A", 70);
        let id = snap.id.as_str().to_owned();
        let (mut state, _dir) = temp_state();
        state.record(&id, 35, 1); // the user's chosen level
        let controlled: BTreeSet<String> = std::iter::once(id.clone()).collect();
        adopt_enumeration(&[snap], &controlled, &[identity_cfg()], &mut state, 2);
        assert_eq!(
            state.level(&id),
            Some(35),
            "a user-controlled level must survive an enumeration echo"
        );
    }

    #[test]
    fn toggle_decision_hides_a_visible_flyout() {
        // Visible → hide, regardless of the last-hidden timestamp.
        assert_eq!(
            toggle_decision(true, None, TOGGLE_GUARD),
            ToggleDecision::Hide
        );
        assert_eq!(
            toggle_decision(true, Some(Duration::from_millis(10)), TOGGLE_GUARD),
            ToggleDecision::Hide
        );
    }

    #[test]
    fn toggle_decision_ignores_a_click_right_after_focus_loss_hide() {
        // Hidden within the guard window: this click is the tail of the gesture
        // that just dismissed the flyout; swallow it (do not re-open).
        assert_eq!(
            toggle_decision(false, Some(Duration::from_millis(50)), TOGGLE_GUARD),
            ToggleDecision::Ignore
        );
    }

    #[test]
    fn toggle_decision_opens_when_hidden_long_ago_or_never() {
        // Never shown, or hidden well before this click → open.
        assert_eq!(
            toggle_decision(false, None, TOGGLE_GUARD),
            ToggleDecision::Show
        );
        assert_eq!(
            toggle_decision(false, Some(Duration::from_millis(500)), TOGGLE_GUARD),
            ToggleDecision::Show
        );
    }
}
