//! The controls' default colors come from the theme: a gallery of them drawn
//! under the light and the dark default themes, each held to a golden, and a
//! swap between them repaints the controls without measuring or laying out.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted};
use viso::platform::{Appearance, ColorScheme, RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{Axis, ColorToken, FlexStyle, Inset, Palette, Sizing, paint_tree};
use viso::widgets::{
    button, checkbox, label, link_label, progress_bar, radio_group, segmented_control, select,
    select_option, slider, stepper, text_input,
};
use viso_image::snapshot::{Image, compare};

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);

/// A column of controls, each in its default style.
struct Gallery;

impl Application for Gallery {
    fn new(_cx: &mut AppCx) -> Self {
        Gallery
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            size: (320.0, 560.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 10.0,
                padding: Inset::all(12.0),
                size: Sizing::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                button("Go").build(cx);
                checkbox("Sound").checked(true).build(cx);
                radio_group(["One", "Two"]).selected(0).build(cx);
                slider("Volume").range(0.0, 100.0).value(40.0).build(cx);
                progress_bar().value(0.6).build(cx);
                segmented_control(["A", "B", "C"]).selected(1).build(cx);
                stepper(3.0).build(cx);
                text_input("Name").build(cx);
                select([select_option("Apple"), select_option("Pear")])
                    .selected(0)
                    .build(cx);
                label("Plain text").build(cx);
                link_label("Docs", "/docs").build(cx);
            },
        );
    }
}

fn redraw() -> RawEvent {
    RawEvent::RedrawRequested { window: WIN }
}

fn scheme(color_scheme: ColorScheme) -> RawEvent {
    RawEvent::AppearanceChanged(Appearance {
        color_scheme,
        ..Appearance::default()
    })
}

/// The gallery drawn after `script`, settled.
fn run(script: Vec<RawEvent>) -> DrivenApp<Gallery> {
    let mut all = vec![redraw(), redraw()];
    all.extend(script);
    all.extend(std::iter::repeat_n(redraw(), 20));
    drive_scripted::<Gallery>(all, STEP)
}

/// Holds what `app` draws to the golden `name`, or blesses it with `BLESS`.
fn hold(app: &DrivenApp<Gallery>, name: &str) {
    let (w, h, bgra) = app.pixels().expect("the window has a surface");
    let image = Image::from_bgra8(w, h, &bgra);
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.png"));
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, image.to_png()).unwrap();
        eprintln!("blessed golden: {}", path.display());
        return;
    }
    let golden = Image::from_png(&std::fs::read(&path).expect("the golden exists; BLESS=1"))
        .expect("the golden decodes");
    let comparison = compare(&golden, &image, 0.0);
    assert!(
        comparison.same_size && comparison.differing == 0,
        "{name} departs from its golden: {comparison:?}"
    );
}

/// Every color the scene paints is one a palette resolved: none names a
/// token.
fn paints_no_token(app: &DrivenApp<Gallery>) {
    let mut out = Vec::new();
    paint_tree(app.store(), app.root().unwrap(), &mut out);
    assert!(!out.is_empty());
    let drawn = format!("{out:?}");
    assert!(!drawn.contains("a: -1.0"), "a token reached the paint");
}

#[test]
fn the_controls_draw_in_the_light_theme() {
    let app = run(vec![scheme(ColorScheme::Light)]);
    assert_eq!(*app.store().palette(), Palette::light());
    paints_no_token(&app);
    hold(&app, "theme_light");
}

#[test]
fn the_controls_draw_in_the_dark_theme() {
    let app = run(vec![scheme(ColorScheme::Dark)]);
    assert_eq!(*app.store().palette(), Palette::dark());
    paints_no_token(&app);
    hold(&app, "theme_dark");
}

#[test]
fn a_swap_of_theme_repaints_without_measure_or_layout() {
    let mut script = vec![redraw(), redraw(), scheme(ColorScheme::Light)];
    script.extend(std::iter::repeat_n(redraw(), 20));
    script.extend([scheme(ColorScheme::Dark), redraw()]);
    let app = drive_scripted::<Gallery>(script, STEP);
    let frame = app.recompute();
    assert_eq!(frame.measured, 0, "{frame:?}");
    assert_eq!(frame.laid_out, 0, "{frame:?}");
    assert!(frame.painted > 0, "{frame:?}");
    let primary = app.store().palette().get(ColorToken::Primary);
    assert_eq!(primary, Palette::dark().get(ColorToken::Primary));
    paints_no_token(&app);
}

#[test]
fn a_swap_draws_as_the_theme_drawn_from_the_start() {
    let mut script = vec![scheme(ColorScheme::Light)];
    script.extend(std::iter::repeat_n(redraw(), 20));
    script.push(scheme(ColorScheme::Dark));
    let swapped = run(script);
    let fresh = run(vec![scheme(ColorScheme::Dark)]);
    assert_eq!(swapped.pixels(), fresh.pixels());
}
