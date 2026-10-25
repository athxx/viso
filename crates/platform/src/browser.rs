//! What the browser does for a page that the web build carries no code of:
//! decoding the image formats it links no decoder for (AVIF).

use js_sys::{Array, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, CanvasRenderingContext2d, HtmlCanvasElement, ImageBitmap};

/// A picture the browser decoded: `width × height` straight-alpha sRGB RGBA8
/// pixels, rows top first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `bytes`, an image of MIME type `mime`, decoded by the browser's own
/// decoder — the first frame of an animation — or why it did not decode.
pub async fn decode_image(bytes: &[u8], mime: &str) -> Result<DecodedImage, String> {
    let window = web_sys::window().ok_or("no window")?;
    let parts = Array::of1(&Uint8Array::from(bytes));
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options).map_err(error)?;
    let decoding = window.create_image_bitmap_with_blob(&blob).map_err(error)?;
    let bitmap: ImageBitmap = JsFuture::from(decoding)
        .await
        .map_err(error)?
        .unchecked_into();
    let (width, height) = (bitmap.width(), bitmap.height());
    let pixels = draw(&window, &bitmap, width, height);
    bitmap.close();
    Ok(DecodedImage {
        width,
        height,
        rgba: pixels?,
    })
}

/// The pixels of `bitmap`, read back through a canvas of its size.
fn draw(
    window: &web_sys::Window,
    bitmap: &ImageBitmap,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let document = window.document().ok_or("no document")?;
    let canvas: HtmlCanvasElement = document
        .create_element("canvas")
        .map_err(error)?
        .unchecked_into();
    canvas.set_width(width);
    canvas.set_height(height);
    let context: CanvasRenderingContext2d = canvas
        .get_context("2d")
        .map_err(error)?
        .ok_or("no 2D canvas")?
        .unchecked_into();
    context
        .draw_image_with_image_bitmap(bitmap, 0.0, 0.0)
        .map_err(error)?;
    let data = context
        .get_image_data(0.0, 0.0, f64::from(width), f64::from(height))
        .map_err(error)?;
    Ok(data.data().0)
}

fn error(value: JsValue) -> String {
    value
        .dyn_ref::<js_sys::Error>()
        .map(|e| String::from(e.message()))
        .or_else(|| value.as_string())
        .unwrap_or_else(|| format!("{value:?}"))
}
