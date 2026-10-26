//! The clipboard as nodes reach it: a handler copies content
//! ([`EventCx::copy`](crate::EventCx::copy)), asks for a paste delivered to a
//! node of its choosing ([`EventCx::request_paste`](crate::EventCx::request_paste)),
//! and a node that takes pastes — a paste target ([`NodeStore::set_paste_target`])
//! — hears them through its key handler ([`EventCx::pasted`](crate::EventCx::pasted)).
//!
//! A paste gesture (the shortcut, the edit menu) goes to the focused node:
//! a text field takes plain text into its text as it always does; otherwise
//! the nearest paste target from the focused node up — or from the root,
//! with nothing focused — hears the kinds it takes; with none, the focused
//! node hears the text as an [`ImeEvent::Paste`](crate::ImeEvent::Paste).
//!
//! What another app copied comes as it lies on the clipboard: an image as
//! its file, shown without decoding on the frame path through
//! [`Asset::from`](crate::asset::Asset) or decoded at once with
//! [`decode_image`]; an image the app copies is pixels
//! ([`image_from_raster`]). The facade carries each copy and paste request to
//! the platform, so nothing here waits on the clipboard.

pub use viso_runtime::{ClipboardData, ClipboardImage, ClipboardKinds};

use crate::asset::Asset;
use crate::component::NodeStore;
use crate::node::NodeId;
use viso_render::asset::{AssetError, RasterImage, decode_raster};

/// What a store holds of the clipboard between a handler and the facade.
#[derive(Debug, Default)]
pub(crate) struct Clipboard {
    /// What a handler copied this frame; the last copy wins.
    copied: Option<ClipboardData>,
    /// The paste targets and the kinds each takes.
    targets: Vec<(NodeId, ClipboardKinds)>,
    /// The pastes handlers asked for this frame, for the node each goes to.
    asks: Vec<(NodeId, ClipboardKinds)>,
}

impl Clipboard {
    pub(crate) fn clear(&mut self) {
        self.copied = None;
        self.targets.clear();
        self.asks.clear();
    }
}

impl NodeStore {
    /// Put `data` on the clipboard: the facade hands it to the platform after
    /// the frame's input. The last of a frame wins.
    pub fn copy(&mut self, data: ClipboardData) {
        self.clipboard.copied = Some(data);
    }

    /// Put `text` on the clipboard ([`copy`](Self::copy)).
    pub fn copy_text(&mut self, text: String) {
        self.copy(ClipboardData::text(text));
    }

    /// What was put on the clipboard since this was last asked, if anything.
    pub fn take_copied(&mut self) -> Option<ClipboardData> {
        self.clipboard.copied.take()
    }

    /// Make node `id` take pastes of `kinds` — a gesture's when the focus is
    /// in it — or none with `None`.
    pub fn set_paste_target(&mut self, id: NodeId, kinds: Option<ClipboardKinds>) {
        let live = self.arena().is_live(id);
        let targets = &mut self.clipboard.targets;
        targets.retain(|(n, _)| *n != id);
        if let Some(kinds) = kinds.filter(|_| live) {
            targets.push((id, kinds));
        }
    }

    /// The kinds node `id` takes pastes of, if it is a paste target.
    pub fn paste_target(&self, id: NodeId) -> Option<ClipboardKinds> {
        self.clipboard
            .targets
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, k)| *k)
    }

    /// The paste target a gesture from `from` goes to: `from` or its nearest
    /// ancestor that takes pastes, with the kinds it takes.
    pub fn paste_target_from(&self, from: NodeId) -> Option<(NodeId, ClipboardKinds)> {
        if self.clipboard.targets.is_empty() {
            return None;
        }
        let arena = self.arena();
        let mut at = Some(from).filter(|&n| arena.is_live(n));
        while let Some(node) = at {
            if let Some(kinds) = self.paste_target(node) {
                return Some((node, kinds));
            }
            at = arena.parent(node);
        }
        None
    }

    /// Ask for what the clipboard holds of `kinds`, delivered to node `to`'s
    /// key handler ([`EventCx::pasted`](crate::EventCx::pasted)) once read.
    pub fn ask_paste(&mut self, to: NodeId, kinds: ClipboardKinds) {
        self.clipboard.asks.push((to, kinds));
    }

    /// Move the pastes asked for since this was last called into `out`,
    /// clearing it first.
    pub fn take_paste_asks(&mut self, out: &mut Vec<(NodeId, ClipboardKinds)>) {
        out.clear();
        out.append(&mut self.clipboard.asks);
    }
}

/// `image` as pixels the clipboard takes: its texels decoded to RGBA8 where
/// it is in another format, and its alpha straightened; `None` when its
/// texels are not its format's size.
pub fn image_from_raster(image: &RasterImage) -> Option<ClipboardImage> {
    let pixels = image.to_straight_rgba8()?;
    ClipboardImage::rgba(image.width, image.height, pixels)
}

/// `image` decoded as an asset decodes: premultiplied RGBA8, bounded as
/// every raster is.
///
/// # Errors
///
/// Bytes in no raster format Viso reads, a decoder error, or an image past
/// the limits.
pub fn decode_image(image: &ClipboardImage) -> Result<RasterImage, AssetError> {
    if let Some(bytes) = image.encoded_bytes() {
        return decode_raster(bytes);
    }
    let (width, height, pixels) = image
        .rgba_pixels()
        .ok_or_else(|| AssetError::Decode("an image of no pixels".to_owned()))?;
    RasterImage::from_straight_rgba8(width, height, pixels)
}

impl From<&ClipboardImage> for Asset {
    /// The image as an asset a node shows, decoded off the frame path: a
    /// file shares its bytes; pixels are premultiplied into a converted
    /// image, which needs no decode.
    fn from(image: &ClipboardImage) -> Asset {
        if let Some(bytes) = image.encoded_bytes() {
            return Asset::bytes(bytes.clone());
        }
        match decode_image(image) {
            Ok(raster) => Asset::bytes(viso_render::asset::texture::write(&raster)),
            Err(_) => Asset::bytes(Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, LeafStyle, StackStyle};

    #[test]
    fn a_gesture_goes_to_the_nearest_paste_target() {
        let mut store = NodeStore::new();
        let (mut child, mut leaf) = (None, None);
        let root = BuildCx::new(&mut store)
            .stack(StackStyle::default(), |cx| {
                child = Some(
                    cx.stack(StackStyle::default(), |cx| {
                        leaf = Some(cx.leaf(LeafStyle::default()).id());
                    })
                    .id(),
                );
            })
            .id();
        let (child, leaf) = (child.unwrap(), leaf.unwrap());
        assert_eq!(store.paste_target_from(leaf), None);
        store.set_paste_target(root, Some(ClipboardKinds::ALL));
        store.set_paste_target(child, Some(ClipboardKinds::TEXT));
        assert_eq!(
            store.paste_target_from(leaf),
            Some((child, ClipboardKinds::TEXT))
        );
        store.set_paste_target(child, None);
        assert_eq!(
            store.paste_target_from(leaf),
            Some((root, ClipboardKinds::ALL))
        );

        store.ask_paste(leaf, ClipboardKinds::ALL);
        let mut asks = Vec::new();
        store.take_paste_asks(&mut asks);
        assert_eq!(asks, vec![(leaf, ClipboardKinds::ALL)]);
        store.take_paste_asks(&mut asks);
        assert!(asks.is_empty());

        store.copy_text("a".into());
        store.copy(ClipboardData::html("<b>b</b>", "b"));
        assert_eq!(
            store.take_copied(),
            Some(ClipboardData::html("<b>b</b>", "b"))
        );
        assert_eq!(store.take_copied(), None);
    }

    #[test]
    fn an_image_crosses_to_the_clipboard_and_back() {
        let raster =
            RasterImage::from_straight_rgba8(2, 1, &[255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let image = image_from_raster(&raster).unwrap();
        let (w, h, pixels) = image.rgba_pixels().unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(&pixels[..4], &[255, 0, 0, 255]);
        assert_eq!(pixels[7], 128);
        assert!((i16::from(pixels[6]) - 255).abs() <= 1, "{pixels:?}");
        assert_eq!(decode_image(&image).unwrap(), raster);

        let file = ClipboardImage::encoded(image.png().unwrap());
        let decoded = decode_image(&file).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        let asset = Asset::from(&file);
        let crate::asset::AssetSource::Bytes(bytes) = &asset.source else {
            panic!("bytes at hand");
        };
        assert_eq!(&bytes[..], &file.encoded_bytes().unwrap()[..]);
        let converted = Asset::from(&image);
        let crate::asset::AssetSource::Bytes(bytes) = &converted.source else {
            panic!("bytes at hand");
        };
        assert!(bytes.starts_with(b"VTX1"));
    }
}
