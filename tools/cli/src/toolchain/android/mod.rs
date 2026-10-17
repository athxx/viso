//! The Android development environment Viso installs: the tested component
//! set of an API level ([`components`]) unpacked into `~/.viso/android/sdk`,
//! the level chosen as this machine's default in
//! `~/.viso/android/config.toml`, and the licenses accepted for it.
//!
//! An SDK is found at `$ANDROID_HOME` or `$ANDROID_SDK_ROOT` first, then the
//! one Viso installed once a level is chosen, then the default location; a
//! build, a run and the doctor all ask [`sdk`].
//!
//! A component installs whole: its archive verified, unpacked (only what a
//! build needs, for the NDK and the platform) into a staging directory with a
//! marker naming its path and revision, and renamed into place. A component
//! is installed when its marker names the pinned revision, so switching to a
//! level already installed downloads nothing.

pub(crate) mod components;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, value};
use viso_package::android::Sdk;
use viso_project::DeviceStore;

use self::components::{Component, DEFAULT_API, Host, License};
use super::download::{self, Failure, Transfer};
use super::zip::Archive;
use crate::error::ENV_ANDROID_SDK;
use crate::output::Output;

/// The file in an installed component's directory naming what it is.
const MARKER: &str = ".viso-component";

/// `~/.viso/android`.
#[derive(Debug, Clone)]
pub(crate) struct Home {
    root: PathBuf,
}

impl Home {
    /// This machine's.
    pub(crate) fn current() -> Option<Home> {
        DeviceStore::home().map(|store| Home::at(store.root().join("android")))
    }

    pub(crate) fn at(root: PathBuf) -> Home {
        Home { root }
    }

    /// The SDK Viso installs into.
    pub(crate) fn sdk(&self) -> PathBuf {
        self.root.join("sdk")
    }

    /// Where verified archives wait to be unpacked.
    fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
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

    /// The API level chosen as this machine's default.
    pub(crate) fn default_api(&self) -> Option<u32> {
        let level = self.document().get("api")?.as_integer()?;
        u32::try_from(level).ok()
    }

    /// Chooses `level` as the default, keeping the rest of the file.
    pub(crate) fn set_default_api(&self, level: u32) -> io::Result<()> {
        let mut doc = self.document();
        doc["api"] = value(i64::from(level));
        fs::create_dir_all(&self.root)?;
        crate::atomic::write(&self.config(), doc.to_string().as_bytes())
    }

    fn license(&self, license: &License) -> PathBuf {
        self.root.join("licenses").join(license.id)
    }

    /// Whether `license` was accepted on this machine.
    pub(crate) fn accepted(&self, license: &License) -> bool {
        self.license(license).is_file()
    }

    /// Records that `license`, whose text is `text`, was accepted: the
    /// text's SHA-1, as the Android tools record it.
    pub(crate) fn accept(&self, license: &License, text: &str) -> io::Result<()> {
        let path = self.license(license);
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let digest = sha1_hex(text.as_bytes());
        crate::atomic::write(&path, format!("\n{digest}").as_bytes())
    }

    /// The SDK `viso android use` installed, when a level was chosen.
    fn chosen_sdk(&self) -> Option<PathBuf> {
        self.default_api()?;
        Some(self.sdk())
    }
}

fn sha1_hex(bytes: &[u8]) -> String {
    use sha1::{Digest, Sha1};
    Sha1::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The SDK builds, runs and the doctor use.
pub(crate) fn sdk() -> Result<Sdk, String> {
    let viso = Home::current().and_then(|home| home.chosen_sdk());
    Sdk::locate(viso.as_deref())
}

/// Fails for an SDK that is not there, `why`, pointing at the command that
/// installs one.
pub(crate) fn no_sdk(out: &mut Output, why: &str) -> u8 {
    let level = Home::current()
        .and_then(|home| home.default_api())
        .unwrap_or(DEFAULT_API);
    out.failure_next(
        ENV_ANDROID_SDK,
        why,
        &[],
        &[format!("viso android use {level}")],
    )
}

/// Whether `component` is installed in the SDK at `sdk`.
pub(crate) fn installed(sdk: &Path, component: &Component) -> bool {
    fs::read_to_string(sdk.join(component.dir()).join(MARKER))
        .is_ok_and(|text| text == marker(component))
}

fn marker(component: &Component) -> String {
    format!("path={}\nrevision={}\n", component.path, component.revision)
}

/// Why a component did not install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallError {
    /// Its archive did not arrive verified.
    Download(Failure),
    /// It could not be unpacked or moved into place.
    Unpack(String),
    /// The SDK has no archive of it for this host.
    Host,
    /// An interrupt stopped it unpacking; the verified archive is kept.
    Interrupted,
}

/// Installs `component` into `home`'s SDK for `host`: its archive fetched
/// by `transfer` and verified, unpacked into a staging directory, and moved
/// into place. `progress` hears the bytes downloaded. The verified archive
/// is deleted once installed.
pub(crate) fn install(
    home: &Home,
    component: &Component,
    host: Host,
    transfer: &dyn Transfer,
    progress: &mut dyn FnMut(u64),
) -> Result<(), InstallError> {
    let archive = component.archive(host).ok_or(InstallError::Host)?;
    let url = component.url(archive);
    let pinned = download::Pinned {
        url: &url,
        size: archive.size,
        sha1: archive.sha1,
    };
    let cached = home.downloads().join(archive.file);
    download::fetch(&pinned, &cached, transfer, progress).map_err(InstallError::Download)?;
    let unpack = |why: io::Error| InstallError::Unpack(format!("{}: {why}", archive.file));
    let sdk = home.sdk();
    let staging = sdk.join(".staging").join(format!(
        "{}-{}",
        component.dir().replace('/', "_"),
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging).map_err(unpack)?;
    let unpacked = (|| {
        let mut zip = Archive::open(&cached)?;
        let top = top_dir(&zip)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not one top directory"))?;
        zip.extract(&staging, |name| {
            let rest = name.strip_prefix(&top)?.trim_end_matches('/');
            (!rest.is_empty() && component.keeps(rest)).then(|| rest.to_owned())
        })?;
        fs::write(staging.join(MARKER), marker(component))?;
        let target = sdk.join(component.dir());
        if target.exists() {
            fs::remove_dir_all(&target)?;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&staging, &target)
    })();
    if let Err(error) = unpacked {
        let _ = fs::remove_dir_all(&staging);
        if error.kind() == io::ErrorKind::Interrupted {
            return Err(InstallError::Interrupted);
        }
        return Err(unpack(error));
    }
    let _ = fs::remove_file(&cached);
    Ok(())
}

/// The one directory every entry of `zip` is under, with its `/`.
fn top_dir(zip: &Archive) -> Option<String> {
    let first = zip.entries().first()?.name.split('/').next()?.to_owned() + "/";
    zip.entries()
        .iter()
        .all(|entry| entry.name.starts_with(&first))
        .then_some(first)
}

/// The text of `license` in the manifest `xml`.
pub(crate) fn license_text(xml: &str, license: &License) -> Option<String> {
    let open = format!("<license id=\"{}\"", license.id);
    let start = xml.find(&open)?;
    let body = start + xml[start..].find('>')? + 1;
    let end = body + xml[body..].find("</license>")?;
    Some(
        xml[body..end]
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&"),
    )
}

#[cfg(test)]
mod tests {
    use super::components::{Archive as Pin, Hosts, SDK_LICENSE};
    use super::*;
    use crate::toolchain::download::tests::{Local, sha1_of};
    use crate::toolchain::zip::tests::archive;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-android-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("remote")).unwrap();
        dir
    }

    /// A component of one archive, `bytes`, written to `remote`.
    fn component(remote: &Path, keep: &'static [&'static str], bytes: Vec<u8>) -> Component {
        fs::write(remote.join("ndk.zip"), &bytes).unwrap();
        let sha1: &'static str = Box::leak(sha1_of(&bytes).into_boxed_str());
        let archives: &'static [Pin] = Box::leak(Box::new([Pin {
            hosts: Hosts::Any,
            file: "ndk.zip",
            size: bytes.len() as u64,
            sha1,
        }]));
        Component {
            path: "ndk;1.0",
            revision: "1.0",
            title: "NDK",
            repository: "https://example.invalid/",
            license: SDK_LICENSE,
            keep,
            archives,
        }
    }

    #[test]
    fn a_component_installs_what_it_keeps_and_a_second_use_downloads_nothing() {
        let dir = scratch("install");
        let ndk = component(
            &dir.join("remote"),
            &["source.properties", "toolchains/llvm/prebuilt"],
            archive(&[
                (
                    "android-ndk-r1/source.properties",
                    b"Pkg.Revision = 1.0\n",
                    0o100_644,
                ),
                (
                    "android-ndk-r1/toolchains/llvm/prebuilt/host/bin/clang",
                    b"clang",
                    0o100_755,
                ),
                ("android-ndk-r1/sources/big.c", b"left out", 0o100_644),
            ]),
        );
        let home = Home::at(dir.join("home"));
        let local = Local::new(dir.join("remote"));
        assert!(!installed(&home.sdk(), &ndk));
        install(&home, &ndk, Host::LinuxX64, &local, &mut |_| {}).unwrap();
        let root = home.sdk().join("ndk/1.0");
        assert!(
            root.join("toolchains/llvm/prebuilt/host/bin/clang")
                .is_file()
        );
        assert!(!root.join("sources").exists());
        assert!(installed(&home.sdk(), &ndk));
        assert!(
            !home.downloads().join("ndk.zip").exists(),
            "the archive goes once unpacked"
        );
        assert!(
            fs::read_dir(home.sdk().join(".staging"))
                .unwrap()
                .next()
                .is_none(),
            "nothing is left staged"
        );
        let newer = Component {
            revision: "1.1",
            ..ndk
        };
        assert!(
            !installed(&home.sdk(), &newer),
            "another revision is not installed"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_that_does_not_verify_installs_nothing() {
        let dir = scratch("tampered");
        let ndk = component(
            &dir.join("remote"),
            &[],
            archive(&[("top/a", b"a", 0o100_644)]),
        );
        fs::write(
            dir.join("remote/ndk.zip"),
            archive(&[("top/a", b"b", 0o100_644)]),
        )
        .unwrap();
        let home = Home::at(dir.join("home"));
        let local = Local::new(dir.join("remote"));
        let error = install(&home, &ndk, Host::MacArm, &local, &mut |_| {}).unwrap_err();
        assert!(
            matches!(error, InstallError::Download(Failure::Integrity(_))),
            "{error:?}"
        );
        assert!(!home.sdk().join("ndk/1.0").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    /// The real platform-tools, downloaded and verified against the pinned
    /// table: `cargo test -p viso-cli -- --ignored real_platform_tools`.
    #[test]
    #[ignore = "downloads from the Android repository"]
    fn the_real_platform_tools_install_and_run() {
        let dir = scratch("real");
        let home = Home::at(dir.join("home"));
        let host = Host::current().expect("a supported host");
        let tools = &super::components::SHARED[0];
        install(
            &home,
            tools,
            host,
            &crate::toolchain::download::Curl,
            &mut |_| {},
        )
        .unwrap();
        assert!(installed(&home.sdk(), tools));
        let adb = home.sdk().join("platform-tools/adb");
        let out = std::process::Command::new(&adb)
            .arg("version")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(String::from_utf8_lossy(&out.stdout).contains("Android Debug Bridge"));
        let xml = crate::toolchain::download::text(
            SDK_LICENSE.manifest,
            &crate::toolchain::download::Curl,
        )
        .unwrap();
        assert!(
            license_text(&xml, &SDK_LICENSE)
                .is_some_and(|t| t.contains("Android Software Development Kit"))
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_default_level_and_accepted_licenses_are_machine_state() {
        let dir = scratch("state");
        let home = Home::at(dir.join("home"));
        fs::create_dir_all(dir.join("home")).unwrap();
        fs::write(home.config(), "device = \"pixel\"\n").unwrap();
        assert_eq!(home.default_api(), None);
        assert_eq!(home.chosen_sdk(), None);
        home.set_default_api(35).unwrap();
        assert_eq!(home.default_api(), Some(35));
        let text = fs::read_to_string(home.config()).unwrap();
        assert!(
            text.contains("device = \"pixel\"") && text.contains("api = 35"),
            "{text}"
        );
        assert!(!home.accepted(&SDK_LICENSE));
        home.accept(&SDK_LICENSE, "terms").unwrap();
        assert!(home.accepted(&SDK_LICENSE));
        let xml = r#"<x><license id="android-sdk-license" type="text">Terms &amp; &quot;conditions&quot;</license></x>"#;
        assert_eq!(
            license_text(xml, &SDK_LICENSE).as_deref(),
            Some("Terms & \"conditions\"")
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
