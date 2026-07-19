//! Core display domain model: features, ranges, capabilities and the
//! immutable UI-facing [`DisplaySnapshot`].
//!
//! These types are pure data shared between the OS backends (which produce
//! them) and the UI (which renders them); they carry no behaviour beyond a
//! handful of pure accessors.

use std::collections::BTreeSet;

use crate::id::StableDisplayId;

/// A DDC/CI VCP feature that Duja reads and writes.
///
/// The discriminant order (`Brightness` < `Contrast` < `InputSource`) is
/// deliberate: it makes [`Feature`] usable as a [`BTreeSet`] key with a stable,
/// meaningful ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Feature {
    /// Luminance / backlight (VCP code `0x10`).
    Brightness,
    /// Contrast (VCP code `0x12`).
    Contrast,
    /// Input source selection (VCP code `0x60`).
    InputSource,
}

impl Feature {
    /// Every [`Feature`] variant, for exhaustive iteration in probes and tests.
    pub const ALL: [Feature; 3] = [Feature::Brightness, Feature::Contrast, Feature::InputSource];

    /// The MCCS VCP code that identifies this feature on the wire.
    #[must_use]
    pub fn vcp_code(&self) -> u8 {
        match self {
            Feature::Brightness => 0x10,
            Feature::Contrast => 0x12,
            Feature::InputSource => 0x60,
        }
    }
}

/// The current value and maximum of a VCP feature, as reported by hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureRange {
    /// The current raw value.
    pub current: u16,
    /// The maximum raw value the feature accepts.
    pub max: u16,
}

/// What a display can do, as discovered during probing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// The VCP features the display reports as controllable.
    pub features: BTreeSet<Feature>,
    /// Whether the display exposes a real hardware brightness range (`true` for
    /// DDC/panel-backed displays; `false` for software-only dimming).
    pub hardware_range: bool,
    /// The raw MCCS capability string, if the backend captured one.
    pub raw_capabilities: Option<String>,
    /// The discrete VCP `0x60` input-source values this display accepts, after
    /// intersecting the capability-string value list with any
    /// `input_source_allowed` quirk and clearing everything when `no_input_switch`
    /// is set. Empty when input switching is unsupported, unknown, or disabled.
    ///
    /// The probe computes this once; the values are raw MCCS input codes (e.g.
    /// `0x11` HDMI-1, `0x0F` `DisplayPort` — see [`crate::input_source`]).
    pub allowed_inputs: Vec<u8>,
}

impl Capabilities {
    /// Whether `feature` is among the supported set.
    #[must_use]
    pub fn supports(&self, feature: Feature) -> bool {
        self.features.contains(&feature)
    }

    /// Whether `code` is an input-source value this display accepts.
    #[must_use]
    pub fn allows_input(&self, code: u8) -> bool {
        self.allowed_inputs.contains(&code)
    }
}

/// How sub-hardware-floor dimming is realised for a display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DimMode {
    /// A translucent black overlay window (default; GPU-cheap, HDR-safe).
    #[default]
    Overlay,
    /// A reduced gamma ramp (opt-in; disabled under HDR).
    Gamma,
    /// No software dimming — clamp at the hardware floor.
    Off,
}

/// The physical class (provenance) of a display: external vs. built-in.
///
/// This is a **hardware** classification only, fixed at enumeration. Whether a
/// display currently has no working hardware brightness — so Duja dims it purely
/// in software — is a separate *runtime control-mode* flag,
/// [`DisplaySnapshot::software_only`], **not** a kind: a panel can be an
/// [`InternalPanel`](DisplayKind::InternalPanel) *and* software-only at the same
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayKind {
    /// An external monitor controlled over DDC/CI.
    ExternalDdc,
    /// A built-in laptop/all-in-one panel (WMI / `DisplayServices` / backlight).
    InternalPanel,
}

/// An immutable, UI-facing view of a single display's current state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySnapshot {
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Human-readable name for display in the UI.
    pub name: String,
    /// The display's physical class (external vs. built-in) — provenance only; see
    /// [`software_only`](Self::software_only) for its runtime control mode.
    pub kind: DisplayKind,
    /// Whether this display currently has **no working hardware brightness** and is
    /// therefore dimmed purely in software (a full-slider overlay/gamma continuum).
    ///
    /// A runtime control-mode flag, independent of [`kind`](Self::kind): an
    /// [`InternalPanel`](DisplayKind::InternalPanel) or
    /// [`ExternalDdc`](DisplayKind::ExternalDdc) display can be software-only when
    /// its hardware channel is dead. Enumeration never sets it; only the engine's
    /// runtime no-hardware detection does (via the manager's software-only overlay).
    pub software_only: bool,
    /// The single unified user brightness level, 0..=100.
    pub user_level_pct: u8,
    /// Probed capabilities.
    pub capabilities: Capabilities,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::StableDisplayId;
    use std::collections::BTreeSet;

    /// Build a valid, minimal EDID and derive an id from it (manufacturer AAA,
    /// no serial), so snapshot tests have a real [`StableDisplayId`].
    fn sample_id() -> StableDisplayId {
        let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.push(0x04); // bytes 8..=9 encode "AAA"
        e.push(0x21);
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        StableDisplayId::from_edid(&e).unwrap()
    }

    #[test]
    fn feature_vcp_codes_match_mccs() {
        assert_eq!(Feature::Brightness.vcp_code(), 0x10);
        assert_eq!(Feature::Contrast.vcp_code(), 0x12);
        assert_eq!(Feature::InputSource.vcp_code(), 0x60);
    }

    #[test]
    fn feature_all_lists_every_variant() {
        assert_eq!(Feature::ALL.len(), 3);
        assert!(Feature::ALL.contains(&Feature::Brightness));
        assert!(Feature::ALL.contains(&Feature::Contrast));
        assert!(Feature::ALL.contains(&Feature::InputSource));
    }

    #[test]
    fn feature_range_carries_current_and_max() {
        let r = FeatureRange {
            current: 40,
            max: 100,
        };
        assert_eq!(r.current, 40);
        assert_eq!(r.max, 100);
        assert_eq!(
            r,
            FeatureRange {
                current: 40,
                max: 100
            }
        );
    }

    #[test]
    fn capabilities_report_supported_features() {
        let caps = Capabilities {
            features: BTreeSet::from([Feature::Brightness, Feature::InputSource]),
            hardware_range: true,
            raw_capabilities: Some("(vcp(10 60))".to_owned()),
            allowed_inputs: vec![0x11, 0x0F],
        };
        assert!(caps.supports(Feature::Brightness));
        assert!(caps.supports(Feature::InputSource));
        assert!(!caps.supports(Feature::Contrast));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities.as_deref(), Some("(vcp(10 60))"));
        assert!(caps.allows_input(0x11));
        assert!(caps.allows_input(0x0F));
        assert!(!caps.allows_input(0x12));
    }

    #[test]
    fn capabilities_default_is_empty_software_only() {
        let caps = Capabilities::default();
        assert!(caps.features.is_empty());
        assert!(!caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
        assert!(!caps.supports(Feature::Brightness));
        assert!(caps.allowed_inputs.is_empty());
        assert!(!caps.allows_input(0x11));
    }

    #[test]
    fn dim_mode_defaults_to_overlay() {
        assert_eq!(DimMode::default(), DimMode::Overlay);
    }

    #[test]
    fn display_kind_variants_are_distinct() {
        // Physical provenance only — no software-only kind (that is a runtime flag).
        assert_ne!(DisplayKind::ExternalDdc, DisplayKind::InternalPanel);
    }

    #[test]
    fn display_snapshot_holds_ui_fields() {
        let snap = DisplaySnapshot {
            id: sample_id(),
            name: "Left".to_owned(),
            kind: DisplayKind::ExternalDdc,
            software_only: false,
            user_level_pct: 42,
            capabilities: Capabilities::default(),
        };
        assert_eq!(snap.user_level_pct, 42);
        assert_eq!(snap.kind, DisplayKind::ExternalDdc);
        assert!(!snap.software_only);
        assert_eq!(snap.name, "Left");
        assert!(snap.id.as_str().starts_with("AAA-0000-#h"));
        assert_eq!(snap.clone(), snap);
    }

    #[test]
    fn display_snapshot_can_be_software_only_on_any_physical_kind() {
        // The decouple: software-only is orthogonal to the physical kind, so an
        // internal panel with a dead backlight channel is `InternalPanel` AND
        // `software_only` — the kind is never overwritten to encode the mode.
        let snap = DisplaySnapshot {
            id: sample_id(),
            name: "Internal".to_owned(),
            kind: DisplayKind::InternalPanel,
            software_only: true,
            user_level_pct: 30,
            capabilities: Capabilities::default(),
        };
        assert_eq!(snap.kind, DisplayKind::InternalPanel);
        assert!(snap.software_only);
    }
}
