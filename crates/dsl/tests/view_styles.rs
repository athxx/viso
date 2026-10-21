//! Styles: a node's `styles` apply each style after its bases, a later binding
//! over an earlier one and the node's own properties over them all; a `when`
//! switches a look, a transform, a text's color or a layout value while its
//! selector holds — hover, pressed, focus, selection or the node's own `enabled` — on
//! the hot-reloaded and the packaged view and inside a region; the drag and
//! long-press gestures that win a press from its click. And the
//! checks: a target the file names (`E2001`), bases of the file for the same
//! target (`E2001`, `E2103`) on no cycle (`E2003`), styleable properties
//! (`E3101`) bound once per block (`E3102`) to pure values (`E2502`) of their
//! type (`E2103`), selectors the target supports (`E2001`, `E2103`), a
//! `styles` list of styles for the node (`E2103`), and `@styleable`/`@selector` on the members they fit (`E3710`).

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{
    NodeId, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rgba, Srgb, Vec2,
    deliver_long_presses,
};

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The diagnostic codes compiling `items` beside a component whose view is
/// `view` reports.
fn codes(items: &str, view: &str) -> Vec<&'static str> {
    let source = format!(
        "{items}\nexport component C {{\n    state on = false;\n    view {{ {view} }}\n}}\n"
    );
    let compiled = compile_file(&source, &origin());
    let diagnostics: Vec<_> = compiled.errors().collect();
    eprintln!("{diagnostics:#?}");
    diagnostics.iter().map(|d| d.code).collect()
}

const PILL: &str = "style Pill for Text { opacity: 0.5f32; when hover { background: #ff0000; } }";

#[test]
fn a_style_binds_styleable_properties_of_its_target() {
    assert_eq!(codes(PILL, "Text { styles: [Pill]; }"), CLEAN);
    assert_eq!(codes("style S for Nowhere {}", "Text {}"), ["E2001"]);
    assert_eq!(
        codes("style S for Text { text: \"a\"; }", "Text {}"),
        ["E3101"],
        "`text` is content, not look"
    );
    assert_eq!(codes("style S for Text { glow: 1; }", "Text {}"), ["E3101"]);
    assert_eq!(
        codes(
            "style S for Text { opacity: 1.0f32; opacity: 0.5f32; }",
            "Text {}"
        ),
        ["E3102"]
    );
    assert_eq!(
        codes(
            "style S for Text { opacity: 1.0f32; when hover { opacity: 0.5f32; } }",
            "Text {}"
        ),
        CLEAN,
        "a `when` block binds again"
    );
    assert_eq!(
        codes("style S for Text { opacity: \"half\"; }", "Text {}"),
        ["E2103"]
    );
    assert_eq!(
        codes(
            "action poke() -> F32 { 1.0f32 }\nstyle S for Text { opacity: poke(); }",
            "Text {}"
        ),
        ["E2502"]
    );
}

#[test]
fn bases_are_styles_of_the_target_on_no_cycle() {
    let base = "style A for Text { opacity: 0.5f32; }";
    assert_eq!(
        codes(&format!("{base}\nstyle B for Text: A {{}}"), "Text {}"),
        CLEAN
    );
    assert_eq!(codes("style B for Text: Nowhere {}", "Text {}"), ["E2001"]);
    assert_eq!(
        codes("style A for Column {}\nstyle B for Text: A {}", "Text {}"),
        ["E2103"]
    );
    assert_eq!(
        codes("const N: I64 = 1;\nstyle B for Text: N {}", "Text {}"),
        ["E2103"]
    );
    assert_eq!(
        codes("style A for Text: B {}\nstyle B for Text: A {}", "Text {}"),
        ["E2003", "E2003"]
    );
}

#[test]
fn a_selector_is_one_the_target_supports() {
    let style =
        |selector: &str| format!("style S for Text {{ when {selector} {{ opacity: 0.5f32; }} }}");
    for selector in [
        "hover",
        "pressed",
        "focused",
        "focus_visible",
        "disabled",
        "hover && !pressed",
        "(hover || focused) && !disabled",
    ] {
        assert_eq!(codes(&style(selector), "Text {}"), CLEAN, "{selector}");
    }
    assert_eq!(codes(&style("checked"), "Text {}"), ["E2001"]);
    assert_eq!(
        codes(
            "style S for CheckBox { when checked { opacity: 0.5f32; } }",
            "Text {}"
        ),
        CLEAN
    );
    assert_eq!(codes(&style("dragging"), "Text {}"), CLEAN);
    assert_eq!(codes(&style("expanded"), "Text {}"), ["E2001"]);
    assert_eq!(codes(&style("hovering"), "Text {}"), ["E2001"]);
    assert_eq!(codes(&style("on == true"), "Text {}"), ["E2103"]);
}

#[test]
fn a_node_lists_styles_for_its_type() {
    assert_eq!(codes(PILL, "Column { styles: [Pill]; }"), ["E2103"]);
    assert_eq!(codes(PILL, "Text { styles: Pill; }"), ["E2103"]);
    assert_eq!(
        codes(
            &format!("{PILL}\nconst N: I64 = 1;"),
            "Text { styles: [N]; }"
        ),
        ["E2103"]
    );
    assert_eq!(codes(PILL, "Text { styles: [Pill, Pill]; }"), CLEAN);
}

#[test]
fn styleable_and_selector_mark_the_members_they_fit() {
    let component = |members: &str| {
        let source = format!("export component K {{\n{members}\n    view {{ Text {{}} }}\n}}\n");
        let compiled = compile_file(&source, &origin());
        let codes: Vec<_> = compiled.errors().map(|d| d.code).collect();
        codes
    };
    assert_eq!(
        component(
            "@styleable\ninput tint: Color = #000000;\n@selector\ninput chosen: Bool = false;\n\
             @selector\nstate open = false;"
        ),
        CLEAN
    );
    assert_eq!(component("@styleable\nstate tint = 1;"), ["E3710"]);
    assert_eq!(component("@selector\ninput count: I64 = 0;"), ["E3710"]);
    assert_eq!(
        component("@selector\ninput hover: Bool = false;"),
        ["E3710"]
    );
    let styled = "export component K {\n@styleable\ninput tint: Color = #000000;\n\
                  input label: String = \"\";\n@selector\ninput chosen: Bool = false;\n\
                  view { Text { background: tint; } }\n}\n";
    let check = |style: &str| {
        let source = format!("{styled}{style}\n");
        let compiled = compile_file(&source, &origin());
        let codes: Vec<_> = compiled.errors().map(|d| d.code).collect();
        codes
    };
    assert_eq!(
        check("style S for K { tint: #ff0000; when chosen { tint: #00ff00; } }"),
        CLEAN
    );
    assert_eq!(check("style S for K { label: \"a\"; }"), ["E3101"]);
    assert_eq!(
        check("style S for K { when open { tint: #00ff00; } }"),
        ["E2001"]
    );
}

/// A view whose first box applies `Dim` (`Pill` and a disabled look), its
/// second `Pill` under its own background and toggles the first's `enabled`,
/// and whose third shows under an `if`.
const SOURCE: &str = r#"
style Pill for Text {
    background: #102030;
    opacity: 0.5f32;
    when hover { background: #ff0000; }
    when pressed { background: #00ff00; }
}
style Dim for Text: Pill {
    when disabled { opacity: 0.25f32; }
}
export component Probe {
    state off = false;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; styles: [Dim]; enabled: !off; }
            Text {
                width: 20dp;
                height: 20dp;
                styles: [Pill];
                background: #0000ff;
                on click { off = !off; }
            }
            if !off {
                Text { width: 20dp; height: 20dp; styles: [Dim]; }
            }
        }
    }
}
"#;

fn rgba(hex: u32) -> Rgba {
    Srgb::from_rgba32(hex).into_linear_straight()
}

const REST: u32 = 0x1020_30ff;
const HOVER: u32 = 0xff00_00ff;
const PRESSED: u32 = 0x00ff_00ff;
const OWN: u32 = 0x0000_ffff;

/// The root's children.
fn children(rt: &Rt) -> Vec<NodeId> {
    let root = rt.root.expect("mounted");
    let arena = rt.store.arena();
    let mut out = Vec::new();
    for node in arena.children(root) {
        out.push(node);
    }
    out
}

/// The fill and opacity of the root's `n`th child.
fn look(rt: &Rt, n: usize) -> (Rgba, f32) {
    let node = children(rt)[n];
    (rt.store.style(node).fill, rt.store.opacity(node))
}

/// A pointer sample at the middle of the `n`th box, then a frame.
fn pointer(rt: &mut Rt, n: usize, phase: PointerPhase) {
    sample(rt, 5.0, 5.0 + 20.0 * n as f32, phase);
}

/// A primary pointer sample at `(x, y)`, then a frame.
fn sample(rt: &mut Rt, x: f32, y: f32, phase: PointerPhase) {
    let root = rt.root.expect("mounted");
    let event = PointerEvent {
        x,
        y,
        phase,
        buttons: PointerButtons::PRIMARY,
        modifiers: Default::default(),
    };
    PointerRouter::route(
        &mut rt.store,
        &mut rt.states,
        &rt.bindings,
        root,
        event,
        &mut Vec::new(),
    );
    rt.frame();
}

/// The look of `rt` across hover, press, release, leave and a disable.
fn walk(rt: &mut Rt) {
    assert_eq!(look(rt, 0), (rgba(REST), 0.5), "at rest");
    assert_eq!(look(rt, 1), (rgba(OWN), 0.5), "its own background wins");
    assert_eq!(look(rt, 2), (rgba(REST), 0.5), "in a region");

    pointer(rt, 0, PointerPhase::Move);
    assert_eq!(look(rt, 0).0, rgba(HOVER));
    assert_eq!(look(rt, 2).0, rgba(REST), "only the hovered node");
    pointer(rt, 0, PointerPhase::Down);
    assert_eq!(look(rt, 0).0, rgba(PRESSED), "the later `when` wins");
    pointer(rt, 0, PointerPhase::Up);
    assert_eq!(look(rt, 0).0, rgba(HOVER));
    pointer(rt, 2, PointerPhase::Move);
    assert_eq!(look(rt, 0).0, rgba(REST));
    assert_eq!(look(rt, 2).0, rgba(HOVER), "region content follows hover");

    pointer(rt, 1, PointerPhase::Move);
    assert_eq!(look(rt, 1).0, rgba(OWN), "its own background wins on hover");
    pointer(rt, 0, PointerPhase::Down);
    pointer(rt, 1, PointerPhase::Move);
    assert_eq!(look(rt, 0).0, rgba(REST), "the pointer left the press");
    pointer(rt, 1, PointerPhase::Up);
    assert_eq!(
        look(rt, 0).1,
        0.5,
        "a release on a node the press did not begin on clicks nothing"
    );
    rt.click(1);
    assert_eq!(look(rt, 0), (rgba(REST), 0.25), "disabled");
    assert_eq!(children(rt).len(), 2);
    rt.click(1);
    assert_eq!(look(rt, 0), (rgba(REST), 0.5));
    assert_eq!(look(rt, 2), (rgba(REST), 0.5), "remounted");
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_when_switches_the_look_while_its_selector_holds() {
    let mut rt = Rt::mount(SOURCE);
    walk(&mut rt);
}

#[test]
fn the_release_package_runs_the_same() {
    let mut rt = Rt::packaged(SOURCE);
    walk(&mut rt);
}

#[test]
fn a_hot_reload_restyles_the_kept_nodes() {
    let mut rt = Rt::mount(SOURCE);
    rt.reload(&SOURCE.replace("background: #ff0000;", "background: #00ffff;"));
    pointer(&mut rt, 0, PointerPhase::Move);
    assert_eq!(look(&rt, 0).0, rgba(0x00ff_ffff));
    rt.reload(&SOURCE.replace("styles: [Dim]; enabled: !off; ", ""));
    assert_eq!(look(&rt, 0).1, 1.0, "no style, no look");
}

/// Styles whose `when` switches a layout value, a translate and a text's
/// color, beside a color read from the theme.
const SWITCHES: &str = r#"
style Pad for Column {
    padding: EdgeInsets::all(0dp);
    when hover {
        padding: EdgeInsets::all(4dp);
        translate: Offset { x: 0dp, y: 2dp };
    }
}
style Ink for Text {
    color: theme.colors.primary;
    when hover { color: #ff0000; }
}
export component Probe {
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Column {
                width: 20dp;
                height: 20dp;
                styles: [Pad];
                Text { width: 10dp; height: 10dp; text: "a"; styles: [Ink]; }
            }
        }
    }
}
"#;

fn switches(rt: &mut Rt) {
    let pad = children(rt)[0];
    let text = rt.children(pad)[0];
    let ink = |rt: &Rt| rt.store.text_request(text).map(|r| r.color);
    let rest = ink(rt);
    assert_eq!(
        rest,
        Some(rgba(0x3b6c_f0ff)),
        "a style's theme color reaches the text"
    );
    assert_eq!(rt.store.translate(pad).y, 0.0);
    assert_eq!(rt.store.bounds(text).x, rt.store.bounds(pad).x);
    pointer(rt, 0, PointerPhase::Move);
    assert_eq!(rt.store.translate(pad).y, 2.0, "a `when` moves it");
    assert_eq!(ink(rt), Some(rgba(0xff00_00ff)), "and colors the text");
    assert_eq!(
        rt.store.bounds(text).x,
        rt.store.bounds(pad).x + 4.0,
        "and pads it"
    );
    pointer(rt, 2, PointerPhase::Move);
    assert_eq!(rt.store.translate(pad).y, 0.0);
    assert_eq!(rt.store.bounds(text).x, rt.store.bounds(pad).x);
    assert_eq!(ink(rt), rest);
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_when_switches_layout_transform_and_text_values() {
    switches(&mut Rt::mount(SWITCHES));
    switches(&mut Rt::packaged(SWITCHES));
}

#[test]
fn a_switched_layout_value_needs_a_base() {
    let codes = codes(
        "style S for Text { when hover { padding: EdgeInsets::all(4dp); } }",
        "Text { styles: [S]; }",
    );
    assert_eq!(codes, ["E3711"]);
}

/// A chip whose tint a style gives, open once clicked and wide past two
/// clicks.
const CHIP: &str = r#"
component Chip {
    @styleable
    input tint: Color = #000000;
    @selector
    state open = false;
    state clicks = 0;
    @selector
    computed wide: Bool = clicks > 1;
    view {
        Text {
            width: 20dp;
            height: 20dp;
            background: tint;
            on click { open = !open; clicks += 1; }
        }
    }
}
style Base for Chip { tint: #102030; }
style Lit for Chip: Base {
    when open { tint: #ff0000; }
    when open && wide { tint: #00ff00; }
}
export component C {
    state rows: List<I64> = [1];
    view {
        Column {
            Chip { styles: [Lit]; }
            Chip { styles: [Lit]; tint: #0000ff; }
            for row in rows key row { Chip { styles: [Lit]; } }
        }
    }
}
"#;

#[test]
fn a_style_on_a_component_node_gives_its_styleable_inputs_by_its_selectors() {
    for mut rt in [Rt::mount(CHIP), Rt::packaged(CHIP)] {
        assert_eq!(look(&rt, 0).0, rgba(REST), "the base");
        assert_eq!(look(&rt, 1).0, rgba(OWN), "the node's own binding wins");
        assert_eq!(look(&rt, 2).0, rgba(REST), "in a region too");
        rt.click(0);
        assert_eq!(look(&rt, 0).0, rgba(HOVER), "`when open`");
        rt.click(0);
        assert_eq!(look(&rt, 0).0, rgba(REST), "closed again");
        rt.click(0);
        assert_eq!(
            look(&rt, 0).0,
            rgba(PRESSED),
            "`when open && wide`, the later arm"
        );
        rt.click(2);
        assert_eq!(look(&rt, 2).0, rgba(HOVER), "the region's own instance");
        assert_eq!(look(&rt, 1).0, rgba(OWN));
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_component_style_does_not_switch_an_input_by_an_interaction_selector_yet() {
    let source = CHIP.replace("when open { tint", "when hover { tint");
    let compiled = compile_file(&source, &origin());
    let errors: Vec<_> = compiled
        .errors()
        .map(|d| {
            let at = d.primary.start().to_u32() as usize..d.primary.end().to_u32() as usize;
            (d.code, source[at].to_owned())
        })
        .collect();
    assert_eq!(
        errors.len(),
        2,
        "the chips that take `tint` from it: {errors:?}"
    );
    assert!(
        errors
            .iter()
            .all(|(code, at)| *code == "E3711" && at == "hover"),
        "{errors:?}"
    );
}

#[test]
fn a_drag_past_the_slop_runs_its_handlers_shows_dragging_and_clicks_nothing() {
    let source = r#"
style Grab for Text {
    opacity: 0.5f32;
    when dragging { opacity: 1.0f32; }
}
export component Probe {
    state shift = Offset { x: 0dp, y: 0dp };
    state starts = 0;
    state moves = 0;
    state ends = 0;
    state clicks = 0;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text {
                width: 40dp;
                height: 20dp;
                styles: [Grab];
                translate: shift;
                on drag_start(event) { starts += 1; }
                on drag_move(event) { moves += 1; shift = event.total; }
                on drag_end(event) { ends += 1; }
                on click { clicks += 1; }
            }
        }
    }
}
"#;
    let mut rt = Rt::mount(source);
    let node = children(&rt)[0];
    let counts = |rt: &Rt| {
        rt.ints(["starts", "moves", "ends", "clicks"])
            .map(Option::unwrap)
    };
    sample(&mut rt, 5.0, 5.0, PointerPhase::Down);
    sample(&mut rt, 7.0, 5.0, PointerPhase::Move);
    assert_eq!(counts(&rt), [0, 0, 0, 0], "inside the slop");
    assert_eq!(look(&rt, 0).1, 0.5);
    sample(&mut rt, 15.0, 5.0, PointerPhase::Move);
    assert_eq!(counts(&rt), [1, 0, 0, 0]);
    assert_eq!(look(&rt, 0).1, 1.0, "dragging");
    sample(&mut rt, 25.0, 8.0, PointerPhase::Move);
    assert_eq!(counts(&rt), [1, 1, 0, 0]);
    assert_eq!(rt.store.translate(node), Vec2 { x: 20.0, y: 3.0 });
    sample(&mut rt, 25.0, 8.0, PointerPhase::Up);
    assert_eq!(
        counts(&rt),
        [1, 1, 1, 0],
        "the lift ends the drag, not a click"
    );
    assert_eq!(look(&rt, 0).1, 0.5);
    rt.click_at(30.0, 10.0);
    assert_eq!(counts(&rt), [1, 1, 1, 1], "a press that stays put clicks");
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_press_held_still_runs_its_long_press_and_clicks_nothing() {
    let source = r#"
export component Probe {
    state held = 0;
    state at = 0;
    state clicks = 0;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text {
                width: 40dp;
                height: 20dp;
                on long_press(event) { held += 1; if event.position.x > 5dp { at = 1; } }
                on click { clicks += 1; }
            }
        }
    }
}
"#;
    let mut rt = Rt::mount(source);
    sample(&mut rt, 8.0, 5.0, PointerPhase::Down);
    let mut timers = Vec::new();
    rt.store.take_timer_requests(&mut timers);
    assert_eq!(timers.len(), 1, "the press asked for its delay");
    for timer in timers {
        (timer.on_fire)(&mut rt.store);
    }
    deliver_long_presses(&mut rt.store, &mut rt.states, &rt.bindings);
    rt.frame();
    assert_eq!(
        rt.ints(["held", "at", "clicks"]),
        [Some(1), Some(1), Some(0)]
    );
    sample(&mut rt, 8.0, 5.0, PointerPhase::Up);
    assert_eq!(rt.int("clicks"), Some(0), "the lift after a long press");
    rt.click_at(8.0, 5.0);
    assert_eq!(rt.int("clicks"), Some(1));
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_tab_strip_selects_its_child() {
    let source = r#"
style Tab for Text {
    opacity: 0.5f32;
    when selected { opacity: 1.0f32; }
}
export component Probe {
    state tab: U32 = 1;
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Tabs {
                width: 300dp;
                height: 20dp;
                bind selected <=> tab;
                Text { width: 100dp; height: 20dp; styles: [Tab]; }
                Text { width: 100dp; height: 20dp; styles: [Tab]; }
                Text { width: 100dp; height: 20dp; styles: [Tab]; }
            }
        }
    }
}
"#;
    let mut rt = Rt::mount(source);
    let tabs = children(&rt)[0];
    let opacities = |rt: &Rt| {
        rt.children(tabs)
            .into_iter()
            .map(|tab| rt.store.opacity(tab))
            .collect::<Vec<_>>()
    };
    assert_eq!(opacities(&rt), [0.5, 1.0, 0.5]);
    rt.click_at(250.0, 10.0);
    assert_eq!(opacities(&rt), [0.5, 0.5, 1.0]);
    assert_eq!(rt.fault(), None);
}

#[test]
fn focus_and_visible_focus_select_apart() {
    let source = r#"
style Ring for Text {
    when focused { opacity: 0.75f32; }
    when focus_visible { opacity: 0.25f32; }
}
export component Probe {
    view {
        Column {
            width: 400dp;
            height: 100dp;
            Text { width: 20dp; height: 20dp; focusable: true; styles: [Ring]; }
        }
    }
}
"#;
    let mut rt = Rt::mount(source);
    let node = children(&rt)[0];
    assert_eq!(look(&rt, 0).1, 1.0);
    rt.store.set_focused(Some(node));
    rt.store.set_focus_visible(false);
    rt.frame();
    assert_eq!(look(&rt, 0).1, 0.75, "a pointer focused it");
    rt.store.set_focus_visible(true);
    rt.frame();
    assert_eq!(look(&rt, 0).1, 0.25, "the keyboard focused it");
    rt.store.set_focused(None);
    rt.frame();
    assert_eq!(look(&rt, 0).1, 1.0);
}
