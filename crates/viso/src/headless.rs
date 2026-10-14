//! The run of a headless build, and the UI scenario it plays.
//!
//! Every window gets the surface the launch names (1280 × 720 at 1x when it
//! names none) and draws through the software raster; each frame steps the
//! clock by one frame of 60 Hz; the run ends when the app settles, after the
//! frames the launch bounds it to, or once its scenario is played. No
//! service has an OS behind it.
//!
//! A scenario ([`viso_view::headless::Script`]) plays against the first
//! window. A step waits for the app to settle — no animation running, no
//! text owed — or for the scenario's settle bound, then runs: input goes in
//! through the platform as the user's device would deliver it, aimed at the
//! center of the node its selector finds; an expectation reads the window's
//! semantics. Each step's outcome is a line of the report; the first failed
//! step ends the run.

use std::fs::File;
use std::io::{BufWriter, Write};

use viso_platform::backend::headless::{HeadlessApp, HeadlessFeed};
use viso_platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, RawScroll,
    RawText, WindowId,
};
use viso_runtime::{FixedStepClock, RuntimeCx, Scheduler};
use viso_ui::NodeId;
use viso_ui::semantics::SemanticsNode;
use viso_view::headless::{
    Action, Chord, Expectation, FRAMES_ENV, HeadlessSurface, Key, REPORT_ENV, Reported, SCRIPT_ENV,
    SURFACE_ENV, Script, Selector, State,
};

use crate::{AppDriver, Application, WindowState, services};

/// How many labels a failed expectation lists of what the window shows.
const LABELS_SHOWN: usize = 12;

pub(crate) fn run<A: Application>() {
    let surface = viso_platform::launch_var(SURFACE_ENV)
        .and_then(|value| HeadlessSurface::from_env(&value))
        .unwrap_or_default();
    let mut app = Box::new(HeadlessApp::new());
    app.fixed_surface(surface.width, surface.height, surface.scale);
    if let Some(frames) =
        viso_platform::launch_var(FRAMES_ENV).and_then(|value| value.trim().parse().ok())
    {
        app.frame_budget(frames);
    }
    let mut driver = AppDriver::<A>::new(Some(services::Services::unsupported()));
    if let Some(path) = viso_platform::launch_var(SCRIPT_ENV) {
        let report = viso_platform::launch_var(REPORT_ENV);
        match ScriptRunner::open(&path, report.as_deref(), app.feed()) {
            Ok(runner) => driver.script = Some(runner),
            Err(why) => {
                eprintln!("viso: {why}");
                std::process::exit(2);
            }
        }
    }
    let clock = FixedStepClock::new(
        viso_platform::Instant::now(),
        std::time::Duration::from_nanos(1_000_000_000 / 60),
    );
    Scheduler::with_clock(app, driver, clock).run();
}

/// A scenario being played.
pub(crate) struct ScriptRunner {
    script: Script,
    /// The next step to run.
    next: usize,
    /// Frames run since the last input step.
    since: u32,
    /// Frames a `wait` still lets run.
    waiting: u32,
    /// Frames run in all.
    frames: u64,
    finished: bool,
    report: Option<BufWriter<File>>,
    feed: HeadlessFeed,
}

impl ScriptRunner {
    fn open(path: &str, report: Option<&str>, feed: HeadlessFeed) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read the scenario `{path}`: {error}"))?;
        let script = Script::parse(&text)
            .map_err(|error| format!("{path}:{}: {}", error.line, error.message))?;
        let report = match report {
            Some(report) => {
                Some(BufWriter::new(File::create(report).map_err(|error| {
                    format!("cannot write the report `{report}`: {error}")
                })?))
            }
            None => None,
        };
        Ok(ScriptRunner {
            script,
            next: 0,
            since: 0,
            waiting: 0,
            frames: 0,
            finished: false,
            report,
            feed,
        })
    }

    /// After each frame of the first window `ws`: run the steps due.
    pub(crate) fn after_frame(&mut self, ws: &WindowState, cx: &mut RuntimeCx<'_>) {
        if self.finished {
            return;
        }
        self.frames += 1;
        if self.waiting > 0 {
            self.waiting -= 1;
            cx.request_redraw(ws.window);
            return;
        }
        let busy = ws.is_animating() || ws.text_work_pending();
        if busy && self.since < self.script.settle {
            self.since += 1;
            return;
        }
        while let Some(step) = self.script.steps.get(self.next) {
            let line = step.line;
            self.next += 1;
            let outcome = match &step.action {
                Action::Expect(expectation) => expect(ws, expectation),
                Action::Wait(frames) => {
                    self.waiting = *frames - 1;
                    self.write(Reported::Passed { line });
                    cx.request_redraw(ws.window);
                    return;
                }
                input => match events(ws, input) {
                    Ok(events) => {
                        for event in events {
                            self.feed.push(event);
                        }
                        self.since = 0;
                        self.write(Reported::Passed { line });
                        // The input runs a frame whether or not it changes
                        // anything, so the next step is reached.
                        cx.request_redraw(ws.window);
                        return;
                    }
                    Err(message) => Err(message),
                },
            };
            match outcome {
                Ok(()) => self.write(Reported::Passed { line }),
                Err(message) => {
                    self.write(Reported::Failed { line, message });
                    break;
                }
            }
        }
        self.finish(ws.window, cx);
    }

    fn finish(&mut self, window: WindowId, cx: &mut RuntimeCx<'_>) {
        self.finished = true;
        let frames = self.frames;
        self.write(Reported::Done { frames });
        cx.close_window(window);
    }

    fn write(&mut self, reported: Reported) {
        if let Some(report) = &mut self.report {
            let _ = writeln!(report, "{}", reported.to_line());
            let _ = report.flush();
        }
    }
}

/// A node of the window as a scenario sees it.
struct Shown<'a> {
    node: &'a SemanticsNode,
    center: (f64, f64),
}

/// The nodes of `ws` shown now, in tree order, with their centers.
fn shown(ws: &WindowState, semantics: &[SemanticsNode]) -> Vec<(usize, (f64, f64))> {
    let Some(root) = ws.root else {
        return Vec::new();
    };
    let tree = ws.store.inspect_tree(root);
    let hidden = |id: NodeId| {
        let mut at = tree.get(id);
        while let Some(node) = at {
            if node.flags.hidden {
                return true;
            }
            at = node.parent.and_then(|parent| tree.get(parent));
        }
        false
    };
    semantics
        .iter()
        .enumerate()
        .filter(|(_, node)| !hidden(node.id))
        .filter_map(|(index, node)| {
            let world = tree.get(node.id)?.world;
            (world.w > 0.0 && world.h > 0.0).then(|| {
                (
                    index,
                    (
                        f64::from(world.x + world.w / 2.0),
                        f64::from(world.y + world.h / 2.0),
                    ),
                )
            })
        })
        .collect()
}

/// The first node `selector` finds in `ws`, or why there is none.
fn find(
    ws: &WindowState,
    selector: &Selector,
    f: impl FnOnce(Shown<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let semantics = match ws.root {
        Some(root) => ws.store.derive_semantics(root).nodes,
        None => Vec::new(),
    };
    let shown = shown(ws, &semantics);
    let found = shown.iter().find(|(index, _)| {
        let node = &semantics[*index];
        node.label.as_deref() == Some(selector.label.as_str())
            && selector.role.is_none_or(|role| node.role == role)
    });
    match found {
        Some(&(index, center)) => f(Shown {
            node: &semantics[index],
            center,
        }),
        None => Err(format!(
            "no {selector} is shown{}",
            labels(&semantics, &shown)
        )),
    }
}

/// What the window shows, for a message: `; the window shows "A", "B"`.
fn labels(semantics: &[SemanticsNode], shown: &[(usize, (f64, f64))]) -> String {
    let labels: Vec<String> = shown
        .iter()
        .filter_map(|(index, _)| semantics[*index].label.as_ref())
        .take(LABELS_SHOWN)
        .map(|label| format!("{label:?}"))
        .collect();
    if labels.is_empty() {
        "; the window shows no labeled node".to_owned()
    } else {
        format!("; the window shows {}", labels.join(", "))
    }
}

/// Whether `expectation` holds of `ws`, or why not.
fn expect(ws: &WindowState, expectation: &Expectation) -> Result<(), String> {
    match expectation {
        Expectation::Shown(selector) => find(ws, selector, |_| Ok(())),
        Expectation::Absent(selector) => match find(ws, selector, |_| Ok(())) {
            Ok(()) => Err(format!("{selector} is shown")),
            Err(_) => Ok(()),
        },
        Expectation::State(selector, state) => find(ws, selector, |shown| {
            let node = shown.node;
            let live = node.state.unwrap_or_default();
            let holds = match *state {
                State::Focused => node.focused,
                State::Checked => live.checked == Some(true),
                State::Unchecked => live.checked == Some(false),
                State::Selected => live.selected == Some(true),
                State::Expanded => live.expanded == Some(true),
                State::Collapsed => live.expanded == Some(false),
                State::Value(value) => live.value.is_some_and(|actual| {
                    (actual - value).abs() <= f32::EPSILON * value.abs().max(1.0)
                }),
            };
            if holds {
                return Ok(());
            }
            let actual = match *state {
                State::Focused => "not focused".to_owned(),
                State::Checked | State::Unchecked => match live.checked {
                    Some(true) => "checked".to_owned(),
                    Some(false) => "unchecked".to_owned(),
                    None => "not checkable".to_owned(),
                },
                State::Selected => "not selected".to_owned(),
                State::Expanded | State::Collapsed => match live.expanded {
                    Some(true) => "expanded".to_owned(),
                    Some(false) => "collapsed".to_owned(),
                    None => "not expandable".to_owned(),
                },
                State::Value(_) => match live.value {
                    Some(actual) => format!("value {actual}"),
                    None => "without a value".to_owned(),
                },
            };
            Err(format!("{selector} is {actual}, not {}", state.describe()))
        }),
    }
}

/// The platform events an input step delivers to `ws`.
fn events(ws: &WindowState, action: &Action) -> Result<Vec<RawEvent>, String> {
    let window = ws.window;
    let pointer = |(x, y): (f64, f64), buttons, phase| {
        RawEvent::Pointer(RawPointer::mouse(
            window,
            x,
            y,
            buttons,
            Modifiers::default(),
            phase,
        ))
    };
    let mut events = Vec::new();
    match action {
        Action::Click(selector) => find(ws, selector, |shown| {
            events = vec![
                pointer(shown.center, PointerButtons::NONE, PointerPhase::Moved),
                pointer(shown.center, PointerButtons::PRIMARY, PointerPhase::Down),
                pointer(shown.center, PointerButtons::NONE, PointerPhase::Up),
            ];
            Ok(())
        })?,
        Action::Hover(selector) => find(ws, selector, |shown| {
            events = vec![pointer(
                shown.center,
                PointerButtons::NONE,
                PointerPhase::Moved,
            )];
            Ok(())
        })?,
        Action::Scroll(selector, dx, dy) => find(ws, selector, |shown| {
            let (x, y) = shown.center;
            events = vec![
                pointer(shown.center, PointerButtons::NONE, PointerPhase::Moved),
                RawEvent::Scroll(RawScroll {
                    window,
                    x,
                    y,
                    delta_x: *dx,
                    delta_y: *dy,
                    modifiers: Modifiers::default(),
                }),
            ];
            Ok(())
        })?,
        Action::Type(text) => events.push(RawEvent::Text(RawText {
            window,
            text: text.clone(),
        })),
        Action::Key(chord) => {
            let modifiers = modifiers(chord);
            let code = key_code(chord.key);
            for pressed in [true, false] {
                events.push(RawEvent::Key(RawKey {
                    window,
                    code,
                    pressed,
                    repeat: false,
                    modifiers,
                }));
            }
        }
        Action::Wait(_) | Action::Expect(_) => {}
    }
    Ok(events)
}

/// The modifiers `chord` holds, its `Primary` as this platform's.
fn modifiers(chord: &Chord) -> Modifiers {
    let primary_is_logo = cfg!(target_vendor = "apple");
    Modifiers {
        shift: chord.shift,
        control: chord.control || (chord.primary && !primary_is_logo),
        alt: chord.alt,
        logo: chord.logo || (chord.primary && primary_is_logo),
    }
}

fn key_code(key: Key) -> KeyCode {
    const LETTERS: [KeyCode; 26] = [
        KeyCode::A,
        KeyCode::B,
        KeyCode::C,
        KeyCode::D,
        KeyCode::E,
        KeyCode::F,
        KeyCode::G,
        KeyCode::H,
        KeyCode::I,
        KeyCode::J,
        KeyCode::K,
        KeyCode::L,
        KeyCode::M,
        KeyCode::N,
        KeyCode::O,
        KeyCode::P,
        KeyCode::Q,
        KeyCode::R,
        KeyCode::S,
        KeyCode::T,
        KeyCode::U,
        KeyCode::V,
        KeyCode::W,
        KeyCode::X,
        KeyCode::Y,
        KeyCode::Z,
    ];
    const DIGITS: [KeyCode; 10] = [
        KeyCode::Digit0,
        KeyCode::Digit1,
        KeyCode::Digit2,
        KeyCode::Digit3,
        KeyCode::Digit4,
        KeyCode::Digit5,
        KeyCode::Digit6,
        KeyCode::Digit7,
        KeyCode::Digit8,
        KeyCode::Digit9,
    ];
    const FUNCTIONS: [KeyCode; 12] = [
        KeyCode::F1,
        KeyCode::F2,
        KeyCode::F3,
        KeyCode::F4,
        KeyCode::F5,
        KeyCode::F6,
        KeyCode::F7,
        KeyCode::F8,
        KeyCode::F9,
        KeyCode::F10,
        KeyCode::F11,
        KeyCode::F12,
    ];
    match key {
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Escape,
        Key::Tab => KeyCode::Tab,
        Key::Space => KeyCode::Space,
        Key::Backspace => KeyCode::Backspace,
        Key::Delete => KeyCode::Delete,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::Letter(c) => LETTERS[(c as u8 - b'A') as usize],
        Key::Digit(n) => DIGITS[n as usize],
        Key::Function(n) => FUNCTIONS[n as usize - 1],
    }
}
