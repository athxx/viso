//! Search fields driven through the facade: typing reports the query and the
//! suggestions it brings show below with the query's letters marked; the
//! arrows and Enter or a click take one; Escape closes, then clears; the
//! clear button empties the field; a debounced field reports once the typing
//! rests.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, RawText, WindowId,
};
use viso::prelude::*;
use viso::ui::{Axis, FlexStyle, Inset, NodeId, NodeStore, Role, Sizing, StateValue};
use viso::widgets::{SearchFieldHandle, search_field};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

const CITIES: [&str; 5] = ["Berlin", "Bern", "Boston", "Lisbon", "Oslo"];

thread_local! {
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

fn hear(what: String) {
    HEARD.with(|h| h.borrow_mut().push(what));
}

/// The cities holding `query`, in order.
fn found(query: &str) -> Vec<&'static str> {
    let query = query.to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    CITIES
        .into_iter()
        .filter(|c| c.to_lowercase().contains(&query))
        .collect()
}

/// A city search at 100, 100 suggesting as it reports; a debounced one
/// below it.
struct SearchApp;

impl Application for SearchApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(&include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf")[..]);
        SearchApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let field: SearchFieldHandle = cx.handle();
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                padding: Inset {
                    left: 100.0,
                    top: 100.0,
                    ..Inset::default()
                },
                gap: 200.0,
                ..FlexStyle::default()
            },
            |cx| {
                search_field()
                    .label("City")
                    .on_change(move |ev, q| {
                        hear(format!("change {q}"));
                        field.set_suggestions(ev, found(q));
                    })
                    .on_submit(|_, q| hear(format!("submit {q}")))
                    .handle(field)
                    .build(cx);
                search_field()
                    .debounce(Duration::from_millis(100))
                    .on_change(|_, q| hear(format!("slow {q}")))
                    .build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn mouse(x: f32, y: f32, buttons: RawButtons, phase: RawPhase) -> RawEvent {
    RawEvent::Pointer(RawPointer::mouse(
        WIN,
        f64::from(x),
        f64::from(y),
        buttons,
        RawModifiers::default(),
        phase,
    ))
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Up),
        redraw(),
        redraw(),
    ]
}

fn key(code: Key) -> Vec<RawEvent> {
    let raw = |pressed| {
        RawEvent::Key(RawKey {
            window: WIN,
            code,
            pressed,
            repeat: false,
            modifiers: RawModifiers::default(),
        })
    };
    vec![raw(true), raw(false), redraw(), redraw()]
}

fn typed(text: &str) -> Vec<RawEvent> {
    vec![
        RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }),
        redraw(),
        redraw(),
    ]
}

/// `n` frames passing, a wakeup before each.
fn frames(n: usize) -> Vec<RawEvent> {
    (0..n).flat_map(|_| [RawEvent::Wakeup, redraw()]).collect()
}

fn run(steps: &[Vec<RawEvent>]) -> DrivenApp<SearchApp> {
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<SearchApp>(script, STEP)
}

/// A search field's box, its text field, its clear button, its panel and
/// its rows.
struct Parts {
    field: NodeId,
    input: NodeId,
    cross: NodeId,
    panel: NodeId,
    rows: Vec<NodeId>,
}

fn parts(app: &DrivenApp<SearchApp>, which: usize) -> Parts {
    let store = app.store();
    let wrapper = store
        .arena()
        .children(app.root().unwrap())
        .nth(which)
        .unwrap();
    let mut kids = store.arena().children(wrapper);
    let (field, panel) = (kids.next().unwrap(), kids.next().unwrap());
    let mut row = store.arena().children(field).skip(1);
    let (input, cross) = (row.next().unwrap(), row.next().unwrap());
    Parts {
        field,
        input,
        cross,
        panel,
        rows: store.arena().children(panel).collect(),
    }
}

fn middle(store: &NodeStore, id: NodeId) -> (f32, f32) {
    let r = store.world(id);
    (r.x + r.w / 2.0, r.y + r.h / 2.0)
}

fn lit(app: &DrivenApp<SearchApp>, row: NodeId) -> bool {
    let look = app
        .store()
        .interaction_style(row)
        .expect("a row has a look");
    matches!(
        look.hover_cell.and_then(|c| app.states().get(c)),
        Some(StateValue::Bool(true))
    )
}

/// What the showing rows say.
fn shown(app: &DrivenApp<SearchApp>, p: &Parts) -> Vec<String> {
    let store = app.store();
    p.rows
        .iter()
        .filter(|&&r| !store.hidden(r))
        .map(|&r| store.semantics(r).and_then(|s| s.label.clone()).unwrap())
        .collect()
}

fn fields() -> ((f32, f32), (f32, f32)) {
    let app = run(&[]);
    let store = app.store();
    (
        middle(store, parts(&app, 0).input),
        middle(store, parts(&app, 1).input),
    )
}

#[test]
fn typing_reports_and_suggestions_show_below() {
    let (city, _) = fields();
    let app = run(&[]);
    let p = parts(&app, 0);
    let store = app.store();
    assert_eq!(p.rows.len(), 8, "a row for each suggestion it may show");
    assert!(store.hidden(p.cross), "nothing to clear");
    assert_eq!(
        store.semantics(p.input).map(|s| s.role),
        Some(Role::SearchField)
    );
    assert_eq!(
        store.semantics(p.cross).map(|s| (s.role, s.label.clone())),
        Some((Role::Button, Some("Clear".to_owned())))
    );
    assert!(!store.focusable(p.cross));

    let app = run(&[click(city), typed("bo")]);
    let p = parts(&app, 0);
    let store = app.store();
    assert_eq!(heard(), ["change bo"]);
    assert!(!store.hidden(p.panel), "open");
    assert!(!store.hidden(p.cross));
    assert_eq!(shown(&app, &p), ["Boston", "Lisbon"]);
    assert_eq!(
        store.semantic_state(p.rows[1]).and_then(|s| s.position),
        Some((2, 2))
    );
    assert_eq!(
        store.semantic_state(p.input).and_then(|s| s.expanded),
        Some(true)
    );
    let panel = store.world(p.panel);
    let field = store.world(p.field);
    assert!(panel.y >= field.y + field.h, "below the field");
    assert!(panel.w >= field.w - 0.5, "as wide as the field");
    // No suggestion closes the list.
    let app = run(&[click(city), typed("bo"), typed("x")]);
    assert!(app.store().hidden(parts(&app, 0).panel));
}

#[test]
fn the_arrows_highlight_and_enter_takes_the_highlight() {
    let (city, _) = fields();
    let app = run(&[click(city), typed("ber"), key(Key::Up)]);
    let p = parts(&app, 0);
    assert!(lit(&app, p.rows[1]), "Up starts from the last");
    assert_eq!(app.store().active_descendant(p.input), Some(p.rows[1]));
    heard();
    let app = run(&[
        click(city),
        typed("ber"),
        key(Key::Down),
        key(Key::Down),
        key(Key::Down),
        key(Key::Enter),
    ]);
    let p = parts(&app, 0);
    let all = heard();
    assert_eq!(
        &all[all.len() - 2..],
        ["change Berlin", "submit Berlin"],
        "Down wraps round to the first"
    );
    assert_eq!(app.focused_text(), Some("Berlin"));
    assert!(app.store().hidden(p.panel));
    assert_eq!(app.store().active_descendant(p.input), None);
    // Without a highlight Enter submits the text.
    let app = run(&[click(city), typed("ber"), key(Key::Enter)]);
    assert_eq!(heard(), ["change ber", "submit ber"]);
    assert!(app.store().hidden(parts(&app, 0).panel));
}

#[test]
fn a_click_on_a_suggestion_takes_it() {
    let (city, _) = fields();
    let probe = run(&[click(city), typed("o")]);
    let oslo = middle(probe.store(), parts(&probe, 0).rows[2]);
    assert_eq!(shown(&probe, &parts(&probe, 0))[2], "Oslo");
    let app = run(&[click(city), typed("o"), click(oslo)]);
    let all = heard();
    assert_eq!(&all[all.len() - 2..], ["change Oslo", "submit Oslo"]);
    assert_eq!(app.focused_text(), Some("Oslo"));
    assert_eq!(app.store().focused(), Some(parts(&app, 0).input));
}

#[test]
fn escape_closes_then_clears_and_the_button_clears() {
    let (city, _) = fields();
    let app = run(&[click(city), typed("os"), key(Key::Escape)]);
    let p = parts(&app, 0);
    assert!(app.store().hidden(p.panel), "the first Escape closes");
    assert_eq!(app.focused_text(), Some("os"));
    let app = run(&[click(city), typed("os"), key(Key::Escape), key(Key::Escape)]);
    let p = parts(&app, 0);
    assert_eq!(app.focused_text(), Some(""), "the second clears");
    assert!(app.store().hidden(p.cross));
    assert_eq!(heard().last().map(String::as_str), Some("change "));

    let probe = run(&[click(city), typed("os")]);
    let cross = middle(probe.store(), parts(&probe, 0).cross);
    let app = run(&[click(city), typed("os"), click(cross)]);
    let p = parts(&app, 0);
    assert_eq!(app.focused_text(), Some(""));
    assert_eq!(app.store().focused(), Some(p.input), "the focus stays");
    assert!(app.store().hidden(p.panel));
    assert!(app.store().hidden(p.cross));
    assert_eq!(heard().last().map(String::as_str), Some("change "));
}

#[test]
fn a_debounced_field_reports_once_the_typing_rests() {
    let (_, slow) = fields();
    run(&[click(slow), typed("a"), typed("b"), frames(2)]);
    assert!(heard().is_empty(), "still typing");
    run(&[click(slow), typed("a"), typed("b"), frames(12)]);
    assert_eq!(heard(), ["slow ab"], "one report, of the last query");
    run(&[click(slow), typed("a"), frames(12), typed("b"), frames(12)]);
    assert_eq!(heard(), ["slow a", "slow ab"]);
}

// --- golden screenshot -------------------------------------------------------

mod golden {
    use std::path::PathBuf;

    use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
    use viso::render::{Rect, Renderer, Rgba};
    use viso::ui::{
        Axis, BindingTable, BoxStyle, BuildCx, Component, Inset, Length, NodeId, NodeStore,
        SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists, paint_tree,
    };
    use viso::widgets::{ViewStyle, search_field, view};

    const W: u32 = 260;
    const H: u32 = 100;
    const TOL: u8 = 2;

    /// An empty field showing only its magnifier, and one holding text with
    /// its clear button; text itself is left unshaped.
    fn scene(store: &mut NodeStore) -> NodeId {
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let container = view(ViewStyle {
            axis: Axis::Column,
            padding: Inset::all(10.0),
            gap: 10.0,
            size: Sizing::fill(),
            background: BoxStyle::solid(Rgba::new(1.0, 1.0, 1.0, 1.0)),
            ..Default::default()
        })
        .children(|cx| {
            let width = Sizing {
                width: Length::Fixed(240.0),
                height: Length::Fit,
            };
            search_field().size(width).build(cx);
            search_field().text("Oslo").size(width).build(cx);
        });
        let mut cx = BuildCx::with_reactive(
            store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        container.build(&mut cx);
        cx.root().unwrap()
    }

    #[test]
    fn a_magnifier_leads_and_a_clear_button_ends_and_match_golden() {
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = Renderer::new(&mut gpu, format);
        let mut store = NodeStore::new();
        let root = scene(&mut store);
        store.layout(
            root,
            Rect::new(0.0, 0.0, W as f32, H as f32),
            &mut Vec::new(),
        );
        let mut primitives = Vec::new();
        paint_tree(&store, root, &mut primitives);
        renderer.upload(&mut gpu, &primitives);
        renderer.submit(
            &mut gpu,
            surface,
            [0.0, 0.0, 0.0, 1.0],
            [W as f32, H as f32],
        );
        let actual = gpu.read_pixels_bgra8(surface);
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/search_field.bgra8");
        if std::env::var("BLESS").is_ok() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
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
}
