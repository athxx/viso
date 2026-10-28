//! The `web-dom` build: the app's root view as its page's DOM, which the
//! browser lays out, and the view's behavior on the VM in the page.
//!
//! The app starts as everywhere — [`Application::new`], then
//! [`Application::build`] — but `view!` builds no node there: it offers the
//! page its view, the module it embeds with the code and the native
//! libraries it runs with. The page script then opens the view of the
//! component it mounts. No window opens and no frame runs: nothing the
//! renderer, the layout or the text runtime does is reached, so none of it
//! is in the module.

use viso_behavior::aot::NativeCode;
use viso_behavior::native::NativeLibrary;
use viso_ui::context::AppCx;
use viso_ui::{
    BindingTable, BuildCx, Handle, LeafStyle, NodeStore, SemanticProjector, StateStore, TextEdits,
    VirtualLists,
};

use crate::Application;

thread_local! {
    /// How many views `build` offered.
    static OFFERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Starts `A` for its page.
pub(crate) fn run<A: Application>() {
    let mut cx = AppCx::__new();
    let mut app = A::new(&mut cx);
    let mut store = NodeStore::new();
    let mut states = StateStore::new();
    let mut bindings = BindingTable::new();
    let mut lists = VirtualLists::new();
    let mut edits = TextEdits::new();
    let mut projectors = SemanticProjector::new();
    let mut build = BuildCx::with_reactive(
        &mut store,
        &mut states,
        &mut bindings,
        &mut lists,
        &mut edits,
        &mut projectors,
    );
    app.build(&mut build);
    // Each offered view stands as one placeholder node; anything else was
    // built in Rust, which a page does not show.
    let built = store.arena().live_count();
    let offered = OFFERED.with(std::cell::Cell::get);
    if built > offered {
        crate::eprintln!(
            "viso: `build` makes {} node(s) in Rust beside its `view!`, which a `web-dom` page does not show: build for `web-gpu` or `web-hybrid` to show them",
            built - offered
        );
    }
    // The app lives as long as its page.
    std::mem::forget(app);
}

/// `view!`'s mount in a `web-dom` build: offers the page the view of
/// `component` and returns a node standing for it.
#[doc(hidden)]
pub fn __offer_view(
    cx: &mut BuildCx<'_>,
    module: &'static [u8],
    component: &'static str,
    native: Option<&'static NativeCode>,
    libraries: &[&'static [&'static NativeLibrary]],
) -> Handle {
    viso_dom::offer(viso_dom::Boot {
        module,
        component,
        native,
        libraries: Vec::leak(libraries.to_vec()),
    });
    OFFERED.with(|offered| offered.set(offered.get() + 1));
    cx.leaf(LeafStyle::default())
}
