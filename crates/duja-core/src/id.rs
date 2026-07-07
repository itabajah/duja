//! Stable, EDID-derived display identity.
//!
//! A monitor's OS-assigned handle changes across replug, reboot and port
//! changes; its EDID does not. [`StableDisplayId`] derives a durable key from
//! the 128-byte EDID base block so per-monitor settings survive hot-plug.

use std::fmt;

/// Length of the EDID base block, in bytes.
const BASE_BLOCK_LEN: usize = 128;

/// The fixed 8-byte EDID base-block header (`00 FF FF FF FF FF FF 00`).
const HEADER_MAGIC: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

/// The 26 uppercase letters, indexed by PNP value minus one.
const ALPHABET: &[u8; 26] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// An error encountered while parsing an EDID base block.
///
/// Parsing is total: every malformed input yields one of these variants rather
/// than panicking (the parser is a fuzz target).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EdidError {
    /// Fewer than 128 bytes were supplied; the wrapped value is the actual len.
    #[error("EDID too short: need at least 128 bytes, got {0}")]
    TooShort(usize),
    /// The 8-byte header did not match the standard `00 FF..FF 00` magic.
    #[error("EDID base-block header is not the standard 00 FF..FF 00 magic")]
    BadHeader,
    /// The base block's bytes did not sum to zero modulo 256.
    #[error("EDID base-block checksum does not sum to zero mod 256")]
    BadChecksum,
    /// Bytes 8..=9 did not encode three A–Z letters.
    #[error("EDID manufacturer id is not three A–Z letters")]
    InvalidManufacturer,
}

/// Structured fields extracted from an EDID base block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EdidInfo {
    /// Three-letter PNP manufacturer id (e.g. `"GSM"` for LG).
    pub manufacturer: String,
    /// Product code (bytes 10..=11, little-endian).
    pub product_code: u16,
    /// Numeric serial (bytes 12..=15, little-endian); `0` when unset.
    pub serial_number: u32,
    /// Serial-number string from the 0xFF display descriptor, if present.
    pub serial_string: Option<String>,
    /// Monitor-name string from the 0xFC display descriptor, if present.
    pub monitor_name: Option<String>,
}

impl EdidInfo {
    /// Parse the 128-byte EDID base block.
    ///
    /// Inputs longer than 128 bytes (base block + extensions) are accepted;
    /// only the base block is read. Every access is bounds-checked, so this
    /// never panics on malformed input.
    ///
    /// # Errors
    /// - [`EdidError::TooShort`] if fewer than 128 bytes are supplied.
    /// - [`EdidError::BadHeader`] if the 8-byte magic is wrong.
    /// - [`EdidError::BadChecksum`] if the base block does not sum to 0 mod 256.
    /// - [`EdidError::InvalidManufacturer`] if bytes 8..=9 do not encode three
    ///   A–Z letters.
    pub fn parse(edid: &[u8]) -> Result<Self, EdidError> {
        let base = edid
            .get(..BASE_BLOCK_LEN)
            .ok_or(EdidError::TooShort(edid.len()))?;

        if base.get(..8) != Some(&HEADER_MAGIC[..]) {
            return Err(EdidError::BadHeader);
        }

        let checksum = base.iter().copied().fold(0u8, u8::wrapping_add);
        if checksum != 0 {
            return Err(EdidError::BadChecksum);
        }

        let b8 = base.get(8).copied().unwrap_or(0);
        let b9 = base.get(9).copied().unwrap_or(0);
        let manufacturer = parse_manufacturer(b8, b9)?;

        let b10 = base.get(10).copied().unwrap_or(0);
        let b11 = base.get(11).copied().unwrap_or(0);
        let product_code = u16::from_le_bytes([b10, b11]);

        let serial_number = match base.get(12..16) {
            Some([a, b, c, d]) => u32::from_le_bytes([*a, *b, *c, *d]),
            _ => 0,
        };

        let (serial_string, monitor_name) = parse_descriptors(base);

        Ok(EdidInfo {
            manufacturer,
            product_code,
            serial_number,
            serial_string,
            monitor_name,
        })
    }
}

/// Decode the big-endian bit-packed manufacturer id from bytes 8..=9.
fn parse_manufacturer(b8: u8, b9: u8) -> Result<String, EdidError> {
    let packed = u16::from_be_bytes([b8, b9]);
    let groups = [(packed >> 10) & 0x1F, (packed >> 5) & 0x1F, packed & 0x1F];
    let mut mfg = String::with_capacity(3);
    for g in groups {
        mfg.push(pnp_letter(g).ok_or(EdidError::InvalidManufacturer)?);
    }
    Ok(mfg)
}

/// Map a 5-bit PNP group (1..=26) to `A`..=`Z`, or `None` if out of range.
fn pnp_letter(value: u16) -> Option<char> {
    if value == 0 || value > 26 {
        return None;
    }
    let idx = value.wrapping_sub(1);
    ALPHABET.get(usize::from(idx)).copied().map(char::from)
}

/// Scan the four 18-byte descriptors for the serial (0xFF) and name (0xFC).
fn parse_descriptors(base: &[u8]) -> (Option<String>, Option<String>) {
    let mut serial = None;
    let mut name = None;
    let Some(region) = base.get(54..126) else {
        return (serial, name);
    };
    for chunk in region.chunks_exact(18) {
        // A display descriptor is flagged by three leading zero bytes;
        // anything else is a detailed-timing descriptor we skip.
        if chunk.get(..3) != Some(&[0x00, 0x00, 0x00][..]) {
            continue;
        }
        let tag = chunk.get(3).copied().unwrap_or(0);
        let text = chunk.get(5..18).and_then(parse_descriptor_text);
        match tag {
            0xFF if serial.is_none() => serial = text,
            0xFC if name.is_none() => name = text,
            _ => {}
        }
    }
    (serial, name)
}

/// Decode a descriptor text field: printable ASCII up to an 0x0A terminator,
/// with trailing padding trimmed. Returns `None` if nothing remains.
fn parse_descriptor_text(raw: &[u8]) -> Option<String> {
    let mut text = String::with_capacity(raw.len());
    for &b in raw {
        if b == 0x0A {
            break;
        }
        if (0x20..=0x7E).contains(&b) {
            text.push(char::from(b));
        }
    }
    let trimmed = text.trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// A durable, EDID-derived identity for a display.
///
/// The serial component is chosen by rank:
/// 1. the 0xFF-descriptor serial *string* (e.g. `"GSM-5B09-312NTAB1C234"`),
/// 2. else a **non-zero** numeric serial from bytes 12..=15, as
///    `MFG-PROD-s<decimal>` — it survives EDID byte changes (e.g. firmware
///    updates) that would shift a content hash,
/// 3. else `MFG-PROD-#hXXXXXXXX`, an FNV-1a hash of the full EDID (a zero
///    numeric serial is unset, which is common, and must not collide).
///
/// [`with_slot`](Self::with_slot) disambiguates identical twin monitors that
/// share an EDID.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StableDisplayId(String);

impl StableDisplayId {
    /// Derive an identity straight from a raw EDID blob.
    ///
    /// # Errors
    /// Propagates any [`EdidError`] from [`EdidInfo::parse`].
    pub fn from_edid(edid: &[u8]) -> Result<Self, EdidError> {
        let info = EdidInfo::parse(edid)?;
        let base = format!("{}-{:04X}", info.manufacturer, info.product_code);
        let serial_key = info
            .serial_string
            .as_deref()
            .map(sanitize)
            .filter(|s| !s.is_empty());
        let id = match serial_key {
            Some(serial) => format!("{base}-{serial}"),
            None if info.serial_number != 0 => format!("{base}-s{}", info.serial_number),
            None => format!("{base}-#h{}", fnv1a_hex(edid)),
        };
        Ok(StableDisplayId(id))
    }

    /// Borrow the id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return a new id with a `-slot<n>` disambiguator appended.
    #[must_use]
    pub fn with_slot(&self, slot: u32) -> Self {
        StableDisplayId(format!("{}-slot{slot}", self.0))
    }
}

impl fmt::Display for StableDisplayId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Keep only `[A-Za-z0-9]` from a serial string so ids stay in the id charset.
fn sanitize(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect()
}

/// FNV-1a (32-bit) of `bytes`, formatted as eight lowercase hex digits.
fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= u32::from(b);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    format!("{hash:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- synthetic-EDID construction helpers (built without indexing so the
    //     test module stays clean under `indexing_slicing`) ---

    /// The fixed 8-byte EDID header.
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];

    /// Map an ASCII uppercase letter to its 1..=26 PNP value (`A` -> 1).
    fn letter_val(c: u8) -> u16 {
        u16::from(c).wrapping_sub(64)
    }

    /// Pack a three-letter manufacturer id into big-endian bytes 8..=9.
    fn mfg_bytes(mfg: &str) -> [u8; 2] {
        let bytes: Vec<u8> = mfg.bytes().collect();
        let v0 = letter_val(bytes.first().copied().unwrap_or(b'A'));
        let v1 = letter_val(bytes.get(1).copied().unwrap_or(b'A'));
        let v2 = letter_val(bytes.get(2).copied().unwrap_or(b'A'));
        let packed: u16 = (v0 << 10) | (v1 << 5) | v2;
        packed.to_be_bytes()
    }

    /// Build an 18-byte display descriptor carrying `text` under `tag`.
    fn descriptor(tag: u8, text: &str) -> Vec<u8> {
        let mut d = Vec::with_capacity(18);
        d.extend_from_slice(&[0x00, 0x00, 0x00, tag, 0x00]);
        let mut body: Vec<u8> = text.bytes().take(13).collect();
        body.push(0x0A);
        body.resize(13, 0x20);
        d.extend_from_slice(&body);
        d
    }

    /// An unused (all-zero) descriptor slot.
    fn filler() -> Vec<u8> {
        vec![0u8; 18]
    }

    /// Assemble a checksum-valid 128-byte EDID from the given parts.
    fn synth_edid(mfg: &str, product: u16, serial_num: u32, descriptors: [Vec<u8>; 4]) -> Vec<u8> {
        let mut e = Vec::with_capacity(128);
        e.extend_from_slice(&HEADER);
        e.extend_from_slice(&mfg_bytes(mfg));
        e.extend_from_slice(&product.to_le_bytes());
        e.extend_from_slice(&serial_num.to_le_bytes());
        // Pad through the timing/params region up to the descriptor block.
        e.resize(54, 0x00);
        for d in descriptors {
            e.extend_from_slice(&d);
        }
        e.push(0x00); // byte 126: extension block count
        let sum: u8 = e.iter().copied().fold(0u8, u8::wrapping_add);
        e.push(sum.wrapping_neg()); // byte 127: checksum
        e
    }

    /// LG UltraGear-like fixture: has both a serial string and a name.
    fn lg_edid() -> Vec<u8> {
        synth_edid(
            "GSM",
            0x5B09,
            0x0100_2A3B,
            [
                filler(),
                descriptor(0xFF, "312NTAB1C234"),
                descriptor(0xFC, "LG ULTRAGEAR"),
                filler(),
            ],
        )
    }

    /// Dell-like fixture: name only, no serial string (hash-fallback path).
    fn dell_edid() -> Vec<u8> {
        synth_edid(
            "DEL",
            0xA131,
            0x0000_3039,
            [
                filler(),
                descriptor(0xFC, "DELL U2720Q"),
                filler(),
                filler(),
            ],
        )
    }

    #[test]
    fn parse_rejects_empty_and_short_input() {
        assert!(matches!(EdidInfo::parse(&[]), Err(EdidError::TooShort(0))));
        assert!(matches!(
            EdidInfo::parse(&[0u8; 64]),
            Err(EdidError::TooShort(64))
        ));
    }

    #[test]
    fn parse_rejects_bad_header() {
        // 128 zero bytes: checksum sums to zero, but the header is wrong.
        assert_eq!(EdidInfo::parse(&[0u8; 128]), Err(EdidError::BadHeader));
    }

    #[test]
    fn parse_rejects_bad_checksum() {
        let mut bad = lg_edid();
        if let Some(last) = bad.last_mut() {
            *last = last.wrapping_add(1);
        }
        assert_eq!(EdidInfo::parse(&bad), Err(EdidError::BadChecksum));
    }

    #[test]
    fn parses_manufacturer_and_product() {
        let info = EdidInfo::parse(&lg_edid()).unwrap();
        assert_eq!(info.manufacturer, "GSM");
        assert_eq!(info.product_code, 0x5B09);
        assert_eq!(info.serial_number, 0x0100_2A3B);
    }

    #[test]
    fn parses_serial_string_and_name() {
        let info = EdidInfo::parse(&lg_edid()).unwrap();
        assert_eq!(info.serial_string.as_deref(), Some("312NTAB1C234"));
        assert_eq!(info.monitor_name.as_deref(), Some("LG ULTRAGEAR"));
    }

    #[test]
    fn id_uses_serial_string() {
        let id = StableDisplayId::from_edid(&lg_edid()).unwrap();
        assert_eq!(id.as_str(), "GSM-5B09-312NTAB1C234");
    }

    /// Dell-like fixture with a ZERO numeric serial: the true hash-fallback path.
    fn dell_edid_no_serial() -> Vec<u8> {
        synth_edid(
            "DEL",
            0xA131,
            0,
            [
                filler(),
                descriptor(0xFC, "DELL U2720Q"),
                filler(),
                filler(),
            ],
        )
    }

    #[test]
    fn id_uses_numeric_serial_when_no_serial_string() {
        // dell_edid carries numeric serial 0x3039 = 12345 and no 0xFF string:
        // the numeric serial must rank above the content-hash fallback.
        let id = StableDisplayId::from_edid(&dell_edid()).unwrap();
        assert_eq!(id.as_str(), "DEL-A131-s12345");
    }

    #[test]
    fn serial_string_wins_over_numeric_serial() {
        // lg_edid carries BOTH a 0xFF serial string and a nonzero numeric
        // serial; the string must win.
        let id = StableDisplayId::from_edid(&lg_edid()).unwrap();
        assert_eq!(id.as_str(), "GSM-5B09-312NTAB1C234");
    }

    #[test]
    fn id_falls_back_to_hash_without_serial() {
        // No 0xFF string AND a zero numeric serial -> content hash.
        let id = StableDisplayId::from_edid(&dell_edid_no_serial()).unwrap();
        assert!(
            id.as_str().starts_with("DEL-A131-#h"),
            "unexpected id: {}",
            id.as_str()
        );
        // "#h" + exactly eight lowercase hex digits.
        let hex = id.as_str().rsplit("#h").next().unwrap_or_default();
        assert_eq!(hex.len(), 8);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_fallback_is_deterministic_and_distinct() {
        let a = StableDisplayId::from_edid(&dell_edid_no_serial()).unwrap();
        let b = StableDisplayId::from_edid(&dell_edid_no_serial()).unwrap();
        assert_eq!(a, b);

        let mut other = dell_edid_no_serial();
        // Perturb the product code and re-checksum so the hash must change.
        if let Some(byte) = other.get_mut(10) {
            *byte = byte.wrapping_add(1);
        }
        let sum: u8 = other.iter().take(127).copied().fold(0u8, u8::wrapping_add);
        if let Some(cksum) = other.get_mut(127) {
            *cksum = sum.wrapping_neg();
        }
        let c = StableDisplayId::from_edid(&other).unwrap();
        assert_ne!(a.as_str(), c.as_str());
    }

    #[test]
    fn with_slot_appends_disambiguator() {
        let id = StableDisplayId::from_edid(&lg_edid()).unwrap();
        assert_eq!(id.with_slot(2).as_str(), "GSM-5B09-312NTAB1C234-slot2");
    }

    #[test]
    fn dell_fixture_manufacturer_and_name() {
        let info = EdidInfo::parse(&dell_edid()).unwrap();
        assert_eq!(info.manufacturer, "DEL");
        assert_eq!(info.product_code, 0xA131);
        assert_eq!(info.serial_string, None);
        assert_eq!(info.monitor_name.as_deref(), Some("DELL U2720Q"));
    }

    #[test]
    fn id_is_charset_sanitized() {
        // Serial with spaces and punctuation must collapse to [A-Za-z0-9].
        let edid = synth_edid(
            "GSM",
            0x5B09,
            0,
            [descriptor(0xFF, "AB CD-12"), filler(), filler(), filler()],
        );
        let info = EdidInfo::parse(&edid).unwrap();
        assert_eq!(info.serial_string.as_deref(), Some("AB CD-12"));

        let id = StableDisplayId::from_edid(&edid).unwrap();
        assert_eq!(id.as_str(), "GSM-5B09-ABCD12");
        assert!(
            id.as_str()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'#')
        );
    }

    #[test]
    fn all_whitespace_serial_falls_back_to_hash() {
        // A serial descriptor that sanitizes to empty must not win over hash.
        let edid = synth_edid(
            "GSM",
            0x5B09,
            0,
            [descriptor(0xFF, "   "), filler(), filler(), filler()],
        );
        let id = StableDisplayId::from_edid(&edid).unwrap();
        assert!(id.as_str().contains("#h"));
    }

    #[test]
    fn rejects_out_of_range_manufacturer() {
        // Force a zero 5-bit group in bytes 8..=9 (invalid PNP letter).
        let mut edid = lg_edid();
        if let Some(b) = edid.get_mut(8) {
            *b = 0x00;
        }
        if let Some(b) = edid.get_mut(9) {
            *b = 0x00;
        }
        let sum: u8 = edid.iter().take(127).copied().fold(0u8, u8::wrapping_add);
        if let Some(cksum) = edid.get_mut(127) {
            *cksum = sum.wrapping_neg();
        }
        assert_eq!(EdidInfo::parse(&edid), Err(EdidError::InvalidManufacturer));
    }

    #[test]
    fn accepts_edid_longer_than_base_block() {
        // A 256-byte blob (base block + one extension) parses off the base.
        let mut edid = lg_edid();
        edid.resize(256, 0x00);
        let info = EdidInfo::parse(&edid).unwrap();
        assert_eq!(info.manufacturer, "GSM");
    }

    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(10_000))]

        /// Arbitrary bytes must never panic — this is a fuzz target later.
        #[test]
        fn parse_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..=300)) {
            let _ = EdidInfo::parse(&bytes);
            let _ = StableDisplayId::from_edid(&bytes);
        }

        /// Any valid synthetic EDID round-trips its manufacturer and product.
        #[test]
        fn valid_edid_roundtrips_mfg_product(
            a in b'A'..=b'Z',
            b in b'A'..=b'Z',
            c in b'A'..=b'Z',
            product in any::<u16>(),
            serial in any::<u32>(),
        ) {
            let mfg = String::from_utf8(vec![a, b, c]).unwrap();
            let edid = synth_edid(&mfg, product, serial, [filler(), filler(), filler(), filler()]);
            let info = EdidInfo::parse(&edid).unwrap();
            prop_assert_eq!(info.manufacturer, mfg);
            prop_assert_eq!(info.product_code, product);
            prop_assert_eq!(info.serial_number, serial);
        }
    }
}
