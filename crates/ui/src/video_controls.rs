//! A video's controls — the parts the framework builds over a video node,
//! shared by the `Video` widget and the view's `Video` node.
//!
//! They lie over the picture, children of its leaf: a big play glyph in the
//! middle while it is paused, a spinner while it waits for data, and along
//! the bottom a bar holding a scrubber over a row of a play / pause button,
//! a mute button, a volume slider, the time played of the time it plays and
//! a full-screen button. The bar fades out while the video plays and nothing
//! stirs — no pointer over the video, no key, no drag — and back in at a
//! stir, at a pause or at the end; a press on the faded bar only brings it
//! back. The pictures stay the video's own: the parts read what its player
//! reports through a watch on its node and steer it with the requests a
//! handler makes ([`EventCx::play_video`], [`EventCx::seek_video`],
//! [`EventCx::set_playback`], [`EventCx::set_fullscreen`]).
//!
//! The pointer: a click on the picture plays or pauses it and a double
//! click puts it full screen or back; a tap shows or hides the bar, and a
//! double tap on the picture's start or end half jumps ten seconds back or
//! on. A drag on the scrubber seeks as it goes. The keyboard, wherever the
//! focus is among the parts: Space and K play or pause, the arrows left and
//! right seek five seconds (J and L ten), up and down step the volume, Home
//! and End go to the ends, the digits to tenths of the way, M mutes, F goes
//! full screen and Escape comes back.
//!
//! Semantics: each button is a `button` named by its [`VideoLabels`] for what
//! it does now; the scrubber and the volume are `slider`s, the scrubber's
//! value the seconds played of its length. The parts' surface takes the
//! focus as the video, so a click on it lets the keys steer it.
//!
//! Invalidation: what the player reports writes the parts' cells — a few
//! times a second while it plays — and each part redraws alone: the
//! scrubber's thumb a `PAINT`; the time, once a second, its text; a glyph
//! only when it changes.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use crate::slider::{Slider, SliderInput, SliderRange, SliderShape, Thumb, slider_input};
use crate::structure::StructureCx;
use crate::{
    Align, Alignment2D, Axis, BoxStyle, BuildCx, Content, DispatchPhase, Easing, EventCx,
    FlexStyle, FontSize, Handle, Inset, InteractionStyle, Justify, Key, LeafStyle, Length, LineCap,
    LineJoin, LookValue, NodeId, Notice, NoticeKinds, PathCmd, Point, PointerButtons, PointerPhase,
    Rgba, Role, SemanticState, Semantics, Size, SpinnerLook, StackPlacement, StackStyle, StateId,
    StateStore, StateValue, Stroke, TextRequest, TextStyle, Timing, Vec2,
};

/// The side of a button.
const BUTTON: f32 = 32.0;

/// The side of a button's glyph, centred in it.
const GLYPH: f32 = 20.0;

/// The side of the big play glyph's disc.
const BADGE: f32 = 64.0;

/// How long the bar stays while the video plays and nothing stirs.
const IDLE: Duration = Duration::from_millis(2500);

/// The high bits of the idle wake's token; its low bits count the stirs, so
/// a wake a stir since has overtaken is let go.
const IDLE_WAKE: u32 = 0x5643_0000;

/// How the bar fades in and out.
const FADE: Timing = Timing::eased(Duration::from_millis(200), Easing::EASE_OUT);

/// How far an arrow seeks, and J or L.
const STEP: Duration = Duration::from_secs(5);
const JUMP: Duration = Duration::from_secs(10);

/// How far up or down steps the volume.
const VOLUME_STEP: f32 = 0.05;

/// How the controls look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoControlsStyle {
    /// The glyphs, the time and the thumbs.
    pub ink: Rgba,
    /// The bar along the bottom.
    pub bar: Rgba,
    /// A button under the pointer.
    pub hover: Rgba,
    /// The disc the big play glyph sits on.
    pub badge: Rgba,
    /// A slider's track.
    pub track: Rgba,
    /// The stretch of a slider's track up to its value: played, or loud.
    pub fill: Rgba,
    /// The time's size.
    pub font_size: f32,
}

impl Default for VideoControlsStyle {
    fn default() -> Self {
        VideoControlsStyle {
            ink: Rgba::new(1.0, 1.0, 1.0, 1.0),
            bar: Rgba::new(0.0, 0.0, 0.0, 0.55),
            hover: Rgba::new(1.0, 1.0, 1.0, 0.18),
            badge: Rgba::new(0.0, 0.0, 0.0, 0.5),
            track: Rgba::new(1.0, 1.0, 1.0, 0.3),
            fill: Rgba::new(1.0, 1.0, 1.0, 1.0),
            font_size: 12.0,
        }
    }
}

/// What the controls are called for assistive technology.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoLabels {
    pub play: String,
    pub pause: String,
    pub mute: String,
    pub unmute: String,
    pub volume: String,
    pub seek: String,
    pub fullscreen: String,
    pub exit_fullscreen: String,
}

impl Default for VideoLabels {
    fn default() -> Self {
        VideoLabels {
            play: "Play".to_owned(),
            pause: "Pause".to_owned(),
            mute: "Mute".to_owned(),
            unmute: "Unmute".to_owned(),
            volume: "Volume".to_owned(),
            seek: "Seek".to_owned(),
            fullscreen: "Full screen".to_owned(),
            exit_fullscreen: "Exit full screen".to_owned(),
        }
    }
}

/// The cells the player's reports and the parts' own stirs write.
#[derive(Debug, Clone, Copy)]
struct Cells {
    playing: StateId,
    /// Waiting for data while asked to play.
    waiting: StateId,
    /// The pointer or a key stirred since the bar last faded.
    active: StateId,
    /// The scrubber is carried.
    scrubbing: StateId,
    /// The whole seconds played.
    secs: StateId,
    /// The whole seconds it plays, `-1` for a live stream or before it is
    /// ready.
    total: StateId,
    /// How far through it is, `0` to `1`.
    frac: StateId,
    /// Silent: muted, or at no volume.
    muted: StateId,
    volume: StateId,
    full: StateId,
}

/// What shows a part as its cells say, the first time or once they changed.
type Show = fn(&Parts, &mut StructureCx<'_>, bool);

/// The parts, and what their handlers share.
struct Parts {
    video: NodeId,
    /// The node shown full screen: the video, or one holding it.
    screen: NodeId,
    root: NodeId,
    badge: NodeId,
    spinner: NodeId,
    bar: NodeId,
    scrub: NodeId,
    play: NodeId,
    mute: NodeId,
    volume: NodeId,
    time: NodeId,
    full: NodeId,
    cells: Cells,
    style: VideoControlsStyle,
    labels: VideoLabels,
    /// The stirs counted, the low bits of the idle wake's token.
    stirs: Cell<u32>,
    /// The pointer is over the bar, which keeps it.
    over_bar: Cell<bool>,
}

/// The controls built over a video node. Cheap to clone.
#[derive(Clone)]
pub struct VideoControls(Rc<Parts>);

/// The parts' nodes as they are built.
#[derive(Default)]
struct Built {
    badge: Option<Handle>,
    spinner: Option<Handle>,
    bar: Option<Handle>,
    scrub: Option<Handle>,
    play: Option<Handle>,
    mute: Option<Handle>,
    volume: Option<Handle>,
    time: Option<Handle>,
    full: Option<Handle>,
    /// The cells the play, mute and full-screen buttons are lit by.
    hovers: Vec<StateId>,
}

impl VideoControls {
    /// Builds the controls of the video node `video` at `cx`'s cursor — over
    /// its picture when `cx` builds inside it ([`BuildCx::inside`]), or over
    /// the stack holding it. Full screen shows `screen`: the video, or the
    /// node holding it and the controls. Requires a
    /// [`BuildCx::with_reactive`] cx.
    pub fn build(
        cx: &mut BuildCx<'_>,
        video: NodeId,
        screen: NodeId,
        style: VideoControlsStyle,
        labels: VideoLabels,
    ) -> VideoControls {
        let bool_cell = |cx: &mut BuildCx<'_>| cx.state(StateValue::Bool(false));
        let cells = Cells {
            playing: bool_cell(cx),
            waiting: bool_cell(cx),
            active: bool_cell(cx),
            scrubbing: bool_cell(cx),
            secs: cx.state(StateValue::Int(0)),
            total: cx.state(StateValue::Int(-1)),
            frac: cx.state(StateValue::Float(0.0)),
            muted: bool_cell(cx),
            volume: cx.state(StateValue::Float(1.0)),
            full: bool_cell(cx),
        };
        let mut b = Built::default();
        let root = cx.stack(
            StackStyle {
                align: Alignment2D::Center,
                size: Size::fill(),
                ..StackStyle::default()
            },
            |cx| build_parts(cx, &style, &mut b),
        );
        let take = |h: Option<Handle>| h.expect("built").id();
        let hovers = std::mem::take(&mut b.hovers);
        let parts = Rc::new(Parts {
            video,
            screen,
            root: root.id(),
            badge: take(b.badge),
            spinner: take(b.spinner),
            bar: take(b.bar),
            scrub: take(b.scrub),
            play: take(b.play),
            mute: take(b.mute),
            volume: take(b.volume),
            time: take(b.time),
            full: take(b.full),
            cells,
            style,
            labels,
            stirs: Cell::new(0),
            over_bar: Cell::new(false),
        });
        wire(cx, &parts, &hovers);
        VideoControls(parts)
    }

    /// The surface the parts lie on, over the whole picture.
    pub fn root(&self) -> NodeId {
        self.0.root
    }

    /// The video node they steer.
    pub fn video(&self) -> NodeId {
        self.0.video
    }
}

/// Builds the parts inside the controls' surface.
fn build_parts(cx: &mut BuildCx<'_>, style: &VideoControlsStyle, b: &mut Built) {
    let badge = cx.flex(
        FlexStyle {
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fixed(BADGE, BADGE),
            style: BoxStyle::solid(style.badge).with_radius(BADGE * 0.5),
            ..FlexStyle::default()
        },
        |cx| {
            let leaf = cx.leaf(LeafStyle {
                size: Size::fixed(BADGE * 0.5, BADGE * 0.5),
                style: BoxStyle::NONE,
            });
            cx.store
                .set_content_payload(leaf.id(), glyph(Glyph::Play, style.ink, BADGE * 0.5));
        },
    );
    cx.unhit_below(badge);
    cx.store.set_hittable(badge.id(), false);
    b.badge = Some(badge);
    let spinner = cx.spinner(SpinnerLook::native(), 40.0, style.ink);
    cx.set_hidden(spinner, true);
    b.spinner = Some(spinner);
    cx.place(StackPlacement {
        align: Some(Alignment2D::Bottom),
        layer: 0,
    });
    let bar = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            gap: 2.0,
            padding: Inset {
                left: 10.0,
                top: 4.0,
                right: 10.0,
                bottom: 4.0,
            },
            align: Align::Stretch,
            size: Size {
                width: Length::fill(),
                height: Length::Fit,
            },
            style: BoxStyle::solid(style.bar),
            ..FlexStyle::default()
        },
        |cx| {
            b.scrub = Some(cx.slider_leaf(
                Size {
                    width: Length::fill(),
                    height: Length::Fixed(16.0),
                },
                Slider::new(
                    SliderShape::Bar(Axis::Row),
                    style.track,
                    style.fill,
                    style.ink,
                    0.0,
                ),
            ));
            cx.flex(
                FlexStyle {
                    gap: 6.0,
                    align: Align::Center,
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fit,
                    },
                    ..FlexStyle::default()
                },
                |cx| {
                    b.play = Some(button(cx, Glyph::Play, style, &mut b.hovers));
                    b.mute = Some(button(cx, Glyph::Volume, style, &mut b.hovers));
                    b.volume = Some(cx.slider_leaf(
                        Size::fixed(64.0, 16.0),
                        Slider::new(
                            SliderShape::Bar(Axis::Row),
                            style.track,
                            style.fill,
                            style.ink,
                            1.0,
                        ),
                    ));
                    let time = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.text_request(time, line("0:00", style));
                    cx.store.set_hittable(time.id(), false);
                    b.time = Some(time);
                    // The room between the time and the full-screen button.
                    let gap = cx.leaf(LeafStyle {
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fixed(1.0),
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.store.set_hittable(gap.id(), false);
                    b.full = Some(button(cx, Glyph::Enter, style, &mut b.hovers));
                },
            );
        },
    );
    b.bar = Some(bar);
}

/// Wires the parts: their focus, semantics and handlers, the watch on the
/// video, and the hooks that show what its cells hold.
fn wire(cx: &mut BuildCx<'_>, parts: &Rc<Parts>, hovers: &[StateId]) {
    let p = &**parts;
    let c = p.cells;
    let root = Handle::of(p.root);
    cx.focusable(root, true);
    cx.count_clicks(root);
    for (node, label) in [
        (p.play, &p.labels.play),
        (p.mute, &p.labels.mute),
        (p.full, &p.labels.fullscreen),
    ] {
        cx.semantics(
            Handle::of(node),
            Semantics::role(Role::Button).with_label(label.clone()),
        );
    }
    let scrub = Handle::of(p.scrub);
    let volume = Handle::of(p.volume);
    cx.focusable(scrub, true);
    cx.focusable(volume, true);
    cx.semantics(
        scrub,
        Semantics::role(Role::Slider).with_label(p.labels.seek.clone()),
    );
    cx.semantics(
        volume,
        Semantics::role(Role::Slider).with_label(p.labels.volume.clone()),
    );
    cx.bind_semantic_state(scrub, move |cx| {
        let total = int(cx.get(c.total)).max(0) as f32;
        SemanticState::slider(int(cx.get(c.secs)) as f32, 0.0, total)
    });
    cx.bind_semantic_state(volume, move |cx| {
        let shown = if truth(cx.get(c.muted)) {
            0.0
        } else {
            float(cx.get(c.volume))
        };
        SemanticState::slider(shown, 0.0, 1.0)
    });

    // What the player reports.
    let hear = move |notice: &Notice, states: &mut StateStore| match *notice {
        Notice::Video(s) => {
            states.set(c.playing, StateValue::Bool(s.playing));
            states.set(c.waiting, StateValue::Bool(s.waiting && s.playing));
            states.set(c.secs, StateValue::Int(whole(s.position)));
            let total = s
                .duration
                .map_or(-1, |d| whole(d + Duration::from_millis(500)));
            states.set(c.total, StateValue::Int(total));
            let frac = s
                .duration
                .filter(|d| !d.is_zero())
                .map_or(0.0, |d| (s.position.as_secs_f64() / d.as_secs_f64()) as f32);
            states.set(c.frac, StateValue::Float(frac.clamp(0.0, 1.0)));
            states.set(c.muted, StateValue::Bool(s.muted || s.volume <= 0.0));
            states.set(c.volume, StateValue::Float(s.volume));
        }
        Notice::Fullscreen(on) => {
            states.set(c.full, StateValue::Bool(on));
        }
        _ => {}
    };
    cx.store.watch_on(
        p.root,
        p.video,
        NoticeKinds::VIDEO | NoticeKinds::FULLSCREEN,
        Box::new(hear),
    );

    let at = Rc::clone(parts);
    cx.on_pointer(root, move |ev| at.surface_pointer(ev));
    let at = Rc::clone(parts);
    cx.on_key(root, move |ev| at.surface_key(ev));
    let at = Rc::clone(parts);
    cx.on_pointer(Handle::of(p.bar), move |ev| at.bar_pointer(ev));
    let at = Rc::clone(parts);
    let scrub_input = Rc::new(move |ev: &mut EventCx<'_>| at.scrub_input(ev));
    let pointer = Rc::clone(&scrub_input);
    cx.on_pointer(scrub, move |ev| pointer(ev));
    cx.on_key(scrub, move |ev| scrub_input(ev));
    let at = Rc::clone(parts);
    let volume_input = Rc::new(move |ev: &mut EventCx<'_>| at.volume_input(ev));
    let pointer = Rc::clone(&volume_input);
    cx.on_pointer(volume, move |ev| pointer(ev));
    cx.on_key(volume, move |ev| volume_input(ev));
    let acts: [fn(&Parts, &mut EventCx<'_>); 3] =
        [Parts::toggle, Parts::toggle_mute, Parts::toggle_fullscreen];
    for ((node, act), &hover) in [p.play, p.mute, p.full].into_iter().zip(acts).zip(hovers) {
        let at = Rc::clone(parts);
        let handle = Handle::of(node);
        cx.on_pointer(handle, {
            let at = Rc::clone(&at);
            move |ev| {
                if ev.phase() == DispatchPhase::Capture {
                    return;
                }
                let Some(pt) = ev.pointer().copied() else {
                    return;
                };
                match pt.phase {
                    PointerPhase::Enter | PointerPhase::Leave => {
                        ev.set(hover, StateValue::Bool(pt.phase == PointerPhase::Enter));
                    }
                    PointerPhase::Down if pt.buttons.contains(PointerButtons::PRIMARY) => {
                        ev.stop_propagation();
                        act(&at, ev);
                        at.stir(ev);
                    }
                    _ => {}
                }
            }
        });
        cx.on_key(handle, move |ev| {
            if ev.phase() != DispatchPhase::Capture
                && let Some(k) = ev.key()
                && k.pressed
                && !k.repeat
                && matches!(k.key, Key::Space | Key::Enter)
            {
                ev.stop_propagation();
                act(&at, ev);
                at.stir(ev);
            }
        });
    }

    let at = Rc::clone(parts);
    cx.structure(move |s| at.show_all(s));
}

impl Parts {
    /// Plays the video if paused, pauses it if playing.
    fn toggle(&self, ev: &mut EventCx<'_>) {
        let playing = ev.playback(self.video).is_some_and(|p| p.playing);
        ev.play_video(self.video, !playing);
    }

    /// Silences it, or lets it sound — at full volume if it had none.
    fn toggle_mute(&self, ev: &mut EventCx<'_>) {
        let Some(mut playback) = ev.playback(self.video) else {
            return;
        };
        if playback.muted || playback.volume <= 0.0 {
            playback.muted = false;
            if playback.volume <= 0.0 {
                playback.volume = 1.0;
            }
        } else {
            playback.muted = true;
        }
        ev.set_playback(self.video, playback);
    }

    /// Shows it full screen, or brings it back.
    fn toggle_fullscreen(&self, ev: &mut EventCx<'_>) {
        let full = ev.fullscreen() == Some(self.screen);
        ev.set_fullscreen(self.screen, !full);
    }

    /// Moves it to `time`, held within its length.
    fn seek_to(&self, ev: &mut EventCx<'_>, time: Duration) {
        let Some(length) = ev.video_status(self.video).and_then(|s| s.duration) else {
            return;
        };
        let time = time.min(length);
        ev.seek_video(self.video, time);
        if !length.is_zero() {
            let f = (time.as_secs_f64() / length.as_secs_f64()) as f32;
            ev.show_slider(self.scrub, 0.0, f);
        }
    }

    /// Moves it `by` on, or back when `back`.
    fn seek_by(&self, ev: &mut EventCx<'_>, by: Duration, back: bool) {
        let Some(at) = ev.video_status(self.video).map(|s| s.position) else {
            return;
        };
        let to = if back { at.saturating_sub(by) } else { at + by };
        self.seek_to(ev, to);
    }

    /// Sets its volume to `volume`, sounding unless it is none.
    fn set_volume(&self, ev: &mut EventCx<'_>, volume: f32) {
        let Some(mut playback) = ev.playback(self.video) else {
            return;
        };
        playback.volume = volume.clamp(0.0, 1.0);
        playback.muted = playback.volume <= 0.0;
        ev.set_playback(self.video, playback);
        ev.show_slider(self.volume, 0.0, playback.volume);
    }

    /// The volume it sounds at now: none while muted.
    fn heard_volume(&self, ev: &EventCx<'_>) -> f32 {
        ev.playback(self.video)
            .map_or(1.0, |p| if p.muted { 0.0 } else { p.volume })
    }

    /// Whether the bar is faded out: it plays, and nothing stirs.
    fn faded(&self, ev: &EventCx<'_>) -> bool {
        let c = self.cells;
        truth(ev.get(c.playing)) && !truth(ev.get(c.active)) && !truth(ev.get(c.scrubbing))
    }

    /// Something stirred: the bar shows, and fades again once nothing has
    /// for a while.
    fn stir(&self, ev: &mut EventCx<'_>) {
        ev.set(self.cells.active, StateValue::Bool(true));
        let stirs = self.stirs.get().wrapping_add(1) & 0xffff;
        self.stirs.set(stirs);
        ev.wake_after(self.root, IDLE, IDLE_WAKE | stirs);
    }

    /// The surface over the picture: a click plays or pauses it, a double
    /// click goes full screen; a tap shows or hides the bar, a double tap
    /// jumps back or on; the pointer moving over it stirs.
    fn surface_pointer(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(pt) = ev.pointer().copied() else {
            return;
        };
        let direct = ev.direct();
        let primary = pt.buttons.contains(PointerButtons::PRIMARY);
        let on_surface = ev.phase() == DispatchPhase::Target;
        match pt.phase {
            PointerPhase::Enter | PointerPhase::Move if !direct => self.stir(ev),
            PointerPhase::Leave if !direct => {
                ev.set(self.cells.active, StateValue::Bool(false));
            }
            PointerPhase::Down if primary && on_surface && !direct => {
                ev.request_focus(self.root);
                // A second click takes back the first's play or pause.
                self.toggle(ev);
                if ev.clicks() == 2 {
                    self.toggle_fullscreen(ev);
                }
                self.stir(ev);
            }
            PointerPhase::Up if on_surface && direct && ev.still() => {
                if ev.clicks() >= 2 {
                    let rect = ev.rect().unwrap_or_default();
                    let back = (pt.x < rect.w * 0.5) != ev.reads_rtl();
                    self.seek_by(ev, JUMP, back);
                    self.stir(ev);
                } else if self.faded(ev) || !truth(ev.get(self.cells.playing)) {
                    self.stir(ev);
                } else {
                    ev.set(self.cells.active, StateValue::Bool(false));
                }
            }
            _ => {}
        }
    }

    /// The keys, from wherever among the parts the focus is, and the idle
    /// wake that fades the bar.
    fn surface_key(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        if let Some(token) = ev.woken() {
            let held = self.over_bar.get()
                || truth(ev.get(self.cells.scrubbing))
                || ev
                    .focused()
                    .is_some_and(|f| f != self.root && ev.is_within(f, self.bar));
            if token == IDLE_WAKE | self.stirs.get() && !held {
                ev.set(self.cells.active, StateValue::Bool(false));
            }
            return;
        }
        let Some(k) = ev.key().copied() else {
            return;
        };
        let m = k.modifiers;
        if !k.pressed || m.control || m.alt || m.logo {
            return;
        }
        let rtl = ev.reads_rtl();
        let length = ev.video_status(self.video).and_then(|s| s.duration);
        match k.key {
            Key::Space | Key::K if !k.repeat => self.toggle(ev),
            Key::Left | Key::Right => {
                self.seek_by(ev, STEP, (k.key == Key::Left) != rtl);
            }
            Key::J => self.seek_by(ev, JUMP, true),
            Key::L => self.seek_by(ev, JUMP, false),
            Key::Up => {
                let v = self.heard_volume(ev) + VOLUME_STEP;
                self.set_volume(ev, v);
            }
            Key::Down => {
                let v = self.heard_volume(ev) - VOLUME_STEP;
                self.set_volume(ev, v);
            }
            Key::Home => self.seek_to(ev, Duration::ZERO),
            Key::End => {
                if let Some(length) = length {
                    self.seek_to(ev, length);
                }
            }
            Key::M if !k.repeat => self.toggle_mute(ev),
            Key::F if !k.repeat => self.toggle_fullscreen(ev),
            Key::Escape if ev.fullscreen() == Some(self.screen) => {
                ev.set_fullscreen(self.screen, false);
            }
            key => {
                let Some(tenth) = digit(key) else {
                    return;
                };
                let Some(length) = length else {
                    return;
                };
                self.seek_to(ev, length.mul_f32(tenth as f32 / 10.0));
            }
        }
        ev.stop_propagation();
        self.stir(ev);
    }

    /// The bar: a press on it while faded only brings it back; the pointer
    /// over it keeps it.
    fn bar_pointer(&self, ev: &mut EventCx<'_>) {
        let Some(pt) = ev.pointer().copied() else {
            return;
        };
        if ev.phase() == DispatchPhase::Capture {
            if pt.phase == PointerPhase::Down && self.faded(ev) {
                ev.stop_propagation();
                self.stir(ev);
            }
            return;
        }
        match pt.phase {
            PointerPhase::Enter => self.over_bar.set(true),
            PointerPhase::Leave => self.over_bar.set(false),
            _ => {}
        }
    }

    /// The scrubber: a press or a drag seeks there, an arrow five seconds,
    /// Home and End to the ends.
    fn scrub_input(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(input) = slider_input(ev, self.scrub, Thumb::High) else {
            return;
        };
        ev.stop_propagation();
        let Some(status) = ev.video_status(self.video) else {
            return;
        };
        let Some(length) = status.duration.filter(|d| !d.is_zero()) else {
            return;
        };
        match input {
            SliderInput::To(_, f) => {
                ev.set(self.cells.scrubbing, StateValue::Bool(true));
                self.seek_to(ev, length.mul_f32(f));
            }
            SliderInput::Step(_, n) => {
                self.seek_by(ev, STEP.mul_f64(n.abs()), n < 0.0);
            }
            SliderInput::ToMin(_) => self.seek_to(ev, Duration::ZERO),
            SliderInput::ToMax(_) => self.seek_to(ev, length),
            SliderInput::Release => {
                ev.set(self.cells.scrubbing, StateValue::Bool(false));
            }
            SliderInput::Reset => {}
        }
        self.stir(ev);
    }

    /// The volume slider.
    fn volume_input(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(input) = slider_input(ev, self.volume, Thumb::High) else {
            return;
        };
        ev.stop_propagation();
        let range = SliderRange::new(0.0, 1.0, Some(f64::from(VOLUME_STEP)));
        let now = f64::from(self.heard_volume(ev));
        if let Some(next) = input.apply(now, &range, Thumb::High) {
            self.set_volume(ev, next as f32);
        }
        self.stir(ev);
    }

    /// Shows what the cells hold, and again each time they change.
    fn show_all(self: &Rc<Self>, s: &mut StructureCx<'_>) {
        let c = self.cells;
        let hooks: [(&[StateId], Show); 6] = [
            (&[c.playing, c.waiting], Parts::show_playing),
            (&[c.playing, c.active, c.scrubbing], Parts::show_bar),
            (&[c.secs, c.total], Parts::show_time),
            (&[c.frac], Parts::show_progress),
            (&[c.muted, c.volume], Parts::show_sound),
            (&[c.full], Parts::show_full),
        ];
        for (deps, show) in hooks {
            show(self, s, true);
            let at = Rc::clone(self);
            s.store
                .add_structure_hook(deps.iter().copied(), move |s, _| show(&at, s, false));
        }
    }

    /// The play button's glyph and name, the big play glyph while paused
    /// and the spinner while it waits.
    fn show_playing(&self, s: &mut StructureCx<'_>, _first: bool) {
        let playing = truth(s.states.get(self.cells.playing));
        let waiting = truth(s.states.get(self.cells.waiting));
        let (g, label) = if playing {
            (Glyph::Pause, &self.labels.pause)
        } else {
            (Glyph::Play, &self.labels.play)
        };
        self.show_glyph(s, self.play, g, label);
        s.store.set_hidden(self.badge, playing || waiting);
        s.store.set_hidden(self.spinner, !waiting);
        s.store.set_spinning(self.spinner, waiting);
    }

    /// The bar, faded out while it plays and nothing stirs.
    fn show_bar(&self, s: &mut StructureCx<'_>, first: bool) {
        let c = self.cells;
        let shown = !truth(s.states.get(c.playing))
            || truth(s.states.get(c.active))
            || truth(s.states.get(c.scrubbing));
        let opacity = if shown { 1.0 } else { 0.0 };
        if first {
            s.store.set_opacity(self.bar, opacity);
        } else {
            s.store
                .transition(self.bar, LookValue::Opacity(opacity), FADE);
        }
    }

    /// The time played of the time it plays, the scrubber only for a video
    /// that has a length.
    fn show_time(&self, s: &mut StructureCx<'_>, _first: bool) {
        let secs = int(s.states.get(self.cells.secs));
        let total = int(s.states.get(self.cells.total));
        let long = total >= 3600;
        let text = if total < 0 {
            clock(secs, long)
        } else {
            format!("{} / {}", clock(secs, long), clock(total, long))
        };
        s.store.set_text(self.time, line(&text, &self.style));
        s.store.set_hidden(self.scrub, total < 0);
    }

    /// The scrubber's thumb where it is, but for while it is carried.
    fn show_progress(&self, s: &mut StructureCx<'_>, first: bool) {
        let carried = s
            .store
            .slider(self.scrub)
            .is_some_and(|sl| sl.drag.is_some());
        if !carried {
            let f = float(s.states.get(self.cells.frac));
            s.store.show_slider(self.scrub, 0.0, f);
            if first {
                s.store.land_slider(self.scrub);
            }
        }
    }

    /// The mute button's glyph and name, and the volume slider.
    fn show_sound(&self, s: &mut StructureCx<'_>, first: bool) {
        let muted = truth(s.states.get(self.cells.muted));
        let (g, label) = if muted {
            (Glyph::Muted, &self.labels.unmute)
        } else {
            (Glyph::Volume, &self.labels.mute)
        };
        self.show_glyph(s, self.mute, g, label);
        let carried = s
            .store
            .slider(self.volume)
            .is_some_and(|sl| sl.drag.is_some());
        if !carried {
            let v = if muted {
                0.0
            } else {
                float(s.states.get(self.cells.volume))
            };
            s.store.show_slider(self.volume, 0.0, v);
            if first {
                s.store.land_slider(self.volume);
            }
        }
    }

    /// The full-screen button's glyph and name.
    fn show_full(&self, s: &mut StructureCx<'_>, _first: bool) {
        let (g, label) = if truth(s.states.get(self.cells.full)) {
            (Glyph::Exit, &self.labels.exit_fullscreen)
        } else {
            (Glyph::Enter, &self.labels.fullscreen)
        };
        self.show_glyph(s, self.full, g, label);
    }

    /// Button `button` shows `g`, named `label`, unless it does already.
    fn show_glyph(&self, s: &mut StructureCx<'_>, button: NodeId, g: Glyph, label: &str) {
        let Some(leaf) = s.store.arena().children(button).next() else {
            return;
        };
        let content = glyph(g, self.style.ink, GLYPH);
        if s.store.content_payload(leaf) != Some(&content) {
            s.store.set_content_payload(leaf, content);
        }
        let named = Semantics::role(Role::Button).with_label(label.to_owned());
        if s.store.semantics(button) != Some(&named) {
            s.store.set_semantics(button, named);
        }
    }
}

/// Whole seconds of `time`.
fn whole(time: Duration) -> i32 {
    i32::try_from(time.as_secs()).unwrap_or(i32::MAX)
}

/// `secs` as a clock reads: `m:ss`, or `h:mm:ss` when `long`.
fn clock(secs: i32, long: bool) -> String {
    let secs = secs.max(0);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if long {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{}:{s:02}", secs / 60)
    }
}

/// The tenth of the way a digit key goes to.
fn digit(key: Key) -> Option<u8> {
    Some(match key {
        Key::Digit0 => 0,
        Key::Digit1 => 1,
        Key::Digit2 => 2,
        Key::Digit3 => 3,
        Key::Digit4 => 4,
        Key::Digit5 => 5,
        Key::Digit6 => 6,
        Key::Digit7 => 7,
        Key::Digit8 => 8,
        Key::Digit9 => 9,
        _ => return None,
    })
}

fn truth(value: Option<StateValue>) -> bool {
    matches!(value, Some(StateValue::Bool(true)))
}

fn int(value: Option<StateValue>) -> i32 {
    match value {
        Some(StateValue::Int(v)) => v,
        _ => 0,
    }
}

fn float(value: Option<StateValue>) -> f32 {
    match value {
        Some(StateValue::Float(v)) => v,
        _ => 0.0,
    }
}

/// The glyphs the controls draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Glyph {
    Play,
    Pause,
    Volume,
    Muted,
    Enter,
    Exit,
}

/// The path of `g` inked `ink`, drawn in a box `side` across.
fn glyph(g: Glyph, ink: Rgba, side: f32) -> Content {
    // Drawn on a grid 24 across, centred in the box.
    let s = side / 24.0;
    let p = |x: f32, y: f32| Point::new(x * s, y * s);
    let stroke = |width: f32| Stroke {
        cap: LineCap::Round,
        join: LineJoin::Round,
        ..Stroke::new(width * s, ink)
    };
    let (cmds, fill, stroke) = match g {
        Glyph::Play => (
            vec![
                PathCmd::MoveTo(p(7.0, 4.5)),
                PathCmd::LineTo(p(19.0, 12.0)),
                PathCmd::LineTo(p(7.0, 19.5)),
                PathCmd::Close,
            ],
            Some(ink),
            Some(stroke(2.0)),
        ),
        Glyph::Pause => (
            vec![
                PathCmd::MoveTo(p(8.0, 5.0)),
                PathCmd::LineTo(p(8.0, 19.0)),
                PathCmd::MoveTo(p(16.0, 5.0)),
                PathCmd::LineTo(p(16.0, 19.0)),
            ],
            None,
            Some(stroke(3.5)),
        ),
        Glyph::Volume | Glyph::Muted => {
            let mut cmds = vec![
                PathCmd::MoveTo(p(11.0, 5.0)),
                PathCmd::LineTo(p(6.0, 9.0)),
                PathCmd::LineTo(p(2.5, 9.0)),
                PathCmd::LineTo(p(2.5, 15.0)),
                PathCmd::LineTo(p(6.0, 15.0)),
                PathCmd::LineTo(p(11.0, 19.0)),
                PathCmd::Close,
            ];
            if g == Glyph::Volume {
                // Two waves, quarter circles about the speaker's mouth.
                cmds.extend([
                    PathCmd::MoveTo(p(15.54, 8.46)),
                    PathCmd::CubicTo(p(17.49, 10.42), p(17.49, 13.58), p(15.54, 15.54)),
                    PathCmd::MoveTo(p(19.07, 4.93)),
                    PathCmd::CubicTo(p(22.98, 8.84), p(22.98, 15.16), p(19.07, 19.07)),
                ]);
            } else {
                cmds.extend([
                    PathCmd::MoveTo(p(16.0, 9.0)),
                    PathCmd::LineTo(p(22.0, 15.0)),
                    PathCmd::MoveTo(p(22.0, 9.0)),
                    PathCmd::LineTo(p(16.0, 15.0)),
                ]);
            }
            (cmds, None, Some(stroke(2.0)))
        }
        Glyph::Enter => (
            corners([
                [p(8.0, 3.0), p(3.0, 3.0), p(3.0, 8.0)],
                [p(16.0, 3.0), p(21.0, 3.0), p(21.0, 8.0)],
                [p(21.0, 16.0), p(21.0, 21.0), p(16.0, 21.0)],
                [p(3.0, 16.0), p(3.0, 21.0), p(8.0, 21.0)],
            ]),
            None,
            Some(stroke(2.0)),
        ),
        Glyph::Exit => (
            corners([
                [p(8.0, 3.0), p(8.0, 8.0), p(3.0, 8.0)],
                [p(16.0, 3.0), p(16.0, 8.0), p(21.0, 8.0)],
                [p(21.0, 16.0), p(16.0, 16.0), p(16.0, 21.0)],
                [p(3.0, 16.0), p(8.0, 16.0), p(8.0, 21.0)],
            ]),
            None,
            Some(stroke(2.0)),
        ),
    };
    Content::Path {
        cmds,
        fill,
        stroke,
        natural: Vec2::new(side, side),
    }
}

/// Four corners, each two strokes meeting at its middle point.
fn corners(corners: [[Point; 3]; 4]) -> Vec<PathCmd> {
    corners
        .into_iter()
        .flat_map(|[a, b, c]| [PathCmd::MoveTo(a), PathCmd::LineTo(b), PathCmd::LineTo(c)])
        .collect()
}

/// A button showing `g`, focusable, its box lit while the cell it pushes
/// on `hovers` holds `true`.
fn button(
    cx: &mut BuildCx<'_>,
    g: Glyph,
    style: &VideoControlsStyle,
    hovers: &mut Vec<StateId>,
) -> Handle {
    let hover = cx.state(StateValue::Bool(false));
    hovers.push(hover);
    let button = cx.flex(
        FlexStyle {
            align: Align::Center,
            justify: Justify::Center,
            size: Size::fixed(BUTTON, BUTTON),
            ..FlexStyle::default()
        },
        |cx| {
            let leaf = cx.leaf(LeafStyle {
                size: Size::fixed(GLYPH, GLYPH),
                style: BoxStyle::NONE,
            });
            cx.store
                .set_content_payload(leaf.id(), glyph(g, style.ink, GLYPH));
        },
    );
    cx.unhit_below(button);
    cx.focusable(button, true);
    let lit = BoxStyle::solid(style.hover).with_radius(6.0);
    cx.interaction_style(
        button,
        InteractionStyle {
            resting: BoxStyle::NONE,
            hover: lit,
            pressed: lit,
            pressed_cell: None,
            hover_cell: Some(hover),
        },
    );
    button
}

/// One line of the time, in `style`.
fn line(text: &str, style: &VideoControlsStyle) -> TextRequest {
    TextRequest {
        text: text.to_owned(),
        font_size: FontSize::Fixed(style.font_size),
        color: style.ink,
        soft_wrap: false,
        locale: None,
        style: TextStyle::default(),
        spans: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flush::settle_states;
    use crate::input::{KeyEvent, KeyRouter, Modifiers, PointerEvent, route_pointer};
    use crate::{
        AssetFit, BindingTable, ComputedStore, EffectStore, NodeStore, SemanticProjector,
        TextEdits, VideoSource, VideoStatus, VirtualLists, deliver_notices,
    };
    use viso_render::Rect;

    const SURFACE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 320.0,
        h: 180.0,
    };

    struct World {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        edits: TextEdits,
        projectors: SemanticProjector,
        root: NodeId,
        controls: VideoControls,
    }

    /// A 320 × 180 video with its controls over it, laid out.
    fn world() -> World {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut controls = None;
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut edits,
                &mut projectors,
            );
            cx.flex(FlexStyle::default(), |cx| {
                let leaf = cx.leaf(LeafStyle {
                    size: Size::fixed(320.0, 180.0),
                    style: BoxStyle::NONE,
                });
                cx.video(leaf, &VideoSource::file("a.mp4"), AssetFit::Contain, None);
                controls = Some(cx.inside(leaf, |cx| {
                    VideoControls::build(
                        cx,
                        leaf.id(),
                        leaf.id(),
                        VideoControlsStyle::default(),
                        VideoLabels::default(),
                    )
                }));
            });
            cx.root().unwrap()
        };
        let mut w = World {
            store,
            states,
            bindings,
            lists,
            edits,
            projectors,
            root,
            controls: controls.unwrap(),
        };
        w.store.set_reduced_motion(true);
        w.frame();
        w
    }

    impl World {
        fn parts(&self) -> &Parts {
            &self.controls.0
        }

        /// Settles what was written, lays out, and hears the player.
        fn frame(&mut self) {
            let settle = |w: &mut World| {
                settle_states(
                    &mut w.store,
                    &mut w.states,
                    &mut w.bindings,
                    &mut ComputedStore::new(),
                    &mut w.projectors,
                    &mut EffectStore::default(),
                    &mut w.lists,
                    &mut Vec::new(),
                )
                .unwrap();
            };
            settle(self);
            self.store.layout(self.root, SURFACE, &mut Vec::new());
            deliver_notices(&mut self.store, &self.lists, &mut self.states);
            settle(self);
            self.store.layout(self.root, SURFACE, &mut Vec::new());
        }

        fn report(&mut self, status: VideoStatus) {
            let video = self.controls.video();
            self.store.videos_mut().get_mut(video).unwrap().status = status;
            self.frame();
        }

        fn pointer(&mut self, at: NodeId, phase: PointerPhase) {
            let r = self.store.world(at);
            let ev = PointerEvent {
                x: r.x + r.w * 0.5,
                y: r.y + r.h * 0.5,
                phase,
                buttons: if phase == PointerPhase::Move {
                    PointerButtons::NONE
                } else {
                    PointerButtons::PRIMARY
                },
                modifiers: Modifiers::default(),
            };
            let root = self.root;
            route_pointer(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                root,
                ev,
                &mut Vec::new(),
            );
            self.frame();
        }

        fn key(&mut self, key: Key) {
            let ev = KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let root = self.root;
            KeyRouter::route_key(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.edits,
                root,
                ev,
                &mut Vec::new(),
            );
            self.frame();
        }

        fn label(&self, node: NodeId) -> String {
            self.store.semantics(node).unwrap().label.clone().unwrap()
        }

        fn time(&self) -> String {
            self.store
                .text_request(self.parts().time)
                .unwrap()
                .text
                .clone()
        }
    }

    fn playing_at(secs: u64) -> VideoStatus {
        VideoStatus {
            ready: true,
            position: Duration::from_secs(secs),
            duration: Some(Duration::from_secs(90)),
            playing: true,
            ..VideoStatus::default()
        }
    }

    #[test]
    fn the_controls_lie_over_the_picture_and_show_what_the_player_reports() {
        let mut w = world();
        let (video, p) = (w.controls.video(), w.parts());
        assert_eq!(w.store.world(p.root), w.store.world(video));
        let bar = w.store.world(p.bar);
        assert_eq!(bar.y + bar.h, SURFACE.h, "the bar lies along the bottom");
        assert!(!w.store.hidden(p.badge));
        assert!(w.store.hidden(p.scrub), "no length yet");
        assert_eq!(w.time(), "0:00");

        w.report(playing_at(30));
        let p = w.parts();
        assert!(w.store.hidden(p.badge));
        assert!(!w.store.hidden(p.scrub));
        assert_eq!(w.time(), "0:30 / 1:30");
        let thumb = w.store.slider(p.scrub).unwrap().value[1];
        assert!((thumb - 1.0 / 3.0).abs() < 1e-4, "{thumb}");
        assert_eq!(w.label(p.play), "Pause");

        // Waiting shows the spinner; sound off, the mute button unmutes.
        w.report(VideoStatus {
            waiting: true,
            muted: true,
            ..playing_at(31)
        });
        let p = w.parts();
        assert!(!w.store.hidden(p.spinner));
        assert_eq!(w.label(p.mute), "Unmute");
        assert_eq!(w.store.slider(p.volume).unwrap().value[1], 0.0);
    }

    #[test]
    fn its_buttons_and_keys_steer_the_video() {
        let mut w = world();
        let video = w.controls.video();
        w.store.set_playback(
            video,
            crate::Playback {
                playing: true,
                ..crate::Playback::default()
            },
        );
        w.report(playing_at(30));
        let (play, mute, full, root) = {
            let p = w.parts();
            (p.play, p.mute, p.full, p.root)
        };
        // The bar faded as it played: the pointer brings it back first.
        w.pointer(root, PointerPhase::Move);
        w.pointer(play, PointerPhase::Down);
        assert!(!w.store.playback(video).unwrap().playing, "paused");
        w.pointer(mute, PointerPhase::Down);
        assert!(w.store.playback(video).unwrap().muted);

        // A click on the picture plays it and takes the focus for the keys.
        w.pointer(root, PointerPhase::Down);
        assert!(w.store.playback(video).unwrap().playing);
        assert_eq!(w.store.focused(), Some(root));
        w.key(Key::Right);
        assert_eq!(
            w.store.videos().get(video).unwrap().seek,
            Some(Duration::from_secs(35))
        );
        w.key(Key::Digit5);
        assert_eq!(
            w.store.videos().get(video).unwrap().seek,
            Some(Duration::from_secs(45))
        );
        w.key(Key::M);
        assert!(!w.store.playback(video).unwrap().muted);

        // F goes full screen, the button saying how to come back; Escape
        // comes back.
        w.key(Key::F);
        assert_eq!(w.store.fullscreen(), Some(video));
        assert_eq!(w.store.world(video), SURFACE);
        assert_eq!(w.label(full), "Exit full screen");
        w.key(Key::Escape);
        assert_eq!(w.store.fullscreen(), None);
        assert_eq!(w.label(full), "Full screen");
    }

    #[test]
    fn the_bar_fades_while_it_plays_and_nothing_stirs() {
        let mut w = world();
        let bar = w.parts().bar;
        assert_eq!(w.store.opacity(bar), 1.0, "shown while paused");
        w.report(playing_at(1));
        assert_eq!(w.store.opacity(bar), 0.0);

        // The pointer over it stirs; nothing since, the idle wake fades it.
        let root = w.parts().root;
        w.pointer(root, PointerPhase::Move);
        assert_eq!(w.store.opacity(bar), 1.0);
        let mut timers = Vec::new();
        w.store.take_timer_requests(&mut timers);
        let idle = timers.pop().expect("the idle wake");
        (idle.on_fire)(&mut w.store);
        KeyRouter::route_wakes(&mut w.store, &mut w.states, &w.bindings, &mut w.edits);
        w.frame();
        assert_eq!(w.store.opacity(bar), 0.0);

        // A press on the faded bar only brings it back.
        let play = w.parts().play;
        w.pointer(play, PointerPhase::Down);
        assert_eq!(w.store.opacity(bar), 1.0);
        assert!(
            !w.store.playback(w.controls.video()).unwrap().playing,
            "the press reached no button"
        );
    }
}
