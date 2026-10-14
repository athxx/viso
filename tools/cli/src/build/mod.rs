//! The Build Service: how the app is compiled and packaged for a target and a
//! profile, shared by `viso build` and `viso run`.
//!
//! - [`rust_plan`]: the Rust target, the compiler arguments and the
//!   environment of a target — the simulator triple, the emulator ABI linked
//!   as the Java shell's library with the NDK, `wasm32-unknown-unknown`.
//! - [`cargo`]: one `cargo build` with the profile's flags, killable with
//!   every process under it.
//! - [`produce`]: a build packaged for its target into
//!   `target/viso/build/<build-id>/out/` — the executable, the simulator
//!   `.app` signed ad hoc, the APK, or the web bundle as the profile's web
//!   settings shape it ([`web`]).
//! - [`publish`]: the packaged output copied into `dist/<platform>/` whole,
//!   with its `artifact.json`, replacing what was there in one rename, so
//!   `dist/` never holds part of a build; each file with its size and
//!   SHA-256.

pub(crate) mod cargo;
mod web;

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use viso_package::android::{self, Abi, Sdk, Shell};
use viso_package::{apple, web as bundle};
use viso_project::{Layout, Profile, Target, Toolchain, WebSettings};

use cargo::{BuildCommand, BuildFailure, Sink, is_cancelled};

/// How the app is compiled for a target.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RustPlan {
    /// The Rust target, when not the host.
    pub(crate) target: Option<String>,
    /// The arguments after `cargo rustc --`; `cargo build` when empty.
    pub(crate) rustc_args: Vec<String>,
    /// The variables Cargo runs with.
    pub(crate) env: Vec<(String, OsString)>,
}

/// How the app is compiled for `target` with `profile`; an Android build
/// links with the NDK of `sdk`.
pub(crate) fn rust_plan(target: Target, profile: Profile, sdk: Option<&Sdk>) -> RustPlan {
    match target {
        Target::Host | Target::Headless => RustPlan::default(),
        Target::Ios => RustPlan {
            target: Some(apple::simulator_triple().to_owned()),
            rustc_args: Vec::new(),
            env: vec![(
                "IPHONEOS_DEPLOYMENT_TARGET".to_owned(),
                apple::MIN_IOS.into(),
            )],
        },
        Target::Android => {
            let abi = Abi::host();
            let mut env: Vec<(String, OsString)> = sdk
                .map(|sdk| sdk.cargo_env(abi))
                .unwrap_or_default()
                .into_iter()
                .map(|(k, v)| (k, v.into_os_string()))
                .collect();
            if profile == Profile::Dev {
                // The debug info stays on the host: the APK carries the code
                // only, so it installs in seconds.
                env.push(("CARGO_PROFILE_DEV_STRIP".to_owned(), "debuginfo".into()));
            }
            RustPlan {
                target: Some(abi.triple().to_owned()),
                rustc_args: android::RUSTC_ARGS.iter().map(|&a| a.to_owned()).collect(),
                env,
            }
        }
        Target::WebGpu | Target::WebDom | Target::WebHybrid => RustPlan {
            target: Some(bundle::TRIPLE.to_owned()),
            ..RustPlan::default()
        },
    }
}

/// The compiler identity of the build: `rustc -V`, or `unknown` when it does
/// not run.
pub(crate) fn toolchain() -> Toolchain {
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
    let version = Command::new(rustc)
        .arg("-V")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map_or_else(|| "unknown".to_string(), |v| v.trim().to_string());
    Toolchain::new(version, Toolchain::current_host())
}

/// What an artifact's `target` names: the platform the host resolves to,
/// as its `dist/` directory does, or the target.
pub(crate) fn platform(layout: &Layout, target: Target) -> String {
    match target {
        Target::Host => layout
            .dist(target)
            .file_name()
            .map_or("host".to_owned(), |name| {
                name.to_string_lossy().into_owned()
            }),
        other => other.as_str().to_owned(),
    }
}

/// A build to compile and package.
pub(crate) struct Job {
    /// The compile; it packages nothing itself.
    pub(crate) cargo: BuildCommand,
    pub(crate) target: Target,
    pub(crate) profile: Profile,
    /// How a web bundle is shaped.
    pub(crate) web: WebSettings,
    /// The build's own directory, `target/viso/build/<build-id>/`.
    pub(crate) dir: PathBuf,
    /// The SDK and Java shell an Android build is packed with.
    pub(crate) android: Option<(Sdk, Shell)>,
}

/// A build compiled and packaged, not yet published.
#[derive(Debug)]
pub(crate) struct Staged {
    /// The packaged output, `<build dir>/out/`.
    pub(crate) out: PathBuf,
    /// Each artifact: its kind and its path under `out`.
    pub(crate) artifacts: Vec<(&'static str, PathBuf)>,
    /// How the output is signed: `none`, `ad-hoc` or `debug-keystore`.
    pub(crate) signing: &'static str,
    /// Each stage and how long it took.
    pub(crate) stages: Vec<(&'static str, Duration)>,
}

/// Compiles `job` and packages it for its target, until it is done or
/// `cancelled` fires; what Cargo and the packaging say goes to `sink`.
pub(crate) fn produce(
    job: &Job,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Staged, BuildFailure> {
    let mut stages = Vec::new();
    let started = Instant::now();
    let exe = cargo::build(&job.cargo, cancelled, sink)?;
    stages.push(("compile", started.elapsed()));
    if is_cancelled(cancelled) {
        return Err(BuildFailure::Cancelled);
    }

    let started = Instant::now();
    let out = job.dir.join("out");
    let written = |error: io::Error, path: &Path| {
        BuildFailure::Write(format!("cannot write `{}`: {error}", path.display()))
    };
    if let Err(error) = fs::remove_dir_all(&out)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(written(error, &out));
    }
    fs::create_dir_all(&out).map_err(|e| written(e, &out))?;
    let file = exe
        .file_name()
        .ok_or_else(|| BuildFailure::Package(format!("`{}` has no name", exe.display())))?;
    let bin = exe
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| BuildFailure::Package(format!("`{}` has no name", exe.display())))?;
    let (artifacts, signing) = match job.target {
        Target::Host | Target::Headless => {
            let path = out.join(file);
            fs::copy(&exe, &path).map_err(|e| written(e, &path))?;
            (vec![("binary", path)], "none")
        }
        Target::Ios => {
            let app = apple::bundle(&exe, &out, bin, true).map_err(BuildFailure::Package)?;
            apple::sign(&app.path, None, None, &job.dir.join("sign"))
                .map_err(BuildFailure::Package)?;
            (vec![("app-bundle", app.path)], "ad-hoc")
        }
        Target::Android => {
            let (sdk, shell) = job
                .android
                .as_ref()
                .expect("an Android build has its SDK and shell");
            let flavor = match job.profile {
                Profile::Dev => android::Flavor::Dev,
                Profile::Release | Profile::Shipping => android::Flavor::Release,
            };
            let apk = sdk
                .package(
                    &exe,
                    Abi::host(),
                    &job.dir.join("android"),
                    bin,
                    shell,
                    flavor,
                )
                .map_err(BuildFailure::Package)?;
            let path = out.join(format!("{bin}.apk"));
            fs::copy(&apk.path, &path).map_err(|e| written(e, &path))?;
            (vec![("apk", path)], "debug-keystore")
        }
        Target::WebGpu | Target::WebDom | Target::WebHybrid => {
            let files = web::bundle(&exe, &out, bin, job.profile, &job.web, cancelled, sink)?;
            (files, "none")
        }
    };
    stages.push(("package", started.elapsed()));
    let artifacts = artifacts
        .into_iter()
        .map(|(kind, path)| {
            let relative = path.strip_prefix(&out).map(Path::to_owned).unwrap_or(path);
            (kind, relative)
        })
        .collect();
    Ok(Staged {
        out,
        artifacts,
        signing,
        stages,
    })
}

/// An artifact as published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Written {
    pub(crate) kind: &'static str,
    /// Where it is, under `dist/<platform>/`.
    pub(crate) path: PathBuf,
    /// Its bytes; a directory's, all its files'.
    pub(crate) size: u64,
    /// `sha256:<hex>`; a directory's over its files ([`hash`]).
    pub(crate) hash: String,
}

/// The name of the manifest beside a published build.
pub(crate) const MANIFEST: &str = "artifact.json";

/// Publishes `staged` as `dist`, `dist/<platform>/`, with the manifest
/// `manifest` writes of its artifacts: copied beside it, then swapped in
/// with one rename, the old output removed after. The artifacts, the
/// manifest last.
pub(crate) fn publish(
    staged: &Staged,
    dist: &Path,
    manifest: impl FnOnce(&[Written]) -> String,
) -> Result<Vec<Written>, String> {
    let parent = dist
        .parent()
        .ok_or_else(|| format!("`{}` has no parent", dist.display()))?;
    let name = dist
        .file_name()
        .ok_or_else(|| format!("`{}` has no name", dist.display()))?
        .to_string_lossy();
    let fresh = parent.join(format!(".{name}.{}.new", std::process::id()));
    let failed =
        |error: io::Error, path: &Path| format!("cannot write `{}`: {error}", path.display());
    fs::create_dir_all(parent).map_err(|e| failed(e, parent))?;
    let _ = fs::remove_dir_all(&fresh);
    let result = (|| {
        copy_tree(&staged.out, &fresh).map_err(|e| failed(e, &fresh))?;
        let mut written = Vec::with_capacity(staged.artifacts.len() + 1);
        for (kind, relative) in &staged.artifacts {
            let (size, hash) = hash(&fresh.join(relative)).map_err(|e| failed(e, relative))?;
            written.push(Written {
                kind,
                path: dist.join(relative),
                size,
                hash,
            });
        }
        let path = fresh.join(MANIFEST);
        fs::write(&path, manifest(&written)).map_err(|e| failed(e, &path))?;
        let (size, hash) = hash(&path).map_err(|e| failed(e, &path))?;
        written.push(Written {
            kind: "manifest",
            path: dist.join(MANIFEST),
            size,
            hash,
        });
        replace(&fresh, dist, parent, &name).map_err(|e| failed(e, dist))?;
        Ok(written)
    })();
    // After a swap it holds the old output; after a failure, part of the new.
    let _ = fs::remove_dir_all(&fresh);
    result
}

/// Puts `fresh` where `dist` is: swapped in one rename where the system
/// can, else the old moved aside first. `fresh` then holds the old output.
fn replace(fresh: &Path, dist: &Path, parent: &Path, name: &str) -> io::Result<()> {
    if fs::symlink_metadata(dist).is_err() {
        return fs::rename(fresh, dist);
    }
    match exchange(fresh, dist) {
        Ok(()) => return Ok(()),
        Err(error) if !unsupported(&error) => return Err(error),
        Err(_) => {}
    }
    let old = parent.join(format!(".{name}.{}.old", std::process::id()));
    let _ = fs::remove_dir_all(&old);
    fs::rename(dist, &old)?;
    if let Err(error) = fs::rename(fresh, dist) {
        let _ = fs::rename(&old, dist);
        return Err(error);
    }
    fs::rename(&old, fresh)
}

/// Whether a swap failed only because the system or filesystem has none.
fn unsupported(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Unsupported | io::ErrorKind::InvalidInput
    ) || {
        #[cfg(unix)]
        {
            matches!(
                error.raw_os_error(),
                Some(libc::ENOTSUP | libc::ENOSYS | libc::EINVAL | libc::EXDEV)
            )
        }
        #[cfg(not(unix))]
        {
            false
        }
    }
}

/// Swaps the entries at `a` and `b` atomically.
#[cfg(target_vendor = "apple")]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call.
    if unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Swaps the entries at `a` and `b` atomically.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn exchange(a: &Path, b: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let a = CString::new(a.as_os_str().as_bytes())?;
    let b = CString::new(b.as_os_str().as_bytes())?;
    // SAFETY: both are NUL-terminated paths that live across the call, each
    // taken from the current directory as `AT_FDCWD` asks.
    let swapped = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            a.as_ptr(),
            libc::AT_FDCWD,
            b.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    if swapped == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Swaps the entries at `a` and `b` atomically, where the system can.
#[cfg(not(any(target_vendor = "apple", target_os = "linux", target_os = "android")))]
fn exchange(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::ErrorKind::Unsupported.into())
}

/// Copies the tree at `from` to `to`, which does not exist: each file
/// cloned where the filesystem can, links recreated as links.
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let (source, target) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_tree(&source, &target)?;
        } else if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(fs::read_link(&source)?, &target)?;
            #[cfg(not(unix))]
            fs::copy(&source, &target).map(drop)?;
        } else {
            fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// The size and `sha256:<hex>` of the file or directory at `path`. A
/// directory's hash is over its files in path order: each one's path below
/// it, `/`-separated, a NUL, and the SHA-256 of its bytes; its size is
/// theirs together.
pub(crate) fn hash(path: &Path) -> io::Result<(u64, String)> {
    let digest = |bytes: &[u8]| -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() };
    if !fs::symlink_metadata(path)?.is_dir() {
        let (size, sum) = file_hash(path)?;
        return Ok((size, format!("sha256:{}", digest(&sum))));
    }
    let mut files = Vec::new();
    collect(path, path, &mut files)?;
    files.sort();
    let mut tree = Sha256::new();
    let mut total = 0;
    for name in files {
        let (size, sum) = file_hash(&path.join(&name))?;
        total += size;
        tree.update(name.as_bytes());
        tree.update([0]);
        tree.update(sum);
    }
    Ok((total, format!("sha256:{}", digest(&tree.finalize()))))
}

/// The SHA-256 of a file's bytes, and how many there are.
fn file_hash(path: &Path) -> io::Result<(u64, [u8; 32])> {
    let mut hasher = Sha256::new();
    let size = io::copy(&mut fs::File::open(path)?, &mut hasher)?;
    Ok((size, hasher.finalize().into()))
}

/// The files under `dir`, as paths below `root` with `/` separators.
fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            collect(root, &path, files)?;
        } else {
            let relative = path.strip_prefix(root).unwrap_or(&path);
            let name: Vec<String> = relative
                .iter()
                .map(|part| part.to_string_lossy().into_owned())
                .collect();
            files.push(name.join("/"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-build-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_target_compiles_for_its_device() {
        assert_eq!(
            rust_plan(Target::Host, Profile::Release, None),
            RustPlan::default()
        );
        let web = rust_plan(Target::WebGpu, Profile::Dev, None);
        assert_eq!(web.target.as_deref(), Some("wasm32-unknown-unknown"));
        let ios = rust_plan(Target::Ios, Profile::Dev, None);
        assert_eq!(ios.target.as_deref(), Some(apple::simulator_triple()));
        let android = rust_plan(Target::Android, Profile::Release, None);
        assert!(!android.rustc_args.is_empty());
        assert!(
            android
                .env
                .iter()
                .all(|(k, _)| k != "CARGO_PROFILE_DEV_STRIP")
        );
        let android = rust_plan(Target::Android, Profile::Dev, None);
        assert!(
            android
                .env
                .iter()
                .any(|(k, _)| k == "CARGO_PROFILE_DEV_STRIP")
        );
    }

    #[test]
    fn a_file_and_a_tree_hash_as_their_bytes_say() {
        let dir = scratch("hash");
        fs::write(dir.join("a"), b"abc").unwrap();
        assert_eq!(
            hash(&dir.join("a")).unwrap(),
            (
                3,
                "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad".into()
            )
        );
        let app = dir.join("demo.app");
        fs::create_dir_all(app.join("res")).unwrap();
        fs::write(app.join("demo"), b"exe").unwrap();
        fs::write(app.join("res/x"), b"xy").unwrap();
        let (size, first) = hash(&app).unwrap();
        assert_eq!(size, 5);
        fs::write(app.join("res/x"), b"xz").unwrap();
        assert_ne!(hash(&app).unwrap().1, first, "a file's bytes count");
        fs::write(app.join("res/x"), b"xy").unwrap();
        assert_eq!(hash(&app).unwrap().1, first);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn publishing_replaces_the_output_whole() {
        let dir = scratch("publish");
        let out = dir.join("build/out");
        fs::create_dir_all(out.join("demo.app")).unwrap();
        fs::write(out.join("demo.app/demo"), b"new").unwrap();
        let dist = dir.join("dist/ios");
        fs::create_dir_all(&dist).unwrap();
        fs::write(dist.join("stale"), b"old").unwrap();
        let staged = Staged {
            out: out.clone(),
            artifacts: vec![("app-bundle", PathBuf::from("demo.app"))],
            signing: "ad-hoc",
            stages: Vec::new(),
        };
        let written = publish(&staged, &dist, |artifacts| {
            format!("{{\"count\":{}}}\n", artifacts.len())
        })
        .unwrap();
        let kinds: Vec<&str> = written.iter().map(|w| w.kind).collect();
        assert_eq!(kinds, ["app-bundle", "manifest"]);
        assert_eq!(written[0].path, dist.join("demo.app"));
        assert_eq!(written[0].size, 3);
        assert_eq!(
            fs::read_to_string(dist.join(MANIFEST)).unwrap(),
            "{\"count\":1}\n"
        );
        assert!(!dist.join("stale").exists(), "the old output is gone");
        assert_eq!(fs::read(dist.join("demo.app/demo")).unwrap(), b"new");
        assert!(
            out.join("demo.app/demo").exists(),
            "the build keeps its own"
        );
        let left: Vec<_> = fs::read_dir(dir.join("dist"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["ios"], "nothing is left beside it");

        // Into a `dist/` that does not exist yet.
        let fresh = dir.join("other/dist/web");
        publish(&staged, &fresh, |_| String::new()).unwrap();
        assert!(fresh.join("demo.app/demo").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
