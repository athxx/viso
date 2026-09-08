//! The [`Tabs`] control — a strip of selectable tabs, each revealing one panel.
//!
//! A `Tabs` lays a horizontal [`TabList`](viso_ui::Role::TabList) of clickable
//! tab buttons above a panel area holding one panel per tab. Exactly one tab is
//! selected at a time; selecting a tab shows its panel and hides the others.
//! Selecting a tab — a primary-pointer press then release on it, Enter/Space
//! while it is focused, or a Left/Right arrow step across the strip — writes the
//! shared `selected` cell to that tab's index and fires `on_change` once with the
//! new index. Pointer and keyboard activation are the *same* action
//! semantically, so an interactive control must have a keyboard equivalent
//! (AGENTS section 15).
//!
//! All panels build once, at build time. Switching tabs does not rebuild them —
//! `DirtyClass::STRUCTURE` bubbles but no pass rebuilds a subtree, so panels are
//! shown and hidden through the retained [`hidden`](viso_ui::NodeStore::hidden)
//! flag: a hidden panel folds out of layout and paint without a structural
//! change (architecture section 8.4 / section 11). A handler cannot flip the
//! flag directly (an [`EventCx`] holds no node store), so the flip is a deferred
//! request the router applies after the handler returns — the same discipline as
//! a state edit. Selecting writes a reactive `selected` cell bound to the strip's
//! `PAINT`, repainting the strip alone (a targeted invalidation, not a rebuild,
//! architecture section 47); the panels' visibility is the layout/paint effect of
//! the deferred `hidden` flips. Lazy panels (build only the selected panel, build
//! the rest on first reveal) are a later slice; this slice builds all panels and
//! hides the unselected ones.
//!
//! ```
//! use viso_widgets::tabs;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let control = tabs()
//!     .tab("Details", |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .tab("History", |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .selected(0)
//!     .on_change(|_ev, index| {
//!         // handle the newly-selected tab index
//!         let _ = index;
//!     });
//!
//! // Tabs authors reactive state, so it builds through a reactive cx.
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! control.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;

use viso_ui::{
    Align, Axis, Border, BoxStyle, BuildCx, Component, DirtyClass, EventCx, FlexStyle, Inset,
    Justify, Key, Length, PointerButtons, PointerPhase, Rgba, Role, Semantics, Size, StateId,
    StateValue,
};

use crate::label;

/// A shared, mutable change callback carrying the newly-selected tab index. It is
/// cloned into every tab's pointer and key handler at build time so a pointer
/// click, a keyboard activation, and an arrow step drive the same `on_change`.
/// Pointer and keyboard input are never concurrent, so the runtime never
/// re-enters the `RefCell` borrow.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// A panel's build-time content builder. It authors the panel's subtree into the
/// panel area; boxed so a `Tabs` can hold a heterogeneous list of closures.
type PanelBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A deselected tab button's fill — a neutral chip that reads against the strip.
const TAB_DESELECTED: Rgba = Rgba {
    r: 0.16,
    g: 0.17,
    b: 0.20,
    a: 1.0,
};

/// A selected tab button's fill — a bright accent so the active tab reads at a
/// glance.
const TAB_SELECTED: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The tab caption color: near-white for contrast against a dark surface.
const CAPTION: Rgba = Rgba {
    r: 0.98,
    g: 0.98,
    b: 1.0,
    a: 1.0,
};

/// A tab button's border, giving a deselected chip a visible outline.
const BORDER: Rgba = Rgba {
    r: 0.45,
    g: 0.47,
    b: 0.52,
    a: 1.0,
};

/// The default tab-button border width.
const BORDER_WIDTH: f32 = 1.0;

/// The default corner radius of a tab chip.
const TAB_RADIUS: f32 = 4.0;

/// The horizontal gap between tab buttons in the strip.
const TAB_GAP: f32 = 4.0;

/// The vertical gap between the strip and the panel area.
const AREA_GAP: f32 = 8.0;

/// The tab chip's internal padding around its caption.
const TAB_PADDING: f32 = 6.0;

/// The visual and layout parameters of a [`Tabs`]: the deselected and selected
/// tab-button fills and the control's own size request within its parent.
///
/// `size` defaults to [`Length::Fill`] on both axes so the control fills its
/// parent region (the panel area grows to hold panel content); override it with a
/// [`Length::Fixed`] or [`Length::Fit`] axis. `deselected` and `selected` default
/// to a bordered neutral chip and a filled accent chip. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabsStyle {
    /// The control's own size request within its parent. Defaults to `Fill` on
    /// both axes.
    pub size: Size,
    /// A tab button's fill/border while deselected.
    pub deselected: BoxStyle,
    /// A tab button's fill while selected.
    pub selected: BoxStyle,
}

impl Default for TabsStyle {
    fn default() -> Self {
        TabsStyle {
            size: Size {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fill { weight: 1.0 },
            },
            deselected: BoxStyle {
                border: Border {
                    width: BORDER_WIDTH,
                    color: BORDER,
                },
                ..BoxStyle::solid(TAB_DESELECTED).with_radius(TAB_RADIUS)
            },
            selected: BoxStyle::solid(TAB_SELECTED).with_radius(TAB_RADIUS),
        }
    }
}

/// One tab: its caption (also its accessible name) and the builder that authors
/// its panel's content.
struct Tab {
    /// The tab's caption, used as its accessible name.
    caption: String,
    /// The panel's content builder.
    panel: PanelBuilder,
}

/// A strip of selectable tabs, each revealing one panel.
///
/// Construct one with [`tabs`], add tabs with [`Tabs::tab`], and attach behavior
/// with the chainable setters. Selecting a tab — a primary-pointer click, an
/// Enter/Space activation, or a Left/Right arrow step — shows its panel, hides the
/// others, and fires `on_change` with the new index.
///
/// See the [module docs](self) for a build example. Invalidation: selecting
/// writes a reactive `selected` cell bound to the strip's `PAINT`, and defers a
/// pair of `hidden` flips (hide the old panel, show the new) the router applies —
/// each marking its panel `LAYOUT | PAINT`. No rebuild: all panels are built once.
pub struct Tabs {
    /// The tabs, in strip order.
    tabs: Vec<Tab>,
    /// The initially-selected tab index (defaults to `0`).
    selected: usize,
    style: TabsStyle,
    /// The shared change callback (see [`SharedChange`]). `None` until
    /// [`Tabs::on_change`] is called; a control with no handler still switches
    /// panels, just without notifying anyone.
    on_change: SharedChange,
}

/// Construct an empty [`Tabs`] with the first tab selected and no handler yet.
/// Chain [`Tabs::tab`] to add tabs, [`Tabs::selected`] to choose the initial tab,
/// [`Tabs::on_change`] to give it behavior, and [`Tabs::style`]/[`Tabs::size`] to
/// adjust its appearance.
pub fn tabs() -> Tabs {
    Tabs {
        tabs: Vec::new(),
        selected: 0,
        style: TabsStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl Tabs {
    /// Add a tab with the given caption (also its accessible name) and a builder
    /// that authors its panel's content into the panel area at build time.
    pub fn tab(
        mut self,
        caption: impl Into<String>,
        panel: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.tabs.push(Tab {
            caption: caption.into(),
            panel: Box::new(panel),
        });
        self
    }

    /// Set the change callback, fired with the newly-selected index on a click, a
    /// keyboard activation, or an arrow step. Replaces any previously set handler.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initially-selected tab index (defaults to `0`).
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Replace the whole [`TabsStyle`].
    pub fn style(mut self, style: TabsStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the control's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// The initial selection clamped to a valid tab index, or `0` when empty.
    fn initial_index(&self) -> usize {
        if self.tabs.is_empty() {
            0
        } else {
            self.selected.min(self.tabs.len() - 1)
        }
    }
}

/// Drive the shared callback with the newly-selected index if one is set; a
/// handler-less control is a no-op. Pointer and keyboard activation never overlap,
/// so the borrow is uncontended.
fn fire(cb: &SharedChange, ev: &mut EventCx<'_>, index: usize) {
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, index);
    }
}

/// Select `index` — but only when it is a genuine change. Selecting the
/// already-selected tab is a no-op: no cell write, no `hidden` flip, no callback.
/// This also coalesces the capture/bubble double-dispatch a router performs when
/// the activation lands on a leaf inside a tab button (the button is then an
/// ancestor, so its handler runs on both passes): the first pass writes the new
/// index, flips the panels, and fires; the second reads the just-written value and
/// short-circuits. `EventCx::set` writes the cell eagerly (the flush is deferred,
/// but the stored value updates now), so the guard sees the first pass's write
/// within the same route.
///
/// The two `set_hidden` requests are deferred (an `EventCx` holds no node store);
/// the router applies them after the handler returns, hiding the old panel and
/// showing the new. `panels[i]` is tab `i`'s panel node.
fn select(
    cb: &SharedChange,
    ev: &mut EventCx<'_>,
    cell: StateId,
    panels: &[viso_ui::NodeId],
    index: usize,
) {
    let next = StateValue::Int(index as i32);
    let prev = match ev.get(cell) {
        Some(StateValue::Int(i)) => i as usize,
        _ => return,
    };
    if prev == index {
        return;
    }
    ev.set(cell, next);
    if let Some(old) = panels.get(prev) {
        ev.set_hidden(*old, true);
    }
    if let Some(new) = panels.get(index) {
        ev.set_hidden(*new, false);
    }
    fire(cb, ev, index);
}

impl Component for Tabs {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // One shared `selected` cell holding the chosen tab index. The strip binds
        // this cell to its PAINT so a selection repaints the strip (the active
        // chip's fill swap) in one targeted invalidation, not a rebuild. Panels
        // switch through deferred `hidden` flips, not this cell.
        let initial = self.initial_index();
        let selected = cx.state(StateValue::Int(initial as i32));

        let tab_count = self.tabs.len();
        let deselected = self.style.deselected;

        // Tab buttons are built before their panels, so a button's handler cannot
        // capture the panel ids by value — they are not known yet. A button needs
        // them only at *event* time (to flip `hidden`), long after the whole tree
        // is built, so it captures a shared, deferred-fill slot instead: the
        // panel-build phase below pushes each panel's id into this vector, and the
        // handlers read it when they run. It is written once during build and read
        // only during event dispatch — never concurrently — so the borrow is
        // uncontended.
        let panels_cell: Rc<RefCell<Vec<viso_ui::NodeId>>> =
            Rc::new(RefCell::new(Vec::with_capacity(tab_count)));

        // The control is a column: a tab strip above a panel area.
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: AREA_GAP,
                padding: Inset::all(0.0),
                align: Align::Stretch,
                justify: Justify::Start,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                // The tab strip: a row of focusable tab buttons. Its handle carries
                // the `TabList` role and the shared PAINT binding.
                let strip = cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: TAB_GAP,
                        padding: Inset::all(0.0),
                        align: Align::Center,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        for (index, tab) in self.tabs.iter().enumerate() {
                            let caption = tab.caption.clone();
                            let button = cx.flex(
                                FlexStyle {
                                    axis: Axis::Row,
                                    gap: 0.0,
                                    padding: Inset::all(TAB_PADDING),
                                    align: Align::Center,
                                    justify: Justify::Start,
                                    size: Size {
                                        width: Length::Fit,
                                        height: Length::Fit,
                                    },
                                    style: deselected,
                                },
                                |cx| {
                                    label(caption.clone()).color(CAPTION).build(cx);
                                },
                            );

                            // Bind the shared cell to this button's PAINT so a
                            // selection repaints the affected chips (the active
                            // chip's fill swap is a later paint slice, matching the
                            // radio dot precedent).
                            cx.bind(selected, button, DirtyClass::PAINT);
                            cx.focusable(button, true);

                            // Pointer activation: a primary press-then-release
                            // selects this tab. The release is the activation,
                            // matching the click gate used across the controls. The
                            // panel nodes are resolved lazily inside the handler —
                            // they are built after the strip, so their ids are not
                            // known here; the handler reads them from the shared
                            // `panels` slice captured below.
                            let pointer_cb = self.on_change.clone();
                            let pointer_panels = panels_cell.clone();
                            cx.on_pointer(button, move |ev| {
                                let Some(p) = ev.pointer() else { return };
                                if p.phase == PointerPhase::Up
                                    && p.buttons.contains(PointerButtons::PRIMARY)
                                {
                                    let panels = pointer_panels.borrow();
                                    select(&pointer_cb, ev, selected, &panels, index);
                                }
                            });

                            // Keyboard activation: Enter/Space activates the focused
                            // tab; Left/Right steps the selection across the strip
                            // (the arrow-step precedent is the splitter, not the
                            // radio's Enter/Space-only model). Auto-repeat is
                            // ignored for the activation keys; arrow steps honor a
                            // held key so a ramp across many tabs works.
                            let key_cb = self.on_change.clone();
                            let key_panels = panels_cell.clone();
                            cx.on_key(button, move |ev| {
                                let Some(k) = ev.key() else { return };
                                if !k.pressed {
                                    return;
                                }
                                let panels = key_panels.borrow();
                                match k.key {
                                    Key::Enter | Key::Space if !k.repeat => {
                                        select(&key_cb, ev, selected, &panels, index);
                                    }
                                    Key::Left if index > 0 => {
                                        select(&key_cb, ev, selected, &panels, index - 1);
                                    }
                                    Key::Right if index + 1 < tab_count => {
                                        select(&key_cb, ev, selected, &panels, index + 1);
                                    }
                                    _ => {}
                                }
                            });

                            // Each tab is an interactive, named node: the `Tab` role
                            // with its caption as its accessible name (AGENTS
                            // section 15). The live selection is proven by the
                            // reactive cell and input tapes.
                            cx.semantics(
                                button,
                                Semantics::role(Role::Tab).with_label(caption.clone()),
                            );
                        }
                    },
                );
                cx.bind(selected, strip, DirtyClass::PAINT);
                cx.semantics(strip, Semantics::role(Role::TabList));

                // The panel area: all panels build once as a stack of groups filling
                // the area. Each panel is hidden except the selected one; switching
                // tabs flips the `hidden` flag, folding the old panel out of layout
                // and paint and the new one in — no rebuild.
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: 0.0,
                        padding: Inset::all(0.0),
                        align: Align::Stretch,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fill { weight: 1.0 },
                        },
                        style: BoxStyle::NONE,
                    },
                    |cx| {
                        let mut panels = panels_cell.borrow_mut();
                        for (index, tab) in self.tabs.iter().enumerate() {
                            let panel = cx.flex(
                                FlexStyle {
                                    axis: Axis::Column,
                                    gap: 0.0,
                                    padding: Inset::all(0.0),
                                    align: Align::Stretch,
                                    justify: Justify::Start,
                                    size: Size {
                                        width: Length::Fill { weight: 1.0 },
                                        height: Length::Fill { weight: 1.0 },
                                    },
                                    style: BoxStyle::NONE,
                                },
                                |cx| {
                                    (tab.panel)(cx);
                                },
                            );
                            // Show only the selected panel; hide the rest at build
                            // time so the initial frame lays out one panel.
                            cx.set_hidden(panel, index != initial);
                            cx.semantics(panel, Semantics::role(Role::Group));
                            panels.push(panel.id());
                        }
                    },
                );
            },
        );

        // The control groups the strip and panels; `Role::Group` is the accessible
        // wrapper. The column `cx.flex` returned the root's handle.
        cx.semantics(root, Semantics::role(Role::Group));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::rc::Rc;
    use viso_ui::{
        BindingTable, KeyEvent, LeafStyle, Modifiers, NodeId, NodeStore, PointerEvent,
        SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a tabs build writes into, kept together so a test can
    /// build the control and then drive its handlers against the same state.
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

        /// Build a control through a reactive cx (it authors state, so a plain
        /// `BuildCx::new` would panic) and return its root (the column container).
        fn build(&mut self, control: Tabs) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("tabs declares a root node")
        }

        /// Feed a pointer sample to a node's pointer handler, restoring it after,
        /// then apply any deferred `hidden` flips the handler recorded — the router
        /// discipline (take, drive, restore, apply) reproduced for the test.
        fn pointer(&mut self, node: NodeId, ev: PointerEvent) {
            let mut handler = self.store.take_handler(node).expect("pointer handler");
            let hidden = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_hidden_requests()
            };
            self.store.restore_handler(node, handler);
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
        }

        /// Feed a key sample to a node's key handler, restoring it after, then
        /// apply any deferred `hidden` flips it recorded.
        fn key(&mut self, node: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            let hidden = {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_hidden_requests()
            };
            self.store.restore_key_handler(node, handler);
            for (id, h) in hidden {
                self.store.set_hidden(id, h);
            }
        }

        /// The current value of the shared selection cell (via a throwaway read
        /// cx).
        fn selected(&mut self, cell: StateId) -> Option<i32> {
            let ev = read_pointer();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(cell) {
                Some(StateValue::Int(i)) => Some(i),
                _ => None,
            }
        }
    }

    /// A neutral pointer sample for read-only cx construction.
    fn read_pointer() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A primary-button pointer sample in the given phase.
    fn primary(phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
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

    /// The shared selection cell authored by the build. Tabs authors exactly one
    /// state cell (the shared `selected` Int), so a fresh `StateStore` allocating
    /// one `Int` cell yields the very handle the build produced (there is no public
    /// constructor for a bare `StateId`, and the build does not return it).
    fn shared_cell() -> StateId {
        StateStore::new().alloc(StateValue::Int(0))
    }

    /// Collect a node's direct children by walking the arena sibling chain.
    fn children(store: &NodeStore, parent: NodeId) -> Vec<NodeId> {
        let arena = store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(parent).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }

    /// A two-tab control for the common test shape.
    fn two_tabs() -> Tabs {
        tabs()
            .tab("One", |cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(10.0, 10.0),
                    style: BoxStyle::NONE,
                });
            })
            .tab("Two", |cx| {
                cx.leaf(LeafStyle {
                    size: Size::fixed(10.0, 10.0),
                    style: BoxStyle::NONE,
                });
            })
    }

    /// The root is a column of two children: the tab strip and the panel area.
    /// The strip holds one focusable, handler-bearing tab button per tab; the panel
    /// area holds one panel per tab.
    #[test]
    fn tabs_builds_a_strip_of_buttons_over_a_panel_area() {
        let mut rx = Reactive::new();
        let root = rx.build(two_tabs());

        let top = children(&rx.store, root);
        assert_eq!(top.len(), 2, "a strip and a panel area");
        let (strip, area) = (top[0], top[1]);

        let buttons = children(&rx.store, strip);
        assert_eq!(buttons.len(), 2, "one tab button per tab");
        for button in &buttons {
            assert!(
                rx.store.has_handler(*button),
                "each tab attaches a pointer handler"
            );
            assert!(
                rx.store.has_key_handler(*button),
                "each tab attaches a key handler"
            );
            assert!(rx.store.focusable(*button), "each tab is focusable");
        }

        let panels = children(&rx.store, area);
        assert_eq!(panels.len(), 2, "one panel per tab");
    }

    /// At build time only the selected panel is shown; the rest are hidden.
    #[test]
    fn only_the_selected_panel_is_shown_at_build() {
        let mut rx = Reactive::new();
        let root = rx.build(two_tabs().selected(1));

        let area = children(&rx.store, root)[1];
        let panels = children(&rx.store, area);
        assert!(rx.store.hidden(panels[0]), "the unselected panel is hidden");
        assert!(!rx.store.hidden(panels[1]), "the selected panel is shown");
    }

    /// The strip is a `TabList`, each button a `Tab` named by its caption, and each
    /// panel a `Group`; the root is a `Group`.
    #[test]
    fn tabs_derive_tablist_over_named_tabs_and_group_panels() {
        let mut rx = Reactive::new();
        let root = rx.build(two_tabs());

        assert_eq!(
            rx.store.semantics(root).expect("root has semantics").role,
            Role::Group,
            "the control is a Group"
        );

        let top = children(&rx.store, root);
        let (strip, area) = (top[0], top[1]);
        assert_eq!(
            rx.store.semantics(strip).expect("strip semantics").role,
            Role::TabList,
            "the strip is a TabList"
        );

        let buttons = children(&rx.store, strip);
        for (button, expected) in buttons.iter().zip(["One", "Two"]) {
            let sem = rx.store.semantics(*button).expect("tab semantics");
            assert_eq!(sem.role, Role::Tab, "each button is a Tab");
            assert_eq!(sem.label.as_deref(), Some(expected), "named by its caption");
        }

        for panel in children(&rx.store, area) {
            assert_eq!(
                rx.store.semantics(panel).expect("panel semantics").role,
                Role::Group,
                "each panel is a Group"
            );
        }
    }

    /// A primary click on a tab selects it, fires `on_change` once with its index,
    /// and flips the panels' `hidden` flags; a press alone does not select.
    #[test]
    fn pointer_click_selects_tab_and_switches_panels() {
        let count = Rc::new(Cell::new(0u32));
        let last = Rc::new(Cell::new(None::<usize>));
        let (c, l) = (count.clone(), last.clone());

        let mut rx = Reactive::new();
        let root = rx.build(two_tabs().on_change(move |_ev, index| {
            l.set(Some(index));
            c.set(c.get() + 1);
        }));
        let cell = shared_cell();
        let top = children(&rx.store, root);
        let (strip, area) = (top[0], top[1]);
        let buttons = children(&rx.store, strip);
        let panels = children(&rx.store, area);

        // A press alone does not select.
        rx.pointer(buttons[1], primary(PointerPhase::Down));
        assert_eq!(count.get(), 0, "the press alone does not select");
        assert_eq!(rx.selected(cell), Some(0), "selection unchanged by a press");

        // Release on the second tab selects index 1 and switches the panels.
        rx.pointer(buttons[1], primary(PointerPhase::Up));
        assert_eq!(count.get(), 1, "press-then-release is one selection");
        assert_eq!(last.get(), Some(1), "on_change carries the selected index");
        assert_eq!(rx.selected(cell), Some(1), "the shared cell holds index 1");
        assert!(rx.store.hidden(panels[0]), "the old panel is now hidden");
        assert!(!rx.store.hidden(panels[1]), "the new panel is now shown");

        // Re-selecting the current tab is a no-op (guarded).
        rx.pointer(buttons[1], primary(PointerPhase::Up));
        assert_eq!(count.get(), 1, "re-selecting the current tab does nothing");
    }

    /// Enter and Space each activate the focused tab; Left/Right step the selection
    /// across the strip. An auto-repeat of Enter and a key-up do not activate.
    #[test]
    fn keyboard_activates_and_arrows_step_selection() {
        let count = Rc::new(Cell::new(0u32));
        let last = Rc::new(Cell::new(None::<usize>));
        let (c, l) = (count.clone(), last.clone());

        let mut rx = Reactive::new();
        let root = rx.build(
            tabs()
                .tab("A", |_cx| {})
                .tab("B", |_cx| {})
                .tab("C", |_cx| {})
                .on_change(move |_ev, index| {
                    l.set(Some(index));
                    c.set(c.get() + 1);
                }),
        );
        let cell = shared_cell();
        let strip = children(&rx.store, root)[0];
        let buttons = children(&rx.store, strip);

        // Right from tab 0 selects tab 1.
        rx.key(buttons[0], key_ev(Key::Right, true, false));
        assert_eq!(rx.selected(cell), Some(1), "Right steps to the next tab");
        assert_eq!(last.get(), Some(1));

        // Enter on the focused tab 2 activates it.
        rx.key(buttons[2], key_ev(Key::Enter, true, false));
        assert_eq!(
            rx.selected(cell),
            Some(2),
            "Enter activates the focused tab"
        );

        // Left from tab 1 selects tab 0.
        rx.key(buttons[1], key_ev(Key::Left, true, false));
        assert_eq!(rx.selected(cell), Some(0), "Left steps to the previous tab");

        // Space activates the focused tab 1.
        rx.key(buttons[1], key_ev(Key::Space, true, false));
        assert_eq!(
            rx.selected(cell),
            Some(1),
            "Space activates the focused tab"
        );

        let before = count.get();
        rx.key(buttons[2], key_ev(Key::Enter, true, true)); // auto-repeat: ignored
        rx.key(buttons[2], key_ev(Key::Enter, false, false)); // key-up: ignored
        assert_eq!(count.get(), before, "repeat and key-up do not activate");
    }

    /// Left at the first tab and Right at the last tab are clamped no-ops.
    #[test]
    fn arrow_steps_clamp_at_the_ends() {
        let mut rx = Reactive::new();
        let root = rx.build(two_tabs());
        let cell = shared_cell();
        let strip = children(&rx.store, root)[0];
        let buttons = children(&rx.store, strip);

        rx.key(buttons[0], key_ev(Key::Left, true, false));
        assert_eq!(
            rx.selected(cell),
            Some(0),
            "Left at the first tab is a no-op"
        );

        rx.pointer(buttons[1], primary(PointerPhase::Up));
        assert_eq!(rx.selected(cell), Some(1));
        rx.key(buttons[1], key_ev(Key::Right, true, false));
        assert_eq!(
            rx.selected(cell),
            Some(1),
            "Right at the last tab is a no-op"
        );
    }

    /// A control with no handler still switches panels through the shared cell —
    /// `build` does not panic and the handlers are no-ops on the callback.
    #[test]
    fn handlerless_tabs_still_switches() {
        let mut rx = Reactive::new();
        let root = rx.build(two_tabs());
        let cell = shared_cell();
        let top = children(&rx.store, root);
        let (strip, area) = (top[0], top[1]);
        let buttons = children(&rx.store, strip);
        let panels = children(&rx.store, area);

        rx.pointer(buttons[1], primary(PointerPhase::Up));
        assert_eq!(
            rx.selected(cell),
            Some(1),
            "a handler-less control still moves the shared cell"
        );
        assert!(!rx.store.hidden(panels[1]), "and shows the new panel");
    }

    /// The initial selection index is written into the shared cell at build time.
    #[test]
    fn initial_selection_seeds_the_shared_cell() {
        let mut rx = Reactive::new();
        rx.build(two_tabs().selected(1));
        let cell = shared_cell();
        assert_eq!(
            rx.selected(cell),
            Some(1),
            "the initial index seeds the cell"
        );
    }
}
