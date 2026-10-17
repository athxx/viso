//! Human-readable output: diagnostics to stderr, the summary to stdout.
//!
//! A diagnostic prints its header, its location and the source line under a caret
//! underline, then its related spans, notes and fix titles. A related span in
//! another file follows a `:::` line naming it:
//!
//! ```text
//! error[E2001]: imported name is not exported: `lib::Point`
//!  --> src/app.vs:1:15
//!   |
//! 1 | import lib::{ Point };
//!   |               ^^^^^
//!   |
//!  ::: src/lib.vs:1:8
//!   |
//! 1 | record Point { x: I64; }
//!   |        ----- declared here without `export`
//!   = help: export `Point` from `lib`
//! ```

use std::fmt::{self, Write as _};
use std::io::Write as _;
use std::ops::Range;

use crate::dev::report::{Outcome, RebuildEvent, ReloadEvent};

use super::{Location, Report, Source};

/// Writes `report` to stderr, followed by a blank line, in color when `color`.
pub(super) fn print(report: &Report<'_>, color: bool) {
    let mut text = render_with(report, Paint(color));
    text.push('\n');
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// The line a hot reload attempt of `file` prints.
pub(super) fn dev(file: &str, event: &ReloadEvent) -> String {
    let ms = event.elapsed_us as f64 / 1000.0;
    match event.outcome {
        Outcome::Rejected => {
            let codes = event.codes().join(",");
            format!(
                "✗ {file} candidate       kept r{}   {codes}",
                event.last_good_revision
            )
        }
        Outcome::Held => {
            format!("… {file} candidate       compiled, not applied (--no-hot-reload)")
        }
        outcome => {
            let mut line = format!(
                "✓ {file} patch r{} -> r{}   {ms:.0} ms",
                event.base_revision, event.candidate_revision
            );
            let dirty = dirty_summary(&event.counts.dirty);
            if !dirty.is_empty() {
                line.push_str("   ");
                line.push_str(&dirty);
            }
            if let Some(game) = &event.game {
                line.push_str(&format!(
                    "   game {} from tick {}",
                    game.tier.as_str(),
                    game.tick
                ));
                let plural = |n: u32| if n == 1 { "" } else { "s" };
                if game.reset > 0 {
                    let n = game.reset;
                    line.push_str(&format!(", {n} state{} reset", plural(n)));
                }
                if game.characters > 0 {
                    let n = game.characters;
                    line.push_str(&format!(", {n} character{} kept", plural(n)));
                }
            }
            if outcome == Outcome::ScopedReset {
                line.push_str("   (scoped reset)");
            }
            line
        }
    }
}

/// The line a build of the Rust sources prints.
pub(super) fn rebuild(event: &RebuildEvent) -> String {
    let files = match event.files.as_slice() {
        [] => "rust".to_owned(),
        [file] => file.clone(),
        [file, rest @ ..] => format!("{file} (+{})", rest.len()),
    };
    let s = event.build_us as f64 / 1e6;
    match event.outcome {
        Outcome::Rejected => format!(
            "✗ {files} not restarted   kept r{}   {}",
            event.last_good_revision,
            event.codes.join(",")
        ),
        Outcome::Held => format!(
            "… {files} built in {s:.1} s, not restarted (--no-hot-reload); restart `viso run` to run it"
        ),
        _ => {
            let restart = event
                .restart_us
                .map(|us| format!(", restarted in {:.2} s", us as f64 / 1e6))
                .unwrap_or_default();
            let state = if event.restored {
                ""
            } else {
                ", state not restored"
            };
            format!("✓ {files} warm restart{state}   built in {s:.1} s{restart}")
        }
    }
}

/// The dirty classes a commit left behind, as `"1 node paint-dirty, 3 nodes
/// layout-dirty"`; empty when the commit marked no node (a text request's
/// reshape, not a dirty mark).
fn dirty_summary(dirty: &viso_view::dev::wire::DirtyCounts) -> String {
    dirty
        .nonzero()
        .map(|(class, count)| {
            let plural = if count == 1 { "" } else { "s" };
            format!("{count} node{plural} {class}-dirty")
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// ANSI styling, or none.
#[derive(Clone, Copy)]
struct Paint(bool);

impl Paint {
    /// `text` in the SGR style `sgr`.
    fn style(self, sgr: &str, text: &str) -> String {
        if self.0 {
            format!("\x1b[{sgr}m{text}\x1b[0m")
        } else {
            text.to_owned()
        }
    }

    /// The color of a severity, and of its primary underline.
    fn severity(severity: &str) -> &'static str {
        match severity {
            "error" => "1;31",
            "warning" => "1;33",
            _ => "1;36",
        }
    }

    /// The gutter, arrows and `=`.
    fn frame(self, text: &str) -> String {
        self.style("1;34", text)
    }
}

#[cfg(test)]
fn render(report: &Report<'_>) -> String {
    render_with(report, Paint(false))
}

fn render_with(report: &Report<'_>, paint: Paint) -> String {
    let mut out = String::new();
    // Writing to a `String` cannot fail.
    let _ = write(&mut out, report, paint);
    out
}

fn write(out: &mut String, report: &Report<'_>, paint: Paint) -> fmt::Result {
    let severity = Paint::severity(report.severity);
    writeln!(
        out,
        "{}{}",
        paint.style(severity, &format!("{}[{}]", report.severity, report.code)),
        paint.style("1", &format!(": {}", report.message))
    )?;
    let mut gutter = 0;
    match &report.location {
        Location::Span(source, range) => {
            let last = std::iter::once(source.position(range.start).0)
                .chain(
                    report
                        .related
                        .iter()
                        .map(|r| r.source.position(r.range.start).0),
                )
                .max()
                .unwrap_or(1);
            gutter = last.to_string().len();
            let (line, column) = source.position(range.start);
            let bar = paint.frame("|");
            writeln!(
                out,
                "{:gutter$}{} {}:{line}:{column}",
                "",
                paint.frame("-->"),
                source.name
            )?;
            writeln!(out, "{:gutter$} {bar}", "")?;
            let primary = Mark {
                char: '^',
                sgr: severity,
            };
            snippet(out, source, range, primary, "", gutter, paint)?;
            let mut shown: &Source<'_> = source;
            for related in &report.related {
                if !std::ptr::eq(related.source, shown) {
                    shown = related.source;
                    let (line, column) = shown.position(related.range.start);
                    writeln!(out, "{:gutter$} {bar}", "")?;
                    writeln!(
                        out,
                        "{:gutter$}{} {}:{line}:{column}",
                        "",
                        paint.frame(":::"),
                        shown.name
                    )?;
                    writeln!(out, "{:gutter$} {bar}", "")?;
                }
                let secondary = Mark {
                    char: '-',
                    sgr: "1;34",
                };
                snippet(
                    out,
                    shown,
                    &related.range,
                    secondary,
                    related.label,
                    gutter,
                    paint,
                )?;
            }
        }
        Location::Point { file, line, column } => {
            writeln!(out, " {} {file}:{line}:{column}", paint.frame("-->"))?
        }
        Location::None => {}
    }
    let equals = paint.frame("=");
    for note in report.notes {
        writeln!(
            out,
            "{:gutter$} {equals} {}: {note}",
            "",
            paint.style("1", "note")
        )?;
    }
    for fix in &report.fixes {
        writeln!(
            out,
            "{:gutter$} {equals} {}: {}",
            "",
            paint.style("1", "help"),
            fix.title
        )?;
    }
    Ok(())
}

/// How a snippet underlines its range: the character, and its color.
#[derive(Clone, Copy)]
struct Mark {
    char: char,
    sgr: &'static str,
}

/// One source line with `range` underlined by `mark`, and an optional label after
/// the underline. A range running past the line is underlined to the line's end.
fn snippet(
    out: &mut String,
    source: &Source<'_>,
    range: &Range<usize>,
    mark: Mark,
    label: &str,
    gutter: usize,
    paint: Paint,
) -> fmt::Result {
    let start = source.clamp(range.start);
    let (text, line_start) = source.line_at(start);
    let (line, _) = source.position(start);
    let line_end = line_start + text.len();
    let end = range.end.clamp(start, line_end);
    // Tabs are copied into the padding so the underline lines up however the
    // terminal expands them.
    let pad: String = source.text[line_start..start.min(line_end)]
        .chars()
        .map(|c| if c == '\t' { '\t' } else { ' ' })
        .collect();
    let width = source.text[start.min(line_end)..end].chars().count().max(1);
    let underline: String = std::iter::repeat_n(mark.char, width).collect();
    let bar = paint.frame("|");
    let number = paint.frame(&format!("{line:>gutter$}"));
    writeln!(out, "{number} {bar} {text}")?;
    let label = if label.is_empty() {
        String::new()
    } else {
        format!(" {label}")
    };
    let underline = paint.style(mark.sgr, &format!("{underline}{label}"));
    writeln!(out, "{:gutter$} {bar} {pad}{underline}", "")
}

/// A byte count in binary units: `612 B`, `18 KiB`, `1.82 MiB`; three
/// significant digits from a kibibyte up.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    let decimals = if value >= 100.0 {
        0
    } else if value >= 10.0 {
        1
    } else {
        2
    };
    format!("{value:.decimals$} {}", UNITS[unit])
}

/// The one-line summary of a check.
pub(super) fn summary(package: &str, files: usize, errors: usize, warnings: usize) -> String {
    let count = |n: usize, what: &str| format!("{n} {what}{}", if n == 1 { "" } else { "s" });
    let mut line = format!("checked `{package}`: {}", count(files, "file"));
    match (errors, warnings) {
        (0, 0) => line.push_str(", no problems"),
        _ => {
            for (n, what) in [(errors, "error"), (warnings, "warning")] {
                if n > 0 {
                    line.push_str(", ");
                    line.push_str(&count(n, what));
                }
            }
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use viso_dsl::diag::{Applicability, Fix, Related};
    use viso_dsl::{Diagnostic, TextRange, TextSize};

    use super::*;
    use crate::output::{Suggestion, compiler_report};

    fn range(text: &str, needle: &str) -> TextRange {
        let start = text.find(needle).unwrap();
        TextRange::new(
            TextSize::new(start as u32),
            TextSize::new((start + needle.len()) as u32),
        )
    }

    fn render_in(source: &Source<'_>, diagnostic: &Diagnostic) -> String {
        render(&compiler_report(Some(source), &[], diagnostic))
    }

    #[test]
    fn a_diagnostic_underlines_its_span_by_characters() {
        let text = "const a = \"日本\";\n\tstate p: Pointt;\n";
        let root = Path::new("/p");
        let source = Source::new(Path::new("/p/src/app.vs"), root, text);
        let mut error = Diagnostic::error("E2001", range(text, "Pointt"), "unresolved `Pointt`");
        error
            .related
            .push(Related::new(range(text, "\"日本\""), "declared here"));
        error.notes.push("names are case-sensitive".to_string());
        error.fixes.push(Fix {
            title: "replace with `Point`".to_string(),
            applicability: Applicability::MachineApplicable,
            edits: Vec::new(),
        });
        assert_eq!(
            render_in(&source, &error),
            "error[E2001]: unresolved `Pointt`\n \
             --> src/app.vs:2:11\n  \
             |\n\
             2 | \tstate p: Pointt;\n  \
             | \t         ^^^^^^\n\
             1 | const a = \"日本\";\n  \
             |           ---- declared here\n  \
             = note: names are case-sensitive\n  \
             = help: replace with `Point`\n"
        );
    }

    #[test]
    fn an_error_saying_no_fix_points_at_its_explanation() {
        let text = "x\n";
        let source = Source::new(Path::new("x.vs"), Path::new(""), text);
        let help = |diagnostic: &Diagnostic| {
            render_in(&source, diagnostic)
                .lines()
                .filter(|line| line.contains("= help"))
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let error = Diagnostic::error("E2001", range(text, "x"), "unresolved `x`");
        assert_eq!(help(&error), "= help: run `viso explain E2001`");
        let unknown = Diagnostic::error("E0001", range(text, "x"), "not a registered code");
        assert_eq!(help(&unknown), "");
        let warning = Diagnostic::warning("E2001", range(text, "x"), "unused");
        assert_eq!(help(&warning), "");
    }

    #[test]
    fn color_styles_the_header_frame_and_marks_and_nothing_else() {
        let text = "let x = y;\n";
        let source = Source::new(Path::new("/p/a.vs"), Path::new("/p"), text);
        let mut error = Diagnostic::error("E2001", range(text, "y"), "unresolved `y`");
        error.notes.push("names are case-sensitive".to_string());
        let report = compiler_report(Some(&source), &[], &error);
        let colored = render_with(&report, Paint(true));
        assert!(
            colored.starts_with("\x1b[1;31merror[E2001]\x1b[0m\x1b[1m: unresolved `y`\x1b[0m\n")
        );
        assert!(colored.contains("\x1b[1;31m^\x1b[0m"), "{colored:?}");
        // Without the escapes it is the plain rendering.
        let mut plain = String::new();
        let mut rest = colored.as_str();
        while let Some(at) = rest.find('\x1b') {
            plain.push_str(&rest[..at]);
            rest = &rest[at + rest[at..].find('m').unwrap() + 1..];
        }
        plain.push_str(rest);
        assert_eq!(plain, render(&report));
    }

    #[test]
    fn a_related_span_in_another_file_follows_a_line_naming_it() {
        let app = "import lib::{ Point };\n";
        let lib = "record Point { x: I64; }\n";
        let root = Path::new("/p");
        let package = [
            Source::new(Path::new("/p/src/app.vs"), root, app).of_module("app".to_string()),
            Source::new(Path::new("/p/src/lib.vs"), root, lib).of_module("lib".to_string()),
        ];
        let mut error = Diagnostic::error("E2001", range(app, "Point"), "not exported");
        error.related.push(Related::in_module(
            "lib",
            range(lib, "Point"),
            "declared here",
        ));
        error
            .related
            .push(Related::in_module("gone", range(lib, "x"), "dropped"));
        assert_eq!(
            render(&compiler_report(Some(&package[0]), &package, &error)),
            "error[E2001]: not exported\n \
             --> src/app.vs:1:15\n  \
             |\n\
             1 | import lib::{ Point };\n  \
             |               ^^^^^\n  \
             |\n \
             ::: src/lib.vs:1:8\n  \
             |\n\
             1 | record Point { x: I64; }\n  \
             |        ----- declared here\n  \
             = help: run `viso explain E2001`\n"
        );
    }

    #[test]
    fn a_span_past_its_line_stops_at_the_line_end_and_an_empty_one_is_one_mark() {
        let text = "ab\ncd";
        let source = Source::new(Path::new("x.vs"), Path::new(""), text);
        let wide = Diagnostic::error(
            "E1",
            TextRange::new(TextSize::new(1), TextSize::new(5)),
            "m",
        );
        assert!(render_in(&source, &wide).contains("1 | ab\n  |  ^\n"));
        let empty = Diagnostic::error("E1", TextRange::empty(TextSize::new(5)), "m");
        assert!(render_in(&source, &empty).contains("2 | cd\n  |   ^\n"));
    }

    #[test]
    fn a_diagnostic_without_a_location_is_its_header_notes_and_next_commands() {
        let notes = ["`--bogus` is not an option of `viso check`".to_string()];
        let text = render(&Report {
            severity: "error",
            code: "CLI_USAGE",
            message: "unexpected argument",
            location: Location::None,
            related: Vec::new(),
            expected: &[],
            actual: None,
            notes: &notes,
            fixes: vec![Suggestion::command("viso check --help")],
        });
        assert_eq!(
            text,
            "error[CLI_USAGE]: unexpected argument\n \
             = note: `--bogus` is not an option of `viso check`\n \
             = help: run `viso check --help`\n"
        );
    }

    fn reload_event(outcome: Outcome, stage: viso_view::dev::wire::Stage) -> ReloadEvent {
        ReloadEvent {
            base_revision: 41,
            candidate_revision: 42,
            last_good_revision: 41,
            outcome,
            stage,
            elapsed_us: 37_000,
            counts: Default::default(),
            codes: Vec::new(),
            game: None,
        }
    }

    #[test]
    fn a_patch_that_swapped_a_game_names_its_tier_and_first_tick() {
        use viso_view::dev::wire::{FileId, GameCommit, GameTier, Rebuild};
        let mut event = reload_event(Outcome::Applied, viso_view::dev::wire::Stage::RuntimeCommit);
        event.game = Some(GameCommit {
            file: FileId(0),
            tier: GameTier::LogicMigration,
            sessions: 1,
            tick: 312,
            carried: 4,
            reset: 1,
            characters: 0,
        });
        assert_eq!(
            dev("game.vs", &event),
            "✓ game.vs patch r41 -> r42   37 ms   game logic+state-migration from tick 312, 1 state reset"
        );
        event.game = Some(GameCommit {
            tier: GameTier::WorldRebuild {
                keep: Rebuild::KeepCharacters,
            },
            tick: 0,
            reset: 0,
            characters: 2,
            ..event.game.unwrap()
        });
        assert!(
            dev("game.vs", &event).ends_with("game world-rebuild from tick 0, 2 characters kept")
        );
    }

    #[test]
    fn an_applied_patch_names_its_revisions_time_and_dirty_nodes() {
        let mut event = reload_event(Outcome::Applied, viso_view::dev::wire::Stage::RuntimeCommit);
        event.counts.dirty.paint = 1;
        assert_eq!(
            dev("view.vs", &event),
            "✓ view.vs patch r41 -> r42   37 ms   1 node paint-dirty"
        );
    }

    #[test]
    fn a_scoped_reset_is_marked_after_the_dirty_nodes() {
        let mut event = reload_event(
            Outcome::ScopedReset,
            viso_view::dev::wire::Stage::RuntimeCommit,
        );
        event.counts.dirty.layout = 3;
        assert_eq!(
            dev("view.vs", &event),
            "✓ view.vs patch r41 -> r42   37 ms   3 nodes layout-dirty   (scoped reset)"
        );
    }

    #[test]
    fn a_reshape_with_no_dirty_node_has_no_trailing_segment() {
        let event = reload_event(Outcome::Applied, viso_view::dev::wire::Stage::RuntimeCommit);
        assert_eq!(dev("view.vs", &event), "✓ view.vs patch r41 -> r42   37 ms");
    }

    #[test]
    fn a_rejected_candidate_keeps_the_last_good_revision_and_names_its_codes() {
        let mut event = reload_event(
            Outcome::Rejected,
            viso_view::dev::wire::Stage::ShaderCompile,
        );
        event.codes = vec!["E_SHADER_TYPE".into()];
        assert_eq!(
            dev("view.vs", &event),
            "✗ view.vs candidate       kept r41   E_SHADER_TYPE"
        );
    }

    #[test]
    fn a_held_candidate_is_compiled_but_not_applied() {
        let event = reload_event(Outcome::Held, viso_view::dev::wire::Stage::Transport);
        assert_eq!(
            dev("view.vs", &event),
            "… view.vs candidate       compiled, not applied (--no-hot-reload)"
        );
    }

    #[test]
    fn a_size_has_three_significant_digits_in_binary_units() {
        assert_eq!(size(612), "612 B");
        assert_eq!(size(18 * 1024), "18.0 KiB");
        assert_eq!(size(1_908_408), "1.82 MiB");
        assert_eq!(size(612 * 1024), "612 KiB");
        assert_eq!(size(1023), "1023 B");
    }

    #[test]
    fn the_summary_counts_what_was_reported() {
        assert_eq!(
            summary("app", 1, 0, 0),
            "checked `app`: 1 file, no problems"
        );
        assert_eq!(
            summary("app", 3, 2, 1),
            "checked `app`: 3 files, 2 errors, 1 warning"
        );
        assert_eq!(
            summary("app", 0, 0, 2),
            "checked `app`: 0 files, 2 warnings"
        );
    }
}
