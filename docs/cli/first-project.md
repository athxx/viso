# Your first project

## Create it

```bash
viso new my_app
cd my_app
viso run
```

`viso new` writes a small app that runs as it is, makes the directory a Git
repository and tells you what to run next. A window opens with a counter; edit
`src/app.vs`, save, and the window changes without restarting.

Pick what the project starts as with `--template`:

| Template | What you get |
|---|---|
| `app` (default) | A cross-platform app: one component, a UI test |
| `game` | A game: a fixed-tick system, a game tape test and a UI test |
| `web` | An app whose default web target is the DOM backend |
| `library` | Components for other apps to use, no executable |

```bash
viso new my_game --template game
viso new widgets --template library --no-git
viso new . --template app          # in the current directory, named after it
```

Other options:

```text
--name <NAME>        the package name; the directory's name otherwise
--edition <YEAR>     the Rust edition, 2021 or 2024 (default 2024)
--no-git             do not run `git init`
--force-empty-dir    create it in a directory that already has files; none is
                     replaced, and a file the project would write is an error
--framework <PATH>   depend on a Viso checkout instead of the published crate
```

A name must be a valid Cargo package name: ASCII letters, digits, `-` and `_`,
starting with a letter or `_`, at most 64 characters, and not a Rust keyword or
reserved name. When the directory's name is not one, `viso new` suggests a
`--name`.

## What is in it

```text
my_app/
├── Viso.toml              the project: name, language version, packaging
├── Cargo.toml             the Rust package; depends on `viso`
├── src/
│   ├── main.rs            fn main() { viso::run::<App>(); }
│   ├── app.rs             the Application: loads the fonts, mounts app.vs
│   └── app.vs             the view, in the Viso language
├── i18n/
│   └── en.toml            the app's text, by key
├── assets/
│   └── fonts/             the font every platform renders with
└── tests/
    └── ui/
        └── clicks.tape    a UI scenario: click, then expect the new text
```

`Viso.toml` marks the project root. Commands find it by searching upward from the
current directory, so you can run `viso` from any directory inside the project, or
point at one elsewhere with `--project`:

```bash
viso --project ~/src/my_app check
```

A freshly created `Viso.toml` is short:

```toml
[package]
name = "my_app"
language = "1.0"

[i18n]
source = "en"
```

Everything else has a default. `viso config show` lists every key, its value and
where it came from — see [Configuration](configuration.md).

## The first commands

```bash
viso check            # check the .vs sources and Viso.toml, without building
viso fmt              # format the .vs sources
viso test             # run every test: unit, UI, game
viso build            # build for this machine into dist/
```

Each prints a one-line summary and exits `0` on success. Add `--json` to any of
them for machine-readable events (see [Automation and CI](automation.md)).

## Next

- [The dev loop](dev-loop.md): hot reload and inspecting the running app.
- [Mobile](mobile.md): run the same project on an iOS simulator or Android emulator.
- [Building and packaging](packaging.md): give it a version and an icon, and ship it.
