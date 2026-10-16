//! The `Viso.toml` schema — typed, total, and span-carrying.
//!
//! Two properties drive every decision in this module.
//!
//! **Total.** Every key the schema defines is listed in one place per table, and a
//! key that is not in that list is a diagnostic
//! ([`ConfigCode::UnknownKey`]) with a suggestion. Silence is the failure mode
//! this avoids: a typo'd `opt_levle` that a lenient parser skips means the
//! user's setting never took effect and nothing said so.
//!
//! **Span-carrying.** Every accepted value keeps the byte position it was parsed
//! from, wrapped in [`Spanned`]. That is the reason this crate parses with
//! `toml_edit` instead of deserializing: `viso config show` must report *where*
//! each resolved value came from, and a
//! deserialize-into-struct throws the position away at exactly the moment it is
//! cheapest to keep.
//!
//! Tables whose commands are still to come (`[target.android]`, `[target.ios]`,
//! `[export.*]`) are parsed and typed already: a project configured for them must
//! not be told its configuration is unknown.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{Document, Item, TableLike};

use crate::diag::{ConfigCode, ConfigDiagnostic, Span};
use crate::target::{Auto, OptLevel, Profile, Target, WebOptimize};

/// A value together with the position in `Viso.toml` it was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spanned<T> {
    /// The parsed value.
    pub value: T,
    /// Where it was written.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Pairs a value with its position.
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }

    /// Applies `f` to the value, keeping the position.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            value: f(self.value),
            span: self.span,
        }
    }

    /// Borrows the value, keeping the position.
    pub fn as_ref(&self) -> Spanned<&T> {
        Spanned {
            value: &self.value,
            span: self.span,
        }
    }
}

impl<T: fmt::Display> fmt::Display for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

/// `[package]` — identity.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Package {
    /// The project name. The one required key in the whole file: everything that
    /// names an artifact needs it, and guessing it from the directory would make
    /// a rename silently rename the bundle.
    pub name: Option<Spanned<String>>,
    /// Reverse-DNS bundle identifier, required only by the packaging phases.
    pub bundle_id: Option<Spanned<String>>,
    /// Project version.
    pub version: Option<Spanned<String>>,
    /// The Viso language version the package's `.vs` sources are written in; a
    /// source file never restates it.
    pub language: Option<Spanned<String>>,
    /// The capabilities the package is granted, such as `storage.persist`.
    pub capabilities: Vec<Spanned<String>>,
    /// The name a person sees under the app's icon; the project name when
    /// absent.
    pub display_name: Option<Spanned<String>>,
    /// The build number, rising with every upload of one version: iOS's
    /// `CFBundleVersion`, Android's `versionCode`; 1 when absent.
    pub build: Option<Spanned<u32>>,
    /// `[package.icons] source`: the square PNG, at least 1024 pixels, every
    /// icon is scaled from, relative to the project root.
    pub icon: Option<Spanned<String>>,
    /// The orientations a mobile app turns to; every one when absent.
    pub orientation: Option<Spanned<Orientation>>,
    /// `[package.permissions]`: each permission the app asks for, with the
    /// reason the system shows when it asks.
    pub permissions: Vec<(Spanned<Permission>, Spanned<String>)>,
    /// `[package.ios]`: delivery configuration, not a build property, so not
    /// under `[target.ios]`.
    pub ios: IosPackage,
    /// `[package.android]`.
    pub android: AndroidPackage,
    /// `[package.macos]`.
    pub macos: MacosPackage,
    /// `[package.windows]`.
    pub windows: WindowsPackage,
}

/// The orientations a mobile app turns to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Orientation {
    /// Portrait and landscape.
    Any,
    /// Upright only.
    Portrait,
    /// Either way on its side.
    Landscape,
}

impl Orientation {
    /// The name `Viso.toml` gives it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Orientation::Any => "any",
            Orientation::Portrait => "portrait",
            Orientation::Landscape => "landscape",
        }
    }

    /// Every orientation, in declaration order.
    pub const ALL: &'static [Orientation] = &[
        Orientation::Any,
        Orientation::Portrait,
        Orientation::Landscape,
    ];
}

/// A permission an app asks the system for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// The camera.
    Camera,
    /// The microphone.
    Microphone,
    /// Where the device is, while the app is in use.
    Location,
    /// The photo library.
    Photos,
    /// The contacts.
    Contacts,
    /// Bluetooth devices nearby.
    Bluetooth,
    /// Showing notifications.
    Notifications,
}

impl Permission {
    /// The name `[package.permissions]` gives it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Permission::Camera => "camera",
            Permission::Microphone => "microphone",
            Permission::Location => "location",
            Permission::Photos => "photos",
            Permission::Contacts => "contacts",
            Permission::Bluetooth => "bluetooth",
            Permission::Notifications => "notifications",
        }
    }

    /// Every permission, in declaration order.
    pub const ALL: &'static [Permission] = &[
        Permission::Camera,
        Permission::Microphone,
        Permission::Location,
        Permission::Photos,
        Permission::Contacts,
        Permission::Bluetooth,
        Permission::Notifications,
    ];
}

/// `[package.ios]`: how the iOS package is signed. The identity's key stays
/// in the keychain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IosPackage {
    /// The Apple Developer team.
    pub team_id: Option<Spanned<String>>,
    /// The signing identity, as the keychain names it; the one of the team
    /// when absent.
    pub identity: Option<Spanned<String>>,
    /// The provisioning profile embedded and signed with, relative to the
    /// project root.
    pub provisioning_profile: Option<Spanned<String>>,
}

/// `[package.android]`: how the Android package is built and signed. The
/// keystore's passwords are never configuration: they come from the
/// environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AndroidPackage {
    /// The release keystore, relative to the project root.
    pub keystore: Option<Spanned<String>>,
    /// The key in it.
    pub key_alias: Option<Spanned<String>>,
    /// The ABIs the package carries; `arm64-v8a` when absent.
    pub abis: Vec<Spanned<String>>,
}

/// `[package.macos]`: how the macOS package is signed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacosPackage {
    /// The signing identity, as the keychain names it; a Developer ID one
    /// found there when absent.
    pub identity: Option<Spanned<String>>,
}

/// `[package.windows]`: how the Windows executable is signed. The
/// certificate and its key stay in the system's certificate store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsPackage {
    /// The certificate: its subject name, or its SHA-1 thumbprint.
    pub identity: Option<Spanned<String>>,
    /// The RFC 3161 server the signature is timestamped by, so it outlives
    /// the certificate.
    pub timestamp_url: Option<Spanned<String>>,
}

/// One `[profile.X]` table.
///
/// Every field is optional: absent means "use the profile's own default"
/// ([`Profile::default_opt_level`] and friends), which is what keeps the resolved
/// configuration total without the manifest having to spell out all three
/// profiles.
///
/// There is deliberately no `hot_reload` field. See [`Profiles`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileConfig {
    /// Optimization level override.
    pub opt_level: Option<Spanned<OptLevel>>,
    /// Source-map emission override.
    pub source_maps: Option<Spanned<bool>>,
    /// Symbol-stripping override.
    pub strip: Option<Spanned<bool>>,
    /// `[profile.X.web]`.
    pub web: WebProfileConfig,
}

/// `[profile.X.web]`: how a web build of the profile is optimized. `strip`
/// and `source_maps` fall back to the profile's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebProfileConfig {
    /// `optimize`: `none`, `speed` or `size`.
    pub optimize: Option<Spanned<WebOptimize>>,
    /// `strip`: the name and producers sections, and the debug info.
    pub strip: Option<Spanned<bool>>,
    /// `source_maps`: the module keeps its debug info.
    pub source_maps: Option<Spanned<bool>>,
    /// `brotli`: a `.br` beside each file.
    pub brotli: Option<Spanned<bool>>,
    /// `gzip`: a `.gz` beside each file.
    pub gzip: Option<Spanned<bool>>,
    /// `split`: the module split into a startup and a lazy part.
    pub split: Option<Spanned<Auto>>,
    /// `threads`: a shared-memory, threaded module.
    pub threads: Option<Spanned<Auto>>,
}

/// The three `[profile.X]` tables.
///
/// A struct with three named fields rather than a map, because the profile set is
/// closed ([`Profile`]) and a map would invite a fourth profile whose dev-runtime
/// question has no answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profiles {
    /// `[profile.dev]`.
    pub dev: ProfileConfig,
    /// `[profile.release]`.
    pub release: ProfileConfig,
    /// `[profile.shipping]`.
    pub shipping: ProfileConfig,
}

impl Profiles {
    /// The table for one profile.
    pub fn get(&self, profile: Profile) -> &ProfileConfig {
        match profile {
            Profile::Dev => &self.dev,
            Profile::Release => &self.release,
            Profile::Shipping => &self.shipping,
        }
    }
}

/// `[web.serve]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebServe {
    /// Dev-server port.
    pub port: Option<Spanned<u16>>,
    /// Whether to open a browser on start.
    pub open: Option<Spanned<bool>>,
}

/// `[game] determinism`: how reproducible a game's simulation must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GameDeterminism {
    /// The same build on the same target replays tick for tick.
    #[default]
    SameBinary,
    /// Every Tier-1 target produces byte-identical snapshot hashes.
    CrossPlatform,
}

impl GameDeterminism {
    /// The spelling `Viso.toml` uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            GameDeterminism::SameBinary => "same_binary",
            GameDeterminism::CrossPlatform => "cross_platform",
        }
    }
}

/// `[game] collision_delivery`: the order a tick hands its contacts to the
/// `CollisionListener`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CollisionDelivery {
    /// Each contact to every listener before the next contact.
    #[default]
    EventMajor,
    /// Every contact to one listener before the next listener.
    ListenerMajor,
}

impl CollisionDelivery {
    /// The spelling `Viso.toml` uses.
    pub const fn as_str(self) -> &'static str {
        match self {
            CollisionDelivery::EventMajor => "event_major",
            CollisionDelivery::ListenerMajor => "listener_major",
        }
    }
}

/// `[game]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Game {
    /// `determinism`.
    pub determinism: Option<Spanned<GameDeterminism>>,
    /// `tick_rate`: the fixed step's ticks a second, 1 to 1000; 60 when
    /// absent.
    pub tick_rate: Option<Spanned<u32>>,
    /// `collision_delivery`; `event_major` when absent.
    pub collision_delivery: Option<Spanned<CollisionDelivery>>,
}

/// Device pixels per logical pixel: finite, above 0 and at most
/// [`SurfaceScale::MAX`].
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct SurfaceScale(f64);

// A scale is never NaN, so equality is total.
impl Eq for SurfaceScale {}

impl SurfaceScale {
    /// The largest scale.
    pub const MAX: f64 = 8.0;

    /// 1x.
    pub const ONE: SurfaceScale = SurfaceScale(1.0);

    /// `value` as a scale, when it is one.
    pub fn new(value: f64) -> Option<SurfaceScale> {
        (value > 0.0 && value <= Self::MAX).then_some(SurfaceScale(value))
    }

    /// The scale as a number.
    pub const fn get(self) -> f64 {
        self.0
    }
}

/// `[test.surface]`: the surface every window of a headless run gets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestSurface {
    /// `width`, in logical pixels, 1 to [`TestSurface::MAX_EDGE`]; 1280 when
    /// absent.
    pub width: Option<Spanned<u32>>,
    /// `height`, likewise; 720 when absent.
    pub height: Option<Spanned<u32>>,
    /// `scale`; 1 when absent.
    pub scale: Option<Spanned<SurfaceScale>>,
}

impl TestSurface {
    /// The largest edge, in logical pixels.
    pub const MAX_EDGE: u32 = 16384;
    /// The width when none is set.
    pub const WIDTH: u32 = 1280;
    /// The height when none is set.
    pub const HEIGHT: u32 = 720;

    /// The size and scale in effect.
    pub fn resolved(&self) -> (u32, u32, SurfaceScale) {
        (
            self.width.map_or(Self::WIDTH, |w| w.value),
            self.height.map_or(Self::HEIGHT, |h| h.value),
            self.scale.map_or(SurfaceScale::ONE, |s| s.value),
        )
    }
}

/// `[test]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Test {
    /// `[test.surface]`.
    pub surface: TestSurface,
}

/// `[i18n]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct I18n {
    /// `source`: the BCP 47 locale whose catalog (`i18n/<source>.toml`)
    /// declares every message; `en` when absent.
    pub source: Option<Spanned<String>>,
}

/// `[web]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Web {
    /// Default web target, one of the three `web-*` variants.
    pub default_target: Option<Spanned<Target>>,
    /// `[web.serve]`.
    pub serve: WebServe,
}

/// `[target.android]`. The GPU backend is not configuration: each target's is
/// fixed at compile time.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Android {
    /// Minimum Android SDK level.
    pub min_sdk: Option<Spanned<u32>>,
}

/// `[target.ios]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ios {
    /// Minimum OS version, as written (`"17.0"`).
    pub minimum_os: Option<Spanned<String>>,
}

/// `[target.*]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Targets {
    /// `[target.android]`.
    pub android: Android,
    /// `[target.ios]`.
    pub ios: Ios,
}

/// `[export.html]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportHtml {
    /// Output directory.
    pub out_dir: Option<Spanned<String>>,
}

/// `[export.solid]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportSolid {
    /// Output directory.
    pub out_dir: Option<Spanned<String>>,
    /// Which package manager the generated project expects.
    pub package_manager: Option<Spanned<String>>,
}

/// `[export.*]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Export {
    /// `[export.html]`.
    pub html: ExportHtml,
    /// `[export.solid]`.
    pub solid: ExportSolid,
}

/// `[workspace]`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    /// Member directory paths, relative to the root.
    pub members: Vec<Spanned<String>>,
}

/// A parsed, typed `Viso.toml`.
///
/// Absence is represented by `None`/`Vec::new()`, never by a substituted default:
/// substituting here would erase the difference between "the user wrote this" and
/// "the framework chose this", which is precisely what
/// [`crate::Origin`] has to report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// The file this was parsed from.
    pub path: PathBuf,
    /// `[package]`.
    pub package: Package,
    /// `[profile.*]`.
    pub profiles: Profiles,
    /// `[web]`.
    pub web: Web,
    /// `[target.*]`.
    pub targets: Targets,
    /// `[export.*]`.
    pub export: Export,
    /// `[workspace]`.
    pub workspace: Workspace,
    /// `[game]`.
    pub game: Game,
    /// `[test]`.
    pub test: Test,
    /// `[i18n]`.
    pub i18n: I18n,
}

impl Manifest {
    /// Parses manifest text.
    ///
    /// Returns the manifest *and* its warnings on success, and the errors on
    /// failure. An unknown key is an error: a setting that never takes effect is
    /// a build other than the one its author asked for. The one warning is a key
    /// that is known and redundant, `hot_reload` under the dev profile.
    pub fn parse(path: impl Into<PathBuf>, text: &str) -> ParseOutcome {
        let path = path.into();
        // `Document`, not `DocumentMut`: only the immutable parse retains byte spans,
        // and spans are the entire reason this module parses rather than
        // deserializes. Nothing here edits the document.
        let doc: Document<&str> = match Document::parse(text) {
            Ok(doc) => doc,
            Err(err) => {
                let span = err.span().map(|range| Span::from_range(text, range));
                let mut diag = ConfigDiagnostic::error(
                    ConfigCode::ManifestSyntax,
                    err.message().trim().to_string(),
                )
                .at(&path);
                if let Some(span) = span {
                    diag = diag.span(span);
                }
                return ParseOutcome::Failed(vec![diag]);
            }
        };

        let mut reader = Reader {
            path: &path,
            text,
            diags: Vec::new(),
        };
        let manifest = reader.read(&path, doc.as_table());
        let (errors, warnings): (Vec<_>, Vec<_>) =
            reader.diags.into_iter().partition(|d| d.is_error());
        if errors.is_empty() {
            ParseOutcome::Parsed { manifest, warnings }
        } else {
            ParseOutcome::Failed(errors)
        }
    }

    /// Reads and parses a manifest from disk.
    pub fn load(path: impl AsRef<Path>) -> ParseOutcome {
        let path = path.as_ref();
        match std::fs::read_to_string(path) {
            Ok(text) => Manifest::parse(path, &text),
            Err(err) => ParseOutcome::Failed(vec![
                ConfigDiagnostic::error(
                    ConfigCode::ManifestUnreadable,
                    format!("could not read `{}`: {err}", path.display()),
                )
                .at(path),
            ]),
        }
    }

    /// The project name, or the [`ConfigCode::MissingRequired`] diagnostic.
    pub fn require_name(&self) -> Result<&str, ConfigDiagnostic> {
        self.package
            .name
            .as_ref()
            .map(|n| n.value.as_str())
            .ok_or_else(|| {
                ConfigDiagnostic::error(ConfigCode::MissingRequired, "`package.name` is required")
                    .at(&self.path)
                    .note("add `[package]` with `name = \"my-app\"`")
            })
    }
}

/// A bundle identifier both iOS and Android accept.
fn bundle_id(id: &str) -> Result<(), &'static str> {
    let parts: Vec<&str> = id.split('.').collect();
    let part = |p: &&str| {
        p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
            && p.chars().all(|c| c.is_ascii_alphanumeric())
    };
    if parts.len() >= 2 && parts.iter().all(part) {
        Ok(())
    } else {
        Err(
            "a reverse-DNS name such as `com.example.app`: two or more parts of letters and \
             digits, each starting with a letter",
        )
    }
}

/// An `http` or `https` URL.
fn timestamp_url(url: &str) -> Result<(), &'static str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"));
    match rest {
        Some(host) if !host.is_empty() && !url.contains(char::is_whitespace) => Ok(()),
        _ => Err("an `http://` or `https://` URL"),
    }
}

/// `1`, `1.2` or `1.2.3`, with an optional `-pre` or `+build` suffix.
fn version(version: &str) -> Result<(), &'static str> {
    let core = version.split(['-', '+']).next().unwrap_or_default();
    let suffix_ok = version.len() == core.len() || version.len() > core.len() + 1;
    if suffix_ok && dotted_numbers(core) {
        Ok(())
    } else {
        Err(
            "one to three dot-separated numbers such as `1.4.0`, optionally followed by \
             `-pre` or `+build`",
        )
    }
}

/// An Apple Developer team ID.
fn team_id(id: &str) -> Result<(), &'static str> {
    if id.len() == 10
        && id
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        Ok(())
    } else {
        Err("ten uppercase letters and digits, as the Apple Developer account shows it")
    }
}

/// The highest build number Android's `versionCode` allows.
pub const MAX_BUILD: u32 = 2_100_000_000;

/// The ABIs an Android package can carry.
pub const ANDROID_ABIS: &[&str] = &["arm64-v8a", "armeabi-v7a", "x86_64", "x86"];

/// A name a home screen can show: not empty, at most 30 characters, no
/// control characters.
fn display_name(name: &str) -> Result<(), &'static str> {
    if !name.trim().is_empty() && name.chars().count() <= 30 && !name.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err("a name of 1 to 30 characters, with no control characters")
    }
}

/// An OS version such as `17.0`.
fn os_version(version: &str) -> Result<(), &'static str> {
    if dotted_numbers(version) {
        Ok(())
    } else {
        Err("one to three dot-separated numbers such as `17.0`")
    }
}

fn dotted_numbers(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    (1..=3).contains(&parts.len())
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// A directory inside the project, relative to its root.
fn out_dir(dir: &str) -> Result<(), &'static str> {
    let path = Path::new(dir);
    let inside = path.components().all(|c| {
        matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    });
    if !dir.is_empty() && path.is_relative() && inside {
        Ok(())
    } else {
        Err("a directory relative to the project root that stays inside it, such as `dist-html`")
    }
}

/// A package manager the generated project can name.
fn package_manager(name: &str) -> Result<(), &'static str> {
    match name {
        "npm" | "pnpm" | "yarn" | "bun" => Ok(()),
        _ => Err("expected one of: npm, pnpm, yarn, bun"),
    }
}

/// The result of parsing a manifest.
///
/// The success variant is much larger than the failure one, which clippy flags. Boxing
/// either half would trade a real allocation on the path that always runs for a smaller
/// enum on a path taken once per file and consumed immediately.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ParseOutcome {
    /// The manifest is usable; `warnings` may still be non-empty.
    Parsed {
        /// The typed manifest.
        manifest: Manifest,
        /// Non-fatal problems.
        warnings: Vec<ConfigDiagnostic>,
    },
    /// The manifest is not usable.
    Failed(Vec<ConfigDiagnostic>),
}

impl ParseOutcome {
    /// The manifest, discarding warnings — for callers that only want the data.
    pub fn ok(self) -> Result<Manifest, Vec<ConfigDiagnostic>> {
        match self {
            ParseOutcome::Parsed { manifest, .. } => Ok(manifest),
            ParseOutcome::Failed(diags) => Err(diags),
        }
    }
}

/// Table-walking state: the file being read and the diagnostics collected so far.
///
/// A reader rather than a set of free functions because every accessor needs the
/// same three things (path for the diagnostic, text for the span, sink for the
/// result), and threading them through twenty call sites is how one of them ends
/// up losing the span.
struct Reader<'a> {
    path: &'a Path,
    text: &'a str,
    diags: Vec<ConfigDiagnostic>,
}

impl<'a> Reader<'a> {
    fn read(&mut self, path: &Path, root: &dyn TableLike) -> Manifest {
        self.deny_unknown(
            root,
            &[
                "package",
                "profile",
                "web",
                "target",
                "export",
                "workspace",
                "game",
                "test",
                "i18n",
            ],
            "",
        );

        let package = match self.table(root, "package") {
            Some(t) => {
                self.deny_unknown(
                    t,
                    &[
                        "name",
                        "bundle_id",
                        "version",
                        "language",
                        "capabilities",
                        "display_name",
                        "build",
                        "icons",
                        "orientation",
                        "permissions",
                        "ios",
                        "android",
                        "macos",
                        "windows",
                    ],
                    "package",
                );
                let orientations: Vec<(&str, Orientation)> =
                    Orientation::ALL.iter().map(|o| (o.as_str(), *o)).collect();
                Package {
                    name: self.string(t, "name", "package"),
                    bundle_id: self.checked(t, "bundle_id", "package", bundle_id),
                    version: self.checked(t, "version", "package", version),
                    language: self.string(t, "language", "package"),
                    capabilities: self.string_array(t, "capabilities", "package"),
                    display_name: self.checked(t, "display_name", "package", display_name),
                    build: self.build_number(t),
                    icon: match self.table(t, "icons") {
                        Some(icons) => {
                            self.deny_unknown(icons, &["source"], "package.icons");
                            self.string(icons, "source", "package.icons")
                        }
                        None => None,
                    },
                    orientation: self.choice(t, "orientation", "package", &orientations),
                    permissions: self.permissions(t),
                    ios: match self.table(t, "ios") {
                        Some(ios) => {
                            self.deny_unknown(
                                ios,
                                &["team_id", "identity", "provisioning_profile"],
                                "package.ios",
                            );
                            IosPackage {
                                team_id: self.checked(ios, "team_id", "package.ios", team_id),
                                identity: self.string(ios, "identity", "package.ios"),
                                provisioning_profile: self.string(
                                    ios,
                                    "provisioning_profile",
                                    "package.ios",
                                ),
                            }
                        }
                        None => IosPackage::default(),
                    },
                    android: match self.table(t, "android") {
                        Some(android) => {
                            self.deny_unknown(
                                android,
                                &["keystore", "key_alias", "abis"],
                                "package.android",
                            );
                            AndroidPackage {
                                keystore: self.string(android, "keystore", "package.android"),
                                key_alias: self.string(android, "key_alias", "package.android"),
                                abis: self.abis(android),
                            }
                        }
                        None => AndroidPackage::default(),
                    },
                    macos: match self.table(t, "macos") {
                        Some(macos) => {
                            self.deny_unknown(macos, &["identity"], "package.macos");
                            MacosPackage {
                                identity: self.string(macos, "identity", "package.macos"),
                            }
                        }
                        None => MacosPackage::default(),
                    },
                    windows: match self.table(t, "windows") {
                        Some(windows) => {
                            self.deny_unknown(
                                windows,
                                &["identity", "timestamp_url"],
                                "package.windows",
                            );
                            WindowsPackage {
                                identity: self.string(windows, "identity", "package.windows"),
                                timestamp_url: self.checked(
                                    windows,
                                    "timestamp_url",
                                    "package.windows",
                                    timestamp_url,
                                ),
                            }
                        }
                        None => WindowsPackage::default(),
                    },
                }
            }
            None => Package::default(),
        };

        let profiles = match self.table(root, "profile") {
            Some(t) => {
                // A `[profile.X]` whose name is not a profile is an error rather
                // than an unknown key: the user configured a build that will never
                // run, and the nearest correct spelling is worth naming.
                let known: Vec<&str> = Profile::ALL.iter().map(|p| p.as_str()).collect();
                for (name, item) in t.iter() {
                    if !known.contains(&name) {
                        let span = self.span_of(self.key_range(t, name).or_else(|| item.span()));
                        let mut diag = ConfigDiagnostic::error(
                            ConfigCode::UnknownProfile,
                            format!("`{name}` is not a profile"),
                        )
                        .at(self.path)
                        .note("expected one of: dev, release, shipping");
                        if let Some(span) = span {
                            diag = diag.span(span);
                        }
                        self.diags.push(diag);
                    }
                }
                Profiles {
                    dev: self.profile(t, Profile::Dev),
                    release: self.profile(t, Profile::Release),
                    shipping: self.profile(t, Profile::Shipping),
                }
            }
            None => Profiles::default(),
        };

        let web = match self.table(root, "web") {
            Some(t) => {
                self.deny_unknown(t, &["default_target", "serve"], "web");
                Web {
                    default_target: self.web_target(t, "default_target", "web"),
                    serve: match self.table(t, "serve") {
                        Some(s) => {
                            self.deny_unknown(s, &["port", "open"], "web.serve");
                            WebServe {
                                port: self.port(s, "port", "web.serve"),
                                open: self.boolean(s, "open", "web.serve"),
                            }
                        }
                        None => WebServe::default(),
                    },
                }
            }
            None => Web::default(),
        };

        let targets = match self.table(root, "target") {
            Some(t) => {
                self.deny_unknown(t, &["android", "ios"], "target");
                Targets {
                    android: match self.table(t, "android") {
                        Some(a) => {
                            self.deny_unknown(a, &["min_sdk"], "target.android");
                            Android {
                                min_sdk: self.sdk_level(a, "min_sdk", "target.android"),
                            }
                        }
                        None => Android::default(),
                    },
                    ios: match self.table(t, "ios") {
                        Some(i) => {
                            self.deny_unknown(i, &["minimum_os"], "target.ios");
                            Ios {
                                minimum_os: self.checked(i, "minimum_os", "target.ios", os_version),
                            }
                        }
                        None => Ios::default(),
                    },
                }
            }
            None => Targets::default(),
        };

        let export = match self.table(root, "export") {
            Some(t) => {
                self.deny_unknown(t, &["html", "solid"], "export");
                Export {
                    html: match self.table(t, "html") {
                        Some(h) => {
                            self.deny_unknown(h, &["out_dir"], "export.html");
                            ExportHtml {
                                out_dir: self.checked(h, "out_dir", "export.html", out_dir),
                            }
                        }
                        None => ExportHtml::default(),
                    },
                    solid: match self.table(t, "solid") {
                        Some(s) => {
                            self.deny_unknown(s, &["out_dir", "package_manager"], "export.solid");
                            ExportSolid {
                                out_dir: self.checked(s, "out_dir", "export.solid", out_dir),
                                package_manager: self.checked(
                                    s,
                                    "package_manager",
                                    "export.solid",
                                    package_manager,
                                ),
                            }
                        }
                        None => ExportSolid::default(),
                    },
                }
            }
            None => Export::default(),
        };

        let workspace = match self.table(root, "workspace") {
            Some(t) => {
                self.deny_unknown(t, &["members"], "workspace");
                Workspace {
                    members: self.string_array(t, "members", "workspace"),
                }
            }
            None => Workspace::default(),
        };

        let game = match self.table(root, "game") {
            Some(t) => {
                self.deny_unknown(
                    t,
                    &["determinism", "tick_rate", "collision_delivery"],
                    "game",
                );
                Game {
                    determinism: self.determinism(t, "determinism", "game"),
                    tick_rate: self.tick_rate(t, "tick_rate", "game"),
                    collision_delivery: self.collision_delivery(t, "collision_delivery", "game"),
                }
            }
            None => Game::default(),
        };

        let test = match self.table(root, "test") {
            Some(t) => {
                self.deny_unknown(t, &["surface"], "test");
                let surface = match self.table(t, "surface") {
                    Some(s) => {
                        self.deny_unknown(s, &["width", "height", "scale"], "test.surface");
                        TestSurface {
                            width: self.edge(s, "width", "test.surface"),
                            height: self.edge(s, "height", "test.surface"),
                            scale: self.scale(s, "scale", "test.surface"),
                        }
                    }
                    None => TestSurface::default(),
                };
                Test { surface }
            }
            None => Test::default(),
        };

        let i18n = match self.table(root, "i18n") {
            Some(t) => {
                self.deny_unknown(t, &["source"], "i18n");
                I18n {
                    source: self.string(t, "source", "i18n"),
                }
            }
            None => I18n::default(),
        };

        Manifest {
            path: path.to_path_buf(),
            package,
            profiles,
            web,
            targets,
            export,
            workspace,
            game,
            test,
            i18n,
        }
    }

    /// Reads one `[profile.X]` table.
    ///
    /// The hot-reload check lives here rather than in a generic key check because
    /// the *same spelling* means two different things depending on the profile: an
    /// error under release/shipping, and a
    /// redundant-but-harmless warning under dev, where the Dev Runtime is present
    /// regardless.
    fn profile(&mut self, profiles: &dyn TableLike, profile: Profile) -> ProfileConfig {
        let Some(t) = self.table(profiles, profile.as_str()) else {
            return ProfileConfig::default();
        };
        let label = format!("profile.{profile}");
        self.deny_unknown_except(
            t,
            &["opt_level", "source_maps", "strip", "web"],
            HOT_RELOAD_SYNONYMS,
            &label,
        );

        for synonym in HOT_RELOAD_SYNONYMS {
            let Some(item) = t.get(synonym) else { continue };
            let span = self.span_of(self.key_range(t, synonym).or_else(|| item.span()));
            let mut diag = match profile.dev_runtime() {
                crate::target::DevRuntime::Absent => ConfigDiagnostic::error(
                    ConfigCode::HotReloadInRelease,
                    format!("`{label}.{synonym}` cannot enable hot reload"),
                )
                .note(
                    "the Dev Runtime is fixed by the profile: dev has it, release and \
                     shipping do not",
                )
                .note("remove the key; build with `--profile dev` to develop"),
                crate::target::DevRuntime::Present => ConfigDiagnostic::warning(
                    ConfigCode::UnknownKey,
                    format!("`{label}.{synonym}` has no effect"),
                )
                .note("the dev profile always carries the Dev Runtime"),
            }
            .at(self.path);
            if let Some(span) = span {
                diag = diag.span(span);
            }
            self.diags.push(diag);
        }

        let web = match self.table(t, "web") {
            Some(w) => {
                let label = format!("{label}.web");
                self.deny_unknown(
                    w,
                    &[
                        "optimize",
                        "strip",
                        "source_maps",
                        "brotli",
                        "gzip",
                        "split",
                        "threads",
                    ],
                    &label,
                );
                let optimize = WebOptimize::ALL.map(|o| (o.as_str(), o));
                let auto = Auto::ALL.map(|a| (a.as_str(), a));
                WebProfileConfig {
                    optimize: self.choice(w, "optimize", &label, &optimize),
                    strip: self.boolean(w, "strip", &label),
                    source_maps: self.boolean(w, "source_maps", &label),
                    brotli: self.boolean(w, "brotli", &label),
                    gzip: self.boolean(w, "gzip", &label),
                    split: self.choice(w, "split", &label, &auto),
                    threads: self.choice(w, "threads", &label, &auto),
                }
            }
            None => WebProfileConfig::default(),
        };
        ProfileConfig {
            opt_level: self.opt_level(t, "opt_level", &label),
            source_maps: self.boolean(t, "source_maps", &label),
            strip: self.boolean(t, "strip", &label),
            web,
        }
    }

    /// A string that names one of `options`, as the value it names.
    fn choice<T: Copy>(
        &mut self,
        t: &dyn TableLike,
        key: &str,
        path: &str,
        options: &[(&str, T)],
    ) -> Option<Spanned<T>> {
        let raw = self.string(t, key, path)?;
        match options.iter().find(|(name, _)| *name == raw.value) {
            Some((_, value)) => Some(Spanned::new(*value, raw.span)),
            None => {
                let names: Vec<String> = options.iter().map(|(n, _)| format!("`{n}`")).collect();
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` is not `{}`", raw.value),
                    )
                    .at(self.path)
                    .span(raw.span)
                    .note(format!("expected one of: {}", names.join(", "))),
                );
                None
            }
        }
    }

    /// `[package] build`: a build number both stores take, 1 to 2100000000.
    fn build_number(&mut self, t: &dyn TableLike) -> Option<Spanned<u32>> {
        let raw = self.integer(t, "build", "package")?;
        match u32::try_from(raw.value) {
            Ok(build) if (1..=MAX_BUILD).contains(&build) => Some(Spanned::new(build, raw.span)),
            _ => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`package.build` cannot be {}", raw.value),
                    )
                    .at(self.path)
                    .span(raw.span)
                    .note(format!(
                        "a whole number from 1 to {MAX_BUILD}, as Android's `versionCode` allows"
                    )),
                );
                None
            }
        }
    }

    /// `[package.permissions]`: each known permission with a reason that
    /// says why.
    fn permissions(&mut self, t: &dyn TableLike) -> Vec<(Spanned<Permission>, Spanned<String>)> {
        let Some(table) = self.table(t, "permissions") else {
            return Vec::new();
        };
        let names: Vec<&str> = Permission::ALL.iter().map(|p| p.as_str()).collect();
        self.deny_unknown(table, &names, "package.permissions");
        let mut out = Vec::new();
        for permission in Permission::ALL {
            let name = permission.as_str();
            let Some(reason) = self.string(table, name, "package.permissions") else {
                continue;
            };
            if reason.value.trim().is_empty() {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`package.permissions.{name}` says no reason"),
                    )
                    .at(self.path)
                    .span(reason.span)
                    .note("the system shows it when it asks, as `\"Scans receipts\"`"),
                );
                continue;
            }
            let key = self
                .span_of(self.key_range(table, name))
                .unwrap_or(reason.span);
            out.push((Spanned::new(*permission, key), reason));
        }
        out
    }

    /// `[package.android] abis`: each one Android names.
    fn abis(&mut self, t: &dyn TableLike) -> Vec<Spanned<String>> {
        let abis = self.string_array(t, "abis", "package.android");
        let mut out = Vec::with_capacity(abis.len());
        for abi in abis {
            if ANDROID_ABIS.contains(&abi.value.as_str()) {
                out.push(abi);
            } else {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`package.android.abis` cannot hold `{}`", abi.value),
                    )
                    .at(self.path)
                    .span(abi.span)
                    .note(format!("one of: {}", ANDROID_ABIS.join(", "))),
                );
            }
        }
        out
    }

    /// A sub-table, accepting both `[a.b]` and `a = { b = ... }` forms.
    fn table<'t>(&mut self, parent: &'t dyn TableLike, key: &str) -> Option<&'t dyn TableLike> {
        let item = parent.get(key)?;
        match item.as_table_like() {
            Some(t) => Some(t),
            None => {
                self.wrong_type(parent, key, item, "a table", key);
                None
            }
        }
    }

    fn string(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<String>> {
        let item = t.get(key)?;
        match item.as_str() {
            Some(s) => Some(Spanned::new(s.to_string(), self.value_span(t, key, item))),
            None => {
                self.wrong_type(t, key, item, "a string", path);
                None
            }
        }
    }

    /// A string `check` accepts, or an [`ConfigCode::InvalidValue`] with the
    /// note `check` gives.
    fn checked(
        &mut self,
        t: &dyn TableLike,
        key: &str,
        path: &str,
        check: fn(&str) -> Result<(), &'static str>,
    ) -> Option<Spanned<String>> {
        let value = self.string(t, key, path)?;
        match check(&value.value) {
            Ok(()) => Some(value),
            Err(expected) => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` cannot be `{}`", value.value),
                    )
                    .at(self.path)
                    .span(value.span)
                    .note(expected),
                );
                None
            }
        }
    }

    fn boolean(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<bool>> {
        let item = t.get(key)?;
        match item.as_bool() {
            Some(b) => Some(Spanned::new(b, self.value_span(t, key, item))),
            None => {
                self.wrong_type(t, key, item, "a boolean", path);
                None
            }
        }
    }

    fn integer(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<i64>> {
        let item = t.get(key)?;
        match item.as_integer() {
            Some(i) => Some(Spanned::new(i, self.value_span(t, key, item))),
            None => {
                self.wrong_type(t, key, item, "an integer", path);
                None
            }
        }
    }

    /// A TCP port: an integer, range-checked. The range check is here rather than
    /// at use because a `port = 99999` is wrong when it is *written*, and saying so
    /// two phases later, from inside a server that failed to bind, is worse.
    fn port(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<u16>> {
        let raw = self.integer(t, key, path)?;
        match u16::try_from(raw.value) {
            Ok(port) if port != 0 => Some(Spanned::new(port, raw.span)),
            _ => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` must be a port between 1 and 65535"),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                None
            }
        }
    }

    fn sdk_level(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<u32>> {
        let raw = self.integer(t, key, path)?;
        match u32::try_from(raw.value) {
            Ok(level) => Some(Spanned::new(level, raw.span)),
            Err(_) => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` must be a non-negative SDK level"),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                None
            }
        }
    }

    fn edge(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<u32>> {
        let raw = self.integer(t, key, path)?;
        match u32::try_from(raw.value) {
            Ok(edge @ 1..=TestSurface::MAX_EDGE) => Some(Spanned::new(edge, raw.span)),
            _ => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!(
                            "`{path}.{key}` is from 1 to {} logical pixels",
                            TestSurface::MAX_EDGE
                        ),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                None
            }
        }
    }

    /// A scale, written as an integer or a float.
    fn scale(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<SurfaceScale>> {
        let item = t.get(key)?;
        let span = self.value_span(t, key, item);
        let Some(value) = item
            .as_float()
            .or_else(|| item.as_integer().map(|n| n as f64))
        else {
            self.wrong_type(t, key, item, "a number", path);
            return None;
        };
        match SurfaceScale::new(value) {
            Some(scale) => Some(Spanned::new(scale, span)),
            None => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!(
                            "`{path}.{key}` is above 0 and at most {}",
                            SurfaceScale::MAX
                        ),
                    )
                    .at(self.path)
                    .span(span),
                );
                None
            }
        }
    }

    fn tick_rate(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<u32>> {
        let raw = self.integer(t, key, path)?;
        match u32::try_from(raw.value) {
            Ok(rate @ 1..=1000) => Some(Spanned::new(rate, raw.span)),
            _ => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` is a tick rate from 1 to 1000 Hz"),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                None
            }
        }
    }

    fn determinism(
        &mut self,
        t: &dyn TableLike,
        key: &str,
        path: &str,
    ) -> Option<Spanned<GameDeterminism>> {
        let raw = self.string(t, key, path)?;
        let value = match raw.value.as_str() {
            "same_binary" => GameDeterminism::SameBinary,
            "cross_platform" => GameDeterminism::CrossPlatform,
            other => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!(
                            "`{path}.{key}` is `same_binary` or `cross_platform`, not `{other}`"
                        ),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                return None;
            }
        };
        Some(Spanned::new(value, raw.span))
    }

    fn collision_delivery(
        &mut self,
        t: &dyn TableLike,
        key: &str,
        path: &str,
    ) -> Option<Spanned<CollisionDelivery>> {
        let raw = self.string(t, key, path)?;
        let value = match raw.value.as_str() {
            "event_major" => CollisionDelivery::EventMajor,
            "listener_major" => CollisionDelivery::ListenerMajor,
            other => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!(
                            "`{path}.{key}` is `event_major` or `listener_major`, not `{other}`"
                        ),
                    )
                    .at(self.path)
                    .span(raw.span),
                );
                return None;
            }
        };
        Some(Spanned::new(value, raw.span))
    }

    /// One of the three web targets.
    fn web_target(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<Target>> {
        let raw = self.string(t, key, path)?;
        match Target::parse(&raw.value) {
            Ok(target @ (Target::WebGpu | Target::WebDom | Target::WebHybrid)) => {
                Some(Spanned::new(target, raw.span))
            }
            Ok(_) => {
                self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::InvalidValue,
                        format!("`{path}.{key}` must be a web target, not `{}`", raw.value),
                    )
                    .at(self.path)
                    .span(raw.span)
                    .note("expected one of: web-gpu, web-dom, web-hybrid"),
                );
                None
            }
            Err(diag) => {
                self.diags.push(diag.at(self.path).span(raw.span));
                None
            }
        }
    }

    /// `opt_level` accepts both `0` and `"size"` (the documented example uses
    /// each form), so the integer is normalized to its name
    /// and both spellings land on one [`OptLevel`].
    fn opt_level(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<OptLevel>> {
        let item = t.get(key)?;
        let span = self.value_span(t, key, item);
        let text = match (item.as_integer(), item.as_str()) {
            (Some(i), _) => i.to_string(),
            (_, Some(s)) => s.to_string(),
            _ => {
                self.wrong_type(t, key, item, "an integer or a string", path);
                return None;
            }
        };
        match OptLevel::parse(&text) {
            Ok(level) => Some(Spanned::new(level, span)),
            Err(diag) => {
                self.diags.push(diag.at(self.path).span(span));
                None
            }
        }
    }

    fn string_array(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Vec<Spanned<String>> {
        let Some(item) = t.get(key) else {
            return Vec::new();
        };
        let Some(array) = item.as_array() else {
            self.wrong_type(t, key, item, "an array of strings", path);
            return Vec::new();
        };
        let mut out = Vec::with_capacity(array.len());
        for value in array.iter() {
            let span = self.span_of(value.span()).unwrap_or(Span::FILE_START);
            match value.as_str() {
                Some(s) => out.push(Spanned::new(s.to_string(), span)),
                None => self.diags.push(
                    ConfigDiagnostic::error(
                        ConfigCode::WrongType,
                        format!(
                            "`{path}.{key}` must contain strings, found {}",
                            value.type_name()
                        ),
                    )
                    .at(self.path)
                    .span(span),
                ),
            }
        }
        out
    }

    /// Flags every key in `t` that the schema does not define.
    ///
    /// A warning, not an error, plus the nearest known key when one is close. The
    /// severity choice matters: a manifest written for a newer Viso should still
    /// build with an older one, loudly.
    fn deny_unknown(&mut self, t: &dyn TableLike, allowed: &[&str], path: &str) {
        self.deny_unknown_except(t, allowed, &[], path);
    }

    /// As above, but silent about keys that already have a better diagnostic of their
    /// own. Two reports for one key sends the reader to fix it twice, and the weaker
    /// "unknown key, did you mean…" buries the one that explains the real problem.
    fn deny_unknown_except(
        &mut self,
        t: &dyn TableLike,
        allowed: &[&str],
        ignored: &[&str],
        path: &str,
    ) {
        for (name, item) in t.iter() {
            if allowed.contains(&name) || ignored.contains(&name) {
                continue;
            }
            let span = self.span_of(self.key_range(t, name).or_else(|| item.span()));
            let where_ = if path.is_empty() {
                format!("`{name}`")
            } else {
                format!("`{path}.{name}`")
            };
            let mut diag =
                ConfigDiagnostic::error(ConfigCode::UnknownKey, format!("unknown key {where_}"))
                    .at(self.path);
            if let Some(span) = span {
                diag = diag.span(span);
            }
            diag = match nearest(name, allowed) {
                Some(suggestion) => diag.note(format!("did you mean `{suggestion}`?")),
                None => diag.note(format!("known keys here: {}", allowed.join(", "))),
            };
            self.diags.push(diag);
        }
    }

    fn wrong_type(
        &mut self,
        t: &dyn TableLike,
        key: &str,
        item: &Item,
        expected: &str,
        path: &str,
    ) {
        let where_ = if path.is_empty() || path == key {
            format!("`{key}`")
        } else {
            format!("`{path}.{key}`")
        };
        let mut diag = ConfigDiagnostic::error(
            ConfigCode::WrongType,
            format!("{where_} must be {expected}, found {}", item.type_name()),
        )
        .at(self.path);
        if let Some(span) = self.span_of(self.key_range(t, key).or_else(|| item.span())) {
            diag = diag.span(span);
        }
        self.diags.push(diag);
    }

    /// The span to report for a value: the value itself when the parser kept one,
    /// otherwise its key. Dotted and inline forms do not always carry a value
    /// range, and a key-level span is still the right line.
    fn value_span(&self, t: &dyn TableLike, key: &str, item: &Item) -> Span {
        self.span_of(item.span().or_else(|| self.key_range(t, key)))
            .unwrap_or(Span::FILE_START)
    }

    fn key_range(&self, t: &dyn TableLike, key: &str) -> Option<std::ops::Range<usize>> {
        t.key(key).and_then(|k| k.span())
    }

    fn span_of(&self, range: Option<std::ops::Range<usize>>) -> Option<Span> {
        range.map(|r| Span::from_range(self.text, r))
    }
}

/// Spellings of "turn hot reload back on" that a release profile must not accept.
///
/// An explicit list rather than a substring match: `hot_reload_port` is a
/// plausible future dev-profile key, and a substring test would reject it for the
/// wrong reason.
const HOT_RELOAD_SYNONYMS: &[&str] = &[
    "hot_reload",
    "hot-reload",
    "hotreload",
    "live_reload",
    "live-reload",
    "live_editing",
    "live-editing",
    "dev_runtime",
    "dev-runtime",
];

/// The closest allowed key to `name`, if one is within a plausible typo distance.
///
/// The threshold scales with length, so `abc` cannot "correct" to an unrelated
/// three-letter key while `package_manager` still absorbs a slip or two. A guess
/// that is not close is worse than no guess: it sends the reader to fix the wrong
/// key.
pub fn nearest<'k>(name: &str, allowed: &[&'k str]) -> Option<&'k str> {
    let budget = (name.len() / 4).clamp(1, 3);
    allowed
        .iter()
        .map(|candidate| (typo_distance(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= budget)
        .min_by_key(|(distance, candidate)| (*distance, candidate.len()))
        .map(|(_, candidate)| candidate)
}

/// Optimal string alignment distance over bytes: insertion, deletion,
/// substitution, and transposition of two adjacent characters.
///
/// Transposition counts as one edit rather than two, which is the whole reason this
/// is not plain Levenshtein — `naem` for `name` and `pacakge` for `package` are the
/// most common real typos, and a Levenshtein budget tight enough to avoid bad
/// guesses would miss both.
///
/// Keys are ASCII identifiers, so bytes and characters agree; three rows of scratch
/// is all the matrix these lengths need.
fn typo_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    // `two_back` holds row i-2, needed only by the transposition case.
    let mut two_back = vec![0usize; b.len() + 1];
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for i in 0..a.len() {
        current[0] = i + 1;
        for j in 0..b.len() {
            let cost = usize::from(a[i] != b[j]);
            let mut best = (previous[j] + cost)
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
            if i > 0 && j > 0 && a[i] == b[j - 1] && a[i - 1] == b[j] {
                best = best.min(two_back[j - 1] + 1);
            }
            current[j + 1] = best;
        }
        std::mem::swap(&mut two_back, &mut previous);
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Manifest, Vec<ConfigDiagnostic>) {
        match Manifest::parse("Viso.toml", text) {
            ParseOutcome::Parsed { manifest, warnings } => (manifest, warnings),
            ParseOutcome::Failed(diags) => panic!("expected a parse, got {diags:?}"),
        }
    }

    fn errors(text: &str) -> Vec<ConfigDiagnostic> {
        match Manifest::parse("Viso.toml", text) {
            ParseOutcome::Parsed { warnings, .. } => {
                panic!("expected failure, got warnings {warnings:?}")
            }
            ParseOutcome::Failed(diags) => diags,
        }
    }

    /// The minimal documented manifest, verbatim.
    #[test]
    fn the_minimal_manifest_parses_with_no_diagnostics() {
        let (manifest, warnings) = parse(
            r#"
[package]
name = "hello-viso"
bundle_id = "com.example.hello"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(manifest.require_name().unwrap(), "hello-viso");
        assert_eq!(
            manifest.package.bundle_id.as_ref().unwrap().value,
            "com.example.hello"
        );
    }

    /// Every documented table in one file, including the ones whose commands are
    /// still to come. The assertion that matters most is `warnings.is_empty()`: a
    /// project configured for them must not be told its configuration is unknown.
    #[test]
    fn every_documented_table_parses_including_deferred_ones() {
        let (m, warnings) = parse(
            r#"
[package]
name = "app"
bundle_id = "com.example.app"
version = "0.2.0"
language = "1.0"
capabilities = ["storage.persist"]

[package.ios]
team_id = "ABCDE12345"

[profile.dev]
opt_level = 0
source_maps = true

[profile.release]
opt_level = 3

[profile.shipping]
opt_level = "size"
strip = true

[web]
default_target = "web-dom"

[web.serve]
port = 8080
open = true

[target.android]
min_sdk = 26

[target.ios]
minimum_os = "17.0"

[export.html]
out_dir = "dist-html"

[export.solid]
out_dir = "web-solid"
package_manager = "pnpm"

[workspace]
members = ["apps/one", "apps/two"]
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");

        assert_eq!(m.package.version.as_ref().unwrap().value, "0.2.0");
        assert_eq!(m.package.language.as_ref().unwrap().value, "1.0");
        assert_eq!(m.package.ios.team_id.as_ref().unwrap().value, "ABCDE12345");

        assert_eq!(m.profiles.dev.opt_level.unwrap().value, OptLevel::Zero);
        assert!(m.profiles.dev.source_maps.unwrap().value);
        assert_eq!(m.profiles.release.opt_level.unwrap().value, OptLevel::Three);
        // `"size"` and `3` are both accepted forms and land on one type.
        assert_eq!(m.profiles.shipping.opt_level.unwrap().value, OptLevel::Size);
        assert!(m.profiles.shipping.strip.unwrap().value);
        assert_eq!(
            m.profiles.get(Profile::Release).opt_level.unwrap().value,
            OptLevel::Three
        );

        assert_eq!(m.web.default_target.unwrap().value, Target::WebDom);
        assert_eq!(m.web.serve.port.unwrap().value, 8080);
        assert!(m.web.serve.open.unwrap().value);

        assert_eq!(m.targets.android.min_sdk.unwrap().value, 26);
        assert_eq!(m.targets.ios.minimum_os.as_ref().unwrap().value, "17.0");

        assert_eq!(m.export.html.out_dir.as_ref().unwrap().value, "dist-html");
        assert_eq!(
            m.export.solid.package_manager.as_ref().unwrap().value,
            "pnpm"
        );

        let members: Vec<&str> = m
            .workspace
            .members
            .iter()
            .map(|s| s.value.as_str())
            .collect();
        assert_eq!(members, ["apps/one", "apps/two"]);
        assert_eq!(m.package.capabilities[0].value, "storage.persist");
    }

    /// Spans are the reason this module uses `toml_edit`. If they were wrong, every
    /// downstream provenance answer would point at the wrong line and nothing else
    /// would notice.
    #[test]
    fn every_value_carries_the_line_it_was_written_on() {
        let text = "[package]\nname = \"app\"\n\n[web]\ndefault_target = \"web-gpu\"\n";
        let (m, _) = parse(text);
        assert_eq!(m.package.name.as_ref().unwrap().span.line, 2);
        assert_eq!(m.web.default_target.unwrap().span.line, 5);
    }

    /// The hard rule of profiles. This is the one configuration mistake that must be
    /// an error rather than a warning: silently honoring it produces a shipped
    /// binary that accepts remote patches.
    #[test]
    fn hot_reload_in_a_shipping_profile_is_an_error() {
        for profile in ["release", "shipping"] {
            for synonym in HOT_RELOAD_SYNONYMS {
                let text =
                    format!("[package]\nname = \"a\"\n\n[profile.{profile}]\n{synonym} = true\n");
                let diags = errors(&text);
                let hit = diags
                    .iter()
                    .find(|d| d.code == ConfigCode::HotReloadInRelease)
                    .unwrap_or_else(|| panic!("{profile}/{synonym} was accepted: {diags:?}"));
                assert!(hit.is_error());
                assert_eq!(hit.span.unwrap().line, 5);
                assert!(
                    hit.notes.iter().any(|n| n.contains("--profile dev")),
                    "the diagnostic must say what to do instead"
                );
            }
        }
    }

    /// The same spelling under `dev` is redundant, not dangerous — it asks for
    /// what it already has. A warning keeps the file honest without failing a
    /// build that would behave exactly as the author intended.
    #[test]
    fn hot_reload_in_the_dev_profile_is_only_a_warning() {
        let (_, warnings) = parse("[package]\nname = \"a\"\n\n[profile.dev]\nhot_reload = true\n");
        let warning = warnings
            .iter()
            .find(|d| d.message.contains("hot_reload"))
            .expect("expected a warning");
        assert!(!warning.is_error());
        assert!(warning.notes.join(" ").contains("always carries"));
    }

    #[test]
    fn an_unknown_key_is_an_error_that_suggests_the_nearest_real_one() {
        let diags = errors("[package]\nnaem = \"app\"\n");
        let diag = &diags[0];
        assert_eq!(diag.code, ConfigCode::UnknownKey);
        assert!(diag.is_error());
        assert_eq!(diag.span.unwrap().line, 2);
        assert!(diag.notes[0].contains("`name`"), "{:?}", diag.notes);

        // A key that resembles nothing gets the full list instead of a bad guess.
        let diags = errors("[package]\nname = \"app\"\nzzzzzzzz = 1\n");
        assert!(
            diags[0].notes[0].contains("known keys here"),
            "{:?}",
            diags[0].notes
        );

        // Unknown *top-level* tables are caught too, with no `path.` prefix.
        let diags = errors("[package]\nname = \"a\"\n\n[pacakge]\nx = 1\n");
        assert!(
            diags
                .iter()
                .any(|d| d.message == "unknown key `pacakge`" && d.notes[0].contains("`package`")),
            "{diags:?}"
        );

        // No key chooses the target a command builds for when it names none.
        for text in [
            "[build]\ndefault_target = \"ios\"\n",
            "[target.android]\nbackend = \"vulkan\"\n",
        ] {
            assert_eq!(errors(text)[0].code, ConfigCode::UnknownKey, "{text}");
        }
    }

    #[test]
    fn a_wrong_type_is_an_error_that_names_both_types() {
        let diags = errors("[package]\nname = 7\n");
        assert_eq!(diags[0].code, ConfigCode::WrongType);
        assert!(diags[0].message.contains("`package.name`"));
        assert!(diags[0].message.contains("must be a string"));
        assert_eq!(diags[0].span.unwrap().line, 2);

        let diags = errors("package = \"app\"\n");
        assert_eq!(diags[0].code, ConfigCode::WrongType);
        assert!(diags[0].message.contains("must be a table"));

        let diags = errors("[workspace]\nmembers = [1, 2]\n");
        assert_eq!(diags[0].code, ConfigCode::WrongType);
        assert!(diags[0].message.contains("must contain strings"));
    }

    #[test]
    fn an_invalid_value_is_an_error_that_lists_the_valid_ones() {
        let diags = errors("[web]\ndefault_target = \"playstation\"\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
        assert!(diags[0].notes[0].contains("headless"));
        assert_eq!(diags[0].span.unwrap().line, 2);

        let diags = errors("[web]\ndefault_target = \"ios\"\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
        assert!(diags[0].notes[0].contains("web-gpu"));

        let diags = errors("[profile.dev]\nopt_level = 9\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);

        let diags = errors("[web.serve]\nport = 99999\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
        assert!(diags[0].message.contains("65535"));
    }

    #[test]
    fn identity_signing_and_export_values_are_checked_where_written() {
        for (text, line) in [
            ("[package]\nbundle_id = \"app\"\n", 2),
            ("[package]\nbundle_id = \"com.my-app\"\n", 2),
            ("[package]\nversion = \"v1\"\n", 2),
            ("[package]\nversion = \"1.2.3-\"\n", 2),
            ("[package.ios]\nteam_id = \"abc\"\n", 2),
            ("[target.ios]\nminimum_os = \"latest\"\n", 2),
            ("[export.html]\nout_dir = \"../site\"\n", 2),
            ("[export.html]\nout_dir = \"/tmp/site\"\n", 2),
            ("[export.solid]\npackage_manager = \"pip\"\n", 2),
        ] {
            let diags = errors(text);
            assert_eq!(diags[0].code, ConfigCode::InvalidValue, "{text}");
            assert_eq!(diags[0].span.unwrap().line, line, "{text}");
            assert!(!diags[0].notes.is_empty(), "{text}");
        }
        let (m, warnings) = parse(
            "[package]\nbundle_id = \"com.example.App2\"\nversion = \"1.4.0-beta.1\"\n\n\
             [export.html]\nout_dir = \"./web/html\"\n",
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(m.package.version.unwrap().value, "1.4.0-beta.1");
    }

    #[test]
    fn delivery_metadata_is_read_and_checked_where_written() {
        let (m, warnings) = parse(
            r#"
[package]
name = "notes"
display_name = "Notes"
bundle_id = "com.example.notes"
version = "2.1.0"
build = 42
orientation = "portrait"

[package.icons]
source = "assets/icon.png"

[package.permissions]
camera = "Scans receipts into notes"
notifications = "Reminds you of a note"

[package.ios]
team_id = "ABCDE12345"
identity = "Apple Distribution: Example (ABCDE12345)"
provisioning_profile = "signing/notes.mobileprovision"

[package.android]
keystore = "signing/release.jks"
key_alias = "upload"
abis = ["arm64-v8a", "x86_64"]

[package.macos]
identity = "Developer ID Application: Example (ABCDE12345)"

[package.windows]
identity = "Example Ltd"
timestamp_url = "http://timestamp.example.com"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let p = &m.package;
        assert_eq!(p.display_name.as_ref().unwrap().value, "Notes");
        assert_eq!(p.build.as_ref().unwrap().value, 42);
        assert_eq!(p.icon.as_ref().unwrap().value, "assets/icon.png");
        assert_eq!(p.orientation.as_ref().unwrap().value, Orientation::Portrait);
        let permissions: Vec<(Permission, &str)> = p
            .permissions
            .iter()
            .map(|(permission, reason)| (permission.value, reason.value.as_str()))
            .collect();
        assert_eq!(
            permissions,
            [
                (Permission::Camera, "Scans receipts into notes"),
                (Permission::Notifications, "Reminds you of a note")
            ]
        );
        assert_eq!(p.permissions[0].0.span.line, 14, "the key is the span");
        assert_eq!(
            p.ios.provisioning_profile.as_ref().unwrap().value,
            "signing/notes.mobileprovision"
        );
        assert_eq!(p.android.key_alias.as_ref().unwrap().value, "upload");
        assert_eq!(p.android.abis.len(), 2);
        assert!(p.macos.identity.is_some());
        assert_eq!(p.windows.identity.as_ref().unwrap().value, "Example Ltd");

        for (text, line) in [
            ("[package]\nbuild = 0\n", 2),
            ("[package]\nbuild = 2100000001\n", 2),
            ("[package]\ndisplay_name = \"\"\n", 2),
            (
                "[package]\ndisplay_name = \"A name far too long for any home screen\"\n",
                2,
            ),
            ("[package]\norientation = \"upside-down\"\n", 2),
            ("[package.permissions]\ncamera = \" \"\n", 2),
            ("[package.android]\nabis = [\"mips\"]\n", 2),
            ("[package.windows]\ntimestamp_url = \"timestamp\"\n", 2),
        ] {
            let diags = errors(text);
            assert_eq!(diags[0].code, ConfigCode::InvalidValue, "{text}");
            assert_eq!(diags[0].span.unwrap().line, line, "{text}");
            assert!(!diags[0].notes.is_empty(), "{text}");
        }
        let diags = errors("[package.permissions]\ncamra = \"Scans\"\n");
        assert_eq!(diags[0].code, ConfigCode::UnknownKey);
        assert_eq!(diags[0].notes, ["did you mean `camera`?"]);
    }

    #[test]
    fn an_unknown_profile_table_is_an_error() {
        let diags = errors("[profile.fast]\nopt_level = 2\n");
        assert_eq!(diags[0].code, ConfigCode::UnknownProfile);
        assert!(diags[0].message.contains("`fast`"));
        assert_eq!(diags[0].span.unwrap().line, 1);
    }

    #[test]
    fn broken_toml_reports_a_syntax_error_with_a_position() {
        let diags = errors("[package\nname = \"a\"\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].code, ConfigCode::ManifestSyntax);
        assert!(diags[0].span.is_some());
        assert!(!diags[0].message.is_empty());
    }

    /// An absent manifest field stays `None`. Substituting a default here would
    /// erase the difference between a user's choice and the framework's, which is
    /// the whole point of tracking provenance.
    #[test]
    fn absence_is_absence_not_a_default() {
        let (m, warnings) = parse("[package]\nname = \"a\"\n");
        assert!(warnings.is_empty());
        assert!(m.web.default_target.is_none());
        assert!(m.profiles.release.opt_level.is_none());
        assert!(m.profiles.dev.strip.is_none());
        assert!(m.web.serve.port.is_none());
        assert!(m.workspace.members.is_empty());
    }

    #[test]
    fn a_missing_name_is_reported_where_it_is_needed() {
        let (m, _) = parse("[web]\ndefault_target = \"web-gpu\"\n");
        let diag = m.require_name().unwrap_err();
        assert_eq!(diag.code, ConfigCode::MissingRequired);
        assert_eq!(diag.code.exit_code(), 1);
    }

    /// Inline and dotted spellings are the same configuration, so they must parse
    /// to the same manifest — a schema that only understood `[web.serve]` would
    /// reject a valid TOML file.
    #[test]
    fn inline_and_dotted_forms_are_equivalent_to_tables() {
        let (a, _) = parse("[package]\nname = \"a\"\n\n[web.serve]\nport = 3000\n");
        // A root-level inline table has to precede the first header, or it would
        // belong to `[package]` instead.
        let (b, _) = parse("web = { serve = { port = 3000 } }\n\n[package]\nname = \"a\"\n");
        let (c, _) = parse("[package]\nname = \"a\"\n\n[web]\nserve.port = 3000\n");
        assert_eq!(a.web.serve.port.unwrap().value, 3000);
        assert_eq!(b.web.serve.port.unwrap().value, 3000);
        assert_eq!(c.web.serve.port.unwrap().value, 3000);
    }

    #[test]
    fn typo_distance_matches_hand_computed_values() {
        assert_eq!(typo_distance("", "abc"), 3);
        assert_eq!(typo_distance("abc", ""), 3);
        assert_eq!(typo_distance("name", "name"), 0);
        assert_eq!(typo_distance("nam", "name"), 1, "deletion");
        assert_eq!(
            typo_distance("naem", "name"),
            1,
            "transposition is one edit"
        );
        assert_eq!(typo_distance("pacakge", "package"), 1, "transposition");
        assert_eq!(typo_distance("kitten", "sitting"), 3);

        // The budget scales with length: a short key does not get corrected to an
        // unrelated short key two edits away.
        assert_eq!(nearest("nam", &["name", "version"]), Some("name"));
        assert_eq!(nearest("abc", &["xyz"]), None);
        assert_eq!(
            nearest("package_manger", &["out_dir", "package_manager"]),
            Some("package_manager")
        );
    }

    #[test]
    fn the_test_surface_is_a_size_and_a_scale() {
        let (m, _) = parse(
            "[package]\nname = \"g\"\n[test.surface]\nwidth = 390\nheight = 844\nscale = 3\n",
        );
        let (w, h, scale) = m.test.surface.resolved();
        assert_eq!((w, h, scale.get()), (390, 844, 3.0));
        let (m, _) = parse("[package]\nname = \"g\"\n[test.surface]\nscale = 2.625\n");
        let (w, h, scale) = m.test.surface.resolved();
        assert_eq!((w, h, scale.get()), (1280, 720, 2.625));
        let (m, _) = parse("[package]\nname = \"g\"\n");
        assert_eq!(m.test.surface.resolved().2, SurfaceScale::ONE);
        for bad in [
            "width = 0",
            "height = 20000",
            "scale = 0",
            "scale = 9.5",
            "scale = \"2\"",
            "depth = 1",
        ] {
            let diags = errors(&format!("[package]\nname = \"g\"\n[test.surface]\n{bad}\n"));
            assert_eq!(diags.len(), 1, "{bad}: {diags:?}");
        }
    }

    #[test]
    fn game_determinism_is_one_of_two_tiers() {
        let (m, warnings) =
            parse("[package]\nname = \"g\"\n[game]\ndeterminism = \"cross_platform\"\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            m.game.determinism.unwrap().value,
            GameDeterminism::CrossPlatform
        );
        let (m, _) = parse("[package]\nname = \"g\"\n");
        assert!(m.game.determinism.is_none());
        let diags = errors("[package]\nname = \"g\"\n[game]\ndeterminism = \"exact\"\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
    }

    #[test]
    fn game_collision_delivery_is_event_or_listener_major() {
        let (m, warnings) =
            parse("[package]\nname = \"g\"\n[game]\ncollision_delivery = \"listener_major\"\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            m.game.collision_delivery.unwrap().value,
            CollisionDelivery::ListenerMajor
        );
        let diags = errors("[package]\nname = \"g\"\n[game]\ncollision_delivery = \"random\"\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
    }

    #[test]
    fn game_tick_rate_is_from_1_to_1000_hz() {
        let (m, warnings) = parse("[package]\nname = \"g\"\n[game]\ntick_rate = 30\n");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(m.game.tick_rate.unwrap().value, 30);
        for bad in ["0", "1001", "-5"] {
            let diags = errors(&format!(
                "[package]\nname = \"g\"\n[game]\ntick_rate = {bad}\n"
            ));
            assert_eq!(diags[0].code, ConfigCode::InvalidValue, "{bad}");
        }
    }
}
