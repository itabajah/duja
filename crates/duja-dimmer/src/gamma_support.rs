//! The cross-platform vocabulary of the opt-in gamma path: whether it may be used
//! at all, and what a restore pass did.
//!
//! One verdict, three probes. Every platform answers "is HDR active?" its own way
//! — DXGI's `IDXGIOutput6::GetDesc1` colour space on Windows, `NSScreen`'s EDR
//! headroom on macOS, the session's transport on Linux (which has no query at
//! all) — but what that answer *means* is the same everywhere, and it is the part
//! with a rule worth pinning: an uncertain probe must read as "no gamma", because
//! a ramp under HDR is at best ignored and at worst a display Duja believes it has
//! dimmed and has not.
//!
//! So the verdict lives here, unconditionally, and is tested on all three CI
//! lanes; the probes stay in `win::hdr`, `mac::edr` and `linux::gamma`, each of
//! which imports what it needs. This was two byte-identical copies before Linux
//! would have made it three.
//!
//! The **crate**'s surface is unchanged — the root exports these names for every
//! target instead of once per backend — but the per-platform modules' is not:
//! `win::hdr::GammaSupport` and its macOS twin are gone, and the two `mod.rs`
//! re-export lists shrank to match. Nothing outside this crate could name them.

/// Whether a display can safely use the gamma dimming path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GammaSupport {
    /// Gamma is safe here (SDR display, probe succeeded).
    Supported,
    /// An HDR colour space is active; gamma must not be used (force overlay).
    UnsupportedHdr,
    /// The probe could not determine HDR state; the caller should default to
    /// overlay dimming (the safe choice).
    Unknown,
}

impl GammaSupport {
    /// Whether the gamma path may be used. Only [`Supported`](Self::Supported)
    /// returns `true`; [`Unknown`](Self::Unknown) is treated as "no" so an
    /// uncertain probe never risks an ineffective gamma dim under HDR.
    #[must_use]
    pub fn allows_gamma(self) -> bool {
        matches!(self, GammaSupport::Supported)
    }
}

/// Map the raw HDR probe (`Some(true)` = HDR active, `Some(false)` = SDR,
/// `None` = unknown) to a [`GammaSupport`]. Pure, so it is unit-tested directly.
#[must_use]
pub fn gamma_support_from_hdr(hdr_active: Option<bool>) -> GammaSupport {
    match hdr_active {
        Some(true) => GammaSupport::UnsupportedHdr,
        Some(false) => GammaSupport::Supported,
        None => GammaSupport::Unknown,
    }
}

/// What a `restore_all` pass did: the displays whose gamma it reset and the ones
/// it could not, with the error text.
///
/// The shape is common; what a row *means* is per-platform and is documented at
/// each backend's `restore_all`, because the three differ in ways a caller can
/// see. Windows writes an identity ramp to each display it enumerated and can
/// report a per-display failure. macOS makes one global
/// `CGDisplayRestoreColorSyncSettings` call that returns `void`, so its `failed`
/// is always empty and its "restored" means the profile, not identity.
///
/// Linux is two answers, because it is two channels. On **X11** it writes identity
/// to every `RandR` CRTC with a writable table — **including ones driving no
/// output**, because a gamma table survives its CRTC being disabled — and can fail
/// per CRTC like Windows. On **Wayland** it is not a rescue at all and cannot be:
/// the compositor restores an output's table when the client's gamma-control
/// object dies, and it does that when the socket closes, so there is never a stale
/// ramp for a later process to find. It hands back the controls *this* process
/// holds, names those outputs, and never fails.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Names of the displays whose gamma was restored.
    pub restored: Vec<String>,
    /// `(name, error)` for each display that could not be restored.
    pub failed: Vec<(String, String)>,
}

impl RestoreReport {
    /// Whether every attempted display was restored (no failures).
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_is_clean_until_something_fails() {
        let mut report = RestoreReport::default();
        assert!(report.is_clean());
        report.restored.push("DP-1".to_owned());
        assert!(report.is_clean(), "a success must not dirty the report");
        report.failed.push(("DP-2".to_owned(), "boom".to_owned()));
        assert!(!report.is_clean());
    }

    #[test]
    fn hdr_maps_to_unsupported() {
        assert_eq!(
            gamma_support_from_hdr(Some(true)),
            GammaSupport::UnsupportedHdr
        );
        assert!(!gamma_support_from_hdr(Some(true)).allows_gamma());
    }

    #[test]
    fn sdr_maps_to_supported() {
        assert_eq!(gamma_support_from_hdr(Some(false)), GammaSupport::Supported);
        assert!(gamma_support_from_hdr(Some(false)).allows_gamma());
    }

    #[test]
    fn unknown_defaults_to_no_gamma() {
        assert_eq!(gamma_support_from_hdr(None), GammaSupport::Unknown);
        assert!(!gamma_support_from_hdr(None).allows_gamma());
    }
}
