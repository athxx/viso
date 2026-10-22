//! Accessibility roles and live-region settings.

/// The accessibility role of a node — what kind of thing it is to an assistive
/// technology. Coarse this slice (the roles the current widgets need); grows as
/// widgets land. [`Group`](Role::Group) is the default for a pure
/// layout/decoration node that carries no authored semantics and is not
/// interactive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Role {
    /// A structural grouping with no interaction (a layout container).
    #[default]
    Group,
    /// An activatable control (the default role an interactive node takes when
    /// it has a handler but no authored role).
    Button,
    /// A two-state toggle: checked or unchecked. Carries a boolean state an
    /// assistive technology announces. The live checked value reaches the
    /// derived tree through the node's [`SemanticState`] side column (projected
    /// from the control's reactive cell in the flush phase), so a snapshot
    /// announces `checked`, not just the role and name.
    CheckBox,
    /// A slider: a continuous value picked from a range. Announces the current
    /// value and its `[min, max]` bounds. The live value reaches the derived
    /// tree through the node's [`SemanticState`] side column (`value` + `range`,
    /// projected from the control's reactive cell in the flush phase); the cold
    /// role carries neither.
    Slider,
    /// One option in a radio group: exactly one of the group is selected.
    /// Announces its selected state as `checked` (WAI-ARIA `role=radio` uses the
    /// checked state for selection). The live selection reaches the derived tree
    /// through the node's [`SemanticState`] side column, projected from the
    /// group's reactive cell in the flush phase.
    Radio,
    /// A static text label.
    Label,
    /// An image: a picture or an icon, named by its label (its text
    /// alternative). An image with no label is decoration, a mute
    /// [`Group`](Role::Group).
    Image,
    /// An editable text field: accepts typed characters and IME composition,
    /// carries a caret and an optional selection. The live text value, caret,
    /// and selection are held in the control's own reactive cells, not this cold
    /// role; this slice announces the role and name and proves the editing state
    /// through the control's cells and input tapes (the derive pass has no state
    /// store today — the same treatment as [`CheckBox`](Role::CheckBox)).
    TextField,
    /// One selectable tab in a [`TabList`](Role::TabList): activating it shows
    /// its associated panel and hides the others. Which tab is selected is held
    /// in the control's own reactive cell, not this cold role; this slice
    /// announces the role and name and proves the selection through the control's
    /// cell and input tapes (the derive pass has no state store today — the same
    /// treatment as [`CheckBox`](Role::CheckBox)).
    Tab,
    /// The container of a set of [`Tab`](Role::Tab)s — the tab strip. Groups the
    /// tabs so an assistive technology can present them as one selectable set.
    TabList,
    /// A navigation container holding a stack of pages, of which only the top is
    /// visible. Pushing a page reveals it and hides the one below; popping does
    /// the reverse. Which page is on top is held in the control's own reactive
    /// cell, not this cold role; this slice announces the role and proves the
    /// stack depth through the control's cell and input tapes (the derive pass
    /// has no state store today — the same treatment as
    /// [`CheckBox`](Role::CheckBox)).
    Navigation,
    /// A modal dialog: a window layered over the scene that takes focus and, while
    /// open, confines keyboard navigation to its own subtree (a focus scope) so
    /// the background is inert until it closes. An assistive technology announces
    /// it as a dialog and scopes its reading to the dialog's content. Whether the
    /// dialog is open is held in the control's own reactive cell, not this cold
    /// role; this slice announces the role and proves the open state and focus
    /// trap through the control's cell and input tapes (the derive pass has no
    /// state store today — the same treatment as [`CheckBox`](Role::CheckBox)).
    Dialog,
    /// A polite live region: a transient status message layered over the scene
    /// that announces itself without moving focus (WAI-ARIA `role=status`). The
    /// [`Toast`](../../viso_widgets/index.html) control's content carries this
    /// role. Unlike a [`Dialog`](Role::Dialog) it neither takes focus nor traps
    /// keyboard navigation — an assistive technology reads it when it appears and
    /// the user keeps working. Whether the status is showing is held in the
    /// control's own reactive cell, not this cold role; this slice announces the
    /// role and proves the show/auto-dismiss through the control's cell, input
    /// tapes, and the one-shot timer (the derive pass has no state store today —
    /// the same treatment as [`CheckBox`](Role::CheckBox)).
    Status,
    /// A named landmark region: a perceivable, standalone section of the scene an
    /// assistive technology can navigate to and announce by name (WAI-ARIA
    /// `role=region`). Stronger than [`Group`](Role::Group), which is a mute
    /// structural container, and distinct from [`Navigation`](Role::Navigation),
    /// which is specifically a page stack. The [`Dock`](../../viso_widgets/index.html)
    /// container and each detached floating panel carry this role so a screen
    /// reader can jump between docked areas by name. Purely structural — it carries
    /// no live reactive state, only its role and label.
    Region,
    /// The container of a hierarchical set of [`TreeItem`](Role::TreeItem)s — a tree
    /// browser (WAI-ARIA `role=tree`). The [`FileTree`](../../viso_widgets/index.html)
    /// control's viewport carries this role so an assistive technology presents its
    /// rows as one navigable hierarchy. Structural — it carries its role and label,
    /// no live reactive state.
    Tree,
    /// One node in a [`Tree`](Role::Tree) — a row in the tree browser (WAI-ARIA
    /// `role=treeitem`). Announces its expanded state (a directory row, via
    /// `aria-expanded`) and whether it is selected (via `aria-selected`). The live
    /// expanded/selected facts reach the derived tree through the node's
    /// [`SemanticState`] side column (`expanded` + `selected`), written by the
    /// control's structural reconcile step from its warm flattened-row model — not a
    /// reactive scalar cell, so the write is direct rather than projected.
    TreeItem,
    /// A heading: text that titles the section after it, which an assistive
    /// technology lists for navigation (WAI-ARIA `role=heading`).
    Heading,
    /// A list of [`ListItem`](Role::ListItem)s, announced with its item count
    /// (WAI-ARIA `role=list`).
    List,
    /// One item in a [`List`](Role::List) (WAI-ARIA `role=listitem`).
    ListItem,
    /// A [`TextField`](Role::TextField) whose text is masked: an assistive
    /// technology announces it as secure and never reads its text.
    SecureField,
    /// A [`TextField`](Role::TextField) holding a number that steps up and
    /// down: announces its value and any bounds, and takes increment and
    /// decrement (WAI-ARIA `role=spinbutton`).
    SpinButton,
}

impl Role {
    /// Every role, in declaration order.
    pub const ALL: [Role; 21] = [
        Role::Group,
        Role::Button,
        Role::CheckBox,
        Role::Slider,
        Role::Radio,
        Role::Label,
        Role::Image,
        Role::TextField,
        Role::Tab,
        Role::TabList,
        Role::Navigation,
        Role::Dialog,
        Role::Status,
        Role::Region,
        Role::Tree,
        Role::TreeItem,
        Role::Heading,
        Role::List,
        Role::ListItem,
        Role::SecureField,
        Role::SpinButton,
    ];

    /// The role whose [`label`](Self::label) is `label`.
    pub fn from_label(label: &str) -> Option<Role> {
        Role::ALL.into_iter().find(|role| role.label() == label)
    }

    /// The lowercase label used in an inspector dump / JSON snapshot (`group`,
    /// `button`, …), the readable discriminator for the role. A cold-path
    /// readout only, mirroring `InspectKind::label` and `BatchPipeline::label`.
    pub fn label(self) -> &'static str {
        match self {
            Role::Group => "group",
            Role::Button => "button",
            Role::CheckBox => "checkbox",
            Role::Slider => "slider",
            Role::Radio => "radio",
            Role::Label => "label",
            Role::Image => "image",
            Role::TextField => "textfield",
            Role::Tab => "tab",
            Role::TabList => "tablist",
            Role::Navigation => "navigation",
            Role::Dialog => "dialog",
            Role::Status => "status",
            Role::Region => "region",
            Role::Tree => "tree",
            Role::TreeItem => "treeitem",
            Role::Heading => "heading",
            Role::List => "list",
            Role::ListItem => "listitem",
            Role::SecureField => "securefield",
            Role::SpinButton => "spinbutton",
        }
    }
}

/// How an assistive technology announces a change to a node's content: not
/// at all, once the user is idle, or at once, interrupting what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LiveRegion {
    #[default]
    Off,
    Polite,
    Assertive,
}

impl LiveRegion {
    /// Every live setting, in declaration order.
    pub const ALL: [LiveRegion; 3] = [LiveRegion::Off, LiveRegion::Polite, LiveRegion::Assertive];

    /// The lowercase label of an inspector dump (`off`, `polite`, `assertive`).
    pub fn label(self) -> &'static str {
        match self {
            LiveRegion::Off => "off",
            LiveRegion::Polite => "polite",
            LiveRegion::Assertive => "assertive",
        }
    }
}

/// A node's *live* accessibility state — the reactive facts an assistive
/// technology announces alongside the role and name: whether a checkbox is
/// checked, a slider's current value and range, whether a disclosure is
/// expanded. Distinct from authored [`Semantics`] (cold, static role + label)
/// and derived at flush time.
///
/// This is the projection target that keeps the derive pass single-layer: a
/// control's reactive cell drives a parallel binding that writes this struct
/// into the node's side column during the flush phase (where both stores are
/// live); the derive pass then reads only the node column (`&self`), never the
/// state store, preserving the one-way dependency direction (AGENTS section
/// 3.5). Mirrors the `focused` slot precedent — live state reaches semantics
/// through a node column, not a cross-layer read.
///
/// `Copy` and heap-free: every field is a small scalar, so the side column
/// reuses its slots with no per-frame allocation. Fields are `Option` so a node
/// carries only the facets its role defines (a checkbox sets `checked`; a slider
/// sets `value` + `range`; a plain container sets none and stores `None` in the
/// column).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SemanticState {
    /// Two-state checked value: a [`CheckBox`](Role::CheckBox) / toggle, or the
    /// selected state of a [`Radio`](Role::Radio) option.
    pub checked: Option<bool>,
    /// A [`Slider`](Role::Slider)'s current value, resolved to the real range
    /// (not the relative 0..1 the control stores internally).
    pub value: Option<f32>,
    /// A [`Slider`](Role::Slider)'s inclusive `(min, max)` bounds.
    pub range: Option<(f32, f32)>,
    /// Whether a disclosure/expandable node is expanded. Reserved for later
    /// wiring (Navigation/Dialog disclosure); carried in the model now so the
    /// column and snapshot shape are stable.
    pub expanded: Option<bool>,
    /// Whether a selectable node is selected — a [`TreeItem`](Role::TreeItem) row
    /// (WAI-ARIA `aria-selected`). Distinct from [`checked`](Self::checked): a
    /// checkbox/radio announces a boolean *value*, whereas a tree row announces
    /// whether it is part of the current *selection*. `None` for a node whose role
    /// carries no selection.
    pub selected: Option<bool>,
    /// An item's 1-based place in its collection and the collection's size — a
    /// [`ListItem`](Role::ListItem) row of a virtual list, which an assistive
    /// technology cannot count from the rows it is shown (WAI-ARIA
    /// `aria-posinset` / `aria-setsize`).
    pub position: Option<(u32, u32)>,
}

impl SemanticState {
    /// A checked/unchecked state (checkbox, toggle, or radio-option selection).
    pub fn checked(checked: bool) -> Self {
        Self {
            checked: Some(checked),
            ..Self::default()
        }
    }

    /// A slider state: a current `value` within inclusive `(min, max)` bounds.
    pub fn slider(value: f32, min: f32, max: f32) -> Self {
        Self {
            value: Some(value),
            range: Some((min, max)),
            ..Self::default()
        }
    }

    /// A number field's state: its `value` and, when it has both bounds, its
    /// inclusive `(min, max)` range.
    pub fn number(value: f64, min: f64, max: f64) -> Self {
        Self {
            value: Some(value as f32),
            range: (min.is_finite() && max.is_finite()).then_some((min as f32, max as f32)),
            ..Self::default()
        }
    }

    /// This with an `expanded` disclosure flag.
    pub fn with_expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    /// This with a `selected` flag (a tree row's `aria-selected`).
    pub fn with_selected(mut self, selected: bool) -> Self {
        self.selected = Some(selected);
        self
    }

    /// This at 1-based place `index` of a collection of `count`.
    pub fn with_position(mut self, index: u32, count: u32) -> Self {
        self.position = Some((index, count));
        self
    }
}

/// What a node draws, derived from its content payload rather than stored — the
/// node model keeps no `kind` column, so a plain layout/decoration node reports
/// [`Container`](InspectKind::Container) and a content-bearing node reports the
/// variant of its payload. Enough for a tree readout to label each row without
/// inventing a field the runtime does not keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectKind {
    /// A node with no drawable content — a layout container or decoration box.
    Container,
    /// A node carrying a shaped glyph run.
    Text,
    /// A node carrying a textured image.
    Image,
    /// A node carrying a vector path.
    Path,
    /// A node whose box a brush program paints.
    Shader,
    /// A node showing an asset.
    Asset,
}

impl InspectKind {
    /// The lowercase label used in a tree dump (`container`, `text`, …).
    pub fn label(self) -> &'static str {
        match self {
            InspectKind::Container => "container",
            InspectKind::Text => "text",
            InspectKind::Image => "image",
            InspectKind::Path => "path",
            InspectKind::Shader => "shader",
            InspectKind::Asset => "asset",
        }
    }
}
