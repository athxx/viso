//! `viso export html` end to end: the built binary over scratch projects,
//! asserting the files it writes, the `EXPORT_*` diagnostics that stop it,
//! and the directory it refuses to replace.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// A scratch project directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "viso-cli-export-{label}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, relative: &str, text: &str) -> &Self {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        self
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.0.join(relative)).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

const MANIFEST: &str = "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n";

const COUNTER: &str = "\
export component App {
    state count = 0;
    view {
        Column {
            Text { width: 100dp; height: 20dp; text: \"Count\"; }
            Button { width: 100dp; height: 32dp; text: \"Add\"; on click { count += 1; } }
        }
    }
}
";

fn counter(label: &str) -> Scratch {
    let s = Scratch::new(label);
    s.write("Viso.toml", MANIFEST).write("src/app.vs", COUNTER);
    s
}

#[test]
fn a_view_exports_as_a_page_with_its_script() {
    let s = counter("page");
    let out = viso(&s.0, &["export", "html", "--json"]);
    let json = stdout(&out);
    assert_eq!(code(&out), 0, "{json}{}", stderr(&out));
    assert!(
        json.contains("\"type\":\"artifact\"")
            && json.contains("\"kind\":\"export-dir\"")
            && json.contains("\"path\":\"dist-html\"")
            && json.contains("\"target\":\"html\"")
            && json.contains("\"profile\":null"),
        "{json}"
    );
    let index = s.read("dist-html/index.html");
    assert!(
        index.contains("<script src=\"app.js\" defer></script>"),
        "{index}"
    );
    assert!(index.contains(">Count</span>"), "{index}");
    assert!(s.read("dist-html/app.js").ends_with("start(m0);\n})();\n"));
    assert!(
        s.read("dist-html/styles.css")
            .starts_with("/* Generated from Viso source.")
    );
    let record = s.read("dist-html/.viso-generated.json");
    assert!(
        record.contains("\"generator\":\"viso export html\""),
        "{record}"
    );
    assert!(record.contains("\"path\":\"index.html\""), "{record}");

    // An unchanged export is replaced as it is.
    let again = viso(&s.0, &["export", "html"]);
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert_eq!(
        stdout(&again).lines().next(),
        Some("Exported html in 3 files")
    );
}

#[test]
fn a_static_export_refuses_what_changes_and_writes_nothing() {
    let s = counter("static");
    let out = viso(&s.0, &["export", "html", "--static"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.starts_with("error[EXPORT_HTML_DYNAMIC_REQUIRED]: "),
        "{err}"
    );
    assert!(err.contains(" --> src/app.vs:6:"), "{err}");
    assert!(!s.0.join("dist-html").exists());
}

#[test]
fn an_edited_export_is_kept_unless_forced() {
    let s = counter("edited");
    assert_eq!(code(&viso(&s.0, &["export", "html"])), 0);
    s.write("dist-html/index.html", "mine");
    let out = viso(&s.0, &["export", "html", "--json"]);
    assert_eq!(code(&out), 6);
    let json = stdout(&out);
    assert!(json.contains("\"code\":\"EXPORT_CONFLICT\""), "{json}");
    assert!(json.contains("`index.html`"), "{json}");
    assert_eq!(s.read("dist-html/index.html"), "mine");

    let forced = viso(&s.0, &["export", "html", "--force"]);
    assert_eq!(code(&forced), 0, "{}", stderr(&forced));
    assert!(
        s.read("dist-html/index.html")
            .starts_with("<!DOCTYPE html>")
    );
}

#[test]
fn a_directory_the_export_did_not_write_is_refused() {
    let s = counter("foreign");
    s.write("site/notes.txt", "keep");
    let out = viso(&s.0, &["export", "html", "--out", "site"]);
    assert_eq!(code(&out), 6);
    let err = stderr(&out);
    assert!(err.starts_with("error[EXPORT_CONFLICT]: "), "{err}");
    assert!(err.contains("did not write"), "{err}");
    assert_eq!(s.read("site/notes.txt"), "keep");

    let out = viso(&s.0, &["export", "html", "--out", "src/web"]);
    assert_eq!(code(&out), 2, "{}", stderr(&out));
    assert!(stderr(&out).starts_with("error[CLI_USAGE]: "));
}

#[test]
fn a_named_component_exports_alone() {
    let s = Scratch::new("named");
    s.write("Viso.toml", MANIFEST).write(
        "src/ui.vs",
        "export component Card {\n    input title: String = \"Hi\";\n    view { Text { width: 80dp; height: 20dp; text: title; } }\n}\n\
         export component Needy {\n    input title: String;\n    view { Text { text: title; } }\n}\n",
    );
    let out = viso(&s.0, &["export", "html"]);
    assert_eq!(code(&out), 1);
    assert!(
        stderr(&out).starts_with("error[EXPORT_NO_VIEW]: "),
        "{}",
        stderr(&out)
    );

    let out = viso(
        &s.0,
        &["export", "html", "ui::Card", "--static", "--out", "card"],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let index = s.read("card/index.html");
    assert!(index.contains(">Hi</span>"), "{index}");
    assert!(!s.0.join("card/app.js").exists());

    let out = viso(&s.0, &["export", "html", "Needy"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(err.contains("`ui::Needy` needs `title`"), "{err}");
}

#[test]
fn a_message_translates_on_the_page() {
    let s = Scratch::new("i18n");
    s.write(
        "Viso.toml",
        "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n\n[i18n]\nsource = \"en\"\n",
    )
    .write(
        "i18n/en.toml",
        "[app]\nclicks = \"{count, plural, one {# click} other {# clicks}}\"\nclick = \"Click me\"\n",
    )
    .write(
        "src/app.vs",
        "import viso::i18n::tr;\n\nexport component App {\n    state clicks = 0;\n    view {\n        Column {\n            \
         Text { width: 100dp; height: 20dp; text: tr(\"app.clicks\", count: clicks); }\n            \
         Button { width: 100dp; height: 20dp; text: tr(\"app.click\"); on click { clicks += 1; } }\n        }\n    }\n}\n",
    );
    let out = viso(&s.0, &["export", "html"]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert!(
        s.read("dist-html/index.html")
            .contains(">Click me</button>")
    );
    let script = s.read("dist-html/app.js");
    assert!(
        script.contains("const T = { Ls: [\"en\"], Ms: [["),
        "{script}"
    );
    assert!(script.contains("{ one: [[2], \" click\"] }"), "{script}");
}

#[test]
fn a_solid_export_is_a_project_whose_installs_survive_the_next_one() {
    let s = counter("solid");
    let out = viso(&s.0, &["export", "solid", "--json"]);
    let json = stdout(&out);
    assert_eq!(code(&out), 0, "{json}{}", stderr(&out));
    assert!(
        json.contains("\"kind\":\"export-dir\"")
            && json.contains("\"path\":\"web-solid\"")
            && json.contains("\"target\":\"solid\""),
        "{json}"
    );
    let component = s.read("web-solid/src/components/App.tsx");
    assert!(
        component.contains("export default function App("),
        "{component}"
    );
    assert!(component.contains("{\"Count\"}"), "{component}");
    assert!(
        s.read("web-solid/package.json")
            .contains("\"vite-plugin-solid\"")
    );

    // What the package manager and the bundler write stays.
    s.write("web-solid/node_modules/solid-js/package.json", "{}")
        .write("web-solid/package-lock.json", "{}")
        .write("web-solid/dist/index.html", "built");
    let again = viso(&s.0, &["export", "solid"]);
    assert_eq!(code(&again), 0, "{}", stderr(&again));
    assert!(stdout(&again).contains("next: cd web-solid && npm install && npm run dev"));
    assert_eq!(s.read("web-solid/node_modules/solid-js/package.json"), "{}");
    assert_eq!(s.read("web-solid/package-lock.json"), "{}");
    assert_eq!(s.read("web-solid/dist/index.html"), "built");

    // A generated file edited is the user's until `--force`.
    s.write("web-solid/src/App.tsx", "mine");
    let out = viso(&s.0, &["export", "solid"]);
    assert_eq!(code(&out), 6);
    assert!(stderr(&out).contains("`src/App.tsx`"), "{}", stderr(&out));
    assert_eq!(s.read("web-solid/src/App.tsx"), "mine");
}
