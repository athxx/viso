# ADR 0058 — A `web-dom` page runs its view's behavior on the VM in the app's wasm

- Status: Accepted
- Date: 2026-10-28

## Context

`web-dom` is the web target whose page the browser lays out and draws: HTML and CSS, not a
canvas. `viso export html` already lowers a `.vs` view to that, with a small JS runtime that
keeps the DOM current, but it also lowers the view's behavior to JS. That second lowering
covers part of the language. Natives, tasks and compiled release code do not cross it, and it
is a second implementation of the language's semantics.

A `web-dom` build is still a Viso app: `viso::run`, `Application::new` and `build`, the same
packages and natives as every other target.

## Decision

1. **The page is the export's lowering; the behavior stays on the VM.** `viso_dsl::export::dom`
   lowers the view with the html export's lowering, and the page script uses the same DOM
   runtime. Each value a node shows and each handler, region entry, effect and task calls the
   VM in the app's wasm. The script calls `V.e` to evaluate an entry and `V.t` to run one as a
   transaction, then takes back what the calls changed, emitted, started and asked of named
   nodes. The script never computes a value itself.
2. **A new crate, `viso-dom`.** It holds the page's VM instance and the wasm bindings
   (`visoDom`). Its only internal dependency is `viso-behavior`. Its `NEVER` edges forbid
   `viso-ui`, `viso-render`, `viso-gpu` and `viso-text`, so nothing of the frame pipeline can
   be in a page.
3. **The root is a `.vs` view.** Under the facade's `web-dom` feature, `view!` builds no node.
   It offers the page its module, its compiled code and its native libraries, and returns a
   placeholder leaf. `viso::run` runs `new` and `build` without a window or a frame, so the
   release wasm drops the renderer, layout and text. Nodes `build` makes in Rust are not shown.
   The app prints a warning when it makes any. `viso check web-dom` reports what the page
   cannot hold:
   - `WEB_DOM_NO_VIEW` when the project has no view;
   - `WEB_GPU_ONLY_NODE` for a `ShaderView` or a `Viewport3D`;
   - `WEB_DOM_UNSUPPORTED` for the rest (Rust components, routers, games).
4. **Dev pages carry their module; release pages check theirs.** A dev page ships
   `view.<hash>.vsb`, and the app runs it on the interpreter, so a `.vs` edit rebuilds the page
   but not the Rust. A release page uses the module the app embeds, which is compiled code in
   release. It opens only when the module's build hash matches the page's.

## Consequences

- The language has one runtime semantics on the web too. A fix to the VM or the compiled code
  lands in `web-dom` pages.
- A page always loads wasm, even when its view has no behavior. A release counter's wasm is
  912 KiB.
- The export knows a package's natives by their schema only. A value that calls one is not
  prerendered, and the page's VM computes it as the page opens. A component whose initial
  state calls one does not lower yet.
- `web-hybrid` (a DOM shell with GPU islands) builds on this runtime and the GPU path, with its
  boundary explicit in the UI IR.
