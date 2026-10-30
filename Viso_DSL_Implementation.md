# Viso DSL 1.0：实现与工具

> 文档状态：Viso DSL 1.0 Draft，与 `Viso_DSL_1.0.md` 同步修订  
> 规范级别：编译器架构与 Lowering 为说明性；工具合同（§136–§143）与验收（§151–§158）为规范性  
> 范围：编译器架构、工具合同、实现阶段与验收，以及 `Viso_DSL_1.0.md` 各节的当前实现说明（按原章节归档）。章节编号在 DSL 文档集内全局唯一；语言本身（文法、类型、运行时语义）以 `Viso_DSL_1.0.md` 为准。

---

# 第零部分：语言各节的当前实现说明

## 22. Compilation Unit

当前实现：一个文件单独编译时（`view!`、`component!`、热重载、`viso dump`/`export`/`snapshot`），编译器读它直接或间接 import 的包内 Module（`a::b` 对应 `src/a/b.vs` 或 `src/a/b/mod.vs`），与它组成同一个 Module Graph 一起解析、检查与降级，不读包的其余文件；被 import 的 Module 有错误时，每个错误报在读它的那条 `import` 上（`in `model`: …`，related 指向它在该 Module 中的位置）。`view!`/`component!` 把被读的文件列为编译依赖，开发会话在其中任一文件变化时重新编译 import 它的文件。按名 import（`import model::{twice};`）的 `fn`、`const`、Record 与 Enum 可用，经整模块 import 以路径读取（`import model;` 后写 `model::twice(x)`、`model::Size { .. }`、`model::Turn::left`、类型 `model::Size`）与按名 import 相同：名字须是该 Module `export` 的声明，未导出或不存在时在该名字处报 `E2001`（附 `export` Fix 或最近的导出名）；View 内联本文件与经 import 可见的 Component 与 Template：被 import 者在其声明 Module 中检查与降级（读该 Module 的 `const`、Style 与 `fn`），其中的诊断报在本文件放置它的节点或 `use` 上并注明所在 Component；经 import 可见的 Style 与本文件的同样应用，其值在声明它的 Module 中检查与降级，`when` 无法切换的诊断报在本文件节点的 `styles` 上。同名 import 两次报 `E2002`；`const` 的值直接或经其调用的 `fn` 读到自身报 `E2003`。

## 31. Impl

当前实现：Trait 缺少必需成员、Impl 成员不属于该 Trait、签名与 Trait 声明不一致、Bound 不是 Trait 时报 `E2201`；同一 Trait 对同一类型有两个可能重叠的 Impl 报 `E2202`。

## 36. Task

当前实现：Task 编译为 Task Chunk，在 Behavior VM 的 Fiber 上运行：Task Native（如 `viso::time::sleep`）交出一个 Future 时 Fiber 挂起，结果在帧边界交回后从原处继续；Task 直接调用另一个 Task 时在同一个 Fiber 上运行并随之挂起。`await` 只标出挂起点；Action、Event、Effect 等非 Task 主体中的 `await` 报 `E4101`。启动时的快照：第一个挂起点之前，Task 读到的就是启动那一刻的 State 与 Input；挂起点（`await` 或 Task 调用）之后直接读取 State、Input、Computed，或调用读取它们的 `fn`，以及在含挂起点的循环中读取，报 `E4102`；绕过静态检查（例如经由挂起前创建的闭包）的挂起后读取在运行时以 `E4102` 故障结束该 Task。Task 不写 State、不 `emit`（`E2501`），结果经 Start Handler 交回。`viso::time::sleep` 使用宿主安装的 `Timers` 服务，未安装时使用进程共享的计时线程。

## 37. Effect

当前实现：`when (...)` 编译为一个返回各依赖值列表的纯 Region Entry，Body 编译为一个 Effect Chunk，返回 `cleanup` 闭包（没有 `cleanup` 时返回 `None`）；二者随组件布局写入 Module（`Component::effects`），由控制流区域挂载的组件实例的 Effect 写入所在分支的模板。依赖覆盖：Body 在 `untracked(..)` 之外读取的 State、Input 与 Computed，若不被 `when` 中的表达式读取，且（对 Computed）其底层 State 与 Input 不全被 `when` 直接读取，报 `E4201`；`when` 中读取的 Computed 不覆盖其底层值，因为它可能在底层变化时保持不变。`when` 中的表达式按 Computed 的调用权检查（副作用报 `E2502`）；System 中的 `effect` 报 `E9109`。视图的 Effect 挂在视图根节点上，区域内实例的 Effect 挂在该实例的根节点上（View 的根本身是控制流区域、没有自己的节点时，挂在该次挂载持有的树外节点上，随挂载释放），每次挂载各自运行；运行时语义见 §91。热重载在替换代码前取消旧 Module 的全部 Effect，各 Cleanup 以创建它的旧代码运行恰好一次，替换后重新挂载新 Module 的 Effect：`mount` 与 `mount_and_change` 重新运行一次，`change` 等待下一次变化。Body 中的 `return` 发生在 `cleanup` 求值之前时，该次运行没有 Cleanup。

## 38.3 Resource 状态

当前实现：`resource` 是组件成员（System 中报 `E9109`），类型必须写作 `Resource<T, E>`（否则 `E2103`）。`load`、`key` 缺少或任一 Item 重复报 `E4301`，未知 Item 是语法错误。`policy` 是 `ResourcePolicy::keep_latest`、`debounce(d)`、`cache_for(d)`、`cache_errors` 的列表，各至多一次，`d` 为大于零的常量 Duration；`cache_errors` 需要 `cache_for`；`scope` 是 `ResourceScope::component` 或 `ResourceScope::shared`；违反者报 `E4302`。`key` 是纯表达式（Computed 上下文），类型须为 `StableKey`（整数、`Bool`、`Char`、`String`、`Unit`，以及由它们组成的 Tuple、Record、`Option` 与 Enum），否则报 `E2701`。`load` 必须是 Task Call（含 Native Task），否则报 `E4401`；其参数是纯表达式，结果类型须为 `Result<T, E>`（`E2103`）。Resource 只经 `.state`（`ResourceState<T, E>`）读取，可在表达式与 View `match` 中匹配并做穷尽检查（`E2301`）；对其赋值报 `E2110`；其他成员名报 `E2001`。

## 39. Start Statement 与 Task 生命周期

当前实现：`start` 只出现在 Action、Event Handler 与 Effect 主体（含其 `transaction`）中；在 `cleanup`、`fn`、Task 与初始化器中报 `E2501`，在 View 与 Computed 中报 `E2502`；目标不是 Task Call（含 Native Task）报 `E4401`；Module 级 `action` 中的 `start` 没有所属实例，报 `E4501`；System 中的 `start` 报 `E9109`。调用参数在启动它的 Transaction 中求值，Task 在该 Transaction 提交后启动。`success(p)` 绑定返回 `Result` 的 Task 的 `Ok` 载荷，否则绑定整个返回值；`error(p)` 绑定 `Err` 载荷，用在不返回 `Result` 的 Task 上报 `E2103`。三种 Handler 各自是一个新的 Transaction（Event 主体的调用权），以闭包捕获启动处读到的局部值，从不在启动它的 Transaction 内运行：`success` / `error` 在 Task 返回后的帧边界运行（即使 Task 未挂起就返回），`cancelled` 在取消它的 Transaction 提交之后运行。`policy = [..]` 恰好一个 `TaskPolicy::keep_latest`、`drop_new`、`queue` 或 `parallel(n)`（`n` 为不小于 1 的整数字面量），且只用于命名 Slot；具名 Slot 默认 `keep_latest`；未命名的 `start` 不受限并行。策略不合法、重复的 `policy` 或重复的 Handler 报 `E4302`。

## 57. Part

当前实现：`part name: T { .. }` 是带局部名 `name` 的节点，同一 View 中重复的 Part 名报 `E2002`；Component 与 Template 都可暴露 Part。`override part`/`replace part` 写在放置该 Component 的节点体或 Template 的 `use` 体中，名字须是其 Part（否则 `E3501`），写在 Widget 节点上或 View 顶层报 `E2103`。`override part` 的绑定与 Handler 属于调用方（读调用方的 State，在调用方的执行域中求值），按 Part 的节点类型检查（`E3101`、`E3102` 等）；它取代 Part 自身对同一 Property（`:` 或 `bind`）的绑定与同一 Event 的 Handler，其余保留。可覆盖的是 Widget Part 的全部 Property 与 Event；Part 是用户 Component 时其 Input 不经 `override` 覆盖（`E3711`），可用 `replace`。`replace part` 的 View Block 属于调用方，必须恰好挂载一个节点（否则 `E3502`）。Part 位于定义方 `for`/`match` 之内时，调用方的 `override`/`replace` 无法在其中求值（`E3711`）。

## 58. Template

当前实现：Template 是没有 State 的 Component，参数是它的 Input，与 Component 实例一样在编译期内联进放置它的 View（§40.1），每个 `use` 一个实例，实例身份为 `Template名#序号`。成员只能是 `slot`、`const`、`fn` 与 `view`，其他成员或缺少 `view` 报 `E3601`。`use` 的实参按位置或 `name: value` 给出参数，每个参数至多给一次（`E3102`），未知参数名报 `E3101`，多余实参与缺少无默认值的参数报 `E2103`，实参按参数类型检查；`use` 体只含 `fill`、`override part`、`replace part`（其他项 `E3601`）。Template 只能以 `use` 放置：写成节点 `T { .. }`，或 `use` 一个 Component，报 `E2103`。经 `use` 直接或间接放置自身所在的 Template 报 `E3601`：参数是运行期值，无法在编译期终止展开。展开后的节点保留定义处的 Source Origin，其实例记录调用处（`use` 或节点）的 Source Origin；挂载失败（`E3711`）的诊断同时指向两处。经 import 可见的其他文件的 Template 与本文件的同样内联。

## 59. Style

当前实现：Style 的目标是本文件可见的原生 Widget 或用户 Component（否则 `E2001`）；Base 是本文件或经 import 可见的同目标 Style（名字无法解析报 `E2001`，解析为非 Style 或目标不同报 `E2103`），Base 图有环的 Style 报 `E2003`。Style 只绑定 Widget Schema 标为 Styleable 的 Property（各节点的布局、外观、变换、`transition.*`、容器的间距与对齐、文本样式；`styles`、内容、焦点与语义属性不是）或用户 Component 的 `@styleable` Input，否则 `E3101`；同一块（顶层或同一 `when`）内重复绑定报 `E3102`；值按 Property 类型检查（`E2103`），按 View 上下文检查副作用（`E2502`），可读 `theme`。`when` 的选择器只由选择器名、`!`、`&&`、`||` 与括号组成（否则 `E2103`），名字须是目标支持的选择器（U12.1，否则 `E2001`）。节点的 `styles` 是本文件或经 import 可见的 Style 名的列表（不是列表、元素不是名字或不是 Style 报 `E2103`，Style 目标不是节点类型报 `E2103`）。应用：每个 Style 先按顺序深度优先应用其 Base，再按源码顺序应用自身各项；对同一 Property，无条件绑定取代此前的全部值，`when` 绑定作为分支叠加，后写的优先；节点自身绑定（`:` 或 `bind`）的 Property 不取 Style 的值。`background` 与 `opacity` 编译为节点的基值入口加按优先级排列的分支（选择器的后缀式与值入口，入口注册在节点 `styles` 值的 Site 上，按确定顺序编号），运行时取第一个成立的分支，否则取基值，都没有时取属性默认值；`transition.background`/`transition.opacity` 取无条件值；其他 Property 的无条件值与节点自身的值同样折叠或成为 Binding，其上的 `when` 报 `E3711`。选择器中的 `disabled`、`checked`、`invalid` 读取节点自身的 `enabled`、`checked`、`invalid`（未绑定时取默认值并在编译期折叠：恒假的分支删除，恒真的分支成为基值）。用户 Component 节点上的 `styles` 给出其 `@styleable` Input（U2.3）。热重载、Release 包与宏路径相同。

## 60. Theme

当前实现：`theme X { .. }` 是类型为 `Theme`（U12.2）的常量：每个 Item 给出同名字段的值，未给出的字段取 Base（`theme X: B`，`B` 必须是 `Theme` 值）的对应字段，没有 Base 时取 Record 默认值。Item 名不是 `Theme` 字段报 `E2001`，同一字段给出两次或值类型不符报 `E2103`，没有 Base 时缺少无默认值的字段（`colors`、`elevation`）报 `E2103`；Base 不存在报 `E2001`、不是 `Theme` 报 `E2103`；同一模块内经 Base 或 Item 表达式互相依赖的 Theme 报 `E2003`。Item 表达式按 Initializer 检查（调用 Action 报 `E2501`），可以读取其他 Theme 与常量；Theme 的 Item 表达式中 `theme` 是其 Base 的值（`theme Dark: Light { colors = ColorPalette { primary: theme.colors.accent, ..theme.colors }; }`），没有 Base 的 Theme 中读取 `theme` 报 `E2111`。宿主按名字切换 Theme，因此一个包内 Theme 名唯一（跨模块重名报 `E2002`）；编译结果在 Module 中记录名字到常量 Chunk 的表，随线格式进入 Release 包。`theme` 上下文是状态存储环境的一个字段：宿主以 `ViewHost::theme(name)` 求得包内 Theme 的值，以 `set_theme(states, value)` 原子替换，读取 `theme` 的 Binding 按各自 Property 的失效类别重新求值，未读取的不受影响，Handler 此后读到新值；替换前存储提供与 U12.2 默认值一致的浅色 Theme（`default_theme()`），因此不存在缺失 Context 的状态。热重载重新编译全部 Theme，已设置的值保留到宿主再次替换。

## 79. Trait 和约束

当前实现：未满足的 Bound 报 `E2201`，同一层多个候选报 `E2202`。Generic 参数的 Method 经其 Bound 及 Super Trait 解析；`Trait::f(x)` 在 `Self` 确定后解析到对应 Impl 成员。Auto Trait 由 Prelude 提供：`Clone`（所有类型）、`Eq`（函数类型与 `dyn` 以外）、`Hash`（满足 `Eq` 且不含浮点）；对无 `Eq` Bound 的 Generic 参数使用 `==`/`!=` 报 `E2201`。

## 90. Event Dispatch

当前实现：Handler 的事件参数有 `stop_propagation()`、`stop_immediate_propagation()` 与 `prevent_default()` 三个无参方法（带参数是 `E2103`；模块自己的 Record 声明了同名方法时用它的），降为 `viso::event` 的同名 action。View 宿主在 Handler 返回后应用请求：`stop_propagation` 停止后续节点，`stop_immediate_propagation` 还跳过同一节点余下的 Handler，`prevent_default` 取消默认动作——目前有默认动作的是 `link`（交给系统打开）。

## 91. Effect 调度

当前实现：State Flush 的每一轮只把 Effect 排入队列（§86）：先是上一轮以来挂载的 Effect，再是依赖单元被本轮写入的 Effect；整帧在 `Submit` 之后的 `RunEffects` 阶段按入队顺序运行队列，每个 Effect 每帧至多一次。Effect 先登记 `when` 读取的单元为依赖，再求值依赖列表并与上次的值按值比较（`==`），由 Run Policy 决定是否运行 Body：`mount` 只在首次，`change` 在首次之后值变化时，`mount_and_change` 在首次与值变化时；值未变的唤醒什么都不做。Body 作为一个 Transaction 运行，读取不登记依赖，写入进入下一帧的 Transaction；运行前先执行上一次运行返回的 Cleanup，节点释放时同样执行。一帧 flush 连续 16 轮仍有待处理写入即 `E4202`；一个 Effect 连续 16 帧仅因 Effect 的写入被唤醒，同样是 `E4202`（跨帧反应环），该 Effect 停止运行，直到外部写入再次唤醒它。`viso run` 下该诊断在源码上标出：State 指向其声明，Effect 指向其 Body，与编译诊断同样渲染。

## 92. Task 与结构化并发

当前实现：视图宿主把每个 Task 交给 UI 任务协议（ADR 0032）运行，不自带执行器：Task 每次挂起由一个 UI Task 等待 Native 交出的 Future，其 Continuation 在帧边界恢复 Fiber。UI Task 归启动它的组件实例所有：视图自身与其静态子实例的 Task 属于视图根节点，控制流区域挂载的实例的 Task 属于该实例在此次挂载中的根节点；释放该节点即丢弃 Task，不运行任何 Handler。Task Slot 以（区域挂载、实例、Slot 名）为键，按启动时的策略处理正在运行的同名 Task：`keep_latest` 取消前序 Task 并运行其 `cancelled`；`drop_new` 丢弃新 Start；`queue` 与 `parallel(n)` 排队，直至 Slot 有空位。热重载在应用结构补丁之前取消全部 Task，各 `cancelled` 以启动它的旧代码运行，其写入随状态迁移保留；此前交出的 Continuation 在替换后失效。

## 93. Resource 运行时

当前实现：Resource 编译为一个 Effect：依赖是 `key`，主体启动 Loader Task（无 Handler），另有读写其状态槽的入口；状态初值 `idle`。挂载与每次 Key 值变化后：命中缓存则直接写入缓存状态；否则状态进入 `loading`（持有值时为 `reloading(value)`），再启动 Loader（有 `debounce` 时在延迟后启动，期间再变化会重新计时）。Loader 返回 `Ok(v)` 写 `ready(v)`、`Err(e)` 写 `error(e)`，各为一个新 Transaction，且仅当当前 Key 仍等于该次加载的 Key 时写入：过期结果从不覆盖新 Key 的状态。`keep_latest` 在 Key 变化时取消进行中的加载；没有它，旧加载照常完成并填充缓存。`cache_for(d)` 在结果提交时开始计时，到期移除；`cache_errors` 使错误同样缓存。缓存按挂载（`ResourceScope::component`）与 Key 值保存；加载、去抖与缓存到期都是该实例节点的 UI Task，卸载即全部丢弃。`ResourceScope::shared` 的加载与缓存属于该声明在 View 中的池：某 Key 已在加载时，其他实例加入等待而不再启动 Loader，结果作为各自的 Transaction 写入每个仍处在该 Key 的等待者；Loader 在发起它的实例的作用域里求值参数（Key 相同即视为同一次加载）；`keep_latest` 只在已无实例等待时取消加载；池的加载与缓存到期是 View 根节点的 UI Task，发起它的实例卸载后照常完成并填充缓存，去抖仍属各实例。热重载以旧代码取消进行中的加载，状态按类型迁移保留，重新挂载后再次加载（新 Loader 即新 LoaderVersion，缓存不跨越热重载）。Loader 的 Capability 在其 Fiber 首次调用 Native 时检查（同步发生在启动内，早于任何异步工作）；被拒绝时报 `E6103`，状态停留在 `loading`。

## 94.3 Sensitive State

当前实现：编译结果记下每个 `@sensitive` 状态（`CandidatePlan::sensitive`），以及读取它们的属性所在的静态节点（`sensitive_nodes`）。落盘的快照由宿主按 App 运行的版本请求：每个文件的这些状态的 Cell 与链接到它们的槽位、这些节点的编辑文本，以及有敏感状态的文件中 Region 节点的文本都被省去；所有 `secure` 输入框的文本一律省去；宿主不知道版本的文件省去全部状态与文本。内存中的 Warm Restart 快照照旧完整。Rust 状态 Cell 与扩展（`AppCx::dev_state`）按原样写入。

## 95. Capability 与执行预算运行时

当前实现：包的授权（`Viso.toml [package] capabilities`，`viso check` 与 `view!`/`component!` 宏读取同一字段）随编译结果写入 Module 及其线格式；视图宿主按 Module 携带的授权链接 Native，Release 包与内嵌模块因此带着构建时的授权运行。热重载的候选不读清单，替换后的宿主沿用被替换宿主的授权与已安装的宿主服务，编辑源文件不能扩大授权。Handler 调用未授权的 Native 时报 `E6103`（指出 Native 与缺少的 Capability），该 Transaction 的状态写入与事件全部丢弃，视图继续处理后续事件。预览宿主以指定授权链接、忽略包的授权，默认授权为上述三项（`PREVIEW_CAPABILITIES`）。`viso preview <file> [<component>] [--grant <capability>]...`（CLI §23.7）单独挂载生成的组件，在 headless 预览宿主上运行，授权为这三项加上 `--grant` 指定的项。每个 fault 都在其源码位置报告，`E6103` 也在其中。

## 99.1 Loop 规则

当前实现：Shader 自己作用域内的名字——参数、`let` 局部、成员与 Shader `fn`——加上 Intrinsic；类型名取 §98 的封闭集合或 Resolver 绑定的 `@shader_value` Record（字段必须都是 Shader 值类型，不能含自身，不带默认值），其余 Host 类型（`String`、`I64`、`List`、Tuple、Enum、未标记的 Record、`Color` 字面量等）报 `E8101`，`F64` 与 `f64` 字面量报 `E8102`。无后缀整数字面量取另一操作数或实参的标量类型，无上下文为 `I32`，在 `F32` 上下文中直接定型为 `F32`；无后缀浮点字面量在整数上下文报 `E2103`；不同标量之间没有隐式转换（`E2102`，用 `as` 显式转换同 Lane 数的数值标量或向量）。运算：同类型数值标量/向量逐 Lane 运算，向量与其 Lane 标量双向广播；矩阵加减同型矩阵，乘矩阵、同维向量或 `F32`；位运算与移位只在整数上；比较只比较同类型标量，结果 `Bool`。构造写成类型调用：`Vec4F32(rgb, 1.0)` 按顺序填满 Lane（单个标量填满所有 Lane），`Mat3F32(c0, c1, c2)` 按列或按列给出全部标量，`ColorLinear(r, g, b, a)`；Record 用 Record 表达式给出全部字段（无 `..base`）。字段访问取 Record 字段，向量与 `ColorLinear` 取 `xyzw` 或 `rgba` 的一到四个 Lane；`v[i]`/`m[i]` 取 Lane/列。`a.f(b)` 即 `f(a, b)`。Intrinsic：`abs sign floor ceil round（四舍六入五成双） trunc fract sqrt inverse_sqrt exp exp2 log log2 sin cos tan asin acos atan sinh cosh tanh radians degrees atan2 pow step min max clamp mix smoothstep length distance dot normalize cross transpose`，Fragment 专用的 `dpdx dpdy fwidth sample(texture, sampler, uv)` 与 `discard()` 语句，任意阶段的 `sample_level(texture, sampler, uv, lod)`，以及 Render Profile 的 `to_vec4 quad_vertex to_clip rounded_rect_sdf`（§100）。`let` 绑定一个名字，`let mut` 可赋值（含 `+=` 等复合赋值，目标可到字段、单个 Lane 或字面量下标）；`if`/`match`/Block 作为值时降为局部变量加分支赋值；`match` 只作用于 `I32`、`U32`、`Bool`，Arm 为字面量、`|` 组合、`_` 或名字，无 Guard，必须穷尽（`E2301`）；`for` 只遍历整数区间 `a..b`/`a..=b`，两端为字面量时自身即上限，否则必须 `@max_iterations(N)`（运行时计数，超过 N 即停止），否则与 `while`/`loop` 一样报 `E8103`；`@max_iterations` 标在其他 Statement 上报 `E8105`。Shader `fn` 是纯函数，只读参数，不读成员、不用阶段专用 Intrinsic（`E8106`）；`fn` 之间不得直接或间接递归（`E8105`）。Closure、`?`、`??`、命名实参、Turbofish、`emit`、`transaction`、`await` 与泛型 Shader 报 `E8105`。

## 101. Instance ABI

当前实现（Layout Version 1）：Instance 数据按声明顺序紧密排列，每个 Leaf 4 字节对齐（向量占 `4 × Lane` 字节，矩阵逐列，`ColorLinear` 16 字节），Stride 为总和；Uniform Block 按 WGSL Uniform 地址空间规则：标量对齐 4，二元向量对齐 8，三/四元向量与 `ColorLinear` 对齐 16（三元向量仍占 12 字节，其后标量可放进末 4 字节），矩阵按列对齐并以列对齐为 `matrix_stride`，Block 大小向上取整到 16。`@shader_value` Record 成员按字段顺序展开为 Leaf，按所在 Block 的规则放置，Descriptor 以点分路径命名（`light.position`）。Varying 按声明顺序取 Location，`F32` 透视插值，整数 Flat。`stable_field_id` 是 Shader 名、Block 与点分名的 FNV-1a；`ShaderId` 是 Shader 名的 FNV-1a；Layout Version 与全部 Descriptor 进入 Pipeline Cache Key 的 Layout 部分。Host 端用 Encoder 按 Descriptor 写入（值的形状或 Lane 类型不符即报错），Rust `#[derive(GpuPod)]` 结构的 `InstanceLayout` 与 Instance Descriptor 逐字段比对名字、格式、Offset 与 Stride，不符报 `E8104`。Backend 读取同一 Layout：MSL 以显式填充的 Packed 结构声明并由编译器断言大小；WGSL 的 Uniform 结构以 `@size` 固定每个 Offset，Instance 为 4 字节标量 Leaf 的只读 Storage Buffer；HLSL 的 `cbuffer` 每个 Leaf 带 `packoffset`，Instance 为标量 Leaf 的 `StructuredBuffer`。CI 用 naga 反射 WGSL 结构与 Descriptor 逐 Offset 比对。

## 102. Shader Lowering

当前实现：Shader HIR 检查后直接降为结构化带类型的 Program IR（局部变量槽、`if`、有界 `for`、`break`/`continue`/`return`/`discard`；`if`/`match` 值已降为局部变量赋值，无 SSA），经 Validation 后由同一遍历生成 MSL、WGSL 与 HLSL（Shader Model 5.1）。所有名字由编译器生成（`l3`、`fn1`、`u0`），源码名不会与目标语言关键字冲突。各后端语义对齐：`round` 四舍六入五成双，浮点 `%` 按截断取余，移位量取低 5 位，`v[i]`/`m[i]` 的下标钳到最后一个 Lane/列，Fragment 读到的 Instance 成员作为 Flat Varying 传入；HLSL 以转置形式保存矩阵（行即列），`a * b` 生成 `mul(b, a)`。Metal 以关闭 Fast Math 编译。CPU Reference Interpreter 按同一语义执行 Program IR（整数运算回绕，整数除以零得被除数、取余得零），Fragment 以 2×2 Quad 为单位执行，`dpdx`/`dpdy`/`fwidth` 取 Quad 内的精细差分，三角形外的 Lane 作为 Helper 只参与差分；配套的小光栅器按每个 Instance 六个顶点画两个三角形，像素中心采样、左上填充规则、`F32` Varying 透视校正插值、整数 Varying 取首顶点，混合后按 8 位通道量化。CI 用它与 Metal 渲染同一组编码后的 Buffer，逐像素比对，每通道差不超过 2/255。Cross-backend Validation：同一组编码后的 Buffer 还由 Vulkan 渲染（`vulkan` Feature，macOS 上经 MoltenVK；无 Vulkan 设备时跳过）——Program 的 WGSL 经 naga 写成 SPIR-V（与内置 Shader 相同，在 Vertex Stage 翻转 Clip-space y），由 `VulkanBackend::install_program` 按 Program ABI 安装：Set 0 的 Binding 0 是 Uniform Block、Binding 1 是只读 Storage Buffer 形式的 Instance，Set 1 依次是 Texture 与 Sampler；每次绘制的 Descriptor Set 由 Bind Group 与 Instance Buffer 写入，取自帧槽重开时整体复位的 Descriptor Pool，Instance Offset 须是设备 `minStorageBufferOffsetAlignment` 的倍数，Uniform 只经 `Binding::Uniform` Buffer 读取。Vulkan 图像与参考、与 Metal 分别逐像素比对，容差相同；可用时开启 Khronos Validation Layer 并要求无错误。Headless Raster 没有 Shader 编译器，它运行 Program 的 `CpuShader`（`ProgramCode::Cpu`）：按 Program 的 Layout 从绘制的 Uniform 与 Instance 字节解码，取 Bind Group 中按槽位排列的 Texture 与 Sampler，在 Scissor 内由同一光栅器画进 Framebuffer（扩展范围目标保留浮点值，不按 8 位量化），因此 Headless Golden 与上述各后端读同一组字节；同一组比对在所有平台上也对 Headless 图像执行。

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
- 结构性语法错误使用 `E1401`–`E1407`（附录 C），不使用临时前缀码；
- Parser 有两项预算：表达式、类型、Pattern、Block 与 View Body 的嵌套不超过 256 层，超出处的最内层构造整体（一个 Token 或一个定界组）保留为 `ErrorNode` 并报一次 `E1406`；一个源文本至多 2^20 个有效 Token，其后的全部 Token 保留为一个 `ErrorNode` 并报 `E1407`。因此 Parser 与之后遍历语法树的各遍栈深有界，CST 仍逐字节无损；预算附近的编辑总是全量重解析；
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

- 只含 `dp` 与 `%` 分量的常量直接 Lower 为 Layout 的 Fixed 或 Relative 值，不进入 Binding 侧表；
- 其余长度族值与 `MixedLength` Lower 为定长 `LengthTerms { dp, px, sp, em, pct: F32 }`，常量在编译期折叠，不产生表达式树或堆分配；
- `LengthTerms` 与其依赖掩码存放在 Binding 侧表，不进入 Layout 热存储；环境分量在依赖变化时预折叠：`fixed = dp + px / scale_factor + text_scale_curve(sp) + em × resolved_font_size`；
- Layout 热存储只保存折叠后的 `Length`（`viso_schema::Length`）：含 `%` 的值是 `Length::Relative { fixed: F32, pct: F32 }`（payload 8 字节），Layout 求解是一次 `fixed + pct × percent_basis`（结果不为负）；纯 `Dp` 常量是 `Length::Fixed`，走 Fixed 路径；
- `font_size` 与 `line_height` 的 Percent 基准是字号，在 Typography 解析时折叠进 `fixed`，Layout 看到的 `pct` 为 `0`；
- 环境值变化时只重新折叠依赖掩码命中的 Binding（§19.6），不重新求值整棵树；折叠结果未变的 Node 不标脏；
- Percent Basis 是否确定由父布局沿 Layout 递归传入；重新布局的子树根自其祖先链求得；
- `E3105`/`E3106` 每个 Node 每类只报告一次，经 `NodeStore::take_length_warnings` 取出，Release 中为空；`NodeStore::length_stats` 的 `nonfinite`（即 `length_nonfinite`）与 `indefinite_basis` 计数器在所有构建中计数；
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

目标为内置 Widget 双向 Property 的 `bind checked <=> on;` 同样 Lower 为读取 `on` 的 Property 条目加写回 Handler：控件的内建响应读取该条目得到当前值，投递 Property 配对的 Event（主 Property 为 `changed`，其余为 `<name>_changed`）；写回 Handler 取 Payload 首个字段写入 `on` 的 Lens，在作者声明的同名 Handler 之前运行。带 `using C` 时条目读取 `C::to_view(on)`，写回 Handler 写入 `C::to_model(value)`，其结果为 `None` 时不写入（§51）。

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

Fix 生成规则（当前实现）：

- `child`（`E3001`）删除该词，事件箭头（`E3201`）把 `=> stmt` 包成 `{ stmt }`，均为 `machine-applicable`；`Float`（`E2101`）给出 `F64`、`F32` 两个 `maybe-incorrect` 替换，`F64` 在前；
- 未知名字、方法、字段、Variant（`E2001`/`E3101` 等）的候选按 Optimal String Alignment 距离（插入、删除、替换、相邻交换各计 1，`cuont → count` 为 1）排序，只给阈值内最近者；
- 非法隐式数值转换 `E2102` 带一个 `maybe-incorrect` Fix：在源操作数后追加 `as T`（非原子表达式加括号）；二元运算两侧数值类型不同，转换整数一侧到浮点类型，否则转换右侧到左侧类型，结果按未知类型继续检查，不产生级联诊断。

---

## 139. Schema 查询

```bash
viso schema viso::widgets::Button --json
```

至少返回：

```json
{
  "schema_version": "1.0",
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
  "capabilities": [],
  "origin": "framework"
}
```

对象字段：

- `schema_version`：对象格式版本（当前 `1.0`），删除字段或改变含义时升级；
- `origin`：框架符号为 `"framework"`，项目组件为其声明位置、SymbolId、`visibility` 与 schema `revision`（CLI §18.3）；
- `kind`：`component`、`record`、`enum`、`native_library`、`native_function` 或 `native_type`；查询 `Symbol.member` 时另有 `member`，`inputs`/`events`/`functions`/`fields`/`variants` 只保留该成员；
- `inputs`：Component 的 Property（分组 Property 写作 `semantics.label`，`invalidates` 为该 Binding 实际失效的 Dirty Class 规范名，`two_way` 标记可 `bind`），或 Native 函数的参数；`default` 未记录时为 `null`；
- `events`：`bubbles` 为 Capture → Target → Bubble 路由的输入事件为真；Component 自身事件不冒泡；
- `slots`：每项 `{name, type, default}`，`type` 为 `Slot<Node>`、`OptionalSlot<Node>` 或 `SlotList<Node>`，`default` 标记 `@default`（Native Widget 为其 Default Slot）；
- `record` 另有 `fields`（每项 `name`、`type`、`required`、`default`，`default` 为源码形式，未声明为 `null`）；`enum` 另有 `variants`（每项 `name`、`payload`：Tuple 载荷 `(I64, String)`、Record 载荷 `{ code: I64 }`，Unit 为 `null`）；
- `capabilities`：函数自身所需，或 Library/Type 全部函数所需的并集；
- `native_library`、`native_function`、`native_type` 另有 `functions`（每项：`name`、`symbol`、`kind`、`method`、`params`、`returns`、`capabilities`、`thread`、`deterministic`、`realtime_safe`、`cost`）；`native_library` 另有 `types`；`native_type` 另有 `ownership` 与 `thread`。

未知符号为 `E2001`，附最近的符号或成员名。

AI 在使用未知 Component/Property/Event 前必须查询 Schema 或依赖已锁定版本的本地索引。

---

## 140. Formatter 唯一输出

Formatter 必须规范：

- 4 空格缩进；
- 条目以换行结束，删除行尾与 `}` 前多余的 `;`，作者写在同一行、以 `;` 分隔的条目保持一行（§14）；
- 删除 `match` Arm 行尾的 `,`；
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

当前实现（`crates/lsp/src/format`）只改空白，输出只取决于 Token、所在节点与作者的换行，因而幂等：

- 作者写在一行的 `{ }`，整行不超过 100 列时保持一行（`Text { text: label; }`），否则展开：`{` 后换行、Body 缩进一级、`}` 独占一行；空 Block 为 `{}`；Import 的 `{A, B}` 紧贴内容；
- 展开 Block 中 `;` 结束一行，Block 自身层级（不在 `(` `[` `<` 内）的 `,` 也结束一行；
- `}` 后的 `,` `;` `)` `]` `.` `?` 与 `else` 留在同一行；
- 语句内作者的换行保留（多个折为一个），续行按未闭合的 `(` `[` `<` 层数再缩进；Attribute 后的换行开始新的一项；
- 声明与语句之间的空行保留，连续空行折为一行，`{` 之后与 `}` 之前不留空行；
- 与前面代码同一行的注释留在该行，行注释结束一行；
- `(`/`[` 紧贴前面的名字或闭括号（调用、下标），前缀 `-`/`!`、区间 `..`/`..=`、Closure 的 `|` 与泛型 `<>` 紧贴操作数。

`crates/lsp/tests/format_fuzz.rs` 对随机与变异源码检查 Token 不变与幂等。

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

当前实现：`viso_dsl::edit::Document` 对一个文件提供上述 11 种编辑（`StructuredEdit`），代码字段（类型、表达式、语句、节点）是源码文本：

- Symbol ID 的文本形式是 32 位小写十六进制；`Document::symbol(&["Counter", "count"])` 按声明路径取得，`symbol_at(offset)` 取包含该位置的最内层声明；
- Syntax ID 指向 View 结构节点、Property Binding 与 Event Handler：所属声明的 Symbol ID 加从声明向下的路径，每步是节点种类与它在父节点同种类子项中的序号，文本形式 `<symbol>/ViewDecl.0/AnonymousNode.1/PropertyBinding.0`。它在所属声明之外的任何编辑、以及声明内不在其路径前方增加同种类兄弟的编辑后仍然有效；`syntax_id_at(offset)` 取包含该位置的最内层节点；
- 插入位置按规范成员顺序：`AddInput` 在最后一个 `input`/`slot`/`event` 之后，`AddState` 在最后一个 `state` 之后，`AddAction` 在 `view` 之前；`InsertNode` 是父节点的第 `index` 个结构子项（越界为最后一个）；`SetPropertyBinding` 只替换已有绑定的值，否则在最后一个绑定之后新增；`AttachEventHandler` 在绑定与 Handler 之后；`WrapInKeyedFor` 以 `for item in list key k { .. }` 包住节点；`ConvertTaskToResource` 要求组件成员 Task 返回 `Result<T, E>`，在 Task 之后声明 `resource name: Resource<T, E> { load = task(args); key = k; }`，并删除该组件中不带 Slot 与 Handler 的 `start task(..);`；`AddImport` 在最后一个 Import 之后（已存在时拒绝）；`CreateComponent`、`AddTraitImpl` 追加到文件末尾。插入文本按四空格缩进放在兄弟成员所在的缩进上，空的 `{}` 展开为多行；
- `apply` 返回不重叠、按源码顺序排列的 Text Edit 与编辑后的源码，以及新建的声明或节点的 ID 以便链式编辑。编辑后重新编译：出现源码原本没有的错误（按错误码与消息计数）时拒绝并返回这些诊断；不存在的 Symbol/Syntax ID 与种类不符的目标在改动文本前拒绝；
- LSP 声明 `codeActionProvider`：`textDocument/codeAction` 返回与范围相交的诊断所带的同文件 Fix（`quickfix`，`machine-applicable` 的标为首选）与由光标处声明完全确定的结构化编辑（`refactor.rewrite`，目前是把返回 `Result` 的 Task 按其参数转为 Resource，名称去掉 `fetch_`/`load_`/`get_` 前缀），均已检查。`viso/syntaxId` 返回某位置的 Symbol ID 与 Syntax ID；`viso/structuredEdit` 接受 `{textDocument, edit: {kind, ..字段}}`，返回 `{edit: WorkspaceEdit, created}`，被拒绝时返回 `-32602` 错误与原因。

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

当前实现：`crates/dsl/tests/doc_examples.rs` 按上述规则检查两份文档的全部代码块，`ComponentMember*`、`NodeMember*` 与 Block Body 是 Parser 的片段入口（`Entry::ComponentMembers`/`NodeMembers`/`BlockBody`，不是源码形式）；`crates/dsl/tests/parser_acceptance.rs` 覆盖 §152 的其余条目，其中随机编辑序列逐步比较增量重解析与全量 Parse 的 Token、树与诊断；`crates/lsp/tests/format_fuzz.rs` 用固定种子的随机与变异源码检查 Formatter 不 Panic、保留有效 Token，并对每个可作为 `CompilationUnit` 解析的文档示例检查格式化后树形不变且幂等。Formatter 在两个 Token 紧贴会被词法化为其他 Token 时（如 `:` `:`）插入空格，未闭合字符串之后必换行，最后一个 Token 未闭合时不修剪也不追加换行。

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

当前实现：`crates/dsl/tests/type_acceptance.rs` 从源码经完整前端检查拓宽阶梯与禁止转换（`E2102`/`E2103`/`E2106`）、`Float`（`E2101`）、StableKey（`E2701`）、Closure 期望类型、Capability 传播（`E2601`）、Computed 环路径（`E2105`）、`MixedLength`（`E2106`/`E2107`）与 Percent 流（`E3104`）；`crates/dsl/tests/behavior_vm.rs` 检查借用 Native Handle 不能存入 `state`、`input`、`computed`、Event 参数、Record 字段、Enum Variant Payload、`const` 与返回值（`E6102`）；`crates/dsl/tests/length_basis.rs` 在布局后检查 `font_size` 与其他 Property 的 `em`/`%` 基准。

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

当前实现：`crates/dsl/tests/runtime_acceptance.rs` 从源码经挂载后的视图检查 `capture` Handler 先于 Target、Bubble 在后（Hot Reload 与 Release Package）、Paint-only 写入不产生 Measure/Layout、Keyed Reorder 保留焦点与行内 Task 且 Key 变化取消被移除行的 Task、旧 Key 的加载晚于新 Key 完成时被丢弃、Handler 中 Native Panic 与指令 Budget 超限回滚写入且视图继续运行、环境变化只弄脏读取该字段的子树（同类 Resize 不 Patch、跨 Breakpoint、Keyboard/SafeArea、TextScale 的 `sp` 长度重新 Measure/Layout 并以新盒发布语义）、Preserve 分支保留原节点、Preserve Budget 按离开先后逐出、内存警告逐出全部保留分支；其余条目由 `view_handlers.rs`、`view_regions.rs`、`view_tasks.rs`、`view_effects.rs`、`hot_reload.rs`、`state_retype.rs`、`view_capability.rs`、`behavior_vm.rs` 与 `adaptive_runtime.rs` 覆盖。语义树的盒随 LAYOUT 重新发布（门面的无障碍桥在 SEMANTICS、LAYOUT、TRANSFORM 任一弄脏时重新派生）。

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

当前实现：十一个任务各有一份按本文档写成的参考解 `crates/dsl/tests/usability/NN-*.vs`。`crates/dsl/tests/usability_samples.rs` 检查全部零诊断，并验证行为：Counter 点击计数、Todo 用 `push` 添加并按 Key 渲染、`SizeClass` 三档布局、`AdaptiveScope` 侧栏不随 Window 宽度变化、Quick Game 与拆出的 FixedUpdate System 对同一输入 Tape 状态一致、`count * scale` 的 `E2102` 应用 Fix 后零诊断、Hot Reload 后输入焦点保留；`crates/lsp/tests/usability_format.rs` 检查每份格式化后不变。人工计时与混淆率记录需真人参与，不在自动化范围内。

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

当前实现：`crates/dsl/tests/ai_repair.rs` 实现修复闭环本身——编译、取第一个 Error 的第一个 Fix 应用、再编译，直到无 Error（至多 8 轮），不接受任何其他输入；冻结集为生成代码最常见的错误：`child`、事件箭头、`Float`、拼错的 Property/Event/方法/Action/Variant、`text` 绑数字、混合数值类型、对非 Option 用 `?.`。每例断言修复所经的诊断码序列，修复后以 Event Trace（点击后的 State 与 Text）或 Game Tape（摇杆输入后的 System State）验证行为，并统计 First-pass Parse/Type-check Rate、平均修复轮次与 Diagnostic-guided Repair Rate（必须为 100%）。真实模型的 Prompt 集与生成运行属于 CLI/外部评测，不在本测试内。

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

当前实现：`crates/dsl/tests/definition_of_done.rs` 逐条检查。`crates/dsl/tests/ebnf_golden.rs` 从附录 A 读出全部 Production，要求每个恰有一行正例与反例（反例的诊断码固定，含 Lexer 码），增删 Production 而不改表即失败；与 Component 共用成员集的 Body（Template、System、`use` 体）与 Style 选择器的形状在解析之后以针对性诊断限制（`E3601`、`E2103`）。关键字清单逐词对照 §12.1–§12.3，Lexer/Parser 的关键字种类数等于两表之和；优先级以 Golden Tree 锁定每对相邻级别与每级结合性，并对照 §64 表格原文。运行项在 Headless 宿主中挂载同一个 Component：Action 写入经 Computed 到达 Text 无需 Render 调用，Keyed 重排移动同一批节点，被拒绝的热重载保留上一版本与 State，删除的行其 NodeId 不再解析；System 在 8 Hz 下按经过时间运行整数 Tick。Shader 一项由 `crates/viso/tests/shader_golden.rs` 覆盖：CPU 参考光栅器与 Metal（`vulkan` Feature 下另有 MoltenVK 上的 Vulkan）在容差内一致。
