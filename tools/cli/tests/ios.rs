//! `viso ios list|use|doctor` against a scripted Xcode: `xcrun simctl` and
//! `xcodebuild` replaced on `PATH` by fakes that answer from files, so the
//! runtimes listed, the default chosen, the download Xcode is asked for and
//! its failures are all deterministic.

use std::process::Command;

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[cfg(not(target_os = "macos"))]
#[test]
fn every_ios_command_needs_macos() {
    for args in [
        &["ios", "list"][..],
        &["ios", "use", "27"],
        &["ios", "simulator", "list"],
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_viso"))
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert!(text(&out.stderr).contains("ENV_IOS_HOST"));
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::path::{Path, PathBuf};
    use std::process::Output;

    use super::*;

    const RUNTIME: &str = r#"{"platform":"iOS","version":"VERSION","buildversion":"BUILD","identifier":"com.apple.CoreSimulator.SimRuntime.iOS-ID","isAvailable":true,
      "supportedDeviceTypes":[{"name":"iPhone 17","identifier":"com.apple.CoreSimulator.SimDeviceType.iPhone-17","productFamily":"iPhone"},
                              {"name":"iPad Pro 11-inch (M5)","identifier":"com.apple.CoreSimulator.SimDeviceType.iPad-Pro-11-M5","productFamily":"iPad"}]}"#;

    fn runtimes(list: &[(&str, &str)]) -> String {
        let items: Vec<String> = list
            .iter()
            .map(|(version, build)| {
                RUNTIME
                    .replace("VERSION", version)
                    .replace("BUILD", build)
                    .replace("ID", &version.replace('.', "-"))
            })
            .collect();
        format!(r#"{{"runtimes":[{}]}}"#, items.join(","))
    }

    /// A machine with the fake Xcode: its home, and the directory of the fakes'
    /// answers.
    struct Mac {
        home: PathBuf,
        fake: PathBuf,
    }

    impl Mac {
        fn new(label: &str, installed: &[(&str, &str)]) -> Mac {
            use std::os::unix::fs::PermissionsExt;
            let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("ios-{label}"));
            let _ = std::fs::remove_dir_all(&root);
            let (home, fake) = (root.join("home"), root.join("fake"));
            std::fs::create_dir_all(&home).unwrap();
            std::fs::create_dir_all(fake.join("bin")).unwrap();
            std::fs::write(fake.join("runtimes.json"), runtimes(installed)).unwrap();
            std::fs::write(
                fake.join("after.json"),
                runtimes(&[("27.0", "24A434"), ("26.3.1", "23D8133")]),
            )
            .unwrap();
            let scripts = [
                (
                    "xcrun",
                    r#"#!/bin/sh
    F="$(dirname "$0")/.."
    case "$1 $2 $3" in
      "simctl list runtimes") cat "$F/runtimes.json" ;;
      "simctl runtime match") printf '{"iphoneos27.0":{"sdkVersion":"27.0"},"appletvos27.0":{"sdkVersion":"27.0"}}' ;;
      "simctl create "*) echo "$3 $4 $5" > "$F/created"; echo 0000-UDID ;;
      *) echo "unexpected xcrun $*" >&2; exit 9 ;;
    esac
    "#,
                ),
                (
                    "xcodebuild",
                    r#"#!/bin/sh
    F="$(dirname "$0")/.."
    case "$1" in
      -license) [ -f "$F/unlicensed" ] && exit 1; exit 0 ;;
      -version) echo "Xcode 27.0"; echo "Build version 27A266a" ;;
      -downloadPlatform)
        echo "$*" > "$F/download"
        echo "Finding content..."
        if [ -f "$F/unavailable" ]; then echo "iOS $4 is not available for download."; exit 70; fi
        printf 'Downloading iOS 27.0 Simulator (24A434): 10.0%% (0.8 GB of 8.1 GB)\rDownloading iOS 27.0 Simulator (24A434): 100.0%% (8.1 GB of 8.1 GB)\r'
        echo "Downloading iOS 27.0 Simulator (24A434): Done."
        cp "$F/after.json" "$F/runtimes.json"
        echo "iOS 27.0 (24A434) - 0000" ;;
      *) echo "unexpected xcodebuild $*" >&2; exit 9 ;;
    esac
    "#,
                ),
                (
                    "xcode-select",
                    "#!/bin/sh\necho /Applications/Xcode.app/Contents/Developer\n",
                ),
            ];
            for (name, script) in scripts {
                let path = fake.join("bin").join(name);
                std::fs::write(&path, script).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            Mac { home, fake }
        }

        fn viso(&self, args: &[&str]) -> Output {
            let path = format!(
                "{}:{}",
                self.fake.join("bin").display(),
                std::env::var("PATH").unwrap_or_default()
            );
            Command::new(env!("CARGO_BIN_EXE_viso"))
                .args(args)
                .current_dir(&self.home)
                .env("VISO_HOME", &self.home)
                .env("PATH", path)
                .env_remove("CI")
                .output()
                .unwrap()
        }

        fn config(&self) -> String {
            std::fs::read_to_string(self.home.join("ios/config.toml")).unwrap_or_default()
        }
    }

    #[test]
    fn list_shows_the_runtimes_xcodes_own_and_the_one_new_simulators_use() {
        let mac = Mac::new("list", &[("26.3.1", "23D8133"), ("18.6", "22G86")]);
        let out = mac.viso(&["ios", "list"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let shown = text(&out.stdout);
        let lines: Vec<&str> = shown.lines().collect();
        assert!(lines[0].starts_with("RUNTIME       STATUS"), "{shown}");
        assert!(
            lines[1].starts_with("iOS 27.0") && lines[1].contains("available"),
            "{shown}"
        );
        assert!(
            lines[2].starts_with("iOS 26.3.1") && lines[2].ends_with('*'),
            "{shown}"
        );
        assert!(
            lines[3].starts_with("iOS 18.6") && !lines[3].ends_with('*'),
            "{shown}"
        );

        let json = text(&mac.viso(&["ios", "list", "--json"]).stdout);
        assert!(json.contains(r#""xcode":"27.0","default":null"#), "{json}");
        assert!(
            json.contains(r#""phone":"iPhone 17","tablet":"iPad Pro 11-inch (M5)""#),
            "{json}"
        );

        let shown = text(&mac.viso(&["ios", "list", "--offline"]).stdout);
        assert!(!shown.contains("available"), "{shown}");
    }

    #[test]
    fn use_chooses_an_installed_runtime_and_new_simulators_are_made_on_it() {
        let mac = Mac::new("use", &[("26.3.1", "23D8133"), ("18.6", "22G86")]);
        let out = mac.viso(&["ios", "use", "18.6"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        assert!(text(&out.stdout).contains("using iOS 18.6 (22G86)"));
        assert!(
            mac.config().contains("runtime = \"18.6\""),
            "{}",
            mac.config()
        );
        let shown = text(&mac.viso(&["ios", "list"]).stdout);
        assert!(
            shown
                .lines()
                .any(|l| l.starts_with("iOS 18.6") && l.ends_with('*')),
            "{shown}"
        );

        let out = mac.viso(&["ios", "simulator", "create", "iphone"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
        let created = std::fs::read_to_string(mac.fake.join("created")).unwrap();
        assert!(
            created.contains("iPhone-17 com.apple.CoreSimulator.SimRuntime.iOS-18-6"),
            "{created}"
        );

        let out = mac.viso(&["ios", "use", "latest"]);
        assert_eq!(out.status.code(), Some(2));
        assert!(text(&out.stderr).contains("IOS_RUNTIME_INVALID"));
    }

    #[test]
    fn a_missing_runtime_is_downloaded_by_xcode_only_when_asked() {
        let mac = Mac::new("download", &[("26.3.1", "23D8133")]);
        let out = mac.viso(&["ios", "use", "27", "--offline"]);
        assert_eq!(out.status.code(), Some(3));
        assert!(text(&out.stderr).contains("ENV_OFFLINE"));

        // No terminal and no `--download`: Xcode is not asked.
        let out = mac.viso(&["ios", "use", "27", "--json"]);
        assert_eq!(out.status.code(), Some(2), "{}", text(&out.stdout));
        let json = text(&out.stdout);
        assert!(json.contains(r#""code":"IOS_RUNTIME_MISSING""#), "{json}");
        assert!(json.contains("viso ios use 27 --download"), "{json}");
        assert!(!mac.fake.join("download").exists());

        std::fs::write(mac.fake.join("unlicensed"), "").unwrap();
        let out = mac.viso(&["ios", "use", "27", "--download"]);
        assert_eq!(out.status.code(), Some(3));
        assert!(text(&out.stderr).contains("ENV_XCODE_LICENSE"));
        std::fs::remove_file(mac.fake.join("unlicensed")).unwrap();

        let out = mac.viso(&["ios", "use", "27", "--download", "--json"]);
        assert_eq!(out.status.code(), Some(0), "{}", text(&out.stdout));
        let json = text(&out.stdout);
        assert!(
            json.contains("downloading iOS 27.0 Simulator (24A434): 10.0%"),
            "progress is reported: {json}"
        );
        assert!(
            json.contains(r#""version":"27.0","build":"24A434""#),
            "{json}"
        );
        assert!(json.contains(r#""installed":true"#), "{json}");
        let asked = std::fs::read_to_string(mac.fake.join("download")).unwrap();
        assert_eq!(
            asked.trim(),
            "-downloadPlatform iOS",
            "Xcode's own runtime has no build version"
        );
        assert!(mac.config().contains("runtime = \"27\""));

        std::fs::write(mac.fake.join("unavailable"), "").unwrap();
        let out = mac.viso(&["ios", "use", "17.5", "--download"]);
        assert_eq!(out.status.code(), Some(3));
        let err = text(&out.stderr);
        assert!(
            err.contains("ENV_DOWNLOAD") && err.contains("not available for download"),
            "{err}"
        );
        assert!(err.contains("viso ios use 27.0 --download"), "{err}");
        let asked = std::fs::read_to_string(mac.fake.join("download")).unwrap();
        assert_eq!(asked.trim(), "-downloadPlatform iOS -buildVersion 17.5");
    }

    #[test]
    fn the_doctor_holds_the_default_runtime_to_being_installed() {
        let mac = Mac::new("doctor", &[("26.3.1", "23D8133")]);
        let out = mac.viso(&["ios", "doctor", "--json"]);
        let json = text(&out.stdout);
        for name in [
            "xcode",
            "xcode-license",
            "ios-runtime",
            "ios-disk",
            "ios-rust",
            "ios-profiles",
        ] {
            assert!(
                json.contains(&format!(r#""name":"{name}""#)),
                "{name}: {json}"
            );
        }
        assert!(json.contains("iOS 26.3.1 (23D8133), the newest"), "{json}");

        std::fs::create_dir_all(mac.home.join("ios")).unwrap();
        std::fs::write(mac.home.join("ios/config.toml"), "runtime = \"27\"\n").unwrap();
        let out = mac.viso(&["ios", "doctor", "--json"]);
        assert_eq!(out.status.code(), Some(3));
        let json = text(&out.stdout);
        assert!(
            json.contains("the default, iOS 27, is not installed"),
            "{json}"
        );
        assert!(json.contains("viso ios use 27 --download"), "{json}");
    }
}
