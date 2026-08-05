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
//! # A name match is not trusted over an EDID that contradicts it
//!
//! The NVIDIA case is worse than "the names do not match", and getting this
//! backwards is how a fallback becomes a hazard. That driver indexes from zero
//! where DRM indexes from one, so the two namespaces **overlap and are offset by
//! one**: sysfs `DP-1` and the server's `DP-1` are *adjacent monitors*, not the
//! same one and not unrelated. A rule that took every name match first would
//! place two of three displays on their neighbour's screen and stamp the result
//! "matched by name" — a silent wrong answer, in the exact configuration the
//! fallback was added for.
//!
//! So the passes run strongest-evidence-first: name **and** EDID agreeing, then
//! EDID alone, and a bare name only where the server published no EDID to check
//! it with. See [`join`].
//!
//! # Ambiguity refuses; it does not guess
//!
//! Two identical monitors with no serial number in their EDID are byte-identical
//! to both sides. A join that picked one anyway would place an overlay on the
//! wrong screen, which is a silent wrong answer rather than a visible missing
//! one. So a pair is claimed only when the match is unique **in both
//! directions** — this connector matches no other output, and no other connector
//! matches this output — and anything else stays unplaced: hardware control
//! intact, software dimming off, which is exactly the state Linux was already in
//! before this module existed.
//!
//! Both directions, because only checking one is half a rule and the missing half
//! is reachable. Two identical monitors with one of them **disabled** in display
//! settings leave a single output that both connectors match equally well; a
//! multi-GPU machine produces two connectors both called `DP-1` once the
//! `card<N>-` prefix is stripped. In each case a one-sided rule hands the output
//! to whichever connector the loop reached first and calls it evidence.
//!
//! Claiming is still one-to-one across passes: an output settled by pass 1 is out
//! of the pool before pass 2 runs. That is what makes the mixed case work — one
//! monitor named consistently and one renamed by the driver resolve to one
//! placement each, by different evidence.
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
    /// On Wayland it is the output name, and the two cannot disagree because a
    /// mirrored monitor has no `wl_output` to name: the compositors that implement
    /// mirroring withdraw the replica's global (`#130` verified `KWin` and
    /// Hyprland; wlroots has no mirror mode at all).
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
/// Carried rather than discarded because the three mean different things to a
/// person reading a log, and because they are ordered by how much they prove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Matched {
    /// The names were equal **and** the EDID base blocks agreed. The modern
    /// stack working as designed, and the only evidence that is self-checking.
    ByNameAndEdid,
    /// The names differed and the EDID base blocks agreed. The driver names
    /// outputs its own way — a real configuration, not a fault, but the one
    /// worth knowing about when a display does not appear where it should.
    ByEdid,
    /// The names were equal and **the server published no EDID to check it
    /// with**. Every Wayland placement is this, since Wayland has no protocol
    /// for an EDID, as is every X11 one on a driver with no `EDID` property.
    ByName,
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
/// Dropping them early also **destroys evidence**, and that is worth saying out
/// loud because it weakens the refusal promise below in one case. Two identical
/// monitors with one disabled leave a single claimant, so a connector carrying
/// that EDID is placed on the enabled one rather than refused. That is right when
/// the connector is the enabled twin and wrong when it is the disabled one, which
/// is reachable if the disabled twin is the only one of the pair with a DDC link.
/// Keeping disabled outputs in the pool purely to contest matches would cost
/// every ordinary disabled-monitor session its placement, which is the far more
/// common case, so the trade is deliberate rather than overlooked.
///
/// # Three passes, strongest evidence first
///
/// 1. **Name and EDID agree.** Self-checking, and therefore the only evidence
///    that cannot be a coincidence of naming.
/// 2. **EDID alone**, for the connectors pass 1 could not settle.
/// 3. **Name alone**, and only where the **server** published no EDID to check
///    it with.
///
/// A bare name match is deliberately *not* taken while both sides published an
/// EDID that disagrees, and this ordering is the whole reason the fallback
/// works at all. The NVIDIA proprietary driver — the case this fallback exists
/// for — indexes outputs from zero where DRM indexes from one, so the two
/// namespaces do not merely differ, they **overlap and are offset by one**.
/// sysfs `DP-1` and the server's `DP-1` are then adjacent monitors. A name-first
/// rule places two of three displays on their neighbour's screen and stamps the
/// result "matched by name"; running the EDID first places all three correctly.
#[must_use]
pub fn join(connectors: &[Connector<'_>], outputs: &[ServerOutput]) -> Vec<Option<Placement>> {
    let mut pool: Vec<Option<&ServerOutput>> = outputs
        .iter()
        .filter(|output| !output.bounds.is_empty())
        .map(Some)
        .collect();
    let mut placements: Vec<Option<Placement>> = connectors.iter().map(|_| None).collect();

    resolve(
        connectors,
        &mut placements,
        &mut pool,
        Matched::ByNameAndEdid,
        |connector, output| output.name == connector.name && edids_agree(connector, output),
    );
    resolve(
        connectors,
        &mut placements,
        &mut pool,
        Matched::ByEdid,
        edids_agree,
    );
    resolve(
        connectors,
        &mut placements,
        &mut pool,
        Matched::ByName,
        |connector, output| output.name == connector.name && !output_has_edid(output),
    );

    placements
}

/// Whether both sides published a base block and the two are equal.
fn edids_agree(connector: &Connector<'_>, output: &ServerOutput) -> bool {
    match (
        base_block(connector.edid),
        output.edid.as_deref().and_then(base_block),
    ) {
        (Some(from_sysfs), Some(from_server)) => from_sysfs == from_server,
        _ => false,
    }
}

/// Whether the **server** published a base block for this output.
///
/// The distinction pass 3 turns on, and it asks about the server side only. A
/// name match with nothing to check it against is the best evidence available; a
/// name match the server could have checked and did not corroborate is not
/// evidence at all — it reached pass 3 precisely because passes 1 and 2 found the
/// EDIDs disagreeing or absent from sysfs.
///
/// Asking "could *either* side have checked it" would be the wrong question, and
/// wrong in the direction this module exists to avoid. A connector whose own EDID
/// came back short cannot corroborate anything, but that says nothing about
/// whether the server's name is trustworthy — and on the NVIDIA offset-by-one
/// namespace it is not. Taking the name there would place the overlay on the
/// neighbouring monitor and stamp it "matched by name", which is the exact defect
/// the pass order was rewritten to close. Duja's own callers never reach it
/// (`duja_core::linux::drm::scan` drops a connector whose EDID is shorter than a
/// base block), but this function is public and must not depend on an invariant
/// enforced two crates away.
fn output_has_edid(output: &ServerOutput) -> bool {
    output.edid.as_deref().and_then(base_block).is_some()
}

/// Claim every pair that `matches` relates **mutually uniquely**, then record it.
///
/// Mutual is the load-bearing word. Refusing only when one connector matches
/// several outputs is half a rule: the other half — several connectors matching
/// one output — is just as ambiguous and, left unchecked, is resolved by
/// whichever connector the loop reached first. That is reachable and not
/// exotic. Two identical monitors with no serial number, one of them disabled in
/// display settings, leaves a single output that both connectors match equally
/// well; a one-sided rule hands it to the first and calls it evidence. So does a
/// multi-GPU machine, where `card0-DP-1` and `card1-DP-1` both arrive here as
/// `DP-1` once the prefix is stripped.
///
/// Every decision is made against **one snapshot** of the state and applied
/// afterwards, so the result does not depend on the order connectors are visited
/// in. Two decisions can never want the same output: whichever connector came
/// second would have seen the first contesting it and refused.
fn resolve(
    connectors: &[Connector<'_>],
    placements: &mut [Option<Placement>],
    pool: &mut [Option<&ServerOutput>],
    by: Matched,
    matches: impl Fn(&Connector<'_>, &ServerOutput) -> bool,
) {
    let open: Vec<bool> = placements.iter().map(Option::is_none).collect();
    let wants = |connector: &Connector<'_>| -> Option<usize> {
        let mut only = None;
        for (index, slot) in pool.iter().enumerate() {
            if slot.is_some_and(|output| matches(connector, output)) {
                if only.is_some() {
                    return None;
                }
                only = Some(index);
            }
        }
        only
    };

    let mut decided: Vec<(usize, usize)> = Vec::new();
    for (index, connector) in connectors.iter().enumerate() {
        if !open.get(index).copied().unwrap_or(false) {
            continue;
        }
        let Some(target) = wants(connector) else {
            continue;
        };
        let Some(output) = pool.get(target).copied().flatten() else {
            continue;
        };
        let contested = connectors.iter().enumerate().any(|(other, rival)| {
            other != index && open.get(other).copied().unwrap_or(false) && matches(rival, output)
        });
        if !contested {
            decided.push((index, target));
        }
    }

    for (index, target) in decided {
        let Some(output) = pool.get_mut(target).and_then(Option::take) else {
            continue;
        };
        if let Some(slot) = placements.get_mut(index) {
            *slot = Some(place(output, by));
        }
    }
}

/// The first [`EDID_BASE_BLOCK`] bytes, or `None` if there are not that many.
///
/// A short EDID is not a truncated match, it is not a base block at all, and
/// comparing whatever prefix exists would let a corrupt read match a healthy
/// monitor.
fn base_block(edid: &[u8]) -> Option<&[u8]> {
    edid.get(..EDID_BASE_BLOCK)
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
    /// same way **and** the EDIDs agree, which is the only self-checking
    /// evidence there is.
    #[test]
    fn an_agreeing_name_and_edid_place_the_connector() {
        let sink = edid(1);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [ServerOutput {
            name: "DP-1".to_owned(),
            edid: Some(sink.clone()),
            bounds: DisplayBounds::new(1920, 0, 2560, 1080),
            token: "crtc-63".to_owned(),
        }];

        let placed = only(join(&connectors, &outputs));

        assert_eq!(placed.bounds, DisplayBounds::new(1920, 0, 2560, 1080));
        assert_eq!(placed.token, "crtc-63");
        assert_eq!(placed.by, Matched::ByNameAndEdid);
    }

    /// A server that publishes no EDID — every Wayland compositor, and an X
    /// driver with no `EDID` property — leaves the name as the only evidence,
    /// and it is then the best evidence available rather than an unchecked one.
    #[test]
    fn a_name_alone_places_the_connector_when_nothing_could_check_it() {
        let sink = edid(1);
        let connectors = [Connector {
            name: "DP-1",
            edid: &sink,
        }];
        let outputs = [at("DP-1", "crtc-63", 1920, 2560)];

        let placed = only(join(&connectors, &outputs));

        assert_eq!(placed.token, "crtc-63");
        assert_eq!(placed.by, Matched::ByName);
    }

    /// **The defect this rule's ordering exists for, and the reason a name is
    /// not taken first.** The NVIDIA proprietary driver indexes outputs from
    /// zero where DRM indexes from one, so the namespaces overlap and are offset
    /// by one: the server's `DP-1` is sysfs's `DP-2`. A name-first join placed
    /// two of these three on their neighbour's screen and stamped the result
    /// "matched by name"; the third was left unplaced because the name pass had
    /// already consumed the output its EDID wanted.
    #[test]
    fn an_offset_by_one_namespace_places_every_display_correctly() {
        let (first, second, third) = (edid(11), edid(22), edid(33));
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &first,
            },
            Connector {
                name: "DP-2",
                edid: &second,
            },
            Connector {
                name: "DP-3",
                edid: &third,
            },
        ];
        let outputs = [
            output("DP-0", "crtc-a", Some(first.clone())),
            output("DP-1", "crtc-b", Some(second.clone())),
            output("DP-2", "crtc-c", Some(third.clone())),
        ];

        let placements = join(&connectors, &outputs);

        for (index, expected) in ["crtc-a", "crtc-b", "crtc-c"].into_iter().enumerate() {
            let placed = nth(&placements, index).expect("every display is placed");
            assert_eq!(placed.token, expected, "connector {index}");
            assert_eq!(placed.by, Matched::ByEdid, "connector {index}");
        }
    }

    /// The same trap with two displays: the server renames one port onto
    /// another's sysfs name. Taking the name would put the overlay on the wrong
    /// screen; taking the EDID first places both.
    #[test]
    fn a_name_that_belongs_to_a_different_monitor_is_not_trusted() {
        let (mine, theirs) = (edid(44), edid(55));
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &mine,
            },
            Connector {
                name: "DP-2",
                edid: &theirs,
            },
        ];
        // The server calls `mine` DP-2 and `theirs` DP-1 — every name is a lie,
        // and every name still matches something.
        let outputs = [
            output("DP-2", "crtc-mine", Some(mine.clone())),
            output("DP-1", "crtc-theirs", Some(theirs.clone())),
        ];

        let placements = join(&connectors, &outputs);

        assert_eq!(
            nth(&placements, 0).map(|p| p.token.clone()),
            Some("crtc-mine".to_owned())
        );
        assert_eq!(
            nth(&placements, 1).map(|p| p.token.clone()),
            Some("crtc-theirs".to_owned())
        );
    }

    /// A name match whose EDID contradicts it, with no correct output anywhere,
    /// stays unplaced. The name is not evidence once something better has
    /// disagreed with it, and an overlay on the wrong screen is worse than none.
    #[test]
    fn a_contradicted_name_places_nothing_rather_than_the_wrong_thing() {
        let mine = edid(66);
        let connectors = [Connector {
            name: "DP-1",
            edid: &mine,
        }];
        let outputs = [output("DP-1", "crtc-someone-else", Some(edid(77)))];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// Half a rule is not the rule. Two identical monitors with **one disabled**
    /// leave a single output that both connectors match equally well — there is
    /// no evidence distinguishing them, and a check that only refuses when one
    /// connector matches several outputs would hand it to whichever came first.
    #[test]
    fn two_connectors_wanting_one_output_are_both_refused() {
        let twin = edid(88);
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
        // Only one of the twins is enabled, so only one output is in the pool.
        let outputs = [output("DP-9", "crtc-only", Some(twin.clone()))];

        let placements = join(&connectors, &outputs);

        assert!(nth(&placements, 0).is_none(), "{placements:?}");
        assert!(nth(&placements, 1).is_none(), "{placements:?}");
    }

    /// The same shape reached without twin monitors: a multi-GPU machine, where
    /// `card0-DP-1` and `card1-DP-1` both arrive here as `DP-1` once the prefix
    /// is stripped. Their EDIDs differ, so pass 1 settles neither by name, and
    /// pass 3 must not settle it by name either.
    #[test]
    fn two_cards_with_the_same_connector_name_do_not_race() {
        let (left, right) = (edid(90), edid(91));
        let connectors = [
            Connector {
                name: "DP-1",
                edid: &left,
            },
            Connector {
                name: "DP-1",
                edid: &right,
            },
        ];
        // The server publishes no EDID, so nothing can tell the two apart.
        let outputs = [at("DP-1", "crtc-0", 0, 1920)];

        let placements = join(&connectors, &outputs);

        assert!(nth(&placements, 0).is_none(), "{placements:?}");
        assert!(nth(&placements, 1).is_none(), "{placements:?}");
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
        assert_eq!(second.by, Matched::ByNameAndEdid);
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

        assert_eq!(
            nth(&placements, 0).map(|p| p.by),
            Some(Matched::ByNameAndEdid)
        );
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

    /// The narrower form of the same trap, on the side it is easy to get wrong.
    /// sysfs came up short, so nothing can corroborate the name — but the
    /// *server* published an EDID, and it says this is a different monitor.
    /// Asking "could either side have checked it" would take the name here and
    /// place the overlay on the neighbour; asking about the server alone refuses.
    #[test]
    fn a_short_connector_edid_does_not_license_a_bare_name_match() {
        let unreadable = vec![0_u8; 64];
        let connectors = [Connector {
            name: "DP-1",
            edid: &unreadable,
        }];
        let outputs = [output("DP-1", "crtc-neighbour", Some(edid(99)))];

        assert!(
            join(&connectors, &outputs)
                .first()
                .is_some_and(Option::is_none)
        );
    }

    /// The same short EDID **is** placed when the server published nothing
    /// either: the name is then the only evidence there is, which is the Wayland
    /// case and not a hazard.
    #[test]
    fn a_short_connector_edid_still_joins_a_server_with_no_edid() {
        let unreadable = vec![0_u8; 64];
        let connectors = [Connector {
            name: "DP-1",
            edid: &unreadable,
        }];
        let outputs = [at("DP-1", "crtc-0", 0, 1920)];

        assert_eq!(only(join(&connectors, &outputs)).by, Matched::ByName);
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
