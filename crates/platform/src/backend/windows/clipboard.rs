//! The Win32 clipboard: `CF_UNICODETEXT`, `HTML Format`, and an image as the
//! registered `PNG` format beside `CF_DIBV5`. A copied image of pixels is
//! promised (delayed rendering) and encoded only when another app asks for a
//! format, through the owner window's `WM_RENDERFORMAT`, or for all of them
//! as the owner goes (`WM_RENDERALLFORMATS`). A paste reads the kinds it
//! takes, an image as the `PNG` file or a DIB as the BMP file it makes; while
//! the app still owns the clipboard, a paste is its own copy, shared.

use std::cell::RefCell;
use std::time::Duration;

use ::windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use ::windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardOwner,
    IsClipboardFormatAvailable, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use ::windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use ::windows::core::w;

use super::translate;
use crate::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds, win32};

const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_DIBV5: u32 = 17;

/// The app's last copy while its window owns the clipboard.
struct Owned {
    owner: HWND,
    data: ClipboardData,
}

thread_local! {
    static OWNED: RefCell<Option<Owned>> = const { RefCell::new(None) };
}

fn html_format() -> u32 {
    // SAFETY: registering (or looking up) a format by a static name.
    unsafe { RegisterClipboardFormatW(w!("HTML Format")) }
}

fn png_format() -> u32 {
    // SAFETY: as in `html_format`.
    unsafe { RegisterClipboardFormatW(w!("PNG")) }
}

/// The clipboard, open for the life of the value. Another process may hold
/// it briefly, so opening retries for a few milliseconds.
struct Session;

impl Session {
    fn open(owner: Option<HWND>) -> Option<Self> {
        for _ in 0..10 {
            // SAFETY: opening the clipboard for this thread; closed on drop.
            if unsafe { OpenClipboard(owner) }.is_ok() {
                return Some(Self);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        None
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard in `open`.
        let _ = unsafe { CloseClipboard() };
    }
}

/// Hand `bytes` to the open clipboard as `format`.
///
/// # Safety
///
/// The clipboard is open on this thread, or this answers a
/// `WM_RENDERFORMAT`.
unsafe fn put(format: u32, bytes: &[u8]) {
    // SAFETY: the block is sized for `bytes`, filled while locked, and handed
    // to the clipboard, which owns it once `SetClipboardData` succeeds; on
    // any failure it is freed here.
    unsafe {
        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1)) else {
            return;
        };
        let dst = GlobalLock(block).cast::<u8>();
        if dst.is_null() {
            let _ = GlobalFree(Some(block));
            return;
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len());
        let _ = GlobalUnlock(block);
        if SetClipboardData(format, Some(HANDLE(block.0))).is_err() {
            let _ = GlobalFree(Some(block));
        }
    }
}

/// What the open clipboard holds as `format`.
///
/// # Safety
///
/// The clipboard is open on this thread.
unsafe fn get(format: u32) -> Option<Vec<u8>> {
    // SAFETY: the clipboard is open, so its data handle stays valid until it
    // closes. The locked block holds `GlobalSize` bytes; the slice stays
    // within them and ends before the unlock.
    unsafe {
        let data = GetClipboardData(format).ok()?;
        let block = HGLOBAL(data.0);
        let src = GlobalLock(block).cast::<u8>().cast_const();
        if src.is_null() {
            return None;
        }
        let bytes = std::slice::from_raw_parts(src, GlobalSize(block)).to_vec();
        let _ = GlobalUnlock(block);
        Some(bytes)
    }
}

fn available(format: u32) -> bool {
    // SAFETY: a format query with no pointers.
    unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

/// Replace what the clipboard holds with `data`, `owner` owning it. An
/// image of pixels is promised when there is an owner to render it.
pub(super) fn write(owner: Option<HWND>, data: ClipboardData) {
    let Some(_session) = Session::open(owner) else {
        return;
    };
    // SAFETY: the clipboard is open on this thread.
    unsafe {
        if EmptyClipboard().is_err() {
            return;
        }
        if let Some(text) = &data.text {
            let units = translate::clipboard_units(text);
            put(CF_UNICODETEXT, bytes_of(&units));
        }
        if let Some(html) = &data.html {
            put(html_format(), &win32::cf_html(html));
        }
        if let Some(image) = &data.image {
            if image.rgba_pixels().is_some() && owner.is_some() {
                // Promised: rendered on `WM_RENDERFORMAT`.
                let _ = SetClipboardData(png_format(), None);
                let _ = SetClipboardData(CF_DIBV5, None);
            } else {
                render_image(image, None);
            }
        }
    }
    OWNED.with(|o| {
        *o.borrow_mut() = owner.map(|owner| Owned { owner, data });
    });
}

/// Render `image` as `format`, or as every format it goes as with `None`.
///
/// # Safety
///
/// As [`put`].
unsafe fn render_image(image: &ClipboardImage, format: Option<u32>) {
    let png = png_format();
    if format.is_none_or(|f| f == png)
        && let Some(file) = image.png()
    {
        // SAFETY: per the function contract.
        unsafe { put(png, &file) };
    }
    if format.is_none_or(|f| f == CF_DIBV5)
        && let Some((width, height, pixels)) = image.rgba_pixels()
    {
        // SAFETY: per the function contract.
        unsafe { put(CF_DIBV5, &win32::dibv5(width, height, pixels)) };
    }
}

/// `WM_RENDERFORMAT`: another app asks for `format` of the image `hwnd`
/// promised.
pub(super) fn render_format(hwnd: HWND, format: u32) {
    let image = OWNED.with(|o| {
        o.borrow()
            .as_ref()
            .filter(|o| o.owner == hwnd)
            .and_then(|o| o.data.image.clone())
    });
    if let Some(image) = image {
        // SAFETY: answering `WM_RENDERFORMAT`, where the clipboard is the
        // asking app's to read and must not be opened.
        unsafe { render_image(&image, Some(format)) };
    }
}

/// `WM_RENDERALLFORMATS`: `hwnd`, the owner, is going; what it promised is
/// rendered now, while it still owns the clipboard.
pub(super) fn render_all(hwnd: HWND) {
    let Some(image) = OWNED.with(|o| {
        o.borrow()
            .as_ref()
            .filter(|o| o.owner == hwnd)
            .and_then(|o| o.data.image.clone())
    }) else {
        return;
    };
    let Some(_session) = Session::open(Some(hwnd)) else {
        return;
    };
    // SAFETY: a query with no pointers, then rendering into the clipboard
    // this thread opened.
    unsafe {
        if GetClipboardOwner().ok() == Some(hwnd) {
            render_image(&image, None);
        }
    }
}

/// `WM_DESTROYCLIPBOARD`: another app emptied the clipboard `hwnd` owned.
pub(super) fn forget(hwnd: HWND) {
    OWNED.with(|o| {
        let mut owned = o.borrow_mut();
        if owned.as_ref().is_some_and(|o| o.owner == hwnd) {
            *owned = None;
        }
    });
}

/// What the clipboard holds of `kinds` ([`ClipboardKinds::pick`]).
pub(super) fn read(owner: Option<HWND>, kinds: ClipboardKinds) -> ClipboardData {
    // SAFETY: a query with no pointers.
    let current = unsafe { GetClipboardOwner() }.ok();
    let own = OWNED.with(|o| {
        o.borrow()
            .as_ref()
            .filter(|o| Some(o.owner) == current)
            .map(|o| o.data.taken(kinds))
    });
    if let Some(own) = own {
        return own;
    }
    let (html, png) = (html_format(), png_format());
    let held = ClipboardKinds {
        text: available(CF_UNICODETEXT),
        html: available(html),
        image: available(png) || available(CF_DIBV5) || available(CF_DIB),
    };
    let pick = kinds.pick(held);
    if !pick.any() {
        return ClipboardData::default();
    }
    let Some(_session) = Session::open(owner) else {
        return ClipboardData::default();
    };
    // SAFETY: the clipboard is open on this thread.
    unsafe {
        let text = pick
            .text
            .then(|| get(CF_UNICODETEXT))
            .flatten()
            .map(|bytes| {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|p| u16::from_le_bytes(*p))
                    .collect();
                translate::clipboard_text(&units)
            });
        let html = pick
            .html
            .then(|| get(html))
            .flatten()
            .and_then(|bytes| win32::html_fragment(&bytes));
        let image = if pick.image {
            get(png)
                .or_else(|| get(CF_DIBV5).and_then(|d| win32::bmp_file(&d)))
                .or_else(|| get(CF_DIB).and_then(|d| win32::bmp_file(&d)))
                .map(ClipboardImage::encoded)
        } else {
            None
        };
        ClipboardData { text, html, image }
    }
}

fn bytes_of(units: &[u16]) -> &[u8] {
    // SAFETY: `u16` has no padding and any byte pattern is a valid `u8`; the
    // slice covers exactly the units' bytes and borrows them.
    unsafe { std::slice::from_raw_parts(units.as_ptr().cast(), units.len() * 2) }
}
