//! The **red tree**: a navigable, position-aware view layered over the immutable
//! [`GreenNode`] tree.
//!
//! The green tree ([`super::cst`]) is offset-relative and parent-unaware — a
//! subtree carries only its own length so it can be shared and spliced. That is
//! exactly what a compiler wants for storage, but a formatter, an LSP, name
//! resolution, and the typed AST all need the opposite: absolute positions, a way
//! to walk *up* to a parent, and stable per-node identity. The red tree supplies
//! those without duplicating the tree — each red node is a thin handle carrying a
//! pointer to its green node plus the context the green node lacks: its parent,
//! its absolute start offset, and its index among the parent's children.
//!
//! This is the rust-analyzer red/green design. Red nodes are created on demand as
//! you navigate and cached per parent so repeated `children()`/`parent()` calls
//! over the same subtree return the same `Rc`-shared handles (pointer-stable
//! identity, and no re-walk). It is a cold-path editor/compiler structure, so the
//! `Rc` sharing and the interior-mutable child cache are deliberate and allowed
//! (AGENTS section 7.2).
//!
//! Identity: two [`SyntaxNode`]s are the same tree position when they wrap the
//! same green node pointer at the same absolute offset — see [`SyntaxNode::eq`].
//! That is stronger than green equality (an identical subtree can appear twice)
//! and is what rename/goto/reference tooling keys on.

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use super::cst::{GreenChild, GreenNode, GreenToken};
use super::kind::SyntaxKind;
use super::span::{TextRange, TextSize};

/// The shared, mutable inside of a [`SyntaxNode`]: everything a red node needs to
/// know its place in the tree, plus a lazily-filled cache of its red children so
/// navigation is stable and cheap on repeat.
struct NodeData {
    /// The green node this red node views. The single source of truth for kind,
    /// length, and children.
    green: Rc<GreenNode>,
    /// The parent red node, or `None` for the root. Held as an `Rc` so walking up
    /// never rebuilds ancestors.
    parent: Option<SyntaxNode>,
    /// This node's absolute start offset in the source (the root's is zero).
    offset: TextSize,
    /// This node's index among its parent's green children (`0` for the root).
    index_in_parent: usize,
    /// Lazily-materialized red children, one slot per green child, filled on first
    /// access so repeated navigation returns the same handles. `None` = not yet
    /// built; `Some(None)` = a token slot (no red *node* there).
    children: RefCell<Option<Vec<Option<SyntaxNode>>>>,
}

/// A node in the red tree: a positioned, navigable handle onto a [`GreenNode`].
///
/// Cheap to clone (`Rc` bump). Cloning yields another handle to the *same* tree
/// position, not a copy of the subtree.
#[derive(Clone)]
pub struct SyntaxNode {
    data: Rc<NodeData>,
}

/// A leaf in the red tree: a positioned handle onto a [`GreenToken`].
///
/// A token has no children, so it is modeled as its parent node plus the index of
/// the green token in that parent's child list, together with its absolute offset.
/// The green token itself is read back by indexing the parent's green children.
#[derive(Clone)]
pub struct SyntaxToken {
    parent: SyntaxNode,
    /// The token's index among its parent's green children.
    index_in_parent: usize,
    /// The token's absolute start offset in the source.
    offset: TextSize,
}

/// A node or a token: the element type for mixed child/sibling iteration, so a
/// caller can walk every child in source order (trivia and tokens included) and
/// not just the interior nodes.
#[derive(Clone)]
pub enum SyntaxElement {
    /// An interior node.
    Node(SyntaxNode),
    /// A leaf token.
    Token(SyntaxToken),
}

impl SyntaxNode {
    /// Builds the root red node for a green tree. Its offset is zero, it has no
    /// parent, and its index is zero.
    pub fn new_root(green: Rc<GreenNode>) -> SyntaxNode {
        SyntaxNode {
            data: Rc::new(NodeData {
                green,
                parent: None,
                offset: TextSize::ZERO,
                index_in_parent: 0,
                children: RefCell::new(None),
            }),
        }
    }

    /// The node's syntax kind.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.data.green.kind()
    }

    /// The green node this red node views.
    #[inline]
    pub fn green(&self) -> &Rc<GreenNode> {
        &self.data.green
    }

    /// The node's absolute byte range in the source.
    #[inline]
    pub fn text_range(&self) -> TextRange {
        TextRange::new(
            self.data.offset,
            self.data.offset + self.data.green.text_len(),
        )
    }

    /// The node's index among its parent's children (`0` for the root).
    #[inline]
    pub fn index_in_parent(&self) -> usize {
        self.data.index_in_parent
    }

    /// This node's parent, or `None` if it is the root.
    #[inline]
    pub fn parent(&self) -> Option<SyntaxNode> {
        self.data.parent.clone()
    }

    /// Reconstructs the node's source text (equals the original slice for this
    /// span; for the root, the whole source byte-for-byte).
    pub fn text(&self) -> String {
        self.data.green.text()
    }

    /// Ensures the child cache is materialized, then runs `f` over it. Building
    /// the cache walks the green children once, accumulating the running offset so
    /// each red child records its absolute position and index.
    fn with_children<R>(&self, f: impl FnOnce(&[Option<SyntaxNode>]) -> R) -> R {
        if self.data.children.borrow().is_none() {
            let mut built = Vec::with_capacity(self.data.green.children().len());
            let mut offset = self.data.offset;
            for (index, child) in self.data.green.children().iter().enumerate() {
                match child {
                    GreenChild::Node(green) => {
                        built.push(Some(SyntaxNode {
                            data: Rc::new(NodeData {
                                green: Rc::clone(green),
                                parent: Some(self.clone()),
                                offset,
                                index_in_parent: index,
                                children: RefCell::new(None),
                            }),
                        }));
                    }
                    GreenChild::Token(_) => built.push(None),
                }
                offset = offset + child.text_len();
            }
            *self.data.children.borrow_mut() = Some(built);
        }
        let borrow = self.data.children.borrow();
        f(borrow.as_ref().expect("children just built"))
    }

    /// This node's interior child *nodes* in source order (tokens skipped). Use
    /// [`SyntaxNode::children_with_tokens`] to include tokens.
    pub fn children(&self) -> Vec<SyntaxNode> {
        self.with_children(|slots| slots.iter().flatten().cloned().collect())
    }

    /// This node's children as [`SyntaxElement`]s in source order — nodes *and*
    /// tokens, so the walk is lossless.
    pub fn children_with_tokens(&self) -> Vec<SyntaxElement> {
        self.with_children(|slots| {
            let mut offset = self.data.offset;
            let mut out = Vec::with_capacity(slots.len());
            for (index, (slot, green_child)) in
                slots.iter().zip(self.data.green.children()).enumerate()
            {
                match slot {
                    Some(node) => out.push(SyntaxElement::Node(node.clone())),
                    None => out.push(SyntaxElement::Token(SyntaxToken {
                        parent: self.clone(),
                        index_in_parent: index,
                        offset,
                    })),
                }
                offset = offset + green_child.text_len();
            }
            out
        })
    }

    /// The first interior child node, if any.
    pub fn first_child(&self) -> Option<SyntaxNode> {
        self.with_children(|slots| slots.iter().flatten().next().cloned())
    }

    /// The last interior child node, if any.
    pub fn last_child(&self) -> Option<SyntaxNode> {
        self.with_children(|slots| slots.iter().flatten().next_back().cloned())
    }

    /// The next sibling node after this one (skipping tokens), if any.
    pub fn next_sibling(&self) -> Option<SyntaxNode> {
        let parent = self.data.parent.as_ref()?;
        parent.with_children(|slots| {
            slots[self.data.index_in_parent + 1..]
                .iter()
                .flatten()
                .next()
                .cloned()
        })
    }

    /// The previous sibling node before this one (skipping tokens), if any.
    pub fn prev_sibling(&self) -> Option<SyntaxNode> {
        let parent = self.data.parent.as_ref()?;
        parent.with_children(|slots| {
            slots[..self.data.index_in_parent]
                .iter()
                .flatten()
                .next_back()
                .cloned()
        })
    }

    /// This node and its ancestors, innermost first, ending at the root.
    pub fn ancestors(&self) -> Ancestors {
        Ancestors {
            next: Some(self.clone()),
        }
    }

    /// Every descendant node in pre-order (this node first), tokens excluded.
    pub fn descendants(&self) -> Vec<SyntaxNode> {
        let mut out = Vec::new();
        self.collect_descendants(&mut out);
        out
    }

    fn collect_descendants(&self, out: &mut Vec<SyntaxNode>) {
        out.push(self.clone());
        for child in self.children() {
            child.collect_descendants(out);
        }
    }

    /// Every descendant element (nodes and tokens) in pre-order — the lossless
    /// walk. The node itself is included as the first element.
    pub fn descendants_with_tokens(&self) -> Vec<SyntaxElement> {
        let mut out = Vec::new();
        out.push(SyntaxElement::Node(self.clone()));
        self.collect_elements(&mut out);
        out
    }

    fn collect_elements(&self, out: &mut Vec<SyntaxElement>) {
        for element in self.children_with_tokens() {
            match element {
                SyntaxElement::Node(node) => {
                    out.push(SyntaxElement::Node(node.clone()));
                    node.collect_elements(out);
                }
                token @ SyntaxElement::Token(_) => out.push(token),
            }
        }
    }

    /// Whether two handles denote the same tree position: the same green node
    /// pointer at the same absolute offset. Stronger than green-value equality,
    /// which an identical repeated subtree would also satisfy.
    #[inline]
    pub fn ptr_eq(&self, other: &SyntaxNode) -> bool {
        self.data.offset == other.data.offset && Rc::ptr_eq(&self.data.green, &other.data.green)
    }
}

/// An iterator over a node and its ancestors, innermost first (see
/// [`SyntaxNode::ancestors`]).
pub struct Ancestors {
    next: Option<SyntaxNode>,
}

impl Iterator for Ancestors {
    type Item = SyntaxNode;

    fn next(&mut self) -> Option<SyntaxNode> {
        let current = self.next.take()?;
        self.next = current.parent();
        Some(current)
    }
}

impl PartialEq for SyntaxNode {
    #[inline]
    fn eq(&self, other: &SyntaxNode) -> bool {
        self.ptr_eq(other)
    }
}

impl Eq for SyntaxNode {}

impl fmt::Debug for SyntaxNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}@{:?}", self.kind(), self.text_range())
    }
}

impl SyntaxToken {
    /// The green token this red token views, read back from the parent's children.
    fn green(&self) -> &GreenToken {
        match &self.parent.data.green.children()[self.index_in_parent] {
            GreenChild::Token(t) => t,
            GreenChild::Node(_) => {
                unreachable!("SyntaxToken index must point at a green token")
            }
        }
    }

    /// The token's syntax kind.
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        self.green().kind()
    }

    /// The token's source spelling.
    pub fn text(&self) -> String {
        self.green().text().to_string()
    }

    /// The token's absolute byte range in the source.
    #[inline]
    pub fn text_range(&self) -> TextRange {
        TextRange::new(self.offset, self.offset + self.green().text_len())
    }

    /// The token's index among its parent's children.
    #[inline]
    pub fn index_in_parent(&self) -> usize {
        self.index_in_parent
    }

    /// The node this token is a child of.
    #[inline]
    pub fn parent(&self) -> SyntaxNode {
        self.parent.clone()
    }
}

impl fmt::Debug for SyntaxToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:?}@{:?} {:?}",
            self.kind(),
            self.text_range(),
            self.text()
        )
    }
}

impl SyntaxElement {
    /// The element's syntax kind (node or token).
    #[inline]
    pub fn kind(&self) -> SyntaxKind {
        match self {
            SyntaxElement::Node(n) => n.kind(),
            SyntaxElement::Token(t) => t.kind(),
        }
    }

    /// The element's absolute byte range.
    #[inline]
    pub fn text_range(&self) -> TextRange {
        match self {
            SyntaxElement::Node(n) => n.text_range(),
            SyntaxElement::Token(t) => t.text_range(),
        }
    }

    /// The node, if this element is one.
    pub fn as_node(&self) -> Option<&SyntaxNode> {
        match self {
            SyntaxElement::Node(n) => Some(n),
            SyntaxElement::Token(_) => None,
        }
    }

    /// The token, if this element is one.
    pub fn as_token(&self) -> Option<&SyntaxToken> {
        match self {
            SyntaxElement::Token(t) => Some(t),
            SyntaxElement::Node(_) => None,
        }
    }
}

impl fmt::Debug for SyntaxElement {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SyntaxElement::Node(n) => write!(f, "{n:?}"),
            SyntaxElement::Token(t) => write!(f, "{t:?}"),
        }
    }
}
