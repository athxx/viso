//! A `view!` painting with a shader embeds it packed at build time: the
//! target backend's code beside the reference interpreter's program, loaded
//! once and shared by every mount, its node painting with the program and
//! the value's parameters.

use std::sync::Arc;

use viso::ui::{
    BindingTable, BuildCx, Content, NodeId, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};
use viso_gpu::{Backend, GpuBackend, ShaderLang};

/// `build` built into a fresh store: the store and the root.
fn mount(build: impl FnOnce(&mut BuildCx<'_>) -> viso::ui::Handle) -> (NodeStore, NodeId) {
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
        build(&mut cx).id()
    };
    (store, root)
}

/// The program the shader view of the card at `root` paints with.
fn painted(store: &NodeStore, root: NodeId) -> Arc<viso_ui::shader::BrushProgram> {
    let view = store
        .arena()
        .links(root)
        .and_then(|l| l.first_child)
        .unwrap();
    let Some(Content::Shader {
        program, params, ..
    }) = store.content_payload(view)
    else {
        panic!("{:?}", store.content_payload(view));
    };
    let program = Arc::clone(store.shaders().get(*program).unwrap());
    assert_eq!(params.len(), program.layout.stride as usize);
    program
}

#[test]
fn an_embedded_shader_paints_its_view_with_the_targets_code() {
    let (store, root) = mount(viso::view!("fixtures/shader_card.vs"));
    let program = painted(&store, root);
    assert_eq!(&*program.name, "Ripple");
    let langs: Vec<ShaderLang> = program.code.iter().map(|c| c.lang()).collect();
    let target = <Backend as GpuBackend>::SHADER_LANG;
    if target == ShaderLang::None {
        assert_eq!(langs, [ShaderLang::None]);
    } else {
        assert_eq!(langs, [target, ShaderLang::None]);
    }

    let (again, root) = mount(viso::view!("fixtures/shader_card.vs"));
    assert!(
        Arc::ptr_eq(&painted(&again, root), &program),
        "loaded once, shared by every mount"
    );
}
