//! macOS FFI for the event pump: display-reconfiguration and system-power
//! notifications delivered onto a dedicated thread's `CFRunLoop`.
//!
//! Everything that touches `unsafe` FFI lives here behind safe wrappers so the
//! orchestration in [`super`] stays free of `unsafe`. Each `unsafe` block
//! carries a `// SAFETY:` justifying the call. Run-loop plumbing uses the
//! maintained [`core_foundation_sys`] bindings; the `CoreGraphics` and `IOKit`
//! entry points have no such binding in our dependency set and are declared as
//! `extern "C"` here, resolved by the framework `#[link]`s.
//!
//! ## Reaching the channel from the C callbacks
//!
//! Both callbacks are bare `extern "C"` functions with no captured state. We
//! heap-allocate a [`PumpState`] (which owns the [`Sender`] and the `IOKit`
//! connection needed to acknowledge sleep) and hand its pointer to `CoreGraphics`
//! as `userInfo` and to `IOKit` as `refcon`; each invocation casts it back to a
//! shared reference. The `PumpState` outlives every callback because the pump
//! thread keeps the `Box` alive across the whole run loop and unregisters both
//! callbacks *before* the `Box` drops.
//!
//! ## Threading
//!
//! `IOKit`'s system-power run-loop source and `CoreGraphics`' reconfiguration
//! callback are both delivered on the run loop of the thread that registers
//! them (the classic daemon pattern; see [`super`] for the design note). We
//! therefore register on our dedicated thread and add a private [`StopSource`]
//! so `CFRunLoopRun` blocks even when no display/power source is present.
//!
//! ## Why the stop goes through a source and not `CFRunLoopStop`
//!
//! `CFRunLoopStop` is **not latched**: it takes the loop's lock, and if
//! `_currentMode` is `NULL` — i.e. the loop is not *currently running* — it sets
//! nothing and returns. The request is silently dropped, not remembered.
//!
//! That is a live race here, not a theoretical one. [`run_pump`] reports
//! readiness (handing the caller a [`RunLoopHandle`]) *before* it enters
//! `CFRunLoopRun`, and the caller's readiness channel is buffered, so
//! `Pump::spawn` can return — and the owner can call [`stop`] — while the pump
//! thread is still between those two statements. A `CFRunLoopStop` landing in
//! that window does nothing, and because the stop source keeps the loop alive by
//! construction, `CFRunLoopRun` would then never return and the owner's `join`
//! would block forever.
//!
//! `CFRunLoopSourceSignal` *is* latched: it sets a pending flag on the source
//! object itself, which `CFRunLoopRun` honours on its first pass whether or not
//! the signal arrived before the loop started. So [`stop`] signals the source
//! (and wakes the loop in case it is already blocked); the source's `perform`
//! callback then calls `CFRunLoopStop` from *inside* the loop, where
//! `_currentMode` is non-`NULL` by definition and the call always takes effect.
//!
//! The Windows backend has no equivalent race because `PostMessageW` queues into
//! a window's message queue that already exists when `spawn` returns, and a
//! queued message survives until `GetMessageW` retrieves it.

// RATIONALE (non_snake_case): these are foreign C symbols exported by
// CoreGraphics and IOKit; their names must match the framework exports
// verbatim, so Rust's snake_case convention cannot apply.
#![allow(non_snake_case)]

use core::cell::Cell;
use core::ffi::{c_long, c_void};

use core_foundation_sys::base::{CFIndex, CFRelease, CFRetain};
use core_foundation_sys::runloop::{
    CFRunLoopAddSource, CFRunLoopGetCurrent, CFRunLoopRef, CFRunLoopRemoveSource, CFRunLoopRun,
    CFRunLoopSourceContext, CFRunLoopSourceCreate, CFRunLoopSourceInvalidate, CFRunLoopSourceRef,
    CFRunLoopSourceSignal, CFRunLoopStop, CFRunLoopWakeUp, kCFRunLoopDefaultMode,
};
use core_foundation_sys::string::CFStringRef;
use crossbeam_channel::Sender;

use crate::mac_events::{map_display_flags, map_power_message, power_message_needs_ack};
use crate::{PlatformError, PlatformEvent};

// -- Foreign types --------------------------------------------------------

/// `CGDirectDisplayID` — an opaque display identifier (unused by our mapping).
type CGDirectDisplayID = u32;
/// `CGDisplayChangeSummaryFlags` — the reconfiguration flag bitset.
type CGDisplayChangeSummaryFlags = u32;
/// `CGError` — `kCGErrorSuccess` is `0`.
type CGError = i32;
/// The CoreGraphics reconfiguration callback pointer type.
type CGDisplayReconfigurationCallBack =
    unsafe extern "C" fn(CGDirectDisplayID, CGDisplayChangeSummaryFlags, *mut c_void);

/// `io_connect_t` / `io_object_t` — a Mach port right (`mach_port_t`, `u32`).
type MachPort = u32;
/// `IOReturn` / `kern_return_t`.
type IOReturn = i32;
/// `IONotificationPortRef` — opaque notification-port handle.
type IONotificationPortRef = *mut c_void;
/// The `IOKit` interest callback pointer type.
type IOServiceInterestCallback = unsafe extern "C" fn(*mut c_void, MachPort, u32, *mut c_void);

// RATIONALE (non_snake_case): foreign symbol names, matched verbatim (see the
// module-level rationale; repeated here because attributes do not inherit onto
// the extern block through the inner attribute above on some toolchains).
#[allow(non_snake_case)]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGDisplayRegisterReconfigurationCallback(
        callback: CGDisplayReconfigurationCallBack,
        user_info: *mut c_void,
    ) -> CGError;
    fn CGDisplayRemoveReconfigurationCallback(
        callback: CGDisplayReconfigurationCallBack,
        user_info: *mut c_void,
    ) -> CGError;
}

// RATIONALE (non_snake_case): foreign symbol names, matched verbatim (same as
// the CoreGraphics extern block above; repeated because the attribute does not
// inherit onto the extern block on some toolchains).
#[allow(non_snake_case)]
#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        the_port_ref: *mut IONotificationPortRef,
        callback: IOServiceInterestCallback,
        notifier: *mut MachPort,
    ) -> MachPort;
    fn IODeregisterForSystemPower(notifier: *mut MachPort) -> IOReturn;
    fn IOServiceClose(connect: MachPort) -> IOReturn;
    fn IONotificationPortGetRunLoopSource(port: IONotificationPortRef) -> CFRunLoopSourceRef;
    fn IONotificationPortDestroy(port: IONotificationPortRef);
    fn IOAllowPowerChange(kernel_port: MachPort, notification_id: c_long) -> IOReturn;
}

// -- Shared pump state ----------------------------------------------------

/// State owned by the pump thread and reached by both C callbacks through the
/// `userInfo`/`refcon` pointer.
pub(super) struct PumpState {
    /// Sink for normalized events. Sends are best-effort: a dropped receiver
    /// discards the event rather than panicking.
    sender: Sender<PlatformEvent>,
    /// The `IOKit` root-power connection used to acknowledge sleep, or `0` before
    /// power registration (or when it is unavailable). Held in a [`Cell`] so the
    /// callback can read it through a shared reference; only ever touched on the
    /// pump thread.
    root_port: Cell<MachPort>,
}

impl PumpState {
    fn new(sender: Sender<PlatformEvent>) -> Self {
        PumpState {
            sender,
            root_port: Cell::new(0),
        }
    }
}

/// A retained handle to the pump thread's run loop plus its [`StopSource`], used
/// by the owning [`super::Pump`] to stop the loop from another thread.
///
/// Both references are retained for the handle's whole lifetime, so [`stop`] is
/// safe to call at any time — including after the pump thread has already torn
/// down, where signalling the (by then invalidated) source is a documented
/// no-op rather than a use-after-free.
pub(super) struct RunLoopHandle {
    run_loop: CFRunLoopRef,
    stop_source: CFRunLoopSourceRef,
}

// SAFETY: `CFRunLoop` and `CFRunLoopSource` are documented thread-safe;
// `CFRunLoopSourceSignal`, `CFRunLoopWakeUp` and `CFRelease` may be called from
// any thread. Both references are CFRetain'd in `run_loop_handle` before this
// handle crosses to another thread and CFRelease'd exactly once on drop, so the
// pointers stay live for the handle's whole lifetime.
unsafe impl Send for RunLoopHandle {}

impl Drop for RunLoopHandle {
    fn drop(&mut self) {
        // SAFETY: both were retained when this handle was created; release the
        // single reference we own of each, exactly once.
        unsafe {
            CFRelease(self.stop_source.cast());
            CFRelease(self.run_loop.cast());
        }
    }
}

/// Ask the pump thread's run loop to stop, causing its `CFRunLoopRun` to return.
///
/// Signals the [`StopSource`] rather than calling `CFRunLoopStop` directly, so a
/// stop requested before `CFRunLoopRun` is entered is remembered instead of
/// dropped — see the module docs. Safe to call more than once, and safe after
/// teardown (signalling an invalidated source is a no-op).
pub(super) fn stop(handle: &RunLoopHandle) {
    // SAFETY: both refs are retained and live for the handle's lifetime, and both
    // calls are thread-safe. Signalling latches the request on the source itself;
    // the wake-up only matters when the loop is already blocked, and is harmless
    // otherwise.
    unsafe {
        CFRunLoopSourceSignal(handle.stop_source);
        CFRunLoopWakeUp(handle.run_loop);
    }
}

// -- The thread body ------------------------------------------------------

/// Set up the notification sources on the current thread, report readiness, run
/// the run loop until stopped, then tear everything down.
///
/// `on_ready` is invoked exactly once: with `Ok(handle)` after every source is
/// live (its `bool` return says whether to proceed into the run loop — `false`
/// when the spawner has vanished), or with `Err` if a mandatory source could
/// not be registered (the run loop is never entered).
pub(super) fn run_pump(
    sender: Sender<PlatformEvent>,
    on_ready: impl FnOnce(Result<RunLoopHandle, PlatformError>) -> bool,
) {
    let mode = default_mode();

    // The state must live for the whole run loop; the callbacks reach it via the
    // pointer we pass to CoreGraphics/IOKit. The heap address is stable and the
    // callbacks are removed before this box drops.
    let state = Box::new(PumpState::new(sender));
    let state_ptr: *mut c_void = core::ptr::from_ref(&*state).cast::<c_void>().cast_mut();

    // DisplaysChanged is the mandatory source: mirror the Windows backend and
    // fail initialization if it cannot be registered.
    if let Err(e) = register_reconfiguration(state_ptr) {
        let _ = on_ready(Err(e));
        return;
    }

    // A private source that keeps `CFRunLoopRun` blocked even if the power source
    // is absent (graceful degradation), and that stops the loop when signalled.
    let stop_source = match StopSource::create(mode) {
        Ok(k) => k,
        Err(e) => {
            remove_reconfiguration(state_ptr);
            let _ = on_ready(Err(e));
            return;
        }
    };

    // Suspend/Resume is best-effort: a host without system-power notifications
    // (e.g. a constrained CI runner) yields no power events rather than failing.
    let power = PowerRegistration::register(state_ptr, mode);
    if let Some(ref p) = power {
        state.root_port.set(p.root_port);
    }

    let handle = run_loop_handle(stop_source.source);
    let proceed = on_ready(Ok(handle));
    if proceed {
        // SAFETY: runs this thread's run loop until the stop source performs; the
        // sources above keep it alive. A stop signalled before we got here is
        // already latched on the source and is honoured on the first pass, so
        // this cannot block on a request that arrived during the gap between
        // `on_ready` and this call. Callbacks fire re-entrantly on this thread
        // only.
        unsafe { CFRunLoopRun() };
    }

    // Teardown in both paths, reverse order, all before `state` drops so no
    // callback can fire against freed state.
    if let Some(p) = power {
        p.teardown(mode);
    }
    stop_source.teardown(mode);
    remove_reconfiguration(state_ptr);
    drop(state);
}

/// The default run-loop mode string (a CoreFoundation process-lifetime constant).
fn default_mode() -> CFStringRef {
    // SAFETY: `kCFRunLoopDefaultMode` is a constant global CFString owned by
    // CoreFoundation for the whole process; reading the pointer is always valid.
    unsafe { kCFRunLoopDefaultMode }
}

/// Capture and retain the current thread's run loop, plus the stop source that
/// ends it, for cross-thread stopping.
///
/// `stop_source` must be the source added to *this* run loop by
/// [`StopSource::create`]. Both references are retained here and released in
/// [`RunLoopHandle::drop`], independently of the pump thread's own ownership, so
/// the handle stays valid even after the pump has finished tearing down.
fn run_loop_handle(stop_source: CFRunLoopSourceRef) -> RunLoopHandle {
    // SAFETY: returns this thread's run loop, created lazily on first use.
    let run_loop = unsafe { CFRunLoopGetCurrent() };
    // SAFETY: retain the two references we are about to share with the owning
    // handle; each is released once in `RunLoopHandle::drop`.
    unsafe {
        CFRetain(run_loop.cast());
        CFRetain(stop_source.cast());
    }
    RunLoopHandle {
        run_loop,
        stop_source,
    }
}

// -- CoreGraphics reconfiguration -----------------------------------------

/// Register the display-reconfiguration callback with `state_ptr` as `userInfo`.
fn register_reconfiguration(state_ptr: *mut c_void) -> Result<(), PlatformError> {
    // SAFETY: `reconfig_callback` is a valid 'static callback; CoreGraphics
    // stores `state_ptr` opaquely and passes it back on each invocation. Returns
    // `kCGErrorSuccess` (0) on success.
    let err = unsafe { CGDisplayRegisterReconfigurationCallback(reconfig_callback, state_ptr) };
    if err == 0 {
        Ok(())
    } else {
        Err(PlatformError::Init(format!(
            "CGDisplayRegisterReconfigurationCallback failed: CGError {err}"
        )))
    }
}

/// Unregister the reconfiguration callback (idempotent at the OS level).
fn remove_reconfiguration(state_ptr: *mut c_void) {
    // SAFETY: unregisters the exact (callback, userInfo) pair registered above;
    // a redundant removal is harmless.
    unsafe {
        let _ = CGDisplayRemoveReconfigurationCallback(reconfig_callback, state_ptr);
    }
}

/// CoreGraphics reconfiguration callback: map the flags and emit.
unsafe extern "C" fn reconfig_callback(
    _display: CGDirectDisplayID,
    flags: CGDisplayChangeSummaryFlags,
    user_info: *mut c_void,
) {
    if user_info.is_null() {
        return;
    }
    // SAFETY: `user_info` is the `PumpState` pointer registered above; CoreGraphics
    // invokes this on the pump thread's run loop while the state is alive (it is
    // unregistered before the state drops). Shared access only.
    let state = unsafe { &*user_info.cast::<PumpState>() };
    if let Some(event) = map_display_flags(flags) {
        let _ = state.sender.send(event);
    }
}

// -- IOKit system power ---------------------------------------------------

/// A live registration for system sleep/wake notifications.
struct PowerRegistration {
    port: IONotificationPortRef,
    notifier: MachPort,
    root_port: MachPort,
    source: CFRunLoopSourceRef,
    run_loop: CFRunLoopRef,
}

impl PowerRegistration {
    /// Register for root-domain power notifications and wire the port's source
    /// into the current run loop. Returns `None` (graceful) if the host offers
    /// no such notifications.
    fn register(state_ptr: *mut c_void, mode: CFStringRef) -> Option<Self> {
        let mut port: IONotificationPortRef = core::ptr::null_mut();
        let mut notifier: MachPort = 0;
        // SAFETY: out-params are valid and written by the call; `state_ptr` is
        // stored opaquely as the callback refcon. Returns `MACH_PORT_NULL` (0) on
        // failure.
        let root_port = unsafe {
            IORegisterForSystemPower(state_ptr, &raw mut port, power_callback, &raw mut notifier)
        };
        if root_port == 0 || port.is_null() {
            return None;
        }
        // SAFETY: `port` is the live notification port; the returned source is
        // owned by the port (not separately released).
        let source = unsafe { IONotificationPortGetRunLoopSource(port) };
        if source.is_null() {
            // SAFETY: undo the partial registration in reverse order.
            unsafe {
                let _ = IODeregisterForSystemPower(&raw mut notifier);
                let _ = IOServiceClose(root_port);
                IONotificationPortDestroy(port);
            }
            return None;
        }
        // SAFETY: this thread's run loop is valid; add the port's source in the
        // default mode.
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        // SAFETY: `run_loop`, `source`, and `mode` are all live.
        unsafe { CFRunLoopAddSource(run_loop, source, mode) };
        Some(PowerRegistration {
            port,
            notifier,
            root_port,
            source,
            run_loop,
        })
    }

    /// Remove the source and release every `IOKit` resource, in reverse order.
    fn teardown(mut self, mode: CFStringRef) {
        // SAFETY: `run_loop`/`source` were paired in `register`; deregistering
        // releases the notifier, `IOServiceClose` closes the root-power
        // connection that `IORegisterForSystemPower` implicitly opened, and
        // destroying the port frees its source. Each is done exactly once.
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, mode);
            let _ = IODeregisterForSystemPower(&raw mut self.notifier);
            let _ = IOServiceClose(self.root_port);
            IONotificationPortDestroy(self.port);
        }
    }
}

/// `IOKit` system-power callback: acknowledge sleep transitions and emit.
unsafe extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: MachPort,
    message_type: u32,
    message_argument: *mut c_void,
) {
    if refcon.is_null() {
        return;
    }
    // SAFETY: `refcon` is the `PumpState` pointer passed to
    // `IORegisterForSystemPower`; invoked on the pump thread's run loop while the
    // state is alive (deregistered before the state drops). Shared access only.
    let state = unsafe { &*refcon.cast::<PumpState>() };
    if power_message_needs_ack(message_type) {
        let root_port = state.root_port.get();
        if root_port != 0 {
            // SAFETY: `root_port` is the connection from `IORegisterForSystemPower`;
            // acknowledging with the message argument is mandatory or the system
            // stalls ~30 s waiting on us.
            unsafe {
                let _ = IOAllowPowerChange(root_port, message_argument as c_long);
            }
        }
    }
    if let Some(event) = map_power_message(message_type) {
        let _ = state.sender.send(event);
    }
}

// -- Stop / keep-alive run-loop source ------------------------------------

/// A private `CFRunLoopSource` that serves both liveness and shutdown.
///
/// While unsignalled it never performs, so it does nothing except give the mode a
/// source — which is what keeps `CFRunLoopRun` blocked when no display or power
/// source is present. Signalling it (through [`stop`]) runs
/// [`stop_perform`], which stops the loop from inside.
///
/// One source rather than two because the two jobs are the same object's two
/// states, and because the latching the shutdown path needs is a property of
/// *this* source being in the loop's mode.
struct StopSource {
    source: CFRunLoopSourceRef,
    run_loop: CFRunLoopRef,
}

impl StopSource {
    fn create(mode: CFStringRef) -> Result<Self, PlatformError> {
        let mut context = CFRunLoopSourceContext {
            version: 0,
            info: core::ptr::null_mut(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: stop_perform,
        };
        // SAFETY: `context` is fully initialized and outlives the call (CF copies
        // it); a null allocator selects the default allocator.
        let source =
            unsafe { CFRunLoopSourceCreate(core::ptr::null(), 0 as CFIndex, &raw mut context) };
        if source.is_null() {
            return Err(PlatformError::Init(
                "CFRunLoopSourceCreate returned null".to_owned(),
            ));
        }
        // SAFETY: this thread's run loop is valid; add the source in `mode`.
        let run_loop = unsafe { CFRunLoopGetCurrent() };
        // SAFETY: `run_loop`, `source`, and `mode` are all live.
        unsafe { CFRunLoopAddSource(run_loop, source, mode) };
        Ok(StopSource { source, run_loop })
    }

    fn teardown(self, mode: CFStringRef) {
        // SAFETY: remove the source from the loop it was added to, invalidate it,
        // and release the single owning reference from `CFRunLoopSourceCreate`.
        // Any `RunLoopHandle` holds its own retain, so the object itself outlives
        // this and a late `stop` signal lands on an invalidated (inert) source
        // rather than freed memory.
        unsafe {
            CFRunLoopRemoveSource(self.run_loop, self.source, mode);
            CFRunLoopSourceInvalidate(self.source);
            CFRelease(self.source.cast());
        }
    }
}

/// The stop source's perform callback: stop the loop it is running on.
///
/// Runs on the pump thread inside `CFRunLoopRun`, so `_currentMode` is non-`NULL`
/// and `CFRunLoopStop` always takes effect — the property the module docs explain
/// we cannot rely on when calling it from outside.
extern "C" fn stop_perform(_info: *const c_void) {
    // SAFETY: called by CoreFoundation on the run loop's own thread while that
    // loop is running; `CFRunLoopGetCurrent` returns it and stopping it is the
    // documented way to end `CFRunLoopRun` from within a callback.
    unsafe { CFRunLoopStop(CFRunLoopGetCurrent()) };
}
