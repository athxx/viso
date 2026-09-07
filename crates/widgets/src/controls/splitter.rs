//! The [`Splitter`] control — a draggable divider between two resizable panes.
//!
//! A `Splitter` lays two panes side by side (a horizontal split, `Axis::Row`) or
//! stacked (a vertical split, `Axis::Column`) with a thin draggable bar between
//! them. Dragging the bar with the primary pointer or nudging it with the arrow
//! keys moves the split — the two are the *same* action semantically, so an
//! interactive control must have a keyboard equivalent (AGENTS section 15) — and
//! both drive one shared `on_change`, reporting the new split *fraction* in
//! `0.0..=1.0` (pane A's share of the main-axis extent).
//!
//! The internal state is that fraction, kept as a small `Copy` `Float` cell like
//! the reference [`Slider`](crate::Slider). Dragging is *delta*-based: a press
//! records the down position on the main axis and the fraction at that instant,
//! and each move adds `(pos - down_pos) / extent` to the recorded fraction — so
//! the bar tracks the finger without the handler needing the container's world
//! geometry (an `EventCx` handler has no node bounds). The press captures the
//! pointer (`capture_pointer`) so a drag keeps receiving samples even when the
//! cursor leaves the bar box; the release frees it.
//!
//! It maps to one interactive `viso-ui` node: a focusable flex container (a row
//! for a horizontal split, a column for a vertical one) carrying a reactive
//! `fraction` cell bound to `PAINT`, composed of pane A (sized to its build-time
//! share of the main axis), the divider bar (a fixed leaf), and pane B (filling
//! the rest). Dragging writes the `fraction` cell, repainting the container alone
//! (a targeted invalidation — no rebuild, architecture section 47), and reports
//! the new fraction through `on_change` so the app owns the live pane sizing. Its
//! accessible role is [`Role::Group`] with an authored label. A divider that
//! *re-lays* the panes in place as the cell moves (rather than the app driving the
//! panes from the reported fraction) is a later slice: a bound layout cell does
//! not re-derive a leaf's build-time size today, matching the [`Slider`] thumb
//! precedent (the thumb sits at its build-time position; sliding it in paint is a
//! later slice).
//!
//! ```
//! use viso_widgets::splitter;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let split = splitter("Editor / Preview")
//!     .fraction(0.6)
//!     .panes(
//!         |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); },
//!         |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); },
//!     )
//!     .on_change(|_ev, f| {
//!         // handle the new split fraction — e.g. resize the app's panes
//!         let _ = f;
//!     });
//!
//! // A splitter authors reactive state, so it builds through a reactive cx.
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
    Align, Axis, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset, Key,
    LeafStyle, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateId,
    StateValue,
};

/// A shared, mutable change callback carrying the new split fraction. It is cloned
/// into both the pointer handler and the key handler at build time so a drag and
/// an arrow-key step drive the same `on_change`. Pointer and keyboard input are
/// never concurrent, so the runtime never re-enters the `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, f32)>>>>;

/// A pane's build-time content builder. It authors the pane's subtree into the
/// container's flex; boxed so a `Splitter` can hold two heterogeneous closures.
type PaneBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The divider bar's fill — a neutral seam that reads against either pane.
const BAR: Rgba = Rgba {
    r: 0.3,
    g: 0.31,
    b: 0.34,
    a: 1.0,
};

/// The default divider thickness on the main axis.
const BAR_SIZE: f32 = 6.0;

/// The main-axis extent a full `0.0..=1.0` drag is assumed to cover when the
/// container's own size is `Fit`/unknown at build time. The delta model divides
/// the pixel drag by this to a fraction; a `Fixed` container overrides it with its
/// own main-axis length. Chosen as a common pane-region width.
const DEFAULT_EXTENT: f32 = 400.0;

/// The fraction of the extent one arrow-key press moves the split.
const KEY_STEP_FRACTION: f32 = 0.02;

/// The visual and layout parameters of a [`Splitter`]: the split axis, the divider
/// thickness and fill, and the splitter's own size request within its parent.
///
/// `size` defaults to [`Length::Fill`] on both axes so the split fills its parent
/// region; override it with a [`Length::Fixed`] axis for a fixed box. All fields
/// are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SplitterStyle {
    /// The split axis: [`Axis::Row`] places the panes side by side (a horizontal
    /// split), [`Axis::Column`] stacks them (a vertical split).
    pub axis: Axis,
    /// The splitter's own size request within its parent. Defaults to `Fill` on
    /// both axes (fill the parent region).
    pub size: Size,
    /// The divider's thickness on the main axis.
    pub bar_size: f32,
    /// The divider's fill.
    pub bar: BoxStyle,
}

impl Default for SplitterStyle {
    fn default() -> Self {
        SplitterStyle {
            axis: Axis::Row,
            size: Size {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fill { weight: 1.0 },
            },
            bar_size: BAR_SIZE,
            bar: BoxStyle::solid(BAR),
        }
    }
}

/// A draggable divider between two resizable panes.
///
/// Construct one with [`splitter`], give it two pane content builders with
/// [`Splitter::panes`], and attach behavior with the chainable setters. Dragging
/// the divider with the primary pointer or nudging it with the arrow keys moves
/// the split and fires `on_change` with the new split *fraction* (pane A's share
/// of the main-axis extent, in `0.0..=1.0`).
///
/// See the [module docs](self) for a build example. Invalidation: moving the
/// split writes a reactive `fraction` cell bound to `PAINT`, repainting the
/// splitter's own container without a rebuild; the panes are static (the app
/// re-sizes them from the reported fraction).
pub struct Splitter {
    /// The accessible name.
    label: String,
    /// The initial split fraction in `0.0..=1.0` (pane A's share).
    fraction: f32,
    /// The main-axis extent a full drag covers, overriding [`DEFAULT_EXTENT`] when
    /// the style requests a `Fixed` main-axis size.
    extent: f32,
    style: SplitterStyle,
    /// Pane A's content builder (the leading pane). `None` builds an empty pane.
    pane_a: Option<PaneBuilder>,
    /// Pane B's content builder (the trailing pane). `None` builds an empty pane.
    pane_b: Option<PaneBuilder>,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`Splitter::on_change`] is called; a splitter with no handler still moves
    /// its fraction cell, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct a [`Splitter`] with the given accessible label, a default `0.5`
/// centered split, a horizontal (`Axis::Row`) layout, and no panes or handler
/// yet. Chain [`Splitter::panes`] to give it content, [`Splitter::fraction`] and
/// [`Splitter::axis`] to shape the split, and [`Splitter::on_change`] to give it
/// behavior.
pub fn splitter(label: impl Into<String>) -> Splitter {
    Splitter {
        label: label.into(),
        fraction: 0.5,
        extent: DEFAULT_EXTENT,
        style: SplitterStyle::default(),
        pane_a: None,
        pane_b: None,
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Splitter {
    /// Set the two pane content builders — the leading pane (A) and the trailing
    /// pane (B). Each authors its subtree into the container at build time.
    pub fn panes(
        mut self,
        pane_a: impl Fn(&mut BuildCx<'_>) + 'static,
        pane_b: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.pane_a = Some(Box::new(pane_a));
        self.pane_b = Some(Box::new(pane_b));
        self
    }

    /// Set the change callback, fired with the new split fraction on a drag or an
    /// arrow-key step. Replaces any previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, f32) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initial split fraction in `0.0..=1.0` (pane A's share of the
    /// main-axis extent). Clamped into range at build time.
    pub fn fraction(mut self, fraction: f32) -> Self {
        self.fraction = fraction;
        self
    }

    /// Set the split axis: [`Axis::Row`] (a horizontal split, the default) or
    /// [`Axis::Column`] (a vertical split).
    pub fn axis(mut self, axis: Axis) -> Self {
        self.style.axis = axis;
        self
    }

    /// Set the main-axis extent a full `0.0..=1.0` drag covers — the pixel span
    /// the delta model divides by. Defaults to [`DEFAULT_EXTENT`]; set it to the
    /// container's known main-axis size for a `Fixed`-sized splitter.
    pub fn extent(mut self, extent: f32) -> Self {
        self.extent = extent.max(0.0);
        self
    }

    /// Replace the whole [`SplitterStyle`].
    pub fn style(mut self, style: SplitterStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the splitter's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// The initial split fraction clamped into `0.0..=1.0`.
    fn initial_fraction(&self) -> f32 {
        self.fraction.clamp(0.0, 1.0)
    }
}

/// Drive the shared callback with the new split fraction if one is set; a
/// handler-less splitter is a no-op. Pointer and keyboard activation never
/// overlap, so the borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, fraction: f32) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, fraction);
    }
}

/// Read a float state cell, defaulting to `0.0` for a stale handle or a non-float
/// value (neither happens in normal use — the cells are authored as `Float` and
/// live as long as the node).
fn read_f32(ev: &EventCx<'_>, cell: StateId) -> f32 {
    match ev.get(cell) {
        Some(StateValue::Float(v)) => v,
        _ => 0.0,
    }
}

/// The pointer sample's coordinate on the split's main axis.
fn main_axis_pos(axis: Axis, x: f32, y: f32) -> f32 {
    match axis {
        Axis::Row => x,
        Axis::Column => y,
    }
}

/// A [`Size`] whose main-axis length is `main` and cross-axis length is `cross`
/// for the given split `axis` — the pane/bar sizing helper (`Size` stores width
/// and height, not main/cross, so this maps the split axis onto them).
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

impl Component for Splitter {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // Three reactive `Float` cells keep the drag self-contained: `fraction` is
        // pane A's share (bound to PAINT so a move repaints the container alone, a
        // targeted invalidation, not a rebuild); `down_pos` and `start_frac`
        // record the press anchor so a move is a pure delta from it — the handler
        // never needs the container's world geometry (an `EventCx` has no node
        // bounds).
        let frac0 = self.initial_fraction();
        let fraction = cx.state(StateValue::Float(frac0));
        let down_pos = cx.state(StateValue::Float(0.0));
        let start_frac = cx.state(StateValue::Float(frac0));

        // Read the (Copy) style and extent by value; `build` takes `&self`, so the
        // label is cloned (cold, build-time — not a per-frame path).
        let axis = self.style.axis;
        let bar_size = self.style.bar_size;
        let bar_box = self.style.bar;
        let extent = self.extent;

        // Pane A takes its build-time share of the free main-axis room (the extent
        // minus the divider), and pane B fills the rest. Live re-sizing from the
        // cell is a later slice (a bound layout cell does not re-derive a leaf's
        // build-time size); the app re-sizes the panes from the reported fraction.
        let free = (extent - bar_size).max(0.0);
        let pane_a_main = frac0 * free;
        let pane_a_size = size_on(
            axis,
            Length::Fixed(pane_a_main),
            Length::Fill { weight: 1.0 },
        );
        let bar_leaf_size = size_on(axis, Length::Fixed(bar_size), Length::Fill { weight: 1.0 });
        let pane_b_size = size_on(
            axis,
            Length::Fill { weight: 1.0 },
            Length::Fill { weight: 1.0 },
        );

        let pane_a = self.pane_a.as_ref();
        let pane_b = self.pane_b.as_ref();

        let root = cx.flex(
            FlexStyle {
                axis,
                gap: 0.0,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                // Pane A: its build-time share of the main axis, filling the cross
                // axis, holding the author's leading content.
                cx.flex(
                    FlexStyle {
                        axis,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        size: pane_a_size,
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        if let Some(build) = pane_a {
                            build(cx);
                        }
                    },
                );
                // The divider bar: a fixed-thickness leaf across the cross axis.
                cx.leaf(LeafStyle {
                    size: bar_leaf_size,
                    style: bar_box,
                });
                // Pane B: fills the remaining main axis, holding the trailing
                // content.
                cx.flex(
                    FlexStyle {
                        axis,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        size: pane_b_size,
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        if let Some(build) = pane_b {
                            build(cx);
                        }
                    },
                );
            },
        );

        // Bind the fraction cell to the container's PAINT so a move repaints just
        // this subtree.
        cx.bind(fraction, root, DirtyClass::PAINT);
        cx.focusable(root, true);

        // Pointer drag: a primary press records the anchor (down-pos on the main
        // axis + the fraction at that instant) and captures the pointer, so
        // subsequent samples route here even outside the bar box. Each move adds
        // the pixel delta over the extent to the recorded fraction and fires
        // `on_change` with the new fraction. The release frees the capture. The
        // delta model needs no node geometry — only the build-time `extent`.
        let pointer_cb = self.on_change.clone();
        let root_id = root.id();
        cx.on_pointer(root, move |ev| {
            let Some(p) = ev.pointer() else { return };
            if !p.buttons.contains(PointerButtons::PRIMARY) && p.phase != PointerPhase::Up {
                return;
            }
            match p.phase {
                PointerPhase::Down => {
                    ev.set(down_pos, StateValue::Float(main_axis_pos(axis, p.x, p.y)));
                    ev.set(start_frac, StateValue::Float(read_f32(ev, fraction)));
                    ev.capture_pointer(root_id);
                }
                PointerPhase::Move => {
                    let dpos = main_axis_pos(axis, p.x, p.y) - read_f32(ev, down_pos);
                    let delta = if extent > 0.0 { dpos / extent } else { 0.0 };
                    let next = (read_f32(ev, start_frac) + delta).clamp(0.0, 1.0);
                    if ev.set(fraction, StateValue::Float(next)) {
                        fire(&pointer_cb, ev, next);
                    }
                }
                PointerPhase::Up => {
                    ev.release_pointer();
                }
                PointerPhase::Leave => {}
            }
        });

        // Keyboard step: Left/Up decrement and Right/Down increment the fraction by
        // a small step, firing the same `on_change` — the accessibility equivalent
        // of the drag (AGENTS section 15). For a row split Left/Right are the
        // natural axis; for a column split Up/Down are — both pairs are accepted so
        // either orientation is keyboard-drivable. Auto-repeat is honored so a held
        // arrow ramps the split.
        let key_cb = self.on_change.clone();
        cx.on_key(root, move |ev| {
            let Some(k) = ev.key() else { return };
            if !k.pressed {
                return;
            }
            let dir = match k.key {
                Key::Right | Key::Down => 1.0,
                Key::Left | Key::Up => -1.0,
                _ => return,
            };
            let next = (read_f32(ev, fraction) + dir * KEY_STEP_FRACTION).clamp(0.0, 1.0);
            if ev.set(fraction, StateValue::Float(next)) {
                fire(&key_cb, ev, next);
            }
        });

        // A splitter is a resizable-pane group; its accessible name is the authored
        // label (AGENTS section 15). The live fraction is proven by the reactive
        // cell and input tapes; wiring it into the derived tree is a later slice
        // (the derive pass has no state store).
        cx.semantics(
            root,
            Semantics::role(Role::Group).with_label(self.label.clone()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeId, NodeStore, PointerEvent, SemanticProjector,
        StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a splitter build writes into, kept together so a test
    /// can build a splitter and then drive its handlers against the same state.
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        /// Build a splitter through a reactive cx (a splitter authors state, so a
        /// plain `BuildCx::new` would panic) and return its root node.
        fn build(&mut self, s: Splitter) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            s.build(&mut cx);
            cx.root().expect("splitter declares a root node")
        }

        /// Feed a pointer sample to the root's pointer handler, restoring it after,
        /// and return any pending capture request the handler made.
        fn pointer(&mut self, root: NodeId, ev: PointerEvent) -> Option<Option<NodeId>> {
            let mut handler = self.store.take_handler(root).expect("pointer handler");
            let capture = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_capture_request()
            };
            self.store.restore_handler(root, handler);
            capture
        }

        /// Feed a key sample to the root's key handler, restoring it after.
        fn key(&mut self, root: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(root).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(root, handler);
        }

        /// The current value of the splitter's `fraction` cell — the first cell the
        /// build authors (a fresh store mints index 0, generation 0).
        fn fraction(&mut self) -> f32 {
            let ev = still();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(fraction_cell()) {
                Some(StateValue::Float(v)) => v,
                _ => f32::NAN,
            }
        }
    }

    /// The splitter's `fraction` cell handle. The build authors `fraction` first,
    /// so in a fresh store it is index 0, generation 0 — the handle a throwaway
    /// store mints for its first `Float` cell (there is no public bare-`StateId`
    /// constructor).
    fn fraction_cell() -> StateId {
        StateStore::new().alloc(StateValue::Float(0.0))
    }

    /// A still (no-button) pointer sample, for read-only state peeks.
    fn still() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A primary-button pointer sample at `(x, y)` in the given phase.
    fn primary_at(x: f32, y: f32, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample.
    fn key_ev(key: Key, pressed: bool, repeat: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat,
            modifiers: Modifiers::default(),
        }
    }

    /// Count a node's direct children by walking the arena sibling chain.
    fn child_count(store: &NodeStore, parent: NodeId) -> usize {
        let arena = store.arena();
        let mut n = 0;
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            n += 1;
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        n
    }

    /// A splitter builds an interactive node: a pointer handler, a key handler, a
    /// focusable flag, a `Group` semantics node named by its caption, and three
    /// composed children (pane A, the divider bar, pane B).
    #[test]
    fn splitter_builds_a_focusable_group_with_two_panes_and_a_bar() {
        let mut rx = Reactive::new();
        let root = rx.build(splitter("Editor / Preview"));

        assert!(
            rx.store.has_handler(root),
            "splitter attaches a pointer handler"
        );
        assert!(
            rx.store.has_key_handler(root),
            "splitter attaches a key handler"
        );
        assert!(rx.store.focusable(root), "splitter is focusable");
        assert_eq!(
            child_count(&rx.store, root),
            3,
            "splitter composes pane A, the divider bar, and pane B"
        );

        let sem = rx
            .store
            .semantics(root)
            .expect("splitter authors semantics");
        assert_eq!(sem.role, Role::Group);
        assert_eq!(sem.label.as_deref(), Some("Editor / Preview"));
    }

    /// The chainable setters override the fraction, axis, extent, and size, and the
    /// default is a centered `0.5` horizontal (`Axis::Row`) split at the default
    /// extent, `Fill` on both axes.
    #[test]
    fn setters_override_defaults() {
        let default = splitter("x");
        assert_eq!(default.fraction, 0.5);
        assert_eq!(default.extent, DEFAULT_EXTENT);
        assert_eq!(default.style.axis, Axis::Row);
        assert_eq!(default.style.size, Size::fill());

        let widget = splitter("x")
            .fraction(0.25)
            .axis(Axis::Column)
            .extent(800.0)
            .size(Size::fixed(300.0, 300.0));
        assert_eq!(widget.fraction, 0.25);
        assert_eq!(widget.extent, 800.0);
        assert_eq!(widget.style.axis, Axis::Column);
        assert_eq!(widget.style.size, Size::fixed(300.0, 300.0));
        // The build clamps the fraction into range.
        assert!((widget.initial_fraction() - 0.25).abs() < 1e-6);
        assert!((splitter("x").fraction(1.5).initial_fraction() - 1.0).abs() < 1e-6);
        assert!((splitter("x").fraction(-0.5).initial_fraction() - 0.0).abs() < 1e-6);
    }

    /// A primary press records the anchor and captures the pointer to the root; a
    /// move drags the fraction by the pixel delta over the extent and fires
    /// `on_change` with the new fraction; the release frees capture.
    #[test]
    fn pointer_drag_moves_fraction_captures_and_fires_change() {
        let last = Rc::new(Cell::new(None::<f32>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        // A 200px-extent row split starting centered at 0.5.
        let root = rx.build(splitter("split").extent(200.0).on_change(move |_, f| {
            l.set(Some(f));
            c.set(c.get() + 1);
        }));

        assert!((rx.fraction() - 0.5).abs() < 1e-6, "starts centered");

        // Press at x=100: records the anchor and requests capture to the root.
        let cap = rx.pointer(root, primary_at(100.0, 0.0, PointerPhase::Down));
        assert_eq!(cap, Some(Some(root)), "the press captures the pointer");
        assert_eq!(count.get(), 0, "the press alone does not fire a change");

        // Move right by 40px over a 200px extent: +0.2 -> 0.7.
        rx.pointer(root, primary_at(140.0, 0.0, PointerPhase::Move));
        assert!((rx.fraction() - 0.7).abs() < 1e-6, "dragged right by 0.2");
        assert_eq!(count.get(), 1, "the move fires one change");
        assert_eq!(last.get(), Some(0.7), "on_change carries the new fraction");

        // Move past the far end: clamps to 1.0.
        rx.pointer(root, primary_at(400.0, 0.0, PointerPhase::Move));
        assert!((rx.fraction() - 1.0).abs() < 1e-6, "clamps at the far end");
        assert_eq!(last.get(), Some(1.0));

        // Release frees capture.
        let cap = rx.pointer(root, primary_at(400.0, 0.0, PointerPhase::Up));
        assert_eq!(cap, Some(None), "the release frees the capture");
    }

    /// A column split drags on the y axis: the same pixel delta on y moves the
    /// fraction, and x is ignored.
    #[test]
    fn column_split_drags_on_the_y_axis() {
        let mut rx = Reactive::new();
        let root = rx.build(splitter("v").axis(Axis::Column).extent(200.0));

        rx.pointer(root, primary_at(999.0, 50.0, PointerPhase::Down));
        // A y move of +40 over 200 is +0.2 -> 0.7; the x jump is ignored.
        rx.pointer(root, primary_at(0.0, 90.0, PointerPhase::Move));
        assert!(
            (rx.fraction() - 0.7).abs() < 1e-6,
            "a column split reads the y delta"
        );
    }

    /// The drag is a pure delta from the press anchor: a second press re-anchors,
    /// so a move after it is measured from the new down position.
    #[test]
    fn drag_reanchors_on_each_press() {
        let mut rx = Reactive::new();
        let root = rx.build(splitter("s").extent(200.0));

        // First drag to 0.7.
        rx.pointer(root, primary_at(100.0, 0.0, PointerPhase::Down));
        rx.pointer(root, primary_at(140.0, 0.0, PointerPhase::Move));
        assert!((rx.fraction() - 0.7).abs() < 1e-6);
        rx.pointer(root, primary_at(140.0, 0.0, PointerPhase::Up));

        // Second press at x=10 re-anchors at 0.7; a move to x=30 is +20px = +0.1,
        // landing at 0.8 — not measured from the original down position.
        rx.pointer(root, primary_at(10.0, 0.0, PointerPhase::Down));
        rx.pointer(root, primary_at(30.0, 0.0, PointerPhase::Move));
        assert!(
            (rx.fraction() - 0.8).abs() < 1e-6,
            "the second drag deltas from the new anchor"
        );
    }

    /// A non-primary button neither anchors nor drags.
    #[test]
    fn non_primary_pointer_does_not_drag() {
        let count = Rc::new(Cell::new(0u32));
        let c = count.clone();
        let mut rx = Reactive::new();
        let root = rx.build(splitter("s").on_change(move |_, _| c.set(c.get() + 1)));

        let non_primary_down = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(0.0, 0.0, PointerPhase::Down)
        };
        let cap = rx.pointer(root, non_primary_down);
        assert_eq!(cap, None, "a non-primary press does not capture");
        let non_primary_move = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(60.0, 0.0, PointerPhase::Move)
        };
        rx.pointer(root, non_primary_move);
        assert_eq!(count.get(), 0, "a non-primary drag fires nothing");
    }

    /// Arrow keys step the fraction and fire `on_change`; Right/Down increment,
    /// Left/Up decrement, each by the key step.
    #[test]
    fn arrow_keys_step_the_fraction() {
        let last = Rc::new(Cell::new(None::<f32>));
        let count = Rc::new(Cell::new(0u32));
        let (l, c) = (last.clone(), count.clone());
        let mut rx = Reactive::new();
        let root = rx.build(splitter("s").fraction(0.5).on_change(move |_, f| {
            l.set(Some(f));
            c.set(c.get() + 1);
        }));

        assert!((rx.fraction() - 0.5).abs() < 1e-6, "starts centered");

        rx.key(root, key_ev(Key::Right, true, false));
        assert!(
            (rx.fraction() - (0.5 + KEY_STEP_FRACTION)).abs() < 1e-6,
            "Right steps up one step"
        );
        rx.key(root, key_ev(Key::Down, true, false));
        assert!(
            (rx.fraction() - (0.5 + 2.0 * KEY_STEP_FRACTION)).abs() < 1e-6,
            "Down steps up too"
        );

        rx.key(root, key_ev(Key::Left, true, false));
        assert!(
            (rx.fraction() - (0.5 + KEY_STEP_FRACTION)).abs() < 1e-6,
            "Left steps down"
        );
        rx.key(root, key_ev(Key::Up, true, false));
        assert!((rx.fraction() - 0.5).abs() < 1e-6, "Up steps down too");

        assert_eq!(count.get(), 4, "each arrow press fired one change");

        // A key-up and an unrelated key do nothing.
        rx.key(root, key_ev(Key::Right, false, false));
        rx.key(root, key_ev(Key::Enter, true, false));
        assert_eq!(count.get(), 4, "key-up and Enter do not step");
    }

    /// A handler-less splitter still moves its fraction without panicking.
    #[test]
    fn splitter_without_handler_still_moves() {
        let mut rx = Reactive::new();
        let root = rx.build(splitter("s").extent(200.0));

        rx.pointer(root, primary_at(100.0, 0.0, PointerPhase::Down));
        rx.pointer(root, primary_at(140.0, 0.0, PointerPhase::Move)); // no callback
        assert!(
            (rx.fraction() - 0.7).abs() < 1e-6,
            "still moves the fraction"
        );
        assert!(rx.store.focusable(root), "still focusable");
    }
}
