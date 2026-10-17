//! Downloads a toolchain archive whose size and digest are pinned: over HTTPS
//! only, into a `.part` file the next attempt resumes, and renamed into place
//! only once its size and SHA-1 are the pinned ones. A transfer cut off while
//! bytes still arrive resumes at once; it fails after a few attempts in a row
//! bring nothing. A cached archive that
//! still verifies is not downloaded again; one that does not is deleted.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use sha1::{Digest, Sha1};

use crate::interrupt;
use crate::process::ProcessTree;

/// An archive as a component set pins it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pinned<'a> {
    pub url: &'a str,
    pub size: u64,
    /// Lowercase hex.
    pub sha1: &'a str,
}

/// Why a download did not land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The transfer failed; what was received is kept to resume.
    Transfer(String),
    /// What arrived is not the pinned archive, and is deleted.
    Integrity(String),
    /// An interrupt stopped it; what was received is kept to resume.
    Interrupted,
}

/// What moves an archive's bytes into a `.part` file, resuming what it holds.
pub(crate) trait Transfer {
    /// Fetches `url` into `part`, appending to what is there, calling
    /// `progress` with the bytes it holds as it grows.
    fn fetch(&self, url: &str, part: &Path, progress: &mut dyn FnMut(u64)) -> Result<(), Failure>;

    /// Waits before an attempt that follows one which brought nothing.
    fn pause(&self) {
        thread::sleep(Duration::from_secs(2));
    }
}

/// Attempts in a row that bring no byte before a download fails.
const STALLED: u32 = 3;

/// `curl`, HTTPS only, resuming and retrying.
pub(crate) struct Curl;

impl Transfer for Curl {
    fn fetch(&self, url: &str, part: &Path, progress: &mut dyn FnMut(u64)) -> Result<(), Failure> {
        if !url.starts_with("https://") {
            return Err(Failure::Transfer(format!("`{url}` is not HTTPS")));
        }
        let mut command = Command::new("curl");
        command
            .args([
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--tlsv1.2",
                "--fail",
                "--location",
                "--silent",
                "--show-error",
                "--retry",
                "3",
                "--retry-delay",
                "2",
                "--connect-timeout",
                "30",
                "--continue-at",
                "-",
                "--output",
            ])
            .arg(part)
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut tree = ProcessTree::spawn(&mut command).map_err(|error| {
            Failure::Transfer(if error.kind() == io::ErrorKind::NotFound {
                "`curl` is not installed".to_owned()
            } else {
                format!("cannot run `curl`: {error}")
            })
        })?;
        let mut stderr = tree.child.stderr.take();
        let said = thread::spawn(move || {
            let mut text = String::new();
            if let Some(stderr) = &mut stderr {
                let _ = stderr.read_to_string(&mut text);
            }
            text
        });
        let status = loop {
            if interrupt::count() > 0 {
                tree.kill();
                return Err(Failure::Interrupted);
            }
            match tree.child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    progress(fs::metadata(part).map_or(0, |m| m.len()));
                    thread::sleep(Duration::from_millis(250));
                }
                Err(error) => {
                    tree.kill();
                    return Err(Failure::Transfer(format!(
                        "cannot wait for `curl`: {error}"
                    )));
                }
            }
        };
        let said = said.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(Failure::Transfer(format!(
                "`curl` failed ({status}): {}",
                said.trim()
            )))
        }
    }
}

/// The SHA-1 of the file at `path` and its length.
pub(crate) fn digest(path: &Path) -> io::Result<(String, u64)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut buffer = vec![0; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        len += n as u64;
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((hex, len))
}

/// Whether the file at `path` is the archive `pinned` names.
fn verifies(path: &Path, pinned: &Pinned<'_>) -> Result<(), String> {
    let (sha1, len) = digest(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if len != pinned.size {
        return Err(format!("{len} bytes where {} are pinned", pinned.size));
    }
    if sha1 != pinned.sha1 {
        return Err(format!("SHA-1 {sha1} where {} is pinned", pinned.sha1));
    }
    Ok(())
}

/// The archive `pinned` names at `path`: kept when it is there and verifies,
/// else downloaded by `transfer` (resuming `<path>.part`) and verified.
/// `progress` hears the bytes received of the pinned size.
pub(crate) fn fetch(
    pinned: &Pinned<'_>,
    path: &Path,
    transfer: &dyn Transfer,
    progress: &mut dyn FnMut(u64),
) -> Result<(), Failure> {
    if path.is_file() {
        if verifies(path, pinned).is_ok() {
            return Ok(());
        }
        let _ = fs::remove_file(path);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| Failure::Transfer(format!("{}: {e}", dir.display())))?;
    }
    let part = part_of(path);
    if fs::metadata(&part).is_ok_and(|m| m.len() > pinned.size) {
        let _ = fs::remove_file(&part);
    }
    let held = || fs::metadata(&part).map_or(0, |m| m.len());
    let mut stalled = 0;
    while held() < pinned.size {
        let before = held();
        match transfer.fetch(pinned.url, &part, progress) {
            Ok(()) => break,
            Err(Failure::Transfer(why)) => {
                stalled = if held() > before { 0 } else { stalled + 1 };
                if stalled == STALLED {
                    return Err(Failure::Transfer(why));
                }
                if stalled > 0 {
                    transfer.pause();
                }
            }
            Err(other) => return Err(other),
        }
    }
    progress(pinned.size);
    if let Err(why) = verifies(&part, pinned) {
        let _ = fs::remove_file(&part);
        return Err(Failure::Integrity(format!("{}: {why}", pinned.url)));
    }
    fs::rename(&part, path).map_err(|e| Failure::Transfer(format!("{}: {e}", path.display())))
}

/// The text at `url`, unpinned: a manifest naming what is pinned elsewhere.
pub(crate) fn text(url: &str, transfer: &dyn Transfer) -> Result<String, Failure> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let part = std::env::temp_dir().join(format!(
        "viso-manifest-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_file(&part);
    let fetched = transfer.fetch(url, &part, &mut |_| {});
    let text = fetched.and_then(|()| {
        fs::read_to_string(&part).map_err(|e| Failure::Transfer(format!("{url}: {e}")))
    });
    let _ = fs::remove_file(&part);
    text
}

/// Where the download of `path` collects.
fn part_of(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_owned();
    name.push(".part");
    path.with_file_name(name)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::Cell;

    use super::*;

    /// A transfer copying from a directory of files by their URL's last
    /// segment, in chunks, counting its calls.
    pub(crate) struct Local {
        pub dir: PathBuf,
        pub calls: Cell<u32>,
        /// Bytes to deliver before failing, to test a resume.
        pub cut: Cell<Option<u64>>,
    }

    impl Local {
        pub(crate) fn new(dir: PathBuf) -> Local {
            Local {
                dir,
                calls: Cell::new(0),
                cut: Cell::new(None),
            }
        }
    }

    impl Transfer for Local {
        fn fetch(
            &self,
            url: &str,
            part: &Path,
            progress: &mut dyn FnMut(u64),
        ) -> Result<(), Failure> {
            self.calls.set(self.calls.get() + 1);
            let name = url.rsplit('/').next().unwrap();
            let bytes =
                fs::read(self.dir.join(name)).map_err(|e| Failure::Transfer(e.to_string()))?;
            let have = fs::metadata(part).map_or(0, |m| m.len()) as usize;
            let end = match self.cut.take() {
                Some(cut) => (cut as usize).min(bytes.len()),
                None => bytes.len(),
            };
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(part)
                .unwrap();
            std::io::Write::write_all(&mut file, &bytes[have.min(end)..end]).unwrap();
            progress(end as u64);
            if end < bytes.len() {
                return Err(Failure::Transfer("cut".into()));
            }
            Ok(())
        }

        fn pause(&self) {}
    }

    pub(crate) fn sha1_of(bytes: &[u8]) -> String {
        Sha1::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    #[test]
    fn a_download_resumes_verifies_and_is_kept_once_it_does() {
        let dir = std::env::temp_dir().join(format!("viso-download-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("remote")).unwrap();
        let bytes: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(dir.join("remote/a.zip"), &bytes).unwrap();
        let sha1 = sha1_of(&bytes);
        let pinned = Pinned {
            url: "https://example.invalid/a.zip",
            size: bytes.len() as u64,
            sha1: &sha1,
        };
        let local = Local::new(dir.join("remote"));
        let path = dir.join("cache/a.zip");
        // Cut off after 4000 bytes, it resumes from them at once.
        local.cut.set(Some(4000));
        fetch(&pinned, &path, &local, &mut |_| {}).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(!part_of(&path).exists());
        fetch(&pinned, &path, &local, &mut |_| {}).unwrap();
        assert_eq!(
            local.calls.get(),
            2,
            "a verified archive is not fetched again"
        );

        let gone = Pinned {
            url: "https://example.invalid/gone.zip",
            ..pinned
        };
        let before = local.calls.get();
        assert!(matches!(
            fetch(&gone, &dir.join("cache/gone.zip"), &local, &mut |_| {}),
            Err(Failure::Transfer(_))
        ));
        assert_eq!(
            local.calls.get() - before,
            STALLED,
            "attempts that bring nothing end it"
        );

        let wrong = Pinned {
            sha1: "0000000000000000000000000000000000000000",
            ..pinned
        };
        fs::remove_file(&path).unwrap();
        match fetch(&wrong, &path, &local, &mut |_| {}) {
            Err(Failure::Integrity(why)) => assert!(why.contains("is pinned"), "{why}"),
            other => panic!("{other:?}"),
        }
        assert!(!path.exists() && !part_of(&path).exists());
        let plain = Pinned {
            url: "http://example.invalid/a.zip",
            ..pinned
        };
        assert_eq!(
            Curl.fetch(plain.url, &part_of(&path), &mut |_| {}),
            Err(Failure::Transfer(
                "`http://example.invalid/a.zip` is not HTTPS".into()
            ))
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
