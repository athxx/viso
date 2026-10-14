//! Exit codes, and every diagnostic code the CLI raises itself, each bound to
//! the exit code a failure under it ends the command with.
//!
//! A code is declared once here, so the exit a failure takes cannot drift from
//! the code it reports: [`Output::failure`](crate::output::Output::failure)
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
#[allow(
    dead_code,
    reason = "the exit of the delivery commands, which are still to come"
)]
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
}

macro_rules! codes {
    ($($(#[$doc:meta])* $name:ident = $exit:ident, $summary:literal;)*) => {
        $(
            $(#[$doc])*
            pub const $name: Code = Code {
                name: stringify!($name),
                exit: $exit,
                summary: $summary,
            };
        )*
        /// Every code, in declaration order.
        pub const ALL: &[Code] = &[$($name),*];
    };
}

codes! {
    CLI_USAGE = USAGE, "The command line did not parse or asks for something the command does not do.";
    INTERNAL_PANIC = INTERNAL, "The CLI failed on its own; this is a bug in Viso.";

    ENV_CURRENT_DIR = ENVIRONMENT, "The current directory could not be read.";
    ENV_STDOUT = ENVIRONMENT, "Standard output could not be written.";
    ENV_SOURCE_UNREADABLE = ENVIRONMENT, "A source file the command needs could not be read.";
    ENV_CARGO_MANIFEST = ENVIRONMENT, "The project root has no `Cargo.toml` to build.";
    ENV_CARGO = ENVIRONMENT, "Cargo could not be run.";
    ENV_NO_EXECUTABLE = ENVIRONMENT, "The build produced no executable, or more than one.";
    ENV_DEV_CHANNEL = ENVIRONMENT, "The dev channel could not be opened on the loopback interface.";
    ENV_DEVICE = ENVIRONMENT, "The simulator or emulator toolchain is missing or failed.";
    ENV_NO_DEV_SESSION = ENVIRONMENT, "No `viso run` session is running for the project, or it cannot be reached.";
    ENV_TAPE_WRITE = ENVIRONMENT, "A recorded tape could not be written.";
    ENV_TEST_INPUT = ENVIRONMENT, "A test scenario or tape could not be found or read.";
    ENV_LOCK_HELD = ENVIRONMENT, "Another viso process holds a lock the command needs free.";
    ENV_REMOVE = ENVIRONMENT, "A file or directory the command removes could not be removed.";

    BUILD_FAILED = BUILD, "`cargo build` failed.";
    RUN_APP_FAILED = RUNTIME, "The app could not be launched, crashed, or exited with a failure.";

    TEST_INPUT_INVALID = DIAGNOSTICS, "A test scenario or tape does not parse or fit the build.";
    TEST_EXPECTATION = DIAGNOSTICS, "A game test's expectation was not met.";

    DEVICE_MISSING = ENVIRONMENT, "No simulator or emulator profile exists.";
    DEVICE_UNKNOWN = USAGE, "`--device` or a configured default names no profile.";
    DEVICE_AMBIGUOUS = USAGE, "Several profiles exist and none is the default.";
    DEVICE_ID_INVALID = USAGE, "A profile ID that is not one.";
    DEVICE_EXISTS = USAGE, "A profile of that ID exists already.";
    DEVICE_SYSTEM = ENVIRONMENT, "No installed system image or runtime fits the profile.";

    GAME_NOT_RUNNING = ENVIRONMENT, "No mounted view of the named file runs a game.";
    GAME_AMBIGUOUS = USAGE, "The game's name matches more than one mounted view.";
    GAME_UNKNOWN_STATE = DIAGNOSTICS, "The game has no such system or state.";
    GAME_BUSY = ENVIRONMENT, "The game replays a tape, or is being recorded already.";
    GAME_ENDED = RUNTIME, "The game ended before the recording did.";
    GAME_FAULT = RUNTIME, "The World did not restart for the recording.";
    GAME_ANSWER_INVALID = INTERNAL, "The running session's answer did not decode or outgrew a dev frame.";
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
                "RUN" => &[RUNTIME],
                "TEST" => &[DIAGNOSTICS],
                // Device and game codes are about one subject and fail in
                // more than one way.
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
            assert!(code.summary.ends_with('.'), "{}", code.name);
        }
        assert_eq!(by_name("NOPE"), None);
    }
}
