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
            semantics.role: Role::region;
            semantics.label: "Welcome";
            Text { width: 200dp; height: 20dp; text: "Hello <world>"; color: theme.colors.primary; }
            Row {
                justify: Justify::center;
                align: Align::center;
                Button { width: Sizing::fill { weight: 2.0 }; height: 32dp; text: "Go"; }
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
                transform_origin: Alignment2D::top_start;
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
            Button {
                width: 40dp; height: 20dp; text: "Shake";
                on click {
                    let shake = card.animate(Animate::rotation(10deg), Transition { duration: 80ms });
                    if shake.is_running() { shake.finish(); }
                }
            }
            node card: Text {
                width: 40dp; height: 20dp; text: "card";
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
            Button {
                width: 40dp; height: 20dp; text: "Pulse";
                on click {
                    let track = Keyframes {
                        stops: [
                            Keyframe { at: 0.5f32, value: Animate::scale(2.0f32), easing: Easing::ease_out },
                            Keyframe { at: 1.0f32, value: Animate::scale(1.0f32) },
                        ],
                        duration: 100ms,
                        repeat: Repeat::times(3),
                        direction: PlayDirection::alternate,
                    };
                    card.play(track);
                }
            }
            Button {
                width: 40dp; height: 20dp; text: "Spin";
                on click {
                    let track = Keyframes {
                        stops: [Keyframe { at: 1.0f32, value: Animate::opacity(0.0f32) }],
                        repeat: Repeat::forever,
                    };
                    let spin = card.play(track);
                    if spin.is_running() { spin.finish(); }
                }
            }
            node card: Text {
                width: 40dp; height: 20dp; text: "card";
                on animation_end(e) {
                    scaled = match e.target { Animate::scale(_) => e.finished, _ => scaled };
                    faded = match e.target { Animate::opacity(_) => e.finished, _ => faded };
                }
            }
            Text { text: format("scaled={}", scaled); }
            Text { text: format("faded={}", faded); }
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
            Button { width: 40dp; height: 20dp; text: "Off"; enabled: false; }
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
            Column { width: 40dp; height: 20dp; cursor: Cursor::pointer; }
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
                font_style: FontStyle::italic;
                font_weight: FontWeight::bold;
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
            Text { text: "always"; selectable: true; }
            Text { text: "sometimes"; selectable: pick; on click { pick = !pick; } }
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
            CheckBox { width: 20dp; height: 20dp; label: "Remember"; }
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
            TextInput { width: 100dp; height: 20dp; input: InputKind::decimal; max_length: 8; }
            TextInput { width: 100dp; height: 20dp; input: InputKind::email; }
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
            Text { width: 100dp; height: 20dp; text: format("{}", count); }
            Button {
                width: 100dp;
                height: 32dp;
                text: "Add";
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
            (Need::Dynamic, "format(\"{}\", count)"),
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
        script.contains("on(e[1], 0, false, 4, (p) => f"),
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
                Text { width: 100dp; height: 20dp; text: "Open"; }
            } else {
                Text { width: 100dp; height: 20dp; text: "Closed"; }
            }
            for item in items key item {
                Text { width: 100dp; height: 20dp; text: item; }
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
                        Column { width: 200dp; height: 20dp; Button { text: \"go\"; } }
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
    let source = "import viso::i18n::{tr};\nexport component Inbox {\n    view {\n        Column {\n            Text { text: tr(\"inbox.title\"); }\n            Text { text: tr(\"inbox.unread\", count: 1); }\n        }\n    }\n}\n";
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
            "{}\n{messages}\nglobalThis.navigator = {{ languages: {languages} }};\nsettle(T);\n\
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
    ] {
        assert!(dom.contains(shown), "{shown} in {dom}");
    }
    for gone in [">item=a<", ">x="] {
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
            Button { text: "Own"; focus_ring: false; }
            Button { text: "Bound"; focus_ring: plain; on click { plain = false; } }
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
fn a_disclosure_exports_as_details_with_its_header_as_summary() {
    const PANEL: &str = r#"
export component Panel {
    state open = false;
    view {
        Disclosure {
            bind expanded <=> open;
            Text { text: "More"; }
            Text { text: "Body"; }
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
                Text { text: "Sure?"; }
            }
            Toast {
                open: false;
                Text { text: "Saved"; }
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
                        Text { height: 40dp; sticky: i == 0; text: \"card\"; }
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
            Button { text: "Next"; width: 40dp; height: 20dp; on click { page = 1; } }
            Text { text: "Second"; }
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
            Button { text: "Open"; width: 40dp; height: 40dp; hero: "card"; on click { page = 1; } }
            Text { text: "Card"; width: 80dp; height: 80dp; hero: "card"; }
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
