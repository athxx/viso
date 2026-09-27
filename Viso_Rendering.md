# Viso Rendering — Canonical Architecture, Implementation Plan & Design Specification

> Document status: **Viso 1.0 Draft**  
> Role: **single canonical rendering design + implementation-order document for vibe coding**  
> Scope: `viso-math`, `viso-gpu`, `viso-shader`, `viso-render`, render-facing `viso-text`, `viso-ui`, `viso-widgets`, and platform GPU backends  
> Correctness rule: pixel, color, alpha, coordinate, resource-lifetime, paint-order, and synchronization semantics must never be weakened by optimization.  
> Optimization priority: **frame stability > hot-path CPU > GPU bandwidth/fill-rate > memory/VRAM > cold-path setup cost > implementation complexity.**  
> Construction rule: **foundation first, then drawing, then composition, then effects, then materials, then advanced optimization. Upper layers never become prerequisites of lower layers.**  
> Governing ADRs: 0001 (renderer primitive contract), 0002 (layer offscreen compositing), 0012 (`viso-math`), 0017 (Shader IR), 0025/0026 (text/glyph cache & color glyph), 0029 (static GPU backend selection). On conflict the Accepted ADR wins until superseded.

---

# 0. Vibe Coding Execution Contract

This document is the implementation order. Do not invent a second roadmap. Progress/status lives only in `todo_rendering.md`; this file holds contracts and plans, not checkbox state.

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
1. Audit existing code; assign every module/type to the current or a lower layer (§35); remove reverse dependencies first.
2. Implement the smallest independently verifiable unit.
3. Run the stage gate (below) + the stage's §33 tests/benchmarks; check §30 counters.
4. Focused commit per verified unit; repeat until the §33 exit criteria hold.
5. Freeze the completed layer's contract (§3.1); only then enter the next stage.
```

Stage gate (every stage, every unit):

```text
cargo xtask check-deps
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p viso-render --test golden
cargo bench -p viso-render -- --test   (renderer_steady_state smoke; full run for perf claims)
```

Hard rules:

- Do not implement Material/Glass to discover missing Rect/Clip/Blur foundations.
- Do not use Compute/Bindless/Indirect as a workaround for an inefficient basic renderer.
- Do not expose public Custom Shader/Canvas contracts before the standard renderer ABI is stable enough to support them without back-driving the foundation.
- Do not add a general RenderGraph before real multi-pass requirements exist; begin with the smallest render-pass model required by the current stage.
- `unsafe` / SIMD / backend fast paths follow §22/§23; foundation defects follow §3.
- Same-stage parallel work is allowed; cross-stage back-driving is not (e.g. D1 RRect shader and RRect instance layout may proceed in parallel; changing the F4 instance model into a material-specific layout for M1 may not).

Normative vocabulary:

- **必须 / 禁止 / 不得** = testable contract, frozen after the stage gate; **建议 / 推荐 / 可以** and "实现指引" = guidance subordinate to the section contract.
- **[target]** = benchmark acceptance goal, not a measured claim; **[hypothesis]** = reasoning that must be confirmed by the named §31 benchmark before it drives a default heuristic.

---

# 1. 文档目标

本文定义 Viso 2D/应用渲染内核从基础到外层的唯一实现分层与依赖合同，回答：

Shader 属于哪一层、各 primitive 由谁拥有、实现先后、zero-copy / unsafe / SIMD 边界、analytic / mesh / compute 的选择、何时需要 offscreen、效果建立在哪些基础上、高刷新率下每帧允许与禁止什么。

本文不是 Widget API 清单，也不是视觉材质规范：

- 完整绘画/效果语义在本文对应 D/C/E/M 阶段定义；
- Glass / Frosted / Liquid Glass 的材质语义由 `Viso_Visual_Materials.md` 承接——**该文档尚未创建**；在它存在之前，M0/M1 的材质参数语义以本文 §18/§19 为唯一来源，且不得超出本文定义的底层能力；
- 字体 shaping / glyph representation 由 `Viso_Text_Font_Runtime.md` 承接（ADR 0025/0026）；
- 本文同时定义 **Rendering Foundation、Core Implementation、Drawing/Composition/Effects 设计与严格施工顺序**。

---

# 2. 核心边界：Drawing 不是 Shader

Viso 必须严格区分 Drawing Semantics、Renderer Runtime、Shader Programs、GPU Backend；它们不是同一个层。

```text
RoundedRect
├── geometry / radius / bounds / transform / clip    -> viso-render
├── retained primitive identity                      -> viso-render
├── instance packing / batching / dirty upload       -> viso-render
├── analytic coverage program                        -> viso-shader
└── buffer / pipeline / command / surface            -> viso-gpu

Blur
├── sigma / radius / edge mode / effect bounds       -> viso-render
├── ROI / offscreen / pass reuse / target lifetime   -> viso-render
├── blur kernel                                      -> viso-shader
└── texture / dispatch / barriers / synchronization  -> viso-gpu

Glass
├── material semantics                               -> visual material layer (§18/§19)
├── backdrop dependency / ROI                        -> viso-render
├── blur / distortion / color kernels                -> viso-shader
└── texture / pass / sync                            -> viso-gpu
```

> **Shader 是绘画系统的执行基础设施，不拥有 Rect、Path、Shadow、Blur、Glass 等高层语义。**

`viso-shader` 不得知道 Widget、Node、ClipChain、Material Widget 或 Layout。`viso-gpu` 不得知道 Rect、Text、Shadow、Button、Glass。

---

# 3. 唯一依赖方向

Viso Rendering Foundation 的规范依赖链固定为：

```text
F0  Pixel / Math / Color Foundation
F1  GPU Foundation
F2  Shader Foundation + Typed GPU ABI
F3  Retained Render Scene
F4  Persistent Data Path / Upload / Batch
D0  Rect Baseline
D1  Analytic UI Shapes / Border / Line
D2  Brush / Gradient / Image
D3  Path / Bezier / Fill / Stroke
C0  Clip / Mask / Group / Blend
E0  Analytic Shadow
E1  Offscreen / Blur / ROI / Transient Targets
E2  Backdrop / Color Effects / Advanced Blend
M0  Frosted / Generic Glass Material
M1  Platform Materials / Liquid Glass / HDR Integration
A0  Compute Vector / Bindless / Indirect / GPU Culling
        (each line depends only on the lines above it)
```

硬规则：

- `F0~F4` 是地基；`D0~D3` 是基础绘画；`C0` 是合成基础；`E0~E2` 是效果；`M0~M1` 是外层材质；`A0` 是高级优化，不是基础功能前置；
- 不允许为了实现 Glass 而让 `Rect` 依赖 Effect Graph；不允许为了 GPU Compute Path 而让普通 Button 先经过 Compute Queue；不允许为了统一 API 把最常见 primitive 强制走最昂贵 representation；
- 上层只能依赖已通过 Gate 的下层合同，禁止以“上层实现方便”为理由反向修改地基；
- 下层合同若存在 correctness 缺陷或经 benchmark 证明的结构性性能阻塞，必须先作为 foundation defect 单独修复并重新通过该层 Gate，未重新通过前不得继续向上扩展。

## 3.1 阶段冻结规则

```text
implement current layer -> correctness -> golden/reference comparison -> CPU benchmark
  -> GPU benchmark -> memory/resource benchmark -> cross-backend validation
  -> PASS -> freeze lower-layer contract -> enter next layer
```

冻结的含义：public semantic contract、stable GPU/renderer ownership、resource lifetime contract、pixel/color/alpha semantics、hot-path invariants 全部 frozen。冻结由对应 `*_contract_frozen.rs` / `render_contract_1_0.rs` 测试钉住（§33）。

实现内部仍可在不改变上述合同、且 benchmark 证明有收益的前提下继续优化，例如替换 SIMD kernel、改变 buffer suballocation 算法或增加 backend-specific fast path。

## 3.2 总方案：Adaptive Multi-Lane Retained Renderer

Viso 不选择“所有东西都 SDF”“所有东西都 tessellation”“所有东西都 GPU compute”中的任何一种。

```text
UI / Canvas / Text / SVG / Game
              ↓
       Retained Paint IR (§8)
              ↓
     Primitive Classifier
   ┌──────────┼───────────┬──────────────┬────────────┬─────────────┐
Analytic   Atlas       Vector Mesh    Vector Compute  Mesh/3D    Effect/Material
Lane(§11)  Lane(§12)   Lane(§13)      Lane(§20.1)     Lane(§21.4) Lane(§16-§19)
   └──────────┴───────────┴──────────────┴────────────┴─────────────┘
                              ↓
                    Order-safe Batch Planner (§9.6)
                              ↓
                       Effect / Clip Planner (§14, §17.5)
                              ↓
                     Reusable RenderGraph Plan (§25)
                              ↓
                       Backend Command Encode
                              ↓
                    Metal / D3D12 / Vulkan / WebGPU (ADR 0029)
```

选择原则：简单 UI 几何 → analytic instance；图片/glyph/sprite → atlas/image instance；稳定 path → retained cached geometry；大量动态 path → compute vector lane（§31.4 允许时）；需要邻域采样/目标读取 → effect/offscreen lane；普通 opacity/color matrix/tint → fuse，不建 offscreen。

Representation decision 挂在 retained primitive / paint chunk 上，不在每个 fragment、每个 glyph 或每帧重新决策；只有相关 revision（§8.2）变化时重新分类。

---

# 4. Crate 所有权

## 4.1 `viso-math`

拥有 allocation-free 基础数学（ADR 0012；f32-primary，仅 `DVec2/DPoint/DRect` 为 f64）：

```text
Vec2 / Vec3 / Vec4, DVec2 / DPoint / DRect
Mat2 / Mat3 / Mat4, Quat
Affine2, Transform3
Point / Size / Rect / Insets
Aabb / Ray / Plane
```

所有 public 类型 `#[repr(C)]` + `Copy`，不含 `usize`。Renderer 2D 热路径优先使用 `Affine2 / Vec2 / Rect`；普通 2D transform 禁止仅因为 API 统一而全部使用 `Mat4`。

`viso-math` 不拥有 Brush、Path、Clip、Primitive、GPU ABI、Shader layout（Math ABI ≠ GPU ABI）。

> 注：Architecture `viso-math` 节称 math 不负责 Color 语义，但当前 F0 值类型（`color.rs`、`coverage.rs`、`snap.rs`、`legality.rs`）位于 `crates/math`。F0 合同与所在 crate 无关；归属待 ADR 裁决。

## 4.2 `viso-gpu`

只拥有极薄 RHI：Device、Queue、Surface、Buffer、Texture、TextureView、Sampler、RenderPipeline、ComputePipeline、ResourceTable/BindGroup、CommandEncoder、RenderPass、ComputePass、Fence/Epoch、RetireQueue、generational SlotMap。

禁止 UI 或绘画概念（Primitive、Brush、Path、Clip、Effect）进入 `viso-gpu`。

## 4.3 `viso-shader`

拥有 typed Shader IR（ADR 0017，唯一 source of truth）、IR builders、`.vs` shader parser/type checker、reflection、instance/uniform/storage schema、backend codegen（MSL/HLSL/WGSL）、pipeline ABI validation、PipelineManifest、`ShaderPipeline{last_good}` dev hot reload、self-contained diagnostics。

不拥有 Paint order、PrimitiveStore、Clip semantics、Effect graph、Widget material、Render damage；不依赖 `viso-dsl`。

## 4.4 `viso-render`

拥有：retained paint scene、primitive stores、transform / bounds、brush / image references、clip / mask semantics、paint order、batch planner、persistent instance residency、GPU upload plan、path geometry cache、effect planner、render graph、transient target planner、damage/culling、frame packet、选择 pipeline family 与准备 resource binding。

## 4.5 `viso-text`

拥有 font resolve、shaping、glyph representation（`GlyphImageKind`: MaskA8 默认 / ScalableMtsdf / OutlineVector / ColorRgba8 / ColorVector，ADR 0025）、glyph residency pools 与 eviction、text correctness。输出稳定的 `GlyphRun` / representation handle 作为 Render primitive 进入 `viso-render`。

`viso-render` 拥有 GlyphRun placement、atlas sample pipeline、clip/blend/effect composition、GPU batch；**不重新 shape 文字**。ColorRgba8 glyph lower 为 `Primitive::Image`（white tint），color atlas 始终 premultiplied（ADR 0026）。

## 4.6 `viso-ui` / `viso-widgets`

负责 Node / Layout / Style / paint invalidation，并把 Widget/Node paint state lower 成 retained render primitive。它们不拥有 GPU buffer、pipeline cache 或 texture barrier。

UI 侧 dirty class 只有规范的 8 个：`STRUCTURE STYLE MEASURE LAYOUT TRANSFORM PAINT HIT_TEST SEMANTICS`。Render 侧不定义新的 dirty class，只维护 revision plane（§8.2），映射见 §24.2。

## 4.7 边界汇总

| 边界 | 本文/`viso-render` 拥有 | 对方拥有 |
|---|---|---|
| Visual Material（`Viso_Visual_Materials.md`，未创建） | backdrop capture、blur planner、effect graph、ROI、transient resources、composite、GPU material primitives | Glass/Frosted/Liquid Glass 语义、Apple system material mapping、MaterialGroup、accessibility adaptation、native material island、platform fallback |
| Text（`Viso_Text_Font_Runtime.md`） | GlyphRun placement、atlas sample pipeline、composition、batch | font resolve、shaping、glyph representation/atlas、text correctness |
| Shader（`viso-shader`） | pipeline family 选择、resource binding、instance data、render graph、batch | IR/parse/type、codegen、reflection、pipeline ABI |

两份规范不能重复发明两套 blur/render graph。

---

# 5. F0 — Pixel / Math / Color Foundation

这是所有绘画之前必须冻结的地基。没有 F0，不允许开始设计高级 Effect。

## 5.1 坐标空间

必须区分：

Logical（dp，ADR 0033：`1px = 1/scale_factor dp`）、Device（physical pixel）、Local（primitive-local）、Parent、World/Window、Surface、Clip（GPU normalized）、Texture（normalized 或 texel）。

不得把 `scale_factor` / device pixel ratio 隐式散落在 Widget 中。规范转换链：

```text
Local --Affine2--> World/Window Logical (dp) --DeviceScale--> Physical Device Pixel
      --backend viewport transform--> GPU Clip Space
```

## 5.2 Surface origin

Viso Render IR 使用统一二维坐标语义：`origin = top-left, +x = right, +y = down`。像素 `(i, j)` 覆盖 device 区间 `[i, i+1) × [j, j+1)`，像素中心为 `(i + 0.5, j + 0.5)`。

各 backend 的 NDC、texture origin、projection 差异只能在 GPU/backend lowering 内处理。Offscreen target 与 surface 使用相同 origin 约定，composite 时不做 Y-flip（ADR 0002）。

## 5.3 Rect 边界规则

2D `Rect` 使用半开区间 `[min_x, max_x) × [min_y, max_y)`（ADR 0012）：

- `Rect::contains(p)` 半开：`min ≤ p < max`；
- `Rect::intersects(a, b)` 严格：仅共享一条边的两个 rect **不**相交；
- 3D `Aabb` 的 `contains/intersects` 为闭区间，不适用本节。

用于 clip、intersection、damage、culling、pixel bounds、atlas allocation，保证邻接 primitive 在边界处不 double-hit / 不留 gap。

Device-pixel 整数化规则：

| 用途 | 规则 |
|---|---|
| damage / culling / ROI / scissor | 保守外扩：`x0 = floor(min_x)`, `x1 = ceil(max_x)`（y 同），再 clamp 到 viewport |
| atlas allocation | 整数 texel rect + §12.5 gutter |
| 空判定 | 整数化后 `x1 ≤ x0` 或 `y1 ≤ y0` 即 empty，fast reject |

## 5.4 Color canonical representation

Renderer 内部 canonical blend representation：**linear-light + premultiplied alpha**（`LinearPremul`）。

资源输入可以是 sRGB、Display P3、linear sRGB、extended linear / HDR；每种输入是独立类型，只能通过显式 `into_linear_straight()` / `into_linear_premul()` 进入 working space，不存在隐式转换。

规则：

- blur、gradient interpolation、coverage AA、lighting-like effect、SrcOver 及标准 Porter-Duff 必须在线性光空间执行；禁止在 sRGB 编码值上执行；
- **Wire 表示**：GPU instance 数据与 primitive value 携带 straight linear RGBA（`LinearStraight`）；fragment shader 在输出前 premultiply。纹理（image、glyph color atlas、gradient LUT、offscreen target）存储 premultiplied；
- **输出编码**：写入 sRGB 编码的 surface 时，transfer function 只能执行一次——使用 `*Srgb` format/view 时由硬件编码，否则由最终 composite shader 编码；二者不得同时生效，也不得都不生效；
- 链路：public color / image profile → target working space → linear-light 运算 → GPU premultiplied → output transfer（exactly once）。

SDR 常规 surface 不要求全部变成 RGBA16F；只有 HDR、wide-gamut effect chain、high-precision intermediate 才使用 FP16/更高成本 target（§19.2）。

## 5.5 Alpha

- GPU 内部 canonical：premultiplied alpha；
- 同一 pipeline 禁止一部分按 straight、一部分按 premultiplied；
- 标准 SrcOver blend state：`color = src + dst·(1 − src.a)`，即 `(One, OneMinusSrcAlpha)` 同时作用于 RGB 与 A；
- 普通 blend 热路径禁止 `unpremultiply → blend → premultiply`；只有 W3C non-separable / 部分 separable blend（§14.6）与 color matrix（§17.3）可以在 shader 内局部 unpremultiply，且 `a = 0` 时结果定义为全零；
- Shader 输出、texture format metadata 与 blend state 必须一致，由 `color_domain.rs` / `blend_contract.rs` 测试钉住。

## 5.6 Anti-aliasing coverage

Coverage 统一定义：

```text
coverage ∈ [0, 1]            (Coverage(f32)：clamp，NaN -> 0)
output = premultiplied_color * coverage
```

Analytic primitive、Path AA、glyph mask 最终都必须映射到同一 coverage/composite 语义。

Analytic edge 合同：

- coverage 在 **device space** 计算：`d` 为像素中心到几何边的 signed distance（外正内负），以 device px 为单位；非均匀 scale / 旋转下通过 `fwidth` 或 transform Jacobian 把 local 距离换算为 device px；
- 过渡带宽度为 **1 device px**，以几何边为中心：`coverage = clamp(0.5 − d, 0, 1)`（或等价的 box-filter 近似，误差由 golden 容差约束）；
- 结果在 subpixel 平移下连续（不因 translation 改变 fringe 宽度），不依赖重建 geometry fringe；
- 默认不对整个 UI surface 开 4x/8x MSAA。MSAA 只用于 3D、特定 mesh/vector pass，或经 backend benchmark 证明收益的场景（§13.4 的 AA 选择）。

## 5.7 Pixel snapping

`PixelSnap` 在 **device space、完整 transform 之后** 应用；只对 axis-aligned（scale + translate，无旋转/斜切）的 effective transform 生效，否则按 `None` 处理：

| Mode | 定义 |
|---|---|
| `None`（默认） | 不 snap，按精确 subpixel 位置光栅化；用于旋转、非轴对齐、自由动画几何 |
| `Position` | origin 各分量 `round()` 到 device 网格，尺寸不变；避免移动中 shimmer |
| `Bounds` | 两条边独立 round：`x0 = round(x)`, `x1 = round(x + w)`（y 同）；用于 crisp 填充 rect/背景 |
| `Stroke` | stroke 的 device 宽度 `w_d` 为奇数时，把几何中心线钉在半像素（`k + 0.5`），偶数时钉在整数；使 stroke 覆盖整像素 |

规则：

- `round` 为 round-half-away-from-zero（`f32::round`），全部实现共用 `viso-math` snap 函数；
- 默认 Widget 只在需要 pixel alignment 时使用 snapping；不得为了“锐利”对动画中的 transform 强制 round 到整数像素；
- Snap 在 retained commit 时按当前 device scale 解析；device scale 变化时 render 侧 bump 受影响 primitive 的 `TransformRevision` 并重新解析。

## 5.8 Hairline

`Hairline` 是独立语义，不等价于 `stroke_width = 1 dp`：

- 目标宽度以 device px 表示（`Hairline::device_px`，默认 `1.0`）；
- 线宽沿**法线方向**在 device space 保持 `device_px`，与当前 device scale、非整数 scale、旋转无关；
- 与 `PixelSnap::Stroke` 组合时，轴对齐 hairline 覆盖恰好一行/列像素；
- 其 paint bounds inflation 为 `0.5 · device_px` device px 换算回 local。

## 5.9 非法数值

进入 retained render state 的 public geometry 必须在 state commit / cold boundary 验证并分类为 `GeometryLegality`：`Ok / NaN / NonFinite / IllegalExtent / SingularTransform`（名称以 `crates/math/src/legality.rs` 为准）。

- 非 `Ok` 的 primitive 不进入 GPU 数据，按 §5.10 的 degenerate 语义处理（通常 reject），并在 debug 下报告；
- Release 热路径不得处处重复 `is_finite()`。

## 5.10 Degenerate geometry

以下必须有确定语义，优先 fast reject，不 panic，不产生未定义 GPU 数据：

| 输入 | 语义 |
|---|---|
| zero-area rect | fill 不绘制；stroke/hairline 按线段绘制 |
| zero-length line | butt cap 不绘制；round/square cap 绘制点/方块 |
| coincident path points | 去重后按剩余 segment 处理 |
| zero radius | 等价普通 rect |
| radius > half extent | 按 §11.2 normalize |
| singular transform | 面积为 0，fill reject；需要 inverse 的操作（hit-test、local ROI）返回 empty |
| empty clip / empty mask | subtree reject |

---

# 6. F1 — GPU Foundation

F1 的目标不是“能画很多效果”，而是建立最小、稳定、可验证的 GPU substrate。

## 6.1 必需对象

```text
GpuDevice  GpuQueue  GpuSurface
GpuBuffer  GpuTexture  GpuTextureView  GpuSampler
GpuRenderPipeline  GpuComputePipeline  ResourceTable/BindGroup
GpuCommandEncoder  GpuRenderPass  GpuComputePass
Fence / Epoch  RetireQueue
```

命名可以在 Rust API 收敛，但职责不可混合。

## 6.2 Buffer usage

至少区分 Vertex、Index、Uniform、Storage、Upload、Readback、Indirect。一个资源可以具有组合 usage，但 backend 必须验证合法性。

## 6.3 Texture formats baseline

```text
R8Unorm  RG8Unorm  RGBA8Unorm  RGBA8UnormSrgb  BGRA8Unorm  BGRA8UnormSrgb
RGBA16Float  Depth / DepthStencil (backend-required format)
```

用途：sampled、render attachment、storage where supported、copy src/dst。Format 的 color-space 属性（Srgb vs Unorm）必须随 texture metadata 传递，供 §5.4 的“只编码一次”规则检查。

## 6.4 Surface 与 device loss

Surface 必须支持：acquire、present、resize、DPI change、format/color-space change、occlusion/minimize、out-of-date recovery。

Device loss 分两级，行为固定：

| 级别 | 触发 | 行为 |
|---|---|---|
| Surface / drawable loss | GPU reset、driver restart、display reconfiguration、abandoned frame | `GpuBackend::device_lost(surface)`：丢弃持有的 drawable；把当前 epoch 视为完成以解除 RetireQueue 阻塞；下一次 `begin_frame` 重新 acquire。Persistent buffer/texture/pipeline 保留。 |
| Full device loss | 设备被移除/重建 | 旧 device 的所有 GPU handle 失效（generation 检查使其 resolve 为 `None`，不得命中新对象）；Renderer 从 retained CPU stores（PrimitiveStore、path geometry、image source、glyph source）重建 GPU residency，从 PipelineManifest 重建 pipeline；retained scene 状态不丢失；恢复后第一帧允许 full upload，并计入 counters |

Full device loss 的重建路径必须有 headless 模拟测试（§32）。

## 6.5 Resource generation

所有 GPU resource handle 必须是 generation-safe：`{index: u32, generation: u32}`，8 字节 `#[repr(C)]`（`viso_gpu::slots::RawId`）。Slot 回收时 generation bump；stale BufferId / TextureId / PipelineId 必须 resolve 为 `None`，禁止在回收或设备重建后命中新对象。

## 6.6 Resource lifetime 与 deferred destruction

CPU 释放引用不代表 GPU 已不再使用。

- 帧以单调 `Epoch` 编号，每次 `begin_frame` 前进；队列 in-order，因此 `Fence::completed = N` 蕴含所有 `≤ N` 的 epoch 完成；
- 销毁的 handle 以 retire epoch 进入 `RetireQueue`；当且仅当 `completed ≥ retire_epoch` 时回收 slot 并 bump generation；
- 禁止资源销毁依赖“希望这一帧已经执行完”；
- frames in flight 为 backend 常量（当前 D3D12/Vulkan = 2），不进入 public ABI。

资源生命周期分类：

| 类别 | 例子 | 生命周期 |
|---|---|---|
| Persistent | atlas、retained mesh、instance pool、image texture、gradient LUT | 显式创建；deferred destroy；受 §28.3 byte budget 约束 |
| Frame | small constants、upload ring slice、indirect args | 一个 epoch；fence 完成后整体回收 |
| Transient RenderGraph | blur intermediate、temporary mask、offscreen layer | 一个 graph execution；允许 lifetime aliasing / size-class pool reuse（§16.4） |

## 6.7 UMA 与 discrete GPU

公开模型统一，内部策略不同：

```text
UMA / Apple Silicon / mobile SoC   persistent shared/mapped fast path
discrete GPU                       mapped upload/staging -> transfer/copy -> device-local resource
```

不得为了伪造“物理 zero-copy”而放弃 device-local 性能。

## 6.8 Backend 静态选择与 fast path

Backend 由 cfg 在编译期静态选定（ADR 0029）：`viso_gpu::Backend` 是 type alias —— apple → Metal，Windows → D3D12，Linux/Android → Vulkan，wasm32 → WebGpu，其余 → HeadlessRaster。`vulkan` feature 只用于交叉测试编译 Vulkan 模块。Tier-2 backend 以 module + cfg 加入，能力差异通过 `Caps` 表达。

禁止在热路径对每个 primitive 做 `Box<dyn GpuBackend>::draw(...)` 式 virtual dispatch。

实现指引（统一语义，backend 可不同实现）：Metal —— unified memory、argument table、memoryless attachment、native material（M1）；D3D12 —— upload/descriptor heap、PSO cache、barriers、indirect；Vulkan —— host-visible staging、descriptor indexing、transient attachment、pipeline cache；WebGPU —— persistent buffers、batched writes、binding arrays（可用时）、compute 仅在支持且测量后。

Core 不为了 WebGPU 的最低能力限制 Native backend。

---

# 7. F2 — Shader Foundation 与 Typed GPU ABI

## 7.1 Release 不运行时临时编译标准 shader

Typed Shader IR 是唯一 source of truth（ADR 0017）：

```text
built-in pipelines:  Rust-side IR builders (quad_ir / image_ir / glyphrun_ir / mesh_ir)
user .vs shaders:    build-time Parser / Type Checker
                         ↓
                 Typed Shader IR
                         ↓
         Reflection + Layout Validation (§7.3)
                         ↓
    Backend Codegen: MSL / HLSL / WGSL (+ backend artifact)
                         ↓
                  PipelineManifest
```

- Release 标准绘制路径不得因为第一次出现某个 Button 才解析/编译 shader source（`metal_no_runtime_compile.rs`）；
- Dev 模式 hot reload 通过 `ShaderPipeline{last_good}`：候选 IR 编译失败时保留 last-good，诊断 self-contained，不依赖 VM 或 `viso-dsl`。

## 7.2 最小 Shader Surface

基础只要求 vertex、fragment、uniform、instance、storage、texture、sampler、varying。Compute 在 F2 可以具备语言能力，但 D0~D3 绘画不得依赖 compute 才能成立。

## 7.3 Typed GPU ABI

禁止“Rust struct 某段内存碰巧符合 shader layout”。GPU 数据必须通过显式 ABI：

```rust
#[repr(C)]
#[derive(GpuPod)]
struct SolidRectInstance {
    rect: [f32; 4],
    color: [f32; 4],      // LinearStraight (§5.4)
    transform_id: u32,
    flags: u32,
    _pad: [u32; 2],
}
```

`#[derive(GpuPod)]`（`viso-macros`，经 `viso-gpu` re-export；早期文档称 `GpuInstance`）在编译期拒绝（compile-fail，不是 runtime）：

```text
non-#[repr(C)]         pointer / reference
bool ABI ambiguity     usize / isize
enum layout assumption uninitialized/implicit padding
non-fixed-width scalar non-Copy
```

并生成 `const LAYOUT: InstanceLayout`（`offset_of!` 真值：size、alignment、member offset、stride）。

Pipeline 注册时，`InstanceLayout::validate_against` / `validate_attrs` 用 shader reflection 交叉校验 count / name / format / offset / stride，失败返回 `OffsetMismatch` / `StrideMismatch` 等错误，不得静默继续。Instance layout 由 `instance_abi_frozen.rs` 冻结。

## 7.4 Zero-copy 的准确语义

> **CPU Paint Data -> GPU Upload Data 不进行无意义的中间序列化与重复 copy。** 不承诺离散 GPU “物理零 copy”。

```text
typed &[T: GpuPod]
  -> typed byte view (no conversion)
  -> directly write mapped upload/ring memory
  -> UMA: GPU may consume shared memory directly
     discrete: DMA/copy dirty range to device-local resource
```

禁止 `Vec<Instance> -> Vec<f32> -> Vec<u8> -> staging Vec<u8> -> mapped buffer` 这类多层转换。

## 7.5 Pipeline variant 与 manifest

不要建立一个巨大 Uber Shader。基础 pipeline families（`PipelineFamily`）：

```text
SolidRect  AnalyticRRect  AnalyticEllipse  AnalyticLine
Image  Gradient  PathFill  PathStroke  MaskComposite
```

`VariantKey` 是 packed integer，只包含真正改变 pipeline 的维度：fixed-function state、resource layout、shader family、sample count、depth/stencil behavior。color、radius、shadow color、opacity、gradient angle 等是数据，进 instance/uniform。

Pipeline manifest / cache（predictable 策略）：

- 标准 pipeline 在 build 时枚举；app custom shader 在 build 时编译与 reflect；runtime 只 cache PSO/pipeline objects；
- Baseline primitive pipeline 在 Device 初始化或首屏前预热；
- 少见 variant 可以 background/lazy create，但**不得在需要当前帧立即展示的 animation hot path 同步做 pipeline compile**。Lazy 创建发生在 retained commit（cold path），不在 frame encode；动画帧内出现首次 pipeline 创建计入 `shader_pipeline_creations`，steady-state bench 以此为失败条件。

---

# 8. F3 — Retained Render Scene

Viso 不是每帧重录全部 Canvas command 的 immediate renderer。标准 Widget 和普通 Component 不应该每帧重新生成完整 Canvas command list。

## 8.1 Retained Paint IR

```text
Widget / Node
  ↓ paint lowering
PaintChunk
  ↓
stable PrimitiveId ranges
  ↓
PrimitiveStore
  ↓
only dirty fields/ranges changed
```

没有变化时：0 primitive reconstruction、0 path retessellation、0 brush reconstruction、0 string lookup。

当前 producer 以稳定 pre-order 重新发出 primitive 流，scene ingest 按位置分配 slot 并 diff：未变化 primitive 不产生任何 mutation（`scene_diff.rs`）。

## 8.2 Typed IDs 与 revision planes

所有 retained 身份是 fixed-width dense generational handle：`{index: u32, generation: u32}`，8 字节 `#[repr(C)]`，与 `viso_gpu::slots::RawId` 同形；每种 ID 是 `#[repr(transparent)]` newtype，不可互换。禁止把稳定身份建立在 pointer、`usize`、Rust object address、native handle 之上。

唯一 ID 表（`crates/render/src/scene/ids.rs` 与 GPU 侧）：

| ID | 标识 | 所在层 |
|---|---|---|
| `PrimitiveId` | 一个 retained primitive | scene |
| `PaintChunkId` | 一个 widget/subtree 的 paint 输出（lowering 单位） | scene |
| `RenderChunkId` | 由一个或多个连续 primitive range 组成的 batch/增量单位（§9.7）；一个 PaintChunk 映射到 ≥1 个 RenderChunk | render batch |
| `TransformId` | transform store 条目 | scene |
| `BrushId` | brush 条目（§12.1） | scene |
| `ClipId` | 单个 clip item | scene |
| `ClipChainId` | retained clip chain（§14.2） | scene |
| `GeometryId` | retained 几何结果（analytic 参数或 tessellation 输出） | scene |
| `PathId` | path source（§13.2） | scene |
| `MeshId` | mesh geometry | scene |
| `ImageId` | image resource 引用 | scene |
| `EffectChainId` | effect chain | scene |
| `MaterialId` | material 实例（M0/M1） | scene |
| `TargetId` / `PassNodeId` | transient target / graph pass | render graph |
| `BufferId` / `TextureId` / `SamplerId` / `PipelineId` | GPU 资源 | viso-gpu |

每个 primitive 维护独立 revision plane（`scene/revision.rs`）：

```text
GeometryRevision  PaintRevision  TransformRevision  ClipRevision
ResourceRevision  EffectRevision  VisibilityRevision
```

Revision plane 不是 dirty class；UI dirty class 到 revision plane 的映射见 §24.2。一个 opacity 变化不得逼迫 Path 重新 tessellate。

## 8.3 Stores

按类型紧凑存储，不使用 `Vec<Box<dyn Primitive>>` 对每个可见 primitive 做虚调用：

```text
PrimitiveStore
├── SolidQuadStore      ├── AnalyticShapeStore   ├── ImageStore
├── GlyphRunStore       ├── VectorPathStore      ├── MeshStore
├── ClipStore           ├── EffectStore          └── CustomStore
TransformStore  BrushStore  GeometryStore  ImageRefStore
```

Public 层可以对象化；Render hot storage 使用 dense typed arrays / indexed SoA / compact AoS。热字段紧凑，冷字段进入 side table（§21.1）。

## 8.4 Transform 与 primitive 分离

纯移动/动画只使 `TransformStore` 条目与 `TransformRevision` 变化，不得 rebuild Rect geometry、rebuild Brush、retessellate Path（例外见 §13.4 quality bucket）。

## 8.5 Bounds

每个 primitive 维护：

```text
local_bounds
world_bounds
clip_bounds
paint_bounds  = geometry bounds + stroke inflation (§11.3) + shadow/effect outsets
effect_bounds where applicable
```

- `paint_bounds` 必须保守（包含全部非零 coverage 像素，含 1 device px AA 过渡带）；
- 用于 culling、damage、ROI、hit-test helper、backdrop dependency；
- Bounds 在 geometry commit 时计算并缓存，不重复解析 Path；
- Effect 阶段再计算严格 ROI（§16.2）。

## 8.6 Paint order

绘制顺序属于正确性合同。Batch Planner 只能在 §9.6 定义的条件下 reorder；不能为了减少 draw call 按 texture/pipeline 全局排序透明 UI。

---

# 9. F4 — Persistent Data Path / Upload / Batch

这一层是性能地基。

## 9.1 Persistent Instance Pool

稳定 primitive 获得稳定 instance slot：`PrimitiveId -> InstanceSlot -> Persistent Instance Pool`。

- 一个 hover 改变只使对应 slot dirty，而不是 rebuild/upload 全部 instance；
- Pool 是 grow-only device buffer：稳态不创建/销毁 buffer；增长属于 cold path，计入 counters；
- Frame transient 数据使用 upload ring（§9.2）；retained static geometry 使用长期 device resource，不与 ring 混用。

```text
Persistent GPU Instance Pool  <-  dirty ranges  <-  Typed Upload Ring / Staging Arena
```

Backend 策略（实现指引）：UMA 在 benchmark 证明有益时直接 mapped/shared；discrete 用 staging ring + copy to device-local；WebGPU 用 persistent resource + queue write/staging。

## 9.2 Frame Upload Ring

Transient upload 使用按 epoch 分区的 frame ring（Frame N / N+1 / N+2 …），通过 fence/epoch 回收（§6.6）。热路径允许 persistent mapping、`unsafe` pointer bump、aligned typed write，但必须由受控模块封装（§22）。

## 9.3 Dirty Range Coalescer

多个 dirty slot 合并成少量 upload range：`{10, 11, 12, 400}` 上传 `10..13` 与 `400..401`，而不是 4 次 API call。

- Coalescer 使用固定容量 scratch/arena；稳态不 heap allocate；
- 合并策略（相邻/小间隙填充）是实现参数，由 `coalescer.rs` 测试钉住“结果覆盖且仅覆盖 dirty 数据或其保守超集”。

## 9.4 Frame Arena

每帧临时 CPU 数据（visible chunk list、batch scratch、clip scratch、small pass descriptors、sort/radix scratch、graph compile scratch）使用 bump/frame arena：bump allocation、bulk O(1) reset、no per-object free。

禁止热路径为每个 primitive `Box` / `Vec` / `String` / `HashMap` 临时分配。大对象/长期对象进入 persistent cache，不放 frame arena。

## 9.5 Key 与 HashMap 边界

所有 pipeline/resource cache key 是 integer ID（`PipelineId`、`BrushId`、`SamplerId`、`TextureId`、`ClipChainId`、`EffectChainId`、`TargetId`），禁止 hot path string。Batch key 为 packed integer（`BatchKey(u64)`）。

HashMap 只允许在 cold path：resource interning、pipeline lookup on miss、cache construction、asset resolve。Per-primitive 每帧 traversal 使用 dense ID + array indexing、small fixed lookup、sorted/radix keys。

## 9.6 Order-safe batching

默认策略：**order-preserving adjacent batching**。目标不是“draw call 最少”，而是：

> **在正确绘制顺序内最大化连续 compatible instances。**

`BatchKey`（packed `u64`）至少包含：pipeline family / variant、render target / pass、resource table / texture set（需要时）、blend state、sample count、color target class、depth/stencil class。另外，structural clip realization 必须相同。

合并规则：

1. primitive 只能并入**紧邻前一个** draw，且 key 相等、clip 与 pass target 相同；
2. 只有 geometry 位于共享 family buffer 的 family（quad、triangle mesh）可以扩展 run；image / glyph run / composite 各自绑定资源，是独立 batch；
3. **Reorder 条件**：draw B 可以越过中间 draw 集合 M 与更早的兼容 draw A 合并，当且仅当对 M 中每个 draw X，`B.paint_bounds` 与 `X.paint_bounds`（device space，clip 后，含 AA 带）按 §5.3 严格不相交，且 B 与 M 均无 destination/backdrop 依赖（§17.4 nonlocal）。不满足即为 order barrier；
4. “opaque 可任意重排”**不成立**：重叠 opaque 仍依赖顺序；只有引入 depth-ordered opaque pass 后才能放宽，该优化属于 A0，须单独合同。

当前实现只执行规则 1–2（未产生 reorder-safe span），由 `batch_planner.rs` 与 `data_path_contract_frozen.rs` 钉住。

Draw 优化优先级：

```text
1. eliminate unnecessary pass/layer
2. eliminate unnecessary upload
3. retain geometry/instances
4. reduce pipeline switch
5. reduce texture binding switch
6. merge adjacent draw
7. optional multi-draw/indirect (A0)
```

[hypothesis] GPU pass/bandwidth 成本通常高于少量轻量 draw 的成本；由 §31 C0/E1 benchmark 的 pass count 与 GPU time 对照验证。

## 9.7 RenderChunk

单 Node 过细，整棵树过粗。`RenderChunk` 是 batch 与增量更新的中间单位：

```text
subtree/widget paint output -> one or more contiguous primitive ranges -> batch spans
```

Chunk 记录：order range、bounds、primitive ranges、pipeline/material summary、clip chain、effect dependencies、revision。局部节点变动只重建相关 chunk；key 未变化时 batch structure 不动。

## 9.8 多线程模型

```text
UI/Main Thread         state / layout / invalidation
      ↓ Paint ChangeSet
Worker Pool            path preprocess, tessellation, heavy effect metadata, image work
      ↓ staged result
Render Thread / GPU Owner   commit validated ranges, batch/graph patch, encode/submit
```

- 禁止 renderer hot state 在多个 worker 之间通过细粒度 mutex 共享；
- 使用 revisioned jobs、immutable input snapshots、SPSC/MPSC handoff、frame-boundary commit；
- job 结果携带输入 revision；commit 时 revision 过期的结果丢弃。

---

# 10. D0 — Rect Baseline

D0 是第一个完整可见 renderer。只实现：Solid Rect、Affine2 transform、Opacity、SrcOver、Rect Scissor、Surface present。

## 10.1 SolidRect 主路径

`shared unit quad (4 vertices / 6 indices) + SolidRectInstance + SolidRect pipeline`。不为每个 Rect 建独立 vertex buffer。

## 10.2 D0 热路径合同

稳定场景每帧满足 §36 中 D0 适用的零计数（per-primitive alloc / string / HashMap / virtual dispatch、shader compile、full-scene upload），由 `renderer_steady_state.rs` 的 allocation/dispatch invariant 断言（计数，不是计时）。

## 10.3 D0 验收场景 [target]

```text
1 rect / 10k rect / 100k rect synthetic
large scrolling list  single hover dirty
window resize  DPI change  surface recreate
```

记录 CPU build time、CPU encode time、uploaded bytes、draw calls、pipeline switches、alloc count、GPU frame time。绝对时间由 reference hardware profile 建 baseline，不把某个 GPU 的毫秒数写成跨平台 ABI（§31）。

---

# 11. D1 — Analytic UI Shapes

加入：RoundedRect、per-corner radius、RoundedSuperellipse、Circle、Ellipse、Capsule、Line、RectBorder、RRectBorder、Circle/Ellipse Stroke。

## 11.1 Representation

普通 UI shape 默认：`shared bounding quad + compact typed instance + analytic coverage`（§5.6），而不是 tessellation。

```text
Shared Unit Quad + N × Typed Instance -> Instanced Draw
```

最低 primitive family：SolidRect、AnalyticRRect、AnalyticEllipse、AnalyticCapsule、AnalyticLine（含 arc）、DecoratedShape（§15.3）。最常见的纯色 Rect 必须有最短 branch-free fast path。

Radius/size 动画只更新少量 instance fields（rect、radius、border、color、transform），GPU 按局部坐标求 coverage。[hypothesis] 相比每次动画重新 tessellate，可避免 CPU geometry churn、allocator pressure、upload bandwidth 与 batch fragmentation；由 §31 D0/D1 “animated radius/hover” 场景验证。

Analytic/SDF 适用：RRect、circle、ellipse、capsule、line segment、arc、简单 icon/badge、border、simple inner/outer shadow。不要求任意复杂 SVG/Path 转换成 SDF。

## 11.2 Radius 规范化

Per-corner radius 使用唯一算法（CSS overlapping-curves rule，`Corners::normalized`）：

```text
r_i = max(r_i, 0)
f   = min over 4 edges of ( edge_len / (sum of the two radii on that edge) ), capped at 1
r_i = r_i * f
```

所有 radius 用同一个 `f` 等比缩放；Widget 传入 authored radii，禁止自行 clamp。

## 11.3 Border alignment

支持 `Inside / Center / Outside`。Paint bounds inflation 与 alignment 一致：Inside = 0，Center = width/2，Outside = width（均另加 §8.5 的 AA 带）。

Rect/RRect border 不进入 general path stroker；Analytic Lane 直接用 outer/inner distance 求边框 coverage。

## 11.4 Line cap / join 基础

Line/Polyline 基础语义：cap `butt / round / square`，join `miter / bevel / round`，`miter_limit`（超过 limit 的 miter join 退化为 bevel，与 SVG 一致）。复杂 Path Stroke 在 D3 完成。

## 11.5 Analytic shader 分级

```text
Tier A  SolidRect
Tier B  RRect / Circle / Ellipse
Tier C  Fill + Border + Gradient
Tier D  Expanded quad + analytic simple shadow
Tier E  Specialized custom analytic primitive
```

> **最常见 primitive 不应该支付最复杂 primitive 的 ALU、register pressure 和分支成本。**

Pipeline family 在 build-time 枚举；动态颜色、尺寸、hover 等仍只是 instance data。

---

# 12. D2 — Brush / Gradient / Image

## 12.1 Brush

```text
Brush
├── Solid
├── LinearGradient
├── RadialGradient
├── SweepGradient
├── ImagePattern
└── ShaderBrush   (custom shader 合同就绪后，§27.2；D2 不要求)
```

GPU 内部使用 premultiplied alpha。

## 12.2 Gradient stops

```text
2-stop common gradient                -> inline instance colors
small stop count                      -> compact shared stop table
many stops / expensive interpolation  -> cached 1D Gradient LUT atlas (persistent)
```

- 不为每个 gradient 默认绑定独立 uniform buffer；禁止每个 gradient 每帧重新创建 texture；
- LUT key 至少包含：stop colors、stop offsets、interpolation space、extend mode、target color profile class；静态 gradient 只 bake 一次。

## 12.3 Gradient interpolation

- 插值空间由 `InterpolationSpace` 显式指定：`LinearRgb`（默认）、`Srgb`（web 外观）、`OkLab`（保留，bake 时拒绝）；不能偶然由 texture format 决定；
- stop 在所选空间以 straight 形式插值，结果转换回 linear 并以 premultiplied 存储；
- `ExtendMode`（`Clamp` 默认 / `Repeat` / `Mirror`）作用于参数 `t`，区别于 texture address mode。

## 12.4 Image

支持 Image、ImageRect（source rect / destination rect）、fit、alignment、opacity。Sampling：Nearest、Linear、MipmapLinear（可用且合适时）。Sampler 使用 interned `SamplerId`，不创建 per-widget sampler。

资源策略：

```text
small immutable UI image           -> atlas candidate
large / frequently replaced image  -> standalone texture
video / camera                     -> external texture / platform image path
heavy downscale                    -> mipmaps
```

## 12.5 Image edge behavior

明确 `Clamp / Repeat / Mirror`。Atlas/sprite sampling 必须防止相邻 texel bleeding：atlas region 之间保留 gutter（linear sampling 至少 1 texel，mip 链按级别放大），或把 UV clamp 到 region 内缩半 texel。

## 12.6 Nine-slice / Tile / Sprite Atlas

在 ImageRect 稳定后加入 NineSlice、TiledImage、SpriteRegion / atlas region、ImagePattern、External/Video texture。它们复用 Image pipeline family，不建立独立高成本体系。

---

# 13. D3 — Path / Bezier / Fill / Stroke

General Path 在 Rect/UI shape 稳定后实现。

## 13.1 Path commands

最小：`MoveTo / LineTo / QuadTo / CubicTo / Close`。Arc、Conic 等 API lower 为 canonical path segment 或专用 representation。

## 13.2 Path storage

```text
PathArena
├── tags:   compact command stream
└── points: tightly packed f32 coordinates
```

避免每个 segment 都是带 vtable/Box 的对象。Path 创建阶段同时维护/异步计算：bounds、segment count、convexity hint、simple-shape recognition hint、complexity score；render 时不再扫描 command stream。

## 13.3 Fill rule

必须支持 `NonZero` 与 `EvenOdd`。

## 13.4 Vector Mesh Lane（稳定 path 默认路径）

```text
Path -> worker flatten/tessellate -> cached local-space geometry (GeometryId)
     -> GPU vertex/index buffer -> reuse across frames
```

- Geometry 与颜色/opacity 分离：颜色/opacity/transform 变化不得触发重新 tessellate；
- scale 只有超过 flatness/quality bucket 才重新生成，并使用 hysteresis 避免 zoom 临界值抖动；
- 小 geometry 优先 16-bit index，超过范围使用 32-bit；
- static device-local geometry 不与 frame upload ring 混在一起。

AA 表示按 path 类型选择：

```text
simple convex                     -> direct cached geometry + coverage edge
general stable path               -> cached tessellation / coverage representation
self-intersection / complex clip  -> stencil / coverage mask
dense dynamic vector scene        -> tiled GPU compute coverage (A0, §20.1)
```

## 13.5 Stroke

Stroke 合同：width、alignment（语义支持时）、cap、join、miter_limit、dash_array、dash_offset、hairline（§5.8）。

```text
PathRevision + StrokeStyleRevision -> worker preprocess -> retained stroke geometry
```

只有 path 或 stroke geometry 变化时重新处理；颜色变化不重建 stroke geometry。

## 13.6 SIMD 候选

Path preprocessing（bounds、flatness evaluation、segment transform、rect intersection、point classification、stroke preprocessing）可以使用 SIMD；标量实现作为 correctness oracle（§23）。

## 13.7 SVG 与 Path/SVG cache

SVG 是输入格式，不是每帧 XML DOM renderer：

```text
SVG bytes -> parse -> normalized vector scene -> Path/Brush/Stroke -> cached Render IR
```

静态 asset 可以在 build-time 预解析；runtime 动态 SVG 解析放 worker。

Cache key：content hash / `ResourceRevision`、geometry/style revision、quality/tessellation bucket。颜色可与 geometry 分离时，换颜色不得失效 geometry cache。

---

# 14. C0 — Clip / Mask / Group / Blend

Effect 之前先完成合成基础。

## 14.1 Clip ladder

性能合同：

> **Border radius 不等于自动 clip children。普通容器默认 overflow-visible；只有语义明确需要时才 clip。**

```text
Axis-aligned Rect                       -> merged hardware scissor
Axis-aligned RRect / simple analytic    -> analytic clip where profitable
General stable clip                     -> stencil or cached clip mask
Repeated complex clip chain             -> R8 ClipMaskAtlas / cached realization
Effect/isolation required               -> offscreen only when semantics force it (§17.5)
```

- Scissor rect 按 §5.3 外扩整数化（`floor` min / `ceil` max，clamp 到 viewport）。Device 边恰为整数的 rect clip 完全由 scissor 表达；分数边需要 AA 时，外扩 scissor 负责粗裁剪，分数边由 analytic clip coverage 承担；
- 滚动 viewport 应得到一个 scissor，不应仅因为父节点有圆角就创建 offscreen layer。

## 14.2 ClipChain

ClipChain 必须 retained：`ClipChainId -> pre-resolved ClipChainDescriptor`。Geometry 没变化时：no path reparse、no mask re-raster、no per-primitive clip-stack tree walk；不得每帧重新 flatten 整条 clip ancestry。嵌套 axis-aligned rect 在 resolve 时提前求交集。

复杂 `ClipMaskKey` 至少包含：geometry revision、effective transform bucket、device scale、fill rule（`ClipFillRule`）、clip composition（`ClipComposition`）。

## 14.3 Empty clip

空 clip（§5.3 整数化后 empty）立即 subtree reject。

## 14.4 Mask

两个正交维度：

| 维度 | 取值 |
|---|---|
| Mask mode | `AlphaMask`（取 source alpha）、`LuminanceMask`（取 linear luminance × alpha） |
| Mask source | image（`ImageMask`）、path/geometry（`PathMask`）、rendered content |

- 稳定 mask 进入独立 mask cache（mask page 分配）；
- 存储：能用 R8 就用 R8、tight ROI、tile/page allocation；只有真正需要 color 信息时才使用 RGBA；禁止把 mask 永久存成全屏 RGBA texture；
- 高级 mask boolean 可以后续 lane 建立，但不改变基础 mask composition 语义。

## 14.5 Opacity

区分 primitive opacity 与 group opacity。

- **Primitive opacity** 直接乘进 premultiplied color（`fold_child_opacity`：factor clamp 到 `[0,1]`）。
- **Group / Layer opacity** 的规范语义由 ADR 0002（Accepted）固定：
  - `LayerClip.opacity == 1`：in-pass scissor，不创建 offscreen；
  - `LayerClip.opacity < 1`：子树渲染到 offscreen texture，再经 Image pipeline 以 premultiplied over、tint `(1, 1, 1, opacity)` 合成；
  - 所有 offscreen pass 排在 main pass 之前，无 Y-flip，offscreen texture 按 size 池化。
- **Fold-down 优化**（Effect Planner 规则，§17.5）：当 children 可证明互不重叠（disjoint bounds 或单 child，`ChildOverlap::Disjoint`）时，group opacity 与逐 child opacity 等价，可以下推为 per-child 乘法而不建 layer；overlap 或无法判定（`Overlapping` / `Unknown`）时必须 isolation layer（`LayerReason::GroupOpacity`）。
  - 该优化超出 ADR 0002 的字面规则，须由修订/取代 ADR 0002 的新 ADR 批准后才是规范；当前代码（`crates/render/src/opacity.rs`）已实现 fold-down。

## 14.6 Blend

分阶段：

```text
C0 baseline:  Clear  Src  Dst  SrcOver
C0 then:      Plus  Multiply  Screen  Overlay  Darken  Lighten
E2 (§17.3):   DstOver  SrcIn/DstIn  SrcOut/DstOut  SrcATop/DstATop  Xor
              ColorDodge  ColorBurn  HardLight  SoftLight  Difference  Exclusion
              Hue  Saturation  Color  Luminosity
```

实现策略：

```text
fixed-function blend available                         -> fixed function
destination read / framebuffer-fetch fast path          -> backend-specialized path
otherwise                                              -> bounded offscreen composite (LayerReason::AdvancedBlend)
```

- 公式在 premultiplied linear 操作数上定义（W3C Compositing Level 1）；
- 需要 destination read 或 isolation 的 blend 必须显式进入 Effect Planner；
- 复杂 blend 不允许污染最常用 `SrcOver` pipeline。

---

# 15. E0 — Analytic Shadow

普通 UI Shadow 是高频能力，必须有 fast lane。

## 15.1 Fast shapes

Rect、RRect、Circle、Ellipse、Capsule 优先 analytic shadow（`AnalyticShadow`），不默认 `mask -> full texture blur -> composite`：

```text
expanded instance quad + analytic distance + Gaussian-like coverage approximation
```

## 15.2 Parameters

`offset`、`sigma`（Gaussian 标准差，device px；public API 若以 blur radius 表达，必须在 lowering 时以单一固定换算得到 sigma）、`spread`、`color`、`inner`。Paint bounds outset 为 `|offset| + spread + ceil(3σ)`。

## 15.3 Decorated shape fusion

同一 primitive family 可以绘制 outer shadow + fill + border（`DecoratedShape`）。是否一次 draw 完成由 register pressure / overdraw benchmark 决定；语义上不要求业务拆成多个 widget。纯 Rect 必须保留更短 pipeline，不能强制所有 Rect 进入大 shader。

## 15.4 Arbitrary path shadow

只有 general path shadow 才允许进入 `Path/Mask -> tight shadow mask -> blur -> offset/color composite`（`PathShadow`）。

稳定 path 缓存**未着色**的 blurred mask：same geometry + same sigma、只改 shadow color/offset 时复用。

## 15.5 Inner shadow

简单 analytic geometry 直接用距离函数实现（analytic lane）；general path 使用 mask/filter lane。

---

# 16. E1 — Offscreen / Blur / ROI / Transient Targets

到此阶段才建立完整 offscreen infrastructure。

## 16.1 为什么不能更早抽象完整 RenderGraph

基础 Rect/Path 不需要复杂 graph。先由真实需求产生 main pass、mask pass、shadow blur pass、offscreen group，再抽象 RenderGraph，避免过早为理论通用性引入 pass/node/edge 开销。

## 16.2 Tight ROI

任何 offscreen effect 必须先求最小必要区域：

```text
ROI = (content/effect bounds + kernel expansion) ∩ clip bounds ∩ surface bounds
      then §5.3 outward integer rounding
```

禁止小 widget 的 blur 默认处理整个 surface（例如 200×80 panel -> full-screen copy -> full-screen blur -> crop back）。

## 16.3 Blur ladder

- Gaussian 支撑半径 `r = ceil(3σ)`（device px）；σ 不超过最小阈值时不规划 blur pass；
- 按有效 σ / ROI / backend 选择：

```text
small   -> direct separable Gaussian (two passes)
medium  -> optimized separable / compute where profitable
large   -> downsample pyramid (integer factor d so ceil(3σ/d) ≤ tap budget) -> blur -> upsample
```

- 阈值与 tap budget 是 benchmark 参数（当前 tap budget = 32），不进入 public ABI。

## 16.4 Transient Target Planner

目标：tight bounds、appropriate format（§19.2）、appropriate sample count、reuse、alias。

- 使用 lifetime analysis、size/format/sample compatibility、alias/reuse、frame-local pool，而不是每个 effect `create_texture()` / `destroy_texture()`；
- Pool 按 format、usage、size class/bucket、sample count 复用；
- 必须避免：每个 shadow 一个新 texture、每个 clip 一个 RGBA texture、每个 MaterialSurface 一张全屏 texture；
- 稳态重复帧不分配新 target、不重新规划（`offscreen_contract_frozen.rs`）。

## 16.5 Tile-based GPU

[hypothesis] iOS/Android tile GPU 对 attachment store/load 与 offscreen bandwidth 敏感；由 §31 mobile 120Hz 场景验证。因此：

```text
avoid unnecessary pass break / render-target switches
avoid full-screen intermediate
keep effects ROI-bounded; avoid large transparent overdraw
allow transient/memoryless attachments when backend can
fuse passes only when it lowers measured bandwidth
```

Effect Planner 必须能看到 tile-friendly cost。Backend 可以拥有 tile-GPU specialized implementation，不强迫 desktop D3D12/Vulkan 使用相同策略。

---

# 17. E2 — Backdrop / Color Effects / Advanced Blend

## 17.1 Backdrop capture

Backdrop 是依赖“后方已经绘制内容”的明确语义。它必须通过 RenderGraph dependency 表达，不允许 Widget 自己读取当前 framebuffer 的未定义状态。

```text
content behind material -> BackdropDependency -> capture only required ROI
  -> shared blur/downsample resource when possible -> material composite
```

## 17.2 Shared backdrop

同一区域多个 Frosted/Glass material：shared capture + shared blur pyramid（兼容时）+ multiple composites。不得 N 个 widget = N 次 full-screen capture + N 次 blur；必须允许 union ROI、shared backdrop pyramid、MaterialGroup、shared effect pass。相同 sigma 的 backdrop layer 共享一次 capture、一条 blur ladder。

## 17.3 Color effects

支持 Brightness、Contrast、Saturation、HueRotate、Grayscale、Sepia、Invert、ColorMatrix、Tint，以及 §14.6 的 E2 blend 集合。

连续兼容 effect 必须合并为一个 color op（§17.6）；禁止在可数学合并时产生 `Brightness pass -> Contrast pass -> Saturation pass`。

## 17.4 Effect 分类：Local vs Nonlocal

**Local**（`EffectLocality::Local`）——不要求邻域像素或 destination read，必须尽量 fuse 进现有 draw shader：opacity、tint、color matrix、brightness、contrast、saturation、simple gradient、simple mask、fixed-function blend states。

**Nonlocal**（`EffectLocality::Nonlocal`）——需要 neighbor samples、previous framebuffer 或 group isolation，才考虑 offscreen/render target：blur、backdrop blur、large/general shadow、destination-dependent advanced blend、displacement、group opacity with overlapping children。

## 17.5 Effect Planner 与 LayerReason

每个潜在 layer 必须记录 `LayerReason`：

```text
GroupOpacity  ImageFilter  BackdropFilter  AdvancedBlend
Isolation  ComplexMask  SnapshotCache  NativeMaterialBoundary
```

Planner 按顺序尝试消除 layer（elimination ladder）：

```text
1. Can push opacity into children?   (§14.5 fold-down)
2. Can fuse color matrix?            (§17.6)
3. Can replace clip layer with scissor? (§14.1)
4. Can use analytic shadow?          (§15.1)
5. Can share backdrop?               (§17.2)
6. Can collapse adjacent effects?
```

只有全部不成立才创建 offscreen。

Planner 同时回答：最小 ROI 是多少（§16.2）、哪个 transient target 可复用（§16.4）、是否需要 destination/backdrop。

> **Offscreen 是昂贵实现机制，不是方便的默认 API 语义。**

## 17.6 Color filter fusion

brightness、contrast、saturation、tint、opacity 等连续 local color effect 是 straight linear RGBA 上的仿射变换 `out = M · (r, g, b, a, 1)`（4×5 `ColorMatrix`），连续仿射 stage 由 `fuse` 相乘为单个 `ColorOp`，在一个 fragment 内求值（shader 内按 §5.5 局部 unpremultiply），最后一个 op 附着在 layer 的 composite draw 上。只有不能表达为该仿射形式的 stage（如 per-channel gamma）才关闭当前 run、产生额外 pass。

## 17.7 Effect damage

Backdrop/blur 不能只看自身 property dirty。每个 backdrop capture 维护 retained `BackdropDependency { roi: Rect, revision: u64, dirty: bool }`：

- `revision` 由 ROI 内被采样内容的 content stamp 派生，只做相等比较；
- ROI 背后内容变化时只让对应 capture 及其 material/effect ROI dirty；其他 backdrop 不受影响；
- 内容与 ROI 都未变化时复用上一帧 effect 结果。

---

# 18. M0 — Frosted / Generic Glass

M0 是材质层，不是 Renderer 地基。建立在 BackdropCapture、Blur、ColorTransform、Mask、Border/Highlight、Shadow、Shared ROI 之上。

典型 Frosted：

```text
backdrop -> blur -> saturation/tint -> optional noise -> material mask -> border/highlight
```

- 材质参数与平台语义由 `Viso_Visual_Materials.md` 定义（**未创建**；在此之前以本节与 `FrostedMaterial` / `MaterialLane` 类型为准）；本文只定义它必须复用 E1/E2 的底层能力，不得新增 blur/graph 实现；
- 多个相邻/重叠 material 按 §17.2 共享 capture 与 blur；
- 静态 UI 不因 blur/glass 存在而持续重绘（§24.4）。

---

# 19. M1 — Platform Material / Liquid Glass / HDR

## 19.1 Platform material

平台高级材质最后实现。Apple 平台可以存在 Native System Material Lane（`NativeMaterialRegion`，`LayerReason::NativeMaterialBoundary`）与 GPU Material Lane。选择取决于系统集成要求、视觉一致性、可组合性、动画需求、性能 profile、是否需要 Viso GPU 内容参与。

不得为了 Liquid Glass 把平台私有 API 泄漏到通用 Render IR。

## 19.2 HDR / Wide Gamut

建立在 F0 color contract、F1 surface format 与 E 层 effect correctness 之后。至少区分 SDR sRGB target、wide-gamut target、HDR target。

- Blur、Glass、Gradient、Blend 的 intermediate format 由 RenderGraph planner 按需求决定；
- 禁止所有 offscreen 一律 RGBA16F；也禁止 HDR scene 中途降成 8-bit sRGB 再升回去。

---

# 20. A0 — Advanced GPU Optimization

A0 不作为普通 UI correctness 前置。包括：GPU Compute Vector、path binning、GPU culling、bindless / descriptor indexing、indirect draw、multi-draw、GPU-driven batching、advanced compute effects、depth-ordered opaque reorder（§9.6）。

## 20.1 Vector Compute Lane

只有 §31.4 benchmark 证明获益时使用，典型条件：high path count、high path mutation rate、large total segment count、frequent clipping/compositing、CPU tessellation 成为帧瓶颈、GPU compute capability 足够。

```text
Path Segment Scene -> tile/binning -> parallel prefix / allocation -> per-tile coverage -> fine raster
```

具体算法不是 public ABI。

> **Compute Lane 是 workload specialization，不允许因为它“高级”就让普通 Button/Panel/几十个稳定 Path 经过 compute dispatch。**

## 20.2 Bindless / resource table

能力检测：Metal argument/resource tables、D3D12 descriptor heap、Vulkan descriptor indexing、WebGPU binding arrays（where available）。可用时进入 resource-table fast path（`instance.texture_index`），避免 texture-change batch break；不可用时回退 atlas + small texture set + bind-group batching（`BindingModel`）。

Public Paint API 不因 backend 不同而改变。

---

# 21. Render 数据布局

## 21.1 Hot/Cold 分离

Hot：primitive type、transform id、clip id、bounds、brush id、instance slot、render flags。  
Cold：debug name、source span、inspection metadata、rare effect parameters、accessibility cross-reference。

热遍历不得拖着冷字符串和 `Arc` graph。

## 21.2 AoS / SoA

不做教条统一：GPU instance upload 在匹配 GPU fetch/layout 时用 AoS；CPU culling/bounds scan 在 SIMD scan 有明显收益时用 SoA 或 hybrid。具体 lane 的 layout 由 benchmark 决定。

## 21.3 Fixed-width ABI

Public/stable IDs 和 GPU ABI 禁止 `usize`；使用 `u16 / u32 / u64`、`f32`、`f16`（显式支持处）。保证 32-bit / 64-bit / wasm32 语义一致。

## 21.4 2D 与 3D 共存

2D Renderer 与 3D Scene 共用 GPU Device、Resource Manager、Shader compiler、RenderGraph、Frame scheduler、Profiler；但 2D primitive pipeline 不因为 3D 存在就引入 per-node depth object、material graph overhead、scene graph traversal。3D 作为独立 pass/lane（Mesh/3D Lane）接入。

---

# 22. `unsafe` 性能策略

Viso 允许 **有证据的 unsafe 优化**，但必须局部化。

允许区域：GPU mapped-memory writes、`GpuPod` slice cast、arena bump allocation、SIMD intrinsics、validated unchecked indexing in proven hot loops、platform/backend FFI、backend command encoding。

每个 unsafe block / module 必须有：

```text
SAFETY invariant (owner / lifetime / alignment)
debug assertion where possible
safe/scalar reference path or test oracle
fuzz/property tests where appropriate
sanitizer/Miri coverage for applicable code
benchmark evidence (§31.5)
```

禁止：用 unsafe 掩盖 ownership / 边界问题；依赖 Rust 默认 layout 或未验证 layout 就 reinterpret；跨帧保存可失效 raw pointer；跨线程裸指针无所有权合同；把 native / GPU pointer 当 stable ID。

---

# 23. SIMD 策略

## 23.1 候选热点

Affine transform batches（bounds/points）、Rect intersection、bounds propagation、culling、Bezier flatness/bounds、stroke preprocessing、color conversion、image swizzle、mask CPU fallback、dirty-bit/range scan。

## 23.2 Dispatch

- `viso-math` kernel：按 target 在**编译期** `#[cfg]` 选择（x86_64 → SSE2，aarch64 → NEON，wasm32+simd128 → wasm128，其余 → scalar），使用 `core::arch`，不使用 `std::simd`、feature flag 或 runtime dispatch（ADR 0012）；
- `viso-render` 的 CPU-heavy kernel（path preprocess、color conversion、image swizzle 等）若需要编译期 baseline 以外的指令集（如 AVX2），可以在进程/设备初始化时一次性选择 kernel table，热循环不每个 primitive 重新检测；此类 runtime 选择必须有 §31.5 benchmark 证据。

## 23.3 等价性与 Public ABI

- 存在 scalar reference implementation；
- `viso-math` SIMD kernel 与 scalar **bit-exact**（不使用 FMA 改变运算顺序），由测试钉住；
- render kernel 默认同样要求 bit-exact；确需容差的 kernel 必须逐 kernel 声明容差并在测试中断言；
- SIMD vector width / backend SIMD type 不进入 public ABI；public 仍使用稳定 `Vec2/Vec4` 等语义类型。

---

# 24. Culling / Damage / High Refresh

## 24.1 Culling

至少：surface bounds reject、clip bounds reject、zero-alpha fast reject、empty geometry reject（均使用 §8.5 保守 paint bounds 与 §5.3 严格相交）。

普通 UI 用 CPU bounds + clip intersection 即可。大型 scene 可进一步 chunk-level CPU cull、spatial bins（`CullPlan`；元素数低于阈值时 grid 构建成本高于逐个测试，不建 grid）、hierarchical culling，以及 A0 的 GPU cull / indirect draw。不要让几十个 UI node 为了“GPU driven”产生 compute dispatch。

## 24.2 Damage：UI dirty class 与 render revision

UI 侧 dirty class 只有 `STRUCTURE STYLE MEASURE LAYOUT TRANSFORM PAINT HIT_TEST SEMANTICS`；render 侧只维护 §8.2 的 revision plane。“Resource” 是 resource-key revision，不是 dirty class。映射：

| UI dirty class | Render 侧效果 |
|---|---|
| `STRUCTURE` | primitive 插入/删除/kind 变化：slot reslot（generation bump）、相关 RenderChunk 重建 |
| `STYLE` | 由 style resolve 落为下列某一类；render 不直接消费 |
| `MEASURE` / `LAYOUT` | 尺寸/形状变化 → `GeometryRevision` + bounds；仅位置变化 → `TransformRevision` |
| `TRANSFORM` | `TransformRevision`（UI 同时标 `HIT_TEST \| PAINT`，从不标 `MEASURE`/`LAYOUT`） |
| `PAINT` | `PaintRevision`；引用资源变化 → `ResourceRevision`；effect 参数 → `EffectRevision`；clip → `ClipRevision`；可见性 → `VisibilityRevision` |
| `HIT_TEST` / `SEMANTICS` | 无 render revision |

规则：只 bump 实际变化的 plane；一个颜色改变不得升级成 geometry rebuild；backdrop 依赖按 §17.7 另行追踪。

## 24.3 120/144/240Hz 稳态合同

关键不是“每帧更快重建”，而是“不重建”：无 render state 变化时必须满足 §36 的全部零计数；有局部变化时只处理受影响部分：

```text
Transform-only animation  -> update compact transform data -> reuse geometry -> reuse batch
Color/opacity-only        -> update instance range
Scroll                    -> update transform / clip / virtualized items
```

不得 retessellate、rebuild all batches、recompile graph、recreate render targets。

## 24.4 Idle

没有 animation、input、timer、async completion、surface damage、external invalidation 时：0 UI frame、0 renderer encode、0 GPU submit。静态 UI 不因为 blur/glass/shadow 存在就持续重绘。

---

# 25. RenderGraph 的建立时机与合同

RenderGraph 在 E1 才成为完整基础设施（之前的最小 pass 模型见 §16.1）。

负责：pass dependency、resource read/write usage、barrier/state lowering、transient lifetime、attachment compatibility、pass merge opportunity。不负责：Widget tree、Layout、State binding、Font fallback。

编译规则：

- `RenderGraphPlan` 只在 dependency topology 或 target requirement 变化时重新 compile；
- 参数变化（blur sigma、color、tint、transform）只 update parameter，不 destroy/rebuild graph、不 reallocate textures；
- 例：`Main UI -> Backdrop Capture -> Blur -> Glass Composite -> Present` 中 tint 从 A 变 B 只更新参数。

---

# 26. Effect Planner 的建立时机与合同

Effect Planner 在 C0（group opacity fold / clip ladder）、E0（analytic shadow）、E1（ROI / blur ladder / target reuse）逐步获得事实，在 E2 完整形成。规范合同见 §17.4–§17.7；cost 分类见 §30.2。

---

# 27. 基础功能清单

## 27.1 Viso 1.0 标准绘画能力

| 阶段 | 能力 |
|---|---|
| D0/D1 | Rect、RoundedRect / per-corner radius、RoundedSuperellipse、Circle、Ellipse、Capsule、Line / Polyline；stroke width / hairline / alignment / cap / join / miter limit |
| D2 | Solid、Linear / Radial / Sweep Gradient、ImagePattern；image source rect / fit / sampling / nine-slice / atlas / tiling |
| D3 | Arc / Pie、Polygon、Quadratic / Cubic Bezier、Path、SVG、Mesh2D（Mesh3D 见 §21.4）；dash pattern / offset |
| C0 | Rect / RRect / Path clip、ClipChain、Alpha / Luminance mask；primitive / group opacity、§14.6 C0 blend |
| E0 / E1 / E2 | outer / inner / path shadow；blur；backdrop blur、§17.3 color effects、advanced blend |
| M0 / M1 | Frosted、Glass；platform material、Liquid Glass、HDR |
| §27.2 | ShaderBrush、custom fragment / compute effect |

文字由 `Viso_Text_Font_Runtime.md` 定义，最终作为 `GlyphRun` primitive 进入本渲染系统。E2 之前的全部能力稳定后才进入 Frosted/Glass/Liquid Glass。

## 27.2 Custom Shader

Viso Shader Domain 在受控能力中允许 vertex、fragment、compute（不同于只提供 fragment shader 的设计）；普通 UI custom effect 默认从 fragment/local effect 开始。

Release：`.vs` shader source -> build-time typed compile（§7.1）-> MSL / HLSL / WGSL package -> PipelineManifest。禁止在交互动画热路径临时编译 shader source。

Custom shader 必须声明 §30.2 cost class；在标准 renderer ABI 冻结（F4 Done）前不公开。

## 27.3 Custom Canvas

`Canvas2D` / `Scene2DBuilder` 风格 API 是：

> **Immediate authoring facade over retained Render IR**

而不是每个 vsync replay user callback、allocate command list、parse commands。静态/状态驱动 Canvas 在依赖没变时复用其 `Scene2D`；动态图更新已有 `PrimitiveHandle`，避免每帧重建整个 Scene。

## 27.4 Rust Low-level API（概念示例，不冻结签名）

```rust
let shape = scene.shape(ShapeGeometry::RoundedRect { rect, radii });
shape.fill(Brush::Solid(color)).stroke(stroke).shadow(shadow);

let mut sprites = scene.sprite_batch(texture);
sprites.extend_pod(&instances);

let handle = scene.insert(...);
scene.update_transform(handle, transform); // must not rebuild path geometry
scene.update_opacity(handle, opacity);
```

## 27.5 DSL / Standard Schema

绘画能力属于 Native Schema，不增加语言关键字（ADR 0034）。属性绑定使用 `:` 与分号（CLAUDE.md §21.5.2）。概念：

```viso
Shape {
    geometry: ShapeGeometry::RoundedRect { radius: 16dp };
    fill: Brush::LinearGradient {
        from: vec2(0.0, 0.0),
        to: vec2(1.0, 1.0),
        stops: [GradientStop { offset: 0.0, color: #ff8a00 }, GradientStop { offset: 1.0, color: #e52e71 }],
    };
    stroke: Option::Some(StrokeStyle { width: 1dp, alignment: StrokeAlignment::Inside });
}

EffectSurface {
    effects: [
        Effect::Shadow(Shadow { offset: vec2(0dp, 8dp), blur: 24dp, spread: 0dp, color: #00000040 }),
    ];
    CardContent {}
}
```

标准 Schema 必须能够被 compiler 查询 effect cost class（§30.2）。

## 27.6 Default Widget 绘画策略

Button / Card / Panel 等普通控件的 background、border、radius、simple shadow、state color 必须优先 lower 成一个或极少数 analytic instances；禁止默认 lower 成 `Path + ClipPath + Offscreen + Blur + Composite`。

Scroll：axis-aligned viewport -> scissor。Image：one image instance。Text：retained glyph run。

---

# 28. 容易遗漏但必须提前固定的边界功能

## 28.1 清单（均已在对应阶段定义）

后期最容易迫使 Renderer 重写的数据合同，均已固定：snapping/hairline（§5.7/§5.8）、radius normalize（§11.2）、非法/退化输入（§5.9/§5.10）、alpha 与编码（§5.4/§5.5）、texture origin 与 atlas bleeding（§5.2/§12.5）、fill rule/miter/dash/arc（§13）、clip nesting 与 fast reject（§14.1–§14.3、§24.1）、bounds inflation（§8.5、§11.3、§15.2、§16.2）、scale/resize/sample-count/color-space 变化（§5.7、§6.4、§19.2）、device loss 与 lifetime（§6.4–§6.6）。

## 28.2 Memory pressure

释放顺序（建议）：

```text
cold snapshot/effect output
cold path tessellation variants
cold clip/mask pages
cold image mip/atlas pages
cold MTSDF/vector glyph representation (per-pool eviction, ADR 0025)
other reconstructible caches
```

仍被当前 frame/fence 引用的 GPU resource 必须 pin 到安全完成点（§6.6）；禁止收到 memory pressure 后直接销毁当前帧仍引用的 texture/buffer。

## 28.3 Resource budget 按 bytes

禁止以 `max_paths = 1000`、`max_images = 100` 作为主要内存策略。使用：`path_geometry_budget_bytes`、`mask_atlas_budget_bytes`、`effect_cache_budget_bytes`、`image_budget_bytes`、`transient_target_budget_bytes`。大资源有 oversize admission policy。

---

# 29. 模块布局

保持 crate 边界少而硬。新增模块按 §4.4 职责归位，不因此新增 crate。`viso-render` 当前布局（`crates/render/src`）：

```text
scene/{ids, revision, store, bounds, ingest}   F3 retained scene
primitive.rs  path.rs  vector_lane.rs          D0–D3 primitive / path lanes
atlas_plane  rect_packer  gradient_lut          D2 atlas / LUT
glyph_atlas  color_atlas  mtsdf_atlas           text representation residency (render side)
clip  mask  mask_page  raster_mask              C0 clip / mask
opacity  blend                                  C0 group opacity / blend
color_effect  effect_cost  effect_plan          E2 effects / planner
graph  transient                                E1 RenderGraph / transient targets
pool/{instance_pool, coalescer}                 F4 persistent pool / dirty ranges
batch/{chunk, planner}  binding_model           F4 batching / A0 binding model
frame/arena  cull  inspect                      F4 arena / culling / counters & inspector
renderer.rs                                     frame orchestration
```

---

# 30. 性能计数器必须从 D0 就存在

## 30.1 Counters

不能等 Renderer “做完”以后才 profile。至少记录（`FrameStats`，由 `counter_contract_frozen.rs` 冻结）：

```text
visible_primitives  culled_primitives  render_chunks  batches  draw_calls
pipeline_switches  texture_binding_switches  uploaded_bytes  uploaded_ranges
instance_rebuilds  path_tessellations  clip_mask_builds  offscreen_passes
transient_target_bytes  blur_pixels  backdrop_capture_pixels  shader_pipeline_creations
cpu_render_build_time  cpu_encode_time  gpu_frame_time
```

## 30.2 Effect cost metadata

Schema/IR 为每个 effect 标记 `EffectCost`：`Local < Analytic < NeedsMask < NeedsOffscreen < NeedsBackdrop < DestinationRead < ComputePreferred`，以及 `EffectLocality`（§17.4）。

Compiler/LSP/Inspector 可以提示 “this effect creates offscreen ROI”“this backdrop forces source dependency”“this clip uses mask path”，避免生成视觉正确但成本失控的组合。复杂 effect 不需要禁止，但必须可观测。

## 30.3 Inspector

必须显示真实渲染成本：§30.1 counters 按 lane / pass 分解，另加 clip class distribution、transient target peak bytes、persistent GPU bytes、dirty instance ranges、compute-vector workload、overdraw estimate、GPU time per pass，以及每个 layer 的 `LayerReason`。

## 30.4 Debug overlay（仅 dev tooling path）

paint bounds、dirty regions、clip masks、offscreen ROI、backdrop dependency、batch breaks、path lane、overdraw、GPU resource residency。

---

# 31. Benchmark Gate

## 31.1 测量合同

- **计数类**（allocation 次数、pipeline/texture 创建、upload bytes、draw calls、pass count、tessellation 次数）是确定性断言，在 `cargo test` / bench smoke 中精确比较，不依赖硬件；
- **时间类**（CPU build/encode、GPU time、frame-time p50/p95/p99）对 reference hardware profile 的 baseline 比较，不用单一绝对毫秒绑死所有设备；
- 回归阈值沿用 Architecture §59.1：>3% 标记趋势，>5% CI warning / 需说明，>10% 默认阻止合并（除非明确批准）；具体阈值按 benchmark 稳定性调整；
- 每个 benchmark 覆盖 100% static、mixed ~10% dynamic、100% dynamic，量化性能悬崖；
- 本节所有 workload 为 [target] 验收场景；未落地到 `crates/render/benches/renderer_steady_state.rs`（或对应 crate bench）前，不得以其结论作为默认 heuristic 依据。

## 31.2 阶段 workload

| 阶段 | Workload |
|---|---|
| F0/F1/F2 | Affine transform batch、Rect intersection、buffer upload bandwidth、mapped ring allocation、pipeline lookup、command encode baseline |
| F3/F4 | 100k retained primitive traversal、single dirty primitive、1% dirty、full dirty scene、batch construction、upload range coalescing |
| D0/D1 | 10k / 100k SolidRect、10k RRect、10k borders、mixed Rect/RRect/Circle、scroll transform-only、hover paint-only、animated radius |
| D2 | 1k multi-stop gradients、image grid、10k image/sprite instances、texture-binding pressure、glyph-heavy UI |
| D3 | 1k small SVG icons、large complex SVG、dynamic chart path、path transform-only、stroke/dash heavy、500 morphing paths |
| C0 | deep rect clip、nested rounded clips、complex cached clip、group opacity overlap/no-overlap、blend stress |
| E0/E1/E2 | 1k–5k analytic shadows、path shadow reuse、small/medium/large blur ROI、many small ROI blur、shared backdrop、color-effect fusion、advanced blend |
| M0/M1 | 30 overlapping frosted/glass surfaces、HDR effect chain |
| System | large vector canvas pan/zoom、4K 60Hz、1440p 144Hz、1080p 240Hz、mobile 120Hz、memory pressure、device loss/recreate |

每组记录 §30.1 counters、allocations、persistent GPU / CPU retained-scene bytes、resource create/destroy、bandwidth proxy、frame-time p50/p95/p99；Path / Clip / Blur / Backdrop / Material 阶段另记各 cache 峰值/稳态占用，防止以内存换速度掩盖回归。

## 31.3 外部对照

- **Makepad**：10k rounded cards、animated radius/hover、SDF icons、sprite/text；确认 retained/typed path 没有引入不必要成本。
- **Flutter/Impeller**：RRect、Path、atlas、Clip、Shadow、Blur、Backdrop、Group opacity；记录 saveLayer-equivalent offscreen count 与 target bytes。[target] 等价视觉下 offscreen 更少。

## 31.4 Vector Compute Lane 验证

Compute Lane 只有在测得 crossover 点后才启用默认 heuristic。测试 10 / 100 / 1k / 10k paths × static / transform-only / morphing / clip-heavy，对比 cached CPU tessellation 与 GPU compute vector。阈值按 backend/device class profile，不写死进 public API。

## 31.5 Unsafe / SIMD gate

任何新增 unsafe/SIMD 优化必须给出 before、after、workload、device/CPU、correctness comparison（§23.3）。收益不显著时保留更简单安全实现。允许 unsafe 不代表追求 unsafe 数量。

---

# 32. Correctness Gate

性能优化不能绕过这些测试：

| 测试 | 现有载体 |
|---|---|
| pixel golden（byte-identical，`BLESS=1` 重新 bake） | `render/tests/golden.rs`、`metal_golden.rs`、`metal_naga_golden.rs` |
| cross-backend image diff | `d3d12_backend.rs`、`vulkan_backend.rs`、`webgpu_backend.rs`、headless |
| premultiplied alpha / sRGB-linear | `color_domain.rs`、math `color.rs` tests |
| blend | `blend_contract.rs` |
| gradient edge、stroke join/cap、path fill-rule、clip nesting | golden scenes（按阶段补齐） |
| degenerate geometry / NaN | math `legality.rs` tests + golden |
| DPI scaling、surface recreation | gpu surface tests（`metal_uikit_surface.rs` 等） |
| resource lifetime stress、generation | `gpu/tests/generation.rs`、`render/tests/fence_recycle.rs` |
| device-loss simulation | headless backend（full device loss 路径待建，§6.4） |
| ABI | `gpu_instance_compile_fail.rs`、`instance_abi_frozen.rs` |

Path/geometry 还应有 fuzz/property tests。Unsafe/SIMD kernel 必须与 scalar oracle 做等价测试（§23.3）。

---

# 33. 阶段完成门槛

每个阶段的 Exit 同时要求 §0 stage gate 全绿。M0/M1/A0 不得被列为 D0~E2 的完成前置。

| 阶段 | Scope | Exit criteria | Verification |
|---|---|---|---|
| F0 | §5 | §5 每条规则有单元测试；SIMD bit-exact | `cargo test -p viso-math`、`color_domain.rs`、`math/benches/math.rs` |
| F1 | §6 | 各 Tier-1 backend clear/present；resize/DPI/surface recreate 正确；stale handle resolve 为 `None`；deferred free 不早于 fence | `gpu/tests/generation.rs`、`headless_quad.rs`、`headless_indexed_mesh.rs`、`fence_recycle.rs`、backend tests |
| F2 | §7 | release 无标准 shader runtime 编译；derive compile-fail 覆盖 §7.3；`validate_against` 在注册时运行 | `gpu_instance_compile_fail.rs`、`gpu_instance_derive.rs`、`instance_abi_frozen.rs`、`metal_no_runtime_compile.rs`、`dev_shader_pipeline.rs`、`gpu_specialization.rs` |
| F3 | §8 | 未变化 primitive 零 mutation；transform/brush/clip identity 分离 | `scene_contract_frozen.rs`、`scene_diff.rs` |
| F4 | §9 | local dirty 不全量上传；稳态零 per-primitive alloc；order-preserving batching | `instance_pool.rs`、`coalescer.rs`、`frame_arena.rs`、`batch_planner.rs`、`data_path_contract_frozen.rs`、`counter_contract_frozen.rs`、`renderer_steady_state` |
| D0 | §10 | 100k rect bench 可重复；§10.2 断言成立 | `golden.rs`、`renderer_steady_state` |
| D1 | §11 | analytic AA 满足 §5.6；transform-only 零 tessellation | golden、`render_contract_1_0.rs`、bench |
| D2 | §12 | 静态 gradient 零 texture 创建；无 atlas bleeding | `texture_binding.rs`、golden、bench |
| D3 | §13 | transform/color 不重建 geometry；quality bucket hysteresis | `vector_lane.rs`、golden |
| C0 | §14 | rect clip 走 scissor；group opacity 行为符合 ADR 0002（fold-down 需 ADR 批准） | `blend_contract.rs`、golden |
| E0 | §15 | simple shadow 不建 blur layer；path shadow mask 复用 | `shadow_contract_frozen.rs`、`shadow_scene_byte_identity.rs` |
| E1 | §16、§25 | 稳态零 target 分配/零 replan；ROI 不超出 §16.2 | `offscreen_contract_frozen.rs` |
| E2 | §17 | 每个 layer 有 `LayerReason`；backdrop 按 ROI 独立 dirty；affine color chain 零额外 pass | `effect_contract_frozen.rs`、`effect_planner_contract.rs`、`color_effect_contract.rs`、`backdrop_contract.rs` |
| M0 | §18 | 共享 capture/blur；无新 blur/graph 实现；静态时零 submit | `material_contract.rs`、`material_lane.rs` |
| M1 | §19 | 平台 API 不进入 Render IR；HDR intermediate 不降级 | 待建：native material island 测试、HDR golden |
| A0 | §20 | 每项有 §31.4/§31.5 crossover 证据；fallback 行为不变 | 待建：crossover bench；fallback 由 `texture_binding.rs`、`culling.rs` 覆盖 |

## 33.1 Viso 1.0 Definition of Done

Rendering Runtime 同时满足下列合同才算达到 1.0（每项指向定义处）：

```text
primitive path: Rect/RRect/Circle/Ellipse/Line/Arc/Path/Image/Text/Mesh      §27.1
brush 语义：Solid/Linear/Radial/Sweep/ImagePattern/ShaderBrush             §12.1
stroke cap/join/miter/dash/alignment                                       §11.3 §11.4 §13.5
simple UI shape 默认 analytic/instanced；stable path retained geometry       §11.1 §13.4
compute vector 仅作为大型动态 workload lane                                 §20.1
default UI 不全局启用 MSAA                                                  §5.6
rect clip scissor；border radius 不自动 clip children；complex clip mask cache  §14.1 §14.2
simple shadow 不默认建 blur layer；blur/backdrop tight ROI；共享 capture/blur    §15.1 §16.2 §17.2
local color effect fuse；每个 offscreen layer 有 LayerReason                  §17.5 §17.6
group opacity 只在语义需要时 isolation                                      §14.5
GPU canonical alpha premultiplied；HDR/wide-gamut intermediate 不降级        §5.4 §19.2
standard shader build-time 编译；frame hot path 不同步编译                   §7.1 §7.5
GpuPod ABI 无 pointer/usize/uninitialized padding；无多层 Vec 转换          §7.3 §7.4
persistent instance pool + dirty range；transient textures pool/alias       §9.1 §9.3 §16.4
local change 不重建全 scene；transform-only 不 retessellate                  §24.2 §24.3
stable frame 不重建 clip/shadow/gradient cache；idle 不 submit               §24.3 §24.4
120/144/240Hz benchmark 有 regression gate                                 §31.1
memory pressure 不释放 in-flight resource；device loss 可恢复                 §28.2 §6.4
Inspector 可见 pass/offscreen/upload/batch/blur/backdrop 成本               §30.3
unsafe/SIMD 有 correctness reference 与 benchmark                          §22 §23 §31.5
```

---

# 34. 实现工作顺序

规范执行顺序即 §0 的 stage 链与 §3 的依赖链，不另立路线；同阶段内并行规则见 §0 Hard rules。

---

# 35. 现有代码收敛原则

已有实现重构时按职责归位，而不是继续在现有模块上叠抽象。对每个现有模块回答：

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

Retained UI 已就绪时，普通稳态帧：

```text
0 primitive reconstruction / Paint IR rebuild when unchanged or for unrelated local dirty
0 path parse; 0 tessellation unless geometry/quality bucket changed
0 gradient LUT / clip mask / shadow mask rebuild unless its inputs changed
0 standard shader or pipeline compilation; 0 pipeline creation
0 GPU buffer/texture creation per primitive
0 per-primitive heap allocation / string lookup / global HashMap lookup
0 Rc/RefCell borrow or mutex/RwLock in primitive traversal
0 per-primitive backend virtual dispatch
0 full-scene upload for local change
0 unnecessary offscreen pass
static scene: 0 frame (§24.4)
```

可计数项（heap allocation、GPU resource/pipeline 创建、draw call、upload、tessellation、offscreen pass）由 `renderer_steady_state` 精确断言（§31.1）；lock / virtual dispatch / HashMap 等结构性条款由 review 与 `*_contract_frozen.rs` 结构测试保证。

**典型热路径**：

```text
dirty revisions -> changed PaintChunks -> patch typed primitive fields
  -> coalesce dirty GPU ranges -> patch affected batch spans only if key changed
  -> reuse RenderGraphPlan -> encode -> submit -> present
```

**典型场景目标** [target]：

| 场景 | 目标路径 | 禁止 |
|---|---|---|
| Button hover | paint parameter dirty -> one instance range write；same pipeline/geometry/clip/batch | widget tree / path / shader / pipeline rebuild |
| 圆角 Card（RRect fill + border + one shadow） | 1 analytic primitive，或 1 shadow instance + 1 shape instance；是否融合由 GPU benchmark 决定，IR 语义保持同一 Decoration | 多级 offscreen |
| 大型 Blur Panel | visible ROI -> downsample/share -> blur -> composite；backdrop 未变时复用结果；panel 移动而 backdrop 不变时按 capture model 复用或局部重采样 | 整屏重算 |

API 可以提供 `Effect::Blur`、`Effect::BackdropBlur`、`BlendMode::SoftLight` 等便捷能力，但 Schema/Inspector 必须给出 cost class（§30.2）。

---

# 37. 最终架构摘要

```text
                       UI / Text / Canvas
                              ▼
                       Retained Paint IR
                 ┌────────────┴────────────┐
           Primitive Stores           Path Geometry
                 └────────────┬────────────┘
                     Bounds / Clip / Damage
                              ▼
                 Persistent Instance / Geometry
                              ▼
                    Order-safe Batch Planner
                              ▼
                    Effect Planner when needed
                              ▼
                 RenderGraph only when needed
                    ┌─────────┴─────────┐
                viso-shader          viso-gpu
                    └─────────┬─────────┘
             Metal / D3D12 / Vulkan / WebGPU
```

> **把最常见的路径做到最短，把昂贵技术只用于真正能摊薄成本的 workload；基础数据 retained、可局部失效、可局部上传，Shader 与 GPU backend 都服务于这一目标。**

## 37.1 外部经验

- **Makepad**：shader-based UI primitive（Sdf2d）+ instancing，圆角框/icon/状态动画无需 CPU 几何重建。Viso 采用，但以 typed instance、retained instance range、partial dirty upload、multi-lane path、显式 offscreen cost model 取代无类型 `Vec<f32>` 数据流。
- **Flutter / Impeller**：完整 2D Canvas primitive 语义与 offline shader compilation、explicit pipeline cache。其文档指出 `saveLayer()`、过度 clipping、BackdropFilter 的 offscreen 成本；Viso 以 Effect Planner、ROI、fusion、transient aliasing、shared backdrop 取代 saveLayer 式默认实现。
- **Vello**：GPU compute 适合大型/动态 vector scene，但 dispatch、temporary buffer、prefix scan 有固定成本；Viso 只在 §31.4 测得 crossover 后启用 Vector Compute Lane。

## 37.2 外部参考

参考经过验证的算法、性能特征、工具链经验和用户体验，不复制耦合边界，不建立兼容关系。

1. Makepad draw：<https://github.com/makepad/makepad/tree/dev/draw/src>、<https://github.com/makepad/makepad/blob/dev/draw/src/draw_list_2d.rs>、<https://github.com/makepad/makepad/tree/dev/draw/src/shader>
2. Flutter Canvas：<https://api.flutter.dev/flutter/dart-ui/Canvas-class.html>
3. Flutter Impeller：<https://docs.flutter.dev/perf/impeller>
4. Flutter `saveLayer` guidance：<https://docs.flutter.dev/perf/best-practices>、<https://api.flutter.dev/flutter/dart-ui/Canvas/saveLayer.html>
5. Flutter fragment shader：<https://docs.flutter.dev/ui/design/graphics/fragment-shaders>
6. Vello：<https://github.com/linebender/vello>、<https://github.com/linebender/vello/blob/main/ARCHITECTURE.md>
7. WGPU StagingBelt：<https://wgpu.rs/doc/wgpu/util/belt/struct.StagingBelt.html>

---

# Canonical Document Rule

`Viso_Rendering.md` is the single authoritative rendering document for architecture, implementation order, low-level rendering contracts, drawing primitives, composition, effects, materials integration, optimization gates, and vibe-coding execution.

Other Viso specifications may reference this file, but must not define an independent rendering implementation sequence.
