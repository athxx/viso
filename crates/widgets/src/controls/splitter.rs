//! The [`Splitter`] control — two panes and a divider between them that the
//! pointer or the keys move, the panes re-laid out live as it goes.
//!
//! A splitter lays its panes side by side ([`Axis::Row`]) or stacked
//! ([`Axis::Column`]) with a bar between them. Where the bar stands is a
//! [`SplitAt`]: a share of the room the panes divide ([`SplitAt::Fraction`],
//! both panes growing with the splitter), or the leading or trailing pane's
//! extent ([`SplitAt::Start`], [`SplitAt::End`], that pane keeping its size
//! as the splitter resizes, the other taking the rest). A move keeps the kind
//! it was given.
//!
//! Dragging the bar moves it under the pointer, the grab point held; the
//! arrow keys move it by a step (Shift a larger one), Home and End to the
//! ends, and Enter folds the leading pane down to its minimum and back. The
//! panes never shrink below their minimums ([`Splitter::min`]). Each move
//! rewrites the two panes' sizes in place — a re-layout of the splitter's
//! box, no rebuild — and reports the new place through `on_change`.
//!
//! The bar is the one focusable node, a [`Role::Separator`] named by the
//! splitter's label: a share reports its percentage (0–100), a pinned pane its
//! extent in dp; assistive technology increments and decrements it like the
//! arrows. The bar shows the resize cursor, and lights under the pointer and
//! while dragged.
//!
//! ```
//! use viso_widgets::{SplitAt, splitter};
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let split = splitter("Editor / Preview")
//!     .at(SplitAt::Start(240.0))
//!     .min(120.0, 160.0)
//!     .panes(
//!         |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); },
//!         |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); },
//!     )
//!     .on_change(|_ev, at| {
//!         // remember where the user left it
//!         let _ = at;
//!     });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! split.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, Component, CursorIcon, EventCx, FlexStyle, Handle,
    InteractionStyle, Justify, Key, LeafStyle, Length, NodeId, NodeStore, PointerButtons,
    PointerPhase, Rect, Rgba, Role, SemanticState, Semantics, Size, StateId, StateValue,
};

/// Where a splitter's bar stands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SplitAt {
    /// The leading pane's share, `0..=1`, of the room the panes divide (the
    /// splitter's extent less the bar): both panes grow with the splitter.
    Fraction(f32),
    /// The leading pane's extent (dp): it keeps it as the splitter resizes.
    Start(f32),
    /// The trailing pane's extent (dp): it keeps it as the splitter resizes.
    End(f32),
}

impl Default for SplitAt {
    fn default() -> Self {
        SplitAt::Fraction(0.5)
    }
}

/// Which kind of [`SplitAt`] a splitter keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Fraction,
    Start,
    End,
}

impl SplitAt {
    fn kind(self) -> Kind {
        match self {
            SplitAt::Fraction(_) => Kind::Fraction,
            SplitAt::Start(_) => Kind::Start,
            SplitAt::End(_) => Kind::End,
        }
    }

    fn value(self) -> f32 {
        match self {
            SplitAt::Fraction(v) | SplitAt::Start(v) | SplitAt::End(v) => v,
        }
    }

    /// The leading pane's extent, given the room `free` the panes divide.
    fn lead(self, free: f32) -> f32 {
        match self {
            SplitAt::Fraction(f) => f * free,
            SplitAt::Start(v) => v,
            SplitAt::End(v) => free - v,
        }
    }

    /// The main-axis lengths of the two panes.
    fn lengths(self) -> (Length, Length) {
        match self {
            SplitAt::Fraction(f) => (Length::Fill { weight: f }, Length::Fill { weight: 1.0 - f }),
            SplitAt::Start(v) => (Length::Fixed(v), Length::fill()),
            SplitAt::End(v) => (Length::fill(), Length::Fixed(v)),
        }
    }

    /// Clamped into what its kind can hold.
    fn sane(self) -> SplitAt {
        let v = self.value();
        match self {
            SplitAt::Fraction(_) => SplitAt::Fraction(if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.5
            }),
            SplitAt::Start(_) => SplitAt::Start(if v.is_finite() { v.max(0.0) } else { 0.0 }),
            SplitAt::End(_) => SplitAt::End(if v.is_finite() { v.max(0.0) } else { 0.0 }),
        }
    }
}

impl Kind {
    fn at(self, value: f32) -> SplitAt {
        match self {
            Kind::Fraction => SplitAt::Fraction(value),
            Kind::Start => SplitAt::Start(value),
            Kind::End => SplitAt::End(value),
        }
    }

    /// The place that puts the leading pane at `lead` of `free`.
    fn placing(self, lead: f32, free: f32) -> SplitAt {
        match self {
            Kind::Fraction => SplitAt::Fraction(if free > 0.0 { lead / free } else { 0.5 }),
            Kind::Start => SplitAt::Start(lead),
            Kind::End => SplitAt::End(free - lead),
        }
    }
}

/// How a [`Splitter`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitterStyle {
    /// [`Axis::Row`] sets the panes side by side, [`Axis::Column`] stacks
    /// them.
    pub axis: Axis,
    /// The splitter's own size within its parent; it fills it by default.
    pub size: Size,
    /// The bar's thickness on the main axis.
    pub bar_size: f32,
    /// The bar at rest, under the pointer, and dragged.
    pub bar: BoxStyle,
    pub hover: BoxStyle,
    pub drag: BoxStyle,
}

const fn rgb(r: f32, g: f32, b: f32) -> Rgba {
    Rgba { r, g, b, a: 1.0 }
}

impl Default for SplitterStyle {
    fn default() -> Self {
        SplitterStyle {
            axis: Axis::Row,
            size: Size::fill(),
            bar_size: 6.0,
            bar: BoxStyle::solid(rgb(0.3, 0.31, 0.34)),
            hover: BoxStyle::solid(rgb(0.4, 0.42, 0.47)),
            drag: BoxStyle::solid(rgb(0.22, 0.45, 0.85)),
        }
    }
}

/// How far an arrow moves the bar, and with Shift (dp).
const STEP: f32 = 10.0;
const BIG_STEP: f32 = 50.0;

type Change = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, SplitAt)>>>>;
type Pane = Box<dyn Fn(&mut BuildCx<'_>)>;

/// Two panes and a divider between them. Construct one with [`splitter`].
///
/// Invalidation: a move rewrites the two panes' sizes (`MEASURE | LAYOUT |
/// PAINT` on the panes) and the bar's semantic value; the bar's light swaps
/// its style (`PAINT`). Nothing is rebuilt.
pub struct Splitter {
    label: String,
    at: SplitAt,
    min: (f32, f32),
    style: SplitterStyle,
    panes: (Option<Pane>, Option<Pane>),
    on_change: Change,
}

/// A splitter named `label`, its panes sharing the room evenly, side by side.
pub fn splitter(label: impl Into<String>) -> Splitter {
    Splitter {
        label: label.into(),
        at: SplitAt::default(),
        min: (0.0, 0.0),
        style: SplitterStyle::default(),
        panes: (None, None),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Splitter {
    /// Build the leading and the trailing pane.
    pub fn panes(
        mut self,
        lead: impl Fn(&mut BuildCx<'_>) + 'static,
        trail: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.panes = (Some(Box::new(lead)), Some(Box::new(trail)));
        self
    }

    /// Stand the bar at `at` (default an even share).
    pub fn at(mut self, at: SplitAt) -> Self {
        self.at = at;
        self
    }

    /// Keep the leading pane at least `lead` and the trailing one at least
    /// `trail` across (dp) when the bar moves.
    pub fn min(mut self, lead: f32, trail: f32) -> Self {
        self.min = (lead.max(0.0), trail.max(0.0));
        self
    }

    /// Call `handler` with where the bar stands after each move.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, SplitAt) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the panes side by side ([`Axis::Row`]) or stacked
    /// ([`Axis::Column`]).
    pub fn axis(mut self, axis: Axis) -> Self {
        self.style.axis = axis;
        self
    }

    /// Replace the whole [`SplitterStyle`].
    pub fn style(mut self, style: SplitterStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the splitter's own size within its parent (it fills it by default).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

/// `main` along `axis`, `cross` across it.
fn size_on(axis: Axis, main: Length, cross: Length) -> Size {
    match axis {
        Axis::Row => Size {
            width: main,
            height: cross,
        },
        Axis::Column => Size {
            width: cross,
            height: main,
        },
    }
}

/// A rect's start and extent along `axis`.
fn span(axis: Axis, r: Rect) -> (f32, f32) {
    match axis {
        Axis::Row => (r.x, r.w),
        Axis::Column => (r.y, r.h),
    }
}

/// A built splitter's nodes and the cells its moves go through.
#[derive(Debug, Clone, Copy)]
struct Seam {
    axis: Axis,
    kind: Kind,
    bar_size: f32,
    min: (f32, f32),
    root: NodeId,
    lead: NodeId,
    trail: NodeId,
    bar: NodeId,
    /// The value of where the bar stands, in its kind's terms.
    at: StateId,
    /// Where on the bar the press took it, along the main axis.
    grab: StateId,
    /// The leading pane's extent before Enter folded it, or NaN.
    unfolded: StateId,
    dragging: StateId,
    hover: StateId,
}

/// The splitter's geometry as an event sees it.
struct Room {
    /// The splitter's main-axis start and the room the panes divide.
    start: f32,
    extent: f32,
    free: f32,
    /// Whether the leading pane lies after the bar (a mirrored row).
    mirrored: bool,
}

fn float(ev: &EventCx<'_>, cell: StateId) -> f32 {
    match ev.get(cell) {
        Some(StateValue::Float(v)) => v,
        _ => f32::NAN,
    }
}

impl Seam {
    fn room(&self, ev: &EventCx<'_>) -> Option<Room> {
        let root = ev.rect_of(self.root)?;
        let (start, extent) = span(self.axis, root);
        let mirrored = match (ev.rect_of(self.lead), ev.rect_of(self.bar)) {
            (Some(lead), Some(bar)) => span(self.axis, bar).0 < span(self.axis, lead).0,
            _ => false,
        };
        Some(Room {
            start,
            extent,
            free: (extent - self.bar_size).max(0.0),
            mirrored,
        })
    }

    /// The leading pane's extent as the bar stands now.
    fn lead_now(&self, ev: &EventCx<'_>, room: &Room) -> f32 {
        self.kind.at(float(ev, self.at)).lead(room.free)
    }

    /// The leading pane's extent clamped to both panes' minimums.
    fn clamp(&self, lead: f32, free: f32) -> f32 {
        let (lo, hi) = (self.min.0, free - self.min.1);
        if lo > hi {
            // No room for both: share the shortfall by the minimums.
            let total = self.min.0 + self.min.1;
            return if total > 0.0 {
                free * self.min.0 / total
            } else {
                free * 0.5
            };
        }
        lead.clamp(lo, hi)
    }

    /// Stand the leading pane at `lead`, reporting the move.
    fn place(&self, ev: &mut EventCx<'_>, room: &Room, lead: f32, change: &Change) {
        let lead = self.clamp(lead, room.free);
        let at = self.kind.placing(lead, room.free);
        if float(ev, self.at) == at.value() {
            return;
        }
        ev.set(self.at, StateValue::Float(at.value()));
        if let Some(f) = change.borrow_mut().as_mut() {
            f(ev, at);
        }
    }

    fn pointer(&self, ev: &mut EventCx<'_>, change: &Change) {
        let Some(p) = ev.pointer() else { return };
        let main = match self.axis {
            Axis::Row => p.x,
            Axis::Column => p.y,
        };
        match p.phase {
            PointerPhase::Enter => {
                ev.set(self.hover, StateValue::Bool(true));
            }
            PointerPhase::Leave => {
                ev.set(self.hover, StateValue::Bool(false));
            }
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                let Some(bar) = ev.rect_of(self.bar) else {
                    return;
                };
                ev.set(self.grab, StateValue::Float(main - span(self.axis, bar).0));
                ev.set(self.dragging, StateValue::Bool(true));
                ev.capture_pointer(self.bar);
                ev.request_focus(self.bar);
                ev.stop_propagation();
            }
            PointerPhase::Move if p.buttons.contains(PointerButtons::PRIMARY) => {
                if ev.get(self.dragging) != Some(StateValue::Bool(true)) {
                    return;
                }
                let Some(room) = self.room(ev) else { return };
                // Where the bar's leading edge goes, from the splitter's start.
                let edge = main - float(ev, self.grab) - room.start;
                let lead = if room.mirrored {
                    room.extent - edge - self.bar_size
                } else {
                    edge
                };
                self.place(ev, &room, lead, change);
            }
            PointerPhase::Up if ev.get(self.dragging) == Some(StateValue::Bool(true)) => {
                ev.set(self.dragging, StateValue::Bool(false));
                ev.release_pointer();
            }
            _ => {}
        }
    }

    fn key(&self, ev: &mut EventCx<'_>, change: &Change) {
        let Some(&k) = ev.key() else { return };
        if !k.pressed {
            return;
        }
        let Some(room) = self.room(ev) else { return };
        let now = self.lead_now(ev, &room);
        let step = if k.modifiers.shift { BIG_STEP } else { STEP };
        // The arrows move the bar on screen: in a mirrored row the leading
        // pane is on the right.
        let toward_end = |key: Key| match key {
            Key::Right => Some(!room.mirrored),
            Key::Left => Some(room.mirrored),
            Key::Down => Some(true),
            Key::Up => Some(false),
            _ => None,
        };
        let lead = match k.key {
            Key::Home => 0.0,
            Key::End => room.free,
            Key::Enter => {
                let unfolded = float(ev, self.unfolded);
                if unfolded.is_nan() || now > self.min.0 + 0.5 {
                    ev.set(self.unfolded, StateValue::Float(now));
                    0.0
                } else {
                    ev.set(self.unfolded, StateValue::Float(f32::NAN));
                    unfolded
                }
            }
            key => match toward_end(key) {
                Some(true) => now + step,
                Some(false) => now - step,
                None => return,
            },
        };
        ev.stop_propagation();
        if k.key != Key::Enter {
            ev.set(self.unfolded, StateValue::Float(f32::NAN));
        }
        self.place(ev, &room, lead, change);
    }

    /// Size the panes, and report the bar's value, for where it stands.
    fn show(&self, store: &mut NodeStore, value: f32) {
        let at = self.kind.at(value);
        let (lead, trail) = at.lengths();
        let fill = Length::fill();
        store.set_fixed_size(self.lead, size_on(self.axis, lead, fill));
        store.set_fixed_size(self.trail, size_on(self.axis, trail, fill));
        let state = match at {
            SplitAt::Fraction(f) => SemanticState::slider(f * 100.0, 0.0, 100.0),
            SplitAt::Start(v) | SplitAt::End(v) => SemanticState {
                value: Some(v),
                ..SemanticState::default()
            },
        };
        store.set_semantic_state(self.bar, state);
    }
}

impl Component for Splitter {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let axis = style.axis;
        let at0 = self.at.sane();
        let at = cx.state(StateValue::Float(at0.value()));
        let grab = cx.state(StateValue::Float(0.0));
        let unfolded = cx.state(StateValue::Float(f32::NAN));
        let dragging = cx.state(StateValue::Bool(false));
        let hover = cx.state(StateValue::Bool(false));
        let (lead_len, trail_len) = at0.lengths();
        let pane = |cx: &mut BuildCx<'_>, main: Length, build: &Option<Pane>| -> Handle {
            cx.flex(
                FlexStyle {
                    axis,
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: size_on(axis, main, Length::fill()),
                    ..FlexStyle::default()
                },
                |cx| {
                    if let Some(build) = build {
                        build(cx);
                    }
                },
            )
        };
        let mut parts = None;
        let root = cx.flex(
            FlexStyle {
                axis,
                align: Align::Stretch,
                justify: Justify::Start,
                size: style.size,
                ..FlexStyle::default()
            },
            |cx| {
                let lead = pane(cx, lead_len, &self.panes.0);
                let bar = cx.leaf(LeafStyle {
                    size: size_on(axis, Length::Fixed(style.bar_size), Length::fill()),
                    style: style.bar,
                });
                let trail = pane(cx, trail_len, &self.panes.1);
                parts = Some((lead, bar, trail));
            },
        );
        let (lead, bar, trail) = parts.expect("the panes and the bar are built");
        cx.cursor(
            bar,
            match axis {
                Axis::Row => CursorIcon::ResizeCol,
                Axis::Column => CursorIcon::ResizeRow,
            },
        );
        cx.interaction_style(
            bar,
            InteractionStyle {
                resting: style.bar,
                hover: style.hover,
                pressed: style.drag,
                pressed_cell: Some(dragging),
                hover_cell: Some(hover),
            },
        );
        cx.focusable(bar, true);
        let mut semantics = Semantics::role(Role::Separator);
        semantics.label = Some(self.label.clone());
        cx.semantics(bar, semantics);

        let seam = Seam {
            axis,
            kind: at0.kind(),
            bar_size: style.bar_size,
            min: self.min,
            root: root.id(),
            lead: lead.id(),
            trail: trail.id(),
            bar: bar.id(),
            at,
            grab,
            unfolded,
            dragging,
            hover,
        };
        let change = self.on_change.clone();
        cx.on_pointer(bar, move |ev| seam.pointer(ev, &change));
        let change = self.on_change.clone();
        cx.on_key(bar, move |ev| seam.key(ev, &change));
        cx.structure(|s| {
            seam.show(s.store, at0.value());
            s.store.add_structure_hook([at], move |s, _| {
                if let Some(StateValue::Float(v)) = s.states.get(seam.at)
                    && s.store.arena().is_live(seam.bar)
                {
                    seam.show(s.store, v);
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_lays_out_and_reads_back_the_leading_pane() {
        let free = 400.0;
        for at in [
            SplitAt::Fraction(0.25),
            SplitAt::Start(100.0),
            SplitAt::End(300.0),
        ] {
            assert_eq!(at.lead(free), 100.0, "{at:?}");
            assert_eq!(at.kind().placing(100.0, free), at);
        }
        assert_eq!(
            SplitAt::Fraction(0.25).lengths(),
            (Length::Fill { weight: 0.25 }, Length::Fill { weight: 0.75 })
        );
        assert_eq!(
            SplitAt::End(80.0).lengths(),
            (Length::fill(), Length::Fixed(80.0))
        );
        assert_eq!(SplitAt::Fraction(2.0).sane(), SplitAt::Fraction(1.0));
        assert_eq!(SplitAt::Start(f32::NAN).sane(), SplitAt::Start(0.0));
    }
}
