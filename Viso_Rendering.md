# Viso Rendering — Canonical Architecture, Implementation Plan & Design Specification

> Document status: **Viso 1.0 Draft**  
> Role: **single canonical rendering design + implementation-order document for vibe coding**  
> Scope: `viso-math`, `viso-gpu`, `viso-shader`, `viso-render`, render-facing `viso-text`, `viso-ui`, `viso-widgets`, and platform GPU backends  
> Correctness rule: pixel, color, alpha, coordinate, resource-lifetime, paint-order, and synchronization semantics must never be weakened by optimization.  
> Optimization priority: **frame stability > hot-path CPU > GPU bandwidth/fill-rate > memory/VRAM > cold-path setup cost > implementation complexity.**  
> Construction rule: **foundation first, then drawing, then composition, then effects, then materials, then advanced optimization. Upper layers never become prerequisites of lower layers.**

---

# 0. Vibe Coding Execution Contract

This document is the implementation order. Do not invent a second roadmap.

```text
F0 -> F1 -> F2 -> F3 -> F4
   -> D0 -> D1 -> D2 -> D3
   -> C0
   -> E0 -> E1 -> E2
   -> M0 -> M1
   -> A0
```

For every stage:

```text
1. Audit existing code and assign every module/type to the current or lower layer.
2. Remove reverse dependencies before adding new features.
3. Implement the smallest independently verifiable unit.
4. Run correctness tests, golden/reference tests, formatter, linter, and stage benchmarks.
5. Check CPU, GPU, allocation, upload, and resident/transient memory counters.
6. Make a focused commit for that verified unit.
7. Repeat until the stage Definition of Done is complete.
8. Run the full stage gate.
9. Freeze the external contract of the completed lower layer.
10. Only then enter the next stage.
```

Hard rules:

- Do not implement Material/Glass to discover missing Rect/Clip/Blur foundations.
- Do not use Compute/Bindless/Indirect as a workaround for an inefficient basic renderer.
- Do not expose public Custom Shader/Canvas contracts before the standard renderer ABI is stable enough to support them without back-driving the foundation.
- Do not add a general RenderGraph before real multi-pass requirements exist; begin with the smallest render-pass model required by the current stage.
- Internal optimization may use `unsafe`, zero-copy staging, SIMD, platform-specific resource binding, and backend fast paths only when invariants are explicit and correctness/benchmark gates pass.
- A lower-layer correctness defect is fixed as a foundation defect and must re-pass its gate before upper-layer work continues.

---
# 1. 文档目标

本文定义 Viso 2D/应用渲染内核从基础到外层的唯一实现分层与依赖合同。

它回答以下问题：

```text
Shader 在整个 Renderer 中属于哪一层？
Rect / RRect / Path / Image / Text 到底由谁拥有？
哪些能力必须先实现，哪些效果必须后实现？
哪些数据可以 zero-copy 上传？
哪些 hot path 可以使用 unsafe / SIMD？
哪些 primitive 应 analytic，哪些应 retained mesh，哪些才值得 compute？
Clip / Mask / Opacity / Blend 在什么时候需要 offscreen？
Shadow / Blur / Backdrop / Glass 应建立在哪些基础能力上？
120/144/240Hz 下每帧允许发生什么、禁止发生什么？
```

本文不是 Widget API 清单，也不是视觉材质规范。

- 完整绘画/效果语义在本文对应 D/C/E/M 阶段定义；
- Glass / Frosted / Liquid Glass 的材质语义由 `Viso_Visual_Materials.md` 承接；
- 字体 shaping / glyph representation 由 `Viso_Text_Font_Runtime.md` 承接；
- 本文同时定义 **Rendering Foundation、Core Implementation、Drawing/Composition/Effects 设计与严格施工顺序**。

---

# 2. 核心边界：Drawing 不是 Shader

Viso 必须严格区分：

```text
Drawing Semantics
Renderer Runtime
Shader Programs
GPU Backend
```

它们不是同一个层。

以 `RoundedRect` 为例：

```text
RoundedRect
├── geometry / radius / bounds / transform / clip    -> viso-render
├── retained primitive identity                      -> viso-render
├── instance packing / batching / dirty upload       -> viso-render
├── analytic coverage program                        -> viso-shader
└── buffer / pipeline / command / surface             -> viso-gpu
```

以 `Blur` 为例：

```text
Blur
├── sigma / radius / edge mode / effect bounds       -> viso-render
├── ROI / offscreen / pass reuse / target lifetime   -> viso-render
├── blur kernel                                      -> viso-shader
└── texture / dispatch / barriers / synchronization  -> viso-gpu
```

以 `Glass` 为例：

```text
Glass
├── material semantics                 -> visual material layer
├── backdrop dependency / ROI          -> viso-render
├── blur / distortion / color kernels  -> viso-shader
└── texture/pass/sync                   -> viso-gpu
```

因此：

> **Shader 是绘画系统的执行基础设施，不拥有 Rect、Path、Shadow、Blur、Glass 等高层语义。**

`viso-shader` 不得知道 Widget、Node、ClipChain、Material Widget 或 Layout。

`viso-gpu` 不得知道 Rect、Text、Shadow、Button、Glass。

---

# 3. 唯一依赖方向

Viso Rendering Foundation 的规范依赖链固定为：

```text
F0  Pixel / Math / Color Foundation
             ↓
F1  GPU Foundation
             ↓
F2  Shader Foundation + Typed GPU ABI
             ↓
F3  Retained Render Scene
             ↓
F4  Persistent Data Path / Upload / Batch
             ↓
D0  Rect Baseline
             ↓
D1  Analytic UI Shapes / Border / Line
             ↓
D2  Brush / Gradient / Image
             ↓
D3  Path / Bezier / Fill / Stroke
             ↓
C0  Clip / Mask / Group / Blend
             ↓
E0  Analytic Shadow
             ↓
E1  Offscreen / Blur / ROI / Transient Targets
             ↓
E2  Backdrop / Color Effects / Advanced Blend
             ↓
M0  Frosted / Generic Glass Material
             ↓
M1  Platform Materials / Liquid Glass / HDR Integration
             ↓
A0  Compute Vector / Bindless / Indirect / GPU Culling
```

硬规则：

- `F0~F4` 是地基；
- `D0~D3` 是基础绘画；
- `C0` 是合成基础；
- `E0~E2` 是效果；
- `M0~M1` 是外层材质；
- `A0` 是高级优化，不是基础功能前置；
- 不允许为了实现 Glass 而让 `Rect` 依赖 Effect Graph；
- 不允许为了 GPU Compute Path 而让普通 Button 先经过 Compute Queue；
- 不允许为了统一 API 把最常见 primitive 强制走最昂贵 representation；
- 上层只能依赖已通过 Gate 的下层合同，禁止以“上层实现方便”为理由反向修改地基；
- 一个阶段通过对应 Correctness / Performance / Resource / Cross-backend Gate 后，其对外合同进入冻结状态；
- 后续若发现下层合同本身存在 correctness 缺陷或经 benchmark 证明的结构性性能阻塞，必须先作为 foundation defect 单独修复并重新通过该层 Gate，未重新通过前不得继续向上扩展。

## 3.1 阶段冻结规则

Viso Rendering 的施工模型是单向的：

```text
implement current layer
    ↓
correctness
    ↓
golden image / reference comparison
    ↓
CPU benchmark
    ↓
GPU benchmark
    ↓
memory / resource benchmark
    ↓
cross-backend validation
    ↓
PASS
    ↓
freeze lower-layer contract
    ↓
enter next layer
```

冻结的含义：

```text
public semantic contract       frozen
stable GPU/renderer ownership  frozen
resource lifetime contract     frozen
pixel/color/alpha semantics    frozen
hot-path invariants            frozen
```

实现内部仍可以在不改变上述合同、且 benchmark 证明有收益的前提下继续优化，例如替换 SIMD kernel、改变 buffer suballocation 算法或增加 backend-specific fast path。

---

## Detailed Design — 最终总方案：Adaptive Multi-Lane Retained Renderer

Viso 不选择“所有东西都 SDF”“所有东西都 tessellation”“所有东西都 GPU compute”中的任何一种。

最终模型：

```text
UI / Canvas / Text / SVG / Game
              ↓
       Retained Paint IR
              ↓
     Primitive Classifier
              │
   ┌──────────┼───────────┬──────────────┬────────────┬─────────────┐
   ▼          ▼           ▼              ▼            ▼             ▼
Analytic   Atlas       Vector Mesh    Vector Compute  Mesh/3D    Effect/Material
Lane       Lane        Lane           Lane            Lane       Lane
   │          │           │              │            │             │
   └──────────┴───────────┴──────────────┴────────────┴─────────────┘
                              ↓
                    Order-safe Batch Planner
                              ↓
                       Effect / Clip Planner
                              ↓
                     Reusable RenderGraph Plan
                              ↓
                       Backend Command Encode
                              ↓
                    Metal / D3D12 / Vulkan / WebGPU
```

选择原则：

```text
简单 UI 几何
-> analytic instance

图片 / glyph / sprite
-> atlas/image instance

稳定任意 vector path
-> retained cached geometry

大量动态 path / creative scene
-> GPU compute vector lane（能力与 benchmark 允许时）

真正需要邻域采样/目标读取的效果
-> effect/offscreen lane

普通 opacity / color matrix / tint
-> 尽量 fuse，不创建 offscreen
```

Representation decision 挂在 retained primitive / paint chunk 上，不在每个 fragment、每个 glyph 或每帧重新决策。

---

# 4. Crate 所有权

## 4.1 `viso-math`

拥有 allocation-free 基础数学：

```text
Vec2 / Vec3 / Vec4
Mat2 / Mat3 / Mat4
Affine2
Point / Size / Rect / Insets
Aabb / Ray / Plane
```

Renderer 2D 热路径优先使用：

```text
Affine2
Vec2
Rect
```

普通 2D transform 禁止仅因为 API 统一而全部使用 `Mat4`。

`viso-math` 不拥有：

```text
Color
Brush
Path
Clip
Primitive
GPU ABI
Shader layout
```

## 4.2 `viso-gpu`

只拥有极薄 RHI：

```text
Device
Queue
Surface
Buffer
Texture
TextureView
Sampler
RenderPipeline
ComputePipeline
CommandEncoder
RenderPass
ComputePass
Fence / Timeline / Sync
```

禁止 UI 概念进入 `viso-gpu`。

## 4.3 `viso-shader`

拥有：

```text
shader parser
shader type system
Shader IR
reflection
instance/uniform/storage schema
backend codegen
pipeline ABI validation
PipelineManifest input
```

不拥有：

```text
Paint order
PrimitiveStore
Clip semantics
Effect graph
Widget material
Render damage
```

## 4.4 `viso-render`

拥有：

```text
retained paint scene
primitive stores
transform / bounds
brush / image references
clip / mask semantics
paint order
batch planner
persistent instance residency
GPU upload plan
path geometry cache
effect planner
render graph
transient target planner
damage/culling
frame packet
```

## 4.5 `viso-text`

生成稳定的 `GlyphRun` / representation handle，最终作为 Render primitive 进入 `viso-render`。

`viso-render` 不重新 shape 文字。

## 4.6 `viso-ui` / `viso-widgets`

负责把 Widget/Node paint state lower 成 retained render primitive。

它们不拥有 GPU buffer、pipeline cache 或 texture barrier。

---

## Detailed Design — Render RHI 不应该暴露 UI 概念

`viso-gpu`：

```text
Buffer
Texture
Sampler
Pipeline
ResourceTable/BindGroup
RenderPass
ComputePass
CommandEncoder
Fence
Surface
```

`viso-render`：

```text
Primitive
Brush
Path
Clip
Effect
Batch
RenderGraph
Material bridge
```

`viso-ui`：

```text
Node
Layout
Style
Paint invalidation
```

边界必须保持。

---

## Detailed Design — 与 Visual Material Runtime 的边界

本文拥有：

```text
Backdrop capture
Blur planner
Effect graph
ROI
Transient resources
Blend/composite
GPU material primitives
```

`Viso_Visual_Materials.md` 拥有：

```text
Glass/Frosted public semantic
Apple system material mapping
Liquid Glass behavior
MaterialGroup
accessibility material adaptation
native material island
platform fallback
```

两份规范不能重复发明两套 blur/render graph。

---

## Detailed Design — 与 Text Runtime 的边界

`Viso_Text_Font_Runtime.md`：

```text
font resolve
shaping
glyph representation
glyph atlas
text correctness
```

本文：

```text
GlyphRun placement
atlas sample pipeline
clip/blend/effect composition
GPU batch
```

文字不能因为进入 Renderer 又重新 shape。

---

## Detailed Design — 与 Shader Runtime 的边界

`viso-shader`：

```text
parse/type/IR
backend codegen
reflection
pipeline ABI
```

`viso-render`：

```text
choose pipeline family
prepare resource bindings
instance data
render graph
batch
```

---

# 5. F0 — Pixel / Math / Color Foundation

这是所有绘画之前必须冻结的地基。

没有 F0，不允许开始设计高级 Effect。

## 5.1 坐标空间

必须区分：

```text
Logical Space        dp / logical unit
Device Space         physical pixel
Local Space          primitive-local
Parent Space
World / Window Space
Surface Space
Clip Space           GPU normalized/device clip
Texture Space        normalized or texel space
```

不得把 `device_pixel_ratio` 隐式散落在 Widget 中。

规范转换链：

```text
Local
  ↓ Affine2
World / Window Logical
  ↓ DeviceScale
Physical Device Pixel
  ↓ Backend viewport transform
GPU Clip Space
```

## 5.2 Surface origin

Viso Render IR 使用统一二维坐标语义：

```text
origin = top-left
+x = right
+y = down
```

各 backend 的 NDC、texture origin、projection 差异只能在 GPU/backend lowering 内处理。

## 5.3 Rect 边界规则

所有 axis-aligned bounds 使用一致的半开区间语义：

```text
[min_x, max_x)
[min_y, max_y)
```

用于：

```text
clip
intersection
damage
culling
pixel bounds
atlas allocation
```

避免邻接 primitive 在边界处发生 double-hit / gap 的语义分裂。

## 5.4 Color canonical representation

Renderer 内部 canonical blend representation：

```text
linear-light
premultiplied alpha
```

资源输入可以是：

```text
sRGB
Display P3
linear sRGB
extended linear / HDR
```

但进入 blend/effect pipeline 前必须有明确 color-space transform。

禁止在 sRGB 编码空间直接执行应在线性光空间完成的：

```text
blur
gradient interpolation
lighting-like effect
most compositing math
```

## 5.5 Alpha

默认：

```text
premultiplied alpha
```

内部禁止同一 pipeline 一部分按 straight alpha、一部分按 premultiplied alpha。

Shader 输出、texture format metadata 与 blend state 必须一致。

## 5.6 Anti-aliasing coverage

Coverage 统一定义：

```text
coverage ∈ [0, 1]
output = premultiplied_color * coverage
```

Analytic primitive、Path AA、glyph mask 最终都必须能映射到同一 coverage/composite 语义。

## 5.7 Pixel snapping

提供底层策略：

```text
PixelSnap::None
PixelSnap::Position
PixelSnap::Bounds
PixelSnap::Stroke
```

默认 Widget 只在真正需要 pixel alignment 时使用 snapping。

不得为了“锐利”而对动画中的所有 transform 强制 round 到整数像素。

## 5.8 Hairline

`Hairline` 是独立语义，不等价于 `stroke_width = 1 logical unit`。

其目标是：

```text
在当前 device scale 下维持约一个 device-pixel coverage 的视觉线宽。
```

Hairline 必须正确处理旋转、非整数 scale 与高 DPI。

## 5.9 非法数值

进入 retained render state 的 public geometry 必须验证：

```text
NaN
±Inf
negative extent where illegal
singular transform where operation requires inverse
```

Release 热路径不得处处重复 `is_finite()`；验证应在 state commit / cold boundary 完成。

## 5.10 Degenerate geometry

以下必须有确定语义：

```text
zero-area rect
zero-length line
coincident path points
zero radius
radius > half extent
singular transform
empty clip
empty mask
```

优先 fast reject，不 panic，不产生未定义 GPU 数据。

---

## Detailed Design — Anti-Aliasing 合同

Viso 不默认对整个 UI surface 开 4x/8x MSAA。

### 9.1 简单 analytic shape

使用 device-pixel-aware analytic coverage：

```text
signed distance / edge equation
+
derivative/fwidth or equivalent pixel footprint
+
premultiplied coverage
```

目标：

```text
1px transition band
stable under subpixel translation
no geometry fringe rebuild
```

### 9.2 Vector path

根据 path 类型选：

```text
simple convex
-> direct cached geometry + coverage edge

general stable path
-> cached tessellation / coverage representation

self-intersection / complex clip
-> stencil/coverage-mask or compute path lane

dense dynamic vector scene
-> tiled GPU compute coverage
```

### 9.3 MSAA

MSAA 是：

```text
3D
certain mesh/vector passes
backend-specific measured win
```

而不是普通 2D UI 的全局默认税。

---

## Detailed Design — Canonical Alpha / Color 合同

GPU 内部 canonical：

```text
premultiplied alpha
```

禁止在普通 blend 热路径反复：

```text
unpremultiply
blend
premultiply
```

颜色需要明确 color-space metadata。

建议工作模型：

```text
Public Color / Image Color Profile
       ↓
resolve to target working space
       ↓
linear-light operations where required
       ↓
GPU premultiplied representation
       ↓
output transfer function
```

SDR 常规 surface 不要求全部变成 RGBA16F；只有：

```text
HDR
wide-gamut effect chain
high-precision intermediate
```

才使用 FP16/更高成本 target。

---

# 6. F1 — GPU Foundation

F1 的目标不是“能画很多效果”，而是建立最小、稳定、可验证的 GPU substrate。

## 6.1 必需对象

```text
GpuDevice
GpuQueue
GpuSurface
GpuBuffer
GpuTexture
GpuTextureView
GpuSampler
GpuRenderPipeline
GpuComputePipeline
GpuCommandEncoder
GpuRenderPass
GpuComputePass
GpuFence / Timeline
```

命名可以在 Rust API 收敛，但职责不可混合。

## 6.2 Buffer usage

至少区分：

```text
Vertex
Index
Uniform
Storage
Upload
Readback
Indirect
```

一个资源可以具有组合 usage，但 backend 必须验证合法性。

## 6.3 Texture formats baseline

基础至少覆盖：

```text
R8Unorm
RG8Unorm
RGBA8Unorm
RGBA8UnormSrgb
BGRA8Unorm
BGRA8UnormSrgb
RGBA16Float
Depth / DepthStencil backend-required format
```

用途：

```text
sampled
render attachment
storage where supported
copy src/dst
```

## 6.4 Surface

Surface 必须支持：

```text
acquire
present
resize
DPI change
format/color-space change
occlusion/minimize
out-of-date recovery
device loss recovery hook
```

## 6.5 Resource generation

所有 GPU resource handle 必须是 generation-safe。

禁止：

```text
stale BufferId
stale TextureId
stale PipelineId
```

在设备重建后继续命中新的对象。

## 6.6 Deferred destruction

CPU 释放引用不代表 GPU 已不再使用。

必须存在：

```text
retire queue
in-flight epoch/fence
safe reclamation
```

禁止资源销毁依赖“希望这一帧已经执行完”。

## 6.7 UMA 与 discrete GPU

公开模型统一，内部策略不同：

```text
UMA / Apple Silicon / mobile SoC
    persistent shared/mapped fast path

discrete GPU
    mapped upload/staging
    -> transfer/copy
    -> device-local resource
```

不得为了伪造“物理 zero-copy”而放弃 device-local 性能。

## 6.8 Backend hot path

Release target 应静态选定 backend。

禁止每个 primitive：

```rust
Box<dyn GpuBackend>::draw(...)
```

在热路径做 virtual dispatch。

---

## Detailed Design — Buffer / Texture Resource Lifetime

必须区分：

```text
Persistent
    atlas
    retained mesh
    instance pool
    image texture

Frame
    small constants
    upload ring slice
    indirect args

Transient RenderGraph
    blur intermediate
    temporary mask
    offscreen layer
```

Transient 资源必须允许 lifetime aliasing / pool reuse。

---

## Detailed Design — Platform Fast Paths

统一语义，backend 可不同实现。

### Apple / Metal

可以针对：

```text
unified memory upload
argument/resource table
tile-based attachment behavior
memoryless/transient attachment
native Liquid Glass/Frosted composition
```

做 fast path。

### Windows / D3D12

可以针对：

```text
upload heap
descriptor heap
pipeline state cache
resource barriers
indirect draw
```

优化。

### Linux / Vulkan

可以针对：

```text
host-visible staging
descriptor indexing
transient attachments
pipeline cache
subpass/dynamic rendering strategy
```

优化。

### WebGPU

优先：

```text
persistent buffers
batched writes/staging
binding arrays where capability exists
compute vector/effect only when supported and measured
```

Core 不为了 WebGPU 的最低能力限制 Native backend。

---

# 7. F2 — Shader Foundation 与 Typed GPU ABI

## 7.1 Release 不运行时临时编译标准 shader

标准 pipeline：

```text
Viso Shader Source
        ↓ build-time
Parser / Type Checker
        ↓
Typed Shader IR
        ↓
Reflection + Layout Validation
        ↓
Backend Codegen
        ↓
MSL / DXIL / SPIR-V / WGSL or backend artifact
        ↓
PipelineManifest
```

Release 标准绘制路径不得因为第一次出现某个 Button 才解析/编译 shader source。

## 7.2 最小 Shader Surface

基础只要求：

```text
vertex
fragment
uniform
instance
storage
texture
sampler
varying
```

Compute 在 F2 可以具备语言能力，但普通 D0~D3 绘画不得依赖 compute 才能成立。

## 7.3 Typed GPU layout

GPU 数据必须通过显式 ABI：

```rust
#[repr(C)]
#[derive(GpuPod)]
struct SolidRectInstance {
    rect: [f32; 4],
    color: [f32; 4],
    transform_id: u32,
    flags: u32,
    _padding: [u32; 2],
}
```

要求：

```text
no implicit padding ambiguity
no pointer
no reference
no bool ABI ambiguity
no usize/isize
no enum layout assumption
```

Compile-time 生成/验证：

```text
size
alignment
member offset
backend binding layout
shader reflection compatibility
```

## 7.4 Zero-copy 的准确语义

Viso 的 zero-copy 指：

> **CPU 数据路径不为了 GPU 上传进行无意义的中间序列化和重复拷贝。**

允许：

```text
&[GpuPod]
  ↓ typed byte view
mapped upload range
  ↓
GPU-visible memory / transfer
```

禁止：

```text
Vec<Instance>
→ Vec<f32>
→ Vec<u8>
→ temporary staging Vec
→ backend copy
```

Discrete GPU 到 device-local resource 的 DMA 不被错误宣传为“零物理拷贝”。

## 7.5 Pipeline variant

不要建立一个巨大 Uber Shader。

基础 pipeline families：

```text
SolidRect
AnalyticRRect
AnalyticEllipse
AnalyticLine
Image
Gradient
PathFill
PathStroke
MaskComposite
```

Variant key 只能包含真正改变 pipeline/state/code 的维度。

动态参数应进 instance/uniform，而不是全部变成 pipeline permutation。

---

## Detailed Design — Typed GPU ABI

禁止：

```text
Rust struct 某段内存碰巧符合 shader layout
```

必须使用明确 ABI：

```rust
#[repr(C)]
#[derive(GpuPod)]
struct SolidRectInstance {
    rect: [f32; 4],
    color: [f32; 4],
    transform_id: u32,
    flags: u32,
    _pad: [u32; 2],
}
```

`GpuPod` derive 必须验证：

```text
fixed-width scalar only
known alignment
known offsets
no reference
no pointer
no usize/isize
no uninitialized padding
Copy
```

并生成 shader reflection/layout descriptor。

---

## Detailed Design — Zero-copy 的准确含义

Viso 追求的是：

> **CPU Paint Data -> GPU Upload Data 不进行无意义的中间序列化与重复 copy。**

不是承诺所有离散 GPU 都“物理零 copy”。

Canonical：

```text
typed &[GpuPod]
       ↓
directly write mapped upload/ring memory
       ↓
UMA backend:
    GPU may consume shared memory directly

discrete backend:
    DMA/copy dirty range to device-local resource
```

不得：

```text
QuadInstance
-> Vec<f32>
-> Vec<u8>
-> staging Vec<u8>
-> mapped buffer
```

这种多层转换。

---

## Detailed Design — Pipeline Manifest / Pipeline Cache

参考 Impeller 的 predictable philosophy：

```text
known standard pipelines
-> build enumerated

app custom shaders
-> build compiled/reflected

runtime
-> cache PSO/pipeline objects
```

Baseline primitive pipeline 在 Device 初始化阶段或进入首屏前预热。

非常少见的 variant 可以 background/lazy create，但：

> **不得在需要当前帧立即展示的 animation hot path 同步做昂贵 pipeline compile。**

---

## Detailed Design — 控制 Pipeline Variant Explosion

不要把以下全部做 specialization：

```text
color
radius
shadow color
opacity
gradient angle
```

这些是数据。

只有真正改变：

```text
fixed function state
resource layout
shader family
sample count
depth/stencil behavior
```

才进入 pipeline key。

---

# 8. F3 — Retained Render Scene

Viso 不是每帧重录全部 Canvas command 的 immediate renderer。

## 8.1 Retained Paint IR

UI commit 后形成稳定状态：

```text
Widget / Node
    ↓ paint lowering
Retained Paint Primitive
    ↓
PrimitiveStore
```

没有变化时：

```text
0 primitive reconstruction
0 path retessellation
0 brush reconstruction
0 string lookup
```

## 8.2 Typed IDs

至少需要概念身份：

```text
PrimitiveId
TransformId
BrushId
ClipId
ImageId
GeometryId
RenderChunkId
```

使用 fixed-width dense/generational ID。

禁止把稳定身份建立在：

```text
pointer
usize
Rust object address
```

之上。

## 8.3 Stores

建议：

```text
PrimitiveStore
TransformStore
BrushStore
ClipStore
GeometryStore
ImageRefStore
EffectStore
```

热字段应紧凑，冷字段进入 side table。

## 8.4 Transform 与 primitive 分离

纯移动/动画：

```text
Transform changed
→ TransformStore dirty
```

不应该：

```text
rebuild Rect geometry
rebuild Brush
retessellate Path
```

## 8.5 Bounds

每个 primitive 至少维护：

```text
local_bounds
world_bounds
clip_bounds
paint_bounds
effect_bounds where applicable
```

其中：

```text
paint_bounds
= geometry bounds
+ stroke inflation
+ filter-required inflation
```

Effect 阶段再计算严格 ROI。

## 8.6 Paint order

绘制顺序属于正确性合同。

Batch Planner 只能在不改变可观察结果的范围内 reorder。

不能为了减少 draw call 任意按 texture/pipeline 全局排序透明 UI。

---

## Detailed Design — Paint IR：Retained，不是每帧重录命令流

标准 Widget 和普通 Component 不应该每帧重新生成完整 Canvas command list。

Canonical：

```text
Node
  ↓
PaintChunk
  ↓
stable PrimitiveId ranges
  ↓
PrimitiveStore
  ↓
only dirty fields/ranges changed
```

建议内部身份：

```text
PrimitiveId
PaintChunkId
BrushId
PathId
TransformId
ClipChainId
EffectChainId
ImageId
MeshId
MaterialId
```

热路径 ID 使用固定宽度整数；禁止把 `usize`、pointer、native handle 作为稳定身份。

每个 Primitive 至少拥有独立 revision plane：

```text
GeometryRevision
PaintRevision
TransformRevision
ClipRevision
ResourceRevision
EffectRevision
VisibilityRevision
```

一个 opacity 变化不得逼迫 Path 重新 tessellate。

---

## Detailed Design — PrimitiveStore 使用按类型紧凑存储

推荐：

```text
PrimitiveStore
├── SolidQuadStore
├── AnalyticShapeStore
├── ImageStore
├── GlyphRunStore
├── VectorPathStore
├── MeshStore
├── ClipStore
├── EffectStore
└── CustomStore
```

不要：

```text
Vec<Box<dyn Primitive>>
```

在每个可见 primitive 上做虚调用。

Public 层可以对象化；Render hot storage 使用 dense typed arrays / indexed SoA / compact AoS。

---

## Detailed Design — Bounds

所有 retained primitive 必须维护 conservative paint bounds：

```text
geometry bounds
+ stroke expansion
+ shadow/effect outsets
```

用于：

```text
culling
damage
ROI effect
hit-test helper
backdrop dependency
```

Bounds 计算不应重复解析 Path。

---

# 9. F4 — Persistent Data Path / Upload / Batch

这一层是性能地基。

## 9.1 Persistent Instance Pool

稳定 primitive 获得稳定 GPU/CPU instance slot：

```text
PrimitiveId
    ↓
InstanceSlot
    ↓
Persistent Instance Pool
```

一个 hover 改变：

```text
slot 417 dirty
```

而不是：

```text
rebuild/upload all 100,000 instances
```

## 9.2 Frame Upload Ring

Transient upload 使用 frame-ring：

```text
Frame N region
Frame N+1 region
Frame N+2 region
...
```

通过 fence/epoch 回收。

热路径允许：

```text
persistent mapping
unsafe pointer bump
aligned typed write
```

但必须由受控模块封装。

## 9.3 Dirty Range Coalescer

多个相邻 dirty slot：

```text
417
418
420
421
```

应合并成少量 upload range，避免大量微小 backend copy command。

## 9.4 Frame Arena

每帧临时 CPU 数据使用 bump/frame arena：

```text
visible chunk list
batch scratch
clip scratch
small pass descriptors
sort/radix scratch
```

帧结束 O(1) reset。

禁止热路径为每个 primitive `Vec::new()` / `Box::new()`。

## 9.5 HashMap 边界

HashMap 可以用于：

```text
cold cache build
resource lookup
pipeline creation
asset registration
```

per-primitive traversal 应优先：

```text
dense ID
array/indexed store
small fixed lookup
sorted/radix keys
```

## 9.6 Order-safe batching

Batch key 可能包含：

```text
pipeline family
blend state
clip realization
texture binding group
render target/pass
```

但 batching 必须保留 order barriers。

目标不是“draw call 最少”，而是：

> **在正确绘制顺序内最大化连续 compatible instances。**

---

## Detailed Design — Render Chunk：批处理与增量更新的中间单位

单 Node 过细，整棵树过粗。

Viso 使用稳定 `PaintChunk` / `RenderChunk`：

```text
subtree/widget paint output
       ↓
one or more contiguous primitive ranges
       ↓
batch spans
```

Chunk 记录：

```text
order range
bounds
primitive ranges
pipeline/material summary
clip chain
effect dependencies
revision
```

局部节点变动只重建相关 chunk。

---

## Detailed Design — Batching

默认：

```text
order-preserving adjacent batching
```

`BatchKey` 最低包含：

```text
pipeline family / variant
render target
blend state
clip mode
sample count
color target class
resource table / texture set when required
depth/stencil class
```

不要为了 draw-call 数字破坏视觉顺序。

只有明确标记：

```text
opaque
reorder-safe
no destination dependency
```

的 span 才可以扩大 reorder window。

---

## Detailed Design — Draw Call 优化顺序

优化优先级：

```text
1. eliminate unnecessary pass/layer
2. eliminate unnecessary upload
3. retain geometry/instances
4. reduce pipeline switch
5. reduce texture binding switch
6. merge adjacent draw
7. optional multi-draw/indirect
```

不是：

```text
先为了 1 个 draw call 做全局排序
```

GPU pass/bandwidth 通常比少几个轻量 draw 更重要。

---

## Detailed Design — Persistent Instance Pool + Upload Ring

稳态 UI 不能每帧重新创建 instance buffer。

推荐：

```text
Persistent GPU Instance Pool
          ↑
      dirty ranges
          ↑
Typed Upload Ring / Staging Arena
```

只有变化的 range 上传。

Backend 根据硬件选择：

```text
unified memory
-> direct mapped/shared fast path when benchmark beneficial

discrete GPU
-> staging ring + copy to device-local

WebGPU
-> persistent resource + tuned queue/staging path
```

Frame transient 数据使用 ring；retained static geometry 使用长期 device resource。

---

## Detailed Design — Dirty Range 合并

例如：

```text
instance 10 changed
instance 11 changed
instance 12 changed
instance 400 changed
```

上传：

```text
range 10..13
range 400..401
```

而不是 4 次 API call。

Range coalescer 使用固定容量 scratch/arena；稳态不 heap allocate。

---

## Detailed Design — Frame Arena

Paint/Batch/Graph 编译临时数据使用 frame arena：

```text
bump allocation
bulk reset
no per-object free
```

不允许每个 primitive：

```text
Box
Vec
String
HashMap
```

临时分配。

大对象/长期对象进入 persistent cache，不放 frame arena。

---

## Detailed Design — 多线程模型

推荐：

```text
UI/Main Thread
    state/layout/invalidation
          ↓
Paint ChangeSet
          ↓
Worker Pool
    path preprocess
    tessellation
    heavy effect metadata
    image work
          ↓
staged result
          ↓
Render Thread / GPU Owner
    commit validated ranges
    batch/graph patch
    encode/submit
```

禁止 renderer hot state 在多个 worker 之间通过细粒度 mutex 共享。

使用：

```text
revisioned jobs
immutable input snapshots
SPSC/MPSC handoff where appropriate
frame-boundary commit
```

旧 revision 结果自动丢弃。

---

## Detailed Design — Pipeline/Resource Cache Key

禁止 hot path string。

所有 key：

```text
PipelineId
BrushId
SamplerId
TextureId
ClipChainId
EffectChainId
TargetId
```

最终 batch key 允许 packed integer representation。

---

## Detailed Design — HashMap 只能留在冷路径

允许：

```text
resource interning
pipeline lookup on miss
cache construction
asset resolve
```

不允许：

```text
for every primitive in every frame:
    HashMap<String, ...>
```

稳态 traversal 使用 dense ID + array indexing。

---

# 10. D0 — Rect Baseline

D0 是第一个完整可见 renderer。

只实现：

```text
Solid Rect
Affine2 transform
Opacity
SrcOver
Rect Scissor
Surface present
```

## 10.1 SolidRect 主路径

```text
shared unit quad
+
SolidRectInstance
+
SolidRect pipeline
```

不为每个 Rect 建 4 个独立 vertex buffer。

## 10.2 D0 热路径目标

稳定场景：

```text
0 heap alloc per primitive
0 string lookup
0 global HashMap lookup per primitive
0 per-primitive backend virtual dispatch
0 shader compile
0 full-scene upload
```

## 10.3 D0 验收场景

至少：

```text
1 rect
10k rect
100k rect synthetic
large scrolling list
single hover dirty
window resize
DPI change
surface recreate
```

测试指标记录：

```text
CPU build time
CPU encode time
uploaded bytes
draw calls
pipeline switches
alloc count
GPU frame time
```

绝对时间由 reference hardware profile 建 baseline，不把某个 GPU 的毫秒数写成跨平台 ABI。

---

# 11. D1 — Analytic UI Shapes

加入：

```text
RoundedRect
per-corner radius
RoundedSuperellipse
Circle
Ellipse
Capsule
Line
RectBorder
RRectBorder
Circle/Ellipse Stroke
```

## 11.1 Representation

普通 UI shape 默认：

```text
shared bounding quad
+
compact instance
+
analytic coverage
```

而不是默认 tessellation。

## 11.2 Radius 规范化

Per-corner radius 超出可用尺寸时必须按统一算法 normalize，禁止不同 widget 自己 clamp。

## 11.3 Border alignment

支持：

```text
Inside
Center
Outside
```

Bounds inflation 必须与 stroke alignment 一致。

## 11.4 Line cap / join 基础

Line/Polyline 基础语义：

```text
cap: butt / round / square
join: miter / bevel / round
miter_limit
```

复杂 Path Stroke 在 D3 完成。

---

## Detailed Design — Analytic Primitive Lane：普通 UI 的主力

### 7.1 Unit Quad + typed instances

简单 UI 不为每个 Rect 创建独立 vertex buffer。

全局共享：

```text
4 vertices
6 indices
```

实例描述 geometry / paint：

```text
Shared Unit Quad
      +
N × Typed Instance
      ↓
Instanced Draw
```

最低 primitive family 建议：

```text
SolidRect
AnalyticRRect
AnalyticEllipse
AnalyticLineArc
DecoratedShape
```

不要让所有 Rect 都走一个“支持所有功能”的巨大 shader。

最常见的纯色 Rect 应有最短 branch-free fast path。

### 7.2 为什么不是所有简单 shape 都 tessellate

圆角矩形按钮如果每次 radius/size 动画都重新生成 vertex/index：

```text
CPU geometry churn
allocator pressure
upload bandwidth
batch fragmentation
```

Analytic shape 只更新少量 instance fields：

```text
rect
radius
border
color
transform
```

GPU 根据局部坐标求 coverage。

### 7.3 SDF/implicit 只用于适合它的 primitive

Analytic/SDF 适合：

```text
RRect
circle
ellipse
capsule
line segment
arc
简单 icon / badge
border
simple inner/outer shadow
```

不要求任意复杂 SVG/Path 都转换成 SDF。

---

## Detailed Design — Analytic Shape Shader 需要分级，而不是万能 Shader

建议至少：

```text
Tier A
    SolidRect

Tier B
    RRect / Circle / Ellipse

Tier C
    Fill + Border + Gradient

Tier D
    Expanded quad + analytic simple shadow

Tier E
    Specialized custom analytic primitive
```

原因：

> **最常见 primitive 不应该支付最复杂 primitive 的 ALU、register pressure 和分支成本。**

Pipeline family 在 build-time 枚举；动态颜色、尺寸、hover 等仍只是 instance data。

---

# 12. D2 — Brush / Gradient / Image

## 12.1 Brush

基础：

```text
Solid
LinearGradient
RadialGradient
SweepGradient
ImagePattern
```

## 12.2 Gradient stops

小 stop count 可使用 inline compact data / uniform-like path。

较大 stop count 进入共享 gradient resource/table。

禁止每个 gradient 每帧重新创建 texture。

## 12.3 Gradient interpolation

默认颜色插值应遵循明确的 color-space policy，不能偶然由 texture format 决定。

## 12.4 Image

支持：

```text
Image
ImageRect
source rect
destination rect
fit
alignment
opacity
```

Sampling：

```text
Nearest
Linear
MipmapLinear where available/appropriate
```

## 12.5 Image edge behavior

明确：

```text
Clamp
Repeat
Mirror
```

Atlas/sprite sampling 必须防止相邻 texel bleeding。

## 12.6 Nine-slice / Tile / Sprite Atlas

在基础 ImageRect 稳定后加入：

```text
NineSlice
Tile
Sprite
Atlas region
```

它们复用 Image pipeline family，不建立独立高成本体系。

---

## Detailed Design — Fill / Brush

Canonical：

```text
Brush
├── Solid
├── LinearGradient
├── RadialGradient
├── SweepGradient
├── ImagePattern
└── ShaderBrush
```

GPU 内部使用 premultiplied alpha。

### 10.1 Gradient stop 策略

不要为每个 gradient 默认绑定一块独立 uniform buffer。

推荐：

```text
2-stop common gradient
-> inline instance colors

small stop count
-> compact shared stop table

many stops / expensive interpolation
-> cached 1D Gradient LUT Atlas
```

LUT 的 key 至少包含：

```text
stop colors
stop offsets
interpolation space
extend/tile mode
target color profile class
```

静态 gradient 只建立一次。

---

## Detailed Design — Image / Sprite / Atlas Lane

支持：

```text
Image
ImageRect
NineSlice
Sprite
TextureAtlas
TiledImage
ImagePattern
External/Video texture
```

资源策略：

```text
small immutable UI image
-> atlas candidate

large image / frequently replaced image
-> standalone texture

video / camera
-> external texture / platform image path

heavy downscale
-> mipmaps
```

Image sampling 不应创建 per-widget sampler；Sampler 使用 interned `SamplerId`。

---

# 13. D3 — Path / Bezier / Fill / Stroke

General Path 在 Rect/UI shape 稳定后实现。

## 13.1 Path commands

最小：

```text
move_to
line_to
quad_to
cubic_to
close
```

Arc 等 API 可 lower 为 canonical path segment 或专用 representation。

## 13.2 Path storage

建议紧凑分离：

```text
PathCommand tags
+
packed f32 coordinates
```

避免每个 segment 都是带 vtable/Box 的对象。

## 13.3 Fill rule

必须：

```text
NonZero
EvenOdd
```

## 13.4 Stable path default lane

普通稳定 Path：

```text
Path
→ worker flatten/preprocess
→ tessellate
→ retained GeometryId
→ cached GPU geometry
```

颜色/opacity/transform 变化不得触发重新 tessellate。

## 13.5 Stroke

完整：

```text
width
alignment where semantically supported
cap
join
miter_limit
dash pattern
dash offset
hairline
```

## 13.6 SIMD 候选

Path preprocessing 可以使用 SIMD：

```text
bounds
flatness evaluation
segment transform
rect intersection
point classification
stroke preprocessing
```

标量实现作为 correctness oracle。

---

## Detailed Design — Stroke

Stroke 不能只是 `Path + width` 的模糊约定。

合同至少包括：

```text
width
alignment
cap
join
miter_limit
dash_array
dash_offset
```

普通 Rect/RRect border 不进入 general path stroker；Analytic Lane 直接通过 outer/inner distance 求边框 coverage。

General Path stroke：

```text
PathRevision
+
StrokeStyleRevision
        ↓
worker preprocess
        ↓
retained stroke geometry / segment representation
```

只有 path 或 stroke geometry 变化时重新处理。

颜色变化不重建 stroke geometry。

---

## Detailed Design — Vector Path 数据结构

Path source 建议紧凑表示：

```text
PathArena
├── tags: compact command stream
└── points: tightly packed f32 coordinates
```

命令至少：

```text
MoveTo
LineTo
QuadTo
CubicTo
Conic/Arc lowering
Close
```

Fill rule：

```text
NonZero
EvenOdd
```

Path 创建阶段同时维护/异步计算：

```text
bounds
segment count
convexity hint
simple-shape recognition hint
complexity score
```

不要每次 render 再扫描整个 command stream。

---

## Detailed Design — Vector Mesh Lane：稳定 Path 的默认 general-path 路径

稳定 SVG/icon/chart/path：

```text
Path
  ↓
worker flatten/tessellate
  ↓
cached local-space geometry
  ↓
GPU vertex/index buffer
  ↓
reuse across frames
```

重点：

- Geometry 与颜色/opacity 分离；
- transform-only 动画不 retessellate；
- scale 只有超过 flatness/quality bucket 才重新生成；
- 使用 hysteresis，避免 zoom 临界值抖动；
- 小 geometry 优先 16-bit index，超过范围使用 32-bit；
- static device-local geometry 不跟 frame upload ring 混在一起。

---

## Detailed Design — SVG

SVG 是输入格式，不是每帧 XML DOM renderer。

Build/runtime：

```text
SVG bytes
  ↓
parse
  ↓
normalized vector scene
  ↓
Path/Brush/Stroke
  ↓
cached Render IR
```

静态 asset 可以在 build-time 预解析部分 metadata/scene。

Runtime 动态 SVG 解析放 worker。

---

## Detailed Design — Path / SVG Cache

Key：

```text
content hash / ResourceRevision
geometry/style revision
quality/tessellation bucket
```

颜色等可以与 geometry 分离时，不应让换颜色失效 geometry cache。

---

# 14. C0 — Clip / Mask / Group / Blend

Effect 之前先完成合成基础。

## 14.1 Clip ladder

```text
Rect Clip
    -> merged hardware scissor when axis-aligned

Simple RRect Clip
    -> analytic clip where profitable

Complex Path Clip
    -> stencil or mask

Stable repeated complex clip
    -> retained ClipMaskAtlas / cached realization
```

## 14.2 ClipChain

ClipChain 必须 retained。

没有 clip 变化时不得每帧重新 flatten 整条 clip ancestry。

## 14.3 Empty clip

空 clip 立即 subtree reject。

## 14.4 Mask

基础：

```text
AlphaMask
LuminanceMask
```

高级 mask boolean 可以 later lane 建立，但不改变基础 mask composition 语义。

## 14.5 Opacity

区分：

```text
primitive opacity
group opacity
```

Primitive opacity 可直接乘进 premultiplied color。

Group opacity 在子内容重叠导致逐 primitive opacity 语义不等价时，才允许 isolation/offscreen。

## 14.6 Blend baseline

先：

```text
Clear
Src
Dst
SrcOver
```

再补：

```text
Plus
Multiply
Screen
Overlay
Darken
Lighten
```

需要 destination read 或 isolation 的 blend 必须显式进入 Effect Planner。

---

## Detailed Design — Clip：默认不裁剪，按成本分级

这是重要性能合同：

> **Border radius 不等于自动 clip children。普通容器默认 overflow-visible；只有语义明确需要时才 clip。**

Clip Planner：

```text
Axis-aligned Rect
-> merged Scissor

Axis-aligned RRect / simple analytic shape
-> analytic clip when profitable

General stable clip
-> cached Clip Mask / stencil strategy

Repeated complex clip chain
-> R8 ClipMaskAtlas

Effect/isolation required
-> offscreen only when semantics force it
```

滚动 viewport 应优先得到一个 scissor，不应仅因为父节点有圆角就默认创建 offscreen layer。

---

## Detailed Design — ClipChain 必须 retained

```text
ClipChainId
    ↓
pre-resolved clip descriptor
```

Geometry 没变化时：

```text
no path reparse
no mask re-raster
no clip-stack tree walk per primitive
```

嵌套 axis-aligned rect 可以提前求交集。

复杂 ClipMask key 至少包含：

```text
geometry revision
effective transform bucket
device scale
fill rule
clip composition
```

---

## Detailed Design — Mask

最低支持：

```text
AlphaMask
LuminanceMask
ImageMask
PathMask
```

稳定 mask 应进入独立 mask cache。

不要把 mask 永久存成全屏 RGBA texture：

```text
R8 where possible
tight ROI
tile/page allocation
```

只有真正需要 color information 时才使用 RGBA。

---

## Detailed Design — Group Opacity

以下情况：

```text
parent opacity = 0.5
children do not overlap
```

可以安全 push opacity 到 children。

但：

```text
children overlap
+
group must preserve compositing result
```

需要 isolation layer。

是否 overlap 可由保守 bounds/paint metadata 判断；不确定时保持正确性，使用 layer。

---

## Detailed Design — Blend Mode

至少支持标准 Porter-Duff 与常见艺术 blend：

```text
SrcOver
Src
DstOver
SrcIn / DstIn
SrcOut / DstOut
SrcATop / DstATop
Xor
Plus

Multiply
Screen
Overlay
Darken
Lighten
ColorDodge
ColorBurn
HardLight
SoftLight
Difference
Exclusion
Hue
Saturation
Color
Luminosity
```

实现策略：

```text
fixed-function blend available
-> use fixed function

destination read/subpass/framebuffer-fetch fast path available
-> backend-specialized path

otherwise
-> bounded offscreen composite
```

复杂 blend 不允许污染最常用 `SrcOver` pipeline。

---

# 15. E0 — Analytic Shadow

普通 UI Shadow 是高频能力，必须有 fast lane。

## 15.1 Fast shapes

```text
Rect
RRect
Circle
Ellipse
Capsule
```

优先 analytic shadow，不默认：

```text
mask
→ full texture blur
→ composite
```

## 15.2 Parameters

```text
offset
sigma / blur radius semantic
spread
color
```

## 15.3 Decorated shape fusion

经过 benchmark 后允许建立：

```text
shadow + fill + border
```

的 fused `DecoratedShape` pipeline。

但纯 Rect 必须保留更短 pipeline，不能强制所有 Rect 进入大 shader。

## 15.4 Arbitrary path shadow

只有 general path shadow 才允许进入：

```text
tight mask
→ blur
→ composite
```

并缓存稳定 mask/blur 结果。

---

## Detailed Design — Shadow：简单 shape 不应走通用 Blur

### 20.1 Rect / RRect / Circle / Capsule

常见 shadow 优先：

```text
expanded instance quad
+
analytic distance
+
Gaussian-like coverage approximation
```

可在同一 primitive family 中绘制：

```text
outer shadow
fill
border
```

是否一次 draw 完成由 shader register pressure / overdraw benchmark 决定；语义上不要求业务拆成多个 widget。

### 20.2 任意 Path shadow

```text
Path/Mask
   ↓
tight shadow mask
   ↓
blur
   ↓
offset/color composite
```

稳定 Path 可以缓存未着色 blur mask：

```text
same geometry + same sigma
changing shadow color/offset
-> reuse blurred mask
```

### 20.3 Inner Shadow

简单 analytic geometry 直接距离函数实现；general path 使用 mask/filter lane。

---

# 16. E1 — Offscreen / Blur / ROI / Transient Targets

到此阶段才建立完整 offscreen infrastructure。

## 16.1 为什么不能更早抽象完整 RenderGraph

基础 Rect/Path 不需要复杂 graph。

先由真实需求产生：

```text
main pass
mask pass
shadow blur pass
offscreen group
```

再抽象 RenderGraph。

避免过早为了理论通用性引入 pass/node/edge 开销。

## 16.2 Tight ROI

任何 offscreen effect 必须先求最小必要区域：

```text
content/effect bounds
∩ clip bounds
∩ surface bounds
```

禁止小 Widget 的 blur 默认处理整个 4K surface。

## 16.3 Blur ladder

按有效 sigma / ROI / backend 自动选择：

```text
small
    -> direct / separable blur

medium
    -> optimized separable / compute where profitable

large
    -> downsample pyramid
       multi-scale blur
       upsample
```

选择策略是内部性能策略，不进入 public ABI。

## 16.4 Transient Target Planner

Offscreen texture 使用：

```text
lifetime analysis
size/format/sample compatibility
alias/reuse
frame-local pool
```

而不是每个 Effect `create_texture()` / `destroy_texture()`。

## 16.5 Tile-based GPU

移动 GPU 上应减少：

```text
unnecessary render-target switches
full-screen intermediate
store/load cycles
large transparent overdraw
```

Effect Planner 必须能看到 tile-friendly cost。

---

## Detailed Design — Blur：按半径与 ROI 自适应

Blur 是最容易浪费带宽的 effect。

禁止：

```text
small 200x80 panel
-> full-screen copy
-> full-screen blur
-> crop back
```

必须：

```text
effect bounds
+
kernel expansion
+
clip/intersection
        ↓
tight ROI
```

Blur Planner 根据 sigma、面积、backend 选择：

```text
small radius
-> direct/separable Gaussian

medium radius
-> optimized separable or compute blur

large radius
-> downsample pyramid / Kawase-like multiscale path
```

具体阈值是 benchmark 参数，不是 public ABI。

---

## Detailed Design — Render Graph：结构变化才重编译

Graph topology 例如：

```text
Main UI
-> Backdrop Capture
-> Blur
-> Glass Composite
-> Present
```

如果只是 tint 从 A 变 B：

```text
update parameter
```

不应：

```text
destroy graph
rebuild graph
reallocate textures
```

RenderGraphPlan 只有 dependency topology / target requirement 变化才重新 compile。

---

## Detailed Design — Transient Render Target Planner

目标：

```text
tight bounds
appropriate format
appropriate sample count
reuse
alias
```

必须避免：

```text
每个 shadow 一个新 texture
每个 clip 一个 RGBA texture
每个 MaterialSurface 一张全屏 texture
```

Render target pool 按：

```text
format
usage
size class/bucket
sample count
```

复用。

---

## Detailed Design — Tile-based Mobile GPU 优化

iOS/Android 很多 GPU 对 attachment store/load 与 offscreen bandwidth 非常敏感。

因此：

```text
avoid unnecessary pass break
avoid full-screen intermediate
keep effects ROI-bounded
allow transient/memoryless attachments when backend can
fuse passes only when it lowers real bandwidth
```

Backend 可以拥有 tile-GPU specialized implementation，不强迫 D3D12/Vulkan desktop 使用相同策略。

---

# 17. E2 — Backdrop / Color Effects / Advanced Blend

## 17.1 Backdrop capture

Backdrop 是依赖“后方已经绘制内容”的明确语义。

它必须通过 RenderGraph dependency 表达，不允许 Widget 自己读取当前 framebuffer 的未定义状态。

## 17.2 Shared backdrop

同一区域多个 Frosted/Glass material：

```text
shared capture
+
shared blur pyramid where compatible
+
multiple material composites
```

不得默认每个 material 独立完整 blur chain。

## 17.3 Color effects

支持：

```text
Brightness
Contrast
Saturation
HueRotate
Grayscale
Sepia
Invert
ColorMatrix
Tint
```

连续兼容 effect 应合并为一个 color transform/pass。

禁止：

```text
Brightness pass
→ Contrast pass
→ Saturation pass
```

在可以数学合并时产生三个 render target pass。

---

## Detailed Design — Effect 分类：Local vs Nonlocal

### Local Effect

不要求邻域像素或 destination read：

```text
opacity
tint
color matrix
brightness
contrast
saturation
simple gradient
simple mask
certain blend states
```

应该尽量 fuse 进现有 draw shader。

### Nonlocal Effect

需要：

```text
neighbor samples
previous framebuffer
group isolation
```

例如：

```text
blur
backdrop blur
large/general shadow
destination-dependent advanced blend
displacement
group opacity with overlapping children
```

才考虑 offscreen/render target。

---

## Detailed Design — Effect Planner 取代滥用 saveLayer

每个潜在 Layer 必须记录原因：

```text
LayerReason
    GroupOpacity
    ImageFilter
    BackdropFilter
    AdvancedBlend
    Isolation
    ComplexMask
    SnapshotCache
    NativeMaterialBoundary
```

Planner 尝试依次消除：

```text
Can push opacity into children?
Can fuse color matrix?
Can replace clip layer with scissor?
Can use analytic shadow?
Can share backdrop?
Can collapse adjacent effects?
```

只有全部不成立才创建 offscreen。

这条是 Viso 与传统“遇到效果就 saveLayer”实现的重要区别。

---

## Detailed Design — Color Filter 必须尽可能合并

多个：

```text
brightness
contrast
saturation
tint
opacity
```

应编译成：

```text
single ColorTransform / matrix-like operation
```

而不是五个 pass。

只有不能表达成 local color transform 的 custom filter 才创建额外 effect pass。

---

## Detailed Design — Effect Damage

Backdrop/blur 不能只看自身 property dirty。

如果其 ROI 背后的内容变化：

```text
BackdropDependencyRevision
```

只让相关 material/effect ROI dirty。

其他 backdrop 不受影响。

---

# 18. M0 — Frosted / Generic Glass

M0 是材质层，不是 Renderer 地基。

建立在：

```text
BackdropCapture
Blur
ColorTransform
Mask
Border/Highlight
Shadow
Shared ROI
```

之上。

典型 Frosted：

```text
backdrop
→ blur
→ saturation/tint
→ optional noise
→ material mask
→ border/highlight
```

`Viso_Visual_Materials.md` 定义材质参数与平台语义；本文只定义它必须复用 E1/E2 的底层能力。

---

## Detailed Design — Backdrop Blur / Frosted / Glass

Backdrop effect 必须进入依赖图：

```text
content behind material
        ↓
Backdrop Dependency
        ↓
capture only required ROI
        ↓
shared blur/downsample resource when possible
        ↓
material composite
```

多个相邻/重叠 Frosted/Glass：

```text
不得 N 个 widget = N 次 full-screen capture + N 次 blur
```

必须允许：

```text
union ROI
shared backdrop pyramid
MaterialGroup
shared effect pass
```

完整 Apple Liquid Glass / Frosted / native material lane 见：

```text
Viso_Visual_Materials.md
```

---

# 19. M1 — Platform Material / Liquid Glass / HDR

平台高级材质最后实现。

Apple 平台可以存在：

```text
Native System Material Lane
GPU Material Lane
```

选择取决于：

```text
系统集成要求
视觉一致性
可组合性
动画需求
性能 profile
是否需要 Viso GPU 内容参与
```

不得为了 Liquid Glass 把平台私有 API 泄漏到通用 Render IR。

HDR / Wide Gamut 同样建立在 F0 color contract、F1 surface format 与 E-layer effect correctness 之后。

---

## Detailed Design — HDR / Wide Gamut

至少区分：

```text
SDR sRGB target
wide-gamut target
HDR target
```

Blur、Glass、Gradient、Blend 的 intermediate format 由 RenderGraph Planner 根据需求决定。

禁止：

```text
所有 offscreen 一律 RGBA16F
```

也禁止：

```text
HDR scene 中途降成 8-bit sRGB 再升回去
```

资源格式要按真正需求选择。

---

# 20. A0 — Advanced GPU Optimization

A0 不作为普通 UI correctness 前置。

包括：

```text
GPU Compute Vector
path binning
GPU culling
bindless / descriptor indexing
indirect draw
multi-draw
GPU-driven batching
advanced compute effects
```

## 20.1 Vector Compute Lane

只有 benchmark 证明以下 workload 获益时使用：

```text
大量动态 path
vector editor / canvas
复杂 clip scene
高 path churn
CPU tessellation 成为瓶颈
```

普通 Button/Panel/几十个稳定 Path 不因架构统一而经过 compute pipeline。

## 20.2 Bindless

能力检测：

```text
Metal argument/resource tables
D3D12 descriptor heap
Vulkan descriptor indexing
WebGPU binding arrays where available
```

不可用时回退：

```text
atlas
small texture set
binding-group batching
```

不改变 public Drawing API。

---

## Detailed Design — Vector Compute Lane：大量动态 Path 的高级路径

只有满足类似条件才进入：

```text
high path count
high path mutation rate
large total segment count
frequent clipping/compositing
CPU tessellation becomes frame bottleneck
GPU compute capability sufficient
```

概念：

```text
Path Segment Scene
      ↓
tile/binning
      ↓
parallel prefix / allocation
      ↓
per-tile coverage work
      ↓
fine raster
```

具体算法不是 public ABI；实现可以参考现代 GPU compute vector renderer 的经验。

硬规则：

> **Compute Lane 是 workload specialization，不允许因为它“高级”就让 20 个普通按钮也经过多次 compute dispatch。**

---

## Detailed Design — Texture binding：Bindless fast path + portable fallback

Backend capability 允许时：

```text
Metal argument/resource tables
D3D12 descriptor heap style table
Vulkan descriptor indexing
WebGPU binding array capability
```

可以进入 bindless/resource-table fast path：

```text
instance.texture_index
```

从而避免大量 texture-change batch break。

不具备能力时：

```text
atlas
+
small texture set
+
bind-group batching
```

Public Paint API 不因 backend 不同而改变。

---

## Detailed Design — Culling

普通 UI：

```text
CPU bounds + clip intersection
```

即可。

大量 Canvas/scene：

```text
chunk-level CPU cull
+
optional GPU cull / indirect draw
```

不要让 50 个 UI node 为了“GPU driven”产生 compute dispatch。

---

# 21. Render 数据布局

## 21.1 Hot/Cold 分离

Hot：

```text
primitive type
transform id
clip id
bounds
brush id
instance slot
render flags
```

Cold：

```text
debug name
source span
inspection metadata
rare effect parameters
accessibility cross-reference
```

热遍历不得拖着冷字符串和 Arc graph。

## 21.2 AoS / SoA

不做教条统一。

规则：

```text
GPU instance upload
    -> AoS when matching GPU fetch/layout is best

CPU culling/bounds scan
    -> SoA or hybrid when SIMD scan benefits明显
```

最终由 benchmark 决定具体 lane 的 layout。

## 21.3 Fixed-width ABI

Public/stable IDs 和 GPU ABI 禁止 `usize`。

使用：

```text
u16 / u32 / u64
f32 / f16 where explicitly supported
```

保证 32-bit / 64-bit / wasm32 语义一致。

---

## Detailed Design — 2D 与 3D 共存

2D Renderer 与 3D Scene 共用：

```text
GPU Device
Resource Manager
Shader compiler
RenderGraph
Frame scheduler
Profiler
```

但 2D primitive pipeline 不因为 3D 存在就引入：

```text
per-node depth object
material graph overhead
scene graph traversal
```

3D 作为独立 pass/lane 接入。

---

# 22. `unsafe` 性能策略

Viso 允许并鼓励 **有证据的 unsafe 优化**。

允许区域：

```text
GPU mapped-memory writes
Pod slice reinterpret
arena bump allocation
SIMD intrinsics
unchecked indexed hot loop after proof
platform FFI
backend command encoding
```

每个 unsafe block 必须有：

```text
SAFETY invariant
owner/lifetime/alignment explanation
debug assertion where possible
safe/scalar reference path or test oracle
fuzz/property coverage where appropriate
benchmark evidence
```

禁止：

```text
因为 unsafe “可能更快”就删除边界设计
依赖 Rust 默认 struct layout
跨线程裸指针无所有权合同
把 stale GPU pointer 当稳定 handle
```

---

## Detailed Design — `unsafe` Policy

Viso 明确允许 `unsafe` 进入性能地基，但必须局部化。

允许位置：

```text
GpuPod slice cast
mapped buffer access
arena bump allocation
SIMD intrinsics
backend FFI
validated unchecked indexing in proven hot loops
```

不允许：

```text
用 unsafe 掩盖所有 ownership 问题
跨帧保存可失效 raw pointer
把 native pointer 当 stable ID
未验证 layout 就 reinterpret
```

每个 unsafe module 必须：

```text
document invariants
debug assertions
scalar/safe reference tests where possible
fuzz/property tests
sanitizer/Miri coverage for applicable code
benchmark proving value
```

---

# 23. SIMD 策略

## 23.1 候选热点

```text
Affine transform batches
Rect intersection
bounds propagation
culling
Bezier flatness/bounds
stroke preprocessing
color conversion
image swizzle
mask CPU fallback
dirty-bit/range scan
```

## 23.2 Dispatch

进程/设备初始化阶段选择 kernel table：

```text
scalar
SSE family
AVX family
NEON
other target-specific
```

热循环不每个 primitive 重新 CPUID。

## 23.3 Public ABI

SIMD vector width 不进入 public ABI。

Public 仍使用稳定 `Vec2/Vec4` 等语义类型。

---

## Detailed Design — SIMD

允许并鼓励 SIMD，但只用于真正 CPU-heavy 的部分。

候选：

```text
batch transform of bounds/points
rect intersection
path bounds
curve flatten preprocess
stroke preprocess
color conversion
image swizzle
mask CPU fallback
visibility/culling
dirty bit scan
```

规则：

```text
scalar reference implementation exists
SIMD implementation pixel/geometry equivalent
feature dispatch once per process/device class
not once per primitive
```

SIMD backend 不进入 public ABI。

---

# 24. Culling / Damage / High Refresh

## 24.1 Culling

至少：

```text
surface bounds reject
clip bounds reject
zero-alpha fast reject
empty geometry reject
```

大型 scene 可进一步：

```text
chunk bounds
spatial bins
hierarchical culling
GPU culling in A0
```

## 24.2 Damage

Retained scene 必须知道哪些内容真正变化。

变化分类：

```text
Transform dirty
Paint parameter dirty
Geometry dirty
Clip dirty
Effect dirty
Resource dirty
```

不允许一个颜色改变升级成整个 scene geometry rebuild。

## 24.3 120/144/240Hz 稳态合同

无 render state 变化时，不得执行：

```text
primitive rebuild
path tessellation
shader compile
pipeline creation
resource allocation
full instance upload
clip reconstruction
effect graph rebuild
HashMap lookup per primitive
heap allocation per primitive
```

有局部 hover/animation 时，只处理受影响的：

```text
Transform/instance range
paint chunk
visible batch metadata
```

---

## Detailed Design — Damage / High Refresh

60/120/144/240Hz 的关键不是“每帧更快重建”，而是“不重建”。

Transform-only animation：

```text
TransformStore dirty
-> update compact transform buffer
-> reuse primitive geometry
-> reuse batch
```

Color/opacity-only：

```text
update instance range
```

Scroll：

```text
update transform / clip / virtualized items
```

不应：

```text
retessellate SVG
rebuild all batches
recompile graph
recreate render targets
```

---

## Detailed Design — Idle

没有：

```text
animation
input
timer
async completion
surface damage
external invalidation
```

时：

```text
0 UI frame
0 renderer encode
0 GPU submit
```

静态 UI 不因为 blur/glass/shadow 存在就持续重绘。

---

# 25. RenderGraph 的建立时机与合同

RenderGraph 只在 E1 开始成为完整基础设施。

它负责：

```text
pass dependency
resource read/write usage
barrier/state lowering
transient lifetime
attachment compatibility
pass merge opportunity
```

不负责：

```text
Widget tree
Layout
State binding
Font fallback
```

Graph 结构无变化时复用 compiled plan。

参数变化：

```text
blur sigma changed
color changed
transform changed
```

不应自动重建整个 graph topology。

---

# 26. Effect Planner 的建立时机与合同

Effect Planner 在 C0/E0/E1 逐步拥有足够事实后形成。

它回答：

```text
这个 effect 是否可以 inline/fuse？
是否真的需要 isolation？
是否需要 destination/backdrop？
最小 ROI 是多少？
哪个 transient target 可以复用？
能否共享 backdrop/blur pyramid？
是否可以使用 analytic fast path？
```

原则：

> **Offscreen 是昂贵实现机制，不是方便的默认 API 语义。**

---

# 27. 基础功能清单

以下是 Renderer Core 在进入 Material 层前必须具备的基础能力。

## Geometry

```text
Rect
RRect
per-corner RRect
RoundedSuperellipse
Circle
Ellipse
Capsule
Line
Polyline
Arc
Pie
Polygon
QuadraticBezier
CubicBezier
Path
```

## Stroke

```text
width
hairline
inside/center/outside
butt/round/square cap
miter/bevel/round join
miter limit
dash
dash offset
```

## Brush

```text
Solid
LinearGradient
RadialGradient
SweepGradient
ImagePattern
```

## Image

```text
Image
ImageRect
source crop
fit/alignment
nearest/linear/mipmap
NineSlice
Tile
Sprite
Atlas region
```

## Clip / Mask

```text
RectClip
RRectClip
PathClip
ClipChain
AlphaMask
LuminanceMask
```

## Composition

```text
primitive opacity
group opacity
SrcOver
Clear/Src/Dst
common blend modes
```

## Effects before Material

```text
analytic outer shadow
inner shadow
path shadow
blur
backdrop blur
color matrix family
```

只有这些稳定后才进入 Frosted/Glass/Liquid Glass。

---

## Detailed Design — Public 绘画能力必须完整

Viso 1.0 标准绘画能力至少包含：

```text
Geometry
    Rect
    RoundedRect / per-corner radius
    RoundedSuperellipse
    Circle
    Ellipse
    Capsule
    Line / Polyline
    Arc / Pie
    Polygon
    QuadraticBezier
    CubicBezier
    Path
    SVG
    Mesh2D
    Mesh3D

Paint
    Solid Color
    Linear Gradient
    Radial Gradient
    Sweep / Conic Gradient
    Image Pattern
    Shader Brush

Stroke
    width
    inside / center / outside alignment
    butt / round / square cap
    miter / round / bevel join
    miter limit
    dash pattern
    dash offset
    hairline

Image
    full image
    source rect
    nine-slice
    atlas/sprite
    tiling
    fit/crop
    mipmapped scaling

Composition
    opacity
    clip
    alpha/luminance mask
    blend mode
    group/isolation

Effects
    outer shadow
    inner shadow
    blur
    backdrop blur
    color matrix
    brightness
    contrast
    saturation
    hue rotation
    grayscale
    invert
    sepia
    custom fragment effect
    custom compute effect where supported

Materials
    Frosted
    Glass
    platform system material
    GPU backdrop material
```

文字本身由 `Viso_Text_Font_Runtime.md` 定义，但最终仍作为 `GlyphRun` primitive 进入本渲染系统。

---

## Detailed Design — Custom Shader

Viso 自定义 shader 不应像 Flutter 那样只提供 fragment shader。

Viso Shader Domain 应允许在受控能力中提供：

```text
vertex
fragment
compute
```

但普通 UI custom effect 默认从 fragment/local effect 开始。

Release：

```text
Shader Source
   ↓
build-time typed compile
   ↓
MSL / DXIL / SPIR-V / WGSL package
   ↓
PipelineManifest
```

禁止在交互动画热路径临时编译 shader source。

---

## Detailed Design — Custom Canvas 的正确定位

Viso 可以提供 `Canvas2D` / `Scene2DBuilder` 风格 API，但它是：

> **Immediate authoring facade over retained Render IR**

而不是：

```text
每个 vsync
-> replay user callback
-> allocate command list
-> parse commands
```

静态/状态驱动 Canvas 在依赖没变时复用其 `Scene2D`。

动态图可以更新已有 `PrimitiveHandle`，避免每帧重建整个 Scene。

---

## Detailed Design — 建议 Rust Low-level API

概念示例，不冻结具体函数签名：

```rust
let shape = scene.shape(ShapeGeometry::RoundedRect {
    rect,
    radii,
});

shape
    .fill(Brush::Solid(color))
    .stroke(stroke)
    .shadow(shadow);
```

大量 sprite：

```rust
let mut sprites = scene.sprite_batch(texture);
sprites.extend_pod(&instances);
```

动态 primitive：

```rust
let handle = scene.insert(...);

// later
scene.update_transform(handle, transform);
scene.update_opacity(handle, opacity);
```

`update_transform` 不允许重建 path geometry。

---

## Detailed Design — DSL / Standard Schema

绘画能力属于 Native Schema，不增加一堆语言关键字。

概念：

```viso
Shape {
    geometry = ShapeGeometry::RoundedRect {
        radius: 16dp,
    };

    fill = Brush::LinearGradient {
        from: vec2(0.0f32, 0.0f32),
        to: vec2(1.0f32, 1.0f32),
        stops: [
            GradientStop { offset: 0.0f32, color: #ff8a00 },
            GradientStop { offset: 1.0f32, color: #e52e71 },
        ],
    };

    stroke = Option::Some(StrokeStyle {
        width: 1dp,
        alignment: StrokeAlignment::Inside,
        cap: StrokeCap::Butt,
        join: StrokeJoin::Round,
    });
}
```

Effect：

```viso
EffectSurface {
    effects = [
        Effect::Shadow(Shadow {
            offset: vec2(0dp, 8dp),
            blur: 24dp,
            spread: 0dp,
            color: #00000040,
        }),
    ];

    CardContent {}
}
```

标准 Schema 必须能够被 compiler 查询 effect cost class。

---

## Detailed Design — Default Widget 绘画策略

Button / Card / Panel 等普通控件：

```text
background
border
radius
simple shadow
state color
```

必须优先 lower 成一个或极少数 analytic instances。

禁止默认 lower 成：

```text
Path
+ ClipPath
+ Offscreen
+ Blur
+ Composite
```

Scroll：

```text
axis-aligned viewport
-> scissor
```

Image：

```text
one image instance
```

Text：

```text
retained glyph run
```

---

# 28. 容易遗漏但必须提前固定的边界功能

必须在相应 foundation 阶段明确：

```text
pixel snapping
hairline
per-corner radius normalization
empty/negative rect handling
NaN/Inf validation
degenerate path behavior
singular transform behavior

premultiplied alpha
linear/sRGB conversion
texture origin
UV edge/half-texel correctness
atlas bleeding prevention

fill rule
stroke miter limit
dash phase
open/closed path semantics
arc direction

clip nesting
empty clip fast reject
fully transparent fast reject
opaque fast path

stroke bounds inflation
shadow bounds inflation
blur/effect bounds inflation

device scale change
surface resize
MSAA/sample-count changes where used
HDR/color-space surface change

GPU device loss
resource generation
deferred destruction
in-flight safety
```

这些不是“边角情况”，而是后期最容易迫使 Renderer 重写的数据合同。

---

## Detailed Design — Memory Pressure

释放顺序建议：

```text
cold snapshot/effect output
cold path tessellation variants
cold clip/mask pages
cold image mip/atlas pages
cold MTSDF/vector representation
other reconstructible caches
```

仍在当前 frame/fence 使用的 GPU resource 必须 pin 到安全完成点。

禁止收到 memory pressure 后直接销毁当前帧仍引用的 texture/buffer。

---

## Detailed Design — Resource Budget 必须按 bytes

禁止：

```text
max_paths = 1000
max_images = 100
```

作为主要内存策略。

应该：

```text
path_geometry_budget_bytes
mask_atlas_budget_bytes
effect_cache_budget_bytes
image_budget_bytes
transient_target_budget_bytes
```

大资源有 oversize admission policy。

---

# 29. 模块布局建议

保持 crate 边界少而硬。

`viso-render` 内部可按职责组织：

```text
render/src/
├── scene/
│   ├── primitive_id.rs
│   ├── primitive_store.rs
│   ├── transform_store.rs
│   ├── bounds.rs
│   └── damage.rs
├── primitive/
│   ├── rect.rs
│   ├── rrect.rs
│   ├── ellipse.rs
│   ├── line.rs
│   └── image.rs
├── brush/
│   ├── color.rs
│   ├── gradient.rs
│   └── image_brush.rs
├── path/
│   ├── path.rs
│   ├── flatten.rs
│   ├── stroke.rs
│   ├── tessellate.rs
│   └── geometry_cache.rs
├── clip/
│   ├── clip_chain.rs
│   ├── mask.rs
│   └── clip_cache.rs
├── compose/
│   ├── blend.rs
│   └── group.rs
├── effect/
│   ├── shadow.rs
│   ├── blur.rs
│   ├── backdrop.rs
│   ├── color_filter.rs
│   └── planner.rs
├── batch/
│   ├── render_chunk.rs
│   └── planner.rs
├── upload/
│   ├── instance_pool.rs
│   ├── upload_ring.rs
│   └── dirty_range.rs
├── graph/
│   ├── render_graph.rs
│   └── transient_targets.rs
├── frame/
│   ├── frame_arena.rs
│   └── frame_packet.rs
└── profile/
    └── counters.rs
```

这只是 module 组织，不代表必须拆成更多 crate。

---

# 30. 性能计数器必须从 D0 就存在

不能等 Renderer “做完”以后才 profiler。

至少记录：

```text
visible_primitives
culled_primitives
render_chunks
batches
draw_calls
pipeline_switches
texture_binding_switches
uploaded_bytes
uploaded_ranges
instance_rebuilds
path_tessellations
clip_mask_builds
offscreen_passes
transient_target_bytes
blur_pixels
backdrop_capture_pixels
shader_pipeline_creations
cpu_render_build_time
cpu_encode_time
gpu_frame_time
```

这样每增加一个新功能，都能看到它是否破坏基础路径。

---

## Detailed Design — Effect Cost Metadata

Schema/IR 至少标记：

```text
Local
Analytic
NeedsMask
NeedsOffscreen
NeedsBackdrop
DestinationRead
ComputePreferred
```

Compiler/LSP/Inspector 可以提示：

```text
this effect creates offscreen ROI
this backdrop forces source dependency
this clip uses mask path
```

这对 AI coding 也很重要，避免生成视觉正确但灾难性的组合。

---

## Detailed Design — Inspector 必须显示真实渲染成本

至少：

```text
primitive count by lane
paint chunk count
batch count
draw call count
pipeline switches
texture/resource table switches
clip class distribution
offscreen pass count
offscreen pixels
blur pixels/samples
backdrop capture pixels
transient target peak bytes
persistent GPU bytes
upload bytes/frame
dirty instance ranges
path tessellation jobs
compute-vector workload
overdraw estimate
GPU time per pass
```

用户看到一个漂亮 Card 时，必须能知道它为什么贵。

---

## Detailed Design — Debug Overlay

建议：

```text
show paint bounds
show dirty regions
show clip masks
show offscreen ROI
show backdrop dependency
show batch breaks
show path lane
show overdraw
show GPU resource residency
```

这些只编入 Dev tooling path。

---

# 31. Benchmark Gate

每个阶段完成前必须有对应 benchmark。

## F0/F1/F2

```text
Affine transform batch
Rect intersection
buffer upload bandwidth
mapped ring allocation
pipeline lookup
command encode baseline
```

## F3/F4

```text
100k retained primitive traversal
single dirty primitive
1% dirty primitives
full dirty scene
batch construction
upload range coalescing
```

## D0/D1

```text
10k / 100k Rect
10k RRect
mixed Rect/RRect/Circle
scroll transform-only
hover paint-only
```

## D2

```text
many gradients
image grid
sprite atlas
texture-binding pressure
```

## D3

```text
small stable SVG-like paths
large static path scene
path transform-only
stroke/dash heavy
path churn
```

## C0

```text
deep Rect clip
mixed RRect clip
complex cached clip
nested opacity
blend stress
```

## E0/E1/E2

```text
1k analytic shadows
path shadow reuse
small blur
medium blur
large blur
many small ROI blur
shared backdrop
color-effect fusion
```

## Resource / Memory

每个阶段还必须记录资源成本，不能只看 draw call 或 frame time：

```text
CPU retained-scene bytes
CPU staging/upload bytes
GPU persistent buffer bytes
GPU texture/atlas bytes
transient render-target peak bytes
allocation count/frame
allocation bytes/frame
upload bytes/frame
resource create/destroy count
```

涉及 Path / Clip / Blur / Backdrop / Material 的阶段还应分别记录 geometry cache、mask cache、transient target 与 backdrop cache 的峰值/稳态占用，防止通过“更快但显著吃内存”的方式掩盖回归。

## High refresh

对 60 / 120 / 144 / 240Hz profile 记录：

```text
steady frame CPU work
upload bytes/frame
allocation/frame
pass count
frame-time variance
resident CPU bytes
resident GPU bytes
transient GPU peak bytes
```

性能回归以 reference hardware/profile baseline 比较，不用单一绝对毫秒绑死所有设备。

---

## Detailed Design — Benchmark Matrix

最低必须覆盖：

```text
10k / 100k SolidRect
10k RRect
10k borders
5k analytic shadows
1k multi-stop gradients
10k image/sprite instances
glyph-heavy UI
deep rect clip
nested rounded clips
1k SVG icons
large complex SVG
dynamic chart path
500 continuously morphing paths
large vector canvas pan/zoom
group opacity overlap/no-overlap
small blur ROI
large blur ROI
30 overlapping frosted/glass surfaces
advanced blend
HDR effect chain
4K 60Hz
1440p 144Hz
1080p 240Hz
mobile 120Hz
memory pressure
device loss/recreate
```

每组记录：

```text
CPU paint/update time
batch build time
upload bytes
allocations
draw calls
pass count
transient bytes
persistent GPU bytes
GPU time
bandwidth proxy
frame-time p50/p95/p99
```

---

## Detailed Design — Makepad 对照 Benchmark

至少实现：

```text
same 10k rounded cards
same animated radius/hover
same simple SDF icons
same sprite/text workload
```

比较：

```text
CPU frame cost
draw calls
upload
GPU time
memory
```

目标不是“必须赢一个数字”，而是确认 Viso retained/typed path 没有引入不必要成本。

---

## Detailed Design — Flutter/Impeller 对照 Benchmark

重点不是 Widget build，而是绘画行为：

```text
RRect
Path
Image atlas
Clip
Shadow
Blur
Backdrop
Group opacity
```

特别记录：

```text
offscreen pass / saveLayer-equivalent count
render target bytes
GPU time
```

Viso Effect Planner 应在等价视觉下减少不必要 offscreen。

---

## Detailed Design — Vector Compute Lane 验证

Compute Lane 只有在真实 crossover 点出现后才启用默认 heuristic。

测试：

```text
10 paths
100 paths
1k paths
10k paths
static
transform-only
morphing
clip-heavy
```

对比：

```text
cached CPU tessellation
GPU compute vector
```

阈值按 backend/device class profile，不写死进 public API。

---

## Detailed Design — Unsafe / SIMD Benchmark Gate

任何新增 unsafe/SIMD 优化必须给出：

```text
before
after
workload
device/CPU
correctness comparison
```

如果收益不显著：

```text
保留更简单安全实现
```

Viso 允许 unsafe，不代表追求 unsafe 数量。

---

# 32. Correctness Gate

性能优化不能绕过这些测试：

```text
pixel golden tests
cross-backend image diff
premultiplied alpha tests
sRGB/linear tests
gradient edge tests
stroke join/cap tests
clip nesting tests
path fill-rule tests
degenerate geometry tests
DPI scaling tests
surface recreation tests
resource lifetime stress
device-loss simulation where backend permits
```

Path/geometry 还应有 fuzz/property tests。

Unsafe/SIMD kernel 必须与 scalar oracle 做等价测试。

---

# 33. 阶段完成门槛

## F0 Done

```text
[ ] 坐标/像素/Rect 边界合同固定
[ ] ColorSpace / linear / premultiplied alpha 固定
[ ] PixelSnap / Hairline 合同固定
[ ] NaN/Inf/degenerate 规则固定
```

## F1 Done

```text
[ ] 各 Tier-1 backend 可 clear/present
[ ] Buffer/Texture/Sampler/Pipeline/Pass 基线成立
[ ] resize/device-scale/surface recreate 正确
[ ] resource generation + deferred free 正确
```

## F2 Done

```text
[ ] build-time standard shader compilation
[ ] reflection + layout validation
[ ] GpuPod/typed GPU ABI
[ ] pipeline manifest/cache baseline
[ ] Release 无标准 shader runtime source compilation
```

## F3 Done

```text
[ ] Retained PrimitiveStore
[ ] Transform/Brush/Clip identity 分离
[ ] Paint order 固定
[ ] local/world/paint bounds
[ ] damage category
```

## F4 Done

```text
[ ] persistent instance slot
[ ] upload ring
[ ] dirty-range coalescing
[ ] frame arena
[ ] order-safe batching
[ ] profiler counters
```

## D0 Done

```text
[ ] Solid Rect + SrcOver + Scissor
[ ] local dirty 不全量上传
[ ] 100k rect benchmark 可重复
[ ] 热路径结构性零 per-primitive heap allocation
```

## D1 Done

```text
[ ] RRect/per-corner/Circle/Ellipse/Capsule
[ ] Border/Line/Cap/Join 基础
[ ] analytic AA correctness
```

## D2 Done

```text
[ ] Linear/Radial/Sweep Gradient
[ ] Image/ImageRect/Sampling
[ ] NineSlice/Tile/Atlas
```

## D3 Done

```text
[ ] Path commands
[ ] NonZero/EvenOdd
[ ] Fill/Stroke/Dash
[ ] retained tessellation cache
[ ] transform/color 不重建 geometry
```

## C0 Done

```text
[ ] Rect/RRect/Path clip ladder
[ ] ClipChain retained
[ ] Mask
[ ] Group opacity
[ ] Blend baseline
```

## E0 Done

```text
[ ] UI shape analytic shadow
[ ] inner shadow
[ ] path shadow fallback
```

## E1 Done

```text
[ ] offscreen ROI
[ ] blur ladder
[ ] transient target reuse
[ ] RenderGraph compile/reuse
```

## E2 Done

```text
[ ] backdrop dependency
[ ] shared backdrop/blur
[ ] color effect fusion
[ ] advanced blend isolation
```

M0/M1/A0 不得被列为 D0~E2 的完成前置。

---

## Detailed Design — Definition of Done

Viso Rendering Runtime 只有同时满足下面条件才算达到 1.0 合同：

```text
[ ] Rect/RRect/Circle/Ellipse/Line/Arc/Path/Image/Text/Mesh 有明确 primitive path
[ ] Solid/Linear/Radial/Sweep/ImagePattern/Shader Brush 有明确语义
[ ] Stroke cap/join/miter/dash/alignment 有明确语义
[ ] simple UI shape 默认 analytic/instanced，不默认 CPU tessellate
[ ] general stable path 可以 retained cached geometry
[ ] compute vector 仅作为大型动态 workload lane
[ ] default UI 不全局启用 MSAA
[ ] Rect clip 使用 scissor fast path
[ ] Border radius 不自动意味着 clip children
[ ] stable complex clip 可进入 mask cache
[ ] simple shadow 不默认创建 blur layer
[ ] general blur/backdrop 使用 tight ROI
[ ] 多个 backdrop/material 可以共享 capture/blur
[ ] local color effects 可以 fuse
[ ] offscreen layer 有明确 LayerReason
[ ] group opacity 只在语义需要时 isolation
[ ] GPU canonical alpha 为 premultiplied
[ ] HDR/wide-gamut intermediate 不被错误降级
[ ] standard shader 在 build-time 编译
[ ] frame hot path 不同步编译 shader
[ ] typed GpuPod ABI 不包含 pointer/usize/uninitialized padding
[ ] CPU -> upload 不经过多层 Vec<float>/Vec<byte> 转换
[ ] persistent instance pool + dirty range upload
[ ] transient textures 可 pool/alias
[ ] local paint change 不重建全 scene
[ ] transform-only 不 retessellate path
[ ] stable frame 不重建 clip/shadow/gradient cache
[ ] idle scene 不持续 submit
[ ] 120/144/240Hz benchmark 有 regression gate
[ ] memory pressure 不释放 in-flight resource
[ ] Inspector 可看到 pass/offscreen/upload/batch/blur/backdrop 成本
[ ] unsafe/SIMD 路径有 correctness reference 与 benchmark
```

---

# 34. 实现工作顺序

规范执行顺序：

```text
F0
↓
F1
↓
F2
↓
F3
↓
F4
↓
D0
↓
D1
↓
D2
↓
D3
↓
C0
↓
E0
↓
E1
↓
E2
↓
M0
↓
M1
↓
A0
```

允许同一阶段内部并行开发，但不得破坏层级依赖。

例如：

```text
可以：
D1 的 RRect shader 与 RRect CPU instance layout 并行

不可以：
为了做 M1 Liquid Glass，先把 F4 instance model 改成 material-specific layout
```

---

# 35. 现有代码收敛原则

如果已有基础实现，重构时按职责归位，而不是继续在现有模块上叠抽象。

对每个现有模块回答：

```text
它属于 F0/F1/F2/F3/F4/D/C/E/M/A 哪一层？
它是否依赖比自己更外层的语义？
它的数据是否 retained？
它是否在每帧重复做可缓存工作？
它是否产生多余 Vec/serialization/copy？
它是否把 HashMap/trait object/lock 放在 primitive hot traversal？
它是否把 offscreen 当成默认方便路径？
它是否能被 benchmark/计数器观测？
```

发现反向依赖时，优先修复边界，而不是再增加 adapter 掩盖问题。

---

# 36. 最终硬性能合同

Viso Rendering Core 的长期目标：

```text
Normal retained UI steady frame

0 primitive reconstruction when unchanged
0 path tessellation when geometry unchanged
0 standard shader compilation
0 pipeline creation
0 per-primitive heap allocation
0 global HashMap lookup in primitive traversal
0 Rc/RefCell borrow in primitive traversal
0 mutex/RwLock in single-thread render hot path
0 per-primitive backend virtual dispatch
0 full-scene upload for local change
0 unnecessary offscreen pass
```

常见 UI 应尽量退化为：

```text
Retained Primitive IDs
        ↓
visible RenderChunks
        ↓
compatible Batch ranges
        ↓
small dirty GPU updates
        ↓
prebuilt pipelines
        ↓
encode / submit / present
```

---

## Detailed Design — Steady-state 硬性能合同

Retained UI 已就绪时，普通帧不得做：

```text
0 shader source compilation
0 pipeline source compilation
0 GPU buffer creation per primitive
0 GPU texture creation per primitive
0 general path parse
0 path tessellation unless geometry/quality bucket changed
0 gradient LUT rebuild unless gradient changed
0 clip mask rebuild unless clip changed
0 shadow mask rebuild unless geometry/sigma changed
0 full-scene Paint IR rebuild for local dirty
0 heap allocation per primitive
0 string lookup per primitive
0 per-primitive mutex/RwLock
```

静态 scene：

```text
0 frame
```

---

## Detailed Design — 典型热路径目标

```text
dirty bitset
   ↓
changed PaintChunks
   ↓
patch typed primitive fields
   ↓
coalesce dirty GPU ranges
   ↓
patch affected batch spans if key changed
   ↓
reuse RenderGraphPlan
   ↓
encode
   ↓
submit
```

如果 key 未变化：

```text
batch structure 不动
```

---

## Detailed Design — 典型按钮必须有极短路径

```text
Button hover
     ↓
hover value changes
     ↓
paint color/parameter dirty
     ↓
one instance range write
     ↓
same pipeline
same geometry
same clip
same batch
```

不允许 hover 导致：

```text
widget tree rebuild
path rebuild
shader rebuild
pipeline rebuild
```

---

## Detailed Design — 典型圆角 Card

目标：

```text
RRect fill
+ border
+ one normal shadow
```

优先为：

```text
1 analytic primitive
or
1 shadow instance + 1 shape instance
```

而不是多级 offscreen。

是否融合成一个 draw 由 GPU benchmark 决定，但 Public/IR 语义保持同一 Decoration。

---

## Detailed Design — 大型 Blur Panel

目标：

```text
visible ROI
      ↓
downsample/share
      ↓
blur
      ↓
composite
```

后台未变化时允许 reuse effect result。

Panel 自己移动但 backdrop 内容不变时，Planner 可以根据 capture model 决定复用/局部重采样；不得无脑重算整屏。

---

## Detailed Design — API 的“方便”不能隐藏灾难成本

可以提供：

```text
Effect::Blur
Effect::BackdropBlur
BlendMode::SoftLight
```

但 Schema/Inspector 必须告诉开发者 cost class。

复杂 effect 不需要禁止，但必须可观测。

---

# 37. 最终架构摘要

```text
                       UI / Text / Canvas
                              │
                              ▼
                       Retained Paint IR
                              │
                 ┌────────────┴────────────┐
                 ▼                         ▼
           Primitive Stores           Path Geometry
                 │                         │
                 └────────────┬────────────┘
                              ▼
                     Bounds / Clip / Damage
                              │
                              ▼
                 Persistent Instance / Geometry
                              │
                              ▼
                    Order-safe Batch Planner
                              │
                              ▼
                    Effect Planner when needed
                              │
                              ▼
                 RenderGraph only when needed
                              │
                    ┌─────────┴─────────┐
                    ▼                   ▼
                viso-shader          viso-gpu
                    │                   │
                    └─────────┬─────────┘
                              ▼
             Metal / D3D12 / Vulkan / WebGPU
```

Viso Rendering Foundation 的核心不是“所有东西都使用最新 GPU 技术”，而是：

> **把最常见的路径做到最短，把昂贵技术只用于真正能摊薄成本的 workload；基础数据 retained、可局部失效、可局部上传，Shader 与 GPU backend 都服务于这一目标。**

## Detailed Design — 从 Makepad、Flutter/Impeller、Vello 吸收什么

### 3.1 Makepad：保留 shader-first + instancing，不复制所有边界

Makepad 当前公开实现体现了两个非常有价值的经验：

```text
Sdf2d / shader-based UI primitive
+
appendable draw call / many instances
```

这说明：

- 圆角框、简单 icon、状态动画可以避免 CPU 几何重建；
- 同 shader/material 的大量对象适合 instance batching；
- 自定义视觉应该可以直接下钻 shader，而不是只能拼平台 widget。

Viso 采用这些思想，但进一步要求：

```text
typed instance struct
retained instance range
partial dirty upload
effect planner
multi-lane path rendering
explicit clip/offscreen cost model
```

而不是把所有实例压成无类型 `Vec<f32>` 风格的数据流。

### 3.2 Flutter / Impeller：保留完整绘画语义与 predictable pipeline

Flutter `Canvas` 暴露了 Rect/RRect/Path/Image/Atlas/Vertices/Clip/Shadow 等完整 2D primitive；Impeller 强调：

```text
offline shader compilation
explicit pipeline/resource cache
modern GPU API
concurrency
predictable performance
```

这些方向 Viso 全部采纳。

Flutter 文档同时明确指出 `saveLayer()`、过度 clipping、BackdropFilter 等会产生昂贵的 offscreen/render-target switching。

Viso 因此不把 `saveLayer` 式思维作为普通效果默认实现，而引入：

```text
Effect Planner
ROI
effect fusion
transient target aliasing
shared backdrop
```

### 3.3 Vello：GPU compute 是大型 Vector Scene 的 lane，不是普通 UI 默认

Vello 证明 GPU compute 对大型 vector scene、复杂 clipping、并行 path processing 很有潜力。

但 GPU dispatch、temporary buffers、prefix scan 等也有固定成本。

因此 Viso 的决定是：

> **普通 UI 不为了“架构统一”强制走 compute；只有 path 数量、动态程度、coverage 工作量达到阈值时，Vector Compute Lane 才参与。**

这是性能策略，不是 feature 降级。

---

## Detailed Design — 外部参考

本文参考以下公开资料，但不建立兼容关系：

1. Makepad `draw/src` 与 shader/draw-list 公开实现：
   - <https://github.com/makepad/makepad/tree/dev/draw/src>
   - <https://github.com/makepad/makepad/blob/dev/draw/src/draw_list_2d.rs>
   - <https://github.com/makepad/makepad/tree/dev/draw/src/shader>
2. Flutter Canvas：
   - <https://api.flutter.dev/flutter/dart-ui/Canvas-class.html>
3. Flutter Impeller：
   - <https://docs.flutter.dev/perf/impeller>
4. Flutter rendering performance / `saveLayer` guidance：
   - <https://docs.flutter.dev/perf/best-practices>
   - <https://api.flutter.dev/flutter/dart-ui/Canvas/saveLayer.html>
5. Flutter fragment shader：
   - <https://docs.flutter.dev/ui/design/graphics/fragment-shaders>
6. Vello GPU vector renderer：
   - <https://github.com/linebender/vello>
   - <https://github.com/linebender/vello/blob/main/ARCHITECTURE.md>
7. WGPU staging design 作为 WebGPU/reference adapter 的实现经验：
   - <https://wgpu.rs/doc/wgpu/util/belt/struct.StagingBelt.html>

参考原则：

> **参考经过验证的算法、性能特征、工具链经验和用户体验，不复制耦合边界。**

---

---

# Canonical Document Rule

`Viso_Rendering.md` is the single authoritative rendering document for architecture, implementation order, low-level rendering contracts, drawing primitives, composition, effects, materials integration, optimization gates, and vibe-coding execution.

Other Viso specifications may reference this file, but must not define an independent rendering implementation sequence.
