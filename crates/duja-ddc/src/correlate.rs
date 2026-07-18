//! Pure, cross-platform correlation of enumerated monitor targets to stable
//! display identities.
//!
//! The Windows DDC backend discovers, per active display path, a
//! [`MonitorTarget`] (adapter name, monitor device interface path, friendly
//! name, and whether the path drives an **internal** embedded panel) plus a map
//! of monitor device interface path -> raw EDID bytes. [`correlate`] joins the
//! two: it resolves each external target to its EDID-derived identity and name,
//! and **omits internal targets** — a laptop's built-in panel is owned by
//! `duja-panel` (surfaced as an internal panel), never by the DDC backend.
//!
//! This module is deliberately free of any FFI so its behaviour is unit-tested
//! on every OS: the Windows `sys` layer fills the plain structs from the CCD
//! API, and the Windows enumeration attaches the physical-monitor handles to the
//! [`CorrelatedDisplay`]s this returns.

use duja_core::id::{EdidInfo, StableDisplayId};

/// One active display path, as plain (non-FFI) data ready for correlation.
///
/// The Windows FFI populates these from the `DISPLAYCONFIG_*` CCD queries; the
/// fields mirror the source/target device names plus the decoded
/// output-technology internal flag.
#[derive(Debug, Clone)]
pub(crate) struct MonitorTarget {
    /// The monitor device interface path, lower-cased (the key into the EDID
    /// map).
    pub interface_path: String,
    /// The GDI adapter/source device name, lower-cased (e.g. `\\.\display1`).
    /// Keys a resolved display back to its `HMONITOR` handle and pixel bounds.
    pub gdi_device: String,
    /// The monitor's friendly name from the CCD target, if any. Used as the
    /// display name only when the EDID carries no monitor-name descriptor.
    pub friendly: Option<String>,
    /// Whether this path drives an internal, embedded panel (e.g. a laptop eDP).
    /// Such targets are owned by `duja-panel` and are omitted from the DDC list.
    pub is_internal: bool,
}

/// An external monitor whose identity has been resolved from its EDID, still
/// awaiting the physical-monitor handle and pixel bounds attached by the Windows
/// enumeration once the matching `HMONITOR` is found via [`gdi_device`].
///
/// [`gdi_device`]: CorrelatedDisplay::gdi_device
#[derive(Debug, Clone)]
pub(crate) struct CorrelatedDisplay {
    /// The GDI adapter/source device name (lower-cased) this identity belongs
    /// to; the enumeration matches it against each `HMONITOR`'s adapter.
    pub gdi_device: String,
    /// Durable EDID-derived identity.
    pub id: StableDisplayId,
    /// Human-readable name: the EDID monitor-name descriptor, else the CCD
    /// friendly name, else `None`.
    pub name: Option<String>,
    /// The raw EDID bytes the identity was derived from.
    pub edid: Vec<u8>,
    /// Deterministic sort key (the monitor device interface path).
    pub sort_key: String,
}

/// Correlate enumerated monitor targets to stable identities, **omitting
/// internal panels**.
///
/// For every non-internal target that has a readable, parseable EDID in `edids`
/// (keyed by lower-cased interface path), one [`CorrelatedDisplay`] is produced
/// carrying its EDID-derived identity and name. A target that is internal, has
/// no EDID, or whose EDID cannot be parsed contributes nothing (no fabricated
/// identity). Output order follows `targets`.
pub(crate) fn correlate(
    targets: &[MonitorTarget],
    edids: &[(String, Vec<u8>)],
) -> Vec<CorrelatedDisplay> {
    let mut out = Vec::new();
    for target in targets {
        if target.is_internal {
            // The internal panel is enumerated by duja-panel as an internal
            // panel; the DDC backend must not surface it a second time.
            continue;
        }
        let Some((_, edid)) = edids.iter().find(|(key, _)| *key == target.interface_path) else {
            continue;
        };
        let Ok(id) = StableDisplayId::from_edid(edid) else {
            continue;
        };
        let name = EdidInfo::parse(edid)
            .ok()
            .and_then(|info| info.monitor_name)
            .or_else(|| target.friendly.clone());
        out.push(CorrelatedDisplay {
            gdi_device: target.gdi_device.clone(),
            id,
            name,
            edid: edid.clone(),
            sort_key: target.interface_path.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- lint-clean synthetic EDID construction (no indexing, wrapping
    //     arithmetic) mirroring duja-core::id's test fixtures ---

    /// Pack a three-letter manufacturer id into big-endian bytes 8..=9.
    fn mfg_bytes(mfg: &str) -> [u8; 2] {
        let mut bytes = mfg.bytes();
        let val = |c: u8| u16::from(c).wrapping_sub(64) & 0x1F;
        let v0 = val(bytes.next().unwrap_or(b'A'));
        let v1 = val(bytes.next().unwrap_or(b'A'));
        let v2 = val(bytes.next().unwrap_or(b'A'));
        ((v0 << 10) | (v1 << 5) | v2).to_be_bytes()
    }

    /// An 18-byte 0xFC monitor-name display descriptor.
    fn name_descriptor(name: &str) -> Vec<u8> {
        let mut d = vec![0x00u8, 0x00, 0x00, 0xFC, 0x00];
        let mut body: Vec<u8> = name.bytes().take(13).collect();
        body.push(0x0A);
        body.resize(13, 0x20);
        d.extend_from_slice(&body);
        d
    }

    /// A checksum-valid 128-byte EDID with a zero numeric serial (so identity
    /// takes the name/hash path) and an optional monitor-name descriptor.
    fn synth_edid(mfg: &str, product: u16, name: Option<&str>) -> Vec<u8> {
        let mut e: Vec<u8> = vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.extend_from_slice(&mfg_bytes(mfg));
        e.extend_from_slice(&product.to_le_bytes());
        e.extend_from_slice(&0u32.to_le_bytes());
        e.resize(54, 0x00);
        match name {
            Some(n) => e.extend_from_slice(&name_descriptor(n)),
            None => e.extend_from_slice(&[0u8; 18]),
        }
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        e
    }

    fn target(
        interface: &str,
        gdi: &str,
        friendly: Option<&str>,
        is_internal: bool,
    ) -> MonitorTarget {
        MonitorTarget {
            interface_path: interface.to_owned(),
            gdi_device: gdi.to_owned(),
            friendly: friendly.map(str::to_owned),
            is_internal,
        }
    }

    #[test]
    fn internal_target_is_skipped() {
        // An internal panel that DOES have a readable EDID must still be omitted
        // (it belongs to duja-panel), so correlate yields nothing.
        let edid = synth_edid("GSM", 0x5B09, Some("BUILT-IN"));
        let targets = vec![target(
            "iface-internal",
            "gdi-internal",
            Some("Built-in"),
            true,
        )];
        let edids = vec![("iface-internal".to_owned(), edid)];
        let out = correlate(&targets, &edids);
        assert!(
            out.is_empty(),
            "internal target must be skipped, got {out:?}"
        );
    }

    #[test]
    fn two_external_targets_yield_two_displays() {
        let a = synth_edid("GSM", 0x5B09, Some("LG A"));
        let b = synth_edid("DEL", 0xA131, Some("DELL B"));
        let id_a = StableDisplayId::from_edid(&a).unwrap();
        let id_b = StableDisplayId::from_edid(&b).unwrap();
        let targets = vec![
            target("iface-a", "gdi-a", None, false),
            target("iface-b", "gdi-b", None, false),
        ];
        let edids = vec![("iface-a".to_owned(), a), ("iface-b".to_owned(), b)];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 2);
        // Identity is preserved per target, in input order.
        assert_eq!(out.first().map(|c| c.id.clone()), Some(id_a));
        assert_eq!(out.get(1).map(|c| c.id.clone()), Some(id_b));
        // The GDI device is carried through for HMONITOR re-association.
        assert_eq!(out.first().map(|c| c.gdi_device.as_str()), Some("gdi-a"));
        assert_eq!(out.get(1).map(|c| c.gdi_device.as_str()), Some("gdi-b"));
    }

    #[test]
    fn edid_name_wins_and_friendly_is_the_fallback() {
        // EDID with a monitor-name descriptor: the EDID name is used and the CCD
        // friendly name is ignored. EDID without a name: fall back to friendly.
        let named = synth_edid("GSM", 0x5B09, Some("LG ULTRAGEAR"));
        let unnamed = synth_edid("DEL", 0xA131, None);
        let targets = vec![
            target("iface-named", "gdi-1", Some("ignored-friendly"), false),
            target("iface-unnamed", "gdi-2", Some("Friendly Dell"), false),
        ];
        let edids = vec![
            ("iface-named".to_owned(), named),
            ("iface-unnamed".to_owned(), unnamed),
        ];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.first().and_then(|c| c.name.clone()),
            Some("LG ULTRAGEAR".to_owned())
        );
        assert_eq!(
            out.get(1).and_then(|c| c.name.clone()),
            Some("Friendly Dell".to_owned())
        );
    }

    #[test]
    fn target_without_edid_is_skipped() {
        // No fabricated identity: an external target whose interface path has no
        // EDID entry contributes nothing.
        let targets = vec![target("iface-missing", "gdi-x", Some("X"), false)];
        let edids: Vec<(String, Vec<u8>)> = Vec::new();
        assert!(correlate(&targets, &edids).is_empty());
    }

    #[test]
    fn external_next_to_internal_keeps_only_external() {
        // A laptop: internal eDP + one external DDC monitor. Only the external
        // survives correlation; the internal panel is dropped.
        let ext = synth_edid("DEL", 0xA131, Some("DELL EXT"));
        let intl = synth_edid("AUO", 0x1234, Some("INTERNAL"));
        let id_ext = StableDisplayId::from_edid(&ext).unwrap();
        let targets = vec![
            target("iface-internal", "gdi-internal", Some("Built-in"), true),
            target("iface-ext", "gdi-ext", None, false),
        ];
        let edids = vec![
            ("iface-internal".to_owned(), intl),
            ("iface-ext".to_owned(), ext.clone()),
        ];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 1);
        assert_eq!(out.first().map(|c| c.id.clone()), Some(id_ext));
        assert_eq!(out.first().map(|c| c.sort_key.as_str()), Some("iface-ext"));
        // The raw EDID is carried through verbatim.
        assert_eq!(out.first().map(|c| c.edid.clone()), Some(ext));
    }
}
