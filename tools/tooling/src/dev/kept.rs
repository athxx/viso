//! The state `viso run --keep-state` keeps between sessions: the app's dev
//! snapshot, taken for the disk as the session stops (every `@sensitive`
//! state and secure field's text left out), with the text of each view the
//! app ran and where its windows were. One file per target under the
//! ignored dev cache, readable by its owner alone; the next session with
//! the flag takes it, removing it, and restores it into its first launch.

use std::io;
use std::path::{Path, PathBuf};

use viso_ende::{Decoder, Encoder};

use viso_dev_protocol::wire::{DEV_PROTOCOL_VERSION, MAX_FILES, MAX_PATH, WindowGeometry};

const MAGIC: &[u8; 4] = b"VKS1";

/// The most bytes a kept view's text or the snapshot holds.
const MAX_TEXT: usize = 16 << 20;
const MAX_SNAPSHOT: usize = 256 << 20;
const MAX_WINDOWS: usize = 256;

/// What a session kept for the next.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Kept {
    /// Where the app's windows were.
    pub windows: Vec<WindowGeometry>,
    /// The text of each view the app ran, by path.
    pub views: Vec<(PathBuf, String)>,
    /// The snapshot, as the app encoded it.
    pub snapshot: Vec<u8>,
}

/// Where the state kept for `target` is, under the dev cache `dev`.
pub fn path(dev: &Path, target: &str) -> PathBuf {
    dev.join("kept").join(format!("{target}.state"))
}

impl Kept {
    fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        enc.write_raw(MAGIC);
        enc.write_u16(DEV_PROTOCOL_VERSION);
        enc.write_varint(self.windows.len() as u64);
        for window in &self.windows {
            enc.write_f64(window.size.0);
            enc.write_f64(window.size.1);
            enc.write_bool(window.position.is_some());
            if let Some((x, y)) = window.position {
                enc.write_f64(x);
                enc.write_f64(y);
            }
        }
        enc.write_varint(self.views.len() as u64);
        for (path, text) in &self.views {
            enc.write_str(&path.to_string_lossy());
            enc.write_str(text);
        }
        enc.write_bytes(&self.snapshot);
        enc.into_bytes()
    }

    fn decode(bytes: &[u8]) -> Result<Kept, String> {
        let malformed = |_| "it does not read as kept state".to_owned();
        let mut dec = Decoder::new(bytes);
        if dec.read_raw(MAGIC.len()).map_err(malformed)? != MAGIC {
            return Err("it is not kept state".into());
        }
        if dec.read_u16().map_err(malformed)? != DEV_PROTOCOL_VERSION {
            return Err("another version of `viso` kept it".into());
        }
        let count = |dec: &mut Decoder<'_>, max: usize| {
            let n = dec.read_varint().map_err(malformed)?;
            usize::try_from(n)
                .ok()
                .filter(|&n| n <= max)
                .ok_or_else(|| "it lists too much".to_owned())
        };
        let string = |dec: &mut Decoder<'_>, max: usize| {
            let text = dec.read_str().map_err(malformed)?;
            if text.len() > max {
                return Err("it holds too long a text".to_owned());
            }
            Ok(text.to_owned())
        };
        let mut kept = Kept::default();
        for _ in 0..count(&mut dec, MAX_WINDOWS)? {
            let size = (
                dec.read_f64().map_err(malformed)?,
                dec.read_f64().map_err(malformed)?,
            );
            let position = if dec.read_bool().map_err(malformed)? {
                Some((
                    dec.read_f64().map_err(malformed)?,
                    dec.read_f64().map_err(malformed)?,
                ))
            } else {
                None
            };
            kept.windows.push(WindowGeometry { size, position });
        }
        for _ in 0..count(&mut dec, MAX_FILES)? {
            let path = PathBuf::from(string(&mut dec, MAX_PATH)?);
            kept.views.push((path, string(&mut dec, MAX_TEXT)?));
        }
        let snapshot = dec.read_bytes().map_err(malformed)?;
        if snapshot.len() > MAX_SNAPSHOT {
            return Err("its snapshot is too large".into());
        }
        kept.snapshot = snapshot.to_vec();
        if !dec.is_empty() {
            return Err("it does not end where it should".into());
        }
        Ok(kept)
    }

    /// Writes `self` to `path`: beside it first, then moved over it, the
    /// file and its directory readable by their owner alone.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let part = path.with_extension("part");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            options.mode(0o600);
        }
        let written = options.open(&part).and_then(|mut file| {
            use std::io::Write;
            file.write_all(&self.encode())?;
            file.sync_all()
        });
        match written.and_then(|()| std::fs::rename(&part, path)) {
            Ok(()) => Ok(()),
            Err(error) => {
                let _ = std::fs::remove_file(&part);
                Err(error)
            }
        }
    }

    /// Takes the state kept at `path`, removing it: `None` when there is
    /// none, `Err` with why it cannot be read.
    pub fn take(path: &Path) -> Option<Result<Kept, String>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => return Some(Err(error.to_string())),
        };
        let _ = std::fs::remove_file(path);
        Some(Kept::decode(&bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kept_state_is_written_for_its_owner_and_taken_once() {
        let dir = std::env::temp_dir().join(format!("viso-kept-{}", std::process::id()));
        let path = path(&dir, "host");
        let kept = Kept {
            windows: vec![
                WindowGeometry {
                    size: (800.0, 600.0),
                    position: Some((10.0, 20.0)),
                },
                WindowGeometry {
                    size: (300.0, 200.0),
                    position: None,
                },
            ],
            views: vec![("/app/main.vs".into(), "component Main {}".into())],
            snapshot: vec![1, 2, 3],
        };
        kept.write(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(Kept::take(&path), Some(Ok(kept)));
        assert_eq!(Kept::take(&path), None, "taken once");

        std::fs::write(&path, b"VKS1\x00\x00").unwrap();
        assert!(matches!(Kept::take(&path), Some(Err(_))));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
