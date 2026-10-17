//! `viso new <path>`: a project from a template built into the CLI, so it
//! needs no network.
//!
//! The templates are `app` (a view, its state and a UI test), `game` (a
//! system on the fixed tick, its HUD and a game test), `web` (the app with
//! the browser's DOM as its first target) and `library` (components for
//! other apps, no binary). Each is a Cargo package depending on `viso` at
//! this CLI's version, or on the framework at `--framework`'s path (a
//! checkout, or its `crates/viso`), a `Viso.toml`, a message catalog its
//! views take their text from, an app's font in `assets/fonts/` (so its text
//! looks the same everywhere, headless snapshots included), and its tests:
//! as created it passes `viso check` and `viso test`.
//!
//! The package is named after the directory unless `--name` names it, and
//! the name must be one Cargo accepts. A directory that has files is
//! refused unless `--force-empty-dir`, and even then no file is replaced.
//! The files are written to a staging directory and moved into place once
//! every one is written, so a failure, or Ctrl-C, leaves nothing behind. The
//! directory becomes a Git repository unless `--no-git` says otherwise or it
//! lies in one already; without Git that is a warning.

use std::borrow::Cow;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use crate::args::{NewArgs, Template};
use crate::atomic;
use crate::context::Context;
use crate::error::{
    INTERRUPTED, NEW_FRAMEWORK, NEW_GIT, NEW_NAME_INVALID, NEW_PATH_TAKEN, NEW_WRITE, SUCCESS,
};
use crate::interrupt;
use crate::output::Output;

/// The version of `viso` a new package depends on: this CLI's, whose dev
/// protocol the framework speaks.
const VISO_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How many of a directory's entries a refusal lists.
const LISTED: usize = 8;

/// A file a template writes.
struct File {
    /// Where it goes, below the project root.
    path: &'static str,
    body: Body,
}

enum Body {
    /// Written as it is.
    Verbatim(&'static [u8]),
    /// Text naming the package, its edition, the framework it depends on and
    /// the language version it is written for: `{name}`, `{edition}`,
    /// `{viso}` (a TOML value), `{language}`.
    Filled(&'static str),
}

impl File {
    const fn verbatim(path: &'static str, text: &'static str) -> File {
        File {
            path,
            body: Body::Verbatim(text.as_bytes()),
        }
    }

    const fn binary(path: &'static str, bytes: &'static [u8]) -> File {
        File {
            path,
            body: Body::Verbatim(bytes),
        }
    }

    const fn filled(path: &'static str, text: &'static str) -> File {
        File {
            path,
            body: Body::Filled(text),
        }
    }
}

macro_rules! template {
    ($path:literal) => {
        include_str!(concat!("../../templates/", $path))
    };
}

macro_rules! template_bytes {
    ($path:literal) => {
        include_bytes!(concat!("../../templates/", $path))
    };
}

/// The face an app renders its text with, the same on every platform and in
/// headless snapshots, and its license.
const FONT: File = File::binary(
    "assets/fonts/NotoSans-Regular.ttf",
    template_bytes!("app/assets/fonts/NotoSans-Regular.ttf"),
);
const FONT_LICENSE: File = File::verbatim(
    "assets/fonts/OFL.txt",
    template!("app/assets/fonts/OFL.txt"),
);

const CARGO: File = File::filled("Cargo.toml", template!("app/Cargo.toml.in"));
const GITIGNORE: File = File::verbatim(".gitignore", template!("app/gitignore"));
const MAIN: File = File::verbatim("src/main.rs", template!("app/src/main.rs"));
const APP: File = File::verbatim("src/app.rs", template!("app/src/app.rs"));

const APP_FILES: &[File] = &[
    CARGO,
    File::filled("Viso.toml", template!("app/Viso.toml")),
    GITIGNORE,
    MAIN,
    APP,
    File::verbatim("src/app.vs", template!("app/src/app.vs")),
    File::verbatim("i18n/en.toml", template!("app/i18n/en.toml")),
    FONT,
    FONT_LICENSE,
    File::verbatim(
        "tests/ui/clicks.tape",
        template!("app/tests/ui/clicks.tape"),
    ),
];

const GAME_FILES: &[File] = &[
    CARGO,
    File::filled("Viso.toml", template!("game/Viso.toml")),
    GITIGNORE,
    MAIN,
    APP,
    File::verbatim("src/app.vs", template!("game/src/app.vs")),
    File::verbatim("i18n/en.toml", template!("game/i18n/en.toml")),
    FONT,
    FONT_LICENSE,
    File::verbatim(
        "tests/game/jump.tape",
        template!("game/tests/game/jump.tape"),
    ),
    File::verbatim("tests/ui/hint.tape", template!("game/tests/ui/hint.tape")),
];

const WEB_FILES: &[File] = &[
    CARGO,
    File::filled("Viso.toml", template!("web/Viso.toml")),
    GITIGNORE,
    MAIN,
    APP,
    File::verbatim("src/app.vs", template!("app/src/app.vs")),
    File::verbatim("i18n/en.toml", template!("app/i18n/en.toml")),
    FONT,
    FONT_LICENSE,
    File::verbatim(
        "tests/ui/clicks.tape",
        template!("app/tests/ui/clicks.tape"),
    ),
];

const LIBRARY_FILES: &[File] = &[
    CARGO,
    File::filled("Viso.toml", template!("app/Viso.toml")),
    GITIGNORE,
    File::verbatim("src/lib.rs", template!("library/src/lib.rs")),
    File::verbatim("src/counter.vs", template!("library/src/counter.vs")),
    File::verbatim("i18n/en.toml", template!("library/i18n/en.toml")),
];

/// The directories every project starts with, empty.
const DIRECTORIES: &[&str] = &["assets"];

fn files(template: Template) -> &'static [File] {
    match template {
        Template::App => APP_FILES,
        Template::Game => GAME_FILES,
        Template::Web => WEB_FILES,
        Template::Library => LIBRARY_FILES,
    }
}

/// The commands to run in a new project of `template`, first to last.
fn next(template: Template) -> &'static [&'static str] {
    match template {
        Template::App => &["viso run", "viso test"],
        Template::Game => &["viso run", "viso test game"],
        Template::Web => &["viso serve", "viso test"],
        Template::Library => &["viso check", "viso test"],
    }
}

pub fn run(ctx: &Context<'_>, args: &NewArgs, out: &mut Output) -> u8 {
    let cwd = match ctx.cwd(out) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let root = normalized(&cwd.join(&args.path));
    let given = args.name.is_some();
    let name = match &args.name {
        Some(name) => name.clone(),
        None => root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if let Some(why) = invalid_name(&name) {
        return name_failure(&name, &why, given, &args.path, out);
    }
    let viso = match &args.framework {
        Some(path) => match framework(&normalized(&cwd.join(path))) {
            Ok(dir) => format!(
                "{{ path = {} }}",
                toml_path(&relative(&resolved(&root), &resolved(&dir)))
            ),
            Err(why) => {
                return out.failure(
                    NEW_FRAMEWORK,
                    &format!("`{}` is not the framework: {why}", path.display()),
                    &["`--framework` names a Viso checkout or its `crates/viso`".to_owned()],
                );
            }
        },
        None => toml_string(VISO_VERSION),
    };
    let files = files(args.template);
    if let Err(code) = vacant(&root, files, args, out) {
        return code;
    }

    let filled = |file: &File| -> Cow<'static, [u8]> {
        match file.body {
            Body::Verbatim(bytes) => Cow::Borrowed(bytes),
            Body::Filled(text) => Cow::Owned(
                text.replace("{name}", &name)
                    .replace("{edition}", args.edition.as_str())
                    .replace("{viso}", &viso)
                    .replace("{language}", viso_dsl::frontend::LANGUAGE_VERSION)
                    .into_bytes(),
            ),
        }
    };
    out.progress(
        "new",
        &format!(
            "writing the {} template to {}",
            args.template.as_str(),
            root.display()
        ),
    );
    interrupt::catch();
    match create(&root, files, &filled) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::Interrupted => return INTERRUPTED,
        Err(error) => {
            return out.failure(
                NEW_WRITE,
                &format!("cannot write the project to `{}`: {error}", root.display()),
                &[],
            );
        }
    }
    if !args.no_git {
        git(&root, out);
    }

    let mut written: Vec<&str> = files.iter().map(|file| file.path).collect();
    written.sort_unstable();
    let here = root == cwd;
    out.result(
        |w| {
            w.begin_object();
            w.name("path");
            w.string(&root.display().to_string());
            w.name("template");
            w.string(args.template.as_str());
            w.name("files");
            w.begin_array();
            for path in &written {
                w.string(path);
            }
            w.end_array();
            w.end_object();
        },
        || {
            let mut text = if here {
                format!("Created {} `{name}` here\n", article(args.template))
            } else {
                format!(
                    "Created {} `{name}` in {}\n",
                    article(args.template),
                    args.path.display()
                )
            };
            text.push_str("\nNext:\n");
            if !here {
                text.push_str(&format!("  cd {}\n", shell_word(&args.path)));
            }
            for command in next(args.template) {
                text.push_str(&format!("  {command}\n"));
            }
            text
        },
    );
    SUCCESS
}

/// "the app", "the game", … for a sentence.
fn article(template: Template) -> &'static str {
    match template {
        Template::App => "the app",
        Template::Game => "the game",
        Template::Web => "the web app",
        Template::Library => "the library",
    }
}

/// `path` as a shell word: quoted when it has a character a shell reads.
fn shell_word(path: &Path) -> String {
    let text = path.display().to_string();
    if text
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        text
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

/// The directory of the `viso` crate `path` names: the crate itself, or the
/// checkout holding it as `crates/viso`; why not, when it names neither.
fn framework(path: &Path) -> Result<PathBuf, String> {
    let checkout = path.join("crates").join("viso");
    let mut why = None;
    for dir in [path, checkout.as_path()] {
        let manifest = dir.join("Cargo.toml");
        let text = match fs::read_to_string(&manifest) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("cannot read `{}`: {error}", manifest.display())),
        };
        let name = text
            .parse::<toml_edit::DocumentMut>()
            .ok()
            .and_then(|doc| doc.get("package")?.get("name")?.as_str().map(str::to_owned));
        match name {
            Some(name) if name == "viso" => return Ok(dir.to_path_buf()),
            Some(name) => why.get_or_insert(format!("its `Cargo.toml` is the package `{name}`")),
            None => why.get_or_insert("its `Cargo.toml` names no package".to_owned()),
        };
    }
    Err(why.unwrap_or_else(|| "it holds no `Cargo.toml`".to_owned()))
}

/// `path` with its links resolved: those of the longest part of it that
/// exists, the rest as it is. A relative path between two directories holds
/// only between their resolved forms, as `..` steps out of where a link
/// leads, not of the link.
fn resolved(path: &Path) -> PathBuf {
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return rest.iter().rev().fold(real, |path, part| path.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                existing = parent;
            }
            _ => return path.to_owned(),
        }
    }
}

/// `to` as a path from the directory `from`, both normalized and absolute;
/// `to` itself when they share no root.
fn relative(from: &Path, to: &Path) -> PathBuf {
    let (from, to): (Vec<_>, Vec<_>) = (from.components().collect(), to.components().collect());
    if from.first() != to.first() {
        return to.iter().collect();
    }
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut path: PathBuf = from[common..]
        .iter()
        .map(|_| Component::ParentDir)
        .collect();
    path.extend(&to[common..]);
    if path.as_os_str().is_empty() {
        path.push(".");
    }
    path
}

/// `path` as a TOML basic string, its separators written `/`, which Cargo
/// reads on every host.
fn toml_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    if cfg!(windows) {
        toml_string(&text.replace('\\', "/"))
    } else {
        toml_string(&text)
    }
}

/// `text` as a TOML basic string.
fn toml_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            c if c.is_control() => quoted.push_str(&format!("\\u{:04X}", c as u32)),
            c => quoted.push(c),
        }
    }
    quoted.push('"');
    quoted
}

/// `path` with `.` and `..` resolved by its components alone, so the name a
/// project takes is the one its path ends in.
fn normalized(path: &Path) -> PathBuf {
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            other => resolved.push(other),
        }
    }
    resolved
}

/// Why `name` is not a package name Cargo accepts and a registry publishes,
/// when it is not.
fn invalid_name(name: &str) -> Option<String> {
    const KEYWORDS: &[&str] = &[
        "abstract", "as", "async", "await", "become", "box", "break", "const", "continue", "crate",
        "do", "dyn", "else", "enum", "extern", "false", "final", "fn", "for", "gen", "if", "impl",
        "in", "let", "loop", "macro", "match", "mod", "move", "mut", "override", "priv", "pub",
        "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
        "typeof", "unsafe", "unsized", "use", "virtual", "where", "while", "yield",
    ];
    const STANDARD: &[&str] = &["alloc", "core", "proc_macro", "std", "test"];
    const DEVICES: &[&str] = &[
        "aux", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "con",
        "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9", "nul", "prn",
    ];
    if name.is_empty() {
        return Some("a package needs a name".to_owned());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '-' || *c == '_'))
    {
        return Some(format!(
            "`{}` is not allowed: a name is ASCII letters, digits, `-` and `_`",
            c.escape_debug()
        ));
    }
    if name.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        return Some("a name starts with a letter or `_`".to_owned());
    }
    if name.len() > 64 {
        return Some("a name is at most 64 characters".to_owned());
    }
    let crate_name = name.replace('-', "_").to_ascii_lowercase();
    if KEYWORDS.contains(&crate_name.as_str()) {
        return Some("it is a Rust keyword".to_owned());
    }
    if STANDARD.contains(&crate_name.as_str()) {
        return Some("it names a crate of Rust's standard library".to_owned());
    }
    if crate_name == "viso" {
        return Some("it is the framework's, which the package depends on".to_owned());
    }
    if DEVICES.contains(&crate_name.as_str()) {
        return Some("it is a device name Windows reserves".to_owned());
    }
    None
}

/// The usage failure of the invalid name `name`, with a name that would do
/// when `--name` did not give it.
fn name_failure(name: &str, why: &str, given: bool, path: &Path, out: &mut Output) -> u8 {
    let mut next = Vec::new();
    if !given {
        let fixed: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect();
        let fixed = fixed.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-');
        let fixed = [fixed.to_owned(), format!("{fixed}_app")]
            .into_iter()
            .find(|candidate| invalid_name(candidate).is_none())
            .unwrap_or_else(|| "my_app".to_owned());
        next.push(format!("viso new {} --name {fixed}", shell_word(path)));
    }
    let subject = if given {
        format!("`{name}` is not a package name")
    } else {
        format!("the directory's name, `{name}`, is not a package name")
    };
    out.failure_next(NEW_NAME_INVALID, &format!("{subject}: {why}"), &[], &next)
}

/// Fails unless the project can be created at `root`: a path that is not
/// there, an empty directory, or with `--force-empty-dir` a directory with
/// none of the files the project writes.
fn vacant(root: &Path, files: &[File], args: &NewArgs, out: &mut Output) -> Result<(), u8> {
    let shown = args.path.display();
    let meta = match fs::metadata(root) {
        Ok(meta) => meta,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return match root.ancestors().skip(1).find(|a| a.exists()) {
                Some(ancestor) if !ancestor.is_dir() => Err(out.failure(
                    NEW_PATH_TAKEN,
                    &format!("`{}` is a file, not a directory", ancestor.display()),
                    &[],
                )),
                _ => Ok(()),
            };
        }
        Err(error) => {
            return Err(out.failure(NEW_WRITE, &format!("cannot read `{shown}`: {error}"), &[]));
        }
    };
    if !meta.is_dir() {
        return Err(out.failure(
            NEW_PATH_TAKEN,
            &format!("`{shown}` is a file, not a directory"),
            &[],
        ));
    }
    let mut entries: Vec<String> = match fs::read_dir(root) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect(),
        Err(error) => {
            return Err(out.failure(NEW_WRITE, &format!("cannot read `{shown}`: {error}"), &[]));
        }
    };
    if entries.is_empty() {
        return Ok(());
    }
    entries.sort_unstable();
    if !args.force_empty_dir {
        let mut next = format!("viso new {} --force-empty-dir", shell_word(&args.path));
        if args.template != Template::App {
            next.push_str(&format!(" --template {}", args.template.as_str()));
        }
        return Err(out.failure_next(
            NEW_PATH_TAKEN,
            &format!("`{shown}` is not empty"),
            &[listed("it holds", &entries)],
            &[next],
        ));
    }
    // A file the project writes, or a file where a directory of its goes.
    let file_at = |path: &Path| fs::symlink_metadata(root.join(path)).is_ok_and(|m| !m.is_dir());
    let taken: Vec<String> = files
        .iter()
        .filter(|file| {
            let path = Path::new(file.path);
            fs::symlink_metadata(root.join(path)).is_ok()
                || path
                    .ancestors()
                    .skip(1)
                    .filter(|a| !a.as_os_str().is_empty())
                    .any(file_at)
        })
        .map(|file| file.path)
        .chain(
            DIRECTORIES
                .iter()
                .copied()
                .filter(|directory| file_at(Path::new(directory))),
        )
        .map(str::to_owned)
        .collect();
    if !taken.is_empty() {
        return Err(out.failure(
            NEW_PATH_TAKEN,
            &format!("`{shown}` holds files the project would write"),
            &[listed("they are", &taken)],
        ));
    }
    Ok(())
}

/// "`lead` `a`, `b` and 3 more".
fn listed(lead: &str, names: &[String]) -> String {
    let shown: Vec<String> = names
        .iter()
        .take(LISTED)
        .map(|n| format!("`{n}`"))
        .collect();
    let mut text = format!("{lead} {}", shown.join(", "));
    if names.len() > LISTED {
        text.push_str(&format!(" and {} more", names.len() - LISTED));
    }
    text
}

/// A file's bytes as the project has them.
type Fill<'f> = dyn Fn(&File) -> Cow<'static, [u8]> + 'f;

/// Writes `files` and [`DIRECTORIES`] to a staging directory and moves them
/// to `root`; on failure, or Ctrl-C (an `Interrupted` error), nothing it
/// made is left.
fn create(root: &Path, files: &[File], filled: &Fill<'_>) -> io::Result<()> {
    if root.exists() {
        // A directory that is there already, `.` say, stays the one it is:
        // the staging directory is inside it, and its files move up one by
        // one.
        let staging = root.join(format!(".viso-new-{}.tmp", std::process::id()));
        let staged = stage(&staging, files, filled);
        let moved = staged.and_then(|()| merge(&staging, root, files));
        let _ = fs::remove_dir_all(&staging);
        return moved;
    }
    // The first of its ancestors that is not there: what a failure removes.
    let top = root
        .ancestors()
        .take_while(|a| !a.exists())
        .last()
        .unwrap_or(root)
        .to_path_buf();
    let staging = atomic::sibling(root);
    let made = (|| {
        if let Some(parent) = root.parent() {
            fs::create_dir_all(parent)?;
        }
        stage(&staging, files, filled)?;
        interrupted()?;
        fs::rename(&staging, root)
    })();
    if made.is_err() {
        let _ = fs::remove_dir_all(&staging);
        if top != root {
            let _ = fs::remove_dir_all(&top);
        }
    }
    made
}

/// An `Interrupted` error once Ctrl-C has come.
fn interrupted() -> io::Result<()> {
    if interrupt::count() > 0 {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
    }
    Ok(())
}

/// Writes the project to `staging`, made afresh.
fn stage(staging: &Path, files: &[File], filled: &Fill<'_>) -> io::Result<()> {
    let _ = fs::remove_dir_all(staging);
    fs::create_dir(staging)?;
    for directory in DIRECTORIES {
        fs::create_dir_all(staging.join(directory))?;
    }
    for file in files {
        interrupted()?;
        let path = staging.join(file.path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, filled(file))?;
    }
    Ok(())
}

/// Moves the staged project into `root`, which is there already, file by
/// file; on failure, or Ctrl-C, what was moved is removed again.
fn merge(staging: &Path, root: &Path, files: &[File]) -> io::Result<()> {
    let mut made: Vec<PathBuf> = Vec::new();
    let moved = (|| {
        for directory in DIRECTORIES {
            let path = root.join(directory);
            if !path.is_dir() {
                fs::create_dir(&path)?;
                made.push(path);
            }
        }
        for file in files {
            interrupted()?;
            let to = root.join(file.path);
            let mut missing: Vec<PathBuf> = to
                .ancestors()
                .skip(1)
                .take_while(|a| !a.exists())
                .map(Path::to_path_buf)
                .collect();
            missing.reverse();
            for directory in missing {
                fs::create_dir(&directory)?;
                made.push(directory);
            }
            fs::rename(staging.join(file.path), &to)?;
            made.push(to);
        }
        Ok(())
    })();
    if moved.is_err() {
        // Deepest first: each file before the directories made for it.
        for path in made.iter().rev() {
            let _ = if path.is_dir() {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            };
        }
    }
    moved
}

/// Makes `root` a Git repository unless it lies in one; a warning when Git
/// cannot.
fn git(root: &Path, out: &mut Output) {
    use crate::process::{Failure, QUICK, capture, status};

    let inside = status(
        Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(root),
        QUICK,
    );
    let skip = ["`--no-git` creates the project without one".to_owned()];
    match inside {
        Err(Failure::NotFound(_)) => {
            out.warning(
                NEW_GIT,
                "Git is not installed, so the project is not a repository",
                &skip,
            );
            return;
        }
        Err(error) => {
            out.warning(NEW_GIT, &error.to_string(), &skip);
            return;
        }
        // Its files are the enclosing repository's.
        Ok(status) if status.success() => return,
        Ok(_) => {}
    }
    out.progress("git", "git init");
    match capture(
        Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root),
        Some(QUICK),
    ) {
        Ok(output) if output.status.success() => {}
        Ok(output) => out.warning(
            NEW_GIT,
            &format!("`git init` failed: {}", output.said()),
            &skip,
        ),
        Err(error) => out.warning(NEW_GIT, &error.to_string(), &skip),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_framework_path_is_related_through_links() {
        let base = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("viso-new-links-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("real/projects")).unwrap();
        fs::create_dir_all(base.join("framework")).unwrap();
        std::os::unix::fs::symlink(base.join("real/projects"), base.join("link")).unwrap();
        let root = base.join("link/app");
        assert_eq!(resolved(&root), base.join("real/projects/app"));
        assert_eq!(
            relative(&resolved(&root), &resolved(&base.join("framework"))),
            PathBuf::from("../../../framework")
        );
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn package_names_follow_cargo() {
        for name in ["app", "my_app", "my-app", "_app", "a1", "Viso2"] {
            assert_eq!(invalid_name(name), None, "{name}");
        }
        for name in [
            "", "1app", "-app", "my app", "app!", "fn", "Self", "std", "viso", "con", "café",
        ] {
            assert!(invalid_name(name).is_some(), "{name}");
        }
        assert!(invalid_name(&"a".repeat(65)).is_some());
    }

    #[test]
    fn a_path_resolves_by_its_components() {
        assert_eq!(normalized(Path::new("/a/b/../c/./d")), Path::new("/a/c/d"));
        assert_eq!(
            normalized(&Path::new("/a/b").join("."))
                .file_name()
                .unwrap(),
            "b"
        );
    }

    #[test]
    fn every_template_has_a_manifest_a_configuration_and_its_tests() {
        for template in [
            Template::App,
            Template::Game,
            Template::Web,
            Template::Library,
        ] {
            let paths: Vec<&str> = files(template).iter().map(|f| f.path).collect();
            for needed in ["Cargo.toml", "Viso.toml", ".gitignore", "i18n/en.toml"] {
                assert!(paths.contains(&needed), "{template:?}: {needed}");
            }
            let binary = paths.contains(&"src/main.rs");
            assert_eq!(binary, template != Template::Library, "{template:?}");
            if binary {
                assert!(
                    paths.iter().any(|p| p.starts_with("tests/")),
                    "{template:?}"
                );
            }
            for file in files(template) {
                let filled = match file.body {
                    Body::Filled(text) => text.contains("{name}"),
                    Body::Verbatim(bytes) => bytes.windows(6).any(|w| w == b"{name}"),
                };
                assert_eq!(
                    filled,
                    matches!(file.body, Body::Filled(_)),
                    "{}",
                    file.path
                );
            }
            if binary {
                assert!(
                    paths.iter().any(|p| p.starts_with("assets/fonts/")),
                    "{template:?}"
                );
            }
        }
    }
}
