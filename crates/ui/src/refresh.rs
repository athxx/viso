//! Pull to refresh: a column viewport pulled down past its start asks its
//! content to refresh.
//!
//! A refreshable viewport ([`NodeStore::set_refreshable`]) takes a pull past
//! its start even where the window's viewports end hard there, and even with
//! nothing to scroll. The window's [`RefreshStyle`] says how it shows:
//!
//! - **Inset** (Apple): the content follows the finger down and a spinner
//!   shows in the gap it opens, its spokes appearing one by one as the pull
//!   nears the mark. Reaching the mark while the finger holds it refreshes:
//!   the range gains a gap at its start, so the content rests that far down
//!   with the spinner spinning in the gap and scrolls over it as over any
//!   content; ended, the gap springs shut.
//! - **Overlay** (Material): the content stays put and a puck carrying the
//!   spinner slides down over it with the pull, the arc growing; let go past
//!   the mark it refreshes, the puck easing to rest below the top, spinning;
//!   short of it, the puck slides back up. Ended, the puck shrinks away.
//!
//! A refresh the pull starts is told to the viewport's key handler on the
//! next frame ([`EventCx::refresh`](crate::context::EventCx::refresh));
//! [`NodeStore::set_refreshing`] ends it, or starts one without a pull. A
//! controlled viewport ([`NodeStore::set_refresh_controlled`]) refreshes only
//! while its owner says so: a refresh it was told of that no
//! `set_refreshing(true)` confirmed by the next layout ends there. Everything
//! moves by translates, rotations and the spinner's paint, never a layout.

use std::time::Duration;

use viso_render::Rgba;

use crate::animation::{Easing, LookValue, Timing};
use crate::component::{BuildCx, NodeStore, StackStyle};
use crate::dirty::DirtyClass;
use crate::layout::{Alignment2D, Axis, OnAxis, Size, Vec2};
use crate::node::NodeId;
use crate::shadow::Shadow;
use crate::spinner::SpinnerLook;
use crate::style::BoxStyle;
use crate::timer::TimerRequest;

/// How a window's refreshable viewports show a pull and a refresh. Logical
/// points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RefreshStyle {
    /// Whether the spinner shows in the gap the pull opens above the content,
    /// which holds a gap of `hold` open while refreshing; else a puck
    /// carrying it slides down over the still content and rests `hold` down.
    pub inset: bool,
    /// How far the pull must reach to refresh: the gap while the finger holds
    /// it (inset), the puck's travel when it lets go.
    pub threshold: f32,
    pub hold: f32,
    pub look: SpinnerLook,
    /// The spinner's side and color.
    pub side: f32,
    pub color: Rgba,
    /// The puck's diameter and fill; none inset.
    pub puck: f32,
    pub puck_color: Rgba,
}

impl RefreshStyle {
    /// `UIRefreshControl`'s: gray spokes in the gap.
    pub const fn apple(_dark: bool) -> RefreshStyle {
        RefreshStyle {
            inset: true,
            threshold: 90.0,
            hold: 60.0,
            look: SpinnerLook::Spokes,
            side: 28.0,
            // systemGray, alike in both schemes.
            color: Rgba::new(0.2705, 0.2705, 0.2918, 1.0),
            puck: 0.0,
            puck_color: Rgba::TRANSPARENT,
        }
    }

    /// Material 3's: an arc in a raised puck.
    pub const fn material(dark: bool) -> RefreshStyle {
        // onSurfaceVariant over surfaceContainerHigh.
        let (color, puck_color) = if dark {
            (
                Rgba::new(0.5906, 0.552, 0.6308, 1.0),
                Rgba::new(0.0242, 0.0222, 0.0296, 1.0),
            )
        } else {
            (
                Rgba::new(0.0666, 0.0595, 0.0782, 1.0),
                Rgba::new(0.8389, 0.7912, 0.8713, 1.0),
            )
        };
        RefreshStyle {
            inset: false,
            threshold: 64.0,
            hold: 64.0,
            look: SpinnerLook::Arc,
            side: 24.0,
            color,
            puck: 40.0,
            puck_color,
        }
    }

    /// This system's.
    pub fn native(dark: bool) -> RefreshStyle {
        if cfg!(any(target_os = "macos", target_os = "ios")) {
            Self::apple(dark)
        } else {
            Self::material(dark)
        }
    }

    /// The indicator's side: the puck, or the spinner alone.
    fn side_of(&self) -> f32 {
        self.puck.max(self.side)
    }

    /// How far the pull shows at most: twice the mark.
    pub(crate) fn reach(&self) -> f32 {
        self.threshold * 2.0
    }
}

impl Default for RefreshStyle {
    fn default() -> Self {
        Self::native(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    Refreshing,
    /// Ended, until the gap shuts or the puck has shrunk away.
    Ending,
}

/// How a pull past a viewport's end shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PullLook {
    /// The content moves with it.
    Shift,
    /// The content stretches.
    Stretch,
    /// The content stays: a refresher's puck shows it.
    Still,
}

/// One refreshable viewport.
#[derive(Debug, Clone, Copy)]
struct Refresher {
    viewport: NodeId,
    layer: NodeId,
    puck: NodeId,
    spinner: NodeId,
    enabled: bool,
    phase: Phase,
    /// A puck's pull past the mark, for the lift.
    armed: bool,
    /// A refresh it asked for, not yet told; one told, not yet confirmed.
    asked: bool,
    told: bool,
    controlled: bool,
    /// The gap the range gained at its start while refreshing (inset).
    held: f32,
    /// A puck's travel down from hidden, its speed, and whether it eases on
    /// its own rather than following a held pull.
    travel: f32,
    speed: f32,
    easing: bool,
}

/// A window's refreshable viewports and how they show.
#[derive(Debug, Default)]
pub(crate) struct Refreshers {
    style: RefreshStyle,
    list: Vec<Refresher>,
}

impl Refreshers {
    pub(crate) fn clear(&mut self) {
        self.list.clear();
    }

    /// Forgets the refresher of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if self.list.is_empty() {
            return;
        }
        self.list.retain(|r| {
            r.viewport.index() != index
                && r.layer.index() != index
                && r.puck.index() != index
                && r.spinner.index() != index
        });
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    fn of(&self, viewport: NodeId) -> Option<&Refresher> {
        self.list
            .iter()
            .find(|r| r.viewport == viewport && r.enabled)
    }

    /// The gap viewport `viewport`'s range gained at its start.
    #[inline]
    pub(crate) fn held(&self, viewport: NodeId) -> f32 {
        if self.list.is_empty() {
            return 0.0;
        }
        self.of(viewport).map_or(0.0, |r| r.held)
    }

    /// Whether a puck eases.
    pub(crate) fn easing(&self) -> bool {
        self.list.iter().any(|r| r.easing)
    }
}

/// How long an ended puck takes to shrink away, and a spinner to brighten
/// or dim as its pull crosses the mark.
const SHRINK: Duration = Duration::from_millis(150);
const FADE: Duration = Duration::from_millis(300);
/// A puck's ease, as a smooth scroll's.
const EASE: f32 = 14.0;
/// A puck's spinner short of the mark shows faint.
const FAINT: f32 = 0.3;

fn eased(duration: Duration) -> Timing {
    Timing {
        duration,
        delay: Duration::ZERO,
        easing: Easing::EaseOut,
    }
}

const INSTANT: Timing = Timing {
    duration: Duration::ZERO,
    delay: Duration::ZERO,
    easing: Easing::Linear,
};

impl BuildCx<'_> {
    /// Make the column viewport `handle` names refresh when pulled down past
    /// its start ([`NodeStore::set_refreshable`]).
    pub fn refreshable(&mut self, handle: crate::component::Handle, on: bool) {
        self.store.set_refreshable(handle.id(), on);
    }

    /// Give viewport `viewport` its refresh indicator: a chrome layer over
    /// it holding the puck, and the spinner in it.
    fn refresher(&mut self, viewport: NodeId) {
        let style = self.store.refreshers.style;
        let side = style.side_of();
        self.stack.push(viewport);
        let mut puck = None;
        let mut spinner = None;
        let layer = self.stack(
            StackStyle {
                align: Alignment2D::Top,
                size: Size::fill(),
                ..StackStyle::default()
            },
            |cx| {
                let p = cx.stack(
                    StackStyle {
                        align: Alignment2D::Center,
                        size: Size::fixed(side, side),
                        style: BoxStyle {
                            fill: style.puck_color,
                            radius_pct: 0.5,
                            ..BoxStyle::NONE
                        },
                        ..StackStyle::default()
                    },
                    |cx| {
                        spinner = Some(cx.spinner(style.look, style.side, style.color).id());
                    },
                );
                puck = Some(p.id());
            },
        );
        self.stack.pop();
        let (layer, puck) = (layer.id(), puck.expect("the puck is built"));
        let spinner = spinner.expect("the spinner is built");
        let store = &mut *self.store;
        store.set_chrome(layer, true);
        store.set_hittable(layer, false);
        store.set_hittable(puck, false);
        store.set_transform_origin(puck, Alignment2D::Center);
        store.set_opacity(puck, 0.0);
        store.set_spinner_progress(spinner, 0.0);
        store.set_translate(puck, Vec2 { x: 0.0, y: -side });
        if !style.inset {
            store.set_shadow(puck, Some(puck_shadow()));
        }
        store.refreshers.list.push(Refresher {
            viewport,
            layer,
            puck,
            spinner,
            enabled: true,
            phase: Phase::Idle,
            armed: false,
            asked: false,
            told: false,
            controlled: false,
            held: 0.0,
            travel: 0.0,
            speed: 0.0,
            easing: false,
        });
    }
}

fn puck_shadow() -> Shadow {
    Shadow::below(Rgba::new(0.0, 0.0, 0.0, 0.3), 1.0, 3.0)
}

impl NodeStore {
    /// How this window's refreshable viewports show; [`RefreshStyle::native`]
    /// unless set.
    pub fn refresh_style(&self) -> RefreshStyle {
        self.refreshers.style
    }

    /// Make this window's refreshable viewports show as `style`, those built
    /// already too.
    pub fn set_refresh_style(&mut self, style: RefreshStyle) {
        let was = self.refreshers.style;
        if was == style {
            return;
        }
        // A gap held open closes before the look changes from under it.
        if was.inset != style.inset {
            for i in 0..self.refreshers.list.len() {
                self.drop_refresh_gap(i);
            }
        }
        self.refreshers.style = style;
        let side = style.side_of();
        for i in 0..self.refreshers.list.len() {
            let r = self.refreshers.list[i];
            if !self.arena().is_live(r.puck) {
                continue;
            }
            *self.layout_input_mut(r.puck).size_mut() = Size::fixed(side, side);
            *self.layout_input_mut(r.spinner).size_mut() = Size::fixed(style.side, style.side);
            self.mark_dirty(r.puck, DirtyClass::MEASURE | DirtyClass::LAYOUT);
            self.mark_dirty(r.spinner, DirtyClass::MEASURE | DirtyClass::LAYOUT);
            self.set_fill(r.puck, style.puck_color);
            self.set_shadow(r.puck, (!style.inset).then(puck_shadow));
            let spinning = self.spinner(r.spinner).is_some_and(|s| s.spinning);
            self.set_spinning(r.spinner, false);
            if let Some(spinner) = self.spinner_mut(r.spinner) {
                spinner.look = style.look;
                spinner.color = style.color;
                spinner.natural = Vec2 {
                    x: style.side,
                    y: style.side,
                };
            }
            self.mark_dirty(r.spinner, DirtyClass::PAINT);
            self.set_spinning(r.spinner, spinning);
            if r.phase == Phase::Refreshing && !style.inset {
                self.refreshers.list[i].easing = true;
            }
        }
        self.sync_refreshers();
    }

    /// Make column viewport `id` refresh when pulled down past its start, or
    /// stop it; a refresh under way ends. A stale handle or a node that
    /// scrolls no column is a no-op.
    pub fn set_refreshable(&mut self, id: NodeId, on: bool) {
        if !self.arena().is_live(id) || self.scroll_axis(id) != Some(Axis::Column) {
            return;
        }
        match self.refreshers.list.iter().position(|r| r.viewport == id) {
            Some(i) => {
                if self.refreshers.list[i].enabled == on {
                    return;
                }
                if !on {
                    self.drop_refresh_gap(i);
                    self.reset(i);
                }
                self.refreshers.list[i].enabled = on;
                let layer = self.refreshers.list[i].layer;
                self.set_hidden(layer, !on);
            }
            None if on => {
                BuildCx::new(self).refresher(id);
                self.mark_dirty(
                    id,
                    DirtyClass::STRUCTURE | DirtyClass::LAYOUT | DirtyClass::PAINT,
                );
            }
            None => {}
        }
    }

    /// Whether viewport `id` refreshes when pulled.
    pub fn is_refreshable(&self, id: NodeId) -> bool {
        self.refreshers.of(id).is_some()
    }

    /// Whether viewport `id` refreshes.
    pub fn is_refreshing(&self, id: NodeId) -> bool {
        self.refreshers
            .of(id)
            .is_some_and(|r| r.phase == Phase::Refreshing)
    }

    /// The puck of viewport `id`'s refresh indicator (the spinner's holder,
    /// inset), and its spinner.
    pub fn refresh_indicator(&self, id: NodeId) -> Option<(NodeId, NodeId)> {
        self.refreshers.of(id).map(|r| (r.puck, r.spinner))
    }

    /// Make viewport `id` refreshing, showing its spinner, or end its
    /// refresh. Started so, it asks for nothing: its owner knows. Started at
    /// the top, an inset viewport glides down to show the spinner.
    pub fn set_refreshing(&mut self, id: NodeId, on: bool) {
        let Some(i) = self.refresher_at(id) else {
            return;
        };
        self.refreshers.list[i].told = false;
        let phase = self.refreshers.list[i].phase;
        match (on, phase) {
            (true, Phase::Refreshing) | (false, Phase::Idle | Phase::Ending) => {}
            (true, _) => {
                self.begin(i);
                let style = self.refreshers.style;
                if style.inset && self.scroll(id).y <= 0.5 {
                    self.glide_to(
                        id,
                        Vec2 {
                            x: 0.0,
                            y: -style.hold,
                        },
                    );
                }
            }
            (false, Phase::Refreshing) => self.end(i),
        }
    }

    /// Refresh viewport `id` as a pull would: it shows refreshing and asks
    /// its key handler on the next frame. A viewport refreshing already, or
    /// not refreshable, is a no-op. Returns whether it started.
    pub fn begin_refresh(&mut self, id: NodeId) -> bool {
        let Some(i) = self.refresher_at(id) else {
            return false;
        };
        if self.refreshers.list[i].phase == Phase::Refreshing {
            return false;
        }
        self.set_refreshing(id, true);
        self.refreshers.list[i].asked = true;
        true
    }

    /// Make viewport `id`'s refreshes its owner's to say: one a pull started
    /// lasts only if `set_refreshing(true)` follows the request by the next
    /// layout.
    pub fn set_refresh_controlled(&mut self, id: NodeId, on: bool) {
        if let Some(i) = self.refresher_at(id) {
            self.refreshers.list[i].controlled = on;
        }
    }

    /// Whether a refresh waits to be told.
    #[inline]
    pub fn refreshes_due(&self) -> bool {
        self.refreshers.list.iter().any(|r| r.asked)
    }

    /// The viewports whose refresh waits to be told, now told.
    pub(crate) fn take_refresh_asks(&mut self, out: &mut Vec<NodeId>) {
        for r in &mut self.refreshers.list {
            if std::mem::take(&mut r.asked) && r.phase == Phase::Refreshing {
                r.told = true;
                out.push(r.viewport);
            }
        }
    }

    fn refresher_at(&self, id: NodeId) -> Option<usize> {
        self.refreshers
            .list
            .iter()
            .position(|r| r.viewport == id && r.enabled && self.arena().is_live(id))
    }

    /// Start refresher `i`'s refresh: an inset one's range gains its gap, a
    /// puck eases to rest, the spinner spins.
    fn begin(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = self.refreshers.list[i];
        if r.phase == Phase::Ending && !style.inset {
            self.transition(r.puck, LookValue::Scale(1.0), INSTANT);
        }
        let r = &mut self.refreshers.list[i];
        r.phase = Phase::Refreshing;
        r.armed = true;
        let (puck, spinner) = (r.puck, r.spinner);
        if style.inset {
            r.held = style.hold;
        } else {
            r.easing = true;
        }
        self.set_opacity(puck, 1.0);
        self.transition(spinner, LookValue::Opacity(1.0), INSTANT);
        self.set_spinner_progress(spinner, 1.0);
        self.set_spinning(spinner, true);
    }

    /// End refresher `i`'s refresh: the gap springs shut, or the puck
    /// shrinks away.
    fn end(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = self.refreshers.list[i];
        self.refreshers.list[i].phase = Phase::Ending;
        self.refreshers.list[i].told = false;
        self.set_spinning(r.spinner, false);
        if style.inset {
            self.drop_refresh_gap(i);
            return;
        }
        self.refreshers.list[i].easing = false;
        self.transition(r.puck, LookValue::Scale(0.0), eased(SHRINK));
        let viewport = r.viewport;
        self.queue_timer(TimerRequest::new(r.puck, SHRINK, move |store| {
            if let Some(i) = store.refresher_at(viewport)
                && store.refreshers.list[i].phase == Phase::Ending
            {
                store.reset(i);
            }
        }));
    }

    /// Refresher `i`'s gap leaves its range: what of it shows passes to a
    /// pull that springs shut.
    fn drop_refresh_gap(&mut self, i: usize) {
        let r = self.refreshers.list[i];
        if r.held == 0.0 {
            return;
        }
        self.refreshers.list[i].held = 0.0;
        let id = r.viewport;
        let offset = self.scroll(id).y;
        // The offset clamps to the range left; the gap it stood in shows
        // pulled instead.
        let x = self.scroll(id).x;
        self.set_scroll(id, Vec2 { x, y: 0.0 });
        if offset < 0.0 {
            self.shift_pull(id, Axis::Column, offset);
        }
    }

    /// Refresher `i` back at rest, its indicator hidden.
    fn reset(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = &mut self.refreshers.list[i];
        r.phase = Phase::Idle;
        r.armed = false;
        r.asked = false;
        r.told = false;
        r.travel = 0.0;
        r.speed = 0.0;
        r.easing = false;
        let (puck, spinner) = (r.puck, r.spinner);
        self.set_spinning(spinner, false);
        self.set_spinner_progress(spinner, 0.0);
        self.transition(puck, LookValue::Scale(1.0), INSTANT);
        self.set_opacity(puck, 0.0);
        self.set_translate(
            puck,
            Vec2 {
                x: 0.0,
                y: -style.side_of(),
            },
        );
    }

    /// Whether a pull of `delta` past viewport `id`'s end along `axis` goes
    /// to its refresher: down past the start of a refreshable column, where
    /// a puck takes it only while no refresh runs.
    pub(crate) fn refresh_takes(&self, id: NodeId, axis: Axis, delta: f32) -> bool {
        if axis != Axis::Column || delta >= 0.0 {
            return false;
        }
        self.refreshers
            .of(id)
            .is_some_and(|r| self.refreshers.style.inset || r.phase == Phase::Idle && r.held == 0.0)
    }

    /// How far a pull of viewport `id` may show, when it is refreshable.
    pub(crate) fn refresh_reach(&self, id: NodeId) -> Option<f32> {
        if self.refreshers.is_empty() {
            return None;
        }
        self.refreshers
            .of(id)
            .map(|_| self.refreshers.style.reach())
    }

    /// How a pull of viewport `id` showing `shown` (made by a finger or not)
    /// looks, where the physics alone shows it as `physics`.
    pub(crate) fn refresh_pull_look(
        &self,
        id: NodeId,
        axis: Axis,
        shown: f32,
        finger: bool,
        physics: PullLook,
    ) -> PullLook {
        if axis != Axis::Column || shown >= 0.0 {
            return physics;
        }
        let Some(r) = self.refreshers.of(id) else {
            return physics;
        };
        if self.refreshers.style.inset || r.held > 0.0 {
            PullLook::Shift
        } else if finger && r.phase == Phase::Idle {
            PullLook::Still
        } else {
            physics
        }
    }

    /// The fingers let go: a puck pulled past the mark refreshes, one short
    /// of it slides back. Its pull, which never moved the content, is gone.
    pub(crate) fn release_refreshers(&mut self) {
        if self.refreshers.is_empty() || self.refreshers.style.inset {
            return;
        }
        let style = self.refreshers.style;
        for i in 0..self.refreshers.list.len() {
            let r = self.refreshers.list[i];
            if !r.enabled || r.phase != Phase::Idle || !self.take_finger_pull(r.viewport) {
                continue;
            }
            self.refreshers.list[i].easing = true;
            if r.travel >= style.threshold {
                self.begin(i);
                self.refreshers.list[i].asked = true;
            }
        }
    }

    /// Ease every puck a frame of `dt` seconds toward where it rests.
    pub(crate) fn tick_refreshers(&mut self, dt: f32) {
        if !self.refreshers.easing() {
            return;
        }
        let style = self.refreshers.style;
        for i in 0..self.refreshers.list.len() {
            let mut r = self.refreshers.list[i];
            if !r.easing {
                continue;
            }
            let target = if r.phase == Phase::Refreshing {
                style.hold
            } else {
                0.0
            };
            let e = r.travel - target;
            let decay = (-EASE * dt).exp();
            let c = r.speed + EASE * e;
            let e = (e + c * dt) * decay;
            r.speed = (r.speed - EASE * c * dt) * decay;
            r.travel = target + e;
            if e.abs() < 0.5 && r.speed.abs() < 10.0 {
                r.travel = target;
                r.speed = 0.0;
                r.easing = false;
            }
            self.refreshers.list[i] = r;
            if !r.easing && r.phase == Phase::Idle {
                self.reset(i);
            } else {
                self.place_puck(i);
            }
        }
    }

    /// After a layout: start an inset refresh pulled past the mark, end a
    /// controlled one nobody confirmed, and move each indicator to where its
    /// pull or rest puts it.
    pub(crate) fn sync_refreshers(&mut self) {
        if self.refreshers.is_empty() {
            return;
        }
        let style = self.refreshers.style;
        for i in 0..self.refreshers.list.len() {
            let r = self.refreshers.list[i];
            if !r.enabled || !self.arena().is_live(r.viewport) || !self.arena().is_live(r.puck) {
                continue;
            }
            if r.controlled && r.told && r.phase == Phase::Refreshing {
                self.end(i);
            }
            if style.inset {
                self.sync_inset(i);
            } else {
                self.sync_overlay(i);
            }
        }
    }

    fn sync_inset(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = self.refreshers.list[i];
        let id = r.viewport;
        let offset = self.scroll(id).y;
        let pulled = self.overscroll(id).min(0.0);
        let gap = (-(offset + pulled)).max(0.0);
        if r.phase == Phase::Idle && gap >= style.threshold && self.finger_pull(id) {
            self.begin(i);
            self.refreshers.list[i].asked = true;
            // The range gained the gap: the offset takes what it can of the
            // pull, so the content shows where it stood.
            let to = (offset + pulled).max(-style.hold);
            let x = self.scroll(id).x;
            self.set_scroll(id, Vec2 { x, y: to });
            self.shift_pull(id, Axis::Column, offset - to);
        } else if r.phase == Phase::Ending && gap <= 0.5 {
            self.reset(i);
            return;
        }
        let r = self.refreshers.list[i];
        if r.phase == Phase::Idle {
            self.set_spinner_progress(r.spinner, gap / style.threshold);
            self.set_opacity(r.puck, if gap > 0.0 { 1.0 } else { 0.0 });
        }
        // Halfway down the gap, then riding just above the content.
        let center = if gap <= style.hold {
            gap * 0.5
        } else {
            gap - style.hold * 0.5
        };
        let y = center - style.side_of() * 0.5;
        if self.translate(r.puck).y != y {
            self.set_translate(r.puck, Vec2 { x: 0.0, y });
        }
    }

    fn sync_overlay(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = self.refreshers.list[i];
        if r.phase == Phase::Idle && !r.easing {
            let travel = if self.finger_pull(r.viewport) {
                -self.overscroll(r.viewport).min(0.0)
            } else {
                0.0
            };
            self.refreshers.list[i].travel = travel;
            let armed = travel >= style.threshold;
            if armed != r.armed || travel > 0.0 && r.travel == 0.0 {
                self.refreshers.list[i].armed = armed;
                let opacity = if armed { 1.0 } else { FAINT };
                self.transition(r.spinner, LookValue::Opacity(opacity), eased(FADE));
            }
            self.set_spinner_progress(r.spinner, travel / style.threshold);
            self.set_opacity(r.puck, if travel > 0.0 { 1.0 } else { 0.0 });
        }
        self.place_puck(i);
    }

    /// Move refresher `i`'s puck to its travel.
    fn place_puck(&mut self, i: usize) {
        let style = self.refreshers.style;
        let r = self.refreshers.list[i];
        if style.inset {
            return;
        }
        if r.phase == Phase::Idle {
            self.set_spinner_progress(r.spinner, r.travel / style.threshold);
        }
        let y = r.travel - style.side_of();
        if self.translate(r.puck).y != y {
            self.set_translate(r.puck, Vec2 { x: 0.0, y });
        }
    }
}

impl NodeStore {
    /// How far viewport `id`'s offset stands from its start along `axis`:
    /// what lies scrolled out of view before it, none in a gap held open
    /// there.
    pub(crate) fn scroll_distance(&self, id: NodeId, axis: Axis) -> f32 {
        let offset = self.scroll(id).on(axis);
        let rtl = axis == Axis::Row
            && self.scroll_axis(id) == Some(Axis::Row)
            && self.direction_at(id.index()) == crate::adaptive::LayoutDirection::Rtl;
        if rtl { -offset } else { offset }.max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{FlexStyle, LeafStyle, ScrollStyle};
    use crate::layout::Length;
    use crate::scroll_bar::ScrollBars;
    use crate::scroll_motion::{OverscrollStyle, ScrollPhysics};
    use viso_render::Rect;

    const VIEW: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 200.0,
        h: 600.0,
    };

    /// A refreshable column of `rows` rows 50 tall, laid out, under `style`
    /// and `physics`.
    fn list(
        rows: usize,
        style: RefreshStyle,
        physics: ScrollPhysics,
    ) -> (NodeStore, NodeId, NodeId) {
        let mut store = NodeStore::new();
        store.set_refresh_style(style);
        store.set_scroll_physics(physics);
        let mut column = None;
        let viewport = BuildCx::new(&mut store)
            .scroll(
                ScrollStyle {
                    size: Size::fixed(VIEW.w, VIEW.h),
                    bars: ScrollBars::Hidden,
                    refresh: true,
                    ..ScrollStyle::default()
                },
                |cx| {
                    column = Some(
                        cx.flex(
                            FlexStyle {
                                axis: Axis::Column,
                                size: Size {
                                    width: Length::Fill { weight: 1.0 },
                                    height: Length::Fit,
                                },
                                ..FlexStyle::default()
                            },
                            |cx| {
                                for _ in 0..rows {
                                    cx.leaf(LeafStyle {
                                        size: Size::fixed(100.0, 50.0),
                                        ..Default::default()
                                    });
                                }
                            },
                        )
                        .id(),
                    );
                },
            )
            .id();
        store.layout(viewport, VIEW, &mut Vec::new());
        (store, viewport, column.unwrap())
    }

    fn frame(store: &mut NodeStore, viewport: NodeId) {
        store.tick_scroll_motion(Duration::from_millis(16));
        store.relayout_dirty(viewport, VIEW, &mut Vec::new(), &mut Vec::new());
    }

    /// A finger on `from` moving the offset by `dy` (down the screen is
    /// negative), as a pan does: a pull eases, the viewport scrolls, and
    /// the rest pulls.
    fn pan(store: &mut NodeStore, viewport: NodeId, from: NodeId, dy: f32) {
        let rest = store.ease_pulls(from, Vec2 { x: 0.0, y: dy });
        if rest.y != 0.0 {
            let before = store.scroll(viewport).y;
            store.scroll_by(viewport, rest);
            let left = rest.y - (store.scroll(viewport).y - before);
            if left != 0.0 {
                store.pull_on(from, Vec2 { x: 0.0, y: left });
            }
        }
        frame(store, viewport);
    }

    fn lift(store: &mut NodeStore, viewport: NodeId, from: NodeId) {
        store.release_pan(from, Vec2::ZERO);
        frame(store, viewport);
    }

    fn settle(store: &mut NodeStore, viewport: NodeId) {
        for _ in 0..90 {
            frame(store, viewport);
        }
    }

    fn asks(store: &mut NodeStore) -> Vec<NodeId> {
        let mut out = Vec::new();
        store.take_refresh_asks(&mut out);
        out
    }

    fn shown_y(store: &NodeStore, id: NodeId) -> f32 {
        store.world(id).y - store.world(store.parent(id).unwrap()).y
    }

    #[test]
    fn an_inset_pull_past_the_mark_refreshes_holding_a_gap_open_until_it_ends() {
        let (mut store, viewport, column) =
            list(40, RefreshStyle::apple(false), ScrollPhysics::APPLE);
        let (puck, spinner) = store.refresh_indicator(viewport).unwrap();
        for _ in 0..5 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert!(!store.is_refreshing(viewport));
        let partly = store.spinner(spinner).unwrap().progress;
        assert!(partly > 0.2 && partly < 1.0, "{partly}");
        let gap = -store.overscroll(viewport);
        let middle = store.translate(puck).y + 14.0;
        assert!((middle - gap / 2.0).abs() < 0.5, "halfway down the gap");

        for _ in 0..30 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert!(store.is_refreshing(viewport));
        assert!(store.spinner(spinner).unwrap().spinning);
        assert_eq!(asks(&mut store), [viewport]);
        assert!(store.scroll(viewport).y < 0.0, "the offset takes the gap");

        lift(&mut store, viewport, column);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).y, -60.0, "held open");
        assert_eq!(store.overscroll(viewport), 0.0);
        assert!(!store.is_scroll_moving());
        assert!((shown_y(&store, column) - 60.0).abs() < 0.5);
        // The gap scrolls away under the content as any of it does.
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 100.0 });
        assert_eq!(store.scroll(viewport).y, 40.0);
        store.scroll_by(viewport, Vec2 { x: 0.0, y: -200.0 });
        assert_eq!(store.scroll(viewport).y, -60.0);

        store.set_refreshing(viewport, false);
        assert!(!store.is_refreshing(viewport));
        assert_eq!(store.scroll(viewport).y, 0.0);
        assert!(store.overscroll(viewport) < -50.0, "the gap shows pulled");
        settle(&mut store, viewport);
        assert_eq!(store.overscroll(viewport), 0.0, "and springs shut");
        assert!(shown_y(&store, column).abs() < 0.5);
        assert_eq!(store.spinner(spinner).unwrap().progress, 0.0);
        assert!(!store.spinner(spinner).unwrap().spinning);
    }

    #[test]
    fn a_puck_slides_over_the_still_content_and_refreshes_let_go_past_the_mark() {
        let (mut store, viewport, column) =
            list(40, RefreshStyle::material(false), ScrollPhysics::ANDROID);
        let (puck, spinner) = store.refresh_indicator(viewport).unwrap();
        for _ in 0..4 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert_eq!(shown_y(&store, column), 0.0, "the content stays");
        assert!(
            store.pull_stretch(viewport).is_none(),
            "and does not stretch"
        );
        let travel = store.translate(puck).y + 40.0;
        assert!(travel > 10.0 && travel < 64.0, "{travel}");
        // Let go short of the mark: the puck slides back.
        lift(&mut store, viewport, column);
        assert!(!store.is_refreshing(viewport));
        assert_eq!(store.overscroll(viewport), 0.0, "its pull is gone");
        settle(&mut store, viewport);
        assert_eq!(store.translate(puck).y, -40.0);
        assert_eq!(store.opacity(puck), 0.0);

        for _ in 0..30 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert!(!store.is_refreshing(viewport), "a puck waits for the lift");
        lift(&mut store, viewport, column);
        assert!(store.is_refreshing(viewport));
        assert_eq!(asks(&mut store), [viewport]);
        settle(&mut store, viewport);
        assert_eq!(store.translate(puck).y, 24.0, "resting its hold down");
        assert!(store.spinner(spinner).unwrap().spinning);
        assert!(store.is_spinning());

        // Refreshing, a pull down past the start stretches as it would.
        for _ in 0..3 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert!(store.pull_stretch(viewport).is_some());
        lift(&mut store, viewport, column);
        settle(&mut store, viewport);

        store.set_refreshing(viewport, false);
        let mut timers = Vec::new();
        store.take_timer_requests(&mut timers);
        assert_eq!(timers.len(), 1, "the shrink's end");
        (timers.pop().unwrap().on_fire)(&mut store);
        assert_eq!(store.translate(puck).y, -40.0);
        assert!(!store.is_spinning());
    }

    #[test]
    fn a_controlled_refresh_lasts_only_once_its_owner_says_so() {
        let (mut store, viewport, column) =
            list(40, RefreshStyle::material(false), ScrollPhysics::ANDROID);
        store.set_refresh_controlled(viewport, true);
        let pull = |store: &mut NodeStore| {
            for _ in 0..30 {
                pan(store, viewport, column, -20.0);
            }
            lift(store, viewport, column);
        };
        pull(&mut store);
        assert_eq!(asks(&mut store), [viewport]);
        frame(&mut store, viewport);
        assert!(!store.is_refreshing(viewport), "told, and nobody confirmed");

        settle(&mut store, viewport);
        let mut timers = Vec::new();
        store.take_timer_requests(&mut timers);
        for t in timers {
            (t.on_fire)(&mut store);
        }
        pull(&mut store);
        assert_eq!(asks(&mut store), [viewport]);
        store.set_refreshing(viewport, true);
        frame(&mut store, viewport);
        assert!(store.is_refreshing(viewport), "confirmed");
    }

    #[test]
    fn a_short_list_still_refreshes_where_the_range_ends_hard() {
        let physics = ScrollPhysics {
            overscroll: OverscrollStyle::None,
            ..ScrollPhysics::APPLE
        };
        let (mut store, viewport, column) = list(3, RefreshStyle::apple(false), physics);
        assert_eq!(store.scroll_range(viewport, Axis::Column), 0.0);
        assert!(store.can_pull(column, Axis::Column, -1.0));
        assert!(
            !store.can_pull(column, Axis::Column, 1.0),
            "not past its end"
        );
        for _ in 0..40 {
            pan(&mut store, viewport, column, -20.0);
        }
        assert!(store.is_refreshing(viewport));
    }

    #[test]
    fn a_refresh_begun_without_a_pull_shows_and_asks_once() {
        let (mut store, viewport, _) = list(40, RefreshStyle::apple(false), ScrollPhysics::APPLE);
        assert!(store.begin_refresh(viewport));
        assert!(!store.begin_refresh(viewport), "refreshing already");
        assert!(store.refreshes_due());
        assert_eq!(asks(&mut store), [viewport]);
        settle(&mut store, viewport);
        assert_eq!(store.scroll(viewport).y, -60.0, "glided down to show it");
        store.set_refreshable(viewport, false);
        assert!(!store.is_refreshing(viewport));
        assert_eq!(store.scroll_span(viewport, Axis::Column).0, 0.0);
    }
}
