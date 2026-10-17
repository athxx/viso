# Install

## The CLI

`viso` needs a Rust toolchain: install one with [rustup](https://rustup.rs). Then
build the CLI from a checkout of the Viso repository:

```bash
git clone <viso repository> viso
cargo install --path viso/tools/cli
```

This puts `viso` in `~/.cargo/bin`. Check it:

```bash
viso --version
viso doctor
```

> Until the `viso` crate is published, a new project depends on the framework in
> your checkout. Pass it to `viso new` with `--framework`:
>
> ```bash
> viso new my_app --framework path/to/viso
> ```

## The language server

Editors get diagnostics, completion and formatting for `.vs` files from
`viso-lsp`. Install it from the same checkout, at the same version as the CLI:

```bash
cargo install --path viso/crates/lsp
```

`viso lsp` finds it next to `viso` or on `PATH`; `viso doctor` warns when it is
missing or a different version. Point your editor's LSP client at:

```bash
viso lsp --stdio
```

## Shell completion

```bash
viso completion zsh  > ~/.zfunc/_viso          # zsh, with ~/.zfunc on fpath
viso completion bash > ~/.local/share/bash-completion/completions/viso
viso completion fish > ~/.config/fish/completions/viso.fish
viso completion powershell >> $PROFILE
```

## What each target needs

The desktop needs only Rust and the system's C linker (Xcode Command Line Tools on
macOS, Visual Studio Build Tools on Windows, `cc` on Linux) and a GPU driver:
Metal on macOS, Direct3D 12 on Windows, Vulkan on Linux.

`viso doctor <target>` tells you what a target still needs, and `--fix` makes the
small, reversible fixes it can — adding a Rust target with `rustup`:

```bash
viso doctor android --fix
viso doctor web-gpu
```

| Target | What it needs | How to get it |
|---|---|---|
| `ios` | macOS, Xcode with its license accepted, an iOS Simulator runtime | `viso ios use <runtime> --download` |
| `android` | The Android SDK, NDK, emulator and a system image | `viso android use 36` — Viso downloads its tested set |
| `web-gpu` | The `wasm32-unknown-unknown` Rust target, `wasm-bindgen` at the version `Cargo.lock` pins, and `wasm-opt` for optimized builds | `viso doctor web-gpu --fix`, `cargo install wasm-bindgen-cli --version <v>`, binaryen's `wasm-opt` |
| `headless` | Nothing beyond the desktop | — |

Viso keeps what it downloads, and the emulator and simulator profiles you create,
under `~/.viso/` (set `VISO_HOME` to move it). Nothing there belongs in a project.
See [Mobile](mobile.md) for the Android and iOS environments.
