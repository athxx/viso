//! Apple app bundles: the iOS `.app` and the `.ipa` it ships in, the macOS
//! `.app` and its `.dmg`, with their Info.plist, icons and signatures.
//!
//! A simulator app is signed ad-hoc and needs no identity or provisioning; a
//! device app is signed with an identity and embeds the provisioning profile
//! whose entitlements it is signed with. A macOS app signed with an identity
//! runs under the hardened runtime, entitled to what it asks the user for,
//! and is timestamped, as notarization requires.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::archive::{self, Entry, Kind};
use crate::{Access, Orientations, io, output, run};

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

/// The lowest iOS version an app supports (its `MinimumOSVersion`) unless
/// the project names another.
pub const MIN_IOS: &str = "15.0";

/// The pixel sizes a macOS icon holds, each a PNG.
pub const ICNS_SIZES: &[u32] = &[16, 32, 64, 128, 256, 512, 1024];

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

/// What an app's Info.plist says of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// The executable's name in the bundle.
    pub executable: String,
    pub id: String,
    /// The name under its icon.
    pub display_name: String,
    /// Its version; only the leading `1.2.3` reaches the plist.
    pub version: String,
    /// Its build number.
    pub build: u32,
    /// The lowest iOS it runs on.
    pub minimum_os: String,
    pub orientations: Orientations,
    /// What it asks the user for, with the reason shown when it asks.
    pub usage: Vec<(Access, String)>,
}

impl Info {
    /// A development app's: named after the executable, version 1.0, every
    /// orientation, asking for nothing.
    pub fn dev(bin: &str) -> Info {
        Info {
            executable: bin.to_owned(),
            id: bundle_id(bin),
            display_name: bin.to_owned(),
            version: "1.0".to_owned(),
            build: 1,
            minimum_os: MIN_IOS.to_owned(),
            orientations: Orientations::Any,
            usage: Vec::new(),
        }
    }

    /// `CFBundleShortVersionString`: one to three numbers, without a
    /// prerelease or build suffix.
    fn short_version(&self) -> &str {
        let end = self.version.find(['-', '+']).unwrap_or(self.version.len());
        &self.version[..end]
    }

    /// The keys every bundle has.
    fn common(&self) -> Vec<(&'static str, Value<'_>)> {
        let name: String = self.display_name.chars().take(15).collect();
        vec![
            ("CFBundleDevelopmentRegion", Value::text("en")),
            ("CFBundleDisplayName", Value::text(&self.display_name)),
            ("CFBundleExecutable", Value::text(&self.executable)),
            ("CFBundleIdentifier", Value::text(&self.id)),
            ("CFBundleInfoDictionaryVersion", Value::text("6.0")),
            ("CFBundleName", Value::Text(name.into())),
            ("CFBundlePackageType", Value::text("APPL")),
            (
                "CFBundleShortVersionString",
                Value::text(self.short_version()),
            ),
            (
                "CFBundleVersion",
                Value::Text(self.build.to_string().into()),
            ),
        ]
    }

    /// The usage description keys of what it asks for.
    fn usage(&self, macos: bool) -> impl Iterator<Item = (&'static str, Value<'_>)> {
        self.usage.iter().flat_map(move |(access, reason)| {
            usage_keys(*access, macos)
                .iter()
                .map(move |key| (*key, Value::text(reason)))
        })
    }
}

/// The Info.plist keys that give the reason for `access`.
fn usage_keys(access: Access, macos: bool) -> &'static [&'static str] {
    match access {
        Access::Camera => &["NSCameraUsageDescription"],
        Access::Microphone => &["NSMicrophoneUsageDescription"],
        Access::Location if macos => &["NSLocationUsageDescription"],
        Access::Location => &["NSLocationWhenInUseUsageDescription"],
        Access::Photos => &["NSPhotoLibraryUsageDescription"],
        Access::Contacts => &["NSContactsUsageDescription"],
        Access::Bluetooth => &["NSBluetoothAlwaysUsageDescription"],
        // Asked for at run time, with no reason in the plist.
        Access::Notifications => &[],
    }
}

/// The hardened runtime's entitlement a macOS app needs for `access`.
fn entitlement(access: Access) -> Option<&'static str> {
    match access {
        Access::Camera => Some("com.apple.security.device.camera"),
        Access::Microphone => Some("com.apple.security.device.audio-input"),
        Access::Location => Some("com.apple.security.personal-information.location"),
        Access::Photos => Some("com.apple.security.personal-information.photos-library"),
        Access::Contacts => Some("com.apple.security.personal-information.addressbook"),
        Access::Bluetooth | Access::Notifications => None,
    }
}

/// A property list value.
enum Value<'a> {
    Text(std::borrow::Cow<'a, str>),
    Bool(bool),
    Integer(u32),
    Array(Vec<Value<'a>>),
    Dict(Vec<(&'static str, Value<'a>)>),
}

impl<'a> Value<'a> {
    fn text(text: &'a str) -> Value<'a> {
        Value::Text(text.into())
    }

    fn write(&self, out: &mut String, depth: usize) {
        let pad = "\t".repeat(depth);
        match self {
            Value::Text(text) => {
                let _ = writeln!(out, "{pad}<string>{}</string>", escape(text));
            }
            Value::Bool(true) => {
                let _ = writeln!(out, "{pad}<true/>");
            }
            Value::Bool(false) => {
                let _ = writeln!(out, "{pad}<false/>");
            }
            Value::Integer(n) => {
                let _ = writeln!(out, "{pad}<integer>{n}</integer>");
            }
            Value::Array(items) => {
                let _ = writeln!(out, "{pad}<array>");
                for item in items {
                    item.write(out, depth + 1);
                }
                let _ = writeln!(out, "{pad}</array>");
            }
            Value::Dict(entries) if entries.is_empty() => {
                let _ = writeln!(out, "{pad}<dict/>");
            }
            Value::Dict(entries) => {
                let _ = writeln!(out, "{pad}<dict>");
                for (key, value) in entries {
                    let _ = writeln!(out, "{pad}\t<key>{key}</key>");
                    value.write(out, depth + 1);
                }
                let _ = writeln!(out, "{pad}</dict>");
            }
        }
    }
}

/// `dict` as a property list file.
fn plist(dict: Vec<(&'static str, Value<'_>)>) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n",
    );
    Value::Dict(dict).write(&mut out, 0);
    out.push_str("</plist>\n");
    out
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// An iOS app's Info.plist: one full-screen scene in the orientations it
/// turns to, on iPhone and iPad.
pub fn info_plist(info: &Info, simulator: bool) -> String {
    let platform = if simulator {
        "iPhoneSimulator"
    } else {
        "iPhoneOS"
    };
    let portrait = ["UIInterfaceOrientationPortrait"];
    let upside_down = ["UIInterfaceOrientationPortraitUpsideDown"];
    let landscape = [
        "UIInterfaceOrientationLandscapeLeft",
        "UIInterfaceOrientationLandscapeRight",
    ];
    let list = |names: Vec<&'static str>| {
        Value::Array(names.into_iter().map(|n| Value::Text(n.into())).collect())
    };
    let (phone, pad): (Vec<&str>, Vec<&str>) = match info.orientations {
        Orientations::Any => (
            portrait.iter().chain(&landscape).copied().collect(),
            portrait
                .iter()
                .chain(&upside_down)
                .chain(&landscape)
                .copied()
                .collect(),
        ),
        Orientations::Portrait => (
            portrait.to_vec(),
            portrait.iter().chain(&upside_down).copied().collect(),
        ),
        Orientations::Landscape => (landscape.to_vec(), landscape.to_vec()),
    };
    let mut dict = info.common();
    dict.extend([
        (
            "CFBundleSupportedPlatforms",
            Value::Array(vec![Value::text(platform)]),
        ),
        ("LSRequiresIPhoneOS", Value::Bool(true)),
        ("MinimumOSVersion", Value::text(&info.minimum_os)),
        (
            "UIDeviceFamily",
            Value::Array(vec![Value::Integer(1), Value::Integer(2)]),
        ),
        ("UILaunchScreen", Value::Dict(Vec::new())),
        (
            "UIApplicationSceneManifest",
            Value::Dict(vec![(
                "UIApplicationSupportsMultipleScenes",
                Value::Bool(false),
            )]),
        ),
        // An app that does not turn every way fills the iPad's screen.
        (
            "UIRequiresFullScreen",
            Value::Bool(info.orientations != Orientations::Any),
        ),
        ("UISupportedInterfaceOrientations", list(phone)),
        ("UISupportedInterfaceOrientations~ipad", list(pad)),
    ]);
    dict.extend(info.usage(false));
    plist(dict)
}

/// A macOS app's Info.plist, naming its icon `AppIcon` when it has one.
pub fn macos_info_plist(info: &Info, icon: bool) -> String {
    let mut dict = info.common();
    if icon {
        dict.push(("CFBundleIconFile", Value::text("AppIcon")));
    }
    dict.extend([
        (
            "LSApplicationCategoryType",
            Value::text("public.app-category.productivity"),
        ),
        ("NSHighResolutionCapable", Value::Bool(true)),
        ("NSPrincipalClass", Value::text("NSApplication")),
        ("NSSupportsAutomaticGraphicsSwitching", Value::Bool(true)),
    ]);
    dict.extend(info.usage(true));
    plist(dict)
}

/// Wraps the executable `binary` as the iOS app `<dir>/<executable>.app`,
/// replacing what was there, for the simulator or a device. It is not yet
/// signed.
pub fn bundle(binary: &Path, dir: &Path, info: &Info, simulator: bool) -> Result<App, String> {
    let app = dir.join(format!("{}.app", info.executable));
    let _ = std::fs::remove_dir_all(&app);
    io(&app, std::fs::create_dir_all(&app))?;
    let exe = app.join(&info.executable);
    io(&exe, std::fs::copy(binary, &exe))?;
    let path = app.join("Info.plist");
    io(&path, std::fs::write(&path, info_plist(info, simulator)))?;
    Ok(App {
        path: app,
        id: info.id.clone(),
    })
}

/// Wraps the executable `binary` as the macOS app `<dir>/<name>.app`, with
/// `icns` as its icon. It is not yet signed.
pub fn macos_bundle(
    binary: &Path,
    dir: &Path,
    name: &str,
    info: &Info,
    icns: Option<&[u8]>,
) -> Result<App, String> {
    let app = dir.join(format!("{name}.app"));
    let _ = std::fs::remove_dir_all(&app);
    let contents = app.join("Contents");
    let macos = contents.join("MacOS");
    io(&macos, std::fs::create_dir_all(&macos))?;
    let exe = macos.join(&info.executable);
    io(&exe, std::fs::copy(binary, &exe))?;
    if let Some(icns) = icns {
        let resources = contents.join("Resources");
        io(&resources, std::fs::create_dir_all(&resources))?;
        let path = resources.join("AppIcon.icns");
        io(&path, std::fs::write(&path, icns))?;
    }
    let path = contents.join("Info.plist");
    io(
        &path,
        std::fs::write(&path, macos_info_plist(info, icns.is_some())),
    )?;
    let path = contents.join("PkgInfo");
    io(&path, std::fs::write(&path, "APPL????"))?;
    Ok(App {
        path: app,
        id: info.id.clone(),
    })
}

/// `images`, each a square PNG of one of [`ICNS_SIZES`], as an `.icns`:
/// the sizes of an iconset, 16 to 512 points at 1x and 2x.
pub fn icns(images: &[(u32, Vec<u8>)]) -> Vec<u8> {
    const TYPES: &[(&[u8; 4], u32)] = &[
        (b"icp4", 16),
        (b"ic11", 32),
        (b"icp5", 32),
        (b"ic12", 64),
        (b"ic07", 128),
        (b"ic13", 256),
        (b"ic08", 256),
        (b"ic14", 512),
        (b"ic09", 512),
        (b"ic10", 1024),
    ];
    let mut body = Vec::new();
    for (kind, size) in TYPES {
        if let Some((_, png)) = images.iter().find(|(s, _)| s == size) {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&(png.len() as u32 + 8).to_be_bytes());
            body.extend_from_slice(png);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"icns");
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// Compiles `icon`, a 1024-pixel opaque PNG, into the iOS app `app`'s asset
/// catalog with `actool`, and adds the keys that name it to its Info.plist.
pub fn compile_icons(
    app: &Path,
    icon: &[u8],
    minimum_os: &str,
    scratch: &Path,
) -> Result<(), String> {
    let catalog = scratch.join("Assets.xcassets");
    let set = catalog.join("AppIcon.appiconset");
    let _ = std::fs::remove_dir_all(&catalog);
    io(&set, std::fs::create_dir_all(&set))?;
    let about = r#""info":{"author":"viso","version":1}"#;
    for (path, text) in [
        (catalog.join("Contents.json"), format!("{{{about}}}\n")),
        (
            set.join("Contents.json"),
            format!(
                r#"{{"images":[{{"filename":"icon.png","idiom":"universal","platform":"ios","size":"1024x1024"}}],{about}}}"#
            ) + "\n",
        ),
    ] {
        io(&path, std::fs::write(&path, text))?;
    }
    let png = set.join("icon.png");
    io(&png, std::fs::write(&png, icon))?;
    let partial = scratch.join("icons.plist");
    run(Command::new("xcrun")
        .arg("actool")
        .arg(&catalog)
        .arg("--compile")
        .arg(app)
        .args(["--platform", "iphoneos", "--minimum-deployment-target"])
        .arg(minimum_os)
        .args(["--app-icon", "AppIcon", "--target-device", "iphone"])
        .args([
            "--target-device",
            "ipad",
            "--errors",
            "--output-partial-info-plist",
        ])
        .arg(&partial))?;
    run(Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Merge {}", partial.display()))
        .arg(app.join("Info.plist")))
}

/// What a provisioning profile allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provisioning {
    /// The team it belongs to.
    pub team: String,
    /// The app identifier it signs: `<team>.<bundle id>`, or a wildcard
    /// ending in `*`.
    pub app_id: String,
    /// Its entitlements, decoded under the scratch directory.
    pub entitlements: PathBuf,
}

impl Provisioning {
    /// Whether it signs the app `id`.
    pub fn allows(&self, id: &str) -> bool {
        let Some(pattern) = self.app_id.strip_prefix(&format!("{}.", self.team)) else {
            return false;
        };
        match pattern.strip_suffix('*') {
            Some(prefix) => id.starts_with(prefix),
            None => pattern == id,
        }
    }
}

/// Decodes the provisioning profile `profile` under `scratch`.
pub fn provisioning(profile: &Path, scratch: &Path) -> Result<Provisioning, String> {
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
    let raw = |key: &str| {
        output(
            Command::new("plutil")
                .args(["-extract", key, "raw", "-o", "-"])
                .arg(&decoded),
        )
        .map(|value| value.trim().to_owned())
    };
    Ok(Provisioning {
        team: raw("TeamIdentifier.0")?,
        app_id: raw("Entitlements.application-identifier")?,
        entitlements,
    })
}

/// Signs the iOS app `app`: ad-hoc without an `identity`; with a
/// provisioning `profile`, embedding it and signing with its entitlements,
/// decoded under `scratch`.
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
        let provisioning = provisioning(profile, scratch)?;
        codesign
            .arg("--entitlements")
            .arg(&provisioning.entitlements);
    }
    run(codesign.arg(app))
}

/// Signs `path`, a macOS app or disk image: with `identity` under the
/// hardened runtime, entitled to `access`, and timestamped; ad-hoc without.
pub fn sign_macos(
    path: &Path,
    identity: Option<&str>,
    access: &[Access],
    scratch: &Path,
) -> Result<(), String> {
    let mut codesign = Command::new("codesign");
    codesign.arg("--force");
    match identity {
        Some(identity) => {
            codesign
                .args(["--options", "runtime", "--timestamp", "--sign"])
                .arg(identity);
            let keys: Vec<(&'static str, Value<'_>)> = access
                .iter()
                .filter_map(|a| entitlement(*a))
                .map(|key| (key, Value::Bool(true)))
                .collect();
            if !keys.is_empty() && path.extension().is_some_and(|x| x == "app") {
                io(scratch, std::fs::create_dir_all(scratch))?;
                let file = scratch.join("app.entitlements");
                io(&file, std::fs::write(&file, plist(keys)))?;
                codesign.arg("--entitlements").arg(&file);
            }
        }
        None => {
            codesign.args(["--sign", "-"]);
        }
    }
    run(codesign.arg(path))
}

/// The names of the valid code signing identities in the keychain.
pub fn identities() -> Result<Vec<String>, String> {
    let text = output(Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]))?;
    Ok(parse_identities(&text))
}

/// The quoted names `security find-identity` lists.
fn parse_identities(text: &str) -> Vec<String> {
    text.lines()
        .filter(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit()))
        .filter_map(|line| {
            let start = line.find('"')? + 1;
            let end = line.rfind('"')?;
            (end > start).then(|| line[start..end].to_owned())
        })
        .collect()
}

/// The disk image `out` holding `app` beside a link to `/Applications`,
/// compressed, under the volume name `volume`; staged under `scratch`.
pub fn dmg(app: &Path, out: &Path, volume: &str, scratch: &Path) -> Result<(), String> {
    let stage = scratch.join("dmg");
    let _ = std::fs::remove_dir_all(&stage);
    io(&stage, std::fs::create_dir_all(&stage))?;
    let name = app.file_name().ok_or("the app has no name")?;
    // `ditto` keeps the signature's extended attributes as they are.
    run(Command::new("ditto").arg(app).arg(stage.join(name)))?;
    #[cfg(unix)]
    {
        let link = stage.join("Applications");
        io(&link, std::os::unix::fs::symlink("/Applications", &link))?;
    }
    let _ = std::fs::remove_file(out);
    run(Command::new("hdiutil")
        .args(["create", "-quiet", "-volname", volume, "-srcfolder"])
        .arg(&stage)
        .args([
            "-ov",
            "-fs",
            "HFS+",
            "-format",
            "UDZO",
            "-imagekey",
            "zlib-level=9",
        ])
        .arg(out))?;
    let _ = std::fs::remove_dir_all(&stage);
    Ok(())
}

/// The `.ipa` `out` of the signed iOS app `app`: it under `Payload/`.
pub fn ipa(app: &Path, out: &Path) -> Result<(), String> {
    let name = app
        .file_name()
        .ok_or("the app has no name")?
        .to_string_lossy();
    let mut entries = vec![Entry {
        name: "Payload/".into(),
        kind: Kind::Directory,
        data: Vec::new(),
        stored: true,
    }];
    entries.extend(archive::entries(app, &format!("Payload/{name}/"), &|_| {
        false
    })?);
    archive::zip(&mut entries, out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes() -> Info {
        Info {
            executable: "notes".into(),
            id: "com.example.notes".into(),
            display_name: "Notes & More".into(),
            version: "2.1.0-beta.1".into(),
            build: 42,
            minimum_os: "17.0".into(),
            orientations: Orientations::Portrait,
            usage: vec![
                (Access::Camera, "Scans receipts".into()),
                (Access::Notifications, "Reminds you".into()),
            ],
        }
    }

    #[test]
    fn the_plist_says_what_the_app_is() {
        let dev = info_plist(&Info::dev("hello_world"), true);
        assert!(dev.contains("<string>hello_world</string>"));
        assert!(dev.contains("<string>dev.viso.hello-world</string>"));
        assert!(dev.contains("<string>iPhoneSimulator</string>"));
        assert!(dev.contains("<key>UIRequiresFullScreen</key>\n\t<false/>"));

        let plist = info_plist(&notes(), false);
        assert!(plist.contains("<string>iPhoneOS</string>"));
        assert!(plist.contains("<string>Notes &amp; More</string>"));
        assert!(plist.contains("<key>CFBundleShortVersionString</key>\n\t<string>2.1.0</string>"));
        assert!(plist.contains("<key>CFBundleVersion</key>\n\t<string>42</string>"));
        assert!(plist.contains("<key>MinimumOSVersion</key>\n\t<string>17.0</string>"));
        assert!(
            plist
                .contains("<key>NSCameraUsageDescription</key>\n\t<string>Scans receipts</string>")
        );
        assert!(!plist.contains("Reminds you"));
        assert!(!plist.contains("Landscape"));
        assert!(plist.contains("PortraitUpsideDown"));

        let mac = macos_info_plist(&notes(), true);
        assert!(mac.contains("<key>CFBundleIconFile</key>\n\t<string>AppIcon</string>"));
        assert!(mac.contains("NSHighResolutionCapable"));
        assert!(!mac.contains("UISupportedInterfaceOrientations"));
    }

    #[test]
    fn a_profile_signs_its_own_app_or_its_wildcard() {
        let profile = |app_id: &str| Provisioning {
            team: "ABCDE12345".into(),
            app_id: app_id.into(),
            entitlements: PathBuf::new(),
        };
        assert!(profile("ABCDE12345.com.example.notes").allows("com.example.notes"));
        assert!(!profile("ABCDE12345.com.example.notes").allows("com.example.other"));
        assert!(profile("ABCDE12345.com.example.*").allows("com.example.notes"));
        assert!(profile("ABCDE12345.*").allows("org.any"));
        assert!(!profile("OTHER.*").allows("org.any"));
    }

    #[test]
    fn identities_are_the_quoted_names() {
        let text = "  1) 0123ABCD \"Developer ID Application: Example (ABCDE12345)\"\n  \
                    2) 4567EF01 \"Apple Development: me@example.com (XYZ)\"\n     \
                    2 valid identities found\n";
        assert_eq!(
            parse_identities(text),
            [
                "Developer ID Application: Example (ABCDE12345)",
                "Apple Development: me@example.com (XYZ)"
            ]
        );
    }

    #[test]
    fn an_icns_holds_each_size_under_its_types() {
        let images: Vec<(u32, Vec<u8>)> =
            ICNS_SIZES.iter().map(|&s| (s, vec![s as u8; 3])).collect();
        let icns = icns(&images);
        assert_eq!(&icns[..4], b"icns");
        assert_eq!(
            u32::from_be_bytes(icns[4..8].try_into().unwrap()) as usize,
            icns.len()
        );
        // Ten entries of 8 + 3 bytes: each iconset size at 1x and 2x.
        assert_eq!(icns.len(), 8 + 10 * 11);
        assert_eq!(&icns[8..12], b"icp4");
    }

    #[test]
    fn a_bundle_holds_the_executable_and_its_plist() {
        let dir = std::env::temp_dir().join(format!("viso-package-app-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let binary = dir.join("built");
        std::fs::write(&binary, b"exe").unwrap();
        let app = bundle(&binary, &dir, &Info::dev("demo"), true).unwrap();
        assert_eq!(app.path, dir.join("demo.app"));
        assert_eq!(app.id, "dev.viso.demo");
        assert_eq!(std::fs::read(app.path.join("demo")).unwrap(), b"exe");
        assert!(app.path.join("Info.plist").is_file());

        let mac = macos_bundle(&binary, &dir, "Notes & More", &notes(), Some(b"icns")).unwrap();
        assert_eq!(mac.path, dir.join("Notes & More.app"));
        assert_eq!(
            std::fs::read(mac.path.join("Contents/MacOS/notes")).unwrap(),
            b"exe"
        );
        assert!(mac.path.join("Contents/Resources/AppIcon.icns").is_file());
        assert!(mac.path.join("Contents/Info.plist").is_file());

        let out = dir.join("demo.ipa");
        ipa(&app.path, &out).unwrap();
        let entries = archive::unzip(&std::fs::read(&out).unwrap()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Payload/",
                "Payload/demo.app/",
                "Payload/demo.app/Info.plist",
                "Payload/demo.app/demo"
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
