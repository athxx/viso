//! A view's look on its nodes: `background` and `opacity`, constant or read
//! from state, reach the retained nodes at mount and again only when a state
//! they read changes, on the hot-reloaded and the packaged view, and inside a
//! region; with a `transition.*` a changed value moves in over time.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::{Origin, compile_file};
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, Rgba, SemanticProjector, Srgb, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{ViewHost, load_view};

const FADE: &str = r#"
component Fade {
    state lit = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Text {
                width: 20dp;
                height: 20dp;
                background: if lit { #ff0000 } else { #ff000000 };
                opacity: if lit { 1.0f32 } else { 0.0f32 };
                transition.opacity: Transition { duration: 100ms, easing: Easing::linear };
                transition.background: Transition {
                    duration: 100ms,
                    delay: 50ms,
                    easing: Easing::linear,
                    reduced: ReducedMotion::keep,
                };
                on click { lit = !lit; }
            }
        }
    }
}
"#;

const SOURCE: &str = r#"
component Look {
    state lit = false;
    state level: F32 = 0.5f32;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            background: #102030;
            Text {
                width: 20dp;
                height: 20dp;
                background: if lit { #ff0000 } else { #00ff0080 };
                opacity: level;
                on click { lit = !lit; level = 1.0f32; }
            }
            Column {
                width: 100dp;
                height: 100dp;
                if lit {
                    Text { width: 10dp; height: 10dp; opacity: 0.25f32; }
                }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["look".into()],
        language: None,
    }
}

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
    /// The candidate a reload made live.
    plan: CandidatePlan,
}

impl Rt {
    fn reloaded() -> Self {
        Rt::reload(SOURCE)
    }

    fn packaged() -> Self {
        Rt::package(SOURCE)
    }

    fn reload(source: &str) -> Self {
        let mut rt = Rt::default();
        rt.edit(source);
        rt
    }

    /// Reloads `source` over the live view.
    fn edit(&mut self, source: &str) {
        let mut live = LiveRuntime {
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
        let done = hot_reload_view(&mut live, &self.plan, source, &origin()).expect("reloads");
        self.root = live.root;
        self.plan = done.candidate;
        self.layout();
    }

    fn package(source: &str) -> Self {
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
        }
    }

    fn click(&mut self, x: f32, y: f32) {
        let root = self.root.expect("mounted");
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x,
                y,
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
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &changed,
        );
        self.layout();
    }

    fn children(&self, node: NodeId) -> Vec<NodeId> {
        let arena = self.store.arena();
        let mut out = Vec::new();
        let mut child = arena.links(node).and_then(|l| l.first_child);
        while let Some(c) = child {
            out.push(c);
            child = arena.links(c).and_then(|l| l.next_sibling);
        }
        out
    }
}

fn rgba(r: f32, g: f32, b: f32, a: f32) -> Rgba {
    Rgba { r, g, b, a }
}

#[test]
fn a_nodes_look_follows_the_state_it_reads() {
    for mut rt in [Rt::reloaded(), Rt::packaged()] {
        let root = rt.root.expect("mounted");
        let fill = rt.store.style(root).fill;
        assert_eq!(fill, Srgb::from_rgba32(0x102030ff).into_linear_straight());
        let [text, region] = rt.children(root)[..] else {
            panic!("a text and a region column");
        };
        assert_eq!(
            rt.store.style(text).fill,
            rgba(0.0, 1.0, 0.0, 128.0 / 255.0)
        );
        assert_eq!(rt.store.opacity(text), 0.5);
        assert!(rt.children(region).is_empty());

        rt.click(5.0, 5.0);
        assert_eq!(rt.store.style(text).fill, rgba(1.0, 0.0, 0.0, 1.0));
        assert_eq!(rt.store.opacity(text), 1.0);
        let [shown] = rt.children(region)[..] else {
            panic!("the region's text");
        };
        assert_eq!(
            rt.store.opacity(shown),
            0.25,
            "a region node shows its look"
        );
    }
}

#[test]
fn a_changed_look_moves_in_over_its_transition() {
    let ms = Duration::from_millis;
    for mut rt in [Rt::reload(FADE), Rt::package(FADE)] {
        let root = rt.root.expect("mounted");
        let [text] = rt.children(root)[..] else {
            panic!("the text");
        };
        assert_eq!(rt.store.opacity(text), 0.0, "the first value shows at once");
        assert!(!rt.store.is_transitioning());

        rt.click(5.0, 5.0);
        assert_eq!(rt.store.opacity(text), 0.0, "a changed value moves in");
        rt.store.tick_transitions(ms(50));
        assert_eq!(rt.store.opacity(text), 0.5);
        assert_eq!(
            rt.store.style(text).fill.a,
            0.0,
            "the background waits out its delay"
        );
        rt.store.tick_transitions(ms(50));
        assert_eq!(rt.store.opacity(text), 1.0);
        assert_eq!(rt.store.style(text).fill, rgba(1.0, 0.0, 0.0, 0.5));
        rt.store.tick_transitions(ms(50));
        assert_eq!(rt.store.style(text).fill, rgba(1.0, 0.0, 0.0, 1.0));
        assert!(!rt.store.is_transitioning(), "both arrived");

        rt.states.update_env(|env| env.reduced_motion = true);
        rt.click(5.0, 5.0);
        assert_eq!(
            rt.store.opacity(text),
            0.0,
            "an instant transition skips its move under reduced motion"
        );
        rt.store.tick_transitions(ms(100));
        assert_eq!(
            rt.store.style(text).fill,
            rgba(1.0, 0.0, 0.0, 0.5),
            "a kept transition still moves"
        );
        assert!(rt.store.is_transitioning());
    }
}

/// The error codes a view whose text node binds `property: value;` compiles with.
fn codes(property: &str, value: &str) -> Vec<String> {
    let source =
        format!("export component A {{ view {{ Text {{ width: 10dp; {property}: {value}; }} }} }}");
    compile_file(&source, &origin())
        .errors()
        .map(|d| d.code.to_string())
        .collect()
}

#[test]
fn a_transition_names_an_animatable_property_and_takes_a_transition() {
    let spec = "Transition { duration: 100ms }";
    assert_eq!(codes("transition.opacity", spec), Vec::<String>::new());
    assert_eq!(
        codes("transition.background", "Transition {}"),
        Vec::<String>::new()
    );
    assert_eq!(codes("transition.opacity", "100ms"), ["E3703"]);
    assert_eq!(codes("transition.text", spec), ["E3703"]);
    assert_eq!(
        codes("transition.translate", spec),
        ["E3711"],
        "a transition the runtime does not play yet is no silent no-op"
    );
}

/// A view whose text, and a text its region mounts, fade with `lit`; `{lead}`
/// opens its column and `{target}` is the lit opacity.
fn carried(lead: &str, target: &str) -> String {
    format!(
        "component Carry {{
    state lit = false;
    view {{
        Column {{
            width: 400dp;
            height: 300dp;
            {lead}
            Text {{
                width: 20dp;
                height: 20dp;
                opacity: if lit {{ {target} }} else {{ 0.0f32 }};
                transition.opacity: Transition {{ duration: 100ms, easing: Easing::linear }};
                on click {{ lit = !lit; }}
            }}
            Column {{
                width: 100dp;
                height: 100dp;
                if true {{
                    Text {{
                        width: 10dp;
                        height: 10dp;
                        opacity: if lit {{ 1.0f32 }} else {{ 0.0f32 }};
                        transition.opacity: Transition {{ duration: 100ms, easing: Easing::linear }};
                    }}
                }}
            }}
        }}
    }}
}}"
    )
}

#[test]
fn a_transition_in_flight_carries_across_a_reload() {
    let ms = Duration::from_millis;
    // A property edit of a region-free view keeps its nodes and their moves.
    let mut rt = Rt::reload(FADE);
    rt.click(5.0, 5.0);
    rt.store.tick_transitions(ms(50));
    rt.edit(&FADE.replace("width: 400dp", "width: 401dp"));
    let root = rt.root.expect("mounted");
    let [text] = rt.children(root)[..] else {
        panic!("the text");
    };
    assert_eq!(
        rt.store.opacity(text),
        0.5,
        "the edit keeps the shown value"
    );
    rt.store.tick_transitions(ms(25));
    assert_eq!(rt.store.opacity(text), 0.75, "and the move's clock");

    // A rebuild carries each kept node's move, a region's included.
    let mut rt = Rt::reload(&carried("", "1.0f32"));
    rt.click(5.0, 5.0);
    rt.store.tick_transitions(ms(50));
    let root = rt.root.expect("mounted");
    let [text, _] = rt.children(root)[..] else {
        panic!("the text and the region column");
    };
    assert_eq!(rt.store.opacity(text), 0.5, "mid-move before the edit");
    rt.edit(&carried("Row { width: 5dp; height: 5dp; }", "1.0f32"));
    let root = rt.root.expect("mounted");
    let [_, text, column] = rt.children(root)[..] else {
        panic!("a new row, the text and the region column");
    };
    let [shown] = rt.children(column)[..] else {
        panic!("the region's text");
    };
    assert_eq!(
        rt.store.opacity(text),
        0.5,
        "the rebuilt node shows the move"
    );
    assert_eq!(rt.store.opacity(shown), 0.5, "so does the region's");
    rt.store.tick_transitions(ms(25));
    assert_eq!(rt.store.opacity(text), 0.75);
    assert_eq!(rt.store.opacity(shown), 0.75);

    // A new target turns the carried move from where it is.
    rt.edit(&carried("", "0.25f32"));
    let root = rt.root.expect("mounted");
    let [text, _] = rt.children(root)[..] else {
        panic!("the text and the region column");
    };
    assert_eq!(rt.store.opacity(text), 0.75);
    rt.store.tick_transitions(ms(50));
    assert_eq!(rt.store.opacity(text), 0.5, "from 0.75 halfway to 0.25");
    rt.store.tick_transitions(ms(50));
    assert_eq!(rt.store.opacity(text), 0.25);
    assert!(!rt.store.is_transitioning(), "both arrived");
}
