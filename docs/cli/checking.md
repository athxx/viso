# Checking and formatting

## `viso check`

```bash
viso check                 # the project's .vs sources and Viso.toml
viso check ios             # as built for iOS
viso check --cargo         # the Rust code too, with cargo check
viso check --watch         # again after every save, until Ctrl-C
```

`viso check` parses and type-checks every `.vs` file, resolves names, validates
properties, events and natives against their schemas, checks the reactive graph,
shaders and resource references, lowers each view the way the build will, and
validates `Viso.toml` — all without building. It is fast enough to run on every
save.

```text
checked `my_app`: 1 file, no problems
```

A problem is printed at its line, with what was expected and how to fix it:

```text
error[C0006]: `package.version` is required to package
 = note: add `version = "1.0.0"` under `[package]`
```

**For a target.** `viso check <target>` resolves the configuration as that target's
build would, and checks the sources against the input its devices have — touch on
iOS, Android and the web, keyboard, mouse and gamepad on the desktop. Without a
target it checks for this machine plus the mobile targets `Viso.toml` configures.

**Rust code** is not checked by default, to keep `check` fast. `--cargo` runs
`cargo check` with the target's Rust target, features and profile, and reports its
errors as diagnostics with their spans.

**Stricter findings.** Accessibility and localization findings are warnings unless
you make them errors:

```bash
viso check --a11y strict   # missing labels and roles are errors
viso check --i18n strict   # text shown without `tr`, and literal text, are errors
```

## `viso fmt`

```bash
viso fmt                   # format the .vs files of src/ and tests/, and Viso.toml
viso fmt src/app.vs        # one file
viso fmt --check           # write nothing; fail when a file is not formatted
```

There is one canonical layout and no options. Only files whose text changes are
written. `--check` exits `1` and names each file that would change, with the first
line that differs — use it in CI. A file that does not parse is reported and left
as it is. In `Viso.toml` only spacing is normalized; keys, values, order and
comments stay as you wrote them.

## `viso explain`

Every diagnostic has a stable code. `viso explain` says why it is raised and how to
fix it:

```bash
viso explain E3101
viso explain C0006
viso explain ENV_LOCK_HELD
viso explain --list        # every code with its title
```

```text
C0006 required configuration key is missing
config

A key the configuration needs is absent: `[package] name` everywhere, and the keys
a packaging step needs when it runs.

Fix:
  Add the key to `Viso.toml`.

Suggested actions:
  viso config validate
```

Code families: `E####` (the compiler), `C####` (configuration), and the CLI's own
named codes — `CLI_*`, `ENV_*`, `BUILD_*`, `RUN_*`, `TEST_*`, `DEVICE_*`,
`PACKAGE_*`, `EXPORT_*`, … Each CLI code maps to one exit code (see
[Automation and CI](automation.md)).

## `viso schema`

The typed interface of anything you can use in a view:

```bash
viso schema Button                 # a widget's properties and events
viso schema Button.text            # one member
viso schema viso::widgets::Button  # by full path
viso schema text::upper            # a native function
viso schema --search text          # every symbol and member whose path has `text`
viso schema --list widgets         # widgets, components, natives or types
```

Your own components appear too, with the file and line that declare them.

## `viso dump`

The compiler's intermediate forms, for debugging the compiler or understanding what
a view becomes:

```bash
viso dump ui-ir src/app.vs           # the view a file mounts, node by node
viso dump reactive-ir Counter        # what each state of Counter invalidates
viso dump behavior-ir --symbol App.increment src/app.vs
viso dump module-graph               # every module and its imports
viso dump hir src/app.vs --out hir.txt
```

Kinds: `ast`, `hir`, `ui-ir`, `reactive-ir`, `behavior-ir`, `shader-ir`,
`system-ir`, `module-graph`.

## In the editor

`viso lsp --stdio` is the language server editors start for `.vs` files: the same
diagnostics as `viso check` as you type, hover, signature help, go to definition,
find references, rename, code actions for the fixes diagnostics suggest, and
formatting with the `viso fmt` layout. It runs `viso-lsp`, which [installs](install.md#the-language-server)
alongside the CLI.
