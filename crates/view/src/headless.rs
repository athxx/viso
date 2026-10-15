//! How a tool launches a headless build: the surface every window gets, how
//! many frames the run lasts, and the UI scenario it plays, passed in the
//! environment; and what the build reports of the scenario.
//!
//! A headless build reads them as it starts; a run without them gets the
//! default surface and lasts until the app settles. A run handed a view
//! package mounts it alone instead of the app's view.
//!
//! A UI scenario ([`Script`]) is text, one step a line: input aimed at a node
//! found by its accessible label (and role), waits, and expectations on what
//! the window shows. The build plays it against its first window, each step
//! once the app has settled, and writes one [`Reported`] line per step.

use viso_ende::{JsonValue, JsonWriter};
use viso_ui::semantics::Role;

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

/// A node, by its accessible label and, optionally, role: the first such
/// node shown, in tree order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selector {
    pub role: Option<Role>,
    pub label: String,
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(role) = self.role {
            write!(f, "{} ", crate::source_name(role))?;
        }
        write!(f, "{:?}", self.label)
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

    /// `"<label>"` or `<role> "<label>"`.
    fn selector(&mut self) -> Result<Selector, String> {
        let role = if self.at_quote() {
            None
        } else {
            let word = self.required("a node: `\"<label>\"` or `<role> \"<label>\"`")?;
            Some(
                crate::source_role(&word)
                    .or_else(|| Role::from_label(&word))
                    .ok_or_else(|| {
                        format!(
                            "unknown role `{word}`: one of {}",
                            crate::SOURCE_ROLES.join(", ")
                        )
                    })?,
            )
        };
        let label = self.quoted("a node's label")?;
        Ok(Selector { role, label })
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
        let line = value.get("line")?.as_f64()? as u32;
        match value.get("status")?.as_str()? {
            "pass" => Some(Reported::Passed { line }),
            "fail" => Some(Reported::Failed {
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

#[cfg(test)]
mod tests {
    use super::*;

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
        Selector {
            role,
            label: label.into(),
        }
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
            Reported::Done { frames: 412 },
        ] {
            assert_eq!(Reported::from_line(&reported.to_line()), Some(reported));
        }
        assert_eq!(Reported::from_line("not json"), None);
    }
}
