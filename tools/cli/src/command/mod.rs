//! Command handlers. Each resolves what it needs, calls the owning service and
//! returns the process exit code.

mod build;
mod check;
mod clean;
mod completion;
mod config;
mod device;
mod doctor;
mod dump;
mod explain;
mod fmt;
mod game;
mod lsp;
mod new;
mod run;
mod schema;
mod serve;
mod test;

use viso_project::Platform;

use crate::args::{AndroidCommand, Cli, Command, Global, IosCommand};
use crate::context::Context;
use crate::error::{CLI_USAGE, INTERNAL_PANIC, SUCCESS};
use crate::output::Output;

/// Runs the parsed command, ends its output with the summary, and returns its exit
/// code. A panic in the command is reported as the CLI's own failure, so the
/// event stream still ends with its summary.
pub fn run(cli: &Cli) -> u8 {
    let ctx = Context::new(&cli.global);
    let mut out = Output::new(&cli.global, Some(cli.command.name()));
    let ran = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        dispatch(&ctx, &cli.command, &mut out)
    }));
    let code = match ran {
        Ok(code) => code,
        Err(panic) => out.failure(
            INTERNAL_PANIC,
            &format!("the CLI failed: {}", panic_message(&*panic)),
            &["this is a bug in Viso; please report it with the command line".to_owned()],
        ),
    };
    out.finish(code)
}

fn dispatch(ctx: &Context<'_>, command: &Command, out: &mut Output) -> u8 {
    match command {
        Command::Android(AndroidCommand::Emulator(command)) => {
            device::run(Platform::Android, command, out)
        }
        Command::Build(args) => build::run(ctx, args, out),
        Command::Check(args) => check::run(ctx, args, out),
        Command::Clean => clean::run(ctx, out),
        Command::Doctor(args) => doctor::run(ctx, args, out),
        Command::Completion(args) => completion::run(ctx, args, out),
        Command::Dump(args) => dump::run(ctx, args, out),
        Command::Explain(args) => explain::run(args, out),
        Command::Config(command) => config::run(ctx, command, out),
        Command::Fmt(args) => fmt::run(ctx, args, out),
        Command::Ios(IosCommand::Simulator(command)) => device::run(Platform::Ios, command, out),
        Command::Game(command) => game::run(ctx, command, out),
        Command::Lsp(args) => lsp::run(ctx, args, out),
        Command::New(args) => new::run(ctx, args, out),
        Command::Run(args) => run::run(ctx, args, out),
        Command::Schema(args) => schema::run(ctx, args, out),
        Command::Serve(args) => serve::run(ctx, args, out),
        Command::Test(args) => test::run(ctx, args, out),
    }
}

/// Reports a usage error for `command`, when the command line names one, as
/// JSON events or human text, with the commands to run next, and returns its
/// exit code.
pub fn usage_failure(
    json: bool,
    command: Option<&str>,
    message: &str,
    notes: &[String],
    next: &[String],
) -> u8 {
    let mut out = Output::new(&bare(json), command);
    let code = out.failure_next(CLI_USAGE, message, notes, next);
    out.finish(code)
}

/// Answers `--version`: its text, or a `result` under `--json`.
pub fn version(json: bool) -> u8 {
    let mut out = Output::new(&bare(json), None);
    let version = &crate::help::VERSION;
    out.result(|w| version.write(w), || version.text());
    out.finish(SUCCESS)
}

/// The global options of a command line that did not parse: only `--json`
/// is known.
fn bare(json: bool) -> Global {
    Global {
        json,
        ..Global::default()
    }
}

/// What a panic said.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("a panic with no message")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_reports_what_it_said() {
        let caught = |f: fn()| std::panic::catch_unwind(f).unwrap_err();
        let hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));
        let literal = caught(|| panic!("index out of range"));
        let formatted = caught(|| panic!("{} out of range", 7));
        let other = caught(|| std::panic::panic_any(7u8));
        std::panic::set_hook(hook);
        assert_eq!(panic_message(&*literal), "index out of range");
        assert_eq!(panic_message(&*formatted), "7 out of range");
        assert_eq!(panic_message(&*other), "a panic with no message");
    }
}
