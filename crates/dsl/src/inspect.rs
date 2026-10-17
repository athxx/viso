//! What each node of a compiled view is in its source, by the index a
//! runtime's census names it with: the table
//! [`viso_view::inspect::resolve`] joins a census with.

use std::collections::HashSet;

use viso_view::inspect::{FileSources, NodeSource, StateSource};

use crate::hir::Ty;
use crate::hotreload::CandidatePlan;
use crate::ir::ui_ir::{UiItem, UiNode, UiTree};
use crate::syntax::{LineIndex, TextSize};
use crate::view_regions::{RegionKeys, StaticNodes, arms};

/// The nodes of `plan`, compiled from `text`: the view of `file`, as the
/// build records it, shown as `path`, in module `module`.
pub fn file_sources(
    plan: &CandidatePlan,
    text: &str,
    file: String,
    path: String,
    module: Vec<String>,
) -> FileSources {
    let tree = &plan.tree;
    let lines = LineIndex::new(text);
    let owner = plan.component.as_deref().unwrap_or_default();
    let keyed = keyed_nodes(tree);
    let mut rooted: HashSet<u32> = HashSet::new();
    let mut sources: Vec<NodeSource> = keyed
        .iter()
        .map(|node| source(tree, node, owner, &lines, text.len(), &mut rooted))
        .collect();
    for edge in &plan.bindings.edges {
        if let Some(node) = sources.get_mut(edge.node.0 as usize) {
            let bound = (edge.property.clone(), (edge.source.hi, edge.source.lo));
            if !node.bindings.contains(&bound) {
                node.bindings.push(bound);
            }
        }
    }
    // The view's root is the root of the component it mounts too.
    if let (Some(root), Some(component)) = (sources.first_mut(), &plan.component) {
        root.components
            .insert(0, component.trim_start_matches("r#").to_owned());
    }
    let statics = StaticNodes::of(tree);
    let mut by_index: Vec<Option<NodeSource>> = vec![None; statics.len()];
    for (key, source) in sources.iter().enumerate() {
        if let Some(index) = statics.ordinal(crate::ir::binding_ir::NodeKey(key as u32)) {
            by_index[index as usize] = Some(source.clone());
        }
    }
    let regional = RegionKeys::of(tree)
        .iter()
        .filter_map(|(at, key)| Some((at, sources.get(key.0 as usize)?.clone())))
        .collect();
    let at = |offset: usize| {
        let at = lines.line_col_utf16(TextSize::new(offset.min(text.len()) as u32));
        (at.line + 1, at.column + 1)
    };
    let mut states: Vec<StateSource> = plan
        .sources
        .iter()
        .enumerate()
        .map(|(n, id)| StateSource {
            key: (id.hi, id.lo),
            name: plan.source_names.get(n).cloned().unwrap_or_default(),
            type_name: plan
                .types
                .get(n)
                .filter(|ty| !matches!(ty, Ty::Unknown))
                .map(|ty| plan.schemas.describe(ty)),
            start: plan
                .declared
                .get(n)
                .map_or((0, 0), |range| at(range.start().to_usize())),
        })
        .collect();
    states.sort_by_key(|state| state.key);
    states.dedup_by_key(|state| state.key);
    FileSources {
        file,
        path,
        module,
        states,
        statics: by_index
            .into_iter()
            .map(Option::unwrap_or_default)
            .collect(),
        regional,
    }
}

/// Every node of `tree` by its key: in pre-order, each region's arms in
/// order.
fn keyed_nodes(tree: &UiTree) -> Vec<&UiNode> {
    fn walk<'a>(items: &'a [UiItem], out: &mut Vec<&'a UiNode>) {
        for item in items {
            match item {
                UiItem::Node(node) => {
                    out.push(node);
                    walk(&node.children, out);
                }
                _ => {
                    for items in arms(item) {
                        walk(items, out);
                    }
                }
            }
        }
    }
    let mut out = Vec::new();
    walk(&tree.items, &mut out);
    out
}

/// What `node` is in its source; the first node of an instance in key
/// order is the root of its component, and of each enclosing instance not
/// rooted yet, `rooted` holding those that are.
fn source(
    tree: &UiTree,
    node: &UiNode,
    owner: &str,
    lines: &LineIndex,
    len: usize,
    rooted: &mut HashSet<u32>,
) -> NodeSource {
    let mut components = Vec::new();
    let mut instance = node.instance;
    while let Some(placed) = tree.instance(instance) {
        if !rooted.insert(instance) {
            break;
        }
        components.push(placed.name.trim_start_matches("r#").to_owned());
        instance = placed.parent;
    }
    components.reverse();
    let owner = tree
        .instance(node.instance)
        .map_or(owner, |placed| placed.name.as_str());
    let start = node.origin.start().to_usize().min(len);
    let end = node.origin.end().to_usize().clamp(start, len);
    let at = |offset: usize| {
        let at = lines.line_col_utf16(TextSize::new(offset as u32));
        (at.line + 1, at.column + 1)
    };
    NodeSource {
        type_name: node.type_name.clone(),
        name: node.local_name.clone(),
        styles: node.styles.clone(),
        components,
        owner: owner.trim_start_matches("r#").to_owned(),
        bytes: (start as u32, end as u32),
        start: at(start),
        end: at(end),
        bindings: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::Origin;
    use crate::hir::TargetProfile;
    use crate::hotreload::plan_view_for;

    #[test]
    fn each_node_names_its_type_name_styles_component_and_declaration() {
        let text = "\
style Primary for Button {
    opacity: 0.9f32;
}

component Tag {
    input text: String = \"t\";
    view {
        Text { text: text; }
    }
}

export component Home {
    state items = [1, 2];
    state shown = true;
    view {
        Column {
            visible: shown;
            node save: Button { text: \"Save\"; styles: [Primary]; }
            Tag {}
            for item in items key item {
                node row: Text { text: \"row\"; }
            }
        }
    }
}
";
        let origin = Origin {
            package: "app".into(),
            module: vec!["home".into()],
            language: None,
        };
        let plan = plan_view_for(text, &origin, TargetProfile::default())
            .unwrap_or_else(|d| panic!("{d:?}"));
        let file = file_sources(
            &plan,
            text,
            "/p/src/home.vs".into(),
            "src/home.vs".into(),
            vec!["home".into()],
        );
        let types: Vec<&str> = file.statics.iter().map(|n| n.type_name.as_str()).collect();
        assert_eq!(types, ["Column", "Button", "Text"]);
        assert_eq!(file.statics[0].components, ["Home"]);
        let save = &file.statics[1];
        assert_eq!(save.name.as_deref(), Some("save"));
        assert_eq!(save.styles, ["Primary"]);
        assert_eq!(save.owner, "Home");
        assert_eq!(save.start, (18, 13));
        assert_eq!(&text[save.bytes.0 as usize..][..9], "node save");
        let tag = &file.statics[2];
        assert_eq!(tag.components, ["Tag"]);
        assert_eq!(tag.owner, "Tag");
        let (at, row) = &file.regional[0];
        assert_eq!(*at, (0, 0, 0));
        assert_eq!(row.name.as_deref(), Some("row"));
        let column = &file.statics[0];
        let [(property, key)] = &column.bindings[..] else {
            panic!("not one binding: {:?}", column.bindings);
        };
        assert_eq!(property, "visible");
        let shown = file.state(*key).expect("the state");
        assert_eq!(shown.name, "shown");
        assert_eq!(shown.type_name.as_deref(), Some("Bool"));
        assert_eq!(shown.start, (14, 5));
        assert!(save.bindings.is_empty());
        let items = file
            .states
            .iter()
            .find(|s| s.name == "items")
            .expect("items");
        assert_eq!(items.start, (13, 5));
    }
}
