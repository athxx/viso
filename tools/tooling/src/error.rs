//! Exit codes, and every diagnostic code the tools raise themselves, each bound
//! to the exit code a failure under it ends the command with.
//!
//! A code is declared once here, so the exit a failure takes cannot drift from
//! the code it reports: [`Reporter::failure`](crate::report::Reporter::failure)
//! returns the exit of the code it is given.

/// Nothing failed.
pub const SUCCESS: u8 = 0;
/// A source, configuration, check or test diagnostic failed.
pub const DIAGNOSTICS: u8 = 1;
/// The command line asked for something it cannot.
pub const USAGE: u8 = 2;
/// The environment could not provide what the command needs: an SDK, a
/// toolchain, a device, a free lock.
pub const ENVIRONMENT: u8 = 3;
/// The build, the compiler or the linker failed.
pub const BUILD: u8 = 4;
/// The app, a device or a test runner failed while running.
pub const RUNTIME: u8 = 5;
/// Packaging, signing or an exporter failed.
pub const PACKAGE: u8 = 6;
/// A tooling service broke its protocol, or the CLI itself failed.
pub const INTERNAL: u8 = 7;
/// The user interrupted the command.
pub const INTERRUPTED: u8 = 130;

/// A diagnostic code the CLI raises, and the exit its failure takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Code {
    /// The stable name, as the diagnostic reports it.
    pub name: &'static str,
    /// The exit code.
    pub exit: u8,
    /// What it means, in a sentence.
    pub summary: &'static str,
    /// Why it happens, for `viso explain`.
    pub explanation: &'static str,
    /// How to resolve it.
    pub fix: &'static str,
    /// Commands that help.
    pub commands: &'static [&'static str],
}

macro_rules! codes {
    ($(
        $(#[$doc:meta])*
        $name:ident = $exit:ident {
            summary: $summary:literal,
            explanation: $explanation:literal,
            fix: $fix:literal,
            commands: [$($command:literal),* $(,)?],
        }
    )*) => {
        $(
            $(#[$doc])*
            pub const $name: Code = Code {
                name: stringify!($name),
                exit: $exit,
                summary: $summary,
                explanation: $explanation,
                fix: $fix,
                commands: &[$($command),*],
            };
        )*
        /// Every code, in declaration order.
        pub const ALL: &[Code] = &[$($name),*];
    };
}

codes! {
    CLI_USAGE = USAGE {
        summary: "The command line did not parse or asks for something the command does not do.",
        explanation: "The arguments name an unknown command or option, miss a required value, combine options that exclude each other, or ask a command for a target or profile it does not build. Nothing ran.",
        fix: "Read the command's help page and correct the command line; the diagnostic names the next command to run.",
        commands: ["viso <command> --help"],
    }
    INTERNAL_PANIC = INTERNAL {
        summary: "The CLI failed on its own; this is a bug in Viso.",
        explanation: "The CLI hit an internal invariant failure and stopped. Its output up to that point is valid; the failure is not caused by the project.",
        fix: "Report the command line and the message to the Viso issue tracker. Rerunning with `--json` records the full event stream to attach.",
        commands: [],
    }

    ENV_CURRENT_DIR = ENVIRONMENT {
        summary: "The current directory could not be read.",
        explanation: "The process's working directory was removed, renamed or made unreadable while the shell still points at it, so no project can be located from it.",
        fix: "Change into an existing directory, or name the project with `--project <path>`.",
        commands: [],
    }
    ENV_STDOUT = ENVIRONMENT {
        summary: "Standard output could not be written.",
        explanation: "Writing to standard output failed, usually because the reading end of a pipe closed early or the output file's disk is full.",
        fix: "Keep the consumer of the output reading until the command ends, or redirect the output to a writable file.",
        commands: [],
    }
    ENV_SOURCE_UNREADABLE = ENVIRONMENT {
        summary: "A source file the command needs could not be read.",
        explanation: "A `.vs` source, a message catalog or an asset directory exists but cannot be read: its permissions, a broken link, or a file that is not UTF-8 text.",
        fix: "Fix the file's permissions or encoding; the diagnostic names the path.",
        commands: ["viso check"],
    }
    ENV_CARGO_MANIFEST = ENVIRONMENT {
        summary: "The project root has no `Cargo.toml` to build.",
        explanation: "A Viso app is a Cargo package: the build runs Cargo in the directory that holds `Viso.toml`, and that directory has no `Cargo.toml`.",
        fix: "Create the project with `viso new`, or add a `Cargo.toml` beside `Viso.toml` that depends on `viso`.",
        commands: ["viso new <name>"],
    }
    ENV_CARGO = ENVIRONMENT {
        summary: "Cargo could not be run.",
        explanation: "The build starts `cargo`, which is not on `PATH` or failed to start.",
        fix: "Install Rust with rustup and make sure `cargo` is on `PATH`.",
        commands: ["viso doctor"],
    }
    ENV_NO_EXECUTABLE = ENVIRONMENT {
        summary: "The build produced no executable, or more than one.",
        explanation: "`viso run` launches the one binary target the package builds; the package has none, or several and none is chosen.",
        fix: "Give the package exactly one binary target (`src/main.rs`), or set `default-run` in `Cargo.toml`.",
        commands: [],
    }
    ENV_DEV_CHANNEL = ENVIRONMENT {
        summary: "The dev channel could not be opened.",
        explanation: "A dev session talks to the running app over a loopback socket, or, for a board over SSH, a socket on this machine's address on the route to it, which must be on a local network (private, link-local or unique-local); binding it failed, usually because the port is taken or loopback networking is blocked, or the route to the board leaves from a public address.",
        fix: "Stop the other process holding the port, or let the session pick a free one by not pinning it in the configuration.",
        commands: ["viso doctor"],
    }
    ENV_DEVICE = ENVIRONMENT {
        summary: "The simulator or emulator toolchain is missing or failed.",
        explanation: "Starting, creating or reaching a simulator or emulator needs the platform's tools (Xcode's `simctl`, the Android emulator), which are missing or reported a failure.",
        fix: "Install the platform toolchain, then check it.",
        commands: ["viso doctor"],
    }
    ENV_DEVICE_TARGET = ENVIRONMENT {
        summary: "The device target runs on its board, not on this machine.",
        explanation: "A device target of `Viso.toml` (`[target.<name>]`) is built for its board's triple, and an embedded profile drives the display through DRM/KMS. `viso run` runs it on the board its `deploy` names, over SSH, or here where this machine is such a board: Linux, with the target's triple.",
        fix: "Give the target `deploy = \"user@host\"` to run the session on the board, run an `embedded-cpu` target with `--emulate` on a Mac, or build it with `viso build <name>` and start the build in `dist/<name>/` on the board.",
        commands: ["viso build <name>"],
    }
    ENV_DEPLOY = ENVIRONMENT {
        summary: "The board could not be reached, or the app could not be copied to it.",
        explanation: "`viso run <name>` of a device target with `deploy` opens a session directory on the board with `ssh` and copies the dev artifact into it with `scp`, both in batch mode, which asks for no password. The board did not answer, refused the login, or the copy failed.",
        fix: "Check that `ssh <deploy>` logs in without a password (the board takes this machine's key, or an agent's) and that the board has room in its home directory.",
        commands: [],
    }
    ENV_EMULATE = ENVIRONMENT {
        summary: "The device target's profile cannot be emulated on this machine.",
        explanation: "`viso run <name> --emulate` runs an `embedded-cpu` target in a macOS window, drawn by the same CPU raster as on the board. An `embedded-gpu` board draws through its card's GLES 3 or Vulkan, which a desktop does not have, and other hosts have no emulation window.",
        fix: "Run an `embedded-gpu` target on its board with `deploy`, or emulate on a Mac.",
        commands: [],
    }
    ENV_NO_DEV_SESSION = ENVIRONMENT {
        summary: "No `viso run` session is running for the project, or it cannot be reached.",
        explanation: "The command talks to a running dev session of the project, and none is registered, or its socket does not answer.",
        fix: "Start one with `viso run` in another terminal and rerun the command.",
        commands: ["viso run"],
    }
    ENV_TAPE_WRITE = ENVIRONMENT {
        summary: "A recorded tape could not be written.",
        explanation: "Recording ended but the tape file could not be created or written: its directory is missing or read-only, or the disk is full.",
        fix: "Record into a writable path.",
        commands: [],
    }
    ENV_TEST_INPUT = ENVIRONMENT {
        summary: "A test scenario or tape could not be found or read.",
        explanation: "A scenario or tape named on the command line, or found under `tests/`, does not exist or cannot be read.",
        fix: "Check the path and its permissions; `viso test --filter` selects scenarios by name.",
        commands: ["viso test --help"],
    }
    ENV_SNAPSHOT_TARGET = ENVIRONMENT {
        summary: "Snapshots are not taken on that target here.",
        explanation: "`viso snapshot` renders each scenario in a headless build of the app, drawn by the software raster; this build of viso takes snapshots on no other target.",
        fix: "Take them on the headless target, the default.",
        commands: ["viso snapshot compare --target headless"],
    }
    ENV_SNAPSHOT_WRITE = ENVIRONMENT {
        summary: "A golden snapshot, or what a comparison found, could not be written.",
        explanation: "Capturing and updating write goldens under `tests/snapshots/` (or `--output`); a comparison that departs writes the image taken, its tree and a diff under `target/viso/snapshots/`. Creating or writing one of them failed: its directory is read-only, or the disk is full.",
        fix: "Make the directory writable, or free disk space, and rerun.",
        commands: [],
    }
    ENV_LSP = ENVIRONMENT {
        summary: "The language server `viso-lsp` was not found or could not be started.",
        explanation: "`viso lsp` runs the `viso-lsp` binary installed next to `viso`, or else the first on `PATH`, handing it the editor's stdin and stdout. Neither place has one, or starting it failed.",
        fix: "Install `viso-lsp` with the same Viso version as the CLI (`cargo install viso-lsp`), next to `viso` or on `PATH`.",
        commands: ["viso doctor"],
    }
    ENV_DUMP_WRITE = ENVIRONMENT {
        summary: "The dump could not be written to the `--out` file.",
        explanation: "`viso dump --out` writes the dump to a file, and creating or writing it failed: its directory is missing or read-only, or the disk is full.",
        fix: "Give `--out` a writable path, or drop it to print the dump.",
        commands: [],
    }
    ENV_SOURCE_WRITE = ENVIRONMENT {
        summary: "A source file the command rewrites could not be written.",
        explanation: "The formatter or a fix rewrites a source in place and the write failed: a read-only file or directory, or a full disk. The original is left as it was.",
        fix: "Make the file writable, or check the formatting without writing it.",
        commands: ["viso fmt --check"],
    }
    ENV_LOCK_HELD = ENVIRONMENT {
        summary: "Another viso process holds a lock the command needs free.",
        explanation: "Builds, dev sessions and cleaning take locks under `target/viso/locks` so two processes never write the same output; another live process holds the one this command needs. A lock its holder stopped refreshing is taken over.",
        fix: "Wait for the other command to finish, or stop it; the diagnostic names its process and age.",
        commands: [],
    }
    ENV_REMOVE = ENVIRONMENT {
        summary: "A file or directory the command removes could not be removed.",
        explanation: "Cleaning or replacing an output failed to delete a path, usually because of permissions or a file another process keeps open.",
        fix: "Close what holds the file, fix its permissions, and rerun.",
        commands: ["viso clean"],
    }
    ENV_BUILD_WRITE = ENVIRONMENT {
        summary: "The build's output could not be written.",
        explanation: "The build's generated sources, cache or artifacts could not be written under `target/viso` or `dist`: permissions, or a full disk.",
        fix: "Free disk space or fix the permissions of the output directories; `--target-dir` moves them.",
        commands: [],
    }
    ENV_TOOL = ENVIRONMENT {
        summary: "A tool the build needs is not installed or could not be run.",
        explanation: "A step of the build calls an external tool (a wasm optimizer, a packager, a signing tool) that is missing from `PATH` or failed to start.",
        fix: "Install the tool the diagnostic names, then check the environment.",
        commands: ["viso doctor"],
    }
    ENV_TEST_TARGET = ENVIRONMENT {
        summary: "The test domain does not run on that target here.",
        explanation: "The tests asked for run on a target this machine cannot run them on, for example a GPU target without a GPU.",
        fix: "Run them on the headless target, or on a machine that has the target.",
        commands: ["viso test --target headless"],
    }
    ENV_RUST_TOOLCHAIN = ENVIRONMENT {
        summary: "The Rust toolchain is not installed, or older than Viso builds with.",
        explanation: "Viso builds with a minimum Rust version, and the installed toolchain is older or missing.",
        fix: "Update Rust with `rustup update`, then check the environment.",
        commands: ["viso doctor"],
    }
    ENV_RUST_TARGET = ENVIRONMENT {
        summary: "The Rust standard library of a target the build needs is not installed.",
        explanation: "Building for a platform other than the host needs that target's standard library, which rustup has not installed.",
        fix: "Install it with `rustup target add <triple>`; the diagnostic names the triple.",
        commands: ["viso doctor"],
    }
    ENV_GPU = ENVIRONMENT {
        summary: "The host has no GPU backend Viso renders with.",
        explanation: "Running a windowed app needs Metal, Vulkan or Direct3D 12, and none is available on this machine or in this session (for example over a remote shell).",
        fix: "Run on a machine with a supported GPU, or use the headless target for tests.",
        commands: ["viso doctor", "viso test --target headless"],
    }
    ENV_ANDROID_SDK = ENVIRONMENT {
        summary: "No Android SDK, with its NDK, build tools and a platform, is found.",
        explanation: "Android builds need an SDK with an NDK, build tools and a platform; none is found under `ANDROID_HOME`, `ANDROID_SDK_ROOT`, the SDK `viso android use` installs, or the default locations.",
        fix: "Install a tested component set with `viso android use <api>`, or point `ANDROID_HOME` at an existing SDK.",
        commands: ["viso android use 36", "viso android doctor"],
    }
    ENV_ANDROID_LICENSE = ENVIRONMENT {
        summary: "A license of the Android components to install is not accepted.",
        explanation: "The Android SDK archives are published under licenses that must be accepted before they are installed. Viso asks at a terminal; elsewhere, or when declined, nothing is downloaded.",
        fix: "Run the command at a terminal and accept, or pass `--accept-licenses` once you have read them.",
        commands: ["viso android use 36 --accept-licenses"],
    }
    ENV_DOWNLOAD = ENVIRONMENT {
        summary: "A toolchain archive could not be downloaded.",
        explanation: "The transfer of a pinned archive failed (no network, a refused connection, an HTTP error). What arrived is kept, so the next attempt resumes it.",
        fix: "Check the network, then run the command again.",
        commands: [],
    }
    ENV_DOWNLOAD_INTEGRITY = ENVIRONMENT {
        summary: "A downloaded archive is not the one pinned.",
        explanation: "Every toolchain archive is pinned by its size and digest; what arrived differs, so it was deleted and nothing of it was unpacked. A proxy rewriting downloads, or a changed upstream file, does this.",
        fix: "Run the command again; if it persists, the network between this machine and the repository alters the file.",
        commands: [],
    }
    ENV_INSTALL = ENVIRONMENT {
        summary: "A verified toolchain archive could not be unpacked into place.",
        explanation: "The archive was verified, but unpacking it into the staging directory or moving that into the SDK failed, usually for disk space or permissions; the component was left as it was.",
        fix: "Free disk space or fix the permissions of `~/.viso`, then run the command again.",
        commands: [],
    }
    ENV_OFFLINE = ENVIRONMENT {
        summary: "`--offline` was given and something needs downloading.",
        explanation: "With `--offline` nothing is downloaded, and a component that is not installed and verified already is missing.",
        fix: "Run the command without `--offline`.",
        commands: [],
    }

    ENV_IOS_HOST = ENVIRONMENT {
        summary: "The iOS Simulator is on macOS only.",
        explanation: "`viso ios` manages Xcode's Simulator runtimes and simulators, which exist on a Mac only; on another host every `viso ios` command stops here.",
        fix: "Develop for iOS on a Mac; elsewhere `viso run android` or `viso run` runs the app.",
        commands: [],
    }
    ENV_XCODE = ENVIRONMENT {
        summary: "Xcode is not installed or not the selected developer directory.",
        explanation: "The Simulator, `simctl` and runtime downloads come with Xcode; the Command Line Tools alone have none of them.",
        fix: "Install Xcode from the App Store and select it with `sudo xcode-select -s /Applications/Xcode.app`.",
        commands: ["viso ios doctor"],
    }
    ENV_XCODE_LICENSE = ENVIRONMENT {
        summary: "The Xcode license is not accepted.",
        explanation: "Xcode's tools refuse to run until its license is accepted, which needs an administrator and is never done by Viso.",
        fix: "Run `sudo xcodebuild -license` and accept it.",
        commands: ["viso ios doctor"],
    }

    BUILD_FAILED = BUILD {
        summary: "`cargo build` failed.",
        explanation: "The Rust build of the app failed; the compiler's own diagnostics precede this one and name the cause.",
        fix: "Fix the errors the Rust compiler reported; `rustc --explain <code>` explains a Rust error code.",
        commands: ["viso check"],
    }
    BUILD_PACKAGE = BUILD {
        summary: "The built app could not be packaged for its target.",
        explanation: "The executable was built, but wrapping it for its target (an app bundle, an APK, a web bundle) failed.",
        fix: "Read the packaging step's message; it names the file or tool that failed.",
        commands: [],
    }
    PACKAGE_SIGNING = PACKAGE {
        summary: "The package could not be signed as `--signing` asks.",
        explanation: "`--signing required` found no identity, provisioning profile, keystore or certificate configured or on this machine; or the one `Viso.toml` names is missing from the keychain, its password is not in the environment, or the signing tool failed on it. A secret is never read from the command line or `Viso.toml`.",
        fix: "Name the identity in `[package.macos]`, `[package.ios]`, `[package.android]` or `[package.windows]`, keep its key in the keychain or certificate store and a keystore's passwords in `VISO_ANDROID_KEYSTORE_PASSWORD`/`VISO_ANDROID_KEY_PASSWORD`; `--dry-run` shows what would sign.",
        commands: ["viso package --dry-run"],
    }
    PACKAGE_UNSIGNED = PACKAGE {
        summary: "The package is signed short of what its platform needs to install it elsewhere.",
        explanation: "Under `--signing auto` nothing better was configured or found, so the package is signed ad hoc, with the debug keystore, or not at all: it runs where it was built or installs for testing, but a store or another machine refuses it. The command still succeeds.",
        fix: "Configure the platform's identity as the warning's note says, or pass `--signing off` to package unsigned on purpose.",
        commands: ["viso package --dry-run"],
    }
    PACKAGE_ARCHIVE = PACKAGE {
        summary: "The package or its archive could not be made.",
        explanation: "The app was built, but wrapping it as its platform ships it failed: the `.app` or disk image, the asset catalog or `.ipa`, the APK or app bundle, the zip or tar, in its tool or on disk.",
        fix: "Read the step's message; it names the tool or file that failed.",
        commands: [],
    }
    EXPORT_HTML_DYNAMIC_REQUIRED = DIAGNOSTICS {
        summary: "`--static` was asked for, but the view changes while it runs.",
        explanation: "A static export has no JavaScript, so nothing in it can change: a binding that reads state or input, a handler, an `if`, `for` or `match`, a control the user moves. The diagnostic points at the first such place in each node.",
        fix: "Drop `--static` to export the changing parts as vanilla JavaScript, or make the value constant.",
        commands: ["viso export html"],
    }
    EXPORT_HTML_GPU_ONLY_NODE = DIAGNOSTICS {
        summary: "A node only the Viso renderer can draw has no HTML form.",
        explanation: "A `ShaderView` paints with a shader on the GPU; an HTML export has no element that runs it, and dropping it would change what the page shows.",
        fix: "Build for the browser with the Viso renderer, `viso build web-gpu`, or with DOM and GPU islands, `viso build web-hybrid`; or rewrite the node with widgets HTML has.",
        commands: ["viso build web-gpu", "viso build web-hybrid"],
    }
    EXPORT_SOLID_GPU_ONLY_NODE = DIAGNOSTICS {
        summary: "A node only the Viso renderer can draw has no SolidJS form.",
        explanation: "A `ShaderView` paints with a shader on the GPU; a SolidJS component renders DOM elements, none of which runs it, and dropping it would change what the component shows.",
        fix: "Build for the browser with the Viso renderer, `viso build web-gpu`, or with DOM and GPU islands, `viso build web-hybrid`; or rewrite the node with widgets the DOM has.",
        commands: ["viso build web-gpu", "viso build web-hybrid"],
    }
    EXPORT_UNSUPPORTED = DIAGNOSTICS {
        summary: "Part of the view has no form the exporter writes.",
        explanation: "The exporter lowers what it can say as HTML, CSS and JavaScript (vanilla, or a SolidJS component) with the same meaning; this construct (a game's systems, a task, an effect, a native call, a Rust component) it cannot, and it never drops one silently.",
        fix: "Build for the browser instead, `viso build web-gpu`, or move the construct out of the exported view.",
        commands: ["viso build web-gpu"],
    }
    EXPORT_NO_VIEW = DIAGNOSTICS {
        summary: "The project has no view to export.",
        explanation: "An export starts from the component it is named, else from the one `src/app.vs` exports, the app's own; the project declares no such component.",
        fix: "Name a component the project declares, `Name` or `module::Name`, or add `src/app.vs` as `viso new` starts a project.",
        commands: ["viso new"],
    }
    EXPORT_CONFLICT = PACKAGE {
        summary: "The export directory holds files the export did not write, or that were edited since.",
        explanation: "An export replaces its directory whole, and only while every file there is one it wrote, unchanged, as its `.viso-generated.json` records. A file someone added or edited would be lost.",
        fix: "Move the edits out, export into another directory with `--out`, or pass `--force` to replace them.",
        commands: ["viso export html --force"],
    }
    EXPORT_WRITE = PACKAGE {
        summary: "The export could not be written.",
        explanation: "Writing the staging tree, or putting it in place of the export directory, failed; the directory is as it was.",
        fix: "Read the message; it names the file and the system's error.",
        commands: [],
    }
    WEB_GPU_ONLY_NODE = DIAGNOSTICS {
        summary: "A node only the Viso renderer can draw is in a `web-dom` page.",
        explanation: "A `web-dom` page is HTML the browser lays out and draws; a `ShaderView` paints with a shader and a `Viewport3D` draws a scene on the GPU, which no element of the page runs, and dropping the node would change what the page shows.",
        fix: "Build for the browser with the Viso renderer, `viso build web-gpu`, or with DOM and GPU islands, `viso build web-hybrid`; or rewrite the node with widgets the DOM has.",
        commands: ["viso build web-gpu", "viso build web-hybrid"],
    }
    WEB_DOM_UNSUPPORTED = DIAGNOSTICS {
        summary: "Part of the view has no form in a `web-dom` page.",
        explanation: "A `web-dom` page lowers the view to HTML and CSS whose behavior runs on the VM in the page; this construct (a Rust component or widget, a router's pages, a game's systems, a value the page cannot switch) only the Viso runtime mounts, and it is never dropped silently.",
        fix: "Build for the browser with the Viso renderer, `viso build web-gpu`, or move the construct out of the view the page mounts.",
        commands: ["viso build web-gpu"],
    }
    WEB_DOM_NO_VIEW = DIAGNOSTICS {
        summary: "The project has no view for its `web-dom` page.",
        explanation: "A `web-dom` or `web-hybrid` page mounts the component `src/app.vs` exports, the root view `Application::build` mounts with `view!`; the project has no such file.",
        fix: "Add `src/app.vs` exporting the app's root component, as `viso new` starts a project, and mount it with `view!(\"app.vs\")`.",
        commands: ["viso new"],
    }
    WEB_HYBRID_NO_ISLAND = DIAGNOSTICS {
        summary: "A `web-hybrid` page's view has no `Island`.",
        explanation: "A `web-hybrid` page is a `web-dom` page with one `Island`, the component the Viso renderer draws in it; the view has none, so the renderer would have nothing to draw and its window would cover the page.",
        fix: "Wrap the component the renderer draws in `Island { … }`, or build the page alone with `viso build web-dom`.",
        commands: ["viso build web-dom"],
    }
    RUN_APP_FAILED = RUNTIME {
        summary: "The app could not be launched, crashed, or exited with a failure.",
        explanation: "The app started by `viso run` did not start, crashed, or exited with a nonzero status; its own output precedes this diagnostic.",
        fix: "Read the app's output and its exit record; rerun with the debugger or logs the diagnostic suggests.",
        commands: [],
    }
    RUN_TEST_CRASHED = RUNTIME {
        summary: "A test case's runner could not start, crashed, or ended without its report.",
        explanation: "A test ran in its own process, which crashed or exited before reporting a result. The other cases still ran.",
        fix: "Run the case alone with `--nocapture` to see its output.",
        commands: ["viso test --filter <name> --nocapture"],
    }
    RUN_SNAPSHOT_FAILED = RUNTIME {
        summary: "A scenario's snapshot could not be taken.",
        explanation: "A snapshot step reads the frame back from the headless surface and writes it with its tree; the run had no surface to read, or could not write the files.",
        fix: "Run the scenario alone with `--nocapture` to see the run's output.",
        commands: ["viso test ui <scenario> --nocapture"],
    }

    RUN_SHEET_FAILED = RUNTIME {
        summary: "A game scenario's frame sheet could not be drawn.",
        explanation: "`--sheet` runs the scenario's game again in a headless build of the app, which draws the frames through the software raster and writes them as one image; the build did not start, the game did not run there, or the image could not be written.",
        fix: "Run the scenario alone with `--nocapture` to see the run's output.",
        commands: ["viso test game <scenario> --sheet sheet.png --nocapture"],
    }

    EXPLAIN_UNKNOWN = DIAGNOSTICS {
        summary: "No diagnostic code has that name.",
        explanation: "`viso explain` knows every code the compiler, the project configuration and the CLI raise; the one given is none of them. A code of the form `E0XXX` is the Rust compiler's.",
        fix: "Use one of the codes the diagnostic suggests, or list them all.",
        commands: ["viso explain --list"],
    }

    TEST_INPUT_INVALID = DIAGNOSTICS {
        summary: "A test scenario or tape does not parse or fit the build.",
        explanation: "A scenario or tape has a syntax error, names a node, action or state the app does not have, or was recorded for another build.",
        fix: "Fix the step the diagnostic points at, or record the tape again.",
        commands: [],
    }
    TEST_SNAPSHOT_MISMATCH = DIAGNOSTICS {
        summary: "A snapshot differs from its golden.",
        explanation: "The image a scenario's snapshot step took has another size than its golden under `tests/snapshots/`, or more pixels departing from it than the golden's tolerance allows. A pixel departs when its perceptual distance from the golden's is above the tolerance's threshold. The image taken, its tree and a diff are under `target/viso/snapshots/`.",
        fix: "Look at the diff: fix the app, or rewrite the golden with `viso snapshot update` if the change is meant. The sidecar's `tolerance` loosens the comparison of one snapshot.",
        commands: ["viso snapshot update"],
    }
    TEST_SNAPSHOT_MISSING = DIAGNOSTICS {
        summary: "A snapshot has no golden, or a golden no snapshot.",
        explanation: "A scenario takes a snapshot that has no golden under `tests/snapshots/`, or a golden there is taken by no scenario. A golden is written only on request, never because a comparison failed.",
        fix: "Write the missing golden with `viso snapshot update`; remove a golden no scenario takes.",
        commands: ["viso snapshot update"],
    }
    TEST_SNAPSHOT_INVALID = DIAGNOSTICS {
        summary: "A golden snapshot or its sidecar does not read.",
        explanation: "The golden's image is not a PNG, or its JSON sidecar does not parse or gives a tolerance out of range: a threshold from 0 to 1 and a count of pixels.",
        fix: "Fix the sidecar, or capture the golden again.",
        commands: ["viso snapshot capture <scenario>"],
    }
    TEST_EXPECTATION = DIAGNOSTICS {
        summary: "A test's expectation was not met.",
        explanation: "An `expect` step of a scenario, or an assertion of a test, found a different value or tree than it states.",
        fix: "Compare the expected and actual values the diagnostic lists; fix the app, or the expectation if the change is meant.",
        commands: [],
    }

    NEW_NAME_INVALID = USAGE {
        summary: "The package name is not one Cargo accepts.",
        explanation: "The project's name becomes its Cargo package name, which must be ASCII letters, digits, `-` or `_`, not start with a digit, and not be a Rust keyword.",
        fix: "Give a valid name with `--name`.",
        commands: ["viso new <path> --name <name>"],
    }
    NEW_PATH_TAKEN = USAGE {
        summary: "The project's directory is a file, or holds files and `--force-empty-dir` is not given, or holds a file the project would write.",
        explanation: "`viso new` never replaces a file: the target directory must be empty unless `--force-empty-dir` is given, and even then none of the files the template writes may exist.",
        fix: "Pick another directory, or pass `--force-empty-dir` when its other files may stay.",
        commands: ["viso new <path> --force-empty-dir"],
    }
    NEW_FRAMEWORK = USAGE {
        summary: "The `--framework` path is not the `viso` crate or a checkout holding it.",
        explanation: "`--framework` points at a local copy of Viso to depend on; the path must be the `viso` crate or a checkout with `crates/viso`.",
        fix: "Point it at the checkout's root or its `crates/viso` directory.",
        commands: [],
    }
    NEW_GIT = ENVIRONMENT {
        summary: "Git could not make the new project a repository.",
        explanation: "The project was created, but `git init` was not found or failed, so it is not a repository.",
        fix: "Install Git and run `git init` in the project, or create projects with `--no-git`.",
        commands: [],
    }
    NEW_WRITE = INTERNAL {
        summary: "The project's files could not be written; nothing was left behind.",
        explanation: "Writing the template into its staging directory failed, so the project was not created and the staging files were removed.",
        fix: "Check that the parent directory is writable and has space, then rerun.",
        commands: [],
    }

    DEVICE_MISSING = ENVIRONMENT {
        summary: "No simulator or emulator profile exists.",
        explanation: "Running on iOS or Android needs a device profile, and none has been created.",
        fix: "Create one, then run again.",
        commands: ["viso ios simulator create", "viso android emulator create"],
    }
    DEVICE_UNKNOWN = USAGE {
        summary: "`--device` or a configured default names no profile.",
        explanation: "The device named on the command line or in the configuration is not one of the profiles that exist.",
        fix: "List the profiles and name one of them.",
        commands: ["viso ios simulator list", "viso android emulator list"],
    }
    DEVICE_AMBIGUOUS = USAGE {
        summary: "Several profiles exist and none is the default.",
        explanation: "With several device profiles and no default, a command run without a terminal to ask in cannot choose one.",
        fix: "Name the device with `--device`, or set a default profile.",
        commands: [],
    }
    DEVICE_ID_INVALID = USAGE {
        summary: "A profile ID that is not one.",
        explanation: "A device profile ID is lowercase ASCII letters, digits and `-`; the one given is not.",
        fix: "Give an ID of that form.",
        commands: [],
    }
    DEVICE_EXISTS = USAGE {
        summary: "A profile of that ID exists already.",
        explanation: "Creating a device profile never replaces one; a profile with that ID is there.",
        fix: "Pick another ID, or delete the existing profile first.",
        commands: [],
    }
    DEVICE_SYSTEM = ENVIRONMENT {
        summary: "No installed system image or runtime fits the profile.",
        explanation: "The profile asks for an OS version or system image that is not installed for this host.",
        fix: "Install the runtime or system image, or create the profile for one that is installed.",
        commands: ["viso doctor"],
    }

    DUMP_INPUT = USAGE {
        summary: "The file `viso dump` is given is not a `.vs` source of the project.",
        explanation: "A dump covers the project's package: a file input must be one of the `.vs` sources the package loads from `src/`. Anything else, a missing file or one outside the package, has no IR to dump.",
        fix: "Name a `.vs` file under the project's `src/`, a declaration by its name, or nothing to dump every file.",
        commands: ["viso dump --help"],
    }

    ANDROID_API_UNSUPPORTED = USAGE {
        summary: "The Android API level is not one Viso supports.",
        explanation: "`viso android use` installs a component set Viso is tested with, and has one for each supported API level only.",
        fix: "Pick a level `viso android list` shows.",
        commands: ["viso android list"],
    }
    ANDROID_ADB_FAILED = ENVIRONMENT {
        summary: "`adb` reported a failure.",
        explanation: "`viso android adb --json` ran the SDK's `adb`, which exited with a failure status: the device it addressed, or what it was asked, did not let it finish.",
        fix: "Read the `log` events it wrote; check the emulator is running and the arguments are ones `adb` takes.",
        commands: ["viso android emulator list"],
    }
    IOS_RUNTIME_INVALID = USAGE {
        summary: "The iOS runtime given is not a version.",
        explanation: "`viso ios use` takes a Simulator runtime version — `27`, `26.3`, `iOS 27.0` — of up to three numbers.",
        fix: "Pick a version `viso ios list` shows.",
        commands: ["viso ios list"],
    }
    IOS_RUNTIME_MISSING = USAGE {
        summary: "The iOS runtime is not installed, and installing it was not asked for.",
        explanation: "A Simulator runtime is a download of several gigabytes through Xcode; `viso ios use` starts one only when `--download` is given or the user agrees at a terminal.",
        fix: "Pass `--download`, or run the command at a terminal and agree.",
        commands: ["viso ios list"],
    }
    INSPECT_SELECTOR = USAGE {
        summary: "The selector `viso inspect query` is given does not parse.",
        explanation: "A selector is one or more comma-separated chains of compounds — a type, `#name`, `.Style`, `[attribute]` or `[attribute=value]` — joined by a space (descendant) or `>` (child); the one given breaks that grammar or names an attribute there is none of.",
        fix: "Correct the selector at the byte the diagnostic points at; `viso inspect query --help` lists the grammar and the attributes.",
        commands: ["viso inspect query --help"],
    }
    INSPECT_UNSUPPORTED = ENVIRONMENT {
        summary: "The app's inspector does not tell what was asked.",
        explanation: "A runtime reports what its inspector can tell when it connects: the tree, node detail, frame detail, GPU time, events. An app built by another Viso version, or a target whose runtime does not take that part yet, reports less than the command needs.",
        fix: "Rebuild and restart the app with the same Viso version as the CLI, or inspect a headless run with `--target headless`.",
        commands: ["viso inspect tree --target headless"],
    }
    INSPECT_ANSWER_INVALID = INTERNAL {
        summary: "The app's inspection did not arrive whole or did not decode.",
        explanation: "The census the app sent, or the table of its views, is not one this CLI reads: the CLI and the app speak different protocol versions, the tree outgrew a dev frame, or the run failed while writing it.",
        fix: "Rebuild and restart the app with the same Viso version as the CLI.",
        commands: ["viso run"],
    }
    PROFILE_APP_GONE = RUNTIME {
        summary: "The app closed, or crashed, while it was profiled.",
        explanation: "`viso profile` samples the frames of the app a dev session runs, or of a headless run of it; the app left before the capture ended, so there is no profile of the frames asked for.",
        fix: "Read the app's output in the `viso run` session, or the run's output the diagnostic carries, and profile again once it runs.",
        commands: ["viso run"],
    }
    PROFILE_WRITE = ENVIRONMENT {
        summary: "The profile's trace could not be written.",
        explanation: "`viso profile` writes its trace under `target/viso/traces/`, or where `--output` and `--chrome` name; the directory could not be created or the file written.",
        fix: "Check that the path is writable and the disk has room, or name another with `--output`.",
        commands: [],
    }
    PROFILE_ANSWER_INVALID = INTERNAL {
        summary: "The app's frames did not arrive whole or did not decode.",
        explanation: "The trace the session or the headless run handed over is not one this CLI reads: the CLI and the app speak different protocol versions, or the run failed while writing it.",
        fix: "Rebuild and restart the app with the same Viso version as the CLI.",
        commands: ["viso run"],
    }

    GAME_NOT_RUNNING = ENVIRONMENT {
        summary: "No mounted view of the named file runs a game.",
        explanation: "The game command addresses a running game by the file that declares it, and no view mounted from that file in the dev session runs one.",
        fix: "Start the app with `viso run` and open the view that runs the game.",
        commands: ["viso run"],
    }
    GAME_AMBIGUOUS = USAGE {
        summary: "The game's name matches more than one mounted view.",
        explanation: "Several mounted views run a game from files of that name.",
        fix: "Give the file's path relative to the project instead of its name.",
        commands: [],
    }
    GAME_UNKNOWN_STATE = DIAGNOSTICS {
        summary: "The game has no such system or state.",
        explanation: "The path names a system, or a state of a system, that the running game does not declare.",
        fix: "Use one of the names the diagnostic suggests.",
        commands: [],
    }
    GAME_BUSY = ENVIRONMENT {
        summary: "The game replays a tape, or is being recorded already.",
        explanation: "A game records or replays one tape at a time.",
        fix: "Wait for the replay or recording to end, then rerun.",
        commands: [],
    }
    GAME_NO_HISTORY = ENVIRONMENT {
        summary: "The game keeps no history to replay yet.",
        explanation: "A game a dev session runs keeps the last seconds of play from its first ticks; this one has kept none yet.",
        fix: "Let the game run a moment, then replay again.",
        commands: [],
    }
    GAME_ENDED = RUNTIME {
        summary: "The game ended before the recording did.",
        explanation: "The game's view unmounted, or the app exited, while the recording was still running; the tape is incomplete and was not written.",
        fix: "Keep the game running for the recording's length.",
        commands: [],
    }
    GAME_FAULT = RUNTIME {
        summary: "The World did not restart for the recording.",
        explanation: "Recording starts from a fresh World, and restarting it failed: a system faulted during its first frame.",
        fix: "Fix the fault the game's diagnostics report, then record again.",
        commands: [],
    }
    GAME_ANSWER_INVALID = INTERNAL {
        summary: "The running session's answer did not decode or outgrew a dev frame.",
        explanation: "The dev session answered with a frame the CLI could not decode, or one larger than the dev protocol allows; the CLI and the app speak different protocol versions or the session failed.",
        fix: "Rebuild and restart the app with the same Viso version as the CLI.",
        commands: ["viso run"],
    }
}

/// The code named `name`.
pub fn by_name(name: &str) -> Option<Code> {
    ALL.iter().copied().find(|code| code.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The prefix of a code names its failure class, so a reader knows the
    /// exit from the name alone.
    #[test]
    fn each_prefix_has_one_exit_class() {
        for code in ALL {
            let prefix = code.name.split('_').next().unwrap();
            let classes: &[u8] = match prefix {
                "CLI" => &[USAGE],
                "INTERNAL" => &[INTERNAL],
                "ENV" => &[ENVIRONMENT],
                "BUILD" => &[BUILD],
                "PACKAGE" => &[PACKAGE],
                "EXPORT" => &[DIAGNOSTICS, PACKAGE],
                "RUN" => &[RUNTIME],
                "TEST" | "EXPLAIN" | "WEB" => &[DIAGNOSTICS],
                "DUMP" => &[USAGE],
                // Device and game codes are about one subject and fail in
                // more than one way.
                "NEW" => &[USAGE, ENVIRONMENT, INTERNAL],
                "INSPECT" => &[USAGE, ENVIRONMENT, INTERNAL],
                "PROFILE" => &[ENVIRONMENT, RUNTIME, INTERNAL],
                "ANDROID" => &[USAGE, ENVIRONMENT],
                "IOS" => &[USAGE],
                "DEVICE" | "GAME" => &[DIAGNOSTICS, USAGE, ENVIRONMENT, RUNTIME, INTERNAL],
                other => panic!("`{}` has no known prefix `{other}`", code.name),
            };
            assert!(classes.contains(&code.exit), "{}", code.name);
        }
    }

    #[test]
    fn names_are_unique_and_found() {
        for (i, code) in ALL.iter().enumerate() {
            assert!(
                ALL[..i].iter().all(|other| other.name != code.name),
                "{} twice",
                code.name
            );
            assert_eq!(by_name(code.name), Some(*code));
            for text in [code.summary, code.explanation, code.fix] {
                assert!(text.ends_with('.'), "{}: {text}", code.name);
            }
            assert!(code.commands.iter().all(|c| c.starts_with("viso ")));
        }
        assert_eq!(by_name("NOPE"), None);
    }

    /// A code nothing raises would explain a failure that cannot happen.
    #[test]
    fn every_code_is_raised() {
        fn sources(dir: &std::path::Path, out: &mut String) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") && !path.ends_with("error.rs")
                {
                    out.push_str(&std::fs::read_to_string(&path).unwrap());
                }
            }
        }
        // The services raise them, and the command that runs them.
        let mut text = String::new();
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        sources(&here.join("src"), &mut text);
        sources(&here.join("../cli/src"), &mut text);
        let unraised: Vec<&str> = ALL
            .iter()
            .map(|code| code.name)
            .filter(|name| {
                !text.match_indices(name).any(|(i, _)| {
                    let after = text.as_bytes().get(i + name.len());
                    !after.is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
                })
            })
            .collect();
        assert!(unraised.is_empty(), "never raised: {unraised:?}");
    }
}
