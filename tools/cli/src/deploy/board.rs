//! A device target's board reached over SSH: the dev
//! artifact and the project's assets are copied into a session directory
//! under the login's home with `scp`, and the app is started there with
//! `ssh`, the dev variables in its environment; its stdout and stderr come
//! back over the same connection, and the connection's exit status is the
//! app's.
//!
//! The session directory is `viso-dev/<project>`, emptied when the session
//! opens. The remote shell writes the app's pid beside it, so a close is a
//! termination request sent over a second connection, and the app ends as
//! it does on the desktop. A warm restart copies the new executable beside
//! the running one first and moves it into place once the old app is gone.
//!
//! Every connection runs in batch mode: SSH asks for no password, so the
//! board takes the host's key, or an agent's.

use std::ffi::OsString;
use std::io::{self, Read};
use std::net::{IpAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use viso_project::DeployAddress;

use crate::dev::build::{Stamp, stamp};
use crate::process::Launch;

/// How long a quick SSH call — opening the session, moving a file, sending a
/// signal — may take.
const QUICK: Duration = Duration::from_secs(20);

/// The file the remote shell writes the app's pid to.
const PID_FILE: &str = ".pid";

/// How the board's session directory is reached over SSH.
#[derive(Clone)]
pub(crate) struct Remote {
    ssh: OsString,
    address: DeployAddress,
    /// The session directory, relative to the login's home.
    dir: String,
}

impl Remote {
    /// An `ssh` to the board, its remote command `script` run by `sh`.
    fn ssh(&self, script: &str) -> Command {
        let mut command = Command::new(&self.ssh);
        command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if let Some(port) = self.address.port {
            command.args(["-p", &port.to_string()]);
        }
        command
            .arg(self.address.ssh_destination())
            .arg("--")
            .arg(format!("sh -c {}", quote(script)));
        command
    }

    /// Sends the app the signal `name` (`TERM`, `KILL`).
    pub(crate) fn signal(&self, name: &str) {
        let pid = format!("{}/{PID_FILE}", self.dir);
        let _ = quick(&mut self.ssh(&format!(
            "[ -f {pid} ] && kill -{name} \"$(cat {pid})\"",
            pid = quote(&pid)
        )));
    }
}

/// The board and the session directory on it.
pub(crate) struct Board {
    remote: Remote,
    scp: OsString,
    /// The project's `assets/`, copied into the session directory once.
    assets: Option<PathBuf>,
    assets_copied: bool,
    /// The executable the last restart copied beside the running one, and
    /// its size and time then.
    staged: Mutex<Option<(PathBuf, Stamp)>>,
}

impl Board {
    /// The board at `address`, the session directory named after the
    /// project `name`, `assets` its assets directory if it has one; reached
    /// with the `ssh` and `scp` on the `PATH`.
    pub(crate) fn new(address: DeployAddress, name: &str, assets: Option<PathBuf>) -> Board {
        Board::with_tools("ssh".into(), "scp".into(), address, name, assets)
    }

    /// As [`Board::new`], reached with the programs `ssh` and `scp`.
    pub(crate) fn with_tools(
        ssh: OsString,
        scp: OsString,
        address: DeployAddress,
        name: &str,
        assets: Option<PathBuf>,
    ) -> Board {
        Board {
            remote: Remote {
                ssh,
                address,
                dir: format!("viso-dev/{name}"),
            },
            scp,
            assets,
            assets_copied: false,
            staged: Mutex::new(None),
        }
    }

    /// Where the board is reached, as `Viso.toml` writes it.
    pub(crate) fn address(&self) -> &DeployAddress {
        &self.remote.address
    }

    /// How the app on the board is signalled.
    pub(crate) fn remote(&self) -> Remote {
        self.remote.clone()
    }

    fn ssh(&self, script: &str) -> Command {
        self.remote.ssh(script)
    }

    /// An `scp` of `from` into `to`, a path in the session directory.
    fn scp(&self, from: &Path, to: &str, recursive: bool) -> Command {
        let mut command = Command::new(&self.scp);
        command.args(["-q", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
        if recursive {
            command.arg("-r");
        }
        if let Some(port) = self.remote.address.port {
            command.args(["-P", &port.to_string()]);
        }
        command.arg(from).arg(format!(
            "{}:{}/{to}",
            self.remote.address.scp_destination(),
            self.remote.dir
        ));
        command
    }

    /// Opens the session: empties the session directory and returns this
    /// machine's address on the route to the board, which the app is to
    /// reach the dev channel at.
    ///
    /// # Errors
    ///
    /// The board cannot be reached, or says nothing of the connection.
    pub(crate) fn open(&self) -> Result<IpAddr, String> {
        let dir = quote(&self.remote.dir);
        let said = quick(&mut self.ssh(&format!(
            "rm -rf {dir} && mkdir -p {dir} && echo \"$SSH_CONNECTION\""
        )))?;
        // `client port server port`: the board's own address on this
        // connection, which a datagram socket routes from.
        let board: IpAddr = said
            .split_whitespace()
            .nth(2)
            .and_then(|ip| ip.split('%').next()?.parse().ok())
            .ok_or_else(|| {
                format!(
                    "`{}` does not say which address it is reached at (`$SSH_CONNECTION` is `{}`)",
                    self.remote.address,
                    said.trim()
                )
            })?;
        route_from(board)
    }

    /// Starts copying `exe` beside the running app, for the restart onto it.
    pub(crate) fn stage(&self, exe: &Path) -> io::Result<Child> {
        let mut command = self.scp(exe, &staged_name(exe), false);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        own_group(&mut command);
        let child = command.launch()?;
        *self.staged.lock().unwrap_or_else(|e| e.into_inner()) = Some((exe.to_owned(), stamp(exe)));
        Ok(child)
    }

    /// Puts `exe` into the session directory: the copy staged beside the
    /// old app moved into place when it arrived whole, otherwise a copy
    /// now; the assets with it the first time.
    fn place(&mut self, exe: &Path) -> Result<(), String> {
        let bin = file_name(exe)?;
        let staged = self
            .staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .filter(|(path, at)| path == exe && *at == stamp(exe) && at.is_some());
        let moved = match staged {
            Some((_, Some((size, _)))) => {
                let (next, bin, dir) = (
                    quote(&staged_name(exe)),
                    quote(&bin),
                    quote(&self.remote.dir),
                );
                quick(&mut self.ssh(&format!(
                    "cd {dir} && [ $(wc -c < {next}) -eq {size} ] && mv -f {next} {bin}"
                )))
                .is_ok()
            }
            _ => false,
        };
        if !moved {
            copy(&mut self.scp(exe, &bin, false))?;
        }
        if !self.assets_copied {
            if let Some(assets) = self.assets.as_deref().filter(|dir| dir.is_dir()) {
                copy(&mut self.scp(assets, "", true))?;
            }
            self.assets_copied = true;
        }
        Ok(())
    }

    /// Copies `exe` to the board and starts it there with `args` and the
    /// variables `vars`, in the session directory; its outputs piped when
    /// `piped`. The `ssh` that carries it runs in a group of its own, so
    /// the terminal's Ctrl-C reaches the session, which closes the app.
    pub(crate) fn launch(
        &mut self,
        exe: &Path,
        args: &[OsString],
        vars: &[(&str, String)],
        piped: bool,
    ) -> Result<Child, String> {
        self.place(exe)?;
        let bin = file_name(exe)?;
        let mut script = format!(
            "cd {} && echo $$ > {PID_FILE} && chmod +x {} && exec env",
            quote(&self.remote.dir),
            quote(&bin)
        );
        for (name, value) in vars {
            script.push(' ');
            script.push_str(name);
            script.push('=');
            script.push_str(&quote(value));
        }
        script.push_str(&format!(" ./{}", quote(&bin)));
        for arg in args {
            script.push(' ');
            script.push_str(&quote(&arg.to_string_lossy()));
        }
        let mut command = self.ssh(&script);
        command.stdin(Stdio::null());
        if piped {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        own_group(&mut command);
        command
            .launch()
            .map_err(|e| format!("cannot run `ssh`: {e}"))
    }
}

/// How an app that ran over SSH ended, read from the connection's status:
/// 255 is SSH's own, a connection that failed or was lost.
pub(crate) fn ended(status: ExitStatus) -> Option<String> {
    (status.code() == Some(255))
        .then(|| "the SSH connection to the board failed or was lost".to_owned())
}

/// This machine's address on the route to `board`.
fn route_from(board: IpAddr) -> Result<IpAddr, String> {
    let unspecified: IpAddr = match board {
        IpAddr::V4(_) => [0, 0, 0, 0].into(),
        IpAddr::V6(_) => [0u16; 8].into(),
    };
    UdpSocket::bind((unspecified, 0))
        .and_then(|socket| {
            socket.connect((board, 9))?;
            socket.local_addr()
        })
        .map(|local| local.ip())
        .map_err(|e| format!("no route to the board at {board}: {e}"))
}

/// The name a restart's executable is copied under beside the running one.
fn staged_name(exe: &Path) -> String {
    format!(
        ".next-{}",
        exe.file_name().unwrap_or_default().to_string_lossy()
    )
}

fn file_name(exe: &Path) -> Result<String, String> {
    exe.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| format!("`{}` has no name", exe.display()))
}

/// `word` single-quoted for `sh`.
fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// Puts `command` in a process group of its own, out of the terminal's
/// Ctrl-C.
fn own_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(not(unix))]
    let _ = command;
}

/// Runs a copy to its end; why it failed.
fn copy(command: &mut Command) -> Result<(), String> {
    run(command, None).map(drop)
}

/// Runs a quick SSH call to its end, or for [`QUICK`]; what it wrote, or why
/// it failed.
fn quick(command: &mut Command) -> Result<String, String> {
    run(command, Some(QUICK))
}

/// Runs `command` with no input, out of the terminal's Ctrl-C, to its end or
/// for `timeout`: a call the session makes while it closes, after an
/// interrupt, runs all the same.
fn run(command: &mut Command, timeout: Option<Duration>) -> Result<String, String> {
    let name = crate::process::name(command.get_program());
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    own_group(command);
    let mut child = command.launch().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => format!("`{name}` is not installed"),
        _ => format!("cannot run `{name}`: {e}"),
    })?;
    let until = timeout.map(|timeout| Instant::now() + timeout);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if until.is_some_and(|until| Instant::now() >= until) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "`{name}` did not finish in {} s",
                    timeout.unwrap_or_default().as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(e) => return Err(format!("cannot run `{name}`: {e}")),
        }
    };
    let read = |stream: Option<Box<dyn Read>>| {
        let mut text = String::new();
        if let Some(mut stream) = stream {
            let _ = stream.read_to_string(&mut text);
        }
        text
    };
    let stdout = read(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read>));
    let stderr = read(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read>));
    if status.success() {
        Ok(stdout)
    } else {
        let said = match stderr.trim() {
            "" => stdout.trim().to_owned(),
            said => said.to_owned(),
        };
        Err(format!("`{name}` failed ({status}): {said}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_reads_back_as_itself_in_sh() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("it's"), r"'it'\''s'");
        assert_eq!(staged_name(Path::new("/t/debug/probe")), ".next-probe");
    }

    #[test]
    fn the_loopback_routes_from_itself() {
        assert_eq!(route_from([127, 0, 0, 1].into()), Ok([127, 0, 0, 1].into()));
    }
}
