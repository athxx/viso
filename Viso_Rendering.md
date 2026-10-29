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

拥有 allocation-free 基础数学与 F0 值类型（ADR 0012；叶子，`no_std`）。`f32` 是唯一精度：没有 f64 public 类型，热数据不携带 `f64`，同一布局可在单精度 FPU 上运行；`f64` 只在内部用于数值稳定（如 `Affine2` 求逆的行列式）。

```text
Vec2 / Vec3 / Vec4
Mat2 / Mat3 / Mat4, Quat
Affine2, Transform3
Point / Size / Rect / Insets                 2D 几何类型的唯一 owner（check-deps 拒绝重复定义）
Aabb / Ray / Plane
LinearPremul / LinearStraight / Srgb / LinearSrgb / DisplayP3 / ExtendedLinear,
InterpolationSpace / ExtendMode              颜色值与色彩空间换算（§5.4）
Coverage / composite                         coverage 与 premultiplied source-over（§5.5–§5.6）
PixelSnap / Hairline / snap_*                像素对齐（§5.7–§5.8）
GeometryLegality / classify_*                非法数值分类（§5.9）
Easing / CubicBezier / Curve / Spring        动画曲线
```

所有 public 类型 `Copy`，不含 `usize`；几何类型与线性颜色类型 `#[repr(C)]`。Renderer 2D 热路径优先使用 `Affine2 / Vec2 / Rect`；普通 2D transform 禁止仅因为 API 统一而全部使用 `Mat4`。

`viso-math` 不拥有 Brush、Path、Clip、Primitive、GPU ABI、Shader layout（Math ABI ≠ GPU ABI），也不拥有输出编码与色彩管理策略（surface 编码归 render/gpu，ADR 0048）。颜色值类型、色彩空间换算与 composite 归 `viso-math`（Architecture §9 `viso-math`）：render、svg、image、ui 共享这些值类型，不需要另起 crate 或新增依赖边。

## 4.2 `viso-gpu`

只拥有极薄 RHI。当前形态：每个 target 一个静态 `GpuBackend`（`crates/gpu/src/backend.rs`），以 generation-safe 句柄（`SlotMap`）管理 Buffer、Texture、Sampler、Pipeline、BindGroup、Surface，帧路径是 `write_buffer` + `begin_frame` → `encode(DrawList)` → `present` / `present_with_damage`；能力由 `Caps` 报告；Fence/Epoch 与 `RetireQueue` 在各 backend 内部。没有 command encoder、compute pipeline/pass 与 barrier API（pass 顺序即同步）；格式与 ABI 类型在叶子 crate `viso-gpu-abi`。

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

Renderer 的输入全部是 logical point：primitive 的几何与长度（半径、线宽、offset、blur sigma）、clip、damage rect 与 `submit`/`render_to_texture` 的 viewport。device scale 由 `Renderer::set_surface_size(physical_size, scale)` 给出，只在以下位置换成 physical：

- scissor = clip × scale（`clip_to_scissor`），按 physical surface 夹取；
- offscreen / backdrop capture target 的 ROI 先对齐到 physical 像素网格，texel 数 = ROI × scale，pass viewport 为 target 的 physical extent ÷ scale，内容仍按 logical 几何绘制；composite 的 uv 用 used extent ÷ scale；
- blur 链的 sigma × scale（blur / color rung 在 texel 空间工作）；
- coverage mask 以 scale 栅格化，scale 进入 mask key（`device_scale_q`）；
- `transient_target_bytes` 等计数器报告 physical 字节。

scale 或 logical 尺寸变化时下一次 upload 走整帧路径（patch / splice 拒绝）。CPU raster 对每个 draw 以 target 宽度 ÷ viewport uniform 推出 scale，GPU shader 的 AA 由 `dfdx/dfdy` 自适应。scale 2 的场景与"所有长度 ×2、scale 1"的场景逐 texel 一致（`render/tests/device_scale.rs`，headless 与 Metal）。

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
- **落地（ADR 0048）**：`ColorSpace::Srgb` 的 surface 报告 `*Srgb` format，内建 pipeline 与 intermediate 随之，由硬件编码一次（Metal `BGRA8Unorm_sRGB`、Vulkan SRGB swapchain、D3D12 `_SRGB` RTV、WebGPU `-srgb` view、GLES sRGB colorspace window、`CpuRaster` 软件查表）；颜色纹理（image、color glyph atlas、gradient LUT、3D scene target）为 `*Srgb`，采样即解码；数据纹理（A8 coverage、MTSDF、mask、float）保持线性；sRGB texel 在线性光里 premultiply：`enc(dec(c)·a)`，缩放亦在线性光里做；glyph coverage 按墨色线性亮度 `l` 校正 `cov' = (1−l)(1−dec(1−cov)) + l·dec(cov)`，使黑字白底 / 白字黑底与编码空间混合的字重一致；
- **无 sRGB format 的 surface**：GLES window 拿不到 `EGL_KHR_gl_colorspace` 或驱动拒绝 sRGB surface、device 画的 `Rgb565Unorm` panel（`Caps::encodes_packed` 为 false；`CpuRaster` 窄化时自己编码，不走此路）等报告 unorm format 的 `Srgb`/`DisplayP3` surface，由 `Renderer::for_surface` 改在其 sRGB 版（565 用 `Bgra8UnormSrgb`）的 working target 里画：pipeline、program 与 intermediate 都以它为 attachment（`Renderer::target_format`），最后一个 OutputEncode pass 采样它、做 sRGB transfer 写入 surface，565 再加一阶 integer-lattice dither（与 CPU 版 bit-exact）。working target 跨帧保留，partial redraw 以它的 age（同尺寸且上一帧已 present 为 1，否则 0）求重画区域，encode pass 每帧整面写 drawable，与 drawable 自身 age 无关；`render_to_texture` 走同一链路，之后 working target 记为未 present。`Renderer::new(format)` 照调用方给的 attachment format 原样存储，不加此 pass。

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

命名可以在 Rust API 收敛，但职责不可混合。当前收敛结果见 §4.2：Device/Queue 合进 backend 值，CommandEncoder/RenderPass 由声明式 `DrawList`（pass + draw command 区间）取代，Fence/Epoch/RetireQueue 是 backend 内部；Compute 对象尚未建立（A0）。

## 6.2 Buffer usage

至少区分 Vertex、Index、Uniform、Storage、Upload、Readback、Indirect。一个资源可以具有组合 usage，但 backend 必须验证合法性。

## 6.3 Texture formats baseline

```text
R8Unorm  RG8Unorm  RGBA8Unorm  RGBA8UnormSrgb  BGRA8Unorm  BGRA8UnormSrgb
RGBA16Float  Depth / DepthStencil (backend-required format)
```

用途：sampled、render attachment、storage where supported、copy src/dst。Format 的 color-space 属性（Srgb vs Unorm）必须随 texture metadata 传递，供 §5.4 的“只编码一次”规则检查。

Sampled-only 的转换图像格式（构建期转换的 asset，Architecture §51.1）：

```text
16-bit packed  Rgb565Unorm  Rgba4444Unorm (R 在高 nibble)  Rgba5551Unorm (A 在 bit 0)
BC             Bc1RgbaUnorm  Bc3RgbaUnorm  Bc7RgbaUnorm
ETC2 / EAC     Etc2Rgb8Unorm  Etc2Rgb8A1Unorm  Etc2Rgba8Unorm
ASTC LDR       Astc4x4Unorm  Astc6x6Unorm  Astc8x8Unorm
```

- texel 与其它 texture 同一约定：premultiplied（线性光）、按存储的 sRGB 编码；8-bit 与块格式各有 `*Srgb` 变体（`TextureFormat::srgb` / `linear` / `is_srgb`），图像以该变体建 texture，采样解码到线性光；16-bit packed 无硬件 sRGB 变体，GPU 后端不采样，由 asset worker 解码为 RGBA8，`CpuRaster` 软件解码；
- 块格式（`TextureFormat::block`）只作 sampled texture，尺寸必须是整块（转换器保证；D3D12 / WebGPU 要求）；`write_texture` 收按块行排列的数据、块对齐的区域，`row_bytes` / `block_rows` / `data_size` 给出布局；
- 设备采样哪些族由 `Caps::sampled: SampledFormats` 报告，`Caps::samples(format)` 查询：Metal 按 GPU family（Apple2+ ETC2/ASTC，BC 取 `supportsBCTextureCompression`）；D3D12 `CheckFeatureSupport`（BC）；Vulkan 设备特性 `textureCompressionBC/ETC2/ASTC_LDR`（创建设备时开启）；GLES3 ETC2 为 core，ASTC 经 `KHR_texture_compression_astc_ldr`，BC 需 S3TC（含 sRGB）+ BPTC；WebGPU 按 adapter feature 请求 `texture-compression-*`（packed 格式不可用）；`CpuRaster` 只采样 16-bit packed；
- 不采样的格式由 asset worker 用 `viso_image::codec` 软件解码为 RGBA8 后上传；`create_texture` 收到不采样的格式是调用方错误；
- `create_texture_static(desc, &'static [u8])`：texel 在程序生命期内不变（嵌入 binary / 映射 flash），默认实现为 create + write；`CpuRaster` 对可原地采样的格式直接借用，不复制。

## 6.4 Surface 与 device loss

Surface 必须支持：acquire、present、resize、DPI change、format/color-space change、occlusion/minimize、out-of-date recovery。

Device loss 分两级，行为固定：

| 级别 | 触发 | 行为 |
|---|---|---|
| Abandoned frame | `begin_frame` 后没有 `present` | `GpuBackend::abandon_frame(surface)`：丢弃持有的 drawable；把当前 epoch 视为完成以解除 RetireQueue 阻塞；下一次 `begin_frame` 重新 acquire。Persistent buffer/texture/pipeline 保留。 |
| Full device loss | GPU reset、driver 更新或重启、外接 GPU 拔出：D3D12 `DXGI_ERROR_DEVICE_REMOVED`/`RESET`、Vulkan `VK_ERROR_DEVICE_LOST`、WebGPU `device.lost`、EGL `EGL_CONTEXT_LOST`、Metal command buffer 的 `DeviceRemoved` | backend 记下丢失，`GpuBackend::is_lost()` 为真：此后 `begin_frame` 为 `None`、encode 丢弃，backend 可安全 drop。facade 不再在它上面建任何东西，在下一帧开头放下窗口的整份 GPU 状态（backend、Renderer、glyph atlas），不释放旧 handle；`viso_gpu::try_create_device()` 开新 device（打不开时按 250 ms 起、翻倍到 4 s 重试，期间窗口不画；浏览器里异步向 WebGPU 要新 device）；在新 device 上重建 surface 与 Renderer，text 重新 shape，asset table 的 texture/model 全部忘掉后从保留的编码字节（无 origin 的资源）或 origin 重新解码上传，等待期间节点按原尺寸画空；3D scene slot 标为未填、由 filler 重画；brush program 重新安装；整棵树 PAINT。retained tree 与 state 不丢失。 |

Full device loss 的重建路径有 headless 测试：`CpuRaster::lose_device` 模拟丢失，窗口在新 device 上画出与未丢失时逐字节相同的帧（`crates/viso/tests/device_loss.rs`）。D3D12 另有 `D3D12Backend::remove_device`，在真机上触发真实 removal。

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

Backend 由 cfg 在编译期静态选定（ADR 0029）：`viso_gpu::Backend` 是 type alias —— apple → Metal，Windows → D3D12，Linux/Android → Vulkan（`native-vulkan`，facade 默认开启），wasm32 → WebGpu，其余 → CpuRaster。Android 的例外（ADR 0062）：`[target.android] gpu` 默认 `auto`，`Backend` 是 `AndroidBackend`（`VulkanBackend` / `Gles3Backend` 的枚举），进程内首次建 backend 或首次问 `viso_gpu::shader_lang()` 时选定：Vulkan 设备像 Vulkan backend 那样打得开就用它，否则用 GLES 并打印一次原因，启动变量 `VISO_GPU=vulkan|gles`（Android 是 intent extra，facade 经 `viso_gpu::ask_api` 交入）改选；每个 RHI 调用一个 `match`（不到 primitive），program 两种 AOT 语言都带，`__per_backend!` 按选定的 API 取；`gpu = "vulkan"` / `"gles"`（构建环境 `VISO_GPU`，`cfg(viso_gpu)`）只编译其一。Footprint profile 的 feature 改选（ADR 0044）：`gles` → `Gles3Backend`（Linux / Android，embedded-gpu），`cpu` 或 `headless` → `CpuRaster`（embedded-cpu、测试与 golden）；embedded build 关闭 facade 默认 feature，`embedded-cpu` 与 GLES 板卡不开 `native-vulkan`，不链接 Vulkan bindings（`ash`），Linux/Android 上 `gles` 与 `native-vulkan` 都没开时 Backend 是 CpuRaster。`vulkan` feature 在其他平台额外编译 Vulkan 模块（macOS 的 MoltenVK、Windows 的 Vulkan runtime），只用于交叉测试。D3D12 在没有 feature level 11.0 的硬件 adapter 时退回 WARP 软件 adapter（`d3d12.rs`）。Tier-2 backend 以 module + cfg 加入，能力差异通过 `Caps` 表达。

### 6.8.1 CpuRaster

`viso_gpu::cpu` 是唯一的 CPU backend，同时承担 headless / golden 角色：
- 64×64 tile binning，按提交顺序；只访问被触及的 tile，所以 damage 多大代价就多大；band 在常驻 worker pool 上并行，输出与 worker 数无关（worker 数取 footprint 的 `workers`，`0` 时只用调用线程）；
- 格式：XRGB8888 / ARGB8888（`Bgra8Unorm`，整数 src-over）、RGB565（渐变 4×4 Bayer dither）、`f32` HDR、A8 atlas；sampled image 另有 RGB565 / RGBA4444 / RGBA5551（构建期转换，§6.3），从 `create_texture_static` 借用的 texel 原地采样（嵌入 binary / XIP flash，不复制、不计 texture 内存）；不接收块压缩格式（`Caps::sampled` 不含，fallback 为 RGBA8）；
- 消费与 GPU 完全相同的 draw list，没有 shader 编译器：每个 built-in pipeline 由 `BuiltinShader` 标签选择其 shader IR 的 CPU 复述（`fill`），pipeline 的 blend state 照常生效；
- analytic AA lane（RRect / Ellipse / Capsule / Line / Shadow 在像素中心求 SDF 覆盖）；后端声明 `Caps::coverage_paths`，所有实心填充 path 走 scanline coverage 的 mask lane，描边仍走三角形 lane；A8 glyph / mask cache 在系统内存；
- NEON / SSE2 / SIMD128 kernel 与标量逐位一致；
- 只被区域合成（Image / ColorTransform，clamp-to-edge）读取的 offscreen pass（opacity < 1 的 group、融合的色彩滤镜）推迟到 tile 内求值：合成它的每个 tile 把要读的 texel 区域求值到 worker 本地窗口，不分配 target，结果与整张 target 逐位一致；被 blur、advanced blend、backdrop 或用户 program 读取的 layer 照常整块绘制（`set_layer_deferral` 可关闭推迟，供对照）；
- 无 alpha 的面板（RGB565）上，layer 要保存 coverage，中间目标为 `Bgra8Unorm`（按面板的编码选 sRGB 变体，`Renderer` 的 `intermediate_format`）；
- `Blitter` capability seam（`viso_gpu::cpu::Blitter`，`CpuRaster::set_blitter`）：板卡的 2D 加速器（i.MX PXP、Allwinner G2D、STM32 DMA2D 类）只接 solid fill（pass clear），拒绝的工作留在 CPU，像素不变；
- 每个 brush 都打包了 `ShaderLang::None` 程序（寄存器码解释器 `CpuShader`），custom shader 没有缺失 CPU 路径的可能；以 CPU 绘制的构建（`cpu`/`headless`）中，`view!` 在构建期把每个 brush 的 Program 生成 Rust（`NativeKernel`），`CpuShader` 只用它替换 Vertex/Fragment 入口的执行体，Setup、Varying 与颜色提取不变；生成代码与解释器调用同一组运算函数（`interp::rt`），常量寄存器下标，所以结果逐位相同（Golden 逐字节比对），按 Program 编码的哈希与寄存器数核对，不匹配（如热重载后的新 Program）即回退解释器；`cpu_raster` bench 上 800×480 用户 Shader：圆角矩形 12.4→7.3 ms，带函数与循环的 Ripple 78.5→15.5 ms；
- 3D scene lane 的 CPU 光栅（`viso_gpu::cpu::scene`）：实例三角形经 place、投影、near/far 裁剪、按 winding 剔除，在像素中心光栅（top-left fill rule），深度按屏幕空间线性插值，varying 透视校正，片元按 `viso_gpu::scene` 中与 GPU program 相同的表达式着色（§21.5）；
- 测量：`pixel_bytes()`（目标与 surface 像素）、`scratch_bytes()`（worker 窗口）、`deferred_layers()`；bench `cpu_raster_800x480_{full,damage_5pct}_{xrgb8888,xrgb8888_srgb,xrgb8888_4t,rgb565}`（`crates/render/benches/cpu_raster.rs`）；
- 呈现到 `RawWindowHandle::Scanout`：KMS dumb buffer 双缓冲，或桌面窗口的 layer（embedded profile 仿真）。

Web 回退：wasm 的 `Backend` 是 `WebBackend`，枚举 `Gpu(WebGpuBackend)` / `Cpu(CpuRaster)`，在页面打开设备（`webgpu::prepare`）之后按结果选定；没有 `navigator.gpu` 或拿不到 adapter 就走 CpuRaster。CPU 页面的窗口由平台建 canvas 2D 的 `Scanout`（`CanvasScanout`），帧经 `present_pixels` 只把 damage 矩形换成 RGBA 后 `putImageData`，尺寸变化时整帧写入。`GpuBackend::shader_lang()` 给出运行时的语言（`SHADER_LANG` 是常量，`WebBackend` 的是 WGSL），renderer 建立时记下它，内建 pipeline、3D lane 与用户 program 都按它取代码；每个 brush 包本就带 `ShaderLang::None` 程序，回退时由解释器执行（wasm 不嵌 `NativeKernel`）；线程起不来时 CpuRaster 只用调用线程。验证：01-counter 在 headless Chromium 中关掉 WebGPU（没有 `navigator.gpu`、adapter 为 null 两种）与 WebGPU 路径、headless golden（`examples/01-counter/golden/counter.png`）逐像素相同，点击与改尺寸后 CPU 与 WebGPU 帧仍相同。

### 6.8.2 Gles3Backend

`viso_gpu::gles` 面向 GLES 3.0+ 的嵌入式 GPU：
- EGL 与 GLES 3.0 运行时加载（`libEGL` / `libGLESv2`，不链接）；display 与 context 在第一个 surface 上按其平台创建：Wayland、XCB、Android、KMS scanout 的 GBM surface，无窗口时 pbuffer；scanout 的每次 swap 连同该帧 damage 交给平台 page flip；
- 着色器是 `viso-shader` 生成的 GLSL ES 3.00，一个程序一份源码含两个 stage，顶点半部在 `VISO_VERTEX` 下，backend 各加 `#version 300 es` prelude 编译两次；built-in 由 shader IR 打印，用户 program 由 program codegen 打印（`ShaderLang::Glsl`），两者在测试中都过 glslang 的 GLSL ES 3.00 检查；GLSL ES 不做隐式转换，整数字面量按上下文类型拼写；
- uniform：built-in 读 `std140` 的 `ViewBlock`（block binding 0）；一个 draw list 所有 draw 的 inline uniform 打进一个 uniform buffer，按 `UNIFORM_BUFFER_OFFSET_ALIGNMENT` 对齐、按 range 绑定；
- instance：built-in 用 divisor 1 的 vertex attribute（每个 draw 重设指针到其 offset）；用户 program 没有 storage buffer，instance 以 `uvec4` 字读自 block binding 1 的 uniform block，每块 `INSTANCE_CHUNK`（16 KB，GLES 3.0 保证的最小 block 大小），经 `glCopyBufferSubData` 拷进 uniform ring 中各自的槽，`viso_base_instance` 给出块首索引；
- bind group：texture 依次绑到 texture unit `0..`，sampler 状态是绑到同一 unit 的 sampler object；
- `Bgra8Unorm` 纹理存为 `RGBA8`（核心 GLES 无 BGRA 存储），上传时交换 R/B，渲染无需处理；sRGB 变体为 `SRGB8_ALPHA8`；8-bit 窗口经 `EGL_KHR_gl_colorspace` 建在 sRGB colorspace 中，报告 `Bgra8UnormSrgb`，default framebuffer 编码 pipeline 写出的值；驱动列出该扩展却拒绝为这个窗口建 sRGB surface 时（部分驱动的 GBM surface，`EGL_BAD_MATCH`），窗口改建为线性，报告 `Bgra8Unorm`，走无 sRGB format 的 OutputEncode 路径；
- 方向：texture target 的 clip-space y 取反（`viso_target`），行仍自上而下存储，`v = 0` 采样顶行，与其它 backend 一致；fragment 位置由 `gl_FragCoord` 还原成左上原点；surface 保持 GL 自身方向；
- 用户 program 在共享对象的第二个 context 上编译，编译器可在任意线程运行（`Gles3Compiler`），结束后恢复调用线程原有的 current context；vertex array 不共享，安装时在主 context 创建；
- 能力表：无 compute、无 bindless、无 indirect；
- partial update：`EGL_EXT_buffer_age` → `frame_age`，`EGL_KHR_partial_update` 区域来自 surface pass 的 scissor，`eglSwapBuffersWithDamage`；
- 面板旋转而 plane 不能旋转时，顶点阶段按 `viso_target` 的四分之一圈数旋转绘制（`viso_place` / `viso_frag_position`），damage 与 scissor 同步旋转；
- 外部画面（`gles::external`）：Android `AHardwareBuffer` 经 `EGL_ANDROID_get_native_client_buffer` 成 EGL image，以 `samplerExternalOES`（`GL_OES_EGL_image_external_essl3`，驱动做 Y′CbCr 转换）按区域与四分之一圈数采样；线性 NV12 dma-buf 两个平面各成 `R8` / `GR88` EGL image（`EGL_EXT_image_dma_buf_import`，有 modifiers 扩展时注明 linear）；手头 NV12 平面上传进 texture 自带的 `R8` / `RG8` texture；后两者按矩阵行（含范围）与色度缩放/偏移转换；都以一个三角形画进自建的 `SRGB8_ALPHA8` texture，framebuffer 写入时编码，shader 先把读到的 gamma 编码值解码；image 按 holds 缓存最近 16 个（buffer 已无人引用的随即释放），每次转换后放一个 fence，hold 持有到 fence signal（帧首与每次转换时轮询）；
- `read_texture`：经 texture 的 framebuffer `glReadPixels`，顶行在前，验证路径。

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
    Backend Codegen: MSL / HLSL SM 5.1 / WGSL / GLSL ES 3.00
                     SPIR-V（WGSL 经 naga 转换；built-in 冻结为提交的 spirv/*.spv）
                     None（CpuRaster：built-in 按 BuiltinShader 分派，用户 program 带 CpuShader 寄存器码）
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
    radius: f32,
    border_width: f32,
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

当前 producer 以稳定 pre-order 重新发出 primitive 流，scene ingest 按位置分配 slot 并 diff：未变化 primitive 不产生任何 mutation（`scene_diff.rs`）。流中每个 `Primitive` 只有最宽的 leaf 加 tag 那么宽（72 字节）：少见而大的几种——frosted material、path、shader、color effect——装在 `Box` 里，不让每个 quad 为它们付宽度。

**Retained drawing（ADR 0053）**。`Content::Drawing`（`viso_ui::Drawing`，`crates/ui/src/drawing.rs`）是节点的一份共享 primitive（`Arc<[Primitive]>`），位于 `(0, 0)`–`extent` 的绘图空间，paint 时按轴拉伸映射到节点的盒子。它可以在 loop 线程之外（如 `viso_ui::work` 的 worker）一次建好；只映射几何类 primitive（quad、image、path、mesh，其余跳过），mesh 经 `MeshPlace` 放置而不复制顶点，extrude 不缩放。它没有固有尺寸，按身份比较，只标 `PAINT`：再次设置同一份 drawing、移动或改变节点大小都不复制、不重建；只随 transform root 移动（平移、缩放、旋转）的节点不重新 paint。

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

Transform root 合同（ADR 0043）：
- scroll 容器的 content 与被平移的节点是 transform root；paint 按前序把它们记进 `TransformTable`，并用 `Primitive::Transform` 括住其子树；
- primitive 按绘制时的 world space 写入；segment 携带自己的 root 与移动 clip 链（`Space`），从不跨 root 合批；instance 里没有 per-instance transform 字节；
- 每个 root 有一个 place：把它绘制时的几何映射到当前位置的 `Affine2`——root 在原位时为恒等，滚动或滑动后为平移，root 被转动（`Turn`：缩放后绕 `transform_origin` 顺时针旋转）时为绕其原点的缩放/旋转，这是轴对齐 instance 无法烘焙的；
- 只改 place 的一帧把表交给 `Renderer::set_transforms`：每个 built-in 读 32 B 的 per-draw view uniform（`viewport`、`shift`、`axis_x`、`axis_y`，即 viewport 加 place 的平移与两轴像，`renderer.rs` 的 `view_uniforms`），draw 按其 root 的 place 映射，root 内打开的 clip 随之移动；不重写也不上传任何 instance，代价按 draw 而非按 primitive；
- scissor 只能跟随平移：转动 root 内打开的 clip 改经 offscreen layer 合成；scissor clip 将被转动的 root 需要重新 paint；
- 像素不只是自身 instance 经 place 移动的内容——offscreen layer（ROI 已按 surface 与 clip 裁过）、backdrop 或 material（采样背后）、用户 program（自写 uniform）——污染最内层打开的 root；移动被污染的 root，或在捕获 backdrop 的帧里移动任何 root，`set_transforms` 回答 `TransformUpdate::NeedsUpload`，调用方按新 place 重新 paint 并上传；
- 平移部分按设备像素取整（length-env scale factor），移动的文字不发虚；屏幕上的 rect 由 `NodeStore::world` 推导，hit test 按逆仿射把指针映射进每个节点的局部空间。

## 8.5 Bounds

每个 primitive 维护两个 rect：

```text
world_bounds  = 自身几何的紧 extent（primitive 不带自己的 transform，所以也是 local extent）
paint_bounds  = geometry bounds + stroke inflation (§11.3) + shadow/effect outsets
```

clip 后的 extent 是 `world_bounds ∩ 发出时的 clip`，在读取处由 entry 的 clip 推导，不在每个 entry 上存一份；需要读取邻域像素的 effect 按 §16.2 在 layer 的 ROI 上计算，也不另存。每个 paint-order entry 因此只带 32 字节 bounds。

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
2. 不绑定 per-draw 资源的 family（quad、各 analytic shape、shadow、mesh）只按 key 合并；采样一张 texture 的 family（image、glyph run 两条 lane、gradient）同样合并，因为 texture 的 bind group 就在 key 里，key 相等即同一资源；用户 program 按 program 槽位合并。只有 ColorTransform、AdvancedBlend、Material 三个 composite family 各自独立成 batch：它们带 bind group 之外的 per-draw 状态（融合矩阵、destination snapshot、backdrop）（`BatchFamily::mergeable`）；
3. **Reorder 条件**：draw B 可以越过中间 draw 集合 M 与更早的兼容 draw A 合并，当且仅当 B 的 instance 在 family buffer 里紧接 A 的末尾（合并后仍是一段连续 range），且对 M 中每个与 B 同 pass 的 draw X：X 与 B 处于同一 transform space，X 的 cover 已知，且 B 与 X 的 cover（paint bounds，没有时用内容 extent；加顶点 extrude 与 1px AA 带；再与 clip 求交）严格不相交。别的 pass 的 draw 不构成 barrier。不满足即为 order barrier。advanced blend、material、backdrop composite 读取的 destination 是在各自 pass 里重画出来的 snapshot，所以对它们只要求像素不相交；
4. “opaque 可任意重排”**不成立**：重叠 opaque 仍依赖顺序；只有引入 depth-ordered opaque pass 后才能放宽，该优化属于 A0，须单独合同。

实现分两层。lowering 按规则 1–2 产生 segment：每个 segment 覆盖连续的 paint-order entry，patch 与 splice 只认这一层。`batch::plan` 再按规则 3 把 segment 折成 encoder 发出的 draw：每个 segment 最多回看 32 个 draw，遇到第一个越不过的 draw 就停下；program draw 的 instance 布局依赖前一个 segment，所以不移动。plan 按 64 个 segment 一块分别折叠，draw 不跨块合并：整帧 lowering 与卸载 program 后全部重折，原地 splice 只重折它碰到的块，所以 splice 的开销不随页面变大。一页 2000 条 note（底色、色块、标签）从 4000 个 draw 降到约 130 个，打一个字的帧多花约 0.5 µs。patch 不改 segment：叶子 patch 要求 bounds 不变，glyph run patch 要求每个 glyph 框不变，所以已证明的不相交依然成立。transform root 移动不重新 plan，因为跨 space 的 draw 本来就是 barrier。规则由 `batch_planner.rs`、`texture_binding.rs`、planner 单测，以及“plan 后与按 paint order 逐 segment 绘制逐像素一致”的 renderer 测试钉住。

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

**Paint-only patch（已实现）**。最常见的局部变动是只改颜色：hover、pressed、focus、颜色动画。此时 stream 的形状不变，只是若干 primitive 换了值。`Renderer::patch(backend, primitives, changed)` 接收上次上传的 stream（已原位改写）和改写位置的升序列表，只处理这些 primitive：

1. 校验：stream 长度不变；帧内没有 backdrop capture（capture 会重新渲染其下方的 entry）；每个位置恰好对应一个 paint-order entry，且是单 instance 单 store 的种类（Quad、四种 analytic、AnalyticShadow、Image）或 glyph 个数、atlas、lane、每个 glyph 的 rect 都不变的 GlyphRun；新 instance 的 bounds（world / paint）与 entry 记录的相同；Image 的纹理与 sampler 相同。任何一条不成立就返回 `false`，什么都不改，调用方上传整个 stream。
2. 把新 instance（乘上 entry 记录的 group-opacity fold）写回 store slot，按 store 的字段 diff 只 bump 移动了的 revision plane，并重算该 slot 的 content stamp 与 damage key。
3. 每个 entry 在 lower 时记下它在 family scratch 中的起始位置；patch 按同样的 origin 减法写回这些 slot（被 offscreen ROI cull 掉的 entry 没有 slot，只更新 store）。segment 不变，因为 key、clip、target 都没变。
4. 各 family 的改写位置本就按 paint order 升序，经同一个 dirty-range coalescer 只上传这些 slot。
5. 改写过的 entry 重算 surface draw key。offscreen pass 的内容 key 是各 entry key 按位置混合后的和，一项变化就地加减，再沿 composite 向上直到 surface draw。自上次 present 以来位移没变（`Transforms` 的 epoch 相同）时，damage 直接比较这些 draw 新旧 key，不再整表 diff。

**Fade patch（已实现）**。opacity 在 (0, 1) 内变化而 layer 的 plan 不变（同样的 reason 被同样的 rung 消除）时，`Layer` primitive 也是一个可 patch 的位置。upload 时每个 layer 记下它的 stream 区间、请求给 planner 的 `LayerRequest`、父级 fold 和绘制方式（in-pass / fold / offscreen pass）。patch 用新 opacity 重新跑 planner，`requested` / `eliminated` / `surviving` 任一变化就拒绝；clip、blur、backdrop 也必须不变。

- offscreen layer（没有 color op、没有 advanced blend）：改 pass 的 opacity 与 pass key，把 composite instance 的 alpha 换成新值，按记录的 image scratch slot 重新 lower，只上传这一个 slot，并沿 composite 换 surface draw key。
- fold 进子节点的 layer：新 fold = 父级 fold × 新 opacity，区间内的每个 entry 以新 fold 重新生成 instance（与普通改写同样校验），只上传这些 slot。同帧在区间内被改写的子节点也用新 fold。

opacity 跨过 0 或 1（layer 出现、消失，或被跳过）仍然是形状变化，整 stream 上传。

差分测试保证 patch 与整 stream 上传留下相同的 scratch、segment、pool shadow、entry key 与 surface draw；一帧 patch 之后 present 的像素与新 surface 上整帧绘制的相同，且只重绘改写的那一行。UI 一侧见 Architecture §19.2（`PaintIndex` / `Painted::Patched`）。`paint_scale` bench：2000 张卡片的页面 hover 一帧约 0.3 µs，与 100 张卡片相同；改之前约 0.7 ms，随卡片数线性增长。一张卡片在两个半透明 opacity 之间淡入淡出一帧约 0.5 µs，同样与卡片数无关。

**Splice（已实现）**。形状改变的局部变动——虚拟列表挂载一行、卸载一行，文本编辑改变 glyph 个数，节点出现、消失、移动、改变大小——改写 stream 的若干段，其余部分只是整体平移。

- UI 侧：每次 paint 给每个节点记下它的 subtree 区间（`Record`：自身 drawable 区间、整段结束位置、所在 transform root、父节点、深度、有无 fade / scroll layer），按 epoch 失效。一帧开始时，从 PAINT worklist、移动过 bounds 的节点、重新挂接过子节点的父节点出发向根收集要访问的节点，按旧区间排序后单遍走：保留下来的子节点区间原样留在 stream 中，只是平移；自身 drawable 改变的节点重新生成并与旧值比较；子节点列表改变的父节点按旧记录 diff 子节点，新子节点在游标处重新 paint，消失的子节点连同区间一起删去。Arena 的 relink 日志同时记下每次改变涉及的子节点（挂上、摘下、开始或停止绘制），diff 因此只访问这些子节点：各自旧区间删去，仍在这个父节点下且绘制的，在它前面最近一个保留区间的兄弟之后重新 paint；起点没变的原样保留。日志不知道是哪些子节点（`forget_children`，或一帧超过 1024 条）时，才逐个走子节点。连续的改写合并成 `Edit`，自右向左 `Vec::splice` 进 stream，并给出新 stream 下标的 `Splice { at, removed, inserted }` 列表；没有改变形状时退化为原位改写（`Painted::Patched`）。overlay 移动、出现或消失，跨过 `Transform` 的删除，fade / scroll layer 开始或结束，transform root 首次出现，都退回整树 paint。
- Renderer 侧：`Renderer::splice(backend, primitives, splices)` 只接受只跨 leaf（Quad、analytic、shadow、Image、GlyphRun）、处在 surface pass 中且不在 folded group 内的段；每段把 paint order 与每个 leaf store 中对应的 slot 换成新 primitive，其后的 entry 按差值移动 stream 下标与 slot，只 ingest 插入的 primitive。`patch` 遇到不再适合原 slot 的 leaf（种类、glyph 个数、bounds、纹理改变）时，把这些位置按一换一的段交给 `splice`。
- Lowering 也 splice：draw 从 segment 指向的位置读 instance，所以插入的 leaf lower 到各 family scratch 的末尾，只上传这段尾巴（`InstancePool::append`）；其余 instance 原地不动，被替换的 leaf 留下的 slot 成为空洞，等下一次整帧 lowering 回收。每个 segment 记着它从哪个 paint-order entry 开始（`segment_entries`），据此找到段两侧的 segment：不超过 256 个 instance、只画 surface leaf 的邻居跟段一起重新 lower，结果与整帧 lowering 的 segment 一致；更长的邻居在段的边界处切开，少一次合并（fragment）。fragment 超过 64 个，或空洞超过活 instance 的一半时，下一次 splice 整帧 lower，压实 scratch。Surface draw 与它们的 key 同样 splice：只给插入的 entry 算 key 和 surface draw，其余 draw 保留原 key，之后的 entry 下标整体平移；offscreen pass 的内容和按 entry 在 pass 内的位置（相对 pass 的第一个 entry）混合，pass 整体平移时不变。Damage 也 splice：被删去的 draw 上次显示的 rect 与插入的 draw 的 rect 记入 staged damage，已显示的 rect 表按同一段 splice，下一次 present 仍是局部重绘；staged rect 超过 256 个时退为整帧。差分测试逐帧比较 splice 后实际画出的内容（把可以合并的相邻 segment 合并后的 instance 字节、每个 entry 的 instance、每个 pool 与其 scratch）、每个 entry 的 key 与 surface draw、每个 pass 的内容与整 stream 上传相同，其间穿插 offscreen pass 内的 patch。

差分测试：UI 侧 16 个随机种子 × 400 帧（填色、隐藏、opacity、插入、释放、移动、改变大小、改文字，页面含 overlay 与平移过的卡片），每帧 splice 后的 stream 与同一帧整树 paint 的逐个相等；renderer 侧 splice 后的 scene 与整 stream 上传逐 store 相等，present 像素与新 surface 整帧绘制相同。`paint_scale` bench：

| case | 整树 paint + 整 stream 上传 | splice |
|---|---|---|
| `mount/rows_30` | 8.3 µs | 4.4 µs |
| `mount/rows_160` | 45.0 µs | 3.6 µs |
| `type/notes_100`（一个 label 增减一个 glyph） | 75 µs | 3.0 µs |
| `type/notes_2000` | 1.49 ms | 3.2 µs |

虚拟列表在数据未变、新窗口与旧窗口重叠时只滑动窗口：两端离开的行回收，两端露出的行挂载，其余行的 key 与 offset 不变，不再逐行求 prefix sum、按 key 二分查找。挂载一行的一帧因此与挂载的行数基本无关。label 的 re-measure 只重做它所在的那一行（Architecture §19.1）；glyph store 把插入的 run 的 glyph 追加在末尾，被移除的 run 的 glyph 留作空洞，其余 run 不动，空洞超过一半时整理一次。一次文本编辑的代价因此与页面大小基本无关；剩下随页面增长的只有 paint order 与 surface draw 下标的逐项平移（整数加法）。

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

Release 构建的 asset 图像在构建期转换为 target 采样的格式（§6.3、Architecture §51.1）：runtime 读 `VTX1` header 借用 texel 直接上传，不解码；`natural` size 是图像布局用的尺寸（构建期预缩小或从 SVG 以 `svg_scale` 光栅化后 texel 尺寸与之不同）。转换图像不生成 mip 链。

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

**Extruded vertices 与共享 mesh buffer（ADR 0053）**：

- `MeshVertex { pos, color, edge, extrude }`：`extrude: [f32; 2]` 跟在 `edge` 之后（stride 28 → 36，`instance_abi_frozen.rs` 冻结），是 draw 的 place 之后再加的屏幕点偏移。mesh 顶点阶段加上 `axis_x · e.x / |axis_x| + axis_y · e.y / |axis_y|`：place 转动偏移，但它的缩放从不拉伸偏移；CPU raster 做同样的计算。普通几何的 extrude 为零。一条线按中心线建、顶点按半宽 extrude，在它所随的 root 的任何缩放下都保持线宽；extrude 一点的 AA fringe 始终是一点。地图等缩放中的几何因此不必按 zoom 重新 tessellate。
- `Mesh` 共享持有 buffer（`Arc<[MeshVertex]>`、`Arc<[u32]>`）与一个 `MeshPlace`（先按轴缩放、再平移，lower 时作用于位置，不作用于 extrude）。mesh store 保存共享 buffer 而非副本；同一组 buffer 在同一 place 视为未变，不逐顶点比较；新 place 是一次 geometry 变化。大 mesh 无论被 paint、移动或重新上传多少次，CPU 上只存一份。
- Damage：mesh 经 place 之后的范围再外扩其最大 extrude（surface draw 记录的 `reach`），收缩中的 root 不会少报 damage（§24.2.1）。
- 代价：每个 mesh 与 path 顶点多 8 字节；冻结的 `mesh.spv` 与 mesh MSL oracle 随顶点布局更新。

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
- page 按需出现：没有 mask 时不占任何 texture；第一页的边长随 footprint profile 定（desktop / web 512²，mobile 256²，embedded 128²，即 16 KiB；`Renderer::set_first_mask_page`），满了追加一页，边长为上一页的两倍且不小于 ROI 向上取的 2 的幂，上限 2048²；ROI 在各页 first-fit。驱逐后重排幸存者，尾部空页归还。设备侧不保留 CPU 镜像：每个新建的 mask 只上传自己的 slot，新页创建时清零，使 slot 边缘的线性采样读到 0；
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

## 21.5 3D Scene Lane 与 glTF 模型

- **Lane**：`Content::Scene` 节点占一个 `SceneSlot`；filler 填 `SceneFrame`（camera、`Lighting`、背景、按 mesh 分批的 `SceneInstance`）后 commit，facade 用 `Renderer::swap_scene` 交换进 renderer；`Primitive::Scene` 把 slot 的离屏 target（`Bgra8UnormSrgb` color + transient `Depth32Float`，reversed-Z）合成进 2D 流。
- **模型（`viso_render::scene3d::model`）**：自带的 glTF 2.0 读取器（`.glb` 与 `.gltf`，JSON 由 `viso-ende` 解析，图像经 `viso_image::decode_straight` 解为 straight RGBA8），不引第三方 glTF crate。`ModelAsset` = meshes（`ModelPrimitive`：索引三角形、法线（缺失时生成平面法线）、两套 UV（`TEXCOORD_0` / `TEXCOORD_1`）、切线（有 normal map 而文件无切线时按该 map 所读那套 UV 的梯度生成）、顶点色（`COLOR_0`，float / normalized u8 / u16，RGB 或 RGBA）、joints/weights）+ metallic-roughness `ModelMaterial` + textures/images + 共享的 `Arc<ModelRig>`（默认 scene 的节点层级、skins、animations）。支持 `KHR_materials_unlit`、`KHR_materials_emissive_strength`、`KHR_mesh_quantization`、`EXT_texture_webp`；其它 *required* 扩展拒绝。读取全部做越界/一致性检查（accessor 越出 view、索引越出顶点、节点成环、URI 跳出目录、元素数超过 2^24 皆拒绝而不越界读）。
- **姿态**：`Pose` = 每节点 local TRS + 解出的 world 矩阵；`ModelRig::animate` 采样 step / linear（旋转 slerp）/ cubic-spline 关键帧并可按权重混合，`Pose::solve` 沿层级合成，`joint_matrices` 给出 skin 的 joint 矩阵。
- **顶点附加属性**：`VertexExtra`（16 B = 第二套 UV `[f32; 2]` + 线性 straight RGBA 顶点色，4 个 unorm16 两两打包进 `[u32; 2]`）只跟在**有它的 mesh** 的每个顶点后面：`Renderer::create_scene_mesh_extra(&[SceneVertex], &[VertexExtra], &[u32])` / `create_scene_skinned_mesh_extra` 上传 64 B / 80 B 的交错顶点，由 `SceneProgram::ExtraMesh` / `ExtraDepth` / `ExtraSkinnedMesh` / `ExtraSkinnedDepth`（`variant` 低字节 7–10，绑定约定同其基础 program）读；没有它的 mesh 仍是 48 B（蒙皮 64 B），走原 program，一字节不多读。取舍：所有顶点加宽到 64 B 会让每个 mesh（内建 shape、地图建筑等绝大多数没有这两项的几何）的顶点读取与显存都 +33%；单独的可选属性流对有它的 mesh 同样是 48 + 16 B，却要每个 backend 多一个顶点 buffer 绑定和一个逐 draw 的「有无」标志，只在 depth / AO pass 对不透明材质省下 16 B；按 mesh 选格式则无绑定改动、一次取一条 64 B（一个 cache line）。`push_model` 只在颜色不全为白、或某个 map 读 `texCoord: 1` 时才用附加格式（Blender 等导出的全白 `COLOR_0` 不占带宽）。顶点色乘 base color（RGB 与 alpha，mask 的 depth pass 亦然）；`MaterialUniforms::mode[3]` 的 `MATERIAL_UV1 << k` 让第 k 张 map（base color、metallic-roughness、normal、occlusion、emissive）读第二套 UV（及其导数），没有第二套的 mesh 以第一套代之；`texCoord` ≥ 2 读第一套。CPU raster 读同一布局、算同一表达式。
- **驻留**：`Renderer::create_scene_model(asset) -> SceneModel` 把几何上传为 lane 的 mesh、图像上传为带 mip 的 texture（颜色图 `Rgba8UnormSrgb`、数据图 `Rgba8Data`，同一图像按两种编码采样时各传一份）、材质建为 lane 的 `MaterialId`，之后 `SceneModel` 只保留 rig；`SceneFrame::push_model(&model, &pose, &transform, tint)` 逐节点按材质推实例。`release_scene_model` 释放。
- **材质**：`Renderer::create_scene_material(&Material) -> MaterialId`（`base_color`、`metallic`、`roughness`、`emissive`、`AlphaMode`、`double_sided`、`unlit`；`release_scene_material` 释放），`SceneFrame::push_with(mesh, material, instance)` 按 (材质, mesh) 分批；`push` / `push_unlit` 用内建的 lit（白、非金属、roughness 0.5）/ unlit 材质。每个材质一个 64 B uniform block（`MaterialUniforms`）+ 5 张 map（base color、metallic-roughness、normal、occlusion、emissive，缺的绑 1×1 白）+ 1 个 sampler；pipeline 按 blend × 双面分四个。`AlphaMode::Mask` 在 fragment 里 discard，`Blend` 的实例与 sprite 一起由远到近排序、合并成连续 run 画。
- **着色**：metallic-roughness PBR —— GGX 分布、height-correlated Smith 可见性、Schlick Fresnel；直射光按「白色漫反射正对强度 1 的光反射其颜色」定标；法线 map 按切线空间（切线 w 给副切线方向，镜像实例翻转）；镜像实例（行列式为负）的面绕向相反，单面材质对它改用剔除正面的 pipeline（`CullMode::Front`），双面材质不剔除；蒙皮 draw 的绕向由实例与 mesh 用到的 joint（上传时记下有权重的 joint 位图）的行列式符号一起定：一致时整个 draw 剔除背面或正面，有的 joint 镜像、有的不镜像时整个 draw 不按绕向剔除，由蒙皮 program 逐 fragment 判断（`viso_gpu::scene::faces_away`：world 位置的屏幕导数给出三角形平面，取相机所在一侧为正，已按行列式翻回的外法线背离它的单面材质 fragment 即 discard，与绕向、屏幕手性无关）——按三角形拆 draw 要每帧在 CPU 上蒙皮，这里只多着色混合 draw 的背面，仍是一个 draw；specular AA 以法线的屏幕导数加宽 roughness；环境光 = SH9 漫反射辐照度 + 按 roughness 选 level 的预滤波 radiance × Karis 近似 env BRDF，occlusion map 也压暗 specular；emissive 叠加；按 `Lighting::exposure` 曝光后 Khronos PBR Neutral tonemap，再叠 fog。`Lighting::ambient` / `reflections` 缩放漫反射 / 镜面环境光。所有 map 都以显式梯度（`gradient2d`）采样，梯度在 fragment 开头一次算出，WGSL 的 uniformity 规则下也可在分支内采样。CPU raster 用 `viso_gpu::scene::shade_mesh` —— 与 GPU program 同一表达式，导数取相邻像素中心（三角形外延伸）。
- **环境**：`Environment::from_equirect(w, h, radiance)`（线性 RGB 的 equirect 图）或 `Environment::gradient(sky, horizon, ground)` 在调用线程上预滤波（数毫秒）：256×256 `Rgba16Float` atlas 内 6 个 equirect level（level k 为 (256>>k)×(128>>k)，roughness = k/5），每个 texel 以 48 个 GGX 重要性样本按样本立体角从源的 mip 链取（filtered importance sampling）；漫反射为源投影的 SH9（带上余弦瓣的 band 系数，存为 frame uniform）。`SceneFrame::environment` 为 `None` 时由 `Lighting` 的 sky / ground 生成 gradient 环境，lane 缓存最近 4 个；slot 的环境变化时才重新上传。
- **阴影**：方向光的 cascaded shadow map，`Lighting::shadows: Shadows { strength, distance, resolution }`（默认 1.0 / 200 / 1024；`Shadows::NONE` 或 strength 0 不画阴影 pass）。3 个 cascade 拟合到视锥里**有接收者**的那段深度（可见 lit 实例的视深范围，限在 `distance` 内；近远端按每倍四级的几何阶梯取整，texel 尺寸只阶跃不漂移），按 0.75 log + 0.25 uniform 切分；每个 cascade 取切片 8 角点的包围球（相机转动时 texel 尺寸不变），中心在光空间按整 texel 吸附（相机平移时栅格不爬行），深度向光源方向延伸到所有 caster 的世界包围盒（视野外的高物也能投进来）。caster = 非 blend 材质的全部实例（不受视锥裁剪；mask 材质按自己的 base color alpha 裁剪，其余合并成内建 lit 材质，同 mesh 一个 draw），每个 cascade 只画包围盒触及其 tile 的实例。阴影图集为每个 slot 一张 `3r × r` 的 `Rgba8Data` render target（r = 视图长边的 2 次幂，限在 `256..=resolution` 且 3r 不超过设备最大纹理），三块 tile 并排、各自 scissor；深度程序（`SceneProgram::Depth`，同一绑定约定、`variant` 低字节 1、两面都画、reversed-Z transient depth）把到光源的深度打包成 24 位写进 RGB（`viso_gpu::scene::pack_depth`，8 位目标精确存储），不依赖可采样的深度纹理或浮点 render target，所有 backend 与 CPU raster 一致。接收端（只有 lit 材质）取包含该点的最近 cascade，沿几何法线外推 `1.5 texel × (1 − N·L)`、深度 bias `texel × (1 + 2(1 − N·L))`，4×4 tent PCF（16 次点采样，3×3 box 在 texel 间插值，半影连续），cascade 外圈 10% 与下一级交叉淡化，最后一级之外淡出为全亮；只压暗直射光。阴影 pass 与该 slot 的场景 pass 一同只在 frame / 尺寸变化时渲染；`SceneStats::shadow_passes` / `shadow_draws` 计数。
- **蒙皮**：`SkinnedVertex`（64 B = `SceneVertex` + 4 个 u16 joint 索引 + 4 个 unorm16 权重，各两两打包进 `[u32; 2]`）由 `Renderer::create_scene_skinned_mesh(&[SkinnedVertex], &[u32])` 上传；`SceneFrame::push_skinned(mesh, material, instance, &joints, bounds)` 每个实例给出 joint 调色板（每个 joint 从 bind pose 到当前姿态的仿射，mesh 空间，最多 `MAX_JOINTS` = 256 个）和姿态下的包围盒（用于视锥裁剪与 caster 选择）。调色板每 joint 3 行，写进该 draw 的第三个 uniform 块（每个蒙皮 draw 一块 256 × 48 B 的 buffer，slot 内按需增加、跨帧复用）；每个蒙皮实例单独一个 draw。顶点程序 `SceneProgram::SkinnedMesh` / `SkinnedDepth`（`variant` 低字节 2 / 3，同一绑定约定）在 GPU 上按归一化权重求和 joint 仿射，再套实例变换，法线走余子式（镜像时翻转）、切线走仿射；CPU raster 走同一表达式（`viso_gpu::scene::skin_vertex`）。`push_model` 对带 skin 的节点用 `joint world × inverse bind` 作调色板，包围盒取每个 joint 影响半径球经姿态变换后的并集；`SceneModel::bounds(&pose)` 返回模型在该姿态下的包围盒，供取景。
- **天空**：`SceneFrame::sky: Option<Sky>`（`Sky { sun, turbidity, strength, sun_disc }`，默认 turbidity 2.5）在没有不透明 mesh 的地方画 Preetham 解析日光天空：只依赖太阳与浑浊度的部分（每个通道 5 个 Perez 系数、天顶 Yxy 及其归一化）由 `Sky::uniforms(inv_view_proj)` 每帧在 CPU 上算一次，写进 `SkyUniforms`（208 B，uniform slot 1）；`SceneProgram::Sky`（`variant` 低字节 4）用一个覆盖整个 target 的远平面三角形，沿每个 fragment 的视线（由 `inv_view_proj` 反投影近平面与两倍深度两点，透视与正交相机通用）求 Perez 函数，xyY→线性 sRGB，按与 mesh 相同的曝光与 tonemap 输出，不加 fog。pipeline 用 `GreaterEqual`、不写深度，在不透明 draw 之后、blended / sprite 之前画，只着色真正露出的天空。太阳低于约 2° 时模型的太阳停在 2°（Perez 归一化在其下发散），日间天空随太阳高度在 0° 到 −12° 间淡出到夜空穹顶，并叠加以太阳方位为中心的暮光；太阳圆盘（角半径 0.00465 rad，lane 保证至少 1.5 px 宽）按与中心夹角的 `|d × s|` 平滑边缘。帧未指定 `environment` 时，环境取 `Environment::from_sky(&sky)`（128×64 equirect、不含太阳圆盘，再预滤波；lane 按 sky 值缓存最近几个，太阳动画时每次变化要重建一次，数毫秒，可在 worker 上自建后设 `frame.environment`）。`Lighting::sunlit(&sky)` 给出来自太阳方向、颜色为 `Sky::sunlight()`（Kasten–Young 大气质量下的 Rayleigh + 霾消光，正午约为白，日落偏红，日落后为 0）的直射光。CPU raster 走同一表达式（`viso_gpu::scene::shade_sky` / `sky_radiance`）。
- **环境光遮蔽**：`Lighting::occlusion: AmbientOcclusion { strength, radius, bias }`（默认 0.7 / 0.5 / 0.02，`radius` 与 `bias` 是世界单位；`AmbientOcclusion::NONE` 关闭）。帧里有不透明的受光 mesh 且 `strength > 0` 时，lane 每帧多画 4 个半分辨率 pass（`SceneStats::occlusion_passes` 计数）：用 depth program 与相机的 mesh 分组画一遍视图深度（打包的 `1 − z`，Rgba8Data），`SceneProgram::Ao`（`variant` 低字节 5）对每个 texel 反投影出表面点，法线取左右 / 上下邻点中深度更接近的一侧（轮廓处不产生假斜面），按法线半球取 12 个 tap（离切面抬高 0.18–0.92，距离 (0.2 + 0.8 i²)·radius 由近到远，按 interleaved gradient noise 每 texel 固定旋转，不闪烁），tap 被其所在 texel 的表面挡住超过 `bias + d · slope`（slope 取 `0.002` 与半个 texel 在该处的世界尺寸中较大者）即计入，深度差超过 radius 的另一物体渐隐；输出 R = 未遮蔽比例，G/B 以 16 位存 `log2(1 + d) / 16`（d 为到相机距离）。`SceneProgram::AoBlur`（低字节 6）沿横、竖各一遍 9 tap 双边模糊（高斯权重 × 距离差 `exp(−(Δd / (0.03 d + 0.02))²)`），不跨轮廓。这两个 program 共用 `AoUniforms`（112 B，uniform slot 1：`inv_view_proj`、radius / bias / slope、texel 尺寸、模糊步长），输入绑 T6（点采样），pass 都附上同一张 transient depth（`Always`、不写），CPU raster 也能执行。mesh program 以线性 sampler 在 T7 读结果（`FrameUniforms::occlusion = [strength, 1/w, 1/h, 0]`，`FrameUniforms` 因此为 512 B），只压暗环境光（SH 漫反射与预滤波镜面）：`ao = 1 − strength · (1 − T7)`，与 occlusion map 相乘，直射光不受影响。没有 AO 时 T7 绑 1×1 白，不建任何 target。CPU raster 走同一表达式（`viso_gpu::scene::shade_ao` / `shade_ao_blur`）。
- **Mip**：glTF 图像在 asset worker 上按用途生成整条 mip 链（`ImageKind`：颜色在线性光里按 alpha 加权平均、数据原样平均、法线平均后重新归一化；同一图像多用途时 Data > Normal > Color），经 `GpuBackend::create_texture_mipped(desc, levels)` 一次上传（Metal / Vulkan / D3D12 / WebGPU / GLES / CPU raster 都支持，默认实现只传 level 0），以 `FilterMode::MipmapLinear` 三线性采样。
- **绑定约定**：scene program（`SceneProgram`：`Mesh`、`Depth`、`SkinnedMesh`、`SkinnedDepth`、`Sky`、`Ao`、`AoBlur`、`ExtraMesh`、`ExtraDepth`、`ExtraSkinnedMesh`、`ExtraSkinnedDepth`，`Sky`、`Ao`、`AoBlur` 为覆盖整个 target 的屏幕 program；`viso_shader::scene_program_code(program, lang)` 给出各语言代码，SPIR-V 同样冻结）的 bind group 依次为 3 个 uniform（frame、material 或屏幕 program 的 `SkyUniforms` / `AoUniforms`、skin 的 joint 调色板）、8 张 texture（5 张材质 map、环境 atlas、阴影图集或 AO pass 的输入、AO 结果）、3 个 sampler（材质、线性 clamp、点采样）；第 k 个同类 binding 进该类的第 k 个槽：Metal `buffer(2+k)` / `texture(i)` / `sampler(j)`，HLSL `b{k}` / `t{i}` / `s{j}`（scene 专用 root signature：3 个 root CBV + SRV table + sampler table），WebGPU group 0 = uniform、group 1 = texture + sampler，Vulkan / GLES 按 `viso_gpu::scene::NATIVE_*_BINDINGS`；texture 用哪个 sampler 由 `TEXTURE_SAMPLERS` 定。lane 总是填满所有槽（缺的 texture 绑 1×1 白、缺的 uniform 绑 16 B 备用 buffer）。

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
| `TRANSFORM` | `TransformRevision`：transform table 条目，damage 来自子树 bounds（只标 `TRANSFORM`，不标 `MEASURE` / `LAYOUT` / `PAINT` / `HIT_TEST`；节点第一次平移时 paint 一次以记录 root） |
| `PAINT` | `PaintRevision`；引用资源变化 → `ResourceRevision`；effect 参数 → `EffectRevision`；clip → `ClipRevision`；可见性 → `VisibilityRevision` |
| `HIT_TEST` / `SEMANTICS` | 无 render revision |

规则：只 bump 实际变化的 plane；一个颜色改变不得升级成 geometry rebuild；backdrop 依赖按 §17.7 另行追踪。

### 24.2.1 Damage-driven present

- damage 由每个 entry 的 content key（store entry + context + 采样的 pass / capture / material / clock）与上一次 present 的列表按序比较得出；transform root 的 place 与 clip 在 submit 时作用（`viso_render::damage`、`renderer/present.rs`）：draw 的 paint bounds 经 place 映射，再外扩 1 device px AA fringe 与 mesh 的最大 extrude（ADR 0053，§13.4）；
- 合并成最多 4 个不相交 rect；超过 surface 60% 退回整帧；尺寸变化、clear 变化、`damage_all()` 强制整帧；
- buffer-age 感知的局部重绘：`frame_age`（D3D12、Vulkan、GLES3、CpuRaster、headless；4 帧历史），`LoadOp::Load` + 每个 rect 的 clear quad + scissor 回放；空 damage 跳过整帧；与整帧逐像素一致（`crates/render/tests/damage.rs`）；
- `present_with_damage`：D3D12 `Present1` dirty rects、Vulkan `VK_KHR_incremental_present`、GLES3 `eglSwapBuffersWithDamage`、KMS `FB_DAMAGE_CLIPS`；Metal / WebGPU 不知道 drawable 年龄，变化的帧整帧重绘；
- 计数器 `damage_px`、`damage_rects`、`full_frame_fallbacks`；`viso inspect frame` 报告该帧 damage 供 Inspector overlay。

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

当前实现：`Primitive::Shader(Box<ShaderDraw { rect, program: ProgramSlot, params }>)` 以一个 Brush Profile Program（DSL §100）绘制一个盒子。`params` 是该 Program 的一个已编码 Instance（`BrushLayout::encode`，盒子为零），以 `Arc<[u8]>` 共享，节点未变时重发只复制指针，Scene Store 的 Diff 先比指针：`rect_pos` 归 Transform、`rect_size` 与 Program 归 Geometry、参数归 Paint。Program 按 Renderer 的槽位安装：`set_program`（调用方自备的 Pipeline）或 `install_program`/`install_brush`（按代码取 Pipeline），同一槽位再次安装即替换；`release_program` 清空槽位并经 Backend 的 Fence 延迟销毁其 Instance Buffer。Renderer 按生成代码与 ABI 持有 Pipeline：代码与 ABI 相同的 Program 共用一个 Pipeline，Renderer 未持有时才当场编译（`Renderer::program_compiles` 计数）；在渲染线程外用 Backend 的 `program_compiler` 编译好的 Program 以 `adopt_program` 交给 Renderer，之后安装它的槽位直接取用。没有槽位使用的 Pipeline 最多保留 `IDLE_PIPELINES`（8）个，供回到它的编辑复用，超出时最久未用的先经 Fence 销毁，所以多少次编辑之后 Pipeline 数都有上界（`FrameStats::program_pipelines`）；调用方自备的 Pipeline 随其最后一个槽位销毁。槽位为空或参数长度不是其 Stride 的绘制不画（`FrameStats::program_draws_dropped`）。每个 Program 一个持久 Instance Pool：Lowering 把盒子写进复制来的 Instance（减去 Offscreen Layer 的 Origin），不接续上一绘制的 Run 起于对齐到 `Caps::program_instance_align` 的字节 Offset，Pool 与 CPU Shadow 逐字比对，只上传变化的范围。Batch Family `Program` 的 Key 在保留的高位字段（50..64）带槽位，所以只有同一 Program 的相邻绘制合并为一次 Draw，两个 Program 相邻计为一次 Pipeline 切换。Uniform Block（`viewport_size`，以及声明了的 `time`）以 Inline Uniform 按 Program 的 Layout 在构建每个 Draw Command 时写入：每个 Pass 的 Viewport 与 `Renderer::set_time` 设下的时钟（Facade 每帧写入窗口首帧以来的秒数），所以只走时钟的帧上传零字节。主 Pass 上盒子与其 Clip 及 Surface 都不相交的 Program 绘制不 Lower（计入 `culled_primitives`，Inspect 报零长 Range）；Lowering 统计画出的读时钟盒子（`FrameStats::clock_instances`，`Renderer::reads_time`），Facade 据此像动画一样让窗口继续出帧，该数为零即停：判定的是这一帧实际画出的盒子，不是装着的 Program。采样图像的 Program：`ShaderDraw::textures` 按 Program 的 Texture 槽位给出节点的图像（`ProgramTextures`），缺任一图像的绘制不画（计入 `program_draws_dropped`）；一次绘制绑定一个 Bind Group，先是这些图像、再是 Program 的 Sampler（安装时按 Descriptor Intern），每组图像的 Bind Group 缓存在 Program 上，稳定帧不新建，随 Program 的替换/释放或 `release_texture` 所释放的图像一起经 Fence 销毁；Bind Group 进入 Batch Key，所以同一 Program 的相邻绘制只在图像相同时合并，换图只动 Resource 平面、不上传 Instance。Program 的输出对 Renderer 不透明，Group Opacity 从不折进它（`scan_layer_subtree` 视其为不可折），包着它的半透明组走 Offscreen。Inspect 以 `BatchPipeline::Program` 报告其 Batch（Range 起点为字节 Offset），`FrameStats` 增加 `shader_instances`、`program_draws_dropped` 与 `programs`。Headless 上 Program 以 `CpuShader` 运行，Metal 上同一场景与之逐像素比对（每通道差不超过 2/255）。UI 侧 `Content::Shader { program, params, images }` 让节点以其盒子发出该 Primitive，`images` 是各 Texture 参数所名的 Resource 及其驻留后的 Texture，Store 像解析 Asset 节点一样经 Asset Table 解析它们：图像驻留或被替换时只把采样它的节点标 `PAINT`，未全部驻留时节点不画；Node Store 的 `ShaderSlots` 按 Program 的 `Arc` 身份分配槽位并带 Revision，Facade 只在 Revision 变化时把变化的槽位装进窗口的 Renderer。作者侧是 DSL §100 的 `ShaderView { shader: Ripple { .. }; }`：Shader 名即其参数的 Record 类型，`shader` 只脏 `PAINT`。

View 的 Shader 表：View 把它的 `ShaderView` 所画的每个 Shader 按首次出现的顺序列一次，控件以表中下标指名其 Shader（`ControlKind::Shader`、`Control::shader`），值的字段按声明顺序是 Brush 的参数，送达时以 `BrushLayout::encode` 编成一个 Instance（`ColorLinear` 由 `Color` 转到线性光），参数或 Program 变化才重画。表中每项是构建期打包的 `PackedBrush`（Layout 与每种语言的代码各一份）：Release Package（`ViewPackage::shaders`）打包 `TargetProfile::shader_langs` 给出的语言，即目标 Backend 的与参考解释器的；`view!` 在宏展开时为每个 Backend 各打一包，经 `__per_backend!` 以与 `viso_gpu::Backend` 相同的 `cfg` 只留下目标的那一包（另含参考解释器的 Program）；Dev Host 按构建嵌入的同一组语言（Runtime 报告的语言与参考解释器的）打包随 View Patch 发送。Package 不带 Source Span（Span 只对编译它的文件有意义），所以编辑只移动了 Shader 在文件中的位置时它打包出的字节不变。相同字节在进程内加载为同一个 `Arc<BrushProgram>`（弱引用缓存，构建嵌入的在 Loop 上加载、Patch 带来的在 Dev Link 的读线程上加载，二者共用），所以同一 Shader 无论出自哪个 View、Package 或 Reload，在一个窗口里只占一个槽位、一条 Pipeline，Reload 未改动的 Shader 什么都不重装。槽位计数画它的节点：节点换成别的内容或被释放即放开，最后一个放开时槽位清空。Facade 每帧在 Settle 之后、Layout 之前，只在 `ShaderSlots` 的 Revision 变化时把变化的槽位装进窗口的 Renderer（`install_brush`，以该 Backend 语言的代码），清空的槽位经 Fence 释放；Backend 拒绝的 Program 报告一次，其槽位不画。Hot Reload 时 Patch 带来的新 Program 已在渲染线程外为每个窗口编译好并在提交前交给其 Renderer，所以这一步只取用，失败的 Program 在提交前就让整个 Revision 被拒绝（Hot Reload §19）。

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
| cross-backend image diff | `d3d12_backend.rs`、`vulkan_backend.rs`、`webgpu_backend.rs`、headless；DSL Shader Program：`viso/tests/shader_golden.rs`（CPU Reference、Metal、Vulkan）；3D scene lane：`scene3d_lane.rs` 与 `scene_model.rs` 把同一画面在本机每个设备上画出并与 CpuRaster 比较（`tests/devices`：Apple 上 Metal，Linux/Android 或 `vulkan` feature 下 Vulkan，Linux 加 `gles` feature 时 GLES 3，Windows 上 D3D12；缺设备则跳过），`webgpu_backend.rs` 在 WebGPU 上画同一组 lane 画面（`tests/scene_pictures`）。Deno 2 下跑 WebGPU 测试用 `tools/deno2` 作 `deno` |
| premultiplied alpha / sRGB-linear | `color_domain.rs`、math `color.rs` tests |
| blend | `blend_contract.rs` |
| gradient edge、stroke join/cap、path fill-rule、clip nesting | golden scenes（按阶段补齐） |
| degenerate geometry / NaN | math `legality.rs` tests + golden |
| DPI scaling、surface recreation | gpu surface tests（`metal_uikit_surface.rs` 等） |
| resource lifetime stress、generation | `gpu/tests/generation.rs`、`render/tests/fence_recycle.rs` |
| device-loss simulation | headless backend：`CpuRaster::lose_device`（§6.4） |
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
