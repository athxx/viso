//! A localized inbox: every label comes from the message catalogs in `i18n/`
//! through `tr`, checked against them when the crate compiles, and shown in
//! the reader's locale (`env.locale`) with its plural rules and digits. A
//! locale the catalogs do not translate falls back along the CLDR chain to the
//! source locale, `[i18n] source` in `Viso.toml`.

use viso::prelude::*;

struct Inbox;

impl Application for Inbox {
    fn new(_cx: &mut AppCx) -> Self {
        Inbox
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("inbox.vs")(cx);
    }
}

fn main() {
    viso::run::<Inbox>();
}

#[cfg(test)]
mod tests {
    use viso::render::Rect;
    use viso::ui::{
        BindingTable, BuildCx, EffectStore, NodeId, NodeStore, SemanticProjector, StateStore,
        TextEdits, VirtualLists, run_structure_hooks,
    };

    /// The inbox, mounted and laid out, and the stores it runs over.
    struct Mounted {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
        root: NodeId,
        shown: Vec<(NodeId, String)>,
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
                viso::view!("inbox.vs")(&mut cx).id()
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
                w: 400.0,
                h: 120.0,
            };
            self.store.layout(self.root, surface, &mut Vec::new());
        }

        fn locale(&mut self, tag: &str) {
            self.states.update_env(|e| e.locale = tag.into());
            self.states.settle_env(&self.store);
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

        /// The text each label shows, in node order.
        fn labels(&mut self) -> Vec<String> {
            let mut requests = Vec::new();
            self.store.take_text_requests(&mut requests);
            for (id, request) in requests {
                match self.shown.iter_mut().find(|(n, _)| *n == id) {
                    Some(entry) => entry.1 = request.text,
                    None => self.shown.push((id, request.text)),
                }
            }
            self.shown.sort_by_key(|(id, _)| id.index());
            self.shown.iter().map(|(_, t)| t.clone()).collect()
        }
    }

    #[test]
    fn the_inbox_reads_in_each_readers_locale() {
        let mut inbox = Mounted::new();
        assert_eq!(
            inbox.labels(),
            ["Inbox", "3 unread messages", "Mark one read"]
        );
        inbox.locale("de-CH");
        assert_eq!(
            inbox.labels(),
            [
                "Posteingang",
                "3 ungelesene Nachrichten",
                "Eine als gelesen markieren"
            ]
        );
        inbox.locale("ar");
        let ar = inbox.labels();
        assert_eq!(ar[1], "3 رسائل غير مقروءة", "Arabic's few");
        assert_eq!(ar[2], "Mark one read", "untranslated, from the source");
    }
}
