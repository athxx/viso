//! The general pasteboard: one item of `public.utf8-plain-text`,
//! `public.html` and `public.png`. The text and HTML are written at once; a
//! copied image of pixels is encoded to PNG on a worker thread and the item
//! rewritten with it on the main thread, unless something else was copied
//! meanwhile. A paste reads the kinds it takes — an image as the PNG or JPEG
//! file the pasteboard holds, else the image it holds encoded as PNG — and
//! while the app's copy is still the pasteboard's, is that copy, shared.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::Mutex;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_foundation::{NSArray, NSData, NSDictionary, NSInteger, NSString};
use objc2_ui_kit::UIPasteboard;

use crate::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds};

const TEXT: &str = "public.utf8-plain-text";
const HTML: &str = "public.html";
const PNG: &str = "public.png";
const JPEG: &str = "public.jpeg";

/// The app's last copy, and the pasteboard's change count once written.
struct Owned {
    change: NSInteger,
    data: ClipboardData,
}

thread_local! {
    static OWNED: RefCell<Option<Owned>> = const { RefCell::new(None) };
}

/// A copy whose image a worker encoded, waiting for the main thread.
static ENCODED: Mutex<Option<(NSInteger, ClipboardData, Vec<u8>)>> = Mutex::new(None);

fn pasteboard() -> Retained<UIPasteboard> {
    UIPasteboard::generalPasteboard()
}

fn change_count(pasteboard: &UIPasteboard) -> NSInteger {
    // SAFETY: a plain property read.
    unsafe { pasteboard.changeCount() }
}

/// Replace what the pasteboard holds with `data`.
pub(super) fn write(data: ClipboardData) {
    let pasteboard = pasteboard();
    let ready = data.image.as_ref().and_then(|i| match i.rgba_pixels() {
        Some(_) => None,
        None => i.encoded_bytes().map(|b| b.to_vec()),
    });
    set_items(&pasteboard, &data, ready.as_deref());
    let change = change_count(&pasteboard);
    if let Some(image) = data.image.clone().filter(|i| i.rgba_pixels().is_some()) {
        let pending = data.clone();
        let spawned = std::thread::Builder::new()
            .name("viso-copy".into())
            .spawn(move || {
                let Some(png) = image.png() else {
                    return;
                };
                *ENCODED.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some((change, pending, png.to_vec()));
                super::super::main_queue::post_to_main(encoded);
            });
        drop(spawned);
    }
    OWNED.with(|o| *o.borrow_mut() = Some(Owned { change, data }));
}

/// On the main thread: rewrite the item with the image a worker encoded,
/// if the pasteboard still holds the copy it was encoded for.
extern "C" fn encoded(_: *mut c_void) {
    super::guarded(|| {
        let Some((change, data, png)) = ENCODED.lock().unwrap_or_else(|e| e.into_inner()).take()
        else {
            return;
        };
        let pasteboard = pasteboard();
        if change_count(&pasteboard) != change {
            return;
        }
        set_items(&pasteboard, &data, Some(&png));
        let change = change_count(&pasteboard);
        OWNED.with(|o| {
            if let Some(owned) = o.borrow_mut().as_mut() {
                owned.change = change;
            }
        });
    });
}

/// Set one item of `data`'s text and HTML, and of `image` as a PNG or
/// another image file.
fn set_items(pasteboard: &UIPasteboard, data: &ClipboardData, image: Option<&[u8]>) {
    let mut keys: Vec<Retained<NSString>> = Vec::with_capacity(3);
    let mut values: Vec<Retained<AnyObject>> = Vec::with_capacity(3);
    if let Some(text) = &data.text {
        keys.push(NSString::from_str(TEXT));
        values.push(NSString::from_str(text).into());
    }
    if let Some(html) = &data.html {
        keys.push(NSString::from_str(HTML));
        values.push(NSString::from_str(html).into());
    }
    if let Some(bytes) = image {
        let kind = if crate::clipboard::png::is_png(bytes) {
            PNG
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            JPEG
        } else {
            "public.image"
        };
        keys.push(NSString::from_str(kind));
        values.push(NSData::with_bytes(bytes).into());
    }
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let item = NSDictionary::from_retained_objects(&keys, &values);
    let items = NSArray::from_retained_slice(&[item]);
    // SAFETY: every value is an NSString or NSData, as the pasteboard
    // takes; it copies the items.
    unsafe { pasteboard.setItems(&items) };
}

/// What the pasteboard holds of `kinds` ([`ClipboardKinds::pick`]).
pub(super) fn read(kinds: ClipboardKinds) -> ClipboardData {
    let pasteboard = pasteboard();
    let change = change_count(&pasteboard);
    let own = OWNED.with(|o| {
        o.borrow()
            .as_ref()
            .filter(|o| o.change == change)
            .map(|o| o.data.taken(kinds))
    });
    if let Some(own) = own {
        return own;
    }
    let types = |names: &[&str]| -> bool {
        let names: Vec<Retained<NSString>> = names.iter().map(|n| NSString::from_str(n)).collect();
        pasteboard.containsPasteboardTypes(&NSArray::from_retained_slice(&names))
    };
    // SAFETY: plain property reads.
    let held = unsafe {
        ClipboardKinds {
            text: pasteboard.hasStrings(),
            html: types(&[HTML]),
            image: pasteboard.hasImages(),
        }
    };
    let pick = kinds.pick(held);
    let data_of = |kind: &str| pasteboard.dataForPasteboardType(&NSString::from_str(kind));
    let text = pick
        .text
        .then(|| {
            // SAFETY: a plain property read.
            unsafe { pasteboard.string() }
        })
        .flatten()
        .map(|s| s.to_string());
    let html = pick
        .html
        .then(|| data_of(HTML))
        .flatten()
        .map(|d| crate::clipboard::html_text(&d.to_vec()));
    let image = pick
        .image
        .then(|| {
            data_of(PNG).or_else(|| data_of(JPEG)).or_else(|| {
                // SAFETY: a plain property read.
                let image = unsafe { pasteboard.image() }?;
                image.png_representation()
            })
        })
        .flatten()
        .map(|d| ClipboardImage::encoded(d.to_vec()));
    ClipboardData { text, html, image }
}

/// Whether the pasteboard holds anything a paste could take.
pub(super) fn has_content() -> bool {
    let pasteboard = pasteboard();
    // SAFETY: plain property reads.
    unsafe { pasteboard.hasStrings() || pasteboard.hasImages() }
}
