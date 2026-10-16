//! The iOS Simulator runtimes of this Mac, as Xcode keeps them: the ones
//! installed (`simctl list runtimes`), the one Xcode's iOS SDK pairs with
//! and can download (`simctl runtime match list`), and the one chosen as
//! this machine's default in `~/.viso/ios/config.toml`.
//!
//! A runtime is installed only through Xcode's own mechanism,
//! `xcodebuild -downloadPlatform iOS`; Viso orchestrates it, reports its
//! progress and checks the runtime is there afterwards, and never touches
//! Xcode itself.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;

use toml_edit::{DocumentMut, value};
use viso_ende::JsonValue;
use viso_project::DeviceStore;

use crate::device::{capture, version_key};

/// `~/.viso/ios`.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    root: PathBuf,
}

impl Home {
    /// This machine's.
    pub(crate) fn current() -> Option<Home> {
        DeviceStore::home().map(|store| Home::at(store.root().join("ios")))
    }

    pub(crate) fn at(root: PathBuf) -> Home {
        Home { root }
    }

    fn config(&self) -> PathBuf {
        self.root.join("config.toml")
    }

    fn document(&self) -> DocumentMut {
        fs::read_to_string(self.config())
            .ok()
            .and_then(|text| text.parse().ok())
            .unwrap_or_default()
    }

    /// The runtime version chosen as this machine's default.
    pub(crate) fn default_runtime(&self) -> Option<String> {
        Some(self.document().get("runtime")?.as_str()?.to_owned())
    }

    /// Chooses `version` as the default, keeping the rest of the file.
    pub(crate) fn set_default_runtime(&self, version: &str) -> io::Result<()> {
        let mut doc = self.document();
        doc["runtime"] = value(version);
        fs::create_dir_all(&self.root)?;
        crate::atomic::write(&self.config(), doc.to_string().as_bytes())
    }
}

/// A device type a runtime runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DeviceType {
    pub name: String,
    pub identifier: String,
    /// `iPhone`, `iPad`, …
    pub family: String,
}

/// An iOS Simulator runtime `simctl` lists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Runtime {
    /// `26.3.1`.
    pub version: String,
    pub build: String,
    /// `com.apple.CoreSimulator.SimRuntime.iOS-26-3`.
    pub identifier: String,
    /// Why it cannot run, when it cannot.
    pub unusable: Option<String>,
    pub device_types: Vec<DeviceType>,
}

impl Runtime {
    pub(crate) fn usable(&self) -> bool {
        self.unusable.is_none()
    }
}

fn simctl() -> Command {
    let mut command = Command::new("xcrun");
    command.arg("simctl");
    command
}

/// The iOS runtimes installed, newest first.
pub(crate) fn runtimes() -> Result<Vec<Runtime>, String> {
    parse_runtimes(&capture(simctl().args(["list", "runtimes", "-j"]))?)
}

fn parse_runtimes(text: &str) -> Result<Vec<Runtime>, String> {
    let json = JsonValue::parse(text).map_err(|e| format!("`simctl` wrote no JSON: {e}"))?;
    let field = |value: &JsonValue, name: &str| {
        value
            .get(name)
            .and_then(JsonValue::as_str)
            .unwrap_or("")
            .to_owned()
    };
    let mut runtimes: Vec<Runtime> = json
        .get("runtimes")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter(|r| r.get("platform").and_then(JsonValue::as_str) == Some("iOS"))
        .map(|r| Runtime {
            version: field(r, "version"),
            build: field(r, "buildversion"),
            identifier: field(r, "identifier"),
            unusable: (r.get("isAvailable").and_then(JsonValue::as_bool) != Some(true)).then(
                || {
                    Some(field(r, "availabilityError"))
                        .filter(|why| !why.is_empty())
                        .unwrap_or_else(|| "unavailable".to_owned())
                },
            ),
            device_types: r
                .get("supportedDeviceTypes")
                .and_then(JsonValue::as_array)
                .unwrap_or_default()
                .iter()
                .map(|t| DeviceType {
                    name: field(t, "name"),
                    identifier: field(t, "identifier"),
                    family: field(t, "productFamily"),
                })
                .collect(),
        })
        .collect();
    runtimes.sort_by_key(|r| std::cmp::Reverse(version_key(&r.version)));
    Ok(runtimes)
}

/// The runtime version Xcode's iOS SDK pairs with, which
/// `xcodebuild -downloadPlatform iOS` installs.
pub(crate) fn xcode_runtime() -> Option<String> {
    let text = capture(simctl().args(["runtime", "match", "list", "-j"])).ok()?;
    parse_xcode_runtime(&text)
}

fn parse_xcode_runtime(text: &str) -> Option<String> {
    let json = JsonValue::parse(text).ok()?;
    let JsonValue::Object(sdks) = json else {
        return None;
    };
    sdks.iter()
        .filter(|(name, _)| name.starts_with("iphoneos"))
        .filter_map(|(_, sdk)| sdk.get("sdkVersion")?.as_str())
        .max_by_key(|version| version_key(version))
        .map(str::to_owned)
}

/// `wanted` as a runtime version: digits and dots, `iOS ` allowed before.
pub(crate) fn version(wanted: &str) -> Option<String> {
    let wanted = wanted.trim();
    let wanted = wanted
        .strip_prefix("iOS ")
        .or_else(|| wanted.strip_prefix("ios "))
        .unwrap_or(wanted)
        .trim();
    let parts: Vec<&str> = wanted.split('.').collect();
    (parts.len() <= 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
    .then(|| wanted.to_owned())
}

/// Whether the runtime `version` is the one `wanted` names: `26.3` names
/// `26.3.1`, `27` names `27.0`, and a missing trailing part is zero.
pub(crate) fn names(wanted: &str, version: &str) -> bool {
    let parts = |v: &str| -> Vec<u32> { v.split('.').filter_map(|p| p.parse().ok()).collect() };
    let (wanted, version) = (parts(wanted), parts(version));
    wanted
        .iter()
        .enumerate()
        .all(|(i, part)| version.get(i).copied().unwrap_or(0) == *part)
}

/// The newest usable runtime `wanted` names.
pub(crate) fn find<'a>(runtimes: &'a [Runtime], wanted: &str) -> Option<&'a Runtime> {
    runtimes
        .iter()
        .find(|r| r.usable() && names(wanted, &r.version))
}

/// The runtime new simulators are made on: the default when it is
/// installed, else the newest.
pub(crate) fn chosen<'a>(runtimes: &'a [Runtime], home: Option<&Home>) -> Option<&'a Runtime> {
    home.and_then(Home::default_runtime)
        .and_then(|wanted| find(runtimes, &wanted))
        .or_else(|| runtimes.iter().find(|r| r.usable()))
}

/// Bytes free on the volume simulator runtimes are installed on.
#[cfg(unix)]
pub(crate) fn free_bytes() -> Option<u64> {
    use std::ffi::CString;
    let path = CString::new("/Library/Developer").ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is NUL-terminated and `stat` is a writable `statvfs`
    // that `statvfs` fills when it returns 0.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statvfs` returned 0, so it filled `stat`.
    let stat = unsafe { stat.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(not(unix))]
pub(crate) fn free_bytes() -> Option<u64> {
    None
}

/// Space a runtime takes once installed, about.
pub(crate) const RUNTIME_BYTES: u64 = 10 << 30;

/// What `xcodebuild -downloadPlatform` says of its download in a line:
/// `iOS 27.0 Simulator (24A434): 12.3% (1 GB of 8.1 GB)`.
pub(crate) fn download_progress(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("Downloading ")?;
    rest.contains('%').then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"{"runtimes":[
        {"platform":"iOS","version":"18.6","buildversion":"22G86","identifier":"com.apple.CoreSimulator.SimRuntime.iOS-18-6","isAvailable":true,"supportedDeviceTypes":[]},
        {"platform":"iOS","version":"26.3.1","buildversion":"23D8133","identifier":"com.apple.CoreSimulator.SimRuntime.iOS-26-3","isAvailable":true,
         "supportedDeviceTypes":[{"name":"iPhone 17","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","productFamily":"iPhone"}]},
        {"platform":"iOS","version":"27.0","buildversion":"24A434","identifier":"com.apple.CoreSimulator.SimRuntime.iOS-27-0","isAvailable":false,"availabilityError":"Xcode is too old","supportedDeviceTypes":[]},
        {"platform":"watchOS","version":"27.0","buildversion":"x","identifier":"w","isAvailable":true,"supportedDeviceTypes":[]}
    ]}"#;

    #[test]
    fn runtimes_are_ios_newest_first_with_why_one_cannot_run() {
        let runtimes = parse_runtimes(LIST).unwrap();
        let versions: Vec<&str> = runtimes.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, ["27.0", "26.3.1", "18.6"]);
        assert_eq!(runtimes[0].unusable.as_deref(), Some("Xcode is too old"));
        assert_eq!(runtimes[1].device_types[0].family, "iPhone");
        assert_eq!(
            find(&runtimes, "26.3").map(|r| r.build.as_str()),
            Some("23D8133")
        );
        assert_eq!(
            find(&runtimes, "27"),
            None,
            "an unusable runtime is not found"
        );
        assert_eq!(chosen(&runtimes, None).unwrap().version, "26.3.1");
    }

    #[test]
    fn a_wanted_version_names_its_runtimes() {
        assert_eq!(version("iOS 26.3").as_deref(), Some("26.3"));
        assert_eq!(version("27").as_deref(), Some("27"));
        for bad in ["", "latest", "26.", "1.2.3.4", "26.x"] {
            assert_eq!(version(bad), None, "{bad}");
        }
        assert!(names("26.3", "26.3.1"));
        assert!(names("27", "27.0"));
        assert!(names("27.0", "27.0"));
        assert!(names("27.0.0", "27.0"));
        assert!(!names("26.3", "26.4"));
        assert!(!names("26.3.1", "26.3"));
        assert!(!names("2", "26.0"));
    }

    #[test]
    fn xcode_pairs_its_ios_sdk_with_a_runtime_and_reports_its_download() {
        let text = r#"{"appletvos27.0":{"sdkVersion":"27.0"},"iphoneos27.0":{"sdkVersion":"27.0","chosenRuntimeBuild":"24A434"}}"#;
        assert_eq!(parse_xcode_runtime(text).as_deref(), Some("27.0"));
        assert_eq!(parse_xcode_runtime("{}"), None);
        assert_eq!(
            download_progress("Downloading iOS 27.0 Simulator (24A434): 12.3% (1 GB of 8.1 GB)"),
            Some("iOS 27.0 Simulator (24A434): 12.3% (1 GB of 8.1 GB)")
        );
        assert_eq!(download_progress("Finding content..."), None);
    }

    #[test]
    fn the_default_runtime_is_machine_state() {
        let dir = std::env::temp_dir().join(format!("viso-ios-home-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("config.toml"), "device = \"iphone\"\n").unwrap();
        let home = Home::at(dir.clone());
        assert_eq!(home.default_runtime(), None);
        home.set_default_runtime("18.6").unwrap();
        assert_eq!(home.default_runtime().as_deref(), Some("18.6"));
        let text = fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(text.contains("device = \"iphone\""), "{text}");
        let runtimes = parse_runtimes(LIST).unwrap();
        assert_eq!(chosen(&runtimes, Some(&home)).unwrap().version, "18.6");
        let _ = fs::remove_dir_all(&dir);
    }
}
