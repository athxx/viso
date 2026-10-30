//! Reactive property values reaching their nodes: a label's text, a text
//! field's seeded buffer and a control's displayed value, evaluated from the
//! view's handler table at mount and re-evaluated when a state they read
//! changes.

use std::rc::Rc;

use std::cell::RefCell;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::plan_view;
use viso_dsl::ir::binding_ir::NodeKey;

use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, BuildCx, EffectStore, FieldSeed, LeafStyle, LiveRegion, NodeId, NodeStore, Role,
    SemanticState, StateStore, StateValue, StructureCx, run_structure_hooks,
};
use viso_view::{ControlKind, Scope, Value, ViewHost, mount_values};

const SOURCE: &str = r#"
export component Values {
    state count = 2;
    state name = "Ada";
    view {
        Column {
            Text { text: "{count} items"; }
            Button { text: "Go"; }
            TextInput { bind value <=> name; }
            Text { }
            Text { text: "{count / 10}"; }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["values".into()],
        language: None,
        ..Default::default()
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
    assert_eq!(text(2, ControlKind::Button), "Go");
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
        lists: &mut VirtualLists::new(),
    };
    mount_values(&mut structure, &host, &nodes);

    let mut texts = declared(&mut store);
    texts.sort_by_key(|(id, _)| id.index());
    let expected = [(ids[1], "2 items"), (ids[2], "Go"), (ids[5], "0")];
    let expected: Vec<(NodeId, String)> =
        expected.iter().map(|&(id, t)| (id, t.to_owned())).collect();
    assert_eq!(texts, expected);
    let mut seeds: Vec<(NodeId, FieldSeed)> = Vec::new();
    store.take_text_seeds(&mut seeds);
    assert_eq!(seeds.len(), 1);
    assert_eq!(
        (seeds[0].0, seeds[0].1.request.text.as_str()),
        (ids[3], "Ada")
    );
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
        &mut VirtualLists::new(),
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
        &mut VirtualLists::new(),
        &changed,
    );
    assert_eq!(ran, 0);
    assert!(declared(&mut store).is_empty());
}

const CONTROLS: &str = r#"
export component Shows {
    state on = true;
    state level: F32 = 2.0;
    state top: F32 = 10.0;
    state tab: U32 = 1;
    view {
        Column {
            Toggle { bind checked <=> on; }
            Slider { min: 0.0; max: top; bind value <=> level; }
            Tabs {
                bind selected <=> tab;
                Text { }
                Text { }
            }
        }
    }
}
"#;

#[test]
fn controls_project_their_value_and_range_into_semantic_state() {
    let plan = plan_view(CONTROLS, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let host = Rc::new(RefCell::new(host));
    let mut store = NodeStore::new();
    let mut states = StateStore::default();
    let mut bindings = BindingTable::default();
    let mut effects = EffectStore::default();
    let cells = [
        ("on", StateValue::Bool(true)),
        ("level", StateValue::Float(2.0)),
        ("top", StateValue::Float(10.0)),
        ("tab", StateValue::Int(1)),
    ]
    .map(|(name, initial)| {
        let id = states.alloc(initial);
        let mut host = host.borrow_mut();
        let slot = host.state_slot(name).expect("a state");
        assert!(host.mirror(slot, id));
        id
    });
    let mut cx = BuildCx::new(&mut store);
    let (mut toggle, mut slider, mut tabs, mut first, mut second) = Default::default();
    cx.flex(Default::default(), |cx| {
        toggle = Some(cx.leaf(LeafStyle::default()).id());
        slider = Some(cx.leaf(LeafStyle::default()).id());
        tabs = Some(
            cx.flex(Default::default(), |cx| {
                first = Some(cx.leaf(LeafStyle::default()).id());
                second = Some(cx.leaf(LeafStyle::default()).id());
            })
            .id(),
        );
    });
    cx.root();
    let [toggle, slider, tabs, first, second] =
        [toggle, slider, tabs, first, second].map(|id: Option<NodeId>| id.expect("built"));
    let nodes: Vec<(NodeId, viso_view::Control)> = [(toggle, 1), (slider, 2), (tabs, 3)]
        .map(|(id, key)| (id, view.control(NodeKey(key)).expect("a control")))
        .into();
    let mut structure = StructureCx {
        store: &mut store,
        states: &mut states,
        bindings: &mut bindings,
        effects: &mut effects,
        lists: &mut VirtualLists::new(),
    };
    mount_values(&mut structure, &host, &nodes);

    let state = |store: &NodeStore, id| store.semantic_state(id).expect("a semantic state");
    assert_eq!(state(&store, toggle), SemanticState::checked(true));
    let slid = state(&store, slider);
    assert_eq!((slid.value, slid.range), (Some(2.0), Some((0.0, 10.0))));
    assert_eq!(state(&store, first).selected, Some(false));
    assert_eq!(state(&store, second).selected, Some(true));

    // A write re-projects the nodes reading the written state.
    let [on, _, top, _] = cells;
    states.set(top, StateValue::Float(20.0));
    states.set(on, StateValue::Bool(false));
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    run_structure_hooks(
        &mut store,
        &mut states,
        &mut bindings,
        &mut effects,
        &mut VirtualLists::new(),
        &changed,
    );
    assert_eq!(state(&store, toggle), SemanticState::checked(false));
    assert_eq!(state(&store, slider).range, Some((0.0, 20.0)));
    assert_eq!(state(&store, second).selected, Some(true));
}

const NAMED: &str = r#"
export component Named {
    state count = 1;
    view {
        Column {
            semantics.role: Role::region;
            semantics.label: Option::Some("Totals");
            Text { text: "{count}"; }
            Button { text: "Add"; semantics.label: Option::Some("Add to {count}"); }
            Image { opacity: 1.0; }
        }
    }
}
"#;

#[test]
fn nodes_take_their_role_and_the_name_they_show() {
    let plan = plan_view(NAMED, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let host = Rc::new(RefCell::new(host));
    let mut store = NodeStore::new();
    let mut states = StateStore::default();
    let mut bindings = BindingTable::default();
    let mut effects = EffectStore::default();
    let count = states.alloc(StateValue::Int(1));
    {
        let mut host = host.borrow_mut();
        let slot = host.state_slot("count").expect("a state");
        assert!(host.mirror(slot, count));
    }
    let mut cx = BuildCx::new(&mut store);
    let ids: Vec<NodeId> = (0..4).map(|_| cx.leaf(LeafStyle::default()).id()).collect();
    cx.root();
    let nodes: Vec<(NodeId, viso_view::Control)> = (0..4)
        .filter_map(|key| Some((ids[key], view.control(NodeKey(key as u32))?)))
        .collect();
    let mut structure = StructureCx {
        store: &mut store,
        states: &mut states,
        bindings: &mut bindings,
        effects: &mut effects,
        lists: &mut VirtualLists::new(),
    };
    mount_values(&mut structure, &host, &nodes);

    let named = |store: &NodeStore, id| {
        store
            .semantics(id)
            .map(|s| (s.role, s.label.clone().unwrap_or_default()))
    };
    assert_eq!(named(&store, ids[0]), Some((Role::Region, "Totals".into())));
    assert_eq!(named(&store, ids[1]), Some((Role::Label, "1".into())));
    assert_eq!(
        named(&store, ids[2]),
        Some((Role::Button, "Add to 1".into()))
    );
    assert_eq!(
        named(&store, ids[3]),
        Some((Role::Group, String::new())),
        "an image without a label is decoration"
    );

    states.set(count, StateValue::Int(4));
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    run_structure_hooks(
        &mut store,
        &mut states,
        &mut bindings,
        &mut effects,
        &mut VirtualLists::new(),
        &changed,
    );
    assert_eq!(named(&store, ids[1]), Some((Role::Label, "4".into())));
    assert_eq!(
        named(&store, ids[2]),
        Some((Role::Button, "Add to 4".into()))
    );
}

const SAID: &str = r#"
export component Said {
    state step = 1;
    state busy = false;
    view {
        Column {
            semantics.role: Role::heading;
            semantics.heading_level: 2;
            semantics.label: "Settings";
            semantics.merge: true;
            Text {
                text: "x";
                semantics.hint: "Opens the details";
                semantics.value: "Step {step}";
                semantics.live: LiveRegion::polite;
            }
            Text { text: "y"; semantics.hidden: busy; }
            Tabs { selected: 0; Text { text: "A"; } Text { text: "B"; } }
            RadioGroup { selected: 0; if busy { Text { text: "r1"; } } else { Text { text: "r2"; } } }
        }
    }
}
"#;

#[test]
fn nodes_say_their_hint_value_live_setting_level_and_hiding() {
    let plan = plan_view(SAID, &origin()).expect("the view compiles");
    let view = plan.view.expect("the view has behavior");
    let host = ViewHost::new(Rc::clone(&view.module), &view.component).expect("mounts");
    let host = Rc::new(RefCell::new(host));
    let mut store = NodeStore::new();
    let mut states = StateStore::default();
    let mut bindings = BindingTable::default();
    let mut effects = EffectStore::default();
    let step = states.alloc(StateValue::Int(1));
    let busy = states.alloc(StateValue::Bool(false));
    {
        let mut host = host.borrow_mut();
        let slot = host.state_slot("step").expect("a state");
        assert!(host.mirror(slot, step));
        let slot = host.state_slot("busy").expect("a state");
        assert!(host.mirror(slot, busy));
    }
    // Column, the two texts, the tabs and their pages, the radio group and
    // the items of its two arms.
    const NODES: usize = 9;
    let mut cx = BuildCx::new(&mut store);
    let ids: Vec<NodeId> = (0..NODES)
        .map(|_| cx.leaf(LeafStyle::default()).id())
        .collect();
    cx.root();
    let nodes: Vec<(NodeId, viso_view::Control)> = (0..NODES)
        .filter_map(|key| Some((ids[key], view.control(NodeKey(key as u32))?)))
        .collect();
    let mut structure = StructureCx {
        store: &mut store,
        states: &mut states,
        bindings: &mut bindings,
        effects: &mut effects,
        lists: &mut VirtualLists::new(),
    };
    mount_values(&mut structure, &host, &nodes);

    let heading = store.semantics(ids[0]).expect("the heading says");
    assert_eq!(heading.role, Role::Heading);
    assert_eq!(heading.heading_level, Some(2));
    assert_eq!(heading.label.as_deref(), Some("Settings"));
    assert!(heading.merged, "it reads as one element");
    let detail = store.semantics(ids[1]).expect("the text says");
    assert_eq!(detail.role, Role::Label);
    assert_eq!(detail.hint.as_deref(), Some("Opens the details"));
    assert_eq!(detail.value.as_deref(), Some("Step 1"));
    assert_eq!(detail.live, LiveRegion::Polite);
    assert!(!store.semantics(ids[2]).expect("says").hidden);
    assert!(!store.semantics(ids[2]).expect("says").merged);
    let role = |store: &NodeStore, id| store.semantics(id).map(|s| s.role);
    assert_eq!(role(&store, ids[3]), Some(Role::TabList));
    assert_eq!(role(&store, ids[4]), Some(Role::Tab), "a page is a tab");
    assert_eq!(role(&store, ids[5]), Some(Role::Tab));
    assert_eq!(role(&store, ids[6]), None, "a radio group is a group");
    assert_eq!(
        role(&store, ids[7]),
        Some(Role::Radio),
        "an item is a radio"
    );
    assert_eq!(role(&store, ids[8]), Some(Role::Radio), "in every arm");

    states.set(step, StateValue::Int(2));
    states.set(busy, StateValue::Bool(true));
    let mut changed = Vec::new();
    states.take_pending(&mut changed);
    run_structure_hooks(
        &mut store,
        &mut states,
        &mut bindings,
        &mut effects,
        &mut VirtualLists::new(),
        &changed,
    );
    let detail = store.semantics(ids[1]).expect("the text says");
    assert_eq!(detail.value.as_deref(), Some("Step 2"));
    assert!(store.semantics(ids[2]).expect("says").hidden);
}

#[test]
fn merging_is_a_constant_flag() {
    for (merge, says) in [
        ("true", ""),
        ("false", ""),
        ("1 > 0", "`semantics.merge` is a constant `true` or `false`"),
    ] {
        let source = format!(
            "export component C {{\n    view {{ Row {{ semantics.merge: {merge}; Text {{ text: \"x\"; }} }} }}\n}}\n"
        );
        match plan_view(&source, &origin()) {
            Ok(_) => assert!(says.is_empty(), "{merge} is rejected"),
            Err(errors) => {
                let all = format!("{errors:?}");
                assert!(!says.is_empty() && all.contains(says), "{merge}: {all}");
            }
        }
    }
}

#[test]
fn a_role_is_a_constant_variant_of_role() {
    for (role, says) in [
        ("Role::heading_one", "`Role` has no variant `heading_one`"),
        ("Role::button", ""),
        ("Justify::start", "a constant `Role::…`"),
    ] {
        let source = format!(
            "export component C {{\n    view {{ Column {{ semantics.role: {role}; Text {{ text: \"x\"; }} }} }}\n}}\n"
        );
        let planned = plan_view(&source, &origin());
        match planned {
            Ok(_) => assert!(says.is_empty(), "{role} is rejected"),
            Err(errors) => {
                let all = format!("{errors:?}");
                assert!(!says.is_empty() && all.contains(says), "{role}: {all}");
            }
        }
    }
}
