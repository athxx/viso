//! Simulator and emulator profiles: the developer machine's virtual devices,
//! by the IDs `viso run ios|android --device` takes (`Viso_CLI.md`).
//!
//! A profile is machine state, never project configuration: it lives in one
//! file per profile under `~/.viso/ios/simulators/` or
//! `~/.viso/android/emulators/`, and names the device the toolchain knows it
//! by — a Simulator UDID, an AVD name — which never enters `Viso.toml`. The
//! default profile when `--device` is omitted comes from the project's
//! VCS-ignored `.viso/local.toml`, then the machine's
//! `~/.viso/<platform>/config.toml`, then the only profile there is
//! ([`DeviceStore::select`]).
//!
//! ```toml
//! # ~/.viso/ios/simulators/iphone-local.toml
//! preset = "phone"
//! system = "com.apple.CoreSimulator.SimRuntime.iOS-26-3"
//! device = "5CFA3D6A-6721-4469-8CAD-95B06B4AC5AD"
//!
//! # <project>/.viso/local.toml
//! [ios]
//! device = "iphone-local"
//! ```

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, value};

/// The platform of a virtual device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    /// An Apple Simulator.
    Ios,
    /// An Android Emulator.
    Android,
}

impl Platform {
    /// The name in commands, events and paths.
    pub const fn as_str(self) -> &'static str {
        match self {
            Platform::Ios => "ios",
            Platform::Android => "android",
        }
    }

    /// What its virtual device is called: `simulator` or `emulator`.
    pub const fn device_kind(self) -> &'static str {
        match self {
            Platform::Ios => "simulator",
            Platform::Android => "emulator",
        }
    }

    /// The directory under the store its profiles are in.
    fn dir(self) -> &'static str {
        match self {
            Platform::Ios => "ios/simulators",
            Platform::Android => "android/emulators",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The form factor a profile is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Preset {
    /// A current phone.
    #[default]
    Phone,
    /// A current tablet.
    Tablet,
}

impl Preset {
    /// The name `--preset` takes.
    pub const fn as_str(self) -> &'static str {
        match self {
            Preset::Phone => "phone",
            Preset::Tablet => "tablet",
        }
    }

    /// The preset `name` names.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "phone" => Some(Preset::Phone),
            "tablet" => Some(Preset::Tablet),
            _ => None,
        }
    }
}

/// A virtual device Viso knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// The ID `--device` takes.
    pub id: String,
    /// The platform it runs.
    pub platform: Platform,
    /// Its form factor.
    pub preset: Preset,
    /// The system it runs: a Simulator runtime identifier, an Android API
    /// level.
    pub system: String,
    /// The device the toolchain knows it by: a Simulator UDID, an AVD name.
    pub device: String,
}

/// Which profile a `viso run` uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// This one.
    Profile(DeviceProfile),
    /// `--device` or a configured default names no profile.
    Unknown(String),
    /// There is no profile.
    Missing,
    /// There are several and no default.
    Ambiguous(Vec<String>),
}

/// Whether `id` can name a profile: lowercase letters, digits and `-`,
/// starting with a letter or digit, at most 40 characters.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 40
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The profiles of this machine.
#[derive(Debug, Clone)]
pub struct DeviceStore {
    root: PathBuf,
}

impl DeviceStore {
    /// The store at `$VISO_HOME`, or `~/.viso`.
    pub fn home() -> Option<Self> {
        let root = std::env::var_os("VISO_HOME")
            .map(PathBuf::from)
            .or_else(|| home_dir().map(|home| home.join(".viso")))?;
        Some(Self::at(root))
    }

    /// The store at `root`.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Its directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn file(&self, platform: Platform, id: &str) -> PathBuf {
        self.root.join(platform.dir()).join(format!("{id}.toml"))
    }

    /// The profiles of `platform`, by ID. A file that does not parse is left
    /// out.
    pub fn list(&self, platform: Platform) -> Vec<DeviceProfile> {
        let Ok(entries) = std::fs::read_dir(self.root.join(platform.dir())) else {
            return Vec::new();
        };
        let mut profiles: Vec<DeviceProfile> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let id = name.to_str()?.strip_suffix(".toml")?;
                self.get(platform, id)
            })
            .collect();
        profiles.sort_by(|a, b| a.id.cmp(&b.id));
        profiles
    }

    /// The profile `id` of `platform`.
    pub fn get(&self, platform: Platform, id: &str) -> Option<DeviceProfile> {
        if !valid_id(id) {
            return None;
        }
        let text = std::fs::read_to_string(self.file(platform, id)).ok()?;
        let doc: DocumentMut = text.parse().ok()?;
        let field = |key: &str| doc.get(key)?.as_str().map(str::to_owned);
        Some(DeviceProfile {
            id: id.to_owned(),
            platform,
            preset: Preset::parse(&field("preset")?)?,
            system: field("system")?,
            device: field("device")?,
        })
    }

    /// Writes `profile`, replacing one with its ID.
    pub fn save(&self, profile: &DeviceProfile) -> io::Result<()> {
        let path = self.file(profile.platform, &profile.id);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut doc = DocumentMut::new();
        doc["preset"] = value(profile.preset.as_str());
        doc["system"] = value(profile.system.as_str());
        doc["device"] = value(profile.device.as_str());
        let staged = path.with_extension("toml.new");
        std::fs::write(&staged, doc.to_string())?;
        std::fs::rename(&staged, &path)
    }

    /// Removes the profile `id`, saying whether there was one.
    pub fn remove(&self, platform: Platform, id: &str) -> io::Result<bool> {
        if !valid_id(id) {
            return Ok(false);
        }
        match std::fs::remove_file(self.file(platform, id)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The default profile ID of `platform` for the project at `root`: its
    /// `.viso/local.toml`, then this machine's.
    pub fn default_id(&self, platform: Platform, root: &Path) -> Option<String> {
        let read = |path: PathBuf, table: Option<&str>| {
            let doc: DocumentMut = std::fs::read_to_string(path).ok()?.parse().ok()?;
            let item = match table {
                Some(table) => doc.get(table)?.get("device")?,
                None => doc.get("device")?,
            };
            item.as_str().map(str::to_owned)
        };
        read(
            root.join(".viso").join("local.toml"),
            Some(platform.as_str()),
        )
        .or_else(|| read(self.root.join(platform.as_str()).join("config.toml"), None))
    }

    /// The profile a run of the project at `root` on `platform` uses:
    /// `requested`, the default, or the only one.
    pub fn select(&self, platform: Platform, requested: Option<&str>, root: &Path) -> Selection {
        let named = requested
            .map(str::to_owned)
            .or_else(|| self.default_id(platform, root));
        if let Some(id) = named {
            return match self.get(platform, &id) {
                Some(profile) => Selection::Profile(profile),
                None => Selection::Unknown(id),
            };
        }
        let mut profiles = self.list(platform);
        match profiles.len() {
            0 => Selection::Missing,
            1 => Selection::Profile(profiles.remove(0)),
            _ => Selection::Ambiguous(profiles.into_iter().map(|p| p.id).collect()),
        }
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(name: &str) -> (DeviceStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!("viso-devices-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        (DeviceStore::at(dir.join("home")), dir)
    }

    fn profile(id: &str, platform: Platform) -> DeviceProfile {
        DeviceProfile {
            id: id.to_owned(),
            platform,
            preset: Preset::Phone,
            system: "36".to_owned(),
            device: format!("viso_{id}"),
        }
    }

    #[test]
    fn ids_are_lowercase_words() {
        assert!(valid_id("pixel-local"));
        assert!(valid_id("ipad2"));
        assert!(!valid_id(""));
        assert!(!valid_id("-x"));
        assert!(!valid_id("Pixel"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("../x"));
        assert!(!valid_id(&"a".repeat(41)));
    }

    #[test]
    fn a_saved_profile_reads_back_and_is_listed_by_id() {
        let (store, dir) = store("roundtrip");
        let b = profile("b-phone", Platform::Android);
        let a = DeviceProfile {
            preset: Preset::Tablet,
            ..profile("a-tablet", Platform::Android)
        };
        store.save(&b).unwrap();
        store.save(&a).unwrap();
        store.save(&profile("iphone", Platform::Ios)).unwrap();
        assert_eq!(store.get(Platform::Android, "b-phone"), Some(b.clone()));
        assert_eq!(store.list(Platform::Android), vec![a, b]);
        assert_eq!(store.list(Platform::Ios).len(), 1);
        assert!(store.remove(Platform::Android, "a-tablet").unwrap());
        assert!(!store.remove(Platform::Android, "a-tablet").unwrap());
        assert_eq!(store.get(Platform::Android, "a-tablet"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn selection_takes_the_request_then_the_defaults_then_the_only_one() {
        let (store, dir) = store("select");
        let project = dir.join("project");
        std::fs::create_dir_all(project.join(".viso")).unwrap();
        let android = Platform::Android;
        assert_eq!(store.select(android, None, &project), Selection::Missing);
        store.save(&profile("one", android)).unwrap();
        assert!(matches!(
            store.select(android, None, &project),
            Selection::Profile(p) if p.id == "one"
        ));
        store.save(&profile("two", android)).unwrap();
        assert_eq!(
            store.select(android, None, &project),
            Selection::Ambiguous(vec!["one".into(), "two".into()])
        );
        assert_eq!(
            store.select(android, Some("three"), &project),
            Selection::Unknown("three".into())
        );
        std::fs::create_dir_all(store.root().join("android")).unwrap();
        std::fs::write(
            store.root().join("android/config.toml"),
            "device = \"two\"\n",
        )
        .unwrap();
        assert!(matches!(
            store.select(android, None, &project),
            Selection::Profile(p) if p.id == "two"
        ));
        std::fs::write(
            project.join(".viso/local.toml"),
            "[android]\ndevice = \"one\"\n",
        )
        .unwrap();
        assert!(matches!(
            store.select(android, None, &project),
            Selection::Profile(p) if p.id == "one"
        ));
        assert!(matches!(
            store.select(android, Some("two"), &project),
            Selection::Profile(p) if p.id == "two"
        ));
        assert_eq!(
            store.select(Platform::Ios, None, &project),
            Selection::Missing
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
