//! The [`AdaptiveNavigation`] control — an app's top-level destinations, as a
//! bottom bar, a rail or a sidebar by size class.
//!
//! Each destination is a caption, an optional icon and a page; the selected
//! destination's page fills the content area and the others fold away. How
//! the destinations are laid out follows the size class the environment gives
//! the control (the window's, or the nearest adaptive scope's), by default:
//!
//! - compact: a bar along the bottom, the destinations sharing its width,
//!   each icon above its caption;
//! - medium: a rail along the leading edge, the destinations stacked from
//!   the top, each icon above its caption;
//! - expanded: a sidebar along the leading edge, each icon beside its
//!   caption.
//!
//! A press on a destination selects it; so do the arrows while one has the
//! focus (along the bar, mirrored right to left, or up and down the rail and
//! the sidebar), Home and End going to the first and last. The destinations
//! are one Tab stop, landing on the selected one.
//!
//! Every page builds once. A size class change re-arranges the same nodes —
//! the bar moves to the other side of the content and turns its axis — so the
//! pages' state and the focus survive it.
//!
//! Semantics: a `group` holding a `tablist` of `tab`s named by their
//! captions, each announcing whether it is selected and its place, and a
//! `tabpanel` per destination.
//!
//! ```
//! use viso_widgets::adaptive_navigation;
//! use viso_ui::{SemanticProjector, BuildCx, BindingTable, LeafStyle, NodeStore, StateStore, TextEdits, VirtualLists};
//! use viso_ui::{BoxStyle, Sizing};
//!
//! let nav = adaptive_navigation()
//!     .destination("Home", |cx| { cx.leaf(LeafStyle { size: Sizing::fill(), style: BoxStyle::NONE }); })
//!     .destination("Search", |cx| { cx.leaf(LeafStyle { size: Sizing::fill(), style: BoxStyle::NONE }); })
//!     .on_change(|_ev, destination| { let _ = destination; });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let handle = nav.build(&mut cx);
//! ```

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::adaptive::{AnchorId, EnvField, SizeClass};
use viso_ui::{
    Align, Axis, BoxStyle, BuildCx, ColorToken, Component, Controls, DirtyClass, DispatchPhase,
    EventCx, FlexStyle, Inset, InteractionStyle, Justify, Key, Length, NodeId, NodeStore,
    PageTransition, PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Sizing,
    StackStyle, StateId, StateValue, StructureCx,
};

use crate::label;

/// How an [`AdaptiveNavigation`] lays its destinations out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NavigationLayout {
    /// A bar along the bottom, the destinations sharing its width, each icon
    /// above its caption.
    Bar,
    /// A narrow rail along the leading edge, the destinations stacked from
    /// the top, each icon above its caption.
    Rail,
    /// A sidebar along the leading edge, each icon beside its caption.
    Sidebar,
}

/// What runs with a destination selected.
type SharedSelect = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// A page's or an icon's builder.
type Builder = Box<dyn Fn(&mut BuildCx<'_>)>;

viso_ui::control_handle! {
    /// A handle an app captures to select a built [`AdaptiveNavigation`]'s
    /// destinations from an event handler.
    ///
    /// `Copy`, an id alone: `build` returns it, or mint one first with
    /// [`BuildCx::handle`](viso_ui::BuildCx::handle). Once the control is
    /// gone, every call is a no-op.
    pub struct DestinationHandle;
}

impl DestinationHandle {
    /// Select destination `destination`, reporting it; past the last one, a
    /// no-op.
    pub fn select(self, ev: &mut EventCx<'_>, destination: usize) {
        if let Some(parts) = ev.control::<Destinations>(self.0) {
            parts.select(ev, destination);
        }
    }

    /// The selected destination, `None` once the control is gone.
    pub fn selected(self, ev: &mut EventCx<'_>) -> Option<usize> {
        ev.control::<Destinations>(self.0)
            .and_then(|parts| parts.selected(ev))
    }

    /// How the destinations are laid out now, `None` once the control is
    /// gone or before its first arrangement.
    pub fn layout(self, ev: &mut EventCx<'_>) -> Option<NavigationLayout> {
        ev.control::<Destinations>(self.0)
            .and_then(|parts| parts.layout.get())
    }
}

/// How an [`AdaptiveNavigation`] looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveNavigationStyle {
    /// The control's own size within its parent (default `Fill` both ways).
    pub size: Sizing,
    /// Behind the destinations.
    pub bar: BoxStyle,
    /// A destination at rest, under the pointer, and selected.
    pub deselected: BoxStyle,
    pub hover: BoxStyle,
    pub selected: BoxStyle,
    /// The captions' ink.
    pub caption: Rgba,
    /// Around the destinations, and between them.
    pub padding: Inset,
    pub gap: f32,
    /// Around a destination's icon and caption.
    pub item_padding: Inset,
    /// The rail's width and the sidebar's.
    pub rail_width: f32,
    pub sidebar_width: f32,
    /// The layout at each size class: compact, medium, expanded.
    pub layouts: [NavigationLayout; 3],
    /// How the pages trade places.
    pub transition: PageTransition,
}

impl Default for AdaptiveNavigationStyle {
    fn default() -> Self {
        AdaptiveNavigationStyle {
            size: Sizing::fill(),
            bar: BoxStyle::solid(ColorToken::Surface.rgba()),
            deselected: BoxStyle::NONE.with_radius(16.0),
            hover: BoxStyle::solid(ColorToken::OnSurface.alpha(0.08)).with_radius(16.0),
            selected: BoxStyle::solid(ColorToken::Primary.alpha(0.2)).with_radius(16.0),
            caption: ColorToken::Foreground.rgba(),
            padding: Inset::all(8.0),
            gap: 4.0,
            item_padding: Inset::all(8.0),
            rail_width: 80.0,
            sidebar_width: 240.0,
            layouts: [
                NavigationLayout::Bar,
                NavigationLayout::Rail,
                NavigationLayout::Sidebar,
            ],
            transition: PageTransition::Fade,
        }
    }
}

/// One destination as declared.
struct Destination {
    caption: String,
    icon: Option<Builder>,
    page: Builder,
}

/// An app's top-level destinations, as a bottom bar, a rail or a sidebar by
/// size class. Construct one with [`adaptive_navigation`]; see the
/// [module docs](self).
pub struct AdaptiveNavigation {
    destinations: Vec<Destination>,
    selected: usize,
    style: AdaptiveNavigationStyle,
    on_change: SharedSelect,
    handle: Option<DestinationHandle>,
}

/// A control with no destinations yet, the first to be selected.
pub fn adaptive_navigation() -> AdaptiveNavigation {
    AdaptiveNavigation {
        destinations: Vec::new(),
        selected: 0,
        style: AdaptiveNavigationStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
        handle: None,
    }
}

impl AdaptiveNavigation {
    /// Add a destination captioned `caption` whose page `page` builds.
    pub fn destination(
        mut self,
        caption: impl Into<String>,
        page: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.destinations.push(Destination {
            caption: caption.into(),
            icon: None,
            page: Box::new(page),
        });
        self
    }

    /// Add a destination captioned `caption` beside the icon `icon` builds,
    /// whose page `page` builds.
    pub fn destination_with_icon(
        mut self,
        caption: impl Into<String>,
        icon: impl Fn(&mut BuildCx<'_>) + 'static,
        page: impl Fn(&mut BuildCx<'_>) + 'static,
    ) -> Self {
        self.destinations.push(Destination {
            caption: caption.into(),
            icon: Some(Box::new(icon)),
            page: Box::new(page),
        });
        self
    }

    /// Start with destination `destination` selected (default the first).
    pub fn selected(mut self, destination: usize) -> Self {
        self.selected = destination;
        self
    }

    /// Run `handler` with each destination selected.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Replace the whole [`AdaptiveNavigationStyle`].
    pub fn style(mut self, style: AdaptiveNavigationStyle) -> Self {
        self.style = style;
        self
    }

    /// Lay the destinations out as `compact`, `medium` and `expanded` say at
    /// each size class (default bar, rail, sidebar).
    pub fn layouts(
        mut self,
        compact: NavigationLayout,
        medium: NavigationLayout,
        expanded: NavigationLayout,
    ) -> Self {
        self.style.layouts = [compact, medium, expanded];
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`](viso_ui::BuildCx::handle)
    /// before the control is built.
    pub fn handle(mut self, handle: DestinationHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the control into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> DestinationHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

/// One destination's nodes and cells.
#[derive(Debug, Clone, Copy)]
struct Item {
    button: NodeId,
    /// Whether it is selected: its look.
    on: StateId,
}

/// What a control's handlers, its hook and its handle share.
struct Destinations {
    items: Vec<Item>,
    root: NodeId,
    /// The destinations' strip, and the paged stack of their pages.
    nav: NodeId,
    content: NodeId,
    /// The selected destination, `-1` for none.
    selected: StateId,
    anchor: Cell<Option<AnchorId>>,
    layout: Cell<Option<NavigationLayout>>,
    style: AdaptiveNavigationStyle,
    on_change: SharedSelect,
}

impl Destinations {
    fn selected(&self, ev: &EventCx<'_>) -> Option<usize> {
        match ev.get(self.selected) {
            Some(StateValue::Int(i)) if i >= 0 => Some(i as usize),
            _ => None,
        }
    }

    /// Select destination `to`, reporting it.
    fn select(&self, ev: &mut EventCx<'_>, to: usize) {
        let was = self.selected(ev);
        if was == Some(to) || to >= self.items.len() {
            return;
        }
        if let Some(old) = was.and_then(|w| self.items.get(w)) {
            ev.set(old.on, StateValue::Bool(false));
        }
        ev.set(self.items[to].on, StateValue::Bool(true));
        ev.set(self.selected, StateValue::Int(to as i32));
        ev.set_page(self.content, Some(to), self.style.transition);
        if let Some(f) = self.on_change.borrow_mut().as_mut() {
            f(ev, to);
        }
    }

    /// Select destination `to` and give it the focus.
    fn go(&self, ev: &mut EventCx<'_>, to: usize) {
        self.select(ev, to);
        ev.request_focus(self.items[to].button);
    }

    /// What a key on destination `at` asks.
    fn key(&self, ev: &mut EventCx<'_>, at: usize) {
        let Some(k) = ev.key().copied().filter(|k| k.pressed) else {
            return;
        };
        let m = k.modifiers;
        if m.shift || m.control || m.alt || m.logo {
            return;
        }
        let along = self.layout.get() == Some(NavigationLayout::Bar);
        let across = if ev.reads_rtl() { -1 } else { 1 };
        let last = self.items.len() - 1;
        let step = |by: i32| (at as i32 + by).clamp(0, last as i32) as usize;
        let to = match k.key {
            Key::Left if along => Some(step(-across)),
            Key::Right if along => Some(step(across)),
            Key::Up if !along => Some(step(-1)),
            Key::Down if !along => Some(step(1)),
            Key::Home => Some(0),
            Key::End => Some(last),
            Key::Enter | Key::Space => {
                if !k.repeat {
                    self.select(ev, at);
                }
                ev.stop_propagation();
                return;
            }
            _ => None,
        };
        if let Some(to) = to {
            self.go(ev, to);
            ev.stop_propagation();
        }
    }

    /// Arrange the destinations for the size class at the control, and
    /// announce each one's place and selection.
    fn arrange(&self, s: &mut StructureCx<'_>) {
        let Some(anchor) = self.anchor.get() else {
            return;
        };
        if !s.store.arena().is_live(self.root) {
            s.states.release_anchor(anchor);
            self.anchor.set(None);
            return;
        }
        let class = s.states.env().size_class(anchor).unwrap_or_default();
        let layout = self.style.layouts[match class {
            SizeClass::Compact => 0,
            SizeClass::Medium => 1,
            SizeClass::Expanded => 2,
        }];
        if self.layout.get() != Some(layout) {
            self.lay_out(s.store, layout);
            self.layout.set(Some(layout));
        }
        let selected = match s.states.get(self.selected) {
            Some(StateValue::Int(i)) if i >= 0 => Some(i as usize),
            _ => None,
        };
        let n = self.items.len() as u32;
        for (at, item) in self.items.iter().enumerate() {
            s.store.set_semantic_state(
                item.button,
                SemanticState {
                    selected: Some(selected == Some(at)),
                    position: Some((at as u32 + 1, n)),
                    ..SemanticState::default()
                },
            );
        }
    }

    /// Lay the control out as `layout`: the strip below the content or
    /// before it, along its axis, and each destination's icon above or
    /// beside its caption.
    fn lay_out(&self, store: &mut NodeStore, layout: NavigationLayout) {
        let style = self.style;
        let bar = layout == NavigationLayout::Bar;
        store.set_flex_arrangement(
            self.root,
            (
                if bar { Axis::Column } else { Axis::Row },
                Justify::Start,
                Align::Stretch,
            ),
        );
        store.arena_detach(self.nav);
        store.arena_insert_before(self.root, self.nav, (!bar).then_some(self.content));
        store.mark_dirty(
            self.nav,
            DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT | DirtyClass::SEMANTICS,
        );
        let (axis, size) = match layout {
            NavigationLayout::Bar => (
                Axis::Row,
                Sizing {
                    width: Length::fill(),
                    height: Length::Fit,
                },
            ),
            NavigationLayout::Rail => (
                Axis::Column,
                Sizing {
                    width: Length::Fixed(style.rail_width),
                    height: Length::fill(),
                },
            ),
            NavigationLayout::Sidebar => (
                Axis::Column,
                Sizing {
                    width: Length::Fixed(style.sidebar_width),
                    height: Length::fill(),
                },
            ),
        };
        store.set_flex_arrangement(self.nav, (axis, Justify::Start, Align::Stretch));
        store.set_fixed_size(self.nav, size);
        let beside = layout == NavigationLayout::Sidebar;
        for item in &self.items {
            store.set_flex_arrangement(
                item.button,
                if beside {
                    (Axis::Row, Justify::Start, Align::Center)
                } else {
                    (Axis::Column, Justify::Center, Align::Center)
                },
            );
            store.set_gap(item.button, if beside { 12.0 } else { 4.0 });
        }
    }
}

impl Component for AdaptiveNavigation {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let style = self.style;
        let count = self.destinations.len();
        let initial = (count > 0).then(|| self.selected.min(count - 1));
        let selected = cx.state(StateValue::Int(initial.map_or(-1, |i| i as i32)));
        let fill = FlexStyle {
            axis: Axis::Column,
            gap: 0.0,
            padding: Inset::all(0.0),
            align: Align::Stretch,
            justify: Justify::Start,
            size: Sizing::fill(),
            style: BoxStyle::NONE,
        };
        let mut buttons = Vec::with_capacity(count);
        let mut nav = None;
        let mut content = None;
        let root = cx.flex(
            FlexStyle {
                size: style.size,
                ..fill
            },
            |cx| {
                content = Some(cx.stack(
                    StackStyle {
                        size: Sizing::fill(),
                        ..Default::default()
                    },
                    |cx| {
                        for d in &self.destinations {
                            let page = cx.flex(fill, |cx| (d.page)(cx));
                            cx.semantics(
                                page,
                                Semantics::role(Role::TabPanel).with_label(d.caption.clone()),
                            );
                        }
                    },
                ));
                nav = Some(cx.flex(
                    FlexStyle {
                        axis: Axis::Row,
                        gap: style.gap,
                        padding: style.padding,
                        align: Align::Stretch,
                        justify: Justify::Start,
                        size: Sizing {
                            width: Length::fill(),
                            height: Length::Fit,
                        },
                        style: style.bar,
                    },
                    |cx| {
                        for (i, d) in self.destinations.iter().enumerate() {
                            let button = cx.flex(
                                FlexStyle {
                                    axis: Axis::Column,
                                    gap: 4.0,
                                    padding: style.item_padding,
                                    align: Align::Center,
                                    justify: Justify::Center,
                                    size: Sizing {
                                        width: Length::fill(),
                                        height: Length::Fit,
                                    },
                                    style: if initial == Some(i) {
                                        style.selected
                                    } else {
                                        style.deselected
                                    },
                                },
                                |cx| {
                                    if let Some(icon) = &d.icon {
                                        icon(cx);
                                    }
                                    label(d.caption.clone()).color(style.caption).build(cx);
                                },
                            );
                            buttons.push(button);
                        }
                    },
                ));
            },
        );
        let (nav, content) = (nav.expect("built"), content.expect("built"));
        cx.page(content, initial);
        cx.clip(content, true);
        cx.semantics(root, Semantics::role(Role::Group));
        cx.semantics(nav, Semantics::role(Role::TabList));
        cx.roving(nav);

        let mut items = Vec::with_capacity(count);
        let mut hovers = Vec::with_capacity(count);
        for (i, &button) in buttons.iter().enumerate() {
            let on = cx.state(StateValue::Bool(initial == Some(i)));
            let hover = cx.state(StateValue::Bool(false));
            items.push(Item {
                button: button.id(),
                on,
            });
            hovers.push(hover);
            cx.focusable(button, true);
            cx.semantics(
                button,
                Semantics::role(Role::Tab).with_label(self.destinations[i].caption.clone()),
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
        }
        let parts = Rc::new(Destinations {
            items,
            root: root.id(),
            nav: nav.id(),
            content: content.id(),
            selected,
            anchor: Cell::new(None),
            layout: Cell::new(None),
            style,
            on_change: Rc::clone(&self.on_change),
        });
        for (i, &button) in buttons.iter().enumerate() {
            let hover = hovers[i];
            let d = Rc::clone(&parts);
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
                        d.select(ev, i);
                        if ev.focused() != Some(d.items[i].button) {
                            ev.request_focus(d.items[i].button);
                        }
                    }
                    _ => {}
                }
            });
            let d = Rc::clone(&parts);
            cx.on_key(button, move |ev| d.key(ev, i));
        }
        if let Some(handle) = self.handle {
            cx.bind_control(handle.0, root.id(), Rc::clone(&parts));
        }
        cx.structure(|sx| {
            let anchor = sx.states.anchor_env(root.id(), None);
            let class = sx
                .states
                .anchor_cell(anchor, EnvField::SizeClass)
                .expect("an anchored field");
            parts.anchor.set(Some(anchor));
            parts.arrange(sx);
            let hook = Rc::clone(&parts);
            sx.store
                .add_structure_hook([class, selected], move |sx, _| hook.arrange(sx));
        });
    }
}
