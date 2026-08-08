//! The frame instrument, driven end to end.
//!
//! A test binary of its own because Slint binds its platform per *thread* and
//! once: `duja-ui`'s unit tests install the testing backend, so a probe sharing
//! their binary would be racing them for a slot only one can hold. Cargo gives
//! each `tests/*.rs` its own binary, which is the isolation this needs.
//!
//! Two tests, and the split is deliberate:
//!
//! - [`the_real_flyout_renders_frames_through_the_software_renderer`] runs on
//!   every lane, every push. It asserts the harness *works* - that the real
//!   `FlyoutShell` instantiates headless, that frames come back timed, and that
//!   the buffer is not uniform - and asserts nothing about how long they took.
//!   This is the part that must not be allowed to rot.
//! - [`the_flyout_renders_inside_the_frame_budget`] is `#[ignore]`d and prints
//!   the number. It is a *timing* assertion, and a shared CI runner under an
//!   unknown load is not where this project wants one of those: [D-110] is the
//!   standing note that gating on a number nobody has measured is how a check
//!   becomes a thing people disable. Run it by hand, on a build profile you
//!   name, per `docs/qa-checklist.md`.
//!
//! [D-110]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-110

#![allow(clippy::expect_used)]

use duja_core::id::StableDisplayId;
use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
use duja_ui::flyout_vm::FlyoutVm;
use duja_ui::frame_probe::{self, FRAME_BUDGET, PROBE_SIZE, Verdict};

/// How many frames the timed run measures.
///
/// Enough that [`frame_probe::warmup_frames`] discards a real warm-up (a tenth,
/// so twelve here) and enough that one unlucky scheduler slice does not decide
/// the verdict on its own.
const TIMED_FRAMES: u32 = 120;

/// A three-monitor flyout: the shape the budget is written about, and more work
/// per frame than the one-monitor case most developers would be looking at.
fn three_monitors() -> FlyoutVm {
    let mut vm = FlyoutVm::new();
    vm.set_displays(
        [("A", "Left", 40), ("B", "Middle", 65), ("C", "Right", 80)]
            .into_iter()
            .map(|(serial, name, level)| DisplaySnapshot {
                id: StableDisplayId::from_parts("GSM", 0x0001, Some(serial))
                    .expect("a three-part id with a serial is well formed"),
                name: name.to_owned(),
                kind: DisplayKind::ExternalDdc,
                software_only: false,
                user_level_pct: level,
                capabilities: Capabilities::default(),
            })
            .collect(),
    );
    vm
}

/// The harness itself: does the real flyout render real frames with no display
/// server, on this lane?
///
/// Nothing here is about speed. It is about the claim `docs/` makes in several
/// places - that Duja's UI is instantiable headless - being executed rather
/// than repeated, and about the probe returning frames it actually drew.
#[test]
fn the_real_flyout_renders_frames_through_the_software_renderer() {
    let stats = frame_probe::probe(8, three_monitors())
        .expect("the software renderer needs no display server");

    // Eight frames, one discarded as warm-up.
    assert_eq!(
        stats.frames(),
        7,
        "every frame after the warm-up must be timed, or the probe is measuring \
         fewer frames than it ran"
    );
    assert!(
        stats.min().is_some() && stats.max().is_some() && stats.mean().is_some(),
        "a run with frames in it has a min, a max and a mean"
    );
    assert_ne!(
        stats.verdict(),
        Verdict::Unmeasurable,
        "the renderer drew nothing, so the timings above are measuring an empty loop"
    );
    // The assertion that makes the number believable. Every frame must have
    // redrawn the whole window: the probe asks for `NewBuffer`, which is a
    // full redraw by definition, so anything less means the component laid out
    // smaller than the window it was measured in and the timings describe a
    // flyout nobody will ever see.
    let (width, height) = PROBE_SIZE;
    assert_eq!(
        stats.least_drawn_pixels(),
        Some(u64::from(width).saturating_mul(u64::from(height))),
        "every frame must redraw the full {width}x{height} window"
    );
}

/// The empty flyout still paints - it draws its own no-displays state - so the
/// measurability check must not be reading "has rows" by accident.
#[test]
fn the_empty_state_paints_too() {
    let stats = frame_probe::probe(4, FlyoutVm::new()).expect("the empty flyout renders headless");
    assert_ne!(stats.verdict(), Verdict::Unmeasurable);
}

/// The timed run. `#[ignore]`d: see the module header.
///
/// Invoke it with `--nocapture` to read the numbers, and on `--release` if the
/// number is meant to say anything about the shipped profile - which is the
/// whole of [D-109]'s point, since the profile is what changed under it.
///
/// [D-109]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-109
#[test]
#[ignore = "a timing assertion; run it by hand on a named profile, not on a shared runner"]
fn the_flyout_renders_inside_the_frame_budget() {
    let stats = frame_probe::probe(TIMED_FRAMES, three_monitors())
        .expect("the software renderer needs no display server");

    let (width, height) = PROBE_SIZE;
    println!(
        "frames={} min={:?} mean={:?} max={:?} drawn={:?}/{} budget={:?} verdict={:?}",
        stats.frames(),
        stats.min(),
        stats.mean(),
        stats.max(),
        stats.least_drawn_pixels(),
        u64::from(width).saturating_mul(u64::from(height)),
        FRAME_BUDGET,
        stats.verdict(),
    );

    // The same believability check the always-on test makes. It is repeated
    // rather than assumed because this is the run whose number gets written
    // into `docs/perf-budgets.md`, and a figure quoted from a partial redraw
    // would be wrong in the direction nobody checks.
    assert_eq!(
        stats.least_drawn_pixels(),
        Some(u64::from(width).saturating_mul(u64::from(height))),
        "every frame must redraw the full {width}x{height} window"
    );
    assert_eq!(
        stats.verdict(),
        Verdict::Pass,
        "the slowest frame was {:?}, over the {:?} budget",
        stats.max(),
        FRAME_BUDGET,
    );
}
