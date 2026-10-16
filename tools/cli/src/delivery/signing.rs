//! How a package is signed under `--signing`: `auto` signs with the
//! identity configured or found, else as far as the platform allows and
//! warns; `required` fails without a real one; `off` signs nothing.
//!
//! Configuration names an identity, a provisioning profile, a keystore or a
//! certificate, never a secret: an identity's key stays in the keychain or
//! the certificate store, and a keystore's passwords are read from the
//! environment by the signing tool itself.

use std::path::PathBuf;

use viso_ende::JsonWriter;
use viso_package::android::{KEY_PASSWORD_ENV, KEYSTORE_PASSWORD_ENV};
use viso_package::{apple, windows};
use viso_project::Project;

use super::{Metadata, Platform};

/// What `--signing` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Policy {
    Auto,
    Required,
    Off,
}

impl Policy {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Policy::Auto => "auto",
            Policy::Required => "required",
            Policy::Off => "off",
        }
    }
}

/// What signs a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Signer {
    /// Nothing.
    None,
    /// A macOS signature with no identity, valid on the Mac that made it.
    AdHoc,
    /// A macOS identity, under the hardened runtime.
    Apple { identity: String },
    /// An iOS identity and the provisioning profile it signs under.
    Provisioned { identity: String, profile: PathBuf },
    /// A release keystore and the key in it.
    Keystore { keystore: PathBuf, alias: String },
    /// The standard Android debug keystore.
    DebugKeystore,
    /// A Windows certificate, by subject name or thumbprint.
    Authenticode {
        identity: String,
        timestamp_url: Option<String>,
        signtool: PathBuf,
    },
}

impl Signer {
    /// The status `artifact.json` records: `none`, `ad-hoc`,
    /// `debug-keystore` or `signed`.
    pub(crate) fn status(&self) -> &'static str {
        match self {
            Signer::None => "none",
            Signer::AdHoc => "ad-hoc",
            Signer::DebugKeystore => "debug-keystore",
            Signer::Apple { .. }
            | Signer::Provisioned { .. }
            | Signer::Keystore { .. }
            | Signer::Authenticode { .. } => "signed",
        }
    }

    /// Who signs, when it is someone: the identity, or the keystore and key.
    pub(crate) fn identity(&self, root: &std::path::Path) -> Option<String> {
        match self {
            Signer::Apple { identity }
            | Signer::Provisioned { identity, .. }
            | Signer::Authenticode { identity, .. } => Some(identity.clone()),
            Signer::Keystore { keystore, alias } => Some(format!(
                "{} ({alias})",
                keystore.strip_prefix(root).unwrap_or(keystore).display()
            )),
            Signer::None | Signer::AdHoc | Signer::DebugKeystore => None,
        }
    }

    /// How the package is signed, as a sentence ends: `unsigned`, `signed
    /// ad hoc`, `signed with …`.
    pub(crate) fn summary(&self) -> String {
        match self {
            Signer::None => "unsigned".to_owned(),
            other => format!("signed {}", other.describe()),
        }
    }

    /// How it signs, after "signed".
    pub(crate) fn describe(&self) -> String {
        match self {
            Signer::None => "not at all".to_owned(),
            Signer::AdHoc => "ad hoc".to_owned(),
            Signer::Apple { identity } => {
                format!("with `{identity}` under the hardened runtime")
            }
            Signer::Provisioned { identity, profile } => format!(
                "with `{identity}` under `{}`",
                profile.file_name().map_or_else(
                    || profile.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            ),
            Signer::Keystore { keystore, alias } => format!(
                "with the key `{alias}` of `{}`",
                keystore.file_name().map_or_else(
                    || keystore.display().to_string(),
                    |n| n.to_string_lossy().into_owned()
                )
            ),
            Signer::DebugKeystore => "with the debug keystore".to_owned(),
            Signer::Authenticode { identity, .. } => format!("with the certificate `{identity}`"),
        }
    }
}

/// How a package is signed, and what that falls short of under `auto`.
#[derive(Debug, Clone)]
pub(crate) struct Signing {
    pub(crate) policy: Policy,
    pub(crate) signer: Signer,
    /// The warning `auto` gives when it signs short of what the platform's
    /// users need.
    pub(crate) shortfall: Option<Problem>,
}

impl Signing {
    /// It as a `signing` object: the policy, the status and the identity.
    pub(crate) fn write(&self, w: &mut JsonWriter, root: &std::path::Path) {
        w.begin_object();
        w.name("policy");
        w.string(self.policy.as_str());
        w.name("status");
        w.string(self.signer.status());
        w.name("identity");
        match self.signer.identity(root) {
            Some(identity) => w.string(&identity),
            None => w.null(),
        }
        w.end_object();
    }
}

/// Why a package cannot be signed as asked, or falls short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Problem {
    pub(crate) message: String,
    pub(crate) notes: Vec<String>,
}

impl Problem {
    fn new(message: impl Into<String>, notes: &[&str]) -> Problem {
        Problem {
            message: message.into(),
            notes: notes.iter().map(|&n| n.to_owned()).collect(),
        }
    }
}

/// What signing can find on this machine.
pub(crate) struct Machine<'a> {
    /// The keychain's code signing identities.
    pub(crate) identities: &'a dyn Fn() -> Result<Vec<String>, String>,
    /// Whether an environment variable is set.
    pub(crate) env: &'a dyn Fn(&str) -> bool,
    /// `signtool`.
    pub(crate) signtool: &'a dyn Fn() -> Option<PathBuf>,
    /// What a provisioning profile allows.
    pub(crate) provisioning: &'a dyn Fn(&std::path::Path) -> Result<apple::Provisioning, String>,
}

impl Machine<'static> {
    /// This machine.
    pub(crate) fn this() -> Machine<'static> {
        Machine {
            identities: &apple::identities,
            env: &|name| std::env::var_os(name).is_some_and(|v| !v.is_empty()),
            signtool: &windows::signtool,
            provisioning: &|profile| {
                let scratch =
                    std::env::temp_dir().join(format!("viso-provisioning-{}", std::process::id()));
                let result = apple::provisioning(profile, &scratch);
                let _ = std::fs::remove_dir_all(&scratch);
                result
            },
        }
    }
}

/// The identities the keychain holds whose names start with one of
/// `kinds`, in that order.
fn first_of(found: &[String], kinds: &[&str]) -> Option<String> {
    kinds
        .iter()
        .find_map(|kind| found.iter().find(|name| name.starts_with(kind)))
        .cloned()
}

/// How `metadata`'s package of `project` for `platform` is signed under
/// `policy`, or why it cannot be.
pub(crate) fn resolve(
    policy: Policy,
    platform: Platform,
    project: &Project,
    metadata: &Metadata,
    machine: &Machine<'_>,
) -> Result<Signing, Problem> {
    let signing = |signer: Signer, shortfall: Option<Problem>| Signing {
        policy,
        signer,
        shortfall,
    };
    if policy == Policy::Off {
        return Ok(signing(Signer::None, None));
    }
    let package = &project.manifest.package;
    // An identity named in configuration must be in the keychain.
    let keychain = || (machine.identities)();
    let configured = |identity: &str, key: &str| -> Result<String, Problem> {
        let found = keychain().map_err(|why| {
            Problem::new(format!("cannot list the keychain's identities: {why}"), &[])
        })?;
        if found.iter().any(|name| name.contains(identity)) {
            Ok(identity.to_owned())
        } else {
            Err(Problem::new(
                format!("the identity `{identity}` that `{key}` names is not in the keychain"),
                &["`security find-identity -v -p codesigning` lists the identities there"],
            ))
        }
    };
    match platform {
        Platform::MacOs => {
            let identity = match &package.macos.identity {
                Some(identity) => Some(configured(&identity.value, "package.macos.identity")?),
                None => first_of(
                    &keychain().unwrap_or_default(),
                    &["Developer ID Application:"],
                ),
            };
            let advice = "sign it with a Developer ID Application identity: name it in \
                          `[package.macos] identity`, or add its certificate to the keychain";
            match identity {
                Some(identity) => Ok(signing(Signer::Apple { identity }, None)),
                None if policy == Policy::Required => Err(Problem::new(
                    "no Developer ID Application identity to sign the app with",
                    &[advice],
                )),
                None => Ok(signing(
                    Signer::AdHoc,
                    Some(Problem::new(
                        "the app is signed ad hoc: it runs on the Mac that built it, and \
                         Gatekeeper refuses it elsewhere",
                        &[advice],
                    )),
                )),
            }
        }
        Platform::Ios => {
            let identity = match &package.ios.identity {
                Some(identity) => Some(configured(&identity.value, "package.ios.identity")?),
                None => first_of(
                    &keychain().unwrap_or_default(),
                    &[
                        "Apple Distribution:",
                        "iPhone Distribution:",
                        "Apple Development:",
                        "iPhone Developer:",
                    ],
                ),
            };
            let profile = package
                .ios
                .provisioning_profile
                .as_ref()
                .map(|p| project.root.join(&p.value));
            match (identity, profile) {
                (Some(identity), Some(profile)) => {
                    let allowed = (machine.provisioning)(&profile).map_err(|why| {
                        Problem::new(format!("cannot read the provisioning profile: {why}"), &[])
                    })?;
                    let id = metadata.id.as_deref().unwrap_or_default();
                    if !allowed.allows(id) {
                        return Err(Problem::new(
                            format!(
                                "the provisioning profile signs `{}`, not `{id}`",
                                allowed.app_id
                            ),
                            &["make a profile for the app's `package.bundle_id`"],
                        ));
                    }
                    if let Some(team) = &package.ios.team_id
                        && team.value != allowed.team
                    {
                        return Err(Problem::new(
                            format!(
                                "the provisioning profile is the team `{}`'s, not `{}`'s",
                                allowed.team, team.value
                            ),
                            &["`package.ios.team_id` and the profile name the same team"],
                        ));
                    }
                    Ok(signing(Signer::Provisioned { identity, profile }, None))
                }
                (identity, profile) => {
                    let mut notes = Vec::new();
                    if profile.is_none() {
                        notes.push(
                            "name the provisioning profile in `[package.ios] provisioning_profile`",
                        );
                    }
                    if identity.is_none() {
                        notes.push(
                            "add an Apple Distribution certificate to the keychain, or name an \
                             identity in `[package.ios] identity`",
                        );
                    }
                    if policy == Policy::Required {
                        Err(Problem::new("nothing to sign the iOS app with", &notes))
                    } else {
                        Ok(signing(
                            Signer::None,
                            Some(Problem::new(
                                "the `.ipa` is not signed: no device installs it until it is",
                                &notes,
                            )),
                        ))
                    }
                }
            }
        }
        Platform::Android => match (&package.android.keystore, &package.android.key_alias) {
            (Some(keystore), Some(alias)) => {
                if !(machine.env)(KEYSTORE_PASSWORD_ENV) {
                    return Err(Problem {
                        message: format!(
                            "the keystore's password is not in `{KEYSTORE_PASSWORD_ENV}`"
                        ),
                        notes: vec![format!(
                            "set it in the environment, from a CI secret or a password \
                             manager; `{KEY_PASSWORD_ENV}` holds the key's when it differs"
                        )],
                    });
                }
                Ok(signing(
                    Signer::Keystore {
                        keystore: project.root.join(&keystore.value),
                        alias: alias.value.clone(),
                    },
                    None,
                ))
            }
            _ => {
                let advice = "name the release keystore in `[package.android] keystore` and \
                              `key_alias`";
                if policy == Policy::Required {
                    Err(Problem::new(
                        "no release keystore to sign the package with",
                        &[advice],
                    ))
                } else {
                    Ok(signing(
                        Signer::DebugKeystore,
                        Some(Problem::new(
                            "the package is signed with the debug keystore: a store refuses it",
                            &[advice],
                        )),
                    ))
                }
            }
        },
        Platform::Windows => match &package.windows.identity {
            Some(identity) => {
                let signtool = (machine.signtool)().ok_or_else(|| {
                    Problem::new(
                        "no `signtool` to sign the executable with",
                        &["it comes with the Windows SDK"],
                    )
                })?;
                Ok(signing(
                    Signer::Authenticode {
                        identity: identity.value.clone(),
                        timestamp_url: package
                            .windows
                            .timestamp_url
                            .as_ref()
                            .map(|u| u.value.clone()),
                        signtool,
                    },
                    None,
                ))
            }
            None => {
                let advice = "name its certificate in `[package.windows] identity`: its subject \
                              name or SHA-1 thumbprint";
                if policy == Policy::Required {
                    Err(Problem::new(
                        "no certificate to sign the executable with",
                        &[advice],
                    ))
                } else {
                    Ok(signing(
                        Signer::None,
                        Some(Problem::new(
                            "the executable is not signed: SmartScreen warns whoever runs it",
                            &[advice],
                        )),
                    ))
                }
            }
        },
        Platform::Linux | Platform::Web => {
            if policy == Policy::Required {
                let what = if platform == Platform::Linux {
                    "a Linux archive"
                } else {
                    "a web bundle"
                };
                Err(Problem::new(
                    format!("{what} has no signature to make"),
                    &["`--signing auto` packages it as it is"],
                ))
            } else {
                Ok(signing(Signer::None, None))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::delivery::metadata;

    fn project(manifest: &str) -> (Project, Metadata) {
        let dir = std::env::temp_dir().join(format!(
            "viso-signing-{}-{}",
            std::process::id(),
            manifest.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Viso.toml"), manifest).unwrap();
        for file in ["k.jks", "p.mobileprovision"] {
            std::fs::write(dir.join(file), "").unwrap();
        }
        let project = Project::discover(&dir).unwrap();
        let platform = if manifest.contains("[package.ios]") {
            Platform::Ios
        } else {
            Platform::MacOs
        };
        let metadata = metadata::read(&project, platform).0.unwrap();
        (project, metadata)
    }

    const BASE: &str =
        "[package]\nname = \"a\"\nversion = \"1.0.0\"\nbundle_id = \"com.example.a\"\n\n";

    fn machine<'a>(
        identities: &'a dyn Fn() -> Result<Vec<String>, String>,
        env: &'a dyn Fn(&str) -> bool,
    ) -> Machine<'a> {
        Machine {
            identities,
            env,
            signtool: &|| None,
            provisioning: &|_: &Path| {
                Ok(apple::Provisioning {
                    team: "ABCDE12345".into(),
                    app_id: "ABCDE12345.com.example.*".into(),
                    entitlements: PathBuf::new(),
                })
            },
        }
    }

    fn manifest(extra: &str) -> String {
        format!("{BASE}{extra}")
    }

    #[test]
    fn macos_signs_with_the_identity_found_else_ad_hoc_under_auto() {
        let (p, m) = project(&manifest(""));
        let none = || Ok(Vec::new());
        let developer = || {
            Ok(vec![
                "Developer ID Application: Example (ABCDE12345)".to_owned(),
            ])
        };
        let env = |_: &str| false;

        let s = resolve(
            Policy::Auto,
            Platform::MacOs,
            &p,
            &m,
            &machine(&developer, &env),
        )
        .unwrap();
        assert_eq!(s.signer.status(), "signed");
        assert!(s.shortfall.is_none());

        let s = resolve(Policy::Auto, Platform::MacOs, &p, &m, &machine(&none, &env)).unwrap();
        assert_eq!(s.signer, Signer::AdHoc);
        assert!(s.shortfall.unwrap().message.contains("ad hoc"));

        let refused = resolve(
            Policy::Required,
            Platform::MacOs,
            &p,
            &m,
            &machine(&none, &env),
        );
        assert!(refused.unwrap_err().message.contains("no Developer ID"));

        let s = resolve(
            Policy::Off,
            Platform::MacOs,
            &p,
            &m,
            &machine(&developer, &env),
        )
        .unwrap();
        assert_eq!(s.signer, Signer::None);

        // A named identity missing from the keychain is refused even under auto.
        let (p, m) = project(&manifest(
            "[package.macos]\nidentity = \"Developer ID Application: Other\"\n",
        ));
        let refused = resolve(
            Policy::Auto,
            Platform::MacOs,
            &p,
            &m,
            &machine(&developer, &env),
        );
        assert!(refused.unwrap_err().message.contains("not in the keychain"));
    }

    #[test]
    fn ios_needs_an_identity_and_a_profile_for_the_app() {
        let distribution = || Ok(vec!["Apple Distribution: Example (ABCDE12345)".to_owned()]);
        let env = |_: &str| false;
        let (p, m) = project(&manifest(
            "[package.ios]\nprovisioning_profile = \"p.mobileprovision\"\n",
        ));
        let s = resolve(
            Policy::Required,
            Platform::Ios,
            &p,
            &m,
            &machine(&distribution, &env),
        )
        .unwrap();
        assert!(matches!(s.signer, Signer::Provisioned { .. }));

        let none = || Ok(Vec::new());
        let s = resolve(Policy::Auto, Platform::Ios, &p, &m, &machine(&none, &env)).unwrap();
        assert_eq!(s.signer, Signer::None);
        assert_eq!(s.shortfall.unwrap().notes.len(), 1);

        let (p, m) = project(&manifest(
            "[package.ios]\nteam_id = \"ZZZZZ12345\"\nprovisioning_profile = \"p.mobileprovision\"\n",
        ));
        let refused = resolve(
            Policy::Auto,
            Platform::Ios,
            &p,
            &m,
            &machine(&distribution, &env),
        );
        assert!(refused.unwrap_err().message.contains("team"));
    }

    #[test]
    fn android_reads_its_passwords_from_the_environment_only() {
        let none = || Ok(Vec::new());
        let (p, m) = project(&manifest(
            "[package.android]\nkeystore = \"k.jks\"\nkey_alias = \"upload\"\n",
        ));
        let unset = |_: &str| false;
        let refused = resolve(
            Policy::Auto,
            Platform::Android,
            &p,
            &m,
            &machine(&none, &unset),
        );
        assert!(refused.unwrap_err().message.contains(KEYSTORE_PASSWORD_ENV));
        let set = |name: &str| name == KEYSTORE_PASSWORD_ENV;
        let s = resolve(
            Policy::Required,
            Platform::Android,
            &p,
            &m,
            &machine(&none, &set),
        )
        .unwrap();
        assert_eq!(s.signer.identity(&p.root).unwrap(), "k.jks (upload)");

        let (p, m) = project(&manifest(""));
        let s = resolve(
            Policy::Auto,
            Platform::Android,
            &p,
            &m,
            &machine(&none, &unset),
        )
        .unwrap();
        assert_eq!(s.signer, Signer::DebugKeystore);
        assert!(
            resolve(
                Policy::Required,
                Platform::Web,
                &p,
                &m,
                &machine(&none, &unset)
            )
            .is_err()
        );
        let s = resolve(
            Policy::Required,
            Platform::Windows,
            &p,
            &m,
            &machine(&none, &unset),
        );
        assert!(s.unwrap_err().message.contains("certificate"));
    }
}
