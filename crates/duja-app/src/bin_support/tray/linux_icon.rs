//! The one decision in the Linux tray that can be wrong without a display.
//!
//! [ADR-0011]'s split, applied to the tray: the rule lives here and is compiled
//! `cfg(any(test, target_os = "linux"))`, so **every** lane's `cargo test`
//! contains it while only the Linux build ships it. The wire half — `ksni::Icon`,
//! the `Tray` impl, the D-Bus service — is in [`super::ksni_tray`] and is
//! `cfg(target_os = "linux")`, because none of those types exist elsewhere.
//!
//! That division is worth stating because the tempting arrangement is the other
//! one. Putting this function next to its only caller would leave it tested on
//! the ubuntu lane alone, which is precisely where a byte-order mistake is
//! *least* likely to be noticed: the ubuntu runner has no
//! `StatusNotifierWatcher`, so nothing there renders the icon either.
//!
//! [ADR-0011]: https://github.com/itabajah/duja/blob/main/docs/adr/0011-linux-software-dimming.md

/// The pixmap side length, in pixels.
///
/// A `StatusNotifierItem` pixmap is scaled by the host rather than requested at a
/// fixed size, so this is not a number the protocol asked for. It is the same 32
/// the Windows tray uses, for the same reason: the glyph is drawn to be legible
/// small, and handing the host a larger source to downscale is what the other
/// arms do.
pub(super) const ICON_SIZE: u32 = 32;

/// Convert an RGBA byte buffer to the ARGB32 network-byte-order one the
/// `StatusNotifierItem` spec requires.
///
/// Two facts make this more than a shuffle, both from the spec wording that
/// `ksni::Icon` quotes — "ARGB32 format, network byte order":
///
/// - alpha moves from **last to first**, and
/// - "network byte order" is big-endian, so the bytes are *written positionally*
///   in the order A, R, G, B rather than reinterpreted from a `u32`.
///
/// Writing them positionally is what would also make this correct on a big-endian
/// host, where a `u32::to_be_bytes` round trip is a no-op and a `to_ne_bytes` one
/// is silently wrong. Duja ships no big-endian target, so that half is reasoning
/// rather than something a lane exercises — said plainly, because the tests below
/// run little-endian and cannot tell the two spellings apart.
pub(super) fn rgba_to_argb32(rgba: &[u8]) -> Vec<u8> {
    let mut argb = Vec::with_capacity(rgba.len());
    for pixel in rgba.chunks_exact(4) {
        // Destructuring rather than indexing: `indexing_slicing` is denied, and
        // `chunks_exact` already guarantees the length this states to the
        // compiler. A short trailing chunk is impossible by that same guarantee,
        // which is why there is no `else` — see the test that pins it.
        if let [r, g, b, a] = *pixel {
            argb.extend_from_slice(&[a, r, g, b]);
        }
    }
    argb
}

#[cfg(test)]
mod tests {
    use super::{ICON_SIZE, rgba_to_argb32};

    #[test]
    fn the_alpha_byte_moves_from_last_to_first() {
        // One pixel whose four bytes all differ, so any rotation, reversal or
        // truncation shows up. A buffer whose bytes repeat passes under several
        // wrong permutations, which is the trap this avoids.
        assert_eq!(
            rgba_to_argb32(&[0x11, 0x22, 0x33, 0x44]),
            vec![0x44, 0x11, 0x22, 0x33]
        );
    }

    #[test]
    fn every_pixel_is_converted_and_none_is_reused() {
        // Two pixels rather than one: a conversion that handled only the first
        // chunk, or wrote the same pixel twice, passes the test above.
        let converted = rgba_to_argb32(&[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]);
        assert_eq!(
            converted,
            vec![0x04, 0x01, 0x02, 0x03, 0x08, 0x05, 0x06, 0x07]
        );
    }

    #[test]
    fn a_real_glyph_keeps_its_pixel_count() {
        // The production buffer at the size the Linux tray uses. Pinned because
        // `ksni::Icon` carries `width`/`height` separately from the data, so a
        // conversion that dropped pixels would be a silently truncated image
        // rather than an error the host could report.
        let rgba = duja_ui::icon::monitor_rgba(ICON_SIZE, [0x2E, 0x8B, 0xC0]);
        let expected = (ICON_SIZE as usize)
            .saturating_mul(ICON_SIZE as usize)
            .saturating_mul(4);
        assert_eq!(rgba.len(), expected);
        assert_eq!(rgba_to_argb32(&rgba).len(), expected);
    }

    #[test]
    fn a_trailing_partial_pixel_is_dropped_rather_than_panicking() {
        // `chunks_exact` cannot yield a short chunk, so five bytes give one pixel
        // and the stray byte is discarded. Unreachable from the caller, whose
        // buffer is `size * size * 4` by construction — pinned because the
        // alternative spellings of this loop (`chunks`, or indexing) would panic
        // or emit a wrong-length buffer instead, and both are the obvious edit.
        assert_eq!(rgba_to_argb32(&[1, 2, 3, 4, 5]), vec![4, 1, 2, 3]);
        assert!(rgba_to_argb32(&[1, 2, 3]).is_empty());
        assert!(rgba_to_argb32(&[]).is_empty());
    }
}
