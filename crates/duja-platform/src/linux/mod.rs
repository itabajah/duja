//! The Linux event pump: kernel uevents for display hot-plug, logind for
//! suspend and resume.
//!
//! Two sources rather than one, and deliberately not two D-Bus sources.
//! Hot-plug comes from the kernel's `NETLINK_KOBJECT_UEVENT` socket, which works
//! in a container and on a machine with no systemd — the environments where the
//! D-Bus half is absent. Suspend and resume come from logind, because the kernel
//! offers userspace no equivalent notification. ADR-0022 records both halves of
//! that split. (The sleep listener uses the **system** bus, which is
//! machine-wide; see its own docs for why "no bus over `ssh`" is a claim about
//! the *session* bus and does not apply here.)
//!
//! # Partial availability is the normal case
//!
//! [`Pump::spawn`] fails only if the **netlink** socket cannot be opened. A
//! missing logind is not a failure: the pump runs with hot-plug alone.
//!
//! It also does not currently *report* that, and the omission is deliberate
//! rather than forgotten. `duja-platform` has no logger — it takes no `tracing`
//! dependency — and a `sleep_events_available()` accessor with no caller is code
//! that cannot be wrong because nothing reads it. The place this belongs is
//! `dujactl doctor` (wave 5), where a capability report has somewhere to be
//! printed; a `docs/debt.md` row carries it until then. ADR-0022's rule still
//! holds by construction: a missing bus produces no output at all here, which is
//! a stronger form of "never a `warn!` on every start".
//!
//! # What is not delivered here
//!
//! [`PlatformEvent::SessionUnlocked`] has no Linux source yet. logind's `Lock`
//! and `Unlock` signals are *requests to* a session's lock screen rather than
//! reports of its state, so treating them as notifications would fire on a lock
//! that never happened. The state is logind's `LockedHint` property, and
//! watching it means a `PropertiesChanged` subscription that this wave does not
//! add. A `docs/debt.md` row carries it. Nothing breaks meanwhile: the app
//! re-applies on `Resumed` and on `DisplaysChanged`, and an unlock that changes
//! neither is a screen that nothing disturbed.

mod sleep;
mod uevent;

use crossbeam_channel::Receiver;

use crate::{PlatformError, PlatformEvent};

use sleep::SleepListener;
use uevent::UeventListener;

/// The Linux backend: a netlink listener, and a logind listener where there is
/// a logind.
pub(crate) struct Pump {
    uevent: UeventListener,
    sleep: Option<SleepListener>,
}

impl Pump {
    /// Start both listeners and return the receiver they share.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Init`] if the `NETLINK_KOBJECT_UEVENT` socket cannot be
    /// opened or bound. The interesting case is `EPERM`, which would mean this
    /// kernel does not let an unprivileged process onto the multicast group —
    /// an assumption ADR-0022 records as unverified, and the one this error
    /// would falsify.
    pub(crate) fn spawn() -> Result<(Self, Receiver<PlatformEvent>), PlatformError> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let uevent = UeventListener::spawn(tx.clone())
            .map_err(|e| PlatformError::Init(format!("uevent netlink socket: {e}")))?;
        // Deliberately after the netlink listener and deliberately not fatal: a
        // machine with no system bus still gets hot-plug, which is the half that
        // matters for correctness of the display set.
        let sleep = SleepListener::spawn(tx);
        Ok((Pump { uevent, sleep }, rx))
    }

    /// Stop both listeners and join their threads. Idempotent.
    pub(crate) fn shutdown(&mut self) {
        // The netlink listener first: it is the one that always exists, and its
        // stop path is a pipe close rather than a socket teardown, so it is the
        // one guaranteed to return promptly.
        self.uevent.shutdown();
        if let Some(sleep) = self.sleep.as_mut() {
            sleep.shutdown();
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.shutdown();
    }
}
