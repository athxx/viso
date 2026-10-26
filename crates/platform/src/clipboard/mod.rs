//! What the clipboard carries: plain text, HTML beside its plain-text
//! alternative, and an image; the kinds a paste takes; and the handshakes a
//! copy or paste gesture makes with the app.
//!
//! An image the app copies is straight-alpha RGBA8 pixels, written as a PNG
//! file where the OS keeps encoded images (and encoded only when someone
//! asks for it where the OS lets the copy wait for that); one another app
//! copied arrives as the file the OS holds, decoded by whoever shows it. All
//! of it is shared, not copied, as it moves from the OS to the app.
//!
//! A paste names the kinds it takes and gets the richest of them the
//! clipboard holds — an image over HTML — along with the plain text, when it
//! takes text and there is some.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

pub(crate) mod png;
#[cfg(any(target_os = "windows", test))]
pub(crate) mod win32;

/// The kinds of content a paste takes, or a clipboard holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ClipboardKinds {
    pub text: bool,
    pub html: bool,
    pub image: bool,
}

impl ClipboardKinds {
    /// No kind.
    pub const NONE: ClipboardKinds = ClipboardKinds {
        text: false,
        html: false,
        image: false,
    };
    /// Plain text alone, what a text field takes.
    pub const TEXT: ClipboardKinds = ClipboardKinds {
        text: true,
        html: false,
        image: false,
    };
    /// Every kind.
    pub const ALL: ClipboardKinds = ClipboardKinds {
        text: true,
        html: true,
        image: true,
    };

    /// Whether it names any kind.
    pub fn any(self) -> bool {
        self.text || self.html || self.image
    }

    /// What a paste taking these kinds reads of a clipboard holding
    /// `held`: the richest kind both name — an image over HTML — and the
    /// text, when both name it.
    pub fn pick(self, held: ClipboardKinds) -> ClipboardKinds {
        let image = self.image && held.image;
        ClipboardKinds {
            text: self.text && held.text,
            html: !image && self.html && held.html,
            image,
        }
    }
}

/// Content on the clipboard: any of plain text, HTML and an image. HTML
/// goes with its plain-text alternative, for whoever takes text alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClipboardData {
    pub text: Option<String>,
    pub html: Option<String>,
    pub image: Option<ClipboardImage>,
}

impl ClipboardData {
    /// Plain text.
    pub fn text(text: impl Into<String>) -> ClipboardData {
        ClipboardData {
            text: Some(text.into()),
            ..ClipboardData::default()
        }
    }

    /// HTML, with `text` its plain-text alternative.
    pub fn html(html: impl Into<String>, text: impl Into<String>) -> ClipboardData {
        ClipboardData {
            text: Some(text.into()),
            html: Some(html.into()),
            image: None,
        }
    }

    /// An image.
    pub fn image(image: ClipboardImage) -> ClipboardData {
        ClipboardData {
            image: Some(image),
            ..ClipboardData::default()
        }
    }

    /// It with `text` as its plain text.
    pub fn with_text(mut self, text: impl Into<String>) -> ClipboardData {
        self.text = Some(text.into());
        self
    }

    /// Whether it holds nothing.
    pub fn is_empty(&self) -> bool {
        !self.kinds().any()
    }

    /// The kinds it holds.
    pub fn kinds(&self) -> ClipboardKinds {
        ClipboardKinds {
            text: self.text.is_some(),
            html: self.html.is_some(),
            image: self.image.is_some(),
        }
    }

    /// What of it a paste taking `kinds` gets ([`ClipboardKinds::pick`]).
    pub fn taken(&self, kinds: ClipboardKinds) -> ClipboardData {
        let pick = kinds.pick(self.kinds());
        ClipboardData {
            text: self.text.clone().filter(|_| pick.text),
            html: self.html.clone().filter(|_| pick.html),
            image: self.image.clone().filter(|_| pick.image),
        }
    }
}

/// An image on the clipboard: an image file's bytes — PNG, JPEG, BMP, GIF,
/// WebP, … — as another app or the OS put it there, or straight-alpha sRGB
/// RGBA8 pixels, rows top first, as the app copies them. Cloning shares it.
#[derive(Clone)]
pub struct ClipboardImage(Arc<Image>);

struct Image {
    form: Form,
    /// The PNG file of an image of pixels, once encoded.
    png: OnceLock<Arc<[u8]>>,
}

enum Form {
    Encoded(Arc<[u8]>),
    Rgba {
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
}

impl ClipboardImage {
    /// An image file's bytes.
    pub fn encoded(bytes: impl Into<Arc<[u8]>>) -> ClipboardImage {
        ClipboardImage(Arc::new(Image {
            form: Form::Encoded(bytes.into()),
            png: OnceLock::new(),
        }))
    }

    /// `width × height` straight-alpha sRGB RGBA8 pixels, rows top first;
    /// `None` when there are none, or `pixels` is not `width × height × 4`
    /// bytes.
    pub fn rgba(width: u32, height: u32, pixels: Vec<u8>) -> Option<ClipboardImage> {
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (width > 0 && height > 0 && pixels.len() == len).then(|| {
            ClipboardImage(Arc::new(Image {
                form: Form::Rgba {
                    width,
                    height,
                    pixels,
                },
                png: OnceLock::new(),
            }))
        })
    }

    /// Its file's bytes, when it is a file.
    pub fn encoded_bytes(&self) -> Option<&Arc<[u8]>> {
        match &self.0.form {
            Form::Encoded(bytes) => Some(bytes),
            Form::Rgba { .. } => None,
        }
    }

    /// Its size and pixels, when it is pixels.
    pub fn rgba_pixels(&self) -> Option<(u32, u32, &[u8])> {
        match &self.0.form {
            Form::Encoded(_) => None,
            Form::Rgba {
                width,
                height,
                pixels,
            } => Some((*width, *height, pixels)),
        }
    }

    /// It as a PNG file: its own bytes when it is one, its pixels encoded
    /// otherwise — once, whoever asks first; `None` for a file in another
    /// format.
    pub fn png(&self) -> Option<Arc<[u8]>> {
        match &self.0.form {
            Form::Encoded(bytes) => png::is_png(bytes).then(|| bytes.clone()),
            Form::Rgba {
                width,
                height,
                pixels,
            } => Some(
                self.0
                    .png
                    .get_or_init(|| png::encode(*width, *height, pixels).into())
                    .clone(),
            ),
        }
    }

    /// Whether `self` and `other` share one image.
    pub fn same(&self, other: &ClipboardImage) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl PartialEq for ClipboardImage {
    fn eq(&self, other: &Self) -> bool {
        self.same(other)
            || match (&self.0.form, &other.0.form) {
                (Form::Encoded(a), Form::Encoded(b)) => a == b,
                (
                    Form::Rgba {
                        width,
                        height,
                        pixels,
                    },
                    Form::Rgba {
                        width: w,
                        height: h,
                        pixels: p,
                    },
                ) => (width, height, pixels) == (w, h, p),
                _ => false,
            }
    }
}

impl Eq for ClipboardImage {}

impl std::fmt::Debug for ClipboardImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0.form {
            Form::Encoded(bytes) => write!(f, "ClipboardImage({} encoded bytes)", bytes.len()),
            Form::Rgba { width, height, .. } => write!(f, "ClipboardImage({width}×{height} RGBA)"),
        }
    }
}

/// The reply slot of a [`RawEvent::CopyRequested`](crate::RawEvent)
/// handshake.
///
/// The handler puts the content to copy in it; after the handler returns the
/// backend writes whatever it holds to the system clipboard. Left empty, the
/// clipboard is untouched.
#[derive(Debug, Clone, Default)]
pub struct ClipboardReply(Rc<RefCell<Option<ClipboardData>>>);

impl PartialEq for ClipboardReply {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl ClipboardReply {
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer the request with `data`.
    pub fn set(&self, data: ClipboardData) {
        *self.0.borrow_mut() = Some(data);
    }

    /// Take the answer, leaving the slot empty.
    pub fn take(&self) -> Option<ClipboardData> {
        self.0.borrow_mut().take()
    }

    /// The answer, left in the slot; an image in it is shared.
    pub fn get(&self) -> Option<ClipboardData> {
        self.0.borrow().clone()
    }
}

/// The reply slot of a [`RawEvent::PasteRequested`](crate::RawEvent)
/// handshake: the kinds the focused content takes, plain text until the
/// handler says otherwise. After the handler returns the backend reads them
/// and delivers what it read as a [`RawEvent::Paste`](crate::RawEvent);
/// [`ClipboardKinds::NONE`] reads nothing.
#[derive(Debug, Clone)]
pub struct PasteReply(Rc<Cell<ClipboardKinds>>);

impl Default for PasteReply {
    fn default() -> Self {
        PasteReply(Rc::new(Cell::new(ClipboardKinds::TEXT)))
    }
}

impl PartialEq for PasteReply {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl PasteReply {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, kinds: ClipboardKinds) {
        self.0.set(kinds);
    }

    pub fn get(&self) -> ClipboardKinds {
        self.0.get()
    }
}

/// HTML a clipboard holds as bytes, as text: UTF-16 behind its byte-order
/// mark, as some browsers write it, else UTF-8, a `NUL` and what follows it
/// dropped.
#[cfg(any(
    target_os = "linux",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly",
    test
))]
pub(crate) fn html_text(bytes: &[u8]) -> String {
    let wide = |le: bool| {
        let units: Vec<u16> = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| {
                if le {
                    u16::from_le_bytes(p)
                } else {
                    u16::from_be_bytes(p)
                }
            })
            .take_while(|&u| u != 0)
            .collect();
        String::from_utf16_lossy(&units)
    };
    match bytes {
        [0xFF, 0xFE, ..] => wide(true),
        [0xFE, 0xFF, ..] => wide(false),
        _ => {
            let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
            let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
            String::from_utf8_lossy(&bytes[..end]).into_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_takes_the_richest_kind_it_names_and_the_text() {
        let held = ClipboardKinds::ALL;
        assert_eq!(
            ClipboardKinds::ALL.pick(held),
            ClipboardKinds {
                text: true,
                html: false,
                image: true
            }
        );
        let no_image = ClipboardKinds {
            image: false,
            ..ClipboardKinds::ALL
        };
        assert_eq!(
            ClipboardKinds::ALL.pick(no_image),
            ClipboardKinds {
                text: true,
                html: true,
                image: false
            }
        );
        assert_eq!(ClipboardKinds::TEXT.pick(held), ClipboardKinds::TEXT);
        assert_eq!(
            ClipboardKinds::TEXT.pick(ClipboardKinds::NONE),
            ClipboardKinds::NONE
        );

        let image = ClipboardImage::rgba(1, 1, vec![1, 2, 3, 4]).unwrap();
        let data = ClipboardData::html("<b>hi</b>", "hi");
        let data = ClipboardData {
            image: Some(image.clone()),
            ..data
        };
        let taken = data.taken(ClipboardKinds::ALL);
        assert_eq!(taken.text.as_deref(), Some("hi"));
        assert_eq!(taken.html, None);
        assert!(taken.image.unwrap().same(&image), "shared, not copied");
        assert_eq!(data.taken(ClipboardKinds::TEXT), ClipboardData::text("hi"));
    }

    #[test]
    fn pixels_are_checked_and_encoded_once() {
        assert!(ClipboardImage::rgba(2, 2, vec![0; 15]).is_none());
        assert!(ClipboardImage::rgba(0, 2, Vec::new()).is_none());
        let image = ClipboardImage::rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let png = image.png().unwrap();
        assert!(png::is_png(&png));
        assert!(Arc::ptr_eq(&png, &image.png().unwrap()));
        let file = ClipboardImage::encoded(png.clone());
        assert!(Arc::ptr_eq(&file.png().unwrap(), &png), "a PNG is itself");
        assert_eq!(ClipboardImage::encoded(&b"\xFF\xD8\xFF"[..]).png(), None);
    }

    #[test]
    fn html_bytes_read_as_utf8_or_marked_utf16() {
        assert_eq!(html_text(b"<b>h\xC3\xA9</b>\0junk"), "<b>hé</b>");
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("<i>中</i>".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(html_text(&utf16), "<i>中</i>");
    }
}
