//! `viso game` (`Viso_CLI.md` section 22.3): ask the game the running
//! `viso run` session's app plays. The session is found by its lock, which
//! names its control endpoint; the game by its `.vs` file among the views the
//! app mounts.
//!
//! `peek` prints a state, written as JSON by its type. `record` restarts the
//! game's World, so the tape replays from tick 0 on a game started with its
//! seed, and writes the tape of the next stretch of play: its text form,
//! which `viso test game --tape` replays and a scenario can extend with
//! expectations, or its binary form when the run took live patches, which
//! only the binary form carries.

use std::fs;
use std::time::Duration;

use viso_dsl::scenario::read_tape;
use viso_project::{ArtifactKind, Layout, LockGuard, LockKind, Overrides, Profile, Target};
use viso_view::dev::wire::{GameAsk, MAX_RECORD_TICKS, RecordSpan};

use super::check::{config_in_its_file, failure_code};
use super::{CLI_USAGE, DIAGNOSTICS, ENV_CURRENT_DIR, ENVIRONMENT, RUNTIME, SUCCESS, USAGE};
use crate::args::{GameCommand, Global, PeekArgs, RecordArgs};
use crate::dev::ControlReply;
use crate::dev::control::{
    self, ENV_NO_DEV_SESSION, GAME_AMBIGUOUS, GAME_ENDED, GAME_FAULT, GAME_UNKNOWN_STATE,
};
use crate::output::Output;

/// How long a peek waits for the app: it answers at its next frame.
const PEEK_WAIT: Duration = Duration::from_secs(10);

/// A tape could not be written.
pub const ENV_TAPE_WRITE: &str = "ENV_TAPE_WRITE";

pub fn run(global: &Global, command: &GameCommand, out: &mut Output) -> u8 {
    match command {
        GameCommand::Peek(args) => peek(global, args, out),
        GameCommand::Record(args) => record(global, args, out),
    }
}

/// The control endpoint the project's `viso run` session named in its lock.
fn endpoint(global: &Global, out: &mut Output) -> Result<String, u8> {
    let cwd = std::env::current_dir().map_err(|error| {
        out.failure(
            ENV_CURRENT_DIR,
            &format!("cannot read the current directory: {error}"),
            &[],
        );
        ENVIRONMENT
    })?;
    let flags = Overrides {
        target: Some(Target::Host),
        profile: Some(Profile::Dev),
        ..Overrides::default()
    };
    let (project, _) =
        viso_project::load(global.project.as_deref(), &cwd, ArtifactKind::Build, &flags).map_err(
            |diagnostics| {
                for diagnostic in &diagnostics {
                    config_in_its_file(out, diagnostic);
                }
                failure_code(&diagnostics)
            },
        )?;
    let kind = LockKind::DevSession {
        target: Target::Host,
        device: None,
    };
    match LockGuard::holder(&Layout::new(&project.root), kind) {
        Some(holder) => holder.endpoint.clone().ok_or_else(|| {
            out.failure(
                ENV_NO_DEV_SESSION,
                "the `viso run` session of this project takes no `viso game` requests",
                &[format!(
                    "it is starting, or failed to open its control endpoint ({holder})"
                )],
            );
            ENVIRONMENT
        }),
        None => {
            out.failure(
                ENV_NO_DEV_SESSION,
                "no `viso run` session is running for this project",
                &["`viso game` asks the app a `viso run` session runs: start one".to_owned()],
            );
            Err(ENVIRONMENT)
        }
    }
}

fn peek(global: &Global, args: &PeekArgs, out: &mut Output) -> u8 {
    let Some((system, state)) = args
        .state
        .split_once('.')
        .filter(|(system, state)| !system.is_empty() && !state.is_empty())
    else {
        out.failure(
            CLI_USAGE,
            &format!("`{}` is not `System.state`", args.state),
            &["name the state as `Player.score`".to_owned()],
        );
        return USAGE;
    };
    let endpoint = match endpoint(global, out) {
        Ok(endpoint) => endpoint,
        Err(code) => return code,
    };
    let ask = GameAsk::Peek {
        system: system.to_owned(),
        state: state.to_owned(),
    };
    match control::call(&endpoint, &args.game, ask, Some(PEEK_WAIT)) {
        Ok(ControlReply::State {
            file,
            json,
            tick,
            sessions,
        }) => {
            out.result(
                |w| {
                    w.begin_object();
                    w.name("game");
                    w.string(&file);
                    w.name("system");
                    w.string(system);
                    w.name("state");
                    w.string(state);
                    w.name("value");
                    w.raw(&json);
                    w.name("tick");
                    w.uint(tick);
                    w.name("sessions");
                    w.uint(u64::from(sessions));
                    w.end_object();
                },
                || {
                    let of = if sessions > 1 {
                        format!(", the first of {sessions} sessions")
                    } else {
                        String::new()
                    };
                    format!("{system}.{state} = {json} ({file} at tick {tick}{of})\n")
                },
            );
            SUCCESS
        }
        Ok(reply) => refused(reply, out),
        Err(message) => {
            out.failure(ENV_NO_DEV_SESSION, &message, &[]);
            ENVIRONMENT
        }
    }
}

fn record(global: &Global, args: &RecordArgs, out: &mut Output) -> u8 {
    let span = match args.ticks {
        Some(ticks) if (1..=MAX_RECORD_TICKS).contains(&ticks) => RecordSpan::Ticks(ticks),
        None if args.seconds.is_finite() && args.seconds > 0.0 => {
            RecordSpan::Millis((args.seconds * 1000.0).ceil() as u64)
        }
        _ => {
            out.failure(
                CLI_USAGE,
                "a recording runs at least one tick",
                &[format!(
                    "`--ticks` is 1 to {MAX_RECORD_TICKS}, `--seconds` above 0"
                )],
            );
            return USAGE;
        }
    };
    let endpoint = match endpoint(global, out) {
        Ok(endpoint) => endpoint,
        Err(code) => return code,
    };
    let length = match span {
        RecordSpan::Ticks(ticks) => format!("{ticks} ticks"),
        RecordSpan::Millis(_) => format!("{} s", args.seconds),
    };
    out.log(
        "info",
        "tool",
        &format!(
            "recording {length} of `{}` from a restarted World",
            args.game
        ),
    );
    let reply = control::call(&endpoint, &args.game, GameAsk::Record { span }, None);
    let (file, bytes) = match reply {
        Ok(ControlReply::Tape { file, tape }) => (file, tape),
        Ok(reply) => return refused(reply, out),
        Err(message) => {
            out.failure(ENV_NO_DEV_SESSION, &message, &[]);
            return ENVIRONMENT;
        }
    };
    let tape = match read_tape(&bytes, 0, 0) {
        Ok(tape) => tape,
        Err(error) => {
            out.failure(
                control::GAME_ANSWER_INVALID,
                &format!("the app sent a malformed tape: {error}"),
                &[],
            );
            return RUNTIME;
        }
    };
    // The text form carries no patch.
    let binary = !tape.patches.is_empty();
    let written = if binary {
        bytes
    } else {
        tape.to_text().into_bytes()
    };
    let path = &args.output;
    let wrote = path
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(path, &written));
    if let Err(error) = wrote {
        out.failure(
            ENV_TAPE_WRITE,
            &format!("cannot write `{}`: {error}", path.display()),
            &[],
        );
        return ENVIRONMENT;
    }
    let form = if binary { "binary" } else { "text" };
    out.result(
        |w| {
            w.begin_object();
            w.name("game");
            w.string(&file);
            w.name("path");
            w.string(&path.display().to_string());
            w.name("form");
            w.string(form);
            w.name("ticks");
            w.uint(tape.ticks);
            w.name("seed");
            w.uint(tape.seed);
            w.name("patches");
            w.uint(tape.patches.len() as u64);
            w.end_object();
        },
        || {
            let patches = match tape.patches.len() {
                0 => String::new(),
                1 => ", 1 live patch".to_owned(),
                n => format!(", {n} live patches"),
            };
            format!(
                "recorded {} ticks of {file} to {} ({form}{patches})\n",
                tape.ticks,
                path.display()
            )
        },
    );
    SUCCESS
}

/// Reports a refusal and returns its exit code.
fn refused(reply: ControlReply, out: &mut Output) -> u8 {
    let ControlReply::Refused {
        code,
        message,
        notes,
    } = reply
    else {
        out.failure(
            control::GAME_ANSWER_INVALID,
            "`viso run` answered another question",
            &[],
        );
        return RUNTIME;
    };
    out.failure(&code, &message, &notes);
    match &*code {
        GAME_UNKNOWN_STATE => DIAGNOSTICS,
        GAME_AMBIGUOUS => USAGE,
        GAME_FAULT | GAME_ENDED => RUNTIME,
        _ => ENVIRONMENT,
    }
}
