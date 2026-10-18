//! A census joined with the sources of the views it names: every node of
//! every window, with what its source declares it as where a view mounts
//! it.

use super::census::{Census, CensusNode, NodeOrigin};
use super::source::{FileSources, NodeSource, SourceTable};

/// Every node of every window, window by window, each window's in
/// pre-order.
#[derive(Debug, Clone, Default)]
pub struct Inspection<'a> {
    pub nodes: Vec<Inspected<'a>>,
}

/// One node.
#[derive(Debug, Clone, Copy)]
pub struct Inspected<'a> {
    /// Its window, in the order the app opened them.
    pub window: u32,
    /// Its parent, by index into [`Inspection::nodes`].
    pub parent: Option<usize>,
    pub node: &'a CensusNode,
    /// The view and the declaration it comes from, `None` for a node no
    /// view of the table mounts.
    pub source: Option<(&'a FileSources, &'a NodeSource)>,
}

impl Inspected<'_> {
    /// Its identity as a tool names it: `<index>:<generation>`.
    pub fn id(&self) -> String {
        format!("{}:{}", self.node.index, self.node.generation)
    }

    /// The source path of the component whose view declares it, then its
    /// name, or its type when it has none: `home::Home::save`.
    pub fn symbol(&self) -> Option<String> {
        let (file, node) = self.source?;
        let mut path: Vec<&str> = file.module.iter().map(String::as_str).collect();
        path.push(&node.owner);
        path.push(node.name.as_deref().unwrap_or(&node.type_name));
        Some(path.join("::"))
    }
}

/// The nodes of `census`, joined with what `table` says of the views it
/// names.
pub fn resolve<'a>(census: &'a Census, table: &'a SourceTable) -> Inspection<'a> {
    let mut nodes = Vec::with_capacity(census.windows.iter().map(|w| w.nodes.len()).sum());
    for (window, taken) in census.windows.iter().enumerate() {
        let files: Vec<Option<&FileSources>> = taken
            .mounts
            .iter()
            .map(|mount| table.file(&mount.file))
            .collect();
        let base = nodes.len();
        for node in &taken.nodes {
            let source = node.origin.and_then(|origin| match origin {
                NodeOrigin::Static { mount, index } => {
                    let file = files[mount as usize]?;
                    Some((file, file.static_node(index)?))
                }
                NodeOrigin::Region {
                    mount,
                    region,
                    arm,
                    item,
                } => {
                    let file = files[mount as usize]?;
                    Some((file, file.region_node(region, arm, item)?))
                }
            });
            nodes.push(Inspected {
                window: window as u32,
                parent: node.parent.map(|parent| base + parent as usize),
                node,
                source,
            });
        }
    }
    Inspection { nodes }
}
