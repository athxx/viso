//! Apple app bundles: the iOS `.app` and the `.ipa` it ships in, the macOS
//! `.app` and its `.dmg`, with their Info.plist, icons and signatures.
//!
//! A simulator app is signed ad-hoc and needs no identity or provisioning; a
//! device app is signed with an identity and embeds the provisioning profile
//! whose entitlements it is signed with. A macOS app signed with an identity
//! runs under the hardened runtime, entitled to what it asks the user for,
//! and is timestamped, as notarization requires.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;

use crate::archive::{self, Entry, Kind};
use crate::fragment::{self, Fragment, Problem};
use crate::plist::{self, Dict, Value};
use crate::{Access, Links, Orientations, io, output, run};

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
    /// The URLs that open it.
    pub links: Links,
    /// The Info.plist fragments of the packages it links, merged in.
    pub fragments: Vec<Fragment>,
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
            links: Links::default(),
            fragments: Vec::new(),
        }
    }

    /// `CFBundleShortVersionString`: one to three numbers, without a
    /// prerelease or build suffix.
    fn short_version(&self) -> &str {
        let end = self.version.find(['-', '+']).unwrap_or(self.version.len());
        &self.version[..end]
    }

    /// The keys every bundle has.
    fn common(&self) -> Dict {
        let name: String = self.display_name.chars().take(15).collect();
        Dict::of(vec![
            ("CFBundleDevelopmentRegion", Value::string("en")),
            ("CFBundleDisplayName", Value::string(&self.display_name)),
            ("CFBundleExecutable", Value::string(&self.executable)),
            ("CFBundleIdentifier", Value::string(&self.id)),
            ("CFBundleInfoDictionaryVersion", Value::string("6.0")),
            ("CFBundleName", Value::String(name)),
            ("CFBundlePackageType", Value::string("APPL")),
            (
                "CFBundleShortVersionString",
                Value::string(self.short_version()),
            ),
            ("CFBundleVersion", Value::String(self.build.to_string())),
        ])
    }

    /// `CFBundleURLTypes`: the schemes that open it, under its bundle id.
    fn url_types(&self) -> Option<(&'static str, Value)> {
        if self.links.schemes.is_empty() {
            return None;
        }
        let schemes = self.links.schemes.iter().map(Value::string).collect();
        Some((
            "CFBundleURLTypes",
            Value::Array(vec![Value::Dict(Dict::of(vec![
                ("CFBundleTypeRole", Value::string("Viewer")),
                ("CFBundleURLName", Value::string(&self.id)),
                ("CFBundleURLSchemes", Value::Array(schemes)),
            ]))]),
        ))
    }

    /// The usage description keys of what it asks for.
    fn usage(&self, macos: bool) -> impl Iterator<Item = (&'static str, Value)> + '_ {
        self.usage.iter().flat_map(move |(access, reason)| {
            usage_keys(*access, macos)
                .iter()
                .map(move |key| (*key, Value::string(reason)))
        })
    }

    /// `dict` with the fragments merged in, as a property list file.
    fn finish(&self, mut dict: Dict) -> Result<String, Problem> {
        fragment::merge_plist(&mut dict, GENERATED, &self.fragments)?;
        Ok(plist::write(&dict))
    }
}

/// What the keys Viso writes itself are, as a message names them.
const GENERATED: &str = "what Viso writes from `Viso.toml`";

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
        // Touch ID asks with no reason in the plist.
        Access::Biometrics if macos => &[],
        Access::Biometrics => &["NSFaceIDUsageDescription"],
        // Asked for at run time, with no reason in the plist.
        Access::Notifications => &[],
        // Apple apps reach the network without asking.
        Access::Network => &[],
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
        Access::Bluetooth | Access::Notifications | Access::Biometrics | Access::Network => None,
    }
}

/// An iOS app's Info.plist: one full-screen scene in the orientations it
/// turns to, on iPhone and iPad, with its fragments merged in.
pub fn info_plist(info: &Info, simulator: bool) -> Result<String, Problem> {
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
    let list =
        |names: Vec<&'static str>| Value::Array(names.into_iter().map(Value::string).collect());
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
            Value::Array(vec![Value::string(platform)]),
        ),
        ("LSRequiresIPhoneOS", Value::Bool(true)),
        ("MinimumOSVersion", Value::string(&info.minimum_os)),
        (
            "UIDeviceFamily",
            Value::Array(vec![Value::Integer(1), Value::Integer(2)]),
        ),
        ("UILaunchScreen", Value::Dict(Dict::default())),
        (
            "UIApplicationSceneManifest",
            Value::Dict(Dict::of(vec![(
                "UIApplicationSupportsMultipleScenes",
                Value::Bool(false),
            )])),
        ),
        // An app that does not turn every way fills the iPad's screen.
        (
            "UIRequiresFullScreen",
            Value::Bool(info.orientations != Orientations::Any),
        ),
        ("UISupportedInterfaceOrientations", list(phone)),
        ("UISupportedInterfaceOrientations~ipad", list(pad)),
        // An iPhone's ProMotion display runs above 60 Hz for an app only
        // with this; the display link then asks for the screen's most.
        ("CADisableMinimumFrameDurationOnPhone", Value::Bool(true)),
    ]);
    dict.extend(info.url_types());
    dict.extend(info.usage(false));
    info.finish(dict)
}

/// A macOS app's Info.plist, naming its icon `AppIcon` when it has one,
/// with its fragments merged in.
pub fn macos_info_plist(info: &Info, icon: bool) -> Result<String, Problem> {
    let mut dict = info.common();
    if icon {
        dict.set("CFBundleIconFile", Value::string("AppIcon"));
    }
    dict.extend([
        (
            "LSApplicationCategoryType",
            Value::string("public.app-category.productivity"),
        ),
        ("NSHighResolutionCapable", Value::Bool(true)),
        ("NSPrincipalClass", Value::string("NSApplication")),
        ("NSSupportsAutomaticGraphicsSwitching", Value::Bool(true)),
    ]);
    dict.extend(info.url_types());
    dict.extend(info.usage(true));
    info.finish(dict)
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
    let plist = info_plist(info, simulator).map_err(|problem| problem.to_string())?;
    io(&path, std::fs::write(&path, plist))?;
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
    let plist = macos_info_plist(info, icns.is_some()).map_err(|problem| problem.to_string())?;
    io(&path, std::fs::write(&path, plist))?;
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
    // iOS names the app `application-identifier`, macOS
    // `com.apple.application-identifier`.
    let allowed = io(&entitlements, std::fs::read_to_string(&entitlements))?;
    let allowed = plist::read(&allowed, 0).map_err(|e| {
        format!(
            "the provisioning profile's entitlements do not read: {}",
            e.message
        )
    })?;
    let app_id = ["application-identifier", MACOS_APP_ID]
        .iter()
        .find_map(|key| match allowed.get(key) {
            Some(Value::String(id)) => Some(id.clone()),
            _ => None,
        })
        .ok_or("the provisioning profile names no app")?;
    Ok(Provisioning {
        team: raw("TeamIdentifier.0")?,
        app_id,
        entitlements,
    })
}

/// The entitlement that names the hosts whose links open an app.
const ASSOCIATED_DOMAINS: &str = "com.apple.developer.associated-domains";

/// The associated domains of `hosts`: each as a link host.
pub fn associated_domains(hosts: &[String]) -> Vec<String> {
    hosts
        .iter()
        .map(|host| format!("applinks:{host}"))
        .collect()
}

/// What the entitlements Viso writes itself are, as a message names them.
const GENERATED_ENTITLEMENTS: &str = "the entitlements of `[package.links]`";

/// What an app is entitled to: to open the links of `hosts`, and what the
/// entitlements fragments of the packages it links ask for.
#[derive(Debug, Clone, Copy, Default)]
pub struct Entitlements<'a> {
    pub hosts: &'a [String],
    pub fragments: &'a [Fragment],
}

impl Entitlements<'_> {
    /// Those a simulator app has. The simulator runs it as a process of this
    /// Mac, which refuses an ad-hoc signature carrying what only a
    /// provisioning profile allows; so they are linked into the executable
    /// (see [`simulated_entitlements`]), where the simulator reads them,
    /// and the signature carries none.
    pub fn simulated(&self) -> Result<Dict, Problem> {
        let mut dict = Dict::default();
        let domains = associated_domains(self.hosts);
        if !domains.is_empty() {
            dict.set(
                ASSOCIATED_DOMAINS,
                Value::Array(domains.into_iter().map(Value::String).collect()),
            );
        }
        fragment::merge_plist(&mut dict, GENERATED_ENTITLEMENTS, self.fragments)?;
        Ok(dict)
    }

    /// Those signed under a provisioning profile for `app_id` that allows
    /// `allowed`: the profile's own, the link hosts in place of its
    /// associated domains. Each key a fragment asks for must be one the
    /// profile allows, and is signed with the profile's value, which says
    /// what it is for (a push environment, a group).
    fn provisioned(&self, mut allowed: Dict, app_id: &str) -> Result<Dict, String> {
        let domains = associated_domains(self.hosts);
        if !domains.is_empty() {
            if allowed.get(ASSOCIATED_DOMAINS).is_none() {
                return Err(format!(
                    "the provisioning profile does not allow Associated Domains, which the hosts \
                     of `[package.links]` need: enable it for `{app_id}` and download the profile \
                     again"
                ));
            }
            allowed.set(
                ASSOCIATED_DOMAINS,
                Value::Array(domains.into_iter().map(Value::String).collect()),
            );
        }
        let mut asked = Dict::default();
        fragment::merge_plist(&mut asked, GENERATED_ENTITLEMENTS, self.fragments)
            .map_err(|problem| problem.to_string())?;
        if let Some(entry) = asked.0.iter().find(|e| allowed.get(&e.key).is_none()) {
            return Err(format!(
                "the provisioning profile does not allow `{}`, which `{}` needs: enable its \
                 capability for `{app_id}` and download the profile again",
                entry.key,
                self.fragments[entry.from - 1].package
            ));
        }
        Ok(allowed)
    }
}

/// Signs the iOS app `app`: ad-hoc without an `identity`, as a simulator
/// app whose entitlements are linked in; with a provisioning `profile`,
/// embedding it and signing with its entitlements, decoded under
/// `scratch`, which must allow what `entitlements` asks for.
pub fn sign(
    app: &Path,
    identity: Option<&str>,
    profile: Option<&Path>,
    entitlements: &Entitlements<'_>,
    scratch: &Path,
) -> Result<(), String> {
    let mut codesign = Command::new("codesign");
    codesign
        .args(["--force", "--timestamp=none", "--sign"])
        .arg(identity.unwrap_or("-"));
    let signed = match profile {
        Some(profile) => {
            io(
                profile,
                std::fs::copy(profile, app.join("embedded.mobileprovision")),
            )?;
            let provisioning = provisioning(profile, scratch)?;
            let file = &provisioning.entitlements;
            let text = io(file, std::fs::read_to_string(file))?;
            let allowed = plist::read(&text, 0).map_err(|e| {
                format!(
                    "the provisioning profile's entitlements do not read: {}",
                    e.message
                )
            })?;
            Some(entitlements.provisioned(allowed, &provisioning.app_id)?)
        }
        None => None,
    };
    if let Some(dict) = signed {
        io(scratch, std::fs::create_dir_all(scratch))?;
        let file = scratch.join("app.entitlements");
        io(&file, std::fs::write(&file, plist::write(&dict)))?;
        codesign.arg("--entitlements").arg(&file);
    }
    run(codesign.arg(app))
}

/// The `rustc` arguments that link `entitlements` into a simulator app's
/// executable, as the simulator reads them: the property list in its
/// `__TEXT,__entitlements` section and, DER-encoded, in `__TEXT,__ents_der`.
/// Each is written in `dir` under a name its content gives, so a change
/// links again. None for no entitlements.
pub fn simulated_entitlements(entitlements: &Dict, dir: &Path) -> Result<Vec<String>, String> {
    if entitlements.0.is_empty() {
        return Ok(Vec::new());
    }
    io(dir, std::fs::create_dir_all(dir))?;
    let xml = plist::write(entitlements).into_bytes();
    let der = plist::der(entitlements);
    let mut args = Vec::new();
    for (section, bytes, extension) in [
        ("__entitlements", &xml, "plist"),
        ("__ents_der", &der, "der"),
    ] {
        let digest = sha2::Sha256::digest(bytes);
        let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!("entitlements-{name}.{extension}"));
        if !path.is_file() {
            io(&path, std::fs::write(&path, bytes))?;
        }
        args.push(format!(
            "-Clink-arg=-Wl,-sectcreate,__TEXT,{section},{}",
            path.display()
        ));
    }
    Ok(args)
}

/// The entitlements a macOS app signed under a provisioning profile names
/// itself and its team by.
const MACOS_APP_ID: &str = "com.apple.application-identifier";
const MACOS_TEAM_ID: &str = "com.apple.developer.team-identifier";

/// What a macOS app signed under the Developer ID provisioning `profile`
/// is entitled to: `entitlements`, those of the hardened runtime, and what
/// the entitlements `fragments` ask for that only a profile allows, each
/// with the profile's value, beside the app and team identifiers. The
/// profile must allow each; `app` embeds it. Decoded under `scratch`.
pub fn macos_provisioned(
    app: &Path,
    profile: &Path,
    mut entitlements: Dict,
    fragments: &[Fragment],
    scratch: &Path,
) -> Result<Dict, String> {
    let embedded = app.join("Contents/embedded.provisionprofile");
    io(profile, std::fs::copy(profile, &embedded))?;
    let provisioning = provisioning(profile, scratch)?;
    let file = &provisioning.entitlements;
    let allowed = plist::read(&io(file, std::fs::read_to_string(file))?, 0).map_err(|e| {
        format!(
            "the provisioning profile's entitlements do not read: {}",
            e.message
        )
    })?;
    provisioned_macos(&mut entitlements, &allowed, fragments, &provisioning.app_id)?;
    Ok(entitlements)
}

/// Adds to `entitlements` what `fragments` ask for that only a profile
/// allows, with `allowed`'s values, and the app and team identifiers.
fn provisioned_macos(
    entitlements: &mut Dict,
    allowed: &Dict,
    fragments: &[Fragment],
    app_id: &str,
) -> Result<(), String> {
    let mut asked = Dict::default();
    fragment::merge_plist(&mut asked, GENERATED_ENTITLEMENTS, fragments)
        .map_err(|problem| problem.to_string())?;
    for entry in asked.0.iter().filter(|e| restricted(&e.key)) {
        let Some(value) = allowed.get(&entry.key) else {
            return Err(format!(
                "the provisioning profile does not allow `{}`, which `{}` needs: enable its \
                 capability for `{app_id}` and download the profile again",
                entry.key,
                fragments[entry.from - 1].package
            ));
        };
        entitlements.set(&entry.key, value.clone());
    }
    for key in [MACOS_APP_ID, MACOS_TEAM_ID] {
        if let Some(value) = allowed.get(key) {
            entitlements.set(key, value.clone());
        }
    }
    Ok(())
}

/// Whether `key` is an entitlement only a provisioning profile allows.
fn restricted(key: &str) -> bool {
    key.starts_with("com.apple.developer.") || key == "aps-environment"
}

/// What a macOS app signed with an identity is entitled to under the
/// hardened runtime: what `access` asks the user for, and what the
/// entitlements `fragments` ask for but the keys only a provisioning
/// profile allows, which a macOS package does not embed. Those are left
/// out, each answered with the package that asks for it.
pub fn macos_entitlements(
    access: &[Access],
    fragments: &[Fragment],
) -> Result<(Dict, Vec<(String, String)>), Problem> {
    let mut dict = Dict::default();
    for key in access.iter().filter_map(|a| entitlement(*a)) {
        dict.set(key, Value::Bool(true));
    }
    fragment::merge_plist(
        &mut dict,
        "the entitlements of `[package.permissions]`",
        fragments,
    )?;
    let mut left_out = Vec::new();
    dict.0.retain(|entry| {
        let keep = !restricted(&entry.key);
        if !keep {
            left_out.push((entry.key.clone(), fragments[entry.from - 1].package.clone()));
        }
        keep
    });
    Ok((dict, left_out))
}

/// Signs `path`, a macOS app or disk image: with `identity` under the
/// hardened runtime, an app entitled to `entitlements`, and timestamped;
/// ad-hoc without.
pub fn sign_macos(
    path: &Path,
    identity: Option<&str>,
    entitlements: &Dict,
    scratch: &Path,
) -> Result<(), String> {
    let mut codesign = Command::new("codesign");
    codesign.arg("--force");
    match identity {
        Some(identity) => {
            codesign
                .args(["--options", "runtime", "--timestamp", "--sign"])
                .arg(identity);
            if !entitlements.0.is_empty() && path.extension().is_some_and(|x| x == "app") {
                io(scratch, std::fs::create_dir_all(scratch))?;
                let file = scratch.join("app.entitlements");
                io(&file, std::fs::write(&file, plist::write(entitlements)))?;
                codesign.arg("--entitlements").arg(&file);
            }
        }
        None => {
            codesign.args(["--sign", "-"]);
        }
    }
    run(codesign.arg(path))
}

/// Who submits to Apple's notary service, never holding a secret: a
/// keychain profile `xcrun notarytool store-credentials` saved, or an App
/// Store Connect API key, its private key a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notary {
    Profile(String),
    ApiKey {
        key: PathBuf,
        key_id: String,
        issuer: String,
    },
}

impl Notary {
    /// The `notarytool` arguments that authenticate as it.
    fn args(&self, notarytool: &mut Command) {
        match self {
            Notary::Profile(profile) => {
                notarytool.arg("--keychain-profile").arg(profile);
            }
            Notary::ApiKey {
                key,
                key_id,
                issuer,
            } => {
                notarytool
                    .arg("--key")
                    .arg(key)
                    .args(["--key-id", key_id, "--issuer", issuer]);
            }
        }
    }

    fn notarytool(&self, args: &[&str]) -> Command {
        let mut notarytool = Command::new("xcrun");
        notarytool.arg("notarytool").args(args);
        self.args(&mut notarytool);
        notarytool
    }
}

/// Notarizes `path`, an app or a disk image signed with a Developer ID
/// identity, and staples the ticket to it, so Gatekeeper opens it on any
/// Mac, offline too. An app is submitted zipped, staged under `scratch`.
pub fn notarize(path: &Path, notary: &Notary, scratch: &Path) -> Result<(), String> {
    let app = path.extension().is_some_and(|x| x == "app");
    let submitted = if app {
        io(scratch, std::fs::create_dir_all(scratch))?;
        let zip = scratch.join("notarize.zip");
        let _ = std::fs::remove_file(&zip);
        run(Command::new("ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(path)
            .arg(&zip))?;
        zip
    } else {
        path.to_owned()
    };
    let mut submit = notary.notarytool(&["submit", "--output-format", "plist"]);
    let id = submission(&output(submit.arg(&submitted))?, "id")?;
    if app {
        let _ = std::fs::remove_file(&submitted);
    }
    // `wait` fails on a rejected submission as on a lost connection; `info`
    // tells which.
    let waited = run(&mut notary.notarytool(&["wait", &id]));
    let mut info = notary.notarytool(&["info", &id, "--output-format", "plist"]);
    let status = submission(&output(&mut info)?, "status")?;
    if status != "Accepted" {
        let log = output(&mut notary.notarytool(&["log", &id])).unwrap_or_default();
        let waited = waited.err().map(|e| format!(" ({e})")).unwrap_or_default();
        return Err(format!(
            "the notary service answered `{status}` for the submission `{id}`{waited}\n{}",
            log.trim()
        ));
    }
    run(Command::new("xcrun").args(["stapler", "staple"]).arg(path))
}

/// The string `key` of the property list `notarytool` wrote.
fn submission(text: &str, key: &str) -> Result<String, String> {
    let dict = plist::read(text.trim(), 0).map_err(|e| {
        format!(
            "notarytool wrote what does not read as a property list: {}",
            e.message
        )
    })?;
    match dict.get(key) {
        Some(Value::String(value)) => Ok(value.clone()),
        _ => Err(format!("notarytool's answer has no `{key}`")),
    }
}

/// Whether the Info.plist and entitlements fragments of an app described
/// by `info` merge with what Viso writes and with each other, for macOS or
/// iOS.
pub fn check(info: &Info, entitlements: &[Fragment], macos: bool) -> Result<(), Problem> {
    if macos {
        macos_info_plist(info, false)?;
        macos_entitlements(&[], entitlements)?;
    } else {
        info_plist(info, true)?;
        Entitlements {
            hosts: &info.links.hosts,
            fragments: entitlements,
        }
        .simulated()?;
    }
    Ok(())
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

    #[test]
    fn notarytool_answers_read_for_their_id_and_status() {
        let answer = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>id</key>
	<string>2efe2717-52ef-43a5-96dc-0797e4ca1041</string>
	<key>message</key>
	<string>Successfully uploaded file</string>
	<key>status</key>
	<string>Invalid</string>
</dict>
</plist>
"#;
        assert_eq!(
            submission(answer, "id").unwrap(),
            "2efe2717-52ef-43a5-96dc-0797e4ca1041"
        );
        assert_eq!(submission(answer, "status").unwrap(), "Invalid");
        assert!(submission(answer, "name").is_err());
        assert!(submission("Error: HTTP status code: 401", "id").is_err());
    }

    #[test]
    fn a_notary_authenticates_by_its_profile_or_its_key() {
        let args = |notary: &Notary| -> Vec<String> {
            notary
                .notarytool(&["info", "x"])
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(&Notary::Profile("viso".into())),
            ["notarytool", "info", "x", "--keychain-profile", "viso"]
        );
        let key = Notary::ApiKey {
            key: "AuthKey_ABC.p8".into(),
            key_id: "ABC".into(),
            issuer: "69a6de7e".into(),
        };
        assert_eq!(
            args(&key),
            [
                "notarytool",
                "info",
                "x",
                "--key",
                "AuthKey_ABC.p8",
                "--key-id",
                "ABC",
                "--issuer",
                "69a6de7e"
            ]
        );
    }

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
            links: Links {
                schemes: vec!["notes".into(), "x-notes".into()],
                hosts: vec!["notes.example.com".into()],
            },
            fragments: Vec::new(),
        }
    }

    fn fragment(package: &str, file: &str, body: &str) -> Fragment {
        Fragment {
            package: package.into(),
            path: PathBuf::from(format!("/p/{package}/{file}")),
            text: format!(
                "<?xml version=\"1.0\"?>\n<plist version=\"1.0\">\n<dict>\n{body}</dict>\n</plist>\n"
            ),
        }
    }

    #[test]
    fn the_plist_says_what_the_app_is() {
        let dev = info_plist(&Info::dev("hello_world"), true).unwrap();
        assert!(dev.contains("<string>hello_world</string>"));
        assert!(dev.contains("<string>dev.viso.hello-world</string>"));
        assert!(dev.contains("<string>iPhoneSimulator</string>"));
        assert!(dev.contains("<key>UIRequiresFullScreen</key>\n\t<false/>"));
        assert!(dev.contains("<key>CADisableMinimumFrameDurationOnPhone</key>\n\t<true/>"));

        let plist = info_plist(&notes(), false).unwrap();
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

        let url_types = "<key>CFBundleURLTypes</key>\n\t<array>\n\t\t<dict>\n\
                         \t\t\t<key>CFBundleTypeRole</key>\n\t\t\t<string>Viewer</string>\n\
                         \t\t\t<key>CFBundleURLName</key>\n\t\t\t<string>com.example.notes</string>\n\
                         \t\t\t<key>CFBundleURLSchemes</key>\n\t\t\t<array>\n\
                         \t\t\t\t<string>notes</string>\n\t\t\t\t<string>x-notes</string>\n\
                         \t\t\t</array>\n\t\t</dict>\n\t</array>\n";
        assert!(plist.contains(url_types), "{plist}");
        assert!(!dev.contains("CFBundleURLTypes"));

        let mac = macos_info_plist(&notes(), true).unwrap();
        assert!(mac.contains(url_types));
        assert!(mac.contains("<key>CFBundleIconFile</key>\n\t<string>AppIcon</string>"));
        assert!(mac.contains("NSHighResolutionCapable"));
        assert!(!mac.contains("UISupportedInterfaceOrientations"));
    }

    #[test]
    fn fragments_merge_into_the_plist() {
        let mut info = notes();
        info.usage
            .push((Access::Biometrics, "Unlocks your notes".into()));
        info.fragments = vec![
            fragment(
                "viso-background",
                "ios/Info.plist",
                "<key>UIBackgroundModes</key><array><string>fetch</string><string>processing</string></array>\n",
            ),
            fragment(
                "viso-push",
                "ios/Info.plist",
                "<key>UIBackgroundModes</key><array><string>remote-notification</string></array>\n",
            ),
            fragment(
                "notes",
                "ios/Info.plist",
                "<key>BGTaskSchedulerPermittedIdentifiers</key><array><string>com.example.notes.sync</string></array>\n\
                 <key>CFBundleDevelopmentRegion</key><string>en</string>\n",
            ),
        ];
        let plist = info_plist(&info, true).unwrap();
        let dict = plist::read(&plist, 0).unwrap();
        assert_eq!(
            dict.get("UIBackgroundModes"),
            Some(&Value::Array(
                ["fetch", "processing", "remote-notification"]
                    .map(Value::string)
                    .to_vec()
            ))
        );
        assert_eq!(
            dict.get("BGTaskSchedulerPermittedIdentifiers"),
            Some(&Value::Array(vec![Value::string("com.example.notes.sync")]))
        );
        assert_eq!(
            dict.get("NSFaceIDUsageDescription"),
            Some(&Value::string("Unlocks your notes"))
        );
        assert!(
            !macos_info_plist(&info, false)
                .unwrap()
                .contains("NSFaceIDUsageDescription")
        );

        info.fragments.push(fragment(
            "viso-odd",
            "ios/Info.plist",
            "<key>CFBundleIdentifier</key><string>com.example.other</string>\n",
        ));
        let problem = info_plist(&info, true).unwrap_err();
        assert_eq!(problem.kind, fragment::Kind::Conflict);
        assert_eq!(
            problem.message,
            "`CFBundleIdentifier` is \"com.example.other\" in `viso-odd`'s `ios/Info.plist` but \
             \"com.example.notes\" in what Viso writes from `Viso.toml`"
        );
        let broken = Info {
            fragments: vec![fragment("viso-bad", "ios/Info.plist", "<key>a</key>\n")],
            ..notes()
        };
        let problem = check(&broken, &[], false).unwrap_err();
        assert_eq!(problem.kind, fragment::Kind::Invalid);
        assert_eq!(problem.message, "`a` has no value");
    }

    #[test]
    fn entitlements_come_from_the_links_and_the_fragments() {
        let push = [fragment(
            "viso-push",
            "ios/entitlements.plist",
            "<key>aps-environment</key><string>development</string>\n",
        )];
        let hosts = ["notes.example.com".to_owned()];
        let entitlements = Entitlements {
            hosts: &hosts,
            fragments: &push,
        };
        let simulated = entitlements.simulated().unwrap();
        assert_eq!(
            simulated.keys().collect::<Vec<_>>(),
            [ASSOCIATED_DOMAINS, "aps-environment"]
        );
        assert_eq!(
            Entitlements::default().simulated().unwrap(),
            Dict::default()
        );
        let dir = std::env::temp_dir().join(format!("viso-simulated-{}", std::process::id()));
        let args = simulated_entitlements(&simulated, &dir).unwrap();
        assert_eq!(args.len(), 2);
        assert!(args[0].starts_with("-Clink-arg=-Wl,-sectcreate,__TEXT,__entitlements,"));
        assert!(args[1].contains(",__TEXT,__ents_der,") && args[1].ends_with(".der"));
        assert_eq!(simulated_entitlements(&simulated, &dir).unwrap(), args);
        assert!(
            simulated_entitlements(&Dict::default(), &dir)
                .unwrap()
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();

        // A profile's value is the one signed; what it lacks is an error.
        let profile = |entries: Vec<(&str, Value)>| Dict::of(entries);
        let signed = entitlements
            .provisioned(
                profile(vec![
                    ("aps-environment", Value::string("production")),
                    (ASSOCIATED_DOMAINS, Value::string("*")),
                ]),
                "T.com.example.notes",
            )
            .unwrap();
        assert_eq!(
            signed.get("aps-environment"),
            Some(&Value::string("production"))
        );
        assert_eq!(
            signed.get(ASSOCIATED_DOMAINS),
            Some(&Value::Array(vec![Value::string(
                "applinks:notes.example.com"
            )]))
        );
        let missing = Entitlements {
            hosts: &[],
            fragments: &push,
        }
        .provisioned(profile(vec![]), "T.com.example.notes")
        .unwrap_err();
        assert_eq!(
            missing,
            "the provisioning profile does not allow `aps-environment`, which `viso-push` needs: \
             enable its capability for `T.com.example.notes` and download the profile again"
        );
        assert!(
            entitlements
                .provisioned(
                    profile(vec![("aps-environment", Value::string("x"))]),
                    "T.a"
                )
                .unwrap_err()
                .contains("Associated Domains")
        );

        // Without a profile, what only one allows is left out of a macOS
        // package.
        let mac = [fragment(
            "viso-push",
            "macos/entitlements.plist",
            "<key>com.apple.developer.aps-environment</key><string>development</string>\n\
             <key>com.apple.security.network.client</key><true/>\n",
        )];
        let (dict, left_out) = macos_entitlements(&[Access::Camera], &mac).unwrap();
        assert_eq!(
            dict.keys().collect::<Vec<_>>(),
            [
                "com.apple.security.device.camera",
                "com.apple.security.network.client"
            ]
        );
        assert_eq!(
            left_out,
            [(
                "com.apple.developer.aps-environment".to_owned(),
                "viso-push".to_owned()
            )]
        );
        // Under one, it is signed with the profile's value, beside the app's
        // and the team's identifiers.
        let allowed = Dict::of(vec![
            (
                "com.apple.developer.aps-environment",
                Value::string("production"),
            ),
            (MACOS_APP_ID, Value::string("T.com.example.notes")),
            (MACOS_TEAM_ID, Value::string("T")),
            (
                "keychain-access-groups",
                Value::Array(vec![Value::string("T.*")]),
            ),
        ]);
        let mut signed = dict.clone();
        provisioned_macos(&mut signed, &allowed, &mac, "T.com.example.notes").unwrap();
        assert_eq!(
            signed.get("com.apple.developer.aps-environment"),
            Some(&Value::string("production"))
        );
        assert_eq!(signed.get(MACOS_TEAM_ID), Some(&Value::string("T")));
        assert!(signed.get("keychain-access-groups").is_none());
        assert!(signed.get("com.apple.security.device.camera").is_some());
        let mut refused = dict;
        let lacking = Dict::of(vec![(MACOS_APP_ID, Value::string("T.com.example.notes"))]);
        let why = provisioned_macos(&mut refused, &lacking, &mac, "T.com.example.notes");
        assert!(
            why.unwrap_err()
                .contains("does not allow `com.apple.developer.aps-environment`")
        );
    }

    #[test]
    fn link_hosts_are_associated_domains() {
        assert_eq!(
            associated_domains(&["notes.example.com".into(), "*.example.org".into()]),
            ["applinks:notes.example.com", "applinks:*.example.org"]
        );
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
