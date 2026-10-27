# Viso 架构设计

> 文档状态：Viso 1.0 Draft / Architecture Specification  
> 目标读者：Viso 核心维护者、Renderer/GPU 工程师、UI/Widget 工程师、DSL/Compiler 工程师、工具链工程师、AI Coding Agent  
> 基线日期：2026-10-26  
> 适用范围：Viso 是独立设计、独立实现的 Rust-native 跨平台 UI/Application Framework。本文只定义 Viso 自身的 API、ABI、运行时、DSL、工具链与项目合同。

---

## 0. 文档目的

本文是 **Viso 1.0** 的架构设计，用于固定框架的核心语义、依赖边界、运行时数据模型、工具链合同与性能约束。已实现的部分描述代码现状；尚未实现的部分以「目标（尚未实现）」逐条标出。与 Accepted ADR（`docs/adr/`）冲突时以 ADR 为准；`Viso_Rendering.md`、`Viso_Text_Font_Runtime.md`、`Viso_DSL_1.0.md`、`Viso_Hot_Reload.md`、`Viso_CLI.md` 在各自子系统内与本文同级，本文对这些子系统只给摘要与指引。

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

Viso 的 Rust 集成提供**按语义命名的三个宏入口**（`viso-ui-macros`，由 facade 重导出）：

```rust
// 1. 内联 View Fragment：无状态的小组件、测试、示例、局部静态 UI
let content = ui! {
    Column {
        Text { text: "Hello"; }
    }
};

// 2. 外部 .vs View：页面、主题、大组件，获得独立 hot reload
let page = viso::view!("features/home/view.vs");

// 3. 内联完整 Component：适合很小、与 Rust 紧密耦合的组件
viso::component! {
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

`ui!` 展开为 `|cx: &mut BuildCx| -> Handle` 的构建闭包；`view!` 展开为 `|cx| -> MountedView<_>` 的构建闭包（`root` 与 `nodes`：根组件每个只构建一次的具名节点一个 `Handle<kind::K>` 字段），可带根组件的 Input（`view!("x.vs", name: expr)`）；`component!` 展开为一个字段为各 `state` 的 `StateId` 的 struct 与具名节点的 `{Name}Nodes` struct，带 `build(cx) -> (Self, MountedView<{Name}Nodes>)`。

这三个宏**不是三套语言**，而是同一个 Viso compiler（`viso_dsl::frontend`）的三个明确 parser entry point：

```text
ui!         -> ViewFragment
component!  -> ComponentDecl
view!(...)  -> external .vs CompilationUnit / exported Component
                    ↓
          Name Resolution / Schema
                    ↓
                 Typed HIR
                    ↓
   UI IR / Binding IR / Behavior IR / Shader IR
```

宏层只决定 source origin 与合法的顶层语法范围，不复制 type checker、schema、HIR、binding 或 runtime 语义。外部 `.vs` 由 `viso run` 内的 watcher 独立增量编译并以 typed patch hot reload（app 端没有 watcher，也不链接 `viso-dsl`）；`ui!` / `component!` 随 Rust rebuild 更新，`viso run` 中经 warm restart 保留状态（`Viso_Hot_Reload.md` §26）。三者展开为同一种产物：静态节点的 `BuildCx` builder 调用、`cx.bind` 失效边、control-flow region 的模板数据与 behavior module；Release 中 behavior 为生成的 Rust（§0.2-13），Viso runtime 不需要解析 Rust 或 `.vs` 源码。

`Live` 只描述 live editing / hot reload 能力，不作为 Viso 的语言、crate、目录或文件格式名称。

### 0.2 Viso 1.0 的硬决策

Viso 1.0 将以下地基问题定义为正式架构决策：

1. **Node hot storage：采用 hybrid indexed SoA。** `NodeArena` 的 `NodeLinks`（parent/children/siblings，20 字节）保持紧凑 AoS 以服务树遍历，generation 单独成列；Layout/Transform/Paint/Interaction 等高频字段使用与 `NodeIndex` 同索引的连续数组（`viso_ui::NodeStore`）；低频/可选数据进入 sparse cold side tables。暂不采用全 ECS/chunked archetype。
2. **Handle 只表达 identity/capability，不直接暴露跨帧 `&T`/`&mut T`。** builder 返回的 `Handle` 只携带 `NodeId`；控件的 typed handle（`ModalHandle`、`NavHandle`、`PopupHandle` 等）只携带它的 `StateId`/`NodeId`，操作都接受 `&mut EventCx`；state 经 `cx.get(StateId)` / `cx.set(..)` 读写。读写必须通过受 context 约束的 API，避免绕过 reactive invalidation 和生命周期检查（§78）。
3. **Transform：与 Layout、Paint 都使用独立的失效/传播平面。** 三者共享 `NodeId` 和结构语义；transform/scroll/transform animation 只重算 transform table 中各 root 的 place（平移，或绕 root 原点的缩放/旋转），`Renderer::set_transforms` 按 place 映射 retained draw，不触发布局，不重建 primitive，也不上传 instance；含 offscreen layer、backdrop 或 user program 的 root 移动时整帧重绘（§19.2，ADR 0043）。
4. **自研与依赖必须分级。** Viso 只在决定差异化、热路径和语义所有权的部分强制自研；Unicode/shaping、accessibility bridge、async executor、网络/TLS、媒体 codec 等优先复用成熟实现，并由 Viso 自己掌握 integration/cache/ABI contract。
5. **模块边界必须机器化验证。** crate DAG 由 `cargo xtask check-deps`（`xtask/src/main.rs` 中的 allowlist）在 CI 验证，不再只依赖 AGENTS.md 和 code review；module 级 import 规则检查仍是目标（§10.2）。
6. **Reactive dynamic fallback 必须显式、可计数、可 benchmark。** 编译器能够静态解析的绑定不得静默掉入动态路径（`ReactiveCounters::dynamic_fallback_nodes`，`viso-ui` 的 `reactive_binding` bench 断言静态路径为零）。
7. **Backend 采用 Device Class × tier × capability 模型。** 每个 Device Class（Desktop / Mobile / Web / Embedded-GPU / Embedded-CPU，§50A）有自己的 Tier-1 后端，并针对现代主后端优化；兼容后端可以追加，但不得反向把 renderer 设计压成最低公分母。
8. **Viso DSL 使用 `ui!` / `view!` / `component!` 三个语义化 Rust 入口。** `ui!` 解析 View Fragment，`component!` 解析 Component，`view!("...vs")` 编译外部 `.vs`；三者共享同一 schema/HIR/IR/runtime 语义。`.vs` 是唯一 canonical 外部 DSL 文件扩展名。
9. **Identity 分层。** 源码名字使用 compiler-local `NameId`；跨编译稳定身份使用 128-bit `SymbolId`（artifact 与 wire 中为 `StableId`、`StateKey`）；运行时热路径使用 typed dense ID：实例为 generational `NodeId`（内部链接为无 generation 的 `NodeIndex`），state 为 `StateId`，另有 `ComputedId`、`EffectId` 等，behavior 入口以 chunk 索引寻址。property 不设运行时 ID：构建期 lower 为具体 builder 调用、`AotStyle` 字段与 `DirtyClass` 边；event 由 schema 的稠密枚举 `EventRoute` 分派；用户组件在编译期内联（ADR 0039），没有运行时组件类型 ID。禁止一个万能 ID 贯穿所有 subsystem（§10.4）。
10. **Viso 自研 Ende。** `viso-ende` 负责内部 Binary/JSON Encode/Decode、wire schema、协议版本、cache/snapshot/tool transport；RON 不属于 Viso；Serde 只作为生态 integration。
11. **Hot Reload 是 Dev-only build capability。** Dev Runtime、patch transport、DevSnapshot endpoint 与 hot-reload-only metadata 只编入 Dev artifact（facade feature `viso/hot-reload`）；Release/Shipping 完全不编入这些路径（facade 的 build script 拒绝带 `hot-reload` 的 release/shipping build，`cargo xtask check-release-absence` 扫描产物），不能靠运行时配置重新开启，release frame hot path 不承担热更税（`cargo xtask check-frame-tax`）。
12. **`viso run` 的 desktop host 是隐式且唯一的。** macOS/Windows/Linux 开发统一使用无参数 `viso run`；`viso run ios/android` 只面向 Simulator/Emulator；`viso run web-gpu|web-dom|web-hybrid` 面向浏览器；Android/iOS 开发环境分别由 `viso android` / `viso ios` 管理，不把手机 physical-device debugging 作为 Viso 1.0 开发闭环的一部分。Embedded class 的开发闭环是：经 SSH 的板卡 Dev session（`viso run <target>`，target 写 `deploy`），以及在 macOS 上仿真 `embedded-cpu` 板卡（`viso run <target> --emulate`）（`Viso_CLI.md` §13.11，§50A.7）。
13. **Release 不解释执行 UI 行为。** `ui!` / `component!` / `view!` 都是构建期入口；Release 中模块的每个 chunk（handler、region entry、显示值、computed、effect、state 初始化、函数、闭包、task）都 lower 为生成的 Rust 函数（按 chunk 索引的静态函数表），task 是可恢复状态机。Behavior bytecode 解释器是 facade 的 `vm` feature，不在默认集内，由 `viso build` / `viso run` 只为 Dev build 打开：开启时 debug build 运行 bytecode（Dev 热更新 patch 替换它）；release build 与任何不开 `vm` 的 build 只运行生成的 Rust（`viso::__behavior!`）。显式 dynamic capability 包与 sandbox/AI preview 使用解释器是目标（尚未实现，§44）（§41，ADR 0041）。
14. **编译器不依赖运行时栈。** `viso-dsl` frontend 只依赖纯数据与无 runtime 的 crate：`viso-lexer`、`viso-schema`、`viso-ende`、`viso-behavior`、`viso-shader`、`viso-dev-protocol`、`viso-math`；LSP、`viso check` 与 CI 分析不链接 GPU/platform/runtime。`gpu` 不依赖 `runtime`，`text` 不依赖 `gpu`（§10，ADR 0042）。
15. **资源占用是一等合同。** 每个 Device Class 都有 Footprint Profile，在编译期裁剪能力；`cargo xtask footprint` 度量二进制体积（含 data/bss）、首帧时间、峰值/空闲 RSS、counted heap、GPU 内存与空闲 CPU，基线记录在 `benches/footprint/baseline.md`，`--check` 按容差比较；CI 以 `--against <base>` 在同一台机器上与所基于的提交成对比较。Embedded-GPU（嵌入式 Linux + GLES3/Vulkan）属于 1.0 目标；Embedded-CPU（无 GPU，CPU 光栅 + damage 局部刷新）是 1.0 Tier-2；MCU 不在 1.0 交付，但 core crates 必须保持 MCU-ready（CI 的 `cargo xtask mcu-smoke`）（§50A，ADR 0044）。

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

API 层可以有 `Button`、`window(..)` / `WindowHandle`、`StateId`、`NavHandle` 等自然概念；热路径内部不要求用同样的对象布局。

例如：

```text
Button API
   ↓
NodeId
   ↓
NodeArena: generation[] / NodeLinks[]
layout[] / bounds[] / measured[]
translate[] / scroll[] / turn[]
style[] / opacity[] / …
hittable[] / hidden[] / …
sparse cold tables（handler、semantics、content、placement …）
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

Dev 与 Release 执行同一份 Typed HIR / UI IR 语义：Dev 用 VM 解释执行（以支持热更新），Release 生成原生代码。二者的语义一致性由 differential test 保证（`crates/dsl/tests/differential.rs`：fixture 模块的每个 chunk lower 为 Rust 后与解释器逐步并跑，比较返回值或 fault、state、revision、dirty slot、事件与 cost），而不是靠维护者记忆。

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
13. 资源占用回归同样进入 CI：每个 Footprint Profile 记录二进制体积、静态内存、峰值 heap、空闲 RSS 与首帧时间（§50A.5）。度量与基线已有（`cargo xtask footprint`，目前覆盖 desktop、embedded-cpu 与 headless）；CI 接入与 mobile/web/embedded-gpu 的度量是目标（尚未实现）。
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
- image/audio/video media codec（视频经 OS 播放器解码，ADR 0049）；
- accessibility OS bridge；
- platform bindings；
- cryptography / TLS。

Viso 在这些领域拥有的是 **cache policy、数据布局、生命周期、增量接口、性能 instrumentation 与替换边界**。例如 `viso-text` 使用 rustybuzz、ttf-parser、unicode-*/ICU segmenter 等成熟 crate，但 paragraph cache、glyph atlas、增量 line layout 与 UI invalidation 仍属于 Viso。

Accessibility 的 canonical model 是 Viso 自有 semantics tree；平台 adapter 优先复用 AccessKit 等成熟桥接能力，只在能力或性能不满足时实现平台专用 adapter。仅在辅助技术激活时发布、请求以 `RawEvent::Accessibility`（`AccessRequest`）回流，以及 iOS（UIAccessibility container）与 Web（ARIA DOM mirror）两个 Viso 自有 adapter，见 [ADR 0030](docs/adr/0030-accessibility-os-bridge.md)。platform 边界格式是 Viso 自有的 `SemanticsDelta`（ADR 0042）：`PlatformApp::update_accessibility(window, SemanticsDelta)` 按值推送，delta 只携带新增或已变化的 `AccessNode`（typed `SemanticsId`、`SemanticsRole`、label/description/value、live、level、access key、物理像素 bounds、`SemanticsState`、`AccessActions`、active descendant、error message、children），外加新树时的 root 与当前 focus；不再可达的节点由接收端丢弃。AccessKit 只出现在 macOS/Windows/Linux/Android adapter 内部：`viso-platform` 的私有模块把 delta 一次性转成 `accesskit::TreeUpdate`，`accesskit` 依赖仅对这些 target 编译，Linux 上只随桌面 backend（`viso-platform` 的 `x11`/`wayland` feature）编译——只开 `kms` 的 embedded build 不链接 AccessKit、AT-SPI 与 D-Bus；iOS 与 Web adapter 直接把 delta 折叠进自有 mirror，不链接 AccessKit。没有 a11y bridge 的 backend（KMS 的 `PlatformCaps::FULLSCREEN`，`accessibility: false`）上 facade 不建 bridge、不生成 delta；semantics tree 本身仍然保留，供测试与 Inspector 使用。

#### Tier C — Adapter / 可替换依赖

- Tokio / Smol / 其他 async executor；
- tracing / serde 等生态集成；
- HTTP client / database / telemetry。

它们必须位于 adapter/service 边界，不拥有 Viso main loop、frame semantics、UI identity 或 renderer architecture。网络、数据库等非 UI 能力不进核心，由 `viso-ext` 插件 crate 提供（如 `viso-net`，§49）。

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

facade 的 feature 少而稳定，普通应用不手工设置任何 feature：默认集（`desktop`、`native-vulkan`、`rasterizer`、`complex-line-break`、`avif`、`dates`、`pdf`）覆盖桌面、移动与 Web；`vm`（behavior 解释器）由 CLI 为每个 target 的 Dev build 加入，release/shipping build 不带；`hot-reload`（隐含 `vm`）由 `viso run` 的 Dev build 加入，release/shipping build 带它会被拒绝；`headless` 由 `viso build headless` / `viso test` 加入；`inspector` 是工具用的可选项。

不推荐暴露一个巨大的 feature 笛卡尔积。底层 executor、平台 backend、GPU backend 由 target 自动解析（ADR 0029），而不是让普通用户手工组合。

能力裁剪通过 **Footprint Profile**（`desktop` / `mobile` / `web` / `embedded-gpu` / `embedded-cpu`，§50A）完成。内建 target 隐含 profile（desktop host 为 `desktop`，`ios`/`android` 为 `mobile`，web target 为 `web`）；板卡在 `Viso.toml` 的 `[target.<name>]` 中声明 profile。CLI 把 profile 解析成一组固定的 feature 集合传给 Cargo（embedded profile 关闭默认 feature，只开 `gles`/`cpu` + `kms`；任何 profile 的 Dev build 再加 `vm`），并以 `VISO_FOOTPRINT` 告知 facade 的 build script。用户只选 target，不手工组合 feature；`cargo xtask check-profiles` 在 CI 中验证这几组固定组合的 release 依赖闭包。

### 5.1 `viso::prelude::*`

`prelude` 只导出普通应用高频、稳定、低歧义的概念；GPU/backend/internal compiler 类型永不进入。当前内容（`crates/viso/src/lib.rs` 的 `prelude`）：

```text
应用入口      Application, run, AppCx, ServicesExt
场景构建      BuildCx, EventCx, State, StateId, StateValue, NodeId, DirtyClass,
              Component, Sizing, Axis, Align, Justify, Inset, BoxStyle, Rgba,
              FlexStyle, LeafStyle, GridStyle, GridPlacement, TrackSizing, TrackMax,
              AdaptiveColumns, VirtualListStyle, SpanStyle, TextRequest,
              Semantics, Role, CursorIcon, InputKind
路由          Navigator, router_view, routes!
控件          每个官方控件的 builder 函数、类型、Style 与 Handle
              （label, button, image, scroll, virtual_list, grid, tabs, modal,
              sheet, popup, toast, navigation_stack, text_input, form, data_grid …）
窗口与菜单    window, WindowBuilder, WindowHandle, WindowConfig, WindowChrome,
              Menu, MenuCommandId, Accel, SystemAction
几何          Point, Rect, Vec2
宏            ui!, component!, view!
```

其余宏在 crate 根：`viso::asset!`、`viso::packaged_fonts!`，以及 `viso::print!` / `println!` / `eprint!` / `eprintln!`（Web 上写入页面 console）。`hello_world`、`01-counter`、`links` 与 `04-navigation` 的视图代码只 `use viso::prelude::*`；其余示例另外引用的是指针、光标、托盘、native 等高级模块。

高级模块必须显式进入：

```rust
use viso::ui::*;
use viso::render::*;
use viso::gpu::*;
use viso::platform::*;
```

### 5.2 API 稳定层级

文档和模块明确三类 API：

```text
Stable      普通应用长期依赖
Advanced    Widget/renderer 扩展作者使用
Internal    Viso 自身，不保证兼容
```

通过 module 组织表达：

```rust
viso::prelude                     // Stable：默认集
viso::{audio, cursor, shell, services, native, fonts, clipboard, svg, pdf, …}
                                  // Stable：按需进入的应用能力
viso::{ui, widgets, render, gpu, platform, runtime, math}
                                  // Advanced：内部 crate 的整体重导出
viso::__private, viso::__test_support
                                  // Internal：#[doc(hidden)]，宏展开与 facade 自身测试用
```

---

## 6. Application 模型

最小应用：

```rust
use viso::prelude::*;

struct App;

impl Application for App {
    fn new(_cx: &mut AppCx) -> Self {
        App
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        ui! { Text { text: "Hello Viso"; } }(cx);
    }
}

fn main() {
    viso::run::<App>();
}
```

也允许 DSL：

```rust
impl Application for App {
    fn new(_cx: &mut AppCx) -> Self {
        App
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("app.vs")(cx);
    }
}
```

`Application` 的职责：

- `new(&mut AppCx)` 构造应用；`AppCx` 在第一个窗口打开前登记字体（`load_font`、`use_packaged_fonts`）与 warm restart 要保留的状态（`dev_state`）；
- `window_config()` 给出启动窗口的标题、逻辑尺寸与 chrome（默认自绘 caption）；
- `build(&mut self, &mut BuildCx)` 在启动时构建一次启动窗口的 retained 场景：声明节点、分配 state、注册 handler、连接 binding；
- 其余 hook 都有默认实现：`menu` / `on_menu_command`、`on_open_url`（`[package.links]` 深链接）、`on_lifecycle`（§50 的顺序）、`on_tray`、`memory_budget`、`image_loader`。

窗口不是节点控件：更多窗口在 handler 中以 `window(config).content(|cx| ..).open(ev)` 打开，返回 `WindowHandle`。`run` 拥有 platform event pump 与 frame scheduler，无原生 backend 时回退 headless，直到最后一个窗口关闭（§77）。

### 6.1 `Application` 不强制 Router/Store

小应用不应该被迫包含：

```text
Router
Global Store
Reducer
Effect System
```

这些是按需能力：应用自己的 struct 持有它需要的 `StateId` 与控件 handle（如 `NavigationStack` 的 `NavHandle`）。

```rust
struct App {
    nav: Option<NavHandle>,
}
```

Typed route（`viso_ui::router`）：路由是 app 自己的 `Clone + PartialEq` 类型，通常是一个 variant 携带页面参数的 enum。

```rust
let nav: Navigator<Route> = router_view(Route::Inbox, routes! {
    Route::Inbox => inbox,
    Route::Message { id } => |cx| message(cx, *id),
})
.on_back_requested(|ev, request| if unsaved(ev) { request.prevent_default() })
.build(cx);

nav.push(ev, Route::Message { id: 7 }); // 任意 StateWriter：handler、task、on_open_url
```

- `Navigator<R>` 是 `Copy` 的控件 handle（§78），可先 `cx.handle()` 铸造再 `.navigator(nav)` 绑定；读 `current` / `can_pop` / `depth` / `routes` 经 `StateRead + Controls`，在 effect / computed 中记录依赖；命令 `push` / `replace` / `pop` / `reset`；
- 一个 transaction 内的多次导航只抬高一个 revision cell，`RouterView` 在 flush 的 structure hook 中 reconcile 一次：按路由值匹配已挂载的页面（保留节点与 state），挂载新页面（每种 variant 一个 `Template`），释放弹出的页面（显示中的页面先完成离场再释放），把子节点排成栈序，再翻到栈顶；
- 显示已有页面向后翻、新页面向前翻，转场与 `NavigationStack` 相同；翻页后焦点移到新页面的首个 heading，没有时移到页面根；
- 系统返回（Android Back、Escape、鼠标后退键）经 `back_target` 交给最内层显示多于一页的 `RouterView`：`on_back_requested` 可 `prevent_default`，否则 `pop`；只剩根页时交给宿主；
- 深链接在 `Application::on_open_url` 中把 URL 解析为路由，再 `reset` / `push`；
- `routes!` 展开为一个 `match`，缺少页面的路由是编译错误。

DSL 侧：`.vs` 的 `input nav: Navigator<Route>` 由 `view!("shell.vs", nav: router)` 传入 Rust 的 `RouterView<R>`（`viso_view::ViewInput`），`.vs` 的 `RouterView { navigator: nav; }` 节点在自身内部构建这个 Rust router view，`back_requested` 先于系统返回派发（`Viso_DSL_1.0.md` §U11，ADR 0055）。Viso 不提供全局 `Store` 类型；跨 feature 的共享状态是应用自己的 Rust 类型。

### 6.2 推荐但不强制的应用分层

局部状态：直接 state cell（Rust 中 `cx.state(..)` 返回的 `StateId`，DSL 中组件的 `state`）；  
父子交互：typed callback / action；  
跨 feature：domain action / 应用自己的共享状态；  
异步：task + typed result（`EventCx::spawn_then`）；  
导航：`NavigationStack`；typed route 为目标（§6.1）。

不要把所有按钮点击都变成全局 `Message`。

---

## 7. Component 模型

用户侧组件应该非常普通。State 属于 Component：读取或写入 State 的 View 是一个 Component 的 `view`（`component!` 或 `.vs`），`ui!` Fragment 是无状态 View，只挂载常量值与它命名的 Rust Component：

```rust
viso::component! {
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

`ui!` 把 schema 中没有的节点类型当作 Rust Component 路径，以 `<Path as viso::ui::Mount>::mount(cx)` 挂载，返回其根。`Mount` 是二者的同一形状：`component!` 生成的 struct 实现它（`build` 分配自己的 State），手写控件实现 `viso::ui::Component`（`fn build(&self, cx: &mut BuildCx)`）并 `Default` 即得到它；`BuildCx::component(&c)` 构建任一 `Component` 并返回其根。

DSL 语法（property binding 用 `property: expr;`，命名节点用 `node name: Type { ... }`，行为赋值用 `=`）以 `Viso_DSL_1.0.md` 为准。

但是 `view { … }` 的作者体验不代表运行时每次真的构造新树。

宏/编译器生成：

```text
Static nodes      直接的 BuildCx builder 调用（cx.flex / cx.leaf / …）
Binding edges     cx.bind(StateId, node, DirtyClass) → BindingTable
Region templates  if / for / match 的模板数据，由 viso-view 挂载
Behavior module   handler / computed / effect 的 chunk 表（Release 为生成的 Rust）
```

首次 mount 实例化 retained node；之后状态变化只按 binding 边标脏、更新受影响节点。

### 7.1 Component 与 Node 分离

严格区分：

- **Component**：业务状态、行为、生命周期（`Component` trait / `component!` struct，state 在 `StateStore`）；
- **Template/Element**：声明结构（DSL 的 UI IR，编译后为 builder 调用与 region 模板，无运行时 element 层）；
- **Node**：运行时 retained tree 中的实体（`NodeStore` 中的一个 `NodeId`）；
- **Paint/Render data**：GPU 相关数据（`viso-render` 的 `Primitive` 与 retained draw）。

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
├── Viso_DSL_1.0.md / Viso_DSL_Rationale.md / Viso_Hot_Reload.md / Viso_CLI.md
├── Viso_Rendering.md / Viso_Text_Font_Runtime.md
├── crates/
│   ├── viso/          facade
│   ├── macros/        viso-macros（proc-macro）
│   ├── ui-macros/     viso-ui-macros（ui! / component! / view!）
│   ├── math/  ende/  handle/  glyph-a8/  gpu-abi/  lexer/   叶子
│   ├── schema/  dev-protocol/   编译边界与 dev 链路的纯数据合同（ADR 0042）
│   ├── runtime/  platform/
│   ├── gpu/  shader/  text/  render/  svg/  image/
│   ├── pdf/           viso-pdf（primitive 流写成 PDF 1.7）
│   ├── media/         viso-media（经 OS 播放器播放媒体，叶子，ADR 0049）
│   ├── ui/  widgets/
│   ├── behavior/  view/   viso-behavior（VM）/ viso-view（compiled view 运行时）
│   ├── dsl/  lsp/
│   └── services/
├── extras/
│   ├── charts/        viso-charts（折线/面积/柱状/散点图与 sparkline，选装）
│   ├── map/           viso-map（矢量地图视图：瓦片金字塔、相机、标注、覆盖物、倾斜，选装）
│   └── code-editor/   viso-code-editor（rope 文档、多光标、高亮、折叠、查找替换、LSP client，选装）
├── tools/
│   ├── project/       viso-project（项目发现/配置/构建身份）
│   ├── package/       viso-package（iOS .app、Android APK、web bundle、Windows MSIX、Linux desktop entry、归档与扩展的平台片段合并）
│   └── cli/           viso-cli（`viso` 命令：参数、输出、exit code）
├── libs/woff2/        viso-woff2（调用方侧 WOFF2 解压，ADR 0028）
├── examples/          hello_world、01-counter、10-custom-shader、i18n、images、links、persist、pointer、shell、video、code-editor；
│                      map 自成 workspace（依赖 `../viso-ext`，被根 workspace `exclude`）
├── benches/footprint/ viso-footprint（`cargo xtask footprint` 的参考 app 与 baseline）
├── tests/characterization/  fuzz/（独立的 cargo-fuzz workspace）  docs/adr/
└── xtask/             cargo xtask check-deps 等（§10.2）
```

生态 adapter（tokio/tracing/serde 等）不设 `integrations/` 目录，归同级 `viso-ext` 仓库（§74）。目标（尚未建立）：`extras/` 的其余部分（markdown、pdf viewer、browser、xr）、`tools/studio`、`tools/migrate`（§38）、`vendor/`。新增目录或 crate 时必须同步 §8、§9、§10 与 xtask allowlist（`check-deps` 对未登记的 workspace member 直接失败）。

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

Facade。热路径逻辑在下层 crate；facade 负责把它们组装成一个 app：

- `run::<App>()`；
- app driver：`AppDriver` 实现 `viso_runtime::FrameDriver`，持有每个窗口的 `WindowState`（node store、renderer、surface、语义镜像），把 platform 事件、ui 的帧阶段、render 的提交与 gpu 的 present 接在一起；
- `prelude`；
- feature 聚合（`vm`、`hot-reload`、`headless`、`desktop`、`gles`、`kms`、`cpu`、`emulate`、`pdf`、`avif`、`rasterizer`、`complex-line-break`、`dates`、`inspector` 等）；
- public module re-export；
- 平台能力查询（`AppCx::capabilities`）；
- 只有 facade 能接线的部分：CoreText / DirectWrite / fontconfig / Android 系统字体 provider（ADR 0026）、媒体播放器与 asset 表、dev session（`hot-reload`）、headless 运行、`viso::pdf` 导出。

目标（尚未实现）：版本查询 API。

### `viso-macros`

proc-macro crate，只依赖叶子 `viso-glyph-a8`（构建期 bake 与运行时 raster 共用同一个 rasterizer，宏不链接 text runtime）。当前提供：

- `#[derive(GpuPod)]`：GPU instance/uniform 显式布局（ADR 0001 中称为 `GpuInstance` ABI）；
- `#[derive(NativeData)]`：`.vs` 可读字段的 native record，或 `.vs` 可构造、`match` 的 native enum（Variant 可带字段），及其 schema；
- `packaged_fonts!`：build-time 字体登记、embedded 字体子集与 baked A8 page（§37.8）。


### `viso-glyph-a8`

叶子 crate：一个 glyph 的精确 A8 coverage 的两个来源——`raster` feature 下的 `rasterize_coverage`（`ttf-parser` outline + `ab_glyph_rasterizer`）与 baked page（"VBA8" 格式的编码、校验与原地读取、整数重采样）。不带 `raster` 时是 `no_std` + `alloc`，只有 reader。`viso-text` 与 `viso-macros` 都依赖它，保证 baked pixel 与运行时 raster 逐字节相同（Text/Font Runtime §3.6）。

### `viso-ui-macros`

`ui! { ... }` / `component! { ... }` / `view!("...vs")` 三个 DSL 入口（ADR 0014）。它依赖 `viso-dsl`，复用同一 frontend/HIR/UI IR，并生成 `viso_ui::` builder 代码；不拥有第二套 DSL parser。宏层只负责 Rust token/span、build artifact 与 typed API glue。

### `viso-ende`

Viso-owned Encode/Decode infrastructure（叶子，`no_std`）：

- Binary wire format（`Encoder` / `Decoder`）；
- bounded decoder：每次读取都校验剩余长度，不可信输入不 panic、不越界；
- JSON：诊断/工具交换用的 `JsonWriter`，以及有界 reader `JsonValue`（工具读其他工具的 JSON、glTF、GeoJSON、LSP 消息）；
- protocol/schema metadata（`ProtocolTag`、`WIRE_VERSION`、`content_hash`）；
- execute-in-place：`Aligned` 对齐 `static`、`SectionWriter` / `Sections` section table（§50A.6 第 5 条）；
- cache/snapshot encoding；

框架消息的 framing 在 `viso-dev-protocol`，不在本 crate。不支持 RON；不承担 image/audio/video media codec；不作为 frame 内部数据模型。Serde 互操作不在本仓库，由 viso-ext 插件按需提供（如 `viso-net` 的 `serde` feature）。

### `viso-math`

Viso-owned allocation-free numeric and geometry foundation（ADR 0012，叶子，`no_std`，不需要 `alloc`）。它不是 draw helper，也不是新的 `core/utils`；它定义 UI、Render、Shader interface、Text geometry、Input、Animation、Game 等子系统共享的基础数值/几何/颜色值语义。

负责：

- `Vec2` / `Vec3` / `Vec4`（`f32` 是唯一精度）；
- `Mat2` / `Mat3` / `Mat4`（column-major）；
- `Quat` / `Affine2` / `Transform3`；
- `Point` / `Size` / `Rect` / `Insets`；
- `Ray` / `Plane` / `Aabb` 等基础空间几何；
- dot/cross/normalize/intersection/containment 等 allocation-free primitive；
- 颜色值与色彩空间换算：canonical blend 表示 `LinearPremul`、wire 表示 `LinearStraight`，`Srgb` / `LinearSrgb` / `DisplayP3` / `ExtendedLinear`，插值空间（Rendering §5）；
- coverage 与 source-over `composite`、pixel snap、geometry legality 分类；
- 缓动曲线（`Easing` / `CubicBezier` / `Curve`）与 `Spring`；
- target-specific SIMD specialization 的内部实现边界（编译期 `cfg` 选择，与 scalar bit-exact）。

不负责：

- UI measure/layout 协议、layout policy；
- 输出编码与色彩管理策略（surface 编码归 render/gpu，ADR 0048）；
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
- `viso-math` 不依赖任何框架 crate；
- Ende wire representation 由 `viso-ende` 定义，不直接 dump Rust struct memory。

### `viso-runtime`

App runtime：

- main loop：`Scheduler` 实现 `viso_platform::AppHandler`，经 `FrameDriver` 反转驱动上层，自身不认识 UI；
- frame scheduler：redraw reason 聚合、deadline 驱动的唤醒（ADR 0019）、无事可做时真正 idle；
- frame phase orchestration（`FramePhase`，ADR 0043）；
- clock（`WallClock` / `FixedStepClock` / `ManualClock`）；
- task identity 与 wakeup（`TaskSet`：waker 可跨线程唤醒，经 platform 的 `LoopWaker` 踢醒 loop，只 poll 被唤醒的 task，ADR 0032）；
- lifecycle；
- 归一化后的 input 类型。

不负责 Widget 实现。node-scoped timer 与 node 所有的 task 在 `viso-ui`（`timer`、`task`），建立在这里的 `TaskSet` 与 deadline 之上。

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
- `LoopWaker` 跨线程唤醒（ADR 0032）；
- 菜单、拖放、gamepad（每帧轮询，ADR 0052）、默认音频输出 `AudioOutput`（ADR 0050）、标准输出；
- embedded 显示：DRM/KMS（`kms`）与桌面上的面板仿真（`emulate`）。

系统字体**不**在 `viso-platform`：`SystemFontProvider` / `ColorGlyphRasterizer` trait 在 `viso-text`，各平台实现在 facade（ADR 0026）。原生窗口句柄类型在叶子 crate `viso-handle`。平台实现放在同一 crate 的 `backend/` 目录，另有始终可用的确定性 headless backend；必要时再拆独立 backend crate。

### `viso-handle`

叶子 crate，定义原生句柄：`RawWindowHandle`——由 `viso-platform` 产生、`viso-gpu` 消费以创建 surface（无 compositor 的显示经 `RawWindowHandle::Scanout` 交出 page flip 的 `Scanout`）；`ExternalImage`——其他框架写入 GPU 可原地采样内存的画面（macOS / iOS 的 `IoSurface`、D3D12 共享纹理、Android `AHardwareBuffer`、dma-buf、Web `<video>`），由 `viso-media` 交出、`viso-gpu` 采纳为 texture；`Nv12`——手头的 NV12 平面，交 backend 转换。生产者与消费者因此无需互相依赖。

### `viso-gpu`

极薄 GPU RHI（backend 由 `viso_gpu::Backend` 按 cfg 静态选择，ADR 0029）。只依赖 `viso-handle`、`viso-macros` 与 `viso-gpu-abi`，不依赖 runtime（ADR 0042）：frame 节奏由 runtime 驱动 gpu，而不是反过来。

- device；
- queue；
- buffer；
- texture（`create_texture_mipped` 一次上传整条 mip 链；含外部画面：`import_external` 建一个原地采样 `ExternalImage` 的 texture，`adopt_external` 把同一 texture 指向同尺寸的新画面；Metal 以 `newTextureWithDescriptor:iosurface:plane:` 包装 IOSurface，每个 IOSurface 只包装一次（最近 16 个），采样中的 surface 记 use count 使其 pool 不往里写新画面，被替换的在其所在帧完成后才归还；D3D12 接收另一设备写入的共享纹理（`ExternalImage::DxgiShared`：纹理与共享 fence 的 NT handle、fence 值与 `ExternalHolds`），按 holds 缓存最近打开的 16 个，在 GPU 上等 fence 到值后 `CopyResource` 进自建的 sRGB texture（bind group 在创建时就记下了 resource，故复制而非换 resource），持有 hold 及所读的 resource / fence 直到复制所在的帧完成，生产者只写无人持有的槽；Vulkan 接收 Android 的 `AHardwareBuffer`（`ExternalImage::AndroidHardwareBuffer`，带画面在 buffer 中的区域与 holds）：设备支持 `samplerYcbcrConversion` 与 `VK_ANDROID_external_memory_android_hardware_buffer` 时按 holds 缓存最近 16 个导入的 image（外部格式、专用分配），按设备建议的模型/范围/色度位置每种编码建一次 YCbCr 转换、不可变 sampler 与管线，每帧在 upload 命令缓冲里从 foreign 队列取得所有权、以全屏三角形经转换采样画进自建 sRGB texture 的 UNORM 视图（`MUTABLE_FORMAT`），再交还所有权，hold 持有到该帧完成；转换 shader 是后端私有的冻结 SPIR-V；WebGPU 接收 `<video>` 元素（`ExternalImage::WebVideo`，借用其 wasm-bindgen 句柄），以 `copyExternalImageToTexture` 在 GPU 上复制当前画面进自建的 `rgba8unorm-srgb` texture（浏览器按视频的色彩标记做色彩管理，与页面自己显示 `<video>` 一致），复制排在已提交的帧之后，不经 CPU；不支持的 backend 返回 `None` / `false`，调用方回落为复制）；手头的 NV12 平面（`viso_handle::Nv12`：Y 与交错 CbCr 两个平面及各自行距、矩阵 BT.601/709/2020、全/有限范围、色度是否左对齐）经 `write_nv12` 写进 backend 自建的 texture（给定的同尺寸 texture 或新建一个）：Vulkan 把两个平面原样经 staging（`bufferRowLength` 带行距）上传进该 texture 自带的 R8 / R8G8 平面 image，再以全屏三角形用普通 shader 按 push constant 里的矩阵行（含范围与 8 位色度零点）及色度缩放/偏移（奇数尺寸向上取整、左对齐色度偏移四分之一样本）画进 sRGB texture 的 UNORM 视图，都在 upload 命令缓冲里，不依赖 YCbCr 扩展；不支持的 backend 返回 `None`，调用方在 CPU 上转换后复制；Vulkan 还接收线性 NV12 的 dma-buf（`ExternalImage::DmaBuf`：fd、长度、两平面的偏移与行距、矩阵/范围/色度位置与 holds）：设备有 `VK_KHR_external_memory_fd` + `VK_EXT_external_memory_dma_buf` 时把 fd 的副本导入为 transfer-source buffer（按 holds 缓存最近 16 个），从外部队列取得所有权后在 GPU 上按行距拷进平面 image 再同样转换，hold 持有到该帧完成；导入一次失败即视设备不收 dma-buf（有的驱动失败时也关掉了 fd，故不再关它）；
- sampler；
- pipeline；
- bind group；
- command encoder；
- surface；
- fences/sync；
- backend resource handles（每种资源一个 dense generational `SlotMap`）；
- present damage（`present_with_damage(rects)`，映射到 `EGL_KHR_partial_update` / `VK_KHR_incremental_present` / DXGI dirty rects / KMS `FB_DAMAGE_CLIPS`，§30.3）。

Backend 集合：Metal、D3D12、Vulkan、WebGPU、`Gles3`（Embedded-GPU，GLES 3.0+，§48.1）、`CpuRaster`（Embedded-CPU、headless 与无 GPU 环境，§30.4）。shader 编译器与 tooling 只需要 ABI 描述类型（`InstanceSchema`、`ShaderCode`、`BlendMode` 等）；它们由 `viso-gpu-abi` 定义，`viso-gpu` 整体 re-export 并按 backend 实现（ADR 0042）。transform root 的位置表不在 gpu，在 `viso-render`（§19.2）。

### `viso-gpu-abi`

GPU ABI 描述（ADR 0042），叶子 crate：instance layout 与 shader 声明的 `InstanceSchema`、用户 program 的 ABI / code / `CpuProgram` 接口、buffer/texture/sampler/pipeline 描述与格式、3D scene lane 的数据布局、typed generational resource handle（`BufferId` / `TextureId` / `SamplerId` / `PipelineId` / `BindGroupId` / `SurfaceId`）。不含任何 backend；`viso-shader`、`viso-image`、`viso-dev-protocol` 经它描述 GPU 数据而不链接 backend。

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
- font revision/incremental invalidation 与 profiler counters；
- 文本编辑模型（position、caret、hit test、selection、IME），供 `viso-ui` 的文本控件使用。

完整实现规范见 `Viso_Text_Font_Runtime.md`。

`viso-text` 是纯 CPU crate，只依赖 `viso-glyph-a8`，不依赖 `viso-gpu` / `viso-render` / `viso-platform`（ADR 0042）。glyph 的 GPU atlas texture/page/upload 归 `viso-render`，text 只产出 raster bitmap 与 residency 决策。crate 本身需要 `std`；shaping / segmentation 路径守 §50A.6 第 1 条：不直接使用 `std::thread`、`std::time`、`std::fs`、`std::sync::Mutex`，系统字体、worker 调度等 std 能力经 trait 注入。

### `viso-render`

- paint primitive；
- retained paint cache；
- clip/layer；
- batch builder；
- image/glyph atlas management；
- render graph；
- GPU upload plan；
- frame packet；
- transform root 的位置表：每个 root 一个 affine place（平移，或绕 root origin 的缩放/旋转），`Renderer::set_transforms` 经 view uniform 移动该 root 的已上传 draw，不改写 instance、不上传（ADR 0043，§19.2）；
- 3D scene lane（`scene3d`，ADR 0047；glTF 经 `viso-ende` 的 JSON reader 读取）；
- damage region（由 changed primitive range 的旧/新 bounds 合并而成，§30.3）。

几何与颜色基础类型直接使用 `viso-math`，不另定义同名类型。primitive/batch 前端与 GPU 提交后端分离：GPU backend 和 `CpuRaster` 消费同一份 frame packet。asset 解码：raster 经 `viso-image`，SVG 由 `viso-svg` 解析为规范化文档后在 `viso_render::svg` lower 为 primitive；asset 身份来自 `viso-schema::resource`。

详细规范见 `Viso_Rendering.md`。

### `viso-svg`

SVG 输入通道：bytes 一次性解析为规范化 vector 文档（`tree::Tree`），只依赖 `viso-math`，不依赖 renderer；不是逐帧 renderer。lower 为 retained `viso_render::Primitive` 流的一步在 `viso_render::svg`，因此 tooling 校验 SVG asset 不链接 render/gpu。

### `viso-pdf`

PDF 1.7 输出：页面是 `viso_render::Primitive` 流加按字形定位的文本 run，写成矢量路径、shading、Type 3 字体（带 ToUnicode，可选中复制）、透明组与 SMask；没有矢量形式的（图像、阴影、shader、scene）由调用方的 `Source` 栅格化后嵌入。只依赖 `viso-render` 与 `viso-math`，不依赖 ui/gpu；facade 的 `viso::pdf`（`pdf` feature，默认开启）在窗口的 store 里排版一棵分离子树、分页并提供字形与栅格。系统打印对话框不在核心，是 viso-ext 的 `viso-print`。

### `viso-image`

CPU 上的 raster 图像：PNG/APNG、JPEG、GIF、WebP（有损/无损/动画）、AVIF（静图、grid、序列；8/10/12-bit，按 nclx 或 AV1 序列头的 CICP 换算，PQ/HLG 色调映射到 SDR，`clap`/`irot`/`imir`；`avif` feature，纯 Rust AV1 解码，wasm 交给浏览器）、BMP、ICO、QOI asset 解码为 premultiplied RGBA8（header 先行检查尺寸上限，按 EXIF orientation 摆正）；动画（`Animation`）流式逐帧合成到一张画布（region + blend/dispose，或解码器自行合成的整帧），内存只有画布与当前帧，循环次数与帧时长按文件，≤10 ms 的帧按 100 ms 显示；构建期把图像转换为 target 采样的 texel 格式（`convert`：16-bit packed、BC1/BC3/BC7、ETC2、ASTC 4×4/6×6/8×8 的 Viso 自有纯 Rust 编解码器 `codec`，Mitchell 重采样 `resample`，SVG 构建期光栅化 `vector`）与其容器 `texture`（`VTX1`）——解码器同时是不支持该格式的设备的 fallback；headless snapshot 的 straight-alpha 图像及其 PNG 写出与感知比较。renderer 与 CLI 共用；格式词汇来自 leaf `viso-gpu-abi`，不链接任何 backend。依赖 `viso-schema`、`viso-gpu-abi`、`viso-svg`、`viso-math`。

### `viso-media`

经各 target 自带播放器播放媒体（ADR 0049）：`VideoPlayer` 打开文件、URL（平台自行拉取，含 HLS）或内存字节（只读文件的播放器写入临时文件，播放器释放时删除），打开时暂停；在 loop 线程上 `poll` 就绪（尺寸、时长、帧率、有无声音、可否 seek）/ 结束 / 失败 / 等待数据，`frame` 只在画面变化时交出当前应显示的一帧（BGRA 或 RGBA 行，YUV 矩阵、范围与 HDR 映射已由平台或播放器完成；或 `Frame::planes()` 交出的 NV12 平面，交 GPU 转换，`copy_rgba` 时在 CPU 上按其矩阵与范围转换）：画面所在的平台内存以 `Frame::native()` 交出为 `viso_handle::ExternalImage`，`copy_rgba` 才把它映射到 CPU 复制出来（不能映射的纯 GPU 帧由 `Frame::external` 构造，只能被采纳）；play / pause / seek（暂停时也显示目标帧）/ rate / volume / muted / looping。解码、声音输出与音画同步都在平台。macOS / iOS 走 AVFoundation（`AVPlayer` + `AVPlayerItemVideoOutput`，IOSurface 背书的 BGRA pixel buffer）；Windows 走 Media Foundation 的 `IMFMediaEngine`（独占一条 MTA 线程，经自建在高性能 adapter 上的 D3D11 设备硬解；新帧 `TransferVideoFrame` 进至多 4 个 NT handle 共享纹理中无 backend 持有、播放器也未交出的一个，写完 signal 共享 fence 并 flush，交给 D3D12 原地读取；只有暂停时要的画面、或调用方要过行数据之后，才经 staging 纹理读回到循环复用的缓冲；设备不支持共享 fence 时每帧读回；位置无锁共享）；Linux / BSD 走 GStreamer `playbin` + `appsink`（运行时 dlopen，未安装时只是视频报 `Unsupported`；全部调用在 loop 线程且不等待，bus 在 poll 时读取，appsink 先要线性 NV12 的 `memory:DMABuf`（`DMA_DRM` 与旧式 caps，有 libgstallocators / libgstvideo 时），再要内存里的 NV12 / BGRA（解码器原样输出或只重排交错，不在 CPU 上转 RGB），并在 sink pad 的 allocation query 里声明支持 `GstVideoMeta`，上游带 padding 的 buffer 原样交来、按 meta 的偏移与行距读；dma-buf 帧以 `ExternalImage::DmaBuf` 交 GPU 原地读，holds 以 qdata 挂在 GstMemory 上随它释放，backend 仍持有的帧留住其 sample 不还给 pool；backend 不收时映射出平面并改要内存里的 NV12；按 caps 的 `colorimetry` / `chroma-site` 读出矩阵、范围与色度位置（未写时按高度取 BT.601/709、左对齐），平面以 GStreamer 写入的映射内存原地交出、不再复制，交 GPU 转换；调用方复制过一次行数据后 appsink 改要 BGRA 并发 reconfigure 重新协商，之后由 GStreamer 的转换器出行数据，暂停时取 preroll 显示 seek 目标帧，关闭在独立线程完成以免等网络）；Android 走系统 `MediaPlayer`（经 APK 内的 `dev.viso.media.VisoVideo` Java 类驱动，框架把 activity 交给 `attach_android` 一次后才能打开；解码、出声、音画同步、HLS 与保调变速都在系统，报告在 Java 主线程投递进播放器的 channel；画面画进 NDK `AImageReader`（YUV_420_888，GPU 采样 + CPU 读用途，至多 5 张），每张 image 的 hardware buffer 交给 Vulkan 原地转换，每个 buffer 一份 holds，换下的 image 在无人持有后才 delete；只有调用方要复制时才按裁剪区转成 RGBA；内存字节经 direct `ByteBuffer` + `MediaDataSource` 原地读取，不写临时文件）；Web 走页面里隐藏的 `<video>`（浏览器自行拉取、解码、出声；`requestVideoFrameCallback` 报出新呈现的画面后才交出，交给 WebGPU 在 GPU 上复制，backend 不收时才画进 `OffscreenCanvas` 读回 RGBA 行，内存字节经 blob URL，自动播放被拒时保持暂停）；其余 target 暂报 `Unsupported`。播放器报告变化时 facade 立即再跑一帧，让控件拿到时长与位置。扩展提供的实时画面（摄像头、屏幕、通话）实现 `LiveSource`（poll / frame / play / pause），由 `VideoPlayer::live` 或已装的 provider 打开，经同一路径出帧：无时长、位置恒为零、不可 seek、无声音。摄像头（ADR 0050）作为实时视频经同一个 `VideoPlayer` 打开，由扩展经 `provide_cameras` 装入的 `CameraProvider` 应答（先装者生效；未装时打开报 `Unsupported`，`cameras()` 为空）：`VideoSource::Camera(CameraRequest { camera, width, height, frame_rate })`（`CameraChoice::Default` / `Facing` / `Id`，默认 1280×720、30 帧），`cameras()` 列出 `CameraInfo { id, name, facing }`；首帧到达即就绪（无时长、不可 seek、无声音，`VideoInfo::facing` 报朝向），play 即采集，pause 释放设备（画面停在最后一帧），seek / rate / volume 无效；用户拒绝授权报 `VideoError::Denied`。viso-media 为扩展每个 target 公开一个出帧 hook，使采集的画面走与视频相同的原地采样路径：Apple `Frame::pixel_buffer`（IOSurface 背书的 `CVPixelBuffer`）、Android `ImageReader`（`AImageReader` hardware buffer，带转正所需的旋转数）、Windows `SharedCanvas` / `SharedPictures`（解码用的 D3D11 设备、共享纹理槽与 fence）、Linux / BSD `gstreamer_live`（源元素与 caps，经 appsink 路径）、Web `MediaStreams` 经 `web_live`（隐藏的 `<video>`）；这些 hook 是 viso-media 的 public 合同。各平台的采集实现（设备枚举、模式选择、授权）在 viso-ext 的 `viso-camera`，见 ADR 0050。叶子 crate，只依赖 `viso-handle`。

### `viso-ui`

- NodeArena / `NodeStore`（`NodeId` 外部句柄，`NodeIndex` 内部链接，§10.4.7）；
- component runtime；
- state slots；
- reactive invalidation；
- layout；
- style；
- input/focus/gesture；
- semantics；
- animation；
- node-scoped timer 与 task（`timer`、`task`）；
- paint node bridge（含 retained drawing `Content::Drawing`、plot、video 等 node content）；
- resident worker 池（`viso_ui::work`，facade 按 footprint 定大小，无线程处 inline 执行）；
- Release AOT package loader（`crates/ui/src/aot`，ADR 0016）；package 是 8 字节对齐的 section 布局，`AotView` 在 mmap / XIP 的原地字节上一次校验、逐条解码 node 与 edge，不拷贝（§50A.6 第 5 条）。

Layout / hit test / transform 的几何类型来自 `viso-math`（`ui -> math`，ADR 0042）；文本编辑模型来自 `viso-text`；`viso-schema` 的数据类型在原路径 re-export。

### `viso-widgets`

官方 UI kit：

- primitive controls；
- layout containers；
- navigation；
- overlays；
- adaptive UI；
- desktop shell；
- theme defaults。

只依赖 `viso-ui` 与 `viso-math`。PDF/browser/chart/map/code editor 等不是基础 widget：图表、地图、代码编辑器在 `extras/`（见下），PDF 输出是 `viso-pdf` + `viso::pdf`，不平铺进核心 widgets。

### `viso-lexer`

`.vs` tokenizer，叶子 crate：源码到扁平、lossless 的 token 流（`SyntaxKind`、`Token`、`LexError`、`TextRange` / `LineIndex`），错误 span 不中断扫描；`LexState` 支持逐行续扫（跨行只带块注释深度或 raw string 的 `#` 数）。`viso-dsl` 从它解析并在原路径 re-export，`viso-code-editor` 用它逐行高亮，因此编辑器不链接编译器。

### `viso-dsl`

- tokenizer（来自 `viso-lexer`）/CST/AST/HIR；
- type checker（schema 来自 `viso-schema` 的纯数据，不来自 `viso-ui` 的 Rust 类型，ADR 0042）；
- module graph 与 filesystem package loader（`package`）；
- UI IR / Binding IR（ADR 0014）、Shader IR（ADR 0017）；
- transactional hot reload 的 compile 侧（`crates/dsl/src/hotreload`：plan → diff → migrate，产出 `viso-dev-protocol` 的 patch 数据，ADR 0015）。commit（修改 retained state）是 apply 侧职责，只在 `viso-view` 的 dev 模块（ADR 0042）；进程内驱动 commit 的只有测试；
- Release AOT emitter（ADR 0016）：UI IR / Binding IR 以 compact asset 输出，handler / binding evaluator / computed 以生成的 Rust 源码输出（ADR 0041）；
- source map 与 inspector 源码表（`inspect`）；
- formatter（`format`，`viso-lsp` 与 `viso fmt` 共用）、structured edit（`edit`，按 `SymbolId` / `SyntaxId` 定位）、schema query（`schema`，`viso schema`）；
- i18n 消息目录（`i18n`）、headless game scenario（`scenario`）、单向导出（`export`：HTML 页面、SolidJS 项目）；
- Behavior IR → bytecode lowering（`Program::bytecode`，ADR 0035），只服务 Dev 热更新与显式 dynamic；
- Native Package（ADR 0051）：扩展 crate 在 Cargo manifest 中声明的 Native Library，以 `viso-natives` JSON Schema（`viso_behavior::native::schema`，包的测试以 `viso::native::check` 保持同步）读入为 stub；`native_package::link_with` 先按标准 Registry 编译，出现 `E2001` 时经 `cargo metadata` 发现 App crate 直接依赖中的包并重编译，`view!`/`component!`、`viso check`/`export`/`dev`/`schema` 与 LSP 共用。展开把模块调用到的包的 Rust Library 交给 `viso-view` 的 Host，Host、其 Game 与 Release View Package 以 `Natives::shared`（每组 Library 一个进程内共享 Registry）链接。框架不认识任何扩展。网络不是 DSL Library：`.vs` 调用应用 workspace 中 natives crate 的领域 Native，其 Rust 实现使用 `viso-ext/viso-net`。

`viso-dsl` 只依赖 `viso-lexer`、`viso-schema`、`viso-dev-protocol`、`viso-ende`、`viso-behavior`、`viso-shader` 与叶子 `viso-math`（Web 导出按其缓动曲线采样 CSS）；集成测试另 dev-depend `viso-ui`、`viso-view` 以挂载编译结果。传递依赖闭包不含 `ui`/`render`/`gpu`（backend）/`runtime`/`platform`，这样 LSP、`viso check` 与 CI 分析不链接运行时栈（`check-deps` 的闭包规则执行）；任何 app artifact（含 dev artifact）都不链接 `viso-dsl`：`view!` 在构建期展开，dev artifact 应用 host 编译好的 patch（`check-release-absence` 检查）。Shader IR 的唯一 owner 是 `viso-shader`：`viso-dsl` 解析、检查 `.vs` 中的 shader 声明（与其余 DSL 共用 CST、resolver 与诊断），lower 为 `viso_shader::program::Program`，validation 与 codegen 由 `viso-shader` 完成。

### `viso-behavior`

Behavior bytecode 格式、verifier 与 interpreter（ADR 0035）：寄存器 VM、8 字节指令、事务 undo log、执行预算与 Runtime Fault（`E7101`–`E7105`）；另有 game world、i18n 格式化（`dates` feature）与 audio host。依赖 `viso-ende`、`viso-math`、`viso-schema`；`no_std` + `alloc`（`std` feature 默认开启）；`runtime` / `ui` / `widgets` 不得依赖它。

解释器（`interpreter` feature，`viso-view`/`viso` 的 `vm`）用途限定为 Dev 热更新 patch、显式 `dynamic` 构造、sandbox/AI preview。`aot::lower_module` 把模块的每个 chunk lower 为 Rust（task 为可恢复状态机，`TaskFrame` + `Poll`），`Vm::install_native` 按 build hash 安装；生成代码仍在 VM 的运行时外壳上执行——事务与 undo log、预算、fault 位置（报告 bytecode pc）、computed memo、native、事件与 task 启动——所以两条路径的语义只有一份（ADR 0041）。不带解释器的构建中没有编译代码的 chunk 报 `E7105`。

### `viso-view`

Compiled view 的运行时（ADR 0037–0041）：handler table、state layout 与镜像、control response、control-flow region、task/resource/persist/theme 接线、named node（`NodeRef`）请求、game 与 world scene 接线；dev 模块（`hot-reload`）是 hot reload 的 apply 侧，`inspect` feature 记录每个 mount 的来源。三种入口（宏、热更新 commit、release package）都经它挂载，因此 handler 的执行语义只有一份。

Release lowering（ADR 0041）：
- 每个 chunk 是生成的 Rust 函数：`view!` 在宏展开时 lower 模块并把代码嵌在模块字节旁（`viso::__behavior!` 在 release 或无 `vm` 的构建中保留代码与去掉 bytecode 的 `Module::compiled()` 字节，debug + `vm` 时只保留完整 bytecode、代码不编译）；view package 带 bytecode，`viso_dsl::aot::package_native_code` 生成代码由 app 编译，经 `ViewLoad::native` 交给 loader，`viso_dsl::aot::compile_package` 给出与之配套、去掉 bytecode 的 package；
- 富类型 state（字符串、列表、记录）没有 UI cell，实例保留自己的 state 向量；镜像到 UI cell 的标量 state 按 `StateStore::epoch` 门控：cell 与实例 revision 都未变时 sync 不拷贝任何 cell，dispatch 对镜像数是 O(1)；
- Dev 下解释执行与 Release 生成代码共享同一 handler table 布局（entry → chunk 索引），热更新只替换 table 条目。

### `viso-schema`（ADR 0042）

DSL compiler 与 runtime 之间交换的**纯数据合同**：widget / property / event / slot 描述与标准 widget 表、`DirtyClass`、state 身份与值（`StateKey`、`StateValue`）、role / interaction / layout / env 词汇、length 项、resource 身份与 asset 格式（`ResourceId`、`ResourceRevision`）、`StableId`、值类型 schema 与 `Retyping`、AOT package 与 view package（control / route / region 表）格式。`no_std + alloc`，只依赖 `viso-ende`。`viso-ui` / `viso-view` / `viso-behavior` 在原路径 re-export，`viso-dsl` 消费它。它不是 `core/common`：只放跨编译边界的数据类型及其 codec 与定义其含义的小纯函数，不放运行时逻辑。

### `viso-dev-protocol`（ADR 0042）

`viso`（CLI）与 dev runtime 之间的开发链路：framed host/runtime message 与其携带的 patch bundle（UI / game section）、asset blob、game query，以及 running app 报告给工具的数据——inspection census / node detail / event log / source table / selector、profile frame、headless scenario。两侧都依赖它，CLI 因此不链接 runtime；runtime 侧的采集（census、node detail）留在 `viso-view`。依赖 `viso-schema`、`viso-ende`、`viso-math`、`viso-gpu-abi`。

### `viso-lsp`

`.vs` formatter 与 language server（ADR 0018）：纯 analysis engine（goto-definition、find-references、rename、formatting，无协议依赖）+ 薄的同步 stdio JSON-RPC frontend，只依赖 `viso-dsl`；格式化算法本身是 `viso_dsl::format`。

### `viso-cli`

`viso` 命令（`Viso_CLI.md` §39–§41）：参数解析、project/config resolution、调用 `viso-project` / `viso-package` / `viso-dsl`、human 输出与 JSON event stream（经 `viso-ende` 的 `JsonWriter`）、exit code；不实现任何 domain service。

CLI 依赖 `viso-project`、`viso-package`、`viso-dsl`、`viso-dev-protocol`、`viso-schema`、`viso-ende`、`viso-image`、`viso-svg`、`viso-text`，传递依赖不含 `runtime` / `platform` / GPU backend（`check-deps` 闭包规则）。需要运行 app 的命令（`run`、`snapshot`、`test`、`inspect`、`profile`、`scenario`）启动构建出的 app artifact 子进程（headless 命令用 `headless` feature 的 build，在 app 进程内经 `CpuRaster` 绘制），通过 `viso-dev-protocol` 通信；asset 校验、图标与 snapshot 比较只用 `viso-image` / `viso-svg`，字体经 `viso-text` 的 sfnt 解析。glyph 预烘焙不在 CLI，由 `packaged_fonts!`（`viso-macros` + `viso-glyph-a8`）在 app 构建期完成。

### `viso-project` / `viso-package`

两者都不依赖任何内部 crate。`viso-project`：项目发现、`Viso.toml` 与 resolved configuration、device target、构建身份（`BuildId`）与缓存布局，纯数据，`viso --help` 与 `viso config show` 不需要初始化框架。`viso-package`：运行各平台自己的工具链，把构建好的 app 包成 iOS `.app`、Android APK、web bundle、Windows 带图标与版本的可执行文件 / MSIX 与签名、Linux desktop entry、可复现的 zip/tar，并合并 app 链接的扩展携带的平台片段（manifest、Info.plist、entitlement、service worker）。`xtask` 的 `bundle` 与 `check-release-absence` 也使用它。

### `viso-services`

核心 app service protocol（ADR 0031 修订）：haptics、launcher（打开 URL），加 `app_data_dir` / `LocalStorage` 供持久化；`Reply<T>` 一次性应答，`Services` 注册表，`Mock` 供 headless 测试。非 UI 能力（文件对话框、分享、本地通知、安全存储、权限、定位、网络）是 `viso-ext` 插件 crate，各自携带 OS 实现与 mock，共用 `viso-common` 的 `Reply`。

只在 Android 上依赖 `viso-platform`（JNI 调用经 platform 持有的 activity）；不依赖 runtime。低频 service 可以使用 trait object，避免把所有 OS 逻辑塞回 platform/runtime。

### Extras：`viso-charts` / `viso-map` / `viso-code-editor`

选装的大型控件，不进默认 widget 依赖图（AGENTS §33），facade 不依赖它们，app 直接依赖；测试经 facade headless 驱动（dev-depend `viso`）。

- `viso-charts`（`extras/charts`）：折线/面积/柱状/散点图与 sparkline，一个 plot 节点绘制；依赖 ui、widgets、math。
- `viso-map`（`extras/map`）：矢量地图视图；瓦片在 `viso_ui::work` 上一次三角化为 retained drawing，平移/缩放/旋转只移动 transform root；倾斜复用 scene3d lane。依赖 ui、widgets、render、text、math、services（瓦片的 `Reply`）、ende（GeoJSON）。瓦片格式读取、网络源、路线与导航在 viso-ext（`viso-tiles`、`viso-map-nav`、`viso-map-build`）。
- `viso-code-editor`（`extras/code-editor`）：rope 文档、多光标、undo、`.vs` 高亮（`vs` feature，`viso-lexer`）、折叠、换行、inlay、查找替换、`LanguageService`；`lsp` feature（仅桌面）提供经 stdio 的 `LspClient`（`viso-ende` 的 JSON）。依赖 ui、lexer、ende；测试另 dev-depend `viso-lsp`。

---

## 10. Crate 依赖 DAG

**当前合同**是 `xtask/src/main.rs` 的 `allowed_edges()` allowlist，由 `cargo xtask check-deps` 在 CI 执行；不在表中的 workspace 内部边一律拒绝。本节与该 allowlist 不一致时以 allowlist 为准，并应同步修正本节。

| crate | allowlist（normal） | 另允许的 dev-dependency | 备注 |
|---|---|---|---|
| `viso`（facade） | runtime, ui, ui-macros, widgets, view, behavior, services, render, image, media, gpu, platform, text, shader, macros, ende, math, pdf | dsl, dev-protocol | DSL 测试自己编译 `.vs` 并用 dev link 的 patch 重载，app 不链接二者；`pdf` 是可选依赖（`pdf` feature，默认开启）；`behavior` 的解释器只在 `vm` feature 下编入（ADR 0041） |
| `viso-widgets` | ui, math | — | |
| `viso-charts`（extras） | ui, widgets, math | viso | |
| `viso-map`（extras） | ui, widgets, render, text, math, services, ende | viso | bench 用 criterion |
| `viso-code-editor`（extras） | ui, lexer, ende | viso, lsp | lexer 在 `vs` feature、ende 在 `lsp` feature 下 |
| `viso-ui-macros` | dsl | — | |
| `viso-lsp` | dsl | — | |
| `viso-cli` | project, package, dsl, dev-protocol, schema, ende, image, svg, text | — | |
| `viso-dsl` | lexer, schema, dev-protocol, ende, behavior, shader, math | ui, view | |
| `viso-view` | ui, schema, dev-protocol, behavior, ende | — | `behavior` 的解释器只在 `vm` feature 下编入（ADR 0041） |
| `viso-behavior` | ende, math, schema | — | |
| `viso-services` | platform | — | manifest 只在 Android 上依赖 platform |
| `viso-ui` | render, runtime, ende, text, math, schema | — | |
| `viso-render` | math, ende, schema, text, image, svg, shader, gpu | — | ende 用于 glTF JSON |
| `viso-pdf` | render, math | — | |
| `viso-svg` | math, ende | — | `ende` 只为 data URL 的 base64 |
| `viso-image` | schema, gpu-abi, svg, math | — | |
| `viso-media` | handle | — | 各 target 的媒体栈是外部依赖 |
| `viso-text` | glyph-a8 | — | |
| `viso-shader` | gpu-abi, ende | — | |
| `viso-gpu` | handle, macros, gpu-abi | — | |
| `viso-runtime` | platform | — | |
| `viso-platform` | handle | — | |
| `viso-dev-protocol` | ende, math, schema, gpu-abi | — | |
| `viso-schema` | ende | — | |
| `viso-macros` | glyph-a8 | — | |
| `viso-gpu-abi` / `viso-math` / `viso-ende` / `viso-handle` / `viso-glyph-a8` / `viso-lexer` / `viso-project` / `viso-package` | —（叶子） | — | |

`xtask`、`examples/*`、`benches/*`（`viso-footprint`）与 `libs/*`（`viso-woff2`）在 DAG 之外：它们是应用、checker 自身或无框架依赖的独立库，没有任何框架 crate 依赖它们（框架 crate 一旦命名它们，allowlist 照样拒绝）。`tools/*` 只能依赖框架，框架依赖 `tools/*` 由 `check-deps` 以单独的 FORBIDDEN EDGE 报出（CLI §41）。

ADR 0042 的调整（已完成）及其理由：
- `gpu -> runtime` 与 `text -> gpu` 在源码中没有任何引用，属于历史残留边；它们让 runtime 位于 gpu 之下，也让纯 CPU 的 text 被迫链接 GPU backend。已删除。
- `dsl -> ui` 让 LSP / `viso check` 传递链接 render、gpu、runtime、platform。改为 `dsl -> schema` 后，编译器与运行时只通过纯数据合同耦合；dev 链路的协议数据单独成 `viso-dev-protocol`，CLI 与 runtime 共用而互不依赖。
- `shader -> gpu` 让编译器链接 GPU backend；ABI 描述拆为 `viso-gpu-abi` 后，shader 只描述 GPU 数据。
- `svg -> render` 让 CLI 为校验 SVG 链接 renderer；SVG 解析与 lowering 拆开后，`render -> svg`，CLI 只用解析器。
- `math` 是几何类型的唯一 owner；ui/widgets 直接使用它，render 删除重复的 `Rect`/`Point`。

`viso-ende` 被 schema、behavior、shader、dev-protocol、dsl、view、ui、render、cli、facade 与 extras（map、code-editor）使用，不位于 frame 数据流主链。

### 10.1 禁止依赖

以下边无论 allowlist 如何演进都不得出现（与 AGENTS.md §3.5 一致）。标 ★ 的边同时列在 `xtask` 的 `NEVER` 表：allowlist 若加入它们，`check-deps` 也失败。

- `platform -> ui / widgets / dsl` ★、`platform -> Studio`；
- `ui -> widgets` ★、`ui -> dsl` ★；
- `render -> widgets` ★；
- `gpu -> ui` ★；
- `text -> platform` ★（系统字体经 `viso-text` trait 由 facade 实现，ADR 0026）；
- `ui / runtime -> Studio / tools`；
- `viso-dsl -> concrete widget implementation`：原则上禁止，使用 schema/registry；
- framework core crate 依赖 app/example：绝对禁止；
- `viso-ende` / `viso-math` 依赖任何框架 crate：禁止，二者保持叶子；
- `runtime / ui / widgets -> behavior` ★：禁止，Pure Rust UI 不依赖 DSL 执行器；
- `viso-math` public ABI 依赖 pointer width、`usize` 或 backend SIMD type：禁止；
- `viso-math` struct 内存布局直接作为 GPU uniform/instance wire ABI：禁止；
- frame hot path 通过 Ende encode/decode 传递 Node/Layout/Paint 数据：禁止；
- `gpu -> runtime / platform` ★：禁止（ADR 0042）；
- `text -> gpu / render` ★：禁止；
- `schema -> ui`、`dev-protocol -> ui` ★：禁止，纯数据合同不反向依赖运行时；
- `viso-dsl` / `viso-lsp` / `viso-ui-macros` / `viso-cli` 的传递依赖包含 `runtime` / `platform` / `gpu`：禁止（`check-deps` 的传递闭包检查执行）；
- `shader -> gpu`、`svg -> render`、`dsl -> ui / view`、`cli -> view` ★：禁止（ADR 0042）；
- 同一几何概念在 `viso-math` 之外重复定义 public 类型：禁止（`check-deps` 扫描 `pub struct` 名，见 §10.2）；
- Release artifact 链接 `viso-behavior` 的解释器（`interpreter` feature）：禁止（app 显式打开 `viso/vm` 以加载 bytecode view 时除外，ADR 0041）。`viso-behavior` 本身是 VM 运行时外壳，release 与 embedded 构建都需要。`vm` 不在 facade 默认集内，CLI 只为 Dev build 加它；`check-profiles` 拒绝任何 release 闭包打开 `viso-behavior/interpreter`（报告打开它的 feature 链），`check-release-absence` 要求 dev artifact 含解释器循环的字符串、release artifact 不含。

### 10.2 边界必须由 CI 机器化验证

AGENTS.md 里的 dependency 规则不是“建议”。`cargo xtask check-deps`（CI 每次运行）基于各 crate `Cargo.toml` 验证：

- 每条内部边都在 allowlist 中（dev-dependency 单列，不进入 crate 自身闭包）；
- allowlist 不含 `NEVER` 表中的边（§10.1 ★）；
- 框架 crate 不依赖 `tools/*`；
- compiler/tooling 侧（dsl/lsp/ui-macros/cli）普通依赖的传递闭包不含 runtime/platform/gpu，命中时给出路径；
- 每个 workspace member（DAG 之外的目录除外）都在 allowlist 中且目录一致；
- `viso-math` 之外没有名为 `Rect`、`Point`、`Size`、`Insets`、`Vec2/3/4`、`Affine2`、`Mat2/3/4`、`Quat`、`Transform3`、`Aabb`、`Ray`、`Plane` 的 `pub struct`（布局的尺寸请求是 `viso_ui::Sizing`，地图相机的边距是 `viso_map::Padding`）；
- module 级 import 规则（`xtask/src/imports.rs`，用 `syn` 解析源码，`#[cfg(test)]`/`#[test]` 除外）：platform、ui、text、render、gpu、dsl 不得经路径或 `extern crate` 指到 `NEVER` 表中的 crate；`viso-ui` 的 arena/reactive/layout 模块与 `viso-text` 的 shaping/segmentation 模块不用 `std::thread`/`std::time`/`std::fs`/`std::sync::Mutex`（§50A.6）；ui 与 render 的帧热路径模块不用 `viso_ende`；DSL 的 `syntax`、`ast` 不指向后续阶段，`resolve` 与 `hir` 不指向消费 IR 的 `aot`/`export`/`hotreload`/`dump`/`frontend`（HIR 遍历在检查时把 behavior 与 view lower 为 IR，二者同处一遍）；gpu 的 backend 合同与 CPU 光栅不指名任何原生 backend。

其余 xtask gate：

| 命令 | 检查 | CI |
|---|---|---|
| `check-release-absence` | release artifact 不含 dev 层（dev 环境变量、patch 检查、dev link 线程/类型、失败 overlay、snapshot 端点、behavior 解释器循环）：dev 构建必须含全部 marker 且不含 `viso_dsl` 符号，`VISO_PROFILE=shipping` + `hot-reload` 必须被 facade build script 拒绝，release 构建一个 marker 都不能有；host 上还启动两者，dev 必须连上、release 必须不连；可选 `--target ios-sim/android/web` | 是（macOS、Linux、iOS sim、web、Android） |
| `mcu-smoke` | `thumbv7em-none-eabihf` 上 `--no-default-features` 检查 `viso-math`、`viso-ende`、`viso-schema`、`viso-behavior`（另含 `interpreter`）、`viso-glyph-a8`（§50A.6） | 是 |
| `check-frame-tax` | `frame_loop` bench 在 release 下有无 `hot-reload` 交替运行（默认 5 轮），dev 帧最多比普通帧贵 10% + 250 ns | 是 |
| `check-benches --against <rev>` | `rev` 的 detached worktree（独立 target 目录）与工作树在同一台机器上跑 `frame_loop`、`list_scroll`、`paint_scale`（viso）与 `reactive_binding`、`grid_layout`、`large_list`（viso-ui）：每个 bench target 两侧背靠背运行、先后顺序逐轮交替（默认 4 轮），每轮取两侧 criterion 中位数之比，按各轮比值的中位数判定；工作树最多慢 15%，另加 `rev` 最好中位数上 20 ns | 是（对 PR 的 base 或 push 前的提交） |
| `check-profiles` | 以 `viso build --release` 解析的 feature 集（embedded 板卡：facade 默认 feature 关闭，加 profile 的 feature 与 `rasterizer`，`embedded-gpu` 分 GLES 与 Vulkan 两种板卡；desktop：facade 默认集）为 aarch64 与 armv7 Linux 解析 facade 的 normal 依赖闭包（不含 proc-macro 子树），检查禁止项：Linux 桌面栈（`x11rb*`、`wayland-*`、`zbus*`、`accesskit*`）、DSL 编译器与 hot reload watcher（`viso-dsl`、`viso-lsp`、`notify*`、`kqueue*`、`inotify*`）、其他平台的图形与系统 API（`windows*`、`objc2*`、`metal`、`wgpu*`）；GLES 板卡另禁 `ash`，Vulkan 板卡另禁 `glow`、`khronos-egl`，`embedded-cpu` 禁一切 GPU API（另加 `ash`、`d3d12`、`glow`、`khronos-egl`）。desktop 只禁 DSL 编译器与 watcher。任何 profile 都不得打开 `viso-behavior/interpreter`。命中的 crate 连同依赖路径、命中的 feature 连同打开它的 feature 链一起报告。`viso-behavior` 是 `no_std` 的必需依赖，不在禁止项内；fontconfig 不是 crate 依赖（运行时 dlopen，embedded 构建经 `cfg(viso_no_system_fonts)` 编译掉，§50A.4） | 是 |
| `check-targets` | 对 8 个 app target（macOS、iOS、iOS sim、Android arm64/armv7、wasm32、Windows、Linux）跑 `clippy -D warnings`；cross target 排除只在开发机运行的 xtask/project/cli/lsp，wasm 不含 bench | 是（macOS） |
| `footprint` | 参考 app 在各 footprint profile 下的启动、内存、idle 唤醒、帧时间与 GPU 内存，与 Makepad 对照；`--check` 对 `benches/footprint/baseline.json` 超出容差即失败（另一台机器上只作趋势）；`--against <rev>` 在同一台机器上与 `rev` 的 worktree 成对测量，体积与计数堆用更紧的成对容差（体积 1% + 4 KiB，堆 5% + 64 KiB） | `--against` 是（headless 两个 profile） |

`cargo xtask bundle` 是打包工具，不是 gate。

目标（尚未实现）：

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

| 类型 | 语义 | 表示 | 跨编译稳定 | 所在 / 主要路径 |
|---|---|---:|---:|---|
| `NameId` | 当前编译会话中的 interned identifier（按 NFC 归一） | `u32` | 否 | `viso_dsl::resolve`，compiler |
| `SymbolId` | 稳定源码/Schema/声明身份 | 128 bit `{hi, lo}` | 是 | `viso_dsl::resolve`，compile/link/hot reload/structured edit |
| `StableId` / `StateKey` | `SymbolId` 跨编译边界的纯数据形式（同一 `{hi, lo}` 布局） | 128 bit | 是 | `viso-schema`：behavior module、retype、persist、state store 的 cold side map |
| handler index / `Route` | handler table 下标 + 事件 tag + capture 位 | `u32` + enum | artifact 内 | `viso_schema::view`，hot（dispatch） |
| `ProgramSlot` | renderer 中 user program 的槽 | `u32` | 否 | `viso_render`，hot/warm |
| `BufferId` / `TextureId` / `PipelineId` / `SamplerId` / `BindGroupId` / `SurfaceId` | GPU resource handle | `u32 index + u32 generation` | 否 | `viso-gpu-abi`，hot |
| `NodeId` | 具体 Runtime Node 实例的外部句柄 | `u32 index + u32 generation` | 否 | `viso_ui`，hot |
| `NodeIndex` | arena 内部链接（tree link、binding target、dirty worklist、paint owner） | `u32` | 否 | `viso_ui`，hot |
| `StateId` | state cell 实例 | `u32 index + u32 generation` | 否 | `viso_ui`，hot |
| `StableKey` | 动态集合中业务对象身份 | typed value | 由业务语义决定 | reconciliation |
| `FileId` | dev link / LSP 中的源码文件身份 | `u32` | session/artifact 内 | `viso-dev-protocol`、`viso-lsp`，cold/warm |
| `ResourceId` + `ResourceRevision` | asset 身份（规范化 `assets/` 路径的 FNV-1a-64）与其 payload 版本 | `u64` / `u32` | 是（按路径） | `viso_schema::resource`，warm |

Property、Event 与 component type 没有 runtime dense ID：编译期把它们完全消解（§10.4.6）。

禁止创建如下通用类型并让所有 subsystem 共用：

```rust
pub struct Id(u64);
pub struct VisoId(u128);
```

类型系统必须能阻止：

```text
ComputedId 被当作 EffectId
StateId 被当作 NodeId
FileId 被当作 ResourceId
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

首先进入 compiler interner（`NameInterner`）：

```text
"Button"      -> NameId(21)
"text"        -> NameId(44)
"click"       -> NameId(62)
"save_button" -> NameId(91)
```

实现：

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NameId(u32);
```

规则：

- `NameId` 只保证当前 compiler/database session 内有效；
- 不写入持久 cache 作为稳定 ABI；
- 不跨进程直接传输；
- 不作为 Runtime Property/Event/Node identity；
- string interning 发生在 parser/name-resolution 冷路径；按 NFC 归一，两种归一后相同的拼写是同一个名字；
- AST/HIR 中优先携带 `NameId`/resolved symbol，而不是重复存储 `String`。

Compiler 保留反查：

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

物理表示：

```rust
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId {
    pub hi: u64,
    pub lo: u64,
}
```

跨编译边界（behavior module、state store、persist）时以同布局的 `viso_schema::StableId` / `StateKey` 出现，runtime 只比较与 hash，不解释其位。public/internal ABI 不把 `SymbolId` 定义成“需要 128 位整数算术”的数值类型。

`SymbolId` 只需要支持：

- equality；
- hashing；
- ordering（需要 deterministic output 时）；
- Encode/Decode；
- debug formatting 与 32 位 hex 文本形式（structured edit、LSP）；
- stable cache key composition。

禁止：

```rust
symbol + 1
symbol * 2
```

#### Stable Symbol 生成输入

Canonical symbol identity 由以下信息组成（`SymbolIdentity`，每项带长度前缀，拼接不会碰撞）：

```text
package identity
module path
declaration kind（SymbolKind，判别值是指纹合同的一部分）
canonical declaration path
```

`@stable("…")` 显式指定身份，使声明在受控 rename/move 中保持身份。

例如：

```text
my_app
+ features::home
+ Node
+ HomePage::view::save_button
```

经过**固定、版本化、跨平台确定**的 128-bit fingerprint 算法生成 `SymbolId`：自有的 FNV-1a-128（标准 offset basis 与 prime，两条 `u64` lane），`FINGERPRINT_VERSION` 作为第一个 chunk 混入流中，由已知答案测试钉住。

硬规则：

- 禁止使用 `std::collections::hash_map::DefaultHasher` 生成持久 Symbol；
- 禁止使用进程随机 seed；
- 禁止把源码 byte offset 作为主要稳定身份；
- 普通格式化不改变 Symbol；
- 文件物理移动但 canonical module path 不变时不应改变 Symbol；
- 显式 stable annotation 可以在受控 rename/move 中保持身份；
- compiler/linker 必须检测同一 artifact 内的 Symbol collision，并将其视为构建错误（`E5104`；热重载中 `@stable` 撞上已有身份报 `E5102`，旧版本继续运行）；
- fingerprint algorithm/version 不能静默改变：版本混入指纹，改动任何常量都要升版本。版本（`viso_schema::FINGERPRINT_VERSION`）同时写入持久 artifact：view package 与 persist 存的每个值都在 magic 后带它的 LE `u32`，读取方遇到不同版本报 `DecodeError::Version`（`load_view` 提示重建 package，`Persistence::load` 报告该值不载入），而不是把全部身份当作新声明；版本记录之前写的 persist 值（magic `VPS1`）按版本 1 读。

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

`SymbolId` 为稳定正确性优化；dense ID 与编译期消解为执行性能优化。

加载/AOT link 阶段：

```text
Stable SymbolId
      ↓ resolve once
Dense process-local ID
      ↓
array index / compact table index
```

例如 state：

```text
HomePage::count
        ↓ StateKey(128)
        ↓ mount / hot reload link（StateStore 的 cold side map）
StateId(index, generation)
```

每帧执行：

```rust
let cell = cells[state_id.index() as usize];
```

而不是：

```text
SymbolId
  ↓ hash
HashMap bucket probe
  ↓
state
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

Property、Event 与 component type 在编译期消解，不留 runtime ID：

- property 写成 typed builder 调用（Rust lowering）或 AOT node record 的定宽字段；binding 是 `AotEdge { state, node, class }`——源 state 的 `StateKey`、目标 node 的 pre-order 下标与 `DirtyClass` 位，mount 时解析为 `StateId` / `NodeIndex`；
- event 是 `Route` 的事件 tag（enum 判别值）加 handler table 下标；
- component 实例由编译出的代码或 view package 直接挂载，没有 runtime component type table。

需要 runtime 索引的身份必须用 Rust newtype 隔离语义，例如：

```rust
pub struct NodeIndex(u32);       // #[repr(transparent)]
pub struct ProgramSlot(pub u32);
pub struct TextureId { pub index: u32, pub generation: u32 }
```

这些 newtype 在 optimized build 中没有额外 runtime abstraction cost。package 格式内部的定宽记录字段（handler record 的 node 与 handler 下标等）是格式私有的，解码后以 typed API 交出。

具体宽度由每个 subsystem 的真实上限决定：

- 可证明上限远小于 65535 时，内部 slot MAY 使用 `u16`；
- public/runtime artifact ID 默认优先 `u32`，减少 overflow/转换分支；
- 不为节省 2 bytes 就在 hot loop 引入频繁 widen/narrow；
- 宽度改变属于 artifact/ABI decision，必须有 benchmark 和上限证明。

### 10.4.7 `NodeId`：运行时实例身份

`NodeId` 与 `SymbolId` 的语义完全不同：

```text
SymbolId = 源码/Schema 中“这个声明是谁”
NodeId   = 当前 Runtime 中“这个实例是谁”
```

采用：

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId {
    index: u32,
    generation: u32,
}

#[repr(transparent)]
pub struct NodeIndex(u32);
```

一个源码模板可以产生任意多个 Runtime Node：

```text
TodoRow template SymbolId
    ├── key=100 -> NodeId(27, 1)
    ├── key=101 -> NodeId(28, 1)
    └── key=102 -> NodeId(29, 1)
```

`NodeId` 是外部句柄：holder 可能活得比 node 久（Inspector、DSL runtime、app 持有的句柄），`generation`（live node 恒为奇数）以一次比较发现 stale handle。`NodeIndex` 是内部链接：tree link、binding target、dirty worklist 与 paint owner 用它直接索引 NodeArena/hot stores；arena 在释放前先 unlink，内部链接不会指向已释放的槽，因此不做 generation 检查。

Node hot arrays **不得为每个 Node 固定携带 16-byte `SymbolId`**。需要 debug/hot-reload reverse mapping 时使用：

- cold side table；
- template descriptor；
- optional debug metadata；
- sparse mapping；
- development-only reverse index（`viso-view` 的 `inspect` feature 记录 mount 来源）。

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

- `StableKey` 必须有稳定 Eq/Hash 语义：整数、`Bool`、`Char`、`String`、`Unit`，以及由它们组成的 Tuple、Record、`Option` 与 Enum（DSL 中不满足报 `E2701`）；
- Float 不得实现 `StableKey`；
- 列表排序变化不改变同 key child 的 Node identity；
- key lookup 只存在于 keyed reconciliation，不得扩散成所有 Node 的通用身份机制；
- reconciliation 完成后，后续 hot traversal 使用 `NodeId` / `NodeIndex`。

### 10.4.9 Source、Resource、GPU handle 不复用 SymbolId

`FileId` 表示 dev link / source map / LSP 中的源码文件；`ResourceId` 表示 asset/resource（规范化路径的 64-bit FNV-1a，路径不变则跨构建稳定，每个新 payload 取下一个 `ResourceRevision`）；`BufferId`、`TextureId`、`PipelineId` 等表示 GPU resource。

它们可能在 debug metadata 中关联 `SymbolId`，但不得直接 typedef/alias 为 `SymbolId`。

GPU resource handle 是 generational typed handle（`viso-gpu-abi`，backend 以 dense `SlotMap` 存储，回收槽时递增 generation）：

```rust
#[repr(C)]
pub struct TextureId {
    pub index: u32,
    pub generation: u32,
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

拥有稳定 source symbol（`SymbolKind::Node`）。若 Hot Reload 插入一个无关兄弟节点，`save_button` 的 symbol 应保持稳定，从而焦点、局部状态、动画等可按 Widget/Node migration contract 保留。

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

`.vs` 中它在 Action、Task、Effect 与 Event Handler 作用域里是不可变绑定 `save_button: NodeRef<Button>`（DSL §49.1）。编译器把它定为一个稠密 key（所在内联实例在高 16 位、在该组件具名节点中的序号在低 16 位），`viso-view` 在 view（或 `for` 项）自己的按 key 有序表中找到 `NodeId`，没有字符串查找，也没有 dynamic downcast。

Rust 侧 builder 返回的 `viso_ui::Handle` 只携带 `NodeId`，大型控件另有专用句柄（`ChartHandle`、`MapHandle` 等）。`view!` / `component!` 为根组件中只构建一次的具名节点生成 typed Rust 访问：

```rust
let view = viso::view!("editor.vs")(cx);
let save: Handle<kind::Button> = view.nodes.save_button; // 构建时记下的 NodeId，无查找
```

`if` / `for` / `match` 中的具名节点随其区域挂载，没有字段；内联子组件的具名节点不出现在外层；把一种节点的 handle 当作另一种用不能编译。`ui!` 片段只返回根 `Handle`。

这条规则的目标不是隐藏 ID，而是**不要把编译期已知信息丢掉，再让运行时重新查找一次**。

### 10.4.12 Dev metadata 与 Release stripping

Development artifact 可以保留：

```text
DenseId -> SymbolId
SymbolId -> canonical name
SymbolId -> SourceSpan
NodeId -> template/debug Symbol
property / event -> readable name
```

因此 Inspector（`viso inspect`，经 `viso-dev-protocol` 的 census / node detail / source table）能回答：

```text
NodeId(381, 4)
Component: app::home::TodoRow
Source: features/home/view.vs:82
Property: Button::text
Dirty reason: StateSymbol(todo.title)
```

Release 默认只保留运行所需的 dense/generational table；source name、reverse map、完整 Symbol path 和 debug spans 可按 profile strip。dev 层不进 release artifact 由 `check-release-absence` 验证（§10.2）。

### 10.4.13 Identity 性能合同

Identity benchmark：

```text
identity_node_generation_check_1m        crates/ui/benches/inspect_identity.rs
identity_generation_check_hashmap_1m     同上（HashMap<NodeId, _> 基线）
reconcile_keyed_reorder_within_window    crates/ui/benches/large_list.rs
```

目标（尚未实现）：

```text
identity_name_intern_100k
identity_symbol_build_100k
identity_symbol_link_100k
```

重点比较：

```text
stable Symbol HashMap lookup
vs
dense array lookup
```

不要把 benchmark 重点放在 `u64 == u64` 与 `u32 == u32`；真正的收益来自：

> **hash/probe/pointer chasing -> direct indexed access**。

稳态 frame benchmark 必须证明 local property/event/node hot path 不依赖字符串或 stable-symbol hash lookup（`inspect_identity.rs` 的 `steady_frame` 对照钉住稳态帧的 primitive 计数）。

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

Serde 只用于生态互操作（§10.5.14）；Ende 是 Viso internal architecture。

### 10.5.2 crate 与 public namespace

`viso-ende`（`crates/ende`，ADR 0012）是 `no_std + alloc`、零第三方依赖的 DAG 叶子（xtask allowlist 中它的依赖列表为空）。模块：

- `encode.rs` / `decode.rs`：binary `Encoder` / `Decoder` 与 `Encode` / `Decode` trait；
- `wire.rs`：`ProtocolTag`（`b"VE"` magic + `WIRE_VERSION`）、`content_hash`（跨 toolchain 稳定的 64-bit FNV-1a）；
- `section.rs` / `aligned.rs`：XIP section table（`SectionWriter` / `Sections`）与 8 字节对齐的 static blob 包装 `Aligned<B>`（`SECTION_ALIGN`）；
- `json.rs`：只写 `JsonWriter`；`json_value.rs`：有界 JSON reader `JsonValue::parse`。

保持单一 `viso-ende` crate，不额外制造 derive/core/schema crate。各协议的 message 类型与上限归所属 crate（`viso-schema`、`viso-dev-protocol`、`viso-behavior` 等），不进 ende。

`#[derive(Encode, Decode)]` 是目标（尚未实现；当前全部 impl 手写），落地时放在 proc-macro 叶子 `viso-macros`：

```rust
#[derive(Encode, Decode)]
struct BuildMessage {
    build_id: BuildId,
    target: TargetId,
}
```

Facade 目前不公开 `viso::ende`：`viso_ende` 只经 `viso::__private` 供 `ui!` / `component!` / `view!` / `asset!` 展开使用。derive 落地后再公开：

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
| Ende Binary | runtime/tool IPC、Hot Reload、cache、snapshot、profiler、remote preview、AOT/view package |
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
NodeArena + NodeStore 列（§16）
DirtyLists
Vec<Primitive>
transform table（`TransformTable`，每个 transform root 一个 `Affine2` place）
GPU instance buffer
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
AOT / view package（XIP 原地读取）
diagnostics/schema JSON
```

若同一进程内 producer/consumer 已共享安全 typed memory，禁止为了“统一协议”强行插入 Ende roundtrip。

### 10.5.5 API 设计：直接，不复制复杂通用 visitor surface

Public contract：

```rust
pub trait Encode {
    fn encode(&self, enc: &mut Encoder);
    fn encode_to_vec(&self) -> Vec<u8>;          // 默认方法
}

pub trait Decode: Sized {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError>;
    fn decode_from_slice(data: &[u8]) -> Result<Self, DecodeError>; // 默认方法，拒绝 trailing bytes
}
```

- `Encoder` 是 append-only 的 `Vec<u8>` writer，不会失败，因此 encode 路径没有 `Result`；
- Binary 只有一种格式，trait 直接取具体 `Encoder` / `Decoder`，不做 format 泛型；JSON 走独立的 `JsonWriter`；
- `Decoder<'a>` 借用输入：`read_bytes` / `read_str` / `read_array::<N>` 返回 `&'a` 借用，borrowed decode 直接在 `Decoder` 上完成（§10.5.12）；`Decode` trait 只产生 owned 值。

必须保持：

- direct field code（当前手写；derive 落地后由 derive 生成）；
- 不要求 runtime reflection；
- Binary field loop 不要求字符串 lookup；
- Binary field loop 不要求 per-field HashMap；
- 不依赖 `dyn Trait` 进行每字段动态派发；
- 可在复用 buffer 下做到零 heap allocation encode；
- decoder 有明确 lifetime/borrow 模型。

方便 API：

```rust
let bytes = message.encode_to_vec();
let message = BuildMessage::decode_from_slice(&bytes)?;
```

性能敏感内部代码用 `Encoder::with_capacity(n)` 预留容量，或用一个 `Encoder` 连续写多个值。稳态消息流复用同一 buffer（清空后保留 capacity）是目标（尚未实现：`Encoder` 目前没有 `clear` / 从既有 `Vec` 构造的入口，game net packet 与 dev frame 每条消息新建 `Encoder`）。

### 10.5.6 Binary format 原则

Ende Binary 优先**简单、确定、容易 fuzz、容易跨平台验证**，而不是追求每个 byte 的极限压缩（ADR 0012）。

默认原则：

```text
canonical little-endian
fixed-width little-endian scalars（u8..u64、i8..i64、f32、f64）
bool = 1 byte（0/1）
unsigned LEB128 varint for byte/string/collection lengths（超过 10 字节或越出 u64 即拒绝）
zig-zag varint for signed integers where the codec uses varint
IEEE-754 fixed-width float representation
explicit enum discriminant（手写 1 字节 tag）
length-prefixed UTF-8 string/bytes
known struct field order
bounded collection lengths（上限由协议定义，§10.5.11）
zero padding to an alignment boundary（Encoder::align / Decoder::align，padding 必须为 0）
no field-name strings in normal struct payload
```

例如一条 property 变更消息只按声明顺序写 `node`、`property`、`revision` 三个值；Binary payload 不写：

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

例如 `SymbolId`（及同形的 `StableId` / `StateKey`）：

```text
16 bytes canonical representation
```

逻辑上等价于（`viso_schema::id::write_stable_id`）：

```rust
enc.write_u64(id.hi);
enc.write_u64(id.lo);
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

- 明确选择 `u32` 或 `u64` wire type（长度用 varint `u64`）；
- decode 时检查是否可转换为当前 `usize`；
- 超出目标地址空间时返回结构化错误，不截断。

`Decoder` 对所有长度做这一收窄：超出 `usize` 或剩余字节的长度报 `UnexpectedEof`，不分配。

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

现有实现：

- dev channel（`viso-dev-protocol`）：长度前缀 frame（≤ `MAX_FRAME` 4 MiB），每条消息以稳定的 1 字节 tag 开头；runtime 的 hello 以 `ProtocolTag` + `DEV_PROTOCOL_VERSION` 开头并携带 schema / project fingerprint，host 校验后回 `HostHello` 或带原因的 `Reject`；
- 持久 artifact：以 `ProtocolTag` 加 4 字节 magic 开头（AOT package `AOT1`、view package `VPK2`，view package 与 persist 值 `VPS2` 再带 `FINGERPRINT_VERSION`；trace、input tape 另带各自的 format version），magic / version 不符即拒绝。

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

对于短生命周期 process-local IPC，可以选择严格 exact-version handshake，换取更简单更快的 payload。dev channel 即如此：hello / `Reject` 在所有版本中保持 version-first 布局，异版本 peer 被带原因拒绝，而不是被猜测解码。

### 10.5.11 Decoder 安全预算

所有外部或可损坏输入必须经过 bounded decoder。预算分两层：

**ende 层**（所有协议共享）：

- 每次 read 都经单一 `read_raw(len)` 闸门先检查剩余字节；
- varint 超长（> 10 字节）或越出 `u64` 即拒绝；
- byte/string 借用输入，不分配；声明长度超过剩余字节即报错，不按声明长度预分配；
- 集合 count 经 `Decoder::read_count(max)`：超上限报 `TooMany`，超剩余字节报 `UnexpectedEof`（每个元素至少占一字节），所以对零字节元素的巨大 count 也立即失败；`Decoder::read_list(max, item)` 在其上读出 `Vec`，预分配至多 4096 项；
- `DecodeError` 是 heap-free 的 `Copy` 枚举，携带 byte offset；
- JSON reader 嵌套深度上限 128。

**协议层**（每个协议自己的常量）：

```text
max message/frame bytes（dev：MAX_FRAME）
max string bytes（MAX_PATH、MAX_NAME、MAX_LOG …）
max collection length（MAX_FILES、MAX_SECTIONS …）
max nesting depth（固定深度，如 state retyping）
```

声明的 count 必须先与上限比较、再与剩余字节比较，之后才允许按它预分配。schema、behavior 与 dev-protocol 的 list 读取都走 `read_count` / `read_list`；dev-protocol 把 `TooMany` 映射为带名字的 `WireError::Bound { what }`。

错误模型：

```text
DecodeError（ende）
  UnexpectedEof { offset, needed, available }
  OverlongVarint { offset }
  InvalidUtf8 { offset }
  TrailingBytes { remaining }      // decode_from_slice / Decoder::finish
  Malformed { offset }             // 非法 tag、版本不符、非零 padding、section table 不合法 …
  TooMany { offset, max }          // read_count 超出调用方给的上限

协议错误（如 dev 的 WireError）
  Version / FrameTooLarge / Bound / UnsupportedDomain / Malformed(DecodeError)
```

Decoder：

- MUST NOT panic on malformed input；
- MUST NOT blindly preallocate attacker-controlled length；
- MUST check integer overflow；
- MUST preserve byte offset for diagnostics；
- fuzz target 必须覆盖所有 public decode entry（§10.5.18）。

### 10.5.12 Borrowed / zero-copy decode

对于只读短生命周期 payload，Ende 支持 borrowed decode：`Decoder<'a>` 的 `read_bytes` / `read_str` / `read_array::<N>` 返回 `&'a [u8]` / `&'a str` / `&'a [u8; N]`。

要求：

- lifetime 与输入 buffer 绑定；
- borrowed value 不跨越输入 buffer 生命周期；
- Hot Reload/async queue 若需要长期持有，必须显式 own/copy/retain；
- 不为了 zero-copy 引入 unsafe dangling reference；
- benchmark 证明收益后再扩展复杂 borrowed collection surface。

XIP（execute-in-place）布局：blob 开头是 section table（`SectionWriter` 写、`Sections::read` 读）：section 数，再每项 offset、长度、条目数三个 LE `u32`；每个 section 从 blob 起点按 `SECTION_ALIGN`（8）对齐，间隙为 0，布局唯一。reader 先一次校验整张表（越界、重叠、乱序、非零 padding 都是 `Malformed`），再按 offset 直接定位 section。嵌入为 `static` 的 blob 用 `Aligned<[u8; N]>` 放到对齐地址，因此 AOT/view package 可在 memory-mapped flash 原处读取，不拷贝（§50A.6；`crates/schema/tests/xip_alloc.rs` 验证校验与遍历零分配）。

### 10.5.13 JSON 的定位

Ende JSON 优先：

- human inspectable diagnostics；
- machine-readable compiler messages；
- LSP/AI Schema；
- `viso schema/hir/ir` dump；
- test golden；
- 外部工具互操作。

JSON 不作为 frame/runtime 内部高速消息的默认格式。

现有两半，均手写、无 serde：

- `JsonWriter`：紧凑输出、按 JSON 规范转义、整数格式化不分配、非有限浮点写为 `null`；member 按调用顺序写出，因此输出确定；
- `JsonValue::parse`：严格 RFC 8259（无注释、无尾逗号、无前导零、escape 与 surrogate 全检查），嵌套深度 ≤ 128，object 按文档顺序保留 member（含重复），数字为 `f64`，错误为 `JsonError { offset, message }`。用于读取 Cargo metadata、glTF、native package schema 等外部 JSON。

typed JSON decode 是目标（尚未实现，随 derive 落地）；届时 encoder/decoder 必须共享 Ende 的：

- type/schema metadata；
- limits/error model；
- derive contract；
- deterministic test infrastructure。

需要 canonical JSON 的 snapshot/test 场景必须定义稳定 field ordering；普通 JSON object 语义仍不依赖 map insertion order。

### 10.5.14 Serde integration

Viso 核心 crate 不依赖 serde，也不提供 serde adapter crate。需要 Serde 的生态互操作放在 viso-ext 插件里，由应用按 feature 启用（如 `viso-net` 的 `json` feature），用于：

- Rust 生态已有数据模型；
- third-party API；
- 用户后端/network/database；
- 需要 Serde-specific crate 的场景。

边界：

```text
Viso internal protocol -> Ende
Rust ecosystem interop -> Serde（viso-ext，opt-in）
```

禁止让 `serde::Serialize/Deserialize` 成为 Viso Runtime、Node、Hot Reload 或 Studio protocol 的硬依赖。

同样，Viso 不要求用户业务数据全部改为 Ende；Ende 只拥有 Viso 自己必须稳定控制的内部边界。

### 10.5.15 Ende 与 `viso-macros`

目标（尚未实现）：`viso-macros` 提供 `#[derive(Encode)]` / `#[derive(Decode)]`，覆盖普通 message struct/enum；sectioned/XIP 格式（AOT、view package）继续手写。

derive 必须生成直接 field encode/decode，不建立运行时 reflection map。

Macro expansion 必须：

- 给字段/variant 生成稳定 schema metadata；
- 在编译期拒绝不支持的裸 pointer/reference lifetime；
- 对 `usize/isize` 的持久 wire 使用给出诊断或要求显式 adapter；
- 支持 `#[ende(...)]` 一类受控 attribute 时（如 varint/fixed 选择、集合上限），attribute 集合必须小且有明确 Schema；
- 不允许 attribute 改变基础 tokenization 或引入运行时字段名查找。

### 10.5.16 Ende 与 Identity Architecture

Ende 只定义 ID 的 wire *格式*，不拥有 ID 类型；各 ID 由所属 crate 用 ende 的 primitive 写出。当前表示：

```text
NameId                          不持久编码
SymbolId / StableId / StateKey  fixed 16 bytes：u64 LE hi，再 u64 LE lo
NodeId（inspect / dev 消息）     varint index + varint generation
NodeRef（dev patch）             tag + varint：Static(pre-order index) | Region { region, arm, item }
ResourceId                      fixed u64 LE
```

注意：某 subsystem 内部即使把某个 slot 压缩为 `u16`，跨 artifact/wire 的 ID 表示也必须由协议明确，不允许直接 memcpy 内部 struct。

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

Decode 是否零分配取决于目标值是否拥有 String/Vec 等 owned data；对于 borrowed/预分配目标路径，应提供零或接近零分配选项。现有验证：XIP package 校验与遍历零分配（`crates/schema/tests/xip_alloc.rs`），AOT package 加载不按 record 分配（`crates/ui/tests/aot_load_alloc.rs`）。

Benchmark（目标，尚未建立）至少：

```text
ende_bin_small_message
ende_bin_encode_reused_buffer
ende_bin_decode_owned
ende_bin_decode_borrowed
ende_json_diagnostic_1k
ende_dev_patch_roundtrip
ende_game_packet_roundtrip
ende_profiler_trace_encode
ende_aot_package_validate
```

记录：

- ns/message；
- MB/s；
- allocations/message；
- bytes allocated；
- bytes/message；
- peak memory；
- code size；
- compile time（derive 落地后的 derive-heavy fixture）。

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
overlong varint
huge declared length
unknown enum tag
nesting limit
non-zero alignment padding
schema mismatch
old/new compatible message fixtures
```

`crates/ende` 的单元测试覆盖 roundtrip、截断、overlong varint、invalid UTF-8、trailing bytes、padding，以及对 20 000 组确定性伪随机输入的 fuzz-smoke（不 panic、cursor 不回退不越界）；golden bytes 与 float bit 行为尚无专门测试。

Fuzz（`fuzz/`，cargo-fuzz）：

```text
bin decoder never panics
json decoder never panics
bounded decoder never exceeds configured allocation budget
encode(decode(valid_bytes)) preserves semantic value
```

target：`dev_wire`、`dev_snapshot`、`game_snapshot`、`input_tape`、`aot_package`、`view_package`、`behavior_module`（含校验）检查 encode(decode(x)) 的再解码一致；`game_packet` 让一个 rollback session 接收任意字节并继续推进；`raster`（含动画逐帧）、`svg`（解析后栅格化）、`gltf`、`font`（`parses` 后对前几个 glyph 做 coverage 与 color 栅格化）与 `text`（grapheme / word / line 边界落在字符边界上，bidi 解析）只要求不 panic、不超出分配上限。种子由 `crates/dsl/tests/fuzz_seeds.rs`（编译器产出的 package、module，session 产出的 packet，以及仓库内的图片、SVG、字体与文本样本）和各 crate 的 `fuzz_seeds` 测试写出。`raster` 曾找到两处按帧头尺寸分配：APNG 的 `fcTL` 超出画布（zune-png 按帧分配输出），GIF 帧超出像素上限；现在解码前先扫描 `fcTL` 是否在画布内，GIF 帧尺寸同样经过 `check_size`。

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

`viso-runtime` 是应用执行内核，但不是“万能 Cx”。它只依赖 `viso-platform`，不依赖 ui/widgets/dsl；通过 `FrameDriver` trait 反向调用上层（facade 的 `AppDriver` 实现它）。

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

`Scheduler` 实现 `viso_platform::AppHandler`：每个 raw event 归类为 `RedrawReason`（或运行一帧、或退出），并返回 `ControlFlow`。帧时间由 `FrameClock` 在帧首采样一次（生产用 `WallClock`；headless 注入 `ManualClock` / `FixedStepClock` 得到确定时间）。

### 11.2 Frame phases

一帧的语义阶段如下（ADR 0043，取代 ADR 0001 的十二阶段表；代码中为 `viso_runtime::FramePhase::ORDER`，`run_frame` 按序对每个 phase 调用一次 `FrameDriver::run_phase`）：

```text
 1. CollectInput
 2. DispatchInput           // hit test 是对 NodeStore 的无状态查询（沿 reach 剪枝），没有单独的 hit 结构要重建
 3. RunContinuations        // 到期 timer、spawn_then 续体、async completion、无 worker 时的 inline job；写入当前 transaction
 4. ApplyDevPatches         // Dev-only：已 stage 的 PatchBundle 在帧边界 commit；Release build graph 中不存在
 5. AdvanceAnimations       // 推进 `NodeAnimation` / `LookTransition` 与 frame client；transform 动画只写 transform table
 6. FlushStateTransactions  // 全帧唯一一次 flush：binding 求值 + dirty 标记
 7. ResolveStyle
 8. Measure
 9. Layout
10. UpdateTransforms        // transform root 位移重算（每 root 一次加法）、subtree bounds
11. BuildPaintChanges       // 只有 PAINT 的帧原位改写 dirty 节点的 primitive；结构变化的帧按节点 splice 改变的 run（Rendering §9.7）；其余整树 paint
12. BuildRenderBatches      // 同时计算 damage region（§30.3）
13. UploadGpuChanges        // GPU：range upload；patch / splice 只上传改写的部分；只移动 root 的帧不上传，renderer 取新位移；CpuRaster：damage tile 光栅化
14. Submit                  // present_with_damage
15. RunEffects              // effect 重启 / cleanup；产生的 state 写入进入下一帧
16. UpdateSemantics         // present 之后执行，不在 input-to-photon 关键路径上；只在 AT 激活时发布
17. PostFrameCleanup
```

规则：
- 实现可以合并阶段，但语义顺序必须保留；空阶段的成本必须是 O(1)（按 dirty worklist 是否为空判断，而不是遍历树）。facade 的合并方式：输入在到达时（两帧之间）由 `FrameDriver::on_input` 立即路由，handler 写入进入 pending set，因此帧内 1–2 没有工作；Measure 并入 Layout；BuildRenderBatches 由 renderer 在 UploadGpuChanges 内完成。
- 3–5 产生的所有写入与 handler 的写入合并为同一个 transaction，在 6 一次性 flush。不允许出现「动画 flush 一次，事件再 flush 一次」。
- 需要同帧可见的派生值必须使用 computed，不能用 effect。effect 只承载副作用，它的写入最早在下一帧可见。
- effect 环跨帧检测：一个 effect 连续 `SETTLE_ROUNDS`（16）帧仅因 effect 写入而被唤醒，即判定为跨帧反应环，停止运行并报告 `E4202`；帧内 flush 环仍由 settle 轮数上限截断。
- Semantics 放到 present 之后，是为了不拉长输入到上屏的延迟。辅助技术读到的树最多滞后本帧 present 几百微秒，不会跨帧。

### 11.3 Phase-specific Context

不再让一个 `Cx` 拥有所有能力。每个 context 只暴露其调用点需要的能力：

| Context | 调用点 | 能力 |
|---|---|---|
| `RuntimeCx` | runtime → `FrameDriver` | 创建/关闭窗口、请求 redraw、平台调用；不认识任何 UI 类型 |
| `AppCx` | `Application::new` | `load_font` / packaged fonts、dev state 注册；请求被记录，由 facade 之后执行 |
| `BuildCx` | `Component::build` | 声明节点、state、binding |
| `EventCx` | event handler | 读写 state（写入延迟到 flush）、`spawn` / `spawn_then` / `cancel_task`、`services()`、焦点 / 显隐 / 动画 / timer 请求；不持有 `NodeStore`，不暴露 `mark_dirty` |
| `UpdateCx` | 续体、timer 回调 | 读写 state、`cancel_task`；持有 store 只供 flush 使用 |
| `ComputeCx` | computed 求值 | 只读 `get`，记录依赖；没有 `set` |
| `EffectCx` | effect 运行 | 读写 state、以 owner 节点 spawn task |
| `StructureCx` | view control-flow region 的 structure hook | 在冷路径上原位编辑树（分配、移动、释放节点与 state edge） |

Layout、paint、render 不是用户回调：`Component` 只有 `build`，布局与 paint 由框架按节点数据执行（ADR 0003），所以这些阶段没有面向用户的 context 参数，也就无从获得网络、窗口创建或任意 GPU submit 能力。

这既是架构限制，也是性能与可推理性工具。

---

## 12. Scheduler 与 redraw 策略

不能把 `redraw()` 作为业务层的常规责任。

Runtime 聚合以下原因（`viso_runtime::RedrawReason`，存成一个 `RedrawReasons` bitset：聚合是一次 OR，idle 判断是与 0 比较）：

```text
InputDirty
StateDirty
AnimationActive
AsyncCompletion   // RawEvent::Wakeup：task 完成、dev channel 消息、timer 到期
WindowResize
ExternalSurfaceInvalidation
FirstFrame        // 启动首帧，由 scheduler 拥有（ADR 0004）
```

原因只用于 `decide()`，不区分跨线程唤醒的来源：timer 到期与 dev channel 消息都以 `RawEvent::Wakeup` 进入，共用 `AsyncCompletion`。

最终决定（`FrameDecision`，由 `RedrawReasons::decide` 得出）：

```text
NoFrame                 // ControlFlow::Wait；有 timer 时为 WaitUntil(deadline)
FrameAtNextVsync        // 其余原因
ImmediateFrame          // WindowResize / ExternalSurfaceInvalidation / FirstFrame
```

一帧内（两帧之间到达的所有事件）的多个 state mutation 自动 batch 进同一 transaction。一次性 timer 由 driver 持有，driver 报告最近 deadline（`FrameDriver::next_timer_deadline`），scheduler 只在本来要 `Wait` 时把它转为 `ControlFlow::WaitUntil` 驱动平台 pump，而不是轮询（ADR 0019）；deadline 到达时 backend 合成一次 `RawEvent::Wakeup`。

### 12.1 Idle 必须真正 idle

没有动画、输入、任务和脏数据时：

- 不持续 layout；
- 不持续 paint；
- 不持续 submit；
- 不周期性遍历 UI tree；
- 不轮询每个 component。

App 处于 `Lifecycle::Suspended` 时不运行帧与动画 beat，pending reasons 保留到 `Resumed` 后的第一帧。

---

## 13. Async runtime

最小官方 API 是 handler 上下文的 `cx.spawn(...)` / `cx.spawn_then(...)`：task 归 handler 所在节点所有，节点释放时自动取消；`spawn_then` 的续体在帧边界拿到 `UpdateCx` 写回状态（§13.2，ADR 0032）。

### 13.1 核心协议

Viso 自己拥有的是 **UI task protocol，而不是一套通用 async runtime**：

- task ID（`TaskId`，进程内唯一）；
- wakeup queue；
- cancellation（按 `TaskId` 或所属节点 drop future；没有独立的 cancellation token）；
- main-thread dispatch；
- timer/lifecycle integration；
- scoped-task ownership；
- executor adapter contract。

通用 work-stealing executor、I/O reactor、HTTP/TLS 等不进入 Viso 核心。只靠 waker 完成的 future 无需 adapter：service `Reply`、channel、外部 executor 的 `JoinHandle`，以及 viso-ext 中走 OS 网络栈的 `viso-net`。需要 reactor 的 future（Tokio socket/timer）在该 runtime 上 spawn，UI task await 它的 handle。

Tokio/Smol adapter crate（`viso-tokio`、`viso-smol`）目标，尚未建立；建立时作为 viso-ext 插件，不进入核心，不把 adapter 组合做成核心 crate feature matrix。

### 13.2 实现（ADR 0032）

- `viso-platform` 的 `LoopWaker` 可跨线程 kick，loop 线程上收到一次 `RawEvent::Wakeup`；每个 backend 复用自己已有的唤醒通道。
- `viso-runtime::task::TaskSet` 持有 task 与 wake queue。首次 wake 或 spawn 才 kick；没有 wake 的帧不加锁、不 poll。`FrameDriver::on_wakeup` 为有 task 被唤醒或 timer 到期的窗口请求重绘。
- facade 在 `RunContinuations` 阶段 poll 被唤醒的 task 并运行完成者的续体，续体写入并入本帧 transaction（§11.2）。
- `EventCx::spawn` / `spawn_then` / `cancel_task`：task 归 handler 所在节点所有，释放子树或清空 store 时随之取消。`spawn_then` 的续体在帧边界拿到 `UpdateCx`，因此 `.await` 期间不持有 arena 引用。
- `UpdateCx` 与 `EffectCx` 只有以显式 owner 节点 spawn 的内部入口（供 `viso-view` 使用）和 `cancel_task`；`.vs` 的 `start` task（slot 策略 `keep_latest` / `drop_new` / `queue` / `parallel(n)`）与 `resource` loader 都建立在这套 owner-scoped UI task 之上（DSL §36、§38、§39、§92、§93）。
- `cx.services()` 由 facade prelude 中的 `ServicesExt` 提供；store 以 `Rc<dyn Any>` 持有 `Services` 并以 `&dyn Any` 借给 `EventCx`，所以 `ui` 不依赖 `services`。`Services` 只含 UI 自身调用的服务（`Haptics`、`Launcher`，另有 `app_data_dir`）；异步调用返回一次性 `Reply<T>`（`Future`，也可 `try_take` 轮询），`Completer` 未回答即被 drop 时得到 `ServiceError::Cancelled`（ADR 0031）。
- 暂缓：`UpdateCx` / `BuildCx` 上的公开 spawn 与 services、不归属节点的 detached spawn、adapter crate。

### 13.3 Worker 数量由 profile 决定

worker 线程池是可选资源，不是正确性前提。各 footprint profile 的默认 worker 数：desktop / mobile / web 按机器（核数 − 1，1–8；wasm32 为 0），`embedded-gpu` 为 1，`embedded-cpu` 为 0（`VISO_WORKERS` 可覆盖）。`workers = 0` 时，text shaping、image decode、font parse 这类 TextWork / AssetWork 留在队列里，由主线程每帧按时间预算执行，结果与多线程路径一致：通用 job 在 `RunContinuations` 内执行（`viso_ui::work::run_inline`），text 在 Layout 内执行；有积压时 driver 持续请求帧直到排空。当前预算是按 60 Hz 帧间隔推算的定值（各约 4 ms）；按实际 frame deadline 推算预算是目标（尚未实现）。任何 subsystem 都不得假设「至少存在一个 worker」。

---

# Part V — UI Runtime 数据模型

## 14. 不再以 `Rc<RefCell<Box<dyn Widget>>>` 作为树基础

Viso 使用 generational arena + integer ID（`crates/ui/src/node.rs`）。

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

```rust
/// u32::MAX（NodeIndex::NONE）表示无。树内部链接由 arena 不变量保证有效，不携带 generation。
#[repr(C)]
pub struct NodeLinks {
    pub parent: NodeIndex,
    pub first_child: NodeIndex,
    pub last_child: NodeIndex,
    pub prev_sibling: NodeIndex,
    pub next_sibling: NodeIndex,
} // 20 bytes（有 size 断言）
```

内部链接与外部 handle 分开：
- **外部 handle**（`NodeId` / `Handle`、Inspector、DSL runtime、跨帧持有者）携带 generation，用于检测 stale；
- **内部链接**（parent/child/sibling、dirty worklist、paint range owner）只存 `NodeIndex(u32)`；binding target 存为相对模板实例 node base 的 offset（`TemplateBinding` + `InstanceBase`）。节点释放时 arena 先解链，因此内部链接永远不会指向已释放的 slot。

遍历只读 `NodeLinks` 数组（`parent_of` / `first_child_of` / `next_sibling_of` / `children_of`），不做 generation 检查；需要 live handle 时才经 `id_of` 配上 generation。

Node 的每节点数据存在 `NodeStore` 的并行列里（§16.0），按访问模式分组：

```text
generation[] / NodeLinks[]            arena
bounds[] / reach[] / measured[]       layout 结果
layout[] / style[] / rules_at[]       layout 输入与 style
translate[] / scroll[] / transform_root[] / turn[]   transform
hittable[] / hidden[] / clips[] / overlay[] / focusable[] / disabled[]   interaction / visibility flags
dirty[]（DirtyClass）+ DirtyLists       dirty
semantics[] / semantic_state[]        semantics
PaintIndex（按 slot 的 paint record）   paint range
```

不是要求所有数组永远严格 SoA，而是按访问模式决定 AoS/SoA。原则：**一起访问的数据尽量一起存；很少访问的大字段不要污染热结构。**

### 14.2 Hot / Warm / Cold data

明确三类：

**Hot**（帧内高频）：

- parent/child traversal（`NodeLinks`）；
- bounds / reach；
- transform（translate / scroll / transform root）；
- dirty flags；
- visibility（`hidden`）；
- clip；
- hit-test flags（`hittable`）。

**Warm**（脏时访问）：

- layout style（`LayoutInput`、intern 的 `BoxRules` / `Placement`）；
- computed style（`BoxStyle`、`InteractionStyle`）；
- semantics；
- animation descriptors；
- binding metadata。

**Cold**：

- debug name；
- source map；
- component type name；
- inspector metadata（`inspect` feature 下的 `DirtyJournal`）；
- Viso source location（dev 构建由 `viso-view` 的 mount 记录与 `StateKey` 承载，不进 `NodeStore`）。

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

Rust 侧 `Component` trait 只有 `fn build(&self, cx: &mut BuildCx<'_>)`：component 自身不持有 reactive state，它的 cell 住在 build context（`cx.state`）里。

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

只表达 renderer 需要的信息（`viso_render::Primitive`，Rendering §8.1）：

```text
draw:    Quad / AnalyticRRect / AnalyticEllipse / AnalyticCapsule / AnalyticLine /
         GlyphRun / Image / Gradient / AnalyticShadow / Frosted / Path / Mesh /
         Shader / Scene
markers: Layer … LayerEnd（clip / 合成层，后随 ColorEffect / Blend / ClipCorners）
         Transform … TransformEnd（transform root）
```

primitive stream 是扁平的：layer 与 transform root 以 push/pop marker 表达，而不是递归树，因此 `Vec<Primitive>` 可以直接 batch；少数大而罕见的变体（`Frosted`、`Path`、`Shader`、`ColorEffect`）装箱，其余 primitive 只占 leaf 需要的大小。

一个 Component 可以生成多个 Node；一个 Node 可以对应多个 Primitive；同一类 Primitive 可以被 renderer 合并到同一 batch。

---

## 16. Node Arena 设计建议

### 16.0 Viso 1.0：hot storage 采用 hybrid indexed SoA

Viso 采用 **hybrid indexed SoA**：

```text
NodeId(index,generation)
        │
        ├── NodeArena: generation[index], NodeLinks[index]   // links 为紧凑 AoS
        ├── NodeStore 列[index]                              // layout / transform / flags / dirty / paint … 每列一个 Vec
        ├── intern 表（box_rules / placements，经 rules_at / placed_at 的 u32 下标）
        ├── Option<Box<_>> 列（content_payload / text_request / grid_tracks）
        └── 稀疏表与活跃集合（Sparse<T>、AssetTable、各 feature 的活跃节点列表）
```

关键约束：

- hot 列以同一个 slot index 直接寻址，热遍历不经过 per-node HashMap；
- `NodeLinks` 保留为紧凑 AoS，因为 parent/child/sibling 通常一起访问；`generation` 单独一列，遍历不读它；
- 低频、可选、大对象不强迫所有 node 支付固定空间：少数节点才有的值放进 intern 表、`Option<Box<_>>` 或按 slot 排序的 `Sparse<T>`；只在动画/交互进行时需要逐帧推进的节点放进活跃列表（为空时对应阶段 O(1)）；
- 多个 flag 合并为一个 `NodeFlags` bitset 列是目标（尚未实现：当前每个 flag 一列 `Vec<bool>`）；
- 不采用 archetype migration 作为默认节点变更机制；UI identity 和 hierarchy 比 ECS 组件组合更稳定；
- 后续可以在单个 store 内优化 chunk/page，但不能改变 `NodeId -> fixed index stores` 这一基线语义，除非新 ADR 证明收益。

结构：

```rust
pub struct NodeArena {
    generation: Vec<u32>,          // 奇数 = live，偶数 = free；释放与复用各 +1
    links: Vec<NodeLinks>,
    free: Vec<u32>,
    blocks: Vec<(u32, Vec<u32>)>,  // 已释放的模板实例块，按长度复用
    live: u32,
    fixed: Option<u32>,            // with_fixed_capacity
    // + 子节点变更日志（relinked / touched），供增量 layout / paint 使用
}
```

除单个 slot 外，arena 还分配 **block**：模板实例按顺序填满的一段连续 slot（`reserve_block` / `alloc_at` / `release_block`），binding 因此能以相对 block base 的 offset 指向目标；同长度的 block 互相复用，列表行回收彼此的 block。

容量策略：
- 默认按需增长；所有列在同一步扩到同一容量（几何增长，至少 64 slot），一次突发创建让每列只 realloc O(log n) 次；已知节点数的 builder（模板实例化、AOT package 加载）先调用一次 `NodeStore::reserve_nodes`；
- `NodeArena::with_fixed_capacity(n)` / `NodeStore::with_fixed_capacity(n)`：所有列一次分配，之后不再 realloc；`try_alloc` 在满时返回 `ArenaFull`。embedded profile 用它把峰值内存变成启动期可知的量：窗口的 store 按 `MemoryBudget::nodes` 固定容量建。`NodeStore` 满时拒绝新节点而不 panic：交出一个永不 live 的 handle（各列多留的一格供它写入），经它的写与 link 都是 no-op，`take_refused` 报告 `ArenaFull` 与被拒数，facade 每帧打印一次。

### 16.1 ID 验证

```rust
fn is_live(&self, id: NodeId) -> bool {
    self.generation.get(id.index as usize) == Some(&id.generation) && id.generation & 1 == 1
}
```

generation 的奇偶就是占用位，不另存 `occupied`。所有接受 `NodeId` 的 store 写入都先做这一检查，stale handle 的写入是 no-op。

Debug build 可以增加 owner store 标记，在 `NodeId` 被拿到另一个窗口的 store 上使用时 debug_assert（目标，尚未实现）。

Release 只保留必要字段。

### 16.2 Node mutation

不存在 `&mut Node`：节点没有聚合的 struct，只有按 slot 的列。

修改走短生命周期、按字段的 API：

```rust
store.set_hidden(id, true);   // 内部：live 检查，写列，标记对应 DirtyClass
```

handler 不持有 `NodeStore`。它主要通过写 state 表达意图，由 binding 推出失效；少数直接的节点请求被记录在 context 上，handler 返回后由 router 应用：

```rust
cx.set_hidden(node_id, true);
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

结构性变化（`if` / `match` 分支切换、`for` 列表增删重排、component 类型变化）才进入 reconciliation：控制流区域（ADR 0038）按模板实例挂载、卸载或按 key 移动子树（§18.1），不做整树 diff。

---

## 18. State Slot

内部使用 slot，而不是每个 Signal 一个 `Rc<Vec<Subscriber>>`。

概念（`viso_ui::state`）：

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

状态实际值：

- state store 的 cell 只存小标量 `StateValue`（`Int(i32)` / `Float(f32)` / `Bool` / `Color`）；
- Viso DSL 的 state 值留在 behavior VM 的实例里，cell 镜像标量值（`Mirror`）或只存 revision（`Track`）；`env` 字段与 game query 各占一个 revision cell（`Env` / `Game`，`viso_view::host`）；
- Rust 组件可以把值放在自己的 typed storage 里，只用 cell 的 revision 做依赖。

`StateId` 只负责 dependency indexing，不要求把所有 Rust 值 erase 成动态 `Value`。

Rust 代码用 typed cell `State<T>`（`viso_ui::typed_state`），不 match `StateValue`：

```rust
let count: State<i32> = cx.state_of(0);   // T: StateKind = i32 / f32 / bool / Rgba
cx.bind(count, label, DirtyClass::PAINT);  // bind 收 StateId 或 State<T>
count.update(ev, |n| n + 1);               // 写：任意 StateWriter
let n = count.get(ev);                     // 读：任意 StateRead，Option<T>
```

- `Copy`，只含 `StateId`；`get` 返回 `Option<T>`：cell 已释放（或类型不符）时为 `None`，`update` 在 cell 已释放时不写；
- 读经 `StateRead`（`StateStore`、`EventCx`、`UpdateCx`、`StructureCx`，以及会记录依赖的 `EffectCx`、`ComputeCx`），写经 `StateWriter`（`StateRead` 的子 trait），与 `StateValue` 写入一样 pending；
- `cx.state_of` 与 `cx.state` 一样参与 warm restart 的 cell 保留。

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
- **动态边**：运行时登记的边（动态脚本）按 cell 分组单独存放；静态与动态路径分开计数（`ReactiveCounters`），动态路径不决定静态路径的成本。

Release 原生 lowering（ADR 0041）生成的是节点读取者的求值函数，与失效边分开；evaluator id 就是 behavior module 的 chunk 索引（`NativeCode` 按索引给出编译后的函数），Dev 与 Release 共用同一张 handler 表，热更新只替换表项。

---

## 19. Dirty flags

Dirty class 固定为以下八个，不得增减或改名（ADR 0001；定义在 `viso_schema::DirtyClass`，`viso_ui::dirty` re-export）：

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
| 子节点增删/重排 | STRUCTURE（VirtualList 行 remount 不标 STRUCTURE：canvas 标 LAYOUT，位置变化的行宿主标 LAYOUT + PAINT，ADR 0008） |
| text 内容 | MEASURE + LAYOUT + PAINT + SEMANTICS |
| text color / background | PAINT |
| width | MEASURE + LAYOUT + PAINT（`set_fixed_size`） |
| transform / scroll offset / scale / rotation | TRANSFORM（节点第一次成为 transform root 时另标一次 PAINT；永不 MEASURE/LAYOUT；hit test 在查询时按逆变换换算，不需要 HIT_TEST，ADR 0043） |
| aria/label | SEMANTICS |
| hover/pressed/focus 状态 | STYLE，随后由 style diff 决定 PAINT/LAYOUT；命中区域变化另标 HIT_TEST |
| visibility | `hidden`：MEASURE + LAYOUT + PAINT + SEMANTICS（退出布局、绘制与语义树）；opacity 为 0：只 PAINT，仍占位、仍命中 |

### 19.1 Dirty propagation

不是所有脏状态都向整棵祖先树传播。

例如：

- `PAINT` 通常不需要让祖先 layout dirty；
- `MEASURE` 需要传播到受 child intrinsic size 影响的祖先，停在 layout boundary（两个轴都是 Fixed 的祖先或 `Absolute` 容器，§23.1，ADR 0008）；relayout 一般从最高的 MEASURE 节点的父节点重做；但当父节点是两个轴都 Fixed 的 flex 容器、本帧没有重排子节点也没有 MEASURE / LAYOUT、而该节点没有隐藏、主轴请求是 Fixed 或 fill 份额时，它的新 measure 移动不了父节点和任何兄弟，只从它自己重做：主轴保持原来的区间，交叉轴按新 measure 重新放置；absolute-rows canvas（VirtualList）的行同样只重做自身。节点自身的尺寸请求改变（`set_fixed_size`、fill weight、长度折算、隐藏）都会让父节点重排子节点，所以这条捷径只覆盖内容引起的 re-measure（文本、图片、子树）。差分测试逐帧比较增量 relayout 与整树 layout 的每个 bounds；
- `TRANSFORM` 可以只影响 transform subtree；
- `STRUCTURE` / `MEASURE` / `SEMANTICS` 属于 `BUBBLING`，沿父链上传；`SEMANTICS` 上传后仍只重建 semantics tree 的对应分支（当前实现见 §29.1）。

需要专门的 propagation rules，而不是简单 `parent.redraw()`。

### 19.1.1 Dirty worklist 合同

dirty bit 只回答「这个节点脏不脏」，不能用来找出脏节点。每个 dirty class 必须有自己的 worklist：
- 标脏时，把节点新获得、尚未列入的 class 追加进对应 worklist；每个 slot 另记「已列入的 class」掩码（`DirtyLists`），同一帧内节点释放、slot 复用也不会重复列入；读者以节点当前的 dirty bit 为准，过期条目只多一次读取；
- 每个阶段只消费自己的 worklist，成本是 O(脏节点数)，与树的总节点数无关；
- `MEASURE` / `LAYOUT` 的 worklist 先归约成脏根集合（祖先已在集合中的节点丢弃），再按 depth 升序处理，同一祖先在一帧内只 relayout 一次；
- 所有 worklist 的 buffer 跨帧复用，稳态零分配；
- worklist 为空的阶段直接跳过，这是「idle 真正 idle」的机制保证。

### 19.2 Viso 1.0：Transform 是独立失效平面

Layout tree 定义结构与几何约束；Transform plane 定义最终局部/世界变换、滚动偏移和动画变换（ADR 0043）。两者使用同一个 `NodeId` 索引，但 dirty propagation 分离：

```text
Layout change
  -> MEASURE/LAYOUT
  -> may update transform base rect（转动节点的 pivot 随盒移动）

Scroll / translate / scale / rotate
  -> TRANSFORM（节点第一次成为 transform root 时另加一次 PAINT）
  -> 写该节点的 scroll / translate / turn
  -> UpdateTransforms 按 paint 顺序重算 transform table 中每个 root 的 place
  -> UploadGpuChanges 不上传；Renderer::set_transforms 取新 place，
     encode 时每个 draw 按所属 root 的 place 映射
  -> damage = subtree 旧 placed bounds ∪ 新 placed bounds
  -> never MEASURE/LAYOUT/PAINT/HIT_TEST

color-only change / animation
  -> PAINT
  -> BuildPaintChanges 在上次 paint 的 stream 里原位改写该节点自己的 primitive
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

为什么 paint-only 帧是 O(改变量)：每次 paint 给每个节点记一条 `Record`（`PaintIndex`，每节点 32 字节：自身 drawable 区间、subtree 结束位置、所在 transform root、父节点、深度、generation、stream epoch）。下一帧从 PAINT worklist、bounds 移动过的节点与重新挂接过子节点的父节点出发，按旧区间顺序单遍走：只重新生成自身 drawable 改变的节点，按旧记录 diff 改变过的子节点列表，其余区间原样平移。种类与个数都不变时原位改写（`Painted::Patched`；renderer `patch` 只重新 ingest / lower / 上传这些 slot 并给对应 surface draw 换 key），否则把改写的段 splice 进 stream（`Painted::Spliced`；renderer `splice` 只 ingest 插入的 leaf）；transform root 首次出现、overlay 或 fade / scroll layer 出现、消失时整树 paint。opacity 在 (0, 1) 内变化同样是 patch。renderer 任一校验不成立都退回整 stream 上传，结果总是正确。2000 张卡片（8000 个节点）的页面 hover 一帧约 0.3 µs、淡入淡出约 0.5 µs，与卡片数无关（`paint_scale` bench）。校验条件、差分测试与数值见 `Viso_Rendering.md` §9.7。

为什么 transform 不需要 PAINT：transform table 只为 **transform root** 分配条目——scroll container 的内容，以及被 translate、scale 或 rotate 过的节点（第一次时成为 root，此后一直是）。paint 用 `Primitive::Transform(slot)` / `TransformEnd` 括住每个 root 的 primitive，并按先序记录 root，外层 root 总在内层之前。primitive 按绘制时的世界坐标写入，paint 只烘入平移；table 为每个 root 记下 *place*——从绘制时的位置到现在位置的仿射（`Affine2`）：只平移时是按 device pixel 吸附的位移差，转动（`Turn`：绕 `transform_origin` 先缩放、再顺时针旋转）时是完整仿射。之后只有 root 移动的帧，UI 重算 table（每个 root 一次合成，与内容多少无关），renderer 在 encode 时把 place 写进每个 built-in 都读取的 32 字节 inline view uniform（`viewport`、`shift`、`axis_x`、`axis_y`），root 内打开的 clip 跟着移动。instance 不带 transform id，不重写，不上传：代价按 draw 计，不按 primitive 计，也不需要 storage buffer（GLES 3.0、CpuRaster 同样适用）。不同 root 的 draw 不合批。普通节点的 layout 变化会改变它的世界坐标，这本来就属于 PAINT。retained drawing（`Content::Drawing`，ADR 0053）同理：共享 primitive 只在 paint 时映射到节点的盒，随 root 平移、缩放、旋转不重画；mesh 顶点的 `extrude` 在 place 之后按屏幕点加上，root 的缩放不改变线宽与 AA 边。

为什么 transform 不需要 HIT_TEST：屏幕上的 rect 不存储，由 `bounds` 加上方 root 的 place 推导；hit test 自顶向下携带仿射，把指针映射回每个节点自己的布局坐标（转动的节点在它画出的位置命中），滚动、平移、转动后都无需重算。

看到什么就命中什么：paint、hit test 与 semantics 用同一条规则。只有 scroll viewport 与设了 `clip` 的节点裁剪内容（`clip` 按节点圆角裁剪，圆角外的指针不命中子节点）；不裁剪的容器照常画出越过自身 box 的子节点（比自身矮的 Column、平移出去的子节点），这些子节点也在画出的位置被命中。每个节点存一个 `reach`——自身 box 并上每个显示中、非 overlay 子节点的 reach（按子节点的 translate 与 turn 放置，外扩一个 device pixel 容纳吸附），裁剪子节点的节点与 hidden 节点的 reach 就是自身 box。layout 在重排的 subtree 上自底向上重算 reach，再沿祖先上传，遇到 box 前后都包住改变量的祖先即停；translate 与 turn 只做同样的上传（通常第一层就停），不触发 layout。hit test 按 reach 剪枝、按自身 box 判定命中；半透明节点的 `Layer` 覆盖它的 reach，所以淡出中的容器不会裁掉越界的子节点。overlay 画在顶层：hit test 先按上次 paint 的顶层顺序倒序试 overlay（不受上方 scroll 裁剪），再走场景树并跳过 overlay 子节点。hidden 节点不命中；opacity 为 0 的节点仍占位、仍命中。`NodeStore::seen` 给出节点未被任何 scroll viewport 裁掉的部分：辅助技术的点击与 UI scenario 都点它的中心，完全滚出视口的节点报告为未显示。

例外（必须显式处理）：
- **place 无法表达的内容**：offscreen layer（ROI 按绘制时裁剪）、backdrop / material（采样背后内容）、用户 program（自写 uniform）会污染所在 root；scissor 只能跟随平移，转动的 root 内打开的 clip 改经 offscreen layer 合成，含 scissor clip 的 root 开始转动也算无法表达。移动被污染的 root，或帧内存在 backdrop capture 时移动任何 root，`set_transforms` 返回 `NeedsUpload`，该帧按新 place 重新 paint 并上传，结果总是正确；
- **可见集变化**：paint 发出 root 内的全部节点，滚入视口的节点总有 retained paint；VirtualList 的新行走 LAYOUT 路径（ADR 0008）；
- **glyph 像素对齐**：只平移的 place 按 device pixel 吸附（scale factor 取 length 环境），A8 glyph raster 不因小数偏移失效；转动的 root 不吸附，其中的 A8 glyph 随 place 缩放、旋转采样。目标（尚未实现）：按 Text 规范的 promotion 规则（§37.3）把持续转动的 glyph run 换成 MTSDF lane，静止后回到 A8。

transform 只跟随 UI 父链，不存在以任意矩阵引用形成的 transform parent。overlay 只改变绘制顺序与裁剪：它在原位布局、画在顶层、不受上方 scroll 裁剪，位置仍随父链的变换；anchored layer 由布局按锚点放置（`viso_ui::anchored`）。

目的：滚动和 transform animation 不因层级传播而触发布局。

---

## 20. Transaction 与 batch

写入不立即 flush。Rust 侧每次 `StateStore::set`（经 `EventCx`、`EffectCx`、frame client）只把 cell 记进本帧的 pending 写集（`StateSlot::pending` 去重，O(1)、无分配）；一帧内全部输入派发、continuation 与动画推进之后，`FlushStateTransactions` 阶段由 `settle_states` 一次 drain：依次跑 computed、binding、structure hook、semantic projection，并把被唤醒的 effect 排进 `RunEffects`。同一帧的多个事件、多次写入合并成一次 flush：

```rust
state.a.set(...);
state.b.set(...);
state.c.set(...);
// 帧内只 flush 一次
```

DSL 侧每次 Action / Event Handler 调用自动开启一个 behavior transaction（嵌套 Action 复用最外层），提交时写回 cell，运行时故障按 undo log 回滚；Effect body 写 State 必须显式 `transaction { ... }`（DSL §35、§37）。Rust 侧不需要显式 `transaction` API。

### 20.1 Cycle detection

循环在三处截断，开发模式都给出诊断而不是 runtime hang：

- **编译期**：DSL `computed` 之间的依赖环报 `E2105`，消息列出完整环路（`a -> b -> a`）；
- **求值期**：behavior VM 的 computed 缓存条目求值期间标为 `Evaluating`，再次到达即 `ReactiveCycle` 运行时故障（`E4202`）；
- **flush**：一帧的 flush 至多跑 `SETTLE_ROUNDS`（16）轮，仍有写入即停，丢弃尚未传播的变化并报告 `ReactiveCycle`（`E4202`：被丢弃的 cell 与它们会再唤醒的 effect 节点）；effect 连续 16 帧只因 effect 的写入而重跑视为跨帧环，跳过并报告其节点。adaptive 环境与布局之间的环至多 `ADAPTIVE_ROUNDS`（8）次 layout（`AdaptiveCycle`）。

依赖用失效与值比较判断，不用 version stamp。Rust 侧 computed 只读（`ComputeCx` 没有 `set`），本身不能成环。目标（尚未实现）：flush 的 `E4202` 报告目前只给出 `StateId` / `NodeId`，开发模式应映射回 State 名与源位置。

---

## 21. Computed / Effect

### Computed

必须可缓存、无副作用（DSL §43）：

```viso
computed full_name = format("{} {}", first, last);
```

DSL computed 由 behavior VM 记忆（`viso_behavior::memo`）：编译期为每个零参数 computed 求出它（含其调用的函数）读取的 state / input slot 并建立反向索引；实例缓存已求值的 computed，写入某个 slot 只清空读它的条目，依赖未变就不重算；调用闭包值的 computed 读集无法静态确定，不缓存。Rust 侧 `ComputedStore` 的派生只读（`ComputeCx`），每次求值经 `DepCursor` 重新记录依赖，被依赖写入唤醒后重算，值不变则不标脏下游节点（memo boundary）。

### Effect

副作用显式进入生命周期（DSL §37、§91）：

```viso
effect load when (user_id) {
    start load_user(user_id);
}
```

Effect 支持：

- cancellation：节点释放、分支卸载时取消；effect 启动的 Task 归节点所有，一并取消；
- cleanup：下次重跑前、组件销毁前或热重载替换前恰好执行一次；
- dependency change restart：依赖由 `when` 显式给出、按值比较，变化时先 cleanup 再重跑；body 读到未列出的依赖报 `E4201`；
- 运行时机：flush 只把 effect 排队，整帧在 `Submit` 之后的 `RunEffects` 阶段运行，每个 effect 每帧至多一次，它的写入是下一帧的 transaction；
- hot reload：替换代码前以旧代码取消全部 effect、各 cleanup 运行一次，替换后重新挂载（`mount` / `mount_and_change` 再运行一次，`change` 等下一次变化），不重复产生副作用。

Rust 侧入口是 `NodeStore::add_effect(node, when, body)`（`EffectStore`）：body 可返回 cleanup，经 `EffectCx` 写 state、启动或取消 task。

Render/View 默认保持纯净：View 与 Computed 中的 `start` 报 `E2502`，不在 view evaluation 内启动 I/O。

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

（DSL 名；Rust 为 `BuildCx::{flex, stack, absolute, grid, scroll}`；虚拟集合见 §24。）内部对应 `viso_ui::layout::LayoutInput::{Flex, Stack, Absolute, Leaf, Scroll, AbsoluteRows, Grid}`。

布局单位是 `Dp`（density-independent）。公开 API 区分两件事：

**长度族**（DSL 字面量，全部 resolve 为 `Dp`；规范见 `Viso_DSL_1.0.md` §19）：

```text
dp  px  sp  em  %
1px = 1 / scale_factor dp      c sp = text_scale_curve(c) dp（线性平台为 c × text_scale）
1em = 节点 Resolved Font Size   % 按 Property Schema 声明的 percent_basis 解析
```

跨单位加减得到 `MixedLength`，lower 为 `LengthTerms { dp, px, sp, em, pct: F32 }`；没有 min/max/clamp。不支持 rem、vw/vh、ex/ch/lh、物理单位；`fr` 不是长度单位，只是 Grid track 值。Layout 热存储只保存折叠后的 `Length`：`px` / `sp` / `em` 项在任何 pass 读取前并入 `Length::Relative { fixed, pct }` 的 `fixed`；只有带这些项的节点在冷表里保留 `LengthTerms` 与其依赖掩码，scale factor、text scale、font size 变化时只重折叠受影响节点（`viso_ui::length`）。值域、插值与像素对齐见 DSL §19.8–§19.10（ADR 0033）。

**Sizing mode**（每轴如何求尺寸，`viso_schema::layout::Length`，`viso_ui::layout` re-export）：

```text
Fixed(dp)                 // 精确尺寸
Relative { fixed, pct }   // fixed + pct × percent basis（百分比与混合长度）
Fill { weight }           // 主轴剩余空间按权重分配；Grid 中对应 Fr
Fit                       // 内容尺寸；宽度可压缩到 min-content
MinContent / MaxContent   // 宽度取最窄不溢出宽度 / 不换行宽度，高度取内容高度
```

最小/最大尺寸与 margin 不是 sizing mode，而是可选的冷表 `BoxRules { min_width, max_width, min_height, max_height, margin, padding_pct }`。

Grid track 使用独立的 `TrackSizing { Fixed, Fr, Auto, Percent, Minmax, FitContent, FlexMin, Relative, Length }`（ADR 0009；`Length` 是作者长度，layout 前由 store 折叠为 `Fixed` / `Percent` / `Relative`）。

hidden 子节点退出 flex 布局：测量为零、不占 Fixed/Fit 主轴空间、不分 Fill 份额、两侧不留 gap（同 CSS `display: none`），布局为容器游标处的零尺寸框。

内部使用专用的直接算法，不引入通用 constraint solver（ADR 0003）。

### 22.1 Measure / Layout 协议

不下传 box constraints，两遍完成：

```text
Measure（自底向上，后序）
  每个节点得出 Measured { w, h, min_w }：自然尺寸与最窄不溢出宽度，
  只依赖自身请求、内容与子节点的 Measured
      ↓
Layout（自顶向下，先序）
  父节点把自身 box 分给子节点：Fixed / Fit 先占，Fill 按权重分剩余，
  BoxRules 夹取，再按 Align / Justify 放置，写 bounds
```

百分比按父 box 在该轴是否 definite（`Basis`）解析；基准不定时报告 `E3105`。宽度决定高度的文本（soft wrap）不进 measure：layout 给可换行文本叶子分配的宽度与其 shaped 宽度不同时记下 reflow 请求，facade 按该宽度重新 shape 后再 relayout，轮数有界（ADR 0027）。

### 22.2 单遍 Cursor Layout 的性能思想

Row/Column 是单轴 direct flex：一遍累计 fixed/fit 与 fill 权重，一遍分配剩余空间，一遍对齐放置；热路径只用调用方提供的 scratch，不按节点分配。

内部游标状态、特殊 batch 与约束约定不作为普通用户必须理解的 API。

目标（尚未实现）：换行排列的 Flow 容器（标签、chip 行）——主轴放不下时换到下一行，行的交叉尺寸取该行最大者；当前只能用 Grid 的 `repeat(auto-fill, …)` 做等宽换行。

> **公开稳定布局语义，内部算法以 benchmark 选择。**

---

## 23. Measure cache

每个节点的 `Measured` 跨帧保存在 store 中。measure 不接收约束，所以缓存有效性就是 dirty bit：内容、长度、字体、子节点增删或隐藏变化时标 `MEASURE`，`MEASURE` 沿父链上传到 layout boundary（§23.1）；没有 `MEASURE` 的节点，其 `Measured` 一定有效。`NodeStore::relayout_dirty` 从 MEASURE / LAYOUT worklist 归约出最浅的 redo 根（§19.1），只重做这些子树，其余节点的 `measured` / `bounds` 原样复用；没有 layout dirt 的帧不访问任何节点。宽度相关的文本结果由 `Content::Text` 记录的 shaped 宽度与 viso-text 的 paragraph cache 复用（§37.4）。

不以“前一帧已经计算”作为复用条件：复用只由 dirty bit 决定，差分测试逐帧比较增量 relayout 与整树 layout 的每个 bounds。

目标（尚未实现）：redo 根之内的 `layout::measure` 仍递归整棵子树；没有 `MEASURE` 的子节点应直接复用其 `Measured`，不再进入（ADR 0003 推迟的 measure-cache hit-skipping）。

### 23.1 Layout boundary

`MEASURE` 停在自身尺寸不随内容变化的祖先：两个轴都是 `Fixed` 的节点，或 `Absolute` 容器（它的子节点不参与它的尺寸）。父节点固定、只是子节点内容变化时，只重做该子节点本身（§19.1）。VirtualList 的 canvas 两轴 Fixed，行 remount 因此不会重排列表之上的祖先（ADR 0008）。

这对复杂桌面 UI、大列表、编辑器非常重要。

---

## 24. Virtualized List

`List`（`viso_ui::virtual_list`，部件 `VirtualList`，ADR 0008）是一等组件，不是 Column 的包装：scroll viewport 的唯一内容子节点是一张按全部逻辑项估算尺寸、两轴 Fixed 的 `AbsoluteRows` canvas，只挂载可见窗口与 overscan 的行。

- **visible range**：`HeightTree`（逐项高度上的 Fenwick 树，O(log n) 前缀和与反查）；
- **item key**：`ItemKey` / `key_of`；keyed list 按 key 匹配幸存行，重排时保留行宿主与其状态，无 key 时以逻辑下标为身份；
- **node recycling**：离开窗口的宿主进 pool，行按模板实例复用 block（§18.1）；新窗口与旧窗口重叠时只回收、挂载两端；
- **estimated extent / variable height**：未测量的行取 `HeightCache` 的实测均值，行挂载并布局后 `absorb_measurements` 写回实测高度；
- **anchor preservation / scroll correction**：高度模型变化后，下一次 reconcile 按锚定的逻辑行重新定位滚动偏移；
- **incremental create/destroy**：行 remount 不标 STRUCTURE（§19），窗口内滚动只走 TRANSFORM；
- **accessibility**：挂载的行宣告 1 起的序号与总数（AccessKit `position_in_set` / `size_of_set`，web `aria-posinset` / `aria-setsize`）。

lanes（`LazyGrid`、masonry）、sticky header、分页与吸附、拖动重排、数据表格见 §45。

挂载节点数只与视口与 overscan 有关：100k/1M logical items 不对应 100k/1M mounted Nodes。

目标（尚未实现）：辅助技术的滚动动作（AccessKit `ScrollDown` / `ScrollUp` / `ScrollIntoView`、iOS `accessibilityScroll`）——当前 viewport 不接受，屏幕阅读器只能经焦点移动到达未挂载的行。

---

# Part VIII — Input、Focus 与 Gesture

## 25. 事件路由

```text
RawEvent（viso-platform，逻辑点）
    ↓
InputSample（viso-runtime 归一化，按帧时钟打时间戳；facade 换算为物理像素）
    ↓
PointerRouter：Hit Test + 手势竞技场        KeyRouter：焦点节点
    ↓
Capture（root → target 上方）
    ↓
Target
    ↓
Bubble（target 上方 → root）
```

任一处理器 `stop_propagation` 即结束整条路径。Enter / Leave 由路由合成，只投递给进出的节点，不走链（ADR 0022）。指针锁定期间不做命中测试（§25.2）。

手柄不进入 UI 路由：runtime 在帧首轮询，facade 只把变化交给持有输入焦点、且没有 UI 节点接走按键的窗口的 frame client，不驱动 UI 焦点导航（ADR 0052，§48.1.2）。

### 25.1 Typed event

每个节点至多两个冷处理器：`PointerHandler` 与 `KeyHandler`（都是 `Box<dyn FnMut(&mut EventCx)>`，经 `BuildCx::on_pointer` / `on_key` 注册，不进热 SoA 列）。指针事件只驱动指针处理器，键盘、IME 与焦点类事件只驱动 key 处理器。处理器从 `EventCx` 取类型化载荷——`pointer()`、`key()`、`ime()`、`drag()`、`pinch()`、`long_press()`、`wheel()`、`focus_change()`、`pasted()`、`drop_event()` 等，各返回 `Option`——没有让每个组件 match 全世界的万能 `Event`。DSL 的 `on` 声明经 `EventRoute` lower 到同一处理器。

布局后的状态报告不是输入事件：`Notice`（`viso_ui::notice`：`Scroll`、`VisibleRange`、`AnimationEnd`、`Asset`、`Video`、`Fullscreen`、`Size`）由 `BuildCx::watch` 订阅，布局后按变化每帧至多投递一次，处理器的写入进入下一帧的 transaction。`Size` 在节点首次布局与尺寸变化时报告（ADR 0053）。

### 25.2 Pointer capture

内建（`viso_ui::input`）：

- **pointer capture**：`EventCx::capture_pointer(id)` / `release_pointer()`，捕获期间该指针的样本只投递给捕获者；
- **drag threshold**：鼠标 `MOUSE_SLOP`（4pt），触摸 `TOUCH_SLOP`（8pt）；
- **cancellation**：被平台取消的触点以 `Leave` 结束并释放捕获；被认领的指针取消原持有者；
- **multi-touch identity**：`PointerId`，鼠标恒为 `PointerId::MOUSE`，每个手指或笔触点自 Down 到 Up 各有一个 id；
- **hover enter/leave**：祖先链上的每个节点只收一对（ADR 0022）；
- **wheel/trackpad precision**：滚动样本带精确增量与手势相位 `ScrollPhase`（`None` 为滚轮刻度，`Began` / `Changed` / `Ended` 为手指，`Momentum` / `MomentumEnded` 为系统惯性）。接走滚轮的节点（`NodeStore::set_takes_wheel`，读 `EventCx::wheel()`）在其上方的 viewport 之前收到每个滚轮与触控板样本；它与指针之间的 viewport 照常滚动（ADR 0053）。

**光标**：节点经 `NodeStore::set_cursor` / `BuildCx::cursor`（DSL `cursor`）命名鼠标在它及其子树上显示的光标（`CursorIcon`），离鼠标下节点最近的命名者胜出，捕获期间保持捕获开始时的光标。光标可以是系统形状，也可以是应用注册的图像：`CustomCursor::new(CursorImage)` 把直通 alpha 的 sRGB RGBA 像素（每边至多 `MAX_CURSOR_SIDE` = 256 像素）、热点与“每逻辑点几像素”的 scale 登记进进程级注册表，换回一个紧凑 id（`NonZeroU32`，`CursorIcon::Custom(id)` 仍是 `Copy + Eq + Hash`）；同一图像再登记得到同一 id，重建视图不增长注册表。`viso::cursor::decode` 把编码图像（PNG 等资源格式）读成 `CursorImage`。backend 在某窗口第一次显示某图像光标时才从注册表取像素构建 OS 光标并缓存（按 id 下标的稠密表 `CursorCache`，失败也记下不再重试），之后只是换用：macOS `NSCursor initWithImage:hotSpot:`（sRGB bitmap，按 scale 定点尺寸），Windows `CreateIconIndirect`（32 位 alpha 位图，按窗口 DPI 各建一份），Wayland 共享内存 ARGB buffer 的光标 surface（整数 buffer scale，补齐到整点），X11 Render `CreateCursor`（ARGB32 picture，按窗口 scale），KMS 硬件光标 buffer（预乘、随面板旋转），Web CSS `url(data:image/png…) x y` 加 `image-set` 高密度版本（浏览器读不懂的值不覆盖前一个），Android `PointerIcon.create(Bitmap, x, y)`（activity 按 id 保留 icon，像素只传一次）；iOS 没有图像光标。`PlatformCaps::custom_cursors` 为 false 的平台以默认箭头代替。光标变化在输入路由后立即经 `FrameDriver::after_input` 推给平台，不等下一帧（只移动鼠标、其余不变的输入不会请求帧）；帧尾同样同步一次，只在变化时调用 `PlatformApp::set_cursor`。

**指针锁定**：节点经 `NodeStore::lock_pointer(id, true)` / `EventCx::lock_pointer(id)` 请求把鼠标指针锁给自己（游戏视角、3D 视口环绕），`unlock_pointer` 放开。平台接口是 `PlatformApp::set_pointer_lock(window, lock)`，能力位 `PlatformCaps::pointer_lock`；锁定期间光标隐藏并停在原处，鼠标的移动不再以 `RawEvent::Pointer` 移动报告，而是 `RawEvent::PointerMotion { window, dx, dy }`（逻辑点，右、下为正，屏幕边缘仍持续；backend 可把队列尾部同窗口的相邻 motion 相加合并），按钮与滚轮照常到达（位置停在锁定点）。每次锁定状态变化（应用放开、窗口失焦、浏览器 Escape、平台拒绝）报告为 `RawEvent::PointerLockChanged { window, locked }`。runtime 把 motion 换成物理像素的 `InputSample::Motion`，锁定变化经 `FrameDriver::on_pointer_lock` 交给 driver。UI 侧 `NodeStore` 只记一个冷槽：请求者、平台是否持有锁、待发的请求；facade 在 `after_input`（锁请求仍在按下事件的处理中，满足浏览器的用户手势要求）与帧尾取走请求交给平台，平台没有该能力时立即以“锁已结束”回答。锁定期间鼠标的样本整个路由给持锁节点（不做命中测试、悬停与捕获），motion 以逻辑点增量交给持锁节点的 key handler（`EventCx::pointer_motion`），锁的生效/结束同样交给它（`EventCx::pointer_lock_change`）；Escape、窗口失焦、持锁节点被释放都会结束锁并告知持锁节点一次。macOS：只在应用活跃且窗口为 key 时锁定，把光标移到视图在屏幕可见区内部分的中心、事件抑制间隔置零、`CGAssociateMouseAndMouseCursorPosition(false)` 冻结光标并 `NSCursor hide`，移动事件读 `deltaX/deltaY`（扣掉移位本身带进下一个硬件增量的距离），光标若被系统放回关联则移回锁定点，失焦/关窗时恢复。Web：`canvas.requestPointerLock()`（拒绝的 promise 被吞掉，`pointerlockerror` 报告结束），`pointerlockchange` 以 `document.pointerLockElement` 是否为 canvas 报告状态，锁定期间 `pointermove` 读小数 `movementX/Y`。Windows：只在窗口是前台窗口时锁定，`RegisterRawInputDevices` 注册原始鼠标输入、`ClipCursor` 把光标夹在客户区中心的一个像素上并清空光标形状，`WM_INPUT` 的相对计数（一计数按一物理像素）换成逻辑点，`WM_MOUSEMOVE`/`WM_MOUSELEAVE` 锁定期间不再报告，窗口移动/缩放时重新夹住，`WM_KILLFOCUS`、销毁时放开。Wayland：需要 compositor 同时提供 `zwp_pointer_constraints_v1` 与 `zwp_relative_pointer_manager_v1`（据此报告能力位），只在指针位于窗口且键盘焦点在窗口时请求 oneshot `lock_pointer`，`locked` 事件才算生效并隐藏光标，`relative_motion` 的加速增量即 motion，`unlocked`、键盘离开、seat 失去指针时结束。X11：只在窗口持有键盘焦点时锁定，`XIGrabDevice` 以隐形光标抓取主指针，根窗口上为主设备选择 `XI_RawMotion`，原始事件的处理后数值（valuator 0/1）即增量，每次之后把指针移回窗口中心，锁定期间窗口自身的 motion/leave 只是移回中心的回声（滚动 valuator 照常报告）；服务器对抓取设备的客户端会把同一个原始事件送两次，按时间与序号去重；失焦、关窗时放开。KMS：libinput 的相对移动在锁定时直接成为 motion，光标不动并隐藏，会话暂停时结束。Android（API 26）：`View.requestPointerCapture()`（窗口无焦点或系统版本不足时立即报告拒绝），`onCapturedPointerEvent` 把 `SOURCE_MOUSE_RELATIVE` 的增量（含历史样本）交给 native，按键以锁定前的鼠标位置照常作为鼠标样本，`onPointerCaptureChange` 报告状态。iPadOS 14：视图控制器的 `prefersPointerLocked` 跟随请求（`setNeedsUpdateOfPrefersPointerLocked`），`UIPointerLockStateDidChangeNotification` 读场景 `pointerLockState.isLocked` 报告状态，锁定期间为 GameController 的每个 `GCMouse.mouseInput` 设 `mouseMovedHandler`（y 轴取反）；没有连接鼠标时立即拒绝；能力位只在 iPad 上为真。

### 25.3 Gesture arena

触摸与触控板手势由框架统一裁决，组件不各自 hack（`viso_ui::input` 与各识别器）：

```text
Press / Tap   落点节点的按下与抬起
Clicks        连击计数（double / triple）
LongPress     long_pressable
Drag          draggable，可限定单轴（set_drag_axis）；松手速度足够时为 swipe
Pinch         pinchable：缩放、旋转与焦点平移是同一个 PinchSample；来源为两根触点或触控板 magnify / rotate
Pan / Scroll  路径上的 scroll viewport：按主方向锁轴，惯性沿同轴嵌套视口先内后外传递
```

一根触点在按下时为路径上的识别器各武装一份，在越过阈值的那次移动上裁决；处理器以 `capture_pointer` 显式认领（自定义手势由此直接读原始指针序列），落败方收到 `Leave`。鼠标不平移，只在拖动、长按与按下之间裁决。完整规则见 §45「手势竞技场」。

---

## 26. Focus / Keyboard / IME

焦点是每个窗口的一个槽（`NodeStore::focused`），不另建焦点树：

```text
Focusable       节点标记（BuildCx::focusable），默认 false
TabOrder        树的先序；roving 组（viso_ui::roving）是一个停靠点；带链接的文字逐个链接停靠
FocusScope      一个范围槽（set_focus_scope）：打开的模态把 Tab 限制在自身子树
KeyboardFocus   键盘与 IME 事件沿焦点节点的祖先链 capture → target → bubble（KeyRouter）；
                无人 stop 时 Tab 移动焦点、Escape 关闭最内层轻量关闭层
TextInputFocus  焦点节点是文本框时 facade 调 PlatformApp::set_text_entry(Some(TextEntry))，否则 None 关闭 IME
```

焦点进出（`FocusChange`）、焦点可见与焦点环、访问键（`viso_ui::access`）、键盘滚动见 §45。

IME 是文本子系统的一部分，不是 TextInput 的平台 patch：

- **composition range**：`viso_text::ime::ImeComposition`（逻辑字节区间、clause、选中子区间、提交时替换的区间，带 revision；UTF-16 只在 composition 内桥接）由 `viso_ui::text_edit` 持有；平台以 `RawEvent::ImePreedit { text, caret }` 更新、`RawEvent::Text` 提交，空 preedit 为取消；
- **candidate rect**：`TextEntry::caret`（逻辑点），取自 retained caret map；
- **selection**：文本框自己的选区与编辑命令（`viso_ui::field_input`）；caret、选区与 composition 下划线由框架绘制；
- **platform IME activation/deactivation**：`set_text_entry`（带 `secure`、`InputKind`、`multiline`）与 `show_soft_keyboard`；
- **virtual keyboard inset**：`RawEvent::KeyboardInsetChanged` → `env.keyboard_inset`；
- **keyboard avoidance**：`KeyboardAvoiding` / `SafeArea` 区域（`Avoid::{SafeArea, Keyboard}`）按被覆盖的部分加 padding；锚定层避开安全区与键盘。

目标（尚未实现）：

- **surrounding text**：平台 IME 拿不到文本框的上下文与选区（macOS `attributedSubstringForProposedRange` 返回空、`selectedRange` 为 0，`TextEntry` 不带文本），重转换、自动更正与听写替换不可用；
- **clause**：`RawImePreedit` 只带 text 与 caret，`ImeComposition` 的 clause 与选中 clause 平台不填，长句输入看不到当前转换段。

---

# Part IX — Style、Theme 与 Animation

## 27. Style 不是运行时字符串字典

开发阶段可以写：

```viso
background: theme.colors.surface;
corner_radius: theme.radius.medium;
```

编译后两者都不再以字符串存在：Property 名在编译期解析为目标 Widget 的输入槽、布局槽或外观入口；`theme.colors.surface` 是对 `theme` 环境单元中 `Theme` Record 值的类型化字段读取。

热路径不做字符串比较。

### 27.1 Style pipeline

```text
style / when（编译期展开到节点：Base 先应用，后写优先，节点显式 Property 覆盖全部 Style）
    ↓
每个 Property 一个外观入口：base 值 + 按选择器排列的 when 分支
    ↓
Binding 读取 theme / State / 交互修订单元
    ↓
setter 比较新旧值，只按该 Property 的失效合同标 DirtyClass
```

- 运行时没有 cascade、没有 Computed Style 对象、没有整份 style diff；差分发生在每个属性的 setter：值不变不标脏。
- 交互选择器（`hover`、`pressed`、`focused`、`focus_visible`、`selected`、`dragging`、`drop_over`）是 NodeStore 由路由器记录的事实导出的谓词（`NodeStore::in_state`）。事实变化时，Flush 开始提升一次交互修订单元（`interaction_cell`），只有读取选择器的外观入口重新求值（DSL U12.1）。
- Rust 控件的 resting / hover / pressed 盒样式是 `InteractionStyle`，在 STYLE worklist 上被重新选出。

普通帧若 state、theme、交互事实未变化，没有任何 style 工作。

### 27.2 Theme token

`theme` 是注入的类型化 Context，类型为 prelude 的 `Theme` Record；它的字段就是一等 token 族：

```text
colors.*
typography.*
spacing.*
radius.*
elevation.*
motion.*
```

字段与默认值见 DSL U12.2。Theme 值作为状态存储环境的 `theme` 字段发布；`set_theme` 原子替换并提升该字段的单元，读取 `theme` 的 Binding 按各自 Property 的失效类别重新求值，未读取的不受影响。未设置时发布 `default_theme()` 的浅色 Theme。

Widget 默认引用语义 token，不直接固化 desktop dark/light 颜色。目标（尚未实现）：`viso-widgets` 的 Rust 控件默认样式仍是固定的深色调常量，应改为读取同一个 `theme`。

---

## 28. Animation

Animation 不要求每帧重建 style tree，也不经过 Binding 求值。运行时形式（`viso_ui::animation`）：

```text
LookTransition { node, from: LookValue, to: LookValue, timing: Timing, velocity }   // transition 与转场
NodeAnimation  { Run::Move(LookTransition) | Run::Track(TrackRun), reduced, .. }   // animate / play
```

`LookValue` 是可动属性的封闭集合：Fill、Opacity、Radius、TextColor、Translate、Scale、Rotation、Width、Height、Check、Thumb。每个变体按自身至多四个分量插值，这就是 specialized evaluator，不做通用属性反射。

animation dirty 按属性限定：
- translate / scale / rotation 只标 `TRANSFORM`；节点第一次成为 transform root 时另 paint 一次（§19.2）；
- opacity / fill / 圆角 / 文字颜色 / 勾选进度只标 `PAINT`；opacity 跨过 0 或 1 时 stream 形状改变，整树 paint；
- 只有 width / height（`Fixed` 值之间）标 `MEASURE | LAYOUT | PAINT`。

`AdvanceAnimations` 阶段的 `NodeStore::tick_transitions` 单遍走平坦列表，直接写 NodeStore 的对应列（`translate`、`turn`、`opacity`、`style`），不分配；列表为空时帧循环可以停下（`is_transitioning`）。命令式动画（`animate` / `play`）只接受 translate、scale、rotation 与 opacity：它们移动呈现值，结束后显示 Model 值，因此永不触发布局。handler 经 `EventCx::animate(node, to, timing)` 发起，与其他 Store 请求一样在 handler 返回后应用。

**缓动**（`viso_math::Easing`，`viso_ui` 在原路径 re-export）：`Linear`、Penner 十族曲线（`Curve::{Quad, Cubic, Quart, Quint, Sine, Expo, Circ, Back, Elastic, Bounce}`）各取 `In`/`Out`/`InOut`，与 CSS `CubicBezier`（Newton 求解、二分兜底）；`EASE_IN`/`EASE_OUT`/`EASE_IN_OUT` 为三次族。曲线是无分配的纯函数，放在叶子 `viso-math`，运行时与编译器（Web 导出）共用同一实现；`Easing::named` 给出 DSL `Easing` 声明变体的顺序。会越界的曲线（`Back`、`Elastic`、越界的贝塞尔）让平移、缩放、旋转越过目标，颜色与不透明度在插值处夹回取值范围，尺寸与圆角不小于 0。Web 导出见 DSL U6.1。

**弹簧**（`viso_math::Spring`，`viso_ui` re-export）：`stiffness`/`damping`/`mass` 的阻尼谐振子，欠阻尼、临界与过阻尼三种情形各用闭式解给出任意时刻的位移与速度（不做逐帧积分，结果与帧率无关）；`with_response(response, ratio)` 按无阻尼周期与阻尼比构造，`SMOOTH`/`SNAPPY`/`BOUNCY` 为半秒周期、阻尼比 1/0.85/0.7 的预设；阻尼比下限为 0.01，因此每个弹簧都会停下。`Timing` 由 `delay` 与 `Pace`（`Eased { duration, easing }` 或 `Spring`）组成。外观转场把值拆成至多四个分量（颜色取预乘线性光的四通道，平移取两轴，圆角取 `dp` 与 `pct`），每个分量从起点偏移与初速度独立求解，夹取规则与缓动插值相同；判定收敛用能量界 `x² + (v/ω₀)² ≤ rest²`：弹簧只会损失能量，满足后位移不会再超过 `rest`，此时写入目标值并移出列表（帧循环随之停下）。转向时由 `LookTransition::velocity` 求出当前各分量速度（弹簧取闭式解的导数，缓动取曲线在当前 1 ms 内的斜率），交给新的弹簧移动：`NodeStore::transition` 转向、`resume_transition` 跨重建续上、`animate` 取代同一属性的运行中动画时都这样交接；有 `delay` 或按时长缓动的新移动从静止开始。勾选标记的进度（`LookValue::Check`，0 为未选、1 为选中，存在标记旁）也是外观值：`NodeStore::set_check_mark` 在同一形状换状态时把它过渡到新状态（方框、圆与 V 形 150 ms ease-out，开关为 0.28 s 周期的临界阻尼弹簧），每帧只标 PAINT，绘制按进度插值；标记跟随节点语义状态（V 形随 `expanded`，单选的圆随 `checked` 或 `selected`，其余随 `checked`），因此投影语义状态的控件无需另行驱动。Web 导出见 DSL U6.1。

**关键帧**（`viso_ui::Keyframes`）：`stops: Vec<Keyframe { at, value, easing }>`、每周期 `duration`、`delay`、`Repeat::{Times(n), Forever}`、`PlayDirection::{Normal, Reverse, Alternate, AlternateReverse}`。`NodeStore::play` 与 `animate` 共用 `run_animation`：同样只接受平移、缩放、旋转与不透明度，同样移动呈现值、结束时显示 Model 值并经 `ANIMATION_END` 通知结束（`target` 为最后一个停靠点的值），同一属性的新动画结束旧动画。运行中的 `NodeAnimation` 持有 `Run::Move(LookTransition)` 或 `Run::Track(TrackRun)`：`TrackRun` 在开始时把停靠点过滤为首个停靠点的属性、丢弃非有限的 `at`、夹到 `[0, 1]` 并稳定排序，之后每帧只做一次查找与插值、不分配；时间以 `f64` 秒累计，周期数取整数部分，按方向决定是否倒放，第一个停靠点之前从开始时的呈现值插入，最后一个之后保持。被弹簧动画取代时，轨道的当前速度按最近 1 ms 的差分交出。Web 导出见 DSL U6.3。

**减少动态效果**：`Timing`、`Keyframes` 各带 `ReducedMotion::{Instant, Keep}`（默认 `Instant`）。`NodeStore::set_reduced_motion` 由 facade 在系统外观变化时与 `env.reduced_motion` 一同写入，是唯一执行策略的地方：`transition`、`animate`、`play` 遇到 `Instant` 时间立即落到终值或以 finished 结束；`glide_to`（平滑 `scroll_to`/`scroll_by`/`scroll_into_view`、键盘滚动、跟随末尾、刷新保持位）直接 `set_scroll`，虚拟列表的平滑定位改走即时定位。惯性滚动、越界回弹与吸附滑行（松手后的直接操控延续）不受影响。开启时 `set_reduced_motion(true)` 把进行中的可省略转场写到终值、结束可省略的动画、把所有滑行落到目标。视图不再自行读取 env 决定是否插值，只把 `reduced` 字段传给 Store。DSL 语义见 U6.4。

**页面转场**（`viso_ui::navigation`）：`NodeStore::set_page(stack, page, PageTransition)` 只显示第 `page` 个子节点，其余由 Store 侧谓词 `paged_away` 移出布局、绘制、命中测试与语义树，与 `fold` 相同，不重建子节点，之后挂上的子节点同样折叠，子节点自己的 `hidden` 不被覆盖。两页以呈现值动画（`quiet`，不派发 `animation_end`）交换，不触发布局；进行中再次导航时从当前呈现值继续（弹簧带速度）。`PageTransition::{Native, Slide, SharedAxis, Fade, None}`；减少动态效果时 `Slide` / `SharedAxis` 改为 `Fade`。离开中的页记入冷列表 `leaving`，不参与命中，到位时移出并折叠。`EventCx::set_page` 以 `StoreRequest::Page` 延后执行，`BuildCx::page` 在构建时分页。系统返回动作 `Key::Back`（鼠标后退键、Android 返回、浏览器后退键）未被焦点链处理时，先关闭最内层瞬态层，否则交给 `NodeStore::back_target` 选出的最深分页栈（`KeyRouter::route_back`）。共享元素 `NodeStore::set_hero(id, tag)`：到达页布局之后（`fly_heroes`），同标签节点以平移 + 缩放的静默动画从源位置直线飞入，与页面同一 `Timing`；减少动态效果时不飞。各变体的参数、返回手势与 Web 导出见 DSL U7.5（`NavigationStack`）与 U4（`hero`）。

---

# Part X — Accessibility / Semantics

## 29. Semantics 从第一天进入 Node

节点的语义来自 NodeStore 中的三处，不另建平行结构：

```rust
// 作者写的：冷列（viso_ui::Semantics）
struct Semantics { role: Role, label: Option<String>, hint: Option<String>, value: Option<String>,
                   live: LiveRegion, hidden: bool, heading_level: Option<u8> }
// 控件投影的实时状态：侧列（viso_schema::SemanticState，ADR 0021）
struct SemanticState { checked, value, range, expanded, selected, position: Option<(u32, u32)> }
// 派生的：focused、disabled（含祖先）、由 handler 推出的动作、active descendant、validity、table facts、bounds
```

`NodeStore::derive_semantics` 把三者折叠成扁平的 `SemanticsTree`（先序，`nodes[0]` 为根）。没有作者角色的交互节点默认为 `Button`，其余为 `Group`。DSL 的 `semantics.*` 属性组、默认角色与规则见 DSL U8。

Semantics tree 与 UI tree 不完全一一对应：

- 隐藏、折叠 / 分页移出、`semantics.hidden` 的子树不进入；无名 `Group` 发布为 generic container，辅助技术跳过它；
- 一段文本中的每个链接是该节点下独立的 `link` 节点；
- 虚拟列表只发布挂载的行，每行带 `position`（1 起的序号与总数）；表格以 `TableFacts::extent` 报告含未构建行的行列数；
- 目标（尚未实现）：把多个视觉节点合并为一个语义节点（例如列表行的图标、标题、副标题读作一项）。

平台 adapter（ADR 0030；边界格式为 Viso 自有的 `SemanticsDelta`，ADR 0042）：

```text
macOS     accesskit_macos（content view）
Windows   accesskit_windows（UI Automation，HWND）
Linux     accesskit_unix（AT-SPI，D-Bus 线程）
Android   accesskit_android（host view）
iOS       Viso 自有 UIAccessibilityElement 容器
Web       Viso 自有 ARIA DOM 镜像
```

辅助技术动作（Focus、Click、Increment、Decrement、Refresh、Remove）合成为普通输入，控件不需要第二套激活协议。

### 29.1 性能

Semantics 只在 dirty branch 更新，不在每帧重建整棵 accessibility tree。

- 没有辅助技术监听时，每帧只做一次 `Option` 判断；
- 监听时，只有标了 `SEMANTICS`、`LAYOUT` 或 `TRANSFORM` 的帧在 `UpdateSemantics` 阶段重新派生，与上次发布的结果逐节点比较，只发布变化的节点、根与焦点；
- 目标（尚未实现）：只派生脏分支。当前的重新派生与比较都覆盖整棵树，成本与树大小成正比（ADR 0030 记录的改进点）。

---

# Part XI — Paint / Renderer

## 30. UI 不直接提交 GPU 命令

UI 只产生 `viso_render::Primitive` 流；GPU 命令只由 `viso-render` lowering 后交给 `viso-gpu`：

```text
UI paint（PaintIndex：每节点 drawable 区间、subtree 区间、所在 transform root）
   ↓ Painted::{Whole, Patched, Spliced}
Renderer::upload / patch / splice → retained stores（revision planes、paint order）
   ↓
lowering + order-safe batch planner → family scratch、segment（RenderChunk）
   ↓
InstancePool::sync（只上传改变的 slot，经 dirty-range coalescer）
   ↓
DrawList（RenderPass + DrawCommand）→ GpuBackend::encode / present
```

只移动 transform root 的帧不经过 ingest：`Renderer::set_transforms` 换 place 后 `submit` 重新 encode（§19.2）。同一份 primitive 流也是 PDF 导出（`viso-pdf`）的输入。

### 30.1 为什么不是每帧完整 DisplayList 重建

primitive 流保持 display-list 式的平坦形状（`Layer` / `LayerEnd`、`Transform` / `TransformEnd` 括号，不是递归树），但：

- 未改变节点复用 paint data：只改值的帧原位改写（`Renderer::patch`），改形状的帧只 splice 改变的 subtree（`Renderer::splice`），见 Rendering §9.7；
- primitive 在 retained store 中有稳定 slot，instance 按 slot diff 局部上传（Rendering §9.1–§9.3）；
- clip / layer / transform / paint / geometry 各有 revision plane（Rendering §8.2）；
- 大块几何整体保留（ADR 0053）：`Mesh` 以共享 buffer（`Arc<[MeshVertex]>` / `Arc<[u32]>`）加 `MeshPlace`（每轴 scale，再 shift）持有，store 对同一 buffer、同一 place 视为未变，不逐顶点比较；`Content::Drawing`（`viso_ui::Drawing`）把共享的 `Arc<[Primitive]>` 在 paint 时从 drawing space 映射到节点 box，按 identity 比较，只标 PAINT，节点移动或改尺寸不复制、不重建。

### 30.2 Primitive 分类

ADR 0001 固定的最小 primitive 合同是 `Quad` / `GlyphRun` / `Image` / `Path` / `Mesh` / `Layer`（§15.3）。`viso_render::Primitive` 在此之上有 analytic shape（`AnalyticRRect` / `AnalyticEllipse` / `AnalyticCapsule` / `AnalyticLine`）、`Gradient`、`AnalyticShadow`、`Frosted`（material）、`Shader`（用户 brush program）、`Scene`（3D scene slot 的离屏 target 作为图像合成，ADR 0047），只能紧跟 `Layer` 的标记 `ColorEffect` / `Blend` / `ClipCorners`，以及 transform root 括号 `Transform` / `TransformEnd`。`MeshVertex` 带 `extrude`（屏幕点偏移，draw 的 place 只旋转它、不缩放它），线宽与 AA fringe 在 root 缩放下保持屏幕尺寸（ADR 0053）。完整分类、lane 选择与 Layer 合成（opacity == 1 用 scissor clip；< 1 或圆角 clip 走 offscreen + composite，ADR 0002）见 `Viso_Rendering.md`；3D scene lane（glTF 2.0 模型、metallic-roughness PBR 与 image-based 环境光、方向光 cascaded shadow map、GPU 蒙皮、Preetham 天空、半分辨率 SSAO）见 Rendering §21.5。

### 30.3 Damage-driven present（所有 Device Class）

局部变化只重绘、只上屏受影响的区域。这对桌面/移动是功耗问题，对嵌入式是内存带宽问题（RGB565 800×480 全屏重绘约 750 KB/帧）。

- **damage 来源**：每次 upload 后，renderer 为 surface pass 的每个 paint-order 条目生成 64 位内容 key（retained store 条目的全部像素决定字段、lower 时的 context：origin / space / clip、所采样的 offscreen pass 内容与 pass 参数（rect、opacity、blur、color op、blend）、backdrop capture 下方内容、material 参数、program 读的时钟）与可触及的 rect。Submit 时按 transform root 的当前 place 与移动 clip 放置（mesh 按最大 extrude 外扩），再与上次 present 的列表按序 diff：相同的前缀/后缀，中间 32 条窗口内重新同步，所以插入/删除一行只 damage 那一行。每个未匹配条目贡献它的旧/新 rect，外扩 1px 抗锯齿边缘。只移动 root 的帧（§19.2）因此 damage = root subtree 旧 ∪ 新 placed bounds，且被 clip 限定。
- **合并**：合并为最多 4 个不相交矩形（每次挑并集面积增量最小的一对合并）；合并后面积超过 surface 的 60% 时退化为整帧重绘。size、clear color 变化、`set_partial_redraw`、`damage_all()`（surface 重建、纹理就地改写）都强制整帧。
- **GPU 路径**：backend 报告 drawable 的 buffer age（`GpuBackend::frame_age`：D3D12 flip-sequential、Vulkan 按 image 计数、`EGL_EXT_buffer_age`、CpuRaster/Headless）。age ≥ 1 时，surface pass 用 `LoadOp::Load` 保留像素，renderer 保留最近 4 帧 damage，重绘区 = 本帧 ∪ drawable 错过的 age − 1 帧；每个矩形先用 Replace blend 的 clear quad 清空，再把全部 surface draw 以「原 scissor ∩ 矩形」重放。age 未知（0，Metal / WebGPU 的 drawable）时整帧重绘，但仍然跳过空 damage、上报 damage。present 时附带 damage（`present_with_damage`：DXGI `Present1` dirty rects、`VK_KHR_incremental_present`、`EGL_KHR_partial_update`、KMS `FB_DAMAGE_CLIPS`），compositor 据此只更新这些区域。局部重绘与整帧重绘逐像素一致（`crates/render/tests/damage.rs`）。
- **CPU 路径**：只光栅化与 damage 相交的 tile（§30.4）。
- **空 damage 不 Submit**：不 begin frame、不 encode、不 present，GPU 不被唤醒，KMS 也不 page flip。
- **调试**：profiler counter `damage_px` / `damage_rects` / `full_frame_fallbacks`（`viso profile`）；`viso inspect frame` 的 `damage` 给出该帧重画的区域（`null` / `"whole"` / 矩形列表），Studio/Inspector 据此叠加 damage rect。`Renderer::last_damage()` / `damage_stats()` 是同一数据的 Rust 入口。

### 30.4 CpuRaster 后端

`CpuRaster`（`viso_gpu::cpu`）是唯一的 CPU 后端：Embedded-CPU 的 Tier-1 后端、没有原生 GPU 后端时的兼容路径、golden test / headless 的确定性参考。它消费与 GPU 后端相同的 `DrawList`，不新增 primitive 类型，不另起一套 renderer：

- 每个 built-in pipeline 由 `BuiltinShader` 标签选择其 shader IR 的 CPU 复述（SDF、AA ramp、采样约定），blend state 照常生效；用户 program 运行打包时附带的 `ShaderLang::None` 寄存器码（`CpuShader`），不存在缺 CPU 路径的 program；`embedded-cpu` profile 把包的 shader 语言固定为 `None`（§50A.3），缺少 CPU 代码的包在构建期即不满足 profile；
- 64×64 tile binning，只访问被 draw 触及的 tile，`LoadOp::Load` 的 partial redraw 只花 damage 的代价（§30.3）；band 在常驻 worker pool 上并行，输出与 worker 数无关；SSE2 / NEON / SIMD128 行 kernel 与 scalar bit-exact；
- 只被区域合成读取的 offscreen layer 推迟到 tile 内求值，不分配整块 target，像素与整目标绘制 bit-exact；
- 3D scene lane 同样执行：每个 target 一个 `f32` depth buffer、透视校正的三角形光栅化，着色与 GPU program 同一表达式（`viso_gpu::scene`）；
- 板卡 2D 加速器（i.MX PXP、Allwinner G2D、STM32 DMA2D 类）经 `viso_gpu::cpu::Blitter` 接 solid fill（pass clear），拒绝的部分由 CPU 完成，像素不变；
- 测量：`pixel_bytes()`（目标与 surface 像素）、`scratch_bytes()`（worker 窗口）、`deferred_layers()`；bench `cpu_raster_800x480_{full,damage_5pct}_{xrgb8888,xrgb8888_4t,rgb565}`。

按格式存储（XRGB8888 / RGB565 dither / `f32` HDR / A8 atlas）、analytic AA、path coverage（`Caps::coverage_paths`）、scanout 的实现合同见 Rendering §6.8.1。

目标（尚未实现）：
- 浏览器没有 WebGPU 时以 CpuRaster 画到 canvas（`putImageData`）；目前这种窗口不建 GPU surface，什么都不画；
- 从 Shader IR 构建期生成 Rust 片元函数，取代用户 program 热路径上的寄存器码解释。

---

## 31. Batching

用户绝不应该设置 `new_batch: true` 才保证普通文字出现在背景之上。

Renderer 自动根据以下状态生成 batch：

```text
pipeline family / user program slot
resource table（texture / atlas 的 bind group）
render target（pass index + class）
clip 与 transform space
blend / sample count / color target / depth-stencil class（预留）
```

### 31.1 Batch key

```rust
/// packed u64，低位到高位：
/// family 4 | blend class 4 | sample class 2 | color-target class 2 | depth/stencil class 2
/// | render target index 10 | resource table 24 | target class 2 | program slot 14
pub struct BatchKey(u64);
```

预留字段当前取唯一值 0；advanced blend 是独立 family（`AdvancedBlend`），不占 blend 字段。clip（`Option<Rect>`）与 `Space`（transform root + 移动 clip 链）不打包进 key，在 key 旁按结构比较（`batch::planner::joins`）；不同 root 的 draw 不合批。

### 31.2 Stable batching

避免为了减少 draw calls 而破坏 z-order：

- order-preserving adjacent batching：primitive 只并入紧邻前一个 draw，key、clip、space 都相等；
- 跨节点合并：除 ColorTransform / AdvancedBlend / Material 三种 composite 外，各 family 都可并入相邻 draw——image、glyph run、gradient 共用 atlas 时、用户 program 同一 program 时合为一个 draw；
- 目标（尚未实现）：越过不相交的中间 draw 合并（reorder-safe span，Rendering §9.6 规则 3）。

正确性优先于极端 draw-call 数字。

---

## 32. GPU instance buffer

built-in 的 instance 与顶点布局用 `#[derive(GpuPod)]`（`viso-macros`；ADR 0001 称之为 `GpuInstance` ABI）：

```rust
#[repr(C)]
#[derive(GpuPod)]
pub struct QuadInstance {
    rect_pos: [f32; 2],
    rect_size: [f32; 2],
    color: [f32; 4],        // straight linear RGBA
    radius: f32,
    border_width: f32,
    border_color: [f32; 4],
}
```

derive 要求 `#[repr(C)]`，字段只能是 `f32`、`[f32; 2|3|4]`、`u32`、`[u32; 2|4]`，生成 `const LAYOUT: InstanceLayout`（`offset_of!` 真值）；每个 backend 的 `create_pipeline` 用它对 shader 声明的 instance schema 校验，失败返回 `LayoutError`（§36.1）。

instance 不携带 transform：几何是绘制时的世界坐标，所属 transform root 之后的 place（平移与缩放 / 旋转的轴像）由每个 built-in 都读取的 32 字节 view uniform（`viewport`、`shift`、`axis_x`、`axis_y`）施加（§19.2）。滚动、平移、缩放、旋转 root 都不重写 instance，每个 instance 也不为此多付字节。

禁止依赖：

> “某个 Rust struct 从某字段开始后面的内存刚好全部可作为 GPU instance。”

### 32.1 Host / GPU data 分离

Widget 不定义 GPU instance：host 状态（hover、动画进度、缓存）留在节点里，paint 只发出 `Primitive`。需要自定义 GPU 数据时写 `.vs` 的 `shader` brush：节点发出 `Primitive::Shader(ShaderDraw { rect, program, params, textures })`，`params` 是按 Shader Layout Algorithm（`viso_shader::program::layout`）编码的一个 instance，只含 program 声明的字段；stride、uniform 大小与 texture / sampler 个数记在 `ProgramAbi`。

---

# Part XII — GPU RHI 与 Shader

## 33. GPU abstraction

`viso-gpu` 提供小而硬的 RHI，只有一个 `GpuBackend` trait：

```text
GpuBackend 值               设备 + 队列
BufferId TextureId SamplerId PipelineId BindGroupId SurfaceId   generation-safe 句柄
Frame                       begin_frame 取得，present 交还
DrawList                    RenderPass[]（target、LoadOp、可选 DepthAttachment、command 区间）+ DrawCommand[]
Geometry                    Generated | IndexedMesh | InstancedMesh
ProgramCompiler             任意线程编译用户 program，install_program 安装
Epoch / Fence / RetireQueue backend 内部的 deferred destruction
```

没有对外的 command encoder：renderer 把一帧 lower 成扁平 `DrawList`，backend 一次 `encode`。没有 compute pipeline，也没有 barrier API：pass 顺序就是同步（§34）。depth attachment 与 instanced mesh draw 只服务 3D scene lane（ADR 0047）。buffer / texture / pipeline 描述、格式、instance 与 program ABI、scene 布局在叶子 crate `viso-gpu-abi`，`viso-shader` 只依赖它，`viso-gpu` re-export。

不要把 UI 概念放进 GPU crate。

### 33.1 Backend 静态选择

默认：

```text
macOS/iOS → Metal
Windows   → D3D12（没有 feature level 11.0 的硬件 adapter 时用 WARP）
Linux     → Vulkan
Android   → Vulkan
Web       → WebGPU
Embedded Linux（GPU）   → GLES 3.0+（Vulkan 1.1 驱动成熟的 SoC 可选 Vulkan）
Embedded Linux（无 GPU）→ CpuRaster
其它、`headless` / `cpu` build → CpuRaster
```

Desktop Linux 与 Embedded Linux 的 target triple 相同（例如 `aarch64-unknown-linux-gnu`），因此 embedded backend 由 Footprint Profile 的 feature 选择，而不是由 `cfg(target_os)` 选择；仍然是编译期静态选择，一个 artifact 只含一个 backend。

GLES 3.0 是 Embedded-GPU 的 Tier-1：嵌入式 SoC（Mali、Vivante、PowerVR、VideoCore 等）的 GLES 驱动普遍比 Vulkan 驱动成熟。它在 Embedded-GPU 上是 Tier-1，在桌面 Linux 上只是 Tier-2 兼容路径。这不违反「不做最低公分母」：GLES3 backend 声明自己的 capability profile（无 compute、bind group 由 backend 模拟），renderer 按 capability 选择 lane，Tier-1 RHI 的语义不因此降级。

`Gles3Backend`（`viso_gpu::gles`，`gles` feature，Linux / Android 上替代 Vulkan 成为 `Backend`）：EGL 1.5 与 GLES 3.0 运行时加载、不链接；surface 为 Wayland、XCB、Android、KMS 的 GBM surface 或 pbuffer；built-in 与用户 program 都是 `viso-shader` 打印的 GLSL ES 3.00（一份源码，顶点半部在 `VISO_VERTEX` 下）；bind group、inline uniform 与用户 program 的 instance 由 backend 模拟（texture unit + sampler object、uniform ring、16 KB uniform block）；8-bit 窗口取 sRGB colorspace（`EGL_KHR_gl_colorspace`），没有时由最后的 output encode pass 编码（ADR 0048）；`EGL_EXT_buffer_age` / `EGL_KHR_partial_update` / swap with damage 给出局部刷新；面板旋转而 plane 不能旋转时在顶点阶段旋转。实现合同见 Rendering §6.8.2。

选择由 `viso_gpu::Backend` 类型别名按 `cfg` 在编译期完成，facade 按值持有，帧路径无 `dyn` 分派；详见 [ADR 0029](docs/adr/0029-gpu-backend-static-selection.md)。

源码层的统一接口：

```rust
pub trait GpuBackend {
    const SHADER_LANG: ShaderLang;
    type Compiler: ProgramCompiler;
    fn create_buffer(&mut self, desc: &BufferDesc) -> BufferId;
    fn create_pipeline(&mut self, desc: &PipelineDesc, layout: &InstanceLayout)
        -> Result<PipelineId, LayoutError>;
    fn begin_frame(&mut self, surface: SurfaceId) -> Option<Frame>;
    fn encode(&mut self, list: &DrawList<'_>);
    fn present(&mut self, frame: Frame);
    fn caps(&self) -> &Caps;
    ...
}
```

trait 只为源码层统一与冷路径泛型而存在；release target 静态选定 backend 并单态化。

禁止每个 primitive 在 runtime 上通过 `Box<dyn Backend>::draw()` 动态分派。

### 33.2 不追求最低公分母

统一 API 只统一真正共性的资源与同步模型。

平台能力通过 `Caps` 字段如实报告（`compute_dispatch`、`bindless_texture_slots`、`indirect_draw`、`coverage_paths`、`sampled` 等）；上层据此选择 lane：

```rust
if backend.caps().coverage_paths { /* 实心 path 走 coverage mask lane */ }
```

或使用 backend 专属 fast path（Metal memoryless depth、外部图像 / NV12 直接导入、DXGI `Present1` dirty rects 等），而不是为了抽象漂亮放弃性能特性。

---

## 34. Render Graph

offscreen 类 pass 由 `viso_render::graph` 编排。一帧的节点（`PassWork`）：

```text
Offscreen        opacity < 1、圆角 clip、color effect 的 layer
Blur             blur ladder 的每一级
ColorTransform   不能融合进 composite 的 color effect 链
BackdropCapture  backdrop 背后内容的 ROI 重绘
Surface          直接画到窗口的内容 + 各 layer 的 composite
```

Graph compiler 负责：

- pass dependency：校验每个读都落在已执行的 pass 上；RHI 没有显式 barrier，pass 顺序即 barrier / state lowering；
- 剔除无人读取的 pass，合并写同一 attachment 的相邻 pass，选择 load op；
- resource lifetime：由读边推出 transient target 的生存区间；
- transient texture reuse / attachment aliasing：`TransientTargets` 让 format、usage、sample count、尺寸档相同且生存区间不相交的虚拟 target 共用物理纹理；物理纹理跨帧保留，闲置 60 帧回收（Rendering §16.4）；
- topology hash 不变时复用上一帧的 plan（尺寸、颜色、同一档内的 blur sigma 变化都不重新编译）。

3D scene 的 pass（scene pass、阴影图集、半分辨率 AO）不是 graph 节点：slot 拥有持久 target，只在 frame 或尺寸变化时于 graph 的 pass 之前执行（ADR 0047，Rendering §21.5）。analytic shadow 不需要 pass。surface 不能编码时，output encode pass 在 surface pass 之后（ADR 0048）。

普通 UI 不要求业务开发者接触 render graph。

---

## 35. GPU memory / upload

使用：

- persistent buffers：每个 instance family 一个 grow-only `InstancePool`，CPU shadow diff 只上传改变的 slot，splice 只追加尾部；scene slot 的 instance buffer 按 2 的幂增长；
- 帧内 upload：D3D12 每帧 staging arena、Vulkan host-visible staging、GLES uniform / instance ring、WebGPU queue write；CPU 侧临时数据用 frame arena；
- texture atlas：A8 glyph、彩色 glyph、MTSDF、mask pages、gradient LUT；
- image cache：`viso_ui::asset::AssetTable`，facade 的 asset worker 解码，renderer 持有 texture；mip 链经 `create_texture_mipped` 一次上传；
- pipeline cache：标准 pipeline 在 device 初始化时按 `standard_manifest` 创建；D3D12 按源码缓存字节码，Vulkan 用 `VkPipelineCache`；用户 program 在 worker 上编译后安装；
- bind group cache：`(texture, sampler)` 对应的 bind group 跨帧复用。

稳态滚动列表只更新少量 instance range，而不是每帧重新创建 GPU buffer。

---

## 36. Shader compiler

管线（ADR 0017）：

```text
built-in：Rust 侧 IR builder（quad_ir、image_ir、glyphrun_ir、mesh_ir、scene program …）
用户：.vs 的 `shader` 声明 → DSL lossless CST、名字解析、类型检查 → viso_shader::program::Program
   ↓
Typed Shader IR
   ↓
Validation（类型、stage 使用、有界循环、ABI）+ Shader Layout Algorithm
   ↓
Backend Codegen
   ├── MSL（Metal）
   ├── HLSL SM 5.1（D3D12，`D3DCompile` 编译、按源码缓存）
   ├── WGSL（WebGPU）
   ├── SPIR-V（由 WGSL 经 naga 生成；built-in 冻结为 `.spv`，运行时不带编译器）
   ├── GLSL ES 3.00（GLES3）
   └── CPU 寄存器码（`ShaderLang::None`，CpuRaster）
```

用户 shader 与 Viso DSL 共用前端（lexer、lossless CST、module、名字解析、类型检查与 source span）；`viso-shader` 位于 `viso-dsl` 之下，带自己的 `Diagnostic` / `ProgramError`。GPU program 与 CPU 解释器都不共享 behavior VM。

### 36.1 ABI 验证

编译阶段验证：

- instance field offset、format、stride：`InstanceLayout::validate_against` / `validate_attrs`（`CountMismatch` / `NameMismatch` / `FormatMismatch` / `OffsetMismatch` / `StrideMismatch`），每个 backend 的 `create_pipeline` 都执行；
- alignment 与 scalar / vector type：`GpuPod` 只接受 4 字节对齐的标量 / 向量；用户 program 的布局由 Shader Layout Algorithm 唯一给出，`LAYOUT_VERSION` 进 layout key；
- uniform size、texture / sampler 个数、vertex stage 是否采样：`ProgramAbi`；
- vertex format：mesh 与 scene 顶点布局同样用 `GpuPod` 声明；
- shader-stage visibility：`Program::validate` 的 stage 规则（E8106）。

Mismatch 在编译 / 热重载时失败（热重载保留 last-good pipeline，`ShaderPipeline`），不允许 silent memory corruption。

---

# Part XIII — Text 系统

## 37. Text 是独立性能子系统

完整规范：`Viso_Text_Font_Runtime.md`（以下简称 Text 规范）。本节只列不可违反的合同；细节、数值与 benchmark 以 Text 规范和 ADR 0025–0028 为准。

### 37.1 字体来源与解析

- **不建立 framework-owned FontDB。** 普通启动与普通文字绘制不得扫描、枚举或解析整台机器的系统字体。完整枚举只允许作为显式请求的异步 cold service（`SystemFontCatalog`，例如 Font Picker）。
- 解析顺序：`FontRequest → Resolve Cache → App packaged/dynamic fonts → OS fallback → Missing Glyph policy`。App 字体优先，缺字继续进入系统 fallback。
- 系统字体经 `viso-text` 定义的 `SystemFontProvider` / `ColorGlyphRasterizer` trait 按需查询，facade 提供实现（`viso::system_fonts`）：macOS / iOS CoreText cascade、Windows DirectWrite system collection、Linux fontconfig（运行时加载）、Android `AFontMatcher`（否则读 `fonts.xml`）；WASM、headless 与未开 `system-fonts` 的 embedded build 没有系统字体。只有 Apple 由平台光栅彩色 glyph，其余平台从 face 的 `COLR` / `CBDT` / `sbix` 表经 `viso_text::raster_color` 绘制。negative cache 与 fallback 顺序在 `viso-text`；禁止 `text -> platform`（ADR 0026）。
- Native 可以完全不打包字体；WASM/Canvas 没有隐式系统字体，无字体时 shape 为空而不 panic。
- `assets/fonts/` 在 build time 由 `viso::packaged_fonts!()` 自动登记为 compact `FontManifest`，runtime 首次使用才 lazy load；网络/用户/文档字体经显式 `FontProvider` 注入。
- **输入只接受 SFNT（TTF/OTF/TTC/OTC）。** 框架不解码 WOFF2；调用方先解压（例如 `libs/woff2`）（ADR 0028）。

### 37.2 Fallback 与 Emoji

- CJK/混排 fallback 按 shaping run / cluster 进行，禁止逐字符扫描字体；`zh-Hans`/`zh-Hant`/`ja`/`ko` 是 fallback key 的一部分；首次 OS fallback 结果进入 bounded `FallbackPlan` cache。
- Emoji grapheme/ZWJ/VS16/肤色/旗帜/keycap 序列保持原子，不拆到多个字体。

### 37.3 Glyph representation

五种 `GlyphImageKind`：`MaskA8`（默认，稳定 UI/小字/CJK）、`ScalableMtsdf`（持续 zoom/scale/rotation 时异步 promotion）、`OutlineVector`（极端放大/高精度，retained）、`ColorRgba8`、`ColorVector`。硬规则：

- 变换稳定后异步回到精确 A8 Coverage；promotion/settle 有 hysteresis；纯 scroll translation、opacity、color、clip 不触发 promotion；
- 禁止 FontFace load 时预生成 distance field，禁止为 CJK 整套字体预生成；
- 各 representation 独立 residency pool 与 accounting；representation 决策挂在 retained run 上，不逐帧逐 glyph 重算。

当前运行时只绘制 `MaskA8` 与 `ColorRgba8`（`COLR` v0/v1 也在软件中画成 RGBA8 bitmap）。目标（尚未实现）：把 promotion 接入运行时——`viso_text::glyph_representation::RepresentationState` 状态机、MTSDF 生成与 `viso_render` 的 MTSDF atlas / `GlyphLane::Mtsdf` 都已存在，但 UI paint 总是发出 `GlyphLane::CoverageA8`，facade 不向状态机提供 transform 输入，MTSDF 与 OutlineVector pool 不持有像素；`ColorVector` 没有绘制路径。

### 37.4 Cache 边界

四层独立 cache（ADR 0025）：Font Resolve Cache、byte-budgeted Segmented LRU Font Face Cache（recency 不逐 glyph 更新）、Shaping/Paragraph Cache、按 page 做 frame-age/CLOCK 淘汰的 Glyph Atlas（禁止逐 glyph LRU 与正常压力下 `atlas full -> clear all`）。一层 eviction 不连带清空其他层；内存压力按冷到热逐级释放，不以 `clear()` 作为普通策略。

### 37.5 高刷新率与 world-ready 正确性

- **稳态帧不 shape。** Text/font/layout 未变时，稳态帧 0 system font query、0 font IO/parse、0 shaping、0 glyph raster/MTSDF 生成、0 per-glyph 分配/同步、0 BiDi/line-break/grapheme/CaretMap 重算。高刷滚动只更新 transform/clip。
- 大字体 parse、large paragraph shaping、可线程化 raster 进入 worker/staging；主线程只做有预算的 commit。
- 复杂文字以 UAX #29 / #9 / #14 + locale tailoring 为基线；logical text 是 source truth，BiDi visual reorder 在断行后按行执行；UTF-8/UTF-16 offset、grapheme、cluster、glyph index 使用不同 typed identity；caret/selection/hit-test/IME 共享 logical↔visual 映射；incremental 结果必须与 full recompute 等价。宽度感知 reflow 见 ADR 0027。
- Unicode 官方 conformance corpus（`GraphemeBreakTest`、`LineBreakTest`、`BidiTest`、`BidiCharacterTest`）与 CJK/RTL/IME golden corpus 进入 CI；核心 regression gate：系统字体数量不能让普通 App startup 产生线性 parse 成本。
- 目标（尚未实现）：`ShapedGlyph` 的 cluster 与 glyph id 仍是裸整数，未用 typed identity；`zh-Hans` / `zh-Hant` / `ja` / `ko` 的断行 tailoring 只有一个 ICU ja/zh bucket，尚不能各自区分（Text 规范 §29.1 未勾选项）。

### 37.6 Benchmark 覆盖（合同）

Text profiler counter 列表以 Text 规范为准。Benchmark / regression gate 至少覆盖：Native 无 packaged fonts；系统安装 3000 fonts 的 startup；CJK 10k chars cold/warm；mixed zh/ja/ko；Emoji ZWJ cluster；UAX #9/#14/#29 conformance corpus；BiDi mixed RTL/LTR + numbers + isolates；BiDi dual-caret / multiline selection / hit-test roundtrip；CJK kinsoku；Arabic unsafe-break reshape；CJK/RTL IME composition；font picker 500 faces scan；large CJK font memory pressure；WASM zero-font startup；WASM packaged SFNT first use；60/120/144/240Hz static/scroll/editor workloads。多数 gate 以计数器断言的测试实现（计 parse / query / shaping 次数，不计绝对时间），每一项的测试名、benchmark 与未达成项见 Text 规范 §29.1。

### 37.7 Ownership

`viso-text` 拥有 FontManifest 语义、resolver/fallback policy、Face SLRU、coverage accelerator、shaping/paragraph cache、Unicode integration、typed text position 与 caret/selection/hit-test/IME 映射、TextWork 调度、font revision/invalidation、glyph residency 合同与 profiler counters。标准算法复用成熟实现（`unicode-bidi`、`unicode-segmentation`、`icu_segmenter`、`rustybuzz`、`ttf-parser` 等，ADR 0025）；A8 coverage 的光栅与 baked page 格式在 `viso-glyph-a8`。facade 拥有系统字体 binding；`viso-render` 拥有 GPU atlas texture/page/upload 机制。

### 37.8 Embedded 字体策略

Embedded profile 下默认：
- 没有系统字体 provider（不链接 fontconfig/CoreText 等），只使用 packaged font；需要时以 facade 的 `system-fonts` feature 打开；
- **build-time subset**：字体按「静态字符串用到的字符 + `Viso.toml` 声明的 charset」做子集。动态文本可能出现子集外的字符时，必须声明 charset，否则构建期报 diagnostic，而不是运行时显示 tofu；
- **baked atlas（可选）**：UI 字号固定的产品在 device target 的 `fonts.bake` 声明字号，构建期把子集的 drawable glyph 闭包按这些字号预光栅化为压缩的 A8 page（`viso-glyph-a8` 的 "VBA8" 格式，8 字节对齐 static，原地读取）；运行时 glyph cache 先读 baked page 再 raster，pixel 与运行时 raster 逐字节相同。`fonts.rasterizer = false` 时 build 不链接 font rasterizer（facade 的 `rasterizer` feature），嵌入字体删去 outline 表（`glyf`/`loca`/`CFF `/`gvar`），只留 shaping 所需；任何 glyph 无 baked pixel 或 feature 与之不符都是编译错误，未声明的字号从最近的 baked 字号重采样。合同与实测见 Text/Font Runtime §3.6；
- 默认关闭 MTSDF / OutlineVector / ColorVector；ColorRgba8 emoji 可选；
- Unicode 数据表按声明的 locale 集合裁剪：Thai/Lao/Khmer/Myanmar 的换行 LSTM 模型（Unicode 数据中最大的一块）只在 device target 的 `locales`（缺省为 i18n catalog 的 locale）、静态文本或 charset 用到这四种文字时链接（facade 的 `complex-line-break` feature）；BiDi、grapheme 与 UAX #14 规则表是任何动态文本都需要的，不裁剪（Text/Font Runtime §3.7）。

以上都是 profile 的默认值，不是额外的语义：同一份 `.vs` 在 desktop profile 下照常使用系统字体 fallback。

---

# Part XIV — Viso DSL、热更新与语言工程

本 Part 只固定 DSL 与 Runtime 的边界合同。语言本身（词法、文法、类型系统、运行时语义、lowering）的唯一规范是 **`Viso_DSL_1.0.md`**（下称 DSL 规范）；Dev Runtime 与热更新协议的唯一规范是 **`Viso_Hot_Reload.md`**。二者与本节冲突时，以已接受 ADR 为准，其次以对应专项规范为准。

## 38. 定位：Viso DSL 是一等 Authoring Layer，不是 UI Runtime 的地基

- 依赖方向固定为编译器到运行时单向：`viso-dsl -> viso-schema <- viso-ui`（ADR 0042）。`viso-dsl` 只依赖 `viso-schema`、`viso-dev-protocol`、`viso-behavior`、`viso-shader`、`viso-lexer`、`viso-ende` 与 `viso-math`；`viso-dsl`、`viso-lsp`、`viso-ui-macros` 与 `viso-cli` 的传递闭包不含 runtime、platform 与 gpu（`cargo xtask check-deps`）。`viso-ui` 永不依赖 `viso-dsl` 或 behavior VM（§10）；把 behavior 挂到节点上的是 `viso-view`。纯 Rust 应用完全关闭 `hot-reload` feature 时，UI runtime、layout、renderer、widgets 的基础能力仍然成立。
- Release 加载路径只在 `viso_ui::aot`（`load_from_bytes`）与 `viso_view::load_static_view`，不得触达 `viso-dsl`（ADR 0016）。

### 38.1 三个 Rust 入口，一套语言语义

`ui!`（ViewFragment）、`component!`（ComponentDecl）、`view!(path)`（`.vs` CompilationUnit，路径相对调用文件）见 §0.1 与 ADR 0014；由 `viso-ui-macros` 提供，facade 再导出为 `viso::{ui, component, view}`（另有资源入口 `asset!`）。三者只允许**入口 production** 不同，必须共享 lexer/token model（`viso-lexer`）、component/native schema、name resolution、type/effect/capability checking、Typed HIR、Reactive IR、UI IR、Behavior IR、Shader IR 与 diagnostics/source maps。

它们是构建期 compiler frontend，不代表运行时宏系统，不允许每帧展开/rebuild UI。Release 中所有入口生成相同的 compact AOT descriptors/IR，启动时不 parse `.vs`，也不解析 Rust source。

### 38.2 语言 Surface 分层

语言能力按 Core / Standard / Advanced 分层（关键字分层见 DSL 规范 §12 与 ADR 0034，Proposed）。硬规则：

- Advanced Surface（用户定义 trait/impl、通用泛型、Template/Part 元编程、手写 native 声明、compiler plugins 等）不得阻塞 1.0 vertical slice，并在 Quick Start、formatter 示例与 AI context 中隔离；
- `.vs` 不是第二门必须完整重实现 Rust 的通用语言；
- 语言团队维护 feature matrix，每个 production 标注 `stable / preview / reserved`（目标（尚未建立）：DSL 规范目前只在诊断码表中标注 reserved）。

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

- 普通 `.vs` 不写语言头或 module 头：语言版本来自 `Viso.toml`/lockfile，module identity 来自 package root + source path；conformance fixture 需要显式 identity 时由 test harness 提供，不扩展普通 grammar（DSL 规范 §22「Compilation Unit」）。
- 类型显式度：public/exported API、input/event/slot/native/shader interface、外部持久化 schema 边界必须显式类型；private state/computed、local let、有期望类型的 closure 参数可推断。推断结果写入 schema；热重载时推断类型改变按 schema migration 处理。**1.0 核心没有 Dynamic 类型，无法推断的值不得回退成 dynamic**（DSL 规范 §74「类型推断边界」）。
- Native surface 由 Rust schema 提供（§40）；Capability 从实际 native call graph 推导并与 package/profile grant 比较，`requires { ... }` 只是 public API 的显式 assertion。
- Game support 是一等 vertical slice，只要求 compiler 消费 Native Schema 定义的 scheduler traits（如 `system PlayerController implements FixedUpdate { ... }`），不等待用户定义 Trait/Impl；小型 game facade lower 到同一 scheduler，不新增 parser 关键字（DSL 规范第十三部分「游戏表达能力与 Game Profile」）。

## 39. Viso 编译管线

```text
Source → Tokenizer（viso-lexer）→ Lossless CST → AST → Name Resolution / Module Graph → Typed HIR
      → UI IR / Binding IR / Reactive IR | Behavior IR → bytecode Module | Shader IR | System IR
```

- Lossless CST 必须存在（ADR 0010），服务 formatter 保留注释、精确 rename、incremental parse、AI 结构化修改、IDE code action、hot reload structural diff 与一次报告多个错误；禁止从 token 直接走向 runtime opcode。
- Typed HIR 与 type/effect/capability checking 见 ADR 0011/0013；开发期必须发现属性拼写、类型不匹配、不存在的 event、错误枚举、shader uniform 类型不匹配、invalid resource、read-only property mutation 与模块循环依赖。
- Behavior（handler、binding evaluator、computed、effect body、初始化、函数、闭包、task）降为 `viso-behavior` 的 bytecode `Module`（ADR 0035）：Dev 由解释器执行以便热替换，Release 再降为 Rust（§41）。Shader IR 的唯一 owner 是 `viso_shader::program::Program`，`viso-dsl` 只做 parse、check 与 lower（ADR 0042）。各 IR 由 `viso dump ast|hir|ui-ir|reactive-ir|behavior-ir|shader-ir|system-ir|module-graph` 输出（`Viso_CLI.md` §20）。

## 40. Rust Schema Bridge

Viso DSL compiler 只读纯数据的 schema，不直接操作 Rust 对象内存布局，也不链接 widget 实现（ADR 0042）：

- **Widget**：`viso_schema::widget::NativeWidget`（properties、property groups、events、slots，以及它 lower 到的 retained node 种类 `WidgetNode`）是 `'static` 数据；标准表 `viso_schema::widgets` 即 `viso::widgets`。
- **Native 函数与类型**：`viso_behavior::native` 的 `NativeLibrary` / `NativeFunction` / `NativeType`（effect kind、thread domain、capability、ownership）与 `Natives` 注册表（ADR 0036）。框架外的 native package 在 manifest 旁携带同一 schema 的 JSON（`natives.json`），compiler 经 `cargo metadata` 发现，不构建 app 的 Rust（ADR 0051）；app 自己的包也可以这样声明 native library（例如它的路由 enum），库 target 中称为 `crate::NATIVES`，其余 target 经库 crate 引用；native 数据类型由 `#[derive(NativeData)]` 生成。
- **Component**：`.vs` 组件的 input/state/event/slot 由 compiler 从源码得到（HIR 的 `ComponentSchema`）；`viso schema <symbol>` 输出上述任一对象（DSL 规范 §139）。

不做 `#[derive(Component, Reflect)]` 式从 Rust 类型反射 schema：Rust 能力以 native schema 数据进入 DSL。目标（尚未实现）：native package 携带 widget（与 trait）schema，使 app 或扩展的 Rust widget 可在 `.vs` 中使用（ADR 0051 目前只携带函数与类型）。

IR/runtime 使用 `NodeId`、`NodeKey`、`StateId`、`SymbolId` / `StableId`、`ResourceId`、`NativeId` 等数值 ID；属性在 lowering 时成为 AOT 节点的 style mask 与 payload 字段；开发模式保留字符串用于 diagnostics 与 Inspector；**release hot path 不做字符串属性查找**。

## 41. 开发运行与 Release AOT

- **Dev artifact**（`viso run`）可以保留 CST/HIR incremental cache、source map/debug name、schema reflection、hot-reload Symbol/source metadata、Dev Runtime、PatchBundle receiver/apply path、DevSnapshot endpoint 与 Inspector hooks。
- **Release artifact**：`.vs` / `ui!` / `component!` 在构建期经同一 frontend 生成 compact UI IR / Shader blobs，以 embedded asset 或 generated Rust data 形式打包（ADR 0016）。启动时不 parse、不 type-check、不需要完整 source symbol string table。
- **Release 执行模型**（ADR 0041）：模块的每个 chunk（handler、binding evaluator、computed、effect body、初始化、函数、闭包、task）由 `viso_behavior::aot::lower_module` 生成 Rust 源码，与 app 一起由 rustc 编译优化（内联、常量折叠、LTO）；task 降为可恢复的状态机。handler table 的条目即 chunk 索引，Dev 与 Release 同一布局，patch 只替换表项。`view!` 在宏展开时 lower，`viso::__behavior!` 在 release 或没有 `vm` feature 的构建中选用生成代码并嵌入 `Module::compiled()`（无 bytecode，只留常量池、读集与 build hash），debug + `vm` 时嵌入完整 bytecode。生成代码仍运行在 VM 的 runtime 外壳上（transaction、预算、fault 位置、memo、task）；镜像进 UI cell 的 state 拷贝由 `StateStore::epoch` 门控。`release_handler_native_vs_vm` 测两者差异。
- **语义一致性**：同一份 IR 在 Dev（解释器）与 Release（生成代码）下必须产生相同的结果。`crates/dsl/tests/differential.rs` 对 `differential/*.vs` fixture 双跑，逐步（调用、task 启动、每次恢复、`done` handler）比较值或 fault（种类、位置、消息）、states、revision、dirty 集合、事件、task 启动与 cost；不一致视为 compiler bug。
- Release **完全不编入** Dev transport listener、PatchBundle decoder/apply engine、DevSnapshot endpoint、hot-reload command handler、hot-reload-only 元数据与远程开发控制面。这是 build graph/feature boundary（facade 的 `hot-reload` feature），不是运行时开关；不得通过环境变量、配置或隐藏端口重新开启。Release 稳态帧不得有 per-frame dev socket poll、`hot_reload_enabled` 分支、SymbolId lookup 或 patch revision check。`cargo xtask check-release-absence` 扫描 release artifact 中不得出现的 dev marker 并确认它不连接 dev listener；`cargo xtask check-frame-tax` 把 dev 层的每帧开销限制在 10% + 250 ns 内。Debug symbols/crash source map 是另一项 build policy。

## 42. Development Runtime & Transactional Hot Reload

完整协议见 `Viso_Hot_Reload.md`（ADR 0015、0046）。plan、semantic diff 与 state migration 在 `crates/dsl/src/hotreload/`，产出 PatchBundle；commit 只在 `viso-view` 的 dev 模块（`crates/view/src/dev/`）；host 与 runtime 之间的帧、PatchBundle 与 inspect/profile 数据定义在 `viso-dev-protocol`，两端都不链接对方（ADR 0042）。Architecture 固定以下合同：

1. **Multi-lane**：`.vs` UI/behavior → Typed Semantic Patch；Game System → Fixed Tick-boundary System Patch；Shader → Validated Pipeline Patch；Asset/Font → Resource Revision Patch；Rust → Incremental Build + Stateful Warm Restart。
2. **Transaction**：compile → validate schema/type/capability → semantic diff → state-preservation plan → stage → 等待对应 domain boundary → atomic commit → targeted dirty propagation；失败则 NACK/rollback 并保留 last-good。validation 完成前不得半修改 retained state。
3. **Domain boundary**：UI/Reactive → Frame Boundary；Game System → Fixed Tick Boundary；Shader → GPU-safe Frame Boundary；Resource → Resource-ready + Frame Boundary；Rust → Warm Restart Boundary。
4. **State preservation**：stable SymbolId；compatible state slot 保留；incompatible private state 只 reset 最窄 scope；child reorder 用 StableKey；focus/scroll/selection/animation 各有 preservation contract；Rust Warm Restart 只恢复 typed DevSnapshot，不 dump raw memory。
5. **不注入任意 Rust 机器码**：不以 JIT、machine-code page replacement 或通用 dylib swap 作为跨平台方案；Rust 修改走 incremental build → DevSnapshot → relaunch → restore。iOS/Android 只要求 Simulator/Emulator。
6. **协议**：Host ↔ Dev Runtime 使用 `viso-dev-protocol` 的 Ende Binary 帧，CLI/Studio diagnostics 使用 Ende JSON；Patch 带 session/build/revision/schema identity，禁止乱序 blind apply。一个 `viso run` session 可以同时服务多个 target 的 build（ADR 0046），patch、ACK/NACK 与 last-good 按 runtime 分开。
7. **Last-good**：失败 candidate 不得把画面替换成 half-built tree 或 invalid pipeline。
8. **Remote Dev session（Embedded）**：Dev artifact 在板卡上运行，Host 通过 TCP 上的同一 Ende Binary 协议推送 PatchBundle，只是 transport 地址不同。Rust 修改走 cross build → 经 `ssh`/`scp` 部署 → Warm Restart（`Viso_Hot_Reload.md` §27A）。板卡上的 Dev artifact 不含 compiler，compile 只在 Host 上执行。

## 43. Viso 模块系统

编译器负责 module graph、topo sort、cycle detection、public/private、resource/shader namespace 与 incremental module rebuild；业务代码不靠手工注册顺序保证 UI 能启动（ADR 0011；DSL 规范 §22「Compilation Unit」、§23「Import」）。普通源码不写 `module` 声明（§38.3）。

## 44. Capability-based dynamic scripting

静态 capability 检查不能替代运行时授权：每个 VM 在 link 时得到 grant（默认是 module 所属 package 的 capability；`ViewHost::with_capabilities` 给出指定的 grant），Native Call 时验证，未授予时 handler 以 Runtime Fault `CapabilityDenied`（`E6103`）失败并回滚，不 panic，host 继续运行。AI 生成代码的预览默认只授予 `viso_behavior::native::PREVIEW_CAPABILITIES`（`ui.basic`、`gpu.draw.sandboxed`、`asset.read.package`），网络、文件、进程与剪贴板需显式授予（DSL 规范 §95）。VM 的 instruction / memory / depth / native-call 预算（`viso_behavior::Budget`）同样生效，但不替代 capability security。运行 AI 预览、第三方插件与用户脚本的 sandbox 宿主（Studio）尚未实现。

---

# Part XV — Widget 与 App Kit

## 45. 一个 `viso-widgets`，内部模块化

不预先拆成 `viso-widgets-core/app/adaptive` 等多个 crate，所有官方基础 widget 在单一 `viso-widgets` 内按模块组织。

**当前**（`crates/widgets/src/`）：`containers.rs`（`view`、`blur_view`）、`grid.rs`（ADR 0009）、`list.rs`（VirtualList，ADR 0008）、`lazy_grid.rs`、`scroll.rs`（ADR 0007）、`pager.rs`（Pager、Carousel）、`interactive_viewer.rs`、`swipe_row.rs`、`reorder_handle.rs`、`drag_drop.rs`、`text.rs`、`document/`（Markdown、HTML）、`image.rs`、`icon.rs`、`video.rs`、`camera_view.rs`、`avatar.rs`、`badge.rs`、`progress.rs`、`skeleton.rs`，以及 `controls/`（button、check、checkbox、radio、toggle、chip、slider、stepper、segmented、text_input、number_field、search_field、select、combo_box、color_picker、calendar、date_picker、time_picker、form、labelled、mnemonic、tabs、splitter、popup、modal、sheet、toast、tooltip、navigation_stack、caption_bar、`menu/`（含菜单栏）、`data_grid/`、`dock/`（ADR 0023）、`file_tree/`（ADR 0024））。交互机制（状态机、手势识别、浮层放置、语义投影、部件的写操作）大多在 `viso-ui` 的同名模块中，DSL 节点与 Rust widget 共用同一实现；`viso-widgets` 是其上的组合、样式与 builder。

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

**地图**（`extras/map`，`viso-map`）：矢量地图视图，选装，不进默认 widget 集。瓦片来自任意 `TileSource`（`request(TileId)` 返回任意 future，丢弃即取消，`cancel` 是提示）；读 PMTiles/MBTiles/XYZ、离线下载、搜索、路线与导航不在核心，在 viso-ext（`viso-tiles`、`viso-map-nav`、`viso-map-build`）。每张瓦片在共享 worker 池（`viso_ui::work`）上解码 MVT、按类型化 `Style`（内置 OpenMapTiles 与 Shortbread 两套 schema 的浅/深样式）一次性三角化成 retained drawing，并抽出标注候选；线宽是屏幕点，三角化时挤出在 vertex 里，层内缩放不重建。视图结构：根节点下一个 0×0 的相机节点，子节点依次是矢量瓦片的「图像层以下」部分、每个 raster/hillshade 图层一组图像瓦片（各自的金字塔与命名的 `RasterSource`，DEM 解码与 Horn 坡度着色在 worker 上，结果一次交给 asset 表）、「图像层以上」部分、世界坐标的形状（路线、多边形、米制圆）与定位精度圈；其上一层 overlay 放标注、定位点、marker（普通 Component）与聚合。平移、缩放（层内）、旋转只写相机节点的 translate/turn：节点按当前层级相对一个就近锚点放置，偏离过远或换层时才重放；拖动或旋转的一帧不上传图元、视图自身零分配（`tests/alloc.rs` 经隐藏的 `view::probe` 证明，新瓦片进入视野的那一帧除外）。金字塔：覆盖旋转后视野的瓦片集、加载中以祖先/子瓦片顶替、预取一圈、取消不再需要的请求、按字节 LRU 淘汰（矢量 192 MB、图像每层 64 MB）、同时 12 个请求。标注在相机静止时于 4 ms 预算内按图层与 rank 在 64 pt 网格上做碰撞，`name:xx` 按读者语言回退本地名，文字走 viso-text（各文种、CJK），线标注随路保持正向，淡入淡出 0.2 s。手势：拖动带惯性、双指缩放旋转（近正北吸附）、滚轮/触控板、双击与双指点按、键盘（方向键平移、+/− 缩放、Shift+左右旋转、N 朝北）；语义为 `Role::Image`，值读出经纬度与缩放。`MapHandle`：`jump_to`/`ease_to`/`fly_to`/`fit_bounds`、形状与 marker 增删、`features_at` 查询、`set_raster_source`；回调：点按与长按（经纬度）、视口静止、marker 点按。倾斜（`Camera::pitch`，0–60°）复用 scene3d 通道，不另写透视管线：`View` 的投影与反投影按同一透视（垂直视场 2·atan(1/3)，地面最远三倍视距），手势、查询、标注、marker 都跟着走；pitch 为 0 时完全是上面的 2D 路径。倾斜时每张绘制中的瓦片在 worker 上抬成一个 `ModelAsset`（`tilt::raise`：不透明底面、瓦片自身 drawing 抬高一点按原顺序混合、`Paint::Extrusion` 图层拉伸成受光体块；颜色与边缘覆盖来自一张小调色板纹理），交给 asset 表、由 overlay 之下的一个 scene 节点持有，瓦片离开时 `NodeStore::release_scene_model` 放手；`Style::terrain` 指向 DEM 栅格源时，底面变成 32×32 格网（带裙边），drawing 沿格网切开贴在各三角面上并按坡向着暗，楼体立在其下最低地面，相机看向焦点地面高度；形状在主线程抬起，按颜色逐级抬高，新模型驻留前继续画旧的。Alt 拖上下、双指同向上下滑、Shift+上下倾斜，N 复位。示例 `examples/map` 是自成 workspace 的离线导航（依赖 `../viso-ext`，主 workspace 不需要 viso-ext 就能构建），驾驶时倾斜 50°。

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

**窗口位置记忆**（`viso_ui::WindowConfig::persist`）：带名字的窗口（默认 `Some("main")`，`None` 关闭）在 facade 持久化 store（`viso::persist`，`app_data_dir` 下的 `DirStore`）中以 `window/<name>` 记下内容尺寸（逻辑点）、外框左上角位置与显示状态（normal / maximized / fullscreen；最小化不记），同名窗口下次打开（本次或下次启动）从中恢复：尺寸与位置写进 platform `WindowConfig`，最大化经 `WindowConfig::maximized` 在显示前设置，全屏在窗口创建后经 `set_fullscreen` 请求；已有打开的窗口持有该名字时，新窗口不记也不恢复；移动端与 Web 忽略。platform 以 `RawEvent::Moved`（`outer_position` 同一坐标）报告移动，`Window::mode()` 报告 `WindowMode`（`Normal`/`Maximized`/`Minimized`/`Fullscreen`），runtime 在移动、尺寸、缩放与全屏变化后调 `FrameDriver::on_window_placed`。帧在静止 400 ms 后才写入（经 `next_timer_deadline` 唤醒），最大化/全屏动画经过的中间帧与拖动流不逐个存，进入最大化或全屏前刚到达的未静止帧被丢弃，store 保留的是要回到的普通帧；关闭窗口、进入后台与 loop 结束时立即写入并 flush。给定位置的窗口由 platform 放回屏幕：外框与各屏工作区（macOS `visibleFrame`、Windows `rcWork`、X11 RandR monitor 或 root）的重叠不能覆盖整个外框时，移到重叠最多（其次最近）的屏上并在必要时缩小，旧会话的显示器拔掉后窗口不会开在屏外；Wayland 由 compositor 放置，只恢复尺寸与最大化/全屏。headless backend 把脚本化的 `Moved`/`Resized`/`FullscreenChanged` 应用到窗口，供 facade 测试读回。

**唤醒与阴影**：处理器可用 `EventCx::wake_after(node, delay, token)` 让 `token` 在 `delay` 后送到 `node` 的 key 处理器（`ev.woken()`），在那里做处理器能做的一切；它建立在一次性定时器上，帧在触发定时器后投递唤醒，不取消，过期的 token 由处理器忽略。节点可投下阴影（`BuildCx::shadow` / `NodeStore::set_shadow`，`Shadow { color, offset, blur, spread }`，`blur` 取 CSS 的模糊半径）：作为节点自身 drawable 画在盒子之下，用 renderer 的 analytic shadow（闭式高斯，无 offscreen）；冷表按 slot 排序，没有阴影的树只检查一个空表。

**模块分组**（informative；按子系统而不是按类型机械拆分）：

| 分组 | 现有 |
|---|---|
| 文字与媒体 | label、rich_text、link_label、document（markdown、html）、image、icon、video、camera_view、avatar、badge、progress_bar / progress_ring / spinner、skeleton / placeholder |
| controls | button、checkbox、toggle、radio_group、chip、slider / knob / range_slider、stepper、segmented_control、text_input、number_field、search_field、select、combo_box、color_picker、calendar、date_picker、time_picker、field / Form / fieldset、labelled |
| containers | view、blur_view、grid、scroll、virtual_list、lazy_grid、pager、carousel、interactive_viewer、splitter、swipe_row、reorder_handle、draggable / drop_zone |
| navigation | navigation_stack、tabs |
| overlays | popup / popover、modal、sheet、toast、tooltip、menu / context_menu |
| adaptive | DSL 的 `SafeArea`、`KeyboardAvoiding`、`AdaptiveScope`（`viso-ui` 的 layout 节点）；`LazyGrid` 的 `Columns::Adaptive` |
| desktop | caption_bar、menu_bar、dock、file_tree、data_grid |

目标（尚未实现）：`adaptive_split`（list-detail，窄屏时变为导航栈）与 `adaptive_navigation`（底栏 / rail / 侧栏随 size class 切换）。路由由 typed router（§6.1，`RouterView`）、`NavigationStack` 与 `Application::on_open_url` 承担；自绘标题栏窗口的缩放边框由 platform 提供，不另设 resize region。

---

## 46. Extra widgets 不污染基础 crate

大型或领域专用的 widget 是独立 crate，不是 `viso-widgets` 的依赖，facade 也不依赖它们：

| 能力 | 位置 |
|---|---|
| Charts | `extras/charts`（`viso-charts`） |
| Map | `extras/map`（`viso-map`）；瓦片读取、离线下载、搜索与导航不是 UI，在 viso-ext（`viso-tiles`、`viso-map-nav`、`viso-map-build`） |
| Code editor | `extras/code-editor`（`viso-code-editor`，语言服务经其 `lsp` feature） |
| PDF | 生成在核心（`viso::pdf`，facade 的 `pdf` feature，`viso-pdf`）；打印在 viso-ext（`viso-print`）；查看器尚无 |
| Browser / WebView | viso-ext 的 `viso-webview`（尚未实现） |
| Video editor、XR | 尚无 |

`extras/*` 在 monorepo 中，依赖受 `check-deps` allowlist 约束。Markdown/HTML 文档（`viso_widgets::document`，依赖 pulldown-cmark）留在基础 crate：富文本是常见 UI，DSL 不引用它，app 不用时不进二进制。

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

合同由 `crates/widgets/benches`（每个 widget 的 `build` / `layout` / `paint_tree`）与计数 allocator 断言稳态零分配的 `*_alloc.rs` 测试检查。

复杂 widget 的目录按“子系统”拆（例如 `dock/`、`file_tree/`、`data_grid/`、`menu/`、`document/`），简单 widget 保持单文件（`button.rs`）；Rust widget 与 DSL 共用的机制进 `viso-ui`（文本编辑即 `viso_ui::text_edit`）。

---

# Part XVI — Platform 与 Services

## 48. Platform 保持窄

当前目录（`crates/platform/src/`）：

```text
lib.rs  event.rs  handler.rs  control.rs  menu.rs  shell.rs  cursor.rs  time.rs  wake.rs
locale.rs  launch.rs  stdio.rs  placement.rs  panel.rs  drop.rs  browser.rs
accessibility.rs  accessibility/  access_mirror.rs   # SemanticsDelta 与 OS 无障碍桥（ADR 0030、0042）
clipboard/  gamepad/  audio/                          # 剪贴板、手柄（ADR 0052）、音频输出
backend/
├── mod.rs  drive.rs  headless.rs  main_queue.rs  memory_pressure.rs  links.rs  utf16.rs
├── macos.rs  macos_access.rs  macos_clipboard.rs  macos_panel.rs  macos_shell.rs
├── linux/                                            # x11/  wayland/  kms/（§48.1.1）
└── ios/  windows/  android/  web/
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
| 其它 target 与 headless | — | 测试 / 回退 | `CpuRaster` |

编译期静态选择规则见 [ADR 0029](docs/adr/0029-gpu-backend-static-selection.md)。

Tier-2 compatibility backend 可以存在，例如 Linux OpenGL、Android GLES（已有：`gles` feature 的 `Gles3Backend`，编译期选择）或 Web compatibility renderer，但遵循三条规则：

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
- **未做**：视频 overlay plane（视频画面经 renderer 合成，§50A.4）；触摸板 pinch / swipe（`RawEvent::Gesture` 已有，libinput 的 gesture 事件尚未翻译）。

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
- 系统字体发现、fallback 与字体解析（ADR 0026）；
- 非 UI 的 OS 能力（文件选择、通知、定位、相机采集、网络等）：由 viso-ext 插件提供（§49）。

---

## 49. Services 层

低频的 OS 能力通过 object-safe service protocol 调用，不在帧路径上。核心只保留与 UI 直接相关的协议（ADR 0031 修订）：

```rust
// 打开链接：handler 取会话注册表，在 task 中等待回复（§49.3）
let opened = cx.services().launcher().open_url("https://viso.dev/");
cx.spawn_then(opened, move |cx, opened| { /* … */ });

// 控件的触感：手指或笔造成的改变才播放，帧结束时交给 haptics 协议
cx.feedback(Haptic::Selection);
```

非 UI 能力不进核心。每个是一个 viso-ext 插件，自带各 OS 的实现、权限请求与 mock，共享 `viso-common` 的 `Reply`、权限与位置类型，app 只依赖用到的：文件与媒体选择 `viso-file-picker`、分享 `viso-share`、本地通知 `viso-local-notification`、推送 `viso-push`、安全存储 `viso-secure-storage`、定位 `viso-geolocator`、相机 `viso-camera`（画面经核心的 `CameraView` 显示，ADR 0050）、后台任务 `viso-background`、HTTP `viso-net`（也可作 `Application::image_loader`）、打印 `viso-print`、生物识别 `viso-auth` 等。核心不携带 Keychain、通知中心、portal file chooser 或 Credential Manager 的绑定。

### 49.1 为什么 service 可以用 dyn

这类调用不是 frame hot path。使用：

```rust
Rc<dyn Launcher>
```

在工程性上可能优于把所有 OS 特化写进泛型链。注册表是 `!Send`：service 在 UI 线程调用，OS 实现可以持有主线程对象；跨线程的 OS 回调经 `Send` 的 `Completer` 应答。

性能优化应该集中在真正热点，而不是追求全项目零 virtual call。

### 49.2 Mockability

所有 service protocol 必须可注入 mock/fake（核心为 `viso_services::Mock`，记录调用并可脚本化应答；viso-ext 插件各带自己的 mock），用于：

- headless tests；
- deterministic tests；
- Studio preview；
- AI agent；
- permission denied paths。

### 49.3 协议与实现（ADR 0031）

`viso-services` 定义两个 object-safe 协议：haptics、launcher，另提供持久化位置 `app_data_dir`（原生）与 `LocalStorage`（Web）。`Launcher::open_url` 把绝对 URL 交给系统打开它的应用（macOS `NSWorkspace`、iOS `UIApplication`、Windows `ShellExecuteW`、Linux OpenURI portal 回退 `xdg-open`、Android `ACTION_VIEW`、Web `window.open`），拒绝 `file:`、`javascript:`、`data:`、`vbscript:`、`blob:`、`about:` 等在本机执行或读取的 scheme（`Denied`）。文字中被激活的链接若沿路由无处理器调用 `EventCx::prevent_default`，store 记下其目标，facade 每帧取出并交给 `open_url`。异步调用返回一次性的 `Reply<T>`：它是 `Future`，也可以不经 executor 用 `try_take` 轮询。配对的 `Completer` 是 `Send`，可以在 OS 回调所在的任意线程完成；被丢弃而未应答时，结果为 `Cancelled`。

`Services` 是注册表，每个协议占一个固定字段。`Services::system(app)` 按编译目标选择 OS 实现（facade 在 platform 就绪后创建一次，所有窗口共享），`Services::unsupported()` 对所有调用返回 `Unsupported`（headless 会话的默认），`Mock` 用于 headless 测试并记录调用。

Android 通过 `viso-platform` 暴露的两个窄接口访问 activity：`with_activity` 和 `VisoActivity.addResults`。其余逻辑都在 services 内，包括随 APK 打包的 `dev.viso.services.VisoServices` Java 类（触感与 `ACTION_VIEW`）。框架外的扩展 crate（`viso-ext`）经 `ndk-context` 拿到 Java VM 与当前 activity：platform 在每个 activity 启动时设置它（activity 重建时先 release 再设为新的全局引用），扩展自带的 Java 以 `InMemoryDexClassLoader` 载入，经 `VisoActivity.addResults` 收到授权与 activity 结果。

handler 通过 `cx.services()`（`viso::services::ServicesExt`）取得会话注册表，并用 `cx.spawn_then` 等待 `Reply`（ADR 0032，§13.2）。

---

## 50. Mobile lifecycle

生命周期不是平台回调的逐一转发。各 backend 把 OS 回调去重后映射到少数几种改变框架行为的状态：

| 状态 | 事件 | 来源 | Runtime / facade 的处理 |
|---|---|---|---|
| 后台（不可见） | `Lifecycle::Suspended` / `Resumed`（`RawEvent::Suspended` / `Resumed`） | iOS scene/app `didEnterBackground` / `willEnterForeground`；Android `onStop` / `onStart`；Web `visibilitychange`；macOS 隐藏应用；KMS connector 断开或 VT 切走 | 挂起期间不跑帧、动画 beat 与 timer，task 不推进，只有 OS 事件唤醒 loop；进入时 facade 运行 suspend hook，把 view 的 `@persist` state 与窗口位置写入并阻塞到 durable；恢复时每个窗口重画一帧 |
| 内存紧张 | `Lifecycle::LowMemory` | iOS memory warning；Android `onTrimMemory(≥ RUNNING_LOW)` / `onLowMemory`；Apple 的 memory-pressure dispatch source | 每个窗口 trim：释放可重建的 GPU 资源与切走的 preserved branch，与预算超限同一路径（§50A.4） |
| 失去输入（Inactive） | `RawEvent::WindowFocused { window, focused }`（按窗口） | iOS `willResignActive` / `didBecomeActive`；Android `onWindowFocusChanged`；Web `blur` / `focus`；桌面窗口焦点 | 释放游戏持有的按键与 pointer lock，收起访问键提示，窗口不显示光标、选区变灰；仍可见，帧照常运行 |
| Surface 丢失 | `RawEvent::SurfaceDestroyed` / `SurfaceCreated`（按窗口） | Android 收回 native window；KMS mode 尺寸变化 | 事件返回前销毁该窗口的 swapchain，之后不画；恢复后重建并重画；app 不可见 |

`Lifecycle`（`viso_runtime`）只有 `Suspended`、`Resumed`、`LowMemory` 三个值，经 `FrameDriver::on_lifecycle` 交给 facade。不设单独的 Launching、Active/Inactive、Foreground、Terminating 事件：

- 启动即 `Application::new` 与 `build`；启动时与运行中收到的链接经 `Application::on_open_url` 交付。Deep link 属于核心：`Viso.toml` 的 `[package.links]` 声明 scheme 与 universal link / app link 的 host。
- Active/Inactive 是按窗口的输入焦点，不是应用级状态。
- 移动 OS 结束挂起中的进程不发通知，iOS `applicationWillTerminate` 与 Android 的进程结束都不可靠，因此 **Suspended 是最后一个保证送达的保存点**。facade 在 Suspended、窗口关闭与 loop 结束三处同样把持久化写入做成 durable；app 要保存的数据在 Suspended 时写出。

**Application 合同**：Rust app 经一个 hook 在进入后台时保存自己的数据、恢复时刷新、内存紧张时释放自己的缓存：

```rust
fn on_lifecycle(&mut self, cx: &mut EventCx<'_>, event: Lifecycle) {}
```

`cx` 与 `on_open_url` 的相同（launch window 根上的处理器上下文：可写 state、spawn task），`Lifecycle` 由 facade 再导出。顺序：`Suspended` 先调 hook 再运行 suspend hook 与 flush，使 hook 写入的 `@persist` state 进入同一次 durable 写；`Resumed` 先请求重画并恢复框架暂停的播放，再调 hook；`LowMemory` 先 trim 再调 hook。`.vs` 不另设生命周期事件，持久化由 `@persist` 承担。

**后台不播放**：Suspended 时 facade 暂停每个窗口里正在播放的视频，`NodeStore::suspend` 经 `FrameClient::suspend` 让游戏 session 暂停音频输出（`GameAudioDevice::pause`，`AudioOutput::pause` 停设备、保留 render）；Resumed 时 `NodeStore::resume` 与视频只恢复由框架暂停的那些：app 本来暂停的不动，挂起期间 app 要求暂停的保持暂停。游戏 session 的 `@persist` state 与 view 一样在 Suspended 时 durable：`FrameClient::suspend` 先经 `Scheduler::suspend` 写出并 flush，存储失败的报告进入 `take_persist_reports`；session 不必 drop。

UI 层经窗口的 `Environment` 提供 safe area、软键盘覆盖高度（`keyboard_inset`）、方向（`orientation()`）、size class（`SizeClassPolicy`；`AdaptiveScope` 按自身宽度为子树重设）、display features（折叠、铰链、刘海）、文字缩放与减少动态效果；DSL 以 `env` 读取，并有 `SafeArea`、`KeyboardAvoiding`、`AdaptiveScope` 容器。

非 UI 的移动能力在 viso-ext（§49）：权限随各插件申请（`viso-common` 的 permission 类型），推送 `viso-push`，分享 `viso-share`，相机 `viso-camera`，定位 `viso-geolocator`，后台任务 `viso-background`。

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
| Embedded-CPU | Cortex-A7/A35 单核到双核，无 GPU 或只有 GLES2（例如 i.MX 6ULL、全志 T113） | Linux（KMS dumb buffer） | CpuRaster + 可选 2D blitter（`CpuRaster::set_blitter`） | ≥ 32 MB | Tier-2 |
| MCU | Cortex-M7/M33、ESP32-S3、RP2350 | bare-metal / RTOS | line/tile buffer + DMA2D | 256 KB – 8 MB | 不在 1.0；core 保持 MCU-ready |

### 50A.3 Footprint Profile

Profile 是一组固定的编译期 feature 加上运行时预算默认值。用户只选择 profile，CI 只验证这几组组合（§5）；`cargo xtask check-profiles` 按 profile 检查 release 依赖闭包不含禁止的 crate 与 feature（CI 执行）。

| | desktop | mobile | web | embedded-gpu | embedded-cpu |
|---|---|---|---|---|---|
| GPU backend | 原生 Tier-1 | 原生 Tier-1 | WebGPU / CpuRaster | GLES3 / Vulkan | CpuRaster |
| 包内 shader 语言 | 后端语言 + `None` | 后端语言 + `None` | WGSL + `None` | GLSL ES / SPIR-V | 仅 `None` |
| 默认 worker 数 | N | N | 0–N | 1 | 0 |
| 系统字体 provider | 是 | 是 | 否 | 否（可开） | 否 |
| a11y OS bridge | 是 | 是 | ARIA mirror | 否（KMS 无 AT-SPI） | 否 |
| behavior VM（Release） | 否 | 否 | 否 | 否 | 否 |
| MTSDF / vector glyph | 是 | 是 | 是 | 否 | 否 |
| arena 容量 | 增长 | 增长 | 增长 | 固定 16384 | 固定 4096 |
| hot reload | Dev artifact | Dev artifact | Dev artifact | Remote Dev | Remote Dev |

窗口的 store 按 `MemoryBudget::nodes` 建（`NodeStore::with_fixed_capacity`，§50A.6）：embedded profile 默认固定容量，app 可在 `Application::memory_budget` 里改；`None` 为增长。满了以后再建的节点被拒绝：不进树，其 handle 永不 live，经它的写与 link 都是 no-op；facade 每帧报告一次 `ArenaFull`（容量、profile、被拒数）。

`Viso.toml` 的 device target（字段语法以 `Viso_CLI.md` 为准；另有 `gpu`、`locales`、`deploy`、`images`，`fonts` 还接受 `bake` 与 `rasterizer`）：

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

- face SLRU（`faces`）、shaping cache（`shaping`）、glyph atlas plane（`glyph_atlas`）与 image cache（`images`）从统一的 `MemoryBudget` 领取字节预算，超出时按既有合同从冷到热逐级释放（§37.4）。profile 只提供默认值，app 可以覆盖；
- GPU 资源（texture / buffer 字节）同样计入预算；
- heap 由 app 安装的计数 allocator 统计驻留与峰值（`#[global_allocator] static A: viso::alloc::Counting = viso::alloc::Counting::system();`）。超过 hard budget 时发出 `BudgetExceeded` 诊断事件并在 Inspector 中显示，不静默 OOM；
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

每个 profile 记录（`cargo xtask footprint`；尚未接入 CI）：stripped + LTO 二进制体积、静态内存（`.data` + `.bss`）、峰值与空闲 RSS / memory footprint、GPU 内存、heap 驻留与峰值、冷启动到首帧时间、空闲 CPU 与 wakeup 次数、headless 每帧 UI 线程时间。每帧 damage 像素由 frame profile 计数（§55.1）。

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

当前数字（`benches/footprint/baseline.md`：2026-10-18 记录，桌面窗口行其后重录；Apple M4 16 GiB，macOS 27.0.1，rustc 1.99.1，Makepad `70c90b7c0bc0`），桌面窗口：

| | Viso counter | Makepad counter | Viso list | Makepad list |
|---|---:|---:|---:|---:|
| 二进制（stripped） | 5.00 MiB | 12.24 MiB（原样示例 11.67） | 5.00 MiB | 12.29 MiB |
| 首帧提交 | 108 ms | 2179 ms | 98 ms | 2232 ms |
| 峰值 / 空闲 RSS | 81 / 77 MiB | 346 / 282 MiB | 81 / 71 MiB | 348 / 285 MiB |
| 峰值 / 空闲 footprint | 76 / 26 MiB | 319 / 166 MiB | 81 / 31 MiB | 326 / 172 MiB |
| GPU (OS) | 5.3 MiB | 18.9 MiB | 10.2 MiB | 21.4 MiB |
| heap 驻留 / 峰值 | 1.5 / 1.5 MiB | 103 / 138 MiB | 1.6 / 1.6 MiB | 104 / 138 MiB |
| 空闲 CPU / wakeup | 0 ms/s / 0.3 次/s | 3.3 ms/s / 0.3 次/s | 0 / 0 | 0 / 0.3 |

embedded-cpu（桌面窗口，CpuRaster）：二进制 4.58 MiB，空闲 footprint 24 / 40 MiB（counter / list），heap 驻留 3.7 / 8.7 MiB、峰值 5.1 / 12.5 MiB（含 CPU 帧缓冲）。headless（CpuRaster，420×600@1）：点击帧 p50 18–25 µs，滚动帧 p50 149–152 µs、p95 174–178 µs（desktop 与 embedded-cpu 两种 feature 集相同量级）；heap 3.0 / 4.0 MiB（desktop feature 集，counter / list），其中 GPU (budget) 1.07 MiB（1024² 的 glyph coverage atlas）；embedded-cpu feature 集 heap 1.5 / 2.5 MiB，GPU (budget) 326–337 KiB（mask page 按需分页，三个 9×9 图标只占一页 128²）。全部 subject 见 `benches/footprint/baseline.md`。

尚未测到的：
- 桌面窗口的逐帧 CPU 时间（两侧都没有向真实窗口注入输入的手段；Viso 的逐帧数字来自 headless）；Makepad 的逐帧时间（其 headless 是 `--cfg headless` 加运行时 JIT 编译 shader 的另一条路径，输入只能经 Studio 远程协议注入，与 Viso 不等价）。
- Viso 桌面的首帧提交时 text 尚未 shape（shaping 在 worker 上，文字在下一帧出现），故桌面 Viso 的「首个含文字帧」约晚一帧；Makepad 首帧前的约 2 s 是主线程在其自身代码中的 CPU 时间（stripped 构建，未进一步归因）。
- RSS 含 AppKit / Metal 共享页，跨框架比较以 footprint 为准；嵌入式板卡上的 RSS、首帧与 wakeup 需板卡实测（§50A.5 预算仍是假设）。

### 50A.6 MCU-ready 不变量（1.0 必须守住）

1. `viso-math`、`viso-ende`、`viso-schema`、`viso-behavior` 是 `#![no_std]`（math 不需要 `alloc`，其余需要）。`viso-text` 的 shaping / segmentation 路径和 `viso-ui` 的 arena / reactive / layout 模块不直接使用 `std::thread`、`std::time`、`std::fs`、`std::sync::Mutex`：时间经注入的 `Clock`，并发与唤醒经 runtime trait。CI 用 `thumbv7em-none-eabihf` 做交叉编译 smoke check，逐个 crate 推进：`cargo xtask mcu-smoke` 以 `--no-default-features` 检查 `viso-math`、`viso-ende`、`viso-schema`、`viso-behavior`（另含 `interpreter` feature 的 `viso-behavior`）与 `viso-glyph-a8`。
2. hot data 只使用 `f32` 与整数，不使用 `f64`（Cortex-M 只有单精度 FPU）。
3. 不假设 `usize == u64`（§10.4.4 已有）。
4. arena、hot store、worklist 都支持固定容量模式（§16）：`NodeStore::with_fixed_capacity` 一次分配 arena、全部节点列与 dirty list，超出为 `ArenaFull` 而不是增长。
5. AOT package 支持 XIP：Ende borrowed decode 直接读 memory-mapped flash，不要求先拷贝进 RAM。
   - **布局**：AOT package 与 view package 以 `ProtocolTag` 加 4 字节 magic（`AOT1` / `VPK2`，view package 再跟 `FINGERPRINT_VERSION` 的 LE `u32`）开头，后接 Ende section table（`viso_ende::SectionWriter` / `Sections`：section 数，再每项 offset、长度、条目数三个 LE `u32`）。每个 section 从 blob 起点算按 8 字节（`SECTION_ALIGN`）对齐，间隙为 0，布局唯一；reader 先校验整张表，越界、重叠、乱序或非零 padding 都是 `Malformed`。能定宽的记录定宽：AOT edge 24 字节，view 的 state 40 字节，handler 与 env 各 12 字节；AOT node 是变长记录（kind、style mask、payload、child 数）。view package 内嵌的 AOT blob 本身也在对齐 offset 上，内部 section 因此同样对齐。
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

资源使用数值 handle：

- 资源身份：`viso_schema::ResourceId`（`assets/` 下规范化路径的 FNV-1a 哈希；`Asset::bytes` 每次构造得到独立的 id，`Asset::file` / `Asset::url` 以路径或 URL 为键）与每个新 payload 递增的 `ResourceRevision`；
- 视图引用：`viso_ui::Asset { id, kind, source }`；
- GPU：`TextureId`、`BufferId`、`PipelineId`、`SamplerId`、`BindGroupId`、`SurfaceId`（`viso-gpu-abi`）；3D：`MeshId`、`MaterialId`（`viso_render::scene3d`）；
- 字体：`FontFaceId`（`viso-text`）。

路径与 URL 字符串只在构建、加载与帧外读取时使用，帧路径只比较 id。

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
- SVG 按原文件嵌入，asset worker 在运行时解析并 lower 为 primitive 流（`viso_render::svg`，解析器为 `viso-svg`）；CLI 的 asset 检查只解析不 lower；`images.svg = "raster"` 时构建期转为 raster（见上）；
- glTF 2.0 模型（`.glb` / `.gltf`，`AssetFormat::Gltf`、`AssetKind::Model`）按原文件嵌入，不做构建期转换；asset worker 解码为 `AssetPayload::Model(Arc<ModelAsset>)`（图像的 mip 链也在 worker 上按用途生成）（从文件读取的 `.gltf` 在同目录读取它引用的 buffer/图像，嵌入的需自包含），每个窗口把几何上传进自己的 renderer 并 `NodeStore::attach_model`，随后表放掉解码出的几何与图像。scene filler 用 `NodeStore::scene_model(slot, &asset)` 取驻留的 `Arc<SceneModel>`：slot 的节点在显示 scene 期间持有该 asset，驻留时 slot 被 reset 以便 filler 重画；被替换、被 sweep、或表清空的模型经 `AssetTable::take_retired_models` 交回 facade 从 renderer 释放。DSL 的 `asset("…")` 不接受模型（视图不显示模型，E3713）；
- 字体子集与 baked atlas 见 §37.8；
- 转换结果进入 AOT package，可以 XIP 读取（§50A.6）。

`assets/fonts/` 是 canonical packaged-font 目录。Build scanner 自动识别 TTF/OTF/TTC/OTC，并只提取必要 metadata 生成 compact `FontManifest`；**runtime 不因此 eager parse/load 全部字体**。第一次真正使用某个 face 时才 lazy load 并进入 Font Face SLRU。Native App 没有 packaged/dynamic font 时直接按需使用系统字体与系统 CJK/Emoji fallback；WASM/Canvas 不拥有系统字体，但 packaged fonts 同样可由 `FontManifest` lazy fetch，外部动态字体通过 `FontProvider` 注入。持有 WOFF2 的调用方须先自行解压为 SFNT 再纳入 `assets/fonts/` 或经 `FontProvider` 注入。

### 51.2 Resource lifecycle

- 逻辑引用计数（不要求每次使用一个 `Rc`）：asset table 按 node 统计每个 resource 的使用者，brush 采样也算；
- generation/version：`ResourceRevision`；resource patch（热更新）换新 revision，新 payload 驻留前继续画旧的；
- lazy load：第一次有节点显示时才读取与解码；
- async decode：在 asset worker 上（无 worker 时在 idle 时间片内），上传在帧内；
- eviction policy：无人显示的图片在 `images` 预算内保留，按最近显示时间逐出，显示中的从不释放（§50A.4）；
- 放弃的帧：backend 的 `abandon_frame` 丢弃持有的 drawable、解开 retire 队列，持久资源保留，下一帧重新取 drawable；
- device loss recreate：backend 报告 `is_lost()` 后，facade 下一帧放下窗口的 GPU 状态，用 `try_create_device` 开新 device（打不开时退避重试），asset table 的 texture/model 全部忘掉，再从保留的编码字节（无 origin 的资源）或 origin 重新解码、上传；等待期间节点按原尺寸画空（Viso_Rendering.md §6.4）；
- hot reload。

---

# Part XVIII — 用户项目目录与文件规范

## 52. 默认项目保持简单

`viso new` 生成的应用（`app` 模板；另有 `game`、`web`、`library`）：

```text
my_app/
├── Cargo.toml
├── Viso.toml
├── i18n/en.toml
├── assets/fonts/          # 包内字体（Noto Sans 与 OFL）
├── src/
│   ├── main.rs
│   ├── app.rs             # Application：packaged fonts，view!("app.vs")
│   └── app.vs             # 导出的根组件
└── tests/ui/clicks.tape   # UI scenario
```

长大后按 feature 组织：

```text
my_app/
├── Cargo.toml
├── Viso.toml
├── assets/
└── src/
    ├── main.rs
    ├── app.rs
    ├── app.vs
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
let page = view!("features/home/view.vs"); // 路径相对调用文件

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

Route 默认 Rust（目标（尚未实现）：`routes!`）：

```rust
routes! {
    Home => "/",
    Settings => "/settings",
}
```

Shader 写在 `.vs` 中，跨模块复用经 `export shader` 与 import；不设独立 `.shader` 文件。

原则：每多一种正式文件语言，就意味着 parser、formatter、LSP、diagnostics、AI support、docs 的长期成本。

---

# Part XIX — 工具链与可观测性

## 54. CLI

`viso` 是 Viso 唯一面向普通开发者的命令行 facade。内部可以存在 compiler、packager、inspector、platform toolchain 等多个实现模块，但用户不需要记它们的二进制或 crate 名称。

完整命令、参数、配置、JSON 协议与验收标准由仓库根目录 **`Viso_CLI.md`** 定义；Architecture 只固定不可违反的工具链合同。

### 54.1 Command surface

完整命令分组（PROJECT / MOBILE DEV ENVIRONMENT / DEVELOP / LANGUAGE / TEST·DEBUG / DELIVERY / MAINTENANCE）以 `Viso_CLI.md` 为准。已实现：`new`、`doctor`、`config`、`android`、`ios`、`run`、`build`、`serve`、`fmt`、`check`、`schema`、`explain`、`dump`、`lsp`、`test`、`game`、`snapshot`、`inspect`、`profile`、`package`、`export`、`clean`、`completion`；`studio` 尚未实现。不可违反的规则：

- **host implicit / mobile explicit**：无参数 `viso run/build/package` 表示当前 desktop host；移动与 Web 用逻辑 target（`run ios|android|web-gpu|web-dom|web-hybrid`）。Public CLI 不提供 `run macos|windows|linux|host`、`--target`、`ios --simulator`、`android --emulator`；macOS/Windows/Linux 是 Platform backend resolution，不是用户选择的 development target。
- `run ios/android` 只面向 Simulator/Emulator，`--device` 只选择 Viso virtual-device profile。`viso android|ios use ...` 修改 developer-machine state，不修改 Git tracked `Viso.toml`。开发期不要求 signing/provisioning/physical device；它们只属于 `viso package ios`。
- `viso run` 可同时运行多个 target（`viso run ios web-gpu`，`--desktop` 加上本机），共用一个 dev session（ADR 0046）。`Viso.toml` 声明的 device target（`[target.<name>]`）也是 positional target；写了 `deploy` 的板卡经 SSH 运行（§42 第 8 条），是唯一的物理设备开发路径。

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
log
dev
inspect
summary
```

事件结构和 exit code 必须稳定，CI、Studio、IDE、LSP 和 AI agent 不得依赖解析人类文本。

### 54.4 CLI 不复制业务实现

CLI、IDE/LSP、CI 与将来的 Studio 共享同一组领域实现：compiler 与 schema 在 `viso-dsl`，LSP 在 `viso-lsp`，项目发现、配置与 build identity 在 `viso-project`，打包在 `viso-package`，dev channel 的消息在 `viso-dev-protocol`，Inspector 的采集在 `viso-view`。CLI command handler 只负责参数解析、project/config resolution、调用 domain service、人类/JSON 输出与 exit code；不得在 `tools/cli` 内重新实现 DSL compiler、GPU compiler、packager 或 Inspector runtime。build 编排、dev session、Simulator/Emulator 控制与 inspector 客户端目前在 `tools/cli` 的 `build/`、`dev/`、`device/`、`toolchain/`、`command/inspect/` 中；出现第二个使用者（Studio）之前须移入共享的 tooling crate。

### 54.5 CLI 非目标

`viso` 不替代 Cargo/crates.io、Git、通用 shell task runner、Docker/CI 平台、数据库 schema 工具或任意 package manager。

CLI 只聚焦 Viso-specific 的 source → check → build → run → inspect → test → package → export 生命周期。

---

## 55. Inspector

Inspector 由 runtime 侧的采集（`viso-view` 与 `viso-ui` 的 `inspect` feature，随 `hot-reload` 与 `headless` 开启，release 不编入）与 `viso inspect` 组成，经 dev channel（`viso-dev-protocol`）或 headless 运行的输出读取（`Viso_CLI.md` §24）：

| 内容 | 命令 |
|---|---|
| UI tree、semantics、声明节点的源码位置与所属 component | `viso inspect tree` / `query` |
| Layout box、解析后的 style 与 content、transform 链、dirty class、interaction 状态、event route、绑定的 state、paint primitive | `viso inspect node` |
| 帧计数、draw batch、GPU resource、cache 与 `budget` 组 | `viso inspect frame` |
| 输入事件经过的节点（focus/input path）、焦点移动、每次 dirty 及原因（§56） | `viso inspect events` |

GUI 形式的 Inspector（Studio，`viso studio`）尚未实现。

### 55.1 Frame profile

`viso profile` 与 `viso run --profile-frame`（HUD）每帧报告 UI 线程上各 span 的时间：

```text
Input       输入路由与 handler
Flush       state flush：reload、动画、timer、task、binding 与 effect 的 settle、asset
Text        文本提交、编辑、塑形与重排
Layout      dirty 子树的 measure 与 layout（含 adaptive settle）
Style       interaction style 解析
Paint       dirty 子树 paint 为 primitive
Semantics   语义树更新
Upload      batch 与上传改动到 GPU
Submit      编码与 present（含等待 drawable）
```

以及该帧的 GPU 时间（后端提供时；GLES 尚无）和计数：

```text
nodes / dirty nodes / measured / laid out
painted primitives / static & dynamic binding evals / dynamic fallbacks
primitives / draw calls / batches / instances / GPU upload bytes
damage px / damage rects / full-frame fallbacks
shaped runs / glyph rasters / glyph upload & cache bytes / atlas pages
resource loads
allocations / allocated bytes（安装 viso::alloc::Counting 时）
```

---

## 56. Debug reasons

Dev artifact 与 headless 构建中，任何直接标记的 dirty 都可查询原因：`viso-ui` 的 `inspect` journal 每次直接 mark 记一条（同节点同原因的连续 mark 合并，有界），原因是 `input`、`state`（state cell，以及 source table 给出的名字、类型、声明位置与改变后的值）、`computed`、`animation`、`task`、`reload`、`text`、`asset`、`window`、`layout` 或 `other`；向祖先冒泡的 class 由树决定，不记。`viso inspect events` 读取：

```text
#14    f4     dirty  Text#count 5:0  measure, layout, paint, semantics  ← state taps = 2  src/app.vs:2:5
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
- text segmentation（UAX #9/#14/#29 corpus）；
- resource lifetime。

### Integration

- input → state → layout → paint（`crates/viso/tests/*_widget.rs` 在 headless facade 上驱动）；
- window lifecycle；
- hot reload（`crates/dsl/tests/hot_reload.rs`、`game_reload.rs`）；
- platform service mocks（`viso_services::Mock`）；
- async cancellation；
- 稳态零分配（`*_alloc.rs`，计数 allocator，§58）；
- Release 与 Dev 语义一致（`crates/dsl/tests/differential.rs`，§41）。

### Snapshot / Golden

- layout tree snapshot；
- semantics snapshot；
- render image snapshot（`viso snapshot`：UI scenario 的截图与 golden 比较）；
- shader generated output；
- Viso DSL diagnostics（`crates/dsl/tests/golden`）。

### Fuzz

`fuzz/` 的 cargo-fuzz target（nightly CI，每个 600 s）：wire 与快照（`dev_wire`、`dev_snapshot`、`game_snapshot`、`input_tape`、`game_packet`）、package 与 module（`aot_package`、`view_package`、`behavior_module`）、资源解码器（`raster`、`svg`、`gltf`、`font`）与 text 的 Unicode 边界（`text`），清单见 §10.5.18。lexer、parser 与 formatter 由测试内的随机输入 smoke 覆盖（`crates/dsl/tests/{lexer,parser,format_fuzz}.rs`）。

### Headless

`headless` feature 的构建（`viso build headless`、`viso test`）提供：

- deterministic input（输入磁带 `.tape` 与 UI scenario）；
- fixed time（`FixedStepClock`）；
- screenshot（CpuRaster 固定 surface）；
- node dump 与 semantics dump（`viso inspect --target headless`，`VISO_HEADLESS_DETAIL`）；
- frame profile counters（`VISO_HEADLESS_PROFILE`）。

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

“0”指设计目标和 benchmark 场景，不意味着所有可能的用户自定义代码永远不分配。`*_alloc.rs` 测试以计数 allocator 断言稳态帧零分配，`reactive_binding` bench 启动时断言静态 binding 路径零 dynamic fallback。

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

基准随所测的 crate 放在 `crates/*/benches`（criterion，release 运行），footprint 在 `benches/footprint`（`cargo xtask footprint`，§50A.5.1）。文字与字体的矩阵见 `Viso_Text_Font_Runtime.md` §26，renderer 的 gate 见 `Viso_Rendering.md` §31。已有：

| 场景 | bench |
|---|---|
| 大列表 | `list_100k_scroll/rows_{30,160}`（`viso`）；`reconcile_*`（`viso-ui` 的 `large_list`） |
| 局部改动的帧成本随树增长 | `paint_scale`：`hover` / `fade` / `type` / `mount`（`viso`） |
| 帧循环 | `frame_loop/view`（`viso`，`check-frame-tax` 用它）、`blank_frame_loop`（`viso-runtime`）、`window` / `window/fan_out`（多窗口） |
| reactive | `flush_static` / `flush_dynamic` / `flush_mixed`（`reactive_binding`，启动时断言静态路径零 dynamic fallback） |
| node arena 与 identity | `node_arena_{create,remove,traverse}_100k`（含 `_fixed`、`_checked`）、`identity_node_generation_check_1m` |
| layout、style、动画、hover | `grid_relayout_*`、`length_*_refold*`、`animation_tick`、`hover_move_*`、`timer/arm` |
| 语义 | `derive_full_single_label_change`、`derive_with_state`、`project_wake` |
| text | `text_runtime_{steady,edit}_frame`（`viso`）、`text_steady_frame_layout_cache_hit`、`text_edit_step_zero_main_shaping`、`text_incremental_single_token_edit_8000_words` |
| renderer | `renderer_steady_state`（上传稳态、blur、blend、glass、gradient、SVG path 等）；`cpu_raster`（800×480 全帧与 5% damage，XRGB8888 / sRGB / RGB565，1 与 4 线程） |
| widget | `crates/widgets/benches`：每个 widget 的 `build` / `layout` / `paint_tree` |
| math | `math_vec2_ops_10m`、`math_mat4_mul_1m`、`math_affine2_transform_points_10m`、`math_rect_hit_test_10m`、`math_aabb_intersection_1m`、`math_transform_chain_100k`、`math_point_bounds_4k_x10k`、`math_segment_lengths_4k_x10k` |
| behavior 与游戏 | `release_handler_native_vs_vm/{vm,native}/{action,fib,loop}`、`behavior/*`、`game_native`、`game_world`、`incremental_reparse`、`cpu_shader/*` |
| footprint | 各 profile 的体积、首帧、RSS / footprint、heap、空闲 CPU 与 wakeup，对照 Makepad |

目标（尚未实现），仍须补上的场景：

```text
layout_1k_static
layout_10k_dirty_1pct
layout_10k_dirty_100pct
list_variable_height_scroll
text_10k_labels_static
text_dynamic_1k
animation_transform_5k
animation_layout_1k
hit_test_100k
state_update_1_binding
state_update_1k_bindings
reactive_hot_reload_rebind
hot_reload_small_component
hot_reload_large_module
startup_minimal
startup_complex_app
memory_10k_nodes

damage_single_label_update
damage_scroll_list_partial_present
cpu_raster_glyphs_10k
gles3_batch_10k_quads

identity_symbol_link_100k
identity_dense_property_lookup_1m
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

具体阈值按 benchmark 稳定性调整。当前进入 CI 的只有 `cargo xtask check-frame-tax`（dev 层帧开销 ≤ 10% + 250 ns）；`cargo xtask footprint --check`（容差：体积 3–5%、RSS 20%、footprint 50%、时间 50%，各带绝对下限）与 criterion 基准在本地运行，尚未接入 CI。

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

CI 记录典型 tree 的 memory-per-node 趋势（目标（尚未实现）：runtime 尚无按 hot / warm / cold 统计的每节点字节数，`memory_10k_nodes` 也未建）。

per-node 不设绝对上限，但 Embedded profile 有整机级预算（§50A.5），各 cache 预算由 `MemoryBudget` 统一分配（§50A.4）。NodeLinks 已是 index 链接（五个 `NodeIndex`，20 字节，§14.1）；`hot bytes/node` 的目标是不超过一条 64-byte cache line（尚无测量）。

---

# Part XXII — Unsafe 设计

## 61. Unsafe 边界集中化

允许 `unsafe` 的位置：

- GPU FFI（`viso-gpu` 各 backend：`ash`、`windows`、`objc2`，GLES/EGL 运行时加载）；
- OS FFI（`viso-platform`、`viso-media`、`viso-services`，facade 的 CoreText 等绑定）；
- SIMD（`viso-math::simd`，CpuRaster 的 NEON / SSE2 / SIMD128 kernel）；
- packed buffer / byte view（`viso-render` 的 frame arena、instance pool、scene key 的字节视图）；
- 实时线程的 lock-free 队列（`viso-behavior` game audio 的 ring）；
- mapped buffer；
- generated GPU ABI bridge（`#[derive(GpuPod)]`、`viso-gpu-abi`）。

普通 `widgets` / app API 不需要 unsafe：`viso-ui`、`viso-widgets`、`viso-runtime`、`viso-schema`、`viso-dsl`、`viso-view` 的源码没有 unsafe（测试中的计数 allocator 除外）。这是现状而非禁令：任何 crate 在有性能或 FFI 理由时都可以引入 unsafe，不以 `forbid(unsafe_code)` 封死；约束只有 §61.1 的 `SAFETY:` 注释，由 clippy `undocumented_unsafe_blocks`（workspace lint）在每个目标上检查。

### 61.1 Safety comment

每个 unsafe block 必须写：

```text
SAFETY:
- invariant 1
- invariant 2
- caller guarantee
- why aliasing/lifetime is valid
```

目标（尚未实现）：由 clippy `undocumented_unsafe_blocks` 在 CI 中强制；当前靠 review，少数 block（主要在 macOS backend）缺注释。

### 61.2 Fast/checked 双路径

节点引用分两种，两种都是 safe 代码：

- `NodeId(index, generation)`：外部 handle（Inspector、DSL runtime、app 持有的 handle），每次访问校验 generation，stale 时返回 `None`；
- `NodeIndex(u32)`：内部链接（tree link、binding target、dirty worklist、paint owner），只是 slot 下标，不做 generation 校验（`NodeArena::raw` / `parent_of` / `children_of`）。

`NodeIndex` 路径只在清晰的 phase invariant 下使用：arena 在 free 之前先 unlink，所以内部链接永远不指向已释放的 slot。下标访问仍有 bounds check；只有 benchmark 证明 bounds check 是瓶颈时才允许改成 unsafe unchecked 访问，并在 Debug build 中 assert。

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

这是分类约定，不是一个公共 enum。各级在代码中的落点：

- UserError：`viso-project` 的 `ConfigDiagnostic`（`C####`）、CLI usage error（exit 2）；
- CompileError：`viso_dsl::diag::Diagnostic`（`E1xxx`–`E9xxx`），shader IR 的 `viso_shader::diag::Diagnostic`；
- RuntimeError：behavior fault `E7101`–`E7106`（只影响出错的 handler）、跨帧 effect 环 `E4202`、`BudgetExceeded`、service 的 `ServiceError`；
- DeviceError：`GpuBackend::abandon_frame` 放弃未 present 的帧；`GpuBackend::is_lost` 报告 device 丢失（D3D12 removed/reset、Vulkan `VK_ERROR_DEVICE_LOST`、WebGPU `lost` promise、EGL context lost、Metal device removed），facade 在新 device 上重建窗口；
- InvariantError：Debug panic，Release 尽量输出上下文并安全终止/降级，不 silent corruption。

### 62.1 Diagnostics 统一结构

```rust
Diagnostic {
    severity,      // Note < Warning < Error
    code,          // &'static str，稳定 code
    primary,       // 主 span
    related,       // 带 label 的次 span，可跨 module
    expected,
    actual,
    notes,
    fixes,         // 带 Applicability 的源码编辑
    message,
}
```

- `viso_dsl::diag::Diagnostic` 是 lexer、parser、resolver、Typed HIR、UI/Binding IR、hot reload 迁移检查共用的类型；LSP、CLI 渲染器读同一个类型。
- `viso_dsl::diag::codes` 是唯一的 code 注册表：编译期与运行时（`Stage::Runtime`）的 code 都在这里，每条带 stage、标题、解释、示例与修复；`viso explain`、LSP hover 与生成的 code 表都读它。
- `viso-shader` 位于 `viso-dsl` 之下，不能依赖它，所以有一个平行的 `Diagnostic`（severity 排序相同，多一个 `CompileStage`）；`.vs` 中 shader 块的错误由 DSL 前端以 `E81xx` 报出。
- 配置诊断用 `C####`，与源码诊断的 code 范围不重叠；CLI 的输出层把源码、配置与 cargo 诊断渲染成同一种 JSON 诊断对象。

---

# Part XXIV — Makepad 参考实现与设计经验

> §63 首段（不建立兼容层、不复制耦合边界）是规范；§63.1–§63.8 为 informative 背景。

## 63. 参考边界

Makepad 只作为 Viso 设计过程中的外部参考资料。Viso 不建立 Makepad API/ABI 兼容层，不提供项目转换命令，不在 runtime 中识别 Makepad 类型，也不把 Makepad 的模块注册、Widget 生命周期或脚本执行模型作为 Viso public contract。

参考原则：

> **参考经过验证的算法、性能特征、工具链经验和用户体验，不复制耦合边界。**

所有被采用的思想都必须先经过 Viso 自己的类型、数据布局、crate DAG、性能合同和测试体系重新定义。

### 63.1 Identity / Symbol

Makepad `LiveId` 展示了“把源码名字尽早变成廉价数字身份、避免热路径字符串比较”的价值。Viso 参考这一性能目标，但采用分层 Identity：

```text
NameId                    编译器内 interned 名字（viso-dsl）
  ↓
SymbolId (128-bit)        跨编译稳定身份（viso-dsl；artifact 中为 viso_schema::StableId）
  ↓ link / lower
闭合 enum 判别值与 dense slot
  ControlKind / ControlInput / EventRoute（u8 tag）、StateId、ProgramSlot
  ↓ instantiate
NodeId(index, generation) / NodeIndex
```

用户组件在挂载时内联进 view（ADR 0039），运行时没有组件类型表，所以不需要 `ComponentTypeId`；属性与事件的集合由 schema 闭合定义，用 `#[repr(u8)]` enum 表示，不另设 `PropertyId` / `EventId` newtype。Viso 不使用一个万能 ID 贯穿所有 subsystem。

### 63.2 Ende

Makepad 的轻量序列化实现证明了低依赖、derive 生成、直接 Encode/Decode 对工具协议和高频消息很有价值。Viso 将这类能力定义为 `viso-ende`（`no_std`，无第三方依赖）：

- Viso-owned Binary wire contract；
- Ende JSON 作为 diagnostics/tool interchange（emitter + bounded reader）；
- bounded decoder；
- execute-in-place：对齐分区与借用式 decode（AOT package 可直接从映射的 flash 读）；
- 集合 count 的统一上限与剩余字节检查（`read_count` / `read_list`）；
- 不支持 RON；
- Serde 只作为生态 integration。

目标（尚未实现）：derive 生成 Encode/Decode；可复用 buffer 的零分配 encode（当前 `Encoder` 只有 `new` / `with_capacity` / `into_bytes`，没有 `clear`）。

### 63.3 Math / Geometry

Makepad 中已经验证过的向量、矩阵、矩形、transform、geometry 热路径实现可作为性能与数值行为参考。Viso 使用独立 `viso-math` 类型、ABI 和模块边界；Math public ABI 不暴露 backend SIMD 类型，也不直接等于 GPU uniform/instance layout。

### 63.4 Layout

顺序布局中的单遍 cursor/turtle 类算法可以作为内部实现候选，尤其适用于 Row/Column/flow 场景。Viso public layout contract 仍以 measure / layout 两遍协议（§22.1）、typed units 与独立 invalidation plane 为准，算法是否采用单遍 cursor 由 benchmark 决定。

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

Viso 在这条路径上的差异：GLES3（GBM/EGL）与 CpuRaster（dumb buffer）两种后端（KMS 上不提供 Vulkan）、epoll 事件驱动且空闲零唤醒、damage-driven present、Footprint Profile 与 memory budget 合同、MCU-ready core（§50A）。

---

# Part XXV — Viso 1.0 实施路线图

> **Informative。** 本 Part 是路线图，不是规范性合同；与 Part 0–XXIV 或已接受 ADR 冲突时以后者为准。进度以 `docs/adr/` 与仓库根目录的 `todo_*.md` 为准；每个 Phase 下的「状态」写明已落地的 ADR 与剩余缺口。Phase 0 由 ADR 0001 关闭；Phase 1–6 已完成；Phase 7–8 除各自列出的缺口外已完成；Phase 9 的 CLI 覆盖 `Viso_CLI.md` 主路径，Studio 未开始，`web-dom` / `web-hybrid` 未实现；Phase 10（§73A）除板卡实测与 KMS overlay plane 外已完成。游戏（ADR 0045/0047/0052）、媒体（0049/0050）、输出色彩编码（0048）、Native Package（0051）与 mesh extrusion / retained drawing / worker 池（0053）不属于任何 Phase，见对应 ADR。

## 64. Phase 0 — 固定 Architecture Contract

**状态**：已关闭（ADR 0001）。module 级依赖检查与 unsafe CI gate 未建立（§84）。

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
15. 建立 crate 依赖 allowlist 检查（`cargo xtask check-deps`，§10.2）；
16. 建立 dependency/unsafe/perf CI gates。

### 退出标准

- 最小 Viso workspace 可独立编译；
- CI 自动验证 crate/module 依赖方向；
- core benchmark baseline 已记录；
- `Viso.toml`、CLI target model 与 artifact 目录约定已固定。

---

## 65. Phase 1 — Foundation / Runtime / Platform

**状态**：已完成。ADR 0004（首帧）、0019（timer / `WaitUntil`）、0020（多窗口）、0032（跨线程 wake）。backend：macOS、Windows、Linux（Wayland、X11、KMS）、iOS、Android、Web 与 headless（`crates/platform/src/backend/`）；`Suspended` / `Resumed` / `LowMemory` 由 scheduler 与 facade 处理（§77）。

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

**状态**：已完成。ADR 0002（opacity layer）、0029（静态 backend 选择）、0048（输出色彩编码）。backend：Metal、D3D12、Vulkan、WebGPU、GLES3、CpuRaster；`#[derive(GpuPod)]` 在 `viso-macros`；per-family 持久 instance pool；`viso_render::graph`；golden 测试在 `crates/render/tests` 与 `crates/viso/tests/golden`。

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

**状态**：已完成。ADR 0003；index 链接、generation 奇偶、fixed capacity 与 100k create/traverse/remove bench 见 `todo_arch.md` A3。

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

**状态**：已完成。ADR 0007（Scroll）、0008（VirtualList）、0009（Grid）、0022（hover）、0030（AccessKit bridge）、0033（相对长度单位）；IME 与文本编辑见 `todo_features.md` W2。

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

**状态**：已完成。ADR 0005、0021；模板级 binding 表见 `todo_arch.md` A7；static / mixed / dynamic 基线在 `crates/ui/benches/reactive_binding.rs`。

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

**状态**：已完成。ADR 0010–0018、0034–0041、0046；LSP 提供 diagnostics、formatting、hover、definition、references、rename、signatureHelp、codeAction。

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

**状态**：Tier 1–5 已完成（ADR 0023 Dock、0024 FileTree；code-editor 在 `extras/code-editor`）。Tier 6 已有：Markdown / HTML 文档（`viso_widgets::document`）、Charts（`extras/charts`）、Map（`extras/map`）、Video（`viso-media`，ADR 0049/0050）。未实现：PDF 查看器（`viso-pdf` 只把 view 写成 PDF）、Browser（`viso-ext/viso-webview` 只是空 crate）、UI zoo 应用。Tier 1–4 控件有 build microbench（`crates/widgets/benches`）与 golden；之后加入的控件（DataGrid、Calendar、ColorPicker 等）只有测试与 golden，缺 microbench。

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

**状态**：核心部分已完成：clipboard（`viso-platform`）、haptics / launcher / app data（`viso-services`，ADR 0031）、async task（ADR 0032，`cx.spawn_then`）、NavigationStack、safe area / keyboard inset / adaptive（facade `environment`）、lifecycle（§77）。非 UI 服务按设计不进核心，是 `viso-ext` 插件 crate：file picker、share、本地通知与 push、secure storage、定位、相机 provider（ADR 0050）、打印，权限随各插件；HTTP/WebSocket 的 `viso-ext/viso-net` 进行中（`todo_features.md` W11）。Rust 侧 typed route（`Navigator<R>`、`RouterView`、`routes!`，§6.1）与 `Application::on_lifecycle` 已实现；DSL 侧的 `Navigator<R>` 与 `RouterView`（`Viso_DSL_1.0.md` U11）亦已实现。

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

**状态**：CLI 已覆盖 new、doctor、android、ios、run（多 target lane，ADR 0046）、build、serve、package、export html|solid、fmt、check、schema、explain、dump、test、snapshot、inspect、profile、game、config、lsp、clean、completion；`--json` 为 Ende JSON event stream。`viso inspect` / `viso profile` 与 UI Scenario 共用 `viso_view::inspect` / `viso_view::profile`。未实现：Studio 与 `viso studio`；`web-dom` / `web-hybrid`（报 `C0011`，等待 DOM runtime backend）及按 target 的 capability 表（当前只有 export lowering 的 capability 诊断）。

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

**状态**：工作 1–12 已完成（`todo_arch.md` A1–A18；ADR 0042–0044 的文件状态仍为 Proposed）。缺口：KMS overlay plane（视频）；退出标准中的板卡实测与板卡 bit-exact 对比尚无数据（`benches/footprint/baseline.md` 只有桌面与 headless）；release 产物中不含 VM 尚无自动检查。

### 工作

1. DAG 修正：删除 `gpu -> runtime`、`text -> gpu`；新增 `viso-schema`，把 `dsl -> ui` 改为 `dsl -> schema`；`check-deps` 增加传递闭包检查；
2. Release native lowering：`lower_module` 把每个 chunk（含 task 状态机）生成 Rust；`view!` 与 view package 都走 native 路径，解释器成为 `vm` feature（只在 debug build 执行）；镜像 sync 按 epoch 门控；补 differential test（`crates/dsl/tests/differential.rs`）；
3. Frame phases 改为 17 阶段并同步 `FramePhase::ORDER`；dirty worklist；
4. Transform root + place 表，draw 级 place（`Affine2`，经每个 built-in 读取的 view uniform）；TRANSFORM 不再连带 PAINT / HIT_TEST；
5. NodeLinks 改为 index 链接；binding 表改为模板级；
6. Damage-driven present（GPU 与 CPU；Metal / WebGPU 不知道 drawable age，变化帧整帧重画）；
7. `CpuRaster` 生产化（tile、RGB565、SIMD、damage tile）；
8. `Gles3Backend` 与 GLSL ES 3.00 codegen；
9. `backend/linux/kms`：KMS atomic、GBM/EGL、dumb buffer、evdev、epoll（overlay plane 未做）；
10. Footprint Profile、`MemoryBudget`、`check-profiles`、footprint benchmark、与 Makepad 的对比基线；
11. Embedded 字体：subset、baked atlas；资源构建期转换；
12. MCU-ready 不变量的 `no_std` CI（`cargo xtask mcu-smoke`）。

### 退出标准

- `01-counter` 在一块 Embedded-GPU 板卡与一块 Embedded-CPU 板卡上运行，满足 §50A.5 的预算（或给出校准后的新预算）；
- 桌面仿真与板卡输出 bit-exact（CpuRaster）；
- LSP / `viso check` 的依赖闭包不含 runtime / platform / GPU backend；
- Release 二进制中不含 behavior VM；`release_handler_native_vs_vm`（`crates/dsl/benches/behavior_native.rs`）有基线数据。

---

# Part XXVI — Repository 详细编排

## 74. Framework repo 最终建议

当前仓库结构与 crate 清单见 §8/§9，依赖方向见 §10；本节不再重复一份目录树。顶层目录：

- `crates/`：框架 crate；
- `extras/`：选装的高阶组件：`charts`、`code-editor`、`map`（Markdown/HTML 文档在 `viso-widgets`，PDF 输出在 `crates/pdf`）；
- `tools/`：`cli`、`project`、`package`（打包）；Inspector 是 `viso inspect` 与 `viso_view::inspect`，不是单独的 tool crate；Studio 尚未建立；
- `libs/`：框架自有、可独立复用的小型算法 crate（§76）；
- `examples/`：教程按编号（`01-counter`、`10-custom-shader`），功能示例按名字（`hello_world`、`i18n`、`images`、`video`、`code-editor`、`map` 等）；
- `benches/footprint`、`tests/characterization`、`fuzz/`、`xtask/`。

非 UI 能力与生态 adapter（文件对话框、分享、通知、网络、tokio/serde 等集成）不在本仓库，在同级的 `viso-ext` 仓库作为插件 crate（如 `viso-net` 的 `serde` feature）；本仓库没有 `integrations/` 目录。第三方 fork 放 `vendor/`（§76），当前没有。

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

当前例外（待拆分）：facade `crates/viso/src/lib.rs` 约 5600 行，`AppDriver` 与窗口状态的实现都在其中。

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

全部重新塞进一个模糊 `libs/`。`libs/` 只放框架自有、可独立复用的小型算法 crate，不放第三方 fork：当前只有 `viso-woff2`（由调用方自行解压 WOFF2，ADR 0028）。Base64 是 `viso_ende::base64`（data URL、glTF 内嵌 buffer、CLI 的 WebSocket 握手共用一份）；`viso-package` 按合同不依赖任何内部 crate，留着自己的一份。

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
    fn on_lifecycle(&mut self, cx: &mut viso_ui::EventCx<'_>, event: Lifecycle) {}
    fn on_tray(&mut self, event: viso::shell::TrayEvent) {}
    fn memory_budget() -> MemoryBudget { MemoryBudget::of_build() }
    fn image_loader() -> Option<ImageLoader> { None }
}
```

`run` 拥有 platform event pump 与 frame scheduler，无原生 backend 时回退 headless（`headless` feature 下直接走 headless），直到最后一个窗口关闭才返回（ADR 0006/0020）；web 上 scheduler 在 `run` 返回后继续运行。`memory_budget` 与 `image_loader` 在第一个窗口打开前读一次。

Lifecycle 由框架处理，再告诉 `Application::on_lifecycle`：`Suspended` 时 scheduler 停止出帧，facade 先调 hook，再刷写持久化 state；`Resumed` 时每个窗口重画一次，再调 hook；`LowMemory` 时释放可重建的 GPU 内存与被切走的保留分支，再调 hook（§50）。

事件不由 `Application` 手工接收；UI runtime 自己 route。例外是打开应用的链接（`[package.links]`）：platform 以 `RawEvent::OpenUrl { url }` 交出（在 `AppLaunched` 之后，冷启动与运行中一致；macOS `application:openURLs:`，iOS scene `URLContexts`/`userActivities` 与 `openURLContexts`/`continueUserActivity`，Android 启动 intent 与 `onNewIntent`，Windows/Linux 参数加单实例转交，web 的 fragment 与 `hashchange`），runtime 经 `FrameDriver::on_open_url` 交给门面，门面排队到启动窗口建好，再以该窗口根上的 `EventCx`（`KeyRouter::run_at`，请求按处理器的方式生效）调用 `Application::on_open_url`。

---

## 78. Typed Handle

Viso 1.0 明确：handle 是 **typed identity/capability**，不是组件对象引用。

禁止：

```rust
let state: &T = handle.borrow(cx);         // no long-lived direct borrow
let state: &mut T = handle.borrow_mut(cx); // no mutation bypassing invalidation
```

允许的 public direction：

- 取 node identity（`Handle::id()`）与存活检查；
- typed action/event dispatch；
- schema 生成的 property getter/setter；
- context-scoped read query，返回短生命周期 snapshot/ref；
- 只有 `EventCx` / `UpdateCx` / 生成的 mutation API 可以改变受 reactive 管理的 state。

内部可以存在 `ComponentRef<'a, T>` / `ComponentMut<'a, T>` 之类受生命周期约束的临时 guard，但不得把它们存储到下一帧，也不得允许绕过 dirty/version accounting。

当前形态：

- `viso_ui::Handle<K = AnyNode>`：`BuildCx` 的 builder 返回它，`Copy`，只含 `NodeId`；`K` 是节点的种类：`view!` / `component!` 为具名节点交出 `Handle<kind::Button>`（`view.nodes.save_button`，`viso_ui::kind` 每个标准 Widget 一个 marker），builder 返回 `Handle`（即 `AnyNode`），`.any()` 忘掉种类；
- 控件 handle：需要被程序驱动的控件各有一个 handle 类型（`ModalHandle`、`PopupHandle`、`SheetHandle`、`ToastHandle`、`NavHandle`、`SearchFieldHandle`、`DataGridHandle`、`MapHandle` 等），由 `viso_ui::control_handle!` 声明：`Copy`，只含一个带 generation 的 `ControlId`，不拥有 widget heap object，不需要 `Rc<RefCell<_>>`。

```rust
let modal = modal().content(content).build(cx); // ModalHandle：Copy，只含 id
modal.open(ev);

// 在控件之前声明的闭包（包括控件自己的 handler）要用它：先铸造，再交给 builder
let search: SearchFieldHandle = cx.handle();
search_field().on_change(move |ev, q| search.set_suggestions(ev, find(q))).handle(search).build(cx);
```

- 控件 builder 的 `build(cx)` 返回 handle；`cx.handle()` 预先铸造一个，`.handle(h)` 交给 builder，`build` 绑定它；
- 控件的共享 parts 存在 `StateStore` 的 `ControlTable` 里，表只持 `Weak`；控件根节点持强引用，节点释放时 parts 一起释放，handle 随之失效；
- 方法经 context 访问：写入取 `&mut EventCx`（或 `&mut dyn StateWriter`，task 里也能用），读取取任意 `&impl Controls`（`EventCx`、`UpdateCx`、`EffectCx`、`BuildCx`、`StateStore`）；控件已不在时写入什么也不做，读取答 `None` 或空；
- 没有 state store 的 build（只做布局的 `BuildCx::new`）铸出的是 detached handle，所有调用都不生效；
- 每个控件一个 handle 类型而非一个泛型 `Handle<T>`：Rust 孤儿规则不允许 `viso-widgets` 给 `viso_ui::Handle<Modal>` 写固有方法。

内部转为 state 写入与延迟请求，而非 borrow 组件。

---

## 79. Query API

静态组件尽量使用编译期 handle。app 代码没有运行时节点查询 API。

动态查询属于工具：

- `viso inspect query <selector>`：selector 由类型、`#name`、`.Style`、`[role=…]` / `[label=…]` 等 attribute 与后代 / 子节点组合构成（`Viso_CLI.md` §24.3），由 `viso_view::inspect` 对运行时 census 与编译器 source table 的连接求值；
- UI Scenario 的节点选择：`<role> "<label>"`（semantics 精确匹配），或括号中的 inspector selector。

两者都只在工具询问时生成 census，不在 steady-state UI hot path 上；字符串只出现在工具侧。

---

## 80. Custom painter

自定义绘制分两级 escape hatch，都以 retained 数据进入 paint，不在每帧调用用户代码，也不暴露 raw backend command buffer：

1. **Primitive 级**：`Content::Drawing`（`viso_ui::Drawing`，`BuildCx::drawing` / `NodeStore::set_drawing`）持有共享的 `Arc<[Primitive]>`（quad、image、path、mesh）与自己的 drawing space，paint 时映射到节点的 box；按 identity 比较，只标 PAINT，移动或改尺寸不复制也不重建，只随 transform root 移动时不重画（ADR 0053）。primitive 可在 worker 上构建（`viso_ui::work`）。`Content::Path` 用于单条路径。
2. **Shader 级**：`Content::Shader` 以 brush program 绘制节点的 box：`.vs` 中声明的 typed shader（ADR 0017），参数为 typed record（`set_shader_params`），build 时为目标 backend 打包，同时带 CpuRaster 可执行的 CPU program，所以没有缺 CPU 路径的 shader。
3. 3D 内容走 scene lane（`Content::Scene`，`viso_render::scene3d`，ADR 0047），是 retained render data，不是 UI 节点。

per-node 每帧回调式 `Painter` trait 与可直接录制 backend 命令的 `CustomRenderNode` 都不提供：前者需要每节点 dyn 调用并绕过 patch / splice 的增量 paint，后者会把普通自定义控件绑死在某个 backend 上。

---

# Part XXVIII — 关键架构决策记录（ADR 摘要）

## `docs/adr/` 决策索引（规范来源）

`docs/adr/NNNN-*.md` 是架构决策的唯一记录；本文与 Accepted ADR 冲突时以 ADR 为准。状态取自 ADR 文件；标「已实现」的 Proposed ADR 已按其内容落地，文件状态尚未改为 Accepted。

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
| 0034 | DSL keyword tiers 与 label positions | Proposed（已实现） |
| 0035 | Behavior bytecode VM in a leaf crate | Proposed（已实现） |
| 0036 | Native schema bridge in the behavior crate | Proposed（已实现） |
| 0037 | Mounting a view's handlers on the behavior VM | Proposed（已实现；Release 路径由 ADR 0041 修订） |
| 0038 | Mounting a view's control-flow regions | Proposed（已实现） |
| 0039 | Inlining user-component instances into the mounting view | Proposed（已实现） |
| 0040 | Native control responses mounted from the handler table | Proposed（已实现） |
| 0041 | Release native lowering：handler / binding / computed 生成 Rust，VM 仅 Dev 与 dynamic | Accepted |
| 0042 | Crate DAG 修正：`viso-schema`、编译器不依赖运行时、删除 gpu→runtime 与 text→gpu | Proposed（已实现） |
| 0043 | Frame phases 与 transform plane：17 阶段、dirty worklist、transform root | Proposed（已实现） |
| 0044 | Device Class、Footprint Profile 与 Embedded（GLES3、CpuRaster、KMS、MCU-ready） | Proposed（已实现，板卡实测除外） |
| 0045 | HUD 读游戏状态：reactive game query（`@query`、注入绑定 `game`） | Accepted |
| 0046 | 一个 dev session、多个 build（`viso run` 多 target lane） | Accepted |
| 0047 | `GameViewport` 与 3D scene lane | Proposed（已实现） |
| 0048 | Output color encoding（sRGB 纹理格式与 surface） | Accepted |
| 0049 | Media through the OS players（`viso-media`） | Accepted |
| 0050 | Capture devices as extensions（`LiveSource`、`CameraProvider`） | Accepted |
| 0051 | Native packages（扩展 crate 携带 `.vs` Native Library） | Accepted |
| 0052 | Gamepads polled per frame into the game input | Accepted |
| 0053 | Extruded mesh vertices、retained drawings 与共享 worker 池 | Accepted |

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

*对应 ADR 0014/0015/0016 的依赖方向约束与 ADR 0042。*

**决定**：编译器不依赖运行时：`dsl -> schema <- ui`，`viso-dsl` 不依赖 `viso-ui` / `viso-view`，`check-deps` 检查 dsl / lsp / ui-macros / cli 的依赖闭包不含 runtime、platform、GPU backend。  
**原因**：纯 Rust、AOT、测试、可维护性。  
**代价**：需要 schema/IR bridge（`viso-schema`）。

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

**决定**：Viso main loop 拥有 scheduling；task 可 await 任意 future（`cx.spawn_then`），Tokio/Smol 等需要自己 reactor 的 runtime 由 app 或 `viso-ext` adapter 提供。  
**原因**：控制 frame/vsync/lifecycle，减少 feature matrix。  
**代价**：需要维护小型 task bridge/runtime。

### AD-10：Progressive project structure

**决定**：feature 从 `mod.rs + view.vs` 起步，复杂再拆。  
**原因**：同时适配小/大项目。  
**代价**：团队需要约定何时拆文件。

---

### AD-11：Node hot storage 使用 hybrid indexed SoA

*对应 ADR 0003。*

**决定**：`NodeArena` 中 20 字节的 `NodeLinks`（index 链接）+ `NodeStore` 中同索引的 hot SoA 列（一起增长）+ sparse cold tables。

**理由**：兼顾树遍历 locality、直接 index、可选数据空间效率和实现可控性；避免 pure object graph 与 archetype migration 的复杂度。

**代价**：store schema 演进需要更严格的 memory accounting；部分 node 会在 hot store 中保留 sentinel/compact slot。

### AD-12：`Handle<T>` 不直接暴露组件内部 state 借用

*对应 ADR 0054（控件 handle）。*

**决定**：Handle 表达 identity/capability；状态读写通过 context-scoped query、typed property/action/update API（当前形态见 §78）。控件 handle 是 `Copy` 的 `ControlId`，parts 由控件根节点持有、`ControlTable` 弱引用，控件不在时调用不生效。

**理由**：保持 lifetime、reactive tracking、hot reload state preservation 与 invalidation 的可验证性。

**代价**：某些内部高级代码比直接 `&mut T` 多一层显式 API。

### AD-13：Transform 与 Layout 使用独立失效平面

*对应 ADR 0007；ADR 0043 收紧为 transform 只标 TRANSFORM（§19.2）。*

**决定**：共享 `NodeId`，独立 transform 失效；transform table 只为 transform root 分配条目，primitive 按绘制时世界坐标存储，renderer 按 root 的 place（`Affine2`）移动 draw 与其中的 clip；scroll / transform animation 不触发布局、不重建 primitive、不上传、不重建 hit-test 结构。离屏、backdrop、material、user program 所在的 root 移动时回答 `NeedsUpload`，走正常 paint。

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

**决定**：compiler-local 名字使用 `NameId`，跨编译稳定身份使用 128-bit `SymbolId`（artifact 中为 `StableId`），运行时热路径 lower 为闭合 enum 判别值（`ControlKind`、`ControlInput`、`EventRoute`）与 dense slot（`StateId`、`ProgramSlot`）；具体 UI 实例使用 generational `NodeId`（§63.1）。

**理由**：稳定身份与执行身份具有不同优化目标；前者服务 hot reload/schema/cache/source mapping，后者服务 direct-index hot path。

**代价**：需要显式 link/lowering 阶段和更多 typed 类型，但这些类型在 release 中没有额外抽象成本。

### AD-17：Viso 自研 `viso-ende`

*对应 ADR 0012（derive 为目标，§10.5.15）。*

**决定**：Viso 内部协议、cache、snapshot、tool transport 使用 Viso-owned Ende Binary/JSON contract；RON 不属于 Viso；Serde 只作为生态 integration。

**理由**：协议 ABI、分配行为、bounded decode 与工具链版本语义属于框架架构。

**代价**：需要维护 binary/json encoder/decoder、fuzz 与 compatibility tests，以及目标中的 derive。

### AD-18：基础数值与几何使用独立 `viso-math`

*对应 ADR 0012、0042（几何只有 `viso-math` 一个 owner，§10）。*

**决定**：`viso-math` 提供 allocation-free vector/matrix/rect/size/transform/geometry primitives，是 `no_std`、无依赖的 DAG 叶子，可被任意上层 crate 使用（当前 ui、widgets、render、svg、behavior、dsl、image、pdf、dev-protocol、extras 与 facade）；`check-deps` 拒绝 `crates/math` 之外的 public `Rect`/`Point`/`Vec2`/`Affine2` 等几何类型。Math ABI 不等于 GPU ABI。

**理由**：这些类型横跨 layout、input、render、shader interface、text geometry、animation 与 game；不应被某个上层 subsystem 所有。

**代价**：需要严格控制 crate scope，防止 `math` 退化成新的 `utils/core` 垃圾桶。

### AD-19：`viso` CLI 是统一 Tooling Facade

**决定**：项目创建、环境诊断、build/run/serve、language tooling、test/inspect/profile、package/export 统一通过 `viso`；CLI、Studio、IDE、CI 复用同一套 tooling services。Desktop host 由不带 target 的 `viso run` 隐式确定；一次 `viso run` 可以带多个 target lane，共用一个 dev session（ADR 0046）；Android/iOS 开发环境分别使用 `viso android` / `viso ios`。

**理由**：减少平台命令碎片，给人类和 AI 提供一致入口，并防止 Studio/CLI 分叉出重复 build/compiler 逻辑。

**代价**：需要稳定 command grammar、Ende JSON machine protocol、exit codes，以及 Android Emulator/iOS Simulator tooling adapter。

### AD-20：Hot Reload 只存在于 Dev artifact

*对应 ADR 0015、0016。*

**决定**：Dev Runtime、Hot Reload transport、PatchBundle apply、DevSnapshot endpoint 与 hot-reload-only metadata 只编入 Dev artifact；Release/Shipping build graph完全不包含这些路径，不能在运行时重新开启（`cargo xtask check-release-absence` 在 CI 中验证）。

**理由**：Hot Reload 是开发工具能力，不是产品 runtime capability。编译期移除可以降低发布包攻击面、体积和复杂度，并保证 release frame hot path 零热更分支税。

**代价**：Rust/native 修改不能在 release artifact 上动态注入；开发期必须通过 Dev artifact，Rust code change使用 Stateful Warm Restart。

### AD-21：Development CLI 采用 host implicit、mobile simulator/emulator explicit

**决定**：macOS/Windows/Linux开发统一使用 `viso run`；不提供 `viso run macos/windows/linux/host`。`viso run ios/android` 只面向 Simulator/Emulator，`--device` 只选虚拟设备 profile。Android/iOS SDK/runtime环境由平台子命令管理；physical-device debugging不属于 Viso 1.0 development CLI。Embedded 板卡是 `Viso.toml` 中声明的 device target，按 §50A.7 单独定义 deploy 与远程 session。

**理由**：Desktop target由当前 host唯一决定，显式平台名只制造无效命令组合；移动开发真正需要选择的是 Simulator/Emulator runtime/profile，而不是重复描述 host平台。

**代价**：如果未来加入真机调试，需要单独定义其 security/signing/connection contract，不能偷偷扩展当前 `--device` 语义。

### AD-22：字体系统采用 On-demand OS Fallback + byte-budgeted SLRU + page-aged Atlas

*对应 ADR 0025/0026；packaged-WOFF2 条款已被 ADR 0028 取代。*

**决定**：Viso 不在启动时扫描系统字体，也不建立 framework-owned FontDB。`assets/fonts/` 在 build-time 自动进入 compact `FontManifest`，Runtime lazy load；Native 无 App font 时直接使用系统 UI/font fallback，CJK/Emoji 以 locale-aware cluster/run 交给 OS resolver；WASM/Canvas 无隐式系统字体，但可使用 packaged SFNT 字体或 External FontProvider。Loaded Font Face 使用按字节预算的 Segmented LRU；Shaping Cache 独立；glyph texture atlas 使用 page-level frame-age/CLOCK 淘汰；各项字节预算来自 `MemoryBudget`。稳态高刷文字帧不得执行 resolve/parse/shape/raster。

**理由**：避免系统字体数量影响启动，减少常驻内存和一次性 font-picker scan pollution；复用 OS 已有的 CJK/Emoji fallback 能力而不是复制 FontDB；SLRU 保护真正热 face；page-aged atlas 避免逐 glyph LRU 与 full-atlas reset；Retained ShapedRun/AtlasEntry 让 120/144/240Hz 的成本与变化量而不是总 glyph 数相关。

**代价**：首次 cold system fallback 和新 glyph raster 仍有成本，因此必须有 Resolve/FallbackPlan cache、prewarm、worker/staging、viewport prefetch、main-thread work budget、memory-pressure policy 与高刷 benchmark。完整合同见 `Viso_Text_Font_Runtime.md`。框架不解码 WOFF2（ADR 0028）。

### AD-23：Glyph 渲染使用 Adaptive Coverage + MTSDF + Vector Pipeline

*对应 ADR 0025。*

**决定**：稳定 UI、小字号、CJK、编辑器和文档默认使用目标 device-pixel bucket 的 A8 Coverage；持续 zoom/scale/rotation 的 monochrome glyph 按需异步 promotion 为 MTSDF；变换稳定后重新生成精确 Coverage 并切回；极端 zoom/高精度场景使用 retained OutlineVector；Color Glyph 使用独立 RGBA/Vector 路径。MTSDF/Vector 都不是 FontFace load 时的 eager 资源，也不能永久与 Coverage 为所有 glyph 双份常驻。

当前状态：MTSDF 与 OutlineVector 的生成、atlas 与 lane 已实现，但 lane 选择尚未接入绘制，所有文本以 Coverage 或 Color 绘制（`Viso_Text_Font_Runtime.md` §13）。

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

- 用户用 typed handle（§78）；
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
- profiler 必须暴露 `static_binding_eval`、`dynamic_binding_eval`、`dynamic_subscribe`、`dynamic_fallback_nodes` 等计数（`viso_ui` binding 计数；`viso dump` 的 reactive IR 报 `dynamic_fallback_nodes`）；
- Strict CI 场景中，普通 typed widget/page 出现新的 dynamic fallback 可以直接失败（`reactive_binding` bench 启动时断言 static 路径的 dynamic 计数为零）；
- benchmark 必须同时覆盖 100% static、mixed 10% dynamic、100% dynamic，量化性能悬崖，而不是只测静态 happy path。

---

## 83. 风险：过度 AOT 损害 live-editing / hot-reload 特性

解决：Dev 和 Release 执行同一种 UI IR 语义。

- Dev：IR 来自增量 compiler，行为由 VM 解释执行（`vm` feature 且 debug build）；
- Release：IR 来自 build-time compiler，行为生成 Rust 代码（ADR 0041）。

不维护两套 UI runtime：结构、binding 表、state layout、handler table 是同一份；只有行为函数体有 VM 与 native 两种执行方式，二者由 differential test 锁定为同一语义（§2.3）。

---

## 84. 风险：crate 太少导致编译慢/边界不够硬

当前的 crate 集合（§9）是起点，不是永远不拆；拆分依据必须是测量与依赖边界，而不是目录美学。

边界不能只靠人工复盘：CI 每次运行 `cargo xtask check-deps`（§10.2）；周期性架构复盘只评估是否需要拆 crate。持续检查：

- forbidden crate edges：当前（allowlist、`NEVER` 列表、compiler-side 依赖闭包、未登记 member、重复几何类型）；module 级 edge 为目标；
- compile timings：目标；
- dependency fan-in/out：目标；
- unsafe boundary：目标（§61）；
- feature independence：`check-targets`（每个支持的 target 跑 clippy）与 `check-profiles`（每个 profile 的 release 依赖闭包）在 CI 中执行。

---

## 85. 风险：自己维护 GPU RHI 成本高

这是有意取舍，但 Viso 1.0 不把“所有 GPU 代码都必须从零自研”当成目标。

Viso **必须拥有 RHI contract、resource lifetime、render batching、Shader ABI 与性能语义**，因为这些会直接约束 UI/render hot path；backend 的具体实现则可以来自：

- 从经过验证的 backend 实现中提炼；
- Viso 自研；
- experimental/reference wgpu adapter；
- 对特定平台的成熟底层依赖。

当前六个 backend 都是 Viso 自研，直接建在 `ash`、`windows`、`objc2`、运行时加载的 EGL/GLES 与 `web-sys` 之上；没有 wgpu adapter；`naga` 只在 shader 工具链与测试中把 WGSL 翻译成 MSL/SPIR-V。

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

### 尚未满足

- Engineering：CI 中唯一的性能 gate 是 `check-frame-tax`（dev 层的帧开销），没有通用 benchmark 回归 gate；Studio 不存在；
- Tooling：Studio 未建立；`web-dom` / `web-hybrid` 不可构建；
- Footprint：`cargo xtask footprint` 的基线手动记录，不在 CI 中；没有板卡实测。

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
    └── UpdateTransforms ──→ root place（Affine2）→ Renderer::set_transforms（不经过 paint）
    ↓
Paint Changes（patch / splice，Rendering §9.7）
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

用户（`.vs` handler，或 Rust handler 经 `EventCx` 写入，ADR 0005/0006）：

```text
on click { count += 1; }
```

```rust
count.update(ev, |n| n + 1); // count: State<i32>
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
MEASURE bubbles until an ancestor whose size does not follow its content (§19.1)

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
scroll delta（只标 TRANSFORM）
        ↓
visible range changes by 3
        ↓
park 3 rows past one end
mount 3 rows uncovered at the other end, bind 3 new item records
overlap keeps its keys and offsets
        ↓
new rows splice into the paint stream; only their instance ranges upload
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

当前包含（摘要）：

- 入口：`Application`、`run`、`AppCx`、`BuildCx`、`EventCx`、`ServicesExt`；
- tree 与 state：`NodeId`、`DirtyClass`、`State`、`StateId` / `StateValue`；
- 手写组件：`Component`、`Sizing`、`Axis`、`Align`、`Justify`、`Inset`、`BoxStyle`、`Rgba`；
- 路由：`Navigator`、`router_view`、`routes!`；
- layout 与内容：`FlexStyle`、`LeafStyle`、`GridStyle`、`GridPlacement`、`TrackSizing` / `TrackMax`、`AdaptiveColumns`、`VirtualListStyle`、`SpanStyle`、`TextRequest`、`CursorIcon`、`InputKind`；
- semantics：`Role`、`Semantics`；
- 官方 widget 及其 style、builder 函数与控件 handle / handle slot（Tier 1–4，以及 Avatar、Badge、Chip、Calendar、DatePicker、TimePicker、ColorPicker、NumberField、SearchField、SegmentedControl、RangeSlider、Stepper、Spinner、ProgressIndicator 等）；forms（`Form`、`Field`、`Fieldset`）；`DataGrid`；
- 窗口与菜单：`window`、`WindowBuilder`、`WindowConfig`、`WindowChrome`、`WindowHandle`、`Menu`、`MenuCommandId`、`Accel`、`SystemAction`；
- 几何：`viso_math` 的 `Point`、`Rect`、`Vec2`；
- 宏：`ui!`、`component!`、`view!`。

`asset!` 在 crate 根导出，不在 prelude；`viso::version()` 也在 crate 根，是构建所用 Viso 的包版本（`CARGO_PKG_VERSION`）。不提供 `#[component]` 属性宏：`component!`、`view!` 与 `Component` trait 已覆盖 Rust 侧组件，第四个入口违背三入口合同（§0.2）。

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
2. Handle：**不直接暴露跨帧 typed state borrow/mutation**（AD-12）；
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
13. 编译器依赖：**`dsl -> schema`，编译器不链接运行时栈**（ADR 0042，已实现）；
14. Transform：**transform root + draw 级 place（`Affine2`），TRANSFORM 不连带 PAINT/HIT_TEST**（ADR 0043，已实现）；
15. Embedded：**Device Class + Footprint Profile；Embedded-GPU 为 1.0 Tier-1，Embedded-CPU 为 Tier-2，MCU 只守不变量**（ADR 0044，已实现）。

这些决定应尽早被 prototype/benchmark 验证，但验证的默认动作是调整实现参数，而不是重新打开核心语义。若要推翻，必须新 ADR。

## G.2 仍需 prototype/benchmark 的问题

标「已决」的条目已由实现或 ADR 定下，保留编号以免引用错位。

1. 已决：clip 按代价分级（scissor → analytic rounded rect → coverage mask → retained R8 cache，`viso_render::clip`），由 retained `ClipChain` 承载（Rendering §14.2）；transform root 内打开的 clip 为每帧 intern 的链，随 root 的 place 移动；
2. 已决（ADR 0025）：shaping cache 以 run 为键，text、font、features、fallback chain 不变即命中，字节预算为 `MemoryBudget::shaping`；
3. 已决：每个 instance family 一个持久、只增长的 pool，CPU shadow 逐元素 diff，脏区间合并后上传（`viso_render::pool`）；
4. Web 无 WebGPU 时的兼容路径：§30.4 规定 CpuRaster 经 canvas `putImageData`，尚未实现，`web-gpu` 构建在没有 WebGPU 的浏览器上没有回退；是否另做 WebGL2 backend 视 adoption 数据决定；
5. 已决：Linux / Android 以 `gles` feature 选 GLES3（静态选择，ADR 0029），默认 Vulkan；
6. 已决（ADR 0041）：VM 是 `vm` feature，只在 debug build 执行 view 行为，release 一律走生成的 Rust；
7. platform backend 何时从 module 升格独立 crate；
8. a11y：Viso 自有的 `SemanticsDelta` 是 platform seam，AccessKit 只在 macOS/Windows/Linux/Android adapter 内转换，iOS 与 Web adapter 直接消费 `SemanticsDelta`（ADR 0030）；剩余能力缺口（Web 焦点跟随、文本编辑语义）待定；
9. 已决：Rust 侧除 `ui!` / `component!` / `view!` 外，有 `BuildCx` 命令式 builder 与 widget builder 函数（`button(..)`、`label(..)`，ADR 0003/0006）；
10. 已决（ADR 0047）：3D 内容是 `viso_render::scene3d` 中的 retained scene lane，不是 UI 节点；
11. paragraph cache：内存上限已决（`MemoryBudget` 的 `faces` / `shaping` / `glyph_atlas`）；是否跨 component 共享仍待定；
12. 已决：lowering 在累计丢失 64 次 join 或留下一半 live slot 后整段压实；glyph store 中删除的 glyph 超过一半时打包（Rendering §9.7）；
13. 已决：`embedded-gpu` profile 统一用 GLES3（`viso/gles` + `viso/kms`）；KMS 上不提供 Vulkan；
14. 已决：全局 `MAX_RECTS = 4`、`FULL_FRAME_SHARE = 0.6`（`viso_render::damage`），不按 profile 区分；
15. 已决：draw 级 place 为 `Affine2`（32 字节 inline view uniform：viewport、`shift`、`axis_x`、`axis_y`）；离屏、backdrop、material、user program 所在的 root 回答 `NeedsUpload`，走 paint；
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

