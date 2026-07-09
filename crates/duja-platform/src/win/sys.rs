//! Win32 FFI for the event pump: window-class registration, the hidden
//! top-level window, notification registration, the message loop, and the
//! window procedure.
//!
//! Everything that touches `unsafe` FFI lives here behind safe wrappers so the
//! orchestration in [`super`] stays free of `unsafe`. Each `unsafe` block
//! carries a `// SAFETY:` justifying the call.
//!
//! ## Reaching the channel from the window procedure
//!
//! The window procedure is a bare `extern "system"` function with no captured
//! state. We thread a pointer to a heap-allocated [`WindowState`] (which owns
//! the [`Sender`]) through `CreateWindowExW`'s `lpParam`, stash it in the
//! window's `GWLP_USERDATA` slot on `WM_NCCREATE`, and read it back — as a
//! shared reference only — on every later message. The `WindowState` outlives
//! the window because the owning thread keeps the `Box` alive across the whole
//! message loop, and the window is destroyed before that `Box` drops.

use core::cell::Cell;
use core::ffi::c_void;

use crossbeam_channel::Sender;
use windows::Win32::Devices::Display::GUID_DEVINTERFACE_MONITOR;
use windows::Win32::Foundation::{
    ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification, WTSUnRegisterSessionNotification,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DBT_DEVICEARRIVAL, DBT_DEVICEREMOVECOMPLETE,
    DBT_DEVTYP_DEVICEINTERFACE, DEV_BROADCAST_DEVICEINTERFACE_W, DEVICE_NOTIFY_WINDOW_HANDLE,
    DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW, GetWindowLongPtrW,
    HDEVNOTIFY, MSG, PBT_APMRESUMEAUTOMATIC, PBT_APMSUSPEND, PostQuitMessage, RegisterClassExW,
    RegisterDeviceNotificationW, SetWindowLongPtrW, TranslateMessage, UnregisterDeviceNotification,
    WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_DESTROY, WM_DEVICECHANGE, WM_DISPLAYCHANGE,
    WM_NCCREATE, WM_POWERBROADCAST, WM_WTSSESSION_CHANGE, WNDCLASSEXW, WTS_SESSION_UNLOCK,
};
use windows::core::{PCWSTR, w};

use crate::{PlatformError, PlatformEvent};

/// Window-class name for the hidden event sink. Process-wide unique.
const CLASS_NAME: PCWSTR = w!("DujaPlatformEvents");

/// Private teardown message posted from another thread by [`post_shutdown`].
/// `WM_APP` begins the range reserved for application-private messages.
pub(super) const WM_DUJA_SHUTDOWN: u32 = WM_APP + 1;

/// State owned by the pump thread and reached by the window procedure through
/// the window's `GWLP_USERDATA` slot.
pub(super) struct WindowState {
    /// Sink for normalized events. Sends are best-effort: if the receiver is
    /// gone, the event is dropped (never a panic).
    sender: Sender<PlatformEvent>,
    /// The monitor device-interface notification handle, once registered.
    /// Held in a [`Cell`] so the window procedure can `take` it on `WM_DESTROY`
    /// through a shared reference (the state is never aliased mutably).
    dev_notify: Cell<Option<HDEVNOTIFY>>,
}

impl WindowState {
    pub(super) fn new(sender: Sender<PlatformEvent>) -> Self {
        WindowState {
            sender,
            dev_notify: Cell::new(None),
        }
    }

    /// Record the device-notification handle so the window procedure can
    /// unregister it on `WM_DESTROY`. Uses shared (`&self`) access via the
    /// interior [`Cell`] — the state is never borrowed mutably.
    pub(super) fn set_dev_notify(&self, handle: HDEVNOTIFY) {
        self.dev_notify.set(Some(handle));
    }
}

/// Register the window class, tolerating a prior registration.
///
/// The class is registered once per process and never unregistered (harmless,
/// and it dodges a race between concurrent pumps). A second `spawn` finds the
/// class already present, which we treat as success.
///
/// Returns the module handle to reuse as the window's `hInstance`.
pub(super) fn register_class() -> Result<HINSTANCE, PlatformError> {
    // SAFETY: `None` requests the handle of the current process image; the
    // returned handle is not freed by the caller.
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|e| PlatformError::Init(format!("GetModuleHandleW failed: {e}")))?;
    let hinstance = HINSTANCE(module.0);

    let class = WNDCLASSEXW {
        cbSize: u32::try_from(size_of::<WNDCLASSEXW>()).unwrap_or(0),
        lpfnWndProc: Some(wndproc),
        hInstance: hinstance,
        lpszClassName: CLASS_NAME,
        ..Default::default()
    };

    // SAFETY: `class` is fully initialized and lives across the call; the
    // class name and wndproc have 'static lifetime.
    let atom = unsafe { RegisterClassExW(&raw const class) };
    if atom == 0 {
        // SAFETY: reads the calling thread's last-error code, set by the
        // failed RegisterClassExW above.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(PlatformError::Init(format!(
                "RegisterClassExW failed: {err:?}"
            )));
        }
    }
    Ok(hinstance)
}

/// Create the hidden **top-level** window (never shown).
///
/// A top-level window is mandatory: message-only (`HWND_MESSAGE`) windows do
/// not receive `WM_DISPLAYCHANGE`. `state` is handed to the window procedure via
/// `lpParam` and stashed in `GWLP_USERDATA` on `WM_NCCREATE`.
pub(super) fn create_window(
    hinstance: HINSTANCE,
    state: *const WindowState,
) -> Result<HWND, PlatformError> {
    // SAFETY: all handles/strings are valid for the call; `lpParam` carries our
    // `WindowState` pointer, consumed synchronously during WM_NCCREATE.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CLASS_NAME,
            w!("Duja platform event sink"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinstance),
            Some(state.cast::<c_void>()),
        )
    }
    .map_err(|e| PlatformError::Init(format!("CreateWindowExW failed: {e}")))?;
    Ok(hwnd)
}

/// Register for monitor device-interface arrival/removal on `hwnd`.
pub(super) fn register_device_notification(hwnd: HWND) -> Result<HDEVNOTIFY, PlatformError> {
    let mut filter = DEV_BROADCAST_DEVICEINTERFACE_W {
        dbcc_size: u32::try_from(size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>()).unwrap_or(0),
        dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
        dbcc_classguid: GUID_DEVINTERFACE_MONITOR,
        ..Default::default()
    };
    let filter_ptr: *mut c_void = core::ptr::addr_of_mut!(filter).cast();

    // SAFETY: `hwnd` is a live window we own; `filter` is a fully-initialized
    // DEV_BROADCAST_DEVICEINTERFACE_W and outlives the call.
    let handle = unsafe {
        RegisterDeviceNotificationW(HANDLE(hwnd.0), filter_ptr, DEVICE_NOTIFY_WINDOW_HANDLE)
    }
    .map_err(|e| PlatformError::Init(format!("RegisterDeviceNotificationW failed: {e}")))?;
    Ok(handle)
}

/// Register for session change (lock/unlock) notifications on `hwnd`.
pub(super) fn register_session_notification(hwnd: HWND) -> Result<(), PlatformError> {
    // SAFETY: `hwnd` is a live window we own.
    unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) }
        .map_err(|e| PlatformError::Init(format!("WTSRegisterSessionNotification failed: {e}")))
}

/// Destroy the event window (drives `WM_DESTROY`, which unregisters everything).
pub(super) fn destroy_window(hwnd: HWND) {
    // SAFETY: `hwnd` is a live window we own; called only on the owning thread.
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// Post the private teardown message to the window from another thread.
///
/// `PostMessageW` is thread-safe. The window procedure responds by destroying
/// the window, which unregisters notifications and posts `WM_QUIT`.
pub(super) fn post_shutdown(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut c_void);
    // SAFETY: PostMessageW is safe to call cross-thread; a stale HWND simply
    // fails the post (we ignore the result).
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::PostMessageW(
            Some(hwnd),
            WM_DUJA_SHUTDOWN,
            WPARAM(0),
            LPARAM(0),
        )
    };
}

/// Encode an `HWND` as an `isize` handle for cross-thread transport.
pub(super) fn hwnd_to_isize(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

/// Pump this thread's message queue until `WM_QUIT`.
pub(super) fn run_message_loop() {
    let mut msg = MSG::default();
    loop {
        // SAFETY: `msg` is a valid buffer; `None` pumps all windows on this
        // thread. Returns >0 for a message, 0 for WM_QUIT, -1 on error.
        let ret = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
        if ret.0 <= 0 {
            break;
        }
        // SAFETY: `msg` was populated by GetMessageW.
        let _ = unsafe { TranslateMessage(&raw const msg) };
        // SAFETY: `msg` was populated by GetMessageW; dispatched to its wndproc.
        unsafe { DispatchMessageW(&raw const msg) };
    }
}

/// Send an event to the channel, reaching the sender through `GWLP_USERDATA`.
fn emit(hwnd: HWND, event: PlatformEvent) {
    // SAFETY: GWLP_USERDATA holds the `WindowState` pointer we stored on
    // WM_NCCREATE; reading the slot is defined for any window.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr` points at the live `WindowState` owned by the pump thread
    // for the whole lifetime of the window; we take only a shared reference.
    let state = unsafe { &*ptr };
    // Best-effort: a dropped receiver just discards the event, never panics.
    let _ = state.sender.send(event);
}

/// The window procedure. Runs on the pump thread only, re-entrantly during
/// message dispatch — never concurrently — so shared access to `WindowState`
/// is sound.
// RATIONALE (clippy::cast_possible_truncation): the WPARAM discriminants we
// match are small OS-defined constants; narrowing a live message's WPARAM to
// u32 cannot lose information for the messages handled here.
#[allow(clippy::cast_possible_truncation)]
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            // SAFETY: on WM_NCCREATE, lParam is a valid *const CREATESTRUCTW.
            let create = unsafe { &*(lparam.0 as *const CREATESTRUCTW) };
            let state = create.lpCreateParams as isize;
            // SAFETY: storing our state pointer in the window's user slot.
            unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state) };
            // SAFETY: must defer to DefWindowProcW so creation proceeds.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_DISPLAYCHANGE => {
            emit(hwnd, PlatformEvent::DisplaysChanged);
            LRESULT(0)
        }
        WM_DEVICECHANGE => {
            let event = wparam.0 as u32;
            if event == DBT_DEVICEARRIVAL || event == DBT_DEVICEREMOVECOMPLETE {
                emit(hwnd, PlatformEvent::DisplaysChanged);
            }
            LRESULT(1)
        }
        WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                // PBT_APMRESUMEAUTOMATIC fires on every resume (with or without
                // user presence); PBT_APMRESUMESUSPEND is not guaranteed, so we
                // key off RESUMEAUTOMATIC alone. The consumer debounces, so an
                // occasional duplicate is harmless.
                PBT_APMSUSPEND => emit(hwnd, PlatformEvent::Suspending),
                PBT_APMRESUMEAUTOMATIC => emit(hwnd, PlatformEvent::Resumed),
                _ => {}
            }
            LRESULT(1)
        }
        WM_WTSSESSION_CHANGE => {
            if wparam.0 as u32 == WTS_SESSION_UNLOCK {
                emit(hwnd, PlatformEvent::SessionUnlocked);
            }
            LRESULT(0)
        }
        WM_DUJA_SHUTDOWN | WM_CLOSE => {
            destroy_window(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            unregister_all(hwnd);
            // SAFETY: posts WM_QUIT to this thread, ending the message loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        // SAFETY: default handling for every other message.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Unregister every notification while `hwnd` is still valid (called on
/// `WM_DESTROY`, before the window fully tears down).
fn unregister_all(hwnd: HWND) {
    // SAFETY: reading our own GWLP_USERDATA slot.
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowState;
    if !ptr.is_null() {
        // SAFETY: live state owned by the pump thread; shared access only.
        let state = unsafe { &*ptr };
        if let Some(handle) = state.dev_notify.take() {
            // SAFETY: `handle` came from RegisterDeviceNotificationW and has not
            // been unregistered yet (Cell::take guarantees single use).
            let _ = unsafe { UnregisterDeviceNotification(handle) };
        }
    }
    // SAFETY: `hwnd` is still valid during WM_DESTROY; unregistering a session
    // notification that was never registered is harmless.
    let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
}
