//! `viso export html`'s lowering: a view as semantic HTML, CSS and, for what
//! changes, a script; and every place a page cannot follow the view
//! reported where it is written, never dropped.

mod support;

use std::path::Path;
use std::rc::Rc;
use support::origin;
use viso_dsl::export::{HtmlExport, HtmlOptions, Need, html};

use viso_dsl::assets::AssetCatalog;
use viso_dsl::frontend::compile_file_for;
use viso_dsl::hir::TargetProfile;
use viso_dsl::schema::Natives;

fn options(static_only: bool) -> HtmlOptions {
    HtmlOptions {
        static_only,
        title: "Demo".into(),
        lang: "en".into(),
    }
}

fn export(source: &str, static_only: bool) -> Result<HtmlExport, Vec<(Need, String, String)>> {
    export_in(source, None, static_only)
}

/// [`export`] for a view whose `asset("…")`s are found under `assets`.
fn export_in(
    source: &str,
    assets: Option<&Path>,
    static_only: bool,
) -> Result<HtmlExport, Vec<(Need, String, String)>> {
    let profile = TargetProfile {
        assets: assets.map(|dir| Rc::new(AssetCatalog::scan(dir).unwrap())),
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    html(&compiled, &options(static_only)).map_err(|findings| {
        findings
            .into_iter()
            .map(|f| {
                let at = &source[std::ops::Range::<usize>::from(f.at)];
                (f.need, at.to_owned(), f.message)
            })
            .collect()
    })
}

const CARD: &str = r#"
export component Card {
    view {
        Column {
            width: 320dp;
            gap: 8dp;
            padding: 16dp;
            semantics.role: region;
            semantics.label: "Welcome";
            Text("Hello <world>") { width: 200dp; height: 20dp; color: theme.colors.primary }
            Row {
                justify: center;
                align: center;
                Button("Go") { width: fill { weight: 2.0 }; height: 32dp; }
                Toggle { width: 40dp; height: 24dp; checked: true; }
            }
        }
    }
}
"#;

#[test]
fn a_constant_view_is_html_and_css_alone() {
    let page = export(CARD, true).unwrap();
    assert!(page.script.is_none());
    let index = &page.index;
    assert!(
        index.starts_with("<!DOCTYPE html>\n<!-- Generated from Viso source."),
        "{index}"
    );
    assert!(index.contains("<html lang=\"en\">"));
    assert!(index.contains("<title>Demo</title>"));
    assert!(!index.contains("<script"));
    assert!(
        index.contains(
            "<div id=\"viso-root\" class=\"vx-flex s0\" role=\"region\" aria-label=\"Welcome\">"
        ),
        "{index}"
    );
    assert!(index.contains(">Hello &lt;world&gt;</span>"), "{index}");
    assert!(index.contains("<button class=\"vx-button"), "{index}");
    assert!(index.contains(">Go</button>"));
    assert!(
        index.contains("type=\"checkbox\" role=\"switch\" checked"),
        "{index}"
    );

    let css = &page.styles;
    assert!(css.starts_with("/* Generated from Viso source."));
    assert!(css.contains("--colors-primary: #3b6cf0;"));
    assert!(css.contains("color: var(--colors-primary);"), "{css}");
    assert!(
        css.contains("flex-direction: column; gap: 8px; padding: 16px;"),
        "{css}"
    );
    assert!(
        css.contains("justify-content: center; align-items: center;"),
        "{css}"
    );
    assert!(
        css.contains("flex: 2 0 0; min-width: 0; height: 32px;"),
        "{css}"
    );

    // The same view exports to the same bytes.
    assert_eq!(export(CARD, true).unwrap(), page);
}

const SHAPES: &str = r##"
export component Shapes {
    state round = false;
    view {
        Column {
            Column { width: 40dp; height: 20dp; corner_radius: 4dp + 1sp; border: Option::Some(Border { width: 2dp, color: #ff0000 }); }
            Column { width: 40dp; height: 20dp; corner_radius: 50%; }
            Column { width: 40dp; height: 20dp; corner_radius: if round { 10% } else { 0dp }; }
            Column { width: 40dp; height: 20dp; shadow: Option::Some(Shadow { y: 4dp, blur: 12dp, color: #00000040 }); border: Option::Some(Border { color: #0000ff }); }
            Column { width: 40dp; height: 20dp; shadow: if round { Option::Some(Shadow { color: #000000, inset: true }) } else { Option::None }; }
        }
    }
}
"##;

#[test]
fn a_box_s_corner_radius_border_and_shadow_export_as_rounded_corners_an_inner_stroke_and_a_box_shadow()
 {
    let page = export(SHAPES, false).unwrap();
    let css = &page.styles;
    assert!(
        css.contains("border-radius: calc(4 * 1px + 1 * 0.0625rem);"),
        "{css}"
    );
    // A border and a shadow share the one `box-shadow`.
    assert!(
        css.contains(
            "--vx-border: inset 0 0 0 2px #ff0000; --vx-shadow: 0 0 #0000; \
             box-shadow: var(--vx-shadow), var(--vx-border);"
        ),
        "{css}"
    );
    assert!(
        css.contains(
            "--vx-border: inset 0 0 0 1px #0000ff; --vx-shadow: 0 4px 12px 0 #00000040; \
             box-shadow: var(--vx-shadow), var(--vx-border);"
        ),
        "{css}"
    );
    assert!(
        css.contains("border-radius: calc(9999px);"),
        "a pill: {css}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.vradius("), "{script}");
    assert!(script.contains("P.vshadow("), "{script}");
}

const GLASS: &str = r##"
export component Glass {
    state deep = false;
    view {
        Column {
            BlurView { width: 40dp; height: 20dp; }
            Column { width: 40dp; height: 20dp; backdrop: Option::Some(Backdrop { blur: 8dp, saturation: 1.5, tint: #00000080 }); }
            Column { width: 40dp; height: 20dp; backdrop: if deep { Option::Some(Backdrop {}) } else { Option::None }; }
        }
    }
}
"##;

#[test]
fn a_glass_box_exports_as_a_backdrop_filter_under_a_flat_tint() {
    let page = export(GLASS, false).unwrap();
    let css = &page.styles;
    assert!(
        css.contains(
            "--vx-frost: #ffffff40; -webkit-backdrop-filter: blur(30px) saturate(1.8); \
             backdrop-filter: blur(30px) saturate(1.8); \
             background-image: linear-gradient(var(--vx-frost), var(--vx-frost));"
        ),
        "the standard glass: {css}"
    );
    assert!(
        css.contains("--vx-frost: #00000080; -webkit-backdrop-filter: blur(8px) saturate(1.5);"),
        "{css}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.vbackdrop("), "{script}");
}

const MOVED: &str = r#"
export component Moved {
    state on = false;
    view {
        Column {
            Text {
                width: 40dp;
                height: 20dp;
                translate: Offset { x: 4dp, y: 1sp };
                transition.translate: Transition { duration: 100ms };
                transition.corner_radius: Transition { duration: 200ms };
                transition.opacity: Transition { easing: Easing::spring(300.0f32, 10.0f32, 1.0f32) };
            }
            Text {
                width: 40dp;
                height: 20dp;
                translate: Offset { x: if on { 8dp } else { 0dp }, y: 0dp };
                on click { on = !on; }
            }
        }
    }
}
"#;

#[test]
fn a_translate_and_its_transition_export_as_css_translate() {
    let page = export(MOVED, false).unwrap();
    let css = &page.styles;
    assert!(
        css.contains("translate: calc(4 * 1px) calc(1 * 0.0625rem);"),
        "{css}"
    );
    assert!(css.contains("translate 0.1s"), "{css}");
    assert!(css.contains("border-radius 0.2s"), "{css}");
    // A spring moves for as long as it takes to settle.
    let settle = viso_math::Spring::new(300.0, 10.0, 1.0).settle_time(1e-3);
    let at = css.find("opacity ").expect(css);
    let shown = css[at + 8..].split('s').next().unwrap();
    let seconds: f32 = shown.parse().unwrap();
    assert!((seconds - settle).abs() < 1e-3, "{seconds} vs {settle}");
    assert!(css[at..].starts_with(&format!("opacity {shown}s linear(0, ")));
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.vtranslate("), "{script}");
}

const TURNED: &str = r#"
export component Turned {
    state on = false;
    view {
        Column {
            Text {
                width: 40dp;
                height: 20dp;
                scale: 1.5f32;
                rotation: 90deg;
                transform_origin: top_start;
                transition.rotation: Transition { duration: 100ms };
            }
            Text {
                width: 40dp;
                height: 20dp;
                scale: if on { 2.0f32 } else { 1.0f32 };
                transform_origin: if on { Alignment2D::end } else { Alignment2D::center };
                on click { on = !on; }
            }
        }
    }
}
"#;

#[test]
fn a_scale_rotation_and_origin_export_as_css_and_the_origin_follows_the_direction() {
    let page = export(TURNED, false).unwrap();
    let css = &page.styles;
    for want in [
        "scale: 1.5;",
        "rotate: 90deg;",
        "transform-origin: left top;",
        ":dir(rtl) { transform-origin: right top; }",
        "rotate 0.1s",
        "transform-origin: var(--v",
    ] {
        assert!(css.contains(want), "{want}: {css}");
    }
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.vscale("), "{script}");
    assert!(script.contains("P.vorigin("), "{script}");
}

const SHAKEN: &str = r#"
export component Shaken {
    state ended = false;
    view {
        Column {
            Button("Shake") {
                width: 40dp; height: 20dp; 
                on click {
                    let shake = card.animate(Animate::rotation(10deg), Transition { duration: 80ms });
                    if shake.is_running() { shake.finish(); }
                }
            }
            node card: Text("card") {
                width: 40dp; height: 20dp; 
                on animation_end(e) { ended = e.finished; }
            }
        }
    }
}
"#;

#[test]
fn an_animation_exports_as_a_web_animation_and_its_end_as_an_event() {
    let page = export(SHAKEN, false).unwrap();
    let script = page.script.as_deref().unwrap();
    for want in [
        ".node(7, ",
        ".node(10, ",
        ".node(9, ",
        "el.animate(",
        "viso-animationend",
    ] {
        assert!(script.contains(want), "{want}: {script}");
    }
}

const PULSED: &str = r#"
export component Pulsed {
    state scaled = false;
    state faded = false;
    view {
        Column {
            Button("Pulse") {
                width: 40dp; height: 20dp; 
                on click {
                    let track = Keyframes {
                        stops: [
                            Keyframe { at: 0.5f32, value: Animate::scale(2.0f32), easing: ease_out },
                            Keyframe { at: 1.0f32, value: Animate::scale(1.0f32) },
                        ],
                        duration: 100ms,
                        repeat: Repeat::times(3),
                        direction: PlayDirection::alternate,
                    };
                    card.play(track);
                }
            }
            Button("Spin") {
                width: 40dp; height: 20dp; 
                on click {
                    let track = Keyframes {
                        stops: [Keyframe { at: 1.0f32, value: Animate::opacity(0.0f32) }],
                        repeat: Repeat::forever,
                    };
                    let spin = card.play(track);
                    if spin.is_running() { spin.finish(); }
                }
            }
            node card: Text("card") {
                width: 40dp; height: 20dp; 
                on animation_end(e) {
                    scaled = match e.target { Animate::scale(_) => e.finished, _ => scaled };
                    faded = match e.target { Animate::opacity(_) => e.finished, _ => faded };
                }
            }
            Text("scaled={scaled}")
            Text("faded={faded}")
        }
    }
}
"#;

#[test]
fn a_track_plays_as_a_web_animation_through_its_stops() {
    let page = export(PULSED, false).unwrap();
    let script = page.script.clone().unwrap();
    assert!(script.contains(".node(11, "), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    // Headless Chrome's virtual time does not run an animation's timeline,
    // so what it plays is read off the animation itself.
    let clicks = "\nsetTimeout(() => document.querySelectorAll('button').forEach((b) => b.click()), 50);\n\
        setTimeout(() => document.body.append('played=' + document.getAnimations().map((a) => {\n\
          const t = a.effect.getTiming();\n\
          return a.effect.getKeyframes().map((k) => k.offset + ':' + k.scale + ':' + k.easing).join(' ') + '|' + t.iterations + '|' + t.direction;\n\
        }).join(';') + '.'), 100);\n";
    let dom = run_in_chrome(&page, &(script + clicks), "export-pulsed", &[], chrome);
    // Shown before its first stop, eased into each from the one before.
    assert!(
        dom.contains("played=0:none:linear(0 0%, ")
            && dom.contains(" 0.5:2:linear 1:1:linear|3|alternate."),
        "{dom}"
    );
    assert!(
        dom.contains(">faded=true<"),
        "a track forever finishes where it is: {dom}"
    );
    assert!(
        dom.contains(">scaled=false<"),
        "and leaves the other playing: {dom}"
    );
}

const GUARDED: &str = r#"
export component Guarded {
    state on = true;
    view {
        Column {
            Button("Off") { width: 40dp; height: 20dp; enabled: false }
            Text { width: 40dp; height: 20dp; enabled: on; on click { on = !on; } }
            TextInput { width: 100dp; height: 20dp; autofocus: true; }
        }
    }
}
"#;

#[test]
fn enabled_and_autofocus_export_as_a_guarded_subtree_and_the_autofocus_attribute() {
    let page = export(GUARDED, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("data-viso-off aria-disabled=\"true\" disabled"),
        "{index}"
    );
    assert!(index.contains(" autofocus"), "{index}");
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.enabled("), "{script}");
    assert!(script.contains("closest(\"[data-viso-off]\")"), "{script}");
}

const PICKED: &str = r#"
style Tab for Text {
    opacity: 0.5f32;
    when selected { opacity: 1.0f32; }
}
export component Picked {
    state tab: U32 = 0;
    view {
        Tabs {
            bind selected <=> tab;
            Text { width: 40dp; height: 20dp; styles: [Tab]; }
            Text { width: 40dp; height: 20dp; styles: [Tab]; }
        }
    }
}
"#;

#[test]
fn a_selected_style_follows_the_strip_s_selection_attributes() {
    let page = export(PICKED, false).unwrap();
    let css = &page.styles;
    assert!(
        css.contains(r#":is([aria-selected="true"],[role="radio"][aria-checked="true"])"#),
        "{css}"
    );
}

const DRAGGED: &str = r#"
style Grab for Text {
    when dragging { opacity: 0.5f32; }
}
export component Dragged {
    state shift = Offset { x: 0dp, y: 0dp };
    view {
        Text {
            width: 40dp;
            height: 20dp;
            styles: [Grab];
            translate: shift;
            on drag_move(event) { shift = event.total; }
            on long_press(event) { shift = Offset { x: 0dp, y: 0dp }; }
        }
    }
}
"#;

#[test]
fn a_drag_and_a_long_press_export_as_pointer_gestures_and_the_winner_as_dragging() {
    let page = export(DRAGGED, false).unwrap();
    assert!(
        page.styles.contains("[data-viso-dragging]"),
        "{}",
        page.styles
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains(", 16, false, "), "{script}");
    assert!(script.contains("setPointerCapture"), "{script}");
    assert!(script.contains(", 18, false, "), "{script}");
}

const TAPPED: &str = r#"
export component Tapped {
    state taps = 0;
    state doubles = 0;
    view {
        Column {
            Row {
                width: 40dp; height: 20dp;
                on tap { taps += 1; }
                on double_tap(event) { doubles += 1; }
            }
            Text("taps={taps}")
            Text("doubles={doubles}")
        }
    }
}
"#;

#[test]
fn a_double_tap_exports_as_dblclick_and_the_tap_waits_it_out() {
    let page = export(TAPPED, false).unwrap();
    let script = page.script.clone().unwrap();
    assert!(script.contains(", 23, false, "), "{script}");
    assert!(script.contains("\"dblclick\""), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let taps = "\nconst row = () => document.querySelector('body div div');\n\
        const press = (type, detail) => row().dispatchEvent(new MouseEvent(type, { bubbles: true, detail }));\n\
        setTimeout(() => press('click', 1), 50);\n\
        setTimeout(() => document.body.append('early=' + document.body.textContent.includes('taps=1') + '.'), 100);\n\
        setTimeout(() => { press('click', 1); press('click', 2); press('dblclick', 2); }, 700);\n\
        setTimeout(() => document.body.append('late=' + document.body.textContent.includes('taps=1') + '.'), 1400);\n";
    let dom = run_in_chrome(&page, &(script + taps), "export-tapped", &[], chrome);
    assert!(dom.contains("early=false."), "the tap waits: {dom}");
    assert!(dom.contains("late=true."), "and runs alone: {dom}");
    assert!(dom.contains(">doubles=1<"), "{dom}");
}

const SWIPED: &str = r#"
export component Swiped {
    state lefts = 0;
    state ends = 0;
    view {
        Column {
            Row {
                width: 200dp; height: 100dp;
                on swipe(event) { lefts += match event.direction { SwipeDirection::left => 1, _ => 0 }; }
                on drag_end(event) { ends += 1; }
            }
            Text("lefts={lefts}")
            Text("ends={ends}")
        }
    }
}
"#;

#[test]
fn a_swipe_exports_as_a_drag_that_lifts_fast() {
    let page = export(SWIPED, false).unwrap();
    let script = page.script.clone().unwrap();
    assert!(script.contains(", 24, false, "), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let stroke = "\nconst row = () => document.querySelector('body div div');\n\
        const at = (type, x) => row().dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1, pointerType: 'mouse', isPrimary: true, button: type === 'pointermove' ? -1 : 0, clientX: x, clientY: 50 }));\n\
        [[50, 'pointerdown', 150], [60, 'pointermove', 130], [70, 'pointermove', 100], [80, 'pointermove', 70], [90, 'pointerup', 70],\n\
         [300, 'pointerdown', 150], [400, 'pointermove', 140], [500, 'pointermove', 130], [600, 'pointerup', 130]]\n\
          .forEach(([t, type, x]) => setTimeout(() => at(type, x), t));\n";
    let dom = run_in_chrome(&page, &(script + stroke), "export-swiped", &[], chrome);
    assert!(dom.contains(">ends=2<"), "{dom}");
    assert!(
        dom.contains(">lefts=1<"),
        "the slow drag is no swipe: {dom}"
    );
}

const PINCHED: &str = r#"
export component Pinched {
    state zoom: F32 = 1.0;
    state ends = 0;
    view {
        Column {
            Row {
                width: 200dp; height: 200dp;
                on pinch_move(event) { zoom = event.scale; }
                on pinch_end(event) { ends += 1; }
            }
            Text("zoom={zoom}")
            Text("ends={ends}")
        }
    }
}
"#;

#[test]
fn a_pinch_exports_as_two_touches_and_a_control_wheel() {
    let page = export(PINCHED, false).unwrap();
    let script = page.script.clone().unwrap();
    assert!(script.contains(", 26, false, "), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let pinch = "\nconst row = () => document.querySelector('body div div');\n\
        const touch = (type, id, x) => row().dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: id, pointerType: 'touch', isPrimary: id === 2, button: type === 'pointermove' ? -1 : 0, clientX: x, clientY: 50 }));\n\
        const wheel = (dy) => row().dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, ctrlKey: true, deltaY: dy, clientX: 50, clientY: 50 }));\n\
        setTimeout(() => { touch('pointerdown', 2, 50); touch('pointerdown', 3, 100); touch('pointermove', 3, 140); touch('pointermove', 3, 150); touch('pointerup', 3, 150); touch('pointerup', 2, 50); }, 50);\n\
        setTimeout(() => document.body.append('touched=' + document.body.textContent.includes('zoom=2') + '.'), 100);\n\
        setTimeout(() => { wheel(-100); wheel(100); }, 200);\n";
    let dom = run_in_chrome(&page, &(script + pinch), "export-pinched", &[], chrome);
    assert!(dom.contains("touched=true."), "{dom}");
    assert!(
        dom.contains(">zoom=1<"),
        "the wheel zooms in and back: {dom}"
    );
    assert!(dom.contains(">ends=2<"), "and rests: {dom}");
}

#[test]
fn a_drag_axis_exports_as_the_axis_a_touch_drags_along() {
    const AXIS: &str = r#"
export component Axed {
    state n = 0;
    view {
        Column {
            Row { width: 40dp; height: 20dp; drag_axis: row; on drag_start(e) { n += 1; } }
        }
    }
}
"#;
    let page = export(AXIS, false).unwrap();
    assert!(
        page.index.contains("data-drag-axis=\"row\""),
        "{}",
        page.index
    );
    let script = page.script.unwrap();
    assert!(script.contains("pan-y"), "the page still scrolls across it");
}

const VIEWED: &str = r#"
export component Viewed {
    state most: F32 = 3.0;
    view {
        Column {
            InteractiveViewer {
                width: 200dp; height: 100dp;
                max_scale: most;
                pan_margin: 10dp;
                Column { width: 100%; height: 100%; background: #336699; }
            }
        }
    }
}
"#;

#[test]
fn an_interactive_viewer_exports_as_a_clipped_box_its_script_pans_and_zooms() {
    let page = export(VIEWED, false).unwrap();
    assert!(
        page.index.contains("data-pan-margin=\"10\""),
        "{}",
        page.index
    );
    let all = format!("{}{}", page.index, page.styles);
    assert!(all.contains("overflow: hidden"), "{all}");
    let script = page.script.clone().unwrap();
    assert!(script.contains("viewer("), "{script}");
    assert!(
        script.contains("P.data("),
        "the bound limit follows its state"
    );
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let zoom = "\nconst box = () => document.querySelector('[data-pan-margin]');\n\
        const at = () => box().firstElementChild.style.transform;\n\
        setTimeout(() => { const r = box().getBoundingClientRect(); box().dispatchEvent(new MouseEvent('dblclick', { bubbles: true, detail: 2, clientX: r.left, clientY: r.top })); }, 50);\n\
        setTimeout(() => document.body.append('zoomed=' + at() + '.'), 900);\n\
        setTimeout(() => { box().dispatchEvent(new WheelEvent('wheel', { bubbles: true, cancelable: true, deltaY: 500 })); document.body.append('wheeled=' + at() + '.'); }, 1000);\n";
    let dom = run_in_chrome(&page, &(script + zoom), "export-viewed", &[], chrome);
    assert!(
        dom.contains("zoomed=translate(0px, 0px) scale(2.5)."),
        "a double click zooms about where it lands: {dom}"
    );
    assert!(
        dom.contains("wheeled=translate(0px, -160px) scale(2.5)."),
        "a wheel pans to the bound and the margin past it: {dom}"
    );
}

const SWIPED_ROW: &str = r#"
export component Inbox {
    state deleted = 0;
    view {
        Column {
            SwipeRow {
                width: 300dp; height: 40dp;
                full_swipe: FullSwipe::trailing;
                on full_swiped(e) { if e.edge == SwipeEdge::trailing { deleted += 1; } }
                Column { width: 100%; height: 100%; }
                SwipeActions {
                    edge: SwipeEdge::trailing;
                    Button("Flag") { width: 70dp; height: 40dp }
                    Button("Delete") { width: 70dp; height: 40dp; on click { deleted += 10; } }
                }
            }
            Text("deleted={deleted}")
        }
    }
}
"#;

#[test]
fn a_swipe_row_exports_as_a_clipped_row_its_script_slides_open() {
    let page = export(SWIPED_ROW, false).unwrap();
    assert!(
        page.index.contains("data-swipe-edge=\"1\"") && page.index.contains("data-full-swipe"),
        "{}",
        page.index
    );
    let all = format!("{}{}", page.index, page.styles);
    assert!(all.contains("overflow: hidden"), "{all}");
    let script = page.script.clone().unwrap();
    assert!(script.contains("swipeRow("), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let swipe = "\nconst row = () => document.querySelector('[data-full-swipe]');\n\
        const content = () => row().firstElementChild;\n\
        const at = (type, x) => content().dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1, pointerType: 'mouse', isPrimary: true, button: type === 'pointermove' ? -1 : 0, clientX: x, clientY: 20 }));\n\
        const drag = (t, from, to) => { setTimeout(() => at('pointerdown', from), t); for (let k = 1; k <= 5; k++) setTimeout(() => at('pointermove', from + (to - from) * k / 5), t + 100 * k); setTimeout(() => at('pointerup', to), t + 600); };\n\
        drag(50, 250, 150);\n\
        setTimeout(() => document.body.append('opened=' + content().style.transform + '.'), 1500);\n\
        setTimeout(() => [...row().querySelectorAll('button')].find((b) => b.textContent === 'Delete').click(), 1600);\n\
        setTimeout(() => document.body.append('shut=' + content().style.transform + '.'), 2500);\n\
        drag(2600, 290, 20);\n";
    let dom = run_in_chrome(&page, &(script + swipe), "export-swipe-row", &[], chrome);
    assert!(
        dom.contains("opened=translateX(-140px)."),
        "a slow drag past half the actions opens on them: {dom}"
    );
    assert!(
        dom.contains("shut=translateX(0px)."),
        "an action shuts it: {dom}"
    );
    assert!(
        dom.contains(">deleted=11<"),
        "the action ran, then a full swipe: {dom}"
    );
}

const SHEETED: &str = r#"
export component Page {
    state shown = false;
    state rest: U32 = 1;
    state asks = 0;
    view {
        Column {
            Button("Open") { on click { shown = true; } }
            Text("rest={rest} shown={shown} asks={asks}.")
            Sheet {
                bind open <=> shown;
                bind detent <=> rest;
                detents: [50dp, 100%];
                on changed(event) { asks += 1; }
                Column { width: 300dp; height: 200dp; background: #336699; }
            }
        }
    }
}
"#;

#[test]
fn a_sheet_exports_as_a_dialog_its_script_slides_to_detents_and_off() {
    let page = export(SHEETED, false).unwrap();
    assert!(
        page.index.contains("<dialog")
            && page.index.contains("data-sheet-edge=\"0\"")
            && page.index.contains("data-detents=\"50px 100%\""),
        "{}",
        page.index
    );
    let script = page.script.clone().unwrap();
    assert!(script.contains("sheet("), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let slide = "\nconst dialog = () => document.querySelector('dialog');\n\
        const panel = () => dialog().firstElementChild;\n\
        const at = (type, y) => panel().dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1, pointerType: 'mouse', isPrimary: true, button: type === 'pointermove' ? -1 : 0, clientX: 20, clientY: y }));\n\
        const said = (what) => document.body.append(what + ' ' + document.querySelector('span').textContent + ' ' + panel().style.transform + ' ' + dialog().open + '|');\n\
        setTimeout(() => document.querySelector('button').click(), 50);\n\
        setTimeout(() => said('opened'), 700);\n\
        setTimeout(() => { const y = panel().getBoundingClientRect().top + 100; at('pointerdown', y); for (let k = 1; k <= 5; k++) setTimeout(() => at('pointermove', y + 22 * k), 100 * k); setTimeout(() => at('pointerup', y + 110), 900); }, 800);\n\
        setTimeout(() => said('dragged'), 2400);\n\
        setTimeout(() => dialog().dispatchEvent(new Event('cancel', { cancelable: true })), 2500);\n\
        setTimeout(() => said('closed'), 3300);\n";
    let dom = run_in_chrome(&page, &(script + slide), "export-sheet", &[], chrome);
    assert!(
        dom.contains("opened rest=1 shown=true asks=0. translateY(0px) true|"),
        "it opens to all of its panel: {dom}"
    );
    assert!(
        dom.contains("dragged rest=0 shown=true asks=0. translateY(150px) true|"),
        "a drag down brings it to its lesser detent: {dom}"
    );
    assert!(
        dom.contains("closed rest=0 shown=false asks=1."),
        "Escape closes it: {dom}"
    );
    assert!(dom.contains(" false|"), "out, it closes: {dom}");
}

const REORDERED: &str = r#"
export component Groceries {
    state moved = 0;
    view {
        Column {
            Column {
                width: 300dp;
                on reordered(e) { moved = (e.from as I64) * 10 + (e.to as I64) + 100; }
                Row { width: 300dp; height: 40dp; ReorderHandle { width: 40dp; height: 40dp; semantics.label: "Move milk"; } }
                Row { width: 300dp; height: 40dp; ReorderHandle { width: 40dp; height: 40dp; semantics.label: "Move eggs"; } }
                Row { width: 300dp; height: 40dp; ReorderHandle { width: 40dp; height: 40dp; semantics.label: "Move bread"; } }
            }
            Text("moved={moved}")
        }
    }
}
"#;

#[test]
fn a_reorderable_list_exports_with_handles_its_script_carries_rows_by() {
    let page = export(REORDERED, false).unwrap();
    assert!(
        page.index.contains("data-reorder-handle") && page.index.contains("role=\"button\""),
        "{}",
        page.index
    );
    let script = page.script.clone().unwrap();
    assert!(script.contains(", 29, false, "), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let carry = "\nconst handles = () => [...document.querySelectorAll('[data-reorder-handle]')];\n\
        const at = (type, y) => handles()[0].dispatchEvent(new PointerEvent(type, { bubbles: true, pointerId: 1, pointerType: 'mouse', isPrimary: true, button: type === 'pointermove' ? -1 : 0, clientX: 20, clientY: y }));\n\
        const key = (k) => document.activeElement.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, key: k }));\n\
        const first = () => handles()[0].getBoundingClientRect().top;\n\
        setTimeout(() => { const y = first() + 20; at('pointerdown', y); for (let k = 1; k <= 5; k++) at('pointermove', y + 10 * k); at('pointerup', y + 50); }, 50);\n\
        setTimeout(() => document.body.append('carried=' + document.body.textContent.includes('moved=101') + '.'), 600);\n\
        setTimeout(() => { handles()[2].focus(); key(' '); key('ArrowUp'); key('ArrowUp'); key('Enter'); }, 700);\n";
    let dom = run_in_chrome(&page, &(script + carry), "export-reordered", &[], chrome);
    assert!(
        dom.contains("carried=true."),
        "a drag carries the first row on a place: {dom}"
    );
    assert!(
        dom.contains(">moved=120<"),
        "the keyboard carries the last up: {dom}"
    );
}

const CLIPPED: &str = r#"
export component Clipped {
    state on = true;
    view {
        Column {
            Column { width: 40dp; height: 20dp; corner_radius: 4dp; clip: true; }
            Column { width: 40dp; height: 20dp; clip: on; }
        }
    }
}
"#;

#[test]
fn clip_exports_as_hidden_overflow() {
    let page = export(CLIPPED, false).unwrap();
    let all = format!("{}{}", page.index, page.styles);
    assert!(all.contains("overflow: hidden"), "{all}");
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.clip("), "{script}");
}

const POINTED: &str = r#"
export component Pointed {
    state busy = false;
    view {
        Column {
            Column { width: 40dp; height: 20dp; cursor: pointer; }
            Column { width: 40dp; height: 20dp; cursor: if busy { Cursor::wait } else { Cursor::resize_col }; }
        }
    }
}
"#;

#[test]
fn a_cursor_exports_as_the_css_cursor() {
    let page = export(POINTED, false).unwrap();
    let all = format!("{}{}", page.index, page.styles);
    assert!(all.contains("cursor: pointer"), "{all}");
    let script = page.script.as_deref().unwrap();
    assert!(
        script.contains("P.cursor("),
        "a changing cursor is set as it changes"
    );
}

const NOTED: &str = r#"
export component Noted {
    state loud = false;
    view {
        Column {
            Text {
                font_style: italic;
                font_weight: bold;
                spans: [
                    Span { text: "a<b " },
                    Span { text: "x", color: Option::Some(#ff0000), underline: true },
                    Span { text: "2", font_size: Option::Some(75%), raise: Option::Some(0.5em) },
                ];
            }
            Text { spans: [Span { text: "now", underline: loud }]; on click { loud = !loud; } }
        }
    }
}
"#;

#[test]
fn a_text_s_spans_weight_and_slant_export_as_styled_spans() {
    let page = export(NOTED, false).unwrap();
    let all = format!("{}{}", page.index, page.styles);
    assert!(all.contains("font-style: italic"), "{all}");
    assert!(all.contains("font-weight: 700"), "{all}");
    assert!(all.contains("a&lt;b "), "piece text is escaped: {all}");
    assert!(
        all.contains("text-decoration-line: underline"),
        "a piece's lines: {all}"
    );
    assert!(all.contains("vertical-align: 0.5em"), "{all}");
    assert!(
        all.contains("font-size: 0.75em"),
        "a `%` size is of the text's: {all}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(
        script.contains("P.spans("),
        "pieces that change are rebuilt as they change"
    );
}

const LINKED: &str = r#"
export component Doc {
    state followed = 0;
    view {
        Column {
            Text { spans: [Span { text: "see " }, Span { text: "docs", link: Option::Some("/a?b&c") }]; }
            Text {
                spans: [Span { text: "site", link: Option::Some("https://viso.dev") }];
                on link(e) { followed += 1; }
            }
        }
    }
}
"#;

#[test]
fn a_span_s_link_exports_as_an_anchor_and_on_link_hears_its_clicks() {
    let still = LINKED.replace("on link(e) { followed += 1; }", "");
    let page = export(&still, true).unwrap();
    assert!(
        page.index.contains(r#"<a href="/a?b&amp;c""#),
        "a constant link is an escaped anchor: {}",
        page.index
    );
    let page = export(LINKED, false).unwrap();
    let script = page.script.as_deref().unwrap();
    assert!(
        script.contains("route === 19"),
        "a link's clicks reach `on link`"
    );
}

const SELECTABLE: &str = r#"
export component Note {
    state pick = false;
    view {
        Column {
            Text("always") { selectable: true }
            Text("sometimes") { selectable: pick; on click { pick = !pick; } }
        }
    }
}
"#;

#[test]
fn a_selectable_text_exports_as_one_the_browser_lets_select() {
    let page = export(SELECTABLE, false).unwrap();
    assert!(
        page.styles.contains("user-select: none"),
        "other text is not"
    );
    assert!(page.index.contains("vx-selectable"), "{}", page.index);
    let script = page.script.as_deref().unwrap();
    assert!(
        script.contains("P.selectable("),
        "a changing one follows its state"
    );
}

const LOGIN: &str = r#"

export component Login {
    state hidden = true;
    view {
        Column {
            TextInput { width: 100dp; height: 20dp; placeholder: "Password"; secure: true; }
            TextInput { width: 100dp; height: 20dp; placeholder: "Name"; secure: hidden; }
            CheckBox("Remember") { width: 20dp; height: 20dp }
        }
    }
}
"#;

#[test]
fn a_field_s_placeholder_and_masking_and_a_check_s_caption_export() {
    let page = export(LOGIN, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("type=\"password\" placeholder=\"Password\""),
        "{index}"
    );
    assert!(index.contains("placeholder=\"Name\""), "{index}");
    assert!(
        index.contains("type=\"checkbox\" aria-label=\"Remember\""),
        "{index}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.secure(e[0], f"), "{script}");
}

const NOTES: &str = r#"
export component Notes {
    state lines = true;
    view {
        Column {
            TextInput { width: 100dp; height: 40dp; value: "a"; multiline: true; }
            TextInput { width: 100dp; height: 20dp; multiline: true; secure: true; }
            TextInput { width: 100dp; height: 20dp; multiline: lines; }
        }
    }
}
"#;

#[test]
fn a_multi_line_field_exports_as_a_textarea_and_one_that_turns_multi_line_cannot() {
    let findings = export(NOTES, false).unwrap_err();
    assert!(
        findings
            .iter()
            .any(|(need, at, message)| *need == Need::Unsupported
                && at == "lines"
                && message.contains("holds lines")),
        "{findings:#?}"
    );
    let page = export(&NOTES.replace("multiline: lines;", ""), false).unwrap();
    let index = &page.index;
    assert!(index.contains(">a</textarea>"), "{index}");
    assert_eq!(
        index.matches("<textarea").count(),
        1,
        "a secure field holds one line: {index}"
    );
}

const AMOUNTS: &str = r#"
export component Amounts {
    state kind = InputKind::digits;
    view {
        Column {
            TextInput { width: 100dp; height: 20dp; input: decimal; max_length: 8; }
            TextInput { width: 100dp; height: 20dp; input: email; }
            TextInput { width: 100dp; height: 20dp; input: kind; }
        }
    }
}
"#;

#[test]
fn what_a_field_holds_exports_as_its_keyboard_and_pattern() {
    let page = export(AMOUNTS, false).unwrap();
    let index = &page.index;
    assert!(index.contains(r#"inputmode="decimal""#), "{index}");
    assert!(index.contains(r#"pattern="-?[0-9]*[.]?[0-9]*""#), "{index}");
    assert!(index.contains(r#"maxlength="8""#), "{index}");
    assert!(index.contains(r#"inputmode="email""#), "{index}");
    let script = page.script.as_deref().unwrap();
    assert!(
        script.contains("P.kind("),
        "a changing kind is kept current"
    );
}

const VOLUME: &str = r#"
export component Volume {
    state volume: F64 = 2.5;
    view {
        Column {
            NumberField {
                width: 100dp;
                height: 20dp;
                value: volume;
                min: 0.0;
                max: 10.0;
                step: 0.5;
                on changed(e) { volume = e.value; }
            }
            NumberField { width: 100dp; height: 20dp; min: 0.0; max: 99.0; }
        }
    }
}
"#;

#[test]
fn a_number_field_exports_as_a_spin_button_that_steps() {
    let page = export(VOLUME, false).unwrap();
    let index = &page.index;
    assert!(index.contains(r#"role="spinbutton""#), "{index}");
    assert!(index.contains(r#"inputmode="decimal""#), "{index}");
    assert!(index.contains(r#"aria-valuemin="0.0""#), "{index}");
    assert!(index.contains(r#"aria-valuemax="10.0""#), "{index}");
    assert!(index.contains(r#"data-step="0.5""#), "{index}");
    assert!(
        index.contains(r#"pattern="[0-9]*""#),
        "whole and unsigned: {index}"
    );
    assert!(
        index.contains(r#"aria-valuemax="99" data-step="1" data-places="0" value="0""#),
        "no number shows its origin: {index}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.number("), "the number is kept current");
}

const COUNTER: &str = r#"
export component Counter {
    state count = 0;
    view {
        Column {
            Text("{count}") { width: 100dp; height: 20dp }
            Button("Add") {
                width: 100dp;
                height: 32dp;
                on click { count += 1; }
            }
        }
    }
}
"#;

#[test]
fn static_refuses_what_changes_at_its_place() {
    let findings = export(COUNTER, true).unwrap_err();
    let needs: Vec<_> = findings
        .iter()
        .map(|(n, at, _)| (*n, at.as_str()))
        .collect();
    assert_eq!(
        needs,
        [
            (Need::Dynamic, "\"{count}\""),
            (Need::Dynamic, "on click { count += 1; }"),
        ],
        "{findings:#?}"
    );
}

#[test]
fn what_changes_is_driven_by_the_page_script() {
    let page = export(COUNTER, false).unwrap();
    let script = page.script.as_deref().unwrap();
    assert!(
        page.index
            .contains("<script src=\"app.js\" defer></script>")
    );
    assert!(
        script.starts_with("// Generated from Viso source."),
        "{script}"
    );
    assert!(script.contains("let S = [0n];"), "{script}");
    // The text re-evaluates only when `count` changes.
    assert!(
        script.contains("u.push([[0], () => P.text(e[0], f"),
        "{script}"
    );
    assert!(
        script.contains("on(e[1], 0, false, 15, (p) => f"),
        "{script}"
    );
    assert!(script.contains("$.ck(\"i64\", "), "{script}");
    assert!(script.ends_with("start(m0);\n})();\n"));
}

const REGIONS: &str = r#"
export component Todo {
    state items: List<String> = ["a", "b"];
    state open = true;
    view {
        Column {
            if open {
                Text("Open") { width: 100dp; height: 20dp }
            } else {
                Text("Closed") { width: 100dp; height: 20dp }
            }
            for item in items key item {
                Text(item) { width: 100dp; height: 20dp }
            }
        }
    }
}
"#;

#[test]
fn regions_mount_from_templates() {
    let findings = export(REGIONS, true).unwrap_err();
    assert!(
        findings.iter().all(|(need, at, _)| *need == Need::Dynamic
            && (at.starts_with("if") || at.starts_with("for"))),
        "{findings:#?}"
    );
    let page = export(REGIONS, false).unwrap();
    assert!(
        page.index.contains(
            "<template data-v><template><span class=\"vx-text s1\">Open</span></template>\
             <template><span class=\"vx-text s1\">Closed</span></template></template>"
        ),
        "{}",
        page.index
    );
    let script = page.script.unwrap();
    assert!(script.contains("{ k: 0, sel: (sc) => f"), "{script}");
    assert!(script.contains("{ k: 2, items: (sc) => f"), "{script}");
    assert!(
        script.contains("r.push(region(e[0], G[0], sc, m));"),
        "{script}"
    );
    // The text of each item reads its binding.
    assert!(script.contains("P.text(e[0], f"), "{script}");
}

#[test]
fn a_virtual_list_is_a_list_of_its_rows() {
    let page = export(
        "export component Feed {
            state items = [1, 2, 3];
            view {
                VirtualList {
                    width: 200dp;
                    height: 100dp;
                    for i in items key i {
                        Column { width: 200dp; height: 20dp; Button(\"go\") }
                    }
                }
            }
        }",
        false,
    )
    .unwrap();
    assert!(page.index.contains("role=\"list\""), "{}", page.index);
    assert!(
        page.index.contains("role=\"listitem\"><button"),
        "{}",
        page.index
    );
    assert!(!page.index.contains("<button role="), "{}", page.index);
}

#[test]
fn a_shader_has_no_html_form() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/10-custom-shader");
    let source = std::fs::read_to_string(example.join("src/ripples.vs")).unwrap();
    let findings = export_in(&source, Some(&example.join("assets")), false).unwrap_err();
    assert!(
        findings
            .iter()
            .any(|(need, at, message)| *need == Need::GpuOnly
                && at.starts_with("ShaderView")
                && message.contains("shader")),
        "{findings:#?}"
    );
}

/// A view translated into German and Traditional Chinese beside English,
/// exported with a script or `static_only`.
fn translated(static_only: bool) -> HtmlExport {
    use viso_dsl::i18n::{CatalogFile, Messages};
    let file = |locale: &str, text: &str| CatalogFile {
        locale: locale.into(),
        path: format!("i18n/{locale}.toml"),
        text: text.into(),
    };
    let messages = Messages::compile(
        "en",
        &[
            file(
                "en",
                "[inbox]\ntitle = \"Inbox\"\nunread = \"{count, plural, one {# unread} other {# unread}}\"\n",
            ),
            file(
                "de",
                "[inbox]\ntitle = \"Posteingang\"\nunread = \"{count, plural, one {# ungelesen} other {# ungelesene}}\"\n",
            ),
            file("zh-Hant", "inbox = { title = \"收件匣\" }\n"),
        ],
    );
    let profile = TargetProfile {
        messages: Some(Rc::new(messages)),
        ..TargetProfile::default()
    };
    let source = "import viso::i18n::{tr};\nexport component Inbox {\n    view {\n        Column {\n            Text(tr(\"inbox.title\"))\n            Text(tr(\"inbox.unread\", count: 1))\n        }\n    }\n}\n";
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    html(&compiled, &options(static_only)).unwrap()
}

#[test]
fn a_page_ships_every_locale_and_speaks_its_readers() {
    // A page with no script is written in the source's language.
    let fixed = translated(true);
    assert!(fixed.script.is_none());
    assert!(fixed.index.contains(">Inbox<"), "{}", fixed.index);
    assert!(fixed.index.contains(">1 unread<"), "{}", fixed.index);

    let page = translated(false);
    let script = page.script.expect("translated text is the script's");
    assert!(
        script.contains("Ls: [\"en\", \"de\", \"zh-Hant\"]"),
        "{script}"
    );
    assert!(script.contains("settle(T);"), "{script}");
    assert!(
        !page.index.contains(">Inbox<"),
        "the reader's language is not known when the page is written: {}",
        page.index
    );
    // The page script's messages, read as a browser in each language would.
    let Ok(node) = which("node") else {
        eprintln!("no `node` on PATH: the locale choice is not run");
        return;
    };
    let messages = script
        .lines()
        .find(|line| line.starts_with("const T = "))
        .expect("the messages");
    for (languages, title, unread) in [
        ("[\"de-AT\", \"en\"]", "Posteingang", "1 ungelesen"),
        ("[\"zh-TW\"]", "收件匣", "1 unread"),
        ("[\"fr-FR\", \"de\"]", "Posteingang", "1 ungelesen"),
        ("[\"fr-FR\"]", "Inbox", "1 unread"),
    ] {
        let program = format!(
            "{}\n{messages}\nObject.defineProperty(globalThis, \"navigator\", {{ value: {{ languages: {languages} }}, configurable: true }});\nsettle(T);\n\
             console.log(T.L + \"|\" + translate(T, \"inbox.title\", []) + \"|\" + translate(T, \"inbox.unread\", [1n]));\n",
            include_str!("../src/export/values.js")
        );
        let out = std::process::Command::new(&node)
            .args(["-e", &program])
            .output()
            .unwrap();
        let said = String::from_utf8_lossy(&out.stdout);
        let fields: Vec<&str> = said.trim().split('|').collect();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(fields[1..], [title, unread], "{languages}: {said}");
    }
}

#[test]
fn a_page_writes_dates_and_times_as_its_reader_does() {
    use viso_dsl::i18n::{CatalogFile, Messages};
    let messages = Messages::compile(
        "en",
        &[CatalogFile {
            locale: "en".into(),
            path: "i18n/en.toml".into(),
            text: "when = \"{at, date, full}, {at, time, short}; {d, date, short}; {n, number, integer}\"\n".into(),
        }],
    );
    let profile = TargetProfile {
        messages: Some(Rc::new(messages)),
        ..TargetProfile::default()
    };
    let source = "import viso::i18n::{tr};\nexport component Agenda {\n    state day = Date { year: 2026, month: 10, day: 26 };\n    view { Text(tr(\"when\", at: DateTime { date: day, time: Time { hour: 16, minute: 5 } }, d: day, n: 2.5)) }\n}\n";
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let script = html(&compiled, &options(false))
        .unwrap()
        .script
        .expect("a script");
    assert!(script.contains("[5, 0, \"full\"]"), "{script}");
    let Ok(node) = which("node") else {
        eprintln!("no `node` on PATH: the formats are not run");
        return;
    };
    let messages = script
        .lines()
        .find(|line| line.starts_with("const T = "))
        .expect("the messages");
    for (languages, said) in [
        (
            "[\"en-US\"]",
            "Monday, October 26, 2026, 4:05\u{202f}PM; 10/26/26; 2",
        ),
        (
            "[\"de-DE\"]",
            "Montag, 26. Oktober 2026, 16:05; 26.10.26; 2",
        ),
    ] {
        let program = format!(
            "{}\n{messages}\nObject.defineProperty(globalThis, \"navigator\", {{ value: {{ languages: {languages} }}, configurable: true }});\nsettle(T);\n\
             const d = {{ t: 0, f: [2026n, 10n, 26n] }};\n\
             console.log(translate(T, \"when\", [{{ t: 0, f: [d, {{ t: 0, f: [16n, 5n, 0n] }}] }}, d, 2.5]));\n",
            include_str!("../src/export/values.js")
        );
        let out = std::process::Command::new(&node)
            .args(["-e", &program])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        // Browsers' ICU differs in the space before a day period.
        let spaced = |t: &str| t.replace(['\u{202f}', '\u{a0}'], " ");
        assert_eq!(
            spaced(String::from_utf8_lossy(&out.stdout).trim()),
            spaced(said),
            "{languages}"
        );
    }
}

fn which(program: &str) -> Result<std::path::PathBuf, ()> {
    std::env::var_os("PATH")
        .and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|dir| dir.join(program))
                .find(|path| path.is_file())
        })
        .ok_or(())
}

/// Tasks, effects, a resource and components a region mounts, each with
/// states of its own: everything the page runs as it opens, with no input.
const RUNNING: &str = include_str!("sources/running.vs");

#[test]
fn a_page_runs_its_tasks_effects_and_resources() {
    let page = export(RUNNING, false).unwrap();
    let script = page.script.clone().expect("what runs is the script's");
    assert!(script.contains("async function"), "{script}");
    assert!(script.contains("await $.sleep(tk, "), "{script}");
    assert!(script.contains("fx(m, ["), "{script}");
    assert!(
        script.contains("const m = mount(sc, p, ["),
        "a region mounts its components' states: {script}"
    );

    let findings = export(RUNNING, true).unwrap_err();
    assert!(
        findings
            .iter()
            .any(|(need, _, message)| *need == Need::Dynamic
                && message.contains("only the page script runs")),
        "{findings:#?}"
    );

    // The page as a browser runs it.
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let dom = run_in_chrome(&page, &script, "export-running", &[], chrome);
    for shown in [
        ">log=ca2q3d5q4<",
        ">runs=2<",
        ">item=b<",
        ">item=bb<",
        ">y=12<",
        ">tally=pp<",
        ">tally=rp<",
    ] {
        assert!(dom.contains(shown), "{shown} in {dom}");
    }
    for gone in [">item=a<", ">x=", ">tally=rr<"] {
        assert!(!dom.contains(gone), "{gone} in {dom}");
    }
}

fn chrome() -> Option<std::path::PathBuf> {
    let mac = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.is_file() {
        return Some(mac.to_path_buf());
    }
    ["google-chrome", "chromium", "chromium-browser"]
        .into_iter()
        .find_map(|name| which(name).ok())
}

/// Components reading where they sit (`env.size_class` under adaptive
/// scopes, `env.constraints`) beside the window's fields, one of them in a
/// region.
const ADAPTIVE: &str = include_str!("sources/adaptive.vs");

#[test]
fn a_page_follows_the_browsers_environment() {
    let page = export(ADAPTIVE, false).unwrap();
    let script = page
        .script
        .clone()
        .expect("the environment is the script's");
    assert!(script.contains("const env = new Env(["), "{script}");
    assert!(script.contains("env.anchor(e["), "{script}");
    assert!(page.index.contains("data-scope=\"700\""), "{}", page.index);
    assert!(page.index.contains("data-root"), "{}", page.index);

    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let dom = run_in_chrome(
        &page,
        &script,
        "export-adaptive",
        &["--window-size=1000,700"],
        chrome,
    );
    for shown in [
        ">window=expanded<",
        ">landscape<",
        ">scale=1<",
        ">based=medium<",
        ">scoped=compact<",
        ">scoped max=480dp<",
        ">regional=expanded<",
        ">regional max=1000dp<",
    ] {
        assert!(dom.contains(shown), "{shown} in {dom}");
    }
}

/// `page` as headless Chrome shows it once its timers ran.
fn run_in_chrome(
    page: &HtmlExport,
    script: &str,
    name: &str,
    flags: &[&str],
    chrome: std::path::PathBuf,
) -> String {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("index.html"), &page.index).unwrap();
    std::fs::write(dir.join("styles.css"), &page.styles).unwrap();
    std::fs::write(dir.join("app.js"), script).unwrap();
    let out = std::process::Command::new(chrome)
        .args([
            "--headless",
            "--disable-gpu",
            "--no-first-run",
            "--allow-file-access-from-files",
            "--virtual-time-budget=5000",
            &format!("--user-data-dir={}", dir.join("profile").display()),
            "--dump-dom",
        ])
        .args(flags)
        .arg(format!("file://{}", dir.join("index.html").display()))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_style_on_a_component_node_exports_its_inputs_choice() {
    let source = r#"
component Chip {
    @styleable
    input tint: Color = #000000;
    @selector
    state open = false;
    view {
        Text { width: 20dp; height: 20dp; background: tint; on click { open = !open; } }
    }
}
style Lit for Chip { tint: #102030; when open { tint: #ff0000; } }
export component C { view { Column { Chip { styles: [Lit]; } } } }
"#;
    let page = export(source, false).expect("exports");
    let script = page.script.expect("a script");
    assert!(
        page.styles
            .contains("background-color: var(--v1-background-color)"),
        "{}",
        page.styles
    );
    assert!(
        script.contains("u.push([[0], () => P.vcolor(e[0]"),
        "the tint follows `open`: {script}"
    );
}

#[test]
fn a_node_that_wears_no_focus_ring_exports_without_the_browser_s_outline() {
    const RINGS: &str = r#"
export component Rings {
    state plain = true;
    view {
        Column {
            Button("Own") { focus_ring: false }
            Button("Bound") { focus_ring: plain; on click { plain = false; } }
        }
    }
}
"#;
    let page = export(RINGS, false).unwrap();
    assert!(page.index.contains("outline: none"), "{}", page.index);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.ring(e["), "{script}");
}

#[test]
fn an_access_key_exports_as_the_element_s_accesskey() {
    const KEYS: &str = r#"
export component Keys {
    state key = "o";
    view {
        Column {
            Button("Save") { access_key: "s" }
            Button("Open") { access_key: key; on click { key = "p"; } }
        }
    }
}
"#;
    let page = export(KEYS, false).unwrap();
    assert!(page.index.contains(r#"accesskey="s""#), "{}", page.index);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.key(e["), "{script}");
}

#[test]
fn a_radio_group_s_caption_options_export_their_circles() {
    const RADIOS: &str = r#"
export component Sizes {
    state size: U32 = 0;
    view {
        RadioGroup {
            bind selected <=> size;
            Text("Small")
            Row { Text("Composed") }
        }
    }
}
"#;
    let page = export(RADIOS, false).unwrap();
    assert_eq!(page.index.matches("vx-radio").count(), 1, "{}", page.index);
    assert!(
        page.styles.contains(".vx-radio[aria-checked"),
        "{}",
        page.styles
    );
}

#[test]
fn a_disclosure_exports_as_details_with_its_header_as_summary() {
    const PANEL: &str = r#"
export component Panel {
    state open = false;
    view {
        Disclosure {
            bind expanded <=> open;
            Text("More")
            Text("Body")
        }
    }
}
"#;
    let page = export(PANEL, false).unwrap();
    let index = &page.index;
    assert!(index.contains("<details"), "{index}");
    assert!(
        index.contains("<summary class=\"vx-text vx-chevron s1\">More</summary>"),
        "the header is the summary, which browsers expose as its button, drawing \
         the chevron that turns with it: {index}"
    );
    let styles = &page.styles;
    assert!(
        styles.contains("details[open] > .vx-chevron::before { transform: rotate(135deg); }"),
        "{styles}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.open(e["), "{script}");
    assert!(
        script.contains(", 9, (p) =>"),
        "the toggle reports `changed`: {script}"
    );
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let turn = "\nconst turned = () => getComputedStyle(document.querySelector('summary'), '::before').transform;\n\
        setTimeout(() => { document.body.append('shut=' + turned() + '.'); document.querySelector('summary').click(); }, 200);\n\
        setTimeout(() => document.body.append('turning=' + document.getAnimations().map((a) =>\n\
          a.transitionProperty + ' ' + a.effect.getKeyframes().map((k) => k.transform).join(' to ')).join() + '.'), 250);\n";
    let dom = run_in_chrome(
        &page,
        &(script.to_owned() + turn),
        "export-chevron",
        &[],
        chrome,
    );
    assert!(
        dom.contains("shut=matrix(0.707107, 0.707107, -0.707107, 0.707107, 0, 0).")
            && dom.contains("turning=transform rotate(45deg) to rotate(135deg)."),
        "the chevron points to the end edge while shut and turns down as it opens: {dom}"
    );
}

#[test]
fn a_modal_exports_as_a_dialog_layer_and_a_toast_as_a_hidden_status() {
    const PAGE: &str = r#"
export component Page {
    state shown = false;
    view {
        Column {
            Modal {
                bind open <=> shown;
                Text("Sure?")
            }
            Toast {
                open: false;
                Text("Saved")
            }
        }
    }
}
"#;
    let page = export(PAGE, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("<dialog class=\"vx-stack vx-layer"),
        "{index}"
    );
    assert!(
        !index.contains("role=\"dialog\""),
        "a dialog element is one: {index}"
    );
    assert!(index.contains("vx-layer vx-toast"), "{index}");
    assert!(index.contains("role=\"status\""), "{index}");
    assert!(
        index.contains(" hidden"),
        "a closed toast is hidden: {index}"
    );
    assert!(
        !index.contains("aria-live"),
        "a status speaks politely of itself: {index}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.layer(e["), "{script}");
    assert!(script.contains(", 10, (p) =>"), "{script}");
    assert!(page.styles.contains("dialog.vx-layer:not([open])"));
}

#[test]
fn a_list_in_lanes_is_a_css_grid() {
    let page = export(
        "component Board {
            view {
                VirtualList {
                    min_column: 96dp;
                    gap: 8dp;
                    column_gap: 4dp;
                    masonry: true;
                    for i in 0..20 key i {
                        Text(\"card\") { height: 40dp; sticky: i == 0 }
                    }
                }
            }
        }",
        false,
    )
    .unwrap();
    assert!(page.index.contains("vx-lanes"), "{}", page.index);
    let css = &page.styles;
    for decl in [
        "display: grid",
        "grid-template-columns: repeat(auto-fill, minmax(min(96px, 100%), 1fr))",
        "grid-template-rows: masonry",
        "row-gap: 8px",
        "column-gap: 4px",
    ] {
        assert!(css.contains(decl), "{decl} in {css}");
    }
    assert!(css.contains(".vx-lanes > [style*=\"sticky\"] { grid-column: 1 / -1; }"));
}

#[test]
fn a_page_eases_as_the_runtime_does() {
    let Ok(node) = which("node") else {
        eprintln!("no `node` on PATH: the page's curves are not run");
        return;
    };
    let program = format!(
        "{}\nconst out = [];\nfor (let tag = 0; tag < 34; tag++) for (let k = 0; k <= 40; k++) out.push(curveAt(tag, k / 40));\nconsole.log(out.join(' '));\nconsole.log(easing(2n));\nconsole.log(easing({{ t: 34, f: [0.2, 0, 1.4, 1] }}));\nfor (const f of [[300, 10, 1], [157.9, 25.1, 1], [100, 40, 2], [100, 0, 1]]) {{ const e = {{ t: 35, f }}; const [w, z] = springOf(e); const xs = [spring(e)[1]]; for (let i = 0; i <= 20; i++) xs.push(1 - springAt(w, z, i / 40)[0]); console.log(xs.join(' ')); }}\n",
        include_str!("../src/export/easing.js")
    );
    let out = std::process::Command::new(&node)
        .args(["-e", &program])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    let mut lines = said.lines();
    let values: Vec<f32> = lines
        .next()
        .unwrap()
        .split(' ')
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(values.len(), 34 * 41);
    for tag in 0..34u32 {
        let curve = viso_math::Easing::named(tag).unwrap();
        for k in 0..=40 {
            let js = values[tag as usize * 41 + k];
            let rust = curve.apply(k as f32 / 40.0);
            assert!(
                (js - rust).abs() < 1e-4,
                "{curve:?} at {k}/40: {js} vs {rust}"
            );
        }
    }
    assert!(lines.next().unwrap().starts_with("linear(0, "));
    assert_eq!(lines.next(), Some("cubic-bezier(0.2, 0, 1, 1)"));
    for (k, c, m) in [
        (300.0, 10.0, 1.0),
        (157.9, 25.1, 1.0),
        (100.0, 40.0, 2.0),
        (100.0, 0.0, 1.0),
    ] {
        let spring = viso_math::Spring::new(k, c, m);
        let js: Vec<f32> = lines
            .next()
            .unwrap()
            .split(' ')
            .map(|v| v.parse().unwrap())
            .collect();
        let settle = spring.settle_time(1e-3);
        assert!(
            (js[0] - settle).abs() < settle * 1e-3,
            "{spring:?}: {} vs {settle}",
            js[0]
        );
        for i in 0..=20 {
            let rust = spring.step(i as f32 / 40.0);
            assert!(
                (js[i + 1] - rust).abs() < 1e-4,
                "{spring:?} at {i}/40: {} vs {rust}",
                js[i + 1]
            );
        }
    }
}

#[test]
fn a_navigation_stack_exports_its_pages_folded_but_one_and_turns_them_with_web_animations() {
    const STACK: &str = r#"
export component Pages {
    state page: U32 = 0;
    view {
        NavigationStack {
            width: 200dp;
            height: 100dp;
            page_transition: PageTransition::shared_axis;
            bind current <=> page;
            Button("Next") { width: 40dp; height: 20dp; on click { page = 1; } }
            Text("Second")
        }
    }
}
"#;
    let page = export(STACK, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("role=\"navigation\"") || index.contains("<nav"),
        "{index}"
    );
    assert!(index.contains("data-turn=\"2\""), "{index}");

    assert!(
        page.styles
            .contains("[data-vx-away] { display: none !important; }")
    );
    let script = page.script.clone().unwrap();
    assert!(script.contains("P.page(e["), "{script}");
    assert!(
        script.contains(", 22, false, 14, (p) =>"),
        "the stack hears its back gesture: {script}"
    );
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    // Headless Chrome's virtual time does not run an animation's timeline,
    // so how the pages move is read off their animations.
    let turn = "\nsetTimeout(() => document.querySelector('button').click(), 50);\n\
        setTimeout(() => document.body.append('moved=' + document.getAnimations().map((a) =>\n\
          a.effect.getKeyframes().map((k) => k.transform + '/' + k.opacity).join(' ') + '|' + a.effect.getTiming().duration\n\
        ).join(';') + '.'), 100);\n";
    let still = run_in_chrome(&page, &script, "export-pages-still", &[], chrome.clone());
    assert_eq!(
        still.matches("data-vx-away").count(),
        1,
        "the page it does not show folds: {still}"
    );
    let dom = run_in_chrome(&page, &(script + turn), "export-pages", &[], chrome);
    assert!(
        dom.contains("moved=translateX(30px)/0 translateX(0px)/1|300;translateX(0px)/1 translateX(-30px)/0|300."),
        "the arriving page moves in, the leaving one out: {dom}"
    );
    assert!(
        !dom.contains("data-vx-away"),
        "both pages show as they move: {dom}"
    );
}

#[test]
fn a_hero_exports_its_tag_and_flies_between_the_pages() {
    const HEROES: &str = r#"
export component Pages {
    state page: U32 = 0;
    view {
        NavigationStack {
            width: 400dp;
            height: 300dp;
            page_transition: PageTransition::fade;
            bind current <=> page;
            Button("Open") { width: 40dp; height: 40dp; hero: "card"; on click { page = 1; } }
            Text("Card") { width: 80dp; height: 80dp; hero: "card" }
        }
    }
}
"#;
    let page = export(HEROES, false).unwrap();
    assert_eq!(
        page.index.matches("data-hero=\"card\"").count(),
        2,
        "{}",
        page.index
    );
    let script = page.script.clone().unwrap();
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let turn = "\nsetTimeout(() => document.querySelector('button').click(), 50);\n\
        setTimeout(() => document.body.append('flights=' + document.getAnimations().map((a) =>\n\
          a.effect.getKeyframes().map((k) => k.transform && k.transform.includes('scale(0.5)') ? 'fly' : k.opacity).join(' ')\n\
        ).join(';') + '.'), 100);\n";
    let dom = run_in_chrome(&page, &(script + turn), "export-heroes", &[], chrome);
    assert!(
        dom.contains(";fly ;0 0."),
        "the arriving hero flies from the other's place at its scale, which hides: {dom}"
    );
}

const CARRIED: &str = r#"
export component Board {
    state taken = "";
    state kind: U32 = 9;
    state enters = 0;
    state ended: Option<DropOperation> = None;
    view {
        Column {
            Stack {
                width: 100dp;
                height: 40dp;
                drag: "7";
                drag_kind: "card";
                on drop_end(e) { ended = e.operation; }
                DragPreview { Column { width: 100dp; height: 40dp; } }
            }
            Column {
                width: 300dp;
                height: 100dp;
                drop_accepts: ["text", "card"];
                drop_operation: DropOperation::copy;
                on drop_enter(e) { enters += 1; }
                on drop(e) { taken = e.text; kind = e.kind; }
            }
            Text("taken={taken} kind={kind} enters={enters} ended={ended == Some(DropOperation::copy)}")
        }
    }
}
"#;

#[test]
fn a_drag_source_and_a_drop_target_export_to_the_browser_s_drag_and_drop() {
    let page = export(CARRIED, false).unwrap();
    assert!(
        page.index.contains("data-drag=\"7\"")
            && page.index.contains("draggable=\"true\"")
            && page.index.contains("data-drag-kind=\"card\"")
            && page.index.contains("data-drop-accepts=\"text\ncard\"")
            && page.index.contains("data-drop-operation=\"1\"")
            && page.index.contains("data-drag-preview"),
        "{}",
        page.index
    );
    let script = page.script.clone().unwrap();
    assert!(script.contains("dragDrop();"), "{script}");
    let Some(chrome) = chrome() else {
        eprintln!("no Chrome: the page is not run");
        return;
    };
    let carry = "\nconst dt = new DataTransfer();\n\
        const on = (el, type) => { const r = el.getBoundingClientRect(); el.dispatchEvent(new DragEvent(type, { bubbles: true, cancelable: true, dataTransfer: dt, clientX: r.left + 10, clientY: r.top + 10 })); };\n\
        const source = () => document.querySelector('[data-drag]');\n\
        const zone = () => document.querySelector('[data-drop-accepts]');\n\
        setTimeout(() => { on(source(), 'dragstart'); on(zone(), 'dragenter'); on(zone(), 'dragover'); }, 50);\n\
        setTimeout(() => document.body.append('over=' + zone().hasAttribute('data-viso-drop-over') + ' lifted=' + source().hasAttribute('data-viso-dragging') + '.'), 200);\n\
        setTimeout(() => { on(zone(), 'drop'); on(source(), 'dragend'); }, 300);\n";
    let dom = run_in_chrome(&page, &(script + carry), "export-carried", &[], chrome);
    assert!(
        dom.contains("over=true lifted=true."),
        "the zone takes the card's kind: {dom}"
    );
    assert!(
        dom.contains("taken=7 kind=1 enters=1 ended=true"),
        "the drop hands over its text as its kind, and the source hears it copied: {dom}"
    );
}

const PROGRESS: &str = r##"
export component Progress {
    state done: Option<F32> = Option::None;
    view {
        Column {
            ProgressBar { value: Option::Some(30.0); max: 120.0; label: "Uploading"; tint: Option::Some(#ff0000); }
            ProgressRing { label: "Syncing"; }
            ProgressBar { value: done; on click { done = Option::Some(0.5); } semantics.label: "Saving"; }
            Spinner {}
        }
    }
}
"##;

#[test]
fn progress_exports_as_a_progressbar_whose_fill_is_its_fraction() {
    let page = export(PROGRESS, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains(
            "class=\"vx-progress s1\" aria-valuemin=\"0\" aria-valuemax=\"120\" role=\"progressbar\" aria-valuenow=\"30\" aria-label=\"Uploading\""
        ),
        "{index}"
    );
    assert!(
        index.contains("class=\"vx-ring s2\" aria-valuemin=\"0\" aria-valuemax=\"1\" role=\"progressbar\" aria-label=\"Syncing\""),
        "{index}"
    );
    assert!(
        index.contains("class=\"vx-spinner s2\" role=\"progressbar\""),
        "{index}"
    );
    let css = &page.styles;
    assert!(css.contains("--vx-at: 0.25;"), "{css}");
    assert!(css.contains("color: #ff0000;"), "{css}");
    assert!(
        css.contains(".vx-progress:not([aria-valuenow])::before"),
        "{css}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.progress("), "{script}");
    assert!(script.contains("\"value\")"), "{script}");
}

const LINK_LABEL: &str = r##"
export component Links {
    state to: String = "https://example.org/b";
    view {
        Column {
            LinkLabel("Docs") { url: "https://example.org/docs" }
            LinkLabel("Next") { url: to; on click { to = "https://example.org/c"; } }
        }
    }
}
"##;

#[test]
fn a_link_label_exports_as_an_anchor_to_its_url() {
    let page = export(LINK_LABEL, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("<a class=\"vx-text vx-link")
            && index.contains("href=\"https://example.org/docs\""),
        "{index}"
    );
    assert!(index.contains(">Docs</a>"), "{index}");
    assert!(page.styles.contains(".vx-link {"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.href("), "{script}");
}

const PLATES: &str = r##"
export component Plates {
    state unread: Option<I32> = Option::Some(5);
    state who: String = "Grace Hopper";
    view {
        Column {
            Badge { count: Option::Some(120); }
            Badge { label: "New"; }
            Badge { count: unread; on click { unread = Option::None; } }
            Avatar { name: "Ada Lovelace"; }
            Avatar { name: who; tint: Option::Some(#ff0000); on click { who = "Alan"; } }
        }
    }
}
"##;

#[test]
fn badges_and_avatars_export_as_pills_and_circles_their_script_keeps_current() {
    let page = export(PLATES, false).unwrap();
    let index = &page.index;
    assert!(index.contains(">99+</span>"), "{index}");
    assert!(index.contains("vx-badge vx-dot"), "{index}");
    assert!(index.contains("aria-label=\"New\""), "{index}");
    assert!(index.contains(">AL</span>"), "{index}");
    assert!(
        index.contains("role=\"img\" aria-label=\"Ada Lovelace\""),
        "{index}"
    );
    let ada = viso_schema::plate::avatar_rgb("Ada Lovelace");
    assert!(
        page.styles
            .contains(&format!("background-color: #{ada:06x}")),
        "{}",
        page.styles
    );
    assert!(page.styles.contains(".vx-badge {"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.badge("), "{script}");
    assert!(script.contains("P.avatar("), "{script}");
}

const CHIPS: &str = r##"
export component Chips {
    state veggie: Bool = true;
    state tags: I32 = 2;
    state editing: Bool = false;
    view {
        Row {
            Chip { label: "Vegetarian"; bind selected <=> veggie; }
            Chip { label: "rust"; removable: true; on removed { tags = tags - 1; } }
            Chip { label: "Help"; on click { editing = !editing; } }
            Chip { label: "draft"; removable: editing; }
        }
    }
}
"##;

#[test]
fn chips_export_as_buttons_pressed_while_selected_with_a_cross_when_removable() {
    let page = export(CHIPS, false).unwrap();
    let index = &page.index;
    // The script presses the bound chip as it mounts.
    assert!(
        index.contains("type=\"button\" aria-pressed=\"false\" data-v>Vegetarian<"),
        "{index}"
    );
    assert!(!index.contains("role=\"checkbox\""), "{index}");
    assert!(
        index.contains("data-removable=\"true\" data-v>rust</button>"),
        "{index}"
    );
    assert!(index.contains(">Help</button>"), "{index}");
    assert!(page.styles.contains(".vx-chip {"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.pressed("), "{script}");
    assert!(script.contains("P.data("), "{script}");
}

const STEPPERS: &str = r##"
export component Steppers {
    state guests: F64 = 2.0;
    view {
        Column {
            Stepper { value: 0.0; min: 0.0; max: 3.0; label: "Fixed"; }
            Stepper { bind value <=> guests; min: 1.0; max: 12.0; step: 1.0; wrap: true; }
        }
    }
}
"##;

#[test]
fn steppers_export_as_spin_buttons_their_script_steps() {
    let page = export(STEPPERS, false).unwrap();
    let index = &page.index;
    assert!(
        index.contains("role=\"spinbutton\" tabindex=\"0\" aria-valuemin=\"0\" aria-valuemax=\"3\" aria-valuenow=\"0\""),
        "{index}"
    );
    assert!(
        index.contains("class=\"vx-step vx-spent\" data-by=\"-1\">\u{2212}</button>"),
        "the minus of one at its least is spent: {index}"
    );
    assert!(index.contains("aria-label=\"Fixed\""), "{index}");
    assert!(index.contains("data-wrap=\"true\""), "{index}");
    assert!(page.styles.contains(".vx-stepper {"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.stepper("), "{script}");
}

const SEGMENTED: &str = r##"
export component Ranges {
    state range: U32 = 1;
    view {
        Column {
            SegmentedControl {
                bind selected <=> range;
                tint: Option::Some(#3366ee);
                Text("Day")
                Text("Week")
            }
        }
    }
}
"##;

#[test]
fn a_segmented_control_exports_as_a_radio_group_its_script_slides_a_thumb_in() {
    let page = export(SEGMENTED, false).unwrap();
    let index = &page.index;
    assert!(index.contains("vx-segmented"), "{index}");
    assert!(index.contains("role=\"radiogroup\""), "{index}");
    assert_eq!(index.matches("vx-segment ").count(), 2, "{index}");
    assert!(!index.contains("vx-radio"), "{index}");
    assert!(page.styles.contains("--vx-ink:"), "{}", page.styles);
    assert!(
        page.styles.contains(".vx-segmented::before {"),
        "{}",
        page.styles
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.selected("), "{script}");
}

const SLIDERS: &str = r##"
export component Mixer {
    state level: F32 = 0.25;
    view {
        Column {
            Slider {
                bind value <=> level;
                axis: column;
                marks: true;
                step: Option::Some(0.25);
                tint: Option::Some(#3366ee);
            }
            Knob { min: 0.0; max: 10.0; value: 7.5; }
        }
    }
}
"##;

#[test]
fn a_slider_runs_up_tinted_and_a_knob_turns_to_its_fraction() {
    let page = export(SLIDERS, false).unwrap();
    let index = &page.index;
    assert_eq!(index.matches("type=\"range\"").count(), 2, "{index}");
    assert!(index.contains("vx-vertical"), "{index}");
    assert!(index.contains("vx-knob"), "{index}");
    assert!(page.styles.contains("--vx-ink:"), "{}", page.styles);
    assert!(page.styles.contains("--vx-at: 0.75"), "{}", page.styles);
    assert!(page.styles.contains(".vx-knob {"), "{}", page.styles);
}

const PICKERS: &str = r##"
export component Paint {
    state fill: Color = #3366ee80;
    state clear = true;
    view {
        Column {
            ColorPicker { label: "Fill"; bind value <=> fill; alpha: clear; }
            ColorPicker { value: #ff0000; alpha: false; }
        }
    }
}
"##;

#[test]
fn a_colour_picker_exports_as_a_colour_input_and_an_opacity_range() {
    let page = export(PICKERS, false).unwrap();
    let index = &page.index;
    assert_eq!(index.matches("type=\"color\"").count(), 2, "{index}");
    assert!(index.contains("value=\"#ff0000\""), "{index}");
    assert!(index.contains("role=\"group\""), "{index}");
    assert!(index.contains("aria-label=\"Fill\""), "{index}");
    // The second takes no opacity: its range is hidden.
    assert_eq!(index.matches("vx-color-alpha").count(), 2, "{index}");
    assert_eq!(index.matches(" hidden>").count(), 1, "{index}");
    assert!(page.styles.contains(".vx-color {"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.picker("), "{script}");
}

const LISTED: &str = r##"
export component Find {
    state fruit = "Cherry";
    state query = "";
    state hits: List<String> = [];
    view {
        Column {
            ComboBox {
                label: "Fruit";
                options: ["Apple", "Banana", "Cherry"];
                bind value <=> fruit;
            }
            ComboBox { label: "City"; free_text: true; placeholder: "Where"; options: ["Bern"]; }
            SearchField {
                label: "Find";
                debounce: 250ms;
                suggestions: hits;
                bind value <=> query;
                on submitted { query = ""; }
            }
        }
    }
}
"##;

#[test]
fn combo_boxes_and_search_fields_export_as_inputs_over_a_datalist() {
    let page = export(LISTED, false).unwrap();
    let index = &page.index;
    assert_eq!(index.matches("role=\"combobox\"").count(), 2, "{index}");
    assert_eq!(index.matches("type=\"search\"").count(), 1, "{index}");
    assert_eq!(index.matches("<datalist").count(), 3, "{index}");
    assert!(index.contains("value=\"Cherry\""), "{index}");
    assert!(index.contains("<option value=\"Banana\">"), "{index}");
    assert!(index.contains("aria-label=\"Fruit\""), "{index}");
    assert!(index.contains("placeholder=\"Where\""), "{index}");
    assert!(index.contains("placeholder=\"Search\""), "{index}");
    assert!(index.contains("data-free=\"true\""), "{index}");
    assert!(index.contains("data-debounce=\"250\""), "{index}");
    assert!(page.styles.contains(".vx-combo"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.listed("), "{script}");
}

#[test]
fn a_tiled_image_exports_as_a_repeating_background_or_mask() {
    let source = r#"
export component Patterns {
    view {
        Column {
            Image { url: "https://example.org/a.png"; fit: tile; width: 64dp; height: 64dp; }
            Image { url: "https://example.org/b.png"; fit: tile; tint: #ff0000; width: 64dp; height: 64dp; }
        }
    }
}
"#;
    let page = export_in(source, None, false).unwrap();
    assert!(!page.index.contains("<img"), "{}", page.index);
    assert!(page.index.contains("vx-pattern"), "{}", page.index);
    let styles = &page.styles;
    for want in [
        "background-image: url(\"https://example.org/a.png\")",
        "mask-image: url(\"https://example.org/b.png\")",
        "mask-repeat: repeat",
        "mask-size: auto",
        ".vx-pattern { display: block; background: 0 0 repeat; }",
    ] {
        assert!(styles.contains(want), "{want}: {styles}");
    }
}

#[test]
fn a_camera_view_exports_as_a_video_its_script_gives_the_camera_stream() {
    let source = r#"
export component Preview {
    state on = true;
    state front = true;
    view {
        Column {
            CameraView { facing: CameraFacing::back; mirror: false; on playing_changed(p) { on = p; } }
            CameraView { device: "cam-2"; bind playing <=> on; fit: fill; }
            CameraView { facing: if front { CameraFacing::front } else { CameraFacing::back }; }
        }
    }
}
"#;
    let page = export_in(source, None, false).unwrap();
    let index = &page.index;
    for want in [
        "<video",
        "vx-video",
        "data-vx-facing=\"environment\"",
        "data-vx-mirror=\"0\"",
        "data-vx-device=\"cam-2\"",
        "playsinline",
        " muted",
    ] {
        assert!(index.contains(want), "{want}: {index}");
    }
    assert!(page.styles.contains("object-fit: cover"), "{}", page.styles);
    assert!(page.styles.contains("object-fit: fill"), "{}", page.styles);
    let script = page.script.as_deref().unwrap();
    for want in [
        "camera(",
        "\"capture\")",
        "\"facing\")",
        "getUserMedia",
        ", 40, ",
    ] {
        assert!(script.contains(want), "{want}: {script}");
    }
    assert!(
        export_in(source, None, true).is_err(),
        "a static page captures from no camera"
    );
}

#[test]
fn a_video_exports_as_a_video_element_its_script_steers_and_hears() {
    let dir = std::env::temp_dir().join(format!("viso-export-video-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("clips")).unwrap();
    std::fs::write(
        dir.join("clips/intro.mp4"),
        include_bytes!("../../media/tests/fixtures/three.mp4"),
    )
    .unwrap();
    let source = r#"
export component Clip {
    state playing = false;
    state at = 0ms;
    state full = false;
    view {
        Column {
            Video(asset("clips/intro.mp4")) {
                width: 64dp; height: 48dp;
                controls: true;
                bind fullscreen <=> full;
                fit: cover;
                looping: true; muted: true; volume: 0.5;
                bind playing <=> playing;
                on progress(p) { at = p.position; }
            }
            Video { url: "https://example.com/live.m3u8"; playing: true; muted: true; }
        }
    }
}
"#;
    let page = export_in(source, Some(&dir), false).unwrap();
    let static_only = export_in(source, Some(&dir), true);
    std::fs::remove_dir_all(&dir).unwrap();
    let index = &page.index;
    for want in [
        "<video",
        "vx-video",
        "src=\"assets/clips/intro.mp4\"",
        "src=\"https://example.com/live.m3u8\"",
        "playsinline",
        " loop",
        " muted",
        " autoplay",
        " controls",
    ] {
        assert!(index.contains(want), "{want}: {index}");
    }
    assert!(page.styles.contains("object-fit: cover"), "{}", page.styles);
    assert_eq!(page.assets, ["clips/intro.mp4"]);
    let script = page.script.as_deref().unwrap();
    for want in [
        "\"volume\")",
        "\"autoplay\")",
        "\"fullscreen\")",
        ", 38, ",
        ", 40, ",
        ", 41, ",
    ] {
        assert!(script.contains(want), "{want}: {script}");
    }
    assert!(static_only.is_err(), "a static page sets no volume");
}

#[test]
fn an_image_exports_its_url_as_its_src() {
    let source = r#"
export component Photo {
    state at = "https://example.org/b.png";
    view {
        Column {
            Image { url: "https://example.org/a.png"; width: 32dp; height: 32dp; }
            Image { url: at; width: 32dp; height: 32dp; on click { at = "https://example.org/c.png"; } }
        }
    }
}
"#;
    let page = export(source, false).unwrap();
    assert!(
        page.index.contains("src=\"https://example.org/a.png\""),
        "{}",
        page.index
    );
    assert!(page.assets.is_empty());
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("https://example.org/c.png"), "{script}");
}

#[test]
fn a_wrap_is_a_wrapping_flex_box() {
    let page = export(
        r#"
export component Tags {
    view {
        Wrap {
            gap: 8dp;
            row_gap: 4dp;
            Text("a") { width: 40dp; height: 20dp }
            Text("b") { width: fill; height: 20dp }
        }
    }
}
"#,
        true,
    )
    .unwrap();
    let css = &page.styles;
    assert!(
        css.contains("flex-wrap: wrap; row-gap: 4px; column-gap: 8px;"),
        "{css}"
    );
    assert!(css.contains("flex: none; width: 40px;"), "{css}");
    assert!(
        css.contains("flex: none; width: 100%;"),
        "a fill takes a line: {css}"
    );
}
