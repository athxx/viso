//! The orientation an EXIF block records: tag `0x0112` of its first image
//! directory, read from the TIFF structure the block holds.

/// The orientation tag of `exif`, a TIFF structure, with or without the
/// `Exif\0\0` that opens a JPEG's block; `None` when it has none or the
/// structure is cut short.
pub(crate) fn orientation(exif: &[u8]) -> Option<u16> {
    let tiff = exif.strip_prefix(b"Exif\0\0").unwrap_or(exif);
    let little = match tiff.get(..4)? {
        [b'I', b'I', 42, 0] => true,
        [b'M', b'M', 0, 42] => false,
        _ => return None,
    };
    let u16_at = |at: usize| {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if little {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if little {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let ifd = u32_at(4)? as usize;
    let count = u16_at(ifd)? as usize;
    (0..count).find_map(|i| {
        let entry = ifd + 2 + i * 12;
        // A SHORT, its value held in the entry itself.
        (u16_at(entry)? == 0x0112 && u16_at(entry + 2)? == 3).then(|| u16_at(entry + 8))?
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An EXIF block, as a JPEG holds it, recording `orientation`.
    pub(crate) fn exif(orientation: u16, little: bool) -> Vec<u8> {
        let u16b = |v: u16| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let u32b = |v: u32| {
            if little {
                v.to_le_bytes()
            } else {
                v.to_be_bytes()
            }
        };
        let mut out = b"Exif\0\0".to_vec();
        out.extend_from_slice(if little { b"II" } else { b"MM" });
        out.extend_from_slice(&u16b(42));
        out.extend_from_slice(&u32b(8));
        // Two entries: the image width, then the orientation.
        out.extend_from_slice(&u16b(2));
        out.extend_from_slice(&u16b(0x0100));
        out.extend_from_slice(&u16b(4));
        out.extend_from_slice(&u32b(1));
        out.extend_from_slice(&u32b(640));
        out.extend_from_slice(&u16b(0x0112));
        out.extend_from_slice(&u16b(3));
        out.extend_from_slice(&u32b(1));
        out.extend_from_slice(&u16b(orientation));
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&u32b(0));
        out
    }

    #[test]
    fn the_orientation_is_read_in_either_byte_order() {
        assert_eq!(orientation(&exif(6, true)), Some(6));
        assert_eq!(orientation(&exif(8, false)), Some(8));
        assert_eq!(orientation(&exif(3, true)[6..]), Some(3), "a bare TIFF");
        let cut = exif(6, true);
        assert_eq!(orientation(&cut[..cut.len() - 12]), None);
        assert_eq!(orientation(b"Exif\0\0nonsense"), None);
    }
}
