//! `viso android list|use|doctor|adb` without a network: what a fresh
//! machine lists, the levels and options `use` refuses before it downloads
//! anything, and `adb`'s own refusals. The real download is a unit test of
//! the toolchain, ignored by default.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn home(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("android-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn viso(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(home)
        .env("VISO_HOME", home)
        .env("HOME", home)
        .env_remove("ANDROID_HOME")
        .env_remove("ANDROID_SDK_ROOT")
        .env_remove("CI")
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[test]
fn a_fresh_machine_lists_every_level_available_and_none_default() {
    let home = home("list");
    let out = viso(&home, &["android", "list"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    let shown = text(&out.stdout);
    assert!(shown.starts_with("API   ANDROID   STATUS"), "{shown}");
    for level in ["36", "35", "34"] {
        assert!(
            shown
                .lines()
                .any(|l| l.starts_with(level) && l.contains("available")),
            "{shown}"
        );
    }
    let out = viso(&home, &["android", "list", "--json"]);
    let json = text(&out.stdout);
    assert!(json.contains(r#""default":null"#), "{json}");
    assert!(json.contains(r#""path":"ndk;28.2.13676358""#), "{json}");
    assert!(json.contains(r#""installed":false"#), "{json}");

    let out = viso(&home, &["android", "list", "--offline"]);
    assert_eq!(out.status.code(), Some(0));
    assert!(text(&out.stdout).contains("no Android API level is installed"));
}

#[test]
fn use_refuses_an_unsupported_level_offline_and_without_consent() {
    let home = home("use");
    let out = viso(&home, &["android", "use", "21"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("ANDROID_API_UNSUPPORTED"), "{err}");
    assert!(err.contains("36, 35, 34"), "{err}");

    let out = viso(&home, &["android", "use", "36", "--offline"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(
        err.contains("ENV_OFFLINE") && err.contains("platform-tools"),
        "{err}"
    );

    // No terminal and no `--accept-licenses`: nothing is downloaded.
    let out = viso(&home, &["android", "use", "36", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stdout));
    let json = text(&out.stdout);
    assert!(json.contains(r#""code":"ENV_ANDROID_LICENSE""#), "{json}");
    assert!(!home.join("android/downloads").exists());
    assert!(!home.join("android/config.toml").exists());
}

#[test]
fn adb_has_no_event_form_and_needs_an_sdk() {
    let home = home("adb");
    // After the first argument for adb, everything is adb's: `--json` goes
    // before it.
    let out = viso(&home, &["android", "adb", "--json", "devices"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(text(&out.stdout).contains(r#""code":"CLI_USAGE""#));

    let out = viso(&home, &["android", "adb", "devices"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("ENV_ANDROID_SDK"), "{err}");
    assert!(err.contains("viso android use 36"), "{err}");
}

#[cfg(unix)]
#[test]
fn adb_runs_the_sdks_adb_with_its_arguments_on_a_profiles_emulator() {
    use std::os::unix::fs::PermissionsExt;
    let home = home("adb-run");
    let sdk = home.join("sdk");
    for dir in [
        "platform-tools",
        "ndk/1/toolchains/llvm/prebuilt/host/bin",
        "build-tools/36.0.0",
        "platforms/android-36",
    ] {
        fs::create_dir_all(sdk.join(dir)).unwrap();
    }
    let adb = sdk.join("platform-tools/adb");
    fs::write(
        &adb,
        "#!/bin/sh\nif [ \"$1\" = devices ]; then printf 'List of devices attached\\nemulator-5554\\tdevice\\n'; exit 0; fi\n\
         if [ \"$1 $2\" = \"-s emulator-5554\" ] && [ \"$3\" = emu ]; then echo viso_pixel; echo OK; exit 0; fi\n\
         echo \"adb $*\"; exit 4\n",
    )
    .unwrap();
    fs::set_permissions(&adb, fs::Permissions::from_mode(0o755)).unwrap();
    fs::create_dir_all(home.join("android/emulators")).unwrap();
    fs::write(
        home.join("android/emulators/pixel.toml"),
        "preset = \"phone\"\nsystem = \"36\"\ndevice = \"viso_pixel\"\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(args)
            .env("VISO_HOME", &home)
            .env("ANDROID_HOME", &sdk)
            .output()
            .unwrap()
    };
    let out = run(&["android", "adb", "--", "shell", "echo", "--flag"]);
    assert_eq!(out.status.code(), Some(4), "the exit is adb's");
    assert_eq!(text(&out.stdout), "adb shell echo --flag\n");
    let out = run(&["android", "adb", "--device", "pixel", "logcat", "-d"]);
    assert_eq!(text(&out.stdout), "adb -s emulator-5554 logcat -d\n");
    let out = run(&["android", "adb", "--device", "nope", "devices"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out.stderr));
}
