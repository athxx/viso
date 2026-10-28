# ADR 0059 — `web-hybrid` islands: one component the renderer draws inside a DOM page

- Status: Accepted
- Date: 2026-10-29

## Context

A `web-hybrid` page is a `web-dom` page (ADR 0058) with parts the Viso renderer draws: a chart,
an editor canvas or a 3D viewer inside DOM chrome. The boundary between the two must be explicit
in the UI IR. The exporter must not guess it from widget names. The two sides run on two
runtimes in one wasm:
- the DOM page's VM instance, which the page script calls;
- the GPU runtime's view mount, which draws into a canvas.

Sharing one component instance between them would mean syncing every state both read. That
syncing would cost every write, and its ordering would be subtle.

## Decision

1. **The boundary is a component.** `Island { Chart { points: data; on picked(e) { … } } }`
   marks one component instance of the view's file. The component's inputs and events are all
   that cross the boundary. The two runtimes share no state: inputs flow in and events flow out,
   the way props and callbacks do. Islands do not nest, and an island's content is exactly one
   component of the file. Both are mount errors on every target, so a view stays portable.
2. **Everywhere else an island is a stack.** On a target that does not split the view, `Island`
   is a stack of its one child. The child is inlined as any component instance is (ADR 0039).
3. **The compiler records each island** in `ViewBehavior::islands`: its node, its instance, its
   component, the handler-table entry of each input the caller binds, and the entry of each
   event handler the caller declares.
4. **The page side.** The hybrid lowering writes the island as an element marked
   `data-viso-root` and lowers nothing inside it. The DOM page's VM evaluates the island's input
   entries as bindings and pushes each new value across. It runs a handler entry for each event
   the island emits. The inlined instance's states, effects and tasks are not mounted in the
   page.
5. **The renderer side.** Under the facade's `web-hybrid` feature, `view!` offers the page its
   module, as under `web-dom`. It mounts the island's component as the window's root view,
   compiled at expansion time as `view!` compiles any root (AOT in release). Its inputs are
   root inputs whose values the page pushes. Its events go to a sink that hands them to the
   page's VM once the frame's dispatch is over.
6. **One island per page for now.** The web platform backend has one window. A second island,
   or an island inside a region, is a `web-hybrid` capability diagnostic, not a language rule.

## Consequences

- Each side keeps its fast path: the DOM page computes with its VM, the island renders retained
  GPU data, and only changed input values cross.
- A value crossing the boundary is a plain value. A closure or a native handle does not cross.
- The island's component compiles twice: once inlined in the page's module, once as the
  renderer's root. Only the second runs its states.
