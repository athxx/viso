//! `viso completion <shell>`: the script that completes `viso` in a shell,
//! written to stdout. It reads no project and has no JSON form: what it writes
//! is the script.

use crate::args::CompletionArgs;
use crate::complete;
use crate::context::Context;
use crate::error::{CLI_USAGE, ENV_STDOUT, SUCCESS};
use crate::output::Output;

pub fn run(ctx: &Context<'_>, args: &CompletionArgs, out: &mut Output) -> u8 {
    if ctx.global.json {
        return out.failure(
            CLI_USAGE,
            "`viso completion` writes a shell script, which has no JSON form",
            &["drop `--json`".to_owned()],
        );
    }
    let mut stdout = std::io::stdout().lock();
    match complete::register(args.shell, &mut stdout) {
        Ok(()) => SUCCESS,
        Err(error) => out.failure(
            ENV_STDOUT,
            &format!("cannot write the script: {error}"),
            &[],
        ),
    }
}
