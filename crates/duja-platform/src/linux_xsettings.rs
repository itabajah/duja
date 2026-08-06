//! The `_XSETTINGS_SETTINGS` blob parser: one number out of one binary
//! property.
//!
//! An XSETTINGS manager (`gnome-settings-daemon`, `xfsettingsd`, `xsettingsd`)
//! owns the `_XSETTINGS_S<screen>` selection and publishes every desktop setting
//! as a single binary property on its own window. Exactly one of those settings
//! matters here: `Xft/DPI`, the **second** source winit consults for a window's
//! scale factor on X11 — the first is the `WINIT_X11_SCALE_FACTOR` environment
//! variable, and this one is the first that has to be *read from anywhere*, which
//! is presumably how an earlier version of this sentence lost the distinction. See
//! [`linux_geometry`](crate::linux_geometry)'s
//! `scale_factor` for what the number is then used for and why this crate has to
//! read the same sources winit does.
//!
//! Pure by construction: the X11 backend fetches the property bytes and hands
//! them here, so the format — which is where the bugs are — is unit-tested on
//! **every** CI lane. None of the three has an X server — the Ubuntu one runs no
//! `Xvfb` either — so without this split the format would have no coverage at all
//! rather than one lane's worth. Same shape and same reason as `mac_events` and
//! `linux_events`.
//!
//! # The format
//!
//! From the XSETTINGS specification. All multi-byte fields use the byte order
//! named by the first byte, and every variable-length field is padded to a
//! 4-byte boundary:
//!
//! ```text
//! byte      byte order: b'l' (LSB first) or b'B' (MSB first)
//! byte[3]   padding
//! CARD32    serial
//! CARD32    n_settings
//! n_settings x {
//!     byte      type: 0 = integer, 1 = string, 2 = colour
//!     byte      padding
//!     CARD16    name length
//!     byte[]    name, padded to a multiple of 4
//!     CARD32    last-change serial
//!     ...       value, per type
//! }
//! ```
//!
//! The value is `INT32` for an integer, a `CARD32` length plus that many padded
//! bytes for a string, and four `CARD16`s for a colour.
//!
//! # Deliberately a mirror
//!
//! This is a re-implementation of winit 0.30's `x11::xsettings` parser, not an
//! independent reading of the specification, and the difference matters: the
//! number is only useful if it is the number **winit** will scale the window by.
//! Two of winit's behaviours are therefore reproduced even though the
//! specification does not require them:
//!
//! - an unrecognised byte-order byte falls back to the **host's** endianness
//!   rather than being rejected;
//! - a malformed setting **stops the search**, so a `Xft/DPI` sitting behind one
//!   is not found. (winit's `find` predicate answers `true` for a parse error,
//!   which ends its iteration and turns into an `Err` one line later.)
//!
//! Both cases end the same way in both crates — no DPI from XSETTINGS, fall
//! through to the `Xft.dpi` X resource — which is why they are worth matching
//! rather than improving on.

/// The setting an X11 session's scale factor comes from, when a settings manager
/// is running.
const DPI_NAME: &[u8] = b"Xft/DPI";

/// XSETTINGS stores `Xft/DPI` in 1024ths of a dot per inch, so the raw integer
/// is divided by this to get the DPI itself.
const DPI_MULTIPLIER: f64 = 1024.0;

/// Byte-order marker: least significant byte first.
const LITTLE_ENDIAN: u8 = b'l';
/// Byte-order marker: most significant byte first.
const BIG_ENDIAN: u8 = b'B';

/// The fixed header: the byte-order byte, three bytes of padding, and the
/// four-byte serial, all of which are skipped before the settings count.
const HEADER_BYTES: usize = 8;

/// Type tag for a setting whose value is a 32-bit integer.
const TYPE_INTEGER: u8 = 0;
/// Type tag for a setting whose value is a length-prefixed byte string.
const TYPE_STRING: u8 = 1;
/// Type tag for a setting whose value is four 16-bit colour components.
const TYPE_COLOR: u8 = 2;

/// `Xft/DPI` from an `_XSETTINGS_SETTINGS` property blob, in dots per inch.
///
/// [`None`] covers three cases that are one case downstream — no settings
/// manager published a DPI, the blob is malformed, or `Xft/DPI` is present with
/// a non-integer type — because all three mean the same thing to the caller:
/// take the next source in the chain. That is also what winit does with each of
/// them.
///
/// The returned value is **not** sanity-checked. A settings manager can publish
/// a zero or negative `Xft/DPI` and winit will divide it by 96 and use it, so
/// clamping here would be this crate quietly disagreeing with the window it is
/// placing. [`crate::geometry::sane_scale`] is the one place a degenerate factor
/// is neutralised, and it sits at the end of the chain where it neutralises
/// every source at once.
pub(crate) fn xft_dpi(blob: &[u8]) -> Option<f64> {
    let mut parser = Parser::new(blob)?;
    let count = parser.card32()?;
    for _ in 0..count {
        let setting = parser.setting()?;
        if setting.name == DPI_NAME {
            let Value::Integer(raw) = setting.value else {
                return None;
            };
            return Some(f64::from(raw) / DPI_MULTIPLIER);
        }
    }
    None
}

/// One parsed setting: its name, and its value if the value was an integer.
struct Setting<'a> {
    /// The setting's name, exactly as it appears in the blob.
    name: &'a [u8],
    /// The value, collapsed to "an integer" or "something this module does not
    /// read".
    value: Value,
}

/// A setting's value, narrowed to what this module needs.
///
/// String and colour payloads are consumed (so the walk stays in step) but not
/// retained: nothing here reads a setting other than `Xft/DPI`, and carrying a
/// borrowed string that no caller can reach would be a lifetime for its own
/// sake.
enum Value {
    /// A 32-bit integer, the type `Xft/DPI` is published as.
    Integer(i32),
    /// A string or a colour: parsed past, not kept.
    Other,
}

/// Byte order of the blob being parsed.
#[derive(Clone, Copy)]
enum Endianness {
    /// Least significant byte first.
    Little,
    /// Most significant byte first.
    Big,
}

impl Endianness {
    /// The byte order of the machine this is running on.
    ///
    /// The fallback for an unrecognised marker byte. Not a guess so much as
    /// winit's guess: an X client and its settings manager are on the same
    /// machine in every session Duja runs in, so the host's order is the one a
    /// blob with a corrupt first byte most likely used.
    const fn native() -> Self {
        if cfg!(target_endian = "big") {
            Endianness::Big
        } else {
            Endianness::Little
        }
    }
}

/// Bytes between a field of `len` bytes and the next 4-byte boundary; zero when
/// `len` is already on one.
///
/// `(-len) mod 4`, written as a wrapping negate and a two-bit mask rather than
/// as `(4 - len % 4) % 4`. The two agree for every `usize` — and this form has no
/// subtraction to reason about, which is what the arithmetic lint is asking for.
/// The outer modulo of the readable form is the part worth naming: without it an
/// already-aligned field is followed by four bytes of padding that are not there,
/// and the walk desynchronises on the first setting whose name length is a
/// multiple of four.
const fn word_padding(len: usize) -> usize {
    len.wrapping_neg() & 3
}

/// A cursor over the blob that never reads past the end.
///
/// Every accessor returns [`Option`] and consumes nothing on failure, so a
/// truncated or hostile property produces [`None`] from [`xft_dpi`] rather than
/// a panic. That matters more than it usually would: the blob is written by
/// another process, and [`crate::geometry::cursor_anchor`] promises never to
/// fail.
struct Parser<'a> {
    /// The bytes not yet consumed.
    bytes: &'a [u8],
    /// The byte order named by the blob's first byte.
    endianness: Endianness,
}

impl<'a> Parser<'a> {
    /// Read the fixed header and position the cursor at the settings count.
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let endianness = match *bytes.first()? {
            BIG_ENDIAN => Endianness::Big,
            LITTLE_ENDIAN => Endianness::Little,
            _ => Endianness::native(),
        };
        Some(Parser {
            bytes: bytes.get(HEADER_BYTES..)?,
            endianness,
        })
    }

    /// Take `n` bytes, or [`None`] if fewer remain.
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (part, rest) = self.bytes.split_at_checked(n)?;
        self.bytes = rest;
        Some(part)
    }

    /// Skip to the next multiple of four after a field of `len` bytes.
    fn pad_to_word(&mut self, len: usize) -> Option<()> {
        self.take(word_padding(len)).map(|_| ())
    }

    /// Read one byte.
    fn byte(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    /// Read a 16-bit field in the blob's byte order.
    fn card16(&mut self) -> Option<u16> {
        let bytes: [u8; 2] = self.take(2)?.try_into().ok()?;
        Some(match self.endianness {
            Endianness::Little => u16::from_le_bytes(bytes),
            Endianness::Big => u16::from_be_bytes(bytes),
        })
    }

    /// Read a 32-bit field in the blob's byte order.
    fn card32(&mut self) -> Option<u32> {
        let bytes: [u8; 4] = self.take(4)?.try_into().ok()?;
        Some(match self.endianness {
            Endianness::Little => u32::from_le_bytes(bytes),
            Endianness::Big => u32::from_be_bytes(bytes),
        })
    }

    /// Read one setting, leaving the cursor on the next.
    fn setting(&mut self) -> Option<Setting<'a>> {
        let ty = self.byte()?;
        // One byte of padding after the type tag.
        self.take(1)?;
        let name_len = usize::from(self.card16()?);
        let name = self.take(name_len)?;
        self.pad_to_word(name_len)?;
        // The per-setting last-change serial, which nothing here compares.
        self.take(4)?;

        let value = match ty {
            TYPE_INTEGER => {
                // RATIONALE (cast_possible_wrap): the specification's INT32 is
                // read as a CARD32 and reinterpreted, which is what makes a
                // negative DPI come back negative instead of as four billion.
                #[allow(clippy::cast_possible_wrap)]
                let signed = self.card32()? as i32;
                Value::Integer(signed)
            }
            TYPE_STRING => {
                let len = usize::try_from(self.card32()?).ok()?;
                self.take(len)?;
                self.pad_to_word(len)?;
                Value::Other
            }
            TYPE_COLOR => {
                // Four CARD16 components; read as one 8-byte block because none
                // of them is inspected.
                self.take(8)?;
                Value::Other
            }
            // An unknown type tag has an unknown payload length, so the walk
            // cannot step over it and stays stopped. winit reaches the same
            // place by a different route (its `TryFrom<i8>` errors, its `find`
            // treats the error as a match, and `get_xft_dpi` logs and falls
            // through).
            _ => return None,
        };
        Some(Setting { name, value })
    }
}

#[cfg(test)]
mod tests {
    use super::{BIG_ENDIAN, LITTLE_ENDIAN, word_padding, xft_dpi};

    /// Build a well-formed little-endian blob out of `(name, type, payload)`
    /// triples.
    ///
    /// Written out rather than expressed with a helper per type, because the
    /// padding rule is the part most likely to be wrong in both the parser and a
    /// hand-written fixture, and a builder that shares the parser's arithmetic
    /// would hide a mistake in both at once.
    fn blob(order: u8, settings: &[(&[u8], u8, Vec<u8>)]) -> Vec<u8> {
        let mut out = vec![order, 0, 0, 0];
        let serial: u32 = 7;
        let count = u32::try_from(settings.len()).expect("test fixture");
        let big = order == BIG_ENDIAN;
        let word4 = |value: u32| {
            if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        let word2 = |value: u16| {
            if big {
                value.to_be_bytes()
            } else {
                value.to_le_bytes()
            }
        };
        out.extend_from_slice(&word4(serial));
        out.extend_from_slice(&word4(count));
        for (name, ty, payload) in settings {
            out.push(*ty);
            out.push(0);
            let len = u16::try_from(name.len()).expect("test fixture");
            out.extend_from_slice(&word2(len));
            out.extend_from_slice(name);
            out.extend(std::iter::repeat_n(0_u8, word_padding(name.len())));
            out.extend_from_slice(&word4(serial));
            out.extend_from_slice(payload);
        }
        out
    }

    /// One integer setting named `name`, little-endian.
    fn integer(name: &[u8], value: i32) -> (&[u8], u8, Vec<u8>) {
        (name, 0, value.to_le_bytes().to_vec())
    }

    /// Replace a little-endian blob's `n_settings` field with `u32::MAX`.
    fn overwrite_count(bytes: &mut [u8]) {
        bytes
            .get_mut(8..12)
            .expect("a fixture always carries a full header")
            .copy_from_slice(&u32::MAX.to_le_bytes());
    }

    #[test]
    fn the_dpi_comes_back_in_dots_per_inch_not_in_1024ths() {
        // 96 dpi is 98_304 in the wire encoding. Forgetting the divisor would
        // hand the scale chain a factor of 1024, and every window would be
        // computed as a thousand times too large.
        let bytes = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", 98_304)]);
        assert_eq!(xft_dpi(&bytes), Some(96.0));
    }

    #[test]
    fn a_fractional_dpi_is_not_rounded_to_an_integer() {
        // GNOME publishes non-integral DPI for fractional scaling: 1.25x is
        // 120 dpi, but 110 % of 96 is 105.6, which only survives if the division
        // happens in floating point.
        let bytes = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", 108_134)]);
        let dpi = xft_dpi(&bytes).expect("a well-formed integer setting");
        assert!(
            (dpi - 105.599_609_375).abs() < 1e-9,
            "expected the exact quotient, got {dpi}"
        );
    }

    #[test]
    fn a_setting_before_the_dpi_is_stepped_over_whatever_its_type() {
        // The walk has to consume a string's length prefix and its padding, and
        // a colour's four components, to arrive at the next setting's type byte.
        // Getting either payload length wrong desynchronises the cursor and the
        // DPI behind it is never found.
        let bytes = blob(
            LITTLE_ENDIAN,
            &[
                (b"Net/ThemeName", 1, {
                    let mut v = 5_u32.to_le_bytes().to_vec();
                    v.extend_from_slice(b"Adwai");
                    v.extend_from_slice(&[0, 0, 0]);
                    v
                }),
                (b"Net/CursorColor", 2, vec![0; 8]),
                integer(b"Xft/DPI", 141_312),
            ],
        );
        assert_eq!(xft_dpi(&bytes), Some(138.0));
    }

    #[test]
    fn a_name_whose_length_is_already_a_multiple_of_four_gains_no_padding() {
        // `(4 - len % 4) % 4` is the whole rule: the outer modulo is what stops
        // an aligned name from being followed by four bytes of padding that are
        // not there. Dropping it desynchronises every blob whose first setting
        // has a 4, 8 or 12-character name.
        let bytes = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", 98_304)]);
        assert_eq!(b"Xft/DPI".len() % 4, 3, "the fixture must exercise padding");
        assert_eq!(xft_dpi(&bytes), Some(96.0));

        let aligned = blob(
            LITTLE_ENDIAN,
            &[
                (b"Gtk/FontName", 1, {
                    let mut v = 4_u32.to_le_bytes().to_vec();
                    v.extend_from_slice(b"Sans");
                    v
                }),
                integer(b"Xft/DPI", 98_304),
            ],
        );
        assert_eq!(b"Gtk/FontName".len() % 4, 0, "the fixture must be aligned");
        assert_eq!(xft_dpi(&aligned), Some(96.0));
    }

    #[test]
    fn the_byte_order_byte_is_obeyed() {
        // A big-endian blob read little-endian turns 98_304 into 1_536, i.e.
        // 1.5 dpi. The marker is the only thing that distinguishes them.
        let bytes = blob(
            BIG_ENDIAN,
            &[(b"Xft/DPI", 0, 98_304_i32.to_be_bytes().to_vec())],
        );
        assert_eq!(xft_dpi(&bytes), Some(96.0));
    }

    #[test]
    fn an_unknown_byte_order_marker_falls_back_to_the_host_order() {
        // winit's behaviour, mirrored deliberately: a corrupt marker is treated
        // as "the machine that wrote this is this machine". Rejecting the blob
        // instead would drop a DPI winit is going to read successfully.
        //
        // The fixture is built in the *host's* order rather than a fixed one, so
        // the assertion is "the marker was ignored and the host order used" on
        // either kind of machine — a little-endian fixture read on a big-endian
        // host would only prove that a mis-read blob comes back wrong, which is
        // a different claim. No CI lane is big-endian today; writing it this way
        // is what stops the test from silently meaning something else if one is.
        let host = if cfg!(target_endian = "big") {
            BIG_ENDIAN
        } else {
            LITTLE_ENDIAN
        };
        let mut bytes = blob(host, &[(b"Xft/DPI", 0, 98_304_i32.to_ne_bytes().to_vec())]);
        *bytes.first_mut().expect("a non-empty fixture") = b'?';
        assert_eq!(xft_dpi(&bytes), Some(96.0));
    }

    #[test]
    fn a_missing_dpi_setting_is_none_rather_than_a_default() {
        // The caller has two more sources to try. Inventing 96.0 here would
        // shadow an `Xft.dpi` resource that says something else.
        let bytes = blob(
            LITTLE_ENDIAN,
            &[(b"Gtk/FontName", 1, {
                let mut v = 4_u32.to_le_bytes().to_vec();
                v.extend_from_slice(b"Sans");
                v
            })],
        );
        assert_eq!(xft_dpi(&bytes), None);
    }

    #[test]
    fn a_dpi_published_as_a_string_is_not_read_as_a_number() {
        // The type tag decides how the payload is laid out; reading a string's
        // length prefix as an integer would report a DPI of 4/1024.
        let bytes = blob(
            LITTLE_ENDIAN,
            &[(b"Xft/DPI", 1, {
                let mut v = 2_u32.to_le_bytes().to_vec();
                v.extend_from_slice(b"96");
                v.extend_from_slice(&[0, 0]);
                v
            })],
        );
        assert_eq!(xft_dpi(&bytes), None);
    }

    #[test]
    fn a_negative_dpi_survives_as_a_negative_number() {
        // The wire type is INT32. Reading it unsigned turns a settings manager's
        // nonsense into 4_294_868_992, which is 4_194_208 dpi and a scale factor
        // of roughly forty-three *thousand* — `sane_scale` does not reject it (it
        // guards the low end only) and it would size the flyout into oblivion.
        // Passing the negative through is what lets the guard at the end of the
        // chain see it for what it is.
        let bytes = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", -98_304)]);
        assert_eq!(xft_dpi(&bytes), Some(-96.0));
    }

    #[test]
    fn a_truncated_blob_is_none_rather_than_a_panic() {
        // The property is written by another process, and `cursor_anchor`
        // promises never to fail. Every prefix of a valid blob must come back
        // `None` or `Some`, and none of them may panic.
        let full = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", 98_304)]);
        for len in 0..full.len() {
            let prefix = full.get(..len).expect("len is below the length");
            let _ = xft_dpi(prefix);
        }
        assert_eq!(xft_dpi(&full), Some(96.0));
    }

    #[test]
    fn a_count_larger_than_the_blob_stops_at_the_end() {
        // `n_settings` is attacker-controlled in the same sense the rest is: a
        // manager that writes 4 billion would drive a loop that reads past the
        // end unless every read is checked.
        let mut bytes = blob(LITTLE_ENDIAN, &[integer(b"Xft/DPI", 98_304)]);
        overwrite_count(&mut bytes);
        assert_eq!(
            xft_dpi(&bytes),
            Some(96.0),
            "the DPI is the first setting, so the count never runs out"
        );

        let mut nameless = blob(
            LITTLE_ENDIAN,
            &[(b"Gtk/FontName", 1, {
                let mut v = 4_u32.to_le_bytes().to_vec();
                v.extend_from_slice(b"Sans");
                v
            })],
        );
        overwrite_count(&mut nameless);
        assert_eq!(xft_dpi(&nameless), None);
    }

    #[test]
    fn an_unknown_type_tag_stops_the_walk() {
        // Deliberate, and the reason is that the payload length is unknown: a
        // parser that skipped the tag and kept going would read the payload as a
        // setting header and could report any number at all. Stopping matches
        // winit, which errors on the same byte.
        let bytes = blob(
            LITTLE_ENDIAN,
            &[(b"Some/Future", 9, vec![0; 4]), integer(b"Xft/DPI", 98_304)],
        );
        assert_eq!(xft_dpi(&bytes), None);
    }

    #[test]
    fn a_blob_shorter_than_the_header_is_none() {
        // A zero-length property is what `GetProperty` returns for a window that
        // owns the selection but has not published yet.
        for len in 0..=super::HEADER_BYTES {
            assert_eq!(xft_dpi(&vec![LITTLE_ENDIAN; len]), None, "length {len}");
        }
    }
}
