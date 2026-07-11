//! The macOS FFI boundary: CoreGraphics enumeration, CoreFoundation container
//! reads, IOKit service iteration, and the two I2C transports.
//!
//! Every `unsafe` block here carries a `// SAFETY:` justification; nothing above
//! this module contains `unsafe`. The private CoreDisplay/`IOAVService` symbols
//! are resolved at runtime with `dlopen`/`dlsym` so their absence degrades
//! gracefully (an empty enumeration) rather than failing to link. Public
//! CoreGraphics/CoreFoundation/IOKit symbols come from the maintained
//! `core-graphics` / `core-foundation` / `io-kit-sys` bindings (ADR-0013).
//!
//! The numeric constants (addresses, timings, transaction types) are quoted
//! from MonitorControl (`Arm64DDC.swift`, `IntelDDC.swift`) and the `ddc-macos`
//! crate; each cites its source. **None of this has run on real hardware** —
//! see the module-level experimental note in `mac`.

// RATIONALE: this is the FFI boundary. Casts between the C ABI's fixed integer
// widths (u32 lengths, CFIndex, CGFloat) and Rust's `usize`/`i32`/`u32` are
// inherent; buffer lengths are tiny and supplied by us, and float→int display
// bounds are saturating (`as` clamps and maps NaN to 0), so no meaningful
// truncation or wrap can occur.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap
)]
// RATIONALE: passing `&mut out_param` to a C function taking a raw pointer is
// the idiomatic FFI call shape (identical to `win::sys`); the borrow lives
// exactly for the synchronous call.
#![allow(clippy::borrow_as_ptr)]
// RATIONALE: the docs cite Apple frameworks/symbols by name (CoreDisplay,
// IOAVService, DCPAVServiceProxy, IOI2CInterface, MonitorControl, ddc-macos) in
// running prose; backticking every mention would hurt readability.
#![allow(clippy::doc_markdown)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::string::CFString;
use core_foundation_sys::base::{CFAllocatorRef, CFRelease};
use core_foundation_sys::data::{CFDataGetBytePtr, CFDataGetLength, CFDataRef};
use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
use core_graphics::display::CGDisplay;
use core_graphics::geometry::CGRect;
use io_kit_sys::types::{io_iterator_t, io_object_t, io_registry_entry_t, io_service_t};
use io_kit_sys::{
    IOIteratorNext, IOObjectRelease, IORegistryEntryCreateCFProperty, IOServiceGetMatchingServices,
    IOServiceMatching,
};

use duja_core::dimmer::DisplayBounds;

use crate::ddcci::{DDC_I2C_ADDRESS, DdcWire, I2cBus};
use crate::transport::TransportError;

// --- constants (all cited) -----------------------------------------------

/// `dlopen` lazy-binding flag.
const RTLD_LAZY: c_int = 0x1;

/// The private framework holding `CoreDisplay_DisplayCreateInfoDictionary` and
/// the `IOAVService*` symbols (`ddc-macos` links this framework directly).
const CORE_DISPLAY_PATH: &CStr = c"/System/Library/Frameworks/CoreDisplay.framework/CoreDisplay";

/// DDC/CI source/sub-address, passed as the Apple Silicon `dataAddress` argument
/// and as the Intel reply sub-address. Source: `Arm64DDC.swift`
/// `ARM64_DDC_DATA_ADDRESS = 0x51`.
const DDC_DATA_ADDRESS: u8 = 0x51;

/// Settle time after a write cycle. Source: `Arm64DDC.swift`
/// `writeSleepTime ?? 10000` µs.
const WRITE_SETTLE: Duration = Duration::from_millis(10);

/// Delay between a write and reading the reply. Source: `Arm64DDC.swift`
/// `readSleepTime ?? 50000` µs (comfortably above the DDC/CI ~40 ms floor).
const READ_DELAY: Duration = Duration::from_millis(50);

/// `kIOI2CSimpleTransactionType`. Source: `ddc-macos` `io2c_interface.rs`.
const IO_I2C_SIMPLE_TX: u32 = 1;
/// `kIOI2CDDCciReplyTransactionType`. Source: `ddc-macos` `io2c_interface.rs`.
const IO_I2C_DDCCI_REPLY_TX: u32 = 2;
/// Intel DDC send address (`0x37 << 1`). Source: `IntelDDC.swift` `sendAddress`.
const DDC_SEND_ADDRESS: u32 = 0x6E;
/// Intel DDC reply address. Source: `IntelDDC.swift` `replyAddress = 0x6F`.
const DDC_REPLY_ADDRESS: u32 = 0x6F;
/// Intel `IOI2CRequest.minReplyDelay` default. Source: `IntelDDC.swift`.
const IO_I2C_MIN_REPLY_DELAY: u64 = 10;

/// The IORegistry property whose value `"External"` marks a DDC-capable AV
/// service (skips the internal panel / internal transports). Source:
/// `Arm64DDC.swift` (`"Location" == "External"`).
const LOCATION_EXTERNAL: &str = "External";

// --- runtime symbol resolution -------------------------------------------

/// `CoreDisplay_DisplayCreateInfoDictionary(displayID) -> CFDictionaryRef`.
type FnCreateInfoDict = unsafe extern "C" fn(u32) -> CFDictionaryRef;
/// `IOAVServiceCreateWithService(allocator, service) -> IOAVServiceRef`.
type FnAvCreate = unsafe extern "C" fn(CFAllocatorRef, io_object_t) -> *mut c_void;
/// `IOAVServiceReadI2C(service, chip, offset, buf, size) -> IOReturn`.
type FnAvRead = unsafe extern "C" fn(*mut c_void, u32, u32, *mut c_void, u32) -> i32;
/// `IOAVServiceWriteI2C(service, chip, dataAddress, buf, size) -> IOReturn`.
type FnAvWrite = unsafe extern "C" fn(*mut c_void, u32, u32, *const c_void, u32) -> i32;

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// The private CoreDisplay symbols, resolved once. Any that is absent stays
/// `None`, and the feature that needs it degrades to "no controllable display".
struct Syms {
    create_info: Option<FnCreateInfoDict>,
    av_create: Option<FnAvCreate>,
    av_read: Option<FnAvRead>,
    av_write: Option<FnAvWrite>,
}

static SYMS: OnceLock<Syms> = OnceLock::new();

/// Resolve `name` from `handle` and reinterpret it as the fn-pointer type `T`.
///
/// # Safety
/// `T` must be the exact `extern "C"` fn-pointer type for the C symbol `name`;
/// a pointer-sized `T` is guaranteed since every `T` here is a fn pointer.
unsafe fn resolve<T>(handle: *mut c_void, name: &CStr) -> Option<T> {
    // SAFETY: `handle` is a live dlopen handle and `name` a valid C string.
    let sym = unsafe { dlsym(handle, name.as_ptr()) };
    if sym.is_null() {
        return None;
    }
    // SAFETY: `sym` is a non-null code address and `T` is a pointer-sized
    // fn-pointer type matching the C symbol's ABI (caller's contract).
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&sym) })
}

/// Load the private symbols (once). The framework handle is intentionally never
/// closed — the symbols live for the process lifetime.
fn syms() -> &'static Syms {
    SYMS.get_or_init(|| {
        // SAFETY: constant framework path; dlopen returns null when absent.
        let handle = unsafe { dlopen(CORE_DISPLAY_PATH.as_ptr(), RTLD_LAZY) };
        if handle.is_null() {
            return Syms {
                create_info: None,
                av_create: None,
                av_read: None,
                av_write: None,
            };
        }
        // SAFETY: each name is paired with its correct fn-pointer type alias.
        unsafe {
            Syms {
                create_info: resolve(handle, c"CoreDisplay_DisplayCreateInfoDictionary"),
                av_create: resolve(handle, c"IOAVServiceCreateWithService"),
                av_read: resolve(handle, c"IOAVServiceReadI2C"),
                av_write: resolve(handle, c"IOAVServiceWriteI2C"),
            }
        }
    })
}

// --- CoreGraphics enumeration --------------------------------------------

/// The active external (non-builtin) display ids.
fn active_external_display_ids() -> Result<Vec<u32>, super::DdcError> {
    let ids = CGDisplay::active_displays().map_err(super::DdcError::CoreGraphics)?;
    Ok(ids
        .into_iter()
        .filter(|&id| !CGDisplay::new(id).is_builtin())
        .collect())
}

/// This display's bounds, in points (see the `DdcDisplay::bounds` docs).
fn display_bounds(id: u32) -> DisplayBounds {
    rect_to_bounds(CGDisplay::new(id).bounds())
}

/// Convert a `CGRect` (float origin + size, points) to [`DisplayBounds`]. The
/// `as` casts are saturating: NaN maps to 0, and a negative extent clamps to 0.
fn rect_to_bounds(rect: CGRect) -> DisplayBounds {
    let x = rect.origin.x as i32;
    let y = rect.origin.y as i32;
    let width = rect.size.width.max(0.0) as u32;
    let height = rect.size.height.max(0.0) as u32;
    DisplayBounds::new(x, y, width, height)
}

// --- EDID via CoreDisplay -------------------------------------------------

/// Read a display's raw EDID from CoreDisplay's info dictionary
/// (`"IODisplayEDIDOriginal"`), the path that works on both Intel and Apple
/// Silicon. Returns `None` when the symbol or the key is absent.
fn read_edid(id: u32) -> Option<Vec<u8>> {
    let create = syms().create_info?;
    // SAFETY: `create` is the resolved CoreDisplay symbol; `id` is a live id.
    let dict = unsafe { create(id) };
    if dict.is_null() {
        return None;
    }
    let key = CFString::from_static_string("IODisplayEDIDOriginal");
    // SAFETY: `dict` is a valid CFDictionaryRef; the key is a valid CFStringRef.
    // The returned value is borrowed (Get rule) for the dictionary's lifetime.
    let value = unsafe { CFDictionaryGetValue(dict, key.as_concrete_TypeRef().cast()) };
    let edid = read_cfdata_bytes(value.cast());
    // SAFETY: `dict` came from a Create call and is released exactly once here.
    unsafe { CFRelease(dict.cast()) };
    edid
}

/// Copy the bytes out of a (possibly null) `CFDataRef`.
fn read_cfdata_bytes(data: CFDataRef) -> Option<Vec<u8>> {
    if data.is_null() {
        return None;
    }
    // SAFETY: `data` is a valid CFDataRef borrowed from its owning dictionary.
    let len = unsafe { CFDataGetLength(data) };
    // SAFETY: same; the byte pointer is valid for `len` bytes.
    let ptr = unsafe { CFDataGetBytePtr(data) };
    if ptr.is_null() || len <= 0 {
        return None;
    }
    let len = usize::try_from(len).unwrap_or(0);
    // SAFETY: `ptr` points to `len` readable bytes owned by the CFData.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec())
}

// --- IOKit service discovery ---------------------------------------------

/// Read a string IORegistry property from `entry`, or `None`.
fn registry_string(entry: io_registry_entry_t, key_name: &str) -> Option<String> {
    let key = CFString::new(key_name);
    // SAFETY: `entry` is a live registry entry and the key a valid CFStringRef;
    // a null allocator means the default. The result follows the Create rule.
    let prop = unsafe {
        IORegistryEntryCreateCFProperty(entry, key.as_concrete_TypeRef(), std::ptr::null(), 0)
    };
    if prop.is_null() {
        return None;
    }
    // SAFETY: `prop` is a +1 CFStringRef; wrap to own it and release on drop.
    let value = unsafe { CFString::wrap_under_create_rule(prop.cast()) };
    Some(value.to_string())
}

/// Whether a `DCPAVServiceProxy` entry is an external (DDC-capable) display.
fn is_external(entry: io_registry_entry_t) -> bool {
    registry_string(entry, "Location").as_deref() == Some(LOCATION_EXTERNAL)
}

/// Iterate `IOServiceMatching(class_name)` and hand each `io_object_t` to `f`.
/// The iterator and every entry are released; `f` must retain anything it keeps.
fn for_each_service(class_name: &CStr, mut f: impl FnMut(io_object_t)) {
    // SAFETY: constant class name; returns a +1 matching dict or null.
    let matching = unsafe { IOServiceMatching(class_name.as_ptr()) };
    if matching.is_null() {
        return;
    }
    let mut iter: io_iterator_t = 0;
    // SAFETY: `matching` is consumed by the call; `iter` receives the iterator.
    // A NULL master port (0) tells IOKit to use the default (kIOMasterPortDefault).
    let rc = unsafe { IOServiceGetMatchingServices(0, matching.cast(), &mut iter) };
    if rc != 0 {
        return;
    }
    loop {
        // SAFETY: `iter` is valid; returns 0 when the enumeration is exhausted.
        let entry = unsafe { IOIteratorNext(iter) };
        if entry == 0 {
            break;
        }
        f(entry);
        // SAFETY: `entry` is a +1 reference from IOIteratorNext, released once.
        unsafe { IOObjectRelease(entry) };
    }
    // SAFETY: `iter` came from IOServiceGetMatchingServices, released once.
    unsafe { IOObjectRelease(iter) };
}

/// Create an `IOAVService` for every external `DCPAVServiceProxy` (Apple
/// Silicon). Empty on Intel (no such nodes) or when the symbol is absent.
fn collect_external_av_services() -> Vec<AvService> {
    let mut out = Vec::new();
    let Some(create) = syms().av_create else {
        return out;
    };
    for_each_service(c"DCPAVServiceProxy", |entry| {
        if is_external(entry) {
            // SAFETY: `entry` is a live io_object; a null allocator means default.
            // The returned service follows the Create rule (+1).
            let svc = unsafe { create(std::ptr::null(), entry) };
            if !svc.is_null() {
                out.push(AvService(svc));
            }
        }
    });
    out
}

/// Collect the `IOFramebuffer` services (Intel fallback path).
fn collect_framebuffers() -> Vec<IoObject> {
    let mut out = Vec::new();
    for_each_service(c"IOFramebuffer", |entry| {
        // `for_each_service` releases the iterator's +1 after this closure, so
        // take our own +1 to keep the entry alive (balanced by IoObject::drop).
        // SAFETY: `entry` is a live io_object during the closure.
        unsafe { IOObjectRetain(entry) };
        out.push(IoObject(entry));
    });
    out
}

// --- owned handles --------------------------------------------------------

/// An owned `IOAVServiceRef` (a CoreFoundation object). Released on drop.
#[derive(Debug)]
struct AvService(*mut c_void);

// SAFETY: the wrapped IOAVServiceRef is owned exclusively by whichever thread
// holds this value (it is moved, never cloned or shared), and Apple's IOAVService
// tolerates single-threaded use from any one thread. No aliasing is possible.
unsafe impl Send for AvService {}

impl Drop for AvService {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: `self.0` is a +1 CoreFoundation ref released exactly once.
            unsafe { CFRelease(self.0.cast_const()) };
        }
    }
}

/// An owned IOKit `io_object_t`. Released on drop.
#[derive(Debug)]
struct IoObject(io_object_t);

// SAFETY: the io_object is owned exclusively by the holding thread (moved, never
// shared); IOKit object references are plain refcounts safe to release from any
// single owning thread.
unsafe impl Send for IoObject {}

impl Drop for IoObject {
    fn drop(&mut self) {
        if self.0 != 0 {
            // SAFETY: `self.0` is a +1 io_object released exactly once.
            unsafe { IOObjectRelease(self.0) };
        }
    }
}

// --- Apple Silicon bus (IOAVService) -------------------------------------

/// The Apple Silicon I2C bus: DDC/CI over the private `IOAVService` symbols.
#[derive(Debug)]
pub struct AvServiceBus {
    service: AvService,
}

impl AvServiceBus {
    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let Some(write) = syms().av_write else {
            return Err(TransportError::backend("IOAVServiceWriteI2C unavailable"));
        };
        // SAFETY: `service` is a live IOAVServiceRef owned by this thread; `data`
        // is a valid buffer of `data.len()` bytes read for the call's duration.
        let rc = unsafe {
            write(
                self.service.0,
                u32::from(DDC_I2C_ADDRESS),
                u32::from(DDC_DATA_ADDRESS),
                data.as_ptr().cast(),
                data.len() as u32,
            )
        };
        std::thread::sleep(WRITE_SETTLE);
        io_return_to_result(rc)
    }

    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        let Some(read) = syms().av_read else {
            return Err(TransportError::backend("IOAVServiceReadI2C unavailable"));
        };
        std::thread::sleep(READ_DELAY);
        let mut buf = vec![0u8; len];
        // SAFETY: `service` is live and owned; `buf` is a writable buffer of
        // `len` bytes valid for the call's duration.
        let rc = unsafe {
            read(
                self.service.0,
                u32::from(DDC_I2C_ADDRESS),
                0,
                buf.as_mut_ptr().cast(),
                buf.len() as u32,
            )
        };
        io_return_to_result(rc).map(|()| buf)
    }
}

// --- Intel bus (IOI2CInterface) ------------------------------------------

/// The IOKit `IOI2CRequest`, byte-for-byte per `ddc-macos`'s
/// `io2c_interface.rs`. **`#[repr(C, packed(4))]` — the padding matters**; get
/// an offset wrong and `IOI2CSendRequest` misreads the struct.
#[repr(C, packed(4))]
// RATIONALE: the field names mirror the C ABI struct exactly so the layout is
// auditable against the header; renaming to snake_case would obscure that.
#[allow(non_snake_case)]
#[derive(Clone, Copy)]
struct IOI2CRequest {
    sendTransactionType: u32,
    replyTransactionType: u32,
    sendAddress: u32,
    replyAddress: u32,
    sendSubAddress: u8,
    replySubAddress: u8,
    __reservedA: [u8; 2],
    minReplyDelay: u64,
    result: i32,
    commFlags: u32,
    __padA: u32,
    sendBytes: u32,
    __reservedB: [u32; 2],
    __padB: u32,
    replyBytes: u32,
    completion: *mut c_void,
    sendBuffer: usize,
    replyBuffer: usize,
    __reservedC: [u32; 10],
}

#[link(name = "IOKit", kind = "framework")]
unsafe extern "C" {
    fn IOFBGetI2CInterfaceCount(framebuffer: io_service_t, count: *mut u32) -> i32;
    fn IOFBCopyI2CInterfaceForBus(
        framebuffer: io_service_t,
        bus: u32,
        interface: *mut io_service_t,
    ) -> i32;
    fn IOI2CInterfaceOpen(interface: io_service_t, options: u32, connect: *mut *mut c_void) -> i32;
    fn IOI2CSendRequest(connect: *mut c_void, options: u32, request: *mut IOI2CRequest) -> i32;
    fn IOI2CInterfaceClose(connect: *mut c_void, options: u32) -> i32;
    fn IOObjectRetain(object: io_object_t) -> i32;
}

/// The Intel I2C bus: DDC/CI over `IOI2CInterface`, opening the framebuffer's
/// I2C bus per transaction (as MonitorControl does). A DDC *get* becomes a
/// send-only transaction followed by a reply-only transaction; a *set* is
/// send-only. This split differs slightly from MonitorControl's single combined
/// request and is unverified on hardware.
#[derive(Debug)]
pub struct FramebufferBus {
    framebuffer: IoObject,
}

impl FramebufferBus {
    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let mut empty: [u8; 0] = [];
        let r = self.transact(data, &mut empty);
        std::thread::sleep(WRITE_SETTLE);
        r
    }

    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        std::thread::sleep(READ_DELAY);
        let mut buf = vec![0u8; len];
        self.transact(&[], &mut buf)?;
        Ok(buf)
    }

    /// Run one I2C transaction, trying each of the framebuffer's I2C buses until
    /// one succeeds.
    fn transact(&self, send: &[u8], reply: &mut [u8]) -> Result<(), TransportError> {
        let mut count: u32 = 0;
        // SAFETY: `framebuffer` is a live io_service_t; `count` receives the
        // interface count.
        let rc = unsafe { IOFBGetI2CInterfaceCount(self.framebuffer.0, &mut count) };
        if rc != 0 || count == 0 {
            return Err(TransportError::Timeout);
        }
        for bus in 0..count {
            if try_bus(self.framebuffer.0, bus, send, reply).is_ok() {
                return Ok(());
            }
        }
        Err(TransportError::Timeout)
    }
}

/// Attempt one send/reply transaction on a single I2C bus of `framebuffer`.
fn try_bus(
    framebuffer: io_service_t,
    bus: u32,
    send: &[u8],
    reply: &mut [u8],
) -> Result<(), TransportError> {
    let mut interface: io_service_t = 0;
    // SAFETY: `framebuffer` is live; `interface` receives a +1 io_service_t.
    let rc = unsafe { IOFBCopyI2CInterfaceForBus(framebuffer, bus, &mut interface) };
    if rc != 0 || interface == 0 {
        return Err(TransportError::Timeout);
    }
    let iface = IoObject(interface);
    let mut connect: *mut c_void = std::ptr::null_mut();
    // SAFETY: `iface.0` is a live I2C interface; `connect` receives the handle.
    let rc = unsafe { IOI2CInterfaceOpen(iface.0, 0, &mut connect) };
    if rc != 0 || connect.is_null() {
        return Err(TransportError::Timeout);
    }
    let mut request = build_request(send, reply);
    // SAFETY: `connect` is an open connection; `request` is a fully initialised
    // IOI2CRequest whose send/reply buffers stay valid for the call.
    let send_rc = unsafe { IOI2CSendRequest(connect, 0, &mut request) };
    // SAFETY: closing the connection we opened above, exactly once.
    unsafe { IOI2CInterfaceClose(connect, 0) };
    // Copy the Copy `result` field out of the packed struct by value.
    let op_result = request.result;
    if send_rc != 0 || op_result != 0 {
        return Err(TransportError::Timeout);
    }
    Ok(())
}

/// Build an `IOI2CRequest` for a send (`send` non-empty) and/or reply (`reply`
/// non-empty) transaction.
fn build_request(send: &[u8], reply: &mut [u8]) -> IOI2CRequest {
    let receiving = !reply.is_empty();
    IOI2CRequest {
        sendTransactionType: IO_I2C_SIMPLE_TX,
        replyTransactionType: if receiving { IO_I2C_DDCCI_REPLY_TX } else { 0 },
        sendAddress: DDC_SEND_ADDRESS,
        replyAddress: DDC_REPLY_ADDRESS,
        sendSubAddress: 0,
        replySubAddress: DDC_DATA_ADDRESS,
        __reservedA: [0; 2],
        minReplyDelay: IO_I2C_MIN_REPLY_DELAY,
        result: 0,
        commFlags: 0,
        __padA: 0,
        sendBytes: send.len() as u32,
        __reservedB: [0; 2],
        __padB: 0,
        replyBytes: reply.len() as u32,
        completion: std::ptr::null_mut(),
        sendBuffer: send.as_ptr() as usize,
        replyBuffer: reply.as_mut_ptr() as usize,
        __reservedC: [0; 10],
    }
}

/// Map an `IOReturn` (`kIOReturnSuccess == 0`) onto the classified transport
/// error. Every non-success is treated as the transient DDC no-reply the
/// controller retries; a truly gone display is removed by re-enumeration higher
/// up (the specific disconnect return codes are not distinguished — see debt).
fn io_return_to_result(rc: i32) -> Result<(), TransportError> {
    if rc == 0 {
        Ok(())
    } else {
        Err(TransportError::Timeout)
    }
}

// --- the unified bus ------------------------------------------------------

/// The macOS I2C bus behind a [`DdcCiTransport`](crate::ddcci::DdcCiTransport):
/// `IOAVService` on Apple Silicon, `IOI2CInterface` on Intel.
#[derive(Debug)]
pub enum MacI2cBus {
    /// Apple Silicon path.
    AppleSilicon(AvServiceBus),
    /// Intel path.
    Intel(FramebufferBus),
}

impl I2cBus for MacI2cBus {
    fn wire(&self) -> DdcWire {
        match self {
            MacI2cBus::AppleSilicon(_) => DdcWire::AppleSilicon,
            MacI2cBus::Intel(_) => DdcWire::Intel,
        }
    }

    fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        match self {
            MacI2cBus::AppleSilicon(bus) => bus.write(data),
            MacI2cBus::Intel(bus) => bus.write(data),
        }
    }

    fn read(&mut self, len: usize) -> Result<Vec<u8>, TransportError> {
        match self {
            MacI2cBus::AppleSilicon(bus) => bus.read(len),
            MacI2cBus::Intel(bus) => bus.read(len),
        }
    }
}

// --- top-level enumeration ------------------------------------------------

/// One controllable external display: its CoreGraphics id, EDID, bounds, and an
/// owned I2C bus.
pub(crate) struct MacDisplay {
    pub cg_id: u32,
    pub edid: Vec<u8>,
    pub bounds: DisplayBounds,
    pub bus: MacI2cBus,
}

/// Enumerate controllable external displays.
///
/// # Display ↔ I2C-service matching (the known hard part)
/// Apple exposes no direct `CGDirectDisplayID` → `IOAVService` link. Duja pairs
/// external displays to external AV services **positionally**, in
/// `CGGetOnlineDisplayList` order: the common single-external-display case is
/// unambiguous, but two or more external displays can be mis-paired because the
/// AV-service iteration order need not track the CoreGraphics order. The
/// documented failure mode is "a brightness change lands on the wrong monitor".
/// MonitorControl solves this by scoring each AV service against every display's
/// EDID attributes (vendor/product/serial/`Location`); porting that EDID-scored
/// match is tracked as debt. The Intel path pairs framebuffers the same way.
pub(crate) fn enumerate_displays() -> Result<Vec<MacDisplay>, super::DdcError> {
    let ids = active_external_display_ids()?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut av = collect_external_av_services();
    let mut framebuffers = if av.is_empty() {
        collect_framebuffers()
    } else {
        Vec::new()
    };

    let mut out = Vec::new();
    for id in ids {
        let Some(edid) = read_edid(id) else {
            continue;
        };
        let bounds = display_bounds(id);
        let bus = if av.is_empty() {
            if framebuffers.is_empty() {
                None
            } else {
                Some(MacI2cBus::Intel(FramebufferBus {
                    framebuffer: framebuffers.remove(0),
                }))
            }
        } else {
            Some(MacI2cBus::AppleSilicon(AvServiceBus {
                service: av.remove(0),
            }))
        };
        let Some(bus) = bus else {
            continue;
        };
        out.push(MacDisplay {
            cg_id: id,
            edid,
            bounds,
            bus,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timings_are_within_ddc_ci_bounds() {
        // The write→read gap must clear the DDC/CI ~40 ms reply floor.
        assert!(READ_DELAY >= Duration::from_millis(40));
        assert!(WRITE_SETTLE < READ_DELAY);
    }

    #[test]
    fn rect_to_bounds_is_total_on_degenerate_input() {
        use core_graphics::geometry::{CGPoint, CGSize};
        // NaN / negative extents must clamp, never panic or wrap.
        let r = CGRect::new(&CGPoint::new(f64::NAN, -10.0), &CGSize::new(-5.0, 100.0));
        let b = rect_to_bounds(r);
        assert_eq!(b.width, 0);
        assert_eq!(b.height, 100);
    }

    #[test]
    fn symbol_resolution_does_not_panic() {
        // On a CI mac the CoreDisplay framework is present; on any host the
        // resolver must simply return without panicking.
        let _ = syms();
    }
}
