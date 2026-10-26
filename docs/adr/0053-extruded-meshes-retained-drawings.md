# ADR 0053 — Extruded mesh vertices, retained drawings and a shared worker pool

- Status: Accepted
- Date: 2026-10-26

## Context

A map draws tens of thousands of lines and polygons per tile and is panned, zoomed between
whole levels and rotated continuously. Re-tessellating on every zoom step would put
geometry work and vertex uploads on the frame path; scaling a tessellated mesh with its
transform root scales its line widths and antialiasing fringes with it. A tile's geometry
also needs to be built away from the loop thread, by whichever crate holds the tile
format, and drawn by one node without being copied each time the node moves.

## Decision

- `MeshVertex` gains `extrude: [f32; 2]` after `edge` (stride 28 → 36): an offset in screen
  points added after the draw's place. The mesh vertex stage adds
  `axis_x · e.x / |axis_x| + axis_y · e.y / |axis_y|` — the place turns the offset, its scale
  never stretches it; the CPU raster does the same. Ordinary geometry carries zero. A line
  built as its centerline with vertices extruded by half its width keeps its width, and a
  fringe extruded one point stays one point, at any scale of the root it moves with. Damage
  placement grows a mesh's placed extent by its largest extrusion after the place, so a
  shrinking root never under-damages.
- `Mesh` holds its buffers shared (`Arc<[MeshVertex]>`, `Arc<[u32]>`) and a `MeshPlace` (a scale
  per axis, then a shift) applied to positions when lowered. The mesh store keeps the shared
  buffers instead of a copy and treats the same buffers at the same place as unchanged
  without looking at a vertex; a new place is a geometry change. A large mesh is held once on
  the CPU however often it is painted, moved or re-uploaded.
- `viso-ui` gains `Content::Drawing` (`viso_ui::Drawing`): shared primitives (`Arc<[Primitive]>`)
  in a drawing space of a given extent, mapped onto the node's box at paint (quads, images,
  paths; meshes by their place, so no vertex is copied; extrusions unscaled). It has no intrinsic size, is compared by identity
  and marks PAINT only, so moving or resizing its node copies and rebuilds nothing, and a
  node that only moves with a transform root does not repaint.
- `viso-ui` gains `viso_ui::work`: one resident pool of worker threads for off-loop jobs,
  sized by the facade from the footprint (`workers`), with no threads in a headless build.
  Without threads, jobs queue for the loop, which runs them in an idle slice (4 ms; all of
  them headless) at the frame's task phase and keeps frames coming while any wait. A job
  reports through whatever it captures — usually a completer whose waker wakes the task
  awaiting it.

- `viso-ui` gains wheel takers (`NodeStore::set_takes_wheel`, `EventCx::wheel`): a node that
  takes the wheel hears every wheel and trackpad scroll over it — notches, the fingers'
  scroll and the system's coast — as a sample on its own chain, before any viewport above
  it; a viewport between the pointer and the taker keeps its own. And a `Size` notice
  (`NoticeKinds::SIZE`): a watched node hears its laid-out size at its first layout and
  whenever it changes, and the frame its handler writes in is scheduled.

## Consequences

- Every mesh and path vertex is 8 bytes larger; path geometry is small, so the cost is in
  the map's tiles, where it buys zoom without re-tessellation.
- The frozen `mesh.spv` and the mesh MSL oracle change with the vertex layout.
- Geometry whose screen size must not follow a root's scale (map lines, labels, markers'
  outlines) has a general way to say so; nothing in the UI or renderer knows about maps.
