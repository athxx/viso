//! The `web-dom` page: the view as HTML and CSS, its behavior called on the
//! Viso VM by handler-table entry, and what a page's DOM cannot have
//! reported where it is written.

mod support;

use std::path::Path;
use std::rc::Rc;
use support::origin;
use viso_dsl::assets::AssetCatalog;
use viso_dsl::export::{DomOptions, DomPage, Need, dom};
use viso_dsl::frontend::compile_file_for;
use viso_dsl::hir::TargetProfile;
use viso_dsl::schema::Natives;

fn page(source: &str) -> Result<DomPage, Vec<(Need, String, String)>> {
    page_in(source, None)
}

/// [`page`] for a view whose `asset("…")`s are found under `assets`.
fn page_in(source: &str, assets: Option<&Path>) -> Result<DomPage, Vec<(Need, String, String)>> {
    let profile = TargetProfile {
        assets: assets.map(|dir| Rc::new(AssetCatalog::scan(dir).unwrap())),
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(source, &origin(), Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    let options = DomOptions {
        title: "Demo".into(),
        lang: "en".into(),
    };
    dom(&compiled, &options).map_err(|findings| {
        findings
            .into_iter()
            .map(|f| {
                let at = &source[std::ops::Range::<usize>::from(f.at)];
                (f.need, at.to_owned(), f.message)
            })
            .collect()
    })
}

const COUNTER: &str = r#"
export component Counter {
    state clicks = 0;
    view {
        Column {
            width: 320dp;
            gap: 16dp;
            Text { width: 320dp; height: 40dp; text: format("Clicked {n} times", n: clicks); }
            Button {
                width: 320dp;
                height: 40dp;
                text: "Click";
                on click { clicks += 1; }
            }
        }
    }
}
"#;

#[test]
fn a_page_s_behavior_runs_on_the_vm_by_entry() {
    let page = page(COUNTER).unwrap();
    let script = &page.script;
    assert!(
        script.contains("globalThis.visoPage = (V) => {"),
        "{script}"
    );
    // No function of the behavior is translated: each value and handler is
    // a call of the VM.
    assert!(!script.contains("function f"), "{script}");
    assert!(!script.contains("let S = "), "{script}");
    assert!(script.contains("P.text(e[0], V.e("), "{script}");
    assert!(script.contains("(p) => V.t("), "{script}");
    assert_eq!(page.component, "Counter");
    assert!(!page.module.is_empty());
}

#[test]
fn the_page_is_the_view_s_html_before_its_script_runs() {
    let page = page(COUNTER).unwrap();
    let index = page.index(
        "styles.css",
        "page.js",
        "<script type=\"module\">boot();</script>\n",
    );
    assert!(index.starts_with("<!DOCTYPE html>\n"), "{index}");
    assert!(
        index.contains("<link rel=\"stylesheet\" href=\"styles.css\">"),
        "{index}"
    );
    // The text shows its first value as the page loads, for a reader or a
    // crawler that runs no script.
    assert!(index.contains("Clicked 0 times"), "{index}");
    let script = index
        .find("<script src=\"page.js\" defer></script>")
        .unwrap();
    let boot = index.find("boot();").unwrap();
    assert!(script < boot, "{index}");
    assert!(index.ends_with("</body>\n</html>\n"), "{index}");
}

#[test]
fn a_node_a_page_s_dom_cannot_draw_is_found_where_it_is_written() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/10-custom-shader");
    let source = std::fs::read_to_string(example.join("src/ripples.vs")).unwrap();
    let findings = page_in(&source, Some(&example.join("assets"))).unwrap_err();
    assert!(
        findings
            .iter()
            .any(|(need, at, message)| *need == Need::GpuOnly
                && at.starts_with("ShaderView")
                && message.contains("shader")),
        "{findings:#?}"
    );
}
