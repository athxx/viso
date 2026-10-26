//! Resource identity: a resource is named by the [`ResourceId`] of its path
//! under the package's `assets/` directory, and every payload it takes on over
//! a session has its own [`ResourceRevision`].

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// A resource, by its path under `assets/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ResourceId(pub u64);

impl ResourceId {
    /// The resource at `path`, already [normalized](normalize_path).
    pub const fn of(path: &str) -> ResourceId {
        let bytes = path.as_bytes();
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut i = 0;
        while i < bytes.len() {
            h ^= bytes[i] as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
            i += 1;
        }
        ResourceId(h)
    }
}

/// One payload of a resource; every new payload takes the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct ResourceRevision(pub u32);

/// `path` as one spelling of a file under `assets/`: `/`-separated, without
/// empty or `.` segments; `None` when it is absolute, climbs out with `..`, or
/// names nothing.
pub fn normalize_path(path: &str) -> Option<String> {
    if path.starts_with('/') || path.starts_with('\\') || path.contains(':') {
        return None;
    }
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            s => out.push(s),
        }
    }
    (!out.is_empty()).then(|| out.join("/"))
}

/// What an asset's bytes are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetFormat {
    /// PNG, animated (APNG) too.
    Png,
    Jpeg,
    /// GIF, animated too.
    Gif,
    /// WebP, lossy or lossless, animated too.
    WebP,
    /// AVIF, animated too.
    Avif,
    Bmp,
    /// A Windows icon: the largest of the images it holds.
    Ico,
    Qoi,
    Svg,
    /// MPEG-4 video: `.mp4`, `.m4v`.
    Mp4,
    /// QuickTime video.
    Mov,
    WebM,
    /// Matroska video.
    Mkv,
    /// A glTF 2.0 model: a binary `.glb`, or a `.gltf` document.
    Gltf,
}

/// How an asset draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// Pixels, sampled from a texture.
    Raster,
    /// Paths, placed into the box at any size.
    Vector,
    /// Pictures the platform's player decodes as it plays.
    Video,
    /// A 3D model a scene draws: meshes, materials, a node hierarchy and its
    /// animations.
    Model,
}

impl AssetFormat {
    /// The format `bytes` are in, read from their signature: each raster
    /// format's magic number — an AVIF's `ftyp` brand, a BMP's header size, an
    /// icon's directory — a binary glTF's magic, or an SVG root element or a
    /// glTF document's top-level keys within the document's head.
    pub fn sniff(bytes: &[u8]) -> Option<AssetFormat> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(AssetFormat::Png);
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(AssetFormat::Jpeg);
        }
        if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            return Some(AssetFormat::Gif);
        }
        if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            return Some(AssetFormat::WebP);
        }
        if is_avif(bytes) {
            return Some(AssetFormat::Avif);
        }
        if let Some(video) = video(bytes) {
            return Some(video);
        }
        if bytes.starts_with(b"qoif") {
            return Some(AssetFormat::Qoi);
        }
        if bytes.len() >= 12 && bytes.starts_with(b"glTF") {
            return Some(AssetFormat::Gltf);
        }
        // A BMP's info header is one of the sizes the versions use.
        if bytes.len() >= 18 && bytes.starts_with(b"BM") {
            let header = u32::from_le_bytes([bytes[14], bytes[15], bytes[16], bytes[17]]);
            if matches!(header, 12 | 16 | 40 | 52 | 56 | 64 | 108 | 124) {
                return Some(AssetFormat::Bmp);
            }
        }
        // An icon's directory: reserved zero, type one, at least one image.
        if bytes.len() >= 22 && bytes.starts_with(&[0, 0, 1, 0]) && (bytes[4] | bytes[5]) != 0 {
            return Some(AssetFormat::Ico);
        }
        let head = &bytes[..bytes.len().min(4096)];
        let text = match core::str::from_utf8(head) {
            Ok(text) => text,
            // The head may end inside a character.
            Err(e) if e.error_len().is_none() => {
                core::str::from_utf8(&head[..e.valid_up_to()]).unwrap_or_default()
            }
            Err(_) => return None,
        };
        let text = text.trim_start_matches('\u{feff}').trim_start();
        if text.starts_with('<') && text.contains("<svg") {
            return Some(AssetFormat::Svg);
        }
        // A glTF document is a JSON object holding its `asset` record, most
        // often first; the arrays every model has stand in for one written
        // later.
        let gltf = ["\"asset\"", "\"accessors\"", "\"bufferViews\""];
        (text.starts_with('{') && gltf.iter().any(|key| text.contains(key)))
            .then_some(AssetFormat::Gltf)
    }

    pub const fn kind(self) -> AssetKind {
        match self {
            AssetFormat::Svg => AssetKind::Vector,
            AssetFormat::Mp4 | AssetFormat::Mov | AssetFormat::WebM | AssetFormat::Mkv => {
                AssetKind::Video
            }
            AssetFormat::Gltf => AssetKind::Model,
            _ => AssetKind::Raster,
        }
    }

    /// Its common name, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            AssetFormat::Png => "PNG",
            AssetFormat::Jpeg => "JPEG",
            AssetFormat::Gif => "GIF",
            AssetFormat::WebP => "WebP",
            AssetFormat::Avif => "AVIF",
            AssetFormat::Bmp => "BMP",
            AssetFormat::Ico => "ICO",
            AssetFormat::Qoi => "QOI",
            AssetFormat::Svg => "SVG",
            AssetFormat::Mp4 => "MP4",
            AssetFormat::Mov => "QuickTime",
            AssetFormat::WebM => "WebM",
            AssetFormat::Mkv => "Matroska",
            AssetFormat::Gltf => "glTF",
        }
    }
}

/// Whether `bytes` open with an ISO media `ftyp` box naming an AVIF brand —
/// `avif` or the image sequence's `avis` — as its major brand or a
/// compatible one.
fn is_avif(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(16, 64).min(bytes.len());
    let avif = |brand: &[u8; 4]| brand == b"avif" || brand == b"avis";
    let (brands, _) = bytes[16..end].as_chunks::<4>();
    // The major brand, the minor version, then the compatible brands.
    bytes[8..12].try_into().is_ok_and(avif) || brands.iter().any(avif)
}

/// The video container `bytes` open with: an ISO media `ftyp` box naming
/// no image brand — QuickTime's `qt  ` as its major brand — or an EBML
/// header, its doc type naming WebM or Matroska.
fn video(bytes: &[u8]) -> Option<AssetFormat> {
    if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        let head = &bytes[..bytes.len().min(64)];
        return Some(if head.windows(4).any(|w| w == b"webm") {
            AssetFormat::WebM
        } else {
            AssetFormat::Mkv
        });
    }
    if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
        return None;
    }
    if &bytes[8..12] == b"qt  " {
        return Some(AssetFormat::Mov);
    }
    // HEIF stills and sequences share the box with video.
    let size = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    let end = size.clamp(12, 64).min(bytes.len());
    let image = |brand: &[u8; 4]| {
        matches!(
            brand,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
        )
    };
    let major: &[u8; 4] = bytes[8..12].try_into().ok()?;
    let (brands, _) = bytes[16.min(end)..end].as_chunks::<4>();
    (!image(major) && !brands.iter().any(image)).then_some(AssetFormat::Mp4)
}

/// An asset's encoded bytes: embedded in the binary, or loaded and shared.
#[derive(Clone)]
pub enum AssetBytes {
    Static(&'static [u8]),
    Shared(Arc<[u8]>),
}

impl core::ops::Deref for AssetBytes {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        match self {
            AssetBytes::Static(bytes) => bytes,
            AssetBytes::Shared(bytes) => bytes,
        }
    }
}

impl AsRef<[u8]> for AssetBytes {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

impl PartialEq for AssetBytes {
    fn eq(&self, other: &AssetBytes) -> bool {
        **self == **other
    }
}

impl Eq for AssetBytes {}

impl core::fmt::Debug for AssetBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "AssetBytes({} bytes)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_raster_format_is_known_by_its_signature() {
        let sniff = AssetFormat::sniff;
        assert_eq!(sniff(b"GIF89a\x01\x00"), Some(AssetFormat::Gif));
        assert_eq!(sniff(b"GIF87a"), Some(AssetFormat::Gif));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8L"), Some(AssetFormat::WebP));
        assert_eq!(sniff(b"RIFF\x10\0\0\0WAVEfmt "), None);
        assert_eq!(
            sniff(b"\0\0\0\x1cftypavif\0\0\0\0avifmif1miaf"),
            Some(AssetFormat::Avif)
        );
        // A sequence names `avis`; HEIC is another brand.
        assert_eq!(
            sniff(b"\0\0\0\x18ftypmif1\0\0\0\0avismsf1"),
            Some(AssetFormat::Avif)
        );
        assert_eq!(sniff(b"\0\0\0\x18ftypheic\0\0\0\0mif1heic"), None);
        assert_eq!(sniff(b"qoif\0\0\0\x01"), Some(AssetFormat::Qoi));
        let mut bmp = alloc::vec::Vec::from(&b"BM"[..]);
        bmp.extend_from_slice(&[0; 12]);
        bmp.extend_from_slice(&40u32.to_le_bytes());
        assert_eq!(sniff(&bmp), Some(AssetFormat::Bmp));
        assert_eq!(sniff(b"BMW is not a bitmap"), None);
        let mut ico = alloc::vec![0, 0, 1, 0, 1, 0];
        ico.extend_from_slice(&[0; 16]);
        assert_eq!(sniff(&ico), Some(AssetFormat::Ico));
        ico[4] = 0;
        assert_eq!(sniff(&ico), None, "no image in it");
        assert_eq!(AssetFormat::Avif.kind(), AssetKind::Raster);
    }

    #[test]
    fn each_video_container_is_known_by_its_signature() {
        let sniff = AssetFormat::sniff;
        assert_eq!(
            sniff(b"\0\0\0\x20ftypisom\0\0\x02\0isomiso2avc1mp41"),
            Some(AssetFormat::Mp4)
        );
        assert_eq!(sniff(b"\0\0\0\x14ftypM4V \0\0\0\0"), Some(AssetFormat::Mp4));
        assert_eq!(
            sniff(b"\0\0\0\x14ftypqt  \0\0\0\0qt  "),
            Some(AssetFormat::Mov)
        );
        assert_eq!(
            sniff(b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\x82\x84webm"),
            Some(AssetFormat::WebM)
        );
        assert_eq!(
            sniff(b"\x1a\x45\xdf\xa3\x9f\x42\x86\x81\x01\x42\x82\x88matroska"),
            Some(AssetFormat::Mkv)
        );
        // A HEIF still or sequence is no video.
        assert_eq!(sniff(b"\0\0\0\x18ftypmsf1\0\0\0\0msf1hevc"), None);
        assert_eq!(sniff(b"\0\0\0\x0cftyp"), None, "no brand");
        assert_eq!(AssetFormat::WebM.kind(), AssetKind::Video);
    }

    #[test]
    fn a_gltf_model_is_known_by_its_magic_or_its_keys() {
        let sniff = AssetFormat::sniff;
        assert_eq!(sniff(b"glTF\x02\0\0\0\x20\0\0\0"), Some(AssetFormat::Gltf));
        assert_eq!(sniff(b"glTF"), None, "no header");
        assert_eq!(
            sniff(b"\xef\xbb\xbf {\"asset\":{\"version\":\"2.0\"}}"),
            Some(AssetFormat::Gltf)
        );
        assert_eq!(
            sniff(b"{\"scene\":0,\"accessors\":[]}"),
            Some(AssetFormat::Gltf)
        );
        assert_eq!(sniff(b"{\"name\":\"not a model\"}"), None);
        assert_eq!(AssetFormat::Gltf.kind(), AssetKind::Model);
    }
}
