# Viso DSL 1.0：语言设计与形式化编译规范

> 文档状态：Viso DSL 1.0 Draft / Not Final  
> 规范级别：语言语法、静态语义、运行时语义与编译器 Lowering 合同  
> 目标读者：Viso 编译器、运行时、Widget、Shader、游戏 Profile、LSP 与 AI 编码代理实现者  
> 基线日期：2026-09-02  
> Makepad 对照统一基于当前 `script_mod!` / `ScriptVm` / `App::from_script_mod` 路径；Viso 外部 DSL 文件统一使用 `.vs`。

---

## 0. 规范词语与文档范围

本文使用以下规范词语：

- **必须（MUST）**：兼容实现不得违背；
- **禁止（MUST NOT）**：实现必须拒绝或诊断；
- **应该（SHOULD）**：除非存在记录在案的理由，否则应遵循；
- **可以（MAY）**：可选能力；
- **规范性（Normative）**：决定兼容性的规则；
- **说明性（Informative）**：用于解释，不覆盖规范性规则。

本文完整定义：

1. Viso DSL 1.0 的设计目标与可用性判断；
2. 对 Makepad 当前 `script_mod!` / `ScriptVm` 脚本体系进行语义与迁移对照；
3. UTF-8、标识符、注释、保留字、字面量与单位的词法规范；
4. 模块、类型、组件、System、函数、行为、资源、View 与 Shader 的形式文法；
5. 表达式、运算符优先级、结合性、闭包与 Pattern 文法；
6. 类型推断、泛型、Trait、约束、转换和子类型规则；
7. 响应式状态、节点身份、事件、异步、热重载和游戏循环语义；
8. CST、AST、HIR、UI IR、Reactive IR、Script IR 与 Shader IR 的 Lowering 规则；
9. 编译器、Formatter、LSP、Schema 和 AI Vibe Coding 的交付合同；
10. 解析、类型、运行时、热重载、游戏和 AI 生成的验收标准。

本文不定义：

- 具体 GPU API 的实现；
- 具体物理引擎、音频引擎或 ECS 的内部算法；
- 完整标准库 API；
- 所有 Widget 的属性清单；
- Rust ABI 的具体符号名称。

这些内容由 Viso Runtime、Native Schema 和各 Profile 规范定义，但不得改变本文的核心语法和静态语义。

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

### 1.2 当前规范的清晰度硬规则

Viso DSL 1.0 采用以下唯一规范规则：

| 设计问题                                       | Viso DSL 1.0 的唯一规则                                            |
| ---------------------------------------------- | ------------------------------------------------------------------ |
| `child Column {}` 与裸 `Column {}` 混用        | 删除 `child`；裸组件节点就是匿名子节点                             |
| `on click => expr` 与 `on click { ... }` 并存  | 删除事件箭头简写；事件处理器一律使用 block                         |
| `Float` 有时等于 `F64`，Shader 又要求定宽类型  | 删除 `Float`；只保留 `F32` 与 `F64`                                |
| Resource 有单行子句和逗号列表两套形式          | Resource 一律使用配置 block；策略一律是 `policy = [ ... ];`        |
| `ms` 已定义而 `min`、`sp` 只在例子中出现       | 完整枚举全部单位及量纲规则                                         |
| State 默认值允许“依赖顺序明确”的前向引用       | State 初始化禁止前向引用；Computed 才允许无环前向依赖              |
| 条件分支与列表都使用 `key`                     | 分支缓存使用 `preserve "static-id"`；列表身份使用 `key expression` |
| 省略分号依赖换行猜测                           | 简单声明、属性和语句必须以 `;` 结束                                |
| `:=`, `+:`, `<:`, `>:` 等承担多种隐藏语义      | 从核心语言移除；View 属性只保留 `property: expr;`，其余使用明确关键词 |

### 1.3 学习曲线

语言采用分层学习：

- **Level 1：普通 UI**：Component、State、Action、View、Event；
- **Level 2：应用状态**：Computed、Effect、Task、Resource、Keyed List；
- **Level 3：组件库**：Slot、Template、Style、Trait、Generic；
- **Level 4：底层能力**：Native Handle、System、Capability、Shader；
- **Level 5：工具链**：Hot Reload Migration、Schema、IR、Compiler Plugin。

高级特性不会污染 Level 1 的基本语法。

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
- Widget Property 的失效标签：layout、paint、semantics、input。

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
- 某属性影响 Layout 还是 Paint；
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

### 4.2 是否能像 Makepad 那样做游戏

**可以。** 但必须区分“语言能力”和“游戏引擎能力”。

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

因此 Viso 不需要把 `game` 设成语法关键字。它可以通过导入 `viso::game` 和实现 `FixedUpdate` Trait 获得与宿主注入式游戏 API 等价、但类型更明确的能力。

### 4.3 游戏能力边界

只有 DSL 而没有 Rust 游戏 Runtime 时，语言不能凭空提供：

- 高性能碰撞；
- 复杂物理；
- 模型和动画加载；
- 音频混音；
- GPU 资源管理。

这与普通编程语言本身不会自动成为游戏引擎是同一回事。

---

# 第二部分：Makepad 当前 Script 语义对照

## 5. 对照范围与证据等级

Viso 的 Makepad 对照对象统一为当前 `dev` 分支中的 Rust 内嵌脚本体系：

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

> Makepad 仓库没有把当前 Script Surface 发布成一份单一、权威、完整的 EBNF。本文只记录迁移和架构对照所需的实现推导语义，不把它声明为 Makepad 官方语言标准。

主要源码依据：

```text
platform/script/src/tokenizer.rs
platform/script/src/parser.rs
splashgame.md
仓库内 script_mod!/ScriptVm 使用示例与开发说明
```

---

## 6. 当前 Makepad Script 的核心 authoring surface

迁移器和 Viso 设计需要特别理解以下语义：

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

## 7. Makepad 当前 Script 与 Viso 1.0 的取舍

Viso 保留 Makepad authoring surface 中最容易读、最有生产力的部分，但不保留隐藏语义的 Assignment-family。

| 能力 | Makepad 当前 Script | Viso 1.0 |
| --- | --- | --- |
| View 属性 | `property: value` | `property: expression;` |
| 普通变量赋值 | 多类 assignment | `=` 与普通复合赋值 |
| 具名节点身份 | `name := Type {}` | `node name: Type {}` |
| Merge/Apply | `+:` 等 | `style` / `override` / `replace` / 显式 Record Update |
| Rust bridge | `#(rust_expr)` + runtime registration | 生成的 Typed Native Schema |
| 模块共享 | `mod.*` + 初始化/注册关系 | 编译期 Module Graph + Import |
| Property lookup | 动态 surface 为主 | Typed `PropertyId`，动态能力必须显式 |
| State/派生值 | 脚本变量与宿主约定 | `state` / `computed` |
| UI 更新 | 运行时脚本/渲染约定 | Reactive Binding + 精确 invalidation |
| 列表身份 | 由代码/宿主保证 | `for ... key ...` 强制 StableKey |
| 游戏 Tick | 宿主 `game` API / tick callback | `system` + `FixedUpdate` Profile |
| Shader | Script/Shader 深度结合 | 独立 Shader Domain + 显式 Descriptor ABI |

Viso 的原则是：**保留紧凑度，不保留隐式语义；保留宿主扩展能力，不让运行时注册顺序成为语言模块系统。**

---

## 8. 迁移器的最小 Makepad Script 语义模型

迁移工具应针对 Rust 源码中的 `script_mod!` token stream、`ScriptVm` 创建/传递、Widget/Native 注册、`App::from_script_mod` 与脚本 namespace 构建关系做结构化分析。

迁移器至少需要识别：

```text
property binding/application
named instance identity
merge/apply operation
Rust/native bridge
module/namespace dependency
state-like persistent values
closures and event callbacks
game tick callbacks
shader declarations/usages
manual render/update calls
```

能够证明语义等价的转换才允许自动修改源码；其余输出 Assisted/Manual 诊断。Viso runtime 不包含 Makepad compatibility host。

---

# 第三部分：词法规范

## 9. 源文件与编码

- Viso 外部 DSL 文件的规范扩展名必须是 `.vs`；
- 源文件必须使用 UTF-8；
- UTF-8 BOM 可以被接受，但 Formatter 必须移除；
- 行结束可以是 LF 或 CRLF；CST 必须记录原始范围，Formatter 输出 LF；
- NUL 字符禁止出现在普通源码中；
- 编译器位置必须以 Unicode Scalar、UTF-8 Byte Offset 和 UTF-16 Code Unit 三种坐标可查询，以支持 LSP；
- Tab 被允许作为空白，但 Formatter 必须转换为空格；
- 默认缩进为 4 个空格。

---

## 10. 空白和注释

空白字符集合固定为：

```text
U+0020 SPACE
U+0009 CHARACTER TABULATION
U+000A LINE FEED
U+000D CARRIAGE RETURN
```

其他 Unicode 空白字符在字符串和注释外不属于空白 Token；Compiler 必须报告不可见字符诊断。Viso 1.0 的 XID 与 NFC 数据表固定使用 Unicode 16.0；实现必须按同一数据表执行标识符规范化与校验。

注释：

```viso
// 单行注释

/*
   多行注释；允许嵌套。
*/

/// 声明文档注释

//! 模块文档注释
```

规则：

- `//` 到行结束；
- `/* ... */` 允许嵌套；
- 未闭合多行注释是词法错误；
- Lossless CST 必须保留所有注释和空白；
- 文档注释在 AST 中降低为 `@doc(...)` 元数据，而不是普通注释。

---

## 11. 标识符

规范词法：

```ebnf
identifier_token    = normal_identifier | raw_identifier ;
normal_identifier   = xid_start, { xid_continue } ;
raw_identifier      = "r#", xid_start, { xid_continue } ;

xid_start           = "_" | Unicode_XID_Start ;
xid_continue        = "_" | Unicode_XID_Continue ;
```

规则：

- 标识符按 Unicode NFC 规范化后进入符号表；
- 两个源码拼写若 NFC 后相同，视为同一标识符；
- 编译器应警告 Unicode Confusable；
- 普通标识符不得等于保留字；
- 正文 EBNF 的 `identifier` 与合并 EBNF 的 `IDENT` 均表示 `identifier_token`；Raw Identifier 解码后仍是普通符号名；
- `r#keyword` 可以引用名为关键字的外部 Schema 符号；
- 新代码不应主动创建 raw identifier；
- `r#` 后跟 Identifier Start 时词法化为 Raw Identifier；`r` 后跟 `"` 或 `#...#"` 时词法化为 Raw String，两者使用最长合法匹配；
- 模块、值和属性推荐 `snake_case`；
- 类型、Component、System、Trait 推荐 `UpperCamelCase`；
- 常量推荐 `UPPER_SNAKE_CASE`。

---

## 12. 保留字清单

### 12.1 核心声明关键字

```text
import export as
component system record enum trait impl type
implements where for
input state computed event slot const
fn action effect task resource view
style theme template part
native requires capability
```

### 12.2 控制流和行为关键字

```text
let mut return break continue
if else match while loop in
on capture bubble emit
transaction start await move
when run cleanup
success error cancelled
```

### 12.3 View 和资源关键字

```text
node fill bind using use
override replace preserve key
load policy scope
```

### 12.4 Shader 关键字

```text
shader vertex fragment compute
uniform instance varying texture sampler
```

### 12.5 类型和字面量关键字

```text
true false None
self Self dyn
```

### 12.6 预留但当前禁止使用

以下单词被保留以便迁移诊断或未来扩展，当前语法不会生成对应 AST：

```text
child store merge extend inherit class
macro unsafe extern static yield try
```

`child` 被保留是为了让编译器输出明确迁移提示，而不是把它当作普通组件名。

### 12.7 上下文词、标准库符号与后缀

以下不是全局保留字：

- `viso`：只在 `` 的固定位置被 Parser 识别；其他位置可以通过 Raw Identifier 引用同名外部符号，但不推荐；
- `empty`：只在 `slot ... = empty;` 的 Slot Default 上下文中具有特殊含义；
- `Bool`、`I64`、`F32`、`String` 等是 Prelude 类型符号，不是 Lexer Keyword；
- `EffectRun::mount`、`ResourcePolicy::keep_latest` 等是普通限定路径；
- `i32`、`f32`、`ms`、`dp` 等在紧邻数字时由 Numeric Literal Lexer 识别为后缀，不作为独立 Identifier Token。

关键字大小写敏感；例如 `State` 是普通标识符，`state` 是关键字。库不得通过 Schema 把普通标识符升级为新关键字。

---

## 13. 分隔符和操作符 Token

分隔符：

```text
( ) { } [ ] , ; : :: @
```

操作符：

```text
= += -= *= /= %= &= |= ^= <<= >>=
+ - * / %
! ~
& | ^ << >>
== != < <= > >=
&& || ??
. ?. ?
.. ..=
-> =>
<=>
```

注意：

- `=>` 只用于 `match` arm；禁止用于 Event Handler；
- `<=>` 只用于显式双向绑定；
- `:` 用于类型标注、命名节点的类型分隔、Record 字段、Named Argument，以及 View/Style Property Binding；
- `::` 只用于路径；
- `=` 用于行为域赋值、定义初始化和配置项；View/Style 的单向 Property Binding 使用 `:`；
- 不存在 `:=`、`+:`、`<:`、`>:`、`^:`。

---

## 14. 分号规则

简单声明和简单语句必须使用分号：

```viso
state count = 0;
count += 1;
let label = format("Count: {}", count);
emit changed(count);
```

以下以 block 结束的构造不使用分号：

```viso
action increment() { ... }
if condition { ... }
while condition { ... }
Text { ... }
on click { ... }
```

Block 的最后一个无分号表达式是 Tail Expression，仅允许在函数、Action、Task、闭包和普通表达式 Block 中出现；`view` 和 Node Body 不存在 Tail Expression。

---

## 15. 整数字面量

```ebnf
integer_literal = decimal_integer
                | hex_integer
                | octal_integer
                | binary_integer ;

decimal_integer = decimal_digit, { decimal_digit | "_" }, [ integer_suffix ] ;
hex_integer     = "0x", hex_digit, { hex_digit | "_" }, [ integer_suffix ] ;
octal_integer   = "0o", octal_digit, { octal_digit | "_" }, [ integer_suffix ] ;
binary_integer  = "0b", binary_digit, { binary_digit | "_" }, [ integer_suffix ] ;

integer_suffix  = "i8" | "i16" | "i32" | "i64"
                | "u8" | "u16" | "u32" | "u64" ;
```

规则：

- `_` 不得出现在前缀后第一位、末尾或连续出现；
- 无后缀整数是“未定型整数常量”；
- 由上下文确定类型；
- Host 域无上下文时默认 `I64`；
- Shader 域无上下文时默认 `I32`；
- 常量超出目标类型范围是编译错误，不允许截断。

---

## 16. 浮点字面量

```ebnf
float_literal   = decimal_float, [ float_suffix ] ;

decimal_float  = decimal_digits, ".", decimal_digits, [ exponent ]
               | decimal_digits, exponent ;

exponent        = ( "e" | "E" ), [ "+" | "-" ], decimal_digits ;
float_suffix    = "f32" | "f64" ;
decimal_digits  = decimal_digit,
                  { decimal_digit | ( "_", decimal_digit ) } ;
```

规范决定：

- 数字中的 `_` 只能出现在两个同进制数字之间；禁止前导、尾随或连续 `_`；
- 正负号始终是 Unary Operator，不属于 Numeric Token；
- 十六进制浮点、`.5` 和 `1.` 不属于 Viso 1.0；必须写 `0.5` 和 `1.0`；这使 `1..2` 永远词法化为 Integer + Range；
- 数值后缀必须紧邻数字，后缀前不得有空白；
- 语言中不存在 `Float` 类型；
- 无后缀浮点是“未定型浮点常量”；
- Host 域无上下文时默认 `F64`；
- Shader 域无上下文时默认 `F32`；
- `F64 -> F32` 不存在隐式运行时转换；
- 未定型字面量若能精确或按 IEEE 754 正常舍入到上下文要求的 `F32`，可以直接实例化为 `F32`；这不是 `F64 -> F32` 转换；
- `NaN` 和 `Infinity` 不使用特殊字面量，由标准库常量提供。

示例：

```viso
let host_default = 1.25;       // F64
let shader_value: F32 = 1.25; // 字面量直接定型为 F32
let explicit = 1.25f32;
```

---

## 17. 字符串与字符字面量

普通字符串：

```viso
"hello"
"line 1\nline 2"
"unicode: \u{1F680}"
```

支持的 Escape：

```text
\\  \"  \'  \n  \r  \t  \0
\xNN
\u{H...H}
```

Raw String 使用 Rust 风格：

```viso
r"no escapes"
r#"contains \"quotes\""#
r##"arbitrary # count"##
```

字符字面量：

```viso
'a'
'\n'
'🚀'
```

规则：

- 普通字符串和字符字面量不能跨物理行；换行必须写 `\n`；
- `\xNN` 恰好包含两个十六进制数字，并且只允许编码 U+0000 至 U+007F；其他字符使用 `\u{...}`；
- `\u{H...H}` 包含 1 至 6 个十六进制数字，值不得超过 U+10FFFF，也不得位于代理项范围 U+D800 至 U+DFFF；
- Raw String 的 `#` 数量范围是 0 至 255，结束分隔符必须使用完全相同数量的 `#`；
- `Char` 在 Escape 解码后必须恰好包含一个 Unicode Scalar Value；
- 不提供隐式字符串插值；
- 插值统一使用 `format(...)` 或类型安全模板 API；
- 未闭合字符串是词法错误，但 Streaming Editor Parser 可以产生 `MissingToken` CST Node 继续诊断。

---

## 18. 颜色字面量

合法形式：

```text
#RGB
#RGBA
#RRGGBB
#RRGGBBAA
```

语义：

- 顺序固定为 RGBA；
- 缺失 Alpha 时 Alpha = `FF`；
- 颜色值被解释为 sRGB 编码的 `Color`；
- 线性颜色必须显式转换：`color.to_linear()`；
- 颜色字面量不接受 `#x...` 形式；
- 非十六进制字符立即报错。

示例：

```viso
const BRAND: Color = #2ecc71;
const OVERLAY: Color = #00000080;
```

---

## 19. 单位字面量

### 19.1 完整合法后缀

| 量纲         | 后缀   | 类型        |
| ------------ | ------ | ----------- |
| 逻辑长度     | `dp`   | `Dp`        |
| 设备像素     | `px`   | `Px`        |
| 字体缩放长度 | `sp`   | `Sp`        |
| 百分比       | `%`    | `Percent`   |
| 时间         | `ns`   | `Duration`  |
| 时间         | `us`   | `Duration`  |
| 时间         | `ms`   | `Duration`  |
| 时间         | `s`    | `Duration`  |
| 时间         | `min`  | `Duration`  |
| 角度         | `deg`  | `Angle`     |
| 角度         | `rad`  | `Angle`     |
| 角度         | `turn` | `Angle`     |
| 频率         | `hz`   | `Frequency` |
| 频率         | `khz`  | `Frequency` |

### 19.2 词法形式

```ebnf
unit_literal      = unit_numeric_body, unit_suffix
                  | unit_numeric_body, "%" ;

unit_numeric_body = decimal_digits,
                    [ ".", decimal_digits ] ;

unit_suffix       = "dp" | "px" | "sp"
                  | "ns" | "us" | "ms" | "s" | "min"
                  | "deg" | "rad" | "turn"
                  | "hz" | "khz" ;
```

`numeric_body` 与后缀之间不得出现空白。

```viso
14sp
16dp
1px
50%
250ms
5min
90deg
60hz
```

`50 % 3` 是取模；`50%` 是 Percent Literal。为避免 `50%3` 的歧义，`%` 只有在紧邻数字且其后是 Trivia、分隔符、运算符或文件结束时才成为 Percent 后缀；其后紧跟数字或标识符继续字符时，`%` 是取模运算符。因此 `50%3` 与 `50 % 3` 都解析为取模。

### 19.3 量纲规则

允许：

```viso
let total: Duration = 1s + 250ms;
let half_turn: Angle = 180deg;
```

禁止：

```viso
let x = 10dp + 5px;
let y = 14sp + 2dp;
let z = 50% + 1dp;
```

跨量纲转换必须显式且由具备上下文的 API 完成：

```viso
let px: Px = layout.dp_to_px(10dp);
```

单位后缀集合是封闭的。插件、Widget Schema 和 Native 模块不得注册新的词法后缀；自定义量纲必须使用普通构造函数或 Newtype，例如 `Meters::new(12.5f64)`。这避免 Lexer 插件化、后缀冲突和 AI 猜测单位。

---

## 20. 布尔、Option 和 Result 字面量

```text
true
false
None
```

`Option::Some(expression)`、`Result::Ok(expression)` 和 `Result::Err(expression)` 是普通 Enum Variant Constructor，不是特殊字面量或保留字。

不存在：

```text
null
undefined
nil
```

`None` 的类型必须从上下文推断；无上下文时产生类型推断错误。

---

# 第四部分：形式文法记号

## 21. EBNF 记号

本文 EBNF 使用：

```text
"text"     终结符
name       非终结符
A , B      顺序
A | B      选择
[ A ]      可选
{ A }      重复零次或多次
( A )      分组
```

词法分析完成后，Parser 消费 Token，而不是字符流。Keyword Token 在 Lexer 阶段从普通 Identifier 中区分。

---

# 第五部分：完整语法——模块和声明

## 22. Compilation Unit

普通 `.vs` package source 不需要在每个文件重复声明语言版本或 module path。语言版本来自 `Viso.toml`/lockfile，模块身份来自 package root、source root 与文件路径。

```ebnf
compilation_unit = { import_decl },
                   { top_level_decl },
                   EOF ;

module_path      = identifier, { "::", identifier } ;
```

规则：

- 一个 `.vs` 文件属于一个由编译上下文确定的 Module；
- 一个 Module 可以由多个文件组成，但必须由 Manifest/source-root 规则确定；
- Import Resolution 不依赖运行时注册顺序；
- 编译器、Formatter、LSP 和 Hot Reload 都必须从同一 Source Context 获得 package/module identity；
- 独立 conformance fixture 若需要显式 module identity，应由测试 harness 提供，不扩展普通 source grammar。

### 22.1 三个规范入口：`.vs` 文件、`ui!` 与 `component!`

Viso DSL 有且仅有三个规范 Parser 入口。三者共享**完全相同**的 lexer、name resolution、type/effect/capability checker、Typed HIR、Reactive IR、UI IR、Shader IR、diagnostics 与 runtime 语义；差异只在顶层 production，不在语言语义。

```ebnf
(* 外部 .vs 源文件入口：view!("path.vs") 或 package source *)
compilation_unit = { import_decl }, { top_level_decl }, EOF ;

(* Rust 宿主 view 片段入口：ui! { ... } 的 body *)
view_fragment    = { view_item }, EOF ;

(* Rust 宿主组件入口：component! { ... } 的 body *)
component_entry  = { import_decl }, component_decl, EOF ;
```

入口与 Rust 宿主宏的对应关系：

```text
ui!          -> view_fragment      片段，无 component 外壳
component!   -> component_entry     单个组件声明
view!(path)  -> compilation_unit    外部 .vs 文件
```

规则：

- `ui!` / `component!` 是构建期 proc-macro / compiler frontend，不是运行时宏；它们不得在每帧展开或 rebuild UI；
- 三个入口产出的 Typed HIR 及后续 IR 必须与等价的 `.vs` 写法完全一致；同一段 view 无论来自 `ui!` 还是 `.vs`，lowering 结果必须相同；
- **根节点规则**：`view_fragment` 复用第 48 节的 `view_item`，因此文法层允许零个或多个根项；当它被用作某个 Component 的 `view` 主体或被要求产出单一挂载点时，仍受"View 必须产生恰好一个根 Node，多根需显式 `Fragment`"约束（第 48 节），该约束在 lowering / 宿主挂载点检查，而非 `view_fragment` 文法层；
- `component_entry` 允许在组件前写 `import`，语义与 `.vs` 文件顶层 `import_decl` 一致；
- Release 中所有入口都必须生成相同的 compact AOT descriptor/IR，不在启动时 parse `.vs`，也不在 runtime 解析 Rust source；
- 该三入口约定同时是架构文档 §0.2 第 8 条硬决策与 ADR-015 的形式化对应。

---

## 23. Import

```ebnf
import_decl      = "import", import_source, ";" ;

import_source    = module_path, [ import_suffix ] ;

import_suffix    = "as", identifier
                 | "::", "{", import_item,
                   { ",", import_item }, [ "," ], "}" ;

import_item      = identifier, [ "as", identifier ] ;
```

示例：

```viso
import viso::widgets::{Window, Column, Text, Button};
import app::model::User as AppUser;
```

规则：

- 不支持隐式 Prelude 以外的 wildcard import；
- `::*` 不属于 Viso 1.0 语法；
- Import Resolution 不依赖运行时注册顺序；
- Import Cycle 中只有纯类型边可以被允许；值初始化环必须报错。

---

## 24. Attribute

```ebnf
attribute         = "@", path, [ "(", [ attribute_args ], ")" ] ;
attribute_args    = attribute_arg, { ",", attribute_arg }, [ "," ] ;
attribute_arg     = expression
                  | identifier, ":", expression ;
```

标准 Attribute：

```viso
@stable("app.counter.add_button")
@derive(Eq, Hash, StableKey)
@deprecated(message: "Use NewButton")
@capability("network.http")
```

规则：

- Attribute 必须由 Compiler 或已导入 Schema 注册；
- 未知 Attribute 是错误，不是静默忽略的 Annotation；
- `@stable` 参数必须是编译期字符串常量；
- Attribute 不得改变基础 Tokenization 或运算符优先级。

---

## 25. Top-level Declaration

```ebnf
top_level_decl    = { attribute }, [ "export" ], declaration_core ;

declaration_core = component_decl
                 | system_decl
                 | record_decl
                 | enum_decl
                 | trait_decl
                 | impl_decl
                 | type_alias_decl
                 | const_decl
                 | function_decl
                 | action_decl
                 | task_decl
                 | template_decl
                 | style_decl
                 | theme_decl
                 | shader_decl
                 | native_decl ;
```

`export` 只影响 Module 可见性，不改变运行时生命周期。

### 25.1 Surface maturity

Viso 1.0 Draft 按实现与学习优先级划分 authoring surface：

```text
Core      component/input/state/computed/action/view/record/enum/system/basic fn
          node/property/event/if/match/keyed for/shader profile entry

Standard  effect/task/resource/slot/style/theme/hot-reload migration

Advanced  user-defined trait/impl/general generics/const generics/dyn trait
          template/part/handwritten native interface declarations
```

`Advanced` 可以存在于规范中，但不能成为 UI、Reactive、Game、Shader vertical slice 的前置条件。Rust Native Schema 是默认扩展路径。

---

## 26. Path、Generic 和 Where

```ebnf
path               = identifier, { "::", identifier } ;

type_path          = type_path_segment, { "::", type_path_segment } ;
type_path_segment  = identifier, [ generic_args ] ;

generic_args      = "<", generic_arg,
                    { ",", generic_arg }, [ "," ], ">" ;

generic_arg       = type | const_generic_arg ;
const_generic_arg = "const", const_expression ;

generic_params    = "<", generic_param,
                    { ",", generic_param }, [ "," ], ">" ;

generic_param     = type_generic_param | const_generic_param ;

type_generic_param = identifier,
                     [ ":", trait_bounds ],
                     [ "=", type ] ;

const_generic_param = "const", identifier, ":", type,
                      [ "=", const_expression ] ;

trait_bounds      = type_path, { "+", type_path } ;

implements_clause = "implements", type_path,
                    { "+", type_path } ;

where_clause      = "where", where_predicate,
                    { ",", where_predicate }, [ "," ] ;

where_predicate   = type, ":", trait_bounds ;
```

示例：

```viso
export component KeyedList<T, K>
implements Accessible
where
    T: Clone,
    K: StableKey + Clone,
{
    // ...
}
```

表达式与类型路径的消歧规则：

- Value `path` 本身不携带 `<...>`；
- Type Position 使用 `type_path`，允许 `List<Item>`；
- 表达式中的显式泛型调用必须使用 Turbofish：`decode::<User>(bytes)`；
- `foo<T>(x)` 在表达式中不会被解释成泛型调用，而按比较运算相关 Token 解析并最终产生诊断；
- Parser 仅在 Postfix Call 前看到 `::<...>` 时进入 Generic Call Argument Grammar；
- Const Generic 实参必须显式加 `const`，例如 `Matrix<F32, const 4, const 4>`；这避免单段 Path 究竟是 Type 还是 Const 的歧义；
- 这一规则保证 `<`、`>` 的比较语义以及 Type/Const Generic 分类都无需依赖符号表回馈给 Parser。

---

## 27. Type Grammar

```ebnf
type               = function_type
                   | tuple_type
                   | array_type
                   | slice_type
                   | trait_object_type
                   | "Self"
                   | type_path ;

function_type      = ( "Fn" | "FnMut" | "ActionFn" | "TaskFn" ),
                     "(", [ type_list ], ")",
                     "->", type ;

tuple_type         = "(", type, ",",
                     [ type, { ",", type }, [ "," ] ], ")" ;

array_type         = "[", type, ";", const_expression, "]" ;
slice_type         = "[", type, "]" ;
trait_object_type  = "dyn", trait_bounds ;

type_list          = type, { ",", type }, [ "," ] ;
```

内建泛型容器使用普通 Type Path：

```text
Option<T>
Result<T, E>
List<T>
Map<K, V>
Set<T>
Handle<T>
WeakHandle<T>
Resource<T, E>
NodeRef<T>
```

---

## 28. Record

```ebnf
record_decl        = "record", identifier,
                     [ generic_params ],
                     [ implements_clause ],
                     [ where_clause ],
                     "{", { record_field }, "}" ;

record_field       = { attribute }, identifier, ":", type,
                     [ "=", const_expression ], ";" ;
```

示例：

```viso
@derive(Eq, Hash, StableKey)
export record TodoId {
    value: U64;
}

export record User {
    id: TodoId;
    name: String;
    avatar: Option<Url> = None;
}
```

Record 是名义类型，不是开放字典。未知字段是编译错误。

---

## 29. Enum

```ebnf
enum_decl          = "enum", identifier,
                     [ generic_params ],
                     [ implements_clause ],
                     [ where_clause ],
                     "{", { enum_variant }, "}" ;

enum_variant       = { attribute }, identifier,
                     [ tuple_variant | record_variant ], ";" ;

tuple_variant      = "(", [ type_list ], ")" ;
record_variant     = "{", { record_field }, "}" ;
```

示例：

```viso
export enum LoadState<T, E> {
    idle;
    loading;
    ready(T);
    failed(E);
}
```

Enum Variant 使用 Path：

```viso
LoadState::loading
LoadState::ready(user)
```

---

## 30. Trait

```ebnf
trait_decl         = "trait", identifier,
                     [ generic_params ],
                     [ ":", trait_bounds ],
                     [ where_clause ],
                     "{", { trait_member }, "}" ;

trait_member       = { attribute },
                     ( function_signature, ";"
                     | action_signature, ";"
                     | task_signature, ";"
                     | associated_type_decl
                     | associated_const_decl ) ;

associated_type_decl = "type", identifier,
                       [ ":", trait_bounds ], ";" ;

associated_const_decl = "const", identifier, ":", type, ";" ;
```

示例：

```viso
export trait FixedUpdate {
    action fixed_update(frame: FixedFrame);
}

export trait StableKey: Eq + Hash + Clone {
    fn stable_hash(self) -> U64;
}
```

---

## 31. Impl

```ebnf
impl_decl          = "impl", [ generic_params ],
                     impl_target,
                     [ where_clause ],
                     "{", { impl_member }, "}" ;

impl_target        = type_path, "for", type
                   | type ;

impl_member        = { attribute },
                     ( function_decl
                     | action_decl
                     | task_decl
                     | associated_type_impl
                     | associated_const_impl ) ;

associated_type_impl = "type", identifier, "=", type, ";" ;
associated_const_impl = "const", identifier, ":", type,
                        "=", const_expression, ";" ;
```

`impl Trait for Type` 是 Trait 实现；`impl Type` 是 Inherent Impl。

---

## 32. Type Alias 和 Const

```ebnf
type_alias_decl    = "type", identifier,
                     [ generic_params ],
                     "=", type, ";" ;

const_decl         = "const", identifier, ":", type,
                     "=", const_expression, ";" ;
```

`const_expression` 是可在编译期求值的 Expression 子集，禁止 I/O、State、Native Action、Task、Resource 和非确定性调用。

---

# 第六部分：完整语法——可调用项、Component 与 System

## 33. Parameter、返回类型与 Capability

```ebnf
parameter_list      = [ parameter, { ",", parameter }, [ "," ] ] ;

parameter           = [ "mut" ], identifier, ":", type,
                      [ "=", default_expression ] ;

return_type         = [ "->", type ] ;

capability_clause   = "requires", "{",
                      capability_path,
                      { ",", capability_path }, [ "," ],
                      "}" ;

capability_path     = module_path ;
```

规则：

- Public、Trait、Native 和 Component 接口参数必须显式写类型；
- 默认参数只允许出现在普通 `fn`、`action` 和 `task` 的尾部；
- Trait 方法与 Native 声明禁止默认参数；
- `default_expression` 必须是纯、确定且可在调用点类型检查的表达式；
- Capability 是编译期集合，不是普通字符串；
- 一个调用点所需的 Capability 集合是被调用项声明集合的并集；
- 调用者没有所需 Capability 时必须产生静态错误；
- 动态加载模块还必须在运行时再次进行 Capability 检查。
- Private callable 的 Capability 集合默认由 Typed Call Graph 推导；显式 `requires { ... }` 用作公开安全合同或上界断言，不要求每个私有函数重复书写。

示例：

```viso
task fetch_user(id: UserId) -> Result<User, NetError>
    requires { network::http } {
    return await Http::get_json(format("/users/{}", id));
}
```

---

## 34. 普通函数 `fn`

```ebnf
function_decl       = "fn", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ],
                      block ;

function_signature  = "fn", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ] ;
```

`fn` 的静态语义：

1. 默认是纯函数；
2. 可以读取参数、常量、Input、State、Computed 和不可变 Native Query；
3. 禁止修改 State；
4. 禁止 `emit`；
5. 禁止调用 `action`；
6. 禁止直接启动 Task；
7. 禁止执行未标记为纯的 Native 调用；
8. 可以递归，但受静态递归检查和运行时深度预算限制；
9. 被 `view` 或 `computed` 调用时，其响应式读取会内联计入调用者依赖集。

实现可以提供：

```viso
@const
fn clamp01(value: F32) -> F32 {
    return value.clamp(0.0f32, 1.0f32);
}
```

`@const` 要求函数可在编译期解释，禁止读取 Component 实例状态。

---

## 35. Action

```ebnf
action_decl         = "action", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ],
                      block ;

action_signature    = "action", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ] ;
```

Action 是同步、有界、可修改状态的行为单元。

规则：

- 每次 Action 调用自动开启一个 State Transaction；
- 嵌套 Action 复用最外层 Transaction；
- Action 正常返回时提交；
- Action 抛出未处理错误或触发运行时故障时回滚本次 Transaction；
- 提交后统一计算 Computed、Effect 调度和 UI 失效；
- Action 中禁止 `await`；
- Action 可以 `emit` Typed Event；
- Action 可以调用普通 `fn`、其他 Action 和同步 Native Action；
- Action 可以通过 `start` 启动 Task，但不会等待其完成；
- Action 的返回值不应用作跨线程可变引用。

```viso
action increment(by: I64 = 1) {
    count += by;
    emit changed(count);
}
```

---

## 36. Task

```ebnf
task_decl           = "task", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ],
                      block ;

task_signature      = "task", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      return_type,
                      [ where_clause ],
                      [ capability_clause ] ;
```

Task 是可挂起的结构化异步计算。

规范：

- `await` 只允许出现在 Task、Task Closure 和 Resource Loader 中；
- Task 启动时捕获 Input、State 和参数的不可变快照；
- Task 在挂起后禁止直接访问可变 Component State；
- Task 通过返回值把结果交还 UI Actor；
- Component 销毁、所属 Key 变化或热重载迁移失败时，Task 自动收到取消信号；
- Task 必须在每个可挂起 Native 调用处检查取消；
- 未处理取消不是 Error；
- 非取消 Error 由 `Result` 或 Start Handler 明确处理；
- Task 的默认执行器由 Profile 指定，但 State Commit 始终回到 UI Actor。

示例：

```viso
task load_profile(id: UserId) -> Result<UserProfile, LoadError>
    requires { network::http } {
    let response = await Api::profile(id);
    return response.decode();
}
```

---

## 37. Effect

```ebnf
effect_decl          = "effect", identifier,
                       [ effect_dependencies ],
                       [ "run", effect_run_policy ],
                       "{",
                       { statement },
                       [ cleanup_clause ],
                       "}" ;

effect_dependencies  = "when", "(",
                       expression, { ",", expression }, [ "," ],
                       ")" ;

effect_run_policy    = path ;

cleanup_clause        = "cleanup", block ;
```

标准 `EffectRun` 值：

```text
EffectRun::mount
EffectRun::change
EffectRun::mount_and_change
```

默认规则被固定为：

- 没有 `when` 时，默认是 `EffectRun::mount`；此时显式策略也只能是 `EffectRun::mount`；
- 存在非空 `when(...)` 时，默认是 `EffectRun::mount_and_change`；
- `EffectRun::change` 和 `EffectRun::mount_and_change` 必须带非空 `when(...)`；
- `EffectRun::mount` 禁止同时带 `when(...)`；
- `when()` 空依赖列表在语法层即不合法。

示例：

```viso
effect persist_theme when (theme_name) run EffectRun::change {
    Settings::set_string("theme", theme_name);

    cleanup {
        Settings::flush();
    }
}
```

规范：

- Effect 在 UI Transaction 提交后执行；
- `when` 中的每个表达式必须是纯表达式；
- Effect 依赖集合由 `when` 显式给出，不通过隐式全局追踪猜测；
- 编译器必须诊断 Effect Body 中读取但未列出的 Reactive Value，除非读取位于 `untracked(...)`；
- Effect Body 不能直接使用赋值语句修改 State；
- 若确实需要修改 State，必须显式使用 `transaction { ... }`；
- Cleanup 在下次 Effect 重跑前、Component 销毁前或热重载替换前恰好执行一次；
- Cleanup 禁止启动新的长生命周期 Task；
- Effect Cycle 必须被检测并报告依赖链。

`EffectRun` 是标准库 Enum，不是 Parser 特例；Parser 只解析 Path。

---

## 38. Resource

### 38.1 唯一规范语法

```ebnf
resource_decl        = "resource", identifier, ":", type,
                       "{", { resource_item }, "}" ;

resource_item        = "load", "=", expression, ";"
                     | "key", "=", expression, ";"
                     | "policy", "=", policy_list, ";"
                     | "scope", "=", expression, ";" ;

policy_list          = "[",
                       [ expression,
                         { ",", expression }, [ "," ] ],
                       "]" ;
```

示例：

```viso
resource search_result: Resource<List<SearchItem>, SearchError> {
    load = SearchApi::query(query);
    key = query;
    policy = [
        ResourcePolicy::debounce(250ms),
        ResourcePolicy::keep_latest,
        ResourcePolicy::cache_for(5min),
    ];
    scope = ResourceScope::component;
}
```

### 38.2 配置约束

- `load` 必须且只能出现一次；
- `key` 必须且只能出现一次；
- `policy` 可以省略，默认 `[]`；
- `scope` 可以省略，默认 `ResourceScope::component`；
- Item 的源码顺序不影响语义；
- 重复 Item 是静态错误；
- 未知 Item 是静态错误；
- 不存在单行 `key ... policy ...` 语法；
- 不存在使用逗号分隔的隐式子句语法；
- `policy` 的组合合法性由 `ResourcePolicy` Schema 检查；
- `key` 类型必须实现 `StableKey`；
- `load` 必须产生可取消的异步结果，其成功和错误类型必须与 `Resource<T, E>` 一致。

### 38.3 Resource 状态

```viso
match search_result.state {
    ResourceState::idle => { EmptyView {} }
    ResourceState::loading => { Spinner {} }
    ResourceState::ready(items) => { Results { items = items; } }
    ResourceState::error(error) => { ErrorView { error = error; } }
    ResourceState::reloading(items) => { Results { items = items; dimmed = true; } }
}
```

Resource 状态是标准 Enum；语法没有硬编码上述成员。

---

## 39. Start Statement 与 Task 生命周期

```ebnf
start_statement      = "start", call_expression,
                       [ "as", identifier ],
                       [ start_handler_block ], ";" ;

start_handler_block  = "{", { start_handler }, "}" ;

start_handler        = "policy", "=", policy_list, ";"
                     | "success", "(", pattern, ")", block
                     | "error", "(", pattern, ")", block
                     | "cancelled", block ;
```

规范写法：

```viso
start save_profile(profile) as save_job {
    policy = [TaskPolicy::keep_latest];

    success(saved) {
        current = saved;
    }

    error(reason) {
        last_error = Option::Some(reason);
    }

    cancelled {
        log::debug("save cancelled");
    }
};
```

注意：

- 整个 `start` 是一条语句，因此末尾必须有 `;`；
- `success`、`error` 和 `cancelled` 是 Handler，不是普通函数声明；
- Handler 在 UI Actor 上以新的 Transaction 执行；
- `as save_job` 建立当前 Component 实例范围内的 Task Slot；
- 同名 Task Slot 的策略由 `TaskPolicy` 决定；
- 未命名 Task 仍归属当前生命周期 Scope，禁止成为无主任务；
- `start` 只接受 Task Call，普通 `fn` 或 `action` Call 会报错。

---

## 40. Component 声明

```ebnf
component_decl       = "component", identifier,
                       [ generic_params ],
                       [ implements_clause ],
                       [ where_clause ],
                       "{", { component_member }, "}" ;

component_member     = { attribute },
                       ( input_decl
                       | state_decl
                       | computed_decl
                       | event_decl
                       | slot_decl
                       | const_decl
                       | function_decl
                       | action_decl
                       | task_decl
                       | effect_decl
                       | resource_decl
                       | native_member_decl
                       | view_decl ) ;
```

约束：

- 一个非抽象 Component 必须恰好声明一个 `view`；
- Component 不支持类继承；
- 复用通过 Composition、Trait、Template、Style 和 Slot 完成；
- `input`、`event` 和 `slot` 构成公开 UI 接口；
- `state`、`computed`、内部 Action 和节点默认是私有实现；
- Trait 可以要求 Component 实现 Action 或 Fn；
- 同一 Component 内的成员名称不能在同一 Namespace 冲突；
- Value Namespace、Type Namespace 和 Event Namespace 分离，但 Formatter 应避免同名造成阅读混乱。

---

## 41. Input

```ebnf
input_decl           = "input", identifier, ":", type,
                       [ "=", default_expression ], ";" ;
```

规则：

- Input 是父组件传入的只读值；
- Component 内禁止给 Input 赋值；
- Input 默认值必须是纯、确定的 Default Expression；
- Input 默认值禁止读取另一个 Input、State、Computed、Native Runtime 或当前时间；
- 没有默认值的 Input 是必填属性；
- Input 类型是 Component Schema 的一部分，改变类型属于接口兼容性变更；
- Input 变化会根据实际依赖使 Computed、View、Effect 或 Resource Key 失效。

```viso
input title: String;
input enabled: Bool = true;
```

---

## 42. State 与初始化顺序

```ebnf
state_decl           = "state", identifier,
                       [ ":", type ],
                       "=", init_expression, ";" ;
```

### 42.1 唯一初始化规则

State 按源码声明顺序初始化。

State Initializer 可以读取：

- Component Input；
- Module Const；
- 前面已经初始化完成的 State；
- 纯函数；
- 纯、确定的 Record/Enum Constructor。

State Initializer 禁止读取：

- 后面声明的 State；
- 任意 Computed；
- Resource；
- Node；
- Event；
- Effect；
- Task 状态；
- 不纯 Native API；
- 当前时间、随机数或隐式环境状态。

示例：

```viso
state min_count: I64 = 0;
state count: I64 = min_count; // 合法：只读前置 State
state doubled: I64 = count * 2; // 合法，但通常更适合 computed
```

非法：

```viso
state count: I64 = minimum; // E2104：读取后置 State
state minimum: I64 = 0;
```

**不存在“只要依赖顺序明确就允许前向引用”的例外。** 如需无关源码顺序的派生依赖，必须使用 `computed`。

### 42.2 State 所有权

- 每个 Component 实例拥有独立 State Cell；
- State 值必须满足 `StateValue`；
- 热重载迁移要求迁移前后的值满足兼容规则；
- Handle 类 State 必须有显式 Clone/Retain 语义；
- 不允许把局部借用存入 State。
- 省略 State 类型时，HIR 必须在编译期推断出唯一 concrete type，并把该类型写入 State Schema；无法唯一推断时是编译错误。

---

## 43. Computed

```ebnf
computed_decl        = "computed", identifier,
                       [ ":", type ],
                       "=", expression, ";" ;
```

规则：

- Computed 必须纯；
- 私有 Computed 可以省略类型，由 HIR 推断；
- 被 Trait 或 Schema 暴露的 Computed 必须显式写类型；
- Computed 可以引用同一 Component 中源码前方或后方的 Computed；
- 编译器对全部 Computed 建依赖图并进行拓扑排序；
- 依赖图存在环时静态报错，并输出完整环路径；
- Computed 值按需缓存；
- 依赖版本未变化时不得重复求值；
- Computed 求值失败不得部分提交依赖图。

```viso
computed subtotal: Money = items.fold(Money::zero(), |sum, item| {
    return sum + item.price;
});

computed label = format("{} items", items.length());
```

---

## 44. Event

```ebnf
event_decl           = "event", identifier,
                       "(", event_parameter_list, ")", ";" ;

event_parameter_list = [ event_parameter,
                         { ",", event_parameter }, [ "," ] ] ;

event_parameter      = identifier, ":", type ;
```

规则：

- Event 没有返回值；
- Event Payload 字段必须具名；
- Event 参数类型必须可跨 Component Boundary；
- Event 是否冒泡由 Event Schema 决定；
- 自定义 Component Event 默认不冒泡；
- `emit event_name(...)` 的实参与声明进行静态检查；
- Event Handler 不能通过返回 Bool 隐式取消事件，必须调用 Event Context 的显式 API。

```viso
event changed(value: I64);
event submitted(text: String, source: SubmitSource);
```

---

## 45. Slot

```ebnf
slot_decl            = "slot", identifier, ":", type,
                       [ "=", slot_default ], ";" ;

slot_default         = "None" | "empty" ;
```

标准 Slot 类型：

```text
Slot<Node>          恰好一个节点
OptionalSlot<Node>  零个或一个节点
SlotList<Node>      零个或多个节点
```

`empty` 是仅在 Slot Default 中合法的上下文 Token，不列为全局表达式关键字。Lexer 仍产生 Identifier，Parser 在 `slot_default` 位置解释。

```viso
slot content: Slot<Node>;
slot leading: OptionalSlot<Node> = None;
slot actions: SlotList<Node> = empty;
```

调用方使用 `fill`，而不是把 Slot 名当动态属性。

---

## 46. System 声明

```ebnf
system_decl          = "system", identifier,
                       [ generic_params ],
                       [ implements_clause ],
                       [ where_clause ],
                       "{", { system_member }, "}" ;

system_member        = { attribute },
                       ( input_decl
                       | state_decl
                       | computed_decl
                       | const_decl
                       | function_decl
                       | action_decl
                       | task_decl
                       | effect_decl
                       | resource_decl
                       | native_member_decl ) ;
```

System 与 Component 的差异：

- System 没有 `view`、`event` 和 `slot`；
- System 由 Runtime Scheduler 创建和调度；
- System 可实现 `FixedUpdate`、`FrameUpdate`、`AudioProcess` 等 Profile Trait；
- System 的 State 不属于 UI Node Tree；
- System 生命周期由 Scope/World 决定；
- 多线程 System 必须通过 Trait 和 Schema 声明线程域；
- System 不能隐式访问全局可变单例。

游戏逻辑、音频处理、后台同步、数据索引器和 Inspector Agent 都应优先使用 System，而不是把 Tick 塞进 Widget Event Handler。

---

## 47. Native 成员与顶层 Native 声明

Native 符号默认由 Rust Schema 生成；普通应用代码不手写 Native ABI 声明。手写 `.vs` Native 声明只用于接口存根、测试和受控高级场景。

```ebnf
native_decl          = "native", native_item ;

native_member_decl   = "native", native_item ;

native_item          = native_function
                     | native_action
                     | native_task
                     | native_type_decl ;

native_function      = "fn", identifier,
                       [ generic_params ],
                       "(", parameter_list, ")",
                       return_type,
                       [ where_clause ],
                       [ capability_clause ], ";" ;

native_action        = "action", identifier,
                       [ generic_params ],
                       "(", parameter_list, ")",
                       return_type,
                       [ where_clause ],
                       [ capability_clause ], ";" ;

native_task          = "task", identifier,
                       [ generic_params ],
                       "(", parameter_list, ")",
                       return_type,
                       [ where_clause ],
                       [ capability_clause ], ";" ;

native_type_decl     = "type", identifier,
                       [ generic_params ],
                       [ ":", trait_bounds ],
                       [ where_clause ], ";" ;
```

静态区分：

- `native fn`：只读、确定或由 Schema 标记可安全用于纯上下文；
- `native action`：同步副作用；
- `native task`：异步、可取消副作用；
- Native Rust Panic 必须在 Bridge 边界被捕获并转为结构化 Runtime Fault；
- Native Handle 方法也按 `fn/action/task` 分类；
- Schema 必须声明线程域、Capability、参数所有权、错误类型和 Hot Reload 可迁移性。

---

# 第七部分：完整语法——View、节点、Template、Style 与 Theme

## 48. View 声明

```ebnf
view_decl            = "view", view_block ;

view_block           = "{", { view_item }, "}" ;

view_item            = { attribute },
                       ( named_node
                       | anonymous_node
                       | part_node
                       | view_if
                       | view_for
                       | view_match
                       | template_use ) ;
```

`view_block` 顶层只接受结构性项（节点、`part`、条件/列表/`match`、`template use`）。Property Binding、Two-way Binding、Event Handler、`fill`、Part Override/Replace 只能出现在 Node Body 内部（见 [§49](#49-子节点语法删除-child) 的 `node_item`），不能直接写在 `view {}` 根层。此约束与权威附录 [§A.8](#a8-view-和节点) 的 `ViewStructureItem` / `NodeMember` 拆分完全一致。

规则：

- 每个 Component 必须有且仅有一个 View；
- View 必须生成恰好一个根 Node；
- 多个根节点必须显式包裹 `Fragment`；
- View 是纯执行域；
- View 中禁止普通 `let`、赋值、`return`、`emit`、`start`、I/O 和 Native Action；
- View 可以读取 Input、State、Computed 和 Resource State；
- View 可以调用纯 `fn`；
- View 构建产生 UI IR，不直接执行 OS/GPU 副作用。

---

## 49. 子节点语法：删除 `child`

```ebnf
named_node           = "node", identifier, ":", component_type,
                       node_body ;

anonymous_node       = component_type, node_body ;

component_type       = type_path ;

node_body            = "{", { node_item }, "}" ;

node_item            = { attribute },
                       ( property_binding
                       | two_way_binding
                       | event_handler
                       | fill_clause
                       | named_node
                       | anonymous_node
                       | part_node
                       | view_if
                       | view_for
                       | view_match
                       | template_use
                       | part_override
                       | part_replace ) ;
```

Node Body 是 `view_item` 之外唯一允许 Property Binding、Two-way Binding、Event Handler、`fill`、Part Override/Replace 的位置；它同时允许全部结构性子项（嵌套节点、`part`、条件/列表/`match`、`template use`）。该产生式与权威附录 [§A.8](#a8-view-和节点) 的 `NodeMember` 逐项对应。

唯一规则：

```viso
node root: Window {
    Column {
        Text {
            text: "Hello";
        }
    }
}
```

解释：

- `node root: Window` 创建具有显式局部名称和稳定身份种子的节点；
- `Column` 和 `Text` 是匿名子节点；
- 语言不存在 `child Column {}`；
- `child` 是保留的迁移错误词，出现时诊断 `E3001`；
- 匿名节点仍具有编译器生成的 Structural Node ID，但源码插入兄弟节点可能改变它；
- 需要跨热重载可靠保留局部状态、焦点或动画的节点应该使用 `node` 或 `@stable(...)`。

### 49.1 Named Node 可见性

- Named Node 只在当前 Component 实现中可见；
- Named Node 不是公开字段；
- 行为代码通过生成的 `NodeRef<T>` 查询它；
- View 纯度规则禁止在 View 外直接修改节点属性作为状态源；
- 命令式焦点、滚动、测量等操作通过受控 Node Action 完成。

---

## 50. Property Binding

```ebnf
property_binding     = property_path, ":", expression, ";" ;

property_path        = identifier, { ".", identifier } ;
```

在 Node Body 中：

```viso
Text {
    text: label;
    layout.width: 120dp;
    color: theme.colors.foreground;
}
```

语义：

- 这是单向 Reactive Binding，不是一次性命令式赋值；
- 右侧必须是纯表达式；
- 左侧必须由 Component Schema 暴露；
- 编译器检查类型和 Property Mutability；
- Property Schema 必须声明失效类别：`layout`、`paint`、`semantics`、`input` 或其组合；
- Binding 依赖变化时只触发所需失效；
- 同一 Property 在同一 Node Body 中绑定多次是错误；
- Style 与显式 Property 的优先级在第 76 节定义。

---

## 51. Two-way Binding

```ebnf
two_way_binding     = "bind", property_path, "<=>", assignable_path,
                      [ "using", type_path ], ";" ;

assignable_path      = identifier, { ".", identifier | index_selector } ;
index_selector       = "[", expression, "]" ;
```

```viso
TextInput {
    bind value <=> draft;
}
```

带转换器：

```viso
Slider {
    bind value <=> settings.volume using PercentUnitConverter;
}
```

规则：

- 左侧 Property 必须在 Schema 中声明 `two_way`；
- 右侧必须是可赋值的 State Lens；
- 右侧不能是 Input、Computed、Const、Resource Payload 临时值或普通函数返回值；
- 无 Converter 时两边类型必须相同；
- Converter 必须实现 `TwoWayConverter<Model, View>`；
- 更新必须带 Origin Token，禁止形成回声循环；
- 同一 Property 不能同时使用 `=` 和 `bind`；
- `<=>` 在语言其他位置非法。

---

## 52. Event Handler：唯一 Block 形式

```ebnf
event_handler       = "on", [ event_phase ], event_name,
                      [ "(", pattern, ")" ],
                      block ;

event_phase         = "capture" | "bubble" ;

event_name          = identifier ;
```

规范形式：

```viso
Button {
    on click {
        increment();
    }
}

Canvas {
    on pointer_down(event) {
        if event.button == PointerButton::primary {
            begin_drag(event.position);
        }
    }
}
```

规则：

- 不存在 `on click => increment()`；
- 即使只有一条语句也必须写 Block；
- Handler 的可选 Pattern 绑定整个 Event Payload；
- 忽略 Payload 时省略括号；
- Handler 自动运行在 Action Transaction 中；
- 默认 Phase 由 Event Schema 定义，通常为 Target/Bubble；
- `capture` 和 `bubble` 显式覆盖默认 Phase；
- Event Payload 字段由 Schema 静态检查；
- Handler 中允许同步 State 修改、`emit`、Action Call 和 `start`；
- Handler 中禁止直接 `await`；
- Event 取消使用 `event.stop_propagation()` 和 `event.prevent_default()` 等 Typed API。

`=>` 仅保留给 `match` Arm，因此 Parser 和 Formatter 不会把事件写法分叉成两套。

---

## 53. Fill Slot

```ebnf
fill_clause          = "fill", identifier, view_block ;
```

```viso
Dialog {
    title: "Delete item";

    fill content {
        Text { text: "This cannot be undone."; }
    }

    fill actions {
        Button { text: "Cancel"; }
        Button { text: "Delete"; }
    }
}
```

规则：

- Slot 名必须存在于目标 Component Schema；
- `Slot<Node>` 必须恰好生成一个节点；
- `OptionalSlot<Node>` 生成零或一个节点；
- `SlotList<Node>` 可生成任意数量节点；
- 未命名的匿名子节点只能进入 Schema 指定的 Default Slot；
- Component 没有 Default Slot 时，裸子节点是错误；
- 同一个 Single Slot 重复 `fill` 是错误。

---

## 54. Conditional View 与 `preserve`

```ebnf
view_if             = "if", head_expression,
                      [ "preserve", string_literal ],
                      view_block,
                      [ "else", ( view_if | view_block ) ] ;
```

```viso
if logged_in preserve "user-panel" {
    UserPanel { user = user; }
} else {
    LoginPanel {}
}
```

规范：

- `preserve` 后必须是编译期 String Literal；
- 它不是普通 Key Expression；
- String 在当前 Component 的 Conditional Namespace 中必须唯一；
- 不写 `preserve` 时，离开分支会销毁其 Node、State、Effect、Task 和 Resource Scope；
- 写 `preserve` 时，离开分支会把分支实例移入受限缓存；
- 缓存容量和逐出策略由 Runtime Profile 控制；
- `preserve` 不得用于无限动态值；动态集合必须使用 Keyed List；
- Branch 条件必须是 Bool；
- 各分支输出必须满足所在 Slot 的 Cardinality。

这与列表 `key expression` 是两种不同的 AST Node 和运行时语义。

---

## 55. Keyed List

```ebnf
view_for            = "for", pattern, "in", head_expression,
                      "key", head_expression,
                      view_block ;
```

```viso
for item in items key item.id {
    TodoRow {
        item: item;
    }
}
```

规则：

- `key` 必填；
- Key Expression 的类型必须实现 `StableKey`；
- Key Expression 只能读取 Loop Pattern、不可变外部值和纯函数；
- 同一帧同一列表中 Key 必须唯一；
- Runtime 发现重复 Key 必须产生结构化诊断，Debug 模式拒绝提交该 UI Patch；
- 使用索引作为 Key 只在集合长度和顺序被证明静态不变时允许；否则警告或错误；
- Key 决定 Child Component State、焦点、动画、Task 和 Resource 的迁移身份；
- 项目移动只生成 Move Patch，不销毁重建；
- Key 类型禁止为 F32/F64；
- Key 的 Hash 必须在进程和热重载版本之间稳定。

---

## 56. View Match

```ebnf
view_match          = "match", head_expression, "{",
                      view_match_arm,
                      { ",", view_match_arm }, [ "," ],
                      "}" ;

view_match_arm      = pattern, [ "if", expression ],
                      "=>", view_block ;
```

```viso
match user.state {
    UserState::loading => {
        Spinner {}
    },
    UserState::ready(user) => {
        ProfileCard { user = user; }
    },
    UserState::error(error) => {
        ErrorView { message = error.message; }
    },
}
```

规则：

- `=>` 只在 Match Arm 中合法；
- Match 必须穷尽，除非存在 `_` Arm；
- Guard 必须为纯 Bool Expression；
- 每个 Arm 的 View 输出必须满足同一 Slot Cardinality；
- Pattern Binding 的作用域仅限 Guard 和对应 View Block。

---

## 57. Part

```ebnf
part_node           = "part", identifier, ":", component_type,
                      node_body ;

part_override       = "override", "part", identifier,
                      "{", { property_binding
                            | two_way_binding
                            | event_handler }, "}" ;

part_replace        = "replace", "part", identifier,
                      view_block ;
```

语义：

- `part` 是 Template 或 Component 明确暴露的可定制内部节点；
- 普通 `node` 不可被外部 Override；
- `override part` 只能覆盖 Schema 标记为可覆盖的 Property/Event Binding；
- `replace part` 替换整个 Part 子树，必须满足 Part Contract；
- Part 名属于公开 Schema；
- `override` 与 `replace` 是不同 AST Node，不存在隐式合并；
- Runtime Hot Reload 使用 Part Stable ID 迁移状态。

---

## 58. Template

```ebnf
template_decl       = "template", identifier,
                      [ generic_params ],
                      "(", parameter_list, ")",
                      [ where_clause ],
                      "{", template_member, { template_member }, "}" ;

template_member     = slot_decl | const_decl | function_decl | view_decl ;

template_use        = "use", type_path,
                      "(", argument_list, ")",
                      [ template_use_body ], ";" ;

template_use_body   = "{", { fill_clause
                            | part_override
                            | part_replace }, "}" ;
```

示例：

```viso
export template TitledCard(title: String) {
    slot content: Slot<Node>;

    view {
        Column {
            part heading: Text { text: title; }
            SlotOutlet { slot: content; }
        }
    }
}
```

调用：

```viso
use TitledCard("Profile") {
    override part heading {
        color: theme.colors.accent;
    }

    fill content {
        ProfileBody { user = user; }
    }
};
```

规则：

- Template 没有 State、Effect、Task 或 Resource；
- Template 是编译期 UI IR 生成器；
- Template 参数按值传递；
- `use` 是 View Item，但语法上以 `;` 结束以区分调用式结构；
- Template 展开后保留 Source Origin，诊断可同时指向定义与调用点；
- Template 递归必须有可证明的有限展开，否则编译错误；
- 实现可以延迟 Template 实例化，但语义等价于 Typed IR 展开。

在 Template 定义内部，调用方 Slot 通过标准 `SlotOutlet` Component 放入结构。`fill` 只允许出现在 Template/Component 的调用方，不能用于定义 Slot Outlet。

---

## 59. Style

```ebnf
style_decl          = "style", identifier,
                      "for", component_type,
                      [ style_base_clause ],
                      "{", { style_item }, "}" ;

style_base_clause   = ":", type_path,
                      { "+", type_path } ;

style_item          = property_binding
                    | style_when ;

style_when          = "when", state_selector, "{",
                      { property_binding }, "}" ;

state_selector      = selector_or ;
selector_or         = selector_and, { "||", selector_and } ;
selector_and        = selector_unary, { "&&", selector_unary } ;
selector_unary      = [ "!" ],
                      ( identifier | "(", state_selector, ")" ) ;
```

```viso
export style PrimaryButton for Button {
    background: theme.colors.primary;
    foreground: theme.colors.on_primary;

    when hover {
        background: theme.colors.primary_hover;
    }

    when disabled {
        opacity: 0.5f32;
    }
}
```

规则：

- Style 只能绑定 Schema 标记为 Styleable 的 Property；
- Style Expression 必须纯；
- `state_selector` 名由目标 Component Schema 提供；
- Style 继承使用 `:`，仅表示 Style Base，不表示 Component 继承；
- Base Style 图必须无环；
- 冲突按 Base 顺序后应用当前 Style；
- 显式 Node Property 优先于 Style；
- Style 不得声明 Event Handler、State、Task 或任意副作用。

Style 的应用不引入新的特殊语法。目标 Component Schema 可以声明标准 Typed Property，例如：

```viso
Button {
    styles: [PrimaryButton, DenseControl];
}
```

`styles` 的精确类型由 Component Schema 定义，通常是 `List<StyleRef<Button>>`。编译器必须检查 Style 目标类型兼容性；Style 顺序按列表从左到右应用，后者覆盖前者，节点显式 Property 最后覆盖全部 Style。不存在通过字符串名称动态查找 Style 的语义。

---

## 60. Theme

```ebnf
theme_decl          = "theme", identifier,
                      [ ":", type_path ],
                      "{", { theme_item }, "}" ;

theme_item          = const_decl
                    | identifier, "=", expression, ";" ;
```

```viso
export theme AppTheme {
    colors = ColorPalette {
        primary: #4f7cff,
        foreground: #f4f6fb,
        surface: #151923,
    };

    spacing = SpacingScale {
        small: 4dp,
        medium: 8dp,
        large: 16dp,
    };
}
```

规则：

- Theme 是 Typed Immutable Record Graph；
- Theme Value 可以在运行时整体替换；
- Theme 内部成员不可局部命令式修改；
- Theme Base 图必须无环；
- Theme Expression 必须纯；
- Theme 切换通过 Reactive Context 使依赖的 Binding 失效；
- Theme 不引入动态字符串变量查找。

`theme` 是标准库定义的 Typed Reactive Context Binding，不是任意全局变量。应用根节点或测试 Harness 必须提供一个与当前 Theme Schema 匹配的 Context Value；缺失 Context 是静态配置错误或应用启动错误。组件只能读取 `theme`，Theme 切换必须通过宿主 Context API 进行原子替换。

---

# 第八部分：完整语法——Statement、Expression、Closure 与 Pattern

## 61. Block 与 Tail Expression

```ebnf
block                = "{", { statement }, [ tail_expression ], "}" ;

tail_expression      = expression ;
```

解析规则：

- 以分号结束的 Expression 是 `expression_statement`；
- Block 结束前没有分号的最后一个 Expression 是 Tail Expression；
- `view_block`、`node_body`、`style`、`theme` 和 Resource 配置 Block 不使用普通 Block Grammar，因此没有 Tail Expression；
- 返回类型为 `Unit` 的 Callable 可以省略 Tail Expression；
- 同时出现显式 `return` 和 Tail Expression 是合法的，但控制流必须通过类型检查；
- 在 Block Item 起始位置，未加括号的 `if` 和 `match` 总是由 Statement Parser 接管；
- 因此未加括号的 `if`/`match` 不会被当成 Tail Expression；要把它们作为 Tail Value，必须加括号或使用显式 `return`；
- 这一规则只解决 CST 分类，不改变 `if_expression`/`match_expression` 在赋值、参数和返回值位置的能力。

```viso
fn classify(value: I64) -> String {
    return match value {
        0 => "zero",
        _ => "other",
    };
}

fn square(x: I64) -> I64 {
    x * x
}
```

等价于：

```viso
fn square(x: I64) -> I64 {
    return x * x;
}
```

---

## 62. Statement 总文法

```ebnf
statement            = { attribute }, statement_core ;

statement_core       = let_statement
                     | assignment_statement
                     | expression_statement
                     | return_statement
                     | break_statement
                     | continue_statement
                     | emit_statement
                     | start_statement
                     | transaction_statement
                     | if_statement
                     | match_statement
                     | while_statement
                     | for_statement
                     | loop_statement ;

let_statement        = "let", [ "mut" ], pattern,
                       [ ":", type ],
                       "=", expression, ";" ;

assignment_statement = assignable_path, assignment_operator,
                       expression, ";" ;

assignment_operator  = "=" | "+=" | "-=" | "*=" | "/=" | "%="
                     | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

expression_statement = expression, ";" ;

return_statement     = "return", [ expression ], ";" ;

break_statement      = "break", [ expression ], ";" ;

continue_statement   = "continue", ";" ;

emit_statement       = "emit", identifier,
                       "(", argument_list, ")", ";" ;

transaction_statement = "transaction", block ;

if_statement         = "if", head_expression, block,
                       [ "else", ( if_statement | block ) ] ;

match_statement      = match_expression, [ ";" ] ;

while_statement      = "while", head_expression, block ;

for_statement        = "for", pattern, "in", head_expression, block ;

loop_statement       = "loop", block ;
```

### 62.1 Statement 约束

- `let` Pattern 必须是 Irrefutable Pattern；
- Statement Attribute 必须在 Schema 中声明可作用于对应 Statement Kind；例如 Shader 循环可使用 `@max_iterations(64)`，但同一 Attribute 作用于普通 `let` 时必须报错；
- Statement Attribute 只产生 HIR 元数据，禁止改变 Tokenization、优先级或基础控制流语义；
- Assignment 不是 Expression，禁止 `a = b = c;`；
- `return` 只允许在 Callable/Closure 中；
- `break value;` 只允许从 `loop` 返回值；
- `continue` 只允许在 Behavior Loop 中；
- Behavior `for` 没有 `key`；`key` 只属于 View `for`；
- `transaction` 在 Action/Event Handler 中嵌套复用当前事务；在 Effect/Task Completion 中显式创建 UI 事务；
- 普通 `fn` 中禁止 `transaction`；
- `match_statement` 后的分号是可选的，因为它以结构化 Block 结束；Formatter 对纯 Statement 形式不输出分号。

---

## 63. Expression 顶层文法

```ebnf
expression           = range_expression ;

range_expression     = coalesce_expression,
                       [ ( ".." | "..=" ), coalesce_expression ] ;

coalesce_expression  = logical_or_expression,
                       [ "??", coalesce_expression ] ;

logical_or_expression = logical_and_expression,
                        { "||", logical_and_expression } ;

logical_and_expression = bit_or_expression,
                         { "&&", bit_or_expression } ;

bit_or_expression    = bit_xor_expression,
                       { "|", bit_xor_expression } ;

bit_xor_expression   = bit_and_expression,
                       { "^", bit_and_expression } ;

bit_and_expression   = equality_expression,
                       { "&", equality_expression } ;

equality_expression = comparison_expression,
                       [ ( "==" | "!=" ), comparison_expression ] ;

comparison_expression = shift_expression,
                        [ ( "<" | "<=" | ">" | ">=" ),
                          shift_expression ] ;

shift_expression     = additive_expression,
                       { ( "<<" | ">>" ), additive_expression } ;

additive_expression  = multiplicative_expression,
                       { ( "+" | "-" ), multiplicative_expression } ;

multiplicative_expression = cast_expression,
                            { ( "*" | "/" | "%" ),
                              cast_expression } ;

cast_expression      = unary_expression,
                       { "as", type } ;

unary_expression     = ( "!" | "~" | "+" | "-" | "await" ),
                       unary_expression
                     | postfix_expression ;

postfix_expression   = primary_expression,
                       { postfix_suffix } ;

postfix_suffix       = call_suffix
                     | index_suffix
                     | member_suffix
                     | optional_member_suffix
                     | try_suffix ;

call_suffix          = [ generic_call_args ],
                       "(", argument_list, ")" ;

generic_call_args    = "::", generic_args ;

index_suffix         = "[", expression, "]" ;

member_suffix        = ".", identifier ;

optional_member_suffix = "?.", identifier ;

try_suffix           = "?" ;
```

### 63.1 非结合操作符

比较和相等操作符是 Non-associative：

```viso
// 非法
0 < x < 10;
a == b == c;
```

必须写成：

```viso
0 < x && x < 10;
a == b && b == c;
```

### 63.2 Range

- `a..b` 是半开 Range；
- `a..=b` 是闭区间 Range；
- Viso 1.0 不支持省略起点或终点的 Range Literal；
- Range 不能链式出现；
- Range 类型由 `Range<T>` 或 `RangeInclusive<T>` 表示。

---

## 64. 运算符优先级和结合性表

数字越小优先级越高。

| 级别 | 构造                   | 结合性   | 说明                                     |
| ---: | ---------------------- | -------- | ---------------------------------------- |
|    1 | `()` `[]` `.` `?.` `?` | 左       | Call、Index、Member、Optional Chain、Try |
|    2 | `! ~ + - await`        | 右       | Prefix Unary                             |
|    3 | `as`                   | 左       | 显式转换                                 |
|    4 | `* / %`                | 左       | 乘除余                                   |
|    5 | `+ -`                  | 左       | 加减                                     |
|    6 | `<< >>`                | 左       | 位移                                     |
|    7 | `&`                    | 左       | 位与                                     |
|    8 | `^`                    | 左       | 位异或                                   |
|    9 | `\|`                   | 左       | 位或                                     |
|   10 | `< <= > >=`            | 不结合   | 比较                                     |
|   11 | `== !=`                | 不结合   | 相等                                     |
|   12 | `&&`                   | 左、短路 | 逻辑与                                   |
|   13 | `\|\|`                 | 左、短路 | 逻辑或                                   |
|   14 | `??`                   | 右、短路 | Option/Nullable Coalesce                 |
|   15 | `.. ..=`               | 不结合   | Range                                    |

Assignment 不属于 Expression 优先级表。

### 64.1 `??` 语义

`lhs ?? rhs` 要求：

- `lhs: Option<T>`，结果为 `T`；
- `rhs: T`；
- `lhs` 为 `Some(v)` 时不求值 `rhs`；
- `lhs` 为 `None` 时求值并返回 `rhs`。

Viso 没有隐式 Nullable Reference，因此 `??` 只适用于实现标准 `Coalesce` Trait 的类型；MVP 只内建 `Option<T>`。

### 64.2 Head Expression 消歧

```ebnf
head_expression = expression ;
```

`head_expression` 与普通 Expression 的类型规则完全相同，但 Parser 在下列紧邻结构 Block 的位置禁止最外层出现未加括号的 Record Expression：

- Behavior `if`、`while`、`for` 和 `match` 的 Header；
- View `if`、`for ... in`、`for ... key` 和 `match` 的 Header。

因此：

```viso
if ready { ... }                         // 合法
if (Point { x: 1.0, y: 2.0 }) { ... }  // 可解析，随后通常因条件不是 Bool 报类型错
for item in (Query { limit: 10 }.run()) key item.id { ... }
```

未加括号的 `Type { ... }` 在 Header 中不会吞掉控制流 Block。该限制属于 Parser Mode，不改变括号内 Expression Grammar。

---

## 65. Primary Expression

```ebnf
primary_expression   = literal
                     | path_expression
                     | self_expression
                     | tuple_expression
                     | list_expression
                     | record_expression
                     | grouped_expression
                     | block_expression
                     | if_expression
                     | match_expression
                     | closure_expression ;

literal              = integer_literal
                     | float_literal
                     | string_literal
                     | char_literal
                     | color_literal
                     | unit_literal
                     | "true"
                     | "false"
                     | "None" ;

path_expression      = path ;
self_expression      = "self" | "Self" ;

grouped_expression  = "(", expression, ")" ;

tuple_expression    = "(", expression, ",",
                      [ expression, { ",", expression }, [ "," ] ],
                      ")" ;

list_expression     = "[",
                      [ expression,
                        { ",", expression }, [ "," ] ],
                      "]" ;

record_expression   = path, [ generic_call_args ], "{",
                      [ record_initializer,
                        { ",", record_initializer }, [ "," ] ],
                      "}" ;

record_initializer  = identifier, ":", expression
                    | identifier
                    | "..", expression ;

block_expression    = block ;
```

泛型 Record Constructor 必须使用 Turbofish，例如：

```viso
Pair::<String, I64> { first: "age", second: 42 }
```

禁止在表达式域写 `Pair<String, I64> { ... }`；尖括号形式只属于 Type Position。

Record Shorthand：

```viso
User { id, name, avatar: None }
```

等价于：

```viso
User { id: id, name: name, avatar: None }
```

Record Update：

```viso
User { name: "New", ..old_user }
```

规则：

- `..base` 最多一次且必须是最后一个 Initializer；
- 所有未显式提供的字段从 Base 复制；
- 没有 Base 时必须提供所有无默认值字段；
- 未知、重复或不可见字段是静态错误；
- Record Literal 与 View Node 在不同 Parser 上下文中，且 Field 使用 `:`、Node Property 使用 `=`，不存在语法歧义。

---

## 66. Call 与 Argument

```ebnf
argument_list        = [ argument, { ",", argument }, [ "," ] ] ;

argument             = expression
                     | identifier, ":", expression ;

call_expression      = postfix_expression ;
```

规则：

- Positional Argument 必须出现在 Named Argument 之前；
- 同一参数不能重复提供；
- Named Argument 必须匹配 Callable Schema；
- 有默认值的参数可以省略；
- 方法调用 `receiver.method(args)` 在 HIR 中解析为带 Receiver 的 Call；
- Optional Member Call `value?.method()` 的结果为 `Option<R>`；
- `?` 传播要求当前 Callable 返回兼容的 `Option` 或 `Result`；
- 无字符串动态方法派发；Trait Object 方法通过 VTable Schema 解析。

---

## 67. If Expression

```ebnf
if_expression        = "if", head_expression, block,
                       "else", ( if_expression | block ) ;
```

作为值使用时 `else` 必填：

```viso
let label = if count == 1 {
    "1 item"
} else {
    format("{} items", count)
};
```

所有可达分支的 Tail Expression 必须统一为一个类型。

Statement 位置可以使用 §62 的无 `else` If Statement。

---

## 68. Match Expression

```ebnf
match_expression     = "match", head_expression, "{",
                       match_arm,
                       { ",", match_arm }, [ "," ],
                       "}" ;

match_arm            = pattern,
                       [ "if", expression ],
                       "=>", ( expression | block ) ;
```

规则：

- Match 必须穷尽；
- 不可达 Arm 必须警告，CI Strict Mode 可提升为错误；
- Guard 必须为 Bool 且无副作用；
- 所有 Arm 的结果类型必须统一；
- Statement Match 的结果类型为 `Unit`；
- `=>` 只在 Match Arm 中出现；
- View Match 使用独立 Grammar，Arm 右侧只能是 `view_block`。

---

## 69. Closure

```ebnf
closure_expression   = [ "move" ],
                       ( empty_closure_params | closure_params ),
                       [ "->", type ],
                       ( expression | block ) ;

empty_closure_params = "||" ;

closure_params       = "|", closure_parameter,
                       { ",", closure_parameter }, [ "," ], "|" ;

closure_parameter    = [ "mut" ], pattern,
                       [ ":", type ] ;
```

示例：

```viso
let doubled = items.map(|item| item.value * 2);

let formatter = |value: I64| -> String {
    format("Value: {}", value)
};

start scheduler.after(250ms, move || {
    log::info("timer completed");
});
```

### 69.1 Closure 推断

- 参数类型可以由期望的 Function Type 推断；
- 没有期望类型且参数未标注时是错误；
- 返回类型可由 Tail Expression 推断；
- Closure Capture Set 由 HIR 计算并写入 Schema；
- 普通 Closure 默认只可在当前同步调用范围内借用不可变局部值；
- 逃逸、存储、跨 Task 或跨 Tick 的 Closure 必须使用 `move`；
- `move` 按值复制或 Retain Capture；
- 不满足 `Send`/`Sync` 的 Capture 不能跨线程 Executor；
- Component State 不以裸引用捕获，编译器生成受生命周期约束的 State Lens 或快照；
- 游戏 Fixed Tick 逻辑推荐使用 System State，不推荐依赖长期闭包隐式捕获可变变量。

### 69.2 Closure Kind

HIR 将 Closure 推断为：

```text
Fn       不修改 Capture
FnMut    修改自身拥有的 Capture
TaskFn   仅在期望类型为 TaskFn 时成立；闭包体可以包含 await 并降低为异步状态机
```

对外 API 应显式要求相应 Function Type。Viso 1.0 不引入独立 `async |...|` 词法形式：普通 Closure 只有在期望类型为 `TaskFn(...) -> T` 的位置才能包含 `await`；没有期望类型时，含 `await` 的 Closure 必须报类型推断错误。

---

## 70. Pattern 总文法

```ebnf
pattern                    = or_pattern ;

or_pattern                 = binding_pattern,
                             { "|", binding_pattern } ;

binding_pattern            = [ "mut" ], identifier, "@", range_pattern
                           | range_pattern ;

range_pattern              = primary_pattern,
                             [ ( ".." | "..=" ), primary_pattern ] ;

primary_pattern            = "_"
                           | literal_pattern
                           | identifier_pattern
                           | tuple_pattern
                           | list_pattern
                           | constructor_pattern
                           | qualified_variant_pattern
                           | grouped_pattern ;

literal_pattern            = integer_literal
                           | char_literal
                           | string_literal
                           | "true"
                           | "false" ;

identifier_pattern         = [ "mut" ], identifier ;

tuple_pattern              = "(", pattern, ",",
                             [ pattern, { ",", pattern }, [ "," ] ],
                             ")" ;

list_pattern               = "[",
                             [ list_pattern_item,
                               { ",", list_pattern_item }, [ "," ] ],
                             "]" ;

list_pattern_item          = pattern
                           | "..", [ identifier ] ;

constructor_pattern        = type_path, constructor_pattern_payload ;

constructor_pattern_payload = "(",
                              [ pattern, { ",", pattern }, [ "," ] ],
                              ")"
                            | "{",
                              [ record_pattern_field,
                                { ",", record_pattern_field }, [ "," ] ],
                              "}" ;

qualified_variant_pattern  = identifier, "::", identifier,
                             { "::", identifier } ;

record_pattern_field       = identifier, ":", pattern
                           | identifier
                           | ".." ;

grouped_pattern            = "(", pattern, ")" ;
```

### 70.1 Pattern 分类

Irrefutable：

```text
_
identifier
(mut identifier)
Tuple/Record 仅由 Irrefutable 子 Pattern 构成且类型只有一个构造形式
```

Refutable：

```text
Literal
Range
Enum Variant
List 长度 Pattern
Or Pattern
```

`let` 和普通参数只能使用 Irrefutable Pattern。`match`、`if let`（未来版本）和 Event Payload Handler 可以使用 Refutable Pattern；Event Handler Pattern 不匹配时该 Handler 被跳过。

消歧规则是强制性的：

- 裸单段 `identifier` 永远是 Binding Pattern；
- 无 Payload Enum Variant 必须写成至少两段的限定路径，例如 `LoadState::idle`；
- 带 Payload 的构造式必须紧跟 `(...)` 或 `{...}`，例如 `Option::Some(value)`、`Point { x, y }`；
- Parser 不得根据首字母大小写猜测“绑定还是 Variant”。

### 70.2 Or Pattern

Or Pattern 的每个分支必须绑定相同名称集合和兼容类型：

```viso
LoadState::failed(error) | LoadState::cancelled(error) => {
    log_error(error);
}
```

---

## 71. `let`、Shadowing 与作用域

- `let` 默认不可变；
- 修改局部变量必须声明 `let mut`；
- 同一 Lexical Block 可以 Shadow 外层名称；
- 同一 Block 中不能重复声明尚在同一作用域内的名称；
- State、Input、Computed 名称不允许被 Component 顶层成员 Shadow；
- 局部变量可以 Shadow成员，但 Compiler 必须发出默认警告；
- Pattern Binding 从 Initializer 完成后开始生效；
- Initializer 中读取的是外层同名符号；
- 借用或 Lens 的生存范围由 HIR 计算，不以字符串名称追踪。

```viso
let mut total: I64 = 0;
for item in items {
    total += item.value;
}
```

---

## 72. `return`、错误传播和 Panic

- `return expr;` 类型必须兼容 Callable 返回类型；
- `return;` 只适用于 `Unit`；
- `?` 通过 `Try` Trait 传播；MVP 内建 Option 和 Result；
- 普通业务错误应使用 `Result<T, E>`；
- DSL 不提供用户可调用的 Unchecked Panic；
- `panic!` 不属于核心语法；
- 不变量失败使用标准 `assert(...)`，在 Production Policy 中转成结构化 Fault 或终止当前 Isolate；
- Native Panic 必须在 Bridge 捕获，不能展开穿过 VM 边界。

---

# 第九部分：静态类型系统

## 73. 基础类型的唯一清单

核心标量：

```text
Bool
I8 I16 I32 I64
U8 U16 U32 U64
F32 F64
Char
String
Bytes
Unit
Never
Color
```

UI 量纲类型：

```text
Dp Px Sp Percent Duration Angle Frequency
```

明确决定：

- 没有 `Int`；
- 没有 `UInt`；
- 没有 `Float`；
- 没有平台宽度随目标变化的整数；
- Shader ABI 只使用显式定宽类型；
- 指针大小只通过 Native Schema 的 `USize` opaque 类型暴露，普通 DSL 不可直接算术。

删除 `Float` 是为了消除 Host 默认 F64、GPU 需要 F32 时的跨域歧义。

---

## 74. 类型推断边界

允许推断：

- 局部 `let`；
- Component/System 的私有 `state`；
- 私有 `computed`；
- Closure 参数在存在 Expected Function Type 时；
- Generic Call 的类型参数；
- 未定型数值字面量；
- Match/If Expression 的统一结果类型。

必须显式类型：

- Input；
- 对外公开或跨持久化边界的 State Schema；
- Event Payload；
- Slot；
- Record Field；
- Public/Exported Callable 参数和返回值；
- Trait Method；
- Native 声明；
- Resource 外层类型；
- Shader Interface；
- System Input 和 State；
- Hot Reload 需要持久化的公开数据。

Compiler 禁止把无法推断的值回退成 `dynamic`。Viso 1.0 核心没有隐式 Dynamic 类型。

---

## 75. Numeric Literal 定型

未定型整数或浮点字面量不是运行时类型。

例：

```viso
let a: I32 = 1;       // 字面量直接定型为 I32
let b: F32 = 1.0;     // 字面量直接定型为 F32
let c = 1;            // Host 默认 I64
let d = 1.0;          // Host 默认 F64
```

Literal 定型必须检查范围和精度策略。它不等同于一个已存在的 I64/F64 运行时值被隐式缩窄。

---

## 76. 数值转换

### 76.1 允许的隐式安全拓宽

```text
I8 -> I16 -> I32 -> I64
U8 -> U16 -> U32 -> U64
F32 -> F64
```

### 76.2 禁止的隐式转换

```text
任意 signed <-> unsigned
任意 integer -> float
任意 float -> integer
F64 -> F32
宽整数 -> 窄整数
不同 UI 量纲互转
Color <-> Vec4F32
Bool <-> integer
String <-> number
```

必须显式：

```viso
let x: F32 = value as F32;
let count: U32 = checked_cast(value)?;
```

`as` 的允许集合由 `Cast` Trait 和 Compiler Builtin 定义。可能丢失范围或精度的 Cast 在 Strict Mode 必须要求 `checked_cast`、`saturating_cast` 或 `truncating_cast`，不能只写 `as`。

---

## 77. 名义类型、结构类型和子类型

- Record、Enum、Component、System 和 Native Type 是名义类型；
- Tuple 和 Function Type 是结构类型；
- `Never` 是所有类型的 Bottom Type；
- 具体类型可以向其实现的 `dyn Trait` 进行受控擦除；
- Component Node 可以向 `Node` 接口擦除；
- 不存在类继承子类型；
- 不存在 Record 宽度子类型；
- `List<T>`、`State<T>`、`Resource<T,E>`、`Handle<T>` 默认 Invariant；
- 只读视图类型 `ReadOnlyList<T>` 可以由库声明 Covariant；
- Null 不属于引用类型，因此不存在 Nullable Subtyping。

这使 Component Composition 可扩展，同时避免复杂的隐式父类规则。

---

## 78. Generic

```viso
fn find_by_key<T, K>(items: ReadOnlyList<T>, key: K) -> Option<T>
where
    T: HasKey<K> + Clone,
    K: StableKey,
{
    // ...
}
```

规范：

- Generic 默认采用静态单态化或共享 Typed Bytecode，由 Backend 选择；
- 两种实现必须保持可观察语义一致；
- Generic 参数默认 Invariant；
- Viso 1.0 不提供用户自定义 Variance Annotation；
- Const Generic 只允许整数、Bool、Char 和满足 `ConstValue` 的 Enum；
- Generic Trait Resolution 必须确定且不能依赖 Import 顺序；
- 重叠 Impl 是错误；
- 不提供隐式 Specialization；
- Recursive Type 必须通过 Handle/Box-like 间接层打断无限大小。

---

## 79. Trait 和约束

Trait 可以声明：

- `fn`；
- `action`；
- `task`；
- Associated Type；
- Associated Const；
- Super Trait。

Trait 不能声明：

- State Storage；
- View；
- Field Layout；
- 隐式构造函数；
- 新语法。

Trait Bound：

```viso
T: Clone + Eq + StableKey
```

实现解析采用：

1. 当前 Type 的 Inherent Member；
2. 显式导入且满足的 Trait Member；
3. Auto Trait；
4. 若仍有多个候选则歧义错误，不采用“最后导入者获胜”。

---

## 80. `StableKey` 正式定义

```viso
export trait StableKey: Eq + Hash + Clone {
    fn stable_hash(self) -> U64;
}
```

### 80.1 内建实现

默认实现 `StableKey`：

```text
Bool
I8/I16/I32/I64
U8/U16/U32/U64
Char
String
不带 Payload 的 Enum
所有字段均为 StableKey 的 Tuple/Record
显式稳定的 UUID/ID Native Value
```

默认不实现：

```text
F32/F64
Handle<T>
NodeRef<T>
Resource<T,E>
List<T>
Map<K,V>
包含时间戳随机盐的进程局部 ID
```

### 80.2 派生

```viso
@derive(Eq, Hash, StableKey)
record TodoId {
    value: U64;
}
```

派生要求：

- 所有字段实现 StableKey；
- Stable Hash 算法版本写入 Schema；
- 字段顺序和名称参与类型版本，避免热重载误匹配；
- 不能依赖进程随机 Hash Seed；
- 更改 Stable Hash 语义属于迁移 Breaking Change。

### 80.3 为什么 Float 不能作 Key

NaN、正负零、舍入和平台优化会破坏一致的 Eq/Hash 预期。需要位置类身份时必须量化或转换成显式整数 ID。

---

## 81. Function Effect Type

Callable 的 Effect Class 是类型系统的一部分：

```text
fn      Pure/Read
action  Sync Mutating
Task    Async/Cancelable
```

允许调用矩阵：

| 调用者                    |        `fn` |           `action` | `task` 直接调用 |    `start task` | `await task` |
| ------------------------- | ----------: | -----------------: | --------------: | --------------: | -----------: |
| View/Computed             |          是 |                 否 |              否 |              否 |           否 |
| `fn`                      |          是 |                 否 |              否 |              否 |           否 |
| Action/Event              |          是 |                 是 |              否 |              是 |           否 |
| Effect                    |          是 | 仅显式 Transaction |              否 |              是 |           否 |
| Task                      |          是 |                 否 |              是 |              否 |           是 |
| Task Completion Handler   |          是 |                 是 |              否 |              是 |           否 |
| System FixedUpdate Action |          是 |                 是 |              否 | 受 Profile 限制 |           否 |
| Shader                    | Shader `fn` |                 否 |              否 |              否 |           否 |

静态 Effect Check 必须发生在 HIR 阶段。

---

## 82. Ownership、Value、Handle 和 Lens

Viso 不是直接暴露 Rust Borrow Checker 的语法，但必须有明确所有权模型：

- Scalar、Small Record 和 Immutable Collection 是 Value；
- `Handle<T>` 是引用计数或 Runtime-owned 的稳定句柄；
- `WeakHandle<T>` 不延长生命周期；
- `NodeRef<T>` 只在 UI Actor 和对应 Component 生命周期内有效；
- State Lens 是编译器生成的受限可赋值引用；
- Task Capture 默认取快照；
- `move` Closure Retain 可拥有值；
- Native Schema 必须标记参数为 copy、borrow、consume 或 retain；
- DSL 不允许构造悬垂裸指针；
- 跨线程值必须实现 `SendValue`；
- UI Actor 专属 Handle 不得跨线程。

---

## 83. Error Type 与诊断类型

语言层业务错误使用：

```text
Result<T, E>
Option<T>
```

运行时故障使用独立的 `RuntimeFault`：

```text
InstructionBudgetExceeded
MemoryBudgetExceeded
CapabilityDenied
NativePanic
InvalidHandle
DuplicateKey
ReactiveCycle
ShaderBackendFailure
HotReloadMigrationFailure
```

Runtime Fault 不应伪装成业务 Error。Isolate Policy 决定它导致：

- 回滚当前 Transaction；
- 取消所属 Task；
- 保留 Last-good UI；
- 禁用故障 System；
- 或终止进程。

---

# 第十部分：运行时语义

## 84. Component 实例生命周期

状态机：

```text
Allocated
  -> InputsBound
  -> StateInitialized
  -> ComputedGraphReady
  -> ViewMounted
  -> EffectsMounted
  -> Active
  -> Unmounting
  -> Disposed
```

规则：

1. Input 在 State 之前绑定；
2. State 按源码顺序初始化；
3. Computed 建图但按需求值；
4. 首次 View 生成 UI IR；
5. Node Mount 完成后运行 Mount Effect；
6. Active 期间响应 Event、State 和 Resource；
7. Unmount 时先取消 Task/Resource，再运行 Effect Cleanup，再销毁 Node；
8. Disposed 实例的 Lens、NodeRef 和 UI Handle 立即失效；
9. Hot Reload 使用迁移状态机，不等价于普通 Unmount/Mount。

---

## 85. 响应式依赖图

Reactive Source：

```text
Input Cell
State Cell
Resource State Cell
Theme Context Cell
System Observable Cell
```

Reactive Derived：

```text
Computed
Property Binding
Conditional/List/Match View Node
Effect Dependency
Resource Key
```

编译器为每个 Derived 生成静态读取集合；动态索引、Trait Dispatch 等无法完全静态解析时，Runtime 在求值期间补充精确读取边。

依赖边必须包含：

```text
source_symbol_id
derived_symbol_id
read_kind
source_span
invalidation_class
```

---

## 86. Transaction 与批处理

State 修改只能发生在：

- Action；
- Event Handler；
- Task Completion Handler；
- 显式 `transaction`；
- Profile 允许的 System Action。

事务提交顺序：

```text
1. 校验写集合
2. 应用 State 新值
3. 增加 State Revision
4. 标记 Computed Dirty
5. 拓扑求值被急需的 Computed
6. 生成 UI Patch
7. 验证 Key 和 Slot 不变量
8. 原子提交 UI Patch
9. 排队 Effect
10. 请求 Layout/Paint/Semantics/Input 失效
```

同一外层 Transaction 中对同一 State 多次写入只产生一次 Revision 和一次下游调度。

失败时：

- State 恢复；
- 新 UI Patch 丢弃；
- Effect 不运行；
- Event Emit Buffer 丢弃或按 Event Schema 的 Failure Policy 处理；
- Runtime 返回结构化错误。

---

## 87. 精确失效

每个 Property Schema 声明：

```text
affects(layout)
affects(paint)
affects(semantics)
affects(input)
affects(resource)
```

可组合：

```text
affects(layout + paint + semantics)
```

运行时规则：

- Paint-only 修改不得无条件重算整棵 Layout；
- Semantics-only 修改不得重建 GPU Draw List；
- Input Region 修改必须在下一次 Pointer Dispatch 前生效；
- Layout 修改向祖先传播到第一个 Layout Boundary；
- Draw Cache 必须按 Property Revision 精确失效；
- Animation 每帧只使实际变化的通道失效。

---

## 88. Node Identity

每个 Runtime Node ID 由以下组合生成：

```text
ComponentInstanceId
+ SourceStableSymbolId
+ DynamicBranchNamespace
+ OptionalListKey
+ TemplateExpansionPath
```

### 88.1 Named Node

`node add_button: Button` 的 `SourceStableSymbolId` 来自：

1. 显式 `@stable("...")`；或
2. Module + Component + Node Name + 结构化 Source Path。

### 88.2 Anonymous Node

匿名节点的符号 ID 来自 Parent Stable ID + 同类 Structural Position + Source Fingerprint。它适合无本地交互状态的装饰节点。

### 88.3 Keyed Child

列表 Child ID 追加 Stable Key。列表排序不改变 ID。

### 88.4 Preserved Branch

Preserved Branch 追加编译期 Preserve String。它与 Runtime List Key 无关。

---

## 89. UI Diff 与 Patch

UI IR Patch 至少包括：

```text
CreateNode
DeleteNode
MoveNode
ReplaceNodeType
SetProperty
ClearProperty
AttachHandler
DetachHandler
FillSlot
Invalidate
```

规则：

- 同 ID、同 Component Type：原位更新；
- 同 ID、兼容 Type Migration：执行 Migration；
- 同 ID、不兼容 Type：Replace；
- Keyed List Move 不触发 State 销毁；
- Patch 在完整验证后原子提交；
- Debug 模式保留 Patch Trace，供 AI/Inspector 查询。

---

## 90. Event Dispatch

传播阶段：

```text
Capture -> Target -> Bubble -> DefaultAction
```

Event Schema 声明：

```text
payload type
bubbles
cancelable
composed
trusted
frequency class
coalescing policy
```

规则：

- Capture 从 Root 到 Target Parent；
- Target Handler 在 Target 执行；
- Bubble 从 Target Parent 返回 Root；
- `stop_propagation` 停止后续节点；
- `stop_immediate_propagation` 停止当前节点余下 Handler；
- `prevent_default` 只对 Cancelable Event 有效；
- 高频 Pointer Move 可以由 Schema 合并，但必须保留最后位置和累计 Delta；
- 每个 Handler 在独立或共享 Event Transaction 中执行，Profile 必须固定策略；UI Profile 默认整个 Event Dispatch 共用一个外层 Transaction。

---

## 91. Effect 调度

Effect Queue 在 UI Patch 提交后运行：

```text
Commit UI Patch
-> Layout 可选执行
-> Mount/Change Effects
-> Paint Scheduling
```

实现必须保证：

- 同一 Commit 中同一 Effect 最多排队一次；
- Dependency 使用值语义或 Revision 判定变化；
- 新一次运行前先 Cleanup 上一次；
- Effect 自身提交新 Transaction 时不会递归同步重入同一 Effect；
- 重入被排到下一 Microtask Epoch；
- 一个 Epoch 的 Effect 重跑次数有上限；超限报告 ReactiveCycle。

---

## 92. Task 与结构化并发

Task Tree：

```text
ApplicationScope
  ComponentScope
    NodeKeyScope
      TaskSlot
        ChildTask
```

取消从父向子传播。

任务结果提交规则：

- Completion Handler 只在所属 Scope 仍存活时执行；
- 若 Key 已变化，过期任务结果被丢弃；
- `keep_latest` 会取消仍在运行的前序 Task；
- `drop_new` 忽略新 Start；
- `queue` 顺序执行；
- `parallel(limit)` 有明确并发上限；
- 热重载时只有 Schema、Capture 和 Task Code Version 兼容的任务可以继续；默认取消。

Task 不允许成为无法追踪的 Global Detached Future。确需进程级后台任务时必须显式绑定 Application System Scope。

---

## 93. Resource 运行时

Resource Key：

```text
ResourceSymbolId + StableKeyValue + ScopeId + LoaderVersion
```

缓存行为：

- `cache_for(duration)` 从成功提交时间开始；
- `keep_latest` 保证过期请求结果不能覆盖新 Key；
- `debounce(duration)` 只延迟 Loader Start，不延迟本地 State Commit；
- Error 是否缓存由单独 Policy 决定；
- Resource 重新验证可进入 `reloading(old_value)`；
- Loader Capability 在 Start 前检查；
- LoaderVersion 变化默认使缓存失效，除非声明兼容迁移。

---

## 94. Hot Reload 事务

Hot Reload 管线：

```text
Parse new source
-> Build CST/AST/HIR
-> Type/Effect/Capability Check
-> Lower all affected IR
-> Validate Schema Compatibility
-> Build State/Node/Task/Resource Migration Plan
-> Shadow-evaluate pure initializers
-> Atomically swap code and UI patch
-> Run migration effects
```

任一步失败：

- 保留 Last-good Code；
- 保留 Last-good UI；
- 保留现有 State 和运行中的兼容任务；
- 返回结构化诊断；
- 禁止显示半构建空界面。

### 94.1 State Migration

同 Stable Symbol ID：

- 类型完全一致：保留；
- 安全拓宽：可以自动迁移；
- Record 新增带默认值字段：可以自动迁移；
- Enum 删除当前活跃 Variant：不兼容；
- 类型不兼容：查找显式 `@migrate(from: "...")` 函数；
- 无迁移函数：使用新 Initializer，且产生状态重置通知。

### 94.2 Node Migration

- 同 ID、同 Type：保留局部 Widget State；
- 同 ID、兼容 Schema Version：运行 Widget Migration；
- Type 改变：Replace；
- Part/Slot Contract 改变：验证调用方后原子更新；
- 焦点、选择、滚动和动画必须由 Widget Schema 标记可迁移字段。

---

## 95. Capability 运行时

静态检查不能替代运行时授权。

每个 Isolate/Module 获得 Capability Set：

```text
ui.basic
gpu.draw
network.http
filesystem.read.assets
audio.output
game.world
process.spawn
clipboard.read
clipboard.write
```

Native Call 发生时验证：

```text
required_capability subset_of isolate_capabilities
```

失败返回 `CapabilityDenied`，不得绕过到 Rust Panic。

AI 生成的 Preview 默认仅有：

```text
ui.basic
gpu.draw.sandboxed
asset.read.package
```

网络、文件系统、进程和剪贴板均需显式授权。

---

## 96. 执行预算

每个 Isolate/Profile 必须配置：

```text
instruction budget
wall-clock slice
memory budget
call depth
collection size
node count
shader complexity
native call quota
task count
resource cache budget
```

超限产生 Runtime Fault 并回滚当前 Transaction。预算不能通过递归 Task、热重载或 Native Callback 重置规避。

---

# 第十一部分：Shader、Native ABI 与多执行域边界

## 97. Shader 声明文法

```ebnf
shader_decl          = "shader", identifier,
                       [ generic_params ],
                       "{", { shader_member }, "}" ;

shader_member        = shader_uniform
                     | shader_instance
                     | shader_varying
                     | shader_texture
                     | shader_sampler
                     | shader_function
                     | vertex_entry
                     | fragment_entry
                     | compute_entry ;

shader_uniform       = "uniform", identifier, ":", shader_type, ";" ;

shader_instance      = "instance", identifier, ":", shader_type, ";" ;

shader_varying       = "varying", identifier, ":", shader_type, ";" ;

shader_texture       = "texture", identifier, ":", shader_texture_type, ";" ;

shader_sampler       = "sampler", identifier, ":", shader_sampler_type, ";" ;

shader_function      = "fn", identifier,
                       "(", shader_parameter_list, ")",
                       "->", shader_type,
                       shader_block ;

vertex_entry         = "vertex", "(", shader_parameter_list, ")",
                       "->", shader_type,
                       shader_block ;

fragment_entry       = "fragment", "(", shader_parameter_list, ")",
                       "->", shader_type,
                       shader_block ;

compute_entry        = "compute", "(", shader_parameter_list, ")",
                       "->", shader_type,
                       shader_block ;

shader_parameter_list = [ shader_parameter,
                          { ",", shader_parameter }, [ "," ] ] ;

shader_parameter     = identifier, ":", shader_type ;

shader_block         = block ;
```

Shader 的 Export 与其他顶层声明一致，由 `top_level_decl` 统一处理；`shader_decl` 本身不重复消费 `export`。

---

## 98. Shader Type

MVP Shader 类型：

```text
Bool
I32 U32 F32
Vec2F32 Vec3F32 Vec4F32
Vec2I32 Vec3I32 Vec4I32
Vec2U32 Vec3U32 Vec4U32
Mat2F32 Mat3F32 Mat4F32
ColorLinear
Texture2D<F32>
Texture2D<Vec4F32>
Sampler
```

```ebnf
shader_type          = "Bool" | "I32" | "U32" | "F32"
                     | "Vec2F32" | "Vec3F32" | "Vec4F32"
                     | "Vec2I32" | "Vec3I32" | "Vec4I32"
                     | "Vec2U32" | "Vec3U32" | "Vec4U32"
                     | "Mat2F32" | "Mat3F32" | "Mat4F32"
                     | "ColorLinear"
                     | shader_struct_type ;

shader_texture_type  = type_path ;
shader_sampler_type  = type_path ;
shader_struct_type   = type_path ;
```

明确规则：

- Shader 中禁止 `F64`；
- Host `F64` 不可隐式进入 Shader；
- Host `Color` 必须通过明确的 sRGB-to-linear Conversion 写入 `ColorLinear`；
- Shader Record 必须由 `@shader_value` 标记，且所有字段都是 Shader Type；
- Shader 不支持 String、Bytes、List、Map、Resource、Handle、NodeRef、Trait Object；
- Texture 和 Sampler 是资源绑定，不是普通 Value Copy。

---

## 99. Shader 可用语法子集

允许：

- `let` 和不可变/局部可变标量；
- 算术、位运算、比较、逻辑；
- `if`、`match` 的可静态 Lowering 子集；
- 编译期或可证明有界的 `for`；
- 纯 Shader Function；
- Vector/Matrix 构造与 Swizzle；
- Texture Sample Intrinsic；
- Derivative Intrinsic，仅 Fragment；
- 显式 Cast。

禁止：

- State、Action、Effect、Task、Resource；
- Closure；
- Dynamic Dispatch；
- Recursion；
- Heap Allocation；
- Unbounded `while`/`loop`；
- Native Handle；
- Capability Call；
- Exception/Result Propagation；
- `await`；
- 任意字符串操作。

### 99.1 Loop 规则

```viso
for i in 0..4 {
    // 编译期有界，合法
}
```

若上界来自 Uniform，必须有静态最大值：

```viso
@max_iterations(64)
for i in 0..light_count {
    // Runtime count <= 64
}
```

Backend 不支持动态 Loop 时，Compiler 可以在限制内展开或拒绝，不得无界生成代码。

---

## 100. Shader 示例

```viso
export shader RoundedRect {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance rect_size: Vec2F32;
    instance radius: F32;
    instance color: ColorLinear;

    varying local_pos: Vec2F32;

    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        let position = rect_pos + unit * rect_size;
        local_pos = unit * rect_size;

        return VertexOutput {
            clip_position: to_clip(position, viewport_size),
        };
    }

    fragment() -> Vec4F32 {
        let distance = rounded_rect_sdf(local_pos, rect_size, radius);
        let alpha = smoothstep(1.0f32, 0.0f32, distance);
        return color.to_vec4() * alpha;
    }
}
```

Shader Entry 的实际 Builtin 参数、返回 Record 和可写 Varying 由 Render Profile Schema 定义。上例展示语言形态，不允许 Backend 自行改变核心表达式优先级。

---

## 101. Instance ABI

每个 Shader 生成显式 Descriptor：

```text
ShaderId
UniformBlock[]
InstanceField[]
Varying[]
TextureBinding[]
SamplerBinding[]
EntryPoint[]
```

每个 Field Descriptor 至少包含：

```text
stable_field_id
name
type
alignment
size
offset
array_stride
matrix_stride
interpolation
source_span
```

规范：

- Rust 端不得通过“某个字段之后的内存都是 Instance 数据”读取结构体尾部；
- 不依赖 Rust 默认字段布局；
- ABI Layout 由 Viso Shader Layout Algorithm 唯一生成；
- Rust Bridge 通过生成代码或安全 Encoder 写入字段；
- Descriptor 和 Backend Reflection 必须在 Debug/CI 比对；
- Layout 版本进入 Pipeline Cache Key；
- Hot Reload 若 Instance Layout 不兼容，创建新 Buffer/Pipeline 后原子交换；
- 不再使用的 Buffer 在 GPU Fence 后释放。

---

## 102. Shader Lowering

```text
Shader AST
-> Shader HIR（Name/Type/Stage Check）
-> Structured Control Flow IR
-> SSA-like Shader IR
-> Validation
-> Backend Codegen
   Metal / HLSL / GLSL / WGSL / CPU reference interpreter
```

CPU Reference Interpreter 是测试工具，不要求成为完整 UI 软件 Renderer。它用于：

- 常量折叠验证；
- Shader 单元测试；
- Backend 差异检测；
- Headless Golden Test；
- AI 生成 Shader 的快速安全检查。

---

## 103. Native Handle Schema

```text
NativeTypeSchema {
    type_id
    version
    ownership
    thread_domain
    clone_policy
    drop_policy
    hot_reload_policy
    methods[]
    required_capabilities[]
}
```

Method Schema：

```text
NativeMethodSchema {
    method_id
    kind: Fn | Action | Task
    parameters[]
    return_type
    error_type
    capabilities[]
    thread_domain
    deterministic
    realtime_safe
    budget_cost
}
```

规则：

- DSL 通过 Numeric Stable ID 调用，不在热路径依赖字符串查找；
- Debug 信息保留可读名称；
- Schema Version 变化触发兼容检查；
- 不允许一个方法在不同平台注册为不同 Effect Kind；
- Realtime-safe 方法禁止分配、阻塞或获取非实时锁；
- Invalid Handle 返回 Typed Error 或 Runtime Fault；
- Handle Drop 必须在声明的 Thread Domain 执行。

---

# 第十二部分：游戏表达能力与 Game Profile

## 104. 结论

Viso DSL 1.0 的语言表达能力足以实现与 Makepad 宿主注入式游戏脚本相同类型的游戏逻辑，包括：

- 固定步长更新；
- 输入快照；
- Entity 创建和命令；
- 移动、跳跃、射击和 AI；
- Timer 和异步加载；
- 碰撞事件；
- HUD；
- Shader 和实例化绘制；
- 热重载；
- Last-good 运行版本；
- 可记录、可重放的确定性测试。

不同之处是：长期游戏状态放在明确的 `system state` 中，Tick 通过 Trait/Scheduler 调用，而不是依赖某个动态全局对象和长期闭包的隐式捕获。

---

## 105. Game Profile 不是 Parser 特例

标准库或独立 crate 提供：

```viso
export trait FixedUpdate {
    action fixed_update(frame: FixedFrame);
}

export trait FrameUpdate {
    action frame_update(frame: RenderFrame);
}

export trait CollisionListener {
    action collision(event: CollisionEvent);
}
```

Parser 只认识：

```text
trait
system
implements
action
```

它不认识：

```text
GameWorld
EntityId
walk
jump
raycast
collision
```

这些来自 `viso::game` Native Schema。第三方可替换物理、ECS 或渲染实现而不修改语法。

---

## 106. 固定步长 Scheduler 语义

Game Profile 必须定义：

```text
fixed_dt
max_catch_up_steps
input_sampling_point
system_order
physics_order
collision_delivery_order
render_interpolation_policy
```

推荐顺序：

```text
1. 收集并冻结 InputSnapshot
2. 执行 PrePhysics FixedUpdate Systems
3. 应用 Game Command Buffer
4. 运行 Physics Step
5. 生成 Collision/Event Buffer
6. 执行 PostPhysics Systems
7. 提交 World Revision
8. 生成 Render Extraction Snapshot
9. 插值并提交 GPU
```

要求：

- Fixed Tick 不使用不受控 Wall Clock；
- `frame.dt` 是 Profile 固定值；
- Random 必须来自注入的 Seeded RNG；
- Entity 迭代顺序必须稳定或显式声明无序；
- 多线程系统必须通过 Deterministic Command Buffer 合并；
- 每个 Tick 有 Instruction/Native Call Budget；
- Tick 超限时采用 Profile 策略，不能无限阻塞 UI Thread。

---

## 107. 完整游戏 System 示例

```viso

import viso::game::{
    GameWorld,
    EntityId,
    FixedUpdate,
    FixedFrame,
    CollisionListener,
    CollisionEvent,
    SpawnDesc,
};

export system PlayerController implements FixedUpdate + CollisionListener {
    input world: Handle<GameWorld>;
    input player: EntityId;

    state move_speed: F32 = 6.0f32;
    state jump_speed: F32 = 10.0f32;
    state score: I64 = 0;
    state respawn_point: Vec3F32 = Vec3F32::new(0.0f32, 4.0f32, 0.0f32);

    computed alive: Bool = world.is_alive(player);

    action fixed_update(frame: FixedFrame) {
        if !alive {
            return;
        }

        let movement = Vec2F32::new(
            frame.input.axis(InputAxis::move_x),
            frame.input.axis(InputAxis::move_z),
        );

        world.walk(
            player,
            movement.x * move_speed,
            movement.y * move_speed,
        );

        if frame.input.pressed(InputAction::jump)
            && world.on_floor(player) {
            world.jump(player, jump_speed);
        }

        if world.position(player).y < -20.0f32 {
            world.teleport(player, respawn_point);
        }
    }

    action collision(event: CollisionEvent) {
        match event.other_of(player) {
            Option::Some(other) if world.has_tag(other, GameTag::coin) => {
                world.remove(other);
                score += 1;
            },
            _ => {},
        }
    }
}
```

### 107.1 为什么适合人和 AI

- Tick 入口由 `implements FixedUpdate` 明确；
- 状态都列在 `state`；
- 数值宽度显式；
- World API 由 Schema 查询；
- 输入是 Typed Enum，不依赖任意字符串；
- 碰撞是 Typed Event；
- 没有隐藏全局变量；
- Hot Reload 可以按 System/State Stable ID 迁移；
- AI 可以通过 `cargo viso schema viso::game::GameWorld` 查询方法。

---

## 108. Game World 命令与确定性

Native GameWorld 的 mutating Method 应被标为 `native action`。在多线程或确定性 Profile 中，这些调用可以 Lower 为 Command Buffer：

```text
world.walk(entity, x, z)
-> GameCommand::Walk { entity, x, z, source_system, sequence }
```

Command 合并顺序：

```text
system_order
then source_entity/order_key
then per-system sequence
```

冲突策略由 API 定义：

- Additive Force：累加；
- Set Transform：最后合法命令获胜或冲突报错；
- Remove Entity：覆盖后续针对该 Entity 的普通命令；
- Spawn：返回 Deferred Entity Token，在提交后变为 EntityId。

这些是 Game Profile 语义，不污染通用 UI DSL。

---

## 109. HUD 和游戏场景组合

游戏 Scene 与 UI HUD 使用普通 Component 组合：

```viso
export component GameScreen {
    input session: Handle<GameSession>;

    view {
        node root: Stack {
            GameViewport {
                session: session;
            }

            Column {
                Text {
                    text: format("Score: {}", session.score());
                }

                if session.is_paused() preserve "pause-menu" {
                    PauseMenu {
                        on resume {
                            session.resume();
                        }
                    }
                }
            }
        }
    }
}
```

UI Binding 对 Game Observable Handle 的读取必须通过 Schema 标记为 Reactive Query。高频 World 数据不应逐 Entity 直接驱动 Widget Tree；应通过 Snapshot/Observable 汇总。

---

## 110. 游戏热重载

Game Profile 支持两层热重载：

### 110.1 Logic-only Reload

- 替换 System Bytecode；
- 保留 World；
- 保留兼容 System State；
- 在 Tick Boundary 原子切换；
- 当前 Tick 使用其他实现完整执行，不允许半 Tick 混用版本。

### 110.2 World Rebuild Reload

- 在 Shadow World 执行新 Build Script/System；
- 运行 Schema、Smoke Tick 和预算检查；
- 成功后交换；
- 失败保持 Last-good World；
- 可选通过 Stable Entity Key 迁移玩家状态。

### 110.3 Shader Reload

- 后台编译新 Pipeline；
- Validation 成功后在 Frame Boundary 交换；
- 失败继续使用当前可用 Pipeline；
- 错误回传源码位置和 Backend 日志。

---

## 111. 游戏能力验收矩阵

| 能力         |                  语言支持 | Runtime/Profile 支持 | 结论       |
| ------------ | ------------------------: | -------------------: | ---------- |
| 固定 Tick    | `system + trait + action` |            Scheduler | 完整       |
| 持久状态     |            `system state` |          State Store | 完整       |
| 输入         |           Typed Value/API |         Input Mapper | 完整       |
| 物理         |         Typed Handle Call |       Physics Engine | 需 Runtime |
| 碰撞         |        Typed Action/Event |         Event Buffer | 完整       |
| Entity/ECS   |            Generic/Handle |                  ECS | 需 Runtime |
| HUD          |            Component/View |       Widget Runtime | 完整       |
| Shader       |             Shader Domain |          GPU Backend | 完整       |
| 热重载       |          Symbol/Migration |       Reload Runtime | 完整       |
| AI 生成      |    EBNF/Schema/Diagnostic |              CLI/LSP | 完整       |
| AAA 资产管线 |                    可调用 |           需专门工具 | 非语言本身 |

因此答案不是“DSL 自己就是游戏引擎”，而是“DSL 有足够语义承载游戏 Runtime，并且不需要牺牲类型和工具能力”。

---

# 第十三部分：编译器架构与逐构造 Lowering

## 112. 编译管线

```text
UTF-8 Source
-> Lexer Token Stream
-> Lossless CST
-> Syntax AST
-> Module Graph
-> Name-resolved AST
-> Typed HIR
-> Effect/Capability Check
-> Domain-specific IR
   - UI IR
   - Reactive IR
   - Behavior IR
   - Async IR
   - Resource IR
   - System IR
   - Shader IR
-> Optimization/Validation
-> Runtime Bytecode + Native Schema + GPU Programs
```

每层必须有稳定的数据结构版本和 Dump 格式，供测试、LSP 与 AI 工具使用。

---

## 113. Token 和 Lossless CST

Token 至少记录：

```text
kind
raw_text
byte_range
unicode_scalar_range
utf16_range
leading_trivia
trailing_trivia
lexical_error
```

CST 要求：

- 保留所有 Token、注释和空白；
- 允许 `ErrorNode` 和 `MissingToken`；
- Parser 遇到错误后同步到 `;`、`,`、`}` 或声明关键字；
- 一次编辑尽可能报告多个独立错误；
- Incremental Reparse 只替换受影响 Green Tree；
- Formatter 基于 CST/AST，不以正则重写源码；
- Macro/Template Expansion 不覆盖原 Source Origin。

---

## 114. AST

AST 只表达语法，不执行类型推断。

核心节点：

```text
AstModule
AstImport
AstRecord
AstEnum
AstTrait
AstImpl
AstComponent
AstSystem
AstMember
AstView
AstNode
AstBinding
AstHandler
AstStatement
AstExpression
AstPattern
AstShader
```

每个 AST Node 有：

```text
syntax_id
source_span
attributes
origin_chain
```

不得在 AST 中把：

- Property Binding 降成 Assignment；
- View For 降成普通 For；
- Event Handler 降成 Closure；
- Resource 降成普通 Record；
- Shader Function 降成 Host Function。

这些构造在 AST 层保持独立，避免语义重新依赖上下文猜测。

---

## 115. Symbol ID

```text
SymbolId = hash(
    package_id,
    module_path,
    declaration_kind,
    explicit_stable_name_or_canonical_path,
    generic_arity
)
```

要求：

- 与源码 Byte Offset 无关；
- 普通格式化不改变；
- 文件移动但 Module Path 不变时不改变；
- Explicit `@stable` 可以跨重命名保持；
- 同一 Package 内冲突是编译错误；
- Symbol ID Algorithm 有版本号；
- Hot Reload 和持久 Schema 都记录算法版本。

---

## 116. Typed HIR

HIR 节点必须包含：

```text
resolved_symbol
inferred_type
effect_class
capability_set
ownership_mode
reactive_reads
source_origin
constant_value_if_any
```

主要 HIR：

```text
HirComponent
HirSystem
HirCallable
HirState
HirComputed
HirResource
HirView
HirNode
HirBinding
HirEventHandler
HirExpression
HirPattern
HirShader
```

HIR 构建后，不允许存在：

- 未解析 Identifier；
- 未定型数值 Literal；
- 未确定 Method Candidate；
- 未确定 Event/Property/Slot Schema；
- 未确定 Effect Class；
- 未确定 Capability；
- 隐式 Dynamic。

---

## 117. Component Lowering

```text
AstComponent
-> resolve generic/interface symbols
-> ComponentSchema
-> StateLayout
-> ComputedGraph
-> CallableTable
-> EventSchema
-> SlotSchema
-> ViewFactory
-> MigrationDescriptor
```

输出概念结构：

```text
ComponentIr {
    symbol_id
    generic_params
    inputs[]
    states[]
    computed[]
    events[]
    slots[]
    callables[]
    view_factory
    migration
    capabilities
}
```

Input Default 被 Lower 为无实例依赖的 Const/Pure Thunk。State Initializer 被 Lower 为有序 Init Function。View 被 Lower 为 Reactive UI Factory。

---

## 118. State Lowering

```viso
state count: I64 = 0;
```

Lower：

```text
StateSlot {
    symbol_id: Counter::count
    type: I64
    init_fn: const 0
    revision: RuntimeCell
    persistence: component
    migration: exact_type
}
```

Assignment：

```viso
count += 1;
```

Lower：

```text
tmp0 = StateRead(count)
tmp1 = AddI64(tmp0, ConstI64(1))
StateWrite(transaction, count, tmp1)
```

State Write 不立即触发 Render；只记录 Write Set，提交阶段统一失效。

---

## 119. Computed Lowering

```viso
computed label: String = format("Count: {}", count);
```

Lower：

```text
ComputedNode {
    symbol_id
    result_type: String
    thunk: BehaviorFn
    static_dependencies: [State(count)]
    cache_policy: revision
}
```

若函数调用内部读取 State，Call Graph Analysis 把读取集合传播到 Computed。无法静态确定的 Reactive Query 插入 Runtime Tracking Guard。

---

## 120. Action Lowering

```text
Action Entry
-> BeginTransaction if no active transaction
-> Execute Behavior IR
-> Buffer Event/Native Commands
-> On success CommitTransaction
-> On failure RollbackTransaction
-> Return
```

Native Action 可以声明 `immediate` 或 `buffered`：

- `buffered` 在 Commit 后执行，失败策略由 Schema 定义；
- `immediate` 在 Transaction 内执行，必须提供可回滚或明确不可回滚标记；
- 文件、网络等不可回滚副作用不应在可能失败的 State Transaction 中作为 Immediate Action。

Compiler 对不可回滚调用发出诊断，要求移动到 Effect/Task。

---

## 121. View Node Lowering

```viso
node add_button: Button {
    text: label;
}
```

Lower：

```text
UiNodeTemplate {
    node_symbol_id: Counter::view::add_button
    component_type: Button
    identity_kind: Named
    properties: [
        Binding {
            property_id: Button::text
            value_fn: read Computed(label)
            dependencies: [Computed(label)]
            invalidation: Layout + Paint + Semantics
        }
    ]
}
```

匿名 Node 使用 Structural Symbol ID；`@stable` 覆盖默认生成策略。

---

## 122. Property Binding Lowering

```viso
width = panel_width;
```

Lower 为：

```text
ReactiveBinding {
    target_node
    property_id
    eval_fn
    source_dependencies
    equality_policy
    invalidation_mask
}
```

首次 Mount 执行 Eval。后续仅在依赖 Revision 变化时执行。新值与已提交值按 Property Schema Equality Policy 比较；相同则不提交 SetProperty。

---

## 123. Two-way Binding Lowering

```viso
bind value <=> draft using TextConverter;
```

Lower 为两条带 Origin Token 的单向边：

```text
ModelToView {
    read: draft
    convert: TextConverter::to_view
    write_property: value
    origin: binding_id
}

ViewToModel {
    event: value_changed
    convert: TextConverter::to_model
    write_state: draft
    ignore_origin: binding_id
}
```

Converter Error 必须有 Schema 策略：拒绝更新、显示 Validation State 或产生 Event。不得静默写入错误值。

---

## 124. Event Handler Lowering

```viso
on pointer_down(event) {
    begin_drag(event.position);
}
```

Lower：

```text
HandlerDescriptor {
    event_id
    phase
    payload_pattern
    action_fn
    source_span
}
```

Action Function 自动接受隐藏参数：

```text
ComponentInstance
EventContext
Transaction
```

Pattern 不匹配时返回 `HandlerSkipped`，不视为错误。

---

## 125. Conditional Lowering

无 Preserve：

```text
ConditionalIr {
    condition_fn
    then_factory
    else_factory
    retention: DestroyOnExit
}
```

带 Preserve：

```text
retention: PreserveCache("user-panel")
```

Compiler 对两个 Branch 分配不同 Identity Namespace，避免相同结构位置误迁移。

---

## 126. Keyed List Lowering

```viso
for item in items key item.id {
    TodoRow { item: item; }
}
```

Lower：

```text
RepeatIr {
    collection_fn
    item_pattern
    key_fn
    body_factory
    duplicate_key_policy
}
```

运行时 Diff：

```text
old keys -> key:index map
new keys -> validate unique
reuse/move existing child by key
create new child for unseen key
delete old child not present
```

Key Function 在 Item Lexical Scope 内求值。Key 不进入显示 Property，除非源码显式绑定。

---

## 127. Match Lowering

- Enum Match 优先 Lower 为 Variant Dispatch Table；
- Integer/Char Dense Literal 可以 Lower 为 Jump Table；
- Range/Guard 使用 Decision Tree；
- Pattern Binding 在成功路径创建 SSA Value；
- Exhaustiveness 在 HIR 完成；
- View Match 每个 Arm Lower 为独立 UI Factory 和 Identity Namespace。

---

## 128. Closure Lowering

```text
ClosureExpr
-> Capture Analysis
-> Environment Record
-> Invoke Function
-> Closure Value { code_id, env_handle, kind }
```

`move` 决定 Environment Field 的 ownership。跨热重载长期 Closure 记录 Code Version；不兼容时取消或重建，不能调用已卸载代码。

---

## 129. Task Lowering

```text
Task AST
-> Async HIR
-> Suspension Point Analysis
-> State Machine
-> Cancellation Checks
-> Capture Snapshot
-> Executor Descriptor
```

每个 `await` 前后插入：

```text
CheckCancelled
CheckBudget
StoreContinuationState
```

Completion 产生 Typed Message 返回所属 UI/System Scope，不直接获得可变 State 指针。

---

## 130. Resource Lowering

```text
ResourceIr {
    symbol_id
    value_type
    error_type
    key_fn
    loader_task
    policies[]
    scope
    cache_schema_version
}
```

Compiler 先规范化 Policy 顺序，但必须保留冲突诊断。例如同时出现 `keep_latest` 和 `parallel(4)` 若 Schema 标为互斥则报错。

---

## 131. System Lowering

```text
SystemIr {
    symbol_id
    implemented_traits
    scheduler_hooks
    inputs
    states
    actions
    ordering_constraints
    thread_domain
    determinism_class
}
```

Trait 实现将 Action 绑定到 Scheduler Hook。Game Profile 可以把 `FixedUpdate::fixed_update` 注册到固定 Tick 阶段，而通用 Compiler 不硬编码函数名。

---

## 132. Operator Lowering

内建标量优先 Lower 为 Typed Opcode：

```text
I64 + I64 -> AddI64
F32 * F32 -> MulF32
Bool && Bool -> BranchShortCircuit
```

用户类型通过 Trait：

```text
+   Add<Rhs, Output>
-   Sub<Rhs, Output>
*   Mul<Rhs, Output>
/   Div<Rhs, Output>
%   Rem<Rhs, Output>
==  Eq
<   Ord/PartialOrd
```

为保证可读性和编译器可预测性：

- Viso 1.0 不允许用户声明新 Operator Token；
- Operator Trait 实现不得改变短路行为；
- `&&`、`||`、`??` 和 `?` 不能重载为普通 eager Call；
- Assignment Operator Lower 为 Read-Operate-Write，不返回值。

---

## 133. 求值顺序

Viso 规定所有 Host Expression 的求值顺序为从左到右：

```text
receiver
then generic/type resolution（编译期）
then positional args left-to-right
then named args in source order
then call
```

Binary Operator 左操作数先求值。Short-circuit Operator 只在需要时求值右侧。Record Field Initializer 按源码顺序求值，`..base` 最后求值。

Shader Expression 的纯语义允许 Backend 重排，但不能改变可观察数值规则超过 Shader Precision Profile。

---

## 134. Source Map 和 Origin Chain

每条 Bytecode/IR Instruction 至少映射：

```text
primary source span
source origin kind
definition origin
template expansion callsite
macro/schema generated origin
inlined function origin
```

其中 `source origin kind` 至少区分两种宿主来源，对应 §22.1 的三个入口：

```text
vs-file span      来自外部 .vs 文件（view!(path) / package source）
rust-macro span   来自 Rust 宿主 ui! / component! 宏调用
```

- 来自 `ui!` / `component!` 的 span 必须能回指 Rust 源文件中的宏调用位置（Rust macro span），不能只指向展开后的中间产物；
- 来自 `.vs` 的 span 指向 `.vs` 文件内的字符区间；
- Hot Reload 只重编 `.vs` 时，`rust-macro` 来源的节点保持原 origin 不变。

诊断展示：

1. 用户最接近的 Primary Span（`.vs` 文件位置或 Rust 宏调用位置）；
2. “由此 Template 展开”；
3. “属性在此 Schema 声明”；
4. 必要时展示 Native/Shader Backend Origin。

AI JSON 诊断不得只返回生成文件路径。

---

## 135. 增量编译

Dependency Key：

```text
file content hash
module interface hash
schema hash
compiler version
language version
target profile
capability profile
shader backend set
```

修改普通 Action Body 不应重新编译无关 Shader。修改 Component Input 类型必须使所有调用方重新检查。修改 Style Property 只重建受影响 Style/UI IR。

---

# 第十四部分：AI Vibe Coding 合同

## 136. 目标

AI 不应依赖“看起来像对的”语法。标准循环：

```text
查询 Schema
-> 生成最小改动
-> Formatter
-> Parser/Type/Effect Check
-> 读取 JSON Diagnostic
-> 应用结构化 Fix
-> 运行目标测试
-> 检查 UI/Game Snapshot
```

---

## 137. CLI 合同

必须提供：

```text
cargo viso fmt <paths>
cargo viso check <package> --message-format=json
cargo viso schema <symbol> --format=json
cargo viso explain <error-code> --format=json
cargo viso ast <file> --format=json
cargo viso hir <file> --format=json
cargo viso ir <file> --domain=ui|behavior|shader|system --format=json
cargo viso migrate <file> --from=makepad-script --dry-run
cargo viso test <package>
cargo viso preview <component> --snapshot=<path>
cargo viso game test <scenario> --frames=<n> --seed=<seed>
```

命令退出码：

```text
0 成功
1 源码/测试错误
2 CLI 使用错误
3 工具链/环境错误
4 Runtime/Backend 故障
```

---

## 138. JSON Diagnostic Schema

```json
{
  "schema_version": "1.0",
  "severity": "error",
  "code": "E3001",
  "message": "`child` is not part of Viso DSL 1.0",
  "primary": {
    "file": "src/app.vs",
    "byte_start": 422,
    "byte_end": 427,
    "line": 18,
    "column_utf16": 9
  },
  "related": [],
  "expected": ["anonymous node", "node <name>: <Component>"],
  "actual": "child",
  "notes": ["Anonymous child nodes are written directly as `Column { ... }`."],
  "fixes": [
    {
      "title": "Remove `child`",
      "applicability": "machine-applicable",
      "edits": [
        {
          "file": "src/app.vs",
          "byte_start": 422,
          "byte_end": 428,
          "replacement": ""
        }
      ]
    }
  ]
}
```

要求：

- Error Code 稳定；
- Range 同时可提供 Byte/UTF-16；
- Fix 有 Applicability；
- 多文件 Fix 可原子应用；
- Schema Version 明确；
- 不把编译器 Stack Trace 放进用户 Message；
- AI 可以只按 Error Code 查询长期说明。

---

## 139. Schema 查询

```bash
cargo viso schema viso::widgets::Button --format=json
```

至少返回：

```json
{
  "kind": "component",
  "symbol": "viso::widgets::Button",
  "version": "1.0",
  "inputs": [
    {
      "name": "text",
      "type": "String",
      "required": false,
      "default": "",
      "affects": ["layout", "paint", "semantics"]
    }
  ],
  "events": [
    {
      "name": "click",
      "payload": "ClickEvent",
      "bubbles": true,
      "cancelable": true
    }
  ],
  "slots": [],
  "parts": [],
  "capabilities": []
}
```

AI 在使用未知 Component/Property/Event 前必须查询 Schema 或依赖已锁定版本的本地索引。

---

## 140. Formatter 唯一输出

Formatter 必须规范：

- 4 空格缩进；
- 简单语句分号；
- Trailing Comma 用于多行列表、参数和 Record；
- 一个空格围绕二元 Operator；
- `on event {}` 永不变成箭头；
- 永不输出 `child`；
- 类型使用 canonical 名称 `F32/F64`；
- Resource 永远使用多行配置 Block；
- Import 按 Module 分组并稳定排序；
- Attribute 保持声明关联；
- 注释尽量保持最近语义节点。

Parser 接受的所有合法程序经 Formatter 后必须再次 Parse 为等价 AST。

---

## 141. AI 生成规则

AI 应：

1. 先运行 `schema`；
2. 只编辑完成任务所需文件；
3. 使用唯一规范语法；
4. 不创造属性、事件、单位或 Capability；
5. 动态列表始终写 Key；
6. 异步工作使用 Task/Resource；
7. 状态变化依赖自动响应式，不手工全树 Render；
8. 游戏逻辑使用 System/Trait；
9. Shader 使用定宽类型；
10. 每次改动后运行 Formatter 和 Check；
11. 根据 JSON Fix 修复；
12. 运行最小测试和 Snapshot。

AI 禁止：

- 全仓库盲目字符串替换；
- 为绕过类型错误改成 Dynamic/String；
- 忽略 Capability 诊断；
- 删除失败测试；
- 在 View 内执行副作用；
- 用索引替代真实 Stable Key；
- 在错误时显示空白替代 Last-good UI；
- 修改 EBNF 却不升级 Language Version 和测试。

---

## 142. Vibe Coding 的最小上下文包

工具应向 AI 提供：

```text
language version
package manifest
当前文件
直接依赖的公开 Schema
相关诊断
目标 Component Snapshot
最近一次 Last-good Revision
允许的 Capability
变更文件预算
```

不应把整个大型仓库无差别塞给模型。Schema 和 HIR 摘要比未经筛选的源码更稳定。

---

## 143. 结构化编辑

除普通文本 Patch 外，Compiler/LSP 应支持：

```text
AddImport
CreateComponent
AddInput
AddState
AddAction
InsertNode
SetPropertyBinding
AttachEventHandler
WrapInKeyedFor
ConvertTaskToResource
AddTraitImpl
```

结构化编辑基于 Syntax ID 和 Symbol ID，避免 AI 因行号漂移改错位置。

---

# 第十五部分：从 Makepad 当前 Script 迁移

## 144. 迁移原则

Viso 的迁移输入是 Rust 源码中的 `script_mod!`、`ScriptVm`、`App::from_script_mod`、Widget/Native 注册和脚本 namespace 关系。迁移器必须先理解 Rust AST 与宏 token stream，再建立 Makepad Script Semantic Model，最后生成 Viso Rust/`.vs` 源码。禁止用正则全局替换 Assignment-family 符号。

```text
Rust AST / macro token stream
-> Makepad Script parser
-> Script/registration semantic graph
-> Migration IR
-> Viso Rust AST + Viso DSL AST
-> Formatter
-> Type/Schema/Effect Check
```

无法确定的语义必须生成结构化诊断，不得猜测。

---

## 145. 迁移分类

```text
Auto      可以证明局部语义等价的机械转换
Assisted  生成 Viso skeleton，并附精确诊断/TODO
Manual    生命周期、所有权、绘制、事件或架构模型需要重新实现
```

`--apply` 只允许执行幂等、高置信度的 Auto rewrite。迁移工具的目标是可信分析与规划，不以制造运行时兼容层提高自动化率。

---

## 146. 常见语义映射

| Makepad 当前 Script / Runtime | Viso 1.0 |
| --- | --- |
| `property: value` | `property: expression;` |
| `name := Type { ... }` | `node name: Type { ... }` |
| `object +: { ... }` | 根据语义转换为 `style` / `override` / `replace` / Record Update；无法证明时 Assisted |
| `#(rust_expr)` | 导入 Rust 生成的 Native/Component Schema |
| `mod.widgets.*` / 注册顺序 | 编译期 import/module graph |
| Script 持久变量 | Component/System `state`，由生命周期分析决定归属 |
| 手工 render/update | Reactive Binding + targeted invalidation；命令式特殊绘制需 Manual |
| `game.on_tick(...)` / 宿主 tick callback | `system` + `FixedUpdate` Profile |
| 长期闭包捕获游戏状态 | `system state` 或明确 Runtime Handle |
| 动态 Native method/property | Typed Native Schema；只有显式 dynamic surface 可以保留动态调用 |
| Shader 数据布局约定 | Viso Shader Descriptor ABI |

---

## 147. 迁移器必须恢复的依赖图

迁移器需要分析：

```text
script_mod! 定义与调用
ScriptVm 创建、传递与生命周期
makepad_widgets::script_mod(vm) 等基础模块注册
Struct::register_widget(vm) / Native 注册
App::from_script_mod(...) 入口
mod namespace 写入与读取
组件/Shader/资源引用
事件 callback / closure capture
手工 render/update 调用
```

迁移结果必须能解释“为什么某一段被归类为 Auto/Assisted/Manual”。

---

## 148. Runtime 兼容层禁止

Viso production runtime 不包含：

```text
LegacyWidgetHost
WidgetRef compatibility wrapper
Makepad Cx adapter
Makepad draw/event lifecycle adapter
dual Makepad/Viso widget runtime
```

Makepad-aware 代码只存在于迁移工具、fixture、characterization test 和迁移文档中。原则是：**迁移语义、行为、算法、测试和性能基线，不迁移 Makepad runtime architecture。**

---

## 149. 迁移 canary

至少选择一个真实 Makepad crate 作为迁移 canary，并记录：

```text
script_mod! recognition rate
ScriptVm/module graph recovery
property/widget/shader mapping coverage
Auto / Assisted / Manual 分布
characterization-test parity
performance parity / regression
人工迁移时间
```

在 canary 数据出现之前，不对自动迁移比例做承诺。

---

# 第十六部分：实现阶段与验收

## 151. 实现阶段

### P0：语言核心

- Lexer；
- Lossless CST；
- Module/Import；
- Record/Enum；
- Component/Input/State/Computed/Action/View；
- Node、Property、Block Event；
- Expression/Operator/Pattern；
- Type Inference；
- Formatter；
- JSON Diagnostic。

### P1：响应式和结构 UI

- Reactive Graph；
- Transaction；
- Keyed List；
- Conditional Preserve；
- Slot；
- UI Diff/Patch；
- Last-good Hot Reload。

### P2：行为与扩展

- Effect；
- Task；
- Resource；
- Trait/Generic；
- System；
- Typed Native Schema；
- Capability。

### P3：高级能力

- Template/Part/Style/Theme；
- Shader IR 和 ABI；
- Game Profile；
- Full Migration；
- AI Structured Edit；
- Cross-backend Validation。

每一阶段都必须通过规范、Parser Golden、Formatter、迁移器和测试共同锁定已声明为 stable 的语义；任何 breaking 变更必须先更新规范与 ADR。

---

## 152. Parser 验收

必须包括：

- 每个 EBNF Production 的正例和反例；
- View/Style Property `:`、Record Field `:` 与行为 Assignment `=` 的上下文区分；
- Generic `<...>` 与比较 Operator 区分；
- Generic Type Argument 与显式 `const` Argument 区分；
- Generic Call 和 Generic Record Constructor 必须使用 Turbofish；
- Control Head 中未加括号 Record Expression 被拒绝；
- Block Item 起始的 `if`/`match` 与 Tail Expression 分类固定；
- Closure `||` 与逻辑或区分；
- Unit `%` 与 Modulo `%` 区分，包括 `50%`、`50%3` 和 `50 % 3`；
- Numeric Separator、Escape、Raw String Hash 数量的边界测试；
- `=>` 只在 Match；
- `<=>` 只在 Bind；
- `child` 定向迁移错误；
- Resource 重复 Item；
- 深度和 Token 数预算；
- 未闭合字符串/注释/Block 恢复；
- Unicode Identifier 和 Confusable；
- Incremental Reparse 等价全量 Parse。

Fuzz：

```text
Lexer never panics
Parser never panics
Formatter(parse(x)) never panics
parse(format(parse(valid_x))) AST-equivalent
```

---

## 153. 类型验收

- 所有安全拓宽；
- 所有禁止隐式转换；
- F64 不进 Shader；
- `Float` 未定义；
- Generic Bound；
- Trait 歧义；
- StableKey 派生；
- Float Key 拒绝；
- Match Exhaustiveness；
- Closure Expected Type；
- Effect Call Matrix；
- Capability 传播；
- Native Ownership；
- State 前向引用拒绝；
- Computed 无环前向依赖接受；
- Computed Cycle 输出路径。

---

## 154. 运行时验收

- 一个 Action 多次写同 State 只触发一次 Commit；
- Paint-only 不重算 Layout；
- Event Propagation 顺序稳定；
- Keyed Reorder 保留 State/Focus；
- Duplicate Key 原子拒绝；
- Preserved Branch 保留并可逐出；
- Task 在 Unmount/Key Change 时取消；
- 过期 Task 结果不能覆盖新 Key；
- Effect Cleanup 恰好一次；
- Hot Reload 失败保留 Last-good；
- State Migration 成功/失败路径；
- Native Panic 被隔离；
- Capability Denied 不触发宿主调用；
- Budget 超限可恢复。

---

## 155. 游戏验收

- 固定 Seed + Input Tape 结果可重放；
- 60Hz Tick 不依赖显示刷新率；
- Catch-up Step 有上限；
- System Order 稳定；
- Command Buffer 合并确定；
- Logic Hot Reload 在 Tick Boundary；
- 错误版本不替换 Last-good；
- Entity Key 迁移；
- Shader Reload 失败保留当前可用 Pipeline；
- Headless Simulation 可输出 Entity Snapshot；
- CPU Reference Shader 与至少一个 GPU Backend Golden Image 在容差内一致。

---

## 156. 人类可用性验收

至少进行以下任务测试：

1. 新手在只阅读十分钟 Quick Start 后完成 Counter；
2. 添加表单与双向绑定；
3. 添加 Keyed Todo List；
4. 添加异步搜索 Resource；
5. 创建带 Slot 的复用 Component；
6. 编写一个 FixedUpdate Game System；
7. 修复一个 Compiler Diagnostic；
8. 执行 Hot Reload 并保留输入焦点。

记录：

```text
完成率
完成时间
语法错误次数
需要查 Schema 次数
错误修复成功率
概念混淆点
```

若 `node`/匿名节点、Action/Task、State/Computed 或 Preserve/Key 的混淆率持续偏高，应先改文档和诊断，不轻易新增第二套语法捷径。

---

## 157. AI 生成验收

建立冻结测试集：

```text
100 个基础 UI Prompt
100 个状态/列表 Prompt
50 个异步 Resource Prompt
50 个组件抽取 Prompt
50 个 Shader Prompt
50 个游戏逻辑 Prompt
50 个迁移 Prompt
```

指标：

```text
First-pass Parse Rate
First-pass Type-check Rate
平均修复轮次
Diagnostic-guided Repair Rate
不存在属性幻觉率
Key/Capability 合规率
Snapshot 语义正确率
最小改动率
```

AI 成功不能只看“能编译”；还需 Snapshot、Event Trace 或 Game Tape 验证行为。

---

## 158. Definition of Done

Viso DSL 1.0 的首个可交付实现必须满足：

- 规范中的核心 EBNF 与 Parser 测试一一对应；
- 保留字清单由 Lexer 测试锁定；
- 运算符优先级由 Golden AST 锁定；
- `child`、事件箭头、`Float` 和非规范 Resource 写法均有定向诊断；
- State 前向引用规则唯一；
- `preserve` 与 `key` AST 分离；
- Component、State、Computed、Action、View 可运行；
- 自动响应式不需要手工 Render；
- Keyed List 能保留身份；
- Last-good Hot Reload 工作；
- JSON Diagnostic 和 Schema 可供 AI 使用；
- 至少一个 Game System 在固定 Tick 运行；
- 至少一个 Shader 通过安全 ABI 在两个 Backend 运行；
- 无已知 Parser Panic、VM 越界、Handle UAF 或 GPU Layout 依赖字段顺序的问题。

---

# 附录 A：规范性合并 EBNF

## A.1 权威性

本附录把正文中分散的 Production 合并为单一 Parser 合同。若正文示例、说明性片段与本附录发生纯语法层面的冲突，以本附录为准；静态和运行时语义仍以正文对应章节为准。

以下终结 Token 由 Lexer 提供：

```text
IDENT
INT_LITERAL
FLOAT_LITERAL
STRING_LITERAL
CHAR_LITERAL
COLOR_LITERAL
UNIT_LITERAL
DOC_COMMENT
END_OF_FILE
```

Trivia 不进入普通 Production，但保留在 Lossless CST。

---

## A.2 Compilation Unit 和 Declaration

Parser 有三个规范入口（见 §22.1），共享全部后续语义：

```ebnf
(* view!(path) / package source *)
CompilationUnit
    ::= ImportDecl* TopLevelDecl* END_OF_FILE

(* ui! { ... } body — 见 §A.8 ViewStructureItem *)
ViewFragment
    ::= ViewStructureItem* END_OF_FILE

(* component! { ... } body *)
ComponentEntry
    ::= ImportDecl* ComponentDecl END_OF_FILE

ModulePath
    ::= IDENT ( "::" IDENT )*

ImportDecl
    ::= "import" ModulePath ImportSuffix? ";"

ImportSuffix
    ::= "as" IDENT
     |  "::" "{" ImportItem ( "," ImportItem )* ","? "}"

ImportItem
    ::= IDENT ( "as" IDENT )?

TopLevelDecl
    ::= Attribute* "export"? DeclCore

DeclCore
    ::= ComponentDecl
     |  SystemDecl
     |  RecordDecl
     |  EnumDecl
     |  TraitDecl
     |  ImplDecl
     |  TypeAliasDecl
     |  ConstDecl
     |  FunctionDecl
     |  ActionDecl
     |  TaskDecl
     |  TemplateDecl
     |  StyleDecl
     |  ThemeDecl
     |  ShaderDecl
     |  NativeDecl

Attribute
    ::= "@" Path ( "(" AttributeArgs? ")" )?

AttributeArgs
    ::= AttributeArg ( "," AttributeArg )* ","?

AttributeArg
    ::= Expression
     |  IDENT ":" Expression
```

---

## A.3 Path、Generic、Type 和约束

```ebnf
Path
    ::= IDENT ( "::" IDENT )*

TypePath
    ::= TypePathSegment ( "::" TypePathSegment )*

TypePathSegment
    ::= IDENT GenericArgs?

GenericArgs
    ::= "<" GenericArg ( "," GenericArg )* ","? ">"

GenericArg
    ::= Type
     |  "const" ConstExpression

GenericParams
    ::= "<" GenericParam ( "," GenericParam )* ","? ">"

GenericParam
    ::= TypeGenericParam
     |  ConstGenericParam

TypeGenericParam
    ::= IDENT ( ":" TraitBounds )? ( "=" Type )?

ConstGenericParam
    ::= "const" IDENT ":" Type ( "=" ConstExpression )?

TraitBounds
    ::= TypePath ( "+" TypePath )*

ImplementsClause
    ::= "implements" TraitBound ( "+" TraitBound )*

TraitBound
    ::= TypePath

WhereClause
    ::= "where" WherePredicate ( "," WherePredicate )* ","?

WherePredicate
    ::= Type ":" TraitBounds

Type
    ::= FunctionType
     |  TupleType
     |  ArrayType
     |  SliceType
     |  TraitObjectType
     |  "Self"
     |  TypePath

FunctionType
    ::= ( "Fn" | "FnMut" | "ActionFn" | "TaskFn" )
        "(" TypeList? ")" "->" Type

TupleType
    ::= "(" Type "," ( Type ( "," Type )* ","? )? ")"

ArrayType
    ::= "[" Type ";" ConstExpression "]"

SliceType
    ::= "[" Type "]"

TraitObjectType
    ::= "dyn" TraitBounds

TypeList
    ::= Type ( "," Type )* ","?
```

```ebnf
ConstExpression
    ::= Expression

DefaultExpression
    ::= Expression

InitExpression
    ::= Expression
```

三者共享 Expression Syntax，但分别通过 Const Checker、Default Checker 和 State Init Checker 限制可执行子集。

---

## A.4 Record、Enum、Trait 和 Impl

```ebnf
RecordDecl
    ::= "record" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" RecordField* "}"

RecordField
    ::= Attribute* IDENT ":" Type ( "=" ConstExpression )? ";"

EnumDecl
    ::= "enum" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" EnumVariant* "}"

EnumVariant
    ::= Attribute* IDENT VariantPayload? ";"

VariantPayload
    ::= "(" TypeList? ")"
     |  "{" RecordField* "}"

TraitDecl
    ::= "trait" IDENT GenericParams? ( ":" TraitBounds )? WhereClause?
        "{" TraitMember* "}"

TraitMember
    ::= Attribute*
        ( FunctionSignature ";"
        | ActionSignature ";"
        | TaskSignature ";"
        | AssociatedTypeDecl
        | AssociatedConstDecl )

AssociatedTypeDecl
    ::= "type" IDENT ( ":" TraitBounds )? ";"

AssociatedConstDecl
    ::= "const" IDENT ":" Type ";"

ImplDecl
    ::= "impl" GenericParams? ImplTarget WhereClause?
        "{" ImplMember* "}"

ImplTarget
    ::= TypePath "for" Type
     |  Type

ImplMember
    ::= Attribute*
        ( FunctionDecl
        | ActionDecl
        | TaskDecl
        | AssociatedTypeImpl
        | AssociatedConstImpl )

AssociatedTypeImpl
    ::= "type" IDENT "=" Type ";"

AssociatedConstImpl
    ::= "const" IDENT ":" Type "=" ConstExpression ";"

TypeAliasDecl
    ::= "type" IDENT GenericParams? "=" Type ";"

ConstDecl
    ::= "const" IDENT ":" Type "=" ConstExpression ";"
```

---

## A.5 Callable 和 Capability

```ebnf
ParameterList
    ::= ( Parameter ( "," Parameter )* ","? )?

Parameter
    ::= "mut"? IDENT ":" Type ( "=" DefaultExpression )?

ReturnType
    ::= ( "->" Type )?

CapabilityClause
    ::= "requires" "{" CapabilityPath
        ( "," CapabilityPath )* ","? "}"

CapabilityPath
    ::= ModulePath

FunctionDecl
    ::= "fn" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? Block

FunctionSignature
    ::= "fn" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause?

ActionDecl
    ::= "action" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? Block

ActionSignature
    ::= "action" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause?

TaskDecl
    ::= "task" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? Block

TaskSignature
    ::= "task" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause?
```

`DefaultExpression` 是通过 Pure/Determinism Checker 的 Expression。

---

## A.6 Component、System 和成员

```ebnf
ComponentDecl
    ::= "component" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" ComponentMember* "}"

ComponentMember
    ::= Attribute*
        ( InputDecl
        | StateDecl
        | ComputedDecl
        | EventDecl
        | SlotDecl
        | ConstDecl
        | FunctionDecl
        | ActionDecl
        | TaskDecl
        | EffectDecl
        | ResourceDecl
        | NativeMemberDecl
        | ViewDecl )

SystemDecl
    ::= "system" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" SystemMember* "}"

SystemMember
    ::= Attribute*
        ( InputDecl
        | StateDecl
        | ComputedDecl
        | ConstDecl
        | FunctionDecl
        | ActionDecl
        | TaskDecl
        | EffectDecl
        | ResourceDecl
        | NativeMemberDecl )

InputDecl
    ::= "input" IDENT ":" Type ( "=" DefaultExpression )? ";"

StateDecl
    ::= "state" IDENT ( ":" Type )? "=" InitExpression ";"

ComputedDecl
    ::= "computed" IDENT ( ":" Type )? "=" Expression ";"

EventDecl
    ::= "event" IDENT "(" EventParameterList ")" ";"

EventParameterList
    ::= ( EventParameter ( "," EventParameter )* ","? )?

EventParameter
    ::= IDENT ":" Type

SlotDecl
    ::= "slot" IDENT ":" Type ( "=" SlotDefault )? ";"

SlotDefault
    ::= "None" | "empty"
```

`InitExpression` 是 Expression，经 §42 初始化 Checker 验证；`empty` 是只在 Slot Default 位置识别的上下文词。

---

## A.7 Effect、Resource 和 Start

```ebnf
EffectDecl
    ::= "effect" IDENT EffectDependencies?
        ( "run" EffectRunPolicy )?
        "{" Statement* CleanupClause? "}"

EffectDependencies
    ::= "when" "(" ExpressionList ")"

EffectRunPolicy
    ::= Path

CleanupClause
    ::= "cleanup" Block

ResourceDecl
    ::= "resource" IDENT ":" Type
        "{" ResourceItem* "}"

ResourceItem
    ::= "load" "=" Expression ";"
     |  "key" "=" Expression ";"
     |  "policy" "=" PolicyList ";"
     |  "scope" "=" Expression ";"

PolicyList
    ::= "[" ( Expression ( "," Expression )* ","? )? "]"

StartStatement
    ::= "start" Expression ( "as" IDENT )?
        StartHandlerBlock? ";"

StartHandlerBlock
    ::= "{" StartHandler* "}"

StartHandler
    ::= "policy" "=" PolicyList ";"
     |  "success" "(" Pattern ")" Block
     |  "error" "(" Pattern ")" Block
     |  "cancelled" Block
```

Start 的首个 Expression 必须在 HIR 中解析为 Task Call；纯语法不通过无限 Lookahead 判断 Call Kind。

---

## A.8 View 和节点

```ebnf
ViewDecl
    ::= "view" ViewBlock

ViewBlock
    ::= "{" ViewStructureItem* "}"

ViewStructureItem
    ::= Attribute*
        ( NamedNode
        | AnonymousNode
        | PartNode
        | ViewIf
        | ViewFor
        | ViewMatch
        | TemplateUse )

NamedNode
    ::= "node" IDENT ":" ComponentType NodeBody

AnonymousNode
    ::= ComponentType NodeBody

PartNode
    ::= "part" IDENT ":" ComponentType NodeBody

ComponentType
    ::= TypePath

NodeBody
    ::= "{" NodeMember* "}"

NodeMember
    ::= Attribute*
        ( PropertyBinding
        | TwoWayBinding
        | EventHandler
        | FillClause
        | NamedNode
        | AnonymousNode
        | PartNode
        | ViewIf
        | ViewFor
        | ViewMatch
        | TemplateUse
        | PartOverride
        | PartReplace )

PropertyBinding
    ::= PropertyPath ":" Expression ";"

PropertyPath
    ::= IDENT ( "." IDENT )*

TwoWayBinding
    ::= "bind" PropertyPath "<=>" AssignablePath
        ( "using" TypePath )? ";"

EventHandler
    ::= "on" EventPhase? IDENT ( "(" Pattern ")" )? Block

EventPhase
    ::= "capture" | "bubble"

FillClause
    ::= "fill" IDENT ViewBlock

ViewIf
    ::= "if" HeadExpression ( "preserve" STRING_LITERAL )?
        ViewBlock ( "else" ( ViewIf | ViewBlock ) )?

ViewFor
    ::= "for" Pattern "in" HeadExpression "key" HeadExpression ViewBlock

ViewMatch
    ::= "match" HeadExpression "{" ViewMatchArm
        ( "," ViewMatchArm )* ","? "}"

ViewMatchArm
    ::= Pattern ( "if" Expression )? "=>" ViewBlock

PartOverride
    ::= "override" "part" IDENT
        "{" PartOverrideItem* "}"

PartOverrideItem
    ::= PropertyBinding | TwoWayBinding | EventHandler

PartReplace
    ::= "replace" "part" IDENT ViewBlock
```

`ViewBlock` 的 Cardinality 由 HIR 检查。它不是普通 `Block`，因此不允许 Statement 或 Tail Expression。

---

## A.9 Template、Style、Theme

```ebnf
TemplateDecl
    ::= "template" IDENT GenericParams?
        "(" ParameterList ")" WhereClause?
        "{" TemplateMember+ "}"

TemplateMember
    ::= SlotDecl | ConstDecl | FunctionDecl | ViewDecl

TemplateUse
    ::= "use" TypePath "(" ArgumentList ")"
        TemplateUseBody? ";"

TemplateUseBody
    ::= "{" ( FillClause | PartOverride | PartReplace )* "}"

StyleDecl
    ::= "style" IDENT "for" ComponentType StyleBaseClause?
        "{" StyleItem* "}"

StyleBaseClause
    ::= ":" TypePath ( "+" TypePath )*

StyleItem
    ::= PropertyBinding | StyleWhen

StyleWhen
    ::= "when" StateSelector "{" PropertyBinding* "}"

StateSelector
    ::= SelectorOr

SelectorOr
    ::= SelectorAnd ( "||" SelectorAnd )*

SelectorAnd
    ::= SelectorUnary ( "&&" SelectorUnary )*

SelectorUnary
    ::= "!"? ( IDENT | "(" StateSelector ")" )

ThemeDecl
    ::= "theme" IDENT ( ":" TypePath )?
        "{" ThemeItem* "}"

ThemeItem
    ::= ConstDecl
     |  IDENT "=" Expression ";"
```

---

## A.10 Native 和 Shader

```ebnf
NativeDecl
    ::= "native" NativeItem

NativeMemberDecl
    ::= "native" NativeItem

NativeItem
    ::= NativeFunction
     |  NativeAction
     |  NativeTask
     |  NativeTypeDecl

NativeFunction
    ::= "fn" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? ";"

NativeAction
    ::= "action" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? ";"

NativeTask
    ::= "task" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? ";"

NativeTypeDecl
    ::= "type" IDENT GenericParams?
        ( ":" TraitBounds )? WhereClause? ";"

ShaderDecl
    ::= "shader" IDENT GenericParams?
        "{" ShaderMember* "}"

ShaderMember
    ::= ShaderUniform
     |  ShaderInstance
     |  ShaderVarying
     |  ShaderTexture
     |  ShaderSampler
     |  ShaderFunction
     |  VertexEntry
     |  FragmentEntry
     |  ComputeEntry

ShaderUniform
    ::= "uniform" IDENT ":" ShaderType ";"

ShaderInstance
    ::= "instance" IDENT ":" ShaderType ";"

ShaderVarying
    ::= "varying" IDENT ":" ShaderType ";"

ShaderTexture
    ::= "texture" IDENT ":" TypePath ";"

ShaderSampler
    ::= "sampler" IDENT ":" TypePath ";"

ShaderFunction
    ::= "fn" IDENT "(" ShaderParameterList ")"
        "->" ShaderType Block

VertexEntry
    ::= "vertex" "(" ShaderParameterList ")"
        "->" ShaderType Block

FragmentEntry
    ::= "fragment" "(" ShaderParameterList ")"
        "->" ShaderType Block

ComputeEntry
    ::= "compute" "(" ShaderParameterList ")"
        "->" ShaderType Block

ShaderParameterList
    ::= ( ShaderParameter ( "," ShaderParameter )* ","? )?

ShaderParameter
    ::= IDENT ":" ShaderType

ShaderType
    ::= TypePath
```

Shader HIR Checker 把 `TypePath` 限制在 §98 的 Closed Type Set 及显式 `@shader_value` 类型。

---

## A.11 Block 和 Statement

```ebnf
Block
    ::= "{" Statement* TailExpression? "}"

TailExpression
    ::= Expression

Statement
    ::= Attribute* StatementCore

StatementCore
    ::= LetStatement
     |  AssignmentStatement
     |  ExpressionStatement
     |  ReturnStatement
     |  BreakStatement
     |  ContinueStatement
     |  EmitStatement
     |  StartStatement
     |  TransactionStatement
     |  IfStatement
     |  MatchStatement
     |  WhileStatement
     |  ForStatement
     |  LoopStatement

LetStatement
    ::= "let" "mut"? Pattern ( ":" Type )?
        "=" Expression ";"

AssignmentStatement
    ::= AssignablePath AssignmentOperator Expression ";"

AssignmentOperator
    ::= "=" | "+=" | "-=" | "*=" | "/=" | "%="
     |  "&=" | "|=" | "^=" | "<<=" | ">>="

AssignablePath
    ::= IDENT AssignableSuffix*

AssignableSuffix
    ::= "." IDENT | "[" Expression "]"

ExpressionStatement
    ::= Expression ";"

ReturnStatement
    ::= "return" Expression? ";"

BreakStatement
    ::= "break" Expression? ";"

ContinueStatement
    ::= "continue" ";"

EmitStatement
    ::= "emit" IDENT "(" ArgumentList ")" ";"

TransactionStatement
    ::= "transaction" Block

IfStatement
    ::= "if" HeadExpression Block
        ( "else" ( IfStatement | Block ) )?

MatchStatement
    ::= MatchExpression ";"?

WhileStatement
    ::= "while" HeadExpression Block

ForStatement
    ::= "for" Pattern "in" HeadExpression Block

LoopStatement
    ::= "loop" Block
```

Block Parser 必须优先把 Block Item 起始处未加括号的 `if`/`match` 解析为 `IfStatement`/`MatchStatement`；Tail Expression 若要以二者开头必须使用 Grouped Expression 或显式 `return`。这条优先规则消除 `Statement* TailExpression?` 的 CST 歧义。

---

## A.12 Expression 和运算符

```ebnf
Expression
    ::= RangeExpression

HeadExpression
    ::= Expression

RangeExpression
    ::= CoalesceExpression
        ( ( ".." | "..=" ) CoalesceExpression )?

CoalesceExpression
    ::= LogicalOrExpression
        ( "??" CoalesceExpression )?

LogicalOrExpression
    ::= LogicalAndExpression ( "||" LogicalAndExpression )*

LogicalAndExpression
    ::= BitOrExpression ( "&&" BitOrExpression )*

BitOrExpression
    ::= BitXorExpression ( "|" BitXorExpression )*

BitXorExpression
    ::= BitAndExpression ( "^" BitAndExpression )*

BitAndExpression
    ::= EqualityExpression ( "&" EqualityExpression )*

EqualityExpression
    ::= ComparisonExpression
        ( ( "==" | "!=" ) ComparisonExpression )?

ComparisonExpression
    ::= ShiftExpression
        ( ( "<" | "<=" | ">" | ">=" ) ShiftExpression )?

ShiftExpression
    ::= AdditiveExpression ( ( "<<" | ">>" ) AdditiveExpression )*

AdditiveExpression
    ::= MultiplicativeExpression
        ( ( "+" | "-" ) MultiplicativeExpression )*

MultiplicativeExpression
    ::= CastExpression ( ( "*" | "/" | "%" ) CastExpression )*

CastExpression
    ::= UnaryExpression ( "as" Type )*

UnaryExpression
    ::= ( "!" | "~" | "+" | "-" | "await" ) UnaryExpression
     |  PostfixExpression

PostfixExpression
    ::= PrimaryExpression PostfixSuffix*

PostfixSuffix
    ::= GenericCallArgs? "(" ArgumentList ")"
     |  "[" Expression "]"
     |  "." IDENT
     |  "?." IDENT
     |  "?"

GenericCallArgs
    ::= "::" GenericArgs

PrimaryExpression
    ::= Literal
     |  Path
     |  "self"
     |  "Self"
     |  TupleExpression
     |  ListExpression
     |  RecordExpression
     |  "(" Expression ")"
     |  Block
     |  IfExpression
     |  MatchExpression
     |  ClosureExpression

Literal
    ::= INT_LITERAL
     |  FLOAT_LITERAL
     |  STRING_LITERAL
     |  CHAR_LITERAL
     |  COLOR_LITERAL
     |  UNIT_LITERAL
     |  "true"
     |  "false"
     |  "None"

TupleExpression
    ::= "(" Expression ","
        ( Expression ( "," Expression )* ","? )? ")"

ListExpression
    ::= "[" ( Expression ( "," Expression )* ","? )? "]"

RecordExpression
    ::= Path GenericCallArgs? "{" RecordInitializerList? "}"

RecordInitializerList
    ::= RecordInitializer ( "," RecordInitializer )* ","?

RecordInitializer
    ::= IDENT ":" Expression
     |  IDENT
     |  ".." Expression

IfExpression
    ::= "if" HeadExpression Block "else" ( IfExpression | Block )

MatchExpression
    ::= "match" HeadExpression "{" MatchArm
        ( "," MatchArm )* ","? "}"

MatchArm
    ::= Pattern ( "if" Expression )?
        "=>" ( Expression | Block )

ClosureExpression
    ::= "move"? ( "||" | ClosureParams )
        ( "->" Type )? ( Expression | Block )

ClosureParams
    ::= "|" ClosureParameter ( "," ClosureParameter )* ","? "|"

ClosureParameter
    ::= "mut"? Pattern ( ":" Type )?

ExpressionList
    ::= Expression ( "," Expression )* ","?

ArgumentList
    ::= ( Argument ( "," Argument )* ","? )?

Argument
    ::= Expression
     |  IDENT ":" Expression
```

Named Argument 的语法歧义由 Parser 在 Call Argument Context 中解决；普通 `Path` 后的 `:` 不构成 Expression。`HeadExpression` 使用与 `Expression` 相同的 Production，但按 §64.2 禁止最外层未加括号的 `RecordExpression`。

---

## A.13 Pattern

```ebnf
Pattern
    ::= OrPattern

OrPattern
    ::= BindingPattern ( "|" BindingPattern )*

BindingPattern
    ::= IDENT "@" RangePattern
     |  RangePattern

RangePattern
    ::= PrimaryPattern ( ( ".." | "..=" ) PrimaryPattern )?

PrimaryPattern
    ::= "_"
     |  LiteralPattern
     |  IdentifierPattern
     |  TuplePattern
     |  ListPattern
     |  ConstructorPattern
     |  QualifiedVariantPattern
     |  "(" Pattern ")"

LiteralPattern
    ::= INT_LITERAL | CHAR_LITERAL | STRING_LITERAL
     |  "true" | "false"

IdentifierPattern
    ::= "mut"? IDENT

TuplePattern
    ::= "(" Pattern ","
        ( Pattern ( "," Pattern )* ","? )? ")"

ListPattern
    ::= "[" ( ListPatternItem ( "," ListPatternItem )* ","? )? "]"

ListPatternItem
    ::= Pattern | ".." IDENT?

ConstructorPattern
    ::= TypePath ConstructorPatternPayload

ConstructorPatternPayload
    ::= "(" ( Pattern ( "," Pattern )* ","? )? ")"
     |  "{" ( RecordPatternField
              ( "," RecordPatternField )* ","? )? "}"

QualifiedVariantPattern
    ::= IDENT "::" IDENT ( "::" IDENT )*

RecordPatternField
    ::= IDENT ":" Pattern | IDENT | ".."
```

裸单段 `IDENT` 一律是 Binding Pattern。无 Payload Enum Variant 必须写限定 Path，例如 `State::idle`；带 Payload 的 Constructor 由紧随其后的 `(...)` 或 `{...}` 消除歧义。

---

# 附录 B：七项歧义的最终裁决

| 编号 | 问题                               | 最终唯一规则                                                          | Parser/Checker 诊断         |
| ---: | ---------------------------------- | --------------------------------------------------------------------- | --------------------------- |
|    1 | `child` 与裸节点混用               | 删除 `child`；裸 `Type {}` 是匿名节点，`node id: Type {}` 是具名节点  | `E3001`，可自动删除 `child` |
|    2 | `on click => ...` 与 Block Handler | Handler 只能写 `on click { ... }` 或 `on click(event) { ... }`        | `E3201`，可包成 Block       |
|    3 | `Float`、F64、Shader F32           | 删除 `Float`；Host 和 Shader 都使用 F32/F64 明确宽度，Shader 禁止 F64 | `E2101` / `E8102`           |
|    4 | Resource 子句漂移                  | 只允许 Resource Config Block；Policy 只允许 Typed List                | `E4301` / `E4302`           |
|    5 | `sp`、`min` 等单位未闭合           | 后缀全集固定为 dp/px/sp/%/ns/us/ms/s/min/deg/rad/turn/hz/khz          | `E1204` 未知单位            |
|    6 | State 前向引用                     | 一律禁止；Computed 可建立无环前向依赖图                               | `E2104` 指向声明与引用      |
|    7 | Branch/List 都叫 key               | Branch 使用 `preserve "literal"`；List 使用 `key expression`          | `E3301` / `E3401`           |

附加裁决：

- 简单语句一律有分号；
- `=>` 只属于 Match；
- `<=>` 只属于 Two-way Binding；
- `:` 只承载规范定义的类型/字段/Property Binding 语义，不承载 Makepad Assignment-family 的隐藏 apply 语义；
- `:=`、`+:`、`<:`、`>:`、`^:` 不属于 Viso 1.0；
- View `for` 强制 Key；Behavior `for` 不允许 Key；
- `Float` 不作为兼容别名保留，以避免Makepad 代码悄悄编译成错误精度。

---

# 附录 C：建议的稳定错误码

| 错误码 | 含义                                                    |
| ------ | ------------------------------------------------------- |
| E1001  | 未知或不支持的语言版本                                  |
| E1101  | 非法标识符或 Unicode 规范化冲突                         |
| E1102  | Unicode Confusable（默认警告）                          |
| E1201  | 未闭合字符串                                            |
| E1202  | 未闭合注释                                              |
| E1203  | 非法数值字面量                                          |
| E1204  | 未知单位后缀                                            |
| E1205  | 非法字符串/字符 Escape                                  |
| E1206  | 非法 Numeric Separator 或后缀边界                       |
| E1301  | 保留字被当普通标识符使用                                |
| E2001  | 未解析符号                                              |
| E2002  | Import 歧义                                             |
| E2003  | 值初始化循环                                            |
| E2004  | 表达式泛型缺少 Turbofish 或 Const Argument 缺少 `const` |
| E2101  | `Float` 类型已删除                                      |
| E2102  | 非法隐式数值转换                                        |
| E2103  | 类型不匹配                                              |
| E2104  | State Initializer 前向引用                              |
| E2105  | Computed 循环依赖                                       |
| E2201  | Trait Bound 未满足                                      |
| E2202  | Trait Impl 重叠或歧义                                   |
| E2301  | 非穷尽 Match                                            |
| E2302  | 不可达 Pattern                                          |
| E2401  | Closure 参数无法推断                                    |
| E2501  | Effect Kind 调用违规                                    |
| E2502  | View/Computed 中存在副作用                              |
| E2601  | 缺少 Capability                                         |
| E2701  | 类型不能实现 StableKey                                  |
| E2702  | 重复 Runtime Key                                        |
| E2801  | Control Head 中的 Record Expression 必须加括号          |
| E3001  | 已删除的 `child` 关键字                                 |
| E3002  | View Cardinality 不满足                                 |
| E3003  | Component 没有 Default Slot                             |
| E3101  | 未知 Property                                           |
| E3102  | Property 重复绑定                                       |
| E3103  | Property 不支持双向绑定                                 |
| E3201  | 已删除的事件箭头语法                                    |
| E3202  | 未知 Event 或错误 Payload                               |
| E3301  | Conditional Preserve 必须是静态字符串                   |
| E3401  | View For 缺少 Key                                       |
| E3402  | Key Expression 不稳定                                   |
| E3501  | 未知 Slot/Part                                          |
| E3502  | Slot Cardinality 冲突                                   |
| E3601  | Template 无限递归                                       |
| E4101  | Action 中使用 Await                                     |
| E4102  | Task 跨挂起访问可变 State                               |
| E4201  | Effect 读取未声明依赖                                   |
| E4202  | Reactive Cycle                                          |
| E4203  | Effect Run Policy 与依赖列表不兼容                      |
| E4301  | Resource 缺少或重复 Load/Key                            |
| E4302  | Resource Policy 冲突                                    |
| E4401  | Start 目标不是 Task                                     |
| E4501  | 无主 Detached Task                                      |
| E5101  | Hot Reload 类型不兼容                                   |
| E5102  | Hot Reload Stable ID 冲突                               |
| E6101  | Native Schema 版本冲突                                  |
| E6102  | Native Ownership/Thread Domain 违规                     |
| E6103  | Capability Denied（运行时）                             |
| E7101  | 执行预算超限                                            |
| E7102  | 内存预算超限                                            |
| E8101  | Shader 使用 Host-only 类型                              |
| E8102  | Shader 使用 F64                                         |
| E8103  | Shader Loop 无静态上限                                  |
| E8104  | Shader ABI 不匹配                                       |
| E9101  | Game System Order 循环                                  |
| E9102  | Fixed Tick 预算超限                                     |

错误码文案可以改进，但错误码语义不得在同一 Major 版本中复用。

---

# 附录 D：可直接交给实现 AI 的主提示词

```text
你正在实现 Viso DSL 1.0。唯一语言规范是
`docs/language/viso-dsl-1.0.md`，其中附录 A 的 EBNF 是权威 Parser 合同。

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
9. 修改语法前先更新规范、版本、Parser Golden 和迁移器；未经批准不得偏离规范。
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

以下为架构判断，不是性能 Benchmark：

| 维度           |   评估 | 成立条件                                    | 主要剩余风险                             |
| -------------- | -----: | ------------------------------------------- | ---------------------------------------- |
| 人类清晰度     | 8.8/10 | 文档、Formatter、Schema 同步                | 关键字数量较多，高级区分需教学           |
| 易上手性       | 8.4/10 | Quick Start 只展示 Level 1                  | 一开始展示 Effect/Task/Shader 会造成负担 |
| 可扩展性       | 9.2/10 | 扩展 Schema/Trait/Profile，不扩标点         | Plugin Schema 版本管理复杂               |
| 灵活度         | 9.0/10 | 保留 Native/System/Shader Escape Hatch      | 严格类型会比动态脚本多写一些声明         |
| AI 生成友好    | 9.4/10 | EBNF + Schema + JSON Diagnostic 真正实现    | 只写文档而不做工具时评分会大幅下降       |
| 表达能力       | 9.1/10 | 标准库和 Runtime API 完整                   | DSL 不应承担所有底层算法                 |
| 游戏能力       | 9.0/10 | 有 Game Profile、ECS/物理/输入/音频 Runtime | 大型游戏资产与编辑器仍是独立工程         |
| 编译器可落地性 | 9.0/10 | 按阶段实现，不一次性全做                    | 类型、响应式和热重载组合工程量很大       |

### E.1 人类清晰度的关键判断

新版不是“语法越少越好”，而是“同一概念只有一种写法”：

```text
匿名孩子       Type {}
具名孩子       node name: Type {}
属性           name = expression;
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

本文把这些定义为语言交付的一部分，而不是后续可有可无的附属工具。

### E.4 游戏能力的关键判断

Viso 可以承载 Makepad 式游戏脚本的关键原因是：

- Behavior Language 有控制流、Pattern、Closure 和 Typed Call；
- System 有长期 State；
- Trait 把 Tick/Collision 等 Hook 标准化；
- Native Handle 注入 ECS/物理/输入/音频；
- Shader 域负责 GPU 代码；
- Hot Reload 在 Tick/Frame Boundary 原子切换；
- Game Profile 定义确定性和预算。

因此类型更严格并没有削弱游戏能力，反而让 AI 生成的游戏逻辑更容易检查、回放和迁移。

---

# 附录 F：资料与证据说明

## F.1 Makepad 当前 Script 对照依据

本文对 Makepad 的对照仅基于当前 `script_mod!` / `ScriptVm` 路径及公开源码中可观察到的 tokenizer、parser、注册、namespace、游戏脚本和 Shader/Widget 使用方式。

主要源码入口：

- <https://github.com/makepad/makepad/blob/dev/platform/script/src/tokenizer.rs>
- <https://github.com/makepad/makepad/blob/dev/platform/script/src/parser.rs>
- <https://github.com/makepad/makepad/blob/dev/splashgame.md>

## F.2 证据边界

- 本文没有声称 Makepad 官方发布过一份当前 Script 的完整 EBNF；
- Makepad Script 的文法/语义描述仅用于迁移与架构对照；
- Viso EBNF 是 Viso DSL 1.0 Draft 的 Parser 合同；
- Game Profile 示例证明的是语言承载能力，不代表物理、ECS、音频和资产系统已经自动实现；
- 性能结论必须由 Viso 实现后的 Benchmark 验证。

---

# 结束语

Viso DSL 1.0 的目标是同时获得紧凑的 authoring、Typed Native/Shader/Game 扩展能力，以及长期可维护语言需要的工程基础：

```text
唯一语法
明确词法
完整 EBNF
运算符优先级
静态类型
Trait 与泛型
Effect/Task/Resource 边界
稳定身份
事务式响应式
安全 Native/Shader ABI
逐构造 Lowering
可查询 Schema
机器诊断
Last-good 热重载
```

在这些条件下，它既能让人类快速写 Counter、表单和应用，也能让 AI 在编译器反馈闭环中可靠生成复杂 UI、Shader 和游戏逻辑。
