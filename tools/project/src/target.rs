//! Targets, profiles and artifact kinds — the three axes of "what am I building".
//!
//! These are separate types on purpose. A target says
//! *where* the output runs, a profile says *how* it was compiled, and an artifact
//! kind says *what shape* the output has. The type system must not let one be
//! mistaken for another, because the three combine into different directories,
//! different dev-runtime decisions, and different [`crate::BuildId`]s.
//!
//! Two of the three are closed enumerations with no `Other(String)` escape. A
//! target this build cannot produce is a diagnostic
//! ([`crate::ConfigCode::TargetUnavailable`]), never a string that flows onward
//! and fails somewhere deeper as a path that does not exist.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use crate::diag::{ConfigCode, ConfigDiagnostic};

/// Where the built application runs.
///
/// [`Target::Host`] is resolved from the current OS and is never a name the user
/// types: `viso run` on macOS means macOS, and spelling
/// it out would create two ways to say one thing. [`Target::Headless`] is the
/// deterministic test target.
///
/// The mobile and web variants exist in the enum now, before their commands do, so
/// a `Viso.toml` written for them parses cleanly and `--target ios` produces
/// "not in this build" instead of "unknown value". A user's configuration should
/// not have to be deleted and rewritten when a phase lands.
///
/// [`Target::Device`] is the one variant whose name is the project's: a board a
/// `[target.<name>]` table describes. Its configuration stays in the manifest
/// and resolves into [`crate::ResolvedConfig::device`]; the variant carries only
/// the name, interned, so a target stays a `Copy` value that keys directories,
/// locks and build identities like every other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Target {
    /// The machine running the command, whichever OS that is.
    Host,
    /// Deterministic, no window, no adapter — the test and CI target.
    Headless,
    /// iOS device or simulator.
    Ios,
    /// Android device or emulator.
    Android,
    /// Browser, WebGPU canvas.
    WebGpu,
    /// Browser, DOM output.
    WebDom,
    /// Browser, WebGPU with a DOM overlay.
    WebHybrid,
    /// A device target `Viso.toml` declares as `[target.<name>]`, by its name.
    Device(&'static str),
}

impl Target {
    /// The name accepted on the command line and in `Viso.toml`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Target::Host => "host",
            Target::Headless => "headless",
            Target::Ios => "ios",
            Target::Android => "android",
            Target::WebGpu => "web-gpu",
            Target::WebDom => "web-dom",
            Target::WebHybrid => "web-hybrid",
            Target::Device(name) => name,
        }
    }

    /// The device target named `name`.
    ///
    /// The name is interned: each distinct name is allocated once for the
    /// life of the process, which bounds what interning keeps to the device
    /// targets the process has seen. Whether the manifest declares it is
    /// [`crate::resolve`]'s question, not this one's.
    pub fn device(name: &str) -> Target {
        static NAMES: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());
        let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
        let interned = match names.get(name) {
            Some(interned) => *interned,
            None => {
                let interned: &'static str = Box::leak(name.into());
                names.insert(interned);
                interned
            }
        };
        Target::Device(interned)
    }

    /// Parses a built-in target name, or produces the diagnostic for an unknown one.
    ///
    /// An unknown name is [`ConfigCode::InvalidValue`] — the key was right and the
    /// type was right, only the value is not one of the seven. The note lists all
    /// of them, so the fix needs no documentation lookup.
    pub fn parse(name: &str) -> Result<Self, ConfigDiagnostic> {
        Self::ALL
            .iter()
            .copied()
            .find(|t| t.as_str() == name)
            .ok_or_else(|| {
                ConfigDiagnostic::error(
                    ConfigCode::InvalidValue,
                    format!("`{name}` is not a target"),
                )
                .note(format!("expected one of: {}", Self::names().join(", ")))
            })
    }

    /// Whether this build can produce the target.
    ///
    /// The host, headless, the iOS Simulator, the Android Emulator, the
    /// WebGPU and DOM browser targets and every device target are; the
    /// hybrid one is not yet. This is a
    /// property of the *build*, not of the machine: it is what separates "you
    /// asked for something that does not exist yet" from "you asked for
    /// something your machine cannot do".
    pub const fn available(self) -> bool {
        matches!(
            self,
            Target::Host
                | Target::Headless
                | Target::Ios
                | Target::Android
                | Target::WebGpu
                | Target::WebDom
                | Target::Device(_)
        )
    }

    /// Whether the target runs in a browser.
    pub const fn is_web(self) -> bool {
        matches!(self, Target::WebGpu | Target::WebDom | Target::WebHybrid)
    }

    /// The diagnostic for asking for an unavailable target.
    ///
    /// Exit code 3 (environment), not 1 — nothing in the user's project is wrong.
    pub fn unavailable(self) -> ConfigDiagnostic {
        ConfigDiagnostic::error(
            ConfigCode::TargetUnavailable,
            format!("target `{}` is not available in this build", self.as_str()),
        )
        .note(format!(
            "available targets: {}",
            Self::ALL
                .iter()
                .filter(|t| t.available())
                .map(|t| t.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Checks availability, returning the target unchanged when it is available.
    pub fn require_available(self) -> Result<Self, ConfigDiagnostic> {
        if self.available() {
            Ok(self)
        } else {
            Err(self.unavailable())
        }
    }

    /// Every built-in target, in declaration order.
    pub const ALL: &'static [Target] = &[
        Target::Host,
        Target::Headless,
        Target::Ios,
        Target::Android,
        Target::WebGpu,
        Target::WebDom,
        Target::WebHybrid,
    ];

    /// Every built-in target name, for diagnostics and shell completion.
    pub fn names() -> Vec<&'static str> {
        Self::ALL.iter().map(|t| t.as_str()).collect()
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The device class a build is made for: a fixed set of the facade's features,
/// and the defaults of the budget and worker count the app runs with.
///
/// A device target names one (`profile` in `[target.<name>]`); the user picks
/// the profile and never the features it stands for, so the feature
/// combinations a build can ask for are these five.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Footprint {
    /// macOS, Windows and Linux machines with a native GPU backend.
    Desktop,
    /// Phones and tablets.
    Mobile,
    /// Browsers.
    Web,
    /// Linux boards drawing through GLES 3, on a display driven through
    /// DRM/KMS.
    EmbeddedGpu,
    /// Linux boards drawn by the software raster, on a display driven through
    /// DRM/KMS.
    EmbeddedCpu,
}

impl Footprint {
    /// The name `Viso.toml` and `VISO_FOOTPRINT` give it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Footprint::Desktop => "desktop",
            Footprint::Mobile => "mobile",
            Footprint::Web => "web",
            Footprint::EmbeddedGpu => "embedded-gpu",
            Footprint::EmbeddedCpu => "embedded-cpu",
        }
    }

    /// Parses a profile name, or produces the diagnostic for an unknown one.
    pub fn parse(name: &str) -> Result<Self, ConfigDiagnostic> {
        Self::ALL
            .iter()
            .copied()
            .find(|f| f.as_str() == name)
            .ok_or_else(|| {
                ConfigDiagnostic::error(
                    ConfigCode::InvalidValue,
                    format!("`{name}` is not a footprint profile"),
                )
                .note(format!(
                    "expected one of: {}",
                    Self::ALL
                        .iter()
                        .map(|f| f.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            })
    }

    /// Whether the profile is one of the embedded Linux classes, whose display
    /// is driven through DRM/KMS.
    pub const fn is_embedded(self) -> bool {
        matches!(self, Footprint::EmbeddedGpu | Footprint::EmbeddedCpu)
    }

    /// The facade features a build of the profile turns on. The desktop,
    /// mobile and web profiles take their backend from the Rust target and
    /// turn on none.
    pub const fn features(self) -> &'static [&'static str] {
        match self {
            Footprint::Desktop | Footprint::Mobile | Footprint::Web => &[],
            Footprint::EmbeddedGpu => &["viso/gles", "viso/kms"],
            Footprint::EmbeddedCpu => &["viso/cpu", "viso/kms"],
        }
    }

    /// Whether a build of the profile keeps the facade's default features
    /// (`vm` and `desktop`, the Linux desktop stack). An embedded build turns
    /// them off and has [`Footprint::features`] alone, with `vm` again for a
    /// debug build.
    pub const fn keeps_default_features(self) -> bool {
        !self.is_embedded()
    }

    /// The languages a package of the profile carries its views' shaders in,
    /// or `None` where the target's backend decides: the desktop and mobile
    /// packages carry their backend's language beside the reference
    /// interpreter's [`ShaderLanguage::None`].
    pub const fn shader_langs(self) -> Option<&'static [ShaderLanguage]> {
        match self {
            Footprint::Desktop | Footprint::Mobile => None,
            Footprint::Web => Some(&[ShaderLanguage::Wgsl, ShaderLanguage::None]),
            Footprint::EmbeddedGpu => Some(&[ShaderLanguage::Glsl, ShaderLanguage::SpirV]),
            Footprint::EmbeddedCpu => Some(&[ShaderLanguage::None]),
        }
    }

    /// Every profile, in declaration order.
    pub const ALL: &'static [Footprint] = &[
        Footprint::Desktop,
        Footprint::Mobile,
        Footprint::Web,
        Footprint::EmbeddedGpu,
        Footprint::EmbeddedCpu,
    ];
}

impl fmt::Display for Footprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A shading language a package carries a brush program's code in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShaderLanguage {
    /// No program text: the register code the reference interpreter runs, which
    /// the software raster draws with.
    None,
    /// WGSL (WebGPU).
    Wgsl,
    /// GLSL ES 3.00 (OpenGL ES 3).
    Glsl,
    /// SPIR-V (Vulkan).
    SpirV,
}

/// The operating system [`Target::Host`] resolves to.
///
/// Separate from [`Target`] because `Host` is one target whose meaning depends on
/// the machine: the cache directory and the toolchain string both need to know
/// which machine produced an artifact, while the *target* the user selected stays
/// `host` everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostOs {
    /// macOS, Metal.
    MacOs,
    /// Windows, D3D12.
    Windows,
    /// Linux, Vulkan.
    Linux,
}

impl HostOs {
    /// The OS this binary was compiled for, or `None` on an OS Viso has no
    /// Tier-1 path for.
    pub const fn current() -> Option<Self> {
        #[cfg(target_os = "macos")]
        {
            Some(HostOs::MacOs)
        }
        #[cfg(target_os = "windows")]
        {
            Some(HostOs::Windows)
        }
        #[cfg(target_os = "linux")]
        {
            Some(HostOs::Linux)
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
        {
            None
        }
    }

    /// The short name used in cache paths and the toolchain string.
    pub const fn as_str(self) -> &'static str {
        match self {
            HostOs::MacOs => "macos",
            HostOs::Windows => "windows",
            HostOs::Linux => "linux",
        }
    }
}

impl fmt::Display for HostOs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the output was compiled.
///
/// Three profiles, not an open set: each one fixes a dev-runtime decision
/// ([`Profile::dev_runtime`]) that must not be configurable, so a fourth profile
/// would have to answer that question too and there is no fourth answer worth
/// having.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Profile {
    /// Fast rebuild, full diagnostics, Dev Runtime present.
    Dev,
    /// Optimized, Dev Runtime absent.
    Release,
    /// Optimized and stripped for store submission, Dev Runtime absent.
    Shipping,
}

impl Profile {
    /// The name accepted on the command line and as a `[profile.X]` table name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Profile::Dev => "dev",
            Profile::Release => "release",
            Profile::Shipping => "shipping",
        }
    }

    /// Parses a profile name, or produces the diagnostic for an unknown one.
    pub fn parse(name: &str) -> Result<Self, ConfigDiagnostic> {
        Self::ALL
            .iter()
            .copied()
            .find(|p| p.as_str() == name)
            .ok_or_else(|| {
                ConfigDiagnostic::error(
                    ConfigCode::UnknownProfile,
                    format!("`{name}` is not a profile"),
                )
                .note("expected one of: dev, release, shipping")
            })
    }

    /// Whether the Dev Runtime is compiled in.
    ///
    /// Fixed by the profile and *not* configurable. A release build that could be patched at runtime is a release
    /// build that carries the whole dev surface — the watcher, the connection, the
    /// staging buffers — into a shipped binary. Making this a function rather than
    /// a config key is what makes that impossible to ask for.
    pub const fn dev_runtime(self) -> DevRuntime {
        match self {
            Profile::Dev => DevRuntime::Present,
            Profile::Release | Profile::Shipping => DevRuntime::Absent,
        }
    }

    /// The default optimization level for the profile, overridable per
    /// `[profile.X] opt_level`.
    pub const fn default_opt_level(self) -> OptLevel {
        match self {
            Profile::Dev => OptLevel::Zero,
            Profile::Release => OptLevel::Three,
            Profile::Shipping => OptLevel::Size,
        }
    }

    /// Whether the profile strips symbols by default.
    pub const fn default_strip(self) -> bool {
        matches!(self, Profile::Shipping)
    }

    /// Whether the profile emits source maps by default.
    pub const fn default_source_maps(self) -> bool {
        matches!(self, Profile::Dev)
    }

    /// How the profile optimizes a web module by default.
    pub const fn default_web_optimize(self) -> WebOptimize {
        match self {
            Profile::Dev => WebOptimize::None,
            Profile::Release => WebOptimize::Speed,
            Profile::Shipping => WebOptimize::Size,
        }
    }

    /// Whether the profile writes precompressed copies of a web bundle by
    /// default.
    pub const fn default_web_precompress(self) -> bool {
        matches!(self, Profile::Shipping)
    }

    /// Every profile, in increasing-optimization order.
    pub const ALL: &'static [Profile] = &[Profile::Dev, Profile::Release, Profile::Shipping];
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether a build carries the Dev Runtime.
///
/// A two-state enum rather than a `bool` so the two states have names at every
/// call site; `if cfg.dev_runtime == DevRuntime::Present` cannot be misread the way
/// a bare `if cfg.dev` can.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DevRuntime {
    /// Watcher, patch connection and staging are compiled in.
    Present,
    /// None of the dev surface exists in the binary.
    Absent,
}

impl DevRuntime {
    /// The short name used in diagnostics and `viso config show`.
    pub const fn as_str(self) -> &'static str {
        match self {
            DevRuntime::Present => "present",
            DevRuntime::Absent => "absent",
        }
    }
}

impl fmt::Display for DevRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a web build's module is optimized after it is linked (`wasm-opt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebOptimize {
    /// Not at all.
    None,
    /// For speed.
    Speed,
    /// For size.
    Size,
}

impl WebOptimize {
    /// The name `[profile.X.web] optimize` takes.
    pub const fn as_str(self) -> &'static str {
        match self {
            WebOptimize::None => "none",
            WebOptimize::Speed => "speed",
            WebOptimize::Size => "size",
        }
    }

    /// Every value, for parsing and messages.
    pub const ALL: [WebOptimize; 3] = [WebOptimize::None, WebOptimize::Speed, WebOptimize::Size];
}

/// A web build feature the build turns on where the target supports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Auto {
    /// On where supported.
    Auto,
    /// Off.
    Off,
}

impl Auto {
    /// The name the manifest takes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Auto::Auto => "auto",
            Auto::Off => "off",
        }
    }

    /// Every value, for parsing and messages.
    pub const ALL: [Auto; 2] = [Auto::Auto, Auto::Off];
}

/// Optimization level, mapping onto the backend's own scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OptLevel {
    /// No optimization.
    Zero,
    /// Minimal.
    One,
    /// Balanced.
    Two,
    /// Full speed.
    Three,
    /// Optimize for size.
    Size,
}

impl OptLevel {
    /// The name accepted in `[profile.X] opt_level`.
    pub const fn as_str(self) -> &'static str {
        match self {
            OptLevel::Zero => "0",
            OptLevel::One => "1",
            OptLevel::Two => "2",
            OptLevel::Three => "3",
            OptLevel::Size => "s",
        }
    }

    /// Parses an optimization level, or produces the diagnostic for an unknown one.
    ///
    /// `"size"` is accepted alongside the canonical `"s"` because that is how
    /// `Viso.toml` examples write it; rejecting the documented spelling would be a
    /// worse contract than carrying one alias.
    pub fn parse(name: &str) -> Result<Self, ConfigDiagnostic> {
        if name == "size" {
            return Ok(OptLevel::Size);
        }
        Self::ALL
            .iter()
            .copied()
            .find(|o| o.as_str() == name)
            .ok_or_else(|| {
                ConfigDiagnostic::error(
                    ConfigCode::InvalidValue,
                    format!("`{name}` is not an optimization level"),
                )
                .note("expected one of: 0, 1, 2, 3, s")
            })
    }

    /// Every level, in declaration order.
    pub const ALL: &'static [OptLevel] = &[
        OptLevel::Zero,
        OptLevel::One,
        OptLevel::Two,
        OptLevel::Three,
        OptLevel::Size,
    ];
}

impl fmt::Display for OptLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What shape the output has.
///
/// Three different outputs with three different meanings, and the reason they are
/// one enum rather than three booleans: a command takes exactly one of them, and
/// the artifact directory is keyed by it, so a `build` output can never land where
/// a `package` output belongs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ArtifactKind {
    /// A runnable binary plus its compiled assets — what `viso build` produces.
    Build,
    /// A distributable container (`.app`, `.apk`, `.ipa`) — what `viso package`
    /// produces.
    Package,
    /// Source in another form (HTML, a Solid project) — what `viso export`
    /// produces. Not runnable by Viso and not a Viso artifact at all past the
    /// boundary.
    Export,
}

impl ArtifactKind {
    /// The directory name under `target/viso/`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ArtifactKind::Build => "build",
            ArtifactKind::Package => "package",
            ArtifactKind::Export => "export",
        }
    }

    /// Every kind, in declaration order.
    pub const ALL: &'static [ArtifactKind] = &[
        ArtifactKind::Build,
        ArtifactKind::Package,
        ArtifactKind::Export,
    ];
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_target_name_round_trips_and_is_unique() {
        for target in Target::ALL {
            assert_eq!(Target::parse(target.as_str()).unwrap(), *target);
        }
        let mut names = Target::names();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Target::ALL.len(), "a name is used twice");
    }

    /// A device target's name is the one it was declared with, and interning
    /// gives one value per name however often it is asked for.
    #[test]
    fn a_device_target_is_its_name() {
        let panel = Target::device("panel");
        assert_eq!(panel, Target::device(&String::from("panel")));
        assert_ne!(panel, Target::device("kiosk"));
        assert_eq!(panel.as_str(), "panel");
        assert!(panel.available());
        assert!(!panel.is_web());
        assert!(!Target::ALL.contains(&panel));
    }

    #[test]
    fn every_footprint_profile_round_trips_with_its_features() {
        for footprint in Footprint::ALL {
            assert_eq!(Footprint::parse(footprint.as_str()).unwrap(), *footprint);
        }
        let d = Footprint::parse("embedded").unwrap_err();
        assert_eq!(d.code, ConfigCode::InvalidValue);
        assert!(d.notes.join(" ").contains("embedded-cpu"));

        assert_eq!(Footprint::EmbeddedGpu.features(), ["viso/gles", "viso/kms"]);
        assert_eq!(Footprint::EmbeddedCpu.features(), ["viso/cpu", "viso/kms"]);
        assert!(Footprint::Desktop.features().is_empty());
        assert!(Footprint::Desktop.keeps_default_features());
        assert!(!Footprint::EmbeddedGpu.keeps_default_features());
        assert!(!Footprint::EmbeddedCpu.keeps_default_features());
        // The CPU profile carries nothing but the interpreter's code, and the
        // GPU one none of it.
        assert_eq!(
            Footprint::EmbeddedCpu.shader_langs(),
            Some(&[ShaderLanguage::None][..])
        );
        assert!(
            !Footprint::EmbeddedGpu
                .shader_langs()
                .unwrap()
                .contains(&ShaderLanguage::None)
        );
        assert_eq!(Footprint::Desktop.shader_langs(), None);
    }

    /// An unknown target is an invalid *value*, and the note has to list the real
    /// ones — a diagnostic that only says "unknown" makes the user go read docs.
    #[test]
    fn an_unknown_target_names_the_alternatives() {
        let d = Target::parse("ios-simulator").unwrap_err();
        assert_eq!(d.code, ConfigCode::InvalidValue);
        let note = d.notes.join(" ");
        for target in Target::ALL {
            assert!(note.contains(target.as_str()), "{target} missing from note");
        }
    }

    /// The distinction this build has to keep honest: a declared-but-unbuilt target
    /// is exit 3 with a clear message, not a parse failure and not a silent
    /// success.
    #[test]
    fn declared_but_unavailable_targets_fail_cleanly() {
        for target in Target::ALL {
            let parsed = Target::parse(target.as_str()).expect("declared targets parse");
            match parsed.require_available() {
                Ok(t) => assert!(!matches!(t, Target::WebHybrid)),
                Err(d) => {
                    assert_eq!(d.code, ConfigCode::TargetUnavailable);
                    assert_eq!(d.code.exit_code(), 3);
                    assert!(d.message.contains(target.as_str()));
                    // The note must name what the user *can* do, otherwise the
                    // error is a dead end.
                    assert!(d.notes.join(" ").contains("host"));
                }
            }
        }
    }

    /// The profile-to-runtime linkage. There is deliberately no setter
    /// and no config key: this test exists to pin that the mapping is total and
    /// that release never gains a dev runtime.
    #[test]
    fn the_profile_alone_decides_the_dev_runtime() {
        assert_eq!(Profile::Dev.dev_runtime(), DevRuntime::Present);
        assert_eq!(Profile::Release.dev_runtime(), DevRuntime::Absent);
        assert_eq!(Profile::Shipping.dev_runtime(), DevRuntime::Absent);

        for profile in Profile::ALL {
            let present = profile.dev_runtime() == DevRuntime::Present;
            assert_eq!(
                present,
                *profile == Profile::Dev,
                "{profile} must not carry a dev runtime"
            );
            // Source maps track the dev runtime; stripping is the shipping-only
            // extra. Pinned because both are defaults a `[profile]` table can
            // override, and the *default* is the part that must not drift.
            assert_eq!(profile.default_source_maps(), present, "{profile}");
        }
        assert!(Profile::Shipping.default_strip());
        assert!(!Profile::Release.default_strip());
    }

    #[test]
    fn profile_and_opt_level_names_round_trip() {
        for profile in Profile::ALL {
            assert_eq!(Profile::parse(profile.as_str()).unwrap(), *profile);
        }
        assert_eq!(
            Profile::parse("fast").unwrap_err().code,
            ConfigCode::UnknownProfile
        );

        for level in OptLevel::ALL {
            assert_eq!(OptLevel::parse(level.as_str()).unwrap(), *level);
        }
        assert_eq!(OptLevel::parse("size").unwrap(), OptLevel::Size);
        assert_eq!(
            OptLevel::parse("max").unwrap_err().code,
            ConfigCode::InvalidValue
        );
        assert_eq!(Profile::Dev.default_opt_level(), OptLevel::Zero);
        assert_eq!(Profile::Release.default_opt_level(), OptLevel::Three);
        assert_eq!(Profile::Shipping.default_opt_level(), OptLevel::Size);
    }

    #[test]
    fn artifact_kinds_have_distinct_directories() {
        let mut dirs: Vec<&str> = ArtifactKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(dirs.len(), 3);
        dirs.sort_unstable();
        dirs.dedup();
        assert_eq!(dirs.len(), 3, "two artifact kinds share a directory");
    }

    /// On a Tier-1 OS the host resolves; the point of the assertion is that the
    /// resolution is not `None` where Viso claims support.
    #[test]
    fn the_host_os_resolves_on_a_tier_one_platform() {
        let os = HostOs::current();
        if cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )) {
            assert!(os.is_some());
            assert!(!os.unwrap().as_str().is_empty());
        }
    }
}
