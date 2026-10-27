//! Scroll bars: a viewport's track and thumb, its chrome, which it lays out
//! along its edge and does not scroll with its content.
//!
//! A viewport declared with [`ScrollBars::Auto`] builds a track along its
//! end edge across (its start, in a column read right to left) and a thumb
//! in it. After each layout the thumb is sized to the share of the content
//! the viewport shows and moved, by its translate alone, to where the offset
//! stands; a pull past the end shrinks it against that end. The window's
//! [`ScrollBarStyle`] says how bars look and behave: an overlay bar floats
//! over the content, shows while the offset moves or the pointer is on it,
//! and fades a moment after; a classic bar keeps its room beside the content
//! and shows while there is anything to scroll. An indicator (touch
//! systems) only shows; an interactive bar takes the pointer: the thumb
//! drags, a press on the track pages toward the press and keeps paging
//! while held, and with the jump modifier (Option on Apple systems, Shift
//! elsewhere) the thumb jumps to the press and drags from there. With the
//! pointer on it an overlay bar thickens and shows its track. A wheel over
//! the bar scrolls its viewport.

use std::time::Duration;

use viso_render::Rgba;

use crate::animation::{Easing, LookValue, Timing};
use crate::component::{AbsoluteStyle, BuildCx, LeafStyle, NodeStore};
use crate::input::{PointerButtons, PointerPhase};
use crate::layout::{AbsoluteInsets, Axis, Length, OnAxis, Placement, Sizing, Span, Vec2};
use crate::node::NodeId;
use crate::style::BoxStyle;
use crate::timer::TimerRequest;

/// Whether a viewport shows scroll bars.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScrollBars {
    /// As the window's [`ScrollBarStyle`] says.
    #[default]
    Auto,
    /// None: the viewport still scrolls.
    Hidden,
}

/// How a window's scroll bars look and behave. Logical points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollBarStyle {
    /// Whether the bar floats over the content, showing while the offset
    /// moves or the pointer is on it; else it keeps its room beside the
    /// content and shows while there is anything to scroll.
    pub overlay: bool,
    /// Whether the pointer can work it; else it only shows.
    pub interactive: bool,
    /// How thick the track is across: the room a classic bar keeps, and
    /// where the pointer finds the bar.
    pub track: f32,
    /// The thumb's thickness at rest, and with the pointer on the bar.
    pub thumb: f32,
    pub thumb_hover: f32,
    /// The room between the thumb and the track's outer edge and its ends.
    pub inset: f32,
    /// The thumb's least length.
    pub min_thumb: f32,
    /// The thumb's color at rest, with the pointer on the bar, and dragged.
    pub color: Rgba,
    pub color_hover: Rgba,
    pub color_pressed: Rgba,
    /// The track's color: a classic bar's always, an overlay bar's with the
    /// pointer on it.
    pub track_color: Rgba,
    /// How long an overlay bar stays after the offset stops moving, and how
    /// long it takes to fade.
    pub linger: Duration,
    pub fade: Duration,
}

const fn gray(v: f32, a: f32) -> Rgba {
    Rgba {
        r: v,
        g: v,
        b: v,
        a,
    }
}

impl ScrollBarStyle {
    /// macOS's overlay scroller.
    pub const fn mac(dark: bool) -> ScrollBarStyle {
        let ink = if dark { 1.0 } else { 0.0 };
        ScrollBarStyle {
            overlay: true,
            interactive: true,
            track: 15.0,
            thumb: 7.0,
            thumb_hover: 11.0,
            inset: 2.0,
            min_thumb: 20.0,
            color: gray(ink, 0.45),
            color_hover: gray(ink, 0.55),
            color_pressed: gray(ink, 0.65),
            track_color: gray(if dark { 0.2 } else { 0.95 }, 0.85),
            linger: Duration::from_millis(1000),
            fade: Duration::from_millis(250),
        }
    }

    /// iOS's scroll indicator.
    pub const fn ios(dark: bool) -> ScrollBarStyle {
        let ink = if dark { 1.0 } else { 0.0 };
        ScrollBarStyle {
            overlay: true,
            interactive: false,
            track: 7.0,
            thumb: 3.0,
            thumb_hover: 3.0,
            inset: 3.0,
            min_thumb: 36.0,
            color: gray(ink, 0.35),
            color_hover: gray(ink, 0.35),
            color_pressed: gray(ink, 0.35),
            track_color: gray(0.0, 0.0),
            linger: Duration::from_millis(500),
            fade: Duration::from_millis(250),
        }
    }

    /// Android's scroll bar.
    pub const fn android(dark: bool) -> ScrollBarStyle {
        let ink = if dark { 1.0 } else { 0.0 };
        ScrollBarStyle {
            overlay: true,
            interactive: false,
            track: 6.0,
            thumb: 4.0,
            thumb_hover: 4.0,
            inset: 1.0,
            min_thumb: 32.0,
            color: gray(ink, 0.38),
            color_hover: gray(ink, 0.38),
            color_pressed: gray(ink, 0.38),
            track_color: gray(0.0, 0.0),
            linger: Duration::from_millis(300),
            fade: Duration::from_millis(250),
        }
    }

    /// The thin overlay bar of Windows 11 and GTK 4, thickening under the
    /// pointer.
    pub const fn thin(dark: bool) -> ScrollBarStyle {
        let ink = if dark { 0.8 } else { 0.25 };
        ScrollBarStyle {
            overlay: true,
            interactive: true,
            track: 12.0,
            thumb: 3.0,
            thumb_hover: 8.0,
            inset: 2.0,
            min_thumb: 24.0,
            color: gray(ink, 0.55),
            color_hover: gray(ink, 0.7),
            color_pressed: gray(ink, 0.85),
            track_color: gray(if dark { 0.15 } else { 0.97 }, 0.8),
            linger: Duration::from_millis(1000),
            fade: Duration::from_millis(250),
        }
    }

    /// A classic bar beside the content, always shown while there is
    /// anything to scroll.
    pub const fn classic(dark: bool) -> ScrollBarStyle {
        let ink = if dark { 0.6 } else { 0.55 };
        ScrollBarStyle {
            overlay: false,
            interactive: true,
            track: 14.0,
            thumb: 8.0,
            thumb_hover: 8.0,
            inset: 3.0,
            min_thumb: 24.0,
            color: gray(ink, 0.8),
            color_hover: gray(ink * 0.8, 0.9),
            color_pressed: gray(ink * 0.6, 1.0),
            track_color: gray(if dark { 0.12 } else { 0.94 }, 1.0),
            linger: Duration::ZERO,
            fade: Duration::ZERO,
        }
    }

    /// This system's bars, in a light or `dark` appearance.
    pub const fn native(dark: bool) -> ScrollBarStyle {
        if cfg!(target_os = "macos") {
            Self::mac(dark)
        } else if cfg!(target_os = "ios") {
            Self::ios(dark)
        } else if cfg!(target_os = "android") {
            Self::android(dark)
        } else {
            Self::thin(dark)
        }
    }

    /// The room a viewport keeps beside its content for the bar.
    fn gutter(&self) -> f32 {
        if self.overlay { 0.0 } else { self.track }
    }
}

impl Default for ScrollBarStyle {
    fn default() -> Self {
        Self::native(false)
    }
}

/// What the pointer is doing to a bar.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Grip {
    /// Dragging the thumb, held `grab` along from its start.
    Thumb { grab: f32 },
    /// Paging toward the press, `along` the track.
    Page { along: f32 },
}

/// One viewport's bar.
#[derive(Debug, Clone, Copy)]
struct Bar {
    viewport: NodeId,
    track: NodeId,
    thumb: NodeId,
    axis: Axis,
    /// Where the thumb starts along the track and how long it is, as last
    /// placed; `None` while there is nothing to scroll.
    place: Option<(f32, f32)>,
    /// The offset (and pull) the thumb was last placed for.
    seen: f32,
    /// Whether the bar shows.
    shown: bool,
    hovered: bool,
    grip: Option<Grip>,
    /// Whether the offset moved since the linger timer was armed, and
    /// whether one is armed.
    moved: bool,
    lingering: bool,
}

/// A window's scroll bars and how they look.
#[derive(Debug, Default)]
pub(crate) struct Bars {
    style: ScrollBarStyle,
    bars: Vec<Bar>,
}

impl Bars {
    pub(crate) fn clear(&mut self) {
        self.bars.clear();
    }

    /// Forgets the bars of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if self.bars.is_empty() {
            return;
        }
        self.bars.retain(|b| {
            b.viewport.index() != index && b.track.index() != index && b.thumb.index() != index
        });
    }
}

/// What a pointer on a bar asks of it, applied after the handler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarInput {
    Hover(bool),
    /// A primary press `along` the track, the jump modifier held or not.
    Press {
        along: f32,
        jump: bool,
    },
    /// The pressed pointer moved to `along`.
    Drag {
        along: f32,
    },
    Release,
}

/// How long a held press on the track waits before paging on, and between
/// pages after.
const PAGE_DELAY: Duration = Duration::from_millis(300);
const PAGE_REPEAT: Duration = Duration::from_millis(50);
/// The share of the viewport a page moves.
const PAGE: f32 = 0.875;

impl BuildCx<'_> {
    /// Give viewport `viewport`, scrolling along `axis`, its bar: a chrome
    /// track child and a thumb in it.
    pub(crate) fn scroll_bar(&mut self, viewport: NodeId, axis: Axis) {
        let style = self.store.bars.style;
        let cross = match axis {
            Axis::Row => Sizing {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fixed(style.track),
            },
            Axis::Column => Sizing {
                width: Length::Fixed(style.track),
                height: Length::Fill { weight: 1.0 },
            },
        };
        self.stack.push(viewport);
        let mut thumb = None;
        let track = self.absolute(
            AbsoluteStyle {
                size: cross,
                ..AbsoluteStyle::default()
            },
            |cx| {
                cx.place(Placement::Absolute(outer_edge(axis, style.inset)));
                thumb = Some(
                    cx.leaf(LeafStyle {
                        size: thumb_size(axis, style.thumb, 0.0),
                        style: thumb_look(style.color),
                    })
                    .id(),
                );
            },
        );
        self.stack.pop();
        let (track, thumb) = (track.id(), thumb.expect("the thumb is built"));
        let store = &mut *self.store;
        store.set_chrome(track, true);
        store.set_hittable(thumb, false);
        store.set_hittable(track, false);
        store.set_opacity(track, 0.0);
        if !style.overlay {
            store.set_scroll_gutter(viewport, axis, style.gutter());
            store.set_fill(track, style.track_color);
        }
        store.set_pointer_handler(track, Box::new(move |ev| bar_pointer(ev, track, axis)));
        store.bars.bars.push(Bar {
            viewport,
            track,
            thumb,
            axis,
            place: None,
            seen: f32::NAN,
            shown: false,
            hovered: false,
            grip: None,
            moved: false,
            lingering: false,
        });
    }
}

/// Where a thumb sits in its track: against the track's outer edge across,
/// `inset` in from it and from the track's start along.
fn outer_edge(axis: Axis, inset: f32) -> AbsoluteInsets {
    match axis {
        Axis::Column => AbsoluteInsets {
            top: Some(Span::dp(0.0)),
            end: Some(Span::dp(inset)),
            ..AbsoluteInsets::NONE
        },
        Axis::Row => AbsoluteInsets {
            start: Some(Span::dp(0.0)),
            bottom: Some(Span::dp(inset)),
            ..AbsoluteInsets::NONE
        },
    }
}

fn thumb_size(axis: Axis, thick: f32, len: f32) -> Sizing {
    match axis {
        Axis::Column => Sizing::fixed(thick, len),
        Axis::Row => Sizing::fixed(len, thick),
    }
}

fn thumb_look(color: Rgba) -> BoxStyle {
    BoxStyle {
        fill: color,
        radius_pct: 0.5,
        ..BoxStyle::NONE
    }
}

/// The look properties of a thumb's length and thickness along `axis`.
fn length_of(axis: Axis, v: f32) -> LookValue {
    match axis {
        Axis::Column => LookValue::Height(v),
        Axis::Row => LookValue::Width(v),
    }
}

fn thickness_of(axis: Axis, v: f32) -> LookValue {
    match axis {
        Axis::Column => LookValue::Width(v),
        Axis::Row => LookValue::Height(v),
    }
}

fn eased(duration: Duration) -> Timing {
    Timing::eased(duration, Easing::EASE_OUT)
}

const INSTANT: Timing = Timing::INSTANT;

/// A bar's track handler: its pointer samples become [`BarInput`]s.
fn bar_pointer(ev: &mut crate::context::EventCx<'_>, track: NodeId, axis: Axis) {
    let Some(p) = ev.pointer().copied() else {
        return;
    };
    let along = ev.local(p.x, p.y).on(axis);
    let input = match p.phase {
        PointerPhase::Enter => BarInput::Hover(true),
        PointerPhase::Leave => BarInput::Hover(false),
        PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
            ev.capture_pointer(track);
            let jump = if viso_runtime::primary_is_logo() {
                p.modifiers.alt
            } else {
                p.modifiers.shift
            };
            BarInput::Press { along, jump }
        }
        PointerPhase::Move => BarInput::Drag { along },
        PointerPhase::Up => BarInput::Release,
        _ => return,
    };
    ev.stop_propagation();
    ev.bar_input(track, input);
}

impl NodeStore {
    /// How this window's scroll bars look; [`ScrollBarStyle::native`] unless
    /// set.
    pub fn scroll_bar_style(&self) -> ScrollBarStyle {
        self.bars.style
    }

    /// Make this window's scroll bars look and behave as `style`, the bars
    /// built already too.
    pub fn set_scroll_bar_style(&mut self, style: ScrollBarStyle) {
        if self.bars.style == style {
            return;
        }
        self.bars.style = style;
        for i in 0..self.bars.bars.len() {
            let bar = self.bars.bars[i];
            if !self.arena().is_live(bar.track) {
                continue;
            }
            let track = self.layout_input_mut(bar.track).size_mut();
            let across = match bar.axis {
                Axis::Column => &mut track.width,
                Axis::Row => &mut track.height,
            };
            // A style that only recolors the bar, as a light-dark swap does,
            // leaves its layout be.
            if *across != Length::Fixed(style.track) {
                *across = Length::Fixed(style.track);
                self.mark_dirty(
                    bar.track,
                    crate::dirty::DirtyClass::MEASURE | crate::dirty::DirtyClass::LAYOUT,
                );
            }
            self.set_scroll_gutter(bar.viewport, bar.axis, style.gutter());
            let under = if style.overlay {
                Rgba::TRANSPARENT
            } else {
                style.track_color
            };
            self.set_fill(bar.track, under);
            self.set_placement(
                bar.thumb,
                Placement::Absolute(outer_edge(bar.axis, style.inset)),
            );
            self.transition(bar.thumb, thickness_of(bar.axis, style.thumb), INSTANT);
            self.transition(bar.thumb, LookValue::Fill(style.color), INSTANT);
            self.bars.bars[i].place = None;
            self.bars.bars[i].seen = f32::NAN;
            if !style.overlay || !style.interactive {
                self.bars.bars[i].hovered = false;
            }
        }
    }

    /// The room viewport `id` keeps for a classic bar along `axis`: beside a
    /// vertical one, under a horizontal one.
    pub(crate) fn set_scroll_gutter(&mut self, id: NodeId, axis: Axis, gutter: f32) {
        if let crate::layout::LayoutInput::Scroll { gutter: g, .. } = self.layout_input_mut(id)
            && match axis {
                Axis::Column => g.x,
                Axis::Row => g.y,
            } != gutter
        {
            match axis {
                Axis::Column => g.x = gutter,
                Axis::Row => g.y = gutter,
            }
            self.mark_dirty(
                id,
                crate::dirty::DirtyClass::MEASURE | crate::dirty::DirtyClass::LAYOUT,
            );
        }
    }

    /// The thumb of viewport `id`'s bar along its axis, if it has one.
    pub fn scroll_thumb(&self, id: NodeId) -> Option<NodeId> {
        self.bars
            .bars
            .iter()
            .find(|b| b.viewport == id)
            .map(|b| b.thumb)
    }

    /// The thumb of viewport `id`'s bar along `axis`, if it has one.
    pub fn scroll_thumb_on(&self, id: NodeId, axis: Axis) -> Option<NodeId> {
        self.bars
            .bars
            .iter()
            .find(|b| b.viewport == id && b.axis == axis)
            .map(|b| b.thumb)
    }

    /// Whether viewport `id`'s bar shows.
    pub fn scroll_bar_shown(&self, id: NodeId) -> bool {
        self.bars.bars.iter().any(|b| b.viewport == id && b.shown)
    }

    /// Sizing and move every bar's thumb for its viewport as it now stands,
    /// showing a bar whose offset moved. Returns whether a thumb's length
    /// changed, so the layout runs again for it.
    pub(crate) fn sync_scroll_bars(&mut self) -> bool {
        let mut resized = false;
        for i in 0..self.bars.bars.len() {
            resized |= self.sync_bar(i);
        }
        resized
    }

    fn sync_bar(&mut self, i: usize) -> bool {
        let bar = self.bars.bars[i];
        if !self.arena().is_live(bar.viewport) || !self.arena().is_live(bar.thumb) {
            return false;
        }
        let style = self.bars.style;
        let axis = bar.axis;
        let range = self.scroll_range(bar.viewport, axis);
        // A pull shows along the viewport's own axis.
        let pull = if self.scroll_axis(bar.viewport) == Some(axis) {
            self.overscroll(bar.viewport)
        } else {
            0.0
        };
        let offset = self.scroll(bar.viewport).on(axis);
        let track = self.bounds(bar.track);
        let room = match axis {
            Axis::Column => track.h,
            Axis::Row => track.w,
        } - 2.0 * style.inset;
        if range <= 0.0 || room <= 0.0 {
            if bar.place.is_some() || bar.shown {
                self.bars.bars[i].place = None;
                self.show_bar(i, false, INSTANT);
            }
            return false;
        }
        let gutter = self.scroll_gutter(bar.viewport);
        let view = match axis {
            Axis::Column => self.bounds(bar.viewport).h - gutter.y,
            Axis::Row => self.bounds(bar.viewport).w - gutter.x,
        };
        let full = (room * view / (view + range)).clamp(style.min_thumb.min(room), room);
        // A pull past the end squeezes the thumb against it.
        let len = (full - pull.abs()).max(style.thumb.min(full));
        let (least, _) = self.scroll_span(bar.viewport, axis);
        let share = ((offset - least) / range).clamp(0.0, 1.0);
        let start = if pull > 0.0 {
            room - len
        } else if pull < 0.0 {
            0.0
        } else {
            (room - full) * share
        } + style.inset;
        let was = bar.place;
        self.bars.bars[i].place = Some((start, len));
        let mut resized = false;
        if was.is_none_or(|(_, l)| l != len) {
            self.transition(bar.thumb, length_of(axis, len), INSTANT);
            resized = true;
        }
        // The thumb lays out at the track's start (its right end, along a
        // row read right to left); its translate does the rest.
        let rtl = axis == Axis::Row
            && self.direction_at(bar.track.index()) == crate::adaptive::LayoutDirection::Rtl;
        let base = if rtl {
            room + 2.0 * style.inset - len
        } else {
            0.0
        };
        let shift = start - base;
        let translate = match axis {
            Axis::Column => Vec2 { x: 0.0, y: shift },
            Axis::Row => Vec2 { x: shift, y: 0.0 },
        };
        if self.translate(bar.thumb) != translate {
            self.set_translate(bar.thumb, translate);
        }
        let seen = offset + pull;
        let moved = !bar.seen.is_nan() && seen != bar.seen;
        self.bars.bars[i].seen = seen;
        if !style.overlay {
            if !bar.shown {
                self.show_bar(i, true, INSTANT);
            }
        } else if moved || (bar.hovered && !bar.shown) {
            self.bars.bars[i].moved = true;
            if !bar.shown {
                self.show_bar(i, true, eased(Duration::from_millis(100)));
            }
            self.linger(i);
        }
        resized
    }

    /// Show bar `i`, or fade it, over `timing`.
    fn show_bar(&mut self, i: usize, shown: bool, timing: Timing) {
        let bar = self.bars.bars[i];
        self.bars.bars[i].shown = shown;
        let style = self.bars.style;
        self.transition(
            bar.track,
            LookValue::Opacity(if shown { 1.0 } else { 0.0 }),
            timing,
        );
        self.set_hittable(bar.track, shown && style.interactive);
    }

    /// Arm bar `i`'s linger timer unless one is armed: when it fires, the
    /// bar fades unless the offset moved since or the pointer holds it.
    fn linger(&mut self, i: usize) {
        let bar = self.bars.bars[i];
        if bar.lingering {
            return;
        }
        self.bars.bars[i].lingering = true;
        self.bars.bars[i].moved = false;
        let track = bar.track;
        let delay = self.bars.style.linger;
        self.queue_timer(TimerRequest::new(track, delay, move |store| {
            store.lingered(track);
        }));
    }

    fn lingered(&mut self, track: NodeId) {
        let Some(i) = self.bars.bars.iter().position(|b| b.track == track) else {
            return;
        };
        let bar = self.bars.bars[i];
        self.bars.bars[i].lingering = false;
        if !self.bars.style.overlay || !bar.shown {
            return;
        }
        if bar.moved || bar.hovered || bar.grip.is_some() {
            self.linger(i);
            return;
        }
        let fade = eased(self.bars.style.fade);
        self.show_bar(i, false, fade);
    }

    /// Apply what a pointer on the bar with track `track` asked.
    pub(crate) fn bar_input(&mut self, track: NodeId, input: BarInput) {
        let Some(i) = self.bars.bars.iter().position(|b| b.track == track) else {
            return;
        };
        let style = self.bars.style;
        let bar = self.bars.bars[i];
        match input {
            BarInput::Hover(on) => {
                if bar.hovered == on {
                    return;
                }
                self.bars.bars[i].hovered = on;
                if !style.overlay {
                    return;
                }
                let thick = if on || bar.grip.is_some() {
                    style.thumb_hover
                } else {
                    style.thumb
                };
                let quick = eased(Duration::from_millis(120));
                self.transition(bar.thumb, thickness_of(bar.axis, thick), quick);
                let color = if on { style.color_hover } else { style.color };
                if bar.grip.is_none() {
                    self.transition(bar.thumb, LookValue::Fill(color), quick);
                }
                let under = if on || bar.grip.is_some() {
                    style.track_color
                } else {
                    Rgba::TRANSPARENT
                };
                self.transition(bar.track, LookValue::Fill(under), quick);
                if !on {
                    self.linger(i);
                }
            }
            BarInput::Press { along, jump } => {
                let Some((start, len)) = bar.place else {
                    return;
                };
                self.stop_coast(bar.viewport);
                self.transition(bar.thumb, LookValue::Fill(style.color_pressed), INSTANT);
                if (start..start + len).contains(&along) {
                    self.bars.bars[i].grip = Some(Grip::Thumb {
                        grab: along - start,
                    });
                } else if jump {
                    self.bars.bars[i].grip = Some(Grip::Thumb { grab: len / 2.0 });
                    self.drag_thumb(i, along);
                } else {
                    self.bars.bars[i].grip = Some(Grip::Page { along });
                    self.page(i);
                    self.queue_page(track, PAGE_DELAY);
                }
            }
            BarInput::Drag { along } => match bar.grip {
                Some(Grip::Thumb { .. }) => self.drag_thumb(i, along),
                Some(Grip::Page { .. }) => self.bars.bars[i].grip = Some(Grip::Page { along }),
                None => {}
            },
            BarInput::Release => {
                if bar.grip.is_none() {
                    return;
                }
                self.bars.bars[i].grip = None;
                let quick = eased(Duration::from_millis(120));
                let color = if bar.hovered {
                    style.color_hover
                } else {
                    style.color
                };
                self.transition(bar.thumb, LookValue::Fill(color), quick);
                if !bar.hovered && style.overlay {
                    self.transition(bar.thumb, thickness_of(bar.axis, style.thumb), quick);
                    self.transition(bar.track, LookValue::Fill(Rgba::TRANSPARENT), quick);
                    self.linger(i);
                }
            }
        }
    }

    /// Bar `i`'s thumb dragged so the pointer, `along` the track, holds it
    /// where it grabbed it: the offset follows.
    fn drag_thumb(&mut self, i: usize, along: f32) {
        let bar = self.bars.bars[i];
        let (Some(Grip::Thumb { grab }), Some((_, len))) = (bar.grip, bar.place) else {
            return;
        };
        let style = self.bars.style;
        let room = match bar.axis {
            Axis::Column => self.bounds(bar.track).h,
            Axis::Row => self.bounds(bar.track).w,
        } - 2.0 * style.inset;
        let travel = (room - len).max(f32::EPSILON);
        let share = ((along - grab - style.inset) / travel).clamp(0.0, 1.0);
        let range = self.scroll_range(bar.viewport, bar.axis);
        let (least, _) = self.scroll_span(bar.viewport, bar.axis);
        let mut offset = self.scroll(bar.viewport);
        match bar.axis {
            Axis::Column => offset.y = least + share * range,
            Axis::Row => offset.x = least + share * range,
        }
        self.set_scroll(bar.viewport, offset);
    }

    /// Bar `i` pages once toward its press; whether the thumb has yet to
    /// reach it.
    fn page(&mut self, i: usize) -> bool {
        let bar = self.bars.bars[i];
        let (Some(Grip::Page { along }), Some((start, len))) = (bar.grip, bar.place) else {
            return false;
        };
        if (start..start + len).contains(&along) {
            return false;
        }
        let view = match bar.axis {
            Axis::Column => self.bounds(bar.viewport).h,
            Axis::Row => self.bounds(bar.viewport).w,
        };
        let step = view * PAGE * if along < start { -1.0 } else { 1.0 };
        let delta = match bar.axis {
            Axis::Column => Vec2 { x: 0.0, y: step },
            Axis::Row => Vec2 { x: step, y: 0.0 },
        };
        self.scroll_by(bar.viewport, delta);
        // The thumb's place follows at once, for the next page's check.
        self.sync_bar(i);
        true
    }

    fn queue_page(&mut self, track: NodeId, delay: Duration) {
        self.queue_timer(TimerRequest::new(track, delay, move |store| {
            let Some(i) = store.bars.bars.iter().position(|b| b.track == track) else {
                return;
            };
            if matches!(store.bars.bars[i].grip, Some(Grip::Page { .. })) && store.page(i) {
                store.queue_page(track, PAGE_REPEAT);
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adaptive::LayoutDirection;
    use crate::binding::BindingTable;
    use crate::component::{FlexStyle, ScrollStyle};
    use crate::dirty::DirtyClass;
    use crate::input::{Modifiers, PointerEvent, route_pointer};
    use crate::state::StateStore;
    use viso_render::Rect;

    const VIEW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 100.0,
        h: 200.0,
    };

    /// A 100 by 200 viewport over `long` points of content, its bars `style`.
    struct Page {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        viewport: NodeId,
        content: NodeId,
        chain: Vec<NodeId>,
    }

    fn page(style: ScrollBarStyle, long: f32, rtl: bool) -> Page {
        let mut store = NodeStore::new();
        store.set_scroll_bar_style(style);
        let mut content = None;
        let viewport = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    size: Sizing::fixed(VIEW.w, VIEW.h),
                    ..ScrollStyle::default()
                },
                |cx| {
                    content = Some(
                        cx.flex(
                            FlexStyle {
                                size: Sizing {
                                    width: Length::Fill { weight: 1.0 },
                                    height: Length::Fixed(long),
                                },
                                ..FlexStyle::default()
                            },
                            |_| {},
                        )
                        .id(),
                    );
                },
            )
            .id()
        };
        if rtl {
            store.set_layout_direction(viewport, Some(LayoutDirection::Rtl));
        }
        store.layout(viewport, VIEW, &mut Vec::new());
        Page {
            store,
            states: StateStore::new(),
            bindings: BindingTable::new(),
            viewport,
            content: content.unwrap(),
            chain: Vec::new(),
        }
    }

    impl Page {
        fn frame(&mut self) {
            self.store
                .relayout_dirty(self.viewport, VIEW, &mut Vec::new(), &mut Vec::new());
        }

        fn scroll_to(&mut self, y: f32) {
            self.store.set_scroll(self.viewport, Vec2 { x: 0.0, y });
            self.frame();
        }

        fn thumb(&self) -> NodeId {
            self.store.scroll_thumb(self.viewport).unwrap()
        }

        fn track(&self) -> NodeId {
            self.store.parent(self.thumb()).unwrap()
        }

        /// Where the thumb shows along the viewport, and how long it is.
        fn shown(&self) -> (f32, f32) {
            let thumb = self.thumb();
            let b = self.store.bounds(thumb);
            (b.y + self.store.translate(thumb).y, b.h)
        }

        fn pointer(&mut self, y: f32, phase: PointerPhase, modifiers: Modifiers) {
            let ev = PointerEvent {
                x: 95.0,
                y,
                phase,
                buttons: if phase == PointerPhase::Up {
                    PointerButtons::NONE
                } else {
                    PointerButtons::PRIMARY
                },
                modifiers,
            };
            route_pointer(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.viewport,
                ev,
                &mut self.chain,
            );
            self.frame();
        }

        /// Fire the timers queued so far, in order; how many there were.
        fn fire_timers(&mut self) -> usize {
            let mut due = Vec::new();
            self.store.take_timer_requests(&mut due);
            let n = due.len();
            for req in due {
                (req.on_fire)(&mut self.store);
            }
            self.frame();
            n
        }

        fn offset(&self) -> f32 {
            self.store.scroll(self.viewport).y
        }
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.05
    }

    /// The thin bar: a 12 point track, the thumb 2 in from its ends, at
    /// least 24 long. Over 1000 points the viewport shows a fifth: a thumb
    /// 196 / 5 long, travelling 196 - 39.2.
    const THIN: ScrollBarStyle = ScrollBarStyle::thin(false);

    #[test]
    fn the_thumb_is_the_shown_share_and_moves_by_its_translate_alone() {
        let mut p = page(THIN, 1000.0, false);
        let track = p.store.bounds(p.track());
        assert_eq!((track.x, track.w, track.h), (88.0, 12.0, 200.0));
        assert_eq!(
            p.store.bounds(p.content).w,
            100.0,
            "an overlay keeps no room"
        );
        let (at, len) = p.shown();
        assert!(near(at, 2.0) && near(len, 39.2), "{at} {len}");
        assert!(!p.store.scroll_bar_shown(p.viewport), "nothing moved yet");

        p.store.clear_dirty();
        p.scroll_to(400.0);
        let (at, len) = p.shown();
        assert!(near(at, 2.0 + 156.8 * 0.5) && near(len, 39.2), "{at} {len}");
        let dirty = p.store.dirty(p.thumb());
        assert!(
            !dirty.intersects(DirtyClass::MEASURE | DirtyClass::LAYOUT),
            "{dirty:?}"
        );
        assert!(p.store.scroll_bar_shown(p.viewport), "the offset moved");
        // The bar does not scroll with the content.
        assert_eq!(p.store.world(p.track()).y, 0.0);

        // A tall viewport over a little content: a short thumb, at least 24.
        let p = page(THIN, 20_000.0, false);
        assert!(near(p.shown().1, 24.0));
        // Nothing to scroll: no thumb shows.
        let p = page(THIN, 150.0, false);
        assert!(!p.store.scroll_bar_shown(p.viewport));
    }

    #[test]
    fn an_overlay_bar_fades_a_while_after_the_offset_stops() {
        let mut p = page(THIN, 1000.0, false);
        p.scroll_to(100.0);
        assert!(p.store.scroll_bar_shown(p.viewport));
        // Moving again before the linger ends keeps it.
        p.scroll_to(150.0);
        assert_eq!(p.fire_timers(), 1);
        assert!(p.store.scroll_bar_shown(p.viewport), "it moved since");
        assert_eq!(p.fire_timers(), 1, "armed again");
        assert!(!p.store.scroll_bar_shown(p.viewport), "faded");
        assert!(!p.store.hittable(p.track()), "a faded bar takes no pointer");
    }

    #[test]
    fn the_pointer_on_an_overlay_bar_thickens_it_and_holds_it() {
        let mut p = page(THIN, 1000.0, false);
        p.scroll_to(100.0);
        let track = p.track();
        p.store.bar_input(track, BarInput::Hover(true));
        p.store.tick_transitions(Duration::from_secs(1));
        p.frame();
        assert_eq!(p.store.bounds(p.thumb()).w, THIN.thumb_hover);
        assert_eq!(p.store.style(track).fill, THIN.track_color);
        p.fire_timers();
        assert!(p.store.scroll_bar_shown(p.viewport), "held");
        p.store.bar_input(track, BarInput::Hover(false));
        p.store.tick_transitions(Duration::from_secs(1));
        p.fire_timers();
        assert!(!p.store.scroll_bar_shown(p.viewport));
        assert_eq!(p.store.bounds(p.thumb()).w, THIN.thumb);
    }

    #[test]
    fn the_thumb_drags_and_the_offset_follows() {
        let mut p = page(THIN, 1000.0, false);
        p.scroll_to(400.0);
        let none = Modifiers::default();
        // Grabbed 19.6 points down, at its middle.
        p.pointer(100.0, PointerPhase::Down, none);
        p.pointer(150.0, PointerPhase::Move, none);
        let share = (150.0 - 19.6 - 2.0) / 156.8;
        assert!(near(p.offset(), 800.0 * share), "{}", p.offset());
        p.pointer(400.0, PointerPhase::Move, none);
        assert_eq!(p.offset(), 800.0, "held at the end");
        p.pointer(400.0, PointerPhase::Up, none);
        assert_eq!(p.offset(), 800.0);
    }

    #[test]
    fn a_press_on_the_track_pages_toward_it_until_the_thumb_gets_there() {
        let mut p = page(THIN, 1000.0, false);
        p.scroll_to(1.0);
        p.scroll_to(0.0);
        p.pointer(190.0, PointerPhase::Down, Modifiers::default());
        assert_eq!(p.offset(), 175.0, "a page is seven eighths of the view");
        // Held, it pages on (and the bar stays) until the thumb is under it.
        let mut seen = vec![p.offset()];
        for _ in 0..8 {
            p.fire_timers();
            seen.push(p.offset());
        }
        assert_eq!(&seen[..5], [175.0, 350.0, 525.0, 700.0, 800.0]);
        assert_eq!(p.offset(), 800.0);
        assert!(p.store.scroll_bar_shown(p.viewport));
        let (at, len) = p.shown();
        assert!((at..at + len).contains(&190.0), "under the press");
        p.pointer(190.0, PointerPhase::Up, Modifiers::default());
    }

    #[test]
    fn the_jump_modifier_takes_the_thumb_to_the_press() {
        let mut p = page(THIN, 1000.0, false);
        p.scroll_to(1.0);
        p.scroll_to(0.0);
        let jump = if viso_runtime::primary_is_logo() {
            Modifiers {
                alt: true,
                ..Modifiers::default()
            }
        } else {
            Modifiers {
                shift: true,
                ..Modifiers::default()
            }
        };
        p.pointer(150.0, PointerPhase::Down, jump);
        let share = (150.0 - 19.6 - 2.0) / 156.8;
        assert!(near(p.offset(), 800.0 * share), "{}", p.offset());
        let (at, len) = p.shown();
        assert!(near(at + len / 2.0, 150.0), "centred on the press");
    }

    #[test]
    fn a_classic_bar_keeps_its_room_and_shows_while_there_is_anything_to_scroll() {
        let classic = ScrollBarStyle::classic(false);
        let mut p = page(classic, 1000.0, false);
        assert_eq!(p.store.bounds(p.content).w, 86.0);
        assert_eq!(p.store.bounds(p.track()).x, 86.0);
        assert!(p.store.scroll_bar_shown(p.viewport));
        assert_eq!(p.fire_timers(), 0, "it does not fade");

        // Read right to left, the bar is on the left.
        let p = page(classic, 1000.0, true);
        assert_eq!(p.store.bounds(p.track()).x, 0.0);
        assert_eq!(p.store.bounds(p.content).x, 14.0);

        // Restyled in place, the room goes and the bar floats.
        let mut p = page(classic, 1000.0, false);
        p.store.set_scroll_bar_style(THIN);
        p.frame();
        assert_eq!(p.store.bounds(p.content).w, 100.0);
        assert_eq!(p.store.bounds(p.track()).x, 88.0);
    }
}
