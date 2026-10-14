//! `viso run` across a font edit, end to end: the running app takes the
//! edited packaged font in place, and only its text lays out again; a font
//! that does not parse is reported and kept from the app.
//!
//! It builds a probe app against this repository and opens its window, so it
//! is ignored by default:
//! `cargo test -p viso-cli --test font_patch -- --ignored --nocapture`.
#![cfg(unix)]

mod support;

use std::time::Duration;

use support::Run;

const FONT: &[u8] = include_bytes!("../../../crates/viso/fixtures/DejaVuSans-subset.ttf");

const VIEW: &str = r#"component Probe {
    view {
        Column {
            Text { text: "Ag"; }
            Column { width: 40dp; height: 40dp; }
        }
    }
}
"#;

const MAIN: &str = r#"use viso::prelude::*;

struct Probe;

impl Application for Probe {
    fn new(cx: &mut AppCx) -> Self {
        cx.use_packaged_fonts(viso::packaged_fonts!());
        Probe
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("probe.vs")(cx);
    }
}

fn main() {
    viso::run::<Probe>();
}
"#;

fn font_event(line: &str, outcome: &str) -> bool {
    line.contains("\"file\":\"assets/fonts/Body.ttf\"")
        && (support::dev_event(line, "runtime-commit", outcome)
            || support::dev_event(line, "resource-build", outcome))
}

#[test]
#[ignore = "builds a probe app and opens its window"]
fn an_edited_font_lays_out_again_only_the_text_drawing_with_it() {
    let app = support::project(
        "font_patch",
        &[
            ("assets/fonts/Body.ttf", FONT),
            ("src/probe.vs", VIEW.as_bytes()),
            ("src/main.rs", MAIN.as_bytes()),
        ],
    );
    let run = Run::start(&app, &[], &[]);
    run.connected();

    // Trailing bytes change the file, not its faces: the text drawing with
    // it reshapes — the view's and the window caption's — and nothing else.
    let edited = [FONT, &[0; 4]].concat();
    std::fs::write(app.join("assets/fonts/Body.ttf"), &edited).expect("the font");
    let applied = run.until(Duration::from_secs(60), "the font patch", |line| {
        font_event(line, "applied") || font_event(line, "rejected")
    });
    assert!(font_event(&applied, "applied"), "{applied}");
    assert!(
        applied.contains("\"dirty\":{\"structure\":0,\"style\":0,")
            && !applied.contains("\"measure\":0")
            && applied.contains("\"semantics\":0"),
        "only the text: {applied}"
    );

    // A cut font is reported and the app keeps the last good one.
    std::fs::write(app.join("assets/fonts/Body.ttf"), &FONT[..64]).expect("the font");
    let rejected = run.until(Duration::from_secs(60), "the cut font", |line| {
        font_event(line, "applied") || font_event(line, "rejected")
    });
    assert!(font_event(&rejected, "rejected"), "{rejected}");

    assert_eq!(run.stop(), Some(130));
}
