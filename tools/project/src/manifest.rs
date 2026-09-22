//! The `Viso.toml` schema — typed, total, and span-carrying.
//!
//! Two properties drive every decision in this module.
//!
//! **Total.** Every key the schema defines is listed in one place per table, and a
//! key that is not in that list is a diagnostic
//! ([`ConfigCode::UnknownKey`]) with a suggestion. Silence is the failure mode
//! this avoids: a typo'd `defualt_target` that a lenient parser skips means the
//! user's setting never took effect and nothing said so.
//!
//! **Span-carrying.** Every accepted value keeps the byte position it was parsed
//! from, wrapped in [`Spanned`]. That is the reason this crate parses with
//! `toml_edit` instead of deserializing: `viso config show` must report *where*
//! each resolved value came from (`Viso_CLI.md` section 5), and a
//! deserialize-into-struct throws the position away at exactly the moment it is
//! cheapest to keep.
//!
//! Tables for deferred phases (`[web]`, `[target.android]`, `[target.ios]`,
//! `[export.*]`) are parsed and typed *now*. Their commands do not exist yet, but a
//! project configured for them must not produce unknown-key noise that a user
//! would fix by deleting configuration the next phase needs.

use std::fmt;
use std::path::{Path, PathBuf};

use toml_edit::{Document, Item, TableLike};

use crate::diag::{ConfigCode, ConfigDiagnostic, Span};
use crate::target::{OptLevel, Profile, Target};

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

/// `[package]` — identity (`Viso_CLI.md` section 38).
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
    /// `[package.ios]` signing identity (section 38.4). Kept here rather than
    /// under `[target.ios]` because it is delivery configuration, not a build
    /// property.
    pub ios_team_id: Option<Spanned<String>>,
}

/// `[build]` — build-wide defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Build {
    /// The target used when the command line names none.
    pub default_target: Option<Spanned<Target>>,
}

/// One `[profile.X]` table (`Viso_CLI.md` section 38.2).
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

/// `[web.serve]` (`Viso_CLI.md` section 38.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebServe {
    /// Dev-server port.
    pub port: Option<Spanned<u16>>,
    /// Whether to open a browser on start.
    pub open: Option<Spanned<bool>>,
}

/// `[web]` (section 38.1). Parsed now, honored by a deferred phase.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Web {
    /// Default web target, one of the three `web-*` variants.
    pub default_target: Option<Spanned<Target>>,
    /// `[web.serve]`.
    pub serve: WebServe,
}

/// `[target.android]` (section 38.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Android {
    /// Minimum Android SDK level.
    pub min_sdk: Option<Spanned<u32>>,
    /// GPU backend name.
    pub backend: Option<Spanned<String>>,
}

/// `[target.ios]` (section 38.4).
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

/// `[export.html]` (section 38.5).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportHtml {
    /// Output directory.
    pub out_dir: Option<Spanned<String>>,
}

/// `[export.solid]` (section 38.5).
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
    /// Member directory paths, relative to the root (`Viso_CLI.md` section 4).
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
    /// `[build]`.
    pub build: Build,
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
}

impl Manifest {
    /// Parses manifest text.
    ///
    /// Returns the manifest *and* its warnings on success, and the errors on
    /// failure. Warnings never block: an unknown key in a table that is otherwise
    /// fine is worth saying and not worth refusing to build over — except where
    /// acting on the misunderstanding would ship something wrong, which is why
    /// [`ConfigCode::HotReloadInRelease`] is an error and a stray key is not.
    pub fn parse(path: impl Into<PathBuf>, text: &str) -> ParseOutcome {
        let path = path.into();
        // `Document`, not `DocumentMut`: only the immutable parse retains byte spans,
        // and spans are the entire reason this module parses rather than
        // deserializes. Nothing here edits the document.
        let doc: Document<&str> = match Document::parse(text) {
            Ok(doc) => doc,
            Err(err) => {
                let span = err.span().map(|range| Span::from_offset(text, range.start));
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
                "build",
                "profile",
                "web",
                "target",
                "export",
                "workspace",
            ],
            "",
        );

        let package = match self.table(root, "package") {
            Some(t) => {
                self.deny_unknown(t, &["name", "bundle_id", "version", "ios"], "package");
                Package {
                    name: self.string(t, "name", "package"),
                    bundle_id: self.string(t, "bundle_id", "package"),
                    version: self.string(t, "version", "package"),
                    ios_team_id: match self.table(t, "ios") {
                        Some(ios) => {
                            self.deny_unknown(ios, &["team_id"], "package.ios");
                            self.string(ios, "team_id", "package.ios")
                        }
                        None => None,
                    },
                }
            }
            None => Package::default(),
        };

        let build = match self.table(root, "build") {
            Some(t) => {
                self.deny_unknown(t, &["default_target"], "build");
                Build {
                    default_target: self.target(t, "default_target", "build"),
                }
            }
            None => Build::default(),
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
                    default_target: self.target(t, "default_target", "web"),
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
                            self.deny_unknown(a, &["min_sdk", "backend"], "target.android");
                            Android {
                                min_sdk: self.sdk_level(a, "min_sdk", "target.android"),
                                backend: self.string(a, "backend", "target.android"),
                            }
                        }
                        None => Android::default(),
                    },
                    ios: match self.table(t, "ios") {
                        Some(i) => {
                            self.deny_unknown(i, &["minimum_os"], "target.ios");
                            Ios {
                                minimum_os: self.string(i, "minimum_os", "target.ios"),
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
                                out_dir: self.string(h, "out_dir", "export.html"),
                            }
                        }
                        None => ExportHtml::default(),
                    },
                    solid: match self.table(t, "solid") {
                        Some(s) => {
                            self.deny_unknown(s, &["out_dir", "package_manager"], "export.solid");
                            ExportSolid {
                                out_dir: self.string(s, "out_dir", "export.solid"),
                                package_manager: self.string(s, "package_manager", "export.solid"),
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

        Manifest {
            path: path.to_path_buf(),
            package,
            build,
            profiles,
            web,
            targets,
            export,
            workspace,
        }
    }

    /// Reads one `[profile.X]` table.
    ///
    /// The hot-reload check lives here rather than in a generic key check because
    /// the *same spelling* means two different things depending on the profile: an
    /// error under release/shipping (`Viso_CLI.md` section 38.2), and a
    /// redundant-but-harmless warning under dev, where the Dev Runtime is present
    /// regardless.
    fn profile(&mut self, profiles: &dyn TableLike, profile: Profile) -> ProfileConfig {
        let Some(t) = self.table(profiles, profile.as_str()) else {
            return ProfileConfig::default();
        };
        let label = format!("profile.{profile}");
        self.deny_unknown_except(
            t,
            &["opt_level", "source_maps", "strip"],
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

        ProfileConfig {
            opt_level: self.opt_level(t, "opt_level", &label),
            source_maps: self.boolean(t, "source_maps", &label),
            strip: self.boolean(t, "strip", &label),
        }
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

    fn target(&mut self, t: &dyn TableLike, key: &str, path: &str) -> Option<Spanned<Target>> {
        let raw = self.string(t, key, path)?;
        match Target::parse(&raw.value) {
            Ok(target) => Some(Spanned::new(target, raw.span)),
            Err(diag) => {
                self.diags.push(diag.at(self.path).span(raw.span));
                None
            }
        }
    }

    /// `opt_level` accepts both `0` and `"size"` (`Viso_CLI.md` section 38.2 uses
    /// each form in the same example), so the integer is normalized to its name
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
            let span = self
                .span_of(value.span())
                .unwrap_or(Span { line: 1, column: 1 });
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
                ConfigDiagnostic::warning(ConfigCode::UnknownKey, format!("unknown key {where_}"))
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
            .unwrap_or(Span { line: 1, column: 1 })
    }

    fn key_range(&self, t: &dyn TableLike, key: &str) -> Option<std::ops::Range<usize>> {
        t.key(key).and_then(|k| k.span())
    }

    fn span_of(&self, range: Option<std::ops::Range<usize>>) -> Option<Span> {
        range.map(|r| Span::from_offset(self.text, r.start))
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
fn nearest<'k>(name: &str, allowed: &[&'k str]) -> Option<&'k str> {
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

    /// The minimal manifest from `Viso_CLI.md` section 38, verbatim.
    #[test]
    fn the_minimal_manifest_parses_with_no_diagnostics() {
        let (manifest, warnings) = parse(
            r#"
[package]
name = "hello-viso"
bundle_id = "com.example.hello"

[build]
default_target = "host"
"#,
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(manifest.require_name().unwrap(), "hello-viso");
        assert_eq!(
            manifest.package.bundle_id.as_ref().unwrap().value,
            "com.example.hello"
        );
        assert_eq!(manifest.build.default_target.unwrap().value, Target::Host);
    }

    /// Every table from section 38 in one file, including the ones whose commands
    /// are deferred. The assertion that matters most is `warnings.is_empty()`: a
    /// project configured for a later phase must not be told its configuration is
    /// unknown.
    #[test]
    fn every_documented_table_parses_including_deferred_ones() {
        let (m, warnings) = parse(
            r#"
[package]
name = "app"
bundle_id = "com.example.app"
version = "0.2.0"

[package.ios]
team_id = "ABCDE12345"

[build]
default_target = "host"

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
backend = "vulkan"

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
        assert_eq!(m.package.ios_team_id.as_ref().unwrap().value, "ABCDE12345");

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
        assert_eq!(m.targets.android.backend.as_ref().unwrap().value, "vulkan");
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
    }

    /// Spans are the reason this module uses `toml_edit`. If they were wrong, every
    /// downstream provenance answer would point at the wrong line and nothing else
    /// would notice.
    #[test]
    fn every_value_carries_the_line_it_was_written_on() {
        let text = "[package]\nname = \"app\"\n\n[build]\ndefault_target = \"headless\"\n";
        let (m, _) = parse(text);
        assert_eq!(m.package.name.as_ref().unwrap().span.line, 2);
        assert_eq!(m.build.default_target.unwrap().span.line, 5);
    }

    /// Section 38.2's hard rule. This is the one configuration mistake that must be
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
    fn an_unknown_key_warns_and_suggests_the_nearest_real_one() {
        let (_, warnings) = parse("[package]\nnaem = \"app\"\n");
        let warning = &warnings[0];
        assert_eq!(warning.code, ConfigCode::UnknownKey);
        assert_eq!(warning.span.unwrap().line, 2);
        assert!(warning.notes[0].contains("`name`"), "{:?}", warning.notes);

        // A key that resembles nothing gets the full list instead of a bad guess.
        let (_, warnings) = parse("[package]\nname = \"app\"\nzzzzzzzz = 1\n");
        assert!(
            warnings[0].notes[0].contains("known keys here"),
            "{:?}",
            warnings[0].notes
        );

        // Unknown *top-level* tables are caught too, with no `path.` prefix.
        let (_, warnings) = parse("[package]\nname = \"a\"\n\n[pacakge]\nx = 1\n");
        assert!(
            warnings
                .iter()
                .any(|d| d.message == "unknown key `pacakge`" && d.notes[0].contains("`package`")),
            "{warnings:?}"
        );
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
        let diags = errors("[build]\ndefault_target = \"playstation\"\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
        assert!(diags[0].notes[0].contains("headless"));
        assert_eq!(diags[0].span.unwrap().line, 2);

        let diags = errors("[profile.dev]\nopt_level = 9\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);

        let diags = errors("[web.serve]\nport = 99999\n");
        assert_eq!(diags[0].code, ConfigCode::InvalidValue);
        assert!(diags[0].message.contains("65535"));
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
        assert!(m.build.default_target.is_none());
        assert!(m.profiles.release.opt_level.is_none());
        assert!(m.profiles.dev.strip.is_none());
        assert!(m.web.serve.port.is_none());
        assert!(m.workspace.members.is_empty());
    }

    #[test]
    fn a_missing_name_is_reported_where_it_is_needed() {
        let (m, _) = parse("[build]\ndefault_target = \"host\"\n");
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
}
