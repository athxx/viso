# Viso DSL 1.0：语言设计与形式化编译规范

> 文档状态：Viso DSL 1.0 Draft（1.0 Final 前可修订；修订前本文是兼容实现唯一的规范依据）  
> 规范级别：语言语法、静态语义、运行时语义与编译器 Lowering 合同  
> 目标读者：Viso 编译器、运行时、Widget、Shader、游戏 Profile、LSP 与 AI 编码代理实现者  
> 基线日期：2026-09-08  
> Viso 是独立设计与实现的语言/运行时体系；Makepad 只作为外部 authoring、实时编辑、渲染与游戏体验参考，不参与 Viso 的 public syntax、ABI、runtime contract 或项目模型。Viso 外部 DSL 文件统一使用 `.vs`。

---

## 0. 规范词语与文档范围

本文使用以下规范词语：

- **必须（MUST）**：兼容实现不得违背；
- **禁止（MUST NOT）**：实现必须拒绝或诊断；
- **应该（SHOULD）**：除非存在记录在案的理由，否则应遵循；
- **可以（MAY）**：可选能力；
- **规范性（Normative）**：决定兼容性的规则；
- **说明性（Informative）**：用于解释，不覆盖规范性规则。

本文规范性地定义（设计判断、外部参考与实现提示见说明性文档 `Viso_DSL_Rationale.md`）：

1. 清晰度硬规则与 Surface 分层；
2. Rust 侧三种源码入口（§22.1）；
3. UTF-8、标识符、注释、关键字、字面量与单位的词法规范；
4. 模块、类型、组件、System、函数、行为、资源、View 与 Shader 的形式文法；
5. 表达式、运算符优先级、结合性、闭包与 Pattern 文法；
6. 类型推断、泛型、Trait、约束、转换和子类型规则；
7. 响应式状态、节点身份、事件、异步、热重载、自适应环境与游戏循环语义；
8. CST、AST、HIR、UI IR、Reactive IR、Script IR 与 Shader IR 的 Lowering 规则；
9. 编译器、Formatter、LSP、Schema 和 AI Vibe Coding 的交付合同；
10. 解析、类型、运行时、自适应、热重载、游戏和 AI 生成的验收标准。

本文不定义：

- 具体 GPU API 的实现；
- 具体物理引擎、音频引擎或 ECS 的内部算法；
- 完整标准库 API；
- 所有 Widget 的属性清单；
- Rust ABI 的具体符号名称。

这些内容由 Viso Runtime、Native Schema 和各 Profile 规范定义，但不得改变本文的核心语法和静态语义。

---

# 第一部分：规范总则

## 1. 清晰度硬规则与学习分层

§1.1（设计结论与最小 Counter 示例）已移至 `Viso_DSL_Rationale.md`。

### 1.2 当前规范的清晰度硬规则

Viso DSL 1.0 采用以下唯一规范规则：

| 设计问题                                      | Viso DSL 1.0 的唯一规则                                               |
| --------------------------------------------- | --------------------------------------------------------------------- |
| `child Column {}` 与裸 `Column {}` 混用       | 删除 `child`；裸组件节点就是匿名子节点                                |
| `on click => expr` 与 `on click { ... }` 并存 | 删除事件箭头简写；事件处理器一律使用 block                            |
| `Float` 有时等于 `F64`，Shader 又要求定宽类型 | 删除 `Float`；只保留 `F32` 与 `F64`                                   |
| Resource 有单行子句和逗号列表两套形式         | Resource 一律使用配置 block；策略一律是 `policy = [ ... ];`           |
| `ms` 已定义而 `min`、`sp` 只在例子中出现      | 完整枚举全部单位及量纲规则                                            |
| State 默认值允许“依赖顺序明确”的前向引用      | State 初始化禁止前向引用；Computed 才允许无环前向依赖                 |
| 条件分支与列表都使用 `key`                    | 分支缓存使用 `preserve "static-id"`；列表身份使用 `key expression`    |
| 省略分号依赖换行猜测                          | 简单声明、属性和语句必须以 `;` 结束                                   |
| `:=`, `+:`, `<:`, `>:` 等承担多种隐藏语义     | 从核心语言移除；View 属性只保留 `property: expr;`，其余使用明确关键词 |

### 1.3 学习曲线

语言采用分层学习；Level 与 §25.1 Surface Tier 一一对应，§151 实现阶段按同一 Tier 排序：

- **Level 1（Core UI）**：Component、Input、State、Computed、Action、View、Node、Property、Event、`if`/`match`/Keyed `for`、Record/Enum、基础 `fn`；
- **Level 2（Core System/Shader）**：`system` 与 Native Schema 提供的 Scheduler Trait（如 `FixedUpdate`）、Shader Profile Entry；
- **Level 3（Standard）**：Effect、Task、Resource、Slot、Style、Theme、Hot Reload Migration；
- **Level 4（Advanced）**：用户定义 Trait/Impl、一般泛型、Const Generic、`dyn` Trait、Template/Part、手写 Native 声明、细粒度 Capability 标注。

Schema、IR 与 Compiler Plugin 属于工具链，不属于语言 Surface。高级特性不会污染 Level 1 的基本语法。

§2–§8（可扩展性、AI 友好度、表达与游戏能力的设计判断，以及 Makepad 外部参考）是说明性内容，已移至 `Viso_DSL_Rationale.md`，编号不变。

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

其他 Unicode 空白字符在字符串和注释外不属于空白 Token；Compiler 必须报告不可见字符诊断。Viso 1.0 的 XID 与 NFC 数据表固定使用 Unicode 17.0；实现必须按同一数据表执行标识符规范化与校验。

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

binding_identifier  = identifier_token ;
label               = identifier_token | strict_keyword ;
```

`strict_keyword` 是 §12.1、§12.2 中任一单词产生的 Keyword Token。

规则：

- 标识符按 Unicode NFC 规范化后进入符号表；
- 两个源码拼写若 NFC 后相同，视为同一标识符；
- 同一命名空间内两个声明 NFC 后相同而源码拼写不同时报告 `E1101`（相关位置指向先前声明）；拼写完全相同则是普通重复声明 `E2002`；
- 编译器应警告 Unicode Confusable；
- `normal_identifier` 不得等于严格关键字（§12.1、§12.2）；拼写等于上下文关键字（§12.3）的单词仍词法化为 `identifier_token`；
- 正文 EBNF 的 `identifier` 与合并 EBNF 的 `IDENT` 均表示 `identifier_token`，在 Binding/声明位置即 `binding_identifier`；Label 位置使用 `label`（附录 A 为 `Label`），见 §12.5；Raw Identifier 解码后仍是普通符号名；
- Raw Identifier 只用于在 Binding/声明位置使用严格关键字拼写，例如引用名为 `type` 的外部 Schema 符号时写 `r#type`；Label 位置与上下文关键字都不需要 `r#`；
- 新代码不应主动创建 raw identifier；
- `r#` 后跟 Identifier Start 时词法化为 Raw Identifier；`r` 后跟 `"` 或 `#...#"` 时词法化为 Raw String，两者使用最长合法匹配；
- 模块、值和属性推荐 `snake_case`；
- 类型、Component、System、Trait 推荐 `UpperCamelCase`；
- 常量推荐 `UPPER_SNAKE_CASE`。

---

## 12. 关键字

关键字分为严格关键字与上下文关键字两层。关键字大小写敏感；库不得通过 Schema 引入新关键字。

### 12.1 严格关键字

严格关键字由 Lexer 产生独立 Keyword Token（附录 A 统称 `STRICT_KEYWORD`），永远不能出现在 Binding/声明位置：

```text
import export as
component system record enum trait impl type
implements where for
fn action task const native shader
let mut return break continue
if else match while loop in
true false None self Self dyn await
```

### 12.2 保留并禁止使用

以下单词同属严格关键字，当前语法不生成对应 AST，保留用于稳定诊断或未来扩展：

```text
child store merge extend inherit class
macro unsafe extern static yield try
```

`child` 被保留是为了让编译器输出明确的非法 View 语法诊断（`E3001`），而不是把它当作普通组件名。

### 12.3 上下文关键字

以下单词由 Lexer 词法化为普通 `identifier_token`，只在 §12.4 列出的固定语法位置具有特殊含义；在其他位置（包括全部 Binding/声明位置）就是普通标识符：

```text
input state computed event slot effect resource view
style theme template part requires capability
on capture bubble emit transaction start move
when run cleanup success error cancelled
node fill bind using use override replace preserve key
load policy scope empty
vertex fragment compute uniform instance varying texture sampler
```

EBNF 中带引号的上下文关键字（如 `"state"`）匹配文本相同的 identifier Token。`capability` 当前没有专用语法位置（`@capability` 是普通 Attribute Path），保留在本表中供 Schema 与后续版本使用。

### 12.4 上下文关键字的识别位置

Parser 只在下列位置、以至多 2 个 Token 的 Lookahead 把上下文关键字识别为关键字：

| 位置                        | 识别形式                                                                                           |
| --------------------------- | -------------------------------------------------------------------------------------------------- |
| Statement/Expression 起始   | `emit IDENT`、`start IDENT`、`transaction {`、`move \|`（含 `move \|\|`）                          |
| 顶层声明起始                | `style IDENT`、`theme IDENT`、`template IDENT`                                                     |
| Component/System 成员起始   | `input`/`state`/`computed`/`event`/`slot`/`effect`/`resource` 后随 IDENT；`view {`                 |
| Node Body / View Block 项起始 | `node IDENT :`、`part IDENT :`、`on IDENT`、`on capture`/`on bubble`、`bind IDENT`、`fill IDENT`、`use IDENT`、`override part`、`replace part` |
| View 控制结构               | `if` Header 后的 `preserve STRING`；View `for` Header 中的 `key`                                    |
| Resource Item               | `load =`、`key =`、`policy =`、`scope =`                                                           |
| Start Handler               | `policy =`、`success (`、`error (`、`cancelled {`                                                  |
| Effect 子句                 | `when (`、`run`、`cleanup {`                                                                       |
| Callable 签名尾部           | `requires {`                                                                                       |
| Two-way Binding / Style     | `bind` 之后的 `using`；Style Item 起始的 `when`                                                    |
| Shader 成员                 | `uniform`/`instance`/`varying`/`texture`/`sampler` 后随 IDENT；`vertex (`、`fragment (`、`compute (` |
| Slot Default                | `= empty ;`                                                                                        |

不满足识别形式时按普通 identifier 解析，例如 `emit(x);` 是普通调用、`state: s;` 是名为 `state` 的 Property Binding、`let move = 1;` 声明名为 `move` 的局部变量。

### 12.5 Label 位置与 Binding 位置

- **Label 位置**接受任意 identifier 或关键字（严格或上下文），无需 `r#`：`.`/`?.` 之后的 Member 名、Path 中 `::` 之后的段、Record 字段声明名、Record Literal 与 Record Pattern 的字段标签、Named Argument 标签、Attribute 参数标签、Property Path 段、Enum Variant 名。例如 `token.type`、`Token::match`、`record Rule { type: String; }` 合法；
- **Binding 位置**（`let`、参数、Pattern 绑定、Closure 参数、Import 别名以及全部声明名）只接受 `binding_identifier`：上下文关键字可用，严格关键字不可用，出现时报 `E1301`；
- Record Literal/Pattern 的字段简写（`User { id }`）同时是 Binding 引用，因此只接受 `binding_identifier`。

### 12.6 `theme` 与 `env`

`theme` 与 `env` 不是 Grammar Production。它们在 Expression 中是普通 identifier，由名称解析绑定到隐式注入的 Typed Context Binding：Theme Context 在 View、Style 与 Theme 表达式中可见（§60），`env` 只在 View 中可见（§48、第十一部分）；其他位置读取按未解析符号 `E2001` 诊断。局部 Binding 可按普通词法作用域遮蔽二者（关键字策略见 ADR 0034）。`theme` 同时是 `theme IDENT` 顶层声明的上下文关键字，两种用法由 §12.4 的位置规则区分。

### 12.7 标准库符号与后缀

以下不是关键字：

- `viso`：标准库根 Module 名；应用代码 SHOULD NOT 声明同名顶层 Module，以避免导入歧义；
- `Bool`、`I64`、`F32`、`String` 等是 Prelude 类型符号，不是 Lexer Keyword；
- `EffectRun::mount`、`ResourcePolicy::keep_latest` 等是普通限定路径；
- `i32`、`f32`、`ms`、`dp` 等在紧邻数字时由 Numeric Literal Lexer 识别为后缀，不作为独立 Identifier Token。

例如 `State` 与 `state` 都词法化为 identifier，但只有后者在 §12.4 的位置被识别为关键字。

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
action increment() {}
if condition {}
while condition {}
Text {}
on click {}
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
- `format(template, args...)` 是编译器内建，返回 `String`：`template` 必须是字符串字面量；`{}` 按顺序消费位置实参，`{name}` 消费同名 Named Argument（`name: expr`）；`{{`、`}}` 表示字面大括号；占位符与实参在编译期逐一匹配，每个实参类型必须实现 `Display`；不支持格式说明符，也不读取 Locale；不匹配时报 `E2108`；
- 内建 `Display` 的类型：`Bool`、全部整数与浮点类型、`Char`、`String`、单位长度 `Dp`/`Px`/`Sp`/`Em`/`Percent`、`Duration`、`Angle`、`Frequency`；`Bytes`、`Unit`、`Color`、`MixedLength`、Tuple、`List`、`Map`、`Option`、`Result`、Range、函数、`Handle`/`NodeRef` 以及 Record 与 Enum 均无 `Display`，须显式映射为文本（例如 `match` 到 `tr(...)`），否则报 `E2108`；
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

| 量纲         | 后缀   | 类型        | 分类                     |
| ------------ | ------ | ----------- | ------------------------ |
| 逻辑长度     | `dp`   | `Dp`        | 绝对长度（布局基准单位） |
| 设备像素     | `px`   | `Px`        | 绝对长度                 |
| 字体缩放长度 | `sp`   | `Sp`        | 相对长度（系统字体缩放） |
| 相对字号长度 | `em`   | `Em`        | 相对长度（节点字号）     |
| 百分比       | `%`    | `Percent`   | 无量纲比例 / 相对长度    |
| 时间         | `ns`   | `Duration`  |                          |
| 时间         | `us`   | `Duration`  |                          |
| 时间         | `ms`   | `Duration`  |                          |
| 时间         | `s`    | `Duration`  |                          |
| 时间         | `min`  | `Duration`  |                          |
| 角度         | `deg`  | `Angle`     |                          |
| 角度         | `rad`  | `Angle`     |                          |
| 角度         | `turn` | `Angle`     |                          |
| 频率         | `hz`   | `Frequency` |                          |
| 频率         | `khz`  | `Frequency` |                          |

`Dp`、`Px`、`Sp`、`Em` 与 `Percent` 合称 **长度族（Length Family）**。长度族之间的混合运算得到 `MixedLength`（§19.5），它没有字面量后缀。

### 19.2 词法形式

```ebnf
unit_literal      = unit_numeric_body, unit_suffix
                  | unit_numeric_body, "%" ;

unit_numeric_body = decimal_digits,
                    [ ".", decimal_digits ] ;

unit_suffix       = "dp" | "px" | "sp" | "em"
                  | "ns" | "us" | "ms" | "s" | "min"
                  | "deg" | "rad" | "turn"
                  | "hz" | "khz" ;
```

`numeric_body` 与后缀之间不得出现空白。`unit_numeric_body` 不含指数部分：`1e2dp` 是 `E1203`。`e`/`E` 只有在其后紧跟 `[+-]? decimal_digit` 时才开始浮点指数，因此 `1em`、`1.5em` 始终词法化为 Unit Literal，`1e5` 始终是 Float Literal。

```viso
14sp
16dp
1px
1.5em
50%
250ms
5min
90deg
60hz
```

`50 % 3` 是取模；`50%` 是 Percent Literal。为避免 `50%3` 的歧义，`%` 只有在紧邻数字且其后是 Trivia、分隔符、运算符或文件结束时才成为 Percent 后缀；其后紧跟数字或标识符继续字符时，`%` 是取模运算符。因此 `50%3` 与 `50 % 3` 都解析为取模，`100%-8dp` 与 `100% - 8dp` 都解析为 Percent 减 Dp。

单位后缀集合是封闭的。插件、Widget Schema 和 Native 模块不得注册新的词法后缀；自定义量纲必须使用普通构造函数或 Newtype，例如 `Meters::new(12.5f64)`。这避免 Lexer 插件化、后缀冲突和 AI 猜测单位。

### 19.3 量纲运算规则

对任一量纲类型 `T`（长度族、`Duration`、`Angle`、`Frequency`）：

| 表达式                      | 结果                             |
| --------------------------- | -------------------------------- |
| `T + T`、`T - T`、`-T`      | `T`                              |
| `T * S`、`S * T`、`T / S`   | `T`，`S` 为 `F32` 或未定型数值   |
| `T / T`                     | `F64`                            |
| `T == T`、`T < T` 等比较    | `Bool`                           |
| `T % T`                     | 禁止                             |
| `T * T`                     | 禁止                             |

长度族的跨单位规则：

| 表达式                                           | 结果          |
| ------------------------------------------------ | ------------- |
| 两个不同长度族成员相加减，例如 `100% - 16dp`     | `MixedLength` |
| `MixedLength ± 长度族成员` / `MixedLength ± MixedLength` | `MixedLength` |
| `MixedLength * S`、`S * MixedLength`、`MixedLength / S` | `MixedLength` |
| `-MixedLength`                                   | `MixedLength` |
| `MixedLength` 与任何值比较、相乘、相除           | 禁止（`E2107`） |
| 长度族与非长度量纲混合，例如 `1s + 1dp`          | 禁止（`E2103`） |
| `Percent + 标量`，例如 `50% + 0.5`               | 禁止（`E2107`） |

允许：

```viso
let total: Duration = 1s + 250ms;
let half_turn: Angle = 180deg;
let inset: Dp = 16dp * 2;
let content_width = 100% - 2 * inset;   // MixedLength
let icon_size = 1.25em;                 // Em
```

禁止：

```viso
let x: Dp = 10dp + 5px;        // E2106：MixedLength 不能在布局前定型为 Dp
let y = 1s + 2dp;              // E2103：跨量纲
let z = (100% - 8dp) < 200dp;  // E2107：MixedLength 在布局前不可比较
let w = 10dp < 5px;            // E2103：不同单位比较
```

`MixedLength` 的求值被推迟到 Layout 阶段；它不是运行时隐式单位转换。需要具体单位值时，必须在具有 Layout Context 的受控 API 中显式解析：

```viso
let px: Px = layout.dp_to_px(10dp);
let resolved: Dp = layout.resolve_length(node_ref, 100% - 16dp);
```

View、Computed、Style 和 Theme 中不存在 Layout Context，因此禁止调用上述 API。

### 19.4 长度解析基准（Normative）

所有长度最终解析为 `Dp`；Layout 坐标系的单位是 `Dp`。像素对齐由 Layout/Render 合同负责，不属于长度语义。

| 单位      | 解析为 Dp                                  | 基准来源                                   |
| --------- | ------------------------------------------ | ------------------------------------------ |
| `1dp`     | `1`                                        | —                                          |
| `1px`     | `1 / scale_factor`                         | 节点所在 Surface 的 `env.window.scale_factor` |
| `c sp`    | `text_scale_curve(c)`；线性平台为 `c × env.text_scale` | 节点所在 Adaptive Scope 的字体缩放曲线 |
| `1em`     | 节点的 Resolved Font Size（Dp）            | Typography Context（见下）                 |
| `1%`      | `0.01 × Percent Basis`                     | 由 Property Schema 声明（见下）            |

**`sp` 与 CSS `rem`。** `sp` 是 Viso 对应 `rem` 的单位：它只随系统/用户的可访问性字体缩放变化，与节点树位置无关。Viso 不提供 `rem`；应用级"基础字号"使用 Theme Token（`theme.typography.base_size`），而不是新增单位。

**非线性字体缩放。**

- `sp` 分量按所在 Scope 的 `TextScaleCurve` 解析：一条由平台提供、单调不减、分段线性的 `sp → Dp` 映射；`env.text_scale` 是该曲线的名义倍率，用于查询与语义，不是解析公式；
- 曲线来源：Android 14+ 非线性字体缩放（大字号放大幅度小于小字号）、iOS Dynamic Type 的 Content Size Category、Windows 文本缩放、Web 用户根字号；没有非线性来源的平台使用线性曲线 `c × env.text_scale`；
- 曲线作用于**编译期折叠后的 `sp` 系数**：`8sp * 2` 与 `16sp` 解析结果相同；一个长度值只有一个 `sp` 系数，因此解析仍是 O(1)（一次分段查表）；
- 曲线只作用于 `sp` 分量；`em` 使用已解析的 Resolved Font Size，不再二次缩放。

**Scope 级缩放范围。**

- `AdaptiveScope` 可声明 `text_scale_min: Option<F32>` 与 `text_scale_max: Option<F32>`，把 Scope 内的曲线截断到该名义倍率范围；用于高度受限的系统栏、Tab Bar 与紧凑工具栏；
- Scope 内读取的 `env.text_scale` 是截断后的值；截断只影响本 Scope 子树，不影响兄弟 Scope；
- 被截断 Scope 内的交互节点 SHOULD 通过 Semantics 提供平台的大内容预览（例如 iOS Large Content Viewer），使截断不损失可访问性。

**Typography Context 与 `em`。**

- 每个 Node 都有一个 Resolved Font Size；
- Node Schema 若声明 `font_size` Property 且该 Property 已绑定，Resolved Font Size 为该值的解析结果；否则继承父 Node 的 Resolved Font Size；
- 根 Node 的 Resolved Font Size 为当前 Theme 的 `typography.base_size`；标准 Theme Schema 的默认值为 `14sp`；
- 在 `font_size` Property 自身的值中，`em` 与 `%` 以 **父 Node** 的 Resolved Font Size 为基准，避免自引用；例如 `font_size: 1.2em;` 与 `font_size: 120%;` 等价；
- 在其他 Property 中，`em` 以 **当前 Node** 的 Resolved Font Size 为基准；
- Typography Context 只沿 Node Ancestry 继承一个标量，不是 CSS 式通用 Style Cascade；其他文字属性是否继承由 Text Runtime 与 Widget Schema 决定。

**Percent Basis。**

- `Percent` 本身是无量纲比例：`50%` 的数值为 `0.5`；
- 非长度 Property（例如 `opacity`、`progress`、`volume`）若 Schema 类型为 `Percent`，直接使用比例值；
- 长度 Property 只有在其 Schema 声明 `percent_basis` 时才接受 `Percent` 或含 Percent 分量的 `MixedLength`；否则是 `E3104`；类型不能容纳长度的 Property（`Bool`、数值、`Percent` 比例等）不参与此检查，Percent 在其上只按类型检查；
- 值是否含 Percent 分量由编译期值流分析决定：值书写了 `%` 字面量或 `Percent` 类型的操作数，或引用了含 Percent 分量的值——`state`（初值与所有赋值）、`computed`、`const`、局部绑定（初值、模式解构的值、所有赋值）、省略了带 Percent 默认值字段的 Record 字面量；算术、字段访问、下标、Record/List/Tuple 字面量、`if`/`match`/Block 的结果值保留分量；条件、`match` 被匹配值、守卫与比较/逻辑运算的结果不携带分量；函数调用与 Closure 的结果只按其返回类型判断。分析在模块内求定点（名称的定义属于其模块）；诊断的相关位置指向书写 Percent 的位置；
- 编译期不可见的 Percent 分量（例如经函数返回值或双向绑定到达）在无 `percent_basis` 的 Property 上按 `0` 计算，Debug Runtime 报告 `E3105`（警告）；
- 用户 Component 的 `input` 不书写 `percent_basis`，其基准由该 Component 自身 View 中的绑定推导：Input 的值按上述值流（经 `state`、`computed`、局部绑定、字段访问、下标等，不经函数调用或 Closure）到达无 `percent_basis` 的长度 Property 时，该 Input 无基准；到达另一个 Component 的 Input 时，继承该 Input 的结论；推导在 Package 内所有 View 检查完毕后求定点，导入的 Component 的 Input 保留其在导出模块中推导的结论。向无基准的 Input 传入含 Percent 分量的值是 `E3104`，报告在传入值的模块，诊断的相关位置指向使其无基准的绑定；只经其他途径使用的 Input 视为有基准；
- 标准 Percent Basis：

| Property 类别                                         | Percent Basis                     |
| ----------------------------------------------------- | --------------------------------- |
| `width`、`min_width`、`max_width`，水平 `padding`/`margin`/`inset` | 父节点内容盒可用宽度     |
| `height`、`min_height`、`max_height`，垂直 `padding`/`margin`/`inset` | 父节点内容盒可用高度   |
| 容器 `gap`                                            | 容器自身内容盒在对应轴上的尺寸    |
| `corner_radius`                                       | 节点自身 Border Box 的较短边      |
| `font_size`                                           | 父节点 Resolved Font Size         |
| `line_height`                                         | 当前节点 Resolved Font Size       |

- Percent Basis 不确定时（例如父节点在该轴为 Fit、Scroll 的滚动轴、无上界约束）：值若只含 Percent 分量，该 Property 退回其 Schema 默认 Sizing（通常为 `Fit`）；值若为含其他分量的 `MixedLength`，Percent 分量按 `0` 计算；两种情况 Debug Runtime 都必须报告 `E3105`（警告）；
- `100%` 不等于"占满剩余空间"：Percent 只看基准尺寸、不感知兄弟节点；剩余空间分配使用 Schema 提供的 `Fill` / `Fr` 类 Sizing 值。

### 19.5 `MixedLength`

`MixedLength` 是长度族的线性组合，语义上等价于 CSS `calc()` 中只含加减和标量乘除的子集：

```text
MixedLength = dp·1dp + px·1px + sp·1sp + em·1em + pct·1%
```

规则：

- `Dp`、`Px`、`Sp`、`Em`、`Percent` 到 `MixedLength` 是隐式安全拓宽（§76.1）；
- `MixedLength` 到任何具体单位不存在隐式或 `as` 转换，只能通过 §19.3 的 Layout API 解析；
- `MixedLength` 不支持 `min`、`max`、`clamp`；尺寸约束使用 `min_width`/`max_width` 等 Property；
- 常量 `MixedLength` 必须在编译期折叠为五个系数；
- 纯 Rust 路径（`viso::ui` 的长度构造与运算符）使用同一五系数模型；`ui!`、`component!`、`view!` 与纯 Rust 构造得到相同的 IR 值；
- Property 的 Schema 类型决定其是否接受长度族、`MixedLength` 以及 `Fill`/`Fit` 等 Sizing 值。

示例（Property 名由 Widget Schema 定义，此处仅为说明）：

```viso
Column {
    padding: 1em;

    Text {
        text: title;
        font_size: 1.25em;
    }

    Row {
        width: 100% - 32dp;
        gap: 0.5em;

        Icon {
            size: 1.2em;
        }
    }
}
```

### 19.6 长度的依赖与失效

长度 Binding 按其非零分量静态登记 Layout 依赖，不得在运行时动态追踪：

| 非零分量 | 追加依赖                         | 失效                         |
| -------- | -------------------------------- | ---------------------------- |
| `px`     | `env.window.scale_factor`        | `MEASURE / LAYOUT`           |
| `sp`     | `env.text_scale`                 | `MEASURE / LAYOUT`           |
| `em`     | Resolved Font Size               | `MEASURE / LAYOUT`           |
| `%`      | 无额外 Reactive 依赖             | 基准由父布局在同一 Layout Pass 传入，随父级 `LAYOUT` 求解 |

- 某 Node 的 Resolved Font Size 变化时，只有其子树中含 `em` 分量、且未被中间 Node 的绝对 `font_size` 截断的 Binding 失效；
- 纯 `Dp` 常量不产生任何环境依赖；
- 失效上限由 Property Schema 的 `invalidates` 声明（§87）：接受长度的 Paint-only Property（例如 `border_width`，若 Schema 声明其不参与布局）只失效 `PAINT`，不因单位升级为 Layout。

### 19.7 不支持的 CSS 单位

| CSS 单位                  | Viso 1.0 替代                                              | 理由                                   |
| ------------------------- | ---------------------------------------------------------- | -------------------------------------- |
| `rem`                     | `sp`，或 `theme.typography.base_size` Token                | `sp` 已覆盖根字号 + 可访问性缩放       |
| `vw` / `vh` / `vmin` / `vmax` | `%`（相对父约束）、`env.constraints`、`env.window.logical_size` | 嵌套 Adaptive Scope 中按窗口取值通常是错误的 |
| `ex` / `ch` / `lh` / `cap` | 暂无；需要时由 Text Runtime API 提供度量                   | 依赖字体度量，1.0 不纳入长度族         |
| `fr`                      | Grid Widget Schema 的 Typed Track 值                        | 只在 Grid Track 上下文有意义，不是长度 |
| `cm` / `mm` / `in` / `pt` / `pc` | 暂无                                                | 物理单位与 UI 逻辑坐标无稳定关系       |

### 19.8 值域与非法值

- 长度 Property 的 Schema 声明值域。尺寸类（`width`、`height`、`min_*`、`max_*`、`padding`、`gap`、`corner_radius`、`border_width`、`font_size`）为非负：解析结果小于 `0` 时按 `0`；位置与偏移类（`margin`、`inset`、`offset`）允许负值；
- 同一轴上 `min_*` 大于 `max_*` 时 `min_*` 优先，结果确定；
- 常量折叠中长度族值除以常量 `0` 是 `E2109`；
- 运行时解析得到非有限值（例如动态标量除数为 `0`）时，该 Property 取 Schema 默认值；Debug Runtime 报告 `E3106`（警告）并计入 `length_nonfinite` 计数器；非有限值不得进入 Layout 或 Render State（Rendering §5.9）；Release 不 panic。

### 19.9 插值

- 长度族值与 `MixedLength` 的 Transition 与 Animation 按五个系数逐项线性插值：对每个分量计算 `a + (b - a) × t`；
- 因此 `50%` 到 `200dp` 的跨单位动画不需要在动画开始时解析为具体值；动画期间父尺寸、字号或 `scale_factor` 变化时，结果仍按当前基准正确解析；
- `sp` 分量先插值系数，再经 `TextScaleCurve` 解析；
- 插值不改变 Property 的失效类别：动画 `width` 每帧标记 `MEASURE|LAYOUT`；只需视觉缩放时使用 `scale` 变换，只走 `TRANSFORM|HIT_TEST|PAINT`（§U6.2）。

### 19.10 像素对齐

- Layout 坐标保持 `F32` Dp，Layout 不取整；取整只在 Render 侧按 device space 执行（Rendering §5.7）；
- 容器给相邻子节点的共享边必须来自同一个累加边值：前一子节点的 `x + w` 与后一子节点的 `x` 是同一个 `F32`，而不是分别计算；配合 `PixelSnap::Bounds` 对两条边各自取整，在 `1.25`、`1.5`、`1.75` 等非整数 scale 下相邻背景之间不出现缝隙或重叠；
- 只含 `px` 分量且系数为整数的 stroke/border 宽度解析为整数个 device px，标准 Widget 对其默认使用 `PixelSnap::Stroke`；与 scale 无关的一像素分隔线使用 Rendering `Hairline`（§5.8）；
- Surface 的 `scale_factor` 变化（例如窗口移到另一块显示器）只重新折叠含 `px` 分量的 Binding（§19.6），Snap 由 Render 侧按 Rendering §5.7 重新解析。

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

词法分析完成后，Parser 消费 Token，而不是字符流。严格关键字在 Lexer 阶段产生独立 Keyword Token；上下文关键字词法化为 identifier，由 Parser 按 §12.4 识别。

---

# 第五部分：完整语法——模块和声明

## 22. Compilation Unit

普通 `.vs` package source 不需要在每个文件重复声明语言版本或 module path。语言版本来自 `Viso.toml`/lockfile，模块身份来自 package root、source root 与文件路径。Package 固定的语言版本不是编译器实现的版本时报告 `E1001`，主位置指向 manifest 中的版本值。

```ebnf
compilation_unit = { import_decl },
                   { top_level_decl },
                   EOF ;

module_path      = identifier, { "::", label } ;
```

规则：

- 一个 `.vs` 文件属于一个由编译上下文确定的 Module；
- 一个 Module 可以由多个文件组成，但必须由 Manifest/source-root 规则确定；
- Import Resolution 不依赖运行时注册顺序；
- 编译器、Formatter、LSP 和 Hot Reload 都必须从同一 Source Context 获得 package/module identity；
- 独立 conformance fixture 若需要显式 module identity，应由测试 harness 提供，不扩展普通 source grammar。

### 22.1 Rust 侧源码入口

同一语言有三种源码入口，Production 见附录 A.2：

| 入口                  | Parser Entry      | 接受内容                                                                    |
| --------------------- | ----------------- | --------------------------------------------------------------------------- |
| `view!("path.vs")`    | `CompilationUnit` | 外部 `.vs` 文件，按普通 Module/文件前端编译（§22）                          |
| `ui! { ... }`         | `ViewFragment`    | View 结构项序列，必须恰好生成一个根 Node，多根按 `E3002` 拒绝                  |
| `component! { ... }`  | `ComponentEntry`  | 可选 Import 与 Attribute 后跟一个 Component 声明；`component` 关键字可省略  |

规则：

- 三种入口共享 Component/Native Schema、名称解析、类型/Effect/Capability 检查、Typed HIR、Reactive/UI/Shader IR 与诊断语义；入口只决定起始 Production，不引入宏专用语法或运行时语义；
- `ui!` 不接受 Top-level Declaration、Import 或 Component 成员；`component!` 不接受其他 Top-level Declaration；
- `.vs` 是规范外部文件格式；页面、Theme 与大型 Component 的热重载 SHOULD 使用 `view!`；内联宏的修改通常需要 Rust 增量编译，Runtime 不要求解析 Rust 源码来热重载内联宏；
- 宏内源码的诊断 Span 映射回 Rust 源文件中的宏调用位置（§134）。

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
- 隐式 Prelude 除标量类型外还导出 U7.1 的输入类型与标准事件 Payload、U6.3 的 `Animate`/`AnimationEnd`、U7.4 的 `KeyChord`/`ShortcutScope`、U3.7 的 `ScrollChanged`/`SizeDp` 与 U7.5 的 Widget 事件 Payload；名称按本模块声明 → Import → Prelude 查找，本模块声明或 Import 的同名符号遮蔽 Prelude；
- `::*` 不属于 Viso 1.0 语法；
- Import Resolution 不依赖运行时注册顺序；
- Import Cycle 中只有纯类型边可以被允许；值初始化环必须报错。
- 导入的名称必须是目标模块 `export` 的声明，否则报 `E2001`：名称存在但未导出时，相关位置指向其声明，并给出在目标模块文件中插入 `export` 的 Fix（`maybe-incorrect`），导入仍按该声明绑定；名称不存在时给出目标模块最近的导出名，该名称在导入方的使用处不再重复报告；
- 导入的名称与其导出模块中的声明是同一符号：Record/Enum 类型、Callable 签名与 Effect Class、Component 的 Input/Event/Slot 在导入方按声明检查，与本模块声明无差别。

---

## 24. Attribute

```ebnf
attribute         = "@", path, [ "(", [ attribute_args ], ")" ] ;
attribute_args    = attribute_arg, { ",", attribute_arg }, [ "," ] ;
attribute_arg     = expression
                  | label, ":", expression ;
```

标准 Attribute（由 Compiler 注册；其余 Attribute 由已导入 Schema 注册）：

| Attribute                  | 作用对象                  | 含义                                                   |
| -------------------------- | ------------------------- | ------------------------------------------------------ |
| `@stable("id")`            | 声明、View Node、成员     | Stable Identity（§88、§115）                            |
| `@derive(Eq, Hash, ...)`   | `record`、`enum`          | 派生标准 Trait                                          |
| `@deprecated(message: "")` | 任意声明                  | 使用时产生弃用警告                                      |
| `@capability("path")`      | `native` 声明             | 声明所需 Capability（§33、§95）                         |
| `@default`                 | Component 的 `slot` 成员   | 指定 Default Slot（§45.1）                              |
| `@bindable(event)`         | Component 的 `input` 成员  | 与同名 Component 的 `event` 配对为双向属性（§U2.2）      |
| `@styleable`               | Component 的 `input` 成员  | 允许 Style 绑定该 Input（§U2.3）                         |
| `@selector`                | `Bool` 的 `input`/`state`/`computed` | 公开为 Style 状态选择器（§U2.3）               |
| `@const`                   | `fn`                      | 编译期可求值函数（§34）                                 |
| `@migrate(from: "...")`    | `fn`                      | Hot Reload 状态迁移函数（§94）                          |
| `@persist("key")`          | System/Component 的 `state` | 持久化状态（§106.8）                                  |
| `@local`                   | System 的 `state`          | Presentation 层状态，Simulation 不可访问（§106.4）      |
| `@probe`                   | System 的 `state`          | 每个 Tick 输出到测试 Trace（§110.5）                    |
| `@shader_value`            | `record`                  | Shader 可用值类型（§98）                                |
| `@max_iterations(n)`       | Shader 循环 Statement      | 循环迭代上限（§99）                                     |
| `@doc("...")`              | 任意声明                  | 文档注释降低后的元数据（§10），源码中通常不手写          |

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
          fine-grained capability annotations
```

`Advanced` 可以存在于规范中，但不能成为 UI、Reactive、Game、Shader vertical slice 的前置条件。Rust Native Schema 是默认扩展路径。

---

## 26. Path、Generic 和 Where

```ebnf
path               = identifier, { "::", label } ;

type_path          = type_path_head, { "::", type_path_tail } ;
type_path_head     = identifier, [ generic_args ] ;
type_path_tail     = label, [ generic_args ] ;

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

record_field       = { attribute }, label, ":", type,
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

enum_variant       = { attribute }, label,
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
let pending = LoadState::loading;
let done = LoadState::ready(user);
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
- Private callable 的 Capability 集合默认由 Typed Call Graph 推导，调用图覆盖整个 Package（经 import 的调用同样传播）；显式 `requires { ... }` 用作公开安全合同或上界断言，不要求每个私有函数重复书写。

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
- Task Call 的参数按其签名检查，调用值的类型是 Task 声明的返回类型；`await` 只标出挂起点，不改变操作数的类型；
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
- `untracked(expr)` 是编译器内建：求值 `expr` 并返回其值，其中的 Reactive 读取不登记依赖；只允许出现在 Effect Body 与 `cleanup` 中，其他位置报 `E2501`；
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
    ResourceState::idle => { EmptyView {} },
    ResourceState::loading => { Spinner {} },
    ResourceState::ready(items) => { Results { items: items; } },
    ResourceState::error(error) => { ErrorView { error: error; } },
    ResourceState::reloading(items) => { Results { items: items; dimmed: true; } },
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
- 同一 Component 内的成员名称不能在同一 Namespace 冲突；成员只在所属 Component（System 同理）内可见，不同 Component 可声明同名成员；成员遮蔽同名 Module 声明，局部 Binding 又遮蔽成员；
- Value Namespace、Type Namespace 和 Event Namespace 分离，但 Formatter 应避免同名造成阅读混乱。

### 40.1 Component 实例的挂载

View 中类型为同一文件内 Component 的 Node 是该 Component 的一个实例。实例在编译期内联进挂载它的 View，`component!`、`view!`、Hot Reload 与 Release Package 挂载同一结果：

- 实例的 View 在该 Node 的位置挂载，成为外层 View 的一部分；实例不单独挂载 Component，也不产生额外的 Node；
- 实例的每个 State 是外层 Component 的一个隐藏 State，名为 `实例身份.State 名`；实例身份是从外层 View 到该实例的路径，每段是实例的局部名，没有局部名时为 `类型名#序号`（序号按同一父实例下同类型实例的出现次序）；Hot Reload 按名称保留隐藏 State；
- 实例读取 Input 时求值调用方的实参（以调用方所在区域的绑定为参数），无实参时取 Input 默认值，都没有时为 `None`；实例 View 中读取 Input 的 Property 绑定到实参所读的 Source，读取自身 State 的 Property 绑定到对应隐藏 State；
- 实例 `emit` 一个 Event 时，按调用方声明顺序同步运行调用方为该 Event 写的每个 Handler，Payload 是以 Event 参数为字段的 Record；调用方未处理的 Event 不产生任何效果；
- 与 Event 同名以外的 Property 和 Handler 作用于实例 View 的根 Node；
- 实例可以位于 `if`、`match` 与 Keyed `for` 中，区域绑定对实例的 Input 实参、State 初值与 Handler 可见（§56.1）；
- 控制流区域中的实例，其 State 属于区域的每次挂载：Keyed `for` 的每个 Item、`if`/`match` 的每次进入各有一份，以区域绑定求值初值；State 随 Key 移动，Item 移除或离开无 `preserve` 的分支即丢弃，`preserve` 分支缓存期间保留；这类 State 不按名称跨 Hot Reload 保留，含控制流区域的 View 整树重建时从初值重新开始；
- 以下情形报 `E3711`：Component 直接或间接挂载自身；`bind` 目标为 Component Input 且带 `using`；向 View 不恰好挂载一个 Node 的实例传入非 Input Property 或非 Event Handler；类型为其他文件的 Component；
- `ui!` Fragment 中不是内置 Widget 的类型是外围 Rust 作用域以 `component!` 声明的 Component（可写作 Rust 路径，如 `widgets::Tally`），由其 `build` 挂载，每个实例自带 State 与 Handler；这类 Node 不接受 Property、Handler 与子项（`E3711`），名称无法解析时由 Rust 在该位置报错；Hot Reload 与 Release Package 的 Fragment 没有 Rust 作用域，这类类型报 `E2001`。

---

## 41. Input

```ebnf
input_decl           = "input", identifier, ":", type,
                       [ "=", default_expression ], ";" ;
```

规则：

- Input 是父组件传入的只读值；
- Component 内禁止给 Input 赋值（`E2110`）；
- Input 默认值必须是纯、确定的 Default Expression，其类型须与声明类型相容（`E2103`）；
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
- `emit event_name(...)` 只能命名所在 Component 声明的 Event；实参按位置依次、或以 `name: value` 按名称对应 Event 参数，并按参数类型检查（`E2102`/`E2103`）；未知 Event、不存在或重复给出的参数名、多余或缺少的实参报 `E3202`；
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

`empty` 是上下文关键字（§12.3），只在 `slot_default` 位置解释。

Slot 类型只能是以上三种，其他类型报 `E2103`。`Slot<Node>` 必须由调用方填充，不能声明默认值（写 `= None`/`= empty` 报 `E3502`）；调用方未填充 `Slot<Node>` 同样报 `E3502`。Native Widget 的 Slot 由其 Native Schema 声明：容器（`Row`、`Column`、`Flex`、`Grid`、`Stack`、`Absolute`、`Fragment` 等）有 Default Slot `children: SlotList<Node>`，`Scroll` 有 Default Slot `content: OptionalSlot<Node>`，叶子 Widget（`Text`、`Button`、`TextInput` 等）没有 Slot。

```viso
@default slot content: Slot<Node>;
slot leading: OptionalSlot<Node> = None;
slot actions: SlotList<Node> = empty;
```

调用方使用 `fill`，而不是把 Slot 名当动态属性。

### 45.1 Default Slot

- 一个 Component 或 Template 至多一个 Slot 标记 `@default`，多于一个报 `E3004`；
- 调用方 Node Body 中不在 `fill` 内的结构项（`view_structure_item`，§48）按源码顺序进入 Default Slot，其结果必须满足该 Slot 的 Cardinality；
- 目标 Component 没有 Default Slot 时，出现裸结构项报 `E3003`；
- 同一 Node Body 同时出现裸结构项与 `fill <default_slot>` 报 `E3502`；
- `fill` 的名字不是目标声明的 Slot 报 `E3501`；同一 Node Body 对 `Slot`/`OptionalSlot` 多次 `fill` 报 `E3502`，对 `SlotList` 多次 `fill` 按源码顺序拼接；
- Cardinality 按结构项可能产生的节点数区间静态检查，区间内每个取值都必须满足 Slot 类型：普通节点计 1；`Fragment` 计其子项之和；`SlotOutlet` 计被转发 Slot 类型的区间；`if` 取各分支区间的并（无 `else` 时含 0）；`match` 取各分支区间的并；`for` 计零个或多个；
- `@default` 只改变调用方的书写方式，不改变 Slot 类型与 Schema 名称。

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

### 47.1 生成的 Native Schema

Native Schema 由 Rust 侧生成（ADR 0036）：一个 Native Library 是一条模块路径（如 `viso::text`）下带版本的函数与 Handle 类型集合；编译器与运行时共享同一个 Registry，`.vs` 不重复声明签名。标准 Registry 含 `viso::text`、`viso::math`、`viso::time`（`Stopwatch`）与 `viso::clipboard`。

- 每个函数记录：名称、`fn`/`action`/`task` 分类、参数与返回的 Schema 类型、所需 Capability、线程域（`any`/`ui`/`worker`）、`deterministic`、`realtime_safe` 与每次调用的预算成本；每个 Handle 类型记录方法、所有权（`shared`/`borrowed`）与线程域；
- 调用经 Import 或完整路径解析：`import viso::text;` 后写 `text::upper(s)`，`import viso::math::{clamp};` 后写 `clamp(x, 0.0, 1.0)`，`import viso::time::Stopwatch;` 后写 `Stopwatch::start()`；Handle 方法写 `watch.elapsed_ms()`，Receiver 作为第一个参数。未注册的路径或方法为 `E2001`；
- Effect 分类：`deterministic` 的 `fn` 为 Pure，其余 `fn` 为 Read，`action` 为 Action，`task` 为 Task；在 View/Computed 中调用 `action` 为 `E2502`；
- Capability 推断：调用所在的 Callable 直接需要该函数的 Capability，并沿调用图传递（§95）；
- `E6102`：`worker` 线程域的函数只能在 `task` Body 中调用；`borrowed` Handle 只在接收它的那次调用内有效，不得存入 `state`、`input`、`computed`、Event Payload、`const`、Record 字段或作为返回值，只能作为参数传递或保存在局部变量；
- 编译产物记录每个 Native Import 的路径、参数个数与签名哈希；运行前 `link` 到 Registry：路径未注册或签名不同为 `E6101`，不链接任何 Import；未链接即调用为 `E7105`；
- Native 返回错误或 Panic 为 `E7106` Runtime Fault 并回滚当前 Transaction；Native 已产生的外部副作用（如写剪贴板）不回滚；
- `viso schema` 以 §139 对象输出 Native Library、函数与 Handle 类型的 Schema。

---

# 第七部分：完整语法——View、节点、Template、Style 与 Theme

## 48. View 声明

```ebnf
view_decl            = "view", view_block ;

view_block           = "{", { view_structure_item }, "}" ;

view_structure_item  = { attribute },
                       ( named_node
                       | anonymous_node
                       | part_node
                       | view_if
                       | view_for
                       | view_match
                       | template_use ) ;
```

规则：

- 每个 Component 必须有且仅有一个 View；
- `view_block` 只包含结构项；Property Binding、`bind`、`on`、`fill`、`override part` 与 `replace part` 只出现在 Node Body（§49）；
- View 根 `view_block` 必须生成恰好一个根 Node；其他 `view_block`（`fill`、分支、`for` Body、Match Arm）的 Cardinality 由所在 Slot 决定（§53）；
- 多个根节点必须显式包裹 `Fragment`。`Fragment` 是标准 Component，只有 `@default slot children: SlotList<Node>`，不产生自身 Layout/Paint 节点，子节点直接参与父节点布局；
- View 是纯执行域；
- View 中禁止普通 `let`、赋值、`return`、`emit`、`start`、I/O 和 Native Action；
- View 可以读取 Input、State、Computed、Resource State 和当前 typed `env`；
- `env` 只在 View 执行域注入，普通 State/Computed 初始化器不得隐式依赖局部 Layout Environment；
- View 可以调用纯 `fn`；
- View 构建产生 UI IR，不直接执行 OS/GPU 副作用。

---

## 49. 子节点语法：删除 `child`

```ebnf
named_node           = "node", identifier, ":", component_type,
                       node_body ;

anonymous_node       = component_type, node_body ;

component_type       = type_path ;

node_body            = "{", { node_member }, "}" ;

node_member          = { attribute },
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
- `node name: T { ... }` 在当前 Component 的 Action、Task、Effect 与 Event Handler 作用域中引入不可变绑定 `name: NodeRef<T>`；View、Computed、Style 与 Input 默认值中不可见，出现时报 `E2001`；
- 读取 NodeRef 不建立 Reactive 依赖；
- 目标节点未挂载（分支未激活或已销毁）时，Node Action 返回 `Result` 错误而不是 Panic；
- `view_for` Body 内的 Named Node 不引入 NodeRef 绑定，其名称只作为 Keyed 身份种子；
- View 纯度规则禁止在 View 外直接修改节点属性作为状态源；
- 命令式焦点、滚动、测量等操作通过受控 Node Action 完成。

---

## 50. Property Binding

```ebnf
property_binding     = property_path, ":", expression, ";" ;

property_path        = label, { ".", label } ;
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
- Property Schema 必须声明 `invalidates` Dirty Class 集合（§87）；
- Binding 依赖变化时只触发所需失效；
- 同一 Property 在同一 Node Body 中绑定多次是错误；
- Style 与显式 Property 的优先级在 §59 定义。

---

## 51. Two-way Binding

```ebnf
two_way_binding     = "bind", property_path, "<=>", assignable_path,
                      [ "using", type_path ], ";" ;

assignable_path      = identifier, { ".", label | index_selector } ;
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
- 右侧必须是可赋值的 State Lens：以当前 Component 的 `state` 为根，经 `.label`/`[index]` 到达其字段或元素；
- 右侧不能是 Input、Computed、Const、局部绑定、Resource Payload 临时值或普通函数返回值，否则 `E3107`；
- 无 Converter 时两边类型必须相同（`E2103`），不做 §U1.3 的值提升或数值加宽；
- Converter 必须实现 `TwoWayConverter<Model, View>`；
- 更新必须带 Origin Token，禁止形成回声循环；
- 同一 Property 不能同时使用 `:` 单向绑定和 `bind`；
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
- Handler 的可选 Pattern 绑定整个 Event Payload，且必须不可反驳（`E2303`）；自定义 Component Event 的 Payload 是以其参数为字段的 Record；
- Handler 只能命名节点接受的 Event，否则报 `E3202`：用户 Component 节点接受标准事件（U7.1）与该 Component 声明的 Event；内置 Widget 接受其 Schema 声明的 Event（U3.7、U7.4、U7.5）及标准事件，不参与布局的 `FocusScope`、`KeyShortcut` 与 `Fragment` 不接受标准事件；
- 忽略 Payload 时省略括号；
- Handler 自动运行在 Action Transaction 中；
- 默认 Phase 由 Event Schema 定义，通常为 Target/Bubble；
- `capture` 和 `bubble` 显式覆盖默认 Phase；
- Event Payload 字段由 Schema 静态检查；
- Handler 中允许同步 State 修改、`emit`、Action Call 和 `start`；
- Handler 中禁止直接 `await`；
- Event 取消使用 Typed API：`event.stop_propagation()`、`event.stop_immediate_propagation()` 与 `event.prevent_default()`，语义见 §90。
- Handler 在 `component!`、`view!`、Hot Reload 与 Release Package 中以同一张 Handler 表挂载：每个 Handler 是所在 Component 的一个 Behavior Chunk，节点只登记 `(Event 路由, Handler 下标)`，State 写入在 Transaction 结束时回写 UI State Cell；
- 一次输入样本对每个节点只运行一次 Handler：未写 `capture` 的 Handler 在 Target 与 Bubble 段运行，祖先节点上的 Handler 不因 Capture 段再运行一次；
- 内置控件（`Toggle`/`CheckBox`、`Slider`、`Tabs`/`RadioGroup`、`TextInput`）的内建响应随同一张 Handler 表挂载：控件从 Handler 表中的纯表达式条目读取当前值与范围（`checked`、`value`、`min`、`max`、`step`、`selected`），据样本算出新值后投递 Schema 声明的 `changed`、`selected_changed` 或 `submitted`；
- `ui!` Fragment 没有 Component State，不能声明 Handler；Runtime 尚未投递的标准事件（如 `long_press`、`scroll`、`focus`）或 Behavior 无法 Lower 的 Handler 体报 `E3711`；Hot Reload 中出现 `E3711` 时保留 Last-good Handler；
- Handler 运行期 Fault 只中止该次调用、不回写任何 State，Fault 记录在宿主上。

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
- 不在 `fill` 内的裸结构项进入 Default Slot（§45.1）；
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
    UserPanel { user: user; }
} else {
    LoginPanel {}
}
```

规范：

- `preserve` 后必须是编译期 String Literal，否则报 `E3301`；
- 它不是普通 Key Expression；
- String 在当前 Component 的 Conditional Namespace 中必须唯一，重复使用报 `E3301`（附带首次使用位置）；
- 不写 `preserve` 时，离开分支会销毁其 Node、State、Effect、Task 和 Resource Scope；
- 写 `preserve` 时，离开分支会把分支实例移入受限缓存，回到该分支时原 Node 身份（`NodeId`）复用；
- 缓存容量和逐出策略由 Runtime Profile 控制；默认 Profile 每个分支缓存最近一个实例；
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
- Runtime 发现重复 Key 必须产生结构化 Fault（记录在宿主上），并拒绝提交该列表的 UI Patch：列表保留上一次提交的项；
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
        ProfileCard { user: user; }
    },
    UserState::error(error) => {
        ErrorView { message: error.message; }
    },
}
```

规则：

- `=>` 只在 Match Arm 中合法；
- Match 的穷尽与可达性检查与 Behavior Match 相同（§68、§70.3）：非穷尽报 `E2301`，不可达 Arm 报 `E2302` 警告；
- Guard 必须为纯 Bool Expression；
- 每个 Arm 的 View 输出必须满足同一 Slot Cardinality；
- Pattern Binding 的作用域仅限 Guard 和对应 View Block。

### 56.1 控制流区域的挂载（§54–§56 共用）

- `if`、`match` 与 Keyed `for` 在 `component!`、`view!`、Hot Reload 与 Release Package 中以同一张区域模板挂载：区域外的静态 Node 由目标直接构建，区域内的 Node 是模板，由 Runtime 在其所在父 Node 下挂载、切换和重排；
- 区域的分支选择、Scrutinee、Iterable 与 Key 各是所在 Component Handler 表中的一个纯 Region Entry Chunk，以外层区域的绑定为参数求值，不写 State、不产生 Event；
- 区域读取的 State 变化时，Runtime 在该帧 State Flush 之后重新求值并只修改变化的区域；读取 String/List 等 UI State Cell 无法容纳的 State 时，由修订计数 Cell 触发；
- 区域内 Node 的 Handler 以 Payload 加外层 `for`/`match` 绑定运行（§52）；
- Region Entry 运行期 Fault 保留当前结构并记录在宿主上；
- View 的根必须是 Node，根上的控制流区域报 `E3711`；`VirtualList` 尚不从 View 挂载内容，其子项报 `E3711`；`ui!` Fragment 没有 Component State，不能包含控制流区域（`E3711`）；
- Hot Reload 中出现 `E3711` 时保留 Last-good 区域；含控制流区域的 View 在 Hot Reload 中整树重建，按名称保留 State。

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
        ProfileBody { user: user; }
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

在 Template 定义内部，调用方 Slot 通过标准 `SlotOutlet` Component 放入结构。`SlotOutlet` 只有一个 Property `slot`，其值是当前 Template/Component 声明的 Slot 名；它就地展开调用方为该 Slot 提供的节点，未提供时展开为 Slot 默认值（`None`/`empty` 为零个节点）。`SlotOutlet` 只能出现在声明该 Slot 的 Template/Component 的 View 中，每个 Slot 至多一个 `SlotOutlet`。`fill` 只允许出现在 Template/Component 的调用方，不能用于定义 Slot Outlet。缺少 `slot`、其值不是单个标识符、或不是当前 Component 声明的 Slot 报 `E3501`；同一 Slot 的第二个 `SlotOutlet`，或位于 `for` 内（会把调用方节点放置多次）的 `SlotOutlet` 报 `E3502`。

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

`styles` 的精确类型由 Component Schema 定义，通常是 `List<StyleRef<Button>>`。Style 名在 Expression 中求值为编译期常量 `StyleRef<T>`，`T` 是其 `for` 目标 Component。编译器必须检查 Style 目标类型兼容性；Style 顺序按列表从左到右应用，后者覆盖前者，节点显式 Property 最后覆盖全部 Style。不存在通过字符串名称动态查找 Style 的语义。

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

Expression 中的 `theme` 是普通 identifier，由名称解析绑定到隐式注入的 Typed Reactive Context Binding（§12.6），不是 Grammar Production，也不是任意全局变量；它在 View、Style 与 Theme Expression 中可见。应用根节点或测试 Harness 必须提供一个与当前 Theme Schema 匹配的 Context Value；缺失 Context 是静态配置错误或应用启动错误。组件只能读取 `theme`，Theme 切换必须通过宿主 Context API 进行原子替换。

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

- `let` Pattern、`for` Pattern（Behavior 与 View）和 Closure 参数 Pattern 必须是 Irrefutable Pattern（§70.1），否则报 `E2303`；
- Statement Attribute 必须在 Schema 中声明可作用于对应 Statement Kind；例如 Shader 循环可使用 `@max_iterations(64)`，但同一 Attribute 作用于普通 `let` 时必须报错；
- Statement Attribute 只产生 HIR 元数据，禁止改变 Tokenization、优先级或基础控制流语义；
- Assignment 不是 Expression，禁止 `a = b = c;`；
- 赋值目标的根必须是 `state` 或 `let mut`/`mut` 绑定的局部名，经 `.label`/`[index]` 到达其字段或元素；以 Input、Computed、Const、Callable、不可变局部绑定（含参数与 Pattern 绑定）为根，或根不是名字（调用结果、`?.`、`?` 等），报 `E2110`；
- `return` 只允许在 Callable/Closure 中；
- `break value;` 只允许从 `loop` 返回值；
- `continue` 只允许在 Behavior Loop 中；
- 违反以上三条（`return`/`break`/`continue` 出现在不允许的位置，或 `break value;` 不在 `loop` 中）报 `E2803`；
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

logical_and_expression = comparison_expression,
                         { "&&", comparison_expression } ;

comparison_expression = bit_or_expression,
                        [ comparison_operator, bit_or_expression ] ;

comparison_operator  = "==" | "!=" | "<" | "<=" | ">" | ">=" ;

bit_or_expression    = bit_xor_expression,
                       { "|", bit_xor_expression } ;

bit_xor_expression   = bit_and_expression,
                       { "^", bit_and_expression } ;

bit_and_expression   = shift_expression,
                       { "&", shift_expression } ;

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

member_suffix        = ".", label ;

optional_member_suffix = "?.", label ;

try_suffix           = "?" ;
```

### 63.1 非结合操作符

比较与相等操作符（`== != < <= > >=`）同属一个 Non-associative 级别，与 Rust 一致；链式使用报 `E2802`：

```viso-invalid
0 < x < 10;
a == b == c;
a < b == c;
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

| 级别 | 构造                    | 结合性   | 说明                                     |
| ---: | ----------------------- | -------- | ---------------------------------------- |
|    1 | `()` `[]` `.` `?.` `?`  | 左       | Call、Index、Member、Optional Chain、Try |
|    2 | `! ~ + - await`         | 右       | Prefix Unary                             |
|    3 | `as`                    | 左       | 显式转换                                 |
|    4 | `* / %`                 | 左       | 乘除余                                   |
|    5 | `+ -`                   | 左       | 加减                                     |
|    6 | `<< >>`                 | 左       | 位移                                     |
|    7 | `&`                     | 左       | 位与                                     |
|    8 | `^`                     | 左       | 位异或                                   |
|    9 | `\|`                    | 左       | 位或                                     |
|   10 | `== != < <= > >=`       | 不结合   | 比较与相等（§63.1）                      |
|   11 | `&&`                    | 左、短路 | 逻辑与                                   |
|   12 | `\|\|`                   | 左、短路 | 逻辑或                                   |
|   13 | `??`                    | 右、短路 | Option/Nullable Coalesce                 |
|   14 | `.. ..=`                | 不结合   | Range                                    |

位运算高于比较（与 Rust 一致，不同于 C），因此 `flags & MASK == 0` 解析为 `(flags & MASK) == 0`。Assignment 不属于 Expression 优先级表。

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
if ready {}                              // 合法
if (Point { x: 1.0, y: 2.0 }) {}         // 可解析，随后通常因条件不是 Bool 报类型错
for item in (Query { limit: 10 }.run()) key item.id {}
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

record_initializer  = label, ":", expression
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
- Record Literal 与 View Node 由 Parser 上下文区分：Expression 位置的 `Type { ... }` 是 Record Literal，字段以 `,` 分隔；View 结构项位置的 `Type { ... }` 是 Node，Node Body 成员以 `;` 结束（§49、§50），两者不存在语法歧义。

---

## 66. Call 与 Argument

```ebnf
argument_list        = [ argument, { ",", argument }, [ "," ] ] ;

argument             = expression
                     | label, ":", expression ;

call_expression      = postfix_expression ;
```

规则：

- Positional Argument 必须出现在 Named Argument 之前；
- 同一参数不能重复提供；
- Named Argument 必须匹配 Callable Schema；
- 有默认值的参数可以省略；
- 方法调用 `receiver.method(args)` 在 HIR 中解析为带 Receiver 的 Call；
- Optional Member `value?.label` 与 Optional Member Call `value?.method()` 要求 `value` 为 `Option<T>`，结果为 `Option<R>`（`R` 本身为 `Option` 时不再嵌套）；`value` 为已知的非 `Option` 类型时报 `E2103`，附可机器应用的修复：改为 `.`；
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

- Match 必须穷尽，否则报 `E2301`，诊断列出至多 4 个缺失 Pattern（其余以 “and N more” 汇总）；
- 不可达 Arm 报 `E2302` 警告，CI Strict Mode 可提升为错误；
- 带 Guard 的 Arm 不参与穷尽性计算；
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

- 参数 Pattern 必须是 Irrefutable Pattern（§70.1）；
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

literal_pattern            = [ "-" ], integer_literal
                           | char_literal
                           | string_literal
                           | "true"
                           | "false"
                           | "None" ;

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

qualified_variant_pattern  = identifier, "::", label,
                             { "::", label } ;

record_pattern_field       = label, ":", pattern
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
Enum Variant（类型有多于一个构造形式时）
List Pattern
Or Pattern
```

分类是语法性的：Tuple/Record 只因其子 Pattern 而 Refutable；即使 Or Pattern 的分支合起来覆盖全部值（例如 `true | false`），它仍是 Refutable。

`let`、`for`（Behavior 与 View）和 Closure 参数只能使用 Irrefutable Pattern，否则报 `E2303`。`match`、`if let`（未来版本）和 Event Payload Handler 可以使用 Refutable Pattern；Event Handler Pattern 不匹配时该 Handler 被跳过。

消歧规则是强制性的：

- 裸单段 `identifier` 永远是 Binding Pattern；
- 无 Payload Enum Variant 必须写成至少两段的限定路径，例如 `LoadState::idle`；
- 带 Payload 的构造式必须紧跟 `(...)` 或 `{...}`，例如 `Option::Some(value)`、`Point { x, y }`；
- Parser 不得根据首字母大小写猜测“绑定还是 Variant”。

Pattern 类型规则（违反均报 `E2103`）：

- 整数 Literal 只匹配整数类型，且值必须在该类型范围内；浮点类型不能用 Pattern 匹配，必须用 `==` 比较；
- Char、String、Bool Literal 分别只匹配 `Char`、`String`、`Bool`；`None` 只匹配 `Option<T>`；
- Range Pattern 的两端必须是同类 Literal（整数或 Char），且下界不大于上界；空 Range 是错误；
- List Pattern 只匹配 `List<T>`，最多包含一个 `..`；
- Tuple Pattern 的元素数必须等于 Tuple 类型的元素数；`()` 匹配 `Unit`；
- 构造式 Pattern 的类型必须与被匹配类型兼容。

### 70.3 穷尽性

穷尽性检查按构造形式拆分值域：

- `Bool` 由 `true`、`false` 构成；`Option`/`Result` 与 Enum 由各自 Variant 构成；
- 整数类型按其完整取值范围、`Char` 按 Unicode Scalar Value 范围（`'\0'..='\u{D7FF}'` 与 `'\u{E000}'..='\u{10FFFF}'`）拆分为区间；Literal 与 Range Pattern 共同覆盖这些区间，相邻缺失区间在诊断中合并显示；
- `List<T>` 按长度拆分：`[a, b]` 只覆盖长度 2，`[a, .., b]` 覆盖长度 ≥ 2；
- `String` 的取值不可枚举，必须有 Wildcard 或 Binding Arm。

Pattern 存在类型错误时不再报告该 Match 的 `E2301`，避免级联诊断。

### 70.4 Or Pattern

Or Pattern 的每个分支必须绑定相同名称集合和兼容类型：

```viso
match state {
    LoadState::failed(error) | LoadState::cancelled(error) => {
        log_error(error);
    },
    _ => {},
}
```

---

## 71. `let`、Shadowing 与作用域

- `let` 默认不可变；
- 修改局部变量必须声明 `let mut`（或 Pattern/参数中的 `mut`），否则 `E2110`；
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
Dp Px Sp Em Percent MixedLength Duration Angle Frequency
```

长度族（`Dp`、`Px`、`Sp`、`Em`、`Percent`）与 `MixedLength` 的解析基准、运算与失效规则见 §19.3–§19.6。

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
Dp | Px | Sp | Em | Percent -> MixedLength
```

`Percent -> MixedLength` 只改变类型；目标 Property 是否接受 Percent 分量仍由其 `percent_basis` 检查（`E3104`）。Property Binding 右侧另有 Schema 驱动的值提升（`T -> Option<T>`、长度 -> `Sizing`/`EdgeInsets`），见 §U1.3；函数实参与 `let` 不适用。

### 76.2 禁止的隐式转换

```text
任意 signed <-> unsigned
任意 integer -> float
任意 float -> integer
F64 -> F32
宽整数 -> 窄整数
不同 UI 量纲互转（包括 MixedLength -> 任一具体单位）
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

```text
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

静态 Effect Check 必须发生在 HIR 阶段；模块级 `fn`/`action`/`task` 的函数体与 Component 成员同样检查。

写入 `state`（赋值目标的根是 `state`）与 `emit` 同属 Mutating：只有 Action/Event 函数体可以执行，View/Computed 中报 `E2502`，其他函数体报 `E2501`；Task 通过返回值交还结果。View 中的 `on` Handler 是独立的 Event 函数体，不属于 View 的 Reactive 上下文。State 初值、Input 默认值、`const` 值与 Record 字段默认值按 `fn` 的调用权检查。

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

Behavior 执行器（ADR 0035）产生的 Fault 及其诊断码：

| Fault                                   | 代码    |
| --------------------------------------- | ------- |
| 指令预算 / 调用深度 / Native 调用配额超限 | `E7101` |
| 内存预算超限                            | `E7102` |
| 整数溢出 / 除以零 / 移位量越界          | `E7103` |
| 索引越界                                | `E7104` |
| 函数无法运行（含编译错误、未链接的 Native）/ 缺少必需 Input / 内部不变量失败 | `E7105` |
| Native 调用缺少 Capability 授权         | `E6103` |
| Native 函数返回错误或 Panic             | `E7106` |

整数运算按其类型宽度检查：结果超出范围即 `E7103`，不回绕；移位量必须在 `0..bits` 内，移出宽度的位丢弃。浮点遵循 IEEE 754，`F32` 每步结果舍入到 `f32`。浮点插值文本使用最短可往返十进制形式（`1`、`0.1`、`inf`、`NaN`）。每个 Fault 携带所在函数与源 Span。

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
Adaptive Environment Cell
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
invalidates
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
10. 按 §87 的 Dirty Class 请求失效
```

同一外层 Transaction 中对同一 State 多次写入只产生一次 Revision 和一次下游调度。

写入就地发生：每个 State Slot 在一个 Transaction 内的首次写入把旧值记入 undo log。外层调用成功结束时，若有写入则 Revision 加一并标记被写 Slot 为 dirty；Action 体内不存在中途提交。嵌套调用（Action 调用 Action 或 `fn`）加入外层 Transaction。读取 Computed 是无写入的 Transaction，不增加 Revision。

失败时：

- State 恢复；
- 新 UI Patch 丢弃；
- Effect 不运行；
- Event Emit Buffer 丢弃或按 Event Schema 的 Failure Policy 处理；
- Runtime 返回结构化错误。

---

## 87. 精确失效

Dirty Class 是封闭集合，与 Runtime 位集一一对应，不得扩展：

| Dirty Class  | 含义                                   | 传播                           |
| ------------ | -------------------------------------- | ------------------------------ |
| `STRUCTURE`  | 子节点集合或顺序变化                   | 向祖先传播                     |
| `STYLE`      | Style 解析输入变化（状态选择器、交互态） | 本节点                         |
| `MEASURE`    | 固有尺寸变化                           | 向祖先传播到第一个 Layout Boundary |
| `LAYOUT`     | 需要重新布局本节点子树                 | 本节点                         |
| `TRANSFORM`  | 仅变换矩阵变化                         | 本节点                         |
| `PAINT`      | 仅绘制输出变化                         | 本节点                         |
| `HIT_TEST`   | 命中区域或可交互性变化                 | 本节点                         |
| `SEMANTICS`  | 无障碍语义树变化                       | 向祖先传播                     |

每个 Property Schema 必须声明 `invalidates: DirtyClass set`，例如：

```text
text:      invalidates MEASURE | LAYOUT | PAINT | SEMANTICS
color:     invalidates PAINT
width:     invalidates MEASURE | LAYOUT
transform: invalidates TRANSFORM | HIT_TEST | PAINT
```

运行时规则：

- 未声明 `invalidates` 的 Property 是 Schema 错误；
- Paint-only 修改不得触发 `MEASURE`/`LAYOUT`，也不得使祖先失效；
- Transform-only 修改只产生 `TRANSFORM | HIT_TEST | PAINT`，不得触发 `MEASURE`/`LAYOUT`；
- Semantics-only 修改不得重建 GPU Draw List；
- `HIT_TEST` 修改必须在下一次 Pointer Dispatch 前生效；
- Resource 变化不是 Dirty Class，而是 Resource Key Revision，经由读取它的 Binding 转换为上述类别；
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

## 95. Capability 与执行预算运行时

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

失败返回 `CapabilityDenied`（`E6103`），不得绕过到 Rust Panic。Capability Set 在链接 Native Import 时给定；缺少授权的 Import 仍然链接，调用时才产生 `E6103`，因此不调用该函数的代码不受影响。

AI 生成的 Preview 默认仅有：

```text
ui.basic
gpu.draw.sandboxed
asset.read.package
```

网络、文件系统、进程和剪贴板均需显式授权。

---

### 95.1 执行预算

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

计量口径：instruction budget 每执行一条指令计一单位；memory budget 计外层调用期间字符串、列表、聚合值与闭包分配的字节数；call depth 计嵌套调用帧数；native call quota 每次 Native 调用计一单位（默认 1024），超限为 `E7101`。每次外层调用从满额预算开始。

---

# 第十一部分：自适应布局与环境语义

## 96. Adaptive UI

## 96.1 设计原则

Viso 的自适应设计以 **可用空间和能力** 为核心，而不是以设备名称为核心。普通 UI 禁止把 `phone`、`tablet`、`desktop` 当成语言内建布局类别。

推荐决策顺序：

```text
Local Constraints
-> Adaptive Scope / Size Class
-> Safe Area / Keyboard / Display Features
-> Input & Accessibility Capabilities
-> 结构或属性响应
```

`Orientation` 可以查询，但普通应用布局 SHOULD 优先依赖可用宽高或 `SizeClass`。横屏只是几何变化的一种结果，不是默认布局策略。

---

## 96.2 Typed Adaptive Environment

每个 Component View 都可以读取只读、Typed、Reactive 的 `env`。`env` 不是动态字典，也不允许字符串查询。标准环境至少包含：

```text
env.window            : WindowMetrics
env.constraints       : LocalConstraints
env.size_class        : SizeClass
env.safe_area         : Insets
env.keyboard_inset    : KeyboardInset
env.display_features  : ReadOnlyList<DisplayFeature>
env.input             : InputCapabilities
env.text_scale        : F32
env.reduced_motion    : Bool
env.orientation       : Orientation
env.layout_direction  : LayoutDirection   // §U10.1
env.locale            : Locale            // §U10.3
```

这些符号由标准 Native Schema 注入，Parser 不新增专用关键字。`env` 是 View 执行域的上下文绑定，只能在 View item 的表达式、Property Binding、View `if`/`match`/`for` 条件和调用的纯函数参数中使用；Component 的普通 `state` / `computed` 初始化器不能隐式读取局部 Layout Environment。

典型类型：

```viso
record WindowMetrics {
    logical_size: Size;
    scale_factor: F32;
}

record LocalConstraints {
    min_width: Dp;
    max_width: Option<Dp>;
    min_height: Dp;
    max_height: Option<Dp>;
}

enum SizeClass {
    Compact;
    Medium;
    Expanded;
}

enum Orientation {
    Portrait;
    Landscape;
}

enum DisplayFeature {
    Hinge { bounds: Rect; };
    Fold { bounds: Rect; };
    Cutout { bounds: Rect; };
}
```

`WindowMetrics` 描述整个应用窗口；`LocalConstraints` 描述当前 Component 从父布局接收到的局部约束。两者语义不同，禁止互相替代。

---

## 96.3 Adaptive Scope 与 SizeClass

`env.size_class` 来自最近的 Adaptive Scope。根 Scope 默认以应用可用内容区域建立；标准 `AdaptiveScope` Widget 可以在局部重新建立 Scope，使侧栏、面板、分屏区域根据自己的实际宽度适配，而不是错误使用整个窗口宽度。

默认标准库可以提供 Compact/Medium/Expanded 策略，但 breakpoint 数值属于 Theme/Profile/Design System，不属于语言语法常量。应用可以提供自己的 `SizeClassPolicy`。

根 Adaptive Scope 必须有有限的可用宽度。局部 `AdaptiveScope` 若收到 `max_width = None` 的无界约束，默认继承父 Scope 的 `SizeClass`；只有显式提供有限 `basis` 时才建立新的 SizeClass。实现不得偷偷用设备型号或全局 Window Width 代替无界局部约束。

推荐：

```viso
view {
    match env.size_class {
        SizeClass::Compact => {
            CompactShell {}
        },
        SizeClass::Medium => {
            MediumShell {}
        },
        SizeClass::Expanded => {
            ExpandedShell {}
        },
    }
}
```

局部组件需要更精确的约束时，可以直接读取：

```viso
view {
    match env.constraints.max_width {
        Option::Some(width) if width < 520dp => {
            CompactToolbar {}
        },
        _ => {
            FullToolbar {}
        },
    }
}
```

禁止把平台名或硬件型号作为普通响应式布局的主要分支条件。

---

## 96.4 Reactive Dependency 与失效

Adaptive Environment Read 必须进入 Reactive HIR。编译器必须区分原始环境值和派生环境值。

最低失效合同：

| 环境值                 | 默认影响                                             |
| ---------------------- | ---------------------------------------------------- |
| `env.constraints`      | `MEASURE / LAYOUT`，结构分支读取时可追加 `STRUCTURE` |
| `env.size_class`       | 类别变化时 `STRUCTURE / MEASURE / LAYOUT`            |
| `env.safe_area`        | `MEASURE / LAYOUT`                                   |
| `env.keyboard_inset`   | `MEASURE / LAYOUT`                                   |
| `env.display_features` | 消费者声明的 `STRUCTURE / LAYOUT / HIT_TEST`         |
| `env.input`            | `STYLE / HIT_TEST`                                   |
| `env.text_scale`       | `MEASURE / LAYOUT / SEMANTICS`                       |
| `env.reduced_motion`   | `STYLE / PAINT`，不得强制 Layout                     |
| `env.orientation`      | 仅通知显式读取者                                     |

关键优化：如果 Window 从 1200dp 缩到 1100dp，但 `env.size_class` 仍为 `Expanded`，只读取 `size_class` 的结构分支 **不得** 因原始宽度变化重新构建。

---

## 96.5 Layout Phase 与自适应求值顺序

`LocalConstraints` 是父布局传入当前节点的 **incoming constraints snapshot**，不是当前节点测量后的输出尺寸。

推荐管线：

```text
Parent computes incoming constraints
-> Resolve Adaptive Environment
-> Re-evaluate affected adaptive bindings/branches
-> Measure affected subtree
-> Layout
-> Publish final geometry
```

### 96.5.1 结构级响应 vs 属性级响应

自适应求值区分两类响应，边界由「该值在本节点测量之前是否已确定」划定：

- **结构级响应**：`if` / `match` / `for` 等增删节点、改变子树形状的分支。
- **属性级响应**：树形状不变，只改已存在节点的属性（尺寸、间距、颜色等）。

规则：

1. **结构级分支只能依赖父级已确定的 incoming 值**——即 `env.constraints`（父布局传入的 `LocalConstraints` snapshot），以及由最近一层已求解 Adaptive Scope 导出的 `env.size_class`、`env.orientation`、`env.safe_area`、`env.input` 等。这些量在本节点进入 Measure 之前一定已知，用它们切结构不会形成循环。
2. **本节点自身测量之后才知道的尺寸**——measured/content size、子节点撑开的尺寸、Layout 中途才定的最终 geometry——**只能驱动属性级响应，不得驱动结构级分支**。
3. `AdaptiveScope` 的 `basis` 必须来自父级已确定的 incoming constraints，不得取该 Scope 自身测量内容的输出尺寸；否则 Scope 会依赖它自己所决定的结构，构成隐式循环。

因此结构在一次布局中「先由 incoming 值定形，再测量、再布局」是单向的：结构级分支的输入在 Measure 前已冻结，Measure 只能反过来影响属性，不能倒推回结构。

禁止 View 结构直接依赖其自身尚未完成的 measured output。`AdaptiveCycle` 检测是对以上规则违例的诊断安全网，而非切换结构的主要机制——实现必须检测有限布局周期内的 `AdaptiveCycle`，并产生结构化诊断 `E4204`，而不是无限反复 Measure。

---

## 96.6 Safe Area、Keyboard 与 Foldable

标准 Widget/Profile 至少提供：

```text
SafeArea
KeyboardAvoiding
AdaptiveScope
AdaptiveSplit
AdaptiveNavigation
ResponsiveGrid
```

这些是 Widget/Native Schema，不是语言关键字。

示例：

```viso
view {
    SafeArea {
        KeyboardAvoiding {
            AdaptiveNavigation {
                fill content {
                    RouterView {}
                }
            }
        }
    }
}
```

Fold/Hinge/Cutout 必须通过 `env.display_features` 暴露为 typed geometry。业务代码不应直接解析平台私有字符串。

---

## 96.7 Input 与 Accessibility Adaptive

自适应不仅是宽度。标准 `InputCapabilities` SHOULD 至少描述：

```text
primary_pointer_precision
hover_available
keyboard_available
touch_available
pen_available
gamepad_available
```

因此组件可以做能力适配：

```viso
if env.input.touch_available && !env.input.hover_available {
    TouchToolbar {}
} else {
    PointerToolbar {}
}

if env.input.hover_available {
    HoverHints {}
}
```

`text_scale` 和 `reduced_motion` 必须作为环境依赖进入相应失效平面，不能通过平台 `cfg` 分支绕过 UI 语义。

---

## 96.8 State Preservation Across Adaptive Branches

响应式切换不能把业务状态和局部交互状态混在一起处理。规范要求：

- 需要跨 Compact/Expanded 长期存在的业务状态 SHOULD 提升到分支外的 Component/System State；
- 离开某个布局模式后返回仍需恢复该分支局部状态时，使用该分支自己的 `preserve`；
- 官方 `AdaptiveNavigation` / `AdaptiveSplit` 等容器 SHOULD 在模式变化时保留传入 Content Slot 的 Stable Node Identity，而不是无条件销毁子树；
- focus、selection、scroll、text editing 等局部状态只有在 Widget Schema 声明可迁移/可重挂载时才能跨 Shell 保留。

合法示例：

```viso
view {
    if env.size_class == SizeClass::Compact preserve "compact-shell" {
        CompactShell {
            MainContent { model: model; }
        }
    } else {
        ExpandedShell {
            MainContent { model: model; }
        }
    }
}
```

这里 `model` 属于分支外状态，所以切换布局不会丢失业务数据；`compact-shell` 自己的局部 UI 状态在离开后可以进入 Preserve Cache。需要真正跨 Shell 维持同一个 Child Node 实例时，应使用具备 identity-preserving slot contract 的标准 Adaptive Container，而不是依赖两个不同 Conditional Branch 自动合并身份。

---

## 96.9 Adaptive Authoring 规则

人类和 AI SHOULD：

1. 优先使用 `env.size_class` 或 `env.constraints`；
2. 只有确实与方向本身相关时使用 `env.orientation`；
3. 不用 `platform == ios/android` 决定普通页面结构；
4. 使用 `SafeArea`、`KeyboardAvoiding` 处理系统 Insets；
5. 对 Foldable 使用 `display_features`，不猜测设备型号；
6. 共享内容在不同 Shell 间切换时显式考虑 identity/preserve；
7. breakpoint 集中到 Theme/Profile/Design System，而不是散落 magic numbers。

---

## 96.10 Adaptive 验收场景

标准测试矩阵至少覆盖：

```text
phone portrait
phone landscape
tablet full screen
tablet split screen
desktop narrow window
desktop wide window
keyboard shown/hidden
safe-area change
fold/hinge geometry
text scale change
mouse+keyboard vs touch input
```

同一组件在不同窗口宽度和父容器宽度下必须可以独立测试，不允许只依赖真实设备型号。

---

# 第十一部分 B：UI Authoring 标准面

本部分章节以 `U` 编号（§U1–§U13），不占用主编号序列。

## U1. 范围与规范层级

本部分定义 `.vs` 编写 UI 的标准面：用户 Component 属性元数据、布局、变换、文本、动画、输入与焦点、语义、虚拟化列表、国际化、导航、状态选择器与标准 Theme Schema。

本部分**不引入新语法**，只使用既有构造：Property Binding `name: expr;`（§50）、`on event { }`（§52）、`node n: T { }`（§49）、`for x in xs key k { }`（§55）、`bind p <=> s;`（§51）、Attribute（§24）、Record/Enum 表达式（§65）、Style/Theme（§59、§60）。标识符遵循关键字分层（§12，ADR 0034）：`theme`、`env` 是解析到注入 Context 的普通标识符；Property Path 段、Record 字段、Enum Variant、具名参数标签都是 Label Position，因此 `EdgeInsets.start`、`Align::end`、`semantics.role` 合法。

### U1.1 规范层级

- **语言级规则**（对所有 Component 生效）：Property Group 与父节点提供 Property 的解析、用户 Component 元数据 Attribute 与失效推导、事件分派、焦点顺序、语义义务、`for` 与 `VirtualList` 的挂载语义、本部分新增诊断；
- **`viso::widgets` 标准 Schema 基线**：标准 Widget 的 Property 与 Event，每个条目给出名称、类型、默认值、失效类别与适用时的 `percent_basis`（§19.4）；
- 失效类别只使用八个 Dirty Class：`STRUCTURE STYLE MEASURE LAYOUT TRANSFORM PAINT HIT_TEST SEMANTICS`；
- 基线是**下限**：实现不得删除、改名或改变基线条目的类型、默认值、失效类别；可以增加 Property，但必须经 Schema Query（§139）公开；
- 每个 Widget 的完整 Schema（额外 Property、Slot 基数、Event Payload 全部字段）以 `viso schema query` 为准；
- 标注 **[Runtime 待实现]** 的条目：Schema 与类型检查已冻结，运行时行为尚未实现；实现落地前 Debug Runtime 对非默认值报告 `E3709`，Release 构建拒绝该值。

### U1.2 失效表记法

- 表中失效列是该 Property 变化时源节点被标记的 Dirty Class；
- Layout Pass 后位置或尺寸实际改变的节点由管线派生 `TRANSFORM|HIT_TEST|PAINT`，表中不重复列出；
- `STRUCTURE`、`MEASURE`、`SEMANTICS` 沿祖先冒泡，其余类别保持局部（ADR 0005）；
- 长度值含 `px`、`sp`、`em` 分量时追加 §19.6 依赖；
- 任何 Schema 不得声明 `INTERACTION` 或 `affects(...)`；纯变换变化一律是 `TRANSFORM|HIT_TEST|PAINT`。

### U1.3 Property 值提升

仅在 Property Binding 与 Record 字段初始化的右侧，Schema 类型允许以下隐式提升（均为编译期、无副作用）：

- `T` → `Option<T>`，提升为 `Option::Some(v)`，因此 `max_width: 640dp;`、`semantics.label: tr("x");` 合法；
- 长度族值与 `MixedLength` → `Sizing`，提升为 `Sizing::fixed(v)`（U3.2）；
- 长度族值与 `MixedLength` → `EdgeInsets`，提升为 `EdgeInsets::all(v)`（U3.3）。

一次绑定中 §76.1 拓宽与上述提升至多各发生一次（如 `Dp` → `MixedLength` → `Option<MixedLength>`）；提升不适用于函数实参与 `let`。

### U1.4 明确推迟

Flex 换行；`space_between`/`space_around`；单子节点 `align_self`；Row Baseline 对齐；滚动条外观、惯性、Scroll Snap（ADR 0007）；单个 `VirtualList` 多 Item Template；物理方向（`left`/`right`）Edge 写法；显式段落基础方向 Property；Grid Subgrid 与 Line Name 的 DSL 写法；共享元素转场与路由动画；自定义 Gesture Recognizer 声明；富文本 Span 树。

---

## U2. 用户 Component 的 Property 元数据

用户 Component 的公开面由 `input`、`event`、`slot` 组成（§41–§45）。除 `@default`（§45.1）外，本节定义三个标准 Attribute，只附加在 Component 成员声明上，不改变成员语法：

| Attribute          | 位置                                          | 作用                           |
| ------------------ | --------------------------------------------- | ------------------------------ |
| `@bindable(event)` | `input`                                       | 与 Event 配对为双向属性         |
| `@styleable`       | `input`                                       | 允许 Style 绑定该 Input         |
| `@selector`        | `Bool` 类型的 `input`/`state`/`computed`       | 公开为 Style 状态选择器         |

### U2.1 Default Slot

Default Slot 由 `@default` 标记，规则见 §45.1；示例见 U13.1 的 `SettingsSection`。裸子节点数量必须满足 Default Slot 基数（`Slot` 恰好一个，`OptionalSlot` 零或一个，`SlotList` 任意），否则 `E3502`。

### U2.2 `@bindable`：Input 与 Event 配对

```viso
export component Stepper {
    @bindable(changed)
    input value: I64;
    input step: I64 = 1;

    event changed(value: I64);

    view {
        Row {
            Button { text: "-"; on click { emit changed(value - step); } }
            Text { text: format("{}", value); }
            Button { text: "+"; on click { emit changed(value + step); } }
        }
    }
}

// 调用方
Stepper { bind value <=> settings.retry_count; }
```

- `@bindable(e)` 使该 Input 在 Schema 中成为 `two_way`，可作 §51 `bind` 左侧；
- `@bindable` 只能标注 `input`，参数恰好是一个 Event 名；`e` 必须在同一 Component 中声明，且首个参数类型与 Input 类型相同；违反任一条是 `E3701`；
- Component 内部不得给 Input 赋值（§41），只能 `emit e(新值)` 表达"请求变更"；
- `bind` 降级为 `value: lens;` 加 `on e(ev) { lens = ev.value; }`，并带 Origin Token 防回写（§123）；调用方只写 `value: x;` 时是受控单向属性，`changed` 仍会触发；
- 标准 Widget 约定：主值的配对事件名为 `changed`，其他双向属性为 `<property>_changed`（如 `selected_changed`）。

### U2.3 Styleable Input 与状态选择器

```viso
export component Chip {
    @styleable
    input tint: Option<Color> = Option::None;
    input label: String;

    @selector
    input selected: Bool = false;

    view {
        Row {
            background: tint.unwrap_or(theme.colors.surface);
            Text { text: label; }
        }
    }
}

export style SelectedChip for Chip {
    when selected { tint: Option::Some(theme.colors.accent); }
}
```

- 用户 Component 中只有 `@styleable` Input 是 Styleable；Style 绑定其他 Input 是 §59 既有错误；
- `@selector` 公开的选择器名等于成员名；成员非 `Bool`，或与 U12.1 标准选择器同名但语义不同，是 `E3710`；
- 内部 `state` 可经 `@selector` 公开为只读选择器，外部仍不能读写该 State。

### U2.4 用户 Component 的失效推导

用户 Component **不声明**失效类别。编译器从 View 对每个 Input 的读取位置推导，并写入 Schema：

| 读取位置                                        | 贡献                                    |
| ----------------------------------------------- | --------------------------------------- |
| 标准或原生 Widget 的 Property Binding           | 该 Property 的失效类别                  |
| 子用户 Component 的 Input                       | 该 Input 的推导结果                     |
| `if`/`match` 条件、`for` 迭代源、`key` 表达式   | `STRUCTURE`                             |
| `@selector` 成员被 Style `when` 引用            | `STYLE` 加该 `when` 块内 Property 的类别 |
| 经 `computed` 间接读取                          | 沿依赖图传递                            |

- 推导是跨 Component 的最小不动点，结果确定；它是 Schema 的信息字段，不属于接口兼容性合同；
- 源码中不存在 `affects(...)`、手写 Dirty Class 或 `INTERACTION`；
- `viso schema query` 必须按 Input 输出推导结果，例如 `title: MEASURE|LAYOUT|PAINT|SEMANTICS`。

---

## U3. 布局模型

### U3.1 模型与容器

布局是约束下行、尺寸上行的单次遍历（ADR 0003）：父节点向子节点传递每轴可用上界（可无界），子节点返回 Border Box 尺寸，父节点决定其位置。只有带 `MEASURE`/`LAYOUT` 的脏子树重新布局；滚动、变换与动画不触发布局。所有方向都是逻辑方向：`start`/`end` 为行内方向并随 `env.layout_direction` 翻转，`top`/`bottom` 为块方向。

| Widget     | 语义                                           | 默认 `width` / `height` |
| ---------- | ---------------------------------------------- | ----------------------- |
| `Row`      | Flex，主轴为行内方向                           | `fit` / `fit`           |
| `Column`   | Flex，主轴为块方向                             | `fit` / `fit`           |
| `Flex`     | 主轴由 `axis` 决定                             | `fit` / `fit`           |
| `Stack`    | 子节点叠放于同一框内，尺寸取子节点最大值        | `fit` / `fit`           |
| `Absolute` | 子节点按 Inset 定位，不贡献容器尺寸            | `fill` / `fill`         |
| `Grid`     | Track 网格（ADR 0009）                         | `fill` / `fill`         |
| `Scroll`   | 单子节点视口（ADR 0007）                       | `fill` / `fill`         |
| `Fragment` | 零节点分组：子节点直接拼接进父节点 Slot，不接受 Property，不产生 NodeId | — |

### U3.2 Sizing

```viso
export enum Sizing { fixed(MixedLength); fill { weight: F32 = 1.0; } fit; min_content; max_content; }
```

按 U1.3，`width: 100% - 32dp;` 提升为 `Sizing::fixed(...)`。

| 值                | Flex 主轴                              | 交叉轴 / 非 Flex 父节点 |
| ----------------- | -------------------------------------- | ----------------------- |
| `fixed(v)`        | 按 §19.4 解析，再按 min/max 截断       | 同左                    |
| `fill { weight }` | 按 `weight` 比例分配剩余空间           | 占满可用尺寸            |
| `fit`             | 内容尺寸，受可用尺寸约束               | 同左                    |
| `min_content`     | 最小内容尺寸（文本：最长不可断片段）   | 同左 **[Runtime 待实现]** |
| `max_content`     | 不换行时的内容尺寸                     | 同左 **[Runtime 待实现]** |

`fill` 在无界轴（如 Scroll 滚动轴）退化为 `fit` 并报告 `E3105`；`100%` 不等于 `fill`（§19.4）。

### U3.3 公共布局 Property

适用于除 `Fragment` 外的所有布局节点：

| Property        | 类型                  | 默认                 | 失效              | percent_basis                 |
| --------------- | --------------------- | -------------------- | ----------------- | ----------------------------- |
| `width`         | `Sizing`              | 见 U3.1 / Widget     | `MEASURE\|LAYOUT` | 父内容盒可用宽度              |
| `height`        | `Sizing`              | 见 U3.1 / Widget     | `MEASURE\|LAYOUT` | 父内容盒可用高度              |
| `min_width`     | `MixedLength`         | `0dp`                | `MEASURE\|LAYOUT` | 父内容盒可用宽度              |
| `max_width`     | `Option<MixedLength>` | `Option::None`       | `MEASURE\|LAYOUT` | 父内容盒可用宽度              |
| `min_height`    | `MixedLength`         | `0dp`                | `MEASURE\|LAYOUT` | 父内容盒可用高度              |
| `max_height`    | `Option<MixedLength>` | `Option::None`       | `MEASURE\|LAYOUT` | 父内容盒可用高度              |
| `padding`       | `EdgeInsets`          | `EdgeInsets::zero()` | `MEASURE\|LAYOUT` | `start`/`end` 父宽，`top`/`bottom` 父高 |
| `margin`        | `EdgeInsets`          | `EdgeInsets::zero()` | `MEASURE\|LAYOUT` | 同 `padding` **[Runtime 待实现]** |
| `background`    | `Option<Color>`       | `Option::None`       | `PAINT`           | —                             |
| `corner_radius` | `MixedLength`         | `0dp`                | `PAINT`           | 自身 Border Box 较短边        |
| `border`        | `Option<Border>`      | `Option::None`       | `PAINT`           | —                             |
| `styles`        | `List<StyleRef<T>>`   | `[]`                 | `STYLE`           | —                             |

```viso
export record EdgeInsets { top: MixedLength = 0dp; end: MixedLength = 0dp; bottom: MixedLength = 0dp; start: MixedLength = 0dp; }
export record Border { width: Dp = 1dp; color: Color; }
```

- 构造函数：`EdgeInsets::zero()`、`EdgeInsets::all(v)`、`EdgeInsets::axes(inline: a, block: b)`（`a` 用于 `start`/`end`，`b` 用于 `top`/`bottom`）；
- 按 U1.3，§19.5 的 `padding: 1em;` 提升为 `EdgeInsets::all(1em)`；
- `margin` 不折叠；`border` 在 Border Box 内侧描边，不参与布局；
- `min` 大于 `max` 时以 `min` 为准，Debug Runtime 报告 `E3105`；
- `start`/`end` 在布局时按 `env.layout_direction` 解析为物理边。

### U3.4 Flex 容器（`Row` / `Column` / `Flex`）

| Property  | 类型          | 默认                     | 失效              | percent_basis      |
| --------- | ------------- | ------------------------ | ----------------- | ------------------ |
| `axis`    | `Axis`        | `Axis::row`（仅 `Flex`） | `MEASURE\|LAYOUT` | —                  |
| `gap`     | `MixedLength` | `0dp`                    | `MEASURE\|LAYOUT` | 自身内容盒主轴尺寸 |
| `justify` | `Justify`     | `Justify::start`         | `LAYOUT`          | —                  |
| `align`   | `Align`       | `Align::start`           | `LAYOUT`          | —                  |

- `Axis { row; column; }`；`Justify { start; center; end; }` 为主轴整体分布；`Align { start; center; end; stretch; }` 为交叉轴对齐，`stretch` 只作用于交叉轴 Sizing 为 `fit` 的子节点；
- `Row` 主轴 `start` 是行内起点，RTL 下首个子节点位于右侧；
- `justify` 与 `align` 不改变容器自身测量结果，只标记 `LAYOUT`。

### U3.5 Grid

`fr` 不是长度单位（§19），而是 Track 构造器：

```viso
export enum Track { fixed(MixedLength); fr(F32); auto; minmax(MixedLength, TrackMax); fit_content(MixedLength); }

export enum TrackMax { length(MixedLength); fr(F32); }
export enum AutoRepeat { fill; fit; }

export record AdaptiveColumns { mode: AutoRepeat = AutoRepeat::fill; min: MixedLength; max: TrackMax = TrackMax::fr(1.0); }
```

标准函数 `tracks(count: U16, track: Track) -> List<Track>` 对应 CSS `repeat(n, t)`，在编译期或节点创建时展开。

| Property           | 类型                      | 默认                 | 失效              | percent_basis  |
| ------------------ | ------------------------- | -------------------- | ----------------- | -------------- |
| `columns`          | `List<Track>`             | `[]`                 | `MEASURE\|LAYOUT` | 自身内容盒宽度 |
| `rows`             | `List<Track>`             | `[]`                 | `MEASURE\|LAYOUT` | 自身内容盒高度 |
| `auto_rows`        | `Track`                   | `Track::auto`        | `MEASURE\|LAYOUT` | 自身内容盒高度 |
| `column_gap`       | `MixedLength`             | `0dp`                | `MEASURE\|LAYOUT` | 自身内容盒宽度 |
| `row_gap`          | `MixedLength`             | `0dp`                | `MEASURE\|LAYOUT` | 自身内容盒高度 |
| `align_items`      | `GridAlign`               | `GridAlign::stretch` | `LAYOUT`          | —              |
| `areas`            | `List<String>`            | `[]`                 | `MEASURE\|LAYOUT` | —              |
| `adaptive_columns` | `Option<AdaptiveColumns>` | `Option::None`       | `MEASURE\|LAYOUT` | 自身内容盒宽度 |

子节点 Property（父节点提供，U3.8），失效均为 `MEASURE|LAYOUT`：`grid.column: Option<U16> = Option::None`、`grid.row: Option<U16> = Option::None`、`grid.column_span: U16 = 1`、`grid.row_span: U16 = 1`、`grid.area: Option<String> = Option::None`。

- `GridAlign { stretch; start; center; end; }`；
- 行列号从 0 开始；先放置显式定位的子节点，其余按行优先自动流入空位；隐式行使用 `auto_rows`；
- `adaptive_columns` 存在时覆盖 `columns`，列数由可用宽度决定（ADR 0009）；
- `areas` 每个字符串是一行，名字以空白分隔；`grid.area` 引用未知名字或名字构成非矩形区域时，Debug Runtime 报告 `E3105`。

### U3.6 Stack 与 Absolute

**Stack**：

| Property        | 类型                  | 默认                     | 失效              |
| --------------- | --------------------- | ------------------------ | ----------------- |
| `content_align` | `Alignment2D`         | `Alignment2D::top_start` | `LAYOUT`          |
| `stack.align`   | `Option<Alignment2D>` | `Option::None`           | `LAYOUT`          |
| `stack.layer`   | `I32`                 | `0`                      | `PAINT\|HIT_TEST` |

- `Alignment2D { top_start; top; top_end; start; center; end; bottom_start; bottom; bottom_end; }`；
- Paint 顺序先按 `stack.layer` 升序、同层按源码顺序（稳定排序）；Hit Test 按 Paint 逆序；
- `stack.layer` 不改变布局、语义与焦点顺序（二者始终按源码顺序，U7.2），不能用于重排可访问的阅读顺序；
- **[Runtime 待实现]**。

**Absolute** 子节点 Property：`absolute.top`、`absolute.bottom`、`absolute.start`、`absolute.end`，类型均为 `Option<MixedLength>`、默认 `Option::None`、失效 `LAYOUT`；`percent_basis` 为容器内容盒高度（`top`/`bottom`）或宽度（`start`/`end`）。

- 某轴两端都给出且该轴 Sizing 为 `fit` 时，子节点在该轴上拉伸；两端都未给出时贴 `start`/`top`；
- 子节点不影响容器尺寸，所以 Inset 变化只标记 `LAYOUT`；频繁移动的元素应该用 `translate`（U4），不要逐帧改 Inset；
- **[Runtime 待实现]**。

### U3.7 Scroll

- `axis: ScrollAxes = ScrollAxes::vertical`（`vertical; horizontal; both;`），失效 `MEASURE|LAYOUT`；
- 事件 `scroll_changed(ScrollChanged)`，字段为 `offset: Offset`、`viewport: SizeDp`、`content: SizeDp`（`SizeDp { width: Dp; height: Dp; }`）；
- 子节点在滚动轴上的约束无界；滚动偏移变化只标记视口子树 `TRANSFORM|HIT_TEST|PAINT`，不触发布局（ADR 0007）；
- 滚轮与触控板事件先送往最内层可在该轴滚动的视口，到达边界后沿祖先链传递；
- 命令式滚动只经 NodeRef（U7.6）。

### U3.8 Property Group 与父节点提供的 Property

多段 Property Path 只在两种情况下合法：

- **Schema 声明的 Property Group**（`semantics.*`、`transition.*`）：每个成员是独立 PropertyId，有自己的类型与失效类别；
- **父节点提供的 Property**（`grid.*`、`stack.*`、`absolute.*`）：由直接父节点的 Schema 声明、写在子节点 Node Body 中。编译器在同一 View Block 内静态确定父节点类型后检查前缀；父节点不提供该前缀，或父节点无法静态确定（例如子节点经 Slot 转发到未知容器、位于用户 Component 的 Default Slot 中），是 `E3702`；
- `Fragment`、`if`、`match`、`for` 不形成父节点，其子节点使用外层真实父节点提供的 Property。

---

## U4. 变换、透明度与裁剪

| Property           | 类型          | 默认                  | 失效                         |
| ------------------ | ------------- | --------------------- | ---------------------------- |
| `translate`        | `Offset`      | `Offset::zero()`      | `TRANSFORM\|HIT_TEST\|PAINT` |
| `scale`            | `F32`         | `1.0`                 | `TRANSFORM\|HIT_TEST\|PAINT` |
| `rotation`         | `Angle`       | `0deg`                | `TRANSFORM\|HIT_TEST\|PAINT` |
| `transform_origin` | `Alignment2D` | `Alignment2D::center` | `TRANSFORM\|HIT_TEST\|PAINT` |
| `opacity`          | `F32`         | `1.0`                 | `PAINT`                      |
| `clip`             | `Bool`        | `false`               | `PAINT\|HIT_TEST`            |
| `visible`          | `Bool`        | `true`                | `PAINT\|HIT_TEST\|SEMANTICS` |

- `Offset { x: MixedLength = 0dp; y: MixedLength = 0dp; }` 不声明 `percent_basis`，含 Percent 分量是 `E3104`；
- 变换在布局之后应用，不改变任何节点的布局尺寸与兄弟位置；Hit Test 使用变换后的几何；`translate.x` 是物理方向，不随 RTL 翻转；
- `opacity` 截断到 `[0, 1]`；`opacity: 0.0` 的节点仍参与 Hit Test 与语义；
- `visible: false` 保留布局空间，但不绘制、不参与命中测试，并从语义树移除；需要同时移除布局空间时使用 `if`；
- `clip: true` 按自身 Border Box 与 `corner_radius` 裁剪子节点的绘制与命中测试；滚动视口总是裁剪。

---

## U5. 文本

`Text` 基线（`TextInput` 共享其中的样式条目）：

| Property      | 类型                  | 默认                          | 失效                                | percent_basis             |
| ------------- | --------------------- | ----------------------------- | ----------------------------------- | ------------------------- |
| `text`        | `String`              | `""`                          | `MEASURE\|LAYOUT\|PAINT\|SEMANTICS` | —                         |
| `font_size`   | `MixedLength`         | `1em`（继承）                 | `MEASURE\|LAYOUT\|PAINT`            | 父节点 Resolved Font Size |
| `font_weight` | `FontWeight`          | `FontWeight::regular`         | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `font_family` | `Option<FontFamily>`  | `Option::None`（Theme）       | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `line_height` | `Option<MixedLength>` | `Option::None`（字体度量）    | `MEASURE\|LAYOUT\|PAINT`            | 当前 Resolved Font Size   |
| `color`       | `Color`               | `theme.colors.foreground`     | `PAINT`                             | —                         |
| `soft_wrap`   | `Bool`                | `false`                       | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `max_lines`   | `Option<U32>`         | `Option::None`                | `MEASURE\|LAYOUT\|PAINT` **[Runtime 待实现]** | —               |
| `overflow`    | `TextOverflow`        | `TextOverflow::clip`          | `LAYOUT\|PAINT` **[Runtime 待实现]** | —                        |
| `align`       | `TextAlign`           | `TextAlign::start`            | `LAYOUT\|PAINT`                     | —                         |
| `selectable`  | `Bool`                | `false`                       | `HIT_TEST\|SEMANTICS`               | —                         |
| `locale`      | `Option<Locale>`      | `Option::None`（`env.locale`）| `MEASURE\|LAYOUT\|PAINT`            | —                         |

- `FontWeight { thin; light; regular; medium; semibold; bold; heavy; }`；`TextOverflow { clip; ellipsis; }`；`TextAlign { start; center; end; }`（`start`/`end` 随段落方向解析，`justify` 推迟）；
- Schema 默认值列中的 `theme.*` 表示未绑定时读取 Theme Context；
- **Typography Context**（§19.4）：`Text` 声明 `font_size`，设置时成为子树 Resolved Font Size 的来源；容器不声明 `font_size`，只向下传递；根节点取 `theme.typography.base_size`（默认 `14sp`）；`font_size` 变化按 §19.6 追加标记依赖当前 Resolved Font Size 的后代 Binding；
- 源文本始终为逻辑顺序；Bidi、断行、字形选择与回退由 Text Runtime 负责，DSL 不得反转字符串或手工插入方向控制符；
- 仅因宽度变化引起的重排不标记 `SEMANTICS`（ADR 0027）；
- `overflow: ellipsis` 仅在 `max_lines` 有值或 `soft_wrap: false` 时生效；
- `text` 在 Schema 中标记为 Localizable（U10.3）。
- `String` 文本槽（`text`、`label`、`placeholder` 等）只接受 `String`（或 `Option<String>` 槽的 U1.3 提升），不做隐式 `Display` 转换：`text: count;`（`count: I64`）是 `E2103`，并附 Machine-Applicable Fix `format("{}", count)`；对无 `Display` 的类型不给出该 Fix。

---

## U6. 动画与转场

### U6.1 `transition` Property Group

```viso
export record Transition {
    duration: Duration = 200ms; delay: Duration = 0ms;
    easing: Easing = Easing::ease_out; reduced: ReducedMotion = ReducedMotion::instant;
}

export enum Easing { linear; ease_in; ease_out; ease_in_out; }
export enum ReducedMotion { instant; keep; }
```

```viso
Row {
    background: if selected { theme.colors.accent } else { theme.colors.surface };
    translate: Offset { x: 0dp, y: lift };
    transition.background: Transition { duration: theme.motion.short };
    transition.translate: Transition { duration: theme.motion.medium, easing: Easing::ease_in_out };
}
```

- `transition.p` 作用于同一节点的 Property `p`；`p` 必须在 Schema 中标记为 Animatable，且值类型必须是 `Transition`，否则 `E3703`；
- 挂载后 `p` 的解析值变化才触发动画；首次挂载与 `if`/`for` 新挂载的节点直接呈现目标值；
- Model 值立即变化并始终是事实来源，呈现值在 Duration 内插值；进行中再次变化时从当前呈现值重新出发（Retarget）；
- 变换类 Property 的 Hit Test 使用呈现值；其他 Property 的 Hit Test 与语义使用 Model 值；
- `transition.*` 自身的变化只影响下一次触发，不标记失效。

### U6.2 可动画 Property 与逐帧失效

| Property                                          | 逐帧失效                     |
| ------------------------------------------------- | ---------------------------- |
| `translate`、`scale`、`rotation`                  | `TRANSFORM\|HIT_TEST\|PAINT` |
| `opacity`、`background`、`color`、`corner_radius` | `PAINT`                      |
| `width`、`height`（仅 `fixed` 值之间）            | `MEASURE\|LAYOUT`            |

对 `width`/`height` 设置 transition 合法但每帧重新布局；`viso check` 对 `VirtualList` Item Template 中的这类 transition 报告性能提示。其他 Property 不可动画。

### U6.3 `Animation` 句柄

命令式动画经 Named Node 的 NodeRef 发起（U7.6），返回 UI Handle：

```viso
export enum Animate { translate(Offset); scale(F32); rotation(Angle); opacity(F32); }

// NodeRef<T>：fn animate(self, target: Animate, spec: Transition) -> Animation;
// Animation：fn cancel(self); fn finish(self); fn is_running(self) -> Bool;

component Shake {
    state shaking: Option<Animation> = Option::None;

    action shake() {
        let spec = Transition { duration: 80ms, easing: Easing::ease_in_out };
        shaking = Option::Some(card.animate(Animate::translate(Offset { x: 8dp, y: 0dp }), spec));
    }

    view {
        node card: Column {
            on animation_end(e) { if e.finished { shaking = Option::None; } }
        }
    }
}
```

- `animate` 只能在 Action、Event Handler 与 `start` Handler 中调用；基线目标只含变换类与 `opacity`，因此命令式动画永不触发布局；
- 结束或取消时目标节点收到 `animation_end(AnimationEnd)`，字段为 `target: Animate`、`finished: Bool`（被 `cancel()` 或新动画打断时为 `false`）；
- 结束后呈现值回到该 Property 当前的 Model 值；需要保持终值时在 `animation_end` 中写入 State，或直接使用 U6.1 Transition；
- 同一节点同一目标上的新动画取消旧动画；
- `Animation` 与 `NodeRef` 生命周期相同（§83）：节点 Dispose 或热重载后失效，对失效句柄调用是 No-op，Debug Runtime 报告警告。

### U6.4 Reduced Motion

`env.reduced_motion == true` 时，运行时按 `Transition.reduced` 处理 U6.1 与 U6.3 的动画：`instant` 跳过插值直接呈现终值，仍派发 `finished: true` 的 `animation_end`；`keep` 照常播放，只用于传达信息的必要动画（如进度指示）。作者无需读取 `env.reduced_motion` 来关闭动画，只有在需要改变动画之外的呈现时才读取。

---

## U7. 输入、手势与焦点

### U7.1 标准事件

以下事件对所有布局节点可用。声明 Handler 即在该节点注册对应命中测试或识别器；未声明的节点不参与该类输入。

| Event                                          | Payload          | 默认 Phase | 冒泡     | 来源                                             |
| ---------------------------------------------- | ---------------- | ---------- | -------- | ------------------------------------------------ |
| `click`                                        | `ClickEvent`     | Bubble     | 是       | 激活：指针轻点、焦点上 Enter/Space、辅助技术 Click |
| `tap`                                          | `TapEvent`       | Bubble     | 是       | 手势竞技场 Tap（仅指针）                         |
| `long_press`                                   | `LongPressEvent` | Bubble     | 是       | 手势竞技场                                       |
| `drag_start` / `drag_move` / `drag_end`        | `DragEvent`      | Bubble     | 是       | 手势竞技场 Pan                                   |
| `pointer_down` / `_move` / `_up` / `_cancel`   | `PointerEvent`   | Bubble     | 是       | 原始指针                                         |
| `hover_enter` / `hover_leave`                  | `HoverEvent`     | Target     | 否       | ADR 0022                                         |
| `scroll`                                       | `ScrollEvent`    | Bubble     | 按轴传递 | 滚轮与触控板（ADR 0007）                         |
| `key_down` / `key_up`                          | `KeyEvent`       | Bubble     | 沿焦点链 | 焦点节点                                         |
| `focus` / `blur`                               | `FocusEvent`     | Target     | 否       | 焦点系统                                         |

Payload 基线字段：

- `PointerEvent`：`position: Point`（节点局部 dp）、`button: PointerButton`（`primary; secondary; middle;`）、`buttons: PointerButtons`、`pointer_kind: PointerKind`（`mouse; touch; pen;`）、`modifiers: Modifiers`（`shift`、`control`、`alt`、`logo: Bool`）；
- `KeyEvent`：`key: Key`、`repeat: Bool`、`modifiers: Modifiers`；文本输入与 IME 组字不经 `key_down`，由 `TextInput` 内部处理（CLAUDE.md §13）；
- `DragEvent`：`position: Point`、`delta: Offset`、`total: Offset`；
- `ClickEvent`：`position: Option<Point>`（键盘与辅助技术激活时为 `Option::None`）、`modifiers: Modifiers`；
- `TapEvent`、`LongPressEvent`、`HoverEvent`：`position: Point`、`pointer_kind: PointerKind`；
- `ScrollEvent`：`delta: Offset`、`modifiers: Modifiers`；
- `FocusEvent`：`focus_visible: Bool`（焦点由键盘移动时为 `true`）；
- `Point { x: Dp; y: Dp; }`；`PointerButtons { primary; secondary; middle: Bool }`；`Key` 为 `char(Char); enter; escape; tab; backspace; delete; space; arrow_up; arrow_down; arrow_left; arrow_right; home; end; page_up; page_down; function(U8); unidentified;`。

以上类型、U6.3 的 `Animate`/`AnimationEnd` 与 U7.5 的 Widget 事件 Payload 由隐式 Prelude 导出（§23）。

规则：

- 分派顺序 Capture → Target → Bubble → Default Action（§52），可用 `event.stop_propagation()` 与 `event.prevent_default()` 中断；
- `tap`、`long_press`、`drag_*` 在手势竞技场竞争，胜者独占该指针序列；`click` 不参与竞争，但同一节点上 `drag` 胜出时不产生 `click`；
- `hover_*` 不是语义事件：触摸设备上不触发，指针 Capture 期间被抑制；任何功能不得只依赖 hover（U8.2）；
- Handler 在 Action Transaction 中运行，可以 `start` Task；Task 随节点子树 Dispose 取消（ADR 0032）。

### U7.2 `focusable` 与焦点顺序

| Property    | 类型   | 默认                           | 失效                         |
| ----------- | ------ | ------------------------------ | ---------------------------- |
| `focusable` | `Bool` | `false`；标准交互 Widget 为 `true` | `SEMANTICS`              |
| `autofocus` | `Bool` | `false`                        | —（仅挂载时读取）            |
| `enabled`   | `Bool` | `true`（交互 Widget）          | `STYLE\|HIT_TEST\|SEMANTICS` |

- 焦点顺序是 Focus Scope 内可聚焦节点的**源码先序顺序**，不随 RTL 翻转，也不受 `stack.layer`、`translate` 影响；
- 不提供正数 `tab_index`，需要调整顺序时调整源码结构；
- Tab / Shift+Tab 在当前 Scope 内前后移动；组合 Widget（`Tabs`、`RadioGroup`、`VirtualList`）内部用方向键漫游，对外只占一个 Tab 停靠点；
- `enabled: false` 的节点不可聚焦、不接收指针与键盘事件，语义上暴露为 disabled，并激活 `disabled` 选择器；
- 焦点移动时旧节点与新节点都标记 `PAINT|SEMANTICS`（ADR 0030）。

### U7.3 Focus Scope

`FocusScope` 是不参与布局的包装节点，有两个 Property：`trap: Bool = false`（为真时 Tab 在 Scope 内循环，焦点不能移出）与 `restore_focus: Bool = true`（Scope Dispose 后焦点回到进入 Scope 前的节点）。两者只在焦点移动时读取，不标记失效。`Modal`、`Sheet`、`Popup` 内置 `trap: true` 的 Scope。

### U7.4 键盘快捷键

```viso
KeyShortcut {
    chord: KeyChord { key: Key::char('s'), primary: true };
    on triggered { save(); }
}
```

- `KeyShortcut` 是零尺寸不可见节点，不参与布局、Paint 与语义；
- `chord: KeyChord` 必填，字段为 `key: Key` 与 `primary`、`shift`、`alt`、`control: Bool = false`；`primary` 在 macOS 映射为 Command，其他平台映射为 Control；
- `scope: ShortcutScope = ShortcutScope::parent`：`parent` 表示焦点位于父节点子树内时生效，`window` 表示整个窗口；
- 冲突时焦点链上最近的快捷键生效；焦点位于 `TextInput` 时，不带修饰键的快捷键不触发；
- 快捷键导出到语义树，作为宿主节点的 `keyboard_shortcut` 描述。

### U7.5 交互 Widget 基线

以下条目补充 U3–U5 的公共 Property；所有交互 Widget 均有 `enabled`（U7.2）且默认 `focusable: true`。

| Widget       | Property（类型 = 默认；失效）                                                                 | Event                  |
| ------------ | --------------------------------------------------------------------------------------------- | ---------------------- |
| `Button`     | `text: String = ""`（Localizable；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）                    | `click`                |
| `Toggle`     | `checked: Bool = false`（two_way；`PAINT\|SEMANTICS`）；`label: String = ""`（Localizable；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`） | `changed(value: Bool)` |
| `CheckBox`   | 同 `Toggle`                                                                                   | `changed(value: Bool)` |
| `Slider`     | `value: F32 = 0.0`（two_way；`PAINT\|SEMANTICS`）；`min: F32 = 0.0`、`max: F32 = 1.0`、`step: Option<F32> = Option::None`（`PAINT\|SEMANTICS`） | `changed(value: F32)` |
| `TextInput`  | `value: String = ""`（two_way；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`placeholder: String = ""`（Localizable；同上）；`secure: Bool = false`（同上）；`invalid: Bool = false`（`STYLE\|SEMANTICS`） | `changed(value: String)`、`submitted` |
| `Tabs`、`RadioGroup` | `selected: U32 = 0`（two_way；`STYLE\|PAINT\|SEMANTICS`）                             | `selected_changed(value: U32)` |

Widget 事件的 Payload 是 Prelude Record：`Toggle`/`CheckBox` 的 `changed` 为 `ToggleChanged`，`Slider` 为 `SliderChanged`，`TextInput` 为 `TextChanged`，`selected_changed` 为 `SelectionChanged`，字段均为 `value`；`submitted` 与 `triggered` 无 Payload。

`Toggle` 使用 `checked` 而不是 `on`：`on` 是 View Item 起始位置的 Contextual Keyword，同时 `checked` 与语义状态同名（ADR 0021）。

### U7.6 NodeRef 动作

Named Node `n` 在同一 Component 的 Action 与 Handler 中以 `n: NodeRef<T>` 可见（§49.1）：

| 方法                                            | 适用 `T`      | 效果                                              |
| ----------------------------------------------- | ------------- | ------------------------------------------------- |
| `n.focus()` / `n.blur()`                        | 可聚焦节点    | 程序化焦点移动，不激活 `focus_visible`            |
| `n.scroll_into_view()`                          | 任意          | 最近祖先视口以最小滚动使 `n` 可见                 |
| `n.scroll_to(offset)` / `n.scroll_by(delta)`    | `Scroll`      | 只标记 `TRANSFORM\|HIT_TEST\|PAINT`               |
| `n.scroll_to_key(key)` / `n.scroll_to_index(i)` | `VirtualList` | 见 U9.3                                           |
| `n.animate(target, spec)`                       | 任意          | 见 U6.3                                           |

在 View、Computed 与 `fn` 中调用这些方法是 §83 纯度错误；对已 Dispose 节点调用是 No-op，Debug Runtime 报告警告。

---

## U8. 语义

### U8.1 `semantics` Property Group

所有成员的失效类别均为 `SEMANTICS`：

| Property                  | 类型             | 默认                            |
| ------------------------- | ---------------- | ------------------------------- |
| `semantics.role`          | `Role`           | Widget 默认角色                 |
| `semantics.label`         | `Option<String>` | `Option::None`（由内容推导）    |
| `semantics.hint`          | `Option<String>` | `Option::None`                  |
| `semantics.value`         | `Option<String>` | `Option::None`（由 Widget 投影）|
| `semantics.live`          | `LiveRegion`     | `LiveRegion::off`               |
| `semantics.hidden`        | `Bool`           | `false`                         |
| `semantics.heading_level` | `Option<U8>`     | `Option::None`                  |

- `Role { group; button; check_box; slider; radio; label; text_field; tab; tab_list; navigation; dialog; status; region; tree; tree_item; heading; image; list; list_item; }`，其中 `heading`、`image`、`list`、`list_item` 为 **[Runtime 待实现]**；
- `LiveRegion { off; polite; assertive; }`；
- 默认角色：`Button` → `button`；`Toggle`、`CheckBox` → `check_box`；`Slider` → `slider`；`TextInput` → `text_field`；`Tabs` → `tab_list`，每页为 `tab`；`RadioGroup` 每项为 `radio`；`Text` → `label`；`Modal`、`Sheet` → `dialog`；`Toast` → `status`（隐含 `polite`）；`NavigationStack` → `navigation`；`VirtualList` → `list`；容器 → `group`。

### U8.2 规则

- **只影响语义**：`semantics.*` 变化只标记 `SEMANTICS`，不得引起布局或 Paint；
- **状态投影**：`checked`、`value`、`range`、`expanded`、`selected` 由 Widget Property 自动投影（ADR 0021），作者不应在 `semantics.value` 中重复；
- **交互节点必须可访问**（CLAUDE.md §15）：在非标准交互 Widget 的节点（如 `Row`）或用户 Component 根节点上声明 `click`、`tap`、`long_press`、`drag_*` 或 `key_down` Handler 时，该节点必须：(1) 设置非 `group` 的 `semantics.role`；(2) 有可访问名称（`semantics.label` 或可推导的文本内容）；(3) 有等价键盘路径，即 `focusable: true` 且处理 `click`，只处理 `tap`/`pointer_*` 不算。违反 (1) 或 (2) 是 `E3704`，违反 (3) 是 `E3708`，两者默认为警告，`viso check --a11y strict` 提升为错误；
- **Live Region**：设置 `semantics.live` 的子树文本变化时由 AccessKit 通告，`polite` 排队、`assertive` 打断；表单错误文本应使用 `polite`；
- **隐藏**：`semantics.hidden: true` 把子树从语义树移除但保留绘制，用于纯装饰；`visible: false` 同时移除绘制与语义；
- **辅助技术动作**：Focus、Click、Increment、Decrement 走普通输入路径（ADR 0030），触发同一套 Handler。

---

## U9. 列表：`for` 与 `VirtualList`

### U9.1 语义划分

- 普通 `for x in xs key k { }`（§55）在 Reconcile 时**挂载全部**元素，适合有界小集合；
- `VirtualList` 只挂载视口内及 Overscan 区域的元素，适合任意长度集合；
- 两者使用同一 `for` 写法：`VirtualList` 的 Node Body 必须**恰好**含一个 `for` 作为 Item Template，编译器将其降级为虚拟化模板而不是立即挂载；缺少 `key`、出现多个 `for` 或存在其他子节点是 `E3707`。

### U9.2 Schema 基线

| Property                   | 类型           | 默认            | 失效              | percent_basis          |
| -------------------------- | -------------- | --------------- | ----------------- | ---------------------- |
| `axis`                     | `Axis`         | `Axis::column`  | `MEASURE\|LAYOUT` | —                      |
| `estimated_extent`         | `MixedLength`  | `30dp`          | `LAYOUT`          | 不声明（Percent 为 `E3104`） |
| `overscan`                 | `U32`          | `4`             | `LAYOUT`          | —                      |
| `width` / `height`         | `Sizing`       | `fill` / `fill` | `MEASURE\|LAYOUT` | 见 U3.3                |
| `on visible_range_changed` | `VisibleRange` | —               | —                 | —                      |

`VisibleRange` 字段为 `first: U64`、`last: U64`、`count: U64`。

### U9.3 运行时语义（ADR 0008）

- **结构**：Scroll 视口内一张主轴范围固定的画布，行高由 Fenwick 树维护；未测量行使用 `estimated_extent`（可写 `em`，按列表节点的 Resolved Font Size 解析），测量后更新；
- **身份**：`key` 值经 `StableKey` 映射为 `ItemKey`，作为行身份；重排时存活行保留宿主节点与局部状态并重新锚定；滚动锚点保持在锚定行上，插入与删除不引起视觉跳动；
- **回收**：离开 Overscan 窗口的行宿主进入回收池按 Template 复用；复用即重新绑定新 Item，标记该行子树 `MEASURE|LAYOUT|PAINT`，**不**标记 `STRUCTURE`；行被回收时其局部 `state` 丢弃、行内 `start` 的 Task 取消，需要跨滚动保留的数据应放在列表外的 Model 中；
- **滚动**：只标记 `TRANSFORM|HIT_TEST|PAINT`；进入视口的新行按上述规则挂载或复用；
- **命令式滚动**：`n.scroll_to_key(key)` 与 `n.scroll_to_index(i)` 可滚动到尚未挂载的行；未知 `key` 是 No-op；
- **语义**：列表 Role 为 `list`，每行为 `list_item`，语义树报告集合总数与行位置。

---

## U10. 国际化与 RTL

### U10.1 逻辑方向与 `env.layout_direction`

§96.2 的 `env` 包含本部分使用的两个字段：`layout_direction: LayoutDirection`（`LayoutDirection { ltr; rtl; }`）与 `locale: Locale`（BCP-47）。读取 `env.layout_direction` 的失效为 `MEASURE|LAYOUT`，读取 `env.locale` 为 `MEASURE|LAYOUT|PAINT|SEMANTICS`。

- 所有 `start`/`end`、`Row` 主轴、Grid 列序与 `absolute.start`/`absolute.end` 都按 `env.layout_direction` 自动解析，作者不需要读取它来翻转布局；它只用于非布局决策；
- 标准 Widget 的方向性图标（返回、展开箭头等）自动镜像；用户 `Icon`/`Image` 可设 `mirror_in_rtl: Bool = false`（失效 `PAINT`）；
- 变换（U4）、Canvas 坐标与指针坐标都是物理坐标，不翻转；
- Adaptive Scope（§96）可以局部覆盖 `layout_direction` 与 `locale`，例如内嵌的外语段落。

### U10.2 Bidi

字符串以逻辑顺序存储与传递。段落基础方向默认由首个强方向字符决定，无强方向字符时回退到 `env.layout_direction`。双向重排、镜像字符与光标移动全部由 Text Runtime 负责。

### U10.3 `tr` 与 Localizable Property

`tr` 是库 API（`viso::i18n`），不是语法：

```viso
// fn tr(key: MessageKey, args: List<TrArg> = []) -> String;
// fn tr_arg<T: TrValue>(name: String, value: T) -> TrArg;

Text { text: tr("inbox.unread", [tr_arg("count", unread)]); }
```

- 字符串字面量在期望 `MessageKey` 处于编译期转换为 `MessageKey` 并与项目消息目录核对；键不存在或参数名/类型与目录不符是 `E3706`；
- 复数、性别与数字格式由目录的消息格式决定；`tr` 对 `env.locale` 建立响应式依赖；
- `tr` 可在 View、Computed、Style、Action 中使用；它读取 Context，因此不能在纯 `fn` 中使用。需要在 `fn` 中选择文案时让 `fn` 返回 `MessageKey`，由 View 调用 `tr`（U13.3）；
- Schema 把用户可见文本 Property 标记为 Localizable：`Text.text`、`Button.text`、`Toggle.label`、`TextInput.placeholder`、`semantics.label`、`semantics.hint` 以及各 Widget 的标题文本；
- 在 Localizable Property 上用字符串拼接或含字面文字的 `format(...)` 构造文本是 `E3705`（警告），应改用带参数的 `tr`，因为语序因语言而异；只含单个占位符的 `format("{}", n)` 不受限；
- 纯字面量默认不报错；`viso check --i18n strict` 对 Localizable Property 上的字面量也报告 `E3705`。

---

## U11. 导航

路由是 **Rust 中定义的类型化路由**（CLAUDE.md §32）。DSL 不声明路由表，只使用经 Native Schema 导入的 `Route` Enum 与宿主提供的句柄 `Navigator<R>`：

`Navigator<R>` 的方法：响应式读取 `current() -> R`、`can_pop() -> Bool`；命令 `push(route)`、`replace(route)`、`pop()`、`reset(route)`。

```viso
import app::routes::Route;

export component Shell {
    input nav: Navigator<Route>;
    input has_unsaved_changes: Bool = false;

    view {
        Column {
            Button { text: tr("nav.settings"); on click { nav.push(Route::settings); } }
            RouterView {
                navigator: nav;
                height: Sizing::fill {};
                on back_requested(e) { if has_unsaved_changes { e.prevent_default(); } }
            }
        }
    }
}
```

- `push`、`replace`、`pop`、`reset` 只能在 Action 与 Handler 中调用；一个 Transaction 内的多次导航在提交时合并为一次；
- `current()`、`can_pop()` 可在 View 与 Computed 中读取，产生 `STRUCTURE` 依赖；
- `RouterView` 是导航出口：按 `navigator.current()` 与 Rust 注册的 Route→Page 映射挂载页面，页面切换是 `STRUCTURE`；页面节点以路由值为 Key，返回栈中已有页面时保留其状态；
- 系统返回（Android Back、Escape、边缘手势）先派发 `back_requested` 给最内层 `RouterView`，未被 `prevent_default()` 时执行 `pop()`；`can_pop()` 为假时事件交给宿主；
- 路由参数经 Route Variant 字段传递，URL 与深链接解析在 Rust 中完成；
- 页面切换后焦点移到新页面首个 `heading`，没有时移到页面根节点；转场动画推迟。

---

## U12. 状态选择器与标准 Theme

### U12.1 标准状态选择器

Widget Schema 从下表中声明它支持的选择器；Style 使用 Widget 不支持的选择器是 §59 既有错误。

| 选择器          | 为真条件                                          |
| --------------- | ------------------------------------------------- |
| `hover`         | 指针悬停于节点（ADR 0022；触摸设备恒为假）        |
| `pressed`       | 主按钮按下、仍在节点内且未被手势竞技场判负        |
| `focused`       | 节点持有焦点                                      |
| `focus_visible` | `focused` 且焦点由键盘或辅助技术移入              |
| `disabled`      | `enabled == false`                                |
| `checked`       | `Toggle`、`CheckBox`、`Radio` 的选中值            |
| `selected`      | `Tab`、列表行等的选中态                           |
| `expanded`      | 可展开 Widget 处于展开态                          |
| `invalid`       | 输入 Widget 的 `invalid: Bool` 为真               |
| `dragging`      | 节点是当前拖动手势的胜者                          |

- 选择器变化时源节点标记 `STYLE`，加上所有引用该选择器的 `when` 块中 Property 失效类别的并集（只切换颜色的 hover 是 `STYLE|PAINT`）；
- 同一 Style 内 `when` 块按源码顺序应用，后者覆盖前者；标准 Style 按"静止 → `hover` → `pressed` → `focus_visible` → `disabled`"书写，得到 `pressed > hover > 静止` 的优先级；
- 节点显式 Property 优先于所有 Style（§59），因此依赖选择器的外观应写在 Style 中。

### U12.2 标准 Theme Schema

`theme` Context 的类型是 `viso::theme::Theme`；`theme X { ... }` 未给出的字段取 Record 默认值，无默认值字段必须提供。

```viso
export record Theme {
    colors: ColorPalette;
    typography: TypographyScale = TypographyScale {};
    spacing: SpacingScale = SpacingScale {};
    radius: RadiusScale = RadiusScale {};
    elevation: ElevationScale;
    motion: MotionScale = MotionScale {};
}
```

| Record            | 字段（类型 = 默认）                                                                 |
| ----------------- | ----------------------------------------------------------------------------------- |
| `ColorPalette`    | 均为 `Color`、无默认：`background` `foreground` `surface` `on_surface` `primary` `on_primary` `primary_hover` `accent` `muted` `outline` `error` `on_error` `focus_ring` `scrim` |
| `TypographyScale` | `base_size: Sp = 14sp`、`family: FontFamily = FontFamily::system_ui`、`caption_size: Em = 0.85em`、`title_size: Em = 1.25em`、`headline_size: Em = 1.6em` |
| `SpacingScale`    | 均为 `Dp`：`xsmall = 2dp`、`small = 4dp`、`medium = 8dp`、`large = 16dp`、`xlarge = 24dp` |
| `RadiusScale`     | 均为 `Dp`：`small = 4dp`、`medium = 8dp`、`large = 12dp`                            |
| `ElevationScale`  | 均为 `Shadow`、无默认：`low` `medium` `high`；`Shadow { offset_y: Dp; blur: Dp; color: Color; }` |
| `MotionScale`     | `short: Duration = 100ms`、`medium = 200ms`、`long = 350ms`、`standard: Easing = Easing::ease_out`、`emphasized: Easing = Easing::ease_in_out` |

- `typography.*_size` 是 `Em`，用作 `font_size` 时相对父节点 Resolved Font Size（§19.4）；需要与嵌套深度无关的层级时，写成 `theme.typography.base_size` 的倍数；
- 读取 `theme.*` 的 Binding 在 Theme 整体替换时按各自 Property 的失效类别失效，替换本身不引入额外 Dirty Class；
- 应用自定义 Theme 字段通过扩展 Record 提供，扩展方式沿用 §60 的 Theme Base 规则。

---

## U13. 完整示例

以下示例只使用本部分与 §40–§60 定义的构造。

### U13.1 设置页：分组与双向绑定开关

```viso
import viso::widgets::{Scroll, Column, Row, Text, Toggle, Slider};
import viso::i18n::tr;

export record AppSettings { notifications: Bool = true; dark_mode: Bool = false; volume: F32 = 0.5; }

export component SettingsSection {
    input title: String;

    @default
    slot content: SlotList<Node> = empty;

    view {
        Column {
            width: Sizing::fill {};
            gap: theme.spacing.small;
            padding: EdgeInsets::axes(inline: theme.spacing.large, block: theme.spacing.medium);
            Text {
                text: title;
                font_size: theme.typography.title_size;
                font_weight: FontWeight::semibold;
                semantics.role: Role::heading;
                semantics.heading_level: 2;
            }
            SlotOutlet { slot: content; }
        }
    }
}

export component SettingsPage {
    state settings: AppSettings = AppSettings {};

    view {
        Scroll {
            Column {
                width: Sizing::fill {};
                max_width: 640dp;
                SettingsSection {
                    title: tr("settings.general");
                    Toggle { label: tr("settings.notifications"); bind checked <=> settings.notifications; }
                    Toggle { label: tr("settings.dark_mode"); bind checked <=> settings.dark_mode; }
                }
                SettingsSection {
                    title: tr("settings.sound");
                    Row {
                        width: Sizing::fill {};
                        gap: theme.spacing.medium;
                        align: Align::center;
                        Text { text: tr("settings.volume"); }
                        Slider {
                            width: Sizing::fill {};
                            enabled: settings.notifications;
                            semantics.label: tr("settings.volume");
                            bind value <=> settings.volume;
                        }
                    }
                }
            }
        }
    }
}
```

`SettingsSection` 的裸子节点进入其 `@default`。`Toggle.checked` 与 `Slider.value` 在 Schema 中为 `two_way`，配对事件为 `changed`。`settings.notifications` 只经 `Slider.enabled` 影响 `STYLE|HIT_TEST|SEMANTICS`，不引起 `STRUCTURE`。

### U13.2 聊天消息列表：虚拟化、Keyed、自适应

```viso
import viso::widgets::{VirtualList, Column, Row, Text};
import viso::i18n::tr;

export record ChatMessage { id: MessageId; author: String; body: String; mine: Bool; }

component MessageBubble {
    input message: ChatMessage;
    input compact: Bool = false;

    computed bubble_max: Percent = if compact { 85% } else { 60% };

    view {
        Row {
            width: Sizing::fill {};
            justify: if message.mine { Justify::end } else { Justify::start };
            padding: EdgeInsets::axes(inline: theme.spacing.medium, block: theme.spacing.xsmall);
            Column {
                max_width: bubble_max;
                padding: 0.6em;
                corner_radius: theme.radius.large;
                background: if message.mine { theme.colors.primary } else { theme.colors.surface };
                if !message.mine {
                    Text { text: message.author; font_size: theme.typography.caption_size; color: theme.colors.muted; }
                }
                Text {
                    text: message.body;
                    soft_wrap: true;
                    selectable: true;
                    color: if message.mine { theme.colors.on_primary } else { theme.colors.on_surface };
                }
            }
        }
    }
}

export component ChatMessageList {
    input messages: List<ChatMessage>;

    action jump_to(id: MessageId) { list.scroll_to_key(id); }

    view {
        node list: VirtualList {
            height: Sizing::fill {};
            estimated_extent: 3.5em;
            overscan: 6;
            semantics.label: tr("chat.messages");
            for message in messages key message.id {
                MessageBubble {
                    message: message;
                    compact: env.size_class == SizeClass::Compact;
                }
            }
        }
    }
}
```

`for` 位于 `VirtualList` 内，是 Item Template，只挂载可见行与 Overscan 行。`message.id` 是行身份，新消息插入时既有行保留状态与锚点。`bubble_max` 的 Percent 以行内容盒宽度为基准；`env.size_class` 变化只标记 `MEASURE|LAYOUT`。`estimated_extent: 3.5em` 随 `env.text_scale` 与 `theme.typography.base_size` 缩放。

### U13.3 表单：计算校验、禁用提交、错误文本与 Task 提交

```viso
import viso::widgets::{Column, Text, TextInput, Button, Spinner};
import viso::i18n::{tr, MessageKey};

export record SignupForm { email: String = ""; password: String = ""; }

@derive(Eq)
export enum FieldIssue { malformed; too_short; }

@derive(Eq)
export enum SubmitPhase { idle; submitting; failed(String); done; }

fn check_email(value: String) -> Option<FieldIssue> {
    if !value.is_empty() && !value.contains("@") { return Option::Some(FieldIssue::malformed); }
    return Option::None;
}

fn check_password(value: String) -> Option<FieldIssue> {
    if !value.is_empty() && value.length() < 8 { return Option::Some(FieldIssue::too_short); }
    return Option::None;
}

fn issue_key(issue: FieldIssue) -> MessageKey {
    return match issue {
        FieldIssue::malformed => "form.email.malformed",
        FieldIssue::too_short => "form.password.too_short",
    };
}

task create_account(form: SignupForm) -> Result<AccountId, SignupError>
    requires { network::http } {
    return await Accounts::create(form);
}

component FieldError {
    input issue: Option<FieldIssue>;

    view {
        match issue {
            Option::Some(found) => {
                Text {
                    text: tr(issue_key(found));
                    color: theme.colors.error;
                    semantics.live: LiveRegion::polite;
                }
            },
            Option::None => {},
        }
    }
}

export component SignupPage {
    event signed_up(id: AccountId);

    state form: SignupForm = SignupForm {};
    state phase: SubmitPhase = SubmitPhase::idle;

    computed email_issue: Option<FieldIssue> = check_email(form.email);
    computed password_issue: Option<FieldIssue> = check_password(form.password);
    computed complete: Bool = !form.email.is_empty() && !form.password.is_empty();
    computed valid: Bool = complete && email_issue == Option::None && password_issue == Option::None;
    computed submitting: Bool = phase == SubmitPhase::submitting;

    action submit() {
        if !valid || submitting { return; }
        phase = SubmitPhase::submitting;

        start create_account(form) as submit_job {
            policy = [TaskPolicy::keep_latest];
            success(id) { phase = SubmitPhase::done; emit signed_up(id); }
            error(reason) { phase = SubmitPhase::failed(reason.message()); }
            cancelled { phase = SubmitPhase::idle; }
        };
    }

    view {
        Column {
            gap: theme.spacing.medium;
            padding: EdgeInsets::all(theme.spacing.large);
            node email_field: TextInput {
                placeholder: tr("form.email");
                invalid: email_issue != Option::None;
                enabled: !submitting;
                autofocus: true;
                bind value <=> form.email;
                on submitted { password_field.focus(); }
            }
            FieldError { issue: email_issue; }
            node password_field: TextInput {
                placeholder: tr("form.password");
                secure: true;
                invalid: password_issue != Option::None;
                enabled: !submitting;
                bind value <=> form.password;
                on submitted { submit(); }
            }
            FieldError { issue: password_issue; }
            Button {
                text: tr("form.create_account");
                enabled: valid && !submitting;
                on click { submit(); }
            }
            match phase {
                SubmitPhase::submitting => {
                    Spinner { semantics.label: tr("form.submitting"); }
                },
                SubmitPhase::failed(message) => {
                    Text {
                        text: message;
                        color: theme.colors.error;
                        semantics.live: LiveRegion::assertive;
                    }
                },
                _ => {},
            }
        }
    }
}
```

- 校验状态全部是 `computed`；`valid` 为假或正在提交时 Button `enabled` 为假，从而激活 `disabled` 选择器、在语义上暴露为 disabled 且不可聚焦；
- `fn` 只返回 `MessageKey`，`tr` 在 View 中调用，切换 Locale 时错误文本随之更新；`FieldError.issue` 被推导为 `STRUCTURE`（U2.4，因为它是 `match` 条件）；
- `submit` 同步写入 `phase` 后启动 Task 且不等待；三个 Handler 各在新 Transaction 中回写 `phase`；`keep_latest` 保证重复提交只保留最后一次；页面 Dispose 时 Task 取消（ADR 0032）；
- `password_field.focus()` 是 U7.6 的 NodeRef 动作；
- 字段错误位于 `polite` Live Region，提交失败文本位于 `assertive` Live Region。

---

# 第十二部分：Shader、Native ABI 与多执行域边界

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

# 第十三部分：游戏表达能力与 Game Profile

## 104. 结论

Viso DSL 1.0 的语言表达能力足以承载从快速原型到结构化游戏 Runtime 的核心游戏逻辑，包括：

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
- 可记录、可重放的确定性测试；
- 编译期检查的 Simulation / Derived / Local 状态分层（§106.4）；
- 编译器生成的 Snapshot/Restore：回放、回滚、存档、时间回溯调试（§106.7）；
- 可快照的 Tick 计时器与冷却（§106.6）；
- 带迁移的持久化状态（§106.8）；
- Typed 输入映射与手柄/触屏等价检查（§106.3）；
- 跨平台确定性浮点 Profile（§106.5）。

不同之处是：长期游戏状态放在明确的 `system state` 中，Tick 通过 Trait/Scheduler 调用，而不是依赖某个动态全局对象和长期闭包的隐式捕获。确定性也不是约定，而是类型检查：Simulation 代码读不到 Presentation 状态、Wall Clock 和未注入的随机源。

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

### 105.1 Quick Game Profile

小型游戏、教学 Demo 和 AI/Vibe Coding 不应该被迫先设计完整的多 System graph。标准库提供 `viso::game::quick`，但它仍然 **不是 Parser 特例**。

Quick Game 的规范目标：

```text
更少 imports
单一 typed frame context
固定步长
默认确定性
同一 InputSnapshot
同一 GameWorld / physics / render backend
同一 replay / hot reload / profiler
可无语义损失地拆成多个完整 System
```

标准 Native Schema 可以定义：

```viso
export trait QuickGame {
    action start(cx: QuickStart);
    action fixed(frame: QuickFrame);
}

// QuickStart / QuickFrame 是 viso::game::quick Native Schema 提供的 typed context。
// QuickStart: spawn(desc) -> EntityId，以及 startup-only 资源初始化能力。
// QuickFrame: world, input, dt, tick，以及受控 game action surface。
```

最小游戏：

```viso
import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{EntityId, SpawnDesc, InputAction, InputAxis};

export system TinyGame implements QuickGame {
    state player: Option<EntityId> = Option::None;
    state score: I64 = 0;

    action start(cx: QuickStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player()));
    }

    action fixed(frame: QuickFrame) {
        match player {
            Option::Some(id) => {
                let move = frame.input.axis(InputAxis::move_x);
                frame.world.walk(id, move * 6.0f32, 0.0f32);

                if frame.input.pressed(InputAction::jump)
                    && frame.world.on_floor(id) {
                    frame.world.jump(id, 10.0f32);
                }
            },
            Option::None => {},
        }
    }
}
```

Lowering 要求：

```text
QuickGame.start -> scheduler startup hook
QuickGame.fixed -> FixedUpdate system entry
QuickFrame      -> typed facade over FixedFrame + GameWorld
QuickStart.spawn -> startup transaction committed before first fixed tick
```

Quick Game **不得** 通过 UI frame callback、任意 wall clock、全局 mutable singleton 或长期捕获可变闭包实现。它只是完整 Game Profile 的 low-ceremony surface。

生命周期：

- `start` 在 Quick Game System 首次创建或 World Rebuild 时运行一次；
- Logic-only Hot Reload 不重新运行 `start`；
- `fixed` 每个固定 Tick 运行；
- `start` 失败则回滚启动事务，不进入第一个 Tick；
- `QuickStart.spawn` 的初始化命令必须在第一个 Tick 前提交，所以返回的 `EntityId` 在首次 `fixed` 时已经有效。

当游戏需要独立 Physics/AI/Combat/Audio/Networking 等生命周期时，SHOULD 拆成多个标准 `system ... implements FixedUpdate/FrameUpdate/...`。

### 105.2 Quick Game Kit

`viso::game::kit` 是标准库提供的高层 typed API：地形、基础模型、相机 Rig（第三人称、追随、俯视）、Prefab（角色、载具）、行为（巡逻、追逐、游荡）、粒子与合成音效。它同样只是 Native Schema，不是 Parser 特例。

规则：

- Kit 调用 Lower 为普通 GameWorld 命令（§108），与手写 System 语义一致；
- 每个 Kit 方法在 Schema 中声明所属层（§106.4）：修改 World 的是 `native action`，粒子、音效、相机抖动和 Debug Draw 是 Presentation Command；
- 模型、Tag、音效与动作都是 Typed Enum 或 Resource Key，不接受任意字符串；
- 调用未知方法或 Variant 报 `E2001`，诊断必须在 `related` 与 `fixes`（§138）中给出最近候选：按编辑距离排序，并按 Receiver 类型与期望类型过滤，使 AI 一步修正。

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

### 106.1 时钟、暂停与超限

```text
tick:       U64       单调递增；只有 World Rebuild 和 Restore 会改变它
fixed_dt:   Duration  Profile 编译期常量，默认 1/60 s
time_scale: F32       只缩放 Wall Time 到 Tick 累加器的速率，不改变 fixed_dt
paused:     Bool      暂停时不累加；FrameUpdate 照常运行
```

`frame.time()` 定义为 `tick × fixed_dt`，不是 Wall Clock，Logic-only Reload 不重置它。

超限策略由 Profile 选择：

```viso
export enum TickOverrun {
    DropTime,
    SlowMotion,
}
```

- `DropTime`（默认）：累加器超过 `max_catch_up_steps` 后丢弃剩余时间；
- `SlowMotion`：保留累加时间，不丢 Tick，游戏整体变慢；
- 两种策略都计入 `game.overrun_ticks` 与 `game.dropped_time` 计数器；
- 调试器可 `step(n)` 单步推进 n 个 Tick，单步与正常运行走同一 Scheduler 路径。

### 106.2 输入边沿语义

- 每个 Fixed Tick 看到一份冻结的 `InputSnapshot`；
- 一个渲染帧可能运行 0 个或多个 Tick。`pressed`/`released` 边沿归属到它发生之后的第一个 Tick，并且只被看到一次：0 个 Tick 的帧不丢边沿，多个 Tick 的帧不重复边沿；
- `held` 与 Axis 在同一渲染帧的多个 Tick 中取相同值；
- 同一 Tick 内按下又释放时，`pressed` 与 `released` 都为真；
- 文本输入、IME 和 UI 焦点中的按键不进入 Game 输入，除非 `GameViewport` 持有输入焦点。

### 106.3 Typed 输入映射

未声明映射时使用 `viso::game` 的默认动作集 `InputAction` / `InputAxis`。游戏用普通 Enum 声明自己的动作，用常量声明映射：

```viso
import viso::game::{InputMap, Key, KeySet, PadButton, PadStick};

@derive(Eq, Hash, InputAction)
export enum Act {
    Jump,
    Fire,
}

export const CONTROLS: InputMap<Act> = InputMap::new()
    .key(Key::Space, Act::Jump)
    .pad(PadButton::South, Act::Jump)
    .key(Key::J, Act::Fire)
    .pad(PadButton::West, Act::Fire)
    .move_axes(KeySet::wasd(), PadStick::Left);
```

使用：`frame.input.pressed(Act::Jump)`、`frame.input.move_axes()`；相机相对移动写 `frame.input.move_axes().relative_to(camera_yaw)`。

规则：

- `InputMap` 构造方法都是 `@const`，映射在编译期求值并进入 Schema；
- 死区与对角线归一化（长度不超过 1）由 `InputMap` 统一处理；
- 目标平台包含手柄或触屏时，缺少对应路径的动作报 `E9107`（警告）；
- `InputSnapshot` 只保存动作与轴的值，不保存原始按键，所以 Input Tape 与键位重映射无关。

### 106.4 状态分层

Game Profile 把状态分为三层：

| 层         | 声明                    | 可写入方                                           | 进入 Snapshot    | 联机复制     |
| ---------- | ----------------------- | -------------------------------------------------- | ---------------- | ------------ |
| Simulation | System `state`（默认）  | `start`、FixedUpdate、CollisionListener            | 是               | 是           |
| Derived    | `computed`              | 只读 Simulation 状态，不可写                        | 否，Restore 后重算 | 否，各端重算 |
| Local      | `@local state`          | FrameUpdate、UI、Presentation                       | 否               | 否           |

Simulation 域是 `start`、FixedUpdate、CollisionListener 以及从它们可达的 `fn`/`action`。编译期规则：

- Simulation 域读写 `@local` 状态，或使用 Presentation 方法的返回值，报 `E9103`；
- Simulation 域使用非确定性来源报 `E9104`：Wall Clock、`Task`/`await`、未预加载的 Resource 结果、未注入的 Random、未声明有序的 Hash 容器迭代、宿主超越函数（§106.5）、UI State 与 Adaptive Environment；
- Simulation 状态类型必须实现 `Snapshot`。值类型自动派生；闭包、`Task` 与未声明 Snapshot 的 Handle 不能实现，否则报 `E9105`；
- Presentation 可以只读 Simulation 状态，读到的是本帧提交的 World Revision。

Simulation 可以触发粒子、音效、相机抖动和 Debug Draw，但只能作为 Presentation Command：

- 命令返回 `()`，不能影响 Simulation；
- 命令按 `(tick, source_system, sequence)` 标识；回放、回滚重算与 Restore 时，已交付 Tick 的命令不重复交付；
- Debug Draw 命令在 Release 中被移除。

因此“玩法不依赖本地表现状态”是编译期保证。回放、回滚、存档和热重载都建立在这一分层上。

### 106.5 确定性浮点

`viso.toml` 的 `[game] determinism` 选择：

```text
same_binary      默认；同一目标、同一二进制可逐 Tick 重放
cross_platform   所有 Tier-1 目标上 Snapshot Hash 逐字节一致；联机回滚要求此档
```

`cross_platform` 要求：

- 禁止 FMA 收缩、fast-math、浮点重结合，禁止平台间不一致的 denormal 处理；
- `sin`/`cos`/`atan2`/`exp`/`log`/`pow` 使用 `viso::math` 的确定性实现，不调用宿主 libm；
- `sqrt` 与四则运算按 IEEE 754 最近偶数舍入；
- 归约与迭代顺序固定；并行浮点归约按固定分块顺序合并；
- Physics 等 Native World 在 Schema 中声明自己满足哪一档，不满足时报 `E9104`。

### 106.6 Tick 计时器与冷却

```viso
import viso::game::{Cooldown, TickTimer};

export system Gun implements FixedUpdate {
    state cooldown: Cooldown = Cooldown::new(250ms);
    state wave: TickTimer = TickTimer::every(10s);

    action fixed_update(frame: FixedFrame) {
        if frame.input.held(Act::Fire) && cooldown.ready(frame.tick) {
            cooldown = cooldown.fire(frame.tick);
        }

        if wave.due(frame.tick) {
            wave = wave.rearm(frame.tick);
        }
    }
}
```

- `fixed_dt` 是编译期常量，`Duration` 在编译期换算为整数 Tick（向上取整）；运行时只比较 Tick，不累加浮点时间；
- Timer 是普通值类型：进入 Snapshot，可回放、可迁移、可在调试器中查看；
- 不提供注册闭包的 `every`/`after` API；在 Simulation 状态中存闭包报 `E9105`；
- 只修改 Timer 初始值的 Logic-only Reload 保留当前剩余 Tick（§94.1）。

### 106.7 Snapshot 与 Restore

编译器为每个 Game Session 生成：

```text
GameSnapshot {
    build_hash
    tick
    rng_state
    world      // Native World 通过 Schema 声明的 snapshot/restore
    systems    // 每个 System 的 Simulation State，按 Stable ID 排序
}
```

- Snapshot 只包含 Simulation 层；Derived 在 Restore 后重算，Local 保留当前值；
- 编码使用 Ende 二进制，按 Stable ID 与字段 Schema 版本化，可跨 Logic-only Reload 读取；
- `restore(snapshot(s))` 后继续运行，与不中断运行逐 Tick 一致；
- 用途：Input Tape 回放、联机回滚、存档、时间回溯调试、热重载前后对比；
- Native World 未声明 snapshot 能力时，这些功能在诊断中明确降级，不静默失效。

### 106.8 持久化状态

```viso
export system Progress implements FixedUpdate {
    @persist("best_score")
    state best: I64 = 0;

    action fixed_update(frame: FixedFrame) {}
}
```

- `@persist(key)` 的键在 App 内唯一，类型必须实现 `Snapshot`，并需要 `storage.persist` Capability（§95）；违反时报 `E9106`；
- 在 `start` 之前加载；加载失败时使用 Initializer，并产生诊断事件；
- 写入在 Tick Boundary 合并，按 Profile 策略节流，并在 App Suspend 时落盘；Tick 内不做同步 IO；
- 类型变化走 §94.1 迁移规则与 `@migrate`。

### 106.9 渲染插值

- `RenderFrame.alpha: F32` 取值 `[0, 1)`，等于累加器余量除以 `fixed_dt`；
- World 为每个 Entity 保留上一 Tick 与当前 Tick 的 Transform，Render Extraction 默认按 `alpha` 插值；`teleport` 标记本 Tick 不插值；
- FrameUpdate 属于 Presentation：只读 Simulation，只写 Local。

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
    InputAxis,
    InputAction,
    GameTag,
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
- AI 可以通过 `viso schema viso::game::GameWorld` 查询方法。

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

### 108.1 迭代与查询

- `world.query(tag)` 与 `world.entities()` 按 EntityId 分配顺序迭代；
- 同一 Session 内 EntityId 不复用，槽位复用时 Generation 递增；
- Tag 是 `@derive(GameTag)` 的用户 Enum 或 Schema Enum，不是字符串；
- 迭代中的 Remove/Spawn 进入 Command Buffer，迭代结束后提交。

### 108.2 Release 执行形态

- Dev 使用 Bytecode，以支持 Logic-only Reload；
- Release 可以把 System IR 降低为 Rust，与 App 一起编译。语义以 Bytecode 为准，两者对同一 Input Tape 做差分测试，Snapshot Hash 必须一致；
- Native Lowering 的性能收益是假设，由 Release Benchmark 证实后才能成为默认。

### 108.3 AudioProcess 实时域

- `AudioProcess` System 在音频线程运行，属于 Presentation 层；
- 禁止堆分配、`Task`/`await`、锁、Resource 加载、未标 `realtime` 的 Native 调用以及无静态上限的循环，违反时报 `E9108`；
- 与其他 System 只通过有界无锁队列传递 typed message。

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

编译器按 Stable ID Diff 选择重载层，并把结果写进 Reload 诊断：

| 改动                                             | 重载层                                                   |
| ------------------------------------------------ | -------------------------------------------------------- |
| 只改 Simulation 域的 Action/`fn` 体              | Logic-only                                               |
| 只改 `@local` 状态、FrameUpdate、HUD             | Presentation-only：Frame Boundary 切换，不触碰 Simulation |
| Simulation State 类型或 Initializer 变化         | Logic-only + State Migration（§94.1）                    |
| `start` 或 World 构建变化                        | World Rebuild，按 Stable Entity Key 迁移                 |
| `InputMap` 常量                                  | Logic-only；已录制的 Tape 不受影响                        |
| Shader                                           | Shader Reload                                            |

开发者可以强制 World Rebuild。除 World Rebuild 外，`tick`、RNG 状态与 Timer 剩余 Tick 都保留。

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

### 110.4 回溯重放（Dev）

- Dev Runtime 维护 Snapshot 环形缓冲（默认最近 10 s），并持续记录 Input Tape；
- Logic-only Reload 后可以选择“从 T−k 重放”：Restore 旧 Snapshot，用新代码重跑记录的输入，再从当前 Tick 继续；
- 开发者由此直接看到改动对刚才那段玩法的影响，不必手动重现。

### 110.5 游戏测试与 AI 工具合同

命令形状由 `Viso_CLI.md` §22.3 定义：`viso test game`、`viso game record`、`viso game peek`。

- Input Tape 是版本化 Ende 文件，包含 Seed、Build Hash、determinism 档位、`fixed_dt`，以及按 Tick 的 InputSnapshot（动作与轴，不含原始按键）；另有可手写的文本形式，例如 `30: press Jump`、`31..90: axis move = (1, 0)`；
- `@probe` 状态每个 Tick 输出到 JSON Trace，Scenario 可以对 Probe 断言；
- 测试输出 Snapshot Hash、最终 Entity Snapshot，以及可选的 Headless 帧截图 Sheet；
- 同一 Build + Tape 的 Snapshot Hash 在所选 determinism 档位下逐字节一致；
- `--json` 使用 §138 同一 Diagnostic Schema，AI 可以据此闭环。

---

## 111. 游戏能力验收矩阵

| 能力         |                      语言支持 | Runtime/Profile 支持 | 结论       |
| ------------ | ----------------------------: | -------------------: | ---------- |
| Quick Game   | `system implements QuickGame` | 同一 Fixed Scheduler | 完整       |
| 固定 Tick    |     `system + trait + action` |            Scheduler | 完整       |
| 持久状态     |                `system state` |          State Store | 完整       |
| 状态分层     |    `@local` + 编译期域检查    |     Scheduler/World | 完整       |
| Snapshot/回滚 |           编译器生成 Snapshot |    World Snapshot | 需 Runtime |
| 计时器       |         `Cooldown`/`TickTimer` |          值类型      | 完整       |
| 存档         |                   `@persist`  |     Storage Service | 完整       |
| 确定性浮点   |          `cross_platform` 档位 |       `viso::math` | 需 Runtime |
| 输入         |   Typed Enum + `InputMap` 常量 |         Input Mapper | 完整       |
| 物理         |             Typed Handle Call |       Physics Engine | 需 Runtime |
| 碰撞         |            Typed Action/Event |         Event Buffer | 完整       |
| Entity/ECS   |                Generic/Handle |                  ECS | 需 Runtime |
| HUD          |                Component/View |       Widget Runtime | 完整       |
| Shader       |                 Shader Domain |          GPU Backend | 完整       |
| 热重载       |              Symbol/Migration |       Reload Runtime | 完整       |
| AI 生成      |        EBNF/Schema/Diagnostic |              CLI/LSP | 完整       |
| 测试/回放    |            `@probe` + Input Tape |      Headless Runner | 完整       |
| 联机         |          Simulation 层 = 复制集 |  Netcode（P3）      | 需 Runtime |
| AAA 资产管线 |                        可调用 |           需专门工具 | 非语言本身 |

因此答案不是“DSL 自己就是游戏引擎”，而是“DSL 有足够语义承载游戏 Runtime，并且不需要牺牲类型和工具能力”。

---

# 第十四部分：编译器架构与逐构造 Lowering

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
- 结构性语法错误使用 `E1401`–`E1405`（附录 C），不使用临时前缀码；
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
environment_reads
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
            invalidates: MEASURE | LAYOUT | PAINT | SEMANTICS
        }
    ]
}
```

匿名 Node 使用 Structural Symbol ID；`@stable` 覆盖默认生成策略。

---

## 122. Property Binding Lowering

```viso
width: panel_width;
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

长度值的 Lowering：

- 纯 `Dp` 常量直接 Lower 为 Layout 的 Fixed 值；
- 其余长度族值与 `MixedLength` Lower 为定长 `LengthTerms { dp, px, sp, em, pct: F32 }`，常量在编译期折叠，不产生表达式树或堆分配；
- `LengthTerms` 与其依赖掩码存放在 Binding 侧表，不进入 Layout 热存储；环境分量在依赖变化时预折叠：`fixed = dp + px / scale_factor + text_scale_curve(sp) + em × resolved_font_size`；
- Layout 热存储只保存 `ResolvedLength { fixed: F32, pct: F32 }`（8 字节），Layout 求解是一次 `fixed + pct × percent_basis`；纯 `Dp` 常量 `pct = 0`，与 Fixed 路径同成本；
- `font_size` 与 `line_height` 的 Percent 基准是字号，在 Typography 解析时折叠进 `fixed`，Layout 看到的 `pct` 为 `0`；
- 环境值变化时只重新折叠依赖掩码命中的 Binding（§19.6），不重新求值整棵树；
- `LengthTerms` 的非零分量掩码在编译期确定，并按 §19.6 写入 Binding 的环境依赖与 `invalidates` 集合（§87）。

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
    event: changed
    convert: TextConverter::to_model
    write_state: draft
    ignore_origin: binding_id
}
```

Converter Error 必须有 Schema 策略：拒绝更新、显示 Validation State 或产生 Event。不得静默写入错误值。

目标为 Component Input（U2.2）的 `bind value <=> x;` Lower 为 `value: x;` 加 `on changed(event) { x = event.value; }`：Input 实参读取 `x`，`@bindable` 配对 Event 的首个参数写回 `x` 的 Lens；这类 `bind` 不接受 `using`（`E3711`）。

目标为内置 Widget 双向 Property 的 `bind checked <=> on;` 同样 Lower 为读取 `on` 的 Property 条目加写回 Handler：控件的内建响应读取该条目得到当前值，投递 Property 配对的 Event（主 Property 为 `changed`，其余为 `<name>_changed`）；写回 Handler 取 Payload 首个字段写入 `on` 的 Lens，在作者声明的同名 Handler 之前运行。Converter 尚未挂载，带 `using` 的内置 Widget `bind` 报 `E3711`。

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

### 125.1 Adaptive Environment Read Lowering

读取：

```viso
if env.size_class == SizeClass::Compact {
    CompactShell {}
}
```

Lower 为 typed environment dependency：

```text
EnvironmentRead {
    kind: SizeClass,
    scope: NearestAdaptiveScope,
    value_type: SizeClass,
    revision_source: AdaptiveScopeRevision,
}
```

`env.constraints`、`env.safe_area`、`env.keyboard_inset`、`env.input` 等必须使用不同的 Environment Kind 和 revision source；禁止把整个环境对象作为一个粗粒度全局 revision。

### 125.2 Adaptive Branch Lowering

```viso
match env.size_class {
    SizeClass::Compact => {
        CompactShell {}
    },
    _ => {
        WideShell {}
    },
}
```

Lower 为普通 `ConditionalIr/MatchIr` + Environment Dependency。只有 `SizeClassRevision` 变化时才重新求值结构分支；原始 Window Width 改变但 SizeClass 未变，不得触发该结构 patch。

如果结构变化，引发：

```text
STRUCTURE
-> MEASURE
-> LAYOUT
-> affected PAINT/HIT_TEST/SEMANTICS
```

若只读取 `env.safe_area` 绑定 Padding，则禁止无条件标记 `STRUCTURE`。

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
    state_tiers        // 每个 State 的 Simulation/Local 层
    snapshot_layout    // Simulation State 的 Stable ID 与字段 Schema
    persist_keys
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
definition origin
template expansion callsite
macro/schema generated origin
inlined function origin
```

Behavior IR 的每个 Function 记录其定义模块与名称（Definition Origin），每条 Instruction 记录 Primary Span；Template 展开、Schema 生成与内联不产生 Behavior IR 之前，其余三项为空。

诊断展示：

1. 用户最接近的 Primary Span；
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

# 第十五部分：AI Vibe Coding 合同

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
viso fmt <paths>
viso check [package] --json
viso schema <symbol> --json
viso explain <error-code> --json
viso dump ast <file> --json
viso dump hir <file> --json
viso dump ui-ir|reactive-ir|behavior-ir|shader-ir|system-ir <file> --json
viso test [package] --json
viso snapshot <component> --output=<path>
viso test game <scenario> --frames=<n> --seed=<seed> --json
```

命令退出码以 `Viso_CLI.md` §7 为准（`0` 成功、`1` diagnostics 失败、`2` CLI 使用错误、`3` 环境/工具链不可用、`4` 构建失败、`5` 运行期/设备失败、`6` 打包失败、`7` 工具服务失败、`130` 用户中断）。

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
    "column_utf16": 9,
    "end_line": 18,
    "end_column_utf16": 14
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

位置对象（`primary` 与 `related[]` 的元素）：`file` 相对 project root、以 `/` 分隔；`byte_start`/`byte_end` 为文件内字节区间；`line`/`column_utf16` 为起点、`end_line`/`end_column_utf16` 为终点，行与列都从 1 起，列按 UTF-16 code unit 计。`related[]` 元素另带 `message`（该位置的标签）。无源码位置的诊断 `primary` 为 `null`、`related` 为空。`expected` 列出 primary 处可被接受的各个形式（类型或构造，按源码写法），`actual` 为该处实际所写；诊断不是失配时 `expected` 为空数组、`actual` 为 `null`。`related[]` 与 `fixes[].edits[]` 可以指向 primary 所在文件以外的 Package 文件（例如导入目标模块中的声明），各自的 `file` 给出其文件；`fixes[].edits[].file` 的取值规则同 `file`。

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
viso schema viso::widgets::Button --json
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
      "invalidates": ["MEASURE", "LAYOUT", "PAINT", "SEMANTICS"]
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

对象字段：

- `kind`：`component`、`native_library`、`native_function` 或 `native_type`；查询 `Symbol.member` 时另有 `member`，`inputs`/`events`/`functions` 只保留该成员；
- `inputs`：Component 的 Property（分组 Property 写作 `semantics.label`，`invalidates` 为该 Binding 实际失效的 Dirty Class 规范名，`two_way` 标记可 `bind`），或 Native 函数的参数；`default` 未记录时为 `null`；
- `events`：`bubbles` 为 Capture → Target → Bubble 路由的输入事件为真；Component 自身事件不冒泡；
- `capabilities`：函数自身所需，或 Library/Type 全部函数所需的并集；
- 非 Component 另有 `functions`（每项：`name`、`symbol`、`kind`、`method`、`params`、`returns`、`capabilities`、`thread`、`deterministic`、`realtime_safe`、`cost`）；`native_library` 另有 `types`；`native_type` 另有 `ownership` 与 `thread`。

未知符号为 `E2001`，附最近的符号或成员名。

AI 在使用未知 Component/Property/Event 前必须查询 Schema 或依赖已锁定版本的本地索引。

---

## 140. Formatter 唯一输出

Formatter 必须规范：

- 4 空格缩进；
- 简单语句分号；
- Trailing Comma 用于多行列表、参数和 Record；
- 一个空格围绕二元 Operator；
- 泛型参数与实参的 `<` `>` 紧贴两侧（`List<List<I64>>`），不按二元 Operator 加空格；
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
8. 响应式布局优先使用 `env.size_class` / `env.constraints`，不按设备名称硬编码；
   长度默认用 `dp`；随文字缩放的尺寸用 `sp`/`em`；相对父级用 `%` 或 `MixedLength`（如 `100% - 16dp`）；`px` 只用于发丝线和像素对齐；
9. Safe Area、Keyboard、Foldable 使用 typed adaptive environment；
10. 小型游戏可使用 `QuickGame`，多子系统游戏使用完整 System/Trait；
11. Shader 使用定宽类型；
12. 每次改动后运行 Formatter 和 Check；
13. 根据 JSON Fix 修复；
14. 运行最小测试和 Snapshot。

AI 禁止：

- 全仓库盲目字符串替换；
- 为绕过类型错误改成 Dynamic/String；
- 忽略 Capability 诊断；
- 删除失败测试；
- 在 View 内执行副作用；
- 用索引替代真实 Stable Key；
- 在错误时显示空白替代 Last-good UI；
- 修改 EBNF 却不同时更新规范、Parser Golden、Formatter 与测试。

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

# 第十六部分：实现阶段与验收

## 151. 实现阶段

阶段与 §25.1 Surface Tier 对齐：P0、P1 只实现 Core，P2 实现 Standard，P3 实现 Advanced；后一阶段不得成为前一阶段 Vertical Slice 的前置条件。

### P0：Core 语言前端

- Lexer；
- Lossless CST；
- Module/Import 与三种源码入口（§22.1）；
- Record/Enum；
- Component/Input/State/Computed/Action/View；
- Node、Property、Block Event、`if`/`match`/Keyed `for`；
- 基础 `fn`、`system` 声明；
- Expression/Operator/Pattern；
- Type Inference；
- Formatter；
- JSON Diagnostic。

### P1：Core 运行时、自适应与 System

- Reactive Graph；
- Transaction；
- Keyed List；
- Conditional Preserve；
- UI Diff/Patch；
- Last-good Hot Reload；
- Typed Adaptive Environment；
- LocalConstraints / AdaptiveScope / SizeClass；
- SafeArea / KeyboardInset / DisplayFeature；
- Adaptive branch dependency tests；
- System 与 `FixedUpdate` / `FrameUpdate`；
- Quick Game Profile；
- Deterministic InputSnapshot / replay contract；
- Tick 时钟、超限策略与单步（§106.1）；
- 输入边沿语义与 `InputMap`（§106.2、§106.3）；
- 状态分层与 Simulation 域检查（§106.4，`E9103`–`E9105`）；
- `Cooldown`/`TickTimer`（§106.6）；
- Snapshot/Restore（§106.7）；
- 游戏重载层分类（§110）；
- Typed Native Schema；
- Shader Profile Entry、Shader IR 和 ABI。

### P2：Standard

- Effect；
- Task；
- Resource；
- Slot；
- Style/Theme；
- Hot Reload State Migration；
- Capability 检查；
- `@persist`（§106.8）；
- AudioProcess 实时域检查（§108.3）；
- 回溯重放与 `@probe`/Input Tape 工具（§110.4、§110.5）。

### P3：Advanced

- 用户定义 Trait/Impl、一般泛型、Const Generic、`dyn` Trait；
- Template/Part；
- 手写 Native 声明；
- 多 System Game Profile / physics integration contract；
- `cross_platform` 确定性浮点（§106.5）；
- System IR 的 Release Native Lowering（§108.2）；
- 联机复制与回滚；
- AI Structured Edit；
- Cross-backend Validation。

每一阶段都必须通过规范、Parser Golden、Formatter、Schema Golden 和测试共同锁定已声明为 stable 的语义；任何语义变更必须先更新规范、诊断与测试。

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
- Unit `%` 与 Modulo `%` 区分，包括 `50%`、`50%3`、`50 % 3` 和 `100%-8dp`；
- `1em`、`1.5em` 为 Unit Literal，`1e5` 为 Float，`1e2dp` 为 `E1203`；
- Numeric Separator、Escape、Raw String Hash 数量的边界测试；
- `=>` 只在 Match；
- `<=>` 只在 Bind；
- `child` 作为非法 View 语法产生稳定诊断；
- Resource 重复 Item；
- 深度和 Token 数预算；
- 未闭合字符串/注释/Block 恢复；
- Unicode Identifier 和 Confusable；
- 上下文关键字在 §12.4 位置之外按 identifier 解析，严格关键字在 Label 位置被接受、在 Binding 位置报 `E1301`；
- Incremental Reparse 等价全量 Parse。

### 152.1 文档示例测试

本文与 `Viso_DSL_Rationale.md` 中的代码块受 CI 检查：

- ` ```viso ` 块必须被以下任一入口完整接受：`CompilationUnit`、`ComponentMember*`、`NodeMember*`、Block Body（`Statement* TailExpression?`）或 `Expression`；第三部分（词法规范）中的块只要求完整 Tokenize；
- ` ```viso-invalid ` 块必须在上述全部入口下产生至少一个 Parser 诊断；
- ` ```viso ` 块中注释之外不得出现 `...` 等占位符；
- 可解析但有类型或语义错误的示例（例如标注 `E2103` 的块）仍使用 ` ```viso `，其诊断由类型验收（§153）覆盖；
- 非 Viso 语法片段（Trait Bound 片段、IR 转储、Schema 摘要）使用 ` ```text `。

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
- Computed Cycle 输出路径；
- 长度族跨单位加减得到 `MixedLength`，`MixedLength` 赋给具体单位报 `E2106`，比较/相乘报 `E2107`；
- 无 `percent_basis` 的 Property 接受 Percent 报 `E3104`；
- 经 `state`/`computed`/`const`/局部绑定/字段访问/Record 默认值到达的 Percent 分量报 `E3104`；经函数调用、比较结果到达的不报；
- 给 Input/Computed/Const/不可变局部绑定赋值报 `E2110`；
- `bind` 右侧不是 State Lens 报 `E3107`，无 Converter 时两边类型不同报 `E2103`；`@bindable` 的 Event 不存在或首参数类型不符报 `E3701`；
- 不同 Component 声明同名成员不冲突，成员遮蔽同名 Module `const`；
- 节点不接受的 Event、`emit` 未知 Event 或实参与参数不符报 `E3202`；Handler Payload 字段按 Event 参数类型或标准 Payload Record（U7.1）参与推断；
- `font_size` 中的 `em`/`%` 以父节点字号为基准，其他 Property 以本节点字号为基准。

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
- Budget 超限可恢复；
- Window Width 改变但 SizeClass 不变时，仅订阅 SizeClass 的结构分支不 Patch；
- SizeClass 跨 breakpoint 时只 Patch 受影响 subtree；
- Local AdaptiveScope 使用局部宽度而不是 Window Width；
- SafeArea / KeyboardInset 改变只失效声明依赖者；
- TextScale 改变触发必要 Measure/Layout/Semantics；
- Adaptive branch preserve 保留声明可迁移的 focus/scroll/text state。

---

## 155. 游戏验收

- QuickGame 和等价单 System FixedUpdate 在相同输入下得到相同 simulation result；
- QuickGame 不依赖 UI frame callback 或 wall clock；
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
- 0 个与多个 Tick 的渲染帧中，每个输入边沿恰好被看到一次；
- Simulation 域访问 `@local` 或非确定性来源在编译期被拒绝；
- `restore(snapshot(s))` 后继续运行与不中断运行逐 Tick 一致；
- 回滚重算不重复交付 Presentation Command；
- Timer 以整数 Tick 计时，Logic-only Reload 保留剩余 Tick；
- `@persist` 状态跨进程重启保留，类型变化走迁移；
- 重载层分类与 §110 表一致；
- `cross_platform` 档位下同一 Tape 在所有 Tier-1 目标 Snapshot Hash 一致；
- CPU Reference Shader 与至少一个 GPU Backend Golden Image 在容差内一致。

---

## 156. 人类可用性验收

至少进行以下任务测试：

1. 新手在只阅读十分钟 Quick Start 后完成 Counter；
2. 添加表单与双向绑定；
3. 添加 Keyed Todo List；
4. 添加异步搜索 Resource；
5. 创建带 Slot 的复用 Component；
6. 用 `SizeClass` 实现手机/平板/窄桌面三种布局；
7. 在局部侧栏中使用 `AdaptiveScope` 验证组件不依赖 Window Width；
8. 编写一个 Quick Game；
9. 把 Quick Game 中一个子系统拆成独立 FixedUpdate System；
10. 修复一个 Compiler Diagnostic；
11. 执行 Hot Reload 并保留输入焦点。

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
50 个 Adaptive/Responsive Prompt
50 个 SafeArea/Keyboard/Foldable Prompt
50 个异步 Resource Prompt
50 个组件抽取 Prompt
50 个 Shader Prompt
50 个 Quick Game Prompt
50 个多 System 游戏逻辑 Prompt
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
- 严格关键字与上下文关键字清单由 Lexer/Parser 测试锁定；
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
STRICT_KEYWORD
END_OF_FILE
```

`STRICT_KEYWORD` 指 §12.1、§12.2 中任一严格关键字 Token。上下文关键字（§12.3）由 Lexer 产生 `IDENT`；带引号的上下文关键字终结符（如 `"state"`）匹配文本相同的 `IDENT`，识别位置见 §12.4。Binding/声明位置使用 `IDENT`；Label 位置使用：

```ebnf
Label
    ::= IDENT | STRICT_KEYWORD
```

Trivia 不进入普通 Production，但保留在 Lossless CST。

---

## A.2 Compilation Unit 和 Declaration

```ebnf
CompilationUnit
    ::= ImportDecl* TopLevelDecl* END_OF_FILE

ViewFragment
    ::= ViewStructureItem* END_OF_FILE

ComponentEntry
    ::= ImportDecl* Attribute* ( ComponentDecl | ComponentDeclBody ) END_OF_FILE

ModulePath
    ::= IDENT ( "::" Label )*

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
     |  Label ":" Expression
```

---

## A.3 Path、Generic、Type 和约束

```ebnf
Path
    ::= IDENT ( "::" Label )*

TypePath
    ::= IDENT GenericArgs? ( "::" Label GenericArgs? )*

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
    ::= Attribute* Label ":" Type ( "=" ConstExpression )? ";"

EnumDecl
    ::= "enum" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" EnumVariant* "}"

EnumVariant
    ::= Attribute* Label VariantPayload? ";"

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
    ::= "component" ComponentDeclBody

ComponentDeclBody
    ::= IDENT GenericParams? ImplementsClause? WhereClause?
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
    ::= Label ( "." Label )*

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
    ::= "." Label | "[" Expression "]"

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
    ::= ComparisonExpression ( "&&" ComparisonExpression )*

ComparisonExpression
    ::= BitOrExpression ( ComparisonOperator BitOrExpression )?

ComparisonOperator
    ::= "==" | "!=" | "<" | "<=" | ">" | ">="

BitOrExpression
    ::= BitXorExpression ( "|" BitXorExpression )*

BitXorExpression
    ::= BitAndExpression ( "^" BitAndExpression )*

BitAndExpression
    ::= ShiftExpression ( "&" ShiftExpression )*

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
     |  "." Label
     |  "?." Label
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
    ::= Label ":" Expression
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
     |  Label ":" Expression
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
    ::= "mut"? IDENT "@" RangePattern
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
    ::= "-"? INT_LITERAL | CHAR_LITERAL | STRING_LITERAL
     |  "true" | "false" | "None"

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
    ::= IDENT "::" Label ( "::" Label )*

RecordPatternField
    ::= Label ":" Pattern | IDENT | ".."
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
|    5 | `sp`、`min` 等单位未闭合           | 后缀全集固定为 dp/px/sp/em/%/ns/us/ms/s/min/deg/rad/turn/hz/khz       | `E1204` 未知单位            |
|    6 | State 前向引用                     | 一律禁止；Computed 可建立无环前向依赖图                               | `E2104` 指向声明与引用      |
|    7 | Branch/List 都叫 key               | Branch 使用 `preserve "literal"`；List 使用 `key expression`          | `E3301` / `E3401`           |

附加裁决：

- 简单语句一律有分号；
- `=>` 只属于 Match；
- `<=>` 只属于 Two-way Binding；
- `:` 只承载规范定义的类型/字段/Property Binding 语义，不承载隐藏的 apply/merge 语义；
- `:=`、`+:`、`<:`、`>:`、`^:` 不属于 Viso 1.0；
- View `for` 强制 Key；Behavior `for` 不允许 Key；
- `Float` 不作为模糊兼容别名；需要明确使用 `F32` / `F64`。

---

# 附录 C：稳定错误码（Normative）

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
| E1207  | 非法颜色字面量                                          |
| E1208  | 字符字面量必须恰好包含一个字符                          |
| E1209  | 非法源字符（NUL、孤立 `\r`、无法开始任何 Token 的字符） |
| E1210  | Raw String 定界 `#` 超过 255 个                         |
| E1301  | 严格关键字用于 Binding/声明位置（§12.5）                |
| E1401  | 未闭合定界符 `(` `[` `{`                                |
| E1402  | 多余的闭合定界符                                        |
| E1403  | 无法开始任何声明/语句的 Token（已归入 `ErrorNode`）     |
| E1404  | 缺少文法要求的 Token（以 `MissingToken` 占位）          |
| E1405  | 此处需要 Expression                                     |
| E2001  | 未解析符号                                              |
| E2002  | Import 歧义                                             |
| E2003  | 值初始化循环                                            |
| E2004  | 表达式泛型缺少 Turbofish 或 Const Argument 缺少 `const` |
| E2101  | `Float` 类型已删除                                      |
| E2102  | 非法隐式数值转换                                        |
| E2103  | 类型不匹配                                              |
| E2104  | State Initializer 前向引用                              |
| E2105  | Computed 循环依赖                                       |
| E2106  | MixedLength 不能在布局前定型为具体单位                  |
| E2107  | 非法量纲运算（MixedLength 比较/乘除、Percent 加标量等） |
| E2108  | `format` 模板与实参不匹配（§17）                        |
| E2109  | 长度族值常量除以 0（§19.8）                             |
| E2110  | 赋值目标不可写（§62.1）                                 |
| E2201  | Trait Bound 未满足                                      |
| E2202  | Trait Impl 重叠或歧义                                   |
| E2301  | 非穷尽 Match                                            |
| E2302  | 不可达 Pattern                                          |
| E2303  | `let`/`for`/Closure 参数使用 Refutable Pattern（§70.1） |
| E2401  | Closure 参数无法推断                                    |
| E2501  | Effect Kind 调用违规                                    |
| E2502  | View/Computed 中存在副作用                              |
| E2601  | 缺少 Capability                                         |
| E2701  | 类型不能实现 StableKey                                  |
| E2702  | 重复 Runtime Key                                        |
| E2801  | Control Head 中的 Record Expression 必须加括号          |
| E2802  | 非结合操作符链式使用（§63.1）                           |
| E2803  | `return`/`break`/`continue` 位置非法（§62.1）           |
| E3001  | 已删除的 `child` 关键字                                 |
| E3002  | View Cardinality 不满足                                 |
| E3003  | Component 没有 Default Slot                             |
| E3004  | 多个 Slot 标记 `@default`（§45.1）                      |
| E3101  | 未知 Property                                           |
| E3102  | Property 重复绑定                                       |
| E3103  | Property 不支持双向绑定                                 |
| E3104  | Property 未声明 Percent Basis，不接受 Percent           |
| E3105  | Percent Basis 不确定（Debug Runtime 警告）              |
| E3106  | 长度解析为非有限值（Debug Runtime 警告，§19.8）         |
| E3107  | `bind` 右侧不是 State Lens（§51）                       |
| E3201  | 已删除的事件箭头语法                                    |
| E3202  | 未知 Event 或错误 Payload                               |
| E3301  | Conditional Preserve 必须是静态字符串，且在 Component 中唯一 |
| E3401  | View For 缺少 Key                                       |
| E3402  | Key Expression 不稳定                                   |
| E3501  | 未知 Slot/Part                                          |
| E3502  | Slot Cardinality 冲突                                   |
| E3601  | Template 无限递归                                       |
| E3701  | `@bindable` 配对错误（§U2.2）                           |
| E3702  | 父节点提供的 Property 用于错误或无法静态确定的父节点（§U3.8） |
| E3703  | `transition` 用于不可动画 Property 或类型不符（§U6.1）  |
| E3704  | 交互节点缺少 Role 或可访问名称（警告，§U8.2）           |
| E3705  | Localizable Property 上的文本拼接/字面格式化（警告，§U10.3） |
| E3706  | `tr` 键或参数与消息目录不符（§U10.3）                   |
| E3707  | `VirtualList` Item Template 误用（§U9.1）               |
| E3708  | 交互节点缺少等价键盘路径（警告，§U8.2）                 |
| E3709  | 标注 [Runtime 待实现] 的 Property 使用了非默认值（§U1.1） |
| E3710  | `@selector` 误用（§U2.3）                               |
| E3711  | Handler、控制流区域或 Component 实例未能挂载：Runtime 未投递该 Event、Behavior 未能 Lower，区域位于 View 根、`VirtualList` 内或 `ui!` Fragment 中，或实例无法内联，或 `ui!` Fragment 中的 Rust Component 带 Property、Handler 或子项，或 `bind` 带尚未挂载的 `using` Converter（§40.1、§52、§56.1、§123） |
| E4101  | Action 中使用 Await                                     |
| E4102  | Task 跨挂起访问可变 State                               |
| E4201  | Effect 读取未声明依赖                                   |
| E4202  | Reactive Cycle                                          |
| E4203  | Effect Run Policy 与依赖列表不兼容                      |
| E4204  | Adaptive Cycle（§96.5）                                 |
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
| E7103  | 算术故障：整数溢出、除以零、移位量越界（运行时）        |
| E7104  | 索引越界（运行时）                                      |
| E7105  | 函数无法运行、缺少必需 Input 或内部故障（运行时）       |
| E7106  | Native 函数失败：返回错误或 Panic（运行时）             |
| E8101  | Shader 使用 Host-only 类型                              |
| E8102  | Shader 使用 F64                                         |
| E8103  | Shader Loop 无静态上限                                  |
| E8104  | Shader ABI 不匹配                                       |
| E9101  | Game System Order 循环                                  |
| E9102  | Fixed Tick 预算超限                                     |
| E9103  | Simulation 域访问 Local 状态或 Presentation 返回值（§106.4） |
| E9104  | Simulation 域使用非确定性来源（§106.4、§106.5）          |
| E9105  | Simulation 状态类型未实现 Snapshot（§106.4）             |
| E9106  | `@persist` 键重复、类型不可持久化或缺少 Capability（§106.8） |
| E9107  | 输入动作缺少目标平台的手柄/触屏路径（警告，§106.3）      |
| E9108  | AudioProcess 实时域违规（§108.3）                        |

错误码文案可以改进，但错误码语义不得在同一 Major 版本中复用。

---

# 附录 D–F（已移出）

附录 D（实现 AI 主提示词）、附录 E（设计质量复评）与附录 F（资料与证据说明）是说明性内容，已移至 `Viso_DSL_Rationale.md`，编号不变。

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
