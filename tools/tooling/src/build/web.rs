//! The web bundle of a build, as its profile's web settings shape it: the
//! glue `wasm-bindgen` generates, keeping the module's debug info for source
//! maps or removing its name and producers sections for a stripped build;
//! the module optimized by `wasm-opt` for speed or size, then split into a
//! startup and a lazy part when the profile asks for it ([`super::split`]);
//! the module and its glue named by their content (`<bin>_bg.<hash>.wasm`, `<bin>.<hash>.js`)
//! so a server can cache them forever, and the page that loads them, which
//! keeps its name; and a Brotli and a gzip copy of each file beside it, for a
//! server to hand out precompressed.
//!
//! `wasm-opt` is optional where the profile can ship without it: missing, a
//! dev or release bundle stays unoptimized with a warning, and a shipping
//! build fails.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use viso_dsl::export::DomPage;
use viso_package::web as bundle;
use viso_project::{Auto, Profile, WebOptimize, WebSettings};

use super::cargo::{BuildCommand, BuildFailure, CargoEvent, Sink, is_cancelled};
use crate::process::ProcessTree;

/// How `wasm-opt` is installed, for a message about its absence.
const WASM_OPT_INSTALL: &str = "install Binaryen (`brew install binaryen`, or its release from github.com/WebAssembly/binaryen)";

/// The WebAssembly features Rust's `wasm32-unknown-unknown` emits by
/// default, which the optimized module may keep using.
pub(super) const FEATURES: &[&str] = &[
    "--enable-bulk-memory",
    "--enable-mutable-globals",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-reference-types",
    "--enable-multivalue",
];

/// What a threaded module is compiled with: atomics, a shared memory the
/// glue creates and hands every thread (at most 1 GiB), and the thread-local
/// storage `wasm-bindgen` sets up for each.
const THREADED_RUSTFLAGS: &[&str] = &[
    "-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals",
    "-Clink-arg=--shared-memory",
    "-Clink-arg=--import-memory",
    "-Clink-arg=--max-memory=1073741824",
    "-Clink-arg=--export=__wasm_init_tls",
    "-Clink-arg=--export=__tls_size",
    "-Clink-arg=--export=__tls_align",
    "-Clink-arg=--export=__tls_base",
];

/// How the toolchain a threaded build needs is installed.
const NIGHTLY_INSTALL: &str =
    "rustup toolchain install nightly --component rust-src --target wasm32-unknown-unknown";

/// Whether a web build with `web` is threaded: `threads = "on"`. `auto` is
/// off, as a shared memory needs its page isolated across origins, which
/// its server must do.
pub fn threaded(web: &WebSettings) -> bool {
    web.threads.value == Auto::On
}

/// `command`, a web build, made to build a shared-memory threaded module:
/// the standard library rebuilt with atomics, on the nightly toolchain that
/// can (the one `RUSTUP_TOOLCHAIN` names when it is a nightly).
///
/// # Errors
///
/// No nightly toolchain with the standard library's source.
pub fn thread(command: &mut BuildCommand) -> Result<(), BuildFailure> {
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN")
        .ok()
        .filter(|name| name.starts_with("nightly"))
        .unwrap_or_else(|| "nightly".to_owned());
    let missing = |why: &str| {
        BuildFailure::Tool(format!(
            "a threaded web build needs {why} (`{NIGHTLY_INSTALL}`)"
        ))
    };
    let sysroot = crate::process::capture(
        Command::new("rustc")
            .env("RUSTUP_TOOLCHAIN", &toolchain)
            .args(["--print", "sysroot"]),
        Some(crate::process::QUICK),
    )
    .ok()
    .filter(|run| run.status.success())
    .map(|run| PathBuf::from(run.stdout.trim()))
    .ok_or_else(|| missing(&format!("the `{toolchain}` toolchain")))?;
    if !sysroot.join("lib/rustlib/src/rust/library/std").is_dir() {
        return Err(missing(&format!(
            "the `{toolchain}` toolchain's `rust-src`"
        )));
    }
    // The proxies, so the toolchain they run is the one named here, not the
    // one a `cargo` this runs under pinned.
    command.cargo = PathBuf::from("cargo");
    command.env.extend([
        ("RUSTUP_TOOLCHAIN".to_owned(), toolchain.into()),
        ("RUSTC".to_owned(), "rustc".into()),
    ]);
    command
        .build_args
        .extend(["-Z".into(), "build-std=panic_abort,std".into()]);
    // Flags from the environment replace every configured ones, so these
    // join them there; otherwise they join the configured ones.
    if let Ok(encoded) = std::env::var("CARGO_ENCODED_RUSTFLAGS") {
        let mut flags: Vec<&str> = encoded.split('\x1f').filter(|f| !f.is_empty()).collect();
        flags.extend(THREADED_RUSTFLAGS);
        command.env.push((
            "CARGO_ENCODED_RUSTFLAGS".to_owned(),
            flags.join("\x1f").into(),
        ));
    } else if let Ok(flags) = std::env::var("RUSTFLAGS") {
        let joined = format!("{flags} {}", THREADED_RUSTFLAGS.join(" "));
        command.env.push(("RUSTFLAGS".to_owned(), joined.into()));
    } else {
        let flags: Vec<String> = THREADED_RUSTFLAGS
            .iter()
            .map(|f| format!("{f:?}"))
            .collect();
        command.build_args.extend([
            "--config".into(),
            format!("target.{}.rustflags=[{}]", bundle::TRIPLE, flags.join(",")).into(),
        ]);
    }
    Ok(())
}

/// Brotli at its densest: quality 11, a 16 MiB window.
const BROTLI_QUALITY: u32 = 11;
const BROTLI_WINDOW: u32 = 24;

/// The file kinds of a bundle, by extension.
fn kind(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "wasm" => Some("wasm"),
        "js" => Some("js"),
        "html" => Some("html"),
        "css" => Some("css"),
        "vsb" => Some("view"),
        "br" => Some("brotli"),
        "gz" => Some("gzip"),
        _ => None,
    }
}

/// A `web-dom` build's page, and the project's `assets/` its assets are
/// read from.
#[derive(Debug, Clone)]
pub struct WebPage {
    pub page: DomPage,
    pub assets: PathBuf,
}

/// Writes the bundle of the module `wasm`, the app `bin`, into `dir`, which
/// is empty, with the service worker `worker` when it has one, and the page
/// `page` of a `web-dom` build in place of the canvas page; each file it
/// wrote with its kind, by name.
#[allow(clippy::too_many_arguments)]
pub fn bundle(
    wasm: &Path,
    dir: &Path,
    bin: &str,
    profile: Profile,
    web: &WebSettings,
    worker: Option<&str>,
    page: Option<&WebPage>,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Vec<(&'static str, PathBuf)>, BuildFailure> {
    let mut bindgen = bundle::bindgen_command(wasm, dir, bin);
    if web.source_maps.value {
        bindgen.arg("--keep-debug");
    }
    if web.strip.value {
        bindgen.args(["--remove-name-section", "--remove-producers-section"]);
    }
    tool(
        &mut bindgen,
        "wasm-bindgen",
        bundle::BINDGEN_INSTALL,
        cancelled,
    )?;
    bundle::thread_glue(dir, bin).map_err(BuildFailure::Package)?;

    let module = dir.join(format!("{bin}_bg.wasm"));
    if let Some(level) = match web.optimize.value {
        WebOptimize::None => None,
        WebOptimize::Speed => Some("-O3"),
        WebOptimize::Size => Some("-Oz"),
    } {
        let optimized = dir.join(format!("{bin}_bg.opt.wasm"));
        let mut opt = Command::new("wasm-opt");
        opt.arg(level).args(FEATURES);
        if threaded(web) {
            opt.arg("--enable-threads");
        }
        // Names and debug info survive only when asked to.
        if web.source_maps.value || !web.strip.value {
            opt.arg("-g");
        }
        opt.arg(&module).arg("-o").arg(&optimized);
        match tool(&mut opt, "wasm-opt", WASM_OPT_INSTALL, cancelled) {
            Ok(()) => std::fs::rename(&optimized, &module).map_err(|error| {
                BuildFailure::Package(format!("cannot replace `{}`: {error}", module.display()))
            })?,
            Err(BuildFailure::Spawn(why)) if profile != Profile::Shipping => {
                sink(CargoEvent::Log {
                    level: "warn",
                    line: format!("{why}; the module is not optimized"),
                });
            }
            Err(BuildFailure::Spawn(why)) => return Err(BuildFailure::Tool(why)),
            Err(failure) => return Err(failure),
        }
    }

    let lazy = super::split::split(dir, bin, page, profile, web, threaded(web), cancelled, sink)?;
    fingerprint(dir, bin, worker, page, profile, lazy)?;

    let mut files = listed(dir)?;
    let compress: Vec<(&str, bool)> = vec![("br", web.brotli.value), ("gz", web.gzip.value)];
    if compress.iter().any(|(_, on)| *on) {
        if is_cancelled(cancelled) {
            return Err(BuildFailure::Cancelled);
        }
        precompress(&files, &compress)?;
        files = listed(dir)?;
    }
    Ok(files)
}

/// Digits of a file's SHA-256 its fingerprinted name carries.
const FINGERPRINT: usize = 16;

/// Renames the module and the glue in `dir` by their content and writes the
/// page loading them under those names — the canvas page, or the DOM page
/// `page` with its stylesheet, its script, its assets and, in a dev build,
/// its module, each named by its content too — and the service worker
/// `worker`, which keeps its name, as a browser updates a worker at its URL.
/// A split module's lazy part is named by its content too, and the glue
/// loads it.
fn fingerprint(
    dir: &Path,
    bin: &str,
    worker: Option<&str>,
    page: Option<&WebPage>,
    profile: Profile,
    lazy: Option<bundle::lazy::Lazy>,
) -> Result<(), BuildFailure> {
    let rename = |stem: &str, extension: &str| -> Result<String, BuildFailure> {
        let from = dir.join(format!("{stem}.{extension}"));
        let (_, sum) = super::file_hash(&from).map_err(|error| {
            BuildFailure::Package(format!("cannot read `{}`: {error}", from.display()))
        })?;
        let digest: String = sum.iter().map(|b| format!("{b:02x}")).collect();
        let name = format!("{stem}.{}.{extension}", &digest[..FINGERPRINT]);
        std::fs::rename(&from, dir.join(&name)).map_err(|error| {
            BuildFailure::Package(format!("cannot rename `{}`: {error}", from.display()))
        })?;
        Ok(name)
    };
    let module = rename(&format!("{bin}_bg"), "wasm")?;
    if let Some(lazy) = lazy {
        let lazy = bundle::lazy::Lazy {
            module: rename(&format!("{bin}_lazy"), "wasm")?,
            ..lazy
        };
        let path = dir.join(format!("{bin}.js"));
        let glue = std::fs::read_to_string(&path).map_err(|error| {
            BuildFailure::Package(format!("cannot read `{}`: {error}", path.display()))
        })?;
        let patched = bundle::lazy::glue(&glue, &lazy).map_err(BuildFailure::Package)?;
        std::fs::write(&path, patched).map_err(|error| {
            BuildFailure::Package(format!("cannot write `{}`: {error}", path.display()))
        })?;
    }
    let glue = rename(bin, "js")?;
    match page {
        Some(page) => write_dom_page(dir, &glue, &module, worker, page, profile),
        None => bundle::write_page(dir, bin, &glue, Some(&module), worker)
            .map(drop)
            .map_err(BuildFailure::Package),
    }
}

/// Writes the DOM page of `page` in `dir`, its app the glue `glue` loading
/// the module `module`, in place of the page written there before, whose
/// files go once the new `index.html` is in place. A dev page carries the
/// view's module, which the app runs in place of its own: a `.vs` edit then
/// needs no new app. A page with an island does not: the app draws the
/// island from the view it was built with.
///
/// # Errors
///
/// A file that cannot be written.
pub fn write_dom_page(
    dir: &Path,
    glue: &str,
    module: &str,
    worker: Option<&str>,
    page: &WebPage,
    profile: Profile,
) -> Result<(), BuildFailure> {
    let failed = |path: &Path, error: std::io::Error| {
        BuildFailure::Package(format!("cannot write `{}`: {error}", path.display()))
    };
    let named = |stem: &str, extension: &str, bytes: &[u8]| -> Result<String, BuildFailure> {
        use sha2::Digest as _;
        let digest: String = sha2::Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = format!("{stem}.{}.{extension}", &digest[..FINGERPRINT]);
        let path = dir.join(&name);
        std::fs::write(&path, bytes).map_err(|e| failed(&path, e))?;
        Ok(name)
    };
    let dom = &page.page;
    let styles = named("styles", "css", dom.styles.as_bytes())?;
    let script = named("page", "js", dom.script.as_bytes())?;
    let view_name = match profile {
        Profile::Dev if !dom.island => Some(named("view", "vsb", &dom.module)?),
        Profile::Dev | Profile::Release | Profile::Shipping => None,
    };
    let view = view_name.as_ref().map_or_else(
        || "undefined".to_owned(),
        |name| format!("new Uint8Array(await (await fetch(\"./{name}\")).arrayBuffer())"),
    );
    for asset in &dom.assets {
        let (from, to) = (page.assets.join(asset), dir.join("assets").join(asset));
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed(parent, e))?;
        }
        std::fs::copy(&from, &to).map_err(|e| failed(&to, e))?;
    }
    // The glue too, which an app named `page` names as a page script.
    let glue_name = glue.to_owned();
    let written = [
        Some(&styles),
        Some(&script),
        view_name.as_ref(),
        Some(&glue_name),
    ];
    let mut boot = String::new();
    if let Some(worker) = worker {
        let path = dir.join(bundle::WORKER);
        std::fs::write(&path, worker).map_err(|e| failed(&path, e))?;
        boot.push_str(&format!(
            "<script>navigator.serviceWorker?.register(\"./{}\").catch((e) => console.warn(\"the service worker did not register:\", e));</script>\n",
            bundle::WORKER
        ));
    }
    boot.push_str(&format!(
        "<script type=\"module\">import init, {{ visoDom }} from \"./{glue}\"; await init({{ module_or_path: \"./{module}\" }}); visoPage(await visoDom(\"{}\", {view}, {}n));</script>\n",
        dom.component, dom.build
    ));
    // A page loading now reads the old `index.html` or the new one whole.
    let (index, next) = (dir.join("index.html"), dir.join("index.html.next"));
    std::fs::write(&next, dom.index(&styles, &script, &boot)).map_err(|e| failed(&next, e))?;
    std::fs::rename(&next, &index).map_err(|e| failed(&index, e))?;
    let stale = |name: &str| {
        [("styles.", ".css"), ("page.", ".js"), ("view.", ".vsb")]
            .iter()
            .any(|(stem, extension)| name.starts_with(stem) && name.ends_with(extension))
            && !written.iter().flatten().any(|w| *w == name)
    };
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_str().is_some_and(stale) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}

/// The bundle files in `dir`, each with its kind, by name.
fn listed(dir: &Path) -> Result<Vec<(&'static str, PathBuf)>, BuildFailure> {
    let entries = std::fs::read_dir(dir).map_err(|error| {
        BuildFailure::Package(format!("cannot read `{}`: {error}", dir.display()))
    })?;
    let mut files: Vec<(&'static str, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .filter_map(|path| Some((kind(&path)?, path)))
        .collect();
    files.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(files)
}

/// Writes `<file>.br` and `<file>.gz` beside each file, as `compress` says,
/// every file and form on its own thread.
fn precompress(
    files: &[(&'static str, PathBuf)],
    compress: &[(&str, bool)],
) -> Result<(), BuildFailure> {
    thread::scope(|scope| {
        let jobs: Vec<_> = files
            .iter()
            .flat_map(|(_, path)| {
                compress
                    .iter()
                    .filter(|(_, on)| *on)
                    .map(move |(extension, _)| (path, *extension))
            })
            .map(|(path, extension)| {
                scope.spawn(move || {
                    let mut name = path.as_os_str().to_owned();
                    name.push(".");
                    name.push(extension);
                    let out = PathBuf::from(name);
                    compressed(path, &out, extension).map_err(|error| {
                        BuildFailure::Package(format!("cannot write `{}`: {error}", out.display()))
                    })
                })
            })
            .collect();
        jobs.into_iter()
            .try_for_each(|job| job.join().expect("a compression thread"))
    })
}

/// Writes `path` compressed as `extension` says to `out`.
fn compressed(path: &Path, out: &Path, extension: &str) -> std::io::Result<()> {
    let mut input = BufReader::new(File::open(path)?);
    let output = BufWriter::new(File::create(out)?);
    if extension == "br" {
        let mut writer =
            brotli::CompressorWriter::new(output, 1 << 16, BROTLI_QUALITY, BROTLI_WINDOW);
        std::io::copy(&mut input, &mut writer)?;
        writer.flush()?;
        writer
            .into_inner()
            .into_inner()
            .map(drop)
            .map_err(|e| e.into_error())
    } else {
        let mut writer = flate2::write::GzEncoder::new(output, flate2::Compression::best());
        std::io::copy(&mut input, &mut writer)?;
        writer
            .finish()?
            .into_inner()
            .map(drop)
            .map_err(|e| e.into_error())
    }
}

/// Runs the tool `name` until it ends or `cancelled` fires: not installed is
/// [`BuildFailure::Spawn`] with how to install it (`install`), a failure
/// [`BuildFailure::Package`] with what it wrote.
pub(super) fn tool(
    command: &mut Command,
    name: &str,
    install: &str,
    cancelled: &mpsc::Receiver<()>,
) -> Result<(), BuildFailure> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut tree = match ProcessTree::spawn(command) {
        Ok(tree) => tree,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(BuildFailure::Spawn(format!(
                "`{name}` is not installed ({install})"
            )));
        }
        Err(error) => return Err(BuildFailure::Spawn(format!("cannot run `{name}`: {error}"))),
    };
    let outputs: Vec<_> = [
        tree.child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
        tree.child
            .stderr
            .take()
            .map(|e| Box::new(e) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|mut output| {
        thread::spawn(move || {
            let mut text = String::new();
            let _ = output.read_to_string(&mut text);
            text
        })
    })
    .collect();
    let status = loop {
        if is_cancelled(cancelled) {
            tree.kill();
            return Err(BuildFailure::Cancelled);
        }
        match tree.child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(error) => {
                tree.kill();
                return Err(BuildFailure::Spawn(format!(
                    "cannot wait for `{name}`: {error}"
                )));
            }
        }
    };
    let said: Vec<String> = outputs
        .into_iter()
        .filter_map(|thread| thread.join().ok())
        .collect();
    if status.success() {
        return Ok(());
    }
    let said = said
        .iter()
        .rev()
        .find(|text| !text.trim().is_empty())
        .map_or("", |text| text.trim());
    Err(BuildFailure::Package(format!(
        "`{name}` failed ({status}): {said}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_files_are_known_by_extension() {
        assert_eq!(kind(Path::new("app_bg.wasm")), Some("wasm"));
        assert_eq!(kind(Path::new("app.js.br")), Some("brotli"));
        assert_eq!(kind(Path::new("index.html.gz")), Some("gzip"));
        assert_eq!(kind(Path::new("notes.txt")), None);
    }

    #[test]
    fn a_split_modules_lazy_part_is_named_by_its_content_and_the_glue_loads_it() {
        let dir = std::env::temp_dir().join(format!("viso-web-lazy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("app_bg.wasm"), "\0asm primary").unwrap();
        std::fs::write(dir.join("app_lazy.wasm"), "\0asm lazy").unwrap();
        std::fs::write(
            dir.join("app.js"),
            "function __wbg_get_imports() {\n    return {};\n}\nlet wasm;\nfunction __wbg_finalize_init(instance) {\n    wasm = instance.exports;\n    wasm.__wbindgen_start();\n}\n",
        )
        .unwrap();
        let lazy = bundle::lazy::Lazy {
            module: "app_lazy.wasm".to_owned(),
            namespace: "lazy.deferred".to_owned(),
            table: "%a".to_owned(),
        };
        fingerprint(&dir, "app", None, None, Profile::Shipping, Some(lazy)).unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let named = |prefix: &str, ext: &str| {
            names
                .iter()
                .find(|n| n.starts_with(prefix) && n.ends_with(ext))
                .unwrap_or_else(|| panic!("{prefix}*{ext} in {names:?}"))
                .clone()
        };
        let (lazy, glue) = (named("app_lazy.", ".wasm"), named("app.", ".js"));
        assert_eq!(lazy.len(), "app_lazy..wasm".len() + FINGERPRINT);
        let glue = std::fs::read_to_string(dir.join(glue)).unwrap();
        assert!(glue.contains(&format!("new URL(\"./{lazy}\", import.meta.url)")));
        assert!(glue.contains("__visoLazy.ready(wasm);"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_module_and_its_glue_are_named_by_their_content_and_the_page_by_none() {
        let dir = std::env::temp_dir().join(format!("viso-web-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |wasm: &str| {
            std::fs::write(dir.join("app_bg.wasm"), wasm).unwrap();
            std::fs::write(dir.join("app.js"), "export default function init() {}").unwrap();
            fingerprint(&dir, "app", None, None, Profile::Release, None).unwrap();
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            for name in &names {
                if name != "index.html" {
                    std::fs::remove_file(dir.join(name)).unwrap();
                }
            }
            names
        };
        let first = write("\0asm one");
        let [glue, module, index] = &first[..] else {
            panic!("{first:?}");
        };
        assert_eq!(index, "index.html");
        assert!(glue.starts_with("app.") && glue.len() == "app..js".len() + FINGERPRINT);
        assert!(module.starts_with("app_bg.") && module.ends_with(".wasm"));
        let page = std::fs::read_to_string(dir.join("index.html")).unwrap();
        assert!(page.contains(&format!("./{glue}")) && page.contains(&format!("./{module}")));

        // A changed module is a new name; its unchanged glue keeps its own.
        let second = write("\0asm two");
        assert_eq!(second[0], first[0]);
        assert_ne!(second[1], first[1]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The `web-dom` page of a component showing `text`.
    fn dom_page(text: &str) -> WebPage {
        let source = format!(
            "export component App {{ state n = 0; view {{ Column {{ Text(\"{text}\"); Button(\"+\") {{ on click {{ n += 1; }} }} }} }} }}"
        );
        let origin = viso_dsl::frontend::Origin {
            package: "app".into(),
            module: vec!["app".into()],
            ..Default::default()
        };
        let compiled = viso_dsl::frontend::compile_file(&source, &origin);
        let options = viso_dsl::export::DomOptions {
            title: "app".into(),
            lang: "en".into(),
        };
        WebPage {
            page: viso_dsl::export::dom(&compiled, &options).unwrap(),
            assets: PathBuf::new(),
        }
    }

    #[test]
    fn a_dom_page_written_again_replaces_the_one_before() {
        let dir = std::env::temp_dir().join(format!("viso-web-repage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names = || {
            let mut names: Vec<String> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        std::fs::write(dir.join("page.js"), "glue").unwrap();
        let write = |text: &str| {
            write_dom_page(
                &dir,
                "page.js",
                "page_bg.wasm",
                None,
                &dom_page(text),
                Profile::Dev,
            )
            .unwrap();
            std::fs::read_to_string(dir.join("index.html")).unwrap()
        };
        let first = write("one");
        assert!(first.contains(">one<"), "{first}");
        let before = names();
        let second = write("two");
        assert!(
            second.contains(">two<") && !second.contains(">one<"),
            "{second}"
        );
        let after = names();
        // Each file the page loads is in place, and none the first one did;
        // the glue, named as a page script, stays.
        assert_eq!(after.len(), before.len(), "{after:?}");
        for name in &after {
            assert!(name == "index.html" || name == "page.js" || second.contains(name.as_str()));
        }
        assert!(
            before
                .iter()
                .any(|name| name.starts_with("view.") && !after.contains(name))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn precompressed_copies_decompress_to_the_file() {
        let dir = std::env::temp_dir().join(format!("viso-web-compress-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let text = "export function init() { return 42; }\n".repeat(200);
        std::fs::write(dir.join("app.js"), &text).unwrap();
        std::fs::write(dir.join("index.html"), "<!doctype html>").unwrap();
        let files = listed(&dir).unwrap();
        precompress(&files, &[("br", true), ("gz", true)]).unwrap();
        let kinds: Vec<&str> = listed(&dir).unwrap().iter().map(|(k, _)| *k).collect();
        assert_eq!(kinds, ["js", "brotli", "gzip", "html", "brotli", "gzip"]);

        let mut brotli = String::new();
        brotli::Decompressor::new(File::open(dir.join("app.js.br")).unwrap(), 4096)
            .read_to_string(&mut brotli)
            .unwrap();
        assert_eq!(brotli, text);
        let mut gzip = String::new();
        flate2::read::GzDecoder::new(File::open(dir.join("app.js.gz")).unwrap())
            .read_to_string(&mut gzip)
            .unwrap();
        assert_eq!(gzip, text);
        assert!(std::fs::metadata(dir.join("app.js.br")).unwrap().len() < text.len() as u64 / 10);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
