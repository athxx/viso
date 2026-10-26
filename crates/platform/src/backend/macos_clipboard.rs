//! The general pasteboard: a copy writes one item holding the text, the HTML
//! and the image, the image of pixels promised as a PNG file and encoded only
//! when someone asks for it; a paste reads the kinds it takes, an image as the
//! PNG or JPEG file the pasteboard holds or any other image it holds
//! converted to PNG. While the pasteboard still holds the app's own copy, a
//! paste is that copy, shared.

use objc2::rc::Retained;
use objc2::runtime::{NSObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSPasteboard, NSPasteboardItem,
    NSPasteboardItemDataProvider, NSPasteboardType, NSPasteboardTypeHTML, NSPasteboardTypePNG,
    NSPasteboardTypeString, NSPasteboardTypeTIFF, NSPasteboardWriting,
};
use objc2_foundation::{NSArray, NSData, NSDictionary, NSObjectProtocol, NSString};

use crate::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds};

/// The app's last copy, while the pasteboard may still hold it.
pub(super) struct Owned {
    /// The pasteboard's change count once the copy was written: a later one
    /// means another app wrote since.
    change: isize,
    data: ClipboardData,
    /// The promise of the copy's image, kept until the copy is replaced.
    provider: Option<Retained<ImageProvider>>,
}

/// The pasteboard type names, AppKit constants.
fn types() -> (
    &'static NSPasteboardType,
    &'static NSPasteboardType,
    &'static NSPasteboardType,
    &'static NSPasteboardType,
) {
    // SAFETY: the pasteboard type names are immutable AppKit constants,
    // initialized before any Rust code runs.
    unsafe {
        (
            NSPasteboardTypeString,
            NSPasteboardTypeHTML,
            NSPasteboardTypePNG,
            NSPasteboardTypeTIFF,
        )
    }
}

/// Replace what `pasteboard` holds with `data`.
pub(super) fn write(pasteboard: &NSPasteboard, data: ClipboardData) -> Owned {
    let (string, html, png, _) = types();
    pasteboard.clearContents();
    let item = NSPasteboardItem::new();
    if let Some(text) = &data.text {
        item.setString_forType(&NSString::from_str(text), string);
    }
    if let Some(markup) = &data.html {
        item.setString_forType(&NSString::from_str(markup), html);
    }
    let mut provider = None;
    match data.image.as_ref() {
        Some(image) if image.rgba_pixels().is_some() => {
            let promise = ImageProvider::new(image.clone());
            let as_provider =
                ProtocolObject::<dyn NSPasteboardItemDataProvider>::from_ref(&*promise);
            item.setDataProvider_forTypes(as_provider, &NSArray::from_slice(&[png]));
            provider = Some(promise);
        }
        Some(image) => {
            if let Some(file) = image.png() {
                item.setData_forType(&NSData::with_bytes(&file), png);
            }
        }
        None => {}
    }
    let items = [ProtocolObject::<dyn NSPasteboardWriting>::from_retained(
        item,
    )];
    pasteboard.writeObjects(&NSArray::from_retained_slice(&items));
    Owned {
        change: pasteboard.changeCount(),
        data,
        provider,
    }
}

/// Keep the promise of `owned`'s image before the app goes, while the
/// pasteboard still holds it: asking for the image has the provider write it.
pub(super) fn settle(pasteboard: &NSPasteboard, owned: &Owned) {
    if owned.provider.is_some() && pasteboard.changeCount() == owned.change {
        let (_, _, png, _) = types();
        let _ = pasteboard.dataForType(png);
    }
}

/// What `pasteboard` holds of `kinds` ([`ClipboardKinds::pick`]); the app's
/// own copy `owned`, shared, while it is still what it holds.
pub(super) fn read(
    pasteboard: &NSPasteboard,
    kinds: ClipboardKinds,
    owned: Option<&Owned>,
) -> ClipboardData {
    if let Some(owned) = owned.filter(|o| o.change == pasteboard.changeCount()) {
        return owned.data.taken(kinds);
    }
    let (string, html, png, tiff) = types();
    let jpeg = NSString::from_str("public.jpeg");
    let Some(held_types) = pasteboard.types() else {
        return ClipboardData::default();
    };
    let has = |t: &NSPasteboardType| held_types.containsObject(t);
    let held = ClipboardKinds {
        text: has(string),
        html: has(html),
        image: has(png) || has(&jpeg) || has(tiff),
    };
    let pick = kinds.pick(held);
    let text = |t: &NSPasteboardType| pasteboard.stringForType(t).map(|s| s.to_string());
    ClipboardData {
        text: text(string).filter(|_| pick.text),
        html: text(html).filter(|_| pick.html),
        image: pick.image.then(|| read_image(pasteboard)).flatten(),
    }
}

/// The image `pasteboard` holds: its PNG or JPEG file, or its TIFF
/// converted to PNG.
fn read_image(pasteboard: &NSPasteboard) -> Option<ClipboardImage> {
    let (_, _, png, tiff) = types();
    let jpeg = NSString::from_str("public.jpeg");
    let bytes = |data: Retained<NSData>| {
        // SAFETY: the data is the pasteboard's copy, which nothing mutates
        // while it is read here.
        ClipboardImage::encoded(unsafe { data.as_bytes_unchecked() })
    };
    if let Some(data) = pasteboard.dataForType(png) {
        return Some(bytes(data));
    }
    if let Some(data) = pasteboard.dataForType(&jpeg) {
        return Some(bytes(data));
    }
    let data = pasteboard.dataForType(tiff)?;
    let rep = NSBitmapImageRep::imageRepWithData(&data)?;
    let properties = NSDictionary::new();
    // SAFETY: an empty property dictionary is valid for every file type.
    let file =
        unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) }?;
    Some(bytes(file))
}

/// The provider of a copied image's PNG file, encoded when first asked for.
pub(super) struct ImageIvars {
    image: ClipboardImage,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VisoPasteboardImage"]
    #[ivars = ImageIvars]
    pub(super) struct ImageProvider;

    // SAFETY: an `NSObject` subclass, which conforms to `NSObject`.
    unsafe impl NSObjectProtocol for ImageProvider {}

    // SAFETY: the one required method is implemented below with the
    // signature the protocol declares.
    unsafe impl NSPasteboardItemDataProvider for ImageProvider {
        #[unsafe(method(pasteboard:item:provideDataForType:))]
        fn provide(
            &self,
            _pasteboard: Option<&NSPasteboard>,
            item: &NSPasteboardItem,
            kind: &NSPasteboardType,
        ) {
            let image = &self.ivars().image;
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if let Some(file) = image.png() {
                    item.setData_forType(&NSData::with_bytes(&file), kind);
                }
            }));
        }
    }
);

impl ImageProvider {
    fn new(image: ClipboardImage) -> Retained<Self> {
        let this = Self::alloc().set_ivars(ImageIvars { image });
        // SAFETY: `NSObject`'s `init` on a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Retained<NSPasteboard> {
        NSPasteboard::pasteboardWithUniqueName()
    }

    #[test]
    fn a_copy_reads_back_from_the_pasteboard_once_another_app_holds_it() {
        let pasteboard = board();
        let image = ClipboardImage::rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 64]).unwrap();
        let data = ClipboardData {
            image: Some(image.clone()),
            ..ClipboardData::html("<b>hi</b>", "hi")
        };
        let owned = write(&pasteboard, data.clone());
        let own = read(&pasteboard, ClipboardKinds::ALL, Some(&owned));
        assert!(
            own.image.unwrap().same(&image),
            "the app's own copy, shared"
        );

        // As another app sees it: the promised PNG is the image encoded.
        let other = read(&pasteboard, ClipboardKinds::ALL, None);
        assert_eq!(other.text.as_deref(), Some("hi"));
        assert_eq!(other.html, None, "the image is the richer");
        let file = other.image.unwrap();
        assert_eq!(file.encoded_bytes(), image.png().as_ref());
        let html = read(
            &pasteboard,
            ClipboardKinds {
                image: false,
                ..ClipboardKinds::ALL
            },
            None,
        );
        assert_eq!(html, ClipboardData::html("<b>hi</b>", "hi"));
        settle(&pasteboard, &owned);
    }

    #[test]
    fn a_tiff_reads_as_png() {
        let pasteboard = board();
        let (_, _, png, tiff) = types();
        let file = ClipboardImage::rgba(1, 1, vec![1, 2, 3, 255])
            .unwrap()
            .png()
            .unwrap();
        let rep = NSBitmapImageRep::imageRepWithData(&NSData::with_bytes(&file)).unwrap();
        let tiff_data = rep.TIFFRepresentation().unwrap();
        pasteboard.clearContents();
        pasteboard.setData_forType(Some(&tiff_data), tiff);
        assert_eq!(pasteboard.dataForType(png), None);
        let read = read(&pasteboard, ClipboardKinds::ALL, None);
        let image = read.image.unwrap();
        assert!(crate::clipboard::png::is_png(
            image.encoded_bytes().unwrap()
        ));
        assert_eq!(read.text, None);
    }
}
