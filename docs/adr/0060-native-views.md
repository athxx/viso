# ADR 0060 — Native views: platform views placed over nodes

- Status: Accepted
- Date: 2026-10-29

## Context

Some content only the platform can show well: a browser engine (WKWebView, Android WebView), a
native map or an ad view. Drawing such content into a texture means snapshotting it every frame.
That is slow, loses the platform's own scrolling and text input, and on some platforms is not
possible at all. The usual answer is to put the platform view itself in the window, over the
part of the UI it belongs to. The view must then follow its node as layout, scrolling and
clipping move it, and it must go away with the node.

Until now nothing told code outside the framework where a node is after layout. Notices report
a node's size but not where it is. A frame client gets no rects. The facade pushed rects to the
platform only for its own needs: caption drag regions and the text-entry rect.

## Decision

1. **A node may carry one `NativeView`** (`viso_ui::native`). A plugin attaches it with
   `BuildCx::native_view(handle, view)`. The store keeps the views in a plain cold list, since a
   window has few of them. Freeing the node drops its view, and the view's `Drop` takes the
   platform view down.
2. **One post-layout callback, `NativeView::place(&NativePlace)`.** In `UpdateTransforms`,
   after the world boxes are final, the facade calls `NodeStore::place_native_views` for every
   window that has any native views. Each view hears:
   - the window's `RawWindowHandle`;
   - the node's world box, in logical points;
   - the box intersected with the window and with the boxes of every clipping ancestor (clips
     and scroll viewports);
   - the device scale;
   - whether the node shows (neither it nor an ancestor hidden or transparent, and the clip
     not empty).
   It hears this the first time and then only when one of those changes. The callback runs on
   the window's thread. It is a plugin hook, not a user phase: it gets no context and cannot
   touch the tree.
3. **Reports come back through `NativeSender<T>`** (`BuildCx::native_events(handle, on)`).
   Any thread may send. The node's handler `on(T, &mut StateStore)` hears each report at the
   next frame boundary, the same way a notice handler does. The channel is a UI task owned by
   the node; each continuation re-arms it. When the node is freed the task drops, the inbox
   closes, and later sends return `false` and keep nothing.
4. **The platform view is on top.** It draws over whatever the window draws in its box,
   popups included, and takes the input there through the OS's own hit testing. The framework
   does not composite under or over it.

## Consequences

- A browser widget (`viso-ext/viso-webview`) and similar plugins need no platform hooks in the
  core beyond this one.
- When no node has a native view, the steady-state frame does no work for this. When some do,
  the facade walks each view's ancestors once per frame. There are few such views and the walk
  is shallow.
- Clips are rectangular. A rounded clip corner or a transform other than a translation is not
  applied to the platform view.
- A Viso popup over a native view's box is hidden under the platform view. An app that needs a
  popup there should hide the view for that time.
