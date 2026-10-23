//! The `Sheet` control driven through the facade: a bottom panel over a
//! dimming scrim that a press opens, springing in to its detent; a drag on it
//! carries it to its other detent, or off; Escape and a press on the scrim
//! close it; while open Tab stays inside it, and once out it hides and gives
//! the focus back.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::platform::{
    KeyCode, Modifiers, PointerButtons as RawButtons, PointerPhase as RawPhase, RawEvent, RawKey,
    RawPointer, WindowId,
};
use viso::prelude::*;
use viso::render::{Renderer, Rgba};
use viso::ui::{
    Align, Axis, BoxStyle, BuildCx, Component, FlexStyle, LeafStyle, Length, NodeId, NodeStore,
    PointerButtons, PointerPhase, Role, Size, WindowConfig, paint_tree,
};
use viso::widgets::{SheetDetent, SheetEdge, SheetHandleSlot, sheet};

const W: u32 = 240;
const H: u32 = 120;
const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// Per-channel tolerance (in 0..=255) for the golden comparison.
const TOL: u8 = 2;

/// The panel's full height; its detents show half of it or all.
const EXTENT: f32 = 64.0;

/// An opaque scrim, so the golden does not hang on blending.
const SCRIM: Rgba = Rgba {
    r: 0.10,
    g: 0.10,
    b: 0.12,
    a: 1.0,
};
const PANEL: Rgba = Rgba {
    r: 0.30,
    g: 0.18,
    b: 0.20,
    a: 1.0,
};
const TRIGGER: Rgba = Rgba {
    r: 0.20,
    g: 0.40,
    b: 0.30,
    a: 1.0,
};

thread_local! {
    /// What the sheet told: `None` a dismiss, `Some(k)` a drag settling on
    /// detent `k`.
    static HEARD: RefCell<Vec<Option<usize>>> = const { RefCell::new(Vec::new()) };
}

fn take_heard() -> Vec<Option<usize>> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A 240×120 window: a 240×40 trigger at the top that a press opens the sheet
/// with, and the sheet — a 64-high bottom panel resting at all of it or half,
/// holding a focusable fill.
struct Sheeted {
    slot: SheetHandleSlot,
}

impl Application for Sheeted {
    fn new(_cx: &mut AppCx) -> Self {
        Sheeted {
            slot: Rc::new(RefCell::new(None)),
        }
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            size: (f64::from(W), f64::from(H)),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let slot = self.slot.clone();
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                align: Align::Stretch,
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                let trigger = cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fixed(40.0),
                    },
                    style: BoxStyle::solid(TRIGGER),
                });
                cx.focusable(trigger, true);
                let opens = slot.clone();
                cx.on_pointer(trigger, move |ev| {
                    let Some(p) = ev.pointer() else { return };
                    if p.phase == PointerPhase::Down
                        && p.buttons.contains(PointerButtons::PRIMARY)
                        && let Some(handle) = opens.borrow().clone()
                    {
                        handle.open(ev);
                    }
                });
                sheet()
                    .edge(SheetEdge::Bottom)
                    .extent(EXTENT)
                    .detents([SheetDetent::Fixed(EXTENT / 2.0), SheetDetent::Full])
                    .detent(1)
                    .scrim(SCRIM)
                    .content(|cx| {
                        let body = cx.leaf(LeafStyle {
                            size: Size::fill(),
                            style: BoxStyle::solid(PANEL),
                        });
                        cx.focusable(body, true);
                    })
                    .on_dismiss(|_| HEARD.with(|h| h.borrow_mut().push(None)))
                    .on_detent(|_, k| HEARD.with(|h| h.borrow_mut().push(Some(k))))
                    .handle(&slot)
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f64, y: f64, buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        x,
        y,
        buttons,
        Modifiers::default(),
        phase,
    ))
}

/// A press and release at (`x`, `y`).
fn tap(x: f64, y: f64) -> Vec<RawEvent> {
    vec![
        mouse(x, y, RawButtons::NONE, RawPhase::Moved),
        mouse(x, y, RawButtons::PRIMARY, RawPhase::Down),
        redraw(),
        mouse(x, y, RawButtons::NONE, RawPhase::Up),
        redraw(),
    ]
}

/// A press at `from` dragged down to `to` y in `steps` frames, held `hold`
/// frames, then lifted.
fn drag(from: (f64, f64), to: f64, steps: u32, hold: usize) -> Vec<RawEvent> {
    let mut out = vec![
        mouse(from.0, from.1, RawButtons::NONE, RawPhase::Moved),
        mouse(from.0, from.1, RawButtons::PRIMARY, RawPhase::Down),
    ];
    for k in 1..=steps {
        let y = from.1 + (to - from.1) * f64::from(k) / f64::from(steps);
        out.push(mouse(from.0, y, RawButtons::PRIMARY, RawPhase::Moved));
        out.push(redraw());
    }
    out.extend(std::iter::repeat_n(redraw(), hold));
    out.push(mouse(from.0, to, RawButtons::NONE, RawPhase::Up));
    out.push(redraw());
    out
}

fn key(code: KeyCode) -> Vec<RawEvent> {
    let at = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        })
    };
    vec![at(true), at(false), redraw()]
}

/// Each step, then a second for everything it set moving to come to rest.
fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<Sheeted> {
    take_heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
        script.extend(std::iter::repeat_n(redraw(), 60));
    }
    drive_scripted::<Sheeted>(script, STEP)
}

/// The press that opens the sheet.
fn open() -> Vec<RawEvent> {
    tap(120.0, 20.0)
}

/// The window's trigger, sheet, and the sheet's panel.
fn parts(app: &DrivenApp<Sheeted>) -> (NodeId, NodeId, NodeId) {
    let store = app.store();
    let root = app.root().expect("the app declares a root");
    let mut kids = store.arena().children(root);
    let trigger = kids.next().expect("the trigger");
    let sheet = kids.next().expect("the sheet");
    let panel = store
        .arena()
        .children(sheet)
        .next()
        .expect("the sheet's panel");
    (trigger, sheet, panel)
}

fn shown(app: &DrivenApp<Sheeted>) -> f32 {
    let (_, sheet, _) = parts(app);
    app.store().sheet_shown(sheet).expect("a sheet")
}

/// Whether node `id` is `ancestor` or within it.
fn within(store: &NodeStore, id: NodeId, ancestor: NodeId) -> bool {
    let mut at = Some(id);
    while let Some(n) = at {
        if n == ancestor {
            return true;
        }
        at = store.arena().parent(n);
    }
    false
}

#[test]
fn a_press_opens_the_sheet_to_its_detent_and_matches_golden() {
    let app = run(&[open()]);
    let (_, sheet, panel) = parts(&app);
    let store = app.store();
    assert!(!store.hidden(sheet));
    assert!((shown(&app) - EXTENT).abs() < 0.5, "it rests at all of it");
    assert!(
        (store.world(panel).y - (H as f32 - EXTENT)).abs() < 0.5,
        "its panel lies against the bottom edge: {:?}",
        store.world(panel)
    );
    assert!(
        within(store, store.focused().expect("a focus"), panel),
        "it took the focus"
    );

    let root = app.root().unwrap();
    let mut primitives = Vec::new();
    paint_tree(store, root, &mut primitives);
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [W as f32, H as f32]);
    let actual = gpu.read_pixels_bgra8(surface);

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/sheet_widget.bgra8");
    if std::env::var("BLESS").is_ok() {
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read(&path)
        .unwrap_or_else(|_| panic!("missing golden {}; run with BLESS=1", path.display()));
    assert_eq!(actual.len(), expected.len());
    let worst = actual
        .iter()
        .zip(&expected)
        .map(|(a, e)| a.abs_diff(*e))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= TOL,
        "golden mismatch: max per-channel diff {worst}"
    );
}

#[test]
fn a_slow_drag_down_settles_on_the_lesser_detent() {
    let app = run(&[open(), drag((120.0, 90.0), 120.0, 15, 10)]);
    assert_eq!(take_heard(), [Some(0)]);
    assert!(
        (shown(&app) - EXTENT / 2.0).abs() < 0.5,
        "it rests at half: {}",
        shown(&app)
    );
}

#[test]
fn a_fling_down_dismisses_it_and_gives_the_focus_back() {
    let app = run(&[key(KeyCode::Tab), open(), drag((120.0, 70.0), 118.0, 4, 0)]);
    assert_eq!(take_heard(), [None]);
    let (trigger, sheet, _) = parts(&app);
    let store = app.store();
    assert!(store.hidden(sheet), "out, it hides");
    assert_eq!(shown(&app), 0.0);
    assert_eq!(store.focused(), Some(trigger), "the focus came back");
}

#[test]
fn escape_closes_it() {
    let app = run(&[open(), key(KeyCode::Escape), key(KeyCode::Escape)]);
    assert_eq!(take_heard(), [None], "once");
    let (_, sheet, _) = parts(&app);
    assert!(app.store().hidden(sheet));
}

#[test]
fn a_press_on_the_scrim_closes_it_and_reaches_nothing_beneath() {
    let app = run(&[open(), tap(120.0, 20.0)]);
    assert_eq!(
        take_heard(),
        [None],
        "the press closed it, not the trigger under it reopening it"
    );
    let (_, sheet, _) = parts(&app);
    assert!(app.store().hidden(sheet));
}

#[test]
fn tab_stays_inside_an_open_sheet() {
    let app = run(&[
        open(),
        key(KeyCode::Tab),
        key(KeyCode::Tab),
        key(KeyCode::Tab),
    ]);
    let (_, sheet, _) = parts(&app);
    let store = app.store();
    let focused = store.focused().expect("a focus");
    assert!(within(store, focused, sheet), "the focus stayed inside");
}

#[test]
fn it_reads_as_a_dialog_within_a_group() {
    let app = run(&[open()]);
    let (_, sheet, panel) = parts(&app);
    let store = app.store();
    assert_eq!(store.semantics(sheet).map(|s| s.role), Some(Role::Group));
    assert_eq!(store.semantics(panel).map(|s| s.role), Some(Role::Dialog));
    assert!(store.focusable(panel));
}
