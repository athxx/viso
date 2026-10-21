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
  - `viso-render` gains a `Scene` resource with these parts:
    - a camera (view and projection matrices, with reversed-Z
      perspective);
    - one directional light and a hemispheric ambient;
    - a clear color;
    - a list of instance batches, one per mesh, each a dense array of
      `SceneInstance { model: [Vec4; 3], color: Vec4 }` (a 3×4 affine plus
      linear RGBA, 64 bytes, `GpuPod`).
  - The scene owns its instance buffer, rewritten in place once a frame
    through the frame's upload ring. It owns its color and depth targets.
    Nothing is created per frame.
  - Meshes live in a `SceneMeshStore` keyed by a compact `SceneMeshId`:
    - a vertex buffer of position, normal and uv (`f32×8`, 32 bytes);
    - a `u16`/`u32` index buffer;
    - local bounds.
  - Built-in meshes are generated once, procedurally, on first use. A
    composite Kit model is a fixed list of parts (mesh, local transform,
    tint), expanded into the batches at fill time. A user mesh is uploaded
    the same way; glTF import is a later asset-pipeline decision.
- **The scene lane is a render-graph pass.**
  - `PassWork::Scene { scene }` renders a scene into its color target, with
    a `Depth32Float` attachment that is cleared and not stored. It runs
    before the 2D pass that composites it. Its work:
    1. one instanced draw per non-empty batch, opaque;
    2. particles, as camera-facing billboards, blended, depth-tested
       without depth writes;
    3. debug shapes, as lines.
  - The 2D pass composites the color target through the existing
    `Content::Image` path. Clip, transform, opacity and z-order are the 2D
    scene's as for any image.
  - Culling: per-instance sphere against the frustum during fill, on the
    CPU. A batch whose instances are all culled issues no draw.
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
  - `viso-ui` gains `Content::Scene { scene: SceneHandle }`.
    - Layout: fill by default, like an image of no intrinsic size.
    - Paint: one image primitive of the scene's target.
    - Semantics: role `image`, labelled by the viewport's `label`.
  - The scene's targets follow the node's device-pixel size, rounded up to
    a multiple of 64 so a resize does not reallocate every frame.
  - The DSL and Rust `GameViewport` (in `viso-view`, beside `GameSession`)
    attaches its node's scene to the session of the view's root. Each frame,
    after the session's ticks and `FrameUpdate`s, the session fills the
    scene:
    - the camera from `Stage::view()`, with the node's aspect;
    - instances from `GameWorld::extract` at the clock's alpha, `Model`
      resolved per body kind;
    - terrain boxes, particles and debug shapes from the `Stage`.
  - The fill marks the node `PAINT` only when the scene changed: the
    World, the camera or the size moved. A paused game with a still camera
    renders no scene pass and no 2D repaint.
  - The viewport is focusable. While it holds keyboard focus, keys reach
    the game (§106.2, the current no-focus rule widened to the viewport). A
    pointer down on it takes the focus.
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
  3. `Scene`, `SceneMeshStore` and `PassWork::Scene`;
  4. `Content::Scene` and `GameViewport` with the session fill;
  5. particles, debug shapes and focus.
  Each step lands with its tests.
