//! The built executables wrapped as each platform ships them, into a
//! package build's `out/`:
//!
//! - macOS: the `.app` with its icon, signed, and the `.dmg` that holds it;
//! - Windows: the executable, its icon and version linked in, signed, in a
//!   zip and, with an icon, an MSIX;
//! - Linux: the executable, its desktop entry and icon in a gzipped tar;
//! - iOS: the device `.app` with its asset catalog, signed, as an `.ipa`;
//! - Android: the APK and the app bundle, each ABI's library, signed;
//! - the web: the bundle under `site/`, and a zip of it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use sha2::{Digest, Sha256};

use viso_package::android::{self, Abi, ICON_DENSITIES, Key, Packing, Sdk, Shell};
use viso_package::archive::{self, Entry, Kind};
use viso_package::windows::{msix, resources};
use viso_package::{apple, linux, windows};
use viso_project::{Profile, WebSettings};

use super::fragments::Fragments;
use super::{Plan, Platform, Signer, icon};
use crate::build::cargo::{BuildFailure, CargoEvent, Sink};

/// What a package is made from.
pub struct Built<'a> {
    /// The executable, for the plan's first ABI on Android.
    pub exe: &'a Path,
    /// The library of each of the plan's other ABIs.
    pub more: &'a [(Abi, PathBuf)],
    /// The executable's name.
    pub bin: &'a str,
    /// The build's own directory, for what the package is made of.
    pub dir: &'a Path,
    pub profile: Profile,
    pub web: &'a WebSettings,
    pub android: Option<&'a (Sdk, Shell)>,
    /// What the packages the app links declare for the platform.
    pub fragments: &'a Fragments,
    /// A `web-dom` build's page.
    pub page: Option<&'a crate::build::web::WebPage>,
}

/// Makes `plan`'s package of `built` in `out`, returning each artifact's
/// kind and path.
pub fn make(
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
            let info = apple::Info {
                fragments: built.fragments.info.clone(),
                ..metadata.apple(built.bin)
            };
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
            let (entitlements, left_out) =
                apple::macos_entitlements(&access, &built.fragments.entitlements)
                    .map_err(|problem| package(problem.to_string()))?;
            if identity.is_some() {
                for (key, by) in left_out {
                    sink(CargoEvent::Log {
                        level: "warn",
                        line: format!(
                            "`{by}` asks for the entitlement `{key}`, which only a provisioning \
                             profile allows and a macOS package does not embed: the app is \
                             signed without it"
                        ),
                    });
                }
            }
            if plan.signing.signer != Signer::None {
                apple::sign_macos(&app.path, identity, &entitlements, &scratch).map_err(sign)?;
            }
            let dmg = path("dmg");
            apple::dmg(&app.path, &dmg, &metadata.display_name, &scratch).map_err(package)?;
            if identity.is_some() {
                apple::sign_macos(&dmg, identity, &Default::default(), &scratch).map_err(sign)?;
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
            let mut artifacts = vec![("archive", zip)];
            if metadata.icon.is_some() {
                let packed = path("msix");
                let executable = file("", &exe, true)?;
                let mut files = Vec::with_capacity(1 + msix::LOGOS.len());
                for (name, size) in msix::LOGOS {
                    files.push(Entry::file(name, png(size).unwrap_or_default()));
                }
                let authenticode = match &plan.signing.signer {
                    Signer::Authenticode {
                        identity,
                        timestamp_url,
                        signtool,
                    } => Some((identity, timestamp_url, signtool)),
                    _ => None,
                };
                let subject = metadata
                    .publisher
                    .clone()
                    .unwrap_or_else(|| msix::subject(&metadata.display_name));
                let publisher = if authenticode.is_some() {
                    subject.clone()
                } else {
                    msix::unsigned(&subject)
                };
                let manifest = msix::manifest(&msix::Manifest {
                    name: &metadata.package_name(),
                    publisher: &publisher,
                    publisher_name: &msix::holder(&subject)
                        .unwrap_or_else(|| metadata.display_name.clone()),
                    version: {
                        let [major, minor, patch, _] = metadata.windows_version();
                        // The Store keeps the last number for itself.
                        [major, minor, patch, 0]
                    },
                    architecture: msix::architecture(std::env::consts::ARCH).unwrap_or("x64"),
                    display_name: &metadata.display_name,
                    executable: &executable.name,
                    access: &metadata
                        .access()
                        .into_iter()
                        .map(|(a, _)| a)
                        .collect::<Vec<_>>(),
                    schemes: &metadata.links.schemes,
                });
                files.push(executable);
                msix::write(&files, &manifest, &packed).map_err(package)?;
                if let Some((identity, timestamp_url, signtool)) = authenticode {
                    windows::sign(signtool, &packed, identity, timestamp_url.as_deref())
                        .map_err(sign)?;
                }
                artifacts.push(("msix", packed));
            }
            Ok(artifacts)
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
                    linux::desktop_entry(
                        &metadata.display_name,
                        built.bin,
                        icon_name,
                        &metadata.links.schemes,
                    )
                    .into_bytes(),
                ),
            ];
            if let Some(png) = png(512) {
                entries.push(Entry::file(format!("{top}{name}.png"), png));
            }
            archive::tar_gz(&mut entries, &tar).map_err(package)?;
            Ok(vec![("archive", tar)])
        }
        Platform::Ios => {
            let info = apple::Info {
                fragments: built.fragments.info.clone(),
                ..metadata.apple(built.bin)
            };
            let app = apple::bundle(built.exe, &scratch, &info, false).map_err(package)?;
            if let Some(icon) = &metadata.icon {
                let opaque = icon::opaque(&icon::scaled(&icon.image, icon::MIN_SIZE));
                apple::compile_icons(&app.path, &opaque.to_png(), &metadata.minimum_os, &scratch)
                    .map_err(package)?;
            }
            if let Signer::Provisioned { identity, profile } = &plan.signing.signer {
                let entitlements = apple::Entitlements {
                    hosts: &metadata.links.hosts,
                    fragments: &built.fragments.entitlements,
                };
                apple::sign(
                    &app.path,
                    Some(identity),
                    Some(profile),
                    &entitlements,
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
            let worker = viso_package::fragment::worker(&built.fragments.worker);
            crate::build::web::bundle(
                built.exe,
                &site,
                built.bin,
                built.profile,
                built.web,
                worker.as_deref(),
                built.page,
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
/// The `rustc` arguments that link the icon and version `plan`'s Windows
/// package carries into the executable built for `triple`: a `.res` file
/// for MSVC's linker, a COFF object for GNU's, written in `dir` under a name
/// its content gives, so that a change links again.
pub fn link_resources(
    plan: &Plan,
    profile: Profile,
    triple: &str,
    dir: &Path,
) -> Result<Vec<String>, String> {
    let metadata = &plan.metadata;
    let icon: Vec<resources::IconImage> = match &metadata.icon {
        Some(source) => resources::ICON_SIZES
            .iter()
            .map(|&size| {
                let image = icon::scaled(&source.image, size);
                resources::IconImage {
                    size,
                    png: image.to_png(),
                    rgba: image.rgba,
                }
            })
            .collect(),
        None => Vec::new(),
    };
    let company = metadata.publisher.as_deref().and_then(msix::holder);
    let version = resources::Version {
        numbers: metadata.windows_version(),
        text: &metadata.version,
        product: &metadata.display_name,
        company: company.as_deref(),
        debug: profile == Profile::Dev,
    };
    let (bytes, extension) = if triple.ends_with("-msvc") {
        (resources::res(&icon, &version), "res")
    } else {
        let arch = triple.split('-').next().unwrap_or_default();
        let machine = resources::Machine::of(arch)
            .ok_or_else(|| format!("no resources are linked for `{triple}`"))?;
        (resources::object(&icon, &version, machine), "o")
    };
    let digest = Sha256::digest(&bytes);
    let name: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    let out = dir.join(format!("resources-{name}.{extension}"));
    if !out.is_file() {
        std::fs::create_dir_all(dir)
            .and_then(|()| crate::atomic::write(&out, &bytes))
            .map_err(|e| format!("cannot write `{}`: {e}", out.display()))?;
    }
    Ok(vec![format!("-Clink-arg={}", out.display())])
}

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
    use std::sync::Arc;

    use viso_image::snapshot::Image;
    use viso_package::archive;
    use viso_project::{Env, ProfileSettings, Project, Target};

    use super::*;
    use crate::delivery::{Policy, Signing, metadata};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-pkg-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The Windows package of a project with an icon, in `dir`.
    fn windows_plan(dir: &Path, version: &str) -> (Project, Plan) {
        let icon = Image {
            width: 1024,
            height: 1024,
            rgba: [40, 120, 200, 255].repeat(1024 * 1024),
        };
        std::fs::write(dir.join("icon.png"), icon.to_png()).unwrap();
        std::fs::write(
            dir.join("Viso.toml"),
            format!(
                "[package]\nname = \"my_app\"\ndisplay_name = \"My App\"\nversion = \"{version}\"\n\
                 build = 7\npermissions = {{ camera = \"Scans\" }}\n\n[package.icons]\n\
                 source = \"icon.png\"\n"
            ),
        )
        .unwrap();
        let project = Project::discover(dir).unwrap();
        let (metadata, diagnostics) = metadata::read(&project, Platform::Windows);
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let signing = Signing {
            policy: Policy::Auto,
            signer: Signer::None,
            shortfall: None,
        };
        let plan = Plan::new(
            Target::Host,
            Platform::Windows,
            metadata.unwrap(),
            signing,
            Vec::new(),
        );
        (project, plan)
    }

    #[test]
    fn the_resources_are_named_by_their_content() {
        let dir = scratch("resources");
        let (_, plan) = windows_plan(&dir, "1.2.3");
        let out = dir.join("build");
        let msvc = link_resources(&plan, Profile::Release, "x86_64-pc-windows-msvc", &out).unwrap();
        assert_eq!(msvc.len(), 1);
        let res = PathBuf::from(msvc[0].strip_prefix("-Clink-arg=").unwrap());
        assert_eq!(res.extension().unwrap(), "res");
        assert!(res.is_file());
        assert_eq!(
            link_resources(&plan, Profile::Release, "x86_64-pc-windows-msvc", &out).unwrap(),
            msvc
        );
        let gnu =
            link_resources(&plan, Profile::Release, "aarch64-pc-windows-gnullvm", &out).unwrap();
        assert!(gnu[0].ends_with(".o"));
        let object = std::fs::read(gnu[0].strip_prefix("-Clink-arg=").unwrap()).unwrap();
        assert_eq!(&object[0..2], &0xaa64u16.to_le_bytes());

        // A new version, or a debug build, links another file.
        let (_, newer) = windows_plan(&dir, "1.2.4");
        let next = link_resources(&newer, Profile::Release, "x86_64-pc-windows-msvc", &out);
        assert_ne!(next.unwrap(), msvc);
        let debug = link_resources(&plan, Profile::Dev, "x86_64-pc-windows-msvc", &out);
        assert_ne!(debug.unwrap(), msvc);
        assert!(link_resources(&plan, Profile::Release, "riscv64-pc-windows-gnu", &out).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_windows_package_is_a_zip_and_an_msix() {
        let dir = scratch("windows");
        let (project, plan) = windows_plan(&dir, "1.2.3");
        assert_eq!(
            plan.outputs.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
            ["archive", "msix"]
        );
        let exe = dir.join("my_app.exe");
        std::fs::write(&exe, b"MZ an executable").unwrap();
        let web = ProfileSettings::resolve(
            &project.manifest,
            Profile::Release,
            &Env::empty(),
            &mut Vec::new(),
        )
        .web;
        let built = Built {
            exe: &exe,
            more: &[],
            bin: "my_app",
            dir: &dir.join("build"),
            profile: Profile::Release,
            web: &web,
            android: None,
            fragments: &Fragments::default(),
            page: None,
        };
        let out = dir.join("dist");
        std::fs::create_dir_all(&out).unwrap();
        let (_keep, cancelled) = mpsc::channel();
        let sink: Sink = Arc::new(|_| true);
        let made = make(&plan, &built, &out, &cancelled, &sink).unwrap();
        assert_eq!(made.len(), 2);
        let (kind, msix) = &made[1];
        assert_eq!(*kind, "msix");
        let name = format!("my_app-1.2.3-{}.msix", std::env::consts::ARCH);
        assert_eq!(msix.file_name().unwrap().to_string_lossy(), name);

        let entries = archive::unzip(&std::fs::read(msix).unwrap()).unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Assets/Square150x150Logo.png",
                "Assets/Square44x44Logo.png",
                "Assets/StoreLogo.png",
                "my_app.exe",
                "AppxManifest.xml",
                "AppxBlockMap.xml",
                "[Content_Types].xml",
            ]
        );
        assert_eq!(entries[3].data, b"MZ an executable");
        let logo = Image::from_png(&entries[1].data).unwrap();
        assert_eq!((logo.width, logo.height), (44, 44));
        let manifest = String::from_utf8(entries[4].data.clone()).unwrap();
        // Unsigned, so marked as such; named by the project, version 1.2.3.
        assert!(manifest.contains(
            "<Identity Name=\"my-app\" Publisher=\"CN=My App, \
             OID.2.25.311729368913984317654407730594956997722=1\" Version=\"1.2.3.0\""
        ));
        assert!(manifest.contains("Executable=\"my_app.exe\""));
        assert!(manifest.contains("<DeviceCapability Name=\"webcam\"/>"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_archive_unpacks_into_its_own_name() {
        assert_eq!(
            archive_root(Path::new("/d/app-1.0.0-x86_64.tar.gz")),
            "app-1.0.0-x86_64/"
        );
        assert_eq!(archive_root(Path::new("app-1.0.0.zip")), "app-1.0.0/");
    }
}
