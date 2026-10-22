use super::*;
use viso_ui::{
    BindingTable, EffectStore, Modifiers, PointerButtons, PointerEvent, PointerPhase, Role,
    SemanticProjector, StateStore, TextEdits, VirtualLists, run_structure_hooks,
};

struct Reactive {
    store: NodeStore,
    states: StateStore,
    bindings: BindingTable,
    lists: VirtualLists,
    text_edits: TextEdits,
    projectors: SemanticProjector,
    effects: EffectStore,
}

impl Reactive {
    fn new() -> Self {
        Reactive {
            store: NodeStore::new(),
            states: StateStore::new(),
            bindings: BindingTable::new(),
            lists: VirtualLists::new(),
            text_edits: TextEdits::new(),
            projectors: SemanticProjector::new(),
            effects: EffectStore::default(),
        }
    }

    fn build(&mut self, doc: Markdown) -> NodeId {
        let mut cx = BuildCx::with_reactive(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.lists,
            &mut self.text_edits,
            &mut self.projectors,
        );
        doc.build(&mut cx);
        cx.root().expect("a document declares a root node")
    }

    /// Run the hooks of `text`'s documents, as the frame after a write does.
    fn settle(&mut self, text: &MarkdownText) {
        let cells = text.0.cells.borrow().clone();
        run_structure_hooks(
            &mut self.store,
            &mut self.states,
            &mut self.bindings,
            &mut self.effects,
            &mut self.lists,
            &cells,
        );
    }

    fn blocks(&self, root: NodeId) -> Vec<NodeId> {
        self.store.arena().children(root).collect()
    }

    fn text(&self, node: NodeId) -> &str {
        &self.store.text_request(node).expect("a text").text
    }
}

#[test]
fn each_block_is_one_node_with_its_semantics() {
    let mut rx = Reactive::new();
    let root = rx.build(markdown("# Title\n\nSome *words*.\n\n- a\n- b\n"));
    let blocks = rx.blocks(root);
    assert_eq!(blocks.len(), 3);
    let heading = rx.store.arena().children(blocks[0]).next().unwrap();
    let semantics = rx.store.semantics(heading).unwrap();
    assert_eq!(semantics.role, Role::Heading);
    assert_eq!(semantics.heading_level, Some(1));
    assert_eq!(rx.text(heading), "Title");
    assert_eq!(rx.text(blocks[1]), "Some words.");
    assert_eq!(rx.store.semantics(blocks[2]).unwrap().role, Role::List);
    let items: Vec<NodeId> = rx.store.arena().children(blocks[2]).collect();
    assert_eq!(items.len(), 2);
    assert_eq!(rx.store.semantics(items[0]).unwrap().role, Role::ListItem);
}

#[test]
fn an_append_rebuilds_only_the_last_block_and_adds_the_new_ones() {
    let mut rx = Reactive::new();
    let text = MarkdownText::new("One.\n\nTwo");
    let root = rx.build(markdown(&text));
    let before = rx.blocks(root);
    text.push_str(&mut rx.states, " more");
    rx.settle(&text);
    let after = rx.blocks(root);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], before[0], "the first block stays");
    assert_ne!(after[1], before[1], "the last one is rebuilt");
    assert!(!rx.store.arena().is_live(before[1]));
    assert_eq!(rx.text(after[1]), "Two more");

    text.push_str(&mut rx.states, "\n\n- item");
    rx.settle(&text);
    let grown = rx.blocks(root);
    assert_eq!(grown.len(), 3);
    assert_eq!(grown[0], after[0]);
    assert_eq!(rx.store.semantics(grown[2]).unwrap().role, Role::List);
}

#[test]
fn an_edit_rebuilds_the_changed_blocks_in_their_place() {
    let mut rx = Reactive::new();
    let text = MarkdownText::new("One.\n\nTwo.\n\nThree.");
    let root = rx.build(markdown(&text));
    let before = rx.blocks(root);
    text.set(&mut rx.states, "One.\n\nChanged.\n\nAdded.\n\nThree.");
    rx.settle(&text);
    let after = rx.blocks(root);
    assert_eq!(after.len(), 4);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[3], before[2]);
    let texts: Vec<&str> = after.iter().map(|&n| rx.text(n)).collect();
    assert_eq!(texts, ["One.", "Changed.", "Added.", "Three."]);

    text.set(&mut rx.states, "Three.");
    rx.settle(&text);
    assert_eq!(rx.blocks(root), [before[2]], "the removed blocks go");
}

#[test]
fn a_new_reference_definition_rebuilds_the_blocks_it_reaches() {
    let mut rx = Reactive::new();
    let text = MarkdownText::new("[docs]\n\nTail.");
    let root = rx.build(markdown(&text));
    let before = rx.blocks(root);
    text.push_str(&mut rx.states, "\n\n[docs]: /docs");
    rx.settle(&text);
    let after = rx.blocks(root);
    assert_eq!(after.len(), 2, "a definition draws nothing");
    assert_ne!(after[0], before[0]);
    let spans = &rx.store.text_request(after[0]).unwrap().spans;
    assert_eq!(spans[0].style.link.as_deref(), Some("/docs"));
}

#[test]
fn a_task_box_toggles_in_place_and_writes_its_source() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    let mut rx = Reactive::new();
    let text = MarkdownText::new("- [ ] one\n- [x] two\n");
    let root = rx.build(markdown(&text).on_task(move |_, at, checked| {
        sink.borrow_mut().push((at, checked));
    }));
    let list = rx.blocks(root)[0];
    let item = rx.store.arena().children(list).next().unwrap();
    let check = rx.store.arena().children(item).next().unwrap();
    assert_eq!(
        rx.store.check_mark(check).map(|m| m.checked),
        Some(false),
        "unchecked at first"
    );
    assert_eq!(rx.store.semantics(check).unwrap().role, Role::CheckBox);
    assert!(rx.store.focusable(check));

    let up = PointerEvent {
        x: 0.0,
        y: 0.0,
        phase: PointerPhase::Up,
        buttons: PointerButtons::PRIMARY,
        modifiers: Modifiers::default(),
    };
    let mut handler = rx.store.take_handler(check).expect("a pointer handler");
    {
        let mut cx = EventCx::__new_pointer(&mut rx.states, &rx.bindings, &up);
        handler(&mut cx);
    }
    rx.store.restore_handler(check, handler);
    rx.settle(&text);
    assert_eq!(&*text.text(), "- [x] one\n- [x] two\n");
    assert_eq!(*seen.borrow(), [(0, true)]);
    assert_eq!(rx.blocks(root), [list], "the list is not rebuilt");
    assert_eq!(rx.store.check_mark(check).map(|m| m.checked), Some(true));
    assert_eq!(
        rx.store.semantic_state(check),
        Some(SemanticState::checked(true))
    );
}

#[test]
fn a_read_only_task_box_takes_nothing() {
    let mut rx = Reactive::new();
    let root = rx.build(markdown("- [x] done"));
    let list = rx.blocks(root)[0];
    let item = rx.store.arena().children(list).next().unwrap();
    let check = rx.store.arena().children(item).next().unwrap();
    assert_eq!(rx.store.check_mark(check).map(|m| m.checked), Some(true));
    assert!(!rx.store.focusable(check));
    assert!(!rx.store.has_handler(check));
}

#[test]
fn a_selectable_block_a_write_brings_is_a_seeded_field() {
    let mut rx = Reactive::new();
    let text = MarkdownText::new("One.");
    let root = rx.build(markdown(&text).selectable());
    assert!(rx.store.field_marks(rx.blocks(root)[0]).is_some());
    text.push_str(&mut rx.states, "\n\nTwo.");
    rx.settle(&text);
    let two = rx.blocks(root)[1];
    assert!(rx.store.field_marks(two).is_some());
    assert!(rx.store.has_text_seeds(), "its buffer waits for reconcile");
}

#[test]
fn a_document_freed_ignores_its_source() {
    let mut rx = Reactive::new();
    let text = MarkdownText::new("One.");
    let root = rx.build(markdown(&text));
    rx.store.free_tree(root, &mut rx.effects, &mut Vec::new());
    text.push_str(&mut rx.states, "\n\nTwo.");
    rx.settle(&text);
    assert!(!rx.store.arena().is_live(root));
}
