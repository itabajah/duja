//! Stable, EDID-derived display identity.
//!
//! A monitor's OS-assigned handle changes across replug, reboot and port
//! changes; its EDID does not. [`StableDisplayId`] derives a durable key from
//! the 128-byte EDID base block so per-monitor settings survive hot-plug.

// ---- specs first (TDD); implementation follows in the next commit ----

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

    #[test]
    fn id_falls_back_to_hash_without_serial() {
        let id = StableDisplayId::from_edid(&dell_edid()).unwrap();
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
        let a = StableDisplayId::from_edid(&dell_edid()).unwrap();
        let b = StableDisplayId::from_edid(&dell_edid()).unwrap();
        assert_eq!(a, b);

        let mut other = dell_edid();
        // Perturb the product code and re-checksum so the hash must change.
        if let Some(byte) = other.get_mut(10) {
            *byte = byte.wrapping_add(1);
        }
        let sum: u8 = other
            .iter()
            .take(127)
            .copied()
            .fold(0u8, u8::wrapping_add);
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
            [
                descriptor(0xFF, "AB CD-12"),
                filler(),
                filler(),
                filler(),
            ],
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
