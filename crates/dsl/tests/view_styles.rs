//! Styles: a node's `styles` apply each style after its bases, a later binding
//! over an earlier one and the node's own properties over them all; a `when`
//! switches `background` and `opacity` while its selector holds — hover,
//! pressed, focus, or the node's own `enabled` — on the hot-reloaded and the
//! packaged view and inside a region. And the checks: a target the file names
//! (`E2001`), bases of the file for the same target (`E2001`, `E2103`) on no
//! cycle (`E2003`), styleable properties (`E3101`) bound once per block
//! (`E3102`) to pure values (`E2502`) of their type (`E2103`), selectors the
//! target supports (`E2001`, `E2103`), a `styles` list of styles for the node
//! (`E2103`), and `@styleable`/`@selector` on the members they fit (`E3710`).

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{NodeId, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rgba, Srgb};

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
    assert_eq!(codes(&style("dragging"), "Text {}"), ["E2001"]);
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
    let mut at = arena.links(root).and_then(|l| l.first_child);
    while let Some(node) = at {
        out.push(node);
        at = arena.links(node).and_then(|l| l.next_sibling);
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
    let root = rt.root.expect("mounted");
    let event = PointerEvent {
        x: 5.0,
        y: 5.0 + 20.0 * n as f32,
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
    rt.reload(&SOURCE.replace("styles: [Dim]; enabled", "enabled"));
    assert_eq!(look(&rt, 0).1, 1.0, "no style, no look");
}

#[test]
fn a_style_on_a_component_node_does_not_mount_yet() {
    let source = "component K {\n@styleable\ninput tint: Color = #000000;\n\
                  view { Text { background: tint; } }\n}\n\
                  style S for K { tint: #ff0000; }\n\
                  export component C { view { K { styles: [S]; } } }\n";
    let compiled = compile_file(source, &origin());
    let codes: Vec<_> = compiled.errors().map(|d| d.code).collect();
    assert_eq!(codes, ["E3711"]);
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
