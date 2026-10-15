//! The goldens snapshots are held to: `tests/snapshots/<name>.<target>.png`
//! and, beside it, `<name>.<target>.json`, the sidecar that records how it
//! was taken (the surface, the hash of its tree, the build) and the
//! tolerance it is compared under, which an update keeps.
//!
//! A snapshot that departs from its golden leaves what was found under
//! `target/viso/snapshots/`: the image taken, its tree, and a diff image of
//! where the two depart.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use viso_ende::{JsonValue, JsonWriter};
use viso_view::headless::HeadlessSurface;
use viso_view::snapshot::{Comparison, Image, Tolerance, compare, diff};

/// The directory below the project root that holds the goldens.
pub(in crate::command) const GOLDENS: &str = "tests/snapshots";

/// The version of a sidecar's keys: a key removed or changing meaning
/// raises it.
pub(crate) const SNAPSHOT_VERSION: u32 = 1;

/// What draws a headless snapshot.
const RENDERER: &str = "software";

/// A snapshot a run took.
pub(in crate::command) struct Taken {
    pub name: String,
    /// The PNG the run wrote.
    png: Vec<u8>,
    image: Image,
    /// The tree, layout, paint and batches that drew it.
    tree: String,
    surface: HeadlessSurface,
    /// The component it shows alone, by module path, when it is one's.
    component: Option<String>,
}

impl Taken {
    /// The snapshot `name` a run wrote into `dir` at `surface`.
    ///
    /// # Errors
    ///
    /// Why its files cannot be read.
    pub fn read(dir: &Path, name: &str, surface: HeadlessSurface) -> Result<Taken, String> {
        let path = dir.join(format!("{name}.png"));
        let png = fs::read(&path)
            .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
        let image = Image::from_png(&png)
            .map_err(|error| format!("the snapshot `{name}` is not a PNG: {error}"))?;
        let tree = fs::read_to_string(dir.join(format!("{name}.tree"))).unwrap_or_default();
        Ok(Taken {
            name: name.to_owned(),
            png,
            image,
            tree,
            surface,
            component: None,
        })
    }

    /// The snapshot as one of the component at `path` alone.
    pub fn of_component(mut self, path: &str) -> Taken {
        self.component = Some(path.to_owned());
        self
    }

    fn tree_hash(&self) -> String {
        let sum = Sha256::digest(self.tree.as_bytes());
        let hex: String = sum.iter().map(|b| format!("{b:02x}")).collect();
        format!("sha256:{hex}")
    }
}

/// How a snapshot holds to its golden.
pub(in crate::command) enum Held {
    Matched,
    /// It has no golden.
    Missing,
    /// It departs from its golden by more than the tolerance allows; what
    /// was found is written under `target/viso/snapshots/`.
    Differs(Departure),
    /// Its golden cannot be read, and why.
    Invalid(String),
}

/// How a snapshot departs from its golden, and where that was written.
pub(in crate::command) struct Departure {
    pub comparison: Comparison,
    pub tolerance: Tolerance,
    /// The golden's size, when it differs from the snapshot's.
    pub golden_size: (u32, u32),
    pub size: (u32, u32),
    /// Whether the tree that drew it differs from the golden's.
    pub tree_changed: bool,
    /// The diff image, the image taken and its tree, relative to the project.
    pub diff: String,
    pub actual: String,
    pub tree: String,
}

impl Departure {
    /// What departs, in a sentence.
    pub fn message(&self, name: &str) -> String {
        let how = if !self.comparison.same_size {
            format!(
                "is {} × {}, its golden {} × {}",
                self.size.0, self.size.1, self.golden_size.0, self.golden_size.1
            )
        } else {
            format!(
                "differs from its golden in {} pixel{} (at most {} allowed, distance up to {:.3})",
                self.comparison.differing,
                if self.comparison.differing == 1 {
                    ""
                } else {
                    "s"
                },
                self.tolerance.pixels,
                self.comparison.largest
            )
        };
        let tree = if self.tree_changed {
            "; the tree that drew it changed"
        } else {
            ""
        };
        format!("the snapshot `{name}` {how}{tree}")
    }

    /// Where what departs was written.
    pub fn notes(&self) -> Vec<String> {
        vec![
            format!("diff: {}", self.diff),
            format!("taken: {} (tree {})", self.actual, self.tree),
            "rewrite the golden with `viso snapshot update` if the change is meant".to_owned(),
        ]
    }

    /// The departure's members of a `test` event.
    pub fn write_json(&self, w: &mut JsonWriter) {
        for (name, path) in [
            ("diff", &self.diff),
            ("actual", &self.actual),
            ("tree", &self.tree),
        ] {
            w.name(name);
            w.string(path);
        }
        w.name("same_size");
        w.bool(self.comparison.same_size);
        w.name("differing_pixels");
        w.uint(self.comparison.differing);
        w.name("largest_distance");
        w.number(f64::from(self.comparison.largest));
        w.name("tree_changed");
        w.bool(self.tree_changed);
    }
}

/// A file written for a snapshot.
pub(in crate::command) struct Written {
    /// `snapshot` or `manifest`.
    pub kind: &'static str,
    /// Relative to the project root, `/`-separated.
    pub path: String,
    pub size: u64,
    pub hash: String,
}

/// The goldens of one target, and where departures are written.
pub(in crate::command) struct Store {
    root: PathBuf,
    dir: PathBuf,
    found: PathBuf,
    target: &'static str,
}

impl Store {
    /// The goldens of `target` under `root`, or in `dir` instead; departures
    /// go under `viso_dir`.
    pub fn new(root: &Path, dir: Option<PathBuf>, viso_dir: &Path, target: &'static str) -> Self {
        Store {
            dir: dir.unwrap_or_else(|| root.join(GOLDENS)),
            root: root.to_owned(),
            found: viso_dir.join("snapshots"),
            target,
        }
    }

    fn file(&self, name: &str, extension: &str) -> PathBuf {
        self.dir.join(format!("{name}.{}.{extension}", self.target))
    }

    /// Whether `name` has a golden.
    pub fn has(&self, name: &str) -> bool {
        self.file(name, "png").is_file()
    }

    /// The component, by module path, the golden `name` shows alone; `None`
    /// for a scenario's, or one whose sidecar cannot be read.
    pub fn component(&self, name: &str) -> Option<String> {
        self.sidecar(name).ok()?.component
    }

    /// The names of every golden, in order.
    pub fn names(&self) -> Vec<String> {
        let suffix = format!(".{}.png", self.target);
        let mut names: Vec<String> = fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.strip_suffix(&suffix).map(str::to_owned)
            })
            .collect();
        names.sort();
        names
    }

    /// The golden of `name`, relative to the project.
    pub fn golden_name(&self, name: &str) -> String {
        relative(&self.root, &self.file(name, "png"))
    }

    /// Holds `taken` to its golden, writing what departs.
    ///
    /// # Errors
    ///
    /// Why what departs could not be written.
    pub fn hold(&self, taken: &Taken) -> Result<Held, String> {
        let path = self.file(&taken.name, "png");
        let Ok(bytes) = fs::read(&path) else {
            return Ok(Held::Missing);
        };
        let golden = match Image::from_png(&bytes) {
            Ok(golden) => golden,
            Err(error) => {
                return Ok(Held::Invalid(format!(
                    "the golden `{}` is not a PNG: {error}",
                    relative(&self.root, &path)
                )));
            }
        };
        let sidecar = match self.sidecar(&taken.name) {
            Ok(sidecar) => sidecar,
            Err(why) => return Ok(Held::Invalid(why)),
        };
        let comparison = compare(&golden, &taken.image, sidecar.tolerance.threshold);
        if comparison.matches(sidecar.tolerance) {
            return Ok(Held::Matched);
        }
        let base = format!("{}.{}", taken.name, self.target);
        let write = |file: String, bytes: &[u8]| -> Result<String, String> {
            let path = self.found.join(file);
            fs::write(&path, bytes)
                .map_err(|error| format!("cannot write `{}`: {error}", path.display()))?;
            Ok(relative(&self.root, &path))
        };
        fs::create_dir_all(&self.found)
            .map_err(|error| format!("cannot create `{}`: {error}", self.found.display()))?;
        let image = diff(&golden, &taken.image, sidecar.tolerance.threshold);
        Ok(Held::Differs(Departure {
            comparison,
            tolerance: sidecar.tolerance,
            golden_size: (golden.width, golden.height),
            size: (taken.image.width, taken.image.height),
            tree_changed: sidecar
                .tree_hash
                .is_some_and(|hash| hash != taken.tree_hash()),
            diff: write(format!("{base}.diff.png"), &image.to_png())?,
            actual: write(format!("{base}.png"), &taken.png)?,
            tree: write(format!("{base}.tree"), taken.tree.as_bytes())?,
        }))
    }

    /// Writes `taken` as its golden, with its sidecar, keeping the
    /// tolerance the golden it replaces had.
    ///
    /// # Errors
    ///
    /// Why a file could not be written.
    pub fn write(&self, taken: &Taken, build_id: &str) -> Result<[Written; 2], String> {
        let tolerance = self
            .sidecar(&taken.name)
            .map(|sidecar| sidecar.tolerance)
            .unwrap_or_default();
        fs::create_dir_all(&self.dir)
            .map_err(|error| format!("cannot create `{}`: {error}", self.dir.display()))?;
        let png = self.file(&taken.name, "png");
        let json = self.file(&taken.name, "json");
        let sidecar = self.sidecar_text(taken, build_id, tolerance);
        let write = |path: &Path, kind, bytes: &[u8]| -> Result<Written, String> {
            fs::write(path, bytes)
                .map_err(|error| format!("cannot write `{}`: {error}", path.display()))?;
            let sum = Sha256::digest(bytes);
            let hex: String = sum.iter().map(|b| format!("{b:02x}")).collect();
            Ok(Written {
                kind,
                path: relative(&self.root, path),
                size: bytes.len() as u64,
                hash: format!("sha256:{hex}"),
            })
        };
        Ok([
            write(&png, "snapshot", &taken.png)?,
            write(&json, "manifest", sidecar.as_bytes())?,
        ])
    }

    /// The sidecar of `name`; the default tolerance and no tree when it has
    /// none.
    fn sidecar(&self, name: &str) -> Result<Sidecar, String> {
        let path = self.file(name, "json");
        let Ok(text) = fs::read_to_string(&path) else {
            return Ok(Sidecar::default());
        };
        let invalid = |why: &str| format!("`{}` {why}", relative(&self.root, &path));
        let value = JsonValue::parse(&text).map_err(|error| invalid(&format!("{error}")))?;
        let mut tolerance = Tolerance::default();
        if let Some(given) = value.get("tolerance") {
            if let Some(threshold) = given.get("threshold") {
                let threshold = threshold
                    .as_f64()
                    .filter(|t| (0.0..=1.0).contains(t))
                    .ok_or_else(|| invalid("has a `tolerance.threshold` not from 0 to 1"))?;
                tolerance.threshold = threshold as f32;
            }
            if let Some(pixels) = given.get("pixels") {
                let pixels = pixels
                    .as_f64()
                    .filter(|p| *p >= 0.0 && p.fract() == 0.0)
                    .ok_or_else(|| invalid("has a `tolerance.pixels` that is not a count"))?;
                tolerance.pixels = pixels as u64;
            }
        }
        Ok(Sidecar {
            tolerance,
            component: value
                .get("component")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
            tree_hash: value
                .get("tree_hash")
                .and_then(JsonValue::as_str)
                .map(str::to_owned),
        })
    }

    /// The sidecar of `taken`, one key a line so a change reviews as one.
    fn sidecar_text(&self, taken: &Taken, build_id: &str, tolerance: Tolerance) -> String {
        let string = |value: &str| {
            let mut w = JsonWriter::new();
            w.string(value);
            w.into_string()
        };
        let surface = taken.surface;
        let component = taken
            .component
            .as_deref()
            .map(|path| format!("  \"component\": {},\n", string(path)))
            .unwrap_or_default();
        format!(
            "{{\n  \"snapshot_version\": {SNAPSHOT_VERSION},\n  \"name\": {},\n{component}  \
             \"target\": {},\n  \"renderer\": {},\n  \"surface\": {},\n  \
             \"size\": [{}, {}],\n  \"tree_hash\": {},\n  \"build_id\": {},\n  \
             \"tolerance\": {{ \"threshold\": {}, \"pixels\": {} }}\n}}\n",
            string(&taken.name),
            string(self.target),
            string(RENDERER),
            string(&surface.to_env()),
            taken.image.width,
            taken.image.height,
            string(&taken.tree_hash()),
            string(build_id),
            tolerance.threshold,
            tolerance.pixels,
        )
    }
}

/// What a sidecar says that a comparison uses.
#[derive(Default)]
struct Sidecar {
    tolerance: Tolerance,
    tree_hash: Option<String>,
    component: Option<String>,
}

/// `path` relative to `root`, `/`-separated.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taken(name: &str, color: [u8; 4], tree: &str) -> Taken {
        let image = Image {
            width: 4,
            height: 2,
            rgba: color.repeat(8),
        };
        Taken {
            name: name.into(),
            png: image.to_png(),
            image,
            tree: tree.into(),
            surface: HeadlessSurface {
                width: 4,
                height: 2,
                scale: 1.0,
            },
            component: None,
        }
    }

    #[test]
    fn a_golden_is_written_held_and_departed_from() {
        let root = std::env::temp_dir().join(format!("viso-golden-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = Store::new(&root, None, &root.join("target/viso"), "headless");
        let gray = taken("home", [40, 40, 40, 255], "tree:\n");
        assert!(matches!(store.hold(&gray).unwrap(), Held::Missing));

        let [png, json] = store.write(&gray, "b1").unwrap();
        assert_eq!(png.path, "tests/snapshots/home.headless.png");
        assert_eq!((png.kind, json.kind), ("snapshot", "manifest"));
        assert!(png.hash.starts_with("sha256:"));
        let sidecar = fs::read_to_string(root.join("tests/snapshots/home.headless.json")).unwrap();
        let value = JsonValue::parse(&sidecar).unwrap();
        assert_eq!(
            value.get("surface").and_then(JsonValue::as_str),
            Some("4x2@1")
        );
        assert_eq!(store.names(), ["home"]);
        assert_eq!(store.component("home"), None);
        assert!(matches!(store.hold(&gray).unwrap(), Held::Matched));

        let badge = taken("badge", [0, 0, 0, 255], "tree:\n").of_component("ui::Badge");
        store.write(&badge, "b1").unwrap();
        assert_eq!(store.component("badge").as_deref(), Some("ui::Badge"));

        let red = taken("home", [255, 0, 0, 255], "tree: changed\n");
        let Held::Differs(departure) = store.hold(&red).unwrap() else {
            panic!("a red image matched a gray golden");
        };
        assert_eq!(departure.comparison.differing, 8);
        assert!(departure.tree_changed);
        assert_eq!(
            departure.diff,
            "target/viso/snapshots/home.headless.diff.png"
        );
        assert!(root.join(&departure.actual).is_file());
        assert!(departure.message("home").contains("8 pixels"));

        // A tolerance given in the sidecar is kept by an update.
        let loose = sidecar.replace("\"pixels\": 0", "\"pixels\": 8");
        fs::write(root.join("tests/snapshots/home.headless.json"), loose).unwrap();
        assert!(matches!(store.hold(&red).unwrap(), Held::Matched));
        store.write(&red, "b2").unwrap();
        let rewritten =
            fs::read_to_string(root.join("tests/snapshots/home.headless.json")).unwrap();
        assert!(rewritten.contains("\"pixels\": 8"), "{rewritten}");

        fs::write(root.join("tests/snapshots/home.headless.json"), "{").unwrap();
        assert!(matches!(store.hold(&red).unwrap(), Held::Invalid(_)));
        let _ = fs::remove_dir_all(&root);
    }
}
