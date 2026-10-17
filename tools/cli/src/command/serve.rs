//! `viso serve`: the session `viso run` holds with a web target, its page
//! served where `--host`, `--lan` and `--port` ask, and opened only with
//! `--open`.

use viso_project::Target;

use super::run::session;
use crate::args::{RunArgs, RunTarget, ServeArgs, WebTarget};
use crate::context::Context;
use crate::deploy::web;
use crate::error::{CLI_USAGE, ENV_DEV_CHANNEL};
use crate::output::Output;

pub fn run(ctx: &Context<'_>, args: &ServeArgs, out: &mut Output) -> u8 {
    let listen = match web::Listen::new(args.host, args.lan, args.port) {
        Ok(listen) => listen,
        Err(web::ListenError::Usage(why)) => return out.failure(CLI_USAGE, &why, &[]),
        Err(web::ListenError::Environment(why)) => return out.failure(ENV_DEV_CHANNEL, &why, &[]),
    };
    let target = match args.target {
        Some(target) => target,
        None => match default_web_target(ctx, out) {
            Ok(target) => target,
            Err(code) => return code,
        },
    };
    let run = RunArgs {
        target: Some(match target {
            WebTarget::Gpu => RunTarget::WebGpu,
            WebTarget::Dom => RunTarget::WebDom,
            WebTarget::Hybrid => RunTarget::WebHybrid,
        }),
        device: None,
        browser: None,
        no_hot_reload: args.no_hot_reload,
        profile_frame: false,
        inspect: false,
        open: false,
        env: Vec::new(),
        cwd: None,
        app_args: Vec::new(),
    };
    session(ctx, &run, listen, Some(args.open), out)
}

/// The project's `[web] default_target`, for a `viso serve` that names none.
fn default_web_target(ctx: &Context<'_>, out: &mut Output) -> Result<WebTarget, u8> {
    let project = ctx.project(out)?;
    match project.manifest.web.default_target.map(|t| t.value) {
        Some(Target::WebGpu) => Ok(WebTarget::Gpu),
        Some(Target::WebDom) => Ok(WebTarget::Dom),
        Some(Target::WebHybrid) => Ok(WebTarget::Hybrid),
        _ => Err(out.failure_next(
            CLI_USAGE,
            "`viso serve` serves a web target: name one, or set `[web] default_target`",
            &[],
            &["viso serve web-gpu".to_owned()],
        )),
    }
}
