//! Configuration precedence and provenance (`Viso_CLI.md` section 5).
//!
//! ```text
//! CLI flag
//!     > VISO_* environment variable
//!     > Viso.toml profile override
//!     > Viso.toml project default
//!     > framework default
//! ```
//!
//! Every resolved value carries the [`Origin`] it won from, which is what makes
//! `viso config show` able to answer "why is this value what it is" rather than
//! only "what is it". That question is the one a user actually has when a build
//! behaves unexpectedly, and answering it needs the layer *and* the line — so the
//! manifest origins carry a [`Span`] and the environment origin carries the
//! variable name.
//!
//! The environment is injected ([`Env`]) rather than read from the process at the
//! point of use. Precedence is the kind of logic that has to be tested with all
//! four layers in play at once, and a function that reads `std::env` directly can
//! only be tested by mutating global state in a process where other tests are
//! running in parallel.

use std::collections::BTreeMap;
use std::fmt;

use crate::diag::{ConfigCode, ConfigDiagnostic, Span};
use crate::discovery::Project;
use crate::fingerprint::{BuildId, Hash128, Hasher, ProjectFingerprint, Toolchain};
use crate::manifest::{Manifest, Spanned};
use crate::target::{ArtifactKind, DevRuntime, OptLevel, Profile, Target};

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A command-line flag.
    Flag,
    /// A `VISO_*` environment variable, named.
    Env(&'static str),
    /// A `[profile.X]` table in `Viso.toml`.
    ManifestProfile {
        /// Which profile's table.
        profile: Profile,
        /// Where in the manifest.
        span: Span,
    },
    /// A project-wide key in `Viso.toml` (outside any profile table).
    ManifestDefault {
        /// Where in the manifest.
        span: Span,
    },
    /// Nothing named it; this is Viso's own default.
    FrameworkDefault,
}

impl Origin {
    /// The layer name, without the location — the stable machine-readable form.
    pub const fn layer(self) -> &'static str {
        match self {
            Origin::Flag => "flag",
            Origin::Env(_) => "env",
            Origin::ManifestProfile { .. } => "profile",
            Origin::ManifestDefault { .. } => "manifest",
            Origin::FrameworkDefault => "default",
        }
    }

    /// Precedence rank, highest-winning first. Exists only so the tests can state
    /// the documented order in one place and check the resolver against it; the
    /// resolver itself gets its order from the structure of [`pick`].
    #[cfg(test)]
    const fn rank(self) -> u8 {
        match self {
            Origin::Flag => 0,
            Origin::Env(_) => 1,
            Origin::ManifestProfile { .. } => 2,
            Origin::ManifestDefault { .. } => 3,
            Origin::FrameworkDefault => 4,
        }
    }
}

impl fmt::Display for Origin {
    /// The human form: `flag`, `env VISO_TARGET`, `Viso.toml [profile.dev]:5:13`,
    /// `Viso.toml:3:18`, `default`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Flag => f.write_str("flag"),
            Origin::Env(name) => write!(f, "env {name}"),
            Origin::ManifestProfile { profile, span } => {
                write!(f, "Viso.toml [profile.{profile}]:{span}")
            }
            Origin::ManifestDefault { span } => write!(f, "Viso.toml:{span}"),
            Origin::FrameworkDefault => f.write_str("default"),
        }
    }
}

/// A resolved value and the layer it won from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sourced<T> {
    /// The value in effect.
    pub value: T,
    /// Where it came from.
    pub origin: Origin,
}

impl<T> Sourced<T> {
    /// Pairs a value with its origin.
    pub const fn new(value: T, origin: Origin) -> Self {
        Self { value, origin }
    }
}

impl<T: fmt::Display> fmt::Display for Sourced<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.value, self.origin)
    }
}

/// Values named on the command line.
///
/// `None` means "the flag was not given", which is what lets the next layer
/// speak. A `Default` here would collapse that distinction and make every flag
/// always win.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Overrides {
    /// `--target`.
    pub target: Option<Target>,
    /// `--profile`.
    pub profile: Option<Profile>,
    /// `--opt-level`.
    pub opt_level: Option<OptLevel>,
    /// `--source-maps` / `--no-source-maps`.
    pub source_maps: Option<bool>,
    /// `--strip` / `--no-strip`.
    pub strip: Option<bool>,
}

/// The `VISO_*` environment, captured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

/// `VISO_TARGET`.
pub const ENV_TARGET: &str = "VISO_TARGET";
/// `VISO_PROFILE`.
pub const ENV_PROFILE: &str = "VISO_PROFILE";
/// `VISO_OPT_LEVEL`.
pub const ENV_OPT_LEVEL: &str = "VISO_OPT_LEVEL";
/// `VISO_SOURCE_MAPS`.
pub const ENV_SOURCE_MAPS: &str = "VISO_SOURCE_MAPS";
/// `VISO_STRIP`.
pub const ENV_STRIP: &str = "VISO_STRIP";

/// Every variable this crate reads, in resolution order.
pub const ENV_VARS: &[&str] = &[
    ENV_TARGET,
    ENV_PROFILE,
    ENV_OPT_LEVEL,
    ENV_SOURCE_MAPS,
    ENV_STRIP,
];

impl Env {
    /// An empty environment.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Captures the `VISO_*` variables from the process.
    ///
    /// Only the `VISO_` prefix is captured, so the resolved configuration cannot
    /// accidentally depend on an unrelated variable.
    pub fn from_process() -> Self {
        Self {
            vars: std::env::vars()
                .filter(|(name, _)| name.starts_with("VISO_"))
                .collect(),
        }
    }

    /// Builds an environment from explicit pairs.
    pub fn from_pairs<K: Into<String>, V: Into<String>>(
        pairs: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        Self {
            vars: pairs
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        }
    }

    /// The value of one variable, ignoring an empty one.
    ///
    /// An empty `VISO_TARGET=` is treated as unset: in a shell, that is how a
    /// variable gets cleared, and reading it as a target named `""` would turn a
    /// cleared variable into an error.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }
}

/// A fully resolved configuration.
///
/// Total: every field has a value and an origin, so no consumer has to re-apply a
/// default and risk applying a different one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Where the output runs.
    pub target: Sourced<Target>,
    /// How it is compiled.
    pub profile: Sourced<Profile>,
    /// Optimization level.
    pub opt_level: Sourced<OptLevel>,
    /// Whether source maps are emitted.
    pub source_maps: Sourced<bool>,
    /// Whether symbols are stripped.
    pub strip: Sourced<bool>,
    /// What shape the output has — chosen by the command, not configurable.
    pub artifact: ArtifactKind,
}

impl ResolvedConfig {
    /// Whether the Dev Runtime is compiled in.
    ///
    /// Derived from the profile and carrying no [`Origin`], because there is no
    /// layer that could have named it: `Viso_CLI.md` section 38.2 fixes it, and a
    /// provenance field would imply it was negotiable.
    pub const fn dev_runtime(&self) -> DevRuntime {
        self.profile.value.dev_runtime()
    }

    /// A hash over the resolved *values*, one of the [`BuildId`] inputs.
    ///
    /// Origins are excluded on purpose. Two builds that resolved the same values by
    /// different routes — one from a flag, one from the manifest — produce byte-
    /// identical output, so they must share a [`BuildId`] and reuse each other's
    /// cache.
    pub fn hash(&self) -> Hash128 {
        let mut hasher = Hasher::new();
        hasher.str(self.target.value.as_str());
        hasher.str(self.profile.value.as_str());
        hasher.str(self.opt_level.value.as_str());
        hasher.u64(u64::from(self.source_maps.value));
        hasher.u64(u64::from(self.strip.value));
        hasher.str(self.artifact.as_str());
        hasher.finish()
    }

    /// The [`BuildId`] for this configuration.
    pub fn build_id(&self, fingerprint: ProjectFingerprint, toolchain: &Toolchain) -> BuildId {
        BuildId::compute(
            fingerprint,
            self.target.value,
            self.profile.value,
            toolchain,
            self.hash(),
        )
    }

    /// Every resolved key, in a stable order — the body of `viso config show`.
    pub fn entries(&self) -> Vec<Entry> {
        vec![
            Entry::new("target", self.target.value, self.target.origin),
            Entry::new("profile", self.profile.value, self.profile.origin),
            Entry::new("opt_level", self.opt_level.value, self.opt_level.origin),
            Entry::new(
                "source_maps",
                self.source_maps.value,
                self.source_maps.origin,
            ),
            Entry::new("strip", self.strip.value, self.strip.origin),
            // Derived, so its origin is the framework's: nothing else may claim it.
            Entry::new("dev_runtime", self.dev_runtime(), Origin::FrameworkDefault),
            Entry::new("artifact", self.artifact, Origin::FrameworkDefault),
        ]
    }

    /// One resolved key by name — the body of `viso config get <key>`.
    pub fn get(&self, key: &str) -> Option<Entry> {
        self.entries().into_iter().find(|e| e.key == key)
    }

    /// Every key name, for diagnostics and shell completion.
    pub fn keys(&self) -> Vec<&'static str> {
        self.entries().into_iter().map(|e| e.key).collect()
    }
}

/// One line of `viso config show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The key name.
    pub key: &'static str,
    /// The resolved value, printed.
    pub value: String,
    /// Where it came from.
    pub origin: Origin,
}

impl Entry {
    fn new(key: &'static str, value: impl fmt::Display, origin: Origin) -> Self {
        Self {
            key,
            value: value.to_string(),
            origin,
        }
    }
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {} ({})", self.key, self.value, self.origin)
    }
}

/// A resolved configuration plus the warnings collected on the way.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The configuration.
    pub config: ResolvedConfig,
    /// Non-fatal problems, including any carried from manifest parsing.
    pub warnings: Vec<ConfigDiagnostic>,
}

/// Resolves a configuration for a located project.
///
/// Fails only on a value that cannot be honored: an unparseable environment
/// variable, or a target this build cannot produce. Everything else resolves,
/// possibly with warnings.
pub fn resolve(
    project: &Project,
    artifact: ArtifactKind,
    flags: &Overrides,
    env: &Env,
) -> Result<Resolved, Vec<ConfigDiagnostic>> {
    let mut warnings = project.warnings.clone();
    let mut errors = Vec::new();
    let manifest = &project.manifest;

    let profile = resolve_profile(flags, env, &mut errors);
    let target = resolve_target(manifest, flags, env, &mut errors);
    let table = manifest.profiles.get(profile.value);

    let opt_level = pick(
        flags.opt_level,
        env_value(env, ENV_OPT_LEVEL, OptLevel::parse, &mut errors),
        from_profile(profile.value, table.opt_level),
        None,
        profile.value.default_opt_level(),
    );
    let source_maps = pick(
        flags.source_maps,
        env_value(env, ENV_SOURCE_MAPS, parse_bool, &mut errors),
        from_profile(profile.value, table.source_maps),
        None,
        profile.value.default_source_maps(),
    );
    let strip = pick(
        flags.strip,
        env_value(env, ENV_STRIP, parse_bool, &mut errors),
        from_profile(profile.value, table.strip),
        None,
        profile.value.default_strip(),
    );

    if !errors.is_empty() {
        warnings.extend(errors);
        return Err(warnings);
    }

    Ok(Resolved {
        config: ResolvedConfig {
            target,
            profile,
            opt_level,
            source_maps,
            strip,
            artifact,
        },
        warnings,
    })
}

/// The precedence rule itself, once, for every key.
///
/// Writing it as one function rather than a chain per field is what keeps the five
/// layers in one order everywhere: a hand-rolled `if let` chain per key is how one
/// key ends up letting the manifest beat the environment.
fn pick<T>(
    flag: Option<T>,
    env: Option<(T, &'static str)>,
    profile: Option<(T, Profile, Span)>,
    manifest: Option<(T, Span)>,
    default: T,
) -> Sourced<T> {
    if let Some(value) = flag {
        return Sourced::new(value, Origin::Flag);
    }
    if let Some((value, name)) = env {
        return Sourced::new(value, Origin::Env(name));
    }
    if let Some((value, profile, span)) = profile {
        return Sourced::new(value, Origin::ManifestProfile { profile, span });
    }
    if let Some((value, span)) = manifest {
        return Sourced::new(value, Origin::ManifestDefault { span });
    }
    Sourced::new(default, Origin::FrameworkDefault)
}

fn from_profile<T>(profile: Profile, value: Option<Spanned<T>>) -> Option<(T, Profile, Span)> {
    value.map(|v| (v.value, profile, v.span))
}

/// Reads and parses one environment variable, recording a diagnostic on failure.
///
/// A bad `VISO_*` value is an error rather than a skipped layer: the user set it
/// deliberately, and quietly falling through to the manifest would build something
/// they did not ask for.
fn env_value<T>(
    env: &Env,
    name: &'static str,
    parse: impl FnOnce(&str) -> Result<T, ConfigDiagnostic>,
    errors: &mut Vec<ConfigDiagnostic>,
) -> Option<(T, &'static str)> {
    let raw = env.get(name)?;
    match parse(raw) {
        Ok(value) => Some((value, name)),
        Err(inner) => {
            errors.push(
                ConfigDiagnostic::error(
                    ConfigCode::EnvInvalid,
                    format!("{name}=`{raw}` could not be used: {}", inner.message),
                )
                .note(format!("unset {name} to fall back to Viso.toml"))
                // The inner diagnostic's own notes list the valid values; carrying
                // them forward is what keeps the message actionable.
                .note(inner.notes.join(" ")),
            );
            None
        }
    }
}

fn resolve_profile(
    flags: &Overrides,
    env: &Env,
    errors: &mut Vec<ConfigDiagnostic>,
) -> Sourced<Profile> {
    pick(
        flags.profile,
        env_value(env, ENV_PROFILE, Profile::parse, errors),
        None,
        None,
        Profile::Dev,
    )
}

/// Resolves the target and checks that this build can produce it.
///
/// `[web] default_target` is parsed ([`crate::manifest::Web`]) but deliberately not
/// consulted here: it is the default for the web *commands*, which are deferred, and
/// inventing a rule now — "use it when the target is already a web target" — would
/// be circular and would have to be unpicked when those commands land.
fn resolve_target(
    manifest: &Manifest,
    flags: &Overrides,
    env: &Env,
    errors: &mut Vec<ConfigDiagnostic>,
) -> Sourced<Target> {
    let resolved = pick(
        flags.target,
        env_value(env, ENV_TARGET, Target::parse, errors),
        None,
        manifest.build.default_target.map(|t| (t.value, t.span)),
        Target::Host,
    );
    if let Err(diag) = resolved.value.require_available() {
        // Name the layer that chose it: "target `ios` is not available" is useful,
        // and "...and it came from Viso.toml line 6" is what makes it fixable.
        errors.push(diag.note(format!("selected by: {}", resolved.origin)));
    }
    resolved
}

/// `VISO_*` booleans. Accepts the shell spellings people actually type.
fn parse_bool(raw: &str) -> Result<bool, ConfigDiagnostic> {
    match raw {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ConfigDiagnostic::error(
            ConfigCode::InvalidValue,
            format!("`{raw}` is not a boolean"),
        )
        .note("expected one of: 1, 0, true, false, yes, no, on, off")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    fn project(manifest: &str) -> (Scratch, Project) {
        let s = Scratch::new("config");
        s.write("Viso.toml", manifest);
        let project = Project::discover(s.path()).unwrap();
        (s, project)
    }

    fn resolved(manifest: &str, flags: &Overrides, env: &Env) -> ResolvedConfig {
        let (_s, project) = project(manifest);
        resolve(&project, ArtifactKind::Build, flags, env)
            .unwrap()
            .config
    }

    const MINIMAL: &str = "[package]\nname = \"app\"\n";

    /// With nothing configured, every value is a framework default — and says so.
    /// A `None` origin or an invented `"unknown"` would make `viso config show`
    /// unable to distinguish "Viso chose this" from "you did".
    #[test]
    fn an_empty_project_resolves_entirely_to_framework_defaults() {
        let config = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        assert_eq!(config.target.value, Target::Host);
        assert_eq!(config.profile.value, Profile::Dev);
        assert_eq!(config.opt_level.value, OptLevel::Zero);
        assert!(config.source_maps.value);
        assert!(!config.strip.value);
        assert_eq!(config.dev_runtime(), DevRuntime::Present);
        for entry in config.entries() {
            assert_eq!(entry.origin, Origin::FrameworkDefault, "{}", entry.key);
        }
    }

    /// The whole ladder, with all four namable layers in play at once and each one
    /// winning exactly where section 5 says it should.
    #[test]
    fn all_four_layers_compete_and_the_documented_order_wins() {
        let manifest = "\
[package]
name = \"app\"

[build]
default_target = \"headless\"

[profile.release]
opt_level = 2
source_maps = true
strip = false
";
        let flags = Overrides {
            profile: Some(Profile::Release),
            strip: Some(true),
            ..Overrides::default()
        };
        let env = Env::from_pairs([(ENV_OPT_LEVEL, "1"), (ENV_SOURCE_MAPS, "false")]);
        let config = resolved(manifest, &flags, &env);

        // A flag beats everything below it.
        assert!(config.strip.value);
        assert_eq!(config.strip.origin, Origin::Flag);
        assert_eq!(config.profile.value, Profile::Release);
        assert_eq!(config.profile.origin, Origin::Flag);

        // The environment beats the manifest: the release table says `1` for
        // opt_level and `true` for source_maps, and the environment overrules both.
        assert_eq!(config.opt_level.value, OptLevel::One);
        assert_eq!(config.opt_level.origin, Origin::Env(ENV_OPT_LEVEL));
        assert!(!config.source_maps.value);
        assert_eq!(config.source_maps.origin, Origin::Env(ENV_SOURCE_MAPS));

        // The manifest beats the framework default, and points at its own line.
        assert_eq!(config.target.value, Target::Headless);
        match config.target.origin {
            Origin::ManifestDefault { span } => assert_eq!(span.line, 5),
            other => panic!("expected a manifest origin, got {other:?}"),
        }
    }

    /// The profile table is a distinct layer from the project defaults, and the
    /// origin has to say *which* profile spoke — with two profiles configured, the
    /// wrong answer is indistinguishable from the right one without it.
    #[test]
    fn the_resolved_profile_selects_which_profile_table_applies() {
        let manifest = "\
[package]
name = \"app\"

[profile.dev]
opt_level = 1

[profile.release]
opt_level = 2
";
        let dev = resolved(manifest, &Overrides::default(), &Env::empty());
        assert_eq!(dev.opt_level.value, OptLevel::One);
        assert_eq!(
            dev.opt_level.origin,
            Origin::ManifestProfile {
                profile: Profile::Dev,
                span: Span {
                    line: 5,
                    column: 13
                },
            }
        );

        let release = resolved(
            manifest,
            &Overrides {
                profile: Some(Profile::Release),
                ..Overrides::default()
            },
            &Env::empty(),
        );
        assert_eq!(release.opt_level.value, OptLevel::Two);
        match release.opt_level.origin {
            Origin::ManifestProfile { profile, span } => {
                assert_eq!(profile, Profile::Release);
                assert_eq!(span.line, 8);
            }
            other => panic!("expected the release table, got {other:?}"),
        }

        // A profile with no table falls through to that profile's own defaults,
        // not to another profile's table.
        let shipping = resolved(
            manifest,
            &Overrides {
                profile: Some(Profile::Shipping),
                ..Overrides::default()
            },
            &Env::empty(),
        );
        assert_eq!(shipping.opt_level.value, OptLevel::Size);
        assert_eq!(shipping.opt_level.origin, Origin::FrameworkDefault);
    }

    /// Every origin variant must be reachable, and each one must print a form a
    /// reader can act on. An origin that never occurs is a lie in the type; an
    /// origin that prints without its location is useless in a large manifest.
    #[test]
    fn every_origin_is_reachable_and_prints_its_location() {
        let manifest = "\
[package]
name = \"app\"

[build]
default_target = \"headless\"

[profile.dev]
strip = true
";
        let config = resolved(
            manifest,
            &Overrides {
                opt_level: Some(OptLevel::Three),
                ..Overrides::default()
            },
            &Env::from_pairs([(ENV_SOURCE_MAPS, "off")]),
        );

        let by_key: BTreeMap<&str, Entry> =
            config.entries().into_iter().map(|e| (e.key, e)).collect();
        assert_eq!(by_key["opt_level"].origin.layer(), "flag");
        assert_eq!(by_key["opt_level"].origin.to_string(), "flag");
        assert_eq!(by_key["source_maps"].origin.layer(), "env");
        assert_eq!(
            by_key["source_maps"].origin.to_string(),
            "env VISO_SOURCE_MAPS"
        );
        assert_eq!(by_key["strip"].origin.layer(), "profile");
        assert_eq!(
            by_key["strip"].origin.to_string(),
            "Viso.toml [profile.dev]:8:9"
        );
        assert_eq!(by_key["target"].origin.layer(), "manifest");
        assert_eq!(by_key["target"].origin.to_string(), "Viso.toml:5:18");
        assert_eq!(by_key["profile"].origin.layer(), "default");
        assert_eq!(by_key["profile"].origin.to_string(), "default");

        // Ranks exist only to state the documented order in one place; assert they
        // match the order the resolver actually implements above.
        let ranks: Vec<u8> = ["opt_level", "source_maps", "strip", "target", "profile"]
            .iter()
            .map(|k| by_key[*k].origin.rank())
            .collect();
        assert_eq!(ranks, [0, 1, 2, 3, 4]);
    }

    #[test]
    fn config_show_and_get_agree_and_cover_every_key() {
        let config = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        let keys = config.keys();
        assert_eq!(
            keys,
            [
                "target",
                "profile",
                "opt_level",
                "source_maps",
                "strip",
                "dev_runtime",
                "artifact"
            ]
        );
        for key in keys {
            assert_eq!(config.get(key).unwrap().key, key);
        }
        assert!(config.get("nope").is_none());
        assert_eq!(
            config.get("target").unwrap().to_string(),
            "target = host (default)"
        );
    }

    /// A bad `VISO_*` value is an error, not a skipped layer: the user set it
    /// deliberately, so silently building with the manifest's value would produce
    /// something they did not ask for.
    #[test]
    fn an_unparseable_environment_variable_is_an_error_that_names_the_variable() {
        let (_s, project) = project("[package]\nname = \"app\"\n");
        let env = Env::from_pairs([(ENV_TARGET, "playstation")]);
        let diags =
            resolve(&project, ArtifactKind::Build, &Overrides::default(), &env).unwrap_err();
        let diag = diags
            .iter()
            .find(|d| d.code == ConfigCode::EnvInvalid)
            .expect("expected an env diagnostic");
        assert!(diag.message.contains("VISO_TARGET"));
        assert!(diag.message.contains("playstation"));
        assert!(diag.notes.iter().any(|n| n.contains("unset VISO_TARGET")));
        // The valid values come forward from the inner parse error.
        assert!(diag.notes.iter().any(|n| n.contains("headless")));

        for var in [ENV_PROFILE, ENV_OPT_LEVEL, ENV_SOURCE_MAPS, ENV_STRIP] {
            let env = Env::from_pairs([(var, "nonsense")]);
            let diags =
                resolve(&project, ArtifactKind::Build, &Overrides::default(), &env).unwrap_err();
            assert!(
                diags.iter().any(|d| d.code == ConfigCode::EnvInvalid),
                "{var} was accepted"
            );
        }
    }

    #[test]
    fn an_empty_environment_variable_counts_as_unset() {
        let env = Env::from_pairs([(ENV_TARGET, ""), (ENV_PROFILE, "   ")]);
        let config = resolved(MINIMAL, &Overrides::default(), &env);
        assert_eq!(config.target.origin, Origin::FrameworkDefault);
        assert_eq!(config.profile.origin, Origin::FrameworkDefault);
    }

    #[test]
    fn boolean_environment_spellings_are_accepted() {
        for raw in ["1", "true", "yes", "on"] {
            assert!(parse_bool(raw).unwrap(), "{raw}");
        }
        for raw in ["0", "false", "no", "off"] {
            assert!(!parse_bool(raw).unwrap(), "{raw}");
        }
        assert!(parse_bool("maybe").is_err());
    }

    /// An unavailable target fails with the environment exit code *and* names the
    /// layer that chose it — without that, a `Viso.toml` default and a stale
    /// environment variable produce the same unhelpful message.
    #[test]
    fn an_unavailable_target_says_which_layer_selected_it() {
        let (_s, project) =
            project("[package]\nname = \"app\"\n\n[build]\ndefault_target = \"ios\"\n");
        let diags = resolve(
            &project,
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap_err();
        let diag = diags
            .iter()
            .find(|d| d.code == ConfigCode::TargetUnavailable)
            .expect("expected a target diagnostic");
        assert_eq!(diag.code.exit_code(), 3);
        assert!(
            diag.notes.iter().any(|n| n.contains("Viso.toml:5")),
            "{:?}",
            diag.notes
        );

        // The same target from a flag names the flag instead.
        let diags = resolve(
            &project,
            ArtifactKind::Build,
            &Overrides {
                target: Some(Target::WebGpu),
                ..Overrides::default()
            },
            &Env::empty(),
        )
        .unwrap_err();
        assert!(
            diags
                .iter()
                .any(|d| d.notes.iter().any(|n| n == "selected by: flag")),
            "{diags:?}"
        );
    }

    /// Manifest warnings survive resolution. Dropping them here would mean a typo'd
    /// key is reported by `viso check` and silently ignored by `viso build`.
    #[test]
    fn manifest_warnings_are_carried_through_resolution() {
        let (_s, project) = project("[package]\nname = \"app\"\nnaem = \"typo\"\n");
        let resolved = resolve(
            &project,
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap();
        assert_eq!(resolved.warnings.len(), 1);
        assert_eq!(resolved.warnings[0].code, ConfigCode::UnknownKey);
    }

    /// The configuration hash covers values and ignores origins. Two builds that
    /// agree on every value are the same build, however each value was named — so
    /// they must share a `BuildId` and reuse each other's cache.
    #[test]
    fn the_config_hash_tracks_values_not_provenance() {
        let from_flag = resolved(
            MINIMAL,
            &Overrides {
                opt_level: Some(OptLevel::Two),
                ..Overrides::default()
            },
            &Env::empty(),
        );
        let from_env = resolved(
            MINIMAL,
            &Overrides::default(),
            &Env::from_pairs([(ENV_OPT_LEVEL, "2")]),
        );
        let from_manifest = resolved(
            "[package]\nname = \"app\"\n\n[profile.dev]\nopt_level = 2\n",
            &Overrides::default(),
            &Env::empty(),
        );

        assert_ne!(from_flag.opt_level.origin, from_env.opt_level.origin);
        assert_eq!(from_flag.hash(), from_env.hash());
        assert_eq!(from_flag.hash(), from_manifest.hash());

        // A different value is a different hash.
        let other = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        assert_ne!(from_flag.hash(), other.hash());
    }

    #[test]
    fn every_config_field_moves_the_hash() {
        let base = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        let variants = [
            Overrides {
                target: Some(Target::Headless),
                ..Overrides::default()
            },
            Overrides {
                profile: Some(Profile::Release),
                ..Overrides::default()
            },
            Overrides {
                opt_level: Some(OptLevel::Three),
                ..Overrides::default()
            },
            Overrides {
                source_maps: Some(false),
                ..Overrides::default()
            },
            Overrides {
                strip: Some(true),
                ..Overrides::default()
            },
        ];
        for flags in variants {
            let variant = resolved(MINIMAL, &flags, &Env::empty());
            assert_ne!(base.hash(), variant.hash(), "{flags:?}");
        }

        // The artifact kind is part of the identity too: a package is not a build.
        let (_s, project) = project(MINIMAL);
        let package = resolve(
            &project,
            ArtifactKind::Package,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap()
        .config;
        assert_ne!(base.hash(), package.hash());
    }

    /// Section 38.2 again, from the resolved side: no combination of flags,
    /// environment and manifest can put a Dev Runtime in a release build, because
    /// the profile is the only input.
    #[test]
    fn no_layer_can_give_a_release_build_a_dev_runtime() {
        let manifest = "[package]\nname = \"app\"\n\n[profile.release]\nsource_maps = true\n";
        for env in [Env::empty(), Env::from_pairs([(ENV_SOURCE_MAPS, "1")])] {
            let config = resolved(
                manifest,
                &Overrides {
                    profile: Some(Profile::Release),
                    source_maps: Some(true),
                    ..Overrides::default()
                },
                &env,
            );
            assert_eq!(config.dev_runtime(), DevRuntime::Absent);
        }
    }

    #[test]
    fn the_build_id_follows_the_resolved_configuration() {
        let fingerprint = ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]);
        let toolchain = Toolchain::new("rustc 1.98.1", Toolchain::current_host());

        let dev = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        let release = resolved(
            MINIMAL,
            &Overrides {
                profile: Some(Profile::Release),
                ..Overrides::default()
            },
            &Env::empty(),
        );
        assert_ne!(
            dev.build_id(fingerprint, &toolchain),
            release.build_id(fingerprint, &toolchain)
        );
        assert_eq!(
            dev.build_id(fingerprint, &toolchain),
            dev.build_id(fingerprint, &toolchain)
        );
    }

    #[test]
    fn the_process_environment_capture_is_limited_to_the_viso_prefix() {
        let env = Env::from_process();
        // Whatever the ambient environment holds, nothing without the prefix can
        // reach a resolved value.
        for name in ENV_VARS {
            assert!(name.starts_with("VISO_"));
        }
        assert!(env.get("PATH").is_none());
    }
}
