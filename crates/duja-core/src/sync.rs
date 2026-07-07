//! Multi-monitor **sync groups**: named sets of displays that move together
//! off one master brightness, each member offset by a fixed number of
//! percentage points.
//!
//! Like the rest of `duja-core` this is pure data + pure functions. The
//! controller actor owns a [`SyncGroups`] and calls [`SyncGroups::fan_out`]
//! whenever the master of a group changes; the returned per-display targets are
//! handed to the DDC/panel workers.
//!
//! **No drift by construction:** [`fan_out`](SyncGroups::fan_out) derives every
//! member's value from the master value and the member's offset, never from the
//! member's previous value. Repeatedly fanning out the same master is therefore
//! idempotent — a member pinned at the clamp bound stays there instead of
//! creeping.

// RATIONALE: the domain vocabulary namespaces its central type as `SyncGroups`
// inside the `sync` module; the repetition reads correctly at call sites
// (`sync::SyncGroups`) and the name is fixed by the plan.
#![allow(clippy::module_name_repetitions)]

use std::collections::{BTreeMap, BTreeSet};

use crate::id::StableDisplayId;

/// One display's membership: which group it is in and its percentage offset
/// from that group's master.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Membership {
    group: String,
    offset: i8,
}

/// A collection of named sync groups.
///
/// A display belongs to **at most one** group at a time. The single source of
/// truth is a map from [`StableDisplayId`] to its [`Membership`]; a group is
/// simply the set of displays that name it, so groups are created implicitly by
/// [`add`](Self::add) and vanish when their last member leaves. This makes the
/// "at most one group" invariant impossible to violate.
#[derive(Debug, Clone, Default)]
pub struct SyncGroups {
    members: BTreeMap<StableDisplayId, Membership>,
}

impl SyncGroups {
    /// Create an empty set of sync groups.
    #[must_use]
    pub fn new() -> Self {
        SyncGroups {
            members: BTreeMap::new(),
        }
    }

    /// Whether no display is assigned to any group.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Assign `id` to `group` with `offset` percentage points.
    ///
    /// If `id` already belonged to another group it is moved: a display is only
    /// ever in one group, so the previous membership is replaced.
    pub fn add(&mut self, group: &str, id: StableDisplayId, offset: i8) {
        let _ = (group, id, offset);
    }

    /// Remove `id` from whatever group it is in. Returns `true` if it was a
    /// member of some group.
    pub fn remove(&mut self, id: &StableDisplayId) -> bool {
        let _ = id;
        false
    }

    /// Update the offset for `id`, if it belongs to a group. Returns `true` if
    /// the display was found and updated, `false` if it is not in any group.
    pub fn set_offset(&mut self, id: &StableDisplayId, offset: i8) -> bool {
        let _ = (id, offset);
        false
    }

    /// The offset of `id`, if it belongs to a group.
    #[must_use]
    pub fn offset_of(&self, id: &StableDisplayId) -> Option<i8> {
        let _ = id;
        None
    }

    /// The name of the group `id` belongs to, if any.
    #[must_use]
    pub fn group_of(&self, id: &StableDisplayId) -> Option<&str> {
        let _ = id;
        None
    }

    /// The members of `group` with their offsets, sorted by display id.
    #[must_use]
    pub fn members(&self, group: &str) -> Vec<(StableDisplayId, i8)> {
        let _ = group;
        Vec::new()
    }

    /// The set of distinct group names currently in use, sorted.
    #[must_use]
    pub fn group_names(&self) -> BTreeSet<&str> {
        BTreeSet::new()
    }

    /// Iterate every `(display, group, offset)` triple, ordered by display id.
    #[must_use]
    pub fn iter(&self) -> Iter<'_> {
        Iter {
            inner: self.members.iter(),
        }
    }

    /// Fan a `master_pct` value out to every member of `group`.
    ///
    /// Each member's target is `clamp(master_pct + offset, 0, 100)`. Because the
    /// result depends only on the master and the offset (never on the member's
    /// previous value), repeated calls with the same master are idempotent — no
    /// drift accumulates at the clamp bounds. Returns `(id, target)` pairs sorted
    /// by display id; an unknown group yields an empty vector.
    #[must_use]
    pub fn fan_out(&self, group: &str, master_pct: u8) -> Vec<(StableDisplayId, u8)> {
        let _ = (group, master_pct);
        Vec::new()
    }
}

/// Iterator over every sync-group member as a `(display, group, offset)`
/// triple, ordered by display id. Created by [`SyncGroups::iter`].
#[derive(Debug, Clone)]
pub struct Iter<'a> {
    inner: std::collections::btree_map::Iter<'a, StableDisplayId, Membership>,
}

impl<'a> Iterator for Iter<'a> {
    type Item = (&'a StableDisplayId, &'a str, i8);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner
            .next()
            .map(|(id, m)| (id, m.group.as_str(), m.offset))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl ExactSizeIterator for Iter<'_> {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

impl std::iter::FusedIterator for Iter<'_> {}

impl<'a> IntoIterator for &'a SyncGroups {
    type Item = (&'a StableDisplayId, &'a str, i8);
    type IntoIter = Iter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::StableDisplayId;
    use proptest::prelude::*;

    /// Build a valid EDID with manufacturer `AAA`, product code `0`, and the
    /// given non-zero numeric serial, yielding the stable id `AAA-0000-s<n>`.
    fn id_with_serial(serial: u32) -> StableDisplayId {
        let mut e = vec![0x00u8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
        e.push(0x04); // bytes 8..=9 encode "AAA"
        e.push(0x21);
        e.push(0x00); // product code (little-endian) = 0
        e.push(0x00);
        e.extend_from_slice(&serial.to_le_bytes()); // bytes 12..=15: numeric serial
        e.resize(127, 0x00);
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg());
        StableDisplayId::from_edid(&e).unwrap()
    }

    /// The first three single-digit-serial ids, which sort as `s1 < s2 < s3`.
    fn a() -> StableDisplayId {
        id_with_serial(1)
    }
    fn b() -> StableDisplayId {
        id_with_serial(2)
    }
    fn c() -> StableDisplayId {
        id_with_serial(3)
    }

    #[test]
    fn new_is_empty() {
        let g = SyncGroups::new();
        assert!(g.is_empty());
        assert!(g.group_names().is_empty());
        assert_eq!(g.group_of(&a()), None);
    }

    #[test]
    fn add_records_group_and_offset() {
        let mut g = SyncGroups::new();
        g.add("gaming", a(), 0);
        g.add("gaming", b(), -10);
        assert!(!g.is_empty());
        assert_eq!(g.group_of(&a()), Some("gaming"));
        assert_eq!(g.offset_of(&a()), Some(0));
        assert_eq!(g.group_of(&b()), Some("gaming"));
        assert_eq!(g.offset_of(&b()), Some(-10));
    }

    #[test]
    fn linked_monitors_move_with_offsets() {
        let mut g = SyncGroups::new();
        g.add("desk", a(), 0);
        g.add("desk", b(), -10);
        // Master 80: a stays at 80, b trails by 10 -> 70. Sorted by id (a < b).
        assert_eq!(g.fan_out("desk", 80), vec![(a(), 80), (b(), 70)]);
        // A different master moves the whole group together.
        assert_eq!(g.fan_out("desk", 50), vec![(a(), 50), (b(), 40)]);
    }

    #[test]
    fn offset_clamps_at_bounds_without_drift() {
        let mut g = SyncGroups::new();
        g.add("g", a(), 5);
        // 98 + 5 = 103 -> clamped to 100.
        assert_eq!(g.fan_out("g", 98), vec![(a(), 100)]);
        // Fanning out the same master again must NOT drift past the bound.
        assert_eq!(g.fan_out("g", 98), vec![(a(), 100)]);
        // Dropping the master gives the exact offset value, no residue.
        assert_eq!(g.fan_out("g", 50), vec![(a(), 55)]);
    }

    #[test]
    fn offset_clamps_at_low_bound() {
        let mut g = SyncGroups::new();
        g.add("g", a(), -40);
        // 10 - 40 = -30 -> clamped to 0.
        assert_eq!(g.fan_out("g", 10), vec![(a(), 0)]);
        assert_eq!(g.fan_out("g", 10), vec![(a(), 0)]);
        // Above the offset magnitude the value is exact again.
        assert_eq!(g.fan_out("g", 70), vec![(a(), 30)]);
    }

    #[test]
    fn moving_display_removes_it_from_the_old_group() {
        let mut g = SyncGroups::new();
        g.add("left", a(), 3);
        g.add("right", a(), 7);
        // a is only ever in one group; the second add moved it.
        assert_eq!(g.group_of(&a()), Some("right"));
        assert_eq!(g.offset_of(&a()), Some(7));
        assert!(g.members("left").is_empty());
        assert_eq!(g.members("right"), vec![(a(), 7)]);
        // The abandoned group name is gone entirely.
        assert!(!g.group_names().contains("left"));
    }

    #[test]
    fn remove_reports_presence_and_clears_membership() {
        let mut g = SyncGroups::new();
        g.add("grp", a(), 0);
        assert!(g.remove(&a()));
        assert_eq!(g.group_of(&a()), None);
        assert!(g.is_empty());
        // Removing again reports absence.
        assert!(!g.remove(&a()));
    }

    #[test]
    fn set_offset_updates_member_and_reports_unknown() {
        let mut g = SyncGroups::new();
        g.add("grp", a(), 0);
        assert!(g.set_offset(&a(), -15));
        assert_eq!(g.offset_of(&a()), Some(-15));
        // An id that is not in any group cannot have its offset set.
        assert!(!g.set_offset(&b(), 5));
    }

    #[test]
    fn fan_out_unknown_group_is_empty() {
        let mut g = SyncGroups::new();
        g.add("grp", a(), 0);
        assert!(g.fan_out("nope", 50).is_empty());
    }

    #[test]
    fn fan_out_members_are_sorted_by_id() {
        let mut g = SyncGroups::new();
        // Insert out of id order; output must still be sorted a < b < c.
        g.add("grp", c(), 0);
        g.add("grp", a(), 0);
        g.add("grp", b(), 0);
        let ids: Vec<StableDisplayId> =
            g.fan_out("grp", 60).into_iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec![a(), b(), c()]);
    }

    #[test]
    fn members_are_scoped_to_their_group() {
        let mut g = SyncGroups::new();
        g.add("one", a(), 1);
        g.add("two", b(), 2);
        assert_eq!(g.members("one"), vec![(a(), 1)]);
        assert_eq!(g.members("two"), vec![(b(), 2)]);
    }

    #[test]
    fn group_names_are_distinct_and_sorted() {
        let mut g = SyncGroups::new();
        g.add("beta", a(), 0);
        g.add("alpha", b(), 0);
        g.add("beta", c(), 0);
        let names: Vec<&str> = g.group_names().into_iter().collect();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn iter_yields_every_member_in_id_order() {
        let mut g = SyncGroups::new();
        g.add("y", b(), -5);
        g.add("x", a(), 5);
        let seen: Vec<(StableDisplayId, String, i8)> = g
            .iter()
            .map(|(id, grp, off)| (id.clone(), grp.to_owned(), off))
            .collect();
        assert_eq!(
            seen,
            vec![(a(), "x".to_owned(), 5), (b(), "y".to_owned(), -5),]
        );
    }

    #[test]
    fn master_above_100_is_clamped_before_offset() {
        let mut g = SyncGroups::new();
        g.add("g", a(), -5);
        // Out-of-contract master is treated as 100; 100 - 5 = 95.
        assert_eq!(g.fan_out("g", 200), vec![(a(), 95)]);
    }

    // --- property tests (plan §4.1: sync groups) ---

    /// A pool of ids drawn from small serials so groups actually collide.
    fn pool_id(n: u32) -> StableDisplayId {
        id_with_serial(n.wrapping_rem(6).wrapping_add(1))
    }

    fn add_ops() -> impl Strategy<Value = Vec<(u32, u8, i8)>> {
        // (serial selector, group selector 0..3, offset)
        prop::collection::vec((0u32..12, 0u8..3, any::<i8>()), 0..16)
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2_000))]

        /// Every fanned-out value is a valid percentage, and fanning out the
        /// same master twice yields identical results (idempotent, no drift).
        #[test]
        fn fan_out_output_always_in_range(ops in add_ops(), master in any::<u8>()) {
            let mut g = SyncGroups::new();
            let group_names = ["a", "b", "c"];
            for (serial, gsel, offset) in ops {
                let name = group_names.get(usize::from(gsel)).copied().unwrap_or("a");
                g.add(name, pool_id(serial), offset);
            }
            for name in group_names {
                let first = g.fan_out(name, master);
                for &(_, pct) in &first {
                    prop_assert!(pct <= 100, "{pct} out of range in {name}");
                }
                // Idempotence / no drift: a second identical fan-out matches.
                prop_assert_eq!(&g.fan_out(name, master), &first);
            }
        }

        /// After any sequence of adds, each display is in at most one group, and
        /// `group_of` agrees with `members`.
        #[test]
        fn each_display_in_at_most_one_group(ops in add_ops()) {
            let mut g = SyncGroups::new();
            let group_names = ["a", "b", "c"];
            for (serial, gsel, offset) in ops {
                let name = group_names.get(usize::from(gsel)).copied().unwrap_or("a");
                g.add(name, pool_id(serial), offset);
            }
            for (id, grp, _) in g.iter() {
                // The id resolves to exactly the group iter reported...
                prop_assert_eq!(g.group_of(id), Some(grp));
                // ...and appears in no other group's member list.
                let mut hits = 0u32;
                for other in group_names {
                    if g.members(other).iter().any(|(mid, _)| mid == id) {
                        hits = hits.wrapping_add(1);
                    }
                }
                prop_assert_eq!(hits, 1);
            }
        }
    }
}
