//! The logind write channel: `org.freedesktop.login1.Session.SetBrightness`.
//!
//! This is the *preferred* way to change a backlight on Linux and the reason is
//! permissions, not capability. Writing `/sys/class/backlight/<dev>/brightness`
//! needs root unless the distribution ships a udev rule granting the seat's user
//! access, and many do not. logind grants it to whoever owns the **active
//! session**, which is the person sitting at the machine, without any
//! configuration at all.
//!
//! # Absence is normal, not an error
//!
//! There is no system bus in a container, in an `ssh` session, on a machine not
//! running systemd, and inside many minimal window-manager setups. Every failure
//! here is therefore an ordinary condition the caller falls back from (ADR-0022),
//! and none of it may reach the log as a warning on every start.
//!
//! # The executor this brings with it
//!
//! `zbus::blocking` is a `block_on` wrapper over an async connection that runs
//! its own thread (`"zbus::Connection executor"`). ADR-0022 covers why that is
//! acceptable: the connection already exists in the Linux build through Slint's
//! winit backend, so this is not a new runtime, and Duja's own code stays
//! synchronous per ADR-0005. It *is* a real thread, which is why the connection
//! is made on first write rather than at enumeration: a `dujactl list` that never
//! sets anything should not start one.

use zbus::blocking::Connection;

/// logind's well-known bus name.
const LOGIND: &str = "org.freedesktop.login1";

/// The object path that resolves to the **caller's own** session.
///
/// systemd publishes this alias so a client does not have to ask
/// `GetSessionByPID` for its own id first. Using it means Duja never handles a
/// session id, and therefore cannot address another user's session by mistake.
const SESSION_AUTO: &str = "/org/freedesktop/login1/session/auto";

/// The interface `SetBrightness` lives on.
const SESSION_IFACE: &str = "org.freedesktop.login1.Session";

/// The udev subsystem argument. `"backlight"` for a panel; logind also accepts
/// `"leds"`, which Duja has no use for.
const SUBSYSTEM: &str = "backlight";

/// The method name. Named rather than inlined so the test below can pin it
/// alongside the rest; none of these strings is checked by the compiler.
const METHOD: &str = "SetBrightness";

/// A connection to the system bus, held for the life of a transport.
#[derive(Debug)]
pub(crate) struct Session {
    connection: Connection,
}

impl Session {
    /// Connect to the system bus, or `None` if there is not one.
    ///
    /// Never an error type: every reason this fails — no bus socket, no systemd,
    /// a container, a refused connect — is a condition the caller handles by
    /// using the sysfs channel instead, and modelling them separately would
    /// invite someone to log one of them.
    pub(crate) fn connect() -> Option<Self> {
        Connection::system()
            .ok()
            .map(|connection| Session { connection })
    }

    /// Set `device`'s raw brightness through the active session.
    ///
    /// # Errors
    /// The `zbus::Error` from the call. The interesting ones are
    /// `AccessDenied` (this process does not own the active session, e.g. an
    /// `ssh` login while someone else is at the console) and `UnknownObject`
    /// (there is a system bus but no logind on it).
    pub(crate) fn set_brightness(&self, device: &str, value: u32) -> Result<(), zbus::Error> {
        self.connection.call_method(
            Some(LOGIND),
            SESSION_AUTO,
            Some(SESSION_IFACE),
            METHOD,
            &(SUBSYSTEM, device, value),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every one of these is a string the compiler cannot check and a real seat
    /// is the only thing that would notice a typo — the same reason
    /// `duja-ddc`'s Linux `sys` module pins its ioctl numbers. A wrong bus name
    /// or object path fails at runtime on hardware CI does not have, and the
    /// failure is indistinguishable from "there is no logind here", which the
    /// caller is built to treat as normal. So it would be silent.
    #[test]
    fn the_dbus_names_match_the_logind_interface() {
        assert_eq!(LOGIND, "org.freedesktop.login1");
        assert_eq!(SESSION_AUTO, "/org/freedesktop/login1/session/auto");
        assert_eq!(SESSION_IFACE, "org.freedesktop.login1.Session");
        assert_eq!(METHOD, "SetBrightness");
        assert_eq!(SUBSYSTEM, "backlight");
    }

    /// The object path must be the `auto` alias and not a session id. Duja never
    /// handles a session id precisely so it cannot address another user's
    /// session by mistake, and a path with an id in it would be that mistake.
    #[test]
    fn the_session_path_is_the_self_alias() {
        assert!(SESSION_AUTO.ends_with("/auto"));
        assert!(SESSION_AUTO.starts_with("/org/freedesktop/login1/session/"));
    }

    /// The interface is the **Session** one, not the Manager one. logind puts
    /// `SetBrightness` on the session (it is scoped to who is at the seat) and
    /// `PrepareForSleep` on the manager; swapping them is the plausible slip and
    /// would fail only against a real bus.
    #[test]
    fn brightness_is_on_the_session_interface_not_the_manager() {
        assert!(SESSION_IFACE.ends_with(".Session"));
        assert!(!SESSION_IFACE.ends_with(".Manager"));
    }
}
