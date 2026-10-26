//! Charts drawn by the CPU rasterizer, matching blessed goldens: a smooth
//! line chart with its crosshair readout mark, and stacked bars.

use std::path::PathBuf;

use viso::gpu::{CpuRaster, GpuBackend, RawWindowHandle};
use viso::render::{Rect, Renderer};
use viso::ui::{
    BindingTable, BoxStyle, Component, Inset, NodeId, NodeStore, PlotMark, Point,
    SemanticProjector, Sizing, StateStore, TextEdits, VirtualLists, paint_tree,
};
use viso::widgets::{ViewStyle, view};
use viso_charts::{ChartStyle, area_chart, bar_chart, line_chart, series};

const GW: u32 = 480;
const GH: u32 = 560;
const CLEAR: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
const TOL: u8 = 2;

fn plots(store: &NodeStore, root: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if store.plot(id).is_some() {
            out.push(id);
        }
        let kids: Vec<NodeId> = store.arena().children(id).collect();
        stack.extend(kids.into_iter().rev());
    }
    out
}

#[test]
fn charts_match_golden() {
    let mut gpu = CpuRaster::new();
    let surface = gpu.create_surface(RawWindowHandle::Headless, GW, GH);
    let format = gpu.surface_format(surface);
    let mut renderer = Renderer::new(&mut gpu, format);
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut text_edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let style = ChartStyle::light();
    let size = Sizing::fixed(448.0, 160.0);
    let scene = view(ViewStyle {
        padding: Inset::all(16.0),
        gap: 16.0,
        size: Sizing::fill(),
        background: BoxStyle::solid(style.surface),
        ..Default::default()
    })
    .children(move |cx| {
        let quarters = ["Q1", "Q2", "Q3", "Q4", "Q5", "Q6"];
        line_chart(
            "Revenue",
            vec![
                series("2025", [12.0, 15.0, 14.0, 19.0, 17.0, 22.0]),
                series("2026", [14.0, 18.0, 21.0, 24.0, 20.0, 26.0]),
            ],
        )
        .categories(quarters)
        .smooth()
        .size(size)
        .build(cx);
        area_chart(
            "Load",
            vec![series("Load", [3.0, 6.0, 4.0, f64::NAN, 5.0, 8.0])],
        )
        .categories(quarters)
        .size(size)
        .build(cx);
        bar_chart(
            "Signups",
            vec![
                series("Web", [3.0, 5.0, 2.0]),
                series("App", [2.0, 4.0, 6.0]),
            ],
        )
        .categories(["May", "June", "July"])
        .stacked()
        .size(size)
        .build(cx);
    });
    let root = {
        let mut cx = viso::ui::BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut text_edits,
            &mut projectors,
        );
        scene.build(&mut cx);
        cx.root().unwrap()
    };
    // The readout's mark on the line chart's Q3, as a pointer there sets it.
    let line = plots(&store, root)[0];
    let x = 2.5 / 6.0;
    let dots = vec![
        (Point::new(x, (14.0 - 12.0) / 14.0), style.palette[0]),
        (Point::new(x, (21.0 - 12.0) / 14.0), style.palette[1]),
    ];
    store.set_plot_mark(
        line,
        Some(PlotMark {
            line: Some(x),
            color: style.baseline,
            dots,
            radius: style.dot_radius,
            ring: style.surface,
            ..PlotMark::default()
        }),
    );
    store.layout(
        root,
        Rect::new(0.0, 0.0, GW as f32, GH as f32),
        &mut Vec::new(),
    );
    let mut primitives = Vec::new();
    paint_tree(&store, root, &mut primitives);
    renderer.upload(&mut gpu, &primitives);
    renderer.submit(&mut gpu, surface, CLEAR, [GW as f32, GH as f32]);
    let actual = gpu.read_pixels_bgra8(surface);
    if let Ok(dump) = std::env::var("DUMP") {
        std::fs::write(dump, &actual).unwrap();
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/charts.bgra8");
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
