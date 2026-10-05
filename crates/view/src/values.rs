//! Delivering the values a view's nodes show: a label's text, a text field's
//! seeded buffer, a control's value and range as its semantic state, and any
//! node's look (its `background` and `opacity`). A text field with no `value`
//! is never seeded: its text is what its user types.
//!
//! A look value with a `transition.*` entry moves to each new value over the
//! transition that entry evaluates to when the value changes; the first value
//! a node shows, and every value under reduced motion with an `instant`
//! transition, shows at once. The cells a transition reads are no
//! dependencies: changing one changes only the next move. A first delivery to
//! a node already moving, as a hot reload's is, heads the move to its value
//! rather than cutting it.
//!
//! A node's value is a pure entry of the view's handler table. It is evaluated
//! when the node mounts and again only when a cell it reads changes, and a value
//! equal to the one the node shows delivers nothing, so a frame that changes
//! nothing it reads never touches the node.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_ui::{
    BuildCx, Easing, LookValue, NodeId, NodeStore, Rgba, SemanticState, Srgb, StateId, StructureCx,
    TextRequest, Timing,
};

use crate::control::{Control, ControlInput, ControlKind, When};
use crate::host::ViewHost;
use crate::scope::Scope;

/// The font size of the text a view shows, in logical pixels.
const FONT_SIZE: f32 = 14.0;

/// The color of the text a view shows.
const COLOR: Rgba = Rgba {
    r: 0.05,
    g: 0.05,
    b: 0.06,
    a: 1.0,
};

/// Delivers the value each node of `nodes` shows, then registers one structure
/// hook that re-delivers, on a frame changing cells they read, the values of
/// exactly the nodes reading them. `nodes` are static nodes of the view `host`
/// runs, whose entries run in the empty scope.
///
/// Mounting again, as a hot reload does, replaces the hook the view's previous
/// mount registered: a view keeps one.
pub fn mount_values(
    cx: &mut StructureCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[(NodeId, Control)],
) {
    let Ok(mut view) = host.try_borrow_mut() else {
        return;
    };
    let mut shown: Vec<Shown> = nodes
        .iter()
        .map(|(node, control)| Shown::new(*node, control.clone(), Scope::EMPTY, &view, cx))
        .collect();
    for node in &mut shown {
        node.deliver(cx, &mut view);
    }
    if let Some(prior) = view.replace_values_hook(None) {
        cx.store.remove_structure_hook(prior);
    }
    shown.retain(|node| !node.deps.is_empty());
    if shown.is_empty() {
        return;
    }
    drop(view);
    let deps: Vec<StateId> = shown
        .iter()
        .flat_map(|node| node.deps.iter().copied())
        .collect();
    let keep = Rc::clone(host);
    let hook = cx.store.add_structure_hook(deps, move |cx, changed| {
        let Ok(mut view) = keep.try_borrow_mut() else {
            return;
        };
        for node in &mut shown {
            if node.reads_any(changed) {
                node.deliver(cx, &mut view);
            }
        }
    });
    host.borrow_mut().replace_values_hook(Some(hook));
}

/// [`mount_values`] for the nodes a macro expansion built, run over the stores
/// of that build: `nodes[i]` is the node `controls[i]` drives, `None` for one
/// the expansion did not record.
#[doc(hidden)]
pub fn __mount_values(
    cx: &mut BuildCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[Option<NodeId>],
    controls: &[Control],
) {
    let nodes: Vec<(NodeId, Control)> = nodes
        .iter()
        .zip(controls)
        .filter_map(|(node, control)| Some(((*node)?, control.clone())))
        .collect();
    cx.structure(|cx| mount_values(cx, host, &nodes));
}

/// Appends the cells the entries `control` delivers read in `scope` to `out`.
/// A control whose styles follow interaction states reads the store's
/// interaction cell besides, which the caller adds.
pub(crate) fn control_cells(
    control: &Control,
    scope: &Scope,
    host: &ViewHost,
    out: &mut Vec<StateId>,
) {
    let look = control.look;
    let arms = control.arms.iter().flat_map(|arm| {
        let when = arm.when.iter().filter_map(|w| match *w {
            When::Entry(entry) => Some(entry),
            _ => None,
        });
        when.chain([arm.entry])
    });
    for entry in [
        control.value,
        control.min,
        control.max,
        look.background,
        look.opacity,
    ]
    .into_iter()
    .flatten()
    .chain(arms)
    {
        host.entry_cells(entry, scope, out);
    }
}

/// How many values a node shows.
const SHOWN: usize = 5;

/// A node showing a value of the view.
pub(crate) struct Shown {
    node: NodeId,
    control: Control,
    /// The store's interaction cell, when its styles follow interaction
    /// states.
    interaction: Option<StateId>,
    /// The scope its entries run in.
    scope: Scope,
    /// The cells its entries read, ascending.
    deps: Box<[StateId]>,
    /// The value, lower bound, upper bound, background and opacity it shows,
    /// `None` before the first delivery.
    shows: Option<[Value; SHOWN]>,
}

impl Shown {
    /// Node `node`, showing the values of `control`'s entries in `scope`.
    pub(crate) fn new(
        node: NodeId,
        control: Control,
        scope: Scope,
        host: &ViewHost,
        cx: &mut StructureCx<'_>,
    ) -> Shown {
        let interaction = control
            .reads_state()
            .then(|| cx.store.interaction_cell(cx.states));
        let mut shown = Shown {
            node,
            control,
            interaction,
            scope: Scope::EMPTY,
            deps: Box::default(),
            shows: None,
        };
        shown.rescope(scope, host);
        shown
    }

    /// Moves it into `scope`, as a region mount whose bindings changed does.
    /// It keeps the values it shows, so the next delivery skips equal ones.
    pub(crate) fn rescope(&mut self, scope: Scope, host: &ViewHost) {
        let mut deps = Vec::new();
        control_cells(&self.control, &scope, host, &mut deps);
        deps.extend(self.interaction);
        deps.sort_unstable_by_key(|id| (id.index(), id.generation()));
        deps.dedup();
        self.scope = scope;
        self.deps = deps.into();
    }

    /// Whether a cell of `changed` is one its entries read.
    pub(crate) fn reads_any(&self, changed: &[StateId]) -> bool {
        changed.iter().any(|id| {
            self.deps
                .binary_search_by_key(&(id.index(), id.generation()), |d| {
                    (d.index(), d.generation())
                })
                .is_ok()
        })
    }

    /// Evaluates its value and range against the current states and delivers
    /// them unless the node already shows them. A fault is kept as the host's
    /// [`last_fault`](ViewHost::last_fault) and leaves the node as it is.
    pub(crate) fn deliver(&mut self, cx: &mut StructureCx<'_>, host: &mut ViewHost) {
        let control = &self.control;
        let look = control.look;
        let (Ok(background), Ok(opacity)) = (
            self.pick(ControlInput::Background, look.background, cx, host),
            self.pick(ControlInput::Opacity, look.opacity, cx, host),
        ) else {
            return;
        };
        let mut shows: [Value; SHOWN] = Default::default();
        for (value, entry) in
            shows
                .iter_mut()
                .zip([control.value, control.min, control.max, background, opacity])
        {
            let Some(entry) = entry else { continue };
            match host.evaluate(entry, &self.scope, None, &*cx.states) {
                Ok(evaluated) => *value = evaluated,
                Err(fault) => {
                    host.record_fault(fault);
                    return;
                }
            }
        }
        let prior = self.shows.as_ref();
        if prior == Some(&shows) {
            return;
        }
        // A look value shows when an entry or an arm gives it; with arms and no
        // entry, no arm holding shows the property's default.
        let styled = |input: ControlInput| control.arms.iter().any(|a| a.input == input);
        let shown_background =
            (background.is_some() || styled(ControlInput::Background)).then_some(());
        let shown_opacity = (opacity.is_some() || styled(ControlInput::Opacity)).then_some(());
        let [value, min, max, background, opacity] = &shows;
        let changed = |at: usize| prior.is_none_or(|prior| prior[at] != shows[at]);
        let mut moves: [Option<Timing>; 2] = [None; 2];
        for (at, (shown, transition)) in [
            (3, (shown_background, look.background_transition)),
            (4, (shown_opacity, look.opacity_transition)),
        ] {
            let (Some(_), Some(transition), Some(_)) = (shown, transition, prior) else {
                continue;
            };
            if !changed(at) {
                continue;
            }
            match host.evaluate(transition, &self.scope, None, &*cx.states) {
                Ok(spec) => {
                    let reduced = cx.states.env().environment().reduced_motion;
                    moves[at - 3] = Some(timing(&spec, reduced));
                }
                Err(fault) => host.record_fault(fault),
            }
        }
        let store = &mut *cx.store;
        for (at, entry, value) in [
            (3, shown_background, LookValue::Fill(color(background))),
            (4, shown_opacity, LookValue::Opacity(float(opacity, 1.0))),
        ] {
            if entry.is_none() || !changed(at) {
                continue;
            }
            match (moves[at - 3], value) {
                (Some(timing), value) => store.transition(self.node, value, timing),
                (None, value) if prior.is_none() => store.present(self.node, value),
                (None, LookValue::Fill(fill)) => store.set_fill(self.node, fill),
                (None, LookValue::Opacity(opacity)) => store.set_opacity(self.node, opacity),
            }
        }
        let text = || TextRequest {
            text: value.as_str().unwrap_or_default().to_owned(),
            font_size: FONT_SIZE,
            color: COLOR,
            soft_wrap: false,
            locale: None,
        };
        if prior.is_some() && (0..3).all(|at| !changed(at)) {
            self.shows = Some(shows);
            return;
        }
        match control.kind {
            ControlKind::Plain => {}
            ControlKind::Label => store.set_text_request(self.node, text()),
            ControlKind::TextInput if control.value.is_some() => {
                store.seed_text(self.node, text());
            }
            ControlKind::TextInput => {}
            ControlKind::Toggle => {
                let checked = value.as_int().is_some_and(|v| v != 0);
                store.set_semantic_state(self.node, SemanticState::checked(checked));
            }
            ControlKind::Slider => {
                let (min, max) = (float(min, 0.0), float(max, 1.0));
                let state = SemanticState {
                    value: Some(float(value, min)),
                    range: Some((min, max)),
                    ..SemanticState::default()
                };
                store.set_semantic_state(self.node, state);
            }
            ControlKind::Select => select(store, self.node, value.as_int().unwrap_or(0)),
        }
        self.shows = Some(shows);
    }
}

impl Shown {
    /// The entry `input` shows now: that of the first of its arms whose
    /// condition holds, else `base`. `Err` after a fault, which the host
    /// keeps.
    fn pick(
        &self,
        input: ControlInput,
        base: Option<u32>,
        cx: &StructureCx<'_>,
        host: &mut ViewHost,
    ) -> Result<Option<u32>, ()> {
        for arm in self.control.arms.iter().filter(|arm| arm.input == input) {
            match arm.holds(self.node, cx.store, host, &self.scope, &*cx.states) {
                Ok(true) => return Ok(Some(arm.entry)),
                Ok(false) => {}
                Err(fault) => {
                    host.record_fault(fault);
                    return Err(());
                }
            }
        }
        Ok(base)
    }
}

/// Marks the child of `node` at `selected` selected and each other child not.
fn select(store: &mut NodeStore, node: NodeId, selected: i64) {
    let links = |store: &NodeStore, id| store.arena().links(id).copied();
    let mut child = links(store, node).and_then(|l| l.first_child);
    let mut index = 0;
    while let Some(id) = child {
        child = links(store, id).and_then(|l| l.next_sibling);
        let state = SemanticState {
            selected: Some(index == selected),
            ..SemanticState::default()
        };
        store.set_semantic_state(id, state);
        index += 1;
    }
}

/// A `Color` value (sRGB `0xRRGGBBAA`) as a fill, transparent for `None`.
fn color(value: &Value) -> Rgba {
    match value.as_int() {
        Some(rgba) => Srgb::from_rgba32(rgba as u32).into_linear_straight(),
        None => Rgba::TRANSPARENT,
    }
}

/// The timing of a `Transition` value (`duration`, `delay`, `easing`,
/// `reduced`): instant when `reduced_motion` holds and its `reduced` is
/// `instant`. A negative or non-finite time is zero.
fn timing(spec: &Value, reduced_motion: bool) -> Timing {
    let Value::Agg(spec) = spec else {
        return Timing::default();
    };
    let field = |at: usize| spec.fields.get(at);
    let time = |at: usize| {
        let seconds = field(at).and_then(Value::as_float).unwrap_or(0.0);
        std::time::Duration::try_from_secs_f64(seconds).unwrap_or_default()
    };
    let tag = |at: usize| field(at).and_then(Value::as_int).unwrap_or(0);
    if reduced_motion && tag(3) == 0 {
        return Timing::default();
    }
    let easing = match tag(2) {
        0 => Easing::Linear,
        1 => Easing::EaseIn,
        3 => Easing::EaseInOut,
        _ => Easing::EaseOut,
    };
    Timing {
        duration: time(0),
        delay: time(1),
        easing,
    }
}

/// A `Float` value as `f32`, `default` for any other.
fn float(value: &Value, default: f32) -> f32 {
    value.as_float().map_or(default, |v| v as f32)
}
