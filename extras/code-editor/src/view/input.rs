//! The editor's input: keys, text and the pointer, run as commands on its
//! session.

use std::rc::Rc;

use viso_ui::{
    DispatchPhase, EventCx, ImeEvent, PointerButtons, PointerPhase, Rect, ScrollBehavior,
    ScrollRequest, Vec2,
};

use super::{BLINK, Drag, Shared, assist, bar};
use crate::keymap::{Command, command_for};
use crate::search::Query;
use crate::selection::Selection;
use crate::session::Session;

/// The editor's key handler: keys, text, the focus, the caret's blink.
pub(super) fn on_key(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    // Keys reach the editor on their way to a field in it too: the editor
    // takes only its own.
    let own = ev.node().is_some() && ev.focused() == ev.node();
    if ev.focus_change().is_some() {
        let gained = own;
        if gained == shared.focused.get() {
            return;
        }
        shared.focused.set(gained);
        shared.blink_on.set(true);
        if gained {
            arm_blink(shared, ev);
        } else {
            assist::close_popups(shared);
        }
        {
            let mut s = shared.session.borrow_mut();
            s.pending_cursors();
        }
        shared.bump(ev);
        return;
    }
    if let Some(token) = ev.woken() {
        if token & assist::HOVER_WAKE != 0 {
            assist::hover_woken(shared, ev, token);
            return;
        }
        if token == shared.blink_token.get() && shared.focused.get() {
            shared.blink_on.set(!shared.blink_on.get());
            shared.session.borrow_mut().pending_cursors();
            shared.bump(ev);
            arm_blink(shared, ev);
        }
        return;
    }
    if !own {
        return;
    }
    if let Some(ime) = ev.ime() {
        let ime = ime.clone();
        let mut typed = None;
        {
            let mut s = shared.session.borrow_mut();
            match ime {
                ImeEvent::Preedit { text, caret, .. } => s.set_preedit(&text, caret),
                ImeEvent::Commit { text } => {
                    s.set_preedit("", 0);
                    // Keys that are commands come as keys.
                    if !text.chars().all(char::is_control) {
                        s.type_text(&text);
                        let mut chars = text.chars();
                        typed = chars.next().filter(|_| chars.next().is_none());
                    }
                }
                ImeEvent::Paste { text } => s.paste(&text),
                // The editor shows the input method no surrounding text, so
                // it names no range of it to edit.
                ImeEvent::Edit(_) => {}
            }
        }
        ev.stop_propagation();
        assist::close_hover(shared);
        changed(shared, ev);
        // A word typed on, or a character the service asks on, asks for
        // completions.
        if let Some(c) = typed {
            assist::complete(shared, ev, Some(c));
        }
        return;
    }
    if let Some(data) = ev.pasted() {
        if let Some(text) = data.text.clone() {
            shared.session.borrow_mut().paste(&text);
            changed(shared, ev);
        }
        return;
    }
    let Some(key) = ev.key().copied() else {
        return;
    };
    if !key.pressed {
        return;
    }
    // A composition takes its keys.
    if shared.session.borrow().preedit().is_some() {
        ev.stop_propagation();
        return;
    }
    // An open completion list takes the keys that move in it.
    if shared.assist.completing() && assist::completion_key(shared, ev, &key) {
        ev.stop_propagation();
        return;
    }
    if assist::close_hover(shared) {
        shared.bump(ev);
    }
    let Some(command) = command_for(&key, viso_ui::primary_is_logo()) else {
        return;
    };
    if run(shared, ev, command) {
        ev.stop_propagation();
    }
}

/// Runs `command`; returns whether it was the editor's to run.
pub(super) fn run(shared: &Rc<Shared>, ev: &mut EventCx<'_>, command: Command) -> bool {
    match command {
        Command::Find { replace } => {
            bar::open(shared, ev, replace);
            return true;
        }
        Command::Escape if assist::close_popups(shared) => {
            shared.bump(ev);
            return true;
        }
        Command::Escape
            if shared.session.borrow().selections().len() == 1
                && shared.session.borrow().selections().primary().is_empty() =>
        {
            return bar::close(shared, ev);
        }
        Command::Complete => {
            assist::complete(shared, ev, None);
            return true;
        }
        Command::Hover => {
            assist::hover_at_caret(shared, ev);
            return true;
        }
        Command::GoToDefinition => {
            let at = shared.session.borrow().selections().primary().head;
            assist::go_to_definition(shared, ev, at);
            return true;
        }
        _ => {}
    }
    let mut s = shared.session.borrow_mut();
    let taken = match command {
        Command::Move { motion, extend } => {
            s.move_cursors(motion, extend);
            true
        }
        Command::Backspace(reach) => {
            s.backspace(reach);
            true
        }
        Command::Delete(reach) => {
            s.delete(reach);
            true
        }
        Command::Newline => {
            s.newline();
            true
        }
        Command::OpenLine { above } => {
            s.open_line(above);
            true
        }
        Command::Tab => {
            s.tab();
            true
        }
        Command::Outdent => {
            s.outdent_lines();
            true
        }
        Command::IndentLines => {
            s.indent_lines();
            true
        }
        Command::ToggleComment => s.toggle_comment(),
        Command::MoveLines(dir) => {
            s.move_lines(dir);
            true
        }
        Command::DuplicateLines { above } => {
            s.duplicate_lines(above);
            true
        }
        Command::DeleteLines => {
            s.delete_lines();
            true
        }
        Command::SelectAll => {
            s.select_all();
            true
        }
        Command::SelectLines => {
            s.select_lines();
            true
        }
        Command::AddNextMatch => {
            s.add_next_match();
            true
        }
        Command::SelectAllMatches => s.select_all_matches(),
        Command::AddCursor(dir) => {
            s.add_cursor_vertically(dir);
            true
        }
        Command::Escape => s.collapse(),
        Command::Undo => {
            s.undo();
            true
        }
        Command::Redo => {
            s.redo();
            true
        }
        Command::Copy => {
            if let Some(text) = s.copy_text() {
                ev.copy_text(text);
            }
            true
        }
        Command::Cut => {
            if let Some(text) = s.cut() {
                ev.copy_text(text);
            }
            true
        }
        Command::Fold => {
            let line = s.doc().line_of(s.selections().primary().head);
            s.fold_line(line);
            true
        }
        Command::Unfold => {
            let line = s.doc().line_of(s.selections().primary().head);
            s.unfold(line);
            true
        }
        Command::FoldAll => {
            s.fold_all();
            true
        }
        Command::UnfoldAll => {
            s.unfold_all();
            true
        }
        Command::FindNext { back } => {
            // With no search open, the selection or the word is looked for.
            if s.search().is_none()
                && let Some(text) = s.seed_query()
            {
                let query = Query {
                    text,
                    ..bar::query_of(shared)
                };
                s.find(query);
            }
            s.find_next(back)
        }
        Command::Find { .. } | Command::Complete | Command::GoToDefinition | Command::Hover => {
            false
        }
    };
    drop(s);
    if taken {
        shared.blink_on.set(true);
        changed(shared, ev);
    }
    taken
}

/// After an edit or a move: draw it, keep the caret lit, tell the owner of
/// a new text.
pub(super) fn changed(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    shared.blink_on.set(true);
    arm_blink(shared, ev);
    shared.bump(ev);
    let version = shared.session.borrow().doc().version();
    if version != shared.told.get() {
        shared.told.set(version);
        let mut on_change = shared.on_change.borrow_mut();
        if let Some(f) = on_change.as_mut() {
            f(ev, &shared.session.borrow());
        }
    }
}

/// Starts the caret's next blink, dropping the one in flight.
fn arm_blink(shared: &Shared, ev: &mut EventCx<'_>) {
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let token = shared.blink_token.get().wrapping_add(1);
    shared.blink_token.set(token);
    ev.wake_after(nodes.root, BLINK, token);
}

/// The position under window point `(x, y)`: the row under it, or the
/// nearest one on screen.
fn hit(shared: &Shared, ev: &EventCx<'_>, x: f32, y: f32) -> Option<(usize, usize, f32)> {
    let m = shared.metrics.get();
    let rows = shared.rows.borrow();
    let mut best: Option<(f32, usize, Rect)> = None;
    for r in rows.iter() {
        let Some(rect) = ev.rect_of(r.row) else {
            continue;
        };
        let d = if y < rect.y {
            rect.y - y
        } else if y >= rect.y + rect.h {
            y - (rect.y + rect.h) + 0.001
        } else {
            0.0
        };
        if best.as_ref().is_none_or(|b| d < b.0) {
            best = Some((d, r.line, rect));
        }
    }
    let (_, line, rect) = best?;
    let row = ((y - rect.y) / m.line_height).floor().max(0.0) as usize;
    let cells = (x - rect.x - m.text_x()) / m.cell;
    Some((line, row, cells))
}

/// The editor's pointer handler: placing, adding and dragging cursors.
pub(super) fn on_pointer(shared: &Rc<Shared>, ev: &mut EventCx<'_>) {
    if ev.phase() == DispatchPhase::Capture {
        return;
    }
    let Some(p) = ev.pointer().copied() else {
        return;
    };
    let Some(nodes) = shared.nodes.get() else {
        return;
    };
    let apple = viso_ui::primary_is_logo();
    match p.phase {
        PointerPhase::Down if p.buttons.contains(PointerButtons::PRIMARY) => {
            // Only a press on the text, not on the find bar.
            let Some(list) = ev.rect_of(nodes.list) else {
                return;
            };
            if !contains(list, p.x, p.y) {
                return;
            }
            ev.request_focus(nodes.root);
            ev.capture_pointer(nodes.root);
            if assist::close_popups(shared) {
                shared.bump(ev);
            }
            let Some((line, row, cells)) = hit(shared, ev, p.x, p.y) else {
                return;
            };
            let m = shared.metrics.get();
            let in_gutter = p.x - list.x < m.gutter;
            if in_gutter {
                gutter_press(shared, ev, line, p.x - list.x, p.modifiers.shift);
                return;
            }
            let clicks = ev.clicks();
            let m = p.modifiers;
            // The primary modifier's click goes to where the name is declared.
            let primary = if apple { m.logo } else { m.control };
            if primary && !m.alt && !m.shift {
                let at = shared.session.borrow().position_at(line, row, cells);
                shared.session.borrow_mut().click(at, false);
                changed(shared, ev);
                assist::go_to_definition(shared, ev, at);
                return;
            }
            let add = m.alt && !m.shift && (apple || !m.control);
            let boxed = m.alt && m.shift;
            let mut s = shared.session.borrow_mut();
            let at = s.position_at(line, row, cells);
            let drag = if boxed {
                s.select_box((line, cells), (line, cells));
                Drag::Box(line, cells)
            } else if add {
                s.toggle_cursor(at);
                Drag::Chars
            } else {
                match clicks {
                    2 => {
                        let word = s.word_at(at);
                        s.select_word(at, None);
                        Drag::Words(word)
                    }
                    n if n >= 3 => {
                        let start = s.doc().line_start(line);
                        let end = if line + 1 < s.doc().line_count() {
                            s.doc().line_start(line + 1)
                        } else {
                            s.doc().len()
                        };
                        s.select_line_at(at, None);
                        Drag::Lines(start..end)
                    }
                    _ => {
                        s.click(at, m.shift);
                        Drag::Chars
                    }
                }
            };
            drop(s);
            *shared.drag.borrow_mut() = Some(drag);
            changed(shared, ev);
        }
        PointerPhase::Move if p.buttons.contains(PointerButtons::PRIMARY) => {
            let Some(drag) = shared.drag.borrow().clone() else {
                return;
            };
            let Some((line, row, cells)) = hit(shared, ev, p.x, p.y) else {
                return;
            };
            let mut s = shared.session.borrow_mut();
            let at = s.position_at(line, row, cells);
            match drag {
                Drag::Chars => {
                    let mut primary = s.selections().primary();
                    if primary.head != at {
                        primary.head = at;
                        primary.goal = None;
                        let list: Vec<Selection> = s
                            .selections()
                            .list()
                            .iter()
                            .enumerate()
                            .map(|(i, x)| {
                                if i == s.selections().primary_index() {
                                    primary
                                } else {
                                    *x
                                }
                            })
                            .collect();
                        let index = s.selections().primary_index();
                        s.set_selections(crate::Selections::new(list, index));
                    }
                }
                Drag::Words(word) => s.select_word(at, Some(word)),
                Drag::Lines(lines) => s.select_line_at(at, Some(lines)),
                Drag::Box(from, x) => s.select_box((from, x), (line, cells)),
            }
            drop(s);
            // Past the viewport's edge, scroll toward the pointer.
            if let Some(rect) = ev.rect_of(nodes.list) {
                let lh = shared.metrics.get().line_height;
                let dy = if p.y < rect.y {
                    -lh
                } else if p.y > rect.y + rect.h {
                    lh
                } else {
                    0.0
                };
                if dy != 0.0 {
                    ev.scroll(
                        nodes.list,
                        ScrollRequest::By(Vec2 { x: 0.0, y: dy }),
                        ScrollBehavior::Instant,
                    );
                }
            }
            changed(shared, ev);
        }
        PointerPhase::Up => {
            *shared.drag.borrow_mut() = None;
            ev.release_pointer();
        }
        PointerPhase::Move if p.buttons.is_empty() => {
            // Resting over the text shows what is said of it.
            let over = ev
                .rect_of(nodes.list)
                .filter(|list| {
                    contains(*list, p.x, p.y) && p.x - list.x >= shared.metrics.get().gutter
                })
                .and_then(|_| hit(shared, ev, p.x, p.y))
                .and_then(|(line, row, cells)| {
                    let s = shared.session.borrow();
                    // Past a line's end is over nothing.
                    let at = s.position_at(line, row, cells);
                    let (_, _, col) = s.point_of(at);
                    (cells >= 0.0 && (col as f32) + 0.5 >= cells).then_some(at)
                });
            assist::pointer_moved(shared, ev, over, Vec2 { x: p.x, y: p.y });
        }
        _ => {}
    }
}

/// Whether window point `(x, y)` is in `r`.
fn contains(r: Rect, x: f32, y: f32) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// A press in the gutter at `x` from its left edge, on line `line`: on a
/// fold marker it folds or opens; elsewhere it selects the line, and a
/// drag from it whole lines.
fn gutter_press(shared: &Rc<Shared>, ev: &mut EventCx<'_>, line: usize, x: f32, extend: bool) {
    let m = shared.metrics.get();
    let mut s = shared.session.borrow_mut();
    let on_fold = x >= m.fold_x()
        && (s.folds().at(line).is_some()
            || crate::fold::folds_by_indent(s.doc(), line, s.rules().tab));
    if on_fold {
        s.toggle_fold(line);
        drop(s);
        *shared.drag.borrow_mut() = None;
        changed(shared, ev);
        return;
    }
    let start = s.doc().line_start(line);
    let end = if line + 1 < s.doc().line_count() {
        s.doc().line_start(line + 1)
    } else {
        s.doc().len()
    };
    let from = if extend {
        let p = s.selections().primary();
        let a = s.doc().line_of(p.anchor);
        let a_end = if a + 1 < s.doc().line_count() {
            s.doc().line_start(a + 1)
        } else {
            s.doc().len()
        };
        s.doc().line_start(a)..a_end
    } else {
        start..end
    };
    s.select_line_at(start, Some(from.clone()));
    drop(s);
    *shared.drag.borrow_mut() = Some(Drag::Lines(from));
    changed(shared, ev);
}

impl Session {
    /// Asks for the cursors to be drawn again: the focus or the blink
    /// changed.
    pub(crate) fn pending_cursors(&mut self) {
        self.pending.cursors = true;
    }
}
