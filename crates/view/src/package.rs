//! The release form of a view with behavior: its UI package, its behavior
//! module, the tables that join them, the bytes of the assets it shows and
//! the shaders it paints with, loaded with no compiler present.
//!
//! A package loads in place ([`ViewPackageView`]): the loader validates the
//! whole blob, loads the behavior module and the shaders, and only then
//! mounts, reading the node, edge, state, handler and env tables one record at
//! a time. Loaded from `'static` bytes — an embedded package, mapped flash —
//! nothing of the blob is copied: assets, the shaders' cache entries and the
//! module's names and string constants borrow it ([`load_static_view`]).

use std::cell::RefCell;
use std::fmt;
use std::rc::Rc;

use viso_behavior::aot::NativeCode;
use viso_behavior::{LoadError, Module};
use viso_ende::DecodeError;
use viso_ui::aot::instantiate_indexed;
use viso_ui::{
    AssetBytes, BindingTable, EffectStore, NodeId, NodeStore, StateStore, StructureCx, VirtualLists,
};

use crate::attach::{Route, attach_node};
use crate::control::Control;
use crate::effects::mount_effects;
use crate::host::{HostError, ViewHost};
use crate::regions::mount_regions;
use crate::scope::Scope;
use crate::shaders::load_kept;
use crate::values::mount_values;

pub use viso_schema::view::package::{
    ViewAsset, ViewControl, ViewEnv, ViewHandler, ViewPackage, ViewPackageView, ViewShader,
    ViewState,
};

/// A loaded view: its root and the host its handlers dispatch into.
#[derive(Debug)]
pub struct LoadedView {
    /// The root node, `None` for an empty tree.
    pub root: Option<NodeId>,
    /// The component instance, `None` for a view without handlers.
    pub host: Option<Rc<RefCell<ViewHost>>>,
}

/// Why a [`ViewPackage`] did not load.
#[derive(Debug)]
pub enum ViewLoadError {
    /// The blob is not a well-formed package.
    Decode(DecodeError),
    /// The behavior module does not mount.
    Host(HostError),
    /// A shader of the view is not a packed brush.
    Shader(DecodeError),
}

impl fmt::Display for ViewLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ViewLoadError::Decode(error) => write!(f, "the view package does not decode: {error}"),
            ViewLoadError::Host(error) => error.fmt(f),
            ViewLoadError::Shader(error) => {
                write!(f, "a shader of the view does not load: {error}")
            }
        }
    }
}

impl std::error::Error for ViewLoadError {}

/// How a view loads beyond what its package holds.
#[derive(Default)]
pub struct ViewLoad<'a> {
    /// The package's behavior compiled to Rust (`viso_dsl::aot::package_native_code`,
    /// built into the app), run in place of the interpreter.
    pub native: Option<&'static NativeCode>,
    /// Given the host before the view mounts, to install the services its
    /// natives and `@persist` states use.
    pub setup: Option<&'a mut dyn FnMut(&mut ViewHost)>,
}

impl fmt::Debug for ViewLoad<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ViewLoad")
            .field("native", &self.native.map(|code| code.build))
            .field("setup", &self.setup.is_some())
            .finish()
    }
}

/// Validates `blob` and instantiates the view into the runtime. What the view
/// keeps of the blob — its assets, its module's names and string constants —
/// is copied; [`load_static_view`] borrows it.
pub fn load_view(
    blob: &[u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
) -> Result<LoadedView, ViewLoadError> {
    load_view_with(blob, store, states, bindings, lists, ViewLoad::default())
}

/// [`load_view`] as `load` says.
pub fn load_view_with(
    blob: &[u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
    load: ViewLoad<'_>,
) -> Result<LoadedView, ViewLoadError> {
    let view = ViewPackageView::new(blob).map_err(ViewLoadError::Decode)?;
    mount(&view, Keep::copied(), store, states, bindings, lists, load)
}

/// [`load_view_with`] for a blob that outlives the program — embedded with
/// `include_bytes!`, in memory-mapped flash — read where it lies: the view's
/// assets, its shaders' cache entries and its module's names and string
/// constants borrow the blob, so RAM holds only what the view builds.
pub fn load_static_view(
    blob: &'static [u8],
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
    load: ViewLoad<'_>,
) -> Result<LoadedView, ViewLoadError> {
    let view = ViewPackageView::new(blob).map_err(ViewLoadError::Decode)?;
    mount(
        &view,
        Keep::borrowed(),
        store,
        states,
        bindings,
        lists,
        load,
    )
}

/// How a load keeps what outlives it of a blob it reads for `'a`.
#[derive(Clone, Copy)]
struct Keep<'a> {
    asset: fn(&'a [u8]) -> AssetBytes,
    module: fn(&'a [u8]) -> Result<Module, LoadError>,
}

impl<'a> Keep<'a> {
    /// Copies it.
    fn copied() -> Keep<'a> {
        Keep {
            asset: |bytes| AssetBytes::Shared(bytes.into()),
            module: Module::decode,
        }
    }
}

impl Keep<'static> {
    /// Borrows it.
    fn borrowed() -> Keep<'static> {
        Keep {
            asset: AssetBytes::Static,
            module: Module::decode_static,
        }
    }
}

/// Instantiates a validated view into the runtime: the behavior module, the
/// host and its shaders first — everything that can still fail — then its
/// state cells, with their initial values, the tree, the host's `env` reads,
/// its persisted states, its handlers, its control-flow regions and the values
/// its nodes show.
fn mount<'a>(
    view: &ViewPackageView<'a>,
    keep: Keep<'a>,
    store: &mut NodeStore,
    states: &mut StateStore,
    bindings: &mut BindingTable,
    lists: &mut VirtualLists,
    load: ViewLoad<'_>,
) -> Result<LoadedView, ViewLoadError> {
    let mut host = if view.behavior.is_empty() {
        None
    } else {
        let module = Rc::new(
            (keep.module)(view.behavior)
                .map_err(|error| ViewLoadError::Host(HostError::Load(error)))?,
        );
        let mut host = match load.native {
            Some(native) => ViewHost::with_native(module, view.component, native),
            None => ViewHost::new(module, view.component),
        }
        .map_err(ViewLoadError::Host)?;
        host.set_assets(view.assets().map(|(id, bytes)| (id, (keep.asset)(bytes))));
        let shaders = view.shaders().map(|bytes| load_kept(bytes, keep.asset));
        let shaders: Vec<_> = shaders
            .collect::<Result<_, _>>()
            .map_err(ViewLoadError::Shader)?;
        host.set_shaders(shaders);
        if let Some(setup) = load.setup {
            setup(&mut host);
        }
        Some(host)
    };
    let mut cells = Vec::with_capacity(view.states().len());
    for state in view.states() {
        let id = match states.id_for_key(state.key) {
            Some(id) => id,
            None => {
                let id = states.alloc(state.initial);
                states.bind_key(id, state.key);
                id
            }
        };
        cells.push((state.key, id));
        if let (Some(host), Some(slot)) = (&mut host, state.slot) {
            if state.tracked {
                host.track(slot as usize, id);
            } else {
                host.mirror(slot as usize, id);
            }
        }
    }
    let mut node_ids = Vec::new();
    let root = instantiate_indexed(&view.ui, store, states, bindings, lists, &mut node_ids);
    if let Some(host) = &mut host {
        for read in view.env() {
            let anchor = node_ids.get(read.anchor as usize).copied().flatten();
            host.link_env(read.slot as usize, read.field, anchor, states);
        }
        for read in view.game() {
            host.link_game(read.slot as usize, read.query, states);
        }
    }
    let Some(host) = host.map(ViewHost::shared) else {
        return Ok(LoadedView { root, host: None });
    };
    if let Some(root) = root {
        crate::game::mount_view_game(store, &host, root);
    }
    crate::__mount_persisted(&host, store, states);
    let mut routes: Vec<Route> = Vec::new();
    let mut shown: Vec<(NodeId, Control)> = Vec::with_capacity(view.control_count());
    let mut handlers = view.handlers().peekable();
    let mut controls = view.controls().peekable();
    loop {
        let node = match (handlers.peek(), controls.peek()) {
            (Some(h), Some(c)) => h.node.min(c.node),
            (Some(h), None) => h.node,
            (None, Some(c)) => c.node,
            (None, None) => break,
        };
        routes.clear();
        while let Some(handler) = handlers.next_if(|h| h.node == node) {
            routes.push(handler.route);
        }
        let control = controls.next_if(|c| c.node == node).map(|c| c.control);
        if let Some(Some(id)) = node_ids.get(node as usize).copied() {
            if let Some(control) = &control {
                shown.push((id, control.clone()));
            }
            attach_node(store, &host, id, &routes, control, &Scope::EMPTY);
        }
    }
    // A first mount frees nothing, so no effect is ever cancelled here.
    let mut effects = EffectStore::default();
    let mut cx = StructureCx {
        store,
        states,
        bindings,
        effects: &mut effects,
        lists,
    };
    let regions = view.regions();
    let reaches = crate::nodes::reaches_nodes(view.named().len(), &regions);
    if !regions.is_empty() {
        mount_regions(&mut cx, Rc::new(regions), &host, &node_ids, &cells);
    }
    mount_values(&mut cx, &host, &shown);
    if reaches {
        let named: Vec<(u32, NodeId)> = view
            .named()
            .filter_map(|n| Some((n.key, node_ids.get(n.node as usize).copied().flatten()?)))
            .collect();
        crate::nodes::mount_nodes(&mut cx, &host, &named);
    }
    if let Some(root) = root {
        mount_effects(cx.store, &host, root);
    }
    Ok(LoadedView {
        root,
        host: Some(host),
    })
}
