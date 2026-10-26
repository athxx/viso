//! The code editor on a `.vs` file.
//!
//! `cargo run -p viso-example-code-editor -- [FILE] [--lines N] [--font PATH]`
//! opens FILE (a sample when none is given) in an editor filling the window,
//! its language told by its extension. `--lines N` opens a generated file of
//! N lines instead, to feel how a large one scrolls and edits. `--font PATH`
//! adds the font file at PATH to the app's faces, which a headless run, with
//! no system faces, needs to draw any text. The primary modifier with S
//! saves the file back.
//!
//! When a `viso-lsp` is found — `VISO_LSP`, beside this program, or on the
//! path — it is the editor's language server: diagnostics squiggle under the
//! text, a pointer resting on one says what is wrong, and F12 or the
//! primary modifier's click goes to where a name is declared.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso::prelude::*;
use viso::ui::{Axis, Component, Key, Size};
use viso_code_editor::service::LanguageService;
use viso_code_editor::{EditorSlot, code_editor};

const SAMPLE: &str = include_str!("../../../crates/dsl/tests/native/colony.vs");

struct Editor {
    path: Option<PathBuf>,
    text: String,
    slot: EditorSlot,
}

impl Application for Editor {
    fn new(cx: &mut AppCx) -> Self {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let option = |name: &str| {
            args.iter()
                .position(|a| a == name)
                .and_then(|i| args.get(i + 1))
        };
        let lines = option("--lines").and_then(|n| n.parse::<usize>().ok());
        if let Some(font) = option("--font")
            && let Err(e) = cx.load_font_file(font)
        {
            eprintln!("{font}: {e}");
        }
        let path = args
            .iter()
            .enumerate()
            .find(|(i, a)| !a.starts_with("--") && (*i == 0 || !args[i - 1].starts_with("--")))
            .map(|(_, a)| PathBuf::from(a));
        let text = match (&path, lines) {
            (_, Some(n)) => generated(n),
            (Some(p), None) => std::fs::read_to_string(p).unwrap_or_else(|e| {
                eprintln!("{}: {e}", p.display());
                String::new()
            }),
            (None, None) => SAMPLE.to_owned(),
        };
        Editor {
            path: if lines.is_some() { None } else { path },
            text,
            slot: EditorSlot::default(),
        }
    }

    fn window_config(&self) -> WindowConfig {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_name())
            .map_or("colony.vs".into(), |n| n.to_string_lossy().into_owned());
        WindowConfig {
            title: format!("{name} — Code editor"),
            size: (960.0, 680.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let file = self
            .path
            .clone()
            .unwrap_or_else(|| PathBuf::from("colony.vs"));
        let mut editor = code_editor(self.text.clone())
            .language(viso_code_editor::lang::for_path(&file.to_string_lossy()))
            .size(Size::fill())
            .label(file.to_string_lossy())
            .handle(&self.slot);
        if file.extension().is_some_and(|e| e == "vs")
            && let Some(service) = language_server(&file, &self.text)
        {
            editor = editor.service(service);
        }
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                size: Size::fill(),
                ..Default::default()
            },
            |cx| editor.build(cx),
        );
        // The keys the editor leaves: saving.
        let slot = self.slot.clone();
        let path = self.path.clone();
        cx.on_key(root, move |ev| {
            let Some(k) = ev.key().copied() else {
                return;
            };
            let primary = if viso::ui::primary_is_logo() {
                k.modifiers.logo
            } else {
                k.modifiers.control
            };
            if k.pressed && primary && k.key == Key::S {
                if let (Some(path), Some(h)) = (&path, slot.borrow().as_ref()) {
                    match std::fs::write(path, h.text()) {
                        Ok(()) => eprintln!("saved {}", path.display()),
                        Err(e) => eprintln!("{}: {e}", path.display()),
                    }
                }
                ev.stop_propagation();
            }
        });
    }
}

/// `viso-lsp` serving `file`, if one is found.
#[cfg(not(any(target_arch = "wasm32", target_os = "ios", target_os = "android")))]
fn language_server(file: &Path, text: &str) -> Option<Rc<dyn LanguageService>> {
    use viso_code_editor::lsp::{LspClient, OpenDocument, file_uri};
    let exe = std::env::var_os("VISO_LSP")
        .map(PathBuf::from)
        .or_else(|| {
            let beside = std::env::current_exe().ok()?.with_file_name("viso-lsp");
            beside.is_file().then_some(beside)
        })
        .unwrap_or_else(|| PathBuf::from("viso-lsp"));
    let uri = file_uri(file);
    let root = file.parent().map(file_uri);
    match LspClient::spawn(
        &exe,
        &["--stdio"],
        OpenDocument {
            root: root.as_deref(),
            uri: &uri,
            language: "viso",
            text,
        },
    ) {
        Ok(client) => Some(Rc::new(client)),
        Err(e) => {
            eprintln!("no language server ({}): {e}", exe.display());
            None
        }
    }
}

/// No language server where programs cannot start one.
#[cfg(any(target_arch = "wasm32", target_os = "ios", target_os = "android"))]
fn language_server(_: &Path, _: &str) -> Option<Rc<dyn LanguageService>> {
    None
}

/// `n` lines of `.vs`: the sample over and over.
fn generated(n: usize) -> String {
    let lines: Vec<&str> = SAMPLE.lines().collect();
    let mut out = String::with_capacity(n * 40);
    for i in 0..n {
        out.push_str(lines[i % lines.len()]);
        out.push('\n');
    }
    out
}

fn main() {
    viso::run::<Editor>();
}
