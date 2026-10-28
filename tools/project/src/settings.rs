//! Every configuration key of a project, its value and the layer it came from:
//! what `viso config show` lists and `viso config get` looks up.
//!
//! The build keys (`profile`, `opt_level`, `source_maps`, `strip`) resolve
//! through every layer; `profile.<name>.<key>` is what a build with that profile
//! would get, `profile.<name>.web.<key>` what a browser build with it would; every other key is the manifest's value or the framework's
//! default. A key with no default and no value is [`Value::Unset`], listed all
//! the same, so the list is the whole schema.

use std::fmt;

use crate::config::{Env, Origin, ProfileSettings, ResolvedConfig};
use crate::discovery::Project;
use crate::manifest::{AndroidGpu, Spanned, SurfaceScale, TestSurface};
use crate::target::Profile;

/// The framework's defaults for keys whose consumers live outside this crate.
/// Each consumer's own constant is checked against these where both are in
/// reach, so `config show` never reports a default nothing uses.
pub mod defaults {
    /// `web.serve.port`.
    pub const SERVE_PORT: u16 = 8000;
    /// `web.serve.open`.
    pub const SERVE_OPEN: bool = false;
    /// `game.tick_rate`.
    pub const TICK_RATE: u32 = 60;
    /// `i18n.source`.
    pub const I18N_SOURCE: &str = "en";
    /// `target.android.min_sdk`.
    pub const ANDROID_MIN_SDK: u32 = 26;
    /// `target.ios.minimum_os`.
    pub const IOS_MINIMUM_OS: &str = "15.0";
}

/// A configuration value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// A string or a name.
    Text(String),
    /// A number.
    Integer(i64),
    /// A number with a fraction; never NaN.
    Number(f64),
    /// A boolean.
    Bool(bool),
    /// A list of strings.
    List(Vec<String>),
    /// Nothing set, and no default.
    Unset,
}

impl fmt::Display for Value {
    /// The human form: lists in brackets, an unset value as `(unset)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Text(text) => f.write_str(text),
            Value::Integer(n) => write!(f, "{n}"),
            Value::Number(n) => write!(f, "{n}"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::List(items) => write!(f, "[{}]", items.join(", ")),
            Value::Unset => f.write_str("(unset)"),
        }
    }
}

// No value is NaN, so equality is total.
impl Eq for Value {}

/// One key of `viso config show`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The dotted key name.
    pub key: &'static str,
    /// The value in effect.
    pub value: Value,
    /// Where it came from.
    pub origin: Origin,
}

impl fmt::Display for Entry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} = {} ({})", self.key, self.value, self.origin)
    }
}

/// Every key, in the order [`settings`] lists them.
pub const KEYS: &[&str] = &[
    "profile",
    "opt_level",
    "source_maps",
    "strip",
    "dev_runtime",
    "package.name",
    "package.bundle_id",
    "package.version",
    "package.language",
    "package.capabilities",
    "package.display_name",
    "package.build",
    "package.icons.source",
    "package.orientation",
    "package.permissions",
    "package.ios.team_id",
    "package.ios.identity",
    "package.ios.provisioning_profile",
    "package.android.keystore",
    "package.android.key_alias",
    "package.android.abis",
    "package.macos.identity",
    "package.windows.identity",
    "package.windows.timestamp_url",
    "package.windows.publisher",
    "profile.dev.opt_level",
    "profile.dev.source_maps",
    "profile.dev.strip",
    "profile.dev.web.optimize",
    "profile.dev.web.strip",
    "profile.dev.web.source_maps",
    "profile.dev.web.brotli",
    "profile.dev.web.gzip",
    "profile.dev.web.split",
    "profile.dev.web.threads",
    "profile.release.opt_level",
    "profile.release.source_maps",
    "profile.release.strip",
    "profile.release.web.optimize",
    "profile.release.web.strip",
    "profile.release.web.source_maps",
    "profile.release.web.brotli",
    "profile.release.web.gzip",
    "profile.release.web.split",
    "profile.release.web.threads",
    "profile.shipping.opt_level",
    "profile.shipping.source_maps",
    "profile.shipping.strip",
    "profile.shipping.web.optimize",
    "profile.shipping.web.strip",
    "profile.shipping.web.source_maps",
    "profile.shipping.web.brotli",
    "profile.shipping.web.gzip",
    "profile.shipping.web.split",
    "profile.shipping.web.threads",
    "web.default_target",
    "web.serve.port",
    "web.serve.open",
    "target.android.min_sdk",
    "target.android.gpu",
    "target.ios.minimum_os",
    "export.html.out_dir",
    "export.solid.out_dir",
    "export.solid.package_manager",
    "game.determinism",
    "game.tick_rate",
    "game.collision_delivery",
    "test.surface.width",
    "test.surface.height",
    "test.surface.scale",
    "i18n.source",
    "images.convert",
    "images.quality",
    "images.svg",
    "images.svg_scale",
    "images.fit",
    "workspace.members",
];

/// Every key of `project` as `config` resolved it, in [`KEYS`] order.
///
/// `env` is the environment `config` was resolved with; having resolved, none
/// of its variables fails to parse.
pub fn settings(project: &Project, config: &ResolvedConfig, env: &Env) -> Vec<Entry> {
    let m = &project.manifest;
    let mut entries = Vec::with_capacity(KEYS.len());
    let mut push = |key: &'static str, value: Value, origin: Origin| {
        entries.push(Entry { key, value, origin });
    };

    let text = |v: &dyn fmt::Display| Value::Text(v.to_string());
    let active = &config.settings;
    push(
        "profile",
        text(&config.profile.value),
        config.profile.origin,
    );
    push(
        "opt_level",
        text(&active.opt_level.value),
        active.opt_level.origin,
    );
    push(
        "source_maps",
        Value::Bool(active.source_maps.value),
        active.source_maps.origin,
    );
    push(
        "strip",
        Value::Bool(active.strip.value),
        active.strip.origin,
    );
    // Derived from the profile: no layer can name it.
    push(
        "dev_runtime",
        text(&config.dev_runtime()),
        Origin::FrameworkDefault,
    );

    let package = &m.package;
    push_text(&mut push, "package.name", &package.name, None);
    push_text(&mut push, "package.bundle_id", &package.bundle_id, None);
    push_text(&mut push, "package.version", &package.version, None);
    push_text(&mut push, "package.language", &package.language, None);
    push_list(&mut push, "package.capabilities", &package.capabilities);
    push_text(
        &mut push,
        "package.display_name",
        &package.display_name,
        package.name.as_ref().map(|n| n.value.as_str()),
    );
    push_value(
        &mut push,
        "package.build",
        package
            .build
            .as_ref()
            .map(|b| b.as_ref().map(|n| Value::Integer(i64::from(*n)))),
        Value::Integer(1),
    );
    push_text(&mut push, "package.icons.source", &package.icon, None);
    push_value(
        &mut push,
        "package.orientation",
        package
            .orientation
            .as_ref()
            .map(|o| o.as_ref().map(|o| Value::Text(o.as_str().into()))),
        Value::Text(crate::Orientation::Any.as_str().into()),
    );
    let permissions: Vec<Spanned<String>> = package
        .permissions
        .iter()
        .map(|(permission, _)| permission.as_ref().map(|p| p.as_str().to_owned()))
        .collect();
    push_list(&mut push, "package.permissions", &permissions);
    let ios = &package.ios;
    push_text(&mut push, "package.ios.team_id", &ios.team_id, None);
    push_text(&mut push, "package.ios.identity", &ios.identity, None);
    push_text(
        &mut push,
        "package.ios.provisioning_profile",
        &ios.provisioning_profile,
        None,
    );
    let android = &package.android;
    push_text(
        &mut push,
        "package.android.keystore",
        &android.keystore,
        None,
    );
    push_text(
        &mut push,
        "package.android.key_alias",
        &android.key_alias,
        None,
    );
    if android.abis.is_empty() {
        push(
            "package.android.abis",
            Value::List(vec!["arm64-v8a".into()]),
            Origin::FrameworkDefault,
        );
    } else {
        push_list(&mut push, "package.android.abis", &android.abis);
    }
    push_text(
        &mut push,
        "package.macos.identity",
        &package.macos.identity,
        None,
    );
    let windows = &package.windows;
    push_text(
        &mut push,
        "package.windows.identity",
        &windows.identity,
        None,
    );
    push_text(
        &mut push,
        "package.windows.timestamp_url",
        &windows.timestamp_url,
        None,
    );
    push_text(
        &mut push,
        "package.windows.publisher",
        &windows.publisher,
        None,
    );

    for (profile, keys) in Profile::ALL.iter().zip(PROFILE_KEYS) {
        let mut errors = Vec::new();
        let settings = ProfileSettings::resolve(m, *profile, env, &mut errors);
        debug_assert!(errors.is_empty(), "the environment resolved once already");
        push(
            keys[0],
            text(&settings.opt_level.value),
            settings.opt_level.origin,
        );
        push(
            keys[1],
            Value::Bool(settings.source_maps.value),
            settings.source_maps.origin,
        );
        push(
            keys[2],
            Value::Bool(settings.strip.value),
            settings.strip.origin,
        );
        let web = &settings.web;
        push(
            keys[3],
            text(&web.optimize.value.as_str()),
            web.optimize.origin,
        );
        for (key, value) in [
            (keys[4], web.strip),
            (keys[5], web.source_maps),
            (keys[6], web.brotli),
            (keys[7], web.gzip),
        ] {
            push(key, Value::Bool(value.value), value.origin);
        }
        push(keys[8], text(&web.split.value.as_str()), web.split.origin);
        push(
            keys[9],
            text(&web.threads.value.as_str()),
            web.threads.origin,
        );
    }

    push_value(
        &mut push,
        "web.default_target",
        m.web.default_target.map(|t| t.map(|t| text(&t))),
        Value::Unset,
    );
    push_value(
        &mut push,
        "web.serve.port",
        m.web
            .serve
            .port
            .map(|p| p.map(|p| Value::Integer(p.into()))),
        Value::Integer(defaults::SERVE_PORT.into()),
    );
    push_value(
        &mut push,
        "web.serve.open",
        m.web.serve.open.map(|o| o.map(Value::Bool)),
        Value::Bool(defaults::SERVE_OPEN),
    );
    push_value(
        &mut push,
        "target.android.min_sdk",
        m.targets
            .android
            .min_sdk
            .map(|l| l.map(|l| Value::Integer(l.into()))),
        Value::Integer(defaults::ANDROID_MIN_SDK.into()),
    );
    push_value(
        &mut push,
        "target.android.gpu",
        m.targets
            .android
            .gpu
            .map(|g| g.map(|g| Value::Text(g.as_str().to_owned()))),
        Value::Text(AndroidGpu::Auto.as_str().to_owned()),
    );
    push_text(
        &mut push,
        "target.ios.minimum_os",
        &m.targets.ios.minimum_os,
        Some(defaults::IOS_MINIMUM_OS),
    );
    push_text(
        &mut push,
        "export.html.out_dir",
        &m.export.html.out_dir,
        None,
    );
    push_text(
        &mut push,
        "export.solid.out_dir",
        &m.export.solid.out_dir,
        None,
    );
    push_text(
        &mut push,
        "export.solid.package_manager",
        &m.export.solid.package_manager,
        None,
    );
    push_value(
        &mut push,
        "game.determinism",
        m.game.determinism.map(|d| d.map(|d| text(&d.as_str()))),
        Value::Text(crate::GameDeterminism::default().as_str().into()),
    );
    push_value(
        &mut push,
        "game.tick_rate",
        m.game
            .tick_rate
            .map(|r| r.map(|r| Value::Integer(r.into()))),
        Value::Integer(defaults::TICK_RATE.into()),
    );
    push_value(
        &mut push,
        "game.collision_delivery",
        m.game
            .collision_delivery
            .map(|d| d.map(|d| text(&d.as_str()))),
        Value::Text(crate::CollisionDelivery::default().as_str().into()),
    );
    let surface = &m.test.surface;
    push_value(
        &mut push,
        "test.surface.width",
        surface.width.map(|w| w.map(|w| Value::Integer(w.into()))),
        Value::Integer(TestSurface::WIDTH.into()),
    );
    push_value(
        &mut push,
        "test.surface.height",
        surface.height.map(|h| h.map(|h| Value::Integer(h.into()))),
        Value::Integer(TestSurface::HEIGHT.into()),
    );
    push_value(
        &mut push,
        "test.surface.scale",
        surface.scale.map(|s| s.map(|s| Value::Number(s.get()))),
        Value::Number(SurfaceScale::ONE.get()),
    );
    push_text(
        &mut push,
        "i18n.source",
        &m.i18n.source,
        Some(defaults::I18N_SOURCE),
    );
    let images = &m.images;
    let flag = |b: Option<Spanned<bool>>| b.map(|b| b.map(Value::Bool));
    push_value(
        &mut push,
        "images.convert",
        flag(images.convert),
        Value::Bool(true),
    );
    push_value(
        &mut push,
        "images.quality",
        images.quality.map(|q| q.map(|q| text(&q.as_str()))),
        Value::Text(crate::ImageQuality::High.as_str().into()),
    );
    push_value(
        &mut push,
        "images.svg",
        images.svg.map(|v| {
            v.map(|v| match v {
                crate::SvgImages::Vector => text(&"vector"),
                crate::SvgImages::Raster => text(&"raster"),
            })
        }),
        Value::Text("vector".into()),
    );
    // The target's surface scale when unset.
    push_value(
        &mut push,
        "images.svg_scale",
        images.svg_scale.map(|s| s.map(|s| Value::Number(s.get()))),
        Value::Unset,
    );
    push_value(&mut push, "images.fit", flag(images.fit), Value::Bool(true));
    push_list(&mut push, "workspace.members", &m.workspace.members);

    debug_assert!(entries.iter().map(|e| e.key).eq(KEYS.iter().copied()));
    entries
}

/// One key of `project` as `config` resolved it.
pub fn setting(project: &Project, config: &ResolvedConfig, env: &Env, key: &str) -> Option<Entry> {
    settings(project, config, env)
        .into_iter()
        .find(|e| e.key == key)
}

/// The keys of each profile's table, in [`Profile::ALL`] order.
const PROFILE_KEYS: [[&str; 10]; 3] = [
    [
        "profile.dev.opt_level",
        "profile.dev.source_maps",
        "profile.dev.strip",
        "profile.dev.web.optimize",
        "profile.dev.web.strip",
        "profile.dev.web.source_maps",
        "profile.dev.web.brotli",
        "profile.dev.web.gzip",
        "profile.dev.web.split",
        "profile.dev.web.threads",
    ],
    [
        "profile.release.opt_level",
        "profile.release.source_maps",
        "profile.release.strip",
        "profile.release.web.optimize",
        "profile.release.web.strip",
        "profile.release.web.source_maps",
        "profile.release.web.brotli",
        "profile.release.web.gzip",
        "profile.release.web.split",
        "profile.release.web.threads",
    ],
    [
        "profile.shipping.opt_level",
        "profile.shipping.source_maps",
        "profile.shipping.strip",
        "profile.shipping.web.optimize",
        "profile.shipping.web.strip",
        "profile.shipping.web.source_maps",
        "profile.shipping.web.brotli",
        "profile.shipping.web.gzip",
        "profile.shipping.web.split",
        "profile.shipping.web.threads",
    ],
];

type Push<'a> = dyn FnMut(&'static str, Value, Origin) + 'a;

/// A manifest value with its line, or `default`.
fn push_value(
    push: &mut Push<'_>,
    key: &'static str,
    value: Option<Spanned<Value>>,
    default: Value,
) {
    match value {
        Some(v) => push(key, v.value, Origin::ManifestDefault { span: v.span }),
        None => push(key, default, Origin::FrameworkDefault),
    }
}

fn push_text(
    push: &mut Push<'_>,
    key: &'static str,
    value: &Option<Spanned<String>>,
    default: Option<&str>,
) {
    push_value(
        push,
        key,
        value
            .as_ref()
            .map(|v| v.as_ref().map(|t| Value::Text(t.clone()))),
        default.map_or(Value::Unset, |d| Value::Text(d.into())),
    );
}

/// A list, placed at its first item; an empty one is the default.
fn push_list(push: &mut Push<'_>, key: &'static str, items: &[Spanned<String>]) {
    let value = Value::List(items.iter().map(|i| i.value.clone()).collect());
    match items.first() {
        Some(first) => push(key, value, Origin::ManifestDefault { span: first.span }),
        None => push(key, value, Origin::FrameworkDefault),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ENV_OPT_LEVEL, Overrides, resolve};
    use crate::scratch::Scratch;
    use crate::target::{ArtifactKind, Target};

    fn listed(manifest: &str, flags: &Overrides, env: &Env) -> Vec<Entry> {
        let s = Scratch::new("settings");
        s.write("Viso.toml", manifest);
        let project = Project::discover(s.path()).unwrap();
        let config = resolve(&project, Target::Host, ArtifactKind::Build, flags, env)
            .unwrap()
            .config;
        settings(&project, &config, env)
    }

    fn by_key<'a>(entries: &'a [Entry], key: &str) -> &'a Entry {
        entries.iter().find(|e| e.key == key).expect(key)
    }

    #[test]
    fn every_key_is_listed_once_in_order() {
        let entries = listed(
            "[package]\nname = \"app\"\n",
            &Overrides::default(),
            &Env::empty(),
        );
        let keys: Vec<&str> = entries.iter().map(|e| e.key).collect();
        assert_eq!(keys, KEYS);
        assert_eq!(
            by_key(&entries, "package.name").value,
            Value::Text("app".into())
        );
        assert_eq!(by_key(&entries, "package.bundle_id").value, Value::Unset);
        assert_eq!(
            by_key(&entries, "web.serve.port").to_string(),
            "web.serve.port = 8000 (default)"
        );
        assert_eq!(
            by_key(&entries, "workspace.members").value,
            Value::List(Vec::new())
        );
    }

    /// Each profile's keys say what a build with that profile gets, the
    /// environment included, whichever profile is active.
    #[test]
    fn each_profile_lists_what_a_build_with_it_gets() {
        let manifest = "\
[package]
name = \"app\"

[profile.release]
strip = true
";
        let env = Env::from_pairs([(ENV_OPT_LEVEL, "1")]);
        let entries = listed(manifest, &Overrides::default(), &env);
        assert_eq!(by_key(&entries, "profile").value, Value::Text("dev".into()));
        assert_eq!(
            by_key(&entries, "dev_runtime").value,
            Value::Text("present".into())
        );
        let strip = by_key(&entries, "profile.release.strip");
        assert_eq!(strip.value, Value::Bool(true));
        assert_eq!(strip.origin.to_string(), "Viso.toml [profile.release]:5:9");
        assert_eq!(by_key(&entries, "strip").value, Value::Bool(false));
        for key in ["opt_level", "profile.shipping.opt_level"] {
            let entry = by_key(&entries, key);
            assert_eq!(entry.value, Value::Text("1".into()), "{key}");
            assert_eq!(entry.origin, Origin::Env(ENV_OPT_LEVEL), "{key}");
        }
        assert_eq!(
            by_key(&entries, "profile.shipping.source_maps").origin,
            Origin::FrameworkDefault
        );
    }

    #[test]
    fn a_manifest_value_points_at_its_line() {
        let manifest = "\
[package]
name = \"app\"
capabilities = [\"storage.persist\"]

[game]
tick_rate = 120
";
        let entries = listed(manifest, &Overrides::default(), &Env::empty());
        let rate = by_key(&entries, "game.tick_rate");
        assert_eq!(rate.value, Value::Integer(120));
        assert_eq!(rate.origin.to_string(), "Viso.toml:6:13");
        let capabilities = by_key(&entries, "package.capabilities");
        assert_eq!(capabilities.value.to_string(), "[storage.persist]");
        assert_eq!(capabilities.origin.layer(), "manifest");
        assert_eq!(
            by_key(&entries, "game.collision_delivery").value,
            Value::Text("event_major".into())
        );
    }
}
