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

作为每帧固定路径。

Release 不保留：

```text
per-frame dev socket poll
hot-reload SymbolId HashMap lookup
patch revision branch
source file watcher
state-preservation bookkeeping that is only needed by Dev Runtime
```

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

### 7.2 Content hash

只因 mtime 改变但 bytes 未变：

```text
no semantic compile
```

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

是否允许：

```text
I32 -> I64
F32 -> F64
```

必须由 Viso 1.0 state-conversion table 明确定义；不能让 runtime 猜。

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

禁止 reinterpret raw memory。

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
