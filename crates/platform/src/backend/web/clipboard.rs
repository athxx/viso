//! The browser clipboard. A copy writes plain text with `writeText`, or a
//! `ClipboardItem` of `text/plain`, `text/html` and `image/png` — the PNG
//! handed over as a promise, so the item is written inside the gesture and
//! the pixels encoded after the handler returns. A paste gesture arrives as
//! the `paste` event, whose data is kept until the app says what it takes;
//! a paste the app asks for reads the async clipboard API, which the
//! browser may refuse without a gesture or permission, and is then answered
//! empty.

use js_sys::{Array, Object, Reflect, Uint8Array};
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::JsValue;
use wasm_bindgen_futures::{JsFuture, future_to_promise, spawn_local};
use web_sys::{Blob, BlobPropertyBag, Clipboard, ClipboardItem, DataTransfer, File};

use super::{WINDOW, dom_window, drive, push};
use crate::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds, png};
use crate::control::WindowId;
use crate::event::RawEvent;

/// The clipboard API, where the page may use it.
fn clipboard() -> Option<Clipboard> {
    let clipboard = dom_window().navigator().clipboard();
    (!clipboard.is_undefined()).then_some(clipboard)
}

/// Write `data` to the system clipboard.
pub(super) fn write(data: ClipboardData) {
    let Some(clipboard) = clipboard() else {
        return;
    };
    let image = data
        .image
        .filter(|i| i.rgba_pixels().is_some() || i.encoded_bytes().is_some_and(|b| png::is_png(b)));
    if (data.html.is_some() || image.is_some())
        && let Some(item) = item(data.text.as_deref(), data.html.as_deref(), image)
    {
        let promise = clipboard.write(&Array::of1(&item));
        spawn_local(async move {
            let _ = JsFuture::from(promise).await;
        });
        return;
    }
    if let Some(text) = data.text {
        spawn_local(async move {
            let _ = JsFuture::from(clipboard.write_text(&text)).await;
        });
    }
}

/// A `ClipboardItem` of what there is; `None` where the browser has none.
fn item(text: Option<&str>, html: Option<&str>, image: Option<ClipboardImage>) -> Option<JsValue> {
    let record = Object::new();
    let put = |mime: &str, value: &JsValue| {
        let _ = Reflect::set(&record, &mime.into(), value);
    };
    if let Some(text) = text {
        put("text/plain", blob(text.as_bytes(), "text/plain")?.as_ref());
    }
    if let Some(html) = html {
        put("text/html", blob(html.as_bytes(), "text/html")?.as_ref());
    }
    if let Some(image) = image {
        let png = future_to_promise(async move {
            let file = image.png().ok_or(JsValue::NULL)?;
            blob(&file, "image/png")
                .map(Into::into)
                .ok_or(JsValue::NULL)
        });
        put("image/png", &png);
    }
    ClipboardItem::new_with_record_from_str_to_blob_promise(&record)
        .ok()
        .map(Into::into)
}

fn blob(bytes: &[u8], mime: &str) -> Option<Blob> {
    let bag = BlobPropertyBag::new();
    bag.set_type(mime);
    Blob::new_with_u8_array_sequence_and_options(&Array::of1(&Uint8Array::from(bytes)), &bag).ok()
}

/// Read what the clipboard holds of `kinds` for a request, then deliver it,
/// empty where the browser refused.
pub(super) fn request(window: WindowId, kinds: ClipboardKinds, token: u32) {
    spawn_local(async move {
        let data = read(kinds).await;
        push(RawEvent::Paste {
            window,
            data,
            token: Some(token),
        });
        drive();
    });
}

async fn read(kinds: ClipboardKinds) -> ClipboardData {
    let Some(clipboard) = clipboard() else {
        return ClipboardData::default();
    };
    if (kinds.html || kinds.image)
        && let Some(item) = JsFuture::from(clipboard.read())
            .await
            .ok()
            .and_then(|items| {
                items
                    .unchecked_into::<Array>()
                    .get(0)
                    .dyn_into::<ClipboardItem>()
                    .ok()
            })
    {
        return read_item(&item, kinds).await;
    }
    let mut data = ClipboardData::default();
    if kinds.text {
        data.text = JsFuture::from(clipboard.read_text())
            .await
            .ok()
            .and_then(|t| t.as_string());
    }
    data
}

async fn read_item(item: &ClipboardItem, kinds: ClipboardKinds) -> ClipboardData {
    let types: Vec<String> = item.types().iter().filter_map(|t| t.as_string()).collect();
    let has = |mime: &str| types.iter().any(|t| t == mime);
    let image = types
        .iter()
        .find(|t| t.as_str() == "image/png")
        .or_else(|| types.iter().find(|t| t.starts_with("image/")));
    let pick = kinds.pick(ClipboardKinds {
        text: has("text/plain"),
        html: has("text/html"),
        image: image.is_some(),
    });
    let blob = async |mime: &str| -> Option<Blob> {
        JsFuture::from(item.get_type(mime))
            .await
            .ok()?
            .dyn_into()
            .ok()
    };
    let mut data = ClipboardData::default();
    if pick.text
        && let Some(b) = blob("text/plain").await
    {
        data.text = text(&b).await;
    }
    if pick.html
        && let Some(b) = blob("text/html").await
    {
        data.html = text(&b).await;
    }
    if pick.image
        && let Some(mime) = image
        && let Some(b) = blob(mime).await
    {
        data.image = bytes(&b).await.map(ClipboardImage::encoded);
    }
    data
}

async fn text(blob: &Blob) -> Option<String> {
    JsFuture::from(blob.text()).await.ok()?.as_string()
}

async fn bytes(blob: &Blob) -> Option<Vec<u8>> {
    let buffer = JsFuture::from(blob.array_buffer()).await.ok()?;
    Some(Uint8Array::new(&buffer).to_vec())
}

/// What a `paste` event carried, kept until the app says what it takes.
pub(super) struct Pasting {
    text: Option<String>,
    html: Option<String>,
    /// An image file, read only if the app takes images.
    image: Option<File>,
}

impl Pasting {
    /// What `data` holds; `None` for nothing.
    pub(super) fn of(data: &DataTransfer) -> Option<Pasting> {
        let get = |mime: &str| data.get_data(mime).ok().filter(|t| !t.is_empty());
        let image = data.files().and_then(|files| {
            (0..files.length())
                .filter_map(|i| files.get(i))
                .find(|f| f.type_().starts_with("image/"))
        });
        let pasting = Pasting {
            text: get("text/plain"),
            html: get("text/html"),
            image,
        };
        (pasting.text.is_some() || pasting.html.is_some() || pasting.image.is_some())
            .then_some(pasting)
    }

    /// Deliver what of it a paste of `kinds` takes: at once, or once an
    /// image file it takes is read.
    pub(super) fn finish(self, kinds: ClipboardKinds) {
        let pick = kinds.pick(ClipboardKinds {
            text: self.text.is_some(),
            html: self.html.is_some(),
            image: self.image.is_some(),
        });
        let mut data = ClipboardData {
            text: self.text.filter(|_| pick.text),
            html: self.html.filter(|_| pick.html),
            image: None,
        };
        let paste = |data: ClipboardData| {
            if !data.is_empty() {
                push(RawEvent::Paste {
                    window: WINDOW,
                    data,
                    token: None,
                });
            }
        };
        match self.image.filter(|_| pick.image) {
            Some(file) => spawn_local(async move {
                data.image = bytes(&file).await.map(ClipboardImage::encoded);
                paste(data);
                drive();
            }),
            None => paste(data),
        }
    }
}
