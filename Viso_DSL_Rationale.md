# Viso DSL 1.0 设计说明（Rationale）

> 文档性质：说明性（Informative）。本文收录从 `Viso_DSL_1.0.md` 移出的设计判断、外部参考、实现提示与证据说明，不定义任何兼容性规则；与规范冲突时以 `Viso_DSL_1.0.md` 为准。  
> 章节编号沿用原规范编号（§1.1、§2–§8、附录 D–F），便于既有引用定位；规范中对应位置保留一行指针。  
> 本文中的 ` ```viso ` 代码块同样受规范 §152.1 文档示例测试约束。

---

# 第一部分：总体判断与设计结论

## 1. 对人类是否足够清晰

### 1.1 结论

Viso DSL 1.0 的设计目标是让人类开发者获得清晰且可渐进学习的 authoring surface，同时保留完整的 VM、HIR、Shader ABI、热重载与游戏扩展能力。初学者不需要先理解这些底层实现。

初学者只需先掌握以下十个概念：

```text
import
component
input
state
computed
action
view
node
property: expression;
on event { ... }
```

最小 Counter：

```viso
import viso::widgets::{Window, Column, Text, Button};

export component Counter {
    state count = 0;
    computed label = format("Count: {}", count);

    view {
        Window {
            Column {
                Text {
                    text: label;
                }

                node add_button: Button {
                    text: "Add";
                    on click {
                        count += 1;
                    }
                }
            }
        }
    }
}
```

规范性的清晰度硬规则与学习分层见规范 §1.2、§1.3。

---

## 2. 可扩展性与灵活度

### 2.1 结论

Viso DSL 1.0 的扩展原则是：

> **扩展类型、组件、Trait、事件、Native 服务和 Profile，而不是让每个库扩展新的标点语法。**

这样能同时获得：

- 接近开放宿主脚本的表达能力；
- 稳定的 Parser、Formatter 与 LSP；
- 可静态检查的跨 Rust 边界；
- 不需要修改解析器即可增加新 Widget、新游戏 API、新音频 API 或新数据服务。

### 2.2 扩展面

第三方库可以扩展：

- `component` Schema；
- `record`、`enum`、`trait` 和泛型类型；
- Typed Event；
- `native fn`、`native action`、`native task`；
- `Handle<T>` 的方法；
- System Trait，例如 `FixedUpdate`、`AudioProcess`；
- Shader intrinsic 和受支持的纹理/Buffer 类型；
- Attribute Schema，例如 `@derive(...)`、`@stable(...)`；
- Widget Property 的 `invalidates` Dirty Class 集合（规范 §87）。

第三方库禁止在不修改语言规范并通过 ADR/兼容性检查的情况下引入：

- 新运算符；
- 新括号类型；
- 新的隐式赋值符号；
- 改变现有关键字含义；
- 无 Schema 的动态字段；
- 绕过 Capability 的宿主调用。

### 2.3 为什么这仍然足够灵活

大多数领域扩展并不真正需要新语法。例如，游戏能力可以由以下 API 提供：

```viso
import viso::game::{GameWorld, EntityId, FixedUpdate, FixedFrame};

export system PlayerController implements FixedUpdate {
    input world: Handle<GameWorld>;
    input player: EntityId;

    action fixed_update(frame: FixedFrame) {
        let input = frame.input;
        world.walk(player, input.move_x * 6.0f32, input.move_z * 6.0f32);
    }
}
```

Parser 不需要认识 `GameWorld`、`walk` 或 `FixedUpdate`；这些能力来自 Native Schema 和 Trait 合同。

---

## 3. AI 生成友好度

### 3.1 结论

Viso DSL 1.0 对 AI 生成是友好的，但前提是实现下列工具合同，而不是只依赖模型记忆语法：

```text
固定 EBNF
+ 唯一规范格式
+ 机器可读 Schema
+ JSON 诊断
+ 结构化 Fix
+ 小范围增量检查
```

### 3.2 AI 友好的具体设计

- 一个概念尽量只有一种规范写法；
- 简单语句必须有分号，避免换行敏感；
- 没有 `child` 可选写法；
- 没有事件处理箭头缩写；
- 没有 `Float` 模糊别名；
- 没有字符串形式的枚举、属性或事件；
- 属性、事件和 Slot 都由 Schema 查询；
- 动态列表强制 `key`；
- 诊断包含错误码、主位置、关联位置、期望类型与自动修复；
- Formatter 输出唯一规范形态；
- Compiler 可以输出 AST/HIR/IR 的 JSON 摘要供 AI 检查。

### 3.3 AI 仍可能出错的区域

以下区域即使有形式文法，也必须依赖 Schema 和编译器检查：

- Widget 是否具有某个属性；
- Event Payload 的字段；
- Native 方法所需 Capability；
- 某属性的 `invalidates` Dirty Class 集合；
- Shader Backend 是否支持某 intrinsic；
- Resource Policy 是否可组合；
- Trait 是否满足；
- Hot Reload 是否允许状态迁移。

因此，AI 工作流必须是“生成—检查—读取诊断—修复”，而不是一次性盲写。

---

## 4. 表达能力与游戏能力

### 4.1 普通应用表达能力

语言可表达：

- 声明式 UI 树；
- 响应式状态和派生值；
- Typed Event；
- 动态条件与 Keyed List；
- 同步 Action；
- 生命周期 Effect；
- 异步 Task；
- 缓存 Resource；
- Template、Slot、Style 和 Theme；
- Native 服务；
- GPU Shader；
- Hot Reload 状态迁移。

### 4.2 是否适合做游戏

**适合。** 但必须区分“语言能力”和“游戏引擎能力”。Viso DSL 负责提供可静态检查的游戏执行语义，具体物理、ECS、音频和资产系统由 Rust Runtime/Profile 提供。

Viso DSL 提供：

- 状态、函数、Action、闭包和模式匹配；
- 一等 `system`；
- Fixed Update Trait；
- Typed Native Handle；
- Shader 域；
- 事务和确定性执行模式；
- 热重载和状态迁移。

游戏 Profile 或 Rust Runtime 提供：

- ECS/Entity；
- 物理、碰撞和 Raycast；
- 输入快照；
- 相机；
- 音频；
- 场景和资源；
- 固定步长 Scheduler；
- GPU 绘制。

因此 Viso 不需要把 `game` 设成语法关键字。它通过导入 `viso::game`、实现标准 Scheduler Trait，以及可选的 Quick Game Profile 获得游戏能力；所有路径最终进入同一套 typed scheduler/runtime。

### 4.3 游戏能力边界

只有 DSL 而没有 Rust 游戏 Runtime 时，语言不能凭空提供：

- 高性能碰撞；
- 复杂物理；
- 模型和动画加载；
- 音频混音；
- GPU 资源管理。

这与普通编程语言本身不会自动成为游戏引擎是同一回事。

---

# 第二部分：外部参考——Makepad Authoring 与 Game 经验

## 5. 参考范围与证据等级

本文仅把 Makepad 当前 `dev` 分支中的 Rust 内嵌脚本体系作为外部实现参考：

```text
Rust source
  -> script_mod! { ... }
  -> ScriptVm
  -> mod.prelude / mod.widgets 等脚本 namespace
  -> Rust 类型与 Widget 注册
  -> App::from_script_mod(...)
```

当前 Script tokenizer/parser 可观察到 Identifier、Operator、Separator、括号、字符串、多种数值宽度、颜色、RustValue、字段访问、Optional Field、算术/位运算/比较/逻辑/Range、多类 Assignment Operator，以及 `for`、`while`、`loop`、`match`、destructuring、closure 和 streaming parser checkpoint。

重要说明：

> Makepad 仓库没有把当前 Script Surface 发布成一份单一、权威、完整的 EBNF。本文只记录公开源码中可观察到、且对 Viso authoring/runtime 设计有参考价值的语义，不把它声明为 Makepad 官方语言标准。

主要源码依据：

```text
platform/script/src/tokenizer.rs
platform/script/src/parser.rs
splashgame.md
仓库内 script_mod!/ScriptVm 使用示例与开发说明
```

---

## 6. 当前 Makepad Script 的核心 authoring surface

Viso 设计主要参考以下 authoring surface：

```text
property: value       普通属性/字段应用
name := Type { ... }  具名实例与身份
object +: { ... }     merge/apply
#(rust_expr)          Rust/native bridge
mod.widgets.*         脚本 namespace / 注册后符号访问
```

这些符号之外，Script 还拥有普通表达式、控制流、闭包和宿主注入对象，因此它既可以写 UI，也可以承载较自由的运行时脚本。

### 6.1 优点

- UI 表面语法紧凑，属性 `name: value` 的阅读密度高；
- `ScriptVm` 与 Rust 注册机制让 Widget、Native 对象、游戏 API 和 Shader 能快速暴露给脚本；
- 很适合 Studio、AI 实时生成、小型工具和游戏原型；
- UI、普通脚本和 Shader 在视觉上保持较统一的“对象 + 属性 + 行为”模型；
- 热更新链路与脚本执行模型结合紧密。

### 6.2 结构性代价

- `:=`、`+:`、`<:`、`>:`、`^:` 等符号把身份、merge、方向和 apply 语义压进标点；
- module resolution 与 Rust/脚本注册顺序存在运行时纪律；
- Native bridge 与动态 property/method surface 依赖运行时 VM；
- 大型项目中的属性、事件、Native API 和模块关系难以全部提前静态验证；
- 小型脚本的自由度与大型工程的严格语义没有明确分层。

---

## 7. Makepad 参考经验与 Viso 1.0 的取舍

Viso 保留 Makepad authoring surface 中最容易读、最有生产力的部分，但不保留隐藏语义的 Assignment-family。

| 能力            | Makepad 当前 Script                   | Viso 1.0                                              |
| --------------- | ------------------------------------- | ----------------------------------------------------- |
| View 属性       | `property: value`                     | `property: expression;`                               |
| 普通变量赋值    | 多类 assignment                       | `=` 与普通复合赋值                                    |
| 具名节点身份    | `name := Type {}`                     | `node name: Type {}`                                  |
| Merge/Apply     | `+:` 等                               | `style` / `override` / `replace` / 显式 Record Update |
| Rust bridge     | `#(rust_expr)` + runtime registration | 生成的 Typed Native Schema                            |
| 模块共享        | `mod.*` + 初始化/注册关系             | 编译期 Module Graph + Import                          |
| Property lookup | 动态 surface 为主                     | Typed `PropertyId`，动态能力必须显式                  |
| State/派生值    | 脚本变量与宿主约定                    | `state` / `computed`                                  |
| UI 更新         | 运行时脚本/渲染约定                   | Reactive Binding + 精确 invalidation                  |
| 列表身份        | 由代码/宿主保证                       | `for ... key ...` 强制 StableKey                      |
| 游戏 Tick       | 宿主 `game` API / tick callback       | `system` + `FixedUpdate` Profile                      |
| Shader          | Script/Shader 深度结合                | 独立 Shader Domain + 显式 Descriptor ABI              |

Viso 的原则是：**保留紧凑度，不保留隐式语义；保留宿主扩展能力，不让运行时注册顺序成为语言模块系统。**

### 7.1 游戏 authoring 对照

Makepad 的单文件游戏脚本（`splashgame.md`）证明了几件事值得保留：顶层构建语句加一个 60Hz 固定步长回调就能开始；大量高层动词（地形、相机 Rig、Prefab、行为、粒子、合成音效）让原型极快；Input Tape 测试让 AI 能闭环；按字段区分 Shared/Derived/Local 为联机铺路。

它的代价也很清楚，Viso 逐项给出结构化替代：

| 方面               | Makepad 游戏脚本                          | Viso 1.0                                                  |
| ------------------ | ----------------------------------------- | --------------------------------------------------------- |
| 长期状态           | 可变闭包捕获                              | `system state`，按 Stable ID 迁移                         |
| 状态分层           | Shared/Derived/Local 是约定               | `@local` + 编译期 Simulation 域检查（`E9103`–`E9105`）    |
| 计时器             | `every`/`after` 注册闭包                  | `Cooldown`/`TickTimer` 值类型，按整数 Tick，可快照        |
| 游戏时间           | 编辑后 `game.time()` 重置                 | `tick × fixed_dt`，Logic-only Reload 不重置               |
| 编辑后             | 整个世界重建，状态丢失                    | 按 Diff 分类：Logic-only / Presentation-only / World Rebuild |
| 存档               | `save`/`load` 字符串键                    | `@persist("key")`，Capability 检查 + 迁移                 |
| 输入               | 命名动作，手柄自动对等                    | Typed Enum + `@const` `InputMap`，缺少路径报 `E9107`      |
| Tag / 音效         | 字符串                                    | Typed Enum 或 Resource Key                                |
| 未知 API           | 运行时最近建议                            | 编译期 `E2001` + 按类型过滤的最近候选 Fix                 |
| 回放测试           | Input Tape + Probe + 截图 Sheet           | 同样能力，外加 Snapshot Hash 与 `cross_platform` 档位     |
| 调试               | 日志与 peek                               | 同上，外加 Snapshot 环形缓冲与回溯重放                    |

结论：Viso 的原型速度来自 `viso::game::quick` 与 `viso::game::kit`，与 Makepad 同级；可维护性、确定性和热重载保真度来自类型系统与编译器生成的 Snapshot，这是 Makepad 靠约定做不到的。

---

## 8. 外部参考边界

Makepad 只作为实现经验参考。本文只提取对 Viso 设计有价值的可观察经验。

允许参考：

```text
紧凑 property authoring
实时编辑与 Last-good 体验
shader / UI / game 的工具链联动
固定步长 game update 的开发体验
轻量 Native API 暴露方式
Studio/AI 自动化体验
```

禁止由参考实现反向决定：

```text
Viso public syntax
Viso module system
Viso identity model
Viso reactive semantics
Viso ABI
Viso runtime lifecycle
```

原则：**参考有效经验，不继承兼容负担。**

---

# 附录 D：可直接交给实现 AI 的主提示词

```text
你正在实现 Viso DSL 1.0。唯一语言规范是
`Viso_DSL_1.0.md`，其中附录 A 的 EBNF 是权威 Parser 合同。

必须遵循：

1. 不得发明规范外语法、关键字、单位或隐式类型转换。
2. 不得加入 `child`、事件箭头、`Float`、`:=`、`+:` 等已删除语法。
3. Lexer 必须保留 Trivia 和完整 Source Range。
4. Parser 必须建立 Lossless CST，并对不完整源码产生 ErrorNode/MissingToken，禁止 panic。
5. AST 必须分别保留 ViewFor、BehaviorFor、PropertyBinding、Assignment、EventHandler、MatchArm、Resource、Shader 等节点，不得过早揉成通用 Map/Call。
6. Name Resolution、Type Check、Effect Check、Capability Check 必须在 HIR 完成。
7. 每实现一个 EBNF Production，都同时添加：
   - 至少一个合法测试；
   - 至少两个非法/恢复测试；
   - Formatter round-trip 测试；
   - 必要的 JSON Diagnostic Golden Test。
8. 每个功能 PR 要小且可回滚，不得一次全库重写。
9. 修改语法前先更新规范、Parser Golden、Formatter、Schema Golden 和测试；未经批准不得偏离规范。
10. UI 状态更新必须通过 Transaction 和 Reactive Graph，不得要求用户手工 render 全树。
11. 动态 View List 强制 Stable Key；Branch Cache 使用 Preserve Literal，二者不得共用实现入口。
12. Native 调用必须通过 Typed Schema、Capability、Ownership 和 Thread Domain 检查。
13. Shader 必须使用安全 Descriptor ABI，禁止从 Rust 结构某字段向后读取任意内存。
14. 游戏 Tick 通过 System Trait/Scheduler 实现；Parser 不得硬编码 `game` 对象或具体引擎 API。
15. 任何失败的 Hot Reload 都必须保留 Last-good Code/UI/World。

每轮工作流程：

A. 阅读规范对应章节和 EBNF Production。
B. 检查现有 AST/HIR/Runtime 边界。
C. 写失败测试。
D. 实现最小功能。
E. 运行 fmt、parser tests、type tests、runtime tests。
F. 输出 JSON Diagnostic 样例和 IR Dump。
G. 更新 `docs/language/STATUS.md`，记录：已完成、未完成、偏差、风险、下一 PR。

不得通过以下方式“解决”错误：

- 把类型改成 String/Dynamic；
- 忽略未知 Property/Event；
- 在 View 中执行副作用；
- 用数组索引作为通用 Key；
- Catch Native Panic 后静默继续；
- Hot Reload 失败后清空 UI；
- 删除测试或降低断言；
- 通过正则修改 Parser 语义。

开始前先输出本次计划、涉及 Production、预期 AST/HIR、测试矩阵和回滚点；然后直接实施，不要要求用户再次确认已经明确的设计。
```

---

# 附录 E：设计质量复评

以下为架构判断，不是性能 Benchmark，也不是评分：

| 维度           | 成立条件                                    | 主要剩余风险                             |
| -------------- | ------------------------------------------- | ---------------------------------------- |
| 人类清晰度     | 文档、Formatter、Schema 同步                | 关键字数量较多，高级区分需教学           |
| 易上手性       | Quick Start 只展示 Level 1                  | 一开始展示 Effect/Task/Shader 会造成负担 |
| 可扩展性       | 扩展 Schema/Trait/Profile，不扩标点         | Plugin Schema 版本管理复杂               |
| 灵活度         | 保留 Native/System/Shader Escape Hatch      | 严格类型会比动态脚本多写一些声明         |
| AI 生成友好    | EBNF + Schema + JSON Diagnostic 真正实现    | 只写文档而不做工具时 AI 友好度会明显下降       |
| 表达能力       | 标准库和 Runtime API 完整                   | DSL 不应承担所有底层算法                 |
| 游戏能力       | 有 Game Profile、ECS/物理/输入/音频 Runtime | 大型游戏资产与编辑器仍是独立工程         |
| 编译器可落地性 | 按阶段实现，不一次性全做                    | 类型、响应式和热重载组合工程量很大       |

### E.1 人类清晰度的关键判断

新版不是“语法越少越好”，而是“同一概念只有一种写法”：

```text
匿名孩子       Type {}
具名孩子       node name: Type {}
属性           name: expression;
双向绑定       bind name <=> state;
事件           on click { ... }
动态列表       for ... key ... {}
分支缓存       if ... preserve "..." {}
同步修改       action
异步工作       task/resource
帧循环         system implements Trait
```

这组规则可以形成稳定心智模型。

### E.2 可扩展性的关键判断

Viso 的灵活性来自：

```text
类型系统
+ Trait
+ Component Schema
+ Native Handle
+ System Scheduler
+ Shader Domain
+ Profile
```

而不是来自不断增加 `@#$:+` 组合。这样新增游戏、音频、图表、地图、编辑器或数据库领域时无需修改核心 Parser。

### E.3 AI 友好的关键判断

仅有 EBNF 仍不够。AI 友好度取决于：

```text
语法唯一性
+ 可查询 Schema
+ 机器可读诊断
+ 自动 Fix
+ AST/HIR Dump
+ Snapshot/Test Harness
```

规范把这些定义为语言交付的一部分，而不是后续可有可无的附属工具。

### E.4 游戏能力的关键判断

Viso 可以同时承载快速游戏原型与结构化 System 游戏逻辑的关键原因是：

- Behavior Language 有控制流、Pattern、Closure 和 Typed Call；
- System 有长期 State；
- Trait 把 Tick/Collision 等 Hook 标准化；
- Native Handle 注入 ECS/物理/输入/音频；
- Shader 域负责 GPU 代码；
- Hot Reload 在 Tick/Frame Boundary 原子切换；
- Game Profile 定义确定性和预算；
- Simulation/Local 分层由编译器检查，所以 Snapshot、回放、回滚和存档是生成的，不靠作者自律。

因此类型更严格并没有削弱游戏能力，反而让 AI 生成的游戏逻辑更容易检查、回放和迁移。

---

# 附录 F：资料与证据说明

## F.1 Makepad 外部参考依据

本文把 Makepad 当前公开源码中可观察到的 Script、实时编辑、游戏 authoring、Shader/Widget 与工具链经验作为外部参考。

主要源码入口：

- <https://github.com/visoui/makepad/blob/dev/platform/script/src/tokenizer.rs>
- <https://github.com/visoui/makepad/blob/dev/platform/script/src/parser.rs>
- <https://github.com/visoui/makepad/blob/dev/splashgame.md>

## F.2 证据边界

- 本文没有声称 Makepad 官方发布过一份当前 Script 的完整 EBNF；
- Makepad Script 的文法/语义描述仅用于外部实现经验参考；
- 规范附录 A 的 EBNF 是 Viso DSL 1.0 的 Parser 合同；
- Game Profile 示例证明的是语言承载能力，不代表物理、ECS、音频和资产系统已经自动实现；
- 性能结论必须由 Viso 实现后的 Benchmark 验证。
