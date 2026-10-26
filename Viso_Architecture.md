# Viso 架构设计

> 文档状态：Viso 1.0 Draft / Architecture Specification  
> 目标读者：Viso 核心维护者、Renderer/GPU 工程师、UI/Widget 工程师、DSL/Compiler 工程师、工具链工程师、AI Coding Agent  
> 基线日期：2026-09-04  
> 适用范围：Viso 是独立设计、独立实现的 Rust-native 跨平台 UI/Application Framework。本文只定义 Viso 自身的 API、ABI、运行时、DSL、工具链与项目合同。

---

## 0. 文档目的

本文是一份 **Viso 1.0** 的目标架构设计，用于固定框架的核心语义、依赖边界、运行时数据模型、工具链合同与性能约束。

设计同时优化三个看似冲突、实际上可以兼容的目标：

1. **性能优先**：保持 GPU-first、Rust-native、低开销；稳态热路径以 Data-Oriented、增量更新、低分配、低间接层为核心。
2. **工程化清晰**：crate 依赖方向、目录职责、运行时阶段、数据所有权、平台边界、UI/Render/DSL 边界必须可解释、可测试、可演进。
3. **用户入口极简**：内部可以复杂，但复杂度必须单向下沉。普通应用开发者只需要少量稳定概念。

目标入口固定为：

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

这不是单纯的语法糖，而是架构约束：**内部 crate、平台后端、GPU backend、Viso DSL compiler、runtime 的变化，不应泄露到普通业务代码。**

### 0.1 品牌与源码格式约定

框架正式命名为 **Viso**。Viso 的 public API、ABI、运行时、DSL、CLI、项目结构和发布产物均以 Viso 自身合同为唯一来源。外部项目只可作为设计参考，不进入 Viso 的兼容性语义。

Viso UI/DSL **外部源文件**的唯一规范扩展名统一为 **`.vs`**：

```text
app.vs
theme.vs
features/home/view.vs
```

Viso 的 Rust 集成提供**按语义命名的三个宏入口**：

```rust
// 1. 内联 View Fragment：小组件、测试、示例、局部静态 UI
let content = ui! {
    Column {
        Label { text: "Hello"; }
    }
};

// 2. 外部 .vs View：页面、主题、大组件，获得独立 hot reload
let page = view!("features/home/view.vs");

// 3. 内联完整 Component：适合很小、与 Rust 紧密耦合的组件
component! {
    Counter {
        state count = 0;
        view {
            Column {
                Text { text: format("{}", count); }
            }
        }
    }
}
```

这三个宏**不是三套语言**，而是同一个 Viso compiler 的三个明确 parser entry point：

```text
ui!         -> ViewFragment
component!  -> ComponentDecl
view!(...)  -> external .vs CompilationUnit / exported View
                    ↓
          Name Resolution / Schema
                    ↓
                 Typed HIR
                    ↓
         UI IR / Reactive IR / Shader IR
```

宏层只决定 source origin 与合法的顶层语法范围，不复制 type checker、schema、HIR、binding 或 runtime 语义。外部 `.vs` 可由 watcher 独立增量编译和 hot reload；`ui!` / `component!` 默认随 Rust 增量编译更新。Release 中三者都降低为相同的 typed/AOT IR，Viso runtime 不需要解析 Rust 源码。

`Live` 只描述 live editing / hot reload 能力，不作为 Viso 的语言、crate、目录或文件格式名称。

### 0.2 Viso 1.0 的硬决策

Viso 1.0 将以下地基问题定义为正式架构决策：

1. **Node hot storage：采用 hybrid indexed SoA。** `NodeMeta` 保持紧凑 AoS 以服务树遍历；Layout/Transform/Paint/Interaction 等高频字段使用与 `NodeIndex` 同索引的连续数组；低频/可选数据进入 sparse cold side tables。暂不采用全 ECS/chunked archetype。
2. **`Handle<T>`：只表达 typed identity/capability，不直接暴露跨帧 `&T`/`&mut T`。** 读写必须通过受 context 约束的 query/action/property API，避免绕过 reactive invalidation 和生命周期检查。
3. **Transform：与 Layout、Paint 都使用独立的失效/传播平面。** 三者共享 `NodeId` 和结构语义；transform/scroll/transform animation 只重算 transform table 中各 root 的位移，renderer 按位移差移动 retained draw，不触发布局，不重建 primitive，也不上传 instance（§19.2，ADR 0043）。
4. **自研与依赖必须分级。** Viso 只在决定差异化、热路径和语义所有权的部分强制自研；Unicode/shaping、accessibility bridge、async executor、网络/TLS、媒体 codec 等优先复用成熟实现，并由 Viso 自己掌握 integration/cache/ABI contract。
5. **模块边界必须机器化验证。** crate DAG 由 `cargo xtask check-deps`（`xtask/src/main.rs` 中的 allowlist）在 CI 验证，不再只依赖 AGENTS.md 和 code review；module 级 import 规则检查仍是目标（§10.2）。
6. **Reactive dynamic fallback 必须显式、可计数、可 benchmark。** 编译器能够静态解析的绑定不得静默掉入动态路径。
7. **Backend 采用 Device Class × tier × capability 模型。** 每个 Device Class（Desktop / Mobile / Web / Embedded-GPU / Embedded-CPU，§50A）有自己的 Tier-1 后端，并针对现代主后端优化；兼容后端可以追加，但不得反向把 renderer 设计压成最低公分母。
8. **Viso DSL 使用 `ui!` / `view!` / `component!` 三个语义化 Rust 入口。** `ui!` 解析 View Fragment，`component!` 解析 Component，`view!("...vs")` 编译外部 `.vs`；三者共享同一 schema/HIR/IR/runtime 语义。`.vs` 是唯一 canonical 外部 DSL 文件扩展名。
9. **Identity 分层。** 源码名字使用 compiler-local `NameId`，跨编译稳定身份使用 128-bit `SymbolId`，运行时热路径 lower 为 `PropertyId`/`EventId`/`ComponentTypeId` 等 typed dense ID，实例使用 generational `NodeId`；禁止一个万能 ID 贯穿所有 subsystem。
10. **Viso 自研 Ende。** `viso-ende` 负责内部 Binary/JSON Encode/Decode、wire schema、协议版本、cache/snapshot/tool transport；RON 不属于 Viso；Serde 只作为生态 integration。
11. **Hot Reload 是 Dev-only build capability。** Dev Runtime、patch transport、DevSnapshot endpoint 与 hot-reload-only metadata 只编入 Dev artifact；Release/Shipping 完全不编入这些路径，不能靠运行时配置重新开启，release frame hot path 不承担热更税。
12. **`viso run` 的 desktop host 是隐式且唯一的。** macOS/Windows/Linux 开发统一使用无参数 `viso run`；`viso run ios/android` 只面向 Simulator/Emulator；Android/iOS 开发环境分别由 `viso android` / `viso ios` 管理，不把手机 physical-device debugging 作为 Viso 1.0 开发闭环的一部分。Embedded class 的开发闭环是：在桌面上按 Footprint Profile 仿真，加上远程板卡 Dev session（§50A.7）。
13. **Release 不解释执行 UI 行为。** `ui!` / `component!` / `view!` 都是构建期入口；Release 中模块的每个 chunk（handler、region entry、显示值、computed、effect、state 初始化、函数、闭包、task）都 lower 为生成的 Rust 函数（按 chunk 索引的静态函数表），task 是可恢复状态机。Behavior bytecode 解释器只用于 Dev 热更新 patch、显式 `dynamic` 构造和 sandbox/AI preview，是 `vm` feature（§41，ADR 0041）。
14. **编译器不依赖运行时栈。** `viso-dsl` frontend 只依赖纯数据的 `viso-schema`、`viso-ende` 以及 shader ABI；LSP、`viso check` 与 CI 分析不链接 GPU/platform/runtime。`gpu` 不依赖 `runtime`，`text` 不依赖 `gpu`（§10，ADR 0042）。
15. **资源占用是一等合同。** 每个 Device Class 都有 Footprint Profile，在编译期裁剪能力，并在 CI 中度量二进制体积、静态内存、峰值 heap 与首帧时间。Embedded-GPU（嵌入式 Linux + GLES3/Vulkan）属于 1.0 目标；Embedded-CPU（无 GPU，CPU 光栅 + damage 局部刷新）是 1.0 Tier-2；MCU 不在 1.0 交付，但 core crates 必须保持 MCU-ready（§50A，ADR 0044）。

这些条目属于 Viso 1.0 的架构合同；如果要改变，必须新增/修改 ADR 并附 benchmark 或实现证据。

---

# Part I — 总体判断与设计原则

## 1. Viso 的核心方向

Viso 的产品与技术方向固定为：

- Rust-native application runtime；
- GPU-first 2D/3D rendering；
- 自绘 UI，而不是依赖平台原生控件拼装；
- 跨 macOS / Windows / Linux / iOS / Android / Web，并覆盖嵌入式 Linux（有 GPU 与无 GPU 两类）；
- 资源占用是一等目标：同一套 core 在桌面上做到更低的内存/功耗，也能裁剪进嵌入式设备的内存与 flash 预算；
- `.vs` + Rust 宏入口的一等声明式 authoring layer；
- retained UI tree + targeted invalidation，而不是默认 Virtual DOM；
- Shader 与 UI 深度结合，同时允许高级用户下钻 renderer/GPU；
- Studio / Inspector / Profiler / AI-friendly tooling 从架构第一天进入设计；
- 框架自己掌握关键渲染、Identity、Ende、Math、DSL 与 frame scheduling 合同。

Viso 不以复制某个既有框架的 API 为目标。所有 public surface 必须从 Viso 自身的数据模型、性能目标和跨平台语义推导。

### 1.1 需要主动避免的架构耦合

Viso 从一开始避免以下结构性问题：

- 一个对象同时承担事件、布局、绘制、脚本、动态类型和树存储；
- 使用 `Rc<RefCell<Box<dyn ...>>>` 作为 UI Tree 默认节点模型；
- platform crate 反向拥有 DSL、network、tooling 或 UI 语义；
- UI authoring 必须理解 batching、render pass、shader ABI 才能正确显示；
- runtime 属性依赖字符串查找或全局 `HashMap`；
- 编译器、Studio、CLI 各自复制一套 schema/type/build 逻辑；
- workspace 的物理目录与真实依赖方向长期脱节。

这些问题必须通过 crate DAG、typed ID、phase context、CI architecture check 和 benchmark contract 预防，而不是依赖维护者记忆。

---

## 2. 四条总原则

### 2.1 外部 Declarative，内部 Retained

普通开发者可以用声明式组件和 Viso DSL，但运行时不采用 Virtual DOM / 每次 rebuild + diff 的通用模型。

目标：

```text
Authoring
  Rust component / Viso DSL
          ↓
Static Template + Binding Metadata
          ↓
Retained UI Tree
          ↓
Targeted invalidation
```

状态变化不重建整棵虚拟树，而是精确标记受影响的 property / node / layout / paint。

### 2.2 外部对象化，内部 Data-Oriented

API 层可以有 `Button`、`Window`、`State<T>`、`Route` 等自然概念；热路径内部不要求用同样的对象布局。

例如：

```text
Button API
   ↓
NodeId
   ↓
NodeMeta[]
LayoutData[]
TransformData[]
PaintData[]
InteractionData[]
```

代码组织可以分层，运行时数据必须尽可能扁平、连续、cache-friendly。

### 2.3 开发期 Dynamic，发布期 AOT

开发时允许：

- source map；
- reflection；
- inspector metadata；
- Viso hot reload；
- 动态 schema；
- 可选 scripting VM；
- 结构化错误恢复。

Release 默认走 AOT/预编译结果：

- 不要求运行时 parse `.vs`；
- 不要求属性名字符串查找；
- 不要求保留完整 AST/CST；
- 不要求每帧动态 dependency discovery；
- 可以 strip 调试符号与 inspector metadata；
- 不解释执行 UI 行为：模块的每个 chunk 在构建期生成 Rust 代码，Release 不经解释器执行（ADR 0041）。

Dev 与 Release 执行同一份 Typed HIR / UI IR 语义：Dev 用 VM 解释执行（以支持热更新），Release 生成原生代码。二者的语义一致性由 differential test 保证（同一组 IR fixture 分别跑 VM 与生成代码，逐 transaction 比较 state 与 dirty 输出），而不是靠维护者记忆。

### 2.4 冷路径可以抽象，热路径拒绝抽象税

允许 `dyn Trait` 的典型位置：

- clipboard；
- file picker；
- push notifications；
- platform service；
- optional plugin；
- devtools transport。

不允许因便利而在下列热路径堆叠 `dyn Trait + Rc + RefCell + HashMap + String`：

- layout traversal；
- hit test；
- animation evaluation；
- paint generation；
- text cache lookup 的主路径；
- render batching；
- GPU instance generation；
- large-list scrolling。

一句话总结：

> **代码结构高度分层，运行时数据高度扁平。**

---

## 3. 目标与非目标

### 3.1 目标

Viso 必须做到：

1. 单一稳定 facade：普通用户依赖 `viso` 即可。
2. Rust-first：Rust API 不应是 DSL 的二等适配层。
3. Viso DSL / hot reload 是一等开发体验，但 UI runtime 不依赖 Viso DSL runtime 才能工作。
4. Retained + incremental：状态、布局、绘制增量失效。
5. GPU-first：自动 batching、实例化绘制、资源复用。
6. 平台差异下沉：UI/业务层不直接处理 OS backend 细节。
7. Accessibility 从 Node 模型一开始存在，而非后补。
8. Async 有官方模式，但不把 Tokio/Smol 绑死在核心。
9. Mobile lifecycle / safe area / keyboard / deep link / permissions 有标准模型。
10. Studio/Inspector 只能通过 public/internal-tools API 观察系统，不能反向污染 runtime 核心。
11. CLI / Studio / IDE / CI 复用同一套 compiler、build、inspection 与 packaging service，不复制实现。
12. 性能回归进入 CI，不能只依靠人工感知。
13. 资源占用回归同样进入 CI：每个 Footprint Profile 记录二进制体积、静态内存、峰值 heap、空闲 RSS 与首帧时间（§50A.5）。
14. Damage-driven present：任何 Device Class 上的局部变化都只重绘、只上屏受影响区域（§30.3）。

### 3.2 非目标

Viso 不追求：

- 默认支持任意通用 constraint solver；
- 默认采用 Virtual DOM；
- 默认把整个 UI 编程模型做成动态脚本；
- 为了“插件化”把所有 runtime 接口做成 `dyn Trait`；
- 为了“分层”把每个 module 都拆成独立 crate；
- 一创建页面就生成 5~10 个文件；
- 默认提供一套强制的 Redux/Elm 全局消息架构；
- 为所有平台强制使用同一种底层 GPU abstraction implementation；
- 让用户理解 GPU batching、DrawPass、shader ABI 才能画普通 UI；
- 在 1.0 交付 MCU（bare-metal / RTOS、无 MMU）平台。但 1.0 必须守住 MCU-ready 不变量（§50A.6），避免以后重写 core；
- 支持 GLES2 / WebGL1 级别 GPU：这类设备走 Embedded-CPU 路径。

---

## 4. 性能与工程化同时成立的设计方法

Viso 使用四层性能模型：

```text
┌─────────────────────────────────────────┐
│ Authoring Layer                         │
│ Rust Component / Viso DSL / macros      │
└────────────────────┬────────────────────┘
                     ↓
┌─────────────────────────────────────────┐
│ Reactive Layer                          │
│ State slots / bindings / dirty graph    │
└────────────────────┬────────────────────┘
                     ↓
┌─────────────────────────────────────────┐
│ UI Runtime Layer                        │
│ NodeArena / layout / input / semantics  │
└────────────────────┬────────────────────┘
                     ↓
┌─────────────────────────────────────────┐
│ Render Layer                            │
│ primitives / batches / instances / GPU  │
└─────────────────────────────────────────┘
```

抽象程度从上到下递减。

- Authoring 层优先易用、可读、类型安全；
- Reactive 层优先静态 dependency metadata 和小型 slot；
- UI runtime 优先整数 ID、arena、dirty bit、连续内存；
- Render/GPU 优先分桶、instance buffer、ring buffer、pipeline/cache reuse。

### 4.1 自研 vs 外部依赖：Ownership Ladder

Viso 不以“全部自研”为荣，也不以“尽量依赖现成库”为目标。判据是：**谁必须拥有语义、谁必须控制热路径、谁只是实现细节。**

#### Tier A — Viso 必须拥有

这些能力直接决定 Viso 的性能模型、用户语义或长期差异化，不应由通用第三方抽象反向定义：

- NodeArena / identity / lifecycle；
- reactive invalidation / binding metadata / transaction；
- UI layout contract 与增量算法；
- paint primitive、batching、GPU upload plan；
- `.vs` 与 `ui!` / `view!` / `component!` 的 CST/AST→HIR→UI IR、AOT 与 hot reload 协议；
- Shader IR、Viso shader ABI 与 UI/GPU schema bridge；
- frame scheduler / phase ownership / main-loop integration；
- Viso semantics tree；
- ResourceId / asset lifetime / framework-level profiling counters；
- Footprint Profile、memory budget accounting 与 damage region 计算；
- CPU raster 后端的 primitive 光栅化（与 GPU 路径共享同一 primitive/batch 合同）。
- Identity/Symbol lowering contract：`NameId -> SymbolId -> typed dense runtime ID`；
- `viso-ende` Binary/JSON internal encoding、wire schema、protocol/version contract；
- `viso-math` 基础数值/几何 ABI 与 allocation-free hot math primitives。

#### Tier B — Viso 拥有 integration，优先复用成熟算法

这些领域很重要，但重新发明标准算法通常不能形成足够差异化：

- Unicode segmentation / BiDi / shaping primitive；
- font rasterization primitives；
- image/audio/video media codec；
- accessibility OS bridge；
- platform bindings；
- cryptography / TLS。

Viso 在这些领域拥有的是 **cache policy、数据布局、生命周期、增量接口、性能 instrumentation 与替换边界**。例如 `viso-text` 可以使用成熟 shaping/Unicode crate，但 paragraph cache、glyph atlas、增量 line layout 与 UI invalidation 仍属于 Viso。

Accessibility 的 canonical model 是 Viso 自有 semantics tree；平台 adapter 优先复用 AccessKit 等成熟桥接能力，只在能力或性能不满足时实现平台专用 adapter。仅在辅助技术激活时发布、请求以 `RawEvent::Accessibility`（`AccessRequest`）回流，以及 iOS（UIAccessibility container）与 Web（ARIA DOM mirror）两个 Viso 自有 adapter，见 [ADR 0030](docs/adr/0030-accessibility-os-bridge.md)。platform 边界格式是 Viso 自有的 `SemanticsDelta`（ADR 0042）：`PlatformApp::update_accessibility(window, SemanticsDelta)` 按值推送，delta 只携带新增或已变化的 `AccessNode`（typed `SemanticsId`、`SemanticsRole`、label/description/value、live、level、物理像素 bounds、`SemanticsState`、`AccessActions`、children），外加新树时的 root 与当前 focus；不再可达的节点由接收端丢弃。AccessKit 只出现在 macOS/Windows/Linux/Android adapter 内部：`viso-platform` 的私有模块把 delta 一次性转成 `accesskit::TreeUpdate`，`accesskit` 依赖仅对这些 target 编译，Linux 上只随桌面 backend（`viso-platform` 的 `x11`/`wayland` feature）编译——只开 `kms` 的 embedded build 不链接 AccessKit、AT-SPI 与 D-Bus；iOS 与 Web adapter 直接把 delta 折叠进自有 mirror，不链接 AccessKit。没有 a11y bridge 的 backend（KMS 的 `PlatformCaps::FULLSCREEN`，`accessibility: false`）上 facade 不建 bridge、不生成 delta；semantics tree 本身仍然保留，供测试与 Inspector 使用。

#### Tier C — Adapter / 可替换依赖

- Tokio / Smol / 其他 async executor；
- wgpu reference/experimental backend；
- tracing / serde 等生态集成；
- HTTP client / database / telemetry。

它们必须位于 adapter/service 边界，不拥有 Viso main loop、frame semantics、UI identity 或 renderer architecture。

#### Tier D — 默认不自研

除非 benchmark、平台能力或安全审计给出明确理由，否则不自研：

- 通用 async executor；
- HTTP/TLS stack；
- 通用压缩/图片/音频/视频 media codec；
- Unicode 标准算法；
- 业务数据库。

#### 采用或替换第三方依赖的判据

每个重要依赖 ADR 至少回答：

1. 它是否位于每帧热路径？
2. 它是否决定 Viso 的 public semantics？
3. 是否阻碍 AOT、增量失效或 GPU batching？
4. 是否能通过窄 adapter 隔离？
5. 维护/安全成本与自研成本哪个更高？
6. 是否已有真实 benchmark 证明需要 fork/替换？

没有数据时，优先“复用算法 + 掌握边界”，而不是复制一个完整生态项目进仓库。

---

# Part II — Public API 与用户体验

## 5. 单一 facade crate

业务应用默认只依赖：

```toml
[dependencies]
viso = "1"
```

可选 feature 应少而稳定，例如：

```toml
viso = { version = "1", features = ["mobile", "hot-reload"] }
```

不推荐暴露一个巨大的 feature 笛卡尔积。底层 executor、平台 backend、GPU backend 应尽可能由 target/adapter 自动解析，而不是让普通用户手工组合。

能力裁剪通过 **Footprint Profile**（`desktop` / `mobile` / `web` / `embedded-gpu` / `embedded-cpu`，§50A）完成。profile 由 `Viso.toml` 的 target 段解析成一组固定的 feature 集合，由 CLI 传给 Cargo。用户只选 profile，不手工组合 feature；CI 只验证这几组固定组合。

### 5.1 `viso::prelude::*`

`prelude` 只导出高频、稳定、低歧义概念。建议控制在几十个以内，而不是导出整个 framework。

候选：

```text
Application
AppCx
Component
View
Window
Button
Label
Text
Image
List
Scroll
State
Computed
Event
Task
Route
Theme
Color
Vec2
Rect
Size
Constraints
```

以及必要宏/属性宏：

```text
ui! { ... }          // 当前：viso-ui-macros
component! { ... }   // 当前：viso-ui-macros
view!("...vs")       // 当前：viso-ui-macros
#[component]         // 目标，尚未实现
routes!              // 目标，尚未实现
```

高级模块必须显式进入：

```rust
use viso::gpu::*;
use viso::render::*;
use viso::platform::*;
```

### 5.2 API 稳定层级

建议文档和模块明确三类 API：

```text
Stable      普通应用长期依赖
Advanced    Widget/renderer 扩展作者使用
Internal    Viso 自身，不保证兼容
```

可以通过 module 组织表达，而不是依赖命名约定：

```rust
viso::prelude
viso::ui
viso::render
viso::gpu
viso::internal // 不导出到常规文档首页
```

---

## 6. Application 模型

最小应用：

```rust
use viso::prelude::*;

struct App {
    window: Window,
}

impl Application for App {
    fn new(cx: &mut AppCx) -> Self {
        Self {
            window: Window::new(cx, ui! {
                Label { text: "Hello Viso"; }
            }),
        }
    }
}

fn main() {
    viso::run::<App>();
}
```

也允许 DSL：

```rust
impl Application for App {
    fn new(cx: &mut AppCx) -> Self {
        Self {
            window: Window::from_view(cx, view!("app.vs")),
        }
    }
}
```

### 6.1 `Application` 不强制 Router/Store

小应用不应该被迫包含：

```text
Router
Global Store
Reducer
Effect System
```

这些应为按需能力：

```rust
struct App {
    window: Window,
    router: Router<AppRoute>,
}
```

或：

```rust
struct App {
    window: Window,
    store: Store<AppState>,
}
```

### 6.2 推荐但不强制的应用分层

局部状态：直接 `State<T>`；  
父子交互：typed callback / action；  
跨 feature：domain action / shared store；  
异步：task + typed result；  
导航：typed route。

不要把所有按钮点击都变成全局 `Message`。

---

## 7. Component 模型

用户侧组件应该非常普通。State 属于 Component：读取或写入 State 的 View 是一个 Component 的 `view`（`component!` 或 `.vs`），`ui!` Fragment 是无状态 View，只挂载常量值与它命名的 Rust Component：

```rust
component! {
    Counter {
        state count = 0;
        view {
            Column {
                Text { text: format("{}", count); }
                Button {
                    text: "Add";
                    on click { count += 1; }
                }
            }
        }
    }
}

let page = ui! { Column { Text { text: "Counter"; } Counter {} } };
```

DSL 语法（property binding 用 `property: expr;`，命名节点用 `node name: Type { ... }`，行为赋值用 `=`）以 `Viso_DSL_1.0.md` 为准。

但是 `view()` 的作者体验不代表运行时每次真的构造新树。

宏/编译器应尽量生成：

```text
Static Template
Binding Table
Event Table
State Dependency Table
```

首次 mount 实例化 retained node；之后状态变化只更新 binding。

### 7.1 Component 与 Node 分离

严格区分：

- **Component**：业务状态、行为、生命周期；
- **Template/Element**：声明结构；
- **Node**：运行时 retained tree 中的实体；
- **Paint/Render data**：GPU 相关数据。

禁止重新回到“一个 Widget 对象同时承担所有身份”的模型。

---

# Part III — Workspace、crate 与依赖边界

## 8. 顶层仓库

当前（与根 `Cargo.toml` workspace members 一致）：

```text
viso/
├── Cargo.toml / Cargo.lock / rustfmt.toml
├── AGENTS.md（= CLAUDE.md）
├── Viso_Architecture.md        本文（架构合同主干）
├── Viso_DSL_1.0.md / Viso_Hot_Reload.md / Viso_CLI.md
├── Viso_Rendering.md / Viso_Text_Font_Runtime.md
├── crates/
│   ├── viso/        facade
│   ├── macros/      viso-macros（proc-macro）
│   ├── glyph-a8/    viso-glyph-a8（A8 glyph raster 与 baked page，叶子）
│   ├── ui-macros/   viso-ui-macros（ui! / component! / view!）
│   ├── math/  ende/  handle/
│   ├── runtime/  platform/
│   ├── gpu/  shader/  text/  render/  svg/
│   ├── media/       viso-media（经 OS 播放器播放媒体，叶子，ADR 0049）
│   ├── ui/  widgets/
│   ├── dsl/  lsp/
│   ├── behavior/  view/   viso-behavior（VM）/ viso-view（compiled view 运行时）
│   └── services/
├── extras/charts/   viso-charts（折线/面积/柱状/散点图与 sparkline，选装）
├── tools/project/   viso-project（项目发现/配置/构建身份）
├── tools/package/   viso-package（打包/图标/资源转换）
├── tools/cli/       viso-cli（`viso` 命令：参数、输出、exit code）
├── libs/woff2/      viso-woff2（调用方侧 WOFF2 解压，ADR 0028；libs/base64、libs/lz4 存在但不是 workspace member）
├── examples/        hello_world、01-counter
├── benches/  tests/  docs/adr/
└── xtask/           cargo xtask check-deps 等
```

目标（尚未建立）：`crates/schema/`（viso-schema，ADR 0042）、`integrations/`（tokio/tracing/serde/accesskit 等 adapter）、`extras/` 的其余部分（code-editor/markdown/pdf/browser/map/xr）、`tools/inspector`、`tools/studio`、`tools/packager`、`vendor/`。新增目录或 crate 时必须同步 §9、§10 与 xtask allowlist。

### 8.1 为什么不是几十个 crate

原则：

> **crate 是依赖边界，不是代码整理手段。**

`layout`、`input`、`state`、`style`、`semantics` 先作为 `viso-ui` 内部 module。只有出现以下情况才拆 crate：

- 需要独立复用；
- 需要独立 feature 或 target 编译；
- 需要打破真实 dependency cycle；
- 编译边界/缓存收益明显；
- 需要独立版本或发布；
- 安全/unsafe 边界必须隔离；
- backend 需要可替换且依赖巨大。

### 8.2 避免 `core/common/utils/helpers`

不建议创建：

```text
viso-core
viso-ui-core
viso-render-core
common
utils
helpers
misc
```

如果一个概念无法用职责命名，优先重新思考边界。

`base` 也必须严格控制，不能成为新的垃圾桶。

---

## 9. 目标 crate 职责

### `viso`

Facade。基本不实现热路径逻辑。

负责：

- `run::<App>()`；
- `prelude`；
- feature 聚合；
- public module re-export；
- 版本与 capability 查询。

### `viso-macros`

proc-macro crate，只依赖叶子 `viso-glyph-a8`（构建期 bake 与运行时 raster 共用同一个 rasterizer，宏不链接 text runtime）。当前提供：

- `#[derive(GpuPod)]`：GPU instance/uniform 显式布局（ADR 0001 中称为 `GpuInstance` ABI）；
- `packaged_fonts!`：build-time 字体登记、embedded 字体子集与 baked A8 page（§37.8）。

### `viso-glyph-a8`

叶子 crate：一个 glyph 的精确 A8 coverage 的两个来源——`raster` feature 下的 `rasterize_coverage`（`ttf-parser` outline + `ab_glyph_rasterizer`）与 baked page（"VBA8" 格式的编码、校验与原地读取、整数重采样）。不带 `raster` 时是 `no_std` + `alloc`，只有 reader。`viso-text` 与 `viso-macros` 都依赖它，保证 baked pixel 与运行时 raster 逐字节相同（Text/Font Runtime §3.6）。

目标（尚未实现）：`#[component]`、state/binding metadata 等 derive/attribute 宏。

### `viso-ui-macros`

`ui! { ... }` / `component! { ... }` / `view!("...vs")` 三个 DSL 入口（ADR 0014）。它依赖 `viso-dsl`，复用同一 frontend/HIR/UI IR，并生成 `viso_ui::` builder 代码；不拥有第二套 DSL parser。宏层只负责 Rust token/span、build artifact 与 typed API glue。

### `viso-ende`

Viso-owned Encode/Decode infrastructure：

- Binary wire format；
- JSON diagnostics/tool interchange；
- bounded decoder；
- protocol/schema metadata；
- cache/snapshot encoding；
- Studio/Inspector/Profiler/Hot Reload transport；
- core typed ID 的 canonical wire representation。

不支持 RON；不承担 image/audio/video media codec；不作为 frame 内部数据模型。Serde compatibility 属于目标 `integrations/serde`。

### `viso-math`

Viso-owned allocation-free numeric and geometry foundation。它不是 draw helper，也不是新的 `core/utils`；它定义 UI、Render、Shader interface、Text geometry、Input、Animation、Game 等子系统共享的基础数值/几何语义。

负责：

- `Vec2` / `Vec3` / `Vec4` 与明确精度的向量类型；
- `Mat2` / `Mat3` / `Mat4`；
- quaternion / affine / 2D/3D transform；
- `Point` / `Size` / `Rect` / `Insets`；
- `Ray` / `Plane` / `Aabb` 等基础空间几何；
- dot/cross/normalize/intersection/containment 等 allocation-free primitive；
- target-specific SIMD specialization 的内部实现边界。

不负责：

- UI `Constraints`、layout policy；
- `Color` / color space / premultiplied-alpha 语义；
- tessellation/path flattening/render primitive；
- GPU buffer/uniform/instance ABI；
- shader compiler；
- Node/Resource/Property identity；
- animation timeline 或 game-world 语义。

性能与 ABI 规则：

- hot math operation 不分配堆内存，不使用 `String` / `HashMap` / `Rc` / `Arc` / trait-object virtual dispatch；
- public data layout 不依赖 `usize`，32-bit/64-bit/wasm32 语义一致；
- SIMD 是实现优化，不泄漏为 public API contract；
- Rust math layout 不等于 GPU ABI，上传必须通过 `viso-shader` / `viso-gpu` 已验证的显式布局；
- `viso-math` 不依赖 runtime/platform/gpu/shader/text/render/ui/widgets/dsl/services；
- Ende wire representation 由 `viso-ende` 定义，不直接 dump Rust struct memory。

### `viso-runtime`

App runtime：

- main loop；
- frame scheduler；
- task wakeup；
- timers；
- lifecycle；
- resource lifetime；
- cross-thread mailbox；
- frame phase orchestration。

不负责 Widget 实现。

### `viso-platform`

OS abstraction：

- window/surface；
- pointer/keyboard/IME 原始事件；
- clipboard；
- cursor；
- system appearance；
- lifecycle；
- app activation；
- native handles；
- accessibility bridge（Viso 自有 `SemanticsDelta`，ADR 0030 / ADR 0042）；
- `LoopWaker` 跨线程唤醒（ADR 0032）。

系统字体**不**在 `viso-platform`：`SystemFontProvider` / `ColorGlyphRasterizer` trait 在 `viso-text`，CoreText 实现在 facade（ADR 0026）。原生窗口句柄类型在叶子 crate `viso-handle`。平台实现放在同一 crate 的 `backend/` 目录，必要时再拆独立 backend crate。

### `viso-handle`

叶子 crate，定义两种原生句柄：`RawWindowHandle`——由 `viso-platform` 产生、`viso-gpu` 消费以创建 surface；`ExternalImage`——其他框架写入 GPU 可原地采样内存的画面（macOS / iOS 的 `IoSurface`，BGRA、sRGB 编码），由 `viso-media` 交出、`viso-gpu` 采纳为 texture。生产者与消费者因此无需互相依赖。

### `viso-gpu`

极薄 GPU RHI（backend 由 `viso_gpu::Backend` 按 cfg 静态选择，ADR 0029）。只依赖 `viso-handle`、`viso-macros` 与 `viso-gpu-abi`，不依赖 runtime（ADR 0042）：frame 节奏由 runtime 驱动 gpu，而不是反过来。

- device；
- queue；
- buffer；
- texture（含外部画面：`import_external` 建一个原地采样 `ExternalImage` 的 texture，`adopt_external` 把同一 texture 指向同尺寸的新画面；Metal 以 `newTextureWithDescriptor:iosurface:plane:` 包装 IOSurface，每个 IOSurface 只包装一次（最近 16 个），采样中的 surface 记 use count 使其 pool 不往里写新画面，被替换的在其所在帧完成后才归还；D3D12 接收另一设备写入的共享纹理（`ExternalImage::DxgiShared`：纹理与共享 fence 的 NT handle、fence 值与 `ExternalHolds`），按 holds 缓存最近打开的 16 个，在 GPU 上等 fence 到值后 `CopyResource` 进自建的 sRGB texture（bind group 在创建时就记下了 resource，故复制而非换 resource），持有 hold 及所读的 resource / fence 直到复制所在的帧完成，生产者只写无人持有的槽；Vulkan 接收 Android 的 `AHardwareBuffer`（`ExternalImage::AndroidHardwareBuffer`，带画面在 buffer 中的区域与 holds）：设备支持 `samplerYcbcrConversion` 与 `VK_ANDROID_external_memory_android_hardware_buffer` 时按 holds 缓存最近 16 个导入的 image（外部格式、专用分配），按设备建议的模型/范围/色度位置每种编码建一次 YCbCr 转换、不可变 sampler 与管线，每帧在 upload 命令缓冲里从 foreign 队列取得所有权、以全屏三角形经转换采样画进自建 sRGB texture 的 UNORM 视图（`MUTABLE_FORMAT`），再交还所有权，hold 持有到该帧完成；转换 shader 是后端私有的冻结 SPIR-V；WebGPU 接收 `<video>` 元素（`ExternalImage::WebVideo`，借用其 wasm-bindgen 句柄），以 `copyExternalImageToTexture` 在 GPU 上复制当前画面进自建的 `rgba8unorm-srgb` texture（浏览器按视频的色彩标记做色彩管理，与页面自己显示 `<video>` 一致），复制排在已提交的帧之后，不经 CPU；不支持的 backend 返回 `None` / `false`，调用方回落为复制）；手头的 NV12 平面（`viso_handle::Nv12`：Y 与交错 CbCr 两个平面及各自行距、矩阵 BT.601/709/2020、全/有限范围、色度是否左对齐）经 `write_nv12` 写进 backend 自建的 texture（给定的同尺寸 texture 或新建一个）：Vulkan 把两个平面原样经 staging（`bufferRowLength` 带行距）上传进该 texture 自带的 R8 / R8G8 平面 image，再以全屏三角形用普通 shader 按 push constant 里的矩阵行（含范围与 8 位色度零点）及色度缩放/偏移（奇数尺寸向上取整、左对齐色度偏移四分之一样本）画进 sRGB texture 的 UNORM 视图，都在 upload 命令缓冲里，不依赖 YCbCr 扩展；不支持的 backend 返回 `None`，调用方在 CPU 上转换后复制；Vulkan 还接收线性 NV12 的 dma-buf（`ExternalImage::DmaBuf`：fd、长度、两平面的偏移与行距、矩阵/范围/色度位置与 holds）：设备有 `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` 时把 fd 的副本导入为 transfer-source buffer（按 holds 缓存最近 16 个），从外部队列取得所有权后在 GPU 上按行距拷进平面 image 再同样转换，hold 持有到该帧完成；导入一次失败即视设备不收 dma-buf（有的驱动失败时也关掉了 fd，故不再关它）；
- sampler；
- pipeline；
- bind group；
- command encoder；
- surface；
- fences/sync；
- backend resource handles；
- transform root 的位移表（每个 transform root 一个条目；draw 按所属 root 的位移差经 `shift` uniform 移动，instance 不带 transform id，§19.2）；
- present damage（`present_with_damage(rects)`，映射到 `EGL_KHR_partial_update` / `VK_KHR_incremental_present` / DXGI dirty rects / KMS `FB_DAMAGE_CLIPS`，§30.3）。

Backend 集合：Metal、D3D12、Vulkan、WebGPU、`Gles3`（Embedded-GPU，GLES 3.0+，§48.1）、`CpuRaster`（Embedded-CPU 与无 GPU 环境，由 `CpuRaster` 升级为生产级，§30.4）。shader 编译器与 tooling 只需要 ABI 描述类型（`InstanceSchema`、`ShaderCode`、`BlendMode` 等）；它们由 `viso-gpu-abi` 定义，`viso-gpu` 整体 re-export 并按 backend 实现（ADR 0042）。

### `viso-gpu-abi`

GPU ABI 描述（ADR 0042），叶子 crate：instance layout 与 shader 声明的 `InstanceSchema`、用户 program 的 ABI / code / `CpuProgram` 接口、buffer/texture/sampler/pipeline 描述与格式、typed resource handle。不含任何 backend，`viso-shader` 与 `viso-dev-protocol` 只依赖它。

### `viso-shader`

- Shader IR（`viso_shader::program::Program`，唯一 owner）；
- validation；
- platform codegen；
- reflection；
- instance/uniform schema；
- shader cache key。

只依赖 `viso-gpu-abi` 与 `viso-ende`，不链接 GPU backend（ADR 0042）。

### `viso-text`

- `FontResolver`：App packaged/dynamic font 优先、Native system fallback 其次；
- build-time `FontManifest`：自动登记 `assets/fonts/`，Runtime 仍 lazy load；
- Native system font 按需 query/load，不启动扫描系统字体，不建立 framework-owned FontDB；
- CJK/Emoji 使用 locale-aware、cluster/run-aware 的系统 fallback，不逐字符扫描字体；
- WASM/Canvas 无隐式系统字体；packaged font 与 External FontProvider 均可用；
- TTF/OTF/TTC/OTC 输入归一到统一 FontFace；
- byte-budgeted Segmented LRU Font Face Cache；
- 独立 shaping / paragraph cache；
- BiDi / line breaking；
- glyph raster 与 page-aged/CLOCK atlas residency contract；
- 120/144/240Hz 稳态文本零 resolve/parse/shape/raster 合同；
- font revision/incremental invalidation 与 profiler counters。

完整实现规范见 `Viso_Text_Font_Runtime.md`。

`viso-text` 是纯 CPU crate，不依赖 `viso-gpu`（ADR 0042）。glyph 的 GPU atlas texture/page/upload 归 `viso-render`，text 只产出 raster bitmap 与 residency 决策。shaping / line break / BiDi / paragraph cache 路径保持 MCU-ready（`no_std + alloc` 可编译，§50A.6）；系统字体、worker 调度等 std 能力经 trait 注入。

### `viso-render`

- paint primitive；
- retained paint cache；
- clip/layer；
- batch builder；
- image/glyph atlas management；
- render graph；
- GPU upload plan；
- frame packet；
- damage region（由 changed primitive range 的旧/新 bounds 合并而成，§30.3）。

几何基础类型（`Rect`/`Point`/`Size`/`Affine2`）直接使用 `viso-math`，不另定义同名类型。primitive/batch 前端与 GPU 提交后端分离：GPU backend 和 `CpuRaster` 消费同一份 frame packet。asset 解码：raster 经 `viso-image`，SVG 由 `viso-svg` 解析为规范化文档后在 `viso_render::svg` lower 为 primitive。

详细规范见 `Viso_Rendering.md`。

### `viso-svg`

SVG 输入通道：bytes 一次性解析为规范化 vector 文档（`tree::Tree`），只依赖 `viso-math`，不依赖 renderer；不是逐帧 renderer。lower 为 retained `viso_render::Primitive` 流的一步在 `viso_render::svg`，因此 tooling 校验 SVG asset 不链接 render/gpu。

### `viso-image`

CPU 上的 raster 图像：PNG/APNG、JPEG、GIF、WebP（有损/无损/动画）、AVIF（静图、grid、序列；8/10/12-bit，按 nclx 或 AV1 序列头的 CICP 换算，PQ/HLG 色调映射到 SDR，`clap`/`irot`/`imir`；`avif` feature，纯 Rust AV1 解码，wasm 交给浏览器）、BMP、ICO、QOI asset 解码为 premultiplied RGBA8（header 先行检查尺寸上限，按 EXIF orientation 摆正）；动画（`Animation`）流式逐帧合成到一张画布（region + blend/dispose，或解码器自行合成的整帧），内存只有画布与当前帧，循环次数与帧时长按文件，≤10 ms 的帧按 100 ms 显示；构建期把图像转换为 target 采样的 texel 格式（`convert`：16-bit packed、BC1/BC3/BC7、ETC2、ASTC 4×4/6×6/8×8 的 Viso 自有纯 Rust 编解码器 `codec`，Mitchell 重采样 `resample`，SVG 构建期光栅化 `vector`）与其容器 `texture`（`VTX1`）——解码器同时是不支持该格式的设备的 fallback；headless snapshot 的 straight-alpha 图像及其 PNG 写出与感知比较。renderer 与 CLI 共用；格式词汇来自 leaf `viso-gpu-abi`，不链接任何 backend。

### `viso-media`

经各 target 自带播放器播放媒体（ADR 0049）：`VideoPlayer` 打开文件、URL（平台自行拉取，含 HLS）或内存字节（只读文件的播放器写入临时文件，播放器释放时删除），打开时暂停；在 loop 线程上 `poll` 就绪（尺寸、时长、帧率、有无声音、可否 seek）/ 结束 / 失败 / 等待数据，`frame` 只在画面变化时交出当前应显示的一帧（BGRA 或 RGBA 行，YUV 矩阵、范围与 HDR 映射已由平台或播放器完成；或 `Frame::planes()` 交出的 NV12 平面，交 GPU 转换，`copy_rgba` 时在 CPU 上按其矩阵与范围转换）：画面所在的平台内存以 `Frame::native()` 交出为 `viso_handle::ExternalImage`，`copy_rgba` 才把它映射到 CPU 复制出来（不能映射的纯 GPU 帧由 `Frame::external` 构造，只能被采纳）；play / pause / seek（暂停时也显示目标帧）/ rate / volume / muted / looping。解码、声音输出与音画同步都在平台。macOS / iOS 走 AVFoundation（`AVPlayer` + `AVPlayerItemVideoOutput`，IOSurface 背书的 BGRA pixel buffer）；Windows 走 Media Foundation 的 `IMFMediaEngine`（独占一条 MTA 线程，经自建在高性能 adapter 上的 D3D11 设备硬解；新帧 `TransferVideoFrame` 进至多 4 个 NT handle 共享纹理中无 backend 持有、播放器也未交出的一个，写完 signal 共享 fence 并 flush，交给 D3D12 原地读取；只有暂停时要的画面、或调用方要过行数据之后，才经 staging 纹理读回到循环复用的缓冲；设备不支持共享 fence 时每帧读回；位置无锁共享）；Linux / BSD 走 GStreamer `playbin` + `appsink`（运行时 dlopen，未安装时只是视频报 `Unsupported`；全部调用在 loop 线程且不等待，bus 在 poll 时读取，appsink 先要线性 NV12 的 `memory:DMABuf`（`DMA_DRM` 与旧式 caps，有 libgstallocators / libgstvideo 时），再要内存里的 NV12 / BGRA（解码器原样输出或只重排交错，不在 CPU 上转 RGB），并在 sink pad 的 allocation query 里声明支持 `GstVideoMeta`，上游带 padding 的 buffer 原样交来、按 meta 的偏移与行距读；dma-buf 帧以 `ExternalImage::DmaBuf` 交 GPU 原地读，holds 以 qdata 挂在 GstMemory 上随它释放，backend 仍持有的帧留住其 sample 不还给 pool；backend 不收时映射出平面并改要内存里的 NV12；按 caps 的 `colorimetry` / `chroma-site` 读出矩阵、范围与色度位置（未写时按高度取 BT.601/709、左对齐），平面以 GStreamer 写入的映射内存原地交出、不再复制，交 GPU 转换；调用方复制过一次行数据后 appsink 改要 BGRA 并发 reconfigure 重新协商，之后由 GStreamer 的转换器出行数据，暂停时取 preroll 显示 seek 目标帧，关闭在独立线程完成以免等网络）；Android 走系统 `MediaPlayer`（经 APK 内的 `dev.viso.media.VisoVideo` Java 类驱动，框架把 activity 交给 `attach_android` 一次后才能打开；解码、出声、音画同步、HLS 与保调变速都在系统，报告在 Java 主线程投递进播放器的 channel；画面画进 NDK `AImageReader`（YUV_420_888，GPU 采样 + CPU 读用途，至多 5 张），每张 image 的 hardware buffer 交给 Vulkan 原地转换，每个 buffer 一份 holds，换下的 image 在无人持有后才 delete；只有调用方要复制时才按裁剪区转成 RGBA；内存字节经 direct `ByteBuffer` + `MediaDataSource` 原地读取，不写临时文件）；Web 走页面里隐藏的 `<video>`（浏览器自行拉取、解码、出声；`requestVideoFrameCallback` 报出新呈现的画面后才交出，交给 WebGPU 在 GPU 上复制，backend 不收时才画进 `OffscreenCanvas` 读回 RGBA 行，内存字节经 blob URL，自动播放被拒时保持暂停）；其余 target 暂报 `Unsupported`。播放器报告变化时 facade 立即再跑一帧，让控件拿到时长与位置。扩展提供的实时画面（摄像头、屏幕、通话）实现 `LiveSource`（poll / frame / play / pause），由 `VideoPlayer::live` 或已装的 provider 打开，经同一路径出帧：无时长、位置恒为零、不可 seek、无声音。摄像头（ADR 0050）作为实时视频经同一个 `VideoPlayer` 打开，由扩展经 `provide_cameras` 装入的 `CameraProvider` 应答（先装者生效；未装时打开报 `Unsupported`，`cameras()` 为空）：`VideoSource::Camera(CameraRequest { camera, width, height, frame_rate })`（`CameraChoice::Default` / `Facing` / `Id`，默认 1280×720、30 帧），`cameras()` 列出 `CameraInfo { id, name, facing }`；按请求取摄像头最接近的模式（不小于目标尺寸的最小者，否则最大者；该尺寸下不低于目标帧率的最低者，否则最快者），首帧到达即就绪（无时长、不可 seek、无声音，`VideoInfo::facing` 报朝向），play 即采集，pause 释放设备（画面停在最后一帧），seek / rate / volume 无效；用户拒绝授权报 `VideoError::Denied`。Linux / BSD 由扩展 `viso-camera` 自行 dlopen GStreamer、经 `GstDeviceMonitor` 枚举 `Video/Source`（V4L2、PipeWire、libcamera；朝向取 `api.libcamera.location`，否则按名称判断内置摄像头为前置），设备元素在 READY 下报出的 caps 逐结构 fixate 出模式，同一模式优先原始帧，否则 MJPEG 经 `jpegdec`；源元素与选定的 caps 交给 viso-media 公开的 `gstreamer_live`，管线为 `源 ! capsfilter ! [jpegdec !] videoconvert ! appsink`，复用视频的 NV12 / dma-buf / BGRA 全部路径，暂停置 `NULL` 状态。macOS / iOS 由扩展 `viso-camera` 经 `AVCaptureSession` + `AVCaptureVideoDataOutput` 采集，画面以 `Frame::pixel_buffer`（IOSurface 背书的 BGRA pixel buffer，原地采样、复制时才映射）交出（会话在其串行队列上启停，委托只留最新一个 IOSurface 背书的 BGRA pixel buffer，迟到帧丢弃；按 `activeFormat` 与最小 / 最大帧时长设模式，会话用 `InputPriority` 预设，启动期间锁住设备以免会话改回自己的格式；iOS 把连接转正；未决的授权在打开时请求，拒绝即 `Denied`）。Windows 由扩展 `viso-camera` 在自己的 MTA 线程上经 `MFEnumDeviceSources` 列出视频采集设备（id 为 symbolic link；朝向取设备节点 ACPI 物理位置的面板，否则按名称），play 时以 `MFCreateDeviceSource` 打开、在其媒体类型里选最接近的模式（同一模式优先 NV12、YUY2，MJPEG 最后），经异步 `IMFSourceReader`（viso-media 公开的 `SharedCanvas` 的 D3D11 设备与 DXGI manager、高级视频处理、硬件 MFT）转出 BGRA 纹理，经 `SharedCanvas::draw` 拷进与视频同一种共享纹理槽并 signal 共享 fence，loop 线程上由配对的 `SharedPictures` 取帧；读到的帧经回调投递到线程，不阻塞，pause 关掉媒体源释放设备，隐私设置拒绝（`E_ACCESSDENIED`）报 `Denied`。Android 由扩展 `viso-camera` 自带的 `dev.viso.camera.VisoCamera` Java 类（构建时编成 dex 嵌入，经 `ndk-context` 拿到 VM 与 activity，以 `InMemoryDexClassLoader` 在 activity 的 class loader 下载入）驱动 Camera2（`CameraManager` 列出摄像头、朝向与传感器角度，`YUV_420_888` 的输出尺寸与 AE 帧率范围给出模式；在自己的 HandlerThread 上开设备、建会话、重复 `TEMPLATE_PREVIEW` 请求，帧率取上限等于目标、下限最低的范围），画进 viso-media 公开的 `ImageReader`（与视频同一种 `AImageReader`），hardware buffer 交 Vulkan 原地转换；画面按传感器角度与屏幕旋转（后置相减、前置相加）转正，转数随 `ExternalImage::AndroidHardwareBuffer` 交给 backend 在 Y′CbCr 转换时一并旋转，CPU 复制时按同样转数重排行；未授权时经 activity 请求 `CAMERA`，拒绝即 `Denied`，设备被占用或断开报错。Web 由扩展 `viso-camera` 以 `getUserMedia`（尺寸、帧率与朝向作 ideal 约束，指定设备时 `deviceId` 作 exact 约束）实现 viso-media 公开的 `MediaStreams`，交给 `web_live`：流挂到隐藏的 `<video>`，复用视频的出帧路径，play 时请求、pause 停掉轨道并释放流，朝向取轨道 settings 的 `facingMode`，`NotAllowedError` 报 `Denied`；`cameras()` 读上一次 `enumerateDevices` 的结果并发起下一次（授权前没有名称）。叶子 crate，不依赖框架其他 crate。

### `viso-ui`

- NodeArena；
- component runtime；
- state slots；
- reactive invalidation；
- layout；
- style；
- input/focus/gesture；
- semantics；
- animation；
- paint node bridge；
- Release AOT package loader（`crates/ui/src/aot`，ADR 0016）；package 是 8 字节对齐的 section 布局，`AotView` 在 mmap / XIP 的原地字节上一次校验、逐条解码 node 与 edge，不拷贝（§50A.6 第 5 条）。

Layout / hit test / transform 的几何类型来自 `viso-math`（目标边 `ui -> math`，ADR 0042）。

### `viso-widgets`

官方 UI kit：

- primitive controls；
- layout containers；
- navigation；
- overlays；
- adaptive UI；
- desktop shell；
- theme defaults。

PDF/browser/chart/map 等不是基础 widget，放 optional package/integration，而不是继续平铺进核心 widgets。

### `viso-dsl`

- tokenizer/CST/AST/HIR；
- type checker（schema 来自 `viso-schema` 的纯数据，不来自 `viso-ui` 的 Rust 类型，ADR 0042）；
- module graph；
- UI IR / Binding IR（ADR 0014）、Shader IR（ADR 0017）；
- transactional hot reload 的 compile 侧（`crates/dsl/src/hotreload`：plan → diff → migrate，产出 `viso-dev-protocol` 的 patch 数据，ADR 0015）。commit（修改 retained state）是 apply 侧职责，只在 `viso-view` 的 dev 模块（ADR 0042）；进程内驱动 commit 的只有测试；
- Release AOT emitter（ADR 0016）：UI IR / Binding IR 以 compact asset 输出，handler / binding evaluator / computed 以生成的 Rust 源码输出（ADR 0041）；
- source map；
- Behavior IR → bytecode lowering（`Program::bytecode`，ADR 0035），只服务 Dev 热更新与显式 dynamic；
- Native Package（ADR 0051）：扩展 crate 在 Cargo manifest 中声明的 Native Library，以 `viso-natives` JSON Schema（`viso_behavior::native::schema`，包的测试以 `viso::native::check` 保持同步）读入为 stub；`native_package::link_with` 先按标准 Registry 编译，出现 `E2001` 时经 `cargo metadata` 发现 App crate 直接依赖中的包并重编译，`view!`/`component!`、`viso check`/`export`/`dev`/`schema` 与 LSP 共用。展开把模块调用到的包的 Rust Library 交给 `viso-view` 的 Host，Host、其 Game 与 Release View Package 以 `Natives::shared`（每组 Library 一个进程内共享 Registry）链接。框架不认识任何扩展。网络不是 DSL Library：`.vs` 调用应用 workspace 中 natives crate 的领域 Native，其 Rust 实现使用 `viso-ext/viso-net`。

`viso-dsl` 只依赖 `viso-schema`、`viso-dev-protocol`、`viso-ende`、`viso-behavior`、`viso-shader` 与叶子 `viso-math`（Web 导出按其缓动曲线采样 CSS）；传递依赖闭包不含 `ui`/`render`/`gpu`（backend）/`runtime`/`platform`，这样 LSP、`viso check` 与 CI 分析不链接运行时栈（`check-deps` 的闭包规则执行）。Shader IR 的唯一 owner 是 `viso-shader`：`viso-dsl` 解析、检查 `.vs` 中的 shader 声明（与其余 DSL 共用 CST、resolver 与诊断），lower 为 `viso_shader::program::Program`，validation 与 codegen 由 `viso-shader` 完成。

### `viso-behavior`

Behavior bytecode 格式、verifier 与 interpreter（ADR 0035）：寄存器 VM、8 字节指令、事务 undo log、执行预算与 Runtime Fault（`E7101`–`E7105`）。只依赖 `viso-ende`；`runtime` / `ui` / `widgets` 不得依赖它。

解释器（`interpreter` feature，`viso-view`/`viso` 的 `vm`）用途限定为 Dev 热更新 patch、显式 `dynamic` 构造、sandbox/AI preview。`aot::lower_module` 把模块的每个 chunk lower 为 Rust（task 为可恢复状态机，`TaskFrame` + `Poll`），`Vm::install_native` 按 build hash 安装；生成代码仍在 VM 的运行时外壳上执行——事务与 undo log、预算、fault 位置（报告 bytecode pc）、computed memo、native、事件与 task 启动——所以两条路径的语义只有一份（ADR 0041）。不带解释器的构建中没有编译代码的 chunk 报 `E7105`。

### `viso-view`

Compiled view 的运行时（ADR 0037–0040）：handler table、state layout、control response、task/resource/persist 接线。三种入口（宏、热更新 commit、release package）都经它挂载，因此 handler 的执行语义只有一份。

目标（ADR 0041）：
- Release 下每个 chunk 是生成的 Rust 函数：`view!` 在宏展开时 lower 模块并把代码嵌在模块字节旁（`viso::__behavior!` 在 release 或无 `vm` 的构建中保留代码与去掉 bytecode 的 `Module::compiled()` 字节，debug + `vm` 时只保留完整 bytecode、代码不编译）；view package 带 bytecode，`viso_dsl::aot::package_native_code` 生成代码由 app 编译，经 `ViewLoad::native` 交给 loader，`viso_dsl::aot::compile_package` 给出与之配套、去掉 bytecode 的 package；
- 富类型 state（字符串、列表、记录）没有 UI cell，实例保留自己的 state 向量；镜像到 UI cell 的标量 state 按 `StateStore::epoch` 门控：cell 与实例 revision 都未变时 sync 不拷贝任何 cell，dispatch 对镜像数是 O(1)；
- Dev 下解释执行与 Release 生成代码共享同一 handler table 布局（entry → chunk 索引），热更新只替换 table 条目。

### `viso-schema`（ADR 0042）

DSL compiler 与 runtime 之间交换的**纯数据合同**：widget / property / event / slot 描述与标准 widget 表、`DirtyClass`、state 身份与值、role / interaction / layout / env 词汇、length 项、resource 身份与 asset 格式、`StableId`、值类型 schema 与 `Retyping`、AOT package 与 view package（control / route / region 表）格式。`no_std + alloc`，只依赖 `viso-ende`。`viso-ui` / `viso-view` / `viso-behavior` 在原路径 re-export，`viso-dsl` 消费它。它不是 `core/common`：只放跨编译边界的数据类型及其 codec 与定义其含义的小纯函数，不放运行时逻辑。

### `viso-dev-protocol`（ADR 0042）

`viso`（CLI）与 dev runtime 之间的开发链路：framed host/runtime message 与其携带的 patch bundle（UI / game section）、asset blob、game query，以及 running app 报告给工具的数据——inspection census / node detail / event log / source table / selector、profile frame、headless scenario。两侧都依赖它，CLI 因此不链接 runtime；runtime 侧的采集（census、node detail）留在 `viso-view`。依赖 `viso-schema`、`viso-ende`、`viso-math`、`viso-gpu-abi`。

### `viso-lsp`

`.vs` formatter 与 language server（ADR 0018）：analysis engine + 薄 stdio frontend，只依赖 `viso-dsl`。

### `viso-cli`

`viso` 命令（`Viso_CLI.md` §39–§41）：参数解析、project/config resolution、调用 `viso-project` / `viso-package` / `viso-dsl`、human 输出与 JSON event stream（经 `viso-ende` 的 `JsonWriter`）、exit code；不实现任何 domain service。

CLI 依赖 `viso-dsl`、`viso-dev-protocol`、`viso-schema`、`viso-image`、`viso-svg`、`viso-text`，传递依赖不含 `runtime` / `platform` / GPU backend（`check-deps` 闭包规则）。需要运行 app 的命令（snapshot、headless test）启动构建出的 app artifact 子进程，通过 `viso-dev-protocol` 通信；asset 校验、图标与 snapshot 比较只用 `viso-image` / `viso-svg`；需要栅格化的 build-time 处理（预烘焙 glyph atlas）只使用 `CpuRaster`。

### `viso-services`

核心 app service protocol。当前（ADR 0031 修订）：haptics、launcher，加 `app_data_dir` / `LocalStorage` 供持久化；`Reply<T>` 一次性应答，`Services` 注册表。非 UI 能力（文件对话框、分享、本地通知、安全存储、权限、定位、网络）是 `viso-ext` 插件 crate，各自携带 OS 实现与 mock，共用 `viso-common` 的 `Reply`。

低频 service 可以使用 trait object，避免把所有 OS 逻辑塞回 platform/runtime。

---

## 10. Crate 依赖 DAG

**当前合同**是 `xtask/src/main.rs` 的 `allowed_edges()` allowlist，由 `cargo xtask check-deps` 在 CI 执行；不在表中的 workspace 内部边一律拒绝。本节与该 allowlist 不一致时以 allowlist 为准，并应同步修正本节。

| crate | allowlist（ADR 0042 之后） | 仍待调整 |
|---|---|---|
| `viso`（facade） | runtime, ui, ui-macros, widgets, dsl（dev）, view, behavior, services, render, image, media, dev-protocol, gpu, platform, handle, text, shader, macros, ende, math, schema | `behavior` 的解释器只在 `vm` feature 下编入（ADR 0041） |
| `viso-widgets` | ui, math | — |
| `viso-charts`（extras） | ui, widgets, math；测试另 dev-depend viso | — |
| `viso-ui-macros` | dsl | — |
| `viso-lsp` | dsl | — |
| `viso-cli` | project, package, dsl, dev-protocol, schema, ende, image, svg, text | — |
| `viso-dsl` | schema, dev-protocol, ende, behavior, shader, math；测试另 dev-depend ui、view | — |
| `viso-view` | ui, schema, dev-protocol, behavior, ende | `behavior` 的解释器只在 `vm` feature 下编入（ADR 0041） |
| `viso-behavior` | ende, math, schema | — |
| `viso-services` | runtime, platform | — |
| `viso-ui` | render, runtime, ende, text, math, schema | — |
| `viso-render` | math, schema, text, image, svg, shader, gpu | — |
| `viso-svg` | math | — |
| `viso-image` | schema, gpu-abi, svg, math | — |
| `viso-media` | handle（各 target 的媒体栈） | — |
| `viso-text` | glyph-a8 | — |
| `viso-shader` | gpu-abi, ende | — |
| `viso-gpu` | handle, macros, gpu-abi | — |
| `viso-runtime` | platform | — |
| `viso-platform` | handle | — |
| `viso-dev-protocol` | ende, math, schema, gpu-abi | — |
| `viso-schema` | ende | — |
| `viso-macros` | glyph-a8 | — |
| `viso-gpu-abi` / `viso-math` / `viso-ende` / `viso-handle` / `viso-glyph-a8` / `viso-project` / `viso-package` | —（叶子） | — |

调整的理由：
- `gpu -> runtime` 与 `text -> gpu` 在源码中没有任何引用，属于历史残留边；它们让 runtime 位于 gpu 之下，也让纯 CPU 的 text 被迫链接 GPU backend。
- `dsl -> ui` 让 LSP / `viso check` 传递链接 render、gpu、runtime、platform。改为 `dsl -> schema` 后，编译器与运行时只通过纯数据合同耦合；dev 链路的协议数据单独成 `viso-dev-protocol`，CLI 与 runtime 共用而互不依赖。
- `shader -> gpu` 让编译器链接 GPU backend；ABI 描述拆为 `viso-gpu-abi` 后，shader 只描述 GPU 数据。
- `svg -> render` 让 CLI 为校验 SVG 链接 renderer；SVG 解析与 lowering 拆开后，`render -> svg`，CLI 只用解析器。
- `math` 是几何类型的唯一 owner；ui/widgets 直接使用它，render 删除重复的 `Rect`/`Point`。

`viso-ende` 被 ui/dsl/schema/behavior 使用，不位于 frame 数据流主链。

### 10.1 禁止依赖

以下边无论 allowlist 如何演进都不得出现（与 AGENTS.md §3.5 一致）：

- `platform -> ui / widgets / dsl / Studio`；
- `ui -> widgets`；
- `render -> widgets`；
- `gpu -> ui`；
- `text -> platform`（系统字体经 `viso-text` trait 由 facade 实现，ADR 0026）；
- `ui / runtime -> Studio / tools`；
- `viso-dsl -> concrete widget implementation`：原则上禁止，使用 schema/registry；
- framework core crate 依赖 app/example：绝对禁止；
- `viso-ende` / `viso-math` 依赖任何框架 crate：禁止，二者保持叶子；
- `runtime / ui / widgets -> behavior`：禁止，Pure Rust UI 不依赖 DSL 执行器；
- `viso-math` public ABI 依赖 pointer width、`usize` 或 backend SIMD type：禁止；
- `viso-math` struct 内存布局直接作为 GPU uniform/instance wire ABI：禁止；
- frame hot path 通过 Ende encode/decode 传递 Node/Layout/Paint 数据：禁止；
- `gpu -> runtime / platform`：禁止（ADR 0042）；
- `text -> gpu / render`：禁止；
- `viso-dsl` / `viso-lsp` / `viso-ui-macros` / `viso-cli` 的传递依赖包含 `runtime` / `platform` / `gpu`：禁止（`check-deps` 的传递闭包检查执行）；
- `shader -> gpu`、`svg -> render`、`dsl -> ui / view`、`cli -> view`：禁止（ADR 0042）；
- 同一几何概念在 `viso-math` 之外重复定义 public 类型：禁止；
- Release / embedded profile 的默认 feature 集链接 `viso-behavior`：禁止（显式 dynamic 能力除外）。

### 10.2 边界必须由 CI 机器化验证

AGENTS.md 里的 dependency 规则不是“建议”。`cargo xtask check-deps` 基于各 crate `Cargo.toml` 验证 crate allow/deny edge（dev-dependency 单列，不进入 crate 自身闭包），并对 compiler/tooling 侧（dsl/lsp/ui-macros/cli）计算普通依赖的传递闭包，闭包中出现 runtime/platform/gpu 即失败并给出路径。

目标（尚未实现）：

- profile 规则：`cargo xtask check-profiles` 以 `viso build` 为 embedded 板卡解析的 feature 集（facade 默认 feature 关闭，加 profile 的 feature 与 `vm`；`embedded-gpu` 分 GLES 与 Vulkan 两种板卡）为 aarch64 与 armv7 Linux 解析 facade 的 normal 依赖闭包（不含 proc-macro 子树），检查禁止项：Linux 桌面栈（`x11rb*`、`wayland-*`、`zbus*`、`accesskit*`）、DSL 编译器与 hot reload watcher（`viso-dsl`、`viso-lsp`、`notify*`、`kqueue*`、`inotify*`）、其他平台的图形与系统 API（`windows*`、`objc2*`、`metal`、`wgpu*`）；GLES 板卡另禁 `ash`，Vulkan 板卡另禁 `glow`、`khronos-egl`，`embedded-cpu` 禁一切 GPU API（`ash`、`d3d12`、`glow`、`khronos-egl`）。命中的 crate 连同依赖路径一起报告。`viso-behavior` 是 `no_std` 的必需依赖，不在禁止项内；fontconfig 不是 crate 依赖（运行时 dlopen，embedded 构建经 `cfg(viso_no_system_fonts)` 编译掉，§50A.4）。`no_std` crate 的 `thumbv7em-none-eabihf` 交叉编译 smoke check 由 `cargo xtask mcu-smoke` 执行（§50A.6）；
- framework crate 不得反向依赖 `tools/`、`examples/`、Studio 的自动检查；
- `platform/ui/render/gpu/dsl` 等关键 module 的 forbidden import 扫描（first-party AST/import scanner）；
- target-only dependency 不得泄漏到其他 target；
- public facade re-export 不得绕过稳定性等级。

CI 的最终合同由 Viso 自己控制（xtask），`cargo-modules` 等工具只用于可视化和辅助诊断。

### 10.3 允许的反向通知

当底层需要向上层通知时，使用：

- typed event；
- small callback protocol；
- registered handler ID；
- queue/mailbox；
- compile-time generic；

而不是直接引入上层 crate。

---

## 10.4 Identity & Symbol Architecture

Viso 1.0 不使用一个“万能 ID”贯穿编译器、UI、Shader、资源、事件和运行时实例。不同身份具有不同的稳定性、生命周期和性能要求，必须由不同 Rust 类型表达。

核心原则：

> **Names are for source code. Symbols are for compilation and persistence. Dense typed IDs are for runtime. Generational IDs are for instances.**
>
> **源码使用名字，编译与持久身份使用 Symbol，运行时热路径使用 typed dense ID，实例生命周期使用 generational ID。**

这条规则同时服务三个目标：

1. 源码名字只在编译/链接阶段解析或 intern，运行时热路径不做字符串属性比较；
2. 避免 Property、Event、Shader、Node、Resource、Source 等概念全部退化成同一个整数类型；
3. 让稳定身份和热路径身份分别针对正确性与执行性能优化，而不是强迫一个 ID 同时承担两个目标。

### 10.4.1 ID taxonomy

Viso 1.0 的标准身份类型如下：

| 类型 | 语义 | 建议表示 | 跨编译稳定 | 主要路径 |
|---|---|---:|---:|---|
| `NameId` | 当前编译会话中的 interned identifier | `u32` | 否 | compiler |
| `SymbolId` | 稳定源码/Schema/声明身份 | 128 bit | 是 | compile/link/hot reload |
| `ComponentTypeId` | 当前 runtime image 中的组件类型索引 | `u32` | 否 | hot |
| `PropertyId` | Property Schema 的稠密索引 | `u16/u32` | 否 | hot |
| `EventId` | Typed Event 的稠密索引 | `u16/u32` | 否 | hot |
| `StyleId` | 编译后的 Style 索引 | `u32` | 否 | hot/warm |
| `ShaderId` | runtime shader table 索引 | `u32` | 否 | hot/warm |
| `PipelineId` | GPU pipeline resource handle | generational integer handle | 否 | hot |
| `NodeId` | 具体 Runtime Node 实例身份 | `u32 index + u32 generation` | 否 | hot |
| `StableKey` | 动态集合中业务对象身份 | typed value | 由业务语义决定 | reconciliation |
| `SourceId` | 当前 artifact/source-map 中的源码文件身份 | `u32` | artifact 内 | cold/warm |
| `ResourceId` | asset/resource identity | 独立 typed ID | 按资源合同 | warm |

禁止创建如下通用类型并让所有 subsystem 共用：

```rust
pub struct Id(u64);
pub struct VisoId(u128);
```

类型系统必须能阻止：

```text
PropertyId 被传入 Shader lookup
EventId 被当作 NodeId
SourceId 被当作 ResourceId
SymbolId 被直接当作 Runtime array index
```

### 10.4.2 `NameId`：只属于 compiler interner

源码中的：

```text
Button
text
click
save_button
```

首先进入 compiler interner：

```text
"Button"      -> NameId(21)
"text"        -> NameId(44)
"click"       -> NameId(62)
"save_button" -> NameId(91)
```

建议：

```rust
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NameId(u32);
```

规则：

- `NameId` 只保证当前 compiler/database session 内有效；
- 不写入持久 cache 作为稳定 ABI；
- 不跨进程直接传输；
- 不作为 Runtime Property/Event/Node identity；
- string interning 发生在 parser/name-resolution 冷路径；
- AST/HIR 中优先携带 `NameId`/resolved symbol，而不是重复存储 `String`。

Compiler 可以保留反查：

```text
NameId -> UTF-8 spelling
```

用于 diagnostics、formatter、LSP 和 debug dump。

### 10.4.3 `SymbolId`：稳定语义身份

`SymbolId` 表示“这个语义声明是谁”，而不是“当前进程把它放在数组的第几个位置”。

典型对象：

```text
app::home::HomePage
app::home::HomePage::count
app::home::HomePage::view::save_button
viso::widgets::Button::text
viso::widgets::Button::click
shader::RoundedRect::fragment
```

推荐物理表示：

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymbolId {
    lo: u64,
    hi: u64,
}
```

也可以在内部使用等价的 `[u8; 16]` 表示，但 public/internal ABI 不应把 `SymbolId` 定义成“需要 128 位整数算术”的数值类型。

`SymbolId` 只需要支持：

- equality；
- hashing；
- ordering（需要 deterministic output 时）；
- Encode/Decode；
- debug formatting；
- stable cache key composition。

禁止：

```rust
symbol + 1
symbol * 2
```

#### Stable Symbol 生成输入

Canonical symbol identity 至少由以下信息组成：

```text
package identity
module path
declaration kind
canonical declaration path
explicit stable annotation (optional)
generic arity / schema kind when required
```

例如：

```text
my_app
+ features::home
+ ComponentNode
+ HomePage::view::save_button
```

经过**固定、版本化、跨平台确定**的 128-bit fingerprint 算法生成 `SymbolId`。

硬规则：

- 禁止使用 `std::collections::hash_map::DefaultHasher` 生成持久 Symbol；
- 禁止使用进程随机 seed；
- 禁止把源码 byte offset 作为主要稳定身份；
- 普通格式化不改变 Symbol；
- 文件物理移动但 canonical module path 不变时不应改变 Symbol；
- 显式 stable annotation 可以在受控 rename/move 中保持身份；
- compiler/linker 必须检测同一 artifact 内的 Symbol collision，并将其视为构建错误；
- fingerprint algorithm/version 必须进入 artifact metadata，不能静默改变。

### 10.4.4 128-bit Symbol 与 32-bit target

128-bit `SymbolId` **不要求 128-bit CPU，也不要求 64-bit pointer width**。

在 64-bit CPU 上，它通常由两个 `u64` load/compare 表示；在 32-bit CPU 上，编译器可以拆成多个 32-bit 操作。它只是固定 16-byte value，不是指针。

因此核心设计必须支持：

```text
x86_64
AArch64
x86 32-bit
ARM 32-bit
wasm32
```

是否真正启用某个 32-bit 平台由 platform/GPU/backend 支持矩阵决定，而不是由 `SymbolId` 限制。

禁止：

- `SymbolId(usize)`；
- 假设 `usize == u64`；
- 假设指针是 64 bit；
- 假设 `AtomicU128` 存在；
- 假设 128-bit load/store lock-free；
- 把 `SymbolId` 当作指针或直接 memory-map 为平台 ABI。

Viso 不要求对 `SymbolId` 做原子 128-bit 更新。Symbol 是 immutable value；共享 symbol table 的并发通过 table generation、message passing、RCU/lock 或更高层协议解决。

### 10.4.5 Stable ID 不进入 steady-state hot storage

`SymbolId` 为稳定正确性优化；`PropertyId`、`EventId`、`ComponentTypeId` 等 dense ID 为执行性能优化。

加载/AOT link 阶段：

```text
Stable SymbolId
      ↓ resolve once
Dense process-local ID
      ↓
array index / compact table index
```

例如：

```text
viso::widgets::Button::text
        ↓ SymbolId(128)
        ↓ link
PropertyId(3)
```

每帧执行：

```rust
let slot = properties[property_id.index()];
```

而不是：

```text
SymbolId
  ↓ hash
HashMap bucket probe
  ↓
property
```

**稳态帧禁止因源码身份而执行全局 Symbol HashMap lookup。**

允许 Symbol lookup 的场景：

- module load/link；
- Hot Reload patch linking；
- Inspector query；
- Schema negotiation；
- migration；
- debug/source map；
- cache/artifact loading。

### 10.4.6 Typed dense runtime IDs

Runtime dense ID 必须用 Rust newtype 隔离语义：

```rust
#[repr(transparent)]
pub struct ComponentTypeId(u32);

#[repr(transparent)]
pub struct PropertyId(u32);

#[repr(transparent)]
pub struct EventId(u32);

#[repr(transparent)]
pub struct StyleId(u32);

#[repr(transparent)]
pub struct ShaderId(u32);
```

这些 newtype 在 optimized build 中没有额外 runtime abstraction cost。

具体宽度由每个 subsystem 的真实上限决定：

- 可证明单组件属性数远小于 65535 时，内部 `PropertySlot` MAY 使用 `u16`；
- public/runtime artifact ID 默认优先 `u32`，减少 overflow/转换分支；
- 不为节省 2 bytes 就在 hot loop 引入频繁 widen/narrow；
- 宽度改变属于 artifact/ABI decision，必须有 benchmark 和上限证明。

### 10.4.7 `NodeId`：运行时实例身份

`NodeId` 与 `SymbolId` 的语义完全不同：

```text
SymbolId = 源码/Schema 中“这个声明是谁”
NodeId   = 当前 Runtime 中“这个实例是谁”
```

继续采用：

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}
```

一个源码模板可以产生任意多个 Runtime Node：

```text
TodoRow template SymbolId
    ├── key=100 -> NodeId(27, 1)
    ├── key=101 -> NodeId(28, 1)
    └── key=102 -> NodeId(29, 1)
```

`NodeId.index` 直接索引 NodeArena/hot stores；`generation` 防止 remove/reuse 后的 stale handle 误访问。

Node hot arrays **不得为每个 Node 固定携带 16-byte `SymbolId`**。需要 debug/hot-reload reverse mapping 时使用：

- cold side table；
- template descriptor；
- optional debug metadata；
- sparse mapping；
- development-only reverse index。

Release 在不需要 Inspector/source diagnostics 时可以 strip 大部分 reverse symbol metadata。

### 10.4.8 `StableKey`：动态集合身份

动态列表：

```viso
for item in items key item.id {
    TodoRow {
        item: item;
    }
}
```

`item.id` 是 `StableKey`，不是 `SymbolId`，也不是 `NodeId`。

关系：

```text
Repeat template SymbolId
       + StableKey(value)
              ↓ reconciliation
          existing/new NodeId
```

规则：

- `StableKey` 必须有稳定 Eq/Hash 语义；
- Float 默认不得实现 `StableKey`；
- 列表排序变化不改变同 key child 的 Node identity；
- key lookup 只存在于 keyed reconciliation，不得扩散成所有 Node 的通用身份机制；
- reconciliation 完成后，后续 hot traversal 使用 `NodeId`。

### 10.4.9 Source、Resource、GPU handle 不复用 SymbolId

`SourceId` 表示 source-map/artifact 中的文件或 source unit；`ResourceId` 表示 asset/resource；`BufferId`、`TextureId`、`PipelineId` 表示 GPU resource。

它们可能在 debug metadata 中关联 `SymbolId`，但不得直接 typedef/alias 为 `SymbolId`。

尤其 GPU resource handle 推荐 generational typed handle：

```rust
pub struct TextureId {
    index: u32,
    generation: u32,
}
```

从而把：

```text
stable semantic identity
runtime object identity
GPU lifetime identity
```

保持为三个不同的概念。

### 10.4.10 Hot Reload identity

Hot Reload 使用 stable Symbol 做“新旧声明是否同一语义实体”的匹配，然后把结果应用到 retained runtime instance。

```text
new .vs / ui! / component!
        ↓
new Symbol graph
        ↓ compare stable SymbolId
old Symbol graph
        ↓
Migration Plan
        ↓
keep / patch / replace Runtime NodeId + state slots
```

Named node：

```viso
node save_button: Button {
    text: "Save";
}
```

拥有稳定 source symbol。若 Hot Reload 插入一个无关兄弟节点，`save_button` 的 symbol 应保持稳定，从而焦点、局部状态、动画等可按 Widget/Node migration contract 保留。

Anonymous decorative node 可以使用 structural symbol/fingerprint，但文档和诊断必须明确：结构位置大改时其身份不保证和具名节点相同强度。

### 10.4.11 Rust API 不要求用户手写通用 ID 宏

普通 Viso 用户不应该频繁写：

```text
id!(save_button)
ids!(root.panel.button)
live_id!(text)
```

对于编译期可知的具名节点：

```viso
node save_button: Button { ... }
```

`view!` / `ui!` / `component!` 和 schema codegen 应生成 typed access：

```rust
nodes.save_button
```

其类型可以是：

```rust
NodeHandle<Button>
```

或等价的 compile-time typed key/handle，而不是“字符串 ID lookup + dynamic downcast”。

这条规则的目标不是隐藏 ID，而是**不要把编译期已知信息丢掉，再让运行时重新查找一次**。

### 10.4.12 Dev metadata 与 Release stripping

Development artifact 可以保留：

```text
DenseId -> SymbolId
SymbolId -> canonical name
SymbolId -> SourceSpan
NodeId -> template/debug Symbol
PropertyId -> readable property name
EventId -> readable event name
```

因此 Inspector 能回答：

```text
NodeId(381, 4)
Component: app::home::TodoRow
Source: features/home/view.vs:82
Property: Button::text
Dirty reason: StateSymbol(todo.title)
```

Release 默认只保留运行所需的 dense/generational table；source name、reverse map、完整 Symbol path 和 debug spans 可按 profile strip。

### 10.4.13 Identity 性能合同

Identity benchmark 至少包含：

```text
identity_name_intern_100k
identity_symbol_build_100k
identity_symbol_link_100k
identity_dense_property_lookup_1m
identity_dense_event_lookup_1m
identity_node_generation_check_1m
identity_keyed_reconcile_100k
```

重点比较：

```text
stable Symbol HashMap lookup
vs
dense array lookup
```

不要把 benchmark 重点放在 `u64 == u64` 与 `u32 == u32`；真正的收益来自：

> **hash/probe/pointer chasing -> direct indexed access**。

稳态 frame benchmark 必须证明 local property/event/node hot path 不依赖字符串或 stable-symbol hash lookup。

---

## 10.5 Ende — Encode / Decode Infrastructure

Viso 1.0 自研并拥有 `viso-ende`，作为框架内部 typed encode/decode、wire protocol、artifact/cache serialization 的基础设施。

名称来自：

```text
EnDe = Encode + Decode
```

`Ende` 与媒体领域的 `codec` 必须严格区分：

```text
Ende
  -> structured data encode/decode

image/audio/video codec
  -> media compression/decompression
```

因此 Viso 文档中的 “codec” 默认指 PNG/JPEG/WebP/audio/video/compression 等媒体或压缩算法；结构化序列化一律称 **Ende**。

### 10.5.1 为什么 Ende 属于 Viso-owned infrastructure

Ende 位于 Ownership Ladder 的 Tier A，因为它直接影响：

- Studio ↔ Runtime protocol；
- Compiler ↔ Runtime Hot Reload artifact；
- Inspector / Profiler transport；
- remote preview；
- snapshot/cache；
- build protocol；
- schema/version compatibility；
- `SymbolId` / typed dense ID 的 wire representation；
- allocation behavior；
- malformed input 的安全边界；
- mobile/Web 二进制体积与编译时间。

Viso 自研 Ende 的目标不是“替代整个 Rust Serde 生态”，而是：

> **Viso 必须拥有自己的内部协议 ABI、数据布局、分配策略和版本语义。**

Serde 继续作为 ecosystem integration；Ende 是 Viso internal architecture。

### 10.5.2 crate 与 public namespace

当前 `crates/ende/src`（ADR 0012）：`encode.rs` / `decode.rs`（Binary `Encoder`/`Decoder` 与 `Encode`/`Decode` trait）、`wire.rs`（`WireId`、`ProtocolTag`）、`json.rs`（只写 JSON emitter）。目标可按需拆出 `limits`、`json` decoder 等模块，但保持单一 `viso-ende` crate，不额外制造 derive/core/schema crate。

`#[derive(Encode, Decode)]` 是目标（尚未实现，当前为手写 impl），计划放在 proc-macro 叶子 `viso-macros`：

```rust
#[derive(Encode, Decode)]
struct BuildMessage {
    build_id: BuildId,
    target: TargetId,
}
```

普通应用通过 facade 使用：

```rust
use viso::ende::{Encode, Decode};
```

`Encode/Decode` 不默认塞进 `viso::prelude::*`；只有确实高频且不会制造命名冲突时才考虑 re-export derive macro。

### 10.5.3 Ende 只支持两类一等格式

Viso 1.0 一等支持：

```text
Binary
JSON
```

用途：

| 格式 | 主要用途 |
|---|---|
| Ende Binary | runtime/tool IPC、Hot Reload、cache、snapshot、profiler、remote preview |
| Ende JSON | diagnostics、Schema dump、CLI/LSP/AI、外部工具互操作 |

**RON 不属于 Viso。**

硬规则：

- 不提供 `ende::ron`；
- core protocol 不允许引入 RON；
- Viso 配置不因为 Ende 再增加 RON 文件类型；
- `.vs` 负责 Viso 人类可读 DSL/config authoring；
- JSON 负责通用工具交换；
- binary 负责高效内部协议；
- 第三方库若自行使用 RON，不得让它成为 framework core dependency 或 canonical Viso format。

### 10.5.4 Ende 不是 frame data model

Ende 可以高频使用，但**不能成为 UI frame 内部数据模型**。

禁止：

```text
State
  -> Encode
  -> Decode
  -> UI
```

禁止：

```text
NodeArena
  -> serialize every frame
  -> Renderer
```

Frame 内必须继续使用直接 typed memory：

```text
NodeArena
LayoutStore
TransformStore
PaintStore
RenderPrimitive[]
InstanceBuffer
```

Ende 主要用于边界：

```text
process boundary
thread/message boundary
persistent cache
Studio protocol
remote preview
snapshot
Hot Reload artifact
build protocol
diagnostics/schema JSON
```

若同一进程内 producer/consumer 已共享安全 typed memory，禁止为了“统一协议”强行插入 Ende roundtrip。

### 10.5.5 API 设计：直接，不复制复杂通用 visitor surface

目标 public contract：

```rust
pub trait Encode {
    fn encode<E: Encoder>(&self, encoder: &mut E) -> Result<(), EncodeError>;
}

pub trait Decode<'de>: Sized {
    fn decode<D: Decoder<'de>>(decoder: &mut D) -> Result<Self, DecodeError>;
}
```

具体 trait 可以根据实现收敛，但必须保持：

- derive-generated direct field code；
- format-specific encoder/decoder；
- 不要求 runtime reflection；
- Binary field loop 不要求字符串 lookup；
- Binary field loop 不要求 per-field HashMap；
- 不依赖 `dyn Trait` 进行每字段动态派发；
- 可在复用 buffer 下做到零 heap allocation encode；
- decoder 有明确 lifetime/borrow 模型。

方便 API 可以存在：

```rust
let bytes = ende::bin::encode(&message)?;
let message: BuildMessage = ende::bin::decode(&bytes)?;
```

性能敏感内部代码优先：

```rust
buffer.clear();
message.encode(&mut BinEncoder::new(&mut buffer))?;
```

使已有 capacity 被复用。

### 10.5.6 Binary format 原则

第一版 Ende Binary 优先**简单、确定、容易 fuzz、容易跨平台验证**，而不是追求每个 byte 的极限压缩。

默认原则：

```text
canonical little-endian
fixed-width little-endian scalars
unsigned LEB128 varint for byte/string/collection lengths
zig-zag varint for signed integers where the codec uses varint
IEEE-754 fixed-width float representation
explicit enum discriminant
length-prefixed UTF-8 string/bytes
known struct field order
bounded collection lengths
no field-name strings in normal struct payload
```

例如：

```rust
#[derive(Encode, Decode)]
struct PropertyChanged {
    node: NodeId,
    property: PropertyId,
    revision: Revision,
}
```

Binary payload 不写：

```text
"node"
"property"
"revision"
```

这些字段名属于 Schema/debug metadata，不属于每条 hot protocol message。

如果两端已经共享 message schema，wire 应直接编码固定字段值。

### 10.5.7 不直接序列化 Rust struct memory

Ende Binary 禁止：

```rust
unsafe { write_bytes_of(&value) }
```

作为普通 wire contract。

原因：

- Rust field layout 不稳定；
- padding 不稳定；
- endian 不同；
- pointer width 不同；
- `usize`/pointer 不能形成跨目标稳定 ABI；
- enum niche/layout 不是协议；
- 32-bit/64-bit build 必须互通。

每个 primitive 和 framework ID 都必须显式定义 wire representation。

例如 `SymbolId`：

```text
16 bytes canonical representation
```

逻辑上等价于：

```rust
encode_u64_le(symbol.lo);
encode_u64_le(symbol.hi);
```

因此 32-bit、64-bit、wasm32 可以得到同一 wire value。

### 10.5.8 `usize`、pointer 与 platform handle

Ende stable wire schema 中禁止直接编码：

```text
usize
isize
raw pointer
process-local address
OS handle numeric value as portable identity
```

若业务值本身需要“长度/索引”：

- 明确选择 `u32` 或 `u64` wire type；
- decode 时检查是否可转换为当前 `usize`；
- 超出目标地址空间时返回结构化错误，不截断。

Native/OS handle 跨边界必须通过专门的 handle-transfer protocol，而不是把指针/FD/HANDLE 当普通整数 Ende。

### 10.5.9 Schema、message tag 与稳定身份

Ende 不要求每条消息携带 128-bit `SymbolId`。

推荐：

```text
stable protocol/schema identity
        ↓ handshake/load
process/session-local MessageId / TypeId
        ↓
hot message stream
```

与 UI identity 相同：

> stable identity 用于协商和正确性，dense ID 用于执行。

协议 artifact 至少定义：

```text
protocol id/version
message tag
schema fingerprint
field order/type
optional/required policy
limits
compatibility policy
```

Hot Reload/Studio 可以在建立连接或载入 artifact 时校验 stable schema fingerprint，随后用紧凑 message tag 传输。

### 10.5.10 Versioning 与兼容策略

Ende 的“版本”是**协议/Schema 数据的一部分**，不是靠“decoder 猜字段”。

每个长期持久或跨版本 protocol 必须定义：

- protocol/schema version；
- compatible additive change；
- incompatible change；
- unknown message policy；
- unknown enum tag policy；
- optional field/default semantics；
- cache invalidation policy。

Runtime/Studio/Compiler protocol 不允许静默把不兼容数据按旧布局解释。

对于短生命周期 process-local IPC，可以选择严格 exact-version handshake，换取更简单更快的 payload。

### 10.5.11 Decoder 安全预算

所有外部或可损坏输入必须经过 bounded decoder。

至少限制：

```text
max message bytes
max nesting depth
max string bytes
max bytes payload
max collection length
max map entries
max allocation bytes
max recursion / decode stack
```

错误至少区分：

```text
UnexpectedEof
InvalidUtf8
InvalidTag
InvalidLength
DepthLimitExceeded
AllocationLimitExceeded
SchemaMismatch
ProtocolMismatch
TrailingBytes (strict mode)
```

Decoder：

- MUST NOT panic on malformed input；
- MUST NOT blindly preallocate attacker-controlled length；
- MUST check integer overflow；
- MUST preserve byte offset for diagnostics；
- fuzz target 必须覆盖所有 public decode entry。

### 10.5.12 Borrowed / zero-copy decode

对于只读短生命周期 payload，Ende MAY 支持 borrowed decode：

```text
&'de str
&'de [u8]
BorrowedMessage<'de>
```

要求：

- lifetime 与输入 buffer 绑定；
- borrowed value 不跨越输入 buffer 生命周期；
- Hot Reload/async queue 若需要长期持有，必须显式 own/copy/retain；
- 不为了 zero-copy 引入 unsafe dangling reference；
- benchmark 证明收益后再扩展复杂 borrowed collection surface。

### 10.5.13 JSON 的定位

Ende JSON 优先：

- human inspectable diagnostics；
- machine-readable compiler messages；
- LSP/AI Schema；
- `viso schema/hir/ir` dump；
- test golden；
- 外部工具互操作。

JSON 不作为 frame/runtime 内部高速消息的默认格式。

当前 `json.rs` 只有手写 write-only emitter（无 serde）；目标 JSON decoder 落地时，encoder/decoder 必须共享 Ende 的：

- type/schema metadata；
- limits/error model；
- derive contract；
- deterministic test infrastructure。

需要 canonical JSON 的 snapshot/test 场景必须定义稳定 field ordering；普通 JSON object 语义仍不依赖 map insertion order。

### 10.5.14 Serde integration

Serde 继续位于：

```text
integrations/serde/
```

用途：

- Rust 生态已有数据模型；
- third-party API；
- 用户后端/network/database；
- 需要 Serde-specific crate 的场景。

边界：

```text
Viso internal protocol -> Ende
Rust ecosystem interop -> Serde adapter
```

禁止让 `serde::Serialize/Deserialize` 成为 Viso Runtime、Node、Hot Reload 或 Studio protocol 的硬依赖。

同样，Viso 不要求用户业务数据全部改为 Ende；Ende 只拥有 Viso 自己必须稳定控制的内部边界。

### 10.5.15 Ende 与 `viso-macros`

目标：`viso-macros` 提供 `#[derive(Encode)]` / `#[derive(Decode)]`（尚未实现）。

derive 必须生成直接 field encode/decode，不建立运行时 reflection map。

Macro expansion 必须：

- 给字段/variant 生成稳定 schema metadata；
- 在编译期拒绝不支持的裸 pointer/reference lifetime；
- 对 `usize/isize` 的持久 wire 使用给出诊断或要求显式 adapter；
- 支持 `#[ende(...)]` 一类受控 attribute 时，attribute 集合必须小且有明确 Schema；
- 不允许 attribute 改变基础 tokenization 或引入运行时字段名查找。

### 10.5.16 Ende 与 Identity Architecture

当前 `wire.rs` 的 `WireId` 是 1 字节 `IdKind` tag（`Name`/`Symbol`/`Custom(u8)`）+ varint `u64`；`ProtocolTag` 为 `b"VE"` magic + `WIRE_VERSION`。Ende 只定义 ID 的 wire *格式*，不拥有 ID 类型。以下为目标 artifact 表示（128-bit `SymbolId` 的最终 wire 形式仍待定）：

```text
NameId          通常不持久编码
SymbolId        fixed 16 bytes
NodeId          u32 index + u32 generation
PropertyId      fixed u32 (artifact contract)
EventId         fixed u32
ComponentTypeId fixed u32
SourceId        fixed u32
```

注意：某 subsystem 内部即使把 `PropertySlot` 压缩为 `u16`，跨 artifact/wire 的 `PropertyId` 表示也必须由协议明确，不允许直接 memcpy 内部 struct。

Development protocol 可以附带：

```text
SymbolId + source/name metadata
```

Release/runtime hot protocol 应尽量使用协商后的 dense ID/message tag。

### 10.5.17 Ende 性能合同

Ende Binary steady-state encode（buffer capacity 已满足）目标：

```text
0 heap allocation
0 runtime reflection
0 field-name string lookup
0 per-field HashMap lookup
0 per-field dyn dispatch
```

Decode 是否零分配取决于目标值是否拥有 String/Vec 等 owned data；对于 borrowed/预分配目标路径，应提供零或接近零分配选项。

Benchmark 至少：

```text
ende_bin_small_message
ende_bin_1k_messages
ende_bin_100k_messages
ende_bin_encode_reused_buffer
ende_bin_decode_owned
ende_bin_decode_borrowed
ende_json_diagnostic_1k
ende_schema_load
ende_studio_protocol_roundtrip
ende_hot_reload_patch_roundtrip
ende_profiler_trace_encode
```

记录：

- ns/message；
- MB/s；
- allocations/message；
- bytes allocated；
- bytes/message；
- peak memory；
- code size；
- compile time（derive-heavy fixture）。

### 10.5.18 Ende correctness / fuzz contract

测试至少覆盖：

```text
encode -> decode roundtrip
cross 32/64-bit golden bytes
endianness golden bytes
all primitive boundaries
NaN/Inf/-0 float bit behavior
invalid UTF-8
truncated input
huge declared length
unknown enum tag
nesting limit
schema mismatch
old/new compatible message fixtures
```

Fuzz：

```text
bin decoder never panics
json decoder never panics
bounded decoder never exceeds configured allocation budget
encode(decode(valid_bytes)) preserves semantic value
```

### 10.5.19 Ende 的非目标

Viso Ende 不追求：

- 成为 Rust 通用 Serde 替代生态；
- 支持任意 serialization format plugin；
- RON；
- YAML/XML 等核心格式；
- 通过 reflection 在运行时自动发现未知字段；
- 直接序列化 Rust memory layout；
- 给 GPU buffer layout 提供通用 serialization；
- 让 frame data 在 Encode/Decode 后才能流转；
- 为极限 wire size 默认引入 delta/compression 等复杂编码（长度与有符号整数的 LEB128 / zig-zag varint 是 ADR 0012 的基础格式，不属此列）。

如果 benchmark 表明特定 remote/snapshot 流量需要压缩，应把 compression 作为 Ende payload 外层或专用协议层处理，而不是污染基础 field encoding。

---

# Part IV — Runtime 与 Frame Pipeline

## 11. Runtime 的职责

`viso-runtime` 是应用执行内核，但不是“万能 Cx”。

负责：

```text
OS events
Task wakeups
Timers
VSync / redraw requests
Lifecycle changes
Resource completions
         ↓
Frame Scheduler
         ↓
UI update phases
         ↓
Render submission
```

### 11.1 Main Loop 所有权

Viso 必须拥有 UI 主循环和 frame scheduler 的最高控制权。

不推荐：

```text
Tokio Runtime
   ↓
Viso UI
```

推荐：

```text
Viso Runtime
   ├── OS event pump
   ├── timers
   ├── task completions
   ├── animation tick
   ├── UI update
   └── render/vsync
```

外部 async runtime 通过 adapter 接入，而不是反过来控制 frame 生命周期。

### 11.2 Frame phases

一帧的语义阶段如下（ADR 0043，取代 ADR 0001 的十二阶段表；代码中为 `viso_runtime::FramePhase::ORDER`）：

```text
 1. CollectInput
 2. DispatchInput           // hit-test 结构在此按需重建（HIT_TEST dirty），不单设阶段
 3. RunContinuations        // spawn_then 续体、到期 timer、async completion；写入当前 transaction
 4. ApplyDevPatches         // Dev-only：已 stage 的 PatchBundle 在帧边界 commit；Release build graph 中不存在
 5. AdvanceAnimations       // 推进 AnimationTrack；transform 动画直接写 TransformStore
 6. FlushStateTransactions  // 全帧唯一一次 flush：binding 求值 + dirty 标记
 7. ResolveStyle
 8. Measure
 9. Layout
10. UpdateTransforms        // transform root 位移重算（每 root 一次加法）、subtree bounds
11. BuildPaintChanges       // 只有 PAINT 的帧原位改写 dirty 节点的 primitive，其余整树 paint
12. BuildRenderBatches      // 同时计算 damage region（§30.3）
13. UploadGpuChanges        // GPU：range upload；改写过的帧 Renderer::patch 只上传改写的 slot；只移动 root 的帧不上传，renderer 取新位移；CpuRaster：damage tile 光栅化
14. Submit                  // present_with_damage
15. RunEffects              // effect 重启 / cleanup；产生的 state 写入进入下一帧
16. UpdateSemantics         // present 之后执行，不在 input-to-photon 关键路径上；只在 AT 激活时发布
17. PostFrameCleanup
```

规则：
- 实现可以合并阶段，但语义顺序必须保留；空阶段的成本必须是 O(1)（按 dirty worklist 是否为空判断，而不是遍历树）。
- 3–5 产生的所有写入与 2 中 handler 的写入合并为同一个 transaction，在 6 一次性 flush。不允许出现「动画 flush 一次，事件再 flush 一次」。
- 需要同帧可见的派生值必须使用 computed，不能用 effect。effect 只承载副作用，它的写入最早在下一帧可见。
- effect 环跨帧检测：一个 effect 连续 `SETTLE_ROUNDS`（16）帧仅因 effect 写入而被唤醒，即判定为跨帧反应环，停止运行并报告 `E4202`；帧内 flush 环仍由 settle 轮数上限截断。
- Semantics 放到 present 之后，是为了不拉长输入到上屏的延迟。辅助技术读到的树最多滞后本帧 present 几百微秒，不会跨帧。

### 11.3 Phase-specific Context

不再让一个 `Cx` 拥有所有能力。

建议：

```rust
AppCx
EventCx
UpdateCx
LayoutCx
PaintCx
RenderCx
TaskCx
```

部分 context 可作为 internal API。

例如：

```rust
fn layout(&mut self, cx: &mut LayoutCx<'_>, constraints: Constraints) -> Size;
```

`LayoutCx` 不提供网络请求、窗口创建、任意 GPU submit 等能力。

这既是架构限制，也是性能与可推理性工具。

---

## 12. Scheduler 与 redraw 策略

不能把 `redraw()` 作为业务层的常规责任。

Runtime 聚合以下原因（`viso_runtime::RedrawReason`）：

```text
InputDirty
StateDirty
AnimationActive
TimerDue
AsyncCompletion
WindowResize
ExternalSurfaceInvalidation
HotReload
FirstFrame        // 启动首帧，由 scheduler 拥有（ADR 0004）
```

最终决定（`FrameDecision`）：

```text
NoFrame
FrameAtNextVsync
ImmediateFrame
BackgroundMaintenance
```

多个 state mutation 在一个输入事件中自动 batch。一次性 timer 由 driver 持有，最近 deadline 以 `WaitUntil` 驱动平台 pump，而不是轮询（ADR 0019）。

### 12.1 Idle 必须真正 idle

没有动画、输入、任务和脏数据时：

- 不持续 layout；
- 不持续 paint；
- 不持续 submit；
- 不周期性遍历 UI tree；
- 不轮询每个 component。

---

## 13. Async runtime

最小官方 API 是 handler 上下文的 `cx.spawn(...)` / `cx.spawn_then(...)`：task 归 handler 所在节点所有，节点释放时自动取消；`spawn_then` 的续体在帧边界拿到 `UpdateCx` 写回状态（§13.2，ADR 0032）。

### 13.1 核心协议

Viso 自己拥有的是 **UI task protocol，而不是一套通用 async runtime**：

- task ID；
- wakeup queue；
- cancellation token；
- main-thread dispatch；
- timer/lifecycle integration；
- scoped-task ownership；
- executor adapter contract。

通用 work-stealing executor、I/O reactor、HTTP/TLS 等不进入 Viso 核心。默认发行版可以绑定一个经过验证的 executor adapter，但它不能拥有 UI main loop。

Tokio/Smol 作为 adapter（目标，尚未建立）：`viso-tokio`、`viso-smol`。

不把所有 adapter 组合直接做成核心 crate feature matrix。

### 13.2 实现（ADR 0032）

- `viso-platform` 的 `LoopWaker` 可跨线程 kick，loop 线程上收到一次 `RawEvent::Wakeup`；每个 backend 复用自己已有的唤醒通道。
- `viso-runtime::task::TaskSet` 持有 task 与 wake queue。首次 wake 或 spawn 才 kick；没有 wake 的帧不加锁、不 poll。`FrameDriver::on_wakeup` 为有 task 被唤醒或 timer 到期的窗口请求重绘。
- `EventCx::spawn` / `spawn_then` / `cancel_task`：task 归 handler 所在节点所有，释放子树或清空 store 时随之取消。`spawn_then` 的续体在帧边界拿到 `UpdateCx`，因此 `.await` 期间不持有 arena 引用。
- `cx.services()` 由 facade prelude 中的 `ServicesExt` 提供；store 以 `Rc<dyn Any>` 借出 `Services`，所以 `ui` 不依赖 `services`。
- 暂缓：`UpdateCx` / `BuildCx` / `TaskCx` 上的 spawn 与 services、detached spawn、adapter crate。

### 13.3 Worker 数量由 profile 决定

worker 线程池是可选资源，不是正确性前提。`embedded-cpu` profile 默认 `workers = 0`：text shaping、image decode、font parse 这类 TextWork / AssetWork 在主线程的 idle slice 内按时间预算执行（预算由 frame deadline 推算），结果与多线程路径一致。任何 subsystem 都不得假设「至少存在一个 worker」。

---

# Part V — UI Runtime 数据模型

## 14. 不再以 `Rc<RefCell<Box<dyn Widget>>>` 作为树基础

Viso 使用 generational arena + integer ID。

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}
```

核心原因不是“Rust 风格更漂亮”，而是：

- 减少每节点 heap allocation；
- 减少 pointer chasing；
- 避免 runtime borrow panic；
- 允许高密度数据布局；
- 方便 dirty bitset；
- 方便并行/分阶段处理；
- generation 防止 stale handle；
- Inspector/Viso DSL runtime 可安全持有稳定 ID。

### 14.1 Tree 与数据存储分离

UI 仍然是真正的树，不做“everything is ECS”。

推荐：

```rust
/// u32::MAX 表示无。树内部链接由 arena 不变量保证有效，不携带 generation。
#[repr(C)]
struct NodeLinks {
    parent: NodeIndex,
    first_child: NodeIndex,
    last_child: NodeIndex,
    prev_sibling: NodeIndex,
    next_sibling: NodeIndex,
} // 20 bytes
```

内部链接与外部 handle 分开：
- **外部 handle**（`NodeId` / `Handle<T>`、Inspector、DSL runtime、跨帧持有者）携带 generation，用于检测 stale；
- **内部链接**（parent/child/sibling、binding target、dirty worklist、paint range owner）只存 `NodeIndex(u32)`。节点释放时 arena 先解链，因此内部链接永远不会指向已释放的 slot。

当前实现用 5 个 `Option<NodeId>`（每个 12 bytes，共 60 bytes）。改为 index 链接后，热遍历每条 cache line 能装下约 3 倍的节点。

Node 的热数据分区存储：

```text
NodeMeta[]
NodeLinks[]
LayoutData[]
TransformData[]
ClipData[]
InteractionData[]
SemanticsData[]
PaintHandle[]
```

不是要求所有数组永远严格 SoA，而是按访问模式决定 AoS/SoA。原则：**一起访问的数据尽量一起存；很少访问的大字段不要污染热结构。**

### 14.2 Hot / Warm / Cold data

建议明确三类：

**Hot**（帧内高频）：

- parent/child traversal；
- bounds；
- transform；
- dirty flags；
- visibility；
- clip；
- hit-test flags。

**Warm**（脏时访问）：

- layout style；
- computed style；
- semantics；
- animation descriptors；
- binding metadata。

**Cold**：

- debug name；
- source map；
- component type name；
- inspector metadata；
- Viso source location。

Release 可以 strip 大部分 Cold 数据。

---

## 15. Component、Node、Render Primitive 三者不能合并

### 15.1 Component

负责：

- persistent logical state；
- event handler；
- lifecycle；
- child composition；
- effect/task ownership。

### 15.2 Node

负责：

- tree identity；
- layout box；
- style result；
- transform；
- input region；
- semantics；
- paint reference。

### 15.3 Render Primitive

只表达 renderer 需要的信息：

```rust
enum Primitive {
    Quad(QuadPrimitive),
    GlyphRun(GlyphRunPrimitive),
    Image(ImagePrimitive),
    Path(PathPrimitive),
    Mesh(MeshPrimitive),
    Layer(LayerPrimitive),
}
```

一个 Component 可以生成多个 Node；一个 Node 可以对应多个 Primitive；同一类 Primitive 可以被 renderer 合并到同一 batch。

---

## 16. Node Arena 设计建议

### 16.0 Viso 1.0：hot storage 采用 hybrid indexed SoA

不再把“纯 SoA / hybrid / chunked archetype”留到后期决定。Viso 采用 **hybrid indexed SoA**：

```text
NodeId(index,generation)
        │
        ├── NodeMeta[index]          // compact AoS: parent/child/sibling/type/flags
        ├── LayoutStore.*[index]     // hot SoA
        ├── TransformStore.*[index]  // hot SoA
        ├── PaintStore.*[index]      // hot SoA / compact handles
        ├── InteractionStore.*[index]
        └── SparseColdTables         // semantics detail/debug/rare extension data
```

关键约束：

- hot store 以同一个 `NodeIndex` 直接寻址，热遍历不经过 per-node HashMap；
- `NodeMeta` 保留为紧凑 AoS，因为 parent/child/sibling 通常一起访问；
- 低频、可选、大对象不强迫所有 node 支付固定空间；
- 不采用 archetype migration 作为默认节点变更机制；UI identity 和 hierarchy 比 ECS 组件组合更稳定；
- 后续可以在单个 store 内优化 chunk/page，但不能改变 `NodeId -> fixed index stores` 这一基线语义，除非新 ADR 证明收益。

概念结构：

```rust
pub struct NodeArena {
    slots: Vec<NodeSlot>,
    free: Vec<u32>,
}

struct NodeSlot {
    generation: u32,   // 奇数 = occupied，偶数 = free；释放与复用各 +1
    links: NodeLinks,
    flags: NodeFlags,
}
```

实际优化版可以把 `generation/links/flags` 分离成更紧凑数组。

容量策略：
- 默认按需增长；
- `NodeArena::with_fixed_capacity(n)`：mount 后不再增长，超出时返回结构化错误（`E7xxx ArenaFull`），不触发 realloc。embedded profile 用它把峰值内存变成编译期/启动期可知的量；
- 所有 hot store 与 arena 同步扩容，一次扩容只做一次批量 realloc。

### 16.1 ID 验证

```rust
fn get(&self, id: NodeId) -> Option<&NodeData> {
    let slot = self.slots.get(id.index as usize)?;
    (slot.generation == id.generation && slot.occupied).then_some(...)
}
```

Debug build 可以增加：

- owner tree ID；
- source location；
- creation epoch；
- mutation borrow guard。

Release 只保留必要字段。

### 16.2 Node mutation

不允许业务代码长期持有 `&mut Node`。

使用短生命周期 API：

```rust
cx.node(node_id).set_visible(false);
```

或内部：

```rust
arena.with_node_mut(id, |node| { ... });
```

防止跨 phase 持有引用导致 aliasing 和迁移困难。

---

# Part VI — Reactive State 与增量失效

## 17. 不采用 Virtual DOM

Viso 的默认更新路径：

```text
State changed
   ↓
Binding dependency
   ↓
Dirty property / dirty node
   ↓
Incremental style/layout/paint
```

而不是：

```text
State changed
   ↓
Re-run component
   ↓
Build virtual tree
   ↓
Diff
   ↓
Patch retained tree
```

结构性变化（条件 child、循环列表、component type change）才进入 tree reconciliation。

---

## 18. State Slot

内部推荐使用 slot，而不是每个 Signal 一个 `Rc<Vec<Subscriber>>`。

概念：

```rust
pub struct StateId { index: u32, generation: u32 }

struct StateSlot {
    value: StateValue,
    generation: u32,
    occupied: bool,
    kept: bool,     // 属于某个模板实例的 block（§18.1），释放后不进 free list
    pending: bool,  // 已在本帧 pending 写集里：去重 O(1)
}
```

state 与 binding 分开存：slot 不记录 binding 区间，边集由 binding table 按 slot 索引（§18.1）。

状态实际值可以：

- 对组件局部 state 使用 component-owned typed storage；
- 对 Viso DSL runtime 使用 typed value arena；
- 对 global store 使用专门 store storage。

`StateId` 只负责 dependency indexing，不要求把所有 Rust 值 erase 成动态 `Value`。

### 18.1 Binding table

编译/宏可生成：

```text
State(count) → Binding(label_text)
State(count) → Binding(progress_value)
```

Binding 只是失效边：源 cell 变化时给目标节点标哪些 dirty class。属性值由节点自己的读取者（content、control、`Shown`）在对应阶段取，不挂在边上。

反复挂载的结构——VirtualList 的行、`for` 的 item、`if` / `match` 的 arm——按 **模板实例** 构建（`BuildCx::instance(&mut Template, ..)`，region 内部由 `mount_regions` 自动包裹）：

- 实例的节点占 arena 中一段连续 slot（block），实例自己的 state（`cx.state`、region 保留的 local）占 state store 中一段连续 slot；
- 于是实例的每条边都是 `(cell 偏移, 节点偏移, class)`，对同一模板的所有实例完全相同，模板只存一份：

```rust
/// 模板级：同一模板的所有实例共享一份。
struct TemplateBinding {
    state: StateRef,        // Local(相对实例 state base 的偏移) | Import(实例外的 StateId)
    target: u32,            // 相对实例 node base 的偏移
    dirty: DirtyClass,
}

/// 实例级：每个 regular 实例只存基址（12 bytes）。
struct InstanceBase {
    template: u32,
    node_base: u32,
    state_base: u32,
}
```

规则：

- **录制**：模板的第一个连续实例录下边集与 block 尺寸；之后的实例按尺寸预留 block，构建结束时边集与模板一致即为 regular 实例，不存任何自己的边。
- **Import**：实例外的 cell（列表共享的 selection、主题）的边也存在模板里；它变化时扇出到模板的每个 regular 实例。
- **不一致**：绑定方式不同、尺寸不同或节点不连续的实例退回绝对边 `(StateId, NodeId, class)`，归该实例所有。绑定到实例外节点的边同样是该实例的绝对边。
- **生命周期**：实例活到它最后一个节点被释放。block 内被释放的 slot 留在 block 里，不进普通 free list，所以按偏移标脏永远不会命中别人的节点。`BindingTable::collect` 回收死实例：释放它的 state 与边，并把两个 block 按长度保留给下一个同尺寸实例——行回收复用彼此的 block，滚动不增长任何存储。没有任何实例的模板随之释放，下次重新录制。
- **Flush**：`state → state_owner[slot] → InstanceBase → 模板 run[offset]`，命中节点 `node_base + target`；import 走模板的实例表；绝对边是按 cell 链接的 slab，插入、删除都是 O(1)。

Release 原生 lowering（ADR 0041）生成的是节点读取者的求值函数，与失效边分开；`EvaluatorId` 就是 chunk 索引，Dev 热更新期间同一索引指向解释执行的 chunk。

---

## 19. Dirty flags

Dirty class 固定为以下八个，不得增减或改名（ADR 0001；代码 `viso_ui::dirty::DirtyClass`）：

```rust
pub struct DirtyClass(u8);
// STRUCTURE = 1 << 0   STYLE  = 1 << 1   MEASURE  = 1 << 2   LAYOUT    = 1 << 3
// TRANSFORM = 1 << 4   PAINT  = 1 << 5   HIT_TEST = 1 << 6   SEMANTICS = 1 << 7
// BUBBLING  = STRUCTURE | MEASURE | SEMANTICS   // 沿父链上传；其余只标记本节点
```

不存在 `INPUT` / `INTERACTION` / `RESOURCE` dirty class：交互状态变化标记 `STYLE` 和/或 `HIT_TEST`；资源更新是 Resource key 的 revision，由持有该资源的属性再映射为具体 class；Effect 也不是 dirty class（ADR 0005）。

属性需要定义自己的 invalidation contract。

例：

| 变化 | Dirty |
|---|---|
| 子节点增删/重排 | STRUCTURE（VirtualList 行 remount 只标 LAYOUT，ADR 0008） |
| text 内容 | MEASURE + LAYOUT + PAINT + SEMANTICS |
| text color / background | PAINT |
| width | MEASURE + LAYOUT |
| transform / scroll offset | TRANSFORM（永不 MEASURE/LAYOUT/PAINT；hit test 在查询时按逆 world transform 换算，不需要 HIT_TEST，ADR 0043） |
| aria/label | SEMANTICS |
| hover/pressed/focus 状态 | STYLE，随后由 style diff 决定 PAINT/LAYOUT；命中区域变化另标 HIT_TEST |
| visibility | LAYOUT/PAINT/HIT_TEST/SEMANTICS，具体取决于模式 |

### 19.1 Dirty propagation

不是所有脏状态都向整棵祖先树传播。

例如：

- `PAINT` 通常不需要让祖先 layout dirty；
- `MEASURE` 需要传播到受 child intrinsic size 影响的祖先，停在两个轴都是 Fixed 的祖先（ADR 0008）；relayout 一般从最高的 MEASURE 节点的父节点重做；但当父节点是两个轴都 Fixed 的 flex 容器、本帧没有重排子节点也没有 MEASURE / LAYOUT、而该节点没有隐藏、主轴请求是 Fixed 或 fill 份额时，它的新 measure 移动不了父节点和任何兄弟，只从它自己重做：主轴保持原来的区间，交叉轴按新 measure 重新放置。节点自身的尺寸请求改变（`set_fixed_size`、fill weight、长度折算、隐藏）都会让父节点重排子节点，所以这条捷径只覆盖内容引起的 re-measure（文本、图片、子树）。差分测试逐帧比较增量 relayout 与整树 layout 的每个 bounds；
- `TRANSFORM` 可以只影响 transform subtree；
- `STRUCTURE` / `MEASURE` / `SEMANTICS` 属于 `BUBBLING`，沿父链上传；`SEMANTICS` 上传后仍只重建 semantics tree 的对应分支。

需要专门的 propagation rules，而不是简单 `parent.redraw()`。

### 19.1.1 Dirty worklist 合同

dirty bit 只回答「这个节点脏不脏」，不能用来找出脏节点。每个 dirty class 必须有自己的 worklist：
- 标脏时，若该 bit 原来为 0，就把 `NodeIndex` 追加进对应 class 的 worklist（追加前不检查重复，bit 本身就是去重）；
- 每个阶段只消费自己的 worklist，成本是 O(脏节点数)，与树的总节点数无关；
- `MEASURE` / `LAYOUT` 的 worklist 先归约成脏根集合（祖先已在集合中的节点丢弃），再按 depth 升序处理，同一祖先在一帧内只 relayout 一次；
- 所有 worklist 的 buffer 跨帧复用，稳态零分配；
- worklist 为空的阶段直接跳过，这是「idle 真正 idle」的机制保证。

### 19.2 Viso 1.0：Transform 是独立失效平面

Layout tree 定义结构与几何约束；Transform plane 定义最终局部/世界变换、滚动偏移和动画变换。两者使用同一个 `NodeId` 索引，但 dirty propagation 分离：

```text
Layout change
  -> MEASURE/LAYOUT
  -> may update transform base rect

Scroll / translate
  -> TRANSFORM
  -> 写该节点的 scroll / translate
  -> UpdateTransforms 按 paint 顺序重算 transform table 中每个 root 的位移
  -> UploadGpuChanges 不上传；Renderer::set_transforms 取新位移，
     encode 时每个 draw 按所属 root 的 current - painted 平移
  -> damage = subtree 旧 world bounds ∪ 新 world bounds
  -> never MEASURE/LAYOUT/PAINT/HIT_TEST

color-only change / animation
  -> PAINT
  -> BuildPaintChanges 在上次整树 paint 的 stream 里原位改写该节点自己的 primitive
     （box + content；种类与个数不变才成立），记下被改写的位置
  -> UploadGpuChanges：Renderer::patch 只重 ingest / lower / 上传这些 primitive
  -> damage = 这些 draw 的旧 key 与新 key 不同处
  -> 代价按改变的 primitive 计，与树多大无关

opacity animation
  -> PAINT
  -> opacity 留在 (0, 1) 内：原位改写该节点的 Layer primitive；
     Renderer::patch 重跑 layer planner，plan 不变时改 offscreen composite 的
     alpha，或以新 fold 重写被折叠 group 的子 instance
  -> opacity 跨过 0 或 1：layer 出现 / 消失，整树 paint
```

为什么 paint-only 帧是 O(改变量)：每次整树 paint 都给每个节点记下它自己的 drawable 在 stream 中的区间、所在 transform root（`PaintIndex`，每节点 16 字节）。一帧只有 PAINT（没有 STRUCTURE / MEASURE / LAYOUT 标记，也没有 opacity 跨过 0 或 1、overlay、首次 translate 这类改变 stream 形状的写入）时，UI 只对 PAINT worklist 上的节点按记录的位移重新生成自身 primitive，种类与个数相同就原位替换并列出改变的位置（`Painted::Patched`），否则退回整树 paint。renderer 侧每个 paint-order entry 记着自己在 family scratch 中的位置：`patch` 先校验（同一 store、同一 bounds、同一纹理、帧内没有 backdrop capture），再把新 instance 写回 store slot、按记录位置重新 lower、只上传这些 slot，并给它的 surface draw（经 offscreen pass 的 composite 向上）换 key；自上次 present 以来位移没变时，damage 直接取这些 draw，不再全表 diff。任何一步不成立都返回 `false`，调用方上传整个 stream，结果总是正确。半透明节点的 opacity 在 (0, 1) 内变化时同样走 patch：UI 原位改写它的 `Layer` primitive，renderer 改 offscreen composite 的 alpha，或以新 fold 重写被折叠 group 的子 instance。2000 张卡片（8000 个节点）的页面 hover 一帧从约 0.7 ms 降到约 0.3 µs，淡入淡出一帧约 0.5 µs，都与卡片数无关（`paint_scale` bench）。形状改变的帧走 splice：paint 同时记下每个节点的 subtree 区间，一帧只重新 paint 改变的 subtree（新子节点、自身 drawable 改变的节点），保留的子 subtree 原样平移，把改写的段 splice 进 stream（`Painted::Spliced`），renderer 只 ingest 插入的 leaf（Rendering §9.7）。

为什么 transform 不需要 PAINT：transform table 只为 **transform root** 分配条目——scroll container 的内容，以及被 translate 过的节点（首次 translate 时成为 root，此后一直是）。paint 用 `Primitive::Transform(slot)` / `TransformEnd` 括住每个 root 的 primitive，并按先序记录 root，外层 root 总在内层之前。primitive 按绘制时的世界坐标写入，table 同时记下各 root 当时的位移（painted）；之后只有 root 移动的帧，UI 重算 table（每个 root 一次加法，与内容多少无关），renderer 在 encode 时让每个 draw 平移 `current - painted`——平移量走每个 built-in 都已读取的 16 字节 inline uniform 中的 `shift`，root 内打开的 clip 跟着移动。instance 不带 transform id，不重写，不上传：代价按 draw 计，不按 primitive 计，也不需要 storage buffer（GLES 3.0、CpuRaster 同样适用）。不同 root 的 draw 不合批。普通节点的 layout 变化会改变它的世界坐标，这本来就属于 PAINT。

为什么 transform 不需要 HIT_TEST：屏幕上的 rect 不存储，由 `bounds` 加上方 root 的位移推导；hit test 自顶向下携带位移（即把指针按各 root 的逆变换换算），滚动后无需任何重算。

看到什么就命中什么：paint、hit test 与 semantics 用同一条规则。只有 scroll viewport 裁剪内容；不裁剪的容器照常画出越过自身 box 的子节点（比自身矮的 Column、平移出去的子节点），这些子节点也在画出的位置被命中。每个节点存一个 `reach`——自身 box 并上每个显示中、非 overlay 子节点的 reach（按子节点的 translate 平移），scroll viewport 与 hidden 节点的 reach 就是自身 box。layout 在重排的 subtree 上自底向上重算 reach，再沿祖先上传，遇到 box 前后都包住改变量的祖先即停；translate 只做同样的上传（通常第一层就停），不触发 layout。hit test 按 reach 剪枝、按自身 box 判定命中；半透明节点的 `Layer` 覆盖它的 reach，所以淡出中的容器不会裁掉越界的子节点。overlay 画在顶层：hit test 先按上次 paint 的顶层顺序倒序试 overlay（不受上方 scroll 裁剪），再走场景树并跳过 overlay 子节点。hidden 节点不命中；opacity 为 0 的节点仍占位、仍命中。`NodeStore::seen` 给出节点未被任何 scroll viewport 裁掉的部分：辅助技术的点击与 UI scenario 都点它的中心，完全滚出视口的节点报告为未显示。

例外（必须显式处理）：
- **平移无法表达的内容**：offscreen layer（ROI 按绘制时裁剪）、backdrop / material（采样背后内容）、用户 program（自写 uniform）会污染所在 root；移动被污染的 root，或帧内存在 backdrop capture 时移动任何 root，`set_transforms` 返回 `NeedsUpload`，该帧按新位移重新 paint 并上传，结果总是正确；
- **可见集变化**：paint 发出 root 内的全部节点，滚入视口的节点总有 retained paint；VirtualList 的新行走 STRUCTURE/LAYOUT 路径（ADR 0008）；
- **glyph 像素对齐**：位移按 device pixel snap（scale factor 取 length 环境），保证 A8 glyph raster 不因小数偏移失效。当前 transform 只有平移；作者可写 scale / rotate 时扩展 uniform，并遵循 Text 规范的 promotion 规则（§37.3）。

默认 transform parent 跟随 UI parent；overlay、portal、独立 layer 等场景可以显式拥有不同的 transform/paint parent。该能力必须通过受控 API 表达，不能靠任意矩阵引用形成不可追踪图。

目的：滚动和 transform animation 不因层级传播而触发布局。

---

## 20. Transaction 与 batch

一次 input dispatch 中：

```rust
state.a.set(...);
state.b.set(...);
state.c.set(...);
```

只在事件结束时 flush 一次 reactive queue。

API：

```rust
cx.transaction(|tx| {
    tx.set(...);
    tx.set(...);
});
```

普通 setter 默认自动加入当前 transaction，不要求用户每次显式 batch。

### 20.1 Cycle detection

Computed/effect graph 必须有：

- version stamp；
- evaluation stack；
- cycle diagnostic；
- debug source mapping。

开发模式发现循环依赖必须给出完整链路，而不是 runtime hang。

---

## 21. Computed / Effect

### Computed

必须是可缓存、无副作用：

```rust
let full_name = computed!(first, last => format!("{first} {last}"));
```

重新计算只发生在依赖 version 改变时。

### Effect

副作用显式进入生命周期：

```rust
effect!(user_id => async move {
    load_user(user_id).await
});
```

Effect 必须支持：

- cancellation；
- cleanup；
- dependency change restart；
- component unmount cleanup；
- Viso hot reload 时不重复产生无约束副作用。

Render/View 默认保持纯净，不在 view evaluation 内启动 I/O。

---

# Part VII — Layout

## 22. Public layout API 与内部算法分离

用户看到：

```text
Row
Column
Flex
Grid
Stack
Absolute
Scroll
```

布局单位是 `Dp`（density-independent）。公开 API 区分两件事：

**长度族**（DSL 字面量，全部 resolve 为 `Dp`；规范见 `Viso_DSL_1.0.md` §19）：

```text
dp  px  sp  em  %
1px = 1 / scale_factor dp      c sp = text_scale_curve(c) dp（线性平台为 c × text_scale）
1em = 节点 Resolved Font Size   % 按 Property Schema 声明的 percent_basis 解析
```

跨单位加减得到 `MixedLength`，lower 为 `LengthTerms { dp, px, sp, em, pct: F32 }`；没有 min/max/clamp。不支持 rem、vw/vh、ex/ch/lh、物理单位；`fr` 不是长度单位，只是 Grid track 值。Layout 热存储只保存预折叠的 `ResolvedLength { fixed, pct }`；值域、插值与像素对齐见 DSL §19.8–§19.10（ADR 0033）。

**Sizing mode**（每轴如何求尺寸）：

```text
Fixed(length)        // 当前 viso_ui::layout::Length::Fixed
Fill { weight }      // 当前 Length::Fill；Grid 中对应 Fr
Fit                  // 当前 Length::Fit；即 Auto
MinContent / MaxContent / Percent   // 目标
```

Grid track 使用独立的 `TrackSizing { Fixed, Fr, Auto, Percent, Minmax, FitContent, FlexMin }`（ADR 0009）。

hidden 子节点退出 flex 布局：测量为零、不占 Fixed/Fit 主轴空间、不分 Fill 份额、两侧不留 gap（同 CSS `display: none`），布局为容器游标处的零尺寸框。

内部使用高性能专用算法，不默认引入通用 constraint solver。

### 22.1 Constraints

统一基础：

```rust
pub struct Constraints {
    pub min: Size,
    pub max: Size,
}
```

核心协议：

```text
Parent constraints
      ↓
Measure child
      ↓
Resolved size
      ↓
Place child
```

### 22.2 单遍 Cursor Layout 的性能思想

顺序布局的单遍 cursor 思路值得保留为可 benchmark 的内部算法候选，但 public layout API 不绑定具体实现。

对于 Row/Column/Flow，内部仍可使用：

```text
single-pass cursor
remaining space
fit/fill resolution
alignment pass
```

但不把内部 cursor 状态机、特殊 batch 或复杂约束约定作为普通用户必须理解的 API。

> **公开稳定布局语义，内部算法以 benchmark 选择。**

---

## 23. Measure cache

Node layout cache key 至少包含：

```text
constraint hash/version
style layout version
content version
font/text version
child intrinsic version
```

未变化时直接复用。

不要仅以“前一帧已经计算”作为 cache 条件。

### 23.1 Layout boundary

提供类似 contain/layout-boundary 的内部概念：

当一个容器尺寸固定且子节点变化不会影响父级 intrinsic size 时，dirty propagation 可以停止。

这对复杂桌面 UI、大列表、编辑器非常重要。

---

## 24. Virtualized List

`List` 必须作为一等高性能组件，而不是普通 Column 的包装。

需要：

- visible range 计算；
- item key；
- node recycling；
- estimated extent；
- variable height cache；
- anchor preservation；
- scroll correction；
- incremental create/destroy；
- accessibility virtual range strategy。

目标：100k/1M logical items 不对应 100k/1M mounted Nodes。

---

# Part VIII — Input、Focus 与 Gesture

## 25. 事件路由

原始事件：

```text
PlatformEvent
    ↓
Input Normalization
    ↓
Hit Test / Focus Target
    ↓
Capture
    ↓
Target
    ↓
Bubble
```

### 25.1 Typed event

内部可以统一 envelope，但组件 API 尽量 typed：

```rust
fn on_pointer_down(&mut self, cx: &mut EventCx, event: PointerDown);
fn on_key_down(&mut self, cx: &mut EventCx, event: KeyDown);
```

避免每个组件都拿一个万能 `Event` 后自行遍历 match 全世界。

### 25.2 Pointer capture

必须内建：

- pointer capture；
- drag threshold；
- cancellation；
- multi-touch identity；
- hover enter/leave；
- wheel/trackpad precision。

**光标**：节点经 `NodeStore::set_cursor` / `BuildCx::cursor` 命名鼠标在它及其子树上显示的光标（`CursorIcon`），离鼠标下节点最近的命名者胜出，捕获期间保持捕获开始时的光标。光标可以是系统形状，也可以是应用注册的图像：`CustomCursor::new(CursorImage)` 把直通 alpha 的 sRGB RGBA 像素（每边至多 `MAX_CURSOR_SIDE` = 256 像素）、热点与“每逻辑点几像素”的 scale 登记进进程级注册表，换回一个紧凑 id（`NonZeroU32`，`CursorIcon::Custom(id)` 仍是 `Copy + Eq + Hash`）；同一图像再登记得到同一 id，重建视图不增长注册表。`viso::cursor::decode` 把编码图像（PNG 等资源格式）读成 `CursorImage`。backend 在某窗口第一次显示某图像光标时才从注册表取像素构建 OS 光标并缓存（按 id 下标的稠密表 `CursorCache`，失败也记下不再重试），之后只是换用：macOS `NSCursor initWithImage:hotSpot:`（sRGB bitmap，按 scale 定点尺寸），Windows `CreateIconIndirect`（32 位 alpha 位图，按窗口 DPI 各建一份），Wayland 共享内存 ARGB buffer 的光标 surface（整数 buffer scale，补齐到整点），X11 Render `CreateCursor`（ARGB32 picture，按窗口 scale），KMS 硬件光标 buffer（预乘、随面板旋转），Web CSS `url(data:image/png…) x y` 加 `image-set` 高密度版本（浏览器读不懂的值不覆盖前一个），Android `PointerIcon.create(Bitmap, x, y)`（activity 按 id 保留 icon，像素只传一次）；iOS 没有图像光标。`PlatformCaps::custom_cursors` 为 false 的平台以默认箭头代替。光标变化在输入路由后立即经 `FrameDriver::after_input` 推给平台，不等下一帧（只移动鼠标、其余不变的输入不会请求帧）；帧尾同样同步一次，只在变化时调用 `PlatformApp::set_cursor`。

**指针锁定**：节点经 `NodeStore::lock_pointer(id, true)` / `EventCx::lock_pointer(id)` 请求把鼠标指针锁给自己（游戏视角、3D 视口环绕），`unlock_pointer` 放开。平台接口是 `PlatformApp::set_pointer_lock(window, lock)`，能力位 `PlatformCaps::pointer_lock`；锁定期间光标隐藏并停在原处，鼠标的移动不再以 `RawEvent::Pointer` 移动报告，而是 `RawEvent::PointerMotion { window, dx, dy }`（逻辑点，右、下为正，屏幕边缘仍持续；backend 可把队列尾部同窗口的相邻 motion 相加合并），按钮与滚轮照常到达（位置停在锁定点）。每次锁定状态变化（应用放开、窗口失焦、浏览器 Escape、平台拒绝）报告为 `RawEvent::PointerLockChanged { window, locked }`。runtime 把 motion 换成物理像素的 `InputSample::Motion`，锁定变化经 `FrameDriver::on_pointer_lock` 交给 driver。UI 侧 `NodeStore` 只记一个冷槽：请求者、平台是否持有锁、待发的请求；facade 在 `after_input`（锁请求仍在按下事件的处理中，满足浏览器的用户手势要求）与帧尾取走请求交给平台，平台没有该能力时立即以“锁已结束”回答。锁定期间鼠标的样本整个路由给持锁节点（不做命中测试、悬停与捕获），motion 以逻辑点增量交给持锁节点的 key handler（`EventCx::pointer_motion`），锁的生效/结束同样交给它（`EventCx::pointer_lock_change`）；Escape、窗口失焦、持锁节点被释放都会结束锁并告知持锁节点一次。macOS：只在应用活跃且窗口为 key 时锁定，把光标移到视图在屏幕可见区内部分的中心、事件抑制间隔置零、`CGAssociateMouseAndMouseCursorPosition(false)` 冻结光标并 `NSCursor hide`，移动事件读 `deltaX/deltaY`（扣掉移位本身带进下一个硬件增量的距离），光标若被系统放回关联则移回锁定点，失焦/关窗时恢复。Web：`canvas.requestPointerLock()`（拒绝的 promise 被吞掉，`pointerlockerror` 报告结束），`pointerlockchange` 以 `document.pointerLockElement` 是否为 canvas 报告状态，锁定期间 `pointermove` 读小数 `movementX/Y`。Windows：只在窗口是前台窗口时锁定，`RegisterRawInputDevices` 注册原始鼠标输入、`ClipCursor` 把光标夹在客户区中心的一个像素上并清空光标形状，`WM_INPUT` 的相对计数（一计数按一物理像素）换成逻辑点，`WM_MOUSEMOVE`/`WM_MOUSELEAVE` 锁定期间不再报告，窗口移动/缩放时重新夹住，`WM_KILLFOCUS`、销毁时放开。Wayland：需要 compositor 同时提供 `zwp_pointer_constraints_v1` 与 `zwp_relative_pointer_manager_v1`（据此报告能力位），只在指针位于窗口且键盘焦点在窗口时请求 oneshot `lock_pointer`，`locked` 事件才算生效并隐藏光标，`relative_motion` 的加速增量即 motion，`unlocked`、键盘离开、seat 失去指针时结束。X11：只在窗口持有键盘焦点时锁定，`XIGrabDevice` 以隐形光标抓取主指针，根窗口上为主设备选择 `XI_RawMotion`，原始事件的处理后数值（valuator 0/1）即增量，每次之后把指针移回窗口中心，锁定期间窗口自身的 motion/leave 只是移回中心的回声（滚动 valuator 照常报告）；服务器对抓取设备的客户端会把同一个原始事件送两次，按时间与序号去重；失焦、关窗时放开。KMS：libinput 的相对移动在锁定时直接成为 motion，光标不动并隐藏，会话暂停时结束。Android（API 26）：`View.requestPointerCapture()`（窗口无焦点或系统版本不足时立即报告拒绝），`onCapturedPointerEvent` 把 `SOURCE_MOUSE_RELATIVE` 的增量（含历史样本）交给 native，按键以锁定前的鼠标位置照常作为鼠标样本，`onPointerCaptureChange` 报告状态。iPadOS 14：视图控制器的 `prefersPointerLocked` 跟随请求（`setNeedsUpdateOfPrefersPointerLocked`），`UIPointerLockStateDidChangeNotification` 读场景 `pointerLockState.isLocked` 报告状态，锁定期间为 GameController 的每个 `GCMouse.mouseInput` 设 `mouseMovedHandler`（y 轴取反）；没有连接鼠标时立即拒绝；能力位只在 iPad 上为真。

### 25.3 Gesture arena

Touch/mobile 需要标准 gesture arbitration：

```text
Tap
LongPress
Pan
Scroll
Pinch
Rotate
Custom
```

父 Scroll 与子拖拽控件冲突必须有框架级规则，而不是组件各自 hack。

---

## 26. Focus / Keyboard / IME

Focus 独立建模：

```text
FocusTree
TabOrder
FocusScope
KeyboardFocus
TextInputFocus
```

IME 不应只是 TextInput 的平台 patch。

需要：

- composition range；
- candidate rect；
- selection；
- surrounding text；
- platform IME activation/deactivation；
- virtual keyboard inset；
- keyboard avoidance integration。

---

# Part IX — Style、Theme 与 Animation

## 27. Style 不是运行时字符串字典

开发阶段可以写：

```viso
background: theme.colors.surface;
radius: theme.radius.md;
```

编译后：

```text
PropertyId(17)
TokenId(8)
```

热路径不做字符串比较。

### 27.1 Style pipeline

```text
Declared Style
    ↓
Token Resolution
    ↓
Variant / State Resolution
    ↓
Computed Style
    ↓
Style Diff
    ↓
DirtyClass
```

普通帧若 state/theme 未变化，不重新 cascade/resolve。

### 27.2 Theme token

建议一等 token：

```text
color.*
typography.*
spacing.*
radius.*
elevation.*
motion.*
```

Widget 默认引用语义 token，不直接固化 desktop dark/light 颜色。

---

## 28. Animation

Animation 不能要求每帧重建 style tree。

编译为：

```text
AnimationTrack {
  target_property,
  start,
  end,
  curve,
  duration,
}
```

对 transform/opacity/color 等属性使用 specialized evaluator。

animation dirty 按属性限定：transform 动画只标 `TRANSFORM`，opacity/color 动画只标 `PAINT`；只有 width/height 等 layout property animation 才标 `MEASURE | LAYOUT`。

transform / opacity 动画的 evaluator 在 AdvanceAnimations 阶段直接写入 TransformStore / PaintStore 中的单个字段，不经过 binding 求值。

**缓动**（`viso_math::Easing`，`viso_ui` 在原路径 re-export）：`Linear`、Penner 十族曲线（`Curve::{Quad, Cubic, Quart, Quint, Sine, Expo, Circ, Back, Elastic, Bounce}`）各取 `In`/`Out`/`InOut`，与 CSS `CubicBezier`（Newton 求解、二分兜底）；`EASE_IN`/`EASE_OUT`/`EASE_IN_OUT` 为三次族。曲线是无分配的纯函数，放在叶子 `viso-math`，运行时与编译器（Web 导出）共用同一实现；`Easing::named` 给出 DSL `Easing` 声明变体的顺序。会越界的曲线（`Back`、`Elastic`、越界的贝塞尔）让平移、缩放、旋转越过目标，颜色与不透明度在插值处夹回取值范围，尺寸与圆角不小于 0。Web 导出：静态 CSS 由编译器把曲线自适应采样为误差 ≤0.2% 的 `linear()`，页面脚本带同一组曲线的 JS 实现，首次用到时同样采样并缓存（测试经 `node` 比对两份实现）。

**弹簧**（`viso_math::Spring`，`viso_ui` re-export）：`stiffness`/`damping`/`mass` 的阻尼谐振子，欠阻尼、临界与过阻尼三种情形各用闭式解给出任意时刻的位移与速度（不做逐帧积分，结果与帧率无关）；`with_response(response, ratio)` 按无阻尼周期与阻尼比构造，`SMOOTH`/`SNAPPY`/`BOUNCY` 为半秒周期、阻尼比 1/0.85/0.7 的预设；阻尼比下限为 0.01，因此每个弹簧都会停下。`Timing` 由 `delay` 与 `Pace`（`Eased { duration, easing }` 或 `Spring`）组成。外观转场把值拆成至多四个分量（颜色取预乘线性光的四通道，平移取两轴，圆角取 `dp` 与 `pct`），每个分量从起点偏移与初速度独立求解，夹取规则与缓动插值相同；判定收敛用能量界 `x² + (v/ω₀)² ≤ rest²`：弹簧只会损失能量，满足后位移不会再超过 `rest`，此时写入目标值并移出列表（帧循环随之停下）。转向时由 `LookTransition::velocity` 求出当前各分量速度（弹簧取闭式解的导数，缓动取曲线在当前 1 ms 内的斜率），交给新的弹簧移动：`NodeStore::transition` 转向、`resume_transition` 跨重建续上、`animate` 取代同一属性的运行中动画时都这样交接；有 `delay` 或按时长缓动的新移动从静止开始。勾选标记的进度（`LookValue::Check`，0 为未选、1 为选中，存在标记旁）也是外观值：`NodeStore::set_check_mark` 在同一形状换状态时把它过渡到新状态（方框、圆与 V 形 150 ms ease-out，开关为 0.28 s 周期的临界阻尼弹簧），每帧只标 PAINT，绘制按进度插值；标记跟随节点语义状态（V 形随 `expanded`，单选的圆随 `checked` 或 `selected`，其余随 `checked`），因此投影语义状态的控件无需另行驱动。Web 导出：编译器与页面脚本（同一闭式解的 JS 实现，经 `node` 与 Rust 比对）把弹簧从静止到收敛至千分之一以内的移动采样为 `linear()`，以收敛时间作为 `transition`/`animate` 的时长。

**关键帧**（`viso_ui::Keyframes`）：`stops: Vec<Keyframe { at, value, easing }>`、每周期 `duration`、`delay`、`Repeat::{Times(n), Forever}`、`PlayDirection::{Normal, Reverse, Alternate, AlternateReverse}`。`NodeStore::play` 与 `animate` 共用 `run_animation`：同样只接受平移、缩放、旋转与不透明度，同样移动呈现值、结束时显示 Model 值并经 `ANIMATION_END` 通知结束（`target` 为最后一个停靠点的值），同一属性的新动画结束旧动画。运行中的 `NodeAnimation` 持有 `Run::Move(LookTransition)` 或 `Run::Track(TrackRun)`：`TrackRun` 在开始时把停靠点过滤为首个停靠点的属性、丢弃非有限的 `at`、夹到 `[0, 1]` 并稳定排序，之后每帧只做一次查找与插值、不分配；时间以 `f64` 秒累计，周期数取整数部分，按方向决定是否倒放，第一个停靠点之前从开始时的呈现值插入，最后一个之后保持。被弹簧动画取代时，轨道的当前速度按最近 1 ms 的差分交出。Web 导出见 DSL §U6.3。

**减少动态效果**：`Timing`、`Keyframes` 各带 `ReducedMotion::{Instant, Keep}`（默认 `Instant`）。`NodeStore::set_reduced_motion` 由 facade 在系统外观变化时与 `env.reduced_motion` 一同写入，是唯一执行策略的地方：`transition`、`animate`、`play` 遇到 `Instant` 时间立即落到终值或以 finished 结束；`glide_to`（平滑 `scroll_to`/`scroll_by`/`scroll_into_view`、键盘滚动、跟随末尾、刷新保持位）直接 `set_scroll`，虚拟列表的平滑定位改走即时定位；`AnimationRegistry::tick` 让 `TranslateAnim` 在当帧到位。惯性滚动、越界回弹与吸附滑行（松手后的直接操控延续）不受影响。开启时 `set_reduced_motion(true)` 把进行中的可省略转场写到终值、结束可省略的动画、把所有滑行落到目标。视图不再自行读取 env 决定是否插值，只把 `reduced` 字段传给 Store。

**页面转场**（`viso_ui::navigation`）：分页栈 `NodeStore::set_page(stack, page, PageTransition)` 只显示第 `page` 个子节点，其余子节点像折叠（`fold`）一样由 Store 侧谓词 `paged_away` 移出布局、绘制、命中测试和语义树——之后挂上的子节点同样折叠，子节点自己的 `hidden` 不被覆盖；冷表 `Paged` 只在有分页栈时多一次判空。换页时向更低的页为返回、否则为前进：新页立即显示，旧页移出到位后折叠；第一次分页立即生效。两页都以呈现值动画（平移、不透明度，`NodeAnimation` 标记 `quiet`，不派发 `animation_end`）移动，不触发布局；进行中再次导航时各页从当前呈现值继续（弹簧带速度），被取消的动画不隐藏页面。`PageTransition`：`Native`（Apple 为 `Slide`、Android 为 `SharedAxis`、其余为 `Fade`）；`Slide` 新页从尾边整宽滑入、被覆盖页反向漂移三成宽度（临界阻尼弹簧，半秒周期），返回时相反；`SharedAxis` 两页沿行进方向各移 30dp 并交叉淡入淡出（300 ms，`cubic-bezier(0.2, 0, 0, 1)`）；`Fade` 只淡化上层的页（前进时新页淡入、返回时旧页淡出），栈不透出背景；`None` 立即交换。RTL 栈的尾边在左。减少动态效果时 `Slide`/`SharedAxis` 改为 `Fade`（`ReducedMotion::Keep`，淡入淡出仍保留）。离开中的页记入冷列表 `leaving`（谓词视其为仍显示），命中测试跳过它，到位时移出并折叠。`EventCx::set_page` 以 `StoreRequest::Page` 延后执行，`BuildCx::page` 在构建时分页。`viso_widgets::NavigationStack` 用裁剪的分页 `stack` 容器承载各页，`NavigationStackStyle::transition` 默认 `Native`；DSL 的 `NavigationStack`（`ControlKind::Pages`）以 `current` 分页、`page_transition` 换页，返回手势投递 `current_changed`（`EventRoute::CurrentChanged`）。系统返回动作是 `Key::Back`（鼠标后退键、Android 返回、浏览器后退键）：未被焦点链处理时门面先以 Escape 的方式关闭最内层瞬态层，否则 `NodeStore::back_target` 取最深的、正在显示且不在首页的分页栈，`KeyRouter::route_back` 把它作为目标投递。共享元素（hero）：`NodeStore::set_hero(id, tag)` 记入冷表；换页时记下待飞的一次转场，到达页布局之后（`layout`/`relayout_dirty` 末尾的 `fly_heroes`）为到达页中每个与离开页同标签的显示节点起两段静默动画：平移起点 `源中心 − 上方变换平移 − 原点 − s·(自身中心 − 原点)`、缩放起点 `s = 源宽 / 自身宽`，与页面同一 `Timing`，故其世界位置沿直线移动；源节点以一段全程为 0 的不透明度关键帧隐藏，下次换页时结束这些保持。减少动态效果时不飞。

---

# Part X — Accessibility / Semantics

## 29. Semantics 从第一天进入 Node

每个可访问节点拥有：

```rust
struct Semantics {
    role: Role,
    label: TextRef,
    value: TextRef,
    description: TextRef,
    state: SemanticsState,
    actions: SemanticsActions,
}
```

Semantics tree 可以与 UI tree 不完全一一对应：

- 装饰节点可以跳过；
- 多个视觉节点可以合并；
- 虚拟列表需要虚拟语义节点策略。

平台 adapter：

```text
macOS/iOS accessibility
Windows UI Automation
Linux AT-SPI
Android Accessibility
Web accessibility bridge
```

### 29.1 性能

Semantics 只在 dirty branch 更新，不在每帧重建整棵 accessibility tree。

---

# Part XI — Paint / Renderer

## 30. UI 不直接提交 GPU 命令

UI 只产生/更新 PaintNode 或 Primitive 数据。

推荐：

```text
UI Node
   ↓
Retained Paint Cache
   ↓
Changed Primitive Ranges
   ↓
Batch Builder
   ↓
Frame Packet
   ↓
GPU
```

### 30.1 为什么不是每帧完整 DisplayList 重建

可以保留 display-list-like representation，但对性能敏感的 retained UI，最好支持：

- 未改变节点复用 paint data；
- stable primitive range；
- subtree paint cache；
- partial instance buffer update；
- clip/layer versioning。

### 30.2 Primitive 分类

ADR 0001 固定的最小 primitive 合同是 `Quad` / `GlyphRun` / `Image` / `Path` / `Mesh` / `Layer`（§15.3）。当前 `viso_render::Primitive` 在此之上增加了 analytic shape（RRect/Ellipse/Capsule/Line）、Gradient、AnalyticShadow、material 等 lane；完整分类、lane 选择与 Layer 合成（opacity == 1 用 scissor clip，< 1 走 offscreen + composite，ADR 0002）见 `Viso_Rendering.md`。

### 30.3 Damage-driven present（所有 Device Class）

局部变化只重绘、只上屏受影响的区域。这对桌面/移动是功耗问题，对嵌入式是内存带宽问题（RGB565 800×480 全屏重绘约 750 KB/帧）。

- **damage 来源**：每次 upload 后，renderer 为 surface pass 的每个 paint-order 条目生成 64 位内容 key（retained store 条目的全部像素决定字段、lower 时的 context：origin / space / clip、所采样的 offscreen pass 内容与 pass 参数（rect、opacity、blur、color op、blend）、backdrop capture 下方内容、material 参数、program 读的时钟）与可触及的 rect。Submit 时按 transform root 的当前位移与移动 clip 放置，再与上次 present 的列表按序 diff：相同的前缀/后缀，中间 32 条窗口内重新同步，所以插入/删除一行只 damage 那一行。每个未匹配条目贡献它的旧/新 rect，外扩 1px 抗锯齿边缘。只移动 root 的帧（§19.2）因此 damage = root subtree 旧 ∪ 新 world bounds，且被 clip 限定。
- **合并**：合并为最多 4 个不相交矩形（每次挑并集面积增量最小的一对合并）；合并后面积超过 surface 的 60% 时退化为整帧重绘。size、clear color 变化、`set_partial_redraw`、`damage_all()`（surface 重建、纹理就地改写）都强制整帧。
- **GPU 路径**：backend 报告 drawable 的 buffer age（`GpuBackend::frame_age`：D3D12 flip-sequential、Vulkan 按 image 计数、`EGL_EXT_buffer_age`、CpuRaster/Headless）。age ≥ 1 时，surface pass 用 `LoadOp::Load` 保留像素，renderer 保留最近 4 帧 damage，重绘区 = 本帧 ∪ drawable 错过的 age − 1 帧；每个矩形先用 Replace blend 的 clear quad 清空，再把全部 surface draw 以「原 scissor ∩ 矩形」重放。age 未知（0，Metal / WebGPU 的 drawable）时整帧重绘，但仍然跳过空 damage、上报 damage。present 时附带 damage（`present_with_damage`：DXGI `Present1` dirty rects、`VK_KHR_incremental_present`、`EGL_KHR_partial_update`、KMS `FB_DAMAGE_CLIPS`），compositor 据此只更新这些区域。局部重绘与整帧重绘逐像素一致（`crates/render/tests/damage.rs`）。
- **CPU 路径**：只光栅化与 damage 相交的 tile（§30.4）。
- **空 damage 不 Submit**：不 begin frame、不 encode、不 present，GPU 不被唤醒，KMS 也不 page flip。
- **调试**：profiler counter `damage_px` / `damage_rects` / `full_frame_fallbacks`（`viso profile`）；`viso inspect frame` 的 `damage` 给出该帧重画的区域（`null` / `"whole"` / 矩形列表），Studio/Inspector 据此叠加 damage rect。`Renderer::last_damage()` / `damage_stats()` 是同一数据的 Rust 入口。

### 30.4 CpuRaster 后端

`CpuRaster`（`viso_gpu::cpu`）是 Embedded-CPU 的 Tier-1 后端，同时是 Web 无 WebGPU 时的兼容路径（canvas `putImageData`），以及 golden test / headless 的确定性参考。它是唯一的 CPU 后端，不另起一套 renderer。

- 消费与 GPU 完全相同的 frame packet / draw list，不新增 primitive 类型；每个内置 pipeline 由 `BuiltinShader` 标签选择其 shader IR 的 CPU 逐操作复述（SDF、AA ramp、采样约定），pipeline 的 blend state（`Replace` / premultiplied over）照常生效；
- **按格式存储**：8-bit 目标按格式字节序存四字节（`Bgra8Unorm` 即 XRGB8888/ARGB8888 扫描布局，可直接作为 KMS dumb buffer），合成走整数 src-over（`div255` 精确舍入，同 8-bit attachment）；`Rgb565Unorm` 存 16 bit、不透明，gradient 走 4×4 Bayer ordered dither；HDR 目标存 `f32`；glyph / mask atlas（A8）存一字节每 texel，就是系统内存中的 A8 cache，residency 合同与 GPU atlas 相同；
- **tile**：64×64，draw 按 tile 分 bin（保持提交顺序），只访问被 draw 触及的 tile —— `LoadOp::Load` 的 partial redraw（§30.3）只花 damage 的代价；一行 tile 为一个 band，band 之间像素不相交，`workers > 0` 时在常驻 worker pool 上并行，输出与 worker 数无关；
- **SIMD**：行合成与打包有 SSE2 / NEON / SIMD128 kernel，与 scalar 定义 bit-exact（单元测试穷举 / 随机对比，golden 共用一套）；
- **analytic AA**：RRect / Ellipse / Capsule / Line / Shadow 在像素中心求 SDF 覆盖；path 填充走 sparse scanline coverage —— 后端声明 `Caps::coverage_paths`，renderer 把所有实心 path（凸的也一样）走 mask lane，coverage 每个轮廓只光栅一次、按 glyph 合成；描边仍走三角形 lane；
- **layer**：只被区域合成（Image / ColorTransform、clamp-to-edge）读取的 offscreen pass 被 defer：只分 bin、不分配目标；合成它的每个 tile 把要读的 texel 区域（+1 texel 边距）求值到 worker 本地窗口再采样，所以 opacity < 1 / 融合色彩滤镜的 group 只占一个 tile 的 scratch，不分配整块 offscreen，像素与整目标绘制 bit-exact。被 blur、advanced blend、backdrop 或用户 program 读取的 layer 照常整块绘制。无 alpha 的面板（RGB565）的中间目标为 `Bgra8Unorm`；
- **`Blitter` capability**：2D 加速器（i.MX PXP、Allwinner G2D、STM32 DMA2D 类）实现 `viso_gpu::cpu::Blitter`，raster 把 solid fill（pass clear）交给它，它拒绝的部分由 CPU 完成，像素不变；
- **自定义 shader**：每个 brush program 在打包时都带有 `ShaderLang::None` 代码（编译成扁平四 lane 寄存器码的参考解释器，`CpuShader`），`CpuRaster` 直接运行它，不存在可遗漏的 `cpu_fallback`；`embedded-cpu` profile 把包的 shader 语言固定为 `None`（§50A.3），缺少 CPU 代码的包在构建期即不满足 profile。从 Shader IR 构建期生成 Rust 片元函数（ADR 0044）是后续性能目标；
- **测量**：`pixel_bytes()`（目标与 surface 像素）、`scratch_bytes()`（worker 窗口）、`deferred_layers()`；bench `cpu_raster_800x480_{full,damage_5pct}_{xrgb8888,xrgb8888_4t,rgb565}`。

---

## 31. Batching

用户绝不应该设置 `new_batch: true` 才保证普通文字出现在背景之上。

Renderer 自动根据：

```text
pipeline key
texture/sampler
clip/layer
blend mode
render target
depth/stencil requirements
```

生成 batch。

### 31.1 Batch key

```rust
struct BatchKey {
    pipeline: PipelineId,
    texture_set: TextureSetId,
    clip: ClipId,
    target: TargetId,
    blend: BlendMode,
}
```

尽量使用整数 ID / packed key。

### 31.2 Stable batching

避免为了减少 draw calls 而破坏 z-order。

采用：

- order-preserving adjacent batching；
- layer-based grouping；
- 在明确安全条件下做跨节点 merge。

正确性优先于极端 draw-call 数字。

---

## 32. GPU instance buffer

典型数据：

当前 derive 名为 `#[derive(GpuPod)]`（`viso-macros`），ADR 0001 称之为 `GpuInstance` ABI：

```rust
#[repr(C)]
#[derive(GpuPod)]
struct QuadInstance {
    rect: [f32; 4],
    color: [f32; 4],
    radius: [f32; 4],
    border: [f32; 4],
}
```

宏必须生成明确 layout descriptor，并验证 shader 声明。

instance 不携带 transform：`rect` 是绘制时的世界坐标，所属 transform root 之后的位移由 draw 的 `shift` uniform 施加（§19.2）。因此滚动和 transform 动画不需要重写任何 instance，每个 instance 也不为此多付字节。

禁止依赖：

> “某个 Rust struct 从某字段开始后面的内存刚好全部可作为 GPU instance。”

### 32.1 Host / GPU data 分离

复杂 Painter：

```rust
struct ButtonPainter {
    pipeline: PipelineId,
    cache: ButtonPaintCache,
}

#[repr(C)]
#[derive(GpuPod)]
struct ButtonInstance {
    rect: Vec4,
    color: Vec4,
    radius: f32,
    hover: f32,
}
```

这是推荐默认。

---

# Part XII — GPU RHI 与 Shader

## 33. GPU abstraction

`viso-gpu` 提供小而硬的 RHI：

```text
Device
Queue
Buffer
Texture
Sampler
Pipeline
BindGroup
CommandEncoder
Surface
Fence
```

不要把 UI 概念放进 GPU crate。

### 33.1 Backend 静态选择

默认：

```text
macOS/iOS → Metal
Windows   → D3D12（必要时兼容路径）
Linux     → Vulkan
Android   → Vulkan
Web       → WebGPU
Embedded Linux（GPU）   → GLES 3.0+（Vulkan 1.1 驱动成熟的 SoC 可选 Vulkan）
Embedded Linux（无 GPU）→ CpuRaster
其它      → CpuRaster（由 CpuRaster 升级，测试/无原生后端）
```

Desktop Linux 与 Embedded Linux 的 target triple 相同（例如 `aarch64-unknown-linux-gnu`），因此 embedded backend 由 Footprint Profile 的 feature 选择，而不是由 `cfg(target_os)` 选择；仍然是编译期静态选择，一个 artifact 只含一个 backend。

GLES 3.0 是 Embedded-GPU 的 Tier-1：嵌入式 SoC（Mali、Vivante、PowerVR、VideoCore 等）的 GLES 驱动普遍比 Vulkan 驱动成熟。它在 Embedded-GPU 上是 Tier-1，在桌面 Linux 上只是 Tier-2 兼容路径。这不违反「不做最低公分母」：GLES3 backend 声明自己的 capability profile（无 compute、bind group 由 backend 模拟），renderer 按 capability 选择 lane，Tier-1 RHI 的语义不因此降级。

`Gles3Backend`（`viso-gpu` 的 `gles` feature，Linux / Android 上替代 Vulkan 成为 `Backend`）的实现合同：

- EGL 1.5 与 GLES 3.0 运行时加载（`libEGL` / `libGLESv2`，Wayland 另加 `libwayland-egl`），不链接；display 与 context 在第一个 surface 上按其平台创建（Wayland、XCB、Android、无窗口时 pbuffer）。
- shader：GLSL ES 3.00，一份源码含两个 stage，vertex 半部在 `VISO_VERTEX` 下，backend 各加 `#version 300 es` prelude 编译两次。built-in 由 `viso-shader` 从 shader IR 打印（`emit_glsl`），用户 program 由 program codegen 打印（`ShaderLang::Glsl`），两者都过 glslang 的 GLSL ES 3.00 检查。GLSL ES 不做隐式转换，整数字面量按上下文类型拼写。
- uniform：built-in 读 `std140` 的 `ViewBlock`（binding 0）；一帧所有 draw 的 inline uniform 打进一个 uniform ring，按 `UNIFORM_BUFFER_OFFSET_ALIGNMENT` 对齐、按 range 绑定。
- instance：built-in 用 divisor 1 的 vertex attribute（每个 draw 重设指针到 `instance_offset`）；用户 program 没有 storage buffer，instance 作为 `uvec4` 字读自 binding 1 的 uniform block，每块 16 KB（GLES 3.0 保证的最小 block 大小），`glCopyBufferSubData` 拷进 ring 中各自的槽，`viso_base_instance` 给出块首索引。
- bind group：texture 依次绑到 texture unit，sampler 状态是绑到同一 unit 的 sampler object。
- 方向：texture target 的 clip-space y 取反（`viso_target`），行仍自上而下存储，`v = 0` 采样顶行，与其它 backend 一致；fragment 位置由 `gl_FragCoord` 还原成左上原点。
- `Bgra8Unorm` 纹理存为 `RGBA8`（核心 GLES 无 BGRA 存储），上传时交换 R/B。
- 用户 program 在共享对象的第二个 context 上编译，编译器可在任意线程运行，结束后恢复调用线程原有的 current context；vertex array 不共享，安装时在主 context 创建。
- 局部刷新：`EGL_EXT_buffer_age` 给出 `frame_age`；`EGL_KHR_partial_update` 在第一笔绘制前声明本帧的 surface pass 的 scissor 区域（tiler 只加载这些 tile）；`eglSwapBuffersWithDamage{KHR,EXT}` 上屏 damage。

选择由 `viso_gpu::Backend` 类型别名按 `cfg` 在编译期完成，facade 按值持有，帧路径无 `dyn` 分派；详见 [ADR 0029](docs/adr/0029-gpu-backend-static-selection.md)。

源码层可以：

```rust
trait GpuBackend {
    type Buffer;
    type Texture;
    type Pipeline;
    ...
}
```

但 release target 应尽量 monomorphize/静态选定 backend。

禁止每个 primitive 在 runtime 上通过 `Box<dyn Backend>::draw()` 动态分派。

### 33.2 不追求最低公分母

统一 API 只统一真正共性的资源与同步模型。

平台特殊能力通过 capability：

```rust
if device.caps().supports_argument_buffers() { ... }
```

或 backend-specialized fast path，而不是为了抽象漂亮放弃性能特性。

---

## 34. Render Graph

复杂 pass 使用 render graph：

```text
UI Main Pass
Shadow/Blur Pass
Offscreen Layer
3D Scene Pass
Composite Pass
Present
```

Graph compiler 负责：

- resource lifetime；
- transient texture reuse；
- pass dependency；
- barrier/synchronization；
- attachment aliasing（backend 允许时）。

普通 UI 不要求业务开发者接触 render graph。

---

## 35. GPU memory / upload

使用：

- persistent buffers；
- ring upload buffer；
- staging allocator；
- texture atlas；
- glyph atlas；
- image cache；
- pipeline cache；
- descriptor/bind cache。

稳态滚动列表应优先更新少量 instance range，而不是每帧重新创建 GPU buffer。

---

## 36. Shader compiler

管线：

```text
Shader Source
   ↓
Lossless/parsed AST
   ↓
Typed Shader IR
   ↓
Validation + Optimization
   ↓
Backend Codegen
   ├── MSL
   ├── HLSL/DXIL path
   ├── SPIR-V
   └── WGSL
```

Shader 与 Viso DSL 可以共享：

- diagnostics；
- source map；
- interned symbols；
- module infrastructure；

但不应强制共享同一运行时 VM。

### 36.1 ABI 验证

编译阶段验证：

- instance field offset；
- alignment；
- scalar/vector type；
- uniform size；
- texture/sampler binding；
- vertex format；
- shader-stage visibility。

Mismatch 在编译/热重载时失败，不允许 silent memory corruption。

---

# Part XIII — Text 系统

## 37. Text 是独立性能子系统

完整规范：`Viso_Text_Font_Runtime.md`（以下简称 Text 规范）。本节只列不可违反的合同；细节、数值与 benchmark 以 Text 规范和 ADR 0025–0028 为准。

### 37.1 字体来源与解析

- **不建立 framework-owned FontDB。** 普通启动与普通文字绘制不得扫描、枚举或解析整台机器的系统字体。完整枚举只允许作为显式请求的异步 cold service（`SystemFontCatalog`，例如 Font Picker）。
- 解析顺序：`FontRequest → Resolve Cache → App packaged/dynamic fonts → OS fallback → Missing Glyph policy`。App 字体优先，缺字继续进入系统 fallback。
- 系统字体经 `viso-text` 定义的 `SystemFontProvider` / `ColorGlyphRasterizer` trait 按需查询，facade 提供实现（当前 macOS CoreText；Windows/Linux/Android 为目标）。negative cache 与 fallback 顺序在 `viso-text`；禁止 `text -> platform`（ADR 0026）。
- Native 可以完全不打包字体；WASM/Canvas 没有隐式系统字体，无字体时 shape 为空而不 panic。
- `assets/fonts/` 在 build time 自动登记为 compact `FontManifest`，runtime 首次使用才 lazy load；网络/用户/文档字体经显式 `FontProvider` 注入。
- **输入只接受 SFNT（TTF/OTF/TTC/OTC）。** 框架不解码 WOFF2；调用方先解压（例如 `libs/woff2`）（ADR 0028）。

### 37.2 Fallback 与 Emoji

- CJK/混排 fallback 按 shaping run / cluster 进行，禁止逐字符扫描字体；`zh-Hans`/`zh-Hant`/`ja`/`ko` 是 fallback key 的一部分；首次 OS fallback 结果进入 bounded `FallbackPlan` cache。
- Emoji grapheme/ZWJ/VS16/肤色/旗帜/keycap 序列保持原子，不拆到多个字体。

### 37.3 Glyph representation

五种 `GlyphImageKind`：`MaskA8`（默认，稳定 UI/小字/CJK）、`ScalableMtsdf`（持续 zoom/scale/rotation 时异步 promotion）、`OutlineVector`（极端放大/高精度，retained）、`ColorRgba8`、`ColorVector`。硬规则：

- 变换稳定后异步回到精确 A8 Coverage；promotion/settle 有 hysteresis；纯 scroll translation、opacity、color、clip 不触发 promotion；
- 禁止 FontFace load 时预生成 distance field，禁止为 CJK 整套字体预生成；
- 各 representation 独立 residency pool 与 accounting；representation 决策挂在 retained run 上，不逐帧逐 glyph 重算。

### 37.4 Cache 边界

四层独立 cache（ADR 0025）：Font Resolve Cache、byte-budgeted Segmented LRU Font Face Cache（recency 不逐 glyph 更新）、Shaping/Paragraph Cache、按 page 做 frame-age/CLOCK 淘汰的 Glyph Atlas（禁止逐 glyph LRU 与正常压力下 `atlas full -> clear all`）。一层 eviction 不连带清空其他层；内存压力按冷到热逐级释放，不以 `clear()` 作为普通策略。

### 37.5 高刷新率与 world-ready 正确性

- **稳态帧不 shape。** Text/font/layout 未变时，稳态帧 0 system font query、0 font IO/parse、0 shaping、0 glyph raster/MTSDF 生成、0 per-glyph 分配/同步、0 BiDi/line-break/grapheme/CaretMap 重算。高刷滚动只更新 transform/clip。
- 大字体 parse、large paragraph shaping、可线程化 raster 进入 worker/staging；主线程只做有预算的 commit。
- 复杂文字以 UAX #29 / #9 / #14 + locale tailoring 为基线；logical text 是 source truth，BiDi visual reorder 在断行后按行执行；UTF-8/UTF-16 offset、grapheme、cluster、glyph index 使用不同 typed identity；caret/selection/hit-test/IME 共享 logical↔visual 映射；incremental 结果必须与 full recompute 等价。宽度感知 reflow 见 ADR 0027。
- Unicode 官方 conformance corpus 与 CJK/RTL/IME golden corpus 进入 CI；核心 regression gate：系统字体数量不能让普通 App startup 产生线性 parse 成本。

### 37.6 Benchmark 覆盖（合同）

Text profiler counter 列表以 Text 规范为准。Benchmark 至少覆盖：Native 无 packaged fonts；系统安装 3000 fonts 的 startup；CJK 10k chars cold/warm；mixed zh/ja/ko；Emoji ZWJ cluster；UAX #9/#14/#29 conformance corpus；BiDi mixed RTL/LTR + numbers + isolates；BiDi dual-caret / multiline selection / hit-test roundtrip；CJK kinsoku；Arabic unsafe-break reshape；CJK/RTL IME composition；font picker 500 faces scan；large CJK font memory pressure；WASM zero-font startup；WASM packaged SFNT first use；60/120/144/240Hz static/scroll/editor workloads。

### 37.7 Ownership

`viso-text` 拥有 FontManifest 语义、resolver/fallback policy、Face SLRU、coverage accelerator、shaping/paragraph cache、Unicode integration、typed text position 与 caret/selection/hit-test/IME 映射、TextWork 调度、font revision/invalidation、glyph residency 合同与 profiler counters。标准算法复用成熟实现（`unicode-bidi`、`rustybuzz`、`ttf-parser` 等，ADR 0025）。facade 拥有系统字体 binding；`viso-render` 拥有 GPU atlas texture/page/upload 机制。

### 37.8 Embedded 字体策略

Embedded profile 下默认：
- 没有系统字体 provider（不链接 fontconfig/CoreText 等），只使用 packaged font；
- **build-time subset**：字体按「静态字符串用到的字符 + `Viso.toml` 声明的 charset」做子集。动态文本可能出现子集外的字符时，必须声明 charset，否则构建期报 diagnostic，而不是运行时显示 tofu；
- **baked atlas（可选）**：UI 字号固定的产品在 device target 的 `fonts.bake` 声明字号，构建期把子集的 drawable glyph 闭包按这些字号预光栅化为压缩的 A8 page（`viso-glyph-a8` 的 "VBA8" 格式，8 字节对齐 static，原地读取）；运行时 glyph cache 先读 baked page 再 raster，pixel 与运行时 raster 逐字节相同。`fonts.rasterizer = false` 时 build 不链接 font rasterizer（facade 的 `rasterizer` feature），嵌入字体删去 outline 表（`glyf`/`loca`/`CFF `/`gvar`），只留 shaping 所需；任何 glyph 无 baked pixel 或 feature 与之不符都是编译错误，未声明的字号从最近的 baked 字号重采样。合同与实测见 Text/Font Runtime §3.6；
- 默认关闭 MTSDF / OutlineVector / ColorVector；ColorRgba8 emoji 可选；
- Unicode 数据表按声明的 locale 集合裁剪：Thai/Lao/Khmer/Myanmar 的换行 LSTM 模型（Unicode 数据中最大的一块）只在 device target 的 `locales`（缺省为 i18n catalog 的 locale）、静态文本或 charset 用到这四种文字时链接（facade 的 `complex-line-break` feature）；BiDi、grapheme 与 UAX #14 规则表是任何动态文本都需要的，不裁剪（Text/Font Runtime §3.7）。

以上都是 profile 的默认值，不是额外的语义：同一份 `.vs` 在 desktop profile 下照常使用系统字体 fallback。

---

# Part XIV — Viso DSL、热更新与语言工程

本 Part 只固定 DSL 与 Runtime 的边界合同。语言本身（词法、文法、类型系统、运行时语义、lowering）的唯一规范是 **`Viso_DSL_1.0.md`**（下称 DSL 规范）；Dev Runtime 与热更新协议的唯一规范是 **`Viso_Hot_Reload.md`**。二者与本节冲突时，以已接受 ADR 为准，其次以对应专项规范为准。

## 38. 定位：Viso DSL 是一等 Authoring Layer，不是 UI Runtime 的地基

- 依赖方向固定为编译器到运行时单向：`viso-dsl -> viso-schema <- viso-ui`（ADR 0042）。`viso-ui` 永不依赖 `viso-dsl` 或 Script VM（§10）。纯 Rust 应用完全关闭 `hot-reload` feature 时，UI runtime、layout、renderer、widgets 的基础能力仍然成立。
- Release 加载路径只在 `viso-ui::aot`，不得触达 `viso-dsl`（ADR 0016）。

### 38.1 三个 Rust 入口，一套语言语义

`ui!`（ViewFragment）、`component!`（ComponentDecl）、`view!(path)`（`.vs` CompilationUnit）见 §0.1 与 ADR 0014；当前由 `viso-ui-macros` 提供。三者只允许**入口 production** 不同，必须共享 lexer/token model、component/native schema、name resolution、type/effect/capability checking、Typed HIR、Reactive IR、UI IR、Shader IR 与 diagnostics/source maps。

它们是构建期 compiler frontend，不代表运行时宏系统，不允许每帧展开/rebuild UI。Release 中所有入口生成相同的 compact AOT descriptors/IR，启动时不 parse `.vs`，也不解析 Rust source。

### 38.2 语言 Surface 分层

语言能力按 Core / Standard / Advanced 分层（关键字分层见 DSL 规范「关键字」与 ADR 0034，Proposed）。硬规则：

- Advanced Surface（用户定义 trait/impl、通用泛型、Template/Part 元编程、手写 native 声明、compiler plugins 等）不得阻塞 1.0 vertical slice，并在 Quick Start、formatter 示例与 AI context 中隔离；
- `.vs` 不是第二门必须完整重实现 Rust 的通用语言；
- 语言团队维护 feature matrix，每个 production 标注 `stable / preview / reserved`。

### 38.3 Authoring 规则（摘要）

Property Binding 使用 `property: expr;`，imperative 代码使用 `=` / `+=`；命名节点写作 `node name: Type { ... }`；事件处理只有 `on event { ... }` block 形式。Viso 不使用隐式 apply/merge 运算符；merge、style、override、replace、record update 必须是独立、可类型检查的构造。分号是 property/behavior statement 的稳定终止符。

```viso
import viso::widgets::{Column, Text, Button};

export component Counter {
    state count = 0;

    view {
        Column {
            Text { text: format("{}", count); }
            node add_button: Button {
                text: "Add";
                on click { count += 1; }
            }
        }
    }
}
```

- 普通 `.vs` 不写语言头或 module 头：语言版本来自 `Viso.toml`/lockfile，module identity 来自 package root + source path；conformance fixture 需要显式 identity 时由 test harness 提供，不扩展普通 grammar（DSL 规范「Compilation Unit」）。
- 类型显式度：public/exported API、input/event/slot/native/shader interface、外部持久化 schema 边界必须显式类型；private state/computed、local let、有期望类型的 closure 参数可推断。推断结果写入 schema；热重载时推断类型改变按 schema migration 处理。**1.0 核心没有 Dynamic 类型，无法推断的值不得回退成 dynamic**（DSL 规范「类型推断边界」）。
- Native surface 默认由 Rust schema 或 generated interface 提供；Capability 从实际 native call graph 推导并与 package/profile grant 比较，`requires { ... }` 只是 public API 的显式 assertion。
- Game support 是一等 vertical slice，只要求 compiler 消费 Native Schema 定义的 scheduler traits（如 `system PlayerController implements FixedUpdate { ... }`），不等待用户定义 Trait/Impl；小型 game facade lower 到同一 scheduler，不新增 parser 关键字（DSL 规范「游戏表达能力与 Game Profile」）。

## 39. Viso 编译管线

```text
Source → Streaming Tokenizer → Lossless CST → AST → Name Resolution / Module Graph → Typed HIR
      → UI IR / Binding IR | Shader IR | (目标) Optional Script Bytecode
```

- Lossless CST 必须存在（ADR 0010），服务 formatter 保留注释、精确 rename、incremental parse、AI 结构化修改、IDE code action、hot reload structural diff 与一次报告多个错误；禁止从 token 直接走向 runtime opcode。
- Typed HIR 与 type/effect/capability checking 见 ADR 0011/0013；开发期必须发现属性拼写、类型不匹配、不存在的 event、错误枚举、shader uniform 类型不匹配、invalid resource、read-only property mutation 与模块循环依赖。

## 40. Rust Schema Bridge

Viso DSL compiler 依赖 `ComponentSchema { type_id, name, properties, events, slots }` 一类 schema，不直接操作 Rust 对象内存布局。自动生成 schema 的 `#[derive(Component, Reflect)]` 属于**目标**；当前 schema 由 `viso-dsl` 内的 native schema 表与 `viso-ui` 描述提供；目标是统一由纯数据的 `viso-schema` 承载。`viso-ui` 和 widgets 在构建期导出 schema 表，compiler 只读取这些数据，不链接 widget 实现（ADR 0042）。

IR/runtime 使用 `PropertyId`、`TokenId`、`NodeKey`、`ComponentTypeId` 等数值 ID；开发模式保留字符串用于 diagnostics；**release hot path 不做字符串属性查找**。

## 41. 开发运行与 Release AOT

- **Dev artifact**（`viso run`）可以保留 CST/HIR incremental cache、source map/debug name、schema reflection、hot-reload Symbol/source metadata、Dev Runtime、PatchBundle receiver/apply path、DevSnapshot endpoint 与 Inspector hooks。
- **Release artifact**：`.vs` / `ui!` / `component!` 在构建期经同一 frontend 生成 compact UI IR / Shader blobs，以 embedded asset 或 generated Rust data 形式打包（ADR 0016）。启动时不 parse、不 type-check、不需要完整 source symbol string table。
- **Release 执行模型**（ADR 0041）：模块的每个 chunk（handler、binding evaluator、computed、effect body、初始化、函数、闭包、task）由 `aot::lower_module` 生成 Rust 源码，与 app 一起由 rustc 编译优化（内联、常量折叠、LTO）。handler table 的 `EvaluatorId` 是 chunk 索引；模块字节仍随 artifact 携带布局、常量与 span，供运行时外壳与 fault 位置使用，但其中的指令不再被解释。镜像 state 的拷贝由 `StateStore::epoch` 门控。
- **语义一致性**：同一份 IR 在 Dev（解释器）与 Release（生成代码）下必须产生相同的结果。`crates/dsl/tests/differential.rs` 对 `differential/*.vs` fixture 双跑，逐步（调用、task 启动、每次恢复、`done` handler）比较值或 fault（种类、位置、消息）、states、revision、dirty 集合、事件、task 启动与 cost；不一致视为 compiler bug。
- Release **完全不编入** Dev transport listener、PatchBundle decoder/apply engine、DevSnapshot endpoint、hot-reload command handler、hot-reload-only 元数据与远程开发控制面。这是 build graph/feature boundary，不是运行时开关；不得通过环境变量、配置或隐藏端口重新开启。Release 稳态帧不得有 per-frame dev socket poll、`hot_reload_enabled` 分支、SymbolId lookup 或 patch revision check。Debug symbols/crash source map 是另一项 build policy。

## 42. Development Runtime & Transactional Hot Reload

完整协议见 `Viso_Hot_Reload.md`，事务引擎位于 `crates/dsl/src/hotreload/`（ADR 0015）。Architecture 固定以下合同：

1. **Multi-lane**：`.vs` UI/behavior → Typed Semantic Patch；Game System → Fixed Tick-boundary System Patch；Shader → Validated Pipeline Patch；Asset/Font → Resource Revision Patch；Rust → Incremental Build + Stateful Warm Restart。
2. **Transaction**：compile → validate schema/type/capability → semantic diff → state-preservation plan → stage → 等待对应 domain boundary → atomic commit → targeted dirty propagation；失败则 NACK/rollback 并保留 last-good。validation 完成前不得半修改 retained state。
3. **Domain boundary**：UI/Reactive → Frame Boundary；Game System → Fixed Tick Boundary；Shader → GPU-safe Frame Boundary；Resource → Resource-ready + Frame Boundary；Rust → Warm Restart Boundary。
4. **State preservation**：stable SymbolId；compatible state slot 保留；incompatible private state 只 reset 最窄 scope；child reorder 用 StableKey；focus/scroll/selection/animation 各有 preservation contract；Rust Warm Restart 只恢复 typed DevSnapshot，不 dump raw memory。
5. **不注入任意 Rust 机器码**：不以 JIT、machine-code page replacement 或通用 dylib swap 作为跨平台方案；Rust 修改走 incremental build → DevSnapshot → relaunch → restore。iOS/Android 只要求 Simulator/Emulator。
6. **协议**：Host ↔ Dev Runtime patch 使用 `viso-ende` Binary，CLI/Studio diagnostics 使用 Ende JSON；Patch 带 session/build/revision/schema identity，禁止乱序 blind apply。
7. **Last-good**：失败 candidate 不得把画面替换成 half-built tree 或 invalid pipeline。
8. **Remote Dev session（Embedded）**：Dev artifact 在板卡上运行，Host 通过 TCP 上的 Ende Binary 协议推送 PatchBundle。这与桌面 Dev transport 是同一个协议，只是 transport 不同。Rust 修改走 cross build → 部署（rsync/scp）→ Warm Restart。板卡上的 Dev artifact 不含 compiler，compile 只在 Host 上执行。

## 43. Viso 模块系统

编译器负责 module graph、topo sort、cycle detection、public/private、resource/shader namespace 与 incremental module rebuild；业务代码不靠手工注册顺序保证 UI 能启动（ADR 0011；DSL 规范「Compilation Unit」「Import」）。普通源码不写 `module` 声明（§38.3）。

## 44. Capability-based dynamic scripting

静态 capability 检查不能替代运行时授权：每个 Isolate/Module 持有 Capability Set，Native Call 时验证，失败返回 `CapabilityDenied` 而不是 panic；AI Preview 默认只有最小 UI/GPU/package-asset 能力（DSL 规范「Capability 运行时」）。若保留完整 VM（目标：AI 预览、第三方插件、Studio sandbox、用户脚本），instruction/time budget 保留，但不替代 capability security。

---

# Part XV — Widget 与 App Kit

## 45. 一个 `viso-widgets`，内部模块化

不预先拆成 `viso-widgets-core/app/adaptive` 等多个 crate，所有官方基础 widget 在单一 `viso-widgets` 内按模块组织。

**当前**（`crates/widgets/src/`）：`containers.rs`、`grid.rs`（ADR 0009）、`list.rs`（VirtualList，ADR 0008）、`scroll.rs`（ADR 0007）、`text.rs`、`image.rs`、`icon.rs`、`progress.rs`，以及 `controls/`（button、checkbox、radio、toggle、slider、text_input、tabs、splitter、popup、modal、sheet、toast、navigation_stack、caption_bar、`dock/`（ADR 0023）、`file_tree/`（ADR 0024））。

**进度指示**（`viso_ui::progress`、`viso_widgets::{progress_bar, progress_ring, spinner}`）：进度条与进度环是节点内容 `Content::Progress`（`ProgressShape::{Bar, Ring}`），持有已知时的份额 `value`（`0..=1`，`None` 为时长未知）、正在绘制的份额 `shown`、颜色与运行时间；条从起始边（RTL 为右）填充一枚胶囊，环用描边路径从顶部顺时针画弧，轨道为同色 24% 不透明度。`NodeStore::set_progress` 设置时保留当前 `shown`，新值以 80 ms 时间常数指数趋近（减少动态效果时立即到位，从未知变为已知时从零长出）；时长未知时条上一段按 `fast_out_slow_in` 先伸后缩扫过，环复用转圈的追逐弧（`spinner::chase`）。运动中的节点在 `Progresses` 冷表里，`is_progressing` 在其中有节点显示（`NodeStore::shows`：自身与祖先都未隐藏且不完全透明）时让帧循环跳动，看不见的原地等待；转圈的弧与辐条同样只在显示时推进，`tick_progress` 推进并只标记 `PAINT`，落定的节点离开冷表，槽位释放时遗忘。`Spinner` 复用 `Content::Spinner` 并在建成时转动。三者的语义角色为 `Role::ProgressIndicator`（AccessKit `ProgressIndicator`，Web `progressbar`，iOS 静态文本读百分比），有值时报告数值与范围，没有增减动作。Rust 控件的值可以是常量，或经 `ProgressIndicator::bind` 跟随一个状态格（结构钩子更新内容与语义状态，不重建）；DSL 的 `ProgressBar`/`ProgressRing`/`Spinner` 未写尺寸的轴为 `Fit`，按 160×4、32、20 的固有尺寸测量。

**骨架屏**（`viso_ui::shimmer`、`viso_widgets::{skeleton_rect, skeleton_circle, skeleton_text, placeholder}`）：骨架块是节点内容 `Content::Shimmer`（`ShimmerLook::{Wave, Pulse, Still}`、底色、高光、圆角与时钟），不可命中、无语义。`Wave` 在圆角裁剪层内画光带：每条光带是一枚解析阴影（`AnalyticShadow`，宽 96 的核心以 σ=24 模糊，上下伸出块外只让两侧渐隐），一条光带一个图元、零分配；光带中心按共享时钟沿窗口横向排布、相隔 800，因此同一占位中的头像、文字行、图片块被同一条光带依次扫过，与布局无关。`Pulse` 让底色透明度以 2 s 周期在 100% 与 50% 间往复。一个存储的所有骨架块共用一个时钟（`tick_shimmers`，每帧步长上限 0.1 s，只标记 `PAINT`）；`is_shimmering` 仅在有运动的块显示（`NodeStore::shows`）且未减少动态效果时让帧循环跳动。`skeleton_text(n)` 是一列铺满宽度的行（行高 12、间距 8），多行时末行占 60% 宽度；`SkeletonStyle` 可替换颜色、圆角与行参数。`placeholder(loading)` 一次建成骨架与内容两部分，结构钩子跟随布尔状态格切换两者的 `hidden`：加载中骨架显示，作为 `Role::ProgressIndicator` 以标签（默认 "Loading"）宣告、无数值；加载完毕内容显示并以 200 ms 透明度淡入（减少动态效果时立即到位），隐藏的骨架不再驱动帧循环。

**分隔条**（`viso_widgets::{splitter, SplitAt}`）：`Splitter` 把两个 pane 横排（`Axis::Row`）或竖排（`Axis::Column`），中间一根条。条的位置是 `SplitAt`：`Fraction(f)` 为前 pane 在可分空间（总长减条宽）中的份额，两个 pane 随容器一起伸缩（两侧为 `Fill` 权重 `f` 与 `1-f`）；`Start(dp)`/`End(dp)` 固定前/后 pane 的长度，容器改变尺寸时该 pane 保持不变、另一侧取余下（`Fixed` + `Fill`）。移动保持给定的种类。拖动时条随指针移动并保持按下的抓取点，方向键步进 10（Shift 为 50），Home/End 到两端，Enter 把前 pane 折叠到最小值、再按恢复；`min(lead, trail)` 限定两侧最小长度（空间不足时按最小值比例分配）。每次移动写入一个状态格，结构钩子用 `set_fixed_size` 原地改写两个 pane 的尺寸（pane 上 `MEASURE | LAYOUT | PAINT`），只重排分隔条自身的盒子，不重建；`on_change` 报告新的 `SplitAt`。事件处理器通过 `EventCx::rect_of` 读取容器、条与前 pane 的实时几何，镜像（RTL）的横排里前 pane 位于条之后，拖动与左右键按屏幕方向处理。条是唯一可聚焦的节点，`Role::Separator`，以分隔条的标签命名；份额报告百分比（范围 0–100），固定 pane 报告其长度（dp，无范围）；可聚焦的分隔条与滑块一样提供 Increment/Decrement 无障碍动作（映射为方向键）。条显示调整尺寸光标，悬停与拖动中换用 `SplitterStyle` 的 `hover`/`drag` 样式。

**表单**（`viso_widgets::{field, Form, fieldset, FormControl}`）：`field(caption, control)` 是带标题（`&` 标记访问键，标题即控件的无障碍名称）、可选帮助行与错误行的控件。`FormControl` 让控件报告它持有的值（`FieldValue::{Text, Flag, Number, Choice, Date}`）并在自己的变更处理器之后通知字段；文本框、数字框、复选框、开关、下拉选择、单选组与日期选择器都实现了它，文本框没有自己的提交处理器时 Enter 提交所在表单。字段在控件的处理器里随每次变更检查值：`required(message)` 拒绝空值（空白文本、未勾选、未选择），`rule(f)` 依次检查、第一个拒绝的给出原因。错误不在用户填写时出现：焦点离开字段（字段根节点从焦点变化的 `Lost` 冒泡得知，结构钩子再确认新焦点不在字段内）或表单提交后才显示，此后随每次变更更新、修正后立即消失；显示经一个修订号状态格触发结构钩子，改写错误行文本并切换 `hidden`、更新控件的 `Validity`，不重建。`Form` 是可克隆的共享句柄：字段经 `.form(&form)` 弱引用加入，`submit` 显示所有字段的错误、把焦点移到第一个被拒绝的控件，全部通过才调用 `on_submit`；`reset` 收起错误。`fieldset(legend, content)` 是带边框的分组，`Role::Group` 以图例命名。

**校验语义**：节点的 `Validity { required, invalid, error }` 存于稀疏冷列（`NodeStore::set_validity`，标记 `SEMANTICS`），与控件自身的 `SemanticState` 正交，派生进 `SemanticsNode::validity`。桥接时 AccessKit 设置 `required`、`invalid` 与 `error_message` 关系，Web 写 `aria-required`、`aria-invalid` 与 `aria-errormessage`；由于并非所有读屏软件都读取错误关系，拒绝原因（错误节点的名称）同时并入节点的描述（排在提示之前），iOS 由此经 `accessibilityHint` 读出。

**徽标与头像**（`viso_ui::plate`、`viso_widgets::{badge, dot_badge, avatar}`）：两者都是带 `CheckShape::Plate` 标记的文字叶子——不画标记，字幕居中于节点背景（`BoxStyle::radius_pct = 0.5` 画出的胶囊或圆）上，测量为字幕高度（至少 16）且至少与高一样宽，两侧各留高度的四分之一；字幕为空时为 8 的圆点。标记沿用勾选控件的字幕偏移通道（测量、最小宽、基线、重排宽度、绘制），因此无需在叶子上开放内边距。徽标的计数超过 `max` 时显示 `max+`（`viso_schema::plate::badge_count`），默认 `#b3261e` 底、白字、11sp、字重 500；头像无图时显示名字首末单词的首字母（无大小写的文字只取首字），底色由名字的 FNV-1a 哈希在十种深色中选取（`initials`、`avatar_rgb`，Web 导出的脚本以同一算法实现），字号为边长的 0.4；有图时内容为 `AssetFit::Cover` 的资源并被圆角遮罩裁剪，解码前显示名字的底色。语义：徽标为 `Role::Label`（名称为 `label` 或所显示的计数，无计数无 label 的圆点为装饰），头像为 `Role::Image`（名称为名字）；两者不获得焦点。

**纸片**（`viso_widgets::chip`、`CheckShape::Chip { removable }`）：纸片是一个可获得焦点的文字叶子，标记占满整个节点：圆角为高度 1/4 的主体，未选中时为字幕色 40% 的 1dp 描边，选中时描边淡出、底色渐变为字幕色 16%，字幕前一枚对勾沿描边画入；可移除的纸片在末端边（RTL 为起始边）画一个叉。高度为字幕行高加 12（至少 32），未选中时字幕前后各留高度的 0.375，选中时前方再加 0.6875（对勾 0.5625 的方框两侧各 0.25 的间距），叉占末端 1.0625。对勾占用宽度，所以选中动画（150 ms ease-out）的每一步除 `PAINT` 外还标记 `MEASURE|LAYOUT`（`CheckMark::moves_caption`），纸片在流式布局中逐帧变宽；测量、最小宽、基线与重排宽度都读当前显示的进度。命中叉由 `CheckMark::removes_at`（按节点高度计算末端区域）与 `EventCx::removes_at` 判断。语义：可选中的纸片为 `Role::CheckBox` 并投影选中状态，其余为 `Role::Button`；可移除的纸片向辅助技术提供 `AccessAction::Remove`（AccessKit 与 iOS 的自定义动作 “Remove”），执行时聚焦它并按下 Delete，与键盘移除走同一路径。

**步进器**（`viso_ui::stepper`、`viso_widgets::stepper`）：步进器是节点内容 `Content::Stepper`——一枚分成两半的胶囊（默认 96×32，圆角为高度 1/4），起始边（RTL 为右）是减号、末端边是加号，中间一条分隔线。它不持有数值：驱动者（Rust 控件、DSL 的 `Stepper`）持有数值与边界，经 `NodeStore::show_stepper(lower, upper)` 告诉它哪一半还能步进，不能的那一半减淡。按下一半立即步进一次并按住它（`EventCx::hold_step` → `StoreRequest::HoldStep`）：store 在 500 ms 后用该半的唤醒令牌（`StepHalf::token`）唤醒节点的键处理器再步进，之后每 120 ms 一次、按住八次以后每 50 ms 一次，直到抬起、指针离开、或那一半不能再步进；每次按住递增序号，旧按住的定时器因序号不符而放手。`stepper_input` 把采样读成 `StepInput`（按下/唤醒一步、方向键一步且 RTL 下左右互换、Page Up/Down 十步、Home/End 到边界），`StepInput::apply` 把它作用到数值上（越界止于边界，`wrap` 且两端都有界时绕到另一端，步进落在从下界起的 `step` 网格上），所有驱动者共用同一规则。语义为 `Role::SpinButton`，携带数值与范围，辅助技术的增减动作映射为上下方向键。只有按住状态与能否步进改变时标记 `PAINT`。

**分段控件**（`viso_ui::segments`、`viso_widgets::segmented_control`）：分段控件的轨道是一个行容器，其内容为 `Content::Segments`——只画在子节点之下、不参与测量：选中段盒上的滑块（圆角 6，带轻阴影）与相邻两段之间的细分隔线（离滑块 6dp 内渐隐）。它不持有选择：驱动者（Rust 控件的结构钩子、DSL `SegmentedControl` 的值求值）经 `NodeStore::select_segment` 告诉它选中哪段；轨道已布局时，滑块从当前显示的位置（相对轨道盒记下）以 `LookValue::Thumb` 的弹簧过渡（response 0.3s、临界阻尼）滑到新段，未布局或减少动态效果时立即到位，每帧只标记轨道的 `PAINT`。滑块终点读选中段的实时布局盒，因此尺寸、文字或方向变化后仍贴合；一段的盒移动（`set_bounds`）时也记下其轨道为已移动，使轨道重画。每段是一个带 `CheckShape::Segment` 勾选标记的文字节点：标记不画任何东西，只把文字居中并在四周留白（两侧 12、上下共 10、至少 28 高），选中状态跟随语义状态的 `checked`/`selected`。Rust 控件的段宽为 `Fill`，给定宽度时平分；容器为漫游组（`Role::Group`），每段为 `Role::Radio`，键盘规则与 `RadioGroup` 相同。

**滑块**（`viso_ui::slider`，`Content::Slider`；`viso_widgets::{slider, knob, range_slider}`；DSL `Slider`、`Knob`）：滑块是一个节点的内容，只持有拇指所在的分数（0–1），数值与 `SliderRange { min, max, step }` 由驱动者保管（Rust 控件的值单元与结构钩子、DSL 的值求值），经 `NodeStore::set_slider`/`show_slider` 告诉它画在哪里；拇指以 60 ms 时间常数缓动过去，拖动中或减少动态效果时立即到位（每帧只标记该节点 `PAINT`，`tick_sliders` 随帧循环）。形状：条形沿轴（横向随 RTL 镜像、纵向向上增长），轨道 4 dp 胶囊，填充从起点或 `origin`（双极）到值，两拇指的区间滑块填充两者之间，`marks` 在每一步画点（间距不足 6 dp 不画），拇指为白色圆（20 dp，阴影与细边），按住时外加淡色光晕；旋钮为底部 90° 缺口的圆环、填充弧与带阴影的表盘，表盘上一条指向值的线。`slider_input` 统一解释样本：条形按在轨道上把较近的拇指跳到该处，按在拇指上则原处拾起，拖动以未夹紧的累计分数推进（Shift 为十分之一），区间的两拇指不能交叉，按在重合处时由拖动方向决定拿哪个；旋钮以上/右拖动转动；双击回到默认值（Rust 的 `default_value`，DSL 的 `origin`）；方向键按 step（无 step 时为范围的 1%）、Page Up/Down 十步、Home/End 到两端；滚轮在指针下的滑块（或其拇指）已获焦点时按每 16 px 一步经唤醒令牌送达其按键处理器，否则照常滚动。语义：滑块与旋钮为 `slider`（值与范围）；区间滑块是 `group`，内含两个覆盖整条轨道、不画任何东西的可聚焦 `slider` 节点（下限/上限，各自一个 Tab 停靠点），聚焦的拇指在轨道上画环而不戴通用焦点环。HTML 导出为 `<input type="range">`，`axis: column` 以 `writing-mode` 竖排，`tint` 为 `accent-color`，`Knob` 为圆锥渐变画的圆盘，其分数由脚本写入 `--vx-at`。

**取色器**（`viso_ui::color_plane`，`Content::ColorPlane`；`viso_ui::color_picker` 的部件；`viso_widgets::color_picker`；DSL `ColorPicker`）：色平面是节点内容，持有 `Hsv`（基于 gamma 编码 sRGB，色相以圈计、红色为 0）与不透明度，三种形状：`Square`（饱和度向右、亮度向上）、`Wheel`（顶部为红、顺时针的色相环围着这样一个方块）、`Swatch`（不透明时整块颜色；有透明度时左半不透明、右半按不透明度叠在棋盘格上）。方块与色环用 `Mesh` 绘制，每个顶点带该点的精确颜色（方块 16×16 格，环 96 段，圆角用扇形，边缘外扩 0.8 dp 的 `edge = 0` 羽化），所以显示的就是所选数值；色相/不透明度条复用滑块内容的 `TrackPaint::{Hue, Alpha(Rgba)}`（彩色轨道、棋盘格，拇指为透出轨道的圆环），因此拖动、方向键与滚轮与滑块一致。`plane_input` 解释样本：按在环上取色相、按在方块上取饱和度/亮度，拖动沿用按下的部分并夹在其中；方向键按 1%（Shift 10%）步进聚焦部分。控件：方块（或色环）、色相条（色环时没有）、不透明度条（`alpha(false)` 时没有）、一行十六进制输入与色块。颜色只存在一个颜色单元（自有或 `bind`），结构钩子把它显示到各部分、更新语义并重新播种十六进制文本；控件另记最近一次的 HSV，灰色与黑色时保留色相、黑色时保留饱和度。十六进制接受 `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`（`#` 可省），编辑成完整形式时立即生效，短形式在失焦或 Enter 时生效，此时文本规范为 `#RRGGBB`（有透明度时 `#RRGGBBAA`）。语义：`group`（名为其标签），含方块的 `slider`（值文本如“Saturation 50%, Brightness 80%”）、色相 `slider`（0–360）、不透明度 `slider`（0–100）与 `textfield`，各自一个 Tab 停靠点，名称由 `ColorPickerLabels` 给出。

**叶节点的子节点**：叶节点仍只按自己的尺寸请求与内容固有尺寸测量；它的子节点（`BuildCx::inside(leaf, …)` 或 `with_parent`）在 layout 时像 stack 的子节点一样放在叶节点的盒内（默认居中，`StackPlacement` 可指定对齐），从不撑开叶节点，绘制在叶节点内容之上、命中测试先于叶节点。视频的控制部件即如此叠在画面上。

**全屏**（`viso_ui::fullscreen`）：同一时刻至多一个节点全屏（`NodeStore::set_fullscreen` / `EventCx::set_fullscreen`）。全屏节点成为覆盖整个 surface 的 layer（不论其尺寸请求），以黑色填充为底并 trap focus；若窗口尚未全屏则经 `take_fullscreen_ask` 请 facade 调 platform 的 `set_fullscreen`。退出时节点回到原 flow（原本就是 layer 的保持 layer）、恢复原填充，只有当初由它带进全屏的窗口才被请回。facade 在 `on_fullscreen_changed` 里调 `window_fullscreen_changed`：窗口自行退出全屏时节点随之退出；全屏节点被释放时下次 `take_fullscreen_ask` 请回窗口。`NoticeKinds::FULLSCREEN` 对节点自身或其祖先全屏投递 `Notice::Fullscreen(bool)`，DSL `Video` 的 `fullscreen_changed` 写回双向 `fullscreen`。layer 仍随祖先 transform 移动，因此滚动容器中的节点全屏时带着滚动偏移，这与其他 layer 一致。

**框架建的部件**（DSL `ColorPicker`）：取色器的部件——方块与色环的焦点停靠点、色相条、不透明度条、十六进制输入与色块——由 `viso_ui::color_picker::ColorPickerParts::build` 建成一列，Rust 控件与 DSL 节点共用同一实现。DSL 节点是不接受子节点的 Column（未定尺寸时按内容 `Fit`），视图在首次交付其值时用 `BuildCx::with_parent` 在节点内建出部件，部件消失（如被重建）时再建；`alpha`/`wheel` 之后改变只隐藏或显出不用的部件、改平面形状与高度，不重建。节点自身不响应输入：`attach` 把它报告的路由（`changed`）交给 `ViewHost` 的汇点，部件每取一色即以打包的 sRGB `Color` 触发它们，双向 `value` 由此写回；交付的 `value` 只在与部件所持颜色量化后不同时才显示，因此往返不会让色相或饱和度漂移。HTML 导出为 `role="group"` 的 `<div class="vx-color">`，内含 `<input type="color">` 与不透明度 `<input type="range">`（`alpha: false` 时隐藏），浏览器的取色器代替色环。

DSL `ComboBox` 与 `SearchField` 同样如此：逻辑在 `viso_ui::combo_box::ComboParts` 与 `viso_ui::search_field::SearchParts`，Rust 控件只是包装。部件的写操作经 `viso_ui::part_writes::PartWrites`，由 `EventCx`（处理器内，经 flush 生效，换文字是一次可撤销的编辑）与 `StructureCx`（视图交付时，立即写入，换文字是重新播种）各自实现，所以同一套逻辑既回应输入也显示交付的值。`with_parent` 建的部件没有语义投影注册表，`bind_semantic_state` 把投影挂在 store 上，下一次 flush 由 `SemanticProjector::adopt` 收下并先运行一次；节点已被释放的投影在下次运行时自行释放。组合框的 `options`、`free_text`、`placeholder`、`empty_text`、`label` 改变即释放旧部件重建，`value` 改变只选中对应选项（封闭集合）或换文字（`free_text`）；封闭集合由选中报告标签，`free_text` 只由取用的文字报告，不会重复。搜索框的 `placeholder`、`label`、`debounce` 改变重建，`suggestions` 改变只换建议行。HTML 导出为 `<input role="combobox">` 与 `<input type="search">`，各配一个 `<datalist>`（由浏览器过滤）；封闭集合的组合框只在文字等于某个选项时报告，`change` 时退回所持的值；搜索框的停顿与 Enter 提交由脚本处理，取用建议即报告并提交。

**日历**（`viso_ui::calendar`，`Content::Days`；`viso_widgets::calendar`）：`Date` 是前推格里高利历的一天，以距 1970-01-01 的天数计（Hinnant 算法；`to_state`/`of_state` 存进 `Int` 单元，`NO_DATE` 表示无日期），按月移动时夹到短月月末，另有 ISO 周数与 `YYYY-MM-DD` 读写。`first_weekday`/`hour_cycle` 按 BCP 47 标签的地区（无地区时取该语言最常用的地区）查 CLDR 周首日与 12/24 小时制，`-u-fw-`/`-u-hc-` 关键字优先。控件显示一个月的六周：标题行（上/下月按钮夹着“月份 年份”）、星期列头、六个周行；周首日取区域设置的（可 `first_weekday` 覆盖），可在前面加 ISO 周数。每个周行的内容 `Days` 在其日格下画选中圆盘、今天圆环与指针下的淡色圆盘，只重绘该行；结构钩子只给日期或外观变了的日格重设文字与语义。日格同属一个 roving 组（Tab 落在选中日）；方向键移动一天（左右随 RTL 镜像）或一周，Home/End 到周首/周尾，Page Up/Down 一个月（Shift 一年），到达别的月份即翻到该月；Enter/Space 或点击选取，点相邻月份的日子会翻到那个月。`min`/`max` 之外与 `unavailable` 拒绝的日子显示更淡、不可选，键盘跳过它们。语义：`group` 内含两个按钮与以月份命名的 `table`——一行 `columnheader`（星期全名），每周一行 `row`，日格为 `cell`（全日期为名，报告 selected，今天与不可选的日子带提示）；名称与日期写法由 `DateNames` 给出。

**日期选择器**（`viso_widgets::date_picker`）：字段以 `DateNames::short` 显示所选日期（未选时显示占位文字）并带日历图标；点击、或聚焦时按 Space/Enter/Alt+Down/F4，在字段下方的锚定面板（light dismiss）里打开同一个 `Calendar`（两者共享一个日期单元），焦点落在所选日（未选则今天，取最近可选的一天）。选中即关闭面板并把焦点还给字段；Escape 或在外面按下则原样关闭；打开期间 Tab/Shift+Tab 在可用的翻月按钮与当月落点日之间循环。语义：字段为 `combobox`（值为完整日期，报告 expanded），面板为以标签命名的 `dialog`。

**时间选择器**（`viso_widgets::time_picker`）：时间按段显示——时、分、（`seconds(true)` 时）秒，十二小时制再加 AM/PM；时制取 locale 的 CLDR hour cycle（`viso_ui::calendar::hour_cycle`，认 `-u-hc-`），`hour_cycle` 可覆盖。值是一个 Int 单元：自午夜起的秒数（`Time::to_state`），未设为 `NO_TIME`，各段显示占位符。每段是独立 Tab 停靠点，聚焦段以强调色填充（`InteractionStyle` 的 pressed 单元即该段的聚焦单元）。Up/Down 在段内循环步进（十二小时制的时保持 AM/PM，分按 `minute_step`，不在格点时先落到格点），Home/End 到段的两端，Left/Right 在段间移动（RTL 镜像）；键入数字直接设定该段，再无数字能接上时自动跳到下一段（一次提交的多个字符跟着跳），键入期间其他段按开始键入时的时间保留；AM/PM 段认其名称首字母。结果夹在 `min`/`max` 内，从未设开始时以 `min` 或 00:00 为基准。语义：以标签命名的 `group`，每段 `spinbutton`（名称为段名，值为所示文本，带数值与范围）。

**标签页**（`viso_widgets::tabs`）：标签条是横向 scroll viewport（隐藏滚动条，`set_wheel_across` 让只有竖向滚动的鼠标滚轮横向滚动它），条里一行 `tab`；面板区每个标签一个 `tabpanel`，只显示选中标签的面板，全部面板构建一次。按下或方向键（RTL 镜像）、Home/End 选中并聚焦标签，Ctrl+PageUp/PageDown 在控件内任意处切换；选中时 `scroll_into_view` 把它滚入视野。标签是一个 roving Tab 停靠点。`closable` 的标签带关闭按钮（`permanent_tab` 除外），关闭按钮、中键按下或 Delete 关闭标签：关闭选中的标签时选中前一个（无则后一个），焦点随之；节点在结构钩子里 `free_tree` 释放。`reorderable` 时标签条是 reorder list、每个标签是自己的 handle，拖动沿条移动标签（两端悬停自动滚动），Ctrl+Shift+PageUp/PageDown 把聚焦标签移一位；`RowGesture::Reordered` 改写顺序，结构钩子按顺序重链按钮。`on_change`/`on_close` 报告标签的声明序号，`on_reorder` 报告条内的起止位置。语义：`tab` 带 selected 与位置（`(序号, 总数)`，也是 reorder 判位依据），关闭按钮是名为“Close 标题”的 `button`。

**锚定浮层**（`viso_ui::anchored`）：一个 layer 可以锚在场景里的另一个节点上（`BuildCx::anchored(floater, anchor, Anchoring)`）。`Anchoring` 给出首选边（`Side::{Top, Bottom, Start, End}`，Start/End 随 RTL 镜像）、沿边对齐（`AnchorAlign`）、间距、窗口边距，以及三个碰撞策略：`flip`（首选边放不下且对边更宽裕时翻到对边）、`shift`（沿边平移进窗口）、`fit`（把可用空间作为 Fill 尺寸的上限，`match_anchor` 让宽度跟随锚点）。放置是纯函数 `place(anchor, natural, bounds, &Anchoring, rtl) -> AnchorPlacement`，结果（矩形、实际边、箭头沿边位置、可用空间）存在 `NodeStore` 的稀疏表里，`anchor_placement` 可读。窗口可用区域是 surface 减去 keep-out（facade 每帧设为 safe area，底边取 safe area 与软键盘高度的较大者）。锚点被 scroll / translate 移动时，`update_transforms` 调 `follow_anchors` 只重放受影响的 layer：位置变了才重新 lay 该 layer，不触发场景 layout；窗口 resize 走正常 relayout。`anchor_arrow(floater, arrow)` 登记的箭头节点被平移到实际边上、对准锚点中心，翻边时随之换边。

**轻量关闭**（light dismiss）：`light_dismiss(layer)` 标记的 layer 在窗口里任一按下落在它、它的锚点、以及压在它上面的显示中 layer 之外时收到 `KeyPayload::Dismiss`（`ev.dismissed()`），由 widget 决定关闭；按在锚点上不算外部，所以锚点自己的点击可以 toggle。`Popup` 默认轻量关闭、Escape 关闭，打开时焦点进入内容，关闭时还给打开前的焦点；`popover()` 是带箭头的气泡预设。

**菜单**（`viso_widgets::{menu, context_menu}`）：`menu()` 挂在锚点上（按下锚点打开），`context_menu()` 包住一块内容，在请求菜单的按下处打开（次键按下、Apple 上 Control 加按下、长按，非 Apple 上菜单键与 Shift+F10，见 `EventCx::context_menu`），锚在内容的一个点上（`EventCx::anchor_at`），点在内容的其他地方按下即关闭。菜单项可带图标、快捷键文字（`Shortcut` 按平台拼写：Apple 为 `⇧⌘S`，其他为 `Ctrl+Shift+S`；菜单只显示，绑定归应用）、勾选或单选标记（单选以分隔线划分的段为一组）、禁用、子菜单。指针：按下锚点即开，拖到某项松开即选（离打开处不足 6 逻辑像素的松开不算）；悬停高亮，在有子菜单的项上停留 200 ms 打开，点击立即打开；指针从父项斜向子菜单途经其他项时，只要仍落在由上一个位置与子菜单近边构成的三角形内，子菜单保持 300 ms（hover intent）。键盘：锚点上 Enter/Space/Down 打开并高亮首项、Up 高亮末项；菜单内 Up/Down 在可用项间循环、Home/End、阅读方向键（RTL 为 Left）打开子菜单并进入、反向键返回、Enter/Space 选择、键入的文字在 1 秒内累计跳到以之开头的项（同一字母重复则逐个轮换）、Escape 关闭一层、Tab 全部关闭；关闭后焦点回到打开前的位置。语义：面板 `menu`，项 `menuitem` / `menuitemcheckbox` / `menuitemradio`（报告 checked），带子菜单的项报告 expanded，分隔线 `separator`，锚点为报告 expanded 的 `button`。只有根面板登记轻量关闭，子菜单是它的子孙，按在其中不算外部。

**选择框**（`viso_widgets::select`）：字段显示所选项（未选时显示 placeholder），宽度取最宽的选项（每个选项一份不可见、零高的文字撑宽）。两种行为由 `SelectStyle::kind` 选择，默认随平台：`SelectKind::PopUp`（Apple 弹出按钮）列表盖在字段上，所选项打勾并与字段文字对齐（`EventCx::anchor_over`：锚定层把层内某节点放到与锚点等高处，层放不下窗口或该节点处在会滚动的视口内时退回到锚点一侧），Space/Up/Down 打开；`SelectKind::DropDown` 列表在字段下方、宽度不小于字段，Space/Alt+Down/Alt+Up/F4 打开，关闭时 Up/Down/Home/End 直接改选。打开时高亮所选项并滚入视野，超过 `max_height` 的列表滚动；Up/Down（不循环、跳过禁用项）、Home/End、PageUp/PageDown 按一页移动高亮，Enter/Space 选定，Escape/Tab 不变关闭，按在外部不变关闭；指针按下字段即开，拖到某项松开即选，打开处附近的松开不算。键入在 1 秒内累计跳到以之开头的选项（打开时高亮，关闭时直接选）。焦点随高亮落到选项上。语义：字段 `combobox`（报告 expanded），列表 `listbox`，选项 `option`（报告 selected 与在列表中的位置）。用户改选后调用 `on_change`；`SelectHandle::select` 由程序改选，不调用。处理器换文字用 `EventCx::set_text`（`NodeStore::set_text`：下一次布局前重新排版，`label` 角色的可访问名随之更新）。

**组合框**（`viso_widgets::combo_box`）：一个文本框加它过滤的选项列表。列表显示时焦点始终留在文本框里，高亮项是文本框的 active descendant（`EventCx::set_active_descendant` / `NodeStore::set_active_descendant`，语义树的 `SemanticsNode::active`，AccessKit 的 `active_descendant`，Web 的 `aria-activedescendant`）。键入即打开列表，保留标签中含所键入文字的选项（不区分大小写，保持原顺序），匹配的字母以 `ComboBoxStyle::matched`（默认半粗体）绘制。封闭集合（默认）：高亮首个以之开头的选项，值只能是选项——Enter/Tab 选定高亮项，Escape、按在外部、焦点离开都把所选项的标签放回；无匹配时列表显示 `empty_text`，Enter 不做任何事。`free_text(true)`：文本本身就是值，方向键之前不高亮，Enter 在无高亮时提交文本，焦点离开也提交，无匹配时列表关闭；`on_text` 收到提交的文本，`on_pick` 收到选中的选项。Down/Up 打开并高亮首/末项，Alt+Down 原样打开，打开后 Up/Down 循环、PageUp/PageDown 按页、Alt+Up 关闭，Home/End 等编辑文本。箭头打开/关闭整个列表；文本框未获焦点时的首次按下也打开整个列表并全选文字。语义：文本框为 `editablecombobox`（ARIA `combobox`），列表 `listbox`，选项 `option`（报告 selected 与在可见项中的位置）。处理器可用 `EventCx::edit(node, intent)` 编辑另一个文本框的文字。

**搜索框**（`viso_widgets::search_field`）：文本前一个放大镜，有文字时文本后一个清除按钮（`button`，名 "Clear"，不进 Tab 序——Escape 即可清除）。每次编辑把查询交给 `on_change`：默认立即，`debounce(d)` 则在键入停顿 `d` 后（`EventCx::wake_after` 送到文本框的 key handler，令牌低位计编辑次数，被后续编辑超越的唤醒直接忽略；清除、Enter、取用建议立即报告）。应用经 `SearchFieldHandle::set_suggestions` 给出建议（可在 `on_change` 内同步给，也可等搜索返回后给）：列表锚在文本框下方（`Side::Bottom`、`match_anchor`、light dismiss），行数按 `max_suggestions`（默认 8）一次建好，之后只换文字、隐藏多余行，所以随键变化的列表不建任何节点；与查询匹配的字母（不区分大小写，首次出现）以 `SearchFieldStyle::matched` 绘制。仅当文本框有焦点且内容是键入所得时列表才显示，空建议关闭列表。键盘：Down/Up 循环移动高亮（必要时打开列表），Enter 取用高亮建议（填入、报告 `on_change` 再 `on_submit`）否则提交文本，Escape 先关列表、列表关着时清空；指针：点建议即取用，清除按钮清空并保持焦点。语义：文本框 `searchfield`（AccessKit `SearchInput`，iOS `UIAccessibilityTraitSearchField`，Web `searchbox`），报告 expanded，高亮行是 active descendant；列表 `listbox`，行 `option`（名为其建议文字，经 `EventCx::set_semantics` 更新，报告在可见项中的位置）。

**焦点环**（`viso_ui::focus_ring`）：焦点由键盘或辅助技术移来时（`focus_shows_visible`），绘制在焦点节点子树之后、其自身裁剪与透明层之外、变换根之内追加环的描边四边形（`FocusRingStyle::paint`：外扩 `offset + width`，圆角为节点圆角加外扩，可带内侧第二道描边）；文本框（有 `field_marks`）与有聚焦链接的文字自己画环。窗口级样式 `FocusRingStyle`（`NodeStore::set_focus_ring_style`，门面按系统外观的配色与强调色 `Appearance::accent` 取 `native`）与稀疏的不戴环列表（`set_focus_ring(id, false)`、`BuildCx::focus_ring`、DSL `focus_ring`）一同存在冷表 `FocusRing`。焦点移动或 `focus_visible` 变化时只标脏所涉节点的 PAINT；有出现时长的样式（Apple 收拢、Adwaita 淡入）由 `tick_transitions` 推进一个出现时钟，期间 `is_transitioning` 为真、每帧只重绘焦点节点，减少动态效果时立即出现。环的像素由图元矩形进入损伤区，不受节点 `reach` 限制。

**焦点进出**：焦点移动后，离开的节点及其祖先、再到达的节点及其祖先的 key 处理器依次收到 `ev.focus_change()`（`FocusChange::Lost` / `Gained`，可被 stop），`ev.focused()` 是焦点现在的位置，容器据此判断焦点是否离开了自己；移回原处不算移动，处理器因此再移动焦点会再通知（至多数次）。驱动在每个输入样本与唤醒之后投递。

**提示**（`viso_widgets::tooltip`）：包住一段内容，指针在内容上停留 `TooltipStyle::delay`（默认 500 ms）后在其下方居中显示（无空间则翻到上方，并平移留在窗口内），超过 `max_width` 换行；一个提示消失后 `TOOLTIP_GRACE`（400 ms）内移到另一个带提示的内容上立即显示，同一时刻只显示一个。指针在内容或提示本身上时保持（跨过间隙有 `TOOLTIP_LINGER` 的余量），离开、按下内容、按在别处、无人处理的 Escape 都使之消失，按下后直到指针离开前不再出现。键盘焦点（`ev.focus_visible()`）进入内容立即显示，离开即消失；手指长按显示，抬起 `TOOLTIP_TOUCH`（1.5 s）后消失。语义：气泡为 `tooltip`，内容里第一个控件以提示文字为其描述（hint）。

**指针进出与默认按键**：指针移动时，离开的节点及其祖先（直到与新节点共同的祖先为止）由内向外收到 `Leave`，进入的祖先由外向内、最后是新节点收到 `Enter`；指针仍在其内的祖先两者都不收，所以一个控件无论子节点怎样划分都只收到一对 Enter/Leave。Tab 与 Escape 先走焦点链上的处理器，没有处理器 stop 时才执行默认行为：Tab 移动焦点，Escape 关闭最内层显示中的轻量关闭层（`NodeStore::dismissed_by_escape`）。

**惯性滚动**（`viso_ui::scroll_motion`）：调度器用帧时钟给每个输入样本打上到达时间（`FrameDriver::on_input(sample, at)`，headless 的 `FixedStepClock` 下确定），facade 写入 `NodeStore::set_input_time`。直接触点在一个固定大小的环中记录最近 0.1 s 的样本，抬起时按最后 40 ms 估出速度（某轴总位移不足 4 pt 视为 0，抬起前静止则为 0）；平移抬起（非取消）且速度不低于 `ScrollPhysics::min_velocity` 时，沿该轴承载这次平移、方向上仍有余量的视口开始滑行（`NodeStore::fling`，上限 `max_velocity`）。曲线随平台：Apple 为每毫秒 0.998 的指数衰减，Android 为 `OverScroller` 样条（距离 ∝ v^1.7、时长 ∝ v^0.7），其他平台为 0.997 衰减且快速轻扫向 0.9985 过渡并带 0.25 s 滑翔；`set_scroll_physics` 可替换。每帧 `AdvanceAnimations` 以帧间隔（围绕滑动平均夹紧，单步不超过 0.1 s）推进，只经 `scroll_by` 标 `TRANSFORM`；速度降到停止阈值、到达范围边缘或节点失效时结束，滑行期间帧循环保持跳动。按在滑行中的视口上即接住：滑行停止，速度仍大于 40 pt/s 时这次按压不到达任何控件（之后可继续平移），较慢时按压照常通过；接住后 0.4 s 内同方向再次轻扫叠加接住时的速度。滚轮落在其上、或 `request_scroll` 指向该视口时也停止滑行。

**越界滚动**（`OverscrollStyle`）：拉过范围端点时显示量单独存放（`NodeStore::overscroll`），`scroll()` 仍是夹在范围内的逻辑偏移，虚拟列表、通知等读者不受影响；`content_shift`/`content_transform` 把它加进视口内容的变换，仍只标 `TRANSFORM`。`Bounce`（Apple 及默认）让内容随手指越过端点平移，`Stretch`（Android）让内容沿轴以被拉的端点为轴心拉伸（满幅约 1.09 倍），命中测试按同一变换反算；`None` 为硬边。直接触点在没有余量时，只要沿该轴有可滚范围的视口就接管平移：越界部分按 iOS 橡皮筋 `raw·c·d/(raw·c+d)`（`c=0.55`，`d` 为视口长度的 0.35）显示，回拉先收回越界再滚动；松手后沿临界阻尼弹簧 `x(t)=(x0+(v0+λx0)t)e^(−λt)`（`λ=20/s`）带着抬手速度回到范围内；滑行撞到端点时以剩余速度越界回弹（过冲按剩余余量渐近压缩，不超过上限）。按在回弹中的视口上会定住它（按压不到达控件），抬起后从原处回弹。滚动样本带手势相位（`ScrollPhase`：`None` 为滚轮刻度或无手势信息的设备，`Began`/`Changed`/`Ended` 为触控板手指，`Momentum`/`MomentumEnded` 为系统惯性）：macOS 取自 NSEvent 的 phase/momentumPhase，Wayland 由 `axis_source=finger` 与 `axis_stop` 合成，iPadOS 取自滚动识别器状态，其他平台为 `None`。手指阶段与触摸平移同样拉伸，抬起时若平台不自带惯性（`ScrollPhysics::system_coast` 仅 macOS 为真）则由窗口按手指速度滑行；系统惯性撞到端点时按其速度回弹，并在该次惯性结束前不再移动该视口；`None` 的滚轮到端点即停。回放磁带记录相位（新记录标签，旧磁带仍可读）。

**连击**（`viso_ui::clicks`）：只有登记了计数的节点（`NodeStore::count_clicks`、`BuildCx::count_clicks`；文本框、声明 `double_tap` 的 DSL 节点）之下的主按钮按下才计数，没有登记者时不武装任何东西。同一指针（手指与笔共用一串）距上次按下不超过点击间隔、相距不超过阈值的按下接续同一串，至多三击（`EventCx::clicks`）；间隔是每次按下武装的定时器，计数不读时钟，随输入回放确定。间隔由门面按系统外观 `Appearance::double_click` 设置（`set_click_interval`：macOS `NSEvent.doubleClickInterval`、Windows `GetDoubleClickTime`、Linux 设置门户的 GNOME `double-click`/KDE `DoubleClickInterval`，Android 300 ms，其余取 `CLICK_INTERVAL` 500 ms），`EventCx::click_interval` 读出。DSL 节点同时听 `tap` 与 `double_tap` 时，单击抬起把 `tap` 的载荷留下并以 `wake_after` 在一个间隔后唤醒自己的键处理器，期间第二次按下推进代号使唤醒作废。

**手势竞技场**（`viso_ui::input` 与各识别器）：一根直接接触在按下时为其路径上的识别器各武装一份——落点节点的按下（`click`/`tap`）、最近的拖动节点、最近的长按节点、连击计数、最近的捏合节点，以及路径上的滚动视口——在越过阈值的那次移动上裁决：持有两指的捏合先于一切；已开始的拖动、或其上没有被落点先经过的视口的拖动先于平移；只沿一轴的拖动（`set_drag_axis`）遇到主要沿另一轴的位移即退出（`drag_leads_at`），同一次移动交给平移；平移按主要方向定轴（`ContactSlot::pan_axis`），只能沿该轴有余量（或可下拉刷新）的视口才接收，此后只移动该轴，抬起的惯性也只沿该轴，经 `scroll_chain_rest` 在同轴的嵌套视口间先内后外传递；越过阈值即放弃长按，长按到期取消按下与武装的拖动；处理器以 `capture_pointer` 显式认领（此后不再平移），被认领或胜出的一方让落点节点收到 `Leave`。鼠标不平移，只有拖动、长按与按下之间的裁决。

**轻扫**（`viso_ui::drag`）：拖动的每个样本（`DragSample`）带指针速度，由拖动自带的定长速度跟踪器（与平移惯性同一个，不分配）按输入时钟读出最近 40 ms 的样本；结束样本抬起时取抬起速度，取消为零。`DragSample::swipe` 在结束样本沿速度较大的轴不低于 `SWIPE_SPEED`（400 pt/s）且与该轴总位移同向时给出 `SwipeDirection`，DSL `swipe` 即此判定，声明它的节点照常武装拖动。

**捏合**（`viso_ui::pinch`）：一个识别器同时给出缩放、旋转与焦点平移（`PinchSample`），来源有三：两根直接接触、触控板手势（`RawEvent::Gesture` → `InputSample::Gesture` → `route_trackpad_pinch`；每个样本带自上一样本以来的缩放倍数与顺时针弧度，macOS 把放大与旋转两路合成一个手势）与按住 Control 的滚轮（`route_wheel_zoom`，按 `WHEEL_PINCH_REST` 的定时器结束，帧内由 `deliver_pinch_ends` 投递）。接受捏合的节点在冷表 `Pinches` 中；第二根手指落在同一节点下时记下两指的初始间距、角度与中点，越过触摸阈值时开始并接管两指（Capture、取消按下与长按、以取消结束已开始的拖动、对离开的节点派发 `Leave`），持有两指的捏合使平移状态机跳过它们。样本记在捏合上，处理器经 `EventCx::pinch` 读取，派发后清除。

**查看器**（`viso_ui::viewer`，部件 `InteractiveViewer`）：一个节点（`NodeStore::set_viewer`，限制 `ViewerLimits`：最小/最大缩放与边距）把它唯一的内容子节点按缩放与平移显示，二者写成内容的 turn（原点为内容左上角）与 translate，所以平移和缩放都只标 `TRANSFORM`。查看器自身接受拖动与捏合，样本在派发后的 `settle_drag`/`settle_pinch` 中驱动它：拖动（鼠标与手指同样）平移，捏合绕焦点缩放并随焦点平移，滚轮与触控板双指滚动在 `scroll_chain_rest` 中先于其上的视口平移它，按键在 `scroll_by_key` 之前由 `viewer_key` 处理（方向键/翻页键平移，`+`/`-` 缩放，`0`/Home 复位）。边界逐轴：内容大于查看器时其边缘不进入查看器，小于时居中，`margin` 允许再多移动；平台视口回弹（`OverscrollStyle::Bounce`）时手指、触控板与捏合可越过边界与缩放限制、越拉越硬，松开后回弹。松开时逐轴按平台的 fling 曲线惯性滑行（`Motion`），到边界回弹或停下；捏合越限则以对数匀速的缩放、焦点直线的滑行（`Glide`）回到限制内，`move_viewer`（`ViewerMove`：复位、绕点缩放、平移、双击切换）同样滑行，减弱动效下立即到位。竞技场中触摸拖动若沿出发方向内容无余量而其上的视口有，`drag_leads_at` 放弃拖动交给平移（缩放回静止的照片在翻页器里翻页）；按下滑行中的查看器会接住它。运动随 `tick_scroll_motion` 推进，计入 `is_scroll_moving`；布局后 `fit_viewers` 让静止的查看器按新盒子重新约束。

**滑动行**（`viso_ui::row_gesture::swipe`，部件 `SwipeRow`）：一个行节点（`NodeStore::set_swipe_row`，部件 `SwipeParts`：内容与两端的动作面板，及哪端的全滑执行首个动作）只接受横向拖动（`set_drag_axis(Row)`，竖向移动交给列表平移）。面板铺满整行、层级 -1 画在内容之下，平时被平移到行外一整行宽（行裁剪），内容滑开时面板的边缘贴着内容的边缘跟进；所有移动都写成内容与面板的 translate，只标 `TRANSFORM`。拖动样本在 `settle_drag` 中驱动它：内容跟随指针到动作的总宽，再往外按橡皮筋缓动；允许全滑的一端可一直拖到整行宽。松开时：越过全滑标记（行宽 55%，至少超出动作 48pt）则滑出整行、在下一帧经 `KeyRouter::route_row_gestures` 以 `RowGesture::Swiped(edge)` 告知行的按键处理器，停留片刻后弹回；按 `SWIPE_SPEED` 甩向打开方向或停在动作一半以外则打开，否则关闭，均为临界阻尼弹簧（`SPRING`），减弱动效下立即到位。同一时间只开一行：拖动另一行、或在动作之外按下都会关闭它，按下落在该行所在列表内时作为接住（`catch_coast`，不点击任何控件），按在动作上则在抬起后关闭。触控板横向为主的双指滑动在 `route_scroll` 中先于视口驱动行（整段手势归行），其他滚动关闭打开的行。焦点进入某端的动作时行在该端打开，离开后关闭；Escape 关闭焦点所在的行（`swipe_key`，先于 `scroll_by_key` 的视口处理）。“前端”在从右到左的行中为右侧。运动随 `tick_scroll_motion` 推进并计入 `is_scroll_moving`；布局后 `fit_swipes` 按新的行宽与动作宽度重新放置。

**行重排**（`viso_ui::row_gesture::reorder`，部件 `reorder_handle` 与 `Scroll`/`VirtualList` 的 `on_reorder`）：可重排列表（`NodeStore::set_reorder_list`）以其子节点为行，滚动视口则以其内容节点的子节点为行（`VirtualList` 的行宿主亦然）；行的位置取语义位置（虚拟列表宣告的序号），否则取其在兄弟中的次序。把手（`set_reorder_handle`）是行内任意节点，自身接受拖动；拖动样本在 `settle_drag` 中驱动：起拖时行被提起（`stack_layer` 加 2，画在并命中于兄弟之上），沿列表主轴跟随指针（夹在行容器之内），落点按其余各行布局中点决定；其余行以 `±(行长+间距)` 的偏移让位，均为临界阻尼弹簧。所有移动都是行自身 translate 之外叠加的偏移（与 sticky 的 pin 一样经 `pin_of` 合成），只标 `TRANSFORM`。指针停在视口两端 40pt 内时按深度以至多 800pt/s 滚动视口，滚轮也可滚动，行始终保持在指针下（偏移计入视口滚动量）。松开后行弹入空位，全部静止后在下一帧以 `RowGesture::Reordered { from, to }`（`to` 为去掉该行后的位置）告知列表的按键处理器，告知时所有偏移归零、由 Owner 在同一帧按新顺序重建；落回原位不告知。键盘：焦点在把手上按 Space/Enter 提起，沿主轴的方向键逐位移动并使落点保持可见，Space/Enter 放下，Escape、焦点离开把手或任意按下取消。虚拟列表在行被提起期间把它保持挂载（`held`，滚出窗口也不回收），放下后的下一次 reconcile 回收。运动随 `tick_scroll_motion` 推进并计入 `is_scroll_moving`；布局后 `fit_reorders` 按新布局重新计算。

**面板（Sheet）**（`viso_ui::sheet`，部件 `sheet`）：`NodeStore::set_sheet(id, SheetSpec { edge, detents, dismissible })` 把一个层变为面板，其第一个非 Chrome 子节点是面板体，按完整尺寸贴 `edge` 布局；`present_sheet` 显示层、困住焦点并让面板以临界阻尼弹簧从边外滑到当前停靠点（`SheetDetent::{Full, Fraction, Fixed}`，夹在面板尺寸内），关闭时滑出，到 0 后隐藏并归还焦点。滑动量写成面板 translate 之外叠加的偏移（经 `pin_of` 合成），只标 `TRANSFORM`；遮罩是层自身的填充，按露出量与最小停靠点之比调 alpha（`PAINT`）。面板可拖动（拖动轴为边的轴）：拖动样本在 `settle_drag` 中驱动露出量，越过完整尺寸硬停，低于最小停靠点按 `dismissible` 跟手或阻尼收敛；触摸从面板内的滚动视口起拖时，`drag_leads_at` 经 `sheet_leads` 在面板未全开、或视口在手指方向已无可滚时把拖动交给面板。松开后按露出量 + 速度 × 0.2s 取最近候选（停靠点，`dismissible` 时含 0），在下一帧以 `SheetTell::{Detent, Dismiss}` 经 `KeyRouter::route_sheets` 投递到层的按键处理器（`EventCx::sheet_detent` / `dismissed`）；Owner 收到 Dismiss 仍保持打开时，`fit_sheets` 让它弹回。运动随 `tick_scroll_motion` 推进并计入 `is_scroll_moving`。DSL 的 `Sheet` 把常量 `edge`/`detents`/`dismissible` 折进 `AotStyle.sheet`（加载时 `set_sheet`），`open`/`detent` 作为 Control 输入驱动 `present_sheet`/`set_sheet_detent`，`background` 写入遮罩色。

**拖放**（`viso_ui::drag_drop`，部件 `draggable`/`drop_zone`）：源（`NodeStore::set_drag_source(id, Some(DragData))`）提供数据的若干表示——`DragItem::{Text, Uri, Files, Value(DragKind, Rc<dyn Any>)}`，富者在前；`DragKind` 是名字的 FNV-1a 哈希（`DragKind::of::<T>()` 取类型名），同名即同种。目标（`set_drop_target(id, Some(DropTarget { accepts, operation }))`）取用 `accepts` 中的表示（空为任意）。鼠标在源上按下时武装（`arm_drag_source`），越过鼠标阈值提起；直接接触不武装，而是经长按竞技场：源算作接受长按的节点，长按到期时 `hold_lifts` 提起，因此列表里快速的手指仍然平移。提起后会话（`Session`）占用该指针：`route_contact` 在阈值计算之后把样本交给 `steer_drag_drop`，任何节点都收不到它；鼠标移出窗口不取消，直接接触的 Leave 取消。预览（`set_drag_preview(source, Some(layer))`，与源分开登记，源可以之后才成为源）是一个层，平时隐藏且不可命中；提起时在源的位置显示，随指针从抓取点跟随，其位移是层自身 translate 之外叠加的偏移（经 `pin_of` 合成），只标 `TRANSFORM`；命中测试跳过它。指针下最内层取用数据的目标即“在其上方”（`drop_over`，交互状态 `Interaction::DropOver`），进入与离开以 `DropPhase::{Enter, Leave}` 告知；松手时目标收到 `Drop`，`DropEvent.taken`/`taken_as` 为它按自身 `accepts` 顺序取用的第一个表示；没有目标或取消时数据放回，预览以临界阻尼弹簧（`SPRING`）弹回源上后隐藏。源随后收到 `DragEnd { operation }`。键盘：`carry_key`（`drag_key`、`reorder_key`、`swipe_key`，窗口在移动焦点或关闭层之前先交给它）在聚焦的源上以 Space/Enter 提起到阅读顺序中第一个取用它的目标，Tab/方向键在目标间移动、Space/Enter 放下、Escape 放回；焦点离开源（`drag_focus`）放回。告知在决定它的输入之后经 `KeyRouter::route_drops` 投递到目标与源的按键处理器（`KeyPayload::{Drop, DragEnd}`，`EventCx::drop_event`/`drag_end`）。运动随 `tick_scroll_motion` 推进并计入 `is_scroll_moving`；布局后 `fit_drag_drops` 遗忘离开树的源、目标与预览，源离开树的拖动被放回。DSL 的 `drag`/`drag_kind`/`drop_accepts`/`drop_operation` 是 Control 节点输入，`DragPreview` 把自己登记为父节点的预览层。

**系统拖入**（`viso_platform::drop`）：别的应用拖到窗口上的文件、链接与文本，后端按步骤发出 `RawEvent::Drop(RawDrop { window, phase: DropPhase::{Over, Leave, Drop}, x, y, offer, reply })`，坐标为逻辑点；`DropOffer` 总给出种类（`DropKinds { text, uri, files }`），内容只在系统提供时给出（多数系统放下时才给）。一次经过共用一个 `DropReply`：应用处理每一步后在其中写下“此处放下会做什么”（`Option<DropAction>`），后端按来源允许的动作（`DropActions::settle`）回答系统。能在系统回调里同步驱动应用的后端（macOS `NSDraggingDestination`、Windows `IDropTarget`、web 的 `dragover`/`drop`、iOS `UIDropInteraction`）当场回答；X11（XDND 5，`XdndStatus`/`XdndFinished`）与 Wayland（`wl_data_offer.accept`/`set_actions`/`finish`）在处理器返回后的 `After::Dropped` 回答；Android 的 `onDragEvent` 只在放下时同步询问一次（`nativeDrag` 等待循环线程）。放下时内容在后台读取的平台（Wayland 的管道、iOS 的 item provider、Android 的 content URI 复制到缓存）把这一步暂存，读完再交给应用；只借出读取期间的文件（iOS、Android 的 content URI）先复制到临时目录。`text/uri-list` 经 `uri_list`/`file_path` 分为文件路径与第一个非文件链接；web 只给文件名。运行时把坐标换成物理像素（`InputSample::Drop`），门面以 `NodeStore::drag_from_outside(root, ArrivalPhase, at, DragData)` 交给 `drag_drop`：它与应用内会话无关（`Arrival`），按同一 `drop_over`/`Enter`/`Leave`/`Drop` 告知目标，`Over` 时以占位内容判断取用、放下时以真实内容判断，返回的 `DropOperation` 即回复。输入磁带以 `TapeInput::Drop` 记录与重放。

**拖出**：鼠标携带的应用内拖动（数据含 `Text`/`Uri`/`Files` 之一）越过窗口边缘时，`steer_drag_drop` 把会话标为待交出，帧尾门面取 `NodeStore::drag_out_asked()`，以 `DragOut { text, uri, files, actions }` 调 `PlatformApp::start_drag(window, drag) -> bool`（文本允许复制与移动，文件允许复制与链接，文件留在原处）；`drag_out_started(true)` 让它所在的目标收到离开、预览收起，此后指针样本不再到达任何节点，`false` 则留在应用内且本次不再请求。系统结束拖动时发 `RawEvent::DragEnded { window, action }`（运行时 `InputSample::DragEnded`），`drag_out_ended` 把 `DragEnd { operation }` 告知源。拖回本应用窗口的部分走系统拖入。各后端：macOS 以最近一次按下或拖动的 `NSEvent` `beginDraggingSessionWithItems`（文件为文件 URL 各一项，否则一个带 URL 与字符串的 `NSPasteboardItem`，图标取 `NSWorkspace`），`NSDraggingSource` 给出允许的动作并在结束时回报；Windows 在启动它的处理器返回后以 `SHCreateDataObject` 承载 `CF_HDROP`/`UniformResourceLocatorW`/`CF_UNICODETEXT` 并进入 `SHDoDragDrop` 的模态循环（其间窗口过程照常驱动帧）；X11 由选择窗口持有 `XdndSelection` 并作为源，指针移动时沿根窗口找到最内层 `XdndAware` 窗口，`XdndEnter`/`XdndPosition`（上一条得到 `XdndStatus` 后才发下一条）/`XdndLeave`，松开时对接受者发 `XdndDrop` 并以其 `XdndFinished` 结束，Escape 放回，`SelectionRequest` 按 `text/uri-list` 与文本类型应答；Wayland 以按下的序号 `wl_data_device.start_drag`，数据源按 MIME 写出，`dnd_finished`/`cancelled` 结束（Wayland 无链接，链接按复制）。Web 与 iOS 只能由系统手势开始拖动，Android 不提供，`start_drag` 为 `false`。无头后端接受并记录（`HeadlessApp::drags`），由脚本以 `DragEnded` 结束。

**滚动条**（`viso_ui::scroll_bar`）：`ScrollStyle`/`VirtualListStyle` 的 `bars`（默认 `ScrollBars::Auto`）让视口建一条轨道和其中的滑块。轨道是视口的 chrome 子节点（`NodeStore::is_chrome`，显式标记，不按子节点位置推断）：布局把它沿视口末端（从右向左的纵向视口在左侧）贴边放置，内容不受它影响；绘制在内容变换结束后以视口自身空间绘制，命中测试先于内容测试且不随偏移移动；视口子节点重链且含 chrome 时拼接退回整树重绘（罕见）。每次布局后同步滑块：长度为可见比例（不短于 `min_thumb`，越界时向端点压缩），经外观长度写入（仅长度变化时重排一次）；位置只写平移，滚动仍只标 `TRANSFORM`。外观由窗口的 `ScrollBarStyle` 决定：facade 按系统外观设置——`Appearance::overlay_scroll_bars`（macOS `NSScroller.preferredScrollerStyle` 及其变更通知、Windows“始终显示滚动条”`DynamicScrollbars`、GNOME `overlay-scrolling`，移动平台与 Web 恒为真）为真时取 `native(dark)`（macOS 覆盖式、iOS/Android 指示条、Windows 11/GTK 细条），否则取 `classic(dark)`。覆盖式滚动条浮在内容上，偏移变化或指针停留时显示，停止后经 `linger` 再淡出（期间再动或被按住则顺延）；经典滚动条通过 `LayoutInput::Scroll::gutter` 在内容旁留出轨道宽度，可滚动时常显。可交互样式：指针悬停时滑块加粗并显示轨道；拖动滑块偏移随之；按在轨道上向按压处翻页（视口的 7/8），按住 300 ms 后每 50 ms 继续，直到滑块到达按压处；按住跳转修饰键（Apple 为 Option，其他为 Shift）时滑块跳到按压处并可接着拖动。轨道只在显示且可交互时可命中，指示条样式不接收指针。

**双向滚动**：`ScrollStyle::across`/`VirtualListStyle::across`（部件 `ScrollViewStyle::across`，DSL `across: true`）让视口在其轴之外也横跨滚动（`LayoutInput::Scroll::across`，`NodeStore::scrolls_on(id, axis)` 判断某方向是否可滚；`scroll_axis` 仍是内容堆叠、吸附、尾随、刷新与 sticky 所沿的主轴）。布局时内容两个方向都取自然尺寸（横向上至少铺满视口，`Fill` 不再按视口截断），两向都记入 `content`，`scroll_range` 按视口减去滚动条留白计算；从右向左时内容右边与视口右边对齐、横向偏移取 `[-range, 0]`。滚轮、触控板与系统惯性按轴分别交给链上第一个该向有余量的视口；触摸平移起始方向接近某轴（次轴位移小于主轴一半）时锁定该轴，斜向起始且首个承接视口两向可滚时两向自由平移，松手两向各自惯性（只在主轴回弹，横向到边即停）；方向键按其轴、Page/Space/Home/End 按主轴；`scroll_into_view`/`Reveal` 两向都做最小滚动。`ScrollBars::Auto` 为每个可滚方向各建一条滚动条：纵条贴右（RTL 贴左）、横条贴底，两条都在时各自让出对角；经典样式的留白按方向记在 `gutter: Vec2`（`x` 为纵条宽、`y` 为横条高），`scroll_thumb_on(id, axis)` 取某向滑块。虚拟列表的行横向长度由 `NodeStore::set_list_across(viewport, extent)` 设定（画布横向 `Fixed`，不足视口时仍铺满），表格列宽之和即走此路径；滚动仍只标 `TRANSFORM`。

**绘图内容**（`viso_ui::plot`）：`Content::Plot(Box<Plot>)` 是节点的绘图内容，图层的点都存于单位方格（`x` 自起始边 0 到末端 1，`y` 自底 0 到顶 1），绘制时才映射进节点布局后的框（RTL 起始边在右），所以改尺寸只重绘（`PAINT`），线宽、点径、圆角保持 pt 尺寸。图层：`Line`（点列，可单调三次平滑，Fritsch–Carlson 斜率，不越过数据点）、`Area`（上沿与下沿或基线之间填充，可描上沿；凹形填充走渲染器的 nonzero 遮罩通道）、`Bars`（同一带内 `slot/slots` 并排、组居中，单根不宽于 `max_width`，带间与堆叠段间留 `gap`，只圆数据端）、`Dots`（可带底色环）；`PlotGrid` 画横向发丝网格线与零线，`PlotMark` 画悬停带、十字准线与带环的高亮点（`NodeStore::set_plot_mark` 只换标记、共享图层 `Arc`）。`y` 不是数的点在该处断开线与面积。一条线的点多于框的像素列数四倍时，每个像素列只取首、最低、最高、末四点（M4），视觉不变而工作量随框宽而非数据量。

**图表**（`extras/charts`，`viso-charts`）：`line_chart`/`area_chart`/`bar_chart`/`scatter_chart(label, Vec<Series>)` 与 `sparkline`。`series(name, values)` 按类别（`categories`）或下标取值，`series_xy` 取 `(x, y)` 点；数据先算成模型（刻度、轴标签、单位方格图层、每个位置每条序列的读数），再交给一个绘图节点。`y` 轴取整刻度（1/2/5×10ⁿ，按步长定小数，千以上紧凑 `2K`/`1.5M`），面积与柱包含 0；类别 `x` 轴以带中心为位置、过密时隔几个标注，数值 `x` 轴折线保持数据域、散点外扩到整刻度。轴标签与刻度对齐靠固定行高：`y` 标签列以弹性空隙均分、绘图区上下各内缩半行，`x` 标签以百分比插入定位在 `Absolute` 里。序列配色是固定顺序的八色（浅/深两套，相邻色在常见色觉缺陷下仍可分），颜色随序列而非名次，隐藏一条其余不变色；两条以上序列显示图例，每项是复选框（指针或 Space/Enter 切换显隐，坐标轴随显示的序列重算）。悬停：折线/面积在最近的 `x` 位置画十字准线并读出所有序列，柱状高亮整带，散点取 24pt 内最近的点；触摸点按即读。图表是一个 tab stop，左右（RTL 镜像）、Home/End 移动读数，Enter 触发 `on_select`，Escape 收起；读数框是 polite live region（`Role::Status`），其标签即读数文本。`ChartHandle::set_series`/`set_categories` 换数据，结构钩子重算模型、重设绘图并重建轴标签与图例。处理器只写共享状态并递增修订号。

**数据表格**（`viso_widgets::data_grid`）：`DataGrid` 是一个横跨滚动的虚拟列表，第 0 行是列头（`set_sticky_of` 钉在顶部），其余每行一条数据；每行是一条横排：先一块占位叶子顶替视口左侧未建的列，再是视口横向可见的列（两侧各多建一列）。列表每次 reconcile 用 `watch_across(lists, viewport, watch)` 告诉表格横向偏移与可见宽（`AcrossView`）及已挂的行（`VirtualListState::mounted_rows()`），表格据此逐行增删单元格（前进只拆前端、补后端，跳远才整行重建），百万行百列只挂一屏单元格。命中测试按几何算：画布世界矩形加行高、列起点（`columns::at`）定位行列，不给单元格挂处理器。列头末端 8pt 是改宽把手（`ResizeCol` 光标），拖动实时改宽；拖动列头越过 4pt 后列随指针换位；单击可排序列头升序、再点降序，`on_sort` 之后自动重建单元格。选择分 `GridSelect::{None, Rows, Cells}`：按下选中，Shift 延伸，主修饰键追加（已选中行则剔除），拖动边走边选；键盘方向键（RTL 镜像）、Home/End、Page Up/Down、主修饰键到边，Shift 延伸，Space 切换，主修饰键+A 全选，主修饰键+C 以制表符分隔复制，Enter/F2 或双击激活单元格；列头上 Enter/Space 排序，主修饰键+Shift+左右移列，Alt+左右改宽。光标所在单元格滚入视野：纵向交给列表的 `show_index`（最小滚动，避开钉住的列头，未挂的行按高度模型定位），横向按列起点算。处理器只写共享状态并递增一个修订号，结构钩子统一应用（`LOOKS` 重设填色与边框只标 `PAINT`/`SEMANTICS`，`WIDTHS` 改宽重新布局，`CELLS` 重建已挂单元格，`COUNT` 改行数）。

**表格语义**：`Role::Grid`/`Role::GridCell`（AccessKit `Grid`/`Cell`，web `grid`/`gridcell`）；稀疏列 `NodeStore::set_table_facts(id, TableFacts { extent, place, sort })` 记录表格的总行列数、行或单元格所在的行列（行只给行号）与列头的排序方向（`SortOrder`），AccessKit 映射为 `row_count`/`column_count`/`row_index`/`column_index`/`sort_direction`，web 为 `aria-rowcount`/`aria-colcount`/`aria-rowindex`/`aria-colindex`（从 1 起）/`aria-sort`，未构建的行列照样计数。表格光标单元格是视口的 active descendant（构建时 `BuildCx::active_descendant` 设置，新挂的行也能接上）。

**剪贴板**：内容是 `ClipboardData { text, html, image }`（`viso::clipboard`，类型在平台层定义、经运行时与 `viso_ui::clipboard` 再导出）：纯文本、HTML（附纯文本替代）与 `ClipboardImage`。图像或是文件字节（`ClipboardImage::encoded`，另一应用放上的 PNG/JPEG/BMP 等，原样共享）或是直通 alpha 的 sRGB RGBA8 像素（`ClipboardImage::rgba`，应用复制的，`image_from_raster(&RasterImage)` 由光栅得到）；`png()` 只在首次被要的时候编码一次（自带 deflate 的 PNG 编码器，按行自适应滤波）并缓存于共享的 `Arc` 中，克隆不复制像素。粘贴以 `ClipboardKinds { text, html, image }` 说明接受的种类，`ClipboardKinds::pick` 取双方都有的最丰富一种（图像优先于 HTML）再加上纯文本。平台接口：`PlatformApp::set_clipboard(data)` 写入；`request_paste(window, kinds, token)` 异步读取，总以 `RawEvent::Paste { window, data, token: Some(token) }` 作答（读不到为空）；粘贴手势（快捷键、编辑菜单、web 的 `paste` 事件）先发 `RawEvent::PasteRequested { window, reply: PasteReply }` 握手，处理器在回复中填所取种类（默认 `TEXT`，`NONE` 不读），处理器返回后后端读取并在有内容时发 `Paste { token: None }`；复制握手 `CopyRequested` 的 `ClipboardReply` 携带 `ClipboardData`。读取与编码都不在帧路径上：应用自己的复制仍在剪贴板上时（macOS/iOS 以 changeCount、Windows 以 `GetClipboardOwner`、Wayland/X11 以选择所有权、Android 以剪贴板标签判断）粘贴直接共享那份数据。UI 侧：`EventCx::copy(data)`/`copy_text(text)` 交出要复制的内容，门面帧尾调 `set_clipboard`；`BuildCx::paste_target(handle, kinds)`（`NodeStore::set_paste_target`）登记接受粘贴的节点；粘贴手势时焦点在文本控件则只取纯文本照旧插入，否则焦点（无焦点时为根）向上最近的粘贴目标收到其所取种类，没有目标时退回为焦点节点的 `ImeEvent::Paste`；`EventCx::request_paste(node, kinds)` 主动请求，门面以令牌对应目标节点；内容经按键处理器的 `EventCx::pasted() -> Option<&ClipboardData>` 到达。粘贴来的图像以 `Asset::from(&ClipboardImage)` 显示（文件字节直接成为资源，解码在帧外），或 `decode_image` 立即解码。平台的复制请求在焦点不在文本控件时，向焦点节点合成一次主修饰键+C（剪切为 X）按键，以其交出的内容作答。各后端：macOS 一个 `NSPasteboardItem`（`public.utf8-plain-text`、`public.html`，像素图以 `NSPasteboardItemDataProvider` 承诺 `public.png`、被要时才编码，退出前兑现），读取 PNG，否则 JPEG/TIFF 经 `NSBitmapImageRep` 转为 PNG；iOS `UIPasteboard` 一项（文本、HTML，像素图在工作线程编码后回主线程补写，期间若有新复制则放弃）；Windows `CF_UNICODETEXT`、`HTML Format`（带偏移头）、注册的 `PNG` 与 `CF_DIBV5`，像素图延迟渲染（`WM_RENDERFORMAT`/`WM_RENDERALLFORMATS`），读入的 DIB 补文件头成 BMP，alpha 全零视为不透明；Wayland 数据源提供 `image/png`、`text/html` 与文本类型，PNG 在写管道的线程上编码，粘贴每种一条管道在读线程上读完；X11 `TARGETS` 先行，再依次转换最丰富的种类与 `UTF8_STRING`（退回 `STRING`），大数据走 INCR，排队的粘贴依次进行；Web 文本用 `writeText`，有 HTML 或图像时写 `ClipboardItem`（PNG 以 Promise 交出，手势内写入、返回后编码），`paste` 事件内容保留到握手得出种类再交付，`request_paste` 用 `navigator.clipboard.read()`（浏览器可能因无手势或无权限拒绝，此时答空）；Android `ClipData` 一项（文本、HTML、图像 URI），图像在工作线程编码并写入缓存目录，由壳内 `VisoClipProvider`（`{package}.viso.clipboard`，`grantUriPermissions`）提供，读取在 Java 线程上进行并经收件箱作答；KMS 与无头后端为进程内剪贴板（无头的 `HeadlessApp::clipboard()` 供测试检查）。

**平滑滚动与尾随**：`NodeStore::glide_to` 让视口偏移沿临界阻尼弹簧（`λ=14/s`，约 `4/λ` 秒走完九成，与距离无关）滑向目标，目标每帧夹在当前范围内，到达（误差 0.5 pt 且速度 10 pt/s 以内）后落在目标上；滑行中改目标保留当前速度，不跳变。滑行与惯性同属滚动运动：只经 `set_scroll` 标 `TRANSFORM`，帧循环保持跳动，按压（速度大于 40 pt/s 时吞掉该按压）、滚轮、滚动条、即时滚动请求与新的惯性都会停止它。`request_scroll_with(id, request, ScrollBehavior::Smooth)`（`EventCx::scroll`、DSL 滚动方法的 `smooth` 参数）在布局后执行：`To` 滑向目标，`By` 在滑行目标上累加，`Reveal` 由内向外按各内层视口的目标位置计算每层所需的最小滚动；虚拟列表的 `scroll_to_index_with` 每次协调都按高度模型重新瞄准该行，直到到达或被打断。`ScrollStyle::tail`/`VirtualListStyle::tail`（DSL `tail`）注册尾随视口：首次布局直接位于末端（从右向左的横向视口为最小偏移）；每次布局后若偏移停在上次的末端（或正滑向它）而末端移动了，就滑向新末端；用户滚离末端后不再跟随，回到末端后恢复。

**下拉刷新**（`viso_ui::refresh`）：`ScrollStyle::refresh`/`VirtualListStyle::refresh`、`NodeStore::set_refreshable`（DSL 声明 `on refresh`）让纵向视口在越过起点下拉时刷新：即使窗口物理设定起点硬停、内容不足一屏也可下拉。指示器是视口的覆盖式 chrome 子节点（非固定厚度的 chrome 铺满视口），内含圆盘与 `Content::Spinner`。窗口的 `RefreshStyle` 由 facade 按平台设置：Apple 为内嵌式——内容随手指下移，辐条在露出的间隙中逐根出现，按住时间隙达到 `threshold`（90）即开始刷新，此时视口滚动区间的起点扩展 `hold`（60，`scroll_span` 的最小偏移变为 `-hold`），偏移接住间隙，内容停在下方并可像普通内容一样滚过间隙，结束时间隙转为向回弹的拉伸量后合拢；Material 为浮层式——内容不动（起点下拉既不平移也不拉伸），带阴影的圆盘随下拉滑入、弧线随之伸长并带箭头，越过阈值（64）时转亮，松手越过阈值才刷新，圆盘以临界阻尼缓动到 `hold` 处，结束时缩小消失；刷新中再下拉按物理设定拉伸。由下拉（或辅助技术的 Refresh 自定义操作，AccessKit `CustomAction`、iOS `UIAccessibilityCustomAction`）开始的刷新在下一帧经视口的按键处理器投递（`EventCx::refresh`），`set_refreshing(false)`（`EventCx`/`UpdateCx`）结束；受控视口（`set_refresh_controlled`，DSL 绑定 `refreshing` 时）的刷新若在投递后的下一次布局前未被 `set_refreshing(true)` 确认即结束。一切运动只经平移、旋转与指示器自身重绘，不触发布局。旋转器：辐条式由计时器每 125 ms 以旋转步进一格（不驱动逐帧循环），弧线式每帧推进并重绘（帧循环随之跳动）。

**吸顶**（`viso_ui::sticky`）：`NodeStore::set_sticky`（DSL `sticky`）的节点在最近的滚动视口滚过它时停在视口起点边，直到父节点的末端把它推走；每帧布局后按布局盒与滚动偏移计算一次停靠量（`sync_stickies`），停靠量是叠加在节点自身 `translate` 之上的平移，只标记 `TRANSFORM`。虚拟列表的分组标题由列表自己停靠：`section_headers`/`set_sticky_of`（DSL 行根的 `sticky` 按 Item 求值）在数据变化时求出标题行序列，Reconcile 二分查找视口起点所在分组的标题，标题行离开窗口时不回收而作为窗口外的停靠行保持挂载（回到窗口时原样归位，不重建），停靠量取 `min(scroll, 下一个标题顶 − 标题高) − 标题顶`。停靠中的节点在兄弟之上绘制与命中：其父节点按层绘制（与 Stack 的层同一路径），停靠的开始与结束重排父节点的子节点绘制。

**滚动吸附**（`viso_ui::snap`）：`ScrollStyle::snap`/`VirtualListStyle::snap`（`ScrollSnap { align, paging, proximity, inset }`，DSL `snap`/`snap_inset`）让视口停靠在其项上：项是内容的子节点，或虚拟列表的行（列表按 Fenwick 树解析，未挂载的行同样可停）；吸附点为项按 `align`（起点/居中/末端）在除去 `inset` 后的视口中的位置，夹在滚动区间内。松手时不按惯性滑行，而是取惯性本应停下处最近的吸附点，以弹簧（16/s）滑行过去，初速取松手速度但截到不越过吸附点；`paging` 时一次轻扫至多移动一项（Pager），慢速松手取最近项；`proximity` 时离吸附点超过视口 30% 则照常滑行。慢速松手、按住接住滑行后的抬起、滚轮停转 140 ms 后、触控板抬起（系统惯性被接管）都同样吸附；布局移动了视口所停的项时视口跟随该项（旋转、改变尺寸不丢页）。`VirtualListStyle::pages` 让每行恰为视口长（Pager 的页），视口尺寸变化时保持当前页。视口报告所停、所趋向或最近的项（`snap_index`），变化时经按键处理器投递（`tell_snaps`、`EventCx::snapped`，DSL `on snap_changed`，初始项不报告）；`snap_to_item`（`EventCx::snap_to_item`）滑行到指定项。页点（`viso_ui::page_dots`，`Content::PageDots`）是链接到吸附视口的一行圆点：随视口计数与点亮当前项，每个点占一个含间隙的格子，点击格子滑行到其项，获得焦点时方向键与 Home/End 逐项移动，语义为 Slider（值为 1 起的页序、范围为页数），辅助技术的 Increment/Decrement 经同一按键路径；RTL 下首点在最右。`ScrollStyle::dots`（DSL `page_dots`）把页点作为视口的覆盖 chrome 放在横轴末端边居中。部件层：`Scroll::on_snap`/`VirtualList::on_snap`，`Pager`（横向分页虚拟列表、隐藏滚动条、页点、`on_page`），`Carousel`（带间隙与内边距的横向行，吸附时保留内边距）。HTML 导出为 `scroll-snap-type`/`scroll-snap-align`/`scroll-padding`，分页为 `scroll-snap-stop: always`。

**虚拟网格**（`viso_ui::virtual_grid`）：`VirtualListStyle::grid`（`ListGrid { columns, gap, cross_gap, masonry }`）把虚拟列表的项排进横轴上的条：`Columns::Fixed(n)` 或 `Columns::Adaptive { min }`（按画布横向宽度计条数，宽度变化时在 Reconcile 中重算并把视口起点所在的项保持在视野中）。列表的位置模型 `Flow` 有三种：普通列表的 Fenwick 树；按行排列的 `LineFlow`（项按条依次填满一行，独占整行的项另起一行，行长为行内最长项加间距，Fenwick 树建在行上）；瀑布流的 `LaneFlow`（每项放到末端最小的条，独占整行的项放在最长条之下；已测量的项固定所在条；按条保存项序列，窗口由每条上的二分查找求出；测量与数据变化只标记最早的失效项，Reconcile 前从该项起一次重排）。挂载窗口仍是连续的项区间，Overscan 以行（瀑布流为条数 × 项）计。条的横向放置在布局里：`LayoutInput::AbsoluteRows` 带 `lanes` 与 `cross_gap`，行宿主的条号存于按需增长的节点列（`row_lane`，`FULL_LANE` 为整行），`row_box` 据此给出条宽与横向偏移（列方向网格在 RTL 下条从右起），单行重测仍只重放该行。分组标题与 `full_line_items`（DSL 行根 `list.full_line`）的项独占整行。部件层为 `LazyGrid`（`LazyGridStyle`、`section_headers`、`full_line`、`on_snap`）。

**键盘滚动**（`viso_ui::key_scroll`）：焦点所在路由上没有处理器取走（`stop_propagation`）、且焦点不在可编辑文本框中的按键，滚动键盘所在的视口（`NodeStore::scroll_by_key`，facade 在按键路由之后调用；无焦点时游戏帧客户端优先，未取走才滚动）：视口取焦点节点（无焦点时为指针下节点）向上最近的、能沿该键方向滚动的视口，否则窗口中最外层的视口。上下箭头滚动纵向视口、左右箭头滚动横向视口一行（40 pt），Page Up/Down 与 Space（Shift+Space 反向）滚动一页——视口长度减去保留在视野中的重叠（`max(87.5%, 长度 − 40)`），Home/End 到两端；Apple 上 Command+上下箭头到两端、Option+上下箭头翻页，其他平台 Control+Home/End 亦到两端。一律以滑行抵达，按住或在滑行抵达前再按时从滑行目标继续累加。吸附视口每个箭头移动一项，翻页键移动到一页之外最近的项（分页视口为下一页），Home/End 到首末项。控件取走自己处理的按键：按钮、复选框、开关、单选与 DSL 的勾选/展开在 Space/Enter 激活时，滑块、分隔条、页点、标签条与 DSL 的 Slider/Select 在方向键（及其使用的翻页/首末键）上，因此不会同时滚动。

**漫游组**（`viso_ui::roving`）：`NodeStore::set_roving`/`BuildCx::roving` 把一个节点标为漫游组，Tab 在其下的可聚焦节点中只停一次：落在选中（`checked`/`selected`）的成员上，否则落在组内上次持有焦点的成员上，否则第一个；从成员上 Tab 离开该组。组内的方向键由控件自己处理。`radio_group` 的每个选项是一个节点（文字为标题、`CheckShape::Radio` 为圆，与 `checkbox` 相同的单节点结构），整组是一个漫游组，方向键（左右随书写方向）在选项间循环移动焦点与选择，Home/End 到首末项。

**菜单栏与访问键**（`viso_widgets::menu_bar`，`viso_ui::access`）：菜单栏是一行标题（`menubar`），每个标题（`menuitem`，报告 expanded）是一个 `MenuItem`，其子项即它下方的菜单。按下标题打开其菜单，再按一次关闭；有菜单打开时指针划过另一标题即换成它的菜单。标签里 `&` 标出访问键（`&File` 为 F，`&&` 为字面 `&`），标题与菜单项都适用；菜单打开时键入某项的访问键即选中它（多项同键时在其间移动高亮），没有访问键时按原有的前缀跳转。访问键是框架机制：`BuildCx::access_key` 给节点登记一个字母，`BuildCx::menu_bar` 登记窗口的菜单栏；非 Apple 系统上，焦点链没有处理的 Alt+字母/数字送到树序中（焦点之后的）下一个可见、可用、在焦点作用域内持有该键的节点（`EventCx::access() == Access::Key`），单独按下又松开 Alt 或 F10 送到菜单栏（`Access::MenuBar`），Alt 按住期间各访问键节点与菜单栏收到 `Access::Cues(true/false)` 以显示/隐藏下划线；Apple 系统上 Option 用于输入字符，不参与。控件的访问键同样来自标签里的 `&`：`button("&Save")`、`checkbox`/`toggle`、`radio_group` 的选项去掉标记显示，登记访问键，Alt+键分别点击按钮、聚焦并翻转勾选、聚焦并选中选项；`labelled("&Name:", field)` 把标题放在字段前（`.above()` 放上方），以标题作为字段的可访问名称，Alt+键聚焦字段。下划线在塑形时记为 `SpanStyle::cue` 的提示线（`TextInk::cues`），绘制时只在 `NodeStore::access_cues()` 为真时画出，因此 Alt 按下/松开只重绘持有访问键节点（及其子树）的文本，不重新塑形；窗口失去焦点时提示随之隐藏。`TextRequest::cue(key)` 给任意文本的首个该字母加提示线（DSL `access_key` 用它）。没有按键处理器的访问键节点直接获得焦点（若可聚焦）。访问键作为 `Alt+键` 进入语义树（AccessKit `access_key`、Web `aria-keyshortcuts`）。键盘进入菜单栏后焦点在首个可用标题上，显示下划线；Left/Right（随书写方向）在标题间移动并跳过禁用项、循环，Home/End 到首末，Down/Enter/Space 打开并高亮首项，Up 高亮末项，键入标题的访问键打开它；菜单中阅读方向键在有子菜单的项上打开子菜单，否则与反方向键一起移到相邻标题的菜单；Escape 先关闭菜单回到标题，再离开菜单栏，焦点回到进入前的位置；选中、Tab、按在外面也都把焦点还回去。平台没有系统菜单栏（`PlatformCaps::menus == false`，X11、Wayland）时，带标题栏的窗口在标题栏下绘制 `Application::menu()`：标题与项自动取访问键（优先词首、兄弟间不重复），自定义项送 `on_menu_command`，Quit/Close Window/Copy/Cut/Paste 由窗口执行，做不到的 Hide/Minimize 不列出；加速键仍由平台按键表处理。

**应用外壳**（`viso::shell`）：窗口之外的桌面外壳能力，由 app 线程上任意位置（`Application::new`、handler、task 续体、`on_menu_command`）调用的自由函数表达：调用只记录请求并唤醒 loop，facade 在当前回调返回后（`on_launch` 装好菜单之后、每次 `on_wakeup`）把变化交给 platform，同一轮内后一次调用覆盖前一次。托盘图标 `shell::set_tray(Option<Tray>)`：`Tray { icon, template, tooltip, menu, menu_on_click }` 复用原生菜单模型 `Menu`/`MenuCommandId`，选中项与应用菜单一样经 `RawEvent::MenuCommand` 送到 `Application::on_menu_command`，单击/双击以 `RawEvent::Tray(TrayEvent)` 送到 `Application::on_tray`；`menu_on_click` 为真时任何点击都打开菜单、不再报告点击。图标是 `IconImage`（straight-alpha sRGB RGBA8，`Arc` 共享像素），`shell::icon(&Asset)` 从字节或文件资产解码；`template` 是 macOS 菜单栏的单色模板变体（系统按明暗着色），其他平台忽略。`PlatformCaps::tray` 声明可用性（桌面为真，移动端、Web、KMS 为假），`shell::tray_supported()` 读取。实现：macOS 为 `NSStatusItem`（`menu_on_click` 时菜单归 item 所有，辅助技术也能打开；否则次键/Control 点击时临时借给 item）；Windows 为 UI 线程上一个隐藏的顶层窗口上的 `Shell_NotifyIconW`（version 4），用顶层而非 message-only 窗口是因为只有顶层窗口收到 `TaskbarCreated` 广播，Explorer 重启后据此重新添加图标；Linux（仅 `x11`/`wayland` feature）在后台线程上用 zbus 在 session bus 提供 `org.kde.StatusNotifierItem` 与 `com.canonical.dbusmenu`，向 `org.kde.StatusNotifierWatcher` 注册并在 watcher 重新出现时重新注册，UI 线程只发消息，点击与选中经 loop 的 waker 回流；没有 StatusNotifier host 的桌面上不显示。headless backend 记录最近一次 tray 供测试读取。应用图标角标 `shell::set_badge(Option<AppBadge>)`：`AppBadge::Count(u32)`（0 即清除）或 `AppBadge::Label(String)`（短文本）；`PlatformCaps::badge` 声明可用性，`shell::badge_supported()` 读取。macOS 为 `NSDockTile::badgeLabel`；Windows 为每个窗口任务栏按钮上的 `ITaskbarList3::SetOverlayIcon`，backend 用 GDI 把文本（计数超过 99 显示 `99+`）画进红色圆形小图标，并在 `TaskbarButtonCreated`（新窗口、Explorer 重启）时重新设置；Linux 经同一条 session bus 后台线程发出 `com.canonical.Unity.LauncherEntry` 的 `Update` 信号并应答 `Query`，只支持计数（标签能解析为数字时按计数显示，否则不显示）；iOS 为 `UNUserNotificationCenter setBadgeCount:`（iOS 16+），否则回退 `applicationIconBadgeNumber`，同样只有计数，且需要用户已授权带 badge 选项的通知（由通知扩展申请）；Web 为 `navigator.setAppBadge`/`clearAppBadge`（运行时探测，非数字标签显示为无数字的标记，拒绝的 promise 被吞掉）；Android 不支持（launcher 由通知自行显示角标）。

**唤醒与阴影**：处理器可用 `EventCx::wake_after(node, delay, token)` 让 `token` 在 `delay` 后送到 `node` 的 key 处理器（`ev.woken()`），在那里做处理器能做的一切；它建立在一次性定时器上，帧在触发定时器后投递唤醒，不取消，过期的 token 由处理器忽略。节点可投下阴影（`BuildCx::shadow` / `NodeStore::set_shadow`，`Shadow { color, offset, blur, spread }`，`blur` 取 CSS 的模糊半径）：作为节点自身 drawable 画在盒子之下，用 renderer 的 analytic shadow（闭式高斯，无 offscreen）；冷表按 slot 排序，没有阴影的树只检查一个空表。

**目标分组**（informative；按子系统而不是按类型机械拆分）：

| 分组 | 目标内容 |
|---|---|
| controls | button、label、text_input、checkbox、radio、toggle、slider、dropdown、image、icon、progress |
| containers | row、column、flex、grid、stack、scroll、list、splitter |
| navigation | page、router_view、navigation_stack、tabs、tab_bar |
| overlays | popup、modal、tooltip、toast、sheet、menu |
| adaptive | safe_area、keyboard_avoiding、adaptive_split、adaptive_navigation、responsive_grid |
| desktop | window_shell、menu_bar、dock、resize_region |

---

## 46. Extra widgets 不污染基础 crate

以下能力建议独立 optional package/integration：

```text
PDF Viewer
Browser/CEF
Markdown engine
Map
Charts
Code editor（可以官方但单独 crate）
Video editor
XR
```

它们可以在 monorepo，但不是 `viso-widgets` 的默认 compile dependency。

---

## 47. Widget 性能合同

官方 widget 必须：

- 不在普通 `paint` 每帧 heap allocate；
- 不在事件热路径字符串找 child；
- 不持有任意 `Rc<RefCell<Node>>`；
- 有明确 DirtyClass；
- TextInput 使用 text subsystem；
- List 必须 virtualized；
- animation 不默认引发布局；
- accessibility 信息完整。

复杂 widget 的目录按“子系统”拆（例如 `dock/`、`file_tree/`，或将来的 `text_input/{edit,selection,ime,layout,paint,semantics}.rs`），简单 widget 保持单文件（`button.rs`）。

---

# Part XVI — Platform 与 Services

## 48. Platform 保持窄

当前目录（`crates/platform/src/`）：

```text
lib.rs  event.rs  handler.rs  control.rs  menu.rs  time.rs  wake.rs
accessibility.rs  access_mirror.rs            # AccessKit 桥（ADR 0030）
backend/
├── mod.rs  headless.rs  main_queue.rs  memory_pressure.rs  utf16.rs
├── macos.rs  macos_access.rs
└── ios/  windows/  linux/  android/  web/
```

窗口/表面句柄类型在 `viso-handle`；系统字体不在 platform（ADR 0026，§37.1）。各 backend 初期不拆成独立 crate，只有当某 backend 的依赖、构建或维护成本足够大时再拆。

### 48.1 Backend 支持等级与 capability contract

Viso 不把“跨平台”理解成所有设备都走同一个最低能力后端。Viso 1.0 定义以下主路径：

| Platform | Tier-1 backend | 目标 | 实现（`viso_gpu::Backend`） |
|---|---|---|---|
| macOS / iOS | Metal | first-class / performance baseline | `MetalBackend`（`target_vendor = "apple"`） |
| Windows | D3D12 | first-class / performance baseline | `D3D12Backend` |
| Linux | Vulkan | first-class | `VulkanBackend` |
| Android | Vulkan | first-class；设备能力不足时由兼容策略处理 | `VulkanBackend` |
| Web | WebGPU | first-class | `WebGpuBackend`（`wasm32-unknown-unknown`） |
| Embedded Linux（GPU） | GLES 3.0+ / Vulkan 1.1 | first-class（`embedded-gpu` profile） | `Gles3Backend` / `VulkanBackend` |
| Embedded Linux（无 GPU） | CpuRaster | Tier-2（`embedded-cpu` profile） | `CpuRaster` |
| 其它 target | — | 测试 / 回退 | `CpuRaster`（原 `CpuRaster`） |

编译期静态选择规则见 [ADR 0029](docs/adr/0029-gpu-backend-static-selection.md)。

Tier-2 compatibility backend 可以存在，例如 Linux OpenGL、Android GLES 或 Web compatibility renderer，但遵循三条规则：

1. 不得降低 Tier-1 renderer/RHI 的能力模型；
2. 允许声明 reduced capability profile，并由 tooling 给出清晰诊断；
3. 是否投入 Tier-2 由真实 adoption/device telemetry、维护成本和 benchmark 决定，而不是为了纸面“全覆盖”提前背负多个后端。

因此 Viso 的长期策略是 **WebGPU-first，不把 public architecture 写死成 WebGPU-only**；Linux 同理是 Vulkan-first，而不是永远禁止兼容后端。

### 48.1.1 Embedded Linux platform backend（`backend/linux/kms`）

没有 compositor 的全屏单 surface backend，与 Wayland kiosk（cage/weston）并列；Wayland 继续走现有的 wayland backend。

- **显示**：DRM/KMS atomic modesetting。GPU 路径用 GBM + EGL；CPU 路径用 dumb buffer 双缓冲。page flip 完成事件就是 vsync 信号。支持 `FB_DAMAGE_CLIPS`（§30.3）。视频与光标优先使用硬件 overlay/cursor plane。面板旋转优先使用 KMS plane rotation，硬件不支持时由 renderer 在根 transform 上处理。
- **输入**：直接读取 evdev（multi-touch protocol B、按键、相对设备、旋钮/encoder），libinput 是可选 adapter；hotplug 经 udev netlink。旋钮与物理按键映射为标准 focus navigation 事件，嵌入式 UI 的 focus 路径因此与桌面键盘导航是同一套。
- **事件循环**：drm fd、evdev fd、timerfd、`LoopWaker` 的 eventfd 都进同一个 epoll；空闲时 0 wakeup，不轮询。
- **Session**：可选 seatd/logind 获取 DRM master，也支持以专用用户直接打开 `/dev/dri` 运行（嵌入式常见）。
- **不提供**：多窗口、X11、桌面 portal、系统菜单。这些能力以 capability 声明为不可用，tooling 在构建期报 diagnostic。

实现合同（`viso-platform` 的 `kms` feature；没有 `WAYLAND_DISPLAY` / `DISPLAY` 时自动选用，`VISO_SESSION=kms` 强制；embedded profile 关闭 facade 默认的 `desktop` feature，X11、Wayland、desktop portal 与 AT-SPI 都不编译，桌面 Linux 的默认 build 三者并存）：

- **窗口即 scanout**：唯一窗口的 `raw_handle()` 是 `RawWindowHandle::Scanout`，指向平台的 `Scanout`（`viso-handle`）。CpuRaster 每次 present 调 `present_pixels(像素, stride, damage)`，平台把每个 dumb buffer 缺失的区域（本帧 ∪ 它错过的帧）拷进 back buffer，再提交；GLES 后端经 `Scanout::gbm()` 拿到 GBM device/surface（EGL `EGL_PLATFORM_GBM_KHR`，config 的 `NATIVE_VISUAL_ID` 等于扫描格式 fourcc），swap 之后调 `presented(damage)`，平台锁定 front bo、为它建 FB 并提交。两条路径都只在 page flip 完成后释放被替换的 buffer。
- **DRM 不经 libdrm**：直接 ioctl（结构尺寸与 ioctl 号由单测钉住）；atomic 优先，驱动没有 atomic 时退回 `SETCRTC` / `PAGE_FLIP`（`VISO_KMS_LEGACY` 可强制）。atomic 提交带 `FB_DAMAGE_CLIPS`，首帧与恢复时 `ALLOW_MODESET`，其余 `NONBLOCK | PAGE_FLIP_EVENT`。libgbm、libseat 运行时 dlopen；CPU 路径不加载 libgbm。
- **帧节奏**：flip 在途时 `request_redraw` 只记标记，flip 完成事件到达后才投递 `RedrawRequested`；空 damage 不提交、不 flip。
- **旋转**：`VISO_KMS_ROTATION` 先用 plane `rotation`（启动时用 `TEST_ONLY` 提交探测，部分 plane 只能旋转 tiled buffer）；不支持时 CPU 路径在拷贝时旋转，GPU 路径由 `Scanout::turns` 告诉 GLES 后端在 clip space 旋转、scissor 与 damage 一并换算，fragment 看到的仍是图像坐标。触摸坐标按 `VISO_KMS_TOUCH_ROTATION`（默认同显示）换回图像坐标；硬件光标图像与热点同样旋转。
- **输入**：evdev 设备按声明的事件码分类——键盘/按键、鼠标、旋钮（无左键的 REL 设备 → Tab / Shift+Tab）、触摸屏（MT protocol B，`SYN_DROPPED` 后用 `EVIOCGMTSLOTS` 重同步；或单点 `BTN_TOUCH`）、绝对指针、触摸板（单指移动 + 按键）。按键经 xkbcommon 默认 keymap（`XKB_DEFAULT_*`）出文本，缺 xkbcommon 时退回 US 布局；重复键用内核 autorepeat，不设定时器。装有 libinput 时（运行时 dlopen，`VISO_KMS_INPUT=evdev` 关闭），鼠标、触摸板、触摸屏与绝对指针改由 libinput 读取（path context，设备经 seat 打开，与 evdev 设备共用同一个 epoll）：指针加速、触摸板 tap / 双指与边缘滚动（像素级 `Scroll`）/ 防误触、触摸屏校准矩阵（`VISO_KMS_TOUCH_CALIBRATION`）；键盘与旋钮仍走 evdev 解码器（保留内核 autorepeat 与 focus 步进），libinput 拒绝的设备也退回解码器；VT 切换时 `libinput_suspend` / `resume`。
- **热插拔**：内核 uevent netlink（不依赖 udev 守护进程）：输入设备增删（权限未就绪时短暂重试），connector 变化重新探测；断开/恢复以 `Suspended` / `Resumed` 报告，mode 尺寸变化以 `SurfaceDestroyed` → `ScaleFactorChanged` → `SurfaceCreated` 报告。
- **Session**：有 seatd/logind 时经 libseat 打开设备并在 VT 切换时暂停/恢复（暂停期间不碰 card，恢复时重开输入设备、重设 mode 并显示最新帧）；否则直接打开节点，并把控制 VT 切到 `KD_GRAPHICS` + `K_OFF`，退出时恢复。
- **能力**：`PlatformApp::capabilities()` 返回 `PlatformCaps::FULLSCREEN`（单窗口、无菜单栏但 accelerator 仍生效、无 portal、应用内剪贴板、无 IME）；`framed_windows()` 为 false。
- **未做**：视频 overlay plane（平台尚无视频 surface）；触摸板 pinch / swipe 手势（`RawEvent` 尚无手势事件）。

### 48.1.2 手柄

`viso_platform::gamepad`：每个 backend 以同一标准布局报告手柄——四个按位置命名的面键（`South` 为下方键）、十字键、两个肩键、两个模拟扳机（`0..=1`）、两根摇杆（各轴 `[-1, 1]`，`x` 向右、`y` 向上）与 select / start / home。摇杆过一个半径 0.1 的径向死区：区内读为正中，静止的手柄不产生事件；区外是设备原值、不缩放，游戏 `InputMap` 的死区因此保持原义。扳机越过 0.25 时其按钮位按下，回落到 0.15 以下时松开。

- **轮询**：Runtime 在驱动 `wants_gamepads()` 时每帧帧首调用一次 `PlatformApp::poll_gamepads(&mut Vec<GamepadEvent>)`（Loop 复用同一缓冲，稳态零分配），只拿到变化：`Connected(GamepadInfo { id, name, rumble })`、`Changed(id, GamepadState)`（整份新状态）、`Disconnected(id)`。第一次调用才启动 backend 的手柄支持；两次调用之间发现的连接经 `LoopWaker` 唤醒 Loop，`Wakeup` 时也轮询一次。`GamepadId` 对一次连接唯一，进程内不复用。手柄状态只在帧首与 `Wakeup` 时读：运行中的游戏会话每帧都要帧，因而每帧读到；没有帧的 Loop（暂停的游戏、静止的 UI）不为状态变化醒来，只为连接醒来（Android 的手柄输入本身是推送的，例外）。
- **震动**：`PlatformApp::rumble_gamepad(id, Rumble { strong, weak, duration })`（`RuntimeCx` 同名），设备不支持时返回 `false`。
- **交付**：facade 把连接数写进 `env.input.gamepad_available`；`Changed` 只交给持有输入焦点的窗口、且没有 UI 节点接走按键时的 Frame Client（`FrameClient::gamepad`），`Disconnected` 交给所有窗口。游戏会话把所有手柄合并为本地玩家（按钮按位或，每根摇杆取推得最远者），经 `Scheduler::pads(held)` 与 `Scheduler::stick` 进入 `InputLatch`。反方向，System 的 `kit.rumble` 落在 Stage 上，`FrameClient::take_rumble` 在帧客户端跑完后交出最后一次请求，facade 在窗口持有输入焦点时经 `RuntimeCx::rumble_gamepad` 发给每个已连接的手柄；Rust 应用也可直接调用 `RuntimeCx::rumble_gamepad`。
- **macOS / iOS**：`GameController.framework` 在第一次轮询时 `dlopen`，不在链接期依赖；有 extended gamepad profile 的 `GCController` 是一个手柄。元素在连接时查好并持有，轮询只读各元素的 `value` / `isPressed`；连接与断开通知（block observer）标记列表过期并唤醒 Loop，只有那时才重读 `+[GCController controllers]`。面键按位置（`buttonA` 为 `South`、`buttonX` 为 `West`），`buttonOptions` 为 select、`buttonMenu` 为 start、`buttonHome` 为 home。震动经控制器 `haptics` 的 CoreHaptics 引擎播放一个连续事件（强度取两个马达的较大者，锐度取弱马达所占比例）。
- **Windows**：XInput 在第一次轮询时动态加载（`xinput1_4.dll`，否则 `xinput1_3.dll` / `xinput9_1_0.dll`），四个槽位即四个手柄。已连接的槽位每次轮询读一次，包序号不变就不做任何事；探测空槽位很慢，因此第一次轮询探测全部四个，之后每 250 ms 只探测一个空槽位（新手柄至多约 1 秒被发现）。有隐藏导出 `XInputGetStateEx`（序号 100）时读出 guide 键作 home。震动设两个马达，到期后的那次轮询停下；没有帧时 pump 的等待被截到到期时刻，`Wakeup` 时的轮询停下它。GameInput（`gameinput.dll`）只随 GDK 再分发或较新的 Windows 11 提供，没有 Windows SDK 之外的稳定 ABI，未采用。
- **Linux**：直接读 evdev，不依赖 udev。键位含 `BTN_SOUTH` 的 `/dev/input/event*` 是手柄，以非阻塞方式打开（可写时可震动），每次轮询读到 `EAGAIN` 为止，报告在 `SYN_REPORT` 完整，`SYN_DROPPED` 之后经 `EVIOCGKEY` / `EVIOCGABS` 整体重读。`/dev/input` 上的 inotify（`IN_CREATE` / `IN_ATTRIB` / `IN_DELETE`）发现新节点与权限迟到的节点；读失败即断开。面键按内核 gamepad 布局的位置，只有 `xpad` 驱动（sysfs 的 driver 链接）把 Xbox 左键报为 `BTN_NORTH`、上键报为 `BTN_WEST`，对它交换；摇杆为 `ABS_X`/`ABS_Y` 与 `ABS_RX`/`ABS_RY`，扳机为 `ABS_Z`/`ABS_RZ`（或 `ABS_BRAKE`/`ABS_GAS`），没有 `ABS_RX` 的手柄 `ABS_Z`/`ABS_RZ` 是右摇杆；十字键为 `ABS_HAT0X`/`ABS_HAT0Y` 或 `BTN_DPAD_*`；只有数字扳机时按下即满行程。震动为每个手柄上传一个 `FF_RUMBLE` 效果（`EVIOCSFF`，时长由内核计）并播放。KMS 的输入分类把手柄留给这里，不当键盘。inotify 的 fd 不在各 backend 的等待集中，空闲时插入的手柄在下一帧被发现。
- **Android**：第一次轮询经 JNI 让 activity 注册 `InputManager.InputDeviceListener`，先报告已连接的、再报告每次增减；来源含 `SOURCE_GAMEPAD` 或 `SOURCE_JOYSTICK` 的非虚拟设备是手柄（有马达时可震动）。此后 view 把手柄的按键（`KeyEvent.isGamepadButton` 或来自手柄的键，不含重复）与摇杆的 `ACTION_MOVE` 交给原生侧，不再当键盘与指针；Loop 线程据此累积每个设备的状态，自上次轮询后第一次有变化时唤醒 Loop，轮询时报告。布局按 Android 的位置：`BUTTON_A` 为 `South`，左摇杆 `AXIS_X`/`AXIS_Y`，右摇杆 `AXIS_Z`/`AXIS_RZ`，十字键为 `AXIS_HAT_X`/`AXIS_HAT_Y` 或 `DPAD_*` 键，扳机取 `AXIS_LTRIGGER`/`AXIS_BRAKE`（左）与 `AXIS_RTRIGGER`/`AXIS_GAS`（右）的较大者，没有扳机轴的手柄按下扳机键即满行程。震动经设备的 `VibratorManager`（API 31 起，第一个马达为强、其余为弱）或 `Vibrator` 播放一次性效果。
- **Web**：Gamepad API。第一次轮询时在 window 上监听 `gamepadconnected` / `gamepaddisconnected`，它们标记列表过期并唤醒 Loop（离开的 index 另记一份，即使轮询前同一 index 已被新手柄占用也先断开旧的）；只在有手柄连接或列表过期时每次轮询调用一次 `navigator.getGamepads()`，手柄的 `timestamp` 不变就不读。浏览器要在页面收到某手柄的输入后才暴露它，所以已插着的手柄在第一次按键时连接。`"standard"` mapping 按 Gamepad API 的位置读（按钮 0–16、轴 0–3，y 轴向下，这里翻成向上）；其他 mapping 按同样的下标尽力读。扳机按钮位跟随其模拟值（经共同的滞回），不用浏览器的 `pressed`。震动在有 `vibrationActuator` 的浏览器上播放 `"dual-rumble"` 效果，停止为 `reset()`。
- **Headless**：`HeadlessGamepads` 由测试连接、设置状态与断开手柄，每步落在一次轮询上，经与真实设备相同的标准化；记录收到的震动。

### 48.2 Platform 不负责

- Widget；
- Viso DSL compiler；
- app navigation；
- business networking；
- Studio protocol；
- high-level media UI；
- renderer batching；
- 系统字体发现、fallback 与字体解析（ADR 0026）。

---

## 49. Services 层

业务能力统一通过 service protocol：

```rust
cx.services().haptics().play(Haptic::Light)?;
cx.spawn(cx.services().launcher().open_url("https://viso.dev/"));
```

当前协议见 §49.3（ADR 0031）。文件对话框、分享、通知、安全存储等不进核心，由 `viso-ext` 插件提供（`viso-file-picker`、`viso-share`、`viso-local-notification`、`viso-secure-storage`），app 按需依赖。

### 49.1 为什么 service 可以用 dyn

这类调用不是 frame hot path。使用：

```rust
Arc<dyn ClipboardService>
```

在工程性上可能优于把所有 OS 特化写进泛型链。

性能优化应该集中在真正热点，而不是追求全项目零 virtual call。

### 49.2 Mockability

所有 service protocol 必须可注入 mock/fake，用于：

- headless tests；
- deterministic tests；
- Studio preview；
- AI agent；
- permission denied paths。

### 49.3 协议与实现（ADR 0031）

`viso-services` 定义两个 object-safe 协议：haptics、launcher。`Launcher::open_url` 把绝对 URL 交给系统打开它的应用（macOS `NSWorkspace`、iOS `UIApplication`、Windows `ShellExecuteW`、Linux OpenURI portal 回退 `xdg-open`、Android `ACTION_VIEW`、Web `window.open`），拒绝 `file:`、`javascript:`、`data:`、`vbscript:`、`blob:`、`about:` 等在本机执行或读取的 scheme（`Denied`）。文字中被激活的链接若沿路由无处理器调用 `EventCx::prevent_default`，store 记下其目标，facade 每帧取出并交给 `open_url`。异步调用返回一次性的 `Reply<T>`：它是 `Future`，也可以不经 executor 用 `try_take` 轮询。配对的 `Completer` 是 `Send`，可以在 OS 回调所在的任意线程完成；被丢弃而未应答时，结果为 `Cancelled`。

`Services` 是注册表，每个协议占一个固定字段。`Services::system(app)` 按编译目标选择 OS 实现，`Services::unsupported()` 对所有调用返回 `Unsupported`，`Mock` 用于 headless 测试并记录调用。

Android 通过 `viso-platform` 暴露的两个窄接口访问 activity：`with_activity` 和 `VisoActivity.addResults`。其余逻辑都在 services 内，包括随 APK 打包的 `dev.viso.services.VisoServices` Java 类（触感与 `ACTION_VIEW`）。框架外的扩展 crate（`viso-ext`）经 `ndk-context` 拿到 Java VM 与当前 activity：platform 在每个 activity 启动时设置它（activity 重建时先 release 再设为新的全局引用），扩展自带的 Java 以 `InMemoryDexClassLoader` 载入，经 `VisoActivity.addResults` 收到授权与 activity 结果。

handler 通过 `cx.services()` 取得会话注册表，并用 `cx.spawn_then` 等待 `Reply`（ADR 0032，§13.2）。

---

## 50. Mobile lifecycle

Runtime 提供标准事件：

```text
Launching
Active
Inactive
Background
Foreground
Terminating
LowMemory
SurfaceLost
SurfaceRestored
```

UI 层提供：

```text
SafeArea
KeyboardInset
Orientation
SizeClass
```

平台 service 提供：

```text
Permissions
Push
DeepLink
Share
Camera
Location
```

业务代码不应该在页面中到处 `#[cfg(target_os = ...)]`。

---

## 50A. Device Class、Footprint Profile 与嵌入式（ADR 0044）

### 50A.1 为什么在 1.0 设计

- **市场**：车机、工控 HMI、家电与智能家居面板、医疗与 POS 设备，是 Rust UI 最有机会的场景之一；目前主要由 Qt、Slint、LVGL 覆盖。
- **架构契合**：retained tree、构建期 AOT、稳态零分配、静态 backend 选择、idle 零唤醒，这些本来就是嵌入式需要的性质，Viso 不需要为嵌入式另起一套模型。
- **成本不对称**：现在定下 MCU-ready 不变量几乎没有成本；等 core 写完再补，就要重写 arena、store、text、AOT 加载这些部分。
- **收益外溢**：damage present、固定容量 arena、profile 裁剪、CPU raster 同样降低桌面和移动端的内存与功耗。

### 50A.2 Device Class

| Class | 典型硬件 | OS | 图形 | RAM | 1.0 |
|---|---|---|---|---|---|
| Desktop | x86_64 / arm64 PC | macOS / Windows / Linux | Metal / D3D12 / Vulkan | — | Tier-1 |
| Mobile | 手机 / 平板 | iOS / Android | Metal / Vulkan | — | Tier-1 |
| Web | 浏览器 | — | WebGPU（CpuRaster 兼容） | — | Tier-1 |
| Embedded-GPU | Cortex-A53/A55/A72 级；Mali-G52/G610、Vivante GC7000、PowerVR、VideoCore VI（例如 i.MX 8M Plus、RK3566/RK3588、树莓派 4/5） | Linux（KMS 或 Wayland kiosk） | GLES 3.0+ / Vulkan 1.1 | ≥ 128 MB | Tier-1 |
| Embedded-CPU | Cortex-A7/A35 单核到双核，无 GPU 或只有 GLES2（例如 i.MX 6ULL、全志 T113） | Linux（KMS dumb buffer / fbdev） | CpuRaster + 可选 2D blitter | ≥ 32 MB | Tier-2 |
| MCU | Cortex-M7/M33、ESP32-S3、RP2350 | bare-metal / RTOS | line/tile buffer + DMA2D | 256 KB – 8 MB | 不在 1.0；core 保持 MCU-ready |

### 50A.3 Footprint Profile

Profile 是一组固定的编译期 feature 加上运行时预算默认值。用户只选择 profile，CI 只验证这几组组合（§5）。

| | desktop | mobile | web | embedded-gpu | embedded-cpu |
|---|---|---|---|---|---|
| GPU backend | 原生 Tier-1 | 原生 Tier-1 | WebGPU / CpuRaster | GLES3 / Vulkan | CpuRaster |
| 包内 shader 语言 | 后端语言 + `None` | 后端语言 + `None` | WGSL + `None` | GLSL ES / SPIR-V | 仅 `None` |
| 默认 worker 数 | N | N | 0–N | 0–2 | 0 |
| 系统字体 provider | 是 | 是 | 否 | 否（可开） | 否 |
| a11y OS bridge | 是 | 是 | ARIA mirror | 可选 | 否（可开） |
| behavior VM（Release） | 否 | 否 | 否 | 否 | 否 |
| MTSDF / vector glyph | 是 | 是 | 是 | 否 | 否 |
| arena 容量 | 增长 | 增长 | 增长 | 增长 / 固定 | 固定 |
| hot reload | Dev artifact | Dev artifact | Dev artifact | Remote Dev | Remote Dev |

示意配置（字段语法以 `Viso_CLI.md` 为准，目标）：

```toml
[target.panel]
profile = "embedded-gpu"
triple  = "aarch64-unknown-linux-gnu"
display = { width = 1024, height = 600, format = "xrgb8888", rotation = 90 }
budget  = { heap = "48MiB", glyph_atlas = "4MiB", images = "16MiB" }
workers = 1
fonts   = { charset = ["latin", "gb2312"] }
```

### 50A.4 Memory budget 是运行时合同

- Face SLRU、glyph atlas、image cache、paragraph cache、paint cache 都从统一的 `MemoryBudget` 领取字节预算，超出时按既有合同从冷到热逐级释放（§37.4）。profile 只提供默认值，app 可以覆盖；
- GPU 资源（texture / buffer 字节）同样计入预算；
- 可选的 accounting allocator（profile feature）统计 heap 峰值。超过 hard budget 时发出 `BudgetExceeded` 诊断事件并在 Inspector 中显示，不静默 OOM；
- 预算违规必须能在桌面仿真中复现（§50A.7）。

实现合同：
- profile 在构建期固定：`viso build <target>` 把 target 段的 `profile` / `budget` / `workers` 以 `VISO_FOOTPRINT` / `VISO_BUDGET` / `VISO_WORKERS` 交给 facade 的 build script 烘焙进二进制；普通 `cargo build` 按 target 与后端 feature 推断 profile。profile 与后端 feature 不一致（例如 `embedded-cpu` 带 `viso/gles`）是构建错误；后端的 shader 语言不在 profile 允许的集合内由 `const` 断言拒绝；
- `MemoryBudget { heap, gpu, faces, shaping, glyph_atlas, images }`：profile 给默认值，target 的 `budget` 覆盖，`Application::memory_budget()` 在第一个窗口打开前读一次。`faces` / `shaping` 直接成为 face SLRU 与 shaping cache 的字节预算，`glyph_atlas` 决定 atlas plane 边长（整页取整）；
- 运行时图片来源：`Asset::bytes`（运行时给出的字节，每次构造是一个独立 resource，保留 `Asset` 即可复用其解码）与 `Asset::file`（路径即 resource，build 时只登记，worker 在帧路径外读取——至多 1 GiB——再解码；无 worker 时在 idle slice 内读取）；与 `Asset::url`（URL 即 resource；核心不联网，字节由应用的 `Application::image_loader`——任意客户端，如 `viso-net`——在 loop 线程上以不需要 runtime 的 future 取回，future 唤醒 loop、`receive` 时轮询，取回后交 worker 解码；没有 loader 或取回失败即 failed）；Web 构建不链接 AV1 解码器，AVIF 交给浏览器解码（`viso_platform::browser::decode_image`：`createImageBitmap` 后经 canvas 读回 straight RGBA8，再在线性光里 premultiply），同样是 loop 线程上的 future，序列只取首帧；读不到或无法解码的资源在每个显示它的 table 里记为 failed（`AssetDraw::Failed`，不重试；已有旧修订的 texture/scene 时继续画旧的），asset node 可登记 loading / failed 两个替身节点（`set_asset_fallbacks`），随所画内容切换 hidden；`NoticeKinds::ASSET` 在布局后投递 `AssetLoaded { size }` / `AssetFailed`，`Image` widget 的 `placeholder` / `error` / `on_load` 与 DSL 的 `loaded` / `failed` 事件都建在其上；
- 动画图片（GIF、APNG、动画 WebP、AVIF 序列）：asset worker 解出首帧作为 texture，同时交回编码字节；app 级 `Playback` 持有这些字节，在独立线程（无线程时在帧路径内联）逐帧解码，每个窗口的 `Players` 只在帧到期时用 `write_texture` 原地改写已驻留的 texture 并只重绘显示它的节点——不重建 texture、不触发 layout。按文件给的帧时长与循环次数播放（无限或 N 次后停在末帧）；没有任何显示它的节点在视口内可见（含祖先隐藏、透明、裁剪）、所有节点都被 `set_asset_still` 静止（`Image::animate(false)` / DSL `animate: false`）或系统要求减少动态时暂停并保留剩余时长；下一帧的到期时间进入事件循环的 timer deadline，静止时不唤醒；texture 被替换或释放时关闭对应解码器，`animation_frames` 计数写入的帧；
- 视频（ADR 0049）：video node 是一个 asset node，其 resource 不由解码器填充而由 facade 提供（asset table 中的 hosted 条目：不进入 `wanted`、不被 sweep），因而直接得到 fit、tint、圆角 mask、poster / error 替身、`loaded` / `failed` 通知与可见性判断；store 另有每个 video node 一条的 `VideoTable`——来源（文件、URL、字节，或 `VideoSource::Camera(Camera)`）、app 的请求（`Playback { playing, looping, muted, volume, rate }`、待执行的 seek 与镜像请求 `mirror: Option<bool>`）、播放器的报告（`VideoStatus { ready, position, duration, playing, waiting, ended, muted, volume, facing }`）；镜像是 `Content::Asset` 的 `mirrored`，paint 以负宽的 uv 左右翻转采样，未请求时朝向用户的摄像头镜像显示（就绪时 facade 报朝向后 `mirror_video` 只 `PAINT`）。node 即视频的身份：同一来源保持播放位置，换来源得到新 resource。build 端经 `declare_playback` 声明请求，只有声明值变化才覆盖当前请求，handler 的暂停与播完停下不被无关重建打回；handler 经 `EventCx::play_video` / `seek_video` / `set_playback` 提出请求。facade 的 `Videos` 每帧在 loop 线程上：关闭失去节点的播放器（释放 texture），为新条目打开 `viso_media::VideoPlayer`（打开失败即 failed），把请求差异与 seek 交给播放器，poll 事件（就绪时登记固有尺寸，结束时把请求置为暂停），刷新 status（位置按 250 ms 量化以免每帧都产生通知）；首帧或节点在视口内可见时取画面：画面在 GPU 内存（IOSurface、浏览器的 `<video>`）且 backend 能采纳时零拷贝（WebGPU 为一次 GPU 内复制）——首帧 `import_external`，之后 `adopt_external` 把同一 texture 指向新画面；否则复制 texel，同尺寸写入原 texture（backend 拒绝过一次后，该播放器之后的画面直接复制）；两者都只 `PAINT` 显示它的节点，尺寸变化才新建 texture 并重新 layout。播放且可见时窗口逐帧出帧，加载中或在屏幕外播放时以 100 ms 的 timer 轮询；`video_frames` 计数写入的画面。`NoticeKinds::VIDEO` 投递 `Video(status)`（每次变化）、`VideoEnded`（一次）与 `VideoPlaying(bool)`，`Video` widget 的 `on_progress` / `on_ended` 与 DSL 的 `progress` / `ended` / `playing_changed`（写回双向 `playing`）建在其上；控制部件（`viso_ui::video_controls::VideoControls`，widget 的 `controls(true)` 与 DSL 的 `controls` 共用）建在视频叶节点之内（有 poster / error 时建在外层 stack 的最上层），以 `watch_on` 在自己的根上观察视频节点——不占用视频节点自己的 watch——把报告写进各自的 cell，每个部件由 structure hook 单独更新（进度条只 `PAINT`，时间每秒换一次文字，图标只在变化时换）；控制栏淡入淡出是 opacity transition，空闲计时是 `wake_after` 的 token；
- `images` 同时是图片缓存的上限：asset table 按 node 计数每个 resource 的使用者（brush 采样也算），没有 node 再显示的图片保留 texture（vector 保留 scene），再显示时立即绘制；每帧 settle 后，有资源失去最后一个使用者、或字节数 / 份额变化的窗口做一次 sweep——先丢弃没有可保留内容的条目，再按最近显示时间从旧到新释放，直到该窗口的 texture 加未显示的 scene 不超过 `images` 减其他窗口的 texture 字节；trim 释放全部未显示的图片；显示中的图片从不释放；
- 每帧结束检查三条硬限制：heap（安装 `viso::alloc::Counting` 时统计的 live bytes）、gpu（renderer 的 buffer / gradient ramp / mask pages / transient target 加 glyph atlas plane 与图片 texture 字节，仅在设了 gpu 限制时测量）、images（窗口内驻留图片 texture，增量计数）。第一次超出时所有窗口执行与 `LowMemory` 相同的 trim（丢弃可重建的缓存与切走的分支），仍超出则报告一次 `BudgetExceeded`（stderr 与 Inspector 的 `budget` 资源组），回到限制内后重新计；
- worker 数：target 的 `workers`，否则 profile 默认，否则按机器；`0` 时 text shaping 与 asset decode 在主线程按时间片（text 按 span 预算的 8 倍，asset 4 ms）执行，结果与多线程一致；CpuRaster 的光栅线程数取同一个值；
- embedded profile 默认不读系统字体（`cfg(viso_no_system_fonts)`，facade feature `system-fonts` 可打开），只用 packaged font。

### 50A.5 Footprint 指标与 CI 预算

每个 profile 在 CI 中记录：stripped + LTO 二进制体积、静态内存（`.data` + `.bss`）、空闲 RSS、峰值 heap、冷启动到首帧时间、空闲 wakeup 次数、每帧 damage 像素。

初始预算如下。这些数字是**假设**，需要用第一批板卡的 benchmark 校准（AGENTS §7.3）：

| 场景 | embedded-gpu | embedded-cpu |
|---|---|---|
| `01-counter` 二进制 | ≤ 4 MB | ≤ 2.5 MB |
| `01-counter` 空闲 RSS（不含 GPU 驱动） | ≤ 12 MB | ≤ 6 MB |
| 冷启动到首帧 | ≤ 250 ms | ≤ 400 ms |
| 空闲 wakeup | 0 次/s | 0 次/s |

Desktop / Mobile 不设绝对上限，只做趋势回归（§59.1）。同时与 Makepad 的同等示例在同一台机器上对比二进制体积、RSS、首帧时间和空闲 CPU，作为「比 Makepad 更省资源」的可度量依据（AGENTS §38.6）。

#### 50A.5.1 测量：`cargo xtask footprint`

- **参照 app**：`benches/footprint`，`counter` 是一个 label 加一个使计数加一的按钮（`.vs`），`list` 再加一个 10 000 行的 `VirtualList`（每行一个 label）；字体为包内 Noto Sans Latin。Makepad 侧是 `benches/footprint/makepad`：`counter` 即 Makepad `examples/counter`，`list` 按 `examples/portallist_hit` 的 `PortalList` 写法；它们对 Makepad checkout（`--makepad`、`$MAKEPAD_DIR` 或 workspace 旁的 `../makepad`）在独立 crate 中构建，沿用 checkout 的 `Cargo.lock` 与 `.cargo/config.toml`，另量 Makepad 原样 `examples/counter` 的体积。两侧装同一种计数 allocator（Viso 为 `viso::alloc::Counting`），经 stdin 报告 heap。
- **构建**：共用 `CARGO_TARGET_DIR`，命令行定义的 `footprint` Cargo profile（opt-level 3、thin LTO、1 个 codegen unit、strip、unwind），不受两个 workspace 与用户 `[profile.release]` 影响。Viso 各 profile 的 feature 与 `viso build` 相同：desktop 为默认 feature；embedded-cpu 为 `--no-default-features --features viso/cpu,viso/kms,viso/rasterizer`，`VISO_FOOTPRINT=embedded-cpu`、`VISO_FONT_CHARSET=ascii`；两者各加 `viso/headless` 即 headless subject。
- **窗口 subject**（macOS）：在 `/usr/bin/time -l` 下运行，首帧为 spawn 到 app 报告首帧提交（Viso `VISO_DEBUG`，Makepad `MAKEPAD_TOTAL_DRAWCALLS_DEBUG` 的首个 pass）；首帧 2 s 后 `top` 计 3 s 空闲的 CPU 时间与 idle wakeup，`ps` 取 RSS，`footprint` 取 memory footprint 与其中 `IOAccelerator`/`IOSurface`（GPU (OS)）；峰值 RSS 与峰值 footprint 取自 `time -l`。
- **headless subject**：播放 scenario（120 次点击按钮，`list` 另 120 次滚轮，每次 36 px），读 `VISO_HEADLESS_PROFILE` 中每帧 UI 线程时间（含 raster）的 p50/p95，与 `VISO_HEADLESS_DETAIL` 的 `budget` 组（heap、GPU (budget)）。不改变任何东西的输入不运行帧，所以没有可计时的「空闲帧」：空闲成本即窗口 subject 的空闲 CPU 与 wakeup。
- 每个值取一次 warm-up 之后 3 次运行的中位数。`--record` 写 `benches/footprint/baseline.json` 与 `baseline.md`（机器、rustc、日期、Makepad 版本）；`--check` 只量 Viso，任一指标超过 baseline 的容差（体积 3–5%，RSS 20%，footprint 50%，时间 50%，各带绝对下限）即失败，用于 CI 趋势报告。

当前数字（2026-10-18，Apple M4 16 GiB，macOS 27.0.1，rustc 1.99.1，Makepad `70c90b7c0bc0`），桌面窗口：

| | Viso counter | Makepad counter | Viso list | Makepad list |
|---|---:|---:|---:|---:|
| 二进制（stripped） | 5.01 MiB | 12.24 MiB（原样示例 11.67） | 5.01 MiB | 12.29 MiB |
| 首帧提交 | 110 ms | 2179 ms | 98 ms | 2232 ms |
| 峰值 / 空闲 RSS | 81 / 81 MiB | 346 / 282 MiB | 81 / 81 MiB | 348 / 285 MiB |
| 峰值 / 空闲 footprint | 76 / 25 MiB | 319 / 166 MiB | 81 / 59 MiB | 326 / 172 MiB |
| GPU (OS) | 5.4 MiB | 18.9 MiB | 10.3 MiB | 21.4 MiB |
| heap 驻留 / 峰值 | 5.5 / 5.5 MiB | 103 / 138 MiB | 5.6 / 5.6 MiB | 104 / 138 MiB |
| 空闲 CPU / wakeup | 0 ms/s / 0 次/s | 3.3 ms/s / 0.3 次/s | 0 / 0 | 0 / 0.3 |

embedded-cpu（桌面窗口，CpuRaster）：二进制 4.53 MiB，空闲 footprint 25 / 31 MiB（counter / list），heap 7.7 / 12.7 MiB（含 CPU 帧缓冲）。headless（CpuRaster，420×600@1）：点击帧 p50 18–25 µs，滚动帧 p50 149–152 µs、p95 174–178 µs（desktop 与 embedded-cpu 两种 feature 集相同量级）；heap 3.0 / 4.0 MiB（desktop feature 集，counter / list），其中 GPU (budget) 1.07 MiB（1024² 的 glyph coverage atlas）；embedded-cpu feature 集 heap 1.5 / 2.5 MiB，GPU (budget) 326–337 KiB（mask page 按需分页，三个 9×9 图标只占一页 128²）。桌面窗口那几行是 mask 分页之前测的，需在解锁的屏幕上重新 `--record`。全部 subject 见 `benches/footprint/baseline.md`。

尚未测到的：
- 桌面窗口的逐帧 CPU 时间（两侧都没有向真实窗口注入输入的手段；Viso 的逐帧数字来自 headless）；Makepad 的逐帧时间（其 headless 是 `--cfg headless` 加运行时 JIT 编译 shader 的另一条路径，输入只能经 Studio 远程协议注入，与 Viso 不等价）。
- Viso 桌面的首帧提交时 text 尚未 shape（shaping 在 worker 上，文字在下一帧出现），故桌面 Viso 的「首个含文字帧」约晚一帧；Makepad 首帧前的约 2 s 是主线程在其自身代码中的 CPU 时间（stripped 构建，未进一步归因）。
- RSS 含 AppKit / Metal 共享页，跨框架比较以 footprint 为准；嵌入式板卡上的 RSS、首帧与 wakeup 需板卡实测（§50A.5 预算仍是假设）。

### 50A.6 MCU-ready 不变量（1.0 必须守住）

1. `viso-math`、`viso-ende`、`viso-schema`、`viso-behavior` 是 `#![no_std]`（math 不需要 `alloc`，其余需要）。`viso-text` 的 shaping / segmentation 路径和 `viso-ui` 的 arena / reactive / layout 模块不直接使用 `std::thread`、`std::time`、`std::fs`、`std::sync::Mutex`：时间经注入的 `Clock`，并发与唤醒经 runtime trait。CI 用 `thumbv7em-none-eabihf` 做交叉编译 smoke check，逐个 crate 推进：`cargo xtask mcu-smoke` 以 `--no-default-features` 检查 `viso-math`、`viso-ende`、`viso-schema`、`viso-behavior`（另含 `interpreter` feature 的 `viso-behavior`）与 `viso-glyph-a8`。
2. hot data 只使用 `f32` 与整数，不使用 `f64`（Cortex-M 只有单精度 FPU）。
3. 不假设 `usize == u64`（§10.4.4 已有）。
4. arena、hot store、worklist 都支持固定容量模式（§16）。
5. AOT package 支持 XIP：Ende borrowed decode 直接读 memory-mapped flash，不要求先拷贝进 RAM。
   - **布局**：AOT package 与 view package 以 `ProtocolTag` 加 4 字节 magic（`AOT1` / `VPK1`）开头，后接 Ende section table（`viso_ende::SectionWriter` / `Sections`：section 数，再每项 offset、长度、条目数三个 LE `u32`）。每个 section 从 blob 起点算按 8 字节（`SECTION_ALIGN`）对齐，间隙为 0，布局唯一；reader 先校验整张表，越界、重叠、乱序或非零 padding 都是 `Malformed`。能定宽的记录定宽：AOT edge 24 字节，view 的 state 40 字节，handler 与 env 各 12 字节；AOT node 是变长记录（kind、style mask、payload、child 数）。view package 内嵌的 AOT blob 本身也在对齐 offset 上，内部 section 因此同样对齐。
   - **对齐地址**：宏把 behavior module、control-flow region 和 shader 字节 embed 成 `viso_ende::Aligned`（`#[repr(C, align(8))]`）`static`，blob 起点对齐，section 就落在对齐地址上。
   - **读取**：`AotView` / `ViewPackageView` 在 mount 之前一次校验整个 blob（forest 结构、节点下标、记录取值、升序约束、无尾随字节），之后 node / edge / state / handler / env / asset / shader 迭代器在栈上逐条解码，不生成 `Vec`。Release loader（`viso_ui::aot::load_from_bytes`、`viso_view::load_static_view`）只走 borrowed 路径，asset 与 shader 是借用 blob 的 `AssetBytes::Static`。owned 的 `AotPackage` / `ViewPackage` 只给 emitter 和 dev commit 用，其 `Decode` 建在同一个 view 上，没有第二套解码。
   - **Behavior**：`Module::decode_static` 借用 `'static` blob——chunk、component、state、native、capability、theme 名，persist key，catalog 的 message key 与 locale table，以及字符串常量都是借 blob 的 `Text::Static`。运行 Rust 编译代码的 build（`viso::__behavior!` 选取，cfg 与选取 native code 相同：release，或没有 `vm` feature）embed `Module::compiled()`：没有 bytecode、operand 表和 source span，只留常量池、`BodyReads`（memo 的读集）和 build hash，install 时不再整模块重编码求 hash；debug 且有 `vm` 时 embed 完整 bytecode，hot reload 不变。未选取的那份字节在编译期整个丢弃。
   - **仍然进 RAM 的**：每个 string 常量一个 `Rc` 盒子（文本本身借 blob）；control-flow region 模板（`ViewRegions`）与 control 是活的 runtime 结构，mount 时解码为 owned；`ValueSchema` 与 game input schema 的名字是 owned。
6. renderer 前端（primitive / batch / damage）与 backend 分离，以后可以增加 line-buffer rasterizer，适配 SRAM 放不下整帧 framebuffer 的设备。
7. 文本可以完全 baked（§37.8），二进制中可以不含 font rasterizer：`fonts.rasterizer = false` 的 build 由 `packaged_fonts!` 的编译期断言保证每个 packaged glyph 都有 baked pixel；baked page 的 reader（`viso-glyph-a8` 不带 `raster` feature）是 `no_std` + `alloc`，在 `mcu-smoke` 中检查。
8. core 不假设存在线程、文件系统、动态加载或网络。

MCU platform 本身（bare-metal / Embassy / Zephyr 集成）的决策门槛是：Embedded-CPU 交付，并且上面 1–8 条在 CI 中全部通过。届时另开 ADR。

### 50A.7 Embedded 开发闭环

- **桌面仿真**：`embedded-*` profile 可以在桌面 host 上以窗口运行，使用该 profile 完全相同的 feature 集、分辨率、像素格式（RGB565 量化模拟）、面板旋转、内存预算与 worker 数。footprint 与预算违规在桌面上就能复现；CpuRaster 在桌面与板卡上输出 bit-exact。
- **远程板卡**：Dev artifact 部署到板卡，经 TCP 接收 PatchBundle（§42 第 8 条）；部署、listener 与地址规则见 `Viso_Hot_Reload.md` §27A。
- 具体 CLI 命令由 `Viso_CLI.md` 定义。

桌面仿真的实现合同（macOS，facade 的 `emulate` feature，与 `cpu` 一起用于 `embedded-cpu`）：

- 帧走与板卡相同的路径：CpuRaster 画进 profile 的像素格式，按 damage 交给 `RawWindowHandle::Scanout`；macOS 窗口的 scanout 把 damage 区域（RGB565 按位扩展到 8 bit，色带保留）拷进保留帧，作为 content view layer 的 image 显示，`nearest` 放大，每个面板像素是整数个屏幕像素（能放进主屏可用区域 90% 的最大整数，1–8）。没有 `emulate` 时 CPU 画的 macOS 窗口同样经这条路径显示，按屏幕 backing scale、可缩放。
- 面板由 `VISO_KMS_MODE`（缺省 800×480）、`VISO_KMS_FORMAT`、`VISO_KMS_ROTATION` 与 `VISO_SCALE`（缺省 1）给出，运行时环境优先，其次是构建时烘入的值——与 KMS backend 读同一组变量（`viso-platform` 的 `panel` 模块）。旋转是面板安装后的朝向：窗口取图像的形状（四分之一圈时宽高互换），CPU 帧本身永远是图像的形状，与板卡一致。
- 窗口不可缩放，标题注明面板（`480×272 rgb565 0° @1`）；向 app 报告的 scale 与 surface 尺寸是面板的，pointer、scroll 与 IME caret 在窗口 point 与 app 逻辑单位之间按 `面板像素/point ÷ 面板 scale` 换算；窗口移到另一 backing scale 的屏幕时保持每面板像素的屏幕像素数不变。
- 预算、worker 数与字体策略由 build 的 Footprint Profile 烘入（§50A.3），桌面与板卡相同。

---

# Part XVII — Assets 与 Resource System

## 51. 统一 ResourceId

资源使用 typed handle：

```rust
TextureId
FontId
ImageId
ShaderId
PipelineId
AssetId
```

路径字符串只在加载/构建阶段存在。

### 51.1 Asset manifest

不强制新 `.asset` 语言。

默认可以通过 `Viso.toml` + 文件目录 + build scanner：

```text
assets/
├── fonts/
├── images/
├── icons/
└── data/
```

需要高级 manifest 时再增加声明能力。

Build scanner 按 target profile 做资源转换，runtime 不再做格式转换：
- release 构建（`viso build`，非 dev）由 CLI 把 `assets/` 的每张 raster 静图（`images.svg = "raster"` 时连同 SVG；动图一帧容纳不下，按原文件嵌入）经 `viso_image::convert` 转换为 target 采样的格式，写入 `VISO_ASSET_OVERLAY` 指向的 overlay 目录（按 asset 目录规范路径分目录，路径不变；结果按 sha256(converter 版本, plan, 源字节) 缓存，字节不变不重写，未变 asset 不触发重编译）；`asset!` / `view!` / AOT package 经 `viso_dsl::assets::embedded_file` 嵌入转换后的文件，没有时嵌入源文件；
- 格式族由 target 决定：desktop → BC，iOS/Android → ASTC，embedded-gpu → ETC2，embedded-cpu → `CpuRaster` 原样采样的 RGBA8 / RGB565 / RGBA5551 / RGBA4444；web 与 headless 保留源文件。族内按 alpha（无 / 1-bit / soft）与 `images.quality`（`lossless` / `high` / `balanced` / `compact`）选格式（表见 `viso_image::convert`）；
- 有 display 尺寸的 board 把超出 cover fit 所示 texel 的图像预缩小（旋转后的面板尺寸），块压缩格式拉伸到整块；两者都保留 natural size，布局不变；
- 容器 `VTX1`：32 字节 header（格式码、texel 尺寸、natural size、字节数）+ texel，texel 偏移 32、相对 blob 8 字节对齐；runtime 读 header 后借用 texel（`AssetBytes::Static` 直接交给 `create_texture_static`，`CpuRaster` 在 flash/binary 原地采样，不复制），无解码；
- 设备不采样该格式（`Caps::samples` / `SampledFormats`）时，asset worker 用同一软件解码器解为 RGBA8 再上传——转换永远不让图像不可显示；
- 全部转换后 facade 不链接对应解码器：board 上 `VISO_IMAGES_CONVERTED=raster`（`cfg(viso_converted_rasters)`，无 raster 解码；有动图原样嵌入时不设），`svg = "raster"` 时 `svg`（`cfg(viso_converted_svg)`，无 SVG 解析/lower）；
- SVG 默认在构建期解析，运行时 lower 为 primitive 流（`viso-svg`）；
- 字体子集与 baked atlas 见 §37.8；
- 转换结果进入 AOT package，可以 XIP 读取（§50A.6）。

`assets/fonts/` 是 canonical packaged-font 目录。Build scanner 自动识别 TTF/OTF/TTC/OTC，并只提取必要 metadata 生成 compact `FontManifest`；**runtime 不因此 eager parse/load 全部字体**。第一次真正使用某个 face 时才 lazy load 并进入 Font Face SLRU。Native App 没有 packaged/dynamic font 时直接按需使用系统字体与系统 CJK/Emoji fallback；WASM/Canvas 不拥有系统字体，但 packaged fonts 同样可由 `FontManifest` lazy fetch，外部动态字体通过 `FontProvider` 注入。持有 WOFF2 的调用方须先自行解压为 SFNT 再纳入 `assets/fonts/` 或经 `FontProvider` 注入。

### 51.2 Resource lifecycle

支持：

- ref-counted logical handles（不要求 `Rc` per-use）；
- generation/version；
- lazy load；
- async decode；
- background upload；
- eviction policy；
- device loss recreate；
- hot reload。

---

# Part XVIII — 用户项目目录与文件规范

## 52. 默认项目保持简单

```text
my_app/
├── Cargo.toml
├── Viso.toml
├── assets/
└── src/
    ├── main.rs
    ├── app.rs
    ├── theme.vs
    ├── features/
    │   ├── home/
    │   │   ├── mod.rs
    │   │   └── view.vs
    │   └── settings/
    │       ├── mod.rs
    │       └── view.vs
    ├── components/
    └── services/
```

### 52.1 Progressive structure

小 feature：

```text
home/
├── mod.rs
└── view.vs
```

变复杂后：

```text
home/
├── mod.rs
├── view.vs
├── model.rs
├── state.rs
├── api.rs
└── tests.rs
```

不默认生成：

```text
home_page.rs
home_state.rs
home_effects.rs
home_models.rs
home_controller.rs
```

### 52.2 Feature-first 优于 pages/features 双重分类

建议以业务 feature 为第一组织轴：

```text
features/auth
features/feed
features/settings
```

Feature 内自己拥有 page、state、api。

跨 feature 的纯 UI 才进入 `components/`。

### 52.3 禁止垃圾桶目录

尽量避免：

```text
utils/
common/
helpers/
misc/
shared/
```

如果确实共享，按职责命名：

```text
formatting/
validation/
domain/
```

---

## 53. 文件类型与 DSL 引入方式

默认只要求两种文件：

```text
.rs
.vs
```

其中 `.vs` 是唯一 canonical **外部 DSL 文件格式**。Rust 侧使用按语义命名的入口：

```rust
let small = ui! { Button { text: "Save"; } };
let page = view!("features/home/view.vs");

component! {
    TinyBadge {
        input text: String;
        view { Text { text: text; } }
    }
}
```

这不会引入第三种文件类型，也不能形成多套 type/runtime 语义。`.vs` 走完整 formatter/LSP；`ui!` / `component!` 由 proc-macro/frontend 提供 span 映射和编译诊断；`view!` 把外部文件纳入同一个 module graph。

Theme：

```text
theme.vs
```

Route 默认 Rust（目标，`routes!` 尚未实现）：

```rust
routes! {
    Home => "/",
    Settings => "/settings",
}
```

Shader 允许嵌入 `.vs`；只有高级复用场景才允许独立 `.shader`。

原则：每多一种正式文件语言，就意味着 parser、formatter、LSP、diagnostics、AI support、docs 的长期成本。

---

# Part XIX — 工具链与可观测性

## 54. CLI

`viso` 是 Viso 唯一面向普通开发者的命令行 facade。内部可以存在 compiler、packager、inspector、platform toolchain 等多个实现模块，但用户不需要记它们的二进制或 crate 名称。

完整命令、参数、配置、JSON 协议与验收标准由仓库根目录 **`Viso_CLI.md`** 定义；Architecture 只固定不可违反的工具链合同。

### 54.1 Command surface

完整命令分组（PROJECT / MOBILE DEV ENVIRONMENT / DEVELOP / LANGUAGE / TEST·DEBUG / DELIVERY / MAINTENANCE）以 `Viso_CLI.md` 为准。不可违反的规则：

- **host implicit / mobile explicit**：无参数 `viso run/build/package` 表示当前 desktop host；移动与 Web 用逻辑 target（`run ios|android|web-gpu|web-dom|web-hybrid`）。Public CLI 不提供 `run macos|windows|linux|host`、`--target`、`ios --simulator`、`android --emulator`；macOS/Windows/Linux 是 Platform backend resolution，不是用户选择的 development target。
- `run ios/android` 只面向 Simulator/Emulator，`--device` 只选择 Viso virtual-device profile。`viso android|ios use ...` 修改 developer-machine state，不修改 Git tracked `Viso.toml`。开发期不要求 signing/provisioning/physical device；它们只属于 `viso package ios`。

### 54.2 Build / package / export 与 `run` watcher

- `build` → Viso application artifact；`package` → distributable artifact；`export` → 外部生态 source/static artifact（例如 `export html|solid`）。SolidJS 是 exporter，不是 Viso 中间表示或核心依赖。
- 普通开发只需 `viso run`（或移动/Web 逻辑 target）。`run` 负责 Rust/`.vs` incremental build、shader/asset/font watcher、transactional hot reload、Rust stateful Warm Restart、last-good、Simulator/Emulator launch/install、structured diagnostics 与可选 Inspector/Profile attach；不需要独立 `watch` 命令（`Viso_Hot_Reload.md`）。

### 54.3 Machine-readable output 是正式协议

所有可自动化命令必须支持：

```text
--json
```

输出使用 Ende JSON，至少拥有：

```text
diagnostic
progress
artifact
device
test
profile
summary
```

事件结构和 exit code 必须稳定，CI、Studio、IDE、LSP 和 AI agent 不得依赖解析人类文本。

### 54.4 CLI 不复制业务实现

CLI、Studio、IDE/LSP 与 CI 共享同一组 compiler / build / inspect / package 领域服务。CLI command handler 只负责参数解析、project/config resolution、调用 domain service、人类/JSON 输出与 exit code；不得在目标 `tools/cli` 内重新实现 DSL compiler、GPU compiler、packager、Android/iOS simulator protocol 或 Inspector runtime。

### 54.5 CLI 非目标

`viso` 不替代 Cargo/crates.io、Git、通用 shell task runner、Docker/CI 平台、数据库 schema 工具或任意 package manager。

CLI 只聚焦 Viso-specific 的 source → check → build → run → inspect → test → package → export 生命周期。

---

## 55. Inspector

至少提供：

- UI Tree；
- Component Tree；
- Layout Box；
- Dirty reason；
- Style resolution；
- Paint primitive；
- Batch；
- GPU resource；
- focus/input path；
- semantics tree；
- state dependency；
- Viso source mapping。

### 55.1 Frame profile

每帧显示：

```text
Input
State flush
Style
Measure
Layout
Semantics
Paint
Batch
GPU upload
GPU time
Present
```

以及计数：

```text
mounted nodes
dirty nodes
layout nodes
paint nodes
draw calls
quad instances
glyph instances
GPU upload bytes
heap allocations/frame
main-thread task time
```

---

## 56. Debug reasons

任何 dirty 都可在 debug/profile build 查询原因：

```text
Node 381 marked MEASURE dirty
because:
  property Text changed
  from state App.feed.items[3].title
  at src/features/feed/view.vs:82
```


---

# Part XX — 测试策略

## 57. 测试层级

### Unit

- arena/id；
- layout algorithms；
- reactive graph；
- shader type checker；
- parser/HIR；
- text segmentation；
- resource lifetime。

### Integration

- input → state → layout → paint；
- window lifecycle；
- hot reload；
- platform service mocks；
- async cancellation。

### Snapshot / Golden

- layout tree snapshot；
- semantics snapshot；
- render image snapshot；
- shader generated output；
- Viso DSL diagnostics。

### Fuzz

- Viso DSL parser；
- shader parser；
- hot-reload state preservation；
- text Unicode boundaries；
- resource decoder boundaries。

### Headless

必须有 headless backend 支持：

- deterministic input；
- fixed time；
- screenshot；
- node dump；
- semantics dump；
- frame profile counters。

---

# Part XXI — 性能合同

## 58. 性能不是“以后优化”

框架对 hot path 设明确合同。

### 58.1 稳态帧目标

对于已经构建完成、资源已 warm-up 的普通 UI 帧，目标是：

```text
0 unnecessary heap allocation
0 string property lookup
0 global HashMap lookup in per-node traversal
0 Rc/RefCell borrow in node traversal
0 mutex lock on UI main-thread hot path
0 per-node backend virtual dispatch
0 full-tree rebuild for local state change
```

“0”指设计目标和 benchmark 场景，不意味着所有可能的用户自定义代码永远不分配。

### 58.2 允许分配的场景

合理：

- component mount/unmount；
- route push；
- 新 image decode；
- font load；
- dynamic list 扩容；
- Viso DSL compile；
- one-time pipeline creation；
- async result data construction。

重点是不要把这些变成每帧固定成本。

### 58.3 Math 热路径合同

`viso-math` 是 draw/layout/input/transform/game 等路径共享的底层计算层，默认按 hot-path 标准实现：

```text
0 heap allocation
0 string/hash lookup
0 virtual dispatch
0 hidden synchronization
0 pointer-width-dependent semantic layout
```

对 `Vec*`、`Mat*`、`Rect`、Affine/Transform、intersection 等批量运算，优先保持连续值语义和可内联实现。SIMD specialization 必须由 benchmark 驱动，并保持 scalar fallback 与 public ABI 一致。

---

## 59. 基准测试矩阵

至少：

```text
layout_1k_static
layout_10k_dirty_1pct
layout_10k_dirty_100pct
list_100k_scroll
list_variable_height_scroll
text_10k_labels_static
text_dynamic_1k
font_native_system_default_cold_start
font_native_system_default_warm_100k
font_resolve_app_hit_100k
font_resolve_app_miss_system_hit_100k
font_resolve_negative_cache_100k
font_face_slru_hotset_10_fonts
font_face_slru_scan_500_fonts_then_hotset
font_face_slru_byte_budget_large_cjk
font_face_memory_pressure_shrink
font_fallback_mixed_script_10k
font_progressive_request_dedup_10k
font_progressive_subset_arrival_1k_paragraphs
font_wasm_zero_font_startup
font_wasm_external_font_first_text
font_cache_revision_targeted_invalidation
glyph_atlas_page_eviction_scan_100k
glyph_atlas_hotset_after_cold_scan
animation_transform_5k
animation_layout_1k
hit_test_100k
state_update_1_binding
state_update_1k_bindings
reactive_static_10k
reactive_mixed_dynamic_10pct
reactive_dynamic_10k
reactive_hot_reload_rebind
paint_10k_quads
batch_10k_quads
hot_reload_small_component
hot_reload_large_module
startup_minimal
startup_complex_app
memory_10k_nodes

damage_single_label_update
damage_scroll_list_partial_present
cpu_raster_800x480_rgb565_full
cpu_raster_800x480_damage_5pct
cpu_raster_glyphs_10k
gles3_batch_10k_quads
footprint_binary_size_{profile}
footprint_idle_rss_{profile}
footprint_first_frame_cold_{profile}
footprint_idle_wakeups_{profile}
footprint_vs_makepad_{example}
release_handler_native_vs_vm

math_vec2_ops_10m
math_mat4_mul_1m
math_affine2_transform_points_10m
math_rect_hit_test_10m
math_aabb_intersection_1m
math_transform_chain_100k

identity_symbol_link_100k
identity_dense_property_lookup_1m
identity_node_generation_check_1m
identity_keyed_reconcile_100k

ende_bin_small_message
ende_bin_100k_messages
ende_bin_encode_reused_buffer
ende_bin_decode_borrowed
ende_json_diagnostic_1k
ende_hot_reload_patch_roundtrip
ende_profiler_trace_encode
```

### 59.1 CI regression policy

每个 benchmark 记录：

- wall time；
- CPU cycles（平台支持时）；
- allocations；
- bytes allocated；
- peak RSS；
- draw calls；
- GPU upload bytes；
- GPU frame time。

建议：

- >3%：标记趋势；
- >5%：CI warning / 需要说明；
- >10%：默认阻止合并，除非明确批准。

具体阈值按 benchmark 稳定性调整。

### 59.2 Release 验证

所有性能结论必须使用 release/profile 配置。

Debug build 只用于 correctness，不用于性能比较。

---

## 60. Memory budget

不要简单规定“每 Node 必须 X 字节”，因为节点类型和平台不同。

但必须统计：

```text
hot bytes/node
warm bytes/node
cold debug bytes/node
state bytes/component
paint bytes/primitive
gpu bytes/instance
```

CI 记录典型 tree 的 memory-per-node 趋势。

per-node 不设绝对上限，但 Embedded profile 有整机级预算（§50A.5），各 cache 预算由 `MemoryBudget` 统一分配（§50A.4）。NodeLinks 改为 index 链接后（§14.1），`hot bytes/node` 的目标是不超过一条 64-byte cache line。

---

# Part XXII — Unsafe 设计

## 61. Unsafe 边界集中化

允许 `unsafe` 的主要位置：

- GPU FFI；
- OS FFI；
- SIMD/packed buffer；
- arena unchecked fast path（经验证包装）；
- mapped buffer；
- generated shader ABI bridge。

普通 `widgets` / app API 不应该需要 unsafe。

### 61.1 Safety comment

每个 unsafe block 必须写：

```text
SAFETY:
- invariant 1
- invariant 2
- caller guarantee
- why aliasing/lifetime is valid
```

### 61.2 Fast/checked 双路径

内部可提供：

```rust
get_node_checked()
get_node_unchecked()
```

但 `unchecked` 只在清晰 phase invariant 下使用，Debug build 尽可能 assert。

---

# Part XXIII — Error / Diagnostics

## 62. 错误分级

```text
UserError      可修复配置/输入错误
CompileError   Viso DSL/Shader/Rust schema 错误
RuntimeError   task/service/runtime failure
DeviceError    GPU/device/surface
InvariantError 框架内部 bug
```

InvariantError Debug panic，Release 尽量输出上下文并安全终止/降级，不 silent corruption。

### 62.1 Diagnostics 统一结构

```rust
Diagnostic {
    severity,
    code,
    message,
    primary_span,
    related_spans,
    notes,
    fixits,
}
```

Viso DSL、Shader、Layout strict lint、Migration 共用 diagnostics 基础设施。

---

# Part XXIV — Makepad 参考实现与设计经验

> §63 首段（不建立兼容层、不复制耦合边界）是规范；§63.1–§63.7 为 informative 背景。

## 63. 参考边界

Makepad 只作为 Viso 设计过程中的外部参考资料。Viso 不建立 Makepad API/ABI 兼容层，不提供项目转换命令，不在 runtime 中识别 Makepad 类型，也不把 Makepad 的模块注册、Widget 生命周期或脚本执行模型作为 Viso public contract。

参考原则：

> **参考经过验证的算法、性能特征、工具链经验和用户体验，不复制耦合边界。**

所有被采用的思想都必须先经过 Viso 自己的类型、数据布局、crate DAG、性能合同和测试体系重新定义。

### 63.1 Identity / Symbol

Makepad `LiveId` 展示了“把源码名字尽早变成廉价数字身份、避免热路径字符串比较”的价值。Viso 参考这一性能目标，但采用分层 Identity：

```text
NameId
  ↓
SymbolId (128-bit stable identity)
  ↓ link
PropertyId / EventId / ComponentTypeId / ShaderId
  ↓ instantiate
NodeId(index, generation)
```

Viso 不使用一个万能 ID 贯穿所有 subsystem。

### 63.2 Ende

Makepad 的轻量序列化实现证明了低依赖、derive 生成、直接 Encode/Decode 对工具协议和高频消息很有价值。Viso 将这类能力定义为 `viso-ende`：

- Viso-owned Binary wire contract；
- Ende JSON 作为 diagnostics/tool interchange；
- reused-buffer zero-allocation encode 目标；
- bounded decoder；
- typed schema/version contract；
- 不支持 RON；
- Serde 只作为生态 integration。

### 63.3 Math / Geometry

Makepad 中已经验证过的向量、矩阵、矩形、transform、geometry 热路径实现可作为性能与数值行为参考。Viso 使用独立 `viso-math` 类型、ABI 和模块边界；Math public ABI 不暴露 backend SIMD 类型，也不直接等于 GPU uniform/instance layout。

### 63.4 Layout

顺序布局中的单遍 cursor/turtle 类算法可以作为内部实现候选，尤其适用于 Row/Column/flow 场景。Viso public layout contract 仍以 `Constraints / Measure / Layout`、typed units 与独立 invalidation plane 为准，算法是否采用单遍 cursor 由 benchmark 决定。

### 63.5 Renderer / Shader / Atlas

可以参考已经验证过的：

- shader lowering 思路；
- geometry/tessellation；
- texture/glyph atlas；
- batching；
- instance upload；
- clip/scroll/hit-test 优化；
- Metal/D3D/Vulkan/Web 平台经验。

这些参考不得反向决定 Viso 的 public renderer types、GPU ABI 或 frame phases。

### 63.6 Live Editing / Game Authoring

Makepad 的实时编辑、紧凑 UI authoring、固定步长 game update、host-injected game API 等经验可用于评估 Viso 的开发体验。Viso 使用自己的 `.vs`、Typed HIR、Reactive/System IR、FixedUpdate scheduler、Hot Reload transaction 和 Last-good runtime 合同。

### 63.7 Studio / CLI

Makepad 在跨平台构建、Studio、远程 UI 操作、截图、profile 等方向上的经验可作为 Viso 工具链的产品参考。Viso 的实现统一收敛到 `viso` CLI、Studio 与共享 tooling services；命令语法、协议和项目模型全部由 Viso 自己定义。

### 63.8 Embedded

Makepad 的 Linux direct backend（`platform/src/os/linux/direct`）验证了「DRM/KMS + GBM + EGL 全屏、不依赖 compositor」这条路径可行。它的边界是：只支持 GLES2，每帧从 evdev 轮询输入，没有 damage 局部上屏，没有 CPU 光栅路径，也没有资源预算或 profile 裁剪。

Viso 在这条路径上的差异：GLES3 / Vulkan / CpuRaster 三种后端、epoll 事件驱动且空闲零唤醒、damage-driven present、Footprint Profile 与 memory budget 合同、MCU-ready core（§50A）。

---

# Part XXV — Viso 1.0 实施路线图

> **Informative。** 本 Part 是路线图，不是规范性合同；与 Part 0–XXIV 或已接受 ADR 冲突时以后者为准。进度以 `docs/adr/` 为准：Phase 0 已由 ADR 0001 关闭；Phase 1–8 的已落地切片分别见 ADR 0004/0019/0020/0032（runtime/platform）、0002/0029（GPU/renderer）、0003（tree）、0007–0009/0022/0030（layout/input/semantics）、0005/0021（reactive）、0010/0011/0013–0018（DSL/hot reload/AOT/shader/tooling）、0023/0024（widgets）、0031/0032（services/async）。Phase 9 的 CLI 已起步（`tools/project` 与 `tools/cli` 的 `viso check`），Studio/Inspector 尚未开始。

## 64. Phase 0 — 固定 Architecture Contract

### 目标

先锁定 Viso 自身的边界、性能与工具协议，再扩展功能面。

### 工作

1. 固定 `viso::run::<App>()` facade；
2. 固定 crate dependency DAG；
3. 固定 hot-path contract；
4. 固定 Identity & Symbol Architecture；
5. 固定 Ende Binary/JSON contract；
6. 固定 `viso-math` 基础 ABI；
7. 固定 NodeId/NodeArena identity model；
8. 固定 frame phases；
9. 固定 `.vs` 为唯一 canonical DSL 扩展名；
10. 固定 Dev-only Hot Reload / Rust Warm Restart contract，并建立 `Viso_Hot_Reload.md`；
11. 固定 Text/Font Runtime contract，并建立 `Viso_Text_Font_Runtime.md`；
12. 固定 renderer primitive contract；
13. 固定 GPU instance ABI；
14. 建立 benchmark 与 characterization suite；
15. 建立 crate 依赖 allowlist 检查（当前 `cargo xtask check-deps`，§10.2）；
16. 建立 dependency/unsafe/perf CI gates。

### 退出标准

- 最小 Viso workspace 可独立编译；
- CI 自动验证 crate/module 依赖方向；
- core benchmark baseline 已记录；
- `Viso.toml`、CLI target model 与 artifact 目录约定已固定。

---

## 65. Phase 1 — Foundation / Runtime / Platform

### 目标

得到一个完整、独立的 Viso application loop。

### 工作

1. 实现 `viso-runtime` frame scheduler；
2. 定义 `Application` 与 `AppCx`；
3. 定义 window/surface lifecycle；
4. 定义平台事件标准化；
5. 建立 macOS/Windows/Linux/iOS/Android/Web 的最小 backend；
6. 实现 timer/wakeup/task completion 主线程桥；
7. 建立 headless backend；
8. 保持 runtime 不依赖 UI/DSL/Studio。

### 退出标准

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

可以打开窗口、处理 resize、接收输入、驱动空白帧。

---

## 66. Phase 2 — GPU RHI 与 Renderer

### 目标

建立 Viso 自己的 GPU 与渲染协议。

### 工作

1. typed GPU resource IDs；
2. backend-static / compile-time selected GPU backend；
3. buffer/texture/pipeline/sampler API；
4. upload ring / persistent buffers；
5. render pass / render graph；
6. `Quad`, `GlyphRun`, `Image`, `Path`, `Mesh`, `Clip`, `Layer` primitives；
7. automatic batching；
8. explicit `#[derive(GpuPod)]` instance descriptor；
9. render counters/profiler；
10. screenshot golden tests。

### 实现原则

Renderer 可以采用经过 benchmark 验证的 atlas、batch、tessellation、shader lowering 算法，但 public/runtime 类型先由 Viso contract 定义。普通 Widget 不接触 batch correctness。

### 退出标准

- 纯 Viso test scene 可以绘制；
- 稳态 renderer hot path 满足 allocation/dispatch 约束；
- GPU instance layout 有编译期验证；
- Metal/D3D/Vulkan/WebGPU 至少有一条 Tier-1 路径通过核心测试。

---

## 67. Phase 3 — NodeArena 与 UI Tree

### 目标

建立最终 UI identity、tree storage 与 typed handle 模型。

### 基础结构

```text
NodeId(index, generation)
        │
        ▼
     NodeArena
        │
        ├── Tree links
        ├── Layout data
        ├── Transform data
        ├── Interaction data
        ├── Paint handles
        └── Semantics handles
```

### 工作

1. generation-safe `NodeId`；
2. compact parent/child/sibling links；
3. node create/remove/reparent；
4. stable keyed identity；
5. dirty mask；
6. subtree versioning；
7. debug metadata side table；
8. Inspector-readable snapshot；
9. no `Rc<RefCell<dyn Widget>>` in native UI path。

### 退出标准

- 100k node create/traverse/remove benchmark；
- stale IDs 安全失败；
- 删除 subtree 不留下 dangling state；
- hot traversal 基于连续/紧凑数据结构。

---

## 68. Phase 4 — Layout / Input / Style / Semantics

### 顺序

1. bounds/transform；
2. hit test；
3. pointer routing；
4. keyboard/focus/IME；
5. box constraints；
6. Row/Column/Stack；
7. Scroll；
8. VirtualList；
9. style token；
10. semantics；
11. Grid/Adaptive。

### Layout 取舍

公开 API 使用 Row/Column/Stack/Grid/Absolute 容器、`Dp` 长度族与 sizing mode（§22）。内部算法可使用单遍 cursor、缓存 measure、局部 relayout 等优化，但这些实现细节不泄漏成用户正确性要求。

### 退出标准

- Button/Label/Scroll/VirtualList 可完全运行；
- input dispatch 以 target/capture/bubble/focus 为核心；
- steady-state scrolling 达到 allocation budget；
- semantics 从 Node 模型天然生成。

---

## 69. Phase 5 — Reactive State 与增量失效

### 目标

实现 retained UI + targeted invalidation，而不是 Virtual DOM。

### 工作

1. `StateId` / typed state slot；
2. transaction batching；
3. `DirtyClass`；
4. binding metadata；
5. static dependency fast path；
6. dynamic fallback 仅作为显式 escape hatch；
7. computed cache；
8. effect lifecycle；
9. state inspector；
10. frame coalescing。

### 典型失效

```text
text change
→ MEASURE | LAYOUT | PAINT | SEMANTICS

color change
→ PAINT

transform change
→ TRANSFORM

aria label change
→ SEMANTICS
```

### 退出标准

- Counter 修改 state 不需要 rebuild whole component；
- 单属性变化只触发必要 phase；
- transaction 内多次 set 只 flush 一次；
- 依赖图和 dirty reason 可 profile；
- static/mixed/dynamic reactive benchmark 均有基线。

---

## 70. Phase 6 — Viso DSL / Compiler / Hot Reload

### 目标

建立 typed、incremental、AOT-friendly 的 Viso DSL。

### 编译管线

```text
.vs / ui! / component! / view!
   ↓
Tokenizer
   ↓
Lossless CST
   ↓
AST
   ↓
Name Resolution
   ↓
Typed HIR
   ↓
UI IR / Reactive IR / Shader IR / System IR
   ↓
Dev Hot Reload or Release AOT Artifact
```

### 语言规则

- `.vs` 是唯一 canonical 外部 DSL 文件扩展名；
- Rust 入口使用 `ui! { ... }`、`component! { ... }` 与 `view!("...vs")`；
- 三者进入同一 schema/HIR/IR/runtime；
- component/schema 可静态检查；
- module/import/export 不依赖注册调用顺序；
- view 默认无副作用；
- effect/event 承载副作用；
- state/computed 进入增量依赖图；
- shader 走严格布局验证；
- dynamic scripting 是 escape hatch，不是普通 UI 默认执行模型。

### 退出标准

- formatter/LSP/goto/rename/reference 可用；
- release 不需要启动时 parse `.vs`；
- hot reload 是 compile → validate → atomic patch；
- 失败保留 last-good UI；
- state/focus/scroll hot-reload state preservation 有明确规则。

---

## 71. Phase 7 — 官方 Widgets

### 原则

每个 widget 直接基于 Viso Node/Layout/Input/Paint/Semantics 模型实现。

优先级：

```text
Tier 1: View/Container, Label, Image, Icon
Tier 2: Button, CheckBox, Toggle, Radio, Slider, TextInput
Tier 3: Scroll, VirtualList, Grid, Splitter
Tier 4: Window, NavigationStack, Tabs, Modal, Popup, Sheet, Toast
Tier 5: Dock, FileTree, code-editor primitives
Tier 6: Markdown, PDF, Browser, Charts, Map, Video
```

### 每个 Widget 的验证包

必须包含：

- behavior checklist；
- screenshot golden；
- input tape；
- accessibility snapshot；
- microbenchmark；
- allocation profile；
- platform-specific edge cases。

### 退出标准

- UI zoo 关键控件完整；
- widgets crate 只依赖允许的下层 crate；
- 关键 widgets 达成性能目标。

---

## 72. Phase 8 — Platform Services / Async / App Framework

### 工作

- clipboard；
- file picker；
- share；
- permissions；
- camera/location；
- notifications；
- secure storage；
- HTTP/WebSocket adapter；
- async task API；
- routing/navigation；
- safe area；
- keyboard avoidance；
- adaptive layout；
- app lifecycle。

服务层允许 cold-path trait object；UI/runtime hot path 不因此引入动态开销。

### 退出标准

真实移动/桌面 App 不需要直接调用 OS glue 才能完成常见能力。

---

## 73. Phase 9 — CLI / Studio / Inspector / Web Delivery

### 原则

CLI、Studio、IDE、CI 都是共享 tooling services 的客户，不允许各自复制 build/compiler/package pipeline。

### 工作

- `viso new` / `viso doctor`；
- `viso android list|use|doctor|emulator|adb`；
- `viso ios list|use|doctor|simulator`；
- host-implicit `viso run` 与 simulator/emulator `viso run ios/android --device ...`；
- `viso build` / `viso serve`；
- `viso package`；
- `viso export html|solid`；
- `viso fmt` / `viso check` / `viso schema` / `viso explain` / `viso dump`；
- `viso test` / `viso snapshot`；
- `viso inspect` / `viso profile` / `viso studio`；
- machine-readable Ende JSON output；
- Web GPU / DOM / Hybrid target；
- headless automation；
- source map / semantic query / profile trace。

完整 CLI contract 见仓库根目录 `Viso_CLI.md`。

### 退出标准

- 普通项目从创建到发布不要求用户直接调用内部 tool crate；
- Studio 与 CLI 共用 build/compiler/inspection service；
- `--json` 可供 CI、IDE 与 AI agent 稳定消费；
- Web target 与 HTML/Solid export 有明确 capability diagnostics；
- 性能问题可通过工具直接定位。

---

## 73A. Phase 10 — Architecture 修正与 Embedded（ADR 0041–0044）

### 工作

1. DAG 修正：删除 `gpu -> runtime`、`text -> gpu`；新增 `viso-schema`，把 `dsl -> ui` 改为 `dsl -> schema`；`check-deps` 增加传递闭包检查；
2. Release native lowering：`lower_module` 把每个 chunk（含 task 状态机）生成 Rust；`view!` 与 view package 都走 native 路径，解释器成为 `vm` feature；镜像 sync 按 epoch 门控；补 differential test；
3. Frame phases 改为 17 阶段并同步 `FramePhase::ORDER`；dirty worklist；
4. Transform root + 位移表，draw 级 `shift` uniform；TRANSFORM 不再连带 PAINT / HIT_TEST；
5. NodeLinks 改为 index 链接；binding 表改为模板级；
6. Damage-driven present（GPU 与 CPU）；
7. `CpuRaster` 生产化（tile、RGB565、SIMD、damage tile）；
8. `Gles3Backend` 与 GLSL ES 3.00 codegen；
9. `backend/linux/kms`：KMS atomic、GBM/EGL、dumb buffer、evdev、epoll；
10. Footprint Profile、`MemoryBudget`、`check-profiles`、footprint benchmark、与 Makepad 的对比基线；
11. Embedded 字体：subset、baked atlas；资源构建期转换；
12. MCU-ready 不变量的 `no_std` CI。

### 退出标准

- `01-counter` 在一块 Embedded-GPU 板卡与一块 Embedded-CPU 板卡上运行，满足 §50A.5 的预算（或给出校准后的新预算）；
- 桌面仿真与板卡输出 bit-exact（CpuRaster）；
- LSP / `viso check` 的依赖闭包不含 runtime / platform / GPU backend；
- Release 二进制中不含 behavior VM；`release_handler_native_vs_vm` 有基线数据。

---

# Part XXVI — Repository 详细编排

## 74. Framework repo 最终建议

当前仓库结构与 crate 清单见 §8/§9，依赖方向见 §10；本节不再重复一份目录树。目标补充（informative）：

- `integrations/`：tokio、tracing、serde、robius 等 adapter crate；
- `extras/`：code-editor、markdown、pdf、browser、charts、map、xr；
- `tools/`：cli、inspector、studio、packager；
- `examples/`：按 `00-minimal`、`01-counter`、`02-layout` … `10-custom-shader`、`99-full-app` 渐进编号；
- `vendor/`：第三方 fork（§76）。

crate 内部目录遵循 §75（简单概念一个文件，复杂子系统一个目录）；不在本文冻结每个 crate 的文件清单。

---

## 75. `lib.rs` 规则

`lib.rs` 主要负责：

- crate docs；
- `mod`；
- `pub use`；
- 极少量真正 crate-level setup。

不要把几千行 implementation 堆进 `lib.rs`。

但也不要机械地“一 struct 一个文件”。

规则：

> 简单概念一个文件；复杂子系统一个目录。

---

## 76. Vendor 规则

第三方 fork / vendored code 统一进入：

```text
vendor/
```

框架自己实现的业务/算法 crate 不放 `vendor`。

不得把：

```text
unicode
windows bindings
wayland bindings
rustybuzz fork
map/ai/game 等高阶扩展
```

全部重新塞进一个模糊 `libs/`。当前 `libs/` 只放框架自有、可独立复用的小型算法 crate（如 `viso-woff2`），不放第三方 fork。

---

# Part XXVII — Public API 细化建议

## 77. `run::<App>()`

当前签名（`crates/viso/src/lib.rs`）：

```rust
pub fn run<A: Application>();

pub trait Application: Sized + 'static {
    fn new(cx: &mut AppCx) -> Self;
    fn window_config(&self) -> WindowConfig { WindowConfig::default() }
    fn build(&mut self, cx: &mut BuildCx<'_>) {}
    fn menu(&self) -> Option<viso_platform::Menu> { None }
    fn on_menu_command(&mut self, command: viso_platform::MenuCommandId) {}
    fn on_open_url(&mut self, cx: &mut viso_ui::EventCx<'_>, url: &str) {}
    fn on_tray(&mut self, event: viso::shell::TrayEvent) {}
}
```

`run` 拥有 platform event pump 与 frame scheduler，无原生 backend 时回退 headless，直到最后一个窗口关闭才返回（ADR 0006/0020）。`resumed`/`suspended`/`low_memory` 等 lifecycle hook 为目标（§50）。事件不由 `Application` 手工接收；UI runtime 自己 route。例外是打开应用的链接（`[package.links]`）：platform 以 `RawEvent::OpenUrl { url }` 交出（在 `AppLaunched` 之后，冷启动与运行中一致；macOS `application:openURLs:`，iOS scene `URLContexts`/`userActivities` 与 `openURLContexts`/`continueUserActivity`，Android 启动 intent 与 `onNewIntent`，Windows/Linux 参数加单实例转交，web 的 fragment 与 `hashchange`），runtime 经 `FrameDriver::on_open_url` 交给门面，门面排队到启动窗口建好，再以该窗口根上的 `EventCx`（`KeyRouter::run_at`，请求按处理器的方式生效）调用 `Application::on_open_url`。

---

## 78. Typed Handle

Viso 1.0 明确：`Handle<T>` 是 **typed identity/capability**，不是组件对象引用。

禁止：

```rust
let state: &T = handle.borrow(cx);         // no long-lived direct borrow
let state: &mut T = handle.borrow_mut(cx); // no mutation bypassing invalidation
```

允许的 public direction：

- `handle.is_alive(cx)` / `handle.node_id()`；
- typed action/event dispatch；
- schema 生成的 property getter/setter；
- context-scoped read query，返回短生命周期 snapshot/ref；
- 只有 `UpdateCx`/生成的 mutation API 可以改变受 reactive 管理的 state。

内部可以存在 `ComponentRef<'a, T>` / `ComponentMut<'a, T>` 之类受生命周期约束的临时 guard，但不得把它们存储到下一帧，也不得允许绕过 dirty/version accounting。

普通 Widget API 可以提供：

```rust
pub struct Handle<T> {
    node: NodeId,
    _marker: PhantomData<fn() -> T>,
}
```

它：

- Copy；
- 不拥有 Widget heap object；
- generation validation；
- 可映射 component storage。

用户：

```rust
let button: Handle<Button> = ...;
button.set_text(cx, "Save");
```

内部转为 state/property update，而非 borrow 一个 `RefCell<Button>`。

---

## 79. Query API

静态组件尽量使用编译期 handle。

动态工具查询：

```rust
cx.query(root)
    .role(Role::Button)
    .name("submit")
    .first();
```

Inspector query 可以使用字符串，但不属于 steady-state UI hot path。

---

## 80. Custom painter

高级 API：

```rust
trait Painter {
    type Instance: GpuPod;

    fn paint(
        &mut self,
        cx: &mut PaintCx<'_>,
        node: NodeId,
        out: &mut PaintWriter<'_>,
    );
}
```

`PaintWriter` 输出 primitive/instance，不直接 expose raw backend command buffer。

更底层再提供 `CustomRenderNode`。

分两级 escape hatch，避免普通自定义控件直接绑死 Metal/Vulkan。

---

# Part XXVIII — 关键架构决策记录（ADR 摘要）

## `docs/adr/` 决策索引（规范来源）

`docs/adr/NNNN-*.md` 是已接受架构决策的唯一记录；本文与 ADR 冲突时以 ADR 为准。

| ADR | 标题 | 状态 |
|---|---|---|
| 0001 | Phase 0 Architecture Contract | Accepted |
| 0002 | Layer offscreen compositing（由 opacity 触发） | Accepted |
| 0003 | Retained widget tree：SoA node store、imperative build、direct flex | Accepted |
| 0004 | First-frame trigger：scheduler 拥有启动 redraw | Accepted |
| 0005 | Reactive state：SoA StateStore、hybrid bindings、memo-gated computed、scoped effects | Accepted |
| 0006 | Application scene seam、reactive-reach BuildCx、pointer-phase access | Accepted |
| 0007 | Scroll：viewport transform、clip、wheel routing | Accepted |
| 0008 | VirtualList：scroll viewport、per-frame reconcile、recycle pool | Accepted |
| 0009 | Grid layout：track sizing、placement、two-axis solver | Accepted |
| 0010 | `.vs` lexer、lossless green-tree CST、coarse parser | Accepted |
| 0011 | `.vs` typed AST、name resolution、module graph、unified diagnostics | Accepted |
| 0012 | `viso-math` 与 `viso-ende`：两个自有 leaf foundation | Accepted |
| 0013 | `.vs` Typed HIR 与 type/effect/capability checking | Accepted |
| 0014 | UI IR + Binding IR 与 `ui!` proc-macro | Accepted |
| 0015 | Transactional hot reload（compile → diff → migrate → commit） | Accepted |
| 0016 | Release AOT package（compact IR asset，load path 不含 compiler） | Accepted |
| 0017 | Shader IR（typed single source、ABI cross-check、last-good hot reload） | Accepted |
| 0018 | Formatter 与 LSP（`viso-lsp`） | Accepted |
| 0019 | One-shot timers 与 deadline-driven frame scheduling（`WaitUntil`） | Accepted |
| 0020 | Multi-window：per-window retained state、deferred open/close、platform close seam | Accepted |
| 0021 | Reactive semantic-state projection | Accepted |
| 0022 | Hover：per-node enter/leave synthesis | Accepted |
| 0023 | Dock live re-layout 与 tree-as-warm-state | Accepted |
| 0024 | File tree flattened-visible-row model | Accepted |
| 0025 | Text subsystem ownership 与 cache boundaries | Accepted |
| 0026 | System-font provider 与 color-glyph rasterizer seam（trait 在 viso-text，CoreText 在 facade） | Accepted |
| 0027 | Width-aware text reflow | Accepted |
| 0028 | SFNT-only font input（WOFF2 由调用方解压） | Accepted |
| 0029 | GPU backend static selection per target | Accepted |
| 0030 | Accessibility OS bridge（AccessKit） | Accepted |
| 0031 | Service protocols：一个 registry、one-shot reply、每 OS 一个实现 | Accepted |
| 0032 | UI task protocol：loop-thread task、cross-thread wake、node-scoped cancellation | Accepted |
| 0033 | Relative length units：`em`、Percent basis、`MixedLength` | Accepted |
| 0034 | DSL keyword tiers 与 label positions | Proposed |
| 0035 | Behavior bytecode VM in a leaf crate | Proposed |
| 0036 | Native schema bridge in the behavior crate | Proposed |
| 0037 | Mounting a view's handlers on the behavior VM | Proposed（Release 路径由 ADR 0041 修订） |
| 0038 | Mounting a view's control-flow regions | Proposed |
| 0039 | Inlining user-component instances into the mounting view | Proposed |
| 0040 | Native control responses mounted from the handler table | Proposed |
| 0041 | Release native lowering：handler / binding / computed 生成 Rust，VM 仅 Dev 与 dynamic | Accepted |
| 0042 | Crate DAG 修正：`viso-schema`、编译器不依赖运行时、删除 gpu→runtime 与 text→gpu | Proposed |
| 0043 | Frame phases 与 transform plane：17 阶段、dirty worklist、transform root | Proposed |
| 0044 | Device Class、Footprint Profile 与 Embedded（GLES3、CpuRaster、KMS、MCU-ready） | Proposed |

## 文内架构取舍摘要（AD-01 … AD-24）

以下条目是早于 `docs/adr/` 的设计取舍摘要，编号为 **AD-xx**（原“ADR-0xx”，编号一一对应，例如原 ADR-022 = AD-22）。其他文档中的旧写法（如 `docs/adr/0028` 中的“ADR-022”）均指这里的 AD 条目，而非 `docs/adr/00xx`。有对应 ADR 文件的条目在标题下注明；无对应文件的条目仍是规范性取舍，但若与后续 ADR 冲突，以 ADR 为准。

### AD-01：单一 facade crate

**决定**：普通用户只依赖 `viso`。  
**原因**：隐藏内部拆分，降低 onboarding 和 API churn。  
**代价**：facade 需要谨慎维护 re-export 与 features。

### AD-02：Retained Tree，不采用 Virtual DOM

*对应 ADR 0003（retained tree / imperative build）、ADR 0005（reactive dirty）。*

**决定**：状态变化直接 invalidation retained node/property。  
**原因**：更符合 Viso 的性能定位，避免通用 rebuild/diff 成本。  
**代价**：reactive dependency、structure mutation 和 hot reload 实现更复杂。

### AD-03：NodeArena + generational ID

*对应 ADR 0003（SoA node store）。*

**决定**：Node identity 使用 arena ID。  
**原因**：局部性、低分配、稳定 handle、避免 RefCell。  
**代价**：需要明确 storage/lifetime/handle validity。

### AD-04：UI Tree 不做全 ECS

**决定**：保留 parent/child UI tree，热数据采用 DOD。  
**原因**：UI 的 ancestry 语义强，纯 ECS 会让 layout/focus/semantics 复杂化。  
**代价**：需要设计好 tree + side arrays。

### AD-05：Viso DSL 不作为 UI runtime 基础依赖

*对应 ADR 0014/0015/0016 的依赖方向约束。*

**决定**：编译器单向依赖运行时合同，不是 `ui -> dsl`；目标为 `dsl -> schema <- ui`（ADR 0042）。  
**原因**：纯 Rust、AOT、测试、可维护性。  
**代价**：需要 schema/IR bridge。

### AD-06：Release AOT

*对应 ADR 0016。*

**决定**：默认 release 不 parse `.vs`。  
**原因**：启动、内存、性能、错误提前发现。  
**代价**：build pipeline 更复杂。

### AD-07：GPU backend 静态选择

*对应 ADR 0029。*

**决定**：源代码统一 RHI，target release 尽量单 backend specialization。  
**原因**：避免热路径 dyn dispatch 和最低公分母。  
**代价**：backend 维护成本高于完全依赖通用抽象。

### AD-08：自动 batching

**决定**：batch 是 renderer 责任。  
**原因**：用户不应该靠 `new_batch` 维持正确性。  
**代价**：renderer 需要更强 z-order/clip/layer 模型。

### AD-09：官方 async protocol，不强绑 Tokio

*对应 ADR 0032（loop-thread task、`LoopWaker`、adapter 不随该决定发布）。*

**决定**：Viso main loop 拥有 scheduling；Tokio/Smol 为 adapter。  
**原因**：控制 frame/vsync/lifecycle，减少 feature matrix。  
**代价**：需要维护小型 task bridge/runtime。

### AD-10：Progressive project structure

**决定**：feature 从 `mod.rs + view.vs` 起步，复杂再拆。  
**原因**：同时适配小/大项目。  
**代价**：团队需要约定何时拆文件。

---

### AD-11：Node hot storage 使用 hybrid indexed SoA

*对应 ADR 0003。*

**决定**：compact `NodeMeta` AoS + 同索引 hot SoA stores + sparse cold tables。

**理由**：兼顾树遍历 locality、直接 index、可选数据空间效率和实现可控性；避免 pure object graph 与 archetype migration 的复杂度。

**代价**：store schema 演进需要更严格的 memory accounting；部分 node 会在 hot store 中保留 sentinel/compact slot。

### AD-12：`Handle<T>` 不直接暴露组件内部 state 借用

**决定**：Handle 表达 identity/capability；状态读写通过 context-scoped query、typed property/action/update API。

**理由**：保持 lifetime、reactive tracking、hot reload state preservation 与 invalidation 的可验证性。

**代价**：某些内部高级代码比直接 `&mut T` 多一层显式 API。

### AD-13：Transform 与 Layout 使用独立失效平面

*对应 ADR 0007；ADR 0043 收紧为 transform 只标 TRANSFORM（§19.2）。*

**决定**：共享 `NodeId`，独立 TransformStore/dirty propagation；transform table 只为 transform root 分配条目，primitive 按绘制时世界坐标存储，renderer 按 root 位移差移动 draw；scroll / transform animation 不触发布局、不重建 primitive、不上传、不重建 hit-test 结构。

**理由**：保证高频滚动/动画走窄路径。

**代价**：hit-test、clip、world transform cache 需要明确同步规则。

### AD-14：自研范围使用 Ownership Ladder

*对应 ADR 0012、0025。*

**决定**：UI runtime/reactive/render integration/DSL/HIR/Shader ABI 等核心语义自研；标准算法和通用基础设施优先依赖成熟实现并通过窄边界隔离。

**理由**：把有限工程资源集中在 Viso 的性能和产品差异化上。

**代价**：需要持续维护依赖评估、替换层和版本兼容测试。

### AD-15：Viso DSL 使用 `ui!` / `view!` / `component!` 与 `.vs` 文件

*对应 ADR 0014。*

**决定**：`ui!` 使用 ViewFragment parser entry，`component!` 使用 ComponentDecl entry，`view!("...vs")` 使用外部 `.vs` CompilationUnit；三者共享同一 schema/type/effect checker、Typed HIR、Reactive/UI/Shader IR 和 runtime contracts。`.vs` 是唯一 canonical 外部文件扩展名。

**理由**：宏名直接表达作者意图，避免 `ui!` / `component!` 同时承担 fragment/component/file 的语义模糊；小型 Rust-native 使用场景保持零文件跳转，大型 UI/设计系统获得独立语言服务与 hot reload。

**代价**：compiler/source-map 必须支持 Rust macro span 与 file span 两种 source origin，并维护少量明确 parser entry points。

### AD-16：Identity 使用 Stable Symbol + Typed Dense Runtime ID

**决定**：compiler-local 名字使用 `NameId`，跨编译稳定身份使用 128-bit `SymbolId`，运行时热路径 lower 为 `PropertyId` / `EventId` / `ComponentTypeId` / `ShaderId` 等 typed dense ID；具体 UI 实例使用 generational `NodeId`。

**理由**：稳定身份与执行身份具有不同优化目标；前者服务 hot reload/schema/cache/source mapping，后者服务 direct-index hot path。

**代价**：需要显式 link/lowering 阶段和更多 typed newtype，但这些类型在 release 中没有额外抽象成本。

### AD-17：Viso 自研 `viso-ende`

*对应 ADR 0012（derive 为目标，§10.5.15）。*

**决定**：Viso 内部协议、cache、snapshot、tool transport 使用 Viso-owned Ende Binary/JSON contract；RON 不属于 Viso；Serde 只作为生态 integration。

**理由**：协议 ABI、分配行为、bounded decode、typed ID wire representation 与工具链版本语义属于框架架构。

**代价**：需要维护 binary/json encoder/decoder、fuzz 与 compatibility tests，以及目标中的 derive。

### AD-18：基础数值与几何使用独立 `viso-math`

*对应 ADR 0012（`viso-math` 是 DAG 叶子；当前只被 render/svg 使用，§10）。*

**决定**：`viso-math` 提供 allocation-free vector/matrix/rect/size/transform/geometry primitives，是无依赖的 DAG 叶子，可被任意上层 crate 使用。Math ABI 不等于 GPU ABI。

**理由**：这些类型横跨 layout、input、render、shader interface、text geometry、animation 与 game；不应被某个上层 subsystem 所有。

**代价**：需要严格控制 crate scope，防止 `math` 退化成新的 `utils/core` 垃圾桶。

### AD-19：`viso` CLI 是统一 Tooling Facade

**决定**：项目创建、环境诊断、build/run/serve、language tooling、test/inspect/profile、package/export 统一通过 `viso`；CLI、Studio、IDE、CI 复用同一套 tooling services。Desktop host 由无参数 `viso run` 隐式确定；Android/iOS 开发环境分别使用 `viso android` / `viso ios`。

**理由**：减少平台命令碎片，给人类和 AI 提供一致入口，并防止 Studio/CLI 分叉出重复 build/compiler 逻辑。

**代价**：需要稳定 command grammar、Ende JSON machine protocol、exit codes，以及 Android Emulator/iOS Simulator tooling adapter。

### AD-20：Hot Reload 只存在于 Dev artifact

*对应 ADR 0015、0016。*

**决定**：Dev Runtime、Hot Reload transport、PatchBundle apply、DevSnapshot endpoint 与 hot-reload-only metadata 只编入 Dev artifact；Release/Shipping build graph完全不包含这些路径，不能在运行时重新开启。

**理由**：Hot Reload 是开发工具能力，不是产品 runtime capability。编译期移除可以降低发布包攻击面、体积和复杂度，并保证 release frame hot path 零热更分支税。

**代价**：Rust/native 修改不能在 release artifact 上动态注入；开发期必须通过 Dev artifact，Rust code change使用 Stateful Warm Restart。

### AD-21：Development CLI 采用 host implicit、mobile simulator/emulator explicit

**决定**：macOS/Windows/Linux开发统一使用 `viso run`；不提供 `viso run macos/windows/linux/host`。`viso run ios/android` 只面向 Simulator/Emulator，`--device` 只选虚拟设备 profile。Android/iOS SDK/runtime环境由平台子命令管理；physical-device debugging不属于 Viso 1.0 development CLI。

**理由**：Desktop target由当前 host唯一决定，显式平台名只制造无效命令组合；移动开发真正需要选择的是 Simulator/Emulator runtime/profile，而不是重复描述 host平台。

**代价**：如果未来加入真机调试，需要单独定义其 security/signing/connection contract，不能偷偷扩展当前 `--device` 语义。

### AD-22：字体系统采用 On-demand OS Fallback + byte-budgeted SLRU + page-aged Atlas

*对应 ADR 0025/0026；packaged-WOFF2 条款已被 ADR 0028 取代。*

**决定**：Viso 不在启动时扫描系统字体，也不建立 framework-owned FontDB。`assets/fonts/` 在 build-time 自动进入 compact `FontManifest`，Runtime lazy load；Native 无 App font 时直接使用系统 UI/font fallback，CJK/Emoji 以 locale-aware cluster/run 交给 OS resolver；WASM/Canvas 无隐式系统字体，但可使用 packaged SFNT 字体或 External FontProvider。Loaded Font Face 使用按字节预算的 Segmented LRU；Shaping Cache 独立；glyph texture atlas 使用 page-level frame-age/CLOCK 淘汰。稳态高刷文字帧不得执行 resolve/parse/shape/raster。

**理由**：避免系统字体数量影响启动，减少常驻内存和一次性 font-picker scan pollution；复用 OS 已有的 CJK/Emoji fallback 能力而不是复制 FontDB；SLRU 保护真正热 face；page-aged atlas 避免逐 glyph LRU 与 full-atlas reset；Retained ShapedRun/AtlasEntry 让 120/144/240Hz 的成本与变化量而不是总 glyph 数相关。

**代价**：首次 cold system fallback 和新 glyph raster 仍有成本，因此必须有 Resolve/FallbackPlan cache、prewarm、worker/staging、viewport prefetch、main-thread work budget、memory-pressure policy 与高刷 benchmark。完整合同见 `Viso_Text_Font_Runtime.md`。框架不解码 WOFF2（ADR 0028）。

### AD-23：Glyph 渲染使用 Adaptive Coverage + MTSDF + Vector Pipeline

*对应 ADR 0025。*

**决定**：稳定 UI、小字号、CJK、编辑器和文档默认使用目标 device-pixel bucket 的 A8 Coverage；持续 zoom/scale/rotation 的 monochrome glyph 按需异步 promotion 为 MTSDF；变换稳定后重新生成精确 Coverage 并切回；极端 zoom/高精度场景使用 retained OutlineVector；Color Glyph 使用独立 RGBA/Vector 路径。MTSDF/Vector 都不是 FontFace load 时的 eager 资源，也不能永久与 Coverage 为所有 glyph 双份常驻。

**理由**：精确 Coverage 在稳定小字/CJK 上拥有更好的像素质量、更低的 texture bandwidth 与内存；MTSDF 在动画/连续 zoom 中摊平重复 raster/upload，并用 RGB multi-channel distance 保持尖角、Alpha true distance 支持距离效果；极端 zoom 不受有限 distance-field resolution 约束时，retained vector geometry 提供最高精度。Temporal promotion/settle-back 让资源只为当前行为付费，同时避免 120/144/240Hz 动画期间的 raster bucket churn。

**代价**：需要 representation state machine、MTSDF generation/bucket、Vector cache、跨 representation revision/generation validation 与独立 residency budget；这些复杂度由内部实现承担，不暴露给普通 App authoring。完整合同见 `Viso_Text_Font_Runtime.md`。

### AD-24：World-Ready Text 使用 Unicode 标准基线 + Logical/Visual 双映射

*对应 ADR 0025、0027。*

**决定**：复杂文字正确性统一建立在 UAX #9 / #14 / #29、locale-tailored line breaking、shaping-safe break、typed logical position 与 retained visual mapping 上；BiDi caret、selection、hit testing、IME 共享同一 logical↔visual paragraph contract。

**理由**：仅有 `bidi.rs` / `line_break.rs` 模块不能保证组合正确。BiDi 的 final reorder 依赖 line boundary，CJK 禁则依赖 locale，ligature/Arabic break 依赖 shaping context，编辑器又需要从 visual geometry 无损返回 logical text position。集中合同可以同时保证 Native/WASM/headless 一致性并支持 conformance testing。

**性能合同**：稳态帧不重新执行 BiDi、UAX #14、grapheme segmentation 或 caret-map build；所有 shaping 保持 worker-only；大段落 world-ready recompute 使用 staged commit 和 incremental reflow-until-stable。

**代价**：需要维护 Unicode data/version、locale tailoring、logical/visual mapping 和更多 conformance corpus，但这些成本位于 paragraph cold/update path，不进入 steady glyph draw hot path。

---

# Part XXIX — 风险与取舍

> **Informative。** 风险与缓解措施；其中引用的合同以对应正文章节与 ADR 为准。

## 81. 风险：NodeArena 可能使 API 变得“过底层”

缓解：

- 用户用 typed `Handle<T>`；
- `NodeId` 主要是 runtime/tooling API；
- Component macro 生成静态引用；
- Inspector 用 NodeId。

---

## 82. 风险：编译式 reactive 太复杂

如果所有 dependency 都要求编译期推导，会限制动态 UI。

取舍：

```text
Static fast path
+
Dynamic fallback path
```

静态 Rust/Viso DSL binding 使用 compact dependency table；动态 scripting 可以运行时注册 dependency，但不得让动态路径定义整个框架成本。

Viso 1.0 进一步规定：

- compiler 已知 schema/依赖的绑定 **不得静默 fallback** 到动态订阅；
- 动态依赖必须由 `bind_dynamic` / `dynamic` 语义显式产生，或给出 compiler diagnostic；
- profiler 必须暴露 `static_binding_eval`、`dynamic_binding_eval`、`dynamic_subscribe`、`dynamic_fallback_nodes` 等计数；
- Strict CI 场景中，普通 typed widget/page 出现新的 dynamic fallback 可以直接失败；
- benchmark 必须同时覆盖 100% static、mixed 10% dynamic、100% dynamic，量化性能悬崖，而不是只测静态 happy path。

---

## 83. 风险：过度 AOT 损害 live-editing / hot-reload 特性

解决：Dev 和 Release 执行同一种 UI IR 语义。

- Dev：IR 来自增量 compiler，行为由 VM 解释执行；
- Release：IR 来自 build-time compiler，行为生成 Rust 代码（ADR 0041）。

不维护两套 UI runtime：结构、binding 表、state layout、handler table 是同一份；只有行为函数体有 VM 与 native 两种执行方式，二者由 differential test 锁定为同一语义（§2.3）。

---

## 84. 风险：crate 太少导致编译慢/边界不够硬

当前的 crate 集合（§9）是起点，不是永远不拆；拆分依据必须是测量与依赖边界，而不是目录美学。

边界不能只靠人工复盘：CI 每次运行 `cargo xtask check-deps`（§10.2）；周期性架构复盘只评估是否需要拆 crate。持续检查（crate 级为当前，module 级与其余项为目标）：

- forbidden crate/module edges；
- compile timings；
- dependency fan-in/out；
- unsafe boundary；
- feature independence。

---

## 85. 风险：自己维护 GPU RHI 成本高

这是有意取舍，但 Viso 1.0 不把“所有 GPU 代码都必须从零自研”当成目标。

Viso **必须拥有 RHI contract、resource lifetime、render batching、Shader ABI 与性能语义**，因为这些会直接约束 UI/render hot path；backend 的具体实现则可以来自：

- 从经过验证的 backend 实现中提炼；
- Viso 自研；
- experimental/reference wgpu adapter；
- 对特定平台的成熟底层依赖。

是否替换某个 backend 的判据是 capability、profile 和维护成本，不是意识形态。通用 abstraction 可以作为参考/兼容路径，但不能反向定义 Viso public renderer architecture 或迫使 Tier-1 backend 退化到最低公分母。

---

## 85.1 风险：同时自研过多基础设施导致范围失控

这是 Viso 1.0 最需要主动管理的结构性风险之一。

Viso 的目标架构可以拥有 RHI、Shader IR、DSL、text integration、reactive runtime、a11y semantics，但 **目标所有权不等于第一天全部从零实现**。执行策略：

1. 先定义 Viso-owned contract；
2. 能用成熟依赖或经过验证的实现完成 vertical slice 的，先通过窄 adapter 接入；
3. profiler/capability 证明成为瓶颈后，再 fork、替换或自研对应实现；
4. 不允许“为了最终纯自研”阻塞从 input → state → layout → paint → GPU 的可测量 native vertical slice；
5. 每个替换项目必须同时删除被替换 adapter 路径，避免永久双实现。
6. Embedded 范围按 Device Class 逐级推进：Embedded-GPU → Embedded-CPU → MCU。前一级没有板卡实测数据之前，不开始下一级的 platform 实现；MCU 在 1.0 只守不变量（§50A.6）。

优先做到 100 分的部分是：

```text
Node/identity
incremental invalidation
layout/scroll hot path
paint/batching/upload
DSL typed HIR + AOT/hot reload contract
profiling/diagnostics
```

标准算法和通用基础设施优先达到“可靠、可替换、可测”，而不是追求仓库内全部自有实现。

## 86. 风险：Service 层变成另一个 God Object

不要提供：

```rust
cx.services().everything().do_anything()
```

而是小 trait + capability registry。

服务之间不能随意相互依赖。

---

# Part XXX — Definition of Done

## 87. Viso 核心完成标准

不是“新目录都建好了”，而是同时满足：

### API

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

能构建真实跨平台应用。

### Runtime

- NodeArena 是默认 tree；
- 新 UI 热路径没有 `Rc<RefCell<dyn Widget>>`；
- local state change targeted invalidation；
- idle 真正 idle。

### Layout/Input

- Row/Column/Grid/Stack/Scroll/List；
- virtualization；
- focus/IME/gesture；
- adaptive/mobile insets。

### Renderer/GPU

- automatic batching；
- explicit GPU instance ABI；
- backend static specialization；
- partial upload；
- profiler counters。

### Text

- shaping/fallback/BiDi；
- paragraph cache；
- editor-grade input path。

### Viso DSL / Hot Reload

- CST/AST/HIR；
- typed schema；
- module graph；
- AOT release；
- atomic hot reload；
- hot-reload state preservation。

### Engineering

- clear crate DAG；
- CI dependency rules；
- benchmark regression gates；
- headless tests；
- Studio only depends downward/public tooling APIs。

### Tooling / CLI

- `viso` 覆盖 new/check/build/run/test/package/export 主路径；
- CLI 与 Studio 复用同一套 compiler/build/inspection services；
- 所有自动化命令支持稳定 `--json` 输出；
- Web GPU/DOM/Hybrid target 可构建；
- HTML/Solid exporter 对 unsupported feature 给出结构化诊断；

### Footprint / Embedded

- 每个 Footprint Profile 的体积、内存、首帧、空闲唤醒进入 CI；
- Embedded-GPU 板卡达到 §50A.5 预算；Embedded-CPU 可运行官方 Tier 1–3 widget；
- 空闲时所有 Device Class 0 wakeup；局部变化走 damage present；
- MCU-ready 不变量在 CI 中通过。

---

# Appendix A — 一帧完整数据流

```text
OS / Device
    ↓
viso-platform
    ↓ PlatformEvent
viso-runtime
    ↓ normalized event
viso-ui Input Router
    ↓ target NodeId
Capture → Target → Bubble
    ↓
Component / handler state mutation
    ↓
State Transaction
    ↓
Binding Queue
    ↓
Dirty Propagation
    ├── STRUCTURE
    ├── STYLE
    ├── MEASURE
    ├── LAYOUT
    ├── TRANSFORM
    ├── PAINT
    ├── HIT_TEST
    └── SEMANTICS
    ↓
Incremental UI phases（按 dirty worklist）
    ├── Style / Measure / Layout
    └── UpdateTransforms ──→ root 位移 → Renderer::set_transforms（不经过 paint）
    ↓
Paint Cache Changes
    ↓
Primitive Ranges
    ↓
Batch Builder + Damage Region
    ↓
Frame Packet
    ↓
viso-gpu（damage 为空则不 Submit）
    ↓
Metal / D3D12 / Vulkan / WebGPU / GLES3 / CpuRaster
    ↓
present_with_damage
    ↓
RunEffects → UpdateSemantics（present 之后）
```

---

# Appendix B — State 更新完整示例

用户（示意；当前 API 通过 `StateId` 与 handler context 写入，ADR 0005/0006）：

```rust
self.count.set(self.count.get() + 1);
```

编译期 metadata：

```text
StateId 4
  ├── Binding 12 → Node 81 / Label.text / MEASURE|LAYOUT|PAINT|SEMANTICS
  └── Binding 13 → Node 82 / Progress.value / PAINT
```

运行时：

```text
StateSlot[4].version += 1
enqueue Binding 12,13
        ↓
Binding12 evaluates text
        ↓
Text content version++
Node81 MEASURE|LAYOUT|PAINT|SEMANTICS
        ↓
MEASURE bubbles until an ancestor Fixed on both axes (§19.1)

Binding13 evaluates float
Node82 PAINT
        ↓
next frame only affected branches processed
```

不执行：

```text
rebuild whole component
virtual tree allocation
tree diff
full redraw traversal
```

---

# Appendix C — Large List 滚动示例

```text
100,000 logical items
        ↓
viewport + overscan
        ↓
~40 mounted item nodes
        ↓
scroll delta
        ↓
visible range changes by 3
        ↓
recycle 3 old nodes
bind 3 new item records
update transforms/layout cache
        ↓
only changed text/quad instance ranges uploaded
```

稳态滚动目标：

- 无每帧节点 heap churn；
- item host pool 重用；
- stable glyph cache；
- minimal GPU upload；
- hit-test 只针对 visible nodes/index structure。

---

# Appendix D — 推荐性能日志

```text
Frame #18291                         6.84 ms
------------------------------------------------
Input                               0.08 ms
State flush                         0.04 ms
Style                               0.03 ms
Measure                             0.21 ms
Layout                              0.34 ms
Semantics                           0.01 ms
Paint                               0.42 ms
Batch                               0.19 ms
GPU upload                          0.12 ms
GPU                                 4.91 ms
Present/other                       0.49 ms

Nodes                               12,482
Visible nodes                        2,164
Dirty style                              3
Dirty layout                            31
Dirty paint                             72
Draw calls                               9
Quad instances                       3,771
Glyph instances                      8,032
Upload                             176 KiB
Heap allocations/frame                   0
```

数字仅为格式示例；真实阈值必须由 benchmark 建立。

---

# Appendix E — 推荐 `prelude`

当前 prelude 以 `crates/viso/src/lib.rs` 的 `pub mod prelude` 为准。原则：只包含稳定、高频、无歧义的项；不要把 renderer/GPU/platform 内部类型 re-export 进 prelude。

当前包含（摘要）：`Application`、`run`、`AppCx`、`BuildCx`、`NodeId`、`DirtyClass`、`StateId`/`StateValue`、layout style（`FlexStyle`、`GridStyle`、`TrackSizing` 等）、`Role`/`Semantics`、已落地的官方 widget 及其 style/handle、`window`/`WindowConfig`/`WindowHandle`、`Menu`/`MenuCommandId`/`Accel`/`SystemAction`、`ServicesExt` 与 `ui!`。

目标补充：`Computed`、`Event`、`Task`、`Route`、`Theme`、`Color`、`Vec2`、`Rect`、`Constraints`，以及 `component!` / `view!`（已在 `viso-ui-macros` 实现，尚未进入 prelude）和目标宏 `routes!`、`#[component]`。

---

# Appendix F — 不应出现的反模式

### F.1 UI 热路径

```rust
Rc<RefCell<Box<dyn Widget>>>
```

作为新节点默认存储：禁止。

### F.2 每帧字符串查找

```rust
for node in nodes {
    let color = node.properties.get("background");
}
```

禁止。

### F.3 用户控制 batching correctness

```text
new_batch: true
```

作为普通 UI 正确性要求：禁止。

### F.4 全局 redraw

```rust
cx.redraw_all();
```

作为状态更新默认方式：禁止。

允许 debug/escape hatch，但 profiler 必须显示其成本。

### F.5 为分层而 dyn

```rust
Box<dyn LayoutNode>
Box<dyn PaintNode>
Box<dyn HitTestNode>
```

每节点每帧动态分派：默认禁止。

### F.6 为性能而所有东西 unsafe

性能关键数据结构可以使用 unsafe 实现，但安全边界必须封装并有 benchmark 证明收益。

---

# Appendix G — 已决地基问题与剩余 Open Questions

## G.1 Viso 1.0 已决地基问题

以下问题不再属于 open questions：

1. Node hot storage：**hybrid indexed SoA**（AD-11）；
2. `Handle<T>`：**不直接暴露跨帧 typed state borrow/mutation**（AD-12）；
3. Transform：**与 Layout 使用独立失效平面，共享 NodeId**（AD-13）；
4. 自研/依赖：**使用 Ownership Ladder**（AD-14）；
5. DSL source form：**`ui!` / `component!` / `view!("...vs")` 共享 schema/HIR/IR/runtime，`.vs` 为唯一 canonical 外部格式**（AD-15）；
6. Identity：**128-bit Stable `SymbolId` + typed dense runtime IDs + generational `NodeId`**（AD-16）；
7. Ende：**Viso-owned Binary/JSON，RON 不进入 Viso core，Serde 只做 integration**（AD-17）；
8. Math：**独立 `viso-math`，allocation-free 基础数值/几何 ABI，Math ABI 与 GPU ABI 分离**（AD-18）；
9. CLI：**`viso` 是统一 Tooling Facade，CLI/Studio/IDE/CI 共用 tooling services**（AD-19）；
10. Hot Reload：**仅 Dev artifact 编入，Release/Shipping 完全移除 Dev Runtime/patch/DevSnapshot 路径**（AD-20）；
11. Development target：**desktop host implicit；iOS/Android 只面向 Simulator/Emulator，平台开发环境使用 `viso ios/android`**（AD-21）；
12. Release 执行：**行为生成 Rust，VM 仅 Dev/dynamic**（ADR 0041）；
13. 编译器依赖：**`dsl -> schema`，编译器不链接运行时栈**（ADR 0042）；
14. Transform：**transform root + draw 级位移，TRANSFORM 不连带 PAINT/HIT_TEST**（ADR 0043）；
15. Embedded：**Device Class + Footprint Profile；Embedded-GPU 为 1.0 Tier-1，Embedded-CPU 为 Tier-2，MCU 只守不变量**（ADR 0044）。

这些决定应尽早被 prototype/benchmark 验证，但验证的默认动作是调整实现参数，而不是重新打开核心语义。若要推翻，必须新 ADR。

## G.2 仍需 prototype/benchmark 的问题

1. clip cache 的最佳结构；
2. text shaping cache key 与跨 paragraph 共享粒度；
3. GPU instance buffer 是 per-type persistent pool 还是 frame ring + retained ranges 混合；
4. Web 无 WebGPU 时的兼容路径：默认使用 CpuRaster（§30.4）；是否另做 WebGL2 backend 视 adoption 数据决定；
5. Desktop Linux / Android 上 GLES3 兼容路径的投入时机（Embedded-GPU 的 GLES3 backend 已是 Tier-1，可以直接复用）；
6. dynamic scripting VM 的保留范围；
7. platform backend 何时从 module 升格独立 crate；
8. AccessKit adapter 覆盖 macOS/Windows/Linux/Android，iOS 与 Web 由 Viso 自有 adapter 补齐（ADR 0030）；剩余能力缺口（Web 焦点跟随、文本编辑语义）待定；
9. Rust declarative API 除 `ui!` / `component!` / `view!` 外是否还需要 builder/function syntax；
10. 3D scene graph 放 render、extras 还是独立 crate；
11. paragraph cache 是否跨 component 共享以及内存上限；
12. GPU retained instance range 的碎片整理策略；
13. Embedded-GPU 上 GLES3 与 Vulkan 的默认选择（按 SoC 驱动质量逐板决定，还是统一 GLES3）；
14. damage rect 合并上限 N 与整帧退化阈值的 per-profile 取值；
15. 作者可写 scale / rotate 时，draw 级 uniform 扩为 Affine2（24 bytes），还是对非平移 root 退回 PAINT；
16. MCU line-buffer rasterizer 的行缓冲高度与 DMA 双缓冲策略（MCU ADR 开启时）。

原则：**先确定不可妥协的性能/边界语义，再通过 benchmark 和实现经验决定具体容器、cache 和兼容层级。**

---

# Appendix H — 来源与设计依据（informative）

本文综合了：

1. 用户提供的《Makepad 重构设想：从 UI Engine 到 Rust-first 跨平台 App Framework》，其中关于单一 facade、极简入口、App Framework、移动能力、Viso DSL/热更新工程化、工具链的建议被部分采纳；crate 过度拆分、过多文件类型、页面默认多文件、强 App 架构等部分被本文主动收敛。
2. Makepad 当前 `dev` 分支 README：<https://github.com/makepad/makepad/blob/dev/README.md>
3. Makepad 当前 workspace：<https://github.com/makepad/makepad/blob/dev/Cargo.toml>
4. 当前 Agent/开发说明：<https://github.com/makepad/makepad/blob/dev/AGENTS.md>
5. 当前 Widget 实现：<https://github.com/makepad/makepad/blob/dev/widgets/src/widget.rs>
6. 当前 platform dependencies：<https://github.com/makepad/makepad/blob/dev/platform/Cargo.toml>
7. Makepad 当前 `script_mod!` / `ScriptVm`、实时编辑与 game authoring 仅作为 DSL/工具体验参考；相关说明以 `dev` 分支 `AGENTS.md` 和实现源码为准，`splash.md` 仅作为补充资料：<https://github.com/makepad/makepad/blob/dev/splash.md>

Makepad 仅作为外部参考资料；本文只定义 Viso 自身架构。

---

# 结论

Viso 同时维护三层稳定契约：

### 对用户

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

少概念、少样板、默认正确。

### 对框架维护者

crate 清单与依赖 allowlist 见 §9/§10（`cargo xtask check-deps`）：依赖边界明确、职责可测试、工具链可观测。

### 对 CPU / GPU

```text
integer IDs
arena/chunk storage
dirty bitsets
incremental layout
retained paint cache
compact primitive data
instance batching
persistent resources
partial uploads
static backend specialization
```

最终设计原则可以压缩成四句话：

> **外部 declarative，内部 retained。**  
> **外部对象化，内部 data-oriented。**  
> **开发期 dynamic，发布期 AOT。**  
> **冷路径抽象，热路径扁平。**

后续架构决策应能通过这四条与可重复 benchmark 检验。
