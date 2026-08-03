//! The suspend/resume listener: logind's `PrepareForSleep` signal.
//!
//! One D-Bus signal carries both halves. logind emits `PrepareForSleep(true)`
//! before the machine goes down and `PrepareForSleep(false)` once it is back, so
//! the boolean is the whole payload and [`classify_prepare_for_sleep`] is the
//! whole mapping.
//!
//! # Absence is normal
//!
//! This is the **system** bus, not the session bus, and the distinction matters
//! because it changes which environments are actually affected. A system bus is
//! machine-wide: an `ssh` login on an ordinary systemd host has one, and has a
//! logind session too — logind is what creates it — so sleep events work there.
//! What an `ssh` login lacks is the *session* bus, which is what
//! [`crate::desktop`]'s theme query uses and which has nothing to do with this.
//!
//! The environments that genuinely have no system bus are containers and
//! machines not running systemd. Every failure here yields *no listener* rather
//! than an error: the pump keeps working for display hot-plug, which comes from
//! the kernel and needs no bus at all. ADR-0022's rule holds either way — a
//! missing bus must never be a `warn!` on every start — and here it holds by
//! construction, since this module logs nothing at all.
//!
//! # Duja does not take an inhibitor lock
//!
//! logind waits for delay-inhibitor holders before suspending, and Duja takes no
//! lock, so `Suspending` is a notification and not a window. That matches what
//! the Windows and macOS pumps already offer, and it is the honest contract: a
//! handler here may or may not finish before the machine goes down.

use crossbeam_channel::Sender;
use zbus::blocking::{Connection, MessageIterator};

use crate::PlatformEvent;
use crate::linux_events::classify_prepare_for_sleep;

/// The match rule for logind's sleep signal. Narrow on purpose: a broader rule
/// makes the bus deliver traffic this thread would only discard.
const MATCH_RULE: &str = "type='signal',\
     sender='org.freedesktop.login1',\
     interface='org.freedesktop.login1.Manager',\
     member='PrepareForSleep',\
     path='/org/freedesktop/login1'";

/// A running suspend/resume listener.
#[derive(Debug)]
pub(crate) struct SleepListener {
    connection: Option<Connection>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SleepListener {
    /// Subscribe to `PrepareForSleep` and start forwarding, or `None` when there
    /// is no system bus.
    ///
    /// Deliberately **not** "or no logind on it": registering a match rule
    /// succeeds whether or not `org.freedesktop.login1` is owned, so on a
    /// systemd-less machine that still has a system bus this returns `Some` and
    /// parks a thread on a signal that will never arrive. Harmless today — the
    /// thread costs nothing and shuts down cleanly — but it is why the docs do
    /// not promise a capability check that is not made. Closing it means a
    /// `GetNameOwner` probe, and it is worth doing when `dujactl doctor` starts
    /// reporting this capability, because a report is where the gap would become
    /// a wrong answer rather than an idle thread.
    ///
    /// Never an error type: every reason this returns `None` is an ordinary
    /// environment, and modelling them separately would invite someone to log
    /// one of them.
    pub(crate) fn spawn(tx: Sender<PlatformEvent>) -> Option<Self> {
        let connection = Connection::system().ok()?;
        let signals = MessageIterator::for_match_rule(MATCH_RULE, &connection, None).ok()?;
        let thread = std::thread::Builder::new()
            .name("duja-logind-sleep".to_owned())
            .spawn(move || listen(signals, &tx))
            .ok()?;
        Some(SleepListener {
            // A clone of the same connection, kept so `shutdown` can close it out
            // from under the blocking iterator. The iterator has no interrupt of
            // its own, and closing the socket is what makes it return.
            connection: Some(connection),
            thread: Some(thread),
        })
    }

    /// Close the connection and join the thread. Idempotent.
    pub(crate) fn shutdown(&mut self) {
        if let Some(connection) = self.connection.take() {
            let _ = connection.close();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for SleepListener {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Forward every `PrepareForSleep` until the connection closes or the receiver
/// goes away.
fn listen(signals: MessageIterator, tx: &Sender<PlatformEvent>) {
    for message in signals {
        let Ok(message) = message else {
            // The iterator yields an error when the connection is closed, which
            // is exactly how `shutdown` ends this thread.
            return;
        };
        let Ok(going_to_sleep) = message.body().deserialize::<bool>() else {
            // A `PrepareForSleep` whose body is not a boolean is not something
            // logind sends; skip it rather than guessing which half it was.
            continue;
        };
        if tx.send(classify_prepare_for_sleep(going_to_sleep)).is_err() {
            return;
        }
    }
}
