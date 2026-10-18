//! The runtime's half of an inspection: each window's tree as it stands,
//! with where each node a view mounts comes from in that view.

use std::collections::HashMap;

use viso_dev_protocol::inspect::{
    CensusMount, CensusNode, NodeOrigin, NodeSemantics, WindowCensus,
};
use viso_ui::{NodeId, NodeStore};

use crate::ViewHost;

/// A view mounted in the window the census is taken of.
pub struct CensusView<'a> {
    pub file: &'a str,
    pub source_hash: u64,
    /// The live node of each static node, by static index.
    pub nodes: &'a [Option<NodeId>],
    /// Its behavior host, which knows the nodes its regions mount.
    pub host: Option<&'a ViewHost>,
}

/// The census of the tree under `root` in `store`, whose nodes `views`
/// mount.
pub fn census(store: &NodeStore, root: Option<NodeId>, views: &[CensusView<'_>]) -> WindowCensus {
    let mut out = WindowCensus {
        mounts: views
            .iter()
            .map(|view| CensusMount {
                file: view.file.to_owned(),
                source_hash: view.source_hash,
            })
            .collect(),
        nodes: Vec::new(),
    };
    let Some(root) = root else {
        return out;
    };
    let mut origins: HashMap<NodeId, NodeOrigin> = HashMap::new();
    for (mount, view) in views.iter().enumerate() {
        let mount = mount as u32;
        for (index, node) in view.nodes.iter().enumerate() {
            if let Some(node) = node {
                origins.entry(*node).or_insert(NodeOrigin::Static {
                    mount,
                    index: index as u32,
                });
            }
        }
        for node in view
            .host
            .map(|host| host.region_nodes(store))
            .unwrap_or_default()
        {
            origins.entry(node.node).or_insert(NodeOrigin::Region {
                mount,
                region: node.region,
                arm: node.arm,
                item: node.item,
            });
        }
    }
    let semantics: HashMap<NodeId, NodeSemantics> = store
        .derive_semantics(root)
        .nodes
        .into_iter()
        .map(|node| {
            (
                node.id,
                NodeSemantics {
                    role: node.role,
                    label: node.label,
                    hint: node.hint,
                    value: node.value,
                    heading_level: node.heading_level,
                    state: node.state.unwrap_or_default(),
                },
            )
        })
        .collect();
    let tree = store.inspect_tree(root);
    let mut at: HashMap<NodeId, u32> = HashMap::with_capacity(tree.nodes.len());
    out.nodes.reserve(tree.nodes.len());
    for node in &tree.nodes {
        let parent = node.parent.and_then(|parent| at.get(&parent).copied());
        let shown =
            !node.flags.hidden && parent.is_none_or(|parent| out.nodes[parent as usize].shown);
        at.insert(node.id, out.nodes.len() as u32);
        out.nodes.push(CensusNode {
            index: node.id.index(),
            generation: node.id.generation(),
            parent,
            kind: node.kind,
            bounds: node.world,
            shown,
            focusable: node.flags.focusable,
            focused: node.flags.focused,
            semantics: semantics.get(&node.id).cloned(),
            origin: origins.get(&node.id).copied(),
        });
    }
    out
}

/// The static nodes of a view without regions mounted at `root`, by static
/// index: the live tree walked in pre-order with `shape`, the number of
/// static children of each static node in pre-order. A view's static
/// children are exactly a container's live children; a node that authors no
/// children (a leaf, a `VirtualList` mounting its own items) has none.
pub fn static_nodes(store: &NodeStore, root: NodeId, shape: &[u32]) -> Vec<Option<NodeId>> {
    let mut out = vec![None; shape.len()];
    let mut cursor = 0;
    while cursor < shape.len() {
        let live = (cursor == 0).then_some(root);
        walk_static(store, shape, live, &mut cursor, &mut out);
    }
    out
}

fn walk_static(
    store: &NodeStore,
    shape: &[u32],
    live: Option<NodeId>,
    cursor: &mut usize,
    out: &mut [Option<NodeId>],
) {
    let index = *cursor;
    *cursor += 1;
    out[index] = live;
    let arena = store.arena();
    let mut child = live.and_then(|id| arena.first_child(id));
    for _ in 0..shape[index] {
        if *cursor >= shape.len() {
            return;
        }
        let at = child;
        child = at.and_then(|id| arena.next_sibling(id));
        walk_static(store, shape, at, cursor, out);
    }
}
