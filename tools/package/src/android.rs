//! The Android SDK and NDK, the Cargo build of the app as the shared library
//! the Java shell loads, and the signed APK and app bundle around it.
//!
//! The app's executable is linked as a shared library that keeps `main` and
//! `JNI_OnLoad` exported for the loader and the VM, its segments aligned to
//! 16 KiB so it loads on 16 KiB-page devices. A package holds that library
//! for each ABI, the Java shell compiled to dex, the launcher icon at each
//! density and the manifest filled in for the app. An APK is aligned and
//! signed with `apksigner`; an app bundle (`.aab`), the form a store takes,
//! is laid out as `bundletool` reads it and signed with `jarsigner`. A
//! release key's passwords are read from the environment by the tools
//! themselves and never pass through a command line.
//!
//! The shell's Java and its manifest template come from the packages the app
//! links (see [`Shell`]). A dev APK also asks for the network, for its dev
//! channel; a release APK does not.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::archive::{self, Entry, Kind};
use crate::{Access, Orientations, files_under, io, newest, run};

/// The ABI an app is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    Arm64,
    Armv7,
    X86_64,
    X86,
}

impl Abi {
    /// Every ABI, in the order a package lists them.
    pub const ALL: &'static [Abi] = &[Abi::Arm64, Abi::Armv7, Abi::X86_64, Abi::X86];

    /// The ABI an emulator on this host runs natively.
    pub const fn host() -> Abi {
        if cfg!(target_arch = "x86_64") {
            Abi::X86_64
        } else {
            Abi::Arm64
        }
    }

    /// The ABI `name` names, as an APK's library directory does.
    pub fn parse(name: &str) -> Option<Abi> {
        Abi::ALL.iter().copied().find(|abi| abi.as_str() == name)
    }

    /// The Rust target.
    pub const fn triple(self) -> &'static str {
        match self {
            Abi::Arm64 => "aarch64-linux-android",
            Abi::Armv7 => "armv7-linux-androideabi",
            Abi::X86_64 => "x86_64-linux-android",
            Abi::X86 => "i686-linux-android",
        }
    }

    /// The target the NDK's compiler is named for.
    const fn clang_target(self) -> &'static str {
        match self {
            Abi::Armv7 => "armv7a-linux-androideabi",
            other => other.triple(),
        }
    }

    /// The name of its library directory in an APK and of its system images.
    pub const fn as_str(self) -> &'static str {
        match self {
            Abi::Arm64 => "arm64-v8a",
            Abi::Armv7 => "armeabi-v7a",
            Abi::X86_64 => "x86_64",
            Abi::X86 => "x86",
        }
    }
}

/// The lowest API level an app supports (its `minSdkVersion`) unless the
/// project names a higher one.
pub const MIN_API: u32 = 26;

/// The activity the shell starts in.
pub const ACTIVITY: &str = "dev.viso.VisoActivity";

/// The launcher icon's densities, each with its size in pixels.
pub const ICON_DENSITIES: &[(&str, u32)] = &[
    ("mdpi", 48),
    ("hdpi", 72),
    ("xhdpi", 96),
    ("xxhdpi", 144),
    ("xxxhdpi", 192),
];

/// The arguments after `cargo rustc --` that link the app's executable as the
/// shell's library.
pub const RUSTC_ARGS: &[&str] = &[
    "-A",
    "linker_messages",
    "-Clink-arg=-shared",
    "-Clink-arg=-Wl,--export-dynamic-symbol=main",
    "-Clink-arg=-Wl,--export-dynamic-symbol=JNI_OnLoad",
    "-Clink-arg=-Wl,-u,JNI_OnLoad",
    "-Clink-arg=-Wl,-z,max-page-size=16384",
];

/// The variable a release keystore's password is read from.
pub const KEYSTORE_PASSWORD_ENV: &str = "VISO_ANDROID_KEYSTORE_PASSWORD";
/// The variable its key's password is read from; the keystore's when unset.
pub const KEY_PASSWORD_ENV: &str = "VISO_ANDROID_KEY_PASSWORD";

/// The version `bundletool` reads a bundle as.
const BUNDLETOOL_VERSION: &str = "1.17.2";

/// Where an SDK was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdkOrigin {
    /// The environment variable that names it.
    Variable(&'static str),
    /// The one `viso android use` installed.
    Viso,
    /// The place the Android tools install to by default.
    Default,
}

impl SdkOrigin {
    /// Where it came from, for a reader.
    pub fn describe(self) -> String {
        match self {
            SdkOrigin::Variable(name) => format!("from `{name}`"),
            SdkOrigin::Viso => "installed by `viso android use`".to_owned(),
            SdkOrigin::Default => "at the default location".to_owned(),
        }
    }
}

/// The SDK and NDK tools packaging uses.
#[derive(Debug, Clone)]
pub struct Sdk {
    /// The SDK root.
    pub root: PathBuf,
    /// Where it was found.
    pub origin: SdkOrigin,
    /// The NDK's LLVM `bin` directory.
    pub ndk_bin: PathBuf,
    /// The newest build-tools directory.
    pub build_tools: PathBuf,
    /// The newest platform's `android.jar`.
    pub platform_jar: PathBuf,
    /// `platform-tools/adb`.
    pub adb: PathBuf,
    /// `emulator/emulator`.
    pub emulator: PathBuf,
}

/// The Java shell: its sources' directories and the manifest template, with
/// `{package}`, `{version_code}`, `{version_name}`, `{min_sdk}`,
/// `{permissions}`, `{label}`, `{icon}`, `{orientation}` and `{lib}` to fill
/// in.
#[derive(Debug, Clone)]
pub struct Shell {
    pub java: Vec<PathBuf>,
    pub manifest: PathBuf,
}

/// What kind of APK to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// A development build: optimized dex off, the dev channel's permission
    /// on.
    Dev,
    /// A release build.
    Release,
}

/// What the manifest says of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Its application id.
    pub package: String,
    /// The name under its icon.
    pub label: String,
    pub version_code: u32,
    pub version_name: String,
    pub min_sdk: u32,
    /// What it asks the user for.
    pub access: Vec<Access>,
    /// Whether it may use the network, as a dev build's channel does.
    pub internet: bool,
    pub orientations: Orientations,
    /// Whether it has a launcher icon.
    pub icon: bool,
}

impl Manifest {
    /// A development or `viso build` app's: named after `bin`, version 1,
    /// showing notifications, and on the network for a dev build's channel.
    pub fn dev(bin: &str, flavor: Flavor) -> Manifest {
        Manifest {
            package: package_name(bin),
            label: bin.to_owned(),
            version_code: 1,
            version_name: "1.0".to_owned(),
            min_sdk: MIN_API,
            access: vec![Access::Notifications],
            internet: flavor == Flavor::Dev,
            orientations: Orientations::Any,
            icon: false,
        }
    }
}

/// The `<uses-permission>` lines `access` needs.
fn permissions(access: Access) -> &'static [&'static str] {
    match access {
        Access::Camera => &[r#"<uses-permission android:name="android.permission.CAMERA" />"#],
        Access::Microphone => {
            &[r#"<uses-permission android:name="android.permission.RECORD_AUDIO" />"#]
        }
        Access::Location => &[
            r#"<uses-permission android:name="android.permission.ACCESS_COARSE_LOCATION" />"#,
            r#"<uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />"#,
        ],
        Access::Photos => &[
            r#"<uses-permission android:name="android.permission.READ_MEDIA_IMAGES" />"#,
            r#"<uses-permission android:name="android.permission.READ_EXTERNAL_STORAGE" android:maxSdkVersion="32" />"#,
        ],
        Access::Contacts => {
            &[r#"<uses-permission android:name="android.permission.READ_CONTACTS" />"#]
        }
        Access::Bluetooth => &[
            r#"<uses-permission android:name="android.permission.BLUETOOTH_CONNECT" />"#,
            r#"<uses-permission android:name="android.permission.BLUETOOTH" android:maxSdkVersion="30" />"#,
        ],
        Access::Notifications => {
            &[r#"<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />"#]
        }
    }
}

/// What a package is signed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Key {
    /// The standard debug keystore, created if missing.
    Debug,
    /// A release keystore, its passwords in [`KEYSTORE_PASSWORD_ENV`] and
    /// [`KEY_PASSWORD_ENV`].
    Release { keystore: PathBuf, alias: String },
    /// Nothing.
    Unsigned,
}

/// What to pack.
#[derive(Debug, Clone)]
pub struct Packing<'a> {
    /// The app's library for each ABI.
    pub libraries: Vec<(Abi, PathBuf)>,
    /// Where the package and what it is made of are kept; the compiled
    /// shell and resources are reused while their inputs stay the same.
    pub dir: &'a Path,
    /// The executable's name, which names the library and the package.
    pub bin: &'a str,
    pub shell: &'a Shell,
    pub manifest: Manifest,
    pub flavor: Flavor,
    /// The launcher icon at each of [`ICON_DENSITIES`], as PNGs.
    pub icons: Vec<Vec<u8>>,
    /// What [`Sdk::package`] and [`Sdk::bundle`] sign with; signed apart
    /// with [`Sdk::sign_apk`] and [`sign_bundle`] when `Unsigned`.
    pub key: Key,
}

impl<'a> Packing<'a> {
    /// A dev or `viso build` APK of `library`, for `abi`, asking for
    /// `access` as well.
    pub fn dev(
        library: &Path,
        abi: Abi,
        dir: &'a Path,
        bin: &'a str,
        shell: &'a Shell,
        flavor: Flavor,
        access: &[Access],
    ) -> Packing<'a> {
        let mut manifest = Manifest::dev(bin, flavor);
        for access in access {
            if !manifest.access.contains(access) {
                manifest.access.push(*access);
            }
        }
        Packing {
            libraries: vec![(abi, library.to_owned())],
            dir,
            bin,
            shell,
            manifest,
            flavor,
            icons: Vec::new(),
            key: Key::Debug,
        }
    }
}

/// A packaged app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Apk {
    pub path: PathBuf,
    /// Its application id.
    pub package: String,
}

impl Apk {
    /// The component `am start` launches.
    pub fn component(&self) -> String {
        format!("{}/{ACTIVITY}", self.package)
    }
}

/// The application id of the app named `bin`.
pub fn package_name(bin: &str) -> String {
    format!("dev.viso.{}", library_name(bin))
}

/// The name the library is loaded by: `lib<name>.so`.
pub fn library_name(bin: &str) -> String {
    bin.replace('-', "_")
}

impl Sdk {
    /// The SDK at `$ANDROID_HOME` or `$ANDROID_SDK_ROOT`, else `viso`, the
    /// one Viso installed, else the default place; with the NDK at
    /// `$ANDROID_NDK_HOME` or its newest under the SDK.
    pub fn locate(viso: Option<&Path>) -> Result<Sdk, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let (root, origin) = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
            .into_iter()
            .find_map(|name| {
                let value = std::env::var_os(name).filter(|v| !v.is_empty())?;
                Some((PathBuf::from(value), SdkOrigin::Variable(name)))
            })
            .or_else(|| Some((viso.filter(|v| v.is_dir())?.to_owned(), SdkOrigin::Viso)))
            .unwrap_or_else(|| (default_sdk(&home), SdkOrigin::Default));
        if !root.is_dir() {
            return Err(match origin {
                SdkOrigin::Variable(name) => {
                    format!(
                        "`{name}` names {}, which is not a directory",
                        root.display()
                    )
                }
                _ => format!("no Android SDK at {}", root.display()),
            });
        }
        let ndk = std::env::var_os("ANDROID_NDK_HOME")
            .map(PathBuf::from)
            .or_else(|| newest(&root.join("ndk"), |_| true))
            .ok_or("no NDK under $ANDROID_HOME/ndk (set ANDROID_NDK_HOME)")?;
        let prebuilt = newest(&ndk.join("toolchains/llvm/prebuilt"), |_| true)
            .ok_or("the NDK has no LLVM toolchain")?;
        let build_tools =
            newest(&root.join("build-tools"), |_| true).ok_or("no SDK build-tools installed")?;
        let platform = newest(&root.join("platforms"), |n| {
            n.strip_prefix("android-")
                .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        })
        .ok_or("no SDK platform installed")?;
        Ok(Sdk {
            ndk_bin: prebuilt.join("bin"),
            build_tools,
            platform_jar: platform.join("android.jar"),
            adb: root.join("platform-tools/adb"),
            emulator: root.join("emulator/emulator"),
            root,
            origin,
        })
    }

    /// The environment Cargo builds the app for `abi` with: the NDK's
    /// linker, C compiler and archiver.
    pub fn cargo_env(&self, abi: Abi) -> Vec<(String, PathBuf)> {
        let clang = self
            .ndk_bin
            .join(format!("{}{MIN_API}-clang", abi.clang_target()));
        let target = abi.triple().replace('-', "_");
        vec![
            (
                format!("CARGO_TARGET_{}_LINKER", target.to_uppercase()),
                clang.clone(),
            ),
            (format!("CC_{target}"), clang),
            (format!("AR_{target}"), self.ndk_bin.join("llvm-ar")),
        ]
    }

    /// Packs `packing` into the signed APK `<dir>/<bin>.apk`.
    pub fn package(&self, packing: &Packing<'_>) -> Result<Apk, String> {
        let dir = packing.dir;
        io(dir, std::fs::create_dir_all(dir))?;
        let dex = self.dex(dir, packing.shell, packing.flavor)?;
        let base = self.base(packing, false)?;

        // The resources and manifest as `aapt2` linked them, `resources.arsc`
        // still stored; the libraries stored, so they map straight from the
        // APK; the dex.
        let mut entries = archive::unzip(&io(&base, std::fs::read(&base))?)
            .map_err(|why| format!("{}: {why}", base.display()))?;
        entries.push(Entry::file("classes.dex", io(&dex, std::fs::read(&dex))?));
        entries.extend(libraries(packing, "lib/")?);
        let unaligned = dir.join("unaligned.apk");
        let bytes = archive::zip_bytes(&entries).map_err(|e| e.to_string())?;
        io(&unaligned, std::fs::write(&unaligned, bytes))?;
        let apk = dir.join(format!("{}.apk", packing.bin));
        run(Command::new(self.build_tools.join("zipalign"))
            .args(["-P", "16", "-f", "4"])
            .arg(&unaligned)
            .arg(&apk))?;
        self.sign_apk(&apk, &packing.key)?;
        Ok(Apk {
            path: apk,
            package: packing.manifest.package.clone(),
        })
    }

    /// Lays `packing` out as the app bundle `<dir>/<bin>.aab`: its one
    /// module, `base`, with the manifest and resources in protocol buffer
    /// form, the dex and each ABI's library; signed with `jarsigner`.
    pub fn bundle(&self, packing: &Packing<'_>) -> Result<PathBuf, String> {
        let dir = packing.dir;
        io(dir, std::fs::create_dir_all(dir))?;
        let dex = self.dex(dir, packing.shell, packing.flavor)?;
        let linked = self.base(packing, true)?;
        let mut entries = vec![Entry::file("BundleConfig.pb", bundle_config())];
        let linked_entries = archive::unzip(&io(&linked, std::fs::read(&linked))?)
            .map_err(|why| format!("{}: {why}", linked.display()))?;
        for mut entry in linked_entries {
            if entry.kind == Kind::Directory {
                continue;
            }
            entry.name = match entry.name.as_str() {
                "AndroidManifest.xml" => "base/manifest/AndroidManifest.xml".to_owned(),
                name if name == "resources.pb"
                    || name.starts_with("res/")
                    || name.starts_with("assets/") =>
                {
                    format!("base/{name}")
                }
                name => format!("base/root/{name}"),
            };
            entries.push(entry);
        }
        entries.push(Entry::file(
            "base/dex/classes.dex",
            io(&dex, std::fs::read(&dex))?,
        ));
        entries.extend(libraries(packing, "base/lib/")?);
        let aab = dir.join(format!("{}.aab", packing.bin));
        archive::zip(&mut entries, &aab)?;
        sign_bundle(&aab, &packing.key)?;
        Ok(aab)
    }

    /// Signs the aligned APK `apk` with `key`, in place.
    pub fn sign_apk(&self, apk: &Path, key: &Key) -> Result<(), String> {
        let mut apksigner = Command::new(self.build_tools.join("apksigner"));
        apksigner.arg("sign");
        match key {
            Key::Debug => {
                apksigner.arg("--ks").arg(debug_keystore()?).args([
                    "--ks-pass",
                    "pass:android",
                    "--ks-key-alias",
                    "androiddebugkey",
                ]);
            }
            Key::Release { keystore, alias } => {
                let (store, key) = password_vars();
                apksigner
                    .arg("--ks")
                    .arg(keystore)
                    .args(["--ks-key-alias", alias])
                    .args(["--ks-pass", &format!("env:{store}")])
                    .args(["--key-pass", &format!("env:{key}")]);
            }
            Key::Unsigned => return Ok(()),
        }
        run(apksigner.arg(apk))
    }

    /// The shell compiled to `classes.dex`, again only when a source, the
    /// platform or the flavor changed.
    fn dex(&self, dir: &Path, shell: &Shell, flavor: Flavor) -> Result<PathBuf, String> {
        let mut sources = Vec::new();
        for java in &shell.java {
            files_under(java, "java", &mut sources)?;
        }
        let out = dir.join("dex");
        let dex = out.join("classes.dex");
        let mut inputs = sources.clone();
        inputs.push(self.platform_jar.clone());
        let stamp = stamp(&inputs, &format!("{flavor:?}"));
        let stamp_file = out.join("inputs");
        if dex.is_file() && std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
            return Ok(dex);
        }
        let classes = dir.join("classes");
        let _ = std::fs::remove_dir_all(&classes);
        let _ = std::fs::remove_dir_all(&out);
        for d in [&classes, &out] {
            io(d, std::fs::create_dir_all(d))?;
        }
        run(Command::new("javac")
            .args([
                "-source",
                "8",
                "-target",
                "8",
                "-Xlint:-options",
                "-encoding",
                "UTF-8",
            ])
            .arg("-classpath")
            .arg(&self.platform_jar)
            .arg("-d")
            .arg(&classes)
            .args(&sources))?;
        let mut class_files = Vec::new();
        files_under(&classes, "class", &mut class_files)?;
        run(Command::new(self.build_tools.join("d8"))
            .args(["--min-api", &MIN_API.to_string()])
            .args((flavor == Flavor::Release).then_some("--release"))
            .arg("--lib")
            .arg(&self.platform_jar)
            .arg("--output")
            .arg(&out)
            .args(&class_files))?;
        io(&stamp_file, std::fs::write(&stamp_file, stamp))?;
        Ok(dex)
    }

    /// The manifest and the launcher icons linked by `aapt2`: as an APK, or
    /// in the protocol buffer form a bundle holds when `proto`; again only
    /// when one of them changed.
    fn base(&self, packing: &Packing<'_>, proto: bool) -> Result<PathBuf, String> {
        let dir = packing.dir;
        let template = io(
            &packing.shell.manifest,
            std::fs::read_to_string(&packing.shell.manifest),
        )?;
        let manifest = fill_manifest(&template, &packing.manifest, packing.bin);
        let mut hasher = DefaultHasher::new();
        (&manifest, &packing.icons, proto).hash(&mut hasher);
        let stamp = format!("{:016x}", hasher.finish());
        let name = if proto { "base-proto" } else { "base" };
        let base = dir.join(format!("{name}.apk"));
        let stamp_file = dir.join(format!("{name}.inputs"));
        if base.is_file() && std::fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
            return Ok(base);
        }
        let manifest_path = dir.join("AndroidManifest.xml");
        io(&manifest_path, std::fs::write(&manifest_path, &manifest))?;
        let _ = std::fs::remove_file(&base);

        let compiled = dir.join("compiled");
        let _ = std::fs::remove_dir_all(&compiled);
        let mut flats = Vec::new();
        if !packing.icons.is_empty() {
            let res = dir.join("res");
            let _ = std::fs::remove_dir_all(&res);
            io(&compiled, std::fs::create_dir_all(&compiled))?;
            for ((density, _), png) in ICON_DENSITIES.iter().zip(&packing.icons) {
                let mipmap = res.join(format!("mipmap-{density}"));
                io(&mipmap, std::fs::create_dir_all(&mipmap))?;
                let file = mipmap.join("ic_launcher.png");
                io(&file, std::fs::write(&file, png))?;
                run(Command::new(self.build_tools.join("aapt2"))
                    .arg("compile")
                    .arg("-o")
                    .arg(&compiled)
                    .arg(&file))?;
                flats.push(compiled.join(format!("mipmap-{density}_ic_launcher.png.flat")));
            }
        }
        run(Command::new(self.build_tools.join("aapt2"))
            .arg("link")
            .args(proto.then_some("--proto-format"))
            .arg("-I")
            .arg(&self.platform_jar)
            .arg("--manifest")
            .arg(&manifest_path)
            .args(&flats)
            .arg("-o")
            .arg(&base))?;
        io(&stamp_file, std::fs::write(&stamp_file, stamp))?;
        Ok(base)
    }
}

/// Signs the app bundle `aab` with `key`, in place, as a JAR is signed.
pub fn sign_bundle(aab: &Path, key: &Key) -> Result<(), String> {
    let mut jarsigner = Command::new(jdk_tool("jarsigner"));
    jarsigner.args([
        "-sigalg",
        "SHA256withRSA",
        "-digestalg",
        "SHA-256",
        "-keystore",
    ]);
    let alias = match key {
        Key::Debug => {
            jarsigner
                .arg(debug_keystore()?)
                .args(["-storepass", "android"]);
            "androiddebugkey"
        }
        Key::Release { keystore, alias } => {
            let (store, key) = password_vars();
            jarsigner
                .arg(keystore)
                .args(["-storepass:env", store, "-keypass:env", key]);
            alias.as_str()
        }
        Key::Unsigned => return Ok(()),
    };
    run(jarsigner.arg(aab).arg(alias))
}

/// Each ABI's library of `packing` under `prefix`, stored.
fn libraries(packing: &Packing<'_>, prefix: &str) -> Result<Vec<Entry>, String> {
    let lib = library_name(packing.bin);
    packing
        .libraries
        .iter()
        .map(|(abi, path)| {
            Ok(Entry {
                name: format!("{prefix}{}/lib{lib}.so", abi.as_str()),
                kind: Kind::File { executable: false },
                data: io(path, std::fs::read(path))?,
                stored: true,
            })
        })
        .collect()
}

/// The variables a release key's passwords are in: the key's own when it
/// is set, else the keystore's for both.
fn password_vars() -> (&'static str, &'static str) {
    let key = if std::env::var_os(KEY_PASSWORD_ENV).is_some() {
        KEY_PASSWORD_ENV
    } else {
        KEYSTORE_PASSWORD_ENV
    };
    (KEYSTORE_PASSWORD_ENV, key)
}

/// A JDK tool: under `$JAVA_HOME`, else on `PATH`.
fn jdk_tool(name: &str) -> PathBuf {
    std::env::var_os("JAVA_HOME")
        .map(|home| PathBuf::from(home).join("bin").join(name))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// `BundleConfig.pb`: the `bundletool` version the bundle is laid out for,
/// as a protocol buffer — field 1, `Bundletool`, holding field 2, its
/// version.
fn bundle_config() -> Vec<u8> {
    let version = BUNDLETOOL_VERSION.as_bytes();
    let mut inner = vec![0x12, version.len() as u8];
    inner.extend_from_slice(version);
    let mut config = vec![0x0a, inner.len() as u8];
    config.extend_from_slice(&inner);
    config
}

/// The SDK's default place under `home`.
fn default_sdk(home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        home.join("Library/Android/sdk")
    } else if cfg!(windows) {
        home.join("AppData/Local/Android/Sdk")
    } else {
        home.join("Android/Sdk")
    }
}

/// The manifest `template` filled in for the app `bin`.
fn fill_manifest(template: &str, manifest: &Manifest, bin: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    if manifest.internet {
        lines.push(r#"<uses-permission android:name="android.permission.INTERNET" />"#);
    }
    for access in &manifest.access {
        lines.extend(permissions(*access));
    }
    let permissions = lines.join("\n    ");
    let icon = if manifest.icon {
        r#"android:icon="@mipmap/ic_launcher""#
    } else {
        ""
    };
    let orientation = match manifest.orientations {
        Orientations::Any => "",
        Orientations::Portrait => r#"android:screenOrientation="sensorPortrait""#,
        Orientations::Landscape => r#"android:screenOrientation="sensorLandscape""#,
    };
    template
        .replace("{package}", &manifest.package)
        .replace("{version_code}", &manifest.version_code.to_string())
        .replace("{version_name}", &xml(&manifest.version_name))
        .replace("{min_sdk}", &manifest.min_sdk.to_string())
        .replace("{permissions}", &permissions)
        .replace("{label}", &xml(&manifest.label))
        .replace("{icon}", icon)
        .replace("{orientation}", orientation)
        .replace("{lib}", &library_name(bin))
}

/// `text` as an XML attribute value.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        // A leading `@` or `?` would be read as a reference.
        .replacen('@', "\\@", usize::from(text.starts_with('@')))
        .replacen('?', "\\?", usize::from(text.starts_with('?')))
}

/// What tells two sets of inputs apart: each path, length and modification
/// time, and `extra`.
fn stamp(inputs: &[PathBuf], extra: &str) -> String {
    let mut stamp = String::from(extra);
    for path in inputs {
        let meta = std::fs::metadata(path).ok();
        let len = meta.as_ref().map_or(0, std::fs::Metadata::len);
        let modified = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos());
        stamp.push_str(&format!("\n{} {len} {modified}", path.display()));
    }
    stamp
}

/// The standard Android debug keystore, created if missing.
pub fn debug_keystore() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let dir = home.join(".android");
    let keystore = dir.join("debug.keystore");
    if keystore.exists() {
        return Ok(keystore);
    }
    io(&dir, std::fs::create_dir_all(&dir))?;
    run(Command::new("keytool")
        .args(["-genkeypair", "-keystore"])
        .arg(&keystore)
        .args([
            "-storepass",
            "android",
            "-keypass",
            "android",
            "-alias",
            "androiddebugkey",
            "-keyalg",
            "RSA",
            "-keysize",
            "2048",
            "-validity",
            "10000",
            "-dname",
            "CN=Android Debug,O=Android,C=US",
        ]))?;
    Ok(keystore)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_binary() {
        assert_eq!(package_name("my-app"), "dev.viso.my_app");
        assert_eq!(library_name("my-app"), "my_app");
        let apk = Apk {
            path: PathBuf::from("a.apk"),
            package: package_name("demo"),
        };
        assert_eq!(apk.component(), "dev.viso.demo/dev.viso.VisoActivity");
    }

    #[test]
    fn a_dev_apk_asks_for_what_the_app_declares_once() {
        let shell = Shell {
            java: Vec::new(),
            manifest: PathBuf::new(),
        };
        let dir = Path::new("/out");
        let packing = Packing::dev(
            Path::new("/lib.so"),
            Abi::Arm64,
            dir,
            "a",
            &shell,
            Flavor::Dev,
            &[Access::Camera, Access::Notifications],
        );
        assert_eq!(
            packing.manifest.access,
            [Access::Notifications, Access::Camera]
        );
    }

    #[test]
    fn the_manifest_is_filled_in_for_the_app() {
        let template = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../crates/platform/android/AndroidManifest.xml"
        ))
        .unwrap();
        let dev = fill_manifest(&template, &Manifest::dev("a-b", Flavor::Dev), "a-b");
        assert!(dev.contains("package=\"dev.viso.a_b\""));
        assert!(dev.contains("android:label=\"a-b\""));
        assert!(dev.contains("android:value=\"a_b\""));
        assert!(dev.contains("android.permission.INTERNET"));
        assert!(dev.contains("android.permission.POST_NOTIFICATIONS"));
        assert!(!dev.contains("android:icon"));
        assert!(!dev.contains('{'), "{dev}");
        let release = fill_manifest(&template, &Manifest::dev("b", Flavor::Release), "b");
        assert!(!release.contains("INTERNET"));

        let notes = Manifest {
            package: "com.example.notes".into(),
            label: "Notes & \"More\"".into(),
            version_code: 42,
            version_name: "2.1.0".into(),
            min_sdk: 28,
            access: vec![Access::Camera, Access::Location],
            internet: false,
            orientations: Orientations::Portrait,
            icon: true,
        };
        let filled = fill_manifest(&template, &notes, "notes");
        for expected in [
            "package=\"com.example.notes\"",
            "android:versionCode=\"42\"",
            "android:versionName=\"2.1.0\"",
            "android:minSdkVersion=\"28\"",
            "android:label=\"Notes &amp; &quot;More&quot;\"",
            "android:icon=\"@mipmap/ic_launcher\"",
            "android:screenOrientation=\"sensorPortrait\"",
            "android.permission.CAMERA",
            "android.permission.ACCESS_FINE_LOCATION",
        ] {
            assert!(filled.contains(expected), "{expected} in {filled}");
        }
        assert!(!filled.contains("POST_NOTIFICATIONS"));
    }

    #[test]
    fn the_bundle_config_names_its_bundletool() {
        let config = bundle_config();
        assert_eq!(config[0], 0x0a);
        assert_eq!(usize::from(config[1]), config.len() - 2);
        assert_eq!(&config[4..], BUNDLETOOL_VERSION.as_bytes());
        assert_eq!(Abi::parse("armeabi-v7a"), Some(Abi::Armv7));
        assert_eq!(Abi::parse("mips"), None);
    }

    #[test]
    fn the_cargo_environment_names_the_ndk_tools() {
        let sdk = Sdk {
            origin: SdkOrigin::Default,
            root: PathBuf::from("/sdk"),
            ndk_bin: PathBuf::from("/ndk/bin"),
            build_tools: PathBuf::from("/sdk/build-tools/36.0.0"),
            platform_jar: PathBuf::from("/sdk/platforms/android-36/android.jar"),
            adb: PathBuf::from("/sdk/platform-tools/adb"),
            emulator: PathBuf::from("/sdk/emulator/emulator"),
        };
        let env = sdk.cargo_env(Abi::Arm64);
        assert_eq!(
            env[0],
            (
                "CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER".to_string(),
                PathBuf::from("/ndk/bin/aarch64-linux-android26-clang")
            )
        );
        assert_eq!(env[2].0, "AR_aarch64_linux_android");
        assert_eq!(
            sdk.cargo_env(Abi::Armv7)[1].1,
            PathBuf::from("/ndk/bin/armv7a-linux-androideabi26-clang")
        );
        assert_eq!(
            sdk.cargo_env(Abi::X86_64)[1],
            (
                "CC_x86_64_linux_android".to_string(),
                PathBuf::from("/ndk/bin/x86_64-linux-android26-clang")
            )
        );
    }
}
