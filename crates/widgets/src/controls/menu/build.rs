//! Building a menu: its anchor, its panels and their rows, once.

use std::cell::OnceCell;
use std::rc::Rc;

use viso_ui::{
    Align, AnchorAlign, Anchoring, Axis, BoxStyle, BuildCx, Component, DispatchPhase, FlexStyle,
    Handle, Inset, InteractionStyle, Justify, Key, LeafStyle, Length, NodeId, PointerButtons,
    PointerPhase, Role, SemanticState, Semantics, Side, Sizing, StateValue, Vec2,
};

use super::state::{Level, MarkCell, MenuState, Row, Start};
use super::{Close, Mark, MenuEntry, MenuItem, MenuStyle};
use crate::controls::mnemonic::Mnemonic;
use crate::label;
use viso_ui::glyphs::{check, chevron_end, dot, glyph_slot};

/// The menu state the handlers built before it reach.
type Shared = Rc<OnceCell<Rc<MenuState>>>;

/// Run `f` with the menu state, once it exists.
fn with(shared: &Shared, f: impl FnOnce(&MenuState)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

/// Build a menu hanging off `anchor`: a press on it, or Enter, Space, Up or
/// Down while it has the focus, opens it.
pub(super) fn menu_button(
    cx: &mut BuildCx<'_>,
    entries: &[MenuEntry],
    anchor: Option<&dyn Fn(&mut BuildCx<'_>)>,
    style: &MenuStyle,
    on_close: Option<Close>,
) -> Rc<MenuState> {
    let shared: Shared = Rc::new(OnceCell::new());
    let mut levels = Vec::new();
    let mut button = None;
    cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size: FIT,
            ..FlexStyle::default()
        },
        |cx| {
            let held = cx.flex(
                FlexStyle {
                    size: FIT,
                    ..FlexStyle::default()
                },
                |cx| {
                    if let Some(anchor) = anchor {
                        anchor(cx);
                    }
                },
            );
            level(
                cx,
                entries,
                None,
                held,
                style.placement,
                style,
                &shared,
                &mut levels,
            );
            button = Some(held);
        },
    );
    let button = button.expect("the anchor is built");
    let levels: Vec<Level> = levels.into_iter().map(|l| l.expect("built")).collect();
    let open = levels[0].open;
    cx.focusable(button, true);
    cx.semantics(button, Semantics::role(Role::Button));
    cx.bind_semantic_state(button, move |cx| SemanticState {
        expanded: Some(matches!(cx.get(open), Some(StateValue::Bool(true)))),
        ..SemanticState::default()
    });
    let s = Rc::clone(&shared);
    cx.on_pointer(button, move |ev| {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        if !p.buttons.contains(PointerButtons::PRIMARY) {
            return;
        }
        with(&s, |state| match p.phase {
            PointerPhase::Down if state.is_open() => state.close(ev),
            PointerPhase::Down => state.open(ev, None, None, Some(Vec2::new(p.x, p.y))),
            PointerPhase::Up => state.anchor_release(),
            _ => {}
        });
    });
    let s = Rc::clone(&shared);
    cx.on_key(button, move |ev| {
        let Some(k) = ev.key().copied() else {
            return;
        };
        let start = match k.key {
            Key::Enter | Key::Space | Key::Down => Start::First,
            Key::Up => Start::Last,
            _ => return,
        };
        ev.stop_propagation();
        if k.pressed && !k.repeat {
            with(&s, |state| state.open(ev, None, Some(start), None));
        }
    });
    finish(shared, levels, on_close)
}

/// Build `content` opening a menu at a press that asks for one.
pub(super) fn context_region(
    cx: &mut BuildCx<'_>,
    entries: &[MenuEntry],
    content: Option<&dyn Fn(&mut BuildCx<'_>)>,
    size: Sizing,
    style: &MenuStyle,
    on_close: Option<Close>,
) -> Rc<MenuState> {
    let shared: Shared = Rc::new(OnceCell::new());
    let mut levels = Vec::new();
    let how = Anchoring {
        side: Side::End,
        align: AnchorAlign::Start,
        gap: 0.0,
        ..style.placement
    };
    let held = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size,
            ..FlexStyle::default()
        },
        |cx| {
            // The content, in a box as big as the region the menu opens on.
            let body = cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    align: Align::Stretch,
                    size: Sizing::fill(),
                    ..FlexStyle::default()
                },
                |cx| {
                    if let Some(content) = content {
                        content(cx);
                    }
                },
            );
            level(cx, entries, None, body, how, style, &shared, &mut levels);
        },
    );
    let levels: Vec<Level> = levels.into_iter().map(|l| l.expect("built")).collect();
    cx.long_pressable(held, true);
    let s = Rc::clone(&shared);
    cx.on_pointer(held, move |ev| {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        if let Some(at) = ev.context_menu() {
            ev.stop_propagation();
            let touch = ev.long_press().is_some();
            with(&s, |state| {
                state.close(ev);
                state.open(ev, Some(at), None, (!touch).then_some(at));
            });
        }
    });
    let s = Rc::clone(&shared);
    cx.on_key(held, move |ev| {
        if let Some(at) = ev.context_menu() {
            ev.stop_propagation();
            with(&s, |state| {
                state.open(ev, Some(at), Some(Start::First), None)
            });
        }
    });
    finish(shared, levels, on_close)
}

/// Build the menu of `entries` hanging off `title`, a menu bar's, the way
/// `how` says; the bar opens and closes it.
pub(super) fn bar_menu(
    cx: &mut BuildCx<'_>,
    entries: &[MenuEntry],
    title: Handle,
    how: Anchoring,
    style: &MenuStyle,
    on_close: Option<Close>,
) -> Rc<MenuState> {
    let shared: Shared = Rc::new(OnceCell::new());
    let mut levels = Vec::new();
    level(cx, entries, None, title, how, style, &shared, &mut levels);
    let levels: Vec<Level> = levels.into_iter().map(|l| l.expect("built")).collect();
    finish(shared, levels, on_close)
}

fn finish(shared: Shared, levels: Vec<Level>, on_close: Option<Close>) -> Rc<MenuState> {
    let state = Rc::new(MenuState::new(levels, on_close));
    let _ = shared.set(Rc::clone(&state));
    state
}

const FIT: Sizing = Sizing {
    width: Length::Fit,
    height: Length::Fit,
};

/// Which columns a panel's rows keep room for.
#[derive(Clone, Copy)]
struct Columns {
    mark: bool,
    icon: bool,
}

/// Build the panel of `entries` against `anchor` the way `how` says, its
/// submenus inside it, and note it and them in `levels`; returns its level.
#[allow(clippy::too_many_arguments)]
fn level(
    cx: &mut BuildCx<'_>,
    entries: &[MenuEntry],
    parent: Option<(usize, usize)>,
    anchor: Handle,
    how: Anchoring,
    style: &MenuStyle,
    shared: &Shared,
    levels: &mut Vec<Option<Level>>,
) -> usize {
    let index = levels.len();
    levels.push(None);
    let open = cx.state(StateValue::Bool(false));
    let items = || {
        entries.iter().filter_map(|e| match e {
            MenuEntry::Item(item) => Some(item),
            MenuEntry::Separator => None,
        })
    };
    let columns = Columns {
        mark: items().any(|i| i.mark.is_some()),
        icon: items().any(|i| i.icon.is_some()),
    };
    let mut rows: Vec<Row> = Vec::new();
    let inner = style.padding.left + style.padding.right;
    let panel = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            gap: 0.0,
            padding: style.padding,
            align: Align::Stretch,
            justify: Justify::Start,
            size: FIT,
            style: style.panel,
        },
        |cx| {
            cx.leaf(LeafStyle {
                size: Sizing::fixed((style.min_width - inner).max(0.0), 0.0),
                style: BoxStyle::NONE,
            });
            let mut section = 0;
            let mut subs: Vec<(usize, Handle, &MenuItem)> = Vec::new();
            for entry in entries {
                match entry {
                    MenuEntry::Separator => {
                        rule(cx, style);
                        section += 1;
                    }
                    MenuEntry::Item(item) => {
                        let at = rows.len();
                        let (row, node) = row(cx, item, style, columns, section);
                        let s = Rc::clone(shared);
                        cx.on_pointer(node, move |ev| {
                            with(&s, |state| state.row_pointer(ev, index, at));
                        });
                        rows.push(row);
                        if !item.entries.is_empty() {
                            subs.push((at, node, item));
                        }
                    }
                }
            }
            let beside = Anchoring {
                side: Side::End,
                align: AnchorAlign::Start,
                gap: style.padding.right - 1.0,
                offset: -style.padding.top,
                ..Anchoring::default()
            };
            for (at, node, item) in subs {
                let sub = level(
                    cx,
                    &item.entries,
                    Some((index, at)),
                    node,
                    beside,
                    style,
                    shared,
                    levels,
                );
                rows[at].sub = Some(sub);
                let sub_open = match &levels[sub] {
                    Some(l) => l.open,
                    None => unreachable!("a level is noted when built"),
                };
                cx.bind_semantic_state(node, move |cx| SemanticState {
                    expanded: Some(matches!(cx.get(sub_open), Some(StateValue::Bool(true)))),
                    ..SemanticState::default()
                });
            }
        },
    );
    cx.anchored(panel, anchor, how);
    if let Some(shadow) = style.shadow {
        cx.shadow(panel, shadow);
    }
    if parent.is_none() {
        cx.light_dismiss(panel);
    }
    cx.set_hidden(panel, true);
    cx.focusable(panel, true);
    cx.semantics(panel, Semantics::role(Role::Menu));
    let s = Rc::clone(shared);
    cx.on_key(panel, move |ev| {
        with(&s, |state| state.panel_key(ev, index))
    });
    if parent.is_none() {
        let s = Rc::clone(shared);
        cx.on_pointer(panel, move |ev| with(&s, |state| state.panel_pointer(ev)));
    }
    levels[index] = Some(Level {
        panel: panel.id(),
        rows,
        parent,
        open,
    });
    index
}

/// A separator: a line across the panel with room above and below.
fn rule(cx: &mut BuildCx<'_>, style: &MenuStyle) {
    let line = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            padding: Inset {
                left: 0.0,
                right: 0.0,
                top: 4.0,
                bottom: 4.0,
            },
            align: Align::Stretch,
            size: Sizing {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fit,
            },
            ..FlexStyle::default()
        },
        |cx| {
            cx.leaf(LeafStyle {
                size: Sizing {
                    width: Length::Fill { weight: 1.0 },
                    height: Length::Fixed(1.0),
                },
                style: BoxStyle::solid(style.rule),
            });
        },
    );
    cx.semantics(line, Semantics::role(Role::Separator));
}

/// The row of `item`, in section `section` of its level.
fn row(
    cx: &mut BuildCx<'_>,
    item: &MenuItem,
    style: &MenuStyle,
    columns: Columns,
    section: u32,
) -> (Row, Handle) {
    let lit = cx.state(StateValue::Bool(false));
    let mut mark = None;
    let mnemonic = Mnemonic::of(&item.label);
    let mut shown = None;
    let node = cx.flex(
        FlexStyle {
            axis: Axis::Row,
            gap: 8.0,
            padding: style.item_padding,
            align: Align::Center,
            justify: Justify::Start,
            size: Sizing {
                width: Length::Fill { weight: 1.0 },
                height: Length::Fit,
            },
            style: BoxStyle::NONE,
        },
        |cx| {
            if columns.mark {
                glyph_slot(cx, |cx| {
                    if let Some(m) = item.mark {
                        let (on, radio) = match m {
                            Mark::Check(on) => (on, false),
                            Mark::Radio(on) => (on, true),
                        };
                        let glyph = if radio {
                            dot(cx, style.ink)
                        } else {
                            check(cx, style.ink)
                        };
                        cx.set_hidden(glyph, !on);
                        let cell = cx.state(StateValue::Bool(on));
                        mark = Some(MarkCell {
                            cell,
                            glyph: glyph.id(),
                            radio,
                        });
                    }
                });
            }
            if columns.icon {
                glyph_slot(cx, |cx| {
                    if let Some(icon) = &item.icon {
                        icon.build(cx);
                    }
                });
            }
            let text = label(mnemonic.text.clone())
                .font_size(style.font_size)
                .color(style.ink)
                .size(Sizing {
                    width: Length::Fill { weight: 1.0 },
                    height: Length::Fit,
                });
            shown = Some((text.build_node(cx).id(), text.request()));
            if let Some(shortcut) = &item.shortcut {
                label(shortcut.clone())
                    .font_size(style.font_size)
                    .color(style.quiet)
                    .build_node(cx);
            }
            if !item.entries.is_empty() {
                chevron_end(cx, style.quiet);
            }
        },
    );
    // The row takes the pointer, not what it shows.
    cx.structure(|s| {
        let mut stack: Vec<NodeId> = s.store.arena().children(node.id()).collect();
        while let Some(id) = stack.pop() {
            s.store.set_hittable(id, false);
            stack.extend(s.store.arena().children(id));
        }
        // A disabled row takes no input; the pointer over it reaches the
        // panel, which notes it is there.
        if !item.enabled {
            s.store.set_enabled(node.id(), false);
            s.store.set_hittable(node.id(), false);
            s.store.set_opacity(node.id(), style.disabled);
        }
    });
    cx.focusable(node, true);
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
    let role = match item.mark {
        Some(Mark::Check(_)) => Role::MenuItemCheckBox,
        Some(Mark::Radio(_)) => Role::MenuItemRadio,
        None => Role::MenuItem,
    };
    cx.semantics(
        node,
        Semantics::role(role).with_label(mnemonic.text.clone()),
    );
    if let Some(m) = &mark {
        let cell = m.cell;
        cx.bind_semantic_state(node, move |cx| {
            SemanticState::checked(matches!(cx.get(cell), Some(StateValue::Bool(true))))
        });
    }
    let (label, request) = shown.expect("the label is built");
    let row = Row {
        node: node.id(),
        lit,
        enabled: item.enabled,
        sub: None,
        mark,
        key: mnemonic.text.to_lowercase(),
        label,
        request,
        mnemonic,
        pick: item.pick.clone(),
        section,
    };
    (row, node)
}
