//! The help text the grammar's doc comments do not carry: the top-level
//! command list in its groups, and each command's examples and notes.
//!
//! Every page reads in one order — purpose, usage, examples, arguments,
//! options, notes — and every command, at every level, has its examples. The
//! tables are keyed by command path and checked against the grammar, so a
//! command cannot be left out of its group or without examples, and an
//! example cannot drift from what parses.

use std::fmt::Write as _;

use clap::{Arg, ArgAction, Command};

/// The top-level groups in order, each with its commands in order.
const GROUPS: &[(&str, &[&str])] = &[
    ("PROJECT", &["new", "doctor", "config"]),
    ("MOBILE DEV ENVIRONMENT", &["android", "ios"]),
    ("DEVELOP", &["run", "build", "serve"]),
    (
        "LANGUAGE",
        &["fmt", "check", "schema", "explain", "dump", "lsp"],
    ),
    (
        "TEST / DEBUG",
        &["test", "snapshot", "inspect", "profile", "game"],
    ),
    ("DELIVERY", &["package", "export"]),
    ("MAINTENANCE", &["clean", "completion"]),
];

/// Each command's examples: the command line and what it does.
const EXAMPLES: &[(&str, &[(&str, &str)])] = &[
    (
        "",
        &[
            ("viso new my_app", "create an app in `my_app/`"),
            ("viso run", "build and run the app here, hot reloading it"),
            ("viso check --json", "check the sources, as JSON events"),
        ],
    ),
    (
        "android",
        &[
            (
                "viso android use 36",
                "install API 36's tools and make it the default",
            ),
            (
                "viso android emulator create pixel",
                "create a phone profile",
            ),
        ],
    ),
    (
        "android list",
        &[
            (
                "viso android list",
                "the supported API levels and the default",
            ),
            (
                "viso android list --json",
                "with each level's components, as JSON events",
            ),
        ],
    ),
    (
        "android use",
        &[
            (
                "viso android use 36",
                "install API 36's tested set, asking for its licenses",
            ),
            (
                "viso android use 35 --accept-licenses",
                "accept the licenses without being asked",
            ),
            (
                "viso android use 36 --offline",
                "switch only if nothing needs downloading",
            ),
        ],
    ),
    (
        "android doctor",
        &[
            (
                "viso android doctor",
                "check the SDK, emulator and acceleration",
            ),
            (
                "viso android doctor --json",
                "one `doctor_check` event per check",
            ),
        ],
    ),
    (
        "android adb",
        &[
            ("viso android adb devices", "the devices adb sees"),
            (
                "viso android adb --device pixel shell getprop",
                "run a command on a profile's emulator",
            ),
            (
                "viso android adb -- logcat -d",
                "everything after `--` goes to adb as it is",
            ),
        ],
    ),
    (
        "android emulator",
        &[
            ("viso android emulator list", "list the profiles"),
            (
                "viso android emulator start pixel --no-window",
                "boot one without its window",
            ),
        ],
    ),
    (
        "android emulator list",
        &[
            (
                "viso android emulator list",
                "each profile and whether it is booted",
            ),
            ("viso android emulator list --json", "as JSON events"),
        ],
    ),
    (
        "android emulator create",
        &[
            (
                "viso android emulator create pixel",
                "a phone on the newest installed system",
            ),
            (
                "viso android emulator create tab --preset tablet --api 35",
                "a tablet on API 35",
            ),
        ],
    ),
    (
        "android emulator start",
        &[
            ("viso android emulator start pixel", "boot `pixel`"),
            (
                "viso android emulator start pixel --no-window",
                "boot it without its window",
            ),
        ],
    ),
    (
        "android emulator stop",
        &[
            ("viso android emulator stop pixel", "shut `pixel` down"),
            ("viso android emulator stop pixel --json", "as JSON events"),
        ],
    ),
    (
        "android emulator delete",
        &[
            (
                "viso android emulator delete pixel",
                "delete `pixel` and its device",
            ),
            (
                "viso android emulator delete pixel --json",
                "as JSON events",
            ),
        ],
    ),
    (
        "build",
        &[
            ("viso build", "build the app for this machine into `dist/`"),
            (
                "viso build web-gpu --profile shipping",
                "the smallest web bundle, precompressed",
            ),
            (
                "viso build ios --release --json",
                "a release `.app`, as JSON events",
            ),
        ],
    ),
    (
        "package",
        &[
            (
                "viso package",
                "a signed `.app` and `.dmg` on a Mac, an archive elsewhere",
            ),
            (
                "viso package android --signing required",
                "an APK and app bundle, only with the release keystore",
            ),
            (
                "viso package ios --dry-run",
                "what the `.ipa` would be, building nothing",
            ),
        ],
    ),
    (
        "export",
        &[
            (
                "viso export html",
                "the view as HTML and CSS in `dist-html/`",
            ),
            (
                "viso export solid",
                "the view as a SolidJS component in `web-solid/`",
            ),
        ],
    ),
    (
        "export html",
        &[
            (
                "viso export html",
                "the view as HTML and CSS in `dist-html/`",
            ),
            (
                "viso export html ui::Card --static --out card",
                "one component alone, with no JavaScript, failing on what needs it",
            ),
            (
                "viso export html --out site --force --json",
                "over edits in `site/`, as JSON events",
            ),
        ],
    ),
    (
        "export solid",
        &[
            (
                "viso export solid",
                "the view as a SolidJS component in a Vite project in `web-solid/`",
            ),
            (
                "viso export solid ui::Card --out card",
                "one component alone, its inputs as props",
            ),
            (
                "viso export solid --force --json",
                "over edits in `web-solid/`, as JSON events",
            ),
        ],
    ),
    (
        "check",
        &[
            ("viso check", "check the project here"),
            (
                "viso check ios --cargo",
                "for the iOS Simulator, the Rust code too",
            ),
            (
                "viso check --watch --json",
                "again after every change, as JSON events",
            ),
        ],
    ),
    (
        "new",
        &[
            ("viso new my_app", "an app in `my_app/`, a Git repository"),
            (
                "viso new . --template game",
                "a game in the current directory, named after it",
            ),
            (
                "viso new widgets --template library --no-git",
                "components for other apps, without Git",
            ),
        ],
    ),
    (
        "doctor",
        &[
            (
                "viso doctor",
                "the host's tools, and a word on each target's",
            ),
            ("viso doctor web-gpu", "what a web build needs"),
            (
                "viso doctor android --fix",
                "add the Rust Android target if it is missing",
            ),
        ],
    ),
    (
        "clean",
        &[
            ("viso clean", "remove the project's `target/viso/`"),
            (
                "viso clean --target-dir /tmp/out",
                "the one under another `target/`",
            ),
            ("viso clean --json", "as JSON events"),
        ],
    ),
    (
        "completion",
        &[
            ("viso completion zsh", "the zsh script"),
            ("viso completion bash", "the bash script"),
            ("viso completion fish", "the fish script"),
        ],
    ),
    (
        "fmt",
        &[
            ("viso fmt", "format the project's sources"),
            ("viso fmt src/app.vs", "one file"),
            (
                "viso fmt --check",
                "fail on an unformatted file, writing nothing",
            ),
        ],
    ),
    (
        "config",
        &[
            ("viso config show", "every key, its value and its origin"),
            ("viso config get profile.release.opt_level", "one key"),
            ("viso config validate", "every problem of the configuration"),
        ],
    ),
    (
        "config show",
        &[
            ("viso config show", "as a dev build sees it"),
            (
                "viso config show --profile release",
                "as a release build sees it",
            ),
            ("viso config show --json", "as JSON events"),
        ],
    ),
    (
        "config get",
        &[
            ("viso config get package.name", "the package name"),
            (
                "viso config get opt_level --profile shipping",
                "a shipping build's optimization level",
            ),
        ],
    ),
    (
        "config path",
        &[
            ("viso config path", "the `Viso.toml` in use here"),
            (
                "viso config path --project apps/demo",
                "the one of another project",
            ),
        ],
    ),
    (
        "config validate",
        &[
            ("viso config validate", "report every problem"),
            ("viso config validate --json", "as JSON events"),
        ],
    ),
    (
        "game",
        &[
            (
                "viso game peek movement Player.score",
                "read a state of the running game",
            ),
            (
                "viso game record movement --output runs/jump.tape",
                "record ten seconds of play",
            ),
        ],
    ),
    (
        "game peek",
        &[
            (
                "viso game peek movement Player.score",
                "a state, by the game's file stem",
            ),
            (
                "viso game peek src/levels/one Player.lives --json",
                "by its path, as JSON events",
            ),
        ],
    ),
    (
        "game record",
        &[
            (
                "viso game record movement --output runs/jump.tape",
                "ten seconds of play",
            ),
            (
                "viso game record movement --output runs/long.tape --seconds 30",
                "thirty seconds",
            ),
            (
                "viso game record movement --output runs/short.tape --ticks 600",
                "600 fixed ticks",
            ),
        ],
    ),
    (
        "inspect",
        &[
            (
                "viso inspect tree",
                "every node the running app shows, with its source",
            ),
            (
                "viso inspect node '#save'",
                "what the app holds for the node named `save`",
            ),
            (
                "viso inspect frame --target headless --json",
                "a headless run's last frame, as JSON events",
            ),
        ],
    ),
    (
        "inspect tree",
        &[
            (
                "viso inspect tree",
                "the running app's nodes, indented by depth",
            ),
            (
                "viso inspect tree --target headless --scenario login",
                "a headless run's nodes after the `login` scenario",
            ),
        ],
    ),
    (
        "inspect node",
        &[
            (
                "viso inspect node '#save'",
                "layout, style, bindings and paint of the node named `save`",
            ),
            (
                "viso inspect node 12:1 --json",
                "a node by its ID, as JSON events",
            ),
            (
                "viso inspect node 'Toolbar>Button' --target headless",
                "the buttons right under a toolbar, in a headless run",
            ),
        ],
    ),
    (
        "inspect frame",
        &[
            (
                "viso inspect frame",
                "the running app's last frame: counters, batches, resources",
            ),
            (
                "viso inspect frame --window 1 --json",
                "the second window's frame, as JSON events",
            ),
        ],
    ),
    (
        "inspect query",
        &[
            (
                "viso inspect query '#save'",
                "a node by its name, in the `viso run` session",
            ),
            (
                "viso inspect query '[role=check_box][checked]' --json",
                "the checked check boxes, as JSON events",
            ),
            (
                "viso inspect query 'Toolbar>Text' --target headless --scenario login",
                "the texts right under a toolbar, after the `login` scenario",
            ),
        ],
    ),
    (
        "profile",
        &[
            (
                "viso profile",
                "five seconds of the `viso run` app's frames",
            ),
            (
                "viso profile ios --device iphone --frames 600 --chrome trace.json",
                "600 frames on the simulator, also as a Chrome trace",
            ),
            (
                "viso profile headless --scenario scroll --json",
                "every frame of the `scroll` scenario, headless, as JSON events",
            ),
        ],
    ),
    (
        "ios",
        &[
            ("viso ios list", "the Simulator runtimes and the default"),
            ("viso ios simulator create iphone", "create a phone profile"),
        ],
    ),
    (
        "ios list",
        &[
            (
                "viso ios list",
                "the runtimes installed and the one Xcode can install",
            ),
            (
                "viso ios list --json",
                "with each runtime's build and models, as JSON events",
            ),
        ],
    ),
    (
        "ios use",
        &[
            ("viso ios use 26.3", "make an installed runtime the default"),
            (
                "viso ios use 27 --download",
                "have Xcode download it first when it is not installed",
            ),
            (
                "viso ios use 27 --offline",
                "switch only if nothing needs downloading",
            ),
        ],
    ),
    (
        "ios doctor",
        &[
            (
                "viso ios doctor",
                "check Xcode, its license and the runtime",
            ),
            (
                "viso ios doctor --json",
                "one `doctor_check` event per check",
            ),
        ],
    ),
    (
        "ios simulator",
        &[
            ("viso ios simulator list", "list the profiles"),
            ("viso ios simulator start iphone", "boot one"),
        ],
    ),
    (
        "ios simulator list",
        &[
            (
                "viso ios simulator list",
                "each profile and whether it is booted",
            ),
            ("viso ios simulator list --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator create",
        &[
            (
                "viso ios simulator create iphone",
                "a phone on the newest installed system",
            ),
            ("viso ios simulator create ipad --preset tablet", "a tablet"),
        ],
    ),
    (
        "ios simulator start",
        &[
            ("viso ios simulator start iphone", "boot `iphone`"),
            ("viso ios simulator start iphone --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator stop",
        &[
            ("viso ios simulator stop iphone", "shut `iphone` down"),
            ("viso ios simulator stop iphone --json", "as JSON events"),
        ],
    ),
    (
        "ios simulator delete",
        &[
            (
                "viso ios simulator delete iphone",
                "delete `iphone` and its device",
            ),
            ("viso ios simulator delete iphone --json", "as JSON events"),
        ],
    ),
    (
        "run",
        &[
            ("viso run", "on this machine"),
            (
                "viso run ios --device iphone",
                "on the simulator profile `iphone`",
            ),
            (
                "viso run web-gpu --browser none",
                "serve it, opening no browser",
            ),
        ],
    ),
    (
        "dump",
        &[
            (
                "viso dump ui-ir src/app.vs",
                "the view a file mounts, node by node",
            ),
            (
                "viso dump reactive-ir Counter --json",
                "what each state of `Counter` invalidates, as JSON events",
            ),
            ("viso dump module-graph", "every module and its imports"),
        ],
    ),
    (
        "lsp",
        &[
            ("viso lsp --stdio", "what an editor starts for `.vs` files"),
            ("viso lsp", "the same: stdio is the only transport"),
        ],
    ),
    (
        "explain",
        &[
            (
                "viso explain E3101",
                "what a compiler code means and its fix",
            ),
            (
                "viso explain ENV_LOCK_HELD --json",
                "a CLI code, as JSON events",
            ),
            ("viso explain --list", "every code with its title"),
        ],
    ),
    (
        "schema",
        &[
            ("viso schema Button", "a widget's properties and events"),
            (
                "viso schema Button.text --json",
                "one member, as JSON events",
            ),
            (
                "viso schema --search text",
                "every symbol whose path has `text`",
            ),
        ],
    ),
    (
        "serve",
        &[
            ("viso serve", "the target `[web] default_target` names"),
            ("viso serve web-gpu --open", "and open it in the browser"),
            (
                "viso serve web-gpu --lan --port 9000",
                "to the other devices of the network",
            ),
        ],
    ),
    (
        "snapshot",
        &[
            ("viso snapshot compare", "hold every snapshot to its golden"),
            (
                "viso snapshot capture home",
                "write the snapshots of `tests/ui/home.tape` as goldens",
            ),
            (
                "viso snapshot update --filter settings --json",
                "rewrite the departed goldens named `*settings*`, as JSON events",
            ),
        ],
    ),
    (
        "snapshot capture",
        &[
            (
                "viso snapshot capture",
                "write every snapshot the scenarios take as its golden",
            ),
            (
                "viso snapshot capture home",
                "the snapshots of `tests/ui/home.tape`, or its end",
            ),
            (
                "viso snapshot capture home --output out/home",
                "into `out/home/` instead of `tests/snapshots/`",
            ),
        ],
    ),
    (
        "snapshot compare",
        &[
            ("viso snapshot compare", "hold every snapshot to its golden"),
            ("viso snapshot compare home", "only those `home.tape` takes"),
            (
                "viso snapshot compare --json",
                "one `test` event a golden, as JSON events",
            ),
        ],
    ),
    (
        "snapshot update",
        &[
            (
                "viso snapshot update",
                "rewrite the goldens missing or departed from",
            ),
            ("viso snapshot update home", "only those `home.tape` takes"),
            (
                "viso snapshot update --filter settings",
                "only the snapshots named `*settings*`",
            ),
        ],
    ),
    (
        "test",
        &[
            ("viso test", "every domain"),
            ("viso test ui", "every scenario under `tests/ui/`, headless"),
            (
                "viso test ui --target android --device pixel",
                "the UI scenarios on an emulator profile",
            ),
        ],
    ),
];

/// The notes that close a command's page: what holds for some targets, and
/// where to go next.
const NOTES: &[(&str, &str)] = &[
    (
        "",
        "Run `viso <command> --help` for a command's arguments and options.",
    ),
    (
        "android",
        "Profiles are kept in `~/.viso/` (`$VISO_HOME`); `viso run android --device <id>`\n\
         runs on one.",
    ),
    (
        "android use",
        "The tested set is the platform, a Google APIs system image for this host, and the\n\
         platform-tools, build-tools, NDK and emulator, each archive verified before it is\n\
         unpacked into `~/.viso/android/sdk`. `ANDROID_HOME` still wins when it is set.",
    ),
    (
        "build",
        "The sources are checked first. The output replaces `dist/<platform>/` whole, with\n\
         its `artifact.json`; `ios` builds for the Simulator and `android` for the Emulator.\n\
         `[profile.<name>.web]` in `Viso.toml` shapes a web bundle.",
    ),
    (
        "package",
        "The default profile is `shipping`. `[package]` in `Viso.toml` gives the metadata,\n\
         `[package.<platform>]` the identity; a keystore's passwords come from\n\
         `VISO_ANDROID_KEYSTORE_PASSWORD` and `VISO_ANDROID_KEY_PASSWORD`, never a flag.\n\
         The output replaces `dist/<platform>/` whole, with its `artifact.json`.",
    ),
    (
        "export html",
        "The component named, else the one `src/app.vs` exports, is lowered as it checks: layout to flexbox and grid,\n\
         widgets to their HTML elements and roles, the theme to CSS variables. What HTML\n\
         cannot show (a shader, a game) is an error naming its node, never dropped.\n\
         The directory is replaced whole; one holding files the export did not write, or\n\
         edited since, is refused unless `--force`.",
    ),
    (
        "export solid",
        "The component is lowered as for `export html`, then written as JSX: its states as\n\
         signals, computeds as memos, inputs as typed props, events as `on…` callbacks, each\n\
         handler one transaction; `if`/`match`/`for` as `Show`/`Switch`/keyed `Each`. The\n\
         project needs `solid-js`, `vite` and `vite-plugin-solid`; Viso never installs them.",
    ),
    (
        "check",
        "A named target is checked as its build would be: its configuration, and the input\n\
         its devices have. `--a11y strict` and `--i18n strict` make those findings errors.",
    ),
    (
        "clean",
        "A lock a running `viso` holds stops it before anything is removed.",
    ),
    (
        "completion",
        "Install it from the shell's startup file:\n  \
         bash        source <(viso completion bash)\n  \
         zsh         source <(viso completion zsh)\n  \
         fish        viso completion fish | source\n  \
         powershell  viso completion powershell | Out-String | Invoke-Expression",
    ),
    (
        "config",
        "`Viso.toml` is the source of truth: `viso config` reads it and never writes it.\n\
         A value comes from the first of: a flag, a `VISO_*` variable, the profile's\n\
         table, the manifest, the default.",
    ),
    (
        "new",
        "The templates are built in, so it needs no network. A directory with files is\n\
         refused unless `--force-empty-dir`, which still replaces none of them.\n\
         `--framework <path>` depends on a checkout of the framework instead of a release.",
    ),
    (
        "doctor",
        "It changes nothing but what `--fix` says it runs, and installs no SDK. A check\n\
         that is `missing` or `error` exits 3.",
    ),
    (
        "fmt",
        "A file that does not parse is reported and left as it is. Rust is `cargo fmt`'s.",
    ),
    (
        "game",
        "Both ask the app of this project's running `viso run` session: start one first.",
    ),
    (
        "inspect",
        "It asks the app of this project's running `viso run` session, or a headless run\n\
         of it with `--target headless`.",
    ),
    (
        "inspect node",
        "Takes a selector as `viso inspect query` does, or a node ID `<index>:<generation>`.\n\
         Each node shows its resolved layout, style and content, its box in the window and\n\
         its own, the offsets of its ancestors, the classes it is dirty for, its\n\
         interaction states, the handlers its events reach, the states bound to it with\n\
         their value and declaration, and its span of the paint stream. No match is not an\n\
         error.",
    ),
    (
        "inspect frame",
        "Counters are those of the window's last frame; batches are its draw calls in\n\
         submission order; resources are the GPU memory, caches and assets the window\n\
         holds, with their budget where one bounds them.",
    ),
    (
        "inspect query",
        "Attributes: role, label, hint, value, checked, selected, expanded, focused,\n\
         focusable, shown, kind. `[checked]` alone holds of a checked node, `[label]` of\n\
         one with a label; a value is a name, a number or a quoted string. A type matches\n\
         a widget of that type or the root node of that component (`module::Component`\n\
         by its module). No match is not an error. The selector's syntax errors exit 2; no\n\
         session exits 3.",
    ),
    (
        "profile",
        "Each frame is timed span by span on the UI thread (input, flush, text, layout,\n\
         style, paint, semantics, upload, submit), the GPU's time where the backend\n\
         measures it, and its work counted. Metrics are p50, p95 and max over the frames;\n\
         an idle app draws none. The trace is kept in Ende under `target/viso/traces/`\n\
         (the newest 20) unless `--output` names a file. No session exits 3; an app that\n\
         leaves while profiled exits 5.",
    ),
    (
        "ios",
        "Profiles are kept in `~/.viso/` (`$VISO_HOME`); `viso run ios --device <id>`\n\
         runs on one. Every `viso ios` command needs macOS and Xcode.",
    ),
    (
        "ios use",
        "New simulators are made on the default runtime (`27` follows the newest 27.x).\n\
         A runtime is downloaded only by Xcode (`xcodebuild -downloadPlatform iOS`), only\n\
         with `--download` or a yes at a terminal; Xcode itself is never changed.",
    ),
    (
        "run",
        "`ios` and `android` run on a simulator or emulator profile: see\n\
         `viso ios simulator list` and `viso android emulator list`. The web targets\n\
         build for a browser and serve it as `viso serve` does. The app's own arguments\n\
         follow `--`. `--profile-frame` draws the frame time over the app's windows and\n\
         reports its frames as `viso profile` measures them.",
    ),
    (
        "serve",
        "`viso run web-gpu` serves the same and opens a browser too.",
    ),
    (
        "snapshot",
        "A snapshot is an `expect snapshot \"<name>\"` step of a scenario under `tests/ui/`, or\n\
         the end of one named after a golden; goldens are `tests/snapshots/<name>.headless.png`\n\
         with a `.json` sidecar, whose `tolerance` a rewrite keeps. A golden is rewritten only\n\
         by `capture` or `update`; what departs is left in `target/viso/snapshots/`.",
    ),
    (
        "test",
        "A scenario is `tests/ui/<name>.tape` or `tests/game/<name>.tape`; a failed case exits 1, a crashed runner 5.\n\
         On `--target ios|android` each UI scenario plays on a fresh launch of the app on\n\
         the device, on `--target web-gpu` in a headless Chromium (`VISO_CHROME`); its\n\
         snapshot steps are taken headless only.",
    ),
];

/// What `viso --version` reports: the CLI's version and commit, and the
/// versions of what it speaks — the language, the event stream, the
/// diagnostic object, the schema object, the IR dumps and the dev protocol.
/// Constants only: nothing is loaded to say them.
pub struct Version {
    pub version: &'static str,
    pub commit: &'static str,
    pub language: &'static str,
    pub event_schema: u64,
    pub diagnostic_schema: &'static str,
    pub schema_object: &'static str,
    pub dump_version: u32,
    pub snapshot_version: u32,
    pub dev_protocol: u16,
}

/// This build's [`Version`].
pub const VERSION: Version = Version {
    version: env!("CARGO_PKG_VERSION"),
    commit: env!("VISO_GIT_COMMIT"),
    language: viso_dsl::frontend::LANGUAGE_VERSION,
    event_schema: crate::output::json::SCHEMA_VERSION,
    diagnostic_schema: crate::output::json::DIAGNOSTIC_SCHEMA_VERSION,
    schema_object: viso_dsl::schema::SCHEMA_VERSION,
    dump_version: viso_dsl::dump::DUMP_VERSION,
    snapshot_version: crate::command::SNAPSHOT_VERSION,
    dev_protocol: viso_view::dev::wire::DEV_PROTOCOL_VERSION,
};

impl Version {
    /// The human text.
    pub fn text(&self) -> String {
        format!(
            "viso {} ({})
language {}
event schema {}
diagnostic schema {}
schema object {}
dump {}
snapshot {}
dev protocol {}
",
            self.version,
            self.commit,
            self.language,
            self.event_schema,
            self.diagnostic_schema,
            self.schema_object,
            self.dump_version,
            self.snapshot_version,
            self.dev_protocol
        )
    }

    /// The `result` payload.
    pub fn write(&self, w: &mut viso_ende::JsonWriter) {
        w.begin_object();
        w.name("version");
        w.string(self.version);
        w.name("commit");
        w.string(self.commit);
        w.name("language");
        w.string(self.language);
        w.name("event_schema");
        w.uint(self.event_schema);
        w.name("diagnostic_schema");
        w.string(self.diagnostic_schema);
        w.name("schema_object");
        w.string(self.schema_object);
        w.name("dump_version");
        w.uint(u64::from(self.dump_version));
        w.name("snapshot_version");
        w.uint(u64::from(self.snapshot_version));
        w.name("dev_protocol");
        w.uint(u64::from(self.dev_protocol));
        w.end_object();
    }
}

/// The page of every command but the top level.
const TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

{before-help}{all-args}{after-help}";

/// `command` with its help pages: the grouped top-level list, and every
/// command's examples and notes in the order of [`TEMPLATE`].
pub fn decorate(command: Command) -> Command {
    let root = format!(
        "{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\n{{before-help}}{}\
         Options:\n{{options}}{{after-help}}",
        groups(&command)
    );
    // One page per command: `--help` is `-h`, every value listed inline
    // rather than a paragraph each.
    let help = Arg::new("help")
        .short('h')
        .long("help")
        .global(true)
        .action(ArgAction::HelpShort)
        .help_heading(None::<&str>)
        .help("Print help");
    pages(command, "")
        .help_template(root)
        .disable_help_flag(true)
        .arg(help)
}

/// `command`, at `path`, and every command under it, with its examples and
/// notes.
fn pages(command: Command, path: &str) -> Command {
    let names: Vec<String> = command
        .get_subcommands()
        .map(|sub| sub.get_name().to_owned())
        .collect();
    let mut command = command.help_template(TEMPLATE);
    if let Some(examples) = examples_of(path) {
        command = command.before_help(examples);
    }
    if let Some(notes) = lookup(NOTES, path) {
        command = command.after_help(format!("Notes:\n{}", indent(notes)));
    }
    for name in names {
        let sub_path = if path.is_empty() {
            name.clone()
        } else {
            format!("{path} {name}")
        };
        command = command.mut_subcommand(&name, |sub| pages(sub, &sub_path));
    }
    command
}

/// The `Examples:` section of the command at `path`, its comments aligned.
fn examples_of(path: &str) -> Option<String> {
    let examples = lookup(EXAMPLES, path)?;
    let width = examples.iter().map(|(line, _)| line.len()).max()?;
    let mut text = String::from("Examples:\n");
    for (line, what) in examples {
        let _ = writeln!(text, "  {line:width$}  # {what}");
    }
    Some(text.trim_end().to_owned())
}

/// The top-level command list, in its groups; a group with no command yet is
/// left out.
fn groups(command: &Command) -> String {
    let width = command
        .get_subcommands()
        .map(|sub| sub.get_name().len())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (group, names) in GROUPS.iter().filter(|(_, names)| !names.is_empty()) {
        let _ = writeln!(text, "{group}");
        for name in *names {
            let about = command
                .find_subcommand(name)
                .and_then(Command::get_about)
                .map(ToString::to_string)
                .unwrap_or_default();
            let _ = writeln!(text, "  {name:width$}  {about}");
        }
        text.push('\n');
    }
    text
}

fn lookup<T: Copy>(table: &[(&str, T)], path: &str) -> Option<T> {
    table
        .iter()
        .find(|(key, _)| *key == path)
        .map(|(_, value)| *value)
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every command path under `command`, `""` for the top level.
    fn paths(command: &Command, path: &str, out: &mut Vec<String>) {
        out.push(path.to_owned());
        for sub in command.get_subcommands() {
            let sub_path = if path.is_empty() {
                sub.get_name().to_owned()
            } else {
                format!("{path} {}", sub.get_name())
            };
            paths(sub, &sub_path, out);
        }
    }

    fn all_paths() -> Vec<String> {
        let mut out = Vec::new();
        paths(&crate::args::command(), "", &mut out);
        out
    }

    #[test]
    fn every_top_level_command_is_in_exactly_one_group() {
        let command = crate::args::command();
        let grouped: Vec<&str> = GROUPS
            .iter()
            .flat_map(|(_, names)| *names)
            .copied()
            .collect();
        for sub in command.get_subcommands() {
            let name = sub.get_name();
            let count = grouped.iter().filter(|n| **n == name).count();
            assert_eq!(count, 1, "`{name}` is in {count} groups");
        }
        for name in grouped {
            assert!(
                command.find_subcommand(name).is_some(),
                "the group lists `{name}`, which is not a command"
            );
        }
    }

    /// Two or three examples for every command, each one a command line of
    /// that command which parses.
    #[test]
    fn every_command_has_examples_that_parse() {
        let paths = all_paths();
        for path in &paths {
            let examples =
                lookup(EXAMPLES, path).unwrap_or_else(|| panic!("`{path}`: no examples"));
            assert!(
                (2..=3).contains(&examples.len()),
                "`{path}`: {} examples",
                examples.len()
            );
            for (line, what) in examples {
                assert!(
                    path.is_empty() || line.starts_with(&format!("viso {path}")),
                    "`{line}` is not an example of `viso {path}`"
                );
                assert!(
                    !what.is_empty() && !what.ends_with('.'),
                    "`{line}`: {what:?}"
                );
                let argv: Vec<&str> = line.split(' ').collect();
                if let Err(error) = crate::args::command().try_get_matches_from(&argv) {
                    panic!("`{line}` does not parse: {error}");
                }
            }
        }
        let keys = EXAMPLES
            .iter()
            .map(|(key, _)| key)
            .chain(NOTES.iter().map(|(key, _)| key));
        for key in keys {
            assert!(paths.iter().any(|p| p == key), "`{key}` is not a command");
        }
    }

    /// The top-level page reads purpose, usage, examples, the groups,
    /// options and notes, in that order.
    #[test]
    fn the_top_level_page_reads_in_order() {
        let help = crate::args::command().render_help().to_string();
        let at = |needle: &str| {
            help.find(needle)
                .unwrap_or_else(|| panic!("no `{needle}` in\n{help}"))
        };
        let order = [
            at("Build, check and run"),
            at("Usage: viso"),
            at("Examples:"),
            at("PROJECT\n"),
            at("MOBILE DEV ENVIRONMENT\n"),
            at("DELIVERY\n"),
            at("MAINTENANCE\n"),
            at("Options:"),
            at("Notes:"),
        ];
        assert!(order.is_sorted(), "{help}");
    }

    /// A command page reads purpose, usage, examples, arguments, options and
    /// notes, the global options after its own.
    #[test]
    fn a_command_page_reads_in_order() {
        let help = crate::args::command()
            .try_get_matches_from(["viso", "run", "--help"])
            .unwrap_err()
            .render()
            .to_string();
        let at = |needle: &str| {
            help.find(needle)
                .unwrap_or_else(|| panic!("no `{needle}` in\n{help}"))
        };
        let order = [
            at("Build a dev artifact"),
            at("Usage: viso run"),
            at("Examples:"),
            at("Arguments:"),
            at("Options:"),
            at("Global options:"),
            at("Notes:"),
        ];
        assert!(order.is_sorted(), "{help}");
    }
}
