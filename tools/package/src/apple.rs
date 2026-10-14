//! The iOS `.app`: the executable and its Info.plist, signed.
//!
//! A simulator app is signed ad-hoc and needs no identity or provisioning; a
//! device app is signed with an identity and embeds the provisioning profile
//! whose entitlements it is signed with.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{io, output, run};

/// The Rust target of a device.
pub const DEVICE_TRIPLE: &str = "aarch64-apple-ios";

/// The Rust target of the simulator on this host.
pub const fn simulator_triple() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64-apple-ios"
    } else {
        "aarch64-apple-ios-sim"
    }
}

/// The lowest iOS version an app supports (its `MinimumOSVersion`).
pub const MIN_IOS: &str = "15.0";

/// A packaged app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// The `.app` directory.
    pub path: PathBuf,
    /// Its bundle identifier.
    pub id: String,
}

/// The bundle identifier of the app named `bin`: letters, digits, `-` and
/// `.` only.
pub fn bundle_id(bin: &str) -> String {
    let name: String = bin
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("dev.viso.{name}")
}

/// Wraps the executable `binary` as `<dir>/<bin>.app`, replacing what was
/// there, for the simulator or a device. It is not yet signed.
pub fn bundle(binary: &Path, dir: &Path, bin: &str, simulator: bool) -> Result<App, String> {
    let app = dir.join(format!("{bin}.app"));
    let _ = std::fs::remove_dir_all(&app);
    io(&app, std::fs::create_dir_all(&app))?;
    let exe = app.join(bin);
    io(&exe, std::fs::copy(binary, &exe))?;
    let id = bundle_id(bin);
    let plist = app.join("Info.plist");
    io(
        &plist,
        std::fs::write(&plist, info_plist(bin, &id, simulator)),
    )?;
    Ok(App { path: app, id })
}

/// Signs `app`: ad-hoc without an `identity`; with a provisioning `profile`,
/// embedding it and signing with its entitlements, decoded under `scratch`.
pub fn sign(
    app: &Path,
    identity: Option<&str>,
    profile: Option<&Path>,
    scratch: &Path,
) -> Result<(), String> {
    let mut codesign = Command::new("codesign");
    codesign
        .args(["--force", "--timestamp=none", "--sign"])
        .arg(identity.unwrap_or("-"));
    if let Some(profile) = profile {
        io(
            profile,
            std::fs::copy(profile, app.join("embedded.mobileprovision")),
        )?;
        io(scratch, std::fs::create_dir_all(scratch))?;
        let decoded = scratch.join("profile.plist");
        let text = output(
            Command::new("security")
                .args(["cms", "-D", "-i"])
                .arg(profile),
        )?;
        io(&decoded, std::fs::write(&decoded, text))?;
        let entitlements = scratch.join("app.entitlements");
        run(Command::new("plutil")
            .args(["-extract", "Entitlements", "xml1", "-o"])
            .arg(&entitlements)
            .arg(&decoded))?;
        codesign.arg("--entitlements").arg(&entitlements);
    }
    run(codesign.arg(app))
}

/// The app's Info.plist: one full-screen scene, portrait and landscape, on
/// iPhone and iPad.
pub fn info_plist(bin: &str, id: &str, simulator: bool) -> String {
    let platform = if simulator {
        "iPhoneSimulator"
    } else {
        "iPhoneOS"
    };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>en</string>
	<key>CFBundleExecutable</key>
	<string>{bin}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{bin}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>1.0</string>
	<key>CFBundleVersion</key>
	<string>1</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>{platform}</string>
	</array>
	<key>LSRequiresIPhoneOS</key>
	<true/>
	<key>MinimumOSVersion</key>
	<string>{MIN_IOS}</string>
	<key>UIDeviceFamily</key>
	<array>
		<integer>1</integer>
		<integer>2</integer>
	</array>
	<key>UILaunchScreen</key>
	<dict/>
	<key>UIApplicationSceneManifest</key>
	<dict>
		<key>UIApplicationSupportsMultipleScenes</key>
		<false/>
	</dict>
	<key>UISupportedInterfaceOrientations</key>
	<array>
		<string>UIInterfaceOrientationPortrait</string>
		<string>UIInterfaceOrientationLandscapeLeft</string>
		<string>UIInterfaceOrientationLandscapeRight</string>
	</array>
	<key>UISupportedInterfaceOrientations~ipad</key>
	<array>
		<string>UIInterfaceOrientationPortrait</string>
		<string>UIInterfaceOrientationPortraitUpsideDown</string>
		<string>UIInterfaceOrientationLandscapeLeft</string>
		<string>UIInterfaceOrientationLandscapeRight</string>
	</array>
</dict>
</plist>
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_names_its_executable_and_platform() {
        assert_eq!(bundle_id("hello_world"), "dev.viso.hello-world");
        let plist = info_plist("hello_world", "dev.viso.hello-world", true);
        assert!(plist.contains("<string>hello_world</string>"));
        assert!(plist.contains("<string>dev.viso.hello-world</string>"));
        assert!(plist.contains("<string>iPhoneSimulator</string>"));
        assert!(info_plist("a", "dev.viso.a", false).contains("<string>iPhoneOS</string>"));
    }

    #[test]
    fn a_bundle_holds_the_executable_and_its_plist() {
        let dir = std::env::temp_dir().join(format!("viso-package-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("built");
        std::fs::write(&binary, b"exe").unwrap();
        let app = bundle(&binary, &dir, "demo", true).unwrap();
        assert_eq!(app.path, dir.join("demo.app"));
        assert_eq!(app.id, "dev.viso.demo");
        assert_eq!(std::fs::read(app.path.join("demo")).unwrap(), b"exe");
        assert!(app.path.join("Info.plist").is_file());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
