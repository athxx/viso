//! The [`Tabs`] control — a strip of tabs, each showing one panel.
//!
//! A strip of tabs runs above a panel area holding one panel per tab, of
//! which the selected tab's alone shows. A press on a tab selects it; so do
//! the arrows while a tab has the focus (mirrored right to left), Home and
//! End going to the first and last, and Ctrl+Page Up/Page Down from anywhere
//! in the control. The tabs are one Tab stop, landing on the selected tab.
//!
//! A strip wider than the control scrolls: a wheel turning either way and a
//! finger move it, and a newly selected tab scrolls into sight.
//!
//! A [`closable`](Tabs::closable) tab wears a close button; it, a middle
//! press, or Delete while the tab has the focus closes the tab, freeing its
//! button and panel. Closing the selected tab selects the one before it,
//! else the one after. A [`permanent_tab`](Tabs::permanent_tab) never
//! closes.
//!
//! In a [`reorderable`](Tabs::reorderable) strip a tab dragged along it is
//! carried to another place, the others sliding aside, the strip scrolling
//! when it is held at an end; Ctrl+Shift+Page Up/Page Down moves the focused
//! tab a place. Each change fires its handler: `on_change` and `on_close`
//! with the tab — its place among the tabs as they were declared — and
//! `on_reorder` with where in the strip it came from and went.
//!
//! Semantics: a `group` holding a `tablist` of `tab`s named by their
//! captions, each announcing whether it is selected and its place, and a
//! `tabpanel` per tab named as it is. A close button is a `button` named
//! "Close" and the tab's caption.
//!
//! Invalidation: the selected tab lives in one reactive cell, each tab's
//! look in a cell of its own, and a revision cell the strip's structure hook
//! reads; selecting flips panels' `hidden` flags, and closing and moving
//! tabs free and relink retained nodes. All panels build once.
//!
//! ```
//! use viso_widgets::tabs;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, Component, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Size};
//!
//! let control = tabs()
//!     .tab("Details", |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .tab("History", |cx| { cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE }); })
//!     .closable(true)
//!     .reorderable(true)
//!     .on_change(|_ev, tab| {
//!         let _ = tab;
//!     });
//!
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
    Align, Axis, Border, BoxStyle, BuildCx, Component, DispatchPhase, EventCx, FlexStyle, Handle,
    Inset, InteractionStyle, Justify, Key, Length, NodeId, PointerButtons, PointerPhase, Rgba,
    Role, ScrollBars, ScrollStyle, SemanticState, Semantics, Size, StateId, StateValue,
    StructureCx,
};

use super::glyphs::{cross, glyph_slot};
use crate::label;

/// What runs with a tab selected or closed.
type SharedTab = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// What runs with a tab moved from one place in the strip to another.
type SharedMove = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize, usize)>>>>;

/// A panel's content builder.
type PanelBuilder = Box<dyn Fn(&mut BuildCx<'_>)>;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// How a [`Tabs`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TabsStyle {
    /// The control's own size within its parent (default `Fill` both ways).
    pub size: Size,
    /// The strip behind the tabs.
    pub strip: BoxStyle,
    /// A tab at rest, under the pointer, and selected.
    pub deselected: BoxStyle,
    pub hover: BoxStyle,
    pub selected: BoxStyle,
    /// Around a tab's caption and close button.
    pub padding: Inset,
    /// Between the tabs, and between the strip and the panels.
    pub gap: f32,
    pub area_gap: f32,
    /// The captions' ink.
    pub caption: Rgba,
    /// The close button's cross, and the button under the pointer.
    pub close: Rgba,
    pub close_hover: BoxStyle,
}

impl Default for TabsStyle {
    fn default() -> Self {
        TabsStyle {
            size: Size::fill(),
            strip: BoxStyle::NONE,
            deselected: BoxStyle {
                border: Border {
                    width: 1.0,
                    color: rgba(0.45, 0.47, 0.52, 1.0),
                },
                ..BoxStyle::solid(rgba(0.16, 0.17, 0.20, 1.0)).with_radius(4.0)
            },
            hover: BoxStyle {
                border: Border {
                    width: 1.0,
                    color: rgba(0.55, 0.57, 0.62, 1.0),
                },
                ..BoxStyle::solid(rgba(0.22, 0.23, 0.27, 1.0)).with_radius(4.0)
            },
            selected: BoxStyle::solid(rgba(0.22, 0.45, 0.85, 1.0)).with_radius(4.0),
            padding: Inset::all(6.0),
            gap: 4.0,
            area_gap: 8.0,
            caption: rgba(0.98, 0.98, 1.0, 1.0),
            close: rgba(0.85, 0.86, 0.9, 1.0),
            close_hover: BoxStyle::solid(rgba(1.0, 1.0, 1.0, 0.16)).with_radius(4.0),
        }
    }
}

/// One tab as declared.
struct TabSpec {
    caption: String,
    /// Whether it may close when the strip's tabs do.
    permanent: bool,
    panel: PanelBuilder,
}

/// A strip of tabs, each showing one panel. Construct one with [`tabs`].
pub struct Tabs {
    tabs: Vec<TabSpec>,
    selected: usize,
    closable: bool,
    reorderable: bool,
    close_label: String,
    style: TabsStyle,
    on_change: SharedTab,
    on_close: SharedTab,
    on_reorder: SharedMove,
}

/// A control with no tabs yet, the first to be selected.
pub fn tabs() -> Tabs {
    Tabs {
        tabs: Vec::new(),
        selected: 0,
        closable: false,
        reorderable: false,
        close_label: "Close".to_owned(),
        style: TabsStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
        on_close: Rc::new(RefCell::new(None)),
        on_reorder: Rc::new(RefCell::new(None)),
    }
}

impl Tabs {
    /// Add a tab captioned `caption` whose panel `panel` builds.
    pub fn tab(
        mut self,
        caption: impl Into<String>,
        panel: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.tabs.push(TabSpec {
            caption: caption.into(),
            permanent: false,
            panel: Box::new(panel),
        });
        self
    }

    /// Add a tab that never closes, though the others do.
    pub fn permanent_tab(
        mut self,
        caption: impl Into<String>,
        panel: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.tabs.push(TabSpec {
            caption: caption.into(),
            permanent: true,
            panel: Box::new(panel),
        });
        self
    }

    /// Run `handler` with each tab selected.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Run `handler` with each tab closed.
    pub fn on_close(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_close.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Run `handler` with where in the strip each tab moved from and to.
    pub fn on_reorder(self, handler: impl FnMut(&mut EventCx<'_>, usize, usize) + 'static) -> Self {
        *self.on_reorder.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Start with tab `tab` selected (default the first).
    pub fn selected(mut self, tab: usize) -> Self {
        self.selected = tab;
        self
    }

    /// Let the tabs close, each but a [`permanent_tab`](Self::permanent_tab).
    pub fn closable(mut self, on: bool) -> Self {
        self.closable = on;
        self
    }

    /// Let the tabs be dragged to other places in the strip.
    pub fn reorderable(mut self, on: bool) -> Self {
        self.reorderable = on;
        self
    }

    /// Name the close buttons `label` and the caption (default "Close").
    pub fn close_label(mut self, label: impl Into<String>) -> Self {
        self.close_label = label.into();
        self
    }

    /// Replace the whole [`TabsStyle`].
    pub fn style(mut self, style: TabsStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the control's own size within its parent (default `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }
}

/// One tab's nodes and cells.
#[derive(Debug, Clone, Copy)]
struct TabNodes {
    button: NodeId,
    panel: NodeId,
    /// Whether it is selected: its look.
    on: StateId,
    closable: bool,
}

/// A tab's button as built: its close button and that button's hover
/// cell, and the cells of its look.
#[derive(Clone, Copy)]
struct Built {
    button: Handle,
    close: Option<(Handle, StateId)>,
    on: StateId,
    hover: StateId,
}

/// What a control shares between its handlers and its hook.
struct Strip {
    /// The tabs as declared.
    tabs: Vec<TabNodes>,
    /// The row the tab buttons lie in.
    row: NodeId,
    /// The selected tab, `-1` for none.
    selected: StateId,
    /// Bumped when the strip's order or its tabs change.
    revision: StateId,
    /// The open tabs, in the strip's order.
    order: RefCell<Vec<usize>>,
    /// The tabs closed whose nodes are not freed yet.
    closed: RefCell<Vec<usize>>,
    on_change: SharedTab,
    on_close: SharedTab,
    on_reorder: SharedMove,
}

impl Strip {
    fn selected(&self, ev: &EventCx<'_>) -> Option<usize> {
        match ev.get(self.selected) {
            Some(StateValue::Int(i)) if i >= 0 => Some(i as usize),
            _ => None,
        }
    }

    /// Tab `tab`'s place in the strip.
    fn place(&self, tab: usize) -> Option<usize> {
        self.order.borrow().iter().position(|&t| t == tab)
    }

    /// The tab `by` places along from `tab`, held to the ends.
    fn along(&self, tab: usize, by: i32) -> Option<usize> {
        let order = self.order.borrow();
        let at = order.iter().position(|&t| t == tab)? as i32 + by;
        let at = at.clamp(0, order.len() as i32 - 1);
        order.get(at as usize).copied()
    }

    /// Select tab `tab`, reporting it.
    fn select(&self, ev: &mut EventCx<'_>, tab: usize) {
        let was = self.selected(ev);
        if was == Some(tab) || self.place(tab).is_none() {
            return;
        }
        if let Some(old) = was.and_then(|w| self.tabs.get(w)) {
            ev.set(old.on, StateValue::Bool(false));
            ev.set_hidden(old.panel, true);
        }
        let new = self.tabs[tab];
        ev.set(new.on, StateValue::Bool(true));
        ev.set_hidden(new.panel, false);
        ev.set(self.selected, StateValue::Int(tab as i32));
        ev.scroll_into_view(new.button);
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, tab);
        }
    }

    /// Select tab `tab` and give it the focus.
    fn go(&self, ev: &mut EventCx<'_>, tab: usize) {
        self.select(ev, tab);
        ev.request_focus(self.tabs[tab].button);
    }

    /// Close tab `tab`, selecting a neighbour if it was selected.
    fn close(&self, ev: &mut EventCx<'_>, tab: usize) {
        let Some(at) = self.place(tab) else {
            return;
        };
        let t = self.tabs[tab];
        if !t.closable {
            return;
        }
        let had_focus = ev.focused() == Some(t.button);
        self.order.borrow_mut().remove(at);
        let next = {
            let order = self.order.borrow();
            at.checked_sub(1)
                .or(Some(at))
                .and_then(|i| order.get(i).copied())
        };
        if self.selected(ev) == Some(tab) {
            ev.set(t.on, StateValue::Bool(false));
            ev.set(self.selected, StateValue::Int(-1));
            if let Some(next) = next {
                self.select(ev, next);
            }
        }
        if had_focus && let Some(next) = next {
            ev.request_focus(self.tabs[next].button);
        }
        ev.set_hidden(t.button, true);
        ev.set_hidden(t.panel, true);
        self.closed.borrow_mut().push(tab);
        self.bump(ev);
        if let Some(f) = self.on_close.borrow_mut().as_mut() {
            f(ev, tab);
        }
    }

    /// Move the tab at place `from` in the strip to place `to`.
    fn move_tab(&self, ev: &mut EventCx<'_>, from: usize, to: usize) {
        {
            let mut order = self.order.borrow_mut();
            if from >= order.len() || to >= order.len() || from == to {
                return;
            }
            let tab = order.remove(from);
            order.insert(to, tab);
        }
        self.bump(ev);
        if let Some(f) = self.on_reorder.borrow_mut().as_mut() {
            f(ev, from, to);
        }
    }

    fn bump(&self, ev: &mut EventCx<'_>) {
        let n = match ev.get(self.revision) {
            Some(StateValue::Int(n)) => n,
            _ => 0,
        };
        ev.set(self.revision, StateValue::Int(n.wrapping_add(1)));
    }

    /// What a key on tab `tab` asks.
    fn key(&self, ev: &mut EventCx<'_>, tab: usize, reorderable: bool) {
        let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        let m = k.modifiers;
        let bare = !m.shift && !m.control && !m.alt && !m.logo;
        let across = if ev.reads_rtl() { -1 } else { 1 };
        let taken = match k.key {
            Key::Left | Key::Right if bare => {
                let by = if k.key == Key::Right { across } else { -across };
                if let Some(to) = self.along(tab, by) {
                    self.go(ev, to);
                }
                true
            }
            Key::Home | Key::End if bare => {
                let order = self.order.borrow();
                let to = if k.key == Key::Home {
                    order.first()
                } else {
                    order.last()
                }
                .copied();
                drop(order);
                if let Some(to) = to {
                    self.go(ev, to);
                }
                true
            }
            Key::Enter | Key::Space if bare => {
                if !k.repeat {
                    self.select(ev, tab);
                }
                true
            }
            Key::Delete if bare && self.tabs[tab].closable => {
                if !k.repeat {
                    self.close(ev, tab);
                }
                true
            }
            Key::PageUp | Key::PageDown if reorderable && m.control && m.shift => {
                if let Some(from) = self.place(tab) {
                    let to = if k.key == Key::PageDown {
                        from + 1
                    } else {
                        from.wrapping_sub(1)
                    };
                    self.move_tab(ev, from, to);
                    ev.scroll_into_view(self.tabs[tab].button);
                }
                true
            }
            _ => false,
        };
        if taken {
            ev.stop_propagation();
        }
    }

    /// Ctrl+Page Up/Page Down anywhere in the control: the tab before or
    /// after the selected one.
    fn page_key(&self, ev: &mut EventCx<'_>) {
        let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        let m = k.modifiers;
        if !(m.control && !m.shift && !m.alt && matches!(k.key, Key::PageUp | Key::PageDown)) {
            return;
        }
        ev.stop_propagation();
        let Some(tab) = self.selected(ev) else {
            return;
        };
        let by = if k.key == Key::PageDown { 1 } else { -1 };
        if let Some(to) = self.along(tab, by) {
            let focus_in_strip = self.tabs.iter().any(|t| ev.focused() == Some(t.button));
            if focus_in_strip {
                self.go(ev, to);
            } else {
                self.select(ev, to);
            }
        }
    }

    /// Lay the strip out as its order says, free closed tabs, and announce
    /// each tab's place and selection.
    fn show(&self, s: &mut StructureCx<'_>) {
        let closed = std::mem::take(&mut *self.closed.borrow_mut());
        if !closed.is_empty() {
            let mut scratch = Vec::new();
            for tab in closed {
                let t = self.tabs[tab];
                s.store.free_tree(t.button, s.effects, &mut scratch);
                s.store.free_tree(t.panel, s.effects, &mut scratch);
            }
        }
        let order = self.order.borrow();
        let in_order = s
            .store
            .arena()
            .children(self.row)
            .eq(order.iter().map(|&t| self.tabs[t].button));
        if !in_order {
            for &tab in order.iter() {
                let button = self.tabs[tab].button;
                s.store.arena_detach(button);
                s.store.arena_insert_before(self.row, button, None);
            }
        }
        let selected = match s.states.get(self.selected) {
            Some(StateValue::Int(i)) if i >= 0 => Some(i as usize),
            _ => None,
        };
        let n = order.len() as u32;
        for (at, &tab) in order.iter().enumerate() {
            s.store.set_semantic_state(
                self.tabs[tab].button,
                SemanticState {
                    selected: Some(selected == Some(tab)),
                    position: Some((at as u32 + 1, n)),
                    ..SemanticState::default()
                },
            );
        }
    }
}

impl Component for Tabs {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let initial = (!self.tabs.is_empty()).then(|| self.selected.min(self.tabs.len() - 1));
        let selected = cx.state(StateValue::Int(initial.map_or(-1, |i| i as i32)));
        let revision = cx.state(StateValue::Int(0));
        let mut built: Vec<Built> = Vec::new();
        let mut panels: Vec<Handle> = Vec::new();
        let mut row = None;
        let mut viewport = None;
        let root = cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: style.area_gap,
                align: Align::Stretch,
                justify: Justify::Start,
                size: style.size,
                ..FlexStyle::default()
            },
            |cx| {
                viewport = Some(cx.scroll(
                    ScrollStyle {
                        axis: Axis::Row,
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fit,
                        },
                        style: style.strip,
                        bars: ScrollBars::Hidden,
                        ..ScrollStyle::default()
                    },
                    |cx| {
                        row = Some(cx.flex(
                            FlexStyle {
                                axis: Axis::Row,
                                gap: style.gap,
                                align: Align::Center,
                                justify: Justify::Start,
                                size: Size {
                                    width: Length::Fit,
                                    height: Length::Fit,
                                },
                                ..FlexStyle::default()
                            },
                            |cx| {
                                for (i, tab) in self.tabs.iter().enumerate() {
                                    let closable = self.closable && !tab.permanent;
                                    let mut close = None;
                                    let button = cx.flex(
                                        FlexStyle {
                                            axis: Axis::Row,
                                            gap: 4.0,
                                            padding: style.padding,
                                            align: Align::Center,
                                            justify: Justify::Start,
                                            size: Size {
                                                width: Length::Fit,
                                                height: Length::Fit,
                                            },
                                            style: if initial == Some(i) {
                                                style.selected
                                            } else {
                                                style.deselected
                                            },
                                        },
                                        |cx| {
                                            label(tab.caption.clone())
                                                .color(style.caption)
                                                .build(cx);
                                            if closable {
                                                close = Some(close_button(cx, style));
                                            }
                                        },
                                    );
                                    let on = cx.state(StateValue::Bool(initial == Some(i)));
                                    let hover = cx.state(StateValue::Bool(false));
                                    built.push(Built {
                                        button,
                                        close,
                                        on,
                                        hover,
                                    });
                                }
                            },
                        ));
                    },
                ));
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        align: Align::Stretch,
                        justify: Justify::Start,
                        size: Size::fill(),
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for (i, tab) in self.tabs.iter().enumerate() {
                            let panel = cx.flex(
                                FlexStyle {
                                    axis: Axis::Column,
                                    align: Align::Stretch,
                                    justify: Justify::Start,
                                    size: Size::fill(),
                                    ..FlexStyle::default()
                                },
                                |cx| (tab.panel)(cx),
                            );
                            cx.set_hidden(panel, initial != Some(i));
                            cx.semantics(
                                panel,
                                Semantics::role(Role::TabPanel).with_label(tab.caption.clone()),
                            );
                            panels.push(panel);
                        }
                    },
                );
            },
        );
        cx.semantics(root, Semantics::role(Role::Group));
        let (viewport, row) = (viewport.expect("built"), row.expect("built"));
        cx.wheel_across(viewport, true);
        cx.semantics(row, Semantics::role(Role::TabList));
        cx.roving(row);
        if self.reorderable {
            cx.reorder_list(viewport, true);
        }
        let strip = Rc::new(Strip {
            tabs: built
                .iter()
                .zip(&panels)
                .map(|(b, panel)| TabNodes {
                    button: b.button.id(),
                    panel: panel.id(),
                    on: b.on,
                    closable: b.close.is_some(),
                })
                .collect(),
            row: row.id(),
            selected,
            revision,
            order: RefCell::new((0..self.tabs.len()).collect()),
            closed: RefCell::new(Vec::new()),
            on_change: Rc::clone(&self.on_change),
            on_close: Rc::clone(&self.on_close),
            on_reorder: Rc::clone(&self.on_reorder),
        });
        for (
            i,
            &Built {
                button,
                close,
                on,
                hover,
            },
        ) in built.iter().enumerate()
        {
            let caption = &self.tabs[i].caption;
            cx.focusable(button, true);
            cx.semantics(
                button,
                Semantics::role(Role::Tab).with_label(caption.clone()),
            );
            cx.interaction_style(
                button,
                InteractionStyle {
                    resting: style.deselected,
                    hover: style.hover,
                    pressed: style.selected,
                    pressed_cell: Some(on),
                    hover_cell: Some(hover),
                },
            );
            if self.reorderable {
                cx.reorder_handle(button);
            }
            let s = Rc::clone(&strip);
            cx.on_pointer(button, move |ev| {
                if ev.phase() == DispatchPhase::Capture {
                    return;
                }
                let Some(p) = ev.pointer().copied() else {
                    return;
                };
                match p.phase {
                    PointerPhase::Enter => {
                        ev.set(hover, StateValue::Bool(true));
                    }
                    PointerPhase::Leave => {
                        ev.set(hover, StateValue::Bool(false));
                    }
                    PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                        s.select(ev, i);
                        if ev.focused() != Some(s.tabs[i].button) {
                            ev.request_focus(s.tabs[i].button);
                        }
                    }
                    PointerPhase::Down if p.buttons.contains(PointerButtons::MIDDLE) => {
                        s.close(ev, i);
                    }
                    _ => {}
                }
            });
            let s = Rc::clone(&strip);
            let reorderable = self.reorderable;
            cx.on_key(button, move |ev| s.key(ev, i, reorderable));
            if let Some((close, close_hover)) = close {
                cx.semantics(
                    close,
                    Semantics::role(Role::Button)
                        .with_label(format!("{} {caption}", self.close_label)),
                );
                let s = Rc::clone(&strip);
                cx.on_pointer(close, move |ev| {
                    if ev.phase() == DispatchPhase::Capture {
                        return;
                    }
                    let Some(p) = ev.pointer().copied() else {
                        return;
                    };
                    let primary = p.buttons.contains(PointerButtons::PRIMARY);
                    match p.phase {
                        PointerPhase::Enter => {
                            ev.set(close_hover, StateValue::Bool(true));
                        }
                        PointerPhase::Leave => {
                            ev.set(close_hover, StateValue::Bool(false));
                        }
                        // The press is the button's, not the tab's.
                        PointerPhase::Down if primary => ev.stop_propagation(),
                        PointerPhase::Up if primary => {
                            ev.stop_propagation();
                            s.close(ev, i);
                        }
                        _ => {}
                    }
                });
            }
        }
        // A tab carried elsewhere in the strip.
        let s = Rc::clone(&strip);
        cx.on_key(viewport, move |ev| {
            if let Some(viso_ui::RowGesture::Reordered { from, to }) = ev.row_gesture() {
                s.move_tab(ev, from, to);
            }
        });
        let s = Rc::clone(&strip);
        cx.on_key(root, move |ev| s.page_key(ev));
        cx.structure(|sx| {
            strip.show(sx);
            let hook = Rc::clone(&strip);
            sx.store
                .add_structure_hook([selected, revision], move |sx, _| hook.show(sx));
        });
    }
}

/// A tab's close button: a cross the pointer lights, and the cell its
/// hover is in.
fn close_button(cx: &mut BuildCx<'_>, style: TabsStyle) -> (Handle, StateId) {
    let hover = cx.state(StateValue::Bool(false));
    let button = glyph_slot(cx, |cx| {
        cross(cx, style.close);
    });
    let resting = BoxStyle::NONE.with_radius(style.close_hover.radius);
    cx.interaction_style(
        button,
        InteractionStyle {
            resting,
            hover: style.close_hover,
            pressed: style.close_hover,
            pressed_cell: None,
            hover_cell: Some(hover),
        },
    );
    (button, hover)
}
