//! The menu of edits the window draws where the platform draws none.
//!
//! A text field asks for a menu of edits two ways: a touch selection shows a
//! bar of them beside the selection ([`FieldMarks::menu`]), and a secondary
//! click or the menu key asks for a context menu at a point
//! ([`FieldMarks::context`]). Where the platform draws neither, the window
//! draws them: a dark bar of the edits centred above the selection (below it
//! when there is no room above), and a list of them at the point, flipped to
//! stay inside the window. Both are top-layer nodes mounted under the root the
//! first time one shows and hidden while none does; a panel shows only once
//! it is laid out for what it offers, so it never flashes at a stale place.
//! The bar leaves out the edits that cannot apply and the list dims them. A
//! pick reaches the window through [`DrawnMenu::take_pick`]; the list also
//! answers Up, Down, Home, End, Enter, Space and Escape while it shows.
//!
//! [`FieldMarks::menu`]: viso_ui::FieldMarks::menu
//! [`FieldMarks::context`]: viso_ui::FieldMarks::context

use std::cell::Cell;
use std::rc::Rc;

use viso_platform::{EditLabels, EditMenu};
use viso_runtime::Key;
use viso_ui::{
    AbsoluteStyle, Align, Axis, BindingTable, Border, BoxStyle, BuildCx, Component, FlexStyle,
    Inset, InteractionStyle, Justify, Length, NodeId, NodeStore, PointerButtons, PointerPhase,
    Rect, Rgba, Role, Semantics, Size, StateId, StateStore, StateValue, Vec2,
};
use viso_widgets::label;

/// Which menu of edits shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuLook {
    /// The bar beside a touch selection.
    Bar,
    /// The context menu at a point.
    List,
}

/// An edit a menu offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edit {
    Cut,
    Copy,
    Paste,
    SelectAll,
}

/// The edits in the order a menu offers them.
const EDITS: [Edit; 4] = [Edit::Cut, Edit::Copy, Edit::Paste, Edit::SelectAll];

impl Edit {
    /// Whether `menu` offers it.
    fn offered(self, menu: &EditMenu) -> bool {
        match self {
            Edit::Cut => menu.cut,
            Edit::Copy => menu.copy,
            Edit::Paste => menu.paste,
            Edit::SelectAll => menu.select_all,
        }
    }

    fn title(self, labels: &EditLabels) -> &'static str {
        match self {
            Edit::Cut => labels.cut,
            Edit::Copy => labels.copy,
            Edit::Paste => labels.paste,
            Edit::SelectAll => labels.select_all,
        }
    }
}

/// The room kept between a panel and the window's edges.
const MARGIN: f32 = 8.0;
/// The room kept between the bar and the selection it points at; below the
/// selection, room for the handles' knobs besides.
const GAP: f32 = 8.0;
const BELOW: f32 = 28.0;

const BAR_FILL: Rgba = rgba(0.13, 0.13, 0.14, 0.96);
const BAR_HOVER: Rgba = rgba(1.0, 1.0, 1.0, 0.12);
const BAR_PRESSED: Rgba = rgba(1.0, 1.0, 1.0, 0.22);
const BAR_INK: Rgba = rgba(1.0, 1.0, 1.0, 1.0);
const LIST_FILL: Rgba = rgba(0.98, 0.98, 0.98, 1.0);
const LIST_EDGE: Rgba = rgba(0.0, 0.0, 0.0, 0.16);
const LIST_HOVER: Rgba = rgba(0.23, 0.42, 0.94, 1.0);
const LIST_PRESSED: Rgba = rgba(0.18, 0.34, 0.80, 1.0);
const LIST_INK: Rgba = rgba(0.1, 0.1, 0.1, 1.0);
/// A list item that cannot apply shows at this opacity.
const DIMMED: f32 = 0.4;

const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

/// `h` on the left and right edges, `v` on the top and bottom.
const fn inset(h: f32, v: f32) -> Inset {
    Inset {
        left: h,
        top: v,
        right: h,
        bottom: v,
    }
}

/// One drawn menu: its panel and its items, each with the cell its hover
/// look follows.
struct Panel {
    root: NodeId,
    items: [NodeId; 4],
    hovered: [StateId; 4],
    /// The edits it was last laid out offering.
    offers: Option<[bool; 4]>,
}

/// The drawn menus of one window.
pub(crate) struct DrawnMenu {
    /// The box of no size under the root that the panels hang from.
    anchor: Option<NodeId>,
    bar: Option<Panel>,
    list: Option<Panel>,
    /// What shows, and the menu it shows.
    shown: Option<(MenuLook, EditMenu)>,
    /// The shown panel is laid out for what it offers and placed.
    placed: bool,
    /// The list item the keyboard highlights.
    highlight: Option<usize>,
    /// The edit an item's press picked, until the window takes it.
    picked: Rc<Cell<Option<Edit>>>,
}

/// The shown panel as it is seen: whether it is the list, its box, and each
/// item's box and whether it applies.
pub(crate) type Seen = (bool, Rect, Vec<(Rect, bool)>);

/// What a key did to the shown list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKey {
    /// No list shows, or it has no use for the key.
    Ignored,
    /// The list took it.
    Taken,
    /// The key closes the list.
    Close,
}

impl DrawnMenu {
    pub(crate) fn new() -> Self {
        DrawnMenu {
            anchor: None,
            bar: None,
            list: None,
            shown: None,
            placed: false,
            highlight: None,
            picked: Rc::new(Cell::new(None)),
        }
    }

    /// What shows.
    pub(crate) fn shown(&self) -> Option<MenuLook> {
        self.shown.map(|(look, _)| look)
    }

    /// The edit an item picked since the last call, and the menu it was
    /// picked from.
    pub(crate) fn take_pick(&self) -> Option<(Edit, MenuLook)> {
        let edit = self.picked.take()?;
        Some((edit, self.shown()?))
    }

    /// Whether window point `(x, y)` falls on the shown panel.
    pub(crate) fn holds(&self, store: &NodeStore, x: f32, y: f32) -> bool {
        self.panel().is_some_and(|p| {
            let r = store.world(p.root);
            r.x <= x && x < r.x + r.w && r.y <= y && y < r.y + r.h
        })
    }

    /// The shown panel as it is seen: whether it is the list, its box, and
    /// each item's box and whether it applies, in the order of the edits.
    pub(crate) fn seen(&self, store: &NodeStore) -> Option<Seen> {
        let panel = self.panel().filter(|_| self.placed)?;
        let items = panel
            .items
            .iter()
            .zip(panel.offers?)
            .map(|(&item, offered)| (store.seen(item), offered))
            .collect();
        Some((
            self.shown() == Some(MenuLook::List),
            store.seen(panel.root),
            items,
        ))
    }

    fn panel(&self) -> Option<&Panel> {
        match self.shown()? {
            MenuLook::Bar => self.bar.as_ref(),
            MenuLook::List => self.list.as_ref(),
        }
    }

    /// Show `want` (or nothing) in the window whose tree hangs from
    /// `window_root`,
    /// titled by `labels`: mount the panel the first time, lay it
    /// out for what it offers, then place it. Returns whether it waits for a
    /// layout, so the window draws another frame.
    pub(crate) fn update(
        &mut self,
        store: &mut NodeStore,
        states: &mut StateStore,
        bindings: &mut BindingTable,
        window_root: NodeId,
        want: Option<(MenuLook, EditMenu)>,
        labels: &EditLabels,
    ) -> bool {
        let look = want.map(|(look, _)| look);
        if self.shown() != look {
            if let Some(p) = self.panel() {
                store.set_hidden(p.root, true);
            }
            self.set_highlight(states, None);
            self.shown = None;
            self.placed = false;
        }
        let Some((look, menu)) = want else {
            return false;
        };
        let anchor = *self.anchor.get_or_insert_with(|| {
            let mut cx = BuildCx::with_parent(store, states, bindings, window_root);
            cx.absolute(
                AbsoluteStyle {
                    padding: Inset::default(),
                    size: Size::fixed(0.0, 0.0),
                    style: BoxStyle::NONE,
                },
                |_| {},
            )
            .id()
        });
        let slot = match look {
            MenuLook::Bar => &mut self.bar,
            MenuLook::List => &mut self.list,
        };
        let panel = slot.get_or_insert_with(|| {
            let mut cx = BuildCx::with_parent(store, states, bindings, anchor);
            build_panel(&mut cx, look, labels, &self.picked)
        });
        let offers = EDITS.map(|edit| edit.offered(&menu));
        if panel.offers != Some(offers) {
            for (at, &item) in panel.items.iter().enumerate() {
                match look {
                    MenuLook::Bar => store.set_hidden(item, !offers[at]),
                    MenuLook::List => {
                        store.set_enabled(item, offers[at]);
                        store.set_opacity(item, if offers[at] { 1.0 } else { DIMMED });
                    }
                }
            }
            panel.offers = Some(offers);
            self.placed = false;
        }
        let root = panel.root;
        let moved = self.shown.is_none_or(|(_, was)| was.at != menu.at);
        self.shown = Some((look, menu));
        if !self.placed {
            // Laid out on the frame after it was shown or changed: till
            // then it shows nothing, wherever its box last was.
            let laid_out = !store.hidden(root) && store.world(root).w > 0.0;
            store.set_hidden(root, false);
            if !laid_out {
                store.set_opacity(root, 0.0);
                return true;
            }
            store.set_opacity(root, 1.0);
            self.placed = true;
        } else if !moved {
            return false;
        }
        let origin = store.world(anchor);
        let panel = store.world(root);
        let window = store.world(window_root);
        let at = place(
            look,
            &menu,
            Vec2::new(panel.w, panel.h),
            Vec2::new(window.w, window.h),
        );
        store.set_translate(root, Vec2::new(at.x - origin.x, at.y - origin.y));
        false
    }

    /// Answer a key while the list shows: Up and Down move the highlight
    /// among the items that apply, Home and End to the first and last,
    /// Enter and Space pick the highlighted one, Escape closes it.
    pub(crate) fn key(&mut self, states: &mut StateStore, key: Key, pressed: bool) -> MenuKey {
        let Some((MenuLook::List, menu)) = self.shown else {
            return MenuKey::Ignored;
        };
        let offered: Vec<usize> = (0..EDITS.len())
            .filter(|&at| EDITS[at].offered(&menu))
            .collect();
        let step = |by: isize, from: Option<usize>| {
            let place = from.and_then(|at| offered.iter().position(|&o| o == at));
            let n = offered.len() as isize;
            let next = match place {
                Some(p) => (p as isize + by).rem_euclid(n),
                None if by > 0 => 0,
                None => n - 1,
            };
            offered.get(next as usize).copied()
        };
        if !matches!(
            key,
            Key::Up | Key::Down | Key::Home | Key::End | Key::Enter | Key::Space | Key::Escape
        ) {
            return MenuKey::Ignored;
        }
        if !pressed {
            return MenuKey::Taken;
        }
        let highlight = match key {
            Key::Escape => return MenuKey::Close,
            Key::Enter | Key::Space => {
                if let Some(at) = self.highlight {
                    self.picked.set(Some(EDITS[at]));
                }
                return MenuKey::Taken;
            }
            Key::Up => step(-1, self.highlight),
            Key::Down => step(1, self.highlight),
            Key::Home => offered.first().copied(),
            _ => offered.last().copied(),
        };
        self.set_highlight(states, highlight);
        MenuKey::Taken
    }

    /// Highlight the list's item `at`, or none.
    fn set_highlight(&mut self, states: &mut StateStore, at: Option<usize>) {
        if let Some(list) = &self.list {
            for (i, &cell) in list.hovered.iter().enumerate() {
                if Some(i) == at || Some(i) == self.highlight {
                    states.set(cell, StateValue::Bool(Some(i) == at));
                }
            }
        }
        self.highlight = at;
    }
}

/// Where a panel of `size` showing `menu` sits in a window of `window`.
fn place(look: MenuLook, menu: &EditMenu, size: Vec2, window: Vec2) -> Vec2 {
    let at = Rect {
        x: menu.at.x as f32,
        y: menu.at.y as f32,
        w: menu.at.width as f32,
        h: menu.at.height as f32,
    };
    let keep = |v: f32, extent: f32, room: f32| v.min(room - extent - MARGIN).max(MARGIN);
    match look {
        MenuLook::Bar => {
            let x = at.x + at.w * 0.5 - size.x * 0.5;
            let above = at.y - size.y - GAP;
            let y = if above >= MARGIN {
                above
            } else {
                at.y + at.h + BELOW
            };
            Vec2::new(keep(x, size.x, window.x), keep(y, size.y, window.y))
        }
        MenuLook::List => {
            let x = if at.x + size.x > window.x - MARGIN {
                at.x - size.x
            } else {
                at.x
            };
            let y = if at.y + size.y > window.y - MARGIN {
                at.y - size.y
            } else {
                at.y
            };
            Vec2::new(keep(x, size.x, window.x), keep(y, size.y, window.y))
        }
    }
}

/// Build the panel of `look` titled by `labels`, hidden, its items setting
/// `picked` when pressed.
fn build_panel(
    cx: &mut BuildCx<'_>,
    look: MenuLook,
    labels: &EditLabels,
    picked: &Rc<Cell<Option<Edit>>>,
) -> Panel {
    let (axis, fill, radius, border, padding) = match look {
        MenuLook::Bar => (Axis::Row, BAR_FILL, 8.0, Border::NONE, Inset::all(0.0)),
        MenuLook::List => (
            Axis::Column,
            LIST_FILL,
            6.0,
            Border {
                width: 1.0,
                color: LIST_EDGE,
            },
            Inset::all(4.0),
        ),
    };
    let fit = Size {
        width: Length::Fit,
        height: Length::Fit,
    };
    let mut items = Vec::with_capacity(EDITS.len());
    let mut hovered = Vec::with_capacity(EDITS.len());
    let root = cx.flex(
        FlexStyle {
            axis,
            gap: 0.0,
            padding,
            align: Align::Stretch,
            justify: Justify::Start,
            size: fit,
            style: BoxStyle {
                border,
                ..BoxStyle::solid(fill).with_radius(radius)
            },
        },
        |cx| {
            for edit in EDITS {
                let (item, cell) = build_item(cx, look, edit.title(labels), edit, picked);
                items.push(item);
                hovered.push(cell);
            }
        },
    );
    cx.semantics(root, Semantics::role(Role::Menu));
    cx.set_overlay(root, true);
    cx.set_hidden(root, true);
    Panel {
        root: root.id(),
        items: items.try_into().expect("an item per edit"),
        hovered: hovered.try_into().expect("a cell per edit"),
        offers: None,
    }
}

/// One item of a panel of `look`, titled `title`, picking `edit`.
fn build_item(
    cx: &mut BuildCx<'_>,
    look: MenuLook,
    title: &'static str,
    edit: Edit,
    picked: &Rc<Cell<Option<Edit>>>,
) -> (NodeId, StateId) {
    let (ink, hover, pressed_fill, padding, radius) = match look {
        MenuLook::Bar => (BAR_INK, BAR_HOVER, BAR_PRESSED, inset(12.0, 9.0), 8.0),
        MenuLook::List => (LIST_INK, LIST_HOVER, LIST_PRESSED, inset(16.0, 5.0), 4.0),
    };
    let pressed = cx.state(StateValue::Bool(false));
    let hovered = cx.state(StateValue::Bool(false));
    let item = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 0.0,
            padding,
            align: Align::Center,
            justify: Justify::Start,
            size: Size {
                width: match look {
                    MenuLook::Bar => Length::Fit,
                    MenuLook::List => Length::Fill { weight: 1.0 },
                },
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        },
        |cx| {
            label(title).color(ink).font_size(14.0).build(cx);
        },
    );
    cx.interaction_style(
        item,
        InteractionStyle {
            resting: BoxStyle::NONE,
            hover: BoxStyle::solid(hover).with_radius(radius),
            pressed: BoxStyle::solid(pressed_fill).with_radius(radius),
            pressed_cell: Some(pressed),
            hover_cell: Some(hovered),
        },
    );
    let picked = Rc::clone(picked);
    cx.on_pointer(item, move |ev| {
        let Some(p) = ev.pointer() else { return };
        let primary = p.buttons.contains(PointerButtons::PRIMARY);
        match p.phase {
            PointerPhase::Down if primary => {
                ev.set(pressed, StateValue::Bool(true));
            }
            PointerPhase::Up if ev.get(pressed) == Some(StateValue::Bool(true)) => {
                ev.set(pressed, StateValue::Bool(false));
                picked.set(Some(edit));
            }
            PointerPhase::Enter => {
                ev.set(hovered, StateValue::Bool(true));
            }
            PointerPhase::Leave => {
                ev.set(hovered, StateValue::Bool(false));
                ev.set(pressed, StateValue::Bool(false));
            }
            _ => {}
        }
    });
    cx.semantics(item, Semantics::role(Role::MenuItem).with_label(title));
    (item.id(), hovered)
}
