//! `cargo xtask bundle`: package an app binary for a target that cannot run
//! a bare executable.
//!
//! ```text
//! cargo xtask bundle --target android -p <package> [--bin <name>] [--release] [--run]
//! ```
//!
//! Android: the binary is linked as a shared library exporting `main` and
//! `JNI_OnLoad`, and packed with the `dev.viso` Java shell into a signed APK
//! (debug keystore) under `<target-dir>/bundle/android/`. `--run` installs it
//! on the device `adb` sees and launches it.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

struct Options {
    target: String,
    package: String,
    bin: Option<String>,
    release: bool,
    run: bool,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        target: String::new(),
        package: String::new(),
        bin: None,
        release: false,
        run: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned().ok_or(format!("{arg} needs a value"));
        match arg.as_str() {
            "--target" => o.target = value()?,
            "-p" | "--package" => o.package = value()?,
            "--bin" => o.bin = Some(value()?),
            "--release" => o.release = true,
            "--run" => o.run = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if o.target.is_empty() || o.package.is_empty() {
        return Err("--target and -p are required".into());
    }
    Ok(o)
}

pub(crate) fn bundle(args: &[String]) -> ExitCode {
    let options = match parse(args) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("bundle: {e}\nusage: {USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let result = match options.target.as_str() {
        "android" => android(&options),
        other => Err(format!("unknown bundle target {other:?} (android)")),
    };
    match result {
        Ok(path) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("bundle: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str =
    "cargo xtask bundle --target android -p <package> [--bin <name>] [--release] [--run]";

/// Run `cmd`, failing with its name if it does not succeed.
fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .status()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed ({status})", cmd.get_program()))
    }
}

/// Run `cmd` and capture its stdout.
fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .output()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        return Err(format!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The entry of `dir` whose name sorts last by its numeric parts (the
/// newest version), among those `keep` accepts.
fn newest(dir: &Path, keep: impl Fn(&str) -> bool) -> Option<PathBuf> {
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

/// The executable path of the `--bin` artifact in cargo's JSON messages.
fn executable(messages: &str) -> Option<PathBuf> {
    messages
        .lines()
        .filter(|l| l.contains("\"reason\":\"compiler-artifact\""))
        .filter_map(|l| {
            let at = l.find("\"executable\":\"")? + "\"executable\":\"".len();
            let end = l[at..].find('"')?;
            Some(PathBuf::from(l[at..at + end].replace("\\\\", "\\")))
        })
        .next_back()
}

struct AndroidSdk {
    ndk_bin: PathBuf,
    build_tools: PathBuf,
    platform_jar: PathBuf,
    adb: PathBuf,
}

fn android_sdk() -> Result<AndroidSdk, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let sdk = ["ANDROID_HOME", "ANDROID_SDK_ROOT"]
        .iter()
        .find_map(std::env::var_os)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join("Library/Android/sdk"));
    if !sdk.is_dir() {
        return Err(format!(
            "no Android SDK at {} (set ANDROID_HOME)",
            sdk.display()
        ));
    }
    let ndk = std::env::var_os("ANDROID_NDK_HOME")
        .map(PathBuf::from)
        .or_else(|| newest(&sdk.join("ndk"), |_| true))
        .ok_or("no NDK under $ANDROID_HOME/ndk (set ANDROID_NDK_HOME)")?;
    let prebuilt = newest(&ndk.join("toolchains/llvm/prebuilt"), |_| true)
        .ok_or("NDK has no LLVM toolchain")?;
    let build_tools =
        newest(&sdk.join("build-tools"), |_| true).ok_or("no SDK build-tools installed")?;
    let platform = newest(&sdk.join("platforms"), |n| {
        n.strip_prefix("android-")
            .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
    })
    .ok_or("no SDK platform installed")?;
    Ok(AndroidSdk {
        ndk_bin: prebuilt.join("bin"),
        build_tools,
        platform_jar: platform.join("android.jar"),
        adb: sdk.join("platform-tools/adb"),
    })
}

/// The lowest API level the bundle supports (the manifest's `minSdkVersion`).
const ANDROID_MIN_API: u32 = 26;

fn android(o: &Options) -> Result<PathBuf, String> {
    let root = crate::workspace_root();
    let sdk = android_sdk()?;
    let triple = "aarch64-linux-android";
    let clang = sdk.ndk_bin.join(format!("{triple}{ANDROID_MIN_API}-clang"));
    let bin = o.bin.clone().unwrap_or_else(|| o.package.clone());
    let lib = bin.replace('-', "_");

    // The app, linked as a shared library the activity loads. `main` and
    // `JNI_OnLoad` stay exported (and kept) for the loader and the VM;
    // 16 KiB segments keep it loadable on 16 KiB-page devices.
    let mut cargo = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(&root)
        .args(["rustc", "--message-format=json-render-diagnostics"])
        .args(["-p", &o.package, "--bin", &bin, "--target", triple])
        .args(o.release.then_some("--release"))
        .args(["--", "-A", "linker_messages"])
        .args([
            "-Clink-arg=-shared",
            "-Clink-arg=-Wl,--export-dynamic-symbol=main",
            "-Clink-arg=-Wl,--export-dynamic-symbol=JNI_OnLoad",
            "-Clink-arg=-Wl,-u,JNI_OnLoad",
            "-Clink-arg=-Wl,-z,max-page-size=16384",
        ])
        .env("CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER", &clang)
        .env("CC_aarch64_linux_android", &clang)
        .env("AR_aarch64_linux_android", sdk.ndk_bin.join("llvm-ar"));
    let built = executable(&output(&mut cargo)?).ok_or("cargo reported no executable")?;

    let target_dir = built
        .ancestors()
        .nth(3)
        .ok_or("unexpected artifact path")?
        .to_path_buf();
    let out = target_dir.join("bundle/android").join(&bin);
    let _ = std::fs::remove_dir_all(&out);
    let (classes, dex, stage) = (out.join("classes"), out.join("dex"), out.join("apk"));
    for dir in [&classes, &dex, &stage.join("lib/arm64-v8a")] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let so = stage.join(format!("lib/arm64-v8a/lib{lib}.so"));
    std::fs::copy(&built, &so).map_err(|e| format!("{}: {e}", so.display()))?;

    // The Java shell.
    let shell = root.join("crates/platform/android");
    let sources: Vec<PathBuf> = std::fs::read_dir(shell.join("java/dev/viso"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "java"))
        .collect();
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
        .arg(&sdk.platform_jar)
        .arg("-d")
        .arg(&classes)
        .args(&sources))?;
    let class_files: Vec<PathBuf> = std::fs::read_dir(classes.join("dev/viso"))
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    run(Command::new(sdk.build_tools.join("d8"))
        .args(["--min-api", &ANDROID_MIN_API.to_string()])
        .args(o.release.then_some("--release"))
        .arg("--lib")
        .arg(&sdk.platform_jar)
        .arg("--output")
        .arg(&dex)
        .args(&class_files))?;
    std::fs::copy(dex.join("classes.dex"), stage.join("classes.dex")).map_err(|e| e.to_string())?;

    // Manifest, resource table, then the payload.
    let package = format!("dev.viso.{lib}");
    let manifest = std::fs::read_to_string(shell.join("AndroidManifest.xml"))
        .map_err(|e| e.to_string())?
        .replace("{package}", &package)
        .replace("{label}", &bin)
        .replace("{lib}", &lib);
    let manifest_path = out.join("AndroidManifest.xml");
    std::fs::write(&manifest_path, manifest).map_err(|e| e.to_string())?;
    let unaligned = out.join("unaligned.apk");
    run(Command::new(sdk.build_tools.join("aapt2"))
        .arg("link")
        .arg("-I")
        .arg(&sdk.platform_jar)
        .arg("--manifest")
        .arg(&manifest_path)
        .arg("-o")
        .arg(&unaligned))?;
    // The native library is stored, so it can be mapped from the APK.
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
    let apk = out.join(format!("{bin}.apk"));
    run(Command::new(sdk.build_tools.join("zipalign"))
        .args(["-P", "16", "-f", "4"])
        .arg(&unaligned)
        .arg(&apk))?;
    let keystore = debug_keystore()?;
    run(Command::new(sdk.build_tools.join("apksigner"))
        .args(["sign", "--ks"])
        .arg(&keystore)
        .args([
            "--ks-pass",
            "pass:android",
            "--ks-key-alias",
            "androiddebugkey",
        ])
        .arg(&apk))?;

    if o.run {
        run(Command::new(&sdk.adb).args(["install", "-r"]).arg(&apk))?;
        run(Command::new(&sdk.adb).args([
            "shell",
            "am",
            "start",
            "-n",
            &format!("{package}/dev.viso.VisoActivity"),
        ]))?;
    }
    Ok(apk)
}

/// The standard Android debug keystore, created if missing.
fn debug_keystore() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("HOME is not set")?;
    let dir = home.join(".android");
    let keystore = dir.join("debug.keystore");
    if keystore.exists() {
        return Ok(keystore);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
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
    fn parses_bundle_arguments() {
        let args: Vec<String> = ["--target", "android", "-p", "app", "--release"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let o = parse(&args).unwrap();
        assert_eq!((o.target.as_str(), o.package.as_str()), ("android", "app"));
        assert!(o.release && !o.run && o.bin.is_none());
        assert!(parse(&args[..2]).is_err());
        assert!(parse(&["--frobnicate".to_string()]).is_err());
    }

    #[test]
    fn finds_the_last_executable() {
        let messages = concat!(
            "{\"reason\":\"compiler-artifact\",\"executable\":null}\n",
            "{\"reason\":\"compiler-artifact\",\"executable\":\"/t/aarch64-linux-android/debug/app\"}\n",
            "{\"reason\":\"build-finished\",\"success\":true}\n",
        );
        assert_eq!(
            executable(messages),
            Some(PathBuf::from("/t/aarch64-linux-android/debug/app"))
        );
    }

    #[test]
    fn newest_orders_versions_numerically() {
        let dir = std::env::temp_dir().join(format!("xtask-newest-{}", std::process::id()));
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
