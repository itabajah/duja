// Duja overlay spike — verified Win32 recipe for a per-monitor, borderless,
// always-on-top, variable-alpha, CLICK-THROUGH black dimming overlay.
//
// Throwaway spike code: unwrap()/panic freely, no lint wall.
//
// Runs a sequence of phases and prints raw data to stdout with `>>` markers.

#![allow(non_snake_case)]

use std::mem::size_of;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use windows::core::*;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::ProcessStatus::*;
use windows::Win32::System::Threading::*;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;

// ---- shared state (single-threaded app, but wndprocs are separate fns) -----
static PROBE_CLICKS: AtomicU32 = AtomicU32::new(0);
static OVERLAY_CLICKS: AtomicU32 = AtomicU32::new(0);
static DISPLAYCHANGE_COUNT: AtomicU32 = AtomicU32::new(0);
// When 0, overlay WM_NCHITTEST returns HTTRANSPARENT (click-through).
// When 1, overlay returns DefWindowProc (opaque -> catches clicks). Used for
// the negative control.
static CLICKTHROUGH_ENABLED: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug)]
struct MonitorData {
    rect: RECT,
    work: RECT,
    primary: bool,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// -------- monitor enumeration callback --------
unsafe extern "system" fn monitor_enum(
    hmon: HMONITOR,
    _hdc: HDC,
    _rc: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let vec = &mut *(lparam.0 as *mut Vec<MonitorData>);
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if GetMonitorInfoW(hmon, &mut mi).as_bool() {
        vec.push(MonitorData {
            rect: mi.rcMonitor,
            work: mi.rcWork,
            primary: (mi.dwFlags & MONITORINFOF_PRIMARY) != 0,
        });
    }
    BOOL(1)
}

// -------- probe window proc (normal window under the overlay) --------
unsafe extern "system" fn probe_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    match msg {
        WM_LBUTTONDOWN => {
            PROBE_CLICKS.fetch_add(1, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_DESTROY => {
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// -------- overlay window proc --------
unsafe extern "system" fn overlay_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    match msg {
        WM_NCHITTEST => {
            if CLICKTHROUGH_ENABLED.load(Ordering::SeqCst) == 1 {
                // belt-and-braces: even without WS_EX_TRANSPARENT this makes
                // the window transparent to the mouse.
                LRESULT(HTTRANSPARENT as isize)
            } else {
                DefWindowProcW(hwnd, msg, wp, lp)
            }
        }
        WM_LBUTTONDOWN => {
            OVERLAY_CLICKS.fetch_add(1, Ordering::SeqCst);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            let n = DISPLAYCHANGE_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
            let w = (lp.0 & 0xffff) as i32;
            let h = ((lp.0 >> 16) & 0xffff) as i32;
            let bpp = wp.0 as i32;
            println!(">> WM_DISPLAYCHANGE #{n}: bpp={bpp} newres={w}x{h}");
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn plain_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    DefWindowProcW(hwnd, msg, wp, lp)
}

// Raise a window to the very top of the topmost band.
unsafe fn raise_top(h: HWND) {
    let f = SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE;
    let _ = SetWindowPos(h, Some(HWND_TOPMOST), 0, 0, 0, 0, f);
    let _ = SetWindowPos(h, Some(HWND_TOP), 0, 0, 0, 0, f);
}

// Stack test fixtures deterministically: probe just under the overlays, both
// above every other window on the desktop. Otherwise a pre-existing topmost
// window can sit between the overlay and the probe and swallow the click.
unsafe fn stack_probe_under_overlays(probe: HWND, overlays: &[HWND]) {
    raise_top(probe);
    for &hwnd in overlays {
        raise_top(hwnd);
    }
}

fn pump() {
    unsafe {
        let mut msg = MSG::default();
        while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

fn pump_for(ms: u64) {
    let start = Instant::now();
    while start.elapsed().as_millis() < ms as u128 {
        pump();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn cpu_100ns() -> u64 {
    unsafe {
        let mut c = FILETIME::default();
        let mut e = FILETIME::default();
        let mut k = FILETIME::default();
        let mut u = FILETIME::default();
        GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u).unwrap();
        let ku = ((k.dwHighDateTime as u64) << 32) | k.dwLowDateTime as u64;
        let uu = ((u.dwHighDateTime as u64) << 32) | u.dwLowDateTime as u64;
        ku + uu
    }
}

fn mem_working_set() -> (usize, usize) {
    unsafe {
        let mut pmc = PROCESS_MEMORY_COUNTERS::default();
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut pmc,
            size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        )
        .unwrap();
        (pmc.WorkingSetSize, pmc.PeakWorkingSetSize)
    }
}

fn main() {
    unsafe { run() }
}

unsafe fn run() {
    println!("==== DUJA OVERLAY SPIKE ====");

    // ---------------- Phase 0: DPI awareness ----------------
    let dpi_res = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    println!(">> Phase0 SetProcessDpiAwarenessContext(PER_MONITOR_AWARE_V2) = {dpi_res:?}");

    let hinst: HINSTANCE = GetModuleHandleW(None).unwrap().into();

    // ---------------- Phase 1: enumerate monitors ----------------
    let mut mons: Vec<MonitorData> = Vec::new();
    let _ = EnumDisplayMonitors(
        None,
        None,
        Some(monitor_enum),
        LPARAM(&mut mons as *mut _ as isize),
    );
    println!(">> Phase1 monitor count = {}", mons.len());
    for (i, m) in mons.iter().enumerate() {
        let r = m.rect;
        println!(
            ">>   mon[{i}] rect=({},{},{},{}) {}x{} work=({},{},{},{}) primary={}",
            r.left,
            r.top,
            r.right,
            r.bottom,
            r.right - r.left,
            r.bottom - r.top,
            m.work.left,
            m.work.top,
            m.work.right,
            m.work.bottom,
            m.primary
        );
    }
    let vs_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vs_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vs_w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let vs_h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    println!(">> Phase1 virtual-screen origin=({vs_x},{vs_y}) size={vs_w}x{vs_h}");

    // ---------------- Phase 2: probe window (white, normal) ----------------
    let probe_class = wide("DujaProbeClass");
    let probe_bg = CreateSolidBrush(COLORREF(0x00FFFFFF)); // white
    let wc_probe = WNDCLASSW {
        lpfnWndProc: Some(probe_proc),
        hInstance: hinst,
        lpszClassName: PCWSTR(probe_class.as_ptr()),
        hbrBackground: probe_bg,
        hCursor: LoadCursorW(None, IDC_ARROW).unwrap(),
        ..Default::default()
    };
    let atom_probe = RegisterClassW(&wc_probe);
    assert!(atom_probe != 0, "probe RegisterClassW failed");

    // Place probe on the primary monitor near the top-left work area.
    let prim = mons.iter().find(|m| m.primary).copied().unwrap_or(mons[0]);
    let px = prim.rect.left + 120;
    let py = prim.rect.top + 120;
    let pw = 420;
    let ph = 320;
    let probe = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(probe_class.as_ptr()),
        PCWSTR(wide("Duja Probe (click target)").as_ptr()),
        WS_OVERLAPPEDWINDOW,
        px,
        py,
        pw,
        ph,
        None,
        None,
        Some(hinst),
        None,
    )
    .unwrap();
    let _ = ShowWindow(probe, SW_SHOW);
    let _ = UpdateWindow(probe);
    let _ = SetWindowPos(
        probe,
        Some(HWND_TOP),
        0,
        0,
        0,
        0,
        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
    );
    println!(">> Phase2 probe hwnd={:?} at ({px},{py}) {pw}x{ph}", probe.0);

    // Record foreground BEFORE creating overlays.
    let fg_before = GetForegroundWindow();
    println!(">> Phase2 foreground BEFORE overlays = {:?}", fg_before.0);

    // ---------------- Phase 3: create overlays (one per monitor) ----------------
    let overlay_class = wide("DujaOverlayClass");
    let overlay_bg = CreateSolidBrush(COLORREF(0x00000000)); // black
    let wc_overlay = WNDCLASSW {
        lpfnWndProc: Some(overlay_proc),
        hInstance: hinst,
        lpszClassName: PCWSTR(overlay_class.as_ptr()),
        hbrBackground: overlay_bg,
        ..Default::default()
    };
    let atom_overlay = RegisterClassW(&wc_overlay);
    assert!(atom_overlay != 0, "overlay RegisterClassW failed");

    let ex_style = WS_EX_LAYERED
        | WS_EX_TRANSPARENT
        | WS_EX_NOACTIVATE
        | WS_EX_TOOLWINDOW
        | WS_EX_TOPMOST;

    let mut overlays: Vec<HWND> = Vec::new();
    for (i, m) in mons.iter().enumerate() {
        let r = m.rect;
        let hwnd = CreateWindowExW(
            ex_style,
            PCWSTR(overlay_class.as_ptr()),
            PCWSTR(wide("DujaOverlay").as_ptr()),
            WS_POPUP,
            r.left,
            r.top,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(hinst),
            None,
        )
        .unwrap();
        // uniform alpha, start fully transparent
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA).unwrap();
        // show WITHOUT activating
        let _ = ShowWindow(hwnd, SW_SHOWNA);
        overlays.push(hwnd);
        println!(">>   overlay[{i}] hwnd={:?} covers mon[{i}]", hwnd.0);
    }
    pump();
    let fg_after = GetForegroundWindow();
    println!(">> Phase3 foreground AFTER overlays  = {:?}", fg_after.0);
    println!(
        ">> Phase3 focus-steal check: unchanged={}",
        fg_before.0 == fg_after.0
    );
    // taskbar button check: WS_EX_TOOLWINDOW => not in taskbar/alt-tab.
    println!(">> Phase3 ex_style bits = 0x{:08X}  (WS_EX_LAYERED|TRANSPARENT|NOACTIVATE|TOOLWINDOW|TOPMOST)", ex_style.0);
    println!(">> Phase3 style = WS_POPUP (0x{:08X})", WS_POPUP.0);

    // ---------------- Phase 4: animate alpha + measure CPU/RSS ----------------
    println!(">> Phase4 animating alpha 0 -> 153 -> 0 over ~2000ms, step target <=16ms");
    let ncpu = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let cpu0 = cpu_100ns();
    let wall0 = Instant::now();
    let total_ms = 2000.0_f64;
    let mut frames = 0u32;
    let mut max_gap_ms = 0.0_f64;
    let mut last_frame = Instant::now();
    loop {
        let t = wall0.elapsed().as_secs_f64() * 1000.0;
        if t >= total_ms {
            break;
        }
        // triangular 0->153->0
        let frac = t / total_ms; // 0..1
        let tri = if frac < 0.5 { frac * 2.0 } else { (1.0 - frac) * 2.0 };
        let alpha = (tri * 153.0).round() as u8;
        for &hwnd in &overlays {
            SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA).unwrap();
        }
        pump();
        frames += 1;
        let gap = last_frame.elapsed().as_secs_f64() * 1000.0;
        if gap > max_gap_ms {
            max_gap_ms = gap;
        }
        last_frame = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    // settle back to 0
    for &hwnd in &overlays {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA).unwrap();
    }
    let cpu1 = cpu_100ns();
    let wall_ms = wall0.elapsed().as_secs_f64() * 1000.0;
    let cpu_ms = (cpu1 - cpu0) as f64 / 10_000.0; // 100ns -> ms
    let cpu_pct_1core = cpu_ms / wall_ms * 100.0;
    let cpu_pct_system = cpu_pct_1core / ncpu as f64;
    let (ws, peak_ws) = mem_working_set();
    println!(
        ">> Phase4 frames={frames} wall={wall_ms:.1}ms max_frame_gap={max_gap_ms:.1}ms avg_fps={:.1}",
        frames as f64 / (wall_ms / 1000.0)
    );
    println!(
        ">> Phase4 CPU: cputime={cpu_ms:.1}ms  = {cpu_pct_1core:.1}% of one core / {cpu_pct_system:.1}% of {ncpu}-core system (during anim)"
    );
    println!(
        ">> Phase4 RSS: working_set={} KiB  peak_working_set={} KiB",
        ws / 1024,
        peak_ws / 1024
    );

    // ---------------- Phase 5: click-through proof ----------------
    println!(">> Phase5 CLICK-THROUGH PROOF");
    // set overlays to the dimmed alpha for the test
    for &hwnd in &overlays {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 153, LWA_ALPHA).unwrap();
    }
    // Deterministic stacking: probe directly beneath the (transparent) overlays.
    stack_probe_under_overlays(probe, &overlays);
    pump_for(100);

    // client center of probe in screen coords
    let mut crc = RECT::default();
    let _ = GetClientRect(probe, &mut crc);
    let mut center = POINT {
        x: crc.right / 2,
        y: crc.bottom / 2,
    };
    let _ = ClientToScreen(probe, &mut center);
    println!(">> Phase5 probe client-center screen=({},{})", center.x, center.y);

    // What does WindowFromPoint / hit test see there? (overlay is transparent)
    let wfp = WindowFromPoint(center);
    println!(
        ">> Phase5 WindowFromPoint(center)={:?} (probe={:?}, overlay0={:?})",
        wfp.0, probe.0, overlays[0].0
    );

    // save cursor, move, click THROUGH the dimmed overlay
    let mut saved = POINT::default();
    let _ = GetCursorPos(&mut saved);

    let do_click = |x: i32, y: i32| {
        let _ = SetCursorPos(x, y);
        pump_for(30);
        let inputs = [
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_LEFTDOWN,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
            INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: MOUSEEVENTF_LEFTUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            },
        ];
        SendInput(&inputs, size_of::<INPUT>() as i32);
        pump_for(120);
    };

    // POSITIVE: click-through enabled (recipe active)
    PROBE_CLICKS.store(0, Ordering::SeqCst);
    OVERLAY_CLICKS.store(0, Ordering::SeqCst);
    CLICKTHROUGH_ENABLED.store(1, Ordering::SeqCst);
    do_click(center.x, center.y);
    let pos_probe = PROBE_CLICKS.load(Ordering::SeqCst);
    let pos_overlay = OVERLAY_CLICKS.load(Ordering::SeqCst);
    println!(
        ">> Phase5 POSITIVE (recipe on, alpha=153): probe_clicks={pos_probe} overlay_clicks={pos_overlay}  => PASS={}",
        pos_probe >= 1 && pos_overlay == 0
    );

    // NEGATIVE CONTROL: strip WS_EX_TRANSPARENT + return HTCLIENT -> overlay
    // should now EAT the click, probe gets nothing.
    for &hwnd in &overlays {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, cur & !(WS_EX_TRANSPARENT.0 as isize));
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    CLICKTHROUGH_ENABLED.store(0, Ordering::SeqCst); // WM_NCHITTEST -> HTCLIENT
    // the POSITIVE click activated the probe (a normal window), raising it above
    // the overlay; restore overlays-on-top so the opaque overlay can catch it.
    stack_probe_under_overlays(probe, &overlays);
    pump_for(60);
    let wfp_neg = WindowFromPoint(center);
    println!(
        ">> Phase5 NEG WindowFromPoint(center)={:?} (expect overlay0={:?})",
        wfp_neg.0, overlays[0].0
    );
    PROBE_CLICKS.store(0, Ordering::SeqCst);
    OVERLAY_CLICKS.store(0, Ordering::SeqCst);
    do_click(center.x, center.y);
    let neg_probe = PROBE_CLICKS.load(Ordering::SeqCst);
    let neg_overlay = OVERLAY_CLICKS.load(Ordering::SeqCst);
    println!(
        ">> Phase5 NEGATIVE (transparent stripped): probe_clicks={neg_probe} overlay_clicks={neg_overlay}  => overlay-blocks-confirmed={}",
        neg_probe == 0 && neg_overlay >= 1
    );

    // restore recipe
    for &hwnd in &overlays {
        let cur = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, cur | (WS_EX_TRANSPARENT.0 as isize));
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        );
    }
    CLICKTHROUGH_ENABLED.store(1, Ordering::SeqCst);
    let _ = SetCursorPos(saved.x, saved.y);

    // ---------------- Phase 6: capture exclusion (WDA_EXCLUDEFROMCAPTURE) ----------------
    println!(">> Phase6 CAPTURE EXCLUSION (WDA_EXCLUDEFROMCAPTURE)");
    // Sample a small block at the probe center via BitBlt from the screen DC.
    let sample = |label: &str| -> u32 {
        let bw = 60;
        let bh = 60;
        let sx = center.x - bw / 2;
        let sy = center.y - bh / 2;
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(Some(screen));
        let bmp = CreateCompatibleBitmap(screen, bw, bh);
        let old = SelectObject(mem, bmp.into());
        let _ = BitBlt(mem, 0, 0, bw, bh, Some(screen), sx, sy, SRCCOPY);
        let mut sum: u64 = 0;
        let mut n: u64 = 0;
        let mut yy = 5;
        while yy < bh - 5 {
            let mut xx = 5;
            while xx < bw - 5 {
                let c = GetPixel(mem, xx, yy).0;
                if c != CLR_INVALID {
                    let r = (c & 0xff) as u64;
                    let g = ((c >> 8) & 0xff) as u64;
                    let b = ((c >> 16) & 0xff) as u64;
                    sum += r + g + b;
                    n += 3;
                }
                xx += 10;
            }
            yy += 10;
        }
        SelectObject(mem, old);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        let avg = if n > 0 { (sum / n) as u32 } else { 0 };
        println!(">>   sample[{label}] avg_channel={avg} (0=black,255=white)");
        avg
    };

    // ensure probe (white) is directly beneath the overlays and above all else
    stack_probe_under_overlays(probe, &overlays);
    pump_for(60);
    // baseline: overlay fully transparent (alpha 0) -> probe white shows through
    for &hwnd in &overlays {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 0, LWA_ALPHA).unwrap();
        let _ = SetWindowDisplayAffinity(hwnd, WDA_NONE);
    }
    pump_for(120);
    let base = sample("baseline_alpha0_noaffinity");

    // dimmed, no affinity -> should be darker
    for &hwnd in &overlays {
        SetLayeredWindowAttributes(hwnd, COLORREF(0), 153, LWA_ALPHA).unwrap();
    }
    pump_for(120);
    let dimmed = sample("dimmed_alpha153_noaffinity");

    // dimmed + WDA_EXCLUDEFROMCAPTURE -> capture should skip overlay (bright again)
    let mut aff_ok = true;
    for &hwnd in &overlays {
        let r = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        if r.is_err() {
            aff_ok = false;
            println!(">>   SetWindowDisplayAffinity err on {:?}: {r:?}", hwnd.0);
        }
    }
    pump_for(150);
    let excluded = sample("dimmed_alpha153_EXCLUDEFROMCAPTURE");
    // reset
    for &hwnd in &overlays {
        let _ = SetWindowDisplayAffinity(hwnd, WDA_NONE);
    }
    println!(
        ">> Phase6 result: affinity_set_ok={aff_ok} baseline={base} dimmed={dimmed} excluded={excluded}"
    );
    println!(
        ">> Phase6 interpretation: dimming_visible_in_capture={} exclusion_restores_capture={}",
        dimmed + 25 < base,
        excluded > dimmed + 25
    );

    // ---------------- Phase 7: z-order realities ----------------
    println!(">> Phase7 Z-ORDER");
    // create a SECOND topmost window AFTER the overlays (models Start menu / a
    // topmost popup appearing on top)
    let comp_class = wide("DujaCompetitorClass");
    let comp_bg = CreateSolidBrush(COLORREF(0x000000FF)); // red
    let wc_comp = WNDCLASSW {
        lpfnWndProc: Some(plain_proc),
        hInstance: hinst,
        lpszClassName: PCWSTR(comp_class.as_ptr()),
        hbrBackground: comp_bg,
        ..Default::default()
    };
    RegisterClassW(&wc_comp);
    let comp = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
        PCWSTR(comp_class.as_ptr()),
        PCWSTR(wide("DujaCompetitor").as_ptr()),
        WS_POPUP,
        prim.rect.left + 200,
        prim.rect.top + 200,
        200,
        150,
        None,
        None,
        Some(hinst),
        None,
    )
    .unwrap();
    let _ = ShowWindow(comp, SW_SHOWNA);
    pump_for(60);

    // walk z-order top->bottom
    let mut order: Vec<HWND> = Vec::new();
    let mut cur = GetTopWindow(None).unwrap_or(HWND(std::ptr::null_mut()));
    let mut guard = 0;
    while !cur.0.is_null() && guard < 5000 {
        order.push(cur);
        cur = GetWindow(cur, GW_HWNDNEXT).unwrap_or(HWND(std::ptr::null_mut()));
        guard += 1;
    }
    let idx_of = |h: HWND| order.iter().position(|x| x.0 == h.0);
    let taskbar = FindWindowW(PCWSTR(wide("Shell_TrayWnd").as_ptr()), None)
        .unwrap_or(HWND(std::ptr::null_mut()));
    println!(">> Phase7 zorder list len={} (lower index = higher on screen)", order.len());
    println!(">>   overlay[0] z-index = {:?}", idx_of(overlays[0]));
    println!(">>   competitor(later topmost) z-index = {:?}", idx_of(comp));
    println!(">>   taskbar(Shell_TrayWnd) z-index = {:?}", idx_of(taskbar));
    match (idx_of(overlays[0]), idx_of(comp)) {
        (Some(o), Some(c)) => println!(
            ">> Phase7 later-topmost-window-above-overlay = {} (competitor idx {c} vs overlay idx {o})",
            c < o
        ),
        _ => println!(">> Phase7 could not resolve both indices"),
    }
    match (idx_of(overlays[0]), idx_of(taskbar)) {
        (Some(o), Some(tb)) => println!(
            ">> Phase7 overlay-above-taskbar = {} (overlay idx {o} vs taskbar idx {tb})",
            o < tb
        ),
        _ => println!(">> Phase7 taskbar not found in z-order walk"),
    }
    let _ = DestroyWindow(comp);

    // ---------------- Phase 8: WM_DISPLAYCHANGE handler ----------------
    println!(">> Phase8 WM_DISPLAYCHANGE handler wired in overlay_proc.");
    println!(
        ">> Phase8 displaychange events observed so far = {}",
        DISPLAYCHANGE_COUNT.load(Ordering::SeqCst)
    );
    // brief pump window to catch any incidental display events
    pump_for(200);

    // ---------------- cleanup ----------------
    for &hwnd in &overlays {
        let _ = DestroyWindow(hwnd);
    }
    let _ = DestroyWindow(probe);
    pump();

    println!("==== SPIKE COMPLETE ====");
}
