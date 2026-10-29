//! How a tool launches a headless build: the surface every window gets, how
//! many frames the run lasts, and the UI scenario it plays, passed in the
//! environment; and what the build reports of the scenario.
//!
//! A headless build reads them as it starts; a run without them gets the
//! default surface and lasts until the app settles. A run handed a view
//! package mounts it alone instead of the app's view.
//!
//! A UI scenario ([`Script`]) is text, one step a line: input aimed at a node
//! found by its accessible label (and role) or by an inspector selector,
//! waits, and expectations on what the window shows. The build plays it
//! against its first window, each step once the app has settled, and writes
//! one [`Reported`] line per step.
//!
//! A run handed a [`GameSheet`] ([`GAME_SHEET_ENV`]) mounts no view: it runs
//! the sheet's game on its tape and writes the frames the sheet asks for as
//! one image.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, JsonValue, JsonWriter};
use viso_schema::Role;

/// The environment variable the surface is passed in (see
/// [`HeadlessSurface::to_env`]).
pub const SURFACE_ENV: &str = "VISO_HEADLESS_SURFACE";

/// The environment variable the frame bound is passed in, a count of frames:
/// the run ends after them even if the app keeps asking for more.
pub const FRAMES_ENV: &str = "VISO_HEADLESS_FRAMES";

/// The environment variable the path of the UI scenario to play is passed in.
pub const SCRIPT_ENV: &str = "VISO_HEADLESS_SCRIPT";

/// The environment variable the path the run reports the scenario to is
/// passed in.
pub const REPORT_ENV: &str = "VISO_HEADLESS_REPORT";

/// The environment variable the directory a run writes the snapshots its
/// scenario takes to is passed in: `<name>.png`, the image, and
/// `<name>.tree`, the window's tree, layout, paint and batches as text.
pub const SNAPSHOT_ENV: &str = "VISO_HEADLESS_SNAPSHOTS";

/// The environment variable the path of a [`ViewPackage`](crate::ViewPackage)
/// is passed in: the run mounts it alone in its first window, in place of
/// the app's view, as a tool captures one component.
pub const VIEW_ENV: &str = "VISO_HEADLESS_VIEW";

/// The environment variable that runs the view [`VIEW_ENV`] names as a
/// preview of generated code: with the preview grant, and the capabilities
/// this passes, separated by commas (none when empty), in place of its
/// package's grant; each fault it raises is reported ([`Reported::Fault`]).
pub const GRANT_ENV: &str = "VISO_HEADLESS_GRANT";

/// The environment variable the path of a
/// [`SourceTable`](crate::inspect::SourceTable) is passed in: what the nodes
/// of the views the build mounts are in their source, which a scenario's
/// inspector selectors select by.
pub const SOURCES_ENV: &str = "VISO_HEADLESS_SOURCES";

/// The environment variable a path is passed in where the run writes its
/// [`Census`](crate::inspect::Census) once the scenario ends.
pub const CENSUS_ENV: &str = "VISO_HEADLESS_CENSUS";

/// The environment variable a path is passed in where the run writes the
/// [`WindowDetail`](crate::inspect::WindowDetail) of its first window once the
/// scenario ends: the detail of every node, and its last frame.
pub const DETAIL_ENV: &str = "VISO_HEADLESS_DETAIL";

/// The environment variable a path is passed in where the run writes the
/// [`EventLog`](crate::inspect::EventLog) of its first window once the
/// scenario ends: every input routed, focus move and dirty mark from its
/// start.
pub const EVENTS_ENV: &str = "VISO_HEADLESS_EVENTS";

/// How many frames a step waits for the app to settle by default: two
/// seconds of its clock.
pub const SETTLE_FRAMES: u32 = 120;

/// The largest edge of a surface, in logical pixels.
pub const MAX_EDGE: u32 = 16384;

/// The largest scale of a surface.
pub const MAX_SCALE: f64 = 8.0;

/// Every window's surface in a headless run: its size in logical pixels and
/// its device pixels per logical pixel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeadlessSurface {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

impl Default for HeadlessSurface {
    /// 1280 × 720 at 1x.
    fn default() -> Self {
        HeadlessSurface {
            width: 1280,
            height: 720,
            scale: 1.0,
        }
    }
}

impl HeadlessSurface {
    /// Whether every part is in range: each edge 1 to [`MAX_EDGE`], the
    /// scale above 0 and at most [`MAX_SCALE`].
    pub fn is_valid(&self) -> bool {
        (1..=MAX_EDGE).contains(&self.width)
            && (1..=MAX_EDGE).contains(&self.height)
            && self.scale > 0.0
            && self.scale <= MAX_SCALE
    }

    /// The surface as [`SURFACE_ENV`] spells it: `<width>x<height>@<scale>`.
    pub fn to_env(self) -> String {
        format!("{}x{}@{}", self.width, self.height, self.scale)
    }

    /// The surface `value` spells, `None` unless it is valid.
    pub fn from_env(value: &str) -> Option<HeadlessSurface> {
        let (size, scale) = value.trim().split_once('@')?;
        let (width, height) = size.split_once('x')?;
        let surface = HeadlessSurface {
            width: width.parse().ok()?,
            height: height.parse().ok()?,
            scale: scale.parse().ok()?,
        };
        surface.is_valid().then_some(surface)
    }
}

/// A UI scenario: the surface it asks for, how long each step waits for the
/// app to settle, and its steps.
#[derive(Debug, Clone, PartialEq)]
pub struct Script {
    /// `surface <width>x<height>@<scale>`, over the project's.
    pub surface: Option<HeadlessSurface>,
    /// `settle <frames>`: at most this many frames before each step, while
    /// the app still animates; [`SETTLE_FRAMES`] when not given.
    pub settle: u32,
    pub steps: Vec<Step>,
}

/// One step of a scenario, with the line it is on.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    /// 1-based.
    pub line: u32,
    pub action: Action,
}

/// What a step does.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// `click <selector>`: press and release the primary button at the
    /// node's center.
    Click(Selector),
    /// `hover <selector>`: move the pointer to the node's center.
    Hover(Selector),
    /// `scroll <selector> <dx> <dy>`: scroll by logical pixels over the node.
    Scroll(Selector, f64, f64),
    /// `type "<text>"`: commit text to the focused node.
    Type(String),
    /// `key <chord>`: press and release a key with its modifiers.
    Key(Chord),
    /// `wait <frames>`: let that many frames run.
    Wait(u32),
    /// `expect …`.
    Expect(Expectation),
    /// `expect snapshot "<name>"`: capture the window as the snapshot
    /// `name`, which the tool that launched the run holds to its golden.
    Snapshot(String),
}

/// Whether `name` can name a snapshot: letters, digits, `_` and `-`, as its
/// files are named after it.
pub fn is_snapshot_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A node: the first one shown, in tree order, that the selector finds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selector {
    /// `"<label>"` or `<role> "<label>"`: by its accessible label and,
    /// optionally, role.
    Label { role: Option<Role>, label: String },
    /// `(<selector>)`: by an inspector selector, over what the nodes are in
    /// their source.
    Query(crate::inspect::Selector),
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Selector::Label { role, label } => {
                if let Some(role) = role {
                    write!(f, "{} ", viso_schema::view::source_name(*role))?;
                }
                write!(f, "{label:?}")
            }
            Selector::Query(selector) => write!(f, "({selector})"),
        }
    }
}

/// What an `expect` step asks of the window.
#[derive(Debug, Clone, PartialEq)]
pub enum Expectation {
    /// `expect <selector>`: such a node is shown.
    Shown(Selector),
    /// `expect no <selector>`: none is.
    Absent(Selector),
    /// `expect <selector> <state>`: one is shown, and in that state.
    State(Selector, State),
}

/// A node's state an expectation checks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Focused,
    Checked,
    Unchecked,
    Selected,
    Expanded,
    Collapsed,
    /// `value <number>`: a slider's value.
    Value(f32),
}

impl State {
    /// How the state is written.
    pub fn describe(self) -> String {
        match self {
            State::Focused => "focused".into(),
            State::Checked => "checked".into(),
            State::Unchecked => "unchecked".into(),
            State::Selected => "selected".into(),
            State::Expanded => "expanded".into(),
            State::Collapsed => "collapsed".into(),
            State::Value(value) => format!("value {value}"),
        }
    }
}

/// A key and the modifiers held with it: `Enter`, `Shift+Tab`,
/// `Primary+A` (Command on Apple systems, Control elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub key: Key,
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command / Windows / Super key.
    pub logo: bool,
    /// The platform's accelerator modifier.
    pub primary: bool,
}

/// The keys a scenario presses, by their names in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Escape,
    Tab,
    Space,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// `A` to `Z`.
    Letter(char),
    /// `0` to `9`.
    Digit(u8),
    /// `F1` to `F12`.
    Function(u8),
}

impl Key {
    /// The key `name` names.
    pub fn from_name(name: &str) -> Option<Key> {
        let named = match name {
            "Enter" => Key::Enter,
            "Escape" => Key::Escape,
            "Tab" => Key::Tab,
            "Space" => Key::Space,
            "Backspace" => Key::Backspace,
            "Delete" => Key::Delete,
            "Left" => Key::Left,
            "Right" => Key::Right,
            "Up" => Key::Up,
            "Down" => Key::Down,
            "Home" => Key::Home,
            "End" => Key::End,
            "PageUp" => Key::PageUp,
            "PageDown" => Key::PageDown,
            _ => {
                let mut chars = name.chars();
                return match (chars.next(), chars.as_str()) {
                    (Some(c @ 'A'..='Z'), "") => Some(Key::Letter(c)),
                    (Some(c @ '0'..='9'), "") => Some(Key::Digit(c as u8 - b'0')),
                    (Some('F'), n) => n
                        .parse::<u8>()
                        .ok()
                        .filter(|n| (1..=12).contains(n))
                        .map(Key::Function),
                    _ => None,
                };
            }
        };
        Some(named)
    }
}

/// Why a scenario does not parse: the line, and what is wrong there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptError {
    /// 1-based.
    pub line: u32,
    pub message: String,
}

impl Script {
    /// Parses scenario text.
    pub fn parse(text: &str) -> Result<Script, ScriptError> {
        let mut script = Script {
            surface: None,
            settle: SETTLE_FRAMES,
            steps: Vec::new(),
        };
        for (index, raw) in text.lines().enumerate() {
            let line = index as u32 + 1;
            let fail = |message: String| ScriptError { line, message };
            let mut words = Words::new(raw);
            let Some(verb) = words.word().map_err(fail)? else {
                continue;
            };
            let action = match verb.as_str() {
                "surface" | "settle" if !script.steps.is_empty() => {
                    return Err(fail(format!("`{verb}` comes before the first step")));
                }
                "surface" => {
                    let value = words
                        .required("a surface, `<width>x<height>@<scale>`")
                        .map_err(fail)?;
                    script.surface = Some(HeadlessSurface::from_env(&value).ok_or_else(|| {
                        fail(format!(
                            "`{value}` is not a surface: `<width>x<height>@<scale>`, each edge 1 to {MAX_EDGE} and the scale above 0 and at most {MAX_SCALE}"
                        ))
                    })?);
                    words.end().map_err(fail)?;
                    continue;
                }
                "settle" => {
                    script.settle = words.frames().map_err(fail)?;
                    words.end().map_err(fail)?;
                    continue;
                }
                "click" => Action::Click(words.selector().map_err(fail)?),
                "hover" => Action::Hover(words.selector().map_err(fail)?),
                "scroll" => {
                    let selector = words.selector().map_err(fail)?;
                    let dx = words.number("the horizontal distance").map_err(fail)?;
                    let dy = words.number("the vertical distance").map_err(fail)?;
                    Action::Scroll(selector, dx, dy)
                }
                "type" => Action::Type(words.quoted("the text to type").map_err(fail)?),
                "key" => Action::Key(words.chord().map_err(fail)?),
                "wait" => Action::Wait(words.frames().map_err(fail)?),
                "expect" if words.snapshot().map_err(fail)? => {
                    let name = words.quoted("the snapshot's name").map_err(fail)?;
                    if !is_snapshot_name(&name) {
                        return Err(fail(format!(
                            "`{name}` cannot name a snapshot: letters, digits, `_` and `-`"
                        )));
                    }
                    let taken = script
                        .steps
                        .iter()
                        .any(|step| step.action == Action::Snapshot(name.clone()));
                    if taken {
                        return Err(fail(format!(
                            "the scenario takes a snapshot `{name}` already"
                        )));
                    }
                    Action::Snapshot(name)
                }
                "expect" => Action::Expect(words.expectation().map_err(fail)?),
                other => {
                    return Err(fail(format!(
                        "unknown step `{other}`: one of `click`, `hover`, `scroll`, `type`, `key`, `wait`, `expect`, or the header `surface` or `settle`"
                    )));
                }
            };
            words.end().map_err(fail)?;
            script.steps.push(Step { line, action });
        }
        Ok(script)
    }
}

/// The words of one line: bare words and quoted strings, up to a `#`
/// comment.
struct Words<'a> {
    rest: &'a str,
}

impl<'a> Words<'a> {
    fn new(line: &'a str) -> Self {
        Words { rest: line }
    }

    /// Whether the next word is quoted.
    fn at_quote(&mut self) -> bool {
        self.rest = self.rest.trim_start();
        self.rest.starts_with('"')
    }

    /// The next bare word or quoted string, `None` at the end of the line.
    fn word(&mut self) -> Result<Option<String>, String> {
        self.rest = self.rest.trim_start();
        if self.rest.is_empty() || self.rest.starts_with('#') {
            return Ok(None);
        }
        if self.at_quote() {
            return self.string().map(Some);
        }
        let end = self
            .rest
            .find(|c: char| c.is_whitespace() || c == '"' || c == '#')
            .unwrap_or(self.rest.len());
        let (word, rest) = self.rest.split_at(end);
        self.rest = rest;
        Ok(Some(word.to_owned()))
    }

    /// A quoted string, with `\"`, `\\`, `\n` and `\t` escapes.
    fn string(&mut self) -> Result<String, String> {
        let mut chars = self.rest[1..].char_indices();
        let mut text = String::new();
        while let Some((at, c)) = chars.next() {
            match c {
                '"' => {
                    self.rest = &self.rest[at + 2..];
                    return Ok(text);
                }
                '\\' => match chars.next().map(|(_, c)| c) {
                    Some('"') => text.push('"'),
                    Some('\\') => text.push('\\'),
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some(other) => return Err(format!("unknown escape `\\{other}`")),
                    None => break,
                },
                c => text.push(c),
            }
        }
        Err("the string is not closed".into())
    }

    fn required(&mut self, what: &str) -> Result<String, String> {
        self.word()?.ok_or_else(|| format!("missing {what}"))
    }

    fn quoted(&mut self, what: &str) -> Result<String, String> {
        if !self.at_quote() {
            return Err(format!("{what} is a quoted string"));
        }
        self.string()
    }

    fn end(&mut self) -> Result<(), String> {
        match self.word()? {
            None => Ok(()),
            Some(extra) => Err(format!("unexpected `{extra}` at the end of the step")),
        }
    }

    fn frames(&mut self) -> Result<u32, String> {
        let word = self.required("a count of frames")?;
        word.parse::<u32>()
            .ok()
            .filter(|&n| n > 0)
            .ok_or_else(|| format!("`{word}` is not a count of frames"))
    }

    fn number(&mut self, what: &str) -> Result<f64, String> {
        let word = self.required(what)?;
        word.parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("`{word}` is not a number"))
    }

    /// `"<label>"`, `<role> "<label>"` or `(<selector>)`.
    fn selector(&mut self) -> Result<Selector, String> {
        self.rest = self.rest.trim_start();
        if self.rest.starts_with('(') {
            return self.query();
        }
        let role = if self.at_quote() {
            None
        } else {
            let word = self.required("a node: `\"<label>\"` or `<role> \"<label>\"`")?;
            Some(
                viso_schema::view::source_role(&word)
                    .or_else(|| Role::from_label(&word))
                    .ok_or_else(|| {
                        format!(
                            "unknown role `{word}`: one of {}",
                            viso_schema::view::SOURCE_ROLES.join(", ")
                        )
                    })?,
            )
        };
        let label = self.quoted("a node's label")?;
        Ok(Selector::Label { role, label })
    }

    /// `(<selector>)`: up to the parenthesis that closes it, outside quotes
    /// and brackets.
    fn query(&mut self) -> Result<Selector, String> {
        let (mut quoted, mut escaped, mut brackets) = (false, false, 0u32);
        let mut close = None;
        for (at, c) in self.rest.char_indices().skip(1) {
            match c {
                _ if escaped => escaped = false,
                '\\' if quoted => escaped = true,
                '"' => quoted = !quoted,
                '[' if !quoted => brackets += 1,
                ']' if !quoted => brackets = brackets.saturating_sub(1),
                ')' if !quoted && brackets == 0 => {
                    close = Some(at);
                    break;
                }
                _ => {}
            }
        }
        let close = close.ok_or("the selector's `(` is not closed")?;
        let text = &self.rest[1..close];
        self.rest = &self.rest[close + 1..];
        crate::inspect::Selector::parse(text)
            .map(Selector::Query)
            .map_err(|error| format!("`({text})` is not a selector: {error}"))
    }

    fn chord(&mut self) -> Result<Chord, String> {
        let word = self.required("a key, like `Enter` or `Shift+Tab`")?;
        let mut chord = Chord {
            key: Key::Enter,
            shift: false,
            control: false,
            alt: false,
            logo: false,
            primary: false,
        };
        let mut parts: Vec<&str> = word.split('+').collect();
        let key = parts.pop().unwrap_or_default();
        for modifier in parts {
            let held = match modifier {
                "Shift" => &mut chord.shift,
                "Ctrl" => &mut chord.control,
                "Alt" => &mut chord.alt,
                "Logo" => &mut chord.logo,
                "Primary" => &mut chord.primary,
                other => {
                    return Err(format!(
                        "unknown modifier `{other}`: one of `Shift`, `Ctrl`, `Alt`, `Logo`, `Primary`"
                    ));
                }
            };
            *held = true;
        }
        chord.key = Key::from_name(key).ok_or_else(|| {
            format!(
                "unknown key `{key}`: a letter, a digit, `F1` to `F12`, or one of `Enter`, `Escape`, `Tab`, `Space`, `Backspace`, `Delete`, `Left`, `Right`, `Up`, `Down`, `Home`, `End`, `PageUp`, `PageDown`"
            )
        })?;
        Ok(chord)
    }

    /// Whether the next word is `snapshot`, taking it if so.
    fn snapshot(&mut self) -> Result<bool, String> {
        if self.at_quote() {
            return Ok(false);
        }
        let save = self.rest;
        if self.word()?.as_deref() == Some("snapshot") {
            return Ok(true);
        }
        self.rest = save;
        Ok(false)
    }

    fn expectation(&mut self) -> Result<Expectation, String> {
        let absent = if self.at_quote() {
            false
        } else {
            let save = self.rest;
            match self.word()?.as_deref() {
                Some("no") => true,
                _ => {
                    self.rest = save;
                    false
                }
            }
        };
        let selector = self.selector()?;
        if absent {
            return Ok(Expectation::Absent(selector));
        }
        let Some(word) = self.word()? else {
            return Ok(Expectation::Shown(selector));
        };
        let state = match word.as_str() {
            "focused" => State::Focused,
            "checked" => State::Checked,
            "unchecked" => State::Unchecked,
            "selected" => State::Selected,
            "expanded" => State::Expanded,
            "collapsed" => State::Collapsed,
            "value" => State::Value(self.number("the value")? as f32),
            other => {
                return Err(format!(
                    "unknown state `{other}`: one of `focused`, `checked`, `unchecked`, `selected`, `expanded`, `collapsed`, `value <number>`"
                ));
            }
        };
        Ok(Expectation::State(selector, state))
    }
}

/// One line of a run's report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reported {
    /// The step on `line` passed.
    Passed { line: u32 },
    /// The step on `line` failed, and why; no step after it runs.
    Failed { line: u32, message: String },
    /// The step on `line` captured the snapshot `name`, `width` × `height`
    /// device pixels, into the snapshot directory.
    Snapshot {
        line: u32,
        name: String,
        width: u32,
        height: u32,
    },
    /// The step on `line` was not played here, and why: a snapshot on a
    /// target without the software raster.
    Skipped { line: u32, message: String },
    /// The view a granted run mounts ([`GRANT_ENV`]) faulted with the
    /// diagnostic `code`: the transaction rolled back and the view ran on.
    /// `span` is where in its source, as byte offsets, when the fault was in
    /// code.
    Fault {
        code: String,
        message: String,
        span: Option<(u32, u32)>,
    },
    /// The run played every step it could, in `frames` frames.
    Done { frames: u64 },
}

impl Reported {
    /// The line as written: one JSON object.
    pub fn to_line(&self) -> String {
        let mut w = JsonWriter::new();
        w.begin_object();
        match self {
            Reported::Passed { line } => {
                w.name("line");
                w.uint(u64::from(*line));
                w.name("status");
                w.string("pass");
            }
            Reported::Failed { line, message } => {
                w.name("line");
                w.uint(u64::from(*line));
                w.name("status");
                w.string("fail");
                w.name("message");
                w.string(message);
            }
            Reported::Skipped { line, message } => {
                w.name("line");
                w.uint(u64::from(*line));
                w.name("status");
                w.string("skip");
                w.name("message");
                w.string(message);
            }
            Reported::Snapshot {
                line,
                name,
                width,
                height,
            } => {
                w.name("line");
                w.uint(u64::from(*line));
                w.name("status");
                w.string("snapshot");
                w.name("name");
                w.string(name);
                w.name("width");
                w.uint(u64::from(*width));
                w.name("height");
                w.uint(u64::from(*height));
            }
            Reported::Fault {
                code,
                message,
                span,
            } => {
                w.name("status");
                w.string("fault");
                w.name("code");
                w.string(code);
                w.name("message");
                w.string(message);
                if let Some((start, end)) = span {
                    w.name("start");
                    w.uint(u64::from(*start));
                    w.name("end");
                    w.uint(u64::from(*end));
                }
            }
            Reported::Done { frames } => {
                w.name("done");
                w.uint(*frames);
            }
        }
        w.end_object();
        w.into_string()
    }

    /// The report line `text` is, if it is one.
    pub fn from_line(text: &str) -> Option<Reported> {
        let value = JsonValue::parse(text).ok()?;
        if let Some(frames) = value.get("done").and_then(JsonValue::as_f64) {
            return Some(Reported::Done {
                frames: frames as u64,
            });
        }
        if value.get("status")?.as_str()? == "fault" {
            let offset = |name| {
                value
                    .get(name)
                    .and_then(JsonValue::as_f64)
                    .map(|v| v as u32)
            };
            return Some(Reported::Fault {
                code: value.get("code")?.as_str()?.to_owned(),
                message: value.get("message")?.as_str()?.to_owned(),
                span: offset("start").zip(offset("end")),
            });
        }
        let line = value.get("line")?.as_f64()? as u32;
        match value.get("status")?.as_str()? {
            "pass" => Some(Reported::Passed { line }),
            "fail" => Some(Reported::Failed {
                line,
                message: value.get("message")?.as_str()?.to_owned(),
            }),
            "skip" => Some(Reported::Skipped {
                line,
                message: value.get("message")?.as_str()?.to_owned(),
            }),
            "snapshot" => Some(Reported::Snapshot {
                line,
                name: value.get("name")?.as_str()?.to_owned(),
                width: value.get("width")?.as_f64()? as u32,
                height: value.get("height")?.as_f64()? as u32,
            }),
            _ => None,
        }
    }
}

/// The environment variable the path of a [`GameSheet`] is passed in: the
/// run renders the sheet and ends, mounting no view.
pub const GAME_SHEET_ENV: &str = "VISO_HEADLESS_GAME_SHEET";

/// How many frames a sheet shows by default.
pub const SHEET_TILES: u32 = 12;

/// The size of a sheet's tile by default, in logical pixels: 16:9.
pub const SHEET_TILE: HeadlessSurface = HeadlessSurface {
    width: 480,
    height: 270,
    scale: 1.0,
};

/// A game run drawn as a sheet of frames: the game's module, run on its
/// tape for `frames` frames of one fixed tick each, with `tiles` of the
/// frames, evenly spaced and ending on the last, laid out in rows.
#[derive(Debug, Clone, PartialEq)]
pub struct GameSheet {
    /// The game's behavior module, in its wire form.
    pub module: Vec<u8>,
    /// The input tape it runs, in its binary form: its seed and tick rate.
    pub tape: Vec<u8>,
    pub frames: u64,
    pub tiles: u32,
    /// The size of each tile.
    pub tile: HeadlessSurface,
    /// Where the image is written, a PNG.
    pub output: String,
}

impl GameSheet {
    /// The ticks the tiles show, in order: the frame after each.
    pub fn ticks(&self) -> impl Iterator<Item = u64> + '_ {
        let n = u64::from(self.tiles).min(self.frames);
        (1..=n).map(move |k| (k * self.frames).div_ceil(n) - 1)
    }

    /// How many tiles a row holds.
    pub fn columns(&self) -> u32 {
        let n = u64::from(self.tiles).min(self.frames).max(1) as u32;
        (1..=n).find(|c| c * c >= n).unwrap_or(n)
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// # Errors
    ///
    /// When `bytes` is not a sheet this protocol writes.
    pub fn from_bytes(bytes: &[u8]) -> Result<GameSheet, DecodeError> {
        GameSheet::decode_from_slice(bytes)
    }
}

impl Encode for GameSheet {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_bytes(&self.module);
        enc.write_bytes(&self.tape);
        enc.write_varint(self.frames);
        enc.write_varint(u64::from(self.tiles));
        enc.write_str(&self.tile.to_env());
        enc.write_str(&self.output);
    }
}

impl Decode for GameSheet {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let module = dec.read_bytes()?.to_vec();
        let tape = dec.read_bytes()?.to_vec();
        let frames = dec.read_varint()?;
        let offset = dec.position();
        let tiles = u32::try_from(dec.read_varint()?)
            .ok()
            .filter(|&t| t > 0)
            .ok_or(DecodeError::Malformed { offset })?;
        let offset = dec.position();
        let tile =
            HeadlessSurface::from_env(dec.read_str()?).ok_or(DecodeError::Malformed { offset })?;
        let output = dec.read_str()?.to_owned();
        Ok(GameSheet {
            module,
            tape,
            frames,
            tiles,
            tile,
            output,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_game_sheet_round_trips_and_spaces_its_ticks() {
        let sheet = GameSheet {
            module: vec![1, 2, 3],
            tape: vec![4, 5],
            frames: 120,
            tiles: 12,
            tile: SHEET_TILE,
            output: "out/sheet.png".into(),
        };
        assert_eq!(GameSheet::from_bytes(&sheet.to_bytes()), Ok(sheet.clone()));
        let ticks: Vec<u64> = sheet.ticks().collect();
        assert_eq!(ticks, [9, 19, 29, 39, 49, 59, 69, 79, 89, 99, 109, 119]);
        assert_eq!(sheet.columns(), 4);

        let short = GameSheet {
            frames: 5,
            ..sheet.clone()
        };
        assert_eq!(short.ticks().collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
        assert_eq!(short.columns(), 3);
        let uneven = GameSheet {
            frames: 100,
            tiles: 3,
            ..sheet.clone()
        };
        assert_eq!(uneven.ticks().collect::<Vec<_>>(), [33, 66, 99]);

        let mut bad = sheet.clone();
        bad.tiles = 0;
        assert!(GameSheet::from_bytes(&bad.to_bytes()).is_err());
    }

    #[test]
    fn a_surface_round_trips_through_the_environment() {
        for surface in [
            HeadlessSurface::default(),
            HeadlessSurface {
                width: 390,
                height: 844,
                scale: 3.0,
            },
            HeadlessSurface {
                width: 412,
                height: 915,
                scale: 2.625,
            },
        ] {
            assert_eq!(HeadlessSurface::from_env(&surface.to_env()), Some(surface));
        }
        assert_eq!(HeadlessSurface::default().to_env(), "1280x720@1");
        for bad in [
            "",
            "1280x720",
            "1280@1",
            "0x720@1",
            "1280x720@0",
            "1280x720@9",
            "1280x99999@1",
            "1280x720@NaN",
            "-1x720@1",
        ] {
            assert_eq!(HeadlessSurface::from_env(bad), None, "{bad:?}");
        }
    }

    fn selector(role: Option<Role>, label: &str) -> Selector {
        Selector::Label {
            role,
            label: label.into(),
        }
    }

    #[test]
    fn a_node_is_found_by_an_inspector_selector_in_parentheses() {
        let script = Script::parse(
            "click (Column > #save)   # a comment\n\
             expect ([role=button][label=\"a ) b\"]) checked\n",
        )
        .unwrap();
        let query = |text: &str| Selector::Query(crate::inspect::Selector::parse(text).unwrap());
        assert_eq!(
            script.steps[0].action,
            Action::Click(query("Column > #save"))
        );
        assert_eq!(
            script.steps[1].action,
            Action::Expect(Expectation::State(
                query("[role=button][label=\"a ) b\"]"),
                State::Checked
            ))
        );
        assert_eq!(query("Column > #save").to_string(), "(Column > #save)");
        let error = Script::parse("click (Button\n").unwrap_err();
        assert!(error.message.contains("not closed"), "{}", error.message);
        let error = Script::parse("hover ([colour=red])\n").unwrap_err();
        assert!(
            error.message.contains("no attribute `colour`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_scenario_is_a_step_a_line() {
        let script = Script::parse(
            "# The counter counts.\n\
             surface 390x844@3\n\
             settle 30\n\
             \n\
             click button \"Increment\"   # once\n\
             expect \"Count: 1\"\n\
             hover \"Help\"\n\
             scroll \"List\" 0 -240.5\n\
             type \"a \\\"quoted\\\" word\"\n\
             key Primary+Shift+Z\n\
             key F5\n\
             wait 10\n\
             expect no \"Error\"\n\
             expect checkbox \"Agree\" checked\n\
             expect slider \"Volume\" value 0.5\n\
             expect snapshot \"home-wide\"\n\
             expect \"snapshot\"\n",
        )
        .unwrap();
        assert_eq!(
            script.surface,
            Some(HeadlessSurface {
                width: 390,
                height: 844,
                scale: 3.0
            })
        );
        assert_eq!(script.settle, 30);
        let actions: Vec<(u32, Action)> = script
            .steps
            .into_iter()
            .map(|step| (step.line, step.action))
            .collect();
        assert_eq!(
            actions,
            [
                (5, Action::Click(selector(Some(Role::Button), "Increment"))),
                (
                    6,
                    Action::Expect(Expectation::Shown(selector(None, "Count: 1")))
                ),
                (7, Action::Hover(selector(None, "Help"))),
                (8, Action::Scroll(selector(None, "List"), 0.0, -240.5)),
                (9, Action::Type("a \"quoted\" word".into())),
                (
                    10,
                    Action::Key(Chord {
                        key: Key::Letter('Z'),
                        shift: true,
                        control: false,
                        alt: false,
                        logo: false,
                        primary: true,
                    })
                ),
                (
                    11,
                    Action::Key(Chord {
                        key: Key::Function(5),
                        shift: false,
                        control: false,
                        alt: false,
                        logo: false,
                        primary: false,
                    })
                ),
                (12, Action::Wait(10)),
                (
                    13,
                    Action::Expect(Expectation::Absent(selector(None, "Error")))
                ),
                (
                    14,
                    Action::Expect(Expectation::State(
                        selector(Some(Role::CheckBox), "Agree"),
                        State::Checked
                    ))
                ),
                (
                    15,
                    Action::Expect(Expectation::State(
                        selector(Some(Role::Slider), "Volume"),
                        State::Value(0.5)
                    ))
                ),
                (16, Action::Snapshot("home-wide".into())),
                (
                    17,
                    Action::Expect(Expectation::Shown(selector(None, "snapshot")))
                ),
            ]
        );
        assert_eq!(Script::parse("").unwrap().settle, SETTLE_FRAMES);
    }

    #[test]
    fn a_step_that_does_not_parse_names_its_line() {
        for (text, line, said) in [
            ("jump \"x\"", 1, "unknown step `jump`"),
            ("click", 1, "missing a node"),
            ("\nclick widget \"x\"", 2, "unknown role `widget`"),
            ("click \"open", 1, "not closed"),
            ("click \"x\" now", 1, "unexpected `now`"),
            ("key Hyper+A", 1, "unknown modifier `Hyper`"),
            ("key F13", 1, "unknown key `F13`"),
            ("type Hello", 1, "is a quoted string"),
            ("wait 0", 1, "not a count of frames"),
            ("scroll \"x\" 1", 1, "missing the vertical distance"),
            ("expect \"x\" pressed", 1, "unknown state `pressed`"),
            ("click \"x\"\nsurface 1x1@1", 2, "before the first step"),
            ("surface 1280x720", 1, "is not a surface"),
            (
                "expect snapshot",
                1,
                "the snapshot's name is a quoted string",
            ),
            ("expect snapshot \"a b\"", 1, "cannot name a snapshot"),
            (
                "expect snapshot \"a\"\nexpect snapshot \"a\"",
                2,
                "takes a snapshot `a` already",
            ),
        ] {
            let error = Script::parse(text).unwrap_err();
            assert_eq!(error.line, line, "{text:?}");
            assert!(error.message.contains(said), "{text:?}: {}", error.message);
        }
    }

    #[test]
    fn a_report_line_round_trips() {
        for reported in [
            Reported::Passed { line: 3 },
            Reported::Skipped {
                line: 5,
                message: "the snapshot `home` is taken headless".into(),
            },
            Reported::Failed {
                line: 7,
                message: "no node labeled \"Save\"".into(),
            },
            Reported::Snapshot {
                line: 9,
                name: "home".into(),
                width: 2560,
                height: 1440,
            },
            Reported::Fault {
                code: "E6103".into(),
                message: "`clipboard::write_text` needs `clipboard.write`".into(),
                span: Some((120, 151)),
            },
            Reported::Fault {
                code: "E7105".into(),
                message: "internal behavior fault".into(),
                span: None,
            },
            Reported::Done { frames: 412 },
        ] {
            assert_eq!(Reported::from_line(&reported.to_line()), Some(reported));
        }
        assert_eq!(Reported::from_line("not json"), None);
    }
}
