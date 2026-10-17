//! The archives a package ships in, written the same way every time: a zip
//! (`.zip`, `.ipa`, `.aab`, the APK's payload) and a gzipped tar.
//!
//! The same files give the same bytes: entries in name order, one fixed
//! time, modes reduced to whether a file is executable, and no owner. A zip
//! is read back too, to move the entries `aapt2` writes into a package.

use std::fs;
use std::io::{self, Read, Write};
use std::path::Path;

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

use crate::io as at;

/// The time every entry carries: 1980-01-01 00:00, the zip format's first.
const EPOCH_UNIX: u64 = 315_532_800;
/// That time as a zip's MS-DOS date (time is 0).
const DOS_DATE: u16 = (1 << 5) | 1;

/// What an entry is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// A file, executable or not.
    File { executable: bool },
    /// A directory; its name ends in `/`.
    Directory,
    /// A symbolic link; its data is the path it points to.
    Link,
}

/// One entry of an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Its `/`-separated path in the archive.
    pub name: String,
    pub kind: Kind,
    pub data: Vec<u8>,
    /// Kept as it is rather than deflated: what is mapped straight from
    /// the archive, such as a native library or `resources.arsc`.
    pub stored: bool,
}

impl Entry {
    /// A file entry.
    pub fn file(name: impl Into<String>, data: Vec<u8>) -> Entry {
        Entry {
            name: name.into(),
            kind: Kind::File { executable: false },
            data,
            stored: false,
        }
    }

    /// The Unix mode it carries.
    fn mode(&self) -> u32 {
        match self.kind {
            Kind::File { executable: true } => 0o100_755,
            Kind::File { executable: false } => 0o100_644,
            Kind::Directory => 0o040_755,
            Kind::Link => 0o120_777,
        }
    }
}

/// Every entry under `dir`, named under `prefix` (`""` for none, else
/// ending in `/`), directories first in name order; `stored` says which
/// files to keep as they are.
pub fn entries(
    dir: &Path,
    prefix: &str,
    stored: &dyn Fn(&str) -> bool,
) -> Result<Vec<Entry>, String> {
    let mut out = Vec::new();
    if !prefix.is_empty() {
        out.push(Entry {
            name: prefix.to_owned(),
            kind: Kind::Directory,
            data: Vec::new(),
            stored: true,
        });
    }
    walk(dir, prefix, stored, &mut out)?;
    Ok(out)
}

fn walk(
    dir: &Path,
    prefix: &str,
    stored: &dyn Fn(&str) -> bool,
    out: &mut Vec<Entry>,
) -> Result<(), String> {
    let mut children: Vec<_> = at(dir, fs::read_dir(dir))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for path in children {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let full = format!("{prefix}{name}");
        let meta = at(&path, fs::symlink_metadata(&path))?;
        if meta.file_type().is_symlink() {
            let target = at(&path, fs::read_link(&path))?;
            out.push(Entry {
                name: full,
                kind: Kind::Link,
                data: target.to_string_lossy().replace('\\', "/").into_bytes(),
                stored: true,
            });
        } else if meta.is_dir() {
            let dir_name = format!("{full}/");
            out.push(Entry {
                name: dir_name.clone(),
                kind: Kind::Directory,
                data: Vec::new(),
                stored: true,
            });
            walk(&path, &dir_name, stored, out)?;
        } else {
            out.push(Entry {
                stored: stored(&full),
                kind: Kind::File {
                    executable: executable(&meta),
                },
                data: at(&path, fs::read(&path))?,
                name: full,
            });
        }
    }
    Ok(())
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> bool {
    false
}

/// Writes `entries`, sorted by name, as the zip `out`.
pub fn zip(entries: &mut [Entry], out: &Path) -> Result<(), String> {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    at(
        out,
        fs::write(out, zip_bytes(entries).map_err(|e| e.to_string())?),
    )
}

/// The zip of `entries`, in the order given. No entry or archive reaches
/// 4 GiB, so the format needs no 64-bit extension.
pub fn zip_bytes(entries: &[Entry]) -> io::Result<Vec<u8>> {
    let packed = entries
        .iter()
        .map(|entry| {
            let (method, data) = match entry.kind {
                Kind::File { .. } if !entry.stored && !entry.data.is_empty() => {
                    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::best());
                    encoder.write_all(&entry.data)?;
                    let deflated = encoder.finish()?;
                    if deflated.len() < entry.data.len() {
                        (DEFLATED, deflated)
                    } else {
                        (STORED, entry.data.clone())
                    }
                }
                _ => (STORED, entry.data.clone()),
            };
            Ok(Packed {
                entry,
                method,
                data,
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    zip_packed(&packed, Host::Unix)
}

/// A zip's compression methods.
pub(crate) const STORED: u16 = 0;
pub(crate) const DEFLATED: u16 = 8;
/// The size of an entry's local header, less its name.
pub(crate) const LOCAL_HEADER: usize = 30;

/// An entry with its data as the zip holds it, `method` saying how.
pub(crate) struct Packed<'a> {
    pub(crate) entry: &'a Entry,
    pub(crate) method: u16,
    pub(crate) data: Vec<u8>,
}

/// What a zip says it was made on: Unix, with UTF-8 names and each entry's
/// mode, or MS-DOS, with neither, as an app package's reader expects.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Host {
    Unix,
    Dos,
}

/// The zip of `packed`, in the order given.
pub(crate) fn zip_packed(packed: &[Packed<'_>], host: Host) -> io::Result<Vec<u8>> {
    let mut body = Vec::new();
    let mut central = Vec::new();
    for Packed {
        entry,
        method,
        data,
    } in packed
    {
        let crc = {
            let mut crc = flate2::Crc::new();
            crc.update(&entry.data);
            crc.sum()
        };
        let offset = small(body.len())?;
        let name = entry.name.as_bytes();
        let fields = |w: &mut Vec<u8>| -> io::Result<()> {
            w.extend_from_slice(&20u16.to_le_bytes());
            let flags: u16 = match host {
                // UTF-8 names.
                Host::Unix => 0x0800,
                Host::Dos => 0,
            };
            w.extend_from_slice(&flags.to_le_bytes());
            w.extend_from_slice(&method.to_le_bytes());
            w.extend_from_slice(&0u16.to_le_bytes());
            w.extend_from_slice(&DOS_DATE.to_le_bytes());
            w.extend_from_slice(&crc.to_le_bytes());
            w.extend_from_slice(&small(data.len())?.to_le_bytes());
            w.extend_from_slice(&small(entry.data.len())?.to_le_bytes());
            w.extend_from_slice(&(name.len() as u16).to_le_bytes());
            w.extend_from_slice(&0u16.to_le_bytes());
            Ok(())
        };
        body.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        fields(&mut body)?;
        body.extend_from_slice(name);
        body.extend_from_slice(data);

        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        let made_by: u16 = match host {
            // Made by Unix, so the mode in the external attributes is read.
            Host::Unix => (3 << 8) | 20,
            Host::Dos => 20,
        };
        central.extend_from_slice(&made_by.to_le_bytes());
        fields(&mut central)?;
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes());
        let dos = if entry.kind == Kind::Directory {
            0x10
        } else {
            0
        };
        let attributes = match host {
            Host::Unix => (entry.mode() << 16) | dos,
            Host::Dos => dos,
        };
        central.extend_from_slice(&attributes.to_le_bytes());
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(name);
    }
    let count = u16::try_from(packed.len())
        .map_err(|_| io::Error::other("more than 65535 entries in one archive"))?;
    let start = small(body.len())?;
    let size = small(central.len())?;
    body.extend_from_slice(&central);
    body.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    body.extend_from_slice(&[0; 4]);
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&count.to_le_bytes());
    body.extend_from_slice(&size.to_le_bytes());
    body.extend_from_slice(&start.to_le_bytes());
    body.extend_from_slice(&0u16.to_le_bytes());
    small(body.len())?;
    Ok(body)
}

fn small(len: usize) -> io::Result<u32> {
    u32::try_from(len).map_err(|_| io::Error::other("an archive of 4 GiB or more"))
}

/// The entries of the zip `bytes`, in the order it holds them, each
/// inflated and checked against its CRC.
pub fn unzip(bytes: &[u8]) -> Result<Vec<Entry>, String> {
    let bad = |why: &str| format!("not a zip archive: {why}");
    let u16_at = |at: usize| -> Result<u16, String> {
        bytes
            .get(at..at + 2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .ok_or_else(|| bad("truncated"))
    };
    let u32_at = |at: usize| -> Result<u32, String> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .ok_or_else(|| bad("truncated"))
    };
    let end = (0..=bytes.len().saturating_sub(22))
        .rev()
        .find(|&at| u32_at(at) == Ok(0x0605_4b50))
        .ok_or_else(|| bad("no end of central directory"))?;
    let count = usize::from(u16_at(end + 10)?);
    let mut at = u32_at(end + 16)? as usize;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        if u32_at(at)? != 0x0201_4b50 {
            return Err(bad("a broken central directory"));
        }
        let method = u16_at(at + 10)?;
        let crc = u32_at(at + 16)?;
        let packed = u32_at(at + 20)? as usize;
        let size = u32_at(at + 24)? as usize;
        let name_len = usize::from(u16_at(at + 28)?);
        let extra_len = usize::from(u16_at(at + 30)?);
        let comment_len = usize::from(u16_at(at + 32)?);
        let attributes = u32_at(at + 38)?;
        let local = u32_at(at + 42)? as usize;
        let name = bytes
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| bad("truncated"))?;
        let name = String::from_utf8_lossy(name).into_owned();
        at += 46 + name_len + extra_len + comment_len;

        let start =
            local + 30 + usize::from(u16_at(local + 26)?) + usize::from(u16_at(local + 28)?);
        let raw = bytes
            .get(start..start + packed)
            .ok_or_else(|| bad("truncated"))?;
        let data = match method {
            0 => raw.to_vec(),
            8 => {
                let mut data = Vec::with_capacity(size);
                DeflateDecoder::new(raw)
                    .read_to_end(&mut data)
                    .map_err(|e| bad(&e.to_string()))?;
                data
            }
            other => return Err(bad(&format!("`{name}` uses compression method {other}"))),
        };
        let mut sum = flate2::Crc::new();
        sum.update(&data);
        if sum.sum() != crc || data.len() != size {
            return Err(bad(&format!("`{name}` does not match its checksum")));
        }
        let mode = attributes >> 16;
        let kind = if name.ends_with('/') {
            Kind::Directory
        } else if mode & 0o170_000 == 0o120_000 {
            Kind::Link
        } else {
            Kind::File {
                executable: mode & 0o111 != 0,
            }
        };
        out.push(Entry {
            name,
            kind,
            data,
            stored: method == 0,
        });
    }
    Ok(out)
}

/// Writes `entries`, sorted by name, as the gzipped tar `out`.
pub fn tar_gz(entries: &mut [Entry], out: &Path) -> Result<(), String> {
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let mut tar = Vec::new();
    for entry in entries.iter() {
        tar_entry(&mut tar, entry).map_err(|why| format!("{}: {why}", entry.name))?;
    }
    // The end: two empty blocks.
    tar.extend_from_slice(&[0; 1024]);
    let mut gz = flate2::GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::best());
    let bytes = gz
        .write_all(&tar)
        .and_then(|()| gz.finish())
        .map_err(|e| e.to_string())?;
    at(out, fs::write(out, bytes))
}

/// One ustar header and its data, padded to the block.
fn tar_entry(tar: &mut Vec<u8>, entry: &Entry) -> Result<(), String> {
    let mut header = [0u8; 512];
    let name = entry.name.as_bytes();
    let (prefix, name) = if name.len() <= 100 {
        (&[][..], name)
    } else {
        let split = name[..name.len().min(156)]
            .iter()
            .rposition(|&b| b == b'/')
            .filter(|&at| name.len() - at - 1 <= 100 && at > 0)
            .ok_or("the path is too long for a tar entry")?;
        (&name[..split], &name[split + 1..])
    };
    header[..name.len()].copy_from_slice(name);
    let octal = |field: &mut [u8], value: u64| {
        let text = format!("{value:0width$o}", width = field.len() - 1);
        field[..text.len()].copy_from_slice(text.as_bytes());
    };
    octal(&mut header[100..108], u64::from(entry.mode() & 0o7777));
    octal(&mut header[108..116], 0);
    octal(&mut header[116..124], 0);
    let (flag, size, link) = match entry.kind {
        Kind::File { .. } => (b'0', entry.data.len() as u64, &[][..]),
        Kind::Directory => (b'5', 0, &[][..]),
        Kind::Link => (b'2', 0, &entry.data[..]),
    };
    if link.len() > 100 {
        return Err("the link's target is too long for a tar entry".to_owned());
    }
    octal(&mut header[124..136], size);
    octal(&mut header[136..148], EPOCH_UNIX);
    header[156] = flag;
    header[157..157 + link.len()].copy_from_slice(link);
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    header[345..345 + prefix.len()].copy_from_slice(prefix);
    // The checksum is taken with its own field as spaces.
    header[148..156].copy_from_slice(b"        ");
    let sum: u32 = header.iter().map(|&b| u32::from(b)).sum();
    let text = format!("{sum:06o}\0 ");
    header[148..156].copy_from_slice(text.as_bytes());
    tar.extend_from_slice(&header);
    if flag == b'0' {
        tar.extend_from_slice(&entry.data);
        let pad = (512 - entry.data.len() % 512) % 512;
        tar.extend(std::iter::repeat_n(0, pad));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            Entry {
                name: "app/".into(),
                kind: Kind::Directory,
                data: Vec::new(),
                stored: true,
            },
            Entry {
                name: "app/run".into(),
                kind: Kind::File { executable: true },
                data: b"#!/bin/sh\n".to_vec(),
                stored: false,
            },
            Entry::file("app/readme.txt", "text ".repeat(100).into_bytes()),
            Entry {
                name: "app/lib.so".into(),
                kind: Kind::File { executable: false },
                data: vec![7; 300],
                stored: true,
            },
        ]
    }

    #[test]
    fn a_zip_reads_back_as_it_was_written_and_is_the_same_each_time() {
        let entries = sample();
        let bytes = zip_bytes(&entries).unwrap();
        assert_eq!(bytes, zip_bytes(&entries).unwrap());
        let read = unzip(&bytes).unwrap();
        assert_eq!(read.len(), 4);
        for (written, read) in entries.iter().zip(&read) {
            assert_eq!(written.name, read.name);
            assert_eq!(written.kind, read.kind);
            assert_eq!(written.data, read.data);
        }
        // Text deflates; a stored file stays as it is.
        assert!(!read[2].stored);
        assert!(read[3].stored);
        assert!(bytes.len() < 500 + 300);
    }

    #[test]
    fn a_broken_zip_is_refused() {
        let mut bytes = zip_bytes(&sample()).unwrap();
        assert!(unzip(&bytes[..10]).is_err());
        // A flipped byte in the stored library fails its checksum.
        let at = bytes.windows(4).position(|w| w == [7, 7, 7, 7]).unwrap();
        bytes[at] = 8;
        assert!(unzip(&bytes).unwrap_err().contains("checksum"));
    }

    #[test]
    fn a_tar_has_a_header_a_block_for_each_entry() {
        let dir = std::env::temp_dir().join(format!("viso-archive-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("a.tar.gz");
        let mut entries = sample();
        tar_gz(&mut entries, &out).unwrap();
        let mut tar = Vec::new();
        flate2::read::GzDecoder::new(&fs::read(&out).unwrap()[..])
            .read_to_end(&mut tar)
            .unwrap();
        // Sorted: the directory, the library, the readme, the script.
        assert_eq!(&tar[..4], b"app/");
        assert_eq!(&tar[257..262], b"ustar");
        assert_eq!(&tar[100..107], b"0000755");
        let headers = 4 * 512;
        let data = 512 + 512 + 512;
        assert_eq!(tar.len(), headers + data + 1024);
        fs::remove_dir_all(&dir).unwrap();
    }
}
