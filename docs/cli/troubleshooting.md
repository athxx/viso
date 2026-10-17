# Troubleshooting

## Start with `viso doctor`

```bash
viso doctor                # the host's tools, the project, and a word on each target
viso doctor android        # what an Android build needs
viso doctor web-gpu --fix  # and add the missing Rust target
```

```text
Viso Doctor

[ok] Rust toolchain: rustc 1.99.1 (aarch64-apple-darwin)
[ok] Cargo: cargo 1.99.1
[ok] Viso project: `my_app` at /Users/me/my_app
[ok] Host compiler and linker: Apple clang version 21.0.0
[ok] Host GPU backend: Metal on Apple M4 (metal4)
[ok] WebAssembly target: `wasm32-unknown-unknown` installed
[ok] wasm-bindgen: wasm-bindgen 0.2.128
[warn] wasm-opt: `wasm-opt` is not installed (Binaryen): a release web bundle is left unoptimized, and a shipping one does not build
[ok] Language server: viso-lsp 0.0.0
[ok] Android SDK: SDK at ~/Library/Android/sdk, from `ANDROID_HOME`
[ok] iOS Simulator runtime: iOS 27.0 (24A434), the newest; `viso ios use` chooses another
```

Each check is `ok`, `warn`, `missing` or `error`, with what to do about it.
`viso doctor` changes nothing unless you pass `--fix`, and even then only adds a Rust
target with `rustup` — it never installs an SDK behind your back. `viso android
doctor` and `viso ios doctor` go deeper into their environments.

## Read the code

Every failure has a code. `viso explain <CODE>` says why it happens and how to fix
it:

```bash
viso explain ENV_ANDROID_SDK
viso explain E3101
```

A diagnostic's `help:` line is usually the command to run next.

## Common problems

**`C0001` — no `Viso.toml`** — run the command inside a project, or point at one
with `--project <path>`. `viso new <name>` creates one.

```text
error[C0001]: no `Viso.toml` in `/Users/me/Downloads` or any parent
 = note: run `viso new <name>` to create a project, or pass `--project <path>`
```

**`C0011` — the target is not available.** `web-dom` and `web-hybrid` are not
available yet; use `web-gpu`. A new `--template web` project names `web-dom` as its
`[web] default_target`, so `viso serve` alone fails there: run `viso serve web-gpu`,
or set `default_target = "web-gpu"` in `Viso.toml`.

**`ENV_LOCK_HELD`** — another `viso` is using what this command needs: a `viso run`
in the same project, or a build of the same target. The message names the process.
Stop it, or wait. A lock left by a crashed process is taken over after 90 seconds.

**`ENV_RUST_TARGET`** — the Rust target a build needs is not installed:
`viso doctor <target> --fix` adds it.

**`ENV_ANDROID_SDK` / `ENV_ANDROID_LICENSE`** — run `viso android use 36`, and
accept the licenses it shows (in scripts, `--accept-licenses`).

**`ENV_XCODE` / `ENV_XCODE_LICENSE`** — install Xcode, select it with
`xcode-select`, and accept its license with `sudo xcodebuild -license`.

**`DEVICE_MISSING` / `DEVICE_AMBIGUOUS` / `DEVICE_UNKNOWN`** — create a profile with
`viso android emulator create <id>` or `viso ios simulator create <id>`, or pick one
with `--device`. See [Mobile](mobile.md).

**`ENV_TOOL` on a shipping web build** — install `wasm-opt` (Binaryen).

**`wasm-bindgen` version mismatch** — install the version your `Cargo.lock` pins:
`cargo install wasm-bindgen-cli --version <version>`.

**An edit does not show up in the running app.** Look at the terminal running
`viso run`: an edit that does not compile is rejected and the app keeps its last
good version. With `--no-hot-reload` edits are compiled and reported but never sent.

**`PACKAGE_UNSIGNED`** — the package was signed only as far as the platform allows
without an identity (ad hoc, debug keystore, or not at all). Configure one — see
[Signing](packaging.md#signing) — or pass `--signing required` to make it an error.

**`EXPORT_CONFLICT`** — the export directory holds files the export did not write or
that were edited since. Move your edits elsewhere or pass `--force`.

**A snapshot test fails on another machine.** Snapshots are rendered by the software
rasterizer and should match everywhere; if one differs only slightly, raise its
`tolerance` in `tests/snapshots/<name>.<target>.json`. Look at the `.diff.png` in
`target/viso/snapshots/` first.

## See what it runs

```bash
viso build -v
```

`--verbose` prints the resolved configuration, every external program `viso` runs
with its arguments, and how long each stage took. Secrets are shown as `<hidden>`.

## Start over

```bash
viso clean
```

removes `target/viso/` — builds, caches, dev state, traces and locks. It never
touches `dist/` or `~/.viso/`, and refuses while another `viso` holds a lock there.
Cargo's own build output under `target/` stays; remove it with `cargo clean` if you
need to.

## Report a problem

Include the output of `viso doctor --json`, the command you ran with `--verbose`,
and the exact diagnostic, code included.
