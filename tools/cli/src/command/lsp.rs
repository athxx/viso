//! `viso lsp`: the `.vs` language server for editors. The server is its own
//! binary, `viso-lsp`, a synchronous JSON-RPC loop over stdio; this command
//! only finds it — next to `viso`, then on `PATH` — and replaces itself with
//! it, so stdout carries nothing but the protocol and the server's exit is
//! the command's.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::args::LspArgs;
use crate::context::Context;
use crate::error::{CLI_USAGE, ENV_LSP};
use crate::output::Output;

/// The server's file name on this platform.
const SERVER: &str = if cfg!(windows) {
    "viso-lsp.exe"
} else {
    "viso-lsp"
};

pub fn run(ctx: &Context<'_>, args: &LspArgs, out: &mut Output) -> u8 {
    if ctx.global.json {
        return out.failure(
            CLI_USAGE,
            "`viso lsp` speaks JSON-RPC on stdout, which has no event form",
            &["drop `--json`".to_owned()],
        );
    }
    let Some(server) = locate() else {
        return out.failure(
            ENV_LSP,
            "no `viso-lsp` next to `viso` or on `PATH`",
            &[install()],
        );
    };
    let mut command = Command::new(&server);
    if args.stdio {
        command.arg("--stdio");
    }
    let error = replace(command);
    out.failure(
        ENV_LSP,
        &format!("cannot start `{}`: {error}", server.display()),
        &[],
    )
}

/// `viso-lsp` beside the running executable, else the first on `PATH`.
pub(crate) fn locate() -> Option<PathBuf> {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(SERVER)))
        .filter(|path| is_file(path));
    beside.or_else(|| {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(SERVER))
            .find(|path| is_file(path))
    })
}

/// How to install the server this CLI speaks with.
pub(crate) fn install() -> String {
    format!(
        "cargo install viso-lsp --version {}",
        env!("CARGO_PKG_VERSION")
    )
}

fn is_file(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.is_file())
}

/// Replaces this process with `command`; returns only when that fails.
#[cfg(unix)]
pub(crate) fn replace(mut command: Command) -> std::io::Error {
    use std::os::unix::process::CommandExt;
    crate::process::echo(&command);
    command.exec()
}

/// Runs `command` on this process's stdio and exits with its code; returns
/// only when it cannot start.
#[cfg(not(unix))]
pub(crate) fn replace(mut command: Command) -> std::io::Error {
    crate::process::echo(&command);
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(error) => error,
    }
}
