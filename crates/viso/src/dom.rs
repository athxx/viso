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
//!
//! The `web-hybrid` build offers the page its view the same way, and the
//! app runs as on `web-gpu`: its window is the page's `Island`, whose
//! component `view!` mounts as the root view. The app opens the page it
//! shares with the script to take the values the island starts with; the
//! values the page pushes later land at the next frame, and the events the
//! island emits run on the page once the frame is over.

#[cfg(feature = "web-hybrid")]
use std::cell::RefCell;
#[cfg(feature = "web-hybrid")]
use std::rc::Rc;

use viso_behavior::aot::NativeCode;
use viso_behavior::native::NativeLibrary;
#[cfg(feature = "web-dom")]
use viso_ui::context::AppCx;
#[cfg(feature = "web-dom")]
use viso_ui::{BindingTable, NodeStore, SemanticProjector, StateStore, TextEdits, VirtualLists};
use viso_ui::{BuildCx, Handle, LeafStyle};

#[cfg(feature = "web-dom")]
use crate::Application;

thread_local! {
    /// How many views `build` offered.
    static OFFERED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Starts `A` for its page.
#[cfg(feature = "web-dom")]
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

/// Offers the page the view of `component`.
#[doc(hidden)]
pub fn __offer_page(
    module: &'static [u8],
    component: &'static str,
    native: Option<&'static NativeCode>,
    libraries: &[&'static [&'static NativeLibrary]],
) {
    viso_dom::offer(viso_dom::Boot {
        module,
        component,
        native,
        libraries: Vec::leak(libraries.to_vec()),
    });
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
    __offer_page(module, component, native, libraries);
    OFFERED.with(|offered| offered.set(offered.get() + 1));
    cx.leaf(LeafStyle::default())
}

/// The values the island of the page of `component` starts with: what each
/// of its input entries `entries` gives on the page now.
#[doc(hidden)]
#[cfg(feature = "web-hybrid")]
pub fn __island_values(component: &str, entries: &[u32]) -> Vec<viso_behavior::Value> {
    viso_dom::island::mount(0, component, entries).unwrap_or_else(|error| {
        crate::eprintln!("viso: the page of the island does not open: {error}");
        vec![viso_behavior::Value::Nil; entries.len()]
    })
}

/// Links the island's root view, whose host is `host` and root `root`, to
/// its page: each value the page pushes lands in its root input at the next
/// frame, and each event of `events`, `(the event's index, the page's
/// handler entry)`, runs on the page as the event's record once the frame is
/// over.
#[doc(hidden)]
#[cfg(feature = "web-hybrid")]
pub fn __link_island(
    cx: &mut BuildCx<'_>,
    host: &Rc<RefCell<viso_view::ViewHost>>,
    root: viso_ui::NodeId,
    events: &'static [(u32, u32)],
) {
    host.borrow_mut().set_event_sink(move |event| {
        if let Some(&(_, entry)) = events.iter().find(|(index, _)| *index == event.index) {
            let record = viso_behavior::Aggregate {
                tag: 0,
                fields: event.args,
            };
            viso_dom::island::emit(entry, viso_behavior::Value::Agg(Rc::new(record)));
        }
    });
    let pushes = pushes(Rc::clone(host), root);
    cx.structure(|cx| cx.store.spawn_task(root, pushes));
}

/// The task taking the values the page pushes to the island next, which
/// lands them in the root inputs of `host` and starts the next.
#[cfg(feature = "web-hybrid")]
fn pushes(host: Rc<RefCell<viso_view::ViewHost>>, root: viso_ui::NodeId) -> viso_ui::TaskFuture {
    Box::pin(async move {
        let mut pushed = Vec::new();
        std::future::poll_fn(|cx| viso_dom::island::poll_pushed(0, cx, &mut pushed)).await;
        let land: viso_ui::Continuation =
            Box::new(move |cx: &mut viso_ui::context::UpdateCx<'_>| {
                {
                    let mut view = host.borrow_mut();
                    for (slot, value) in pushed {
                        view.push_input(slot as usize, value, cx);
                    }
                }
                cx.__spawn_for(root, pushes(host, root));
            });
        Some(land)
    })
}
