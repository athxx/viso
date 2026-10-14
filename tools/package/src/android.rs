//! The Android SDK and NDK, the Cargo build of the app as the shared library
//! the Java shell loads, and the signed APK around it.
//!
//! The app's executable is linked as a shared library that keeps `main` and
//! `JNI_OnLoad` exported for the loader and the VM, its segments aligned to
//! 16 KiB so it loads on 16 KiB-page devices. The APK holds that library, the
//! Java shell compiled to dex, and the manifest filled in for the app; it is
//! aligned and signed with the debug keystore.
//!
//! The shell's Java and its manifest template come from the packages the app
//! links (see [`Shell`]). A dev APK also asks for the network, for its dev
//! channel; a release APK does not.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use crate::{files_under, io, newest, run};

/// The ABI an app is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Abi {
    Arm64,
    X86_64,
}

impl Abi {
    /// The ABI an emulator on this host runs natively.
    pub const fn host() -> Abi {
        if cfg!(target_arch = "x86_64") {
            Abi::X86_64
        } else {
            Abi::Arm64
        }
    }

    /// The Rust target.
    pub const fn triple(self) -> &'static str {
        match self {
            Abi::Arm64 => "aarch64-linux-android",
            Abi::X86_64 => "x86_64-linux-android",
        }
    }

    /// The name of its library directory in an APK and of its system images.
    pub const fn as_str(self) -> &'static str {
        match self {
            Abi::Arm64 => "arm64-v8a",
            Abi::X86_64 => "x86_64",
        }
    }
}

/// The lowest API level an app supports (its `minSdkVersion`).
pub const MIN_API: u32 = 26;

/// The activity the shell starts in.
pub const ACTIVITY: &str = "dev.viso.VisoActivity";

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

/// The permission line a dev APK adds, for its dev channel.
const DEV_PERMISSIONS: &str = r#"<uses-permission android:name="android.permission.INTERNET" />"#;

/// The SDK and NDK tools packaging uses.
#[derive(Debug, Clone)]
pub struct Sdk {
    /// The SDK root.
    pub root: PathBuf,
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
/// `{package}`, `{label}`, `{lib}` and `{dev_permissions}` to fill in.
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
    /// The SDK at `$ANDROID_HOME`, `$ANDROID_SDK_ROOT` or the default place,
    /// with the NDK at `$ANDROID_NDK_HOME` or its newest under the SDK.
    pub fn locate() -> Result<Sdk, String> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let root = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
            .iter()
            .find_map(std::env::var_os)
            .map(PathBuf::from)
            .unwrap_or_else(|| default_sdk(&home));
        if !root.is_dir() {
            return Err(format!(
                "no Android SDK at {} (set ANDROID_HOME)",
                root.display()
            ));
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
        })
    }

    /// The environment Cargo builds the app for `abi` with: the NDK's
    /// linker, C compiler and archiver.
    pub fn cargo_env(&self, abi: Abi) -> Vec<(String, PathBuf)> {
        let triple = abi.triple();
        let clang = self.ndk_bin.join(format!("{triple}{MIN_API}-clang"));
        let target = triple.replace('-', "_");
        vec![
            (
                format!("CARGO_TARGET_{}_LINKER", target.to_uppercase()),
                clang.clone(),
            ),
            (format!("CC_{target}"), clang),
            (format!("AR_{target}"), self.ndk_bin.join("llvm-ar")),
        ]
    }

    /// Packs `library`, the app's executable linked as a shared library for
    /// `abi`, with `shell` into `<dir>/<bin>.apk`. The compiled shell and the linked
    /// manifest are kept in `dir` and reused while their inputs stay the same,
    /// so repacking a rebuilt library is quick.
    pub fn package(
        &self,
        library: &Path,
        abi: Abi,
        dir: &Path,
        bin: &str,
        shell: &Shell,
        flavor: Flavor,
    ) -> Result<Apk, String> {
        let lib = library_name(bin);
        let package = package_name(bin);
        io(dir, std::fs::create_dir_all(dir))?;
        let dex = self.dex(dir, shell, flavor)?;
        let base = self.base(dir, shell, bin, &lib, &package, flavor)?;

        // The payload: the library stored, so it maps straight from the APK,
        // and the dex.
        let stage = dir.join("apk");
        let _ = std::fs::remove_dir_all(&stage);
        let libs = stage.join("lib").join(abi.as_str());
        io(&libs, std::fs::create_dir_all(&libs))?;
        let so = libs.join(format!("lib{lib}.so"));
        io(&so, std::fs::copy(library, &so))?;
        io(&stage, std::fs::copy(&dex, stage.join("classes.dex")))?;
        let unaligned = dir.join("unaligned.apk");
        io(&unaligned, std::fs::copy(&base, &unaligned))?;
        run(Command::new("zip")
            .current_dir(&stage)
            .args(["-q", "-0", "-r"])
            .arg(&unaligned)
            .arg("lib"))?;
        run(Command::new("zip")
            .current_dir(&stage)
            .arg("-q")
            .arg(&unaligned)
            .arg("classes.dex"))?;
        let apk = dir.join(format!("{bin}.apk"));
        run(Command::new(self.build_tools.join("zipalign"))
            .args(["-P", "16", "-f", "4"])
            .arg(&unaligned)
            .arg(&apk))?;
        let keystore = debug_keystore()?;
        run(Command::new(self.build_tools.join("apksigner"))
            .args(["sign", "--ks"])
            .arg(&keystore)
            .args([
                "--ks-pass",
                "pass:android",
                "--ks-key-alias",
                "androiddebugkey",
            ])
            .arg(&apk))?;
        Ok(Apk { path: apk, package })
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

    /// The APK with only the linked manifest, again only when the manifest
    /// changed.
    fn base(
        &self,
        dir: &Path,
        shell: &Shell,
        bin: &str,
        lib: &str,
        package: &str,
        flavor: Flavor,
    ) -> Result<PathBuf, String> {
        let template = io(&shell.manifest, std::fs::read_to_string(&shell.manifest))?;
        let manifest = fill_manifest(&template, package, bin, lib, flavor);
        let manifest_path = dir.join("AndroidManifest.xml");
        let base = dir.join("base.apk");
        if base.is_file() && std::fs::read_to_string(&manifest_path).is_ok_and(|m| m == manifest) {
            return Ok(base);
        }
        io(&manifest_path, std::fs::write(&manifest_path, &manifest))?;
        let _ = std::fs::remove_file(&base);
        run(Command::new(self.build_tools.join("aapt2"))
            .arg("link")
            .arg("-I")
            .arg(&self.platform_jar)
            .arg("--manifest")
            .arg(&manifest_path)
            .arg("-o")
            .arg(&base))?;
        Ok(base)
    }
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

/// The manifest `template` filled in for the app.
fn fill_manifest(template: &str, package: &str, bin: &str, lib: &str, flavor: Flavor) -> String {
    let dev = match flavor {
        Flavor::Dev => DEV_PERMISSIONS,
        Flavor::Release => "",
    };
    template
        .replace("{package}", package)
        .replace("{label}", bin)
        .replace("{lib}", lib)
        .replace("{dev_permissions}", dev)
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
    fn only_a_dev_manifest_asks_for_the_network() {
        let template = "<manifest package=\"{package}\">{dev_permissions}<application android:label=\"{label}\"><meta-data android:value=\"{lib}\"/></application></manifest>";
        let dev = fill_manifest(template, "dev.viso.a_b", "a-b", "a_b", Flavor::Dev);
        assert!(dev.contains("package=\"dev.viso.a_b\""));
        assert!(dev.contains("android:label=\"a-b\""));
        assert!(dev.contains("android:value=\"a_b\""));
        assert!(dev.contains("android.permission.INTERNET"));
        let release = fill_manifest(template, "p", "b", "l", Flavor::Release);
        assert!(!release.contains("INTERNET"));
        assert!(!release.contains('{'));
    }

    #[test]
    fn the_cargo_environment_names_the_ndk_tools() {
        let sdk = Sdk {
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
            sdk.cargo_env(Abi::X86_64)[1],
            (
                "CC_x86_64_linux_android".to_string(),
                PathBuf::from("/ndk/bin/x86_64-linux-android26-clang")
            )
        );
    }
}
