//! Reactive property values reaching their nodes: a label's text, a text
//! field's seeded buffer and a control's displayed value, evaluated from the
//! view's handler table at mount and re-evaluated when a state they read
//! changes.

use std::rc::Rc;

use std::cell::RefCell;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::plan_view;
use viso_dsl::ir::binding_ir::NodeKey;

use viso_ui::{
    BindingTable, BuildCx, EffectStore, LeafStyle, NodeId, NodeStore, StateStore, StateValue,
    StructureCx, TextRequest, run_structure_hooks,
};
use viso_view::{ControlKind, Scope, Value, ViewHost, mount_values};

const SOURCE: &str = r#"
export component Values {
    state count = 2;
    state name = "Ada";
    view {
        Column {
            Text { text: format("{} items", count); }
            Button { text: "Go"; }
            TextInput { bind value <=> name; }
            Text { }
            Text { text: format("{}", count / 10); }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["values".into()],
        language: None,
    }
}

#[test]
fn a_label_text_and_a_field_value_compile_to_value_entries() {
    let plan = plan_view(SOURCE, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let mut host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let states = StateStore::default();
    let mut text = |key: u32, kind: ControlKind| {
        let control = view.control(NodeKey(key)).expect("a view-driven node");
        assert_eq!(control.kind, kind);
        let entry = control.value.expect("a value entry");
        match host.evaluate(entry, &Scope::EMPTY, None, &states) {
            Ok(Value::Str(text)) => text.as_str().to_owned(),
            other => panic!("the entry evaluates to a text, not {other:?}"),
        }
    };
    assert_eq!(text(1, ControlKind::Label), "2 items");
    assert_eq!(text(2, ControlKind::Label), "Go");
    assert_eq!(text(3, ControlKind::TextInput), "Ada");
    assert_eq!(view.control(NodeKey(4)), None, "a label without a text");
}

/// The texts declared on the nodes of `store` since the last call, by node.
fn declared(store: &mut NodeStore) -> Vec<(NodeId, String)> {
    let mut requests = Vec::new();
    store.take_text_requests(&mut requests);
    requests.into_iter().map(|(id, r)| (id, r.text)).collect()
}

#[test]
fn values_reach_their_nodes_at_mount_and_again_only_when_changed() {
    let plan = plan_view(SOURCE, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let host = Rc::new(RefCell::new(host));
    let mut store = NodeStore::new();
    let mut states = StateStore::default();
    let mut bindings = BindingTable::default();
    let mut effects = EffectStore::default();
    let count = states.alloc(StateValue::Int(2));
    let name = states.alloc(StateValue::Int(0));
    {
        let mut host = host.borrow_mut();
        let (count_slot, name_slot) = (host.state_slot("count"), host.state_slot("name"));
        assert!(host.mirror(count_slot.unwrap(), count));
        assert!(host.track(name_slot.unwrap(), name));
    }
    let mut cx = BuildCx::new(&mut store);
    let ids: Vec<NodeId> = (0..6).map(|_| cx.leaf(LeafStyle::default()).id()).collect();
    cx.root();
    let nodes: Vec<(NodeId, viso_view::Control)> = (1..6)
        .filter_map(|key| Some((ids[key], view.control(NodeKey(key as u32))?)))
        .collect();
    let mut structure = StructureCx {
        store: &mut store,
        states: &mut states,
        bindings: &mut bindings,
        effects: &mut effects,
    };
    mount_values(&mut structure, &host, &nodes);

    let mut texts = declared(&mut store);
    texts.sort_by_key(|(id, _)| id.index());
    let expected = [(ids[1], "2 items"), (ids[2], "Go"), (ids[5], "0")];
    let expected: Vec<(NodeId, String)> =
        expected.iter().map(|&(id, t)| (id, t.to_owned())).collect();
    assert_eq!(texts, expected);
    let mut seeds: Vec<(NodeId, TextRequest)> = Vec::new();
    store.take_text_seeds(&mut seeds);
    assert_eq!(seeds.len(), 1);
    assert_eq!((seeds[0].0, seeds[0].1.text.as_str()), (ids[3], "Ada"));
    assert_eq!(store.structure_hook_count(), 1, "one hook for the view");

    // `count` 2 -> 3 re-evaluates both texts reading it; `count / 10` is still
    // `0`, so only the first is declared again. Nothing reading `name` runs.
    states.set(count, StateValue::Int(3));
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    let ran = run_structure_hooks(
        &mut store,
        &mut states,
        &mut bindings,
        &mut effects,
        &changed,
    );
    assert_eq!(ran, 1);
    assert_eq!(declared(&mut store), vec![(ids[1], "3 items".to_owned())]);
    assert!(!store.has_text_seeds(), "the field's value did not change");

    // A frame changing no cell they read runs nothing.
    let other = states.alloc(StateValue::Int(0));
    states.set(other, StateValue::Int(1));
    changed.clear();
    states.take_pending(&mut changed);
    let ran = run_structure_hooks(
        &mut store,
        &mut states,
        &mut bindings,
        &mut effects,
        &changed,
    );
    assert_eq!(ran, 0);
    assert!(declared(&mut store).is_empty());
}
