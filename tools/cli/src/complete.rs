//! Shell completion. `viso completion <shell>` writes a script that, as the
//! user types, runs `viso` again with `VISO_COMPLETE` set and the words so
//! far; `main` answers that before anything else. The candidates therefore
//! come from the same grammar as the parser, and the ones that depend on the
//! machine — configuration keys, device profile IDs, test scenarios — are read
//! as they are at that moment, from disk only: completing never builds,
//! boots a device or reaches the network.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

use clap_complete::CompletionCandidate;
use clap_complete::env::{EnvCompleter, Shells};
use viso_project::{DeviceStore, KEYS, Platform};

use crate::args::Shell;

/// The variable that turns a run of `viso` into a completion request.
pub const VAR: &str = "VISO_COMPLETE";

/// Answers a completion request and exits, when this run is one.
pub fn answer() {
    clap_complete::CompleteEnv::with_factory(crate::args::command)
        .var(VAR)
        .shells(SHELLS)
        .complete();
}

/// The shells `viso completion` supports.
const SHELLS: Shells<'static> = Shells(&[
    &clap_complete::env::Bash,
    &clap_complete::env::Zsh,
    &clap_complete::env::Fish,
    &clap_complete::env::Powershell,
]);

/// Writes the script that registers `viso`'s completion with `shell`.
pub fn register(shell: Shell, out: &mut dyn Write) -> std::io::Result<()> {
    let completer: &dyn EnvCompleter = match shell {
        Shell::Bash => &clap_complete::env::Bash,
        Shell::Zsh => &clap_complete::env::Zsh,
        Shell::Fish => &clap_complete::env::Fish,
        Shell::Powershell => &clap_complete::env::Powershell,
    };
    let program = program();
    completer.write_registration(VAR, "viso", "viso", &program.to_string_lossy(), out)
}

/// How the script calls `viso` back: as this run was called, absolute when it
/// was called by a path rather than found on `PATH`.
fn program() -> OsString {
    let invoked = std::env::args_os()
        .next()
        .unwrap_or_else(|| OsString::from("viso"));
    let path = PathBuf::from(&invoked);
    if path.components().count() > 1
        && path.is_relative()
        && let Ok(cwd) = std::env::current_dir()
    {
        return cwd.join(path).into_os_string();
    }
    invoked
}

/// Every configuration key.
pub fn config_keys() -> Vec<CompletionCandidate> {
    KEYS.iter()
        .map(|key| CompletionCandidate::new(*key))
        .collect()
}

/// Every simulator and emulator profile ID.
pub fn device_ids() -> Vec<CompletionCandidate> {
    let Some(store) = DeviceStore::home() else {
        return Vec::new();
    };
    [Platform::Ios, Platform::Android]
        .into_iter()
        .flat_map(|platform| store.list(platform))
        .map(|profile| {
            CompletionCandidate::new(profile.id).help(Some(
                format!("{} {}", profile.platform.as_str(), profile.system).into(),
            ))
        })
        .collect()
}

/// The game scenarios of the project the current directory lies in.
pub fn scenarios() -> Vec<CompletionCandidate> {
    let Some(root) = std::env::current_dir()
        .ok()
        .and_then(|cwd| viso_project::find_root(&cwd))
    else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(root.join("tests/game")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "tape"))
        .filter_map(|path| Some(path.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort();
    names.into_iter().map(CompletionCandidate::new).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shell_registers_a_script_that_calls_viso_back() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish, Shell::Powershell] {
            let mut script = Vec::new();
            register(shell, &mut script).unwrap();
            let script = String::from_utf8(script).unwrap();
            assert!(script.contains(VAR), "{shell:?}: {script}");
        }
    }

    #[test]
    fn the_grammar_completes_its_values() {
        let complete = |line: &[&str], at: usize| -> Vec<String> {
            let mut command = crate::args::command();
            let args: Vec<OsString> = line.iter().map(OsString::from).collect();
            clap_complete::engine::complete(&mut command, args, at, None)
                .unwrap()
                .into_iter()
                .map(|c| c.get_value().to_string_lossy().into_owned())
                .collect()
        };
        let targets = complete(&["viso", "run", ""], 2);
        for target in ["ios", "android", "web-gpu"] {
            assert!(targets.iter().any(|t| t == target), "{targets:?}");
        }
        let profiles = complete(&["viso", "check", "--profile", ""], 3);
        assert_eq!(profiles, ["dev", "release", "shipping"]);
        let keys = complete(&["viso", "config", "get", "profile.rel"], 3);
        assert_eq!(
            keys,
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
            ]
        );
    }
}
