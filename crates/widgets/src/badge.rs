//! The [`Badge`] — a small pill counting what waits, or a dot saying
//! something does.
//!
//! A badge is one leaf: its count is its caption, centred on the pill its
//! background draws, at least as wide as tall, so one digit sits in a
//! circle and more in a pill; past its [`max`](Badge::max) it shows the most
//! and a plus (`99+`). A badge with no count is a dot. It takes no focus and
//! no input. Set it where it marks something — in a stack over the corner of
//! an icon, at the end of a row.
//!
//! Its role is [`Role::Label`], named by its [`label`](Badge::label), else by
//! the count it shows; a dot with no label is decoration.
//!
//! A badge shows a fixed count, or the one a state cell holds
//! ([`Badge::bind`]): writing the cell reshapes the badge alone.
//!
//! ```
//! use viso_widgets::{badge, dot_badge};
//! use viso_ui::{BuildCx, Component, NodeStore};
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! badge(120).label("120 unread").build(&mut cx);
//! dot_badge().build(&mut cx);
//! ```

use viso_ui::plate::{BADGE_FILL, BADGE_INK, badge_count};
use viso_ui::{
    BoxStyle, BuildCx, CheckMark, CheckShape, Component, FontSize, FontWeight, LeafStyle, Length,
    NodeId, NodeStore, Rgba, Role, Semantics, Sizing, StateId, StateValue, TextRequest, TextStyle,
};

/// A badge's caption size.
const FONT_SIZE: f32 = 11.0;

/// A count shown on a pill, or a dot.
///
/// Construct one with [`badge`] or [`dot_badge`]. Invalidation: a bound
/// cell's change reshapes the caption, marking the badge `MEASURE`, `PAINT`
/// and its semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct Badge {
    count: Option<i64>,
    max: i64,
    fill: Rgba,
    ink: Rgba,
    font_size: f32,
    label: Option<String>,
    cell: Option<StateId>,
}

/// A badge counting `count`.
pub fn badge(count: i64) -> Badge {
    Badge {
        count: Some(count),
        ..dot_badge()
    }
}

/// A badge with no count: a dot.
pub fn dot_badge() -> Badge {
    Badge {
        count: None,
        max: 99,
        fill: BADGE_FILL,
        ink: BADGE_INK,
        font_size: FONT_SIZE,
        label: None,
        cell: None,
    }
}

impl Badge {
    /// Show at most `max`, and `max` and a plus past it (99 unless set).
    pub fn max(mut self, max: i64) -> Self {
        self.max = max;
        self
    }

    /// Fill the pill `fill` (the error red unless set).
    pub fn tint(mut self, fill: Rgba) -> Self {
        self.fill = fill;
        self
    }

    /// Ink the count `ink` (white unless set).
    pub fn ink(mut self, ink: Rgba) -> Self {
        self.ink = ink;
        self
    }

    /// Set the count's size, in dp (11 unless set); the pill grows with it.
    pub fn font_size(mut self, size: f32) -> Self {
        self.font_size = size;
        self
    }

    /// Name it for assistive technology ("3 unread"), in place of the count.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Show the count state `cell` holds, following it as it changes: a
    /// [`StateValue::Int`] is the count, anything else a dot. Needs a
    /// [`BuildCx::with_reactive`] cx.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    /// What it shows counting `count`.
    fn caption(&self, count: Option<i64>) -> String {
        count.map_or_else(String::new, |c| badge_count(c, self.max))
    }

    fn request(&self, caption: String) -> TextRequest {
        TextRequest {
            text: caption,
            font_size: FontSize::Fixed(self.font_size),
            color: self.ink,
            soft_wrap: false,
            locale: None,
            style: TextStyle {
                weight: FontWeight(500),
                ..TextStyle::default()
            },
            spans: Vec::new(),
        }
    }

    fn semantics(&self, caption: &str) -> Semantics {
        match (&self.label, caption) {
            (Some(label), _) => Semantics::role(Role::Label).with_label(label.clone()),
            (None, "") => Semantics::role(Role::Group),
            (None, caption) => Semantics::role(Role::Label).with_label(caption),
        }
    }

    /// Shows `count` on node `id`.
    fn show(&self, store: &mut NodeStore, id: NodeId, count: Option<i64>) {
        let caption = self.caption(count);
        store.set_semantics(id, self.semantics(&caption));
        store.set_text_request(id, self.request(caption));
    }
}

impl Component for Badge {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let handle = cx.leaf(LeafStyle {
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            style: BoxStyle {
                fill: self.fill,
                radius_pct: 0.5,
                ..BoxStyle::NONE
            },
        });
        let caption = self.caption(self.count);
        cx.semantics(handle, self.semantics(&caption));
        cx.text_request(handle, self.request(caption));
        cx.check_mark(
            handle,
            CheckMark {
                shape: CheckShape::Plate,
                checked: false,
            },
        );
        let Some(cell) = self.cell else {
            return;
        };
        let id = handle.id();
        let me = self.clone();
        let read = |value: Option<StateValue>| match value {
            Some(StateValue::Int(v)) => Some(i64::from(v)),
            _ => None,
        };
        cx.structure(|s| {
            me.show(s.store, id, read(s.states.get(cell)));
            s.store.add_structure_hook([cell], move |s, _| {
                if s.store.arena().is_live(id) {
                    me.show(s.store, id, read(s.states.get(cell)));
                }
            });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{BindingTable, SemanticProjector, StateStore, TextEdits, VirtualLists};

    #[test]
    fn a_badge_shows_its_count_on_a_pill_and_past_its_most_a_plus() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        badge(120).build(&mut cx);
        let node = cx.root().unwrap();
        assert_eq!(store.text_request(node).unwrap().text, "99+");
        assert_eq!(store.check_mark(node).unwrap().shape, CheckShape::Plate);
        assert_eq!(store.style(node).radius_pct, 0.5);
        let semantics = store.semantics(node).unwrap();
        assert_eq!(semantics.role, Role::Label);
        assert_eq!(semantics.label.as_deref(), Some("99+"));
    }

    #[test]
    fn a_dot_has_no_caption_and_no_name_unless_labelled() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        dot_badge().build(&mut cx);
        let node = cx.root().unwrap();
        assert_eq!(store.text_request(node).unwrap().text, "");
        assert_eq!(store.semantics(node).unwrap().role, Role::Group);
    }

    #[test]
    fn a_bound_badge_follows_its_cell() {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let mut cx = BuildCx::with_reactive(
            &mut store,
            &mut states,
            &mut bindings,
            &mut lists,
            &mut edits,
            &mut projectors,
        );
        let cell = cx.state(StateValue::Int(4));
        dot_badge().bind(cell).build(&mut cx);
        let node = cx.root().unwrap();
        assert_eq!(store.text_request(node).unwrap().text, "4");
        assert_eq!(store.structure_hook_count(), 1);
    }
}
