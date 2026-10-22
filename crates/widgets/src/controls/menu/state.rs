//! What an open menu does: which levels show, what each highlights, and how
//! the pointer and the keys move them.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use viso_ui::adaptive::LayoutDirection;
use viso_ui::{
    DispatchPhase, EventCx, ImeEvent, Key, NodeId, PointerButtons, PointerPhase, StateId,
    StateValue, Vec2,
};

use super::{Close, HOVER_GRACE, HOVER_OPEN, Pick, TYPE_AHEAD};

/// How far a release lands from the press that opened the menu before it
/// picks: closer, it is the click that opened it.
const SWEEP_SLOP: f32 = 6.0;

/// The kind a wake's token carries in its top bits.
const HOVER: u32 = 1 << 28;
const TYPED: u32 = 2 << 28;
const TICK: u32 = (1 << 28) - 1;

/// An item's mark and the cell it reads.
pub(super) struct MarkCell {
    pub(super) cell: StateId,
    pub(super) glyph: NodeId,
    pub(super) radio: bool,
}

/// One item of a level.
pub(super) struct Row {
    pub(super) node: NodeId,
    /// The cell its highlighted look reads.
    pub(super) lit: StateId,
    pub(super) enabled: bool,
    /// The level its submenu is.
    pub(super) sub: Option<usize>,
    pub(super) mark: Option<MarkCell>,
    /// Its label, lowercased, which typing matches.
    pub(super) key: String,
    pub(super) pick: Option<Pick>,
    /// Which section of its level it is in.
    pub(super) section: u32,
}

/// One panel: the root menu or a submenu.
pub(super) struct Level {
    pub(super) panel: NodeId,
    pub(super) rows: Vec<Row>,
    /// The level and row that open it.
    pub(super) parent: Option<(usize, usize)>,
    /// Whether it shows, which its parent row announces as expanded.
    pub(super) open: StateId,
}

/// Which way the first highlight goes when the keyboard opens a menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Start {
    First,
    Last,
}

/// What a wake does when it comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    /// Open the submenu of the row the pointer rests on.
    Open(usize, usize),
    /// Highlight the row the pointer crossed on its way to a submenu.
    Switch(usize, usize),
}

/// A menu and its submenus, shared by their handlers.
pub(super) struct MenuState {
    pub(super) levels: Vec<Level>,
    /// The levels showing, the root first.
    shown: RefCell<Vec<usize>>,
    /// Each level's highlighted row.
    lit: RefCell<Vec<Option<usize>>>,
    /// Where the focus was when the menu opened.
    returns: Cell<Option<NodeId>>,
    /// The latest wake armed for the pointer, and what it does.
    tick: Cell<u32>,
    pending: Cell<Option<Pending>>,
    /// The pointer's last two places.
    trail: Cell<[Vec2; 2]>,
    /// Where the press that opened the menu landed, until it lifts.
    sweep: Cell<Option<Vec2>>,
    /// The row a press landed on.
    pressed: Cell<Option<(usize, usize)>>,
    /// The row the pointer is over.
    over: Cell<Option<(usize, usize)>>,
    typed: RefCell<String>,
    typed_tick: Cell<u32>,
    on_close: Option<Close>,
}

impl MenuState {
    pub(super) fn new(levels: Vec<Level>, on_close: Option<Close>) -> Self {
        let n = levels.len();
        MenuState {
            levels,
            shown: RefCell::new(Vec::new()),
            lit: RefCell::new(vec![None; n]),
            returns: Cell::new(None),
            tick: Cell::new(0),
            pending: Cell::new(None),
            trail: Cell::new([Vec2::ZERO; 2]),
            sweep: Cell::new(None),
            pressed: Cell::new(None),
            over: Cell::new(None),
            typed: RefCell::new(String::new()),
            typed_tick: Cell::new(0),
            on_close,
        }
    }

    fn root(&self) -> NodeId {
        self.levels[0].panel
    }

    pub(super) fn is_open(&self) -> bool {
        !self.shown.borrow().is_empty()
    }

    fn showing(&self, level: usize) -> bool {
        self.shown.borrow().contains(&level)
    }

    /// Open the menu, at window point `at` of its anchor if given, the
    /// keyboard's first highlight at `start`, a pointer's press at `sweep`.
    pub(super) fn open(
        &self,
        ev: &mut EventCx<'_>,
        at: Option<Vec2>,
        start: Option<Start>,
        sweep: Option<Vec2>,
    ) {
        if self.is_open() {
            return;
        }
        let root = self.root();
        if let Some(at) = at {
            ev.anchor_at(root, at);
        }
        self.show(ev, 0);
        self.returns.set(ev.focused().or(ev.node()));
        ev.request_focus(root);
        self.sweep.set(sweep);
        if let Some(start) = start {
            self.light(ev, 0, self.end_row(0, start));
        }
    }

    fn show(&self, ev: &mut EventCx<'_>, level: usize) {
        let l = &self.levels[level];
        ev.set_hidden(l.panel, false);
        ev.set(l.open, StateValue::Bool(true));
        self.shown.borrow_mut().push(level);
    }

    /// Close every level, giving the focus back.
    pub(super) fn close(&self, ev: &mut EventCx<'_>) {
        if !self.is_open() {
            return;
        }
        self.close_after(ev, None);
        self.pending.set(None);
        self.sweep.set(None);
        self.pressed.set(None);
        self.over.set(None);
        self.typed.borrow_mut().clear();
        if let Some(back) = self.returns.take() {
            ev.request_focus(back);
        }
        if let Some(close) = &self.on_close {
            (close.borrow_mut())(ev);
        }
    }

    /// Close the levels shown after `level`, or all of them.
    fn close_after(&self, ev: &mut EventCx<'_>, level: Option<usize>) {
        let keep = match level {
            Some(level) => match self.shown.borrow().iter().position(|&l| l == level) {
                Some(at) => at + 1,
                None => return,
            },
            None => 0,
        };
        let gone: Vec<usize> = self.shown.borrow_mut().drain(keep..).collect();
        for l in gone.into_iter().rev() {
            self.light(ev, l, None);
            let level = &self.levels[l];
            ev.set_hidden(level.panel, true);
            ev.set(level.open, StateValue::Bool(false));
        }
    }

    /// The level shown after `level`.
    fn child_of(&self, level: usize) -> Option<usize> {
        let shown = self.shown.borrow();
        let at = shown.iter().position(|&l| l == level)?;
        shown.get(at + 1).copied()
    }

    /// Highlight row `row` of `level`, or none.
    fn light(&self, ev: &mut EventCx<'_>, level: usize, row: Option<usize>) {
        let was = std::mem::replace(&mut self.lit.borrow_mut()[level], row);
        if was == row {
            return;
        }
        let rows = &self.levels[level].rows;
        if let Some(r) = was {
            ev.set(rows[r].lit, StateValue::Bool(false));
        }
        if let Some(r) = row {
            ev.set(rows[r].lit, StateValue::Bool(true));
            ev.scroll_into_view(rows[r].node);
        }
    }

    fn lit(&self, level: usize) -> Option<usize> {
        self.lit.borrow()[level]
    }

    /// The first or last enabled row of `level`.
    fn end_row(&self, level: usize, start: Start) -> Option<usize> {
        let rows = &self.levels[level].rows;
        let enabled = |&r: &usize| rows[r].enabled;
        match start {
            Start::First => (0..rows.len()).find(enabled),
            Start::Last => (0..rows.len()).rev().find(enabled),
        }
    }

    /// The enabled row `by` steps from the highlight of `level`, wrapping.
    fn step(&self, level: usize, by: isize) -> Option<usize> {
        let rows = &self.levels[level].rows;
        let n = rows.len() as isize;
        let Some(from) = self.lit(level) else {
            return self.end_row(level, if by > 0 { Start::First } else { Start::Last });
        };
        (1..=n)
            .map(|k| (from as isize + by * k).rem_euclid(n) as usize)
            .find(|&r| rows[r].enabled)
    }

    /// Open the submenu of row `row` of `level`, its first item highlighted
    /// and the focus in it when the keyboard asks.
    fn open_sub(&self, ev: &mut EventCx<'_>, level: usize, row: usize, keyboard: bool) {
        let Some(sub) = self.levels[level].rows[row].sub else {
            return;
        };
        self.light(ev, level, Some(row));
        if !self.showing(sub) {
            self.close_after(ev, Some(level));
            self.show(ev, sub);
        }
        if keyboard {
            self.light(ev, sub, self.end_row(sub, Start::First));
            ev.request_focus(self.levels[sub].panel);
        }
    }

    /// Pick row `row` of `level`: open its submenu, or turn its mark, close
    /// the menus and run its pick.
    fn pick(&self, ev: &mut EventCx<'_>, level: usize, row: usize, keyboard: bool) {
        let rows = &self.levels[level].rows;
        let r = &rows[row];
        if !r.enabled {
            return;
        }
        if r.sub.is_some() {
            self.open_sub(ev, level, row, keyboard);
            return;
        }
        let checked = match &r.mark {
            Some(mark) if mark.radio => {
                for other in rows.iter().filter(|o| o.section == r.section) {
                    if let Some(m) = other.mark.as_ref().filter(|m| m.radio) {
                        let on = std::ptr::eq(other, r);
                        ev.set(m.cell, StateValue::Bool(on));
                        ev.set_hidden(m.glyph, !on);
                    }
                }
                true
            }
            Some(mark) => {
                let on = !matches!(ev.get(mark.cell), Some(StateValue::Bool(true)));
                ev.set(mark.cell, StateValue::Bool(on));
                ev.set_hidden(mark.glyph, !on);
                on
            }
            None => false,
        };
        let pick = r.pick.clone();
        self.close(ev);
        if let Some(pick) = pick {
            (pick.borrow_mut())(ev, checked);
        }
    }

    /// Arm a wake on the root for the pointer to do `pending`.
    fn arm(&self, ev: &mut EventCx<'_>, pending: Pending, delay: Duration) {
        let tick = (self.tick.get() + 1) & TICK;
        self.tick.set(tick);
        self.pending.set(Some(pending));
        ev.wake_after(self.root(), delay, HOVER | tick);
    }

    /// The pointer moved to `at`.
    fn trace(&self, at: Vec2) {
        let [_, last] = self.trail.get();
        if last != at {
            self.trail.set([last, at]);
        }
    }

    /// Whether the pointer, moving to `at`, heads for the panel of `level`:
    /// it lies in the triangle from where it was to the panel's near edge.
    fn heads_for(&self, ev: &EventCx<'_>, level: usize, at: Vec2) -> bool {
        let Some(panel) = ev.rect_of(self.levels[level].panel) else {
            return false;
        };
        let [before, last] = self.trail.get();
        let from = if last == at { before } else { last };
        let edge = if panel.x >= from.x {
            panel.x
        } else {
            panel.x + panel.w
        };
        inside(
            at,
            from,
            Vec2::new(edge, panel.y),
            Vec2::new(edge, panel.y + panel.h),
        )
    }

    /// A pointer sample over the menus, which all reach the root panel: a
    /// move onto another row enters it, a disabled one too.
    pub(super) fn panel_pointer(&self, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        let at = Vec2::new(p.x, p.y);
        if p.phase == PointerPhase::Move {
            match self.under(ev, at) {
                Some((level, Some(row))) if self.over.get() != Some((level, row)) => {
                    self.over.set(Some((level, row)));
                    self.enter(ev, level, row, at);
                }
                Some((_, Some(_))) => {}
                _ => self.over.set(None),
            }
        }
        self.trace(at);
    }

    /// The level whose panel `at` is over, the deepest shown first, and the
    /// row of it there.
    fn under(&self, ev: &EventCx<'_>, at: Vec2) -> Option<(usize, Option<usize>)> {
        let holds = |id: NodeId| {
            ev.rect_of(id)
                .is_some_and(|r| r.x <= at.x && at.x < r.x + r.w && r.y <= at.y && at.y < r.y + r.h)
        };
        let shown = self.shown.borrow();
        let level = shown
            .iter()
            .rev()
            .copied()
            .find(|&l| holds(self.levels[l].panel))?;
        let row = self.levels[level].rows.iter().position(|r| holds(r.node));
        Some((level, row))
    }

    /// A pointer sample on row `row` of `level`.
    pub(super) fn row_pointer(&self, ev: &mut EventCx<'_>, level: usize, row: usize) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        let at = Vec2::new(p.x, p.y);
        match p.phase {
            PointerPhase::Leave => {
                if self.over.get() == Some((level, row)) {
                    self.over.set(None);
                }
                let open = self.levels[level].rows[row]
                    .sub
                    .is_some_and(|sub| self.showing(sub));
                if !open && self.lit(level) == Some(row) {
                    self.light(ev, level, None);
                }
            }
            PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
                self.pressed.set(Some((level, row)));
                if self.levels[level].rows[row].sub.is_some() {
                    self.open_sub(ev, level, row, false);
                }
            }
            PointerPhase::Up
                if p.buttons.contains(PointerButtons::PRIMARY)
                    || p.buttons.contains(PointerButtons::SECONDARY) =>
            {
                let here = self.pressed.take() == Some((level, row));
                let swept = self
                    .sweep
                    .take()
                    .is_some_and(|o| (o.x - at.x).hypot(o.y - at.y) > SWEEP_SLOP);
                if here || swept {
                    self.pick(ev, level, row, false);
                }
            }
            _ => {}
        }
    }

    fn enter(&self, ev: &mut EventCx<'_>, level: usize, row: usize, at: Vec2) {
        if ev.focused() != Some(self.levels[level].panel) {
            ev.request_focus(self.levels[level].panel);
        }
        // A row of a submenu the pointer reached: it no longer crosses.
        if let Some(Pending::Switch(l, _)) = self.pending.get()
            && l != level
        {
            self.pending.set(None);
        }
        if let Some(child) = self.child_of(level) {
            let from = self.levels[child].parent.map(|(_, r)| r);
            if from == Some(row) {
                self.pending.set(None);
                self.light(ev, level, Some(row));
                return;
            }
            if self.heads_for(ev, child, at) {
                self.arm(ev, Pending::Switch(level, row), HOVER_GRACE);
                self.trace(at);
                return;
            }
            self.close_after(ev, Some(level));
        }
        self.trace(at);
        self.hover(ev, level, row);
    }

    /// The pointer rests on row `row` of `level`.
    fn hover(&self, ev: &mut EventCx<'_>, level: usize, row: usize) {
        let r = &self.levels[level].rows[row];
        self.light(ev, level, r.enabled.then_some(row));
        if r.enabled && r.sub.is_some_and(|sub| !self.showing(sub)) {
            self.arm(ev, Pending::Open(level, row), HOVER_OPEN);
        } else {
            self.pending.set(None);
        }
    }

    /// A key-handler dispatch on the panel of `level`.
    pub(super) fn panel_key(&self, ev: &mut EventCx<'_>, level: usize) {
        if let Some(token) = ev.woken() {
            self.wake(ev, token);
            return;
        }
        if ev.dismissed() {
            self.close(ev);
            return;
        }
        // Keys and text pass through every panel above the focused one: the
        // focused panel answers them.
        if ev.focused() != Some(self.levels[level].panel) {
            return;
        }
        if let Some(ImeEvent::Commit { text }) = ev.ime() {
            let text = text.clone();
            self.type_ahead(ev, level, &text);
            ev.stop_propagation();
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
            Key::Up | Key::Down | Key::Home | Key::End | Key::Enter | Key::Space | Key::Escape
        ) || k.key == forth
            || k.key == back
            || k.key == Key::Tab;
        if !taken {
            return;
        }
        ev.stop_propagation();
        if !k.pressed {
            return;
        }
        match k.key {
            Key::Down => {
                let next = self.step(level, 1);
                self.light(ev, level, next);
            }
            Key::Up => {
                let next = self.step(level, -1);
                self.light(ev, level, next);
            }
            Key::Home => self.light(ev, level, self.end_row(level, Start::First)),
            Key::End => self.light(ev, level, self.end_row(level, Start::Last)),
            Key::Enter | Key::Space if !k.repeat => {
                if let Some(row) = self.lit(level) {
                    self.pick(ev, level, row, true);
                }
            }
            Key::Escape if !k.repeat => self.back(ev, level),
            Key::Tab => self.close(ev),
            key if key == forth => {
                if let Some(row) = self.lit(level)
                    && self.levels[level].rows[row].enabled
                {
                    self.open_sub(ev, level, row, true);
                }
            }
            key if key == back && level != 0 => self.back(ev, level),
            _ => {}
        }
    }

    /// Close `level`, back to the level that opened it, or all of them.
    fn back(&self, ev: &mut EventCx<'_>, level: usize) {
        match self.levels[level].parent {
            Some((parent, _)) => {
                self.close_after(ev, Some(parent));
                ev.request_focus(self.levels[parent].panel);
            }
            None => self.close(ev),
        }
    }

    /// Jump to the next row of `level` whose label starts with what was typed.
    fn type_ahead(&self, ev: &mut EventCx<'_>, level: usize, text: &str) {
        let typed = {
            let mut typed = self.typed.borrow_mut();
            typed.extend(text.chars().flat_map(char::to_lowercase));
            typed.clone()
        };
        let tick = (self.typed_tick.get() + 1) & TICK;
        self.typed_tick.set(tick);
        ev.wake_after(self.root(), TYPE_AHEAD, TYPED | tick);
        let rows = &self.levels[level].rows;
        let n = rows.len();
        // One letter again and again steps through the rows it starts.
        let mut chars = typed.chars();
        let first = chars.next();
        let repeated = first.is_some_and(|c| chars.all(|d| d == c));
        let prefix = if repeated {
            &typed[..first.map_or(0, char::len_utf8)]
        } else {
            &typed[..]
        };
        // A new search, or one letter again, starts past the highlight; a
        // longer prefix may stay on it.
        let fresh = typed.len() == text.len();
        let from = match self.lit(level) {
            Some(r) if repeated || fresh => r + 1,
            Some(r) => r,
            None => 0,
        };
        let hit = (0..n)
            .map(|k| (from + k) % n.max(1))
            .find(|&r| rows[r].enabled && rows[r].key.starts_with(prefix));
        if hit.is_some() {
            self.light(ev, level, hit);
        }
    }

    fn wake(&self, ev: &mut EventCx<'_>, token: u32) {
        let tick = token & TICK;
        match token & !TICK {
            HOVER if tick == self.tick.get() => match self.pending.take() {
                Some(Pending::Open(level, row)) if self.lit(level) == Some(row) => {
                    self.open_sub(ev, level, row, false);
                }
                Some(Pending::Switch(level, row)) if self.showing(level) => {
                    self.close_after(ev, Some(level));
                    self.hover(ev, level, row);
                }
                _ => {}
            },
            TYPED if tick == self.typed_tick.get() => self.typed.borrow_mut().clear(),
            _ => {}
        }
    }

    /// A release of the press that opened the menu on its anchor.
    pub(super) fn anchor_release(&self) {
        self.sweep.set(None);
    }
}

/// Whether `p` lies in the triangle `a`, `b`, `c`.
fn inside(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> bool {
    let cross = |o: Vec2, u: Vec2, v: Vec2| (u.x - o.x) * (v.y - o.y) - (u.y - o.y) * (v.x - o.x);
    let d1 = cross(p, a, b);
    let d2 = cross(p, b, c);
    let d3 = cross(p, c, a);
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

/// A handle an application captures to open and close a built menu from
/// its own handlers. Cheap to clone.
#[derive(Clone)]
pub struct MenuHandle(pub(super) Rc<MenuState>);

impl MenuHandle {
    /// Open the menu against its anchor.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        self.0.open(ev, None, None, None);
    }

    /// Open the menu at window point `at` of its anchor, as a context menu.
    pub fn open_at(&self, ev: &mut EventCx<'_>, at: Vec2) {
        self.0.open(ev, Some(at), None, None);
    }

    /// Close the menu and its submenus.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        self.0.close(ev);
    }

    /// Whether the menu shows.
    pub fn is_open(&self) -> bool {
        self.0.is_open()
    }
}
