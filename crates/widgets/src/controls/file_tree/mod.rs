//! The [`FileTree`] control — a virtualized, keyboard-navigable tree browser.
//!
//! A `FileTree` renders a hierarchy of directories and files as an indented,
//! expand/collapse list: click a folder's arrow to reveal or hide its children,
//! navigate and expand/collapse from the keyboard, and select one or several
//! rows. It is built for large trees — a directory of 100k files does **not**
//! mount 100k nodes. The visible rows are the flatten of the tree under the set
//! of open folders, fed to a keyed virtual list, so only a window of rows (plus
//! overscan) is ever mounted, and an expand or collapse reuses the surviving
//! rows' host nodes rather than rebuilding the list (architecture section 12.4).
//!
//! Like [`Dock`](super::dock::Dock), this is a control that is a directory rather
//! than a single file (AGENTS section 5): the data model, the build walk, the
//! imperative command handle, the structural reconcile step, and the
//! accessibility contract each own a file.
//!
//! # The model, in one paragraph
//!
//! The tree is an owned [`TreeNode`] forest — the control's **warm** source of
//! truth — plus two [`NodeKey`]-keyed sets: which folders are open, and which
//! rows are selected. A `NodeKey` is a stable path identity, so a folder keeps its
//! expanded and selected state as the tree collapses, reopens, or reorders
//! (AGENTS section 8.6, 21.8). The model's [`flatten`](model::flatten) reads the
//! tree and the open set into the flat visible-row list the build walk authors and
//! the reconcile step re-drives. A click or a key writes an *intent*; the
//! reconcile step (holding `&mut NodeStore` and `&mut VirtualLists`) edits the
//! open/selection/focus state, reflattens, and drives the keyed list — the same
//! handler-writes-intent / reconcile-applies split the dock and the virtualized
//! list use, and the reason this control opens an ADR.
//!
//! # Registering a tree
//!
//! ```
//! use viso_widgets::{file_tree, NodeKey, TreeNode};
//!
//! let control = file_tree(vec![TreeNode::dir(
//!     NodeKey(0),
//!     "src",
//!     vec![
//!         TreeNode::file(NodeKey(1), "main.rs"),
//!         TreeNode::file(NodeKey(2), "lib.rs"),
//!     ],
//! )]);
//! let _ = control;
//! ```
//!
//! # Scope of this first version
//!
//! **In:** an owned tree keyed by a stable [`NodeKey`] (a path identity);
//! expand/collapse as a structural reconcile over a keyed virtual list (so the
//! large-tree virtualization and surviving-row reuse come for free); indent by
//! depth; single and multiple selection; keyboard navigation (Up/Down,
//! Left = collapse-or-parent, Right = expand-or-first-child, Home/End, Space to
//! toggle selection, Shift to range-select), with a mouse equivalent for every
//! action; `Tree`/`TreeItem` accessibility with `aria-expanded`/`aria-selected`.
//!
//! **Out (explicitly deferred):** lazy / async directory loading (the tree is
//! supplied whole; the virtual list makes mounting lazy already — async
//! filesystem loading awaits Viso's services story); drag-to-reorder nodes (a
//! future consumer of the dock's drag mechanism); in-row rename editing (a future
//! consumer of `TextInput`); extra columns (size/mtime); persisting the expansion
//! state across runs (awaits Viso's serialization story); touch / gesture
//! arbitration beyond the primary pointer.

mod build;
mod command;
pub mod model;
mod reconcile;

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use viso_ui::{BoxStyle, BuildCx, Component, NodeId, Size, VirtualListStyle};

pub use command::{FileTreeHandle, FileTreeHandleSlot};
pub use model::{NodeKey, SelectOp, TreeNode, VisibleRow};

use model::flatten;

/// The keyed map from a row's [`NodeKey`] to the host node the build walk mounted
/// for it, shared with the reconcile step (a later section) so it can address a
/// row by key rather than searching the arena — a discrete-action lookup, never a
/// per-frame path (AGENTS section 45).
pub(crate) type RowNodes = Rc<RefCell<HashMap<NodeKey, NodeId>>>;

/// The flattened visible-row list, shared between the build walk (whose keyed
/// `key_of` and row body read it live) and the reconcile step (which reflattens
/// into it on an expand/collapse, then drives the list's item count). A **warm**
/// cell: rewritten only on a discrete toggle, never a per-frame path — the shared
/// cell is what lets the substrate diff by [`NodeKey`] and reuse surviving rows'
/// hosts across a structural reconcile (architecture section 12.4).
pub(crate) type VisibleRows = Rc<RefCell<Vec<VisibleRow>>>;

/// The key-to-label index, resolved once from the tree and shared with the row
/// builder so a row body never walks the tree at mount time. Cold data (the
/// display strings) behind an [`Rc`], read only when a row mounts (AGENTS
/// section 8.4).
pub(crate) type Labels = Rc<HashMap<NodeKey, String>>;

/// How many rows a [`FileTree`] lets the user select at once.
///
/// `Single` (the default) keeps exactly one row selected — selecting a new row
/// replaces the selection. `Multi` lets a set of rows be selected together, via
/// Space / modifier-click to toggle a single row and Shift to extend a range. The
/// mode only bounds what selection *edits* are allowed; the selection itself is
/// always a [`NodeKey`] set on the warm model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SelectMode {
    /// At most one row selected; a new selection replaces the previous.
    #[default]
    Single,
    /// Any number of rows selected together.
    Multi,
}

/// The default row height in logical pixels — a comfortable single-line row.
const DEFAULT_ROW_HEIGHT: f32 = 22.0;
/// The default per-depth indent in logical pixels.
const DEFAULT_INDENT: f32 = 16.0;
/// The default width of the disclosure-arrow column in logical pixels.
const DEFAULT_ARROW_WIDTH: f32 = 14.0;
/// The default gap between a row's arrow column and its label.
const DEFAULT_GAP: f32 = 2.0;

/// The visual and layout parameters of a [`FileTree`]: the control's own size, the
/// per-row height, the per-depth indent, the disclosure-arrow column width, the
/// arrow/label gap, the overscan window, and the viewport's background.
///
/// `size` defaults to [`Size::fill`] so the tree fills its parent region. The row
/// metrics default to a compact single-line row. All fields are `Copy`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FileTreeStyle {
    /// The control's own size request within its parent. Defaults to `Fill`.
    pub size: Size,
    /// Each row's main-axis height in logical pixels.
    pub row_height: f32,
    /// The horizontal indent added per depth level, in logical pixels.
    pub indent: f32,
    /// The width of the fixed disclosure-arrow column, in logical pixels.
    pub arrow_width: f32,
    /// The gap between the arrow column and the label, in logical pixels.
    pub gap: f32,
    /// Extra rows mounted on each side of the visible window, so a small scroll
    /// reveals an already-built row.
    pub overscan: u32,
    /// The viewport's own background/border. [`BoxStyle::NONE`] is a transparent
    /// clip box.
    pub background: BoxStyle,
}

impl Default for FileTreeStyle {
    fn default() -> Self {
        let base = VirtualListStyle::default();
        FileTreeStyle {
            size: Size::fill(),
            row_height: DEFAULT_ROW_HEIGHT,
            indent: DEFAULT_INDENT,
            arrow_width: DEFAULT_ARROW_WIDTH,
            gap: DEFAULT_GAP,
            overscan: base.overscan,
            background: BoxStyle::NONE,
        }
    }
}

/// A virtualized, keyboard-navigable tree browser.
///
/// Construct one with [`file_tree`] over a [`TreeNode`] forest, choose the
/// selection mode and initial open folders with the chainable setters, and adjust
/// appearance with [`FileTree::style`]/[`FileTree::size`]. See the
/// [module docs](self) for the model and the scope of this version.
pub struct FileTree {
    /// The tree forest — the warm source of truth the build walk reads.
    roots: Vec<TreeNode>,
    /// The initially open folders, by key. A folder not here starts collapsed.
    open: HashSet<NodeKey>,
    /// Whether the tree allows single or multiple selection.
    select_mode: SelectMode,
    style: FileTreeStyle,
    /// An optional slot the build fills with the control's [`FileTreeHandle`], so
    /// an application can drive expand/collapse programmatically. `None` when the
    /// app never asked for a handle (a display-only tree).
    handle: Option<FileTreeHandleSlot>,
}

/// Construct a [`FileTree`] over a [`TreeNode`] forest, with every folder
/// collapsed and single selection. Chain [`FileTree::open`] to pre-expand
/// folders, [`FileTree::select_mode`] to allow multiple selection, and
/// [`FileTree::style`]/[`FileTree::size`] to adjust appearance.
pub fn file_tree(roots: Vec<TreeNode>) -> FileTree {
    FileTree {
        roots,
        open: HashSet::new(),
        select_mode: SelectMode::default(),
        style: FileTreeStyle::default(),
        handle: None,
    }
}

impl FileTree {
    /// Pre-expand the folder with the given key, so it renders open on first
    /// build. Call once per folder to open; a key naming a file is ignored by the
    /// flatten (a file never discloses).
    pub fn open(mut self, key: NodeKey) -> Self {
        self.open.insert(key);
        self
    }

    /// Set the selection mode (single by default).
    pub fn select_mode(mut self, mode: SelectMode) -> Self {
        self.select_mode = mode;
        self
    }

    /// Replace the whole [`FileTreeStyle`].
    pub fn style(mut self, style: FileTreeStyle) -> Self {
        self.style = style;
        self
    }

    /// Set the control's own size request within its parent (defaults to `Fill`).
    pub fn size(mut self, size: Size) -> Self {
        self.style.size = size;
        self
    }

    /// Ask the build to fill `slot` with this control's [`FileTreeHandle`], so an
    /// application can drive expand/collapse programmatically. Create the slot with
    /// [`FileTreeHandleSlot::default`], pass it here, then read it after `build`.
    /// The handle is minted inside `build` (it owns the warm state the walk
    /// produces), so it cannot be returned by the builder chain — the app supplies
    /// this slot up front and `build` fills it once, the same deferred-fill idiom
    /// the dock's `DockHandleSlot` uses.
    pub fn handle(mut self, slot: FileTreeHandleSlot) -> Self {
        self.handle = Some(slot);
        self
    }
}

impl Component for FileTree {
    fn build(&self, cx: &mut BuildCx<'_>) {
        // The warm cells the build walk and the reconcile step share: the
        // flattened visible rows (the keyed list's `key_of` and row body read this
        // live, so a reconcile can reflatten into it and grow/shrink the item
        // count), the key-to-label index (resolved once, so a row body never walks
        // the tree at mount), and the keyed row-node map the reconcile step
        // addresses a row by. Cloning the roots into the handle's warm state below
        // hands the reconcile step an owned tree to reflatten against, mirroring the
        // dock's owned `DockTree` (module docs).
        let visible: VisibleRows = Rc::new(RefCell::new(flatten(&self.roots, &self.open)));
        let labels: Labels = Rc::new(build::label_index(&self.roots));
        let row_nodes: RowNodes = Rc::new(RefCell::new(HashMap::new()));

        let viewport = build::build_tree(cx, &visible, &labels, &self.style, &row_nodes);

        // Mint the handle over the warm state the walk produced, and fill the app's
        // slot if it asked for one. `select_mode` rides on the warm state so the
        // keyboard/selection section can read it; this version wires expand/collapse
        // only.
        if let Some(slot) = &self.handle {
            let handle = command::make_handle(
                self.roots.clone(),
                self.open.clone(),
                self.select_mode,
                viewport,
                visible,
                row_nodes,
            );
            *slot.borrow_mut() = Some(handle);
        }
    }
}
