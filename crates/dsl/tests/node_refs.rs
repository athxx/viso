//! A component's named nodes as a `NodeRef`: its handlers focus, blur,
//! scroll and animate them once the view settles, each component instance of
//! a `for` reaches its own, and a named node outside handlers and actions is
//! `E2001`.

use std::collections::HashMap;
use std::time::Duration;
use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_ui::virtual_list::VirtualLists;

use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, Rect, StateStore, Vec2, deliver_notices, run_structure_hooks,
};
use viso_view::load_view;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["nodes".into()],
        language: None,
        ..Default::default()
    }
}

struct Page {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    effects: EffectStore,
    root: NodeId,
    /// The text each node shows, as last requested.
    texts: HashMap<NodeId, String>,
}

impl Page {
    fn load(source: &str) -> Self {
        let blob = build_view_package(source, &origin()).expect("packages");
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let view =
            load_view(&blob, &mut store, &mut states, &mut bindings, &mut lists).expect("loads");
        let mut page = Self {
            store,
            states,
            bindings,
            effects: EffectStore::default(),
            root: view.root.expect("a root"),
            texts: HashMap::new(),
        };
        page.layout();
        page
    }

    fn layout(&mut self) {
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 300.0,
        };
        self.store.layout(self.root, surface, &mut Vec::new());
        let mut requests = Vec::new();
        self.store.take_text_requests(&mut requests);
        for (id, request) in requests {
            self.texts.insert(id, request.text);
        }
    }

    /// A frame `ms` on: the moves tick, the view lays out and hears its
    /// notices, and what they wrote flushes.
    fn frame(&mut self, ms: u64) {
        self.store.tick_transitions(Duration::from_millis(ms));
        self.layout();
        deliver_notices(&mut self.store, &VirtualLists::new(), &mut self.states);
        self.settle();
        self.layout();
    }

    fn settle(&mut self) {
        let mut changed = Vec::new();
        self.states.take_pending(&mut changed);
        self.store
            .flush_state_transactions(&changed, &self.bindings);
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut VirtualLists::new(),
            &changed,
        );
    }

    fn text(&self, node: NodeId) -> &str {
        self.texts.get(&node).map_or("", String::as_str)
    }

    /// A primary click in the middle of `node`, the state flush, and layout.
    fn click(&mut self, node: NodeId) {
        let rect = self.store.world(node);
        let mut chain = Vec::new();
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            let event = PointerEvent {
                x: rect.x + rect.w * 0.5,
                y: rect.y + rect.h * 0.5,
                phase,
                buttons: PointerButtons::PRIMARY,
                modifiers: Default::default(),
            };
            PointerRouter::route(
                &mut self.store,
                &mut self.states,
                &self.bindings,
                self.root,
                event,
                &mut chain,
            );
        }
        self.settle();
        self.layout();
    }

    fn child(&self, parent: NodeId, at: usize) -> NodeId {
        self.store
            .arena()
            .children(parent)
            .nth(at)
            .expect("a child")
    }
}

#[test]
fn a_handler_focuses_blurs_and_scrolls_its_named_nodes() {
    let mut page = Page::load(
        "component Form {
            state open = false;
            view {
                Column {
                    width: 200dp;
                    height: 300dp;
                    Button {
                        width: 100dp; height: 20dp; text: \"go\";
                        on click { field.focus(); list.scroll_to(Offset { x: 0dp, y: 30dp }); }
                    }
                    Button {
                        width: 100dp; height: 20dp; text: \"more\";
                        on click { list.scroll_by(Offset { x: 0dp, y: 15dp }); field.blur(); }
                    }
                    node field: TextInput { width: 100dp; height: 20dp; }
                    node list: Scroll {
                        width: 100dp; height: 50dp;
                        Text { width: 100dp; height: 400dp; text: \"tall\"; }
                    }
                }
            }
        }",
    );
    let go = page.child(page.root, 0);
    let more = page.child(page.root, 1);
    let field = page.child(page.root, 2);
    let list = page.child(page.root, 3);
    assert_eq!(page.store.focused(), None);

    page.click(go);
    assert_eq!(page.store.focused(), Some(field));
    assert_eq!(page.store.scroll(list), Vec2 { x: 0.0, y: 30.0 });

    page.click(more);
    assert_ne!(page.store.focused(), Some(field), "blurred");
    assert_eq!(page.store.scroll(list), Vec2 { x: 0.0, y: 45.0 });
}

#[test]
fn each_instance_of_a_for_reaches_its_own_named_node() {
    let mut page = Page::load(
        "component Entry {
            view {
                Column {
                    width: 100dp; height: 40dp;
                    Button { width: 100dp; height: 20dp; text: \"edit\"; on click { field.focus(); } }
                    node field: TextInput { width: 100dp; height: 20dp; }
                }
            }
        }
        export component Entries {
            state items = [1, 2, 3];
            view {
                Column {
                    width: 200dp; height: 300dp;
                    for item in items key item { Entry {} }
                }
            }
        }",
    );
    for at in [1, 0, 2] {
        let entry = page.child(page.root, at);
        let edit = page.child(entry, 0);
        let field = page.child(entry, 1);
        page.click(edit);
        assert_eq!(page.store.focused(), Some(field), "entry {at}");
    }
}

const SHAKE: &str = "component Shake {
    state shaking: Option<Animation> = Option::None;
    state ended = \"\";
    state running = false;
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button {
                width: 100dp; height: 20dp; text: \"shake\";
                on click {
                    let spec = Transition { duration: 100ms, easing: Easing::linear };
                    shaking = Option::Some(card.animate(Animate::translate(Offset { x: 8dp, y: 0dp }), spec));
                    running = match shaking { Option::Some(a) => a.is_running(), Option::None => false };
                }
            }
            Button {
                width: 100dp; height: 20dp; text: \"stop\";
                on click {
                    match shaking { Option::Some(a) => a.cancel(), Option::None => {} }
                    running = match shaking { Option::Some(a) => a.is_running(), Option::None => false };
                }
            }
            Button {
                width: 100dp; height: 20dp; text: \"check\";
                on click { running = match shaking { Option::Some(a) => a.is_running(), Option::None => false }; }
            }
            node card: Column {
                width: 50dp; height: 20dp;
                on animation_end(e) {
                    ended = match e.target {
                        Animate::translate(_) => \"translate {e.finished}\",
                        _ => \"other\",
                    };
                }
            }
            Text { width: 100dp; height: 20dp; text: ended; }
            Text { width: 100dp; height: 20dp; text: \"{running}\"; }
        }
    }
}";

#[test]
fn an_animation_moves_its_node_and_returns_it_and_its_end_reaches_the_node() {
    let mut page = Page::load(SHAKE);
    let [shake, stop, check, card, ended, running] =
        [0, 1, 2, 3, 4, 5].map(|at| page.child(page.root, at));
    page.click(shake);
    assert_eq!(page.text(running), "true", "running as the call returns");
    assert_eq!(page.store.translate(card).x, 0.0);
    page.frame(50);
    assert_eq!(page.store.translate(card).x, 4.0, "halfway there");
    page.frame(50);
    assert_eq!(
        page.store.translate(card).x,
        0.0,
        "back at its model value once it ends"
    );
    assert_eq!(page.text(ended), "translate true");
    page.click(check);
    assert_eq!(page.text(running), "false");

    page.click(shake);
    page.frame(30);
    assert!(page.store.translate(card).x > 0.0);
    page.click(stop);
    assert_eq!(page.text(running), "false", "cancelled at once");
    page.frame(16);
    assert_eq!(page.store.translate(card).x, 0.0);
    assert_eq!(page.text(ended), "translate false");
    assert!(!page.store.is_transitioning());
}

const PULSE: &str = "component Pulse {
    state pulse: Option<Animation> = Option::None;
    state ended = \"\";
    view {
        Column {
            width: 200dp;
            height: 300dp;
            Button {
                width: 100dp; height: 20dp; text: \"pulse\";
                on click {
                    let track = Keyframes {
                        stops: [
                            Keyframe { at: 0.5f32, value: Animate::scale(2.0f32), easing: Easing::linear },
                            Keyframe { at: 1.0f32, value: Animate::scale(1.0f32) },
                        ],
                        duration: 100ms,
                        repeat: Repeat::times(2),
                        direction: PlayDirection::alternate,
                    };
                    pulse = Option::Some(card.play(track));
                }
            }
            Button {
                width: 100dp; height: 20dp; text: \"forever\";
                on click {
                    let track = Keyframes {
                        stops: [Keyframe { at: 1.0f32, value: Animate::opacity(0.0f32) }],
                        duration: 100ms,
                        repeat: Repeat::forever,
                    };
                    pulse = Option::Some(card.play(track));
                }
            }
            Button {
                width: 100dp; height: 20dp; text: \"finish\";
                on click { match pulse { Option::Some(a) => a.finish(), Option::None => {} } }
            }
            node card: Column {
                width: 50dp; height: 20dp;
                on animation_end(e) {
                    ended = match e.target {
                        Animate::scale(s) => \"scale {s} {e.finished}\",
                        Animate::opacity(o) => \"opacity {o} {e.finished}\",
                        _ => \"other\",
                    };
                }
            }
            Text { width: 100dp; height: 20dp; text: ended; }
        }
    }
}";

#[test]
fn a_track_plays_its_stops_cycle_by_cycle_and_ends_at_its_last() {
    let mut page = Page::load(PULSE);
    let [pulse, forever, finish, card, ended] = [0, 1, 2, 3, 4].map(|at| page.child(page.root, at));
    page.click(pulse);
    page.frame(25);
    assert_eq!(page.store.turn(card).scale, 1.5);
    page.frame(25);
    assert_eq!(page.store.turn(card).scale, 2.0);
    // The second cycle runs backward: from 1 up to 2 and back.
    page.frame(75);
    assert_eq!(page.store.turn(card).scale, 1.5);
    page.frame(75);
    assert_eq!(page.store.turn(card).scale, 1.0);
    assert_eq!(page.text(ended), "scale 1 true");

    page.click(forever);
    page.frame(1000);
    page.frame(50);
    assert_eq!(page.store.opacity(card), 0.5, "it keeps cycling");
    page.click(finish);
    page.frame(16);
    assert_eq!(page.store.opacity(card), 1.0);
    assert_eq!(page.text(ended), "opacity 0 true");
    assert!(!page.store.is_transitioning());
}

#[test]
fn a_named_node_is_a_node_ref_only_in_handlers_and_actions() {
    let source = "component Form {
        view {
            Column {
                node field: TextInput { width: 100dp; height: 20dp; }
                Text { text: \"{field}\"; }
            }
        }
    }";
    let errors = build_view_package(source, &origin()).expect_err("rejected");
    assert!(
        errors.iter().any(|d| d.code == "E2001"),
        "{:?}",
        errors.iter().map(|d| &d.code).collect::<Vec<_>>()
    );
}
