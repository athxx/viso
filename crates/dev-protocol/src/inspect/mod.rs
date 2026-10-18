//! What a running app shows, node by node, for a tool to inspect and select
//! nodes from.
//!
//! An inspection has two halves. The runtime takes a [`Census`] of each
//! window: every node in pre-order with its parent, what it draws, its box,
//! whether it is shown, focus and its semantics, and — for a node a `.vs`
//! view mounts — where in that view it comes from: its static index, or the
//! region, arm and item that mounted it. What that is in the source, its
//! type, name, styles and declaration, is the tool's to know: it compiles
//! the views into a [`SourceTable`]. [`resolve`] joins the two into an
//! [`Inspection`], which a [`Selector`] selects nodes from.
//!
//! A dev runtime answers `viso run` with its census and a headless run writes
//! one; `viso inspect query`, UI scenarios and Studio select by the same
//! selectors over the same join. Neither half is in a release build's frame
//! path: a census is taken only when a tool asks.

mod census;
mod detail;
mod events;
mod selector;
mod source;
mod tree;

pub use census::{Census, CensusMount, CensusNode, NodeOrigin, NodeSemantics, WindowCensus};
pub use detail::{
    BatchDetail, ChainLink, FrameDamage, FrameDetail, InspectAsk, InspectorCaps, MAX_ASKED_NODES,
    NodeBinding, NodeDetail, NodeDetails, PaintSpan, PropertyGroup, ResourceDetail, RouteStop,
    WindowDetail, dirty_names, spell_value, state_names,
};
pub use events::{Cause, EventKind, EventLog, InspectEvent};
pub use selector::{Attribute, Selector, SelectorError};
pub use source::{FileSources, NodeSource, SourceTable, StateSource};
pub use tree::{Inspected, Inspection, resolve};
// The types a census is made of.
pub use viso_math::Rect;
pub use viso_schema::{InspectKind, Role, SemanticState};

#[cfg(test)]
mod tests {
    use viso_ende::{Decode, Encode};

    use super::*;

    fn node(
        parent: Option<u32>,
        origin: Option<NodeOrigin>,
        role: Option<(Role, &str)>,
    ) -> CensusNode {
        CensusNode {
            index: 7,
            generation: 1,
            parent,
            kind: InspectKind::Container,
            bounds: Rect {
                x: 1.0,
                y: 2.0,
                w: 30.0,
                h: 40.0,
            },
            shown: true,
            focusable: role.is_some(),
            focused: false,
            semantics: role.map(|(role, label)| NodeSemantics {
                role,
                label: Some(label.into()),
                hint: None,
                value: None,
                heading_level: None,
                state: SemanticState::checked(true),
            }),
            origin,
        }
    }

    fn source(
        type_name: &str,
        name: Option<&str>,
        styles: &[&str],
        components: &[&str],
    ) -> NodeSource {
        NodeSource {
            type_name: type_name.into(),
            name: name.map(Into::into),
            styles: styles.iter().map(|s| (*s).into()).collect(),
            components: components.iter().map(|s| (*s).into()).collect(),
            owner: "Home".into(),
            bytes: (20, 52),
            start: (3, 5),
            end: (6, 6),
            bindings: Vec::new(),
        }
    }

    /// A window: a Column (a Toolbar component's root) holding a named
    /// primary Button and a Text, then a region's list item, then a node no
    /// view mounts.
    fn sample() -> (Census, SourceTable) {
        let at = |index| Some(NodeOrigin::Static { mount: 0, index });
        let census = Census {
            windows: vec![WindowCensus {
                mounts: vec![CensusMount {
                    file: "/p/src/home.vs".into(),
                    source_hash: 9,
                }],
                nodes: vec![
                    node(None, at(0), None),
                    node(Some(0), at(1), Some((Role::Button, "Save"))),
                    node(Some(0), at(2), Some((Role::Label, "Hello \"you\""))),
                    node(
                        Some(0),
                        Some(NodeOrigin::Region {
                            mount: 0,
                            region: 0,
                            arm: 0,
                            item: 0,
                        }),
                        Some((Role::ListItem, "Row")),
                    ),
                    node(Some(3), None, None),
                ],
            }],
        };
        let table = SourceTable {
            files: vec![FileSources {
                file: "/p/src/home.vs".into(),
                path: "src/home.vs".into(),
                module: vec!["home".into()],
                statics: vec![
                    source("Column", None, &[], &["Toolbar"]),
                    source("Button", Some("save"), &["Primary", "Dense"], &[]),
                    source("Text", None, &[], &[]),
                ],
                regional: vec![((0, 0, 0), source("Row", Some("row"), &[], &[]))],
                states: Vec::new(),
            }],
        };
        (census, table)
    }

    fn select(selector: &str) -> Vec<usize> {
        let (census, table) = sample();
        let inspection = resolve(&census, &table);
        Selector::parse(selector)
            .unwrap_or_else(|e| panic!("{selector}: {e}"))
            .select(&inspection)
    }

    #[test]
    fn selectors_match_by_type_name_style_attribute_and_ancestry() {
        assert_eq!(select("Button"), [1]);
        assert_eq!(select("#save"), [1]);
        assert_eq!(select(".Primary.Dense"), [1]);
        assert_eq!(select("Button.Missing"), Vec::<usize>::new());
        assert_eq!(select("Toolbar"), [0], "a component is its root node");
        assert_eq!(select("home::Toolbar"), [0]);
        assert_eq!(select("other::Toolbar"), Vec::<usize>::new());
        assert_eq!(select("[role=button]"), [1]);
        assert_eq!(select(r#"[label="Hello \"you\""]"#), [2]);
        assert_eq!(select("[checked]"), [1, 2, 3]);
        assert_eq!(select("[label]"), [1, 2, 3]);
        assert_eq!(select("Column > Text"), [2]);
        assert_eq!(select("Toolbar *"), [1, 2, 3, 4]);
        assert_eq!(select("Column > *"), [1, 2, 3]);
        assert_eq!(select("#row"), [3], "a region's node resolves by its item");
        assert_eq!(select("Column Row > [kind=container]"), [4]);
        assert_eq!(select("Text, #save"), [1, 2]);
    }

    #[test]
    fn a_symbol_is_the_declaring_component_and_the_node() {
        let (census, table) = sample();
        let inspection = resolve(&census, &table);
        assert_eq!(
            inspection.nodes[1].symbol().as_deref(),
            Some("home::Home::save")
        );
        assert_eq!(
            inspection.nodes[2].symbol().as_deref(),
            Some("home::Home::Text")
        );
        assert_eq!(inspection.nodes[4].symbol(), None);
        assert_eq!(inspection.nodes[1].id(), "7:1");
    }

    #[test]
    fn a_bad_selector_says_where() {
        let error = |text: &str| Selector::parse(text).unwrap_err();
        assert_eq!(error("").at, 0);
        assert_eq!(error("Button >").at, 8);
        assert!(
            error("[colour=red]")
                .message
                .contains("no attribute `colour`")
        );
        assert!(error("[label=\"open").message.contains("unterminated"));
        assert!(error("#").message.contains("node name"));
        assert!(error("Button,").message.starts_with("expected a type"));
        assert!(error("Button!").message.contains("`!`"));
    }

    #[test]
    fn a_selector_prints_as_it_parses() {
        for text in [
            "Button#save.Primary[role=\"button\"]",
            "home::Toolbar > * Text, [checked]",
            "[label=\"a \\\"b\\\"\"]",
        ] {
            let parsed = Selector::parse(text).unwrap();
            assert_eq!(
                Selector::parse(&parsed.to_string()).unwrap(),
                parsed,
                "{text}"
            );
        }
    }

    #[test]
    fn a_census_and_a_table_round_trip_and_mutations_never_panic() {
        let (census, table) = sample();
        let bytes = census.encode_to_vec();
        assert_eq!(Census::decode_from_slice(&bytes), Ok(census));
        let sources = table.encode_to_vec();
        assert_eq!(SourceTable::decode_from_slice(&sources), Ok(table));
        let mut rng: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = |n: usize| {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            (rng % n.max(1) as u64) as usize
        };
        for round in 0..20_000 {
            let mut mutated = if round % 2 == 0 {
                bytes.clone()
            } else {
                sources.clone()
            };
            let at = next(mutated.len());
            match next(3) {
                0 => mutated[at] ^= 1 << next(8),
                1 => mutated[at] = 0xFF,
                _ => mutated.truncate(at),
            }
            let _ = Census::decode_from_slice(&mutated);
            let _ = SourceTable::decode_from_slice(&mutated);
        }
    }
}
