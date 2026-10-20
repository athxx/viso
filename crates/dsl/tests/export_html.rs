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
        }
    }
}
"##;

#[test]
fn a_box_s_corner_radius_and_border_export_as_rounded_corners_and_an_inner_stroke() {
    let page = export(SHAPES, false).unwrap();
    let css = &page.styles;
    assert!(
        css.contains("border-radius: calc(4 * 1px + 1 * 0.0625rem);"),
        "{css}"
    );
    assert!(
        css.contains("box-shadow: inset 0 0 0 2px #ff0000;"),
        "{css}"
    );
    assert!(
        css.contains("border-radius: calc(9999px);"),
        "a pill: {css}"
    );
    let script = page.script.as_deref().unwrap();
    assert!(script.contains("P.vradius("), "{script}");
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
