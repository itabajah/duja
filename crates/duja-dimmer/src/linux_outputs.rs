//! Giving a Linux display the rectangle it sits on.
//!
//! Sysfs answers "which monitors exist and how do I talk to them" and never
//! "where are they": the DRM connector tree knows nothing about a desktop, and a
//! Linux session may have no display server at all. So `duja-ddc`'s Linux
//! `DdcDisplay` and `duja-panel`'s Linux panel both report **no** geometry, and
//! until something supplies one the planner correctly plans no overlay and the
//! continuum stops at the hardware floor.
//!
//! This module is the join that supplies it. The display server enumerates its
//! outputs; sysfs enumerated the connectors; the two lists are matched, and a
//! connector that matches an output acquires that output's bounds.
//!
//! # The join key, and why it needs a fallback
//!
//! [`duja_core::linux::drm`] carries the DRM connector name with its `card<N>-`
//! prefix stripped, which is the form the display server uses: `DP-1`, `eDP-1`,
//! `HDMI-A-2`. On the modern stack — the modesetting DDX, and DRM-backed Wayland
//! compositors — those strings are equal on both sides and a name match is the
//! whole answer.
//!
//! They are reported not to be equal on the NVIDIA proprietary X11 driver, which
//! indexes outputs its own way (`DP-0`, `HDMI-0`), nor on the legacy
//! `xf86-video-intel` DDX, which omits the hyphen before the index (`eDP1`,
//! `DP1`) and so defeats a string-equality join outright. Wave 2 recorded that it
//! was carrying the name as the best key available rather than a guarantee, and
//! that this wave owed it a fallback. This is the fallback: **the EDID**, which
//! both sides can read and neither invents.
//!
//! # Ambiguity refuses; it does not guess
//!
//! Two identical monitors with no serial number in their EDID are byte-identical
//! to both sides. A join that picked one anyway would place an overlay on the
//! wrong screen, which is a silent wrong answer rather than a visible missing
//! one. So a match is taken only when it is **unique among the outputs not
//! already claimed**, and an ambiguous connector stays unplaced — hardware
//! control intact, software dimming off, which is exactly the state Linux was
//! already in before this module existed.
//!
//! Claiming is one-to-one for the same reason: an output that one connector
//! matched by name is out of the pool before the EDID pass runs, so a second
//! connector cannot be given the same rectangle. That also makes the mixed case
//! work — one monitor named consistently and one renamed by the driver resolve to
//! one placement each, by different evidence.
//!
//! # It names no display-server type
//!
//! Same constraint as [`crate::linux_caps`], for the same reason: this module is
//! compiled and tested on all three CI lanes, where `x11rb` and `wayland-client`
//! do not exist. Outputs arrive as plain data — a name, an optional EDID, a
//! rectangle and an addressing token — and the modules that talk to a display
//! server build them.

use duja_core::dimmer::DisplayBounds;

/// The length of an EDID base block, and the only part of an EDID this module
/// compares.
///
/// Extension blocks are deliberately excluded. Both sides read the same bytes
/// from the same monitor, but they do not always read the same *number* of them:
/// sysfs publishes the whole blob, while an X11 driver may publish only the base
/// block in its `EDID` output property. Comparing the base block alone therefore
/// matches strictly more often, and gives up no discrimination that matters —
/// two monitors whose base blocks are identical are exactly the case this module
/// refuses to guess about anyway.
pub const EDID_BASE_BLOCK: usize = 128;

/// One output as the display server describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerOutput {
    /// The server's name for this output: the `RandR` output name on X11, the
    /// `wl_output` name on Wayland.
    pub name: String,
    /// The EDID the server could read, if it publishes one at all. X11 `RandR`
    /// exposes it as an output property; **Wayland has no protocol for it**, so
    /// a Wayland output is always `None` here and joins by name or not at all.
    pub edid: Option<Vec<u8>>,
    /// Where this output sits in the display server's coordinate space.
    pub bounds: DisplayBounds,
    /// The token that both addresses this output's gamma ramp and names the
    /// framebuffer it shares with any mirror.
    ///
    /// One string does both jobs on Linux, as on Windows and unlike macOS, and
    /// for a reason rather than by coincidence: on X11 it is the **CRTC** id, and
    /// two outputs driven by one CRTC show the same pixels *and* share one gamma
    /// table, so the mirror-group key and the gamma address are the same thing.
    /// On Wayland it is the output name, and there is no mirroring for the two to
    /// disagree about.
    pub token: String,
}

/// A connector as sysfs described it: the two things the join can key on.
///
/// Borrowed so the caller can pass what it already has without cloning an EDID
/// per display.
#[derive(Debug, Clone, Copy)]
pub struct Connector<'a> {
    /// The DRM connector name, `card<N>-` prefix already stripped.
    pub name: &'a str,
    /// The raw EDID bytes read from the connector.
    pub edid: &'a [u8],
}

/// What evidence placed a connector.
///
/// Carried rather than discarded because the two mean different things to a
/// person reading a log: a name match is the modern stack working as designed,
/// and an EDID match is the driver naming outputs its own way — which is a real
/// configuration, not a fault, but the one worth knowing about when a display
/// does not appear where it should.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matched {
    /// The connector name and the output name were equal.
    ByName,
    /// The names differed and the EDID base blocks were equal.
    ByEdid,
}

/// Where one connector was placed, and on what evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The output's rectangle.
    pub bounds: DisplayBounds,
    /// The output's addressing token (see [`ServerOutput::token`]).
    pub token: String,
    /// What matched.
    pub by: Matched,
}

/// Match every connector to at most one output.
///
/// Returns one entry per connector, in the same order, so callers zip rather
/// than look up. `None` means this connector has no rectangle: no output matched
/// it, or more than one did.
///
/// Outputs with **empty bounds are dropped before matching**. A `RandR` output
/// with no CRTC, or a `wl_output` the compositor has disabled, is a monitor the
/// desktop is not currently drawing on; it has no area to cover, and matching a
/// connector to it would hand the planner a zero-area rectangle to place an
/// overlay in. Unplaced is the honest answer and the one the rest of the pipeline
/// already handles.
///
/// Name matches are taken first, across all connectors, before any EDID match is
/// considered. That ordering is what makes the mixed case resolve: on a machine
/// where the driver renames only some outputs, the ones it names consistently
/// claim their outputs first and leave a smaller, less ambiguous pool for the
/// rest.
#[must_use]
pub fn join(connectors: &[Connector<'_>], outputs: &[ServerOutput]) -> Vec<Option<Placement>> {
    let mut pool: Vec<Option<&ServerOutput>> = outputs
        .iter()
        .filter(|output| !output.bounds.is_empty())
        .map(Some)
        .collect();

    let mut placements: Vec<Option<Placement>> = connectors
        .iter()
        .map(|connector| {
            claim_unique(&mut pool, |output| output.name == connector.name)
                .map(|output| place(output, Matched::ByName))
        })
        .collect();

    for (slot, connector) in placements.iter_mut().zip(connectors) {
        if slot.is_some() {
            continue;
        }
        let Some(wanted) = base_block(connector.edid) else {
            continue;
        };
        *slot = claim_unique(&mut pool, |output| {
            output.edid.as_deref().and_then(base_block) == Some(wanted)
        })
        .map(|output| place(output, Matched::ByEdid));
    }

    placements
}

/// The first [`EDID_BASE_BLOCK`] bytes, or `None` if there are not that many.
///
/// A short EDID is not a truncated match, it is not a base block at all, and
/// comparing whatever prefix exists would let a corrupt read match a healthy
/// monitor.
fn base_block(edid: &[u8]) -> Option<&[u8]> {
    edid.get(..EDID_BASE_BLOCK)
}

/// Take the one unclaimed output satisfying `matches`, leaving the pool without
/// it.
///
/// `None` when nothing matches **and** when more than one thing does. Those are
/// different situations with the same correct answer: neither licenses a guess.
fn claim_unique<'a>(
    pool: &mut [Option<&'a ServerOutput>],
    mut matches: impl FnMut(&ServerOutput) -> bool,
) -> Option<&'a ServerOutput> {
    let mut found: Option<usize> = None;
    for (index, slot) in pool.iter().enumerate() {
        if slot.is_some_and(&mut matches) {
            if found.is_some() {
                return None;
            }
            found = Some(index);
        }
    }
    pool.get_mut(found?).and_then(Option::take)
}

/// Build a placement from a matched output.
fn place(output: &ServerOutput, by: Matched) -> Placement {
    Placement {
        bounds: output.bounds,
        token: output.token.clone(),
        by,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 128-byte EDID base block whose bytes are distinct per `seed`, so two
    /// different seeds are never accidentally equal.
    fn edid(seed: u8) -> Vec<u8> {
        let mut bytes = vec![0_u8; EDID_BASE_BLOCK];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = seed.wrapping_add(u8::try_from(index % 251).unwrap_or(0));
        }
        bytes
    }

    fn output(name: &str, token: &str, edid: Option<Vec<u8>>) -> ServerOutput {
        ServerOutput {
            name: name.to_owned(),
            edid,
            bounds: DisplayBounds::new(0, 0, 1920, 1080),
            token: token.to_owned(),
        }
    }

    fn at(name: &str, token: &str, x: i32, width: u32) -> ServerOutput {
        ServerOutput {
            name: name.to_owned(),
            edid: None,
            bounds: DisplayBounds::new(x, 0, width, 1080),
            token: token.to_owned(),
        }
    }

    fn only(placements: Vec<Option<Placement>>) -> Placement {
        assert_eq!(placements.len(), 1, "{placements:?}");
        placements
            .into_iter()
            .next()
            .flatten()
            .expect("the single connector should have been placed")
    }

    fn nth(placements: &[Option<Placement>], index: usize) -> Option<&Placement> {
        placements.get(index).and_then(Option::as_ref)
    }

    /// The modern stack: sysfs and the display server spell the connector the
    /// same way, and that is the whole join.
    #[test]
    fn equal_names_place_the_connector() {
        let sink = edid(1);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [at("DP-1", "crtc-63", 1920, 2560)];

        let placed = only(join(&connectors, &outputs));

        assert_eq!(placed.bounds, DisplayBounds::new(1920, 0, 2560, 1080));
        assert_eq!(placed.token, "crtc-63");
        assert_eq!(placed.by, Matched::ByName);
    }

    /// The fallback wave 2 said it was owed. The NVIDIA proprietary driver
    /// indexes outputs its own way, so `DP-1` in sysfs is `DP-0` to X11 and a
    /// string-equality join finds nothing. The EDID is the same bytes on both
    /// sides because it came off the same monitor.
    #[test]
    fn a_renamed_output_is_found_by_its_edid() {
        let panel = edid(7);
        let connectors = [Connector {
            name: "DP-1",
            edid: &panel,
        }];
        let outputs = [output("DP-0", "crtc-70", Some(panel.clone()))];

        let placed = only(join(&connectors, &outputs));

        assert_eq!(placed.by, Matched::ByEdid);
        assert_eq!(placed.token, "crtc-70");
    }

    /// Two identical monitors with no serial number are byte-identical to both
    /// sides. Placing one anyway would put an overlay on the wrong screen, which
    /// is worse than not placing it: the user sees a screen they did not ask to
    /// dim go dark and the one they did stay bright.
    #[test]
    fn two_identical_monitors_the_driver_renamed_are_both_refused() {
        let twin = edid(3);
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &twin,
            },
            Connector {
                name: "DP-2",
                edid: &twin,
            },
        ];
        let outputs = [
            output("DP-0", "crtc-1", Some(twin.clone())),
            output("DP-4", "crtc-2", Some(twin.clone())),
        ];

        let placements = join(&connectors, &outputs);

        assert!(nth(&placements, 0).is_none(), "{placements:?}");
        assert!(nth(&placements, 1).is_none(), "{placements:?}");
    }

    /// The mixed case, and the reason name matching runs to completion before
    /// any EDID matching starts. Two identical monitors, one of which the server
    /// names the same as sysfs does: it claims its output by name, which leaves
    /// exactly one candidate for the other, and an ambiguous pair becomes two
    /// unambiguous placements.
    #[test]
    fn a_name_match_disambiguates_its_identical_twin() {
        let twin = edid(9);
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &twin,
            },
            Connector {
                name: "DP-2",
                edid: &twin,
            },
        ];
        let outputs = [
            output("DP-2", "crtc-b", Some(twin.clone())),
            output("DP-9", "crtc-a", Some(twin.clone())),
        ];

        let placements = join(&connectors, &outputs);

        let first = nth(&placements, 0).expect("DP-1 should fall back to the EDID");
        assert_eq!(first.by, Matched::ByEdid);
        assert_eq!(first.token, "crtc-a");

        let second = nth(&placements, 1).expect("DP-2 matches by name");
        assert_eq!(second.by, Matched::ByName);
        assert_eq!(second.token, "crtc-b");
    }

    /// Claiming is one-to-one. Without it the EDID pass would hand a second
    /// connector the rectangle a name match already took, and two displays would
    /// share one overlay while a third screen stayed bright.
    #[test]
    fn an_output_claimed_by_name_is_not_offered_to_the_edid_pass() {
        let shared = edid(5);
        let connectors = [
            Connector {
                name: "HDMI-A-1",
                edid: &shared,
            },
            Connector {
                name: "DP-1",
                edid: &shared,
            },
        ];
        // Only one output, and the first connector matches it by name.
        let outputs = [output("HDMI-A-1", "crtc-0", Some(shared.clone()))];

        let placements = join(&connectors, &outputs);

        assert_eq!(nth(&placements, 0).map(|p| p.by), Some(Matched::ByName));
        assert!(nth(&placements, 1).is_none(), "{placements:?}");
    }

    /// A `RandR` output with no CRTC, or a `wl_output` the compositor disabled,
    /// covers no area. Matching it would hand the planner a zero-area rectangle
    /// to place an overlay in; unplaced is the state the pipeline already knows
    /// how to handle.
    #[test]
    fn an_output_with_no_area_places_nothing_even_on_an_exact_name() {
        let sink = edid(2);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [ServerOutput {
            name: "DP-1".to_owned(),
            edid: Some(sink.clone()),
            bounds: DisplayBounds::new(0, 0, 0, 0),
            token: "crtc-none".to_owned(),
        }];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// A disabled output must not consume the EDID match its enabled twin needs.
    /// Dropping it before the pool is built is what makes that work; filtering
    /// after the fact would have left it in as a claimant.
    #[test]
    fn a_disabled_output_does_not_make_its_twin_ambiguous() {
        let twin = edid(11);
        let connectors = [Connector {
            name: "DP-1",
            edid: &twin,
        }];
        let outputs = [
            ServerOutput {
                name: "DP-7".to_owned(),
                edid: Some(twin.clone()),
                bounds: DisplayBounds::new(0, 0, 0, 0),
                token: "crtc-off".to_owned(),
            },
            output("DP-8", "crtc-on", Some(twin.clone())),
        ];

        let placed = only(join(&connectors, &outputs));

        assert_eq!(placed.by, Matched::ByEdid);
        assert_eq!(placed.token, "crtc-on");
    }

    /// Wayland publishes no EDID at all — there is no protocol for it — so a
    /// Wayland output joins by name or not at all. A compositor that renames
    /// outputs leaves the display hardware-only, which is the honest answer and
    /// not something an EDID pass can rescue.
    #[test]
    fn an_output_with_no_edid_cannot_be_matched_by_one() {
        let sink = edid(4);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [at("DP-99", "DP-99", 0, 1920)];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// A connector sysfs could not read a full base block for has nothing to
    /// fall back to. Comparing whatever prefix exists would let a corrupt read
    /// match a healthy monitor.
    #[test]
    fn a_short_edid_never_matches() {
        let stub = vec![0_u8; 64];
        let connectors = [Connector {
            name: "DP-1",
            edid: &stub,
        }];
        let outputs = [output("DP-0", "crtc-0", Some(vec![0_u8; 64]))];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// Only the base block is compared: sysfs publishes the whole blob and an
    /// X11 driver may publish the base block alone, and they are the same
    /// monitor.
    #[test]
    fn extension_blocks_do_not_have_to_agree() {
        let mut with_extension = edid(6);
        with_extension.extend(std::iter::repeat_n(0x02_u8, EDID_BASE_BLOCK));
        let connectors = [Connector {
            name: "DP-1",
            edid: &with_extension,
        }];
        let outputs = [output("DP-0", "crtc-3", Some(edid(6)))];

        assert_eq!(only(join(&connectors, &outputs)).by, Matched::ByEdid);
    }

    /// Two outputs on one CRTC is X11 mirroring. Each connector gets its own
    /// placement, and the two placements carry the **same** token — which is what
    /// makes the group logic collapse them into one overlay rather than
    /// double-darkening the shared framebuffer.
    #[test]
    fn mirrored_outputs_share_a_token() {
        let left = edid(20);
        let right = edid(21);
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &left,
            },
            Connector {
                name: "HDMI-A-1",
                edid: &right,
            },
        ];
        let outputs = [
            output("DP-1", "crtc-42", Some(left.clone())),
            output("HDMI-A-1", "crtc-42", Some(right.clone())),
        ];

        let placements = join(&connectors, &outputs);

        let first = nth(&placements, 0).expect("placed by name");
        let second = nth(&placements, 1).expect("placed by name");
        assert_eq!(first.token, second.token);
        assert_eq!(first.bounds, second.bounds);
    }

    /// No display server, or one with nothing enabled: every connector comes
    /// back unplaced, in order, and nothing panics on the empty pool.
    #[test]
    fn no_outputs_places_nothing_and_preserves_order() {
        let a = edid(30);
        let b = edid(31);
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &a,
            },
            Connector {
                name: "DP-2",
                edid: &b,
            },
        ];

        let placements = join(&connectors, &[]);

        assert_eq!(placements.len(), 2);
        assert!(placements.iter().all(Option::is_none), "{placements:?}");
    }

    /// No connectors is not an error either, and the outputs are simply unused.
    #[test]
    fn no_connectors_returns_no_placements() {
        assert!(join(&[], &[at("DP-1", "crtc-0", 0, 1920)]).is_empty());
    }

    /// A server listing one name twice is malformed. Refusing is the same rule
    /// as the identical-monitor case: there is no evidence for choosing, so
    /// there is no choice to make.
    #[test]
    fn a_duplicated_output_name_is_ambiguous_rather_than_first_wins() {
        let sink = edid(40);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [
            at("DP-1", "crtc-a", 0, 1920),
            at("DP-1", "crtc-b", 1920, 1920),
        ];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// Negative origins are ordinary: a monitor left of or above the primary
    /// sits at negative coordinates in every display server's space.
    #[test]
    fn a_negative_origin_survives_the_join() {
        let sink = edid(50);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [ServerOutput {
            name: "DP-1".to_owned(),
            edid: None,
            bounds: DisplayBounds::new(-2560, -400, 2560, 1440),
            token: "crtc-9".to_owned(),
        }];

        assert_eq!(
            only(join(&connectors, &outputs)).bounds,
            DisplayBounds::new(-2560, -400, 2560, 1440)
        );
    }
}
