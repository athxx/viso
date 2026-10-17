# The `viso` command-line tool

`viso` creates, checks, runs, tests, builds, packages and exports Viso apps. One
tool covers the whole loop — from `viso new` to a signed package — on the desktop,
on iOS and Android simulators and emulators, and in the browser.

```bash
viso new my_app      # create an app
cd my_app
viso run             # build it, run it, and hot reload every edit you save
viso test            # run its tests
viso package         # build it as its platform ships it, signed
```

## Guides

| Page | What it covers |
|---|---|
| [Install](install.md) | The CLI, the language server, and what each target needs |
| [Your first project](first-project.md) | `viso new`, the templates, the files a project has |
| [The dev loop](dev-loop.md) | `viso run`, hot reload, `inspect`, `profile`, `game` |
| [Checking and formatting](checking.md) | `check`, `fmt`, `explain`, `schema`, `dump`, the editor |
| [Testing](testing.md) | `viso test`, UI scenarios, game tapes, snapshots |
| [Mobile](mobile.md) | Android and iOS environments, emulators and simulators |
| [Web](web.md) | `web-gpu`, `viso serve`, the web targets |
| [Building and packaging](packaging.md) | `build`, profiles, `package`, signing, `artifact.json` |
| [Exporting to web projects](export.md) | `export html`, `export solid` |
| [Configuration](configuration.md) | `Viso.toml`, precedence, `viso config` |
| [Automation and CI](automation.md) | `--json` events, exit codes, running in CI |
| [Troubleshooting](troubleshooting.md) | `viso doctor`, common errors, `viso clean` |

## Commands at a glance

```text
PROJECT                 new, doctor, config
MOBILE DEV ENVIRONMENT  android, ios
DEVELOP                 run, build, serve
LANGUAGE                fmt, check, schema, explain, dump, lsp
TEST / DEBUG            test, snapshot, inspect, profile, game
DELIVERY                package, export
MAINTENANCE             clean, completion
```

`viso --help` lists them and `viso <command> --help` shows a command's arguments,
options and examples.

## Three ideas that shape the CLI

**A target is where the app runs.** The desktop you are on is the default and has
no name: `viso run` runs on this machine. Every other target is named as the first
argument — `ios`, `android`, `web-gpu`, `web-dom`, `web-hybrid` and, for tests and
tooling, `headless`:

```bash
viso run ios
viso build web-gpu
viso check android
```

**A profile is how it is built**, separate from the target: `dev`, `release` or
`shipping`, chosen with `--profile`. There is no `web-release` or `android-debug`.

```bash
viso build web-gpu --profile shipping
```

**Three kinds of output, three commands.** `viso build` writes what runs, into
`dist/`. `viso package` writes what ships — signed, with metadata — into
`dist/<platform>/`. `viso export` writes source code that another web project owns.

## Global options

Every command takes these where they apply:

```text
--project <PATH>     use the project at PATH instead of the one found upward
--json               write JSON events to stdout, one per line
-q, --quiet          print only errors, requested paths and the summary
-v, --verbose        also print the resolved configuration, the tools run, stage times
--color <WHEN>       auto | always | never
--offline            use no network
--locked             require Cargo.lock to be up to date
--jobs <N>           run at most N jobs at once
--profile <NAME>     dev | release | shipping
--target-dir <PATH>  put build output under PATH instead of target/
```

`--project` works before or after the command: `viso --project apps/notes check`
and `viso check --project apps/notes` are the same.
