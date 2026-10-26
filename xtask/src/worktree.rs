//! A detached worktree of another commit, for the gates that measure it
//! beside the work tree on one machine.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A detached worktree at `path`, removed when dropped.
pub(crate) struct Worktree {
    root: PathBuf,
    path: PathBuf,
}

impl Worktree {
    /// Checks out `rev` of the repository at `root` at `path`, first
    /// removing what a run that stopped early left there.
    pub(crate) fn add(root: &Path, path: &Path, rev: &str) -> Result<Worktree, String> {
        let _ = run(git(root).args(["worktree", "remove", "--force"]).arg(path));
        let _ = std::fs::remove_dir_all(path);
        let _ = run(git(root).args(["worktree", "prune"]));
        run(git(root)
            .args(["worktree", "add", "--detach"])
            .arg(path)
            .arg(rev))?;
        Ok(Worktree {
            root: root.to_owned(),
            path: path.to_owned(),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        let _ = run(git(&self.root)
            .args(["worktree", "remove", "--force"])
            .arg(&self.path));
    }
}

/// `git` in `root`.
fn git(root: &Path) -> Command {
    let mut command = Command::new("git");
    command.current_dir(root);
    command
}

fn run(command: &mut Command) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`{command:?}` failed ({status})"))
    }
}
