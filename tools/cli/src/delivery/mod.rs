//! Delivery: what `viso package` makes of a build — the metadata it
//! carries, how it is signed, the files it ships as — decided before
//! anything is built, so `--dry-run` shows exactly what a package would be.
//!
//! - [`metadata`]: the app's id, names, version, icon, orientation and
//!   permissions from `Viso.toml`, checked for the platform.
//! - [`signing`]: the identity, keystore or certificate a package is signed
//!   with under `--signing auto|required|off`; a secret never leaves the
//!   keychain or the environment.
//! - [`package`]: the built executables wrapped as the platform ships
//!   them.

pub(crate) mod icon;
pub(crate) mod metadata;
pub(crate) mod package;
pub(crate) mod signing;

use std::path::Path;

use viso_ende::JsonWriter;
use viso_package::android::Abi;
use viso_project::{HostOs, Target};

pub(crate) use metadata::Metadata;
pub(crate) use signing::{Policy, Signer, Signing};

/// What a package is made for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Platform {
    MacOs,
    Windows,
    Linux,
    Ios,
    Android,
    Web,
}

impl Platform {
    /// The platform `target` packages for on this machine, if any.
    pub(crate) fn of(target: Target) -> Option<Platform> {
        Some(match target {
            Target::Host => match HostOs::current()? {
                HostOs::MacOs => Platform::MacOs,
                HostOs::Windows => Platform::Windows,
                HostOs::Linux => Platform::Linux,
            },
            Target::Headless | Target::Device(_) => return None,
            Target::Ios => Platform::Ios,
            Target::Android => Platform::Android,
            Target::WebGpu | Target::WebDom | Target::WebHybrid => Platform::Web,
        })
    }

    /// Its name, as a sentence says it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Platform::MacOs => "macOS",
            Platform::Windows => "Windows",
            Platform::Linux => "Linux",
            Platform::Ios => "iOS",
            Platform::Android => "Android",
            Platform::Web => "the web",
        }
    }

    /// Whether its packages carry a bundle identifier.
    pub(crate) fn needs_id(self) -> bool {
        matches!(self, Platform::MacOs | Platform::Ios | Platform::Android)
    }

    /// Whether its packages carry an icon.
    pub(crate) fn shows_icon(self) -> bool {
        !matches!(self, Platform::Web)
    }
}

/// A package to make: what goes in, how it is signed, what comes out.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    pub(crate) target: Target,
    pub(crate) platform: Platform,
    pub(crate) metadata: Metadata,
    pub(crate) signing: Signing,
    /// The ABIs an Android package carries, the first built first.
    pub(crate) abis: Vec<Abi>,
    /// What it writes under `dist/<platform>/`: each artifact's kind and
    /// `/`-separated path.
    pub(crate) outputs: Vec<(&'static str, String)>,
}

impl Plan {
    /// The package of `metadata` for `target`, signed as `signing` says.
    pub(crate) fn new(
        target: Target,
        platform: Platform,
        metadata: Metadata,
        signing: Signing,
        abis: Vec<Abi>,
    ) -> Plan {
        let stem = format!("{}-{}", metadata.file_name(), metadata.version);
        let arch = std::env::consts::ARCH;
        let outputs = match platform {
            Platform::MacOs => vec![
                ("app-bundle", format!("{}.app", metadata.bundle_name())),
                ("dmg", format!("{stem}-{arch}.dmg")),
            ],
            Platform::Windows => {
                let mut outputs = vec![("archive", format!("{stem}-{arch}.zip"))];
                // Its tiles are drawn from the icon.
                if metadata.icon.is_some() {
                    outputs.push(("msix", format!("{stem}-{arch}.msix")));
                }
                outputs
            }
            Platform::Linux => vec![("archive", format!("{stem}-{arch}.tar.gz"))],
            Platform::Ios => vec![("ipa", format!("{stem}.ipa"))],
            Platform::Android => vec![
                ("apk", format!("{stem}.apk")),
                ("aab", format!("{stem}.aab")),
            ],
            Platform::Web => vec![
                ("site", "site".to_owned()),
                ("archive", format!("{stem}-{}.zip", target.as_str())),
            ],
        };
        Plan {
            target,
            platform,
            metadata,
            signing,
            abis,
            outputs,
        }
    }

    /// The steps that make it, as a reader follows them.
    pub(crate) fn steps(&self) -> Vec<String> {
        let mut steps = Vec::new();
        let compile = match self.platform {
            Platform::Android => format!(
                "compile for {}",
                self.abis
                    .iter()
                    .map(|abi| abi.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Platform::Ios => format!("compile for {}", viso_package::apple::DEVICE_TRIPLE),
            Platform::Windows if self.metadata.icon.is_some() => {
                "compile, linking in the icon and the version".to_owned()
            }
            Platform::Windows => "compile, linking in the version".to_owned(),
            _ => "compile".to_owned(),
        };
        if self.platform == Platform::Windows && self.metadata.icon.is_some() {
            steps.push("render the icon at 16 to 256 pixels".to_owned());
        }
        steps.push(compile);
        let icon = self.metadata.icon.is_some();
        let signed = self.signing.signer != Signer::None;
        let sign = self.signing.signer.describe();
        let output = |kind: &str| {
            self.outputs
                .iter()
                .find(|(k, _)| *k == kind)
                .map(|(_, name)| name.clone())
                .unwrap_or_default()
        };
        match self.platform {
            Platform::MacOs => {
                if icon {
                    steps.push("render the icon at 16 to 1024 pixels".to_owned());
                }
                steps.push(format!("bundle {}", output("app-bundle")));
                if signed {
                    steps.push(format!("sign it {sign}"));
                }
                steps.push(format!("make the disk image {}", output("dmg")));
                if matches!(self.signing.signer, Signer::Apple { .. }) {
                    steps.push(format!("sign the disk image {sign}"));
                }
            }
            Platform::Windows => {
                if signed {
                    steps.push(format!("sign the executable {sign}"));
                }
                steps.push(format!("archive {}", output("archive")));
                if icon {
                    steps.push(format!("pack the app package {}", output("msix")));
                    if signed {
                        steps.push(format!("sign the app package {sign}"));
                    }
                }
            }
            Platform::Linux => {
                if icon {
                    steps.push("render the icon at 512 pixels".to_owned());
                }
                steps.push("write the desktop entry".to_owned());
                steps.push(format!("archive {}", output("archive")));
            }
            Platform::Ios => {
                steps.push("bundle the app for devices".to_owned());
                if icon {
                    steps.push("compile the icon into the asset catalog".to_owned());
                }
                if signed {
                    steps.push(format!("sign it {sign}"));
                }
                steps.push(format!("archive {}", output("ipa")));
            }
            Platform::Android => {
                if icon {
                    steps.push("render the launcher icon at each density".to_owned());
                }
                steps.push(format!("pack and align {}", output("apk")));
                steps.push(format!("lay out the app bundle {}", output("aab")));
                if signed {
                    steps.push(format!("sign both {sign}"));
                }
            }
            Platform::Web => {
                steps.push("bundle the module, its glue and page into site/".to_owned());
                steps.push(format!("archive {}", output("archive")));
            }
        }
        steps
    }

    /// The `--dry-run` result: the target, the metadata, the signing, the
    /// steps and where each output goes, relative to the project root.
    /// `name` is the target as `dist/` names it: the platform for this
    /// machine.
    pub(crate) fn write(
        &self,
        w: &mut JsonWriter,
        name: &str,
        profile: &str,
        dist: &str,
        root: &Path,
    ) {
        w.begin_object();
        w.name("target");
        w.string(name);
        w.name("profile");
        w.string(profile);
        w.name("metadata");
        self.metadata.write(w);
        w.name("signing");
        self.signing.write(w, root);
        w.name("steps");
        w.begin_array();
        for step in self.steps() {
            w.string(&step);
        }
        w.end_array();
        w.name("output");
        w.string(dist);
        w.name("artifacts");
        w.begin_array();
        for (kind, name) in &self.outputs {
            w.begin_object();
            w.name("kind");
            w.string(kind);
            w.name("path");
            w.string(&format!("{dist}/{name}"));
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }

    /// The `--dry-run` result as text.
    pub(crate) fn text(&self, name: &str, profile: &str, dist: &str, root: &Path) -> String {
        let m = &self.metadata;
        let mut rows: Vec<(&str, String)> = Vec::new();
        if let Some(id) = &m.id {
            rows.push(("id", id.clone()));
        }
        rows.push(("name", m.display_name.clone()));
        rows.push(("version", format!("{} ({})", m.version, m.build)));
        if let Some(icon) = &m.icon {
            let path = icon.path.strip_prefix(root).unwrap_or(&icon.path);
            rows.push(("icon", path.display().to_string()));
        }
        if matches!(self.platform, Platform::Ios | Platform::Android) {
            rows.push(("orientation", m.orientation.as_str().to_owned()));
        }
        if !m.permissions.is_empty() {
            let names: Vec<&str> = m.permissions.iter().map(|(p, _)| p.as_str()).collect();
            rows.push(("permissions", names.join(", ")));
        }
        // Verified web links exist on the phones alone.
        let hosts = matches!(self.platform, Platform::Ios | Platform::Android);
        let links: Vec<String> = m
            .links
            .schemes
            .iter()
            .map(|s| format!("{s}://"))
            .chain(
                m.links
                    .hosts
                    .iter()
                    .filter(|_| hosts)
                    .map(|h| format!("https://{h}")),
            )
            .collect();
        if !links.is_empty() {
            rows.push(("links", links.join(", ")));
        }
        rows.push(("signing", self.signing.signer.summary()));
        let mut text = format!("Package {name} ({profile})\n");
        for (key, value) in rows {
            text.push_str(&format!("  {key:<12} {value}\n"));
        }
        for (i, step) in self.steps().iter().enumerate() {
            let key = if i == 0 { "steps" } else { "" };
            text.push_str(&format!("  {key:<12} {}. {step}\n", i + 1));
        }
        for (i, (_, name)) in self.outputs.iter().enumerate() {
            let key = if i == 0 { "output" } else { "" };
            text.push_str(&format!("  {key:<12} {dist}/{name}\n"));
        }
        text
    }
}
