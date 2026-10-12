//! A package's assets: the files under `assets/`, beside its `Viso.toml`,
//! indexed by the path `asset("…")` names them by.
//!
//! [`AssetCatalog::scan`] walks the directory once, reading only the head of
//! each file to sniff its format; a file in no format the asset system draws is
//! skipped. Compiling checks every `asset("…")` against the catalog; packaging
//! reads the bytes of the assets a view uses, and nothing else.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub use viso_ui::resource::{AssetFormat, AssetKind, ResourceId, normalize_path};

/// The directory of a package's assets, beside its `Viso.toml`.
pub const ASSET_DIR: &str = "assets";

/// The diagnostic code of every misuse of an asset.
pub const ASSET_ERROR: &str = "E3713";

/// The assets of one package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetCatalog {
    dir: PathBuf,
    files: BTreeMap<String, AssetFormat>,
}

/// Why an `asset("…")` path names no asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetIssue {
    /// Absolute, climbing out with `..`, or empty.
    Outside,
    /// No file of the catalog.
    Missing { nearest: Option<String> },
    /// A file in no format the asset system draws.
    Format,
}

impl AssetCatalog {
    /// The catalog of the files in `dir` and its subdirectories; empty when it
    /// does not exist.
    ///
    /// # Errors
    ///
    /// When a directory or a file head cannot be read.
    pub fn scan(dir: &Path) -> std::io::Result<AssetCatalog> {
        let mut files = BTreeMap::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(at) = stack.pop() {
            let entries = match std::fs::read_dir(&at) {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && at == dir => {
                    return Ok(AssetCatalog {
                        dir: dir.to_path_buf(),
                        files,
                    });
                }
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                let kind = entry.file_type()?;
                if kind.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !kind.is_file() {
                    continue;
                }
                let Some(name) = path
                    .strip_prefix(dir)
                    .ok()
                    .and_then(|p| p.to_str())
                    .and_then(normalize_path)
                else {
                    continue;
                };
                let mut head = Vec::with_capacity(4096);
                std::fs::File::open(&path)?
                    .take(4096)
                    .read_to_end(&mut head)?;
                if let Some(format) = AssetFormat::sniff(&head) {
                    files.insert(name, format);
                }
            }
        }
        Ok(AssetCatalog {
            dir: dir.to_path_buf(),
            files,
        })
    }

    /// A catalog of `files` (path and format) in `dir`, for tests and tools
    /// that already hold the index.
    pub fn of(
        dir: PathBuf,
        files: impl IntoIterator<Item = (String, AssetFormat)>,
    ) -> AssetCatalog {
        AssetCatalog {
            dir,
            files: files.into_iter().collect(),
        }
    }

    /// The asset `path` names: its normalized path, identity and format.
    ///
    /// # Errors
    ///
    /// A path outside `assets/`, a file the catalog does not hold, or one in
    /// no asset format.
    pub fn lookup(&self, path: &str) -> Result<(String, ResourceId, AssetFormat), AssetIssue> {
        let normal = normalize_path(path).ok_or(AssetIssue::Outside)?;
        match self.files.get(&normal) {
            Some(format) => {
                let id = ResourceId::of(&normal);
                Ok((normal, id, *format))
            }
            None if self.dir.join(&normal).is_file() => Err(AssetIssue::Format),
            None => Err(AssetIssue::Missing {
                nearest: self.nearest(&normal),
            }),
        }
    }

    /// The directory the catalog indexes.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every asset, by path.
    pub fn files(&self) -> impl Iterator<Item = (&str, AssetFormat)> {
        self.files.iter().map(|(p, f)| (p.as_str(), *f))
    }

    /// The catalog path closest to `path` by edit distance, for a suggestion.
    fn nearest(&self, path: &str) -> Option<String> {
        let candidates: Vec<crate::resolve::suggest::Candidate<'_>> = self
            .files
            .keys()
            .map(|name| crate::resolve::suggest::Candidate {
                name,
                declared_at: None,
            })
            .collect();
        crate::resolve::suggest::nearest(path, candidates)
            .into_iter()
            .next()
            .map(|s| s.name.to_owned())
    }
}

/// One asset a compiled package can show.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct AssetUse {
    pub id: ResourceId,
    /// Its path under `assets/`, normalized.
    pub path: String,
    pub format: AssetFormat,
}
