# Viso Development Runtime & Transactional Hot Reload

> 文档状态：Viso 1.0 Draft / Development Runtime Specification  
> 目标读者：Runtime、DSL/Compiler、CLI、Studio、Shader/GPU、Game、Asset、Platform 工程师与 AI Coding Agent  
> 适用范围：只定义 **开发期** Dev Runtime、Hot Reload、Rust Warm Restart、DevSnapshot 与 Host ↔ Running App 协议。Release / Shipping artifact 不包含本子系统。

## 与 ADR / Architecture 的关系

本文档是 `Viso_Architecture.md` 第 42 节（Hot Reload 合同锚点）的详细实现规范，位于 Architecture-document 权威层级。它细化并整合以下已接受的 ADR，决策层面以 ADR 为准：

- [ADR-0015](./docs/adr/0015-transactional-hot-reload.md) — 事务式热重载（compile → diff → migrate → commit）。
- [ADR-0016](./docs/adr/0016-release-aot-package.md) — Release AOT package（本子系统不进入 shipping artifact 的边界依据）。

本文档若引入新的架构决策（例如改变热重载失败回滚语义、迁移模型或 Host↔App 协议边界），须回写为新的/更新的 ADR，而不是在本 draft 中静默覆盖。

---

## 0. 核心结论

Viso Hot Reload 的设计不是“把整个脚本重新跑一遍”，也不是“在 release App 里留一个远程代码注入入口”。

Viso 采用：

> **Typed Transactional Domain Hot Reload**

不同源码使用不同的最窄更新路径：

```text
Source Change
    │
    ▼
Viso Dev Session
    │ classify
    ├── .vs UI / behavior ──> Typed Semantic Patch
    ├── Game System       ──> Tick-boundary System Patch
    ├── Shader            ──> Validated Pipeline Patch
    ├── Asset / Font      ──> Resource Revision Patch
    └── Rust              ──> Incremental Build + Stateful Warm Restart
```

所有可原地应用的更新都必须：

```text
compile candidate
    ↓
validate
    ↓
stage
    ↓
compatibility analysis
    ↓
wait domain atomic boundary
    ↓
commit
    ↓
precise invalidation
```

失败永远：

```text
NACK + diagnostics + keep last-good running app
```

---

# Part I — Dev-only Hard Contract

## 1. Hot Reload 只属于 Dev artifact

这是 **build-time contract**，不是 release 中的 runtime flag。

### Dev artifact

允许编入：

```text
Dev Runtime
Dev transport client/server endpoint
PatchBundle decoder
Symbol/source reverse metadata
state-preservation metadata
DevSnapshot capture/restore endpoint
Inspector control endpoint
hot-reload diagnostics
```

### Release / Shipping artifact

必须不编入：

```text
Dev transport listener
PatchBundle decoder/apply engine
DevSnapshot endpoint
hot-reload command handlers
hot-reload-only source metadata
hot-reload-only schema reverse maps
remote development control surface
```

Release 中仍然可以保留真正产品需要的：

```text
normal runtime IDs
normal state schemas required by the app
crash diagnostics chosen by build policy
production telemetry chosen by app policy
```

但它们不能因为开发期 Hot Reload 而增加 steady-state frame cost。

### 1.1 禁止 runtime re-enable

Release binary 不允许：

```text
hot_reload = true
VIS0_ENABLE_HOT_RELOAD=1
hidden network flag
secret debug URL
```

把开发能力重新开启。

要获得 Hot Reload，必须重新构建 Dev artifact。

### 1.2 零 release 热路径税

Release frame loop 不得出现：

```rust
if hot_reload_enabled { ... }
```

作为每帧固定路径。dev 层整体由 `#[cfg(feature = "hot-reload")]` 编入或去除，release 帧循环里没有它的分支。

测量（`cargo bench -p viso --bench frame_loop`，挂载 `view!` 的 app，每帧切换一个 `if` 分支，release、Apple Silicon）：不开 feature 每帧 3.83 µs，开 feature（session 空闲）每帧 3.78 µs，差在噪声内（H1.2 时的测量，当时 session 还在 app 内启动 watcher；app 不再监听文件之后 dev 层只剩挂载记录与 dev channel，未重测）。

Release 不保留：

```text
per-frame dev socket poll
hot-reload SymbolId HashMap lookup
patch revision branch
source file watcher
state-preservation bookkeeping that is only needed by Dev Runtime
```

### 1.3 `view!` 挂载记录

`hot-reload` feature 打开时，每个 `view!` 挂载在树建好后向 UI 线程的挂载队列登记一条记录：

- `.vs` 文件路径与编译时源码的 `source_hash`（§35），不含源码文本；
- 编译所用的模块身份（package、module path、language 版本），以及 build 所用的 catalog 目录与 source locale、package 被授予的 capability、编译器 schema fingerprint（§34）；
- 静态模板的形状（按先序每个静态节点的子节点数），无 region 的 view 据此在首个 patch 时按静态序号找回每个静态节点；
- 挂载根节点；
- 每个状态 cell 及其由 `SymbolId` 得出的持久 key；
- behavior host（有 behavior 时）；
- 含 region 的 view 还记录每个静态节点（按静态序号），因为 region 内容与静态节点交错，单凭树无法还原。

Dev session 在帧边界取走这些记录。

feature 关闭时登记宏展开为空：二进制里既没有记录代码，也没有 dev 元数据。feature 打开时 app 也不含 `.vs` 编译器：dev artifact 只链接 typed patch 的解码、校验与提交（§9）。

### 1.4 Dev session

app driver 在每个窗口同步构建完成后立即领取该窗口的挂载记录；之后才登记的挂载（例如列表行）不是窗口构建的一部分，不被跟踪。窗口关闭或挂载根已被释放时，对应挂载被丢弃。app 端的 session 是 `viso run` 的对端（§3）：它不监听任何文件、不读项目源码。

- **连接与 inventory**：首个挂载时按 `viso run` 给的环境打开 dev channel（§46）；没有 `viso run`（环境里没有 dev channel）时不建 session，挂载记录直接丢弃。每个首次挂载的文件以一条 `MountEntry`（file id、canonical path、模块身份、catalog、capability、所运行源码的 `source_hash`）报给 host（§35）；同一文件再次挂载不重复报告。
- **帧边界**：唤醒只把 patch 检查后暂存（§36）并为有挂载的窗口请求一帧；提交发生在下一帧 `FlushStateTransactions` 开头、窗口 flush 之前，因此一次 reload 的写入在同一帧结算并绘制。
- **整 patch 原子**：一个 patch 的所有 view 先全部 stage——解码并校验 candidate 的 behavior 模块、确认其挂载的 component 存在（失败 NACK `NACK_UNLOADABLE_VIEW`，stage `runtime-stage`），任何一个失败则什么都不提交；全部成功后才在帧边界依次按 host 给的 reload plan 提交到每个文件的每个挂载（同一文件挂载多次时，提交前先把持久 key 指回当前挂载的 cell），revision 前进并 ACK。文件被 patch 之后才出现的挂载（例如新开的窗口）由 build 的 view 建成，领取时立即以 fresh plan 重建为该文件当前的 candidate。
- **失败显示**：host 拒绝的编辑以 `Failure` 送达，app 在挂载该文件的窗口 last-good UI 之上显示，直到 host 清除（编辑被撤回）或该文件的 patch 提交（§37.1）。

---

## 2. `viso run` 是唯一普通入口

Desktop：

```bash
viso run
```

Mobile simulator/emulator：

```bash
viso run ios
viso run android
viso run ios --device iphone-local
viso run android --device pixel-local
```

Web：

```bash
viso run web-gpu
viso run web-dom
viso run web-hybrid
```

`viso run` 默认：

```text
Dev profile
+ Dev Runtime
+ watcher
+ incremental compiler/build coordinator
+ persistent development transport
+ Last-good runtime policy
```

开发者不需要另开 `viso watch`。

### 2.1 `--no-hot-reload`

```bash
viso run --no-hot-reload
```

只用于排查开发问题。

它的含义是：

```text
Dev artifact 仍然是 Dev artifact
但当前 session 不自动 apply patch
```

它不改变 Release/Shipping contract。

当前实现：app 仍以 `hot-reload` feature 构建并连上 session、报告挂载；host 照常 watch、编译并报告每个 candidate 的诊断，但不向 app 发送 patch 或 failure overlay；编译通过的 candidate 的 `dev` event 为 `outcome: held`。

---

# Part II — Overall Architecture

## 3. Host owns source watching and compilation

Simulator、Emulator、Desktop App、Browser runtime **不监听项目源码目录**。

文件监听发生在开发机：

```text
Editor / AI Agent
      │ save
      ▼
Host File System
      │
      ▼
Viso Dev Session
      │
      ├── watcher
      ├── incremental DSL compiler
      ├── shader compiler coordinator
      ├── asset pipeline
      ├── Rust build coordinator
      └── device/browser connection manager
```

Running App 只接收已经编译/验证过的开发 artifact 或 patch。

这保证：

- iOS Simulator 不需要读 host project tree；
- Android Emulator 不需要共享源码目录；
- Browser 不需要下载 `.vs` source；
- source path 不成为 runtime security boundary；
- compiler 版本只有 Host 一份 source of truth。

---

## 4. Dev Session Service

实现上可以有常驻 daemon，也可以由当前 `viso run` process 持有同等生命周期的 service；public CLI 不要求用户记 `viso daemon` 命令。

概念组件：

```text
DevSessionService
├── ProjectWatcher
├── ChangeCoalescer
├── IncrementalDslCompiler
├── ShaderBuildService
├── AssetBuildService
├── RustBuildService
├── PatchPlanner
├── SnapshotService
├── RuntimeConnectionManager
├── DiagnosticRouter
├── InspectorBridge
└── ProfilerBridge
```

Architecture 关心的是 service contract，而不是一定存在独立 OS daemon process。

已实现（desktop host，`tools/cli/src/dev/`）：service 由 `viso run` 进程持有，`DevSession` 在命令的整个生命周期拥有 session 身份、session lock（`LockKind::DevSession`，同一 project 与 target 同时只有一个 dev session）、ProjectWatcher（§6、§7，`dev/watch/`）、ChangeCoalescer（§7.1）、source graph 与 DSL 编译器（`dev/sources.rs`）以及 RuntimeConnectionManager（`dev/link.rs`）。顺序：取 lock → 计算 build id → 启动 watcher → `cargo build` → 启动 app → 接受连接、编译、发 patch，直到 app 退出。

- watcher 在 build 之前启动，所以 session 知道 build 读到的每个文件；runtime 报来的 `source_hash` 与 host 上该文件当前内容不同，就说明 build 与启动之间有编辑，它直接成为第一个 patch，不需要 runtime 发源码；
- `view!` 单独编译每个文件（只依赖该文件与其 package 的 catalog 和 capability），所以一个编辑只重新检查它碰到的文件，catalog 改动重新检查该 package 所有已挂载文件。跨文件 `.vs` import 在 build 本身支持之前不存在，因此也没有跨文件的受影响集合要算；
- 每个文件保留最新文本的 `IncrementalParse`，编辑以公共前后缀算出单个 `Edit`，只重解析被碰到的声明。release 测量（36 KB、200 个 component 的文件，单属性编辑，`dev::sources::tests::incremental_parse_cost`，Apple M4）：完整解析 1.71 ms，增量 0.048 ms（50/50 次就地重解析），但同一文件的完整 host 编译（resolve、lower、typecheck、plan）是 23 ms——编辑之后的 resolve/typecheck 仍是整文件的，语义层增量是后续工作；
- host 用 build 的同一 profile 编译（mount entry 带来的 capability + 该 package catalog 的最近一次无错编译），调用与 app 相同的 view planner，所以 host 接受的就是 app 能 plan 的；
- 同一时刻只有一个 patch 在途：等待 ACK/NACK 时保存的编辑合并为下一个 batch。

### 4.1 Session identity

每次 `viso run` 创建：

```text
DevSessionId
BuildId
ProjectFingerprint
ProtocolVersion
```

每个 Running App connection 还拥有：

```text
RuntimeSessionId
TargetProfile
RuntimeRevision
SchemaFingerprint
CapabilitySet
```

禁止仅靠端口号猜测“这是哪个运行中的 App”。

已实现：`SchemaFingerprint` 是编译器 schema 的 128-bit 指纹（`viso_dsl::hotreload::schema_fingerprint`：编译器版本、candidate plan 格式号、每个标准 native library 的签名内容），`view!` 展开时写进 mount record，runtime 在 hello 里报告；与 `viso run` 自己的不同则 `Reject::Schema`——host 不给用另一个编译器构建的 runtime 编译 patch。runtime 的 `TargetProfile`（capability、catalog）随每条 `MountEntry` 报告。

---

## 5. Connection topology

### Desktop host

优先：

```text
Unix domain socket / named pipe / loopback
```

具体 transport 可以按平台实现，但不能影响 Patch protocol semantic。

### Android Emulator

可以通过：

```text
adb forward/reverse
or host-reachable local transport
```

Viso CLI 负责 emulator ID → adb serial → connection mapping。

### iOS Simulator

通过 host 可达的 simulator development transport 建立连接。

不需要 signing/provisioning/physical-device tunnel。

### Web

通过 Viso dev server 建立：

```text
WebSocket / equivalent dev channel
```

Web Dev Runtime 同样只存在于 dev build。

---

# Part III — Change Detection

## 6. Watch scope

默认监听：

```text
*.vs
embedded/external shader source
assets/**
font assets / external dev font manifests
Viso.toml fields relevant to development
Rust source in active workspace dependency closure
build scripts/config that affect active artifact
```

不默认监听：

```text
target/
dist/
.generated cache/
.git/
editor temp directories
unrelated workspace members outside active dependency graph
```

已实现（`dev/watch/scope.rs`）：监听 `*.vs`、`i18n/` 目录下的 `*.toml` catalog 与 root `Viso.toml`；shader、asset、font 与 Rust 源码随 patch 它们的 domain 加入。排除 root 下的 `target/`、`dist/`，所有隐藏目录（`.git/`、`.viso/`、编辑器状态），含 `CACHEDIR.TAG` 的目录（移走的 Cargo target dir、生成的 cache），`node_modules/`，以及编辑器临时文件（隐藏、`#…#`、`…~`）。`Viso.toml` 的改动只给出 warning：运行中的 app 保留它 build 时的配置，需重启 `viso run`。

---

## 7. File-system events are not semantic changes

Editor 可能产生：

```text
write temp
rename
chmod
write final
remove temp
```

因此必须：

```text
raw fs events
    ↓
short debounce/coalesce
    ↓
canonical path resolution
    ↓
content hash / metadata check
    ↓
semantic change set
```

不要一个 filesystem event 启一次编译。

### 7.1 Debounce

初始实现可使用很短窗口，例如几十毫秒级，但最终值由 benchmark/AI-edit workload 测试决定。

目标：

- 人类单次 save 快；
- AI 连续修改多个相关文件时合并；
- 不为了 debounce 引入明显视觉延迟。

已实现（`dev/watch/`、`dev/mod.rs`）：project watcher 是 host 上的一条线程，启动时递归扫描 scope，监听每个 scope 目录（macOS/FreeBSD 为 kqueue，Linux 为 inotify，Windows 为 `ReadDirectoryChangesW`）；目录事件触发对该目录的重新扫描，之后新建的文件与目录加入 scope（inotify 只为新目录报告 `IN_CREATE`，新文件在 `IN_CLOSE_WRITE` 时才读）。文件的事件停止满后端的静默窗口后读取：inotify 只订阅表示写入完成的事件，窗口为 0；kqueue 与 Windows 每次写入都报告，窗口为 5 ms，使 truncate/write 连写合并为一次变更。无后端或无法监听的目录退回轮询：文件每 25 ms 一次 `stat`，(长度, mtime) 稳定 25 ms 后读取，目录每 250 ms 重新扫描（Windows 一次 wait 至多覆盖 63 个目录，其余目录轮询）。路径是 canonical 的（project root 先 canonicalize）。

coalescer：一个 batch 在最后一次变更之后静默 5 ms 时取走，持续变更时最多等首次变更后 50 ms。窗口由测量决定（`dev::watch::tests::multi_file_save_spread`，release，Apple M4/kqueue，每种 40 轮）：工具背靠背写 N 个相关文件（AI 应用多文件编辑）时，各文件到达 session 的时间差（首到末）为

```text
 1 file   spread 0      first arrival median 6.0 ms
 2 files  spread median 0.04 ms  max 0.08 ms
 4 files  spread median 0.16 ms  max 0.28 ms
 8 files  spread median 0.37 ms  max 0.52 ms
16 files  spread median 0.60 ms  max 0.95 ms
```

5 ms 是 16 个文件最大时间差的五倍以上，单次 save 只多等 5 ms（首个文件到达本身约 6 ms，几乎全是 kqueue 的静默窗口）。由多次工具调用分开写的编辑（彼此间隔以秒计）不在任何不可感知的窗口内，各自成为一个 candidate。inotify 与 Windows 的数值未在此测量。

### 7.2 Content hash

只因 mtime 改变但 bytes 未变：

```text
no semantic compile
```

已实现：watcher 线程内按内容 hash 去重；host 与 runtime 都用稳定的 64-bit FNV-1a `source_hash`（与工具链无关），host 据此判断 runtime 运行的是文件的哪个版本；一个内容已经成为过 candidate（已发送或被拒）就不再重复编译或发送，撤回到 runtime 运行的版本只清除 failure。

---

# Part IV — Patch Result Classes

## 8. Three outcomes

每次 candidate 编译后必须归类为：

```text
PATCH
PATCH_WITH_SCOPED_RESET
WARM_RESTART_REQUIRED
```

### PATCH

完全兼容。

例：

```text
Text.text value changed
color changed
layout property changed
compatible action body changed
compatible game system body changed
shader implementation changed with same interface
```

状态保持。

### PATCH_WITH_SCOPED_RESET

局部 schema/identity 不兼容，但不需要整个 process restart。

例：

```text
one private component state slot changes incompatible type
one subtree loses stable identity
one System state schema cannot be preserved
```

只 reset 最窄 scope。

### WARM_RESTART_REQUIRED

当前 Running App executable/ABI 必须替换。

典型：

```text
Rust native code changed
native schema changed beyond dev patch boundary
platform binding changed
Rust type layout used by native runtime changed
link-time feature changed
```

流程：

```text
build candidate binary while old app keeps running
    ↓
success
    ↓
capture compatible DevSnapshot
    ↓
stop/replace/relaunch candidate
    ↓
reconnect Dev Runtime
    ↓
restore snapshot
```

---

# Part V — `.vs` Typed Semantic Patch

## 9. Do not ship source diff to runtime

Host 可以使用 source/CST diff 做 incremental compilation，但 runtime wire artifact 应是 typed semantic patch。

错误：

```text
"line 18 changed from X to Y"
```

正确方向：

```text
PropertyPatch
InsertNode
RemoveNode
MoveNode
BindingPatch
ActionBodyPatch
ComponentSchemaPatch
SystemPatch
```

Runtime 不需要重新解析 `.vs` source。

当前实现（`viso-view::dev::patch`）：`ui` section 的每个 view 是 `ViewPatch { file, package, plan }`——`package` 是 candidate 的 release 形态 `ViewPackage`（与 release build 嵌入的同一格式：保留树、绑定边、已校验 behavior 字节码、state/handler/control/region/env 表，catalog 已编译进去），`plan` 是 host 由 last-good 与 candidate 的 IR 算出的 `ReloadPlan`：

```text
ReloadPlan
  preserving          candidate 是否原地保留 last-good 的每个节点
  nodes[]             NodeCarry { from, to: NodeRef, carries: MigratableState }
  states[]            StatePlan { key: StateKey, action: Keep|Convert|Reset|New,
                                  initial, from_slot, slot, retype: RetypePlan? }
NodeRef = Static(先序静态序号) | Region { region, arm, item }
RetypePlan { conversion: Retyping?, migrator: (Retyping, chunk)?, held,
             name, from, to, at }        # name/from/to/at 只用于 reset notice
```

Dropped 的 state 不进 plan。app 端提交引擎（`viso-view::dev::commit`）只按静态序号与 `StateKey` 工作，不做名字查找、不做模板 diff、不解析源码。host 不知道某挂载所运行版本的 IR 时（§1.3 的 `source_hash` 不是 session 读到过的版本）发送 `ReloadPlan::fresh`：重建全部节点、每个 cell 从初值开始。

---

## 10. Compile pipeline

```text
changed .vs
   ↓
incremental token/CST reparse
   ↓
affected module graph
   ↓
name resolution
   ↓
type/effect/capability check
   ↓
Typed HIR candidate
   ↓
old HIR / schema comparison
   ↓
semantic patch plan
```

只有 candidate 完整通过验证才可以进入 stage。

---

## 11. Symbol identity

Hot Reload 使用 stable `SymbolId` 判断跨编译声明身份。

```text
old declaration SymbolId
          │
          ├── same --> preserve/patch candidate
          │
          └── absent/different --> replace/reset analysis
```

Runtime steady-state 仍使用 dense runtime ID。

Hot Reload 允许：

```text
SymbolId -> runtime typed ID
```

在 patch linking 阶段查询；禁止把 stable 128-bit identity 放进每节点 frame hot storage。

当前实现把这一步放在 host：plan 在 host 按 `NodeKey`/`SymbolId` 对齐后降为 runtime 名字——节点为静态序号或 region/arm/item，state 为持久 `StateKey`（`SymbolId` 的 runtime 孪生，cell 分配时已登记）——app 端无需 link 表。

---

## 12. UI patch examples

### Property-only

```viso
Text {
    color: #ff0000;
}
```

变更只应产生类似：

```text
PropertyPatch(TextNode, color)
→ DirtyMask::PAINT
```

不允许因为一个颜色变化重新 build 整棵 UI tree。

### Layout property

```text
padding 8dp -> 12dp
```

产生：

```text
PropertyPatch
→ MEASURE/LAYOUT dirty on affected boundary
```

### Structural insert

```viso
Column {
    Text { text: "Title"; }
    Button { text: "Save"; }
}
```

新增 Text 时：

```text
InsertNode(parent, position, type, initial bindings)
```

具名/稳定 sibling 的 state/focus 不应因为前面插入无关节点而丢失。

### Commit 作用域

一次 commit 只触及被 reload 的 view 自己的子树：

- 结构保持且不含 region 的 patch 按静态序号原地复用该 view 的每个静态节点，只重设 size/gap/长度等声明值，只为原本未绑定的边标脏，只 flush 值变化的 cell；因此一个属性编辑只标脏该节点该属性的 `DirtyClass`（宽度编辑 = 该节点 `LAYOUT|PAINT`，文本编辑 = 该文本节点一次重新 shaping，其它节点不脏）；
- 结构性 patch 按 `StructuralOp::{Remove,Replace,Insert}` 逐节点改动，不是释放该 view 的根重建：只有 op 具名的节点被释放/新建，未具名的每个节点原地保留它的 `NodeId`，不需要为它迁移任何状态；`Replace` 新建的子树仍按 migration plan 把被替换节点（及其因祖先被替换而一并重建的、本应保留的子节点）的可迁移状态装到新节点；
- rebind 只替换该 view 节点的静态边，其它节点的边保持不变；
- 只移除该 view 自己注册的 region / value hook。

同一窗口中 view 之外的内容（兄弟节点、其它 view、宿主 Rust UI）不受影响。

结构性 patch 释放一个 `Replace` 节点的旧子树前，按 migration plan 取出它、及因它一并重建的每个本应保留的子节点的可迁移状态（焦点、非零滚动偏移、编辑缓冲、进行中的转场、`ACTIVE_CHILD` 标记的某个 Widget 的"哪个直接子节点在显示"，以 Widget Schema 标记为准，DSL §94.2），重建后按 plan 的候选 `NodeRef` 装到新节点；进行中的转场等新视图投递值后再续上；Region 挂载的节点在新 Region 挂载后，按候选 region/arm/item 与外层 `for` 的 Item Key 路径装到新挂载的节点。滚动偏移推迟到新节点首次布局后恢复。焦点原在该 view 内而未迁移时清除焦点并报告 `focus_lost`，未迁移的非零滚动偏移计入 `scroll_lost`。

---

# Part VI — State Preservation

## 13. State identity

状态保留依据：

```text
owning SymbolId
state slot SymbolId
concrete type/schema fingerprint
preservation policy
```

不是 source line number。

UI 状态 cell 与 behavior VM 的状态 slot 走同一份 migration plan：plan 按 `SymbolId` 把 last-good 组件的 slot 与 candidate 组件的 slot 配对，reload 后的 host 把每个保留状态的 VM 值从旧 slot 搬到新 slot。状态改名、组件改名（`SymbolId` 随声明路径变化）或被删除时不按名字搬运，新 slot 从其 initializer 开始。handler 体的编辑只换 bytecode，每个状态（包括没有 UI cell 镜像的 `String`、`List`、record）保持原值。

---

## 14. Compatibility

### Exact compatible

```text
I32 -> I32
Vec2F32 -> Vec2F32
same record schema
```

直接保留。

### Explicitly convertible

```text
I32 -> I64
F32 -> F64
I64 -> F64            活值在 ±2^53 内
record + 带默认值字段
enum 重排 / 删除未持有的 variant
```

由 Viso 1.0 state-conversion table（DSL §94.1）明确定义；不能让 runtime 猜。转换表在 commit 前作为纯数据算出（每个状态一份 `Retyping`），commit 只对活值应用它：behavior 的状态读取旧 host 的活值、转换后写入新 host，UI cell 镜像的状态再写回 cell；新增字段的默认值由新 module 计算。

### Incompatible

```text
I32 -> String
record A -> unrelated record B
```

必须：

```text
scoped reset
or explicit user-defined dev conversion hook
```

dev conversion hook 即 `@migrate(from: "T")` 函数（DSL §94.1）：静态不可转换或活值转换失败时，活值先按转换表转换为其参数类型，再由新 module 的该函数算出新值；函数 Fault 时按重置处理，`E5101` 附带 Fault 信息。

禁止 reinterpret raw memory。重置只作用于该状态：其他状态照常保留，reload 照常提交，并报告 `E5101` 警告。两个状态持有同一 stable identity 时 candidate 被拒绝（`E5102`）。

---

## 15. UI ephemeral state

可保留：

```text
focus owner
scroll offset
selection
text editing selection/composition when compatible
active tab/navigation state
animation progress when animation identity remains compatible
```

每类状态必须有自己的 preservation contract。

当前 contract：focus owner、scroll offset、text editing（文本、选区、composition）、进行中的 `transition.*`（DSL §94.2）与 active tab/navigation state（`MigratableState::ACTIVE_CHILD`：`Tabs`/`NavigationStack` 等 Widget 的"哪个直接子节点的 `hidden` 为否"）随被保留节点迁移（见 Commit 作用域）。

不能用“dump UI object memory”实现。

---

# Part VII — Game System Hot Reload

## 16. Fixed tick boundary

Game System Patch 不能在 Fixed Tick 中间切换。

```text
Tick N
  old systems
  physics
  post physics
  commit
---------------- atomic reload barrier
Tick N+1
  new systems
```

如果 patch 在 Tick N 执行期间到达：

```text
stage now
commit after Tick N
```

---

## 17. Preserve World, patch logic

compatible logic-only patch：

```text
World stays
Entity IDs stay
compatible System state stays
score/session state stays
new FixedUpdate code starts next tick
```

不允许为了改移动速度销毁整个 GameWorld。

---

## 18. Determinism

Hot Reload 本身是一次明确的 simulation boundary event。

Replay/debug trace 可以记录：

```text
PatchRevision applied at TickId
SystemCodeRevision
StateReset events
```

在 deterministic replay 模式中，可以：

- 禁止 live patch；或
- 将 patch artifact 固定记录进 replay timeline。

不能 silently 改变 replay 语义。

---

# Part VIII — Shader Hot Reload

## 19. Shadow compile

```text
shader source change
    ↓
parse/typecheck Shader IR
    ↓
backend codegen
    ↓
create/validate candidate pipeline
    ↓
ready
```

任何失败：

```text
keep current last-good pipeline
```

画面不能因为编辑中的 shader syntax error 变成永久黑屏。

---

## 20. Interface compatibility

同 interface：

```text
pipeline implementation swap
```

如果 instance/uniform/resource interface 改变：

```text
validate dependent material/render schemas
rebuild affected bindings/resources
then scoped commit
```

不能仅因为 shader compile 成功就假设 host GPU ABI 兼容。

---

## 21. GPU-safe boundary

Pipeline swap 必须在 renderer/GPU 定义的安全 frame boundary 提交。

旧 pipeline/resource 的销毁遵循 GPU lifetime/fence contract，不在 apply patch 后立即 free 尚在 flight 的资源。

---

# Part IX — Assets and Fonts

## 22. Resource patch

Asset change：

```text
content hash changed
    ↓
decode/build candidate
    ↓
ResourceRevision++
    ↓
update logical ResourceId mapping
    ↓
precise dependents dirty
```

旧资源只有在新资源 ready 后才替换。

---

## 23. Font patch

App font 或 Dev External Font Provider 数据更新：

```text
FontFaceRevision / subset revision changes
    ↓
font coverage/cache update
    ↓
only paragraphs/glyph runs depending on affected face/coverage invalidate
```

不要：

```text
clear all font cache
clear all paragraph cache
rebuild whole UI
```

远程 progressive font 在 Dev Runtime 和 Release runtime 都可以作为**产品资源能力**存在；但“因源码文件变化触发 Dev patch”的部分只属于 Dev Runtime。

---

# Part X — Rust Warm Restart

## 24. No arbitrary Rust machine-code injection

Viso 1.0 不把以下能力作为架构目标：

```text
JIT arbitrary Rust
replace random machine-code pages in running process
dylib swap as universal application code hot reload
unsafe native ABI guessing
```

原因包括：

```text
Rust AOT/link model
platform ABI
code signing / W^X
static data initialization
thread stacks
OS/GPU handles
cross-platform consistency
```

---

## 25. Rust rebuild flow

Rust source change：

```text
Host detects affected Rust dependency closure
    ↓
start cargo incremental build
    ↓
OLD APP CONTINUES RUNNING
    ↓
build fails? ---- yes ---> keep old app + diagnostics
    │ no
    ▼
new executable/artifact ready
    ↓
request DevSnapshot
    ↓
stop/replace/reinstall candidate
    ↓
launch
    ↓
handshake
    ↓
restore compatible DevSnapshot
    ↓
resume dev session
```

关键 UX：

> Rust 编译期间不要先杀死当前可运行 App。

---

## 26. Desktop warm restart

Desktop：

```text
snapshot
spawn/replace host process
reconnect
restore
```

旧 process 的窗口是否复用 OS handle 不作为要求；窗口 geometry 和 app state 可以通过 snapshot 恢复。

---

## 27. Android Emulator warm restart

```text
cargo/native rebuild
package dev APK artifact
install/replace into selected emulator
launch
reconnect via dev transport
restore
```

Viso CLI 负责 emulator profile → adb serial。

如果安装过程需要停止旧 process，尽量延迟到 candidate 完全 build 成功之后。

---

## 28. iOS Simulator warm restart

```text
rebuild simulator artifact
install/replace in selected Simulator
launch
reconnect
restore
```

这是 Simulator-only development path，不涉及 physical-device signing/provisioning contract。

---

# Part XI — DevSnapshot

## 29. DevSnapshot is typed state, not memory dump

DevSnapshot 只保存 Viso 明确理解、能够验证 schema 的开发状态。

允许候选：

```text
Component state
System state
navigation stack / route state
selected tabs
scroll offsets
focus identity
text editing logical state
window geometry
explicit app dev-restorable data
game snapshot when Game Profile exposes compatible snapshot contract
```

---

## 30. Never snapshot raw runtime resources

默认禁止：

```text
raw pointer
GPU resource handle
OS window handle
file descriptor
socket
audio device handle
mutex/condvar internals
thread stack
Future/task machine stack
JNI/ObjC opaque object pointer
platform service object
```

这些在新 process 中重新建立。

---

## 31. Snapshot schema

概念结构：

```rust
struct DevSnapshot {
    snapshot_version: u32,
    source_build: BuildId,
    app_schema: SchemaFingerprint,
    component_state: Vec<ComponentStateRecord>,
    system_state: Vec<SystemStateRecord>,
    ui_ephemeral: UiEphemeralState,
    app_extensions: Vec<DevStateExtension>,
}
```

具体 wire 使用 Ende Binary，不 raw-dump Rust memory。

---

## 32. Snapshot storage/security

默认：

```text
ephemeral
host-local
current dev session only
```

不默认持久化到 repo。

可能包含 token/用户输入的状态不得自动打印到 CLI JSON/verbose log。

如果未来允许落盘：

- 必须显式 opt-in；
- 路径在 ignored dev cache；
- 支持敏感字段 exclusion；
- 不成为 production persistence format。

---

# Part XII — Patch Protocol

## 33. Ende Binary on the wire

Runtime patch/ack 使用：

```text
viso-ende Binary
```

而不是 JSON。

JSON 用于：

```text
CLI diagnostics
Studio inspection
LSP/AI
human/machine logs
```

---

## 34. Handshake

连接建立：

```text
HostHello {
    protocol_version,
    dev_session_id,
    project_fingerprint,
    expected_build_id,
}

RuntimeHello {
    protocol_version,
    runtime_session_id,
    build_id,
    current_revision,
    schema_fingerprint,
    capabilities,
}
```

不兼容 protocol：

```text
reject clearly
request restart/rebuild
```

不能 decoder 猜版本。

已实现（`viso_view::dev::wire`，`DEV_PROTOCOL_VERSION = 4`；version 2 是 runtime 先说话、绑定 session/build/revision 的 handshake，version 3 加入 mount inventory、host 对被拒编辑的 `Failure` 与 `ui` section，version 4 把 `ui` section 换成 typed patch）：

- runtime 连接并先说话：`RuntimeHello { protocol_version, token, dev_session, runtime_session, build_id, current_revision, schema_fingerprint, capabilities, target }`。连接方用 session token 证明自己是本 session 启动的 app，所以 token 在 `RuntimeHello` 里而不在 `HostHello` 里——host 不向尚未证明身份的连接发送任何东西；
- host 回 `HostHello { protocol_version, dev_session, runtime_session, project_fingerprint, expected_build_id }`（接受，`runtime_session` 原样回显）或 `Reject { Protocol{host} | Build | Session | Schema }`；token 不符的连接不回应直接关闭（不给猜 token 的 oracle）。runtime 也检查 `HostHello`：protocol、session、runtime session 与 build 任一不符即断开；
- hello 与 reject 的前缀在所有 protocol version 中保持不变（stream `ProtocolTag`，再是 dev protocol version），解码在读到别的 version 时立即以该 version 号停止，不去解读其后布局可能不同的字段；
- build id 由 CLI 经 `VISO_DEV_BUILD` 交给所启动的 app，app 原样报告，host 用它拒绝不是其当前编译目标的 artifact（Rust rebuild 之后仍连着的旧 app）；`schema_fingerprint` 取自 mount record（§4.1），与 host 的不同即 `Reject::Schema`；`capabilities` 是 runtime 能 apply 的 domain 集合（当前为 `ui`），host 不发送其外的 domain。

---

## 35. PatchBundle

概念：

```rust
struct PatchBundle {
    dev_session: DevSessionId,
    target_runtime: RuntimeSessionId,
    base_revision: Revision,
    next_revision: Revision,
    build_id: BuildId,
    modules: Vec<ModulePatch>,
    ui: Vec<UiPatch>,
    systems: Vec<SystemPatch>,
    shaders: Vec<ShaderPatch>,
    resources: Vec<ResourcePatch>,
    state_plan: StatePreservationPlan,
}
```

要求：

- bounded lengths；
- stable message tags；
- canonical ID encoding；
- no field-name strings in shared-schema binary hot protocol；
- protocol version negotiation at connection/build boundary。

已实现的 wire 形状：`PatchBundle { dev_session, target_runtime, base_revision, next_revision, build_id, sections }`，`sections` 每个 domain 至多一段、按 tag 排序，整个 bundle 一起 commit 或都不 commit（§39）。domain tag 稳定：`ui=0 module=1 state=2 system=3 shader=4 resource=5`；每个 domain 的 payload 由 apply 它的阶段定义，此前该 tag 保留，解码为 `UnsupportedDomain`；同一 domain 出现两次或乱序是 malformed。ID 是定宽 128-bit（两个 u64 LE），revision 是 varint；所有字符串与计数在读之前检查上限（token 64 B、NACK code 至多 64 个且每个 32 B、log 16 KiB、section 4096、文件 4096、路径 4 KiB、名字 256 B、module segment 64、capability 256、每个 view 的 node carry 与 state plan 各至多 65536、failure 16 行且每行 1 KiB、notice 64），且不超过剩余字节；嵌套层数由消息结构固定（`ui` section → view → package / plan），唯一递归的 `Retyping` 有深度上限。帧上限 4 MiB，超限的帧在读 body 之前拒绝。

`ui` section 是 `UiPatch { views: [ViewPatch{file, package, plan}] }`（§9）：candidate 的 release 形态与 host 算出的 reload plan。behavior 字节码在 stage 时由 runtime 的 verifier 重新校验，校验不过或缺少所挂载的 component 时 NACK `NACK_UNLOADABLE_VIEW`；catalog 已编译进 package，不再单独发送。只有 host 接受的编辑会被发送——被拒的编辑从不离开 host（§41）。app 不含 `.vs` 编译器（§1.3）。

另外两条消息（version 3 起）：

- `RuntimeMessage::Mounts([MountEntry{file, path, package, module, language, catalog, capabilities, source_hash}])`：每个首次挂载的文件一条，`file` 是 runtime 分配的 `FileId`，patch 与 failure 都用它指代文件；host 据此编译与 build 完全一致的 candidate，并按 `source_hash` 知道 runtime 运行的是哪个版本——app 不发源码；
- `HostMessage::Failure { file, lines }`：host 拒绝某文件最新编辑时的 overlay 行（`path:line:col: CODE message`，至多 16 行）；`lines` 为空表示清除（编辑被撤回到 runtime 运行的版本）。它只是显示，不带 revision。

---

## 36. Revision rules

Runtime 只有在：

```text
patch.base_revision == runtime.current_revision
```

时才直接应用。

否则：

```text
NACK_REVISION_MISMATCH
```

Host 重新生成从 runtime revision 到 candidate revision 的 patch，或要求 warm restart。

禁止 blind apply out-of-order patch。

已实现：runtime 在 stage 之前按序检查（`RuntimeIdentity::check`）——session 与 runtime session（否则 `NACK_UNKNOWN_SESSION`）、build（`NACK_BUILD_MISMATCH`）、`base_revision == current_revision`（`NACK_REVISION_MISMATCH`）、`next_revision > base_revision`（`NACK_REVISION_ORDER`，host 可以跳号）、只含 runtime 声明的 domain（`NACK_UNSUPPORTED_DOMAIN`）。已 stage 未 commit 的 patch 之后到达的 patch 以前者的 `next_revision` 为基准检查，可以串联；同一 frame boundary 按 revision 顺序逐个 commit、逐个 ACK。launch 时 revision 为 1。

---

## 37. ACK / NACK

成功：

```text
PatchAck {
    revision,
    applied_domains,
    scoped_resets,
    timings,
}
```

失败：

```text
PatchNack {
    base_revision,
    candidate_revision,
    stage,
    diagnostic_codes,
    last_good_revision,
}
```

NACK 后 running app 保持 last-good revision。

已实现：`PatchAck { revision, applied_domains, files: [FileCommit{file, counts}], notices, timings{decode_us, stage_us, commit_us} }`，`counts` 是每个 view 提交到其所有挂载时保留与丢失的计数（`mounts, migrated, reset, focus_lost, scroll_lost, handlers_lost`）与 `dirty`（§11 八个 dirty class 各一个 node 计数，由 `commit()` 提交后扫描该 view 的静态节点在 `NodeStore` 中实际携带的 dirty bit 得出，再按文件对每个挂载求和——即 commit 要重做的量，不是语义上"改了什么"），`scoped_resets` 由保留/丢失计数求和（不含 `dirty`）；`notices` 是 commit 产生的提示（`E5101` 状态重置），span 指向 host 为该文件编译的 candidate 源码（plan 带着 state 声明的范围），由 host 解析成行列。`PatchNack { base_revision, candidate_revision, stage, diagnostic_codes, last_good_revision }`，`stage` 取 §51 的全部 stage。NACK code 是稳定字符串：上面 §36 的五个，host frame 无法解码时的 `NACK_MALFORMED`（帧长度保住了帧边界，channel 不断开；超过帧上限的帧则读不到边界，连接结束），patch 指向 runtime 未报告的文件时的 `NACK_UNKNOWN_FILE`，candidate 无法加载时的 `NACK_UNLOADABLE_VIEW`（stage `runtime-stage`）。runtime 端的 link 不阻塞 UI loop：入站 frame 经有界队列交给 loop 并唤醒它（队列满时阻塞的是 reader 线程），出站消息 `try_send` 进有界队列，满了就丢弃并计数，计数在有空位时以 `Dropped{count}` 发出。

### 37.1 当前实现：`.vs` reload event

host 为每个 candidate revision 的每个 `.vs` 文件构造一条 report（`tools/cli/src/dev/report.rs`），由它自己的编译与 runtime 的 ACK/NACK 组成：`base_revision, candidate_revision, last_good_revision, outcome(applied|scoped_reset|rejected|held), stage, elapsed, counts, codes`（`held`：`--no-hot-reload` 下编译通过、未发送，stage 为 `transport`；`counts.dirty` 随 `counts` 一起来自 ACK，rejected/held 时全为 0）；诊断由 host 用它编译的源码给出行列，app 不发源码也不发诊断。`--json` 的 `dev` payload 在这些字段之外另带 `runtime_session_id`（已连接 runtime 的 `RuntimeSessionId` 十六进制；没有已连接 runtime 时为空字符串）与嵌套的 `dirty` object（`Viso_CLI.md` §36.4）。

- 一个 batch 是一个 candidate revision（被拒的 candidate 也消耗一个号，下一个 patch 跳号）；host 拒绝的文件 `stage` 是 §51 中的失败 stage（`parse`、`resolve`、`typecheck`、`capability`、`state-compat`、`shader-compile`），runtime NACK 的文件是 NACK 的 stage，提交的文件为 `runtime-commit`，计数来自 ACK；
- `elapsed` 从 batch 开始编译算到 runtime 的回答（含编译、传输、等待帧边界与提交）；
- host 拒绝时，app 收到 `Failure`，在挂载该文件的 window 的 last-good UI 之上画 overlay，列出错误（至多 8 条，余数汇总）；该文件的 patch 提交、或编辑被撤回（host 发空 `Failure`）时移除。overlay 是 window store 中的 detached subtree，不在 semantics tree、不接受输入；release 不编译它。

---

# Part XIII — Staging and Atomic Boundaries

## 38. No partial live mutation during validation

Candidate 在 stage 时不得先改真实 retained runtime，再“发现后面失败”。

需要 staging 的对象包括：

```text
new component/schema descriptors
new UI nodes/structure plan
new shader pipelines
new resource payloads
new system bytecode/IR
state conversion results
```

---

## 39. Domain atomic boundaries

```text
UI / Reactive patch
    -> Frame Boundary

Game FixedUpdate patch
    -> Fixed Tick Boundary

Shader pipeline patch
    -> GPU-safe Frame Boundary

Resource patch
    -> Resource Ready + Frame Boundary

Rust executable replacement
    -> Warm Restart Boundary
```

一个 PatchBundle 可以包含多个 domain，但 commit coordinator 必须保证用户看不到“半新半旧”的非法组合。

---

## 40. Cross-domain patch

例如 `.vs` 同时改：

```text
Button structure
+ shader interface
+ Game System UI observable
```

Host 必须生成统一 candidate revision。

如果 shader validation 失败：

```text
whole candidate revision NACK
```

除非 compiler 能证明各 patch 是独立 revision 并明确拆包。

简单优先：1.0 默认同一 save batch 形成一个 candidate revision。

---

# Part XIV — Last-good Runtime

## 41. Last-good is a first-class contract

开发者保存错误代码时：

```text
running app keeps rendering/responding
```

而不是：

```text
blank screen
half-built tree
invalid shader pipeline
corrupt GameWorld
```

---

## 42. Error loop

```text
Revision 10 running
    ↓
edit candidate 11
    ↓
compile error
    ↓
Revision 10 keeps running
    ↓
edit candidate 12
    ↓
valid
    ↓
apply as next runtime revision
```

Runtime revision 不需要等于每次 source-save counter；只追踪成功 commit 的 revision。

---

# Part XV — Multiple Running Sessions

## 43. One source graph, multiple runtimes

Dev tooling 应允许一个 project session 服务多个 running runtime connection，即使 public CLI 1.0 首先只要求一次 `viso run` 管理一个主要 target。

```text
Typed HIR candidate
      │
      ├── desktop runtime
      ├── iOS Simulator
      ├── Android Emulator
      └── Web runtime
```

共享：

```text
source parse
module graph
most type checking
semantic diff intent
```

Target-specific：

```text
shader backend validation
platform capabilities
resource format
native build
DOM/WebGPU lowering
```

这为 Studio Device Matrix 留出架构空间，但不要求 CLI 1.0 一条命令同时启动所有平台。

---

## 44. Runtime-specific revisions

某 target shader candidate 失败时：

```text
iOS revision may advance
Android revision may remain last-good
```

Studio/CLI 必须清楚显示每个 runtime 的 revision/status，不能假装所有 target 永远同步。

---

# Part XVI — Security

## 45. Development transport is privileged

Dev Runtime 可以：

```text
change UI behavior
replace shaders/resources
query state
capture snapshots
control inspector
```

因此必须当作 privileged development interface。

---

## 46. Connection security defaults

默认：

- local host / simulator / emulator scope；
- ephemeral session token；
- handshake 绑定 project/build/session；
- 不监听公网 `0.0.0.0`；
- Web dev server 若暴露 LAN，必须显式 opt-in；
- unknown session rejected；
- malformed Ende payload bounded decode；
- Release artifact 根本没有此 endpoint。

当前实现：`viso run` 在 `127.0.0.1` 临时端口监听，经 `VISO_DEV_RUNTIME`/`VISO_DEV_TOKEN`/`VISO_DEV_SESSION`/`VISO_DEV_BUILD` 交给 app；app 拒绝非 loopback 地址；handshake（§34）绑定 token、dev session、runtime session、build 与编译器 schema，host 的 `HostHello` 带 project fingerprint；host 为每个接受的 runtime 起一条 writer 线程，session 从不阻塞在读得慢的 app 上；每个 patch 再按 session、launch 与 build 检查（§36）；所有解码有界（§35），帧长上限 4 MiB。

---

## 47. No secrets in diagnostics

Patch diagnostics/CLI JSON 不打印：

```text
app auth tokens
secure storage contents
arbitrary text field contents unless user asked Inspector to reveal
private keys/signing secrets
```

DevSnapshot 也遵守敏感字段 policy。

---

# Part XVII — Performance

## 48. Main metrics

`viso run` development loop应测：

```text
file event -> semantic change latency
CST incremental reparse
module invalidation count
Typed HIR rebuild time
semantic diff time
patch encode bytes/time
transport time
runtime validate/stage time
atomic commit time
first visible frame latency
Rust incremental build time
Warm Restart snapshot time
relaunch/install time
restore time
```

当前实现（`.vs` 单属性编辑，release，Apple M4），按 host/app 拆开的各段：

- host 检测：写盘到 session 收到约 6.0 ms（几乎全是 kqueue 的 5 ms 静默窗口，§7.1），再加 5 ms coalesce 窗口；
- host 编译：与文件大小成正比（36 KB 的文件 23 ms，其中解析 0.05 ms，§4）；
- host 编译 + plan + 编码与 app 端：`viso` 的 `hot_reload::tests::patch_to_pixels`（`#[ignore]`，`cargo test --release -p viso --features hot-reload --lib -- --ignored patch_to_pixels --nocapture`）对 counter view 的单个宽度编辑走完整 host 往返（host 编译 → plan → 编码 → loopback 传输 → app 解码、检查、加载并校验 → 帧边界 commit → relayout + repaint，不含 GPU upload/submit），60 次：

```text
compile            min 1.181 ms  median 1.334 ms  p95 2.541 ms
plan               min 0.002 ms  median 0.002 ms  p95 0.006 ms   # diff + migration + release 形态
encode             min 0.001 ms  median 0.001 ms  p95 0.003 ms   # patch 帧 449 B
transport          min 0.045 ms  median 0.074 ms  p95 0.090 ms   # 写、读、解码、检查、加载与校验
commit             min 0.005 ms  median 0.006 ms  p95 0.017 ms   # 原地 restyle，只标脏该节点
repaint            min 0.001 ms  median 0.001 ms  p95 0.002 ms
compile-to-pixels  min 1.265 ms  median 1.428 ms  p95 2.648 ms
```

app 端（transport + commit + repaint）中位数约 0.08 ms；此前 app 为每个 patch 重新编译源码时为 1.27 ms。端到端（保存 → 像素）= 检测（约 6 ms + 5 ms coalesce）+ 上表。inotify 无静默窗口；Linux 与 Windows 的数值未在此测量。GPU 上传与呈现不在测量内。

---

## 49. Hot patch targets

典型 UI property patch 目标：

```text
0 full-tree rebuild
0 full-app restart
0 source parse in runtime
0 string property lookup in node traversal
precise DirtyMask only
```

Shader：

```text
old pipeline remains usable while candidate compiles
```

Rust：

```text
old app remains running during host compile
```

---

## 50. Patch allocation

Hot Reload 是开发 cold/warm path，可以分配；但不能因此改变 release hot storage model。

Dev Runtime patch apply仍应避免明显 O(total nodes) work 对一个局部 property change。

---

# Part XVIII — Diagnostics and Tooling

## 51. Diagnostic stages

错误必须标注：

```text
watch
parse
resolve
typecheck
capability
patch-plan
state-compat
shader-compile
gpu-validate
resource-build
transport
runtime-stage
runtime-commit
warm-restart
snapshot-restore
```

不要所有错误都叫 `HOT_RELOAD_FAILED`。

已实现：wire 的 `Stage` 覆盖全部 15 个 stage；`dev` event 的 `stage` 用这些名字：host 编译失败按首个错误码的范围归到 `parse`、`resolve`、`typecheck`、`capability`、`state-compat`、`shader-compile`，传输与会话不符为 `transport`，runtime 的检查与 plan 为 `runtime-stage`，提交为 `runtime-commit`。`watch` 与 `patch-plan` 目前没有会失败的路径（watcher 读不到的文件不报告；patch 由 host 的 plan 直接组成）。

---

## 52. Studio status

Studio 至少显示：

```text
Running revision
Candidate revision
Last-good revision
Target/runtime
Patch class
Scoped resets
compile time
transport bytes
commit time
Rust rebuild state
connection state
```

---

## 53. CLI output

Human：

```text
✓ .vs patch r41 -> r42   37 ms   1 node paint-dirty
✗ shader candidate       kept r42   E_SHADER_TYPE
… Rust rebuild           old app still running
✓ warm restart           state restored
```

Machine：Ende JSON event stream。

---

# Part XIX — Internal Code Organization

## 54. Suggested tooling modules

```text
tools/dev/
├── session.rs
├── watcher.rs
├── coalesce.rs
├── connection.rs
├── protocol.rs
├── patch_plan.rs
├── snapshot.rs
├── warm_restart.rs
└── targets/
    ├── desktop.rs
    ├── android_emulator.rs
    ├── ios_simulator.rs
    └── web.rs
```

也可以作为 shared tooling crate 的 module；不要为了图漂亮立即拆出十个 crate。

已实现的布局是 `viso-cli` 里的一个 module，`tools/cli/src/dev/`：`mod.rs`（session、coalescer、candidate 与 patch 发送）、`watch/`（project watcher 与 scope，各平台后端）、`sources.rs`（source graph、每个挂载的 last-good 与 host 编译）、`link.rs`（连接管理）、`report.rs`（`dev` event 的 report）。patch planner 在 `viso-dsl::hotreload::patch`（diff + migration + retype 降为 `ReloadPlan`）。wire 协议在 `viso-view::dev::wire`，host 与 runtime 共用。

---

## 55. Runtime dev modules

```text
crates/runtime/src/dev/
├── mod.rs
├── handshake.rs
├── patch.rs
├── stage.rs
├── commit.rs
├── snapshot.rs
└── diagnostics.rs
```

整个 `dev` module tree 必须通过 build configuration 从 Release/Shipping binary 中消失。

已实现的布局：`viso-view::dev`（`hot-reload` feature）——`wire.rs`（协议）、`patch.rs`（typed UI patch 与其编码）、`commit.rs`（candidate 加载与提交引擎：原地 restyle 或重建、节点状态迁移、state 迁移与 retype、rebind、behavior 重挂）；facade 的 `viso::hot_reload`——`mod.rs`（session：inventory、stage、帧边界 commit、ACK/NACK）、`link.rs`（dev channel）、`overlay.rs`（失败 overlay）。

---

## 56. DSL compiler responsibilities

`viso-dsl` 提供：

```text
incremental source graph
stable SymbolId
schema fingerprints
Typed HIR diff
UI/Reactive/System semantic patch IR
state preservation metadata
source diagnostics
```

它不负责 device connection。

---

## 57. Runtime responsibilities

Runtime Dev layer提供：

```text
handshake
patch validation/linking
staging
atomic commit
scoped reset
DevSnapshot capture/restore hooks
ACK/NACK
```

Runtime core 的正常 frame semantics 不依赖 Dev Runtime 存在。

---

# Part XX — Build Integration

## 58. Dev configuration boundary

推荐概念：

```text
cfg(viso_dev_runtime)
```

或者等价的内部 Cargo/build flag。

要求不是名字，而是：

```text
release/shipping compile graph does not include dev apply/transport code
```

已实现的边界：

- 内部 flag 是 facade 的 Cargo feature `viso/hot-reload`，它只打开 `viso-view/hot-reload`（wire 协议、typed patch 的解码与提交引擎）与 facade 的 dev session；未开 feature 的 artifact 不链接 dev channel；任何 artifact 都不链接 `.vs` 编译器 `viso-dsl`——`ui!`/`view!` 的 proc-macro 在构建期使用它，dev artifact 应用的是 host 编译好的 patch（§9）；app 端没有 watcher（§3），文件监听只在 `viso run` 里；
- facade 的 build script 是 build-time gate：`hot-reload` 与 `VISO_PROFILE=release|shipping` 同时出现时构建失败。gate 看 Viso artifact profile 而不是 Cargo profile——`--release` 优化过的 Dev artifact（如 `patch_to_pixels` 测量）仍是 Dev artifact；
- `viso run` 是唯一打开该 feature 的 CLI 路径，并以 `VISO_PROFILE=dev` 构建；构建 release/shipping artifact 的 CLI 命令必须设置对应的 `VISO_PROFILE`。
- 两者由 §64 的 `cargo xtask check-release-absence` 在 CI 中验证。

---

## 59. Profile semantics

```text
viso run
    -> Dev artifact + Dev Runtime

viso build
    -> normal dev build unless profile says otherwise

viso build --profile release
    -> Release AOT, no Dev Runtime

viso package ...
    -> Release/Shipping, no Dev Runtime
```

`Viso.toml` 不允许在 release/shipping 中把 Hot Reload重新开启。

---

## 60. Debug symbols are separate

Release 是否保留 crash/debug symbols，与 Hot Reload 是否存在是两个独立问题。

允许：

```text
release binary without Hot Reload
+ external debug symbols / source maps for crash analysis
```

不要因为“需要符号”把 Dev Runtime 带回 release。

---

# Part XXI — Testing

## 61. Unit tests

必须覆盖：

```text
semantic diff
SymbolId preservation
state compatibility matrix
revision ordering
PatchBundle bounds
ACK/NACK
scoped reset planning
snapshot schema
```

---

## 62. Integration tests

### UI

```text
launch dev app
change one property
assert no process restart
assert expected DirtyMask
assert state/focus/scroll preserved
```

### Invalid `.vs`

```text
running revision N
send invalid source
assert N remains running
assert no partial node change
```

### Shader

```text
valid pipeline
send invalid candidate
assert old pipeline remains
```

### Game

```text
run tick N
stage system patch
assert old code completes N
assert new code starts N+1
assert World identity preserved
```

### Rust

```text
run app with state
edit Rust
build candidate
assert old app runs until build success
warm restart
assert compatible state restored
```

---

## 63. Simulator/emulator tests

Android fake + real CI environment应覆盖：

```text
emulator boot
install
connect
hot patch
Rust warm restart
reconnect
```

iOS Simulator on macOS CI覆盖同样流程。

不要求 physical device 才能完成 Viso 1.0 dev-runtime DoD。

---

## 64. Release absence test

CI 必须对 Release/Shipping artifact 做静态/运行验证：

```text
no Dev Runtime symbols/sections where practical
no dev transport listener
PatchBundle message rejected because handler absent, not disabled
no DevSnapshot endpoint
no hot-reload runtime configuration key
```

这是安全与体积测试，不只是功能测试。

已实现的是 `cargo xtask check-release-absence [-p <package>] [--no-launch]`（CI 在 macOS 上带 launch、在 Linux 上 `--no-launch` 运行；默认 package 是挂载 `view!` 的 `viso-example-i18n`）：

- 同一组 marker（dev channel 的 env 名 `VISO_DEV_RUNTIME`/`VISO_DEV_TOKEN`/`VISO_DEV_SESSION`/`VISO_DEV_BUILD`、patch 检查与 candidate stage 的 NACK code、dev link 的线程名与类型名、失败 overlay 的文案）先在 `--release --features viso/hot-reload`、`VISO_PROFILE=dev` 的对照 artifact 中必须全部出现——marker 失效时在这里失败，而不是让 release 扫描空过；release artifact（`VISO_PROFILE=release`、无 feature）中必须一个都不出现。`viso_dsl` 符号在两个 artifact 中都不得出现（§1.3：app 不含编译器）。符号从可执行文件本身读取，所以扫描只在符号留在其中的平台（macOS、Linux）上运行；
- 同一构建在 `VISO_PROFILE=shipping` 下必须被 facade build script 拒绝（§58）；
- launch：两个 artifact 都以 dev channel 的全部环境变量（`VISO_DEV_RUNTIME` 指向 loopback listener，以及 token、session、build）启动；对照 artifact 必须连上，release artifact 必须持续运行对照连接耗时的 3 倍（至少 5 秒）且从不连接，提前退出也算失败；
- 尚未覆盖：PatchBundle、DevSnapshot endpoint 等尚不存在的 dev 层，它们落地时各自把 marker 加入列表。

---

# Part XXII — Implementation Order

## 65. Phase A — One-process desktop loop

先实现：

```text
viso run
watch .vs
incremental compile
semantic property patch
Ende transport
frame-boundary commit
last-good
```

不要一开始同时解决所有 mobile/network 问题。

---

## 66. Phase B — Structural/state patch

实现：

```text
insert/remove/reorder
StableKey
SymbolId linking
state preservation
focus/scroll preservation
scoped reset
```

---

## 67. Phase C — Shader/resource/game

实现：

```text
shader shadow pipeline
asset revision
font revision
Game System tick-boundary patch
```

---

## 68. Phase D — Rust Warm Restart

实现：

```text
cargo incremental coordinator
old-app-keeps-running
DevSnapshot
relaunch
restore
```

---

## 69. Phase E — Android Emulator / iOS Simulator / Web

扩展 transport和 deploy adapter，不改变 patch semantics。

---

## 70. Phase F — Multi-session Studio

实现：

```text
multiple runtime connections
per-target revision/status
Device Matrix
shared source graph
backend-specific validation
```

这不是最初 `viso run` 成功的前置条件。

---

# Part XXIII — Definition of Done

## 71. Development Runtime 1.0 完成标准

### Dev only

- Hot Reload 只在 Dev artifact 编入；
- Release/Shipping 完全没有 dev transport / patch apply / DevSnapshot endpoint；
- Release 热路径零 Hot Reload 分支税。

### UI

- `.vs` property patch 不重启 process；
- structural patch使用 stable identity；
- invalid candidate 保持 last-good；
- focus/scroll/state preservation有测试。

### Shader

- candidate 后台验证；
- 失败保留旧 pipeline；
- 成功 GPU-safe boundary切换。

### Game

- System patch只在 Fixed Tick boundary切换；
- compatible World/System state保留；
- replay能够记录/禁止 code revision event。

### Resources

- image/font/asset revision可独立更新；
- 不为局部资源变化清空全 UI cache。

### Rust

- Rust 编译期间旧 App 保持运行；
- build success 后 Warm Restart；
- typed DevSnapshot 恢复兼容状态；
- raw OS/GPU/runtime handles不进入 snapshot。

### Mobile/Web

- Android Emulator可 patch/restart/reconnect；
- iOS Simulator可 patch/restart/reconnect；
- Web Dev Runtime可 patch；
- 1.0 Dev Runtime 不依赖 physical-device debugging。

### Protocol

- Ende Binary bounded decode；
- revision ordered；
- protocol/schema不兼容明确拒绝；
- ACK/NACK稳定；
- malformed patch不能破坏 last-good runtime。

---

# Appendix A — Example Development Session

```text
$ viso run android --device pixel-local

Viso Dev
  target      android emulator
  device      pixel-local
  api         36
  hot reload  enabled
  revision    1

[run] app launched
[dev] connected runtime r1

# user edits view.vs
[patch] .vs candidate
[ok] r1 -> r2, 42 ms
     structure 0
     layout dirty 3
     paint dirty 7

# user makes shader syntax error
[shader] candidate failed E_SHADER_PARSE
[keep] runtime remains r2, last-good pipeline active

# user fixes shader
[ok] shader staged
[commit] r2 -> r3 at frame boundary

# user edits Rust
[rust] incremental build started; current app remains running
[rust] build succeeded
[snapshot] captured compatible state
[restart] replacing dev app
[restore] state restored
[dev] connected runtime r4
```

---

# Appendix B — Anti-patterns

## B.1 Release listener hidden behind flag

```text
release binary contains hot reload server
but defaults disabled
```

禁止。

## B.2 Send source to runtime and parse there

普通 `.vs` dev patch不应该要求 device runtime携带完整 compiler frontend。

## B.3 Restart on every UI save

如果任何 `.vs` 修改都重新安装/restart App，说明没有实现 Viso Hot Reload contract。

## B.4 Patch during Fixed Tick

禁止。

## B.5 Kill old app before Rust build succeeds

禁止；这会把普通编译错误变成开发中断。

## B.6 Snapshot raw memory

禁止。

## B.7 Apply shader before validation

禁止。

## B.8 Full tree rebuild for property patch

禁止作为默认路径。

---

# 结论

Viso Development Runtime 的目标可以压缩成六句话：

> **源码监听和编译在 Host。**  
> **Runtime 接收 typed artifact，不重新解释项目源码。**  
> **不同 domain 使用不同的最窄更新路径。**  
> **更新先 stage/validate，再在正确的原子边界 commit。**  
> **Rust 不做不可靠的任意机器码注入，而做 stateful Warm Restart。**  
> **Release / Shipping 完全不包含 Hot Reload Runtime。**

这使 Viso 可以同时拥有快速开发反馈、严格状态语义、Game/Shader 专用原子边界，以及没有开发期热更成本的发布运行时。
