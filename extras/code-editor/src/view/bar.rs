//! The find bar: a field the search is typed in, its options, a count of
//! what it found, and a replace row under it.
//!
//! The bar is built once with the editor, hidden, and shown over the text's
//! top when a search opens. What is typed in its field searches as it is
//! typed, the primary cursor going to the first match at or after it; Enter
//! goes to the next, Shift+Enter to the one before; Escape closes it and
//! gives the text the focus back. Enter in the replace field replaces the
//! match the cursor is on and goes to the next; the accelerator with Alt and
//! Enter replaces them all, as one undo step.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_ui::text_edit::EditIntent;
use viso_ui::{
    Align, Axis, Border, BoxStyle, BuildCx, EventCx, FieldLook, FlexStyle, FontSize, Inset, Key,
    LeafStyle, Length, LiveRegion, NodeId, PointerPhase, Rgba, Role, Semantics, Size, StructureCx,
    TextRequest, TextStyle, field_input,
};

use super::input::{changed, run};
use super::{Shared, text_request};
use crate::keymap::{Command, command_for};
use crate::search::Query;
use crate::session::Session;

/// Whether the bar shows, and how much of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Closed,
    Find,
    Replace,
}

/// One of the search's options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Opt {
    Case,
    Word,
    Regex,
}

/// The find bar's nodes and what it holds.
pub(crate) struct Bar {
    root: NodeId,
    pub find: NodeId,
    replace_row: NodeId,
    pub replace: NodeId,
    pub count: NodeId,
    options: [(Opt, NodeId); 3],
    mode: Cell<Mode>,
    /// What the search looks for, its options with it.
    query: RefCell<Query>,
    /// What replaces a match.
    with: RefCell<String>,
    /// What the count and the options show now.
    shown: RefCell<(String, Option<[bool; 3]>)>,
}

impl Bar {
    /// Whether it shows, and whether with its replace row.
    pub fn shown(&self) -> Option<bool> {
        match self.mode.get() {
            Mode::Closed => None,
            Mode::Find => Some(false),
            Mode::Replace => Some(true),
        }
    }

    /// What its count says.
    pub fn count_text(&self) -> String {
        self.shown.borrow().0.clone()
    }

    /// Shows what the search found and which options are on.
    pub fn update(&self, shared: &Shared, session: &Session, s: &mut StructureCx<'_>) {
        if self.mode.get() == Mode::Closed {
            return;
        }
        let count = match session.search() {
            None => String::new(),
            Some(search) if search.error.is_some() => "Invalid pattern".into(),
            Some(search) if search.query.text.is_empty() => String::new(),
            Some(search) if search.matches().is_empty() => "No results".into(),
            Some(search) => match session.current_match() {
                Some(i) => format!("{} of {}", i + 1, search.matches().len()),
                None => format!("{} found", search.matches().len()),
            },
        };
        let q = self.query.borrow();
        let on = [q.case, q.word, q.regex];
        let mut shown = self.shown.borrow_mut();
        if shown.0 != count {
            let text = if count.is_empty() { " " } else { &count };
            s.store.set_text(
                self.count,
                text_request(
                    shared,
                    text.to_owned(),
                    shared.theme.line_number,
                    Vec::new(),
                ),
            );
            s.store.set_semantics(
                self.count,
                Semantics::role(Role::Label)
                    .with_label(count.clone())
                    .with_live(LiveRegion::Polite),
            );
            shown.0 = count;
        }
        if shown.1 != Some(on) {
            for (k, (opt, node)) in self.options.iter().enumerate() {
                s.store.set_fill(*node, option_fill(shared, on[k]));
                s.store.set_semantics(
                    *node,
                    Semantics::role(Role::Button)
                        .with_label(option_label(*opt))
                        .with_value(if on[k] { "on" } else { "off" }),
                );
            }
            shown.1 = Some(on);
        }
    }
}

fn option_fill(shared: &Shared, on: bool) -> Rgba {
    if on {
        shared.theme.popup_selected
    } else {
        Rgba::TRANSPARENT
    }
}

fn option_label(opt: Opt) -> &'static str {
    match opt {
        Opt::Case => "Match case",
        Opt::Word => "Match whole word",
        Opt::Regex => "Use regular expression",
    }
}

/// The query the bar holds: what the editor's next search looks for, its
/// options with it.
pub(crate) fn query_of(shared: &Shared) -> Query {
    shared
        .bar
        .borrow()
        .as_ref()
        .map_or_else(Query::default, |b| b.query.borrow().clone())
}

/// Builds the bar, hidden, under `cx`.
pub(crate) fn build(shared: &Rc<Shared>, cx: &mut BuildCx<'_>) -> Bar {
    let theme = &shared.theme;
    let m = shared.metrics.get();
    let lh = m.line_height;
    let row_style = FlexStyle {
        axis: Axis::Row,
        gap: 4.0,
        align: Align::Center,
        size: Size {
            width: Length::fill(),
            height: Length::Fit,
        },
        ..FlexStyle::default()
    };
    let mut find = None;
    let mut replace = None;
    let mut replace_row = None;
    let mut count = None;
    let mut options = Vec::new();
    let root = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            gap: 4.0,
            padding: Inset::all(4.0),
            size: Size {
                width: Length::fill(),
                height: Length::Fit,
            },
            style: BoxStyle {
                fill: theme.popup,
                border: Border {
                    width: 1.0,
                    color: theme.popup_border,
                },
                ..BoxStyle::NONE
            },
            ..FlexStyle::default()
        },
        |cx| {
            cx.flex(row_style, |cx| {
                find = Some(field(shared, cx, "Find", false));
                for (opt, label) in [(Opt::Case, "Aa"), (Opt::Word, "W"), (Opt::Regex, ".*")] {
                    let sh = Rc::clone(shared);
                    let node = button(shared, cx, label, option_label(opt), move |ev| {
                        toggle(&sh, ev, opt);
                    });
                    options.push((opt, node));
                }
                let leaf = cx.leaf(LeafStyle {
                    size: Size {
                        width: Length::MaxContent,
                        height: Length::Fixed(lh),
                    },
                    style: BoxStyle::NONE,
                });
                cx.text_request(
                    leaf,
                    text_request(shared, " ".into(), theme.line_number, Vec::new()),
                );
                count = Some(leaf.id());
                let sh = Rc::clone(shared);
                button(shared, cx, "\u{2191}", "Previous match", move |ev| {
                    step(&sh, ev, true);
                });
                let sh = Rc::clone(shared);
                button(shared, cx, "\u{2193}", "Next match", move |ev| {
                    step(&sh, ev, false);
                });
                let sh = Rc::clone(shared);
                button(shared, cx, "\u{d7}", "Close", move |ev| {
                    close(&sh, ev);
                });
            });
            let row = cx.flex(row_style, |cx| {
                replace = Some(field(shared, cx, "Replace", true));
                let sh = Rc::clone(shared);
                button(shared, cx, "Replace", "Replace", move |ev| {
                    replace_one(&sh, ev);
                });
                let sh = Rc::clone(shared);
                button(shared, cx, "All", "Replace all", move |ev| {
                    replace_all(&sh, ev);
                });
            });
            replace_row = Some(row.id());
        },
    );
    cx.set_hidden(root, true);
    cx.semantics(root, Semantics::role(Role::Group).with_label("Find"));
    let options: [(Opt, NodeId); 3] = [options[0], options[1], options[2]];
    Bar {
        root: root.id(),
        find: find.expect("built"),
        replace_row: replace_row.expect("built"),
        replace: replace.expect("built"),
        count: count.expect("built"),
        options,
        mode: Cell::new(Mode::Closed),
        query: RefCell::new(Query::default()),
        with: RefCell::new(String::new()),
        shown: RefCell::new((String::new(), None)),
    }
}

/// A one-line field in the bar: the find field, or with `replacing` the
/// replace field.
fn field(shared: &Rc<Shared>, cx: &mut BuildCx<'_>, label: &str, replacing: bool) -> NodeId {
    let m = shared.metrics.get();
    let theme = &shared.theme;
    let leaf = cx.text_input(
        LeafStyle {
            size: Size {
                width: Length::fill(),
                height: Length::Fixed(m.line_height),
            },
            style: BoxStyle {
                fill: theme.background,
                radius: 3.0,
                ..BoxStyle::NONE
            },
        },
        TextRequest {
            text: String::new(),
            font_size: FontSize::Fixed(m.font_size),
            color: theme.text,
            soft_wrap: false,
            locale: None,
            style: TextStyle {
                family: Some(shared.family.clone()),
                ..TextStyle::default()
            },
            spans: Vec::new(),
        },
        FieldLook {
            placeholder: label.to_owned(),
            ..FieldLook::default()
        },
    );
    cx.focusable(leaf, true);
    cx.cursor(leaf, viso_ui::CursorIcon::Text);
    cx.semantics(
        leaf,
        Semantics::role(if replacing {
            Role::TextField
        } else {
            Role::SearchField
        })
        .with_label(label.to_owned()),
    );
    cx.on_pointer(leaf, |ev| {
        field_input(ev, false);
        ev.stop_propagation();
    });
    let sh = Rc::clone(shared);
    cx.on_key(leaf, move |ev| field_key(&sh, ev, replacing));
    leaf.id()
}

/// A field's keys: its text edits the search or the replacement; Enter
/// steps or replaces; Escape closes the bar; the editor's own find keys
/// work from it.
fn field_key(shared: &Rc<Shared>, ev: &mut EventCx<'_>, replacing: bool) {
    if let Some(text) = ev.text_change() {
        let text = text.to_owned();
        if replacing {
            if let Some(bar) = shared.bar.borrow().as_ref() {
                *bar.with.borrow_mut() = text;
            }
        } else {
            set_query(shared, ev, |q| q.text = text);
        }
        return;
    }
    if let Some(k) = ev.key().copied()
        && k.pressed
    {
        let apple = viso_ui::primary_is_logo();
        let m = k.modifiers;
        let primary = if apple { m.logo } else { m.control };
        match k.key {
            Key::Enter if replacing && primary && m.alt => replace_all(shared, ev),
            Key::Enter if replacing && !primary && !m.alt => replace_one(shared, ev),
            Key::Enter if !primary && !m.alt => step(shared, ev, m.shift),
            Key::Escape => {
                close(shared, ev);
            }
            // The options' keys, as code editors have them.
            Key::C if m.alt && !primary => toggle(shared, ev, Opt::Case),
            Key::W if m.alt && !primary => toggle(shared, ev, Opt::Word),
            Key::R if m.alt && !primary => toggle(shared, ev, Opt::Regex),
            _ => match command_for(&k, apple) {
                Some(c @ (Command::Find { .. } | Command::FindNext { .. })) => {
                    run(shared, ev, c);
                }
                _ => {
                    field_input(ev, false);
                    return;
                }
            },
        }
        ev.stop_propagation();
        return;
    }
    field_input(ev, false);
}

/// Changes the query by `f` and searches for it, the cursor going to the
/// first match at or after it.
fn set_query(shared: &Rc<Shared>, ev: &mut EventCx<'_>, f: impl FnOnce(&mut Query)) {
    let query = {
        let bar = shared.bar.borrow();
        let Some(bar) = bar.as_ref() else {
            return;
        };
        let mut q = bar.query.borrow_mut();
        f(&mut q);
        q.clone()
    };
    {
        let mut s = shared.session.borrow_mut();
        s.find(query);
        s.find_from_cursor();
    }
    changed(shared, ev);
}

fn toggle(shared: &Rc<Shared>, ev: &mut EventCx<'_>, opt: Opt) {
    set_query(shared, ev, |q| match opt {
        Opt::Case => q.case = !q.case,
        Opt::Word => q.word = !q.word,
        Opt::Regex => q.regex = !q.regex,
    });
}

fn step(shared: &Rc<Shared>, ev: &mut EventCx<'_>, back: bool) {
    shared.session.borrow_mut().find_next(back);
    changed(shared, ev);
}

fn replace_one(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    let with = with_of(shared);
    shared.session.borrow_mut().replace_current(&with);
    changed(shared, ev);
}

fn replace_all(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    let with = with_of(shared);
    shared.session.borrow_mut().replace_all(&with);
    changed(shared, ev);
}

fn with_of(shared: &Shared) -> String {
    shared
        .bar
        .borrow()
        .as_ref()
        .map_or_else(String::new, |b| b.with.borrow().clone())
}

/// A small button in the bar saying `text`, named `label`, running `act`
/// when clicked.
fn button(
    shared: &Rc<Shared>,
    cx: &mut BuildCx<'_>,
    text: &str,
    label: &str,
    act: impl Fn(&mut EventCx<'_>) + 'static,
) -> NodeId {
    let lh = shared.metrics.get().line_height;
    let node = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            padding: Inset {
                left: 6.0,
                right: 6.0,
                top: 0.0,
                bottom: 0.0,
            },
            size: Size {
                width: Length::Fit,
                height: Length::Fit,
            },
            style: BoxStyle::NONE.with_radius(3.0),
            ..FlexStyle::default()
        },
        |cx| {
            let leaf = cx.leaf(LeafStyle {
                size: Size {
                    width: Length::MaxContent,
                    height: Length::Fixed(lh),
                },
                style: BoxStyle::NONE,
            });
            cx.text_request(
                leaf,
                text_request(shared, text.to_owned(), shared.theme.text, Vec::new()),
            );
            cx.semantics(leaf, Semantics::role(Role::Label).with_hidden(true));
        },
    );
    cx.cursor(node, viso_ui::CursorIcon::Pointer);
    cx.semantics(
        node,
        Semantics::role(Role::Button).with_label(label.to_owned()),
    );
    cx.on_pointer(node, move |ev| {
        if let Some(p) = ev.pointer()
            && p.phase == PointerPhase::Up
        {
            act(ev);
        }
        ev.stop_propagation();
    });
    node.id()
}

/// Opens the bar, its replace row with `replace`, looking for the selection
/// or the word at the caret, and puts the focus in its field.
pub(crate) fn open(shared: &Rc<Shared>, ev: &mut EventCx<'_>, replace: bool) {
    let guard = shared.bar.borrow();
    let Some(bar) = guard.as_ref() else {
        return;
    };
    let seed = shared.session.borrow().seed_query();
    let query = {
        let mut q = bar.query.borrow_mut();
        if let Some(seed) = seed {
            q.text = seed;
        }
        q.clone()
    };
    bar.mode
        .set(if replace { Mode::Replace } else { Mode::Find });
    ev.set_hidden(bar.root, false);
    ev.set_hidden(bar.replace_row, !replace);
    // The field shows the query, selected to type over.
    ev.edit(bar.find, EditIntent::SelectAll);
    if !query.text.is_empty() {
        ev.edit(bar.find, EditIntent::Insert(query.text.clone()));
        ev.edit(bar.find, EditIntent::SelectAll);
    }
    ev.request_focus(bar.find);
    drop(guard);
    {
        let mut s = shared.session.borrow_mut();
        s.find(query);
        s.find_from_cursor();
    }
    changed(shared, ev);
}

/// Closes the bar and its search, giving the text the focus back; returns
/// whether it was open.
pub(crate) fn close(shared: &Rc<Shared>, ev: &mut EventCx<'_>) -> bool {
    let guard = shared.bar.borrow();
    let Some(bar) = guard.as_ref() else {
        return false;
    };
    if bar.mode.get() == Mode::Closed {
        return false;
    }
    bar.mode.set(Mode::Closed);
    ev.set_hidden(bar.root, true);
    drop(guard);
    shared.session.borrow_mut().close_search();
    if let Some(nodes) = shared.nodes.get() {
        ev.request_focus(nodes.root);
    }
    changed(shared, ev);
    true
}
