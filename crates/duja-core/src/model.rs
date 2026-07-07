//! Core display domain model: features, ranges, capabilities and the
//! immutable UI-facing [`DisplaySnapshot`].
//!
//! These types are pure data shared between the OS backends (which produce
//! them) and the UI (which renders them); they carry no behaviour beyond a
//! handful of pure accessors.

// ---- specs first (TDD); implementation follows in the next commit ----

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
        assert_eq!(r, FeatureRange { current: 40, max: 100 });
    }

    #[test]
    fn capabilities_report_supported_features() {
        let caps = Capabilities {
            features: BTreeSet::from([Feature::Brightness, Feature::InputSource]),
            hardware_range: true,
            raw_capabilities: Some("(vcp(10 60))".to_owned()),
        };
        assert!(caps.supports(Feature::Brightness));
        assert!(caps.supports(Feature::InputSource));
        assert!(!caps.supports(Feature::Contrast));
        assert!(caps.hardware_range);
        assert_eq!(caps.raw_capabilities.as_deref(), Some("(vcp(10 60))"));
    }

    #[test]
    fn capabilities_default_is_empty_software_only() {
        let caps = Capabilities::default();
        assert!(caps.features.is_empty());
        assert!(!caps.hardware_range);
        assert_eq!(caps.raw_capabilities, None);
        assert!(!caps.supports(Feature::Brightness));
    }

    #[test]
    fn dim_mode_defaults_to_overlay() {
        assert_eq!(DimMode::default(), DimMode::Overlay);
    }

    #[test]
    fn display_kind_variants_are_distinct() {
        assert_ne!(DisplayKind::ExternalDdc, DisplayKind::InternalPanel);
        assert_ne!(DisplayKind::InternalPanel, DisplayKind::SoftwareOnly);
    }

    #[test]
    fn display_snapshot_holds_ui_fields() {
        let snap = DisplaySnapshot {
            id: sample_id(),
            name: "Left".to_owned(),
            kind: DisplayKind::ExternalDdc,
            user_level_pct: 42,
            capabilities: Capabilities::default(),
        };
        assert_eq!(snap.user_level_pct, 42);
        assert_eq!(snap.kind, DisplayKind::ExternalDdc);
        assert_eq!(snap.name, "Left");
        assert!(snap.id.as_str().starts_with("AAA-0000-#h"));
        assert_eq!(snap.clone(), snap);
    }
}
