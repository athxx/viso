//! Native controls under hot reload and the release package: a toggle, a
//! slider, a tab strip and a text field respond to pointer, key and IME samples
//! and write their change back through a `bind` before the author's handler
//! for the same event runs, and a handler on an ancestor
//! runs once per sample.

#[path = "support/reload.rs"]
mod reload;

use std::cell::RefCell;
use std::rc::Rc;

use reload::{LiveRuntime, hot_reload_view};
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::CandidatePlan;
use viso_ui::text_edit::reconcile;
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, CheckMark, CheckShape, EffectStore, ImeEvent, Key, KeyEvent, KeyRouter,
    Modifiers, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase, PointerRouter, Rect,
    ScrollEvent, ScrollRouter, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
export component Controls {
    state on = false;
    state level: F32 = 0.0;
    state tab: U32 = 0;
    state name = "";
    state notes = "";
    state amount = "";
    state volume: F64 = 2.0;
    state volumes = 0;
    state changes = 0;
    state seen: F32 = 0.0;
    state submits = 0;
    state outer = 0;
    state local: Dp = 0dp;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            on click { outer += 1; }
            Toggle {
                width: 40dp;
                height: 20dp;
                on changed(event) { if on { changes += 1; } }
                bind checked <=> on;
            }
            Slider {
                width: 200dp;
                height: 20dp;
                min: 0.0;
                max: 10.0;
                step: 1.0;
                bind value <=> level;
                on changed(event) { seen = event.value; }
            }
            Row {
                width: 400dp;
                height: 20dp;
                Tabs {
                    width: 300dp;
                    height: 20dp;
                    bind selected <=> tab;
                    Text { width: 100dp; height: 20dp; }
                    Text { width: 100dp; height: 20dp; }
                    Text { width: 100dp; height: 20dp; }
                }
            }
            TextInput {
                width: 200dp;
                height: 20dp;
                bind value <=> name;
                on submitted { submits += 1; }
            }
            Row {
                width: 400dp;
                height: 20dp;
                Text {
                    width: 100dp;
                    height: 20dp;
                    on pointer_down(event) { local = event.position.x; }
                }
            }
            TextInput {
                width: 200dp;
                height: 20dp;
                multiline: true;
                bind value <=> notes;
                on submitted { submits += 10; }
            }
            TextInput {
                width: 200dp;
                height: 20dp;
                input: InputKind::integer;
                max_length: 4;
                bind value <=> amount;
            }
            NumberField {
                width: 200dp;
                height: 20dp;
                min: 0.0;
                max: 10.0;
                step: 0.5;
                bind value <=> volume;
                on changed(event) { volumes += 1; }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["controls".into()],
        language: None,
        ..Default::default()
    }
}

/// A mounted view and the stores it runs over.
#[derive(Default)]
struct Rt {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    root: Option<NodeId>,
    scratch: Vec<NodeId>,
    nodes: Vec<Option<NodeId>>,
    view: Option<Rc<RefCell<ViewHost>>>,
    last_good: CandidatePlan,
}

impl Rt {
    /// The view of `source` mounted by a hot reload.
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        let mut live = LiveRuntime {
            store: &mut rt.store,
            states: &mut rt.states,
            bindings: &mut rt.bindings,
            effects: &mut rt.effects,
            lists: &mut rt.lists,
            text_edits: &mut rt.text_edits,
            projectors: &mut rt.projectors,
            root: rt.root,
            scratch: &mut rt.scratch,
            nodes: &mut rt.nodes,
            view: &mut rt.view,
        };
        let done = hot_reload_view(&mut live, &rt.last_good, source, &origin()).expect("reloads");
        rt.root = live.root;
        rt.last_good = done.candidate;
        rt.settle();
        rt
    }

    /// The view of `source` loaded from its release package.
    fn packaged(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut rt = Rt::default();
        let view = load_view(
            &blob,
            &mut rt.store,
            &mut rt.states,
            &mut rt.bindings,
            &mut rt.lists,
        )
        .expect("loads");
        rt.root = view.root;
        rt.view = view.host;
        rt.settle();
        rt
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        if let Some(root) = self.root {
            self.store.layout(root, surface, &mut Vec::new());
            // Hit testing reads the top layer the last paint drew.
            self.store.repaint_dirty(root, &mut Vec::new());
        }
    }

    /// One pointer sample at `(x, y)`.
    fn pointer(&mut self, x: f32, y: f32, phase: PointerPhase) {
        let event = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        PointerRouter::route(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// One sample of finger `id` at `(x, y)`.
    fn touch(&mut self, id: u64, x: f32, y: f32, phase: PointerPhase) {
        let event = PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        };
        PointerRouter::route_contact(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root.expect("mounted"),
            viso_ui::PointerContact {
                id: viso_ui::PointerId(id),
                direct: true,
            },
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// A trackpad's pinch sample at `(x, y)`.
    fn trackpad(&mut self, x: f32, y: f32, scale: f32, rotation: f32, phase: viso_ui::PinchPhase) {
        viso_ui::route_trackpad_pinch(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            self.root.expect("mounted"),
            viso_ui::TrackpadPinch {
                x,
                y,
                scale,
                rotation,
                phase,
            },
            &mut self.scratch,
        );
        self.settle();
    }

    /// A primary click at `(x, y)`.
    fn click(&mut self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Down);
        self.pointer(x, y, PointerPhase::Up);
    }

    /// The time passing that fires every timer armed so far, then the wakes
    /// they deliver.
    fn wait(&mut self) {
        let mut timers = Vec::new();
        self.store.take_timer_requests(&mut timers);
        for timer in timers {
            (timer.on_fire)(&mut self.store);
        }
        KeyRouter::route_wakes(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
        );
        self.settle();
    }

    /// A wheel scroll of `delta_y` at `(x, y)`.
    fn wheel(&mut self, x: f32, y: f32, delta_y: f32) {
        let event = ScrollEvent {
            x,
            y,
            delta_x: 0.0,
            delta_y,
            modifiers: Modifiers::default(),
            phase: viso_ui::ScrollPhase::None,
        };
        ScrollRouter::route(&mut self.store, self.root.expect("mounted"), event);
        self.settle();
    }

    /// `key` pressed with Alt held, reaching the node whose access key it is.
    fn access(&mut self, key: char) {
        KeyRouter::route_access(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root.expect("mounted"),
            key,
        );
        self.settle();
    }

    /// A press of `key` to the focused node.
    fn key(&mut self, key: Key) {
        self.key_with(key, Modifiers::default());
    }

    /// A press of `key` with `modifiers` held to the focused node.
    fn key_with(&mut self, key: Key, modifiers: Modifiers) {
        let event = KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers,
        };
        KeyRouter::route_key(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// An IME sample to the focused node.
    fn ime(&mut self, event: ImeEvent) {
        KeyRouter::route_ime(
            &mut self.store,
            &mut self.states,
            &self.bindings,
            &mut self.text_edits,
            self.root.expect("mounted"),
            event,
            &mut self.scratch,
        );
        self.settle();
    }

    /// The frame's edit reconcile, change delivery, flush and structure hooks.
    fn settle(&mut self) {
        reconcile(&mut self.store, &mut self.text_edits, None);
        let mut edited = Vec::new();
        self.text_edits.take_changed(&mut edited);
        for node in edited {
            let Some(text) = self.text_edits.get(node).map(|b| b.text.clone()) else {
                continue;
            };
            KeyRouter::route_text_change(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                &mut self.text_edits,
                node,
                &text,
            );
        }
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut self.lists,
            &changed,
        );
        self.layout();
    }

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }

    fn int(&self, name: &str) -> i64 {
        self.state(name)
            .and_then(|v| v.as_int())
            .unwrap_or_else(|| panic!("`{name}` is no integer"))
    }

    fn float(&self, name: &str) -> f64 {
        self.state(name)
            .and_then(|v| v.as_float())
            .unwrap_or_else(|| panic!("`{name}` is no float"))
    }

    fn faulted(&self) -> bool {
        let host = self.view.as_ref().expect("a host");
        host.borrow().last_fault().is_some()
    }
}

fn targets() -> [Rt; 2] {
    [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)]
}

#[test]
fn a_toggle_flips_its_bound_state_on_a_click_and_space() {
    let switch = |checked| {
        Some(CheckMark {
            shape: CheckShape::Switch,
            checked,
        })
    };
    for mut rt in targets() {
        let toggle = rt.children_of_root()[0];
        assert_eq!(
            rt.store.check_mark(toggle),
            switch(false),
            "a track, knob at the start"
        );
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 1);
        assert_eq!(rt.int("changes"), 1);
        assert_eq!(rt.store.check_mark(toggle), switch(true));
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 0);
        assert_eq!(rt.int("changes"), 1);
        rt.store.set_focused(rt.children_of_root()[0].into());
        rt.key(Key::Space);
        assert_eq!(rt.int("on"), 1);
        assert_eq!(rt.int("changes"), 2);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_slider_takes_its_value_from_the_pointer_and_steps_with_keys() {
    for mut rt in targets() {
        rt.pointer(60.0, 30.0, PointerPhase::Down);
        assert_eq!(rt.float("level"), 3.0);
        assert_eq!(rt.float("seen"), 3.0);
        rt.pointer(200.0, 30.0, PointerPhase::Move);
        assert_eq!(rt.float("level"), 10.0);
        rt.pointer(200.0, 30.0, PointerPhase::Up);
        rt.key(Key::Left);
        assert_eq!(rt.float("level"), 9.0);
        rt.key(Key::Home);
        assert_eq!(rt.float("level"), 0.0);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_tab_strip_selects_the_child_a_click_lands_in() {
    for mut rt in targets() {
        rt.click(150.0, 50.0);
        assert_eq!(rt.int("tab"), 1);
        rt.click(250.0, 50.0);
        assert_eq!(rt.int("tab"), 2);
        rt.store.set_focused(rt.tabs().into());
        rt.key(Key::Left);
        assert_eq!(rt.int("tab"), 1);
        rt.key(Key::Home);
        assert_eq!(rt.int("tab"), 0);
    }
}

#[test]
fn a_text_field_writes_its_edited_text_back_and_submits() {
    for mut rt in targets() {
        rt.click(10.0, 70.0);
        rt.ime(ImeEvent::Preedit {
            text: "ni".into(),
            caret: 2,
        });
        rt.ime(ImeEvent::Commit {
            text: "你好".into(),
        });
        assert_eq!(rt.state("name"), Some(Value::Str(Rc::new("你好".into()))));
        rt.key(Key::Backspace);
        assert_eq!(rt.state("name"), Some(Value::Str(Rc::new("你".into()))));
        rt.key(Key::Enter);
        assert_eq!(rt.int("submits"), 1);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_number_field_takes_a_number_within_its_length() {
    for mut rt in targets() {
        rt.click(10.0, 130.0);
        rt.ime(ImeEvent::Commit { text: "x-1".into() });
        rt.ime(ImeEvent::Paste {
            text: "2a345".into(),
        });
        assert_eq!(rt.state("amount"), Some(Value::Str(Rc::new("-123".into()))));
        assert!(!rt.faulted());
    }
}

/// `got` is `want` as the view's cells hold an `F64`: at single precision.
fn assert_near(got: f64, want: f64, what: &str) {
    assert!((got - want).abs() < 1e-5, "{got} is not {want}: {what}");
}

#[test]
fn a_number_field_scrubs_steps_and_settles_its_number() {
    for mut rt in targets() {
        rt.pointer(10.0, 150.0, PointerPhase::Down);
        rt.pointer(30.0, 150.0, PointerPhase::Move);
        rt.pointer(30.0, 150.0, PointerPhase::Up);
        assert_near(rt.float("volume"), 7.0, "ten steps scrubbed from 2");
        rt.click(10.0, 150.0);
        rt.ime(ImeEvent::Commit { text: "4x".into() });
        assert_near(rt.float("volume"), 4.0, "the text replaced its selection");
        rt.key(Key::Up);
        assert_near(rt.float("volume"), 4.5, "");
        rt.ime(ImeEvent::Commit { text: "9".into() });
        assert_near(rt.float("volume"), 4.59, "typed on, read as it is");
        rt.key(Key::Enter);
        assert_near(rt.float("volume"), 4.6, "settled at the precision");
        rt.wheel(10.0, 150.0, -32.0);
        assert_near(rt.float("volume"), 5.6, "two steps up");
        rt.key(Key::Home);
        rt.ime(ImeEvent::Commit { text: "9".into() });
        assert_near(rt.float("volume"), 5.6, "95.6 is out of range");
        rt.key(Key::Enter);
        assert_near(rt.float("volume"), 10.0, "settled within the range");
        assert!(rt.int("volumes") >= 6);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_multi_line_field_breaks_lines_on_enter_and_submits_with_the_accelerator() {
    for mut rt in targets() {
        rt.click(10.0, 110.0);
        rt.ime(ImeEvent::Commit { text: "a".into() });
        rt.key(Key::Enter);
        rt.ime(ImeEvent::Paste {
            text: "b\r\nc".into(),
        });
        assert_eq!(
            rt.state("notes"),
            Some(Value::Str(Rc::new("a\nb\nc".into())))
        );
        assert_eq!(rt.int("submits"), 0);
        let apple = cfg!(target_vendor = "apple");
        rt.key_with(
            Key::Enter,
            Modifiers {
                control: !apple,
                logo: apple,
                ..Modifiers::default()
            },
        );
        assert_eq!(rt.int("submits"), 10);
        assert!(!rt.faulted());
    }
}

#[test]
fn an_ancestor_handler_runs_once_per_sample() {
    for mut rt in targets() {
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("outer"), 1);
        rt.click(390.0, 290.0);
        assert_eq!(rt.int("outer"), 2);
    }
}

#[test]
fn a_pointer_position_is_local_to_the_node() {
    for mut rt in targets() {
        rt.pointer(30.0, 90.0, PointerPhase::Down);
        assert_eq!(rt.float("local"), 30.0);
        rt.pointer(30.0, 90.0, PointerPhase::Up);
    }
}

impl Rt {
    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let arena = self.store.arena();
        let mut out = Vec::new();
        for c in arena.children(node) {
            out.push(c);
        }
        out
    }

    fn children_of_root(&self) -> Vec<NodeId> {
        self.children(self.root.expect("mounted"))
    }

    /// The tab strip, inside the root's third child.
    fn tabs(&self) -> NodeId {
        self.children(self.children_of_root()[2])[0]
    }
}

#[test]
fn a_control_a_region_mounts_responds_and_writes_back() {
    const REGIONS: &str = r#"
export component Checks {
    state show = true;
    state on = false;
    state ids = [1, 2, 3];
    state picked = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            if show {
                Toggle {
                    width: 40dp;
                    height: 20dp;
                    bind checked <=> on;
                }
            }
            for id in ids key id {
                Toggle {
                    width: 40dp;
                    height: 20dp;
                    checked: id == 2;
                    on changed(event) { picked = id; }
                }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(REGIONS), Rt::packaged(REGIONS)] {
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("on"), 1);
        rt.click(10.0, 70.0);
        assert_eq!(rt.int("picked"), 3);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_converted_bind_shows_and_writes_back_through_its_converter() {
    const CONVERTED: &str = r#"
record Tenths {}
impl TwoWayConverter<I64, F32> for Tenths {
    fn to_view(model: I64) -> F32 { return (model as F32) / 10.0; }
    fn to_model(view: F32) -> Option<I64> {
        if view > 8.0 { return None; }
        return Some((view * 10.0) as I64);
    }
}
export component Dial {
    state level = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Slider {
                width: 200dp;
                height: 20dp;
                min: 0.0;
                max: 10.0;
                step: 1.0;
                bind value <=> level using Tenths;
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(CONVERTED), Rt::packaged(CONVERTED)] {
        rt.pointer(60.0, 10.0, PointerPhase::Down);
        assert_eq!(rt.int("level"), 30, "to_model of the slider's 3.0");
        rt.pointer(200.0, 10.0, PointerPhase::Move);
        assert_eq!(rt.int("level"), 30, "a `None` rejects the update");
        rt.pointer(200.0, 10.0, PointerPhase::Up);
        rt.store.set_focused(rt.children_of_root()[0].into());
        rt.key(Key::Left);
        assert_eq!(
            rt.int("level"),
            20,
            "the slider reads to_view(30) and steps down"
        );
        assert!(!rt.faulted());
    }

    let codes: Vec<String> = build_view_package(
        "export component Field {
            state amount = 5;
            view { Column { TextInput { bind value <=> amount using I64; } } }
        }",
        &origin(),
    )
    .expect_err("does not package")
    .into_iter()
    .map(|d| d.code.to_string())
    .collect();
    assert!(codes.contains(&"E2201".to_string()), "{codes:?}");
}

#[test]
fn a_template_part_reads_its_parameter_and_runs_its_callers_override() {
    const TEMPLATED: &str = r#"
template Dial(level: F32, label: String = "dial") {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            part knob: Slider {
                width: 200dp;
                height: 20dp;
                min: 0.0;
                max: 10.0;
                step: 1.0;
                value: level;
            }
            part caption: Text { width: 100dp; height: 20dp; text: label; }
        }
    }
}
export component Mixer {
    state amount: F32 = 5.0;
    state taps = 0;
    view {
        Column {
            use Dial(amount) {
                override part knob {
                    on changed(event) { amount = event.value; }
                }
                override part caption {
                    on click { taps += 1; }
                }
            };
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(TEMPLATED), Rt::packaged(TEMPLATED)] {
        let dial = rt.children_of_root()[0];
        let knob = rt.children(dial)[0];
        rt.store.set_focused(knob.into());
        rt.key(Key::Left);
        assert_eq!(
            rt.float("amount"),
            4.0,
            "the knob reads `level`, which is `amount`"
        );
        rt.key(Key::Left);
        assert_eq!(rt.float("amount"), 3.0);
        rt.click(10.0, 30.0);
        assert_eq!(rt.int("taps"), 1, "the caller's handler on the caption");
        assert!(!rt.faulted());
    }
}

#[test]
fn a_disclosure_shows_its_body_while_expanded_and_its_header_flips_it() {
    const DISCLOSURE: &str = r#"
export component Panel {
    state open = false;
    state flips = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Disclosure {
                width: 400dp;
                bind expanded <=> open;
                on changed(event) { flips += 1; }
                Row {
                    width: 400dp;
                    height: 20dp;
                    Text { text: "More"; width: 100dp; height: 20dp; }
                }
                Column { width: 400dp; height: 100dp; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(DISCLOSURE), Rt::packaged(DISCLOSURE)] {
        let disclosure = rt.children_of_root()[0];
        let [header, body] = rt.children(disclosure)[..] else {
            panic!("a header and a body");
        };
        let expanded = |rt: &Rt| rt.store.semantic_state(header).and_then(|s| s.expanded);
        assert!(rt.store.folded(disclosure) && rt.store.hidden(body));
        assert_eq!(rt.store.bounds(body).h, 0.0);
        assert_eq!(expanded(&rt), Some(false));

        rt.click(10.0, 10.0);
        assert_eq!((rt.int("open"), rt.int("flips")), (1, 1));
        assert!(!rt.store.hidden(body));
        assert_eq!(rt.store.bounds(body).h, 100.0);
        assert_eq!(expanded(&rt), Some(true));

        rt.click(10.0, 60.0);
        assert_eq!(rt.int("flips"), 1, "the body flips nothing");

        rt.store.set_focused(disclosure.into());
        rt.key(Key::Space);
        assert_eq!(rt.int("open"), 0);
        assert!(rt.store.hidden(body));
        assert!(!rt.faulted());
    }
}

#[test]
fn a_radio_group_s_caption_options_draw_a_circle_that_fills_with_the_selection() {
    const RADIOS: &str = r#"
export component Sizes {
    state size: U32 = 1;
    view {
        Column {
            RadioGroup {
                bind selected <=> size;
                Text { text: "Small"; width: 100dp; height: 20dp; }
                Text { text: "Large"; width: 100dp; height: 20dp; }
                Row { Text { text: "Composed"; } }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(RADIOS), Rt::packaged(RADIOS)] {
        let [group] = rt.children_of_root()[..] else {
            panic!("a group");
        };
        let [small, large, composed] = rt.children(group)[..] else {
            panic!("three options");
        };
        let radio = |checked| {
            Some(CheckMark {
                shape: CheckShape::Radio,
                checked,
            })
        };
        assert_eq!(rt.store.check_mark(small), radio(false));
        assert_eq!(rt.store.check_mark(large), radio(true));
        assert_eq!(rt.store.check_mark(composed), None, "it draws its own");

        rt.click(10.0, 10.0);
        assert_eq!(rt.int("size"), 0);
        assert_eq!(rt.store.check_mark(small), radio(true));
        assert_eq!(rt.store.check_mark(large), radio(false));
        assert!(rt.store.is_transitioning(), "the circles ease over");
    }
}

#[test]
fn a_disclosure_s_caption_header_draws_a_chevron_that_turns_with_it() {
    const DISCLOSURE: &str = r#"
export component Panel {
    state open = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Disclosure {
                bind expanded <=> open;
                Text { text: "More"; width: 100dp; height: 20dp; }
                Row {
                    Text { text: "Body"; }
                }
            }
            Disclosure {
                Row { Text { text: "Composed"; } }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(DISCLOSURE), Rt::packaged(DISCLOSURE)] {
        let [caption, composed] = rt.children_of_root()[..] else {
            panic!("two disclosures");
        };
        let header = rt.children(caption)[0];
        let chevron = |checked| {
            Some(CheckMark {
                shape: CheckShape::Chevron,
                checked,
            })
        };
        assert_eq!(rt.store.check_mark(header), chevron(false));
        assert_eq!(rt.store.check_mark(rt.children(composed)[0]), None);

        rt.click(10.0, 10.0);
        assert_eq!(rt.int("open"), 1);
        assert_eq!(rt.store.check_mark(header), chevron(true), "it turns down");
        assert!(rt.store.is_transitioning());
        rt.store
            .tick_transitions(std::time::Duration::from_millis(200));
        assert!(!rt.store.is_transitioning());
    }
}

#[test]
fn a_node_can_show_its_focus_its_own_way_instead_of_the_ring() {
    const RINGS: &str = r#"
export component Rings {
    state plain = true;
    view {
        Column {
            Button { text: "Ringed"; }
            Button { text: "Own"; focus_ring: false; }
            Button {
                text: "Bound";
                width: 100dp;
                height: 20dp;
                focus_ring: plain;
                on click { plain = false; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(RINGS), Rt::packaged(RINGS)] {
        let [ringed, own, bound] = rt.children_of_root()[..] else {
            panic!("three buttons");
        };
        assert!(rt.store.wears_focus_ring(ringed));
        assert!(!rt.store.wears_focus_ring(own));
        assert!(rt.store.wears_focus_ring(bound));
        let at = rt.store.world(bound);
        rt.click(at.x + 2.0, at.y + 2.0);
        assert!(!rt.store.wears_focus_ring(bound), "it follows its binding");
    }
}

#[test]
fn a_modal_covers_the_surface_traps_focus_and_asks_to_close_on_its_scrim_or_escape() {
    const MODAL: &str = r#"
export component Page {
    state shown = false;
    state asks = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            CheckBox { width: 100dp; height: 20dp; bind checked <=> shown; }
            Modal {
                bind open <=> shown;
                on changed(event) { asks += 1; }
                Column {
                    width: 200dp;
                    height: 100dp;
                    TextInput { width: 200dp; height: 20dp; }
                }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(MODAL), Rt::packaged(MODAL)] {
        let [check, modal] = rt.children_of_root()[..] else {
            panic!("a check and a modal");
        };
        let content = rt.children(modal)[0];
        let field = rt.children(content)[0];
        let role = |rt: &Rt| rt.store.semantics(modal).map(|s| s.role);
        assert!(rt.store.is_layer(modal) && rt.store.hidden(modal));
        assert_eq!(role(&rt), Some(viso_ui::Role::Dialog));

        rt.store.set_focused(Some(check));
        rt.key(Key::Space);
        assert_eq!(rt.int("shown"), 1);
        assert!(!rt.store.hidden(modal));
        let bounds = rt.store.bounds(modal);
        assert_eq!((bounds.w, bounds.h), (400.0, 300.0), "over the surface");
        let at = rt.store.bounds(content);
        assert_eq!((at.x, at.y), (100.0, 100.0), "its content centred");
        assert_eq!(rt.store.focused(), Some(field), "focus moves in");
        assert_eq!(rt.store.focus_scope(), Some(modal));

        rt.key(Key::Escape);
        assert_eq!((rt.int("shown"), rt.int("asks")), (0, 1));
        assert!(rt.store.hidden(modal));
        assert_eq!(rt.store.focused(), Some(check), "focus comes back");
        assert_eq!(rt.store.focus_scope(), None);

        rt.key(Key::Space);
        assert_eq!(rt.int("shown"), 1);
        rt.click(150.0, 150.0);
        assert_eq!(rt.int("shown"), 1, "a press inside stays open");
        rt.click(10.0, 10.0);
        assert_eq!((rt.int("shown"), rt.int("asks")), (0, 2));
        assert!(!rt.store.hidden(check) && rt.store.hidden(modal));
        assert!(!rt.faulted());
    }
}

#[test]
fn a_toast_shows_while_open_speaks_politely_and_lets_the_pointer_through() {
    const TOAST: &str = r#"
export component Page {
    state shown = true;
    state taps = 0;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Button { text: "Tap"; width: 100dp; height: 20dp; on click { taps += 1; } }
            Toast {
                open: shown;
                Text { text: "Saved"; width: 100dp; height: 20dp; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(TOAST), Rt::packaged(TOAST)] {
        let [_, toast] = rt.children_of_root()[..] else {
            panic!("a button and a toast");
        };
        let note = rt.children(toast)[0];
        assert!(rt.store.is_layer(toast) && !rt.store.hidden(toast));
        let said = rt.store.semantics(toast).expect("described");
        assert_eq!(said.role, viso_ui::Role::Status);
        assert_eq!(said.live, viso_ui::LiveRegion::Polite);
        let at = rt.store.bounds(note);
        assert_eq!((at.x, at.y), (150.0, 280.0), "at the bottom");
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("taps"), 1, "the toast's box lets the press through");
        assert_eq!(rt.store.focus_scope(), None, "a toast traps nothing");
        assert!(!rt.faulted());
    }
}

#[test]
fn a_navigation_stack_shows_its_current_page_slides_to_the_next_and_goes_back() {
    const STACK: &str = r#"
export component Page {
    state page: U32 = 0;
    state backs = 0;
    view {
        NavigationStack {
            width: 400dp;
            height: 300dp;
            page_transition: PageTransition::slide;
            bind current <=> page;
            on current_changed(event) { backs += 1; }
            Column { width: 400dp; height: 300dp; on click { page = 1; } }
            Column { width: 400dp; height: 300dp; }
            Column { width: 400dp; height: 300dp; }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(STACK), Rt::packaged(STACK)] {
        let stack = rt.root.expect("mounted");
        let [first, second, third] = rt.children(stack)[..] else {
            panic!("three pages");
        };
        assert!(!rt.store.hidden(first) && rt.store.hidden(second) && rt.store.hidden(third));
        assert!(
            rt.store.clips(stack),
            "a stack clips its pages as they move"
        );

        rt.click(10.0, 10.0);
        assert_eq!(rt.int("page"), 1);
        assert!(
            !rt.store.hidden(second) && !rt.store.hidden(first),
            "both show as they move"
        );
        assert_eq!(
            rt.store.translate(second).x,
            400.0,
            "it slides in from the edge"
        );
        for _ in 0..120 {
            rt.store
                .tick_transitions(std::time::Duration::from_millis(16));
        }
        assert!(rt.store.hidden(first) && !rt.store.hidden(second));
        assert_eq!(rt.store.translate(second).x, 0.0);

        rt.store.set_focused(stack.into());
        rt.key(Key::Back);
        assert_eq!(
            (rt.int("page"), rt.int("backs")),
            (0, 1),
            "back asks for the page before"
        );
        assert!(!rt.store.hidden(first));
        rt.key(Key::Escape);
        assert_eq!(rt.int("backs"), 1, "the first page has none before it");
        assert!(!rt.faulted());
    }
}

#[test]
fn a_hero_flies_between_a_navigation_stack_s_pages() {
    const HEROES: &str = r#"
export component Page {
    state page: U32 = 0;
    view {
        NavigationStack {
            width: 400dp;
            height: 300dp;
            page_transition: PageTransition::fade;
            bind current <=> page;
            Stack {
                width: 400dp;
                height: 300dp;
                on click { page = 1; }
                Column { width: 40dp; height: 40dp; hero: "card"; }
            }
            Stack {
                width: 400dp;
                height: 300dp;
                content_align: Alignment2D::center;
                Column { width: 80dp; height: 80dp; hero: "card"; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(HEROES), Rt::packaged(HEROES)] {
        let stack = rt.root.expect("mounted");
        let [first, second] = rt.children(stack)[..] else {
            panic!("two pages");
        };
        let (from, to) = (rt.children(first)[0], rt.children(second)[0]);
        assert_eq!(rt.store.hero(to), Some("card"));
        rt.click(200.0, 200.0);
        assert_eq!(rt.int("page"), 1);
        let r = rt.store.world(to);
        assert!(
            (r.x + r.w / 2.0 - 20.0).abs() < 0.5 && (r.w - 40.0).abs() < 0.5,
            "it starts where the other stands: {r:?}"
        );
        assert_eq!(rt.store.opacity(from), 0.0);
        for _ in 0..40 {
            rt.store
                .tick_transitions(std::time::Duration::from_millis(16));
        }
        assert_eq!(rt.store.world(to).w, 80.0);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_button_takes_the_focus_and_clicks_on_space_or_enter() {
    const SRC: &str = r#"
export component Clicks {
    state clicks = 0;
    view {
        Column {
            Button { text: "Go"; width: 100dp; height: 20dp; on click { clicks += 1; } }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(SRC), Rt::packaged(SRC)] {
        let [button] = rt.children_of_root()[..] else {
            panic!("a button");
        };
        assert!(rt.store.focusable(button), "a button takes the focus");
        rt.store.set_focused(Some(button));
        rt.key(Key::Enter);
        rt.key(Key::Space);
        assert_eq!(rt.int("clicks"), 2);
        rt.key(Key::A);
        assert_eq!(rt.int("clicks"), 2);
        assert!(!rt.faulted());
    }
}

#[test]
fn a_tap_waits_out_a_double_tap_and_a_double_tap_takes_its_place() {
    const SRC: &str = r#"
export component Taps {
    state taps = 0;
    state doubles = 0;
    state plain = 0;
    view {
        Column {
            Row {
                width: 100dp; height: 40dp;
                on tap { taps += 1; }
                on double_tap(event) { doubles += 1; }
            }
            Row { width: 100dp; height: 40dp; on tap { plain += 1; } }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(SRC), Rt::packaged(SRC)] {
        let [both, plain] = rt.children_of_root()[..] else {
            panic!("two rows");
        };
        assert!(!rt.store.focusable(both), "a waiting tap takes no focus");
        rt.click(10.0, 10.0);
        assert_eq!(rt.int("taps"), 0, "the tap waits");
        rt.wait();
        assert_eq!(rt.int("taps"), 1, "and runs when no second press came");

        rt.click(10.0, 10.0);
        rt.click(12.0, 10.0);
        assert_eq!(rt.int("doubles"), 1);
        rt.wait();
        assert_eq!(rt.int("taps"), 1, "a double tap is no tap");
        assert_eq!(rt.int("doubles"), 1);

        rt.click(10.0, 50.0);
        assert_eq!(rt.int("plain"), 1, "a tap alone runs at once");
        assert!(!rt.store.focusable(plain));
        assert!(!rt.faulted());
    }
}

#[test]
fn a_drag_that_lifts_fast_swipes_its_way_and_ends_with_its_velocity() {
    const SRC: &str = r#"
export component Swipes {
    state lefts = 0;
    state ups = 0;
    state speed = Offset { x: 0dp, y: 0dp };
    view {
        Column {
            Row {
                width: 200dp; height: 200dp;
                on swipe(event) {
                    lefts += match event.direction { SwipeDirection::left => 1, _ => 0 };
                    ups += match event.direction { SwipeDirection::up => 1, _ => 0 };
                }
                on drag_end(event) { speed = event.velocity; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(SRC), Rt::packaged(SRC)] {
        let t0 = viso_ui::Instant::now();
        let stroke = |rt: &mut Rt, points: &[(f32, f32)]| {
            for (k, &(x, y)) in points.iter().enumerate() {
                rt.store
                    .set_input_time(t0 + std::time::Duration::from_millis(10 * k as u64));
                let phase = match k {
                    0 => PointerPhase::Down,
                    _ if k == points.len() - 1 => PointerPhase::Up,
                    _ => PointerPhase::Move,
                };
                rt.pointer(x, y, phase);
            }
        };
        stroke(
            &mut rt,
            &[
                (150.0, 100.0),
                (130.0, 100.0),
                (100.0, 101.0),
                (70.0, 101.0),
                (70.0, 101.0),
            ],
        );
        assert_eq!((rt.int("lefts"), rt.int("ups")), (1, 0));
        // The velocity's `x`, a length's px part.
        let speed = match rt.state("speed") {
            Some(Value::Agg(offset)) => match &offset.fields[0] {
                Value::Agg(x) => x.fields[0].as_float(),
                other => other.as_float(),
            },
            _ => None,
        };
        assert!(speed.is_some_and(|v| v < -1000.0), "{speed:?}");
        stroke(
            &mut rt,
            &[
                (100.0, 150.0),
                (100.0, 145.0),
                (101.0, 141.0),
                (101.0, 138.0),
                (101.0, 138.0),
            ],
        );
        assert_eq!((rt.int("lefts"), rt.int("ups")), (1, 0), "too slow");
        stroke(
            &mut rt,
            &[
                (100.0, 150.0),
                (100.0, 120.0),
                (101.0, 90.0),
                (101.0, 60.0),
                (101.0, 60.0),
            ],
        );
        assert_eq!((rt.int("lefts"), rt.int("ups")), (1, 1));
        assert!(!rt.faulted());
    }
}

#[test]
fn two_fingers_or_a_trackpad_pinch_a_node_that_hears_pinches() {
    const SRC: &str = r#"
export component Pinches {
    state zoom: F32 = 1.0;
    state turn: Angle = 0deg;
    state starts = 0;
    state ends = 0;
    view {
        Column {
            Row {
                width: 200dp; height: 200dp;
                on pinch_start(event) { starts += 1; }
                on pinch_move(event) { zoom = event.scale; turn = event.rotation; }
                on pinch_end(event) { ends += 1; }
            }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(SRC), Rt::packaged(SRC)] {
        rt.touch(1, 50.0, 100.0, PointerPhase::Down);
        rt.touch(2, 100.0, 100.0, PointerPhase::Down);
        rt.touch(2, 150.0, 100.0, PointerPhase::Move);
        rt.touch(2, 50.0, 200.0, PointerPhase::Move);
        assert_eq!(rt.int("starts"), 1);
        assert_eq!(rt.float("zoom"), 2.0);
        assert!(
            (rt.float("turn") - 90.0).abs() < 1e-3,
            "{}",
            rt.float("turn")
        );
        rt.touch(1, 50.0, 100.0, PointerPhase::Up);
        assert_eq!(rt.int("ends"), 1);
        rt.touch(2, 50.0, 200.0, PointerPhase::Up);

        use viso_ui::PinchPhase::{End, Move, Start};
        rt.trackpad(100.0, 100.0, 1.0, 0.0, Start);
        rt.trackpad(100.0, 100.0, 1.5, 0.0, Move);
        rt.trackpad(100.0, 100.0, 2.0, -std::f32::consts::FRAC_PI_2, Move);
        rt.trackpad(100.0, 100.0, 1.0, 0.0, End);
        assert_eq!((rt.int("starts"), rt.int("ends")), (2, 2));
        assert_eq!(rt.float("zoom"), 3.0);
        assert!(
            (rt.float("turn") + 90.0).abs() < 1e-3,
            "{}",
            rt.float("turn")
        );
        assert!(!rt.faulted());
    }
}

#[test]
fn an_access_key_is_cued_in_the_text_and_reaches_its_control() {
    const SRC: &str = r#"
export component Keys {
    state clicks = 0;
    state wrap = false;
    view {
        Column {
            Button { text: "Save"; access_key: "s"; on click { clicks += 1; } }
            CheckBox { label: "Word wrap"; access_key: "W"; bind checked <=> wrap; }
            TextInput { width: 100dp; height: 20dp; access_key: "n"; }
        }
    }
}
"#;
    for mut rt in [Rt::reloaded(SRC), Rt::packaged(SRC)] {
        let [button, check, field] = rt.children_of_root()[..] else {
            panic!("three controls");
        };
        assert_eq!(rt.store.access_key(button), Some('s'));
        let cued = |rt: &Rt, node| {
            let request = rt.store.text_request(node).expect("a text");
            request
                .spans
                .iter()
                .filter(|s| s.style.cue)
                .map(|s| s.range.clone())
                .collect::<Vec<_>>()
                .pop()
        };
        assert_eq!(cued(&rt, button), Some(0..1));
        assert_eq!(cued(&rt, check), Some(0..1), "whatever its case");

        rt.access('s');
        assert_eq!(rt.int("clicks"), 1);
        rt.access('w');
        assert_eq!(rt.int("wrap"), 1);
        assert_eq!(rt.store.focused(), Some(check));
        rt.access('n');
        assert_eq!(rt.store.focused(), Some(field));
        assert!(!rt.faulted());
    }
}
