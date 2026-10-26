//! A small Cargo package whose `viso` is a stand-in with the `hot-reload`
//! and `headless` features, the default `vm`, `desktop` and `native-vulkan`,
//! the embedded profiles' `gles`, `cpu` and `kms`, and `emulate`,
//! and `viso` run over it with its own `target/`: what the tests
//! of `viso build` and `viso run` drive without building the framework.

#![allow(dead_code, reason = "each test file uses its part")]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub fn viso(cwd: &Path, args: &[&str]) -> Output {
    viso_with(cwd, args, &[])
}

/// As [`viso`], with the variables `env` set besides.
pub fn viso_with(cwd: &Path, args: &[&str], env: &[(&str, String)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_viso"));
    command.args(args).current_dir(cwd);
    command.envs(env.iter().map(|(name, value)| (name, value)));
    for name in [
        "VISO_PROFILE",
        "VISO_OPT_LEVEL",
        "VISO_SOURCE_MAPS",
        "VISO_STRIP",
        "VISO_FOOTPRINT",
        "VISO_BUDGET",
        "VISO_WORKERS",
        "VISO_FONT_TEXT",
        "VISO_FONT_CHARSET",
    ] {
        command.env_remove(name);
    }
    // Each project builds in its own `target/`, whatever Cargo is
    // configured with.
    command.env("CARGO_TARGET_DIR", cwd.join("target"));
    command.output().unwrap()
}

pub fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// The app's `main`: the profile it was built as and whether the Dev
/// Runtime's hot reload is in it.
pub const MAIN: &str = "fn main() {\n    println!(\"{} {}\", option_env!(\"VISO_PROFILE\").unwrap_or(\"-\"), viso::HOT_RELOAD);\n}\n";

/// A project `label`: a Cargo package and its `Viso.toml`, its `viso` a
/// path package with the `hot-reload` and `headless` features, in a directory of its own.
pub fn project(label: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("stand-in-{label}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("viso/src")).unwrap();
    fs::write(
        dir.join("viso/Cargo.toml"),
        "[package]\nname = \"viso\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [features]\ndefault = [\"desktop\", \"native-vulkan\", \"rasterizer\", \
         \"complex-line-break\"]\nvm = []\ndesktop = []\nnative-vulkan = []\nemulate = []\n\
         rasterizer = []\ncomplex-line-break = []\n\
         hot-reload = []\nheadless = []\ngles = []\ncpu = []\nkms = []\n",
    )
    .unwrap();
    fs::write(
        dir.join("viso/src/lib.rs"),
        "pub const HOT_RELOAD: bool = cfg!(feature = \"hot-reload\");\n\
         pub const HEADLESS: bool = cfg!(feature = \"headless\");\n\
         pub const CPU: bool = cfg!(feature = \"cpu\");\n\
         pub const KMS: bool = cfg!(feature = \"kms\");\n\
         pub const VM: bool = cfg!(feature = \"vm\");\n\
         pub const DESKTOP: bool = cfg!(feature = \"desktop\");\n\
         pub const EMULATE: bool = cfg!(feature = \"emulate\");\n\
         pub const RASTERIZER: bool = cfg!(feature = \"rasterizer\");\n\
         pub const COMPLEX_LINE_BREAK: bool = cfg!(feature = \"complex-line-break\");\n",
    )
    .unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
         [dependencies]\nviso = { path = \"viso\" }\n[workspace]\n",
    )
    .unwrap();
    fs::write(dir.join("src/main.rs"), MAIN).unwrap();
    fs::write(
        dir.join("Viso.toml"),
        "[package]\nname = \"probe\"\nversion = \"1.2.0\"\n",
    )
    .unwrap();
    dir
}

/// The value of `"key":"…"` in `line`.
pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let at = line.find(&format!("\"{key}\":\""))? + key.len() + 4;
    line[at..].split('"').next()
}

pub fn events<'a>(stdout: &'a str, kind: &str) -> Vec<&'a str> {
    let tag = format!("\"type\":\"{kind}\"");
    stdout.lines().filter(|line| line.contains(&tag)).collect()
}
