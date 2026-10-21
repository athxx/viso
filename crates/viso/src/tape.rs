//! UI input tapes ([`viso_view::tape`]): recording the input the
//! first window takes while the launch names a tape to record to, and a
//! headless run replaying one.
//!
//! The recorder keeps every frame the window runs, with the time the frame
//! clock advanced by and the input the window took ahead of it, in logical
//! points, and the dev revision each live patch moved the app to; the tape is
//! written as the window closes or the session ends. The player feeds each
//! frame's input through the backend's queue ahead of that frame and holds
//! the frame clock to the recorded time, so the replay runs the same frames
//! at the same times; past the tape, the clock steps one frame of 60 Hz.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use viso_platform::{Instant, WindowId};
use viso_runtime::{FrameClock, InputSample, PointerKind};
use viso_view::tape::{
    BuildId, MAX_FRAMES, MAX_INPUTS, TapeFrame, TapeInput, TapeKey, TapeModifiers, TapePointerKind,
    TapePointerPhase, TapeSurface, UiTape,
};

/// The first window's input being recorded, and where it goes.
pub(crate) struct TapeRecorder {
    /// Where the tape is written; a recorder with none keeps it to be taken.
    path: Option<PathBuf>,
    tape: UiTape,
    /// The window recorded; `None` until the first opens.
    window: Option<WindowId>,
    /// The input taken since the last frame began.
    pending: Vec<TapeInput>,
    /// The scale the window's samples are in, physical pixels a point.
    scale: f32,
    /// Whether the tape reached its bound: it stops recording.
    full: bool,
    written: bool,
}

impl TapeRecorder {
    /// A recorder writing to `path` once the run ends, or keeping the tape.
    pub(crate) fn new(path: Option<PathBuf>) -> TapeRecorder {
        TapeRecorder {
            path,
            tape: UiTape::default(),
            window: None,
            pending: Vec::new(),
            scale: 1.0,
            full: false,
            written: false,
        }
    }

    /// Records `window`, `size` physical pixels at `scale`, if no window is
    /// recorded yet; `code` is the build and the dev revision it runs, when
    /// a dev session names them.
    pub(crate) fn adopt(
        &mut self,
        window: WindowId,
        scale: f32,
        size: (u32, u32),
        code: Option<(BuildId, u64)>,
    ) {
        if self.window.is_some() {
            return;
        }
        self.window = Some(window);
        if let Some((build, revision)) = code {
            self.tape.build = build;
            self.tape.revision = revision;
        }
        self.scale = scale.max(f32::MIN_POSITIVE);
        self.tape.surface = surface(scale, size);
    }

    fn records(&self, window: WindowId) -> bool {
        !self.full && self.window == Some(window)
    }

    fn push(&mut self, input: TapeInput) {
        if self.pending.len() < MAX_INPUTS {
            self.pending.push(input);
        }
    }

    /// `sample` reached its window, in physical pixels.
    pub(crate) fn input(&mut self, sample: &InputSample) {
        if !self.records(sample.window()) {
            return;
        }
        let scale = self.scale;
        let input = match sample {
            InputSample::Pointer(p) => TapeInput::Pointer {
                pointer: p.pointer.0,
                kind: match p.kind {
                    PointerKind::Mouse => TapePointerKind::Mouse,
                    PointerKind::Touch => TapePointerKind::Touch,
                    PointerKind::Pen => TapePointerKind::Pen,
                },
                phase: match p.phase {
                    viso_runtime::PointerPhase::Down => TapePointerPhase::Down,
                    viso_runtime::PointerPhase::Move => TapePointerPhase::Moved,
                    viso_runtime::PointerPhase::Up => TapePointerPhase::Up,
                    viso_runtime::PointerPhase::Leave => TapePointerPhase::Left,
                    viso_runtime::PointerPhase::Cancel => TapePointerPhase::Cancel,
                },
                x: p.x / scale,
                y: p.y / scale,
                pressure: p.pressure,
                buttons: p.buttons,
                modifiers: modifiers(p.modifiers),
            },
            InputSample::Scroll(s) => TapeInput::Scroll {
                x: s.x / scale,
                y: s.y / scale,
                delta_x: s.delta_x / scale,
                delta_y: s.delta_y / scale,
                modifiers: modifiers(s.modifiers),
            },
            InputSample::Key(k) => TapeInput::Key {
                key: match k.key.named_index() {
                    Some(at) => TapeKey::Named(at),
                    None => match k.key {
                        viso_runtime::Key::Other(code) => TapeKey::Other(code),
                        _ => return,
                    },
                },
                pressed: k.pressed,
                repeat: k.repeat,
                modifiers: modifiers(k.modifiers),
            },
            InputSample::Text(t) => TapeInput::Text(t.text.clone()),
            InputSample::ImePreedit(p) => TapeInput::Preedit {
                text: p.text.clone(),
                caret: u32::try_from(p.caret).unwrap_or(u32::MAX),
            },
            InputSample::Paste(t) => TapeInput::Paste(t.text.clone()),
            InputSample::Copy(c) => TapeInput::Copy { cut: c.cut },
        };
        self.push(input);
    }

    /// `window` gained or lost the focus.
    pub(crate) fn focus(&mut self, window: WindowId, focused: bool) {
        if self.records(window) {
            self.push(TapeInput::Focus(focused));
        }
    }

    /// `window` took a new size, in physical pixels, and scale.
    pub(crate) fn resize(&mut self, window: WindowId, scale: f32, size: (u32, u32)) {
        if !self.records(window) {
            return;
        }
        if scale > 0.0 {
            self.scale = scale;
        }
        self.push(TapeInput::Resize(surface(self.scale, size)));
    }

    /// A frame begins, `delta` after the one before.
    pub(crate) fn frame(&mut self, delta: Duration) {
        if self.window.is_none() || self.full {
            return;
        }
        if self.tape.frames.len() == MAX_FRAMES {
            self.full = true;
            return;
        }
        self.tape.frames.push(TapeFrame {
            delta_us: u32::try_from(delta.as_micros()).unwrap_or(u32::MAX),
            inputs: std::mem::take(&mut self.pending),
        });
    }

    /// A live patch moved the app to `revision` in the frame running now.
    #[cfg(feature = "hot-reload")]
    pub(crate) fn patch(&mut self, revision: u64) {
        if self.window.is_none() || self.full {
            return;
        }
        let frame = self.tape.frames.len().saturating_sub(1) as u32;
        self.tape
            .patches
            .push(viso_view::tape::TapePatch { frame, revision });
    }

    /// `window` closed: the tape is written if it is the one recorded.
    pub(crate) fn closed(&mut self, window: WindowId) {
        if self.window == Some(window) {
            self.write();
        }
    }

    /// Writes the tape, once.
    pub(crate) fn write(&mut self) {
        if std::mem::replace(&mut self.written, true) || self.window.is_none() {
            return;
        }
        let Some(path) = &self.path else {
            return;
        };
        if let Err(error) = std::fs::write(path, self.tape.to_bytes()) {
            eprintln!("viso: cannot write the tape `{}`: {error}", path.display());
        }
    }

    /// The tape recorded so far, the input of a frame not yet begun left out.
    pub(crate) fn tape(&self) -> &UiTape {
        &self.tape
    }
}

impl Drop for TapeRecorder {
    fn drop(&mut self) {
        self.write();
    }
}

fn surface(scale: f32, (width, height): (u32, u32)) -> TapeSurface {
    let scale = if scale > 0.0 { scale } else { 1.0 };
    TapeSurface {
        width: width.max(1) as f32 / scale,
        height: height.max(1) as f32 / scale,
        scale,
    }
}

fn modifiers(m: viso_runtime::Modifiers) -> TapeModifiers {
    TapeModifiers::of(m.shift, m.control, m.alt, m.logo)
}

/// The frame clock of a headless run: one frame of 60 Hz a read, or, while
/// a tape replays, a cursor the player sets and a read does not move.
#[cfg(feature = "headless")]
pub(crate) enum HeadlessClock {
    Fixed(viso_runtime::FixedStepClock),
    Tape(TapeClock),
}

#[cfg(feature = "headless")]
impl FrameClock for HeadlessClock {
    fn now(&mut self) -> Instant {
        match self {
            HeadlessClock::Fixed(clock) => clock.now(),
            HeadlessClock::Tape(clock) => clock.now(),
        }
    }

    fn peek(&self) -> Instant {
        match self {
            HeadlessClock::Fixed(clock) => clock.peek(),
            HeadlessClock::Tape(clock) => clock.peek(),
        }
    }
}

/// The cursor a replay holds the frame clock to.
#[derive(Clone)]
pub(crate) struct TapeClock(Rc<Cell<Instant>>);

impl TapeClock {
    pub(crate) fn new(base: Instant) -> TapeClock {
        TapeClock(Rc::new(Cell::new(base)))
    }

    /// Moves the clock on by `by`.
    fn advance(&self, by: Duration) {
        self.0.set(self.0.get() + by);
    }
}

impl FrameClock for TapeClock {
    fn now(&mut self) -> Instant {
        self.0.get()
    }

    fn peek(&self) -> Instant {
        self.0.get()
    }
}

/// One frame of 60 Hz.
pub(crate) const STEP: Duration = Duration::from_nanos(1_000_000_000 / 60);

/// A tape being replayed against the first window.
pub(crate) struct TapePlayer {
    tape: UiTape,
    clock: TapeClock,
    feed: viso_platform::backend::headless::HeadlessFeed,
    window: Option<WindowId>,
    /// The frame the next feed is for.
    next: usize,
    /// The patches the replay has passed.
    passed: usize,
    report: Option<PathBuf>,
}

impl TapePlayer {
    pub(crate) fn new(
        tape: UiTape,
        clock: TapeClock,
        feed: viso_platform::backend::headless::HeadlessFeed,
        report: Option<PathBuf>,
    ) -> TapePlayer {
        TapePlayer {
            tape,
            clock,
            feed,
            window: None,
            next: 0,
            passed: 0,
            report,
        }
    }

    /// The first window opened: the input ahead of the first frame goes in.
    pub(crate) fn adopt(&mut self, window: WindowId) {
        if self.window.is_none() {
            self.window = Some(window);
            self.feed_next(Duration::ZERO);
        }
    }

    /// A frame ended: the next frame's input goes in, the clock moves to its
    /// time, and the next frame is asked for while the tape lasts.
    pub(crate) fn after_frame(&mut self, cx: &mut viso_runtime::RuntimeCx<'_>) {
        let Some(window) = self.window else {
            return;
        };
        let frames = self.tape.frames.len();
        while self.passed < self.tape.patches.len()
            && (self.tape.patches[self.passed].frame as usize) < self.next
        {
            self.passed += 1;
        }
        if self.next >= frames {
            if self.next == frames {
                self.next += 1;
                self.finish();
            }
            self.clock.advance(STEP);
            return;
        }
        let delta = Duration::from_micros(u64::from(self.tape.frames[self.next].delta_us));
        self.feed_next(delta);
        cx.request_redraw(window);
    }

    /// Feeds the input of frame `next` and moves the clock to its time.
    fn feed_next(&mut self, delta: Duration) {
        let Some(window) = self.window else {
            return;
        };
        let Some(frame) = self.tape.frames.get(self.next) else {
            return;
        };
        self.clock.advance(delta);
        for input in &frame.inputs {
            for event in raw(window, input) {
                self.feed.push(event);
            }
        }
        self.next += 1;
    }

    /// The replay reached the tape's end: its report line.
    fn finish(&mut self) {
        let Some(path) = &self.report else {
            return;
        };
        let mut json = viso_ende::JsonWriter::new();
        json.begin_object();
        json.name("kind");
        json.string("tape");
        json.name("frames");
        json.uint(self.tape.frames.len() as u64);
        json.name("build");
        json.string(&self.tape.build.to_hex());
        json.name("revisions");
        json.begin_array();
        for revision in self.tape.revisions() {
            json.uint(revision);
        }
        json.end_array();
        json.end_object();
        let mut line = json.into_string();
        line.push('\n');
        if let Err(error) = std::fs::write(path, line) {
            eprintln!(
                "viso: cannot write the report `{}`: {error}",
                path.display()
            );
        }
    }
}

/// What platform events deliver `input` to `window`.
fn raw(window: WindowId, input: &TapeInput) -> Vec<viso_platform::RawEvent> {
    use viso_platform::{
        ClipboardReply, KeyCode, Modifiers, PointerButtons, PointerId, PointerPhase, RawEvent,
        RawImePreedit, RawKey, RawPointer, RawScroll, RawText,
    };
    let mods = |m: TapeModifiers| Modifiers {
        shift: m.holds(TapeModifiers::SHIFT),
        control: m.holds(TapeModifiers::CONTROL),
        alt: m.holds(TapeModifiers::ALT),
        logo: m.holds(TapeModifiers::LOGO),
    };
    let event = match input {
        TapeInput::Pointer {
            pointer,
            kind,
            phase,
            x,
            y,
            pressure,
            buttons,
            modifiers,
        } => RawEvent::Pointer(RawPointer {
            window,
            pointer: PointerId(*pointer),
            kind: match kind {
                TapePointerKind::Mouse => PointerKind::Mouse,
                TapePointerKind::Touch => PointerKind::Touch,
                TapePointerKind::Pen => PointerKind::Pen,
            },
            x: f64::from(*x),
            y: f64::from(*y),
            pressure: *pressure,
            buttons: PointerButtons(*buttons),
            modifiers: mods(*modifiers),
            phase: match phase {
                TapePointerPhase::Moved => PointerPhase::Moved,
                TapePointerPhase::Down => PointerPhase::Down,
                TapePointerPhase::Up => PointerPhase::Up,
                TapePointerPhase::Left => PointerPhase::Left,
                TapePointerPhase::Cancel => PointerPhase::Cancel,
            },
        }),
        TapeInput::Scroll {
            x,
            y,
            delta_x,
            delta_y,
            modifiers,
        } => RawEvent::Scroll(RawScroll {
            window,
            x: f64::from(*x),
            y: f64::from(*y),
            delta_x: f64::from(*delta_x),
            delta_y: f64::from(*delta_y),
            modifiers: mods(*modifiers),
        }),
        TapeInput::Key {
            key,
            pressed,
            repeat,
            modifiers,
        } => {
            let code = match *key {
                TapeKey::Named(at) => match KeyCode::NAMED.get(usize::from(at)) {
                    Some(&code) => code,
                    None => return Vec::new(),
                },
                TapeKey::Other(code) => KeyCode::Other(code),
            };
            RawEvent::Key(RawKey {
                window,
                code,
                pressed: *pressed,
                repeat: *repeat,
                modifiers: mods(*modifiers),
            })
        }
        TapeInput::Text(text) => RawEvent::Text(RawText {
            window,
            text: text.clone(),
        }),
        TapeInput::Preedit { text, caret } => RawEvent::ImePreedit(RawImePreedit {
            window,
            text: text.clone(),
            caret: *caret as usize,
        }),
        TapeInput::Paste(text) => RawEvent::Paste {
            window,
            text: text.clone(),
        },
        TapeInput::Copy { cut } => RawEvent::CopyRequested {
            window,
            cut: *cut,
            reply: ClipboardReply::default(),
        },
        TapeInput::Focus(focused) => RawEvent::WindowFocused {
            window,
            focused: *focused,
        },
        TapeInput::Resize(surface) => RawEvent::ScaleFactorChanged {
            window,
            scale: f64::from(surface.scale),
            width: (surface.width * surface.scale).round() as u32,
            height: (surface.height * surface.scale).round() as u32,
        },
    };
    vec![event]
}
