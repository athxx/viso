//! The default caption wrap, driven end to end through the real facade frame
//! loop (`drive_scripted` + `FixedStepClock`, the same pump `viso::run` uses).
//!
//! Mirroring makepad's `show_caption_bar: true` default, the facade wraps every
//! window's authored content in a self-drawn caption band unless the app opts
//! out. An app that authors no title bar therefore settles with a window-centered
//! title above its own content — no authoring — while an app that sets
//! `caption: false` in its `window_config` keeps its returned root as the window
//! root verbatim.
//!
//! These assert the seam against the headless backend (launch window id 1), which
//! lays the tree out against its surface so the caption band's world box is real.

use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{NodeId, NodeStore, Role, Size, WindowChrome, WindowConfig};

const STEP: Duration = Duration::from_millis(16);

/// A minimal app that authors a single full-surface node and no title bar — the
/// hello-world shape. `window_config` is left at the default, so the facade
/// wraps it in the default caption band.
struct BareApp;

impl Application for BareApp {
    fn new(_cx: &mut AppCx) -> Self {
        BareApp
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
            },
            |_cx| {},
        );
    }
}

/// The same content, but the app opts out of the wrap through `window_config`.
struct NoCaptionApp;

impl Application for NoCaptionApp {
    fn new(_cx: &mut AppCx) -> Self {
        NoCaptionApp
    }
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            caption: false,
            ..Default::default()
        }
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
            },
            |_cx| {},
        );
    }
}

/// An app that names its own window title, to prove the title flows from
/// `window_config` into the caption band's accessible label.
struct TitledApp;

impl Application for TitledApp {
    fn new(_cx: &mut AppCx) -> Self {
        TitledApp
    }
    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "My Window".to_string(),
            ..Default::default()
        }
    }
    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            viso::ui::FlexStyle {
                size: Size::fill(),
                ..viso::ui::FlexStyle::default()
            },
            |_cx| {},
        );
    }
}

/// One priming redraw runs the first layout so every node has a world box.
fn redraw() -> RawEvent {
    RawEvent::RedrawRequested {
        window: WindowId(1),
    }
}

/// Direct children of `parent`, in sibling order.
fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
    let arena = store.arena();
    let mut out = Vec::new();
    let mut child = arena.links(parent).and_then(|l| l.first_child);
    while let Some(c) = child {
        out.push(c);
        child = arena.links(c).and_then(|l| l.next_sibling);
    }
    out
}

#[test]
fn default_wraps_content_in_a_centered_caption_bar() {
    let app = drive_scripted::<BareApp>(vec![redraw()], STEP);

    // The launch window's root is the wrap Column, not the app's own node: it has
    // exactly two children — the caption band on top, the body below.
    let root = app.root().expect("launch window declares a root");
    let store = app.store();
    let top = children(store, root);
    assert_eq!(top.len(), 2, "wrap = caption band + body");

    let (caption, body) = (top[0], top[1]);

    // The caption band is the bar: it carries the `Group` semantics the widget
    // authors, and the default title flows into its accessible label.
    let sem = store
        .semantics(caption)
        .expect("the caption band authors semantics");
    assert_eq!(sem.role, Role::Group, "the caption band is a Group");
    assert_eq!(sem.label.as_deref(), Some("Viso"), "the default title");

    // The bar splits into three sections — leading / center / trailing — and the
    // center title band is a `Fill` child that eats the main-axis slack between
    // the two `Fit` edge sections, centering the title within that span.
    //
    // The headless backend reports no native traffic-light box (it has no OS
    // chrome), so a `SelfDrawn` window here self-draws its own min/max/close in
    // the trailing section — the same layout a Linux custom-chrome window gets.
    // The band therefore centers between the empty leading edge and the
    // self-drawn buttons, i.e. on the bar's *content* span, not on the window.
    // (The macOS case — where the OS overlays the traffic lights and the caption
    // yields, letting the band span the full bar and center on the window — is a
    // real-device fact headless cannot render; it is covered by Step D on-device
    // verification.) So we assert the band centers on the span left between the
    // leading and trailing sections, which is what this backend produces.
    let sections = children(store, caption);
    assert_eq!(sections.len(), 3, "leading / center / trailing");
    let (leading, center_band, trailing) = (sections[0], sections[1], sections[2]);
    let lead = store.world(leading);
    let trail = store.world(trailing);
    let span_center_x = (lead.x + lead.w + trail.x) / 2.0;
    let band = store.world(center_band);
    let band_center_x = band.x + band.w / 2.0;
    assert!(
        (band_center_x - span_center_x).abs() < 1.0,
        "the title band centers on the span between the edge sections \
         ({band_center_x} vs {span_center_x})"
    );

    // The caption sits above a real body: the caption's top is at the window top
    // and the body begins below it (a full-height body under a fixed-height bar).
    let cap = store.world(caption);
    let bod = store.world(body);
    assert!(cap.y <= bod.y, "the caption band sits above the body");
    assert!(
        bod.h > cap.h,
        "the body fills the height left under the bar"
    );
}

#[test]
fn caption_false_leaves_the_app_root_verbatim() {
    let app = drive_scripted::<NoCaptionApp>(vec![redraw()], STEP);

    // With the wrap opted out, the window root is the app's own single node: it
    // declares no children, and carries no caption `Group` semantics.
    let root = app.root().expect("launch window declares a root");
    let store = app.store();
    assert!(
        children(store, root).is_empty(),
        "the app's own leaf root, unwrapped"
    );
    assert!(
        store.semantics(root).is_none(),
        "no caption band was inserted"
    );
}

#[test]
fn window_config_title_flows_into_the_caption_label() {
    let app = drive_scripted::<TitledApp>(vec![redraw()], STEP);

    let root = app.root().expect("launch window declares a root");
    let store = app.store();
    let caption = children(store, root)[0];
    let sem = store
        .semantics(caption)
        .expect("the caption band authors semantics");
    assert_eq!(
        sem.label.as_deref(),
        Some("My Window"),
        "the app's window_config title flows into the caption"
    );
}

/// The default chrome is self-drawn, so a captioned launch window is a full-size
/// content view — the mode the caption band reads to decide its layout. This
/// pins the framework default the wrap relies on.
#[test]
fn default_window_config_is_self_drawn_and_captioned() {
    let cfg = WindowConfig::default();
    assert_eq!(cfg.chrome, WindowChrome::SelfDrawn);
    assert!(cfg.caption);
}
