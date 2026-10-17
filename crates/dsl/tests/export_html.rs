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
        script.contains("r.push(region(e[0], G[0], sc));"),
        "{script}"
    );
    // The text of each item reads its binding.
    assert!(script.contains("P.text(e[0], f"), "{script}");
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
