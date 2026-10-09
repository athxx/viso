//! The failed-reload overlay: a panel drawn over a window's last-good UI while
//! the host rejects an edit of a file it mounts, listing the lines the host
//! sent, and removed by the next good reload.
//!
//! The panel is a detached subtree of the window's own store, so its text
//! shapes with the window's runs and it costs nothing while hidden. It is laid
//! out against the window surface and painted after the tree on every frame
//! that repaints; it takes no input and is not in the semantics tree.

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, DirtyClass, FlexStyle, Inset, LeafStyle, Length, NodeId,
    NodeStore, Rect, Rgba, Size, TextRequest,
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
const FONT_SIZE: f32 = 13.0;

/// Shows `lines` over `ws`, replacing a panel already shown, or removes the
/// panel when `lines` is empty.
pub(super) fn show(ws: &mut WindowState, lines: &[&str], scratch: &mut Vec<NodeId>) {
    if let Some(panel) = ws.dev_overlay.take() {
        ws.store.free_subtree(panel, &mut ws.effects, scratch);
        if let Some(root) = ws.root {
            ws.store.mark_dirty(root, DirtyClass::PAINT);
        }
    }
    if lines.is_empty() {
        return;
    }
    let panel = build(&mut ws.store, lines);
    ws.store.mark_dirty(panel, DirtyClass::PAINT);
    ws.dev_overlay = Some(panel);
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
                text,
                font_size: FONT_SIZE,
                color: FOREGROUND,
                soft_wrap: false,
                locale: None,
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

/// Lays out and appends the panel of `ws` to its primitives on a frame that
/// repainted, returning the primitives it added.
pub(crate) fn paint(ws: &mut WindowState, surface: Rect, painted: u32) -> u32 {
    let Some(panel) = ws.dev_overlay else {
        return 0;
    };
    if painted == 0 || !ws.store.arena().is_live(panel) {
        return 0;
    }
    ws.store.layout(panel, surface, &mut ws.scratch);
    let before = ws.primitives.len();
    viso_ui::paint::paint_tree(&ws.store, panel, &mut ws.primitives);
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
}
