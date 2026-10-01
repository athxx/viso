//! The adaptive environment a view reads as `env`.
//!
//! The environment lives beside the state cells it publishes through: each
//! field a view reads is an integer revision cell the [`StateStore`] raises
//! only when the field's value changes, so the bindings and regions reading a
//! field wake exactly when it does. The window-wide fields have one cell each,
//! shared by every reader. The anchored fields, `constraints` and
//! `size_class`, depend on where the reader sits: each reader's component root
//! is an anchor whose values [`StateStore::settle_env`] resolves after layout
//! from the constraints its parent gives it and the nearest adaptive scope
//! above it.

use crate::component::NodeStore;
use crate::layout::{Align, Axis, Inset, LayoutInput, LayoutTree, Length};
use crate::node::NodeId;
use crate::state::{StateId, StateStore, StateValue};
use viso_render::Rect;

/// A field of the adaptive environment, in the order the prelude's
/// `Environment` record declares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EnvField {
    /// `env.window`: the window's logical size and scale factor.
    Window,
    /// `env.constraints`: the constraints the reading component's root
    /// receives from its parent.
    Constraints,
    /// `env.size_class`: the class of the nearest adaptive scope.
    SizeClass,
    /// `env.safe_area`: the insets the system reserves.
    SafeArea,
    /// `env.keyboard_inset`: the software keyboard's occlusion.
    KeyboardInset,
    /// `env.display_features`: hinges, folds and cutouts.
    DisplayFeatures,
    /// `env.input`: the available input devices.
    Input,
    /// `env.text_scale`: the user's text scale.
    TextScale,
    /// `env.reduced_motion`: whether the user asked for less motion.
    ReducedMotion,
    /// `env.orientation`: portrait or landscape.
    Orientation,
    /// `env.layout_direction`: left-to-right or right-to-left.
    LayoutDirection,
    /// `env.locale`: the user's locale.
    Locale,
}

impl EnvField {
    /// Every field, in declaration order.
    pub const ALL: [EnvField; 12] = [
        EnvField::Window,
        EnvField::Constraints,
        EnvField::SizeClass,
        EnvField::SafeArea,
        EnvField::KeyboardInset,
        EnvField::DisplayFeatures,
        EnvField::Input,
        EnvField::TextScale,
        EnvField::ReducedMotion,
        EnvField::Orientation,
        EnvField::LayoutDirection,
        EnvField::Locale,
    ];

    /// The field's source name.
    pub const fn name(self) -> &'static str {
        match self {
            EnvField::Window => "window",
            EnvField::Constraints => "constraints",
            EnvField::SizeClass => "size_class",
            EnvField::SafeArea => "safe_area",
            EnvField::KeyboardInset => "keyboard_inset",
            EnvField::DisplayFeatures => "display_features",
            EnvField::Input => "input",
            EnvField::TextScale => "text_scale",
            EnvField::ReducedMotion => "reduced_motion",
            EnvField::Orientation => "orientation",
            EnvField::LayoutDirection => "layout_direction",
            EnvField::Locale => "locale",
        }
    }

    /// The field named `name`.
    pub fn named(name: &str) -> Option<EnvField> {
        EnvField::ALL.into_iter().find(|f| f.name() == name)
    }

    /// The field's wire tag, its declaration index.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The field of wire tag `tag`.
    pub fn from_tag(tag: u8) -> Option<EnvField> {
        EnvField::ALL.get(tag as usize).copied()
    }

    /// Whether the field's value depends on where the reader sits in the
    /// tree rather than on the window alone.
    pub const fn anchored(self) -> bool {
        matches!(self, EnvField::Constraints | EnvField::SizeClass)
    }
}

/// The window's logical size and scale factor.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowMetrics {
    /// Logical width, in dp.
    pub width: f32,
    /// Logical height, in dp.
    pub height: f32,
    /// Physical pixels per dp.
    pub scale_factor: f32,
}

impl Default for WindowMetrics {
    fn default() -> Self {
        WindowMetrics {
            width: 0.0,
            height: 0.0,
            scale_factor: 1.0,
        }
    }
}

/// A coarse width class, from the nearest adaptive scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SizeClass {
    /// Narrower than [`SizeClassPolicy::medium`].
    #[default]
    Compact,
    /// At least [`SizeClassPolicy::medium`], narrower than
    /// [`SizeClassPolicy::expanded`].
    Medium,
    /// At least [`SizeClassPolicy::expanded`].
    Expanded,
}

/// Whether the window is taller or wider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Orientation {
    /// At least as tall as it is wide.
    #[default]
    Portrait,
    /// Wider than it is tall.
    Landscape,
}

/// How precisely the primary pointer points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PointerPrecision {
    /// A mouse, trackpad or stylus.
    #[default]
    Fine,
    /// A finger.
    Coarse,
    /// No pointer.
    Unavailable,
}

/// The direction text and layout flow in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayoutDirection {
    /// Left to right.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

/// What a [`DisplayFeature`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DisplayFeatureKind {
    /// A hinge between two screens, which content should not straddle.
    Hinge,
    /// A fold in a flexible screen.
    Fold,
    /// A camera or sensor cutout.
    Cutout,
}

/// A hinge, fold or cutout, in window coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DisplayFeature {
    /// What the feature is.
    pub kind: DisplayFeatureKind,
    /// Where it is, in dp.
    pub bounds: Rect,
}

/// The input devices available.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InputCapabilities {
    /// How precisely the primary pointer points.
    pub primary_pointer_precision: PointerPrecision,
    /// Whether a pointer can hover without pressing.
    pub hover_available: bool,
    /// Whether a hardware keyboard is attached.
    pub keyboard_available: bool,
    /// Whether the screen takes touch.
    pub touch_available: bool,
    /// Whether a pen is available.
    pub pen_available: bool,
    /// Whether a gamepad is connected.
    pub gamepad_available: bool,
}

impl Default for InputCapabilities {
    fn default() -> Self {
        InputCapabilities {
            primary_pointer_precision: PointerPrecision::Fine,
            hover_available: true,
            keyboard_available: true,
            touch_available: false,
            pen_available: false,
            gamepad_available: false,
        }
    }
}

/// The constraints a node receives from its parent, before it measures: a
/// maximum is `None` along an axis its parent does not bound, as a scroll
/// viewport's scrolling axis.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct LocalConstraints {
    /// The least width the node may take, in dp.
    pub min_width: f32,
    /// The most width the node may take, in dp.
    pub max_width: Option<f32>,
    /// The least height the node may take, in dp.
    pub min_height: f32,
    /// The most height the node may take, in dp.
    pub max_height: Option<f32>,
}

/// The widths at which the size classes begin, in dp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SizeClassPolicy {
    /// The least width of [`SizeClass::Medium`].
    pub medium: f32,
    /// The least width of [`SizeClass::Expanded`].
    pub expanded: f32,
}

impl Default for SizeClassPolicy {
    fn default() -> Self {
        SizeClassPolicy {
            medium: 600.0,
            expanded: 840.0,
        }
    }
}

impl SizeClassPolicy {
    /// The class of a scope `width` dp wide.
    pub fn classify(&self, width: f32) -> SizeClass {
        if width >= self.expanded {
            SizeClass::Expanded
        } else if width >= self.medium {
            SizeClass::Medium
        } else {
            SizeClass::Compact
        }
    }
}

/// The window-wide adaptive environment the app reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Environment {
    /// The window's logical size and scale factor.
    pub window: WindowMetrics,
    /// The insets the system reserves, in dp.
    pub safe_area: Inset,
    /// The height the software keyboard covers at the bottom, in dp.
    pub keyboard_inset: f32,
    /// The hinges, folds and cutouts.
    pub display_features: Vec<DisplayFeature>,
    /// The input devices available.
    pub input: InputCapabilities,
    /// The user's text scale; `1.0` is the default size.
    pub text_scale: f32,
    /// Whether the user asked for less motion.
    pub reduced_motion: bool,
    /// The direction text and layout flow in.
    pub layout_direction: LayoutDirection,
    /// The user's locale, a BCP 47 tag.
    pub locale: String,
}

impl Default for Environment {
    fn default() -> Self {
        Environment {
            window: WindowMetrics::default(),
            safe_area: Inset::default(),
            keyboard_inset: 0.0,
            display_features: Vec::new(),
            input: InputCapabilities::default(),
            text_scale: 1.0,
            reduced_motion: false,
            layout_direction: LayoutDirection::Ltr,
            locale: "und".to_owned(),
        }
    }
}

impl Environment {
    /// Whether the window is taller or wider.
    pub fn orientation(&self) -> Orientation {
        if self.window.width > self.window.height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        }
    }

    /// The constraints the window's content area gives: the window less the
    /// safe area.
    pub fn content_constraints(&self) -> LocalConstraints {
        let inset = self.safe_area;
        LocalConstraints {
            min_width: 0.0,
            max_width: Some((self.window.width - inset.left - inset.right).max(0.0)),
            min_height: 0.0,
            max_height: Some((self.window.height - inset.top - inset.bottom).max(0.0)),
        }
    }

    /// Whether `field` differs between `self` and `other`. An anchored field
    /// is not window-wide and never differs here.
    fn differs(&self, other: &Environment, field: EnvField) -> bool {
        match field {
            EnvField::Window => self.window != other.window,
            EnvField::SafeArea => self.safe_area != other.safe_area,
            EnvField::KeyboardInset => self.keyboard_inset != other.keyboard_inset,
            EnvField::DisplayFeatures => self.display_features != other.display_features,
            EnvField::Input => self.input != other.input,
            EnvField::TextScale => self.text_scale != other.text_scale,
            EnvField::ReducedMotion => self.reduced_motion != other.reduced_motion,
            EnvField::Orientation => self.orientation() != other.orientation(),
            EnvField::LayoutDirection => self.layout_direction != other.layout_direction,
            EnvField::Locale => self.locale != other.locale,
            EnvField::Constraints | EnvField::SizeClass => false,
        }
    }
}

/// A handle to one anchor of the environment's anchored fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnchorId {
    index: u32,
    generation: u32,
}

/// The anchored fields at one node.
#[derive(Debug)]
struct Anchor {
    node: NodeId,
    generation: u32,
    live: bool,
    constraints: LocalConstraints,
    class: SizeClass,
    /// The revision cells of `constraints` and `class`, allocated when first
    /// asked for.
    cells: [Option<StateId>; 2],
    /// A cell raised with either revision.
    wake: Option<StateId>,
}

/// The adaptive environment: the window-wide [`Environment`], the
/// [`SizeClassPolicy`], the anchors resolving the anchored fields, and the
/// adaptive scopes. It is part of the [`StateStore`], which publishes it.
#[derive(Debug, Default)]
pub struct AdaptiveEnv {
    env: Environment,
    policy: SizeClassPolicy,
    /// The class of the window's content width.
    window_class: SizeClass,
    /// The revision cell of each window-wide field, by tag, allocated when
    /// first asked for.
    cells: [Option<StateId>; EnvField::ALL.len()],
    anchors: Vec<Anchor>,
    free: Vec<u32>,
    /// The adaptive scopes and their static bases, by ascending node index.
    scopes: Vec<(NodeId, Option<f32>)>,
    /// Whether something the anchors resolve from changed outside a layout
    /// since the last settle.
    unsettled: bool,
}

impl AdaptiveEnv {
    /// The window-wide environment.
    pub fn environment(&self) -> &Environment {
        &self.env
    }

    /// The size class policy.
    pub fn policy(&self) -> SizeClassPolicy {
        self.policy
    }

    /// The class of the window's content width, which a reader under no
    /// adaptive scope gets.
    pub fn window_class(&self) -> SizeClass {
        self.window_class
    }

    /// The constraints `anchor` last resolved to, `None` for a released one.
    pub fn constraints(&self, anchor: AnchorId) -> Option<LocalConstraints> {
        self.anchor(anchor).map(|a| a.constraints)
    }

    /// The size class `anchor` last resolved to, `None` for a released one.
    pub fn size_class(&self, anchor: AnchorId) -> Option<SizeClass> {
        self.anchor(anchor).map(|a| a.class)
    }

    /// The number of live anchors.
    pub fn anchor_count(&self) -> usize {
        self.anchors.iter().filter(|a| a.live).count()
    }

    /// Whether the anchors must settle again although no layout ran: the
    /// window, the policy or the scopes changed, or an anchor was added.
    pub fn unsettled(&self) -> bool {
        self.unsettled
    }

    fn anchor(&self, id: AnchorId) -> Option<&Anchor> {
        self.anchors
            .get(id.index as usize)
            .filter(|a| a.live && a.generation == id.generation)
    }

    fn scope(&self, node: NodeId) -> Option<Option<f32>> {
        self.scopes
            .binary_search_by_key(&node.index(), |(n, _)| n.index())
            .ok()
            .filter(|&at| self.scopes[at].0 == node)
            .map(|at| self.scopes[at].1)
    }

    /// The class of the nearest adaptive scope at or above `node`: a scope
    /// with a basis classifies it, one without classifies the width its
    /// parent gives it, and one its parent does not bound inherits the class
    /// of the scope above. Under no scope it is the window's class.
    fn class_at(&self, nodes: &NodeStore, node: NodeId) -> SizeClass {
        if self.scopes.is_empty() {
            return self.window_class;
        }
        let mut at = Some(node);
        while let Some(node) = at {
            if let Some(basis) = self.scope(node)
                && let Some(width) = basis.or_else(|| incoming(nodes, node, Axis::Row))
            {
                return self.policy.classify(width);
            }
            at = nodes.parent(node);
        }
        self.window_class
    }
}

/// The constraints `node`'s parent gives it along `axis`: the parent's content
/// box, found through every parent whose extent along `axis` its content
/// decides, less their padding; `None` under a viewport scrolling along `axis`.
/// The root gets its own box.
fn incoming(nodes: &NodeStore, node: NodeId, axis: Axis) -> Option<f32> {
    let extent = |rect: Rect| match axis {
        Axis::Row => rect.w,
        Axis::Column => rect.h,
    };
    let mut inset = 0.0;
    let mut at = node;
    loop {
        let Some(parent) = nodes.parent(at) else {
            return Some((extent(nodes.bounds(at)) - inset).max(0.0));
        };
        let input = nodes.input(parent.index());
        match input {
            LayoutInput::Scroll { axis: along, .. }
            | LayoutInput::AbsoluteRows { axis: along, .. }
                if along == axis =>
            {
                return None;
            }
            LayoutInput::Flex { padding, .. } | LayoutInput::Grid { padding, .. } => {
                inset += padding_on(padding, axis);
            }
            _ => {}
        }
        if !content_sized(nodes, parent, axis) {
            return Some((extent(nodes.bounds(parent)) - inset).max(0.0));
        }
        at = parent;
    }
}

/// Whether `node`'s content decides its extent along `axis`: it fits its
/// content, or it fills the cross axis of a flex container that does not
/// stretch its children.
fn content_sized(nodes: &NodeStore, node: NodeId, axis: Axis) -> bool {
    match nodes.input(node.index()).size().on(axis) {
        Length::Fit => true,
        Length::Fill { .. } => matches!(
            nodes.parent(node).map(|parent| nodes.input(parent.index())),
            Some(LayoutInput::Flex { axis: main, align, .. })
                if main != axis && align != Align::Stretch
        ),
        Length::Fixed(_) | Length::Relative { .. } => false,
    }
}

fn padding_on(padding: Inset, axis: Axis) -> f32 {
    match axis {
        Axis::Row => padding.left + padding.right,
        Axis::Column => padding.top + padding.bottom,
    }
}

/// The adaptive environment the store publishes.
impl StateStore {
    /// The adaptive environment.
    pub fn env(&self) -> &AdaptiveEnv {
        &self.env
    }

    /// Changes the window-wide environment with `change`, raising the cell of
    /// each field whose value it changed and of nothing else. Returns whether
    /// any field changed.
    pub fn update_env(&mut self, change: impl FnOnce(&mut Environment)) -> bool {
        let before = self.env.env.clone();
        change(&mut self.env.env);
        let mut changed = false;
        for field in EnvField::ALL {
            if !self.env.env.differs(&before, field) {
                continue;
            }
            changed = true;
            if let Some(cell) = self.env.cells[field.tag() as usize] {
                self.raise(cell);
            }
        }
        if self.env.env.window != before.window || self.env.env.safe_area != before.safe_area {
            self.env.window_class = self
                .env
                .policy
                .classify(self.env.env.content_constraints().max_width.unwrap_or(0.0));
            self.env.unsettled = true;
        }
        changed
    }

    /// Replaces the size class policy; the anchors resolve against it when
    /// they next settle.
    pub fn set_size_class_policy(&mut self, policy: SizeClassPolicy) {
        if self.env.policy == policy {
            return;
        }
        let env = &mut self.env;
        env.policy = policy;
        env.window_class = policy.classify(env.env.content_constraints().max_width.unwrap_or(0.0));
        env.unsettled = true;
    }

    /// The revision cell of window-wide field `field`, `None` for an anchored
    /// one, whose cells are an anchor's.
    pub fn env_cell(&mut self, field: EnvField) -> Option<StateId> {
        if field.anchored() {
            return None;
        }
        let at = field.tag() as usize;
        if let Some(cell) = self.env.cells[at] {
            return Some(cell);
        }
        let cell = self.alloc(StateValue::Int(0));
        self.env.cells[at] = Some(cell);
        Some(cell)
    }

    /// An anchor resolving the anchored fields at `node`, raising `wake`
    /// whenever one of its values changes. It starts with the window content
    /// area's constraints and class; the next settle resolves it.
    pub fn anchor_env(&mut self, node: NodeId, wake: Option<StateId>) -> AnchorId {
        let constraints = self.env.env.content_constraints();
        let class = self.env.window_class;
        self.env.unsettled = true;
        let env = &mut self.env;
        if let Some(index) = env.free.pop() {
            let anchor = &mut env.anchors[index as usize];
            anchor.node = node;
            anchor.live = true;
            anchor.constraints = constraints;
            anchor.class = class;
            anchor.cells = [None; 2];
            anchor.wake = wake;
            return AnchorId {
                index,
                generation: anchor.generation,
            };
        }
        let index = env.anchors.len() as u32;
        env.anchors.push(Anchor {
            node,
            generation: 0,
            live: true,
            constraints,
            class,
            cells: [None; 2],
            wake,
        });
        AnchorId {
            index,
            generation: 0,
        }
    }

    /// The revision cell of anchored field `field` at `anchor`; `None` for a
    /// window-wide field or a released anchor.
    pub fn anchor_cell(&mut self, anchor: AnchorId, field: EnvField) -> Option<StateId> {
        let at = match field {
            EnvField::Constraints => 0,
            EnvField::SizeClass => 1,
            _ => return None,
        };
        self.env.anchor(anchor)?;
        if let Some(cell) = self.env.anchors[anchor.index as usize].cells[at] {
            return Some(cell);
        }
        let cell = self.alloc(StateValue::Int(0));
        self.env.anchors[anchor.index as usize].cells[at] = Some(cell);
        Some(cell)
    }

    /// Releases `anchor` and frees its cells. Returns whether it was live.
    pub fn release_anchor(&mut self, anchor: AnchorId) -> bool {
        if self.env.anchor(anchor).is_none() {
            return false;
        }
        let slot = &mut self.env.anchors[anchor.index as usize];
        slot.live = false;
        slot.generation = slot.generation.wrapping_add(1);
        slot.wake = None;
        let cells = std::mem::take(&mut slot.cells);
        self.env.free.push(anchor.index);
        for cell in cells.into_iter().flatten() {
            self.free(cell);
        }
        true
    }

    /// Marks `node` an adaptive scope, classifying `basis` when given and
    /// otherwise the width its parent gives it.
    pub fn mark_adaptive_scope(&mut self, node: NodeId, basis: Option<f32>) {
        let scopes = &mut self.env.scopes;
        match scopes.binary_search_by_key(&node.index(), |(n, _)| n.index()) {
            Ok(at) => scopes[at] = (node, basis),
            Err(at) => scopes.insert(at, (node, basis)),
        }
        self.env.unsettled = true;
    }

    /// Unmarks `node` as an adaptive scope. Returns whether it was one.
    pub fn unmark_adaptive_scope(&mut self, node: NodeId) -> bool {
        let scopes = &mut self.env.scopes;
        match scopes.binary_search_by_key(&node.index(), |(n, _)| n.index()) {
            Ok(at) if scopes[at].0 == node => {
                scopes.remove(at);
                self.env.unsettled = true;
                true
            }
            _ => false,
        }
    }

    /// Resolves every anchor against the laid-out `nodes`, raising the cells
    /// of each value that changed and the anchor's wake cell with them.
    /// Returns whether any value changed.
    pub fn settle_env(&mut self, nodes: &NodeStore) -> bool {
        self.env.unsettled = false;
        let arena = nodes.arena();
        self.env.scopes.retain(|&(node, _)| arena.is_live(node));
        let mut changed = false;
        for index in 0..self.env.anchors.len() {
            let anchor = &self.env.anchors[index];
            if !anchor.live || !nodes.arena().is_live(anchor.node) {
                continue;
            }
            let node = anchor.node;
            let constraints = LocalConstraints {
                min_width: 0.0,
                max_width: incoming(nodes, node, Axis::Row),
                min_height: 0.0,
                max_height: incoming(nodes, node, Axis::Column),
            };
            let class = self.env.class_at(nodes, node);
            let anchor = &mut self.env.anchors[index];
            let mut raise = [None; 3];
            let mut moved = false;
            if anchor.constraints != constraints {
                anchor.constraints = constraints;
                raise[0] = anchor.cells[0];
                moved = true;
            }
            if anchor.class != class {
                anchor.class = class;
                raise[1] = anchor.cells[1];
                moved = true;
            }
            if !moved {
                continue;
            }
            raise[2] = anchor.wake;
            changed = true;
            for cell in raise.into_iter().flatten() {
                self.raise(cell);
            }
        }
        changed
    }

    /// Raises the integer revision in `cell`.
    fn raise(&mut self, cell: StateId) {
        if let Some(StateValue::Int(revision)) = self.get(cell) {
            self.set(cell, StateValue::Int(revision.wrapping_add(1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_and_names_round_trip() {
        for (i, field) in EnvField::ALL.into_iter().enumerate() {
            assert_eq!(field.tag() as usize, i);
            assert_eq!(EnvField::from_tag(field.tag()), Some(field));
            assert_eq!(EnvField::named(field.name()), Some(field));
        }
        assert_eq!(EnvField::from_tag(12), None);
        assert_eq!(EnvField::named("theme"), None);
    }

    use crate::component::{BuildCx, FlexStyle, LeafStyle, ScrollStyle};
    use crate::layout::Size;

    fn revision(states: &StateStore, cell: StateId) -> i32 {
        match states.get(cell) {
            Some(StateValue::Int(revision)) => revision,
            other => panic!("not a revision: {other:?}"),
        }
    }

    fn window(states: &mut StateStore, width: f32, height: f32) {
        states.update_env(|env| {
            env.window.width = width;
            env.window.height = height;
        });
    }

    #[test]
    fn an_update_raises_only_the_fields_it_changes() {
        let mut states = StateStore::new();
        let cells: Vec<StateId> = [
            EnvField::Window,
            EnvField::Orientation,
            EnvField::TextScale,
            EnvField::Locale,
        ]
        .into_iter()
        .map(|field| states.env_cell(field).unwrap())
        .collect();
        assert_eq!(states.env_cell(EnvField::SizeClass), None);
        assert_eq!(states.env_cell(EnvField::Window), Some(cells[0]));
        let revisions = |states: &StateStore| -> Vec<i32> {
            cells.iter().map(|&cell| revision(states, cell)).collect()
        };

        window(&mut states, 800.0, 600.0);
        assert_eq!(revisions(&states), [1, 1, 0, 0]);
        window(&mut states, 1200.0, 600.0);
        assert_eq!(revisions(&states), [2, 1, 0, 0], "still landscape");
        assert!(!states.update_env(|env| env.text_scale = 1.0));
        assert_eq!(revisions(&states), [2, 1, 0, 0], "an unchanged value");
        states.update_env(|env| {
            env.text_scale = 1.5;
            env.locale = "ar".to_owned();
        });
        assert_eq!(revisions(&states), [2, 1, 1, 1]);
        assert_eq!(states.env().window_class(), SizeClass::Expanded);
    }

    /// A 1000×500 row padded by 10dp holding a 300dp column with an adaptive
    /// scope inside, a filling column, and a horizontal scroll viewport with a
    /// scope inside; a leaf in each.
    struct Tree {
        nodes: NodeStore,
        root: NodeId,
        scoped: NodeId,
        scope: NodeId,
        plain: NodeId,
        scroll_scope: NodeId,
        scrolled: NodeId,
    }

    fn tree() -> Tree {
        let mut nodes = NodeStore::new();
        let leaf = |cx: &mut BuildCx<'_>| {
            cx.leaf(LeafStyle {
                size: Size::fixed(10.0, 10.0),
                ..Default::default()
            })
            .id()
        };
        let column = |size: Size| FlexStyle {
            axis: Axis::Column,
            size,
            ..Default::default()
        };
        let (mut scoped, mut scope, mut plain) = (None, None, None);
        let (mut scroll_scope, mut scrolled) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut nodes);
            cx.flex(
                FlexStyle {
                    padding: Inset {
                        left: 10.0,
                        top: 10.0,
                        right: 10.0,
                        bottom: 10.0,
                    },
                    ..Default::default()
                },
                |cx| {
                    cx.flex(column(Size::fixed(300.0, 200.0)), |cx| {
                        scope = Some(
                            cx.flex(column(Size::fill()), |cx| scoped = Some(leaf(cx)))
                                .id(),
                        );
                    });
                    cx.flex(column(Size::fill()), |cx| plain = Some(leaf(cx)));
                    cx.scroll(
                        ScrollStyle {
                            axis: Axis::Row,
                            size: Size::fixed(200.0, 200.0),
                            ..Default::default()
                        },
                        |cx| {
                            scroll_scope = Some(
                                cx.flex(column(Size::fixed(900.0, 100.0)), |cx| {
                                    scrolled = Some(leaf(cx))
                                })
                                .id(),
                            );
                        },
                    );
                },
            );
            cx.root().unwrap()
        };
        let mut tree = Tree {
            nodes,
            root,
            scoped: scoped.unwrap(),
            scope: scope.unwrap(),
            plain: plain.unwrap(),
            scroll_scope: scroll_scope.unwrap(),
            scrolled: scrolled.unwrap(),
        };
        tree.layout(1000.0);
        tree
    }

    impl Tree {
        fn layout(&mut self, width: f32) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: 500.0,
            };
            self.nodes.layout(self.root, surface, &mut Vec::new());
        }
    }

    #[test]
    fn anchors_resolve_from_their_parent_and_nearest_scope() {
        let tree = tree();
        let mut states = StateStore::new();
        window(&mut states, 1000.0, 500.0);
        states.mark_adaptive_scope(tree.scope, None);
        states.mark_adaptive_scope(tree.scroll_scope, None);
        let scoped = states.anchor_env(tree.scoped, None);
        let plain = states.anchor_env(tree.plain, None);
        let scrolled = states.anchor_env(tree.scrolled, None);
        let root = states.anchor_env(tree.root, None);
        assert_eq!(
            states.env().constraints(scoped),
            Some(states.env().environment().content_constraints()),
            "before layout, the window's content area"
        );
        assert!(states.env().unsettled());
        assert!(states.settle_env(&tree.nodes));
        assert!(!states.env().unsettled());
        let env = states.env();

        let width = |anchor| env.constraints(anchor).unwrap().max_width;
        assert_eq!(width(scoped), Some(300.0));
        assert_eq!(
            env.size_class(scoped),
            Some(SizeClass::Compact),
            "the scope is 300dp"
        );
        assert_eq!(width(plain), Some(480.0), "the row's leftover");
        assert_eq!(
            env.size_class(plain),
            Some(SizeClass::Expanded),
            "the window's"
        );
        assert_eq!(width(scrolled), Some(900.0));
        assert_eq!(
            env.size_class(scrolled),
            Some(SizeClass::Expanded),
            "an unbounded scope inherits"
        );
        assert_eq!(width(root), Some(1000.0));
        let scope = env.constraints(scoped).unwrap();
        assert_eq!(scope.max_height, Some(200.0));

        states.mark_adaptive_scope(tree.scroll_scope, Some(500.0));
        states.settle_env(&tree.nodes);
        assert_eq!(
            states.env().size_class(scrolled),
            Some(SizeClass::Compact),
            "its basis"
        );
        assert!(!states.settle_env(&tree.nodes), "nothing moved");
    }

    #[test]
    fn a_scroll_axis_leaves_its_content_unbounded() {
        let tree = tree();
        let mut states = StateStore::new();
        window(&mut states, 1000.0, 500.0);
        let content = states.anchor_env(tree.scroll_scope, None);
        states.settle_env(&tree.nodes);
        let constraints = states.env().constraints(content).unwrap();
        assert_eq!(constraints.max_width, None);
        assert_eq!(constraints.max_height, Some(200.0));
    }

    #[test]
    fn a_resize_that_keeps_the_class_raises_only_the_constraints() {
        let mut tree = tree();
        let mut states = StateStore::new();
        window(&mut states, 1000.0, 500.0);
        let wake = states.alloc(StateValue::Int(0));
        let plain = states.anchor_env(tree.plain, Some(wake));
        let constraints = states.anchor_cell(plain, EnvField::Constraints).unwrap();
        let class = states.anchor_cell(plain, EnvField::SizeClass).unwrap();
        assert_eq!(states.anchor_cell(plain, EnvField::Window), None);
        states.settle_env(&tree.nodes);
        let before = [constraints, class, wake].map(|cell| revision(&states, cell));

        window(&mut states, 900.0, 500.0);
        tree.layout(900.0);
        assert!(states.settle_env(&tree.nodes));
        let after = [constraints, class, wake].map(|cell| revision(&states, cell));
        assert_eq!(after, [before[0] + 1, before[1], before[2] + 1]);

        window(&mut states, 500.0, 500.0);
        tree.layout(500.0);
        states.settle_env(&tree.nodes);
        assert_eq!(revision(&states, class), before[1] + 1);
        assert_eq!(states.env().size_class(plain), Some(SizeClass::Compact));
    }

    #[test]
    fn a_policy_reclassifies_and_a_released_anchor_goes_stale() {
        let tree = tree();
        let mut states = StateStore::new();
        window(&mut states, 1000.0, 500.0);
        let plain = states.anchor_env(tree.plain, None);
        let cell = states.anchor_cell(plain, EnvField::SizeClass).unwrap();
        states.settle_env(&tree.nodes);
        states.set_size_class_policy(SizeClassPolicy {
            medium: 400.0,
            expanded: 1200.0,
        });
        assert!(states.env().unsettled());
        states.settle_env(&tree.nodes);
        assert_eq!(states.env().size_class(plain), Some(SizeClass::Medium));
        assert_eq!(states.env().anchor_count(), 1);

        assert!(states.release_anchor(plain));
        assert!(!states.release_anchor(plain));
        assert!(!states.is_live(cell));
        assert_eq!(states.env().size_class(plain), None);
        let next = states.anchor_env(tree.plain, None);
        assert_ne!(next, plain, "a reused slot is a new anchor");
        assert_eq!(states.env().anchor_count(), 1);
    }
}
