//! `viso test [<domain>] [<scenario>]`: the project's tests, domain by
//! domain, each case reported as a `test` event and tallied in the summary.
//!
//! - `unit`: the Cargo package's own tests, through `cargo test`.
//! - `ui`: the scenarios under `tests/ui/`, each played by its own run of a
//!   headless build of the app.
//! - `game`: the scenarios under `tests/game/`, run headless on the package's
//!   systems.
//! - `web`: tests in a browser, which this build does not run yet.
//!
//! Without a domain every one runs; a domain with nothing to run here is
//! skipped with the reason. `--filter` (with `--exact`) narrows each domain's
//! cases by name; `--fail-fast` stops at the first failed case.

mod game;
mod live;
mod ui;
mod unit;

use std::path::Path;

use super::check;
use super::scenario::{TAPE, scenarios};
use crate::args::{TestArgs, TestDomain, TestTarget};
use crate::context::Context;
use crate::error::{CLI_USAGE, ENV_TEST_TARGET, SUCCESS, TEST_INPUT_INVALID};
use crate::output::{Output, Source, Test, TestStatus};

pub fn run(ctx: &Context<'_>, args: &TestArgs, out: &mut Output) -> u8 {
    let domain = args.domain.unwrap_or(TestDomain::All);
    if let Some(why) = misplaced(args, domain) {
        return out.failure(CLI_USAGE, &why, &[]);
    }
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    let every = domain == TestDomain::All;
    let domains = if every {
        vec![
            TestDomain::Unit,
            TestDomain::Ui,
            TestDomain::Game,
            TestDomain::Web,
        ]
    } else {
        vec![domain]
    };
    let run = Run {
        args,
        filter: Filter {
            pattern: args.filter.clone(),
            exact: args.exact,
        },
        every,
    };
    let mut code = SUCCESS;
    for domain in domains {
        let ended = match domain {
            TestDomain::Unit => unit::run(ctx, &checked, &run, out),
            TestDomain::Ui => ui::run(ctx, &checked, &run, out),
            TestDomain::Game => game::run(&checked, &run, out),
            TestDomain::Web => web(&run, out),
            TestDomain::All => unreachable!("every domain is run one by one"),
        };
        code = code.max(ended);
        if run.stopped(out) {
            break;
        }
    }
    out.tally(code == SUCCESS);
    code
}

/// What every domain runs under.
struct Run<'a> {
    args: &'a TestArgs,
    filter: Filter,
    /// Whether every domain runs, so one with nothing to run is skipped
    /// rather than failing the command.
    every: bool,
}

impl Run<'_> {
    /// Whether `--fail-fast` stops the run: a case failed.
    fn stopped(&self, out: &Output) -> bool {
        self.args.fail_fast && out.test_counts().fail > 0
    }

    /// Reports that `domain` has nothing to run, and why: a skipped case
    /// when every domain runs, else the failure under `code`.
    fn nothing(&self, out: &mut Output, domain: &str, code: crate::error::Code, why: &str) -> u8 {
        if self.every {
            skip(out, domain, why);
            SUCCESS
        } else {
            out.failure(code, why, &[])
        }
    }
}

/// The usage error of an option the domain does not take.
fn misplaced(args: &TestArgs, domain: TestDomain) -> Option<String> {
    let game = domain == TestDomain::Game;
    for (given, option) in [
        (args.frames.is_some(), "--frames"),
        (args.seed.is_some(), "--seed"),
        (args.tape.is_some(), "--tape"),
    ] {
        if given && !game {
            return Some(format!(
                "`{option}` is an option of game scenarios: `viso test game {option} …`"
            ));
        }
    }
    if args.update_snapshots && !matches!(domain, TestDomain::Ui | TestDomain::All) {
        return Some(
            "`--update-snapshots` rewrites the goldens of UI scenarios: `viso test ui --update-snapshots`"
                .into(),
        );
    }
    if args.scenario.is_some() && !matches!(domain, TestDomain::Ui | TestDomain::Game) {
        return Some(
            "a scenario is named in the `ui` or `game` domain: `viso test ui <name>`".into(),
        );
    }
    if args.device.is_some() && !matches!(args.target, Some(TestTarget::Ios | TestTarget::Android))
    {
        return Some("`--device` names the profile of `--target ios` or `--target android`".into());
    }
    if args.target.is_some() && matches!(domain, TestDomain::Unit | TestDomain::Game) {
        return Some(format!(
            "`--target` is where `ui` and `web` cases run; `{}` tests run on this machine",
            domain.as_str()
        ));
    }
    None
}

/// The `web` domain: browser tests, which this build does not run yet.
fn web(run: &Run<'_>, out: &mut Output) -> u8 {
    if let Some(target) = run.args.target
        && !matches!(
            target,
            TestTarget::WebGpu | TestTarget::WebDom | TestTarget::WebHybrid
        )
    {
        if run.every {
            return SUCCESS;
        }
        return out.failure(
            CLI_USAGE,
            "web tests run in a browser: `--target web-gpu|web-dom|web-hybrid`",
            &[],
        );
    }
    run.nothing(
        out,
        "web",
        ENV_TEST_TARGET,
        "tests in a browser are not run by this build of viso",
    )
}

/// The cases a filter keeps, by name.
struct Filter {
    pattern: Option<String>,
    exact: bool,
}

impl Filter {
    fn keeps(&self, name: &str) -> bool {
        match &self.pattern {
            None => true,
            Some(pattern) if self.exact => name == pattern,
            Some(pattern) => name.contains(pattern.as_str()),
        }
    }
}

/// Reports `domain` skipped as a whole, and why.
fn skip(out: &mut Output, domain: &str, why: &str) {
    report(out, domain, domain, TestStatus::Skip, 0, Some(why), |_| {});
}

/// Reports one case: its `test` event, or its line of human output.
pub(super) fn report(
    out: &mut Output,
    domain: &str,
    name: &str,
    status: TestStatus,
    duration_ms: u64,
    message: Option<&str>,
    extra: impl FnOnce(&mut viso_ende::JsonWriter),
) {
    let test = Test {
        name,
        domain,
        status,
        duration_ms,
        message,
    };
    out.test(&test, extra, || {
        let word = status.word();
        match (status, message) {
            (TestStatus::Skip, Some(why)) if name == domain => {
                format!("test {domain} ... {word} ({why})\n")
            }
            _ => format!("test {domain} {name} ... {word}\n"),
        }
    });
}

/// Reports a scenario that does not parse or does not fit the build, at `at`
/// in `source` when known.
pub(super) fn invalid(
    out: &mut Output,
    source: Option<&Source<'_>>,
    at: Option<viso_dsl::TextRange>,
    message: &str,
) {
    match (source, at) {
        (Some(source), Some(at)) => out.source(
            Some(source),
            &[],
            &viso_dsl::Diagnostic::error(TEST_INPUT_INVALID.name, at, message),
        ),
        _ => {
            out.failure(TEST_INPUT_INVALID, message, &[]);
        }
    }
}

/// `path` relative to `root`, when it lies under it.
pub(super) fn relative<'a>(root: &Path, path: &'a Path) -> &'a Path {
    path.strip_prefix(root).unwrap_or(path)
}

/// The byte range of 1-based line `line` of `text`, without its terminator.
pub(super) fn line_range(text: &str, line: u32) -> viso_dsl::TextRange {
    use viso_dsl::{TextRange, TextSize};
    let mut start = 0;
    for (n, content) in text.split('\n').enumerate() {
        if n + 1 == line as usize {
            let content = content.strip_suffix('\r').unwrap_or(content);
            return TextRange::new(
                TextSize::new(start as u32),
                TextSize::new((start + content.len()) as u32),
            );
        }
        start += content.len() + 1;
    }
    TextRange::empty(TextSize::new(text.len() as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_keeps_names_containing_or_equal_to_its_pattern() {
        let filter = |pattern: Option<&str>, exact| Filter {
            pattern: pattern.map(str::to_owned),
            exact,
        };
        assert!(filter(None, false).keeps("anything"));
        assert!(filter(Some("count"), false).keeps("counter_increments"));
        assert!(!filter(Some("count"), true).keeps("counter_increments"));
        assert!(filter(Some("counter"), true).keeps("counter"));
    }

    #[test]
    fn a_line_range_spans_the_line() {
        let text = "a\r\nbc\nd";
        let range = line_range(text, 2);
        assert_eq!(&text[range.as_usize()], "bc");
        assert_eq!(line_range(text, 9).start(), viso_dsl::TextSize::new(7));
    }
}
