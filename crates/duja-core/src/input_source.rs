//! Human-readable names for MCCS VCP `0x60` input-source values.
//!
//! DDC/CI encodes the active input as a single byte written to VCP feature
//! `0x60`. The MCCS 2.2 standard fixes the common code points (`DisplayPort`,
//! HDMI, DVI, VGA, …); this module is the pure, total mapping between those
//! codes and short slugs Duja shows and accepts on the command line.
//!
//! # Example
//!
//! ```
//! use duja_core::input_source::{code_name, parse_input};
//!
//! assert_eq!(code_name(0x11), Some("hdmi1"));
//! assert_eq!(parse_input("DisplayPort"), Some(0x0F));
//! assert_eq!(parse_input("0x11"), Some(0x11));
//! assert_eq!(parse_input("17"), Some(0x11));
//! ```

// RATIONALE: the mapping table is the single source of truth; the slug/alias
// arrays deliberately mirror the MCCS wording, so the module reads best when its
// public helpers are qualified (`input_source::code_name`).
#![allow(clippy::module_name_repetitions)]

/// One MCCS input-source code point: its raw byte, canonical slug, and the
/// human-facing aliases that also parse to it.
struct InputCode {
    /// The raw VCP `0x60` value.
    code: u8,
    /// The canonical short slug Duja prints (e.g. `hdmi1`).
    slug: &'static str,
    /// Extra spellings that parse to this code (matched case-insensitively).
    aliases: &'static [&'static str],
}

/// The MCCS 2.2 input-source table Duja recognises (§ VCP 0x60 discrete values).
const INPUTS: &[InputCode] = &[
    InputCode {
        code: 0x01,
        slug: "vga1",
        aliases: &["vga", "analog1", "analog"],
    },
    InputCode {
        code: 0x02,
        slug: "vga2",
        aliases: &["analog2"],
    },
    InputCode {
        code: 0x03,
        slug: "dvi1",
        aliases: &["dvi"],
    },
    InputCode {
        code: 0x04,
        slug: "dvi2",
        aliases: &[],
    },
    InputCode {
        code: 0x0F,
        slug: "dp1",
        aliases: &["displayport", "displayport1", "dp"],
    },
    InputCode {
        code: 0x10,
        slug: "dp2",
        aliases: &["displayport2"],
    },
    InputCode {
        code: 0x11,
        slug: "hdmi1",
        aliases: &["hdmi"],
    },
    InputCode {
        code: 0x12,
        slug: "hdmi2",
        aliases: &[],
    },
    InputCode {
        code: 0x13,
        slug: "usbc",
        aliases: &["usb-c", "type-c", "typec"],
    },
];

/// The canonical slug for an input-source `code`, or `None` if it is not one of
/// the input codes Duja names.
///
/// An unknown but valid code is still writable — callers fall back to a numeric
/// label (see [`label`]).
#[must_use]
pub fn code_name(code: u8) -> Option<&'static str> {
    INPUTS
        .iter()
        .find(|entry| entry.code == code)
        .map(|entry| entry.slug)
}

/// A display label for `code`: its slug if known, else its hex value.
#[must_use]
pub fn label(code: u8) -> String {
    code_name(code).map_or_else(|| format!("{code:#04x}"), ToOwned::to_owned)
}

/// Parse a user-supplied input token into a raw code.
///
/// Accepts a canonical slug or alias (case-insensitively), a hex literal
/// (`0x11`), or a plain decimal (`17`). Returns `None` on anything else or an
/// out-of-range number.
#[must_use]
pub fn parse_input(token: &str) -> Option<u8> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Name / alias, case-insensitively.
    for entry in INPUTS {
        if entry.slug.eq_ignore_ascii_case(trimmed)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(trimmed))
        {
            return Some(entry.code);
        }
    }
    // Hex (0x..) or plain decimal.
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        return u8::from_str_radix(hex, 16).ok();
    }
    trimmed.parse::<u8>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes_round_trip_name_to_code() {
        for entry in INPUTS {
            let slug = code_name(entry.code).expect("known code names");
            assert_eq!(
                parse_input(slug),
                Some(entry.code),
                "slug {slug} must parse back"
            );
        }
    }

    #[test]
    fn common_codes_match_the_mccs_table() {
        assert_eq!(code_name(0x0F), Some("dp1"));
        assert_eq!(code_name(0x11), Some("hdmi1"));
        assert_eq!(code_name(0x12), Some("hdmi2"));
        assert_eq!(code_name(0x01), Some("vga1"));
        assert_eq!(code_name(0x03), Some("dvi1"));
    }

    #[test]
    fn parse_accepts_aliases_case_insensitively() {
        assert_eq!(parse_input("HDMI"), Some(0x11));
        assert_eq!(parse_input("DisplayPort"), Some(0x0F));
        assert_eq!(parse_input("Dp"), Some(0x0F));
        assert_eq!(parse_input("usb-c"), Some(0x13));
    }

    #[test]
    fn parse_accepts_hex_and_decimal() {
        assert_eq!(parse_input("0x11"), Some(0x11));
        assert_eq!(parse_input("0X0F"), Some(0x0F));
        assert_eq!(parse_input("17"), Some(0x11));
        assert_eq!(parse_input("15"), Some(0x0F));
        // A valid but unnamed code still parses (any byte is writable).
        assert_eq!(parse_input("0x20"), Some(0x20));
        assert_eq!(code_name(0x20), None);
    }

    #[test]
    fn parse_rejects_garbage_and_out_of_range() {
        assert_eq!(parse_input(""), None);
        assert_eq!(parse_input("  "), None);
        assert_eq!(parse_input("nope"), None);
        assert_eq!(parse_input("256"), None); // beyond a byte
        assert_eq!(parse_input("0x100"), None);
    }

    #[test]
    fn label_falls_back_to_hex_for_unknown_codes() {
        assert_eq!(label(0x11), "hdmi1");
        assert_eq!(label(0x20), "0x20");
    }

    #[test]
    fn slugs_and_codes_are_unique() {
        let mut codes: Vec<u8> = INPUTS.iter().map(|e| e.code).collect();
        codes.sort_unstable();
        let before = codes.len();
        codes.dedup();
        assert_eq!(before, codes.len(), "duplicate input codes");

        let mut slugs: Vec<&str> = INPUTS.iter().map(|e| e.slug).collect();
        slugs.sort_unstable();
        let before = slugs.len();
        slugs.dedup();
        assert_eq!(before, slugs.len(), "duplicate slugs");
    }
}
