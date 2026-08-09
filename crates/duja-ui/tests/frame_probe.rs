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
/// The expected sizes are **literals**. A first version compared
/// `probe_size(rows)` against `flyout_logical_height(rows)` - the function
/// `probe_size` is *defined* in terms of - so the assertion could not fail for
/// any implementation; a second version claimed to have fixed that and did not,
/// because the edit silently failed to apply and a vacuous test kept passing.
///
/// **What this pins is the public size surface, not the probe's use of it.** It
/// reds if `probe_size` returns the wrong pair - which is the shape the original
/// defect took, a `PROBE_SIZE` constant of `(360, 260)`. It stays *green* if
/// `probe()` ignores `probe_size` and sizes its window some other way; the two
/// tests below are what catch that, and they do.
#[test]
fn the_probe_window_is_the_size_the_app_presents() {
    assert_eq!(probe_size(0), (360, 178));
    assert_eq!(probe_size(1), (360, 179));
    assert_eq!(probe_size(2), (360, 288));
    assert_eq!(probe_size(3), (360, 397));
    assert_eq!(probe_size(5), (360, 615));
}

/// **A third monitor must put a whole card's worth of new pixels on the
/// screen.**
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
///
/// **Deliberately 1, 2 and 3 rather than a monotonicity claim.** The count is
/// pixels unequal to the buffer's modal colour, and the mode is not a fixed
/// thing: it is the window background at 0 and 1 rows and the card fill from 2
/// on. So the sequence is **not** monotone - the first monitor lowers the count
/// by about 2,650, and the sixth lowers it by about 15,600 because the window
/// has hit its 620 px clamp, so the extra card's upper band enters the viewport
/// and the rest of it becomes scrollable rather than drawn - and a test named
/// for "every extra monitor" would have been asserting something false. (The
/// rows do **not** compress: `flyout.slint` packs cards at their natural height
/// inside a `ScrollView`, and the bands measure identically from one monitor to
/// seven.) What is stable, and what the sizing
/// defect breaks, is that a card which fits adds a card's worth.
#[test]
fn a_third_monitor_adds_a_whole_card_of_pixels() {
    let content = |n: usize| {
        frame_probe::probe(4, monitors(n))
            .expect("the flyout renders headless")
            .least_content_pixels()
            .expect("a run with frames in it has a content count")
    };

    let (two, three) = (content(2), content(3));
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

    // **Restored after a review demonstrated its absence.** This assertion was
    // in the first version, was dropped when the test was rewritten to fix the
    // sizing defect, and its absence let this exact run go green on that very
    // defect: it printed `size=360x397 ... drawn=Some(93600)/142920` and passed,
    // because `--ignored` filters out the always-on test that checks it. This is
    // the run whose numbers get written into `docs/perf-budgets.md`, so it is
    // the last place the check should be missing.
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
