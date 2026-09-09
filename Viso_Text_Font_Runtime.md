# Viso Text / Font Runtime 设计规范

> 文档状态：Viso 1.0 Draft / Text & Font Runtime Specification
> 适用范围：`viso-text`、`viso-platform` 字体 adapter、`viso-render` glyph atlas、Asset/Build font manifest、WASM/Canvas font provider
> 设计优先级：帧稳定性 > 热路径 CPU > GPU/内存占用 > 首次字体命中延迟 > API 便利性
> 核心原则：Native 默认依赖系统字体但不扫描系统字体；项目字体 build-time 自动登记、runtime lazy load；WASM 不拥有隐式系统字体；Glyph 渲染按实际屏幕行为选择精确 A8 Coverage、可缩放 MTSDF、极端缩放的 retained Vector Outline 或 Color Glyph 路径；稳定状态回到最适合当前像素密度的精确表示；Text 稳态路径不做字体解析、fallback、shaping、representation promotion 或 raster 工作。

---

# 1. 目标

Viso Text Runtime 必须同时满足：

```text
Native App 默认不需要打包字体
CJK / Emoji / 多语言默认依赖 OS fallback
启动不扫描整台机器字体
项目字体无需写初始化代码即可使用
WASM/Canvas 不偷偷读取系统字体
WOFF2 是一等 packaged/external font 格式
长期运行时字体内存有明确预算
font picker / 大文档 / CJK 不污染常用字体 cache
Glyph Atlas 满时不全量 reset
普通 UI / 小字号 / CJK 默认走 A8 Coverage
连续 zoom / 大范围 scale / rotation / world-space text 才按需使用 MTSDF；极端缩放/高精度矢量场景可进入 retained Vector Outline
Color Emoji / Color Glyph 使用独立 RGBA 路径
120 / 144 / 240 Hz 下静态文字接近零 CPU 维护成本
```

Text Runtime 不追求：

```text
自己复制一份系统 FontDB
启动时解析所有系统字体
每帧通过 family string 查字体
每个 glyph draw 修改 LRU 链表
把全部字体相关对象塞进一个总 cache
让所有普通文字默认生成 SDF/MSDF/MTSDF
让每个 glyph 默认同时常驻 Coverage + MTSDF + Vector 多份表示
每帧、每 glyph 重新决定 representation policy
为了 Web Canvas 偷偷依赖浏览器 system-ui
```

---

# 2. 最终总模型

```text
                        BUILD TIME
                            │
                 scan project assets/fonts/
                            │
                    Compact FontManifest
                            │
            ┌───────────────┴────────────────┐
            │                                │
          Native                         WASM/Canvas
            │                                │
   App packaged/dynamic fonts       App packaged/dynamic fonts
            │                                │
            │ miss/coverage miss             │ miss/coverage miss
            ▼                                ▼
   OS Font/Fallback Resolver        External FontProvider
            │                                │
            └───────────────┬────────────────┘
                            ▼
                      Resolve Cache
                            │
                            ▼
                Font Face Cache (SLRU)
                    byte-budgeted
                            │
                            ▼
                     Shaping Cache
                    byte-budgeted
                            │
                            ▼
                 Retained Paragraph/Run
                            │
                            ▼
                 Glyph Representation Policy
                 quality/perf adaptive + retained
                            │
          ┌─────────────────┼──────────────────┬─────────────────┐
          │                 │                  │                 │
          ▼                 ▼                  ▼                 ▼
      MaskA8          ScalableMtsdf       OutlineVector      ColorGlyph
   exact/stable       animated/zoom       extreme/precise     source-aware
          │                 │                  │                 │
          ▼                 ▼                  ▼                 ▼
  Coverage Atlas      MTSDF Atlas       Vector/Mesh Cache   Color Atlas/Vector
          │                 │                  │                 │
          └─────────────────┴────────────┬─────┴─────────────────┘
                                         ▼
                                    GPU Batches
```

这里的关键是：

> **字体“发现/登记”、字体“解析/加载”、文字 shaping、glyph raster、GPU residency 是五个不同生命周期。**

它们不得被一个 `FontDB` 或一个统一 LRU 混在一起。

---

# 3. 项目字体：自动登记，不 eager load

## 3.1 `assets/fonts/` 的正式语义

项目中：

```text
assets/
└── fonts/
    ├── Inter-Regular.woff2
    ├── Inter-Bold.woff2
    └── NotoSansSC-Regular.woff2
```

构建时 Viso 自动：

```text
scan assets/fonts/
    ↓
validate font container
    ↓
extract minimal metadata
    ↓
family / style / weight / stretch / face index / variation metadata
    ↓
small ScriptCoverageSummary / color-font capability summary
    ↓
create FontManifest entries
    ↓
package asset
```

`ScriptCoverageSummary` 只用于快速排除明显不可能覆盖某个 script 的 packaged face，不代替完整 cmap，也不把完整 Unicode coverage 常驻进 Manifest。精确覆盖仍在 face lazy load 后验证。

这叫：

```text
build-time automatic registration
```

而不是：

```text
runtime eager font loading
```

Runtime 启动只保留 compact descriptor：

```text
Inter Regular -> AssetId(12)
Inter Bold    -> AssetId(13)
Noto Sans SC  -> AssetId(14)
```

第一次真正需要 `Inter Regular` 时才：

```text
FontManifest lookup
    ↓
Face Cache miss
    ↓
lazy read/decode font asset
    ↓
parse required tables
    ↓
insert Font Face SLRU
```

因此项目里即使存在几十个字体，也不能导致启动阶段全部解析。

## 3.2 动态字体仍然显式输入

不在 build asset graph 中的字体使用显式 runtime API：

```rust
cx.fonts().register_bytes(...);
cx.fonts().register_provider(...);
cx.fonts().set_default_family(...);
cx.fonts().set_fallback_chain(...);
```

典型用途：

```text
user selected local file
document embedded font
network font
plugin font
host bridge
runtime generated/subset font
```

## 3.3 默认 family

Native：

```text
Text 没有指定 font family
    ↓
App/theme 显式 default family 存在？
    ├─ yes -> App default
    └─ no  -> SystemUi
```

WASM/Canvas：

```text
Text 没有指定 font family
    ↓
App/theme default family 存在？
    ├─ yes -> use packaged/dynamic/provider font
    └─ no  -> Missing Font policy
```

WASM 不因为 `Text` 需要显示就偷偷内置一个框架字体。

---

# 4. Native：不扫描系统字体

## 4.1 普通 App 不建立 FontDB

Viso 禁止正常启动执行：

```text
enumerate every installed font
    ↓
open every font file
    ↓
parse name/cmap tables
    ↓
build framework-owned FontDB
```

普通 Native `FontRequest`：

```text
FontRequest
    ↓
Resolve Cache
    ├─ hit -> ResolvedFaceLocator
    └─ miss
         ↓
    App FontManifest / dynamic app fonts
         ↓ miss
    OS native font resolver
         ↓
    one matching face / fallback run
         ↓
    Resolve Cache
```

因此“用户安装了 3000 个字体”不能直接把 Viso App 的启动时间变成 3000 个字体的函数。

## 4.2 平台 adapter

Viso 的 public Text API 不依赖任何系统字体对象。`viso-platform` 只实现窄 adapter：

```text
macOS / iOS
    CoreText font creation + cascade/fallback

Windows
    DirectWrite font matching + system fallback

Linux
    fontconfig-backed matching/coverage query

Android
    platform font matcher/configuration adapter
```

平台对象只存在 adapter/runtime 内部。

Android 的优先实现路径：

```text
API 29+  -> AFontMatcher / system font APIs
older supported API -> compatibility adapter based on Android system font configuration
```

兼容 adapter 也不得为了匹配一个 run 而遍历任意字体目录。若旧 Android 的系统能力无法在性能/正确性上满足合同，应通过 target capability/minimum API 策略解决，而不是在 Viso 核心重新建立全局 FontDB。

系统 path/native pointer 不进入：

```text
public API
stable wire ABI
Ende snapshot
cross-device cache key
```

## 4.3 系统字体目录是独立 cold service

只有 Font Picker、设计软件等明确需要所有字体时才调用：

```rust
cx.fonts().catalog().enumerate_async(...)
```

其语义：

```text
explicit request
    ↓
async enumerate descriptors only
    ↓
family/style/weight/locator metadata
    ↓
VirtualList display
    ↓
only visible/selected face is actually loaded
```

系统 catalog 枚举：

- 不属于 App startup；
- 不属于普通 Text resolution；
- 必须 async；
- 不得 parse 全部字体 outline/table；
- 结果允许短期缓存；
- 不得让 3000 个 preview face 进入 Protected Face Cache。

---

# 5. CJK fallback 最终方案

## 5.1 不逐字符扫描字体

错误方案：

```text
character U+4F60
    ↓
scan all system fonts
    ↓
find cmap containing U+4F60
```

Viso 禁止这种实现。

正确方案：

```text
Unicode/BiDi segmentation
    ↓
script/language shaping run
    ↓
requested/App face local coverage check
    ↓ coverage miss on a contiguous cluster/run
OS fallback resolver(text range, locale, style, base face)
    ↓
fallback face + mapped run length
    ↓
load/cache that face once
    ↓
shape whole mapped run
```

关键点：

> **系统 fallback 查询以 run/cluster 为单位，不以 Unicode scalar 为单位。**

## 5.2 常见 CJK 后续不重复问 OS

第一次中文 run：

```text
Inter -> coverage miss
    ↓
OS fallback -> PingFang / YaHei / Noto CJK / system-selected face
    ↓
FallbackPlan remembers candidate
    ↓
Face coverage accelerator available
```

后续同类 run：

```text
(base face, script=Han, locale=zh-*, style)
    ↓
recent fallback candidate
    ↓
local cmap/coverage check
    ├─ covers -> use directly
    └─ miss   -> OS fallback again
```

所以正常中文页面不会每个汉字调用 CoreText/DirectWrite/fontconfig/Android matcher。

## 5.3 CJK locale 是 fallback key 的一部分

必须区分：

```text
zh-Hans
zh-Hant
ja
ko
```

因为同一 Unicode Han 字符在不同语言环境中的 preferred glyph/font 可能不同。

FallbackPlanKey 至少包含：

```text
base/requested face identity
script
language/locale
weight/style/stretch
variation coordinates when relevant
presentation mode
font source revision
```

---

# 6. Emoji 最终方案

Emoji 不能按单 code point fallback。

必须先保持 grapheme/emoji cluster 原子性，包括：

```text
variation selector VS15 / VS16
ZWJ sequences
skin tone modifiers
regional indicator flags
keycap sequences
family/person sequences
emoji presentation selectors
```

例如：

```text
👩‍💻
```

不能拆成：

```text
👩  +  ZWJ  +  💻
```

分别选择三个字体。

流程：

```text
emoji grapheme cluster
    ↓
requested/App face supports full cluster?
    ├─ yes -> shape
    └─ no
         ↓
    OS fallback resolver receives full cluster
         ↓
    system emoji face + mapped range
         ↓
    shape/raster as one cluster
```

Native 默认系统字体路径必须正确命中系统 Emoji font，而不要求 App 打包 Emoji 字体。

## 6.1 Color Emoji

Glyph image contract 至少区分：

```text
MaskA8          normal monochrome glyph
ColorRgba8      rasterized color glyph / emoji
VectorColor     optional retained/vector color representation
```

Native 的最低产品要求是：

> **系统 Emoji 能通过平台字体/raster adapter 正确输出 RGBA glyph，而不要求 Viso 自己重新实现所有系统 color-font table。**

Packaged/WASM color-font 支持由可替换 font primitive 提供；标准能力至少应覆盖现代 OpenType color-font 的主流路径，并对无法解析的 color glyph 给出结构化诊断，而不是崩溃。

Color Emoji 使用独立 atlas budget：

```text
Glyph Atlas A8
Glyph Atlas RGBA
```

避免大量 Emoji RGBA 页面把普通文本 mask atlas 挤掉。

---

# 7. 字体格式

Viso 1.0 packaged/external font 输入至少支持：

```text
.ttf
.otf
.ttc
.otc
.woff2
```

WOFF2 是一等格式，尤其适用于 Web/WASM asset delivery。

内部不把 WOFF2 当成 shaping ABI：

```text
WOFF2
    ↓ decode/decompress
normalized OpenType/SFNT face data
    ↓
FontFace
    ↓
Shaper / Rasterizer
```

因此 Font Face Cache 缓存的是可直接使用的 decoded/parsed face state，不是“每次 shaping 都重新解 WOFF2”。

Web 默认推荐源资源使用 WOFF2，但是否由 build pipeline 自动将 TTF/OTF 转换为 WOFF2 属于 packager optimization policy；它不能改变字体许可，也不能静默改写用户资源语义。

---

# 8. Resolve Cache

Resolve Cache 是很小的 warm-path cache，不是 FontDB。

```text
FontRequestKey
    -> AppFace / SystemFaceLocator / FallbackPlan / Missing
```

Key 至少包含：

```text
app font manifest/runtime revision
system font revision (Native)
family/style/weight/stretch
language/script
variation/presentation information when relevant
```

支持 negative caching，避免不存在的 family 重复调用 OS。

但是：

```text
steady glyph draw
```

永远不访问 Resolve Cache。

解析结束后使用 typed：

```text
FontFaceId
```

---

# 9. Font Face Cache：byte-budgeted SLRU

## 9.1 为什么是 SLRU

Font Face Cache 使用：

```text
Segmented LRU
    Probation
    Protected
```

流程：

```text
new/cold face
    ↓
Probation
    ↓ second meaningful reuse
Protected
    ↓ protected over target
Demote to Probation
    ↓ total budget exceeded
Evict cold Probation entries
```

它优于最简单 LRU 的主要原因是抗 scan pollution。

Font picker 连续预览 500 个字体时：

```text
500 one-shot faces -> Probation
hot App UI / CJK / Emoji faces -> Protected
```

一次性字体不应把长期使用的字体全部挤出 cache。

## 9.2 按 bytes 计费

禁止：

```text
max_fonts = 64
```

应该使用：

```text
font_face_cache_bytes
```

entry cost 估算包括：

```text
owned decoded font bytes
parsed table allocations
shaping face state
variation state
coverage accelerator
retained platform resources
```

CJK、variable、color font 成本远大于普通 Latin face，因此 entry count 不能代表资源成本。

## 9.3 Recency 不允许逐 glyph 更新

Font SLRU 的 touch 粒度是：

```text
face used by a newly shaped run / paragraph epoch
```

而不是：

```text
every glyph draw
```

同一 frame 一个 face 被使用 10,000 次，只允许把 recency touch 合并成一次 epoch update。实现可使用 `last_touched_epoch` 或 owner-thread `used_face_set`，禁止每 glyph/instance 修改 SLRU 双向链表。

## 9.4 Pinned hot faces

以下 face 可以被 runtime 临时 pin：

```text
current default UI face
currently visible/in-flight shaping face
faces referenced by in-flight GPU/raster jobs
explicitly pinned document face
```

Pin 不是永久常驻 API。退出相关 scope 后必须恢复可淘汰状态。

---

# 10. Coverage Accelerator

已加载 FontFace 可以构建 compact coverage accelerator，用于回答：

```text
这个 face 能否覆盖当前 shaping cluster/run？
```

它来源于当前 face 的 cmap/variation 信息，而不是系统全局 FontDB。

要求：

- lazy build；
- cost 计入 Face Cache；
- 支持 Unicode plane/range；
- 对 emoji/ZWJ complex cluster，单纯 codepoint coverage 只作为候选过滤，最终仍必须经过 shaper/cluster validation；
- 不使用固定 1.1M Unicode scalar 大表作为所有 face 的强制常驻布局；实现可用 compressed ranges/page index 等结构。

---

# 11. Shaping Cache

Shaping Cache 与 Face SLRU 独立。

Key 至少包含：

```text
FontFaceId + FontRevision
text/content revision or verified hash
script/language/direction
features
variation coordinates
size/scale fields that change shaping
```

规则：

- text/font/features 未变不 reshape；
- paragraph-local shaped runs 优先复用；
- global shaping cache 有 byte budget；
- 长篇一次性日志不能无界污染 global cache；
- visible/reused runs 可以进入 protected segment；
- cache lifetime 不与 Face SLRU 链表机械绑定；
- FontRevision 改变时只失效引用该 revision 的 runs。

---

# 12. Glyph Representation：Quality/Performance-first Adaptive Pipeline

Viso 1.0 不把某一种 glyph image 当成所有场景的万能表示。目标不是减少实现分支，而是让**每一种屏幕行为走质量和成本最合适的表示**。

最终内部 representation 分为四条 lane：

```text
Stable small/medium UI / CJK / editor / document
    -> MaskA8 exact Coverage

Continuous zoom / scale animation / rotation / world-space text
    -> ScalableMtsdf

Extreme scale / high-precision vector / very large transformed text
    -> OutlineVector (retained path/mesh)

Color Emoji / Color Glyph
    -> source-aware ColorGlyph
```

概念内部类型：

```rust
enum GlyphImageKind {
    MaskA8,
    ScalableMtsdf,
    OutlineVector,
    ColorRgba8,
    ColorVector,
}
```

这些是 `viso-text` / `viso-render` 内部合同，不是 DSL 关键字，也不要求普通 App 手工选择。普通作者只看到正确、稳定的文字结果；Runtime 根据 retained metadata、transform history、device scale、glyph source capability 与资源预算做选择。

## 12.1 稳定文字优先精确 A8 Coverage

普通 App 绝大多数文字在一段稳定时间内具有确定的字号、device scale 和 transform。对这类文字，针对最终 device-pixel bucket 直接 raster 出 Coverage 仍是默认最优路径：

```text
小字号边缘最清晰
复杂 CJK 笔画最容易保持锐利
R8/A8 atlas footprint 最小
fragment shader 最简单
GPU texture bandwidth 最低
不支付 distance-field generation/reconstruction 成本
```

默认使用 `MaskA8`：

```text
Label / Button / Menu
TextInput
Code Editor
Document / Chat
普通 heading/body text
CJK UI / CJK long text
稳定字号的 Native / WASM Canvas UI
```

Coverage key 至少包含真正影响像素结果的 raster bucket：

```text
FontFaceId / FontRevision / GlyphId
logical font size
actual device scale bucket
subpixel phase when enabled
hinting/raster policy revision
variation coordinates
```

Viso 可以对小字号启用**灰度 Coverage 的 subpixel-position phase quantization**，以减少亚像素移动导致的跳动；这不是 RGB/LCD subpixel AA。是否启用以及 phase 数量由 backend/reference-device benchmark 决定，高 DPI 下可以减少或关闭 phase bucket。

禁止为了减少 bucket 数量而把低分辨率 A8 glyph 长期放大。稳定 transform 改变到新的有效像素尺寸后，应异步准备新的精确 Coverage bucket。

## 12.2 可缩放 lane 采用 MTSDF，而不是普通 SDF

连续缩放、旋转和 world-space text 的问题不是最终静态质量，而是 Coverage 在动画期间不断跨 raster bucket 会造成：

```text
re-raster
atlas allocation
GPU upload
frame-time spikes
```

Viso 的可缩放 representation 采用 `ScalableMtsdf`：

```text
RGB   -> multi-channel signed distance (sharp-corner reconstruction)
Alpha -> true signed distance
```

MTSDF 同时保留 MSDF 对尖角/serif/复杂 outline 的优势，并提供 true-distance alpha，用于 outline/glow/soft effect 与距离相关质量修正。Viso 1.0 不建立普通单通道 SDF 作为另一条常规 lane。

MTSDF 数据必须按**线性值**解释；不能把距离通道当成 sRGB 颜色纹理采样。

典型使用：

```text
canvas / diagram editor zoom
game/editor world labels
持续 font-scale animation
rotation-heavy transformed text
缩放手势期间的标题/节点标签
```

## 12.3 Temporal Promotion：动画时 MTSDF，稳定后回到精确 Coverage

这是 Viso 字体渲染与“永久 distance-field-first”方案最重要的区别。

```text
Stable state
    ↓
exact MaskA8 Coverage
    ↓
sustained scale/rotation demand detected
    ↓
queue MTSDF generation on worker
    ↓
continue last-good Coverage while pending
    ↓
MTSDF ready + revision validation
    ↓ frame boundary
switch run/group to ScalableMtsdf
    ↓
continuous animation / zoom / rotation
    ↓
transform settles in a stable raster bucket
    ↓
queue exact Coverage raster for settled size
    ↓
continue MTSDF while pending
    ↓ Coverage ready + generation validation
frame-boundary switch back to MaskA8
    ↓
cold MTSDF residency becomes evictable
```

结果：

- 动画期间避免反复 raster/upload；
- 动画结束后重新得到目标 DPI/字号上的最佳小字清晰度；
- MTSDF 不必永久占用所有普通文字的多通道 atlas；
- 同一 glyph 允许在**过渡窗口**短暂拥有两种 representation，但不永久 pin 双份数据。

## 12.4 Promotion / settle policy 必须有 hysteresis

不能：

```text
1.00x -> Coverage
1.01x -> MTSDF
0.99x -> Coverage
```

Representation 状态挂在 retained run/group metadata 上，按事件和历史变化，不按每 glyph 每 frame 重算。

重新评估条件可以包括：

```text
font/glyph revision changed
sustained scale velocity / scale variance
rotation or non-axis-aligned transform persists
raster bucket churn rate
world-space/canvas hint
settled transform duration
MTSDF quality window exhausted
memory pressure
backend/device recreation
```

不应触发 promotion 的普通操作：

```text
纯平移 scroll
opacity animation
text color change
clip change
same-bucket subpixel movement
refresh rate change itself
```

具体 scale/时间阈值不是 public ABI；由 benchmark/profile 校准并保留 hysteresis。

## 12.5 MTSDF 也不能无限缩放：使用 quality window + 多分辨率 bucket

Distance field 有有限纹理分辨率和 distance range。Viso 禁止生成一个很小的 MTSDF glyph 后无限放大。

每个 MTSDF entry 必须记录：

```text
source texel resolution / px-per-em
distance range
validated min/max effective scale
Font/Glyph revision
generator/error-correction revision
```

当 zoom 超出当前 quality window：

```text
keep current last-good representation
    ↓
async generate higher/lower MTSDF bucket
    ↓
validate
    ↓
frame-boundary swap
```

这样大范围 zoom 不需要每一档都 Coverage raster，同时也不会为了复用 atlas 牺牲清晰度。

## 12.6 极端缩放/高精度场景使用 Retained OutlineVector

对于非常大的文字、极端 zoom、几何编辑、精确截图/导出或 MTSDF quality window 不再经济的场景，Viso 可以直接保留 glyph outline 的 vector/path/mesh representation：

```text
Font outline
    ↓
Retained Outline Path
    ↓
backend tessellation / analytic path / cached mesh
    ↓
GPU
```

`OutlineVector` 不是普通 UI 默认路径，因为小字号时 Coverage 更清晰且更便宜；它只用于：

```text
very large effective pixel size
extreme zoom
high-precision vector canvas
export/screenshot fidelity path
transform regime where MTSDF would require excessive atlas resolution
```

Vector cache 独立按 bytes 预算，不与 A8/MTSDF Atlas 共用一个 LRU。Retained mesh/path 可以跨帧复用；禁止在 120/144/240Hz 每帧重新 tessellate 相同 glyph。

## 12.7 CJK 的 representation policy

CJK 正常 UI/编辑器/文档仍默认 `MaskA8`，因为复杂笔画在小字号下 exact Coverage 的质量/内存/fragment 成本更好。

CJK 只有在真实的持续变换需求下才 promotion：

```text
CJK stable UI      -> MaskA8
CJK zoom gesture   -> lazy MTSDF for visible/near-visible glyph set
CJK zoom settled   -> exact Coverage at settled bucket
CJK extreme zoom   -> MTSDF higher bucket or OutlineVector
```

MTSDF generation 对复杂 CJK glyph 较贵，因此必须：

```text
visible-first
near-viewport prefetch
worker generation
request deduplication
no whole-font pre-generation
```

绝不因为一个 CJK font 被加载就生成整套 CJK MTSDF atlas。

## 12.8 Color Glyph / Emoji 采用 source-aware lane

Color glyph 不强行压入 A8/MTSDF contract。至少区分：

```text
Bitmap/strike color glyph
    -> ColorRgba8 at selected strike/raster bucket

Vector color glyph (for example supported COLR/SVG path)
    -> stable size: rasterized RGBA when cheaper
    -> sustained/extreme transform: retained ColorVector when backend supports it
```

Native 系统 Emoji 允许平台 adapter 直接提供高质量 color glyph raster；packaged/WASM color font 通过 Viso font primitive 解码。Emoji ZWJ/VS/skin-tone cluster 在 shaping/fallback 阶段保持原子性，representation 层不得重新拆 cluster。

Color atlas/Vector cache 拥有独立预算，避免 Emoji workload 挤掉普通 UI/CJK。

## 12.9 Representation-specific residency pools

GPU/renderer 至少区分：

```text
A8 Coverage Pages          R8/A8-like mask storage
MTSDF Pages                linear multi-channel distance storage
RGBA Color Pages           color glyph raster
Vector Glyph Cache         retained path/mesh GPU resources
```

`GlyphKey`：

```text
FontFaceId
FontRevision
GlyphId
variation revision
representation kind
coverage raster bucket / subpixel phase
MTSDF bucket + distance-range/generator revision
color source/raster revision
```

不能让：

```text
大量 Emoji RGBA       -> 挤掉普通 UI A8
偶发 Canvas MTSDF     -> 挤掉 CJK/UI hot set
大量 CJK Coverage     -> 迫使 color glyph 重建
vector mesh workload  -> 占满 glyph texture atlas
```

各 pool 的预算可以在统一 TextMemoryController 下协调，但 eviction queue / residency accounting 必须分开。

## 12.10 Atlas eviction：page-age + CLOCK，不逐 glyph LRU

A8、MTSDF、RGBA atlas 都使用 page-level residency：

```text
need atlas space
    ↓
select representation pool
    ↓
CLOCK / second-chance over pages
    ↓
choose cold non-inflight page
    ↓
page generation++
    ↓
invalidate only entries on that page
    ↓
reuse page
```

禁止正常压力下：

```text
atlas full -> clear whole atlas
```

每个 page 的 `last_used_epoch` 一帧最多更新一次。Renderer 只记录本帧命中的 page set/bitset，在 frame cleanup/submit 边界批量刷新；不能每 glyph draw 修改 LRU/CLOCK 元数据。

## 12.11 Quality fallback 与失败策略

MTSDF/Vector 是优化表示，不是文字正确性的单点依赖。

如果：

```text
MTSDF generator detects invalid/low-confidence shape
worker job exceeds deadline
vector backend unsupported
memory pressure evicts scalable representation
```

Runtime 必须保留正确 fallback：

```text
exact Coverage at suitable bucket
```

对于正在动画中的场景，如果新 scalable representation 尚未 ready，可以短暂继续使用现有 Coverage/MTSDF last-good，但不能阻塞 UI thread 等待生成。

## 12.12 高刷性能合同

在 retained representation 已就绪后，120/144/240Hz 的文字绘制热路径不得执行：

```text
representation policy recomputation per glyph
Coverage raster
MTSDF generation
outline tessellation
font fallback
shaping
atlas allocation per glyph
per-glyph cache recency mutation
heap allocation / lock per glyph
```

热路径应接近：

```text
Retained GlyphRun
    ↓
resolved representation handles
    ↓
validated AtlasEntry / VectorHandle generations
    ↓
persistent/reused GPU instance ranges
    ↓
GPU batch
```

## 12.13 性能与质量最终取舍

```text
MaskA8 Coverage
    best stable small/medium text quality
    lowest atlas bandwidth/footprint
    ideal for UI/CJK/editor/document

MTSDF
    amortizes continuous zoom/scale/rotation
    preserves sharp corners better than monochrome SDF
    true-distance alpha supports scalable effects
    multi-channel memory only paid for transformed hot set

OutlineVector
    exact geometry for extreme size/precision
    no finite distance-field quality range
    retained/cached; never ordinary small-text path

Color Glyph
    source-aware RGBA/vector path
    independent residency budget
```

最终原则：

> **静态时追求目标像素上的精确 Coverage；运动时用 MTSDF 把重复 raster/upload 摊平；极端放大时切到 retained Vector；稳定后再回到精确 Coverage。效果和帧稳定性优先，资源只为当前真正需要的 representation 付费。**

# 13. 高刷新率：60 / 120 / 144 / 240 Hz

## 13.1 Frame budget

```text
60 Hz    16.67 ms
120 Hz    8.33 ms
144 Hz    6.94 ms
240 Hz    4.17 ms
```

Viso 不能假设 display 永远固定 60/120Hz。ProMotion/VRR/Adaptive-Sync 下 runtime 使用当前 scheduler/vsync 提供的实际 frame interval 计算 TextWork commit budget；刷新率升高时自动缩小每帧允许提交的非关键 font work，而不是继续使用固定毫秒预算。

Viso Text Runtime 的目标不是“让 shaping 快到可以每帧做”，而是：

> **正常帧根本不做 shaping。**

## 13.2 稳态文字帧合同

没有 text/font/layout 变化时：

```text
0 system font resolver calls
0 font file IO
0 WOFF2 decode
0 OpenType parse
0 new coverage build
0 shaping
0 glyph rasterization
0 MTSDF generation
0 representation-policy recompute per glyph
0 Face SLRU touch per glyph
0 atlas LRU mutation per glyph
0 heap allocation per glyph
0 mutex/RwLock per glyph
```

稳态路径应接近：

```text
Retained Paragraph
    ↓
Retained ShapedRuns
    ↓
stable GlyphImageKind / representation metadata
    ↓
validated AtlasEntry generations
    ↓
existing GlyphRun/instances
    ↓
GPU batch
```

## 13.3 高刷滚动

滚动已有 paragraph 时：

```text
scroll offset / transform changes
    ↓
clip/transform update
    ↓
reuse line layout
reuse shaping
reuse atlas
reuse glyph instance ranges where possible
```

不能因为 120/144/240Hz scroll：

```text
reshape all visible text
reraster all glyphs
rebuild font fallback
```

VirtualList/CodeEditor 进入 viewport 的新行可以产生 text work，但必须配合：

```text
viewport prefetch margin
visible-first priority
near-viewport second priority
background third priority
```

在新行真正进入屏幕前尽可能完成 shaping/raster。

## 13.4 Text Work Scheduler

Text cold work 分优先级：

```text
CriticalVisible
NearViewport
InteractiveEdit
BackgroundPrewarm
CatalogPreview
```

可后台执行的昂贵工作：

```text
WOFF2 decode
large font parse
coverage accelerator construction
font catalog enumeration
large paragraph shaping
MTSDF generation / scalable-representation promotion
retained outline tessellation / vector promotion
color glyph decode/raster where backend permits
glyph rasterization where thread-safe
```

主线程只接收 staged result，并在安全 phase commit。

推荐主线程 font maintenance budget：

```text
min(500 us, 5% of current frame interval)
```

即默认大致：

```text
60Hz   <= 500 us
120Hz  <= 416 us
144Hz  <= 347 us
240Hz  <= 208 us
```

这是 Dev/benchmark 的默认调度目标，不是 public ABI；设备 benchmark 可以调整。

超过 budget 的非关键 atlas upload/cache maintenance 延后，而不是吞掉整个 frame。

## 13.5 输入文字

TextInput 不能因为异步 worker 导致明显的键盘延迟。

已加载 face 的短局部编辑允许：

```text
incremental segmentation
incremental shape dirty run only
caret/selection update
```

在当前 UI phase 同步完成。

以下情况切到 async/staged：

```text
新字体首次加载
远程字体
WOFF2 decode
大 paragraph 全量重排
新的复杂 system fallback cold path
```

目标是普通键盘输入在一个 display frame 内出现，同时不让一个大型字体 miss 阻塞 UI thread。

---

# 14. 第一次字体命中与 Prewarm

Lazy load 降低启动资源，但不能把 cold miss 卡顿转移到第一个可见 frame。

## 14.1 Native System UI face

Native 默认 UI font 是高概率第一屏依赖。

允许 runtime 在启动/首窗口创建的 warm phase：

```text
resolve exactly SystemUi
load minimal required face state
insert/pin in Protected Face Cache
```

这是“预热一个默认 face”，不是系统字体扫描。

## 14.2 Packaged fonts

Build 可以生成：

```text
FontWarmSet
```

仅包含：

```text
app/theme default family
initial route statically-known critical families
```

WarmSet 可异步预取/解析。

禁止把整个 `assets/fonts/` 变成 preload list。

## 14.3 CJK / Emoji prewarm

Native 不需要打包 CJK/Emoji。

如果 initial route 静态/运行时已经出现：

```text
Han
Kana
Hangul
Emoji
```

可以在 first-layout preparation 阶段触发相应 OS fallback run resolve，从而在真正提交 glyph 前得到 fallback face。

但不能为了“可能以后有中文”在启动时枚举/加载所有系统 CJK 字体。

---

# 15. WASM / Canvas

## 15.1 零系统字体

WASM/Canvas runtime 永远没有隐式：

```text
SystemFontResolver
CSS system-ui
browser local font enumeration
OS installed font access
Viso bundled default font
```

## 15.2 Packaged fonts 仍然自动登记

如果项目有：

```text
assets/fonts/Inter.woff2
```

build 仍然生成 FontManifest。

WASM 启动：

```text
Manifest exists
Decoded Face Cache empty
font bytes not necessarily fetched yet
```

第一次真正使用：

```text
manifest lookup
    ↓
lazy fetch packaged asset
    ↓
WOFF2 decode on worker
    ↓
Font Face SLRU
```

所以“WASM 默认没有字体”的准确含义是：

> **框架和系统不给你字体；项目显式携带的 font asset 仍然是项目资源，并且 build-time 自动登记、runtime lazy 获取。**

## 15.3 External FontProvider

没有 packaged font，或者文档需要动态字体时，可以注入：

```text
HTTP/CDN
host JavaScript bridge
plugin
in-memory stream
remote subset service
```

远程按需字体不能强制“一字符一个 HTTP”。Runtime 必须做：

```text
deduplicate
batch
subset
priority
cancellation
byte budget
revision validation
```

---

# 16. Progressive / Remote Font

按需字符描述的是 coverage granularity，而不是网络请求 granularity。

请求单位可以是：

```text
cluster
small character set
script page
font subset
full face
```

必须保留 shaping 所需数据：

```text
cmap
metrics
GSUB
GPOS
GDEF
variation axes
script/language features
```

新 subset 到达：

```text
FontRevision
    ↓
affected shaping runs only
    ↓
affected paragraphs only
    ↓
MEASURE/LAYOUT if metrics changed
PAINT only if geometry unchanged
```

不允许全 App text cache flush。

---

# 17. Cache 生命周期必须独立

非常重要：

```text
Face SLRU eviction
```

不等于：

```text
clear shaping cache
clear paragraph layout
clear glyph atlas
```

如果一个已布局 paragraph 仍拥有完整 shaped result，并且 glyph 仍 resident：

```text
它可以继续渲染
```

只有未来需要：

```text
reshape
new glyph raster
```

才需要重新 lazy load face。

这种 independent cache lifetime 可以显著减少 memory pressure 后的 frame spike。

---

# 18. 内存预算

不要用固定 “64 fonts / 4096 glyphs”。

Text memory budget 分开：

```text
Resolve Cache
Face Cache decoded/parsed bytes
Global Shaping Cache
Paragraph-local cache
A8 Coverage Atlas GPU bytes
MTSDF Atlas GPU bytes
RGBA Color Atlas GPU bytes
Retained Vector Glyph CPU/GPU bytes
font staging/decode/MTSDF temporary bytes
```

建议 runtime 只有少量 memory class：

```text
Compact
Standard
Large
```

具体 MiB 由 reference-device benchmark 决定，不进入 public ABI。

策略顺序：

```text
memory pressure
    ↓
1. drop negative/old resolve entries
2. shrink global shaping cold entries
3. evict Face SLRU Probation
4. demote/shrink Face Protected
5. evict cold A8/RGBA atlas pages not in flight
6. keep visible/in-flight/pinned state
```

临时 WOFF2 decode buffer 在 face publish 后尽快释放。

Native 可 mmap/read-only reference 的 TTF/OTF 应避免无意义 full copy。

---

# 19. Resource / CPU / GPU 性能原则

## CPU

稳态：

```text
complexity proportional to changed/entering text runs
not total application glyph count
```

## Memory

```text
only actually-used FontFace enters SLRU
only reusable/visible shaping stays cached
only resident glyphs consume atlas pages
```

## GPU

```text
batch glyph instances
persistent atlas pages
partial uploads
no atlas full reset
no per-frame glyph texture recreation
```

## IO

```text
no startup system-font directory scan
packaged fonts lazy read/fetch
remote fonts request only when needed
```

---

# 20. System font revision

系统字体集合可能变化。

Viso 不轮询字体目录。

允许：

```text
OS font-change notification where available
lifecycle resume validation
explicit font catalog refresh
platform font resolver revision update
```

Revision 变化：

```text
invalidate system Resolve/FallbackPlan Cache
```

不必立即清掉仍可用的 loaded FontFace、shaping、atlas；只有 resolver identity/face validity 确认失效时才逐级处理。

---

# 21. Threading

`FontResolver` 与 cache owner 不允许把 mutex 带进每 glyph draw。

推荐：

```text
UI/Text owner thread
    Resolve metadata / paragraph state / cache commit

Worker jobs
    WOFF2 decode
    heavy parse
    large shaping
    coverage build
    raster

Render/GPU
    atlas resource allocation
    upload
    residency generation
```

Worker 结果：

```text
stage
    ↓
validate revision/generation
    ↓
commit in Text/Frame phase
```

旧 async result 不能覆盖已经改变的 text/font request。

Android 等 non-thread-safe matcher object 必须 thread-confined/thread-local，不共享一个全局 matcher 锁给所有 text work。

---

# 22. Hot-path identity

Source/API 可以使用：

```text
family string
```

但是解析后必须 lower 为 typed identity：

```text
FontFamilyId
FontFaceId
FontRevision
GlyphId
AtlasPageId/generation
```

稳态 Paint/Render 禁止：

```text
String family lookup
system font lookup
path lookup
platform font object lookup by string
```

---

# 23. Missing font / glyph policy

至少支持：

```text
FallbackToNextSource
PlaceholderGlyph
InvisibleButMeasured
BlockUntilRequiredFont      explicit workflow only
ErrorInStrictMode
```

Native 默认：

```text
App packaged/dynamic
    ↓
System requested/fallback
    ↓
Placeholder
```

WASM/Canvas 默认：

```text
App packaged/dynamic
    ↓
External provider
    ↓
Placeholder
```

普通 UI 不允许因为远程字体无限阻塞 frame。

---

# 24. Inspector / Profiler

至少记录：

```text
font_manifest_entries
font_resolve_hit/miss
font_resolve_negative_hit
system_font_query_count
system_font_query_us
system_fallback_query_count
system_fallback_mapped_clusters
fallback_plan_hit/miss
font_face_slru_hit/miss
font_face_probation_bytes
font_face_protected_bytes
font_face_promotions/demotions/evictions
font_face_decode_bytes
woff2_decode_us
shaping_hit/miss
shaping_us
glyph_atlas_hit/miss
atlas_a8_bytes
atlas_rgba_bytes
atlas_page_evictions
glyph_raster_count
glyph_raster_us
glyph_upload_bytes
missing_cluster_count
text_main_thread_maintenance_us
text_worker_queue_depth
```

Inspector 某段文字至少显示：

```text
requested family
resolved app/system face
language/script
CJK/emoji fallback chain
fallback mapped run length
FontFaceId + revision
SLRU segment/cost
shaping cache state
atlas page/generation
pending external request
```

---

# 25. Benchmark / Regression Matrix

## Startup

```text
native_no_packaged_fonts_startup
native_3000_system_fonts_startup
native_system_ui_first_resolve
wasm_zero_font_startup
wasm_manifest_only_startup
```

`native_3000_system_fonts_startup` 的目标是证明启动时间不会随着系统字体文件数量线性增长。

## Resolution / fallback

```text
latin_system_font_cold/warm
cjk_10k_chars_cold/warm
cjk_mixed_zh_ja_ko
emoji_zwj_1000_clusters
mixed_latin_cjk_emoji
missing_family_negative_cache
```

## Face Cache

```text
face_slru_hot_set
font_picker_500_faces_scan
large_cjk_face_pressure
variable_font_pressure
color_emoji_face_pressure
```

## Shaping

```text
static_paragraph_warm
text_input_incremental
code_editor_scroll_10k_lines
long_log_scan
bidi_arabic_indic
```

## Glyph Representation / Atlas

```text
atlas_a8_steady_scroll
atlas_mtsdf_zoom_animation
atlas_rgba_emoji_scroll
atlas_page_eviction
atlas_no_full_reset
atlas_device_loss_rebuild
coverage_small_text_quality
coverage_cjk_small_text_quality
mtsdf_corner_quality
mtsdf_quality_window_regeneration
coverage_to_mtsdf_promotion_latency
mtsdf_to_coverage_settle_back_latency
outline_vector_extreme_zoom
representation_transition_no_frame_spike
representation_memory_pressure
```

必须额外覆盖动态场景：

```text
1x -> 8x continuous zoom -> settle
8x -> 1x continuous zoom -> settle
rotation animation -> settle
CJK canvas zoom
large heading scale animation
extreme zoom crossing MTSDF quality window
```

质量/性能断言：

```text
stable small text ends on exact Coverage
sustained transform does not repeatedly reraster Coverage buckets
settle-back does not block the frame
MTSDF bucket regeneration is staged
OutlineVector is retained across steady frames
no permanent Coverage+MTSDF+Vector duplication for cold glyphs
```

## High refresh

在 reference devices 上必须覆盖：

```text
60Hz
120Hz
144Hz
240Hz
```

场景：

```text
static UI
continuous scroll
VirtualList new lines entering viewport
text editor typing
emoji-heavy chat
CJK article
font picker scan
continuous canvas zoom with MTSDF
zoom settle-back to exact Coverage
extreme vector zoom
```

稳态断言至少包括：

```text
system_font_query_count == 0 per steady frame
shaping_count == 0 when text/layout unchanged
glyph_raster_count == 0 when atlas resident
font_face_slru_touch is coalesced per face/epoch
no atlas full reset
no text hot-path heap allocation per glyph
no per-glyph representation-policy recomputation
no MTSDF generation or vector tessellation in steady frame
```

---

# 26. 推荐 `viso-text` 模块

```text
crates/text/src/
├── lib.rs
├── font_request.rs
├── font_manifest.rs
├── font_format.rs
├── resolver.rs
├── app_fonts.rs
├── system_fonts.rs
├── font_provider.rs
├── fallback.rs
├── coverage.rs
├── font_cache.rs
├── progressive.rs
├── shaping.rs
├── bidi.rs
├── line_break.rs
├── paragraph.rs
├── text_work.rs
├── glyph_representation.rs
├── mtsdf.rs
├── outline_cache.rs
└── glyph_cache.rs
```

职责：

```text
font_manifest.rs
    packaged font descriptors / AssetId mapping

font_format.rs
    TTF/OTF/TTC/OTC/WOFF2 container normalization

resolver.rs
    App-first / System-second / External policy

system_fonts.rs
    narrow platform fallback adapter facade

fallback.rs
    cluster/run fallback planning and recent candidate cache

font_cache.rs
    byte-budgeted Face SLRU

text_work.rs
    high-refresh-aware async/prewarm/commit scheduling

glyph_representation.rs
    Coverage/MTSDF/Vector/Color retained state machine + hysteresis

mtsdf.rs
    scalable distance-field generation contract / bucket / quality window

outline_cache.rs
    retained glyph outline/path/mesh cache for extreme scale and precision

glyph_cache.rs
    glyph identity + atlas/vector residency metadata
```

`viso-render` 仍然拥有实际 GPU texture/page/upload machinery。

---

# 27. 实现顺序

## P0 — Native Latin baseline

```text
FontManifest
SystemUi on-demand resolver
FontFaceId
Face SLRU
basic shaping
A8 atlas page cache
```

## P1 — System fallback

```text
run/cluster fallback
CJK locale handling
Emoji cluster handling
FallbackPlan cache
```

## P2 — High refresh

```text
retained shaped runs
page-age/CLOCK atlas
prefetch
TextWork scheduler
120/144/240Hz benchmarks
```

## P3 — Packaged formats

```text
TTF/OTF/TTC/OTC
WOFF2
build-time FontManifest automatic discovery
lazy decode
```

## P4 — WASM

```text
zero system-font runtime
packaged font lazy fetch
External FontProvider
progressive subset
```

## P5 — Tool workloads

```text
async SystemFontCatalog
font picker scan resistance
large CJK/editor workloads
memory-pressure tuning
```

---

# 28. Definition of Done

Viso Text/Font Runtime 只有同时满足下面条件才算达到 1.0 合同：

```text
[ ] Native App 不打包任何字体也能使用系统 UI/CJK/Emoji
[ ] Native startup 不全量扫描系统字体
[ ] 3000 installed fonts 不造成线性 startup parse 成本
[ ] assets/fonts 自动进入 FontManifest，但 runtime 不 eager parse
[ ] WASM 没有隐式系统/框架字体
[ ] WASM packaged WOFF2 可以 lazy fetch/decode
[ ] CJK fallback 以 run/cluster + locale 处理
[ ] Emoji ZWJ/VS/skin-tone cluster 不被错误拆 font
[ ] common fallback face 可以复用，避免逐字符 OS query
[ ] Face Cache 是 byte-budgeted SLRU
[ ] Face SLRU recency 不按每 glyph 更新
[ ] Shaping Cache 有独立预算
[ ] 普通 UI / 小字号 / CJK 默认使用 A8 Coverage
[ ] 连续 zoom/scale/rotation 场景可 lazy promotion 到 MTSDF
[ ] MTSDF generation 不阻塞当前 frame，pending 时继续 last-good Coverage
[ ] transform settle 后可异步生成精确 Coverage 并 frame-boundary switch back
[ ] MTSDF 超出 quality window 时使用新的 MTSDF bucket 或 OutlineVector，而不是无限放大
[ ] 极端 zoom/高精度场景拥有 retained OutlineVector path，且稳态不每帧 tessellate
[ ] 不为每个 glyph 默认永久保存 Coverage + MTSDF + Vector 多份表示
[ ] Viso 1.0 不额外建立普通 SDF lane
[ ] Glyph representation policy 不按每 glyph / 每 frame 重算
[ ] Glyph Atlas page eviction 不逐 glyph LRU
[ ] Atlas 满时不正常执行 whole-atlas reset
[ ] A8 / MTSDF / color RGBA atlas 与 Vector cache 使用独立 residency budget
[ ] memory pressure 不引发全 Text cache 连锁清空
[ ] static text steady frame 不执行 resolve/shape/raster
[ ] 120/144/240Hz benchmark 有独立 regression gate
[ ] TextInput 的普通局部编辑在已加载字体下无需 IO/parse
[ ] Inspector 能解释每一个 fallback 和 cache miss
```

---

# 29. 外部平台事实依据

这些资料只用于确定平台 adapter 能力；Viso public API 与 cache policy 不依赖它们的类型：

1. Apple CoreText `CTFontCreateForString`：根据当前字体的 cascade list，为指定字符串范围选择可编码该范围的最佳替代字体。  
   https://developer.apple.com/documentation/coretext/ctfontcreateforstring

2. Microsoft DirectWrite `IDWriteFontFallback::MapCharacters`：将文本起始范围映射到适合的 fallback font，并返回 mapped length。  
   https://learn.microsoft.com/windows/win32/api/dwrite_2/nf-dwrite_2-idwritefontfallback-mapcharacters

3. Android NDK `AFontMatcher_match`：按 family、locale、style 与 UTF-16 文本匹配系统字体，并返回 font run length；API 29+ 可用。  
   https://developer.android.com/ndk/reference/group/font

4. Fontconfig `FcFontMatch` / `FcFontSort`：从系统配置/cache 中选择匹配字体，并可使用 Unicode coverage 做 fallback 排序。  
   https://fontconfig.pages.freedesktop.org/fontconfig/fontconfig-devel/

5. W3C WOFF File Format 2.0 Recommendation：WOFF2 是 OpenType/TrueType 字体的压缩封装格式，支持 variable、color font 和 font collection。  
   https://www.w3.org/TR/WOFF2/

6. `msdfgen` 当前 API 的 `generateMTSDF`：RGB 保存 multi-channel signed distance，Alpha 保存 true signed distance；Viso 仅参考其 representation semantics，不绑定该库为 runtime hard dependency。  
   https://github.com/Chlumsky/msdfgen

---

# 30. 最终决定摘要

```text
SYSTEM FONT
    no Viso FontDB
    no startup scan
    OS on-demand run/cluster fallback

APP FONT
    assets/fonts build-time auto manifest
    runtime lazy load
    dynamic fonts explicit API

CJK
    locale-aware run fallback
    recent fallback candidate reuse
    no per-character system lookup

EMOJI
    grapheme/ZWJ cluster atomic
    native system emoji by OS fallback
    separate RGBA glyph atlas budget

CACHE
    Resolve Cache: small bounded warm cache
    Face Cache: byte-budgeted SLRU
    Shaping Cache: independent byte budget
    Glyph Atlas: page-age + CLOCK, not glyph LRU

GLYPH REPRESENTATION
    stable UI / small text / CJK -> exact A8 Coverage
    sustained zoom / scale / rotation -> lazy MTSDF
    transform settles -> exact Coverage re-raster + frame-boundary switch-back
    MTSDF quality window exhausted -> new MTSDF bucket or retained OutlineVector
    extreme zoom / high-precision vector -> retained OutlineVector
    color emoji / color glyph -> source-aware RGBA/Vector
    no permanent Coverage+MTSDF+Vector duplication
    no separate ordinary SDF lane

WASM
    no implicit system font
    packaged fonts allowed and auto-manifested
    WOFF2 first-class
    External FontProvider optional

HIGH REFRESH
    steady frame performs no resolve/parse/shape/raster/MTSDF generation/vector tessellation
    retained shaped runs + stable representation + atlas + GPU instances
    async cold work + bounded main-thread commit
    explicit 120/144/240Hz regression tests
```
