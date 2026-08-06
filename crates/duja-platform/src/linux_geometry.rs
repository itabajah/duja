//! Pure X11 → tray-anchor geometry for the Linux [`geometry`](crate::geometry)
//! backend: which monitor the cursor is on, what a panel has reserved on it, and
//! what scale factor the window will be drawn at.
//!
//! Not one line here talks to an X server. The backend runs `QueryPointer`, walks
//! `RandR`'s CRTCs, reads the EWMH strut properties off every managed window and
//! fetches the three DPI sources, copies them into the plain structs below, and
//! hands them to [`anchor_from_x11`] — so every decision *about the geometry* is
//! unit-tested on **every** CI lane. Same shape and same reason as `mac_geometry`.
//!
//! Not every decision, and an earlier version of this sentence claimed otherwise
//! by saying "only the reads themselves need an X server". The wire half keeps the
//! rules that are *about* the fetching, and several of them are consequential: the
//! CRTC filter that drops zero-extent and output-less monitors, the `same_screen`
//! guard on `QueryPointer`, the `RandR` version comparison, and the compositing
//! owner check. Each is documented where it is made, and each is in the half no
//! lane can run — which is precisely why the guards downstream of them are pinned
//! here rather than trusted.
//!
//! # Coordinate space
//!
//! X11 needs no conversion — neither half of one — which puts it beside Windows
//! rather than alone. ADR-0021's amendment states the same thing and states it
//! this way round: the interesting divergence in that contract has always been
//! macOS's, and a backend that finds both its answers boring is probably right
//! rather than careless.
//!
//! - **Orientation.** Root-window coordinates are top-left origin, y **down** —
//!   already [`geometry`](crate::geometry)'s contract, so there is no flip to get
//!   wrong (`mac_geometry` carries one because Cocoa is y-up).
//! - **Unit.** X11 has no notion of a logical pixel. CRTC rectangles, the pointer
//!   position, struts and window positions are all in device pixels, and winit's
//!   `set_outer_position` hands a `PhysicalPosition` straight through on X11 — so
//!   the anchor is [`AnchorUnit::PhysicalPixels`], exactly as on Windows, and
//!   `anchor_to_physical` is `1.0`.
//!
//! # The scale factor is a mirror of winit's, deliberately
//!
//! The other two fields describe the screen; `scale` describes something else —
//! how big the flyout is going to be — and it is only useful if it is the number
//! **winit** will size that window by. The consumer multiplies a logical
//! (`.slint` design-unit) size by it to get the box it then clamps into the work
//! area, so a scale this crate invents independently would clamp the wrong
//! rectangle and let the flyout overhang the panel it was placed to avoid.
//!
//! So [`scale_factor`] reproduces winit 0.30's X11 chain rather than choosing its
//! own, in winit's order:
//!
//! 1. `WINIT_X11_SCALE_FACTOR`, which is either the literal `randr` or a float;
//! 2. `Xft/DPI` from the XSETTINGS manager (see
//!    [`linux_xsettings`](crate::linux_xsettings)), divided by 96;
//! 3. `Xft.dpi` from the root window's `RESOURCE_MANAGER`, divided by 96;
//! 4. [`randr_scale`] — pixels per millimetre from the CRTC's size and the
//!    output's physical dimensions.
//!
//! Only step 4 is per-monitor; the first three are session-wide, which is why an
//! X11 session usually reports the same scale for every display no matter how
//! their densities differ. That is also what keeps the mirror honest in practice:
//! wherever a source above step 4 answers, *which* monitor the chain is evaluated
//! on cannot matter.
//!
//! ## Where it is evaluated, when step 4 is reached
//!
//! [`scale_factor`] is evaluated on the monitor under the cursor. winit is not,
//! and the difference is worth stating precisely because it is a live divergence
//! rather than a hypothetical one.
//!
//! winit picks a new X11 window's monitor in `x11/window.rs`, by querying the
//! pointer and taking the first monitor rectangle containing it — except that
//! `XIQueryPointer` reports `Fp1616`, 16.16 fixed point, and winit casts
//! `root_x`/`root_y` to `i64` without the `>> 16`. Every coordinate is therefore
//! 65536× too large, no rectangle contains it, and the guess falls through to
//! `monitors[0]`: the first enabled CRTC, whatever the pointer is doing.
//!
//! Every coordinate but one — `0 << 16` is still `0`, so a pointer resting
//! exactly on the root's origin does match, and winit's `contains_point` is
//! inclusive on both edges. "Always" would be the wrong word by one pixel.
//!
//! winit's CRTC list is also one entry shorter than this module's wherever
//! `GetOutputInfo` failed or an output name was not UTF-8, which it drops and
//! this keeps — so even "the first CRTC" is not always the same CRTC.
//!
//! **That guess is a transient, not the settled value.** On the first synthetic
//! `ConfigureNotify` — the absolute-coordinate one an ICCCM reparenting window
//! manager sends after placing the window — winit recomputes the scale from
//! `get_monitor_for_window`, largest overlap with the window rectangle, and emits
//! `ScaleFactorChanged`. Since the flyout is placed on the cursor's monitor, the
//! settled scale is the one computed here.
//!
//! So the divergence needs the chain to reach step 4, and that happens **two**
//! ways, not one: `WINIT_X11_SCALE_FACTOR=randr` goes straight to the measurement
//! by design, skipping XSETTINGS and `Xft.dpi` entirely; otherwise it takes no
//! override *and* no XSETTINGS manager *and* no `Xft.dpi` resource. Either route
//! then needs two CRTCs of different densities with the cursor not on the first.
//!
//! The `randr` override is the cheap way in and worth naming first, because
//! `docs/qa-checklist.md` tells a tester to set exactly that — an earlier draft of
//! this paragraph listed only the bare-window-manager route and so described the
//! checklist's own instruction as unreachable.
//!
//! What it costs there is a clamp box computed for the settled size while the
//! window is briefly created at another, and on a non-reparenting window manager
//! that sends no synthetic `ConfigureNotify`, for longer than briefly.
//! Reproducing winit's fixed-point bug to match it is the one thing not worth
//! doing — it would be wrong in the other direction the day upstream fixes it —
//! so `docs/debt.md` carries it instead.
//!
//! Two traps are worth naming because both look like omissions:
//!
//! - **`WINIT_HIDPI_FACTOR` is not consulted.** winit reads it only to emit a
//!   deprecation warning and then ignores it, so honouring it here would be this
//!   crate scaling by a number the window is not scaled by.
//! - **An invalid `WINIT_X11_SCALE_FACTOR` falls through instead of panicking.**
//!   winit panics on one; [`crate::geometry::cursor_anchor`] promises never to
//!   fail, and the divergence is unobservable in the shipping binary because
//!   winit reaches its own panic while creating the first window.
//!
//! `docs/debt.md` carries what a mirror costs: it is pinned to a version of
//! winit, and an upstream change to that chain becomes a silent mis-size here.
//!
//! # Saturating arithmetic: three calls can fire, the rest cannot
//!
//! Every coordinate here is an `i32` or a `u32` widened to `i64` before anything
//! is done to it, so a sum or difference of two of them is under 2³³ and an `i64`
//! has room to spare. That single fact settles fourteen of the seventeen
//! `saturating_*` calls outside this file's test module: `work_area`'s two
//! monitor edges (`i32` origin plus `u32` extent) and two strut subtractions
//! (`u32` screen extent minus `u32` depth), `contains`'s two right-and-bottom
//! edges, `extent_from`'s headroom and span, and `distance_squared`'s four edge
//! steps and two gaps — **none of them can reach a bound**. Rewriting all
//! fourteen to the wrapping form at once leaves the suite green.
//!
//! The first version of this paragraph listed twelve of the fourteen, omitting
//! `contains`, under a heading that is a claim about all of them.
//!
//! The exceptions are the three in [`distance_squared`]'s last line, because
//! squaring leaves that range: two gaps that each fit in an `i64` have squares
//! that do not, and a sum that does not. Those are documented and pinned there.
//!
//! This is written once, here, rather than seventeen times: the alternative was a
//! note on each call, and a note on each call is how a claim of unreachability
//! ends up true of thirteen of fourteen.
//!
//! # Work area
//!
//! X11 has no per-monitor work area to ask for. `_NET_WORKAREA` is one rectangle
//! **per desktop**, not per monitor — EWMH defines it as the current page minus
//! docks — so on a two-monitor session a panel on either one shrinks the single
//! global rectangle and the other monitor inherits a gap it does not have.
//!
//! The per-monitor answer has to be computed, from the same inputs a window
//! manager uses: every managed window's `_NET_WM_STRUT_PARTIAL` (or the legacy
//! `_NET_WM_STRUT`), minus the reserved bands that touch this monitor.
//! [`work_area`] is a **conservative** version of that, and the difference is
//! worth stating rather than claiming parity. Mutter builds a minimal spanning set
//! over the strut-subtracted region and clips to the best rectangle in it
//! (`meta_workspace_ensure_work_areas_validated`); this pushes each of the four
//! edges past every band that meets the monitor at all. What that buys is two
//! statements, one per axis and one for the rectangle, because the two axes fail
//! independently.
//!
//! Both are scoped to the bands this module **accepts**: a non-zero depth *and*
//! [`band_meets`] answering true. The depth half is vacuous — a zero-depth band
//! reserves the empty region, which every span excludes — so the scope that does
//! work is `band_meets`, and it is not a formality; see below.
//!
//! - **On an axis the struts did not empty, the result's span on that axis
//!   excludes every accepted band reserved from that axis's edges.** `left` is at
//!   least every such left strut's depth and `right` at most every such right
//!   one's, so no accepted left or right reservation reaches into `[left, right)`;
//!   likewise for the vertical.
//! - **So when neither axis was emptied, the rectangle overlaps no accepted
//!   reservation**, which is the form placement consumes.
//!
//! [`band_meets`] rejects a band for two quite different reasons, and only one of
//! them is harmless:
//!
//! - **Its range on the other axis misses the monitor.** Then the band is nowhere
//!   near this rectangle and there is nothing to exclude. A left panel on the
//!   lower of two stacked monitors reserves `x ∈ [0, 800)` for rows the upper
//!   monitor does not have, so that monitor's span stays `[0, 1920)` — and should.
//! - **Its range is backwards** (`start > end`), which EWMH does not define.
//!   [`band_meets`] discards it, and its own docs say what that costs: an ignored
//!   panel is a panel the flyout may sit under. `a_band_whose_end_precedes_its_start_is_ignored`
//!   is that case, and it is why neither bullet can say "no reservation **at
//!   all**" — a malformed band can be squarely on the monitor.
//!
//! Two drafts of the second bullet did say that, the second one justifying it with
//! "a non-applicable band misses the monitor on the other axis" — true of the
//! first rejection reason and not of the second.
//!
//! What it costs is that a band touching one column of a monitor reserves that
//! monitor's whole edge, where Mutter would have kept the full-height rectangle
//! beside it. Both agree for a panel that spans its monitor, which is every panel
//! anyone actually runs.
//!
//! The per-axis phrasing is not a hedge, and the shorter alternative is not merely
//! weaker but false: an emptied axis is given back **in full**, which produces a
//! perfectly non-degenerate rectangle lying across the very bands that emptied it.
//! `two_conformant_panels_can_empty_an_axis_between_them` is that rectangle — a
//! whole 1920×1080 monitor, overlapping two 1000 px docks. "A non-degenerate
//! result never overlaps a reserved band" would be contradicted by this module's
//! own test suite. [`work_area`] documents why the exception is chosen: an empty
//! rectangle pins the flyout to a corner, an overlapping one merely sits under a
//! panel.
//!
//! Struts are in **root-window**
//! coordinates and the specification is explicit that they are *not* relative to
//! a Xinerama monitor, which is what makes the arithmetic non-obvious: a panel
//! along the bottom of a short monitor beside a taller one reserves a band
//! measured from the bottom of the whole screen, so its `bottom` value is much
//! larger than the panel is tall.

use std::str::FromStr;

use crate::geometry::{AnchorUnit, TrayAnchor, WorkRect, sane_scale};

/// The most a strut property is asked for: thirteen four-byte units, one more
/// than `_NET_WM_STRUT_PARTIAL` can legitimately hold.
///
/// **The extra word is the whole point, and asking for twelve is a live bug.**
/// `GetProperty` returns `MINIMUM(remaining, 4 * long_length)`, so a property
/// carrying thirteen `CARDINAL`s answers a twelve-word request with exactly
/// twelve values and a `bytes_after` nothing here reads â€” and
/// `<[u32; 12]>::try_from` then *succeeds*, honouring a malformed property that
/// an unbounded read rejected. Any client on the display could publish thirteen
/// values and have Duja reserve space a conformant window manager ignores.
///
/// Thirteen restores the old behaviour exactly: a conformant property still
/// returns twelve and is accepted, an over-long one returns thirteen and fails
/// the conversion, and the cost of the guard is four bytes per window. (The first
/// version of this constant was twelve, and its doc said the cap "can only
/// allocate memory nothing will read" â€” true of the memory and false of the
/// behaviour.)
pub(crate) const STRUT_WORDS: u32 = 13;

/// The most `_NET_CLIENT_LIST` is asked for: 8192 windows, 32 KB.
///
/// A desktop session has tens of managed windows and a busy one has hundreds. The
/// cap exists because the property lives on the **root window**, which every
/// client on the display can write: without one, a single hostile or broken
/// client turns a tray click into a multi-gigabyte allocation, and then into two
/// `GetProperty` requests *per listed entry*, all queued before the first reply
/// is read. A list this long is not a session, and truncating it costs at worst
/// a strut from a window beyond the eight-thousandth.
pub(crate) const CLIENT_LIST_WORDS: u32 = 8192;

/// The most `_XSETTINGS_SETTINGS` is asked for: 256 KB.
///
/// GNOME publishes a few kilobytes. The owner of that selection is another
/// client, so the same argument as [`CLIENT_LIST_WORDS`] applies â€” and the parser
/// this feeds is explicitly written for a blob "written by another process". A
/// blob past this cap is truncated, the parser stops where the bytes stop, and
/// the scale chain falls through to the `Xft.dpi` resource, which is what it does
/// for a malformed blob anyway. winit reads the same property in 4 KB chunks with
/// no ceiling at all; matching that would mean matching its unboundedness.
pub(crate) const XSETTINGS_WORDS: u32 = 65_536;

/// The largest extent a [`WorkRect`] produced here reports.
///
/// `i32::MAX`, the same ceiling `mac_geometry::MAX_EXTENT` names and the Windows
/// backend's saturating `right - left` produces, so `x + w` stays inside the
/// `i32` space the anchor contract is expressed in on all three backends. Not
/// reachable from real `RandR` data — a CRTC's extent is a `u16` — but the
/// conversion back out of the 64-bit strut arithmetic has to be total, and
/// picking a *different* cap would be the one way to make the downstream
/// placement kernel see a width it has no substitute for.
const MAX_EXTENT: u32 = i32::MAX.unsigned_abs();

/// The reference DPI a scale factor of 1.0 corresponds to, on X11 as everywhere
/// else.
const BASELINE_DPI: f64 = 96.0;

/// One enabled `RandR` CRTC, as the X11 backend reads it.
///
/// A CRTC rather than a `RandR` 1.5 "monitor" because that is what winit walks,
/// and the scale factor has to agree with winit's per-CRTC one. The backend
/// applies winit's own filter before building these — a zero extent, or no
/// outputs at all — so every entry here is a CRTC that drives something.
///
/// **Not "displaying something", and not "connected".** Neither this crate nor
/// winit reads `RandR`'s `connection` field, so a CRTC still driving an output
/// the server considers `Disconnected` passes the filter in both. An earlier
/// version of this sentence said "no connected output", which the wire half's
/// twin of it was corrected away from and this one was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X11Monitor {
    /// The CRTC's rectangle in root-window coordinates, device pixels.
    pub(crate) bounds: WorkRect,
    /// Physical width in millimetres of the first output this CRTC drives, or
    /// `0` when `RandR` would not say.
    ///
    /// Read only by [`randr_scale`], the last resort of the scale chain. Zero is
    /// a real answer rather than a hypothetical one — winit guards against it and
    /// its comment there ("`XRandR` reported that the display's 0mm in size,
    /// which is certifiably insane") cites the xpra bug that prompted it — which
    /// is why that function opens with a guard instead of a division.
    pub(crate) mm_width: u32,
    /// Physical height in millimetres, with the same caveats as
    /// [`mm_width`](Self::mm_width).
    pub(crate) mm_height: u32,
}

/// The root window's dimensions, which is the space struts are measured in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct X11Screen {
    /// Root window width in pixels.
    pub(crate) width: u32,
    /// Root window height in pixels.
    pub(crate) height: u32,
}

/// One window's reserved space, normalised to EWMH's twelve-field partial form.
///
/// Every field is in **root-window** coordinates. The four widths say how deep
/// the reservation is from the corresponding edge **of the screen**; the eight
/// range fields say the span along that edge over which it applies, and both
/// ends are inclusive (which is how window managers read them — Mutter builds the
/// band's width as `end - start + 1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct X11Strut {
    /// Pixels reserved inward from the screen's left edge.
    pub(crate) left: u32,
    /// Pixels reserved inward from the screen's right edge.
    pub(crate) right: u32,
    /// Pixels reserved downward from the screen's top edge.
    pub(crate) top: u32,
    /// Pixels reserved upward from the screen's bottom edge.
    pub(crate) bottom: u32,
    /// First row the left reservation covers.
    pub(crate) left_start_y: u32,
    /// Last row the left reservation covers, inclusive.
    pub(crate) left_end_y: u32,
    /// First row the right reservation covers.
    pub(crate) right_start_y: u32,
    /// Last row the right reservation covers, inclusive.
    pub(crate) right_end_y: u32,
    /// First column the top reservation covers.
    pub(crate) top_start_x: u32,
    /// Last column the top reservation covers, inclusive.
    pub(crate) top_end_x: u32,
    /// First column the bottom reservation covers.
    pub(crate) bottom_start_x: u32,
    /// Last column the bottom reservation covers, inclusive.
    pub(crate) bottom_end_x: u32,
}

impl X11Strut {
    /// A `_NET_WM_STRUT_PARTIAL` value, which is already this shape.
    ///
    /// The array is ordered as the property is: `left`, `right`, `top`,
    /// `bottom`, then the four `_start`/`_end` pairs in the same edge order. A
    /// fixed-size array rather than twelve arguments precisely because they are
    /// all `u32` and a transposed pair is a silent wrong answer.
    pub(crate) const fn from_partial(values: [u32; 12]) -> Self {
        let [
            left,
            right,
            top,
            bottom,
            left_start_y,
            left_end_y,
            right_start_y,
            right_end_y,
            top_start_x,
            top_end_x,
            bottom_start_x,
            bottom_end_x,
        ] = values;
        X11Strut {
            left,
            right,
            top,
            bottom,
            left_start_y,
            left_end_y,
            right_start_y,
            right_end_y,
            top_start_x,
            top_end_x,
            bottom_start_x,
            bottom_end_x,
        }
    }

    /// Whether this strut reserves any space at all.
    ///
    /// Only the four depths are consulted, because the ranges are meaningless
    /// without one — which is also how a window manager reads them: Mutter's
    /// `meta_window_x11_update_struts` does `if (thickness == 0) continue;`
    /// before it looks at the corresponding `_start`/`_end` pair.
    ///
    /// [`choose_strut`] uses this to decide whether an all-zero
    /// `_NET_WM_STRUT_PARTIAL` should let the legacy `_NET_WM_STRUT` through;
    /// [`work_area`] does not need it, since a zero depth moves no edge there
    /// either. (The caller was the X11 backend until that rule moved here, which
    /// is also why this can now be a link.)
    pub(crate) const fn reserves_anything(&self) -> bool {
        self.left > 0 || self.right > 0 || self.top > 0 || self.bottom > 0
    }

    /// The legacy four-field `_NET_WM_STRUT`, widened to the partial form.
    ///
    /// EWMH defines the short property as the partial one "where all start
    /// values are 0 and all end values are the height or width of the logical
    /// screen", so the ranges come from `screen` rather than from the window.
    ///
    /// Note that this puts the `_end` fields one past the last row or column,
    /// where the field docs above call both ends inclusive. That is the
    /// specification's own wording rather than a slip, and it is followed to the
    /// letter because **it is observable**.
    ///
    /// Two earlier versions of this paragraph said the opposite — "it cannot
    /// matter: the extra row is `screen.height`, and no monitor's rows begin
    /// there", and "widening to `height - 1` instead would be equally correct".
    /// Both are false on an input this module documents elsewhere as reachable: a
    /// monitor whose rows begin *at or past* a stale screen's height, which is what
    /// [`work_area`]'s own doc describes and what its fixtures exercise. On a
    /// `1000x500` screen with a monitor at `y = 500`, a legacy left strut widens to
    /// `left_end_y = 500`, `band_meets(0, 500, (500, 1580))` is true, and the edge
    /// is reserved; with `height - 1` it is `499 >= 500`, false, and it is not. The
    /// two widenings differ on exactly the monitor the old sentence said could not
    /// exist.
    ///
    /// A caller that has both properties prefers the partial one — EWMH says the
    /// window manager MUST ignore `_NET_WM_STRUT` when `_NET_WM_STRUT_PARTIAL` is
    /// present — **except when the partial one reserves nothing**, which is where
    /// [`choose_strut`] follows Mutter instead of the letter of the rule. Mutter
    /// falls back to the legacy property whenever the partial one produced no
    /// strut at all, so on GNOME, obeying the MUST is precisely what would
    /// disagree with the shell about where a window fits. [`choose_strut`]
    /// documents the trade, and its tests walk the whole product of what the two
    /// properties can be.
    pub(crate) const fn from_legacy(values: [u32; 4], screen: X11Screen) -> Self {
        let [left, right, top, bottom] = values;
        X11Strut {
            left,
            right,
            top,
            bottom,
            left_start_y: 0,
            left_end_y: screen.height,
            right_start_y: 0,
            right_end_y: screen.height,
            top_start_x: 0,
            top_end_x: screen.width,
            bottom_start_x: 0,
            bottom_end_x: screen.width,
        }
    }
}

/// The three session-wide DPI sources winit consults, in the order it consults
/// them.
///
/// Two of the three are held as **unparsed strings**, and that is deliberate:
/// winit parses them with `f64::from_str` and treats a parse failure as "this
/// source said nothing". Parsing them in the backend would put that decision in
/// the half no CI lane can run.
pub(crate) struct DpiSources<'a> {
    /// `WINIT_X11_SCALE_FACTOR`, verbatim. Either the literal `randr` or a
    /// float; see [`scale_factor`] for what an invalid value does here versus in
    /// winit.
    pub(crate) scale_override: Option<&'a str>,
    /// `Xft/DPI` from the XSETTINGS manager, already divided out of its 1024ths
    /// by [`crate::linux_xsettings::xft_dpi`] — so this is a DPI, not a scale.
    pub(crate) xsettings_dpi: Option<f64>,
    /// `Xft.dpi` from the root window's `RESOURCE_MANAGER`, verbatim.
    pub(crate) xft_dpi: Option<&'a str>,
}

/// What `WINIT_X11_SCALE_FACTOR` asked for.
enum ScaleOverride {
    /// The literal `randr`: skip the DPI sources and measure the display.
    Randr,
    /// An explicit scale factor.
    Fixed(f64),
}

/// Whether winit would accept `factor` as a scale factor.
///
/// winit's own `validate_scale_factor`, reproduced: positive **and normal**, so
/// zero, negatives, infinities, NaN and subnormals are all rejected. Distinct
/// from [`sane_scale`], which is this crate's floor for a factor a layout will
/// multiply by; this one exists to answer "would winit have taken this", which is
/// the question the override needs.
fn validate_scale_factor(factor: f64) -> bool {
    factor.is_sign_positive() && factor.is_normal()
}

/// Parse `WINIT_X11_SCALE_FACTOR`.
///
/// [`None`] means "no usable override, take the next source", which covers the
/// unset variable, the empty string (winit's own `NotSet`), and the two cases
/// winit **panics** on: a value that is neither `randr` nor a float, and a float
/// that fails [`validate_scale_factor`].
///
/// The `randr` comparison is ASCII-case-insensitive where winit lowercases the
/// whole string first. They cannot disagree: no non-ASCII character lowercases
/// into any of `r`, `a`, `n` or `d`, so the set of strings matching either form
/// is the same set.
fn parse_override(raw: Option<&str>) -> Option<ScaleOverride> {
    let raw = raw?;
    if raw.eq_ignore_ascii_case("randr") {
        return Some(ScaleOverride::Randr);
    }
    let parsed = f64::from_str(raw).ok()?;
    validate_scale_factor(parsed).then_some(ScaleOverride::Fixed(parsed))
}

/// The scale factor winit will apply to a window on `monitor`.
///
/// Never returns a factor a layout cannot multiply by: every branch ends at
/// [`sane_scale`], which is where a settings manager's zero or a negative
/// `Xft.dpi` is neutralised. The individual sources deliberately do **not**
/// clamp — see [`crate::linux_xsettings::xft_dpi`] for why one guard at the end
/// of the chain beats four along it.
///
/// # The top end is deliberately not guarded
///
/// [`sane_scale`] guards the low end only, and says a backend whose source can
/// produce garbage at the top must guard that itself. Only one of these four
/// does: [`randr_scale`] inherits winit's `> 20` ceiling. An XSETTINGS `Xft/DPI`
/// near `i32::MAX`, an `Xft.dpi` of `1e10`, or `WINIT_X11_SCALE_FACTOR=1e10` all
/// pass through as finite `f32`s.
///
/// That is the answer, not an omission, for two reasons. **winit does the same**
/// — it applies no ceiling to either DPI source, and a scale this crate clamped
/// where winit did not would clamp the box while the window kept growing, which
/// is the failure this whole mirror exists to avoid. And the consequence is
/// bounded rather than catastrophic: the app's `scale_extent` saturates and its
/// placement clamps into the work area, so an absurd factor puts the flyout at
/// the work area's origin rather than off-screen or in a panic. A user who
/// writes `Xft.dpi: 1e10` has broken every toolkit on the machine, and Duja
/// failing in the same direction as the rest of the desktop is the honest
/// behaviour.
pub(crate) fn scale_factor(sources: &DpiSources<'_>, monitor: &X11Monitor) -> f32 {
    let resolved = match parse_override(sources.scale_override) {
        Some(ScaleOverride::Randr) => randr_scale(monitor),
        Some(ScaleOverride::Fixed(factor)) => factor,
        None => sources
            .xsettings_dpi
            .or_else(|| sources.xft_dpi.and_then(|raw| f64::from_str(raw).ok()))
            .map_or_else(|| randr_scale(monitor), |dpi| dpi / BASELINE_DPI),
    };
    // RATIONALE (cast_possible_truncation): the anchor carries an `f32`, and
    // every value that reaches here is either a plausible scale factor or
    // something `sane_scale` is about to replace with 1.0. A value too large for
    // `f32` becomes an infinity, which `sane_scale` rejects — so the narrowing
    // cannot manufacture a finite wrong answer.
    #[allow(clippy::cast_possible_truncation)]
    let narrowed = resolved as f32;
    sane_scale(narrowed)
}

/// The scale factor measured from the display itself: pixels per millimetre,
/// quantised to twelfths.
///
/// winit's `calc_dpi_factor`, reproduced including its two escape hatches — a
/// display claiming zero physical size answers 1.0, and so does one whose
/// computed factor exceeds 20, which is how a bogus millimetre reading is
/// stopped from scaling a window off the screen.
///
/// The first of those two is **belt and braces rather than load-bearing**, and
/// saying so is what stops a later reader deleting the second by mistake.
/// Removing the guard changes no answer, by two different routes depending on
/// the pixel count:
///
/// - with pixels to divide, a zero millimetre reading gives an infinity that
///   survives `round` and `max` unchanged and is then caught by the ceiling;
/// - with **no** pixels either — a zero-extent CRTC, which only this module's own
///   suite constructs — it is `0.0 / 0.0`, and the answer comes back 1.0 because
///   `f64::max` returns the *other* operand when one is a NaN, before the ceiling
///   is reached at all.
///
/// Three versions of this paragraph offered only the first route, for a guard
/// whose own first clause admits the second. It is kept because winit has it, and
/// because an answer that depends on that many floating-point edge cases lining
/// up is not one to rely on.
fn randr_scale(monitor: &X11Monitor) -> f64 {
    if monitor.mm_width == 0 || monitor.mm_height == 0 {
        return 1.0;
    }
    let pixels = f64::from(monitor.bounds.w) * f64::from(monitor.bounds.h);
    let millimetres = f64::from(monitor.mm_width) * f64::from(monitor.mm_height);
    let per_mm = (pixels / millimetres).sqrt();
    // 25.4 mm to the inch, over the 96 dpi baseline, times 12 to quantise to
    // twelfths — winit's constant folded exactly as winit writes it, because a
    // re-associated version of the same expression can round differently.
    let quantised = (per_mm * (12.0 * 25.4 / BASELINE_DPI)).round() / 12.0;
    let factor = quantised.max(1.0);
    if factor <= 20.0 { factor } else { 1.0 }
}

/// Index of the monitor the cursor is on, or of the nearest one.
///
/// **This is Duja's choice, not a mirror of winit's.** The two questions are
/// separate and only one of them has to agree with anything: [`scale_factor`] has
/// to be the number winit sizes the window by, because the consumer multiplies a
/// logical size by it; *which monitor's work area to clamp into* is a placement
/// decision with no winit counterpart at all — winit never computes a work area.
/// So the precedent here is Win32's `MONITOR_DEFAULTTONEAREST`, which is the same
/// decision on a platform that ships an answer for it, and the module docs cover
/// separately what it means that the scale is then read off this monitor.
///
/// Containment is half-open on the right and bottom edges, so two abutting
/// monitors never both claim the pixel column between them. A cursor that is on
/// no monitor at all — the gap in an L-shaped layout, or a stale pointer report
/// during a hot-plug — falls back to the nearest rectangle by squared distance,
/// which keeps the flyout on a screen rather than at the fallback rectangle.
///
/// [`None`] only for an empty list, which means `RandR` reported no enabled CRTC.
///
/// # The containment check looks redundant and is not
///
/// [`distance_squared`] is zero for exactly the points [`contains`] accepts —
/// it measures to the last *pixel* rather than to the exclusive edge — **as long
/// as both extents are at least one**. On a zero-extent rectangle they part
/// company: `contains` accepts nothing, while the distance is zero at the
/// origin, because the guard that stops `Ord::clamp` panicking on inverted
/// bounds pulls the last pixel back to the first.
///
/// That is enough to make the early return load-bearing. Put a zero-extent CRTC
/// ahead of a monitor that really contains the cursor, and without the fast path
/// the degenerate one records distance zero first — after which `distance < best`
/// (ties to the earlier monitor) will not let the real one displace it. The
/// answer is a monitor the cursor is nowhere near.
///
/// **Three earlier versions of this paragraph said the opposite**, and cited the
/// surviving mutation as proof: "removing the early return is the one mutation of
/// this function the suite does not redden, which is the evidence rather than an
/// embarrassment". It was the embarrassment. The counterexample was created by
/// this module's own suite, twenty lines away, when
/// `a_zero_extent_monitor_does_not_panic_the_nearest_search` first constructed a
/// zero-extent rectangle — a mutation surviving is a question, and answering it
/// with "therefore the code is redundant" is the same move as trusting a green
/// test.
///
/// Reachable only through a zero-extent CRTC, which `linux::geometry::monitors`
/// filters out — in the half no CI lane can *run*, which is exactly the argument
/// [`distance_squared`]'s own guard is documented under.
///
/// One asymmetry is worth keeping from those earlier versions, because it is
/// still true: an edge that is too *narrow* is absorbed by the fallback on
/// non-degenerate input, so among the ordinary mistakes only a too-*wide* edge
/// changes an answer — by claiming a pixel column that belongs to the neighbour.
/// That is the direction
/// `the_shared_column_between_two_monitors_belongs_to_the_right_hand_one` tests
/// from.
pub(crate) fn monitor_for_cursor(cursor: (i32, i32), monitors: &[X11Monitor]) -> Option<usize> {
    let mut nearest: Option<(usize, i64)> = None;
    for (index, monitor) in monitors.iter().enumerate() {
        if contains(monitor.bounds, cursor) {
            return Some(index);
        }
        let distance = distance_squared(monitor.bounds, cursor);
        // Strictly less, so ties go to the earlier monitor. RandR reports CRTCs
        // in a stable order, so an equidistant cursor picks the same screen every
        // time rather than alternating between two.
        if nearest.is_none_or(|(_, best)| distance < best) {
            nearest = Some((index, distance));
        }
    }
    nearest.map(|(index, _)| index)
}

/// `bounds` minus every strut band that reaches onto it — unless subtracting them
/// would empty an axis, which gives that axis back in full.
///
/// (The first sentence carried no exception for one commit. It is the sentence
/// rustdoc puts in the item list and in search results, so it is both the
/// most-read form of the claim and the one a correction to the paragraphs below
/// does not touch. This project has watched that happen often enough to name it.)
///
/// The reservation an edge suffers is capped at the screen edge's, not summed:
/// two panels stacked on the same edge reserve as much as the deeper one, which
/// is what `max`/`min` across the list gives.
///
/// **Every overlap test is against `bounds`, never against the partially reduced
/// result.** Testing against the running value would make the answer depend on
/// the order the windows happen to appear in `_NET_CLIENT_LIST`: a top panel that
/// had already lowered the work area's top edge could make a left panel's row
/// range stop overlapping, and the left panel would then be ignored on a screen
/// it really covers.
///
/// A strut set that consumes the monitor **along one axis** gives that axis back
/// its full extent and keeps the other. That is the module's standing preference
/// for a wrong-but-usable answer — placement clamps the flyout into whatever it is
/// given, so a zero rectangle pins the window to a corner while the full extent
/// merely risks overlapping a panel — applied per axis rather than to the whole
/// rectangle, because the two axes fail independently. Falling back wholesale
/// would let one malformed `left` value throw away a perfectly good top panel's
/// reservation and open the flyout underneath it.
///
/// **This is one of the two ways the result can overlap a band**, and the only
/// one that involves a band this module accepted; the other is a band with a
/// backwards range, which [`band_meets`] discards and which can therefore sit
/// anywhere. The module docs state both bullets against exactly these two
/// exceptions. (This paragraph said "the one case" for two commits, and before
/// that pointed at a "never overlaps" property the module docs now quote only in
/// order to call it false.)
///
/// Giving the axis back is an overlap wherever the reservations that emptied it
/// actually lie on this monitor, which is the case worth planning for and not
/// quite the same as "always": the two round trips behind a strut and a monitor
/// rectangle are separate, so a reconfiguration between them can leave a
/// reservation recorded against a screen the monitor no longer sits inside. It is
/// chosen anyway, for the reason above.
///
/// Reaching it does **not** need a single absurd strut, which is what an earlier
/// version of this paragraph claimed. Opposing reservations sum: the x axis
/// empties whenever
/// `max(monitor_left, left) >= min(monitor_right, screen_width - right)`, so two
/// well-formed docks of 1000 px each on a 1920-wide single-monitor screen do it
/// between them with neither one deeper than the monitor. Absurd jointly rather
/// than individually — the bound is the inequality, not the depth of any one
/// strut, and `two_conformant_panels_can_empty_an_axis_between_them` pins it from
/// that side.
pub(crate) fn work_area(bounds: WorkRect, screen: X11Screen, struts: &[X11Strut]) -> WorkRect {
    let monitor_left = i64::from(bounds.x);
    let monitor_top = i64::from(bounds.y);
    let monitor_right = monitor_left.saturating_add(i64::from(bounds.w));
    let monitor_bottom = monitor_top.saturating_add(i64::from(bounds.h));
    let screen_right = i64::from(screen.width);
    let screen_bottom = i64::from(screen.height);

    let mut left = monitor_left;
    let mut top = monitor_top;
    let mut right = monitor_right;
    let mut bottom = monitor_bottom;

    for strut in struts {
        let rows = (monitor_top, monitor_bottom);
        let columns = (monitor_left, monitor_right);
        if strut.left > 0 && band_meets(strut.left_start_y, strut.left_end_y, rows) {
            left = left.max(i64::from(strut.left));
        }
        if strut.right > 0 && band_meets(strut.right_start_y, strut.right_end_y, rows) {
            right = right.min(screen_right.saturating_sub(i64::from(strut.right)));
        }
        if strut.top > 0 && band_meets(strut.top_start_x, strut.top_end_x, columns) {
            top = top.max(i64::from(strut.top));
        }
        if strut.bottom > 0 && band_meets(strut.bottom_start_x, strut.bottom_end_x, columns) {
            bottom = bottom.min(screen_bottom.saturating_sub(i64::from(strut.bottom)));
        }
    }

    // Per axis, not per rectangle: see the note above. A monitor that was already
    // degenerate comes back degenerate, which is the same answer `bounds` gave.
    if right <= left {
        left = monitor_left;
        right = monitor_right;
    }
    if bottom <= top {
        top = monitor_top;
        bottom = monitor_bottom;
    }
    rect_from_edges(left, top, right, bottom)
}

/// The anchor for an X11 session, or [`None`] when `RandR` reported no monitor to
/// place a flyout on.
pub(crate) fn anchor_from_x11(
    cursor: (i32, i32),
    monitors: &[X11Monitor],
    screen: X11Screen,
    struts: &[X11Strut],
    dpi: &DpiSources<'_>,
) -> Option<TrayAnchor> {
    let monitor = monitors.get(monitor_for_cursor(cursor, monitors)?)?;
    Some(TrayAnchor {
        cursor,
        work_area: work_area(monitor.bounds, screen, struts),
        scale: scale_factor(dpi, monitor),
        unit: AnchorUnit::PhysicalPixels,
    })
}

/// Which of a window's two strut properties to believe, if either.
///
/// The rule EWMH states and the rule window managers implement differ, and this
/// is where that is decided rather than in the module that fetched the bytes.
/// Both arguments are the raw `CARDINAL` lists as they came off the wire, because
/// "is this the right length" is part of the decision: EWMH's partial form is
/// exactly twelve values and the legacy form exactly four, and anything else is
/// not the property it claims to be.
///
/// The partial form wins when it reserves something. **When it reserves nothing
/// the legacy form is consulted anyway**, which is a deliberate departure from
/// EWMH's "the Window Manager MUST ignore `_NET_WM_STRUT`" — see
/// [`X11Strut::reserves_anything`] and Mutter's `meta_window_x11_update_struts`,
/// which falls back whenever the partial form produced no strut at all. On GNOME,
/// obeying the letter of the rule is what would disagree with the shell.
///
/// This lived in the X11 backend until a review pointed out that it is a
/// consequential rule in that module which is a pure function of its inputs — and
/// that the module's own opening sentence claims nothing there subtracts a strut.
/// (It is not *the* one: `monitors`' zero-extent CRTC filter is another, as three
/// passages in this file say and as the sentence that used to stand here did not.)
///
/// It is now tested on every CI lane over the product of the two properties'
/// possible shapes. **What those shapes are is defined by the `partials` and
/// `legacies` arrays in `the_partial_strut_wins_unless_it_reserves_nothing`, and
/// deliberately not restated here.**
///
/// That is the sixth version of this paragraph. The five before it were each
/// wrong in a new direction and the count was always the thing that broke: "four
/// cases" while the legacy-only shape was missing; "every shape a window can
/// present" while the partial-only one was; "twenty cells" for a grid whose legacy
/// axis was short an all-zero row; "twenty" again after that row was added; and
/// "five values on each axis, and the test walks all twenty-five" in a sentence
/// claiming, one breath later, to be derived from the arrays rather than restated.
///
/// The fifth attempt's *diagnosis* was wrong too, which is why this one points at
/// the arrays instead of describing them. It said growing an array "leaves the
/// suite green because the fixed-length types guard only against shrinking";
/// growing is `error[E0308]` exactly as shrinking is. The real hazard is that
/// `[PartialCase<'_>; 5]`'s `5` is itself a hand-written numeral — the compiler
/// makes you edit it in lockstep with the rows, and at that moment any prose
/// counting those rows is stale with nothing to catch it. A pointer cannot rot
/// that way; a description of the arrays' contents can, and a count of them
/// certainly does.
pub(crate) fn choose_strut(
    partial: Option<&[u32]>,
    legacy: Option<&[u32]>,
    screen: X11Screen,
) -> Option<X11Strut> {
    let twelve = partial
        .and_then(|values| <[u32; 12]>::try_from(values).ok())
        .map(X11Strut::from_partial)
        .filter(X11Strut::reserves_anything);
    if twelve.is_some() {
        return twelve;
    }
    // Deliberately **not** `.filter(X11Strut::reserves_anything)`, unlike the arm
    // above. That filter is not a validity check — it is the fallback decision,
    // and there is nothing after this one to fall back to. An all-zero legacy
    // therefore comes back as a strut that reserves nothing rather than as
    // `None`, which `work_area` treats identically because it skips every zero
    // depth. Adding the filter here changes no work area; it went untested for
    // exactly that reason, and it is written down because the asymmetry between
    // the two arms otherwise reads as an omission.
    <[u32; 4]>::try_from(legacy?)
        .ok()
        .map(|four| X11Strut::from_legacy(four, screen))
}

/// The windowing system winit will put the flyout on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WindowSystem {
    /// An X server, reachable through `DISPLAY`.
    X11,
    /// A Wayland compositor, reachable through `WAYLAND_DISPLAY` or an inherited
    /// `WAYLAND_SOCKET`.
    Wayland,
    /// Neither: a TTY, a service unit, or an `ssh` session with no forwarding.
    None,
}

/// The environment variables that decide it.
///
/// Borrowed rather than read here so the rule is a pure function of three
/// strings, which is what lets the table below be a unit test instead of a
/// process-environment fixture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DisplayEnv<'a> {
    /// `WAYLAND_DISPLAY`.
    pub(crate) wayland_display: Option<&'a str>,
    /// `WAYLAND_SOCKET`: a compositor handing a connected file descriptor to a
    /// client it launched itself.
    pub(crate) wayland_socket: Option<&'a str>,
    /// `DISPLAY`.
    pub(crate) display: Option<&'a str>,
}

/// Which windowing system a flyout would be created on.
///
/// **This is winit's rule, not `duja-dimmer`'s, and the difference is
/// deliberate.** `duja_dimmer::linux_caps::transport` answers a different
/// question — which display server to *drive dimming through* — and consults
/// `WAYLAND_DISPLAY` and `DISPLAY` only. This one has to predict which backend
/// winit's `EventLoop` will pick, because the anchor describes the window winit
/// is about to place, so it consults `WAYLAND_SOCKET` as well. A session with a
/// handed-over socket and a stale `DISPLAY` is the case where the two disagree,
/// and each is right about its own question. `docs/debt.md` carries the half of
/// that which is arguably a gap on the dimmer's side.
///
/// Wayland wins when both are set, for the reason it wins everywhere: nearly
/// every Wayland session also runs Xwayland and sets `DISPLAY`. An **empty**
/// value is treated as unset, which is what a session script that cleared one
/// leaves behind — and, again, what winit does.
pub(crate) fn window_system(env: DisplayEnv<'_>) -> WindowSystem {
    let set = |value: Option<&str>| value.is_some_and(|value| !value.is_empty());
    if set(env.wayland_display) || set(env.wayland_socket) {
        WindowSystem::Wayland
    } else if set(env.display) {
        WindowSystem::X11
    } else {
        WindowSystem::None
    }
}

/// Whether a strut's inclusive `[start, end]` band overlaps the half-open span
/// `[low, high)` of the monitor's opposite axis.
///
/// `start > end` is treated as no band at all. EWMH does not define it, and the
/// safe direction for a malformed property is to reserve nothing: an ignored
/// panel costs an overlapping flyout, while a band read backwards could shrink an
/// unrelated monitor to nothing.
fn band_meets(start: u32, end: u32, (low, high): (i64, i64)) -> bool {
    let (start, end) = (i64::from(start), i64::from(end));
    start <= end && start < high && end >= low
}

/// Whether `point` is inside `rect`, right and bottom edges exclusive.
fn contains(rect: WorkRect, (x, y): (i32, i32)) -> bool {
    let (x, y) = (i64::from(x), i64::from(y));
    let left = i64::from(rect.x);
    let top = i64::from(rect.y);
    x >= left
        && x < left.saturating_add(i64::from(rect.w))
        && y >= top
        && y < top.saturating_add(i64::from(rect.h))
}

/// Squared distance from `point` to the nearest pixel of `rect`, zero inside it.
///
/// Squared because only the ordering matters and a square root would introduce a
/// rounding difference between two monitors that are genuinely equidistant.
///
/// # Which of these guards can fire, and which cannot
///
/// `Ord::clamp` **panics** when its bounds are inverted, and a zero-extent
/// rectangle inverts them: `left + 0 - 1` is one below `left`. Without the
/// `.max(left)` and `.max(top)`, a CRTC reporting a zero width or height would
/// panic inside a call chain that [`crate::geometry::cursor_anchor`] promises
/// never to fail.
///
/// The saturating arithmetic is the rest of the answer, and this paragraph has
/// been rewritten twice for getting the *count* wrong — first "the only guard",
/// then "three". There are nine saturating calls and two `.max()` calls here, so
/// the useful question is not how many are guards but which can fire:
///
/// - **The three that can.** `dx.saturating_mul(dx)`, `dy.saturating_mul(dy)` and
///   the `saturating_add` that combines them. A 1×1 monitor at `i32::MAX` probed
///   from `i32::MIN` gives a gap of `4_294_967_295` on that axis, whose square is
///   twice `i64::MAX` and wraps to **negative** `8_589_934_591` — which beats every
///   real monitor, so the nearest-monitor search returns the rectangle *furthest*
///   from the cursor. The sum overflows on a smaller input still: a 1×1 rectangle
///   at **(7, 3)** probed from `(i32::MIN, i32::MAX)` gives components summing
///   to `9_223_372_049_739_677_761`, about 13 billion past `i64::MAX`. The origin
///   is load-bearing and two versions of this sentence said only "a 1×1
///   rectangle": the same rectangle at the *`(0, 0)`* origin sums to
///   `9_223_372_032_559_808_513`, which fits with 4.3 billion to spare.
/// - **The six that cannot.** Computing `right` and `bottom` adds an extent
///   (`u32`, so under 2³²) to an origin (`i32`, so under 2³¹) and subtracts one:
///   under 2³³ in an `i64`, with no reachable edge. Computing `dx` and `dy`
///   subtracts one `i32`-ranged value from another, so under 2³². They are
///   saturating for uniformity, not for a case — which is true of the eight
///   others in this module too, for the same reason. The module docs state it
///   once over all seventeen, and these three are the exceptions they name.
///
/// Two facts about how that list was arrived at, both worth more than the list.
/// An earlier version offered "the `saturating_add` already saturates on every run
/// of the suite" as though execution were coverage; `wrapping_add` there survived
/// the entire file, because no assertion looked at the result. And the version
/// that fixed *that* pinned `dx`'s multiplication and not `dy`'s — a fix landing
/// on one of a symmetric pair, inside the very note written to correct a fix that
/// landed on one of a symmetric pair. Both axes and the sum are now asserted in
/// `a_wrapping_distance_would_pick_the_furthest_monitor`.
///
/// Unreachable today only because `linux::geometry::monitors` filters zero
/// extents out — which is in the half **no lane can run**. The Ubuntu lane
/// compiles and lints that filter, and the `docs` job doc-checks it; what no lane
/// has is an X server, so nothing ever executes it. (Four comments in this file
/// said "no CI lane compiles", which is a different and false claim — the CI
/// workflow's own comment states the rule the other way round, for the
/// `cfg(windows)` and `cfg(target_os = "macos")` halves that really are
/// uncompiled here.) That is why the guard is pinned by
/// `a_zero_extent_monitor_does_not_panic_the_nearest_search` rather than
/// trusted.
///
/// The same degeneracy is why [`monitor_for_cursor`]'s containment check is not
/// redundant: pulling the last pixel back to the first is what makes this
/// function answer zero for a point [`contains`] rejects, and that disagreement
/// is the whole reason the early return is load-bearing. This note is the one
/// [`monitor_for_cursor`] refers to; for one commit it referred to a note that
/// had never been written, because the round that added the test above added no
/// paragraph here.
fn distance_squared(rect: WorkRect, (x, y): (i32, i32)) -> i64 {
    let left = i64::from(rect.x);
    let top = i64::from(rect.y);
    // The last pixel, not the exclusive edge: clamping to the edge would report a
    // one-pixel distance for a cursor sitting on the far column of a rectangle it
    // is arguably inside.
    let right = left
        .saturating_add(i64::from(rect.w))
        .saturating_sub(1)
        .max(left);
    let bottom = top
        .saturating_add(i64::from(rect.h))
        .saturating_sub(1)
        .max(top);
    let dx = i64::from(x).clamp(left, right).saturating_sub(i64::from(x));
    let dy = i64::from(y).clamp(top, bottom).saturating_sub(i64::from(y));
    dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
}

/// Build a [`WorkRect`] from four edges, saturating rather than wrapping.
///
/// The origin saturates into `i32` and each extent is then capped **twice**: at
/// [`MAX_EXTENT`], and at the room left between the origin and `i32::MAX`. So
/// `x + w` stays representable however absurd the inputs were, which is the
/// promise [`MAX_EXTENT`]'s own doc makes and did not keep: until a review
/// constructed the case, the extent was capped only at [`MAX_EXTENT`] and never
/// against the origin, so a monitor near the top of the coordinate space reported
/// its full width and the sum ran off the end.
///
/// **No saturation is needed to reach that**, which is worth stating because the
/// first fix and its test both said otherwise. `x = i32::MAX - 100` with a width
/// of 4000 is entirely representable, `clamp_to_i32` is the identity on it, and
/// the old expression still returned 4000. Unreachable from `RandR`, whose CRTC
/// origin is an `i16` and extent a `u16` — but `work_area` is `pub(crate)` and its
/// own suite drives it with `i32::MIN` and `u32::MAX`.
///
/// Measuring the extent from the **clamped** origin rather than from `left` is
/// the other half of that first fix, and it is inert. [`work_area`]'s left edge
/// starts inside the `i32` range and only ever moves up, so `clamp_to_i32` can
/// never **raise** it; the one thing it can do is **lower** an edge that a `u32`
/// strut depth pushed past `i32::MAX`, and there the headroom is zero, so the
/// width is zero measured from either edge. (An earlier version of this sentence
/// had those two verbs the other way round. The conclusion was right and the
/// argument for it described an unreachable case.) It is kept for a future caller
/// that could pass a left edge below `i32::MIN`, and named here rather than left
/// to look load-bearing — the same treatment [`randr_scale`]'s zero-millimetre
/// guard and [`clamp_extent`]'s lower cap get, for the same reason.
///
/// [`monitor_for_cursor`]'s containment check was the third member of that list
/// until the commit above this one, which found it was not inert at all. The
/// list is short on purpose now: a claim of inertness is a claim about every
/// input, and this file has already shipped one that a later test falsified.
fn rect_from_edges(left: i64, top: i64, right: i64, bottom: i64) -> WorkRect {
    let x = clamp_to_i32(left);
    let y = clamp_to_i32(top);
    WorkRect {
        x,
        y,
        w: extent_from(x, right),
        h: extent_from(y, bottom),
    }
}

/// The extent from `origin` to `far`, capped so `origin + extent` is still an
/// `i32`.
///
/// The origin is simply an `i32`; whether it got there by clamping is the
/// caller's business, and no saturation is involved in the case this exists for.
fn extent_from(origin: i32, far: i64) -> u32 {
    let origin = i64::from(origin);
    let headroom = i64::from(i32::MAX).saturating_sub(origin);
    clamp_extent(far.saturating_sub(origin).min(headroom))
}

/// Saturate an `i64` coordinate into the `i32` the anchor contract uses.
fn clamp_to_i32(value: i64) -> i32 {
    i32::try_from(value.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
}

/// Saturate an `i64` extent into a `u32` no larger than [`MAX_EXTENT`].
///
/// The lower half of the clamp is inert and is named here rather than left to
/// look load-bearing: `u32::try_from` rejects every negative `i64`, so
/// `unwrap_or(0)` already answers zero for exactly the values `clamp(0, ..)`
/// maps to zero. Replacing the clamp with a bare `.min()` changes no answer, and
/// a mutation run confirms it.
///
/// Unlike the two claims of inertness this module has had to retract, that one
/// is a statement about `u32::try_from` rather than about which inputs a caller
/// can produce — it cannot be falsified by a new fixture. What the clamp buys is
/// that `unwrap_or` becomes unreachable rather than merely unreached, which is
/// the difference between a fallback and a second implementation of the rule.
fn clamp_extent(value: i64) -> u32 {
    u32::try_from(value.clamp(0, i64::from(MAX_EXTENT))).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        DisplayEnv, DpiSources, MAX_EXTENT, WindowSystem, X11Monitor, X11Screen, X11Strut,
        anchor_from_x11, choose_strut, monitor_for_cursor, scale_factor, window_system, work_area,
    };
    use crate::geometry::{AnchorUnit, WorkRect};

    /// A monitor with no physical size reported, so [`super::randr_scale`]
    /// answers 1.0 and the tests that are about geometry are not also about DPI.
    fn monitor(x: i32, y: i32, w: u32, h: u32) -> X11Monitor {
        X11Monitor {
            bounds: WorkRect { x, y, w, h },
            mm_width: 0,
            mm_height: 0,
        }
    }

    /// The three DPI sources all silent, so the chain falls to the display
    /// measurement.
    const NO_DPI: DpiSources<'static> = DpiSources {
        scale_override: None,
        xsettings_dpi: None,
        xft_dpi: None,
    };

    fn approx(got: f32, want: f32) {
        assert!((got - want).abs() < 1e-6, "expected ~{want}, got {got}");
    }

    // -- which monitor -----------------------------------------------------

    #[test]
    fn the_cursor_picks_the_monitor_it_is_inside() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 2560, 1440)];
        assert_eq!(monitor_for_cursor((10, 10), &monitors), Some(0));
        assert_eq!(monitor_for_cursor((2000, 900), &monitors), Some(1));
    }

    #[test]
    fn the_shared_column_between_two_monitors_belongs_to_the_right_hand_one() {
        // Half-open containment is the whole point: with an inclusive right edge
        // both monitors claim x = 1920, and which one wins depends on iteration
        // order — so a flyout opened from the seam would land on a different
        // screen depending on the order RandR happened to list the CRTCs.
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 1920, 1080)];
        assert_eq!(monitor_for_cursor((1919, 0), &monitors), Some(0));
        assert_eq!(monitor_for_cursor((1920, 0), &monitors), Some(1));
        // The same rule on the other axis, for a stacked layout.
        let stacked = [monitor(0, 0, 1920, 1080), monitor(0, 1080, 1920, 1080)];
        assert_eq!(monitor_for_cursor((0, 1079), &stacked), Some(0));
        assert_eq!(monitor_for_cursor((0, 1080), &stacked), Some(1));
    }

    #[test]
    fn a_monitor_left_of_the_primary_keeps_its_negative_origin() {
        // RandR puts the root origin at the top-left of the bounding box, so a
        // negative CRTC origin is not the norm on X11 the way it is on Win32 —
        // but a cursor report can arrive mid-reconfiguration, and dropping the
        // sign would fold the left monitor onto the right one.
        let monitors = [monitor(-1920, -180, 1920, 1200), monitor(0, 0, 1920, 1080)];
        assert_eq!(monitor_for_cursor((-1000, -100), &monitors), Some(0));
        assert_eq!(monitor_for_cursor((-1, 0), &monitors), Some(0));
        assert_eq!(monitor_for_cursor((0, 0), &monitors), Some(1));
    }

    #[test]
    fn a_cursor_on_no_monitor_falls_to_the_nearest_rather_than_the_first() {
        // The gap in an L-shaped layout, and a pointer report that outlived the
        // CRTC it was on. Returning the first monitor unconditionally would put
        // the flyout on the wrong screen whenever the tray is on the second one.
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 1080, 1920, 1080)];
        // (4000, 2500), not (3800, 2000). The old probe was *inside* monitor 1 —
        // 40 px within its right edge and 160 within its bottom — so it returned
        // through the containment fast path and never reached the nearest-search
        // this test is named for, while its own message called it "far past the
        // second monitor's bottom-right". A probe that a test's message places
        // outside the rectangle it is inside makes the test a duplicate of
        // `the_cursor_picks_the_monitor_it_is_inside`.
        assert_eq!(
            monitor_for_cursor((4000, 2500), &monitors),
            Some(1),
            "past the second monitor's bottom-right corner, and nearer to it"
        );
        assert_eq!(
            monitor_for_cursor((-500, -500), &monitors),
            Some(0),
            "off the top-left of the first"
        );
        // Just outside monitor 1's left edge but level with it. Measuring to the
        // origin instead of to the rectangle would give the same answer here, so
        // this probe pins the fallback's *existence*, not its shape — the
        // rectangle-versus-origin distinction is what the next test's `(149, 50)`
        // and `(151, 50)` pin, and deleting that one on the grounds that this one
        // covers it would take the only coverage with it.
        assert_eq!(monitor_for_cursor((1910, 1500), &monitors), Some(1));
    }

    #[test]
    fn an_equidistant_cursor_picks_the_earlier_monitor_every_time() {
        // Two monitors with a gap between them and a cursor exactly in the
        // middle. A `<=` comparison would hand this to the later one, and the
        // choice would look arbitrary; what matters is that repeated calls agree,
        // because the flyout must not hop screens between one open and the next.
        //
        // The gap is 101 wide rather than 100 so that a tie exists at all:
        // distance is measured to the nearest *pixel*, so the left monitor's
        // closest column is 99 and the right one's is 201, and x = 150 is 51 from
        // each. With an even gap no integer coordinate is equidistant and this
        // test would pass without exercising the comparison.
        let monitors = [monitor(0, 0, 100, 100), monitor(201, 0, 100, 100)];
        let picked = monitor_for_cursor((150, 50), &monitors);
        assert_eq!(picked, Some(0));
        // No second call with the same arguments here. One used to sit at this
        // line asserting that a pure function agrees with itself, which no
        // mutation can redden; what makes the choice stable is the strict `<`,
        // pinned by the tie above and the two probes below.
        // One pixel either way still goes to the nearer monitor, which is what
        // makes the case above a genuine tie rather than a left-hand bias.
        assert_eq!(monitor_for_cursor((149, 50), &monitors), Some(0));
        assert_eq!(monitor_for_cursor((151, 50), &monitors), Some(1));
    }

    #[test]
    fn a_mirrored_pair_resolves_to_the_first_crtc() {
        // `xrandr --same-as` can leave two CRTCs at one rectangle. Both contain
        // the cursor, and which one is picked has to be stable rather than
        // right — the work area is identical either way, and the scale differs
        // only if the two outputs report different physical sizes, which is
        // exactly when a mirrored pair has no single correct answer.
        let mirrored = [monitor(0, 0, 1920, 1080), monitor(0, 0, 1920, 1080)];
        assert_eq!(monitor_for_cursor((960, 540), &mirrored), Some(0));
    }

    #[test]
    fn containment_and_a_zero_distance_agree_on_every_real_rectangle() {
        // The fast path in `monitor_for_cursor` returns early on containment, and
        // the fallback picks the first strictly-nearest monitor. Those agree
        // because "inside" and "distance zero" describe the same set of points —
        // `distance_squared` clamps to the last pixel, not to the exclusive edge.
        //
        // "Real" is the load-bearing word, and the next test is why: on a
        // zero-extent rectangle the two disagree, so this one asserts the
        // equivalence over rectangles a CRTC can actually have. An earlier version
        // was named for a universal equivalence and had a fixture set that
        // excluded the only counterexample.
        let rects = [
            WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            WorkRect {
                x: -1920,
                y: -180,
                w: 1920,
                h: 1200,
            },
            WorkRect {
                x: 7,
                y: 3,
                w: 1,
                h: 1,
            },
        ];
        let probes = [
            (0, 0),
            (-1, 0),
            (0, -1),
            (1919, 1079),
            (1920, 1079),
            (1919, 1080),
            (-1920, -180),
            (-1921, -180),
            (7, 3),
            (8, 3),
            (7, 4),
            (i32::MIN, i32::MAX),
        ];
        for rect in rects {
            for probe in probes {
                assert_eq!(
                    super::contains(rect, probe),
                    super::distance_squared(rect, probe) == 0,
                    "{rect:?} and {probe:?} disagree about inside"
                );
            }
        }
    }

    #[test]
    fn an_empty_rectangle_is_the_one_place_the_two_predicates_disagree() {
        // And the reason `monitor_for_cursor`'s early return is not redundant.
        // `contains` accepts nothing from a zero-extent rectangle; the distance is
        // zero at its origin, because `distance_squared`'s clamp guard pulls the
        // last pixel back to the first rather than panicking on inverted bounds.
        let empty = WorkRect {
            x: 7,
            y: 3,
            w: 0,
            h: 0,
        };
        assert!(!super::contains(empty, (7, 3)));
        assert_eq!(super::distance_squared(empty, (7, 3)), 0);
    }

    #[test]
    fn a_degenerate_crtc_cannot_capture_a_cursor_that_is_inside_another() {
        // What that disagreement costs if the fast path goes. Listed first, the
        // zero-extent rectangle records distance zero, and `distance < best` ties
        // to the earlier monitor — so the fallback alone would answer with a
        // monitor the cursor is nowhere near, while the cursor sits squarely
        // inside the second one.
        //
        // Unreachable while `linux::geometry::monitors` filters zero extents, in
        // the half no lane can run — compiled and linted on Ubuntu, executed
        // nowhere, for want of an X server. That is the same footing the guard in
        // `distance_squared` stands on, and the reason both are pinned here.
        let monitors = [monitor(10, 10, 0, 0), monitor(0, 0, 1920, 1080)];
        assert_eq!(monitor_for_cursor((10, 10), &monitors), Some(1));
    }

    #[test]
    fn an_extent_saturates_at_both_ends_rather_than_wrapping() {
        // `clamp_extent` is reached from `extent_from` with a value that has
        // already been through two saturating steps, so the ends are hard to drive
        // from `work_area` and easy to leave unpinned. Its own doc calls the lower
        // cap inert — true, and the reason is `u32::try_from`, so this pins the
        // answer rather than the mechanism.
        assert_eq!(super::clamp_extent(-1), 0);
        assert_eq!(super::clamp_extent(i64::MIN), 0);
        assert_eq!(super::clamp_extent(0), 0);
        assert_eq!(super::clamp_extent(1), 1);
        assert_eq!(super::clamp_extent(i64::from(MAX_EXTENT)), MAX_EXTENT);
        assert_eq!(
            super::clamp_extent(i64::from(MAX_EXTENT) + 1),
            MAX_EXTENT,
            "one past the cap is the cap, not a wrap to zero"
        );
        assert_eq!(super::clamp_extent(i64::MAX), MAX_EXTENT);
    }

    #[test]
    fn no_monitors_is_none_rather_than_a_guess() {
        // RandR reporting nothing enabled is a real state (every output asleep),
        // and it is the backend's cue to use the documented fallback anchor
        // instead of inventing a rectangle here.
        assert_eq!(monitor_for_cursor((0, 0), &[]), None);
        assert!(anchor_from_x11((0, 0), &[], screen(1920, 1080), &[], &NO_DPI).is_none());
    }

    // -- work area ---------------------------------------------------------

    const fn screen(width: u32, height: u32) -> X11Screen {
        X11Screen { width, height }
    }

    /// A bottom panel `depth` deep, spanning columns `from..=to`.
    fn bottom_panel(depth: u32, from: u32, to: u32) -> X11Strut {
        X11Strut {
            bottom: depth,
            bottom_start_x: from,
            bottom_end_x: to,
            ..X11Strut::default()
        }
    }

    #[test]
    fn a_bottom_panel_lifts_only_the_bottom_edge() {
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        // A 40px panel on a single-monitor screen: the strut and the panel's own
        // height coincide only because the monitor reaches the screen's bottom.
        let work = work_area(bounds, screen(1920, 1080), &[bottom_panel(40, 0, 1919)]);
        assert_eq!(
            work,
            WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1040
            }
        );
    }

    #[test]
    fn a_panel_on_one_monitor_leaves_the_other_alone() {
        // The case `_NET_WORKAREA` cannot express, and the reason this function
        // exists. A 40px panel along the bottom of the short right-hand monitor
        // reserves its band from the bottom of the *screen*, so the raw strut is
        // 160 rather than 40 — and the taller left monitor, whose columns the
        // band never touches, must keep its full height.
        let tall = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1200,
        };
        let short = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let root = screen(3840, 1200);
        // 160, not 40: the band runs from the screen's bottom edge at 1200 up to
        // the panel's top at 1040.
        let panel = bottom_panel(160, 1920, 3839);

        assert_eq!(
            work_area(short, root, &[panel]),
            WorkRect {
                x: 1920,
                y: 0,
                w: 1920,
                h: 1040
            },
            "the monitor the panel is on loses exactly the panel's 40 pixels"
        );
        assert_eq!(
            work_area(tall, root, &[panel]),
            tall,
            "the monitor the panel's columns never reach is untouched"
        );
    }

    #[test]
    fn a_left_panel_does_not_reach_the_monitor_to_its_right() {
        let left = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let right = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let panel = X11Strut {
            left: 60,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(left, screen(3840, 1080), &[panel]),
            WorkRect {
                x: 60,
                y: 0,
                w: 1860,
                h: 1080
            }
        );
        assert_eq!(
            work_area(right, screen(3840, 1080), &[panel]),
            right,
            "clamping the right monitor's left edge up to 60 would be a no-op \
             only by accident; it must stay at 1920"
        );
    }

    #[test]
    fn each_band_is_matched_against_the_axis_it_spans_not_the_other_one() {
        // `X11Strut::from_partial`'s docs warn that a transposed pair is a silent
        // wrong answer. So is a transposed *axis*: a left band's range is rows and
        // a top band's is columns, and swapping which span each is tested against
        // survived the whole suite until these three fixtures existed. The right
        // and bottom equivalents were already covered, which is exactly what made
        // the gap look closed.
        let upper = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        // A left dock on the *lower* of two stacked monitors. Its rows are
        // [1080, 2159], which the upper monitor does not have — but its columns
        // would overlap, so matching it against columns reserves 60px here.
        let lower_dock = X11Strut {
            left: 60,
            left_start_y: 1080,
            left_end_y: 2159,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(upper, screen(1920, 2160), &[lower_dock]),
            upper,
            "a left band is a range of rows"
        );

        // The mirror, for the top edge: a top panel spanning the left monitor of a
        // horizontal pair, checked against the right one. Its columns stop at 1919;
        // its rows would overlap.
        let right = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let left_panel = X11Strut {
            top: 30,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(right, screen(3840, 1080), &[left_panel]),
            right,
            "a top band is a range of columns"
        );

        // And the third of `band_meets`' clauses. A band lying entirely *before*
        // the monitor's span is rejected by `end >= low` rather than by
        // `start < high`. This bottom panel's columns end at 1919 and the monitor
        // starts at 1920.
        //
        // An earlier version of this comment claimed every other fixture in the
        // file rejects by `start < high`, "so deleting that clause survived". It
        // did not: the `left_panel` block twelve lines above rejects by this same
        // clause (columns ending at 1919 against a monitor starting at 1920), and
        // both blocks arrived in the same commit — so the mutation the sentence
        // described was never green on the file as shipped. The case below is
        // still worth its lines; the claim about what it uniquely covers was not.
        let early_panel = bottom_panel(40, 0, 1919);
        assert_eq!(
            work_area(right, screen(3840, 1080), &[early_panel]),
            right,
            "a band that ends before the monitor begins does not reach it"
        );
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_the_windows_were_listed_in() {
        // The trap this pins: if each strut's overlap test ran against the
        // *running* work area instead of the monitor, a top panel processed first
        // would lift the top edge past a left panel's row range, and the left
        // panel would then be skipped on a screen it genuinely covers. Struts
        // arrive in `_NET_CLIENT_LIST` order, which is mapping order, so that bug
        // would show up as "the flyout is fine until you restart the panel".
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let top = X11Strut {
            top: 30,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let left = X11Strut {
            left: 60,
            left_start_y: 0,
            left_end_y: 20,
            ..X11Strut::default()
        };
        let expected = WorkRect {
            x: 60,
            y: 30,
            w: 1860,
            h: 1050,
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[top, left]),
            expected
        );
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[left, top]),
            expected
        );

        // The same trap on the other axis, which is a separate line and was
        // separately unpinned: a *narrow* top panel, whose columns a left dock
        // would push the running left edge past. Testing the column span against
        // the running value instead of the monitor makes the answer depend on
        // which of the two was listed first.
        let narrow_top = X11Strut {
            top: 30,
            top_start_x: 0,
            top_end_x: 40,
            ..X11Strut::default()
        };
        let both = WorkRect {
            x: 60,
            y: 30,
            w: 1860,
            h: 1050,
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[left, narrow_top]),
            both
        );
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[narrow_top, left]),
            both
        );
    }

    #[test]
    fn two_panels_on_one_edge_reserve_the_deeper_one_not_their_sum() {
        // A dock and a taskbar on the same edge overlap each other; adding them
        // would leave a strip of work area no window ever occupies.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        // Deepest first, so "the deepest wins" and "the last one wins" stop
        // agreeing — listed 40-then-60 they give the same answer, which is what
        // let a plain assignment survive here.
        let work = work_area(
            bounds,
            screen(1920, 1080),
            &[bottom_panel(60, 0, 1919), bottom_panel(40, 0, 1919)],
        );
        assert_eq!(work.h, 1020, "1080 - 60, not 1080 - 40 and not 1080 - 100");
        let other_order = work_area(
            bounds,
            screen(1920, 1080),
            &[bottom_panel(40, 0, 1919), bottom_panel(60, 0, 1919)],
        );
        assert_eq!(other_order, work, "and the order does not matter");

        // The bottom edge is a `min` against the monitor as well as the screen,
        // and nothing exercised the monitor half: a panel at the bottom of a
        // *taller* screen must not pull the upper monitor's edge down past its
        // own. Under a plain assignment this reports a rectangle 920px taller
        // than the monitor, reaching onto the screen below it.
        let upper = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(
            work_area(upper, screen(1920, 2160), &[bottom_panel(160, 0, 1919)]),
            upper,
            "a monitor cannot grow past its own bottom edge"
        );
    }

    #[test]
    fn a_band_is_matched_against_the_monitors_own_edges_to_the_pixel() {
        // The span `work_area` tests bands against has four endpoints and only one
        // of them was pinned. Each case below moves a band to sit exactly on one
        // edge, so a span off by a pixel in either direction changes the answer.
        //
        // This is the behaviour the module docs single out as the chosen cost — "a
        // band touching one column of a monitor reserves that monitor's whole
        // edge".
        //
        // An earlier version added "and every other fixture in this file either
        // spans a monitor's full height or misses it by hundreds of pixels". At
        // least five miss by exactly one — `lower_dock`, `left_panel`,
        // `early_panel`, the 1919/1920 bottom panels, and the `sliver` — and a
        // mutation census confirms three of `work_area`'s four band endpoints are
        // already killed by those. What this block adds is the fourth,
        // `monitor_top`, which none of them reached.
        let stacked = screen(1920, 2160);
        let lower = WorkRect {
            x: 0,
            y: 1080,
            w: 1920,
            h: 1080,
        };
        let dock = |start: u32, end: u32| X11Strut {
            left: 60,
            left_start_y: start,
            left_end_y: end,
            ..X11Strut::default()
        };

        // Ends exactly on the monitor's first row: inclusive, so it applies.
        assert_eq!(work_area(lower, stacked, &[dock(0, 1080)]).x, 60);
        // Ends one row short: it does not.
        assert_eq!(work_area(lower, stacked, &[dock(0, 1079)]).x, 0);
        // Starts on the monitor's last row: applies. One past it: does not.
        assert_eq!(work_area(lower, stacked, &[dock(2159, 3000)]).x, 60);
        assert_eq!(work_area(lower, stacked, &[dock(2160, 3000)]).x, 0);

        // And both endpoints again on the column axis, with a bottom panel
        // overhanging its neighbour by exactly one column. `band_meets` is one
        // shared function, so the row half above already kills every mutation of
        // its two *endpoint* clauses — though not of `start <= end`, which no row
        // here violates and which
        // `a_band_whose_end_precedes_its_start_is_ignored` pins instead. (An
        // earlier version of this sentence said "every mutation of it", which a
        // one-line deletion falsifies.) An earlier version also claimed "the same
        // two endpoints" while driving only `start < high`, with `end` at 3839
        // against a low of 0 in both rows. The two cases below close that.
        let side_by_side = screen(3840, 1080);
        let left_monitor = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(
            work_area(left_monitor, side_by_side, &[bottom_panel(40, 1919, 3839)]).h,
            1040,
            "one shared column is enough to reserve the whole edge"
        );
        assert_eq!(
            work_area(left_monitor, side_by_side, &[bottom_panel(40, 1920, 3839)]).h,
            1080,
            "and one column past it is not"
        );

        // The far end of the span, which is the endpoint the two rows above hold
        // fixed: a panel ending exactly on the monitor's first column reserves the
        // edge, and one ending a column short of it does not.
        let right_monitor = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(
            work_area(right_monitor, side_by_side, &[bottom_panel(40, 0, 1920)]).h,
            1040,
            "ending on the monitor's first column is inclusive"
        );
        assert_eq!(
            work_area(right_monitor, side_by_side, &[bottom_panel(40, 0, 1919)]).h,
            1080,
            "and ending one column short of it is not"
        );
    }

    #[test]
    fn a_wrapping_distance_would_pick_the_furthest_monitor() {
        // The other load-bearing guard in `distance_squared`, and the one that
        // changes an *answer* rather than a failure mode. Its docs called the two
        // `.max()` calls "the only genuinely load-bearing guard here" for one
        // commit; the saturating arithmetic below them is judged by the same
        // standard and meets it.
        //
        // A 1x1 monitor at `i32::MAX` probed from `i32::MIN` gives a horizontal
        // gap of 4_294_967_295. Squared, that is about 18.4 quintillion — twice
        // `i64::MAX` — and wrapping turns it into *negative* 8_589_934_591. A
        // negative distance beats every real monitor, so the nearest search would
        // answer with the rectangle furthest from the cursor. Saturating gives
        // `i64::MAX`, and the real monitor at 4_611_686_018_427_387_904 wins.
        let far = [monitor(i32::MAX, 0, 1, 1), monitor(0, 0, 1920, 1080)];
        assert_eq!(
            monitor_for_cursor((i32::MIN, 0), &far),
            Some(1),
            "the cursor is at the opposite end of the space from monitor 0"
        );

        // And the same layout rotated, because there are two multiplications and
        // the first version of this test asserted one of them. `dy.wrapping_mul`
        // survived the whole file — a fix landing on one of a symmetric pair,
        // written inside the note that exists because an earlier fix landed on one
        // of a symmetric pair.
        let far_down = [monitor(0, i32::MAX, 1, 1), monitor(0, 0, 1920, 1080)];
        assert_eq!(
            monitor_for_cursor((0, i32::MIN), &far_down),
            Some(1),
            "and the vertical axis is a separate multiplication"
        );

        // The same gaps measured directly, so the saturation is asserted rather
        // than inferred from which monitor won.
        for rect in [
            WorkRect {
                x: i32::MAX,
                y: 0,
                w: 1,
                h: 1,
            },
            WorkRect {
                x: 0,
                y: i32::MAX,
                w: 1,
                h: 1,
            },
        ] {
            let probe = if rect.x == 0 {
                (0, i32::MIN)
            } else {
                (i32::MIN, 0)
            };
            assert_eq!(
                super::distance_squared(rect, probe),
                i64::MAX,
                "the square of the widest possible gap saturates on either axis"
            );
        }

        // The `saturating_add` needs its own case, and the reason is worth stating
        // because the first version of this test did not have one. That addition
        // already overflows on every run of the suite — the 1x1 rectangle at
        // (7, 3), probed from `(i32::MIN, i32::MAX)`, sums to
        // 9_223_372_049_739_677_761, about 13 billion past `i64::MAX` — and
        // `wrapping_add` there survived the whole file anyway, because nothing
        // asserted the result. A line that executes is not a guard that is
        // exercised, which is the same confusion this module has already had to
        // retract about a surviving mutation.
        //
        // The origin is doing work here and two versions of this comment said only
        // "a 1x1 rectangle". At (0, 0) the same probe sums to
        // 9_223_372_032_559_808_513, which fits with 4.3 billion to spare, so the
        // obvious rectangle to reach for is one that would not have overflowed.
        assert_eq!(
            super::distance_squared(
                WorkRect {
                    x: 7,
                    y: 3,
                    w: 1,
                    h: 1
                },
                (i32::MIN, i32::MAX)
            ),
            i64::MAX,
            "two components that each fit still saturate when summed"
        );
    }

    #[test]
    fn a_zero_extent_monitor_does_not_panic_the_nearest_search() {
        // `distance_squared` clamps the cursor into the monitor's last pixel, and
        // `Ord::clamp` panics when its bounds are inverted — which a zero-extent
        // rectangle produces, since the last pixel is one before the first. The
        // `.max(left)`/`.max(top)` there are what stop it, and until this test
        // existed they were a load-bearing guard with nothing pinning them:
        // removing either left the suite green while `cursor_anchor`, which
        // promises never to fail, panicked on a rectangle its own backend can
        // produce.
        //
        // Unreachable through the X11 backend, which filters `width > 0 &&
        // height > 0` — in the half no lane can run. That filter *is* now named
        // within sight of the guard, in `distance_squared`'s own docs; this
        // comment went on saying it was not for one commit after the note landed,
        // which is the half-of-a-pair the same commit was fixing elsewhere.
        let degenerate = [monitor(10, 10, 0, 0), monitor(100, 100, 1920, 1080)];
        assert_eq!(monitor_for_cursor((0, 0), &degenerate), Some(0));
        assert_eq!(monitor_for_cursor((500, 500), &degenerate), Some(1));
        assert_eq!(
            monitor_for_cursor((10, 10), &[monitor(10, 10, 0, 0)]),
            Some(0)
        );
    }

    #[test]
    fn the_deepest_reservation_wins_on_every_edge_not_just_the_bottom() {
        // `work_area` implements this rule on four separate lines, and the test
        // named for it built both of its panels with `bottom_panel` — so three of
        // the four accepted a last-wins implementation with the suite green.
        // Round 17 found the defect on the bottom edge and fixed the fixture
        // there; this is the same rule on the lines that fix did not reach.
        //
        // The configuration is ordinary: a left dock beside a left-edge launcher,
        // or a top bar beside a second top-strut publisher, with the shallower one
        // later in `_NET_CLIENT_LIST`. Under last-wins the flyout opens under the
        // deeper panel until a restart happens to reorder the list.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let side = |depth: u32, right: bool| {
            if right {
                X11Strut {
                    right: depth,
                    right_start_y: 0,
                    right_end_y: 1079,
                    ..X11Strut::default()
                }
            } else {
                X11Strut {
                    left: depth,
                    left_start_y: 0,
                    left_end_y: 1079,
                    ..X11Strut::default()
                }
            }
        };
        let top_of = |depth: u32| X11Strut {
            top: depth,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let root = screen(1920, 1080);

        // Deeper first in each pair, which is the order that separates "deepest
        // wins" from "last wins".
        assert_eq!(
            work_area(bounds, root, &[side(60, false), side(40, false)]).x,
            60,
            "left"
        );
        assert_eq!(
            work_area(bounds, root, &[side(80, true), side(40, true)]).w,
            1840,
            "right"
        );
        assert_eq!(
            work_area(bounds, root, &[top_of(60), top_of(30)]).y,
            60,
            "top"
        );
        assert_eq!(
            work_area(
                bounds,
                root,
                &[bottom_panel(60, 0, 1919), bottom_panel(40, 0, 1919)]
            )
            .h,
            1020,
            "bottom"
        );
    }

    #[test]
    fn a_zero_width_strut_reserves_nothing_however_its_ranges_are_set() {
        // A window that publishes the property with all four depths zero is
        // saying "I reserve nothing"; the ranges are then meaningless and must
        // not move an edge.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let idle = X11Strut {
            left_end_y: 1079,
            top_end_x: 1919,
            right_end_y: 1079,
            bottom_end_x: 1919,
            ..X11Strut::default()
        };
        assert_eq!(work_area(bounds, screen(1920, 1080), &[idle]), bounds);

        // The monitor above sits at the origin, where `left.max(0)` and
        // `top.max(0)` are no-ops — so it cannot see the `> 0` guards at all, and
        // deleting them survived. A negative origin can: `a_monitor_left_of_the_
        // primary_keeps_its_negative_origin` says one is reachable during a
        // reconfiguration, and there `max(0)` would move the edge.
        let off_origin = WorkRect {
            x: -100,
            y: -100,
            w: 1920,
            h: 1080,
        };
        let idle_over_it = X11Strut {
            left_start_y: 0,
            left_end_y: 4000,
            top_start_x: 0,
            top_end_x: 4000,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(off_origin, screen(1920, 1080), &[idle_over_it]),
            off_origin,
            "a zero depth must not pull an edge up to the screen's origin"
        );

        // The right and bottom guards need the mirror shape: a monitor extending
        // past the recorded screen, which is the hot-plug race between
        // `GetGeometry` and the CRTC walk that `work_area`'s docs name. There a
        // zero-depth `min` against the screen edge would shrink the monitor.
        let past_the_screen = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let idle_ranges = X11Strut {
            left_end_y: 4000,
            right_end_y: 4000,
            top_end_x: 4000,
            bottom_end_x: 4000,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(past_the_screen, screen(1000, 500), &[idle_ranges]),
            past_the_screen,
            "a zero depth must not pull an edge back to a stale screen extent"
        );
    }

    #[test]
    fn an_edge_beyond_the_coordinate_space_saturates_the_origin_not_the_width() {
        // `clamp_to_i32`'s clamp is what `rect_from_edges`' docs spend a paragraph
        // on — a `u32` strut depth pushing an edge past `i32::MAX` — and deleting
        // it left the suite green, because no fixture reached that edge with a
        // surviving axis.
        //
        // This one does: the monitor's right edge is already past `i32::MAX`, so a
        // left strut just beyond it leaves `right > left` and the axis is not
        // given back. Without the clamp the origin falls to 0 and the width
        // becomes `MAX_EXTENT` — a rectangle two billion pixels wide starting at
        // the origin, which is as wrong as an answer gets.
        let far = WorkRect {
            x: i32::MAX - 100,
            y: 0,
            w: 4000,
            h: 1080,
        };
        let past_the_end = X11Strut {
            left: 2_147_484_647,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        let work = work_area(far, screen(u32::MAX, u32::MAX), &[past_the_end]);
        assert_eq!(work.x, i32::MAX, "the origin saturates");
        assert_eq!(work.w, 0, "and takes the width with it");

        // `rect_from_edges` clamps two origins on two lines, and this test drove
        // only the first. The same shape on the y axis, with a top strut past the
        // end of the space.
        let far_down = WorkRect {
            x: 0,
            y: i32::MAX - 100,
            w: 1920,
            h: 4000,
        };
        let below_the_end = X11Strut {
            top: 2_147_484_647,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let vertical = work_area(far_down, screen(u32::MAX, u32::MAX), &[below_the_end]);
        assert_eq!(vertical.y, i32::MAX, "so does the vertical origin");
        assert_eq!(vertical.h, 0);
    }

    #[test]
    fn a_band_whose_end_precedes_its_start_is_ignored() {
        // Undefined by EWMH. Reserving nothing costs an overlapping flyout;
        // reading the range backwards could take a whole edge off a monitor the
        // panel is not on.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let backwards = bottom_panel(40, 1919, 0);
        assert_eq!(work_area(bounds, screen(1920, 1080), &[backwards]), bounds);
    }

    #[test]
    fn a_single_row_band_still_counts() {
        // `start == end` is a one-pixel band, not an empty one — the inclusive
        // reading. A half-open reading would drop it, and with it every panel
        // that happens to report a degenerate range.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let sliver = bottom_panel(40, 0, 0);
        assert_eq!(work_area(bounds, screen(1920, 1080), &[sliver]).h, 1040);
    }

    #[test]
    fn a_strut_that_swallows_the_monitor_yields_the_monitor_not_an_empty_rect() {
        // Placement clamps into whatever it is handed, so an empty rectangle
        // pins the flyout to a corner — a worse failure than overlapping a panel,
        // and one that looks deliberate.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let absurd = X11Strut {
            top: 2000,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        assert_eq!(work_area(bounds, screen(1920, 1080), &[absurd]), bounds);
    }

    /// Assert one strut against a computed work area, per the module docs'
    /// guarantee: on an axis that was not handed back in full, the span excludes
    /// every band **this module accepts** reserved from that axis's edges.
    ///
    /// The qualifier is the same one the module docs carry, and it was missing
    /// here for two commits after being restored there — a second copy of a claim
    /// sixty lines from the fixed one.
    ///
    /// The **range** half of acceptance comes from [`super::band_meets`] rather
    /// than from a hand-written copy of it, which deliberately makes this a check
    /// of `work_area`'s arithmetic *given* that rule, not of the rule: a test that
    /// restated the predicate would agree with a wrong one, which is how a sibling
    /// test in this crate quietly became a copy of the function it was checking.
    ///
    /// The **depth** half (`> 0`) *is* restated here, and that is worth admitting
    /// rather than glossing: it is one comparison against a literal, where
    /// `band_meets` is a three-clause predicate over four values, so copying it
    /// costs what copying the other would have cost. A previous version of this
    /// paragraph said acceptance came from `band_meets` full stop — the same
    /// one-condition-for-two imprecision that had just been corrected in the module
    /// docs, one screen away in this file.
    ///
    /// The escape below is **broader** than "this axis was not emptied": an axis
    /// that simply had nothing to subtract also equals the monitor's extent and is
    /// also skipped. That is a weaker check than the sentence above describes, and
    /// it is only sound because the skipped case is vacuous — no accepted band on
    /// an unreduced axis exists in any fixture here. A fixture that broke that
    /// would silently narrow this test rather than fail it.
    fn assert_respects(work: WorkRect, bounds: WorkRect, root: X11Screen, strut: &X11Strut) {
        let (monitor_left, monitor_top) = (i64::from(bounds.x), i64::from(bounds.y));
        let monitor_right = monitor_left.saturating_add(i64::from(bounds.w));
        let monitor_bottom = monitor_top.saturating_add(i64::from(bounds.h));
        let left = i64::from(work.x);
        let top = i64::from(work.y);
        let right = left.saturating_add(i64::from(work.w));
        let bottom = top.saturating_add(i64::from(work.h));
        let rows = (monitor_top, monitor_bottom);
        let columns = (monitor_left, monitor_right);
        // "Handed back" is the documented exception, and it is the only escape:
        // an axis identical to the monitor's own extent either had nothing to
        // subtract or was emptied and restored.
        let kept_x = !(left == monitor_left && right == monitor_right);
        let kept_y = !(top == monitor_top && bottom == monitor_bottom);
        let screen_right = i64::from(root.width);
        let screen_bottom = i64::from(root.height);

        if kept_x && strut.left > 0 && super::band_meets(strut.left_start_y, strut.left_end_y, rows)
        {
            assert!(
                left >= i64::from(strut.left),
                "{work:?} starts inside a left band"
            );
        }
        if kept_x
            && strut.right > 0
            && super::band_meets(strut.right_start_y, strut.right_end_y, rows)
        {
            let edge = screen_right.saturating_sub(i64::from(strut.right));
            assert!(right <= edge, "{work:?} ends inside a right band");
        }
        if kept_y && strut.top > 0 && super::band_meets(strut.top_start_x, strut.top_end_x, columns)
        {
            assert!(
                top >= i64::from(strut.top),
                "{work:?} starts inside a top band"
            );
        }
        if kept_y
            && strut.bottom > 0
            && super::band_meets(strut.bottom_start_x, strut.bottom_end_x, columns)
        {
            let edge = screen_bottom.saturating_sub(i64::from(strut.bottom));
            assert!(bottom <= edge, "{work:?} ends inside a bottom band");
        }
    }

    #[test]
    fn a_surviving_axis_excludes_every_band_that_reaches_it() {
        // The module docs' guarantee, checked rather than asserted in prose: for
        // every band the code **accepts**, either the result's span on that band's
        // axis excludes it, or that axis was handed back in full.
        //
        // Two things that phrasing is careful about, both of which earlier versions
        // of this comment got wrong. "Accepts", not "applicable" — the module docs
        // retired that word because it read as "reaches this monitor", and
        // `band_meets` also rejects a backwards range, which can be anywhere. And
        // the handed-back arm is *one* of the two reasons the guarantee cannot be
        // stated as "the result never overlaps"; a discarded malformed band is the
        // other, and this test does not cover it because `work_area` is right to
        // ignore it — see `a_band_whose_end_precedes_its_start_is_ignored`.
        let full = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let side = |depth: u32, right: bool| {
            if right {
                X11Strut {
                    right: depth,
                    right_start_y: 0,
                    right_end_y: 1079,
                    ..X11Strut::default()
                }
            } else {
                X11Strut {
                    left: depth,
                    left_start_y: 0,
                    left_end_y: 1079,
                    ..X11Strut::default()
                }
            }
        };
        let top = X11Strut {
            top: 30,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let stacked = WorkRect {
            x: 0,
            y: 1080,
            w: 1920,
            h: 1080,
        };
        let tall = screen(1920, 2160);
        let short_of_two = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };

        let cases: [(WorkRect, X11Screen, Vec<X11Strut>); 5] = [
            // A single monitor with a bottom panel.
            (full, screen(1920, 1080), vec![bottom_panel(40, 0, 1919)]),
            // Two monitors, the panel on the short one, measured from the screen.
            (
                short_of_two,
                screen(3840, 1200),
                vec![bottom_panel(160, 1920, 3839)],
            ),
            // Four edges at once on a stacked layout, via the legacy form.
            (
                stacked,
                tall,
                vec![X11Strut::from_legacy([60, 80, 30, 160], tall)],
            ),
            // An axis emptied by two conformant docks: the second arm.
            (
                full,
                screen(1920, 1080),
                vec![side(1000, false), side(1000, true)],
            ),
            // One axis emptied, the other not: both arms in one case.
            (full, screen(1920, 1080), vec![top, side(4000, false)]),
        ];

        for (bounds, root, struts) in cases {
            let work = work_area(bounds, root, &struts);
            for strut in &struts {
                assert_respects(work, bounds, root, strut);
            }
        }
    }

    #[test]
    fn an_extent_is_capped_against_its_own_origin_not_only_at_max_extent() {
        // `MAX_EXTENT`'s doc promises `x + w` stays inside `i32`, and capping the
        // extent only at `MAX_EXTENT` broke that promise: a monitor near the top
        // of the coordinate space reported its full width, so the span ran past
        // the end of it and back across columns an accepted strut had excluded.
        //
        // **Neither fixture saturates anything**, and the first version of this
        // test was named and commented as though both did. `i32::MAX - 100` is a
        // perfectly ordinary coordinate; `clamp_to_i32` is the identity on it. The
        // defect needed no saturation at all, which is precisely what made the
        // first diagnosis of it comfortable and wrong.
        //
        // Unreachable from RandR — a CRTC origin is `i16` and its extent `u16` —
        // which is exactly why it needs a test rather than a comment: nothing else
        // in this file can reach the arithmetic.
        let far = WorkRect {
            x: i32::MAX - 100,
            y: i32::MAX - 100,
            w: 4000,
            h: 4000,
        };
        let work = work_area(far, screen(u32::MAX, u32::MAX), &[]);
        assert_eq!(work.x, i32::MAX - 100);
        assert_eq!(
            i64::from(work.x).saturating_add(i64::from(work.w)),
            i64::from(i32::MAX),
            "the span must stop at the end of the coordinate space, not past it"
        );
        assert_eq!(
            i64::from(work.y).saturating_add(i64::from(work.h)),
            i64::from(i32::MAX)
        );

        // And an origin sitting exactly at the end has no room at all. (Not
        // "saturates": `clamp_to_i32` is the identity on `i32::MAX` too. That
        // word survived here for a commit after the paragraph above was written
        // to retire it.)
        let pinned = WorkRect {
            x: i32::MAX,
            y: 0,
            w: 4000,
            h: 1080,
        };
        assert_eq!(work_area(pinned, screen(u32::MAX, u32::MAX), &[]).w, 0);
    }

    #[test]
    fn two_conformant_panels_can_empty_an_axis_between_them() {
        // The claim `work_area`'s docs now rest on, after an earlier version said
        // emptying an axis "needs a strut deeper than the monitor's own extent,
        // which a conformant panel cannot publish". Opposing reservations sum:
        // neither of these 1000px docks is deeper than the 1920px monitor, and
        // between them they leave nothing.
        //
        // Pinned from this side because the two tests either side of it use the
        // individually-absurd shape the doc explicitly says is *not* the bound, so
        // without this one the correction would be prose the suite never checks.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let left_dock = X11Strut {
            left: 1000,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        let right_dock = X11Strut {
            right: 1000,
            right_start_y: 0,
            right_end_y: 1079,
            ..X11Strut::default()
        };
        // left = max(0, 1000) = 1000; right = min(1920, 1920 - 1000) = 920.
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[left_dock, right_dock]),
            bounds,
            "the x axis empties and is given back; y was never touched"
        );

        // And the boundary itself, which the pair above steps past: two 960px
        // docks leave `right == left` exactly. Weakening the test to `right < left`
        // survived every other fixture, and under it this hands placement a
        // zero-width rectangle — the outcome the give-back exists to avoid.
        let half = X11Strut {
            left: 960,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        let other_half = X11Strut {
            right: 960,
            right_start_y: 0,
            right_end_y: 1079,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[half, other_half]),
            bounds,
            "an exactly-emptied axis is emptied"
        );

        // And the same boundary on the other axis, because the two `if`s are
        // separate lines and the x one was pinned first: `bottom <= top` weakened
        // to `<` survived every fixture until this existed.
        let upper_half = X11Strut {
            top: 540,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let lower_half = X11Strut {
            bottom: 540,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[upper_half, lower_half]),
            bounds,
            "the y axis has its own boundary and its own `if`"
        );
        // One dock alone is well inside the bound and reserves normally, which is
        // what makes the pair the interesting case rather than either half.
        assert_eq!(work_area(bounds, screen(1920, 1080), &[left_dock]).x, 1000);
        assert_eq!(work_area(bounds, screen(1920, 1080), &[right_dock]).w, 920);
    }

    #[test]
    fn one_swallowed_axis_does_not_discard_the_other_axis_reservations() {
        // The fallback is per axis, not per rectangle. A real 30px top panel plus
        // one window publishing a `left` deeper than the monitor is wide must
        // still keep the top panel's reservation — falling back wholesale would
        // open the flyout underneath a panel that reserved space correctly,
        // because of an unrelated window's malformed property.
        let bounds = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let panel = X11Strut {
            top: 30,
            top_start_x: 0,
            top_end_x: 1919,
            ..X11Strut::default()
        };
        let nonsense = X11Strut {
            left: 4000,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[panel, nonsense]),
            WorkRect {
                x: 0,
                y: 30,
                w: 1920,
                h: 1050
            },
            "the x axis is given back in full; the y axis keeps its panel"
        );
        // And the same the other way round, so neither axis is the special case.
        let side = X11Strut {
            left: 60,
            left_start_y: 0,
            left_end_y: 1079,
            ..X11Strut::default()
        };
        let tall_nonsense = X11Strut {
            bottom: 4000,
            bottom_start_x: 0,
            bottom_end_x: 1919,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(bounds, screen(1920, 1080), &[side, tall_nonsense]),
            WorkRect {
                x: 60,
                y: 0,
                w: 1860,
                h: 1080
            }
        );
    }

    #[test]
    fn the_legacy_strut_is_widened_to_the_whole_screen() {
        // `_NET_WM_STRUT` carries no ranges, and EWMH defines it as the partial
        // form with start 0 and end at the screen's extent. Widening it to
        // anything narrower would silently ignore older panels — tint2 and
        // several window-manager-provided bars still publish only this one.
        let root = screen(3840, 1200);
        let widened = X11Strut::from_legacy([0, 0, 0, 160], root);
        let short = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(work_area(short, root, &[widened]).h, 1040);
        // And, unlike the partial form, it reaches every monitor - which is
        // exactly the imprecision the partial property was introduced to fix.
        let tall = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1200,
        };
        assert_eq!(work_area(tall, root, &[widened]).h, 1040);
    }

    #[test]
    fn the_legacy_strut_reaches_a_monitor_that_touches_no_screen_corner() {
        // The other three widened ranges, which the bottom-edge case above
        // cannot reach. Widening `left_end_y` to zero instead of the screen
        // height still *looks* right on any monitor whose top row is 0 — the
        // degenerate band `[0, 0]` overlaps it — so the mistake only shows on a
        // monitor stacked below one, which is where this layout comes from.
        let root = screen(1920, 2160);
        let lower = WorkRect {
            x: 0,
            y: 1080,
            w: 1920,
            h: 1080,
        };
        let widened = X11Strut::from_legacy([60, 80, 30, 160], root);
        assert_eq!(
            work_area(lower, root, &[widened]),
            WorkRect {
                x: 60,
                y: 1080,
                w: 1780,
                h: 920
            },
            "left and right bands must span the whole screen height"
        );
        // A portrait root, which is what separates widening the left and right
        // bands to the screen's *height* from widening them to its *width*. On
        // the landscape roots above the two are indistinguishable, because the
        // monitor's first row is below both.
        let portrait = screen(1080, 3840);
        let lower_portrait = WorkRect {
            x: 0,
            y: 1920,
            w: 1080,
            h: 1920,
        };
        assert_eq!(
            work_area(
                lower_portrait,
                portrait,
                &[X11Strut::from_legacy([60, 80, 0, 0], portrait)]
            ),
            WorkRect {
                x: 60,
                y: 1920,
                w: 940,
                h: 1920
            },
            "the left and right bands span the screen's height"
        );

        // And the top band, on a monitor whose columns start past zero.
        let right_of_origin = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let wide_root = screen(3840, 1080);
        let full_width = X11Strut::from_legacy([0, 0, 30, 0], wide_root);
        assert_eq!(
            work_area(right_of_origin, wide_root, &[full_width]).y,
            30,
            "the top band must span the whole screen width"
        );
    }

    #[test]
    fn only_a_depth_makes_a_strut_a_strut() {
        // What `choose_strut` uses to decide whether an all-zero
        // `_NET_WM_STRUT_PARTIAL` lets the legacy property through. Ranges alone
        // must not count: a window that publishes twelve values of which only the
        // `_start`/`_end` pairs are set has reserved nothing, and treating it as a
        // strut would suppress a legacy property that reserves something real.
        assert!(!X11Strut::default().reserves_anything());
        assert!(
            !X11Strut::from_partial([0, 0, 0, 0, 0, 1079, 0, 1079, 0, 1919, 0, 1919])
                .reserves_anything()
        );
        for edge in 0..4 {
            let mut values = [0_u32; 12];
            *values.get_mut(edge).expect("four depths") = 40;
            assert!(
                X11Strut::from_partial(values).reserves_anything(),
                "edge {edge} alone must count"
            );
        }
    }

    #[test]
    fn the_partial_field_order_matches_the_property() {
        // Twelve `u32`s in one array: a transposed pair compiles, and the only
        // thing standing between the property's order and this struct is this
        // assertion.
        let strut = X11Strut::from_partial([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]);
        assert_eq!(
            strut,
            X11Strut {
                left: 1,
                right: 2,
                top: 3,
                bottom: 4,
                left_start_y: 5,
                left_end_y: 6,
                right_start_y: 7,
                right_end_y: 8,
                top_start_x: 9,
                top_end_x: 10,
                bottom_start_x: 11,
                bottom_end_x: 12,
            }
        );
    }

    #[test]
    fn a_right_edge_panel_is_measured_from_the_screens_right_edge() {
        // The mirror image of the bottom-panel case, and the one an
        // implementation is most likely to get wrong by subtracting from the
        // monitor's own width instead of the screen's.
        //
        // The monitor the panel is on cannot show that mistake: its right edge
        // *is* the screen's, so both arithmetics agree. The **left** monitor is
        // where they diverge — measuring from its own right edge would take 80
        // pixels off a screen the panel is nowhere near, and a full-height band
        // means the row test does not save it either.
        let left_monitor = WorkRect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let right_monitor = WorkRect {
            x: 1920,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let panel = X11Strut {
            right: 80,
            right_start_y: 0,
            right_end_y: 1079,
            ..X11Strut::default()
        };
        assert_eq!(
            work_area(right_monitor, screen(3840, 1080), &[panel]),
            WorkRect {
                x: 1920,
                y: 0,
                w: 1840,
                h: 1080
            }
        );
        assert_eq!(
            work_area(left_monitor, screen(3840, 1080), &[panel]),
            left_monitor,
            "the panel is 1920 pixels away from this monitor's right edge"
        );
    }

    // -- scale -------------------------------------------------------------

    #[test]
    fn a_numeric_override_wins_over_every_other_source() {
        let sources = DpiSources {
            scale_override: Some("1.75"),
            xsettings_dpi: Some(192.0),
            xft_dpi: Some("144"),
        };
        approx(scale_factor(&sources, &monitor(0, 0, 1920, 1080)), 1.75);
    }

    #[test]
    fn the_randr_override_skips_the_dpi_sources_and_measures_the_display() {
        // `WINIT_X11_SCALE_FACTOR=randr` exists precisely to ignore a desktop's
        // Xft.dpi. Letting XSETTINGS through here would make the escape hatch do
        // nothing.
        let laptop = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            mm_width: 344,
            mm_height: 194,
        };
        let sources = DpiSources {
            scale_override: Some("randr"),
            xsettings_dpi: Some(192.0),
            xft_dpi: Some("144"),
        };
        approx(scale_factor(&sources, &laptop), 1.5);
        // Case-insensitively, as winit lowercases the variable first.
        let shouty = DpiSources {
            scale_override: Some("RandR"),
            ..sources
        };
        approx(scale_factor(&shouty, &laptop), 1.5);
    }

    #[test]
    fn an_override_winit_would_reject_falls_through_instead_of_being_used() {
        // winit panics on the first five; this crate may not, because
        // `cursor_anchor` promises never to fail. What it must not do is *use*
        // them — a scale of zero or NaN reaching the layout is the failure mode
        // `sane_scale` exists for, and falling through gets a real number instead
        // of a substituted one. The empty string is the one winit treats as
        // unset, and it takes the same route here.
        for bad in ["garbage", "0", "-1.5", "nan", "inf", ""] {
            let sources = DpiSources {
                scale_override: Some(bad),
                xsettings_dpi: Some(144.0),
                xft_dpi: None,
            };
            approx(scale_factor(&sources, &monitor(0, 0, 1920, 1080)), 1.5);
        }
    }

    #[test]
    fn xsettings_outranks_the_x_resource() {
        // winit's order. A desktop that changes its DPI at runtime updates
        // XSETTINGS immediately and the resource database on its own schedule, so
        // reading them the other way round means scaling by a stale number for as
        // long as the two disagree.
        let sources = DpiSources {
            scale_override: None,
            xsettings_dpi: Some(192.0),
            xft_dpi: Some("96"),
        };
        approx(scale_factor(&sources, &monitor(0, 0, 1920, 1080)), 2.0);
    }

    #[test]
    fn the_x_resource_is_used_when_no_settings_manager_is_running() {
        let sources = DpiSources {
            scale_override: None,
            xsettings_dpi: None,
            xft_dpi: Some("120"),
        };
        approx(scale_factor(&sources, &monitor(0, 0, 1920, 1080)), 1.25);
    }

    #[test]
    fn an_unparseable_x_resource_is_the_same_as_none() {
        // `Xft.dpi: 96dpi` is a real thing to find in a hand-written
        // `.Xresources`. winit's `f64::from_str` rejects it and measures the
        // display instead.
        let laptop = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            mm_width: 344,
            mm_height: 194,
        };
        let sources = DpiSources {
            scale_override: None,
            xsettings_dpi: None,
            xft_dpi: Some("96dpi"),
        };
        approx(scale_factor(&sources, &laptop), 1.5);
    }

    #[test]
    fn the_measured_fallback_quantises_to_twelfths_the_way_winit_does() {
        // A 3:2 panel, and the only fixture in this file that separates twelfths
        // from its neighbours: 2256x1504 at 285x190 mm measures 2.0944 raw, which
        // is 2.0833 in twelfths, 2.1667 in sixths, and 2.0 in quarters or halves.
        //
        // Every other fixture here agrees across all four step sizes, which is why
        // replacing `12.0` with 6, 4 or 2 survived the whole suite — on the one
        // constant this module exists to keep bit-identical to winit's
        // `calc_dpi_factor`. This test's comment used to assert the opposite in
        // as many words ("rounding it to anything else — nearest half, nearest
        // quarter — gives a window winit will draw at a different size"); nearest
        // half of 1.4748 is 1.5, the same answer, which is exactly the claim a
        // fixture should have been made to check rather than a sentence to make.
        let three_by_two = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 2256,
                h: 1504,
            },
            mm_width: 285,
            mm_height: 190,
        };
        approx(scale_factor(&NO_DPI, &three_by_two), 25.0 / 12.0);

        // Two real displays. The 15.6-inch 1080p panel lands on 1.5, and pins the
        // quantisation's *existence* rather than its step: the raw ratio is
        // 1.4748 and every step size above rounds it there.
        let laptop = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            mm_width: 344,
            mm_height: 194,
        };
        approx(scale_factor(&NO_DPI, &laptop), 1.5);

        // A 24-inch 1080p desktop monitor: 91 dpi, below the baseline, and the
        // `max(1.0)` is what stops it reporting a factor under 1.
        let desktop = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            mm_width: 531,
            mm_height: 299,
        };
        approx(scale_factor(&NO_DPI, &desktop), 1.0);
    }

    #[test]
    fn a_display_that_reports_no_physical_size_measures_as_one() {
        // A display reporting 0 mm is a real answer rather than a hypothetical
        // one — winit guards the same case and cites the xpra bug that prompted
        // it. (An earlier version of this comment named three kinds of hardware
        // that supposedly do it; two of the three I could not support, and the
        // commit that struck them from `X11Monitor::mm_width`'s doc left this
        // copy standing.)
        //
        // This pins the *property*, not the guard that delivers it, and the
        // distinction is worth stating because deleting the guard does not redden
        // this test: the division yields an infinity, `round` and `max` leave it
        // one, and the `> 20` ceiling below turns it into the same 1.0. The guard
        // is kept for the reason it exists in winit — so the answer comes from a
        // decision rather than from three float edge cases agreeing — and no test
        // can distinguish the two, which is why one is not claimed here.
        //
        // The last row is a different mechanism rather than a fourth fixture of
        // the same one, and it is here because `randr_scale`'s doc argued the
        // infinity route for a guard that also admits this: with no pixels to
        // divide either, the ratio is a NaN, and 1.0 comes back from `f64::max`
        // preferring the non-NaN operand long before the ceiling is consulted.
        for (pixels, mm_width, mm_height) in [
            ((1920, 1080), 0, 194),
            ((1920, 1080), 344, 0),
            ((1920, 1080), 0, 0),
            ((0, 0), 0, 0),
        ] {
            let odd = X11Monitor {
                bounds: WorkRect {
                    x: 0,
                    y: 0,
                    w: pixels.0,
                    h: pixels.1,
                },
                mm_width,
                mm_height,
            };
            approx(scale_factor(&NO_DPI, &odd), 1.0);
        }
    }

    #[test]
    fn the_measurement_ceiling_is_inclusive_the_way_winits_is() {
        // winit's escape hatch is `if dpi_factor <= 20.` and this mirrors it, so
        // the boundary is part of the mirror. A factor of exactly 20.0 is
        // reachable — 3840x2160 reported as 3x482 mm quantises to it precisely —
        // and `<` instead of `<=` there discards a measurement winit keeps.
        let exactly_twenty = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 3840,
                h: 2160,
            },
            mm_width: 3,
            mm_height: 482,
        };
        approx(scale_factor(&NO_DPI, &exactly_twenty), 20.0);

        // And just past it, which pins the ceiling's *value* rather than only its
        // inclusivity: the other above-ceiling fixture measures 762, so raising
        // the constant to 21, 30 or 700 changed no answer. 3840x2160 at 2x690 mm
        // quantises to 20.5.
        let just_over = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 3840,
                h: 2160,
            },
            mm_width: 2,
            mm_height: 690,
        };
        approx(scale_factor(&NO_DPI, &just_over), 1.0);
    }

    #[test]
    fn an_absurd_measurement_is_discarded_rather_than_scaled_by() {
        // winit's ceiling: a 4K display claiming to be one millimetre across
        // computes a factor of 762, and a window scaled by that is not merely
        // wrong, it is unmappable. Above 20 the measurement is treated as noise.
        let nonsense = X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 3840,
                h: 2160,
            },
            mm_width: 1,
            mm_height: 1,
        };
        approx(scale_factor(&NO_DPI, &nonsense), 1.0);
    }

    /// The five values a source can carry that a layout cannot multiply by.
    ///
    /// Written as strings because two of the three sources are unparsed text; the
    /// XSETTINGS loop parses them back, which keeps one list behind all three.
    const DEGENERATE: [&str; 5] = ["0", "-96", "nan", "inf", "0.0"];

    /// A 15.6-inch 1080p laptop panel, which [`randr_scale`] measures at 1.5.
    ///
    /// The fixture every test about the *chain* wants, as opposed to a test about
    /// one link of it. A display reporting no physical size measures 1.0, which is
    /// also what `sane_scale` substitutes — so against that fixture "the value was
    /// clamped" and "the chain fell through to the measurement" are the same
    /// number, and no assertion can tell them apart. This one measures something,
    /// so they can.
    fn measured_monitor() -> X11Monitor {
        X11Monitor {
            bounds: WorkRect {
                x: 0,
                y: 0,
                w: 1920,
                h: 1080,
            },
            mm_width: 344,
            mm_height: 194,
        }
    }

    #[test]
    fn a_degenerate_dpi_from_either_read_source_is_neutralised_at_the_end() {
        // A settings manager can publish zero or a negative, and a hand-written
        // `.Xresources` can carry the same; `sane_scale` is the single place that
        // is caught, which is why neither parser invents a floor of its own.
        //
        // That second half is the claim the fixture has to make checkable, and
        // `measured_monitor` is why it is. A parser that filtered a degenerate
        // value at the source — returning `None` instead of passing it through —
        // would hand the chain on to the measurement, and against a 0 mm display
        // the measurement is also 1.0, so the test would stay green while the
        // property it names had been broken. Against a display that measures 1.5,
        // that mutation reddens.
        //
        // Both *read* sources, which is what this test's name claims. The
        // remaining source — `WINIT_X11_SCALE_FACTOR`, which the chain consults
        // **first** — does not belong here: it is rejected a step earlier, by
        // winit's `validate_scale_factor`, and never reaches `sane_scale` at all.
        // The next test pins that distinction.
        //
        // Named rather than numbered: "the third source" invites the reader to
        // count the chain, where the override is first.
        for raw in DEGENERATE {
            let parsed: f64 = raw.parse().expect("the fixture is a float");
            let from_xsettings = DpiSources {
                scale_override: None,
                xsettings_dpi: Some(parsed),
                xft_dpi: None,
            };
            approx(scale_factor(&from_xsettings, &measured_monitor()), 1.0);

            let from_resource = DpiSources {
                scale_override: None,
                xsettings_dpi: None,
                xft_dpi: Some(raw),
            };
            approx(scale_factor(&from_resource, &measured_monitor()), 1.0);
        }
    }

    #[test]
    fn a_degenerate_override_hands_the_chain_on_rather_than_being_substituted() {
        // `WINIT_X11_SCALE_FACTOR`: the source the chain consults first, and the
        // one that behaves differently when it is degenerate. winit's
        // `validate_scale_factor` rejects these before the chain resumes, so the
        // answer is *the next source's*, not `sane_scale`'s 1.0.
        //
        // The fixture is what makes that observable: a display measuring 1.5, so a
        // pass-through and a substitution give different numbers. Asserting this
        // against a 0mm display — where the fall-through also yields 1.0 — is a
        // test no mutation can fail, which is exactly what an earlier version of
        // it was.
        let laptop = measured_monitor();
        for raw in DEGENERATE {
            let rejected = DpiSources {
                scale_override: Some(raw),
                xsettings_dpi: None,
                xft_dpi: None,
            };
            approx(scale_factor(&rejected, &laptop), 1.5);
            // And with a source behind it, the fall-through lands there instead —
            // which is the half that distinguishes "rejected" from "clamped".
            let with_xsettings = DpiSources {
                scale_override: Some(raw),
                xsettings_dpi: Some(192.0),
                xft_dpi: None,
            };
            approx(scale_factor(&with_xsettings, &laptop), 2.0);
        }
        // A *valid* override is used rather than handed on, which is what stops
        // the assertions above from passing for a `parse_override` that rejects
        // everything.
        let honoured = DpiSources {
            scale_override: Some("1.25"),
            xsettings_dpi: Some(192.0),
            xft_dpi: None,
        };
        approx(scale_factor(&honoured, &laptop), 1.25);
    }

    // -- the assembled anchor ----------------------------------------------

    #[test]
    fn the_anchor_describes_the_cursors_monitor_and_declares_physical_pixels() {
        let monitors = [
            X11Monitor {
                bounds: WorkRect {
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
                mm_width: 531,
                mm_height: 299,
            },
            X11Monitor {
                bounds: WorkRect {
                    x: 1920,
                    y: 0,
                    w: 1920,
                    h: 1080,
                },
                // Deliberately a *different* density from the first monitor, and
                // deliberately with no session-wide DPI source below: otherwise
                // "the cursor's monitor" and "monitors[0]" are the same number and
                // the scale half of this test's name is unobservable. That is the
                // behaviour the module docs, STATUS and a debt row all spend their
                // longest passages on, so it earns a fixture that can see it.
                mm_width: 344,
                mm_height: 194,
            },
        ];
        let sources = NO_DPI;
        let anchor = anchor_from_x11(
            (2500, 900),
            &monitors,
            screen(3840, 1080),
            &[bottom_panel(40, 1920, 3839)],
            &sources,
        )
        .expect("two enabled monitors");

        assert_eq!(anchor.cursor, (2500, 900));
        assert_eq!(
            anchor.work_area,
            WorkRect {
                x: 1920,
                y: 0,
                w: 1920,
                h: 1040
            },
            "the second monitor's work area, not the first monitor's"
        );
        assert_eq!(anchor.unit, AnchorUnit::PhysicalPixels);
        approx(anchor.scale, 1.5);
        // X11 converts in neither direction — the anchor is already the space
        // `set_outer_position` takes — which is the same pair of factors Windows
        // gets, not a distinction from it.
        approx(anchor.anchor_to_physical(), 1.0);
        approx(anchor.logical_to_anchor(), 1.5);
    }

    /// A partial-strut fixture: its name, its bytes, and the strut it wins with —
    /// [`None`] when it does not win, in which case the legacy value decides.
    ///
    /// The third column was a `bool` for two commits, which made the oracle reach
    /// outside the row for `X11Strut::from_partial(partial)` and hard-code the
    /// fixture that happens to be the only winning one. Correct only because both
    /// outcomes coincide there; a second conformant row with different bytes would
    /// have been asserted against the first row's answer.
    type PartialCase<'a> = (&'a str, Option<&'a [u32]>, Option<X11Strut>);
    /// A legacy-strut fixture: its name, its bytes, and what it decodes to.
    type LegacyCase<'a> = (&'a str, Option<&'a [u32]>, Option<[u32; 4]>);

    #[test]
    fn a_strut_request_asks_for_one_word_more_than_a_strut_can_hold() {
        // `STRUT_WORDS` used to live in the X11 backend, which is
        // `cfg(target_os = "linux")`, so no test on any lane referenced it: setting
        // it back to twelve — the value whose own doc calls it "a live bug" —
        // left every lane green. The strut grid below pins what `choose_strut`
        // does with thirteen values; this pins that thirteen is what arrives.
        //
        // `GetProperty` returns `MINIMUM(remaining, 4 * long_length)`, so asking
        // for exactly twelve makes a thirteen-`CARDINAL` property answer with
        // twelve, which `<[u32; 12]>::try_from` then accepts — honouring a
        // malformed property that an unbounded read rejects. One word of headroom
        // is what turns truncation into a detectable over-length.
        assert_eq!(
            super::STRUT_WORDS,
            12 + 1,
            "one more than `_NET_WM_STRUT_PARTIAL`'s twelve CARDINALs"
        );
        // The other two bounds are caps rather than detectors, so what matters is
        // only that they are generous enough for a real session. Written as
        // `const` blocks because that is what they are — a comparison of two
        // literals, checked when this file compiles rather than when the test
        // runs, which is the form clippy asks for and the form `geometry.rs`
        // already uses for its own floor. They fail the build, not the suite.
        const { assert!(super::CLIENT_LIST_WORDS >= 1024) };
        const { assert!(super::XSETTINGS_WORDS >= 4096) };
    }

    #[test]
    fn the_partial_strut_wins_unless_it_reserves_nothing() {
        // The rule that deliberately disobeys EWMH's MUST, over the product of
        // what each property can be — the kinds are listed in the two arrays at the
        // end of this test. It was unreachable from any test until it moved out of
        // the X11 backend.
        //
        // This comment has been wrong five times, each in a new direction. It said
        // "four shapes" while the legacy-only case was absent; the round that added
        // that case promoted it to "every shape a window can present" and left out
        // the partial-only case, the commoner of the two; the round that added
        // *that* named the axes and claimed their product while asserting twelve of
        // its twenty cells; the round that added the missing all-zero legacy row
        // updated the loop and left three copies describing a four-value axis; and
        // the round that fixed *those* bumped this sentence's count from three to
        // four without adding the fourth item to this list.
        //
        // Walking the rows did not catch any of it, and an earlier version of this
        // paragraph claimed it would. The grid checks the *rule*; nothing checks a
        // number written beside it, which is why there is no longer one.
        let root = screen(1920, 1080);
        let partial = [40, 0, 0, 0, 0, 1079, 0, 0, 0, 0, 0, 0];
        let empty_partial = [0_u32; 12];
        let legacy = [0, 0, 60, 0];

        // A conformant partial wins outright.
        assert_eq!(
            choose_strut(Some(&partial), Some(&legacy), root),
            Some(X11Strut::from_partial(partial)),
            "the partial form is preferred"
        );
        // One that reserves nothing lets the legacy form through, which is
        // Mutter's behaviour rather than EWMH's.
        assert_eq!(
            choose_strut(Some(&empty_partial), Some(&legacy), root),
            Some(X11Strut::from_legacy(legacy, root)),
            "an all-zero partial is not a reservation"
        );
        // No partial property at all, and a well-formed legacy one: the ordinary
        // shape for a panel that publishes only `_NET_WM_STRUT`. Every other row
        // that drives the legacy branch reaches it through a *partial* that was
        // rejected, so a reader could reasonably have taken the legacy form for a
        // fallback rather than a source in its own right.
        assert_eq!(
            choose_strut(None, Some(&legacy), root),
            Some(X11Strut::from_legacy(legacy, root)),
            "a legacy-only panel reserves its edge"
        );
        // And its mirror image, which is the *more* ordinary of the two and was
        // missing from the round that added the one above. Every row with a valid
        // twelve-word partial also carried a valid legacy beside it, so nothing
        // asserted what a modern panel — one that publishes only
        // `_NET_WM_STRUT_PARTIAL` — actually gets. `struts` reads the two atoms
        // independently precisely so that shape arrives here as `(Some, None)`.
        //
        // The surviving mutation was the natural simplification: compute the
        // legacy candidate with `legacy?` up front and end with
        // `twelve.or(legacy)`. Every assertion in this test passed, while a modern
        // panel's reservation was silently discarded and the flyout opened
        // underneath it.
        assert_eq!(
            choose_strut(Some(&partial), None, root),
            Some(X11Strut::from_partial(partial)),
            "a partial-only panel reserves its edge with no legacy to fall back to"
        );
        assert_eq!(
            choose_strut(Some(&empty_partial), None, root),
            None,
            "and one that reserves nothing has nothing to fall back to"
        );
        // A partial of the wrong length is not the property it claims to be.
        assert_eq!(
            choose_strut(Some(&partial[..11]), Some(&legacy), root),
            Some(X11Strut::from_legacy(legacy, root))
        );
        assert_eq!(choose_strut(Some(&partial[..11]), None, root), None);
        // As is a legacy value of the wrong length.
        assert_eq!(choose_strut(None, Some(&legacy[..3]), root), None);

        // And too *long* is the direction that matters, because it is the only
        // one an attacker chooses: [`STRUT_WORDS`] asks for thirteen words
        // precisely so an over-long property arrives here as thirteen values and
        // fails this conversion.
        let thirteen = [40, 0, 0, 0, 0, 1079, 0, 0, 0, 0, 0, 0, 99];
        assert_eq!(
            choose_strut(Some(&thirteen), Some(&legacy), root),
            Some(X11Strut::from_legacy(legacy, root)),
            "thirteen values are not a partial strut"
        );
        assert_eq!(choose_strut(Some(&thirteen), None, root), None);
        let five = [0, 0, 60, 0, 99];
        assert_eq!(choose_strut(None, Some(&five), root), None);
        // And neither is nothing at all.
        assert_eq!(choose_strut(None, None, root), None);

        // The rows above are the ones worth reading, each with the reason it
        // exists. They are a subset of the cells the grid below walks, and no
        // attempt is made here to say how big either set is or to characterise the
        // difference. Three versions tried. The first said the omissions were
        // "eight" and all of them a present partial against a malformed legacy; the
        // second corrected the tally to thirteen and named one omitted cell as "the
        // one" a mutation reddens, when it reddens four; the third left five live
        // numerals in this paragraph — twelve, twenty-five, thirteen, five, one —
        // in the very commit whose message was about deleting such numerals from
        // the paragraph above.
        //
        // Every one of them describes the *contents* of the two arrays at the end
        // of this test, which is where they can be counted by anyone who needs the
        // number and where they cannot go stale. The one fact worth keeping is
        // qualitative: round 24's mutation — adding
        // `.filter(X11Strut::reserves_anything)` to the legacy arm — reddens every
        // all-zero-legacy cell whose partial does not win, and no cell that the
        // readable rows above cover. That is why the grid earns its place.
        //
        // Rather than let the sentence outrun the assertions again, the whole
        // product is walked. The expectation is the rule itself, stated once:
        // a conformant partial wins whatever the legacy is; otherwise a valid
        // legacy wins; otherwise nothing.
        // The third column of each row is that row's expectation: for a partial,
        // the strut it wins with or `None`; for a legacy, the four words it decodes
        // to. The rule is then one line — take the partial's answer, else the
        // legacy's — and it reads only the row in front of it. Two earlier versions
        // did not: one keyed on the label "valid", which cannot tell a well-formed
        // empty legacy from a malformed one, and one carried a `bool` and reached
        // outside the row for the winner's value.
        //
        // The two columns are not equally independent, and an earlier version of
        // this comment called both "written out rather than computed". The legacy
        // column is raw wire words that the loop decodes; the conformant partial's
        // column calls `X11Strut::from_partial`, the implementation's own
        // constructor. So this grid pins the *precedence* rule rather than
        // `from_partial`'s field mapping, which is pinned separately by
        // `the_partial_field_order_matches_the_property`.
        let zero_legacy = [0_u32; 4];
        let partials: [PartialCase<'_>; 5] = [
            ("absent", None, None),
            (
                "conformant",
                Some(&partial),
                Some(X11Strut::from_partial(partial)),
            ),
            ("all zero", Some(&empty_partial), None),
            ("too short", Some(&partial[..11]), None),
            ("too long", Some(&thirteen), None),
        ];
        // Five values, not four. The first version of this grid gave the partial
        // axis an "all zero" row and the legacy axis none, which made it the
        // product of the *implementation's* equivalence classes rather than of
        // what the two properties can be — the difference being precisely that the
        // partial arm filters and the legacy arm does not. Adding
        // `.filter(X11Strut::reserves_anything)` to the legacy arm survived all
        // twenty cells and the whole file.
        let legacies: [LegacyCase<'_>; 5] = [
            ("absent", None, None),
            ("valid", Some(&legacy), Some(legacy)),
            ("all zero", Some(&zero_legacy), Some(zero_legacy)),
            ("too short", Some(&legacy[..3]), None),
            ("too long", Some(&five), None),
        ];
        for (partial_name, partial_value, wins_with) in partials {
            for (legacy_name, legacy_value, decoded) in legacies {
                let expected =
                    wins_with.or_else(|| decoded.map(|four| X11Strut::from_legacy(four, root)));
                assert_eq!(
                    choose_strut(partial_value, legacy_value, root),
                    expected,
                    "partial {partial_name}, legacy {legacy_name}"
                );
            }
        }
        // No `assert_eq!(cells, …)` here. An earlier version counted its
        // iterations and asserted the total, presented as the check that made the
        // comment's cell count verified rather than claimed. It is a compile-time
        // tautology: the two arrays have fixed-length types and the body is
        // unconditional, so the product is fixed on every possible execution and
        // changing an array's length in *either* direction is a compiler error
        // rather than a test failure. (An earlier note said only shrinking was.)
        // The grid's size is settled by the type system; what this loop checks is
        // the rule.
    }

    // -- which windowing system --------------------------------------------

    fn display_env(
        wayland_display: Option<&'static str>,
        wayland_socket: Option<&'static str>,
        display: Option<&'static str>,
    ) -> DisplayEnv<'static> {
        DisplayEnv {
            wayland_display,
            wayland_socket,
            display,
        }
    }

    #[test]
    fn wayland_wins_over_a_display_that_xwayland_also_set() {
        // Almost every Wayland session runs Xwayland and sets `DISPLAY`, so
        // preferring X11 when both are present would take Xwayland's view of the
        // screen — and, worse, would build an anchor for a window winit is going
        // to create on Wayland, where nothing can position it.
        assert_eq!(
            window_system(display_env(Some("wayland-1"), None, Some(":0"))),
            WindowSystem::Wayland
        );
        assert_eq!(
            window_system(display_env(None, None, Some(":0"))),
            WindowSystem::X11
        );
        assert_eq!(
            window_system(display_env(None, None, None)),
            WindowSystem::None
        );
    }

    #[test]
    fn an_inherited_wayland_socket_counts_as_a_wayland_session() {
        // The rule this shares with winit and not with the dimmer's own
        // transport check: a compositor that launches a client with a connected
        // file descriptor sets `WAYLAND_SOCKET` and need not set
        // `WAYLAND_DISPLAY`. Missing it would build an X11 anchor for a Wayland
        // window whenever a stale `DISPLAY` was also in the environment.
        assert_eq!(
            window_system(display_env(None, Some("12"), None)),
            WindowSystem::Wayland
        );
        assert_eq!(
            window_system(display_env(None, Some("12"), Some(":0"))),
            WindowSystem::Wayland
        );
    }

    #[test]
    fn an_empty_variable_is_treated_as_unset() {
        // `DISPLAY=` is what a login shell is left with when a session script
        // clears it; reading it as a server name produces a connect failure where
        // the truth is that there is no server.
        assert_eq!(
            window_system(display_env(Some(""), Some(""), Some(""))),
            WindowSystem::None
        );
        assert_eq!(
            window_system(display_env(Some(""), None, Some(":0"))),
            WindowSystem::X11
        );
        assert_eq!(
            window_system(display_env(Some(""), Some("12"), Some(":0"))),
            WindowSystem::Wayland
        );
    }

    #[test]
    fn both_backends_cap_an_absurd_extent_at_the_same_value() {
        // `mac_geometry` is compiled under `cfg(test)` on every lane, so unlike
        // the Windows-side version of this claim, this one can be checked
        // everywhere. Two different ceilings would mean `x + w` overflows `i32`
        // on one backend and not the other, and the placement kernel downstream
        // is written against a single space.
        assert_eq!(MAX_EXTENT, crate::mac_geometry::MAX_EXTENT);
    }

    #[test]
    fn an_extreme_layout_saturates_instead_of_wrapping() {
        // Not reachable from RandR, whose extents are `u16` — but the strut
        // arithmetic runs in `i64` and has to come back into the anchor's `i32`
        // space totally, whatever it was handed.
        let huge = WorkRect {
            x: i32::MIN,
            y: i32::MIN,
            w: u32::MAX,
            h: u32::MAX,
        };
        let work = work_area(huge, screen(u32::MAX, u32::MAX), &[]);
        assert_eq!(work.x, i32::MIN);
        assert_eq!(work.y, i32::MIN);
        assert_eq!(work.w, MAX_EXTENT);
        assert_eq!(work.h, MAX_EXTENT);
        // And the selection arithmetic survives the same rectangle.
        let monitors = [X11Monitor {
            bounds: huge,
            mm_width: u32::MAX,
            mm_height: u32::MAX,
        }];
        assert_eq!(monitor_for_cursor((i32::MAX, i32::MAX), &monitors), Some(0));
    }
}
