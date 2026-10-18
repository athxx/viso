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
    Png,
    Jpeg,
    Svg,
}

/// How an asset draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AssetKind {
    /// Pixels, sampled from a texture.
    Raster,
    /// Paths, placed into the box at any size.
    Vector,
}

impl AssetFormat {
    /// The format `bytes` are in, read from their signature: the PNG and JPEG
    /// magic numbers, an SVG root element within the document's head.
    pub fn sniff(bytes: &[u8]) -> Option<AssetFormat> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Some(AssetFormat::Png);
        }
        if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            return Some(AssetFormat::Jpeg);
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
        (text.starts_with('<') && text.contains("<svg")).then_some(AssetFormat::Svg)
    }

    pub const fn kind(self) -> AssetKind {
        match self {
            AssetFormat::Png | AssetFormat::Jpeg => AssetKind::Raster,
            AssetFormat::Svg => AssetKind::Vector,
        }
    }

    /// Its common name, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            AssetFormat::Png => "PNG",
            AssetFormat::Jpeg => "JPEG",
            AssetFormat::Svg => "SVG",
        }
    }
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
