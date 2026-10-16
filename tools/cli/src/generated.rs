//! A directory a command generates whole, such as an export: every file is
//! written into a staging tree beside it, which then takes its place in one
//! rename, so a reader sees the old tree or the new one and an interrupted
//! write leaves the old one as it was.
//!
//! What it writes it records in the tree's [`RECORD`], each file with its
//! SHA-256. The next write replaces the tree only while every file in it is
//! one it recorded, unchanged: a file someone edited or added, or a
//! directory it never wrote, is a conflict, which `--force` overrides.

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

    /// Each file's path and bytes, in path order.
    pub(crate) fn files(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.files.iter().map(|(p, b)| (p.as_str(), b.as_slice()))
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
        conflicts(dir)?;
    }
    let staging = atomic::sibling(dir);
    let _ = remove(&staging);
    let staged = stage(&staging, &tree, generator);
    let placed = staged.and_then(|()| place(&staging, dir, exists));
    if placed.is_err() {
        let _ = remove(&staging);
    }
    placed
}

/// Fails with what in the existing `dir` the record does not account for.
fn conflicts(dir: &Path) -> Result<(), Refusal> {
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
    for path in found.into_iter().filter(|p| p != RECORD) {
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

/// Puts `staging` in the place of `dir`: the old tree moves aside first and
/// comes back when the new one cannot take its place.
fn place(staging: &Path, dir: &Path, exists: bool) -> Result<(), Refusal> {
    if !exists {
        if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, &e))?;
        }
        return fs::rename(staging, dir).map_err(|e| io_error(dir, &e));
    }
    let aside = aside(dir);
    let _ = remove(&aside);
    fs::rename(dir, &aside).map_err(|e| io_error(dir, &e))?;
    if let Err(e) = fs::rename(staging, dir) {
        let _ = fs::rename(&aside, dir);
        return Err(io_error(dir, &e));
    }
    let _ = remove(&aside);
    Ok(())
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
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
    fn an_empty_directory_is_written_into() {
        let root = scratch("empty");
        write(&root, tree(&[("a.html", "a")]), "t", false).unwrap();
        assert_eq!(names(&root), [RECORD, "a.html"]);
        let _ = fs::remove_dir_all(&root);
    }
}
