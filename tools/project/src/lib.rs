//! Project discovery, configuration and build identity for the Viso tooling.
//!
//! This crate answers three questions, and nothing else:
//!
//! 1. **What project am I in?** — [`Project::locate`] walks up to a `Viso.toml`, or
//!    opens the one `--project` names.
//! 2. **What is the resolved configuration?** — [`config::resolve`] applies the five
//!    precedence layers and records, for every value, the layer it won from;
//!    [`settings`] lists every key so.
//! 3. **What am I building, and where does it go?** — [`BuildId`] identifies the build,
//!    and [`Layout`] fixes every path under `target/viso/` and `dist/`.
//!
//! It deliberately depends on no `viso-*` crate. A resolved configuration is plain
//! data, and keeping it that way is what lets `viso --help` and `viso config show`
//! run without initializing any part of the framework, while keeping the
//! dependency direction one-way: `tools/* → framework crates`, never the reverse.
//!
//! Nothing here prints. Diagnostics are returned as [`ConfigDiagnostic`] values with
//! spans, and the command layer decides whether they become human text or JSON lines —
//! a crate that writes to stderr cannot be used by `viso config validate --json`.

#![deny(missing_docs)]

pub mod cache;
pub mod config;
pub mod device;
pub mod diag;
pub mod discovery;
pub mod fingerprint;
pub mod manifest;
pub mod settings;
pub mod target;

#[cfg(test)]
mod scratch;

pub use cache::{CacheKind, Layout, LockError, LockGuard, LockHolder, LockKind};
pub use config::{
    ENV_OPT_LEVEL, ENV_PROFILE, ENV_SOURCE_MAPS, ENV_STRIP, ENV_VARS, Env, Origin, Overrides,
    ProfileSettings, Resolved, ResolvedConfig, Sourced, resolve,
};
pub use device::{DeviceProfile, DeviceStore, Platform, Preset, Selection};
pub use diag::{ConfigCode, ConfigDiagnostic, Severity, Span};
pub use discovery::{MANIFEST_NAME, Project, find_root};
pub use fingerprint::{BuildId, FINGERPRINT_VERSION, Hash128, ProjectFingerprint, Toolchain};
pub use manifest::{CollisionDelivery, GameDeterminism, Manifest, ParseOutcome, Spanned, nearest};
pub use settings::{Entry, KEYS, Value, setting, settings};
pub use target::{ArtifactKind, DevRuntime, HostOs, OptLevel, Profile, Target};

/// Locates a project and resolves its configuration in one step.
///
/// The shape almost every command wants: `--project`, the target the command
/// line named, flags and the environment in, a [`Resolved`] out. Offered as a
/// function rather than made the only entry point because `viso new` and `viso
/// completion` have no project.
pub fn load(
    explicit_project: Option<&std::path::Path>,
    cwd: impl AsRef<std::path::Path>,
    target: Target,
    artifact: ArtifactKind,
    flags: &Overrides,
    env: &Env,
) -> Result<(Project, Resolved), Vec<ConfigDiagnostic>> {
    let project = Project::locate(explicit_project, cwd)?;
    let resolved = resolve(&project, target, artifact, flags, env)?;
    Ok((project, resolved))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    /// The one-call path a command takes, end to end: discover, parse, resolve,
    /// identify, place. If these five compose, the crate does its job.
    #[test]
    fn a_project_resolves_from_discovery_through_to_its_output_paths() {
        let s = Scratch::new("lib");
        s.write(
            "Viso.toml",
            "[package]\nname = \"demo\"\nbundle_id = \"com.example.demo\"\n",
        );
        s.write("src/main.rs", "fn main() {}");
        let nested = s.dir("src/features");

        let (project, resolved) = load(
            None,
            &nested,
            Target::Headless,
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap();

        assert_eq!(project.root, s.path());
        assert_eq!(project.name().unwrap(), "demo");
        assert_eq!(resolved.config.target, Target::Headless);
        assert_eq!(resolved.config.profile.value, Profile::Dev);
        assert_eq!(resolved.config.dev_runtime(), DevRuntime::Present);
        assert!(resolved.warnings.is_empty());

        let fingerprint = ProjectFingerprint::compute(&project.root).unwrap();
        let toolchain = Toolchain::new("rustc 1.98.1", Toolchain::current_host());
        let id = resolved.config.build_id(fingerprint, &toolchain);

        let layout = Layout::new(&project.root);
        assert_eq!(
            layout.build(id),
            s.path().join("target/viso/build").join(id.to_hex())
        );
        assert_eq!(
            layout.dist(Target::Headless),
            s.path().join("dist/headless")
        );

        // Editing a source file moves the identity; nothing else has to change.
        s.write("src/main.rs", "fn main() { println!(); }");
        let after = ProjectFingerprint::compute(&project.root).unwrap();
        assert_ne!(
            id,
            resolved.config.build_id(after, &toolchain),
            "a source edit must produce a new build id"
        );
    }

    /// The whole point of the provenance layer, exercised through the public surface:
    /// a resolved value can say where it came from, which is what `viso config show`
    /// prints and what makes an unexpected build explainable.
    #[test]
    fn a_resolved_value_can_explain_itself() {
        let s = Scratch::new("lib-provenance");
        s.write(
            "Viso.toml",
            "[package]\nname = \"demo\"\n\n[profile.dev]\nopt_level = 1\n",
        );
        let project = Project::locate(None, s.path()).unwrap();
        let resolved = resolve(
            &project,
            Target::Host,
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap();

        let entry = setting(&project, &resolved.config, &Env::empty(), "opt_level").unwrap();
        assert_eq!(entry.value, Value::Text("1".into()));
        assert_eq!(entry.origin.layer(), "profile");
        assert_eq!(
            entry.to_string(),
            "opt_level = 1 (Viso.toml [profile.dev]:5:13)"
        );
    }
}
