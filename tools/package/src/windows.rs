//! A Windows executable's signature: Authenticode with `signtool`, the
//! certificate and its key kept in the system's certificate store.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{newest, run};

/// `signtool`: on `PATH`, else the newest one the Windows SDK installed.
pub fn signtool() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "signtool.exe"
    } else {
        "signtool"
    };
    if let Some(path) = std::env::var_os("PATH")
        .iter()
        .flat_map(std::env::split_paths)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
    {
        return Some(path);
    }
    let kits = std::env::var_os("ProgramFiles(x86)")
        .map(PathBuf::from)?
        .join("Windows Kits/10/bin");
    let version = newest(&kits, |n| n.starts_with("10."))?;
    let arch = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    };
    Some(version.join(arch).join(name)).filter(|path| path.is_file())
}

/// Whether `identity` is a certificate's SHA-1 thumbprint rather than its
/// subject name.
fn thumbprint(identity: &str) -> bool {
    identity.len() == 40 && identity.chars().all(|c| c.is_ascii_hexdigit())
}

/// Signs `exe` with the certificate `identity` names, its SHA-256 digest
/// timestamped by `timestamp_url` when there is one.
pub fn sign(
    signtool: &Path,
    exe: &Path,
    identity: &str,
    timestamp_url: Option<&str>,
) -> Result<(), String> {
    let mut cmd = Command::new(signtool);
    cmd.args(["sign", "/fd", "SHA256"]);
    cmd.arg(if thumbprint(identity) { "/sha1" } else { "/n" })
        .arg(identity);
    if let Some(url) = timestamp_url {
        cmd.args(["/tr", url, "/td", "SHA256"]);
    }
    run(cmd.arg(exe))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbprint_is_forty_hex_digits() {
        assert!(thumbprint("0123456789abcdef0123456789ABCDEF01234567"));
        assert!(!thumbprint("Example Ltd"));
        assert!(!thumbprint("0123"));
    }
}
