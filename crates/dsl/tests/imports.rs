//! A one-file compile reads the package's modules its source imports (§22):
//! a `fn`, a `const` and a `record` of a sibling module reach the view under
//! hot reload and from the release package, a module that does not compile
//! fails the importer at its import, and an import with nowhere to read
//! modules from stays unresolved.

mod support;

use std::sync::Arc;

use support::{Rt, origin};
use viso_dsl::frontend::{Imported, Origin, Siblings, compile_file, package_root};

const MODEL: &str = r#"
import units::{ONE};
export const START: I64 = ONE + 1;
export fn twice(x: I64) -> I64 { x * 2 }
"#;

const UNITS: &str = "export const ONE: I64 = 1;\n";

const PAGE: &str = r#"
import model::{twice, START};
export component Page {
    state n = 0;
    view {
        Column {
            Text { width: 20dp; height: 20dp; on click { n = twice(n + START); } }
        }
    }
}
"#;

fn siblings(model: &str) -> Siblings {
    Siblings::Memory(Arc::from(vec![
        (vec!["model".to_owned()], model.to_owned()),
        (vec!["units".to_owned()], UNITS.to_owned()),
    ]))
}

fn module(path: &str) -> Imported {
    Imported {
        module: vec![path.to_owned()],
        path: None,
    }
}

#[test]
fn an_imported_fn_and_const_reach_the_view_in_every_target() {
    let origin = Origin {
        siblings: siblings(MODEL),
        ..origin()
    };
    let compiled = compile_file(PAGE, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    assert_eq!(compiled.imported, [module("model"), module("units")]);
    for mut rt in [
        Rt::mount_in(PAGE, siblings(MODEL)),
        Rt::packaged_in(PAGE, siblings(MODEL)),
    ] {
        rt.click(0);
        assert_eq!(rt.int("n"), Some(4), "twice(0 + 2)");
        rt.click(0);
        assert_eq!(rt.int("n"), Some(12));
    }
}

#[test]
fn a_reload_reads_the_imports_again() {
    let mut rt = Rt::mount_in(PAGE, siblings(MODEL));
    rt.origin = Some(Origin {
        siblings: siblings(&MODEL.replace("x * 2", "x * 3")),
        ..origin()
    });
    rt.reload(PAGE);
    rt.click(0);
    assert_eq!(rt.int("n"), Some(6), "the new twice");
}

#[test]
fn a_module_that_does_not_compile_fails_its_importer_at_the_import() {
    let origin = Origin {
        siblings: siblings(&MODEL.replace("x * 2", "x * missing")),
        ..origin()
    };
    let compiled = compile_file(PAGE, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert_eq!(errors.len(), 1, "{errors:#?}");
    let error = errors[0];
    assert!(
        error.message.starts_with("in `model`: "),
        "{}",
        error.message
    );
    let at = error.primary.start().to_u32() as usize..error.primary.end().to_u32() as usize;
    assert_eq!(&PAGE[at], "import model::{twice, START};");
    assert_eq!(error.related[0].module.as_deref(), Some("model"));
}

const SHAPES: &str = r#"
import units::{BASE};
export const SIDE: MixedLength = BASE;
export record Size { w: I64; h: I64; }
export enum Turn { left; right; }
export fn area(s: Size) -> I64 { s.w * s.h }
fn hidden() -> I64 { 0 }
"#;

const QUALIFIED: &str = r#"
import shapes;
export component Page {
    state n = 0;
    state turn: shapes::Turn = shapes::Turn::left;
    view {
        Column {
            Row {
                width: shapes::SIDE;
                height: 20dp;
                on click {
                    let s: shapes::Size = shapes::Size { w: 2, h: 3 };
                    n += match turn {
                        shapes::Turn::left => shapes::area(s),
                        shapes::Turn::right => 0,
                    };
                    turn = shapes::Turn::right;
                }
            }
        }
    }
}
"#;

fn shapes(source: &str) -> Siblings {
    Siblings::Memory(Arc::from(vec![
        (vec!["shapes".to_owned()], source.to_owned()),
        (
            vec!["units".to_owned()],
            "export const BASE: MixedLength = 120dp;\n".to_owned(),
        ),
    ]))
}

#[test]
fn a_declaration_reads_through_its_module_in_every_target() {
    for mut rt in [
        Rt::mount_in(QUALIFIED, shapes(SHAPES)),
        Rt::packaged_in(QUALIFIED, shapes(SHAPES)),
    ] {
        let row = rt.children(rt.root.expect("mounted"))[0];
        assert_eq!(rt.store.bounds(row).w, 120.0, "`shapes::SIDE`");
        rt.click(0);
        assert_eq!(rt.int("n"), Some(6), "`shapes::area` of a `shapes::Size`");
        rt.click(0);
        assert_eq!(rt.int("n"), Some(6), "matched `shapes::Turn::right`");
    }
}

#[test]
fn a_name_its_module_does_not_export_is_e2001_at_the_name() {
    let page = QUALIFIED.replace("shapes::area(s)", "shapes::hidden() + shapes::areas(s)");
    let origin = Origin {
        siblings: shapes(SHAPES),
        ..origin()
    };
    let errors: Vec<_> = compile_file(&page, &origin)
        .errors()
        .map(|d| {
            let at = d.primary.start().to_u32() as usize..d.primary.end().to_u32() as usize;
            (d.code, page[at].to_owned(), d.fixes.len())
        })
        .collect();
    assert_eq!(
        errors,
        [
            ("E2001", "hidden".to_owned(), 1),
            ("E2001", "areas".to_owned(), 1)
        ],
        "unexported (fix: export it), unknown (fix: the nearest export)"
    );
}

#[test]
fn with_nowhere_to_read_it_an_import_stays_unresolved() {
    let codes: Vec<_> = compile_file(PAGE, &origin())
        .errors()
        .map(|d| d.code)
        .collect();
    assert!(codes.contains(&"E2001"), "{codes:?}");
}

#[test]
fn a_package_directory_holds_the_imports_and_its_root_is_found() {
    let root = std::env::temp_dir().join(format!("viso-imports-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src/model")).unwrap();
    std::fs::write(root.join("src/model/mod.vs"), MODEL).unwrap();
    std::fs::write(root.join("src/units.vs"), UNITS).unwrap();
    let file = root.join("src/page.vs");
    std::fs::write(&file, PAGE).unwrap();
    assert_eq!(
        package_root(&file, &["page".to_owned()]).as_deref(),
        Some(root.as_path())
    );
    let origin = Origin::for_file("app", &root, &file, None);
    let compiled = compile_file(PAGE, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let read: Vec<_> = compiled.imported.iter().map(|i| i.path.clone()).collect();
    assert_eq!(
        read,
        [
            Some(root.join("src/model/mod.vs")),
            Some(root.join("src/units.vs"))
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}

const SIZES: &str = r#"
import units::{BASE};
export const WIDE: MixedLength = BASE;
"#;

const SIZED: &str = r#"
import sizes::{WIDE};
export component Sized {
    state taps = 0;
    view {
        Column {
            Row { width: WIDE; height: 20dp; on click { taps += 1; } }
        }
    }
}
"#;

#[test]
fn an_imported_const_folds_into_style_as_one_of_the_file() {
    let siblings = || {
        Siblings::Memory(Arc::from(vec![
            (vec!["sizes".to_owned()], SIZES.to_owned()),
            (
                vec!["units".to_owned()],
                "export const BASE: MixedLength = 120dp;\n".to_owned(),
            ),
        ]))
    };
    for rt in [
        Rt::mount_in(SIZED, siblings()),
        Rt::packaged_in(SIZED, siblings()),
    ] {
        let row = rt.children(rt.root.expect("mounted"))[0];
        assert_eq!(rt.store.bounds(row).w, 120.0, "`WIDE` through `BASE`");
    }
}

const WIDGETS: &str = r#"
import units::{BASE};
style Dim for Text { opacity: 0.5f32; }
export component Counter {
    input step: I64 = 1;
    event counted(value: I64);
    state count = 0;
    view {
        Row {
            width: BASE;
            height: 20dp;
            Text { width: 20dp; height: 20dp; on click { count += step; emit counted(count); } }
            Text { width: 20dp; height: 20dp; styles: [Dim]; text: format("{}", count); }
            if count > 2 {
                Text { width: 20dp; height: 20dp; text: "many"; }
            }
        }
    }
}
export component Loop {
    view { Column { Loop {} } }
}
"#;

const BOARD: &str = r#"
import widgets::{Counter};
export component Board {
    state total = 0;
    view {
        Column {
            width: 200dp;
            height: 100dp;
            Counter { step: 2; on counted(event) { total = event.value; } }
            Counter {}
        }
    }
}
"#;

fn widgets() -> Siblings {
    Siblings::Memory(Arc::from(vec![
        (vec!["widgets".to_owned()], WIDGETS.to_owned()),
        (
            vec!["units".to_owned()],
            "export const BASE: MixedLength = 120dp;\n".to_owned(),
        ),
    ]))
}

#[test]
fn a_component_of_another_file_inlines_with_its_state_events_and_constants() {
    for mut rt in [
        Rt::mount_in(BOARD, widgets()),
        Rt::packaged_in(BOARD, widgets()),
    ] {
        let root = rt.root.expect("mounted");
        let rows = rt.children(root);
        assert_eq!(
            rt.store.bounds(rows[0]).w,
            120.0,
            "`BASE` read in `widgets`"
        );
        let shown = rt.children(rows[0])[1];
        assert_eq!(rt.store.opacity(shown), 0.5, "`Dim`, a style of `widgets`");
        rt.click(0);
        assert_eq!(rt.children(rows[0]).len(), 2);
        rt.click(0);
        assert_eq!(rt.int("total"), Some(4), "the first counter steps by 2");
        assert_eq!(rt.children(rows[0]).len(), 3, "its `if` shows past 2");
        rt.click(1);
        assert_eq!(rt.int("total"), Some(4), "the second keeps its own count");
        assert_eq!(rt.children(rows[1]).len(), 2);
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn what_does_not_mount_in_another_file_is_reported_where_this_one_places_it() {
    let page = BOARD
        .replace("Counter {}", "Loop {}")
        .replace("{Counter}", "{Counter, Loop}");
    let origin = Origin {
        siblings: widgets(),
        ..origin()
    };
    let errors: Vec<_> = compile_file(&page, &origin)
        .errors()
        .map(|d| {
            let at = d.primary.start().to_u32() as usize..d.primary.end().to_u32() as usize;
            (d.code, page[at].to_owned(), d.message.contains("in `Loop`"))
        })
        .collect();
    assert_eq!(
        errors,
        [("E3711", "Loop {}".to_owned(), true)],
        "{errors:?}"
    );
}

#[test]
fn a_component_of_another_file_exports_to_the_web() {
    let origin = Origin {
        siblings: widgets(),
        ..origin()
    };
    let compiled = compile_file(BOARD, &origin);
    let page = viso_dsl::export::html(
        &compiled,
        &viso_dsl::export::HtmlOptions {
            static_only: false,
            title: "Board".into(),
            lang: "en".into(),
        },
    )
    .expect("exports");
    let script = page.script.expect("a script");
    assert_eq!(script.matches("  on(e[").count(), 2, "{script}");
    assert_eq!(script.matches("region(e[").count(), 2, "{script}");
    assert!(page.styles.contains("opacity: 0.5"), "{}", page.styles);
}

#[test]
fn a_template_of_another_file_inlines_with_its_parameters() {
    let widgets = Siblings::Memory(Arc::from(vec![(
        vec!["cards".to_owned()],
        "export template Card(w: MixedLength = 30dp) {\n    view { Text { width: w; height: 10dp; } }\n}\n"
            .to_owned(),
    )]));
    let page = r#"
import cards::{Card};
export component Deck {
    view {
        Column {
            use Card(50dp);
            use Card();
        }
    }
}
"#;
    for rt in [
        Rt::mount_in(page, widgets.clone()),
        Rt::packaged_in(page, widgets.clone()),
    ] {
        let root = rt.root.expect("mounted");
        let cards = rt.children(root);
        assert_eq!(rt.store.bounds(cards[0]).w, 50.0);
        assert_eq!(rt.store.bounds(cards[1]).w, 30.0);
    }
}

const LOOKS: &str = r#"
import units::{BASE};
export style Faded for Text { opacity: 0.25f32; }
export style Wide for Text: Faded {
    width: BASE;
    when hover { opacity: 1.0f32; }
}
"#;

const GALLERY: &str = r#"
import looks::{Wide, Faded};
style Mine for Text: Faded { height: 20dp; }
export component Gallery {
    view {
        Column {
            Text { styles: [Wide]; height: 20dp; }
            Text { styles: [Mine]; width: 20dp; }
        }
    }
}
"#;

fn looks() -> Siblings {
    Siblings::Memory(Arc::from(vec![
        (vec!["looks".to_owned()], LOOKS.to_owned()),
        (
            vec!["units".to_owned()],
            "export const BASE: MixedLength = 60dp;\n".to_owned(),
        ),
    ]))
}

/// A primary pointer move to `(x, y)`, then a frame.
fn hover(rt: &mut Rt, x: f32, y: f32) {
    let root = rt.root.expect("mounted");
    let event = viso_ui::PointerEvent {
        x,
        y,
        phase: viso_ui::PointerPhase::Move,
        buttons: viso_ui::PointerButtons::default(),
        modifiers: Default::default(),
    };
    viso_ui::PointerRouter::route(
        &mut rt.store,
        &mut rt.states,
        &rt.bindings,
        root,
        event,
        &mut Vec::new(),
    );
    rt.frame();
}

#[test]
fn a_style_of_another_file_applies_with_its_bases_constants_and_selectors() {
    for mut rt in [
        Rt::mount_in(GALLERY, looks()),
        Rt::packaged_in(GALLERY, looks()),
    ] {
        let root = rt.root.expect("mounted");
        let texts = rt.children(root);
        assert_eq!(rt.store.bounds(texts[0]).w, 60.0, "`BASE` read in `looks`");
        assert_eq!(rt.store.opacity(texts[0]), 0.25, "`Faded`, its base");
        assert_eq!(rt.store.opacity(texts[1]), 0.25, "a base of another file");
        assert_eq!(rt.store.bounds(texts[1]).h, 20.0);
        hover(&mut rt, 5.0, 5.0);
        assert_eq!(rt.store.opacity(texts[0]), 1.0, "its `when hover`");
        assert_eq!(rt.store.opacity(texts[1]), 0.25);
        assert_eq!(rt.fault(), None);
    }
}

#[test]
fn a_style_of_another_file_exports_to_the_web() {
    let origin = Origin {
        siblings: looks(),
        ..origin()
    };
    let compiled = compile_file(GALLERY, &origin);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let page = viso_dsl::export::html(
        &compiled,
        &viso_dsl::export::HtmlOptions {
            static_only: false,
            title: "Gallery".into(),
            lang: "en".into(),
        },
    )
    .expect("exports");
    assert!(page.styles.contains("opacity: 0.25"), "{}", page.styles);
    assert!(page.styles.contains(":hover"), "{}", page.styles);
}

#[test]
fn what_a_style_of_another_file_cannot_switch_is_reported_at_the_styles() {
    let looks = Siblings::Memory(Arc::from(vec![(
        vec!["looks".to_owned()],
        "export style Grow for Text { when hover { width: 40dp; } }\n".to_owned(),
    )]));
    let page = r#"
import looks::{Grow};
export component Page {
    view { Column { Text { styles: [Grow]; height: 20dp; } } }
}
"#;
    let origin = Origin {
        siblings: looks,
        ..origin()
    };
    let errors: Vec<_> = compile_file(page, &origin)
        .errors()
        .map(|d| {
            let at = d.primary.start().to_u32() as usize..d.primary.end().to_u32() as usize;
            (d.code, page[at].to_owned())
        })
        .collect();
    assert_eq!(errors, [("E3711", "[Grow]".to_owned())], "{errors:?}");
}
