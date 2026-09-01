# Viso 架构设计与重构迁移方案

> 文档状态：Draft / Architecture Baseline 1.0  
> 目标读者：Viso 核心维护者、负责 Makepad→Viso 迁移的工程师、Renderer/GPU 工程师、UI/Widget 工程师、工具链工程师、AI Coding Agent  
> 基线日期：2026-08-31  
> 适用范围：假设可以从现有 Makepad 重新设计一代框架，不以兼容成本和工作量为第一约束，但要求提供可执行迁移路径。

---

## 0. 文档目的

本文不是对现有 Makepad 做“小步整理”的计划，而是一份 **Viso** 的目标架构与迁移设计。

设计同时优化三个看似冲突、实际上可以兼容的目标：

1. **性能优先**：保持并扩大 Viso 的 GPU-first、Rust-native、低开销优势；稳态热路径以 Data-Oriented、增量更新、低分配、低间接层为核心。
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

本设计中的新框架正式命名为 **Viso**。`Makepad` 仅用于描述迁移来源、旧 API、旧行为和性能基线，不是 Viso 的运行时兼容目标。

Viso UI/DSL 源文件的唯一规范扩展名统一为 **`.vs`**：

```text
app.vs
theme.vs
features/home/view.vs
```


Makepad 的当前迁移来源是 Rust 源码中的 `script_mod!` 宏、`ScriptVm` 初始化/执行路径以及相关脚本模块注册关系，而不是 `.live` 文件。Viso 的 DSL 输出格式只使用 `.vs`。`Live` 只描述 live editing / hot reload 能力，不作为 Viso 的语言、crate、目录或文件格式名称。

---

# Part I — 总体判断与设计原则

## 1. 当前 Makepad 值得保留的核心

Viso 不应该变成一个“更像 Web 框架的 Rust UI”，也不应该复制 Slint、Flutter 或 React。应保留 Makepad 已经形成辨识度的核心方向：

- Rust-native application runtime；
- GPU-first 2D/3D rendering；
- 自绘 UI，而不是平台原生控件拼装；
- 跨 macOS / Windows / Linux / iOS / Android / Web；
- 基于 `script_mod!` / `ScriptVm` 的运行时可编辑 UI 与脚本能力；
- Shader 与 UI 深度结合，同时允许高级用户下钻到底层；
- Studio / Inspector / AI 辅助开发能力；
- 框架自己掌握关键渲染链路，而不是被浏览器、DOM 或通用 Widget toolkit 限制。

当前 Makepad 官方 README 已将自身描述为 Rust-first、跨平台、高性能 UI runtime、live-editable design language、Studio 与 AI-accelerated workflow 的组合。当前 dev 分支也已经从旧 `live_design!` 迁移到以 `script_mod!` 为中心的运行时脚本方向。Viso 的目标不是否定这些方向，而是把它们重新放入更清晰的边界中。

### 1.1 当前结构中值得重构的信号

当前代码中存在一些典型的“边界混合”信号：

- `Widget` 同时参与事件、绘制、脚本、动态类型与树操作；
- `WidgetRef` 当前以 `Rc<RefCell<Option<...>>>` 包装动态 Widget；
- `platform` crate 直接依赖脚本、network、live reload 等能力，职责超出纯平台抽象；
- 当前 `script_mod!` / `ScriptVm` UI 脚本体系仍存在手工 `render()`、`new_batch`、部分 `Fit/Fill` 约束等作者需要理解的实现细节；
- repo workspace 同时承载 framework、Studio、examples、game、vendored 依赖和多类实验能力，物理结构没有完整表达依赖层次。

这些都不是“某个文件写坏了”，而是长期迭代后自然出现的架构耦合。Viso 应在保留性能和 live-editing 能力的前提下，把这些耦合重新组织。

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
11. 迁移工具和兼容层有明确退出时间。
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

以及必要宏：

```text
component!
ui!
view!
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
├── AGENTS.md
├── rustfmt.toml
│
├── crates/
│   ├── viso/
│   ├── macros/
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
- state/binding metadata；
- shader instance layout；
- Viso DSL schema；
- static template 生成；
- compile-time diagnostics。

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
- state migration metadata；
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

### 10.2 允许的反向通知

当底层需要向上层通知时，使用：

- typed event；
- small callback protocol；
- registered handler ID；
- queue/mailbox；
- compile-time generic；

而不是直接引入上层 crate。

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

Viso 自己拥有：

- task ID；
- wakeup queue；
- cancellation token；
- main-thread dispatch；
- timer source；
- worker pool（默认实现可很小）。

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

> **As built (Slice B) — 见 ADR 0005。** `BindingTable` 落地为混合两条路径：
> 静态边按 `StateId` 存为 dense contiguous run，并做 same-node class folding
> （一个 cell 变化对每个受影响节点只 dirty 一次）；动态脚本走 `bind_dynamic`
> 运行时回退（§10.3）。`Computed` **不**走 `bind_dynamic`——binding flush 会
> 无条件 dirty 每条动态边，绕过 memo 边界——而是自持反向索引唤醒（见 §21 注）。

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

> **As built (Slice B) — 见 ADR 0005。** 实际落地为 `DirtyClass`：`u8` 位集，
> 恰好八个类（此草图写的是 `DirtyMask: u16`，多出的位未使用，故收敛为 u8）。
> 类的语义与顺序不变，一位一义。`Computed`/`Effect` 刻意**不**占用其中的位
> （见 §21 注），保持八位干净。

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

> **As built (Slice B) — 见 ADR 0005。** `Computed` 与 `Effect` 各自持有一条
> `StateId → Vec<Id>` 反向索引（**不**是 dirty 位集）来唤醒，二者唤醒方式因
> 输出不同而不同：`Computed` 的输出**就是**节点的一个 dirty class，
> `ComputedStore::wake_computed` 重算受影响的派生，**仅当派生值真的变化**时才
> `mark_dirty` 下游节点——memo 边界落在唤醒本身；`Effect` 无 dirty class，
> 其输出是副作用，`EffectStore::wake` 直接重跑 body（依赖变化时先跑上一次
> cleanup 再跑 body），`cancel`/`cancel_for_node` 在 unmount 时跑 cleanup。
> 每次 eval/run 通过 `deindex → 刷新 deps → reindex` 维护反向索引，故停止读取
> 某 cell 的派生/effect 会停止被它唤醒。三个 reactor 在帧的
> `FlushStateTransactions` 阶段按 wake_computed → 静态/动态 binding flush →
> effects.wake 顺序运行，共享同一次 drain 的 `changed` 集合。
> 已知后续（超出本 slice）：`StateId` slot 复用时唤醒的 generation 检查；
> Computed 依赖 Computed 的级联。

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

### 22.2 保留 Turtle 的性能思想

不建议简单删除 Makepad 现有顺序布局/Turtle 思路。

对于 Row/Column/Flow，内部仍可使用：

```text
single-pass cursor
remaining space
fit/fill resolution
alignment pass
```

但不把 `Turtle`、特殊 batch、复杂 Fit/Fill 约定作为普通用户必须理解的 API。

> **隐藏 Turtle，而不是删除它在热路径上的价值。**

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

不要仅以“上一帧算过”作为 cache 条件。

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

### 34.1 Layer 裁剪与 offscreen 合成

`Layer` 有两条互斥路径，由 `LayerClip.opacity` 决定：

- **`opacity == 1`（矩形裁剪）**：`Layer..LayerEnd` 子树在**主 pass 内**用硬件
  scissor 裁到 layer rect，不创建任何 offscreen 目标。绝大多数 Layer 走此路径，
  零额外 pass / 零额外纹理。
- **`opacity < 1`（offscreen 合成）**：子树先渲染进一张 offscreen 纹理
  （`RenderTarget::Texture`，clear 成透明 `[0,0,0,0]`），再在主 pass 用一个
  composite 段把该纹理以 layer rect 为目标矩形整幅采样贴回，tint = `(1,1,1,opacity)`
  （premultiplied over-blend，故整层按 opacity 混合）。composite 复用 `Image`
  pipeline / sampler，不引入新 shader。

`DrawList.passes` 顺序恒为 **offscreen 全部在前、主 pass 在后**。offscreen pass 的
viewport = layer rect 尺寸；该区间内图元的坐标在 lowering 时减去 layer rect 原点，
使 layer 左上角映射到纹理 `(0,0)`，因此 offscreen 与主 pass 共用同一套 pixel→NDC
约定，无需 Y 翻转。offscreen 纹理按尺寸池化复用（稳态不新增分配）。

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

### 38.1 默认语言职责

`.vs` 默认负责：

- UI structure；
- typed properties；
- style/theme；
- binding；
- event declaration；
- small pure expressions；
- animation；
- shader declaration/引用。

通用动态脚本能力可以保留，但作为明确的扩展层，而不是所有 UI 节点的默认执行模型。

---

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
.vs source
    ↓ build time
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
Prepare state migration
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
- shader compile failure 是否保持旧 pipeline；
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

### 48.1 Platform 不负责

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

## 53. 文件类型

默认只要求：

```text
.rs
.vs
```

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

长期目标：

```text
viso new
viso doctor
viso run
viso run ios
viso run android
viso inspect
viso profile
viso snapshot
viso test-ui
viso migrate
viso package
```

CLI 是 facade，不应要求用户记底层 tool crate 名。

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
- state migration；
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
paint_10k_quads
batch_10k_quads
hot_reload_small_component
hot_reload_large_module
startup_minimal
startup_complex_app
memory_10k_nodes
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

# Part XXIV — 从现有 Makepad 到 Viso 的迁移总策略

## 63. 迁移基本原则

Viso 是 clean-slate framework。迁移目标不是让旧 Makepad runtime 能“寄生”在 Viso 中，而是把已经验证过的行为、算法、平台经验和性能特征迁移到新的架构模型。

最重要的迁移原则是：

> **Migrate semantics, not architecture.**
>
> **迁移能力、行为、算法、测试和性能基线，不迁移旧运行时抽象。**

因此 Viso 生产运行时明确禁止：

- Makepad `WidgetRef` runtime wrapper；
- 在 Viso Node tree 中托管旧 Makepad Widget；
- 让旧 `Cx` / `Walk` / `DrawStep` / `Event` 生命周期继续存在于 Viso runtime；
- 为兼容旧 Makepad 而在 `viso-ui`、`viso-render`、`viso-runtime`、`viso-dsl` 中增加 legacy feature flag；
- 双 UI runtime 长期共存。

迁移支持只允许出现在：

```text
tools/migrate/
docs/migration/
tests/migration/
fixtures/makepad/
```

它们可以理解当前 Makepad Rust 源码中的 `script_mod!`、`ScriptVm`、脚本模块注册关系和相关 UI 脚本语义，但不得成为 Viso runtime 的依赖。

### 63.1 正确的迁移边界

```text
Existing Makepad Project
          │
          ▼
     viso migrate
          │
          ├── Rust source analysis
          ├── script_mod! macro analysis
          ├── ScriptVm / module-registration analysis
          ├── widget/API mapping
          ├── asset/style rewrite
          └── migration diagnostics
          │
          ▼
      Viso Source
       .rs + .vs
          │
          ▼
      Viso Compiler
          │
          ▼
    Pure Viso Runtime
```

禁止：

```text
Makepad Widget
      X  (no runtime bridge)
Viso NodeArena
```

迁移工具可以认识 Makepad；Viso runtime 不认识 Makepad。

### 63.2 迁移分三类

#### A. 可以复用算法或底层实现思想

典型包括：

- 字体 shaping / fallback / glyph cache；
- shader compiler 中已验证的 backend lowering；
- geometry / tessellation；
- texture atlas；
- image/audio/video decode 中可独立复用的部分；
- OS backend 中稳定的平台调用；
- 高性能滚动、命中测试、批处理中的算法思想。

要求：先定义 Viso 边界和数据模型，再迁实现。不得为了复用代码反向修改 Viso API 去适应旧结构。

#### B. 参考行为并重新实现

典型包括：

```text
Button
Label
TextInput
Scroll
List
Dock
Window
Slider
CheckBox
PortalList
```

旧实现只作为：

- behavior reference；
- UX reference；
- regression reference；
- performance baseline；
- edge-case source。

新实现必须直接落在 Viso 的：

```text
Component
NodeArena
State/Binding
Layout
Input Router
Paint
Semantics
Render Primitive
```

模型上。

#### C. 明确舍弃的旧抽象

默认不迁移：

```text
Rc<RefCell<dyn Widget>> 作为 UI 主身份
WidgetRef / WidgetWeakRef 主路径
万能 Cx
Widget::handle_event 全树分发模型
Widget::draw_walk 生命周期
公开 Walk/Turtle 作者心智模型
显式 new_batch 正确性开关
状态修改后手工 render/redraw 作为默认协议
script_mod! 手工注册顺序
隐式 GPU instance 字段尾部 ABI
```

其中某些**算法思想**可以保留，例如 Turtle 的单遍布局策略；但 public/runtime abstraction 不保留。

---

## 64. 迁移前建立 Characterization Baseline

虽然 Viso 不提供 runtime compatibility，仍然必须对 Makepad 做可测量的行为与性能冻结。迁移是否成功，以这些基线作为参照，而不是以“旧代码是否还能直接运行”为标准。

### 64.1 基准场景

至少准备：

```text
minimal_window
ui_zoo
large_list_10k
large_list_100k
text_stress
code_editor
animated_dashboard
image_gallery
3d_scene
mobile_form
hot_reload_ui
ime_text_input
nested_scroll
multi_window
```

### 64.2 每个场景记录

- cold/warm startup；
- idle CPU；
- CPU frame time；
- GPU frame time；
- input-to-present latency；
- draw calls；
- pipeline switches；
- GPU uploads/frame；
- allocations/frame；
- peak and steady memory；
- layout node count；
- dirty node count；
- text shaping time；
- glyph cache hit rate；
- scroll performance；
- resize performance；
- hot reload latency；
- platform behavior；
- accessibility snapshot。

### 64.3 行为快照

保存：

- screenshots；
- input tapes；
- semantic/accessibility snapshots；
- expected focus order；
- IME sequences；
- scroll/gesture traces；
- shader golden outputs；
- Makepad `script_mod!` / `ScriptVm` parser、evaluation 与 registration diagnostics；
- widget interaction traces。

这些数据用于验证 Viso 行为，而不是驱动 runtime compatibility。

---

# Part XXV — Clean-Slate 重构与源码迁移计划

## 65. Phase 0 — 固定 Viso Architecture Contract

### 目标

在复制任何旧实现前，先锁定 Viso 的目标约束。

### 工作

1. 固定 `viso::run::<App>()` facade；
2. 固定 crate dependency DAG；
3. 固定 hot-path contract；
4. 固定 NodeId/NodeArena identity model；
5. 固定 frame phases；
6. 固定 `.vs` 为唯一 canonical DSL 扩展名；
7. 固定 renderer primitive contract；
8. 固定 GPU instance ABI；
9. 建立 benchmark 与 characterization suite；
10. 建立 dependency/unsafe/perf CI gates。

### 退出标准

- 新架构可以在没有任何 Makepad runtime 类型的情况下编译最小空壳；
- CI 可验证依赖方向；
- baseline 数据已记录；

---

## 66. Phase 1 — 建立 Viso Foundation / Runtime / Platform

### 目标

先得到一个完全独立的 Viso application loop。

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

此阶段不允许通过旧 `AppMain` adapter 实现。

---

## 67. Phase 2 — 建立 Viso GPU RHI 与 Renderer

### 目标

建立与 Makepad 旧 Draw API 无关的新 GPU 与渲染协议。

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

### 迁移方式

可把旧 renderer 的算法、shader lowering、atlas 逻辑作为参考或移植来源，但 Viso public/runtime 类型必须先定义。

禁止让：

```text
DrawQuad
DrawText
DrawVars
new_batch
```

成为新 renderer 的兼容入口。

### 退出标准

- 纯 Viso test scene 可以绘制；
- 稳态 renderer hot path 满足 allocation/dispatch 约束；
- 主要 benchmark 不低于 Makepad baseline 的目标阈值；
- GPU instance layout 有编译期验证。

---

## 68. Phase 3 — 建立 NodeArena 与 UI Tree

### 目标

从第一天使用 Viso 最终 UI identity，不经历旧 Widget host 过渡期。

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
- hot traversal 基于连续/紧凑数据结构；
- 没有任何 Makepad Widget 类型参与。

---

## 69. Phase 4 — Layout / Input / Style / Semantics

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

内部可以保留 Makepad Turtle 类算法中已经证明高效的单遍 cursor 思想，但不保留旧 `Walk/Turtle` public abstraction。

### 退出标准

- Button/Label/Scroll/VirtualList 可完全在新系统运行；
- input dispatch 以 target/capture/bubble/focus 为核心；
- steady-state scrolling 可做到目标 allocation budget；
- semantics 从 Node 模型天然生成。

---

## 70. Phase 5 — Reactive State 与增量失效

### 目标

实现 retained UI + targeted invalidation，而不是 Virtual DOM。

### 工作

1. `StateId` / typed state slot；
2. transaction batching；
3. `DirtyMask`；
4. binding metadata；
5. static dependency fast path；
6. dynamic fallback 仅用于高级 DSL/script；
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

- Counter 修改 state 不需要 `render()`；
- 单属性变化只触发必要 phase；
- transaction 内多次 set 只 flush 一次；
- 依赖图和 dirty reason 可 profile。

---

## 71. Phase 6 — Viso DSL：`.vs`

### 目标

建立 typed、incremental、AOT-friendly 的 Viso DSL。

### 编译管线

```text
.vs source
   ↓
Streaming Tokenizer
   ↓
Lossless CST
   ↓
AST
   ↓
Name Resolution
   ↓
Typed HIR
   ↓
UI IR / Binding IR / Shader IR
   ↓
Dev hot reload or Release AOT package
```

### 语言规则

- `.vs` 是唯一 canonical 扩展名；
- `.vs` 是 Viso 唯一的 DSL 源格式；迁移器不得假设当前 Makepad 项目存在 `.live` 文件；
- component/schema 可静态检查；
- module/import/export 不依赖注册调用顺序；
- view 默认无副作用；
- effect/event 承载副作用；
- state/computed 进入增量依赖图；
- shader 走严格布局验证；
- dynamic scripting 是 escape hatch，不是普通 UI 的默认执行模型。

### Makepad Script 迁移

当前 Makepad 的迁移输入是 Rust 源码内嵌的运行时脚本体系，而不是独立 `.live` 文件：

```text
Current Makepad:
    app.rs
      ├── script_mod! { ... }
      ├── App::run(vm: &mut ScriptVm)
      └── App::from_script_mod(...)

    widget modules
      ├── script_mod! { ... }
      ├── Struct::register_widget(vm)
      └── mod.widgets.* registration
          │
          ▼
      viso migrate
          │
          ├── parse Rust macro token streams
          ├── recover script module definitions
          ├── build ScriptVm registration/dependency graph
          ├── map widget/style/shader semantics
          └── emit migration diagnostics
          │
          ▼
Viso:
    app.rs
    app.vs
    theme.vs
    feature/view.vs
```

迁移器做源码级转换：从 `script_mod!` 宏及 `ScriptVm` 注册/初始化代码恢复当前 Makepad 脚本语义，再生成纯 Viso `.rs + .vs`。Viso runtime 不嵌入 Makepad `ScriptVm`，也不保留 Makepad 脚本运行时兼容层。

### 退出标准

- formatter/LSP/goto/rename/reference 可用；
- release 不需要启动时 parse `.vs`；
- hot reload 是 compile → validate → atomic patch；
- 新版本失败保留 last-good UI；
- state/focus/scroll migration 有明确规则。

---

## 72. Phase 7 — 官方 Widgets 原生重写

### 原则

每个 widget 都直接基于 Viso 模型实现，不 wrap Makepad widget。

优先级：

```text
Tier 1: View/Container, Label, Image, Icon
Tier 2: Button, CheckBox, Toggle, Radio, Slider, TextInput
Tier 3: Scroll, VirtualList, Grid, Splitter
Tier 4: Window, NavigationStack, Tabs, Modal, Popup, Sheet, Toast
Tier 5: Dock, FileTree, code-editor primitives
Tier 6: Markdown, PDF, Browser, Charts, Map, Video
```

### 每个迁移 widget 的验证包

必须包含：

- behavior checklist；
- screenshot golden；
- input tape；
- accessibility snapshot；
- microbenchmark；
- allocation profile；
- old/new frame comparison；
- platform-specific edge cases。

### 退出标准

- UI zoo 100% 原生 Viso；
- widgets crate 不依赖任何 Makepad runtime crate；
- 关键 widgets 达成性能目标。

---

## 73. Phase 8 — Platform Services / Async / App Framework

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

## 74. Phase 9 — Studio / Inspector / CLI

### 原则

Studio 是 Viso public/tooling API 的客户，不是 core runtime 特例。

### 工作

- Node inspector；
- layout inspector；
- state timeline；
- event trace；
- GPU counters；
- frame profiler；
- `.vs` compiler service；
- hot reload diagnostics；
- source map；
- semantic query；
- `viso migrate`；
- `viso profile`；
- `viso test-ui`；
- `viso snapshot`。

### 退出标准

- Studio 不被 runtime/gpu/ui 反向依赖；
- headless/Studio 共用 introspection schema；
- 性能问题可通过工具直接定位。

---

## 75. Phase 10 — Makepad 源码迁移工具与历史清理

### 目标

让需要迁移的旧项目通过离线工具转换到纯 Viso，而不是维持旧 runtime。

### `viso migrate` 至少识别

```text
app_main! / AppMain
Cx
Widget
WidgetRef
WidgetWeakRef
WidgetSet
Walk
Layout
Turtle
DrawQuad
DrawText
DrawVars
handle_event
draw_walk
script_mod!
ScriptVm
App::from_script_mod
Struct::register_widget(vm)
mod.widgets.* registration
manual render/redraw
new_batch
```

### 输出类别

#### Auto

可以确定语义的机械转换。

#### Assisted

生成 Viso skeleton + TODO + precise diagnostic。

#### Manual

对于自定义 imperative widget、复杂 shader ABI、未建模的 OS glue 等，给出迁移步骤而不是生成兼容 wrapper。

示例诊断：

```text
VISOMIGRATE E204

Custom Makepad widget `MyWidget` uses an imperative draw lifecycle
with no direct Viso runtime equivalent.

Rewrite as:
1. Component state
2. Layout contract
3. typed input handlers
4. Painter / render primitives
5. Semantics

```

### 最终退出标准

- Viso 默认及完整功能构建均不依赖 Makepad runtime；
- migration fixtures 以当前 Makepad 的 `script_mod!` / `ScriptVm` 源码形态为准，不以 `.live` 文件作为迁移输入；
- 新示例只使用 `.rs + .vs`；
- Makepad 仅作为历史参考、算法来源和性能 baseline。


# Part XXVI — API 迁移映射

## 76. 入口

| Legacy | Next |
|---|---|
| `app_main!(App)` | `viso::run::<App>()` |
| `impl AppMain` | `impl Application` |
| `use makepad_widgets::*` | `use viso::prelude::*` |


---

## 77. Context

| Legacy | Next |
|---|---|
| `&mut Cx` everywhere | `AppCx/EventCx/LayoutCx/PaintCx/...` |
| `Cx` 直接拥有多种全局能力 | capability-based phase context |
| `cx.redraw_*` | state/dirty scheduler 自动请求 frame |

Migration lint：发现函数只用 layout 能力却接受万能 Cx 时给建议。

---

## 78. Widget / Ref

| Legacy | Next |
|---|---|
| `trait Widget` | `Component` + runtime Node/Painter contracts |
| `WidgetRef` | `NodeId` / typed `Handle<T>` |
| `WidgetWeakRef` | generational weak handle / optional `NodeId` validity check |
| `WidgetSet` | query result over NodeIds，短生命周期 |
| dynamic downcast | typed component handle / schema query |

普通用户不应频繁操作 raw `NodeId`；高级 runtime/inspector 才使用。

---

## 79. Child lookup

Legacy：

```rust
self.ui.button(id!(submit))
```

Next 选择：

### 静态绑定

宏生成 field/slot handle：

```rust
self.nodes.submit
```

### 动态查询

只用于 Inspector/动态内容：

```rust
cx.query(root, selector)
```

热路径禁止每帧以字符串/LiveId 做 tree search。

---

## 80. Layout

| Legacy | Next |
|---|---|
| `Walk` | `SizeRule`/layout properties |
| `Layout` | Container layout spec |
| `Turtle` | internal Row/Column/Flow algorithm |
| `Fit/Fill` 复杂组合 | `Auto/Fr/Percent/Px` 统一规则 |
| widget 自己驱动 draw_walk | measure/layout/paint phase |

迁移器可以把常见：

```text
width: Fill
height: Fit
flow: Down
```

转换为：

```text
width: 1fr
height: auto
layout: column
```

语法是否最终采用 `1fr` 可另议，IR 语义按统一尺寸模型处理。

---

## 81. Event / Action

| Legacy | Next |
|---|---|
| `handle_event(&Event)` | target-based typed event |
| `Actions` 集合扫描 | typed action queue / direct local handler |
| widget 全树 event traversal | hit-test/focus target routing |


---

## 82. Draw

| Legacy | Next |
|---|---|
| `DrawQuad` | `QuadPainter`/Quad primitive |
| `DrawText` | Text layout + GlyphRun primitive |
| `DrawVars` | explicit shader resource/binding data |
| implicit instance tail memory | generated `GpuInstance` descriptor |
| `new_batch` user property | renderer automatic batching |

---

## 83. Viso DSL / Current Makepad Script System

| Legacy | Next |
|---|---|
| `script_mod!` registration order | module graph + compiler topo sort |
| `ScriptVm` runtime evaluation | compile-time/AOT `.vs` IR + dev hot-reload evaluator |
| `App::from_script_mod` / widget registration | Viso schema/module instantiation |
| script owns widget creation | schema → UI IR → Node instantiate |
| dynamic property lookup | typed property IDs |
| manual `ui.x.render()` | state binding invalidation |
| source-to-opcode direct runtime path | CST → AST → HIR → IR/bytecode |

---

## 84. Theme

| Legacy | Next |
|---|---|
| concrete dark/light widget theme definitions | semantic token system |
| widget-specific raw color override | token + component variant |
| repeated draw_text color fixes | inherited/default semantic text color rules |

---

# Part XXVII — Migration Tooling

## 85. `viso migrate`

建议提供：

```text
viso migrate report
viso migrate app-entry
viso migrate script
viso migrate widget
viso migrate check
```

### 85.1 report

扫描并输出：

```text
legacy app_main!           3
WidgetRef                 81
Rc<RefCell<dyn Widget>>    2 custom copies
manual render()           17
new_batch                 12
script_mod!               45
ScriptVm init/register      7
Walk/Fill/Fit            133
custom DrawVars            8
unsafe instance layout     4
```

### 85.2 自动修复级别

每个 rule 标记：

```text
SAFE_AUTOFIX
REVIEW_REQUIRED
MANUAL
```

禁止迁移器在语义不确定时静默重写。

### 85.3 结构化迁移

Makepad 脚本迁移必须先从 Rust 的 `script_mod!` 宏 token stream 与 `ScriptVm` 注册图恢复结构化脚本表示，再基于 CST/AST/HIR 做转换；不得用正则批量替换。

Rust 迁移尽量基于 rust-analyzer/AST 或 proc macro diagnostics。

---

# Part XXVIII — Repository 详细编排

## 86. Framework repo 最终建议

```text
viso/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── ARCHITECTURE.md
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

## 87. `lib.rs` 规则

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

## 88. Vendor 规则

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
Makepad 自己的 map/ai/game
```

全部重新塞进一个模糊 `libs/`。

---

# Part XXIX — Public API 细化建议

## 89. `run::<App>()`

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

## 90. Typed Handle

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

## 91. Query API

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

## 92. Custom painter

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

# Part XXX — 关键架构决策记录（ADR 摘要）

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

# Part XXXI — 风险与取舍

## 93. 风险：NodeArena 可能使 API 变得“过底层”

缓解：

- 用户用 typed `Handle<T>`；
- `NodeId` 主要是 runtime/tooling API；
- Component macro 生成静态引用；
- Inspector 用 NodeId。

---

## 94. 风险：编译式 reactive 太复杂

如果所有 dependency 都要求编译期推导，会限制动态 UI。

取舍：

```text
Static fast path
+
Dynamic fallback path
```

静态 Rust/Viso DSL binding 使用 compact dependency table；动态 scripting 可以运行时注册 dependency，但不得让动态路径定义整个框架成本。

---

## 95. 风险：过度 AOT 损害 live-editing / hot-reload 特性

解决：Dev 和 Release 执行同一种 UI IR 语义。

- Dev：IR 来自增量 compiler；
- Release：IR 来自 build-time compiler。

不是维护两套 UI runtime。

---

## 96. 风险：crate 太少导致编译慢/边界不够硬

当前建议十余个 crate 是起点，不是永远不拆。

拆分依据必须是测量/依赖边界，而不是目录美学。

每半年检查：

- compile timings；
- dependency fan-in/out；
- unsafe boundary；
- feature independence。

---

## 97. 风险：自己维护 GPU RHI 成本高

这是有意取舍。

Viso 的核心差异化之一就是控制 rendering stack。若完全让渡给通用 abstraction，工程成本下降但长期性能/能力上限受限。

仍可以提供 experimental wgpu backend/integration，但不让其定义 public renderer architecture。

---

## 98. 风险：Service 层变成另一个 God Object

不要提供：

```rust
cx.services().everything().do_anything()
```

而是小 trait + capability registry。

服务之间不能随意相互依赖。

---

# Part XXXII — Definition of Done

## 99. Viso 核心完成标准

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
- state migration。

### Engineering

- clear crate DAG；
- CI dependency rules；
- benchmark regression gates；
- headless tests；
- Studio only depends downward/public tooling APIs。

### Migration

- official examples migrated；
- migration report/autofix available；

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
    pub use crate::{Color, Rect, Size, Vec2}; // facade 统一导出基础几何类型
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

# Appendix G — 需要后续 ADR 决定的问题

以下问题不应在没有 prototype/benchmark 前武断定死：

1. Node hot storage 最终采用纯 SoA、hybrid SoA 还是 chunked archetype；
2. `Handle<T>` 是否允许直接读取组件 typed state；
3. transform tree 是否独立于 layout tree；
4. clip cache 的最佳结构；
5. text shaping cache key 与跨 paragraph 共享粒度；
6. GPU instance buffer 是 per-type persistent pool 还是 frame ring + retained ranges 混合；
7. Web 是否长期使用 WebGPU-only，是否保留兼容后端；
8. Linux Vulkan/OpenGL 兼容策略；
9. dynamic scripting VM 的保留范围；
10. 是否拆 `viso-text` 为独立 crate（本文建议是）；
11. platform backend 何时从 module 升格独立 crate；
12. accessibility 是否直接基于 AccessKit 或保持 Viso 自有 semantics + platform adapter；
13. Rust declarative UI 采用 proc macro、builder、函数式 DSL 的最终语法；
14. `.vs` 已确定为 Viso DSL 的唯一 canonical 扩展名；生成器、示例、文档、LSP、Formatter、Studio 与迁移工具均只输出 `.vs`；
15. 3D scene graph 放 render、extras 还是独立 crate。

原则：**先确定不可妥协的性能/边界语义，再通过 benchmark 和实现经验决定具体容器与语法。**

---

# Appendix H — 来源与设计依据

本文综合了：

1. 用户提供的《Makepad 重构设想：从 UI Engine 到 Rust-first 跨平台 App Framework》，其中关于单一 facade、极简入口、App Framework、移动能力、Viso DSL/热更新工程化、工具链的建议被部分采纳；crate 过度拆分、过多文件类型、页面默认多文件、强 App 架构等部分被本文主动收敛。
2. Makepad 当前 `dev` 分支 README：<https://github.com/makepad/makepad/blob/dev/README.md>
3. Makepad 当前 workspace：<https://github.com/makepad/makepad/blob/dev/Cargo.toml>
4. 当前 Agent/开发说明：<https://github.com/makepad/makepad/blob/dev/AGENTS.md>
5. 当前 Widget 实现：<https://github.com/makepad/makepad/blob/dev/widgets/src/widget.rs>
6. 当前 platform dependencies：<https://github.com/makepad/makepad/blob/dev/platform/Cargo.toml>
7. 当前 `script_mod!` / `ScriptVm` DSL 与迁移规则以 Makepad `dev` 分支 `AGENTS.md` 和相关脚本源码为准；`splash.md` 仅在其内容仍与当前实现一致时作为补充参考：<https://github.com/makepad/makepad/blob/dev/splash.md>

本文的目标架构是设计建议，不代表 Makepad 官方 roadmap。

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

只要后续所有重构决策都能通过这四条和可重复 benchmark 检验，Viso 就可以同时拥有极简开发体验、清晰工程结构和非常高的性能上限。
