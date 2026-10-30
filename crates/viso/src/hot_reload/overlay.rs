//! The panels a development session draws over a window: the failed-reload
//! overlay, over the last-good UI while the host rejects an edit of a file it
//! mounts, listing the lines the host sent, and removed by the next good
//! reload; and the frame-time HUD, while a capture asks for it.
//!
//! A panel is a detached subtree of the window's own store, so its text
//! shapes with the window's runs and it costs nothing while hidden. It is laid
//! out against the window surface and painted after the tree on every frame
//! that repaints; it takes no input and is not in the semantics tree.

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, FlexStyle, Inset, LeafStyle, Length, NodeId, NodeStore, Rect,
    Rgba, Sizing, TextRequest, TextStyle,
};

use crate::driver::WindowState;

/// The diagnostics a panel lists before summarizing the rest.
const MAX_LINES: usize = 8;

const BACKGROUND: Rgba = Rgba {
    r: 0.45,
    g: 0.05,
    b: 0.05,
    a: 0.92,
};
const FOREGROUND: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const HUD_BACKGROUND: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.72,
};
const FONT_SIZE: f32 = 13.0;
const HUD_FONT_SIZE: f32 = 11.0;

/// Shows `lines` over `ws`, replacing a panel already shown, or removes the
/// panel when `lines` is empty.
pub(super) fn show(ws: &mut WindowState, lines: &[&str], scratch: &mut Vec<NodeId>) {
    if let Some(panel) = ws.dev_overlay.take() {
        ws.store.free_tree(panel, &mut ws.effects, scratch);
        ws.store.request_repaint();
    }
    if lines.is_empty() {
        return;
    }
    ws.dev_overlay = Some(build(&mut ws.store, lines));
    ws.store.request_repaint();
}

/// Builds the panel listing `lines` as a detached subtree of `store`.
fn build(store: &mut NodeStore, lines: &[&str]) -> NodeId {
    let mut cx = BuildCx::new(store);
    let text = |cx: &mut BuildCx<'_>, text: String| {
        let leaf = cx.leaf(LeafStyle {
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        });
        cx.text_request(
            leaf,
            TextRequest {
                spans: Vec::new(),
                text,
                font_size: viso_ui::FontSize::Fixed(FONT_SIZE),
                color: FOREGROUND,
                soft_wrap: false,
                locale: None,
                style: TextStyle::default(),
            },
        );
    };
    // A transparent surface-sized column, so the panel takes its content's
    // height at the top of the window.
    cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            ..FlexStyle::default()
        },
        |cx| {
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    gap: 4.0,
                    padding: Inset::all(10.0),
                    align: Align::Start,
                    size: Sizing {
                        width: Length::fill(),
                        height: Length::Fit,
                    },
                    style: BoxStyle::solid(BACKGROUND),
                    ..FlexStyle::default()
                },
                |cx| {
                    text(cx, "Hot reload failed: showing the last good UI".into());
                    for line in lines.iter().take(MAX_LINES) {
                        text(cx, (*line).into());
                    }
                    if lines.len() > MAX_LINES {
                        text(cx, format!("... {} more", lines.len() - MAX_LINES));
                    }
                },
            );
        },
    );
    cx.root().expect("the panel declared a root")
}

/// Shows the frame-time `line` at the top right of `ws`, replacing the one
/// shown, or removes it with `None`; the window repaints either way.
pub(crate) fn show_hud(ws: &mut WindowState, line: Option<&str>, scratch: &mut Vec<NodeId>) {
    if ws.root.is_none() {
        return;
    }
    if let Some(panel) = ws.dev_hud.take() {
        ws.store.free_tree(panel, &mut ws.effects, scratch);
    } else if line.is_none() {
        return;
    }
    ws.store.request_repaint();
    if let Some(line) = line {
        ws.dev_hud = Some(hud(&mut ws.store, line));
    }
}

/// Builds the HUD showing `line` as a detached subtree of `store`.
fn hud(store: &mut NodeStore, line: &str) -> NodeId {
    let mut cx = BuildCx::new(store);
    cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::End,
            padding: Inset::all(6.0),
            ..FlexStyle::default()
        },
        |cx| {
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    padding: Inset::all(5.0),
                    size: Sizing {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    style: BoxStyle::solid(HUD_BACKGROUND),
                    ..FlexStyle::default()
                },
                |cx| {
                    let leaf = cx.leaf(LeafStyle {
                        size: Sizing {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.text_request(
                        leaf,
                        TextRequest {
                            spans: Vec::new(),
                            text: line.to_owned(),
                            font_size: viso_ui::FontSize::Fixed(HUD_FONT_SIZE),
                            color: FOREGROUND,
                            soft_wrap: false,
                            locale: None,
                            style: TextStyle::default(),
                        },
                    );
                },
            );
        },
    );
    cx.root().expect("the HUD declared a root")
}

/// Lays out and appends the panels of `ws` to its primitives on a frame that
/// repainted, returning the primitives they added.
pub(crate) fn paint(ws: &mut WindowState, surface: Rect, painted: u32) -> u32 {
    if painted == 0 {
        return 0;
    }
    let before = ws.primitives.len();
    for panel in [ws.dev_overlay, ws.dev_hud].into_iter().flatten() {
        if ws.store.arena().is_live(panel) {
            ws.store.layout(panel, surface, &mut ws.scratch);
            viso_ui::paint::paint_tree(&ws.store, panel, &mut ws.primitives);
        }
    }
    (ws.primitives.len() - before) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_lists_at_most_its_cap() {
        let mut store = NodeStore::new();
        let lines: Vec<String> = (0..12).map(|n| format!("line {n}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let panel = build(&mut store, &lines);
        let links = |node| store.arena().links(node).expect("a live node");
        let body = links(panel).first_child.expect("the panel body");
        let mut texts = Vec::new();
        let mut child = links(body).first_child;
        while let Some(node) = child {
            texts.push(store.text_request(node).expect("a text").text.clone());
            child = links(node).next_sibling;
        }
        assert_eq!(texts.len(), MAX_LINES + 2);
        assert_eq!(texts[1], "line 0");
        assert_eq!(texts[MAX_LINES + 1], "... 4 more");
    }

    /// The HUD as a frame-time capture shows it, drawn in the window's top
    /// right corner over what the window drew, against its golden. The line
    /// keeps to the fixture font's glyphs, so no system fallback face enters
    /// the pixels.
    #[cfg(feature = "rasterizer")]
    #[test]
    fn the_hud_draws_as_its_golden() {
        use viso_gpu::{CpuRaster, GpuBackend, RawWindowHandle};

        use crate::text_content::{ParagraphSlot, TextShaper};

        const W: u32 = 360;
        const H: u32 = 48;
        const FONT: &[u8] = include_bytes!("../../fixtures/DejaVuSans-subset.ttf");
        let mut gpu = CpuRaster::new();
        let surface = gpu.create_surface(RawWindowHandle::Headless, W, H);
        let format = gpu.surface_format(surface);
        let mut renderer = viso_render::Renderer::new(&mut gpu, format);
        let mut shaper = TextShaper::new();
        shaper.load_font(FONT, 0).expect("the fixture parses");

        let mut store = NodeStore::new();
        let panel = hud(
            &mut store,
            "frame 8.29 ms  p95 8.97 ms  gpu 0.82 ms  90 frames/s",
        );
        let surface_rect = Rect {
            x: 0.0,
            y: 0.0,
            w: W as f32,
            h: H as f32,
        };
        let links = |store: &NodeStore, node| store.arena().links(node).expect("a live node");
        let body = links(&store, panel).first_child.expect("the HUD body");
        let text = links(&store, body).first_child.expect("the HUD text");
        let request = store.text_request(text).expect("a text").clone();
        let sizing = viso_ui::TextSizing {
            env: viso_ui::LengthEnv::default(),
            font_size: HUD_FONT_SIZE,
            line_height: None,
            rtl: false,
        };
        // Shaping runs on a worker: the first ask starts it, the second
        // reads what it shaped.
        let slot = ParagraphSlot::from(text);
        shaper.shape(&mut gpu, slot, &request, sizing, 1.0, None);
        shaper.wait_idle();
        let _ = shaper.pump(
            &mut gpu,
            std::time::Duration::MAX,
            Some(std::time::Duration::MAX),
            &mut Vec::new(),
        );
        let content = shaper.shape(&mut gpu, slot, &request, sizing, 1.0, None);
        store.set_content_payload(text, content);
        store.layout(panel, surface_rect, &mut Vec::new());
        let mut primitives = Vec::new();
        viso_ui::paint::paint_tree(&store, panel, &mut primitives);
        renderer.upload(&mut gpu, &primitives);
        // A window's mid-grey under the translucent panel.
        renderer.submit(
            &mut gpu,
            surface,
            [0.5, 0.5, 0.5, 1.0],
            [W as f32, H as f32],
        );
        let frame = gpu.read_pixels_bgra8(surface);

        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/golden/profile_hud.bgra8");
        if std::env::var_os("BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &frame).unwrap();
            return;
        }
        let golden = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("missing {}; run with BLESS=1", path.display()));
        assert_eq!(golden.len(), frame.len());
        let worst = frame
            .iter()
            .zip(&golden)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(worst <= 2, "the HUD differs from its golden by {worst}");
    }

    #[test]
    fn the_hud_sits_in_the_top_right_corner() {
        let mut store = NodeStore::new();
        let panel = hud(&mut store, "profile  60 frames  frame 8.30/8.90 ms");
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 800.0,
            h: 600.0,
        };
        store.layout(panel, surface, &mut Vec::new());
        let links = |node| store.arena().links(node).expect("a live node");
        let body = links(panel).first_child.expect("the HUD body");
        let b = store.bounds(body);
        assert_eq!((b.x + b.w, b.y), (800.0 - 6.0, 6.0));
        let text = links(body).first_child.expect("the HUD text");
        assert_eq!(
            store.text_request(text).expect("a text").text,
            "profile  60 frames  frame 8.30/8.90 ms"
        );
    }
}
