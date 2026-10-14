//! A photo and an icon from `assets/`: `asset("…")` names each file, checked
//! when the crate compiles and typed by its format, and `view!` embeds the
//! bytes, so editing a file rebuilds the crate. A button switches the photo
//! between two assets; only the ones shown are decoded. Rust code embeds one
//! with `viso::asset!("…")` and shows it with `viso::widgets::image_asset`.

use viso::prelude::*;

struct Gallery;

impl Application for Gallery {
    fn new(_cx: &mut AppCx) -> Self {
        Gallery
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("gallery.vs")(cx);
    }
}

fn main() {
    viso::run::<Gallery>();
}

#[cfg(test)]
mod tests {
    use viso::render::Rect;
    use viso::ui::resource::ResourceId;
    use viso::ui::{
        AssetDraw, AssetFit, BindingTable, BuildCx, Content, EffectStore, NodeId, NodeStore,
        PointerButtons, PointerEvent, PointerPhase, PointerRouter, SemanticProjector, StateStore,
        TextEdits, VirtualLists, run_structure_hooks,
    };

    struct Mounted {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        effects: EffectStore,
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
                viso::view!("gallery.vs")(&mut cx).id()
            };
            let mut mounted = Mounted {
                store,
                states,
                bindings,
                effects: EffectStore::default(),
                root,
            };
            mounted.layout();
            mounted
        }

        fn layout(&mut self) {
            let surface = Rect {
                x: 0.0,
                y: 0.0,
                w: 320.0,
                h: 240.0,
            };
            self.store.layout(self.root, surface, &mut Vec::new());
        }

        /// The nodes in pre-order.
        fn nodes(&self) -> Vec<NodeId> {
            let arena = self.store.arena();
            let mut out = Vec::new();
            let mut stack = vec![self.root];
            while let Some(node) = stack.pop() {
                out.push(node);
                let mut children = Vec::new();
                let mut child = arena.links(node).and_then(|l| l.first_child);
                while let Some(id) = child {
                    children.push(id);
                    child = arena.links(id).and_then(|l| l.next_sibling);
                }
                stack.extend(children.into_iter().rev());
            }
            out
        }

        fn shows(&self, node: NodeId) -> (ResourceId, AssetFit) {
            match self.store.content_payload(node) {
                Some(Content::Asset {
                    resource,
                    draw: AssetDraw::Nothing,
                    fit,
                    ..
                }) => (*resource, *fit),
                other => panic!("{other:?}"),
            }
        }

        fn offered(&self) -> Vec<(ResourceId, usize)> {
            let mut offered: Vec<_> = self
                .store
                .assets()
                .undecoded()
                .map(|(id, _, bytes)| (id, bytes.len()))
                .collect();
            offered.sort();
            offered
        }

        fn click(&mut self, node: NodeId) {
            let at = self.store.world(node);
            let mut chain = Vec::new();
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                let event = PointerEvent {
                    x: at.x + 1.0,
                    y: at.y + 1.0,
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
    }

    #[test]
    fn the_gallery_shows_the_embedded_assets_it_names() {
        let mut gallery = Mounted::new();
        let nodes = gallery.nodes();
        let (photo, star, button) = (nodes[1], nodes[3], nodes[4]);
        let (day, night, icon) = (
            ResourceId::of("photos/day.png"),
            ResourceId::of("photos/night.jpg"),
            ResourceId::of("icons/star.svg"),
        );
        assert_eq!(gallery.shows(photo), (day, AssetFit::Cover));
        assert_eq!(gallery.shows(star), (icon, AssetFit::Contain));
        let size = |path: &str| std::fs::metadata(format!("assets/{path}")).unwrap().len() as usize;
        let mut expected = vec![
            (day, size("photos/day.png")),
            (icon, size("icons/star.svg")),
        ];
        expected.sort();
        assert_eq!(
            gallery.offered(),
            expected,
            "the embedded bytes, only what shows"
        );

        gallery.click(button);
        assert_eq!(gallery.shows(photo), (night, AssetFit::Cover));
        expected.push((night, size("photos/night.jpg")));
        expected.sort();
        assert_eq!(gallery.offered(), expected);
    }

    #[test]
    fn rust_code_embeds_an_asset_with_the_macro() {
        use viso::ui::resource::AssetKind;
        use viso::ui::{Component, Role};

        let star = viso::asset!("icons/star.svg");
        assert_eq!(star.id, ResourceId::of("icons/star.svg"));
        assert_eq!(star.kind, AssetKind::Vector);
        assert_eq!(&*star.bytes, include_bytes!("../assets/icons/star.svg"));
        let mut store = NodeStore::new();
        let mut cx = BuildCx::new(&mut store);
        viso::widgets::image_asset(star)
            .label("Star")
            .build(&mut cx);
        let root = cx.root().expect("a root");
        assert_eq!(store.semantics(root).map(|s| s.role), Some(Role::Image));
        assert_eq!(store.assets().undecoded().count(), 1);
    }
}
