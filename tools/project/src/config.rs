//! Configuration precedence and provenance.
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
use crate::manifest::{DeviceTarget, Manifest, Spanned, nearest};
use crate::target::{ArtifactKind, Auto, DevRuntime, OptLevel, Profile, Target, WebOptimize};

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
    /// The human form: `flag`, `env VISO_PROFILE`, `Viso.toml [profile.dev]:5:13`,
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
    /// `--profile`.
    pub profile: Option<Profile>,
}

/// The `VISO_*` environment, captured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Env {
    vars: BTreeMap<String, String>,
}

/// `VISO_PROFILE`.
pub const ENV_PROFILE: &str = "VISO_PROFILE";
/// `VISO_OPT_LEVEL`.
pub const ENV_OPT_LEVEL: &str = "VISO_OPT_LEVEL";
/// `VISO_SOURCE_MAPS`.
pub const ENV_SOURCE_MAPS: &str = "VISO_SOURCE_MAPS";
/// `VISO_STRIP`.
pub const ENV_STRIP: &str = "VISO_STRIP";

/// Every variable this crate reads, in resolution order.
pub const ENV_VARS: &[&str] = &[ENV_PROFILE, ENV_OPT_LEVEL, ENV_SOURCE_MAPS, ENV_STRIP];

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
    /// An empty `VISO_PROFILE=` is treated as unset: in a shell, that is how a
    /// variable gets cleared, and reading it as a profile named `""` would turn a
    /// cleared variable into an error.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(|v| v.trim())
            .filter(|v| !v.is_empty())
    }
}

/// How one profile builds: the values a build with it gets, each with the
/// layer it won from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileSettings {
    /// Optimization level.
    pub opt_level: Sourced<OptLevel>,
    /// Whether source maps are emitted.
    pub source_maps: Sourced<bool>,
    /// Whether symbols are stripped.
    pub strip: Sourced<bool>,
    /// How a web build is optimized.
    pub web: WebSettings,
}

/// How one profile builds for a browser: `[profile.X.web]` over the
/// profile's own defaults, with `strip` and `source_maps` following the
/// profile's unless the table names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSettings {
    /// How the module is optimized after linking.
    pub optimize: Sourced<WebOptimize>,
    /// Whether the name and producers sections and the debug info are removed.
    pub strip: Sourced<bool>,
    /// Whether the module keeps its debug info.
    pub source_maps: Sourced<bool>,
    /// Whether a `.br` is written beside each file.
    pub brotli: Sourced<bool>,
    /// Whether a `.gz` is written beside each file.
    pub gzip: Sourced<bool>,
    /// Whether the module is split into a startup and a lazy part.
    pub split: Sourced<Auto>,
    /// Whether the module is built shared-memory and threaded.
    pub threads: Sourced<Auto>,
}

impl WebSettings {
    fn resolve(
        manifest: &Manifest,
        profile: Profile,
        strip: Sourced<bool>,
        source_maps: Sourced<bool>,
    ) -> Self {
        let table = &manifest.profiles.get(profile).web;
        let precompress = profile.default_web_precompress();
        Self {
            optimize: pick(
                None,
                None,
                from_profile(profile, table.optimize),
                None,
                profile.default_web_optimize(),
            ),
            strip: inherit(profile, table.strip, strip),
            source_maps: inherit(profile, table.source_maps, source_maps),
            brotli: pick(
                None,
                None,
                from_profile(profile, table.brotli),
                None,
                precompress,
            ),
            gzip: pick(
                None,
                None,
                from_profile(profile, table.gzip),
                None,
                precompress,
            ),
            split: pick(
                None,
                None,
                from_profile(profile, table.split),
                None,
                Auto::Auto,
            ),
            threads: pick(
                None,
                None,
                from_profile(profile, table.threads),
                None,
                Auto::Auto,
            ),
        }
    }

    fn hash(&self, hasher: &mut Hasher) {
        hasher.str(self.optimize.value.as_str());
        hasher.u64(u64::from(self.strip.value));
        hasher.u64(u64::from(self.source_maps.value));
        hasher.u64(u64::from(self.brotli.value));
        hasher.u64(u64::from(self.gzip.value));
        hasher.str(self.split.value.as_str());
        hasher.str(self.threads.value.as_str());
    }
}

/// A web key that follows the profile's: a flag or the environment still
/// wins, then the web table, then whatever the profile resolved to.
fn inherit(profile: Profile, web: Option<Spanned<bool>>, base: Sourced<bool>) -> Sourced<bool> {
    match (base.origin, web) {
        (Origin::Flag | Origin::Env(_), _) | (_, None) => base,
        (_, Some(web)) => Sourced::new(
            web.value,
            Origin::ManifestProfile {
                profile,
                span: web.span,
            },
        ),
    }
}

impl ProfileSettings {
    /// The settings of `profile`: the environment over its `[profile.X]` table
    /// over its own defaults. An unparseable variable is recorded in `errors`.
    pub fn resolve(
        manifest: &Manifest,
        profile: Profile,
        env: &Env,
        errors: &mut Vec<ConfigDiagnostic>,
    ) -> Self {
        let table = manifest.profiles.get(profile);
        let source_maps = pick(
            None,
            env_value(env, ENV_SOURCE_MAPS, parse_bool, errors),
            from_profile(profile, table.source_maps),
            None,
            profile.default_source_maps(),
        );
        let strip = pick(
            None,
            env_value(env, ENV_STRIP, parse_bool, errors),
            from_profile(profile, table.strip),
            None,
            profile.default_strip(),
        );
        Self {
            opt_level: pick(
                None,
                env_value(env, ENV_OPT_LEVEL, OptLevel::parse, errors),
                from_profile(profile, table.opt_level),
                None,
                profile.default_opt_level(),
            ),
            source_maps,
            strip,
            web: WebSettings::resolve(manifest, profile, strip, source_maps),
        }
    }
}

/// A fully resolved build configuration.
///
/// Total: every field has a value and an origin, so no consumer has to re-apply a
/// default and risk applying a different one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    /// Where the output runs: what the command line named, the host when it
    /// named nothing. No configuration layer chooses it, so it carries no
    /// origin.
    pub target: Target,
    /// The `[target.<name>]` table of a [`Target::Device`]; `None` for every
    /// other target.
    pub device: Option<DeviceTarget>,
    /// How it is compiled.
    pub profile: Sourced<Profile>,
    /// What the profile builds with.
    pub settings: ProfileSettings,
    /// What shape the output has — chosen by the command, not configurable.
    pub artifact: ArtifactKind,
}

impl ResolvedConfig {
    /// Whether the Dev Runtime is compiled in.
    ///
    /// Derived from the profile and carrying no [`Origin`], because there is no
    /// layer that could have named it: the profile fixes it, and a provenance
    /// field would imply it was negotiable.
    pub const fn dev_runtime(&self) -> DevRuntime {
        self.profile.value.dev_runtime()
    }

    /// A hash over the resolved *values*, one of the [`BuildId`] inputs.
    ///
    /// Origins are excluded on purpose. Two builds that resolved the same values by
    /// different routes — one from the environment, one from the manifest —
    /// produce byte-identical output, so they must share a [`BuildId`] and reuse
    /// each other's cache.
    pub fn hash(&self) -> Hash128 {
        let mut hasher = Hasher::new();
        hasher.str(self.target.as_str());
        hasher.str(self.profile.value.as_str());
        hasher.str(self.settings.opt_level.value.as_str());
        hasher.u64(u64::from(self.settings.source_maps.value));
        hasher.u64(u64::from(self.settings.strip.value));
        // Only a browser build reads the web settings.
        if self.target.is_web() {
            self.settings.web.hash(&mut hasher);
        }
        // What a device target's build is cut to; where the board is reached
        // is not the build's.
        if let Some(device) = &self.device {
            hasher.str(device.profile.value.as_str());
            hasher.str(device.triple.as_ref().map_or("", |t| t.value.as_str()));
            hasher.str(device.gpu_api().map_or("", |gpu| gpu.as_str()));
            for (name, value) in device.build_env() {
                hasher.str(name);
                hasher.str(&value);
            }
            // The locales decide which Unicode data the build links.
            hasher.u64(device.locales.len() as u64);
            for locale in &device.locales {
                hasher.str(&locale.value);
            }
        }
        hasher.str(self.artifact.as_str());
        hasher.finish()
    }

    /// The [`BuildId`] for this configuration.
    pub fn build_id(&self, fingerprint: ProjectFingerprint, toolchain: &Toolchain) -> BuildId {
        BuildId::compute(
            fingerprint,
            self.target,
            self.profile.value,
            toolchain,
            self.hash(),
        )
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

/// Resolves the configuration of a build of a located project for `target`.
///
/// Fails only on a value that cannot be honored: an unparseable environment
/// variable, or a target this build cannot produce. Everything else resolves,
/// possibly with warnings.
pub fn resolve(
    project: &Project,
    target: Target,
    artifact: ArtifactKind,
    flags: &Overrides,
    env: &Env,
) -> Result<Resolved, Vec<ConfigDiagnostic>> {
    let mut warnings = project.warnings.clone();
    let mut errors = Vec::new();

    let profile = resolve_profile(flags, env, &mut errors);
    let settings = ProfileSettings::resolve(&project.manifest, profile.value, env, &mut errors);
    if let Err(diag) = target.require_available() {
        errors.push(diag);
    }
    let device = match target {
        Target::Device(name) => match project.manifest.targets.device(name) {
            Some(device) => Some(device.clone()),
            None => {
                errors.push(undeclared_device(&project.manifest, name));
                None
            }
        },
        _ => None,
    };

    if !errors.is_empty() {
        warnings.extend(errors);
        return Err(warnings);
    }

    Ok(Resolved {
        config: ResolvedConfig {
            target,
            device,
            profile,
            settings,
            artifact,
        },
        warnings,
    })
}

/// The diagnostic for a target name that is neither built in nor a device
/// target `manifest` declares: the built-in names, the declared device
/// targets, and the nearest of them all.
fn undeclared_device(manifest: &Manifest, name: &str) -> ConfigDiagnostic {
    let devices: Vec<&str> = manifest
        .targets
        .devices
        .iter()
        .map(|d| d.name.value.as_str())
        .collect();
    let mut diag = ConfigDiagnostic::error(
        ConfigCode::InvalidValue,
        format!("`{name}` is not a target"),
    )
    .at(&manifest.path)
    .note(format!("built-in targets: {}", Target::names().join(", ")))
    .note(if devices.is_empty() {
        "Viso.toml declares no device target; a `[target.<name>]` table with a `profile` \
         declares one"
            .to_owned()
    } else {
        format!("device targets in Viso.toml: {}", devices.join(", "))
    });
    let known: Vec<&str> = Target::names().into_iter().chain(devices).collect();
    if let Some(suggestion) = nearest(name, &known) {
        diag = diag.note(format!("did you mean `{suggestion}`?"));
    }
    diag
}

/// The precedence rule itself, once, for every key.
///
/// Writing it as one function rather than a chain per field is what keeps the five
/// layers in one order everywhere: a hand-rolled `if let` chain per key is how one
/// key ends up letting the manifest beat the environment.
pub(crate) fn pick<T>(
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
        resolve(&project, Target::Host, ArtifactKind::Build, flags, env)
            .unwrap()
            .config
    }

    fn release() -> Overrides {
        Overrides {
            profile: Some(Profile::Release),
        }
    }

    const MINIMAL: &str = "[package]\nname = \"app\"\n";

    /// With nothing configured, every value is a framework default — and says so.
    /// A `None` origin or an invented `"unknown"` would make `viso config show`
    /// unable to distinguish "Viso chose this" from "you did".
    #[test]
    fn an_empty_project_resolves_entirely_to_framework_defaults() {
        let config = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        assert_eq!(config.target, Target::Host);
        assert_eq!(
            config.profile,
            Sourced::new(Profile::Dev, Origin::FrameworkDefault)
        );
        let settings = config.settings;
        assert_eq!(
            settings.opt_level,
            Sourced::new(OptLevel::Zero, Origin::FrameworkDefault)
        );
        assert_eq!(
            settings.source_maps,
            Sourced::new(true, Origin::FrameworkDefault)
        );
        assert_eq!(
            settings.strip,
            Sourced::new(false, Origin::FrameworkDefault)
        );
        assert_eq!(config.dev_runtime(), DevRuntime::Present);
    }

    /// The whole ladder, with every namable layer in play at once and each one
    /// winning exactly where the documented order says it should.
    #[test]
    fn every_layer_competes_and_the_documented_order_wins() {
        let manifest = "\
[package]
name = \"app\"

[profile.release]
opt_level = 2
source_maps = true
strip = false
";
        let env = Env::from_pairs([
            (ENV_PROFILE, "shipping"),
            (ENV_OPT_LEVEL, "1"),
            (ENV_SOURCE_MAPS, "false"),
        ]);
        let config = resolved(manifest, &release(), &env);

        // A flag beats the environment.
        assert_eq!(config.profile, Sourced::new(Profile::Release, Origin::Flag));

        // The environment beats the profile table: the release table says `2` for
        // opt_level and `true` for source_maps, and the environment overrules both.
        let settings = config.settings;
        assert_eq!(
            settings.opt_level,
            Sourced::new(OptLevel::One, Origin::Env(ENV_OPT_LEVEL))
        );
        assert_eq!(
            settings.source_maps,
            Sourced::new(false, Origin::Env(ENV_SOURCE_MAPS))
        );

        // The profile table beats the default, and points at its own line.
        assert!(!settings.strip.value);
        match settings.strip.origin {
            Origin::ManifestProfile { profile, span } => {
                assert_eq!((profile, span.line), (Profile::Release, 7));
            }
            other => panic!("expected the release table, got {other:?}"),
        }

        // Without the flag the environment names the profile.
        let config = resolved(manifest, &Overrides::default(), &env);
        assert_eq!(
            config.profile,
            Sourced::new(Profile::Shipping, Origin::Env(ENV_PROFILE))
        );
    }

    /// The profile table is chosen by the resolved profile, and the origin has to
    /// say *which* profile spoke — with two profiles configured, the wrong answer
    /// is indistinguishable from the right one without it.
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
        let dev = resolved(manifest, &Overrides::default(), &Env::empty()).settings;
        assert_eq!(dev.opt_level.value, OptLevel::One);
        let Origin::ManifestProfile { profile, span } = dev.opt_level.origin else {
            panic!("expected a profile origin, got {:?}", dev.opt_level.origin);
        };
        assert_eq!((profile, span.line, span.column), (Profile::Dev, 5, 13));

        let release = resolved(manifest, &release(), &Env::empty()).settings;
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
            },
            &Env::empty(),
        )
        .settings;
        assert_eq!(
            shipping.opt_level,
            Sourced::new(OptLevel::Size, Origin::FrameworkDefault)
        );
    }

    /// Each origin prints a form a reader can act on: an origin that prints
    /// without its location is useless in a large manifest.
    #[test]
    fn every_origin_prints_its_location() {
        let span = Span {
            start: 0,
            end: 1,
            line: 8,
            column: 9,
        };
        let printed = [
            (Origin::Flag, "flag", "flag"),
            (Origin::Env(ENV_SOURCE_MAPS), "env", "env VISO_SOURCE_MAPS"),
            (
                Origin::ManifestProfile {
                    profile: Profile::Dev,
                    span,
                },
                "profile",
                "Viso.toml [profile.dev]:8:9",
            ),
            (
                Origin::ManifestDefault { span },
                "manifest",
                "Viso.toml:8:9",
            ),
            (Origin::FrameworkDefault, "default", "default"),
        ];
        for (rank, (origin, layer, text)) in printed.into_iter().enumerate() {
            assert_eq!(origin.layer(), layer);
            assert_eq!(origin.to_string(), text);
            // Ranks exist only to state the documented order in one place.
            assert_eq!(usize::from(origin.rank()), rank);
        }
    }

    /// A bad `VISO_*` value is an error, not a skipped layer: the user set it
    /// deliberately, so silently building with the manifest's value would produce
    /// something they did not ask for.
    #[test]
    fn an_unparseable_environment_variable_is_an_error_that_names_the_variable() {
        let (_s, project) = project(MINIMAL);
        let env = Env::from_pairs([(ENV_PROFILE, "fast")]);
        let diags = resolve(
            &project,
            Target::Host,
            ArtifactKind::Build,
            &Overrides::default(),
            &env,
        )
        .unwrap_err();
        let diag = diags
            .iter()
            .find(|d| d.code == ConfigCode::EnvInvalid)
            .expect("expected an env diagnostic");
        assert!(diag.message.contains("VISO_PROFILE"));
        assert!(diag.message.contains("fast"));
        assert!(diag.notes.iter().any(|n| n.contains("unset VISO_PROFILE")));
        // The valid values come forward from the inner parse error.
        assert!(diag.notes.iter().any(|n| n.contains("shipping")));

        for var in [ENV_OPT_LEVEL, ENV_SOURCE_MAPS, ENV_STRIP] {
            let env = Env::from_pairs([(var, "nonsense")]);
            let diags = resolve(
                &project,
                Target::Host,
                ArtifactKind::Build,
                &Overrides::default(),
                &env,
            )
            .unwrap_err();
            assert!(
                diags.iter().any(|d| d.code == ConfigCode::EnvInvalid),
                "{var} was accepted"
            );
        }
    }

    #[test]
    fn an_empty_environment_variable_counts_as_unset() {
        let env = Env::from_pairs([(ENV_PROFILE, "   "), (ENV_STRIP, "")]);
        let config = resolved(MINIMAL, &Overrides::default(), &env);
        assert_eq!(config.profile.origin, Origin::FrameworkDefault);
        assert_eq!(config.settings.strip.origin, Origin::FrameworkDefault);
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

    /// Manifest warnings survive resolution. Dropping them here would mean a
    /// warning is reported by `viso check` and silently ignored by `viso build`.
    #[test]
    fn manifest_warnings_are_carried_through_resolution() {
        let (_s, project) =
            project("[package]\nname = \"app\"\n\n[profile.dev]\nhot_reload = true\n");
        let resolved = resolve(
            &project,
            Target::Host,
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
        assert_ne!(
            from_env.settings.opt_level.origin,
            from_manifest.settings.opt_level.origin
        );
        assert_eq!(from_env.hash(), from_manifest.hash());

        // A different value is a different hash.
        let other = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        assert_ne!(from_env.hash(), other.hash());
    }

    #[test]
    fn every_config_field_moves_the_hash() {
        let base = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        let (_s, project) = project(MINIMAL);
        let with = |target: Target, artifact: ArtifactKind, flags: &Overrides, env: &Env| {
            resolve(&project, target, artifact, flags, env)
                .unwrap()
                .config
        };
        let build = ArtifactKind::Build;
        let none = Overrides::default();
        let variants = [
            with(Target::Headless, build, &none, &Env::empty()),
            with(Target::Host, build, &release(), &Env::empty()),
            with(
                Target::Host,
                build,
                &none,
                &Env::from_pairs([(ENV_OPT_LEVEL, "3")]),
            ),
            with(
                Target::Host,
                build,
                &none,
                &Env::from_pairs([(ENV_SOURCE_MAPS, "0")]),
            ),
            with(
                Target::Host,
                build,
                &none,
                &Env::from_pairs([(ENV_STRIP, "1")]),
            ),
            // The artifact kind is part of the identity too: a package is not a
            // build.
            with(Target::Host, ArtifactKind::Package, &none, &Env::empty()),
        ];
        for variant in variants {
            assert_ne!(base.hash(), variant.hash(), "{variant:?}");
        }
    }

    /// No combination of flags, environment and manifest can put a Dev Runtime in
    /// a release build, because the profile is the only input.
    #[test]
    fn no_layer_can_give_a_release_build_a_dev_runtime() {
        let manifest = "[package]\nname = \"app\"\n\n[profile.release]\nsource_maps = true\n";
        for env in [Env::empty(), Env::from_pairs([(ENV_SOURCE_MAPS, "1")])] {
            let config = resolved(manifest, &release(), &env);
            assert_eq!(config.dev_runtime(), DevRuntime::Absent);
        }
    }

    #[test]
    fn the_build_id_follows_the_resolved_configuration() {
        let fingerprint = ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]);
        let toolchain = Toolchain::new("rustc 1.98.1", Toolchain::current_host());

        let dev = resolved(MINIMAL, &Overrides::default(), &Env::empty());
        let release = resolved(MINIMAL, &release(), &Env::empty());
        assert_ne!(
            dev.build_id(fingerprint, &toolchain),
            release.build_id(fingerprint, &toolchain)
        );
        assert_eq!(
            dev.build_id(fingerprint, &toolchain),
            dev.build_id(fingerprint, &toolchain)
        );
    }

    /// A device target resolves to its own table, and what the table says is
    /// part of the identity: two boards never share a build.
    #[test]
    fn a_device_target_resolves_to_its_table_and_its_own_identity() {
        let manifest = "[package]\nname = \"hmi\"\n\n\
                        [target.panel]\nprofile = \"embedded-gpu\"\nworkers = 1\n\n\
                        [target.dash]\nprofile = \"embedded-gpu\"\nworkers = 1\n\n\
                        [target.slim]\nprofile = \"embedded-gpu\"\nworkers = 2\n";
        let (_s, project) = project(manifest);
        let of = |name: &str| {
            resolve(
                &project,
                Target::device(name),
                ArtifactKind::Build,
                &Overrides::default(),
                &Env::empty(),
            )
            .unwrap()
            .config
        };
        let panel = of("panel");
        assert_eq!(panel.target.as_str(), "panel");
        assert_eq!(
            panel.device.as_ref().unwrap().profile.value,
            crate::target::Footprint::EmbeddedGpu
        );
        let host = resolve(
            &project,
            Target::Host,
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap()
        .config;
        assert!(host.device.is_none());

        let fingerprint = ProjectFingerprint::from_sources([("a.rs", b"x".as_slice())]);
        let toolchain = Toolchain::new("rustc 1.98.1", Toolchain::current_host());
        let ids =
            [panel, of("dash"), of("slim"), host].map(|c| c.build_id(fingerprint, &toolchain));
        for (i, id) in ids.iter().enumerate() {
            assert!(!ids[..i].contains(id), "{i} shares a build id");
        }
        // Two tables alike still hash apart: the name is the target.
        assert_ne!(of("panel").hash(), of("dash").hash());
    }

    #[test]
    fn a_device_target_s_locales_and_baked_sizes_move_its_hash() {
        let hash = |table: &str| {
            let (_s, project) = project(&format!(
                "[package]\nname = \"hmi\"\n\n[target.panel]\nprofile = \"embedded-cpu\"\n{table}"
            ));
            resolve(
                &project,
                Target::device("panel"),
                ArtifactKind::Build,
                &Overrides::default(),
                &Env::empty(),
            )
            .unwrap()
            .config
            .hash()
        };
        let plain = hash("");
        let thai = hash("locales = [\"th\"]\n");
        assert_ne!(plain, thai);
        assert_ne!(thai, hash("locales = [\"th\", \"en\"]\n"));
        let baked = hash("fonts = { bake = [16] }\n");
        assert_ne!(plain, baked);
        assert_ne!(baked, hash("fonts = { bake = [16], rasterizer = false }\n"));
        assert_eq!(plain, hash(""));
    }

    #[test]
    fn an_undeclared_device_target_lists_the_declared_ones() {
        let (_s, declared) =
            project("[package]\nname = \"hmi\"\n\n[target.panel]\nprofile = \"embedded-cpu\"\n");
        let errors = resolve(
            &declared,
            Target::device("pnael"),
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap_err();
        let diag = errors.iter().find(|d| d.is_error()).unwrap();
        assert_eq!(diag.code, ConfigCode::InvalidValue);
        assert_eq!(diag.message, "`pnael` is not a target");
        let notes = diag.notes.join("\n");
        assert!(
            notes.contains("device targets in Viso.toml: panel"),
            "{notes}"
        );
        assert!(notes.contains("did you mean `panel`?"), "{notes}");
        assert!(notes.contains("web-gpu"), "{notes}");

        let (_s, bare) = project(MINIMAL);
        let errors = resolve(
            &bare,
            Target::device("panel"),
            ArtifactKind::Build,
            &Overrides::default(),
            &Env::empty(),
        )
        .unwrap_err();
        assert!(
            errors[0]
                .notes
                .join(" ")
                .contains("declares no device target")
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
