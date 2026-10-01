//! A state whose type an edit changes: its live value converts into the new
//! type when the new type holds it — a number into a float, a record into one
//! with a defaulted field, an enum into one with its variants reordered — and is
//! otherwise carried through the `@migrate` function from its old type, or
//! reset to the new initializer with an `E5101` notice.

use std::cell::RefCell;
use std::rc::Rc;

use viso_behavior::Value;
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, HotReloadReport, LiveRuntime, hot_reload_view};
use viso_dsl::ir::binding_ir::NodeKey;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, SemanticProjector, StateStore, StateValue, TextEdits,
};
use viso_view::ViewHost;

/// A component declaring `states` after the declarations `types`, whose leaf
/// handles `body`.
fn view(types: &str, states: &str, body: &str) -> String {
    format!(
        "{types}
        component Clicker {{
            {states}
            view {{
                Column {{
                    width: 200dp;
                    height: 100dp;
                    Text {{ width: 100dp; height: 50dp; {body} }}
                }}
            }}
        }}"
    )
}

/// The live runtime a reload commits into.
#[derive(Default)]
struct Live {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<(NodeKey, NodeId)>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

impl Live {
    fn reload(&mut self, source: &str) -> HotReloadReport {
        let mut rt = LiveRuntime {
            store: &mut self.store,
            states: &mut self.states,
            bindings: &mut self.bindings,
            effects: &mut self.effects,
            lists: &mut self.lists,
            text_edits: &mut self.text_edits,
            projectors: &mut self.projectors,
            root: self.root,
            scratch: &mut self.scratch,
            nodes: &mut self.nodes,
            view: &mut self.view,
        };
        let origin = Origin {
            package: "app".into(),
            module: vec!["clicker".into()],
            language: None,
        };
        let done =
            hot_reload_view(&mut rt, &self.last_good, source, &origin).expect("the edit compiles");
        self.root = rt.root;
        self.last_good = done.candidate;
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
        }
        done.report
    }

    /// A primary click inside the leaf, then the state flush.
    fn click(&mut self) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: 20.0,
                y: 20.0,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                root,
                event,
                &mut chain,
            );
        }
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
    }

    /// The UI cell of the state `name`.
    fn cell(&self, name: &str) -> Option<StateValue> {
        let symbol = self.last_good.symbol_for_name(name)?;
        let key = viso_ui::state::StateKey::from_parts(symbol.hi, symbol.lo);
        self.states.get(self.states.id_for_key(key)?)
    }

    /// The live VM value of the state `name`.
    fn value(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        host.current(host.state_slot(name)?, &self.states)
    }
}

/// The fields of an aggregate value.
fn fields(value: &Value) -> &[Value] {
    match value {
        Value::Agg(agg) => &agg.fields,
        other => panic!("not an aggregate: {other:?}"),
    }
}

#[test]
fn an_integer_state_retyped_to_a_float_keeps_its_value() {
    let mut live = Live::default();
    live.reload(&view("", "state count = 0;", "on click { count += 1; }"));
    live.click();
    live.click();
    assert_eq!(live.cell("count"), Some(StateValue::Int(2)));

    let report = live.reload(&view(
        "",
        "state count: F64 = 0.0;",
        "on click { count += 0.5; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(live.cell("count"), Some(StateValue::Float(2.0)));
    live.click();
    assert_eq!(live.cell("count"), Some(StateValue::Float(2.5)));
}

#[test]
fn a_value_the_new_type_does_not_hold_is_reset_with_a_notice() {
    let mut live = Live::default();
    live.reload(&view("", "state count = 0;", "on click { count += 300; }"));
    live.click();
    assert_eq!(live.cell("count"), Some(StateValue::Int(300)));

    let report = live.reload(&view(
        "",
        "state count: I8 = 1;",
        "on click { count += 1; }",
    ));
    assert_eq!(live.cell("count"), Some(StateValue::Int(1)), "reset");
    let [notice] = &report.notices[..] else {
        panic!("one notice: {:?}", report.notices);
    };
    assert_eq!(notice.code, "E5101");
    assert!(
        notice.message.contains("`count`")
            && notice.message.contains("`I64`")
            && notice.message.contains("`I8`"),
        "{}",
        notice.message
    );
    assert_eq!(report.reset, 1);

    // A value the narrower type holds converts.
    live.click();
    let report = live.reload(&view(
        "",
        "state count: I16 = 0;",
        "on click { count += 1; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(live.cell("count"), Some(StateValue::Int(2)));
}

#[test]
fn an_unrelated_type_resets_and_other_states_are_kept() {
    let mut live = Live::default();
    live.reload(&view(
        "",
        "state count = 0; state label = \"\";",
        "on click { count += 1; label = \"clicked\"; }",
    ));
    live.click();

    let report = live.reload(&view(
        "",
        "state count = 0; state label = false;",
        "on click { count += 1; }",
    ));
    assert_eq!(live.cell("count"), Some(StateValue::Int(1)), "kept");
    assert_eq!(live.value("label"), Some(Value::bool(false)), "reset");
    assert_eq!(report.notices.len(), 1);
    assert!(report.notices[0].message.contains("`label`"));
}

#[test]
fn a_record_gains_a_defaulted_field() {
    let mut live = Live::default();
    live.reload(&view(
        "record Point { x: I64; }",
        "state point = Point { x: 0 }; state seen = 0;",
        "on click { point.x += 4; }",
    ));
    live.click();

    let report = live.reload(&view(
        "record Point { x: I64; y: I64 = 7; }",
        "state point = Point { x: 0 }; state seen = 0;",
        "on click { seen = point.x * 100 + point.y; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    let point = live.value("point").expect("a point");
    assert_eq!(fields(&point), [Value::Int(4), Value::Int(7)]);
    live.click();
    assert_eq!(live.cell("seen"), Some(StateValue::Int(407)));
}

#[test]
fn a_record_field_without_a_default_resets() {
    let mut live = Live::default();
    live.reload(&view(
        "record Point { x: I64; }",
        "state point = Point { x: 0 };",
        "on click { point.x += 4; }",
    ));
    live.click();
    let report = live.reload(&view(
        "record Point { x: I64; y: I64; }",
        "state point = Point { x: 1, y: 2 };",
        "on click { point.x += 4; }",
    ));
    assert_eq!(report.notices.len(), 1);
    let point = live.value("point").expect("a point");
    assert_eq!(fields(&point), [Value::Int(1), Value::Int(2)]);
}

#[test]
fn an_enum_keeps_its_variant_by_name() {
    let types = "enum Mode { A; B(I64); C; }";
    let mut live = Live::default();
    live.reload(&view(
        types,
        "state mode = Mode::A;",
        "on click { mode = Mode::B(3); }",
    ));
    live.click();

    // Reordered, with a removed variant the state does not hold and a payload
    // that widens.
    let report = live.reload(&view(
        "enum Mode { B(F64); A; }",
        "state mode = Mode::A;",
        "on click { mode = Mode::A; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    match live.value("mode") {
        Some(Value::Agg(agg)) => {
            assert_eq!(agg.tag, 0);
            assert_eq!(agg.fields[..], [Value::Float(3.0)]);
        }
        other => panic!("{other:?}"),
    }

    // Removing the variant the state holds resets it.
    let report = live.reload(&view(
        "enum Mode { A; C; }",
        "state mode = Mode::C;",
        "on click { mode = Mode::A; }",
    ));
    assert_eq!(report.notices.len(), 1);
    assert_eq!(live.value("mode"), Some(Value::Int(1)));
}

#[test]
fn a_recursive_record_converts_at_every_depth() {
    let mut live = Live::default();
    live.reload(&view(
        "record Tree { value: I64; kids: List<Tree>; }",
        "state tree = Tree { value: 0, kids: [] };",
        "on click { tree = Tree { value: 1, kids: [Tree { value: 2, kids: [] }] }; }",
    ));
    live.click();

    let report = live.reload(&view(
        "record Tree { value: I64; kids: List<Tree>; weight: I64 = 9; }",
        "state tree = Tree { value: 0, kids: [] };",
        "on click { tree = Tree { value: 0, kids: [] }; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    let tree = live.value("tree").expect("a tree");
    let [value, Value::List(kids), weight] = fields(&tree) else {
        panic!("{tree:?}");
    };
    assert_eq!((value, weight), (&Value::Int(1), &Value::Int(9)));
    assert_eq!(
        fields(&kids[0]),
        [
            Value::Int(2),
            Value::List(Rc::new(Vec::new())),
            Value::Int(9)
        ]
    );
}

#[test]
fn a_migrate_function_carries_a_state_into_an_unrelated_type() {
    let mut live = Live::default();
    live.reload(&view("", "state on = 0;", "on click { on += 3; }"));
    live.click();

    let migrate = "@migrate(from: \"I64\") fn positive(old: I64) -> Bool { old > 0 }";
    let report = live.reload(&view(
        migrate,
        "state on = false;",
        "on click { on = !on; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(live.cell("on"), Some(StateValue::Bool(true)));
    live.click();
    assert_eq!(live.cell("on"), Some(StateValue::Bool(false)));
}

#[test]
fn a_component_member_migrates_a_converted_value() {
    let mut live = Live::default();
    live.reload(&view(
        "record Point { x: I64; }",
        "state point = Point { x: 0 };",
        "on click { point.x += 4; }",
    ));
    live.click();

    // The old `Point` converts into the edited one, which the member then
    // carries into the new type.
    let report = live.reload(&view(
        "record Point { x: I64; y: I64 = 7; }",
        "state point = 0;
        @migrate(from: \"Point\") fn flatten(old: Point) -> I64 { old.x * 100 + old.y }",
        "on click { point += 1; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(live.cell("point"), Some(StateValue::Int(407)));
}

#[test]
fn a_value_the_conversion_misses_falls_back_to_the_migrate_function() {
    let mut live = Live::default();
    live.reload(&view("", "state count = 0;", "on click { count += 300; }"));
    live.click();

    let migrate =
        "@migrate(from: \"I64\") fn clamp(old: I64) -> I8 { if old > 127 { 127 } else { 0 } }";
    let report = live.reload(&view(
        migrate,
        "state count: I8 = 0;",
        "on click { count += 0; }",
    ));
    assert!(report.notices.is_empty(), "{:?}", report.notices);
    assert_eq!(live.cell("count"), Some(StateValue::Int(127)));
}

#[test]
fn a_faulting_migrate_function_resets_with_its_fault() {
    let mut live = Live::default();
    live.reload(&view("", "state count = 0;", "on click { count += 1; }"));
    live.click();

    let migrate = "@migrate(from: \"I64\") fn broken(old: I64) -> Bool { 10 / (old - old) > 0 }";
    let report = live.reload(&view(
        migrate,
        "state count = true;",
        "on click { count = false; }",
    ));
    assert_eq!(live.cell("count"), Some(StateValue::Bool(true)), "reset");
    let [notice] = &report.notices[..] else {
        panic!("one notice: {:?}", report.notices);
    };
    assert_eq!(notice.code, "E5101");
    assert!(notice.message.contains("failed"), "{}", notice.message);
}

/// The diagnostic codes and messages a component with the declarations
/// `types` fails to package with.
fn errors(types: &str, members: &str) -> Vec<(String, String)> {
    let origin = Origin {
        package: "app".into(),
        module: vec!["clicker".into()],
        language: None,
    };
    build_view_package(
        &view(
            types,
            &format!("state on = false; {members}"),
            "on click { on = !on; }",
        ),
        &origin,
    )
    .expect_err("does not package")
    .into_iter()
    .map(|d| (d.code.to_string(), d.message))
    .collect()
}

#[test]
fn a_misused_migrate_is_e3712() {
    let cases = [
        (
            "@migrate(from: \"I64\") record R { x: I64; }",
            "marks a `fn`",
        ),
        (
            "@migrate fn f(old: I64) -> Bool { true }",
            "@migrate(from: \"I64\")",
        ),
        (
            "@migrate(to: \"I64\") fn f(old: I64) -> Bool { true }",
            "@migrate(from: \"I64\")",
        ),
        (
            "@migrate(from: \"I64\") fn f(old: I64, more: I64) -> Bool { true }",
            "one parameter",
        ),
        (
            "@migrate(from: \"I64\") fn f(old: String) -> Bool { true }",
            "but its parameter is `String`",
        ),
        ("@migrate(from: \"I64\") fn f(old: I64) { }", "declare it"),
        (
            "@migrate(from: \"I64\") fn f(old: I64) -> Bool { true }
            @migrate(from: \"I64\") fn g(old: I64) -> Bool { false }",
            "a second `@migrate` function",
        ),
    ];
    for (types, expected) in cases {
        let errors = errors(types, "");
        assert!(
            errors
                .iter()
                .any(|(code, message)| code == "E3712" && message.contains(expected)),
            "{types}: {errors:?}"
        );
    }
    let members = errors("", "@migrate(from: \"I64\") fn f(old: I64) { }");
    assert!(
        members.iter().any(|(code, _)| code == "E3712"),
        "{members:?}"
    );
}
