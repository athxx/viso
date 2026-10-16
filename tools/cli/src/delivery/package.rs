//! The built executables wrapped as each platform ships them, into a
//! package build's `out/`:
//!
//! - macOS: the `.app` with its icon, signed, and the `.dmg` that holds it;
//! - Windows: the executable, signed, in a zip;
//! - Linux: the executable, its desktop entry and icon in a gzipped tar;
//! - iOS: the device `.app` with its asset catalog, signed, as an `.ipa`;
//! - Android: the APK and the app bundle, each ABI's library, signed;
//! - the web: the bundle under `site/`, and a zip of it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use viso_package::android::{self, Abi, ICON_DENSITIES, Key, Packing, Sdk, Shell};
use viso_package::archive::{self, Entry, Kind};
use viso_package::{apple, linux, windows};
use viso_project::{Profile, WebSettings};

use super::{Plan, Platform, Signer, icon};
use crate::build::cargo::{BuildFailure, Sink};

/// What a package is made from.
pub(crate) struct Built<'a> {
    /// The executable, for the plan's first ABI on Android.
    pub(crate) exe: &'a Path,
    /// The library of each of the plan's other ABIs.
    pub(crate) more: &'a [(Abi, PathBuf)],
    /// The executable's name.
    pub(crate) bin: &'a str,
    /// The build's own directory, for what the package is made of.
    pub(crate) dir: &'a Path,
    pub(crate) profile: Profile,
    pub(crate) web: &'a WebSettings,
    pub(crate) android: Option<&'a (Sdk, Shell)>,
}

/// Makes `plan`'s package of `built` in `out`, returning each artifact's
/// kind and path.
pub(crate) fn make(
    plan: &Plan,
    built: &Built<'_>,
    out: &Path,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Vec<(&'static str, PathBuf)>, BuildFailure> {
    let path = |kind: &str| {
        let name = plan
            .outputs
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, name)| name.as_str())
            .expect("the plan names each output");
        out.join(name)
    };
    let package = BuildFailure::Package;
    let sign = BuildFailure::Sign;
    let metadata = &plan.metadata;
    let scratch = built.dir.join("package");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|e| BuildFailure::Write(format!("{}: {e}", scratch.display())))?;
    let png = |size: u32| {
        metadata
            .icon
            .as_ref()
            .map(|icon| icon::scaled(&icon.image, size).to_png())
    };

    match plan.platform {
        Platform::MacOs => {
            let icns = metadata.icon.as_ref().map(|_| {
                let images: Vec<(u32, Vec<u8>)> = apple::ICNS_SIZES
                    .iter()
                    .filter_map(|&size| Some((size, png(size)?)))
                    .collect();
                apple::icns(&images)
            });
            let info = metadata.apple(built.bin);
            let app = apple::macos_bundle(
                built.exe,
                out,
                &metadata.bundle_name(),
                &info,
                icns.as_deref(),
            )
            .map_err(package)?;
            let access: Vec<_> = info.usage.iter().map(|(a, _)| *a).collect();
            let identity = match &plan.signing.signer {
                Signer::Apple { identity } => Some(identity.as_str()),
                _ => None,
            };
            if plan.signing.signer != Signer::None {
                apple::sign_macos(&app.path, identity, &access, &scratch).map_err(sign)?;
            }
            let dmg = path("dmg");
            apple::dmg(&app.path, &dmg, &metadata.display_name, &scratch).map_err(package)?;
            if identity.is_some() {
                apple::sign_macos(&dmg, identity, &[], &scratch).map_err(sign)?;
            }
            Ok(vec![("app-bundle", app.path), ("dmg", dmg)])
        }
        Platform::Windows => {
            let zip = path("archive");
            let top = archive_root(&zip);
            let exe = scratch.join(built.exe.file_name().unwrap_or_default());
            std::fs::copy(built.exe, &exe).map_err(|e| package(e.to_string()))?;
            if let Signer::Authenticode {
                identity,
                timestamp_url,
                signtool,
            } = &plan.signing.signer
            {
                windows::sign(signtool, &exe, identity, timestamp_url.as_deref()).map_err(sign)?;
            }
            let mut entries = vec![directory(&top), file(&top, &exe, true)?];
            archive::zip(&mut entries, &zip).map_err(package)?;
            Ok(vec![("archive", zip)])
        }
        Platform::Linux => {
            let tar = path("archive");
            let top = archive_root(&tar);
            let name = metadata.file_name();
            let icon_name = metadata.icon.as_ref().map(|_| name.as_str());
            let mut entries = vec![
                directory(&top),
                file(&top, built.exe, true)?,
                Entry::file(
                    format!("{top}{name}.desktop"),
                    linux::desktop_entry(&metadata.display_name, built.bin, icon_name).into_bytes(),
                ),
            ];
            if let Some(png) = png(512) {
                entries.push(Entry::file(format!("{top}{name}.png"), png));
            }
            archive::tar_gz(&mut entries, &tar).map_err(package)?;
            Ok(vec![("archive", tar)])
        }
        Platform::Ios => {
            let info = metadata.apple(built.bin);
            let app = apple::bundle(built.exe, &scratch, &info, false).map_err(package)?;
            if let Some(icon) = &metadata.icon {
                let opaque = icon::opaque(&icon::scaled(&icon.image, icon::MIN_SIZE));
                apple::compile_icons(&app.path, &opaque.to_png(), &metadata.minimum_os, &scratch)
                    .map_err(package)?;
            }
            if let Signer::Provisioned { identity, profile } = &plan.signing.signer {
                apple::sign(
                    &app.path,
                    Some(identity),
                    Some(profile),
                    &scratch.join("sign"),
                )
                .map_err(sign)?;
            }
            let ipa = path("ipa");
            apple::ipa(&app.path, &ipa).map_err(package)?;
            Ok(vec![("ipa", ipa)])
        }
        Platform::Android => {
            let (sdk, shell) = built
                .android
                .expect("an Android package has its SDK and shell");
            let mut libraries = vec![(plan.abis[0], built.exe.to_owned())];
            libraries.extend(built.more.iter().cloned());
            let key = match &plan.signing.signer {
                Signer::Keystore { keystore, alias } => Key::Release {
                    keystore: keystore.clone(),
                    alias: alias.clone(),
                },
                Signer::DebugKeystore => Key::Debug,
                _ => Key::Unsigned,
            };
            let dir = built.dir.join("android");
            let packing = Packing {
                libraries,
                dir: &dir,
                bin: built.bin,
                shell,
                manifest: metadata.android(built.bin),
                flavor: android::Flavor::Release,
                icons: ICON_DENSITIES
                    .iter()
                    .filter_map(|&(_, size)| png(size))
                    .collect(),
                key: Key::Unsigned,
            };
            let apk = sdk.package(&packing).map_err(package)?.path;
            sdk.sign_apk(&apk, &key).map_err(sign)?;
            let aab = sdk.bundle(&packing).map_err(package)?;
            android::sign_bundle(&aab, &key).map_err(sign)?;
            let (apk_out, aab_out) = (path("apk"), path("aab"));
            for (from, to) in [(&apk, &apk_out), (&aab, &aab_out)] {
                std::fs::copy(from, to)
                    .map_err(|e| BuildFailure::Write(format!("{}: {e}", to.display())))?;
            }
            Ok(vec![("apk", apk_out), ("aab", aab_out)])
        }
        Platform::Web => {
            let site = path("site");
            crate::build::web::bundle(
                built.exe,
                &site,
                built.bin,
                built.profile,
                built.web,
                cancelled,
                sink,
            )?;
            let zip = path("archive");
            // The precompressed copies are kept as they are.
            let mut entries = archive::entries(&site, "", &|name: &str| {
                name.ends_with(".br") || name.ends_with(".gz")
            })
            .map_err(package)?;
            archive::zip(&mut entries, &zip).map_err(package)?;
            Ok(vec![("site", site), ("archive", zip)])
        }
    }
}

/// The directory an archive unpacks into: its name without the extension,
/// ending in `/`.
fn archive_root(archive: &Path) -> String {
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name
        .strip_suffix(".tar.gz")
        .or_else(|| name.strip_suffix(".zip"))
        .unwrap_or(&name);
    format!("{stem}/")
}

fn directory(name: &str) -> Entry {
    Entry {
        name: name.to_owned(),
        kind: Kind::Directory,
        data: Vec::new(),
        stored: true,
    }
}

/// The file `path` as an entry under `top`.
fn file(top: &str, path: &Path, executable: bool) -> Result<Entry, BuildFailure> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    Ok(Entry {
        name: format!("{top}{name}"),
        kind: Kind::File { executable },
        data: std::fs::read(path)
            .map_err(|e| BuildFailure::Package(format!("{}: {e}", path.display())))?,
        stored: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_archive_unpacks_into_its_own_name() {
        assert_eq!(
            archive_root(Path::new("/d/app-1.0.0-x86_64.tar.gz")),
            "app-1.0.0-x86_64/"
        );
        assert_eq!(archive_root(Path::new("app-1.0.0.zip")), "app-1.0.0/");
    }
}
