//! Charts driven through the facade: the axes line up with the plot's
//! ticks, the pointer and the keyboard move a readout every series is read
//! in, a legend entry hides its series and the axis takes the rest's
//! range, bars mark their band and a scatter its nearest dot, and new data
//! through the handle redraws the plot.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{
    KeyCode as Key, Modifiers as RawModifiers, PointerButtons as RawButtons,
    PointerPhase as RawPhase, RawEvent, RawKey, RawPointer, WindowId,
};
use viso::prelude::*;
use viso::ui::{
    Axis, Component, EventCx, FlexStyle, Inset, NodeId, NodeStore, PlotLayer, Role, Sizing,
};
use viso_charts::{
    ChartHandle, ChartKind, bar_chart, line_chart, scatter_chart, series, series_xy, sparkline,
};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

thread_local! {
    static KIND: Cell<ChartKind> = const { Cell::new(ChartKind::Line) };
    static HEARD: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static SLOT: Cell<Option<ChartHandle>> = const { Cell::new(None) };
    static SWAP: Cell<bool> = const { Cell::new(false) };
}

fn heard() -> Vec<String> {
    HEARD.with(|h| std::mem::take(&mut *h.borrow_mut()))
}

/// A chart 600 by 300 at (20, 20), and a sparkline under it.
struct ChartApp;

impl Application for ChartApp {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(
            &include_bytes!("../../../crates/text/tests/fixtures/DejaVuSans-subset.ttf")[..],
        );
        ChartApp
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let chart: ChartHandle = cx.handle();
        SLOT.with(|s| s.set(Some(chart)));
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                padding: Inset {
                    left: 20.0,
                    top: 20.0,
                    ..Inset::default()
                },
                gap: 20.0,
                ..FlexStyle::default()
            },
            |cx| {
                let size = Sizing::fixed(600.0, 300.0);
                let pick = move |ev: &mut EventCx<'_>, at: viso_charts::ChartPoint| {
                    HEARD.with(|h| h.borrow_mut().push(format!("{:?} {}", at.series, at.index)));
                    // Selecting gives the chart new data.
                    if SWAP.with(Cell::get) {
                        chart.set_series(ev, vec![series("Only", [100.0, 300.0, 200.0, 400.0])]);
                    }
                };
                match KIND.with(Cell::get) {
                    ChartKind::Line | ChartKind::Area => line_chart(
                        "Revenue",
                        vec![
                            series("2025", [12.0, 15.0, 14.0, 19.0]),
                            series("2026", [14.0, 18.0, 21.0, 24.0]),
                        ],
                    )
                    .categories(["Q1", "Q2", "Q3", "Q4"])
                    .size(size)
                    .on_select(pick)
                    .handle(chart)
                    .build(cx),
                    ChartKind::Bar => bar_chart(
                        "Signups",
                        vec![
                            series("Web", [3.0, 5.0, 2.0]),
                            series("App", [2.0, 4.0, 6.0]),
                        ],
                    )
                    .categories(["May", "June", "July"])
                    .size(size)
                    .on_select(pick)
                    .build(cx),
                    ChartKind::Scatter => scatter_chart(
                        "Heights",
                        vec![series_xy("Sample", [(1.0, 1.0), (5.0, 5.0), (9.0, 2.0)])],
                    )
                    .size(size)
                    .on_select(pick)
                    .build(cx),
                };
                sparkline("Latency", [41.0, 38.0, 44.0, 52.0, 47.0]).build(cx);
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

fn hover(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        redraw(),
        redraw(),
    ]
}

fn click(at: (f32, f32)) -> Vec<RawEvent> {
    vec![
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Moved),
        mouse(at.0, at.1, RawButtons::PRIMARY, RawPhase::Down),
        mouse(at.0, at.1, RawButtons::NONE, RawPhase::Up),
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

fn run(kind: ChartKind, steps: &[Vec<RawEvent>]) -> DrivenApp<ChartApp> {
    KIND.with(|k| k.set(kind));
    heard();
    let mut script = vec![redraw(), redraw()];
    for step in steps {
        script.extend(step.iter().cloned());
    }
    script.push(redraw());
    drive_scripted::<ChartApp>(script, STEP)
}

fn find(store: &NodeStore, root: NodeId, keep: &dyn Fn(NodeId) -> bool) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if keep(id) {
            out.push(id);
        }
        let kids: Vec<NodeId> = store.arena().children(id).collect();
        stack.extend(kids.into_iter().rev());
    }
    out
}

fn role(store: &NodeStore, id: NodeId) -> Option<Role> {
    store.semantics(id).map(|s| s.role)
}

fn with_role(app: &DrivenApp<ChartApp>, r: Role) -> Vec<NodeId> {
    let store = app.store();
    find(store, app.root().unwrap(), &|id| role(store, id) == Some(r))
}

/// The chart's plot: the first plot node.
fn plot(app: &DrivenApp<ChartApp>) -> NodeId {
    let store = app.store();
    find(store, app.root().unwrap(), &|id| store.plot(id).is_some())[0]
}

fn readout(app: &DrivenApp<ChartApp>) -> Option<String> {
    let tip = with_role(app, Role::Status)[0];
    let store = app.store();
    (!store.hidden(tip)).then(|| store.semantics(tip).unwrap().label.clone().unwrap())
}

/// Where fraction `(ux, uy)` of the plot is on screen.
fn on_plot(app: &DrivenApp<ChartApp>, ux: f32, uy: f32) -> (f32, f32) {
    let r = app.store().world(plot(app));
    (r.x + ux * r.w, r.y + (1.0 - uy) * r.h)
}

fn label_text(store: &NodeStore, id: NodeId) -> String {
    store
        .semantics(id)
        .and_then(|s| s.label.clone())
        .unwrap_or_default()
}

#[test]
fn the_axes_line_up_with_the_plots_ticks() {
    let app = run(ChartKind::Line, &[]);
    let store = app.store();
    let p = store.world(plot(&app));
    assert!(p.w > 500.0 && p.h > 200.0, "the plot takes the room: {p:?}");
    let grid = store.plot(plot(&app)).unwrap().grid.clone();
    // 12 to 24: ticks 12, 14, … 24.
    let labels: Vec<NodeId> = with_role(&app, Role::Label)
        .into_iter()
        .filter(|&id| {
            let r = store.world(id);
            r.x + r.w <= p.x + 0.5 && r.y >= p.y - 20.0
        })
        .collect();
    assert_eq!(labels.len(), grid.rows.len());
    let texts: Vec<String> = labels.iter().map(|&id| label_text(store, id)).collect();
    assert_eq!(texts, ["24", "22", "20", "18", "16", "14", "12"]);
    for (&id, &row) in labels.iter().zip(grid.rows.iter().rev()) {
        let r = store.world(id);
        let tick = p.y + (1.0 - row) * p.h;
        assert!((r.y + r.h / 2.0 - tick).abs() < 1.0, "label on its tick");
    }
    // The categories sit under the bands' middles.
    let q2 = with_role(&app, Role::Label)
        .into_iter()
        .find(|&id| label_text(store, id) == "Q2")
        .unwrap();
    let r = store.world(q2);
    assert!((r.x + r.w / 2.0 - (p.x + 0.375 * p.w)).abs() < 1.0);
    assert!(r.y >= p.y + p.h);
    // The legend: one check box per series.
    let legend = with_role(&app, Role::CheckBox);
    assert_eq!(legend.len(), 2);
    let sparkline = with_role(&app, Role::Image)[0];
    assert_eq!(label_text(store, sparkline), "Latency: 41 → 47 (38 – 52)");
}

#[test]
fn the_pointer_reads_every_series_at_the_nearest_place() {
    let app = run(ChartKind::Line, &[]);
    let at = on_plot(&app, 0.4, 0.5);
    let app = run(ChartKind::Line, &[hover(at)]);
    assert_eq!(readout(&app).as_deref(), Some("Q2: 2025 15, 2026 18"));
    let mark = app.store().plot(plot(&app)).unwrap().mark.clone().unwrap();
    assert_eq!(mark.line, Some(0.375), "the crosshair on Q2");
    assert_eq!(mark.dots.len(), 2);
    let away = (at.0, 600.0);
    let app = run(ChartKind::Line, &[hover(at), hover(away)]);
    assert_eq!(readout(&app), None, "leaving hides it");
    assert!(app.store().plot(plot(&app)).unwrap().mark.is_none());
    run(ChartKind::Line, &[click(at)]);
    assert_eq!(heard(), ["None 1"]);
}

#[test]
fn the_keyboard_moves_the_readout() {
    let tab = key(Key::Tab);
    let app = run(
        ChartKind::Line,
        &[tab.clone(), key(Key::Right), key(Key::Right)],
    );
    assert_eq!(readout(&app).as_deref(), Some("Q2: 2025 15, 2026 18"));
    let app = run(ChartKind::Line, &[tab.clone(), key(Key::End)]);
    assert_eq!(readout(&app).as_deref(), Some("Q4: 2025 19, 2026 24"));
    let app = run(
        ChartKind::Line,
        &[tab.clone(), key(Key::Home), key(Key::Escape)],
    );
    assert_eq!(readout(&app), None);
    run(ChartKind::Line, &[tab, key(Key::Left), key(Key::Enter)]);
    assert_eq!(heard(), ["None 3"], "Left from nothing starts at the end");
}

#[test]
fn a_legend_entry_hides_its_series_and_the_axis_takes_the_rest() {
    let app = run(ChartKind::Line, &[]);
    let entry = app.store().world(with_role(&app, Role::CheckBox)[1]);
    let press = click((entry.x + entry.w / 2.0, entry.y + entry.h / 2.0));
    let app = run(ChartKind::Line, std::slice::from_ref(&press));
    let store = app.store();
    let shown = store.plot(plot(&app)).unwrap();
    assert_eq!(shown.layers.len(), 1, "2026 is hidden");
    let entry = with_role(&app, Role::CheckBox)[1];
    assert_eq!(
        store.semantic_state(entry).and_then(|s| s.checked),
        Some(false)
    );
    assert_eq!(store.opacity(entry), 0.4);
    // 12 to 19 now: ticks 12, 13, … 19.
    assert_eq!(shown.grid.rows.len(), 8, "{:?}", shown.grid.rows);
    let app = run(ChartKind::Line, &[press.clone(), press]);
    assert_eq!(app.store().plot(plot(&app)).unwrap().layers.len(), 2);
}

#[test]
fn bars_mark_their_band_and_a_scatter_its_nearest_dot() {
    let app = run(ChartKind::Bar, &[]);
    let PlotLayer::Bars { bars, .. } = &app.store().plot(plot(&app)).unwrap().layers[0] else {
        panic!("bars")
    };
    assert_eq!(bars.len(), 6);
    let at = on_plot(&app, 0.5, 0.2);
    let app = run(ChartKind::Bar, &[hover(at)]);
    assert_eq!(readout(&app).as_deref(), Some("June: Web 5, App 4"));
    let mark = app.store().plot(plot(&app)).unwrap().mark.clone().unwrap();
    let band = mark.band.unwrap();
    assert!((band.0 - 1.0 / 3.0).abs() < 1e-6 && (band.1 - 2.0 / 3.0).abs() < 1e-6);
    assert!(mark.dots.is_empty() && mark.line.is_none());

    let app = run(ChartKind::Scatter, &[]);
    let model = app.store().plot(plot(&app)).unwrap().clone();
    let PlotLayer::Dots { points, .. } = &model.layers[0] else {
        panic!("dots")
    };
    let middle = points[1];
    let near = on_plot(&app, middle.x + 0.01, middle.y + 0.02);
    let app = run(ChartKind::Scatter, &[hover(near)]);
    assert_eq!(readout(&app).as_deref(), Some("Sample: 5, 5"));
    let far = on_plot(&app, middle.x + 0.2, middle.y - 0.3);
    let app = run(ChartKind::Scatter, &[hover(near), hover(far)]);
    assert_eq!(readout(&app), None, "no dot within reach");
    run(ChartKind::Scatter, &[click(near)]);
    assert_eq!(heard(), ["Some(0) 1"]);
}

#[test]
fn new_data_through_the_handle_redraws_the_plot() {
    let app = run(ChartKind::Line, &[]);
    let at = on_plot(&app, 0.1, 0.5);
    SWAP.with(|s| s.set(true));
    let app = run(ChartKind::Line, &[click(at)]);
    SWAP.with(|s| s.set(false));
    let store = app.store();
    let shown = store.plot(plot(&app)).unwrap();
    assert_eq!(shown.layers.len(), 1);
    assert_eq!(readout(&app), None, "the readout leaves with the old data");
    let texts: Vec<String> = with_role(&app, Role::Label)
        .into_iter()
        .map(|id| label_text(store, id))
        .collect();
    assert!(
        texts.iter().any(|t| t == "400"),
        "the axis takes the new range: {texts:?}"
    );
    assert_eq!(
        with_role(&app, Role::CheckBox).len(),
        1,
        "the legend names the new series"
    );
    let chart = SLOT.with(Cell::get).unwrap();
    assert_eq!(chart.hovered(app.states()), None);
}
