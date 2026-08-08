//! What to do with a worker thread that has been asked to stop.
//!
//! One decision, cross-platform on purpose. `docs/debt-archive.md` D-045 is about the
//! Windows overlay worker, whose `shutdown()` did a plain `join()`: a thread
//! wedged inside a hung Win32 call never processes the shutdown post, so the
//! join blocked until it did - which on a quit path means the process does not
//! exit. Stable `std` still has no timed join, and the row named the shape the
//! fix needs: *detach-or-timeout semantics like the engine's*.
//!
//! The mechanism is a channel the worker holds and never sends on. When the
//! thread returns - normally, or by unwinding - its sender drops and the
//! receiver disconnects, so `recv_timeout` distinguishes "finished" from "still
//! going" without the worker having to cooperate. The **decision** taken on that
//! answer is what lives here, because it is decidable without an OS and so
//! belongs where all three lanes compile it (ADR-0011).

// Its only non-test caller is the Windows overlay backend, so on the other two
// lanes everything below is dead in a plain `--lib` build - which
// `clippy -D warnings` fails on, and did. `mac_geom` carries the same attribute
// for the same reason and is the module `lib.rs` compares this one to; the first
// version of this file copied the shape and omitted the one line that makes it
// work, breaking the Linux and macOS lanes in the change whose whole claim was
// that it compiles on all three.
#![cfg_attr(not(windows), allow(dead_code))]

use std::sync::mpsc::RecvTimeoutError;

/// What a caller should do with the join handle of a worker it asked to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Teardown {
    /// The thread has already returned. Joining is free and reaps it properly.
    Join,
    /// The thread is still running past its budget. Drop the handle and carry
    /// on: the alternative is a teardown that never completes.
    Detach,
}

/// Decide from a shutdown wait's outcome.
///
/// # The three answers, and why `Ok` is `Detach`
///
/// - **`Err(Disconnected)`** is the expected success. The worker's sender was
///   dropped, which happens when the closure body returns - so the join will not
///   block *on the wedge*. It is not "guaranteed not to block", which an earlier
///   draft said: the thread still drops the closure's remaining captures and runs
///   its TLS destructors before `join()` returns. Bounded and negligible, and
///   different from the property being claimed.
/// - **`Err(Timeout)`** is the case the row exists for. The thread is wedged, or
///   merely slow; either way the caller cannot wait forever. Usually that is
///   because it is on a quit path, which is what this was written for - not
///   always, since a backend can also be retired mid-session.
/// - **`Ok(())`** should be unreachable - nothing ever sends on that channel -
///   and is treated as `Detach` rather than `Join`. That is deliberate: a value
///   arriving means the channel is not the one this reasoning assumes, and the
///   safe answer under a broken assumption is the one that cannot hang. Joining
///   on a guess is how an unbounded wait comes back.
#[must_use]
pub const fn teardown_decision(outcome: Result<(), RecvTimeoutError>) -> Teardown {
    match outcome {
        Err(RecvTimeoutError::Disconnected) => Teardown::Join,
        Err(RecvTimeoutError::Timeout) | Ok(()) => Teardown::Detach,
    }
}

#[cfg(test)]
mod tests {
    use super::{Teardown, teardown_decision};
    use std::sync::mpsc::{RecvTimeoutError, sync_channel};
    use std::time::Duration;

    /// A worker that has returned disconnects its end, and that is the join.
    #[test]
    fn a_disconnected_channel_means_the_thread_finished() {
        assert_eq!(
            teardown_decision(Err(RecvTimeoutError::Disconnected)),
            Teardown::Join
        );
    }

    /// The case D-045 is about: a wedged worker must not hold up teardown.
    #[test]
    fn a_timeout_detaches_rather_than_blocking_the_quit() {
        assert_eq!(
            teardown_decision(Err(RecvTimeoutError::Timeout)),
            Teardown::Detach
        );
    }

    /// The unreachable answer resolves the safe way.
    ///
    /// Nothing sends on the shutdown channel, so `Ok` means the channel is not
    /// what this module assumes. Under a broken assumption the answer that
    /// cannot hang is the right one - and this is exactly the arm a later
    /// refactor would "tidy" into `Join` for symmetry, which is why it is pinned
    /// rather than left to the `match`.
    #[test]
    fn an_unexpected_value_detaches_too() {
        assert_eq!(teardown_decision(Ok(())), Teardown::Detach);
    }

    /// The premise the decision rests on, with real threads.
    ///
    /// That a dropped sender is what a returned thread looks like from the
    /// outside, and that the wait ends when it has not returned. Both halves,
    /// because either alone would be satisfiable by a mechanism that does not
    /// work.
    ///
    /// **What it does not do**, said plainly because an earlier draft implied
    /// otherwise: it is a hand-written replica sharing no code with
    /// `WindowsDimmer::spawn`, so it cannot detect that call site failing to
    /// hold its sender for the thread's life - which is the single most
    /// defect-prone line this fix adds. See `shutdown`'s doc for the limit and
    /// what closes it.
    #[test]
    fn a_returned_thread_disconnects_and_a_live_one_times_out() {
        let (finished_tx, finished_rx) = sync_channel::<()>(0);
        let handle = std::thread::spawn(move || {
            let _hold = finished_tx;
        });
        handle.join().expect("the thread returns immediately");
        assert_eq!(
            teardown_decision(finished_rx.recv_timeout(Duration::from_secs(5))),
            Teardown::Join,
            "a returned thread is observable without cooperating"
        );

        // And one that has not returned: the wait ends anyway.
        let (busy_tx, busy_rx) = sync_channel::<()>(0);
        let (release_tx, release_rx) = sync_channel::<()>(0);
        let busy = std::thread::spawn(move || {
            let _hold = busy_tx;
            let _ = release_rx.recv();
        });
        assert_eq!(
            teardown_decision(busy_rx.recv_timeout(Duration::from_millis(50))),
            Teardown::Detach,
            "a live thread must not extend the wait past its budget"
        );
        drop(release_tx);
        busy.join().expect("and it is still joinable afterwards");
    }
}
