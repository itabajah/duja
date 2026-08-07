//! Running one blocking probe with a deadline, so a wedged display server costs
//! a mis-placed window instead of a frozen application.
//!
//! # Why this exists at all
//!
//! `linux::geometry::cursor_anchor` opens an X11 connection, makes several round
//! trips and reads X resource files out of `$HOME`. None of that is bounded:
//! x11rb 0.13.2 sets no connect or read timeout anywhere, so a hung server hangs
//! the caller — and so does a `$HOME` on an unresponsive network mount, with no
//! server involved at all. That was tolerable while the only caller was a CLI
//! invocation. P7 wave 5 put it on the Slint main thread, on every flyout open.
//!
//! # Why a thread and not the timeout x11rb actually offers
//!
//! x11rb *does* have a knob, and `docs/debt.md`'s row for this proposed it:
//! `rust_connection::Stream` is a public trait and `RustConnection::connect_to_stream`
//! is generic over it, so a wrapper whose `poll` carries a deadline bounds every
//! wait that goes through the stream. It was the right idea and it is the wrong
//! fix, because it bounds only what goes through the stream — and **neither** of
//! the two hazards that row names does:
//!
//! - the socket connect happens in `DefaultStream::connect`, before any `Stream`
//!   exists to wrap, and it is the TCP one (a remote `DISPLAY`) that can hang for
//!   minutes;
//! - the resource-database file reads never touch the server, so no amount of
//!   protocol-level timeout can see them.
//!
//! Taking that route also costs re-implementing display-string parsing and the
//! xauth lookup that `x11rb::connect` does for free. A deadline around the *whole*
//! call bounds all three failure sites, in a fraction of the code, and needs to
//! know nothing about X11 — which is why this module names no x11rb type and is
//! compiled (and tested) on every lane.
//!
//! # What it costs, honestly
//!
//! **A leaked thread per timeout.** Nothing cancels a blocked `connect(2)` or a
//! blocked `read(2)` on an NFS mount, so the worker stays parked until its call
//! returns, which on a truly wedged server is never. [`probe_within`] therefore
//! caps the damage at **one**: while a probe is outstanding, later calls take the
//! fallback immediately rather than spawning a second. A user clicking the tray at
//! a dead X server gets a mis-placed flyout every time and one parked thread in
//! total.
//!
//! **A thread spawn on the happy path.** Tens of microseconds against a socket
//! connect and five round trips, so it is noise — but it is not nothing, and it is
//! the reason this is not applied to paths that already run off the UI thread.
//!
//! What it deliberately does *not* do is cache. The row that asked for this
//! suggested caching the resource database first, and a cache is the wrong tool
//! for the failure: it removes the file reads from the *second* call onward and
//! leaves the first one — still on the main thread — exactly as exposed. It also
//! buys a staleness question (winit reloads its copy on `PropertyNotify`; there is
//! no invalidation here) for a problem the deadline already covers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// The worker's thread name, so it is identifiable in a debugger or a core dump —
/// which matters more here than usual, because the whole point is that one of
/// these can still be sitting there.
const THREAD_NAME: &str = "duja-bounded-probe";

/// Run `probe` on a worker thread and give up on it after `deadline`.
///
/// Returns what `probe` returned, or [`None`] if it did not answer in time, if a
/// previous probe is still outstanding, or if the thread could not be spawned. The
/// caller treats all four the same way, which is the property that makes this
/// usable at all: [`crate::geometry::cursor_anchor`] promises never to fail and
/// substitutes a fallback anchor, so "no answer" is a cosmetic outcome rather than
/// an error to handle.
///
/// `outstanding` is the caller's own latch — a `&'static AtomicBool`, one per call
/// site, rather than a single global — so two unrelated probes cannot block each
/// other. Claimed with a `compare_exchange` so two threads racing into the same
/// call site cannot both spawn.
///
/// # The latch is released by a `Drop` guard, and that is not defensive coding
///
/// If the worker panicked between claiming the latch and clearing it, the latch
/// would stay set for the life of the process and **every** later call would take
/// the fallback — a permanent, silent degradation caused by a transient fault. The
/// guard makes the release run on the unwind path too. (A panic here is not
/// hypothetical: this crate denies `panic!` in its own code, which says nothing
/// about the dependency the probe is calling into.)
pub(crate) fn probe_within<T: Send + 'static>(
    outstanding: &'static AtomicBool,
    deadline: Duration,
    probe: impl FnOnce() -> Option<T> + Send + 'static,
) -> Option<T> {
    if outstanding
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        // A previous probe has not come back. Spawning another would park a second
        // thread on the same wedged resource and answer no sooner.
        return None;
    }

    // Capacity 1 and `SyncSender`: the worker must be able to hand over its result
    // and finish even though this side has long since stopped listening. A
    // rendezvous channel would park it here forever on every timeout — turning the
    // one leaked thread this module tolerates into one that is leaked by *its own*
    // design rather than by the wedged server.
    let (tx, rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name(THREAD_NAME.to_owned())
        .spawn(move || {
            let _release = Unlatch(outstanding);
            let _ = tx.send(probe());
        });
    if spawned.is_err() {
        // Nothing will ever clear the latch, so this call has to.
        outstanding.store(false, Ordering::Release);
        return None;
    }

    // `Disconnected` (the worker panicked before sending) arrives immediately and
    // is the same answer as `Timeout`: no anchor, take the fallback.
    rx.recv_timeout(deadline).ok().flatten()
}

/// Clears the caller's latch when the worker finishes, however it finishes.
struct Unlatch(&'static AtomicBool);

impl Drop for Unlatch {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::{THREAD_NAME, probe_within};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    /// Long enough that a loaded CI runner cannot finish it inside the deadlines
    /// below by accident, short enough that a leaked worker is gone before the
    /// suite is.
    const SLOW: Duration = Duration::from_millis(1_500);
    /// The deadline the timeout tests use. Generously above any scheduling jitter
    /// and far below [`SLOW`].
    const SHORT: Duration = Duration::from_millis(60);

    /// Wait for `latch` to clear, up to a generous bound.
    ///
    /// Polled rather than joined because the worker is deliberately detached —
    /// there is no handle to join, which is the whole point of the design.
    fn wait_for_release(latch: &AtomicBool) -> bool {
        // `elapsed()` against a start rather than `now() + budget`: the workspace
        // denies `arithmetic_side_effects`, and `Instant + Duration` can overflow.
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            if !latch.load(Ordering::Acquire) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_probe_that_answers_in_time_is_returned_verbatim() {
        static LATCH: AtomicBool = AtomicBool::new(false);
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || Some(41 + 1)),
            Some(42)
        );
        // And a probe that answers `None` in time is not confused with a timeout by
        // the caller — both are `None`, which is exactly why the latch must be
        // clear afterwards either way.
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || Option::<u8>::None),
            None
        );
        assert!(
            wait_for_release(&LATCH),
            "a finished probe clears the latch"
        );
    }

    #[test]
    fn a_probe_that_overruns_returns_without_waiting_for_it() {
        // The property the whole module exists for: the caller comes back on the
        // deadline, not on the probe. Before this, `cursor_anchor` returned when
        // X11 did, which on a wedged server is never — on the Slint main thread.
        static LATCH: AtomicBool = AtomicBool::new(false);
        let started = Instant::now();
        let answer = probe_within(&LATCH, SHORT, || {
            std::thread::sleep(SLOW);
            Some(7)
        });
        let waited = started.elapsed();

        assert_eq!(answer, None, "an overrunning probe yields the fallback");
        assert!(
            waited < SLOW / 2,
            "returned after {waited:?}, which is not bounded by the {SHORT:?} deadline"
        );
        assert!(
            wait_for_release(&LATCH),
            "the latch clears when it finishes"
        );
    }

    #[test]
    fn a_second_call_while_one_is_outstanding_does_not_spawn_another() {
        // The cap on the damage. A user clicking the tray at a dead X server must
        // accumulate one parked thread, not one per click — so the second call has
        // to refuse *and* refuse immediately, since waiting out the deadline again
        // would be the freeze this module removes, once per click.
        static LATCH: AtomicBool = AtomicBool::new(false);
        assert_eq!(
            probe_within(&LATCH, SHORT, || {
                std::thread::sleep(SLOW);
                Some(1)
            }),
            None
        );
        assert!(
            LATCH.load(Ordering::Acquire),
            "precondition: the first probe is still outstanding"
        );

        let started = Instant::now();
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(30), || Some(2)),
            None,
            "a second call refuses rather than parking a second thread"
        );
        assert!(
            started.elapsed() < SHORT,
            "and refuses immediately, without consulting its own deadline"
        );

        // Once the first one finishes, the site is usable again — the refusal is a
        // latch, not a fuse.
        assert!(wait_for_release(&LATCH));
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || Some(3)),
            Some(3)
        );
    }

    #[test]
    fn a_panicking_probe_does_not_disable_the_call_site_forever() {
        // Without the `Unlatch` guard this is the nasty one: a single transient
        // panic inside a dependency would leave the latch set, and every flyout
        // from then on would open on the fallback anchor with nothing logged and
        // nothing to notice. The panic message below is printed by the default hook
        // and is expected output, not a failure.
        static LATCH: AtomicBool = AtomicBool::new(false);
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || -> Option<u8> {
                panic!("the probe's dependency blew up")
            }),
            None,
            "a panicking probe is a fallback, not a propagated panic"
        );
        assert!(
            wait_for_release(&LATCH),
            "the unwind path must still clear the latch"
        );
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || Some(9)),
            Some(9),
            "and the call site still works afterwards"
        );
    }

    #[test]
    fn the_worker_is_named_so_a_parked_one_can_be_identified() {
        // The design tolerates a thread that never returns, so "which thread is
        // that" has to be answerable from a debugger or a core dump without the
        // source to hand.
        static LATCH: AtomicBool = AtomicBool::new(false);
        assert_eq!(
            probe_within(&LATCH, Duration::from_secs(5), || {
                std::thread::current().name().map(str::to_owned)
            }),
            Some(THREAD_NAME.to_owned())
        );
        assert!(wait_for_release(&LATCH));
    }
}
