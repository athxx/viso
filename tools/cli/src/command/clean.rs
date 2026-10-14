//! `viso clean`: removes `target/viso/` — builds, caches, dev state, generated
//! sources, traces and locks — and never `dist/` or `~/.viso/`. A lock that a
//! live process holds stops it before anything is removed.

use std::fs;
use std::io;
use std::path::Path;

use viso_project::{LockGuard, Project};

use crate::context::Context;
use crate::error::{ENV_LOCK_HELD, ENV_REMOVE, SUCCESS};
use crate::output::{Output, human};

pub fn run(ctx: &Context<'_>, out: &mut Output) -> u8 {
    let project: Project = match ctx.project(out) {
        Ok(project) => project,
        Err(code) => return code,
    };
    let layout = ctx.layout(&project.root);
    let held = LockGuard::held(&layout);
    if !held.is_empty() {
        let notes: Vec<String> = held
            .iter()
            .map(|lock| {
                format!(
                    "{} is held ({}): {}",
                    lock.holder.description,
                    lock.holder,
                    lock.path.display()
                )
            })
            .collect();
        return out.failure(
            ENV_LOCK_HELD,
            "another viso process is using `target/viso/`; nothing was removed",
            &notes,
        );
    }

    let mut removed = Vec::new();
    let mut freed = 0;
    for path in layout.cleanable() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        let bytes = size(&path, &meta);
        let gone = if meta.is_dir() {
            fs::remove_dir_all(&path)
        } else {
            // A link, or a file in the place of the directory: the entry
            // itself, never what it points at.
            fs::remove_file(&path)
        };
        match gone {
            Ok(()) => {
                freed += bytes;
                removed.push(path);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => {
                return out.failure(
                    ENV_REMOVE,
                    &format!("cannot remove `{}`: {error}", path.display()),
                    &[],
                );
            }
        }
    }

    out.result(
        |w| {
            w.begin_object();
            w.name("removed");
            w.begin_array();
            for path in &removed {
                w.string(&path.display().to_string());
            }
            w.end_array();
            w.name("freed_bytes");
            w.uint(freed);
            w.end_object();
        },
        || match removed.as_slice() {
            [] => "nothing to clean\n".to_owned(),
            paths => paths
                .iter()
                .map(|path| format!("removed {} ({})\n", path.display(), human::size(freed)))
                .collect(),
        },
    );
    SUCCESS
}

/// The bytes of the files under `path`, links counted as themselves and not
/// followed.
fn size(path: &Path, meta: &fs::Metadata) -> u64 {
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.metadata().ok()?;
            Some(size(&entry.path(), &meta))
        })
        .sum()
}
