//! Reading a zip archive: its central directory, and each entry, stored or
//! deflated, streamed to disk with its CRC checked. ZIP64 sizes, offsets and
//! counts are read, so an archive or an entry past 4 GiB opens.
//!
//! Extraction never writes outside the directory it is given: a name that is
//! absolute or climbs out with `..` is refused, and links are made after
//! every file, each refused when it points outside, so no later entry can be
//! written through one.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
const END64: u32 = 0x0606_4b50;
const END64_LOCATOR: u32 = 0x0706_4b50;

/// An entry of the central directory.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    /// Its path in the archive, `/`-separated; a directory's ends in `/`.
    pub(crate) name: String,
    method: u16,
    flags: u16,
    crc: u32,
    compressed: u64,
    size: u64,
    /// Where its local header starts.
    offset: u64,
    /// Its Unix mode, when the archive was made on Unix.
    mode: Option<u32>,
}

impl Entry {
    pub(crate) fn is_dir(&self) -> bool {
        self.name.ends_with('/')
    }

    pub(crate) fn is_link(&self) -> bool {
        self.mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
    }
}

/// An open archive.
pub(crate) struct Archive {
    file: BufReader<File>,
    entries: Vec<Entry>,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().expect("four bytes"))
}

fn u64_at(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().expect("eight bytes"))
}

impl Archive {
    /// Opens the archive at `path` and reads its central directory.
    pub(crate) fn open(path: &Path) -> io::Result<Archive> {
        let mut file = BufReader::new(File::open(path)?);
        let len = file.seek(SeekFrom::End(0))?;
        // The end record is 22 bytes and a comment of up to 64 KiB.
        let tail_len = len.min(22 + 0xFFFF);
        let mut tail = vec![0; tail_len as usize];
        file.seek(SeekFrom::Start(len - tail_len))?;
        file.read_exact(&mut tail)?;
        let end = (0..tail.len().saturating_sub(21))
            .rev()
            .find(|&at| u32_at(&tail, at) == END)
            .ok_or_else(|| invalid("not a zip archive: no end of central directory"))?;
        let mut count = u64::from(u16_at(&tail, end + 10));
        let mut cd_size = u64::from(u32_at(&tail, end + 12));
        let mut cd_offset = u64::from(u32_at(&tail, end + 16));
        let end_at = len - tail_len + end as u64;
        if end >= 20 && u32_at(&tail, end - 20) == END64_LOCATOR {
            let at = u64_at(&tail, end - 12);
            let mut record = [0; 56];
            file.seek(SeekFrom::Start(at))?;
            file.read_exact(&mut record)?;
            if u32_at(&record, 0) != END64 {
                return Err(invalid("a broken ZIP64 end record"));
            }
            count = u64_at(&record, 32);
            cd_size = u64_at(&record, 40);
            cd_offset = u64_at(&record, 48);
        }
        if cd_offset
            .checked_add(cd_size)
            .is_none_or(|end| end > end_at)
        {
            return Err(invalid("the central directory runs past the archive"));
        }
        let mut directory = vec![0; cd_size as usize];
        file.seek(SeekFrom::Start(cd_offset))?;
        file.read_exact(&mut directory)?;
        let mut entries = Vec::with_capacity(count.min(1 << 20) as usize);
        let mut at = 0;
        while at + 46 <= directory.len() && u32_at(&directory, at) == CENTRAL {
            let header = &directory[at..];
            let made_on_unix = u16_at(header, 4) >> 8 == 3;
            let name_len = usize::from(u16_at(header, 28));
            let extra_len = usize::from(u16_at(header, 30));
            let comment_len = usize::from(u16_at(header, 32));
            let next = 46 + name_len + extra_len + comment_len;
            if header.len() < next {
                return Err(invalid("a central directory entry runs past it"));
            }
            let mut entry = Entry {
                name: String::from_utf8_lossy(&header[46..46 + name_len]).into_owned(),
                flags: u16_at(header, 8),
                method: u16_at(header, 10),
                crc: u32_at(header, 16),
                compressed: u64::from(u32_at(header, 20)),
                size: u64::from(u32_at(header, 24)),
                offset: u64::from(u32_at(header, 42)),
                mode: made_on_unix.then(|| u32_at(header, 38) >> 16),
            };
            zip64(
                &mut entry,
                &header[46 + name_len..46 + name_len + extra_len],
            );
            entries.push(entry);
            at += next;
        }
        if entries.len() as u64 != count {
            return Err(invalid(format!(
                "the central directory lists {} entries, its end record {count}",
                entries.len()
            )));
        }
        Ok(Archive { file, entries })
    }

    pub(crate) fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The bytes of `entry`, uncompressed.
    fn reader(&mut self, entry: &Entry) -> io::Result<Box<dyn Read + '_>> {
        if entry.flags & 1 != 0 {
            return Err(invalid(format!("`{}` is encrypted", entry.name)));
        }
        let mut local = [0; 30];
        self.file.seek(SeekFrom::Start(entry.offset))?;
        self.file.read_exact(&mut local)?;
        if u32_at(&local, 0) != LOCAL {
            return Err(invalid(format!("`{}` has no local header", entry.name)));
        }
        let skip = i64::from(u16_at(&local, 26)) + i64::from(u16_at(&local, 28));
        self.file.seek(SeekFrom::Current(skip))?;
        let data = (&mut self.file).take(entry.compressed);
        Ok(match entry.method {
            0 => Box::new(data),
            8 => Box::new(flate2::read::DeflateDecoder::new(data)),
            method => {
                return Err(invalid(format!(
                    "`{}` is compressed by method {method}",
                    entry.name
                )));
            }
        })
    }

    /// Copies `entry` to `out`, checking its size and CRC.
    fn copy(&mut self, entry: &Entry, out: &mut impl Write) -> io::Result<()> {
        let mut reader = self.reader(entry)?;
        let mut crc = flate2::Crc::new();
        let mut buffer = vec![0; 256 * 1024];
        let mut total = 0u64;
        loop {
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            crc.update(&buffer[..n]);
            out.write_all(&buffer[..n])?;
            total += n as u64;
        }
        if total != entry.size || crc.sum() != entry.crc {
            return Err(invalid(format!("`{}` does not match its CRC", entry.name)));
        }
        Ok(())
    }

    /// Extracts the entries `keep` accepts into `dir`, each at the path
    /// `rename` gives its name (`None` leaves it out). Files keep their
    /// executable bits; links are made last. An interrupt stops it with an
    /// `Interrupted` error.
    pub(crate) fn extract(
        &mut self,
        dir: &Path,
        rename: impl Fn(&str) -> Option<String>,
    ) -> io::Result<()> {
        let entries = self.entries.clone();
        let mut links = Vec::new();
        for entry in &entries {
            if crate::interrupt::count() > 0 {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "interrupted"));
            }
            let Some(name) = rename(&entry.name) else {
                continue;
            };
            let relative = safe(&name)
                .ok_or_else(|| invalid(format!("`{}` would land outside", entry.name)))?;
            let path = dir.join(&relative);
            if entry.is_dir() {
                fs::create_dir_all(&path)?;
                continue;
            }
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if entry.is_link() {
                let mut target = Vec::new();
                self.copy(entry, &mut target)?;
                links.push((relative, String::from_utf8_lossy(&target).into_owned()));
                continue;
            }
            let mut out = BufWriter::new(File::create(&path)?);
            self.copy(entry, &mut out)?;
            out.flush()?;
            drop(out);
            set_mode(&path, entry.mode)?;
        }
        for (relative, target) in links {
            let resolved = relative
                .parent()
                .map(|parent| parent.join(&target))
                .unwrap_or_else(|| PathBuf::from(&target));
            if Path::new(&target).is_absolute() || safe(&resolved.to_string_lossy()).is_none() {
                return Err(invalid(format!(
                    "the link `{}` points outside, at `{target}`",
                    relative.display()
                )));
            }
            link(&target, &dir.join(relative))?;
        }
        Ok(())
    }
}

/// Reads the ZIP64 extra field into the sizes and offset its 32-bit fields
/// left at their maximum.
fn zip64(entry: &mut Entry, mut extra: &[u8]) {
    while extra.len() >= 4 {
        let id = u16_at(extra, 0);
        let len = usize::from(u16_at(extra, 2));
        let Some(body) = extra.get(4..4 + len) else {
            return;
        };
        if id == 1 {
            let mut at = 0;
            let mut next = |field: &mut u64| {
                if *field == u64::from(u32::MAX) && at + 8 <= body.len() {
                    *field = u64_at(body, at);
                    at += 8;
                }
            };
            next(&mut entry.size);
            next(&mut entry.compressed);
            next(&mut entry.offset);
            return;
        }
        extra = &extra[4 + len..];
    }
}

/// `name` as a relative path that stays inside the directory it is joined
/// to, `..` resolved; `None` when it is absolute or climbs out.
fn safe(name: &str) -> Option<PathBuf> {
    let name = name.replace('\\', "/");
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for component in Path::new(&name).components() {
        match component {
            Component::Normal(part) => parts.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    (!parts.is_empty()).then(|| parts.iter().collect())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: Option<u32>) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let Some(mode) = mode.map(|m| m & 0o777).filter(|m| *m != 0) else {
        return Ok(());
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o600))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: Option<u32>) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn link(target: &str, path: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, path)
}

/// Without symbolic links, a link is a copy of what it points at.
#[cfg(not(unix))]
fn link(target: &str, path: &Path) -> io::Result<()> {
    let source = path.parent().unwrap_or(Path::new("")).join(target);
    fs::copy(source, path).map(drop)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An archive of `files` (name, bytes, Unix mode), deflated.
    pub(crate) fn archive(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
        use flate2::Compression;
        use flate2::write::DeflateEncoder;
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, bytes, mode) in files {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(bytes).unwrap();
            let data = encoder.finish().unwrap();
            let mut crc = flate2::Crc::new();
            crc.update(bytes);
            let offset = out.len() as u32;
            let common = |buf: &mut Vec<u8>| {
                buf.extend_from_slice(&20u16.to_le_bytes());
                buf.extend_from_slice(&0u16.to_le_bytes());
                buf.extend_from_slice(&8u16.to_le_bytes());
                buf.extend_from_slice(&[0; 4]);
                buf.extend_from_slice(&crc.sum().to_le_bytes());
                buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
                buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
                buf.extend_from_slice(&0u16.to_le_bytes());
            };
            out.extend_from_slice(&LOCAL.to_le_bytes());
            common(&mut out);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&data);
            central.extend_from_slice(&CENTRAL.to_le_bytes());
            central.extend_from_slice(&(3u16 << 8 | 20).to_le_bytes());
            common(&mut central);
            // Comment length, disk and internal attributes.
            central.extend_from_slice(&[0; 6]);
            central.extend_from_slice(&(mode << 16).to_le_bytes());
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let cd_offset = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&END.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&cd_offset.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("viso-zip-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn entries_extract_renamed_with_their_modes_and_links() {
        let dir = scratch("extract");
        let zip = dir.join("a.zip");
        fs::write(
            &zip,
            archive(&[
                ("top/", b"", 0o40_755),
                ("top/bin/tool", b"#!/bin/sh\n", 0o100_755),
                ("top/lib/data.txt", b"data", 0o100_644),
                ("top/lib/alias", b"data.txt", 0o120_777),
                ("top/skip.txt", b"left out", 0o100_644),
            ]),
        )
        .unwrap();
        let mut archive = Archive::open(&zip).unwrap();
        assert_eq!(archive.entries().len(), 5);
        let out = dir.join("out");
        archive
            .extract(&out, |name| {
                let rest = name.strip_prefix("top/")?;
                (!rest.is_empty() && rest != "skip.txt").then(|| rest.to_owned())
            })
            .unwrap();
        assert_eq!(fs::read(out.join("lib/data.txt")).unwrap(), b"data");
        assert!(!out.join("skip.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(out.join("bin/tool"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
            assert_eq!(
                fs::read_link(out.join("lib/alias")).unwrap(),
                Path::new("data.txt")
            );
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_name_or_link_out_of_the_directory_is_refused() {
        let dir = scratch("escape");
        for (name, mode) in [("../evil", 0o100_644), ("/etc/evil", 0o100_644)] {
            let zip = dir.join("bad.zip");
            fs::write(&zip, archive(&[(name, b"x", mode)])).unwrap();
            let error = Archive::open(&zip)
                .unwrap()
                .extract(&dir.join("out"), |n| Some(n.to_owned()))
                .unwrap_err();
            assert!(error.to_string().contains("outside"), "{error}");
        }
        let zip = dir.join("link.zip");
        fs::write(&zip, archive(&[("a/link", b"../../outside", 0o120_777)])).unwrap();
        let error = Archive::open(&zip)
            .unwrap()
            .extract(&dir.join("out"), |n| Some(n.to_owned()))
            .unwrap_err();
        assert!(error.to_string().contains("points outside"), "{error}");
        assert!(!dir.join("evil").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_corrupt_entry_fails_its_crc_and_a_truncated_archive_does_not_open() {
        let dir = scratch("corrupt");
        let mut bytes = archive(&[("f.txt", &[7; 4096], 0o100_644)]);
        let zip = dir.join("c.zip");
        fs::write(&zip, &bytes[..bytes.len() - 10]).unwrap();
        assert!(Archive::open(&zip).is_err());
        // A flipped byte of the deflated data.
        bytes[40] ^= 0xFF;
        fs::write(&zip, &bytes).unwrap();
        let result = Archive::open(&zip)
            .unwrap()
            .extract(&dir.join("out"), |n| Some(n.to_owned()));
        assert!(result.is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
