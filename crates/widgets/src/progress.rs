//! Progress controls — [`ProgressIndicator`] (a bar or a ring) and
//! [`Spinner`].
//!
//! A progress indicator is one leaf drawing how far work has come: a bar
//! filled from its leading edge ([`progress_bar`]) or a ring drawn round from
//! the top ([`progress_ring`]), over a faint track. Given no value it says work
//! of unknown length runs — a stretch of fill sweeping along the bar, an arc
//! chasing round the ring. A new value eases in (at once under reduced
//! motion). A [`Spinner`] only says work is under way, as the system draws it.
//!
//! None of them is interactive: they take no focus and no input. Their role
//! is [`Role::ProgressIndicator`], named by their label; an indicator with a
//! value reports it and its range, one without reports none.
//!
//! An indicator shows a fixed value, or the one a state cell holds
//! ([`ProgressIndicator::bind`]): writing the cell repaints the indicator alone
//! and updates what it reports, with no rebuild.
//!
//! ```
//! use viso_widgets::{progress_bar, spinner};
//! use viso_ui::{BuildCx, Component, NodeStore};
//!
//! let mut store = NodeStore::new();
//! let mut cx = BuildCx::new(&mut store);
//! progress_bar().range(0.0, 100.0).value(40.0).label("Uploading").build(&mut cx);
//! spinner().label("Loading").build(&mut cx);
//! ```

use viso_ui::{
    BuildCx, Component, Length, NodeId, NodeStore, ProgressShape, Rgba, Role, SemanticState,
    Semantics, Sizing, SpinnerLook, StateId, StateValue, Vec2,
};

/// The fill of an indicator given no tint.
const ACCENT: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The size a bar, a ring and a spinner measure to.
const BAR: Vec2 = Vec2 { x: 160.0, y: 4.0 };
const RING: Vec2 = Vec2 { x: 32.0, y: 32.0 };
const SPINNER: f32 = 20.0;

/// A progress bar or ring: how far work has come, or that work of unknown
/// length runs.
///
/// Construct one with [`progress_bar`] or [`progress_ring`]; it shows no value
/// until [`value`](Self::value) or [`bind`](Self::bind) gives one.
/// Invalidation: a bound cell's change marks the indicator `PAINT` and its
/// semantics; nothing else re-lays out.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressIndicator {
    shape: ProgressShape,
    value: Option<f32>,
    min: f32,
    max: f32,
    tint: Rgba,
    label: Option<String>,
    size: Sizing,
    cell: Option<StateId>,
}

/// A progress bar: a track filled from its leading edge, 160x4 unless
/// [`size`](ProgressIndicator::size) says.
pub fn progress_bar() -> ProgressIndicator {
    indicator(ProgressShape::Bar)
}

/// A progress ring: a ring drawn round clockwise from the top, 32 across
/// unless [`size`](ProgressIndicator::size) says.
pub fn progress_ring() -> ProgressIndicator {
    indicator(ProgressShape::Ring)
}

fn indicator(shape: ProgressShape) -> ProgressIndicator {
    ProgressIndicator {
        shape,
        value: None,
        min: 0.0,
        max: 1.0,
        tint: ACCENT,
        label: None,
        size: Sizing {
            width: Length::Fit,
            height: Length::Fit,
        },
        cell: None,
    }
}

impl ProgressIndicator {
    /// Show `value` within its range (`0..=1` unless [`range`](Self::range)
    /// says), clamped to it.
    pub fn value(mut self, value: f32) -> Self {
        self.value = Some(value);
        self
    }

    /// Set the range its value stands in.
    pub fn range(mut self, min: f32, max: f32) -> Self {
        self.min = min;
        self.max = max;
        self
    }

    /// Ink the fill `tint`; the track is it, faint.
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.tint = tint;
        self
    }

    /// Name it for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Set its own size request within its parent.
    pub fn size(mut self, size: Sizing) -> Self {
        self.size = size;
        self
    }

    /// Show the value state `cell` holds, following it as it changes: a
    /// [`StateValue::Float`] is the value, anything else (or NaN) work of
    /// unknown length. Needs a [`BuildCx::with_reactive`] cx.
    pub fn bind(mut self, cell: StateId) -> Self {
        self.cell = Some(cell);
        self
    }

    fn natural(&self) -> Vec2 {
        match self.shape {
            ProgressShape::Bar => BAR,
            ProgressShape::Ring => RING,
        }
    }

    /// `value` clamped to the range, and as a fraction of it.
    fn place(&self, value: Option<f32>) -> Option<(f32, f32)> {
        let (lo, hi) = (self.min.min(self.max), self.max.max(self.min));
        let value = value.filter(|v| !v.is_nan())?.clamp(lo, hi);
        let fraction = if hi > lo {
            (value - self.min) / (self.max - self.min)
        } else {
            0.0
        };
        Some((value, fraction))
    }

    /// Shows `value` on node `id`: its drawing and what it reports.
    fn show(&self, store: &mut NodeStore, id: NodeId, value: Option<f32>) {
        let placed = self.place(value);
        let fraction = placed.map(|(_, f)| f);
        store.set_progress(id, self.shape, self.natural(), self.tint, fraction);
        store.set_semantic_state(id, self.reported(placed));
    }

    fn reported(&self, placed: Option<(f32, f32)>) -> SemanticState {
        SemanticState {
            value: placed.map(|(v, _)| v),
            range: placed.map(|_| (self.min, self.max)),
            ..SemanticState::default()
        }
    }
}

impl Component for ProgressIndicator {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let placed = self.place(self.value);
        let handle = cx.progress(
            self.shape,
            self.size,
            self.natural(),
            self.tint,
            placed.map(|(_, f)| f),
        );
        let mut semantics = Semantics::role(Role::ProgressIndicator);
        semantics.label = self.label.clone();
        cx.semantics(handle, semantics);
        cx.semantic_state(handle, self.reported(placed));
        let Some(cell) = self.cell else {
            return;
        };
        let id = handle.id();
        let me = self.clone();
        let read = |value: Option<StateValue>| match value {
            Some(StateValue::Float(v)) => Some(v),
            _ => None,
        };
        cx.structure(|s| {
            me.show(s.store, id, read(s.states.get(cell)));
            s.store.add_structure_hook([cell], move |s, _| {
                if s.store.progress(id).is_some() {
                    me.show(s.store, id, read(s.states.get(cell)));
                }
            });
        });
    }
}

/// The system's spinner: work under way, turning while it shows.
///
/// Construct one with [`spinner`]. Apple systems draw eight spokes whose lead
/// steps round by the node's rotation alone; others an arc chasing its tail,
/// repainted each frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Spinner {
    tint: Rgba,
    side: f32,
    label: Option<String>,
}

/// A spinner, 20 across unless [`side`](Spinner::side) says.
pub fn spinner() -> Spinner {
    Spinner {
        tint: ACCENT,
        side: SPINNER,
        label: None,
    }
}

impl Spinner {
    /// Ink it `tint`.
    pub fn tint(mut self, tint: Rgba) -> Self {
        self.tint = tint;
        self
    }

    /// Set its side.
    pub fn side(mut self, side: f32) -> Self {
        self.side = side;
        self
    }

    /// Name it for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Component for Spinner {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let handle = cx.spinner(SpinnerLook::native(), self.side, self.tint);
        let mut semantics = Semantics::role(Role::ProgressIndicator);
        semantics.label = self.label.clone();
        cx.semantics(handle, semantics);
        cx.spin(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists,
    };

    #[test]
    fn a_bar_shows_its_value_s_fraction_and_reports_the_value() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        progress_bar()
            .range(0.0, 200.0)
            .value(50.0)
            .label("Uploading")
            .build(&mut cx);
        let bar = cx.root().unwrap();
        let shown = store.progress(bar).unwrap();
        assert_eq!(shown.shape, ProgressShape::Bar);
        assert_eq!(shown.value, Some(0.25));
        let state = store.semantic_state(bar).unwrap();
        assert_eq!(state.value, Some(50.0));
        assert_eq!(state.range, Some((0.0, 200.0)));
        let semantics = store.semantics(bar).unwrap();
        assert_eq!(semantics.role, Role::ProgressIndicator);
        assert_eq!(semantics.label.as_deref(), Some("Uploading"));
    }

    #[test]
    fn a_ring_with_no_value_runs_and_reports_none() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        progress_ring().build(&mut cx);
        let ring = cx.root().unwrap();
        assert_eq!(store.progress(ring).unwrap().value, None);
        assert!(store.is_progressing());
        assert_eq!(store.semantic_state(ring).unwrap().value, None);
    }

    #[test]
    fn a_bound_bar_follows_its_cell() {
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
        let cell = cx.state(StateValue::Float(0.5));
        progress_bar().bind(cell).build(&mut cx);
        let bar = cx.root().unwrap();
        assert_eq!(store.progress(bar).unwrap().value, Some(0.5));
        assert_eq!(store.structure_hook_count(), 1);
    }

    #[test]
    fn a_spinner_spins_as_it_is_built() {
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        spinner().label("Loading").build(&mut cx);
        let node = cx.root().unwrap();
        assert!(store.spinner(node).unwrap().spinning);
        assert_eq!(store.semantics(node).unwrap().role, Role::ProgressIndicator);
    }
}
