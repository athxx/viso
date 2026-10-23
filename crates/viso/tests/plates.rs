//! A view's `Badge` and `Avatar`: a badge shows its count up to its most on
//! a pill and with no count is a dot, and an avatar shows its name's
//! initials on the color the name picks.

use viso::render::Rect;
use viso::ui::plate::{BADGE_FILL, avatar_fill};
use viso::ui::{
    BindingTable, BuildCx, CheckShape, EffectStore, NodeId, NodeStore, PointerButtons,
    PointerEvent, PointerPhase, PointerRouter, Role, SemanticProjector, StateStore, TextEdits,
    VirtualLists, run_structure_hooks,
};

viso::component! {
    Inbox {
        state unread: Option<I32> = Option::Some(120);
        view {
            Column {
                width: 200dp;
                height: 200dp;
                Text {
                    width: 100dp;
                    height: 20dp;
                    on click { unread = Option::Some(3); }
                }
                Badge { count: unread; }
                Badge { label: "New"; }
                Avatar { name: "Ada Lovelace"; }
                Avatar { name: "Grace"; width: 64dp; height: 64dp; tint: Option::Some(#ff0000); }
            }
        }
    }
}

const SURFACE: Rect = Rect {
    x: 0.0,
    y: 0.0,
    w: 400.0,
    h: 300.0,
};

struct Mounted {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    root: NodeId,
}

impl Mounted {
    fn new() -> Self {
        let mut store = NodeStore::new();
        let mut states = StateStore::new();
        let mut bindings = BindingTable::new();
        let mut lists = VirtualLists::new();
        let mut text_edits = TextEdits::new();
        let mut projectors = SemanticProjector::new();
        let root = {
            let mut cx = BuildCx::with_reactive(
                &mut store,
                &mut states,
                &mut bindings,
                &mut lists,
                &mut text_edits,
                &mut projectors,
            );
            Inbox::build(&mut cx).1.id()
        };
        store.layout(root, SURFACE, &mut Vec::new());
        Self {
            store,
            states,
            bindings,
            root,
        }
    }

    fn child(&self, n: usize) -> NodeId {
        self.store.arena().children(self.root).nth(n).unwrap()
    }

    fn text(&self, n: usize) -> String {
        self.store.text_request(self.child(n)).unwrap().text.clone()
    }

    fn click(&mut self, x: f32, y: f32) {
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
                self.root,
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
            &mut EffectStore::new(),
            &mut VirtualLists::new(),
            &changed,
        );
    }
}

#[test]
fn a_badge_shows_its_count_up_to_its_most_on_a_pill_or_is_a_dot() {
    let mut inbox = Mounted::new();
    let badge = inbox.child(1);
    assert_eq!(inbox.text(1), "99+");
    assert_eq!(
        inbox.store.check_mark(badge).unwrap().shape,
        CheckShape::Plate
    );
    assert_eq!(inbox.store.style(badge).fill, BADGE_FILL);
    let semantics = inbox.store.semantics(badge).unwrap();
    assert_eq!(semantics.role, Role::Label);
    assert_eq!(semantics.label.as_deref(), Some("99+"));

    let dot = inbox.child(2);
    assert_eq!(inbox.text(2), "");
    assert_eq!(
        inbox.store.semantics(dot).unwrap().label.as_deref(),
        Some("New")
    );

    inbox.click(10.0, 10.0);
    assert_eq!(inbox.text(1), "3");
    assert_eq!(
        inbox.store.semantics(badge).unwrap().label.as_deref(),
        Some("3")
    );
}

#[test]
fn an_avatar_shows_its_name_s_initials_on_the_color_it_picks() {
    let inbox = Mounted::new();
    let ada = inbox.child(3);
    assert_eq!(inbox.text(3), "AL");
    assert_eq!(inbox.store.bounds(ada).w, 40.0, "a 40dp circle");
    assert_eq!(inbox.store.style(ada).fill, avatar_fill("Ada Lovelace"));
    assert_eq!(inbox.store.style(ada).radius_pct, 0.5);
    let semantics = inbox.store.semantics(ada).unwrap();
    assert_eq!(semantics.role, Role::Image);
    assert_eq!(semantics.label.as_deref(), Some("Ada Lovelace"));

    let grace = inbox.child(4);
    assert_eq!(inbox.text(4), "G");
    assert_eq!(inbox.store.bounds(grace).w, 64.0);
    assert!(inbox.store.style(grace).fill.r > 0.9, "its tint");
}
