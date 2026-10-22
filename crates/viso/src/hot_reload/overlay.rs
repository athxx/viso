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
    Rgba, Size, TextRequest, TextStyle,
};

use crate::WindowState;

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
            size: Size {
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
                    size: Size {
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
                    size: Size {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    style: BoxStyle::solid(HUD_BACKGROUND),
                    ..FlexStyle::default()
                },
                |cx| {
                    let leaf = cx.leaf(LeafStyle {
                        size: Size {
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
