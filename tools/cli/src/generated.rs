//! A directory a command generates whole, such as an export: every file is
//! written into a staging tree beside it, which then takes its place in one
//! rename, so a reader sees the old tree or the new one and an interrupted
//! write leaves the old one as it was.
//!
//! What it writes it records in the tree's [`RECORD`], each file with its
//! SHA-256. The next write replaces the tree only while every file in it is
//! one it recorded, unchanged: a file someone edited or added, or a
//! directory it never wrote, is a conflict, which `--force` overrides.
//! What the tree [keeps](Tree::keep), such as the packages a package
//! manager installed into it, is neither checked nor replaced: it moves into
//! the new tree as it is.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use viso_ende::{JsonValue, JsonWriter};

use crate::atomic;

/// The file a generated tree records its files in.
pub(crate) const RECORD: &str = ".viso-generated.json";

/// How many conflicting paths a refusal names.
const NAMED: usize = 8;

/// A tree of files to write, by `/`-separated path below its directory.
#[derive(Debug, Default)]
pub(crate) struct Tree {
    files: Vec<(String, Vec<u8>)>,
    keep: Vec<String>,
}

impl Tree {
    /// Adds the file `path` holding `bytes`.
    pub(crate) fn add(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        let path = path.into();
        debug_assert!(
            path.split('/')
                .all(|part| !part.is_empty() && part != "." && part != ".."),
            "`{path}` stays below its tree"
        );
        self.files.push((path, bytes.into()));
    }

    /// Keeps the entry `name` of the directory, a file or a directory, as it
    /// is: what another tool writes there.
    pub(crate) fn keep(&mut self, name: &str) {
        debug_assert!(
            !name.is_empty() && !name.contains('/'),
            "`{name}` is one entry"
        );
        self.keep.push(name.to_owned());
    }

    /// Each file's path and bytes, in path order.
    pub(crate) fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files.iter().map(|(p, b)| (p.as_str(), b.as_slice()))
    }

    /// The size and `sha256:<hex>` of its files, as a build hashes a
    /// directory: over each file in path order, its path, a NUL and the
    /// SHA-256 of its bytes.
    pub(crate) fn hash(&self) -> (u64, String) {
        let mut files: Vec<&(String, Vec<u8>)> = self.files.iter().collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files.dedup_by(|a, b| a.0 == b.0);
        let mut tree = Sha256::new();
        let mut size = 0;
        for (path, bytes) in files {
            size += bytes.len() as u64;
            tree.update(path.as_bytes());
            tree.update([0]);
            tree.update(Sha256::digest(bytes));
        }
        (size, format!("sha256:{}", hex(&tree.finalize())))
    }

    fn sort(&mut self) {
        self.files.sort_by(|a, b| a.0.cmp(&b.0));
        self.files.dedup_by(|a, b| a.0 == b.0);
    }
}

/// Why a tree was not written.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// The directory holds what this command did not write, or changed
    /// since: its paths, the first few of them.
    Conflict {
        /// Whether the directory was never a generated tree.
        foreign: bool,
        paths: Vec<String>,
        more: usize,
    },
    /// A write failed.
    Io(String),
}

/// Writes `tree` as the directory `dir`, made by `generator`, replacing the
/// tree there unless it holds what was not generated or was changed since,
/// which `force` replaces too.
///
/// # Errors
///
/// A conflict, or the write that failed; either way `dir` is as it was.
pub(crate) fn write(
    dir: &Path,
    mut tree: Tree,
    generator: &str,
    force: bool,
) -> Result<(), Refusal> {
    tree.sort();
    let exists = match fs::symlink_metadata(dir) {
        Ok(_) => true,
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(e) => return Err(io_error(dir, &e)),
    };
    if exists && !force {
        conflicts(dir, &tree.keep)?;
    }
    let staging = atomic::sibling(dir);
    let _ = remove(&staging);
    let staged = stage(&staging, &tree, generator);
    let placed = staged.and_then(|()| place(&staging, dir, exists, &tree.keep));
    if placed.is_err() {
        let _ = remove(&staging);
    }
    placed
}

/// Fails with what in the existing `dir`, beside what it `keep`s, the record
/// does not account for.
fn conflicts(dir: &Path, keep: &[String]) -> Result<(), Refusal> {
    if !fs::symlink_metadata(dir)
        .map_err(|e| io_error(dir, &e))?
        .is_dir()
    {
        return Err(Refusal::Conflict {
            foreign: true,
            paths: vec![String::new()],
            more: 0,
        });
    }
    let mut found = Vec::new();
    walk(dir, "", &mut found).map_err(|e| io_error(dir, &e))?;
    found.sort();
    let recorded = match fs::read_to_string(dir.join(RECORD)) {
        Ok(text) => Some(recorded(&text)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_error(&dir.join(RECORD), &e)),
    };
    let foreign = recorded.is_none();
    let recorded = recorded.unwrap_or_default();
    let mut changed = Vec::new();
    let kept = |path: &str| {
        let first = path.split('/').next().unwrap_or_default();
        keep.iter().any(|k| k == first)
    };
    for path in found.into_iter().filter(|p| p != RECORD && !kept(p)) {
        let same = match recorded.iter().find(|(p, _)| *p == path) {
            Some((_, hash)) => {
                let bytes = fs::read(dir.join(&path)).map_err(|e| io_error(dir, &e))?;
                *hash == digest(&bytes)
            }
            None => false,
        };
        if !same {
            changed.push(path);
        }
    }
    if changed.is_empty() {
        return Ok(());
    }
    let more = changed.len().saturating_sub(NAMED);
    changed.truncate(NAMED);
    Err(Refusal::Conflict {
        foreign,
        paths: changed,
        more,
    })
}

/// The files `text`, a record, names, with their hashes.
fn recorded(text: &str) -> Vec<(String, String)> {
    let Ok(value) = JsonValue::parse(text) else {
        return Vec::new();
    };
    value
        .get("files")
        .and_then(JsonValue::as_array)
        .unwrap_or_default()
        .iter()
        .filter_map(|file| {
            Some((
                file.get("path")?.as_str()?.to_owned(),
                file.get("sha256")?.as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// Every file and link below `dir`, as `/`-separated paths after `prefix`;
/// an empty directory counts as itself.
fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) -> io::Result<()> {
    let mut any = false;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        any = true;
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{prefix}{name}");
        if entry.file_type()?.is_dir() {
            walk(&entry.path(), &format!("{path}/"), out)?;
        } else {
            out.push(path);
        }
    }
    if !any && !prefix.is_empty() {
        out.push(prefix.to_owned());
    }
    Ok(())
}

/// Writes `tree` and its record into the new directory `staging`.
fn stage(staging: &Path, tree: &Tree, generator: &str) -> Result<(), Refusal> {
    let mut record = JsonWriter::new();
    record.begin_object();
    record.name("generator");
    record.string(generator);
    record.name("files");
    record.begin_array();
    for (path, bytes) in tree.files() {
        let target = staging.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        fs::write(&target, bytes).map_err(|e| io_error(&target, &e))?;
        record.begin_object();
        record.name("path");
        record.string(path);
        record.name("sha256");
        record.string(&digest(bytes));
        record.end_object();
    }
    record.end_array();
    record.end_object();
    fs::create_dir_all(staging).map_err(|e| io_error(staging, &e))?;
    let mut text = record.into_string();
    text.push('\n');
    let path = staging.join(RECORD);
    fs::write(&path, text).map_err(|e| io_error(&path, &e))
}

/// Puts `staging` in the place of `dir`, carrying over what it `keep`s: the
/// old tree moves aside first and comes back, with what was carried, when
/// the new one cannot take its place.
fn place(staging: &Path, dir: &Path, exists: bool, keep: &[String]) -> Result<(), Refusal> {
    if !exists {
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        return fs::rename(staging, dir).map_err(|e| io_error(dir, &e));
    }
    let mut carried = Vec::new();
    for name in keep {
        let from = dir.join(name);
        if fs::symlink_metadata(&from).is_err() {
            continue;
        }
        if let Err(e) = fs::rename(&from, staging.join(name)) {
            carry_back(staging, dir, &carried);
            return Err(io_error(&from, &e));
        }
        carried.push(name.as_str());
    }
    let aside = aside(dir);
    let _ = remove(&aside);
    if let Err(e) = fs::rename(dir, &aside) {
        carry_back(staging, dir, &carried);
        return Err(io_error(dir, &e));
    }
    if let Err(e) = fs::rename(staging, dir) {
        let _ = fs::rename(&aside, dir);
        carry_back(staging, dir, &carried);
        return Err(io_error(dir, &e));
    }
    let _ = remove(&aside);
    Ok(())
}

/// Moves each `carried` entry from `staging` back into `dir`.
fn carry_back(staging: &Path, dir: &Path, carried: &[&str]) {
    for name in carried {
        let _ = fs::rename(staging.join(name), dir.join(name));
    }
}

/// Where the tree being replaced waits while the new one takes its place.
fn aside(dir: &Path) -> PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    dir.with_file_name(format!(".{name}.viso-{}.old", std::process::id()))
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(e) => Err(e),
    }
}

fn digest(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn io_error(path: &Path, e: &io::Error) -> Refusal {
    Refusal::Io(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("viso-generated-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn tree(files: &[(&str, &str)]) -> Tree {
        let mut tree = Tree::default();
        for (path, text) in files {
            tree.add(*path, text.as_bytes());
        }
        tree
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut found = Vec::new();
        walk(dir, "", &mut found).unwrap();
        found.sort();
        found
    }

    #[test]
    fn a_tree_replaces_the_one_it_wrote_and_leaves_nothing_beside_it() {
        let root = scratch("replace");
        let dir = root.join("out");
        write(
            &dir,
            tree(&[("index.html", "a"), ("css/app.css", "b")]),
            "t",
            false,
        )
        .unwrap();
        assert_eq!(names(&dir), [RECORD, "css/app.css", "index.html"]);
        write(&dir, tree(&[("index.html", "c")]), "t", false).unwrap();
        assert_eq!(names(&dir), [RECORD, "index.html"]);
        assert_eq!(fs::read_to_string(dir.join("index.html")).unwrap(), "c");
        assert_eq!(
            names(&root),
            [format!("out/{RECORD}"), "out/index.html".into()]
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_edit_or_a_foreign_file_is_a_conflict_force_replaces() {
        let root = scratch("conflict");
        let dir = root.join("out");
        write(&dir, tree(&[("index.html", "a")]), "t", false).unwrap();
        fs::write(dir.join("index.html"), "edited").unwrap();
        fs::write(dir.join("notes.txt"), "mine").unwrap();
        assert_eq!(
            write(&dir, tree(&[("index.html", "b")]), "t", false),
            Err(Refusal::Conflict {
                foreign: false,
                paths: vec!["index.html".into(), "notes.txt".into()],
                more: 0,
            })
        );
        assert_eq!(
            fs::read_to_string(dir.join("index.html")).unwrap(),
            "edited"
        );
        write(&dir, tree(&[("index.html", "b")]), "t", true).unwrap();
        assert_eq!(names(&dir), [RECORD, "index.html"]);

        let mine = root.join("mine");
        fs::create_dir_all(&mine).unwrap();
        fs::write(mine.join("page.html"), "hand written").unwrap();
        assert!(matches!(
            write(&mine, tree(&[("index.html", "a")]), "t", false),
            Err(Refusal::Conflict { foreign: true, .. })
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn what_a_tree_keeps_is_carried_over_untouched() {
        let root = scratch("keep");
        let dir = root.join("out");
        let kept = |files: &[(&str, &str)]| {
            let mut tree = tree(files);
            tree.keep("node_modules");
            tree.keep("package-lock.json");
            tree
        };
        write(&dir, kept(&[("package.json", "a")]), "t", false).unwrap();
        fs::create_dir_all(dir.join("node_modules/solid-js")).unwrap();
        fs::write(dir.join("node_modules/solid-js/index.js"), "x").unwrap();
        fs::write(dir.join("package-lock.json"), "{}").unwrap();
        write(&dir, kept(&[("package.json", "b")]), "t", false).unwrap();
        assert_eq!(
            names(&dir),
            [
                RECORD,
                "node_modules/solid-js/index.js",
                "package-lock.json",
                "package.json"
            ]
        );
        assert_eq!(fs::read_to_string(dir.join("package.json")).unwrap(), "b");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_directory_is_written_into() {
        let root = scratch("empty");
        write(&root, tree(&[("a.html", "a")]), "t", false).unwrap();
        assert_eq!(names(&root), [RECORD, "a.html"]);
        let _ = fs::remove_dir_all(&root);
    }
}
