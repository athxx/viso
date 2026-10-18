//! A UI scenario ([`viso_view::headless::Script`]) played against the first
//! window: by a headless run, which ends once it is played, or by a dev
//! runtime the dev channel hands one to, on any target, which runs on.
//!
//! A step waits for the app to settle — no animation running, no text owed
//! — or for the scenario's settle bound, then runs: input goes in through
//! the platform as the user's device would deliver it, aimed at the center
//! of the node its selector finds; an expectation reads the window's
//! semantics; a snapshot step writes the window's last frame, read back from
//! the raster, as a PNG with its tree, layout and paint as text beside it,
//! for the launching tool to hold to a golden — a run without the raster
//! reports it skipped. Each step's outcome is a line of the report; the
//! first failed step ends the scenario.
//!
//! A selector in parentheses is an inspector selector: the run joins the
//! census of the window with the source table it is handed and takes the
//! first node it selects that is shown. Once the scenario ends a headless
//! run writes the window's census where its launch names
//! ([`viso_view::headless::CENSUS_ENV`]), for a tool to inspect what it
//! showed.

#[cfg(feature = "headless")]
use std::fs::File;
#[cfg(feature = "headless")]
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso_platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, RawScroll,
    RawText,
};
use viso_runtime::RuntimeCx;
use viso_ui::NodeId;
use viso_ui::semantics::SemanticsNode;
use viso_view::headless::{
    Action, Chord, Expectation, Key, Reported, SOURCES_ENV, Script, Selector, State,
};
use viso_view::inspect::{self, Census, SourceTable, WindowCensus, WindowDetail};

use crate::WindowState;

/// How many labels a failed expectation lists of what the window shows.
const LABELS_SHOWN: usize = 12;

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
    report: Report,
    /// Where snapshot steps write their captures; a run that captures none
    /// skips them.
    snapshots: Option<PathBuf>,
    /// What the views' nodes are in their source, for inspector selectors.
    sources: Option<Rc<SourceTable>>,
    /// Where the census goes once the scenario ends.
    census: Option<PathBuf>,
    /// Where the window's detail goes once the scenario ends.
    detail: Option<PathBuf>,
    /// Where the window's journal goes once the scenario ends.
    events: Option<PathBuf>,
    input: Input,
    /// Whether the run ends with the scenario: its window closes.
    closes: bool,
}

/// Where a scenario's input goes.
pub(crate) enum Input {
    /// Into the headless backend's queue, as its next events.
    #[cfg(feature = "headless")]
    Feed(viso_platform::backend::headless::HeadlessFeed),
    /// Posted to the scheduler, which handles it as the platform's.
    #[cfg(feature = "hot-reload")]
    Post,
}

/// Where the report's lines go.
enum Report {
    #[cfg(feature = "headless")]
    File(BufWriter<File>),
    /// Kept for the dev channel to send.
    #[cfg(feature = "hot-reload")]
    Lines(Vec<String>),
    #[cfg(feature = "headless")]
    None,
}

/// What a launch hands a scenario besides its steps.
#[cfg(feature = "headless")]
pub(crate) struct Outputs<'a> {
    pub(crate) report: Option<&'a str>,
    pub(crate) snapshots: Option<PathBuf>,
    pub(crate) sources: Option<String>,
    pub(crate) census: Option<PathBuf>,
    pub(crate) detail: Option<PathBuf>,
    pub(crate) events: Option<PathBuf>,
}

impl ScriptRunner {
    /// The scenario at `path`, which ends the run once played.
    #[cfg(feature = "headless")]
    pub(crate) fn open(path: &str, outputs: Outputs<'_>, input: Input) -> Result<Self, String> {
        let Outputs {
            report,
            snapshots,
            sources,
            census,
            detail,
            events,
        } = outputs;
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read the scenario `{path}`: {error}"))?;
        let script = Script::parse(&text)
            .map_err(|error| format!("{path}:{}: {}", error.line, error.message))?;
        let report = match report {
            Some(report) => {
                Report::File(BufWriter::new(File::create(report).map_err(|error| {
                    format!("cannot write the report `{report}`: {error}")
                })?))
            }
            None => Report::None,
        };
        let sources = match sources {
            Some(sources) => {
                let bytes = std::fs::read(&sources).map_err(|error| {
                    format!("cannot read the source table `{sources}`: {error}")
                })?;
                Some(Rc::new(SourceTable::from_bytes(&bytes).map_err(
                    |error| format!("the source table `{sources}` does not decode: {error}"),
                )?))
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
            snapshots,
            sources,
            census,
            detail,
            events,
            input,
            closes: true,
        })
    }

    /// The scenario `text` the dev channel brought, with the source table
    /// `sources`: its input posted as the platform's, its report kept for
    /// [`take_lines`](Self::take_lines), its snapshots skipped, and the app
    /// left running once it is played.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn from_host(text: &str, sources: Option<&[u8]>) -> Result<Self, String> {
        let script = Script::parse(text)
            .map_err(|error| format!("scenario:{}: {}", error.line, error.message))?;
        let sources = match sources {
            Some(bytes) => Some(Rc::new(
                SourceTable::from_bytes(bytes)
                    .map_err(|error| format!("the source table does not decode: {error}"))?,
            )),
            None => None,
        };
        Ok(ScriptRunner {
            script,
            next: 0,
            since: 0,
            waiting: 0,
            frames: 0,
            finished: false,
            report: Report::Lines(Vec::new()),
            snapshots: None,
            sources,
            census: None,
            detail: None,
            events: None,
            input: Input::Post,
            closes: false,
        })
    }

    /// The report's lines written since the last call.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn take_lines(&mut self) -> Vec<String> {
        match &mut self.report {
            Report::Lines(lines) => std::mem::take(lines),
            #[cfg(feature = "headless")]
            _ => Vec::new(),
        }
    }

    /// Whether every step it could play has been.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn is_finished(&self) -> bool {
        self.finished
    }

    /// After each frame of the first window `ws`, whose census `census`
    /// takes and whose detail `detail` does: run the steps due.
    pub(crate) fn after_frame(
        &mut self,
        ws: &WindowState,
        census: &dyn Fn() -> WindowCensus,
        detail: Option<&dyn Fn() -> WindowDetail>,
        cx: &mut RuntimeCx<'_>,
    ) {
        if self.finished {
            return;
        }
        let sources = self.sources.clone();
        let window = Window {
            ws,
            census,
            detail,
            sources: sources.as_deref(),
        };
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
                Action::Expect(expectation) => expect(&window, expectation),
                Action::Snapshot(name) if self.snapshots.is_none() && !self.closes => {
                    self.write(Reported::Skipped {
                        line,
                        message: format!("the snapshot `{name}` is taken headless"),
                    });
                    continue;
                }
                Action::Snapshot(name) => match capture(ws, self.snapshots.as_deref(), name) {
                    Ok((width, height)) => {
                        self.write(Reported::Snapshot {
                            line,
                            name: name.clone(),
                            width,
                            height,
                        });
                        continue;
                    }
                    Err(message) => Err(message),
                },
                Action::Wait(frames) => {
                    self.waiting = *frames - 1;
                    self.write(Reported::Passed { line });
                    cx.request_redraw(ws.window);
                    return;
                }
                input => match events(&window, input) {
                    Ok(events) => {
                        for event in events {
                            match &self.input {
                                #[cfg(feature = "headless")]
                                Input::Feed(feed) => feed.push(event),
                                #[cfg(feature = "hot-reload")]
                                Input::Post => cx.post_event(event),
                            }
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
        self.finish(&window, cx);
    }

    fn finish(&mut self, window: &Window<'_>, cx: &mut RuntimeCx<'_>) {
        self.finished = true;
        if let Some(path) = &self.census {
            let census = Census {
                windows: vec![window.census()],
            };
            if let Err(error) = std::fs::write(path, census.to_bytes()) {
                eprintln!(
                    "viso: cannot write the census `{}`: {error}",
                    path.display()
                );
            }
        }
        if let (Some(path), Some(detail)) = (&self.detail, window.detail)
            && let Err(error) = std::fs::write(path, detail().to_bytes())
        {
            eprintln!(
                "viso: cannot write the detail `{}`: {error}",
                path.display()
            );
        }
        if let (Some(path), Some(journal)) = (&self.events, &window.ws.journal)
            && let Err(error) = std::fs::write(path, journal.read(0, 0).to_bytes())
        {
            eprintln!(
                "viso: cannot write the events `{}`: {error}",
                path.display()
            );
        }
        let frames = self.frames;
        self.write(Reported::Done { frames });
        if self.closes {
            cx.close_window(window.ws.window);
        }
    }

    fn write(&mut self, reported: Reported) {
        match &mut self.report {
            #[cfg(feature = "headless")]
            Report::File(report) => {
                let _ = writeln!(report, "{}", reported.to_line());
                let _ = report.flush();
            }
            #[cfg(feature = "hot-reload")]
            Report::Lines(lines) => lines.push(reported.to_line()),
            #[cfg(feature = "headless")]
            Report::None => {}
        }
    }
}

/// Writes the last frame of `ws` as the snapshot `name` into `dir`: the
/// image and, beside it, the tree, layout, paint and batches that drew it;
/// its size in device pixels.
#[cfg(feature = "headless")]
fn capture(ws: &WindowState, dir: Option<&Path>, name: &str) -> Result<(u32, u32), String> {
    use viso_image::Image;
    use viso_view::headless::SNAPSHOT_ENV;
    let dir = dir.ok_or_else(|| {
        format!("the run was given no directory to write the snapshot `{name}` to ({SNAPSHOT_ENV})")
    })?;
    let gpu = ws
        .gpu
        .as_ref()
        .ok_or_else(|| "the window has no surface to capture".to_owned())?;
    let surface = gpu
        .surface
        .ok_or_else(|| "the window has no surface to capture".to_owned())?;
    let (width, height) = gpu.size;
    let bgra = gpu.backend.read_pixels_bgra8(surface);
    if bgra.len() != width as usize * height as usize * 4 {
        return Err(format!(
            "the surface read back {} bytes for {width} × {height} pixels",
            bgra.len()
        ));
    }
    let image = Image::from_bgra8(width, height, &bgra);
    let tree = match ws.root {
        Some(root) => viso_ui::snapshot_ui(
            &ws.store,
            root,
            gpu.renderer.inspect_batches(),
            gpu.renderer.frame_stats(),
        )
        .dump(),
        None => String::new(),
    };
    let write = |file: String, bytes: &[u8]| {
        let path = dir.join(&file);
        std::fs::write(&path, bytes)
            .map_err(|error| format!("cannot write the snapshot `{}`: {error}", path.display()))
    };
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create `{}`: {error}", dir.display()))?;
    write(format!("{name}.png"), &image.to_png())?;
    write(format!("{name}.tree"), tree.as_bytes())?;
    Ok((width, height))
}

/// A frame's readback exists on the software raster only.
#[cfg(not(feature = "headless"))]
fn capture(_: &WindowState, _: Option<&Path>, name: &str) -> Result<(u32, u32), String> {
    Err(format!("the snapshot `{name}` is taken headless"))
}

/// The window a scenario plays against: its state, how its census is taken
/// and what its views' nodes are in their source.
struct Window<'a> {
    ws: &'a WindowState,
    census: &'a dyn Fn() -> WindowCensus,
    detail: Option<&'a dyn Fn() -> WindowDetail>,
    sources: Option<&'a SourceTable>,
}

impl Window<'_> {
    fn census(&self) -> WindowCensus {
        (self.census)()
    }
}

/// A node of the window as a scenario sees it: its semantics, `None` for a
/// node an inspector selector found outside the semantics tree.
struct Shown<'a> {
    node: Option<&'a SemanticsNode>,
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

/// The first node `selector` finds in `window`, or why there is none.
fn find(
    window: &Window<'_>,
    selector: &Selector,
    f: impl FnOnce(Shown<'_>) -> Result<(), String>,
) -> Result<(), String> {
    let ws = window.ws;
    let semantics = match ws.root {
        Some(root) => ws.store.derive_semantics(root).nodes,
        None => Vec::new(),
    };
    let shown = shown(ws, &semantics);
    let missing = || {
        Err(format!(
            "no {selector} is shown{}",
            labels(&semantics, &shown)
        ))
    };
    match selector {
        Selector::Label { role, label } => {
            let found = shown.iter().find(|(index, _)| {
                let node = &semantics[*index];
                node.label.as_deref() == Some(label.as_str())
                    && role.is_none_or(|role| node.role == role)
            });
            match found {
                Some(&(index, center)) => f(Shown {
                    node: Some(&semantics[index]),
                    center,
                }),
                None => missing(),
            }
        }
        Selector::Query(query) => {
            let Some(sources) = window.sources else {
                return Err(format!(
                    "the run was given no source table to find {selector} by ({SOURCES_ENV})"
                ));
            };
            let census = Census {
                windows: vec![window.census()],
            };
            let inspection = inspect::resolve(&census, sources);
            let found = query
                .select(&inspection)
                .into_iter()
                .map(|at| inspection.nodes[at].node)
                .find(|node| node.shown && node.bounds.w > 0.0 && node.bounds.h > 0.0);
            let Some(node) = found else {
                return missing();
            };
            let b = node.bounds;
            f(Shown {
                node: semantics
                    .iter()
                    .find(|s| s.id.index() == node.index && s.id.generation() == node.generation),
                center: (f64::from(b.x + b.w / 2.0), f64::from(b.y + b.h / 2.0)),
            })
        }
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

/// Whether `expectation` holds of `window`, or why not.
fn expect(window: &Window<'_>, expectation: &Expectation) -> Result<(), String> {
    match expectation {
        Expectation::Shown(selector) => find(window, selector, |_| Ok(())),
        Expectation::Absent(selector) => match find(window, selector, |_| Ok(())) {
            Ok(()) => Err(format!("{selector} is shown")),
            Err(_) => Ok(()),
        },
        Expectation::State(selector, state) => find(window, selector, |shown| {
            let Some(node) = shown.node else {
                return Err(format!(
                    "{selector} has no semantics to be {}",
                    state.describe()
                ));
            };
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

/// The platform events an input step delivers to `at`.
fn events(at: &Window<'_>, action: &Action) -> Result<Vec<RawEvent>, String> {
    let window = at.ws.window;
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
        Action::Click(selector) => find(at, selector, |shown| {
            events = vec![
                pointer(shown.center, PointerButtons::NONE, PointerPhase::Moved),
                pointer(shown.center, PointerButtons::PRIMARY, PointerPhase::Down),
                pointer(shown.center, PointerButtons::NONE, PointerPhase::Up),
            ];
            Ok(())
        })?,
        Action::Hover(selector) => find(at, selector, |shown| {
            events = vec![pointer(
                shown.center,
                PointerButtons::NONE,
                PointerPhase::Moved,
            )];
            Ok(())
        })?,
        Action::Scroll(selector, dx, dy) => find(at, selector, |shown| {
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
        Action::Wait(_) | Action::Expect(_) | Action::Snapshot(_) => {}
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
