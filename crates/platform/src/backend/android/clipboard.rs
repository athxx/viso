//! The Android clipboard: one clip item of text, HTML and an image, the
//! image a PNG file the app's content provider serves to whoever pastes it.
//! A copy with an image is encoded and stored on a worker thread, then set
//! unless a later copy came first. Every paste is read by the Java shell on
//! a thread of its own and answered through the inbox; a paste while the
//! clipboard still holds the app's last copy is that copy, shared.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

use super::{jni, push_to_window};
use crate::clipboard::{ClipboardData, ClipboardKinds};
use crate::control::WindowId;
use crate::event::RawEvent;

/// A paste waiting for the shell's answer.
struct Ask {
    id: i64,
    window: WindowId,
    kinds: ClipboardKinds,
    token: Option<u32>,
}

#[derive(Default)]
struct State {
    /// The app's last copy.
    own: Option<ClipboardData>,
    asks: Vec<Ask>,
    next: i64,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

/// The number of copies made, so a slow image copy yields to a later one.
static COPIES: AtomicU64 = AtomicU64::new(0);

/// Put `data` on the clipboard.
pub(super) fn write(data: ClipboardData) {
    let copy = COPIES.fetch_add(1, Ordering::AcqRel) + 1;
    let label = format!("viso:{}:{copy}", std::process::id());
    STATE.with(|s| s.borrow_mut().own = Some(data.clone()));
    let Some(image) = data.image.clone() else {
        jni::set_clipboard(data.text.as_deref(), data.html.as_deref(), None, &label);
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("viso-copy".into())
        .spawn(move || {
            if !jni::attach_current_thread(c"viso-copy") {
                return;
            }
            let png = image.png();
            if COPIES.load(Ordering::Acquire) == copy {
                jni::set_clipboard(
                    data.text.as_deref(),
                    data.html.as_deref(),
                    png.as_deref(),
                    &label,
                );
            }
            jni::detach_current_thread();
        });
    drop(spawned);
}

/// Read what the clipboard holds of `kinds` for `window`: for a request
/// (`token`) always answered, for a gesture only when it holds some.
pub(super) fn request(window: WindowId, kinds: ClipboardKinds, token: Option<u32>) {
    let id = STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.next += 1;
        let id = s.next;
        s.asks.push(Ask {
            id,
            window,
            kinds,
            token,
        });
        id
    });
    if !kinds.any() || !jni::request_clipboard(kinds.image, id) {
        answered(id, false, ClipboardData::default());
    }
}

/// The shell answered paste `id`: the app's own copy, or what it `read`.
pub(super) fn answered(id: i64, own: bool, read: ClipboardData) {
    let Some((ask, data)) = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let at = s.asks.iter().position(|a| a.id == id)?;
        let ask = s.asks.swap_remove(at);
        let data = match (own, &s.own) {
            (true, Some(own)) => own.taken(ask.kinds),
            _ => read.taken(ask.kinds),
        };
        Some((ask, data))
    }) else {
        return;
    };
    if ask.token.is_some() || !data.is_empty() {
        push_to_window(RawEvent::Paste {
            window: ask.window,
            data,
            token: ask.token,
        });
    }
}
