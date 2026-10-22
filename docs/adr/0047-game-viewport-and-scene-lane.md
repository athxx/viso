# ADR 0047 — GameViewport and the 3D scene lane

- Status: Proposed
- Date: 2026-10-22

## Context

- DSL §109 composes a game screen as `Stack { GameViewport {} Column { … } }`.
  A view whose module declares systems runs a `GameSession` on its root
  (ADR 0037), and its HUD reads the game through queries (ADR 0045). Nothing
  draws the World: `GameViewport` does not exist.
- What the game side already has:
  - `GameWorld::extract(alpha, &mut Vec<Extracted>)` yields each live
    entity's interpolated centre, half extents, `Model` and facing.
  - The Kit's `Model` enum names primitives (cube, capsule, sphere,
    cylinder, cone, wedge, ground) and composite props (tree, rock, barrel,
    coin, gem, flag, car, truck, hero, villager, monster, robot).
  - `Stage` has the camera (`CameraRig`, eased into a `CameraView`: eye,
    target, up, fov, near, far), particle `Sprite`s, debug shapes and terrain
    boxes.
  - `viso-math` has `Mat4::perspective` and `look_at`.
- What the GPU side has and lacks:
  - `TextureFormat::Depth32Float` maps on every backend, and
    `PipelineDesc.depth_format` exists. No render pass takes a depth
    attachment, no pipeline has depth state, and nothing sets either.
  - Vertex input is the built-in Path and Mesh layouts. A pipeline cannot
    declare its own attributes or an instance-rate buffer.
  - Inline uniforms are capped at 32 bytes. `Binding::Uniform` buffers
    exist.
  - `RenderTarget::Texture` renders offscreen on every backend, and
    `Content::Image { texture }` composites a texture into the 2D scene.
  - `PassWork` is a closed enum of 2D work, and `MeshStore` holds 2D meshes.
- Rendering §21.4 fixes the shape: the 3D scene shares the device, resource
  manager, shader compiler, render graph, frame scheduler and profiler. It
  joins as its own pass, the Mesh/3D lane. The 2D primitive pipeline gains
  no per-node depth, material graph or scene traversal.
- Makepad's game view renders the scene into a child pass with its own
  color and depth textures (`DepthD32`, cleared to 1.0) and a pass-level
  camera (projection and view uniforms). It composites the color texture as
  the widget's background quad, then draws the HUD and billboard labels in
  2D on top. Its cube and PBR shaders are instanced. Its renderer adds
  shadows, SSAO, sky, skinning and glTF on top of that.

## Decision

- **A scene is retained render data, not UI nodes.**
  - `viso-render::scene3d` holds it. Its owner fills a `SceneFrame`:
    - a `Camera` (eye, target, up, near, far, and a perspective field of
      view or an orthographic height). The renderer projects it with
      reversed depth at the aspect of the rect it draws into. An infinite
      `far` (the default) never clips the distance;
    - `Lighting`: one directional light, a hemispheric ambient (sky and
      ground) and exponential fog, linear RGB;
    - a background, straight linear RGBA; a transparent one shows the 2D
      content behind;
    - one batch of `SceneInstance { rows: [[f32; 4]; 3], color }` per mesh
      (a 3×4 affine plus color, 64 bytes), pushed by `MeshId`.
  - The owner hands the frame over with
    `Renderer::swap_scene(slot: SceneSlot, &mut SceneFrame)`. The renderer
    takes the frame and gives back the slot's previous one, which the owner
    clears and refills. A steady scene neither copies nor allocates.
  - A slot owns its persistent targets: a `Bgra8Unorm` color texture (what
    an image texture holds) and a `Depth32Float` depth. It also owns its
    instance buffer, grown in powers of two, and its uniform buffer and
    bind group. Nothing is created per frame. `Renderer::release_scene`
    frees them.
  - Meshes are keyed by a compact `MeshId`. Each holds a vertex buffer of
    `SceneVertex` (32 bytes), a `u16` index buffer (`u32` past 65 536
    vertices) and local bounds.
    - The built-in `Shape`s are cube, sphere, capsule, cylinder, cone, wedge
      and plane. Each spans `[-1, 1]` on every axis, so an instance's linear
      part is the half extents of the body it draws. Each is generated
      procedurally and uploaded the first time it is drawn.
    - Curved surfaces share their vertices around a ring, so they are smooth
      shaded. Hard edges split their vertices.
    - A capsule's caps are half-ellipsoids half the height of its straight
      part, so a standing character's caps stay round under its half
      extents.
    - `Renderer::create_scene_mesh` uploads a user mesh, and
      `release_scene_mesh` frees it.
  - A composite Kit model is a fixed list of parts (shape, local transform,
    tint). The view expands it into the batches at fill time. glTF import
    is a later asset-pipeline decision.
- **A scene draw places the scene in the 2D stream.**
  - `Primitive::Scene(SceneDraw { rect, slot, revision, mask })` draws the
    slot's scene into `rect`.
  - When the renderer takes the draw (on upload, patch or splice), it sizes
    the slot's targets to the rect's device pixels:
    - targets are rounded up to 64-pixel steps, so a resize within a step
      reuses them;
    - the projection is fitted to the used top-left part of the targets;
    - each draw is scissored to that part.
  - It refills the slot only when its frame revision or its pixel size
    changed since the last fill. A refill:
    1. culls each instance's world box (its mesh bounds placed by the
       affine) against the six frustum planes;
    2. packs the survivors contiguously into the instance buffer;
    3. writes the uniform block;
    4. records one instanced draw per mesh with survivors. A batch culled
       whole issues no draw.
  - The draw lowers to an image of the color target (uv = used / allocated
    size). Clip, transform, opacity, mask and z-order are the 2D scene's, as
    for any image.
  - An image entry carries the revision of its texture's content, and the
    revision is part of its damage key. A new scene revision therefore
    redraws exactly the rect, and a patch rewrites the one entry in place.
- **Scene passes run ahead of the frame's 2D passes.**
  - Each slot refilled since it last rendered contributes one pass, before
    the render graph's passes. The pass:
    - targets the slot's color texture, cleared to the background;
    - has a `Depth32Float` attachment cleared to 0 and discarded;
    - draws its instanced draws with `DepthState::REVERSED` and back-face
      culling.
  - It is not a graph node. Its targets are persistent, not transient, and
    it renders only on change: a paused game with a still camera renders no
    scene pass and redraws no 2D pixel.
  - Particles (camera-facing billboards, blended, depth-tested without
    depth writes) and debug shapes join the same pass.
  - `Renderer::take_scene_stats` counts passes, draws, drawn and culled
    instances, and target allocations (§61).
  - The 1.0 lighting is Lambert diffuse plus Blinn-Phong specular and
    hemispheric ambient, linear color, sRGB target. Shadows, SSAO, PBR, sky,
    skinning and MSAA are later decisions. Each is a pass or pipeline added
    to the lane, never a change to the 2D renderer.
- **The RHI takes depth and an instanced mesh draw.**
  - A render pass may name a `DepthAttachment { texture, clear }`, a
    `Depth32Float` render target the size of its color target. The depth is
    transient: cleared when the pass begins and discarded when it ends, so
    it costs no store. On a tile GPU (Metal Apple family) the texture is
    memoryless. A depth pass draws into a texture, and each of its
    pipelines declares a depth state.
  - `PipelineDesc` gains `depth: Option<DepthState { format, compare,
    write }>` and `cull: CullMode` (`None`, `Back`; front faces wind
    counter-clockwise in clip space, y up). `DepthState::REVERSED`
    (`Greater`, writing) draws opaque geometry, and
    `REVERSED_TEST_ONLY` (`GreaterEqual`, no write) draws billboards.
  - `Geometry::InstancedMesh { vertex_buffer, index_buffer, index_format,
    index_offset, index_count, count }` draws `count` instances of an
    indexed mesh. The command's instance buffer and offset carry the
    instances.
  - The scene's vertex and instance layouts are fixed by the built-in
    (`viso_gpu::scene`), so no general vertex-layout API is added:
    - `SceneVertex { position, normal, uv }`, 32 bytes, is the
      pipeline's `instance_schema` (`scene::VERTEX_SCHEMA`), stepped per
      vertex;
    - `SceneInstance { rows: [[f32; 4]; 3], color }`, 64 bytes
      (`scene::INSTANCE_SCHEMA`), is stepped per instance at the
      locations after the vertex attributes;
    - `SceneUniforms`, 160 bytes, is a uniform buffer the draw's bind group
      holds (`Binding::Uniform`).
  - How each backend implements it:
    - Metal: `MTLDepthStencilState`, `setCullMode`, and vertex pulling
      (vertices at buffer 0, instances at 1, uniforms at 2);
    - Vulkan: render passes and framebuffers with a depth attachment, two
      vertex bindings, and a uniform-buffer binding in the shared
      descriptor set layout;
    - D3D12: a DSV heap, `D3D12_DEPTH_STENCIL_DESC`, two input slots, and
      a scene root signature whose uniforms are a root CBV;
    - WebGPU: `depthStencilAttachment` (`discard` store), two vertex
      buffers, and a scene group-0 layout over the whole uniform buffer;
    - GLES 3: a depth texture on the framebuffer, invalidated after the
      pass, instanced attributes, and the uniform block at binding 0.
    - CpuRaster: an `f32` depth buffer per depth texture and a clipped,
      perspective-correct triangle rasterizer with the top-left fill rule,
      shading through `SceneUniforms::shade`, the expression every scene
      program computes. Headless and `embedded-cpu` builds draw the same
      scenes.
  - The 2D pipelines declare no depth and cull nothing, so their code paths
    and costs are unchanged.
- **The shader gets a scene profile.**
  - Shader IR gains `VertexSource::PerVertexInstanced`: the vertex body
    opens with two fetches, `VertexIn v = verts[vid];` then
    `InstanceIn inst = instances[iid];`. `ShaderIr::instance_attributes`
    declares the instance struct, at the locations after the vertex
    attributes. The uniform block is a uniform buffer read by both stages,
    so the fragment may read `u`.
  - The body subset gains `cross`.
  - The scene built-in (`scene_ir`, `PrimitiveKind::Scene`) is written in
    the body subset once and printed to every backend language:
    - MSL: buffers 0, 1 and 2 (vertices, instances, uniforms);
    - WGSL: two vertex structs, `var<uniform>` at `@group(0) @binding(0)`
      on the web and `@binding(3)` beside the textures natively; frozen
      SPIR-V is made from the native form;
    - HLSL: `ATTR0..` across both structs, the uniforms a `ConstantBuffer`
      at `b0`;
    - GLSL ES 3.00: both structs gathered from `layout(location)` inputs,
      the `std140` block at binding 0.
  - The fragment computes `SceneUniforms::shade` exactly, so the CPU
    raster is the reference image. A test draws one scene through the CPU
    raster, the native MSL, and the web WGSL translated to MSL, on the
    Metal device, and requires the three to agree.
  - Custom scene materials are a later decision. The profile is shaped so
    one adds a material block without changing the vertex interface.
- **`GameViewport` is a node whose content is a scene.**
  - `viso-ui` gains `Content::Scene { slot, revision }`.
    - The node store keeps its nodes' scenes, `SceneSlots`, by slot. Each
      slot holds:
      - the `SceneFrame` the node shows and its revision;
      - whatever state the slot's filler keeps beside it.
    - Layout: no intrinsic size. The DSL fills an axis the author did not
      size.
    - Paint: one `Primitive::Scene` of the slot at the content's revision.
    - Semantics: role `image`, labelled by the viewport's `label`.
  - A filler writes a slot's frame and commits it. The commit:
    - bumps the revision;
    - marks the node `PAINT`;
    - queues the frame for the renderer. Before patch and upload, the facade
      swaps each committed frame into the renderer (`Renderer::swap_scene`),
      getting the renderer's previous frame back. A commit therefore hands
      a frame over whole: the filler refills it completely each time.
  - A node that takes other content, or is freed, lets go of its slot, and
    the facade releases the renderer's side.
  - The scene's targets follow the node's device-pixel size, rounded up to
    a multiple of 64 so a resize does not reallocate every frame.
  - The DSL `GameViewport` and the Rust `viso_view::game_viewport(store,
    node, rig)` mark a slot as a viewport. Each frame, after the session's
    ticks and `FrameUpdate`s, the session fills the viewports under its
    root (`FrameClient::fill_scenes`, a walk up from each slot's node), so
    no registration orders them. A fill draws:
    - the camera: the viewport's own `camera:` rig eased by its own `Camera`,
      or else `Stage::view()`. The renderer applies the node's aspect;
    - instances from `GameWorld::extract` at the clock's alpha, terrain
      included (its blocks are `Model::Ground` bodies), each with its
      `Model` resolved per body kind;
    - particles and debug shapes from the `Stage`.
  - Each `Model` is one built-in shape stretched to the body, or a
    composite of shapes laid out at its prefab's size and stretched with
    the body. It is turned so its `-z` points along the body's facing.
  - The fill compares what it read with what it read last. It commits only
    when the World as drawn or the camera moved. A paused game with a still
    camera renders no scene pass and no 2D repaint. A size change re-renders
    the renderer's retained frame without a commit.
  - Slots that are not viewports belong to app code, which fills them
    through `NodeStore::scenes_mut` and `commit_scene`.
  - The viewport is focusable, and a pointer down on it takes the focus.
    Keys reach the game in two cases (§106.2):
    - nothing is focused;
    - the focused node shows a scene.
  - Several viewports of one session render the same World through their
    own scenes, each with its own camera override (`camera:` property)
    or the stage's.
- **Ownership stays one-way.**
  - `render` owns scenes, meshes and the pass, and `ui` names a scene by
    handle.
  - `view` (which already links `behavior`) translates the World into
    scene data.
  - No new crate edge.

## Consequences

- A game screen costs one offscreen pass per viewport per changed frame:
  one instanced draw per mesh kind in view, plus particles. One textured
  quad composites it into the 2D pass. The HUD stays ordinary UI with
  query-driven invalidation.
- The 2D renderer is untouched: depth, vertex layouts and the scene profile
  are opt-in per pipeline.
- Headless runs draw the scene through CpuRaster, which unblocks the
  frame-sheet tool (`viso game … --sheet`) and golden images of game
  scenes.
- The web export still refuses views that run games (ADR 0045). The WebGPU
  backend implements the lane, for when that changes.
- Implementation order:
  1. RHI depth and the instanced mesh draw on all backends, with a test
     that draws two overlapping quads and reads back their depth order;
  2. the shader scene profile;
  3. `SceneFrame`, the meshes, scene slots and their passes;
  4. `Content::Scene` and `GameViewport` with the session fill;
  5. particles, debug shapes and focus.
  Each step lands with its tests.
