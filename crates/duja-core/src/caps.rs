//! MCCS (Monitor Control Command Set) capability-string parser.
//!
//! A DDC/CI monitor answers a *capabilities request* with a parenthesised,
//! VESA-defined string describing its protocol, model, supported VCP feature
//! codes and — for non-continuous features — the discrete values each accepts.
//! A real example from an MSI MP273QP:
//!
//! ```text
//! (prot(monitor)type(lcd)model(MP273QP)cmds(010203070C4EF3E3)vcp(...60(11120F)...)mccs_ver(2.1))
//! ```
//!
//! [`ParsedCaps::parse`] turns that into structured data. The parser is
//! **total**: it is a fuzz target, so every input — truncated, unbalanced,
//! adversarially nested, or megabytes long — yields either an `Ok` value or a
//! typed [`CapsError`], never a panic, never unbounded recursion, and never a
//! slice-index out of bounds. Inputs above [`MAX_CAPS_LEN`] are rejected before
//! any parsing work is done.
//!
//! # Example
//!
//! ```
//! use duja_core::caps::ParsedCaps;
//!
//! # fn main() -> Result<(), duja_core::caps::CapsError> {
//! let caps = ParsedCaps::parse("(model(FOO)vcp(10 60(1112)))")?;
//! assert_eq!(caps.model.as_deref(), Some("FOO"));
//! assert!(caps.supports(0x10)); // brightness, continuous
//! assert_eq!(caps.allowed_values(0x10), None);
//! assert_eq!(caps.allowed_values(0x60), Some(&[0x11u8, 0x12][..]));
//! # Ok(())
//! # }
//! ```

// RATIONALE: the parser's public vocabulary (`CapsError`, `ParsedCaps`,
// `MAX_CAPS_LEN`) intentionally shares the `caps` module stem; the fully
// qualified names read best at call sites and the surface is frozen by the plan.
#![allow(clippy::module_name_repetitions)]

use std::collections::BTreeMap;

use crate::model::{Capabilities, Feature};

/// The largest capability string [`ParsedCaps::parse`] will accept: 64 KiB.
///
/// The cap is checked before any parsing so a hostile or corrupt blob cannot
/// force large allocations or long scans. Real capability strings are well
/// under 1 KiB.
pub const MAX_CAPS_LEN: usize = 64 * 1024;

/// The deepest parenthesis nesting the parser will descend before failing with
/// [`CapsError::TooDeep`].
///
/// Well-formed capability strings nest at most three levels
/// (outer → section → value list); the generous cap bounds work on pathological
/// input without any recursion.
pub const MAX_CAPS_DEPTH: usize = 32;

/// A failure encountered while parsing an MCCS capability string.
///
/// Parsing is total: every malformed input yields one of these variants rather
/// than panicking (the parser is a fuzz target).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapsError {
    /// The input exceeded the [`MAX_CAPS_LEN`] safety cap.
    #[error("capability string exceeds the size limit ({len} bytes)")]
    TooLarge {
        /// The rejected input's length, in bytes.
        len: usize,
    },
    /// A parenthesis was missing, mismatched, or never closed.
    #[error("capability string has missing or unbalanced parentheses")]
    Unbalanced,
    /// Parentheses nested deeper than [`MAX_CAPS_DEPTH`].
    #[error("capability string nests parentheses too deeply")]
    TooDeep,
}

/// The structured result of parsing an MCCS capability string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedCaps {
    /// The `model(...)` value, trimmed, if present and non-empty.
    pub model: Option<String>,
    /// The `(major, minor)` MCCS version from `mccs_ver(...)`, if parseable.
    pub mccs_version: Option<(u8, u8)>,
    /// The VCP codes the display reports, each mapped to its allowed values.
    ///
    /// A value of `Some(list)` is the parenthesised value list that followed a
    /// non-continuous feature code (e.g. input sources after `0x60`); `None`
    /// marks a continuous feature, or one with no explicit list.
    pub vcp: BTreeMap<u8, Option<Vec<u8>>>,
}

impl ParsedCaps {
    /// Parse an MCCS capability string.
    ///
    /// Whitespace between tokens is tolerated, hex pairs are decoded
    /// case-insensitively, and unknown top-level sections (`prot`, `type`,
    /// `cmds`, …) are skipped. See the [module docs](self) for the guarantees.
    ///
    /// # Errors
    /// - [`CapsError::TooLarge`] if the input is longer than [`MAX_CAPS_LEN`].
    /// - [`CapsError::Unbalanced`] if the parentheses are missing or mismatched.
    /// - [`CapsError::TooDeep`] if nesting exceeds [`MAX_CAPS_DEPTH`].
    pub fn parse(input: &str) -> Result<Self, CapsError> {
        if input.len() > MAX_CAPS_LEN {
            return Err(CapsError::TooLarge { len: input.len() });
        }
        let mut cur = Cursor::new(input.as_bytes());
        cur.skip_ws();
        if cur.bump() != Some(b'(') {
            return Err(CapsError::Unbalanced);
        }
        let mut caps = ParsedCaps::default();
        parse_sections(&mut cur, &mut caps)?;
        Ok(caps)
    }

    /// Whether the display reports VCP `code` at all.
    #[must_use]
    pub fn supports(&self, code: u8) -> bool {
        self.vcp.contains_key(&code)
    }

    /// The discrete allowed values for VCP `code`, or `None` if the code is
    /// continuous, has no explicit list, or is not reported.
    #[must_use]
    pub fn allowed_values(&self, code: u8) -> Option<&[u8]> {
        self.vcp.get(&code).and_then(Option::as_deref)
    }

    /// Map the parsed VCP codes onto the core [`Capabilities`] model.
    ///
    /// Only the codes for known [`Feature`]s are carried across.
    /// [`Capabilities::hardware_range`] is set when brightness (`0x10`) is
    /// present. [`Capabilities::allowed_inputs`] carries the `0x60` value list
    /// (empty when input source is unsupported or listed without discrete
    /// values). [`Capabilities::raw_capabilities`] is left `None` — the backend
    /// that captured the string fills it in.
    #[must_use]
    pub fn to_capabilities(&self) -> Capabilities {
        let mut features = std::collections::BTreeSet::new();
        for feature in Feature::ALL {
            if self.vcp.contains_key(&feature.vcp_code()) {
                features.insert(feature);
            }
        }
        let hardware_range = features.contains(&Feature::Brightness);
        let allowed_inputs = self
            .allowed_values(Feature::InputSource.vcp_code())
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        Capabilities {
            features,
            hardware_range,
            raw_capabilities: None,
            allowed_inputs,
        }
    }
}

/// A forward-only byte cursor. Every read is bounds-checked (`get`) and every
/// advance saturates, so the parser can never index out of bounds or overflow.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    /// The byte at the cursor, or `None` at end of input.
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// The byte `offset` positions ahead of the cursor, without advancing.
    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos.saturating_add(offset)).copied()
    }

    /// Return the byte at the cursor and advance past it.
    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek();
        if byte.is_some() {
            self.pos = self.pos.saturating_add(1);
        }
        byte
    }

    /// Advance past any run of ASCII whitespace.
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b) if b.is_ascii_whitespace()) {
            self.pos = self.pos.saturating_add(1);
        }
    }
}

/// Parse the `key(value)` sections inside the outer parentheses, dispatching the
/// ones we understand and skipping the rest, until the matching `)` is consumed.
///
/// Fully iterative: nesting is tracked by [`capture_balanced`]'s depth counter,
/// never the call stack.
fn parse_sections(cur: &mut Cursor<'_>, caps: &mut ParsedCaps) -> Result<(), CapsError> {
    loop {
        cur.skip_ws();
        match cur.peek() {
            None => return Err(CapsError::Unbalanced),
            Some(b')') => {
                cur.bump();
                return Ok(());
            }
            Some(b'(') => {
                // An anonymous group with no key: consume and ignore it.
                cur.bump();
                capture_balanced(cur)?;
            }
            Some(c) if is_key_byte(c) => {
                let key = read_key(cur);
                cur.skip_ws();
                if cur.peek() == Some(b'(') {
                    cur.bump();
                    let inner = capture_balanced(cur)?;
                    apply_section(caps, key, inner);
                }
                // A bare key with no `(value)` is skipped.
            }
            Some(_) => {
                // Stray punctuation between sections: skip one byte and retry.
                cur.bump();
            }
        }
    }
}

/// Read a run of key bytes (`[A-Za-z0-9_]`) and return it, borrowed from the
/// input. The lifetime ties to the input, not the mutable cursor borrow.
fn read_key<'a>(cur: &mut Cursor<'a>) -> &'a [u8] {
    let start = cur.pos;
    while matches!(cur.peek(), Some(b) if is_key_byte(b)) {
        cur.pos = cur.pos.saturating_add(1);
    }
    cur.bytes.get(start..cur.pos).unwrap_or(&[])
}

/// Scan from just after an opening `(` to its matching `)`, tracking nesting
/// depth, and return the inner bytes (excluding both parentheses). The matching
/// `)` is consumed.
///
/// # Errors
/// - [`CapsError::Unbalanced`] if input ends before the group closes.
/// - [`CapsError::TooDeep`] if nesting exceeds [`MAX_CAPS_DEPTH`].
fn capture_balanced<'a>(cur: &mut Cursor<'a>) -> Result<&'a [u8], CapsError> {
    let start = cur.pos;
    let mut depth: usize = 1;
    loop {
        match cur.bump() {
            None => return Err(CapsError::Unbalanced),
            Some(b'(') => {
                depth = depth.saturating_add(1);
                if depth > MAX_CAPS_DEPTH {
                    return Err(CapsError::TooDeep);
                }
            }
            Some(b')') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let end = cur.pos.saturating_sub(1);
                    return Ok(cur.bytes.get(start..end).unwrap_or(&[]));
                }
            }
            Some(_) => {}
        }
    }
}

/// Route a recognised section's inner bytes into the accumulating result.
fn apply_section(caps: &mut ParsedCaps, key: &[u8], inner: &[u8]) {
    if key.eq_ignore_ascii_case(b"model") {
        if caps.model.is_none() {
            let text = String::from_utf8_lossy(inner);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                caps.model = Some(trimmed.to_owned());
            }
        }
    } else if key.eq_ignore_ascii_case(b"mccs_ver") {
        if caps.mccs_version.is_none() {
            caps.mccs_version = parse_mccs_version(inner);
        }
    } else if key.eq_ignore_ascii_case(b"vcp") {
        parse_vcp(inner, &mut caps.vcp);
    }
    // Every other section is intentionally ignored.
}

/// Parse a `major.minor` MCCS version. A bare `major` defaults minor to `0`;
/// anything non-numeric yields `None`.
fn parse_mccs_version(inner: &[u8]) -> Option<(u8, u8)> {
    let text = String::from_utf8_lossy(inner);
    let trimmed = text.trim();
    let (major_str, minor_str) = trimmed.split_once('.').unwrap_or((trimmed, "0"));
    let major = major_str.trim().parse::<u8>().ok()?;
    let minor = minor_str.trim().parse::<u8>().ok()?;
    Some((major, minor))
}

/// Parse the body of a `vcp(...)` section: a run of hex-pair feature codes, each
/// optionally followed by a parenthesised list of allowed values.
fn parse_vcp(inner: &[u8], out: &mut BTreeMap<u8, Option<Vec<u8>>>) {
    let mut cur = Cursor::new(inner);
    let mut last_code: Option<u8> = None;
    loop {
        cur.skip_ws();
        match cur.peek() {
            None => break,
            Some(b'(') => {
                cur.bump();
                let Ok(group) = capture_balanced(&mut cur) else {
                    // Balance was already guaranteed by the caller; on the
                    // impossible mismatch, stop and keep what we have.
                    break;
                };
                let values = parse_hex_bytes(group);
                if let Some(code) = last_code.take() {
                    out.insert(code, Some(values));
                }
            }
            Some(c) if c.is_ascii_hexdigit() => {
                if let Some(code) = read_hex_pair(&mut cur) {
                    out.entry(code).or_insert(None);
                    last_code = Some(code);
                } else {
                    // A lone trailing hex digit: skip it.
                    cur.bump();
                    last_code = None;
                }
            }
            Some(_) => {
                cur.bump();
                last_code = None;
            }
        }
    }
}

/// Collect every hex pair in `bytes`, skipping any non-hex bytes between them.
fn parse_hex_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut cur = Cursor::new(bytes);
    let mut out = Vec::new();
    loop {
        cur.skip_ws();
        if cur.peek().is_none() {
            break;
        }
        if let Some(value) = read_hex_pair(&mut cur) {
            out.push(value);
        } else {
            cur.bump();
        }
    }
    out
}

/// Read exactly two hex digits into a byte, consuming both only on success.
fn read_hex_pair(cur: &mut Cursor<'_>) -> Option<u8> {
    let hi = cur.peek().and_then(hex_val)?;
    let lo = cur.peek_at(1).and_then(hex_val)?;
    cur.bump();
    cur.bump();
    Some(hi.wrapping_mul(16).wrapping_add(lo))
}

/// Decode a single hex digit to its `0..=15` value.
fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte.wrapping_sub(b'0')),
        b'a'..=b'f' => Some(byte.wrapping_sub(b'a').wrapping_add(10)),
        b'A'..=b'F' => Some(byte.wrapping_sub(b'A').wrapping_add(10)),
        _ => None,
    }
}

/// Whether `byte` may appear in a section key (`[A-Za-z0-9_]`).
fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real capability string from our MSI MP273QP (docs/adr/0002).
    const MSI_MP273QP: &str = "(prot(monitor)type(lcd)model(MP273QP)cmds(010203070C4EF3E3)vcp(020405080B0C101214(0506080B)16181A6C6E70ACAEB6C0C6C8C9CACC(0102030405060708090A0C0D0F18192325)D6(0104)DF60(11120F)628D(0102)FF)mswhql(1)mccs_ver(2.1)asset_eep(32)mpu_ver(01))";

    #[test]
    fn caps_parser_handles_real_world_samples() {
        let caps = ParsedCaps::parse(MSI_MP273QP).expect("MSI fixture must parse");

        // Model and MCCS version are lifted from their sections.
        assert_eq!(caps.model.as_deref(), Some("MP273QP"));
        assert_eq!(caps.mccs_version, Some((2, 1)));

        // Exactly the 30 VCP codes the monitor advertises.
        assert_eq!(caps.vcp.len(), 30);

        // A non-continuous code carries its parenthesised value list...
        assert_eq!(caps.allowed_values(0x60), Some(&[0x11, 0x12, 0x0F][..]));
        assert_eq!(
            caps.allowed_values(0x14),
            Some(&[0x05, 0x06, 0x08, 0x0B][..])
        );
        assert_eq!(caps.allowed_values(0xD6), Some(&[0x01, 0x04][..]));
        assert_eq!(caps.allowed_values(0x8D), Some(&[0x01, 0x02][..]));

        // ...while a continuous code (brightness) has no list.
        assert!(caps.supports(0x10));
        assert_eq!(caps.allowed_values(0x10), None);
        assert!(caps.supports(0xFF));
        assert_eq!(caps.allowed_values(0xFF), None);

        // A code the monitor does not list is unsupported.
        assert!(!caps.supports(0x99));
        assert_eq!(caps.allowed_values(0x99), None);

        // The big CC list is decoded fully (17 values).
        assert_eq!(caps.allowed_values(0xCC).map(<[u8]>::len), Some(17));
    }

    #[test]
    fn caps_parser_maps_to_core_capabilities() {
        let caps = ParsedCaps::parse(MSI_MP273QP).expect("fixture parses");
        let core = caps.to_capabilities();
        assert!(core.supports(Feature::Brightness)); // 0x10
        assert!(core.supports(Feature::Contrast)); // 0x12
        assert!(core.supports(Feature::InputSource)); // 0x60
        assert!(core.hardware_range);
        assert_eq!(core.raw_capabilities, None);
        // The 0x60 value list is carried onto allowed_inputs.
        assert_eq!(core.allowed_inputs, vec![0x11, 0x12, 0x0F]);
    }

    #[test]
    fn caps_without_brightness_is_not_hardware_ranged() {
        let caps = ParsedCaps::parse("(vcp(60(11)))").expect("parses");
        let core = caps.to_capabilities();
        assert!(core.supports(Feature::InputSource));
        assert!(!core.supports(Feature::Brightness));
        assert!(!core.hardware_range);
        assert_eq!(core.allowed_inputs, vec![0x11]);
    }

    #[test]
    fn caps_input_source_without_value_list_has_no_allowed_inputs() {
        // 0x60 present as a bare code (no parenthesised list): supported, but no
        // discrete values are known.
        let caps = ParsedCaps::parse("(vcp(10 60))").expect("parses");
        let core = caps.to_capabilities();
        assert!(core.supports(Feature::InputSource));
        assert!(core.allowed_inputs.is_empty());
    }

    #[test]
    fn caps_parser_parses_mccs_version_variants() {
        assert_eq!(
            ParsedCaps::parse("(mccs_ver(2.1))")
                .expect("parses")
                .mccs_version,
            Some((2, 1))
        );
        assert_eq!(
            ParsedCaps::parse("(mccs_ver(2.0))")
                .expect("parses")
                .mccs_version,
            Some((2, 0))
        );
        // A bare major number defaults minor to zero.
        assert_eq!(
            ParsedCaps::parse("(mccs_ver(3))")
                .expect("parses")
                .mccs_version,
            Some((3, 0))
        );
        // Garbage version is dropped, not fatal.
        assert_eq!(
            ParsedCaps::parse("(mccs_ver(x.y))")
                .expect("parses")
                .mccs_version,
            None
        );
    }

    #[test]
    fn caps_parser_tolerates_whitespace_and_case() {
        let caps = ParsedCaps::parse("  ( MODEL ( Foo )  vcp ( 10 6C aB ) ) ")
            .expect("whitespace-padded input parses");
        assert_eq!(caps.model.as_deref(), Some("Foo"));
        assert!(caps.supports(0x10));
        assert!(caps.supports(0x6C));
        assert!(caps.supports(0xAB)); // lowercase hex decoded
    }

    #[test]
    fn caps_parser_skips_unknown_sections() {
        let caps = ParsedCaps::parse("(prot(monitor)type(lcd)whatever(xyz)vcp(10)mpu_ver(01))")
            .expect("parses");
        assert_eq!(caps.vcp.len(), 1);
        assert!(caps.supports(0x10));
        assert_eq!(caps.model, None);
    }

    #[test]
    fn caps_parser_rejects_unbalanced_parens() {
        assert_eq!(ParsedCaps::parse("(vcp(10"), Err(CapsError::Unbalanced));
        assert_eq!(
            ParsedCaps::parse("(prot(monitor)"),
            Err(CapsError::Unbalanced)
        );
        assert_eq!(ParsedCaps::parse(")"), Err(CapsError::Unbalanced));
        assert_eq!(ParsedCaps::parse(""), Err(CapsError::Unbalanced));
        // No outer parenthesis at all.
        assert_eq!(ParsedCaps::parse("vcp(10)"), Err(CapsError::Unbalanced));
    }

    #[test]
    fn caps_parser_rejects_deep_nesting() {
        let deep = "(".repeat(MAX_CAPS_DEPTH + 8);
        assert_eq!(ParsedCaps::parse(&deep), Err(CapsError::TooDeep));
    }

    #[test]
    fn caps_parser_rejects_oversized_input() {
        let mut huge = String::with_capacity(MAX_CAPS_LEN + 16);
        huge.push('(');
        for _ in 0..=MAX_CAPS_LEN {
            huge.push('a');
        }
        huge.push(')');
        let len = huge.len();
        assert!(len > MAX_CAPS_LEN);
        assert_eq!(ParsedCaps::parse(&huge), Err(CapsError::TooLarge { len }));
    }

    #[test]
    fn caps_parser_accepts_input_at_the_size_limit() {
        // Exactly MAX_CAPS_LEN bytes must be accepted (boundary is inclusive).
        let mut at_limit = String::from("(vcp(10)");
        while at_limit.len() < MAX_CAPS_LEN - 1 {
            at_limit.push(' ');
        }
        at_limit.push(')');
        assert_eq!(at_limit.len(), MAX_CAPS_LEN);
        assert!(ParsedCaps::parse(&at_limit).is_ok());
    }

    #[test]
    fn caps_parser_handles_empty_vcp_and_model() {
        let caps = ParsedCaps::parse("(vcp()model())").expect("parses");
        assert!(caps.vcp.is_empty());
        assert_eq!(caps.model, None); // empty model is dropped
    }

    use proptest::prelude::*;

    /// Strings drawn from a paren/hex-heavy alphabet, to stress balance
    /// tracking, nesting limits and hex-pair scanning far more than uniform
    /// random text. Full-range bytes are covered by the companion proptest;
    /// this maps raw bytes through a small alphabet (O(1) per byte) so 10k
    /// cases stay fast.
    fn caps_fragment() -> impl Strategy<Value = String> {
        // Exactly 32 bytes, so a byte can be mapped in with a 5-bit mask
        // (no modulo, which `arithmetic_side_effects` would flag).
        const ALPHABET: &[u8; 32] = b"()   \t\n_xz0123456789abcdefABCDEF";
        prop::collection::vec(any::<u8>(), 0..400).prop_map(|bytes| {
            let mapped: Vec<u8> = bytes
                .iter()
                .map(|b| {
                    let idx = usize::from(*b) & 0x1F;
                    ALPHABET.get(idx).copied().unwrap_or(b' ')
                })
                .collect();
            String::from_utf8_lossy(&mapped).into_owned()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// The parser must never panic and never run away on arbitrary input.
        #[test]
        fn caps_parse_never_panics(s in caps_fragment()) {
            let _ = ParsedCaps::parse(&s);
        }

        /// Mirrors the fuzz target: arbitrary bytes lossily decoded to a string.
        #[test]
        fn caps_parse_never_panics_on_arbitrary_bytes(
            bytes in prop::collection::vec(any::<u8>(), 0..1024)
        ) {
            let s = String::from_utf8_lossy(&bytes);
            let _ = ParsedCaps::parse(&s);
        }
    }
}
