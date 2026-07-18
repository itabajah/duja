//! Pure, cross-platform correlation of enumerated monitor targets to stable
//! display identities.
//!
//! The Windows DDC backend discovers, per active display path, a
//! [`MonitorTarget`] (adapter name, monitor device interface path, friendly
//! name, and whether the path drives an **internal** embedded panel) plus a map
//! of monitor device interface path -> raw EDID bytes. [`correlate`] joins the
//! two: it resolves every target — external or internal — to its EDID-derived
//! identity and name, flagging which is which. A laptop's built-in panel is
//! normally owned by `duja-panel`, but on many laptops its backlight is driven
//! by the GPU/OEM stack, not ACPI/WMI, so that backend cannot see it. The DDC
//! path can, so the internal target is surfaced here too (flagged `is_internal`)
//! as a **fallback** rather than dropped — otherwise the built-in screen would
//! appear in neither backend and vanish entirely.
//!
//! This module is deliberately free of any FFI so its behaviour is unit-tested
//! on every OS: the Windows `sys` layer fills the plain structs from the CCD
//! API, and the Windows enumeration attaches the physical-monitor handles to the
//! [`CorrelatedDisplay`]s this returns — binding an internal identity, via
//! [`leftover_handles`], only to a handle left over after external pairing.

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
    /// Such targets are normally owned by `duja-panel`; [`correlate`] carries the
    /// flag onto the [`CorrelatedDisplay`] so the enumeration can surface the
    /// panel as a fallback (bound to a leftover handle) when WMI cannot see it.
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
    /// Whether this identity is a laptop's internal, embedded panel (eDP) rather
    /// than an external monitor. The enumeration surfaces an internal identity
    /// only as a **fallback** — bound to a physical-monitor handle left over
    /// after external pairing — so a built-in panel that `duja-panel`'s WMI
    /// backend cannot see still appears instead of vanishing.
    pub is_internal: bool,
}

/// Correlate enumerated monitor targets to stable identities, flagging internal
/// panels rather than dropping them.
///
/// For every target — external or internal — that has a readable, parseable EDID
/// in `edids` (keyed by lower-cased interface path), one [`CorrelatedDisplay`] is
/// produced carrying its EDID-derived identity and name, with `is_internal`
/// copied from the target. A target that has no EDID, or whose EDID cannot be
/// parsed, contributes nothing (no fabricated identity) — identically for
/// internal and external targets. Output order follows `targets`.
///
/// Internal identities are a **fallback**: the Windows enumeration binds them
/// only to physical-monitor handles left over after external pairing (see
/// [`leftover_handles`]), so an external monitor never loses its handle to a
/// built-in panel.
pub(crate) fn correlate(
    targets: &[MonitorTarget],
    edids: &[(String, Vec<u8>)],
) -> Vec<CorrelatedDisplay> {
    let mut out = Vec::new();
    for target in targets {
        // External and internal targets resolve identically: identity always
        // requires a readable, parseable EDID, so a target of either kind with no
        // EDID entry (or an unparseable one) contributes nothing — no fabricated
        // identity. Internal targets are no longer dropped here; they are emitted
        // flagged `is_internal` and bound, as a fallback, to a leftover
        // physical-monitor handle by the Windows enumeration.
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
            is_internal: target.is_internal,
        });
    }
    out
}

/// Handle indices not consumed by external-display pairing, in ascending order —
/// the physical-monitor handles available to bind this `HMONITOR`'s internal
/// displays.
///
/// External displays are bound first by [`pair_handles_to_displays`]; whatever
/// handles it did not claim (identified by `used_handle_indices`) are the
/// leftovers an internal fallback display may take. A laptop's silent eDP handle,
/// which the external probe deliberately yields, is exactly such a leftover, so
/// the mirrored built-in panel binds to it here. The result is positionally
/// zipped against the internal displays: the Nth internal display takes the Nth
/// leftover, and any internal display beyond the leftover count stays unbound
/// (its would-be handle is released), never fabricating a handle.
pub(crate) fn leftover_handles(used_handle_indices: &[usize], handles_len: usize) -> Vec<usize> {
    (0..handles_len)
        .filter(|idx| !used_handle_indices.contains(idx))
        .collect()
}

/// Decide which physical-monitor handle drives each external display correlated
/// to one `HMONITOR`, given a DDC probe result per handle.
///
/// In Windows Duplicate (mirror) mode a single GDI source — hence a single
/// `HMONITOR` — fronts several physical panels, so [`correlate`] resolves more
/// than one external [`CorrelatedDisplay`] to that source while
/// `GetPhysicalMonitorsFromHMONITOR` hands back one handle per physical panel.
/// This routes the two together.
///
/// `answers_ddc[i]` is whether physical handle `i` replied to a DDC probe; the
/// caller fills it with real probe results only for an ambiguous set (more
/// handles than displays — a laptop's silent eDP mirrored beside an external
/// monitor) and otherwise passes all-`true` (a lone monitor, or identical
/// external twins whose handles drive interchangeable panels, need no probe).
/// `display_count` is how many external displays correlated to this source.
///
/// Returns `(display_index, handle_index)` pairs — one per emitted display,
/// capped at the handle count so a missing handle never fabricates a row.
/// DDC-responsive handles are consumed first, so a mirrored eDP handle that
/// stays silent yields to the external panel's handle; within a responsiveness
/// group (and whenever nothing was probed) handle order is preserved, keeping
/// the downstream interface-path ordering intact. Any handle index absent from
/// the result must be released by the caller.
pub(crate) fn pair_handles_to_displays(
    answers_ddc: &[bool],
    display_count: usize,
) -> Vec<(usize, usize)> {
    // Order handle indices so DDC-responsive handles come first, each group
    // preserving its original order: a display then binds to a real external
    // handle before a silent eDP handle, yet identical mirrored twins (all
    // responsive) and un-probed sets (all `true`) keep positional order.
    let responsive = answers_ddc
        .iter()
        .enumerate()
        .filter_map(|(i, &answered)| answered.then_some(i));
    let silent = answers_ddc
        .iter()
        .enumerate()
        .filter_map(|(i, &answered)| (!answered).then_some(i));
    (0..display_count).zip(responsive.chain(silent)).collect()
}

/// Whether a mirrored `HMONITOR`'s physical handles must be DDC-probed to bind
/// each correlated display to the handle that answers.
///
/// A probe is a paced, retried DDC read **per handle** (~300 ms of `thread::sleep`
/// for a silent handle), so it is only worth doing when there is a genuine
/// ambiguity to resolve: **more physical handles than correlated displays, and at
/// least one display to bind**.
///
/// - `handles_len <= matched_len` — every handle already has a display, so
///   pairing is positional and no probe is needed.
/// - `matched_len == 0` — this `HMONITOR` correlated to no external display (its
///   handles are a dropped internal panel, or an external whose EDID failed to
///   parse), so [`pair_handles_to_displays`] emits nothing regardless of the probe
///   results. Probing its silent handles would only stall the caller for a result
///   that is then discarded — the wasteful case this predicate exists to skip.
///
/// Returns `true` exactly when `handles_len > matched_len && matched_len > 0`.
#[must_use]
pub(crate) fn should_probe(handles_len: usize, matched_len: usize) -> bool {
    handles_len > matched_len && matched_len > 0
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
    fn internal_target_is_emitted_as_a_fallback_with_the_internal_flag() {
        // An internal panel that has a readable EDID is now SURFACED, flagged
        // internal, rather than dropped. It still belongs to duja-panel, but when
        // WMI cannot see it (GPU/OEM-driven backlight, empty WmiMonitorBrightness)
        // the DDC path is its only carrier — so correlate must emit it, with its
        // real EDID-derived identity and name, else the built-in panel vanishes.
        let edid = synth_edid("GSM", 0x5B09, Some("BUILT-IN"));
        let id = StableDisplayId::from_edid(&edid).unwrap();
        let targets = vec![target(
            "iface-internal",
            "gdi-internal",
            Some("Built-in"),
            true,
        )];
        let edids = vec![("iface-internal".to_owned(), edid)];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 1, "internal target must be emitted, got {out:?}");
        let disp = out.first().unwrap();
        assert!(
            disp.is_internal,
            "the emitted internal target carries is_internal = true"
        );
        assert_eq!(disp.id, id, "identity is still EDID-derived");
        assert_eq!(
            disp.name.as_deref(),
            Some("BUILT-IN"),
            "the EDID monitor-name wins over the friendly name"
        );
        assert_eq!(disp.gdi_device, "gdi-internal");
        assert_eq!(disp.sort_key, "iface-internal");
    }

    #[test]
    fn internal_target_without_parseable_edid_contributes_nothing() {
        // Identity still requires a readable, parseable EDID: an internal target
        // whose interface path has no EDID entry contributes nothing, exactly as
        // an external one would — the fallback never fabricates an identity.
        let targets = vec![target(
            "iface-internal",
            "gdi-internal",
            Some("Built-in"),
            true,
        )];
        let edids: Vec<(String, Vec<u8>)> = Vec::new();
        assert!(correlate(&targets, &edids).is_empty());
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
        // External targets are flagged external.
        assert!(
            out.iter().all(|c| !c.is_internal),
            "external targets carry is_internal = false"
        );
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
    fn external_next_to_internal_keeps_both_flagged() {
        // A laptop: internal eDP + one external DDC monitor. BOTH now survive
        // correlation — the external flagged external, the internal flagged
        // internal (its fallback carrier) — where the old backend dropped the
        // internal entirely and the built-in panel vanished from the tray.
        let ext = synth_edid("DEL", 0xA131, Some("DELL EXT"));
        let intl = synth_edid("AUO", 0x1234, Some("INTERNAL"));
        let id_ext = StableDisplayId::from_edid(&ext).unwrap();
        let id_intl = StableDisplayId::from_edid(&intl).unwrap();
        let targets = vec![
            target("iface-internal", "gdi-internal", Some("Built-in"), true),
            target("iface-ext", "gdi-ext", None, false),
        ];
        let edids = vec![
            ("iface-internal".to_owned(), intl.clone()),
            ("iface-ext".to_owned(), ext.clone()),
        ];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 2);
        // Output order follows `targets`: the internal target first, then external.
        let internal = out.first().unwrap();
        assert!(
            internal.is_internal,
            "the built-in panel is flagged internal"
        );
        assert_eq!(internal.id, id_intl);
        assert_eq!(internal.sort_key, "iface-internal");
        assert_eq!(internal.edid, intl);
        let external = out.get(1).unwrap();
        assert!(
            !external.is_internal,
            "the external monitor is flagged external"
        );
        assert_eq!(external.id, id_ext);
        assert_eq!(external.sort_key, "iface-ext");
        // The raw EDID is carried through verbatim.
        assert_eq!(external.edid, ext);
    }

    // --- mirror-mode cardinality at the pure correlate seam (BUG 2) ---

    #[test]
    fn mirrored_identical_externals_on_one_gdi_yield_two_colliding_ids() {
        // Duplicate (mirror) mode with two identical external panels: they are two
        // CCD targets sharing ONE GDI source. correlate emits one display PER
        // target — two displays with identical bare ids (so the manager's
        // `assign_twin_slots` later resolves them to `-slot0`/`-slot1`, exercised
        // by `identical_twin_monitors_without_serial_get_distinct_slots` in
        // duja-core) and the same GDI, but distinct interface-path sort keys.
        let edid = synth_edid("DEL", 0xA131, Some("DELL TWIN"));
        let id = StableDisplayId::from_edid(&edid).unwrap();
        let targets = vec![
            target("iface-a", "gdi-1", None, false),
            target("iface-b", "gdi-1", None, false),
        ];
        let edids = vec![
            ("iface-a".to_owned(), edid.clone()),
            ("iface-b".to_owned(), edid),
        ];
        let out = correlate(&targets, &edids);
        assert_eq!(
            out.len(),
            2,
            "one display per mirrored target, not collapsed"
        );
        assert_eq!(out.first().map(|c| c.id.clone()), Some(id.clone()));
        assert_eq!(out.get(1).map(|c| c.id.clone()), Some(id));
        assert_eq!(out.first().map(|c| c.gdi_device.as_str()), Some("gdi-1"));
        assert_eq!(out.get(1).map(|c| c.gdi_device.as_str()), Some("gdi-1"));
        assert_ne!(
            out.first().map(|c| c.sort_key.clone()),
            out.get(1).map(|c| c.sort_key.clone()),
            "the two mirrored twins must keep distinct interface-path sort keys"
        );
    }

    #[test]
    fn mirrored_internal_plus_external_on_one_gdi_keeps_both_flagged() {
        // Laptop in Duplicate mode: the built-in eDP and an external monitor are
        // two targets mirrored on ONE GDI source. correlate now emits BOTH,
        // sharing that GDI — the external flagged external, the internal flagged
        // internal. (The enumeration then probes the HMONITOR's two physical
        // handles: the external identity binds the responsive handle via
        // `pair_handles_to_displays`, and the internal identity binds the leftover
        // silent eDP handle via `leftover_handles`.)
        let ext = synth_edid("DEL", 0xA131, Some("DELL EXT"));
        let intl = synth_edid("AUO", 0x1234, Some("INTERNAL"));
        let id_ext = StableDisplayId::from_edid(&ext).unwrap();
        let id_intl = StableDisplayId::from_edid(&intl).unwrap();
        let targets = vec![
            target("iface-internal", "gdi-1", Some("Built-in"), true),
            target("iface-ext", "gdi-1", None, false),
        ];
        let edids = vec![
            ("iface-internal".to_owned(), intl),
            ("iface-ext".to_owned(), ext),
        ];
        let out = correlate(&targets, &edids);
        assert_eq!(out.len(), 2);
        let internal = out.first().unwrap();
        assert!(internal.is_internal);
        assert_eq!(internal.id, id_intl);
        assert_eq!(internal.gdi_device, "gdi-1");
        let external = out.get(1).unwrap();
        assert!(!external.is_internal);
        assert_eq!(external.id, id_ext);
        assert_eq!(external.gdi_device, "gdi-1");
    }

    // --- physical-handle ↔ display pairing for a mirrored HMONITOR (BUG 2) ---

    #[test]
    fn single_handle_single_display_pairs_positionally() {
        // Regression guard: a normal single-monitor HMONITOR (one handle, one
        // display) still emits exactly one pair — no behaviour change.
        assert_eq!(pair_handles_to_displays(&[true], 1), vec![(0, 0)]);
    }

    #[test]
    fn identical_twin_mirror_pairs_each_display_to_a_distinct_handle() {
        // Two identical external panels mirrored on one GDI: two displays, two
        // handles, both answer DDC. Each display binds to its OWN handle (distinct
        // indices) so both physical panels are independently driven — the core of
        // BUG 2, where today only one handle survived.
        assert_eq!(
            pair_handles_to_displays(&[true, true], 2),
            vec![(0, 0), (1, 1)]
        );
    }

    #[test]
    fn mirrored_edp_yields_to_the_external_handle() {
        // Laptop eDP + external mirrored: one external display, two handles. The
        // display must bind to the handle that answers DDC (the external), never
        // the silent eDP — deterministically, regardless of handle order.
        assert_eq!(pair_handles_to_displays(&[false, true], 1), vec![(0, 1)]);
        assert_eq!(pair_handles_to_displays(&[true, false], 1), vec![(0, 0)]);
    }

    #[test]
    fn excess_silent_handles_are_left_unpaired_for_release() {
        // The one responsive handle among several silent ones is chosen; the rest
        // are absent from the pairing so the caller releases them (no leak).
        assert_eq!(
            pair_handles_to_displays(&[false, true, false], 1),
            vec![(0, 1)]
        );
    }

    #[test]
    fn all_silent_multi_handle_falls_back_to_positional_pairing() {
        // If nothing answered (a transient DDC failure while enumerating) the set
        // is unresolvable; fall back to positional pairing rather than dropping the
        // display. A wrong guess is caught downstream by the verify-first write.
        assert_eq!(pair_handles_to_displays(&[false, false], 1), vec![(0, 0)]);
    }

    #[test]
    fn pairing_never_exceeds_the_handle_count() {
        // Defensive: more correlated identities than physical handles cannot
        // fabricate a handle — the pairing is capped at the handle count.
        assert_eq!(pair_handles_to_displays(&[true], 2), vec![(0, 0)]);
        assert!(pair_handles_to_displays(&[], 2).is_empty());
    }

    #[test]
    fn a_responsive_handle_is_never_dropped_for_a_silent_one() {
        // Defense-in-depth invariant: a handle that answered DDC must never be
        // the one released while a display is bound to a silent handle. Two
        // responsive handles (idx 0, 2) straddle a silent one (idx 1), with two
        // displays: both displays take the responsive handles; the ONLY unpaired
        // (caller-released) handle is the silent one.
        let pairs = pair_handles_to_displays(&[true, false, true], 2);
        assert_eq!(pairs, vec![(0, 0), (1, 2)]);
        let paired: std::collections::BTreeSet<usize> = pairs.iter().map(|&(_, h)| h).collect();
        assert!(
            paired.contains(&0) && paired.contains(&2),
            "both DDC-responsive handles are kept"
        );
        assert!(
            !paired.contains(&1),
            "only the silent handle is left for the caller to release"
        );
    }

    // --- when to spend the paced DDC probe on a mirrored HMONITOR (perf) ---

    #[test]
    fn should_probe_only_with_a_real_ambiguity_to_resolve() {
        // Probe only when there are MORE handles than displays AND at least one
        // display to bind. The pre-fix inline gate was a bare `handles > matched`,
        // which is TRUE for (2, 0) — a mirrored HMONITOR whose every target was
        // dropped (a laptop's silent internal-panel handle is the archetype) — and
        // so wasted ~300 ms of paced DDC reads per silent handle on a `matched`
        // that emits nothing. The predicate returns false there, skipping the work.
        assert!(
            !should_probe(2, 0),
            "no displays to bind ⇒ never probe (the wasteful bug)"
        );
        assert!(
            should_probe(2, 1),
            "an excess handle beside one real display ⇒ probe to disambiguate"
        );
        assert!(
            !should_probe(1, 1),
            "one handle per display ⇒ positional pairing, no probe"
        );
        assert!(
            should_probe(3, 2),
            "still an excess handle over the displays ⇒ probe"
        );
        assert!(!should_probe(0, 0), "nothing at all ⇒ no probe");
    }

    // --- binding internal fallback displays to leftover handles (BUG: vanish) ---

    #[test]
    fn leftover_handles_returns_the_handle_the_external_probe_yielded() {
        // Mirror internal+external, two handles: the external bound the
        // responsive handle (index 1), so the leftover is the silent eDP handle
        // (index 0) — which the internal panel then binds.
        assert_eq!(leftover_handles(&[1], 2), vec![0]);
        // If the external instead answered on handle 0, the eDP is handle 1.
        assert_eq!(leftover_handles(&[0], 2), vec![1]);
    }

    #[test]
    fn leftover_handles_gives_all_handles_when_no_external_consumed_any() {
        // Internal-only HMONITOR (laptop lid open, no external monitor): externals
        // consumed nothing, so every handle is a leftover and the internal panel
        // binds handle 0 positionally (the plain non-mirror case).
        assert_eq!(leftover_handles(&[], 1), vec![0]);
        assert_eq!(leftover_handles(&[], 3), vec![0, 1, 2]);
    }

    #[test]
    fn leftover_handles_is_empty_when_every_handle_is_consumed() {
        // No leftover: the lone handle went to the external, so an internal
        // display on this HMONITOR stays unbound — its handle is never fabricated
        // and it contributes nothing here (the id must be carried by WMI instead).
        assert!(leftover_handles(&[0], 1).is_empty());
        // Two externals consumed both handles.
        assert!(leftover_handles(&[0, 1], 2).is_empty());
    }

    #[test]
    fn leftover_handles_excludes_used_indices_in_any_order_and_stays_ascending() {
        // Used indices need not be sorted; leftovers are still ascending and
        // exclude every used one. Four handles with externals on {2, 0} leaves
        // {1, 3} for internal binding.
        assert_eq!(leftover_handles(&[2, 0], 4), vec![1, 3]);
    }
}
