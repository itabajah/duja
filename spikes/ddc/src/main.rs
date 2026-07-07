//! Duja ADR-0002 spike: exercise the `ddc-hi` crate against REAL DDC/CI hardware.
//!
//! HARDWARE SAFETY (see ADR task):
//!   * Only VCP 0x10 (luminance/brightness) is ever WRITTEN.
//!   * Before any write we read + store the current value and PRINT it first.
//!   * A `RestoreGuard` restores the original value on scope exit AND on panic.
//!   * Dim step only lowers brightness (current-5, clamped >=10) and only when
//!     that is actually a decrease; otherwise it is skipped.
//!
//! Throwaway spike code: liberal unwrap on non-hardware paths is fine, but the
//! DDC read/write paths use explicit error handling so one bad monitor cannot
//! abort the run or leave a monitor dimmed.

use std::time::Instant;

use ddc_hi::{Ddc, Display, FeatureCode, Handle};

const BRIGHTNESS: FeatureCode = 0x10;
const CONTRAST: FeatureCode = 0x12; // read-only probe
const INPUT_SOURCE: FeatureCode = 0x60; // read-only probe

/// Restores VCP `code` to `original` on drop unless disarmed. Uses a raw pointer
/// so the restore can run during unwinding without holding a live `&mut` borrow.
struct RestoreGuard {
    handle: *mut Handle,
    code: FeatureCode,
    original: u16,
    armed: bool,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if self.armed {
            let h = unsafe { &mut *self.handle };
            match h.set_vcp_feature(self.code, self.original) {
                Ok(()) => eprintln!(
                    "[guard] restored VCP 0x{:02x} -> {} (on drop/panic)",
                    self.code, self.original
                ),
                Err(e) => eprintln!(
                    "[guard] !!! FAILED to restore VCP 0x{:02x} -> {}: {}. \
                     MANUAL ACTION MAY BE REQUIRED.",
                    self.code, self.original, e
                ),
            }
        }
    }
}

fn median_ms(sorted_us: &[u128]) -> f64 {
    let n = sorted_us.len();
    if n == 0 {
        return f64::NAN;
    }
    let mid = if n % 2 == 1 {
        sorted_us[n / 2] as f64
    } else {
        (sorted_us[n / 2 - 1] as f64 + sorted_us[n / 2] as f64) / 2.0
    };
    mid / 1000.0
}

fn main() {
    println!("=== Duja ddc-hi spike (ddc-hi 0.4.1 / ddc 0.2.2 / mccs 0.1.3) ===");
    println!("Host: Windows, backend enumeration via ddc-hi Display::enumerate()\n");

    let mut displays = Display::enumerate();
    println!("Display::enumerate() returned {} display(s)\n", displays.len());

    for (idx, display) in displays.iter_mut().enumerate() {
        println!("################ DISPLAY #{} ################", idx);

        // ---- 1. Identity from DisplayInfo (pre-capabilities) ----
        let info = &display.info;
        println!("[enumerate-time DisplayInfo]");
        println!("  backend         : {}", info.backend);
        println!("  id              : {:?}", info.id);
        println!("  manufacturer_id : {:?}", info.manufacturer_id);
        println!("  model_id        : {:?}", info.model_id);
        println!("  model_name      : {:?}", info.model_name);
        println!("  serial (u32)    : {:?}", info.serial);
        println!("  serial_number   : {:?}", info.serial_number);
        println!("  mfg year/week   : {:?}/{:?}", info.manufacture_year, info.manufacture_week);
        println!("  version         : {:?}", info.version);
        println!("  mccs_version    : {:?}", info.mccs_version);
        println!(
            "  edid_data       : {}",
            match &info.edid_data {
                Some(v) => format!("present ({} bytes)", v.len()),
                None => "ABSENT".to_string(),
            }
        );

        // ---- 1b. Capabilities string (raw) + parsed update ----
        println!("\n[capabilities]");
        let caps_t0 = Instant::now();
        let raw_caps = display.handle.capabilities_string();
        let caps_dt = caps_t0.elapsed();
        match &raw_caps {
            Ok(bytes) => {
                println!("  capabilities_string(): OK in {:.1} ms, {} bytes", caps_dt.as_secs_f64() * 1000.0, bytes.len());
                let as_str = String::from_utf8_lossy(bytes);
                println!("  RAW CAPS: {}", as_str);
            }
            Err(e) => println!("  capabilities_string(): ERR: {}", e),
        }

        // Parse the caps string into the structured mccs::Capabilities.
        match display.handle.capabilities() {
            Ok(caps) => {
                println!("  parsed: model={:?} type={:?} mccs_version={:?} ms_whql={:?}", caps.model, caps.ty, caps.mccs_version, caps.ms_whql);
                let vcp_codes: Vec<String> =
                    caps.vcp_features.keys().map(|c| format!("0x{:02x}", c)).collect();
                println!("  vcp_features ({}): {}", vcp_codes.len(), vcp_codes.join(" "));
                let cmds: Vec<String> = caps.commands.iter().map(|c| format!("0x{:02x}", c)).collect();
                println!("  commands ({}): {}", cmds.len(), cmds.join(" "));
                println!("  caps carries EDID: {}", caps.edid.is_some());
            }
            Err(e) => println!("  capabilities() parse: ERR: {}", e),
        }

        // update_capabilities() folds caps back into DisplayInfo (may fill model/edid).
        match display.update_capabilities() {
            Ok(()) => {
                let info = &display.info;
                println!(
                    "  post-caps identity: manufacturer_id={:?} model_name={:?} serial_number={:?} edid={}",
                    info.manufacturer_id,
                    info.model_name,
                    info.serial_number,
                    info.edid_data.as_ref().map(|v| v.len()).map(|n| format!("{} bytes", n)).unwrap_or_else(|| "ABSENT".into()),
                );
            }
            Err(e) => println!("  update_capabilities(): ERR: {}", e),
        }

        // ---- 1c. Read VCP 0x10 / 0x12 / 0x60 ----
        println!("\n[VCP reads]");
        let bright = display.handle.get_vcp_feature(BRIGHTNESS);
        match &bright {
            Ok(v) => println!("  0x10 brightness : current={} max={} ty={}", v.value(), v.maximum(), v.ty),
            Err(e) => println!("  0x10 brightness : ERR: {}", e),
        }
        match display.handle.get_vcp_feature(CONTRAST) {
            Ok(v) => println!("  0x12 contrast   : current={} max={} ty={} (read-only probe)", v.value(), v.maximum(), v.ty),
            Err(e) => println!("  0x12 contrast   : ERR (unsupported?): {}", e),
        }
        match display.handle.get_vcp_feature(INPUT_SOURCE) {
            Ok(v) => println!("  0x60 input src  : current={} max={} ty={} (read-only probe)", v.value(), v.maximum(), v.ty),
            Err(e) => println!("  0x60 input src  : ERR (unsupported?): {}", e),
        }

        // Guard: if 0x10 read failed, do NOT write to this monitor at all.
        let original = match bright {
            Ok(v) => v.value(),
            Err(_) => {
                println!("\n  -> 0x10 read failed; SKIPPING all writes / latency-write for this display.\n");
                continue;
            }
        };
        let max = bright.as_ref().map(|v| v.maximum()).unwrap_or(0);

        // ---- 2a. Read latency: 10x get_vcp_feature(0x10) ----
        // ddc-winapi implements NO inter-command DDC delay, so back-to-back reads
        // intermittently fail. We retry each read (up to 5x, 50ms apart) and count
        // retries -- the retry count itself is a flakiness metric for the quirks DB.
        println!("\n[latency: 10x read of VCP 0x10 (retry-tolerant)]");
        let mut samples_us: Vec<u128> = Vec::with_capacity(10);
        let mut total_retries = 0u32;
        let mut hard_fail = false;
        for _ in 0..10 {
            let mut attempt = 0;
            loop {
                let t0 = Instant::now();
                let r = display.handle.get_vcp_feature(BRIGHTNESS);
                let dt = t0.elapsed().as_micros();
                match r {
                    Ok(_) => {
                        samples_us.push(dt);
                        break;
                    }
                    Err(_) => {
                        attempt += 1;
                        total_retries += 1;
                        if attempt >= 5 {
                            hard_fail = true;
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                }
            }
            if hard_fail {
                break;
            }
        }
        if samples_us.len() == 10 {
            let mut s = samples_us.clone();
            s.sort_unstable();
            println!(
                "  read 0x10 (n=10): min={:.1} ms  median={:.1} ms  max={:.1} ms  (retries needed: {})",
                s[0] as f64 / 1000.0,
                median_ms(&s),
                s[9] as f64 / 1000.0,
                total_retries
            );
        } else {
            println!("  read latency: HARD FAIL after retries ({} samples, {} retries); skipping writes for safety.", samples_us.len(), total_retries);
            continue;
        }

        // ================= WRITE SECTION (VCP 0x10 ONLY) =================
        println!("\n[SAFE WRITE TEST — VCP 0x10 ONLY]");
        println!("  *** SAVED ORIGINAL BRIGHTNESS 0x10 = {} (max {}). If anything goes wrong,", original, max);
        println!("  *** manually set this monitor's brightness back to {}. ***", original);

        let handle_ptr: *mut Handle = &mut display.handle;
        let mut guard = RestoreGuard { handle: handle_ptr, code: BRIGHTNESS, original, armed: true };

        // 2b. Write-same-value latency (set to current -> no visible change).
        {
            let h = unsafe { &mut *handle_ptr };
            let t0 = Instant::now();
            let w = h.set_vcp_feature(BRIGHTNESS, original);
            let dt = t0.elapsed();
            match w {
                Ok(()) => println!("  write-same-value latency: {:.1} ms (set 0x10 -> {})", dt.as_secs_f64() * 1000.0, original),
                Err(e) => {
                    println!("  write-same-value: ERR: {} -> disarming guard, no restore needed.", e);
                    guard.armed = false;
                    continue;
                }
            }
        }

        // 3. Safe dim step: current-5 clamped >=10, ONLY if it is an actual decrease.
        let target = original.saturating_sub(5).max(10);
        if target < original {
            let h = unsafe { &mut *handle_ptr };
            println!("  dim step: set 0x10 -> {} (visible ~500ms), then restore {}", target, original);
            match h.set_vcp_feature(BRIGHTNESS, target) {
                Ok(()) => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    // verify read-back of the dimmed value
                    if let Ok(v) = h.get_vcp_feature(BRIGHTNESS) {
                        println!("  read-back after dim: {} (requested {})", v.value(), target);
                    }
                    // restore
                    let t0 = Instant::now();
                    match h.set_vcp_feature(BRIGHTNESS, original) {
                        Ok(()) => {
                            println!("  restore latency: {:.1} ms (set 0x10 -> {})", t0.elapsed().as_secs_f64() * 1000.0, original);
                            guard.armed = false; // restored cleanly
                        }
                        Err(e) => println!("  restore FAILED: {} (guard will retry on drop)", e),
                    }
                }
                Err(e) => {
                    println!("  dim write FAILED: {} -> restoring original via guard.", e);
                    // leave guard armed; it will restore to original on drop
                }
            }
        } else {
            println!("  original brightness {} <= 15; skipping dim step (would not be a decrease).", original);
            guard.armed = false; // set-same already left it at original
        }

        drop(guard);
        println!();
    }

    // ---- 4. Worker-thread-per-monitor feasibility (Send) ----
    // Enabled only under `--features prove_send` so the normal build still runs.
    #[cfg(feature = "prove_send")]
    {
        // Static assertions: which types satisfy Send? (compile error names the culprit)
        fn assert_send<T: Send>() {}
        assert_send::<Display>();
        assert_send::<Handle>();
        if let Some(display) = displays.into_iter().next() {
            std::thread::spawn(move || {
                let _ = display; // move a Display across a thread boundary
            })
            .join()
            .unwrap();
        }
    }

    println!("=== spike complete ===");
}
