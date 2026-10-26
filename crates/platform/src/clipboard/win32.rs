//! The Win32 clipboard formats that are not plain text: `HTML Format`, a
//! UTF-8 document behind a header of byte offsets, and the device-independent
//! bitmaps `CF_DIB` / `CF_DIBV5`, read as the BMP file they make behind a
//! file header and written from straight-alpha pixels.

/// `html` as `HTML Format`: the header, then a document whose body is the
/// fragment.
pub(crate) fn cf_html(html: &str) -> Vec<u8> {
    const PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
    const SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";
    // The header's numbers are fixed-width, so its length is known first.
    let header = |start_html: usize, end_html: usize, start: usize, end: usize| {
        format!(
            "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\n\
             StartFragment:{start:010}\r\nEndFragment:{end:010}\r\n"
        )
    };
    let head = header(0, 0, 0, 0).len();
    let start = head + PREFIX.len();
    let end = start + html.len();
    let total = end + SUFFIX.len();
    let mut out = header(head, total, start, end).into_bytes();
    out.reserve(total - head + 1);
    out.extend_from_slice(PREFIX.as_bytes());
    out.extend_from_slice(html.as_bytes());
    out.extend_from_slice(SUFFIX.as_bytes());
    out.push(0);
    out
}

/// The fragment `HTML Format` bytes hold: between the header's
/// `StartFragment` and `EndFragment` offsets, or the fragment comments
/// where the offsets are missing or wrong, or the whole document after the
/// header.
pub(crate) fn html_fragment(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let bytes = &bytes[..end];
    let field = |name: &str| -> Option<usize> {
        let at = find(bytes, name.as_bytes())? + name.len();
        let digits = bytes[at..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        std::str::from_utf8(&bytes[at..at + digits])
            .ok()?
            .parse()
            .ok()
    };
    let by_offsets = field("StartFragment:")
        .zip(field("EndFragment:"))
        .filter(|&(s, e)| s <= e && e <= bytes.len());
    let by_comments = || {
        let open = b"<!--StartFragment-->";
        let s = find(bytes, open)? + open.len();
        let e = s + find(&bytes[s..], b"<!--EndFragment-->")?;
        Some((s, e))
    };
    let whole = || {
        let s = field("StartHTML:").filter(|&s| s <= bytes.len())?;
        Some((
            s,
            field("EndHTML:")
                .filter(|&e| s <= e && e <= bytes.len())
                .unwrap_or(bytes.len()),
        ))
    };
    let (s, e) = by_offsets.or_else(by_comments).or_else(whole)?;
    Some(String::from_utf8_lossy(&bytes[s..e]).into_owned())
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// A `CF_DIBV5` of `width × height` straight-alpha RGBA8 `pixels`, rows top
/// first: a `BITMAPV5HEADER` of 32-bit BGRA with an alpha mask, in sRGB,
/// rows bottom first as every reader takes them.
pub(crate) fn dibv5(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let stride = width as usize * 4;
    let mut out = Vec::with_capacity(124 + pixels.len());
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(124);
    put(width);
    put(height);
    // One plane, 32 bits a pixel.
    put(1 | 32 << 16);
    put(BI_BITFIELDS);
    put(u32::try_from(pixels.len()).unwrap_or(0));
    put(2835);
    put(2835);
    put(0);
    put(0);
    put(0x00FF_0000);
    put(0x0000_FF00);
    put(0x0000_00FF);
    put(0xFF00_0000);
    // `LCS_sRGB`, the endpoints and gamma it makes moot, then
    // `LCS_GM_IMAGES` and no profile.
    put(u32::from_be_bytes(*b"sRGB"));
    for _ in 0..12 {
        put(0);
    }
    put(4);
    put(0);
    put(0);
    put(0);
    for row in pixels.chunks_exact(stride).rev() {
        for &[r, g, b, a] in row.as_chunks::<4>().0 {
            out.extend_from_slice(&[b, g, r, a]);
        }
    }
    out
}

/// The BMP file a `CF_DIB` or `CF_DIBV5` makes behind a file header; `None`
/// for one too short to hold its header. A 32-bit bitmap whose alpha is
/// nothing anywhere — what many apps write, meaning no alpha — reads as
/// opaque.
pub(crate) fn bmp_file(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?))
    };
    let header = u32_at(0)? as usize;
    if header < 12 || dib.len() < header {
        return None;
    }
    let (bits, compression, used) = if header == 12 {
        let bits = u16::from_le_bytes(dib.get(10..12)?.try_into().ok()?);
        (u32::from(bits), 0, 0)
    } else {
        let bits = u32::from(u16::from_le_bytes(dib.get(14..16)?.try_into().ok()?));
        (bits, u32_at(16)?, u32_at(32)?)
    };
    // The masks after a plain info header, then the palette.
    let masks = match (header, compression) {
        (40, BI_BITFIELDS) => 12,
        (40, BI_ALPHABITFIELDS) => 16,
        _ => 0,
    };
    let entry = if header == 12 { 3 } else { 4 };
    let colors = match used {
        0 if bits <= 8 => 1usize << bits,
        n => n as usize,
    };
    let offset = 14 + header + masks + colors * entry;
    let total = 14 + dib.len();
    let mut file = Vec::with_capacity(total);
    file.extend_from_slice(b"BM");
    file.extend_from_slice(&u32::try_from(total).ok()?.to_le_bytes());
    file.extend_from_slice(&[0; 4]);
    file.extend_from_slice(&u32::try_from(offset).ok()?.to_le_bytes());
    file.extend_from_slice(dib);
    // An alpha mask over alpha that is nothing everywhere means none.
    let mask_at = match (header, compression) {
        (40, BI_ALPHABITFIELDS) => Some(14 + 40 + 12),
        (h, BI_BITFIELDS | BI_ALPHABITFIELDS) if h >= 56 => Some(14 + 52),
        _ => None,
    };
    if bits == 32
        && let Some(mask_at) = mask_at
        && let Some(mask) = file.get(mask_at..mask_at + 4)
    {
        let mask = u32::from_le_bytes(mask.try_into().ok()?);
        let pixels = file.get(offset..).unwrap_or_default();
        if mask != 0
            && pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| u32::from_le_bytes(*p) & mask == 0)
        {
            file[mask_at..mask_at + 4].fill(0);
        }
    }
    Some(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_format_offsets_frame_the_fragment() {
        let html = "<b>caf\u{e9}</b> 中";
        let bytes = cf_html(html);
        let text = std::str::from_utf8(&bytes[..bytes.len() - 1]).unwrap();
        let field = |name: &str| -> usize {
            let at = text.find(name).unwrap() + name.len();
            text[at..at + 10].parse().unwrap()
        };
        assert_eq!(&text[field("StartFragment:")..field("EndFragment:")], html);
        assert!(text[field("StartHTML:")..].starts_with("<html>"));
        assert_eq!(field("EndHTML:"), text.len());
        assert_eq!(html_fragment(&bytes).as_deref(), Some(html));
    }

    #[test]
    fn a_fragment_is_found_by_its_comments_when_the_offsets_lie() {
        let bytes = b"Version:0.9\r\nStartHTML:-1\r\nEndHTML:-1\r\nStartFragment:999\r\n\
                      EndFragment:9999\r\n<html><body><!--StartFragment--><i>x</i>\
                      <!--EndFragment--></body></html>";
        assert_eq!(html_fragment(bytes).as_deref(), Some("<i>x</i>"));
        assert_eq!(html_fragment(b"nothing"), None);
    }

    #[test]
    fn a_dibv5_reads_back_as_its_bmp_file() {
        let pixels = [255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 0, 9, 9, 9, 255];
        let dib = dibv5(2, 2, &pixels);
        assert_eq!(dib.len(), 124 + 16);
        // Bottom row first, BGRA.
        assert_eq!(&dib[124..128], &[255, 0, 0, 0]);
        assert_eq!(&dib[132..136], &[0, 0, 255, 255]);
        let file = bmp_file(&dib).unwrap();
        assert_eq!(&file[..2], b"BM");
        assert_eq!(
            u32::from_le_bytes(file[10..14].try_into().unwrap()),
            14 + 124
        );
        assert_eq!(&file[14..], &dib[..], "alpha that is there is kept");
    }

    #[test]
    fn alpha_that_is_nothing_everywhere_reads_as_opaque() {
        let mut dib = dibv5(1, 1, &[1, 2, 3, 0]);
        let file = bmp_file(&dib).unwrap();
        assert_eq!(&file[14 + 52..14 + 56], &[0; 4]);
        // A plain info header with masks, and a palette after it.
        dib.clear();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&[0; 10]);
        dib.extend_from_slice(&8u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        let file = bmp_file(&dib).unwrap();
        assert_eq!(
            u32::from_le_bytes(file[10..14].try_into().unwrap()),
            14 + 40 + 256 * 4
        );
        assert_eq!(bmp_file(&[1, 2]), None);
    }
}
