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
//! a node shows shows at once, as the store shows every value under reduced
//! motion with an `instant` transition. The cells a transition reads are no
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
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Duration;

use viso_behavior::{Fault, FaultKind, Value};
use viso_schema::view::route::EventRoute;
use viso_ui::color_picker::{
    ColorPickerParts, ColorPickerStyle, DEFAULT_COLOR, PickSink, PickerSpec,
};
use viso_ui::color_plane::rgba32;
use viso_ui::combo_box::{ComboOption, ComboParts, ComboSpec, Commit, Pick};
use viso_ui::content::Content;
use viso_ui::plate::{BADGE_FILL, BADGE_INK, avatar_fill, badge_count, initials};
use viso_ui::resource::{ResourceId, ResourceRevision};
use viso_ui::search_field::{Report as SearchReport, SearchParts, SearchSpec};
use viso_ui::segments::Segments;
use viso_ui::shader::BrushProgram;
use viso_ui::slider::{Slider as SliderMark, SliderRange, SliderShape};
use viso_ui::{Alignment2D, BoundSize, FontWeight, LayoutValue, LengthTerms};
use viso_ui::{
    AssetFit, Border, BuildCx, CheckMark, CheckShape, CursorIcon, Easing, FieldLook, FieldSeed,
    FontSize, InputKind, LiveRegion, LookValue, NodeId, NodeStore, NumberFormat, PageTransition,
    ProgressShape, Rgba, Role, SemanticState, Semantics, Shadow, SpanStyle, Spinner, SpinnerLook,
    Srgb, StateId, StructureCx, TextRequest, TextSpan, Timing, Vec2, focus_node, number_kind,
};

use crate::control::{
    Control, ControlInput, ControlKind, Holds, LayoutSlot, Switched, When, field,
};
use crate::host::ViewHost;
use crate::layout_values::{alignment, layout_value, length};
use crate::scope::Scope;
use crate::shaders::encode_shader;
use crate::text_style::text_style;

/// The font size of a text whose node resolves none, in logical pixels.
const FONT_SIZE: f32 = 14.0;

/// The color of a text whose node binds no `color`.
/// The wash a dialog or a sheet with no `background` dims the scene behind
/// it with.
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

/// The ink of a link label that binds no `color`: the system link blue.
const LINK: Rgba = Rgba {
    r: 0.0,
    g: 0.133,
    b: 0.604,
    a: 1.0,
};

/// The fill of a progress indicator or spinner that binds no `tint`.
const ACCENT: Rgba = Rgba {
    r: 0.22,
    g: 0.45,
    b: 0.85,
    a: 1.0,
};

/// The size a progress bar, a progress ring and a spinner measure to where
/// their layout gives none, in logical pixels.
const BAR: Vec2 = Vec2 { x: 160.0, y: 4.0 };
const RING: Vec2 = Vec2 { x: 32.0, y: 32.0 };

/// A slider's track and thumb.
const SLIDER_TRACK: Rgba = Rgba {
    r: 0.47,
    g: 0.47,
    b: 0.5,
    a: 0.32,
};
const SLIDER_THUMB: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const SPINNER: f32 = 20.0;

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
                shown.parts = before.parts.clone();
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
        control.node.sticky,
        control.page_transition,
        control.node.hero,
        control.node.focus_ring,
        control.node.access_key,
        control.node.drag_axis,
        control.pan_margin,
        control.node.drag,
        control.node.drag_kind,
        control.node.drop_accepts,
        control.node.drop_operation,
        control.url,
        control.removable,
        control.wrap,
        control.fill_origin,
        control.marks,
        control.axis,
        control.alpha,
        control.wheel,
        control.options,
        control.free_text,
        control.empty_text,
        control.debounce,
        control.animate,
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
/// `shadow`, a refreshable viewport's `refreshing`, its `sticky`, and a
/// navigation stack's `page_transition`, its `hero`, its `focus_ring`, its
/// `access_key`, its `drag_axis`, a viewer's `pan_margin`, and its `drag`,
/// `drag_kind`, `drop_accepts` and `drop_operation`, a link label's `url`
/// a chip's `removable`, a stepper's `wrap`, a slider's `origin`, `marks`
/// and `axis`, a colour picker's `alpha` and `wheel`, and a combo box's
/// `options`, `free_text` and `empty_text` — a search field's `suggestions`
/// are its `options` — a search field's `debounce`, and an image's
/// `animate`.
const SHOWN: usize = 69;

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
    ControlInput::Sticky,
    ControlInput::PageTransition,
    ControlInput::Hero,
    ControlInput::FocusRing,
    ControlInput::AccessKey,
    ControlInput::DragAxis,
    ControlInput::PanMargin,
    ControlInput::Drag,
    ControlInput::DragKind,
    ControlInput::DropAccepts,
    ControlInput::DropOperation,
    ControlInput::Url,
    ControlInput::Removable,
    ControlInput::Wrap,
    ControlInput::FillOrigin,
    ControlInput::Marks,
    ControlInput::Axis,
    ControlInput::Alpha,
    ControlInput::Wheel,
    ControlInput::Options,
    ControlInput::FreeText,
    ControlInput::EmptyText,
    ControlInput::Debounce,
    ControlInput::Animate,
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

/// Where its `sticky` sits among the values a node shows.
const STICKY: usize = 45;

/// Where its `hero` sits among the values a node shows.
const HERO: usize = 47;

/// Where its `focus_ring` sits among the values a node shows.
const FOCUS_RING: usize = 48;

/// Where its `drag_axis` sits among the values a node shows.
const DRAG_AXIS: usize = 50;

/// Where its `access_key` sits among the values a node shows.
const ACCESS_KEY: usize = 49;

/// Where a viewer's `pan_margin` sits among the values a node shows.
const PAN_MARGIN: usize = 51;

/// Where a link label's `url` sits among the values a node shows.
const URL: usize = 56;

/// Where a chip's `removable` sits among the values a node shows.
const REMOVABLE: usize = 57;

/// Where a stepper's `wrap` sits among the values a node shows.
const WRAP: usize = 58;

/// Where a slider's `origin`, `marks` and `axis` sit among the values a
/// node shows.
const SLIDE: usize = 59;

/// Where a combo box's `options`, `free_text` and `empty_text` and a search
/// field's `debounce` sit among the values a node shows.
const LISTED: usize = 64;

/// Where its `drag`, `drag_kind`, `drop_accepts` and `drop_operation` sit
/// among the values a node shows.
const CARRY: usize = 52;

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
    /// The parts the framework builds inside it, once built.
    parts: Option<Parts>,
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
            parts: None,
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
        let sheet = control.kind == ControlKind::Sheet;
        let shown_background = (shown(LOOK) || dialog || sheet).then_some(());
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
            sticky,
            paging,
            hero,
            focus_ring,
            access_key,
            drag_axis,
            pan_margin,
            drag,
            drag_kind,
            drop_accepts,
            drop_operation,
            url,
            removable,
            wrap,
            fill_origin,
            marks,
            axis,
            alpha,
            wheel,
            options,
            free_text,
            empty_text,
            debounce,
            animate,
        ] = &shows;
        let access_key = access_key.as_str().and_then(|key| key.chars().next());
        let spanned = crate::text_style::spans(pieces);
        let changed = |at: usize| prior.is_none_or(|prior| prior[at] != shows[at]);
        let secure = truth(secure, false);
        if semantics.says_anything()
            && ((SEMANTICS..NODE).any(changed)
                || (FIELD..TRANSLATE).any(changed)
                || (matches!(control.kind, ControlKind::Label | ControlKind::Button)
                    && (changed(0) || changed(PIECES + 1)))
                || (control.kind == ControlKind::Badge && (changed(0) || changed(2))))
        {
            let counted = (control.kind == ControlKind::Badge)
                .then(|| {
                    value
                        .as_int()
                        .map(|c| badge_count(c, max.as_int().unwrap_or(99)))
                })
                .flatten();
            let name = match (label.as_str(), control.kind) {
                (Some(label), _) => Some(label),
                (None, ControlKind::Label | ControlKind::Button) => match &spanned {
                    Some((text, _)) => Some(text.as_str()),
                    None => value.as_str(),
                },
                (None, ControlKind::Badge) => caption
                    .as_str()
                    .filter(|c| !c.is_empty())
                    .or(counted.as_deref()),
                (
                    None,
                    ControlKind::Check
                    | ControlKind::Switch
                    | ControlKind::Chip
                    | ControlKind::Stepper
                    | ControlKind::Progress
                    | ControlKind::Ring
                    | ControlKind::Spinner
                    | ControlKind::Avatar
                    | ControlKind::ColorPicker,
                ) => caption.as_str(),
                (None, _) => None,
            };
            let role = match semantics.role.unwrap_or_default() {
                Role::TextField if secure => Role::SecureField,
                // A chip that does not select is a button.
                Role::CheckBox if control.kind == ControlKind::Chip && control.value.is_none() => {
                    Role::Button
                }
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
        let mut move_of = |at: usize, transition: Option<u32>| {
            let transition = transition.filter(|_| !first && changed(at))?;
            match host.evaluate(transition, &self.scope, None, &*cx.states) {
                Ok(spec) => Some(timing(&spec)),
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
            ControlKind::Label
            | ControlKind::Button
            | ControlKind::TextInput
            | ControlKind::Number => move_of(TEXT, look.color_transition.filter(|_| given(TEXT))),
            _ => None,
        };
        match control.kind {
            ControlKind::ColorPicker => {
                let (alpha, wheel) = (truth(alpha, true), truth(wheel, false));
                show_picker(&mut self.parts, self.node, cx, host, value, alpha, wheel);
            }
            ControlKind::ComboBox => {
                // Its parts are built again when what they are built from
                // changes.
                let stale = [FIELD, FIELD + 7]
                    .into_iter()
                    .chain(LISTED..LISTED + 3)
                    .any(changed);
                let spec = ComboSpec {
                    options: labels(options).into_iter().map(ComboOption::new).collect(),
                    free: truth(free_text, false),
                    placeholder: placeholder.as_str().unwrap_or_default().to_owned(),
                    empty: empty_text
                        .as_str()
                        .map_or_else(|| ComboSpec::default().empty, str::to_owned),
                    label: caption.as_str().map(str::to_owned),
                    ..ComboSpec::default()
                };
                show_combo(&mut self.parts, self.node, cx, host, spec, stale, value);
            }
            ControlKind::SearchField => {
                let stale = [FIELD, FIELD + 7, LISTED + 3].into_iter().any(changed);
                let spec = SearchSpec {
                    placeholder: placeholder
                        .as_str()
                        .map_or_else(|| SearchSpec::default().placeholder, str::to_owned),
                    label: caption.as_str().map(str::to_owned),
                    debounce: debounce
                        .as_float()
                        .and_then(|s| Duration::try_from_secs_f64(s).ok())
                        .unwrap_or_default(),
                    ..SearchSpec::default()
                };
                let suggestions = changed(LISTED).then(|| labels(options));
                show_search(
                    &mut self.parts,
                    self.node,
                    cx,
                    host,
                    spec,
                    stale,
                    value,
                    suggestions,
                );
            }
            _ => {}
        }
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
        } else if sheet {
            // A sheet heads for its detent, and springs in while open and
            // out once not.
            if (first || changed(1))
                && let Some(detent) = min.as_int().and_then(|v| usize::try_from(v).ok())
            {
                store.set_sheet_detent(self.node, detent);
            }
            if first || changed(0) || changed(NODE) {
                let open = truth(value, false) && truth(visible, true);
                store.present_sheet(self.node, open);
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
        // A list row's root heads a section through its list instead.
        if shown(STICKY)
            && changed(STICKY)
            && !store
                .arena()
                .parent(self.node)
                .is_some_and(|host| store.is_positioned_row(host))
        {
            store.set_sticky(self.node, truth(sticky, false));
        }
        if shown(HERO) && changed(HERO) {
            store.set_hero(self.node, hero.as_str());
        }
        if shown(FOCUS_RING) && changed(FOCUS_RING) {
            store.set_focus_ring(self.node, truth(focus_ring, true));
        }
        if shown(ACCESS_KEY) && changed(ACCESS_KEY) {
            store.set_access_key(self.node, access_key);
        }
        if shown(DRAG_AXIS) && changed(DRAG_AXIS) {
            let axis = match drag_axis {
                Value::Int(0) => Some(viso_ui::Axis::Row),
                Value::Int(_) => Some(viso_ui::Axis::Column),
                _ => None,
            };
            store.set_drag_axis(self.node, axis);
        }
        // A node carrying text is a drag source; one listing kinds, a drop
        // target taking them.
        if (shown(CARRY) || shown(CARRY + 1)) && (changed(CARRY) || changed(CARRY + 1)) {
            store.set_drag_source(self.node, drag_data(drag, drag_kind));
        }
        if (shown(CARRY + 2) || shown(CARRY + 3)) && (changed(CARRY + 2) || changed(CARRY + 3)) {
            store.set_drop_target(self.node, drop_target(drop_accepts, drop_operation));
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
                    Value::Nil if dialog || sheet => SCRIM,
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
            // A sheet's background is its scrim, dimmed as it shows.
            if let (true, LookValue::Fill(fill)) = (sheet, value) {
                store.set_sheet_scrim(self.node, fill);
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
                .chain([ACCESS_KEY, PAN_MARGIN, URL, REMOVABLE, WRAP])
                .chain(SLIDE..SHOWN)
                .all(|at| !changed(at))
            && !repaint
        {
            self.shows = Some(shows);
            return;
        }
        match control.kind {
            ControlKind::Plain
            | ControlKind::Dialog
            | ControlKind::Sheet
            | ControlKind::Toast
            | ControlKind::ColorPicker
            | ControlKind::ComboBox
            | ControlKind::SearchField => {}
            // A viewer clips its content unless its `clip` says, and keeps
            // the content within its limits as they change.
            ControlKind::Viewer => {
                if first && !given(NODE + 4) {
                    store.set_clip(self.node, true);
                }
                let limits = viso_ui::ViewerLimits {
                    min_scale: float(min, 1.0),
                    max_scale: float(max, 4.0),
                    margin: dp(pan_margin),
                };
                store.set_viewer(self.node, Some(limits));
            }
            // A swipe row takes its panels among its children, clipping them
            // past its edges unless its `clip` says; a full swipe runs on
            // the edges its `full_swipe` names, the trailing one unless set.
            ControlKind::SwipeRow => {
                if first && !given(NODE + 4) {
                    store.set_clip(self.node, true);
                }
                let full = value.as_int().unwrap_or(2);
                store.set_swipe_row_of_children(self.node, Some((full & 1 != 0, full & 2 != 0)));
            }
            // A preview is a layer its parent, a drag source, shows as it is
            // carried.
            ControlKind::DragPreview => {
                if first {
                    store.set_layer(self.node, true);
                    if let Some(source) = store.arena().parent(self.node) {
                        store.set_drag_preview(source, Some(self.node));
                    }
                }
            }
            // A row's panel of actions waits past the edge it names.
            ControlKind::SwipeActions => {
                let edge = match value.as_int() {
                    Some(0) => viso_ui::SwipeEdge::Leading,
                    _ => viso_ui::SwipeEdge::Trailing,
                };
                store.set_swipe_panel(self.node, Some(edge));
            }
            // A handle carries the reorderable row it lies in, the pointer
            // showing a grab over it unless its `cursor` says.
            ControlKind::ReorderHandle => {
                if first && !given(CURSOR) {
                    store.set_cursor(self.node, Some(CursorIcon::Grab));
                }
                store.set_reorder_handle(self.node, true);
            }
            // A navigation stack shows its current page, clipping the pages
            // as they trade places unless its `clip` says.
            ControlKind::Pages => {
                if first && !given(NODE + 4) {
                    store.set_clip(self.node, true);
                }
                let page = value.as_int().and_then(|v| usize::try_from(v).ok());
                store.set_page(self.node, page, page_transition(paging));
            }
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
                    store.set_asset_still(self.node, !truth(animate, true));
                }
            }
            ControlKind::Label | ControlKind::Button => {
                // A link label's text is one link, in the link blue.
                let linked = url.as_str().filter(|url| !url.is_empty()).map(|url| {
                    let shown = value.as_str().unwrap_or_default().to_owned();
                    let mut style = SpanStyle::default().link(url);
                    if !given(TEXT) {
                        style = style.color(LINK);
                    }
                    let span = TextSpan {
                        range: 0..shown.len(),
                        style,
                    };
                    (shown, vec![span])
                });
                let mut request = match linked.or(spanned) {
                    Some((shown, spans)) => TextRequest {
                        text: shown,
                        spans,
                        ..text(&Value::Nil)
                    },
                    None => text(value),
                };
                // A text shows its access key's cue.
                if let Some(key) = access_key {
                    request.cue(key);
                }
                // A radio group's option that is one caption draws the circle
                // that fills with its selection, and a segmented control's is
                // a segment centred over its thumb; a composed one draws its
                // own.
                if first
                    && store.arena().first_child(self.node).is_none()
                    && store.check_mark(self.node).is_none()
                    && store
                        .semantics(self.node)
                        .is_some_and(|s| s.role == Role::Radio)
                {
                    let checked = store
                        .semantic_state(self.node)
                        .and_then(|s| s.checked.or(s.selected))
                        .unwrap_or(false);
                    let segment = store
                        .arena()
                        .parent(self.node)
                        .is_some_and(|parent| store.segments(parent).is_some());
                    let shape = if segment {
                        CheckShape::Segment
                    } else {
                        CheckShape::Radio
                    };
                    store.set_check_mark(self.node, Some(CheckMark { shape, checked }));
                }
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
                if first || (given(FIELD + 7) && changed(FIELD + 7)) || changed(ACCESS_KEY) {
                    let mut request = text(caption);
                    if let Some(key) = access_key {
                        request.cue(key);
                    }
                    store.set_text_request(self.node, request);
                }
            }
            // A chip shows its caption on its body, selected or not when it
            // selects, with a cross when removable.
            ControlKind::Chip => {
                let checked = value.as_int().is_some_and(|v| v != 0);
                if control.value.is_some() {
                    store.set_semantic_state(self.node, SemanticState::checked(checked));
                }
                let shape = CheckShape::Chip {
                    removable: truth(removable, false),
                };
                store.set_check_mark(self.node, Some(CheckMark { shape, checked }));
                if first || (given(FIELD + 7) && changed(FIELD + 7)) || changed(ACCESS_KEY) {
                    let mut request = text(caption);
                    if let Some(key) = access_key {
                        request.cue(key);
                    }
                    store.set_text_request(self.node, request);
                }
            }
            // A stepper inked its tint, or the text color, dims a half that
            // can step no further and reports its number within its bounds.
            ControlKind::Stepper => {
                if first || changed(4) {
                    let ink = tint.as_int().map_or(COLOR, |_| color(tint));
                    let fill = Rgba {
                        a: ink.a * 0.08,
                        ..ink
                    };
                    let stepper = viso_ui::stepper::Stepper::new(ink, fill);
                    store.set_content_payload(self.node, Content::Stepper(stepper));
                }
                let at = value.as_float().unwrap_or(0.0);
                let (lo, hi) = (min.as_float(), max.as_float());
                let (lower, upper) = viso_ui::stepper::can_step(at, lo, hi, truth(wrap, false));
                store.show_stepper(self.node, lower, upper);
                let state = SemanticState::number(
                    at,
                    lo.unwrap_or(f64::NEG_INFINITY),
                    hi.unwrap_or(f64::INFINITY),
                );
                store.set_semantic_state(self.node, state);
            }
            // A slider fills its tint, or the accent, from its start or its
            // origin, along its axis or round a knob, its thumb easing to
            // its value.
            ControlKind::Slider | ControlKind::Knob => {
                let (lo, hi) = (float(min, 0.0), float(max, 1.0));
                let grid = SliderRange::new(
                    f64::from(lo),
                    f64::from(hi),
                    step.as_float().filter(|s| *s > 0.0),
                );
                let at = f64::from(float(value, lo));
                let shape = match (control.kind, axis) {
                    (ControlKind::Knob, _) => SliderShape::Knob,
                    (_, Value::Int(1)) => SliderShape::Bar(viso_ui::Axis::Column),
                    _ => SliderShape::Bar(viso_ui::Axis::Row),
                };
                let fill = tint.as_int().map_or(ACCENT, |_| color(tint));
                let slider =
                    SliderMark::new(shape, SLIDER_TRACK, fill, SLIDER_THUMB, grid.fraction(at))
                        .with_origin(fill_origin.as_float().map(|o| grid.fraction(o)))
                        .with_marks(if truth(marks, false) { grid.steps() } else { 0 });
                // A new slider stands at its value; one shown eases there.
                store.set_slider(self.node, slider);
                let state = SemanticState {
                    value: Some(at as f32),
                    range: Some((lo, hi)),
                    ..SemanticState::default()
                };
                store.set_semantic_state(self.node, state);
            }
            // A progress indicator with no value runs; one with a value
            // shows its fraction of `min..=max`.
            ControlKind::Progress | ControlKind::Ring => {
                let (lo, hi) = (float(min, 0.0), float(max, 1.0));
                let at = value
                    .as_float()
                    .map(|v| (v as f32).clamp(lo.min(hi), hi.max(lo)));
                let state = SemanticState {
                    value: at,
                    range: at.map(|_| (lo, hi)),
                    ..SemanticState::default()
                };
                store.set_semantic_state(self.node, state);
                let fraction = at.map(|v| if hi > lo { (v - lo) / (hi - lo) } else { 0.0 });
                let (shape, natural) = match control.kind {
                    ControlKind::Progress => (ProgressShape::Bar, BAR),
                    _ => (ProgressShape::Ring, RING),
                };
                let ink = tint.as_int().map_or(ACCENT, |_| color(tint));
                store.set_progress(self.node, shape, natural, ink, fraction);
            }
            // A spinner turns while it shows.
            ControlKind::Spinner => {
                let ink = tint.as_int().map_or(ACCENT, |_| color(tint));
                if store.spinner(self.node).is_none() {
                    let spinner = Spinner::new(SpinnerLook::native(), SPINNER, ink);
                    store.set_content_payload(self.node, Content::Spinner(spinner));
                    store.set_transform_origin(self.node, Alignment2D::Center);
                    store.set_spinning(self.node, true);
                } else {
                    store.set_spinner_color(self.node, ink);
                }
            }
            // A badge shows its count up to its most centred on its pill,
            // white on the error red unless styled; with no count, a dot.
            ControlKind::Badge => {
                let shown = value
                    .as_int()
                    .map_or_else(String::new, |c| badge_count(c, max.as_int().unwrap_or(99)));
                let mut request = text(&Value::str(shown));
                if !given(TEXT) {
                    request.color = BADGE_INK;
                }
                if !given(TEXT + 2) {
                    request.style.weight = FontWeight(500);
                }
                store.set_text_request(self.node, request);
                plate(store, self.node, first && !given(LOOK + 2));
                if (first || changed(4)) && !given(LOOK) {
                    let fill = tint.as_int().map_or(BADGE_FILL, |_| color(tint));
                    store.set_fill(self.node, fill);
                }
            }
            // An avatar shows its picture cropped to its circle, or its
            // name's initials, white on the color the name picks unless
            // styled.
            ControlKind::Avatar => {
                let name = caption.as_str().unwrap_or_default();
                if (first || changed(4) || changed(FIELD + 7)) && !given(LOOK) {
                    let fill = tint
                        .as_int()
                        .map_or_else(|| avatar_fill(name), |_| color(tint));
                    store.set_fill(self.node, fill);
                }
                match resource(value) {
                    Some(picture) => {
                        offer(store, host, picture);
                        store.set_check_mark(self.node, None);
                        store.set_asset(self.node, picture, AssetFit::Cover, None);
                        if first && !given(LOOK + 2) {
                            store.set_corner_radius(self.node, ROUND);
                        }
                    }
                    None => {
                        let mut request = text(&Value::str(initials(name)));
                        if !given(TEXT) {
                            request.color = BADGE_INK;
                        }
                        if !given(TEXT + 2) {
                            request.style.weight = FontWeight(500);
                        }
                        store.set_text_request(self.node, request);
                        plate(store, self.node, first && !given(LOOK + 2));
                    }
                }
            }
            ControlKind::Select => select(store, self.node, value.as_int().unwrap_or(0)),
            // A segmented control's track, inked its tint or the text color,
            // draws the thumb under its selected segment; its caption items
            // are segments, a composed one draws its own.
            ControlKind::Segmented => {
                let at = value.as_int().unwrap_or(0);
                if first || changed(4) {
                    let ink = tint.as_int().map_or(COLOR, |_| color(tint));
                    let faded = |a: f32| Rgba {
                        a: ink.a * a,
                        ..ink
                    };
                    let shown = u32::try_from(at).ok();
                    let segments = Segments::new(faded(0.24), faded(0.2), shown);
                    store.set_content_payload(self.node, Content::Segments(segments));
                    if !given(LOOK) {
                        store.set_fill(self.node, faded(0.1));
                    }
                    if first && !given(LOOK + 2) {
                        store.set_corner_radius(self.node, SEGMENTED_RADIUS);
                    }
                    let mut child = store.arena().first_child(self.node);
                    while let Some(id) = child {
                        child = store.arena().next_sibling(id);
                        if let Some(mark) = store.check_mark(id)
                            && mark.shape == CheckShape::Radio
                        {
                            let segment = CheckMark {
                                shape: CheckShape::Segment,
                                ..mark
                            };
                            store.set_check_mark(id, Some(segment));
                        }
                    }
                }
                select(store, self.node, at);
                store.select_segment(self.node, u32::try_from(at).ok());
            }
            ControlKind::Disclosure => {
                let expanded = value.as_int().is_some_and(|v| v != 0);
                store.set_folded(self.node, !expanded);
                if let Some(header) = store.arena().first_child(self.node) {
                    // A header that is one caption draws the chevron that
                    // turns with it; a composed one, or a check, draws its own.
                    let own = store
                        .check_mark(header)
                        .is_some_and(|mark| mark.shape != CheckShape::Chevron);
                    if first && !own {
                        let caption = store.arena().first_child(header).is_none();
                        let chevron = CheckMark {
                            shape: CheckShape::Chevron,
                            checked: expanded,
                        };
                        store.set_check_mark(header, caption.then_some(chevron));
                    }
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
                Some(Ok(spec)) => Some(timing(&spec)),
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

/// A corner radius of half the shorter side: a pill, or a circle.
const ROUND: LengthTerms = LengthTerms {
    pct: 0.5,
    ..LengthTerms::ZERO
};

/// A segmented control's track's corner radius unless styled.
const SEGMENTED_RADIUS: LengthTerms = LengthTerms {
    dp: 8.0,
    ..LengthTerms::ZERO
};

/// Makes `node` a plate, its caption centred on its background, rounding it
/// to a pill when `round`.
fn plate(store: &mut NodeStore, node: NodeId, round: bool) {
    let plate = CheckMark {
        shape: CheckShape::Plate,
        checked: false,
    };
    store.set_check_mark(node, Some(plate));
    if round {
        store.set_corner_radius(node, ROUND);
    }
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

/// A colour picker builds its parts inside node `node` the first time it
/// shows, or again once they are gone, takes an opacity as `alpha` says and
/// a wheel as `wheel` says, and shows its `value` — a packed sRGB colour —
/// unless the colour it holds already reads as it. Each colour picked runs
/// the node's `changed` routes in `host`.
fn show_picker(
    parts: &mut Option<Parts>,
    node: NodeId,
    cx: &mut StructureCx<'_>,
    host: &mut ViewHost,
    value: &Value,
    alpha: bool,
    wheel: bool,
) {
    let held = value.as_int().map(|c| c as u32);
    let at = |c: u32| Srgb::from_rgba32(c).into_linear_straight();
    let built = match parts.as_ref().filter(|p| p.live(node, cx.store)) {
        Some(Parts::Picker(built)) => {
            built.configure(cx, alpha, wheel);
            built.clone()
        }
        _ => {
            host.prune_sinks(|n| cx.store.arena().is_live(n));
            let this = host.this.clone();
            let sink: PickSink = Box::new(move |ev, color| {
                let Some(host) = this.upgrade() else { return };
                let Ok(mut host) = host.try_borrow_mut() else {
                    return;
                };
                let payload = field(Value::Int(i64::from(rgba32(color))));
                host.fire(node, EventRoute::Changed, payload, ev);
            });
            // Unsized, it is the picker's own width; sized, its parts fill
            // it, the plane as tall as a fixed width asks.
            let mut style = ColorPickerStyle::default();
            let fill = match cx.store.layout_input(node).size().width {
                viso_ui::Length::Fixed(width) => {
                    style.width = width;
                    true
                }
                viso_ui::Length::Fit => false,
                _ => true,
            };
            let spec = PickerSpec {
                value: held.map_or(DEFAULT_COLOR.to_rgba(1.0), at),
                alpha,
                wheel,
                fill,
                style,
                ..PickerSpec::default()
            };
            let mut build = BuildCx::with_parent(cx.store, cx.states, cx.bindings, node);
            let built = ColorPickerParts::build(&mut build, spec, Some(sink));
            *parts = Some(Parts::Picker(built.clone()));
            built
        }
    };
    if let Some(held) = held
        && rgba32(built.color(cx)) != held
    {
        built.show_color(cx, at(held));
    }
}

/// The parts the framework builds inside a node.
#[derive(Clone)]
enum Parts {
    Picker(ColorPickerParts),
    Combo(ComboParts),
    Search(SearchParts),
}

impl Parts {
    /// Their column.
    fn root(&self) -> NodeId {
        match self {
            Parts::Picker(parts) => parts.column().id(),
            Parts::Combo(parts) => parts.root().id(),
            Parts::Search(parts) => parts.root().id(),
        }
    }

    /// Whether they still stand inside node `node`.
    fn live(&self, node: NodeId, store: &NodeStore) -> bool {
        let root = self.root();
        store.arena().is_live(root) && store.parent(root) == Some(node)
    }
}

/// The strings of a list of them, those not strings left out.
fn labels(list: &Value) -> Vec<String> {
    match list {
        Value::List(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// How wide the parts built inside node `node` are: as wide as they ask
/// while it is unsized, else as wide as it is.
fn parts_size(store: &NodeStore, node: NodeId) -> viso_ui::Size {
    let width = match store.layout_input(node).size().width {
        viso_ui::Length::Fit => viso_ui::Length::Fit,
        _ => viso_ui::Length::Fill { weight: 1.0 },
    };
    viso_ui::Size {
        width,
        height: viso_ui::Length::Fit,
    }
}

/// Drops the parts inside node `node` so they are built again: their column
/// and what it holds go, and the routes their reports ran.
fn unbuild(parts: &mut Option<Parts>, node: NodeId, cx: &mut StructureCx<'_>) {
    if let Some(old) = parts.take().filter(|p| p.live(node, cx.store)) {
        cx.store.free_tree(old.root(), cx.effects, &mut Vec::new());
    }
}

/// A combo box builds its parts inside node `node` from `spec` the first
/// time it shows, once they are gone, or again when they are `stale`, and
/// shows its `value` — the chosen option's label, or with free text any
/// text — unless it holds it already. Each value it takes runs the node's
/// `changed` routes in `host`: an option chosen, or free text taken.
fn show_combo(
    parts: &mut Option<Parts>,
    node: NodeId,
    cx: &mut StructureCx<'_>,
    host: &mut ViewHost,
    mut spec: ComboSpec,
    stale: bool,
    value: &Value,
) {
    let held = value.as_str();
    let built = match parts.as_ref().filter(|p| !stale && p.live(node, cx.store)) {
        Some(Parts::Combo(built)) => built.clone(),
        _ => {
            unbuild(parts, node, cx);
            host.prune_sinks(|n| cx.store.arena().is_live(n));
            let report = |host: Weak<RefCell<ViewHost>>| {
                move |ev: &mut viso_ui::EventCx<'_>, text: &str| {
                    let Some(host) = host.upgrade() else { return };
                    let Ok(mut host) = host.try_borrow_mut() else {
                        return;
                    };
                    host.fire(node, EventRoute::Changed, field(Value::str(text)), ev);
                }
            };
            let (pick, commit): (Option<Pick>, Option<Commit>) = if spec.free {
                (None, Some(Box::new(report(host.this.clone()))))
            } else {
                let labels: Vec<String> = spec.options.iter().map(|o| o.label.clone()).collect();
                let report = report(host.this.clone());
                let pick: Pick = Box::new(move |ev, at| report(ev, &labels[at]));
                (Some(pick), None)
            };
            let text = held.unwrap_or_default();
            spec.selected = spec.options.iter().position(|o| o.label == text);
            spec.text = text.to_owned();
            spec.size = parts_size(cx.store, node);
            let mut build = BuildCx::with_parent(cx.store, cx.states, cx.bindings, node);
            let built = ComboParts::build(&mut build, &spec, pick, commit);
            *parts = Some(Parts::Combo(built.clone()));
            return;
        }
    };
    if let Some(held) = held
        && built.text() != held
    {
        if spec.free {
            built.set_text(cx, held);
        } else {
            let at = spec.options.iter().position(|o| o.label == held);
            built.select(cx, at);
        }
    }
}

/// A search field builds its parts inside node `node` from `spec` the first
/// time it shows, once they are gone, or again when they are `stale`; shows
/// its `value`, its query, unless it holds it already, and `suggestions`
/// when they changed. Each query it reports runs the node's `changed`
/// routes in `host`, and each search submitted its `submitted` routes.
#[allow(clippy::too_many_arguments)]
fn show_search(
    parts: &mut Option<Parts>,
    node: NodeId,
    cx: &mut StructureCx<'_>,
    host: &mut ViewHost,
    mut spec: SearchSpec,
    stale: bool,
    value: &Value,
    suggestions: Option<Vec<String>>,
) {
    let held = value.as_str();
    let built = match parts.as_ref().filter(|p| !stale && p.live(node, cx.store)) {
        Some(Parts::Search(built)) => built.clone(),
        _ => {
            unbuild(parts, node, cx);
            host.prune_sinks(|n| cx.store.arena().is_live(n));
            let this = host.this.clone();
            let change: SearchReport = Box::new(move |ev, text| {
                let Some(host) = this.upgrade() else { return };
                let Ok(mut host) = host.try_borrow_mut() else {
                    return;
                };
                host.fire(node, EventRoute::Changed, field(Value::str(text)), ev);
            });
            let this = host.this.clone();
            let submit: SearchReport = Box::new(move |ev, _| {
                let Some(host) = this.upgrade() else { return };
                let Ok(mut host) = host.try_borrow_mut() else {
                    return;
                };
                host.fire(node, EventRoute::Submitted, Value::Nil, ev);
            });
            spec.text = held.unwrap_or_default().to_owned();
            spec.size = parts_size(cx.store, node);
            let mut build = BuildCx::with_parent(cx.store, cx.states, cx.bindings, node);
            let built = SearchParts::build(&mut build, &spec, Some(change), Some(submit));
            *parts = Some(Parts::Search(built.clone()));
            built.set_suggestions(cx, suggestions.unwrap_or_default());
            return;
        }
    };
    if let Some(held) = held
        && built.text() != held
    {
        built.set_text(cx, held);
    }
    if let Some(suggestions) = suggestions {
        built.set_suggestions(cx, suggestions);
    }
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

/// What a node carrying `text` as `kind` offers a drag: plain text, a link
/// (and its text), files one path a line, or a value of the kind the app
/// names holding the text as a `String`; none without text.
fn drag_data(text: &Value, kind: &Value) -> Option<viso_ui::DragData> {
    use viso_ui::{DragData, DragItem, DragKind};
    let text = text.as_str()?;
    Some(match kind.as_str().unwrap_or("text") {
        "text" => DragData::text(text),
        "uri" => DragData::new()
            .with(DragItem::Uri(text.into()))
            .with(DragItem::Text(text.into())),
        "files" => DragData::new().with(DragItem::Files(
            text.lines()
                .filter(|l| !l.is_empty())
                .map(std::path::PathBuf::from)
                .collect(),
        )),
        named => DragData::new().with(DragItem::Value(
            DragKind::named(named),
            Rc::new(text.to_owned()),
        )),
    })
}

/// The drop target a node listing the kinds `kinds` is, a drop on it doing
/// `operation`; none listing none.
fn drop_target(kinds: &Value, operation: &Value) -> Option<viso_ui::DropTarget> {
    use viso_ui::{DragAccept, DragKind, DropOperation, DropTarget};
    let Value::List(kinds) = kinds else {
        return None;
    };
    let accepts: Vec<DragAccept> = kinds
        .iter()
        .filter_map(Value::as_str)
        .map(|kind| match kind {
            "text" => DragAccept::Text,
            "uri" => DragAccept::Uri,
            "files" => DragAccept::Files,
            named => DragAccept::Value(DragKind::named(named)),
        })
        .collect();
    if accepts.is_empty() {
        return None;
    }
    let operation = match operation.as_int() {
        Some(1) => DropOperation::Copy,
        Some(2) => DropOperation::Link,
        _ => DropOperation::Move,
    };
    Some(DropTarget { accepts, operation })
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

/// A `PageTransition` value as the transition it names, `native` for any
/// other.
fn page_transition(value: &Value) -> PageTransition {
    match value.as_int() {
        Some(1) => PageTransition::Slide,
        Some(2) => PageTransition::SharedAxis,
        Some(3) => PageTransition::Fade,
        Some(4) => PageTransition::None,
        _ => PageTransition::Native,
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
/// `reduced`). A negative or non-finite time is zero.
pub(crate) fn timing(spec: &Value) -> Timing {
    let Value::Agg(spec) = spec else {
        return Timing::default();
    };
    let field = |at: usize| spec.fields.get(at);
    let time = |at: usize| {
        let seconds = field(at).and_then(Value::as_float).unwrap_or(0.0);
        std::time::Duration::try_from_secs_f64(seconds).unwrap_or_default()
    };
    let tag = |at: usize| field(at).and_then(Value::as_int).unwrap_or(0);
    let pace = match field(2) {
        Some(Value::Agg(spring)) if spring.tag == SPRING => {
            let at = |i: usize| float(spring.fields.get(i).unwrap_or(&Value::Nil), 0.0);
            viso_ui::Pace::Spring(viso_ui::Spring::new(at(0), at(1), at(2)))
        }
        Some(value) => viso_ui::Pace::Eased {
            duration: time(0),
            easing: easing(value),
        },
        None => viso_ui::Pace::Eased {
            duration: time(0),
            easing: Easing::default(),
        },
    };
    Timing {
        delay: time(1),
        pace,
        reduced: reduced(tag(3)),
    }
}

/// A `ReducedMotion` by its tag.
fn reduced(tag: i64) -> viso_ui::ReducedMotion {
    if tag == 1 {
        viso_ui::ReducedMotion::Keep
    } else {
        viso_ui::ReducedMotion::Instant
    }
}

/// The tag of an `Easing`'s `spring`, past its named curves and its
/// `cubic_bezier`.
const SPRING: u32 = Easing::NAMED + 1;

/// A `Keyframes` value (`stops`, `duration`, `delay`, `repeat`, `direction`,
/// `reduced`), its stops' values folded at `node`. A negative or non-finite
/// time is zero.
pub(crate) fn keyframes(
    store: &NodeStore,
    node: NodeId,
    spec: &Value,
) -> Option<viso_ui::Keyframes> {
    use viso_ui::{Keyframe, PlayDirection, Repeat};
    let Value::Agg(spec) = spec else {
        return None;
    };
    let field = |at: usize| spec.fields.get(at);
    let time = |at: usize| {
        let seconds = field(at).and_then(Value::as_float).unwrap_or(0.0);
        std::time::Duration::try_from_secs_f64(seconds).unwrap_or_default()
    };
    let tag = |at: usize| field(at).and_then(Value::as_int).unwrap_or(0);
    let Some(Value::List(items)) = field(0) else {
        return None;
    };
    let stops = items.iter().filter_map(|stop| {
        let Value::Agg(stop) = stop else {
            return None;
        };
        let value = crate::route::look_of(store, node, stop.fields.get(1)?)?;
        Some(Keyframe {
            at: float(stop.fields.first()?, f32::NAN),
            value,
            easing: stop.fields.get(2).map_or(Easing::Linear, easing),
        })
    });
    let repeat = match field(3) {
        Some(Value::Agg(times)) => Repeat::Times(
            times
                .fields
                .first()
                .and_then(Value::as_int)
                .and_then(|n| u32::try_from(n).ok())
                .unwrap_or(0),
        ),
        Some(Value::Int(1)) => Repeat::Forever,
        _ => Repeat::Times(1),
    };
    let direction = match tag(4) {
        1 => PlayDirection::Reverse,
        2 => PlayDirection::Alternate,
        3 => PlayDirection::AlternateReverse,
        _ => PlayDirection::Normal,
    };
    Some(
        viso_ui::Keyframes::new(time(1), stops)
            .after(time(2))
            .repeat(repeat)
            .direction(direction)
            .reduced(reduced(tag(5))),
    )
}

/// An `Easing` value: a named curve by its tag, or a `cubic_bezier` by its
/// control points.
pub(crate) fn easing(value: &Value) -> Easing {
    match value {
        Value::Agg(bezier) if bezier.tag == Easing::NAMED => {
            let at = |i: usize| float(bezier.fields.get(i).unwrap_or(&Value::Nil), 0.0);
            Easing::CubicBezier(viso_ui::CubicBezier::new(at(0), at(1), at(2), at(3)))
        }
        _ => value
            .as_int()
            .and_then(|tag| u32::try_from(tag).ok())
            .and_then(Easing::named)
            .unwrap_or_default(),
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
