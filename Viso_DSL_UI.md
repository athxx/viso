# Viso DSL 1.0：UI 标准面

> 文档状态：Viso DSL 1.0 Draft，与 `Viso_DSL_1.0.md` 同步修订  
> 规范级别：规范性  
> 范围：自适应环境（§96）与 UI Authoring 标准面（U1–U13）。章节编号在 DSL 文档集内全局唯一；语言本身（文法、类型、运行时语义）以 `Viso_DSL_1.0.md` 为准。

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
env.display_features  : List<DisplayFeature>
env.input             : InputCapabilities
env.text_scale        : F32
env.reduced_motion    : Bool
env.orientation       : Orientation
env.layout_direction  : LayoutDirection   // §U10.1
env.locale            : Locale            // §U10.3
```

这些符号由标准 Native Schema 注入，Parser 不新增专用关键字。`env` 是 View 执行域的上下文绑定，只能在 View item 的表达式、Property Binding、View `if`/`match`/`for` 条件、View Handler 和调用的纯函数参数中使用；Component 的普通 `state` / `computed` 初始化器与成员 Callable 不能隐式读取局部 Layout Environment，否则报 `E2111`，需要它的值经参数传入。

`env` 整体类型为 Prelude 的 `Environment` 记录，字段顺序即上表顺序；`env.<field>` 的类型即该字段类型。`env` 只读：以 `env` 为根的赋值报 `E2110`。同名局部绑定（参数、`let`、Pattern）遮蔽 `env`。`display_features` 与 `env` 的其余部分一样只读，因而用普通 `List`。

每个 Component 实例各自读取 `env`：`window`、`safe_area` 等窗口级字段对全部实例相同；`constraints`、`size_class` 与 `layout_direction` 锚定在读取它的 Component 实例的根节点——`constraints` 是该根节点收到的 incoming constraints，`size_class` 来自该根节点自身或其最近祖先中的 Adaptive Scope，`layout_direction` 来自其最近的设定了方向的 Adaptive Scope，没有时为窗口的方向。View 的根是控制流区域的实例没有自己的根节点：静态实例锚定在视图根节点，区域内的实例锚定在该次挂载所在的父节点。

典型类型：

```viso
record WindowMetrics {
    logical_size: SizeDp
    scale_factor: F32
}

record LocalConstraints {
    min_width: Dp
    max_width: Option<Dp>
    min_height: Dp
    max_height: Option<Dp>
}

enum SizeClass {
    Compact
    Medium
    Expanded
}

enum Orientation {
    Portrait
    Landscape
}

enum DisplayFeature {
    Hinge { bounds: Rect }
    Fold { bounds: Rect }
    Cutout { bounds: Rect }
}

record Rect { x: Dp; y: Dp; width: Dp; height: Dp }
record Insets { top: Dp; right: Dp; bottom: Dp; left: Dp }
record KeyboardInset { height: Dp }
enum LayoutDirection { ltr; rtl }
record Locale { tag: String } // BCP 47
```

`InputCapabilities` 见 §96.7。

`WindowMetrics` 描述整个应用窗口；`LocalConstraints` 描述当前 Component 从父布局接收到的局部约束。两者语义不同，禁止互相替代。

---

## 96.3 Adaptive Scope 与 SizeClass

`env.size_class` 来自最近的 Adaptive Scope。根 Scope 默认以应用可用内容区域建立；标准 `AdaptiveScope` Widget 可以在局部重新建立 Scope，使侧栏、面板、分屏区域根据自己的实际宽度适配，而不是错误使用整个窗口宽度。

默认标准库可以提供 Compact/Medium/Expanded 策略，但 breakpoint 数值属于 Theme/Profile/Design System，不属于语言语法常量。应用可以提供自己的 `SizeClassPolicy`。

根 Adaptive Scope 必须有有限的可用宽度。局部 `AdaptiveScope` 若收到 `max_width = None` 的无界约束，默认继承父 Scope 的 `SizeClass`；只有显式提供有限 `basis` 时才建立新的 SizeClass。实现不得偷偷用设备型号或全局 Window Width 代替无界局部约束。

`AdaptiveScope` 是纵向排列子节点的 Flex 容器（属性同 `Column`），另有可选的 `basis: Option<MixedLength>`：给出时按该宽度分类，否则按父级传给它的 incoming 宽度分类。`basis` 必须是编译期常量的 `dp` 长度，否则报 `E3711`。宽度变化但 SizeClass 不变时，Scope 之下的结构不重建。可选的 `layout_direction: Option<LayoutDirection>` 设定 Scope 及其子树的布局方向（U10.1），`None`（默认）沿用父级的；它可以读取值，变化时只重新布局该 Scope 的子树并重新折叠其中读方向的 `padding`/`margin`。

推荐：

```viso
view {
    match env.size_class {
        SizeClass::Compact => {
            CompactShell {}
        }
        SizeClass::Medium => {
            MediumShell {}
        }
        SizeClass::Expanded => {
            ExpandedShell {}
        }
    }
}
```

局部组件需要更精确的约束时，可以直接读取：

```viso
view {
    match env.constraints.max_width {
        Option::Some(width) if width < 520dp => {
            CompactToolbar {}
        }
        _ => {
            FullToolbar {}
        }
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

帧内顺序：Layout → 以本次布局结果 settle 环境（重算每个 anchor 的 `constraints` 与 `size_class`，只抬升真正变化的字段）→ settle 被唤醒的状态与结构 → 再 Layout，直至没有 anchor 变化。本帧既未放置任何节点、环境输入也未变化时不做任何解析。该循环有界；越界即 `E4204`：anchor 保留新值，但对它们的响应被丢弃，帧保留最后一次的结构，下一帧不再重复振荡。

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
                RouterView {}
            }
        }
    }
}
```

Fold/Hinge/Cutout 必须通过 `env.display_features` 暴露为 typed geometry。业务代码不应直接解析平台私有字符串。

`SafeArea` 与 `KeyboardAvoiding` 是纵向排列子节点的 Flex 容器（属性同 `Column`）。其 Padding 由 Runtime 决定：每条边的 Padding 是系统遮挡带伸入该节点盒的深度，遮挡带从窗口根节点的盒的对应边量起，`SafeArea` 的遮挡带为 `env.safe_area` 的四边，`KeyboardAvoiding` 的遮挡带为底边 `env.keyboard_inset`。因此已被祖先（包括 Runtime 为全屏窗口在根上加的 Safe Area 内边距）或外层同类区域让出的部分不再重复 Padding，嵌套区域只 Pad 一次。内容决定尺寸的轴上，尾边按不含该区域自身 Padding 的盒计量，一次布局即收敛。Padding 在布局之后、Anchor 求值之前更新（§96.5），区域位置或遮挡变化时重新计算，未变化时不写入。作者为 `SafeArea` 或 `KeyboardAvoiding` 写 `padding` 报 `E3711`：应 Pad 其内容。

窗口打开时以及每次尺寸、Scale Factor、Safe Area、键盘高度、Display Feature 变化时，Runtime 把窗口的逻辑尺寸与 Scale Factor、`safe_area`、`keyboard_inset`、`display_features` 写入该窗口的环境；系统外观变化时写入 `reduced_motion` 与 `text_scale`，`text_scale` 同时移动该窗口 `sp` 长度的名义倍率；系统语言变化时把 `locale`（BCP 47）与其书写方向 `layout_direction`（Script 子标签优先，否则按语言的常用 Script）写入每个窗口。值未变化的字段不写入，不唤醒读取者。Headless 测试直接设置环境。

平台来源（平台未报告的值保持默认：`text_scale` 1.0、`locale` `und`、无 Display Feature）：

| 字段 | macOS | iOS | Android | Windows | Linux | Web |
| --- | --- | --- | --- | --- | --- | --- |
| `text_scale` | 无系统字号，1.0 | Content Size Category 的正文字号 / 17 | `Configuration.fontScale` | 辅助功能文本大小 | Portal / GNOME `text-scaling-factor` | 用户默认字号（`medium`）/ 16 |
| `locale` | 首选语言，随系统变化 | 首选语言，随系统变化 | `Configuration` 首个 Locale | 用户默认 Locale，随设置广播变化 | `LC_ALL` / `LC_MESSAGES` / `LANG`，启动时读取 | `navigator.language`，随 `languagechange` 变化 |
| `display_features` | 刘海为 `Cutout` | 无 | `DisplayCutout` 为 `Cutout` | 无 | 无 | Viewport Segments 之间的 `Hinge` / `Fold` |

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

`primary_pointer_precision` 的类型为 `PointerPrecision { fine; coarse; unavailable; }`，其余字段为 `Bool`。

Runtime 从输入推断这些能力：窗口打开时，手机与平板目标假定 `coarse` 触摸、无 Hover 与硬件键盘，其他目标假定 `fine` 鼠标与键盘；此后最近一次使用的 Pointer 种类成为主 Pointer（鼠标为 `fine` 并带 Hover，触摸为 `coarse`，笔为 `fine`），用过的设备标记为可用，按下硬件键标记 `keyboard_available`；`gamepad_available` 跟随手柄连接：有手柄连接时为真，最后一个断开时为假。

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
            MainContent { model: model }
        }
    } else {
        ExpandedShell {
            MainContent { model: model }
        }
    }
}
```

这里 `model` 属于分支外状态，所以切换布局不会丢失业务数据；`compact-shell` 自己的局部 UI 状态在离开后可以进入 Preserve Cache。需要真正跨 Shell 维持同一个 Child Node 实例时，应使用具备 identity-preserving slot contract 的标准 Adaptive Container，而不是依赖两个不同 Conditional Branch 自动合并身份。

`preserve` 分支离开时其节点只被摘下、不被释放，回来时重新挂上同一批节点（同一 `NodeId`），因此节点按身份保存的局部状态——scroll offset、文本编辑缓冲区（文本、selection、IME 组合）——原样保留。焦点规则：

- 分支被摘下时，若焦点节点或 Focus Scope 位于该分支内，二者随分支离开：焦点被清空（键盘与 IME 事件不再送达不可见节点），Focus Scope 被解除，分支记住它们；
- 分支回来时，焦点与 Focus Scope 各自在当时仍空闲（期间没有其他节点取得）时恢复到原节点，否则丢弃；
- 未标 `preserve` 的分支离开即释放节点，以上局部状态随之丢失。

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
- 任何 Schema 不得声明 `INTERACTION` 或 `affects(...)`；纯变换变化一律是 `TRANSFORM`。

### U1.3 Property 值提升

仅在 Property Binding 与 Record 字段初始化的右侧，Schema 类型允许以下隐式提升（均为编译期、无副作用）：

- `T` → `Option<T>`，提升为 `Option::Some(v)`，因此 `max_width: 640dp;`、`semantics.label: tr("x");` 合法；
- 长度族值与 `MixedLength` → `Sizing`，提升为 `Sizing::fixed(v)`（U3.2）；
- 长度族值与 `MixedLength` → `EdgeInsets`，提升为 `EdgeInsets::all(v)`（U3.3）；
- `Color` → `Fill`，提升为 `Fill::color(v)`（U3.3）；
- `T` → `List<T>`，提升为单元素 List `[v]`；期望类型为 `List<T>` 时，List 字面量的每个元素也按 `T` 提升，因此 `background: #fff`、`background: [#fff, Fill::linear { colors: [#0000, #0004] }]` 与 `shadow: theme.elevation.low` 合法。

一次绑定中 §76.1 拓宽与上述提升至多各发生一次（如 `Dp` → `MixedLength` → `Option<MixedLength>`，`Color` → `Fill` → `List<Fill>`）；提升不适用于函数实参与 `let`。

同一位置上，期望类型是 Enum（或其 `Option`）时，值可以只写 Variant 名：`align: center` 即 `align: Align::center`，`width: fill` 即 `width: Sizing::fill {}`（字段全有默认值的 Record Variant 可省略 `{}`），`font_weight: semibold`、`semantics.role: heading`。简写同样适用于该位置上 `if`/`match` 各分支的 Tail（`justify: if mine { end } else { start }`）。规则：

- 名字是期望 Enum 的 Variant 时取该 Variant；同时也是作用域中的值（Input、State、Computed、局部绑定、Const）时报 `E2002`（歧义，附两种写法的 Fix），不按作用域静默遮蔽；
- 名字不是期望 Enum 的 Variant 时按普通名字解析；
- 带 Payload 的 Variant 照常写全路径或简写加 Payload：`width: fill { weight: 2.0 }`；
- 期望类型未知（函数实参、`let`、Computed 初值）时不简写。

当前实现：本部分的 Schema Enum（`Sizing`、`Track`、`TrackMax`、`AutoRepeat`、`Axis`、`Justify`、`Align`、`GridAlign`、`Alignment2D`、`FontWeight`、`TextOverflow`、`TextAlign`、`LiveRegion`、`Role`）与 Record `Border`、`Shadow`、`AdaptiveColumns` 声明在 Prelude 中，无需 Import；以它们（或其 `Option`、`List`）为类型的 Property 按该类型检查绑定：未知 Variant 是 `E2001`（附最近候选），另一个 Enum 的 Variant 或其他类型的值是 `E2103`，`Head::variant` 的 `Head` 不是任何类型时也是 `E2001`。`Offset`、`Transition` 等其余 Record 类型的 Property 尚不按类型检查。

### U1.4 明确推迟

Flex 换行；`space_between`/`space_around`；单子节点 `align_self`；Row Baseline 对齐；滚动条外观、惯性、Scroll Snap（ADR 0007）；单个 `VirtualList` 多 Item Template；物理方向（`left`/`right`）Edge 写法；显式段落基础方向 Property；Grid Subgrid 与 Line Name 的 DSL 写法；共享元素转场与路由动画；自定义 Gesture Recognizer 声明；富文本 Span 树。

---

## U2. 用户 Component 的 Property 元数据

用户 Component 的公开面由 `input`、`event` 与 View 中的 `slot` 组成（§41–§45）。除 `@primary`（§50.1）外，本节定义三个标准 Attribute，只附加在 Component 成员声明上，不改变成员语法：

| Attribute          | 位置                                          | 作用                           |
| ------------------ | --------------------------------------------- | ------------------------------ |
| `@bindable(event)` | `input`                                       | 与 Event 配对为双向属性         |
| `@styleable`       | `input`                                       | 允许 Style 绑定该 Input         |
| `@selector`        | `Bool` 类型的 `input`/`state`/`computed`       | 公开为 Style 状态选择器         |

### U2.1 默认 Slot

Component View 中的 `slot` 是默认 Slot，调用方的裸子节点放在这里，规则见 §45；示例见 U13.1 的 `SettingsSection`。

### U2.2 `@bindable`：Input 与 Event 配对

```viso
export component Stepper {
    @bindable(changed)
    input value: I64
    input step: I64 = 1

    event changed(value: I64)

    view {
        Row {
            Button("-") { on click { emit changed(value - step) } }
            Text("{value}")
            Button("+") { on click { emit changed(value + step) } }
        }
    }
}
```

调用方：

```viso
Stepper { bind value <=> settings.retry_count }
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
    input tint: Option<Color> = Option::None
    input label: String

    @selector
    input selected: Bool = false

    view {
        Row {
            background: tint.unwrap_or(theme.colors.surface)
            Text(label)
        }
    }
}

export style SelectedChip for Chip {
    when selected { tint: Option::Some(theme.colors.accent) }
}
```

- 用户 Component 中只有 `@styleable` Input 是 Styleable；Style 绑定其他 Input 是 §59 既有错误；
- `@selector` 公开的选择器名等于成员名；成员非 `Bool`，或与 U12.1 标准选择器同名但语义不同，是 `E3710`；
- 内部 `state` 可经 `@selector` 公开为只读选择器，外部仍不能读写该 State。

当前实现：`@styleable` 只标在 `input` 上，`@selector` 只标在 `Bool` 类型的 `input`、`state` 或 `computed` 上，且名字不得是运行时判定的标准选择器（`hover`、`pressed`、`focused`、`focus_visible`、`disabled`、`dragging`）；违反者报 `E3710`。以用户 Component 为目标的 Style 可绑定其 `@styleable` Input，`when` 可用其 `@selector` 成员与交互选择器。应用到 Component 节点时，Style 给出的每个 Input（节点自身未绑定的）取第一个成立的 `when` 分支的值，否则取无条件值，都没有时取 Input 默认值；选择器读取该实例自身的 `@selector` 成员（Input、State 或 Computed），成员变化时只重新求值读取该 Input 的绑定。`when` 中的交互选择器（`hover` 等）尚不切换 Component 的 Input（`E3711`，报在该选择器上）。热重载、Release 包、宏与 Web 导出相同。

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
| `Wrap`     | 沿行内方向排成多行，放不下时换行（标签、chip） | `fill` / `fit`          |
| `Stack`    | 子节点叠放于同一框内，尺寸取子节点最大值        | `fit` / `fit`           |
| `Absolute` | 子节点按 Inset 定位，不贡献容器尺寸            | `fill` / `fill`         |
| `Grid`     | Track 网格（ADR 0009）                         | `fill` / `fill`         |
| `Scroll`   | 单子节点视口（ADR 0007）                       | `fill` / `fill`         |
| `Fragment` | 零节点分组：子节点直接拼接进父节点 Slot，不接受 Property，不产生 NodeId | — |

叶节点 Widget（`Text`、`Button`、`Image`、`Toggle` 等）未写 `width`/`height` 的轴为 `fit`，按其内容（文字、图片、勾选标记）测量，没有内容时为 `0`；Widget 另有默认的除外（如 `GameViewport` 与 `RouterView` 为 `fill`，`Avatar` 为 40dp，`SegmentedControl` 的文字段宽为 `fill`）。`ui!`/`component!`/`view!`、Hot Reload 与 Release 包按同一规则构建。

View 的根节点（窗口内容）未写 `width`/`height` 的轴为 `fill`，占满窗口；其余容器未写的轴取上表的默认值。因此只有在设计需要固定尺寸、或要求子节点占满父节点（`width: fill`）时才写尺寸，示例不写死叶节点的宽高。

### U3.2 Sizing

```viso
export enum Sizing { fixed(MixedLength); fill { weight: F32 = 1.0 }; fit; min_content; max_content }
```

按 U1.3，`width: 100% - 32dp;` 提升为 `Sizing::fixed(...)`。

| 值                | Flex 主轴                              | 交叉轴 / 非 Flex 父节点 |
| ----------------- | -------------------------------------- | ----------------------- |
| `fixed(v)`        | 按 §19.4 解析，再按 min/max 截断       | 同左                    |
| `fill { weight }` | 按 `weight` 比例分配剩余空间           | 占满可用尺寸            |
| `fit`             | 内容尺寸，受可用尺寸约束               | 同左                    |
| `min_content`     | 最小内容尺寸（文本：最长不可断片段）   | 同左                    |
| `max_content`     | 不换行时的内容尺寸                     | 同左                    |

`fill` 在无界轴（如 Scroll 滚动轴）退化为 `fit` 并报告 `E3105`；`100%` 不等于 `fill`（§19.4）。

宽度上，`fit` 取 `max_content`，但不超过可用宽度，也不窄于 `min_content`：交叉轴与 `Stack`/`Absolute`/`Grid` 单元格中可用宽度是父节点给出的空间；`Row` 的子节点总宽超出内容盒时，各 `fit` 子节点按各自宽度的同一比例收缩，至 `min_content` 为止，仍放不下时溢出。高度不随可用空间收缩：`fit`、`min_content` 与 `max_content` 的高度都是内容在所得宽度下的高度（文本在所得宽度下换行）。容器的 `min_content` 宽度：`Row` 为子节点的 `min_content` 贡献（`fixed` 为自身，`max_content` 为其 `max_content`）、外边距与间距之和，`Column`/`Stack` 取其中最大者，水平 `Scroll` 为 `0`，`Absolute` 与 `Grid` 为其自然宽度；图片与矢量图的 `min_content` 是其固有宽度。

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
| `margin`        | `EdgeInsets`          | `EdgeInsets::zero()` | `MEASURE\|LAYOUT` | 同 `padding` |
| `background`    | `List<Fill>`          | `[]`                 | `PAINT`           | —                             |
| `corner_radius` | `MixedLength`         | `0dp`                | `PAINT`           | 自身 Border Box 较短边        |
| `border`        | `Option<Border>`      | `Option::None`       | `PAINT`           | —                             |
| `shadow`        | `List<Shadow>`        | `[]`                 | `PAINT`           | —                             |
| `backdrop`      | `Option<Backdrop>`    | `Option::None`       | `PAINT`           | —                             |
| `ripple`        | `Bool`                | 平台惯例             | `PAINT`           | —                             |
| `styles`        | `List<StyleRef<T>>`   | `[]`                 | `STYLE`           | —                             |

```viso
export record EdgeInsets {
    top: MixedLength = 0dp
    end: MixedLength = 0dp
    bottom: MixedLength = 0dp
    start: MixedLength = 0dp
}
export record Border { width: Dp = 1dp; color: Color }
export record Shadow {
    x: Dp = 0dp
    y: Dp = 2dp
    blur: Dp = 8dp
    spread: Dp = 0dp
    color: Color
    inset: Bool = false
}
export enum Fill {
    color(Color)
    linear { angle: Angle = 180deg; colors: List<Color>; stops: List<F32> = []; inset: EdgeInsets = 0dp }
    radial { x: F32 = 0.5; y: F32 = 0.5; colors: List<Color>; stops: List<F32> = []; inset: EdgeInsets = 0dp }
    image { asset: ImageAsset; fit: ImageFit = ImageFit::cover; opacity: F32 = 1.0; inset: EdgeInsets = 0dp }
}
export record Backdrop {
    blur: Dp = 30dp
    saturation: F32 = 1.8
    tint: Color = #ffffff40
    noise: F32 = 0.0
    luminosity: F32 = 0.0
    refraction: Dp = 0dp
    bevel: Dp = 20dp
    dispersion: F32 = 0.0
    highlight: F32 = 0.0
    light: Angle = 315deg
}
```

- 构造函数：`EdgeInsets::zero()`、`EdgeInsets::all(v)`、`EdgeInsets::axes(inline: a, block: b)`（`a` 用于 `start`/`end`，`b` 用于 `top`/`bottom`）；
- 按 U1.3，§19.5 的 `padding: 1em;` 提升为 `EdgeInsets::all(1em)`；
- `margin` 不折叠；`border` 在 Border Box 内侧描边，不参与布局；
- `background` 是盒的各层填充，按书写顺序自下而上绘制，每层按 `corner_radius` 取圆角、画在 `border` 之下：`color` 为平涂；`linear` 沿 `angle`（CSS 约定：`0deg` 朝上、顺时针，`180deg` 自上而下）穿过整个盒，`radial` 以盒内比例位置 `x`/`y` 为圆心、到最远角为半径；二者的 `colors` 至少两种（只有一种时为该色平涂，没有时不绘制），`stops` 为各色在 `0..1` 上的位置，空或与 `colors` 不等长时均分，颜色在线性 RGB 中插值；`image` 按 `fit` 放置图片（同 `Image`），按 `opacity` 淡化，解码前不绘制。渐变与图片层铺在盒减去其 `inset` 的区域内，圆角取盒的圆角向内收缩，`inset` 取 dp（`Fill::linear { colors: [#ffffffb0, #ffffff20, #ffffff00], stops: [0.0, 0.5, 0.5], inset: EdgeInsets { top: 1dp, start: 2dp, end: 2dp } }` 即 Aqua 式上半高光）。第一层是 `color` 时它是盒的底色，`transition.background` 在两个底色之间过渡；其他背景变化立即显示；
- `shadow` 是盒按 `corner_radius` 取圆角、按 `spread` 扩张（负值收缩）、按 `x`/`y` 偏移并按 `blur`（CSS 模糊半径）淡出的投影，画在盒下方，不参与布局与命中；`inset: true` 时落在盒内、`border` 之内，画在背景之上；多个阴影按书写顺序自下而上绘制，外阴影都在盒下、内阴影都在背景之上，因此一个盒可同时有外阴影与内阴影；`theme.elevation.*` 即 `Shadow`，可直接绑定；
- `backdrop` 使盒成为玻璃：盒背后已绘制的内容按 `blur`（CSS 模糊半径）模糊、按 `saturation` 调整饱和度、按 `tint` 的 alpha 叠上 `tint`，再加 `noise`（0..1）颗粒，按 `corner_radius` 取圆角，画在 `background` 之下；节点自身 `opacity` 小于 1 时玻璃按该 opacity 淡出而保持模糊（嵌套在其他半透明祖先之内的玻璃不模糊）。同一背景上同一模糊的多块玻璃共用一次背景捕获与模糊。`BlurView` 是纵向排列子节点的 Flex 容器（属性同 `Column`），未给 `backdrop` 时为标准玻璃 `Backdrop {}`。`luminosity`（0..1）让背后的明暗向 `tint` 的亮度靠拢，玻璃上的内容在亮暗背景上读起来一致（亚克力）。液态玻璃：`refraction` 是边缘处背后内容向内位移的距离，沿边缘深 `bevel` 的四分之一圆弧斜面逐渐减到零，中部平直；`dispersion`（0..1）按该位移分开 RGB；`highlight`（0..1）是沿 `light` 方向（CSS 角度约定）两侧壁上的一条细亮线，朝光一侧更亮。各项为 0 时不计算；有 `refraction`、`highlight` 或 `luminosity` 的玻璃即使不模糊也读取背后内容。Web 导出为 `backdrop-filter: blur(..) saturate(..)` 加一层 `tint` 平涂，不画 `luminosity`、`refraction`、`dispersion`、`highlight`；
- `ripple` 为 `true` 时，按下该节点（或其内未开 `ripple` 的后代）从按下点向外铺开一圈墨迹，约 0.3s 盖满整个盒，按 `corner_radius` 裁圆角，画在 `background` 之上、内容之下；松手或按压被平移手势接管后墨迹边扩边淡出（约 0.2s）。墨迹取盒的反差色：浅色或透明底上为暗墨，深色底上为亮墨。减少动态效果（reduced motion）时墨迹不扩散，一按即满并淡出。未给 `ripple` 时按平台惯例：Android 上 `Button`、`Chip` 默认开启，其他平台默认关闭；Web 导出不带墨迹。手指或触控笔改变控件时伴随系统触觉：`Toggle`/`CheckBox`/可选 `Chip`/`Stepper`/`Select`/`Segmented` 改变值时为 selection 轻触，设了 `step` 的 `Slider` 每到一个刻度一次；按钮点击、鼠标与键盘不触发触觉；
- `min` 大于 `max` 时以 `min` 为准，Debug Runtime 报告 `E3105`；
- `start`/`end` 在布局时按 `env.layout_direction` 解析为物理边；
- `padding` 只作用于容器（`Row`、`Column`、`Flex`、`Stack`、`Absolute`、`Grid`）的内容盒，以及 `Button` 与 `TextInput`：`Button` 的文字与 `TextInput` 的编辑区放在内边距之内，盒的 Fit 尺寸含内边距，未写时按各自默认；`padding` 可为常量（长度、`EdgeInsets::zero/all/axes` 或各字段为常量的 `EdgeInsets { .. }`）或读取值（见 U3.4 的绑定布局值），每条边按 §19.8 取非负，含 `%` 分量的边在布局时读到父内容盒后取非负；非容器节点上的 `padding` 报 `E3711`，不会被静默丢弃。纯 `dp` 且 `start` 与 `end` 相等的 `padding` 在编译期折叠为物理内边距；含 `px`/`sp`/`em` 分量或 `start` 与 `end` 不等的，随 §19.6 环境与 `env.layout_direction` 在布局时折叠，只有折叠结果变化的节点被标脏；
- 常量 `min_*`/`max_*`（`max_*` 另接受 `Option::None` 与 `Option::Some(v)`）与常量 `margin` 适用于任意布局节点，写法同 `padding`：`%` 读父内容盒（父轴不定时 `%` 分量取 `0`），`max_*` 为 `Option::None` 时无上界，`margin` 可为负、计入父容器的自然尺寸；Fill 子项在各自上下界内分配剩余空间。这些规则与 `%` 内边距均不读环境且 `start` 与 `end` 相等时在编译期折叠为节点的盒规则；否则随 §19.6 环境与 `env.layout_direction` 在布局时折叠。读取值的 `min_*`/`max_*`/`margin` 随值布局（U3.4），与节点的常量规则一同在布局时折叠。

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

### U3.4a 换行容器（`Wrap`）

| Property  | 类型          | 默认             | 失效              |
| --------- | ------------- | ---------------- | ----------------- |
| `gap`     | `MixedLength` | `0dp`            | `MEASURE\|LAYOUT` |
| `row_gap` | `MixedLength` | `0dp`            | `MEASURE\|LAYOUT` |
| `justify` | `Justify`     | `Justify::start` | `LAYOUT`          |
| `align`   | `Align`       | `Align::start`   | `LAYOUT`          |

- 子节点沿行内方向依次排入当前行，下一个放不下时另起一行；一行至少放一个子节点，比内容盒还宽的子节点独占一行并截到行宽（`fit` 收缩到行宽，不窄于 `min_content`）；`fill` 宽度的子节点独占整行；
- `gap` 是同一行相邻子节点的间距，`row_gap` 是相邻两行的间距（与 CSS `flex-wrap` 的 `column-gap`/`row-gap` 相同）；
- 行高取该行子节点（含外边距）高度的最大者；`align` 把子节点放在所在行内，`stretch` 把交叉轴为 `fit` 的子节点拉到行高；`justify` 分配每行剩余的宽度；
- 隐藏的子节点不占位置，也不计间距；RTL 下每行从右边起排；
- 高度由行数决定：容器记住上次布局时的内容盒宽度，按它测量行数；宽度变化时同一帧内重新测量并重新布局（每帧最多 3 轮）；
- 容器的 `min_content` 宽度为最宽子节点的 `min_content`，`max_content` 为所有子节点排成一行的宽度。

当前实现：`gap`、`row_gap`、`justify`、`align` 与 Flex 容器同样折叠或随值送达；Web 导出为 `display: flex; flex-wrap: wrap`，`fill` 子节点为 `width: 100%`。Rust 侧为 `BuildCx::flow(FlowStyle, ..)`（`LayoutInput::Flow`，亦支持沿块方向换列）。

当前实现：常量的 `width`/`height`（长度，即 `Sizing::fixed`，以及 `Sizing::fixed(..)`、`Sizing::fill`、`Sizing::fill { weight: w }`、`Sizing::fit`）、Flex 容器的 `justify`、`align` 与 `axis`（`Axis::row`/`Axis::column`）在编译期折叠进节点样式，`Grid` 取其 `width`/`height`；值可以是本文件或按名 import 的 `const`（经 `const` 链折叠，链可跨文件）。热重载就地改写保留节点的排列，Release 包与宏路径相同。`Sizing::fit`/`min_content`/`max_content` 按 U3.2 布局。`corner_radius`、`border` 与 `shadow` 可为常量或读取值，可由 Style 的 `when` 切换：`corner_radius` 的 `%` 读节点绘制时自身盒的较短边，`px`/`sp`/`em` 随 §19.6 环境，圆角不超过较短边的一半；`border` 在盒内侧描边，不移动内容；`shadow` 改变只重绘节点。`clip` 可为常量或读取值：为 `true` 时子树的绘制与命中只落在节点自身盒内，按节点的 `corner_radius` 取圆角（圆角处的像素按覆盖率淡出，圆角外的指针不命中子节点）；HTML 导出为 `overflow: hidden`。尚无运行时路径的 Property（变换）报 `E3711`，不会被静默丢弃；`viso check` 与 `view!` 报告相同。`visible`（折出布局、绘制与语义树）、`focusable`，以及 `Text`/`Button`/`TextInput` 的 `color`、`soft_wrap` 与 `font_size`（节点解析出的字号）随值送达节点，可绑定；文本挂载后其字号与 `line_height` 在塑形时按节点解析出的字号与环境解析，文字缩放、缩放因子或根字号变化、或字号来源变化时只重新排布（em 布局不变），读取值的 `font_size` 随值改写节点的字号来源。读取值（State、Input、`theme` 等）的布局 Property——`font_size`、`width`/`height`、`min_*`/`max_*`、`margin`、`padding`、`gap`、`justify`/`align`/`axis`、Grid 的模板、间距与对齐、`content_align`、子节点的 `grid.column`/`grid.row`/`grid.*_span`、`stack.*` 与 `absolute.*`——随值变化重新布局节点，只标脏其移动的部分：长度经节点的环境长度在布局时折叠（`px`/`sp`/`em` 随 §19.6 环境，`%` 读其基准），与节点的常量长度一同折叠；热重载后全部重新送达，Release 包与宏路径相同。送达的值为 `minmax`/`fit_content`/`AdaptiveColumns` 中含 `%` 的长度时，执行域报告 Fault，节点保留此前的布局。读取值的 `areas`/`grid.area` 报 `E3711`。Style 给出的 Property 与节点自身的走同一送达路径：常量折叠进样式，读取值（含 `theme`）随值送达；Style 的 `when` 可切换节点运行时送达的任何 Property（外观、变换、文本样式、布局等），无 `when` 成立时显示基值；被 `when` 切换的布局 Property 须在 Style 中另有不带 `when` 的基值，否则 `E3711`。Web 导出切换 `background`、`opacity`、`corner_radius`、`border`、`shadow`、`translate`、`scale`、`rotation` 与文本 `color`，其余报导出诊断。`border` 与 `shadow` 导出为同一个 `box-shadow` 列表（`border` 为 `inset` 描边），各自为一个自定义属性，Style 的 `when` 只改写其中一个；`background` 的各层导出为 CSS `background` 列表（顺序反转，因 CSS 首层在上；`linear-gradient`/`radial-gradient`/`url()`，`fit` 为 `background-size`/`background-repeat`），`opacity` 小于 1 的图片层报导出诊断。交叉轴上的 `fill` 在任何 `align` 下都占满；`align: stretch` 只拉伸交叉轴为 `fit` 的子节点，固定或百分比尺寸按自身尺寸由 `align` 定位。

### U3.5 Grid

`fr` 不是长度单位（§19），而是 Track 构造器：

```viso
export enum Track {
    fixed(MixedLength)
    fr(F32)
    auto
    minmax(MixedLength, TrackMax)
    fit_content(MixedLength)
}

export enum TrackMax { length(MixedLength); fr(F32) }
export enum AutoRepeat { fill; fit }

export record AdaptiveColumns {
    mode: AutoRepeat = AutoRepeat::fill
    min: MixedLength
    max: TrackMax = TrackMax::fr(1.0)
}
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
- `areas` 每个字符串是一行，名字以空白分隔，`.` 为空单元；各行单元数不同或某个名字不构成矩形是 `E3108`；
- `grid.area` 把子节点放进其 Grid 的 `areas` 中该名字覆盖的单元，名字在编译期解析为行列号与跨度，不进入 Runtime；引用未知名字、所在 Grid 没有 `areas`、或同时写 `grid.column`/`grid.row`/跨度是 `E3108`；`grid.column_span`/`grid.row_span` 为 `0` 也是 `E3108`；
- 内容决定的列（`Track::auto` 与 `fit_content`）取其单元的 max-content 宽度；各列合计超出 Grid 内容盒时，这些列先取各自单元的 min-content 宽度，剩余空间在它们之间均分，每列不超过其 max-content 宽度（CSS 轨道尺寸算法）；尺寸由内容决定的 Grid 自身的自然宽度为各列 max-content 宽度之和，min-content 宽度为各列最窄宽度之和（含间距与内边距）；
- 只固定列或只固定行的子节点按 CSS 稀疏自动放置：只固定行的在该行内依次取第一个空位，其余（只固定列的与未固定的）共用一个行优先游标，只固定列的取该列在游标行及其后的第一个空位。

当前实现：`columns`、`rows`、`auto_rows`、`column_gap`、`row_gap`、`align_items`、`areas`、`adaptive_columns` 与子节点的 `grid.*` 是常量时在编译期折叠（值可以是本文件或按名 import 的 `const`）：轨道列表可为列表字面量或 `tracks(n, t)`，`Track::fixed` 取任意长度：纯 `dp` 为固定轨道，纯 `%` 为自身内容盒的比例，其余（兼含 `dp` 与 `%`，或含 `px`/`sp`/`em`）由节点存储按 §19.6 环境与 Grid 解析出的字号折叠为固定加比例的轨道，并在其读取的环境变化时重新折叠、只标脏该 Grid；`minmax` 的下界、`fit_content` 的上限、`TrackMax::length` 与 `AdaptiveColumns` 的 `min` 取 `dp` 长度。纯 `dp` 间距折叠为节点值，含 `px`/`sp`/`em` 的间距随环境在布局时折叠；间距的 `%` 读 Grid 的内容盒——列间距读其宽度，行间距在 Grid 高度确定时读其高度（内容决定高度时按 0 计，与测量一致）。读取值的 `areas`/`grid.area` 报 `E3711`；读取值的其余 Property 随值布局（U3.4）。热重载就地改写保留 Grid 的模板与子节点的单元（只标脏变化的节点），Release 包与宏路径相同。

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
- Stack 的自然尺寸是子节点（含 `margin`）的最大外盒加 `padding`；子节点在内容盒中按自身 `stack.align`（未给出时取 `content_align`）定位。

**Absolute** 子节点 Property：`absolute.top`、`absolute.bottom`、`absolute.start`、`absolute.end`，类型均为 `Option<MixedLength>`、默认 `Option::None`、失效 `LAYOUT`；`percent_basis` 为容器内容盒高度（`top`/`bottom`）或宽度（`start`/`end`）。

- 某轴两端都给出且该轴 Sizing 为 `fit` 时，子节点在该轴上拉伸；两端都未给出时贴 `start`/`top`；
- 子节点不影响容器尺寸，所以 Inset 变化只标记 `LAYOUT`；频繁移动的元素应该用 `translate`（U4），不要逐帧改 Inset；
- Absolute 的自然尺寸只含 `padding`（Sizing 默认 `fill`）；子节点的 Inset 从内容盒的对应边量起，子节点的测量不向容器冒泡，只重新布局该子节点。

当前实现：`content_align`、`stack.align`、`stack.layer` 与 `absolute.*` 是常量时在编译期折叠（值可以是本文件或按名 import 的 `const`）；纯 `dp` 或含 `%` 的 Inset 折叠为节点值，`%` 在布局时读到容器内容盒，含 `px`/`sp`/`em` 的 Inset 随 §19.6 环境在布局时折叠。Paint 与 Hit Test 按层次序（全部子节点同层时不排序），层次序不改变语义与焦点顺序。读取值的上述 Property 随值布局（U3.4）。热重载就地改写保留子节点的对齐、层次与 Inset（只标脏变化的节点：对齐与 Inset 只重新布局父容器，层次只重绘），Release 包与宏路径相同。`start`/`end` 按容器所在的布局方向解析（U10.1）。

### U3.7 Scroll

- `axis: Axis = Axis::column`，失效 `MEASURE|LAYOUT`；`across: Bool = false`（取常量）让视口在 `axis` 之外也横跨滚动：内容两向取自然尺寸（横向至少铺满视口），两向各有滚动条，`VirtualList` 同样接受；
- `scroll_bars: Bool = true`：是否显示滚动条（样式随平台与系统设置，见架构文档“滚动条”）；`tail: Bool = false`：打开时位于内容末端，停在末端期间内容增长时平滑跟随末端，滚离末端后不再跟随、回到末端后恢复（日志、聊天）；两者取常量，`VirtualList` 同样接受；
- 事件 `scroll_changed(ScrollChanged)`，字段为 `offset: Offset`、`viewport: SizeDp`、`content: SizeDp`（`SizeDp { width: Dp; height: Dp; }`）；
- 事件 `refresh`（无载荷）：声明它的纵向视口支持下拉刷新——越过起点下拉到阈值（Apple 为按住时到达，Material 为松手时）或辅助技术的 Refresh 操作即开始刷新并在下一帧投递；`refreshing: Bool`（可绑定，失效 `TRANSFORM|PAINT`）由 Owner 决定刷新是否进行：下拉开始的刷新若在投递后的下一次布局前没有被 `refreshing` 变为 `true` 确认就结束，`refreshing` 变为 `false` 时结束（`on refresh { loading = true; reload(); }`，加载完成时 `loading = false`）；`VirtualList` 同样接受；
- `snap: Snap`（`start; center; end; pages;`，取常量）：视口停靠在其项上——`Scroll` 的项是其内容节点的子节点，`VirtualList` 的项是行；松手、滚轮停转或触控板抬起后滑行到惯性停点最近的项，按 `start`/`center`/`end` 对齐，`pages` 一次轻扫至多移动一项；`snap_inset: MixedLength`（常量 `dp`）让项停在距视口两边该距离处（Carousel 的内边距）；`page_dots: Bool` 在视口横轴末端边居中覆盖一行页点，点亮所停项，点击或方向键移动到对应项；事件 `snap_changed(SelectionChanged)` 在所停、所趋向或最近的项变化时投递其序号 `value`（初始项不投递）；`VirtualList` 同样接受；
- 子节点在滚动轴上的约束无界；滚动偏移变化只标记视口子树 `TRANSFORM`，不触发布局（ADR 0007）；
- 滚轮与触控板事件先送往最内层可在该轴滚动的视口，到达边界后沿祖先链传递；
- 键盘：焦点路由上无处理器取走、焦点又不在可编辑文本框中的方向键、Page Up/Down、Space（Shift+Space）与 Home/End 滚动焦点（无焦点时为指针下节点）所在的最近可滚动视口——一行、一页（保留少量重叠）或到两端，平滑抵达，连按累加；吸附视口逐项移动；标准控件取走自己使用的按键（Slider 的方向键、Toggle 的 Space 等）；
- 水平滚动的视口在 RTL 下以右边为起点：内容右边与视口右边对齐，偏移在 `[-range, 0]` 内取值（0 为起点，向左为负）；方向翻转时偏移保持到起点的距离；
- 命令式滚动只经 NodeRef（U7.6）。

当前实现：`scroll_changed` 在布局之后投递，一帧内多次滚动合并为一次，挂载时不报告；Handler 写入的状态进入下一帧事务。导出页面用被动 `scroll` 监听投递同一载荷。

### U3.8 Property Group 与父节点提供的 Property

多段 Property Path 只在两种情况下合法：

- **Schema 声明的 Property Group**（`semantics.*`、`transition.*`）：每个成员是独立 PropertyId，有自己的类型与失效类别；
- **父节点提供的 Property**（`grid.*`、`stack.*`、`absolute.*`）：由直接父节点的 Schema 声明、写在子节点 Node Body 中。编译器在同一 View Block 内静态确定父节点类型后检查前缀；父节点不提供该前缀，或父节点无法静态确定（例如子节点经 Slot 转发到未知容器、位于用户 Component 的 Default Slot 中），是 `E3702`；
- `Fragment`、`if`、`match`、`for` 不形成父节点，其子节点使用外层真实父节点提供的 Property。

---

## U4. 变换、透明度与裁剪

| Property           | 类型          | 默认                  | 失效                         |
| ------------------ | ------------- | --------------------- | ---------------------------- |
| `translate`        | `Offset`      | `Offset::zero()`      | `TRANSFORM`                  |
| `scale`            | `F32`         | `1.0`                 | `TRANSFORM`                  |
| `rotation`         | `Angle`       | `0deg`                | `TRANSFORM`                  |
| `transform_origin` | `Alignment2D` | `Alignment2D::center` | `TRANSFORM`                  |
| `opacity`          | `F32`         | `1.0`                 | `PAINT`                      |
| `clip`             | `Bool`        | `false`               | `PAINT\|HIT_TEST`            |
| `visible`          | `Bool`        | `true`                | `PAINT\|HIT_TEST\|SEMANTICS` |
| `sticky`           | `Bool`        | `false`               | `TRANSFORM\|PAINT\|HIT_TEST` |
| `hero`             | `Option<String>` | `Option::None`     | 无（下次换页读取）           |

- `Offset { x: MixedLength = 0dp; y: MixedLength = 0dp; }` 不声明 `percent_basis`，含 Percent 分量是 `E3104`；
- 变换在布局之后应用，不改变任何节点的布局尺寸与兄弟位置；Hit Test 使用变换后的几何；`translate.x` 是物理方向，不随 RTL 翻转；
- `opacity` 截断到 `[0, 1]`；`opacity: 0.0` 的节点仍参与 Hit Test 与语义；
- `visible: false` 保留布局空间，但不绘制、不参与命中测试，并从语义树移除；需要同时移除布局空间时使用 `if`；
- `clip: true` 按自身 Border Box 与 `corner_radius` 裁剪子节点的绘制与命中测试；滚动视口总是裁剪。
- `sticky: true` 的节点在最近的滚动视口滚过它时停在视口起点边（纵向为上边，横向为起点边，RTL 为右边），直到父节点的末端把它推走（分组标题随其分组离开）；可绑定。停靠只叠加在节点自身 `translate` 之上，只标记 `TRANSFORM`；停靠中的节点在兄弟之上绘制与命中。`VirtualList` 行根的 `sticky` 不作用于行根本身，而是按 Item 求值，标出各分组的标题行（U9.3）。HTML 导出为 `position: sticky`。
- `hero: "tag"` 标记共享元素：`NavigationStack` 换页时，到达页中正在显示、带某个标签的节点从离开页中同标签节点所在之处飞到自己的位置——起点按两者中心对齐、按宽度之比缩放，平移与缩放补偿其所在页自身的移动，因此在屏幕上直线穿行——与页面移动同一时序；其间离开页的那个节点隐藏。到达页布局之后才起飞；再次换页时进行中的飞行落定、隐藏的节点重新显示；减少动态效果或 `PageTransition::none` 时不飞。页自身也可以是 hero。HTML 导出为 `data-hero`，页面脚本以 Web Animations 飞行。
- `transform_origin` 的 `start`/`end` 随布局方向翻转；`scale`、`rotation` 绕它先缩放后旋转（顺时针），再平移 `translate`。

---

## U5. 文本

`Text` 基线（`TextInput` 共享其中的样式条目）：

| Property      | 类型                  | 默认                          | 失效                                | percent_basis             |
| ------------- | --------------------- | ----------------------------- | ----------------------------------- | ------------------------- |
| `text`        | `String`              | `""`                          | `MEASURE\|LAYOUT\|PAINT\|SEMANTICS` | —                         |
| `font_size`   | `MixedLength`         | `1em`（继承）                 | `MEASURE\|LAYOUT\|PAINT`            | 父节点 Resolved Font Size |
| `spans`       | `List<Span>`          | `[]`                          | `MEASURE\|LAYOUT\|PAINT\|SEMANTICS` | —                         |
| `font_weight` | `FontWeight`          | `FontWeight::regular`         | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `font_style`  | `FontStyle`           | `FontStyle::normal`           | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `font_family` | `Option<FontFamily>`  | `Option::None`（Theme）       | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `line_height` | `Option<MixedLength>` | `Option::None`（字体度量）    | `MEASURE\|LAYOUT\|PAINT`            | 当前 Resolved Font Size   |
| `color`       | `Color`               | `theme.colors.foreground`     | `PAINT`                             | —                         |
| `soft_wrap`   | `Bool`                | `false`                       | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `max_lines`   | `Option<U32>`         | `Option::None`                | `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `overflow`    | `TextOverflow`        | `TextOverflow::clip`          | `LAYOUT\|PAINT`                     | —                         |
| `align`       | `TextAlign`           | `TextAlign::start`            | `LAYOUT\|PAINT`                     | —                         |
| `selectable`  | `Bool`                | `false`                       | `PAINT\|HIT_TEST\|SEMANTICS`        | —                         |
| `locale`      | `Option<Locale>`      | `Option::None`（`env.locale`）| `MEASURE\|LAYOUT\|PAINT`            | —                         |
| `text_shadow` | `List<TextShadow>`    | `[]`                          | `PAINT`                             | —                         |

- `FontWeight { thin; light; regular; medium; semibold; bold; heavy; }`（字重 100、300、400、500、600、700、900）；`FontFamily { system_ui; serif; monospace; named(String); }`（`named` 为 App 字体中的族名，无此族时取主 UI 字体）；`TextOverflow { clip; ellipsis; }`；`TextAlign { start; center; end; }`（`start`/`end` 随段落方向解析，`justify` 推迟）；
- `record TextShadow { x: Dp = 0dp; y: Dp = 1dp; blur: Dp = 2dp; color: Color; inset: Bool = false }`：`text_shadow` 是字形的投影，按 `x`/`y` 偏移、按 `blur`（CSS 模糊半径）淡出，画在文字之下，不参与布局与命中；`inset: true` 时落在字形之内、画在文字之上（被偏移的字形之外的部分着色），即内阴影；多个按书写顺序绘制，`Text`、`Button` 与 `TextInput` 都接受，`TextInput` 的阴影随其文本，不作用于占位文字与光标；HTML 导出为 `text-shadow`，内阴影报导出诊断；
- `FontStyle { normal; italic; }`：`italic` 取该族的斜体字体（有则用之）；
- **富文本**：`spans` 只属于 `Text`，非空时各片段的 `text` 依次拼接成节点的文本（取代 `text`，也是无障碍名称），每个片段按自身样式绘制。`record Span { text: String; color: Option<Color> = None; background: Option<Color> = None; font_weight: Option<FontWeight> = None; font_style: Option<FontStyle> = None; font_family: Option<FontFamily> = None; font_size: Option<MixedLength> = None; raise: Option<MixedLength> = None; underline: Bool = false; strikethrough: Bool = false; link: Option<String> = None; }`：`None` 的字段沿用节点自身的值，`color` 为 `None` 的片段随节点 `color`（及其过渡）；`font_size` 的 `em`/`%` 相对节点字号，`raise` 把片段基线抬高（负值降低），其 `em`/`%` 相对片段自身字号；`background` 填在片段字体的 ascent 到 descent 之间，`underline`/`strikethrough` 的位置与粗细取字体度量。只改颜色、背景与线条的片段不打断整形（字距与连字保持），改字体、字号、字重、倾斜或抬升的片段单独整形；一行的高度取该行各片段行盒的最大值（Text Runtime §12.12.2）。片段读取的 State 变化时整段重新声明。HTML 与 Solid 导出把片段写成带内联样式的 `<span>`，变化的片段由页面脚本重建；
- **链接**：带 `link` 的片段是链接，总有下划线。鼠标在其上显示手形，在同一链接上按下并抬起即激活它，`Text` 及其祖先收到 `link(LinkEvent)`；有链接的 `Text` 可获得焦点，Tab/Shift+Tab 先在其链接间逐个移动再离开，Enter 激活焦点所在的链接，键盘移动焦点时该链接外画焦点环。辅助技术把每个链接读作 `Role::link` 的子节点（名称为链接文字，Click 激活、Focus 聚焦它）。HTML 导出为 `<a href>`，有 `on link` 时点击由页面脚本拦截并投递，没有时按浏览器默认跟随；原生运行时上，激活的链接沿路由没有处理器调用 `prevent_default` 时（`on link` 总会调用）交给系统打开（`Launcher::open_url`：浏览器、邮件等外部应用），否则由作者处理；
- **可选择文本**：`selectable: true` 的 `Text` 可以选择与复制，与 `TextInput` 共用同一套选择机制（Rust `Label::selectable`）：拖动选择，双击选词、三击选段，Shift+按下扩展；触摸长按选词并显示选区手柄与编辑菜单；加速键加 C 复制、加 A 全选；次键单击或触摸选区的菜单只提供复制与全选。它不接受任何输入（键入、粘贴、输入法、剪切、撤销都不改变文本），平台不为它显示软键盘或输入法。它可获得焦点；焦点由键盘带来时外画焦点环并显示光标，方向键移动光标、加 Shift 扩展选区；由指针带来时不显示光标。焦点离开时选区收起。它照常绘制 `spans`、`max_lines` 与 `overflow`。`selectable` 可以随 State 变化，节点就地在可选择与普通文本之间切换。HTML 导出中普通文本为 `user-select: none`，可选择文本加 `vx-selectable` 类恢复浏览器的选择，变化时由页面脚本切换；

```viso
Text {
    spans: [
        Span { text: "Saved " },
        Span { text: "3 files", font_weight: Option::Some(FontWeight::bold) },
        Span { text: "2", font_size: Option::Some(75%), raise: Option::Some(0.5em) },
    ]
}
```

- Schema 默认值列中的 `theme.*` 表示未绑定时读取 Theme Context；
- **Typography Context**（§19.4）：`Text` 声明 `font_size`，设置时成为子树 Resolved Font Size 的来源；容器不声明 `font_size`，只向下传递；根节点取 `theme.typography.base_size`（默认 `14sp`）；`font_size` 变化按 §19.6 追加标记依赖当前 Resolved Font Size 的后代 Binding；
- 源文本始终为逻辑顺序；Bidi、断行、字形选择与回退由 Text Runtime 负责，DSL 不得反转字符串或手工插入方向控制符；
- 仅因宽度变化引起的重排不标记 `SEMANTICS`（ADR 0027）；
- `overflow: ellipsis` 仅在 `max_lines` 有值或 `soft_wrap: false` 时生效；
- `max_lines` 为 `Option::Some(0)` 时按 `1` 处理；`TextInput` 是编辑框（`multiline` 决定单行或多行），取 `font_weight`、`font_style`、`font_family`、`line_height` 与 `align`，不取 `max_lines`/`overflow`（`E3711`）；`Button` 的文本取全部文本样式；

- 当前实现：上述样式（常量或读取值）随节点的值条目送达其文本请求，热重载就地改写，Release 包与宏路径相同；`line_height` 的 `em`/`%` 读节点解析出的字号。行盒、行数上限、省略号与对齐的算法见 Text Runtime §12.12.1；`Fit` 宽度的文本按其 max-content 宽度测量，被 `max_width` 等上下界或父容器收窄时按盒宽换行或加省略号。未设置 `font_family` 的文本取当前 Theme 的 `typography.family`，切换 Theme 时只有这些文本重新塑形；`FontFamily::named` 暂只查找应用打包的字体；
- `text` 在 Schema 中标记为 Localizable（U10.3）。
- `String` 文本槽（`text`、`label`、`placeholder` 等）只接受 `String`（或 `Option<String>` 槽的 U1.3 提升），不做隐式 `Display` 转换：`text: count;`（`count: I64`）是 `E2103`，并附 Machine-Applicable Fix `"{count}"`；对无 `Display` 的类型不给出该 Fix。

### U5.1 图像

包的资源放在 `Viso.toml` 旁的 `assets/` 目录中。`asset("…")` 以字符串字面量命名其中的一个文件：路径相对 `assets/`，以 `/` 分隔（`\` 视同 `/`），空段与 `.` 段被规范化掉，绝对路径与 `..` 不允许。编译期检查该文件存在，并按其字节（而非扩展名）判定格式：位图（PNG、JPEG、GIF、WebP、AVIF、BMP、ICO、QOI）得到 `ImageAsset`，SVG 得到 `SvgAsset`，视频（MP4、QuickTime、WebM、Matroska）得到 `VideoAsset`。这些 Prelude Record 只由 `asset` 产生；作为普通值，它们可存入 State、在 `if`/`match` 中选择、作为 Input 传递。资源的身份是规范化路径的 64 位哈希，同一文件的不同拼写是同一资源；编译结果列出包用到的全部资源，Release 包只携带这些文件。

| Widget | Property（类型 = 默认；失效） |
| ------ | ----------------------------- |
| `Image` | `source: ImageAsset`（未绑定时不绘制，固有尺寸为零；`MEASURE\|LAYOUT\|PAINT`）；`url: String = ""`（非空时代替 `source`：图像由应用的 `Application::image_loader` 取回字节后同样解码，同一 URL 是同一资源；没有 loader 或取回失败即 `failed`；HTML 导出为 `<img src>`；`MEASURE\|LAYOUT\|PAINT`）；`fit: ImageFit = ImageFit::contain`（`PAINT`）；`tint: Option<Color> = Option::None`（与像素逐通道相乘；`PAINT`）；`animate: Bool = true`（动画图片——GIF、APNG、动画 WebP、AVIF 序列——是否播放；不在屏幕上或系统减少动态时总是停在当前帧；`false` 停在首帧；`PAINT`） |
| `Svg`   | `source: SvgAsset`（同上）；`fit`、`tint` 同 `Image`（`tint` 与每条路径的填充、描边颜色相乘，白色保持原色，可为单色图标着色） |
| `Video` | `source: Option<VideoAsset> = Option::None`（未绑定且无 `url` 时不绘制；`MEASURE\|LAYOUT\|PAINT`）；`url: String = ""`（非空时代替 `source` 播放：含 `://` 的由平台播放器自行拉取，含 HLS 播放列表，否则为文件路径）；`fit`、`tint` 同 `Image`；`playing: Bool = false`（双向：播放到末尾停下时写回 `false`）；`looping: Bool = false`；`muted: Bool = false`；`volume: F32 = 1.0`（0 到 1）；`rate: F32 = 1.0`（倍速）；`controls: Bool = false`（在画面上显示框架自带的控制条）；`fullscreen: Bool = false`（双向：全屏显示该节点；控制条、按键或窗口退出全屏时写回）；固有尺寸为视频画面的像素尺寸 |
| `CameraView` | `facing: Option<CameraFacing> = Option::None`（`CameraFacing::front` / `back` / `external`：取第一个朝向如此的摄像头，没有则取平台默认）；`device: String = ""`（非空时按平台给的设备 ID 选摄像头，优先于 `facing`）；`fit: ImageFit = ImageFit::cover`；`tint` 同 `Image`；`playing: Bool = true`（双向；暂停时释放设备，画面停在最后一帧）；`mirror: Option<Bool> = Option::None`（未设时朝向用户的摄像头左右镜像显示）；固有尺寸为摄像头画面的像素尺寸 |
| `GameViewport` | `camera: Option<CameraRig> = Option::None`（视口自己的相机，缺省用 Stage 的相机；`PAINT`）；无固有尺寸，未写宽高的轴为 Fill；可获得焦点（§106.2） |

```viso
state dark = false

view {
    Row {
        Image(if dark { asset("images/logo_dark.png") } else { asset("images/logo.png") }) {
            width: 48dp
        }
        Svg(asset("icons/close.svg")) {
            tint: theme.colors.foreground
            width: 16dp
            height: 16dp
        }
    }
}
```

- `ImageFit { contain; cover; fill; tile; }`：`contain` 保持比例完整放入盒子并居中；`cover` 保持比例铺满盒子，居中裁去溢出部分；`fill` 拉伸到盒子；`tile` 以固有尺寸从盒子左上角起重复铺满（图案；栅格图一次绘制、采样器 repeat，矢量图每格一份并裁到盒子；HTML 导出为重复的 `background-image`，有 `tint` 时为重复的 mask；`<img>` 不能重复，fit 在页面上才变成 `tile` 时显示一份固有尺寸的图）；
- 固有尺寸是图像的像素尺寸（SVG 取其 `viewBox` 与 `width`/`height`），1 像素计为 `1dp`；布局属性给出的尺寸优先；
- 解码不在帧路径上：解码完成前节点不绘制，已知尺寸即参与布局；资源的新内容（如热重载）解码并驻留前旧内容继续绘制，之后只脏显示该资源的节点：尺寸不变为 `PAINT`，改变为 `MEASURE|LAYOUT|PAINT`；
- 以下情形报 `E3713`：`asset` 不是恰好一个参数；参数不是不含插值的字符串字面量（要在资源间选择，在 `asset("…")` 值之间分支）；路径越出 `assets/`；文件不存在（附最接近的已有路径作为 Fix）；文件不是位图、SVG 或视频；本次编译没有资源目录；以及 `ImageAsset`/`SvgAsset`/`VideoAsset` 的 Record 表达式；
- 把 `SvgAsset` 绑到 `Image` 的 `source`、`ImageAsset` 绑到 `Svg` 的 `source`，或绑定其他类型的值，是 `E2103`；
- `Image`/`Svg` 的事件 `loaded(SizeDp)` 在来源绘制出来后投递其固有尺寸（换为另一来源、或同一来源的新修订以另一尺寸绘出时再次投递），`failed` 在来源读不到或无法解码时投递；两者都在布局之后投递，Handler 写入的状态进入下一帧事务；
- `Video` 经各平台自带的播放器解码、出声并保持音画同步（AVFoundation、Media Foundation、GStreamer、MediaCodec、浏览器的 `<video>`），平台播放不了的格式报 `failed`；打开时停在首帧，首帧显示时投递 `loaded(SizeDp)`；`progress(VideoProgress)` 在就绪、播放与暂停、等待数据变化时投递，播放中每秒约四次投递位置（`VideoProgress { position: Duration; duration: Option<Duration>; playing: Bool; waiting: Bool; }`，直播流的 `duration` 为 `None`）；`ended` 在播到末尾停下时投递（循环时不投递）；`playing_changed(Bool)` 在播放状态改变时投递并写回双向的 `playing`；`fullscreen_changed(Bool)` 在进入或退出全屏时投递并写回双向的 `fullscreen`。同一节点上 `source`/`url` 不变时视频保持其播放位置，换来源则从头开始；只有 `playing` 等属性的值变化才会重新要求播放器，播放器自己停下（播完）的状态不会被无关的重建打回。正在播放且在屏幕上可见时每帧取新画面写入同一个 texture，只重绘该节点；移出屏幕继续播放（声音不断）但不再取画面；
- HTML 导出为 `<video class="vx-video" playsinline preload="metadata">`，`playing`、`looping`、`muted` 为 `autoplay`、`loop`、`muted` 属性，`volume`、`rate` 与变化的值由页面脚本设置；浏览器通常只允许静音的视频自动播放；`tint` 不能导出；
- `controls: true` 的 `Video` 在画面上叠一层控制部件：暂停时居中的大播放图标、等待数据时的 Spinner，底部一条控制栏——进度条（拖动即 seek）之下依次是播放/暂停、静音、音量滑块、`已播 / 总长` 时间与全屏按钮。播放中且无指针移动、按键或拖动约 2.5 秒后控制栏淡出，指针移动、按键、暂停或播完时淡入；淡出时按在控制栏上只会唤回它。鼠标单击画面播放/暂停、双击进入/退出全屏；触摸单击显示/隐藏控制栏，双击画面前半/后半后退/前进 10 秒。焦点在任一部件上时：Space/K 播放暂停，←/→ 5 秒（J/L 10 秒），↑/↓ 音量，Home/End 首尾，数字键跳到十分之几处，M 静音，F 全屏，Esc 退出全屏。各按钮是以当前动作命名的 `button`，进度条与音量是 `slider`（进度条的值为已播秒数）。部件只读播放器的报告、只发播放请求，画面仍是视频节点自己的；
- 全屏（`fullscreen` 或控制条）把节点作为覆盖整个窗口的层显示在黑底上，焦点留在其中，并请求窗口全屏（窗口已全屏时不动它）；退出时节点回到原位，窗口恢复原状；窗口自行退出全屏（系统按钮、手势）或节点被释放时同样退出。窗口总是铺满屏幕的平台上节点仍覆盖窗口；导出的页面只在读者手势中进入全屏；
- `CameraView` 是摄像头的实时 `Video`：以 1280×720、30 帧为目标取摄像头最接近的模式，首帧显示时投递 `loaded(SizeDp)`，打不开（没有这个摄像头、用户拒绝授权）时投递 `failed`，`playing_changed(Bool)` 写回双向的 `playing`；没有时长与 seek，不出声。换 `facing` / `device` 重新打开摄像头，其余属性变化不重开；
- `Image`/`Svg`/`Video`/`CameraView` 有 `semantics.label` 时语义角色为 `Role::image`，以该标签为替代文本；没有标签的图像是装饰，不被辅助技术朗读；
- Rust 代码以 `viso::asset!("…")` 取得同一资源（同样的路径检查与 `E3713`，字节同样嵌入），由 `viso::widgets::image_asset` 显示，视频由 `viso::widgets::video` 播放（也接受 `VideoSource::file` / `url` / `bytes`）；`Video::controls` 显示同一套控制部件（`controls_style` / `controls_labels` 定制外观与无障碍名称），`Video::handle` 交出的 `VideoHandle` 在 Handler 中 `play` / `pause` / `toggle` / `seek` / `set_fullscreen` 并读取 `status`；摄像头由 `viso::widgets::camera_view(Camera::facing(CameraFacing::Front))` 显示（`Camera::id` 取 `viso::cameras()` 列出的某个，`.size(w, h)` / `.frame_rate(n)` 定目标模式，`mirror` / `playing` / `placeholder` / `error` 同上），`Video::mirror` 对任意视频生效。

---

## U6. 动画与转场

### U6.1 `transition` Property Group

```viso
export record Transition {
    duration: Duration = 200ms
    delay: Duration = 0ms
    easing: Easing = Easing::ease_out
    reduced: ReducedMotion = ReducedMotion::instant
}

export enum Easing {
    linear
    ease_in
    ease_out
    ease_in_out
    in_quad
    out_quad
    in_out_quad
    in_cubic
    out_cubic
    in_out_cubic
    in_quart
    out_quart
    in_out_quart
    in_quint
    out_quint
    in_out_quint
    in_sine
    out_sine
    in_out_sine
    in_expo
    out_expo
    in_out_expo
    in_circ
    out_circ
    in_out_circ
    in_back
    out_back
    in_out_back
    in_elastic
    out_elastic
    in_out_elastic
    in_bounce
    out_bounce
    in_out_bounce
    cubic_bezier(F32, F32, F32, F32)
    spring(F32, F32, F32)
}
export enum ReducedMotion { instant; keep }
```

```viso
Row {
    background: if selected { theme.colors.accent } else { theme.colors.surface }
    translate: Offset { x: 0dp, y: lift }
    transition.background: Transition { duration: theme.motion.short }
    transition.translate: Transition { duration: theme.motion.medium, easing: ease_in_out }
}
```

- `transition.p` 作用于同一节点的 Property `p`；`p` 必须在 Schema 中标记为 Animatable，且值类型必须是 `Transition`，否则 `E3703`；
- 挂载后 `p` 的解析值变化才触发动画；首次挂载与 `if`/`for` 新挂载的节点直接呈现目标值；
- Model 值立即变化并始终是事实来源，呈现值在 `delay` 内保持起点，随后在 `duration` 内插值；进行中再次变化时从当前呈现值重新出发（Retarget），目标不变的再次求值不重启；
- `Color` 值（`background`、`color`）在预乘的线性光空间逐分量插值，从透明淡入时保持色相；
- `Easing` 把移动的归一化时间映射为进度：`in_*` 从静止加速、`out_*` 减速到静止、`in_out_*` 两端都慢，十个族为 Penner 曲线（`quad`、`cubic`、`quart`、`quint`、`sine`、`expo`、`circ`、`back`、`elastic`、`bounce`），`ease_in`/`ease_out`/`ease_in_out` 即 `cubic` 族；`cubic_bezier(x1, y1, x2, y2)` 为 CSS 三次贝塞尔，`x` 取 `[0, 1]` 内；`back`、`elastic` 与 `y` 越出 `[0, 1]` 的贝塞尔会越过目标再回来：`translate`、`scale`、`rotation` 随之越过，颜色与 `opacity` 停在取值范围内，`width`、`height`、`corner_radius` 不小于 0；
- `spring(stiffness, damping, mass)` 让值像弹簧上的质点一样移向目标，不使用 `duration`：从当前呈现值、以当时的速度出发（进行中的移动被转向时速度延续，缓动中的移动转为弹簧时同样带上其速度），`damping` 低于临界值 `2√(stiffness·mass)` 时越过目标并来回振荡；每个分量的剩余能量足以移动的距离都小于静止阈值（`translate`/`width`/`height` 为 0.01dp，其余为千分之一）时停在目标上，此后不再越出。非有限或不大于 0 的 `stiffness`、`mass` 取默认弹簧的值，`damping` 至少为临界值的百分之一。Web 导出把弹簧从静止到收敛在千分之一以内的移动采样为 `linear()`，以收敛所需时间作为时长（CSS 过渡不能延续速度）；
- 变换类 Property 的 Hit Test 使用呈现值；其他 Property 的 Hit Test 与语义使用 Model 值；
- `transition.*` 自身的变化只影响下一次触发，不标记失效。

### U6.2 可动画 Property 与逐帧失效

| Property                                          | 逐帧失效                     |
| ------------------------------------------------- | ---------------------------- |
| `translate`、`scale`、`rotation`                  | `TRANSFORM`                  |
| `opacity`、`background`、`color`、`corner_radius` | `PAINT`                      |
| `width`、`height`（仅 `fixed` 值之间）            | `MEASURE\|LAYOUT`            |

对 `width`/`height` 设置 transition 合法但每帧重新布局；`viso check` 对 `VirtualList` Item Template 中的这类 transition 报告性能提示。其他 Property 不可动画。

当前实现：`translate`、`scale`、`rotation`、`opacity`、`background`、`color`、`corner_radius`、`width`、`height` 的 transition 在 `component!`、`view!`、Hot Reload 与 Release 包中播放（Web 导出为 CSS `transition`：`linear` 与 `cubic_bezier` 为同名 CSS 函数，其他曲线采样为误差不超过 0.2% 的 CSS `linear()`）：长度在开始时按节点的环境与字号折叠，移动途中环境变化时转向新的折叠值而不中断；文本重新排版送达的新颜色不打断颜色的移动。`scale`/`rotation` 绕 `transform_origin` 作用于节点及其子树：渲染器按每个变换根的仿射 Place 绘制、命中测试把指针映射回节点局部坐标；Web 导出为 CSS `scale`/`rotate`/`transform-origin`，`start`/`end` 原点在 `:dir(rtl)` 下取另一侧。

### U6.3 `Animation` 句柄

命令式动画经 Named Node 的 NodeRef 发起（U7.6），返回 UI Handle：

```viso
export enum Animate { translate(Offset); scale(F32); rotation(Angle); opacity(F32) }
export record Keyframe { at: F32; value: Animate; easing: Easing = Easing::linear }
export enum Repeat { times(U32); forever }
export enum PlayDirection { normal; reverse; alternate; alternate_reverse }
export record Keyframes {
    stops: List<Keyframe>
    duration: Duration = 200ms
    delay: Duration = 0ms
    repeat: Repeat = Repeat::times(1)
    direction: PlayDirection = PlayDirection::normal
    reduced: ReducedMotion = ReducedMotion::instant
}

// NodeRef<T>：fn animate(self, target: Animate, spec: Transition) -> Animation;
//            fn play(self, track: Keyframes) -> Animation;
// Animation：fn cancel(self); fn finish(self); fn is_running(self) -> Bool;

component Shake {
    state shaking: Option<Animation> = Option::None

    action shake() {
        let spec = Transition { duration: 80ms, easing: ease_in_out }
        shaking = Option::Some(card.animate(Animate::translate(Offset { x: 8dp, y: 0dp }), spec))
    }

    view {
        node card: Column {
            on animation_end(e) { if e.finished { shaking = Option::None } }
        }
    }
}
```

- `animate` 与 `play` 只能在 Action、Event Handler 与 `start` Handler 中调用；基线目标只含变换类与 `opacity`，因此命令式动画永不触发布局；
- `play` 让呈现值逐周期经过 `track` 的各个停靠点：停靠点按 `at`（周期的份额，夹在 `[0, 1]`）排序，每个以自己的 `easing` 从前一个停靠点移入；第一个停靠点在 `0` 之后时从当前呈现值出发，最后一个在 `1` 之前时保持其值到周期结束。只保留与第一个停靠点同一目标（`Animate` 变体）且 `at` 有限的停靠点。`repeat: Repeat::times(n)` 运行 `n` 个周期（`0` 立即以 `finished: true` 结束），`Repeat::forever` 一直运行到被取消、`finish()` 或被新动画取代；`direction` 为 `normal`、`reverse`（周期倒放，曲线随之倒放）、`alternate`（正、反交替）、`alternate_reverse`（反、正交替）。结束时 `animation_end` 的 `target` 是最后一个停靠点的值；停靠点的 `easing` 取 `spring` 时按 `ease_out` 处理；
- 结束或取消时目标节点收到 `animation_end(AnimationEnd)`，字段为 `target: Animate`、`finished: Bool`（被 `cancel()` 或新动画打断时为 `false`）；
- 结束后呈现值回到该 Property 当前的 Model 值；需要保持终值时在 `animation_end` 中写入 State，或直接使用 U6.1 Transition；
- 同一节点同一目标上的新动画取消旧动画；
- `Animation` 与 `NodeRef` 生命周期相同（§83）：节点 Dispose 或热重载后失效，对失效句柄调用是 No-op，Debug Runtime 报告警告。

当前实现：`animate` 在调用处即返回 `Animation`（`is_running()` 立即为 `true`），请求在事务提交、结构稳定后作用于节点；动画移动的是节点的呈现值，期间对该 Property 的 Model 写入（Binding、Transition）记入 Model，结束时呈现。`cancel()` / `finish()` 在下一次结构稳定时生效，`is_running()` 计入本次调用已排队的请求。`animation_end` 在结束所在帧的布局之后派发，`target` 为折叠后的值（长度为 `dp`）。节点 Dispose 时动画随之结束且不派发事件。Web 导出为 Web Animations（`fill: none`），结束派发同一事件；`play` 的停靠点成为 Web Animations 的关键帧（各自的缓动移到前一个关键帧上，隐含的起点取当时的计算样式），`repeat`、`direction` 成为 `iterations`、`direction`；`forever` 的轨道被 `finish()` 时停在当前处并以 `finished: true` 结束。Debug Runtime 的失效句柄警告尚未实现。

### U6.4 Reduced Motion

`env.reduced_motion == true` 时，运行时按 `Transition.reduced` 与 `Keyframes.reduced` 处理 U6.1 与 U6.3 的动画：`instant` 跳过插值直接呈现终值，仍派发 `finished: true` 的 `animation_end`；`keep` 照常播放，只用于传达信息的必要动画（如进度指示）。作者无需读取 `env.reduced_motion` 来关闭动画，只有在需要改变动画之外的呈现时才读取。带 `smooth: true` 的滚动方法（U7.6）此时直接跳到目标；手指拖动、惯性滚动与松手后的吸附仍照常运动。

当前实现：平台的减少动态效果设置（macOS/iOS 辅助功能、Android 动画缩放、Windows 客户区动画、GNOME `enable-animations`、Web `prefers-reduced-motion`）同时写入 `env.reduced_motion` 与窗口 `NodeStore` 的 `set_reduced_motion`，策略只在 Store 一处执行：弹簧与关键帧同样适用；设置在动画进行中打开时，可省略的转场立即落到终值、动画以 `finished: true` 结束、程序触发的滚动滑行立即到位。Rust 控件自身的过渡（滚动条淡入淡出、下拉刷新指示器、Sheet 滑入滑出）同样遵循；旋转器保持转动。Web 导出以 `prefers-reduced-motion` 判断：转场与动画时长为 0，`smooth` 滚动用 `behavior: "instant"`。

---

## U7. 输入、手势与焦点

### U7.1 标准事件

以下事件对所有布局节点可用。声明 Handler 即在该节点注册对应命中测试或识别器；未声明的节点不参与该类输入。

| Event                                          | Payload          | 默认 Phase | 冒泡     | 来源                                             |
| ---------------------------------------------- | ---------------- | ---------- | -------- | ------------------------------------------------ |
| `click`                                        | `ClickEvent`     | Bubble     | 是       | 激活：指针轻点、焦点上 Enter/Space、辅助技术 Click |
| `tap`                                          | `TapEvent`       | Bubble     | 是       | 手势竞技场 Tap（仅指针）                         |
| `double_tap`                                   | `TapEvent`       | Bubble     | 是       | 手势竞技场 双击（仅指针）                        |
| `long_press`                                   | `LongPressEvent` | Bubble     | 是       | 手势竞技场                                       |
| `drag_start` / `drag_move` / `drag_end`        | `DragEvent`      | Bubble     | 是       | 手势竞技场 Pan                                   |
| `swipe`                                        | `SwipeEvent`     | Bubble     | 是       | 手势竞技场 Pan，快速抬起                         |
| `pinch_start` / `pinch_move` / `pinch_end`     | `PinchEvent`     | Bubble     | 是       | 手势竞技场 双指捏合与旋转、触控板、Control+滚轮  |
| `pointer_down` / `_move` / `_up` / `_cancel`   | `PointerEvent`   | Bubble     | 是       | 原始指针                                         |
| `hover_enter` / `hover_leave`                  | `HoverEvent`     | Target     | 否       | ADR 0022                                         |
| `scroll`                                       | `ScrollEvent`    | Bubble     | 按轴传递 | 滚轮与触控板（ADR 0007）                         |
| `key_down` / `key_up`                          | `KeyEvent`       | Bubble     | 沿焦点链 | 焦点节点                                         |
| `focus` / `blur`                               | `FocusEvent`     | Target     | 否       | 焦点系统                                         |
| `link`                                         | `LinkEvent`      | Bubble     | 是       | 文本的链接被点击或在焦点上按 Enter、辅助技术 Click |

Payload 基线字段：

- `PointerEvent`：`position: Point`（节点局部 dp）、`button: PointerButton`（`primary; secondary; middle;`）、`buttons: PointerButtons`、`pointer_kind: PointerKind`（`mouse; touch; pen;`）、`modifiers: Modifiers`（`shift`、`control`、`alt`、`logo: Bool`）；
- `KeyEvent`：`key: Key`、`repeat: Bool`、`modifiers: Modifiers`；文本输入与 IME 组字不经 `key_down`，由 `TextInput` 内部处理（CLAUDE.md §13）；
- `DragEvent`：`position: Point`、`delta: Offset`、`total: Offset`、`velocity: Offset`（每秒的位移，按最近 40ms 的样本；`drag_end` 为抬起时的速度，取消时为零）；
- `SwipeEvent`：`direction: SwipeDirection`（`left; right; up; down;`，窗口坐标轴）、`position: Point`、`velocity: Offset`；
- `PinchEvent`：`position: Point`（焦点：两指中点，触控板与滚轮为指针处）、`scale: F32` 与 `rotation: Angle`（顺时针）为自两指落下以来的缩放与旋转，`scale_delta: F32`、`rotation_delta: Angle` 与 `pan: Offset`（焦点的位移）为自上一样本以来的；
- `ClickEvent`：`position: Option<Point>`（键盘与辅助技术激活时为 `Option::None`）、`modifiers: Modifiers`；
- `TapEvent`、`LongPressEvent`、`HoverEvent`：`position: Point`、`pointer_kind: PointerKind`；
- `ScrollEvent`：`delta: Offset`、`modifiers: Modifiers`；
- `FocusEvent`：`focus_visible: Bool`（焦点由键盘移动时为 `true`）；
- `LinkEvent`：`url: String`（被激活的 `Span` 的 `link`），先投递给持有该链接的 `Text`，再沿其祖先冒泡；
- `Point { x: Dp; y: Dp; }`；`PointerButtons { primary; secondary; middle: Bool }`；`Key` 为 `char(Char); enter; escape; tab; backspace; delete; space; arrow_up; arrow_down; arrow_left; arrow_right; home; end; page_up; page_down; function(U8); unidentified;`。

以上类型、U6.3 的 `Animate`/`AnimationEnd` 与 U7.5 的 Widget 事件 Payload 由隐式 Prelude 导出（§23）。

规则：

- 分派顺序 Capture → Target → Bubble → Default Action（§52），可用 `event.stop_propagation()` 与 `event.prevent_default()` 中断；
- `tap`、`double_tap`、`long_press`、`drag_*`、`swipe`、`pinch_*` 在手势竞技场竞争，胜者独占该指针序列；`click` 不参与竞争，但同一节点上 `drag` 胜出时不产生 `click`；
- `hover_*` 不是语义事件：触摸设备上不触发，指针 Capture 期间被抑制；任何功能不得只依赖 hover（U8.2）；
- Handler 在 Action Transaction 中运行，可以 `start` Task；Task 随节点子树 Dispose 取消（ADR 0032）。

当前实现：`drag_*` 与 `long_press` 由运行时投递。主按钮按下时，按下节点及其祖先中最近的声明了 `drag_*` 的节点武装一次拖动；指针离按下点超过阈值（鼠标 4dp，触摸 8dp）时拖动开始：该节点获得指针 Capture，按下被取消（抬起不产生 `click`/`tap`），先收到 `drag_start`，此后每次移动收到 `drag_move`，抬起或取消时收到 `drag_end`，期间处于 `dragging`。触摸落点与拖动节点之间有滚动视口时，视口先仲裁：可平移则平移胜出、拖动作废，无可平移余量时拖动胜出；拖动节点在视口内（或就是视口）时拖动胜出。节点的 `drag_axis: Option<Axis>`（默认 `none`，失效 `HIT_TEST`）让拖动只沿该轴：越过阈值时位移主要沿另一轴的按下不成为拖动，交给其上可平移的视口（列表里左右滑动的行上下滑动仍滚动列表）；Web 导出为 `data-drag-axis`，触摸的 `touch-action` 留给浏览器沿另一轴平移。触摸平移在越过阈值时按主要方向定轴，此后只移动该轴上的视口，嵌套的另一轴视口不随手指斜移而动。`position` 为分派节点的局部坐标，`delta` 是相对上一个拖动样本的位移（`drag_start` 为相对按下点），`total` 是相对按下点的位移，均沿窗口坐标轴。主按钮按下还为最近的声明了 `long_press` 的节点武装一次长按：按下保持 500ms、期间未移出阈值也未抬起时，该节点收到 `long_press`（`position` 为按下点），按下被取消（抬起不产生点击），已武装的拖动作废，触摸指针 Capture 到该节点（此后不再平移）。Web 导出以 Pointer Events 实现两者：拖动开始时 `setPointerCapture`，节点带 `data-viso-dragging`；拖动或长按之后的那次指针点击被吞掉。声明了 `double_tap` 的节点为按在其中的主按钮按下计数连击：距上一次按下不超过系统双击时间（macOS `NSEvent.doubleClickInterval`、Windows `GetDoubleClickTime`、GNOME/KDE 设置门户，取不到时 500ms）、相距不超过阈值的按下接续同一串，第二次按下的抬起派发 `double_tap`（`position` 为抬起点）。同一节点同时声明 `tap` 时，`tap` 等待：单击抬起后等满双击时间，期间没有第二次按下才派发 `tap`，第二次按下取消它；只声明 `tap` 的节点立即派发。等待中的 `tap` 不使节点可聚焦。Web 导出以 `dblclick` 实现 `double_tap`，同时声明 `tap` 时单击（`detail` 为 1）推迟 500ms 派发、再次点击取消。`swipe` 是一次拖动：声明它的节点与声明 `drag_*` 的一样武装拖动，拖动抬起时沿速度较大的轴不低于 400dp/s、且与该轴上的总位移同向，节点在 `drag_end` 之后收到 `swipe`；取消的拖动不是轻扫。Web 导出按指针事件的 `timeStamp` 计算同样的速度与判定。声明 `pinch_*` 的节点接受捏合：落在其中或其下的两根手指一同越过阈值（两指间距变化、绕中点转过的弧长或中点位移超过触摸阈值）时捏合开始，节点获得两指的 Capture，两指的按下被取消（抬起不产生点击）、长按放弃、已开始的拖动以取消结束、滚动视口不再平移它们；此后任一手指移动都是一个 `pinch_move`，任一手指抬起或取消时 `pinch_end`。第一根手指已在平移视口时不武装捏合。触控板的捏合与旋转（macOS `magnifyWithEvent:`/`rotateWithEvent:`、Wayland `zwp_pointer_gestures_v1`、X11 XInput 2.4、Safari `gesture*` 事件）交给指针下最近的接受捏合的节点；按住 Control 转动滚轮（浏览器与 Windows 精确式触控板以此报告捏合）在这样的节点上缩放（内容每下移 100dp 缩小为 1/e），停止 150ms 后 `pinch_end`，没有这样的节点时照常滚动。Web 导出以两个触摸指针、Safari `gesture*` 事件与带 `ctrlKey` 的 `wheel` 实现同样的判定。

### U7.2 `focusable` 与焦点顺序

| Property    | 类型   | 默认                           | 失效                         |
| ----------- | ------ | ------------------------------ | ---------------------------- |
| `focusable` | `Bool` | `false`；标准交互 Widget 为 `true` | `SEMANTICS`              |
| `autofocus` | `Bool` | `false`                        | —（仅挂载时读取）            |
| `focus_ring` | `Bool` | `true`                        | `PAINT`                      |
| `access_key` | `String` | 无                          | `PAINT\|SEMANTICS`           |
| `enabled`   | `Bool` | `true`（交互 Widget）          | `STYLE\|SEMANTICS`           |

- 焦点顺序是 Focus Scope 内可聚焦节点的**源码先序顺序**，不随 RTL 翻转，也不受 `stack.layer`、`translate` 影响；
- 不提供正数 `tab_index`，需要调整顺序时调整源码结构；
- Tab / Shift+Tab 在当前 Scope 内前后移动；组合 Widget（`Tabs`、`RadioGroup`、`SegmentedControl`、`VirtualList`）内部用方向键漫游，对外只占一个 Tab 停靠点；
- `enabled: false` 的节点不可聚焦、不接收指针与键盘事件，语义上暴露为 disabled，并激活 `disabled` 选择器；
- 焦点移动时旧节点与新节点都标记 `PAINT|SEMANTICS`（ADR 0030）；
- 键盘或辅助技术移来的焦点（`focus_visible`）由框架在节点外画焦点环，指针按下得到的焦点不画；环沿节点盒子外缘、按其圆角放大，画在其子树之上、自身裁剪之外，随其平移与旋转；样式随平台：Apple 为强调色半透明光晕、从外 6dp 处 200 ms 收拢淡入，Windows 为 2px 前景色加 1px 背景色的双色描边，Linux 为半透明强调色 2px 描边、200 ms 淡入，Android 为 Material 3dp 指示框、外移 2dp，Web 运行时为浏览器式 2px 描边；强调色取系统设置（macOS `controlAccentColor`、Windows 个性化强调色、XDG portal `accent-color`），减少动态效果时立即出现；文本框显示光标而不画环，带链接的文字环绕聚焦的链接；`focus_ring: false` 的节点自行表现焦点（例如 `when focus_visible`），不画框架的环。Web 导出由浏览器的 `:focus-visible` 轮廓表现，`focus_ring: false` 导出为 `outline: none`。
- `access_key` 的首个字符是节点的访问键（不分大小写）：非 Apple 系统上，焦点链没有处理的 Alt+该键送到树序中（焦点之后的）下一个可见、可用、在焦点作用域内持有它的节点——`Button` 收到 `click`，`CheckBox`/`Toggle` 获得焦点并翻转，其他可聚焦节点获得焦点（焦点环照常显示）；节点文本（`Button`/`Text` 的 `text` 与 `spans`、`CheckBox`/`Toggle` 的 `label`）中首个该字母在 Alt 按住期间显示下划线，松开或窗口失去焦点时消失；多个节点同键时依次轮转；Apple 系统上 Option 用于输入字符，访问键不生效、不显示下划线。辅助技术读到 `Alt+键` 快捷键（AccessKit `access_key`、Web `aria-keyshortcuts`）。HTML 导出为 `accesskey` 属性（由浏览器按其修饰键激活，不画下划线）。

`Button` 默认可聚焦，焦点在其上时按 Space 或 Enter 收到 `click`（位置为节点中点，修饰键为按下时的），与指针轻点相同。

当前实现：`enabled` 与 `autofocus` 可为常量或读取值，宏路径、热重载与 Release 包相同。`enabled: false` 使节点及其子树 inert：命中落在其中的指针事件不分派给任何节点（包括祖先，按下不会穿透到外层可点击节点），也不进入 `hover`/`pressed` 状态；指针离开时仍收到 `hover_leave`；滚动照常传给外层滚动容器。inert 节点不可聚焦，Tab 跳过整个子树，辅助技术的动作不作用于它；禁用时焦点或按下位于子树内则被清除。语义树中该节点与其子树标为 disabled（AccessKit `disabled`、Web `aria-disabled`、iOS `NotEnabled`），不暴露动作。节点自身的 `enabled` 仍决定 `disabled` 选择器。`autofocus` 只在节点首次挂载时读取：此时没有节点持有焦点则聚焦它（源码先序中第一个生效），热重载重新挂载不再触发。HTML 导出：禁用的元素带 `data-viso-off` 与 `aria-disabled`（原生控件另加 `disabled`），运行时丢弃目标位于其中的事件并收回子树的 Tab 停靠点；`autofocus` 导出为同名属性，读取值时在挂载后、无焦点时聚焦。

### U7.3 Focus Scope

`FocusScope` 是不参与布局的包装节点，有两个 Property：`trap: Bool = false`（为真时 Tab 在 Scope 内循环，焦点不能移出）与 `restore_focus: Bool = true`（Scope Dispose 后焦点回到进入 Scope 前的节点）。两者只在焦点移动时读取，不标记失效。`Modal`、`Sheet`、`Popup` 内置 `trap: true` 的 Scope。

### U7.4 键盘快捷键

```viso
KeyShortcut {
    chord: KeyChord { key: Key::char('s'), primary: true }
    on triggered { save() }
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
| `Button`     | `text: String = ""`（Localizable；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；U5 的文本样式条目 | `click`                |
| `Toggle`     | `checked: Bool = false`（two_way；`PAINT\|SEMANTICS`）；`label: String = ""`（Localizable；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`） | `changed(value: Bool)` |
| `CheckBox`   | 同 `Toggle`                                                                                   | `changed(value: Bool)` |
| `Slider`     | `value: F32 = 0.0`（two_way；`PAINT\|SEMANTICS`）；`min: F32 = 0.0`、`max: F32 = 1.0`、`step: Option<F32> = Option::None`（`PAINT\|SEMANTICS`）；`origin: Option<F32> = Option::None`（填充起点，双击回到它；`PAINT`）；`marks: Bool = false`（在每一步画刻度，空间不足时不画；`PAINT`）；`axis: Axis = Axis::row`（`column` 时向上增长；`MEASURE\|LAYOUT\|PAINT`）；`tint: Option<Color> = Option::None`（填充色；`PAINT`） | `changed(value: F32)` |
| `Knob`       | 同 `Slider`，无 `marks`/`axis`：底部留四分之一缺口的圆环与指向值的表盘，默认 40×40，上/右拖动转动（200 dp 转满） | `changed(value: F32)` |
| `ColorPicker` | `value: Color = #417ed9`（two_way；`PAINT\|SEMANTICS`）；`alpha: Bool = true`（取不透明度，`false` 时颜色不透明且隐藏不透明度条）；`wheel: Bool = false`（色相环围着方块，隐藏色相条；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`label: String = ""`（Localizable；组的名称）。不接受子节点：方块（或色环）、色相条、不透明度条与十六进制输入加色块由框架建在节点内；未定宽时宽 232，定宽时各部分填满它 | `changed(value: Color)` |
| `ComboBox` | `value: String = ""`（two_way；所选项的标签，`free_text` 时为任意文字；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`options: List<String> = []`（列表的选项，键入时按标签过滤）；`free_text: Bool = false`（接受任意文字，Enter 或焦点离开时取用）；`placeholder: String = ""`、`empty_text: String = "No matches"`、`label: String = ""`（Localizable；文本框的名称）。不接受子节点：文本框、箭头与选项列表由框架建在节点内；`options` 等改变时重建，`value` 改变只更新文字。封闭集合只取选项，其他文字在 Escape 或焦点离开时退回 | `changed(value: String)` |
| `SearchField` | `value: String = ""`（two_way；查询）；`suggestions: List<String> = []`（列表显示的建议，最多 8 条）；`debounce: Duration = 0ms`（键入停顿多久才报告查询；清空、Enter、取用建议立即报告）；`placeholder: String = "Search"`、`label: String = ""`（Localizable）。不接受子节点：放大镜、文本框、清除按钮与建议列表由框架建在节点内 | `changed(value: String)`、`submitted`（Enter 或取用建议） |
| `TextInput`  | `value: String = ""`（two_way；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`placeholder: String = ""`（Localizable；同上）；`secure: Bool = false`（同上）；`multiline: Bool = false`（同上）；`input: InputKind = InputKind::text`（`SEMANTICS`）；`max_length: Option<U32> = Option::None`（`SEMANTICS`）；`invalid: Bool = false`（`STYLE\|SEMANTICS`） | `changed(value: String)`、`submitted` |
| `NumberField` | `value: F64 = 0.0`（two_way；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`min: Option<F64> = Option::None`、`max: Option<F64> = Option::None`、`step: F64 = 1.0`、`precision: Option<U8> = Option::None`（同上） | `changed(value: F64)` |
| `Tabs`、`RadioGroup` | `selected: U32 = 0`（two_way；`STYLE\|PAINT\|SEMANTICS`）                             | `selected_changed(value: U32)` |
| `Disclosure` | `expanded: Bool = false`（two_way；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；Column 的 Flex 属性        | `changed(value: Bool)` |
| `NavigationStack` | `current: U32 = 0`（two_way；`MEASURE\|LAYOUT\|PAINT\|SEMANTICS`）；`page_transition: PageTransition = PageTransition::native`（Styleable；无失效，下次换页读取）；Stack 的 `content_align` 与子节点放置 | `current_changed(value: U32)` |
| `Modal`、`Sheet`、`Toast` | `open: Bool = false`（two_way；`MEASURE\|LAYOUT\|PAINT\|HIT_TEST\|SEMANTICS`）；Stack 的 `content_align` 与子节点放置；`Sheet` 另有 `detent: U32 = 0`（two_way；`TRANSFORM\|PAINT`）、常量 `detents: List<MixedLength>`（`%` 为 Surface 沿滑动轴的份额，`dp` 为长度；至多 4 个，空为整个面板）、常量 `edge: SheetEdge = bottom`（`bottom`/`top`/`leading`/`trailing`）、常量 `dismissible: Bool = true` | `changed(value: Bool)`（`Toast` 无）；`Sheet` 另有 `detent_changed(value: U32)` |
| `InteractiveViewer` | `min_scale: F32 = 1.0`、`max_scale: F32 = 4.0`、`pan_margin: MixedLength = 0dp`（`TRANSFORM`，保留当前缩放与平移并收回界内）；`double_tap: Option<F32> = 2.5`（无失效，下次双击读取；`Option::None` 关闭双击）；Stack 的 `content_align` 与子节点放置 | — |

`RadioGroup` 的每个直接子节点是一个选项；选项是单个文字节点（没有子节点、不是勾选控件）时，文字前（从右到左时为其后）绘制一个圆，按文字首行定尺寸、以文字颜色着色，选中时在 150 ms 内填满并在中心显出圆点（`ease_out`，减少动态效果时立即）；组合的选项自行绘制（例如 `when selected`）。Web 导出时文字选项带 `vx-radio`，以 `::before` 的 CSS 过渡绘制同一标记。

`Disclosure` 是可展开的分组：第一个子节点是标题，其余子节点是正文。在标题上抬起、或焦点在 `Disclosure` 或其标题上时按 Space/Enter，翻转 `expanded`；未展开时正文退出布局、绘制、命中与语义树（`NodeStore::set_folded`，不重建子节点，之后挂入的子节点同样折叠，子节点自身的 `visible` 不受影响）。标题的默认角色为 `button` 并带 `expanded` 语义状态，`Disclosure` 本身是分组；标题是单个文字节点（没有子节点、不是勾选控件）时，文字前（从右到左时为其后）绘制一个指向末端的 V 形标记，按文字首行定尺寸、以文字颜色着色，展开时在 150 ms 内转为向下（`ease_out`，减少动态效果时立即）；组合的标题自行绘制（例如 `when expanded` 加 `transition.rotation`）。`when expanded` 选中展开态。Web 导出为 `<details>`，标题为其 `<summary>`，`toggle` 事件投递 `changed`；文字标题带 `vx-chevron`，以 `::before` 的 CSS 过渡转动同一标记。

`NavigationStack` 是页面栈：子节点按 Stack 重叠，只显示第 `current` 个，其余退出布局、绘制、命中与语义树（`NodeStore::set_page`，与 `Disclosure` 的折叠同为 Store 侧谓词：不重建子节点，之后挂入的子节点同样折叠，子节点自身的 `visible` 不受影响）。`current` 变化时两页按 `page_transition` 交换位置（prelude `enum PageTransition { native; slide; shared_axis; fade; none; }`），向更小的页为返回、否则为前进：`slide` 新页从尾边整宽滑入、被覆盖页反向漂移三成宽度（临界阻尼弹簧，半秒周期），返回相反；`shared_axis` 两页沿行进方向各移 30dp 并交叉淡入淡出（300 ms，`cubic-bezier(0.2, 0, 0, 1)`）；`fade` 只淡化上层的页；`none` 立即交换；`native` 在 Apple 平台为 `slide`、Android 为 `shared_axis`、其余为 `fade`。两页以呈现值移动、不触发布局，移动中再换页时各页从当前位置继续；离开中的页不接受指针，到位后折叠；RTL 栈的尾边在左；减少动态效果时 `slide`、`shared_axis` 改为 `fade`。栈默认裁剪（`clip` 可改）。返回手势在 `current > 0` 时以 `current_changed(current - 1)` 请求上一页：焦点在栈自身时按 Escape 或 Backspace，或栈内任何处的系统返回动作（鼠标后退键：macOS 按钮 3、Windows XBUTTON1、X11 按钮 8、Wayland `BTN_SIDE`；Android 返回键与返回手势；浏览器后退键）；焦点不在任何栈内时，未被处理的系统返回先关闭最内层的瞬态层，没有则交给最深的、正在显示且不在首页的栈。`bind current <=> page` 即随之返回，单向 `current: x` 由 Handler 决定。默认角色为 `navigation`。Web 导出：非当前页带 `data-vx-away`（`display: none`），换页以 Web Animations 移动两页，`slide` 的弹簧取样为 `linear()`；`keydown`（Escape/Backspace 在栈自身上、`BrowserBack`）与鼠标后退键（`button === 3`）投递 `current_changed`，最深的栈先取。

`Modal`、`Sheet`、`Toast` 是层（`NodeStore::set_layer`）：不占父节点的布局空间，按整个 Surface 布局并绘制在顶层，仍挂在书写处，保留所有者、Handler 与语义树位置，祖先的滚动与变换仍作用于它（因此应写在滚动内容之外）。`open` 为假（或 `visible` 为假）时整层隐藏。内容按 Stack 放置，未写 `content_align` 时 `Modal` 居中，`Toast` 贴底，`Sheet` 贴其 `edge`。`Modal`、`Sheet` 未写 `background` 时以半透明黑（32%）作遮罩；打开时把焦点安静地移到层内第一个可聚焦节点（没有则移到对话框自身），Tab 限定在层内循环，关闭时归还打开前的焦点与焦点范围，嵌套对话框按次序归还。在遮罩（对话框自身的盒子、内容之外）上按下、或层内按 Escape，对话框吞掉该指针（按下捕获指针，抬起不落到其下露出的节点）并以 `changed(false)` 请求关闭；`bind open <=> x` 即随之关闭，单向 `open: x` 由 Handler 决定。`Sheet` 的第一个子节点是面板：按其完整尺寸贴边布局，打开时以临界阻尼弹簧沿滑动轴从边外滑到 `detent` 所指的停靠点（`detents` 自小到大的序号；停靠量夹在面板尺寸内），关闭时滑出、滑出后才隐藏并归还焦点；遮罩随露出量在最小停靠点以内由透明加深到 `background`（未写为 32% 黑）。面板沿滑动轴接受拖动（触摸从其内的滚动视口开始、且视口在手指方向已无可滚时也交给面板）：越过完整尺寸处硬停；低于最小停靠点时，`dismissible` 为假则阻尼收敛，否则跟手。松开时按当前露出量加速度 × 0.2s 的投射点取最近的停靠点（`dismissible` 时含 0）：落到其他停靠点以 `detent_changed(k)` 告知（`bind detent <=> x` 随之写回），落到 0 以 `changed(false)` 请求关闭，Owner 保持打开时弹回原停靠点。运动只标 `TRANSFORM`（遮罩为 `PAINT`），减少动态效果时直接到位。`Toast` 自身的盒子不参与命中，指针穿过它落到下层，只有其内容可命中；它没有内置响应，`semantics.live` 默认为 `polite`。Web 导出：`Modal`、`Sheet` 为以 `showModal()` 打开的 `<dialog>`（顶层、背景惰性），`cancel` 与在 `<dialog>` 自身上的 `pointerdown` 投递 `changed(false)`；`Sheet` 另带 `data-sheet-edge`、`data-detents`、`data-detent`、`data-dismissible`，脚本以同样的弹簧（Web Animations）滑入滑出面板并以 `color-mix` 加深遮罩，面板上的拖动按同样规则落到停靠点（`viso-detent` 投递 `detent_changed`）或关闭（`viso-dismiss` 投递 `changed(false)`），关闭在滑出后才 `close()`；`Toast` 为固定定位、`pointer-events: none` 的 `role="status"` 层，关闭时 `hidden`。

`Island` 标出由 Viso renderer 绘制的一个 Component：内容恰为本文件声明的一个 Component 节点（否则为 mount error），不可嵌套；它的 input 与 event 是进出岛的全部。不拆分视图的 target 上它就是该子节点的 Stack（Stack 的 `content_align` 与子节点放置）；`web-hybrid` 页面中岛是页面 DOM 里的一块，由 renderer 在其中的 canvas 绘制该 Component，调用方绑定的 input 随 State 推入，Component 发出的 event 交给调用方的 Handler（`Viso_CLI.md` §32）。

`InteractiveViewer` 是可平移、缩放的查看器（`NodeStore::set_viewer`，与 Rust Widget `interactive_viewer` 共用 `viso_ui::viewer`）：内容按 Stack 放置（未写 `content_align` 时居中），以缩放 1 布局，之后的平移与缩放只是内容的变换，只标记 `TRANSFORM`。拖动平移，双指、触控板捏合或按住 Control 转动滚轮以指针处为中心缩放，滚轮与触控板双指滚动平移，甩动后惯性滑行；边界使比查看器大的内容边缘不进入查看器、比它小的内容居中，`pan_margin` 在两种情况下都放宽边界，缩放限于 `min_scale`..`max_scale`。平台视口回弹时（`ScrollPhysics.overscroll` 为 `bounce`）手指与触控板可拉过边界并弹回，缩放在对数空间回弹；鼠标与滚轮停在边界。双击在静止时缩放到 `double_tap` 并保持双击处不动，已放大时回到原样，以临界阻尼滑行（减少动态效果时立即）。触摸拖动在内容已到边界的方向上让给外层滚动视口。获得焦点时方向键与 PageUp/PageDown 平移，`+`/`=` 与 `-` 按 1.25 倍缩放，`0` 与 Home 复原。默认裁剪（`clip` 可改），角色为 `group`。Web 导出：裁剪的盒子带 `data-min-scale`、`data-max-scale`、`data-pan-margin`、`data-double-tap`（绑定的 Property 随 State 更新），内容以 `transform-origin: 0 0` 的 `translate() scale()` 定位；拖动与捏合沿用 `draggable`/`pinchable`，`wheel` 在能平移时平移、否则让页面滚动，滑行与惯性取样为 Web Animations 播放。

`SwipeRow` 是列表里可滑动露出操作的行（`NodeStore::set_swipe_row_of_children`，与 Rust Widget `swipe_row` 共用 `viso_ui::row_gesture::swipe`）：它是 Stack，第一个不是 `SwipeActions` 的子节点是内容，`SwipeActions { edge: SwipeEdge::leading | trailing; … }`（Row；`edge` 为 `LAYOUT|TRANSFORM`，默认 `trailing`）是该边的操作面板，铺满行、位于内容之下、在行外等待；面板与内容随子节点增删重新读取。横向拖动（手指、鼠标、触控板双指）推开内容，面板随之滑入，只标记 `TRANSFORM`；松手时过操作宽度一半或甩动（≥ 400 dp/s）即弹开停在操作上，否则收回；超过操作宽度后阻尼拉伸。`full_swipe: FullSwipe = FullSwipe::trailing`（`none; leading; trailing; both;`，无失效，下次松手读取）指定可整行滑过的边：越过行宽的 55%（至少操作宽度加 48dp，至多 95%）松手，内容滑出行外，投递 `full_swiped(FullSwipeEvent { edge: SwipeEdge })`，停留 0.15 秒后弹回。纵向拖动仍归列表滚动；同一时刻只有一行打开，别处的按下或滚动收回它（列表内的该按下不点击），按下操作在其抬起后收回。焦点进入操作面板时打开该边，离开时收回，Escape 收回。弹簧临界阻尼（减少动态效果时立即）。默认裁剪（`clip` 可改），不获得焦点，`full_swiped` 不需要焦点。Web 导出：裁剪的行带 `data-full-swipe`，面板带 `data-swipe-edge`（0 leading、1 trailing）并绝对定位铺满行，拖动沿用 `draggable`（`data-drag-axis="row"`），弹簧为 Web Animations 播放，整行滑过以 `viso-fullswipe` 事件投递。

行重排：`Row`、`Column`、`Scroll` 与 `VirtualList` 处理事件 `reordered(ReorderEvent { from: U32, to: U32 })` 时成为可重排列表（`NodeStore::set_reorder_list`，与 Rust 的 `Scroll::on_reorder`/`VirtualList::on_reorder` 共用 `viso_ui::row_gesture::reorder`）：其行是它的子节点，滚动视口则是其内容节点的子节点，位置取 `VirtualList` 宣告的序号，否则取子节点次序。`ReorderHandle`（Stack，子节点放置同 Stack；默认角色 `button`、`focusable: true`，未写 `cursor` 时为 `Cursor::grab`）是行内任意位置的把手：拖动它提起所在行，行在兄弟之上沿列表主轴跟随指针，途经的行按布局中点让位，只标记 `TRANSFORM`；指针停在视口两端 40dp 内时滚动视口，滚轮亦可，行保持在指针下。松手后行弹入空位，全部静止后在下一帧投递 `reordered`，`to` 为去掉该行后的位置，Owner 在该 Handler 中移动其数据（`items.remove(e.from)` 后插入 `e.to`）；落回原位不投递。把手获得焦点时 Space/Enter 提起，沿主轴的方向键逐位移动并保持可见，Space/Enter 放下，Escape、焦点离开或按下取消。`VirtualList` 的被提起行在滚出窗口时保持挂载。`reordered` 不需要焦点。减少动态效果时立即到位。Web 导出：把手带 `data-reorder-handle`、`tabindex="0"`、`role="button"`，列表的脚本以 CSS transition 让位与落位，`viso-reorder` 事件投递 `{ from, to }`。

拖放：任意节点（`Fragment` 除外）的 `drag: Option<String>` 使它成为拖放源，携带这段文本，`drag_kind: String`（默认 `"text"`）给出它的种类：`"text"` 为纯文本，`"uri"` 为链接（同时提供其文本），`"files"` 为文件（每行一个路径），其他名字是应用命名的种类（Rust 侧 `DragKind::named` 同名即同种，值为该 `String`）。`drop_accepts: List<String>` 非空的节点是放置目标，取用其中列出的种类，`drop_operation: DropOperation`（`move`/`copy`/`link`，默认 `move`）是放下时做的事；四者都不引起失效。鼠标在源上按下并移过阈值、触摸或触控笔在源上长按即提起：此后指针样本不再到达任何节点，源处于 `dragging`；源的直接子节点 `DragPreview`（Stack，子节点放置同 Stack）是它的预览：平时收起，提起时作为覆盖整个表面的层显示在源的位置上、内容在其起始角，随指针从抓取点跟随，只标记 `TRANSFORM`，不参与命中。指针下最内层取用所携带种类之一的目标处于 `drop_over`，它在拖动经过时收到 `drop_enter`、离开或被放回时收到 `drop_leave`，在其上松手时收到 `drop`（`DropEvent { position, kind, text, files, operation }`：`kind` 是它取用的种类在其 `drop_accepts` 中的序号——按其列出的顺序取第一个数据提供的种类，`text` 是该种类的文本，`files` 是 `"files"` 的路径，`position` 为目标的局部坐标）；在别处松手、`Escape` 或源离开树都放回数据，预览以临界阻尼弹簧弹回源上再收起。源随后收到 `drop_end(DropEnd { operation: Option<DropOperation> })`，放回时为 `none`。键盘：焦点在源上按 Space/Enter 提起到阅读顺序中第一个取用它的目标上，Tab 与方向键在这些目标间移动，Space/Enter 放下，Escape 或焦点离开放回。这些事件经按键处理器投递，不需要焦点，减少动态效果时预览立即到位。Web 导出用浏览器自身的拖放：源带 `data-drag`、`draggable="true"` 与 `data-drag-kind`，种类写入 `text/plain`、`text/uri-list` 或 `application/x-viso.<种类>`，`DragPreview` 移到页外作为 `setDragImage` 的图像；目标带 `data-drop-accepts`（每行一种）与 `data-drop-operation`，`drop_enter`/`drop_leave` 时浏览器尚不给出文本（`text` 为空），从页外拖入的文件作为 `"files"`，`files` 为文件名。原生窗口同样接受别的应用拖入的文件、链接与文本：它们作为 `"files"`（`files` 为路径）、`"uri"` 与 `"text"` 到达取用它们的目标，经过时目标处于 `drop_over` 并收到 `drop_enter`/`drop_leave`（`text` 为空），放下时收到 `drop`；目标的 `drop_operation`（来源不允许时改为复制）告诉系统这次放下做什么，没有目标取用时系统显示不可放下。携带文本、链接或文件的源在桌面上用鼠标拖出窗口边缘时，拖动交给系统继续：预览收起，其他应用（以及本应用自己的窗口，作为外来拖放）可以接收它，源随后收到 `drop_end`，`operation` 是对方所做的（文本可被复制或移动，文件可被复制或链接，`none` 为放回）。

`TextInput` 无文本时以其文本颜色一半的不透明度绘制 `placeholder`，`placeholder` 同时是没有 `semantics.hint` 时的提示；`secure` 的输入框为每个字素绘制一个 `•`，光标、点击与 IME 位置经绘制文本与原文之间的映射落在原文上，不提供复制或剪切的文本，语义角色为 `Role::secure_field`，不向辅助技术报告其文本；获得焦点时平台输入法按密码处理，不组字、不给候选、不学习或提示其输入：macOS 持有 Secure Event Input 并只允许罗马字输入源，iOS `secureTextEntry`，Android `TYPE_TEXT_VARIATION_PASSWORD`，Web 为 `type=password` 的输入元素，Windows 与 X11 让按键绕过输入法，Wayland `text-input-v3` 的内容用途为 `password`。没有 `value` 的输入框照常取得文本样式、`placeholder` 与 `secure`，其文本是用户输入的内容。`Toggle`/`CheckBox` 的 `label` 是其绘制的文字，也是没有 `semantics.label` 时的可访问名称；文字前（从右到左时为其后）绘制控件自身的标记，按文字首行定尺寸、居中于首行，以文字颜色着色：`CheckBox` 为一个方框，选中时填充并画勾，`Toggle` 为一条轨道，滑块选中时在末端。状态变化时标记过渡到新状态（减少动态效果时立即）：方框的填充淡入、勾沿笔画画出（150 ms，`ease_out`），滑块沿轨道滑动、轨道随之填充（临界阻尼弹簧，0.28 s 周期），途中再翻转时从当前位置继续。

`TextInput` 的编辑按平台惯例响应按键（Rust Widget 与 DSL 共用 `viso_ui::field_input`）：Left/Right 按绘制的光标停靠点逐个字素移动，Up/Down 到上一行/下一行并保持光标所在的列（单行时到文本首尾），Home/End 到行首/行尾，PageUp/PageDown 移动框内可见的行数；Apple 系统（含其上的浏览器）上 Option 按词移动与删除、Command 到行首尾（与 Up/Down 到文本首尾）、Option 加 Up/Down 到段首尾，Control 加 A/E/B/F/P/N/H/D/K 是系统文本框的 Emacs 移动与删除；其他平台上 Control 按词移动与删除，Control 加 Home/End 到文本首尾、加 Up/Down 到段首尾，Control+Y 重做；Shift 随任一移动扩展选区；按下放置光标、按住拖动扩展选区，双击选中指针下的词（或一段空白与标点），三击选中其段落（含其后的换行），之后按住拖动按整词或整段扩展，Shift+按下扩展到按下处——同一指针 500 ms 内、在其上次按下附近（鼠标 4、触摸 16 逻辑像素）的按下计为同一串点击的下一击，至多三击；secure 输入框双击选中全部；加速键（Apple 为 Command，其他为 Control）加 A 全选、加 Z 撤销、加 Shift+Z 重做；单行输入框中 Enter 投递 `submitted`（按住不重复）。`multiline: true` 的输入框持有多行：文本按框宽折行，Enter（含 Shift+Enter）插入换行，加速键加 Enter 投递 `submitted`，粘贴的 `\r\n` 与 `\r` 统一为 `\n`；单行输入框把输入与粘贴中的每个换行折为一个空格；secure 输入框总是单行。HTML 导出把常量多行的输入框导出为 `<textarea>`，`multiline` 随值变化报导出诊断。输入框的文本裁剪在自身盒内，由框架绘制其标记：获得焦点时，选区在文本之下以半透明蓝高亮（窗口不活动时为半透明灰，选中的换行在行尾多高亮 0.3em），IME 组字区在基线下以文本颜色画下划线，光标（1.5 逻辑像素宽、行高）以文本颜色画在文本之上，只在选区为空且窗口活动时显示；光标移动或文本改变后先常亮 530 ms，再以 530 ms 亮灭闪烁，10 秒无操作后常亮且不再唤醒帧；减少动态效果时不闪烁。文本超出盒时输入框在内部滚动以露出光标（单行横向、多行纵向），滚轮、触控板与触摸平移在其文本范围内滚动它，余下的量交给外层滚动视口；单行输入框失去焦点时滚回文本开头。词边界按 UAX #29，跳过空白与标点；secure 输入框按词移动与删除到其首尾。每个输入框有自己的撤销历史：连续输入合为一步并在空白后开始的新词处断开，连续 Backspace、连续前向删除各合为一步，粘贴、剪切、按词或按行删除、一次 IME 组字与一次外部 `value` 改写各为一步，移动光标或选区结束当前步；撤销恢复编辑前的选区，重做恢复撤销时的选区；历史最多保留 256 KiB 文本与 1000 步，超出时丢弃最早的步；secure 输入框不保留历史；热重载迁移缓冲时历史清空。手指与触控笔在抬起时才作用，因此变成平移的触点不放置光标：轻点放置光标；在已获得焦点的输入框中轻点光标处或选区内，显示或隐藏编辑菜单；双击选中词、三击选中段落并显示编辑菜单；长按（500 ms）取得焦点并选中指下的词（落在行尾之后的空白处时放置光标），手指随后移动按整词扩展选区，Apple 系统上长按改为放置光标并随手指移动；抬起时显示编辑菜单。触摸得到的选区两端显示选区手柄，绘制在输入框之上、可越出其盒：Apple 系统上为沿行的竖线，起点上方、终点下方各一圆点，其他系统为行下方指向端点的水滴形；手柄所在的端点滚出输入框时隐藏。手指落在手柄 22 逻辑像素之内时由路由器直接拖动该端（不经节点处理器，不被平移或长按截走），选区至少保留一个字素，拖动时隐藏编辑菜单，抬起后显示。手指携带光标（长按后移动）或拖动手柄时，平台在其上方显示放大镜，随手指移动、对准光标所在的行，抬起时收起（iOS 17 起为 `UITextLoupeSession`，Android 9 起为 `Magnifier`；其他平台不显示）。鼠标、按键或任何非触摸编辑结束手柄与菜单。编辑菜单由平台绘制（iOS 16 起为 `UIEditMenuInteraction`，之前为 `UIMenuController`；Android 为浮动 `ActionMode`），指向选区（或光标）并避开它，提供剪切、复制（有选区且非 secure 时）、粘贴（剪贴板有文本时）与全选（文本非空且未全选时）；剪切、复制、粘贴经与快捷键相同的剪贴板路径，全选后菜单保持显示；用户在别处触摸或选择菜单项使菜单关闭时，平台报告其关闭。窗口不活动时不显示菜单。平台不绘制编辑菜单时（桌面的触摸屏、Web、X11、Wayland、KMS）由窗口自己绘制：一条深色的横条，居中于选区上方，上方放不下时在其下方（让出手柄的位置），只列出可用的编辑，按下即执行，全选后横条保持显示，选别的项后关闭。次键单击（Apple 系统上也可 Control 加单击）使输入框取得焦点并在指针处请求上下文菜单：指针不在选区内时，Apple 系统先选中指下的词（行尾之后的空白处放置光标），其他系统把光标放到指针处；非 Apple 系统上菜单键与 Shift+F10 在光标下方请求它；任何其他编辑收回请求。macOS 弹出 `NSMenu`，Windows 弹出系统弹出菜单（项后附 Ctrl+X/C/V/A），都在请求它的帧结束后由消息循环弹出，菜单项为剪切、复制、粘贴与（分隔线后）全选，可用条件同编辑菜单，标题按应用的 locale（二十余种语言，其余为英文）；菜单追踪期间吞掉的按键抬起在其关闭后补发，关闭时平台报告其关闭；其全选是普通的全选。iPadOS 与 Android 上鼠标的次键单击改用系统编辑菜单，指向指针处。Web、X11、Wayland 与 KMS 上上下文菜单由窗口自己绘制：一列浅色菜单项，左上角在指针处，越出窗口时翻到指针的另一侧，不可用的项变暗且不响应；鼠标悬停高亮，Up/Down 在可用项间移动高亮（Home/End 到首尾），Enter 或 Space 执行高亮项，Escape、在菜单与输入框之外按下、滚动或窗口失去活动时关闭。框架绘制的菜单位于顶层，语义上是 `Role::menu`，各项为 `Role::menu_item`；复制与剪切把选区写入系统剪贴板，粘贴向平台请求剪贴板文本（Web 上可能先征得许可），与快捷键走同一路径。`input` 说明输入框持有什么（prelude `enum InputKind { text; digits; integer; decimal; phone; email; url; search; }`）：它决定软键盘（数字键盘、电话键盘、邮箱/网址键盘、搜索回车键；单行输入框的回车键为“完成”，多行为换行），`digits`/`integer`/`decimal`/`phone` 的输入框在 macOS、Windows 与 X11 上不经输入法组字。键入、粘贴与 IME 提交进入输入框的每段文本依次经过三条规则：`input` 的规则（`digits` 只取数字；`integer` 取数字与仅在开头的一个 `-`，符号之前不再取任何字符；`decimal` 另取全文唯一的一个小数点，`,`、`。` 与全角 `．` 读作小数点；`phone` 取数字与 `+ - . ( ) * #` 和空格；全角数字与减号读作 ASCII），Rust `TextInput::filter` 的自定义映射，以及 `max_length`——按字素计数，只保留能放下的开头部分（与前一字素合并的组合符不占长度）。制表符与换行以外的控制字符从不进入；一段文本一个字符都留不下时整段被拒，它本要替换的选区保持不变；IME 组字过程中的预编辑不受限制，提交时才过规则。外部 `value` 与撤销/重做不过规则，`value` 可以超过 `max_length`。HTML 导出写出 `inputmode`、数字类的 `pattern` 与 `maxlength`，随值变化时由页面脚本更新。

`NumberField` 是持有一个数的输入框（Rust Widget 与 DSL 共用 `viso_ui::number_input`，数的读写与格式化为 `viso_ui::NumberFormat`）：文本就是数，文本读作 `min..=max` 内的数时随键入投递 `changed`。未获得焦点时在其上横向拖动按每 2 逻辑像素一步刮动数值（Shift 十倍、Alt 十分之一，整次拖动为一步撤销），单击取得焦点并全选；获得焦点后按 `TextInput` 编辑，Up/Down 增减一步、PageUp/PageDown 十步（Shift、Alt 同上），滚轮每 16 逻辑像素一步，Enter 或失去焦点把文本落定为范围内、按精度舍入（远离零取整，无负零）的数，Escape 退回上次落定的数。精度缺省为 `step` 的小数位数；`input` 由范围与精度决定（整数且 `min >= 0` 为 `digits`，整数可负为 `integer`，否则 `decimal`），按 §U 的输入规则过滤。外部 `value` 只在文本按精度显示的数与之不同时改写文本，故键入一半的 `1.` 不被回写的 `1` 覆盖。语义角色为 `Role::spin_button`，报告其数值与范围，辅助技术的增减动作映射为 Up/Down。HTML 导出为 `inputmode` 对应的文本输入框，带 `aria-valuenow`/`aria-valuemin`/`aria-valuemax`，页面脚本处理方向键步进、`change` 时落定与 Escape 退回。

Widget 事件的 Payload 是 Prelude Record：`Toggle`/`CheckBox`/`Disclosure` 的 `changed` 为 `ToggleChanged`，`Slider` 为 `SliderChanged`，`ColorPicker` 为 `ColorChanged`，`TextInput`、`ComboBox`、`SearchField` 为 `TextChanged`，`NumberField` 为 `NumberChanged`，`selected_changed` 与 `current_changed` 为 `SelectionChanged`，字段均为 `value`；`submitted` 与 `triggered` 无 Payload。

`Toggle` 使用 `checked` 而不是 `on`：`on` 是 View Item 起始位置的 Contextual Keyword，同时 `checked` 与语义状态同名（ADR 0021）。

链接标签：`LinkLabel` 是只含一个链接的文字，`text: String` 为文字（同 `Text`），`url: String = ""`（`MEASURE|PAINT|SEMANTICS`）为链接目标，整段文字即其链接片段；未给文字颜色时为链接蓝（`#0066cc`）。其余行为与上文带链接的 `Text` 相同：可获得焦点、Enter 或点击激活，没有 `on link` 时交给系统打开 `url`。`url` 为空时是普通文字。HTML 导出为 `<a class="vx-text vx-link" href>`，`url` 随 State 变化时由页面脚本更新 `href`。

徽标与头像：`Badge` 与 `Avatar` 都不是交互 Widget，不获得焦点。`Badge` 的 `count: Option<I32> = Option::None` 是所计的数（`MEASURE|PAINT|SEMANTICS`），不超过 `max: I32 = 99` 时显示该数，超过时显示 `max` 加 `+`（`99+`），负数显示 `0`；没有 `count` 时为 8dp 的圆点。计数居中于胶囊上，胶囊高为文字行高（至少 16dp）、至少与高一样宽、两侧各留高的四分之一；未指定尺寸时为 `Fit`，文字默认 11sp、字重 500、白色，`tint: Option<Color> = Option::None`（Styleable；`PAINT`）为胶囊底色（`None` 为 `#b3261e`，`background` 优先），`corner_radius` 默认 `50%`；`label: String = ""`（Localizable）为可访问名称，否则名称为所显示的计数，没有计数也没有 `label` 的圆点为装饰。`Avatar` 的 `name: String`（Localizable）是所代表的人，`image: Option<ImageAsset> = Option::None` 为其照片；未指定尺寸时为 40dp 的圆（`corner_radius` 默认 `50%`），有 `image` 时照片按 `cover` 填满并裁成圆，解码前显示底色；没有时显示名字首末两个单词的首字母（大写；首字没有大小写之分的文字只显示首字，如“张”），白色、字重 500、字号为常量宽度的 0.4 倍（未写 `font_size` 时）。底色为 `tint`，未给时由名字决定：名字（去掉首尾空白）UTF-8 的 FNV-1a 哈希对 10 取模，选取 `#c62828 #ad1457 #6a1b9a #283593 #1565c0 #00695c #2e7d32 #d84315 #4e342e #37474f` 之一，同名总是同色。`Avatar` 默认角色为 `image`，名称为 `name`。HTML 导出为 `<span class="vx-badge">`（圆点加 `vx-dot`）与 `<span class="vx-avatar" role="img">`（照片为 `background-image`），文字、底色与名称在导出时算好，随 State 变化时由页面脚本以同一规则重算。

纸片：`Chip` 是可获得焦点的交互 Widget，`label: String = ""`（Localizable）为字幕与可访问名称。写了 `selected: Bool`（two-way，`PAINT|MEASURE|LAYOUT|SEMANTICS`）的纸片可选中：在它上（叉以外）按下抬起、Space/Enter 或访问键报告 `changed(ToggleChanged)`，携带取反后的值，`bind selected <=> s` 时先写回；选中时着色并在字幕前长出对勾（纸片随之变宽），默认角色为 `check_box`。没写 `selected` 的纸片不可选中，默认角色为 `button`，Space/Enter 与访问键像按钮一样报告 `click`。`removable: Bool = false`（`PAINT|MEASURE|LAYOUT|SEMANTICS`）为真时在末端边画一个叉：在叉上抬起、或聚焦时按 Delete/Backspace 报告 `removed`（无载荷信号），这次抬起不再报告 `click`；辅助技术可执行 “Remove” 动作，等同 Delete。未指定尺寸时为 `Fit`：高为字幕行高加 12dp（至少 32dp），圆角为高的四分之一，字幕色同时为描边、着色、对勾与叉的颜色（`color`，默认近白）。HTML 导出为 `<button class="vx-chip" type="button">`，可选中时带 `aria-pressed`，可移除时带 `data-removable="true"`，对勾与叉由样式表的 `::before`/`::after` 画出，页面脚本按点击位置区分叉与主体，并在 Delete/Backspace 时报告 `removed`。

步进器：`Stepper` 是可获得焦点的交互 Widget：`value: F64`（two-way，`PAINT|SEMANTICS`）在 `min: Option<F64> = Option::None` 与 `max: Option<F64> = Option::None` 之间按 `step: F64 = 1.0` 步进，`wrap: Bool = false` 为真且两端都有界时越过一端绕到另一端；`tint: Option<Color> = Option::None`（Styleable；`PAINT`）为减号、加号与分隔线的颜色（`None` 为文字色），胶囊为其 8% 不透明度；`label: String = ""`（Localizable）为可访问名称。在减号或加号上按下立即步进一次，按住 500ms 后继续步进（先每 120ms、八次后每 50ms），抬起、移出或到达边界即停止；聚焦时上下方向键步进一次（左右键同样，RTL 下互换），Page Up/Down 十次，Home/End 到边界。每次步进报告 `changed(NumberChanged)`，携带新数值，`bind value <=> s` 时先写回；数值落在从 `min`（无下界时为 0）起的 `step` 网格上。不能再步进的一半减淡。未指定尺寸时为 `Fit`，即 96×32dp，RTL 下减号在右。默认角色 `spin_button`，报告数值与范围。HTML 导出为 `<div class="vx-stepper" role="spinbutton" tabindex="0">`，内含两个不可聚焦、对辅助技术隐藏的 `<button class="vx-step">`，数值与边界写在 `aria-value*` 上，`step` 与 `wrap` 写在 `data-step`、`data-wrap` 上，页面脚本处理按住连发与按键，并为不能再步进的一半加 `vx-spent`。

分段控件：`SegmentedControl` 是可获得焦点的交互 Widget，行布局：`selected: U32 = 0`（two-way，`STYLE|PAINT|SEMANTICS`）是选中的直接子节点（含控制流区域中的），选择规则与 `RadioGroup` 相同（释放选中所在子节点，方向键与 Home/End 移动选中），报告 `selected_changed(SelectionChanged)`；`tint: Option<Color> = Option::None`（Styleable；`PAINT`）为轨道、滑块与分隔线的颜色（`None` 为文字色），轨道为其 10%、滑块 24%、分隔线 20% 不透明度。未写 `padding` 时轨道四周留 2dp，未写 `align` 时为 `stretch`，未写 `background`、`corner_radius` 时轨道着上述底色、圆角 8dp。每个直接子节点是一段：没有子节点的文字段未指定尺寸时宽为 `fill`、高为 `fit`（平分轨道宽度），文字居中、两侧各留 12dp、上下共 10dp、至少 28dp 高；组合的段自行绘制。滑块画在各段之下、选中段的盒上（圆角 6dp，带轻阴影），选中改变时从当前所在位置以弹簧（response 0.3s、临界阻尼）滑到新段，减少动态效果时立即到位；滑块按各段的布局盒定位，因此随尺寸、文字与方向变化而跟随。相邻两段之间画一条细分隔线，离滑块 6dp 内渐隐。容器角色为 `group`（Web 导出为 `radiogroup`），每段默认角色为 `radio`，选中段为 checked。HTML 导出为带 `vx-segmented` 的行容器、每个文字段带 `vx-segment`，页面脚本按选中段的 `offsetLeft`/`offsetWidth` 写 `--vx-thumb-x`/`--vx-thumb-w`，由 `::before` 以 CSS 过渡滑动，尺寸变化时重新定位；`tint` 写为 `--vx-ink`。

进度指示：`ProgressBar` 与 `ProgressRing` 显示工作进行到哪里，`Spinner` 只说明工作正在进行；三者都不是交互 Widget，不获得焦点，默认角色为 `progress_indicator`。`ProgressBar`/`ProgressRing` 的 `value: Option<F32> = Option::None` 是 `min: F32 = 0.0`..`max: F32 = 1.0` 内的进度（`PAINT|SEMANTICS`，越界时夹在范围内），`Option::None` 表示时长未知；三者另有 `tint: Option<Color> = Option::None`（Styleable；`PAINT`，`None` 为强调蓝）与 `label: String = ""`（Localizable；没有 `semantics.label` 时的可访问名称）。`ProgressBar` 是一条胶囊形轨道（`tint` 的 24% 不透明度），未指定尺寸时为 160×4dp，从起始边（从右到左时为右边）按 `value` 在范围中的份额填充；`ProgressRing` 是 32dp 的圆环，线宽为边长的十分之一，从顶部顺时针画出份额。`value` 变化时填充以 80 ms 时间常数的指数缓动趋近新值，途中再变化时从当前位置继续，减少动态效果时立即到位；时长未知时条上一段填充按 `cubic-bezier(0.4, 0, 0.2, 1)` 先伸后缩地扫过轨道（1.8 s 一轮），环上一段弧追逐自身绕圈（与 Material 转圈同一曲线），从未知变为已知时填充从零长出。`Spinner` 为 20dp 的系统转圈（Apple 平台为八根辐条、领头辐条每 125 ms 前进一格只改节点旋转，其他平台为追逐的弧），显示时一直转动。运动中的指示器每帧重绘（只标记 `PAINT`），停下的、以及自身或祖先隐藏或完全透明的不唤醒帧，再显示时从原处继续。语义：有 `value` 时报告数值与范围（iOS 读作百分比），无 `value` 时不报告数值，没有增减动作。HTML 导出为 `<div role="progressbar">`，带 `aria-valuemin`/`aria-valuemax` 与有值时的 `aria-valuenow`，填充份额为 CSS 变量 `--vx-at`（`vx-progress` 以 `::before` 宽度、`vx-ring` 以 `conic-gradient` 绘制，`@property` 使其过渡），缺少 `aria-valuenow` 时以 CSS 动画扫过或转动；`vx-spinner` 为转动的渐变环；`tint` 为元素的 `color`；随 State 变化时由页面脚本更新属性与份额。

### U7.6 NodeRef 动作

Named Node `n` 在同一 Component 的 Action 与 Handler 中以 `n: NodeRef<T>` 可见（§49.1）：

| 方法                                            | 适用 `T`      | 效果                                              |
| ----------------------------------------------- | ------------- | ------------------------------------------------- |
| `n.focus()` / `n.blur()`                        | 可聚焦节点    | 程序化焦点移动，不激活 `focus_visible`            |
| `n.scroll_into_view(smooth?)`                   | 任意          | 最近祖先视口以最小滚动使 `n` 可见                 |
| `n.scroll_to(offset, smooth?)` / `n.scroll_by(delta, smooth?)` | `Scroll` | 只标记 `TRANSFORM` |
| `n.scroll_to_key(key, smooth?)` / `n.scroll_to_index(i, smooth?)` | `VirtualList` | 见 U9.3                         |
| `n.animate(target, spec)` / `n.play(track)`     | 任意          | 见 U6.3                                           |

滚动方法可带末尾的 `smooth: Bool`（默认 `false`）：为 `true` 时平滑滑行到目标而非立即跳转；滑行中再次请求从当前速度转向新目标，`scroll_by` 在滑行目标上累加；手指、滚轮或滚动条操作会停止滑行。

在 View、Computed 与 `fn` 中调用这些方法是 §83 纯度错误；方法返回 `()`，在视图结构落定后生效；对未挂载或已 Dispose 节点调用是 No-op。

当前实现：`focus`、`blur`、`scroll_into_view`、`scroll_to`、`scroll_by`、`scroll_to_index`、`scroll_to_key` 在宏、热重载、Release Package（含 AOT）与 HTML/Solid 导出中生效（导出以 `data-node` 标记节点，从发起事件的元素就近查找）；`scroll_to_index`/`scroll_to_key` 把该行滚到视口起始边（列表末尾不足时尽量靠近），同一帧挂载其周围的窗口，越界 Index 取最后一行；`animate` 与 `play` 见 U6.3。

### U7.7 `cursor`

| Property | 类型     | 默认               | 失效       |
| -------- | -------- | ------------------ | ---------- |
| `cursor` | `Cursor` | 沿祖先继承；无则见下 | `HIT_TEST` |

`Cursor` 为 Prelude Enum：`default`、`pointer`、`text`、`vertical_text`、`crosshair`、`move`、`grab`、`grabbing`、`not_allowed`、`wait`、`progress`、`help`、`context_menu`、`copy`、`alias`、`zoom_in`、`zoom_out`、`resize_ew`、`resize_ns`、`resize_nesw`、`resize_nwse`、`resize_col`、`resize_row`、`hidden`。鼠标在节点上时窗口显示该节点及其祖先中最近一个写了 `cursor` 的值；都没写时，输入框显示 `text`，未获得焦点的 `NumberField` 显示 `resize_ew`（可刮动），其余为 `default`。inert（`enabled: false` 子树内）的节点一律显示 `default`。鼠标被 Capture 期间（拖动、刮动、按住 `Splitter` 分隔条）保持 Capture 开始时的形状，不随经过的节点变化。触摸与笔的直接接触没有光标。`Splitter` 的分隔条为 `resize_col`（左右分割）或 `resize_row`（上下分割）。

当前实现：`cursor` 可为常量或读取值，宏路径、热重载与 Release 包相同；Rust 侧为 `BuildCx::cursor(handle, CursorIcon)`。每帧布局后按鼠标所在节点求出形状，与上次不同时才推给平台。HTML 导出：常量写为元素的 CSS `cursor`（`resize_ew` 为 `ew-resize`、`resize_col` 为 `col-resize`、`hidden` 为 `none`，余同名换连字符），读取值由页面脚本随值设置。

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
| `semantics.merge`         | `Bool`           | `false`                         |

- `Role { group; button; check_box; slider; radio; label; text_field; tab; tab_list; navigation; dialog; status; region; tree; tree_item; heading; image; list; list_item; secure_field; spin_button; menu; menu_item; link; menu_item_check_box; menu_item_radio; menu_bar; separator; list_box; list_option; combo_box; tooltip; table; row; cell; column_header; editable_combo_box; progress_indicator; search_field; }`；
- `LiveRegion { off; polite; assertive; }`；
- 默认角色：`Button` → `button`；`Toggle`、`CheckBox` → `check_box`；`Chip` → 有 `selected` 时 `check_box`，否则 `button`；`Slider`、`Knob` → `slider`；`ColorPicker` → `group`（内含各部分的 `slider` 与 `text_field`）；`TextInput` → `text_field`；`ComboBox`、`SearchField` 节点本身无角色，其文本框分别为 `editable_combo_box` 与 `search_field`，以 `label` 命名；`NumberField`、`Stepper` → `spin_button`；`Tabs` → `tab_list`，每页为 `tab`；`RadioGroup`、`SegmentedControl` 每项为 `radio`；`Text`、`LinkLabel`、`Badge` → `label`；`Avatar` → `image`；`Modal`、`Sheet` → `dialog`；`Toast` → `status`（隐含 `polite`）；`ProgressBar`、`ProgressRing`、`Spinner` → `progress_indicator`；`NavigationStack` → `navigation`；`VirtualList` → `list`；`GameViewport` → `image`；容器 → `group`。

### U8.2 规则

- **只影响语义**：`semantics.*` 变化只标记 `SEMANTICS`，不得引起布局或 Paint；
- **状态投影**：`checked`、`value`、`range`、`expanded`、`selected` 由 Widget Property 自动投影（ADR 0021），作者不应在 `semantics.value` 中重复；
- **交互节点必须可访问**（CLAUDE.md §15）：在非标准交互 Widget 的节点（如 `Row`）或用户 Component 根节点上声明 `click`、`tap`、`double_tap`、`long_press`、`drag_*`、`swipe`、`pinch_*` 或 `key_down` Handler 时，该节点必须：(1) 设置非 `group` 的 `semantics.role`；(2) 有可访问名称（`semantics.label` 或可推导的文本内容）；(3) 有等价键盘路径，即 `focusable: true` 且处理 `click`，只处理 `tap`/`pointer_*` 不算。违反 (1) 或 (2) 是 `E3704`，违反 (3) 是 `E3708`，两者默认为警告，`viso check --a11y strict` 提升为错误；
- **Live Region**：设置 `semantics.live` 的子树文本变化时由 AccessKit 通告，`polite` 排队、`assertive` 打断；表单错误文本应使用 `polite`；
- **隐藏**：`semantics.hidden: true` 把子树从语义树移除但保留绘制，用于纯装饰；`visible: false` 同时移除绘制与语义；
- **合并**：`semantics.merge: true` 让节点读作一项（列表行的图标、标题、副标题）：没有 `semantics.label` 时，子树中各节点的 label 按树序以逗号连接成它的名称，这些节点不再单独出现；子树中自己能行动的节点（可聚焦、有 Handler、含链接，或自身也合并）保持为它的子节点，其 label 不并入；
- **辅助技术动作**：Focus、Click、Increment、Decrement 走普通输入路径（ADR 0030），触发同一套 Handler。

当前实现：View 的原生节点投影 `semantics.role` 与 `semantics.label`。`semantics.role` 是常量 `Role::<variant>`，其他值或未知 Variant 使视图无法挂载（消息列出全部 Variant）；未写时取上表的 Widget 默认角色（`Image`、`Svg` 无名称时为装饰）。可访问名称取 `semantics.label`，否则 `Text`、`Button` 取其 `text`；两者随所读 State 变化重新投影，只标记 `SEMANTICS`。`semantics.hint`、`semantics.value`、`semantics.hidden`、`semantics.heading_level` 同样随所读 State 重新投影；`semantics.live` 是常量 `LiveRegion::<variant>`，`semantics.merge` 是常量 `true` / `false`（其他值使视图无法挂载）。`semantics.hidden` 为真的节点连同子树留在界面上，但不进入语义树。`Tabs` 的直接子节点（含控制流区域中的）默认角色为 `tab`，`RadioGroup` 的为 `radio`，`NavigationStack` 为 `navigation`；作者写的 `semantics.role` 优先。AccessKit 与 Web 后端分别以 description/`aria-description`、value/`aria-valuetext`、live/`aria-live`、level/`aria-level` 发布。`Modal`、`Sheet` 默认角色为 `dialog`，`Toast` 为 `status`（`live` 默认 `polite`）。

标准交互 Widget 是 `Button`、`Toggle`、`CheckBox`、`Slider`、`TextInput`、`NumberField`、`Tabs`、`RadioGroup`、`NavigationStack`、`Disclosure`、`Modal`、`Sheet`、`InteractiveViewer`、`ReorderHandle`（Widget Schema 的 `interactive` 标记）。其他原生 Widget 的节点（包括用户 Component 视图的根节点）处理 `click`、`tap`、`double_tap`、`long_press`、`drag_*`、`swipe`、`pinch_*` 或 `key_down` 时：未绑定 `semantics.role`、或绑定为 `Role::group`，报 `E3704`；节点及其子树中没有 `semantics.label` 或 `text` 绑定，报 `E3704`；没有 `focusable`（或其值为字面量 `false`）、或不处理 `click`，报 `E3708`。均为警告，构建配置的 `a11y_strict`（`--a11y strict`）使之成为错误。调用方在用户 Component 节点上转发到其视图根节点的手势 Handler（不是该 Component 声明的 Event）按根节点检查：根节点与调用方的 Property 合并（调用方的后写优先），名称取两者任一处的 `semantics.label` 或 `text`；根节点是标准交互 Widget 时不报。

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
| `across` / `scroll_bars` / `tail` | `Bool`  | `false` / `true` / `false`| `MEASURE\|LAYOUT` / `LAYOUT` / `LAYOUT` | —（见 U3.7） |
| `width` / `height`         | `Sizing`       | `fill` / `fill` | `MEASURE\|LAYOUT` | 见 U3.3                |
| `on visible_range_changed` | `VisibleRange` | —               | —                 | —                      |
| `refreshing` / `on refresh` | `Bool` / —    | `false` / —     | `TRANSFORM\|PAINT` | —（见 U3.7）          |
| `snap` / `snap_inset` / `page_dots` / `on snap_changed` | `Snap` / `MixedLength` / `Bool` / `SelectionChanged` | 无 / `0dp` / `false` / — | — | —（见 U3.7） |
| `pages`                    | `Bool`         | `false`         | `LAYOUT`          | —                      |
| `columns` / `min_column`   | `U16` / `MixedLength` | 无 / 无  | `LAYOUT`          | 不声明（Percent 为 `E3711`） |
| `masonry`                  | `Bool`         | `false`         | `LAYOUT`          | —                      |
| `gap` / `column_gap`       | `MixedLength`  | `0dp` / `0dp`   | `LAYOUT`          | 不声明                 |
| 行根 `list.full_line`      | `Bool`         | `false`         | `LAYOUT\|PAINT`   | —                      |

`VisibleRange` 字段为 `first: U64`、`last: U64`、`count: U64`。

### U9.3 运行时语义（ADR 0008）

- **结构**：Scroll 视口内一张主轴范围固定的画布，行高由 Fenwick 树维护；未测量行使用 `estimated_extent`（可写 `em`，按列表节点的 Resolved Font Size 解析），测量后更新；
- **身份**：`key` 值经 `StableKey` 映射为 `ItemKey`，作为行身份；重排时存活行保留宿主节点与局部状态并重新锚定；滚动锚点保持在锚定行上，插入与删除不引起视觉跳动；
- **回收**：离开 Overscan 窗口的行宿主进入回收池按 Template 复用；复用即重新绑定新 Item，标记该行子树 `MEASURE|LAYOUT|PAINT`，**不**标记 `STRUCTURE`；行被回收时其局部 `state` 丢弃、行内 `start` 的 Task 取消，需要跨滚动保留的数据应放在列表外的 Model 中；
- **滚动**：只标记 `TRANSFORM`；进入视口的新行按上述规则挂载或复用；
- **命令式滚动**：`n.scroll_to_key(key)` 与 `n.scroll_to_index(i)` 可滚动到尚未挂载的行；未知 `key` 是 No-op；
- **语义**：列表 Role 为 `list`，每行为 `list_item`，语义树报告集合总数与行位置。
- **分页**：`pages: true` 让每行恰为视口在 `axis` 上的长度（Pager 的页），未写 `snap` 时取 `Snap::pages`；视口尺寸变化时保持当前页；横向分页列表配 `page_dots: true` 与 `scroll_bars: false` 即为 Pager；
- **网格**：写了 `columns`（常量 1–1024 条）或 `min_column`（常量长度，按视口横向放下的、不窄于它的条数，视口尺寸变化时重算并保持视口起点所在的项）的列表把行排进横轴上的条（lane）：`gap` 为主轴上相邻两项的间距，`column_gap` 为相邻两条的间距；条宽平分横轴。默认按行排列：每行放满一条一项，行长取该行最长的项（照片墙）；`masonry: true` 时每项放到当前最短的条末尾（卡片瀑布流），已测量的项保持所在条，前面的项被测量时后面的项不换条。行根的 `list.full_line`（可绑定，按 Item 求值）为 `true` 的项独占一整行（横幅）；分组标题总是独占一整行。只写 `masonry: true` 时为两条。网格按行吸附（瀑布流无吸附点）；
- **分组标题**：行根的 `sticky`（`sticky: item.is_header;`）按 Item 与 Key 一同求值；为 `true` 的行是分组标题：视口起点所在分组的标题停在起点边，下一个标题到达时将其推走；标题行滚出窗口后仍保持挂载并停靠；`scroll_to_index`/`scroll_to_key` 让非标题行停在其分组标题下方。

当前实现：`VirtualList` 的 Item Template 在宏、热重载与 Release Package（含 AOT）中挂载：列表节点是其 `axis` 上的 Scroll 视口，`for` 的 Items 与 Key 按普通 `for` 的规则求值（Key 经稳定哈希成为行身份），视口只挂载窗口与 Overscan 内的行；Items 变化时存活行按 Key 保留宿主、局部状态与焦点并以新 Item 重新投递，离开的行回收复用；`estimated_extent` 与 `overscan` 取常量（`em` 在挂载时按列表节点的字号折算）。HTML/Solid 导出挂载全部行，行带 `content-visibility: auto` 与按 `estimated_extent` 的固有尺寸，浏览器跳过屏幕外行的布局与绘制。`on visible_range_changed` 在布局之后投递：首次布局即报告，之后窗口变化才报告，`last` 为视口内最后一行（含）；导出页面由滚动与尺寸变化计算同一范围。列表 Role 为 `list`，每个行宿主为 `list_item` 并带 1 起的位置与集合总数（AccessKit `position_in_set`/`size_of_set`，Web `aria-posinset`/`aria-setsize`），导出页面中无自身 Role 的行根元素为 `listitem`。行内可以再有 `VirtualList`：它随所在行在同一帧挂载，所在行被回收时一并释放。网格的条数、间距与 `masonry` 取常量（`em`/`sp` 在挂载时按列表节点折算）；HTML 导出为 CSS Grid（`grid-template-columns: repeat(n, minmax(0, 1fr))` 或 `repeat(auto-fill, minmax(min(最小宽, 100%), 1fr))`，瀑布流另加 `grid-template-rows: masonry`，浏览器不支持时按行排列），分组标题跨整行；`list.full_line` 在导出页面中按普通项排列。

---

## U10. 国际化与 RTL

### U10.1 逻辑方向与 `env.layout_direction`

§96.2 的 `env` 包含本部分使用的两个字段：`layout_direction: LayoutDirection`（`LayoutDirection { ltr; rtl; }`）与 `locale: Locale`（BCP-47）。读取 `env.layout_direction` 的失效为 `MEASURE|LAYOUT`，读取 `env.locale` 为 `MEASURE|LAYOUT|PAINT|SEMANTICS`。

- 所有 `start`/`end`、`Row` 主轴、Grid 列序与 `absolute.start`/`absolute.end` 都按 `env.layout_direction` 自动解析，作者不需要读取它来翻转布局；它只用于非布局决策；
- 标准 Widget 的方向性图标（返回、展开箭头等）自动镜像；用户 `Icon`/`Image` 可设 `mirror_in_rtl: Bool = false`（失效 `PAINT`）；
- 变换（U4）、Canvas 坐标与指针坐标都是物理坐标，不翻转；
- Adaptive Scope（§96）可以局部覆盖 `layout_direction` 与 `locale`，例如内嵌的外语段落。

当前实现：RTL 下 `Row` 从右向左排列子节点（子节点的物理 `margin` 不随之互换，`margin.start`/`end` 已按方向解析），`Justify` 与 `Column` 的 `Align` 的 `start`/`end` 互换，`Stack` 的 `start`/`end` 对齐与 `Absolute` 的 `absolute.start`/`absolute.end` 以右边为起点（两者都未给出时贴右），`Grid` 列从内容盒右边起排、单元格内的子节点贴右；方向变化时整窗重新布局。`AdaptiveScope` 的 `layout_direction` 在其子树内覆盖窗口方向：布局、`padding`/`margin` 的 `start`/`end`、勾选标记的位置与子树内读取的 `env.layout_direction` 都按最近的设定解析；Web 导出给该元素写 `dir`。水平 `Scroll`（含水平虚拟列表）在 RTL 下从右边起排（U3.7）。没有强方向字符的段落以节点所在的布局方向为基础方向（U10.2），方向变化时这些文字重新排版。

### U10.2 Bidi

字符串以逻辑顺序存储与传递。段落基础方向默认由首个强方向字符决定，无强方向字符时回退到 `env.layout_direction`。双向重排、镜像字符与光标移动全部由 Text Runtime 负责。

### U10.3 `tr` 与 Localizable Property

`tr` 是库 API（`viso::i18n`），不是语法：

```viso
// fn tr(key: MessageKey, name: value, ..) -> String;

Text(tr("inbox.unread", count: unread))
```

- 消息参数以 Named Argument 按消息占位符的名字传入，每个恰好一次；
- 字符串字面量在期望 `MessageKey` 处于编译期转换为 `MessageKey` 并与项目消息目录核对；键不存在或参数名/类型与目录不符是 `E3706`；
- 复数、性别与数字格式由目录的消息格式决定；`tr` 对 `env.locale` 建立响应式依赖；
- `tr` 可在 View、Computed、Style、Action 中使用；它读取 Context，因此不能在纯 `fn` 中使用。需要在 `fn` 中选择文案时让 `fn` 返回 `MessageKey`，由 View 调用 `tr`（U13.3）；
- Schema 把用户可见文本 Property 标记为 Localizable：`Text.text`、`Button.text`、`Toggle.label`、`TextInput.placeholder`、`semantics.label`、`semantics.hint` 以及各 Widget 的标题文本；
- 在 Localizable Property 上用字符串拼接或含字面文字的插值字符串构造文本是 `E3705`（警告），应改用带参数的 `tr`，因为语序因语言而异；只含单个洞的 `"{n}"` 不受限；
- 纯字面量默认不报错；`viso check --i18n strict` 对 Localizable Property 上的字面量也报告 `E3705`。

当前实现：Widget Schema 把 `Text.text`、`Button.text`、`Toggle`/`CheckBox` 的 `label`、`TextInput.placeholder`、`semantics.label`、`semantics.hint` 标为 Localizable。其值（去掉括号后）是类型为 `String` 的 `+` 拼接，或含字面文字或多于一个洞的插值字符串时，报 `E3705`（警告）；构建配置的 `i18n_strict`（`--i18n strict`）使之成为错误，并对 `String` 字面量同样报告。

当前实现（消息目录）：目录是 `Viso.toml` 旁 `i18n/` 下每个 Locale 一个 TOML 文件（`i18n/en.toml`、`i18n/zh-Hant.toml`，文件名为 BCP 47 Tag，按规范形式比较），嵌套表与点分键组成消息键（`[inbox] unread = ".."` 即 `inbox.unread`），值是 ICU MessageFormat 字符串：`{name}`（文本，数字按读者 Locale 格式化）、`{name, number}`（可写 `integer` 样式，按半数取偶舍入为整数）、`{name, date}`（`short`、`medium`、`long`、`full`，缺省 `medium`；`full` 带星期）与 `{name, time}`（`short` 到分、`medium` 到秒，缺省 `medium`；`long`/`full` 需时区，报 `E3706`）、`{name, plural, ..}` 与 `{name, selectordinal, ..}`（`offset:n`、`=n` 精确匹配、`zero`/`one`/`two`/`few`/`many` 类别，必须有 `other`；`#` 是本 Case 的数减去 Offset）、`{name, select, ..}`（必须有 `other`），`''` 是撇号，`'` 后接 `{`、`}`、`#`、`|` 开始引用文字。源 Locale 由 `Viso.toml [i18n] source` 给出，缺省 `en`；源目录声明全部消息与其参数，参数的种类由用法推出：只出现在 `{name}` 的是文本（`String`、数字，或任一有 `Display` 的类型，按字符串插值的显示转为文本），出现在 `number`/`plural`/`selectordinal` 的是数字（整数或浮点），出现在 `select` 的是选择值（`String`，或无 Payload 的 Enum，按 Variant 名匹配），出现在 `date` 的是日期（Prelude 的 `Date { year, month, day }` 或 `DateTime`），出现在 `time` 的是时刻（`Time { hour, minute, second }`，各字段缺省 0，或 `DateTime`），同时出现在 `date` 与 `time` 的是 `DateTime { date, time }`；同一参数的两种用法没有值能同时满足（如既是数字又是选择值、既是日期又是文本）报 `E3706`。日期按公历、时刻按读者 Locale 的 12/24 小时制（Tag 的 `-u-hc-` 优先）写出，不涉及时区；非法日期（`2026-02-30`）或时刻（`24:00`）在运行时报错。翻译可省略消息（沿 CLDR 回退链取最近的已翻译 Locale，最终取源 Locale，如 `zh-TW` → `zh-Hant` → 源），只能使用源消息声明的参数且种类须被源的种类满足（数字与选择值可作文本用）；违反者、格式错误、非字符串值、非法或重复的 Locale、缺少源目录报 `E3706`（错误，该翻译的这条消息回退），源目录没有的键报 `E3706`（警告）。`tr(key, name: value, ..)` 恰有一个位置实参（键）：键是字符串字面量时须为源目录的消息（否则 `E3706`，附最接近的键），每个参数恰好按名给出一次、种类相符；键是其他 `MessageKey` 表达式时不得带参数。期望 `MessageKey` 处的字符串字面量须是不带参数的消息（`E3706`），其他 `String` 表达式不转换（`E2103`）；`MessageKey` 值在运行时就是键文本，可存入 State、比较与持久化，热重载后仍指向同一消息。`tr` 读取 `env.locale`，因此只用在 Component 的 View、Style、Computed、Action 与 Effect 中：`fn`、Task、State/Input 初始化器、Const、Theme 与 Module 级声明中报 `E2111`，System 中报 `E9109`。包没有消息目录而使用 `tr` 或 `MessageKey` 字面量报 `E3706`。编译器把每个 Locale 的消息编译为扁平的 Pattern 表并随 Module 进入线格式与 Release 包（只在有 `tr` 时）；未翻译的消息在编译期指向回退 Locale 的 Pattern，字面量键编译为消息序号，运行时一次索引，不按名字查找参数；读者 Locale 第一次出现时沿 CLDR 链解析一次并缓存，表在首次使用时解码，纯文本消息共享同一字符串。复数类别按消息所在 Locale 的规则、数字、日期与时刻按读者 Locale 的符号、分组与格式（`de-AT` 的 `1 234 567`、`ar-EG` 的 `٤`、`de-DE` 的 `26.10.26`）；日期与时刻的 CLDR 数据随 `viso` 的 `dates` Feature（缺省开启，不到 500 KB）链接，关闭时写作 ISO 8601（`2026-10-26`、`16:05`）。Web 导出用浏览器的 `Intl.DateTimeFormat` 写日期与时刻。`env.locale` 改变时读取它的 Binding 与 Computed 重新求值。`viso check` 报告目录问题并指向目录文件的行列；`view!`/`component!` 把目录文件列为编译依赖，目录错误是编译错误（指向目录文件的行列）；宏读的是源文件所属包的清单与目录：源文件与 crate 根之间有 `Viso.toml` 时取最近的一个，否则取 crate 根处或其上最近的一个；开发会话监视目录文件，编辑后以新目录重新编译该包的全部视图，目录有错误时保持原状。`MessageKey` 值找不到消息（热重载删除了它）时显示键文本本身。

---

## U11. 导航

> **状态**：已实现（Rust 侧 `viso_ui::router`，Architecture §6.1；DSL 侧 `viso::nav` 与 `RouterView`，ADR 0055 Proposed）。示例：`examples/04-navigation`。

路由是 **Rust 中定义的类型化路由**（CLAUDE.md §32）。DSL 不声明路由表，只使用经 Native Schema 导入的 `Route` Enum 与宿主提供的句柄 `Navigator<R>`：

- `Route` 是 Rust enum 派生 `#[derive(NativeData)]`，放进某个 Native Library 的 `types`；`.vs` 把它当作该 Library 声明的普通 Enum：构造、比较、穷尽 `match`，variant 名是 Rust 名的 snake case（`Route::message { id }`）。Library 可以在依赖的 native package 中，也可以在 app 自己的包里（manifest 的 `[package.metadata.viso.natives]`，库 target 中写 `pub static NATIVES`）；
- `Navigator<R>`（`viso::nav`，无需 import）的方法：响应式读取 `current() -> R`、`can_pop() -> Bool`、`depth() -> I64`；命令 `push(route)`、`replace(route)`、`pop() -> Bool`、`reset(route)`。

```viso
import mail::Route

export component Shell {
    input nav: Navigator<Route>
    state has_unsaved_changes = false

    view {
        Column {
            Button(tr("nav.settings")) { on click { nav.push(Route::settings) } }
            RouterView {
                navigator: nav
                height: 100%
                on back_requested(e) { if has_unsaved_changes { e.prevent_default() } }
            }
        }
    }
}
```

```rust
let router = router_view(Route::Inbox, routes! {
    Route::Inbox => viso::view!("inbox.vs"),
    Route::Message { id } => viso::view!("message.vs", id: *id),
    Route::Settings => settings,
})
.navigator(nav); // 先 cx.handle() 铸造，留给 on_open_url
viso::view!("shell.vs", nav: router)(cx);
```

- 根组件的 Input 由 `view!("x.vs", name: expr, ...)` 传入：标量（`Bool`、`I32`、`F32`、`I64`、`F64`、`String`）按值，`RouterView<R>` 成为 `Navigator<R>`；Rust 值的类型与 Input 类型不符、未知 Input 名、缺少无默认值的 Input 都是编译错误；
- `push`、`replace`、`pop`、`reset` 只能在 Action 与 Handler 中调用；导航立即改动返回栈，调用返回后宿主抬高 navigator 的 revision cell，一个 Transaction 内的多次导航在 flush 时一起生效；
- `current()`、`can_pop()`、`depth()` 可在 View 与 Computed 中读取：读 `nav` 的 binding 依赖 navigator 的 revision cell，Rust 侧（如 `on_open_url`）的导航同样触发它们；
- `RouterView` 是导航出口：它在自身内部构建一次 `navigator` 背后的 Rust `RouterView`，页面来自 Rust 的 `routes!` 表（可以是 `view!`）；页面节点以路由值为 Key，返回栈中已有页面时保留其状态；
- 系统返回（Android Back、Escape、鼠标后退键）先派发 `back_requested(BackRequest)` 给最内层显示多于一页的 `RouterView`，未被 `prevent_default()` 时执行 `pop()`；只剩根页时事件交给宿主；
- 路由参数经 Route Variant 字段传递，URL 与深链接解析在 Rust 中完成（`Application::on_open_url` 中 `reset` / `push`）；
- 页面切换的转场与 `NavigationStack` 相同；切换后焦点移到新页面首个 `heading`，没有时移到页面根节点；
- 开发运行时重新挂载与 release package 挂载的视图，根 Input 取其默认值。

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
| `dragging`      | 节点是当前拖动手势的胜者，或其数据正被拖放携带    |
| `drop_over`     | 携带的数据在节点上方，节点是取用它的放置目标      |

- 选择器变化时源节点标记 `STYLE`，加上所有引用该选择器的 `when` 块中 Property 失效类别的并集（只切换颜色的 hover 是 `STYLE|PAINT`）；
- 同一 Style 内 `when` 块按源码顺序应用，后者覆盖前者；标准 Style 按"静止 → `hover` → `pressed` → `focus_visible` → `disabled`"书写，得到 `pressed > hover > 静止` 的优先级；
- 节点显式 Property 优先于所有 Style（§59），因此依赖选择器的外观应写在 Style 中。

当前实现：带标准输入事件的原生 Widget 支持 `hover`、`pressed`、`focused`、`focus_visible`、`selected`、`dragging`、`drop_over`；有 `Bool` 属性 `enabled`、`checked`、`invalid`、`expanded` 的 Widget 分别支持 `disabled`、`checked`、`invalid`、`expanded`（`expanded` 目前即 `Disclosure`）。交互状态由节点存储从路由器记录的事实导出：`hover` 是悬停目标或其祖先，`pressed` 是主按钮按下的命中节点或其祖先、直到该指针抬起，指针移出某个节点的盒子时该节点不再 `pressed`、移回再恢复，触摸平移胜出时整个按下取消；`click`、`tap` 只在按下所在、指针仍在其内的节点上抬起时派发，因此在别处抬起或被平移取消的按下不产生点击。`focused` 是焦点节点，`focus_visible` 是键盘或辅助技术移入的焦点，`selected` 是 `Tabs`、`RadioGroup` 选中的直接子节点（Web 导出为 `aria-selected`，单选项为 `aria-checked`）。存储维护一个修订单元，在 Flush 开始时、上述事实自上一轮变化后提升一次；读取交互选择器的节点依赖它并只重新求值自身的外观入口。

### U12.2 标准 Theme Schema

`theme` Context 的类型是 `viso::theme::Theme`；`theme X { ... }` 未给出的字段取 Record 默认值，无默认值字段必须提供。

```viso
export record Theme {
    colors: ColorPalette
    typography: TypographyScale = TypographyScale {}
    spacing: SpacingScale = SpacingScale {}
    radius: RadiusScale = RadiusScale {}
    elevation: ElevationScale
    motion: MotionScale = MotionScale {}
}
```

| Record            | 字段（类型 = 默认）                                                                 |
| ----------------- | ----------------------------------------------------------------------------------- |
| `ColorPalette`    | 均为 `Color`、无默认：`background` `foreground` `surface` `on_surface` `primary` `on_primary` `primary_hover` `accent` `muted` `outline` `error` `on_error` `focus_ring` `scrim` |
| `TypographyScale` | `base_size: Sp = 14sp`、`caption_size: Em = 0.85em`、`title_size: Em = 1.25em`、`headline_size: Em = 1.6em`、`family: FontFamily = FontFamily::system_ui` |
| `SpacingScale`    | 均为 `Dp`：`xsmall = 2dp`、`small = 4dp`、`medium = 8dp`、`large = 16dp`、`xlarge = 24dp` |
| `RadiusScale`     | 均为 `Dp`：`small = 4dp`、`medium = 8dp`、`large = 12dp`                            |
| `ElevationScale`  | 均为 `Shadow`、无默认：`low` `medium` `high`；`Shadow` 见 U3.3 |
| `MotionScale`     | `short: Duration = 100ms`、`medium = 200ms`、`long = 350ms`、`standard: Easing = Easing::ease_out`、`emphasized: Easing = Easing::ease_in_out` |

- `typography.*_size` 是 `Em`，用作 `font_size` 时相对父节点 Resolved Font Size（§19.4）；需要与嵌套深度无关的层级时，写成 `theme.typography.base_size` 的倍数；
- 读取 `theme.*` 的 Binding 在 Theme 整体替换时按各自 Property 的失效类别失效，替换本身不引入额外 Dirty Class；
- 应用自定义 Theme 字段通过扩展 Record 提供，扩展方式沿用 §60 的 Theme Base 规则。

当前实现：Record 与字段、默认值如上表；设置 Theme 时其 `typography.base_size` 成为根 Resolved Font Size、`typography.family` 成为未设置 `font_family` 的文本的字体族，HTML 导出的 `body` 读取对应的 `--typography-base-size` 与 `--typography-family`。`Color` 值为 `0xRRGGBBAA` 整数，长度为数值，`Duration` 为秒，`Easing` 为变体序号。

---

## U13. 完整示例

以下示例只使用本部分与 §40–§60 定义的构造。

### U13.1 设置页：分组与双向绑定开关

```viso
import viso::widgets::{Scroll, Column, Row, Text, Toggle, Slider}
import viso::i18n::tr

export record AppSettings { notifications: Bool = true; dark_mode: Bool = false; volume: F32 = 0.5 }

export component SettingsSection {
    input title: String

    view {
        Column {
            width: fill
            gap: theme.spacing.small
            padding: EdgeInsets::axes(inline: theme.spacing.large, block: theme.spacing.medium)
            Text(title) {
                font_size: theme.typography.title_size
                font_weight: semibold
                semantics.role: heading
                semantics.heading_level: 2
            }
            slot
        }
    }
}

export component SettingsPage {
    state settings: AppSettings = AppSettings {}

    view {
        Scroll {
            Column {
                width: fill
                max_width: 640dp
                SettingsSection {
                    title: tr("settings.general")
                    Toggle(tr("settings.notifications")) {
                        bind checked <=> settings.notifications
                    }
                    Toggle(tr("settings.dark_mode")) { bind checked <=> settings.dark_mode }
                }
                SettingsSection {
                    title: tr("settings.sound")
                    Row {
                        width: fill
                        gap: theme.spacing.medium
                        align: center
                        Text(tr("settings.volume"))
                        Slider {
                            width: fill
                            enabled: settings.notifications
                            semantics.label: tr("settings.volume")
                            bind value <=> settings.volume
                        }
                    }
                }
            }
        }
    }
}
```

`SettingsSection` 的裸子节点进入其 View 中的 `slot`。`Toggle.checked` 与 `Slider.value` 在 Schema 中为 `two_way`，配对事件为 `changed`。`settings.notifications` 只经 `Slider.enabled` 影响 `STYLE|HIT_TEST|SEMANTICS`，不引起 `STRUCTURE`。

### U13.2 聊天消息列表：虚拟化、Keyed、自适应

```viso
import viso::widgets::{VirtualList, Column, Row, Text}
import viso::i18n::tr

export record ChatMessage { id: MessageId; author: String; body: String; mine: Bool }

component MessageBubble {
    input message: ChatMessage
    input compact: Bool = false

    computed bubble_max: Percent = if compact { 85% } else { 60% }

    view {
        Row {
            width: fill
            justify: if message.mine { Justify::end } else { Justify::start }
            padding: EdgeInsets::axes(inline: theme.spacing.medium, block: theme.spacing.xsmall)
            Column {
                max_width: bubble_max
                padding: 0.6em
                corner_radius: theme.radius.large
                background: if message.mine { theme.colors.primary } else { theme.colors.surface }
                if !message.mine {
                    Text(message.author) {
                        font_size: theme.typography.caption_size
                        color: theme.colors.muted
                    }
                }
                Text(message.body) {
                    soft_wrap: true
                    selectable: true
                    color: if message.mine { theme.colors.on_primary } else {
                        theme.colors.on_surface
                    }
                }
            }
        }
    }
}

export component ChatMessageList {
    input messages: List<ChatMessage>

    action jump_to(id: MessageId) { list.scroll_to_key(id) }

    view {
        node list: VirtualList {
            height: fill
            estimated_extent: 3.5em
            overscan: 6
            semantics.label: tr("chat.messages")
            for message in messages key message.id {
                MessageBubble {
                    message: message
                    compact: env.size_class == SizeClass::Compact
                }
            }
        }
    }
}
```

`for` 位于 `VirtualList` 内，是 Item Template，只挂载可见行与 Overscan 行。`message.id` 是行身份，新消息插入时既有行保留状态与锚点。`bubble_max` 的 Percent 以行内容盒宽度为基准；`env.size_class` 变化只标记 `MEASURE|LAYOUT`。`estimated_extent: 3.5em` 随 `env.text_scale` 与 `theme.typography.base_size` 缩放。

### U13.3 表单：计算校验、禁用提交、错误文本与 Task 提交

```viso
import viso::widgets::{Column, Text, TextInput, Button, Spinner}
import viso::i18n::{tr, MessageKey}

export record SignupForm { email: String = ""; password: String = "" }

@derive(Eq)
export enum FieldIssue { malformed; too_short }

@derive(Eq)
export enum SubmitPhase { idle; submitting; failed(String); done }

fn check_email(value: String) -> Option<FieldIssue> {
    if !value.is_empty() && !value.contains("@") { return Option::Some(FieldIssue::malformed) }
    return Option::None
}

fn check_password(value: String) -> Option<FieldIssue> {
    if !value.is_empty() && value.length() < 8 { return Option::Some(FieldIssue::too_short) }
    return Option::None
}

fn issue_key(issue: FieldIssue) -> MessageKey {
    return match issue {
        FieldIssue::malformed => "form.email.malformed"
        FieldIssue::too_short => "form.password.too_short"
    }
}

task create_account(form: SignupForm) -> Result<AccountId, SignupError>
    requires { network::http } {
    return await Accounts::create(form)
}

component FieldError {
    input issue: Option<FieldIssue>

    view {
        match issue {
            Option::Some(found) => {
                Text(tr(issue_key(found))) {
                    color: theme.colors.error
                    semantics.live: polite
                }
            }
            Option::None => {}
        }
    }
}

export component SignupPage {
    event signed_up(id: AccountId)

    state form: SignupForm = SignupForm {}
    state phase: SubmitPhase = SubmitPhase::idle

    computed email_issue: Option<FieldIssue> = check_email(form.email)
    computed password_issue: Option<FieldIssue> = check_password(form.password)
    computed complete: Bool = !form.email.is_empty() && !form.password.is_empty()
    computed valid: Bool = complete && email_issue == Option::None && password_issue == Option::None
    computed submitting: Bool = phase == SubmitPhase::submitting

    action submit() {
        if !valid || submitting { return }
        phase = SubmitPhase::submitting

        start create_account(form) as submit_job {
            policy = [TaskPolicy::keep_latest]
            success (id) { phase = SubmitPhase::done; emit signed_up(id) }
            error (reason) { phase = SubmitPhase::failed(reason.message()) }
            cancelled { phase = SubmitPhase::idle }
        }
    }

    view {
        Column {
            gap: theme.spacing.medium
            padding: EdgeInsets::all(theme.spacing.large)
            node email_field: TextInput {
                placeholder: tr("form.email")
                invalid: email_issue != Option::None
                enabled: !submitting
                autofocus: true
                bind value <=> form.email
                on submitted { password_field.focus() }
            }
            FieldError { issue: email_issue }
            node password_field: TextInput {
                placeholder: tr("form.password")
                secure: true
                invalid: password_issue != Option::None
                enabled: !submitting
                bind value <=> form.password
                on submitted { submit() }
            }
            FieldError { issue: password_issue }
            Button(tr("form.create_account")) {
                enabled: valid && !submitting
                on click { submit() }
            }
            match phase {
                SubmitPhase::submitting => {
                    Spinner { semantics.label: tr("form.submitting") }
                }
                SubmitPhase::failed(message) => {
                    Text(message) {
                        color: theme.colors.error
                        semantics.live: assertive
                    }
                }
                _ => {}
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
