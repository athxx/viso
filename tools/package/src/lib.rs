//! Wrapping a built app for a target that cannot run a bare executable.
//!
//! - [`apple`]: an iOS `.app` with its Info.plist, signed ad-hoc for the
//!   simulator or with an identity and provisioning profile for a device.
//! - [`android`]: the Android SDK and NDK, the Cargo environment that links an
//!   app as the shared library the Java shell loads, and the signed APK.
//! - [`web`]: the `wasm-bindgen` glue, the page that loads it and the files a
//!   server hands out.
//! - [`windows`]: the icon and version linked into the executable, the MSIX
//!   and the signature of both.
//! - [`linux`]: the desktop entry that launches the app.
//! - [`archive`]: the zip and tar a package ships in, the same bytes for the
//!   same files.
//!
//! Each step runs the platform's own tool through the [`Runner`] the caller
//! installs, the CLI's showing and ending it as every program it starts; a
//! failure is a message naming the tool and what it said.

pub mod android;
pub mod apple;
pub mod archive;
pub mod linux;
pub mod web;
pub mod windows;

/// What an app asks the system to let it use; each platform names it its
/// own way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Camera,
    Microphone,
    Location,
    Photos,
    Contacts,
    Bluetooth,
    Notifications,
    /// Reaching servers: asked of the package on Android and Windows, which
    /// otherwise keep a release build offline.
    Network,
}

/// The URLs the system opens an app for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Links {
    /// Custom schemes, lowercase, without `://`.
    pub schemes: Vec<String>,
    /// Hosts whose `https` links open the app once the site vouches for it;
    /// `*.` takes every subdomain.
    pub hosts: Vec<String>,
}

impl Links {
    /// Whether no URL opens the app.
    pub fn is_empty(&self) -> bool {
        self.schemes.is_empty() && self.hosts.is_empty()
    }
}

/// The orientations a mobile app turns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientations {
    Any,
    Portrait,
    Landscape,
}

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// What runs a tool to its end and returns its stdout, or why it failed with
/// what it said.
pub type Runner = fn(&mut Command) -> Result<String, String>;

static RUNNER: OnceLock<Runner> = OnceLock::new();

/// Runs every tool from now on with `runner`; the first runner installed is
/// the one kept.
pub fn run_tools_with(runner: Runner) {
    let _ = RUNNER.set(runner);
}

/// Runs `cmd`, failing with its name and what it wrote if it does not succeed.
pub(crate) fn run(cmd: &mut Command) -> Result<(), String> {
    output(cmd).map(drop)
}

/// Runs `cmd` and returns its stdout.
pub(crate) fn output(cmd: &mut Command) -> Result<String, String> {
    match RUNNER.get() {
        Some(runner) => runner(cmd),
        None => plain(cmd),
    }
}

/// The runner when none is installed: `cmd` to its end, its output captured.
fn plain(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let said = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(format!(
            "{:?} failed ({}): {}",
            cmd.get_program(),
            out.status,
            said.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The entry of `dir` whose name sorts last by its numeric parts (the newest
/// version), among those `keep` accepts.
pub(crate) fn newest(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
    let key = |name: &str| -> Vec<u64> {
        name.split(|c: char| !c.is_ascii_digit())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect()
    };
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| keep(n))
        .max_by_key(|n| key(n))
        .map(|n| dir.join(n))
}

/// Collects the files under `dir` with `extension`, at any depth, in name
/// order.
pub(crate) fn files_under(
    dir: &Path,
    extension: &str,
    found: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            files_under(&path, extension, found)?;
        } else if path.extension().is_some_and(|x| x == extension) {
            found.push(path);
        }
    }
    Ok(())
}

/// `std::fs` errors with the path they are about.
pub(crate) fn io<T>(path: &Path, result: std::io::Result<T>) -> Result<T, String> {
    result.map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_orders_versions_numerically() {
        let dir = std::env::temp_dir().join(format!("viso-package-newest-{}", std::process::id()));
        for v in ["9.0.0", "36.1.0", "37.0.0", "android-36", "android-36.1"] {
            std::fs::create_dir_all(dir.join(v)).unwrap();
        }
        let digits = |n: &str| n.starts_with(|c: char| c.is_ascii_digit());
        assert_eq!(newest(&dir, digits), Some(dir.join("37.0.0")));
        assert_eq!(
            newest(&dir, |n| n.starts_with("android-")),
            Some(dir.join("android-36.1"))
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
