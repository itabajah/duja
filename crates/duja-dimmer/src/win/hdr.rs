//! The HDR guard: a DXGI advanced-colour probe the app uses to force
//! [`DimMode::Overlay`](duja_core::model::DimMode) on HDR displays.
//!
//! A gamma ramp is meaningless (and often ignored) under HDR, so Duja must never
//! offer the gamma path there. This module asks DXGI whether any output is in an
//! HDR colour space ([`IDXGIOutput6::GetDesc1`]'s `ColorSpace`). The query is
//! read-only and best-effort: if DXGI is unavailable or the call fails we report
//! [`GammaSupport::Unknown`] / `None` and the caller safely defaults to overlay
//! dimming.
//!
//! Live behaviour needs a real display and is hardware-gated; the value logic
//! (mapping the probe result to a [`GammaSupport`]) is covered by pure tests.

use windows::Win32::Graphics::Dxgi::Common::DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput6};
use windows::core::Interface;

// The verdict this probe feeds is the same on every platform — only the probe
// itself is per-platform — so it lives in one unconditional module and is tested
// on all three CI lanes rather than once per backend. The crate root exports it
// unconditionally, so nothing outside sees the move. See `crate::gamma_support`.
use crate::gamma_support::{GammaSupport, gamma_support_from_hdr};

/// Whether any attached display is currently in an HDR colour space.
///
/// Returns `Some(true)` if at least one DXGI output reports the HDR10
/// (`G2084`) colour space, `Some(false)` if every probed output is SDR, and
/// `None` if DXGI could not be queried at all (no factory, or no output
/// answered). Read-only; never changes display state.
#[must_use]
pub fn is_hdr_active() -> Option<bool> {
    // SAFETY: CreateDXGIFactory1 needs no COM apartment init and returns an
    // owned interface (or an error we map to `None`).
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(_) => return None,
    };

    let mut probed_any = false;
    let mut adapter_index = 0u32;
    loop {
        // SAFETY: `factory` is a live interface; EnumAdapters returns an owned
        // adapter or DXGI_ERROR_NOT_FOUND, which ends enumeration.
        let Ok(adapter) = (unsafe { factory.EnumAdapters(adapter_index) }) else {
            break;
        };
        adapter_index = adapter_index.saturating_add(1);

        let mut output_index = 0u32;
        loop {
            // SAFETY: `adapter` is live; EnumOutputs returns an owned output or
            // DXGI_ERROR_NOT_FOUND.
            let Ok(output) = (unsafe { adapter.EnumOutputs(output_index) }) else {
                break;
            };
            output_index = output_index.saturating_add(1);

            let Ok(output6) = output.cast::<IDXGIOutput6>() else {
                continue;
            };
            // SAFETY: `output6` is a live IDXGIOutput6; GetDesc1 returns an owned
            // descriptor or an error.
            if let Ok(desc) = unsafe { output6.GetDesc1() } {
                probed_any = true;
                if desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 {
                    return Some(true);
                }
            }
        }
    }

    if probed_any { Some(false) } else { None }
}

/// Whether gamma dimming is safe on the current display configuration.
///
/// A convenience over [`is_hdr_active`]: HDR ⇒ [`GammaSupport::UnsupportedHdr`],
/// SDR ⇒ [`GammaSupport::Supported`], an indeterminate probe ⇒
/// [`GammaSupport::Unknown`].
#[must_use]
pub fn display_supports_gamma() -> GammaSupport {
    gamma_support_from_hdr(is_hdr_active())
}

#[cfg(test)]
mod tests {
    use super::*;

    // The `gamma_support_from_hdr` mapping is pinned in `crate::gamma_support`,
    // where it now lives; what is Windows-specific — and all this module still
    // owns — is the DXGI probe.

    #[test]
    fn probe_runs_without_panicking() {
        // The DXGI probe is read-only and safe to run anywhere: it must return a
        // value (any) rather than panic, even in a headless/disconnected session.
        let _ = is_hdr_active();
        let _ = display_supports_gamma();
    }
}
