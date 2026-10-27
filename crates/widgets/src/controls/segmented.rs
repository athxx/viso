//! The [`SegmentedControl`] control — a row of mutually exclusive segments
//! on one track, a thumb sliding to the selected one.
//!
//! A segmented control is a [`RadioGroup`](crate::RadioGroup) laid out as a
//! row of segments sharing a rounded track: one `selected` cell holds the
//! chosen index as an [`StateValue::Int`], and the track's
//! [`Segments`](viso_ui::segments) draw a thumb under the selected segment
//! that slides over a spring to a new one, with a hairline between each two
//! neighbours the thumb is not near. Each segment is one focusable node, its
//! caption centred with room round it by its [`CheckShape::Segment`] mark;
//! given a width, the control shares it out evenly.
//!
//! A primary press then release on a segment, Enter or Space while it is
//! focused, or its access key selects it and fires `on_change` once with its
//! index. The control is one Tab stop (a roving group, landing on the
//! selected segment); the arrows move the focus and the selection to the
//! next or previous segment, wrapping, and Home and End to the first and
//! last. Its role is [`Role::Group`] with a [`Role::Radio`] per segment,
//! named by its caption and checked while selected.
//!
//! ```
//! use viso_widgets::segmented_control;
//! use viso_ui::{ColorToken, SemanticProjector, BuildCx, BindingTable, Component, NodeStore, StateStore, TextEdits, VirtualLists};
//!
//! let range = segmented_control(["Day", "Week", "Month"])
//!     .selected(1)
//!     .on_change(|_ev, index| {
//!         let _ = index;
//!     });
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! range.build(&mut cx);
//! ```

use std::cell::RefCell;
use std::rc::Rc;
use viso_ui::ColorToken;

use viso_ui::adaptive::LayoutDirection;
use viso_ui::segments::Segments;
use viso_ui::{
    Access, Align, Axis, BoxStyle, BuildCx, CheckMark, CheckShape, Component, Content, EventCx,
    FlexStyle, Haptic, Inset, Justify, Key, LeafStyle, Length, NodeId, PointerButtons,
    PointerPhase, Rgba, Role, SemanticState, Semantics, Sizing, StateId, StateValue,
};

use super::check::INK;
use super::radio::step;
use crate::controls::mnemonic::Mnemonic;

/// A shared, mutable change callback carrying the newly-selected index,
/// cloned into every segment's pointer and key handler.
type SharedChange = Rc<RefCell<Option<Box<dyn FnMut(&mut EventCx<'_>, usize)>>>>;

/// The room between the track's edge and its segments.
const TRACK_INSET: f32 = 2.0;

/// The look and size of a [`SegmentedControl`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SegmentedStyle {
    /// Its own size request within its parent. Defaults to `Fit` on both
    /// axes: its segments side by side, each its caption with the room round
    /// it. Given a width, the segments share it evenly.
    pub size: Sizing,
    /// The captions' color.
    pub color: Rgba,
    /// The track.
    pub track: Rgba,
    /// The thumb under the selected segment.
    pub thumb: Rgba,
    /// The hairlines between segments.
    pub divider: Rgba,
}

impl Default for SegmentedStyle {
    fn default() -> Self {
        SegmentedStyle {
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            color: INK,
            track: ColorToken::Foreground.alpha(0.1),
            thumb: ColorToken::Foreground.alpha(0.24),
            divider: ColorToken::Foreground.alpha(0.2),
        }
    }
}

/// A row of mutually exclusive segments with a sliding thumb.
///
/// Construct one with [`segmented_control`]. See the [module docs](self) for
/// a build example. Invalidation: a selection writes the reactive `selected`
/// cell, which slides the thumb, repainting the track each frame it moves
/// and the two segments' semantics; the captions and structure are static.
pub struct SegmentedControl {
    /// The segment captions, also each segment's accessible name; an `&`
    /// marks a segment's access key.
    options: Vec<String>,
    /// The initially-selected segment (defaults to `0`).
    selected: usize,
    cell: Option<StateId>,
    style: SegmentedStyle,
    on_change: SharedChange,
}

/// A [`SegmentedControl`] of a segment per caption, the first selected and no
/// handler yet.
pub fn segmented_control<I, S>(options: I) -> SegmentedControl
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    SegmentedControl {
        options: options.into_iter().map(Into::into).collect(),
        selected: 0,
        cell: None,
        style: SegmentedStyle::default(),
        on_change: Rc::new(RefCell::new(None)),
    }
}

impl SegmentedControl {
    /// Set the change callback, fired with the newly-selected index.
    pub fn on_change(self, handler: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        *self.on_change.borrow_mut() = Some(Box::new(handler));
        self
    }

    /// Set the initially-selected segment (defaults to `0`). An index past
    /// the last segment selects none until one is activated.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Keep the selection in int cell `cell`, which it shows and writes,
    /// rather than a cell of its own.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// Replace the whole [`SegmentedStyle`].
    pub fn style(mut self, style: SegmentedStyle) -> Self {
        self.style = style;
        self
    }

    /// Set its own size request within its parent (defaults to `Fit`).
    pub fn size(mut self, size: Sizing) -> Self {
        self.style.size = size;
        self
    }

    /// Set the captions' color.
    pub fn color(mut self, color: Rgba) -> Self {
        self.style.color = color;
        self
    }
}

/// The segment an int cell's value selects, `None` for none.
fn chosen(value: Option<StateValue>) -> Option<u32> {
    match value {
        Some(StateValue::Int(i)) => u32::try_from(i).ok(),
        _ => None,
    }
}

/// Select segment `index` into `cell` and fire `on_change`, unless it is
/// selected already.
fn select(cb: &SharedChange, ev: &mut EventCx<'_>, cell: StateId, index: usize) {
    let next = StateValue::Int(index as i32);
    if ev.get(cell) == Some(next) {
        return;
    }
    ev.set(cell, next);
    ev.feedback(Haptic::Selection);
    if let Some(f) = cb.borrow_mut().as_mut() {
        f(ev, index);
    }
}

impl Component for SegmentedControl {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let cell = self
            .cell
            .unwrap_or_else(|| cx.state(StateValue::Int(self.selected as i32)));
        let style = self.style;
        let captions: Vec<Mnemonic> = self.options.iter().map(|o| Mnemonic::of(o)).collect();
        let mut segments = Vec::with_capacity(captions.len());
        let track = cx.flex(
            FlexStyle {
                axis: Axis::Row,
                gap: 0.0,
                padding: Inset::all(TRACK_INSET),
                align: Align::Stretch,
                justify: Justify::Start,
                size: style.size,
                style: BoxStyle {
                    fill: style.track,
                    radius: 8.0,
                    ..BoxStyle::NONE
                },
            },
            |cx| {
                for (index, caption) in captions.iter().enumerate() {
                    let segment = cx.leaf(LeafStyle {
                        size: Sizing {
                            width: Length::Fill { weight: 1.0 },
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    });
                    cx.text_request(segment, caption.label().color(style.color).request());
                    cx.check_mark(
                        segment,
                        CheckMark {
                            shape: CheckShape::Segment,
                            checked: index == self.selected,
                        },
                    );
                    cx.semantics(
                        segment,
                        Semantics::role(Role::Radio).with_label(caption.text.as_str()),
                    );
                    cx.bind_semantic_state(segment, move |cx| {
                        SemanticState::checked(chosen(cx.get(cell)) == Some(index as u32))
                    });
                    cx.focusable(segment, true);
                    if let Some(key) = caption.char() {
                        cx.access_key(segment, key);
                    }
                    segments.push(segment);
                }
            },
        );
        cx.roving(track);
        cx.semantics(track, Semantics::role(Role::Group));
        let id = track.id();
        cx.structure(|s| {
            let shown = chosen(s.states.get(cell));
            let thumb = Segments::new(style.thumb, style.divider, shown);
            s.store.set_content_payload(id, Content::Segments(thumb));
            s.store.add_structure_hook([cell], move |s, _| {
                s.store.select_segment(id, chosen(s.states.get(cell)));
            });
        });

        let ids: Rc<[NodeId]> = segments.iter().map(|s| s.id()).collect();
        for (index, &segment) in segments.iter().enumerate() {
            let cb = self.on_change.clone();
            cx.on_pointer(segment, move |ev| {
                let Some(p) = ev.pointer() else { return };
                if p.phase == PointerPhase::Up && p.buttons.contains(PointerButtons::PRIMARY) {
                    select(&cb, ev, cell, index);
                }
            });
            let cb = self.on_change.clone();
            let ids = Rc::clone(&ids);
            cx.on_key(segment, move |ev| {
                if ev.access() == Some(Access::Key) {
                    ev.stop_propagation();
                    ev.request_focus(ids[index]);
                    select(&cb, ev, cell, index);
                    return;
                }
                let Some(k) = ev.key().copied() else { return };
                if !k.pressed {
                    return;
                }
                if matches!(k.key, Key::Enter | Key::Space) {
                    ev.stop_propagation();
                    if !k.repeat {
                        select(&cb, ev, cell, index);
                    }
                    return;
                }
                let rtl = ev.env().environment().layout_direction == LayoutDirection::Rtl;
                if let Some(next) = step(k.key, index, ids.len(), rtl) {
                    ev.stop_propagation();
                    ev.request_focus(ids[next]);
                    select(&cb, ev, cell, next);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use viso_ui::{
        BindingTable, KeyEvent, Modifiers, NodeStore, SemanticProjector, StateStore, TextEdits,
        VirtualLists,
    };

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

        fn build(&mut self, control: SegmentedControl) -> NodeId {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            control.build(&mut cx);
            cx.root().expect("a segmented control declares a root node")
        }

        fn key(&mut self, node: NodeId, key: Key) {
            let ev = KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            };
            let mut handler = self.store.take_key_handler(node).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(node, handler);
        }
    }

    #[test]
    fn a_track_of_focusable_radio_segments_drawing_a_thumb() {
        let mut rx = Reactive::new();
        let root = rx.build(segmented_control(["Day", "&Week", "Month"]).selected(1));
        let segments: Vec<NodeId> = rx.store.arena().children(root).collect();
        assert_eq!(segments.len(), 3);
        assert!(rx.store.is_roving(root));
        assert_eq!(rx.store.semantics(root).unwrap().role, Role::Group);
        for &segment in &segments {
            assert!(rx.store.focusable(segment));
            assert_eq!(rx.store.semantics(segment).unwrap().role, Role::Radio);
        }
        assert_eq!(
            rx.store.semantics(segments[1]).unwrap().label.as_deref(),
            Some("Week")
        );
        let thumb = rx.store.segments(root).expect("the track draws segments");
        assert_eq!((thumb.selected, thumb.shown()), (Some(1), 1.0));
    }

    #[test]
    fn arrows_move_the_selection_round_and_report_it() {
        let seen = Rc::new(Cell::new(usize::MAX));
        let s = seen.clone();
        let mut rx = Reactive::new();
        let root = rx.build(segmented_control(["A", "B", "C"]).on_change(move |_, i| s.set(i)));
        let segments: Vec<NodeId> = rx.store.arena().children(root).collect();
        rx.key(segments[0], Key::Left);
        assert_eq!(seen.get(), 2, "the selection wraps round");
        rx.key(segments[2], Key::Home);
        assert_eq!(seen.get(), 0);
        rx.key(segments[0], Key::Space);
        assert_eq!(seen.get(), 0, "selecting the selected reports nothing new");
    }
}
