//! The web bundle of a build, as its profile's web settings shape it: the
//! glue `wasm-bindgen` generates, keeping the module's debug info for source
//! maps or removing its name and producers sections for a stripped build;
//! the module optimized by `wasm-opt` for speed or size; the module and its
//! glue named by their content (`<bin>_bg.<hash>.wasm`, `<bin>.<hash>.js`)
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

use viso_package::web as bundle;
use viso_project::{Profile, WebOptimize, WebSettings};

use super::cargo::{BuildFailure, CargoEvent, Sink, is_cancelled};
use crate::process::ProcessTree;

/// How `wasm-opt` is installed, for a message about its absence.
const WASM_OPT_INSTALL: &str = "install Binaryen (`brew install binaryen`, or its release from github.com/WebAssembly/binaryen)";

/// The WebAssembly features Rust's `wasm32-unknown-unknown` emits by
/// default, which the optimized module may keep using.
const FEATURES: &[&str] = &[
    "--enable-bulk-memory",
    "--enable-mutable-globals",
    "--enable-nontrapping-float-to-int",
    "--enable-sign-ext",
    "--enable-reference-types",
    "--enable-multivalue",
];

/// Brotli at its densest: quality 11, a 16 MiB window.
const BROTLI_QUALITY: u32 = 11;
const BROTLI_WINDOW: u32 = 24;

/// The file kinds of a bundle, by extension.
fn kind(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()? {
        "wasm" => Some("wasm"),
        "js" => Some("js"),
        "html" => Some("html"),
        "br" => Some("brotli"),
        "gz" => Some("gzip"),
        _ => None,
    }
}

/// Writes the bundle of the module `wasm`, the app `bin`, into `dir`, which
/// is empty; each file it wrote with its kind, by name.
pub(crate) fn bundle(
    wasm: &Path,
    dir: &Path,
    bin: &str,
    profile: Profile,
    web: &WebSettings,
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

    let module = dir.join(format!("{bin}_bg.wasm"));
    if let Some(level) = match web.optimize.value {
        WebOptimize::None => None,
        WebOptimize::Speed => Some("-O3"),
        WebOptimize::Size => Some("-Oz"),
    } {
        let optimized = dir.join(format!("{bin}_bg.opt.wasm"));
        let mut opt = Command::new("wasm-opt");
        opt.arg(level).args(FEATURES);
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

    fingerprint(dir, bin)?;

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
/// page loading them under those names.
fn fingerprint(dir: &Path, bin: &str) -> Result<(), BuildFailure> {
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
    let glue = rename(bin, "js")?;
    bundle::write_page(dir, bin, &glue, Some(&module)).map_err(BuildFailure::Package)?;
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
fn tool(
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
    fn the_module_and_its_glue_are_named_by_their_content_and_the_page_by_none() {
        let dir = std::env::temp_dir().join(format!("viso-web-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let write = |wasm: &str| {
            std::fs::write(dir.join("app_bg.wasm"), wasm).unwrap();
            std::fs::write(dir.join("app.js"), "export default function init() {}").unwrap();
            fingerprint(&dir, "app").unwrap();
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
