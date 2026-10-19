//! Delivering the values a view's nodes show: a label's text, a text field's
//! seeded buffer, a control's value and range as its semantic state, an
//! image's asset (its bytes offered to the store the first time it shows), any
//! node's look (its `background` and `opacity`), whether it shows and takes
//! focus, a text's color, size and wrapping, and its role and accessible
//! name. A text field with no `value` is never seeded: its text is what its
//! user types.
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
//! nothing it reads never touches the node. A hot reload that mounts the
//! values of a node again keeps what it shows: only a value the reload changed
//! is delivered, as a first delivery.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::{Fault, FaultKind, Value};
use viso_ui::resource::{ResourceId, ResourceRevision};
use viso_ui::shader::BrushProgram;
use viso_ui::{
    AssetFit, BuildCx, Easing, LiveRegion, LookValue, NodeId, NodeStore, Rgba, Role, SemanticState,
    Semantics, Srgb, StateId, StructureCx, TextRequest, Timing,
};

use crate::control::{Control, ControlInput, ControlKind, Holds, When};
use crate::host::ViewHost;
use crate::layout_values::layout_value;
use crate::scope::Scope;
use crate::shaders::encode_shader;

/// The font size of a text whose node resolves none, in logical pixels.
const FONT_SIZE: f32 = 14.0;

/// The color of a text whose node binds no `color`.
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
/// mount registered: a view keeps one. A node that mounted before with the
/// same kind of control keeps what it shows, so only the values that changed
/// are delivered.
pub fn mount_values(
    cx: &mut StructureCx<'_>,
    host: &Rc<RefCell<ViewHost>>,
    nodes: &[(NodeId, Control)],
) {
    let Ok(mut view) = host.try_borrow_mut() else {
        return;
    };
    let prior = std::mem::take(&mut view.shown);
    let mut shown: Vec<Shown> = nodes
        .iter()
        .map(|(node, control)| {
            let mut shown = Shown::new(*node, control.clone(), Scope::EMPTY, &view, cx);
            if let Some(before) = prior
                .iter()
                .find(|before| before.node == *node && before.control.kind == control.kind)
            {
                shown.shows = before.shows.clone();
                shown.program = before.program.clone();
                shown.font_size = before.font_size;
                shown.seeded = true;
            }
            shown
        })
        .collect();
    for node in &mut shown {
        node.deliver(cx, &mut view);
    }
    if let Some(prior) = view.replace_values_hook(None) {
        cx.store.remove_structure_hook(prior);
    }
    let deps: Vec<StateId> = shown
        .iter()
        .flat_map(|node| node.deps.iter().copied())
        .collect();
    view.shown = shown;
    if deps.is_empty() {
        return;
    }
    drop(view);
    let keep = Rc::clone(host);
    let hook = cx.store.add_structure_hook(deps, move |cx, changed| {
        let Ok(mut view) = keep.try_borrow_mut() else {
            return;
        };
        let mut shown = std::mem::take(&mut view.shown);
        for node in &mut shown {
            if node.reads_any(changed) {
                node.deliver(cx, &mut view);
            }
        }
        view.shown = shown;
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
    let semantics = control.semantics;
    for entry in [
        control.value,
        control.min,
        control.max,
        control.fit,
        control.tint,
        look.background,
        look.opacity,
        semantics.label,
        semantics.hint,
        semantics.value,
        semantics.hidden,
        semantics.heading_level,
        control.node.visible,
        control.node.focusable,
        control.text.color,
        control.text.soft_wrap,
    ]
    .into_iter()
    .flatten()
    .chain(arms)
    .chain(control.layout.iter().map(|&(_, entry)| entry))
    {
        host.entry_cells(entry, scope, out);
    }
}

/// How many values a node shows: its value, the two entries beside it (a
/// slider's bounds, an image's fit and tint), its look's background and
/// opacity, its `semantics.label`, `hint`, `value`, `hidden` and
/// `heading_level`, its `visible` and `focusable`, and its text's `color`
/// and `soft_wrap`.
const SHOWN: usize = 16;

/// Where its background and opacity sit among the values a node shows.
const LOOK: usize = 5;

/// Where its `semantics.*` values start among the values a node shows.
const SEMANTICS: usize = 7;

/// Where its `visible` and `focusable` sit among the values a node shows.
const NODE: usize = 12;

/// Where its text's `color` and `soft_wrap` sit among the values a node
/// shows.
const TEXT: usize = 14;

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
    /// The value, lower bound, upper bound, fit, tint, background, opacity,
    /// `semantics.*`, presence and text style values it shows, `None` before
    /// the first delivery.
    shows: Option<[Value; SHOWN]>,
    /// Whether [`shows`](Self::shows) is what the node showed before a hot
    /// reload mounted it again: the next delivery skips the values equal to
    /// it and delivers the others as a first delivery.
    seeded: bool,
    /// The program a shader view paints with, as last delivered: a reload
    /// that changes it repaints the node even when its parameters hold.
    program: Option<Arc<BrushProgram>>,
    /// The font size its text was last requested at: a reload that resizes
    /// it requests the text again.
    font_size: f32,
    /// The values of its layout entries it lays out by, `None` before the
    /// first delivery and after a hot reload, which restyles the node from
    /// its constants and so wants each of them again.
    laid: Option<Box<[Value]>>,
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
            seeded: false,
            program: None,
            font_size: 0.0,
            laid: None,
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
        if self.seeded {
            self.laid = None;
        }
        if !self.lay_out(cx, host) {
            return;
        }
        let control = &self.control;
        let look = control.look;
        let (Ok(background), Ok(opacity)) = (
            self.pick(ControlInput::Background, look.background, cx, host),
            self.pick(ControlInput::Opacity, look.opacity, cx, host),
        ) else {
            return;
        };
        let semantics = control.semantics;
        let mut shows: [Value; SHOWN] = Default::default();
        for (value, entry) in shows.iter_mut().zip([
            control.value,
            control.min,
            control.max,
            control.fit,
            control.tint,
            background,
            opacity,
            semantics.label,
            semantics.hint,
            semantics.value,
            semantics.hidden,
            semantics.heading_level,
            control.node.visible,
            control.node.focusable,
            control.text.color,
            control.text.soft_wrap,
        ]) {
            let Some(entry) = entry else { continue };
            match host.evaluate(entry, &self.scope, None, &*cx.states) {
                Ok(evaluated) => *value = evaluated,
                Err(fault) => {
                    host.record_fault(fault);
                    return;
                }
            }
        }
        let program = match control.kind {
            ControlKind::Shader => control.shader.and_then(|at| host.shader(at)).cloned(),
            _ => None,
        };
        let repaint = match (&program, &self.program) {
            (Some(next), Some(prior)) => !Arc::ptr_eq(next, prior),
            (next, prior) => next.is_some() != prior.is_some(),
        };
        let font_size = cx.store.resolved_font_size(self.node).unwrap_or(FONT_SIZE);
        let resized = font_size != self.font_size
            && matches!(control.kind, ControlKind::Label | ControlKind::TextInput);
        let prior = self.shows.as_ref();
        let first = prior.is_none() || std::mem::take(&mut self.seeded);
        if prior == Some(&shows) && !repaint && !resized {
            return;
        }
        // A look value shows when an entry or an arm gives it; with arms and no
        // entry, no arm holding shows the property's default.
        let styled = |input: ControlInput| control.arms.iter().any(|a| a.input == input);
        let shown_background =
            (background.is_some() || styled(ControlInput::Background)).then_some(());
        let shown_opacity = (opacity.is_some() || styled(ControlInput::Opacity)).then_some(());
        let [
            value,
            min,
            max,
            fit,
            tint,
            background,
            opacity,
            label,
            hint,
            spoken,
            hidden,
            level,
            visible,
            focusable,
            text_color,
            soft_wrap,
        ] = &shows;
        let changed = |at: usize| prior.is_none_or(|prior| prior[at] != shows[at]);
        if semantics.says_anything()
            && ((SEMANTICS..NODE).any(changed)
                || (control.kind == ControlKind::Label && changed(0)))
        {
            let name = match (label.as_str(), control.kind) {
                (Some(label), _) => Some(label),
                (None, ControlKind::Label) => value.as_str(),
                (None, _) => None,
            };
            let said = Said {
                role: semantics.role.unwrap_or_default(),
                live: semantics.live,
                name,
                hint: hint.as_str(),
                value: spoken.as_str(),
                hidden: hidden.as_int().is_some_and(|v| v != 0),
                heading_level: level.as_int().and_then(|v| u8::try_from(v).ok()),
            };
            describe_node(cx.store, self.node, &said);
        }
        let mut moves: [Option<Timing>; 2] = [None; 2];
        for (at, (shown, transition)) in [
            (LOOK, (shown_background, look.background_transition)),
            (LOOK + 1, (shown_opacity, look.opacity_transition)),
        ] {
            let (Some(_), Some(transition), false) = (shown, transition, first) else {
                continue;
            };
            if !changed(at) {
                continue;
            }
            match host.evaluate(transition, &self.scope, None, &*cx.states) {
                Ok(spec) => {
                    let reduced = cx.states.env().environment().reduced_motion;
                    moves[at - LOOK] = Some(timing(&spec, reduced));
                }
                Err(fault) => host.record_fault(fault),
            }
        }
        let store = &mut *cx.store;
        if control.node.visible.is_some() && changed(NODE) {
            store.set_hidden(self.node, !truth(visible, true));
        }
        if control.node.focusable.is_some() && changed(NODE + 1) {
            store.set_focusable(self.node, truth(focusable, false));
        }
        for (at, entry, value) in [
            (LOOK, shown_background, LookValue::Fill(color(background))),
            (
                LOOK + 1,
                shown_opacity,
                LookValue::Opacity(float(opacity, 1.0)),
            ),
        ] {
            if entry.is_none() || !changed(at) {
                continue;
            }
            match (moves[at - LOOK], value) {
                (Some(timing), value) => store.transition(self.node, value, timing),
                (None, value) if first => store.present(self.node, value),
                (None, LookValue::Fill(fill)) => store.set_fill(self.node, fill),
                (None, LookValue::Opacity(opacity)) => store.set_opacity(self.node, opacity),
            }
        }
        let text = || TextRequest {
            text: value.as_str().unwrap_or_default().to_owned(),
            font_size,
            color: match control.text.color {
                Some(_) => color(text_color),
                None => COLOR,
            },
            soft_wrap: truth(soft_wrap, false),
            locale: None,
        };
        if prior.is_some()
            && (0..LOOK).chain(TEXT..SHOWN).all(|at| !changed(at))
            && !repaint
            && !resized
        {
            self.shows = Some(shows);
            return;
        }
        match control.kind {
            ControlKind::Plain => {}
            // A shader the view's table lacks, or a value that is not its
            // parameters, leaves the node as it is.
            ControlKind::Shader => {
                if let Some(program) = &program
                    && let Some((params, images)) = encode_shader(&program.layout, value)
                {
                    for &image in &images {
                        offer(store, host, image);
                    }
                    store.set_shader(self.node, program, params, &images);
                }
            }
            // An unbound source shows nothing.
            ControlKind::Asset => {
                if let Some(resource) = resource(value) {
                    offer(store, host, resource);
                    let tint = tint.as_int().map(|_| color(tint));
                    store.set_asset(self.node, resource, asset_fit(fit), tint);
                }
            }
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
        self.program = program;
        self.font_size = font_size;
    }
}

impl Shown {
    /// Evaluates its layout entries and lays the node out by each value that
    /// changed. `false` after a fault, which the host keeps and which leaves
    /// the node as it is.
    fn lay_out(&mut self, cx: &mut StructureCx<'_>, host: &mut ViewHost) -> bool {
        if self.control.layout.is_empty() {
            return true;
        }
        let mut laid = Vec::with_capacity(self.control.layout.len());
        for &(_, entry) in self.control.layout.iter() {
            match host.evaluate(entry, &self.scope, None, &*cx.states) {
                Ok(value) => laid.push(value),
                Err(fault) => {
                    host.record_fault(fault);
                    return false;
                }
            }
        }
        let env = cx.store.length_env();
        let font = cx.store.resolved_font_size(self.node).unwrap_or(FONT_SIZE);
        for (at, &(slot, _)) in self.control.layout.iter().enumerate() {
            if self
                .laid
                .as_ref()
                .is_some_and(|prior| prior[at] == laid[at])
            {
                continue;
            }
            match layout_value(slot, &laid[at], &env, font) {
                Ok(value) => cx.store.set_layout_value(self.node, value),
                Err(message) => host.record_fault(Fault {
                    kind: FaultKind::Unsupported,
                    at: None,
                    message,
                }),
            }
        }
        self.laid = Some(laid.into());
        true
    }

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

/// What a node tells assistive technology, as its values evaluate.
struct Said<'a> {
    role: Role,
    live: LiveRegion,
    name: Option<&'a str>,
    hint: Option<&'a str>,
    value: Option<&'a str>,
    hidden: bool,
    heading_level: Option<u8>,
}

/// Gives `node` the semantics `said` says, unless it has them. An image
/// without a name is decoration: a mute group.
fn describe_node(store: &mut NodeStore, node: NodeId, said: &Said<'_>) {
    let role = match (said.role, said.name) {
        (Role::Image, None) => Role::Group,
        (role, _) => role,
    };
    let unchanged = store.semantics(node).is_some_and(|s| {
        s.role == role
            && s.label.as_deref() == said.name
            && s.hint.as_deref() == said.hint
            && s.value.as_deref() == said.value
            && s.live == said.live
            && s.hidden == said.hidden
            && s.heading_level == said.heading_level
    });
    if unchanged {
        return;
    }
    store.set_semantics(
        node,
        Semantics {
            role,
            label: said.name.map(str::to_owned),
            hint: said.hint.map(str::to_owned),
            value: said.value.map(str::to_owned),
            live: said.live,
            hidden: said.hidden,
            heading_level: said.heading_level,
        },
    );
}

/// Marks the child of `node` at `selected` selected and each other child not.
fn select(store: &mut NodeStore, node: NodeId, selected: i64) {
    let links = |store: &NodeStore, id| store.arena().links(id);
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

/// The resource an `ImageAsset` or `SvgAsset` value names.
pub(crate) fn resource(value: &Value) -> Option<ResourceId> {
    let Value::Agg(asset) = value else {
        return None;
    };
    let id = asset.fields.first()?.as_int()?;
    Some(ResourceId(id.cast_unsigned()))
}

/// Offers `resource`'s bytes to the store the first time a node shows it:
/// those the view embeds, else, for an asset a reload named, the app's.
fn offer(store: &mut NodeStore, host: &ViewHost, resource: ResourceId) {
    if store.assets().knows(resource) {
        return;
    }
    match host.asset_bytes(resource) {
        Some(bytes) => {
            store
                .assets_mut()
                .offer(resource, ResourceRevision::default(), bytes.clone());
        }
        None => store.assets_mut().want(resource),
    }
}

/// An `ImageFit` value as the fit it names, `contain` for any other.
fn asset_fit(value: &Value) -> AssetFit {
    match value.as_int() {
        Some(1) => AssetFit::Cover,
        Some(2) => AssetFit::Fill,
        _ => AssetFit::Contain,
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
/// A `Bool` value, `default` when it is none.
fn truth(value: &Value, default: bool) -> bool {
    value.as_int().map_or(default, |v| v != 0)
}

fn float(value: &Value, default: f32) -> f32 {
    value.as_float().map_or(default, |v| v as f32)
}
