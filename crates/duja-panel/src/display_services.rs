//! The macOS internal-panel backend — the private `DisplayServices` framework,
//! resolved at runtime with graceful absence.
//!
//! DDC/CI cannot reach a laptop's internal panel; macOS controls it through the
//! **private** `DisplayServices.framework`, which is not part of any SDK and has
//! no published bindings. This module dlopen's the framework at first use and
//! dlsym's three symbols:
//!
//! - `int  DisplayServicesGetBrightness(CGDirectDisplayID, float *out)`
//! - `int  DisplayServicesSetBrightness(CGDirectDisplayID, float value)`
//! - `bool DisplayServicesCanChangeBrightness(CGDirectDisplayID)`
//!
//! (signatures verified against the open-source `nriley/brightness` and
//! `MonitorControl` users of the same private API). The two `…Brightness` calls
//! return `0` on success; brightness is a `0.0..=1.0` float.
//!
//! # Why dlopen, not a link
//! The framework is private: linking it would break the build on any SDK that
//! omits it and would hard-fail a Mac where Apple removed the symbols. Resolving
//! at runtime lets the backend **degrade to nothing** — a missing framework or
//! symbol makes [`enumerate`](crate::enumerate) return `Ok(vec![])`, never an
//! error, exactly as a desktop with no panel does. The public CoreGraphics
//! calls used for enumeration (`CGGetOnlineDisplayList`, `CGDisplayIsBuiltin`,
//! `CGDisplayVendorNumber`/`ModelNumber`/`SerialNumber`) go through the safe
//! `core-graphics` crate and a single hand-rolled `CGGetOnlineDisplayList`
//! binding.
//!
//! # The transport seam
//! [`DisplayServicesApi`] abstracts the three operations behind a trait so the
//! `unsafe` FFI table (`RealDisplayServices`, macOS-only) and an in-memory
//! fake share one [`DisplayServicesTransport`] and one
//! [`crate::controller::PanelController`]. Consequently the whole macOS control
//! adapter — the float↔level mapping, the identity synthesis, the controller
//! contract — is compiled and unit-tested on **every** OS, while the raw dlopen
//! code compiles only under `cfg(target_os = "macos")`.
//!
//! # Brightness domain and round-trip precision
//! `DisplayServices` speaks `0.0..=1.0` floats; the [`crate::transport`] seam
//! speaks integer levels and the [`crate::controller::PanelController`] speaks
//! percent (`0..=100`). This module maps them one-to-one at percent granularity
//! (`level = round(float * 100)`, `float = level / 100`), a lossless
//! round-trip for the 101 integer percents. `set(get())` may still quantize:
//! the panel hardware snaps a written float to its own internal step count, so a
//! subsequent read can differ by a step — the controller contract tolerates this
//! and the [`crate::transport::PanelBrightness::levels`] list is informative only.

// RATIONALE: `DisplayServicesTransport`/`DisplayServicesApi` namespace the macOS
// backend after the framework; the `module_name_repetitions` pedantic lint
// fights those chosen names (the Windows `wmi` module makes the same call).
#![allow(clippy::module_name_repetitions)]
// RATIONALE: on non-macOS targets this whole adapter is still compiled — that is
// deliberate, so its pure float↔level mapping, identity synthesis, and the
// DisplayServices controller contract run in CI on every OS — but only the tests
// exercise it there; the runtime `enumerate`/FFI wiring is macOS-only. The code
// is far from dead: it is the cross-platform test surface the plan mandates.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::fmt::Debug;

use duja_core::id::{EdidError, StableDisplayId};
use duja_core::macos::{CgRect, MirrorState, bounds_from_cg_rect, surface_id};

use crate::PanelGeometry;
use crate::error::PanelError;
use crate::transport::{PanelBrightness, PanelTransport};

/// A CoreGraphics display id (`CGDirectDisplayID`), a `uint32_t`. Defined here so
/// the cross-platform transport and its tests do not depend on `core-graphics`.
pub type CgDisplayId = u32;

/// The maximum panel level; the percent domain the transport maps onto.
const PANEL_LEVEL_MAX: u8 = 100;

/// Map a `DisplayServices` brightness float (`0.0..=1.0`) to a panel level
/// (`0..=100`).
///
/// The input is clamped first: a backend that returns a value slightly outside
/// the unit interval (or `NaN`, which clamps to the low bound) must not produce
/// an out-of-range level.
fn float_to_level(brightness: f32) -> u8 {
    let pct = (brightness.clamp(0.0, 1.0) * f32::from(PANEL_LEVEL_MAX)).round();
    // RATIONALE(clippy::cast_possible_truncation, clippy::cast_sign_loss): `pct`
    // is `round()` of a value clamped to `0.0..=100.0` — finite, non-negative,
    // and integer-valued — so the `u8` cast is exact and cannot wrap or lose a
    // sign.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let level = pct as u8;
    level
}

/// Map a panel level (`0..=100`) to a `DisplayServices` brightness float
/// (`0.0..=1.0`). Levels above the max are clamped.
fn level_to_float(level: u8) -> f32 {
    f32::from(level.min(PANEL_LEVEL_MAX)) / f32::from(PANEL_LEVEL_MAX)
}

/// The discrete levels the transport reports: every integer percent, ascending.
///
/// Informative only (see [`crate::transport::PanelBrightness::levels`]): the
/// controller advertises a continuous `0..=100` range and the panel snaps a
/// written value to its own internal steps.
fn percent_levels() -> Vec<u8> {
    (0..=PANEL_LEVEL_MAX).collect()
}

/// Synthetic sentinel manufacturer for a builtin panel whose
/// `CGDisplayVendorNumber` does not decode to a PNP id (e.g. a runner that
/// reports `0`/`0xFFFFFFFF`). It is a valid three-letter id so
/// [`StableDisplayId::from_parts`] accepts it. A machine has **at most one**
/// internal panel, so this can never collide on the machine that owns the key,
/// and the `product`/`serial` components still distinguish it in any log.
const SENTINEL_VENDOR: &str = "AAP";

/// Map a 5-bit PNP group (`1..=26`) to `A`..=`Z`, or `None` if out of range —
/// the same decode `duja_core`'s EDID parser applies to bytes 8..=9.
fn pnp_letter(value: u16) -> Option<char> {
    if value == 0 || value > 26 {
        return None;
    }
    let idx = value.wrapping_sub(1);
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZ"
        .get(usize::from(idx))
        .copied()
        .map(char::from)
}

/// Decode a `CGDisplayVendorNumber` as a three-letter PNP id.
///
/// macOS reports the vendor as the EDID manufacturer id packed big-endian into
/// 16 bits (three 5-bit `A=1` groups); Apple's builtin panels decode to `"APP"`
/// (`0x0610`). Returns `None` when the low 16 bits do not encode three `A`–`Z`
/// letters.
fn decode_pnp_vendor(vendor: u32) -> Option<String> {
    let packed = u16::try_from(vendor & 0xFFFF).ok()?;
    let groups = [(packed >> 10) & 0x1F, (packed >> 5) & 0x1F, packed & 0x1F];
    let mut mfg = String::with_capacity(3);
    for group in groups {
        mfg.push(pnp_letter(group)?);
    }
    Some(mfg)
}

/// Synthesize a [`StableDisplayId`] for a builtin panel from the CoreGraphics
/// vendor/model/serial numbers.
///
/// Builtin panels rarely expose a full EDID through a public API, so we cannot
/// call [`StableDisplayId::from_edid`]. Instead we feed
/// [`StableDisplayId::from_parts`] the decoded fields, producing an id whose
/// **shape is identical** to every other backend's:
/// - manufacturer = the PNP-decoded vendor, or [`SENTINEL_VENDOR`] when it does
///   not decode (so `from_parts`'s three-letter invariant always holds);
/// - product = the low 16 bits of `CGDisplayModelNumber` (the EDID product code
///   field is 16-bit);
/// - serial = the decimal `CGDisplaySerialNumber` when non-zero, else `None`,
///   which routes `from_parts` to its stable hash-of-`MFG-PROD` fallback (a zero
///   serial is "unset", the common builtin case).
///
/// **Stability:** all three CoreGraphics numbers are derived from the panel's
/// EDID, which lives in the panel and is invariant across reboot, sleep/wake and
/// GPU re-enumeration; the id therefore keys per-panel settings durably. The
/// `CGDirectDisplayID` itself is *not* used in the identity — it is a volatile
/// session handle — only as the transport's runtime address.
///
/// # Errors
/// [`EdidError::InvalidManufacturer`] can only arise from a bad manufacturer,
/// which both the PNP decode and [`SENTINEL_VENDOR`] rule out, so in practice
/// this is infallible; the `Result` is kept so a future change cannot silently
/// panic and the caller can simply skip an unmappable display.
fn synthesize_panel_id(vendor: u32, model: u32, serial: u32) -> Result<StableDisplayId, EdidError> {
    let manufacturer = decode_pnp_vendor(vendor).unwrap_or_else(|| SENTINEL_VENDOR.to_owned());
    let product_code = u16::try_from(model & 0xFFFF).unwrap_or(0);
    let serial_string = (serial != 0).then(|| serial.to_string());
    StableDisplayId::from_parts(&manufacturer, product_code, serial_string.as_deref())
}

/// Whether an online display is a controllable internal panel: it must be
/// builtin **and** report brightness control. Factored out so the gate is
/// unit-tested on every host, independent of the macOS-only enumeration.
fn is_controllable_panel(is_builtin: bool, can_change_brightness: bool) -> bool {
    is_builtin && can_change_brightness
}

/// Build a builtin panel's [`PanelGeometry`] from the two CoreGraphics facts the
/// enumeration already has in hand: its rect and its mirror state.
///
/// The built-in panel is an ordinary CoreGraphics display, so this is the same
/// derivation `duja-ddc` applies to an external monitor, through the same
/// [`duja_core::macos`] functions — deliberately, because the two backends' tokens
/// are compared against each other app-side. A `MacBook` mirroring its screen to a
/// projector puts the panel (here) and the projector (there) in one mirror set,
/// and the merge that collapses them into one control is *only* correct if both
/// sides computed the surface token by the same rule.
///
/// The gamma token is the panel's own id: a mirror-set master addresses itself,
/// and so does a clone. Only the surface token follows the mirror.
///
/// # A degenerate rect yields `None`, not a degenerate geometry
///
/// `CGDisplayBounds` answers `CGRectNull` — `{{inf, inf}, {0, 0}}` — for a display
/// it considers invalid, and this backend enumerates the **online** list, which
/// deliberately includes a built-in that is online but not the active drawable
/// (see `imp::online_display_ids`). Passed through, that becomes bounds at
/// `i32::MAX` with zero extent: an overlay window of no size, placed nowhere,
/// dimming nothing, with no warning and no retry — the "slider controls nothing"
/// failure this whole change exists to remove.
///
/// So an empty rect is reported as "this backend cannot say where the panel is",
/// which is what `None` already means to every consumer. The panel then behaves
/// exactly as it did before geometry existed — un-dimmable below its floor, and
/// visibly so — instead of appearing dimmable and silently doing nothing. The
/// tokens are discarded with it: all three fields are one answer, and half of one
/// is not better than none.
///
/// Pure, so it is compiled and tested on every OS even though only the macOS
/// `imp::enumerate` below calls it.
fn panel_geometry(rect: CgRect, mirror: MirrorState) -> Option<PanelGeometry> {
    let bounds = bounds_from_cg_rect(rect);
    if bounds.is_empty() {
        return None;
    }
    Some(PanelGeometry::new(
        bounds,
        mirror.display_id.to_string(),
        surface_id(mirror).to_string(),
    ))
}

/// The three `DisplayServices` brightness operations, abstracted so the real
/// dlopen'd table and an in-memory fake are interchangeable.
///
/// `Send + Debug` mirror the [`PanelTransport`] bounds so a
/// [`DisplayServicesTransport`] over any implementation can be moved to a worker
/// thread and logged.
pub trait DisplayServicesApi: Send + Debug {
    /// Whether the panel supports brightness control
    /// (`DisplayServicesCanChangeBrightness`).
    fn can_change_brightness(&self, display: CgDisplayId) -> bool;

    /// Read the panel's brightness as a `0.0..=1.0` float
    /// (`DisplayServicesGetBrightness`).
    ///
    /// # Errors
    /// [`PanelError`] if the underlying call reports failure (the panel is
    /// unreachable or the framework rejected the id).
    fn get_brightness(&mut self, display: CgDisplayId) -> Result<f32, PanelError>;

    /// Set the panel's brightness from a `0.0..=1.0` float
    /// (`DisplayServicesSetBrightness`).
    ///
    /// # Errors
    /// [`PanelError`] if the underlying call reports failure.
    fn set_brightness(&mut self, display: CgDisplayId, value: f32) -> Result<(), PanelError>;
}

/// A [`PanelTransport`] over a [`DisplayServicesApi`], bound to one
/// `CGDirectDisplayID`.
///
/// Generic over the operation table so the macOS `RealDisplayServices` backend
/// and the test fake share one adapter (and one contract run). The transport
/// converts between the framework's `0.0..=1.0` floats and the seam's integer
/// levels; see this module's documentation on round-trip precision.
#[derive(Debug)]
pub struct DisplayServicesTransport<A: DisplayServicesApi> {
    display: CgDisplayId,
    api: A,
}

impl<A: DisplayServicesApi> DisplayServicesTransport<A> {
    /// Bind a transport to `display`, driving it through `api`.
    #[must_use]
    pub fn new(display: CgDisplayId, api: A) -> Self {
        Self { display, api }
    }

    /// The `CGDirectDisplayID` this transport drives.
    #[must_use]
    pub fn display(&self) -> CgDisplayId {
        self.display
    }
}

impl<A: DisplayServicesApi> PanelTransport for DisplayServicesTransport<A> {
    fn query(&mut self) -> Result<PanelBrightness, PanelError> {
        let brightness = self.api.get_brightness(self.display)?;
        Ok(PanelBrightness {
            current: float_to_level(brightness),
            levels: percent_levels(),
        })
    }

    fn set_brightness(&mut self, percent: u8) -> Result<(), PanelError> {
        self.api
            .set_brightness(self.display, level_to_float(percent))
    }
}

// ---------------------------------------------------------------------------
// macOS real backend: the confined `unsafe` FFI. Compiled only on macOS.
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
mod imp {
    use std::ffi::CStr;
    use std::os::raw::c_void;
    use std::sync::OnceLock;

    use core_graphics::display::CGDisplay;

    use super::{
        CgDisplayId, DisplayServicesApi, is_controllable_panel, panel_geometry, synthesize_panel_id,
    };
    use crate::PanelDisplay;
    use crate::error::PanelError;
    use duja_core::macos::{CgRect, MirrorState};

    /// `int DisplayServicesGetBrightness(CGDirectDisplayID, float *)`.
    type GetFn = unsafe extern "C" fn(CgDisplayId, *mut f32) -> i32;
    /// `int DisplayServicesSetBrightness(CGDirectDisplayID, float)`.
    type SetFn = unsafe extern "C" fn(CgDisplayId, f32) -> i32;
    /// `bool DisplayServicesCanChangeBrightness(CGDirectDisplayID)`.
    type CanFn = unsafe extern "C" fn(CgDisplayId) -> bool;

    /// The resolved private-framework function pointers.
    #[derive(Debug)]
    struct DisplayServicesSyms {
        get: GetFn,
        set: SetFn,
        can: CanFn,
    }

    /// Absolute path to the private framework's Mach-O image.
    const FRAMEWORK_PATH: &CStr =
        c"/System/Library/PrivateFrameworks/DisplayServices.framework/DisplayServices";

    /// Resolve a symbol from `handle` as the fn-pointer type `F`.
    ///
    /// # Safety
    /// `handle` must be a live handle returned by `dlopen`, and `F` must be a C
    /// ABI `fn`-pointer type whose signature matches the symbol named `name`.
    unsafe fn resolve_symbol<F: Copy>(handle: *mut c_void, name: &CStr) -> Option<F> {
        // SAFETY: `handle` is a live dlopen handle and `name` is NUL-terminated;
        // dlsym returns null for a missing symbol, which we reject below before
        // interpreting the address.
        let sym = unsafe { libc::dlsym(handle, name.as_ptr()) };
        if sym.is_null() {
            return None;
        }
        // SAFETY: `sym` is a non-null code address for a symbol whose C signature
        // the caller guarantees matches `F`, a pointer-sized fn pointer;
        // `transmute_copy` reinterprets the address as that fn pointer.
        Some(unsafe { core::mem::transmute_copy::<*mut c_void, F>(&sym) })
    }

    /// dlopen the framework and resolve all three symbols, or `None` if the
    /// framework or **any** symbol is absent (a partial table is treated as no
    /// capability, so the backend degrades cleanly).
    fn load_syms() -> Option<DisplayServicesSyms> {
        // SAFETY: `FRAMEWORK_PATH` is a valid NUL-terminated C string; dlopen
        // returns null on failure, which we check. RTLD_LAZY|RTLD_LOCAL binds
        // symbols on first use without polluting the global namespace.
        let handle =
            unsafe { libc::dlopen(FRAMEWORK_PATH.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL) };
        if handle.is_null() {
            return None;
        }
        // The handle is intentionally never `dlclose`d: the framework stays
        // resident for the process lifetime, keeping the fn pointers valid.
        // SAFETY: `handle` is live; each requested type matches the documented C
        // signature of the named symbol.
        let get = unsafe { resolve_symbol::<GetFn>(handle, c"DisplayServicesGetBrightness") }?;
        // SAFETY: as above.
        let set = unsafe { resolve_symbol::<SetFn>(handle, c"DisplayServicesSetBrightness") }?;
        // SAFETY: as above.
        let can =
            unsafe { resolve_symbol::<CanFn>(handle, c"DisplayServicesCanChangeBrightness") }?;
        Some(DisplayServicesSyms { get, set, can })
    }

    /// The process-wide resolved table, computed once. `None` means the private
    /// framework is unavailable on this Mac.
    fn display_services() -> Option<&'static DisplayServicesSyms> {
        static SYMS: OnceLock<Option<DisplayServicesSyms>> = OnceLock::new();
        SYMS.get_or_init(load_syms).as_ref()
    }

    /// The real `DisplayServices` operation table.
    #[derive(Debug, Clone, Copy)]
    pub struct RealDisplayServices {
        syms: &'static DisplayServicesSyms,
    }

    impl RealDisplayServices {
        /// Resolve the private framework, or `None` when it is unavailable.
        #[must_use]
        pub fn resolve() -> Option<Self> {
            display_services().map(|syms| Self { syms })
        }
    }

    impl DisplayServicesApi for RealDisplayServices {
        fn can_change_brightness(&self, display: CgDisplayId) -> bool {
            // SAFETY: `can` is the resolved DisplayServicesCanChangeBrightness; it
            // takes a `CGDirectDisplayID` by value and returns a `_Bool`, with no
            // pointer arguments to invalidate.
            unsafe { (self.syms.can)(display) }
        }

        fn get_brightness(&mut self, display: CgDisplayId) -> Result<f32, PanelError> {
            let mut out: f32 = 0.0;
            // SAFETY: `out` is a valid, aligned, writable `f32`; the resolved
            // DisplayServicesGetBrightness writes the brightness through the
            // pointer and returns 0 on success.
            let rc = unsafe { (self.syms.get)(display, core::ptr::from_mut(&mut out)) };
            if rc == 0 {
                Ok(out)
            } else {
                Err(PanelError::DisplayServices {
                    context: "DisplayServicesGetBrightness",
                    code: rc,
                })
            }
        }

        fn set_brightness(&mut self, display: CgDisplayId, value: f32) -> Result<(), PanelError> {
            // SAFETY: value-only arguments; the resolved
            // DisplayServicesSetBrightness returns 0 on success.
            let rc = unsafe { (self.syms.set)(display, value) };
            if rc == 0 {
                Ok(())
            } else {
                Err(PanelError::DisplayServices {
                    context: "DisplayServicesSetBrightness",
                    code: rc,
                })
            }
        }
    }

    // The public CoreGraphics online-display list. `core-graphics` 0.25 exposes
    // only `active_displays` (CGGetActiveDisplayList); we bind the online list
    // directly to match the plan and to include a builtin panel that is online
    // but not the active drawable (e.g. mirrored). Both bounds-check against the
    // count CoreGraphics reports.
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGGetOnlineDisplayList(
            max_displays: u32,
            online_displays: *mut CgDisplayId,
            display_count: *mut u32,
        ) -> i32;
    }

    /// Flatten a `CGRect` into the FFI-free [`CgRect`] the pure code speaks.
    ///
    /// A named function rather than four lines inlined at the call site, because
    /// this is the one step no pure test can reach: a transposed `origin.x`/
    /// `origin.y` or `size.width`/`size.height` compiles, ships, and puts the
    /// panel's overlay on the wrong rectangle of every Mac while the whole suite
    /// stays green. `duja-ddc`'s `mac::sys::rect_to_bounds` is the same hand-off
    /// with the same reasoning; both are pinned by a test with four *distinct*
    /// field values, which is the only shape that catches a swap.
    fn flatten(rect: core_graphics::geometry::CGRect) -> CgRect {
        CgRect {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        }
    }

    /// The ids of every online display, or an empty list on any CoreGraphics
    /// error (the desktop / headless-runner case).
    fn online_display_ids() -> Vec<CgDisplayId> {
        let mut count: u32 = 0;
        // SAFETY: a null buffer with `max_displays == 0` asks CoreGraphics to
        // write only the display count through `display_count`.
        let rc = unsafe {
            CGGetOnlineDisplayList(0, core::ptr::null_mut(), core::ptr::from_mut(&mut count))
        };
        if rc != 0 || count == 0 {
            return Vec::new();
        }
        let cap = usize::try_from(count).unwrap_or(0);
        let mut ids: Vec<CgDisplayId> = vec![0; cap];
        let mut got: u32 = 0;
        // SAFETY: `ids` has capacity for `count` ids; CoreGraphics writes at most
        // `count` ids into the buffer and the actual number through `got`.
        let rc = unsafe {
            CGGetOnlineDisplayList(count, ids.as_mut_ptr(), core::ptr::from_mut(&mut got))
        };
        if rc != 0 {
            return Vec::new();
        }
        ids.truncate(usize::try_from(got).unwrap_or(0));
        ids
    }

    /// Enumerate controllable internal panels (see [`crate::enumerate`]).
    ///
    /// Yields an empty list when the private framework is unavailable or no
    /// builtin panel reports brightness control — the expected state on a
    /// desktop, a headless CI runner, or a Mac where Apple removed the symbols.
    /// It is infallible: every absence is modelled as an empty list, and an
    /// unmappable display is skipped rather than surfaced. [`crate::enumerate`]
    /// wraps the result in `Ok` for a uniform signature with the Windows
    /// backend, whose enumeration genuinely can fault.
    pub fn enumerate() -> Vec<PanelDisplay> {
        let Some(api) = RealDisplayServices::resolve() else {
            return Vec::new();
        };

        let mut panels = Vec::new();
        for id in online_display_ids() {
            let display = CGDisplay::new(id);
            if !is_controllable_panel(display.is_builtin(), api.can_change_brightness(id)) {
                continue;
            }
            let Ok(stable_id) = synthesize_panel_id(
                display.vendor_number(),
                display.model_number(),
                display.serial_number(),
            ) else {
                continue;
            };
            panels.push(PanelDisplay {
                id: stable_id,
                // CoreGraphics exposes no friendly name for a builtin panel; a
                // localized name would need IOKit. Use the generic label the
                // Windows backend also falls back to.
                name: "Internal Display".to_owned(),
                // On macOS `instance_name` carries the CGDirectDisplayID in
                // decimal; `PanelDisplay::open` parses it back to bind a
                // transport. See the field's docs on the crate root.
                instance_name: id.to_string(),
                // The built-in panel is an ordinary CoreGraphics display, so it
                // has a rect and a mirror state like any other. Read here, in the
                // same snapshot as everything else about this display, and handed
                // to the pure builder — which is what makes a macOS panel
                // software-dimmable at all (`docs/debt.md`).
                geometry: panel_geometry(
                    flatten(display.bounds()),
                    MirrorState {
                        display_id: id,
                        mirrors: display.mirrors_display(),
                    },
                ),
            });
        }
        panels
    }

    #[cfg(test)]
    mod tests {
        use super::{CgRect, flatten};
        use core_graphics::geometry::{CGPoint, CGRect, CGSize};

        /// Every field lands in its matching field. All four values differ, and
        /// the origin is negative where the extents cannot be, so **any** pair
        /// swapped — x/y, width/height, or origin/size — changes the result.
        ///
        /// Runs on the macOS lane only, because `CGRect` exists only there. That
        /// is the same coverage `duja-ddc`'s twin has, and it is the reason this
        /// hand-off is a named function at all.
        #[test]
        fn flatten_maps_each_cg_rect_field_to_its_own() {
            let rect = CGRect::new(&CGPoint::new(-11.0, -22.0), &CGSize::new(33.0, 44.0));
            let flat = flatten(rect);
            assert_eq!(
                flat,
                CgRect {
                    x: -11.0,
                    y: -22.0,
                    width: 33.0,
                    height: 44.0,
                }
            );
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{RealDisplayServices, enumerate};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::PanelController;
    use duja_core::controller::BrightnessController;
    use duja_core::dimmer::DisplayBounds;
    use duja_core::model::Feature;
    use duja_core::testing::contract::{Scenario, run_controller_contract};
    use std::collections::VecDeque;

    // --- float <-> level mapping ---

    #[test]
    fn float_maps_to_percent_level() {
        assert_eq!(float_to_level(0.0), 0);
        assert_eq!(float_to_level(1.0), 100);
        assert_eq!(float_to_level(0.5), 50);
        assert_eq!(float_to_level(0.37), 37);
    }

    #[test]
    fn float_to_level_clamps_out_of_range_and_nan() {
        assert_eq!(float_to_level(1.5), 100);
        assert_eq!(float_to_level(-0.2), 0);
        // NaN clamps to the low bound rather than producing a garbage level.
        assert_eq!(float_to_level(f32::NAN), 0);
    }

    #[test]
    fn level_maps_to_unit_float() {
        assert!((level_to_float(0) - 0.0).abs() < f32::EPSILON);
        assert!((level_to_float(100) - 1.0).abs() < f32::EPSILON);
        assert!((level_to_float(50) - 0.5).abs() < f32::EPSILON);
        // Levels above the max clamp to 1.0.
        assert!((level_to_float(200) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn level_float_round_trips_every_percent() {
        for level in 0..=100u8 {
            assert_eq!(float_to_level(level_to_float(level)), level);
        }
    }

    #[test]
    fn percent_levels_are_ascending_and_full() {
        let levels = percent_levels();
        assert_eq!(levels.len(), 101);
        assert_eq!(*levels.first().unwrap(), 0);
        assert_eq!(*levels.last().unwrap(), 100);
        assert!(levels.windows(2).all(|w| w.first() < w.last()));
    }

    // --- PNP vendor decode + identity synthesis ---

    #[test]
    fn decodes_apple_builtin_vendor() {
        // Apple's builtin panels report 0x0610, which decodes to "APP".
        assert_eq!(decode_pnp_vendor(0x0610).as_deref(), Some("APP"));
    }

    #[test]
    fn rejects_non_pnp_vendor() {
        assert_eq!(decode_pnp_vendor(0), None);
        assert_eq!(decode_pnp_vendor(0xFFFF_FFFF), None);
    }

    #[test]
    fn synthesizes_id_from_decoded_vendor_and_serial() {
        let id = synthesize_panel_id(0x0610, 0x0000_A2E5, 12345).unwrap();
        assert_eq!(id.as_str(), "APP-A2E5-12345");
    }

    #[test]
    fn synthesized_id_uses_low_16_bits_of_model() {
        // The EDID product-code field is 16-bit; a model with high bits set
        // truncates to the low 16, matching from_edid's field width.
        let id = synthesize_panel_id(0x0610, 0x00AB_1234, 1).unwrap();
        assert_eq!(id.as_str(), "APP-1234-1");
    }

    #[test]
    fn synthesized_id_hashes_when_serial_is_zero() {
        // A zero CG serial ("unset") routes to from_parts' stable hash fallback.
        let id = synthesize_panel_id(0x0610, 0x0000_A2E5, 0).unwrap();
        assert!(
            id.as_str().starts_with("APP-A2E5-#h"),
            "unexpected id: {}",
            id.as_str()
        );
    }

    #[test]
    fn synthesized_id_uses_sentinel_for_undecodable_vendor() {
        let id = synthesize_panel_id(0, 0x0000_1234, 0).unwrap();
        assert!(
            id.as_str().starts_with("AAP-1234-"),
            "unexpected id: {}",
            id.as_str()
        );
    }

    #[test]
    fn synthesized_id_is_stable_across_calls() {
        // Same inputs (as across a reboot) must yield the same durable key.
        let a = synthesize_panel_id(0x0610, 0x0000_A2E5, 0).unwrap();
        let b = synthesize_panel_id(0x0610, 0x0000_A2E5, 0).unwrap();
        assert_eq!(a, b);
        // A different model must move the key.
        let c = synthesize_panel_id(0x0610, 0x0000_A2E6, 0).unwrap();
        assert_ne!(a.as_str(), c.as_str());
    }

    // --- panel geometry ---

    /// A panel rect with **four distinct values**, negative where an extent
    /// cannot be: a built-in sitting left of and above the primary. A fixture
    /// with `x == y` (an origin at zero, the tempting realistic choice) would let
    /// the two be transposed and still pass.
    fn rect() -> CgRect {
        CgRect {
            x: -1512.0,
            y: -300.0,
            width: 1512.0,
            height: 982.0,
        }
    }

    fn geometry_of(rect: CgRect, mirror: MirrorState) -> PanelGeometry {
        panel_geometry(rect, mirror).expect("a non-degenerate rect must yield geometry")
    }

    fn mirror(display_id: u32, mirrors: u32) -> MirrorState {
        MirrorState {
            display_id,
            mirrors,
        }
    }

    #[test]
    fn panel_geometry_carries_the_rect_in_points() {
        let geometry = geometry_of(rect(), mirror(1, 0));
        assert_eq!(
            geometry.bounds(),
            DisplayBounds::new(-1512, -300, 1512, 982)
        );
    }

    /// `CGDisplayBounds` answers `CGRectNull` for a display it considers invalid,
    /// and this backend enumerates the *online* list, which can hold a built-in
    /// that is not the active drawable. Reporting that rect would plan a
    /// zero-size overlay at `i32::MAX` — dimming nothing, silently. `None` is the
    /// honest answer and puts the panel back exactly where it was before
    /// geometry existed.
    #[test]
    fn a_degenerate_rect_yields_no_geometry_at_all() {
        let null_rect = CgRect {
            x: f64::INFINITY,
            y: f64::INFINITY,
            width: 0.0,
            height: 0.0,
        };
        assert!(panel_geometry(null_rect, mirror(1, 0)).is_none());
        // A single zero extent is equally unusable, and the tokens go with it:
        // all three fields are one answer.
        let flat = CgRect {
            x: 0.0,
            y: 0.0,
            width: 1512.0,
            height: 0.0,
        };
        assert!(panel_geometry(flat, mirror(1, 0)).is_none());
    }

    /// A standalone built-in panel — the overwhelmingly common Mac — mirrors
    /// nothing, so it names its own surface and both tokens agree.
    #[test]
    fn a_standalone_panel_addresses_and_names_itself() {
        let geometry = geometry_of(rect(), mirror(1, 0));
        assert_eq!(geometry.gamma_token(), "1");
        assert_eq!(geometry.surface_token(), "1");
    }

    /// The case the two tokens exist for. A panel that *mirrors* an external
    /// display still has to be addressed as itself for gamma, while grouping by
    /// the master it draws with. Swapping the two arguments here is the `#66`
    /// double-overlay defect in one direction and a ramp on the wrong screen in
    /// the other — and both compile, because both tokens are `String`.
    #[test]
    fn a_mirroring_panel_addresses_itself_but_names_its_master() {
        let geometry = geometry_of(rect(), mirror(1, 7));
        assert_eq!(geometry.gamma_token(), "1", "gamma must address this panel");
        assert_eq!(
            geometry.surface_token(),
            "7",
            "grouping must follow the shared framebuffer"
        );
    }

    /// The other half of the same mirror set, as `duja-ddc` computes it for the
    /// external clone when the **panel** is the master. The two backends must land
    /// on one string or the app cannot merge the set — this asserts the agreement
    /// directly rather than trusting that both call the same function.
    #[test]
    fn a_master_panel_shares_one_surface_token_with_its_external_clone() {
        // The panel is display 1 and mirrors nothing (it is the master); the
        // external is display 20 and reports `CGDisplayMirrorsDisplay == 1`.
        let panel = geometry_of(rect(), mirror(1, 0));
        let external_surface = surface_id(mirror(20, 1)).to_string();
        assert_eq!(panel.surface_token(), external_surface);
        assert_ne!(
            panel.gamma_token(),
            "20",
            "sharing a surface must not merge the two displays' gamma addresses"
        );
    }

    // --- gating ---

    #[test]
    fn controllable_only_when_builtin_and_changeable() {
        assert!(is_controllable_panel(true, true));
        assert!(!is_controllable_panel(true, false));
        assert!(!is_controllable_panel(false, true));
        assert!(!is_controllable_panel(false, false));
    }

    // --- the controller contract against a fake DisplayServices table ---

    /// A deterministic, scriptable [`DisplayServicesApi`] for the contract suite.
    ///
    /// Mirrors the scenario semantics of the Windows fake: a queued error is
    /// returned once (never poisoning later calls), a disconnected fake fails
    /// every operation with [`PanelError::Disconnected`], and brightness is held
    /// as a `0.0..=1.0` float so the float↔level mapping is exercised end to end.
    #[derive(Debug)]
    struct FakeDisplayServices {
        brightness: f32,
        changeable: bool,
        connected: bool,
        errors: VecDeque<PanelError>,
    }

    impl FakeDisplayServices {
        fn new() -> Self {
            Self {
                brightness: 0.5,
                changeable: true,
                connected: true,
                errors: VecDeque::new(),
            }
        }

        fn disconnected() -> Self {
            let mut fake = Self::new();
            fake.connected = false;
            fake
        }

        fn push_error(&mut self, err: PanelError) {
            self.errors.push_back(err);
        }

        fn gate(&mut self) -> Result<(), PanelError> {
            if let Some(err) = self.errors.pop_front() {
                return Err(err);
            }
            if !self.connected {
                return Err(PanelError::Disconnected);
            }
            Ok(())
        }
    }

    impl DisplayServicesApi for FakeDisplayServices {
        fn can_change_brightness(&self, _display: CgDisplayId) -> bool {
            self.changeable
        }

        fn get_brightness(&mut self, _display: CgDisplayId) -> Result<f32, PanelError> {
            self.gate()?;
            Ok(self.brightness)
        }

        fn set_brightness(&mut self, _display: CgDisplayId, value: f32) -> Result<(), PanelError> {
            self.gate()?;
            self.brightness = value.clamp(0.0, 1.0);
            Ok(())
        }
    }

    fn transport(api: FakeDisplayServices) -> DisplayServicesTransport<FakeDisplayServices> {
        DisplayServicesTransport::new(1, api)
    }

    fn factory(
        scenario: Scenario,
    ) -> PanelController<DisplayServicesTransport<FakeDisplayServices>> {
        let api = match scenario {
            Scenario::Nominal => FakeDisplayServices::new(),
            Scenario::Disconnected => FakeDisplayServices::disconnected(),
            Scenario::ErrorThenOk => {
                let mut api = FakeDisplayServices::new();
                api.push_error(PanelError::Timeout);
                api
            }
        };
        PanelController::new(transport(api))
    }

    #[test]
    fn display_services_controller_satisfies_contract() {
        run_controller_contract(factory, 0);
    }

    #[test]
    fn transport_round_trips_a_percent_through_the_float_seam() {
        let mut controller = factory(Scenario::Nominal);
        controller.set(Feature::Brightness, 37).unwrap();
        assert_eq!(controller.get(Feature::Brightness).unwrap().current, 37);
    }

    #[test]
    fn fake_can_change_gate_is_honored() {
        let mut api = FakeDisplayServices::new();
        api.changeable = false;
        assert!(!api.can_change_brightness(1));
        assert!(!is_controllable_panel(true, api.can_change_brightness(1)));
    }
}
