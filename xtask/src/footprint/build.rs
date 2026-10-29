//! Building the subjects, and the sizes of what was built.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Row, Scene};

/// The Cargo profile every subject is built with, defined on the command
/// line so neither workspace's nor the user's `[profile.release]` changes
/// what is measured, and kept apart from their release builds.
pub(super) const PROFILE: &str = "footprint";

/// The settings of [`PROFILE`].
const PROFILE_CONFIG: &[&str] = &[
    "inherits=\"release\"",
    "opt-level=3",
    "lto=\"thin\"",
    "codegen-units=1",
    "panic=\"unwind\"",
    "strip=\"symbols\"",
    "debug=false",
    "incremental=false",
];

/// The settings of [`PROFILE`], as the baseline records them.
pub(super) fn profile_settings() -> String {
    PROFILE_CONFIG
        .iter()
        .filter(|s| !s.starts_with("inherits"))
        .map(|s| s.replace('"', ""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// What to build.
pub(super) enum Build {
    /// `benches/footprint` with the facade features of a profile.
    Viso {
        features: &'static [&'static str],
        default_features: bool,
        env: &'static [(&'static str, &'static str)],
        /// The triple it is cross-built for; the host's when `None`.
        triple: Option<String>,
    },
    /// `benches/footprint/makepad` against the checkout.
    Makepad { checkout: PathBuf },
    /// A package of the checkout's own workspace.
    MakepadExample {
        checkout: PathBuf,
        package: &'static str,
    },
}

/// What a build gave: an executable per scene, and how it is watched.
pub(super) struct Built {
    counter: PathBuf,
    list: Option<PathBuf>,
    /// The scene is the executable's first argument, not its own executable.
    pub(super) scene_argument: bool,
    /// The variable that makes the app say when it submits its first frame,
    /// and what it says.
    pub(super) first_frame: (&'static str, &'static str),
    /// Whether it answers the control protocol on stdin.
    pub(super) control: bool,
}

impl Built {
    pub(super) fn binary(&self, scene: Scene) -> Option<&Path> {
        match scene {
            Scene::Counter => Some(&self.counter),
            Scene::List => self.list.as_deref(),
        }
    }
}

/// The target directory every build shares: `CARGO_TARGET_DIR`, else the
/// workspace's.
pub(super) fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::workspace_root().join("target"))
}

/// `cargo build` of the package in `dir` with [`PROFILE`], under `target`.
fn cargo(dir: &Path, target: &Path) -> Command {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(dir)
        .env("CARGO_TARGET_DIR", target)
        .arg("build")
        .arg("--profile")
        .arg(PROFILE);
    for setting in PROFILE_CONFIG {
        command
            .arg("--config")
            .arg(format!("profile.{PROFILE}.{setting}"));
    }
    command
}

fn status(mut command: Command, what: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|e| format!("cannot run cargo for {what}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("the build of {what} failed ({status})"))
    }
}

/// Builds `build`; `scratch` holds the crate the Makepad apps build in.
pub(super) fn build(
    build: &Build,
    root: &Path,
    target: &Path,
    scratch: &Path,
) -> Result<Built, String> {
    let out = target.join(PROFILE);
    match build {
        Build::Viso {
            features,
            default_features,
            env,
            triple,
        } => {
            let mut command = cargo(root, target);
            command.args(["-p", "viso-footprint"]);
            let out = match triple {
                Some(triple) => {
                    command.args(["--target", triple]);
                    target.join(triple).join(PROFILE)
                }
                None => out,
            };
            if !default_features {
                command.arg("--no-default-features");
            }
            if !features.is_empty() {
                command.arg("--features").arg(features.join(","));
            }
            command.envs(env.iter().copied());
            status(command, "the reference app")?;
            // Every profile's build is uplifted to the same path: keep this
            // one apart until it is measured.
            let mut name: String = features
                .iter()
                .map(|f| f.trim_start_matches("viso/"))
                .collect::<Vec<_>>()
                .join("-");
            if let Some(triple) = triple {
                name = format!("{name}-{triple}");
            }
            let kept = scratch.join(format!("viso-footprint-{name}"));
            std::fs::copy(out.join("viso-footprint"), &kept)
                .map_err(|e| format!("cannot keep the reference app: {e}"))?;
            Ok(Built {
                counter: kept.clone(),
                list: Some(kept),
                scene_argument: true,
                first_frame: ("VISO_DEBUG", "viso: first frame submitting"),
                control: true,
            })
        }
        Build::Makepad { checkout } => {
            let dir = makepad_crate(root, checkout, scratch)?;
            status(cargo(&dir, target), "the Makepad apps")?;
            Ok(Built {
                counter: out.join("makepad-footprint-counter"),
                list: Some(out.join("makepad-footprint-list")),
                scene_argument: false,
                first_frame: ("MAKEPAD_TOTAL_DRAWCALLS_DEBUG", "total_drawcalls"),
                control: true,
            })
        }
        Build::MakepadExample { checkout, package } => {
            let mut command = cargo(checkout, target);
            command.args(["-p", package]);
            status(command, package)?;
            Ok(Built {
                counter: out.join(package),
                list: None,
                scene_argument: false,
                first_frame: ("MAKEPAD_TOTAL_DRAWCALLS_DEBUG", "total_drawcalls"),
                control: false,
            })
        }
    }
}

/// Writes the crate the Makepad apps build in under `scratch`: their
/// sources in `benches/footprint/makepad`, `makepad-widgets` from the
/// checkout, and the checkout's lock file and Cargo configuration, so its
/// dependencies resolve and compile as in the checkout's own workspace.
fn makepad_crate(root: &Path, checkout: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let dir = scratch.join("makepad");
    let sources = root.join("benches/footprint/makepad");
    std::fs::create_dir_all(dir.join(".cargo"))
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let manifest = format!(
        "[package]\nname = \"makepad-footprint\"\nversion = \"0.0.0\"\nedition = \"2024\"\npublish = false\n\n\
         [[bin]]\nname = \"makepad-footprint-counter\"\npath = {counter:?}\n\n\
         [[bin]]\nname = \"makepad-footprint-list\"\npath = {list:?}\n\n\
         [dependencies]\nmakepad-widgets = {{ path = {widgets:?} }}\n\n[workspace]\n",
        counter = sources.join("counter.rs"),
        list = sources.join("list.rs"),
        widgets = checkout.join("widgets"),
    );
    let write = |path: PathBuf, text: &[u8]| {
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    };
    write(dir.join("Cargo.toml"), manifest.as_bytes())?;
    for (from, to) in [
        ("Cargo.lock", "Cargo.lock"),
        (".cargo/config.toml", ".cargo/config.toml"),
    ] {
        if let Ok(bytes) = std::fs::read(checkout.join(from)) {
            write(dir.join(to), &bytes)?;
        }
    }
    Ok(dir)
}

/// The executable's size and its segments: code and read-only data,
/// initialized static data, and zero-filled static data.
pub(super) fn sizes(binary: &Path, row: &mut Row) -> Result<(), String> {
    let bytes = std::fs::metadata(binary)
        .map_err(|e| format!("cannot read {}: {e}", binary.display()))?
        .len();
    row.set("binary_bytes", Some(bytes as f64));
    let file =
        std::fs::read(binary).map_err(|e| format!("cannot read {}: {e}", binary.display()))?;
    let sections = if file.starts_with(b"\x7fELF") {
        elf_sections(&file)
    } else {
        Command::new("size")
            .arg("-m")
            .arg(binary)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| mach_o_sections(&String::from_utf8_lossy(&o.stdout)))
    };
    if let Some(sections) = sections {
        row.set("text_bytes", Some(sections.text as f64));
        row.set("data_bytes", Some(sections.data as f64));
        row.set("bss_bytes", Some(sections.bss as f64));
    }
    Ok(())
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Sections {
    text: u64,
    data: u64,
    bss: u64,
}

/// The sections `size -m` lists: the `__TEXT` segment, the initialized and
/// the zero-filled sections of the `__DATA*` segments.
fn mach_o_sections(text: &str) -> Sections {
    let mut sections = Sections::default();
    let mut segment = "";
    for line in text.lines() {
        let number = |rest: &str| -> u64 {
            rest.split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        };
        if let Some(rest) = line.strip_prefix("Segment ") {
            let Some((name, size)) = rest.split_once(':') else {
                continue;
            };
            segment = if name.starts_with("__DATA") {
                "data"
            } else {
                ""
            };
            if name == "__TEXT" {
                sections.text = number(size);
            }
        } else if let Some(rest) = line.trim_start().strip_prefix("Section ")
            && segment == "data"
            && let Some((_, size)) = rest.split_once(':')
        {
            if size.contains("zerofill") {
                sections.bss += number(size);
            } else {
                sections.data += number(size);
            }
        }
    }
    sections
}

/// An ELF executable's allocated sections, read from its section headers
/// as `size` counts them: read-only ones (code, constants, unwind tables) as
/// text, writable ones as data, those the loader zero-fills as bss. `None`
/// for a header that does not parse (a big-endian ELF among them).
fn elf_sections(file: &[u8]) -> Option<Sections> {
    const SHT_NOBITS: u32 = 8;
    const SHF_WRITE: u64 = 1;
    const SHF_ALLOC: u64 = 2;
    let wide = match file.get(4)? {
        1 => false,
        2 => true,
        _ => return None,
    };
    if *file.get(5)? != 1 {
        return None;
    }
    let u16_at = |at: usize| Some(u16::from_le_bytes(file.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(file.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(file.get(at..at + 8)?.try_into().ok()?));
    let word = |at: usize| {
        if wide {
            u64_at(at)
        } else {
            u32_at(at).map(u64::from)
        }
    };
    let (shoff, shentsize, shnum) = if wide {
        (u64_at(0x28)?, u16_at(0x3a)?, u16_at(0x3c)?)
    } else {
        (u64::from(u32_at(0x20)?), u16_at(0x2e)?, u16_at(0x30)?)
    };
    let mut sections = Sections::default();
    for k in 0..usize::from(shnum) {
        let at = usize::try_from(shoff).ok()? + k * usize::from(shentsize);
        let kind = u32_at(at + 4)?;
        let (flags, size) = if wide {
            (u64_at(at + 8)?, u64_at(at + 32)?)
        } else {
            (word(at + 8)?, word(at + 20)?)
        };
        if flags & SHF_ALLOC == 0 {
            continue;
        }
        if kind == SHT_NOBITS {
            sections.bss += size;
        } else if flags & SHF_WRITE != 0 {
            sections.data += size;
        } else {
            sections.text += size;
        }
    }
    Some(sections)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_segments_size_lists() {
        let text = "Segment __PAGEZERO: 4294967296 (zero fill) \n\
                    Segment __TEXT: 11337728\n\
                    \tSection __text: 8817008\n\
                    \tSection __const: 1081316\n\
                    Segment __DATA_CONST: 425984\n\
                    \tSection __const: 425416\n\
                    Segment __DATA: 32768\n\
                    \tSection __data: 6520\n\
                    \tSection __common: 512 (zerofill)\n\
                    \tSection __bss: 12624 (zerofill)\n\
                    Segment __LINKEDIT: 475136\n";
        assert_eq!(
            mach_o_sections(text),
            Sections {
                text: 11337728,
                data: 425416 + 6520,
                bss: 512 + 12624,
            }
        );
        // A 64-bit ELF of three allocated sections, code, data and bss, and
        // one that is not loaded.
        let mut elf = vec![0u8; 64];
        elf[..6].copy_from_slice(b"\x7fELF\x02\x01");
        elf[0x28..0x30].copy_from_slice(&64u64.to_le_bytes());
        elf[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        elf[0x3c..0x3e].copy_from_slice(&4u16.to_le_bytes());
        for (kind, flags, size) in [(1u32, 6u64, 1200u64), (1, 3, 30), (8, 3, 7), (1, 0, 99)] {
            let mut header = [0u8; 64];
            header[4..8].copy_from_slice(&kind.to_le_bytes());
            header[8..16].copy_from_slice(&flags.to_le_bytes());
            header[32..40].copy_from_slice(&size.to_le_bytes());
            elf.extend_from_slice(&header);
        }
        assert_eq!(
            elf_sections(&elf),
            Some(Sections {
                text: 1200,
                data: 30,
                bss: 7,
            })
        );
        assert_eq!(elf_sections(b"\x7fELF"), None);
    }

    #[test]
    fn the_profile_is_recorded_without_its_parent() {
        assert_eq!(
            profile_settings(),
            "opt-level=3 lto=thin codegen-units=1 panic=unwind strip=symbols debug=false incremental=false"
        );
    }
}
