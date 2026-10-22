//! Themes: a `theme` declaration is a constant `Theme` built from its items,
//! its base and the record's defaults; a view reads the one the store
//! publishes through `theme`, and switching it re-evaluates what reads it, on
//! the hot-reloaded and the packaged view. And the checks: items name `Theme`
//! fields once (`E2001`, `E2103`), of their type (`E2103`), with no field left
//! without a value (`E2103`); a `Theme` base (`E2001`, `E2103`) on no cycle
//! (`E2003`); pure values (`E2501`); `theme` read only in a view, or in a
//! theme as its base's value (`E2111`).

mod support;

use support::{Rt, origin};
use viso_dsl::frontend::compile_file;
use viso_ui::{FontFamily, NodeId, Rgba, Srgb};
use viso_view::{Value, default_theme, set_theme};

/// No diagnostic.
const CLEAN: [&str; 0] = [];

/// The default theme's palette, as `ColorPalette` fields.
const PALETTE: &str = "background: #ffffff, foreground: #1b1f24, surface: #f5f6f8, \
     on_surface: #1b1f24, primary: #3b6cf0, on_primary: #ffffff, primary_hover: #2f5cd6, \
     accent: #8a4fff, muted: #6b7280, outline: #d0d5dd, error: #d93a3a, on_error: #ffffff, \
     focus_ring: #3b6cf099, scrim: #00000066";

/// The default theme's shadows, as `ElevationScale` fields.
const SHADOWS: &str = "low: Shadow { y: 1dp, blur: 2dp, color: #00000024 }, \
     medium: Shadow { y: 2dp, blur: 6dp, color: #0000002e }, \
     high: Shadow { y: 6dp, blur: 16dp, color: #00000038 }";

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
        codes(&format!(
            "theme A {{ colors = theme.colors; elevation = ElevationScale {{ {SHADOWS} }}; }}"
        )),
        ["E2111"],
        "a theme without a base has no `theme` to read"
    );
}

#[test]
fn theme_in_a_theme_reads_its_base() {
    assert_eq!(codes("theme A: Light { spacing = theme.spacing; }"), CLEAN);
    assert_eq!(
        codes("theme A: Light { spacing = theme.colors; }"),
        ["E2103"],
        "`theme` is the base, typed `Theme`"
    );
    assert_eq!(
        codes(
            "theme A: Light { colors = ColorPalette { primary: theme.colors.accent, ..theme.colors }; }\ntheme B: A { spacing = A.spacing; }"
        ),
        CLEAN
    );
    let source = probe(&themes(
        "theme Swap: Tight {\n    \
         colors = ColorPalette { primary: theme.colors.accent, ..theme.colors };\n}",
    ));
    for mut rt in [Rt::mount(&source), Rt::packaged(&source)] {
        let swap = theme(&rt, "Swap");
        set_theme(&mut rt.states, &mut rt.store, swap);
        rt.frame();
        assert_eq!(first_fill(&rt), rgba(0x8a4f_ffff), "`Tight`'s accent");
        rt.click(0);
        assert_eq!(rt.int("taps"), Some(0), "`Tight`'s spacing");
    }
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
    let first: NodeId = arena.first_child(root).expect("a child");
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
    set_theme(&mut rt.states, &mut rt.store, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(GREEN));
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(1), "the handler read the new theme");

    let roomy = theme(&rt, "Roomy");
    set_theme(&mut rt.states, &mut rt.store, roomy);
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
    set_theme(&mut rt.states, &mut rt.store, tight);
    rt.frame();
    rt.reload(&source.replace("primary: #00ff00", "primary: #0000ff"));
    assert_eq!(
        first_fill(&rt),
        rgba(GREEN),
        "the store keeps the value it was set"
    );
    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, &mut rt.store, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(0x0000_ffff), "the edited theme");
}

#[test]
fn the_release_package_runs_the_same() {
    let mut rt = Rt::packaged(&probe(&themes("")));
    assert_eq!(first_fill(&rt), rgba(PRIMARY));
    assert_eq!(theme(&rt, "Light"), default_theme());
    let tight = theme(&rt, "Tight");
    set_theme(&mut rt.states, &mut rt.store, tight);
    rt.frame();
    assert_eq!(first_fill(&rt), rgba(GREEN));
    rt.click(0);
    assert_eq!(rt.int("taps"), Some(0));
}

#[test]
fn a_theme_sets_the_root_font_size_and_the_family_text_falls_back_to() {
    let mut rt = Rt::mount(&probe(&themes(
        "theme Big: Light {\n    \
         typography = TypographyScale { base_size: 20sp, family: FontFamily::monospace };\n}",
    )));
    assert_eq!(rt.store.length_env().base_font_size, 14.0);
    assert_eq!(*rt.store.text_family(), FontFamily::SystemUi);
    rt.store.take_text_resized();

    let big = theme(&rt, "Big");
    set_theme(&mut rt.states, &mut rt.store, big);
    rt.frame();
    assert_eq!(rt.store.length_env().base_font_size, 20.0);
    assert_eq!(rt.store.length_env().root_font_size(), 20.0);
    assert_eq!(*rt.store.text_family(), FontFamily::Monospace);
    assert!(rt.store.take_text_resized(), "the text shapes anew");

    let again = theme(&rt, "Big");
    set_theme(&mut rt.states, &mut rt.store, again);
    assert!(
        !rt.store.take_text_resized(),
        "the same typography moves nothing"
    );
}
