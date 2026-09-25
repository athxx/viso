# ADR 0030 — Accessibility OS bridge: AccessKit tree updates at the platform boundary

- Status: Accepted
- Date: 2026-09-26

## Context

The canonical accessibility model is Viso's own `SemanticsTree`, derived from the
retained node store (`viso-ui`). Screen readers never saw it: no backend exposed it to
the OS. `Viso_Architecture.md` (Tier B, §29) says the OS bridge is integration Viso
owns but should build on a proven implementation, naming AccessKit, and the repository
tree lists an `integrations/accesskit/` crate.

Three facts shape where the bridge lives:

- An adapter needs the backend's native objects and threads: the `NSView`, the `HWND`
  and its window procedure, the Android host `View` on the Java UI thread, the D-Bus
  connection's worker thread, the DOM.
- `viso-platform` may not depend on `viso-ui` (§3.5), so the semantics tree cannot
  cross the platform boundary as-is.
- AccessKit has no iOS adapter and no Web adapter.

## Decision

### 1. The boundary format is `accesskit::TreeUpdate`

`viso-platform` depends on `accesskit` (a data-only crate) and re-exports it. The
push API is one cold-path method:

```rust
fn update_accessibility(&mut self, window: WindowId, update: accesskit::TreeUpdate) {}
```

It defaults to a no-op, so headless and bridge-less backends need no code. The facade
is the only producer: it converts `SemanticsTree` to AccessKit nodes and sends them.
`viso-ui` stays AccessKit-free.

### 2. Requests come back as one neutral event

```rust
RawEvent::Accessibility { window, request: AccessRequest }
enum AccessRequest { Activated, Deactivated, Action { target: u64, action: AccessAction } }
enum AccessAction { Focus, Click, Increment, Decrement }
```

`RawEvent` carries no AccessKit type. It stays `Clone + PartialEq`, and a backend that
drives a custom bridge (iOS, Web) speaks the same vocabulary. The scheduler routes it to
`FrameDriver::on_accessibility` and schedules a frame.

### 3. The facade publishes only while an assistive technology listens

Per window, the facade holds the last published nodes and does nothing until it gets
`Activated`.

- **On activation:** it publishes the full tree at the next `UpdateSemantics` phase.
- **While active:** it re-derives on a frame that dirtied SEMANTICS, LAYOUT or
  TRANSFORM, and sends only the nodes that differ from the last publication. The update
  also carries the parents of added or removed children, and the focus.
- **On `Deactivated`:** it drops its state.

With no assistive technology running, a frame costs one boolean check.

Mapping:

- An AccessKit id is `index | generation << 32`.
- The root becomes `Role::Window`.
- Hidden subtrees are left out.
- Bounds are the node's world rect in physical pixels.
- Focus is the focused node, or the root when nothing has focus.

### 4. Actions route through the ordinary input path

- `Focus` moves the focus slot the way Tab does: it dirties PAINT and SEMANTICS on the
  old and new focused nodes.
- `Click` synthesizes a primary press and release at the node's center.
- `Increment` and `Decrement` send the arrow keys to the focused target.

No widget learns a second activation protocol.

### 5. Adapters live in their platform backend

`integrations/accesskit/` is not created: each adapter needs its backend's private
native objects and thread model.

| Backend | Adapter |
|---|---|
| macOS | `accesskit_macos::SubclassingAdapter` on the content view |
| Windows | `accesskit_windows::SubclassingAdapter` on the HWND |
| Linux | `accesskit_unix::Adapter`; its callbacks run on the D-Bus thread and reach the loop through a channel plus a wakeup |
| Android | `accesskit_android::InjectingAdapter` on the host view; its callbacks run on the Java UI thread and are queued |
| iOS | Viso-owned `UIAccessibilityElement` container on the view |
| Web | Viso-owned ARIA mirror: hidden, absolutely positioned DOM elements over the canvas |

The two custom adapters consume the same `TreeUpdate`s. If AccessKit ships an adapter
for one of these targets, it replaces the custom one; no second path is kept.

## Consequences / 代价

- `viso-platform` takes AccessKit and, per target, its adapter dependencies.
  - `accesskit_macos` builds on objc2 0.5 next to the backend's objc2 0.6, so two objc2
    generations compile on Apple targets until AccessKit moves.
  - `accesskit_android` brings the `jni` crate next to `jni-sys`.
- The semantics tree is still re-derived whole on a dirty frame, and the diff runs over
  the whole tree. This is proportional to tree size, runs only while an assistive
  technology is active, and is the recorded refinement point (per-subtree derive).
- Two adapters (iOS, Web) are Viso code to maintain. Their role mapping has to follow
  AccessKit's so all targets announce the same tree.
