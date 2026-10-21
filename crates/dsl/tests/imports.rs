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

#[test]
fn a_declaration_read_through_its_module_is_e2001() {
    let page = PAGE
        .replace("import model::{twice, START};", "import model;")
        .replace("twice(n + START)", "model::twice(n + model::START)");
    let origin = Origin {
        siblings: siblings(MODEL),
        ..origin()
    };
    let errors: Vec<_> = compile_file(&page, &origin)
        .errors()
        .map(|d| (d.code, d.message.clone()))
        .collect();
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert!(errors.iter().all(|(code, _)| *code == "E2001"));
    assert!(
        errors[1].1.contains("import model::{START};"),
        "{errors:#?}"
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
