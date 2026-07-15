//! The single source of truth for Duja's accent colours.
//!
//! One table drives *everything* the accent touches: the Slint `Palette` (pushed
//! by both window shells) and both application icons — the taskbar/alt-tab window
//! icon here in `duja-ui`, and the notification-area tray icon over in `duja-app`.
//! Before this module the ruby was written twice, once in `flyout.slint` and again
//! as a hand-copied `ICON_RGB` constant in Rust, with nothing tying them together.
//!
//! Deliberately **Slint-free**: colours are plain RGBA bytes, so `duja-app` can
//! share the table for its tray icon without either crate having to name the
//! other's icon type (`tray-icon` lives only in `duja-app`, the winit backend only
//! here). The shells convert to `slint::Color` at the boundary.

/// An 8-bit RGBA colour.
pub type Rgba = [u8; 4];

/// The user-selectable accent colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccentChoice {
    /// The original — a warm coral red.
    #[default]
    Ruby,
    /// Deep bronze in light, warm amber in dark.
    Gold,
    /// A deep green; mint in dark.
    Emerald,
    /// Navy in light, lifted to azure in dark (a true navy would vanish on a
    /// near-black surface).
    Sapphire,
    /// Adaptive monochrome: near-black in light, near-white in dark. This is why
    /// there is no separate "black" and "white" — either one alone would be
    /// invisible in the theme that matches it.
    Onyx,
}

/// The selector order. **Must** match the settings `ComboBox` model, which maps a
/// row index straight back through this table.
pub const ACCENT_ORDER: [AccentChoice; 5] = [
    AccentChoice::Ruby,
    AccentChoice::Gold,
    AccentChoice::Emerald,
    AccentChoice::Sapphire,
    AccentChoice::Onyx,
];

/// One accent resolved against a theme — the four colours the `Palette` needs.
///
/// `thumb-glow` and `focus-ring` are *not* here: they stay declarative in the
/// markup (`thumb-glow: accent`, `focus-ring: dark ? accent-hover : accent`), so
/// the light/dark asymmetry lives where the rest of the theme rules live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccentColors {
    /// The accent itself: slider fill, a checked pill, the recording highlight.
    pub base: Rgba,
    /// The bright end of the slider's fill gradient, and the hover state.
    pub bright: Rgba,
    /// `base` at a low alpha — the sub-floor (software-dimming) track zone.
    pub wash: Rgba,
    /// The foreground drawn **on top of** a `base` fill (the pill knob, a primary
    /// button's label). White is unreadable on every light-luminance accent — it
    /// scores under 2:1 on Gold, Emerald and Onyx in dark — so this is a
    /// per-(accent, theme) choice rather than a constant.
    pub on: Rgba,
}

/// The sub-floor wash reads a touch stronger against the darker track.
const WASH_ALPHA_DARK: u8 = 0x4d;
/// The sub-floor wash alpha on the lighter track.
const WASH_ALPHA_LIGHT: u8 = 0x33;

/// The light foreground for accents dark enough to carry it.
const FG_WHITE: Rgba = [0xff, 0xff, 0xff, 0xff];
/// The ink foreground for accents too light for white (Gold/Emerald/Sapphire/Onyx
/// in dark). Matches the light theme's `text` so it reads as deliberate.
const FG_INK: Rgba = [0x1a, 0x17, 0x1a, 0xff];

/// Resolve an accent against a theme.
///
/// Ruby reproduces the palette that shipped in #39 exactly, so selecting it is a
/// visual no-op against the previous release.
#[must_use]
pub fn resolve(choice: AccentChoice, dark: bool) -> AccentColors {
    let (base, bright, on) = match (choice, dark) {
        (AccentChoice::Ruby, true) => (rgb(0xf2, 0x55, 0x5a), rgb(0xff, 0x6d, 0x72), FG_WHITE),
        (AccentChoice::Ruby, false) => (rgb(0xe5, 0x48, 0x4d), rgb(0xef, 0x5b, 0x60), FG_WHITE),
        (AccentChoice::Gold, true) => (rgb(0xf0, 0xb4, 0x29), rgb(0xff, 0xc9, 0x4d), FG_INK),
        (AccentChoice::Gold, false) => (rgb(0x9a, 0x6f, 0x00), rgb(0xb0, 0x7f, 0x04), FG_WHITE),
        (AccentChoice::Emerald, true) => (rgb(0x3d, 0xd6, 0x8c), rgb(0x5c, 0xe6, 0xa4), FG_INK),
        (AccentChoice::Emerald, false) => (rgb(0x0d, 0x8a, 0x58), rgb(0x0f, 0x9d, 0x64), FG_WHITE),
        (AccentChoice::Sapphire, true) => (rgb(0x5b, 0x9c, 0xf8), rgb(0x7b, 0xb0, 0xff), FG_INK),
        (AccentChoice::Sapphire, false) => (rgb(0x2f, 0x6f, 0xeb), rgb(0x46, 0x80, 0xf0), FG_WHITE),
        (AccentChoice::Onyx, true) => (rgb(0xe8, 0xe6, 0xea), rgb(0xf7, 0xf5, 0xf8), FG_INK),
        (AccentChoice::Onyx, false) => (rgb(0x2b, 0x26, 0x2b), rgb(0x3c, 0x36, 0x3c), FG_WHITE),
    };
    let alpha = if dark {
        WASH_ALPHA_DARK
    } else {
        WASH_ALPHA_LIGHT
    };
    AccentColors {
        base,
        bright,
        wash: with_alpha(base, alpha),
        on,
    }
}

/// The icon colour for an accent — **theme-independent**, so switching light/dark
/// never touches the icons.
///
/// This is not simply `resolve(..).base`: the icons sit on the Windows taskbar,
/// whose theme is independent of the app's, so each colour is tuned to clear 3:1
/// against a light **and** a dark taskbar. Onyx therefore lands on a mid graphite
/// — its near-black/near-white accents would each vanish on one of them.
#[must_use]
pub fn icon_rgb(choice: AccentChoice) -> [u8; 3] {
    match choice {
        AccentChoice::Ruby => [0xf2, 0x55, 0x5a],
        AccentChoice::Gold => [0xb8, 0x81, 0x1a],
        AccentChoice::Emerald => [0x14, 0x9a, 0x63],
        AccentChoice::Sapphire => [0x3d, 0x7f, 0xef],
        AccentChoice::Onyx => [0x8b, 0x86, 0x91],
    }
}

/// An opaque colour.
const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r, g, b, 0xff]
}

/// The same colour at a different alpha.
const fn with_alpha(colour: Rgba, alpha: u8) -> Rgba {
    let [r, g, b, _] = colour;
    [r, g, b, alpha]
}

#[cfg(test)]
mod tests {
    use super::{
        ACCENT_ORDER, AccentChoice, FG_INK, FG_WHITE, Rgba, icon_rgb, resolve, rgb, with_alpha,
    };

    /// The surfaces an `accent` is drawn against as text/border (`Palette.surface`).
    const SURFACE_DARK: Rgba = rgb(0x26, 0x23, 0x27);
    const SURFACE_LIGHT: Rgba = rgb(0xff, 0xff, 0xff);
    /// Reference Windows taskbars, light and dark.
    const TASKBAR_LIGHT: Rgba = rgb(0xf3, 0xf3, 0xf3);
    const TASKBAR_DARK: Rgba = rgb(0x1f, 0x1f, 0x1f);

    /// One sRGB channel, gamma-expanded to linear light.
    fn linear(channel: u8) -> f32 {
        let c = f32::from(channel) / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG relative luminance of an opaque sRGB colour.
    fn luminance(colour: Rgba) -> f32 {
        let [r, g, b, _] = colour;
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    /// WCAG contrast ratio between two opaque colours (1.0..=21.0).
    fn contrast(a: Rgba, b: Rgba) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// Every (accent, theme) cell.
    fn cells() -> impl Iterator<Item = (AccentChoice, bool)> {
        ACCENT_ORDER
            .into_iter()
            .flat_map(|a| [(a, true), (a, false)])
    }

    #[test]
    fn ruby_reproduces_todays_palette_bit_for_bit() {
        // The no-visual-regression fence: these are the literals that shipped in
        // #39. If this breaks, the previous release's look has changed.
        let dark = resolve(AccentChoice::Ruby, true);
        assert_eq!(dark.base, rgb(0xf2, 0x55, 0x5a));
        assert_eq!(dark.bright, rgb(0xff, 0x6d, 0x72));
        assert_eq!(dark.wash, with_alpha(rgb(0xf2, 0x55, 0x5a), 0x4d));
        assert_eq!(dark.on, FG_WHITE);

        let light = resolve(AccentChoice::Ruby, false);
        assert_eq!(light.base, rgb(0xe5, 0x48, 0x4d));
        assert_eq!(light.bright, rgb(0xef, 0x5b, 0x60));
        assert_eq!(light.wash, with_alpha(rgb(0xe5, 0x48, 0x4d), 0x33));
        assert_eq!(light.on, FG_WHITE);
    }

    #[test]
    fn wash_is_the_base_at_the_theme_alpha() {
        for (accent, dark) in cells() {
            let c = resolve(accent, dark);
            let ([br, bg, bb, _], [wr, wg, wb, wa]) = (c.base, c.wash);
            assert_eq!([br, bg, bb], [wr, wg, wb], "{accent:?} dark={dark}");
            assert_eq!(wa, if dark { 0x4d } else { 0x33 }, "{accent:?} dark={dark}");
        }
    }

    #[test]
    fn base_bright_and_on_are_opaque() {
        for (accent, dark) in cells() {
            let c = resolve(accent, dark);
            for ([_, _, _, a], name) in [(c.base, "base"), (c.bright, "bright"), (c.on, "on")] {
                assert_eq!(a, 0xff, "{accent:?} dark={dark} {name}");
            }
        }
    }

    #[test]
    fn bright_is_brighter_than_base() {
        // The hover/gradient contract: `bright` is the lit end of the fill.
        for (accent, dark) in cells() {
            let c = resolve(accent, dark);
            assert!(
                luminance(c.bright) > luminance(c.base),
                "{accent:?} dark={dark}"
            );
        }
    }

    #[test]
    fn on_contrasts_with_its_accent() {
        // The test that catches the white-on-Gold trap: white scores 1.86:1 on a
        // dark-theme Gold fill, so `on` must flip to ink there. The bar is Ruby's
        // own historical white-on-ruby ratio (~3.37), the tightest cell we ship.
        for (accent, dark) in cells() {
            let c = resolve(accent, dark);
            let on_base = contrast(c.on, c.base);
            assert!(
                on_base >= 3.3,
                "{accent:?} dark={dark}: on/base only {on_base:.2}:1"
            );
            let on_bright = contrast(c.on, c.bright);
            assert!(
                on_bright >= 2.7,
                "{accent:?} dark={dark}: on/bright only {on_bright:.2}:1"
            );
        }
    }

    #[test]
    fn base_reads_on_its_surface() {
        // `accent` doubles as text/border on `surface` (the hotkey "recording"
        // row), which is why Gold-light is a deep bronze and not a bright amber.
        for (accent, dark) in cells() {
            let c = resolve(accent, dark);
            let surface = if dark { SURFACE_DARK } else { SURFACE_LIGHT };
            let ratio = contrast(c.base, surface);
            assert!(
                ratio >= 3.85,
                "{accent:?} dark={dark}: base/surface only {ratio:.2}:1"
            );
        }
    }

    #[test]
    fn icon_rgb_reads_on_both_taskbars() {
        // The icons sit on a taskbar whose theme we do not control, so every
        // accent's icon colour must clear 3:1 against a light AND a dark one.
        for accent in ACCENT_ORDER {
            let [r, g, b] = icon_rgb(accent);
            let icon = rgb(r, g, b);
            let light = contrast(icon, TASKBAR_LIGHT);
            let dark = contrast(icon, TASKBAR_DARK);
            assert!(
                light >= 3.0,
                "{accent:?}: only {light:.2}:1 on a light taskbar"
            );
            assert!(
                dark >= 3.0,
                "{accent:?}: only {dark:.2}:1 on a dark taskbar"
            );
        }
    }

    #[test]
    fn ruby_icon_is_unchanged() {
        // The window icon must not move: only the tray icon changes shape.
        assert_eq!(icon_rgb(AccentChoice::Ruby), [0xf2, 0x55, 0x5a]);
    }

    #[test]
    fn accent_order_is_complete_and_distinct() {
        assert_eq!(ACCENT_ORDER.len(), 5);
        for (i, a) in ACCENT_ORDER.into_iter().enumerate() {
            for b in ACCENT_ORDER.into_iter().skip(i.saturating_add(1)) {
                assert_ne!(a, b, "duplicate accent in ACCENT_ORDER");
            }
        }
        assert_eq!(AccentChoice::default(), AccentChoice::Ruby);
    }

    #[test]
    fn onyx_inverts_luminance_across_themes() {
        // The whole point of Onyx: it is near-white on dark and near-black on
        // light, so a monochrome accent is never invisible.
        let dark = resolve(AccentChoice::Onyx, true);
        let light = resolve(AccentChoice::Onyx, false);
        assert!(luminance(dark.base) > 0.5);
        assert!(luminance(light.base) < 0.1);
        assert_eq!(dark.on, FG_INK);
        assert_eq!(light.on, FG_WHITE);
    }
}
