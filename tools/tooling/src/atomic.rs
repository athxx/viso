//! Writes that land whole or not at all: the new bytes go to a sibling file,
//! which then takes the place of the old one in one rename, so a reader — an
//! editor, a build, a second `viso` — sees the old file or the new one, and an
//! interrupted write leaves the old one as it was.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Replaces the file at `path` with `bytes`, keeping its permissions, its
/// directory created when it is missing.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let staged = sibling(path);
    let written = (|| {
        let mut file = fs::File::create(&staged)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Ok(meta) = fs::metadata(path) {
            fs::set_permissions(&staged, meta.permissions())?;
        }
        fs::rename(&staged, path)
    })();
    if written.is_err() {
        let _ = fs::remove_file(&staged);
    }
    written
}

/// A name beside `path` for staging, hidden and unlikely to be anyone's.
pub fn sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(".{name}.viso-{}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_replaces_the_file_and_leaves_nothing_beside_it() {
        let dir = std::env::temp_dir().join(format!("viso-atomic-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.vs");
        fs::write(&path, "old").unwrap();
        write(&path, b"new").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
        let entries: Vec<_> = fs::read_dir(&dir).unwrap().collect();
        assert_eq!(entries.len(), 1);
        write(&dir.join("new/b.vs"), b"b").unwrap();
        assert_eq!(fs::read_to_string(dir.join("new/b.vs")).unwrap(), "b");
        let _ = fs::remove_dir_all(&dir);
    }
}
