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
//! semantics; a snapshot step writes the window's last frame, read back from
//! the raster, as a PNG with its tree, layout and paint as text beside it,
//! for the launching tool to hold to a golden. Each step's outcome is a line
//! of the report; the first failed step ends the run.
//!
//! A selector in parentheses is an inspector selector: the run joins the
//! census of the window with the source table the launch hands it
//! ([`SOURCES_ENV`]) and takes the first node it selects that is shown.
//! Once the scenario ends the run writes the window's census where the
//! launch names ([`CENSUS_ENV`]), for a tool to inspect what it showed.
//!
//! A run handed a view package ([`VIEW_ENV`]) mounts it alone in its first
//! window, with no caption and none of the app's own view, so a tool
//! captures one component as the app's fonts and assets draw it.

use std::cell::RefCell;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso_platform::backend::headless::{HeadlessApp, HeadlessFeed};
use viso_platform::{
    KeyCode, Modifiers, PointerButtons, PointerPhase, RawEvent, RawKey, RawPointer, RawScroll,
    RawText, WindowId,
};
use viso_runtime::{FixedStepClock, RuntimeCx, Scheduler};
use viso_ui::semantics::SemanticsNode;
use viso_ui::{DirtyClass, NodeId};
use viso_view::headless::{
    Action, CENSUS_ENV, Chord, Expectation, FRAMES_ENV, HeadlessSurface, Key, REPORT_ENV, Reported,
    SCRIPT_ENV, SNAPSHOT_ENV, SOURCES_ENV, SURFACE_ENV, Script, Selector, State, VIEW_ENV,
};
use viso_view::inspect::{self, Census, CensusView, SourceTable, WindowCensus, static_nodes};
use viso_view::snapshot::Image;
use viso_view::{MountRecord, ViewHost, load_view, take_mounts};

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
    if let Some(path) = viso_platform::launch_var(VIEW_ENV) {
        match std::fs::read(&path) {
            Ok(package) => driver.harness.package = Some(package),
            Err(error) => {
                eprintln!("viso: cannot read the view `{path}`: {error}");
                std::process::exit(2);
            }
        }
    }
    if let Some(path) = viso_platform::launch_var(SCRIPT_ENV) {
        let report = viso_platform::launch_var(REPORT_ENV);
        let snapshots = viso_platform::launch_var(SNAPSHOT_ENV).map(PathBuf::from);
        let outputs = Outputs {
            report: report.as_deref(),
            snapshots,
            sources: viso_platform::launch_var(SOURCES_ENV),
            census: viso_platform::launch_var(CENSUS_ENV).map(PathBuf::from),
        };
        match ScriptRunner::open(&path, outputs, app.feed()) {
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

/// The view package a run mounts alone, and once it is mounted the instance
/// that runs it; the views the first window's build mounted.
#[derive(Default)]
pub(crate) struct Harness {
    package: Option<Vec<u8>>,
    host: Option<Rc<RefCell<ViewHost>>>,
    first: Option<WindowId>,
    mounts: Vec<MountRecord>,
}

impl Harness {
    /// Keeps the views `ws`'s build just mounted, when it is the first
    /// window, which a scenario plays against.
    pub(crate) fn adopt(&mut self, ws: &WindowState) {
        let mut records = Vec::new();
        take_mounts(&mut records);
        if *self.first.get_or_insert(ws.window) == ws.window {
            self.mounts.append(&mut records);
        }
    }

    /// The views the first window's build mounted.
    pub(crate) fn mounts(&self) -> &[MountRecord] {
        &self.mounts
    }

    /// Whether a view waits to be mounted in place of the app's.
    pub(crate) fn is_pending(&self) -> bool {
        self.package.is_some()
    }

    /// Mounts the view as the root of `ws`, a window opened with none; a
    /// package that does not load ends the run.
    pub(crate) fn mount(&mut self, ws: &mut WindowState) {
        let Some(package) = self.package.take() else {
            return;
        };
        let loaded = match load_view(
            &package,
            &mut ws.store,
            &mut ws.states,
            &mut ws.bindings,
            &mut ws.virtual_lists,
        ) {
            Ok(loaded) => loaded,
            Err(error) => {
                eprintln!("viso: the view to mount does not load: {error}");
                std::process::exit(2);
            }
        };
        self.host = loaded.host;
        ws.root = loaded.root;
        ws.shape_pending_text();
        if let Some(root) = ws.root {
            ws.store.mark_dirty(
                root,
                DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
            );
        }
    }
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
    /// Where snapshot steps write their captures.
    snapshots: Option<PathBuf>,
    /// What the views' nodes are in their source, for inspector selectors.
    sources: Option<Rc<SourceTable>>,
    /// Where the census goes once the scenario ends.
    census: Option<PathBuf>,
    feed: HeadlessFeed,
}

/// What a launch hands a scenario besides its steps.
struct Outputs<'a> {
    report: Option<&'a str>,
    snapshots: Option<PathBuf>,
    sources: Option<String>,
    census: Option<PathBuf>,
}

impl ScriptRunner {
    fn open(path: &str, outputs: Outputs<'_>, feed: HeadlessFeed) -> Result<Self, String> {
        let Outputs {
            report,
            snapshots,
            sources,
            census,
        } = outputs;
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
            feed,
        })
    }

    /// After each frame of the first window `ws`, whose build mounted
    /// `mounts`: run the steps due.
    pub(crate) fn after_frame(
        &mut self,
        ws: &WindowState,
        mounts: &[MountRecord],
        cx: &mut RuntimeCx<'_>,
    ) {
        if self.finished {
            return;
        }
        let sources = self.sources.clone();
        let window = Window {
            ws,
            mounts,
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
        let frames = self.frames;
        self.write(Reported::Done { frames });
        cx.close_window(window.ws.window);
    }

    fn write(&mut self, reported: Reported) {
        if let Some(report) = &mut self.report {
            let _ = writeln!(report, "{}", reported.to_line());
            let _ = report.flush();
        }
    }
}

/// Writes the last frame of `ws` as the snapshot `name` into `dir`: the
/// image and, beside it, the tree, layout, paint and batches that drew it;
/// its size in device pixels.
fn capture(ws: &WindowState, dir: Option<&Path>, name: &str) -> Result<(u32, u32), String> {
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

/// The window a scenario plays against: its state, the views its build
/// mounted and what their nodes are in their source.
struct Window<'a> {
    ws: &'a WindowState,
    mounts: &'a [MountRecord],
    sources: Option<&'a SourceTable>,
}

impl Window<'_> {
    /// The window's census, its nodes named by the views it mounts.
    fn census(&self) -> WindowCensus {
        let store = &self.ws.store;
        let nodes: Vec<Vec<Option<NodeId>>> = self
            .mounts
            .iter()
            .map(|mount| {
                if mount.nodes.is_empty() {
                    static_nodes(store, mount.root, mount.statics)
                } else {
                    mount.nodes.clone()
                }
            })
            .collect();
        let hosts: Vec<Option<std::cell::Ref<'_, ViewHost>>> = self
            .mounts
            .iter()
            .map(|mount| mount.host.as_ref().map(|host| host.borrow()))
            .collect();
        let views: Vec<CensusView<'_>> = self
            .mounts
            .iter()
            .zip(&nodes)
            .zip(&hosts)
            .map(|((mount, nodes), host)| CensusView {
                file: mount.file,
                source_hash: mount.source_hash,
                nodes,
                host: host.as_deref(),
            })
            .collect();
        inspect::census(store, self.ws.root, &views)
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
