//! What the facade tests share: an app holding one editor, and the raw
//! events a user's keys, text and pointer make.

#![allow(dead_code)]

use std::cell::RefCell;
use std::time::Duration;

pub use viso::__test_support::DrivenApp;
use viso::__test_support::drive_scripted;
pub use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawImePreedit, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, Component, FlexStyle, Sizing};
pub use viso_code_editor::{EditorHandle, EditorSlot, code_editor};

pub const STEP: Duration = Duration::from_millis(16);
pub const WIN: WindowId = WindowId(1);

/// How a test's editor is set up past its text.
pub type Setup = fn(viso_code_editor::CodeEditor) -> viso_code_editor::CodeEditor;

thread_local! {
    static TEXT: RefCell<String> = const { RefCell::new(String::new()) };
    static SLOT: RefCell<Option<EditorSlot>> = const { RefCell::new(None) };
    static SETUP: std::cell::Cell<Option<Setup>> = const { std::cell::Cell::new(None) };
}

/// Set the editor up with `setup` in the runs after.
pub fn setup(setup: Option<Setup>) {
    SETUP.with(|s| s.set(setup));
    CELL.with(|c| c.set(None));
}

pub struct EditorApp;

impl Application for EditorApp {
    fn new(cx: &mut AppCx) -> Self {
        // A monospace face for the text, and a proportional one it falls
        // back to for letters past ASCII, in other widths.
        cx.use_packaged_fonts(viso::packaged_fonts!("tests/fixtures"));
        EditorApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        // One slot a run: a view built again shows the session it showed.
        let slot = SLOT.with(|s| {
            s.borrow_mut()
                .get_or_insert_with(EditorSlot::default)
                .clone()
        });
        let text = TEXT.with(|t| t.borrow().clone());
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                let mut editor = code_editor(text)
                    .family(viso::ui::FontFamily::Named("DejaVu Sans Mono".into()))
                    .size(Sizing::fixed(600.0, 400.0))
                    .handle(&slot);
                if let Some(setup) = SETUP.with(|s| s.get()) {
                    editor = setup(editor);
                }
                editor.build(cx);
            },
        );
    }
}

pub fn handle() -> EditorHandle {
    SLOT.with(|s| s.borrow().as_ref().unwrap().borrow().clone().unwrap())
}

pub fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

pub fn apple() -> bool {
    viso::ui::primary_is_logo()
}

/// The platform's primary modifier, with `shift` and `alt`.
pub fn primary(shift: bool, alt: bool) -> RawModifiers {
    RawModifiers {
        shift,
        alt,
        logo: apple(),
        control: !apple(),
    }
}

pub fn key_with(code: Key, modifiers: RawModifiers) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers,
        })
    };
    vec![raw(true), raw(false), redraw()]
}

pub fn key(code: Key) -> Vec<RawEvent> {
    key_with(code, RawModifiers::default())
}

pub fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.into(),
        }),
        redraw(),
    ]
}

pub fn preedit(text: &str, caret: usize) -> Vec<RawEvent> {
    vec![
        RawEvent::ImePreedit(RawImePreedit {
            window: WIN,
            text: text.into(),
            caret,
        }),
        redraw(),
    ]
}

pub fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase, m: RawModifiers) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        m,
        phase,
    ))
}

pub fn click_with(at: (f32, f32), m: RawModifiers) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved, m),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down, m),
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Up, m),
        redraw(),
        redraw(),
    ]
}

pub fn click(at: (f32, f32)) -> Vec<RawEvent> {
    click_with(at, RawModifiers::default())
}

pub fn run(text: &str, steps: &[Vec<RawEvent>]) -> DrivenApp<EditorApp> {
    TEXT.with(|t| *t.borrow_mut() = text.to_owned());
    SLOT.with(|s| *s.borrow_mut() = None);
    run_again(steps)
}

/// Drives the app again from the text the last run started from, as a
/// window opened again builds its view anew into the slot the last run
/// filled.
pub fn run_again(steps: &[Vec<RawEvent>]) -> DrivenApp<EditorApp> {
    let mut script = vec![redraw(), redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    script.push(redraw());
    drive_scripted::<EditorApp>(script, STEP)
}

pub fn carets(h: &EditorHandle) -> Vec<usize> {
    h.with(|s| s.selections().list().iter().map(|s| s.head).collect())
}

thread_local! {
    static CELL: std::cell::Cell<Option<(f32, f32, f32)>> = const { std::cell::Cell::new(None) };
}

/// The cell's width, the line's height and where the text starts, measured
/// by a run of its own.
pub fn metrics() -> (f32, f32, f32) {
    if let Some(m) = CELL.with(|c| c.get()) {
        return m;
    }
    // A plain editor: the test's setup is left to its own runs.
    let setup = SETUP.with(|s| s.take());
    let _app = run("", &[]);
    SETUP.with(|s| s.set(setup));
    let h = handle();
    let (cell, lh) = h.cell_size();
    let m = (cell, lh, h.text_x());
    CELL.with(|c| c.set(Some(m)));
    m
}

/// Where cell `col` of line `line` is on screen, at the line's middle.
pub fn at(line: usize, col: f32) -> (f32, f32) {
    let (cell, lh, x) = metrics();
    (x + col * cell + cell * 0.25, line as f32 * lh + lh * 0.5)
}

/// A click on the first line's start: the editor takes the focus.
pub fn focus() -> Vec<RawEvent> {
    click(at(0, 0.0))
}
