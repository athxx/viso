//! DOM input: pointer and wheel events on the canvas; keys, text,
//! composition and clipboard events on the hidden input element.
//!
//! Text never accumulates in the input element. Typed text is taken from
//! `beforeinput` and the edit cancelled; a composition is left to the
//! browser (cancelling it breaks the IME) and mirrored as preedit until
//! `compositionend` commits it, after which the element is emptied again.
//! Soft keyboards that send key code 229 for everything are recognized, and
//! their line breaks and deletions are turned back into key presses.

use js_sys::Reflect;
use wasm_bindgen::JsCast;
use wasm_bindgen::prelude::wasm_bindgen;
use web_sys::{
    ClipboardEvent, CompositionEvent, Event, InputEvent, KeyboardEvent, PointerEvent, WheelEvent,
};

use super::{LOOP, Listener, Page, WINDOW, current_page, drive, push, write_clipboard};
use crate::backend::utf16::byte_offset;
use crate::backend::web_translate::{
    buttons, key_code, modifiers, page_claims, pointer_id, pointer_kind, pointer_phase,
    track_buttons, wheel_delta,
};
use crate::control::LogicalRect;
use crate::event::{
    ClipboardReply, ClipboardShortcut, KeyCode, Modifiers, PointerButtons, PointerKind,
    PointerPhase, RawEvent, RawImePreedit, RawKey, RawPointer, RawScroll, RawText,
    clipboard_shortcut,
};

/// The key code browsers report for a key an IME or soft keyboard consumed.
const PROCESS_KEY: u32 = 229;

#[wasm_bindgen]
extern "C" {
    /// A `MouseEvent`, read for its fractional client coordinates (the
    /// generated bindings round them to integers).
    type FractionalMouse;

    #[wasm_bindgen(method, getter, js_name = clientX)]
    fn client_x(this: &FractionalMouse) -> f64;

    #[wasm_bindgen(method, getter, js_name = clientY)]
    fn client_y(this: &FractionalMouse) -> f64;
}

/// Register the canvas and input element listeners.
pub(super) fn listen(target: &Page) -> Vec<Listener> {
    let canvas = &target.canvas;
    let input = &target.input;
    let with_page = |f: fn(&Page, &Event)| {
        move |e: Event| {
            if let Some(page) = current_page() {
                f(&page, &e);
            }
        }
    };
    let mut listeners: Vec<Listener> = [
        "pointerdown",
        "pointermove",
        "pointerup",
        "pointercancel",
        "pointerleave",
    ]
    .into_iter()
    .map(|kind| Listener::new(canvas, kind, with_page(on_pointer)))
    .collect();
    listeners.extend([
        Listener::new(canvas, "wheel", with_page(on_wheel)),
        Listener::new(canvas, "contextmenu", |e| e.prevent_default()),
        Listener::new(input, "keydown", with_page(on_key_down)),
        Listener::new(input, "keyup", with_page(on_key_up)),
        Listener::new(input, "beforeinput", with_page(on_before_input)),
        Listener::new(input, "input", with_page(on_input)),
        Listener::new(
            input,
            "compositionstart",
            with_page(|page, _| page.composing.set(true)),
        ),
        Listener::new(input, "compositionend", with_page(on_composition_end)),
        Listener::new(input, "copy", with_page(on_copy)),
        Listener::new(input, "cut", with_page(on_copy)),
        Listener::new(input, "paste", with_page(on_paste)),
    ]);
    listeners
}

fn event_modifiers(e: &web_sys::MouseEvent) -> Modifiers {
    modifiers(e.shift_key(), e.ctrl_key(), e.alt_key(), e.meta_key())
}

/// The buttons last reported for pointer `id`.
fn buttons_of(page: &Page, id: i32) -> PointerButtons {
    page.buttons
        .borrow()
        .iter()
        .find(|(p, _)| *p == id)
        .map_or(PointerButtons::NONE, |(_, b)| *b)
}

fn on_pointer(page: &Page, e: &Event) {
    let e: &PointerEvent = e.unchecked_ref();
    let kind = pointer_kind(&e.pointer_type());
    let id = e.pointer_id();
    let now = buttons(e.buttons());
    let before = buttons_of(page, id);
    let Some(phase) = pointer_phase(&e.type_(), kind, e.button(), before, now) else {
        return;
    };
    let ended =
        kind == PointerKind::Touch && matches!(phase, PointerPhase::Up | PointerPhase::Cancel);
    track_buttons(&mut page.buttons.borrow_mut(), id, now, ended);

    if e.type_() == "pointerdown" {
        e.prevent_default();
        page.focus_input();
        let _ = page.canvas.set_pointer_capture(id);
    }

    let (left, top) = page.origin();
    let pointer = pointer_id(kind, id);
    let modifiers = event_modifiers(e);
    let sample = |s: &PointerEvent, phase| {
        let at: &FractionalMouse = s.unchecked_ref();
        RawEvent::Pointer(RawPointer {
            window: WINDOW,
            pointer,
            kind,
            x: at.client_x() - left,
            y: at.client_y() - top,
            pressure: s.pressure(),
            buttons: now,
            modifiers,
            phase,
        })
    };
    if phase == PointerPhase::Moved {
        // Every sample the browser merged into this event, oldest first.
        let coalesced = Reflect::has(e, &"getCoalescedEvents".into())
            .unwrap_or(false)
            .then(|| e.get_coalesced_events())
            .filter(|samples| samples.length() > 0);
        match coalesced {
            Some(samples) => {
                for s in samples.iter() {
                    push(sample(s.unchecked_ref(), phase));
                }
            }
            None => push(sample(e, phase)),
        }
    } else {
        push(sample(e, phase));
    }
    if matches!(phase, PointerPhase::Down | PointerPhase::Up) {
        // Handle the press within the gesture, so what it triggers (focus,
        // the soft keyboard, fullscreen) keeps the user activation.
        push(RawEvent::RedrawRequested { window: WINDOW });
    }
    drive();
}

fn on_wheel(page: &Page, e: &Event) {
    let e: &WheelEvent = e.unchecked_ref();
    e.prevent_default();
    let (left, top) = page.origin();
    let at: &FractionalMouse = e.unchecked_ref();
    let (delta_x, delta_y) = wheel_delta(
        e.delta_x(),
        e.delta_y(),
        e.delta_mode(),
        page.logical_size(),
    );
    push(RawEvent::Scroll(RawScroll {
        window: WINDOW,
        x: at.client_x() - left,
        y: at.client_y() - top,
        delta_x,
        delta_y,
        modifiers: event_modifiers(e),
    }));
    drive();
}

fn key_modifiers(e: &KeyboardEvent) -> Modifiers {
    modifiers(e.shift_key(), e.ctrl_key(), e.alt_key(), e.meta_key())
}

fn push_key(code: KeyCode, pressed: bool, repeat: bool, modifiers: Modifiers) {
    push(RawEvent::Key(RawKey {
        window: WINDOW,
        code,
        pressed,
        repeat,
        modifiers,
    }));
}

fn on_key_down(page: &Page, e: &Event) {
    let e: &KeyboardEvent = e.unchecked_ref();
    LOOP.with(|l| l.copied.borrow_mut().take());
    if e.is_composing() || e.key_code() == PROCESS_KEY {
        page.unidentified.set(true);
        return;
    }
    page.unidentified.set(false);
    let code = key_code(&e.code(), &e.key(), e.key_code());
    let modifiers = key_modifiers(e);
    push_key(code, true, e.repeat(), modifiers);
    if page_claims(code, modifiers) {
        e.prevent_default();
    }
    let shortcut = clipboard_shortcut(code, modifiers);
    let copy = matches!(
        shortcut,
        Some(ClipboardShortcut::Copy | ClipboardShortcut::Cut)
    );
    if copy {
        push(RawEvent::CopyRequested {
            window: WINDOW,
            cut: shortcut == Some(ClipboardShortcut::Cut),
            reply: ClipboardReply::new(),
        });
    }
    push(RawEvent::RedrawRequested { window: WINDOW });
    drive();
    if copy {
        // Written here as well as in the `copy` event that follows: some
        // browsers skip that event when the input element has no selection.
        let text = LOOP.with(|l| l.copied.borrow().clone().flatten());
        if let Some(text) = text {
            write_clipboard(text);
        }
    }
}

fn on_key_up(_page: &Page, e: &Event) {
    let e: &KeyboardEvent = e.unchecked_ref();
    if e.is_composing() || e.key_code() == PROCESS_KEY {
        return;
    }
    let code = key_code(&e.code(), &e.key(), e.key_code());
    push_key(code, false, false, key_modifiers(e));
    LOOP.with(|l| l.copied.borrow_mut().take());
    drive();
}

/// A press and release of `code`, standing in for a key the keyboard did
/// not name.
fn synthesize(code: KeyCode) {
    push_key(code, true, false, Modifiers::default());
    push_key(code, false, false, Modifiers::default());
}

fn on_before_input(page: &Page, e: &Event) {
    let e: &InputEvent = e.unchecked_ref();
    let stand_in = |code| {
        if page.unidentified.get() {
            synthesize(code);
        }
    };
    match e.input_type().as_str() {
        "insertText" | "insertReplacementText" => {
            if let Some(text) = e.data().filter(|t| !t.is_empty()) {
                push(RawEvent::Text(RawText {
                    window: WINDOW,
                    text,
                }));
            }
        }
        // The IME edits the element itself; `input` mirrors the result.
        "insertCompositionText" => return,
        "insertLineBreak" | "insertParagraph" => stand_in(KeyCode::Enter),
        "deleteContentBackward" => stand_in(KeyCode::Backspace),
        "deleteContentForward" => stand_in(KeyCode::Delete),
        // Pastes arrive as `paste` events; everything else would only edit
        // the hidden element.
        _ => {}
    }
    e.prevent_default();
    drive();
}

fn on_input(page: &Page, _e: &Event) {
    let value = page.input.value();
    if page.composing.get() {
        let units = page
            .input
            .selection_end()
            .ok()
            .flatten()
            .map_or(value.encode_utf16().count(), |u| u as usize);
        let caret = byte_offset(&value, units);
        push(RawEvent::ImePreedit(RawImePreedit {
            window: WINDOW,
            text: value,
            caret,
        }));
        drive();
    } else if !value.is_empty() {
        page.input.set_value("");
    }
}

fn on_composition_end(page: &Page, e: &Event) {
    let e: &CompositionEvent = e.unchecked_ref();
    page.composing.set(false);
    push(RawEvent::ImePreedit(RawImePreedit {
        window: WINDOW,
        text: String::new(),
        caret: 0,
    }));
    if let Some(text) = e.data().filter(|t| !t.is_empty()) {
        push(RawEvent::Text(RawText {
            window: WINDOW,
            text,
        }));
    }
    page.input.set_value("");
    drive();
}

fn on_copy(_page: &Page, e: &Event) {
    let e: &ClipboardEvent = e.unchecked_ref();
    let mut answer = LOOP.with(|l| l.copied.borrow_mut().take());
    if answer.is_none() {
        // A copy from the browser's menu: no key press asked first.
        push(RawEvent::CopyRequested {
            window: WINDOW,
            cut: e.type_() == "cut",
            reply: ClipboardReply::new(),
        });
        drive();
        answer = LOOP.with(|l| l.copied.borrow_mut().take());
    }
    if let (Some(text), Some(data)) = (answer.flatten(), e.clipboard_data()) {
        let _ = data.set_data("text/plain", &text);
        e.prevent_default();
    }
}

fn on_paste(_page: &Page, e: &Event) {
    let e: &ClipboardEvent = e.unchecked_ref();
    e.prevent_default();
    let text = e
        .clipboard_data()
        .and_then(|data| data.get_data("text/plain").ok())
        .filter(|t| !t.is_empty());
    if let Some(text) = text {
        push(RawEvent::Paste {
            window: WINDOW,
            text,
        });
        drive();
    }
}

/// Place the input element over the caret, so the IME's candidate window
/// opens beside it; `None` ends text input, cancelling any composition.
pub(super) fn set_ime_area(page: &Page, caret: Option<LogicalRect>) {
    match caret {
        Some(r) => {
            let (left, top) = page.origin();
            let style = page.input.style();
            let _ = style.set_property("left", &format!("{}px", left + r.x));
            let _ = style.set_property("top", &format!("{}px", top + r.y));
            let _ = style.set_property("height", &format!("{}px", r.height.max(1.0)));
        }
        None if page.composing.get() => {
            let _ = page.input.blur();
            page.input.set_value("");
            page.focus_input();
        }
        None => {}
    }
}

/// Switch the input element between a text field and a keyless focus
/// target. Refocusing makes the browser re-read `inputmode`, which opens or
/// closes the soft keyboard.
pub(super) fn show_soft_keyboard(page: &Page, show: bool) {
    if page.soft_keyboard.replace(show) == show {
        return;
    }
    let _ = page
        .input
        .set_attribute("inputmode", if show { "text" } else { "none" });
    let _ = page.input.blur();
    page.focus_input();
}
