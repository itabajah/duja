//! The FFI boundary: thin, safe wrappers over the six dxva2 VCP functions, the
//! GDI monitor-enumeration calls, and the `SetupAPI`/registry EDID lookup.
//!
//! Every `unsafe` block here carries a `// SAFETY:` justification. Nothing above
//! this module contains `unsafe`. The Win32 sharp edges encoded here (per
//! ADR-0002): `PHYSICAL_MONITOR` is `repr(packed(1))` so fields are copied out
//! by value, and the VCP functions return a raw `i32` status (0 = failure) while
//! the enumeration/teardown functions return `Result`.

// RATIONALE: this is the FFI boundary. Casts between the Win32 ABI's fixed
// integer widths (u32 lengths, struct sizes) and Rust `usize` are inherent;
// sizes are tiny compile-time constants and lengths are supplied by the API
// itself, so truncation cannot occur in practice.
#![allow(clippy::cast_possible_truncation)]
// RATIONALE: passing `&mut out_param` to a Win32 function that takes a raw
// pointer is the idiomatic FFI call shape; the borrow lives exactly for the
// synchronous call. Spelling every one as `addr_of_mut!` would add noise
// without changing behaviour.
#![allow(clippy::borrow_as_ptr)]

use std::mem::size_of;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICS_FLAG_GLOBAL, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT, DIREG_DEV, SP_DEVICE_INTERFACE_DATA,
    SP_DEVICE_INTERFACE_DETAIL_DATA_W, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiOpenDevRegKey,
};
use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
    DISPLAYCONFIG_TARGET_DEVICE_NAME, DestroyPhysicalMonitor, DisplayConfigGetDeviceInfo,
    GUID_DEVINTERFACE_MONITOR, GetCapabilitiesStringLength, GetDisplayConfigBufferSizes,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    SetVCPFeature,
};
use windows::Win32::Foundation::{
    ERROR_INVALID_HANDLE, ERROR_SUCCESS, GetLastError, HANDLE, LPARAM, RECT, TRUE,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::Registry::{
    HKEY, KEY_READ, REG_VALUE_TYPE, RegCloseKey, RegQueryValueExW,
};
use windows::core::{BOOL, GUID, PCWSTR};

use crate::transport::{TransportError, VcpReading};

/// One active display path resolved via the CCD API: the GDI adapter name, the
/// real monitor device interface path, and the monitor's friendly name.
///
/// This bridges the gap the GDI monitor enumeration leaves on some GPUs (e.g.
/// NVIDIA surfaces a generic `Default_Monitor` with no EDID linkage): the CCD
/// target's `monitorDevicePath` is the true device interface path that keys the
/// registry EDID.
pub(crate) struct MonitorPath {
    /// The GDI adapter device name, lower-cased (e.g. `\\.\display1`).
    pub gdi_device: String,
    /// The monitor device interface path, lower-cased (correlates to the EDID
    /// map key).
    pub interface_path: String,
    /// The monitor's friendly name from the CCD target, if any.
    pub friendly: Option<String>,
}

/// Decode a NUL-terminated (or full) fixed wide buffer into a `String`.
fn wide_to_string(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(buf.get(..end).unwrap_or(buf))
}

/// Decode a NUL-terminated wide string from a raw pointer.
///
/// # Safety
/// `ptr` must be either null or point to a NUL-terminated UTF-16 string that
/// stays valid for the duration of the call.
unsafe fn wide_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }
    let mut len = 0usize;
    // SAFETY: the caller guarantees a NUL-terminated string; we stop at the NUL.
    while unsafe { *ptr.add(len) } != 0 {
        len = len.saturating_add(1);
    }
    // SAFETY: `len` u16s precede the terminator and are all readable.
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    String::from_utf16_lossy(slice)
}

/// Build a NUL-terminated wide string for passing as a `PCWSTR`.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Map a failed dxva2 call to a classified transport error. Best-effort: an
/// invalid handle is terminal (monitor gone); everything else is treated as the
/// transient DDC no-reply the controller retries.
fn classify_failure() -> TransportError {
    // SAFETY: `GetLastError` only reads the calling thread's last-error code.
    let code = unsafe { GetLastError() };
    if code == ERROR_INVALID_HANDLE {
        TransportError::Disconnected
    } else {
        TransportError::Timeout
    }
}

/// The `EnumDisplayMonitors` callback: append each `HMONITOR` to the caller's
/// `Vec`.
unsafe extern "system" fn enum_monitors_cb(
    hmon: HMONITOR,
    _hdc: HDC,
    _clip: *mut RECT,
    data: LPARAM,
) -> BOOL {
    // SAFETY: `data` is the `&mut Vec<HMONITOR>` pointer we passed to
    // EnumDisplayMonitors; it outlives the synchronous enumeration and no other
    // thread accesses it.
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(hmon);
    TRUE
}

/// Enumerate every display `HMONITOR`.
pub(crate) fn enum_hmonitors() -> Vec<HMONITOR> {
    let mut list: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback and `list` pointer are valid for the synchronous
    // duration of EnumDisplayMonitors; errors are non-fatal (empty result).
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitors_cb),
            LPARAM(std::ptr::addr_of_mut!(list) as isize),
        );
    }
    list
}

/// Recover the GDI adapter device name (e.g. `\\.\DISPLAY1`) for `hmon`.
pub(crate) fn gdi_device(hmon: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: `info` begins with a MONITORINFO (cbSize set); the documented
    // calling convention is to pass a MONITORINFOEXW as a MONITORINFO pointer.
    let ok = unsafe { GetMonitorInfoW(hmon, std::ptr::addr_of_mut!(info).cast::<MONITORINFO>()) };
    if !ok.as_bool() {
        return None;
    }
    Some(wide_to_string(&info.szDevice))
}

/// Resolve every active display path to its (GDI adapter name, monitor device
/// interface path, friendly name) via the CCD API. This is the reliable bridge
/// from an `HMONITOR`'s GDI adapter to the real monitor device that owns the
/// registry EDID.
pub(crate) fn monitor_paths() -> Vec<MonitorPath> {
    let mut out = Vec::new();
    let mut num_paths = 0u32;
    let mut num_modes = 0u32;
    // SAFETY: queries the required path/mode buffer sizes for active paths.
    let rc = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut num_paths, &mut num_modes)
    };
    if rc != ERROR_SUCCESS {
        return out;
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); num_paths as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); num_modes as usize];
    // SAFETY: both buffers are sized to the counts just queried.
    let rc = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut num_paths,
            paths.as_mut_ptr(),
            &mut num_modes,
            modes.as_mut_ptr(),
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return out;
    }

    for path in paths.iter().take(num_paths as usize) {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: `source` is a DISPLAYCONFIG_SOURCE_DEVICE_NAME whose header is
        // configured (type/size/adapter/id) for a source-name query.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != 0 {
            continue;
        }
        let gdi = wide_to_string(&source.viewGdiDeviceName);

        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                adapterId: path.targetInfo.adapterId,
                id: path.targetInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: `target` is a DISPLAYCONFIG_TARGET_DEVICE_NAME whose header is
        // configured for a target-name query.
        if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != 0 {
            continue;
        }
        let interface_path = wide_to_string(&target.monitorDevicePath);
        let friendly = {
            let name = wide_to_string(&target.monitorFriendlyDeviceName);
            if name.trim().is_empty() {
                None
            } else {
                Some(name)
            }
        };

        if !gdi.is_empty() && !interface_path.is_empty() {
            out.push(MonitorPath {
                gdi_device: gdi.to_ascii_lowercase(),
                interface_path: interface_path.to_ascii_lowercase(),
                friendly,
            });
        }
    }
    out
}

/// Open the DDC-capable physical-monitor handles behind `hmon`.
///
/// # Errors
/// The Win32 error if the count or handle query fails.
pub(crate) fn physical_monitors(hmon: HMONITOR) -> windows::core::Result<Vec<HANDLE>> {
    let mut count = 0u32;
    // SAFETY: writes the physical-monitor count for `hmon`.
    unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count) }?;
    if count == 0 {
        return Ok(Vec::new());
    }
    let mut monitors = vec![PHYSICAL_MONITOR::default(); count as usize];
    // SAFETY: `monitors` has exactly `count` slots.
    unsafe { GetPhysicalMonitorsFromHMONITOR(hmon, &mut monitors) }?;
    // Copy the HANDLE out of each packed PHYSICAL_MONITOR by value (never
    // reference a packed field; windows-rs #2135).
    Ok(monitors
        .iter()
        .copied()
        .map(|m| m.hPhysicalMonitor)
        .collect())
}

/// Read every monitor's EDID from the registry, keyed by lower-cased device
/// interface path.
///
/// # Errors
/// The Win32 error if the device-information set cannot be opened.
pub(crate) fn collect_monitor_edids() -> windows::core::Result<Vec<(String, Vec<u8>)>> {
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let guid: GUID = GUID_DEVINTERFACE_MONITOR;
    // SAFETY: standard SetupAPI enumeration; the returned set is destroyed below.
    let devinfo_set = unsafe {
        SetupDiGetClassDevsW(
            Some(std::ptr::addr_of!(guid)),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )
    }?;

    let mut index = 0u32;
    loop {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `devinfo_set` is valid and `iface` is sized; the call returns
        // Err (ERROR_NO_MORE_ITEMS) once the enumeration is exhausted.
        let more = unsafe {
            SetupDiEnumDeviceInterfaces(
                devinfo_set,
                None,
                std::ptr::addr_of!(guid),
                index,
                &mut iface,
            )
        };
        if more.is_err() {
            break;
        }
        index = index.saturating_add(1);

        // First call: learn the required detail-buffer size (expected to "fail"
        // with ERROR_INSUFFICIENT_BUFFER, which we ignore).
        let mut required = 0u32;
        // SAFETY: querying the size with a null detail buffer is the documented
        // two-call pattern.
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devinfo_set,
                &iface,
                None,
                0,
                Some(&mut required),
                None,
            )
        };
        if required == 0 {
            continue;
        }

        // Back the detail buffer with `u32` storage so it is aligned for
        // SP_DEVICE_INTERFACE_DETAIL_DATA_W (whose alignment is 4), rounding the
        // byte length up to whole u32 words.
        let words = (required as usize).div_ceil(4);
        let mut buffer = vec![0u32; words];
        let detail = buffer
            .as_mut_ptr()
            .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        // SAFETY: `buffer` spans at least `required` bytes and is 4-aligned; we
        // write only the fixed cbSize header (its value is the header size, not
        // the buffer size, per Win32).
        unsafe {
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }

        let mut devinfo = SP_DEVINFO_DATA {
            cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // SAFETY: `detail` points into `buffer` sized to `required`; `devinfo`
        // is sized.
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devinfo_set,
                &iface,
                Some(detail),
                required,
                None,
                Some(&mut devinfo),
            )
        };
        if ok.is_err() {
            continue;
        }

        // SAFETY: `DevicePath` is a NUL-terminated wide string embedded in
        // `buffer`, which outlives this read.
        let path =
            unsafe { wide_ptr_to_string(std::ptr::addr_of!((*detail).DevicePath).cast::<u16>()) };

        // SAFETY: `devinfo_set` + `devinfo` are valid; returns Err if the device
        // has no hardware key.
        let key = unsafe {
            SetupDiOpenDevRegKey(
                devinfo_set,
                &devinfo,
                DICS_FLAG_GLOBAL.0,
                0,
                DIREG_DEV,
                KEY_READ.0,
            )
        };
        let Ok(hkey) = key else { continue };
        let edid = read_reg_edid(hkey);
        // SAFETY: `hkey` was just opened and is closed exactly once here.
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if let Some(edid) = edid {
            out.push((path.to_ascii_lowercase(), edid));
        }
    }

    // SAFETY: `devinfo_set` came from SetupDiGetClassDevsW and is freed once.
    unsafe {
        let _ = SetupDiDestroyDeviceInfoList(devinfo_set);
    }
    Ok(out)
}

/// Read the `EDID` binary value from an opened device hardware key.
fn read_reg_edid(hkey: HKEY) -> Option<Vec<u8>> {
    let name = wide("EDID");
    let mut value_type = REG_VALUE_TYPE::default();
    let mut len = 0u32;
    // SAFETY: query the value size with a null data buffer (standard pattern).
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut value_type),
            None,
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS || len == 0 {
        return None;
    }
    let mut data = vec![0u8; len as usize];
    // SAFETY: `data` holds `len` bytes, matching the size just queried.
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut value_type),
            Some(data.as_mut_ptr()),
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }
    data.truncate(len as usize);
    Some(data)
}

/// Read a VCP feature, returning the current value and maximum.
///
/// # Errors
/// A [`TransportError`] if the DDC/CI exchange fails.
pub(crate) fn get_vcp(handle: HANDLE, code: u8) -> Result<VcpReading, TransportError> {
    let mut current = 0u32;
    let mut max = 0u32;
    // SAFETY: `handle` is a live physical-monitor handle owned by this thread.
    let ok = unsafe {
        GetVCPFeatureAndVCPFeatureReply(handle, code, None, &mut current, Some(&mut max))
    };
    if ok == 0 {
        return Err(classify_failure());
    }
    Ok(VcpReading {
        current: u16::try_from(current).unwrap_or(u16::MAX),
        max: u16::try_from(max).unwrap_or(u16::MAX),
    })
}

/// Write a VCP feature.
///
/// # Errors
/// A [`TransportError`] if the write is not acknowledged.
pub(crate) fn set_vcp(handle: HANDLE, code: u8, value: u16) -> Result<(), TransportError> {
    // SAFETY: `handle` is a live physical-monitor handle owned by this thread.
    let ok = unsafe { SetVCPFeature(handle, code, u32::from(value)) };
    if ok == 0 {
        Err(classify_failure())
    } else {
        Ok(())
    }
}

/// Read the raw MCCS capability string.
///
/// # Errors
/// A [`TransportError`] if the capability exchange fails.
pub(crate) fn read_caps(handle: HANDLE) -> Result<String, TransportError> {
    let mut len = 0u32;
    // SAFETY: `handle` is a live physical-monitor handle owned by this thread.
    if unsafe { GetCapabilitiesStringLength(handle, &mut len) } == 0 || len == 0 {
        return Err(classify_failure());
    }
    let mut buffer = vec![0u8; len as usize];
    // SAFETY: `buffer` holds `len` bytes, matching the length just queried.
    if unsafe { CapabilitiesRequestAndCapabilitiesReply(handle, &mut buffer) } == 0 {
        return Err(classify_failure());
    }
    if buffer.last() == Some(&0) {
        buffer.pop();
    }
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// Destroy a physical-monitor handle.
pub(crate) fn destroy(handle: HANDLE) {
    // SAFETY: `handle` came from GetPhysicalMonitorsFromHMONITOR and is
    // destroyed exactly once (its owning wrapper's Drop, or an unused extra).
    unsafe {
        let _ = DestroyPhysicalMonitor(handle);
    }
}
