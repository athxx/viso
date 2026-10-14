//! `viso game`: ask the game the running
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
use viso_project::{LockGuard, LockKind, Target};
use viso_view::dev::wire::{GameAsk, MAX_RECORD_TICKS, RecordSpan};

use crate::args::{GameCommand, PeekArgs, RecordArgs};
use crate::context::Context;
use crate::dev::ControlReply;
use crate::dev::control;
use crate::error::{
    self, CLI_USAGE, ENV_NO_DEV_SESSION, ENV_TAPE_WRITE, GAME_ANSWER_INVALID, SUCCESS,
};
use crate::output::Output;

/// How long a peek waits for the app: it answers at its next frame.
const PEEK_WAIT: Duration = Duration::from_secs(10);

pub fn run(ctx: &Context<'_>, command: &GameCommand, out: &mut Output) -> u8 {
    match command {
        GameCommand::Peek(args) => peek(ctx, args, out),
        GameCommand::Record(args) => record(ctx, args, out),
    }
}

/// The control endpoint the project's `viso run` session named in its lock.
fn endpoint(ctx: &Context<'_>, out: &mut Output) -> Result<String, u8> {
    let project = ctx.project(out)?;
    let kind = LockKind::DevSession {
        target: Target::Host,
        device: None,
    };
    match LockGuard::holder(&ctx.layout(&project.root), kind) {
        Some(holder) => holder.endpoint.clone().ok_or_else(|| {
            out.failure(
                ENV_NO_DEV_SESSION,
                "the `viso run` session of this project takes no `viso game` requests",
                &[format!(
                    "it is starting, or failed to open its control endpoint ({holder})"
                )],
            )
        }),
        None => Err(out.failure(
            ENV_NO_DEV_SESSION,
            "no `viso run` session is running for this project",
            &["`viso game` asks the app a `viso run` session runs: start one".to_owned()],
        )),
    }
}

fn peek(ctx: &Context<'_>, args: &PeekArgs, out: &mut Output) -> u8 {
    let Some((system, state)) = args
        .state
        .split_once('.')
        .filter(|(system, state)| !system.is_empty() && !state.is_empty())
    else {
        return out.failure(
            CLI_USAGE,
            &format!("`{}` is not `System.state`", args.state),
            &["name the state as `Player.score`".to_owned()],
        );
    };
    let endpoint = match endpoint(ctx, out) {
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
        Err(message) => out.failure(ENV_NO_DEV_SESSION, &message, &[]),
    }
}

fn record(ctx: &Context<'_>, args: &RecordArgs, out: &mut Output) -> u8 {
    let span = match args.ticks {
        Some(ticks) if (1..=MAX_RECORD_TICKS).contains(&ticks) => RecordSpan::Ticks(ticks),
        None if args.seconds.is_finite() && args.seconds > 0.0 => {
            RecordSpan::Millis((args.seconds * 1000.0).ceil() as u64)
        }
        _ => {
            return out.failure(
                CLI_USAGE,
                "a recording runs at least one tick",
                &[format!(
                    "`--ticks` is 1 to {MAX_RECORD_TICKS}, `--seconds` above 0"
                )],
            );
        }
    };
    let endpoint = match endpoint(ctx, out) {
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
            return out.failure(ENV_NO_DEV_SESSION, &message, &[]);
        }
    };
    let tape = match read_tape(&bytes, 0, 0) {
        Ok(tape) => tape,
        Err(error) => {
            return out.failure(
                GAME_ANSWER_INVALID,
                &format!("the app sent a malformed tape: {error}"),
                &[],
            );
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
        return out.failure(
            ENV_TAPE_WRITE,
            &format!("cannot write `{}`: {error}", path.display()),
            &[],
        );
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
        return out.failure(
            GAME_ANSWER_INVALID,
            "`viso run` answered another question",
            &[],
        );
    };
    match error::by_name(&code) {
        Some(code) => out.failure(code, &message, &notes),
        None => out.failure(
            GAME_ANSWER_INVALID,
            &format!("`viso run` refused with an unknown code `{code}`: {message}"),
            &notes,
        ),
    }
}
