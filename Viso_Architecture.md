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
                Text { text: count; }
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
3. **Transform：与 Layout 使用独立的失效/传播平面。** 二者默认共享 `NodeId` 和结构语义，但 transform/scroll/animation 可在不触发布局的情况下局部更新。
4. **自研与依赖必须分级。** Viso 只在决定差异化、热路径和语义所有权的部分强制自研；Unicode/shaping、accessibility bridge、async executor、网络/TLS、媒体 codec 等优先复用成熟实现，并由 Viso 自己掌握 integration/cache/ABI contract。
5. **模块边界必须机器化验证。** crate DAG 与关键 module import 规则进入 `cargo xtask arch-check`/CI，不再只依赖 AGENTS.md 和 code review。
6. **Reactive dynamic fallback 必须显式、可计数、可 benchmark。** 编译器能够静态解析的绑定不得静默掉入动态路径。
7. **Web/Mobile/Linux backend 采用 tier + capability 模型。** Tier-1 明确优化现代主后端；兼容后端可追加，但不得反向把 renderer 设计压成最低公分母。
8. **Viso DSL 使用 `ui!` / `view!` / `component!` 三个语义化 Rust 入口。** `ui!` 解析 View Fragment，`component!` 解析 Component，`view!("...vs")` 编译外部 `.vs`；三者共享同一 schema/HIR/IR/runtime 语义。`.vs` 是唯一 canonical 外部 DSL 文件扩展名。
9. **Identity 分层。** 源码名字使用 compiler-local `NameId`，跨编译稳定身份使用 128-bit `SymbolId`，运行时热路径 lower 为 `PropertyId`/`EventId`/`ComponentTypeId` 等 typed dense ID，实例使用 generational `NodeId`；禁止一个万能 ID 贯穿所有 subsystem。
10. **Viso 自研 Ende。** `viso-ende` 负责内部 Binary/JSON Encode/Decode、wire schema、协议版本、cache/snapshot/tool transport；RON 不属于 Viso；Serde 只作为生态 integration。

这些条目属于 Viso 1.0 的架构合同；如果要改变，必须新增/修改 ADR 并附 benchmark 或实现证据。

---

# Part I — 总体判断与设计原则

## 1. Viso 的核心方向

Viso 的产品与技术方向固定为：

- Rust-native application runtime；
- GPU-first 2D/3D rendering；
- 自绘 UI，而不是依赖平台原生控件拼装；
- 跨 macOS / Windows / Linux / iOS / Android / Web；
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
- 可以 strip 调试符号与 inspector metadata。

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
- 让用户理解 GPU batching、DrawPass、shader ABI 才能画普通 UI。

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
- ResourceId / asset lifetime / framework-level profiling counters。
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

Accessibility 的 canonical model 是 Viso 自有 semantics tree；平台 adapter 优先复用 AccessKit 等成熟桥接能力，只在能力或性能不满足时实现平台专用 adapter。

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
#[component]
ui! { ... }
view!("...vs")
routes!
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
                Label { text: "Hello Viso" }
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

用户侧组件应该非常普通：

```rust
#[component]
struct Counter {
    count: State<i32>,
}

impl Counter {
    fn view(&self) -> impl View {
        ui! {
            Column {
                Label { text: format_args!("Count: {}", self.count.get()) }
                Button {
                    text: "Add"
                    on_click: |_| self.count.update(|v| *v += 1)
                }
            }
        }
    }
}
```

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

## 8. 推荐顶层仓库

```text
viso/
├── Cargo.toml
├── README.md
├── ARCHITECTURE.md
├── Viso_CLI.md
├── AGENTS.md
├── architecture.toml
├── rustfmt.toml
│
├── crates/
│   ├── viso/
│   ├── macros/
│   ├── ende/
│   ├── math/
│   ├── runtime/
│   ├── platform/
│   ├── gpu/
│   ├── shader/
│   ├── text/
│   ├── render/
│   ├── ui/
│   ├── widgets/
│   ├── dsl/
│   └── services/
│
├── integrations/
├── extras/
│
├── tools/
│   ├── cli/
│   ├── inspector/
│   ├── studio/
│   └── packager/
│
├── examples/
├── benches/
├── tests/
├── docs/
├── xtask/
└── vendor/
```

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

Proc macros：

- `#[component]`；
- `ui! { ... }` inline DSL frontend；
- `view!("...vs")` external DSL module reference/build integration；
- state/binding metadata；
- shader instance layout；
- Viso DSL schema；
- static template 生成；
- compile-time diagnostics。

`viso-macros` 不拥有第二套 DSL parser。inline/file source 都必须复用 `viso-dsl` 的 frontend/HIR 语义；宏层只负责 Rust token/span、build artifact 与 typed API glue。

### `viso-ende`

Viso-owned Encode/Decode infrastructure：

- Binary wire format；
- JSON diagnostics/tool interchange；
- bounded decoder；
- protocol/schema metadata；
- cache/snapshot encoding；
- Studio/Inspector/Profiler/Hot Reload transport；
- core typed ID 的 canonical wire representation。

不支持 RON；不承担 image/audio/video media codec；不作为 frame 内部数据模型。Serde compatibility 位于 `integrations/serde`。

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
- accessibility bridge hook。

平台实现初期放在同一 crate 的 `os/` 目录，必要时再拆独立 backend crate。

### `viso-gpu`

极薄 GPU RHI：

- device；
- queue；
- buffer；
- texture；
- sampler；
- pipeline；
- bind group；
- command encoder；
- surface；
- fences/sync；
- backend resource handles。

### `viso-shader`

- shader parser/type checker；
- Shader IR；
- platform codegen；
- reflection；
- instance/uniform schema；
- shader cache key。

### `viso-text`

- font DB；
- fallback；
- shaping；
- BiDi；
- line breaking；
- paragraph layout；
- glyph cache/atlas integration metadata。

### `viso-render`

- paint primitive；
- retained paint cache；
- clip/layer；
- batch builder；
- image/glyph atlas management；
- render graph；
- GPU upload plan；
- frame packet。

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
- paint node bridge。

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
- type checker；
- module graph；
- UI IR；
- hot reload diff；
- hot-reload state preservation metadata；
- source map；
- 可选 dynamic VM adapter。

### `viso-services`

统一 app service protocol：

- file；
- share；
- notifications；
- permissions；
- camera；
- location；
- secure storage；
- haptics；
- media；
- networking adapter。

低频 service 可以使用 trait object，避免把所有 OS 逻辑塞回 platform/runtime。

---

## 10. Crate 依赖 DAG

推荐逻辑依赖：

```text
                       viso
                        │
              ┌─────────┼──────────┐
              │         │          │
          widgets       dsl     services
              │         │          │
              └────┬────┘          │
                   ↓               │
                   ui              │
                   │               │
                   ↓               │
                 render            │
               ┌───┴────┐          │
               ↓        ↓          │
             text     shader       │
               └───┬────┘          │
                   ↓               │
                  gpu              │
                   │               │
                   └──────┬────────┘
                          ↓
                       runtime
                          │
                          ↓
                       platform

`viso-math` 位于数值/几何依赖底层，被 gpu/shader/text/render/ui 等按需单向依赖：

    gpu ─────┐
    shader ──┤
    text ────┤
    render ──┼──> viso-math
    ui ──────┤
    widgets ─┤
    game/extras ┘
```

`viso-ende` 是低层共享基础设施，不位于 frame 数据流主链：

```text
             dsl        runtime       tools
              │            │            │
              └──────┬─────┴─────┬──────┘
                     ↓           ↓
                 viso-ende   (typed runtime memory)

Ende 不依赖 math/ui/render/gpu/widgets/platform/dsl/runtime；上层按需单向依赖 Ende。`viso-math` 与 `viso-ende` 彼此不形成强制依赖。
```

实际 Rust DAG 可以微调，例如 runtime/platform/gpu 的底部关系因 surface creation 需要通过小接口反转，但必须保持以下禁令：

### 10.1 禁止依赖

- `platform -> ui`：禁止；
- `platform -> widgets`：禁止；
- `platform -> dsl`：禁止；
- `gpu -> ui`：禁止；
- `render -> widgets`：禁止；
- `ui -> widgets`：禁止；
- `ui -> Studio`：禁止；
- `runtime -> Studio`：禁止；
- `viso-dsl -> concrete widget implementation`：原则上禁止，使用 schema/registry；
- framework core crate 依赖 app/example：绝对禁止。
- `viso-ende -> math/runtime/ui/render/gpu/widgets/platform/dsl`：禁止；Ende 必须保持低层、无框架反向依赖；
- `viso-math -> ende/runtime/platform/gpu/shader/text/render/ui/widgets/dsl/services`：禁止；Math 必须保持纯数值/几何底层；
- `viso-math` public ABI 依赖 pointer width、`usize` 或 backend SIMD type：禁止；
- `viso-math` struct 内存布局直接作为 GPU uniform/instance wire ABI：禁止；
- frame hot path 通过 Ende encode/decode 传递 Node/Layout/Paint 数据：禁止。

### 10.2 边界必须由 CI 机器化验证

AGENTS.md 里的 dependency 规则不是“建议”。仓库应维护机器可读的 architecture policy（例如 `architecture.toml`），并由：

```text
cargo xtask arch-check
```

至少验证：

- crate allow/deny edge（基于 `cargo metadata`）；
- framework crate 不得反向依赖 `tools/`、`examples/`、Studio；
- `platform/ui/render/gpu/dsl` 等关键 module 的 forbidden import；
- target-only dependency 不得泄漏到其他 target；
- public facade re-export 不得绕过稳定性等级。

module-level 规则初期可以通过 first-party AST/import scanner 实现；`cargo-modules` 等工具可用于可视化和辅助诊断，但 CI 的最终合同应由 Viso 自己控制，避免边界只存在于文档。

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

Workspace：

```text
crates/
├── ende/
│   ├── Cargo.toml        # package = "viso-ende"
│   └── src/
│       ├── lib.rs
│       ├── encode.rs
│       ├── decode.rs
│       ├── error.rs
│       ├── limits.rs
│       ├── bin/
│       │   ├── mod.rs
│       │   ├── encoder.rs
│       │   └── decoder.rs
│       └── json/
│           ├── mod.rs
│           ├── encoder.rs
│           └── decoder.rs
```

Derive proc macros 放在现有 `viso-macros`，不额外制造一串 derive/core/schema crate：

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
fixed-width integer for core IDs/counters
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

JSON encoder/decoder 必须共享 Ende 的：

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

现有 `viso-macros` 提供：

```text
#[derive(Encode)]
#[derive(Decode)]
```

derive 必须生成直接 field encode/decode，不建立运行时 reflection map。

Macro expansion 必须：

- 给字段/variant 生成稳定 schema metadata；
- 在编译期拒绝不支持的裸 pointer/reference lifetime；
- 对 `usize/isize` 的持久 wire 使用给出诊断或要求显式 adapter；
- 支持 `#[ende(...)]` 一类受控 attribute 时，attribute 集合必须小且有明确 Schema；
- 不允许 attribute 改变基础 tokenization 或引入运行时字段名查找。

### 10.5.16 Ende 与 Identity Architecture

Ende 必须为 Viso 核心 ID 提供显式实现：

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
- 为极限 wire size 默认引入复杂 varint/delta/compression。

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

一帧建议明确为：

```text
1. CollectInput
2. DispatchInput
3. FlushStateTransactions
4. ResolveStyle
5. Measure
6. Layout
7. UpdateSemantics
8. BuildPaintChanges
9. BuildRenderBatches
10. UploadGpuChanges
11. Submit
12. PostFrameCleanup
```

实际可以合并阶段，但语义必须存在。

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

Runtime 需要聚合以下原因：

```text
Input dirty
State dirty
Animation active
Timer due
Async completion
Window resize
External surface invalidation
Viso hot reload
```

最终决定：

```text
NoFrame
FrameAtNextVsync
ImmediateFrame
BackgroundMaintenance
```

多个 state mutation 在一个输入事件中自动 batch。

### 12.1 Idle 必须真正 idle

没有动画、输入、任务和脏数据时：

- 不持续 layout；
- 不持续 paint；
- 不持续 submit；
- 不周期性遍历 UI tree；
- 不轮询每个 component。

---

## 13. Async runtime

提供最小官方 API：

```rust
let task = cx.spawn(async move {
    api.fetch_user().await
});
```

结果可以绑定组件生命周期：

```rust
cx.spawn_scoped(async move {
    let data = load().await?;
    Ok(AppAction::Loaded(data))
});
```

Scope 被销毁时自动取消或 detach，策略显式。

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

Tokio/Smol 作为 adapter：

```text
viso-tokio
viso-smol
```

不把所有 adapter 组合直接做成核心 crate feature matrix。

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
struct NodeLinks {
    parent: Option<NodeId>,
    first_child: Option<NodeId>,
    last_child: Option<NodeId>,
    prev_sibling: Option<NodeId>,
    next_sibling: Option<NodeId>,
}
```

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
    generation: u32,
    occupied: bool,
    links: NodeLinks,
    flags: NodeFlags,
}
```

实际优化版可以把 `generation/occupied/links/flags` 分离成更紧凑数组。

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
#[repr(transparent)]
pub struct StateId(u32);

struct StateSlot {
    version: u32,
    flags: StateFlags,
    value_offset: u32,
    binding_start: u32,
    binding_len: u16,
}
```

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

Binding metadata：

```rust
struct Binding {
    target: NodeId,
    kind: BindingKind,
    dirty: DirtyMask,
    evaluator: BindingEvaluator,
}
```

对于纯 Rust 静态模板，`evaluator` 优先是 monomorphized/static function ID，而非通用 closure box。

对于 Viso DSL UI，使用 compact bytecode/IR function。

---

## 19. Dirty flags

建议至少区分：

```rust
bitflags! {
    struct DirtyMask: u16 {
        const STRUCTURE = 1 << 0;
        const STYLE     = 1 << 1;
        const MEASURE   = 1 << 2;
        const LAYOUT    = 1 << 3;
        const TRANSFORM = 1 << 4;
        const PAINT     = 1 << 5;
        const HIT_TEST  = 1 << 6;
        const SEMANTICS = 1 << 7;
    }
}
```

属性需要定义自己的 invalidation contract。

例：

| 变化 | Dirty |
|---|---|
| text 内容 | MEASURE + LAYOUT + PAINT + SEMANTICS |
| text color | PAINT |
| background | PAINT |
| width | MEASURE + LAYOUT |
| transform | TRANSFORM + HIT_TEST + PAINT bounds |
| aria/label | SEMANTICS |
| hover state | STYLE，随后由 style diff 决定 PAINT/LAYOUT |
| visibility | LAYOUT/PAINT/HIT_TEST/SEMANTICS，具体取决于模式 |

### 19.1 Dirty propagation

不是所有脏状态都向整棵祖先树传播。

例如：

- `PAINT` 通常不需要让祖先 layout dirty；
- `MEASURE` 需要传播到受 child intrinsic size 影响的祖先，直到 fixed constraint boundary；
- `TRANSFORM` 可以只影响 transform subtree；
- `SEMANTICS` 只更新 semantics tree 对应分支。

需要专门的 propagation rules，而不是简单 `parent.redraw()`。

### 19.2 Viso 1.0：Transform 是独立失效平面

Layout tree 定义结构与几何约束；Transform plane 定义最终局部/世界变换、滚动偏移和动画变换。两者使用同一个 `NodeId` 索引，但 dirty propagation 分离：

```text
Layout change
  -> MEASURE/LAYOUT
  -> may update transform base rect

Scroll / translate / scale / opacity-only animation
  -> TRANSFORM / HIT_TEST / PAINT bounds
  -> no MEASURE/LAYOUT by default
```

默认 transform parent 跟随 UI parent；overlay、portal、独立 layer 等场景可以显式拥有不同的 transform/paint parent。该能力必须通过受控 API 表达，不能靠任意矩阵引用形成不可追踪图。

这样做的主要目的不是“架构漂亮”，而是保证滚动和 transform animation 不因为层级传播而触发布局。

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

以及：

```text
Auto
Px
Percent
Fr
MinContent
MaxContent
```

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

```text
background: token(color.surface)
radius: token(radius.md)
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
DirtyMask
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

尽量把 animation dirty 限定到：

- TRANSFORM；
- PAINT；

避免无意义触发布局。

只有 width/height 等 layout property animation 才触发 layout。

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

建议稳定的底层 primitive：

```text
Quad
Border
GlyphRun
Image
VectorPath
Mesh2D
Mesh3D
Clip
Layer
Custom
```

其中 Border 可与 Quad 合并实现，取决于 pipeline 设计。

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

```rust
#[repr(C)]
#[derive(GpuInstance)]
struct QuadInstance {
    rect: [f32; 4],
    color: [f32; 4],
    radius: [f32; 4],
    border: [f32; 4],
    transform_id: u32,
    flags: u32,
}
```

宏必须生成明确 layout descriptor，并验证 shader 声明。

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
#[derive(GpuInstance)]
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
Web       → WebGPU
```

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

`viso-text` 不是普通 Widget 辅助库。

负责：

```text
Font Database
Fallback
Unicode Script Detection
BiDi
Shaping
Line Break
Paragraph Layout
Glyph Cache
Glyph Atlas Metadata
```

### 37.1 三级缓存

建议：

```text
Font/Face Cache
      ↓
Shaping Cache
      ↓
Glyph Atlas Cache
```

Key 必须可增量失效。

### 37.2 Paragraph cache

Text 未变：不 reshape。  
Font/feature 未变：不 reshape。  
可用宽度未变：不重新 line-break。  
Glyph 已在 atlas：不 raster/upload。

### 37.3 文本编辑

TextInput/CodeEditor 使用专门的数据结构：

- rope/piece table（根据编辑器需求）；
- grapheme-aware cursor；
- selection spans；
- shaping segment cache；
- viewport line cache。

不要让大型代码编辑器通过“每个字符一个 UI Node”实现。

### 37.4 Text ownership boundary

`viso-text` 必须拥有 paragraph/shaping cache、glyph atlas contract、增量失效、UI integration 与 profiler；但 **不要求 Viso 重写 Unicode/BiDi/shaping 标准算法**。

优先策略是复用经过验证的 shaping/Unicode/font primitives（必要时 vendor/fork），并用 Viso 自己的数据布局与 cache API 包裹。只有在 profiler 证明通用实现阻碍关键性能或缺失必要能力时，才把对应算法纳入自研范围。

---

# Part XIV — Viso DSL、热更新与语言工程

## 38. 定位：Viso DSL 是一等 Authoring Layer，不是 UI Runtime 的地基

必须保持依赖方向：

```text
viso-dsl
     ↓
 viso-ui
```

而不是：

```text
viso-ui
     ↓
Script VM
```

纯 Rust 应用即使完全关闭 `hot-reload` feature，UI runtime、layout、renderer、widgets 的基础能力仍然成立。

### 38.1 三个 Rust 入口，一套语言语义

Viso DSL 正式支持三个 Rust-side authoring entry point：

```rust
// View Fragment grammar
let toolbar = ui! {
    Row {
        Button { text: "Save"; }
    }
};

// External .vs source
let page = view!("features/home/view.vs");

// Inline Component grammar
component! {
    Counter {
        state count = 0;
        view {
            Column {
                Text { text: count; }
            }
        }
    }
}
```

三者必须共享：

```text
lexer/token model
component/native schema
name resolution
type/effect/capability checking
Typed HIR
Reactive IR
UI IR
Shader IR
diagnostics/source maps
```

允许 parser 有不同**入口 production**，不允许有不同**语言语义**：

```text
ui!          -> ViewFragment entry
component!   -> ComponentDecl entry
view!(path)  -> .vs CompilationUnit entry
```

`ui!` 和 `component!` 是 Rust proc-macro/compiler frontend；它们不代表运行时宏系统，也不允许每次 frame 展开/rebuild UI。`view!` 在构建期把外部 `.vs` 纳入 module graph；Dev watcher 可以独立重新编译该文件并进行 transactional hot reload。

Release 中所有入口都必须生成相同的 compact AOT descriptors/IR，不在启动时 parse `.vs`，也不要求 runtime 解析 Rust source。

### 38.2 DSL 的定位：不是 Rust 2，也不是纯模板语言

Viso DSL 必须同时做到 **极低 authoring 摩擦** 和 **足够开放的实时 UI/游戏能力**；同时不能把 `.vs` 做成第二门必须重新实现完整 Rust 的通用语言。

因此语言能力按 Surface 分层：

**Core Surface（必须先稳定）**

```text
import
component / input / state / computed / action / view
record / enum
node / property / event / if / match / keyed for
system + imported scheduler traits
basic fn / expression / pattern
shader interface + shader body
```

**Standard Surface（应用工程能力）**

```text
effect / task / resource
slot
style / theme
structured async + cancellation
hot-reload migration metadata
```

**Advanced Surface（不得阻塞 1.0 vertical slice）**

```text
user-defined trait / impl
general generics / const generics
trait objects
Template / Part meta-programming
hand-written native declarations
fine-grained capability annotations
compiler plugins
```

Advanced Surface 可以长期存在，但在实现顺序、Quick Start、默认 formatter examples 和 AI context 中必须被隔离。普通 UI 和普通游戏脚本不应要求理解这些能力。

### 38.3 Viso DSL 的 authoring 规则

Viso DSL 的 surface 直接围绕“声明式 UI 与 imperative behavior 可一眼区分”设计：

```viso
Text {
    text: label;
    color: theme.colors.foreground;
}
```

Property Binding 使用 `:`；行为赋值继续使用普通 assignment：

```viso
count = value;
count += 1;
```

命名节点显式写为：

```viso
node add_button: Button {
    text: "Add";
}
```

Viso 不使用一组隐式 apply/merge 运算符来表达节点创建、覆盖或继承；merge、style、override、replace、record update 都必须拥有独立、可类型检查的语义构造。

这样 Property Binding 和 imperative assignment 在 AST 与视觉层都天然分离，同时保持 DSL 紧凑、易读、易生成。分号作为普通 property/behavior statement 的稳定终止符，避免依赖换行敏感语法，并有利于 Lossless CST、formatter 与 incremental parser。

### 38.4 普通 `.vs` 文件不强迫写语言头和 module 头

语言版本由 `Viso.toml` / package lock 决定；module path 默认由 package + source path 决定。普通文件因此可以直接从 import/declaration 开始：

```viso
import viso::widgets::{Column, Text, Button};

export component Counter {
    state count = 0;

    view {
        Column {
            Text { text: count; }
            Button {
                text: "Add";
                on click { count += 1; }
            }
        }
    }
}
```

显式 language/module header 可以保留给 compiler conformance fixtures、generated standalone modules 或未来 package interchange，但不应成为正常 app authoring 的必写 ceremony。

### 38.5 类型显式度：边界严格，私有局部允许推断

必须显式类型：

```text
public/exported API
input/event/slot/native/shader interface
persistent external schema boundary
```

可以推断：

```text
private state (from stable initializer)
private computed
local let
closure parameters with expected type
numeric literal width from typed property/schema
```

因此 Counter 可以写 `state count = 0;`。编译器仍把最终推断类型写入 schema；若热重载时 inferred type 改变，按普通 schema migration 规则处理，不能静默重解释内存。

### 38.6 Capability 与 Native：默认推导，不把安全机制变成样板代码

Native surface 默认由 Rust derive/schema 或 generated interface 提供；普通用户不应手写 `native fn/action/task` 声明来连接每个 Rust API。

Capability 从实际 native call graph 推导，并与 package/profile grant 比较。`requires { ... }` 只作为 public API 的显式 contract/assertion，而不是每个函数都必须重复的 ceremony。

### 38.7 游戏支持必须早于“完整通用类型系统”

Game support 是 Viso 的一等 vertical slice，不等待 user-defined Trait/Impl/Const Generic 完成。

MVP 只要求 compiler 能消费 Native Schema 已定义的 scheduler traits：

```viso
system PlayerController implements FixedUpdate {
    input world: Handle<GameWorld>;
    state speed = 6.0f32;

    action fixed_update(frame: FixedFrame) {
        world.walk(player, frame.input.move_x * speed, frame.input.move_z * speed);
    }
}
```

第三方 Rust crate 可以通过 schema 提供新的 system traits。**用户定义新 Trait/Impl 是 Advanced Surface，不是 Game Profile 的前置条件。**

同时标准库应提供适合原型的小型 game facade，让几十行 demo 不必先设计完整 ECS/System graph；该 facade 最终 lower/注册到相同 scheduler/runtime，不新增 parser 关键字。

### 38.8 Viso DSL 1.0 的形式化规范与实现范围

Viso DSL 1.0 的形式化规范必须覆盖：Lossless CST→AST→Typed HIR、多执行域 IR、State/Computed/Action/Task 区分、Keyed List、Transactional Hot Reload、Shader Descriptor ABI、Schema/JSON diagnostics、System/Game Profile。

Viso 1.0 不把所有高级 production 都当成首个可运行 vertical slice 的前置条件。语言团队应维护 `Core / Standard / Advanced` feature matrix，并为每个 production 标注 maturity：`stable / preview / reserved`。

## 39. Viso 编译管线

```text
Source
  ↓
Streaming Tokenizer
  ↓
Lossless CST
  ↓
AST
  ↓
Name Resolution / Module Graph
  ↓
Typed HIR
  ↓
┌─────────────────────────────┐
│ UI IR / Binding IR          │
│ Shader IR                   │
│ Optional Script Bytecode    │
└─────────────────────────────┘
```

### 39.1 Lossless CST 必须存在

原因：

- formatter 保留注释；
- rename/refactor 精确；
- incremental parse；
- AI 结构化修改；
- IDE code action；
- hot reload structural diff；
- 一次报告多个错误。

不能只从 token 直接走向 runtime opcode，然后把所有语言工具需求事后补丁式恢复。

### 39.2 Typed HIR

组件接口默认强类型：

```text
component Counter {
    state count: i32 = 0
    input title: String
    output changed(value: i32)
}
```

应在开发期发现：

- 属性拼写；
- 类型不匹配；
- 不存在的 callback；
- 错误枚举；
- shader uniform type mismatch；
- invalid resource；
- read-only property mutation；
- 模块循环依赖。

### 39.3 Dynamic 是 escape hatch

允许：

```text
let payload: dynamic = ...
```

但大型 UI 的常规 property、component schema、event payload 不应默认 dynamic。

---

## 40. Rust Schema Bridge

Rust 组件通过 derive/macro 自动生成 schema：

```rust
#[derive(Component, Reflect)]
pub struct Button {
    #[prop]
    pub text: Text,

    #[style]
    pub style: ButtonStyle,
}
```

生成概念：

```rust
ComponentSchema {
    type_id,
    name,
    properties,
    events,
    slots,
}
```

Viso DSL compiler 依赖 schema，不直接操作 Rust 对象内存布局。

### 40.1 ID 编译

源码：

```text
text
background
primary
my_button
```

开发模式保留字符串用于 diagnostics。

IR/runtime 使用：

```text
PropertyId(u32)
TokenId(u32)
NodeKey(u32/u64)
ComponentTypeId(u32)
```

release hot path 不做字符串属性查找。

---

## 41. 开发运行与 Release AOT

### Dev

保留：

- CST/HIR cache；
- source map；
- debug name；
- schema reflection；
- hot reload metadata；
- inspector hooks。

### Release

默认构建步骤：

```text
.vs source / ui! ViewFragment / component! ComponentDecl
    ↓ build time (same frontend/HIR)
Typed HIR
    ↓
Compact UI IR / Shader blobs
    ↓
embedded asset / generated Rust data
```

启动时：

- 不重新 parse source；
- 不重新 type-check；
- 不需要完整 symbol string table；
- 直接 instantiate compact IR。

---

## 42. Hot Reload

Hot reload 不是“重新执行整段脚本”这么简单，而应成为事务协议：

```text
Compile candidate
    ↓
Validate schema
    ↓
Compute structural diff
    ↓
Prepare hot-reload state preservation
    ↓
Validate shader/resources
    ↓
Atomic commit
    ↓
Targeted dirty propagation
```

失败：

```text
rollback / keep last-good UI
```

### 42.1 必须定义状态迁移

明确：

- component key 不变时 state 是否保留；
- state `i32 -> f64` 能否自动转换；
- 字段删除如何清理；
- child reorder 如何保持 identity；
- input focus 是否保持；
- scroll position 是否保持；
- animation 如何继续；
- shader compile failure 是否保持 last-good pipeline；
- effect 是否重新执行。

### 42.2 Stable key

列表必须推荐/要求：

```text
for item in items key item.id {
    ...
}
```

无 key 的动态列表在 strict mode 给 warning。

---

## 43. Viso 模块系统

支持：

```text
module app.home;
import widgets::{Button, Label, Column};
import theme::AppTheme;
export component Home { ... }
```

编译器负责：

- module graph；
- topo sort；
- cycle detection；
- public/private；
- resource namespace；
- shader namespace；
- incremental module rebuild。

不再让业务开发者靠手工注册调用顺序保证 UI 能启动。

---

## 44. Capability-based dynamic scripting

如果保留完整 VM，必须是 capability model：

```text
capabilities {
    ui
    timer
    network("api.example.com")
    asset_read("assets/**")
}
```

用于：

- AI 生成预览；
- 第三方插件；
- Studio sandbox；
- 用户脚本。

VM 的 instruction/time budget 仍保留，但不替代 capability security。

---

# Part XV — Widget 与 App Kit

## 45. 一个 `viso-widgets`，内部模块化

不一开始拆：

```text
viso-widgets-core
viso-widgets-app
viso-widgets-adaptive
```

先在单一 crate 中：

```text
widgets/src/
├── lib.rs
├── controls/
├── containers/
├── navigation/
├── overlays/
├── adaptive/
├── desktop/
└── theme/
```

### 45.1 controls

```text
button
label
text_input
checkbox
radio
toggle
slider
dropdown
image
icon
progress
```

### 45.2 containers

```text
row
column
flex
grid
stack
scroll
list
splitter
```

### 45.3 navigation

```text
page
router_view
navigation_stack
tabs
tab_bar
```

### 45.4 overlays

```text
popup
modal
tooltip
toast
sheet
menu
```

### 45.5 adaptive

```text
safe_area
keyboard_avoiding
adaptive_split
adaptive_navigation
responsive_grid
```

### 45.6 desktop

```text
window_shell
menu_bar
dock
tabs
resize_region
```

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
- 有明确 DirtyMask；
- TextInput 使用 text subsystem；
- List 必须 virtualized；
- animation 不默认引发布局；
- accessibility 信息完整。

复杂 widget 的目录按“子系统”拆，不按“一类型一文件”机械拆。

例如：

```text
text_input/
├── mod.rs
├── edit.rs
├── selection.rs
├── ime.rs
├── layout.rs
├── paint.rs
└── semantics.rs
```

简单 Button 保持：

```text
button.rs
```

---

# Part XVI — Platform 与 Services

## 48. Platform 保持窄

初始目录：

```text
crates/platform/src/
├── lib.rs
├── event.rs
├── window.rs
├── surface.rs
├── cursor.rs
├── clipboard.rs
├── lifecycle.rs
├── handles.rs
└── os/
    ├── macos/
    ├── ios/
    ├── windows/
    ├── linux/
    ├── android/
    └── web/
```

平台 backend 初期不因“架构图漂亮”立刻拆成 6 个 crate。

当某 backend 依赖、构建或维护成本足够大时再拆。

### 48.1 Backend 支持等级与 capability contract

Viso 不把“跨平台”理解成所有设备都走同一个最低能力后端。Viso 1.0 定义以下主路径：

| Platform | Tier-1 backend | 目标 |
|---|---|---|
| macOS / iOS | Metal | first-class / performance baseline |
| Windows | D3D12 | first-class / performance baseline |
| Linux | Vulkan | first-class |
| Android | Vulkan | first-class；设备能力不足时由兼容策略处理 |
| Web | WebGPU | first-class |

Tier-2 compatibility backend 可以存在，例如 Linux OpenGL、Android GLES 或 Web compatibility renderer，但遵循三条规则：

1. 不得降低 Tier-1 renderer/RHI 的能力模型；
2. 允许声明 reduced capability profile，并由 tooling 给出清晰诊断；
3. 是否投入 Tier-2 由真实 adoption/device telemetry、维护成本和 benchmark 决定，而不是为了纸面“全覆盖”提前背负多个后端。

因此 Viso 的长期策略是 **WebGPU-first，不把 public architecture 写死成 WebGPU-only**；Linux 同理是 Vulkan-first，而不是永远禁止兼容后端。

### 48.2 Platform 不负责

- Widget；
- Viso DSL compiler；
- app navigation；
- business networking；
- Studio protocol；
- high-level media UI；
- renderer batching。

---

## 49. Services 层

业务能力统一通过 service protocol：

```rust
cx.services().clipboard().set_text("hello")?;
let file = cx.services().files().open(...).await?;
cx.services().share().text("hello").await?;
```

典型 service：

```text
permissions
clipboard
files
share
camera
location
notifications
secure_storage
haptics
network
media
```

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

Route 默认 Rust：

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

Viso 1.0 的命令组：

```text
PROJECT
    viso new
    viso doctor
    viso config

ENVIRONMENT
    viso target list|install|info
    viso device list|info|boot|logs

DEVELOP
    viso run [target]
    viso build [target]
    viso serve [web-target]

LANGUAGE
    viso fmt
    viso check
    viso schema
    viso explain
    viso dump ast|hir|ui-ir|reactive-ir|shader-ir|system-ir
    viso lsp

TEST / DEBUG
    viso test
    viso snapshot
    viso inspect
    viso profile
    viso studio

DELIVERY
    viso package [target]
    viso export html|solid

MAINTENANCE
    viso clean
    viso completion
```

命令名必须描述 Viso 自己的产品语义，禁止把平台工具的历史命令结构直接暴露给用户。

### 54.2 Build target 与 export target 分离

Runtime/build target：

```text
host
macos
windows
linux
ios
android
web-gpu
web-dom
web-hybrid
headless
```

`build` 产生仍由 Viso runtime/生成 runtime 负责的 artifact；`package` 产生可分发产物；`export` 产生可脱离 Viso 工程继续维护的外部生态源码/静态资产。

因此：

```text
viso build web-dom        # Viso Web DOM runtime
viso build web-hybrid     # DOM + WebGPU islands
viso package android      # distributable Android artifact
viso export html          # HTML/CSS/JS export
viso export solid         # SolidJS source export
```

SolidJS 是 exporter，不是 Viso 的中间表示或核心依赖。

### 54.3 `run` 拥有开发期 watcher

普通开发只需要：

```text
viso run
```

它负责：

- Rust incremental build；
- `.vs` incremental compile；
- shader/asset watcher；
- transactional hot reload；
- Last-good runtime；
- target install/launch；
- structured diagnostics；
- optional Inspector/Profile attachment。

不要求普通用户额外运行独立 `watch` 命令。

### 54.4 Machine-readable output 是正式协议

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

### 54.5 CLI 不复制业务实现

目标结构：

```text
                     shared tooling services
                  /        |        |        \
              viso CLI   Studio    IDE/LSP    CI
                  |          |         |        |
                  +----------+---------+--------+
                             |
              compiler / build / inspect / package APIs
```

CLI command handler 只负责：

- 参数解析；
- project/config resolution；
- 调用 domain service；
- 人类/JSON 输出；
- exit code。

不得在 `tools/cli` 内重新实现 DSL compiler、GPU compiler、packager、device protocol 或 Inspector runtime。

### 54.6 CLI 非目标

`viso` 不成为：

- Cargo/crates.io dependency manager 的替代品；
- Git 替代品；
- 通用 shell task runner；
- Docker/CI 平台；
- 数据库 schema 管理工具；
- arbitrary package manager。

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
Node 381 marked LAYOUT dirty
because:
  property Text changed
  from state App.feed.items[3].title
  at src/features/feed/view.vs:82
```

这是复杂增量系统可维护的关键。

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

---

# Part XXV — Viso 1.0 实施路线图

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
10. 固定 renderer primitive contract；
11. 固定 GPU instance ABI；
12. 建立 benchmark 与 characterization suite；
13. 建立 `architecture.toml` + `cargo xtask arch-check`；
14. 建立 dependency/unsafe/perf CI gates。

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
8. explicit `GpuInstance` descriptor；
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

公开 API 使用：

```text
Auto
Px
Percent
Fr
Min/Max
Row
Column
Stack
Grid
Absolute
```

内部算法可使用单遍 cursor、缓存 measure、局部 relayout 等优化，但这些实现细节不泄漏成用户正确性要求。

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
3. `DirtyMask`；
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
→ TRANSFORM | HIT_TEST | PAINT

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
- `viso target` / `viso device`；
- `viso run` / `viso build` / `viso serve`；
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

# Part XXVI — Repository 详细编排

## 74. Framework repo 最终建议

```text
viso/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── ARCHITECTURE.md
├── Viso_CLI.md
├── AGENTS.md
├── SECURITY.md
├── rustfmt.toml
│
├── crates/
│   ├── viso/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       └── prelude.rs
│   │
│   ├── macros/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── component.rs
│   │       ├── binding.rs
│   │       └── gpu_instance.rs
│   │
│   ├── ende/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── encode.rs
│   │       ├── decode.rs
│   │       ├── error.rs
│   │       ├── bin/
│   │       └── json/
│   │
│   ├── math/
│   │   ├── Cargo.toml              # package = "viso-math"
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── scalar.rs
│   │       ├── vector.rs
│   │       ├── matrix.rs
│   │       ├── geometry.rs
│   │       └── transform.rs
│   │
│   ├── runtime/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── app.rs
│   │       ├── loop.rs
│   │       ├── frame.rs
│   │       ├── scheduler.rs
│   │       ├── task/
│   │       ├── timer.rs
│   │       ├── mailbox.rs
│   │       └── resource.rs
│   │
│   ├── platform/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── event.rs
│   │       ├── window.rs
│   │       ├── surface.rs
│   │       ├── lifecycle.rs
│   │       ├── handles.rs
│   │       └── os/
│   │           ├── macos/
│   │           ├── ios/
│   │           ├── windows/
│   │           ├── linux/
│   │           ├── android/
│   │           ├── web/
│   │           └── headless/
│   │
│   ├── gpu/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── device.rs
│   │       ├── resource.rs
│   │       ├── pipeline.rs
│   │       ├── command.rs
│   │       ├── surface.rs
│   │       ├── caps.rs
│   │       └── backend/
│   │
│   ├── shader/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── syntax/
│   │       ├── hir/
│   │       ├── ir/
│   │       ├── validate/
│   │       └── codegen/
│   │
│   ├── text/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── font_db.rs
│   │       ├── fallback.rs
│   │       ├── shaping.rs
│   │       ├── bidi.rs
│   │       ├── line_break.rs
│   │       ├── paragraph.rs
│   │       └── cache.rs
│   │
│   ├── render/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── primitive.rs
│   │       ├── paint_cache.rs
│   │       ├── clip.rs
│   │       ├── layer.rs
│   │       ├── batch.rs
│   │       ├── atlas/
│   │       ├── graph/
│   │       └── frame_packet.rs
│   │
│   ├── ui/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── node/
│   │       ├── component/
│   │       ├── state/
│   │       ├── layout/
│   │       ├── input/
│   │       ├── focus/
│   │       ├── gesture/
│   │       ├── style/
│   │       ├── animation/
│   │       ├── semantics/
│   │       └── paint/
│   │
│   ├── widgets/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── controls/
│   │       ├── containers/
│   │       ├── navigation/
│   │       ├── overlays/
│   │       ├── adaptive/
│   │       ├── desktop/
│   │       └── theme/
│   │
│   ├── dsl/
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── syntax/
│   │       ├── cst/
│   │       ├── ast/
│   │       ├── module/
│   │       ├── hir/
│   │       ├── ui_ir/
│   │       ├── binding/
│   │       ├── reload/
│   │       └── vm/
│   │
│   └── services/
│       └── src/
│           ├── lib.rs
│           ├── permissions.rs
│           ├── files.rs
│           ├── clipboard.rs
│           ├── share.rs
│           ├── notifications.rs
│           ├── camera.rs
│           ├── location.rs
│           ├── storage.rs
│           └── network.rs
│
├── integrations/
│   ├── tokio/
│   ├── tracing/
│   ├── serde/
│   ├── accesskit/
│   └── robius/
│
├── extras/
│   ├── code-editor/
│   ├── markdown/
│   ├── pdf/
│   ├── browser/
│   ├── charts/
│   ├── map/
│   └── xr/
│
├── tools/
│   ├── cli/
│   ├── inspector/
│   ├── studio/
│   └── packager/
│
├── examples/
│   ├── 00-minimal/
│   ├── 01-counter/
│   ├── 02-layout/
│   ├── 03-state/
│   ├── 04-navigation/
│   ├── 05-async/
│   ├── 06-list/
│   ├── 07-text-input/
│   ├── 08-adaptive/
│   ├── 09-accessibility/
│   ├── 10-custom-shader/
│   └── 99-full-app/
│
├── benches/
├── tests/
├── docs/
├── xtask/
└── vendor/
```

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

全部重新塞进一个模糊 `libs/`。

---

# Part XXVII — Public API 细化建议

## 77. `run::<App>()`

签名概念：

```rust
pub fn run<A: Application>() -> !;
```

Web 等平台可能不是传统 `!`，可以内部平台化，public surface 保持统一。

Application：

```rust
pub trait Application: Sized + 'static {
    fn new(cx: &mut AppCx<'_>) -> Self;

    fn resumed(&mut self, _cx: &mut AppCx<'_>) {}
    fn suspended(&mut self, _cx: &mut AppCx<'_>) {}
    fn low_memory(&mut self, _cx: &mut AppCx<'_>) {}
}
```

事件不一定全部由 `Application` 手工接收；UI runtime 自己 route。

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
    type Instance: GpuInstance;

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

## ADR-001：单一 facade crate

**决定**：普通用户只依赖 `viso`。  
**原因**：隐藏内部拆分，降低 onboarding 和 API churn。  
**代价**：facade 需要谨慎维护 re-export 与 features。

## ADR-002：Retained Tree，不采用 Virtual DOM

**决定**：状态变化直接 invalidation retained node/property。  
**原因**：更符合 Viso 的性能定位，避免通用 rebuild/diff 成本。  
**代价**：reactive dependency、structure mutation 和 hot reload 实现更复杂。

## ADR-003：NodeArena + generational ID

**决定**：Node identity 使用 arena ID。  
**原因**：局部性、低分配、稳定 handle、避免 RefCell。  
**代价**：需要明确 storage/lifetime/handle validity。

## ADR-004：UI Tree 不做全 ECS

**决定**：保留 parent/child UI tree，热数据采用 DOD。  
**原因**：UI 的 ancestry 语义强，纯 ECS 会让 layout/focus/semantics 复杂化。  
**代价**：需要设计好 tree + side arrays。

## ADR-005：Viso DSL 不作为 UI runtime 基础依赖

**决定**：`dsl -> ui`，不是 `ui -> dsl`。  
**原因**：纯 Rust、AOT、测试、可维护性。  
**代价**：需要 schema/IR bridge。

## ADR-006：Release AOT

**决定**：默认 release 不 parse `.vs`。  
**原因**：启动、内存、性能、错误提前发现。  
**代价**：build pipeline 更复杂。

## ADR-007：GPU backend 静态选择

**决定**：源代码统一 RHI，target release 尽量单 backend specialization。  
**原因**：避免热路径 dyn dispatch 和最低公分母。  
**代价**：backend 维护成本高于完全依赖通用抽象。

## ADR-008：自动 batching

**决定**：batch 是 renderer 责任。  
**原因**：用户不应该靠 `new_batch` 维持正确性。  
**代价**：renderer 需要更强 z-order/clip/layer 模型。

## ADR-009：官方 async protocol，不强绑 Tokio

**决定**：Viso main loop 拥有 scheduling；Tokio/Smol 为 adapter。  
**原因**：控制 frame/vsync/lifecycle，减少 feature matrix。  
**代价**：需要维护小型 task bridge/runtime。

## ADR-010：Progressive project structure

**决定**：feature 从 `mod.rs + view.vs` 起步，复杂再拆。  
**原因**：同时适配小/大项目。  
**代价**：团队需要约定何时拆文件。

---

## ADR-011：Node hot storage 使用 hybrid indexed SoA

**决定**：compact `NodeMeta` AoS + 同索引 hot SoA stores + sparse cold tables。

**理由**：兼顾树遍历 locality、直接 index、可选数据空间效率和实现可控性；避免 pure object graph 与 archetype migration 的复杂度。

**代价**：store schema 演进需要更严格的 memory accounting；部分 node 会在 hot store 中保留 sentinel/compact slot。

## ADR-012：`Handle<T>` 不直接暴露组件内部 state 借用

**决定**：Handle 表达 identity/capability；状态读写通过 context-scoped query、typed property/action/update API。

**理由**：保持 lifetime、reactive tracking、hot reload state preservation 与 invalidation 的可验证性。

**代价**：某些内部高级代码比直接 `&mut T` 多一层显式 API。

## ADR-013：Transform 与 Layout 使用独立失效平面

**决定**：共享 `NodeId`，独立 TransformStore/dirty propagation；scroll/transform animation 默认不触发布局。

**理由**：保证高频滚动/动画走窄路径。

**代价**：hit-test、clip、world transform cache 需要明确同步规则。

## ADR-014：自研范围使用 Ownership Ladder

**决定**：UI runtime/reactive/render integration/DSL/HIR/Shader ABI 等核心语义自研；标准算法和通用基础设施优先依赖成熟实现并通过窄边界隔离。

**理由**：把有限工程资源集中在 Viso 的性能和产品差异化上。

**代价**：需要持续维护依赖评估、替换层和版本兼容测试。

## ADR-015：Viso DSL 使用 `ui!` / `view!` / `component!` 与 `.vs` 文件

**决定**：`ui!` 使用 ViewFragment parser entry，`component!` 使用 ComponentDecl entry，`view!("...vs")` 使用外部 `.vs` CompilationUnit；三者共享同一 schema/type/effect checker、Typed HIR、Reactive/UI/Shader IR 和 runtime contracts。`.vs` 是唯一 canonical 外部文件扩展名。

**理由**：宏名直接表达作者意图，避免 `ui!` / `component!` 同时承担 fragment/component/file 的语义模糊；小型 Rust-native 使用场景保持零文件跳转，大型 UI/设计系统获得独立语言服务与 hot reload。

**代价**：compiler/source-map 必须支持 Rust macro span 与 file span 两种 source origin，并维护少量明确 parser entry points。

## ADR-016：Identity 使用 Stable Symbol + Typed Dense Runtime ID

**决定**：compiler-local 名字使用 `NameId`，跨编译稳定身份使用 128-bit `SymbolId`，运行时热路径 lower 为 `PropertyId` / `EventId` / `ComponentTypeId` / `ShaderId` 等 typed dense ID；具体 UI 实例使用 generational `NodeId`。

**理由**：稳定身份与执行身份具有不同优化目标；前者服务 hot reload/schema/cache/source mapping，后者服务 direct-index hot path。

**代价**：需要显式 link/lowering 阶段和更多 typed newtype，但这些类型在 release 中没有额外抽象成本。

## ADR-017：Viso 自研 `viso-ende`

**决定**：Viso 内部协议、cache、snapshot、tool transport 使用 Viso-owned Ende Binary/JSON contract；RON 不属于 Viso；Serde 只作为生态 integration。

**理由**：协议 ABI、分配行为、bounded decode、typed ID wire representation 与工具链版本语义属于框架架构。

**代价**：需要维护 derive、binary/json encoder/decoder、fuzz 与 compatibility tests。

## ADR-018：基础数值与几何使用独立 `viso-math`

**决定**：`viso-math` 提供 allocation-free vector/matrix/rect/size/transform/geometry primitives，并位于 Runtime/GPU/Render/UI 下层。Math ABI 不等于 GPU ABI。

**理由**：这些类型横跨 layout、input、render、shader interface、text geometry、animation 与 game；不应被某个上层 subsystem 所有。

**代价**：需要严格控制 crate scope，防止 `math` 退化成新的 `utils/core` 垃圾桶。

## ADR-019：`viso` CLI 是统一 Tooling Facade

**决定**：项目创建、环境诊断、target/device、build/run/serve、language tooling、test/inspect/profile、package/export 统一通过 `viso`；CLI、Studio、IDE、CI 复用同一套 tooling services。

**理由**：减少平台命令碎片，给人类和 AI 提供一致入口，并防止 Studio/CLI 分叉出重复 build/compiler 逻辑。

**代价**：需要稳定 command grammar、Ende JSON machine protocol、exit codes 和 target/device abstraction。


# Part XXIX — 风险与取舍

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

- Dev：IR 来自增量 compiler；
- Release：IR 来自 build-time compiler。

不是维护两套 UI runtime。

---

## 84. 风险：crate 太少导致编译慢/边界不够硬

当前建议十余个 crate 是起点，不是永远不拆。

拆分依据必须是测量/依赖边界，而不是目录美学。

边界不能只靠人工复盘。Viso 1.0 要求 CI 每次运行 `cargo xtask arch-check`；周期性架构复盘只负责评估是否需要拆 crate。

持续检查：

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
    ├── STYLE
    ├── MEASURE
    ├── LAYOUT
    ├── TRANSFORM
    ├── PAINT
    ├── HIT_TEST
    └── SEMANTICS
    ↓
Incremental UI phases
    ↓
Paint Cache Changes
    ↓
Primitive Ranges
    ↓
Batch Builder
    ↓
Frame Packet
    ↓
viso-gpu
    ↓
Metal / D3D12 / Vulkan / WebGPU
```

---

# Appendix B — State 更新完整示例

用户：

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
propagate layout only to nearest intrinsic-size ancestor

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

建议只包含稳定高频项：

```rust
pub mod prelude {
    pub use crate::{run, Application};
    pub use viso_ui::{
        AppCx,
        Component,
        Handle,
        State,
        Computed,
        View,
    };
    pub use viso_widgets::{
        Window,
        Row,
        Column,
        Stack,
        Scroll,
        List,
        Label,
        Button,
        TextInput,
        Image,
    };
    pub use viso_math::{Rect, Size, Vec2};
    pub use viso_render::Color;
    pub use viso_macros::{component, ui, view, routes};
}
```

不要把所有 renderer/GPU/platform types re-export 进 prelude。

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

1. Node hot storage：**hybrid indexed SoA**（ADR-011）；
2. `Handle<T>`：**不直接暴露跨帧 typed state borrow/mutation**（ADR-012）；
3. Transform：**与 Layout 使用独立失效平面，共享 NodeId**（ADR-013）；
4. 自研/依赖：**使用 Ownership Ladder**（ADR-014）；
5. DSL source form：**`ui!` / `component!` / `view!("...vs")` 共享 schema/HIR/IR/runtime，`.vs` 为唯一 canonical 外部格式**（ADR-015）；
6. Identity：**128-bit Stable `SymbolId` + typed dense runtime IDs + generational `NodeId`**（ADR-016）；
7. Ende：**Viso-owned Binary/JSON，RON 不进入 Viso core，Serde 只做 integration**（ADR-017）；
8. Math：**独立 `viso-math`，allocation-free 基础数值/几何 ABI，Math ABI 与 GPU ABI 分离**（ADR-018）；
9. CLI：**`viso` 是统一 Tooling Facade，CLI/Studio/IDE/CI 共用 tooling services**（ADR-019）。

这些决定应尽早被 prototype/benchmark 验证，但验证的默认动作是调整实现参数，而不是重新打开核心语义。若要推翻，必须新 ADR。

## G.2 仍需 prototype/benchmark 的问题

1. clip cache 的最佳结构；
2. text shaping cache key 与跨 paragraph 共享粒度；
3. GPU instance buffer 是 per-type persistent pool 还是 frame ring + retained ranges 混合；
4. Tier-2 Web compatibility backend（例如 reduced-profile fallback）是否值得长期维护；
5. Linux/OpenGL 与 Android/GLES compatibility backend 的投入时机；
6. dynamic scripting VM 的保留范围；
7. platform backend 何时从 module 升格独立 crate；
8. AccessKit adapter 能覆盖多少目标平台/能力缺口；Viso semantics tree 本身已确定为 canonical model；
9. Rust declarative API 除 `ui!` / `component!` / `view!` 外是否还需要 builder/function syntax；
10. 3D scene graph 放 render、extras 还是独立 crate；
11. paragraph cache 是否跨 component 共享以及内存上限；
12. GPU retained instance range 的碎片整理策略。

原则：**先确定不可妥协的性能/边界语义，再通过 benchmark 和实现经验决定具体容器、cache 和兼容层级。**

---

# Appendix H — 来源与设计依据

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

Viso 最重要的不是“目录变得漂亮”，而是同时建立三层稳定契约：

### 对用户

```rust
use viso::prelude::*;

fn main() {
    viso::run::<App>();
}
```

少概念、少样板、默认正确。

### 对框架维护者

```text
runtime
platform
gpu
shader
text
render
ui
widgets
dsl
services
```

依赖边界明确、职责可测试、工具链可观测。

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

只要后续所有架构决策都能通过这四条和可重复 benchmark 检验，Viso 就可以同时拥有极简开发体验、清晰工程结构和非常高的性能上限。
