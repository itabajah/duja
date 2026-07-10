//! The Windows WMI brightness backend — the crate's confined FFI/`unsafe`
//! module.
//!
//! All raw COM lives here: an [`IWbemLocator`]/[`IWbemServices`] connection to
//! `root\wmi`, reads of `WmiMonitorBrightness` (`CurrentBrightness`, `Level`)
//! and `WmiMonitorID` (identity), and the `WmiMonitorBrightnessMethods`.
//! `WmiSetBrightness` method invocation. Every `unsafe` block carries a
//! `// SAFETY:` note; nothing outside this module is `unsafe`.
//!
//! # Why raw COM
//! The `wmi` crate would pull heavy transitive dependencies for a handful of
//! calls; the plan mandates raw COM through the `windows` crate instead.
//!
//! # Threading
//! [`WmiTransport::open`] initializes a multithreaded COM apartment
//! (`CoInitializeEx(COINIT_MULTITHREADED)`) on the calling thread and connects
//! WMI there. It is designed to be called **on the worker thread that will own
//! the transport**: the app's engine opens controllers through a deferred
//! opener that runs on the worker thread, so `CoInitializeEx`, every WMI call,
//! and the balancing `CoUninitialize` on drop all execute on that one thread —
//! the transport is never constructed on one thread and moved to another.
//! Re-initialization is tolerated: a second init on a thread already in an MTA
//! returns `S_FALSE` (still balanced by an uninit), and a prior STA init returns
//! `RPC_E_CHANGED_MODE`, which we accept without owning (or later releasing) the
//! apartment.

// RATIONALE: `WmiTransport`/`WmiConnection` namespace the WMI backend; the
// `module_name_repetitions` pedantic lint fights the plan's chosen names.
#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeSet;

use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoSetProxyBlanket, CoUninitialize, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_IMP_LEVEL_IMPERSONATE, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayGetLBound, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_GENERIC_FLAG_TYPE, WbemLocator,
};
use windows::core::{BSTR, PCWSTR, VARIANT};

use duja_core::id::StableDisplayId;

use crate::PanelDisplay;
use crate::error::PanelError;
use crate::transport::{PanelBrightness, PanelTransport};

/// `IEnumWbemClassObject::Next` timeout sentinel: block until an object is
/// available or the enumeration ends (`WBEM_INFINITE`).
const WBEM_INFINITE_TIMEOUT: i32 = -1;

// Raw `VARENUM` tag values, compared against the `u16` `vt` field of the
// VARIANT union (which `as_raw` exposes). Kept as locals to avoid coupling to
// the public `VARENUM` newtype's representation.
const VT_UI1_TAG: u16 = 17;
const VT_UI2_TAG: u16 = 18;
const VT_ARRAY_FLAG: u16 = 0x2000;

/// An owned multithreaded COM apartment for the current thread.
///
/// `owns_init` records whether *this* value is responsible for the matching
/// `CoUninitialize`: `true` when our `CoInitializeEx` returned `S_OK`/`S_FALSE`,
/// `false` when it returned `RPC_E_CHANGED_MODE` (the thread was already in an
/// STA we must not tear down).
#[derive(Debug)]
struct ComApartment {
    owns_init: bool,
}

impl ComApartment {
    /// Initialize (or attach to) a COM apartment on the current thread.
    fn init() -> Result<Self, PanelError> {
        // SAFETY: CoInitializeEx takes no borrowed data; we pass a null reserved
        // pointer and a valid apartment flag, and classify every documented
        // HRESULT below. It is balanced by CoUninitialize in Drop when we own
        // the initialization.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == S_OK || hr == S_FALSE {
            Ok(Self { owns_init: true })
        } else if hr == RPC_E_CHANGED_MODE {
            // The thread is already in a (single-threaded) apartment. We can
            // still make calls, but must not uninitialize what we did not own.
            Ok(Self { owns_init: false })
        } else {
            Err(PanelError::Wmi {
                context: "CoInitializeEx",
                hresult: hr.0,
            })
        }
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.owns_init {
            // SAFETY: balanced against exactly one successful CoInitializeEx on
            // this thread; only run when we owned that initialization.
            unsafe { CoUninitialize() };
        }
    }
}

/// A live WMI connection to `root\wmi`, plus the apartment it depends on.
///
/// Field order matters: `services` is dropped before `_apartment`, so the WMI
/// proxy is released while COM is still initialized.
#[derive(Debug)]
struct WmiConnection {
    services: IWbemServices,
    _apartment: ComApartment,
}

// SAFETY: a `WmiConnection` owns an MTA WMI proxy bound to the thread that ran
// `CoInitializeEx`. The `BrightnessController` trait requires `Send`, so this
// impl is what lets a boxed panel controller satisfy that bound. Soundness
// rests on the connection being CONSTRUCTED, used, and dropped on one single
// thread: the app's engine opens each controller through a deferred opener that
// runs ON the worker thread that will own it (it does NOT open on the engine
// thread and move the live proxy across), so `CoInitializeEx`/`CoUninitialize`
// and every WMI call all run on that one thread. The interface pointer is never
// shared; the type is deliberately NOT `Sync`.
unsafe impl Send for WmiConnection {}

impl WmiConnection {
    /// Connect to the `root\wmi` namespace with an impersonation-level proxy.
    fn connect() -> Result<Self, PanelError> {
        let apartment = ComApartment::init()?;

        // SAFETY: CoCreateInstance is given the well-known WbemLocator class id,
        // a null outer unknown, and an in-process server context; it returns an
        // owned interface or an error HRESULT.
        let locator: IWbemLocator = unsafe {
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
        }
        .map_err(|e| PanelError::Wmi {
            context: "CoCreateInstance",
            hresult: e.code().0,
        })?;

        // SAFETY: ConnectServer borrows the BSTRs only for the duration of the
        // call and returns an owned IWbemServices; empty BSTRs select the
        // current user / default locale / no context.
        let services: IWbemServices = unsafe {
            locator.ConnectServer(
                &BSTR::from(r"root\wmi"),
                &BSTR::new(),
                &BSTR::new(),
                &BSTR::new(),
                0,
                &BSTR::new(),
                None,
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "ConnectServer",
            hresult: e.code().0,
        })?;

        // SAFETY: CoSetProxyBlanket configures the just-created services proxy
        // with the caller's identity; all arguments are plain values and the
        // proxy outlives the call.
        unsafe {
            CoSetProxyBlanket(
                &services,
                RPC_C_AUTHN_WINNT,
                RPC_C_AUTHZ_NONE,
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "CoSetProxyBlanket",
            hresult: e.code().0,
        })?;

        Ok(Self {
            services,
            _apartment: apartment,
        })
    }

    /// Run a WQL query and collect every result object.
    fn query(&self, wql: &str) -> Result<Vec<IWbemClassObject>, PanelError> {
        // SAFETY: ExecQuery borrows the two BSTRs for the call and returns an
        // owned enumerator; the flag set requests a forward-only, semi-sync
        // enumeration.
        let enumerator: IEnumWbemClassObject = unsafe {
            self.services.ExecQuery(
                &BSTR::from("WQL"),
                &BSTR::from(wql),
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "ExecQuery",
            hresult: e.code().0,
        })?;

        let mut results = Vec::new();
        loop {
            let mut object: [Option<IWbemClassObject>; 1] = [None];
            let mut returned: u32 = 0;
            // SAFETY: Next fills `object[..returned]` with owned interfaces and
            // writes the count through `returned`; the slice length bounds how
            // many it may write.
            let _ = unsafe {
                enumerator.Next(
                    WBEM_INFINITE_TIMEOUT,
                    &mut object,
                    core::ptr::from_mut(&mut returned),
                )
            };
            if returned == 0 {
                break;
            }
            if let Some(obj) = object[0].take() {
                results.push(obj);
            }
        }
        Ok(results)
    }
}

/// Read a named property of a WMI object as an owned [`VARIANT`].
fn get_variant(object: &IWbemClassObject, name: &str) -> Result<VARIANT, PanelError> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut value = VARIANT::default();
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call;
    // Get writes an owned VARIANT into `value` and ignores the optional type /
    // flavor out-params we pass as None.
    unsafe {
        object.Get(
            PCWSTR::from_raw(wide.as_ptr()),
            0,
            core::ptr::from_mut(&mut value),
            None,
            None,
        )
    }
    .map_err(|e| PanelError::Wmi {
        context: "IWbemClassObject::Get",
        hresult: e.code().0,
    })?;
    Ok(value)
}

/// The `VARENUM` tag of a variant.
fn variant_vt(value: &VARIANT) -> u16 {
    // SAFETY: reading the `vt` tag of the variant union is always valid; every
    // VARIANT initializes it, and `vt` is a plain u16 field.
    unsafe { value.as_raw().Anonymous.Anonymous.vt }
}

/// Read a `VT_BSTR` variant as a Rust string.
fn variant_bstr(value: &VARIANT) -> Result<String, PanelError> {
    // `BSTR::try_from` checks the `VT_BSTR` tag and clones the string out; no
    // raw-union access needed for the safe, owned path.
    let bstr =
        BSTR::try_from(value).map_err(|_| PanelError::Malformed("expected a string property"))?;
    Ok(bstr.to_string())
}

/// Read a scalar `VT_UI1` variant as a `u8`.
fn variant_u8(value: &VARIANT) -> Result<u8, PanelError> {
    if variant_vt(value) != VT_UI1_TAG {
        return Err(PanelError::Malformed("expected a byte property"));
    }
    // SAFETY: the tag check guarantees the active member is the `bVal` byte.
    Ok(unsafe { value.as_raw().Anonymous.Anonymous.Anonymous.bVal })
}

/// Borrow the `SAFEARRAY` behind an array variant, checking the element tag.
///
/// Returns the array pointer and its inclusive `[lower, upper]` bounds.
fn array_bounds(
    value: &VARIANT,
    elem_tag: u16,
) -> Result<(*const SAFEARRAY, i32, i32), PanelError> {
    if variant_vt(value) != (VT_ARRAY_FLAG | elem_tag) {
        return Err(PanelError::Malformed("expected an array property"));
    }
    // SAFETY: the tag check guarantees the active member is the `parray`
    // pointer to a valid, WMI-owned SAFEARRAY. The two `SAFEARRAY` definitions
    // (`windows-core` internal vs `Win32::System::Com`) are layout-identical
    // `#[repr(C)]` types, so the cast is sound.
    let array: *const SAFEARRAY =
        unsafe { value.as_raw().Anonymous.Anonymous.Anonymous.parray }.cast();
    if array.is_null() {
        return Err(PanelError::Malformed("null array"));
    }
    // SAFETY: `array` is a valid single-dimension SAFEARRAY; GetLBound/GetUBound
    // read dimension 1's bounds and return an HRESULT we propagate.
    let lower = unsafe { SafeArrayGetLBound(array, 1) }.map_err(|e| PanelError::Wmi {
        context: "SafeArrayGetLBound",
        hresult: e.code().0,
    })?;
    // SAFETY: same array, same dimension.
    let upper = unsafe { SafeArrayGetUBound(array, 1) }.map_err(|e| PanelError::Wmi {
        context: "SafeArrayGetUBound",
        hresult: e.code().0,
    })?;
    Ok((array, lower, upper))
}

/// Decode a `uint16[]` WMI property (character codes) into a trimmed string.
///
/// `WmiMonitorID` exposes `ManufacturerName`, `ProductCodeID`, `SerialNumberID`
/// and `UserFriendlyName` as arrays of character codes padded with NULs; we keep
/// the printable prefix.
fn variant_u16_array_string(value: &VARIANT) -> Result<String, PanelError> {
    let (array, lower, upper) = array_bounds(value, VT_UI2_TAG)?;
    if upper < lower {
        return Ok(String::new());
    }
    let count = usize::try_from(upper.saturating_sub(lower).saturating_add(1)).unwrap_or(0);
    let mut data: *mut core::ffi::c_void = core::ptr::null_mut();
    // SAFETY: AccessData pins the array and yields a pointer to its contiguous
    // element storage; paired with UnaccessData below before the array is freed.
    unsafe { SafeArrayAccessData(array, core::ptr::from_mut(&mut data)) }.map_err(|e| {
        PanelError::Wmi {
            context: "SafeArrayAccessData",
            hresult: e.code().0,
        }
    })?;
    // SAFETY: `data` points to `count` u16 elements (a uint16[] SAFEARRAY);
    // reading exactly `count` elements stays within the pinned allocation.
    let elems = unsafe { core::slice::from_raw_parts(data.cast::<u16>(), count) };
    let text: String = elems
        .iter()
        .take_while(|&&c| c != 0)
        .filter_map(|&c| char::from_u32(u32::from(c)))
        .filter(|c| !c.is_control())
        .collect();
    // SAFETY: releases the pin taken by SafeArrayAccessData above.
    let _ = unsafe { SafeArrayUnaccessData(array) };
    Ok(text.trim().to_owned())
}

/// Decode a `uint8[]` WMI property (e.g. `Level`) into a `Vec<u8>`.
fn variant_u8_array(value: &VARIANT) -> Result<Vec<u8>, PanelError> {
    let (array, lower, upper) = array_bounds(value, VT_UI1_TAG)?;
    if upper < lower {
        return Ok(Vec::new());
    }
    let count = usize::try_from(upper.saturating_sub(lower).saturating_add(1)).unwrap_or(0);
    let mut data: *mut core::ffi::c_void = core::ptr::null_mut();
    // SAFETY: pins the array and yields its element storage; released below.
    unsafe { SafeArrayAccessData(array, core::ptr::from_mut(&mut data)) }.map_err(|e| {
        PanelError::Wmi {
            context: "SafeArrayAccessData",
            hresult: e.code().0,
        }
    })?;
    // SAFETY: `data` points to `count` u8 elements of the pinned SAFEARRAY.
    let bytes = unsafe { core::slice::from_raw_parts(data.cast::<u8>(), count) }.to_vec();
    // SAFETY: releases the pin taken above.
    let _ = unsafe { SafeArrayUnaccessData(array) };
    Ok(bytes)
}

/// Read the `InstanceName` string property common to the `WmiMonitor*` classes.
fn instance_name_of(object: &IWbemClassObject) -> Result<String, PanelError> {
    variant_bstr(&get_variant(object, "InstanceName")?)
}

/// Build a [`PanelDisplay`] from a `WmiMonitorID` object.
fn panel_from_monitor_id(object: &IWbemClassObject) -> Result<PanelDisplay, PanelError> {
    let instance_name = instance_name_of(object)?;
    let manufacturer = variant_u16_array_string(&get_variant(object, "ManufacturerName")?)?;
    let product_str = variant_u16_array_string(&get_variant(object, "ProductCodeID")?)?;
    let serial = variant_u16_array_string(&get_variant(object, "SerialNumberID")?)?;
    let friendly =
        variant_u16_array_string(&get_variant(object, "UserFriendlyName")?).unwrap_or_default();

    // ProductCodeID arrives as the EDID product code rendered in hex (matching
    // `StableDisplayId::from_edid`'s `{:04X}` field); parse it back to a u16.
    let product_code = u16::from_str_radix(product_str.trim(), 16).unwrap_or(0);
    let serial_opt = if serial.is_empty() {
        None
    } else {
        Some(serial.as_str())
    };
    let id = StableDisplayId::from_parts(&manufacturer, product_code, serial_opt)
        .map_err(|_| PanelError::Malformed("WmiMonitorID manufacturer is not three A-Z letters"))?;

    let name = if friendly.is_empty() {
        "Internal Display".to_owned()
    } else {
        friendly
    };
    Ok(PanelDisplay {
        id,
        name,
        instance_name,
    })
}

/// Enumerate internal panels that expose brightness control (see
/// [`crate::enumerate`]).
///
/// # Errors
/// [`PanelError`] on a COM/WMI failure. An empty `WmiMonitorBrightness` class
/// (the desktop case) is reported as `Ok(vec![])`, not an error.
pub fn enumerate() -> Result<Vec<PanelDisplay>, PanelError> {
    let connection = WmiConnection::connect()?;

    // The set of panels that actually support brightness control. On a desktop
    // this class has no instances, so we return an empty list without ever
    // touching WmiMonitorID.
    let brightness_objects = connection.query("SELECT * FROM WmiMonitorBrightness")?;
    let mut brightness_instances: BTreeSet<String> = BTreeSet::new();
    for object in &brightness_objects {
        brightness_instances.insert(instance_name_of(object)?);
    }
    if brightness_instances.is_empty() {
        return Ok(Vec::new());
    }

    let mut panels = Vec::new();
    for object in connection.query("SELECT * FROM WmiMonitorID")? {
        let instance_name = instance_name_of(&object)?;
        if brightness_instances.contains(&instance_name) {
            panels.push(panel_from_monitor_id(&object)?);
        }
    }
    Ok(panels)
}

/// A [`PanelTransport`] backed by live WMI on a single worker thread.
///
/// Holds its own `root\wmi` connection and the target panel's `InstanceName`;
/// see the [module docs](self) for the COM-threading contract.
#[derive(Debug)]
pub struct WmiTransport {
    connection: WmiConnection,
    instance_name: String,
}

impl WmiTransport {
    /// Open a transport bound to the panel identified by `instance_name`.
    ///
    /// # Errors
    /// [`PanelError`] if the COM apartment or WMI connection cannot be
    /// established.
    pub fn open(instance_name: String) -> Result<Self, PanelError> {
        let connection = WmiConnection::connect()?;
        Ok(Self {
            connection,
            instance_name,
        })
    }

    /// Find the single `WmiMonitor*` object for this transport's panel in the
    /// results of `wql`, matching on `InstanceName` in Rust (so no data ever
    /// enters the WQL string).
    fn find_instance(&self, wql: &str) -> Result<IWbemClassObject, PanelError> {
        for object in self.connection.query(wql)? {
            if instance_name_of(&object)? == self.instance_name {
                return Ok(object);
            }
        }
        // The panel was present at enumeration but is gone now.
        Err(PanelError::Disconnected)
    }

    /// Invoke `WmiMonitorBrightnessMethods.WmiSetBrightness(Timeout, Brightness)`
    /// against the instance at `object_path`.
    fn exec_set_brightness(&self, object_path: &str, percent: u8) -> Result<(), PanelError> {
        // Fetch the method-bearing class, then its in-parameter signature.
        let mut class: Option<IWbemClassObject> = None;
        // SAFETY: GetObject looks up the class by name and writes an owned
        // interface into `class`; the BSTR is borrowed for the call.
        unsafe {
            self.connection.services.GetObject(
                &BSTR::from("WmiMonitorBrightnessMethods"),
                WBEM_GENERIC_FLAG_TYPE::default(),
                None,
                Some(core::ptr::from_mut(&mut class)),
                None,
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "GetObject",
            hresult: e.code().0,
        })?;
        let class = class.ok_or(PanelError::Malformed("WmiMonitorBrightnessMethods missing"))?;

        let method_wide: Vec<u16> = "WmiSetBrightness"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut in_signature: Option<IWbemClassObject> = None;
        // SAFETY: GetMethod borrows the NUL-terminated method name and writes
        // the owned input-signature class into `in_signature`; the unused output
        // signature is passed as a null out-pointer.
        unsafe {
            class.GetMethod(
                PCWSTR::from_raw(method_wide.as_ptr()),
                0,
                core::ptr::from_mut(&mut in_signature),
                core::ptr::null_mut(),
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "GetMethod",
            hresult: e.code().0,
        })?;
        let in_signature =
            in_signature.ok_or(PanelError::Malformed("WmiSetBrightness has no in-params"))?;

        // SAFETY: SpawnInstance allocates a writable instance of the in-params
        // class and returns it owned.
        let in_params = unsafe { in_signature.SpawnInstance(0) }.map_err(|e| PanelError::Wmi {
            context: "SpawnInstance",
            hresult: e.code().0,
        })?;

        put_scalar(&in_params, "Timeout", &VARIANT::from(0u32))?;
        put_scalar(&in_params, "Brightness", &VARIANT::from(percent))?;

        // SAFETY: ExecMethod borrows the object-path / method BSTRs and the
        // in-params instance for the call; we request no out-params or result.
        unsafe {
            self.connection.services.ExecMethod(
                &BSTR::from(object_path),
                &BSTR::from("WmiSetBrightness"),
                WBEM_GENERIC_FLAG_TYPE::default(),
                None,
                &in_params,
                None,
                None,
            )
        }
        .map_err(|e| PanelError::Wmi {
            context: "ExecMethod",
            hresult: e.code().0,
        })?;
        Ok(())
    }
}

impl PanelTransport for WmiTransport {
    fn query(&mut self) -> Result<PanelBrightness, PanelError> {
        let object = self.find_instance("SELECT * FROM WmiMonitorBrightness")?;
        let current = variant_u8(&get_variant(&object, "CurrentBrightness")?)?;
        let levels = variant_u8_array(&get_variant(&object, "Level")?).unwrap_or_default();
        Ok(PanelBrightness { current, levels })
    }

    fn set_brightness(&mut self, percent: u8) -> Result<(), PanelError> {
        let object = self.find_instance("SELECT * FROM WmiMonitorBrightnessMethods")?;
        let path = variant_bstr(&get_variant(&object, "__PATH")?)?;
        self.exec_set_brightness(&path, percent)
    }
}

/// Put a scalar in-parameter (already boxed in a [`VARIANT`]) on a spawned
/// method-argument instance, letting WMI coerce to the property's CIM type.
fn put_scalar(object: &IWbemClassObject, name: &str, value: &VARIANT) -> Result<(), PanelError> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` outlives the call and `value` is a valid owned VARIANT
    // borrowed by Put; a zero CIM type defers to the property's declared type.
    unsafe { object.Put(PCWSTR::from_raw(wide.as_ptr()), 0, value, 0) }.map_err(|e| {
        PanelError::Wmi {
            context: "IWbemClassObject::Put",
            hresult: e.code().0,
        }
    })
}
