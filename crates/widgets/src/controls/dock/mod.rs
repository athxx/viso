//! The [`Dock`] control — an IDE-style container of dockable, draggable panels.
//!
//! A `Dock` arranges a set of application-supplied panels into a tree of split
//! and tabbed regions, with draggable seams between splits, a keyboard-equivalent
//! resize on each seam, drag-to-redock of any tab, undock, and detachment into a
//! floating panel drawn over the scene. It is the repository's first control that
//! is a directory rather than a single file (AGENTS section 5): the layout tree,
//! the build walk, pointer dragging, the live-relayout reconcile step, the
//! imperative command handle, and the accessibility contract each own a file.
//!
//! # The model, in one paragraph
//!
//! The dock owns a [`DockTree`] — a plain, `Box`-linked enum of [`DockNode`]s —
//! as its **warm** source of truth (built once, read by the build walk and the
//! reconcile step; not a bag of reactive scalar cells, which could not hold a
//! tree). Panels are referenced only by a stable [`PanelKey`], never by position,
//! so a panel keeps its identity — and its built-once content subtree and
//! reactive cells — as it moves between docked areas (AGENTS section 8.6, 21.8).
//! The build walk in [`build`] authors the retained node subtree for the tree
//! once; a discrete drag or command writes an *intent*, and the reconcile step
//! (holding `&mut NodeStore`) turns it into a live layout change — the same
//! handler-writes-intent / reconcile-applies split the virtualized list uses, and
//! the reason this control is documented by an ADR.
//!
//! # Registering panels
//!
//! ```
//! use viso_widgets::{dock, DockNode, DockTree, PanelKey};
//! use viso_ui::{Axis, BoxStyle, LeafStyle, Size};
//!
//! let editor = PanelKey(0);
//! let sidebar = PanelKey(1);
//! let control = dock(DockTree::new(DockNode::split(
//!         Axis::Row,
//!         0.25,
//!         DockNode::panel(sidebar),
//!         DockNode::panel(editor),
//!     )))
//!     .panel(sidebar, |cx| {
//!         cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE });
//!     })
//!     .panel(editor, |cx| {
//!         cx.leaf(LeafStyle { size: Size::fill(), style: BoxStyle::NONE });
//!     });
//! let _ = control;
//! ```
//!
//! # Scope of this first version
//!
//! **In:** binary-nested splits and tabbed regions keyed by [`PanelKey`];
//! live proportional seam resize (a 50-logical-pixel minimum-pane floor, a
//! deadzone/weighted drag, a keyboard-step equivalent); drag a tab to redock
//! (edge bands [`Left`](DropPart::Left)/[`Right`](DropPart::Right)/[`Top`](DropPart::Top)/[`Bottom`](DropPart::Bottom),
//! [`Center`](DropPart::Center)/[`TabBar`](DropPart::TabBar) to join a tab group,
//! with an overlay drop hint); undock; float a panel into an overlay container and
//! drag it back to redock; an imperative [`DockHandle`]; `Region`/`TabList`/`Group`
//! accessibility with keyboard equivalents.
//!
//! **Out (explicitly deferred):** tree persistence/serialization (awaits Viso's
//! serialization story); lazy panel build (every panel builds once, as the
//! navigation stack and tabs do); cross-window docking (dragging out into a new OS
//! window — a future consumer of the float mechanism, not built here); animated
//! dock/float transitions (this version snaps); auto-hide/collapsible regions;
//! nested scroll/grid beyond a panel's own builder; touch/gesture arbitration
//! beyond the primary pointer.

mod build;
mod command;
mod drag;
mod reconcile;
mod semantics;
mod tree;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use viso_ui::{BoxStyle, BuildCx, Component, Rgba, Size};

use build::{BuildOut, PanelNodes};

pub use command::{DockHandle, DockHandleSlot};
pub use tree::{DockNode, DockTree, DropPart, Floating, PanelKey};

/// A panel's build-time content builder: it authors the panel's subtree into the
/// dock's panel area. Boxed so a dock can hold a heterogeneous set of panel
/// closures, keyed by [`PanelKey`]. A panel builds exactly once, the first time
/// its region is authored; a redock remounts the *existing* node rather than
/// rerunning the builder (AGENTS section 8.1).
pub type PanelContent = Box<dyn Fn(&mut BuildCx<'_>)>;

/// The seam bar's fill — a neutral divider that reads against either pane, matching
/// the reference splitter's bar so a dock seam and a standalone splitter read alike.
const SEAM_FILL: Rgba = Rgba {
    r: 0.3,
    g: 0.31,
    b: 0.34,
    a: 1.0,
};

/// A tab chip's fill in the dock strip — a neutral chip against the strip.
const TAB_FILL: Rgba = Rgba {
    r: 0.16,
    g: 0.17,
    b: 0.20,
    a: 1.0,
};

/// The visual and layout parameters of a [`Dock`]: the seam bar's fill, a tab
/// chip's fill, and the control's own size request within its parent.
///
/// `size` defaults to [`Size::fill`] so the dock fills its parent region.
/// `seam` and `tab` default to neutral fills matching the reference splitter and
/// tab strip. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DockStyle {
    /// The control's own size request within its parent. Defaults to `Fill`.
    pub size: Size,
    /// The resize-seam bar's fill.
    pub seam: BoxStyle,
    /// A tab chip's fill in a dock region's tab strip.
    pub tab: BoxStyle,
}

impl Default for DockStyle {
    fn default() -> Self {
        DockStyle {
            size: Size::fill(),
            seam: BoxStyle::solid(SEAM_FILL),
            tab: BoxStyle::solid(TAB_FILL).with_radius(4.0),
        }
    }
}

/// An IDE-style container of dockable, draggable panels.
///
/// Construct one with [`dock`] over an initial [`DockTree`], register each panel's
/// content with [`Dock::panel`], and adjust appearance with the chainable
/// [`Dock::style`]/[`Dock::size`] setters. See the [module docs](self) for the
/// model and the scope of this version.
pub struct Dock {
    /// The initial dock arrangement — the warm source of truth the build walk reads.
    tree: DockTree,
    /// Each panel's content builder, keyed by its stable [`PanelKey`]. A key named
    /// by the tree but absent here builds an empty group (a defensive default; a
    /// well-formed dock registers every key it names).
    contents: HashMap<PanelKey, PanelContent>,
    style: DockStyle,
    /// The optional handle slot an application supplies to capture a [`DockHandle`]:
    /// `build` fills it once the tree, seams, and panel-node map exist, the same
    /// deferred-fill idiom the navigation stack uses. `None` when the app does not
    /// need imperative control.
    handle_slot: Option<DockHandleSlot>,
}

/// Construct a [`Dock`] over an initial [`DockTree`], with no panel content
/// registered yet. Chain [`Dock::panel`] to register each panel's content by key,
/// and [`Dock::style`]/[`Dock::size`] to adjust appearance.
pub fn dock(tree: DockTree) -> Dock {
    Dock {
        tree,
        contents: HashMap::new(),
        style: DockStyle::default(),
        handle_slot: None,
    }
}

impl Dock {
    /// Register the content builder for the panel with the given [`PanelKey`]. The
    /// builder authors the panel's subtree the first time the panel's region is
    /// built; registering the same key twice replaces the earlier builder.
    pub fn panel(mut self, key: PanelKey, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.contents.insert(key, Box::new(content));
        self
    }

    /// Replace the whole [`DockStyle`].
    pub fn style(mut self, style: DockStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the control's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Capture a [`DockHandle`] through a shared slot the application owns, to drive
    /// the dock imperatively (select a tab, redock, undock, float, or close a
    /// panel). `build` fills the slot once the retained nodes exist; the app reads
    /// `slot.borrow().clone()` after building. Without this the dock still renders
    /// and its seams still drag — only programmatic control is unavailable.
    pub fn handle(mut self, slot: &DockHandleSlot) -> Self {
        self.handle_slot = Some(Rc::clone(slot));
        self
    }
}

impl Component for Dock {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // The keyed panel-node map, filled once by the build walk and read by the
        // drag/reconcile steps (a later section) to remount an existing panel node
        // when it moves. A discrete-action lookup, never a per-frame path
        // (AGENTS section 45).
        let panels: PanelNodes = Rc::new(RefCell::new(HashMap::new()));

        // The walk's side-records: the seams a live drag drives, the zones a panel
        // drag hit-tests, the shared drag channel (drop-hint overlay, redock intent
        // queue, shared zone registry) the drag-to-redock handlers close over.
        // `empty` also mints the pre-authored drop-hint overlay leaf now, before the
        // region so it paints above the docked tree; the build walk fills the seams
        // and zones as it recurses.
        let mut out = BuildOut::empty(cx);

        // The dock is a named landmark region wrapping the docked tree, so an
        // assistive technology can navigate to it. The docked region tree is
        // authored under it; floating panels attach to an overlay container in a
        // later section.
        let region = cx.flex(
            viso_ui::FlexStyle {
                axis: viso_ui::Axis::Column,
                gap: 0.0,
                padding: viso_ui::Inset::all(0.0),
                align: viso_ui::Align::Stretch,
                size: self.style.size,
                style: BoxStyle::NONE,
            },
            |cx| {
                build::build_tree(
                    cx,
                    &self.tree.root,
                    &self.style,
                    &self.contents,
                    &panels,
                    &mut out,
                );
            },
        );
        cx.semantics(region, semantics::dock_container());

        // Share the zones the walk collected with the drag handlers, which close over
        // `zones_shared` (empty at wire time) and hit-test whatever it holds. The
        // reconcile step refreshes each zone's rect from its node's resolved bounds
        // before a drag resolves against it.
        *out.zones_shared.borrow_mut() = out.zones.clone();

        // If the app supplied a handle slot, fill it now that the retained nodes,
        // seams, zones, the drop-hint overlay, the intent queue, and the panel-node
        // map exist. The handle takes the dock's warm state forward: its own copy of
        // the tree (which the build walk has finished reading), the seams, the shared
        // drag channel (zones/hint/intents), and the shared panel-node map, so a
        // command or a drag drop edits the same arrangement the build authored. The
        // deferred-fill idiom the navigation stack uses — a builder chain cannot
        // return ids minted inside `build`.
        if let Some(slot) = &self.handle_slot {
            let handle = command::make_handle(
                self.tree.clone(),
                out.seams,
                out.floats,
                panels,
                out.zones_shared,
                out.hint,
                out.intents,
            );
            *slot.borrow_mut() = Some(handle);
        }
    }
}
