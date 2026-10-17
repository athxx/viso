# Configuration

## `Viso.toml`

`Viso.toml` sits at the project root and describes the project. Only
`[package] name` is required; everything else has a default.

```toml
[package]
name = "notes"
language = "1.0"                # the Viso language version the sources are written in
display_name = "Notes"
bundle_id = "com.example.notes"
version = "1.0.0"
build = 42

[package.icons]
source = "assets/icon.png"

[package.permissions]
camera = "Scans receipts into notes"

[i18n]
source = "en"                   # the locale the text is written in

[profile.release]
source_maps = true

[profile.shipping.web]
optimize = "size"

[web]
default_target = "web-gpu"

[web.serve]
port = 9000

[game]
tick_rate = 60

[export.html]
out_dir = "dist-html"
```

The packaging keys are described in [Building and packaging](packaging.md), the web
keys in [Web](web.md), the export keys in [Exporting](export.md).

Values are checked when any command reads the project, each problem at its line: an
unknown key, a value of the wrong type or out of range is a `C####` diagnostic, and
a misspelled key suggests the one you meant.

## Where a value comes from

Each key is resolved on its own, from the first layer that sets it:

```text
command-line flags                       flag
VISO_* environment variables             env
[profile.<name>] in Viso.toml            profile
the rest of Viso.toml                    manifest
Viso's defaults                          default
```

The environment variables are `VISO_PROFILE`, `VISO_OPT_LEVEL`, `VISO_SOURCE_MAPS`
and `VISO_STRIP`. A value that does not parse is an error (`C0010`), never skipped.
The target is not configuration: it is always the command's first argument.

## `viso config`

```bash
viso config show                           # every key, its value and where it came from
viso config show --profile shipping        # as a shipping build resolves them
viso config get package.version
viso config get opt_level --profile release
viso config path                           # the Viso.toml in use
viso config validate                       # every problem, VISO_* variables included
```

```text
profile                           dev          default
opt_level                         0            default
source_maps                       true         default
strip                             false        default
dev_runtime                       present      default
package.name                      my_app       Viso.toml:2:8
package.bundle_id                 (unset)      default
package.version                   (unset)      default
```

A value from `Viso.toml` names its line, so you can jump to it. `viso config` only
reads: there is no `set`. Edit `Viso.toml` and run `viso config validate` (or `viso
fmt`, which also normalizes its spacing).

## Per machine, not per project

Some state belongs to your machine and never goes in `Viso.toml`:

```text
~/.viso/                 SDKs Viso installed, emulator and simulator profiles, defaults
.viso/local.toml         this project's default simulator or emulator on this machine
```

Keep `.viso/` out of version control. `VISO_HOME` moves `~/.viso`.

## Build output

```text
target/viso/             builds, caches, dev state, traces, test runs — rebuildable
dist/                    what build and package write — yours
```

`viso clean` removes `target/viso/` and never touches `dist/`. `--target-dir <PATH>`
moves `target/` (and with it `target/viso/`) elsewhere; `dist/` stays at the project
root.
