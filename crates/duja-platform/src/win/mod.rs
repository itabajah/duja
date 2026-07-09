//! Windows backend for the platform event pump.
//!
//! A dedicated thread creates a hidden top-level window, registers for display,
//! power, and session notifications, and pumps the message queue, translating
//! Win32 messages into [`PlatformEvent`]s. Teardown posts a private message that
//! destroys the window (unregistering everything) and ends the loop; the handle
//! then joins the thread.
//!
//! See [`sys`] for the FFI and the pattern used to reach the channel from the
//! window procedure.

mod sys;

use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use crate::{PlatformError, PlatformEvent};

/// A running Windows event pump: the window handle (as an `isize`, for
/// cross-thread `PostMessageW`) plus the join handle of its thread.
pub struct Pump {
    hwnd: isize,
    join: Option<JoinHandle<()>>,
}

impl Pump {
    /// Spawn the event thread and block until it has created its window and
    /// registered every notification (or failed to).
    pub fn spawn() -> Result<(Self, Receiver<PlatformEvent>), PlatformError> {
        let (tx, rx) = crossbeam_channel::unbounded::<PlatformEvent>();
        // One-shot init channel: the thread reports its HWND, or the error that
        // stopped it, exactly once.
        let (init_tx, init_rx) = std::sync::mpsc::sync_channel::<Result<isize, PlatformError>>(1);

        let join = std::thread::Builder::new()
            .name("duja-platform-events".to_owned())
            .spawn(move || thread_main(tx, &init_tx))
            .map_err(|e| PlatformError::ThreadSpawn(e.to_string()))?;

        match init_rx.recv() {
            Ok(Ok(hwnd)) => Ok((
                Pump {
                    hwnd,
                    join: Some(join),
                },
                rx,
            )),
            Ok(Err(e)) => {
                // Thread reported an init failure and is exiting; reap it.
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                // Thread died before reporting; reap it and surface a generic
                // init error.
                let _ = join.join();
                Err(PlatformError::Init(
                    "event thread exited before initialization".to_owned(),
                ))
            }
        }
    }

    /// Post the teardown message and join the thread. Idempotent: the join
    /// handle is taken on the first call, so later calls (including from `Drop`)
    /// are no-ops.
    pub(crate) fn shutdown(&mut self) {
        if let Some(join) = self.join.take() {
            sys::post_shutdown(self.hwnd);
            let _ = join.join();
        }
    }
}

impl Drop for Pump {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Body of the event thread: build the window, register notifications, report
/// readiness, then pump messages until teardown.
fn thread_main(tx: Sender<PlatformEvent>, init_tx: &SyncSender<Result<isize, PlatformError>>) {
    let hinstance = match sys::register_class() {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    // The state must live for the whole message loop; the window procedure
    // reaches it through GWLP_USERDATA. The heap address is stable across the
    // loop and the window is destroyed before this box drops.
    let state = Box::new(sys::WindowState::new(tx));
    let state_ptr: *const sys::WindowState = &raw const *state;

    let hwnd = match sys::create_window(hinstance, state_ptr) {
        Ok(h) => h,
        Err(e) => {
            let _ = init_tx.send(Err(e));
            return;
        }
    };

    // Register notifications. On failure, destroying the window runs WM_DESTROY,
    // which unregisters whatever succeeded, then we report the error.
    match sys::register_device_notification(hwnd) {
        Ok(handle) => state.set_dev_notify(handle),
        Err(e) => {
            sys::destroy_window(hwnd);
            let _ = init_tx.send(Err(e));
            return;
        }
    }
    if let Err(e) = sys::register_session_notification(hwnd) {
        sys::destroy_window(hwnd);
        let _ = init_tx.send(Err(e));
        return;
    }

    // Ready. Hand the HWND back to the spawner.
    if init_tx.send(Ok(sys::hwnd_to_isize(hwnd))).is_err() {
        // Spawner vanished before receiving; tear down and drain WM_QUIT.
        sys::destroy_window(hwnd);
        sys::run_message_loop();
        return;
    }

    sys::run_message_loop();
    // WM_DESTROY already unregistered notifications and posted WM_QUIT.
    drop(state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::ffi::c_void;
    use std::time::Duration;

    use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        PBT_APMRESUMEAUTOMATIC, PostMessageW, WM_DISPLAYCHANGE, WM_POWERBROADCAST,
        WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK,
    };

    const TIMEOUT: Duration = Duration::from_secs(5);

    /// Post a message to the pump's hidden window.
    fn post(hwnd: isize, msg: u32, wparam: usize) {
        let hwnd = HWND(hwnd as *mut c_void);
        // SAFETY: cross-thread PostMessageW to a window we own and know is live
        // (the pump is still in scope in every caller).
        unsafe {
            PostMessageW(Some(hwnd), msg, WPARAM(wparam), LPARAM(0))
                .expect("PostMessageW to the live pump window");
        }
    }

    #[test]
    fn pump_spawns_and_shuts_down_cleanly() {
        let (pump, _rx) = Pump::spawn().expect("first spawn");
        let handle = pump.join.as_ref().map(|j| j.thread().id());
        assert!(handle.is_some());
        drop(pump); // Drop shuts down and joins.

        // Class registration must not collide on a second spawn.
        let (pump2, _rx2) = Pump::spawn().expect("second spawn after teardown");
        pump2.shutdown_for_test();
    }

    #[test]
    fn posted_displaychange_arrives_as_event() {
        let (pump, rx) = Pump::spawn().expect("spawn");
        post(pump.hwnd, WM_DISPLAYCHANGE, 0);
        assert_eq!(rx.recv_timeout(TIMEOUT), Ok(PlatformEvent::DisplaysChanged));
        pump.shutdown_for_test();
    }

    #[test]
    fn posted_resume_arrives_as_event() {
        let (pump, rx) = Pump::spawn().expect("spawn");
        post(
            pump.hwnd,
            WM_POWERBROADCAST,
            PBT_APMRESUMEAUTOMATIC as usize,
        );
        assert_eq!(rx.recv_timeout(TIMEOUT), Ok(PlatformEvent::Resumed));
        pump.shutdown_for_test();
    }

    #[test]
    fn posted_session_unlock_arrives_as_event() {
        let (pump, rx) = Pump::spawn().expect("spawn");
        post(pump.hwnd, WM_WTSSESSION_CHANGE, WTS_SESSION_UNLOCK as usize);
        assert_eq!(rx.recv_timeout(TIMEOUT), Ok(PlatformEvent::SessionUnlocked));
        pump.shutdown_for_test();
    }

    #[test]
    fn drop_shuts_down() {
        let (pump, _rx) = Pump::spawn().expect("spawn");
        // Dropping must not hang: post-and-join completes promptly.
        drop(pump);
    }

    impl Pump {
        /// Explicit teardown for tests (mirrors `EventPump::shutdown`).
        fn shutdown_for_test(mut self) {
            self.shutdown();
        }
    }
}
