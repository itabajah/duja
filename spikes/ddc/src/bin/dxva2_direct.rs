//! ADR-0002 spike, part 2: the "write-own" alternative.
//!
//! Minimal DIRECT dxva2 implementation via the `windows` crate (0.62) -- the same
//! path ddc-winapi wraps, but (a) on the `windows` crate we already use elsewhere
//! (no legacy `winapi` 0.3 dependency) and (b) able to recover STABLE per-display
//! identity via GetMonitorInfoW + EnumDisplayDevicesW, which ddc-hi does NOT expose.
//!
//! READ-ONLY: this binary never calls SetVCPFeature. It exists to size the direct
//! approach (LOC), confirm the API surface, and test identity recovery + latency.
//! The write path is a single `SetVCPFeature(h, 0x10, v)` call, already validated
//! (same underlying API) in the ddc-hi spike.

use std::time::Instant;

use windows::Win32::Devices::Display::{
    CapabilitiesRequestAndCapabilitiesReply, DestroyPhysicalMonitor, GetCapabilitiesStringLength,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR,
    GetVCPFeatureAndVCPFeatureReply, PHYSICAL_MONITOR,
};
use windows::Win32::Foundation::{HANDLE, LPARAM, RECT, TRUE};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, DISPLAY_DEVICEW, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};

const BRIGHTNESS: u8 = 0x10;

fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

unsafe extern "system" fn enum_cb(hmon: HMONITOR, _hdc: HDC, _rc: *mut RECT, data: LPARAM) -> windows::core::BOOL {
    let v = &mut *(data.0 as *mut Vec<HMONITOR>);
    v.push(hmon);
    TRUE
}

fn enumerate_hmonitors() -> Vec<HMONITOR> {
    let mut v: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(enum_cb), LPARAM(&mut v as *mut _ as isize));
    }
    v
}

/// Stable identity for an HMONITOR via GetMonitorInfoW -> EnumDisplayDevicesW.
/// Returns (gdi_device \\.\DISPLAYn, monitor friendly name, monitor PnP DeviceID).
fn identity(hmon: HMONITOR) -> (String, String, String) {
    unsafe {
        let mut mi = MONITORINFOEXW::default();
        mi.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if !GetMonitorInfoW(hmon, &mut mi as *mut _ as *mut MONITORINFO).as_bool() {
            return ("?".into(), "?".into(), "?".into());
        }
        let gdi_device = wide_to_string(&mi.szDevice);

        let mut dd = DISPLAY_DEVICEW::default();
        dd.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
        // second-level enum (idevnum 0 of the adapter) yields the monitor device
        let ok = EnumDisplayDevicesW(
            windows::core::PCWSTR(mi.szDevice.as_ptr()),
            0,
            &mut dd,
            0,
        )
        .as_bool();
        if ok {
            (gdi_device, wide_to_string(&dd.DeviceString), wide_to_string(&dd.DeviceID))
        } else {
            (gdi_device, "?".into(), "?".into())
        }
    }
}

fn get_vcp(h: HANDLE, code: u8) -> Option<(u32, u32)> {
    unsafe {
        let mut cur = 0u32;
        let mut max = 0u32;
        let ok = GetVCPFeatureAndVCPFeatureReply(h, code, None, &mut cur, Some(&mut max));
        if ok != 0 {
            Some((cur, max))
        } else {
            None
        }
    }
}

fn caps_string(h: HANDLE) -> Option<String> {
    unsafe {
        let mut len = 0u32;
        if GetCapabilitiesStringLength(h, &mut len) == 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if CapabilitiesRequestAndCapabilitiesReply(h, &mut buf) == 0 {
            return None;
        }
        if let Some(&0) = buf.last() {
            buf.pop();
        }
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

fn main() {
    println!("=== Duja DIRECT dxva2 spike (windows crate 0.62.2, READ-ONLY) ===\n");
    let hmons = enumerate_hmonitors();
    println!("EnumDisplayMonitors: {} HMONITOR(s)\n", hmons.len());

    for (i, &hmon) in hmons.iter().enumerate() {
        let (gdi, name, pnp) = identity(hmon);
        println!("################ HMONITOR #{} ################", i);
        println!("  gdi_device      : {}", gdi);
        println!("  friendly name   : {}", name);
        println!("  STABLE DeviceID : {}   <-- ddc-hi does NOT expose this", pnp);

        let mut count = 0u32;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(hmon, &mut count) }.is_err() || count == 0 {
            println!("  (no physical monitors / DDC unsupported)\n");
            continue;
        }
        let mut phys = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(hmon, &mut phys) }.is_err() {
            println!("  GetPhysicalMonitorsFromHMONITOR failed\n");
            continue;
        }

        for &pm in &phys {
            // Copy fields out of the packed PHYSICAL_MONITOR before referencing.
            let h = pm.hPhysicalMonitor;
            let desc_arr = pm.szPhysicalMonitorDescription;
            let desc = wide_to_string(&desc_arr);
            println!("  -- physical monitor: desc={:?}", desc);
            match get_vcp(h, BRIGHTNESS) {
                Some((cur, max)) => println!("     0x10 brightness: current={} max={}", cur, max),
                None => println!("     0x10 brightness: READ FAILED"),
            }
            match caps_string(h) {
                Some(s) => println!("     caps ({} bytes): {}", s.len(), s),
                None => println!("     caps: READ FAILED"),
            }
            // latency: 10x read of 0x10, retry-tolerant (same flakiness as ddc-winapi expected)
            let mut samples = Vec::new();
            let mut retries = 0;
            for _ in 0..10 {
                let mut tries = 0;
                loop {
                    let t0 = Instant::now();
                    let r = get_vcp(h, BRIGHTNESS);
                    let dt = t0.elapsed().as_micros();
                    if r.is_some() {
                        samples.push(dt);
                        break;
                    }
                    tries += 1;
                    retries += 1;
                    if tries >= 5 {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            if samples.len() == 10 {
                samples.sort_unstable();
                println!(
                    "     read 0x10 (n=10): min={:.1} median={:.1} max={:.1} ms (retries {})",
                    samples[0] as f64 / 1000.0,
                    samples[5] as f64 / 1000.0,
                    samples[9] as f64 / 1000.0,
                    retries
                );
            } else {
                println!("     read latency: HARD FAIL ({} ok, {} retries)", samples.len(), retries);
            }
            unsafe {
                let _ = DestroyPhysicalMonitor(h);
            }
        }
        println!();
    }
    println!("=== direct dxva2 spike complete ===");
}
