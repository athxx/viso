//! How the editor holds up on a large file: the time to open one and draw
//! its first frame, the time from a keystroke to its frame, and the memory
//! it takes. Run in release, with the numbers printed:
//! `cargo test --release -p viso-code-editor --test perf -- --ignored --nocapture`.

mod common;

use std::time::Instant;

use common::*;

const LINES: usize = 100_000;

/// `n` lines of `.vs`, a sample's over and over.
fn large(n: usize) -> String {
    let sample = include_str!("../../../crates/dsl/tests/native/colony.vs");
    let lines: Vec<&str> = sample.lines().collect();
    let mut out = String::with_capacity(n * 40);
    for i in 0..n {
        out.push_str(lines[i % lines.len()]);
        out.push('\n');
    }
    out
}

/// This process's resident memory, in KiB.
fn rss_kib() -> u64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps");
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0)
}

fn languaged(e: viso_code_editor::CodeEditor) -> viso_code_editor::CodeEditor {
    e.language(viso_code_editor::lang::for_path("a.vs"))
}

/// Seconds a run of `steps` over `text` takes, the best of `tries`.
fn timed(text: &str, steps: &[Vec<RawEvent>], tries: usize) -> f64 {
    (0..tries)
        .map(|_| {
            let t = Instant::now();
            let app = run(text, steps);
            let s = t.elapsed().as_secs_f64();
            drop(app);
            s
        })
        .fold(f64::INFINITY, f64::min)
}

#[test]
#[ignore = "a measurement, for release builds"]
fn a_hundred_thousand_lines_open_and_edit_quickly() {
    setup(Some(languaged));
    let text = large(LINES);
    eprintln!(
        "{LINES} lines, {:.1} MiB",
        text.len() as f64 / (1 << 20) as f64
    );
    // The harness's own frames, with no text to speak of, are taken off.
    let base = timed("", &[], 3);
    let before = rss_kib();
    let t = Instant::now();
    let app = run(&text, &[focus()]);
    let first = t.elapsed().as_secs_f64();
    let after = rss_kib();
    assert_eq!(handle().with(|s| s.doc().line_count()), LINES + 1);
    drop(app);
    let open = timed(&text, &[focus()], 3) - base;
    eprintln!(
        "open + first frames: {:.1} ms (first run {:.1} ms)",
        open * 1e3,
        (first - base) * 1e3
    );
    eprintln!(
        "memory: {:.1} MiB resident for the open editor ({:.1} MiB of text)",
        after.saturating_sub(before) as f64 / 1024.0,
        text.len() as f64 / (1 << 20) as f64
    );
    // Each keystroke is a key or text event and one frame drawn after it.
    const KEYS: usize = 200;
    let per = |name: &str, step: Vec<RawEvent>, at: Vec<RawEvent>| {
        let mut steps = vec![focus(), at.clone()];
        let start = timed(&text, &steps, 3);
        steps.extend(std::iter::repeat_n(step, KEYS));
        let all = timed(&text, &steps, 3);
        let ms = (all - start) / KEYS as f64 * 1e3;
        eprintln!("keystroke to frame, {name}: {ms:.3} ms");
        ms
    };
    let none = Vec::new();
    // The document's end: the primary modifier with Down on Apple systems,
    // with End elsewhere.
    let end = || {
        key_with(
            if apple() { Key::Down } else { Key::End },
            primary(false, false),
        )
    };
    let typing = per("typing at the top", typed("x"), none.clone());
    let at_end = per("typing at the end", typed("x"), end());
    let enter = per("Enter at the top", key(Key::Enter), none.clone());
    let back = per("Backspace at the end", key(Key::Backspace), end());
    let down = per("Down", key(Key::Down), none);
    for ms in [typing, at_end, enter, back, down] {
        assert!(ms < 16.0, "a keystroke within a frame: {ms} ms");
    }
    setup(None);
}
