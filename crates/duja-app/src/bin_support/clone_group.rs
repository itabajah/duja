//! Grouping mirrored (Duplicate/clone) displays into one logical control.
//!
//! In Windows Duplicate mode N physical panels show the SAME cloned framebuffer
//! from ONE GDI source. Since v0.1.2 Duja lists one row per physical panel, but a
//! software-dim overlay is a window over the *shared* desktop region, so it lands
//! on every clone at once — per-panel software dimming of a mirrored set is
//! physically impossible, and two mirrored rows at identical bounds even stack two
//! overlay windows on the same pixels (#66).
//!
//! This module is the app-layer policy that collapses a mirrored set into one
//! control (ADR-0018: the app owns policy; the engine stays per-panel). Displays
//! are grouped by their GDI source; each group presents as one merged row, drives
//! one dimming command per shared surface, and fans a level change back out to its
//! members:
//!
//! - **all members have working hardware** → drive every member's hardware to the
//!   target (the shared content stays uniform; the one overlay only appears below
//!   the floor);
//! - **any member is software-only** → the whole group is software-only: the one
//!   shared overlay is the sole uniform dimmer, and the hardware-capable members
//!   are pinned to MAX so they neither double-dim under the overlay nor sit stuck
//!   dark (see [`fan_out_hardware`]).
//!
//! The anchor of a group is the member with the lowest resolved id string — a pure
//! function of the member *set*, never enumeration order, so it does not flicker
//! across the frequent `DisplaysChanged` echoes and the anchor-keyed state (level,
//! user-controlled, overlay) stays put.
//!
//! The module is OS-free and fully unit-tested; the Windows tray assembly builds a
//! [`CloneGrouping`] on every enumeration and routes every group operation through
//! it.

// RATIONALE: these pure modules are consumed only by the Windows tray assembly,
// but stay cross-platform (not cfg-gated) so their unit tests run on every CI
// OS; the dead-code allow applies only where no consumer exists.
#![cfg_attr(not(windows), allow(dead_code))]

use std::collections::{BTreeMap, BTreeSet};

use duja_core::id::StableDisplayId;
use duja_core::model::DisplayKind;

/// The hardware percentage a hardware-capable member of a *software-only* group is
/// pinned to: full brightness, so the one shared overlay is the group's sole
/// uniform dimming channel (never a partial hardware level that would double-dim
/// under the overlay or leave the panel stuck dark).
const HARDWARE_MAX_PCT: u8 = 100;

/// One physical panel considered for grouping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupMember {
    /// Resolved display id (slot-suffixed for twins).
    pub(crate) id: StableDisplayId,
    /// Physical class (provenance).
    pub(crate) kind: DisplayKind,
    /// Whether this member has no working hardware brightness (dimmed purely in
    /// software). Kept per-member because the fan-out treats hardware and
    /// software-only members of the same group differently.
    pub(crate) software_only: bool,
    /// The GDI source (`\\.\DISPLAY<n>`, case-folded on grouping) the panel draws
    /// from, or `None` for a pure-WMI panel with no plumbed GDI device — which then
    /// cannot be grouped and stays its own singleton.
    pub(crate) device: Option<String>,
    /// Human-readable name (from EDID), for the merged row label.
    pub(crate) name: String,
}

/// A set of panels that share one GDI surface, presented as a single control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloneGroup {
    /// The state/UI key: the member with the lowest resolved id string.
    pub(crate) anchor: StableDisplayId,
    /// Every member of the group (including the anchor), id-sorted, so the fan-out
    /// can address each panel's hardware.
    pub(crate) members: Vec<GroupMember>,
    /// Physical class of the anchor (provenance for the merged row).
    pub(crate) kind: DisplayKind,
    /// Whether *any* member is software-only — i.e. NOT every member has working
    /// hardware. Drives the whole group onto the shared-overlay dimming path.
    pub(crate) software_only: bool,
    /// The merged row label (see [`merged_name`]).
    pub(crate) name: String,
    /// Whether this is a genuine mirror of ≥ 2 panels (vs a lone display).
    pub(crate) mirrored: bool,
}

/// The full grouping of an enumeration, plus a member-id → group index.
#[derive(Debug, Clone, Default)]
pub(crate) struct CloneGrouping {
    groups: Vec<CloneGroup>,
    index: BTreeMap<StableDisplayId, usize>,
}

impl CloneGrouping {
    /// Every group, in a deterministic (anchor-sorted) order.
    pub(crate) fn groups(&self) -> &[CloneGroup] {
        &self.groups
    }

    /// The group a member id belongs to, or `None` if the id is unknown.
    pub(crate) fn group_of(&self, id: &StableDisplayId) -> Option<&CloneGroup> {
        self.index.get(id).and_then(|&i| self.groups.get(i))
    }

    /// The anchor a member id routes to, or `None` if the id is unknown.
    pub(crate) fn anchor_of(&self, id: &StableDisplayId) -> Option<&StableDisplayId> {
        self.group_of(id).map(|group| &group.anchor)
    }
}

/// Group panels by their shared GDI source into [`CloneGroup`]s.
///
/// Panels with the same (case-folded) `device` form one mirrored group; a panel
/// with `device: None` (a pure-WMI panel with no GDI source) cannot be correlated
/// to any surface and stays its own singleton (a documented residual). The anchor
/// of each group is the lowest-id member, so the grouping is a pure function of the
/// member set and stable across enumeration-order churn.
pub(crate) fn group_clones(members: &[GroupMember]) -> CloneGrouping {
    // Bucket by the case-folded GDI source. A `None` device (a pure-WMI panel)
    // cannot be correlated to a surface, so it becomes its own singleton group
    // immediately rather than sharing a bucket with other `None`s.
    let mut keyed: BTreeMap<String, Vec<GroupMember>> = BTreeMap::new();
    let mut groups: Vec<CloneGroup> = Vec::new();
    for member in members {
        match member.device.as_deref() {
            Some(device) => keyed
                .entry(device.to_ascii_lowercase())
                .or_default()
                .push(member.clone()),
            None => groups.extend(build_group(vec![member.clone()])),
        }
    }
    for (_device, bucket) in keyed {
        groups.extend(build_group(bucket));
    }
    // Order the groups by anchor so `groups()` is deterministic regardless of the
    // enumeration order the members arrived in (the flyout re-sorts new rows by id
    // itself, so this order only needs to be stable, not UI-significant).
    groups.sort_by(|a, b| a.anchor.cmp(&b.anchor));
    let index = groups
        .iter()
        .enumerate()
        .flat_map(|(i, group)| group.members.iter().map(move |m| (m.id.clone(), i)))
        .collect();
    CloneGrouping { groups, index }
}

/// Assemble one [`CloneGroup`] from a non-empty bucket of members. Returns `None`
/// for an empty bucket (never produced here) so the caller stays panic-free.
fn build_group(mut members: Vec<GroupMember>) -> Option<CloneGroup> {
    // Sort by id so the anchor is `members[0]` and the fan-out order is stable.
    members.sort_by(|a, b| a.id.cmp(&b.id));
    let (anchor, kind) = {
        let first = members.first()?;
        (first.id.clone(), first.kind)
    };
    let mirrored = members.len() >= 2;
    // Software-only iff NOT every member has working hardware.
    let software_only = members.iter().any(|m| m.software_only);
    let name = {
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        merged_name(&names)
    };
    Some(CloneGroup {
        anchor,
        members,
        kind,
        software_only,
        name,
        mirrored,
    })
}

/// The merged row label for a group's member names.
///
/// One name for a lone display; `"Mirrored — A + B"` for a mirror, de-duplicating
/// identical names so twins showing the same EDID name read as one.
pub(crate) fn merged_name(names: &[&str]) -> String {
    if names.len() <= 1 {
        return names.first().map(|s| (*s).to_owned()).unwrap_or_default();
    }
    // De-duplicate identical names (twins report the same EDID name) while keeping
    // first-seen order.
    let mut unique: Vec<&str> = Vec::new();
    for name in names {
        if !unique.contains(name) {
            unique.push(name);
        }
    }
    format!("Mirrored — {}", unique.join(" + "))
}

/// The per-member hardware writes a group level fans out to.
///
/// `Some(hw)` (an all-hardware group) drives every member to `hw`. `None` (a
/// software-only group) pins every hardware-capable member to [`HARDWARE_MAX_PCT`]
/// and skips the software-only members (they have no hardware to write) — so the
/// group's one shared overlay is the single uniform dimmer.
pub(crate) fn fan_out_hardware(
    members: &[GroupMember],
    hardware_pct: Option<u8>,
) -> Vec<(StableDisplayId, u8)> {
    match hardware_pct {
        // All-hardware group: every member gets the floored hardware target.
        Some(hw) => members.iter().map(|m| (m.id.clone(), hw)).collect(),
        // Software-only group: pin the hardware-capable members to MAX (the shared
        // overlay does all the dimming); the software-only members have no hardware
        // to write and are skipped.
        None => members
            .iter()
            .filter(|m| !m.software_only)
            .map(|m| (m.id.clone(), HARDWARE_MAX_PCT))
            .collect(),
    }
}

/// Whether every member of a group is unresponsive — the condition to grey the
/// merged row. Any live member keeps the merged slider interactive.
pub(crate) fn all_unresponsive(
    members: &[GroupMember],
    unresponsive: &BTreeSet<StableDisplayId>,
) -> bool {
    // A never-populated (empty) group is not "all unresponsive" (vacuous truth
    // would grey a phantom row); a real group greys only with every member down.
    !members.is_empty() && members.iter().all(|m| unresponsive.contains(&m.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(serial: &str) -> StableDisplayId {
        StableDisplayId::from_parts("GSM", 0x0001, Some(serial)).unwrap()
    }

    fn member(
        serial: &str,
        kind: DisplayKind,
        software_only: bool,
        device: Option<&str>,
        name: &str,
    ) -> GroupMember {
        GroupMember {
            id: id(serial),
            kind,
            software_only,
            device: device.map(str::to_owned),
            name: name.to_owned(),
        }
    }

    const DEV1: &str = r"\\.\DISPLAY1";
    const DEV2: &str = r"\\.\DISPLAY2";

    #[test]
    fn two_mirrored_members_form_one_group() {
        // Two panels on the SAME GDI source ⇒ ONE group with two members: the very
        // collapse #66 needs (two rows → one control).
        let members = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "Dell"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "Dell"),
        ];
        let grouping = group_clones(&members);
        assert_eq!(grouping.groups().len(), 1, "same GDI source ⇒ one group");
        let group = grouping.groups().first().expect("one group");
        assert!(group.mirrored);
        assert_eq!(group.members.len(), 2);
        // Both members resolve to the same group, keyed under the lowest-id anchor.
        assert_eq!(grouping.anchor_of(&id("A")), Some(&id("A")));
        assert_eq!(grouping.anchor_of(&id("B")), Some(&id("A")));
    }

    #[test]
    fn extended_members_distinct_devices_do_not_group() {
        // Distinct GDI sources (an EXTENDED desktop) ⇒ two separate groups — the
        // guard against over-merging a normal multi-monitor setup.
        let members = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "Left"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV2), "Right"),
        ];
        let grouping = group_clones(&members);
        assert_eq!(grouping.groups().len(), 2, "distinct sources ⇒ two groups");
        assert!(grouping.groups().iter().all(|group| !group.mirrored));
    }

    #[test]
    fn mixed_hardware_and_software_group_routes_software_only() {
        // A mirror where one clone has working hardware and one is software-only.
        let members = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "HW"),
            member("B", DisplayKind::InternalPanel, true, Some(DEV1), "SW"),
        ];
        let grouping = group_clones(&members);
        let group = grouping.groups().first().expect("one group");
        assert!(
            group.software_only,
            "any software-only member ⇒ software-only group"
        );
        // The apply rule for a software-only group (hardware_pct None): pin ONLY the
        // hardware-capable member to MAX and SKIP the software-only one, so the one
        // shared overlay is the sole uniform dimmer.
        let writes = fan_out_hardware(&group.members, None);
        assert_eq!(writes, vec![(id("A"), 100)]);
    }

    #[test]
    fn all_hardware_group_drives_every_member() {
        // An all-hardware group fans the SAME floored hardware target to every
        // member (one SetUserLevel per panel).
        let members = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "A"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "B"),
        ];
        let grouping = group_clones(&members);
        let group = grouping.groups().first().expect("one group");
        let writes = fan_out_hardware(&group.members, Some(42));
        assert_eq!(writes, vec![(id("A"), 42), (id("B"), 42)]);
    }

    #[test]
    fn anchor_is_lowest_resolved_id_regardless_of_input_order() {
        // The anchor is a pure function of the member SET, so it does not flicker
        // across the frequent DisplaysChanged echoes that arrive in any order.
        let forward = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "A"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "B"),
            member("C", DisplayKind::ExternalDdc, false, Some(DEV1), "C"),
        ];
        let mut reversed = forward.clone();
        reversed.reverse();
        let anchor_fwd = group_clones(&forward)
            .groups()
            .first()
            .expect("one group")
            .anchor
            .clone();
        let anchor_rev = group_clones(&reversed)
            .groups()
            .first()
            .expect("one group")
            .anchor
            .clone();
        assert_eq!(anchor_fwd, id("A"), "lowest id is the anchor");
        assert_eq!(anchor_fwd, anchor_rev, "anchor is order-independent");
    }

    #[test]
    fn none_device_member_is_its_own_group() {
        // A pure-WMI panel (device None) has no GDI source to correlate, so it stays
        // a singleton even beside a real mirror — the documented residual.
        let members = vec![
            member("A", DisplayKind::InternalPanel, false, None, "Panel"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "Ext"),
            member("C", DisplayKind::ExternalDdc, false, Some(DEV1), "Ext"),
        ];
        let grouping = group_clones(&members);
        assert_eq!(grouping.groups().len(), 2);
        let a_group = grouping.group_of(&id("A")).expect("A grouped");
        assert_eq!(a_group.members.len(), 1);
        assert!(!a_group.mirrored);
        let b_group = grouping.group_of(&id("B")).expect("B grouped");
        assert!(b_group.mirrored);
        assert_eq!(b_group.members.len(), 2);
    }

    #[test]
    fn two_none_device_members_stay_separate() {
        // `None` never buckets with `None`: each un-correlatable panel is its own
        // group (they might not even be mirrors of each other).
        let members = vec![
            member("A", DisplayKind::InternalPanel, false, None, "Panel A"),
            member("B", DisplayKind::InternalPanel, false, None, "Panel B"),
        ];
        let grouping = group_clones(&members);
        assert_eq!(grouping.groups().len(), 2);
    }

    #[test]
    fn merged_name_formats_and_dedupes_twins() {
        assert_eq!(merged_name(&["Dell U2720"]), "Dell U2720");
        assert_eq!(merged_name(&["Left", "Right"]), "Mirrored — Left + Right");
        // Identical twin names collapse to one.
        assert_eq!(merged_name(&["Dell", "Dell"]), "Mirrored — Dell");
        assert_eq!(merged_name(&["A", "B", "A"]), "Mirrored — A + B");
    }

    #[test]
    fn device_grouping_is_case_insensitive() {
        // The DDC backend lower-cases GDI names, but grouping case-folds anyway so a
        // stray upper-case device never splits a real mirror.
        let members = vec![
            member(
                "A",
                DisplayKind::ExternalDdc,
                false,
                Some(r"\\.\DISPLAY1"),
                "A",
            ),
            member(
                "B",
                DisplayKind::ExternalDdc,
                false,
                Some(r"\\.\display1"),
                "B",
            ),
        ];
        let grouping = group_clones(&members);
        assert_eq!(grouping.groups().len(), 1, "case-folded device ⇒ one group");
    }

    #[test]
    fn group_greys_only_when_all_members_unresponsive() {
        let members = vec![
            member("A", DisplayKind::ExternalDdc, false, Some(DEV1), "A"),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "B"),
        ];
        let grouping = group_clones(&members);
        let group = grouping.groups().first().expect("one group");
        let mut down = BTreeSet::new();
        assert!(!all_unresponsive(&group.members, &down));
        down.insert(id("A"));
        assert!(
            !all_unresponsive(&group.members, &down),
            "one live member keeps the merged row interactive"
        );
        down.insert(id("B"));
        assert!(
            all_unresponsive(&group.members, &down),
            "all members down ⇒ grey the merged row"
        );
    }

    #[test]
    fn fan_out_software_only_group_skips_a_software_only_anchor() {
        // The lowest-id (anchor) member can itself be the software-only one; the pin
        // rule still only writes the hardware-capable members.
        let members = vec![
            member(
                "A",
                DisplayKind::InternalPanel,
                true,
                Some(DEV1),
                "SW anchor",
            ),
            member("B", DisplayKind::ExternalDdc, false, Some(DEV1), "HW"),
        ];
        let grouping = group_clones(&members);
        let group = grouping.groups().first().expect("one group");
        assert_eq!(group.anchor, id("A"), "A is the lowest id, so the anchor");
        assert!(group.software_only);
        let writes = fan_out_hardware(&group.members, None);
        assert_eq!(
            writes,
            vec![(id("B"), 100)],
            "only the hardware member is pinned"
        );
    }
}
