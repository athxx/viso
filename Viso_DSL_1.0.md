# Viso DSL 1.0：语言设计与形式化编译规范

> 文档状态：Viso DSL 1.0 Draft（1.0 Final 前可修订；修订前本文是兼容实现唯一的规范依据）  
> 规范级别：语言语法、静态语义与运行时语义  
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

DSL 文档集由以下文档组成，章节编号在文档集内全局唯一（§N、UN 在哪份文档中即以该文档为准）：

| 文档 | 内容 | 级别 |
| ---- | ---- | ---- |
| `Viso_DSL_1.0.md`（本文） | 词法、文法、类型、运行时语义、Shader 语言、合并 EBNF、错误码 | 规范性 |
| `Viso_DSL_UI.md` | 自适应环境（§96）与 UI 标准面（U1–U13）：Widget、布局、文本、动画、输入、语义、列表、国际化、导航、Theme | 规范性 |
| `Viso_DSL_Game.md` | Game Profile（§104–§111） | 规范性 |
| `Viso_DSL_Implementation.md` | 编译器架构与逐构造 Lowering（§112–§135）、工具合同（§136–§143）、实现阶段与验收（§151–§158），以及本文各节的当前实现说明 | 说明性（工具合同与验收为规范性） |
| `Viso_DSL_Rationale.md` | 设计判断、外部参考与实现提示 | 说明性 |

本文规范性地定义：

1. 清晰度硬规则与 Surface 分层；
2. Rust 侧三种源码入口（§22.1）；
3. UTF-8、标识符、注释、关键字、字面量与单位的词法规范；
4. 模块、类型、组件、System、函数、行为、资源、View 与 Shader 的形式文法；
5. 表达式、运算符优先级、结合性、闭包与 Pattern 文法；
6. 类型推断、泛型、Trait、约束、转换和子类型规则；
7. 响应式状态、节点身份、事件、异步与热重载语义；
8. 稳定错误码。

本文不定义：

- 具体 GPU API 的实现；
- 具体物理引擎、音频引擎或 ECS 的内部算法；
- 完整标准库 API；
- Widget 的属性清单（见 `Viso_DSL_UI.md`）；
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
| 分号与换行的关系依赖猜测                      | 换行结束完整条目，同一行多个条目用 `;` 分隔，续行由行首 Token 决定（§14） |
| `:=`, `+:`, `<:`, `>:` 等承担多种隐藏语义     | 从核心语言移除；View 属性只保留 `property: expr;`，其余使用明确关键词 |

### 1.3 学习曲线

语言采用分层学习；Level 与 §25.1 Surface Tier 一一对应，§151 实现阶段按同一 Tier 排序：

- **Level 1（Core UI）**：Component、Input、State、Computed、Action、View、Node、Property、Event、`if`/`match`/Keyed `for`、Record/Enum、基础 `fn`；
- **Level 2（Core System/Shader）**：`system` 与 Native Schema 提供的 Scheduler Trait（如 `FixedUpdate`）、Shader Profile Entry；
- **Level 3（Standard）**：Effect、Task、Resource、Slot、Style、Theme、Hot Reload Migration；
- **Level 4（Advanced）**：用户定义 Trait/Impl、一般泛型、Const Generic、`dyn` Trait、手写 Native 声明、细粒度 Capability 标注。

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
- 编译器应警告 Unicode Confusable：按 UTS #39 Mixed-Script 检测的 Highly Restrictive 级别，忽略 Common/Inherited 字符后，Identifier 各字符的 Script_Extensions（汉字按 Augmented Script Set 可与假名、谚文或注音同用）没有共同 Script，且也不是“拉丁字母加日文、韩文或注音汉字之一”时报告 `E1102` 警告；`café`、`名前かな`、`ユーザーID` 不报告，`pаy`（西里尔 `а`）报告；
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
style theme requires capability
on capture bubble emit transaction start move
when run cleanup success error cancelled
node bind using preserve key
load policy scope
vertex fragment compute uniform instance varying texture sampler
```

EBNF 中带引号的上下文关键字（如 `"state"`）匹配文本相同的 identifier Token。`capability` 当前没有专用语法位置（`@capability` 是普通 Attribute Path），保留在本表中供 Schema 与后续版本使用。

### 12.4 上下文关键字的识别位置

Parser 只在下列位置、以至多 2 个 Token 的 Lookahead 把上下文关键字识别为关键字：

| 位置                        | 识别形式                                                                                           |
| --------------------------- | -------------------------------------------------------------------------------------------------- |
| Statement/Expression 起始   | `emit IDENT`、`start IDENT`、`transaction {`、`move \|`（含 `move \|\|`）                          |
| 顶层声明起始                | `style IDENT`、`theme IDENT`                                                                       |
| Component/System 成员起始   | `input`/`state`/`computed`/`event`/`effect`/`resource` 后随 IDENT；`view {`                        |
| Node Body / View Block 项起始 | `node IDENT :`、`on IDENT`、`on capture`/`on bubble`、`bind IDENT`、`slot`（其后不是 `:`/`.`）       |
| View 控制结构               | `if` Header 后的 `preserve STRING`；View `for` Header 中的 `key`                                    |
| Resource Item               | `load =`、`key =`、`policy =`、`scope =`                                                           |
| Start Handler               | `policy =`、`success (`、`error (`、`cancelled {`                                                  |
| Effect 子句                 | `when (`、`run`、`cleanup {`                                                                       |
| Callable 签名尾部           | `requires {`                                                                                       |
| Two-way Binding / Style     | `bind` 之后的 `using`；Style Item 起始的 `when`                                                    |
| Shader 成员                 | `uniform`/`instance`/`varying`/`texture`/`sampler` 后随 IDENT；`vertex (`、`fragment (`、`compute (` |

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

## 14. 换行与分号

条目（Item）是声明、成员、Property Binding、Event Handler、Statement 与 View 结构项。条目以换行或 `;` 结束：

```viso
state count = 0
let mut i = 0; let mut j = 1
count += 1
emit changed(count)
```

规则：

- 一个条目在语法上已经完整时，其后的换行结束它；不完整时（如 `let x =`、`if ready` 之后），换行是空白；
- 同一行写多个条目时，相邻条目之间必须写 `;`，以 `}` 结束的条目之后除外（`Row { Button {} Button {} }`）；缺少时报 `E1404`；
- `( )` 与 `[ ]` 之内的换行一律是空白；Record 表达式的 `{ }`（§65）中，字段之间的换行与 `,` 一样分隔字段（同一行的字段仍以 `,` 分隔），字段值内的换行按本节其余规则处理，行首的 `..` 开始展开项而不延续区间；
- 下一行的首个 Token 是以下之一时，它延续上一行而不结束条目：`.` `?.` `?` `::` `,` `:` `=` 与全部复合赋值、`->` `=>` `as` `else`，以及不能作为前缀的二元操作符 `* / % << >> & | ^ == != < <= > >= && || ?? .. ..=`；
- 行首的 `+` `-`（前缀 Unary）、`(`、`[` 与 `{` 开始新的条目；因此 Call、Index、Record 表达式与 Node Body 的开括号必须与其前面的部分写在同一行；以 `|`/`||` 开头的 Closure 若要独占一行作为表达式，须加括号；
- `return`、`break` 后紧跟换行时没有值；
- `match` 的 Arm 以换行或 `,` 分隔；
- `;` 只是分隔符，不改变含义：行尾或 `}` 前的 `;` 是多余的，Formatter 删除它们；
- 以 Block 结束的构造（`action a() {}`、`if c {}`、`Text {}`、`on click {}`）在其 `}` 处结束。

Block 的最后一个条目若是 Expression，它是 Block 的值（Tail Expression，§61），仅在函数、Action、Task、闭包和普通表达式 Block 中出现；期望类型为 `Unit` 时（未写返回类型的 `fn`、Action、Event Handler、期望 `Unit` 返回的 Closure）其值被丢弃。`view` 和 Node Body 不存在 Tail Expression。

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
- 语言中不存在 `Float` 类型；任何书写类型的位置（参数、返回值、字段、Variant Payload、`let`、`const`、成员、泛型实参、Function Type、`as`）写 `Float` 都由名字解析报一次 `E2101`，不再级联类型不匹配；
- 无后缀浮点是“未定型浮点常量”；
- Host 域无上下文时默认 `F64`；
- Shader 域无上下文时默认 `F32`；
- `F64 -> F32` 不存在隐式运行时转换；
- 未定型字面量若能精确或按 IEEE 754 正常舍入到上下文要求的 `F32`，可以直接实例化为 `F32`；这不是 `F64 -> F32` 转换；
- `NaN` 和 `Infinity` 不使用特殊字面量，由标准库常量提供。

示例：

```viso
let host_default = 1.25 // F64
let shader_value: F32 = 1.25 // 字面量直接定型为 F32
let explicit = 1.25f32
```

---

## 17. 字符串与字符字面量

普通字符串：

```viso
"hello"
"line 1\nline 2"
"unicode: \u{1F680}"
"Count: {count}"
"{user.name} 有 {items.len()} 项，{{未读}}"
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
- 普通字符串可以插值：`{` 与 `}` 之间是一个 Expression（洞），求值后按 `Display` 转为文本拼入；`{{`、`}}` 表示字面大括号；Raw String 与字符字面量不插值；
- 洞内是完整的 Expression，可以含括号、调用、`if`/`match` 与嵌套字符串；洞不能为空，洞内不能换行（字符串本身不跨行）；
- 洞的类型必须实现 `Display`，否则报 `E2108`；不支持格式说明符，也不读取 Locale；数字、日期的本地化格式与语序由 `tr`（U10.3）负责；
- 内建 `Display` 的类型：`Bool`、全部整数与浮点类型、`Char`、`String`、单位长度 `Dp`/`Px`/`Sp`/`Em`/`Percent`、`Duration`、`Angle`、`Frequency`；`Bytes`、`Unit`、`Color`、`MixedLength`、Tuple、`List`、`Map`、`Option`、`Result`、Range、函数、`Handle`/`NodeRef` 以及 Record 与 Enum 均无 `Display`，须显式映射为文本（例如 `match` 到 `tr(...)`），否则报 `E2108`；
- 含洞的字符串是 `String` 类型的 Expression，不是字面量：要求字符串字面量的位置（Import 路径、Attribute 实参、`asset(..)`、`preserve`、`MessageKey`）写含洞的字符串时，按该位置的规则报错；
- 词法上，含洞的字符串是 `StringStart`（`"…{`）、洞内的 Token、零个或多个 `StringMiddle`（`}…{`）与 `StringEnd`（`}…"`）组成的序列，不含洞的字符串仍是一个 `StringLiteral`；Lexer 以定界符栈找到洞的 `}`：洞内配对的 `{ }` 不结束洞；
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
const BRAND: Color = #2ecc71
const OVERLAY: Color = #00000080
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

`numeric_body` 与后缀之间不得出现空白。`unit_numeric_body` 不含指数部分：`1e2dp` 是 `E1203`。`e`/`E` 只有在其后紧跟 `[+-]? decimal_digit` 时才开始浮点指数，因此 `1em`、`1.5em` 始终词法化为 Unit Literal，`1e5` 始终是 Float Literal。`ui!` 与 `component!` 的源文本先经 Rust 词法：Rust 把 `1em`、`1.5em` 读作缺少数字的指数并拒绝，因此内联宏中不能书写 `em` 字面量；`em` 长度写在 `.vs`（`view!`）中，纯 Rust 路径使用 `LengthTerms::em`。

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
let total: Duration = 1s + 250ms
let half_turn: Angle = 180deg
let inset: Dp = 16dp * 2
let content_width = 100% - 2 * inset // MixedLength
let icon_size = 1.25em // Em
```

禁止：

```viso
let x: Dp = 10dp + 5px // E2106：MixedLength 不能在布局前定型为 Dp
let y = 1s + 2dp // E2103：跨量纲
let z = (100% - 8dp) < 200dp // E2107：MixedLength 在布局前不可比较
let w = 10dp < 5px // E2103：不同单位比较
```

`MixedLength` 的求值被推迟到 Layout 阶段；它不是运行时隐式单位转换。需要具体单位值时，必须在具有 Layout Context 的受控 API 中显式解析：

```viso
let px: Px = layout.dp_to_px(10dp)
let resolved: Dp = layout.resolve_length(node_ref, 100% - 16dp)
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
- 纯 Rust 表面：`LengthTerms`（`LengthTerms::dp/px/sp/em/pct` 构造，`+`、`-`、一元 `-`、与 `F32` 的 `*`/`/` 逐项运算）表示一个五系数值；`NodeLengths { width, height, gap, font_size }` 是一个 Node 读取环境的长度；`BuildCx::bind_lengths` / `NodeStore::bind_lengths` 绑定它们，在下一次 Layout 开始时按 `LengthEnv { scale_factor, text_scale, base_font_size }` 与 Typography Context 折叠；`NodeStore::set_length_env` 移动环境；
- Property 的 Schema 类型决定其是否接受长度族、`MixedLength` 以及 `Fill`/`Fit` 等 Sizing 值。

示例（Property 名由 Widget Schema 定义，此处仅为说明）：

```viso
Column {
    padding: 1em

    Text(title) {
        font_size: 1.25em
    }

    Row {
        width: 100% - 32dp
        gap: 0.5em

        Icon {
            size: 1.2em
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
- 插值不改变 Property 的失效类别：动画 `width` 每帧标记 `MEASURE|LAYOUT`；只需视觉缩放时使用 `scale` 变换，只走 `TRANSFORM`（§U6.2）。

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

条目末尾的 `item_end` 是 `;` 或结束条目的换行（§14）；`NEWLINE` 不是 Token，由 Parser 依据换行前的条目是否完整与下一行的首 Token 判定。

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
- `ui!` Fragment 按一个只声明该 View 的无状态 Component 检查与降级：Prelude 的类型、枚举值与函数照常解析（如 `align: TextAlign::center`），值名不读取外围 Rust 作用域的变量，Fragment 自身未声明的名称报 `E2001`；读取 State 的 View 使用 `component!`；
- `.vs` 是规范外部文件格式；页面、Theme 与大型 Component 的热重载 SHOULD 使用 `view!`；内联宏的修改通常需要 Rust 增量编译，Runtime 不要求解析 Rust 源码来热重载内联宏；
- 宏内源码的诊断 Span 映射回 Rust 源文件中的宏调用位置（§134）；
- `view!("x.vs", name: expr, ...)` 传入根组件的 Input（§U11）：未给的 Input 取默认值，无默认值的 Input 未给、未知名称与类型不符的 Rust 值都是编译错误（类型在 `cargo check` 即检查）；
- `view!` 与 `component!` 交出根组件中只构建一次的具名节点的 typed handle：`view.nodes.save_button: Handle<kind::Button>`（Architecture §10.4.11）。

---

## 23. Import

```ebnf
import_decl      = "import", import_source, item_end ;

import_source    = module_path, [ import_suffix ] ;

import_suffix    = "as", identifier
                 | "::", "{", import_item,
                   { ",", import_item }, [ "," ], "}" ;

import_item      = identifier, [ "as", identifier ] ;
```

示例：

```viso
import viso::widgets::{Window, Column, Text, Button}
import app::model::User as AppUser
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
attribute         = "@", label, { "::", label }, [ "(", [ attribute_args ], ")" ] ;
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
| `@bindable(event)`         | Component 的 `input` 成员  | 与同名 Component 的 `event` 配对为双向属性（§U2.2）      |
| `@styleable`               | Component 的 `input` 成员  | 允许 Style 绑定该 Input（§U2.3）                         |
| `@selector`                | `Bool` 的 `input`/`state`/`computed` | 公开为 Style 状态选择器（§U2.3）               |
| `@const`                   | `fn`                      | 编译期可求值函数（§34）                                 |
| `@migrate(from: "...")`    | `fn`                      | Hot Reload 状态迁移函数（§94）                          |
| `@persist("key")`          | System/Component 的 `state` | 持久化状态（§106.8）                                  |
| `@local`                   | System 的 `state`          | Presentation 层状态，Simulation 不可访问（§106.4）      |
| `@probe`                   | System 的 `state`          | 每个 Tick 输出到测试 Trace（§110.5）                    |
| `@after(System, ...)`      | `system`                  | 在所列 System 之后运行（§106）                          |
| `@before(System, ...)`     | `system`                  | 在所列 System 之前运行（§106）                          |
| `@shader_value`            | `record`                  | Shader 可用值类型（§98）                                |
| `@max_iterations(n)`       | Shader 循环 Statement      | 循环迭代上限（§99）                                     |
| `@sampler(filter: …, address: …)` | Shader 的 `sampler` 成员 | Brush 的 Sampler 状态（§100）                     |
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
          handwritten native interface declarations
          fine-grained capability annotations
```

`Advanced` 可以存在于规范中，但不能成为 UI、Reactive、Game、Shader vertical slice 的前置条件。Rust Native Schema 是默认扩展路径。每个 Production 的成熟度（`stable` / `preview` / `reserved`）见附录 A.14。

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

`List<T>` 的内建方法（Receiver 为 `List<T>` 的方法调用，不经 Native Schema）：

| 方法 | 类型 | 语义 |
| ---- | ---- | ---- |
| `len()` | `I64` | 元素个数 |
| `is_empty()` | `Bool` | `len() == 0` |
| `get(i: I64)` | `Option<T>` | 越界为 `None` |
| `first()` / `last()` | `Option<T>` | 空为 `None` |
| `contains(x: T)` | `Bool` | `T` 必须可比较 |
| `push(x: T)` | `()` | 追加到末尾 |
| `insert(i: I64, x: T)` | `()` | 插入到 `i`，`i` 越界为 Runtime Fault |
| `remove(i: I64)` | `T` | 移除并返回第 `i` 个，越界为 Runtime Fault |
| `pop()` | `Option<T>` | 移除并返回最后一个，空为 `None` |
| `clear()` | `()` | 清空 |
| `retain(keep: Fn(T) -> Bool)` | `()` | 只保留 `keep` 为真的元素，保持顺序 |

规则：

- `push`/`insert`/`remove`/`pop`/`clear`/`retain` 修改 Receiver：Receiver 必须是可写位置（`state`、`let mut` 局部或其字段/下标路径），按普通赋值检查：不可写目标报 `E2110`，`fn` 中写 `state` 报 `E2501`，`computed` 中写报 `E2502`；修改 `state` 列表与 `todos = new_list` 一样按该 State 失效；
- 未知方法报 `E2001` 并给出最近方法名 Fix；实参个数或类型不符报 `E2103`；
- 列表按值语义：修改只影响被写的位置，不影响此前读出的副本；写入在原存储上进行，只有存储被共享时才复制一次；
- Audio Realtime 中 `push`/`insert` 会分配，报 `E9108`（§108.3），`remove`/`pop`/`clear` 原地进行。

`String` 的内建方法（Receiver 为 `String`，都不修改它）：

| 方法 | 类型 | 语义 |
| ---- | ---- | ---- |
| `len()` | `I64` | 字符（Unicode 标量值）个数 |
| `is_empty()` | `Bool` | `len() == 0` |
| `contains(s: String)` | `Bool` | 含有子串 `s` |
| `starts_with(s: String)` / `ends_with(s: String)` | `Bool` | 以 `s` 开头 / 结尾 |
| `trim()` | `String` | 去掉两端空白 |
| `to_lowercase()` / `to_uppercase()` | `String` | 按 Unicode 规则转为小写 / 大写，与 Locale 无关 |

未知方法报 `E2001` 并给出最近方法名 Fix；实参个数或类型不符报 `E2103`。比较大小写无关的文本时先各自 `to_lowercase()`。

---

## 28. Record

```ebnf
record_decl        = "record", identifier,
                     [ generic_params ],
                     [ implements_clause ],
                     [ where_clause ],
                     "{", { record_field }, "}" ;

record_field       = { attribute }, label, ":", type,
                     [ "=", const_expression ], item_end ;
```

示例：

```viso
@derive(Eq, Hash, StableKey)
export record TodoId {
    value: U64
}

export record User {
    id: TodoId
    name: String
    avatar: Option<Url> = None
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
                     [ tuple_variant | record_variant ], item_end ;

tuple_variant      = "(", [ type_list ], ")" ;
record_variant     = "{", { record_field }, "}" ;
```

示例：

```viso
export enum LoadState<T, E> {
    idle
    loading
    ready(T)
    failed(E)
}
```

Enum Variant 使用 Path：

```viso
let pending = LoadState::loading
let done = LoadState::ready(user)
```

`@derive(..)` 的每个实参是一个裸 Trait 名：编译器提供的 `Eq`、`Hash`、`StableKey`、`Snapshot`，或某个 Native Library 在 Schema 中声明的 Derive（如 `viso::game` 的 `InputAction`，无需 Import）；其他名字或非裸名实参报 `E2001`（附最近候选）。Schema Derive 只用于所有 Variant 都不带 Payload 的 Enum，否则报 `E2201`。

---

## 30. Trait

```ebnf
trait_decl         = "trait", identifier,
                     [ generic_params ],
                     [ ":", trait_bounds ],
                     [ where_clause ],
                     "{", { trait_member }, "}" ;

trait_member       = { attribute },
                     ( function_signature, ( item_end | block )
                     | action_signature, ( item_end | block )
                     | task_signature, ( item_end | block )
                     | associated_type_decl
                     | associated_const_decl ) ;

associated_type_decl = "type", identifier,
                       [ ":", trait_bounds ], item_end ;

associated_const_decl = "const", identifier, ":", type, item_end ;
```

示例：

```viso
export trait FixedUpdate {
    action fixed_update(frame: FixedFrame)
}

export trait StableKey: Eq + Hash + Clone {
    fn stable_hash(self) -> U64
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

associated_type_impl = "type", identifier, "=", type, item_end ;
associated_const_impl = "const", identifier, ":", type,
                        "=", const_expression, item_end ;
```

`impl Trait for Type` 是 Trait 实现；`impl Type` 是 Inherent Impl。

Trait 中带 Block 的 Callable 是默认实现：Impl 未给出同名成员时使用它，其中的 `Self` 是实现类型。首个参数可以是 `self`（类型为 `Self`，可写 `mut self` 或 `self: Type`）；带 `self` 的成员是 Method，以 `x.f(..)` 调用，不带 `self` 的是 Associated Function，以 `Type::f(..)` 或 `Trait::f(x)` 调用。

---

## 32. Type Alias 和 Const

```ebnf
type_alias_decl    = "type", identifier,
                     [ generic_params ],
                     "=", type, item_end ;

const_decl         = "const", identifier, ":", type,
                     "=", const_expression, item_end ;
```

`const_expression` 是可在编译期求值的 Expression 子集，禁止 I/O、State、Native Action、Task、Resource 和非确定性调用。

需要在编译期得到值的 `const`（如 `InputMap`，§106.3）只由字面量、取负的数、Enum 无 Payload Variant（含 Schema Enum）与 `@const` Native 调用构成：其他表达式或非 `@const` 调用报 `E2501`；`@const` Native 拒绝实参（如越界的死区半径）报 `E2112`。

---

# 第六部分：完整语法——可调用项、Component 与 System

## 33. Parameter、返回类型与 Capability

```ebnf
parameter_list      = [ ( self_parameter | parameter ),
                        { ",", parameter }, [ "," ] ] ;

self_parameter      = [ "mut" ], "self", [ ":", type ] ;

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
    return await Api::user(id)
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
    return value.clamp(0.0f32, 1.0f32)
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
    count += by
    emit changed(count)
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
    let response = await Api::profile(id)
    return response.decode()
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
    Settings::set_string("theme", theme_name)

    cleanup {
        Settings::flush()
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
- Cleanup 不写 State：其中的 State 赋值、`transaction` 与 Component `action` 调用报 `E2501`，Native 调用照常允许；
- Effect Body 中的 Component `action` 调用同样只能写在 `transaction { ... }` 内（§81），Native 调用不受此限；
- Effect Cycle 必须被检测并报告依赖链。

`EffectRun` 是标准库 Enum，不是 Parser 特例；Parser 只解析 Path。

---

## 38. Resource

### 38.1 唯一规范语法

```ebnf
resource_decl        = "resource", identifier, ":", type,
                       "{", { resource_item }, "}" ;

resource_item        = "load", "=", expression, item_end
                     | "key", "=", expression, item_end
                     | "policy", "=", policy_list, item_end
                     | "scope", "=", expression, item_end ;

policy_list          = "[",
                       [ expression,
                         { ",", expression }, [ "," ] ],
                       "]" ;
```

示例：

```viso
resource search_result: Resource<List<SearchItem>, SearchError> {
    load = SearchApi::query(query)
    key = query
    policy = [
        ResourcePolicy::debounce(250ms),
        ResourcePolicy::keep_latest,
        ResourcePolicy::cache_for(5min),
    ]
    scope = ResourceScope::component
}
```

### 38.2 配置约束

- `load` 必须且只能出现一次；
- `key` 必须且只能出现一次；
- `policy` 可以省略，默认 `[]`；
- `scope` 可以省略，默认 `ResourceScope::component`：每个挂载的实例各自加载、各自缓存；`ResourceScope::shared` 则让同一声明在 View 中的所有实例共享，同一 Key 只加载一次、缓存一份；
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
    ResourceState::ready(items) => { Results { items: items } }
    ResourceState::error(error) => { ErrorView { error: error } }
    ResourceState::reloading(items) => { Results { items: items; dimmed: true } }
}
```

Resource 状态是标准 Enum；语法没有硬编码上述成员。

---

## 39. Start Statement 与 Task 生命周期

```ebnf
start_statement      = "start", call_expression,
                       [ "as", identifier ],
                       [ start_handler_block ], item_end ;

start_handler_block  = "{", { start_handler }, "}" ;

start_handler        = "policy", "=", policy_list, item_end
                     | "success", "(", pattern, ")", block
                     | "error", "(", pattern, ")", block
                     | "cancelled", block ;
```

规范写法：

```viso
start save_profile(profile) as save_job {
    policy = [TaskPolicy::keep_latest]

    success (saved) {
        current = saved
    }

    error (reason) {
        last_error = Option::Some(reason)
    }

    cancelled {
        log::debug("save cancelled")
    }
}
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
- 复用通过 Composition、Trait、Style 和 Slot 完成；
- `input`、`event` 和 View 中的 `slot`（§45）构成公开 UI 接口；
- `state`、`computed`、内部 Action 和节点默认是私有实现；
- Trait 可以要求 Component 实现 Action 或 Fn；
- 成员 `const` 是只在所属 Component 内可见的常量，规则同顶层 `const`（§32）；
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
- 以下情形报 `E3711`：Component 直接或间接挂载自身；`bind` 目标为 Component Input 且带 `using`；向 View 不恰好挂载一个 Node 的实例传入非 Input Property 或非 Event Handler；`AdaptiveScope` 的 `basis` 不是常量 `dp` 长度（§96.3）；`SafeArea` 或 `KeyboardAvoiding` 带 `padding`（§96.6）；`padding` 写在非容器节点上（U3.3）；`Grid` 的 `areas` 或子节点的 `grid.area` 不是常量（U3.5）；`VirtualList` 的 `columns`、`min_column`、`gap`、`column_gap` 或 `masonry` 不是常量或长度为百分比（U9.3）；Style 以读取值的绑定给出布局 Property（U3.4）；
- `ui!` Fragment 中不是内置 Widget 的类型是外围 Rust 作用域中实现 `viso::ui::Mount` 的 Rust Component（可写作 Rust 路径，如 `widgets::Tally`）：`Default` 的手写 `impl Component` 由其 `Default` 构建，`component!` 声明的 Component 由其 `build` 挂载并自带 State 与 Handler；这类 Node 不接受 Property、Handler 与子项（`E3711`），名称无法解析时由 Rust 在该位置报错；Hot Reload 与 Release Package 的 Fragment 没有 Rust 作用域，这类类型报 `E2001`。

---

## 41. Input

```ebnf
input_decl           = "input", identifier, ":", type,
                       [ "=", default_expression ], item_end ;
```

规则：

- Input 是父组件传入的只读值；
- Component 内禁止给 Input 赋值（`E2110`）；
- Input 默认值必须是纯、确定的 Default Expression，其类型须与声明类型相容（`E2103`）；
- Input 默认值禁止读取另一个 Input、State、Computed、Native Runtime 或当前时间；
- 没有默认值的 Input 是必填属性；
- Input 类型是 Component Schema 的一部分，改变类型属于接口兼容性变更；
- Input 变化会根据实际依赖使 Computed、View、Effect 或 Resource Key 失效；
- 至多一个单向 Input 可以标 `@primary`，调用方以 `Card("Profile")` 的形式给出它（§50.1）；多于一个、标在 Input 以外的成员上或与 `@bindable` 同标报 `E3109`。

```viso
@primary input title: String
input enabled: Bool = true
```

---

## 42. State 与初始化顺序

```ebnf
state_decl           = "state", identifier,
                       [ ":", type ],
                       "=", init_expression, item_end ;
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
state min_count: I64 = 0
state count: I64 = min_count // 合法：只读前置 State
state doubled: I64 = count * 2 // 合法，但通常更适合 computed
```

非法：

```viso
state count: I64 = minimum // E2104：读取后置 State
state minimum: I64 = 0
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
                       "=", expression, item_end ;
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
    return sum + item.price
})

computed label = "{items.len()} items"
```

---

## 44. Event

```ebnf
event_decl           = "event", identifier,
                       "(", event_parameter_list, ")", item_end ;

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
event changed(value: I64)
event submitted(text: String, source: SubmitSource)
```

---

## 45. Slot

Slot 是 Component View 中留给调用方的位置。它在 View 中声明兼放置，不在成员区重复声明：

```ebnf
slot_item            = "slot", [ identifier ], [ view_block ] ;
```

- `slot` 是默认 Slot：调用方 Node Body 中的裸结构项（`view_structure_item`，§48）按源码顺序放在这里；
- `slot name` 是具名 Slot：调用方以 `name { .. }`（§53）填充；
- 带 `view_block` 的 Slot 有默认内容：调用方未填充时显示它，填充时整体替换它；默认内容属于定义方，读取定义方的 Input、State 与 Computed；
- 一个 Slot 放置零个或多个节点，在 Cardinality 检查中（§45.1）计为零个或多个；
- 同一个 Slot 名在一个 View 中至多出现一次，`slot` 至多一个，否则报 `E3502`；Slot 位于 `for` 内（会把调用方节点放置多次）同样报 `E3502`；Slot 可以位于 `if`/`match` 分支中，分支挂载时挂载调用方的节点；
- Slot 名属于 Component 的公开接口（Schema 的 `slots`），默认 Slot 的 Schema 名为 `children`；
- `slot` 只出现在 Component 的 View 中；Component 可以把自己的 Slot 转放进子 Component 的 Slot：`Card { slot }`、`Card { header { slot title } }`。

### 45.1 Cardinality

Native Widget 的 Slot 由其 Native Schema 声明 Cardinality：容器（`Row`、`Column`、`Flex`、`Grid`、`Stack`、`Absolute`、`Fragment` 等）的默认 Slot 接受零个或多个节点，`Scroll` 的默认 Slot 至多一个节点，叶子 Widget（`Text`、`Button`、`TextInput` 等）没有 Slot。

Cardinality 按结构项可能产生的节点数区间静态检查，区间内每个取值都必须被目标 Slot 接受：普通节点计 1；`Fragment` 计其子项之和；`slot` 计零个或多个；`if` 取各分支区间的并（无 `else` 时含 0）；`match` 取各分支区间的并；`for` 计零个或多个。不被接受时报 `E3502`。

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
                       [ capability_clause ], item_end ;

native_action        = "action", identifier,
                       [ generic_params ],
                       "(", parameter_list, ")",
                       return_type,
                       [ where_clause ],
                       [ capability_clause ], item_end ;

native_task          = "task", identifier,
                       [ generic_params ],
                       "(", parameter_list, ")",
                       return_type,
                       [ where_clause ],
                       [ capability_clause ], item_end ;

native_type_decl     = "type", identifier,
                       [ generic_params ],
                       [ ":", trait_bounds ],
                       [ where_clause ], item_end ;
```

静态区分：

- `native fn`：只读、确定或由 Schema 标记可安全用于纯上下文；
- `native action`：同步副作用；
- `native task`：异步、可取消副作用；
- Native Rust Panic 必须在 Bridge 边界被捕获并转为结构化 Runtime Fault；
- Native Handle 方法也按 `fn/action/task` 分类；
- Schema 必须声明线程域、Capability、参数所有权、错误类型和 Hot Reload 可迁移性。

### 47.1 生成的 Native Schema

Native Schema 由 Rust 侧生成（ADR 0036）：一个 Native Library 是一条模块路径（如 `viso::text`）下带版本的函数与 Handle 类型集合；编译器与运行时共享同一个 Registry，`.vs` 不重复声明签名。标准 Registry 含 `viso::text`、`viso::math`（`F64` 函数——`sqrt`/`abs`/`floor`/`round`/`min`/`max`/`clamp` 与 `sin`/`cos`/`tan`/`atan`/`atan2`/`exp`/`log`/`pow`/`hypot`，均为 `cross_platform`（§106.5）——与 `Vec2F32`/`Vec3F32` 值类型：`new`、Property `x`/`y`/`z`、`add`/`sub`/`scale`/`length`，单精度 IEEE 运算，`cross_platform`）、`viso::time`（`Stopwatch`）与 `viso::clipboard`（`write_text` 为 Action；`read_text` 为 Task，`await` 平台的应答，读不到或被拒为 `None`）；扩展 crate 携带的 Library 见 §47.3。

- 每个函数记录：名称、`fn`/`action`/`task` 分类、参数与返回的 Schema 类型、所需 Capability、线程域（`any`/`ui`/`worker`）、`deterministic`、`realtime_safe`、`@const`（编译期可求值，隐含 `deterministic`）与每次调用的预算成本；每个 Handle 类型记录方法、所有权（`shared`/`borrowed`）与线程域；
- Schema Enum 是带 Variant 的 Native 类型，`.vs` 把它当作其 Library 声明的普通 Enum：`import viso::game::Key;` 后写 `Key::Space`，可构造、比较、`match`（穷尽检查同用户 Enum），未知 Variant 为 `E2001`（附最近候选）；Variant 可携带字段：Rust 侧 `#[derive(NativeData)]` 的 enum（`#[native(path = "app::Route")]`），Variant 名默认取 Rust 名的 snake_case（`UserProfile` 为 `user_profile`，`#[native(name = "..")]` 改名），元组 Variant 写 `Route::message(5)`，具名字段 Variant 写 `Route::message { id: 5 }`；值的表示同用户 Enum：无 Payload Variant 为其序号，带 Payload 的为以序号为 Tag 的字段聚合；
- 每个函数声明可复现档位（`none`/`same_binary`/`cross_platform`）：`deterministic` 的函数默认 `cross_platform`，调用宿主超越函数的声明 `same_binary`，时钟、随机与 I/O 为 `none`；Presentation 函数（粒子、音效、相机抖动）与 Debug Draw 函数在 Schema 中标记；Schema Enum 与 Native 值类型（如 `Cooldown`，由 Native 构造与读取的纯数据，按内容比较）能进入 Snapshot，Handle 的对象位于解释器之外，不能进入；`Duration` 参数可声明为 Tick 时长：实参必须是编译期常量（Duration 字面量及其加减、与数字的乘除），编译器按 Package 的 Tick 频率向上取整换算为整数 Tick，非常量报 `E2501`，为负或超出 `I64` 报 `E2112`；Scheduler Hook 声明所属层（Simulation/Presentation）；
- Schema 类型含 `Result<T, E>`：Native 返回的 `Result` 与 `.vs` 的同一类型，可 `match` 与 `?`；需要报告失败的领域 Native（如 `api::profile(id) -> Result<Profile, String>`）以它返回，不以 Fault 表达可预期的失败；
- Native Handle 类型可以是泛型：Schema 的 `NativeType::params` 声明类型参数，方法签名以 `SchemaTy::Param(i)` 指代接收者的第 `i` 个类型实参；`.vs` 写 `Navigator<Route>`，方法按实参代入类型检查，值以未类型化的 Value 越过边界；
- Native Record 是带字段的 Native 值类型：Rust 侧 `#[derive(NativeData)]` 的 struct（`#[native(path = "api::Profile")]`），各字段为 Schema 类型，字段随 Schema 携带；`.vs` 以 `profile.name`、`profile.best?.name` 读字段，Lower 为聚合字段读取而非 Native 调用；未声明的字段为 `E2001`（附最近候选）；Native Record 由 Native 构造与接收，`.vs` 不以字面量构造；
- Property 是只有 Receiver 参数的方法，写 `frame.input`，不加括号；对 Property 加 `()` 报 `E2103`，读取未声明的 Property 报 `E2001`；
- 调用经 Import 或完整路径解析：`import viso::text;` 后写 `text::upper(s)`，`import viso::math::{clamp};` 后写 `clamp(x, 0.0, 1.0)`，`import viso::time::Stopwatch;` 后写 `Stopwatch::start()`；Handle 方法写 `watch.elapsed_ms()`，Receiver 作为第一个参数。未注册的路径或方法为 `E2001`；
- Effect 分类：`deterministic` 的 `fn` 为 Pure，其余 `fn` 为 Read，`action` 为 Action，`task` 为 Task；在 View/Computed 中调用 `action` 为 `E2502`；
- Capability 推断：调用所在的 Callable 直接需要该函数的 Capability，并沿调用图传递（§95）；
- `E6102`：`worker` 线程域的函数只能在 `task` Body 中调用；`borrowed` Handle 只在接收它的那次调用内有效，不得存入 `state`、`input`、`computed`、Event Payload、`const`、Record 字段或作为返回值，只能作为参数传递或保存在局部变量；
- 编译产物记录每个 Native Import 的路径、参数个数与签名哈希；运行前 `link` 到 Registry：路径未注册或签名不同为 `E6101`，不链接任何 Import；未链接即调用为 `E7105`；
- Native 返回错误或 Panic 为 `E7106` Runtime Fault 并回滚当前 Transaction；Native 已产生的外部副作用（如写剪贴板）不回滚；
- `viso schema` 以 §139 对象输出 Native Library、函数与 Handle 类型的 Schema。

### 47.2 手写 Native 声明

- 声明的路径是所在模块的路径：Package `app` 的模块 `util` 中 `native fn hash(..)` 声明 `app::util::hash`，写在 `component Board` 或 `system Board` 内则为 `app::util::Board::hash`，只在该 Component/System 的成员中可见；顶层声明在本模块可见，其他模块以完整 Native 路径 `import app::util::{hash};`；
- 返回类型省略即 `()`；参数与返回类型必须是 Schema 类型：`Bool`、`I64`、`F32`、`F64`、`Duration`、`String`、`()`、`List<T>`、`Option<T>`、`Result<T, E>`、已注册的 Native 类型或同处声明的 `native type`；参数不带默认值；声明不带泛型参数、`where` 子句，`native type` 不带 Trait Bound（Native Schema 无泛型与 Trait 实现），违者 `E6101`；`requires { a::b }` 声明 Capability `a.b`；
- 编译所用 Registry 已注册声明所在的 Library 时立即检查：路径未注册、`fn`/`action`/`task` 分类或签名哈希不同（参数名不计入签名）、`requires` 与 Schema 的 Capability 集合不同均为 `E6101`，调用使用已注册的 Schema；
- 未注册该 Library 时，声明本身即其 Schema：调用按声明的类型、Effect 分类与 Capability 检查并 Lower 为该路径的 Native Import（`native fn` 为 Read，不确定、UI 线程）；运行前 `link` 到实现它的 Rust Library，路径缺失或签名不同为 `E6101`；
- 同一作用域内重名（含与 `fn`/`action`/`task`/`const` 或成员重名）为 `E2002`；以内建类型名（`Unit`、`String`、`List` 等）声明类型也是 `E2002`，因为类型标注中这些名字总是指内建类型。

### 47.3 Native Package

扩展 crate 携带的 Native Library 不经手写声明（ADR 0051）：Cargo 包在 manifest 中声明它们，App crate 直接依赖它即可在 `.vs` 中 `import`，不写注册代码。

```toml
[package.metadata.viso.natives]
schema = "natives.json"   # Library 的 Schema，与 manifest 同目录
library = "NATIVES"       # crate 中的 `&'static [&'static NativeLibrary]`
feature = "dsl"           # 构建它们的 feature，可省略
widgets = "WIDGETS"       # crate 中 Rust Widget 的 `&'static [WidgetImpl]`，没有 Rust Widget 时省略
```

- Schema 文件是 `viso-natives` JSON（版本 1）：除实现外的每个函数与类型字段；包自身的测试以 `viso::native::check(NATIVES, "natives.json")` 比较，不一致即失败，设 `VISO_WRITE_NATIVES` 时改为写出；Library 的 Trait 与 Rust Widget 同样写入（`traits`、`widgets` 两个键，没有时省略）；
- 只发现 App crate 的直接 normal 依赖（`cargo metadata`；声明了 `feature` 的须已启用），每个 manifest 与 lock 文件在进程内只发现一次；两个 Library 声明同一路径为 `E6101`；
- `view!` 与 `component!` 先按标准 Registry 编译，出现 `E2001` 时才发现并按标准 Library 加上这些包的 Library 重新编译，不依赖 Native Package 的 App 不运行 `cargo metadata`；展开把模块调用到的包的 Rust Library（如 `::app_api::NATIVES`）交给 View 的 Host 链接，View 启动的 Game 同样链接，Release View Package 以 `ViewLoad::libraries` 交入；
- `viso check`、`viso export`、`viso run` 的 dev 编译与 LSP 用同一规则（源文件所属 crate 为 `Viso.toml` 旁或文件之上最近的 `Cargo.toml`）；`viso schema` 预先发现，列出项目可调用的全部 Native；发现失败时作为 note 附在每个保留的 `E2001` 上；
- **Rust Widget**：Library 的 `widgets` 中的 `NativeWidget::rust(name)` 是一个用 Rust 实现的 Widget，Schema 只有它自己的属性与事件，Native Package 只能携带这种 Widget（没有 Slot、属性组与冒泡事件）。`.vs` 像写其他节点一样写它：自己的属性按 Schema 定型（`E2103`），此外接受所有节点都有的布局、变换、焦点、`semantics.*`、`transition.*`、外观与标准事件，不接受子节点。它 Lower 为一个盒节点（Stack），自己的属性成为求值入口，自己事件的处理器按事件在 Schema 中的位置登记，不经事件路由，因此不能写 `capture`。实现是 `viso::native::RustWidget`：节点第一次显示时 `build` 在盒内建出内容，之后每个自有属性的值变化时 `set` 收到新值（相等的值不重复交付，热重载保留已交付的值与 Widget 实例）；它从自己挂上的处理器里以 `WidgetEvents::fire(cells, event, payload)` 报告事件，运行 View 对该事件的处理器。双向属性 `p` 由事件 `p_changed`（主属性也可以是 `changed`）写回，载荷就是新值，`bind p <=> state` 直接写入；包以 `pub static WIDGETS: &[WidgetImpl]` 提供实现，测试以 `viso::native::check_widgets(NATIVES, WIDGETS)` 核对与声明一一对应。展开把 View 用到的 Rust Widget 所在包的 `WIDGETS` 交给 Host，Release View Package 以 `ViewLoad::widgets` 交入；Host 没有链接到某个 Widget 的实现时，该节点不建内容并记录一次 Fault。HTML 导出不支持 Rust Widget；
- Dev Session 的 Patch 只能链接 Live View 的 Host 已链接的 Library：首次调用构建未链接的包需要重新构建；
- Capability 照旧：函数声明自己的 Capability，`Viso.toml` 的 `[package] capabilities` 授予，授权随模块到达 Host，授权之外的调用为 `E6103` Fault；`view!` 与 `component!` 以 `Viso.toml` 为编译依赖，改动授权即重新编译。
- **平台可用性**：函数默认在每个平台存在；只在部分平台实现的函数以 `.only_on(&["ios", "android"])` 声明（名称取自 `viso::native::TARGET_PLATFORMS`：`macos`、`windows`、`linux`、`ios`、`android`、`web`），Schema 以 `targets` 写出（为空时省略），未知平台名在写 Schema 时被拒绝。按某个平台检查或构建时调用它不存在的函数为 `E6104`，指向调用并列出它存在的平台：`viso check <target>` 与 `viso build <target>` 按 target 的平台检查（host 为本机系统，`ios`、`android`，`web-*` 为 `web`，embedded device 为 `linux`，`headless` 与不写 target 时不检查）；`view!`/`component!` 为导入的每个受限函数生成按 `cfg` 守卫的 `compile_error!`，crate 为其他平台构建时以 `E6104` 失败。

- 应用自己的 Native 放在其 workspace 中单独的 crate（如 `app-api`），与扩展 crate 一样是 Native Package：编译 View 的 crate 无法为自己生成它所依赖的 Schema。它只依赖 `viso`：`viso::native` 提供 `native!`、`NativeLibrary` 等写 Library 所需的类型与 `check`；返回的记录是 `#[derive(NativeData)]` 的 struct，可预期的失败以 `Result` 返回，等待网络的函数写作 `native!(task "profile" async |cx, id: i64| -> Result<Profile, String> { .. })`：Body 从 `cx` 取所需服务并返回 Future，其值即调用的值，丢弃即取消；不带默认 feature 依赖 `viso`，以免替 App 选 feature，测试另以 dev-dependency 带默认 feature：

```toml
[dependencies]
viso = { version = "1", default-features = false }

[dev-dependencies]
viso = "1"
```

网络不是 DSL Library：框架与扩展都不向 `.vs` 提供通用 HTTP。`.vs` 调用应用的领域 Native，URL、鉴权、重试、解码与错误分类在其 Rust 实现中（用 `viso-ext/viso-net`），函数声明 `network.http`，由 `Viso.toml` 授予：

```viso
import api::{profile, Profile}

task load_profile(id: I64) -> String {
    return match await profile(id) {
        Result::Ok(found) => found.name
        Result::Err(why) => why
    }
}
```

---

# 第七部分：完整语法——View、节点、Style 与 Theme

## 48. View 声明

```ebnf
view_decl            = "view", view_block ;

view_block           = "{", { view_structure_item }, "}" ;

view_structure_item  = { attribute },
                       ( named_node
                       | anonymous_node
                       | slot_item
                       | view_if
                       | view_for
                       | view_match ) ;
```

规则：

- 每个 Component 必须有且仅有一个 View；
- `view_block` 只包含结构项；Property Binding、`bind`、`on` 与 Slot 填充只出现在 Node Body（§49）；
- View 根 `view_block` 必须生成恰好一个根 Node；其他 `view_block`（Slot 填充与默认内容、分支、`for` Body、Match Arm）的 Cardinality 由所在 Slot 决定（§45.1）；
- 多个根节点必须显式包裹 `Fragment`。`Fragment` 是标准 Component，只有一个默认 Slot，不产生自身 Layout/Paint 节点，子节点直接参与父节点布局；
- View 是纯执行域；
- View 中禁止普通 `let`、赋值、`return`、`emit`、`start`、I/O 和 Native Action；
- View 可以读取 Input、State、Computed、Resource State 和当前 typed `env`；
- `env` 只在 View 执行域注入，普通 State/Computed 初始化器不得隐式依赖局部 Layout Environment；
- View 可以调用纯 `fn`；
- View 构建产生 UI IR，不直接执行 OS/GPU 副作用。

---

## 49. 子节点语法：删除 `child`

```ebnf
named_node           = "node", identifier, ":", node_head ;

anonymous_node       = node_head ;

node_head            = component_type, [ primary_argument ], [ node_body ] ;

primary_argument     = "(", expression, ")" ;

component_type       = type_path ;

node_body            = "{", [ node_member, { item_end, node_member } ], [ ";" ], "}" ;

node_member          = { attribute },
                       ( property_binding
                       | two_way_binding
                       | event_handler
                       | slot_fill
                       | named_node
                       | anonymous_node
                       | slot_item
                       | view_if
                       | view_for
                       | view_match ) ;
```

唯一规则：

```viso
node root: Column {
    Text("Hello")
    Spinner
}
```

解释：

- `node root: Column` 创建具有显式局部名称和稳定身份种子的节点；
- `Text` 和 `Spinner` 是匿名子节点；没有 Property 与子项的节点省略 `{}`；
- 语言不存在 `child Column {}`；
- `child` 是保留的迁移错误词，出现时诊断 `E3001`；
- 匿名节点仍具有编译器生成的 Structural Node ID，但源码插入兄弟节点可能改变它；
- 需要跨热重载可靠保留局部状态、焦点或动画的节点应该使用 `node` 或 `@stable(...)`。

### 49.1 Named Node 可见性

- Named Node 只在当前 Component 实现中可见；
- Named Node 不是公开字段；
- `node name: T { ... }` 在当前 Component 的 Action、Task、Effect 与 Event Handler 作用域中引入不可变绑定 `name: NodeRef<T>`；View、Computed、Style 与 Input 默认值中不可见，出现时报 `E2001`；
- 读取 NodeRef 不建立 Reactive 依赖；
- Node Action 返回 `()`，在本次事务的写入使视图结构落定之后生效，因此同一 Action 刚挂载的节点可被触达；目标节点未挂载（分支未激活、`preserve` 分支切走或已销毁）时为 No-op，而不是 Panic；
- `for` 项中的组件实例各自触达自身的 Named Node；
- `view_for` Body 内的 Named Node 不引入 NodeRef 绑定，其名称只作为 Keyed 身份种子；
- View 纯度规则禁止在 View 外直接修改节点属性作为状态源；
- 命令式焦点、滚动、测量等操作通过受控 Node Action 完成。

---

## 50. Property Binding

```ebnf
property_binding     = property_path, ":", expression, item_end ;

property_path        = label, { ".", label } ;
```

在 Node Body 中：

```viso
Text(label) {
    layout.width: 120dp
    color: theme.colors.foreground
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
- Style 与显式 Property 的优先级在 §59 定义；
- 期望类型是 Enum 时，右侧可以只写 Variant 名（`align: center`、`width: fill`），规则见 U1.3。

### 50.1 主属性

Widget Schema 或用户 Component 至多把一个单向 Property 标为主属性（Component 中写作 `@primary input text: String`）。主属性写在节点类型之后的括号中：

```viso
Text("Hello")
Text(label) { color: theme.colors.muted }
Button("Save") { on click { save() } }
Image(asset("logo.png"))
```

- `T(expr)` 与在 Node Body 中写主属性的 Property Binding 语义相同：单向 Reactive Binding，失效类别、诊断与 Hot Reload 身份不变；
- 主属性只能这样写：在 Node Body 中以 `name: expr` 绑定主属性报 `E3109`（附改写为括号形式的 Fix）；
- 没有主属性的类型写括号实参报 `E3101`；
- 实参与类型写在同一行，Node Body 的 `{` 同样如此（§14）；既无实参也无 Body 的节点省略 `{}`（`Spinner`）；
- 主属性不能是 Two-way Property，也不是 Event；
- 标准 Widget 的主属性：`Text.text`、`Button.text`、`LinkLabel.text`、`Toggle.label`、`CheckBox.label`、`Image.source`、`Svg.source`、`Video.source`（以 `viso::widgets` Schema 的 `primary` 标记为准）。

---

## 51. Two-way Binding

```ebnf
two_way_binding     = "bind", property_path, "<=>", assignable_path,
                      [ "using", type_path ], item_end ;

assignable_path      = identifier, { ".", label | index_selector } ;
index_selector       = "[", expression, "]" ;
```

```viso
TextInput {
    bind value <=> draft
}
```

带转换器：

```viso
Slider {
    bind value <=> settings.volume using PercentUnitConverter
}
```

规则：

- 左侧 Property 必须在 Schema 中声明 `two_way`；
- 右侧必须是可赋值的 State Lens：以当前 Component 的 `state` 为根，经 `.label`/`[index]` 到达其字段或元素；
- 右侧不能是 Input、Computed、Const、局部绑定、Resource Payload 临时值或普通函数返回值，否则 `E3107`；
- 无 Converter 时两边类型必须相同（`E2103`），不做 §U1.3 的值提升或数值加宽；
- Converter 必须实现 `TwoWayConverter<Model, View>`（否则 `E2201`）：

```viso
trait TwoWayConverter<Model, View> {
    fn to_view(model: Model) -> View
    fn to_model(view: View) -> Option<Model>
}
```

  读取时 Property 取 `to_view(model)`；写回时 `to_model(view)` 返回 `None` 则拒绝本次写回，State 保持不变；
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
        increment()
    }
}

Canvas {
    on pointer_down(event) {
        if event.button == PointerButton::primary {
            begin_drag(event.position)
        }
    }
}
```

规则：

- 不存在 `on click => increment()`；
- 即使只有一条语句也必须写 Block；
- Handler 的可选 Pattern 绑定整个 Event Payload，且必须不可反驳（`E2303`）；自定义 Component Event 的 Payload 是以其参数为字段的 Record，不带 Path 的 Record Pattern 直接取出参数：`on opened({ id }) { open(id) }`；
- Handler 只能命名节点接受的 Event，否则报 `E3202`：用户 Component 节点接受标准事件（U7.1）与该 Component 声明的 Event；内置 Widget 接受其 Schema 声明的 Event（U3.7、U7.4、U7.5）及标准事件，不参与布局的 `FocusScope`、`KeyShortcut` 与 `Fragment` 不接受标准事件；
- 忽略 Payload 时省略括号；
- Handler 自动运行在 Action Transaction 中；
- 默认 Phase 由 Event Schema 定义，通常为 Target/Bubble；
- `capture` 和 `bubble` 显式覆盖默认 Phase；
- Event Payload 字段由 Schema 静态检查；
- Handler 中允许同步 State 修改、`emit`、Action Call 和 `start`；
- Handler 中禁止直接 `await`；
- Event 取消使用 Typed API：`event.stop_propagation()`、`event.stop_immediate_propagation()` 与 `event.prevent_default()`，语义见 §90。
- Handler 在 `component!`、`view!`、Hot Reload 与 Release Package 中以同一张 Handler 表挂载：每个 Handler 是所在 Component 的一个 Behavior Chunk，节点只登记 `(Event 路由, Handler 下标, 是否 capture)`，State 写入在 Transaction 结束时回写 UI State Cell；
- 一次输入样本对每个节点只运行一次 Handler：未写 `capture` 的 Handler 在 Target 与 Bubble 段运行，祖先节点上的 Handler 不因 Capture 段再运行一次；写 `capture` 的 Handler 在 Capture 段（自根向 Target，先于 Target 上的 Handler）与 Target 段运行，不在 Bubble 段运行；控件的内建响应仍只在其自身节点投递；
- 内置控件（`Toggle`/`CheckBox`、`Slider`、`Tabs`/`RadioGroup`、`TextInput`）的内建响应随同一张 Handler 表挂载：控件从 Handler 表中的纯表达式条目读取当前值与范围（`checked`、`value`、`min`、`max`、`step`、`selected`），据样本算出新值后投递 Schema 声明的 `changed`、`selected_changed` 或 `submitted`；
- 节点显示的反应式属性值同样是 Handler 表中的纯表达式条目：`Text`/`Button` 的 `text`、`TextInput` 的 `value`、控件的 `checked`、`value`、`selected`、`min`、`max`，以及任一原生节点外观的 `background`（填充色，`None` 为无填充）与 `opacity`（节点及其子树作为一层合成，按节点自身盒合成，`0` 不绘制；只标记 `PAINT`），`translate`（`Offset` 的 `px`/`sp`/`em` 项随环境重新折叠，只标记 `TRANSFORM`），及 `transition.background`、`transition.opacity`、`transition.translate`、`transition.corner_radius`、`transition.width`/`height`（只在两端都是 `fixed` 时插值）与文本的 `transition.color`（只在所移动的值变化时求值，不是依赖，见 §U6.1）。节点挂载时求值一次；之后只有其条目（沿调用传递）所读 State 在该帧变化时才重新求值，与节点当前显示值相等的结果不投递；`Text`/`Button` 的值成为节点文本，`TextInput` 的值播种其编辑缓冲（缓冲已持有相同文本时不重置光标，播种不算编辑、不投递 `changed`）；控件的值与范围成为节点的语义状态：`Toggle`/`CheckBox` 的 `checked`，`Slider` 的 `value` 与 `min`..`max` 范围，`Tabs`/`RadioGroup` 第 `selected` 个子节点为选中、其余未选中；未声明 `value` 的 `TextInput` 不被播种，重新挂载不覆盖用户已输入的文本；`for` 与条件区域内的节点在其片段挂载时求值，之后按同一规则随其所读 State 与所绑定条目重新求值；`component!`、`view!`、Hot Reload 提交与 Release Package 以同一规则投递，保持树结构的 Hot Reload 替换视图原有的求值挂钩而不累加；条目求值 Fault 保留节点原值并记录在宿主上；
- Runtime 尚未投递的标准事件（如 `scroll`、`focus`）或 Behavior 无法 Lower 的 Handler 体报 `E3711`；Hot Reload 中出现 `E3711` 时保留 Last-good Handler；
- Handler 运行期 Fault 只中止该次调用、不回写任何 State，Fault 记录在宿主上。

`=>` 仅保留给 `match` Arm，因此 Parser 和 Formatter 不会把事件写法分叉成两套。

---

## 53. 填充 Slot

```ebnf
slot_fill            = identifier, view_block ;
```

```viso-pending
Dialog {
    title: "Delete item"
    Text("This cannot be undone.")
    actions {
        Button("Cancel")
        Button("Delete")
    }
}
```

规则：

- 放置用户 Component 的 Node Body 中，`identifier { .. }` 在该名字是目标的具名 Slot 时填充该 Slot；Slot 名是 `snake_case`，节点类型是 `UpperCamelCase`，二者不会混淆；名字不是目标的 Slot 也不是可见的类型时报 `E3501`；
- 不在填充内的裸结构项进入默认 Slot；目标没有默认 Slot 时报 `E3003`；
- 同一 Node Body 对同一具名 Slot 填充两次报 `E3502`；
- 填充的 View Block 属于调用方：读取调用方的 State，Handler 在调用方的执行域中运行；
- Native Widget 只有默认 Slot。

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
    UserPanel { user: user }
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
- 缓存容量和逐出策略由 Runtime Profile 控制；默认 Profile 每个分支缓存最近一个实例；视图的 Preserve Budget（`ViewHost::set_preserve_budget`）限定同时切走保留的分支数（含嵌套），一次切换后超出时按离开先后逐出最早离开的分支；平台内存警告（`LowMemory`）逐出视图全部保留分支；被逐出的分支释放其 Node、State、Effect、Task，再次进入时重新挂载（新 `NodeId`，局部状态从初值开始）；Hot Reload 保留 Budget；
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
        item: item
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
                      { ( "," | NEWLINE ), view_match_arm }, [ "," ],
                      "}" ;

view_match_arm      = pattern, [ "if", expression ],
                      "=>", view_block ;
```

```viso
match user.state {
    UserState::loading => {
        Spinner {}
    }
    UserState::ready(user) => {
        ProfileCard { user: user }
    }
    UserState::error(error) => {
        ErrorView { message: error.message }
    }
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
- View 的根必须是 Node，根上的控制流区域报 `E3711`；`VirtualList` 的 Body 是其 Item Template（U9.1），另一 `VirtualList` 行内的 `VirtualList` 尚不挂载，报 `E3711`；
- Hot Reload 中出现 `E3711` 时保留 Last-good 区域；含控制流区域的 View 在 Hot Reload 中整树重建，按名称保留 State。

---

## 57.–58.（已删除）

`part` 与 `template` 已从语言中删除：Template 即没有 State 的 Component，直接写 `component`；需要调用方定制的内部值应成为 Input，需要整体替换的子树应成为带默认内容的 Slot（§45）。

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
    background: theme.colors.primary
    foreground: theme.colors.on_primary

    when hover {
        background: theme.colors.primary_hover
    }

    when disabled {
        opacity: 0.5f32
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
    styles: [PrimaryButton, DenseControl]
}
```

`styles` 的精确类型由 Component Schema 定义，通常是 `List<StyleRef<Button>>`。Style 名在 Expression 中求值为编译期常量 `StyleRef<T>`，`T` 是其 `for` 目标 Component。编译器必须检查 Style 目标类型兼容性；Style 顺序按列表从左到右应用，后者覆盖前者，节点显式 Property 最后覆盖全部 Style。不存在通过字符串名称动态查找 Style 的语义。

---

## 60. Theme

```ebnf
theme_decl          = "theme", identifier,
                      [ ":", type_path ],
                      "{", { theme_item }, "}" ;

theme_item          = identifier, "=", expression, item_end ;
```

Theme Item 只给出 `Theme` 字段的值；Theme 用到的命名常量写成模块级 `const`（Item 表达式可以读取），不在 Theme 内另设 `const` 作用域。

```viso
export theme AppTheme {
    colors = ColorPalette {
        primary: #4f7cff,
        foreground: #f4f6fb,
        surface: #151923,
    }

    spacing = SpacingScale {
        small: 4dp,
        medium: 8dp,
        large: 16dp,
    }
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
block                = "{", [ statement, { item_end, statement } ], [ ";" ], "}" ;

item_end             = ";" | NEWLINE ;
```

`NEWLINE` 是 §14 中结束条目的换行，不是独立 Token：词法上换行仍是空白，Parser 依据 §14 判断它是否结束当前条目。

解析规则：

- Block 的最后一个条目若是 Expression Statement，它是该 Block 的 Tail Expression，无论其后是否写了 `;`；
- 期望类型为 `Unit` 的 Block 丢弃 Tail 的值；否则 Tail 的类型即 Block 的类型；
- `view_block`、`node_body`、`style`、`theme` 和 Resource 配置 Block 不使用普通 Block Grammar，因此没有 Tail Expression；
- 返回类型为 `Unit` 的 Callable 可以省略 Tail Expression；
- 同时出现显式 `return` 和 Tail Expression 是合法的，但控制流必须通过类型检查；
- 在 Block Item 起始位置，未加括号的 `if` 和 `match` 总是由 Statement Parser 接管；
- 因此未加括号的 `if`/`match` 不会被当成更大 Expression 的开头：`if a { 1 } else { 2 } - 1` 是一个 `IfStatement` 加一个独立的 Tail `- 1`，`match x { _ => v }.f()` 中的 `.f()` 是 `E1403`；要以它们开头组成更大的 Expression，必须加括号或使用显式 `return`；
- Block 需要值时，最后一项若是未加括号的 `IfStatement`/`MatchStatement`，它提供 Block 的值，各分支的 Tail 统一为该类型（`if` 须有 `else`）；
- 这一规则只解决 CST 分类，不改变 `if_expression`/`match_expression` 在赋值、参数和返回值位置的能力。

```viso
fn classify(value: I64) -> String {
    return match value {
        0 => "zero"
        _ => "other"
    }
}

fn square(x: I64) -> I64 {
    x * x
}
```

等价于：

```viso
fn square(x: I64) -> I64 {
    return x * x
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
                       "=", expression ;

assignment_statement = assignable_path, assignment_operator,
                       expression ;

assignment_operator  = "=" | "+=" | "-=" | "*=" | "/=" | "%="
                     | "&=" | "|=" | "^=" | "<<=" | ">>=" ;

expression_statement = expression ;

return_statement     = "return", [ expression ] ;

break_statement      = "break", [ expression ] ;

continue_statement   = "continue" ;

emit_statement       = "emit", identifier,
                       "(", argument_list, ")" ;

transaction_statement = "transaction", block ;

if_statement         = "if", head_expression, block,
                       [ "else", ( if_statement | block ) ] ;

match_statement      = match_expression ;

while_statement      = "while", head_expression, block ;

for_statement        = "for", pattern, "in", head_expression, block ;

loop_statement       = "loop", block ;
```

### 62.1 Statement 约束

- `let` Pattern、`for` Pattern（Behavior 与 View）和 Closure 参数 Pattern 必须是 Irrefutable Pattern（§70.1），否则报 `E2303`；
- Statement Attribute 必须在 Schema 中声明可作用于对应 Statement Kind；例如 Shader 循环可使用 `@max_iterations(64)`，但同一 Attribute 作用于普通 `let` 时必须报错；
- Statement Attribute 只产生 HIR 元数据，禁止改变 Tokenization、优先级或基础控制流语义；
- Assignment 不是 Expression，禁止 `a = b = c`；
- 赋值目标的根必须是 `state` 或 `let mut`/`mut` 绑定的局部名，经 `.label`/`[index]` 到达其字段或元素；以 Input、Computed、Const、Callable、`env`、不可变局部绑定（含参数与 Pattern 绑定）为根，或根不是名字（调用结果、`?.`、`?` 等），报 `E2110`；
- `return` 只允许在 Callable/Closure 中；
- `break value` 只允许从 `loop` 返回值；
- `continue` 只允许在 Behavior Loop 中；
- 违反以上三条（`return`/`break`/`continue` 出现在不允许的位置，或 `break value` 不在 `loop` 中）报 `E2803`；
- Behavior `for` 没有 `key`；`key` 只属于 View `for`；
- `transaction` 在 Action/Event Handler 中嵌套复用当前事务；在 Effect/Task Completion 中显式创建 UI 事务；
- 普通 `fn` 中禁止 `transaction`；
- Statement 之间以 `item_end`（§14）分隔，Statement 自身不含 `;`。

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
0 < x && x < 10
a == b && b == c
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

Viso 没有隐式 Nullable Reference，因此 `??` 只适用于实现标准 `Coalesce` Trait 的类型；MVP 只内建 `Option<T>`。左侧为已知的非 `Option` 类型时报 `E2103`。

### 64.2 Head Expression 消歧

```ebnf
head_expression = expression ;
```

`head_expression` 与普通 Expression 的类型规则完全相同，但 Parser 在下列紧邻结构 Block 的位置禁止最外层出现未加括号的 Record Expression：

- Behavior `if`、`while`、`for` 和 `match` 的 Header；
- View `if`、`for ... in`、`for ... key` 和 `match` 的 Header。

因此：

```viso
if ready {} // 合法
if (Point { x: 1.0, y: 2.0 }) {} // 可解析，随后通常因条件不是 Bool 报类型错
for item in (Query { limit: 10 }.run()) key item.id {}
```

未加括号的 `Type { ... }` 在 Header 中不会吞掉控制流 Block。该限制属于 Parser Mode，不改变括号内 Expression Grammar。

---

## 65. Primary Expression

```ebnf
primary_expression   = literal
                     | interpolated_string
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

interpolated_string  = string_start, [ expression ],
                       { string_middle, [ expression ] },
                       string_end ;

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
                        { ( "," | NEWLINE ), record_initializer }, [ "," ] ],
                      "}" ;

record_initializer  = label, ":", expression
                    | identifier
                    | "..", expression ;

block_expression    = block ;
```

泛型 Record Constructor 必须使用 Turbofish，例如：

```viso
Pair::< String, I64 > { first: "age", second: 42 }
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
    "{count} items"
}
```

所有可达分支的 Tail Expression 必须统一为一个类型。

Statement 位置可以使用 §62 的无 `else` If Statement。

---

## 68. Match Expression

```ebnf
match_expression     = "match", head_expression, "{",
                       match_arm,
                       { ( "," | NEWLINE ), match_arm }, [ "," ],
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
let doubled = items.map(|item| item.value * 2)

let formatter = |value: I64| -> String {
    "Value: {value}"
}

start scheduler.after(250ms, move || {
    log::info("timer completed")
})
```

### 69.1 Closure 推断

- 参数 Pattern 必须是 Irrefutable Pattern（§70.1）；
- 参数类型可以由期望的 Function Type 推断；
- 当前实现：`Fn(..) -> R` 与 `FnMut(..) -> R` 降为函数类型，未标注的参数与返回类型取自期望类型；参数个数不符报一个 `E2103`（不再逐个报 `E2401`）；`ActionFn`/`TaskFn` 尚未降为类型（按未知类型处理）；
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

constructor_pattern        = type_path, constructor_pattern_payload
                           | "{",
                             [ record_pattern_field,
                               { ",", record_pattern_field }, [ "," ] ],
                             "}" ;

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

不带 Path 的 Record Pattern `{ x, y: b, .. }` 匹配其被匹配类型的字段：该类型须是 Record 或 Event Payload，否则报 `E2103`；未知字段报 `E2001`。它在 `let`、`for`、Closure 参数与 Handler 中不可反驳，`match` 中按其子 Pattern 判断。

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
        log_error(error)
    }
    _ => {}
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
let mut total: I64 = 0
for item in items {
    total += item.value
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
let a: I32 = 1 // 字面量直接定型为 I32
let b: F32 = 1.0 // 字面量直接定型为 F32
let c = 1 // Host 默认 I64
let d = 1.0 // Host 默认 F64
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
let x: F32 = value as F32
let count: U32 = checked_cast(value)?
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
- 当前实现：Generic Callable 的体以不透明参数类型检查一次，再按每组实参单态化；实参由显式 `::<..>`、期望类型或参数推断，无法推断报 `E2103`；实例化链深度超过 32、实例总数超过 4096 或实参类型超过 256 个节点（无界递归实例化）报 `E2203`；Generic Component 在各使用处共享一次 Lower；
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

`dyn Trait` 的值携带按方法列表排序的 Vtable，调用经 Vtable 槽分派，无字符串查找。具体类型只在期望类型为 `dyn Trait` 处擦除，未实现该 Trait 报 `E2201`；Vtable 中的 Method 不得有自身的类型参数，签名中除 Receiver 外不得出现 `Self`，否则该 Trait 不能用作 `dyn`（`E2201`）。

---

## 80. `StableKey` 正式定义

```viso
export trait StableKey: Eq + Hash + Clone {
    fn stable_hash(self) -> U64
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
    value: U64
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

写入 `state`（赋值目标的根是 `state`）与 `emit` 同属 Mutating：只有 Action/Event 函数体与 Effect Body 的 `transaction { ... }` 可以执行，View/Computed 中报 `E2502`，其他函数体报 `E2501`；Task 通过返回值交还结果。View 中的 `on` Handler 是独立的 Event 函数体，不属于 View 的 Reactive 上下文。State 初值、Input 默认值、`const` 值与 Record 字段默认值按 `fn` 的调用权检查。

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

一帧的 State Flush 取出该帧已提交的写集合，依次调度 Computed、直接 Binding、控制流区域与节点值、语义状态投影，并把被唤醒的 Effect 排入队列（Effect 不在 flush 内运行，见 §86 与 Architecture §11.2）；调度中产生的新写入（如区域挂载写入实例 State）在同一帧再调度一轮，直到没有待处理写入。连续 16 轮后仍有待处理写入即 Reactive Cycle：Runtime 停止本帧调度，报告 `E4202`（列出被丢弃的单元，以及它们会再次唤醒的 Effect 所在的节点），丢弃尚未调度的变化（已写入的值保留，只是其下游不再运行），下一帧不继承这些变化。

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
transform: invalidates TRANSFORM
```

运行时规则：

- 未声明 `invalidates` 的 Property 是 Schema 错误；
- Paint-only 修改不得触发 `MEASURE`/`LAYOUT`，也不得使祖先失效；
- Transform-only 修改只产生 `TRANSFORM`，不得触发 `MEASURE`/`LAYOUT`/`PAINT`/`HIT_TEST`（节点首次成为变换根时额外绘制一次以记录根）；
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

状态转换表是封闭的，按旧类型与新类型的声明比较（不比较源码行号，也不按名字猜），对活值逐层应用：

| 旧类型 → 新类型                                  | 规则                                                                     |
| ------------------------------------------------ | ------------------------------------------------------------------------ |
| `T → T`（同一类型，Record/Enum 声明逐字段相同）    | 保留原值                                                                 |
| 整数 → 更宽的整数（`I32 → I64`、`U8 → I16` …）     | 保留原值                                                                 |
| 整数 → 其他整数类型                               | 新类型容纳活值时转换，否则不兼容                                           |
| 整数 → `F32` / `F64`                              | 浮点能精确表示活值时转换（`I64` 在 `F64` 下为 ±2^53 内），否则不兼容        |
| `F32 → F64`                                       | 保留原值                                                                 |
| `T → Option<U>`（`T` 不是 `Option`/`Unit`）       | `T → U` 转换后成为 `Some`                                                |
| `Option<T> → Option<U>`、`List<T> → List<U>`      | `None` 保持；每个元素按 `T → U` 转换                                     |
| Tuple、Range、`Result<T, E>`                      | 按位置 / `Ok`、`Err` 负载逐个转换，元数须相同                              |
| 同一 Record 声明                                  | 按字段名保留并转换；删除的字段丢弃；新增字段取其默认值，无默认值则不兼容    |
| 同一 Enum 声明                                    | 按 Variant 名映射，负载按位置（tuple）或字段名（record）转换；活值所在 Variant 被删除或负载不可转换时不兼容 |
| 函数类型、其他组合                                | 不兼容                                                                   |

Record 与 Enum 可以递归：递归引用按同一份转换。新增字段的默认值由新代码计算；计算失败视为不兼容。

不兼容时（静态不可转换，或活值不被新类型容纳）：查找 `@migrate` 函数；没有则状态回到新 Initializer，reload 照常提交，并产生 `E5101` 警告（状态名、旧类型、新类型，指向新声明）。

`@migrate(from: "T")` 标记一个模块级或 Component 成员 `fn`，把旧类型拼写为 `T` 的状态带入该函数的返回类型：

```viso
@migrate(from: "I64")
fn positive(old: I64) -> Bool { old > 0 }
```

- `from` 是旧类型的源码拼写（与诊断中的类型拼写一致，如 `"I64"`、`"Point"`、`"List<I64>"`）；
- 函数恰好一个参数，类型与 `from` 一致，并显式声明返回类型；
- 状态的旧类型拼写为 `from`、新类型与返回类型完全一致时选中该函数；活值先按转换表转换为参数类型（同名 Record 新增带默认值字段等），再以新代码调用该函数，返回值成为新活值；
- 同一 `from` 与返回类型只能有一个 `@migrate` 函数；
- 函数执行失败（Fault）时状态回到新 Initializer，`E5101` 警告附带 Fault 信息；
- 用错 `@migrate`（标记的不是 `fn`、缺少或错写 `from`、参数个数或类型不符、未声明返回类型、重复）报 `E3712`。同一视图中两个状态持有同一 Stable ID 时整个 reload 被拒绝（`E5102`），保留 last-good。

### 94.2 Node Migration

- 同 ID、同 Type：保留局部 Widget State；
- 同 ID、兼容 Schema Version：运行 Widget Migration；
- Type 改变：Replace；
- Part/Slot Contract 改变：验证调用方后原子更新；
- 焦点、选择、滚动和动画必须由 Widget Schema 标记可迁移字段。

结构 Diff 在每个父节点内按子节点序列对齐两棵模板树：Type、`node` 名与节点种类都相同的节点对齐为保留，对齐区间内剩余的节点按顺序配对为 Replace，其子节点继续对齐；Control-flow Region 只与同一形式（`if`/`for`/`match`）、同分支数的 Region 对齐。因此在前面插入或删除无关兄弟节点不会改变其余节点的保留关系。

结构性重建时，被保留节点按其 Widget Schema 标记的字段迁移到重建它的新节点：

- `focus`：所有布局 Widget——旧节点持有焦点时新节点获得焦点；
- `scroll`：`Scroll`——非零滚动偏移在新节点首次布局后恢复（按新内容范围钳制）；
- `selection`：`TextInput`——编辑缓冲（文本、Caret/选区、IME Composition）整体迁移；
- `animation`：所有布局 Widget——进行中的 `transition.*`（§U6.1）在新节点投递其值后续上：新节点先呈现旧节点的呈现值，目标未变时沿原时钟继续，目标改变时从该呈现值按原 Timing 转向新目标；结构保持的编辑中，视图重新投递的值同样不打断进行中的转场，只在目标改变时转向；
- 被 Replace 或删除的节点的这些状态丢弃：视图内的焦点因此丢失时报告 `focus_lost`，每个丢失的非零滚动偏移计入 `scroll_lost`；
- 结构保持的编辑原地复用所有节点，上述状态不动；
- Region 挂载的节点同样迁移：旧节点与新节点由同一模板节点（经结构 Diff 保留）构建、且外层每个 `for` 的 Item Key 相同时配对；`if`/`match` 只迁移当前所选分支的节点，切走并保留（`preserve`）的分支随重建丢弃；

### 94.3 Sensitive State

`@sensitive` 标在 Component 的 `state` 上，表示它的值不得落盘：`viso run --keep-state`（CLI §11）在会话停止时写入磁盘的 Dev Snapshot 不含它。被内联的子 Component 的 `@sensitive` 状态同样排除（`for` Region 中的实例也是）。标在其他成员或 System 的状态上报 `E5105`。

```viso
component Login {
    @sensitive state token = ""
    state user = ""
}
```

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

shader_uniform       = "uniform", identifier, ":", shader_type, item_end ;

shader_instance      = "instance", identifier, ":", shader_type, item_end ;

shader_varying       = "varying", identifier, ":", shader_type, item_end ;

shader_texture       = "texture", identifier, ":", shader_texture_type, item_end ;

shader_sampler       = "sampler", identifier, ":", shader_sampler_type, item_end ;

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
    instance radius: F32
    instance color: ColorLinear

    fragment () -> Vec4F32 {
        let distance = rounded_rect_sdf(local, rect_size, radius)
        let alpha = smoothstep(1.0f32, 0.0f32, distance)
        return color.to_vec4() * alpha
    }
}
```

`rect_size` 与 `local` 是下文 Brush Profile 的 Builtin，省略的 `vertex` 是其默认 Vertex。Shader Entry 的实际 Builtin 参数、返回 Record 和可写 Varying 由 Render Profile Schema 定义。上例展示语言形态，不允许 Backend 自行改变核心表达式优先级。

Render Profile：一个 Shader 至多一个 `vertex`，恰好一个 `fragment` Entry（重复或声明 `compute` 报 `E8106`）。手写的 `vertex` 的参数只能是 Builtin `vertex_id: U32`（实例四边形的六个顶点之一，`quad_vertex(vertex_id)` 依次给出 `(0,0) (1,0) (0,1) (1,0) (1,1) (0,1)`）与 `instance_id: U32`，返回 `VertexOutput { clip_position: Vec4F32 }`；`fragment` 的参数只能是 `frag_coord: Vec4F32`，返回 `Vec4F32` 或 `ColorLinear`（颜色目标 0）。只有 `vertex` 写 Varying，Varying 是数值标量或向量（`F32` 透视插值，整数 Flat）；Instance 字段在两个阶段都可读；Uniform 与 Instance 成员不能含 `Bool`（无 Buffer Layout，报 `E8104`），Texture 与 Sampler 只能用 `texture`/`sampler` 声明。`to_clip(position, viewport)` 把左上原点的像素坐标映射到 Clip Space；`rounded_rect_sdf(p, size, radius)` 是 `p` 到原点在左上、尺寸 `size`、圆角 `radius` 的矩形的有符号距离，内部为负。

Brush Profile：绘制一个节点盒子的 Shader。每个 Shader 都有以下 Builtin，不声明即可读取，Renderer 写入它们：

| Builtin | 种类 | 含义 |
| ------- | ---- | ---- |
| `viewport_size: Vec2F32` | Uniform | 目标的像素尺寸，`to_clip` 据此映射 |
| `rect_pos: Vec2F32` | Instance | 盒子在目标中的像素位置，左上原点 |
| `rect_size: Vec2F32` | Instance | 盒子的像素尺寸 |
| `time: F32` | Uniform | 窗口打开以来的秒数（取自帧时刻，Headless 的确定性时钟下可复现，重载不归零） |
| `uv: Vec2F32` | Varying，仅默认 Vertex | 盒内位置，左上 `(0, 0)` 到右下 `(1, 1)` |
| `local: Vec2F32` | Varying，仅默认 Vertex | 盒内像素位置，`uv * rect_size` |

省略 `vertex` 时使用默认 Vertex：它按 `quad_vertex` 展开盒子的四边形，写入 `uv` 与 `local`，与下面的手写形式生成相同的代码：

```text
vertex(vertex_id: U32) -> VertexOutput {
    let unit = quad_vertex(vertex_id)
    uv = unit
    local = unit * rect_size
    return VertexOutput { clip_position: to_clip(rect_pos + unit * rect_size, viewport_size) }
}
```

`uv` 与 `local` 只在 Fragment 读取它们时才占用 Varying Location；手写 `vertex` 时它们不存在，需要的 Varying 自行声明。声明与 Builtin 同名的成员报 `E8107`。读 `time` 的 Brush 在屏幕上被画出时窗口持续出帧，盒子被裁掉、移出目标或节点消失即停，只走时钟的帧不上传任何 Instance。全部 Instance 成员都是参数，类型限于 Host 值可转换的 `F32 I32 U32 Vec2F32 Vec3F32 ColorLinear`；Brush 不声明 Uniform（Host 无从写入）。每个 `texture name: Texture2D<Vec4F32>` 也是参数：节点给出的图像，在 Shader 名所引的 Record 中排在 Instance 参数之后，取 `ImageAsset`（`asset("…")`）；`sampler` 不是参数，而是 Brush 自身的状态，所有节点相同，由成员上的 `@sampler(filter: nearest | linear, address: clamp | repeat | mirror)` 给出，缺省为双线性、钳到边缘（设置未知、取值不在其列、重复设置或标在非 `sampler` 成员上报 `E8105`）。一次绘制按声明顺序绑定节点的图像与 Brush 的 Sampler，至多 4 个 Texture、2 个 Sampler；`sample` 读到的是图像存储的预乘 RGBA，原样返回即与 `Image` 节点显示一致；图像未驻留前节点不画。无 Host 值的参数类型、声明的 Uniform、不是 `Texture2D<Vec4F32>` 的 Texture 与超出上限的 Texture/Sampler 报 `E8107`；只在有 View 用它绘制时报告（每个 Shader 一次，附第一个绘制处）。`BrushLayout` 给出 Instance Stride、盒子的两个 Offset、Uniform 大小、`viewport_size` 与（读取时）`time` 的 Offset、每个参数的名字、类型与 Offset，以及 Texture 参数与每个 Sampler 的状态：节点的参数一次编码成一个盒子为零的 Instance，参数变化原地改写其字节，绘制时复制该 Instance 并写入盒子。同一 Program 经 `program::code` 得到各 Backend 的代码：Metal、WebGPU、Direct3D 12 为源码，Vulkan 为其 WGSL 经 naga 写成的 SPIR-V（`viso-shader` 的 `spirv` Feature，仅由构建期与 Dev Host 的工具启用，运行时 Crate 不链接 naga），Headless Raster 为参考解释器。

Host 侧，Shader 名同时是其参数的 Record 类型：字段是声明的 Instance 成员，按声明顺序，`F32 I32 U32` 取同名 Host 标量，`Vec2F32`/`Vec3F32` 取 `viso::math` 的同名值类型，`ColorLinear` 取 `Color`（编码时由 sRGB 转为线性）；字段没有默认值，Record 表达式须给出全部参数（`E2103`），未知字段与 Builtin 报 `E2001`。叶子 Widget `ShaderView` 以 `shader` 属性取这样一个值，用该 Shader 绘制自己的盒子；`shader` 的值不是 Shader 参数报 `E2103`。`shader` 只脏 `PAINT`：参数变化原地改写 Instance，不重新测量或布局；`ShaderView` 没有固有尺寸，盒子由布局属性给出。

```viso
import viso::math::Vec2F32

shader Ripple {
    instance tint: ColorLinear
    instance rings: F32
    instance center: Vec2F32

    fragment () -> Vec4F32 {
        let d = distance(uv, center)
        let k = 0.5 + 0.5 * cos((d * rings - time * 0.5) * 6.2831855)
        return Vec4F32(tint.r * k, tint.g * k, tint.b * k, 1.0)
    }
}

export component Card {
    state rings: F32 = 8.0
    view {
        ShaderView {
            shader: Ripple { tint: #3366ff, rings: rings, center: Vec2F32::new(0.5, 0.5) }
            width: 120dp
            height: 80dp
        }
    }
}
```

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

# 附录 A：规范性合并 EBNF

## A.1 权威性

本附录把正文中分散的 Production 合并为单一 Parser 合同。若正文示例、说明性片段与本附录发生纯语法层面的冲突，以本附录为准；静态和运行时语义仍以正文对应章节为准。

以下终结 Token 由 Lexer 提供：

```text
IDENT
INT_LITERAL
FLOAT_LITERAL
STRING_LITERAL
STRING_START
STRING_MIDDLE
STRING_END
CHAR_LITERAL
COLOR_LITERAL
UNIT_LITERAL
DOC_COMMENT
STRICT_KEYWORD
END_OF_FILE
```

`STRING_START`、`STRING_MIDDLE`、`STRING_END` 是含洞字符串的片段（§17）：`"…{`、`}…{` 与 `}…"`。`STRICT_KEYWORD` 指 §12.1、§12.2 中任一严格关键字 Token。上下文关键字（§12.3）由 Lexer 产生 `IDENT`；带引号的上下文关键字终结符（如 `"state"`）匹配文本相同的 `IDENT`，识别位置见 §12.4。Binding/声明位置使用 `IDENT`；Label 位置使用：

```ebnf
Label
    ::= IDENT | STRICT_KEYWORD
```

条目（声明、成员、语句、`match` 分支）以 `ItemEnd` 结束。`NEWLINE` 不是 Token：它指条目最后一个 Token 与下一个 Token 之间的 Trivia 含换行，且二者不在 `( )`、`[ ]` 或 Import 列表的 `{ }` 内（Record 字面量内它分隔字段，同 `,`）；下一行以续行 Token 开头时不结束条目（§14）。以 `}` 结尾的条目、紧随其后的 `}` 与 `END_OF_FILE` 同样结束条目。

```ebnf
ItemEnd
    ::= ";" | NEWLINE
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
    ::= "import" ModulePath ImportSuffix? ItemEnd

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
     |  StyleDecl
     |  ThemeDecl
     |  ShaderDecl
     |  NativeDecl

Attribute
    ::= "@" Label ( "::" Label )* ( "(" AttributeArgs? ")" )?

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
    ::= Attribute* Label ":" Type ( "=" ConstExpression )? ItemEnd

EnumDecl
    ::= "enum" IDENT GenericParams? ImplementsClause? WhereClause?
        "{" EnumVariant* "}"

EnumVariant
    ::= Attribute* Label VariantPayload? ItemEnd

VariantPayload
    ::= "(" TypeList? ")"
     |  "{" RecordField* "}"

TraitDecl
    ::= "trait" IDENT GenericParams? ( ":" TraitBounds )? WhereClause?
        "{" TraitMember* "}"

TraitMember
    ::= Attribute*
        ( FunctionSignature ( ItemEnd | Block )
        | ActionSignature ( ItemEnd | Block )
        | TaskSignature ( ItemEnd | Block )
        | AssociatedTypeDecl
        | AssociatedConstDecl )

AssociatedTypeDecl
    ::= "type" IDENT ( ":" TraitBounds )? ItemEnd

AssociatedConstDecl
    ::= "const" IDENT ":" Type ItemEnd

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
    ::= "type" IDENT "=" Type ItemEnd

AssociatedConstImpl
    ::= "const" IDENT ":" Type "=" ConstExpression ItemEnd

TypeAliasDecl
    ::= "type" IDENT GenericParams? "=" Type ItemEnd

ConstDecl
    ::= "const" IDENT ":" Type "=" ConstExpression ItemEnd
```

---

## A.5 Callable 和 Capability

```ebnf
ParameterList
    ::= ( ( SelfParam | Parameter ) ( "," Parameter )* ","? )?

SelfParam
    ::= "mut"? "self" ( ":" Type )?

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
    ::= "input" IDENT ":" Type ( "=" DefaultExpression )? ItemEnd

StateDecl
    ::= "state" IDENT ( ":" Type )? "=" InitExpression ItemEnd

ComputedDecl
    ::= "computed" IDENT ( ":" Type )? "=" Expression ItemEnd

EventDecl
    ::= "event" IDENT "(" EventParameterList ")" ItemEnd

EventParameterList
    ::= ( EventParameter ( "," EventParameter )* ","? )?

EventParameter
    ::= IDENT ":" Type

```

`InitExpression` 是 Expression，经 §42 初始化 Checker 验证。

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
    ::= "load" "=" Expression ItemEnd
     |  "key" "=" Expression ItemEnd
     |  "policy" "=" PolicyList ItemEnd
     |  "scope" "=" Expression ItemEnd

PolicyList
    ::= "[" ( Expression ( "," Expression )* ","? )? "]"

StartStatement
    ::= "start" Expression ( "as" IDENT )?
        StartHandlerBlock? ItemEnd

StartHandlerBlock
    ::= "{" StartHandler* "}"

StartHandler
    ::= "policy" "=" PolicyList ItemEnd
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
        | SlotItem
        | ViewIf
        | ViewFor
        | ViewMatch )

NamedNode
    ::= "node" IDENT ":" ComponentType PrimaryArgument? NodeBody?

AnonymousNode
    ::= ComponentType PrimaryArgument? NodeBody?

ComponentType
    ::= TypePath

PrimaryArgument
    ::= "(" Expression ")"

NodeBody
    ::= "{" NodeMember* "}"

NodeMember
    ::= Attribute*
        ( PropertyBinding
        | TwoWayBinding
        | EventHandler
        | SlotFill
        | NamedNode
        | AnonymousNode
        | SlotItem
        | ViewIf
        | ViewFor
        | ViewMatch )

PropertyBinding
    ::= PropertyPath ":" Expression ItemEnd

PropertyPath
    ::= Label ( "." Label )*

TwoWayBinding
    ::= "bind" PropertyPath "<=>" AssignablePath
        ( "using" TypePath )? ItemEnd

EventHandler
    ::= "on" EventPhase? IDENT ( "(" Pattern ")" )? Block

EventPhase
    ::= "capture" | "bubble"

SlotFill
    ::= IDENT ViewBlock

SlotItem
    ::= "slot" IDENT? ViewBlock?

ViewIf
    ::= "if" HeadExpression ( "preserve" STRING_LITERAL )?
        ViewBlock ( "else" ( ViewIf | ViewBlock ) )?

ViewFor
    ::= "for" Pattern "in" HeadExpression "key" HeadExpression ViewBlock

ViewMatch
    ::= "match" HeadExpression "{" ViewMatchArm
        ( ( "," | NEWLINE ) ViewMatchArm )* ","? "}"

ViewMatchArm
    ::= Pattern ( "if" Expression )? "=>" ViewBlock
```

`ViewBlock` 的 Cardinality 由 HIR 检查。它不是普通 `Block`，因此不允许 Statement 或 Tail Expression。`SlotFill` 的 IDENT 以小写字母或 `_` 开头且与 `{` 同行；`SlotItem` 的 IDENT 与 `ViewBlock` 都须与 `slot` 同行。

---

## A.9 Style、Theme

```ebnf
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
    ::= IDENT "=" Expression ItemEnd
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
        WhereClause? CapabilityClause? ItemEnd

NativeAction
    ::= "action" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? ItemEnd

NativeTask
    ::= "task" IDENT GenericParams?
        "(" ParameterList ")" ReturnType
        WhereClause? CapabilityClause? ItemEnd

NativeTypeDecl
    ::= "type" IDENT GenericParams?
        ( ":" TraitBounds )? WhereClause? ItemEnd

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
    ::= "uniform" IDENT ":" ShaderType ItemEnd

ShaderInstance
    ::= "instance" IDENT ":" ShaderType ItemEnd

ShaderVarying
    ::= "varying" IDENT ":" ShaderType ItemEnd

ShaderTexture
    ::= "texture" IDENT ":" TypePath ItemEnd

ShaderSampler
    ::= "sampler" IDENT ":" TypePath ItemEnd

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
        "=" Expression ItemEnd

AssignmentStatement
    ::= AssignablePath AssignmentOperator Expression ItemEnd

AssignmentOperator
    ::= "=" | "+=" | "-=" | "*=" | "/=" | "%="
     |  "&=" | "|=" | "^=" | "<<=" | ">>="

AssignablePath
    ::= IDENT AssignableSuffix*

AssignableSuffix
    ::= "." Label | "[" Expression "]"

ExpressionStatement
    ::= Expression ItemEnd

ReturnStatement
    ::= "return" Expression? ItemEnd

BreakStatement
    ::= "break" Expression? ItemEnd

ContinueStatement
    ::= "continue" ItemEnd

EmitStatement
    ::= "emit" IDENT "(" ArgumentList ")" ItemEnd

TransactionStatement
    ::= "transaction" Block

IfStatement
    ::= "if" HeadExpression Block
        ( "else" ( IfStatement | Block ) )?

MatchStatement
    ::= MatchExpression

WhileStatement
    ::= "while" HeadExpression Block

ForStatement
    ::= "for" Pattern "in" HeadExpression Block

LoopStatement
    ::= "loop" Block
```

Block 的最后一项若是 `ExpressionStatement`，无论其后是否写 `;`，都作为 `TailExpression`（§61）。

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
     |  InterpolatedString
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

InterpolatedString
    ::= STRING_START Expression? ( STRING_MIDDLE Expression? )* STRING_END

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
    ::= RecordInitializer ( ( "," | NEWLINE ) RecordInitializer )* ","?

RecordInitializer
    ::= Label ":" Expression
     |  IDENT
     |  ".." Expression

IfExpression
    ::= "if" HeadExpression Block "else" ( IfExpression | Block )

MatchExpression
    ::= "match" HeadExpression "{" MatchArm
        ( ( "," | NEWLINE ) MatchArm )* ","? "}"

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
     |  "{" ( RecordPatternField
              ( "," RecordPatternField )* ","? )? "}"

ConstructorPatternPayload
    ::= "(" ( Pattern ( "," Pattern )* ","? )? ")"
     |  "{" ( RecordPatternField
              ( "," RecordPatternField )* ","? )? "}"

QualifiedVariantPattern
    ::= IDENT "::" Label ( "::" Label )*

RecordPatternField
    ::= Label ":" Pattern | IDENT | ".."
```

裸单段 `IDENT` 一律是 Binding Pattern。无 Payload Enum Variant 必须写限定 Path，例如 `State::idle`；带 Payload 的 Constructor 由紧随其后的 `(...)` 或 `{...}` 消除歧义。不带 Path 的 `{...}` 是其被匹配类型的 Record Pattern。

---

## A.14 Production 成熟度

本附录的每个 Production 恰有一个成熟度：

- `stable`：Core 与 Standard Surface（§25.1）的语法。语法与语义随 Viso 1.0 冻结，由 Parser Golden、Formatter 与语义测试锁定；改变它须提升 Package 固定的语言版本（§22）并提供迁移诊断。
- `preview`：Advanced Surface（§25.1）的语法：用户定义 Trait/Impl、一般泛型与 Const Generic、`dyn` Trait、手写 Native 声明与细粒度 Capability 标注。已实现、可以使用，Parser、Formatter 与诊断码同样稳定；但 1.x 的次版本可以调整其语法或语义，调整随发布说明与迁移诊断一同给出。
- `reserved`：Parser 接受并保留在 Lossless CST 中、Formatter 可以格式化，但 1.0 没有任何 Profile 接受其语义：编译报对应的稳定诊断（`ComputeEntry` 报 `E8106`）。保留给后续版本，届时成为 `preview` 或 `stable`。

`crates/dsl/tests/ebnf_golden.rs` 要求下表与本附录的 Production 集合一一对应，每行的成熟度取上述三者之一。

| Production                  | 章节   | 成熟度     |
| --------------------------- | ----- | ---------- |
| `Label`                     | A.1   | `stable`   |
| `ItemEnd`                   | A.1   | `stable`   |
| `CompilationUnit`           | A.2   | `stable`   |
| `ViewFragment`              | A.2   | `stable`   |
| `ComponentEntry`            | A.2   | `stable`   |
| `ModulePath`                | A.2   | `stable`   |
| `ImportDecl`                | A.2   | `stable`   |
| `ImportSuffix`              | A.2   | `stable`   |
| `ImportItem`                | A.2   | `stable`   |
| `TopLevelDecl`              | A.2   | `stable`   |
| `DeclCore`                  | A.2   | `stable`   |
| `Attribute`                 | A.2   | `stable`   |
| `AttributeArgs`             | A.2   | `stable`   |
| `AttributeArg`              | A.2   | `stable`   |
| `Path`                      | A.3   | `stable`   |
| `TypePath`                  | A.3   | `stable`   |
| `GenericArgs`               | A.3   | `stable`   |
| `GenericArg`                | A.3   | `stable`   |
| `GenericParams`             | A.3   | `preview`  |
| `GenericParam`              | A.3   | `preview`  |
| `TypeGenericParam`          | A.3   | `preview`  |
| `ConstGenericParam`         | A.3   | `preview`  |
| `TraitBounds`               | A.3   | `preview`  |
| `ImplementsClause`          | A.3   | `stable`   |
| `TraitBound`                | A.3   | `stable`   |
| `WhereClause`               | A.3   | `preview`  |
| `WherePredicate`            | A.3   | `preview`  |
| `Type`                      | A.3   | `stable`   |
| `FunctionType`              | A.3   | `stable`   |
| `TupleType`                 | A.3   | `stable`   |
| `ArrayType`                 | A.3   | `stable`   |
| `SliceType`                 | A.3   | `stable`   |
| `TraitObjectType`           | A.3   | `preview`  |
| `TypeList`                  | A.3   | `stable`   |
| `ConstExpression`           | A.3   | `stable`   |
| `DefaultExpression`         | A.3   | `stable`   |
| `InitExpression`            | A.3   | `stable`   |
| `RecordDecl`                | A.4   | `stable`   |
| `RecordField`               | A.4   | `stable`   |
| `EnumDecl`                  | A.4   | `stable`   |
| `EnumVariant`               | A.4   | `stable`   |
| `VariantPayload`            | A.4   | `stable`   |
| `TraitDecl`                 | A.4   | `preview`  |
| `TraitMember`               | A.4   | `preview`  |
| `AssociatedTypeDecl`        | A.4   | `preview`  |
| `AssociatedConstDecl`       | A.4   | `preview`  |
| `ImplDecl`                  | A.4   | `preview`  |
| `ImplTarget`                | A.4   | `preview`  |
| `ImplMember`                | A.4   | `preview`  |
| `AssociatedTypeImpl`        | A.4   | `preview`  |
| `AssociatedConstImpl`       | A.4   | `preview`  |
| `TypeAliasDecl`             | A.4   | `stable`   |
| `ConstDecl`                 | A.4   | `stable`   |
| `ParameterList`             | A.5   | `stable`   |
| `SelfParam`                 | A.5   | `preview`  |
| `Parameter`                 | A.5   | `stable`   |
| `ReturnType`                | A.5   | `stable`   |
| `CapabilityClause`          | A.5   | `preview`  |
| `CapabilityPath`            | A.5   | `preview`  |
| `FunctionDecl`              | A.5   | `stable`   |
| `FunctionSignature`         | A.5   | `stable`   |
| `ActionDecl`                | A.5   | `stable`   |
| `ActionSignature`           | A.5   | `stable`   |
| `TaskDecl`                  | A.5   | `stable`   |
| `TaskSignature`             | A.5   | `stable`   |
| `ComponentDecl`             | A.6   | `stable`   |
| `ComponentDeclBody`         | A.6   | `stable`   |
| `ComponentMember`           | A.6   | `stable`   |
| `SystemDecl`                | A.6   | `stable`   |
| `SystemMember`              | A.6   | `stable`   |
| `InputDecl`                 | A.6   | `stable`   |
| `StateDecl`                 | A.6   | `stable`   |
| `ComputedDecl`              | A.6   | `stable`   |
| `EventDecl`                 | A.6   | `stable`   |
| `EventParameterList`        | A.6   | `stable`   |
| `EventParameter`            | A.6   | `stable`   |
| `EffectDecl`                | A.7   | `stable`   |
| `EffectDependencies`        | A.7   | `stable`   |
| `EffectRunPolicy`           | A.7   | `stable`   |
| `CleanupClause`             | A.7   | `stable`   |
| `ResourceDecl`              | A.7   | `stable`   |
| `ResourceItem`              | A.7   | `stable`   |
| `PolicyList`                | A.7   | `stable`   |
| `StartStatement`            | A.7   | `stable`   |
| `StartHandlerBlock`         | A.7   | `stable`   |
| `StartHandler`              | A.7   | `stable`   |
| `ViewDecl`                  | A.8   | `stable`   |
| `ViewBlock`                 | A.8   | `stable`   |
| `ViewStructureItem`         | A.8   | `stable`   |
| `NamedNode`                 | A.8   | `stable`   |
| `AnonymousNode`             | A.8   | `stable`   |
| `ComponentType`             | A.8   | `stable`   |
| `PrimaryArgument`           | A.8   | `stable`   |
| `NodeBody`                  | A.8   | `stable`   |
| `NodeMember`                | A.8   | `stable`   |
| `PropertyBinding`           | A.8   | `stable`   |
| `PropertyPath`              | A.8   | `stable`   |
| `TwoWayBinding`             | A.8   | `stable`   |
| `EventHandler`              | A.8   | `stable`   |
| `EventPhase`                | A.8   | `stable`   |
| `SlotFill`                  | A.8   | `stable`   |
| `SlotItem`                  | A.8   | `stable`   |
| `ViewIf`                    | A.8   | `stable`   |
| `ViewFor`                   | A.8   | `stable`   |
| `ViewMatch`                 | A.8   | `stable`   |
| `ViewMatchArm`              | A.8   | `stable`   |
| `StyleDecl`                 | A.9   | `stable`   |
| `StyleBaseClause`           | A.9   | `stable`   |
| `StyleItem`                 | A.9   | `stable`   |
| `StyleWhen`                 | A.9   | `stable`   |
| `StateSelector`             | A.9   | `stable`   |
| `SelectorOr`                | A.9   | `stable`   |
| `SelectorAnd`               | A.9   | `stable`   |
| `SelectorUnary`             | A.9   | `stable`   |
| `ThemeDecl`                 | A.9   | `stable`   |
| `ThemeItem`                 | A.9   | `stable`   |
| `NativeDecl`                | A.10  | `preview`  |
| `NativeMemberDecl`          | A.10  | `preview`  |
| `NativeItem`                | A.10  | `preview`  |
| `NativeFunction`            | A.10  | `preview`  |
| `NativeAction`              | A.10  | `preview`  |
| `NativeTask`                | A.10  | `preview`  |
| `NativeTypeDecl`            | A.10  | `preview`  |
| `ShaderDecl`                | A.10  | `stable`   |
| `ShaderMember`              | A.10  | `stable`   |
| `ShaderUniform`             | A.10  | `stable`   |
| `ShaderInstance`            | A.10  | `stable`   |
| `ShaderVarying`             | A.10  | `stable`   |
| `ShaderTexture`             | A.10  | `stable`   |
| `ShaderSampler`             | A.10  | `stable`   |
| `ShaderFunction`            | A.10  | `stable`   |
| `VertexEntry`               | A.10  | `stable`   |
| `FragmentEntry`             | A.10  | `stable`   |
| `ComputeEntry`              | A.10  | `reserved` |
| `ShaderParameterList`       | A.10  | `stable`   |
| `ShaderParameter`           | A.10  | `stable`   |
| `ShaderType`                | A.10  | `stable`   |
| `Block`                     | A.11  | `stable`   |
| `TailExpression`            | A.11  | `stable`   |
| `Statement`                 | A.11  | `stable`   |
| `StatementCore`             | A.11  | `stable`   |
| `LetStatement`              | A.11  | `stable`   |
| `AssignmentStatement`       | A.11  | `stable`   |
| `AssignmentOperator`        | A.11  | `stable`   |
| `AssignablePath`            | A.11  | `stable`   |
| `AssignableSuffix`          | A.11  | `stable`   |
| `ExpressionStatement`       | A.11  | `stable`   |
| `ReturnStatement`           | A.11  | `stable`   |
| `BreakStatement`            | A.11  | `stable`   |
| `ContinueStatement`         | A.11  | `stable`   |
| `EmitStatement`             | A.11  | `stable`   |
| `TransactionStatement`      | A.11  | `stable`   |
| `IfStatement`               | A.11  | `stable`   |
| `MatchStatement`            | A.11  | `stable`   |
| `WhileStatement`            | A.11  | `stable`   |
| `ForStatement`              | A.11  | `stable`   |
| `LoopStatement`             | A.11  | `stable`   |
| `Expression`                | A.12  | `stable`   |
| `HeadExpression`            | A.12  | `stable`   |
| `RangeExpression`           | A.12  | `stable`   |
| `CoalesceExpression`        | A.12  | `stable`   |
| `LogicalOrExpression`       | A.12  | `stable`   |
| `LogicalAndExpression`      | A.12  | `stable`   |
| `ComparisonExpression`      | A.12  | `stable`   |
| `ComparisonOperator`        | A.12  | `stable`   |
| `BitOrExpression`           | A.12  | `stable`   |
| `BitXorExpression`          | A.12  | `stable`   |
| `BitAndExpression`          | A.12  | `stable`   |
| `ShiftExpression`           | A.12  | `stable`   |
| `AdditiveExpression`        | A.12  | `stable`   |
| `MultiplicativeExpression`  | A.12  | `stable`   |
| `CastExpression`            | A.12  | `stable`   |
| `UnaryExpression`           | A.12  | `stable`   |
| `PostfixExpression`         | A.12  | `stable`   |
| `PostfixSuffix`             | A.12  | `stable`   |
| `GenericCallArgs`           | A.12  | `stable`   |
| `PrimaryExpression`         | A.12  | `stable`   |
| `InterpolatedString`        | A.12  | `stable`   |
| `Literal`                   | A.12  | `stable`   |
| `TupleExpression`           | A.12  | `stable`   |
| `ListExpression`            | A.12  | `stable`   |
| `RecordExpression`          | A.12  | `stable`   |
| `RecordInitializerList`     | A.12  | `stable`   |
| `RecordInitializer`         | A.12  | `stable`   |
| `IfExpression`              | A.12  | `stable`   |
| `MatchExpression`           | A.12  | `stable`   |
| `MatchArm`                  | A.12  | `stable`   |
| `ClosureExpression`         | A.12  | `stable`   |
| `ClosureParams`             | A.12  | `stable`   |
| `ClosureParameter`          | A.12  | `stable`   |
| `ExpressionList`            | A.12  | `stable`   |
| `ArgumentList`              | A.12  | `stable`   |
| `Argument`                  | A.12  | `stable`   |
| `Pattern`                   | A.13  | `stable`   |
| `OrPattern`                 | A.13  | `stable`   |
| `BindingPattern`            | A.13  | `stable`   |
| `RangePattern`              | A.13  | `stable`   |
| `PrimaryPattern`            | A.13  | `stable`   |
| `LiteralPattern`            | A.13  | `stable`   |
| `IdentifierPattern`         | A.13  | `stable`   |
| `TuplePattern`              | A.13  | `stable`   |
| `ListPattern`               | A.13  | `stable`   |
| `ListPatternItem`           | A.13  | `stable`   |
| `ConstructorPattern`        | A.13  | `stable`   |
| `ConstructorPatternPayload` | A.13  | `stable`   |
| `QualifiedVariantPattern`   | A.13  | `stable`   |
| `RecordPatternField`        | A.13  | `stable`   |

---

# 附录 B：七项歧义的最终裁决

| 编号 | 问题                               | 最终唯一规则                                                          | Parser/Checker 诊断         |
| ---: | ---------------------------------- | --------------------------------------------------------------------- | --------------------------- |
|    1 | `child` 与裸节点混用               | 删除 `child`；裸 `Type {}` 是匿名节点，`node id: Type {}` 是具名节点  | `E3001`，可自动删除 `child` |
|    2 | `on click => ...` 与 Block Handler | Handler 只能写 `on click { ... }` 或 `on click(event) { ... }`        | `E3201`，可包成 Block       |
|    3 | `Float`、F64、Shader F32           | 删除 `Float`；Host 和 Shader 都使用 F32/F64 明确宽度，Shader 禁止 F64 | `E2101`（换成 `F64`/`F32`） / `E8102` |
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
| E1406  | 嵌套超过 Parser 深度预算（256 层）                      |
| E1407  | 源文本超过 Parser Token 预算（2^20 个有效 Token）       |
| E2001  | 未解析符号                                              |
| E2002  | 名字歧义（重名、Import 歧义、Enum 简写与值同名）        |
| E2003  | 值初始化循环（含 Theme 的 Base/Item 依赖环，§60）       |
| E2004  | 表达式泛型缺少 Turbofish 或 Const Argument 缺少 `const` |
| E2005  | 源文件未声明可挂载的组件                                |
| E2006  | 源文件声明多个组件且未导出要挂载的那个                  |
| E2101  | `Float` 类型已删除                                      |
| E2102  | 非法隐式数值转换                                        |
| E2103  | 类型不匹配                                              |
| E2104  | State Initializer 前向引用                              |
| E2105  | Computed 循环依赖（消息列出路径，如 `a` -> `b` -> `a`） |
| E2106  | MixedLength 不能在布局前定型为具体单位                  |
| E2107  | 非法量纲运算（MixedLength 比较/乘除、Percent 加标量等） |
| E2108  | 字符串插值的洞为空或其类型没有 `Display`（§17）         |
| E2109  | 长度族值常量除以 0（§19.8）                             |
| E2110  | 赋值目标不可写（§62.1）                                 |
| E2111  | `env` / `theme` 用在 View 执行域之外，`tr` 用在组件实例之外（§96.2、§60、§U10.3） |
| E2112  | `@const` Native 或 Tick 时长参数在编译期拒绝其实参（§32、§106.6） |
| E2201  | Trait Bound 未满足                                      |
| E2202  | Trait Impl 重叠或歧义                                   |
| E2203  | Generic 实例化无界：深度、实例数或实参类型规模超限        |
| E2301  | 非穷尽 Match                                            |
| E2302  | 不可达 Pattern（警告）                                  |
| E2303  | `let`/`for`/Closure 参数使用 Refutable Pattern（§70.1） |
| E2401  | Closure 参数无法推断                                    |
| E2501  | Effect Kind 调用违规                                    |
| E2502  | View/Computed 中存在副作用                              |
| E2601  | 缺少 Capability                                         |
| E2701  | 类型不能实现 StableKey（Resource 的 `key` 与 View `for` 的 `key`；消息指出违规成员，如 `K.b`） |
| E2702  | 重复 Runtime Key                                        |
| E2801  | Control Head 中的 Record Expression 必须加括号          |
| E2802  | 非结合操作符链式使用（§63.1）                           |
| E2803  | `return`/`break`/`continue` 位置非法（§62.1）           |
| E3001  | 已删除的 `child` 关键字                                 |
| E3002  | View Cardinality 不满足                                 |
| E3003  | Component 没有 Default Slot                             |
| E3101  | 未知 Property                                           |
| E3102  | Property 重复绑定                                       |
| E3103  | Property 不支持双向绑定                                 |
| E3104  | Property 未声明 Percent Basis，不接受 Percent           |
| E3105  | Percent Basis 不确定（Debug Runtime 警告）              |
| E3106  | 长度解析为非有限值（Debug Runtime 警告，§19.8）         |
| E3107  | `bind` 右侧不是 State Lens（§51）                       |
| E3108  | Grid 的 `areas` 或子节点的 `grid.*` 不指向任何单元：各行单元数不同、区域不是矩形、`grid.area` 名字未知或与行列号同写、跨度为 `0`（§U3.5） |
| E3109  | 主属性用错：在 Node Body 中按名字绑定主属性（Fix 改写为 `Type(value)`）、多于一个 `@primary`、`@primary` 不在 `input` 上或标在 `@bindable` Input 上（§50.1） |
| E3201  | 已删除的事件箭头语法                                    |
| E3202  | 未知 Event 或错误 Payload                               |
| E3301  | Conditional Preserve 必须是静态字符串，且在 Component 中唯一 |
| E3401  | View For 缺少 Key                                       |
| E3402  | Key Expression 不稳定（警告）                           |
| E3501  | 未知 Slot                                               |
| E3502  | Slot 放置或填充冲突                                     |
| E3701  | `@bindable` 配对错误（§U2.2）                           |
| E3702  | 父节点提供的 Property 用于错误或无法静态确定的父节点（§U3.8） |
| E3703  | `transition` 用于不可动画 Property 或类型不符（§U6.1）  |
| E3704  | 交互节点缺少 Role 或可访问名称（警告，§U8.2）           |
| E3705  | Localizable Property 上的文本拼接/字面格式化（警告，§U10.3） |
| E3706  | `tr` 键或参数与消息目录不符；消息目录本身的错误（§U10.3） |
| E3707  | `VirtualList` Item Template 误用（§U9.1）               |
| E3708  | 交互节点缺少等价键盘路径（警告，§U8.2）                 |
| E3709  | 标注 [Runtime 待实现] 的 Property 使用了非默认值（§U1.1） |
| E3710  | `@selector` 或 `@styleable` 误用（§U2.3）               |
| E3711  | Handler、控制流区域或 Component 实例未能挂载：Runtime 未投递该 Event、Behavior 未能 Lower，区域位于 View 根，或 `VirtualList` 位于另一 `VirtualList` 的行内，或实例无法内联，或 `ui!` Fragment 中的 Rust Component 带 Property、Handler 或子项，或以 Component Input 为目标的 `bind` 带 `using`，或 `AdaptiveScope` 的 `basis` 不是常量、`SafeArea`/`KeyboardAvoiding` 带 `padding`，或 `padding` 写在非容器节点上，或 `Grid` 的 `areas`/`grid.area` 不是常量，或 Style 以交互选择器切换用户 Component 的 Input、以 `when` 切换无运行时路径的 Property 或无基值的布局 Property（§40.1、§59、§52、§56.1、§96.3、§96.6、§123） |
| E3712  | `@migrate` 误用：不标记 `fn`、`from` 缺失或不是类型拼写字符串、参数不是恰好一个 `from` 类型参数、未声明返回类型，或同一 `from` 与返回类型重复（§94.1） |
| E3713  | 资源误用：`asset` 的参数不是恰好一个字符串字面量、路径越出 `assets/`、文件不存在或不是位图/SVG/视频、编译没有资源目录，或写出 `ImageAsset`/`SvgAsset`/`VideoAsset` 的 Record 表达式（§U5.1） |
| E4101  | 非 Task 主体（Action、Event、Effect 等）中使用 Await    |
| E4102  | Task 跨挂起访问可变 State（编译期；绕过检查时为运行时故障） |
| E4201  | Effect 读取未声明依赖                                   |
| E4202  | Reactive Cycle                                          |
| E4203  | Effect Run Policy 与依赖列表不兼容                      |
| E4204  | Adaptive Cycle（§96.5）                                 |
| E4301  | Resource 缺少 `load`/`key`，或任一 Item 重复（§38）       |
| E4302  | Resource Policy 不合法、重复或冲突（`cache_errors` 需 `cache_for`），Scope 不受支持（§38）；`start` 的 Task Policy 不合法或无命名 Slot、重复的 `policy` 或 Handler（§39） |
| E4401  | Start 目标不是 Task                                     |
| E4501  | 无主 Detached Task                                      |
| E5101  | Hot Reload 状态重置：活值不可转换为新类型，或 `@migrate` 函数执行失败（警告） |
| E5102  | Hot Reload Stable ID 冲突（拒绝 reload）                |
| E5103  | Game Hot Reload 层级：编译器按 Stable ID Diff 选择的重载层与各项改动（提示，§110） |
| E5104  | 同模块内两个声明/成员的 `@stable("id")` 取值相同（kind 相同时会铸出同一 `SymbolId`）（§88） |
| E5105  | `@sensitive` 不在 Component 的 `state` 上（§94.3） |
| E6101  | Native Schema 版本冲突；手写 Native 声明与 Registry 不符或使用非 Schema 类型（§47.2） |
| E6102  | Native Ownership/Thread Domain 违规                     |
| E6103  | Capability Denied（运行时）                             |
| E6104  | Native 在构建的目标平台上不存在（§47.2）                |
| E7101  | 执行预算超限                                            |
| E7102  | 内存预算超限                                            |
| E7103  | 算术故障：整数溢出、除以零、移位量越界（运行时）        |
| E7104  | 索引越界（运行时）                                      |
| E7105  | 函数无法运行、缺少必需 Input 或内部故障（运行时）       |
| E7106  | Native 函数失败：返回错误或 Panic（运行时）             |
| E8101  | Shader 使用 Host-only 类型                              |
| E8102  | Shader 使用 F64                                         |
| E8103  | Shader Loop 无静态上限                                  |
| E8104  | Shader ABI 不匹配，或 Uniform/Instance 成员无 Buffer Layout（`Bool`） |
| E8105  | Shader 使用 §99 子集之外的构造（Closure、递归、`?`、命名实参、泛型 Shader、误放的 `@max_iterations`/`@shader_value`/`@sampler`、`@sampler` 的设置有误等） |
| E8106  | 违反 Render Profile 阶段规则（Entry 签名与 Builtin、Varying 写入、阶段专用 Intrinsic、`fn` 读成员） |
| E8107  | 违反 Brush Profile（保留成员缺失或类型不符、参数类型无 Host 值、其他 Uniform、不是图像的 Texture、超出上限的 Texture/Sampler） |
| E9101  | Game System Order 循环                                  |
| E9102  | Fixed Tick 预算超限                                     |
| E9103  | Simulation 域访问 Local 状态或 Presentation 返回值，或 `@local` 误用（§106.4） |
| E9104  | Simulation 域使用非确定性来源，或 Physics 引擎档位低于 Game 的 `determinism`（§106.4、§106.5） |
| E9105  | Simulation 状态类型未实现 Snapshot（§106.4）             |
| E9106  | `@persist` 键重复、类型不可持久化、缺少 Capability、不在 `state` 上，或持久化状态的 Component 不是视图自身的实例（§106.8） |
| E9107  | 输入动作缺少目标平台的手柄/触屏路径（警告，§106.3）      |
| E9108  | AudioProcess 实时域违规（§108.3）                        |
| E9109  | System 声明 `view`、`event`、`slot`、`effect` 或 `resource` 成员，执行 `start` 或调用 `tr`（§105） |
| E9110  | `@probe` 误用：只能标在 System 的 Simulation `state` 上（§110.5） |
| E9111  | 持久化状态加载、转换或写入失败（运行时，使用 Initializer，§106.8） |
| E9112  | `@query` 误用：只能标在 System 的 `fn` 上，无参数、返回值类型、包内名字唯一（§109） |

每个码的标题、解释、触发示例与修复记录在编译器的诊断注册表（`viso_dsl::diag::codes`）中：`viso explain <code>`、Language Server 的 Hover 与生成的 `docs/diagnostics.md` 都读它。测试保证本表与注册表的码集一致、注册表与编译器及其 Runtime 实际产生的码一致（尚未产生的码在注册表中标为 reserved：E2702、E3002、E3707、E3709），并逐个编译 `.vs` 示例确认其产生对应的码与默认 Severity。

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
