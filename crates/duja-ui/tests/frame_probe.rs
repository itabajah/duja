//! The frame instrument, driven end to end.
//!
//! A test binary of its own because Slint binds its platform per *thread* and
//! once: `duja-ui`'s unit tests install the testing backend, so a probe sharing
//! their binary would be racing them for a slot only one can hold. Cargo gives
//! each `tests/*.rs` its own binary, which is the isolation this needs.
//!
//! The split between what runs always and what is `#[ignore]`d is deliberate.
//! Everything that asserts the harness *works* runs on every lane, every push.
//! The one test that asserts a *duration* is ignored: a shared CI runner under
//! an unknown load is not where this project wants a timing gate, and [D-110]
//! is the standing note that gating on a number nobody has measured is how a
//! check becomes a thing people disable.
//!
//! [D-110]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-110

#![allow(clippy::expect_used)]

use duja_core::id::StableDisplayId;
use duja_core::model::{Capabilities, DisplayKind, DisplaySnapshot};
use duja_ui::flyout_vm::FlyoutVm;
use duja_ui::frame_probe::{self, FRAME_BUDGET, Verdict, probe_size};
use duja_ui::layout::flyout_logical_height;

/// How many frames the timed run measures.
///
/// Enough that `frame_probe::warmup_frames` discards a real warm-up and enough
/// that one unlucky scheduler slice does not decide the verdict on its own.
const TIMED_FRAMES: u32 = 120;

/// A flyout with `n` monitors in it.
fn monitors(n: usize) -> FlyoutVm {
    let mut vm = FlyoutVm::new();
    vm.set_displays(
        (0..n)
            .map(|i| DisplaySnapshot {
                id: StableDisplayId::from_parts("GSM", 0x0001, Some(&format!("S{i}")))
                    .expect("a three-part id with a serial is well formed"),
                name: format!("Monitor {i}"),
                kind: DisplayKind::ExternalDdc,
                software_only: false,
                user_level_pct: 40,
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
    let stats =
        frame_probe::probe(8, monitors(3)).expect("the software renderer needs no display server");

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

    let (width, height) = probe_size(3);
    assert_eq!(
        stats.least_drawn_pixels(),
        Some(u64::from(width).saturating_mul(u64::from(height))),
        "every frame must redraw the full {width}x{height} window"
    );
}

/// **The probe renders the window the app actually presents.**
///
/// The regression this exists for: the first version sized its window from the
/// markup's default `content-height` of 260, which no row count produces. A
/// three-monitor flyout is 397 logical pixels tall, so the third card rendered
/// past the bottom edge and the published timing was a two-card frame wearing a
/// three-monitor label.
#[test]
fn the_probe_window_is_the_size_the_app_presents() {
    for rows in [0_usize, 1, 2, 3, 5] {
        let (_, height) = probe_size(rows);
        assert!(
            (f32::from(u16::try_from(height).expect("a flyout is under 65535 px tall"))
                - flyout_logical_height(rows))
            .abs()
                < 1.0,
            "the probe's {rows}-row window is {height} px, but the app presents \
             {} px",
            flyout_logical_height(rows)
        );
        assert_ne!(
            height, 260,
            "260 is the markup default, not a size the app ever presents"
        );
    }
}

/// **Content has to reach the buffer, and more monitors have to mean more of
/// it.**
///
/// This is the assertion that would have caught the sizing defect, and the
/// drawn-area check provably would not have: under `NewBuffer` the region the
/// renderer reports is the window item's rect, taken from the size the probe
/// itself passed to `set_size`, so it re-asserts its own input.
///
/// Measured against the defect re-inserted at the site it occupied (the probe
/// sizing its window from the markup default and skipping `set_content_height`),
/// a third monitor adds **168** content pixels. With the window sized the way
/// the app presents it, the same monitor adds **14,161**.
#[test]
fn each_extra_monitor_puts_more_content_on_the_screen() {
    let content = |n: usize| {
        frame_probe::probe(4, monitors(n))
            .expect("the flyout renders headless")
            .least_content_pixels()
            .expect("a run with frames in it has a content count")
    };

    let (one, two, three) = (content(1), content(2), content(3));
    assert!(
        two > one,
        "a second monitor drew no more content than one: {one} then {two}"
    );
    assert!(
        three > two,
        "a third monitor drew no more content than two: {two} then {three}"
    );
    // Measured on this box, a card is worth about 14,160 content pixels. The
    // floor is a third of that: comfortably above noise, comfortably below a
    // rendered card, and nowhere near the low hundreds a clipped card produces.
    assert!(
        three.saturating_sub(two) > 5_000,
        "a third monitor added only {} pixels of content, which is the \
         off-the-bottom-edge signature rather than a rendered card",
        three.saturating_sub(two)
    );
}

/// The empty flyout still draws - it has its own no-displays panel - so the
/// measurability check must not be reading "has rows" by accident.
#[test]
fn the_empty_state_draws_too() {
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
    let stats = frame_probe::probe(TIMED_FRAMES, monitors(3))
        .expect("the software renderer needs no display server");

    let (width, height) = probe_size(3);
    println!(
        "frames={} size={width}x{height} min={:?} mean={:?} max={:?} drawn={:?}/{} content={:?} \
         budget={:?} verdict={:?}",
        stats.frames(),
        stats.min(),
        stats.mean(),
        stats.max(),
        stats.least_drawn_pixels(),
        u64::from(width).saturating_mul(u64::from(height)),
        stats.least_content_pixels(),
        FRAME_BUDGET,
        stats.verdict(),
    );

    assert_eq!(
        stats.verdict(),
        Verdict::Pass,
        "the slowest frame was {:?}, over the {:?} budget",
        stats.max(),
        FRAME_BUDGET,
    );
}
