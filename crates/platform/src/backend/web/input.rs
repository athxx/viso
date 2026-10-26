//! DOM input: pointer, wheel and drag-and-drop events on the canvas; keys,
//! text, composition and clipboard events on the hidden input element.
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
    ClipboardEvent, CompositionEvent, DataTransfer, DragEvent, Event, InputEvent, KeyboardEvent,
    PointerEvent, WheelEvent,
};

use super::{LOOP, Listener, Page, WINDOW, clipboard, current_page, drive, push};
use crate::backend::utf16::byte_offset;
use crate::backend::web_translate::{
    buttons, drop_actions, drop_effect, drop_kinds, key_code, modifiers, page_claims, pointer_id,
    pointer_kind, pointer_phase, track_buttons, wheel_delta,
};
use crate::clipboard::{ClipboardReply, PasteReply};
use crate::control::{InputKind, TextEntry};
use crate::drop::{DropOffer, DropPhase, DropReply, RawDrop};
use crate::event::{
    ClipboardShortcut, KeyCode, Modifiers, PointerButtons, PointerKind, PointerPhase, RawEvent,
    RawImePreedit, RawKey, RawPointer, RawScroll, RawText, clipboard_shortcut,
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
        Listener::new(canvas, "gesturestart", with_page(on_gesture)),
        Listener::new(canvas, "gesturechange", with_page(on_gesture)),
        Listener::new(canvas, "gestureend", with_page(on_gesture)),
        Listener::new(canvas, "contextmenu", |e| e.prevent_default()),
        Listener::new(canvas, "dragenter", with_page(on_drag)),
        Listener::new(canvas, "dragover", with_page(on_drag)),
        Listener::new(canvas, "dragleave", with_page(on_drag)),
        Listener::new(canvas, "drop", with_page(on_drag)),
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
        phase: crate::ScrollPhase::None,
    }));
    drive();
}

/// A drag from outside the page, or from elsewhere on it, over the canvas:
/// each step is driven through the app at once, and the drop effect it
/// answers is the browser's answer to the step. The drop is the browser's
/// to cancel, so a file let go over the canvas never opens in the tab.
fn on_drag(page: &Page, e: &Event) {
    let e: &DragEvent = e.unchecked_ref();
    let Some(data) = e.data_transfer() else {
        return;
    };
    let kind = e.type_();
    let phase = match kind.as_str() {
        "dragleave" => DropPhase::Leave,
        "drop" => DropPhase::Drop,
        _ => DropPhase::Over,
    };
    if phase != DropPhase::Leave {
        e.prevent_default();
    }
    let reply = if phase == DropPhase::Over {
        page.arrival
            .borrow_mut()
            .get_or_insert_with(DropReply::new)
            .clone()
    } else {
        page.arrival.borrow_mut().take().unwrap_or_default()
    };
    let offer = if phase == DropPhase::Drop {
        dropped(&data)
    } else {
        let types: Vec<String> = data.types().iter().filter_map(|t| t.as_string()).collect();
        DropOffer {
            kinds: drop_kinds(types.iter().map(String::as_str)),
            ..DropOffer::default()
        }
    };
    let (left, top) = page.origin();
    let at: &FractionalMouse = e.unchecked_ref();
    push(RawEvent::Drop(RawDrop {
        window: WINDOW,
        phase,
        x: at.client_x() - left,
        y: at.client_y() - top,
        offer,
        reply: reply.clone(),
    }));
    drive();
    if phase != DropPhase::Leave {
        let allowed = drop_actions(&data.effect_allowed());
        data.set_drop_effect(drop_effect(allowed.settle(reply.get())));
    }
}

/// What a drop holds: its files, by name (a page never learns their paths),
/// its first link and its text.
fn dropped(data: &DataTransfer) -> DropOffer {
    let read = |format| data.get_data(format).ok().filter(|t| !t.is_empty());
    let (mut files, uri) = read("text/uri-list").map_or_default(|l| crate::drop::uri_list(&l));
    if let Some(list) = data.files() {
        files.extend(
            (0..list.length())
                .filter_map(|i| list.get(i))
                .map(|f| f.name().into()),
        );
    }
    let text = read("text/plain").or_else(|| uri.clone());
    DropOffer {
        kinds: crate::DropKinds {
            text: text.is_some(),
            uri: uri.is_some(),
            files: !files.is_empty(),
        },
        text,
        uri,
        files,
    }
}

/// Safari's trackpad pinch and turn, whose events tell the scale and the turn
/// (degrees, clockwise) since the gesture began. Fingers on the glass pinch
/// as pointers, so a gesture while one is down is only kept from zooming the
/// page.
fn on_gesture(page: &Page, e: &Event) {
    use crate::event::{GesturePhase, RawGesture};
    e.prevent_default();
    if !page.buttons.borrow().is_empty() {
        return;
    }
    let read = |name: &str| {
        Reflect::get(e, &name.into())
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or_default()
    };
    let (scale, turn) = (read("scale"), read("rotation"));
    let (phase, (before_scale, before_turn)) = match (e.type_().as_str(), page.gesture.get()) {
        ("gesturestart", _) => (GesturePhase::Began, (1.0, 0.0)),
        ("gesturechange", Some(last)) => (GesturePhase::Changed, last),
        ("gestureend", Some(last)) => (GesturePhase::Ended, last),
        _ => return,
    };
    let scale = if scale > 0.0 { scale } else { before_scale };
    page.gesture
        .set((phase != GesturePhase::Ended).then_some((scale, turn)));
    // A gesture event is no mouse event: its fields are read by name.
    let held = |name: &str| {
        Reflect::get(e, &name.into())
            .ok()
            .and_then(|v| v.as_bool())
            .unwrap_or_default()
    };
    let (left, top) = page.origin();
    push(RawEvent::Gesture(RawGesture {
        window: WINDOW,
        x: read("clientX") - left,
        y: read("clientY") - top,
        scale: scale / before_scale,
        rotation: (turn - before_turn).to_radians(),
        modifiers: modifiers(
            held("shiftKey"),
            held("ctrlKey"),
            held("altKey"),
            held("metaKey"),
        ),
        phase,
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
        let data = LOOP.with(|l| l.copied.borrow().clone().flatten());
        if let Some(data) = data {
            clipboard::write(data);
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
    let menu = answer.is_none();
    if menu {
        // A copy from the browser's menu: no key press asked first.
        push(RawEvent::CopyRequested {
            window: WINDOW,
            cut: e.type_() == "cut",
            reply: ClipboardReply::new(),
        });
        drive();
        answer = LOOP.with(|l| l.copied.borrow_mut().take());
    }
    let Some(copied) = answer.flatten() else {
        return;
    };
    if let Some(data) = e.clipboard_data() {
        if let Some(text) = &copied.text {
            let _ = data.set_data("text/plain", text);
        }
        if let Some(html) = &copied.html {
            let _ = data.set_data("text/html", html);
        }
        e.prevent_default();
    }
    // An image goes only through the clipboard API, which the key press
    // already wrote to.
    if menu && copied.image.is_some() {
        clipboard::write(copied);
    }
}

fn on_paste(_page: &Page, e: &Event) {
    let e: &ClipboardEvent = e.unchecked_ref();
    e.prevent_default();
    let Some(pasting) = e.clipboard_data().as_ref().and_then(clipboard::Pasting::of) else {
        return;
    };
    LOOP.with(|l| *l.pasting.borrow_mut() = Some(pasting));
    push(RawEvent::PasteRequested {
        window: WINDOW,
        reply: PasteReply::new(),
    });
    drive();
}

/// Place the input element over the caret, so the IME's candidate window
/// opens beside it, a password input for a secure field, which the browser
/// and its keyboards neither compose, suggest nor learn from, and the soft
/// keyboard and return key for what the field holds; `None` ends text
/// input, cancelling any composition.
pub(super) fn set_text_entry(page: &Page, entry: Option<TextEntry>) {
    let kind = if entry.is_some_and(|e| e.secure) {
        "password"
    } else {
        "text"
    };
    let field = entry.map_or(InputKind::Text, |e| e.kind);
    let keys = match field {
        InputKind::Text => "text",
        InputKind::Digits | InputKind::Integer => "numeric",
        InputKind::Decimal => "decimal",
        InputKind::Phone => "tel",
        InputKind::Email => "email",
        InputKind::Url => "url",
        InputKind::Search => "search",
    };
    let enter = if entry.is_some_and(|e| e.multiline) {
        "enter"
    } else if field == InputKind::Search {
        "search"
    } else {
        "done"
    };
    let mut retake = page.input.type_() != kind;
    if retake {
        page.input.set_type(kind);
    }
    if page.keys.replace(keys) != keys && page.soft_keyboard.get() {
        let _ = page.input.set_attribute("inputmode", keys);
        retake = true;
    }
    if page.enter.replace(enter) != enter {
        let _ = page.input.set_attribute("enterkeyhint", enter);
        retake = true;
    }
    // Refocusing makes the browser and its keyboard take the new type.
    if retake && entry.is_some() {
        let _ = page.input.blur();
        page.focus_input();
    }
    match entry.map(|e| e.caret) {
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
        .set_attribute("inputmode", if show { page.keys.get() } else { "none" });
    let _ = page.input.blur();
    page.focus_input();
}
