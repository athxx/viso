//! Links: the stretches of a text that lead somewhere.
//!
//! A text's spans name where they lead ([`SpanStyle::link`]); a text with
//! any is a link host. The mouse shows the hand over a link, and a press and
//! release on the same link activates it, which the host's key handler and
//! then its ancestors' hear ([`EventCx::link`]). A host takes the focus in
//! the ring one link at a time: Tab moves among its links before it moves
//! on, Enter activates the focused one, and the focused link wears a ring
//! while the keyboard moved the focus. Where each link draws is the host's
//! shaped content's ([`TextInk::links`]), so a link is hit where it shows.
//! The table is cold and small: one entry for each host.
//!
//! A link the handlers that heard it did not keep — none called
//! [`EventCx::prevent_default`] — opens: it waits in the store until the
//! driver hands it to the system ([`NodeStore::take_link_opens`]), as a
//! browser follows a link no script took.
//!
//! [`SpanStyle::link`]: crate::text_style::SpanStyle::link
//! [`EventCx::link`]: crate::context::EventCx::link
//! [`EventCx::prevent_default`]: crate::context::EventCx::prevent_default
//! [`TextInk::links`]: crate::content::TextInk::links

use std::sync::Arc;

use crate::component::NodeStore;
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::hit_test::HitTestTree;
use crate::input::PointerId;
use crate::node::NodeId;
use viso_render::Rect;

/// One text's links and what the pointer and the keyboard do to them.
#[derive(Debug)]
struct Host {
    node: NodeId,
    /// Where each link leads, in the order they appear in the text.
    targets: Vec<Arc<str>>,
    /// Each link's text, its accessible name.
    labels: Vec<Box<str>>,
    /// The link under the mouse.
    hovered: Option<u16>,
    /// The link a pointer pressed and has not let go of.
    pressed: Option<(PointerId, u16)>,
    /// The link the focus is on while the host holds it.
    focused: u16,
}

/// The link hosts of a store. Cold.
#[derive(Debug, Default)]
pub(crate) struct Links {
    hosts: Vec<Host>,
    /// The activated links no handler kept, oldest first, to open.
    opens: Vec<Arc<str>>,
}

impl Links {
    pub(crate) fn clear(&mut self) {
        self.hosts.clear();
        self.opens.clear();
    }

    /// Forgets the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        if !self.hosts.is_empty() {
            self.hosts.retain(|h| h.node.index() != index);
        }
    }

    fn host(&self, node: NodeId) -> Option<&Host> {
        self.hosts.iter().find(|h| h.node == node)
    }

    fn host_mut(&mut self, node: NodeId) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.node == node)
    }
}

/// What a pointer did to a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPointer {
    Moved,
    Down,
    Up,
    /// It left the window, or its contact was cancelled.
    Left,
}

impl NodeStore {
    /// Makes `targets` the links of node `id`'s text, in the order they
    /// appear, each named by its text in `labels`; none makes it no host. A
    /// host takes the focus.
    pub(crate) fn set_links(&mut self, id: NodeId, targets: Vec<Arc<str>>, labels: Vec<Box<str>>) {
        let at = self.links.hosts.iter().position(|h| h.node == id);
        match (at, targets.is_empty()) {
            (Some(at), true) => {
                self.links.hosts.swap_remove(at);
            }
            (Some(at), false) => {
                let host = &mut self.links.hosts[at];
                if host.targets != targets {
                    host.focused = host.focused.min(targets.len() as u16 - 1);
                    host.hovered = None;
                    host.pressed = None;
                    host.targets = targets;
                }
                if host.labels != labels {
                    host.labels = labels;
                    self.mark_dirty(id, DirtyClass::SEMANTICS);
                }
            }
            (None, false) if self.arena().is_live(id) => {
                self.links.hosts.push(Host {
                    node: id,
                    targets,
                    labels,
                    hovered: None,
                    pressed: None,
                    focused: 0,
                });
                self.set_focusable(id, true);
            }
            _ => {}
        }
    }

    /// Queues `target`, a link activated that no handler kept, to open.
    pub(crate) fn queue_link_open(&mut self, target: &str) {
        self.links.opens.push(target.into());
    }

    /// The links to open since the last call, oldest first: the driver hands
    /// each to the system, which opens it in the app its URL belongs to.
    pub fn take_link_opens(&mut self) -> Vec<Arc<str>> {
        std::mem::take(&mut self.links.opens)
    }

    /// Where node `id`'s links lead, in order; empty for no host.
    pub fn links(&self, id: NodeId) -> &[Arc<str>] {
        self.links.host(id).map_or(&[], |h| &h.targets)
    }

    /// The text of link `link` of node `id`.
    pub fn link_label(&self, id: NodeId, link: u16) -> Option<&str> {
        let host = self.links.host(id)?;
        host.labels.get(usize::from(link)).map(|l| &**l)
    }

    /// The link of node `id` the focus is on, while `id` holds the focus.
    pub fn focused_link(&self, id: NodeId) -> Option<u16> {
        let host = self.links.host(id)?;
        (self.focused() == Some(id)).then_some(host.focused)
    }

    /// Whether the mouse is over one of node `id`'s links.
    pub(crate) fn over_link(&self, id: NodeId) -> bool {
        self.links.host(id).is_some_and(|h| h.hovered.is_some())
    }

    /// Where link `link` of node `id` draws, in the window: one box for each
    /// line it is on.
    pub fn link_boxes(&self, id: NodeId, link: u16) -> impl Iterator<Item = Rect> + '_ {
        let world = self.world(id);
        let boxes = match self.content_payload(id) {
            Some(Content::Text { ink: Some(ink), .. }) => &ink.links[..],
            _ => &[],
        };
        boxes
            .iter()
            .filter(move |b| b.link == link)
            .map(move |b| Rect {
                x: world.x + b.rect.x,
                y: world.y + b.rect.y,
                ..b.rect
            })
    }

    /// The link of node `id` at window point `(x, y)`.
    fn link_at(&self, id: NodeId, x: f32, y: f32) -> Option<u16> {
        let world = self.world(id);
        let (x, y) = (x - world.x, y - world.y);
        match self.content_payload(id) {
            Some(Content::Text { ink: Some(ink), .. }) => ink
                .links
                .iter()
                .find(|b| b.rect.contains(x, y))
                .map(|b| b.link),
            _ => None,
        }
    }

    /// What `pointer` doing `what` at window point `(x, y)` in the tree
    /// under `root` did to the links: the host and where its link leads when
    /// it activated one. Call after the pointer is routed.
    pub fn link_pointer(
        &mut self,
        root: NodeId,
        pointer: PointerId,
        what: LinkPointer,
        x: f32,
        y: f32,
    ) -> Option<(NodeId, Arc<str>)> {
        if self.links.hosts.is_empty() {
            return None;
        }
        let over = (what != LinkPointer::Left)
            .then(|| HitTestTree::hit(self, root, x, y))
            .flatten();
        let under = over
            .filter(|&n| !self.inert(n))
            .and_then(|n| Some((n, self.link_at(n, x, y)?)));
        let focus = self.focused();
        let mut activated = None;
        let mut repaint = None;
        for host in &mut self.links.hosts {
            let here = under.filter(|(n, _)| *n == host.node).map(|(_, l)| l);
            if pointer == PointerId::MOUSE {
                let hovered = if what == LinkPointer::Left {
                    None
                } else {
                    here
                };
                host.hovered = hovered;
            }
            match what {
                LinkPointer::Down => {
                    if let Some(link) = here {
                        host.pressed = Some((pointer, link));
                        if host.focused != link && focus == Some(host.node) {
                            repaint = Some(host.node);
                        }
                        host.focused = link;
                    }
                }
                LinkPointer::Up => {
                    if let Some((by, link)) = host.pressed
                        && by == pointer
                    {
                        host.pressed = None;
                        if here == Some(link) {
                            activated = Some((host.node, Arc::clone(&host.targets[link as usize])));
                        }
                    }
                }
                LinkPointer::Left => {
                    if host.pressed.is_some_and(|(by, _)| by == pointer) {
                        host.pressed = None;
                    }
                }
                LinkPointer::Moved => {}
            }
        }
        if let Some(node) = repaint {
            self.mark_dirty(node, DirtyClass::PAINT | DirtyClass::SEMANTICS);
        }
        activated
    }

    /// The focused host and where its focused link leads, when `pressed`
    /// Enter activates it.
    pub fn link_key(&self, enter: bool, pressed: bool) -> Option<(NodeId, Arc<str>)> {
        let node = self.focused()?;
        if !(enter && pressed) || self.inert(node) {
            return None;
        }
        let host = self.links.host(node)?;
        let target = host.targets.get(usize::from(host.focused))?;
        Some((node, Arc::clone(target)))
    }

    /// Moves the focus on host `id` one link `forward` or back; `false` at its
    /// last link (or first, going back), where the focus leaves it.
    pub(crate) fn step_link(&mut self, id: NodeId, forward: bool) -> bool {
        let Some(host) = self.links.host_mut(id) else {
            return false;
        };
        let last = host.targets.len() as u16 - 1;
        let next = match (forward, host.focused) {
            (true, at) if at < last => at + 1,
            (false, at) if at > 0 => at - 1,
            _ => return false,
        };
        host.focused = next;
        self.mark_dirty(id, DirtyClass::PAINT | DirtyClass::SEMANTICS);
        true
    }

    /// Puts the focus of host `id` on its link `link`, as an assistive
    /// technology's focus on that link does.
    pub fn focus_link(&mut self, id: NodeId, link: u16) {
        if let Some(host) = self.links.host_mut(id)
            && usize::from(link) < host.targets.len()
            && host.focused != link
        {
            host.focused = link;
            self.mark_dirty(id, DirtyClass::PAINT | DirtyClass::SEMANTICS);
        }
    }

    /// The focus comes to host `id` going `forward` (or back): onto its first
    /// link (or last).
    pub(crate) fn enter_links(&mut self, id: NodeId, forward: bool) {
        if let Some(host) = self.links.host_mut(id) {
            let to = if forward {
                0
            } else {
                host.targets.len() as u16 - 1
            };
            if std::mem::replace(&mut host.focused, to) != to {
                self.mark_dirty(id, DirtyClass::PAINT | DirtyClass::SEMANTICS);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::BuildCx;
    use crate::content::{LinkBox, TextInk, TextRequest};
    use crate::semantics::Role;
    use crate::text_style::{SpanStyle, TextSpan, WidthUse};
    use crate::{FlexStyle, FontSize, LeafStyle, Sizing, Vec2, focus_next};
    use viso_render::{Rgba, TextureId};

    /// A text "see docs and site" whose "docs" and "site" are links drawn at
    /// x 30..70 and 100..140 of its 200 by 20 box, then a focusable box.
    fn scene() -> (NodeStore, NodeId, NodeId, NodeId) {
        let mut store = NodeStore::new();
        let (mut text, mut next) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    size: Sizing::fixed(200.0, 100.0),
                    ..Default::default()
                },
                |cx| {
                    let leaf = cx.leaf(LeafStyle {
                        size: Sizing::fixed(200.0, 20.0),
                        ..Default::default()
                    });
                    text = Some(leaf.id());
                    let button = cx.leaf(LeafStyle {
                        size: Sizing::fixed(50.0, 20.0),
                        ..Default::default()
                    });
                    cx.focusable(button, true);
                    next = Some(button.id());
                },
            )
            .id()
        };
        let (text, next) = (text.unwrap(), next.unwrap());
        let link = |range: std::ops::Range<usize>, target: &str| TextSpan {
            range,
            style: SpanStyle::default().link(target),
        };
        store.set_text_request(
            text,
            TextRequest {
                text: "see docs and site".into(),
                font_size: FontSize::Fixed(14.0),
                color: Rgba::TRANSPARENT,
                soft_wrap: false,
                locale: None,
                style: Default::default(),
                spans: vec![link(4..8, "/docs"), link(13..17, "https://viso.dev")],
            },
        );
        let boxes = [(0, 30.0), (1, 100.0)].map(|(link, x)| LinkBox {
            link,
            rect: Rect::new(x, 0.0, 40.0, 20.0),
        });
        store.set_content_payload(
            text,
            Content::Text {
                ink: Some(Box::new(TextInk {
                    links: boxes.to_vec(),
                    ..TextInk::default()
                })),
                glyphs: Vec::new(),
                atlas: TextureId::new(1),
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: Rgba::TRANSPARENT,
                natural: Vec2 { x: 200.0, y: 20.0 },
                baseline: 15.0,
                shaped_at_width: None,
                width_use: WidthUse::Ignored,
                min_content: 200.0,
                max_content: 200.0,
            },
        );
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        (store, root, text, next)
    }

    #[test]
    fn a_press_and_release_on_one_link_activates_it_and_the_hand_shows_over_it() {
        let (mut store, root, text, _) = scene();
        assert!(store.focusable(text), "a text of links takes the focus");
        let mouse = PointerId::MOUSE;
        assert_eq!(
            store.link_pointer(root, mouse, LinkPointer::Moved, 10.0, 10.0),
            None
        );
        assert!(!store.over_link(text));
        store.link_pointer(root, mouse, LinkPointer::Moved, 35.0, 10.0);
        assert!(store.over_link(text), "the mouse is over \"docs\"");
        store.link_pointer(root, mouse, LinkPointer::Down, 35.0, 10.0);
        assert_eq!(
            store.link_pointer(root, mouse, LinkPointer::Up, 60.0, 12.0),
            Some((text, Arc::from("/docs")))
        );
        store.link_pointer(root, mouse, LinkPointer::Down, 35.0, 10.0);
        assert_eq!(
            store.link_pointer(root, mouse, LinkPointer::Up, 110.0, 10.0),
            None,
            "a release on another link activates neither"
        );
        store.link_pointer(root, mouse, LinkPointer::Down, 105.0, 10.0);
        store.link_pointer(root, mouse, LinkPointer::Left, 105.0, 10.0);
        assert_eq!(
            store.link_pointer(root, mouse, LinkPointer::Up, 105.0, 10.0),
            None,
            "a cancelled press activates nothing"
        );
        store.link_pointer(root, mouse, LinkPointer::Left, 0.0, 0.0);
        assert!(!store.over_link(text), "the mouse left the window");
    }

    #[test]
    fn tab_steps_through_the_links_before_it_leaves_and_enter_activates_the_focused_one() {
        let (mut store, root, text, next) = scene();
        assert_eq!(store.link_key(true, true), None, "nothing has the focus");
        focus_next(&mut store, root, true);
        assert_eq!(store.focused(), Some(text));
        assert_eq!(store.focused_link(text), Some(0));
        assert_eq!(store.link_key(true, true), Some((text, Arc::from("/docs"))));
        assert_eq!(store.link_key(false, true), None, "only Enter");
        assert_eq!(store.link_key(true, false), None, "on its press");
        focus_next(&mut store, root, true);
        assert_eq!(store.focused_link(text), Some(1));
        let boxes: Vec<Rect> = store.link_boxes(text, 1).collect();
        assert_eq!(boxes, [Rect::new(100.0, 0.0, 40.0, 20.0)]);
        focus_next(&mut store, root, true);
        assert_eq!(store.focused(), Some(next), "past the last link");
        focus_next(&mut store, root, false);
        assert_eq!(
            store.focused_link(text),
            Some(1),
            "back in, onto the last link"
        );
        focus_next(&mut store, root, false);
        assert_eq!(store.focused_link(text), Some(0));
    }

    #[test]
    fn each_link_is_a_link_row_under_its_text_named_by_its_words() {
        let (mut store, root, text, _) = scene();
        focus_next(&mut store, root, true);
        focus_next(&mut store, root, true);
        let tree = store.derive_semantics(root);
        let host = tree.nodes.iter().position(|n| n.id == text).unwrap();
        assert!(!tree.nodes[host].focused, "the focus is on a link");
        let links: Vec<_> = tree.nodes[host]
            .children
            .iter()
            .map(|&i| &tree.nodes[i])
            .collect();
        assert_eq!(links.len(), 2);
        for (at, (row, name)) in links.iter().zip(["docs", "site"]).enumerate() {
            assert_eq!(row.id, text);
            assert_eq!(row.link, Some(at as u16));
            assert_eq!(row.role, Role::Link);
            assert_eq!(row.label.as_deref(), Some(name));
            assert_eq!(row.focused, at == 1);
        }
        assert_eq!(links[1].bounds, Rect::new(100.0, 0.0, 40.0, 20.0));
    }
}
