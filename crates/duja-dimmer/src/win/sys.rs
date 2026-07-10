//! Win32 FFI for the overlay backend: window-class registration, the click-
//! through overlay windows, the hidden control window that wakes the worker,
//! alpha/move primitives, and the message loop.
//!
//! Every `unsafe` block is confined to this module behind a safe wrapper and
//! carries a `// SAFETY:` note; the orchestration in [`super`] stays
//! `unsafe`-free. The overlay recipe is the one proven by the P1 overlay spike
//! (`WS_EX_LAYERED | TRANSPARENT | NOACTIVATE | TOOLWINDOW | TOPMOST`, `WS_POPUP`,
//! `SetLayeredWindowAttributes`, `WM_NCHITTEST → HTTRANSPARENT`,
//! `WDA_EXCLUDEFROMCAPTURE`).

use core::ffi::c_void;

use duja_core::dimmer::{DimmerError, DisplayBounds};
use windows::Win32::Foundation::{
    COLORREF, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, HBRUSH};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HMENU,
    HTTRANSPARENT, HWND_MESSAGE, HWND_TOPMOST, LWA_ALPHA, MSG, PostMessageW, RegisterClassW,
    SW_SHOWNA, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetLayeredWindowAttributes,
    SetWindowDisplayAffinity, SetWindowPos, ShowWindow, TranslateMessage, WDA_EXCLUDEFROMCAPTURE,
    WINDOW_EX_STYLE, WM_APP, WM_NCHITTEST, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::PCWSTR;

/// Overlay window class name. Also used by the live tests to `FindWindowExW` the
/// overlays this backend creates — keep it stable.
pub(super) const OVERLAY_CLASS: PCWSTR = windows::core::w!("DujaDimmerOverlay");
/// Hidden control-window class name (message-only; wakes/stops the worker).
const CONTROL_CLASS: PCWSTR = windows::core::w!("DujaDimmerControl");

/// Private message: drain the command channel and process pending ops.
pub(super) const WM_DUJA_WAKE: u32 = WM_APP + 1;
/// Private message: tear down and end the worker's message loop.
pub(super) const WM_DUJA_SHUTDOWN: u32 = WM_APP + 2;

/// Best-effort: make the process per-monitor DPI-aware so overlay coordinates
/// are physical pixels matching the [`DisplayBounds`] the caller supplies.
///
/// Process-wide and one-shot: if the host app already set an awareness context
/// this simply fails, which is harmless (we ignore the result).
pub(super) fn ensure_dpi_awareness() {
    // SAFETY: no pointers; the call only sets a process-wide flag and returns a
    // BOOL we deliberately ignore (a second call in an already-aware process
    // fails without side effects).
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

/// The current process module handle, for `hInstance`.
pub(super) fn module_handle() -> Result<HINSTANCE, DimmerError> {
    // SAFETY: `None` requests the current process image handle; not freed here.
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|e| DimmerError::Os(format!("GetModuleHandleW failed: {e}")))?;
    Ok(HINSTANCE(module.0))
}

/// Register the overlay and control window classes, tolerating a prior
/// registration (classes are process-global and never unregistered).
pub(super) fn register_classes(hinstance: HINSTANCE) -> Result<(), DimmerError> {
    // Black fill so an overlay with no WM_PAINT still shows black behind its
    // uniform alpha.
    // SAFETY: creates a GDI brush; leaked intentionally for the process lifetime
    // (the class references it until the process exits).
    let black: HBRUSH = unsafe { CreateSolidBrush(COLORREF(0x0000_0000)) };

    let overlay = WNDCLASSW {
        lpfnWndProc: Some(overlay_wndproc),
        hInstance: hinstance,
        lpszClassName: OVERLAY_CLASS,
        hbrBackground: black,
        ..Default::default()
    };
    register_one(&overlay)?;

    let control = WNDCLASSW {
        lpfnWndProc: Some(control_wndproc),
        hInstance: hinstance,
        lpszClassName: CONTROL_CLASS,
        ..Default::default()
    };
    register_one(&control)
}

/// Register one class, treating "already registered" as success.
fn register_one(class: &WNDCLASSW) -> Result<(), DimmerError> {
    // SAFETY: `class` is fully initialized and lives across the call; its name
    // and wndproc have 'static lifetime.
    let atom = unsafe { RegisterClassW(&raw const *class) };
    if atom == 0 {
        // SAFETY: reads this thread's last-error, set by the failed RegisterClassW.
        let err = unsafe { GetLastError() };
        if err != ERROR_CLASS_ALREADY_EXISTS {
            return Err(DimmerError::Os(format!("RegisterClassW failed: {err:?}")));
        }
    }
    Ok(())
}

/// Create the hidden **message-only** control window that receives the worker's
/// private wake/shutdown posts.
pub(super) fn create_control_window(hinstance: HINSTANCE) -> Result<HWND, DimmerError> {
    // SAFETY: all arguments valid; HWND_MESSAGE makes this a message-only window
    // (no display, but it receives posted messages). No creation param needed.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            CONTROL_CLASS,
            windows::core::w!("Duja dimmer control"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| DimmerError::Os(format!("control CreateWindowExW failed: {e}")))?;
    Ok(hwnd)
}

/// Create one click-through black overlay window covering `bounds`, showing it
/// at `alpha` (`1..=255`), excluded from screen capture, and raised topmost.
pub(super) fn create_overlay(
    hinstance: HINSTANCE,
    bounds: DisplayBounds,
    alpha: u8,
) -> Result<HWND, DimmerError> {
    let ex_style =
        WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;

    let (x, y, w, h) = bounds_to_i32(bounds);

    // SAFETY: all arguments valid; no creation param. The window is owned by the
    // calling (worker) thread and destroyed there.
    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            OVERLAY_CLASS,
            windows::core::w!("DujaOverlay"),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            Option::<HMENU>::None,
            Some(hinstance),
            None,
        )
    }
    .map_err(|e| DimmerError::Os(format!("overlay CreateWindowExW failed: {e}")))?;

    // Uniform alpha (colour key unused).
    // SAFETY: `hwnd` is our just-created layered window; LWA_ALPHA uses the alpha
    // byte and ignores the colour key.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA) }
        .map_err(|e| DimmerError::Os(format!("SetLayeredWindowAttributes failed: {e}")))?;

    // Defeat screen capture (BitBlt/Desktop Duplication) of the dimming layer.
    // SAFETY: `hwnd` is our live window.
    unsafe { SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) }
        .map_err(|e| DimmerError::Os(format!("SetWindowDisplayAffinity failed: {e}")))?;

    // Show without activating (no focus steal), then pin to the top of the
    // topmost band.
    // SAFETY: `hwnd` is our live window; SW_SHOWNA never activates.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNA);
    }
    raise_topmost(hwnd);
    Ok(hwnd)
}

/// Set an existing overlay's uniform alpha (`1..=255`).
pub(super) fn set_overlay_alpha(hwnd: HWND, alpha: u8) -> Result<(), DimmerError> {
    // SAFETY: `hwnd` is a live layered overlay we created on this thread.
    unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA) }
        .map_err(|e| DimmerError::Os(format!("SetLayeredWindowAttributes failed: {e}")))
}

/// Move/resize an existing overlay to `bounds` and re-pin it topmost.
pub(super) fn move_overlay(hwnd: HWND, bounds: DisplayBounds) -> Result<(), DimmerError> {
    let (x, y, w, h) = bounds_to_i32(bounds);
    // SAFETY: `hwnd` is a live overlay we own; SWP flags reposition/resize
    // without activating.
    unsafe { SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE) }
        .map_err(|e| DimmerError::Os(format!("SetWindowPos (move) failed: {e}")))
}

/// Destroy a window we own (an overlay, or the control window).
pub(super) fn destroy_window(hwnd: HWND) {
    // SAFETY: `hwnd` is a live window we own, destroyed on its owning thread.
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// Raise a window to the very top of the topmost band without activating it.
fn raise_topmost(hwnd: HWND) {
    let flags = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    // SAFETY: `hwnd` is a live window we own; z-order only, no move/size/activate.
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, flags);
    }
}

/// Convert unsigned [`DisplayBounds`] extents to the `i32` Win32 expects,
/// saturating so a pathologically huge extent can never wrap negative.
fn bounds_to_i32(b: DisplayBounds) -> (i32, i32, i32, i32) {
    let w = i32::try_from(b.width).unwrap_or(i32::MAX);
    let h = i32::try_from(b.height).unwrap_or(i32::MAX);
    (b.x, b.y, w, h)
}

/// Post the wake message to the control window (cross-thread safe).
pub(super) fn post_wake(control: isize) {
    post(control, WM_DUJA_WAKE);
}

/// Post the shutdown message to the control window (cross-thread safe).
pub(super) fn post_shutdown(control: isize) {
    post(control, WM_DUJA_SHUTDOWN);
}

/// Post a private, parameterless message to a control window by raw handle.
fn post(control: isize, msg: u32) {
    let hwnd = hwnd_from_isize(control);
    // SAFETY: PostMessageW is safe cross-thread; a stale handle just fails the
    // post, which we ignore.
    let _ = unsafe { PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0)) };
}

/// Encode an `HWND` as an `isize` for cross-thread transport.
pub(super) fn hwnd_to_isize(hwnd: HWND) -> isize {
    hwnd.0 as isize
}

/// Decode an `isize` handle back into an `HWND`.
pub(super) fn hwnd_from_isize(v: isize) -> HWND {
    HWND(v as *mut c_void)
}

/// One turn of the message loop: block for the next message.
///
/// Returns `Some(message_id)` for a message that must be handled by the worker
/// (private messages are not dispatched to a wndproc), or `None` on `WM_QUIT`.
/// Non-private messages are translated and dispatched here and reported with
/// their id so the worker can ignore them.
pub(super) fn pump_next() -> Option<u32> {
    let mut msg = MSG::default();
    // SAFETY: `msg` is a valid buffer; `None` pumps all windows on this thread.
    // Returns >0 for a message, 0 for WM_QUIT, -1 on error.
    let ret = unsafe { GetMessageW(&raw mut msg, None, 0, 0) };
    if ret.0 <= 0 {
        return None;
    }
    if msg.message == WM_DUJA_WAKE || msg.message == WM_DUJA_SHUTDOWN {
        // Handled in the worker loop; do not dispatch to a wndproc.
        return Some(msg.message);
    }
    // SAFETY: `msg` was populated by GetMessageW.
    unsafe {
        let _ = TranslateMessage(&raw const msg);
        DispatchMessageW(&raw const msg);
    }
    Some(msg.message)
}

/// The overlay window procedure: click-through, nothing else.
///
/// `WM_NCHITTEST → HTTRANSPARENT` guarantees the overlay never intercepts input
/// (belt-and-braces with `WS_EX_TRANSPARENT`); every other message defers to
/// the default handler. Stateless, so it is sound to run re-entrantly on the
/// worker thread.
unsafe extern "system" fn overlay_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCHITTEST {
        return LRESULT(HTTRANSPARENT as isize);
    }
    // SAFETY: default handling for every other message; `hwnd` is the overlay.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The control window procedure: pure default handling. Private wake/shutdown
/// posts are consumed in the worker loop (never dispatched), so this only ever
/// sees incidental system messages.
unsafe extern "system" fn control_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: default handling; `hwnd` is our control window.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
