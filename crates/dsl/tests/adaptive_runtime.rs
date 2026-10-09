//! The adaptive environment at runtime, under hot reload and the release
//! package: a region on `env.size_class` switches arms when the window's class
//! changes, a handler reads the environment as of the last settle, an
//! instance a region mounts reads its own `env` fields, avoiding regions pad
//! what the system covers of them, a view lays out around the display
//! features, a preserved branch keeps its focus, scroll and text, and one view
//! adapts across the devices and system states it meets.

use std::cell::RefCell;
use std::rc::Rc;

use viso_dsl::aot::build_view_package;
use viso_dsl::frontend::Origin;
use viso_dsl::hotreload::{CandidatePlan, LiveRuntime, hot_reload_view};
use viso_ui::Rect;
use viso_ui::adaptive::Environment;
use viso_ui::layout::{Inset, LayoutInput, LayoutTree};
use viso_ui::virtual_list::VirtualLists;
use viso_ui::{
    BindingTable, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent, PointerPhase,
    PointerRouter, SemanticProjector, StateStore, TextEdits, run_structure_hooks,
};
use viso_view::{Value, ViewHost, load_view};

const SOURCE: &str = r#"
component Adaptive {
    state taps = 0;
    state tag = "";
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text {
                    width: 20dp;
                    height: 20dp;
                    on click {
                        if env.text_scale > 1.5 { taps += 1; }
                        tag = env.locale.tag;
                    }
                }
            }
            Column {
                width: 400dp;
                height: 60dp;
                if env.size_class == SizeClass::Compact {
                    Text { width: 10dp; height: 10dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
        }
    }
}
"#;

fn origin() -> Origin {
    Origin {
        package: "app".into(),
        module: vec!["adaptive".into()],
        language: None,
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
    /// The view mounted by a hot reload.
    fn reloaded(source: &str) -> Self {
        let mut rt = Rt::default();
        rt.reload(source);
        rt
    }

    /// The view loaded from its release package.
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

    fn reload(&mut self, source: &str) {
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
        let done = hot_reload_view(&mut live, &self.last_good, source, &origin()).expect("reloads");
        self.root = live.root;
        self.last_good = done.candidate;
        self.layout();
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

    /// A primary click at `(x, y)`, then the frame's flush.
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
        self.flush();
    }

    /// Changes the environment, settles it against the last layout, then runs
    /// the frame's flush.
    fn update_env(&mut self, change: impl FnOnce(&mut Environment)) {
        self.states.update_env(change);
        self.states.settle_env(&self.store);
        self.flush();
    }

    /// The frame's state flush, structure hooks and layout.
    fn flush(&mut self) {
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

    /// The children of the root's `index`th child.
    fn region(&self, index: usize) -> Vec<NodeId> {
        let root = self.root.expect("mounted");
        self.children(self.children(root)[index])
    }

    fn state(&self, name: &str) -> Option<Value> {
        let host = self.view.as_ref()?.borrow();
        let slot = host.state_slot(name)?;
        host.state(slot).cloned()
    }
}

fn targets() -> [Rt; 2] {
    [Rt::reloaded(SOURCE), Rt::packaged(SOURCE)]
}

const REGIONAL: &str = r#"
component Panel {
    view {
        Column {
            width: 100dp;
            height: 60dp;
            Row {
                width: 100dp;
                height: 20dp;
                if env.size_class == SizeClass::Compact {
                    Text { width: 10dp; height: 10dp; }
                } else {
                    Text { width: 10dp; height: 10dp; }
                    Text { width: 10dp; height: 10dp; }
                }
            }
            Row {
                width: 100dp;
                height: 20dp;
                if env.reduced_motion {
                    Text { width: 10dp; height: 10dp; }
                }
            }
        }
    }
}

export component Host {
    state shown = false;
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Row {
                width: 400dp;
                height: 20dp;
                Text { width: 20dp; height: 20dp; on click { shown = !shown; } }
            }
            Column {
                width: 400dp;
                height: 100dp;
                if shown {
                    Panel {}
                }
            }
        }
    }
}
"#;

impl Rt {
    /// The children of each row of the mounted `Panel`.
    fn panel(&self) -> Option<[usize; 2]> {
        let root = *self.region(1).first()?;
        let rows = self.children(root);
        Some([self.children(rows[0]).len(), self.children(rows[1]).len()])
    }
}

#[test]
fn a_region_on_the_size_class_switches_with_the_window() {
    for mut rt in targets() {
        assert_eq!(rt.region(1).len(), 1, "a zero-width window is compact");
        rt.update_env(|e| e.window.width = 1000.0);
        assert_eq!(rt.region(1).len(), 2, "expanded");
        let expanded = rt.region(1);
        rt.update_env(|e| e.window.width = 700.0);
        assert_eq!(rt.region(1), expanded, "medium keeps the arm");
        rt.update_env(|e| e.window.width = 320.0);
        assert_eq!(rt.region(1).len(), 1, "compact again");
    }
}

#[test]
fn a_handler_reads_the_settled_environment() {
    for mut rt in targets() {
        rt.click(5.0, 5.0);
        assert_eq!(rt.state("taps"), Some(Value::Int(0)));
        rt.update_env(|e| {
            e.text_scale = 2.0;
            e.locale = "fr-CA".into();
        });
        rt.click(5.0, 5.0);
        assert_eq!(rt.state("taps"), Some(Value::Int(1)));
        assert_eq!(rt.state("tag"), Some(Value::str("fr-CA")));
    }
}

#[test]
fn an_instance_a_region_mounts_reads_its_own_env() {
    for mut rt in [Rt::reloaded(REGIONAL), Rt::packaged(REGIONAL)] {
        assert_eq!(rt.panel(), None);
        rt.update_env(|e| e.window.width = 1000.0);
        rt.click(5.0, 5.0);
        assert_eq!(rt.panel(), Some([2, 0]), "mounted expanded, motion on");
        rt.update_env(|e| e.window.width = 320.0);
        assert_eq!(rt.panel(), Some([1, 0]), "the anchored class moved");
        rt.update_env(|e| e.reduced_motion = true);
        assert_eq!(rt.panel(), Some([1, 1]), "a window-wide field moved");
        rt.click(5.0, 5.0);
        assert_eq!(rt.panel(), None);
        rt.update_env(|e| e.window.width = 1000.0);
        rt.click(5.0, 5.0);
        assert_eq!(
            rt.panel(),
            Some([2, 1]),
            "a fresh mount reads the current env"
        );
        assert_eq!(
            rt.states.env().anchor_count(),
            1,
            "the first mount's anchor is released"
        );
    }
}

const SCOPED: &str = r#"
component Panel {
    view {
        Column {
            width: 100dp;
            height: 20dp;
            if env.size_class == SizeClass::Compact {
                Text { width: 10dp; height: 10dp; }
            } else {
                Text { width: 10dp; height: 10dp; }
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}

export component Scoped {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            Panel {}
            Row {
                width: 400dp;
                height: 40dp;
                Column { width: 300dp; height: 40dp; AdaptiveScope { Panel {} } }
            }
            Row {
                width: 400dp;
                height: 40dp;
                Column { width: 300dp; height: 40dp; AdaptiveScope { basis: 900dp; Panel {} } }
            }
        }
    }
}
"#;

impl Rt {
    /// The nodes of the window-classed panel and of the two scoped ones.
    fn scoped(&self) -> [Vec<NodeId>; 3] {
        let root = self.children(self.root.expect("mounted"));
        let scoped = |row: NodeId| {
            let column = self.children(row)[0];
            let scope = self.children(column)[0];
            self.children(self.children(scope)[0])
        };
        [self.children(root[0]), scoped(root[1]), scoped(root[2])]
    }
}

#[test]
fn a_panel_under_an_adaptive_scope_reads_the_scope_class() {
    for mut rt in [Rt::reloaded(SCOPED), Rt::packaged(SCOPED)] {
        let counts = |rt: &Rt| rt.scoped().map(|nodes| nodes.len());
        rt.update_env(|_| {});
        assert_eq!(counts(&rt), [1, 1, 2], "the basis classifies expanded");
        rt.update_env(|e| e.window.width = 1000.0);
        let scoped = rt.scoped();
        assert_eq!(counts(&rt), [2, 1, 2], "the scope classifies its 300dp");
        rt.update_env(|e| e.window.width = 320.0);
        assert_eq!(counts(&rt), [1, 1, 2]);
        assert_eq!(
            rt.scoped()[1..],
            scoped[1..],
            "a kept class rebuilds nothing"
        );
    }
}

#[test]
fn a_non_constant_basis_is_reported() {
    let source =
        "export component A { view { AdaptiveScope { basis: 10px; Row { width: 10dp; } } } }";
    let compiled = viso_dsl::frontend::compile_file(source, &origin());
    let codes: Vec<String> = compiled.errors().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["E3711"]);
}

const AVOIDING: &str = r#"
export component Avoiding {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            SafeArea {
                width: 400dp;
                height: 100dp;
                Text { width: 10dp; height: 10dp; }
            }
            KeyboardAvoiding {
                width: 400dp;
                height: 200dp;
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}
"#;

impl Rt {
    /// Changes the environment, then pads every avoiding region against the
    /// layout until it holds still.
    fn avoid(&mut self, change: impl FnOnce(&mut Environment)) {
        self.states.update_env(change);
        while self.states.pad_avoiding(&mut self.store) {
            self.layout();
        }
        self.states.settle_env(&self.store);
        self.flush();
    }

    /// The padding of each of the root's children.
    fn paddings(&self) -> Vec<Inset> {
        let root = self.root.expect("mounted");
        self.children(root)
            .into_iter()
            .map(|node| match self.store.input(node.index()) {
                LayoutInput::Flex { padding, .. } => padding,
                _ => Inset::default(),
            })
            .collect()
    }
}

fn inset(top: f32, bottom: f32) -> Inset {
    Inset {
        top,
        bottom,
        ..Inset::default()
    }
}

#[test]
fn avoiding_regions_pad_what_the_system_covers_of_them() {
    for mut rt in [Rt::reloaded(AVOIDING), Rt::packaged(AVOIDING)] {
        rt.avoid(|_| {});
        assert_eq!(rt.paddings(), [Inset::default(), Inset::default()]);
        rt.avoid(|e| {
            e.safe_area = inset(30.0, 20.0);
            e.keyboard_inset = 150.0;
        });
        assert_eq!(
            rt.paddings(),
            [inset(30.0, 0.0), inset(0.0, 150.0)],
            "the top bar covers the safe area, the keyboard the lower region"
        );
        let text = rt.children(rt.children(rt.root.expect("mounted"))[0])[0];
        assert_eq!(
            rt.store.bounds(text).y,
            30.0,
            "content moves out from under"
        );
        rt.avoid(|e| e.keyboard_inset = 0.0);
        assert_eq!(rt.paddings(), [inset(30.0, 0.0), Inset::default()]);
    }
}

#[test]
fn a_padded_avoiding_region_is_reported() {
    for widget in ["SafeArea", "KeyboardAvoiding"] {
        let source = format!(
            "export component A {{ view {{ {widget} {{ padding: 4dp; Row {{ width: 10dp; }} }} }} }}"
        );
        let compiled = viso_dsl::frontend::compile_file(&source, &origin());
        let codes: Vec<String> = compiled.errors().map(|d| d.code.to_string()).collect();
        assert_eq!(codes, ["E3711"], "{widget}");
    }
}

const FOLDED: &str = r#"
export component Folded {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            for feature in env.display_features key match feature {
                DisplayFeature::Hinge { .. } => 0,
                DisplayFeature::Fold { .. } => 1,
                DisplayFeature::Cutout { .. } => 2,
            } {
                match feature {
                    DisplayFeature::Hinge { .. } => { Text { width: 10dp; height: 10dp; } },
                    _ => { },
                }
            }
        }
    }
}
"#;

#[test]
fn a_view_lays_out_around_the_display_features() {
    use viso_ui::adaptive::{DisplayFeature, DisplayFeatureKind};
    let feature = |kind| DisplayFeature {
        kind,
        bounds: Rect {
            x: 196.0,
            y: 0.0,
            w: 8.0,
            h: 300.0,
        },
    };
    for mut rt in [Rt::reloaded(FOLDED), Rt::packaged(FOLDED)] {
        rt.update_env(|_| {});
        let root = rt.root.expect("mounted");
        assert_eq!(rt.children(root).len(), 0, "a flat screen");
        rt.update_env(|e| {
            e.display_features = vec![
                feature(DisplayFeatureKind::Hinge),
                feature(DisplayFeatureKind::Cutout),
            ]
        });
        assert_eq!(rt.children(root).len(), 1, "one hinge");
        rt.update_env(|e| e.display_features.clear());
        assert_eq!(rt.children(root).len(), 0, "unfolded");
    }
}

const PRESERVING: &str = r#"
export component Preserving {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            if env.size_class == SizeClass::Compact preserve "compact" {
                Scroll {
                    width: 100dp;
                    height: 50dp;
                    Column { width: 100dp; height: 500dp; }
                }
                TextInput { width: 100dp; height: 20dp; }
            } else {
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}
"#;

#[test]
fn a_preserved_branch_keeps_its_focus_scroll_and_text() {
    use viso_ui::{Buffer, Vec2};
    for mut rt in [Rt::reloaded(PRESERVING), Rt::packaged(PRESERVING)] {
        rt.update_env(|_| {});
        let root = rt.root.expect("mounted");
        let [scroll, input] = rt.children(root)[..] else {
            panic!("the compact branch is shown");
        };
        rt.store.set_scroll(scroll, Vec2 { x: 0.0, y: 40.0 });
        assert_eq!(rt.store.scroll(scroll).y, 40.0);
        rt.text_edits
            .register(input, Box::new(Buffer::with_text("draft")));
        assert!(viso_ui::focus_node(&mut rt.store, input));

        rt.update_env(|e| e.window.width = 1000.0);
        assert_eq!(rt.children(root).len(), 1, "the wide branch is shown");
        assert_eq!(rt.store.focused(), None, "keys never reach a hidden node");

        rt.update_env(|e| e.window.width = 400.0);
        assert_eq!(rt.children(root), [scroll, input], "the same nodes return");
        assert_eq!(rt.store.scroll(scroll).y, 40.0);
        assert_eq!(
            rt.text_edits.get(input).map(|b| &b.text),
            Some(&Buffer::with_text("draft").text)
        );
        assert_eq!(rt.store.focused(), Some(input), "the focus comes back");
    }
}

const MATRIX: &str = r#"
export component Matrix {
    view {
        Column {
            width: 400dp;
            height: 300dp;
            SafeArea {
                width: 400dp;
                height: 100dp;
                Row {
                    width: 400dp;
                    height: 10dp;
                    match env.size_class {
                        SizeClass::Compact => { Text { width: 10dp; height: 10dp; } },
                        SizeClass::Medium => {
                            Text { width: 10dp; height: 10dp; }
                            Text { width: 10dp; height: 10dp; }
                        },
                        SizeClass::Expanded => {
                            Text { width: 10dp; height: 10dp; }
                            Text { width: 10dp; height: 10dp; }
                            Text { width: 10dp; height: 10dp; }
                        },
                    }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    if env.orientation == Orientation::Landscape { Text { width: 10dp; height: 10dp; } }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    if env.keyboard_inset.height > 0dp { Text { width: 10dp; height: 10dp; } }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    if env.safe_area.top > 0dp || env.safe_area.left > 0dp {
                        Text { width: 10dp; height: 10dp; }
                    }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    for feature in env.display_features key match feature {
                        DisplayFeature::Hinge { .. } => 0,
                        DisplayFeature::Fold { .. } => 1,
                        DisplayFeature::Cutout { .. } => 2,
                    } {
                        Text { width: 10dp; height: 10dp; }
                    }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    if env.text_scale > 1.5 { Text { width: 10dp; height: 10dp; } }
                }
                Row {
                    width: 400dp;
                    height: 10dp;
                    if env.input.touch_available {
                        Text { width: 10dp; height: 10dp; }
                    } else if env.input.hover_available {
                        Text { width: 10dp; height: 10dp; }
                        Text { width: 10dp; height: 10dp; }
                    }
                }
            }
            KeyboardAvoiding {
                width: 400dp;
                height: 200dp;
                Text { width: 10dp; height: 10dp; }
            }
        }
    }
}
"#;

/// A device and the state its system is in.
struct Scenario {
    name: &'static str,
    env: Environment,
    /// The children of each row: size class (1 compact, 2 medium, 3
    /// expanded), landscape, keyboard shown, safe area, display features,
    /// large text, and input (1 touch, 2 mouse and keyboard).
    rows: [usize; 7],
    /// The padding of the safe-area and the keyboard-avoiding region.
    paddings: [Inset; 2],
}

fn scenarios() -> Vec<Scenario> {
    use viso_ui::adaptive::{
        DisplayFeature, DisplayFeatureKind, InputCapabilities, PointerPrecision, WindowMetrics,
    };
    let window = |width, height| WindowMetrics {
        width,
        height,
        scale_factor: 2.0,
    };
    let touch = InputCapabilities {
        primary_pointer_precision: PointerPrecision::Coarse,
        hover_available: false,
        keyboard_available: false,
        touch_available: true,
        ..InputCapabilities::default()
    };
    let phone = Environment {
        window: window(390.0, 844.0),
        safe_area: inset(47.0, 34.0),
        input: touch,
        ..Environment::default()
    };
    let tablet = Environment {
        window: window(1024.0, 1366.0),
        safe_area: inset(24.0, 20.0),
        input: touch,
        ..Environment::default()
    };
    let desktop = Environment {
        window: window(1600.0, 900.0),
        ..Environment::default()
    };
    let none = Inset::default();
    vec![
        Scenario {
            name: "phone portrait",
            env: phone.clone(),
            rows: [1, 0, 0, 1, 0, 0, 1],
            paddings: [inset(47.0, 0.0), none],
        },
        Scenario {
            name: "phone landscape",
            env: Environment {
                window: window(844.0, 390.0),
                safe_area: Inset {
                    left: 47.0,
                    right: 47.0,
                    bottom: 21.0,
                    top: 0.0,
                },
                ..phone.clone()
            },
            // 750dp clear of the safe area: medium.
            rows: [2, 1, 0, 1, 0, 0, 1],
            paddings: [
                Inset {
                    left: 47.0,
                    right: 47.0,
                    ..none
                },
                none,
            ],
        },
        Scenario {
            name: "keyboard shown",
            env: Environment {
                keyboard_inset: 150.0,
                ..phone.clone()
            },
            rows: [1, 0, 1, 1, 0, 0, 1],
            paddings: [inset(47.0, 0.0), inset(0.0, 150.0)],
        },
        Scenario {
            name: "keyboard over the whole region",
            env: Environment {
                keyboard_inset: 336.0,
                ..phone.clone()
            },
            rows: [1, 0, 1, 1, 0, 0, 1],
            paddings: [inset(47.0, 0.0), inset(0.0, 200.0)],
        },
        Scenario {
            name: "keyboard hidden",
            env: phone.clone(),
            rows: [1, 0, 0, 1, 0, 0, 1],
            paddings: [inset(47.0, 0.0), none],
        },
        Scenario {
            name: "safe area change",
            env: Environment {
                safe_area: inset(59.0, 34.0),
                ..phone.clone()
            },
            rows: [1, 0, 0, 1, 0, 0, 1],
            paddings: [inset(59.0, 0.0), none],
        },
        Scenario {
            name: "tablet full screen",
            env: tablet.clone(),
            rows: [3, 0, 0, 1, 0, 0, 1],
            paddings: [inset(24.0, 0.0), none],
        },
        Scenario {
            name: "tablet split view",
            env: Environment {
                window: window(507.0, 1366.0),
                ..tablet.clone()
            },
            rows: [1, 0, 0, 1, 0, 0, 1],
            paddings: [inset(24.0, 0.0), none],
        },
        Scenario {
            name: "desktop narrow",
            env: Environment {
                window: window(700.0, 800.0),
                ..desktop.clone()
            },
            rows: [2, 0, 0, 0, 0, 0, 2],
            paddings: [none, none],
        },
        Scenario {
            name: "desktop wide",
            env: desktop.clone(),
            rows: [3, 1, 0, 0, 0, 0, 2],
            paddings: [none, none],
        },
        Scenario {
            name: "folded on its hinge",
            env: Environment {
                window: window(800.0, 600.0),
                display_features: vec![DisplayFeature {
                    kind: DisplayFeatureKind::Hinge,
                    bounds: Rect {
                        x: 396.0,
                        y: 0.0,
                        w: 8.0,
                        h: 600.0,
                    },
                }],
                ..desktop.clone()
            },
            rows: [2, 1, 0, 0, 1, 0, 2],
            paddings: [none, none],
        },
        Scenario {
            name: "large text",
            env: Environment {
                text_scale: 2.0,
                ..desktop.clone()
            },
            rows: [3, 1, 0, 0, 0, 1, 2],
            paddings: [none, none],
        },
        Scenario {
            name: "touch on the desktop",
            env: Environment {
                input: InputCapabilities {
                    touch_available: true,
                    ..desktop.input
                },
                ..desktop.clone()
            },
            rows: [3, 1, 0, 0, 0, 0, 1],
            paddings: [none, none],
        },
    ]
}

#[test]
fn one_view_adapts_to_every_device_and_system_state() {
    for mut rt in [Rt::reloaded(MATRIX), Rt::packaged(MATRIX)] {
        let root = rt.root.expect("mounted");
        let safe = rt.children(root)[0];
        for scenario in scenarios() {
            let env = scenario.env.clone();
            rt.avoid(|e| *e = env);
            let rows: Vec<usize> = rt
                .children(safe)
                .into_iter()
                .map(|row| rt.children(row).len())
                .collect();
            assert_eq!(rows, scenario.rows, "{}: the branches", scenario.name);
            assert_eq!(
                rt.paddings(),
                scenario.paddings,
                "{}: the avoiding regions",
                scenario.name
            );
        }
    }
}
