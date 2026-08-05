//! The decisions a `zwlr_gamma_control_v1` ramp makes that are arithmetic rather
//! than Wayland.
//!
//! The sixth pure Linux module, and the same split as [`crate::linux_caps`],
//! [`crate::linux_outputs`], [`crate::linux_overlay`], [`crate::linux_gamma`] and
//! [`crate::linux_layer`]: everything decidable without a compositor lives here
//! and is tested on all three CI lanes, and the Linux-only `linux::wlr_gamma`
//! module does nothing but carry the answers to the wire.
//!
//! # It is a sibling of [`crate::linux_gamma`], not a replacement
//!
//! The two transports have the same *curve* and nothing else in common, so the
//! curve is shared — [`crate::linux_gamma::ramp`] builds it for both — and every
//! other decision is duplicated deliberately rather than generalised. `#130`'s
//! successor commit had to take X11's request-length ceiling back out of that
//! shared builder for exactly this reason: it was a fact about `SetCrtcGamma`'s
//! wire encoding wearing the name of a fact about gamma tables, and it would have
//! refused a legal Wayland ramp for an X11 reason.
//!
//! | | X11 (`crate::linux_gamma`) | Wayland (here) |
//! |---|---|---|
//! | transport | a core request, capped by `maximum_request_length` | a **file descriptor**, capped by nothing |
//! | length | `GetCrtcGammaSize`, synchronous | a `gamma_size` **event**, so a round trip |
//! | address | a `RandR` CRTC id | a `wl_output`, by connector name |
//! | who holds the table | the X server, with no owner | the compositor, for as long as the client's object lives |
//! | a wrong length | one rejected request | **the client connection is killed** |
//! | restoring | write the identity table | destroy the object |
//!
//! # A wrong table length is fatal, and that is why this module exists
//!
//! wlroots' `types/wlr_gamma_control_v1.c` answers `set_gamma` by computing
//! `ramp_size * 3 * sizeof(uint16_t)` and reading exactly that many bytes from the
//! descriptor (`pread` at offset 0 since wlroots 0.17; a plain `read` before it,
//! which is why `linux::wlr_gamma`'s `table_file` rewinds). A short read is not a
//! `failed` event and not a degraded dim: it is
//! `wl_resource_post_error(..., ZWLR_GAMMA_CONTROL_V1_ERROR_INVALID_GAMMA, ...)`,
//! which **terminates the whole client connection**. So the length is not a detail
//! that fails locally — getting it wrong takes down every other Wayland object the
//! process holds with it.
//!
//! Three separate ways to get the **length** wrong, and each of them is a decision
//! here: [`ramp_size`] (a length that does not fit the builder), [`table_bytes`]
//! (the byte count) and [`gamma_table`] (the bytes themselves). With
//! [`wlr_gamma_refusal`], which asks whether this session has the channel at all,
//! that is the four this module exports.
//!
//! # The protocol's own prose says "16-byte", and it is wrong
//!
//! `wlr-gamma-control-unstable-v1.xml` describes each ramp as *"an array of
//! 16-byte unsigned integers"*. The implementation computes
//! `sizeof(uint16_t)` — **two** bytes — and the sentence beneath it, *"the file
//! descriptor data must have the same length as three times the gamma size"*, is
//! itself incomplete in the other direction (it omits the entry width entirely).
//! The C is the authority; [`ENTRY_BYTES`] is where that is written down, and a
//! test pins it.
//!
//! # Nothing here can leave a ramp behind
//!
//! The property that decides everything above this module, and the one place
//! Linux's two transports land on opposite sides. An `XRandR` ramp is server state
//! with no owner and survives the client that wrote it — which is why
//! `xrandr --gamma` works as a one-shot command, and why an **X11 session** needs
//! the crash marker Windows carries. A `zwlr_gamma_control_v1` ramp is the opposite: the
//! protocol says destroying the object *"restores the original gamma tables"* — by
//! which it means the output's default, not an earlier client's curve, since the
//! compositor keeps no such thing — and the compositor destroys every object a
//! client holds when its socket closes. So the guarantee survives `SIGKILL`, a
//! panic, and a power-managed session teardown alike, and a Wayland session has
//! **nothing for a rescue pass to find**.
//!
//! That is also why there is no `identity_table` next to [`gamma_table`]. Writing
//! the identity table would be the X11 restore, and on this transport it is the
//! wrong shape: destroying the control gets to the same screen (the compositor
//! then applies no transform at all) *and* releases the output, which an identity
//! write cannot do. Since the protocol grants one client exclusive access per
//! output, letting go is the whole difference — a table that says "no dimming" is
//! still a client holding the output.

use crate::linux_caps::Transport;
use crate::linux_gamma::{MIN_RAMP_SIZE, ramp};

/// How many ramps one gamma table carries: red, green and blue, in that order.
///
/// The order is `r`, `g`, `b` at offsets `0`, `size` and `2 * size` —
/// `wlr_gamma_control_v1_get_color_transform` slices the table exactly that way.
/// Duja's dim is neutral, so all three ramps are identical and a wrong order
/// would be **invisible today**; it stops being invisible the day this composes a
/// dim into a baseline curve rather than replacing it, which `linux::gamma`'s
/// module docs already name as owed. Pinned now, while the answer is known.
pub const CHANNELS: usize = 3;

/// The width of one gamma-table entry, in bytes.
///
/// Two, from `sizeof(uint16_t)` in the implementation — **not** the sixteen the
/// protocol's own prose claims (see the module docs). A table built to the XML's
/// wording would be eight times too long, and that is the direction that does
/// **not** fail loudly: the compositor asks for `table_size` bytes from a file
/// that has more than that, gets exactly what it asked for, and programs a ramp
/// made of the first eighth of the buffer — interleaved low bytes. Only a table
/// that is too *short* trips `INVALID_GAMMA`.
pub const ENTRY_BYTES: usize = size_of::<u16>();

/// The ramp length to build for a compositor that advertised `advertised`
/// entries, or `None` for one this crate cannot serve.
///
/// `gamma_size` arrives as a protocol `uint`, and every table this crate builds is
/// indexed by a `u16` ([`crate::linux_gamma::ramp`]). The narrowing is therefore a
/// decision and not a cast, and it is the one that has to be got right first:
/// truncating `65_536` to `0` — or `65_537` to `1` — produces a table of the wrong
/// length, and a wrong length here is not a refused request but a **killed
/// connection** (module docs). `u16::try_from` refuses instead.
///
/// The floor is [`MIN_RAMP_SIZE`] and it is the transport-independent half: a
/// one-entry table has no input axis and a zero-entry one is an output with no
/// gamma hardware. wlroots never sends the latter — `get_gamma_control` answers a
/// zero `gamma_size` with `failed` and no `gamma_size` event at all — so that arm
/// is a belt for a compositor that is not wlroots rather than a live case.
///
/// There is deliberately **no upper bound below `u16::MAX`**. The X11 sibling has
/// one ([`crate::linux_gamma::MAX_RAMP_SIZE`]) because a `SetCrtcGamma` request
/// body must fit `maximum_request_length`; a Wayland table travels over a file
/// descriptor and is bounded by nothing, so the only ceiling left is the width of
/// the builder itself. At that ceiling the table is `65_535 * 3 * 2` = 393 KB,
/// which is an allocation rather than a problem. Real outputs report 256, 1024 or
/// 4096.
#[must_use]
pub fn ramp_size(advertised: u32) -> Option<u16> {
    let size = u16::try_from(advertised).ok()?;
    if size < MIN_RAMP_SIZE {
        return None;
    }
    Some(size)
}

/// How many bytes a `size`-entry gamma table occupies on the wire, or `None` for
/// a size [`ramp_size`] would not have accepted.
///
/// `size * `[`CHANNELS`]` * `[`ENTRY_BYTES`], which is `ramp_size * 3 *
/// sizeof(uint16_t)` — the same expression the compositor computes, written the
/// same way round so the two can be compared by eye. It is the number that has to
/// match exactly: the compositor `pread`s precisely this many bytes and kills the
/// connection if it gets fewer.
#[must_use]
pub fn table_bytes(size: u16) -> Option<usize> {
    if size < MIN_RAMP_SIZE {
        return None;
    }
    // Checked rather than `saturating`: a saturated length is a *wrong* length,
    // and a wrong length is the failure this whole module is shaped around. On
    // every target this crate builds for — 32- and 64-bit desktop — the product is
    // at most 393_210 and cannot overflow, so this cannot currently fire. It is
    // `checked` rather than `saturating` to say what the right answer would be if
    // it ever could: `None`, which every caller already reads as "send no table",
    // rather than a truncated length that would be sent as if it were right.
    usize::from(size)
        .checked_mul(CHANNELS)
        .and_then(|entries| entries.checked_mul(ENTRY_BYTES))
}

/// The gamma table that scales output brightness by `factor`, ready to be written
/// to the file descriptor `set_gamma` carries.
///
/// The curve is [`crate::linux_gamma::ramp`]'s — the same one the X11 channel
/// writes, clamped to [`GAMMA_FLOOR`](duja_core::dimmer::GAMMA_FLOOR) — repeated
/// once per channel, because the dim is neutral by construction.
///
/// # Entries are **native**-endian, and that is the one silent failure here
///
/// The compositor `pread`s the bytes straight into a `uint16_t *` and indexes it;
/// there is no byte-order conversion anywhere on that path, and there is no
/// protocol byte order to convert to, because a Wayland client and its compositor
/// are the same machine by construction — the transport is a Unix socket.
///
/// This is the one decision in the module that fails **quietly**. Every other way
/// of getting the table wrong changes its length, and a wrong length kills the
/// connection loudly. A byte-swapped table is exactly the right length, so it is
/// accepted, programmed, and shows up only as a screen whose brightness curve is
/// nonsense. `an_entry_is_written_in_the_compositors_own_byte_order` is what holds
/// it.
#[must_use]
pub fn gamma_table(factor: f32, size: u16) -> Option<Vec<u8>> {
    let channel = ramp(factor, size)?;
    let mut table = Vec::with_capacity(table_bytes(size)?);
    for _ in 0..CHANNELS {
        for entry in &channel {
            table.extend_from_slice(&entry.to_ne_bytes());
        }
    }
    Some(table)
}

/// Why this session has no `wlr-gamma-control` channel, or `None` when it may
/// have one.
///
/// The cheap half of the transport gate, and the mirror image of
/// [`crate::linux_gamma::xrandr_refusal`]. Neither of them is the whole answer:
/// this one says whether it is worth opening a connection at all, and the
/// registry says whether the compositor offers the protocol
/// ([`crate::linux_caps::GAMMA_CONTROL`]), and the per-output `failed` event says
/// whether Duja may actually have it ([`crate::linux_caps::SurfaceCaps::refuse_gamma`]).
///
/// The two refusals are **not** symmetric in what they prevent, and the
/// asymmetry is the reason this one is short. `xrandr_refusal` guards against a
/// write that *succeeds and does nothing* — an `XRandR` ramp on a Wayland session
/// lands on an Xwayland virtual CRTC, returns `Success`, and changes no screen.
/// There is no such trap in this direction: an X11 session has no
/// `WAYLAND_DISPLAY`, so the connection simply is not there to be made, and the
/// failure is immediate and loud. This gate saves a socket call; it is not what
/// makes the backend safe.
#[must_use]
pub const fn wlr_gamma_refusal(transport: Transport) -> Option<&'static str> {
    match transport {
        Transport::Wayland => None,
        Transport::X11 => Some(
            "this session is X11: zwlr_gamma_control_v1 is a Wayland protocol, and \
             the XRandR CRTC ramp is the gamma channel an X server has",
        ),
        Transport::None => Some("this session has no display server"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linux_gamma::{MAX_RAMP_SIZE, identity_ramp};

    /// Decode one entry of a table at entry index `at`.
    fn entry(table: &[u8], at: usize) -> u16 {
        let start = at.saturating_mul(ENTRY_BYTES);
        let bytes = table
            .get(start..start.saturating_add(ENTRY_BYTES))
            .expect("the entry is inside the table");
        u16::from_ne_bytes(bytes.try_into().expect("an entry is two bytes"))
    }

    /// The narrowing that kills the connection if it is a cast. `gamma_size` is a
    /// protocol `uint`, so `65_536` is a value the wire can carry, and `as u16`
    /// turns it into `0` — a zero-length table, a `pread` that returns nothing of
    /// what the compositor asked for, and `INVALID_GAMMA` on the whole client.
    #[test]
    fn a_size_that_does_not_fit_the_builder_is_refused_rather_than_truncated() {
        assert_eq!(ramp_size(u32::from(u16::MAX)), Some(u16::MAX));
        assert_eq!(
            ramp_size(u32::from(u16::MAX) + 1),
            None,
            "65_536 must not become a zero-entry table"
        );
        assert_eq!(
            ramp_size(u32::from(u16::MAX) + 2),
            None,
            "65_537 must not become a one-entry table"
        );
        assert_eq!(ramp_size(u32::MAX), None);
    }

    /// The floor is gamma's, not a transport's: an output with no gamma hardware
    /// and a table with no input axis are both nothing this could scale.
    #[test]
    fn a_size_with_no_ramp_in_it_is_refused() {
        assert_eq!(ramp_size(0), None, "an output with no gamma table");
        assert_eq!(ramp_size(1), None, "a table with no input axis");
        assert_eq!(ramp_size(u32::from(MIN_RAMP_SIZE)), Some(MIN_RAMP_SIZE));
    }

    /// **The bound the X11 channel has and this one must not.** `SetCrtcGamma` is
    /// a core request capped by `maximum_request_length`; a `set_gamma` table
    /// travels over a file descriptor, which has no such ceiling. A compositor
    /// reporting more than `MAX_RAMP_SIZE` entries is asking for a table this
    /// crate must build, and refusing it would be refusing a Wayland ramp for an
    /// X11 reason.
    ///
    /// Reds a `ramp_size` that reached for `writable_ramp_size` because the name
    /// sounded right.
    #[test]
    fn the_x11_request_ceiling_does_not_apply_here() {
        let beyond = u32::from(MAX_RAMP_SIZE) + 1;
        let size = ramp_size(beyond).expect("an fd is not bounded by a request length");
        assert_eq!(u32::from(size), beyond);
        assert!(gamma_table(0.5, size).is_some());
        // And the X11 predicate still refuses it, which is the whole point of the
        // two channels having separate rules.
        assert!(!crate::linux_gamma::writable_ramp_size(size));
    }

    /// The expression the compositor computes, computed the same way.
    /// `ramp_size * 3 * sizeof(uint16_t)`.
    #[test]
    fn a_table_is_three_ramps_of_two_byte_entries() {
        assert_eq!(CHANNELS, 3);
        assert_eq!(
            ENTRY_BYTES, 2,
            "the XML says 16-byte; wlroots computes sizeof(uint16_t)"
        );
        assert_eq!(table_bytes(256), Some(256 * 3 * 2));
        assert_eq!(table_bytes(1024), Some(1024 * 3 * 2));
        assert_eq!(table_bytes(4096), Some(4096 * 3 * 2));
        assert_eq!(table_bytes(u16::MAX), Some(65_535 * 3 * 2));
        assert_eq!(table_bytes(0), None);
        assert_eq!(table_bytes(1), None);
    }

    /// The length the backend actually sends is the length the compositor
    /// `pread`s, for every size either of them can name. A mismatch is not a
    /// refused write: it is `INVALID_GAMMA` on the client connection, which takes
    /// the layer-shell overlay down with it.
    #[test]
    fn the_table_is_exactly_as_long_as_the_compositor_will_read() {
        for size in [MIN_RAMP_SIZE, 256, 1024, 4096, MAX_RAMP_SIZE, u16::MAX] {
            let table = gamma_table(0.5, size).expect("a size the builder accepts");
            assert_eq!(
                Some(table.len()),
                table_bytes(size),
                "size {size} is the wrong number of bytes"
            );
        }
    }

    /// Red, green and blue in that order, each `size` entries long, and each of
    /// them the curve the X11 channel writes.
    ///
    /// Duja's dim is neutral, so a swapped channel order is invisible **now** —
    /// which is exactly why it is pinned now, rather than the day this composes a
    /// dim into a baseline curve and the three stop being identical.
    #[test]
    fn a_table_is_the_shared_curve_once_per_channel() {
        let size = 256_u16;
        let curve = ramp(0.5, size).expect("256 is a legal size");
        let table = gamma_table(0.5, size).expect("256 is a legal size");
        for channel in 0..CHANNELS {
            for (index, &expected) in curve.iter().enumerate() {
                let at = channel
                    .saturating_mul(usize::from(size))
                    .saturating_add(index);
                assert_eq!(
                    entry(&table, at),
                    expected,
                    "channel {channel}, entry {index}"
                );
            }
        }
    }

    /// The one way to get this table wrong that the compositor will not catch.
    ///
    /// Every other mistake changes the length, and a wrong length kills the
    /// connection loudly. A byte-swapped table is exactly the right length, so it
    /// is read, programmed, and shows up only as a screen with a nonsense
    /// brightness curve. The compositor indexes a `uint16_t *` with no conversion
    /// anywhere, and a Wayland client shares a machine with its compositor by
    /// construction, so native order is the only correct answer.
    ///
    /// Reds `to_be_bytes` on every lane this project builds on, all of which are
    /// little-endian; a big-endian target would make this test tautological, and
    /// there is none.
    #[test]
    fn an_entry_is_written_in_the_compositors_own_byte_order() {
        // The sample has to be an entry whose two bytes differ, or the assertion
        // below passes under either order. Most of a 256-entry table's entries
        // are useless for that: the identity is `i * 257`, i.e. `0x0101 * i`, so
        // every identity entry is a byte palindrome and so is every *even*
        // entry of a table halved from it. Entry 201 is not — `51_657 * 0.5`
        // rounds to `25_829`, which is `0x64E5`.
        let table = gamma_table(0.5, 256).expect("256 is a legal size");
        let sample = entry(&table, 201);
        assert_ne!(
            sample,
            sample.swap_bytes(),
            "the sample entry cannot tell the two orders apart"
        );
        assert_eq!(
            sample,
            ramp(0.5, 256)
                .expect("256 is a legal size")
                .get(201)
                .copied()
                .expect("a 256-entry ramp has an entry 201"),
            "the entry must decode natively, not byte-swapped"
        );
    }

    /// A factor of 1.0 reaches the wire as the identity curve, so "no dimming" is
    /// the table that changes nothing rather than one that merely looks like it.
    #[test]
    fn an_undimmed_table_is_the_identity_curve() {
        let size = 1024_u16;
        let table = gamma_table(1.0, size).expect("1024 is a legal size");
        let identity = identity_ramp(size).expect("1024 is a legal size");
        for (index, &expected) in identity.iter().enumerate() {
            assert_eq!(entry(&table, index), expected, "entry {index}");
        }
        assert_eq!(Some(table.len()), table_bytes(size));
    }

    /// Every size [`ramp_size`] accepts is one the other two can serve.
    ///
    /// One direction only, deliberately. The converse — every size it refuses, the
    /// others refuse too — is unstatable above `u16::MAX`, because the other two
    /// take a `u16` and cannot be handed the value that was rejected; below it,
    /// `a_table_is_three_ramps_of_two_byte_entries` and
    /// `a_refused_size_produces_no_table` already pin both floors directly.
    ///
    /// The floor lives in three places — `ramp_size`, `table_bytes` and `ramp`
    /// underneath `gamma_table` — and nothing else ties them together. If they
    /// ever disagreed the backend would take an output's gamma control, learn its
    /// size, and then fail to build a table for it: an exclusive claim held for a
    /// write that cannot happen. Reds any edit that moves one floor and not the
    /// others.
    #[test]
    fn a_size_the_narrowing_accepts_is_one_the_builders_can_serve() {
        for advertised in [
            0_u32,
            1,
            2,
            255,
            256,
            1024,
            4096,
            u32::from(MAX_RAMP_SIZE),
            u32::from(MAX_RAMP_SIZE) + 1,
            u32::from(u16::MAX),
            u32::from(u16::MAX) + 1,
            u32::MAX,
        ] {
            let Some(size) = ramp_size(advertised) else {
                continue;
            };
            let bytes = table_bytes(size)
                .unwrap_or_else(|| panic!("{advertised} was accepted but has no byte count"));
            let table = gamma_table(0.5, size)
                .unwrap_or_else(|| panic!("{advertised} was accepted but builds no table"));
            assert_eq!(
                table.len(),
                bytes,
                "size {size} disagrees with its own length"
            );
        }
    }

    /// A size the builder refuses produces no table at all, rather than a short
    /// one — the length is the thing that must never be wrong.
    #[test]
    fn a_refused_size_produces_no_table() {
        assert_eq!(gamma_table(0.5, 0), None);
        assert_eq!(gamma_table(0.5, 1), None);
        assert!(gamma_table(0.5, MIN_RAMP_SIZE).is_some());
    }

    /// Only a Wayland session has this channel, and the reason names the other
    /// one so a user reading `dujactl doctor` is not left thinking Duja has no
    /// gamma path at all on X11.
    #[test]
    fn only_a_wayland_session_has_a_wlr_gamma_channel() {
        assert_eq!(wlr_gamma_refusal(Transport::Wayland), None);

        let x11 = wlr_gamma_refusal(Transport::X11).expect("X11 has no Wayland protocol");
        assert!(x11.contains("XRandR"), "the X11 reason must name it: {x11}");

        assert!(wlr_gamma_refusal(Transport::None).is_some());
    }

    /// The two channels are exclusive and between them they cover every session
    /// with a display server. Reds a pair of gates that both refuse — which would
    /// be a Linux desktop with no gamma path at all and no reason given — and a
    /// pair that both accept, which would be two backends writing one screen.
    #[test]
    fn every_session_has_at_most_one_gamma_channel_and_a_desktop_has_one() {
        for transport in [Transport::X11, Transport::Wayland, Transport::None] {
            let x11 = crate::linux_gamma::xrandr_refusal(transport).is_none();
            let wlr = wlr_gamma_refusal(transport).is_none();
            assert!(
                !(x11 && wlr),
                "{transport:?} claims both gamma channels at once"
            );
            assert_eq!(
                x11 || wlr,
                transport != Transport::None,
                "{transport:?} disagrees about whether it has a gamma channel"
            );
        }
    }
}
