//! A tally that outlives the app: `count` is `@persist`, so it loads before
//! the view mounts and every change is stored, made durable when the app goes
//! to the background or quits; `taps` starts over each run. `Viso.toml`
//! grants `storage.persist`; the facade keeps the values in the app's data
//! directory (`localStorage` on the web).

use viso::prelude::*;

struct Tally;

impl Application for Tally {
    fn new(_cx: &mut AppCx) -> Self {
        Tally
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("tally.vs")(cx);
    }
}

fn main() {
    viso::run::<Tally>();
}

#[cfg(test)]
mod tests {
    use viso::render::Rect;
    use viso::ui::{
        BindingTable, BuildCx, EffectStore, NodeId, NodeStore, PointerButtons, PointerEvent,
        PointerPhase, PointerRouter, SemanticProjector, StateStore, TextEdits, VirtualLists,
        run_structure_hooks,
    };
    use viso_view::persist::{MemoryStore, SharedStore};

    viso::component! {
        Tally {
            @persist("tally.count")
            state count = 0;
            state taps = 0;
            view {
                Column {
                    width: 300dp;
                    height: 80dp;
                    Text {
                        width: 300dp;
                        height: 40dp;
                        text: format("{c} in all, {t} this run", c: count, t: taps);
                    }
                    Button {
                        width: 300dp;
                        height: 40dp;
                        text: "Count";
                        on click { count += 1; taps += 1; }
                    }
                }
            }
        }
    }

    /// Which form of the tally mounts.
    #[derive(Clone, Copy)]
    enum Form {
        File,
        Inline,
    }

    /// A tally, mounted and laid out, and the stores it runs over.
    struct Mounted {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
        root: NodeId,
        shown: Vec<(NodeId, String)>,
    }

    impl Mounted {
        fn new(form: Form) -> Self {
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
                match form {
                    Form::File => viso::view!("tally.vs")(&mut cx).id(),
                    Form::Inline => Tally::build(&mut cx).1.id(),
                }
            };
            let mut mounted = Mounted {
                store,
                states,
                bindings,
                effects: EffectStore::default(),
                root,
                shown: Vec::new(),
            };
            mounted.layout();
            mounted
        }

        fn layout(&mut self) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 300.0,
                h: 80.0,
            };
            self.store.layout(self.root, surface, &mut Vec::new());
        }

        fn click(&mut self) {
            let mut chain = Vec::new();
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let event = PointerEvent {
                    x: 10.0,
                    y: 60.0,
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
                &mut self.effects,
                &changed,
            );
            self.layout();
        }

        /// The tally's label.
        fn label(&mut self) -> String {
            let mut requests = Vec::new();
            self.store.take_text_requests(&mut requests);
            for (id, request) in requests {
                match self.shown.iter_mut().find(|(n, _)| *n == id) {
                    Some(entry) => entry.1 = request.text,
                    None => self.shown.push((id, request.text)),
                }
            }
            self.shown.sort_by_key(|(id, _)| id.index());
            self.shown
                .iter()
                .map(|(_, t)| t.clone())
                .find(|t| t.contains("in all"))
                .expect("the label")
        }
    }

    #[test]
    fn the_tally_outlives_the_run_in_either_form() {
        for form in [Form::File, Form::Inline] {
            let memory = MemoryStore::default();
            viso_view::install_persistence(Some(SharedStore::new(memory.clone())));

            let mut first = Mounted::new(form);
            assert_eq!(first.label(), "0 in all, 0 this run");
            assert_eq!(first.store.suspend_hook_count(), 1);
            first.click();
            first.click();
            assert_eq!(first.label(), "2 in all, 2 this run");
            assert!(memory.get("tally.count").is_some(), "stored at the commit");
            assert_eq!(memory.keys(), ["tally.count"], "`taps` is not persisted");
            first.store.suspend(&first.states);
            assert!(viso_view::take_persist_reports().is_empty());
            drop(first);

            let mut second = Mounted::new(form);
            assert_eq!(
                second.label(),
                "2 in all, 0 this run",
                "loaded before the tree was built"
            );
            second.click();
            assert_eq!(second.label(), "3 in all, 1 this run");

            viso_view::install_persistence(None);
            let mut third = Mounted::new(form);
            assert_eq!(third.label(), "0 in all, 0 this run", "nothing installed");
            assert_eq!(third.store.suspend_hook_count(), 0);
        }
    }
}
