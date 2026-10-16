//! What a package says of the app, from `[package]` in `Viso.toml`, checked
//! for the platform before anything is built: each problem a diagnostic at
//! the line that causes it.

use std::path::PathBuf;

use viso_ende::JsonWriter;
use viso_package::android::{self, MIN_API};
use viso_package::apple::{self, MIN_IOS};
use viso_package::{Access, Orientations};
use viso_project::{ConfigCode, ConfigDiagnostic, Orientation, Permission, Project};
use viso_view::snapshot::Image;

use super::{Platform, icon};

/// The icon a package is drawn from.
#[derive(Debug, Clone)]
pub(crate) struct Icon {
    /// The source file.
    pub(crate) path: PathBuf,
    pub(crate) image: Image,
}

/// What a package says of the app.
#[derive(Debug, Clone)]
pub(crate) struct Metadata {
    /// `package.name`.
    pub(crate) name: String,
    /// The bundle identifier, on the platforms that have one.
    pub(crate) id: Option<String>,
    pub(crate) display_name: String,
    pub(crate) version: String,
    pub(crate) build: u32,
    pub(crate) icon: Option<Icon>,
    pub(crate) orientation: Orientation,
    /// Each permission, with the reason the system shows.
    pub(crate) permissions: Vec<(Permission, String)>,
    /// The lowest iOS the app runs on.
    pub(crate) minimum_os: String,
    /// The lowest Android API level.
    pub(crate) min_sdk: u32,
}

impl Metadata {
    /// The name a package's files are named by: the project's, with only
    /// letters, digits, `.`, `_` and `-`.
    pub(crate) fn file_name(&self) -> String {
        self.name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect()
    }

    /// The name of a macOS `.app`, as Finder shows it: the display name,
    /// without the characters a file name cannot hold.
    pub(crate) fn bundle_name(&self) -> String {
        self.display_name.replace(['/', ':'], "-")
    }

    /// What it asks the user for, as the packager names it.
    pub(crate) fn access(&self) -> Vec<(Access, String)> {
        self.permissions
            .iter()
            .map(|(permission, reason)| (access(*permission), reason.clone()))
            .collect()
    }

    pub(crate) fn orientations(&self) -> Orientations {
        match self.orientation {
            Orientation::Any => Orientations::Any,
            Orientation::Portrait => Orientations::Portrait,
            Orientation::Landscape => Orientations::Landscape,
        }
    }

    /// The Info.plist of the app whose executable is `executable`.
    pub(crate) fn apple(&self, executable: &str) -> apple::Info {
        apple::Info {
            executable: executable.to_owned(),
            id: self
                .id
                .clone()
                .unwrap_or_else(|| apple::bundle_id(executable)),
            display_name: self.display_name.clone(),
            version: self.version.clone(),
            build: self.build,
            minimum_os: self.minimum_os.clone(),
            orientations: self.orientations(),
            usage: self.access(),
        }
    }

    /// The Android manifest of the app whose executable is `executable`.
    pub(crate) fn android(&self, executable: &str) -> android::Manifest {
        android::Manifest {
            package: self
                .id
                .clone()
                .unwrap_or_else(|| android::package_name(executable)),
            label: self.display_name.clone(),
            version_code: self.build,
            version_name: self.version.clone(),
            min_sdk: self.min_sdk,
            access: self.access().into_iter().map(|(a, _)| a).collect(),
            internet: false,
            orientations: self.orientations(),
            icon: self.icon.is_some(),
        }
    }

    /// It as the `metadata` object of a plan.
    pub(crate) fn write(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("id");
        match &self.id {
            Some(id) => w.string(id),
            None => w.null(),
        }
        w.name("name");
        w.string(&self.name);
        w.name("display_name");
        w.string(&self.display_name);
        w.name("version");
        w.string(&self.version);
        w.name("build");
        w.uint(u64::from(self.build));
        w.name("icon");
        match &self.icon {
            Some(icon) => w.string(&icon.path.to_string_lossy()),
            None => w.null(),
        }
        w.name("orientation");
        w.string(self.orientation.as_str());
        w.name("permissions");
        w.begin_array();
        for (permission, _) in &self.permissions {
            w.string(permission.as_str());
        }
        w.end_array();
        w.end_object();
    }
}

/// The packager's name for `permission`.
fn access(permission: Permission) -> Access {
    match permission {
        Permission::Camera => Access::Camera,
        Permission::Microphone => Access::Microphone,
        Permission::Location => Access::Location,
        Permission::Photos => Access::Photos,
        Permission::Contacts => Access::Contacts,
        Permission::Bluetooth => Access::Bluetooth,
        Permission::Notifications => Access::Notifications,
    }
}

/// The metadata `project` gives a package for `platform`, with every
/// problem found on the way; `None` when one of them is an error.
pub(crate) fn read(
    project: &Project,
    platform: Platform,
) -> (Option<Metadata>, Vec<ConfigDiagnostic>) {
    let path = project.manifest_path();
    let package = &project.manifest.package;
    let mut diagnostics = Vec::new();
    let missing = |key: &str, why: String, note: &str| {
        ConfigDiagnostic::error(
            ConfigCode::MissingRequired,
            format!("`package.{key}` is required {why}"),
        )
        .at(&path)
        .note(note.to_owned())
    };
    let name = match project.name() {
        Ok(name) => name.to_owned(),
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let version = package.version.as_ref().map(|v| v.value.clone());
    if version.is_none() {
        diagnostics.push(missing(
            "version",
            "to package".to_owned(),
            "add `version = \"1.0.0\"` under `[package]`",
        ));
    }
    let id = package.bundle_id.as_ref().map(|id| id.value.clone());
    if id.is_none() && platform.needs_id() {
        diagnostics.push(missing(
            "bundle_id",
            format!("to package for {}", platform.name()),
            "add `bundle_id = \"com.example.app\"` under `[package]`",
        ));
    }

    let icon = match &package.icon {
        Some(source) => {
            let file = project.root.join(&source.value);
            let invalid = |why: String| {
                ConfigDiagnostic::error(
                    ConfigCode::InvalidValue,
                    format!("the icon `{}` is {why}", source.value),
                )
                .at(&path)
                .span(source.span)
                .note(format!(
                    "the icon is a square PNG of at least {} pixels",
                    icon::MIN_SIZE
                ))
            };
            match std::fs::read(&file) {
                Err(error) => {
                    diagnostics.push(invalid(format!("not readable: {error}")));
                    None
                }
                Ok(bytes) => match icon::read(&bytes) {
                    Err(why) => {
                        diagnostics.push(invalid(why));
                        None
                    }
                    Ok(image) => {
                        if platform == Platform::Ios && icon::transparent(&image) {
                            diagnostics.push(
                                ConfigDiagnostic::warning(
                                    ConfigCode::InvalidValue,
                                    format!("the icon `{}` has transparent pixels", source.value),
                                )
                                .at(&path)
                                .span(source.span)
                                .note("an iOS icon is opaque: they are shown over white"),
                            );
                        }
                        Some(Icon { path: file, image })
                    }
                },
            }
        }
        None => {
            if platform.shows_icon() {
                diagnostics.push(
                    ConfigDiagnostic::warning(
                        ConfigCode::MissingRequired,
                        "no `[package.icons] source`: the app shows the system's default icon",
                    )
                    .at(&path)
                    .note(format!(
                        "add `[package.icons]` with `source = \"assets/icon.png\"`, a square \
                         PNG of at least {} pixels",
                        icon::MIN_SIZE
                    )),
                );
            }
            None
        }
    };

    // The files signing reads must be there before anything is built.
    let named_file = |value: &viso_project::Spanned<String>, key: &str| {
        (!project.root.join(&value.value).is_file()).then(|| {
            ConfigDiagnostic::error(
                ConfigCode::InvalidValue,
                format!("`{key}` names `{}`, which is not a file", value.value),
            )
            .at(&path)
            .span(value.span)
        })
    };
    match platform {
        Platform::Ios => {
            if let Some(profile) = &package.ios.provisioning_profile {
                diagnostics.extend(named_file(profile, "package.ios.provisioning_profile"));
            }
        }
        Platform::Android => {
            let android = &package.android;
            if let Some(keystore) = &android.keystore {
                diagnostics.extend(named_file(keystore, "package.android.keystore"));
                if android.key_alias.is_none() {
                    diagnostics.push(
                        ConfigDiagnostic::error(
                            ConfigCode::MissingRequired,
                            "`package.android.key_alias` is required with a keystore",
                        )
                        .at(&path)
                        .span(keystore.span)
                        .note("name the key in the keystore the package is signed with"),
                    );
                }
            }
        }
        _ => {}
    }

    if diagnostics.iter().any(ConfigDiagnostic::is_error) {
        return (None, diagnostics);
    }
    let targets = &project.manifest.targets;
    let metadata = Metadata {
        display_name: package
            .display_name
            .as_ref()
            .map_or_else(|| name.clone(), |n| n.value.clone()),
        name,
        id,
        version: version.unwrap_or_default(),
        build: package.build.as_ref().map_or(1, |b| b.value),
        icon,
        orientation: package
            .orientation
            .as_ref()
            .map_or(Orientation::Any, |o| o.value),
        permissions: package
            .permissions
            .iter()
            .map(|(p, reason)| (p.value, reason.value.clone()))
            .collect(),
        minimum_os: targets
            .ios
            .minimum_os
            .as_ref()
            .map_or_else(|| MIN_IOS.to_owned(), |v| v.value.clone()),
        min_sdk: targets
            .android
            .min_sdk
            .as_ref()
            .map_or(MIN_API, |v| v.value.max(MIN_API)),
    };
    (Some(metadata), diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(dir: &std::path::Path, manifest: &str) -> Project {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("Viso.toml"), manifest).unwrap();
        viso_project::Project::discover(dir).unwrap()
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-meta-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_package_needs_its_version_and_the_platform_its_id() {
        let dir = scratch("needs");
        let p = project(&dir, "[package]\nname = \"My App\"\n");
        let (metadata, diagnostics) = read(&p, Platform::MacOs);
        assert!(metadata.is_none());
        let messages: Vec<&str> = diagnostics.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "`package.version` is required to package",
                "`package.bundle_id` is required to package for macOS",
                "no `[package.icons] source`: the app shows the system's default icon",
            ]
        );
        // The web needs no id and shows no icon.
        let (metadata, diagnostics) = read(
            &project(&dir, "[package]\nname = \"My App\"\nversion = \"1.2.0\"\n"),
            Platform::Web,
        );
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let metadata = metadata.unwrap();
        assert_eq!(metadata.file_name(), "My-App");
        assert_eq!(metadata.display_name, "My App");
        assert_eq!(metadata.build, 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_icon_and_signing_files_are_checked_where_named() {
        let dir = scratch("icon");
        std::fs::create_dir_all(&dir).unwrap();
        let small = Image {
            width: 64,
            height: 64,
            rgba: vec![0; 64 * 64 * 4],
        };
        std::fs::write(dir.join("icon.png"), small.to_png()).unwrap();
        let p = project(
            &dir,
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\nbundle_id = \"com.example.a\"\n\n\
             [package.icons]\nsource = \"icon.png\"\n\n[package.android]\nkeystore = \"k.jks\"\n",
        );
        let (metadata, diagnostics) = read(&p, Platform::Android);
        assert!(metadata.is_none());
        let found: Vec<(ConfigCode, u32)> = diagnostics
            .iter()
            .map(|d| (d.code, d.span.unwrap().line))
            .collect();
        assert_eq!(
            found,
            [
                (ConfigCode::InvalidValue, 7),
                (ConfigCode::InvalidValue, 10),
                (ConfigCode::MissingRequired, 10),
            ]
        );
        assert!(diagnostics[0].message.contains("less than 1024"));

        let big = Image {
            width: 1024,
            height: 1024,
            rgba: [10, 20, 30, 0].repeat(1024 * 1024),
        };
        std::fs::write(dir.join("icon.png"), big.to_png()).unwrap();
        let p = project(
            &dir,
            "[package]\nname = \"a\"\nversion = \"1.0.0\"\nbundle_id = \"com.example.a\"\n\
             build = 7\norientation = \"landscape\"\n\n[package.icons]\nsource = \"icon.png\"\n\n\
             [package.permissions]\ncamera = \"Scans\"\n",
        );
        let (metadata, diagnostics) = read(&p, Platform::Ios);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("transparent"));
        let metadata = metadata.unwrap();
        let info = metadata.apple("a");
        assert_eq!(
            (info.build, info.orientations),
            (7, Orientations::Landscape)
        );
        assert_eq!(info.usage, [(Access::Camera, "Scans".to_owned())]);
        let manifest = metadata.android("a");
        assert_eq!(manifest.package, "com.example.a");
        assert!(manifest.icon);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
