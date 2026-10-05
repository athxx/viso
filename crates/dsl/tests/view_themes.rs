//! Themes: a `theme` declaration is a constant `Theme` built from its items,
//! its base and the record's defaults; a view reads the one the store
//! publishes through `theme`, and switching it re-evaluates what reads it, on
//! the hot-reloaded and the packaged view. And the checks: items name `Theme`
//! fields once (`E2001`, `E2103`), of their type (`E2103`), with no field left
//! without a value (`E2103`); a `Theme` base (`E2001`, `E2103`) on no cycle
//! (`E2003`); pure values (`E2501`); `theme` read only in a view (`E2111`).

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{NodeId, Rgba, Srgb};
use viso_view::{Value, default_theme, set_theme};

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The default theme's palette, as `ColorPalette` fields.
const PALETTE: &str = "background: #ffffff, foreground: #1b1f24, surface: #f5f6f8, \
     on_surface: #1b1f24, primary: #3b6cf0, on_primary: #ffffff, primary_hover: #2f5cd6, \
     accent: #8a4fff, muted: #6b7280, outline: #d0d5dd, error: #d93a3a, on_error: #ffffff, \
     focus_ring: #3b6cf099, scrim: #00000066";

/// The default theme's shadows, as `ElevationScale` fields.
const SHADOWS: &str = "low: Shadow { offset_y: 1dp, blur: 2dp, color: #00000024 }, \
     medium: Shadow { offset_y: 2dp, blur: 6dp, color: #0000002e }, \
     high: Shadow { offset_y: 6dp, blur: 16dp, color: #00000038 }";

/// The themes `Light` (the default one, spelled out), `Roomy` and `Tight`
/// over it, and `extra`.
fn themes(extra: &str) -> String {
    format!(
        "theme Light {{\n    colors = ColorPalette {{ {PALETTE} }};\n    \
         elevation = ElevationScale {{ {SHADOWS} }};\n}}\n\
         theme Roomy: Light {{ spacing = SpacingScale {{ large: 40dp }}; }}\n\
         export theme Tight: Roomy {{\n    spacing = SpacingScale {{ large: 4dp, small: 1dp }};\n    \
         colors = ColorPalette {{ primary: #00ff00, ..Light.colors }};\n}}\n\
         {extra}\n"
    )
}

/// The diagnostic codes compiling `extra` beside the themes reports.
fn codes(extra: &str) -> Vec<&'static str> {
    let source = format!(
        "{}export component C {{ view {{ Text {{}} }} }}\n",
        themes(extra)
    );
    let compiled = compile_file(&source, &origin());
    let diagnostics: Vec<_> = compiled.errors().collect();
    eprintln!("{diagnostics:#?}");
    diagnostics.iter().map(|d| d.code).collect()
}

#[test]
fn a_theme_gives_theme_fields_once_and_of_their_type() {
    assert_eq!(codes(""), CLEAN);
    assert_eq!(
        codes("theme A: Light { colors = ColorPalette { accent: #ff0000, ..Light.colors }; }"),
        CLEAN,
        "an item reads another theme"
    );
    assert_eq!(
        codes("theme A: Light { borders = SpacingScale {}; }"),
        ["E2001"]
    );
    assert_eq!(
        codes("theme A: Light { spacing = SpacingScale {}; spacing = SpacingScale {}; }"),
        ["E2103"]
    );
    assert_eq!(
        codes("theme A: Light { spacing = RadiusScale {}; }"),
        ["E2103"]
    );
    assert_eq!(
        codes(&format!(
            "theme A {{ colors = ColorPalette {{ {PALETTE} }}; }}"
        )),
        ["E2103"],
        "`elevation` has no default"
    );
}

#[test]
fn a_base_is_a_theme_on_no_cycle() {
    assert_eq!(codes("theme A: Nowhere {}"), ["E2001"]);
    assert_eq!(codes("const N: I64 = 1;\ntheme A: N {}"), ["E2103"]);
    assert_eq!(codes("theme A: B {}\ntheme B: A {}"), ["E2003", "E2003"]);
    assert_eq!(
        codes("theme A: Light { spacing = B.spacing; }\ntheme B: A {}"),
        ["E2003", "E2003"]
    );
}

#[test]
fn theme_values_are_pure_and_theme_is_read_in_a_view() {
    assert_eq!(
        codes(
            "action poke() -> SpacingScale { SpacingScale {} }\n\
             theme A: Light { spacing = poke(); }"
        ),
        ["E2501"]
    );
    assert_eq!(codes("const C: ColorPalette = theme.colors;"), ["E2111"]);
    assert_eq!(
        codes("theme A: Light { spacing = theme.spacing; }"),
        ["E2111"]
    );
}

/// A view whose first box is filled with the theme's primary color and counts
/// its clicks while the theme's large spacing is above 10dp.
fn probe(themes: &str) -> String {
    format!(
        r#"
{themes}
export component Probe {{
    state taps = 0;
    view {{
        Column {{
            width: 400dp;
            height: 100dp;
            Text {{
                width: 20dp;
                height: 20dp;
                background: theme.colors.primary;
                on click {{ if theme.spacing.large > 10dp {{ taps += 1; }} }}
            }}
            Text {{ width: 20dp; height: 20dp; }}
        }}
    }}
}}
"#
    )
}

/// The fill of the root's first child, as `0xRRGGBBAA`.
fn first_fill(rt: &Rt) -> Rgba {
    let root = rt.root.expect("mounted");
    let arena = rt.store.arena();
    let first: NodeId = arena
        .links(root)
        .and_then(|l| l.first_child)
        .expect("a child");
    rt.store.style(first).fill
}

fn rgba(hex: u32) -> Rgba {
    Srgb::from_rgba32(hex).into_linear_straight()
}

/// The default primary color, and `Tight`'s.
const PRIMARY: u32 = 0x3b6c_f0ff;
const GREEN: u32 = 0x00ff_00ff;

/// The theme `name` the view's package declares.
fn theme(rt: &Rt, name: &str) -> Value {
    let mut host = rt.view.as_ref().expect("a view").borrow_mut();
    host.theme(name).expect("declared").expect("evaluates")
}

#[test]
fn a_declared_theme_matches_the_default_one() {
    let rt = Rt::mount(&probe(&themes("")));
    assert_eq!(theme(&rt, "Light"), default_theme());
    let roomy = theme(&rt, "Roomy");
    let Value::Agg(roomy) = &roomy else {
        panic!("a record: {roomy:?}");
    };
    let Value::Agg(light) = default_theme() else {
        unreachable!()
    };
    assert_eq!(roomy.fields[0], light.fields[0], "the base's colors");
    assert_ne!(roomy.fields[2], light.fields[2], "its own spacing");
    assert!(
        rt.view
            .as_ref()
            .unwrap()
            .borrow_mut()
            .theme("Dim")
            .is_none()
    );
}

#[test]
fn switching_the_theme_re_evaluates_what_reads_it() {
    let mut rt = Rt::mount(&probe(&themes("")));
    assert_eq!(first_fill(&rt), rgba(PRIMARY), "the default theme");
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(1));

    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(GREEN));
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(1), "the handler read the new theme");

    let roomy = theme(&rt, "Roomy");
    set_theme(&mut rt.states, roomy);
    rt.frame();
    assert_eq!(
        first_fill(&rt),
        rgba(PRIMARY),
        "`Roomy` keeps its base's colors"
    );
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(2));
    assert_eq!(rt.fault(), None);
}

#[test]
fn a_hot_reload_rebuilds_the_themes_and_keeps_the_one_set() {
    let source = probe(&themes(""));
    let mut rt = Rt::mount(&source);
    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, tight);
    rt.frame();
    rt.reload(&source.replace("primary: #00ff00", "primary: #0000ff"));
    assert_eq!(
        first_fill(&rt),
        rgba(GREEN),
        "the store keeps the value it was set"
    );
    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(0x0000_ffff), "the edited theme");
}

#[test]
fn the_release_package_runs_the_same() {
    let mut rt = Rt::packaged(&probe(&themes("")));
    assert_eq!(first_fill(&rt), rgba(PRIMARY));
    assert_eq!(theme(&rt, "Light"), default_theme());
    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(GREEN));
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(0));
}
