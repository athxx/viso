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
    SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
export component Controls {
    state on = false;
    state level: F32 = 0.0;
    state tab: U32 = 0;
    state name = "";
    state notes = "";
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
        rt.layout();
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
        rt.layout();
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

    /// A primary click at `(x, y)`.
    fn click(&mut self, x: f32, y: f32) {
        self.pointer(x, y, PointerPhase::Down);
        self.pointer(x, y, PointerPhase::Up);
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
