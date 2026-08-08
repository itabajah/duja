//! The frame instrument: how long the real flyout takes to render one frame,
//! measured through Slint's software renderer with no display server.
//!
//! [D-109] is the row this exists for, and the exposure it names is specific.
//! P8 wave 1 moved the release profile to `opt-level = "s"` and then exempted
//! the crates on the frame path by name - `i-slint-core`,
//! `i-slint-renderer-software`, `swash`, `zeno` and `duja-ui` - rather than
//! measuring what the change did to a frame. **That exemption is an argument,
//! not a measurement**, and the argument has a soft spot the ADR itself names:
//! a per-package `opt-level` override under fat LTO is honoured through
//! per-function `optsize` attributes, which is not the same guarantee as a
//! whole-program `-O3` build.
//!
//! *(D-109 says "the four crates" and lists four. There are five: `swash` was
//! added after the row was written, by the same P8 wave that noticed the first
//! version of the list credited `zeno` with a job half of which is `swash`'s.
//! The row was never updated. Counted here rather than restated, because a
//! count in prose is the thing this project has spent a phase learning not to
//! write.)*
//!
//! This module is the measurement the argument never had.
//!
//! # What it does not measure, said before what it does
//!
//! `docs/perf-budgets.md` has three rows with no live instrument, and **this
//! closes none of them**:
//!
//! - **"Overlay alpha update < 16 ms"** is `duja-dimmer`'s layered window, a
//!   Win32 surface that is not a Slint component at all. Nothing here touches
//!   it.
//! - **"Cold start to tray icon visible < 300 ms"** needs a tray icon, which
//!   needs an interactive session. A headless renderer cannot see it.
//! - **"Slider to DDC write dispatched"** is an engine path, not a render one.
//!
//! D-109's own remedy paragraph proposed this harness for the first two, and
//! that was wrong: the harness renders `FlyoutShell`, and neither row is about
//! `FlyoutShell`. What it *is* about is the frame path the `opt-level`
//! exemption protects, which is the actual exposure and is otherwise unmeasured
//! on every lane.
//!
//! # Why the software renderer rather than a timer around the real window
//!
//! A span around the winit path needs a human watching a screen, which is what
//! `perf-budgets.md` already calls not an instrument. `MinimalSoftwareWindow`
//! renders the *same* item tree with the *same* renderer into a plain buffer,
//! on any lane, with no session - so the number is comparable across the three
//! platforms and reproducible in CI, which the windowed path is not.
//!
//! [D-109]: https://github.com/itabajah/duja/blob/main/docs/debt.md#d-109

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::PhysicalSize;
use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType,
};
use slint::platform::{Platform, PlatformError, WindowAdapter};

use crate::flyout_vm::FlyoutVm;
use crate::shell::FlyoutShell;

/// The per-frame budget, from `docs/perf-budgets.md`.
///
/// Sixteen milliseconds is one frame at 60 Hz. The doc row and this constant
/// are one decision written twice, and a test below reads the doc rather than
/// trusting the comment - the same rule `xtask`'s size budget follows, for the
/// same reason: prose restating a number is what rots.
pub const FRAME_BUDGET: Duration = Duration::from_millis(16);

/// How `docs/perf-budgets.md` names this instrument in its How-measured column.
///
/// A constant rather than a literal in one test, because it is the string that
/// ties the row to the code: the test below finds the row *by* it, so a rename
/// here that does not reach the doc fails loudly instead of silently matching
/// some other row's number.
pub const INSTRUMENT: &str = "--test frame_probe";

/// What a finished probe concluded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Every measured frame landed inside [`FRAME_BUDGET`].
    Pass,
    /// At least one measured frame ran over.
    Over,
    /// The run measured nothing, so it is not a pass. Either no frame was
    /// timed, or a frame drew no pixels, or a frame's buffer came back uniform
    /// - a renderer that drew nothing is fast for the wrong reason.
    Unmeasurable,
}

/// One timed frame, with the two independent answers to "did it draw".
///
/// **Two, because neither alone is enough**, and the first draft of this module
/// shipped only the second. `drawn_pixels` is the renderer's own account of the
/// area it touched, which is authoritative about intent and says nothing about
/// what came out; `painted` is a scan of the buffer, which proves pixels differ
/// and cannot tell a full redraw from a one-pixel one. A probe that reports
/// 180 microseconds for a window it laid out at zero size satisfies the second
/// and fails the first, and 180 microseconds is what this harness measured on
/// its first run - fast enough to need the check before the number could be
/// believed.
#[derive(Debug, Clone, Copy)]
pub struct Frame {
    /// How long [`SoftwareRenderer::render`](slint::platform::software_renderer::SoftwareRenderer::render)
    /// took.
    pub elapsed: Duration,
    /// The area of the region the renderer reported drawing.
    pub drawn_pixels: u64,
    /// Whether the buffer came back with more than one distinct pixel value.
    pub painted: bool,
}

/// How many leading frames a run of `total` discards before it starts timing.
///
/// The first frame is not the frame this budget is about: it rasterises fonts,
/// builds the item tree's caches and touches every page of the buffer for the
/// first time. Timing it would report a cost the user pays once as though they
/// paid it every frame.
///
/// A tenth of the run, at least one, capped at sixteen - the same shape
/// `soak::warmup` uses and for the same reason: a fixed count would make a
/// short run all warm-up, and a fixed fraction would give a long run a warm-up
/// big enough to hide the thing being measured inside it.
#[must_use]
pub fn warmup_frames(total: u32) -> u32 {
    if total == 0 {
        return 0;
    }
    // RATIONALE (arithmetic_side_effects): the divisor is a non-zero literal
    // and the dividend is unsigned, so neither of the two ways integer division
    // can trap is reachable.
    #[allow(clippy::arithmetic_side_effects)]
    let tenth = total / 10;
    tenth.clamp(1, MAX_WARMUP_FRAMES)
}

/// The ceiling on [`warmup_frames`]: past this, more warm-up buys nothing and
/// starts eating the measurement.
const MAX_WARMUP_FRAMES: u32 = 16;

/// Frame timings, accumulated as they are measured.
#[derive(Debug, Default, Clone)]
pub struct FrameStats {
    frames: u32,
    painted: u32,
    min: Option<Duration>,
    max: Option<Duration>,
    total: Duration,
    least_drawn: Option<u64>,
}

impl FrameStats {
    /// Record one timed frame.
    pub fn record(&mut self, frame: Frame) {
        let Frame {
            elapsed,
            drawn_pixels,
            painted,
        } = frame;
        self.frames = self.frames.saturating_add(1);
        if painted {
            self.painted = self.painted.saturating_add(1);
        }
        self.min = Some(self.min.map_or(elapsed, |m| m.min(elapsed)));
        self.max = Some(self.max.map_or(elapsed, |m| m.max(elapsed)));
        self.total = self.total.saturating_add(elapsed);
        self.least_drawn = Some(
            self.least_drawn
                .map_or(drawn_pixels, |d| d.min(drawn_pixels)),
        );
    }

    /// The area the *stingiest* timed frame reported drawing.
    ///
    /// The minimum rather than the maximum, because the question this answers
    /// is whether every frame in the run did real work: one full redraw among a
    /// hundred empty ones is a run whose timings mean nothing, and a maximum
    /// would report it as healthy.
    #[must_use]
    pub const fn least_drawn_pixels(&self) -> Option<u64> {
        self.least_drawn
    }

    /// How many frames were timed.
    #[must_use]
    pub const fn frames(&self) -> u32 {
        self.frames
    }

    /// The fastest timed frame, or `None` if nothing was timed.
    #[must_use]
    pub const fn min(&self) -> Option<Duration> {
        self.min
    }

    /// The slowest timed frame, or `None` if nothing was timed.
    #[must_use]
    pub const fn max(&self) -> Option<Duration> {
        self.max
    }

    /// The mean timed frame, or `None` if nothing was timed.
    #[must_use]
    pub fn mean(&self) -> Option<Duration> {
        self.total.checked_div(self.frames)
    }

    /// What this run concluded.
    ///
    /// The measurability check comes first and it is not a formality: a probe
    /// whose renderer drew a uniform buffer reports beautiful numbers for the
    /// worst possible reason, and a harness that cannot tell that apart from a
    /// fast frame is the false assurance this project rates below an admitted
    /// gap.
    #[must_use]
    pub fn verdict(&self) -> Verdict {
        if self.frames == 0 || self.painted == 0 || self.least_drawn == Some(0) {
            return Verdict::Unmeasurable;
        }
        match self.max {
            Some(worst) if worst >= FRAME_BUDGET => Verdict::Over,
            _ => Verdict::Pass,
        }
    }
}

/// The flyout's design size in logical pixels, which at scale 1.0 is also the
/// buffer's physical size.
///
/// The same pair `FlyoutShell::new` seeds its buffer keeper with. A probe at
/// some other size would be measuring a window the app never shows.
///
/// Public because the drawn-area assertion is only meaningful against it: a
/// test that hard-coded 360 by 260 would go green on a probe that had quietly
/// started measuring some other window.
pub const PROBE_SIZE: (u32, u32) = (360, 260);

thread_local! {
    /// The window every probe on this thread renders into.
    static WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);

    /// Whether *this* thread has already installed [`ProbePlatform`].
    ///
    /// **A thread-local latch, not a `std::sync::Once`**, and the distinction is
    /// one this repository has already paid for once. Slint keeps its platform
    /// in a `thread_local!` `OnceCell`, so "has a platform been set" is a
    /// per-thread question; a `Once` answers it once per *process* and then lets
    /// every later thread run with no platform at all. That bug is green under
    /// a process-per-test runner and red under a thread-per-test one, which is
    /// the worst possible way for it to present.
    static INSTALLED: Cell<bool> = const { Cell::new(false) };
}

/// A platform that hands out one software-rendered window and nothing else.
struct ProbePlatform;

impl Platform for ProbePlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(WINDOW.with(Rc::clone) as Rc<dyn WindowAdapter>)
    }
}

/// Install [`ProbePlatform`] on this thread if it is not already there, and
/// hand back the window it renders into.
///
/// # Errors
/// Fails if this thread already has a *different* Slint platform installed - a
/// winit backend, or the testing backend. Reusing someone else's window would
/// mean measuring a surface this module did not build, so it refuses instead.
fn install() -> Result<Rc<MinimalSoftwareWindow>, PlatformError> {
    INSTALLED.with(|installed| {
        if !installed.get() {
            slint::platform::set_platform(Box::new(ProbePlatform)).map_err(|_| {
                PlatformError::Other(
                    "this thread already has a Slint platform, so the frame probe \
                     cannot install its own; run it in a test binary of its own"
                        .to_owned(),
                )
            })?;
            installed.set(true);
        }
        Ok(WINDOW.with(Rc::clone))
    })
}

/// Whether `buffer` holds more than one distinct pixel value.
///
/// The cheapest possible answer to "did the renderer actually draw", and the
/// reason [`FrameStats::verdict`] has an [`Unmeasurable`](Verdict::Unmeasurable)
/// arm at all.
fn painted(buffer: &[PremultipliedRgbaColor]) -> bool {
    // `PremultipliedRgbaColor` is not `PartialEq`, so the comparison is written
    // out. Four `u8`s rather than a derive is not worth a newtype.
    fn same(a: PremultipliedRgbaColor, b: PremultipliedRgbaColor) -> bool {
        a.alpha == b.alpha && a.red == b.red && a.green == b.green && a.blue == b.blue
    }
    buffer
        .split_first()
        .is_some_and(|(first, rest)| rest.iter().any(|px| !same(*px, *first)))
}

/// Render `frames` frames of the real flyout and time each one.
///
/// The caller supplies the view-model, so the number is attributable: a probe
/// of a three-monitor flyout and a probe of the empty state are different
/// measurements and this signature makes you choose. `vm`'s first row, if it
/// has one, is driven with a changing level each frame, which is the closest a
/// headless harness gets to the slider drag the budget is written about.
///
/// Leading frames are discarded per [`warmup_frames`].
///
/// # Errors
/// Bubbles [`PlatformError`] if this thread already has another Slint platform,
/// or if the flyout component fails to instantiate.
pub fn probe(frames: u32, vm: FlyoutVm) -> Result<FrameStats, PlatformError> {
    let (width, height) = PROBE_SIZE;
    let window = install()?;
    window.set_size(PhysicalSize::new(width, height));

    let vm = Rc::new(RefCell::new(vm));
    let shell = FlyoutShell::new(Rc::clone(&vm))?;

    let pixels = usize::try_from(width)
        .ok()
        .zip(usize::try_from(height).ok())
        .and_then(|(w, h)| w.checked_mul(h))
        .ok_or_else(|| PlatformError::Other("the probe size overflows a buffer".to_owned()))?;
    let mut buffer = vec![PremultipliedRgbaColor::default(); pixels];
    let stride = usize::try_from(width).unwrap_or(0);

    let warmup = warmup_frames(frames);
    let mut stats = FrameStats::default();

    for frame in 0..frames {
        // Dirty the tree the way a drag does. `update_from_vm` rather than the
        // Slint callback because a headless harness has no input to inject; it
        // is the same render path the app's own external-change reflection
        // takes, which is the honest half of a drag.
        {
            let mut vm = vm.borrow_mut();
            if !vm.rows().is_empty() {
                // RATIONALE (cast_possible_truncation): the remainder of a `%
                // 101` is 0..=100, which is a `u8` by construction.
                #[allow(clippy::cast_possible_truncation)]
                let pct = (frame % 101) as u8;
                drop(vm.slider_changed(0, pct));
            }
        }
        shell.update_from_vm(&vm.borrow());
        window.window().request_redraw();

        let drawn = Cell::new(0_u64);
        let started = Instant::now();
        let drew = window.draw_if_needed(|renderer| {
            let region = renderer.render(&mut buffer, stride);
            let size = region.bounding_box_size();
            drawn.set(u64::from(size.width).saturating_mul(u64::from(size.height)));
        });
        let elapsed = started.elapsed();

        if drew && frame >= warmup {
            stats.record(Frame {
                elapsed,
                drawn_pixels: drawn.get(),
                painted: painted(&buffer),
            });
        }
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame that drew the whole probe window, so the tests below can vary
    /// the one thing each is about.
    fn frame(elapsed: Duration, painted: bool) -> Frame {
        let (w, h) = PROBE_SIZE;
        Frame {
            elapsed,
            drawn_pixels: u64::from(w).saturating_mul(u64::from(h)),
            painted,
        }
    }

    /// A run short enough that a tenth rounds to nothing still has to discard
    /// its first frame, because the first frame is the one that is different.
    #[test]
    fn a_short_run_still_discards_one_warmup_frame() {
        assert_eq!(warmup_frames(1), 1);
        assert_eq!(warmup_frames(5), 1);
        assert_eq!(warmup_frames(10), 1);
    }

    #[test]
    fn warmup_is_a_tenth_of_a_longer_run() {
        assert_eq!(warmup_frames(50), 5);
        assert_eq!(warmup_frames(120), 12);
    }

    /// The cap is what stops a long run from spending its measurement on
    /// warm-up: a 10,000-frame probe wants sixteen discarded, not a thousand.
    #[test]
    fn warmup_is_capped_so_a_long_run_measures_what_it_ran() {
        assert_eq!(warmup_frames(1_000), 16);
        assert_eq!(warmup_frames(100_000), 16);
    }

    /// Zero frames is not zero warm-up frames by accident - it is a run that
    /// cannot be measured, and the verdict below is what says so.
    #[test]
    fn a_zero_frame_run_asks_for_no_warmup() {
        assert_eq!(warmup_frames(0), 0);
    }

    #[test]
    fn an_empty_run_is_unmeasurable_rather_than_a_pass() {
        let stats = FrameStats::default();
        assert_eq!(stats.verdict(), Verdict::Unmeasurable);
        assert_eq!(stats.min(), None);
        assert_eq!(stats.max(), None);
        assert_eq!(stats.mean(), None);
    }

    /// The whole point of the painted flag. A renderer that draws a uniform
    /// buffer is fast, and it is fast because it did nothing - which is a
    /// broken harness reporting a healthy number, the failure shape this
    /// project rates worse than an admitted gap.
    #[test]
    fn frames_that_painted_nothing_are_unmeasurable_however_fast_they_were() {
        let mut stats = FrameStats::default();
        for _ in 0..10 {
            stats.record(frame(Duration::from_micros(5), false));
        }
        assert_eq!(stats.frames(), 10);
        assert_eq!(stats.verdict(), Verdict::Unmeasurable);
    }

    #[test]
    fn a_run_inside_the_budget_passes() {
        let mut stats = FrameStats::default();
        stats.record(frame(Duration::from_millis(3), true));
        stats.record(frame(Duration::from_millis(5), true));
        assert_eq!(stats.verdict(), Verdict::Pass);
        assert_eq!(stats.min(), Some(Duration::from_millis(3)));
        assert_eq!(stats.max(), Some(Duration::from_millis(5)));
        assert_eq!(stats.mean(), Some(Duration::from_millis(4)));
    }

    /// One frame over is the whole verdict: a budget written "< 16 ms (one
    /// frame)" is about the frame the user sees drop, not about an average
    /// that hides it.
    #[test]
    fn a_single_frame_over_budget_fails_the_run() {
        let mut stats = FrameStats::default();
        for _ in 0..99 {
            stats.record(frame(Duration::from_millis(1), true));
        }
        stats.record(frame(FRAME_BUDGET, true));
        assert_eq!(stats.verdict(), Verdict::Over);
    }

    /// Exactly at the budget is over: the row reads `< 16 ms`, not `<= 16 ms`.
    #[test]
    fn the_budget_is_exclusive_because_the_row_says_less_than() {
        let mut stats = FrameStats::default();
        stats.record(frame(FRAME_BUDGET, true));
        assert_eq!(stats.verdict(), Verdict::Over);

        let mut inside = FrameStats::default();
        inside.record(frame(
            FRAME_BUDGET
                .checked_sub(Duration::from_nanos(1))
                .expect("a budget of 16 ms is more than a nanosecond"),
            true,
        ));
        assert_eq!(inside.verdict(), Verdict::Pass);
    }

    /// A mixed run is measurable: one painted frame is enough to prove the
    /// renderer drew, and the timings of the rest still count.
    #[test]
    fn one_painted_frame_makes_the_run_measurable() {
        let mut stats = FrameStats::default();
        stats.record(frame(Duration::from_millis(1), false));
        stats.record(frame(Duration::from_millis(2), true));
        assert_eq!(stats.verdict(), Verdict::Pass);
    }

    /// The check the buffer scan cannot make. A frame that laid the window out
    /// at zero size draws nothing, so every pixel in the buffer is whatever the
    /// frame *before* it left there - which a uniformity scan happily calls
    /// painted, and which is the exact shape that made the harness's first
    /// 180-microsecond reading unbelievable.
    #[test]
    fn a_frame_that_drew_no_pixels_is_unmeasurable_even_when_the_buffer_is_not_uniform() {
        let mut stats = FrameStats::default();
        stats.record(Frame {
            elapsed: Duration::from_millis(1),
            drawn_pixels: 0,
            painted: true,
        });
        assert_eq!(stats.verdict(), Verdict::Unmeasurable);
        assert_eq!(stats.least_drawn_pixels(), Some(0));
    }

    /// One empty frame among good ones still poisons the run, which is why the
    /// accumulator keeps the minimum: the timings of a run that stopped drawing
    /// part-way through describe nothing anybody wants to know.
    #[test]
    fn the_stingiest_frame_is_what_the_drawn_area_reports() {
        let mut stats = FrameStats::default();
        stats.record(frame(Duration::from_millis(1), true));
        stats.record(Frame {
            elapsed: Duration::from_millis(1),
            drawn_pixels: 0,
            painted: true,
        });
        stats.record(frame(Duration::from_millis(1), true));
        assert_eq!(stats.frames(), 3);
        assert_eq!(stats.least_drawn_pixels(), Some(0));
        assert_eq!(stats.verdict(), Verdict::Unmeasurable);
    }

    /// The budget constant and `docs/perf-budgets.md` are one decision written
    /// in two places; the doc is what a human reads before deciding a change is
    /// affordable. `xtask`'s size budget has had this test since P8 and the
    /// reason is the same here.
    ///
    /// **The needle names the instrument, not just the number**, and the first
    /// draft of this test did the latter: `perf-budgets.md` already said
    /// "< 16 ms" in the *overlay* row, so a substring search for the number
    /// alone went green before this module's row existed at all - matching the
    /// one row the module header spends a paragraph explaining it does **not**
    /// cover.
    #[test]
    fn the_budget_row_in_perf_budgets_agrees_with_this_constant() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("docs")
                .join("perf-budgets.md"),
        )
        .expect("docs/perf-budgets.md is in the repository");
        let budget = format!("< {} ms", FRAME_BUDGET.as_millis());
        let row = doc
            .lines()
            .find(|line| line.contains(INSTRUMENT))
            .unwrap_or_else(|| panic!("docs/perf-budgets.md has no row naming `{INSTRUMENT}`"));
        assert!(
            row.contains(&budget),
            "the `{INSTRUMENT}` row in docs/perf-budgets.md does not state \
             `{budget}`; it reads: {row}"
        );
    }
}
