//! The menu bar: a row of titles, each opening its menu below it.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use viso_ui::adaptive::LayoutDirection;
use viso_ui::{
    Access, Align, Anchoring, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx,
    FlexStyle, FocusChange, ImeEvent, Inset, InteractionStyle, Justify, Key, Length, NodeId,
    PointerButtons, PointerPhase, Rgba, Role, SemanticState, Semantics, Sizing, StateId,
    StateValue, TextRequest, Vec2,
};

use super::state::{MenuHandle, MenuState, Start};
use super::{MenuItem, MenuStyle, build};
use crate::controls::mnemonic::Mnemonic;
use crate::label;

/// How a menu bar looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MenuBarStyle {
    /// The bar's box.
    pub bar: BoxStyle,
    /// The room between the bar's edge and its titles.
    pub padding: Inset,
    /// The room inside a title.
    pub title_padding: Inset,
    /// The room between titles.
    pub gap: f32,
    pub font_size: f32,
    pub ink: Rgba,
    /// A title's box while the pointer is on it, its menu shows or the
    /// keyboard is on it.
    pub highlight: BoxStyle,
    /// The opacity of a disabled title.
    pub disabled: f32,
    /// How its menus look and sit below their titles.
    pub menu: MenuStyle,
}

impl Default for MenuBarStyle {
    fn default() -> Self {
        let menu = MenuStyle {
            // Flush under its title, even at the window's edge.
            placement: Anchoring {
                gap: 2.0,
                margin: 0.0,
                ..Anchoring::default()
            },
            ..MenuStyle::default()
        };
        MenuBarStyle {
            bar: BoxStyle::NONE,
            padding: Inset {
                left: 4.0,
                right: 4.0,
                top: 2.0,
                bottom: 2.0,
            },
            title_padding: Inset {
                left: 8.0,
                right: 8.0,
                top: 4.0,
                bottom: 4.0,
            },
            gap: 0.0,
            font_size: menu.font_size,
            ink: menu.ink,
            highlight: menu.highlight,
            disabled: menu.disabled,
            menu,
        }
    }
}

/// A shared, mutable slot an application passes to a menu bar's `handle`
/// and reads after `build` for its [`MenuBarHandle`].
pub type MenuBarHandleSlot = Rc<RefCell<Option<MenuBarHandle>>>;

/// A row of menus, each under its title. Construct one with [`menu_bar`].
pub struct MenuBar {
    menus: Vec<MenuItem>,
    style: MenuBarStyle,
    slot: Option<MenuBarHandleSlot>,
}

/// A menu bar with no menus yet.
pub fn menu_bar() -> MenuBar {
    MenuBar {
        menus: Vec::new(),
        style: MenuBarStyle::default(),
        slot: None,
    }
}

impl MenuBar {
    /// Add a menu: `menu`'s label is its title and its items the menu's.
    pub fn menu(mut self, menu: MenuItem) -> Self {
        self.menus.push(menu);
        self
    }

    /// Add `menus`.
    pub fn menus(mut self, menus: impl IntoIterator<Item = MenuItem>) -> Self {
        self.menus.extend(menus);
        self
    }

    /// Replace the whole [`MenuBarStyle`].
    pub fn style(mut self, style: MenuBarStyle) -> Self {
        self.style = style;
        self
    }

    /// Fill `slot` with the bar's [`MenuBarHandle`] when it builds.
    pub fn handle(mut self, slot: &MenuBarHandleSlot) -> Self {
        self.slot = Some(Rc::clone(slot));
        self
    }
}

impl Component for MenuBar {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let state = build_bar(cx, self);
        if let Some(slot) = &self.slot {
            *slot.borrow_mut() = Some(MenuBarHandle(state));
        }
    }
}

/// A handle an application captures to work a built menu bar from its own
/// handlers. Cheap to clone.
#[derive(Clone)]
pub struct MenuBarHandle(Rc<BarState>);

impl MenuBarHandle {
    /// Open menu `index`, its first item highlighted, as the keyboard does.
    pub fn open(&self, ev: &mut EventCx<'_>, index: usize) {
        if index < self.0.titles.len() {
            self.0.open(ev, index, Some(Start::First), None, true);
        }
    }

    /// Take the keyboard to the bar, as Alt alone or F10 does.
    pub fn focus(&self, ev: &mut EventCx<'_>) {
        self.0.activate(ev);
    }

    /// Close its menu and give the focus back.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        self.0.leave(ev);
    }

    /// The menu that shows.
    pub fn open_menu(&self) -> Option<usize> {
        self.0.open.get()
    }
}

/// One title of the bar.
struct Title {
    node: NodeId,
    /// The cell its highlighted look reads.
    lit: StateId,
    label: NodeId,
    request: TextRequest,
    mnemonic: Mnemonic,
    enabled: bool,
    menu: Rc<MenuState>,
}

/// A menu bar, shared by its handlers.
pub(super) struct BarState {
    row: NodeId,
    titles: Vec<Title>,
    /// The menu that shows.
    open: Cell<Option<usize>>,
    /// Whether the bar has the keyboard or a menu of it shows.
    active: Cell<bool>,
    /// The title the keyboard is on while no menu shows.
    focus: Cell<Option<usize>>,
    /// The title the pointer is on.
    hovered: Cell<Option<usize>>,
    /// Where the focus was before the bar took it.
    before: Cell<Option<NodeId>>,
    /// Whether the titles show their access keys' cues.
    cues: Cell<bool>,
    /// Whether the keyboard brought the bar up, so its cues show.
    keyboard: Cell<bool>,
}

type Shared = Rc<OnceCell<Rc<BarState>>>;

fn with(shared: &Shared, f: impl FnOnce(&Rc<BarState>)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

const FIT: Sizing = Sizing {
    width: Length::Fit,
    height: Length::Fit,
};

fn build_bar(cx: &mut BuildCx<'_>, bar: &MenuBar) -> Rc<BarState> {
    let style = &bar.style;
    let shared: Shared = Rc::new(OnceCell::new());
    let mut titles = Vec::new();
    let row = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: style.gap,
            padding: style.padding,
            align: Align::Center,
            justify: Justify::Start,
            size: Sizing {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fit,
            },
            style: style.bar,
        },
        |cx| {
            for (index, item) in bar.menus.iter().enumerate() {
                titles.push(title(cx, item, index, style, &shared));
            }
        },
    );
    cx.semantics(row, Semantics::role(Role::MenuBar));
    cx.menu_bar(row);
    let s = Rc::clone(&shared);
    cx.on_key(row, move |ev| with(&s, |state| state.bar_key(ev)));
    let state = Rc::new(BarState {
        row: row.id(),
        titles,
        open: Cell::new(None),
        active: Cell::new(false),
        focus: Cell::new(None),
        hovered: Cell::new(None),
        before: Cell::new(None),
        cues: Cell::new(false),
        keyboard: Cell::new(false),
    });
    for (index, t) in state.titles.iter().enumerate() {
        t.menu.join_bar(&state, index, t.node);
    }
    let _ = shared.set(Rc::clone(&state));
    state
}

/// The title of `item`, and its menu below it.
fn title(
    cx: &mut BuildCx<'_>,
    item: &MenuItem,
    index: usize,
    style: &MenuBarStyle,
    shared: &Shared,
) -> Title {
    let mnemonic = Mnemonic::of(&item.label);
    let lit = cx.state(StateValue::Bool(false));
    let mut shown = None;
    let node = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            padding: style.title_padding,
            align: Align::Center,
            size: FIT,
            ..FlexStyle::default()
        },
        |cx| {
            let text = label(mnemonic.text.clone())
                .font_size(style.font_size)
                .color(style.ink);
            shown = Some((text.build_node(cx).id(), text.request()));
        },
    );
    let (label, request) = shown.expect("the title is built");
    cx.structure(|s| {
        s.store.set_hittable(label, false);
        if !item.enabled {
            s.store.set_enabled(node.id(), false);
            s.store.set_opacity(node.id(), style.disabled);
        }
    });
    cx.interaction_style(
        node,
        InteractionStyle {
            resting: BoxStyle::NONE,
            hover: style.highlight,
            pressed: style.highlight,
            pressed_cell: None,
            hover_cell: Some(lit),
        },
    );
    cx.semantics(
        node,
        Semantics::role(Role::MenuItem).with_label(mnemonic.text.clone()),
    );
    if let Some(key) = mnemonic.char() {
        cx.access_key(node, key);
    }
    let menu = build::bar_menu(
        cx,
        &item.entries,
        node,
        style.menu.placement,
        &style.menu,
        None,
    );
    let MenuHandle(menu) = menu;
    let open = menu.levels[0].open;
    cx.bind_semantic_state(node, move |cx| SemanticState {
        expanded: Some(matches!(cx.get(open), Some(StateValue::Bool(true)))),
        ..SemanticState::default()
    });
    let s = Rc::clone(shared);
    cx.on_pointer(node, move |ev| {
        with(&s, |state| state.title_pointer(ev, index))
    });
    let s = Rc::clone(shared);
    cx.on_key(node, move |ev| with(&s, |state| state.title_key(ev, index)));
    Title {
        node: node.id(),
        lit,
        label,
        request,
        mnemonic,
        enabled: item.enabled,
        menu,
    }
}

impl BarState {
    /// Highlight the titles the pointer, the keyboard and the open menu are
    /// on, and no other.
    fn paint(&self, ev: &mut EventCx<'_>) {
        let open = self.open.get();
        let focus = self.focus.get().filter(|_| self.active.get());
        let hovered = self.hovered.get();
        for (i, t) in self.titles.iter().enumerate() {
            let on = Some(i) == open || Some(i) == focus || Some(i) == hovered;
            if matches!(ev.get(t.lit), Some(StateValue::Bool(was)) if was != on) {
                ev.set(t.lit, StateValue::Bool(on));
            }
        }
    }

    /// Show the titles' access keys underlined, or not.
    fn set_cues(&self, ev: &mut EventCx<'_>, cues: bool) {
        if self.cues.replace(cues) == cues {
            return;
        }
        for t in &self.titles {
            if t.mnemonic.key.is_some() {
                let mut request = t.request.clone();
                request.spans = t.mnemonic.cue(cues);
                ev.set_text(t.label, request);
            }
        }
    }

    /// The bar takes the keyboard or opens a menu: note where the focus was.
    fn enter(&self, ev: &mut EventCx<'_>, keyboard: bool) {
        if !self.active.replace(true) {
            let focused = ev.focused().filter(|&f| !ev.is_within(f, self.row));
            self.before.set(focused);
            self.keyboard.set(false);
        }
        if keyboard {
            self.keyboard.set(true);
        }
        self.set_cues(ev, self.keyboard.get() || ev.access_cues());
    }

    /// Alt alone or F10: the keyboard comes to the first title, or leaves.
    pub(super) fn activate(&self, ev: &mut EventCx<'_>) {
        let Some(first) = self.step(None, true) else {
            return;
        };
        self.enter(ev, true);
        self.focus_title(ev, first);
    }

    /// Put the keyboard on title `index`, no menu showing.
    fn focus_title(&self, ev: &mut EventCx<'_>, index: usize) {
        self.focus.set(Some(index));
        ev.request_focus(self.titles[index].node);
        self.paint(ev);
    }

    /// The enabled title `forth` or back from `from`, wrapping; from none,
    /// the first or the last.
    fn step(&self, from: Option<usize>, forth: bool) -> Option<usize> {
        let n = self.titles.len();
        let enabled = |&i: &usize| self.titles[i].enabled;
        match from {
            None if forth => (0..n).find(enabled),
            None => (0..n).rev().find(enabled),
            Some(from) => (1..=n)
                .map(|k| {
                    if forth {
                        (from + k) % n
                    } else {
                        (from + n - k % n) % n
                    }
                })
                .find(enabled),
        }
    }

    /// Open menu `index`, the keyboard's first highlight at `start`, a
    /// pointer's press at `sweep`, closing the one that showed.
    pub(super) fn open(
        &self,
        ev: &mut EventCx<'_>,
        index: usize,
        start: Option<Start>,
        sweep: Option<Vec2>,
        keyboard: bool,
    ) {
        let t = &self.titles[index];
        if !t.enabled || self.open.get() == Some(index) {
            return;
        }
        self.enter(ev, keyboard);
        if let Some(was) = self.open.get() {
            self.titles[was].menu.close(ev);
        }
        t.menu.open(ev, None, start, sweep);
        self.open.set(Some(index));
        self.focus.set(Some(index));
        self.paint(ev);
    }

    /// Menu `index` closed; the focus goes back to its title.
    pub(super) fn closed(&self, ev: &mut EventCx<'_>, index: usize) {
        if self.open.get() == Some(index) {
            self.open.set(None);
            self.focus.set(Some(index));
            self.paint(ev);
        }
    }

    /// Left or Right in menu `from`: the menu beside it opens.
    pub(super) fn cross(&self, ev: &mut EventCx<'_>, from: usize, forth: bool) {
        if let Some(next) = self.step(Some(from), forth)
            && next != from
        {
            self.open(ev, next, Some(Start::First), None, true);
        }
    }

    /// The bar lets go: its menu closes and the focus goes back to where it
    /// was before.
    pub(super) fn leave(&self, ev: &mut EventCx<'_>) {
        if let Some(open) = self.open.get() {
            self.titles[open].menu.close(ev);
        }
        if !self.active.get() {
            return;
        }
        match self.before.take() {
            Some(back) => ev.request_focus(back),
            None => ev.clear_focus(),
        }
        self.release(ev);
    }

    /// The bar no longer has the keyboard.
    fn release(&self, ev: &mut EventCx<'_>) {
        self.active.set(false);
        self.keyboard.set(false);
        self.focus.set(None);
        self.before.set(None);
        self.set_cues(ev, ev.access_cues());
        self.paint(ev);
    }

    fn title_pointer(&self, ev: &mut EventCx<'_>, index: usize) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        match p.phase {
            PointerPhase::Enter => {
                self.hovered.set(Some(index));
                // With a menu showing, the pointer crossing a title opens its.
                if self.open.get().is_some_and(|open| open != index) {
                    self.open(ev, index, None, None, false);
                }
                self.paint(ev);
            }
            PointerPhase::Leave => {
                if self.hovered.get() == Some(index) {
                    self.hovered.set(None);
                }
                self.paint(ev);
            }
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                if self.open.get() == Some(index) {
                    self.leave(ev);
                } else {
                    self.open(ev, index, None, Some(Vec2::new(p.x, p.y)), false);
                }
            }
            PointerPhase::Up if p.buttons.contains(PointerButtons::PRIMARY) => {
                self.titles[index].menu.anchor_release();
            }
            _ => {}
        }
    }

    /// A key-handler dispatch on title `index`: the keyboard is on it.
    fn title_key(&self, ev: &mut EventCx<'_>, index: usize) {
        if ev.access() == Some(Access::Key) {
            ev.stop_propagation();
            self.open(ev, index, Some(Start::First), None, true);
            return;
        }
        if ev.focused() != Some(self.titles[index].node) {
            return;
        }
        if let Some(ImeEvent::Commit { text }) = ev.ime() {
            // A letter while the keyboard is on the bar opens the menu whose
            // access key it is.
            let mut chars = text.chars();
            if let (Some(c), None) = (chars.next(), chars.next()) {
                let c = c.to_lowercase().next().unwrap_or(c);
                ev.stop_propagation();
                if let Some(hit) = (0..self.titles.len())
                    .find(|&i| self.titles[i].enabled && self.titles[i].mnemonic.char() == Some(c))
                {
                    self.open(ev, hit, Some(Start::First), None, true);
                }
            }
            return;
        }
        let Some(k) = ev.key().copied() else {
            return;
        };
        let rtl = ev.env().environment().layout_direction == LayoutDirection::Rtl;
        let (forth, back) = if rtl {
            (Key::Left, Key::Right)
        } else {
            (Key::Right, Key::Left)
        };
        let taken = matches!(
            k.key,
            Key::Down
                | Key::Up
                | Key::Enter
                | Key::Space
                | Key::Home
                | Key::End
                | Key::Escape
                | Key::Tab
        ) || k.key == forth
            || k.key == back;
        if !taken {
            return;
        }
        ev.stop_propagation();
        if !k.pressed {
            return;
        }
        match k.key {
            Key::Down | Key::Enter | Key::Space if !k.repeat => {
                self.open(ev, index, Some(Start::First), None, true);
            }
            Key::Up if !k.repeat => self.open(ev, index, Some(Start::Last), None, true),
            Key::Home => {
                if let Some(first) = self.step(None, true) {
                    self.focus_title(ev, first);
                }
            }
            Key::End => {
                if let Some(last) = self.step(None, false) {
                    self.focus_title(ev, last);
                }
            }
            Key::Escape | Key::Tab if !k.repeat => self.leave(ev),
            key if key == forth || key == back => {
                if let Some(next) = self.step(Some(index), key == forth) {
                    self.focus_title(ev, next);
                }
            }
            _ => {}
        }
    }

    /// A key-handler dispatch on the bar: Alt alone or F10, the cues, and
    /// the focus leaving it.
    fn bar_key(&self, ev: &mut EventCx<'_>) {
        match ev.access() {
            Some(Access::MenuBar) => {
                if self.active.get() {
                    self.leave(ev);
                } else {
                    self.activate(ev);
                }
                return;
            }
            Some(Access::Cues(cues)) => {
                self.set_cues(ev, cues || self.keyboard.get());
                return;
            }
            _ => {}
        }
        if ev.focus_change() == Some(FocusChange::Lost)
            && self.active.get()
            && self.open.get().is_none()
            && !ev.focused().is_some_and(|f| ev.is_within(f, self.row))
        {
            // The focus went elsewhere: the bar no longer has the keyboard.
            self.release(ev);
        }
    }
}
