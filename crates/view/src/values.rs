//! Delivering the values a view's nodes show: a label's text, a text field's
//! seeded buffer, a control's value and range as its semantic state, a check's
//! caption, an image's asset (its bytes offered to the store the first time it
//! shows), any node's look (its `background` and `opacity`), whether it shows
//! and takes focus, a text's color, size and wrapping, and its role and
//! accessible name. A text field is seeded with its style, placeholder and
//! masking; with no `value` its text is what its user types. A field's
//! placeholder is its hint where it has none, and a secure field is a
//! [`Role::SecureField`] that tells no value.
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
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::{Fault, FaultKind, Value};
use viso_ui::resource::{ResourceId, ResourceRevision};
use viso_ui::shader::BrushProgram;
use viso_ui::{Alignment2D, BoundSize, LayoutValue, LengthTerms};
use viso_ui::{
    AssetFit, Border, BuildCx, CheckMark, CheckShape, CursorIcon, Easing, FieldLook, FieldSeed,
    FontSize, InputKind, LiveRegion, LookValue, NodeId, NodeStore, NumberFormat, Rgba, Role,
    SemanticState, Semantics, Shadow, Srgb, StateId, StructureCx, TextRequest, Timing, Vec2,
    focus_node, number_kind,
};

use crate::control::{Control, ControlInput, ControlKind, Holds, LayoutSlot, Switched, When};
use crate::host::ViewHost;
use crate::layout_values::{alignment, layout_value, length};
use crate::scope::Scope;
use crate::shaders::encode_shader;
use crate::text_style::text_style;

/// The font size of a text whose node resolves none, in logical pixels.
const FONT_SIZE: f32 = 14.0;

/// The color of a text whose node binds no `color`.
/// The wash a dialog with no `background` dims the scene behind it with.
const SCRIM: Rgba = Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 0.32,
};

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
                shown.seeded = true;
            }
            shown
        })
        .collect();
    for node in &mut shown {
        node.deliver(cx, &mut view);
    }
    // A node that showed values before and is given no entry now shows each
    // property's default once, as a kept node's dropped entry does.
    let kept: HashSet<NodeId> = nodes.iter().map(|(node, _)| *node).collect();
    for before in &prior {
        let (false, true, Some(shows)) = (
            kept.contains(&before.node),
            cx.store.arena().is_live(before.node),
            &before.shows,
        ) else {
            continue;
        };
        let control = Control::new(before.control.kind);
        let mut reset = Shown::new(before.node, control, Scope::EMPTY, &view, cx);
        reset.shows = Some(shows.clone());
        reset.seeded = true;
        reset.deliver(cx, &mut view);
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
        look.corner_radius,
        look.border,
        look.shadow,
        semantics.label,
        semantics.hint,
        semantics.value,
        semantics.hidden,
        semantics.heading_level,
        control.node.visible,
        control.node.focusable,
        control.node.enabled,
        control.node.autofocus,
        control.node.clip,
    ]
    .into_iter()
    .chain(control.text.all())
    .chain([
        control.placeholder,
        control.secure,
        control.multiline,
        control.input_kind,
        control.max_length,
        control.step,
        control.precision,
        control.caption,
        look.translate,
        look.scale,
        look.rotation,
        look.transform_origin,
        control.node.cursor,
        control.node.refreshing,
    ])
    .flatten()
    .chain(arms)
    .chain(control.layout.iter().map(|&(_, entry)| entry))
    {
        host.entry_cells(entry, scope, out);
    }
}

/// How many values a node shows: its value, the two entries beside it (a
/// slider's bounds, an image's fit and tint), its look's background,
/// opacity, corner radius and border, its `semantics.label`, `hint`,
/// `value`, `hidden` and `heading_level`, its `visible`, `focusable`,
/// `enabled`, `autofocus` and `clip`, and its text's `color`, `soft_wrap`,
/// `font_weight`, `font_family`, `line_height`, `max_lines`, `overflow` and
/// `align`, a field's `placeholder`, `secure`, `multiline`, `input` and
/// `max_length`, a number field's `step` and `precision` and a check's
/// caption, its `translate`, `scale`, `rotation` and `transform_origin`, and
/// its `cursor`, its text's `font_style`, `spans` and `selectable`, its
/// `shadow`, and a refreshable viewport's `refreshing`.
const SHOWN: usize = 45;

/// The input of each value a node shows, in order.
const SHOWN_INPUTS: [ControlInput; SHOWN] = [
    ControlInput::Value,
    ControlInput::Min,
    ControlInput::Max,
    ControlInput::Fit,
    ControlInput::Tint,
    ControlInput::Background,
    ControlInput::Opacity,
    ControlInput::CornerRadius,
    ControlInput::Border,
    ControlInput::Label,
    ControlInput::Hint,
    ControlInput::SpokenValue,
    ControlInput::Hidden,
    ControlInput::HeadingLevel,
    ControlInput::Visible,
    ControlInput::Focusable,
    ControlInput::Enabled,
    ControlInput::Autofocus,
    ControlInput::Clip,
    ControlInput::TextColor,
    ControlInput::SoftWrap,
    ControlInput::FontWeight,
    ControlInput::FontFamily,
    ControlInput::LineHeight,
    ControlInput::MaxLines,
    ControlInput::Overflow,
    ControlInput::TextAlign,
    ControlInput::Placeholder,
    ControlInput::Secure,
    ControlInput::Multiline,
    ControlInput::InputKind,
    ControlInput::MaxLength,
    ControlInput::Step,
    ControlInput::Precision,
    ControlInput::Caption,
    ControlInput::Translate,
    ControlInput::Scale,
    ControlInput::Rotation,
    ControlInput::TransformOrigin,
    ControlInput::Cursor,
    ControlInput::FontStyle,
    ControlInput::Spans,
    ControlInput::Selectable,
    ControlInput::Shadow,
    ControlInput::Refreshing,
];

/// Where its `translate` sits among the values a node shows.
const TRANSLATE: usize = 35;

/// Where its `scale`, `rotation` and `transform_origin` sit among the values
/// a node shows.
const TURN: usize = 36;

/// Where its `cursor` sits among the values a node shows.
const CURSOR: usize = 39;

/// Where its text's `font_style` and `spans` sit among the values a node
/// shows.
const PIECES: usize = 40;

/// Where its `shadow` sits among the values a node shows.
const SHADOW: usize = 43;

/// Where a refreshable viewport's `refreshing` sits among the values a node
/// shows.
const REFRESHING: usize = 44;

/// Where its background, opacity, corner radius and border sit among the
/// values a node shows.
const LOOK: usize = 5;

/// Where its `semantics.*` values start among the values a node shows.
const SEMANTICS: usize = 9;

/// Where its `visible`, `focusable`, `enabled`, `autofocus` and `clip` sit
/// among the values a node shows.
const NODE: usize = 14;

/// Where its text's style starts among the values a node shows.
const TEXT: usize = 19;

/// Where a field's placeholder, masking, lines, kind and length, a number
/// field's step and precision and a check's caption sit among the values a
/// node shows.
const FIELD: usize = 27;

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
    /// The value, lower bound, upper bound, fit, tint, look, `semantics.*`,
    /// presence, text style, field and caption values it
    /// shows, `None` before the first delivery.
    shows: Option<[Value; SHOWN]>,
    /// Whether [`shows`](Self::shows) is what the node showed before a hot
    /// reload mounted it again: the next delivery skips the values equal to
    /// it and delivers the others as a first delivery.
    seeded: bool,
    /// The program a shader view paints with, as last delivered: a reload
    /// that changes it repaints the node even when its parameters hold.
    program: Option<Arc<BrushProgram>>,
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
        let mut entries = SHOWN_INPUTS.map(|input| self.control.entry(input));
        if !self.control.arms.is_empty() {
            for (at, &input) in SHOWN_INPUTS.iter().enumerate() {
                match self.pick(Switched::Input(input), entries[at], cx, host) {
                    Ok(entry) => entries[at] = entry,
                    Err(()) => return,
                }
            }
        }
        let control = &self.control;
        let look = control.look;
        let semantics = control.semantics;
        let mut shows: [Value; SHOWN] = std::array::from_fn(|_| Value::default());
        for (value, entry) in shows.iter_mut().zip(entries) {
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
        let prior = self.shows.as_ref();
        let first = prior.is_none() || std::mem::take(&mut self.seeded);
        if prior == Some(&shows) && !repaint {
            return;
        }
        // A look value shows when an entry or an arm gives it; with arms and no
        // entry, no arm holding shows the property's default. A value shown
        // before that a reload gives no entry shows the default too: it is
        // the one value that changes with no entry.
        let held = prior.is_some();
        // Whether the value at `at` is given, by an entry or an arm.
        let given = |at: usize| {
            let input = Switched::Input(SHOWN_INPUTS[at]);
            entries[at].is_some() || control.arms.iter().any(|a| a.target == input)
        };
        // Whether the value at `at` shows: it is given, or it showed before.
        let shown = |at: usize| held || given(at);
        let dialog = control.kind == ControlKind::Dialog;
        let shown_background = (shown(LOOK) || dialog).then_some(());
        let shown_opacity = shown(LOOK + 1).then_some(());
        let shown_radius = shown(LOOK + 2);
        let shown_border = shown(LOOK + 3);
        let [
            value,
            min,
            max,
            fit,
            tint,
            background,
            opacity,
            corner_radius,
            border,
            label,
            hint,
            spoken,
            hidden,
            level,
            visible,
            focusable,
            enabled,
            autofocus,
            clip,
            text_color,
            soft_wrap,
            font_weight,
            font_family,
            line_height,
            max_lines,
            overflow,
            align,
            placeholder,
            secure,
            multiline,
            input_kind,
            max_length,
            step,
            precision,
            caption,
            translate,
            scale,
            rotation,
            origin,
            cursor,
            font_style,
            pieces,
            selectable,
            shadow,
            refreshing,
        ] = &shows;
        let spanned = crate::text_style::spans(pieces);
        let changed = |at: usize| prior.is_none_or(|prior| prior[at] != shows[at]);
        let secure = truth(secure, false);
        if semantics.says_anything()
            && ((SEMANTICS..NODE).any(changed)
                || (FIELD..TRANSLATE).any(changed)
                || (control.kind == ControlKind::Label && (changed(0) || changed(PIECES + 1))))
        {
            let name = match (label.as_str(), control.kind) {
                (Some(label), _) => Some(label),
                (None, ControlKind::Label) => match &spanned {
                    Some((text, _)) => Some(text.as_str()),
                    None => value.as_str(),
                },
                (None, ControlKind::Check | ControlKind::Switch) => caption.as_str(),
                (None, _) => None,
            };
            let role = match semantics.role.unwrap_or_default() {
                Role::TextField if secure => Role::SecureField,
                role => role,
            };
            let said = Said {
                role,
                live: semantics.live,
                name,
                hint: hint
                    .as_str()
                    .or_else(|| placeholder.as_str().filter(|p| !p.is_empty())),
                value: spoken.as_str().filter(|_| role != Role::SecureField),
                hidden: hidden.as_int().is_some_and(|v| v != 0),
                heading_level: level.as_int().and_then(|v| u8::try_from(v).ok()),
            };
            describe_node(cx.store, self.node, &said);
        }
        // How a changed value moves in: over its transition after the first
        // delivery, else at once.
        let reduced = cx.states.env().environment().reduced_motion;
        let mut move_of = |at: usize, transition: Option<u32>| {
            let transition = transition.filter(|_| !first && changed(at))?;
            match host.evaluate(transition, &self.scope, None, &*cx.states) {
                Ok(spec) => Some(timing(&spec, reduced)),
                Err(fault) => {
                    host.record_fault(fault);
                    None
                }
            }
        };
        let moves = [
            shown_background.and_then(|_| move_of(LOOK, look.background_transition)),
            shown_opacity.and_then(|_| move_of(LOOK + 1, look.opacity_transition)),
        ];
        let radius_move = move_of(LOOK + 2, look.corner_radius_transition);
        let translate_move = move_of(TRANSLATE, look.translate_transition);
        let turn_moves = [
            move_of(TURN, look.scale_transition),
            move_of(TURN + 1, look.rotation_transition),
        ];
        let color_move = match control.kind {
            ControlKind::Label | ControlKind::TextInput | ControlKind::Number => {
                move_of(TEXT, look.color_transition.filter(|_| given(TEXT)))
            }
            _ => None,
        };
        let store = &mut *cx.store;
        // A dialog or a toast is a layer that shows while open; a dialog
        // traps the focus meanwhile, and a toast's own box lets the pointer
        // through.
        if matches!(control.kind, ControlKind::Dialog | ControlKind::Toast) {
            if first {
                store.set_layer(self.node, true);
                store.set_hittable(self.node, dialog);
            }
            if first || changed(0) || changed(NODE) {
                let open = truth(value, false) && truth(visible, true);
                store.set_hidden(self.node, !open);
                if dialog {
                    store.trap_focus(self.node, open);
                }
            }
        } else if shown(NODE) && changed(NODE) {
            store.set_hidden(self.node, !truth(visible, true));
        }
        if given(NODE + 1) && changed(NODE + 1) {
            store.set_focusable(self.node, truth(focusable, false));
        }
        if shown(NODE + 2) && changed(NODE + 2) {
            store.set_enabled(self.node, truth(enabled, true));
        }
        if shown(NODE + 4) && changed(NODE + 4) {
            store.set_clip(self.node, truth(clip, false));
        }
        if shown(CURSOR) && changed(CURSOR) {
            store.set_cursor(self.node, cursor_of(cursor));
        }
        // A bound `refreshing` makes the viewport's refreshes its owner's.
        if given(REFRESHING) && changed(REFRESHING) {
            store.set_refresh_controlled(self.node, true);
            store.set_refreshing(self.node, truth(refreshing, false));
        }
        // An autofocus node takes the focus as it mounts, unless something
        // already holds it.
        if prior.is_none() && truth(autofocus, false) && store.focused().is_none() {
            focus_node(store, self.node);
        }
        if shown_radius && changed(LOOK + 2) {
            let radius = length(corner_radius).unwrap_or_default();
            match radius_move {
                Some(timing) => store.transition_corner_radius(self.node, radius, timing),
                None => store.set_corner_radius(self.node, radius),
            }
        }
        if shown(TRANSLATE) && changed(TRANSLATE) {
            let [x, y] = offset(translate);
            store.set_translate_terms(self.node, x, y, translate_move);
        }
        if shown(TURN + 2) && changed(TURN + 2) {
            let origin = alignment(origin).unwrap_or(Alignment2D::Center);
            store.set_transform_origin(self.node, origin);
        }
        for (at, value) in [
            (TURN, LookValue::Scale(float(scale, 1.0))),
            (
                TURN + 1,
                LookValue::Rotation(float(rotation, 0.0).to_radians()),
            ),
        ] {
            if !shown(at) || !changed(at) {
                continue;
            }
            match turn_moves[at - TURN] {
                Some(timing) => store.transition(self.node, value, timing),
                None if first => store.present(self.node, value),
                None => store.transition(self.node, value, Timing::default()),
            }
        }
        if let Some(timing) = color_move {
            store.transition(self.node, LookValue::TextColor(color(text_color)), timing);
        }
        if shown_border && changed(LOOK + 3) {
            store.set_border(self.node, stroke(border));
        }
        if shown(SHADOW) && changed(SHADOW) {
            store.set_shadow(self.node, cast(shadow));
        }
        for (at, entry, value) in [
            (
                LOOK,
                shown_background,
                LookValue::Fill(match background {
                    Value::Nil if dialog => SCRIM,
                    background => color(background),
                }),
            ),
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
                (None, value) => store.present(self.node, value),
            }
        }
        let text = |shown: &Value| TextRequest {
            spans: Vec::new(),
            text: shown.as_str().unwrap_or_default().to_owned(),
            font_size: FontSize::Inherited,
            color: if given(TEXT) {
                color(text_color)
            } else {
                COLOR
            },
            soft_wrap: truth(soft_wrap, false),
            locale: None,
            style: text_style([
                font_weight,
                font_style,
                font_family,
                line_height,
                max_lines,
                overflow,
                align,
            ]),
        };
        if prior.is_some()
            && (0..LOOK)
                .chain(TEXT..TRANSLATE)
                .chain(PIECES..SHADOW)
                .all(|at| !changed(at))
            && !repaint
        {
            self.shows = Some(shows);
            return;
        }
        match control.kind {
            ControlKind::Plain | ControlKind::Dialog | ControlKind::Toast => {}
            // The game of the view's root fills the scene each frame.
            ControlKind::Viewport => {
                if let Some(slot) = store.set_scene(self.node) {
                    let rig = <viso_behavior::game::kit::CameraRig as viso_behavior::native::NativeValue>::from_value(value);
                    crate::world_scene::set_viewport_rig(store, slot, rig);
                }
            }
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
            ControlKind::Label => {
                let request = match spanned {
                    Some((shown, spans)) => TextRequest {
                        text: shown,
                        spans,
                        ..text(&Value::Nil)
                    },
                    None => text(value),
                };
                let selectable = truth(selectable, false);
                let was = store.field_marks(self.node).is_some();
                // A text takes the focus while it is selectable, unless its
                // `focusable` says.
                if selectable != was && !given(NODE + 1) {
                    store.set_focusable(self.node, selectable);
                }
                if selectable {
                    let multiline = request.soft_wrap;
                    let seed = FieldSeed {
                        request,
                        valued: true,
                        look: FieldLook {
                            multiline,
                            read_only: true,
                            ..FieldLook::default()
                        },
                    };
                    store.seed_text(self.node, seed);
                } else {
                    if was {
                        store.unmake_field(self.node);
                    }
                    store.set_text_request(self.node, request);
                }
            }
            ControlKind::TextInput => {
                let seed = FieldSeed {
                    request: text(value),
                    valued: control.value.is_some(),
                    look: FieldLook {
                        placeholder: placeholder.as_str().unwrap_or_default().to_owned(),
                        secure,
                        multiline: truth(multiline, false),
                        kind: input_kind_of(input_kind),
                        max_length: max_length.as_int().and_then(|v| u32::try_from(v).ok()),
                        filter: None,
                        number: None,
                        read_only: false,
                    },
                };
                store.seed_text(self.node, seed);
            }
            ControlKind::Number => {
                let format = number_format(min, max, step, precision);
                let number = format.settle(value.as_float().unwrap_or(format.origin()));
                let shown = Value::str(format.show(number));
                let seed = FieldSeed {
                    request: text(&shown),
                    valued: true,
                    look: FieldLook {
                        kind: number_kind(&format),
                        number: Some(format),
                        ..FieldLook::default()
                    },
                };
                store.seed_text(self.node, seed);
                store.set_semantic_state(
                    self.node,
                    SemanticState::number(number, format.min, format.max),
                );
            }
            ControlKind::Check | ControlKind::Switch => {
                let checked = value.as_int().is_some_and(|v| v != 0);
                store.set_semantic_state(self.node, SemanticState::checked(checked));
                let shape = match control.kind {
                    ControlKind::Switch => CheckShape::Switch,
                    _ => CheckShape::Box,
                };
                store.set_check_mark(self.node, Some(CheckMark { shape, checked }));
                // A check with no caption still shapes an empty one, which
                // sizes and inks its mark.
                if first || (given(FIELD + 7) && changed(FIELD + 7)) {
                    store.set_text_request(self.node, text(caption));
                }
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
            ControlKind::Disclosure => {
                let expanded = value.as_int().is_some_and(|v| v != 0);
                store.set_folded(self.node, !expanded);
                if let Some(header) = store.arena().first_child(self.node) {
                    let state = SemanticState {
                        expanded: Some(expanded),
                        ..SemanticState::default()
                    };
                    store.set_semantic_state(header, state);
                }
            }
        }
        self.shows = Some(shows);
        self.program = program;
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
        for &(slot, base) in self.control.layout.iter() {
            let Ok(Some(entry)) = self.pick(Switched::Layout(slot), Some(base), cx, host) else {
                return false;
            };
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
        let reduced = cx.states.env().environment().reduced_motion;
        let look = self.control.look;
        for (at, &(slot, _)) in self.control.layout.iter().enumerate() {
            if self
                .laid
                .as_ref()
                .is_some_and(|prior| prior[at] == laid[at])
            {
                continue;
            }
            // A fixed size moves in over its transition after the first
            // delivery.
            let transition = match slot {
                LayoutSlot::Width => look.width_transition,
                LayoutSlot::Height => look.height_transition,
                _ => None,
            }
            .filter(|_| self.laid.is_some());
            let timing = match transition.map(|t| host.evaluate(t, &self.scope, None, &*cx.states))
            {
                Some(Ok(spec)) => Some(timing(&spec, reduced)),
                Some(Err(fault)) => {
                    host.record_fault(fault);
                    None
                }
                None => None,
            };
            match (layout_value(slot, &laid[at], &env, font), timing) {
                (Ok(LayoutValue::Width(BoundSize::Fixed(terms))), Some(timing)) => {
                    cx.store.transition_size(self.node, true, terms, timing)
                }
                (Ok(LayoutValue::Height(BoundSize::Fixed(terms))), Some(timing)) => {
                    cx.store.transition_size(self.node, false, terms, timing)
                }
                (Ok(value), _) => cx.store.set_layout_value(self.node, value),
                (Err(message), _) => host.record_fault(Fault {
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
        target: Switched,
        base: Option<u32>,
        cx: &StructureCx<'_>,
        host: &mut ViewHost,
    ) -> Result<Option<u32>, ()> {
        for arm in self.control.arms.iter().filter(|arm| arm.target == target) {
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

/// How a number field bounded by `min` and `max`, stepping by `step` and
/// showing `precision` places reads and shows its number: an absent bound
/// is none, an absent step `1`, an absent precision as many places as the
/// step has.
pub(crate) fn number_format(
    min: &Value,
    max: &Value,
    step: &Value,
    precision: &Value,
) -> NumberFormat {
    let format = NumberFormat::new(
        min.as_float().unwrap_or(f64::NEG_INFINITY),
        max.as_float().unwrap_or(f64::INFINITY),
        step.as_float().unwrap_or(1.0),
    );
    match precision.as_int().and_then(|p| u8::try_from(p).ok()) {
        Some(places) => format.with_precision(places),
        None => format,
    }
}

/// An `InputKind` value as the kind it names, `text` for any other.
fn input_kind_of(value: &Value) -> InputKind {
    match value.as_int() {
        Some(1) => InputKind::Digits,
        Some(2) => InputKind::Integer,
        Some(3) => InputKind::Decimal,
        Some(4) => InputKind::Phone,
        Some(5) => InputKind::Email,
        Some(6) => InputKind::Url,
        Some(7) => InputKind::Search,
        _ => InputKind::Text,
    }
}

/// A `Cursor` value as the pointer shape it names, none for any other.
fn cursor_of(value: &Value) -> Option<CursorIcon> {
    const CURSORS: [CursorIcon; 24] = [
        CursorIcon::Default,
        CursorIcon::Pointer,
        CursorIcon::Text,
        CursorIcon::VerticalText,
        CursorIcon::Crosshair,
        CursorIcon::Move,
        CursorIcon::Grab,
        CursorIcon::Grabbing,
        CursorIcon::NotAllowed,
        CursorIcon::Wait,
        CursorIcon::Progress,
        CursorIcon::Help,
        CursorIcon::ContextMenu,
        CursorIcon::Copy,
        CursorIcon::Alias,
        CursorIcon::ZoomIn,
        CursorIcon::ZoomOut,
        CursorIcon::ResizeEw,
        CursorIcon::ResizeNs,
        CursorIcon::ResizeNesw,
        CursorIcon::ResizeNwse,
        CursorIcon::ResizeCol,
        CursorIcon::ResizeRow,
        CursorIcon::Hidden,
    ];
    let at = usize::try_from(value.as_int()?).ok()?;
    CURSORS.get(at).copied()
}

/// An `ImageFit` value as the fit it names, `contain` for any other.
fn asset_fit(value: &Value) -> AssetFit {
    match value.as_int() {
        Some(1) => AssetFit::Cover,
        Some(2) => AssetFit::Fill,
        _ => AssetFit::Contain,
    }
}

/// A `Border` value as the stroke it draws, none for `None`.
fn stroke(value: &Value) -> Border {
    let Value::Agg(border) = value else {
        return Border::NONE;
    };
    match &border.fields[..] {
        [width, paint] => Border {
            width: dp(width).max(0.0),
            color: color(paint),
        },
        _ => Border::NONE,
    }
}

/// A `Shadow` value as the shadow the node casts, none for `None`.
fn cast(value: &Value) -> Option<Shadow> {
    let Value::Agg(shadow) = value else {
        return None;
    };
    match &shadow.fields[..] {
        [x, y, blur, spread, paint, inset] => Some(Shadow {
            color: color(paint),
            offset: Vec2 { x: dp(x), y: dp(y) },
            blur: dp(blur).max(0.0),
            spread: dp(spread),
            inset: truth(inset, false),
        }),
        _ => None,
    }
}

/// A `Dp` value in dp, zero for none.
fn dp(value: &Value) -> f32 {
    value
        .as_float()
        .or_else(|| length(value).map(|terms| f64::from(terms.dp)))
        .map_or(0.0, |v| v as f32)
}

/// A `Color` value (sRGB `0xRRGGBBAA`) as a fill, transparent for `None`.
pub(crate) fn color(value: &Value) -> Rgba {
    match value.as_int() {
        Some(rgba) => Srgb::from_rgba32(rgba as u32).into_linear_straight(),
        None => Rgba::TRANSPARENT,
    }
}

/// The timing of a `Transition` value (`duration`, `delay`, `easing`,
/// `reduced`): instant when `reduced_motion` holds and its `reduced` is
/// `instant`. A negative or non-finite time is zero.
pub(crate) fn timing(spec: &Value, reduced_motion: bool) -> Timing {
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

/// A `Bool` value, `default` when it is none.
fn truth(value: &Value, default: bool) -> bool {
    value.as_int().map_or(default, |v| v != 0)
}

/// An `Offset`'s `x` and `y` terms; zero for a value that is none.
fn offset(value: &Value) -> [LengthTerms; 2] {
    let Value::Agg(agg) = value else {
        return [LengthTerms::default(); 2];
    };
    let term = |at: usize| agg.fields.get(at).and_then(length).unwrap_or_default();
    [term(0), term(1)]
}

/// A `Float` value as `f32`, `default` for any other.
fn float(value: &Value, default: f32) -> f32 {
    value.as_float().map_or(default, |v| v as f32)
}
