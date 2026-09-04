# Viso CLI 设计规范

> 文档状态：Viso 1.0 Draft / CLI Specification  
> 命令名：`viso`  
> 配置文件：`Viso.toml`  
> 目标读者：Viso CLI/Tooling 工程师、Platform 工程师、Compiler 工程师、Studio 工程师、AI Coding Agent  
> 设计目标：让项目从创建、检查、构建、运行、调试、测试到打包与 Web 导出都通过一个稳定入口完成。

---

## 0. 定位

`viso` 是 Viso 的统一命令行 facade。

普通开发者不需要记住：

```text
cargo package names
xtask commands
compiler binaries
packager binaries
inspector binaries
platform-specific SDK commands
```

用户只需要：

```bash
viso ...
```

CLI 不拥有第二套 compiler、renderer、packager 或 inspector 实现。它只负责：

```text
parse args
resolve project/config
resolve target/device/profile
invoke shared tooling services
render human/JSON output
return stable exit code
```

核心原则：

> **One CLI, shared services, no duplicated toolchain logic.**

---

# Part I — CLI 总体合同

## 1. 顶层命令树

Viso 1.0 的命令面固定为：

```text
PROJECT
    viso new
    viso doctor
    viso config

ENVIRONMENT
    viso target list
    viso target info
    viso target install

    viso device list
    viso device info
    viso device boot
    viso device logs

DEVELOP
    viso run
    viso build
    viso serve

LANGUAGE
    viso fmt
    viso check
    viso schema
    viso explain
    viso dump
    viso lsp

TEST / DEBUG
    viso test
    viso snapshot
    viso inspect
    viso profile
    viso studio

DELIVERY
    viso package
    viso export

MAINTENANCE
    viso clean
    viso completion
```

不提供平台历史命令树，例如：

```text
apple ios ...
android adb ...
wasm toolchain ...
```

平台差异通过 target/device 参数表达。

---

## 2. 三种产物语义必须区分

### 2.1 `build`

```bash
viso build <target>
```

产生 **Viso application artifact**。

例如：

```bash
viso build macos
viso build android
viso build web-gpu
viso build web-dom
viso build web-hybrid
```

Artifact 仍由 Viso runtime、Viso generated runtime 或对应 backend 负责执行。

### 2.2 `package`

```bash
viso package <target>
```

产生 **可分发产物**。

例如：

```text
macOS      .app / selected distribution bundle
Windows    executable / selected installer format
Linux      executable / selected bundle format
iOS        signed application artifact
Android    APK/AAB
Web        deployment directory/archive
```

`package` 可以隐式执行 release build，但必须复用同一 build graph。

### 2.3 `export`

```bash
viso export <format>
```

产生 **可脱离 Viso 工程继续维护的外部生态源码或静态资产**。

Viso 1.0 exporter：

```text
html
solid
```

因此：

```bash
viso build web-dom
```

和：

```bash
viso export solid
```

不是同一个概念。

SolidJS 只属于 exporter，不属于 Viso HIR、UI IR、runtime 或 dependency graph。

---

## 3. Target 模型

标准 target 名称：

```text
host
macos
windows
linux
ios
android
web-gpu
web-dom
web-hybrid
headless
```

### 3.1 `host`

`host` 表示当前开发机原生 target：

```text
macOS   -> macos
Windows -> windows
Linux   -> linux
```

因此：

```bash
viso run
```

等价于：

```bash
viso run host
```

除非 `Viso.toml` 设置了 project default target。

### 3.2 Web target

```text
web-gpu
    Viso retained UI/render pipeline
    WASM + WebGPU
    最大 Viso rendering fidelity

web-dom
    Viso Typed UI IR -> DOM lowering
    HTML/CSS + Viso DOM reactive runtime
    优先 browser semantics / accessibility / SEO-compatible structure

web-hybrid
    DOM + Viso GPU islands
    普通 UI 使用 DOM
    shader/game/custom rendering 使用 WebGPU surface
```

### 3.3 `headless`

用于：

```text
CI
UI tests
layout tests
snapshot tests
compiler tests
automation
server-side validation
```

`headless` 不等价于浏览器 DOM。

---

## 4. Project discovery

CLI 从当前目录向父目录搜索：

```text
Viso.toml
```

找到后该目录成为 Viso project root。

搜索停止条件：

- filesystem root；
- 显式 `--project <path>`；
- 找到第一个 `Viso.toml`。

多 workspace 项目可以在 root `Viso.toml` 中声明 members。

### 4.1 显式项目路径

所有项目相关命令支持：

```bash
viso --project path/to/app check
```

也允许：

```bash
viso check --project path/to/app
```

Parser 必须将两种形式归一化为同一 global option。

---

## 5. 配置优先级

从高到低：

```text
CLI flags
    ↓
VISO_* environment variables
    ↓
Viso.toml target/profile override
    ↓
Viso.toml project defaults
    ↓
framework defaults
```

任何命令都可以通过：

```bash
viso config show
```

查看最终解析结果。

---

## 6. Global options

所有命令统一支持适用的 global options：

```text
--project <path>
--json
--quiet
--verbose
--color <auto|always|never>
--offline
--locked
--jobs <n>
--profile <name>
--target-dir <path>
--help
--version
```

### 6.1 `--json`

不是“把最终人类文本包成 JSON 字符串”。

它切换为稳定 **Ende JSON event stream**。

### 6.2 `--quiet`

只输出：

- fatal diagnostics；
- requested artifact paths；
- final summary。

### 6.3 `--verbose`

可输出：

- resolved config；
- toolchain commands；
- cache hit/miss；
- backend selection；
- build graph timing。

不得泄漏 secret。

---

## 7. Exit codes

稳定 exit code：

```text
0     success
1     source/check/test diagnostics failed
2     CLI usage / invalid argument
3     environment / SDK / target unavailable
4     build / compiler / linker failure
5     runtime / device / test execution failure
6     package / signing / export failure
7     internal protocol / tooling service failure
130   interrupted by user
```

AI/CI 不得依赖解析英文文本判断成功失败。

---

# Part II — Project commands

## 8. `viso new`

创建 Viso 项目。

```bash
viso new my_app
```

默认最小结构：

```text
my_app/
├── Cargo.toml
├── Viso.toml
├── assets/
└── src/
    ├── main.rs
    ├── app.rs
    └── app.vs
```

原则：

- 默认项目文件尽量少；
- 不创建无意义 `utils/`、`common/`；
- 不强迫页面多文件；
- 随项目增长再 progressive split。

### 8.1 模板

```bash
viso new my_app --template app
viso new my_game --template game
viso new landing --template web
viso new controls --template library
```

标准模板：

```text
app
    普通跨平台应用

game
    FixedUpdate + Game Profile 最小项目

web
    Web DOM/Hybrid 优先项目

library
    reusable Viso component/widget library
```

### 8.2 Options

```text
--template <app|game|web|library>
--name <package-name>
--edition <rust-edition>
--no-git
--force-empty-dir
```

默认不覆盖非空目录。

### 8.3 当前目录

```bash
mkdir foo
cd foo
viso new .
```

必须支持。

### 8.4 验收

```text
viso new smoke
cd smoke
viso check
viso run headless
```

必须成功。

---

## 9. `viso doctor`

检查 Viso 开发环境。

```bash
viso doctor
```

检查：

```text
Rust / rustup / cargo
Viso CLI/framework compatibility
Viso.toml
host compiler/linker
GPU backend capability
platform SDKs
mobile SDKs
WASM target
Web build tools
signing metadata availability
devices/simulators
filesystem permissions
required external tools
```

### 9.1 Target-specific doctor

```bash
viso doctor ios
viso doctor android
viso doctor web-gpu
```

### 9.2 输出示例

```text
Viso Doctor

[ok] Rust toolchain
[ok] Viso project
[ok] Metal backend
[ok] WebAssembly target
[warn] Android SDK not configured
[fail] iOS signing identity unavailable

Suggested actions:
  viso target install android
  open Xcode once to complete iOS setup
```

### 9.3 `doctor` 不偷偷做大规模修改

默认只读。

安全、确定的小修复可以显式：

```bash
viso doctor --fix
```

但 SDK 下载/系统级修改优先交给：

```bash
viso target install <target>
```

### 9.4 JSON

```bash
viso doctor --json
```

每个 check 输出：

```json
{"type":"doctor_check","name":"rust","status":"ok"}
{"type":"doctor_check","name":"android-sdk","status":"missing","code":"ENV_ANDROID_SDK"}
```

---

## 10. `viso config`

用于查看和验证最终配置，不直接替代文本编辑器。

### 10.1 Show

```bash
viso config show
```

输出合并后的配置。

### 10.2 Get

```bash
viso config get package.name
viso config get build.default-target
```

### 10.3 Path

```bash
viso config path
```

输出实际使用的：

```text
/path/to/project/Viso.toml
```

### 10.4 Validate

```bash
viso config validate
```

检查：

- unknown keys；
- invalid target；
- conflicting profile；
- unsupported exporter settings；
- invalid resource path；
- signing config shape。

### 10.5 不提供隐式 magic write

不设计：

```text
viso config set arbitrary.deep.key ...
```

作为核心工作流。

原因：配置应接受 code review，文本文件是 source of truth。

---

# Part III — Environment commands

## 11. `viso target`

统一 target/toolchain 管理。

### 11.1 List

```bash
viso target list
```

示例：

```text
TARGET       STATUS       DEFAULT BACKEND
macos        installed    metal
windows      unavailable  d3d12
linux        unavailable  vulkan
ios          installed    metal
android      missing-sdk  vulkan
web-gpu      installed    webgpu
web-dom      installed    dom
web-hybrid   installed    dom+webgpu
headless     installed    software/null
```

### 11.2 Info

```bash
viso target info android
```

显示：

- Rust target triple；
- required SDK；
- backend；
- available devices；
- build capabilities；
- packaging capabilities；
- current config overrides。

### 11.3 Install

```bash
viso target install android
viso target install ios
viso target install web-gpu
```

`install` 可以：

- 安装 Rust target；
- 下载 Viso-owned tool artifacts；
- 调用/指导官方 SDK 安装器；
- 验证版本；
- 缓存 toolchain metadata。

它不是通用 package manager。

### 11.4 Idempotent

重复：

```bash
viso target install web-gpu
```

应该快速返回 already-installed，而不是重复下载。

---

## 12. `viso device`

统一 physical device、simulator、emulator 的发现与基本控制。

### 12.1 List

```bash
viso device list
```

示例：

```text
ID                  PLATFORM  KIND       STATUS
iphone-local        ios       physical   connected
ios-sim-18-pro      ios       simulator  booted
pixel-local         android   physical   connected
pixel-api-37        android   emulator   stopped
```

过滤：

```bash
viso device list ios
viso device list android
```

### 12.2 Info

```bash
viso device info ios-sim-18-pro
```

显示：

- OS version；
- architecture；
- display scale/size；
- GPU/capability summary；
- connection state；
- debug deployment support。

### 12.3 Boot

```bash
viso device boot pixel-api-37
```

只对 simulator/emulator 有意义。

### 12.4 Logs

```bash
viso device logs iphone-local
```

过滤 app：

```bash
viso device logs iphone-local --app com.example.myapp
```

支持：

```text
--follow
--since <duration>
--level <trace|debug|info|warn|error>
```

---

# Part IV — Develop commands

## 13. `viso run`

这是普通开发的主命令。

```bash
viso run
```

默认：

```text
resolve project
resolve target
check environment
incremental build
launch/install
start .vs/shader/asset watcher
connect dev transport
stream diagnostics/logs
keep last-good app on hot reload errors
```

不要求额外 `watch` 命令。

### 13.1 Target

```bash
viso run macos
viso run ios
viso run android
viso run web-gpu
viso run web-dom
viso run web-hybrid
```

也支持：

```bash
viso run --target ios
```

Positional 和 `--target` 最终进入同一 resolved field；两者同时指定且不一致时报 usage error。

### 13.2 Device

```bash
viso run ios --device iphone-local
viso run android --device pixel-api-37
```

如果没有指定 device：

1. 单一可用设备：自动选择；
2. 多设备：使用项目默认；
3. 仍有歧义：人类模式交互选择；
4. `--json`/CI 模式：直接报结构化 ambiguity diagnostic，不进入交互 prompt。

### 13.3 Simulator shortcut

```bash
viso run ios --simulator
viso run android --emulator
```

只作为选择策略，不产生 platform-specific verb。

### 13.4 App arguments

`--` 后传给应用：

```bash
viso run -- --open demo.vs --safe-mode
```

### 13.5 Hot Reload

`viso run` Dev Session 默认监听：

```text
.vs
shader source
assets
Viso.toml relevant dev fields
Rust source
```

处理规则：

```text
.vs change
    incremental compile
    validate
    build patch
    atomic apply

shader change
    compile/validate
    atomic shader/pipeline replacement

asset change
    resource version update

Rust change
    cargo incremental build
    restart/reload according to supported dev boundary
```

任何编译失败：

```text
keep last-good running state
emit diagnostics
do not replace UI with blank/half-built state
```

### 13.6 Options

```text
--release
--profile <name>
--device <id>
--simulator
--emulator
--no-hot-reload
--inspect
--profile-frame
--open
--env KEY=VALUE
--cwd <path>
```

### 13.7 Ctrl-C

必须：

1. stop watcher；
2. request child graceful shutdown；
3. stop dev transport；
4. detach device session；
5. second Ctrl-C force kill。

---

## 14. `viso build`

只构建，不启动。

```bash
viso build
viso build android
viso build web-dom
```

### 14.1 Profiles

内建语义：

```text
dev
release
shipping
```

用户可在 `Viso.toml` 定义额外 profile。

```bash
viso build --profile shipping web-gpu
```

`--release` 是 `--profile release` 的便利别名。

### 14.2 Build profile 不是 target

不能创建：

```text
web-release
android-debug
```

这类组合 target 名。

应该：

```bash
viso build web-gpu --profile release
viso build android --profile dev
```

### 14.3 Web optimization policy

Web shipping profile 可以配置：

```toml
[profile.shipping.web]
strip = true
optimize = "size"
brotli = true
split = "auto"
threads = "auto"
source_maps = false
```

用户不应为了正常发布必须记底层优化工具名。

高级用户可通过显式 config/flag 覆盖 policy。

### 14.4 Artifact summary

Human output：

```text
Built web-gpu (shipping)
  wasm      dist/app.wasm       1.82 MiB
  js        dist/app.js         18 KiB
  assets    dist/assets/        6 files
  brotli    dist/app.wasm.br    612 KiB
```

JSON 输出 `artifact` events。

---

## 15. `viso serve`

只服务 Web target。

```bash
viso serve web-dom
viso serve web-gpu
viso serve web-hybrid
```

默认行为：

```text
build dev artifact
start local HTTP server
start watcher
serve correct MIME types
serve source maps
configure required WebGPU/wasm headers
print local/network URL
```

### 15.1 Options

```text
--host <ip>
--port <n>
--open
--lan
--https
--cert <path>
--key <path>
--no-hot-reload
```

### 15.2 Port selection

若默认端口被占用：

- human mode：自动选择相邻空闲端口并提示；
- `--json`：输出最终端口 event；
- 显式 `--port` 被占用：报错，不静默改端口。

### 15.3 Security headers

WebGPU/threaded WASM 所需 COOP/COEP 等 header 由 target/profile policy 决定，不能要求用户自己拼开发 server 配置。

---

# Part V — Language / Compiler commands

## 16. `viso fmt`

格式化：

```text
.vs
Viso.toml（仅规范化可安全处理的格式时）
```

Rust 继续由 `rustfmt` 负责；`viso fmt` 可以协调调用，但不重新实现 Rust formatter。

### 16.1 Usage

```bash
viso fmt
viso fmt src/app.vs
viso fmt src/features/home/view.vs
viso fmt --check
```

### 16.2 `--check`

不写文件，只检查是否已格式化。

CI 推荐：

```bash
viso fmt --check
```

### 16.3 Parser requirement

Formatter 基于 Lossless CST/AST，不使用正则批量重写。

---

## 17. `viso check`

执行无发布副作用的完整静态验证。

```bash
viso check
```

至少包含：

```text
Rust compile/check integration
.vs parse
name resolution
type checking
component/native schema
property/event validation
reactive graph
capability analysis
shader validation
resource references
target capability constraints
Viso.toml validation
architecture metadata required by project
```

### 17.1 Target check

```bash
viso check web-dom
viso check ios
```

可以提前发现：

```text
unsupported target capability
DOM-incompatible custom primitive
shader feature unavailable
mobile permission declaration missing
invalid signing metadata shape
```

### 17.2 Fast default

`viso check` 不应该默认执行完整 package/signing。

### 17.3 Watch

普通开发用 `viso run`。

如果编辑器/CI 明确只想连续静态检查，可以：

```bash
viso check --watch
```

这不是主要应用运行模式。

---

## 18. `viso schema`

查询 Viso typed schema。

```bash
viso schema Button
```

示例输出：

```text
viso::widgets::Button

Properties
  text        String             invalidates: measure|layout|paint|semantics
  disabled    Bool = false       invalidates: input|paint|semantics
  icon        Option<Image>

Events
  click       ClickEvent

Slots
  content     optional
```

### 18.1 Query forms

```bash
viso schema Button
viso schema viso::widgets::Button
viso schema Button.text
viso schema --search text
```

### 18.2 AI/tool use

```bash
viso schema Button --json
```

必须输出稳定 schema object，不要求 AI 解析人类表格。

### 18.3 Source origin

如果 schema 来自项目组件，应返回：

```text
source file
source span
SymbolId
visibility
schema revision
```

---

## 19. `viso explain`

解释结构化诊断码。

```bash
viso explain E3101
```

示例：

```text
E3101 Unknown Property

The component schema does not expose this property.

Suggested actions:
  viso schema <Component>
  viso schema <Component> --json
```

Diagnostic code 的说明应来自 compiler diagnostics registry，而不是 CLI 自己维护副本。

---

## 20. `viso dump`

用于 compiler/runtime advanced diagnostics。

```bash
viso dump ast src/app.vs
viso dump hir src/app.vs
viso dump ui-ir src/app.vs
viso dump reactive-ir src/app.vs
viso dump shader-ir RoundedRect
viso dump system-ir PlayerController
viso dump module-graph
```

支持：

```text
--out <path>
--pretty
--json
--symbol <path>
```

`dump` 不属于普通应用 authoring API，但必须稳定到足以支持 compiler tests、Studio 和 AI debugging。

---

## 21. `viso lsp`

启动 Viso Language Server。

默认：

```bash
viso lsp --stdio
```

支持：

```text
diagnostics
completion
goto definition
find references
rename
hover
semantic tokens
formatting
code actions
schema lookup
source-to-generated mapping
```

CLI 只负责 transport/launch；language intelligence 来自 `viso-dsl`/compiler services。

---

# Part VI — Test / Debug commands

## 22. `viso test`

统一 Viso-specific 测试入口。

```bash
viso test
```

测试域：

```text
unit
ui
game
web
all
```

### 22.1 Usage

```bash
viso test
viso test ui
viso test game
viso test web --target web-dom
```

Rust unit/integration tests仍可由 Cargo 执行；`viso test` 负责协调 Viso headless/UI/device/browser 测试。

### 22.2 Headless UI

```bash
viso test ui --headless
```

可检查：

```text
layout
semantics
input routing
state changes
paint primitives
snapshot
```

### 22.3 Game deterministic test

```bash
viso test game movement --frames 600 --seed 1234
```

支持固定：

```text
FixedUpdate count
input tape
random seed
clock
replay
```

### 22.4 Test filters

```text
--filter <pattern>
--exact
--jobs <n>
--fail-fast
--nocapture
--update-snapshots
```

---

## 23. `viso snapshot`

Snapshot 不只等于截图。

它可以包含：

```text
visual image
UI tree
layout tree
semantics tree
paint primitive summary
selected state metadata
```

### 23.1 Capture

```bash
viso snapshot capture HomePage
```

### 23.2 Compare

```bash
viso snapshot compare
```

### 23.3 Update

```bash
viso snapshot update HomePage
```

必须显式 update，不因测试失败自动改 golden。

### 23.4 Target

```bash
viso snapshot capture HomePage --target headless
viso snapshot capture HomePage --target ios --device ios-sim-18-pro
```

---

## 24. `viso inspect`

连接 Inspector。

```bash
viso inspect
```

默认寻找当前 project 活跃 Dev Session。

### 24.1 Run and attach

```bash
viso inspect --run
```

等价于启动应用并自动附加 Inspector。

### 24.2 Inspector capability

至少提供：

```text
UI Tree
Component Tree
NodeId / Symbol source mapping
Layout boxes
Transform chain
Dirty reason
Style resolution
State dependencies
Event/focus path
Semantics tree
Paint primitives
Batch groups
GPU resources
resource cache
hot-reload diagnostics
```

### 24.3 Headless query

不打开 GUI：

```bash
viso inspect query '#save_button' --json
```

用于 AI/CI automation。

---

## 25. `viso profile`

采集 Viso framework profile。

```bash
viso profile
```

默认连接当前 Dev Session。

### 25.1 Usage

```bash
viso profile --frames 600
viso profile ios --device iphone-local --seconds 10
```

### 25.2 指标

```text
frame total
input
state flush
style
measure
layout
semantics
paint
batch
GPU upload
GPU time
present

mounted nodes
visible nodes
dirty nodes
reactive evaluations
dynamic fallbacks
draw calls
instances
upload bytes
allocations
glyph cache
atlas occupancy
resource loads
```

### 25.3 Trace output

内部 canonical trace：

```bash
viso profile --output trace.ende
```

可选外部互操作：

```bash
viso profile --chrome trace.json
```

Ende trace schema 由 tooling protocol 定义。

---

## 26. `viso studio`

启动 Viso Studio。

```bash
viso studio
```

可指定：

```bash
viso studio --target android
viso studio --project path/to/app
```

Studio 必须调用与 CLI 相同的：

```text
project resolver
compiler service
build service
device service
inspection service
package service
```

不得重新实现一套 build pipeline。

---

# Part VII — Delivery commands

## 27. `viso package`

构建可分发 artifact。

```bash
viso package macos
viso package windows
viso package linux
viso package ios
viso package android
viso package web-gpu
viso package web-dom
viso package web-hybrid
```

默认使用 `shipping` profile，除非项目另有明确设置。

### 27.1 Package metadata

来自 `Viso.toml`：

```toml
[package]
name = "My App"
bundle_id = "com.example.myapp"
version = "1.0.0"

[package.icons]
source = "assets/icon.png"
```

### 27.2 Signing

签名策略：

```text
auto
required
off
```

示例：

```bash
viso package ios --signing required
```

Secret 不应以明文 CLI echo 或普通 log 输出。

可使用：

```text
OS keychain
CI secret environment
credential provider
```

### 27.3 `--dry-run`

```bash
viso package ios --dry-run
```

输出：

- resolved target；
- bundle metadata；
- signing identity metadata（不含 secret）；
- expected build/package steps；
- output path。

### 27.4 Artifact manifest

每次 package 产生 machine-readable manifest：

```text
dist/<target>/artifact.json
```

记录：

```text
project
version
target
profile
artifact paths
content hashes
build id
Viso toolchain identity
signing status
```

---

## 28. `viso export`

外部生态导出。

```bash
viso export html
viso export solid
```

### 28.1 总规则

Exporter 输入：

```text
Typed HIR
UI IR
Reactive IR
Style/Theme IR
Asset graph
```

Exporter 不重新 parse `.vs` 自己猜语义。

不做双向 round-trip：

```text
.vs -> external source
```

是单向生成。

外部生成代码不是 Viso source of truth。

### 28.2 Capability analysis

导出前必须分类：

```text
Supported
Lowerable with semantic mapping
Requires generated runtime helper
Unsupported
```

任何 Unsupported 必须产生结构化 diagnostic，不允许 silently drop。

---

## 29. `viso export html`

生成标准 HTML/CSS/vanilla JS 产物。

```bash
viso export html --out dist-html
```

目标：

- semantic HTML；
- CSS classes/variables；
- DOM-native accessibility；
- DOM event mapping；
- 对支持的 reactive subset 生成直接 DOM mutation/vanilla JS；
- 不引入 Solid/React/Vue 等 framework dependency。

### 29.1 Static-only mode

```bash
viso export html --static
```

要求输出没有 client JS。

如果源码需要动态 state/event：

```text
EXPORT_HTML_DYNAMIC_REQUIRED
```

直接失败。

### 29.2 Layout lowering

推荐映射：

```text
Row      -> flex row
Column   -> flex column
Grid     -> CSS grid
Stack    -> positioned stacking
Scroll   -> overflow container
Absolute -> positioned element
```

DOM exporter 追求 semantic equivalence，不承诺与 GPU renderer pixel-identical。

### 29.3 Unsupported example

自定义 GPU shader surface 无法直接变成普通 HTML element 时：

```text
EXPORT_HTML_GPU_ONLY_NODE
```

提示用户选择：

```text
viso build web-hybrid
viso build web-gpu
```

或重写为 DOM-capable component。

---

## 30. `viso export solid`

生成 SolidJS source tree。

```bash
viso export solid --out web-solid
```

典型输出：

```text
web-solid/
├── package.json
├── tsconfig.json
├── vite.config.ts
└── src/
    ├── App.tsx
    ├── components/
    ├── styles.css
    └── assets/
```

### 30.1 Reactive mapping

概念映射：

```text
Viso state       -> createSignal / suitable Solid state
Viso computed    -> createMemo
Viso effect      -> createEffect/onCleanup when semantics match
Viso if/match    -> Solid control flow / TS expression
keyed for        -> keyed list semantics
property binding -> JSX property/text binding
```

必须保持 typed semantics；不能通过 source regex 生成。

### 30.2 State example

Viso：

```viso
component Counter {
    state count = 0;

    view {
        Column {
            Text { text: count; }
            Button {
                text: "Add";
                on click { count += 1; }
            }
        }
    }
}
```

导出概念：

```tsx
function Counter() {
    const [count, setCount] = createSignal(0);

    return (
        <div>
            <span>{count()}</span>
            <button onClick={() => setCount(count() + 1)}>
                Add
            </button>
        </div>
    );
}
```

实际生成必须经过 HIR/IR lowering，不依赖文本模板猜测。

### 30.3 SolidJS 不进入 Viso core dependency

`viso-dsl`、`viso-ui`、`viso-runtime`、`viso-render` 不依赖 SolidJS/Node/npm。

只有 exporter/tooling path 可以调用 Node ecosystem 或生成其文件。

### 30.4 Generated source ownership

生成文件头部可标记：

```text
Generated from Viso source.
This directory is an export artifact.
```

但用户可以复制后脱离 Viso 独立维护。

---

# Part VIII — Web runtime 细化

## 31. Web DOM build 与 HTML export 的区别

### `viso build web-dom`

```text
Viso source
  -> Typed HIR
  -> UI/Reactive IR
  -> DOM lowering
  -> Viso DOM runtime artifact
```

支持完整 Viso Web DOM runtime contract，包括 Hot Reload、Viso resource system、typed runtime metadata 等。

### `viso export html`

```text
Viso source
  -> Typed HIR
  -> exporter lowering
  -> ordinary HTML/CSS/vanilla JS artifact
```

目标是外部生态产物，不要求继续运行 Viso development runtime。

---

## 32. Web Hybrid

`web-hybrid` 允许：

```text
DOM subtree
GPU island
DOM subtree
GPU island
```

典型用途：

```text
SaaS dashboard + custom chart renderer
editor chrome + GPU canvas
website + 3D product viewer
game canvas + DOM account/settings UI
```

Hybrid boundary 必须显式进入 UI IR，不能由 exporter 根据控件名称猜测。

---

## 33. Web capability diagnostics

`viso check web-dom` / `viso export ...` 至少识别：

```text
DOM-capable component
GPU-only primitive
unsupported shader dependency
native-only service
filesystem capability mismatch
platform input mismatch
unsupported accessibility mapping
unsupported layout semantic
```

Diagnostic 必须给出：

```text
source span
component/property
requested target
unsupported capability
suggested alternative target or rewrite
```

---

# Part IX — Machine output / Ende JSON

## 34. JSON 是事件流，不是最终对象

长任务需要持续输出进度，所以：

```bash
viso build --json
```

按行输出 JSON event（JSON Lines）。

示例：

```json
{"type":"progress","phase":"compile","message":"Compiling app"}
{"type":"diagnostic","level":"warning","code":"W2104","message":"..."}
{"type":"artifact","kind":"binary","path":"target/.../app"}
{"type":"summary","status":"success","elapsed_ms":842}
```

每行必须是独立合法 JSON。

---

## 35. Common event envelope

逻辑结构：

```text
CliEvent {
    type
    schema
    timestamp
    session_id
    payload
}
```

建议字段：

```json
{
  "type": "diagnostic",
  "schema": "viso.cli.event",
  "timestamp_ms": 1780000000000,
  "session_id": "...",
  "payload": {}
}
```

内部可用 Ende derive 生成。

---

## 36. Event types

一等事件：

```text
progress
diagnostic
artifact
device
test
snapshot
profile
server
log
summary
```

### 36.1 Diagnostic

```text
level
code
message
source
span
notes[]
help[]
related[]
```

### 36.2 Artifact

```text
kind
path
target
profile
size
hash
```

### 36.3 Summary

每个有限命令最终输出一次 summary：

```text
status
elapsed_ms
warning_count
error_count
artifact_count
```

---

## 37. stdout / stderr 规则

Human mode：

```text
stdout -> normal result/progress
stderr -> diagnostics/errors
```

JSON mode：

```text
stdout -> protocol JSON Lines only
stderr -> only unrecoverable pre-protocol launcher failure
```

一旦 JSON protocol 已启动，不允许把普通 debug print 混进 stdout。

---

# Part X — Viso.toml 与 CLI

## 38. 最小配置

```toml
[package]
name = "hello-viso"
bundle_id = "com.example.hello"

[build]
default_target = "host"
```

### 38.1 Web

```toml
[web]
default_target = "web-dom"

[web.serve]
port = 8080
open = true
```

### 38.2 Profiles

```toml
[profile.dev]
opt_level = 0
hot_reload = true
source_maps = true

[profile.release]
opt_level = 3
hot_reload = false

[profile.shipping]
opt_level = "size"
strip = true
hot_reload = false
```

### 38.3 Android

```toml
[target.android]
min_sdk = 26
backend = "vulkan"
```

### 38.4 iOS

```toml
[target.ios]
minimum_os = "17.0"
team_id = "ABCDE12345"
```

Secret 不写进普通 project config。

### 38.5 Export

```toml
[export.html]
out_dir = "dist-html"

[export.solid]
out_dir = "web-solid"
package_manager = "pnpm"
```

Exporter config 不影响 Viso runtime semantics。

---

# Part XI — Internal implementation architecture

## 39. CLI crate

仓库：

```text
tools/cli/
├── Cargo.toml
└── src/
    ├── main.rs
    ├── args.rs
    ├── context.rs
    ├── output/
    │   ├── mod.rs
    │   ├── human.rs
    │   └── json.rs
    ├── command/
    │   ├── mod.rs
    │   ├── new.rs
    │   ├── doctor.rs
    │   ├── config.rs
    │   ├── target.rs
    │   ├── device.rs
    │   ├── run.rs
    │   ├── build.rs
    │   ├── serve.rs
    │   ├── fmt.rs
    │   ├── check.rs
    │   ├── schema.rs
    │   ├── explain.rs
    │   ├── dump.rs
    │   ├── lsp.rs
    │   ├── test.rs
    │   ├── snapshot.rs
    │   ├── inspect.rs
    │   ├── profile.rs
    │   ├── studio.rs
    │   ├── package.rs
    │   ├── export.rs
    │   ├── clean.rs
    │   └── completion.rs
    └── error.rs
```

`command/*.rs` 只做 orchestration。

---

## 40. Shared services

CLI 不应该包含大型 domain implementation。

目标：

```text
Project Resolver
Config Resolver
Compiler Service
Build Service
Target Service
Device Service
Dev Session Service
Web Serve Service
Test Service
Inspector Service
Profiler Service
Package Service
Export Service
```

这些 service 可以位于：

```text
framework crates
platform/tooling modules
tools/* shared libraries
```

具体物理 crate 以真实依赖边界决定，不为了“service”二字拆几十个 crate。

---

## 41. Dependency direction

```text
                       tools/cli
                          |
          +---------------+----------------+
          |               |                |
          v               v                v
       compiler       tooling APIs       packager
          |               |                |
          +--------- framework crates -----+
```

禁止：

```text
viso-runtime -> tools/cli
viso-ui      -> tools/cli
viso-gpu     -> tools/cli
viso-dsl     -> CLI argument parser
```

---

## 42. Args parser

CLI parser 需求：

- subcommands；
- enum values；
- shell completion metadata；
- help generation；
- stable error messages；
- no runtime reflection requirement；
- low startup overhead。

可以使用成熟 Rust CLI parser crate；Viso 不需要为参数解析自研一门 framework。

CLI grammar 是产品合同，不由 parser crate API 决定。

---

## 43. Cancellation

所有长任务必须接受 cancellation token：

```text
build
run
serve
test
profile
package
export
target install
```

Ctrl-C 不能让：

```text
child process
server socket
device install session
temporary package directory
```

永久泄漏。

---

## 44. Project lock

防止同一 project 同一 mutable artifact 被并发破坏。

建议：

```text
target/viso/locks/
```

锁粒度：

```text
build cache lock
package output lock
dev session lock per target/device
```

允许不同 target 的独立 build 并发，只要 artifact/cache 结构安全。

---

## 45. Cache layout

推荐：

```text
target/
└── viso/
    ├── cache/
    │   ├── dsl/
    │   ├── shader/
    │   ├── schema/
    │   └── web/
    ├── dev/
    ├── generated/
    ├── traces/
    └── locks/

dist/
├── macos/
├── ios/
├── android/
├── web/
└── ...
```

不要在 source tree 到处生成临时中间文件。

---

## 46. Build IDs

每个 build/dev session 生成 typed BuildId。

至少绑定：

```text
project identity
resolved target
profile
compiler/toolchain identity
source graph revision
relevant config hash
```

BuildId 用于：

```text
hot reload matching
profile traces
artifact manifests
Studio session
cache validation
```

不要用时间戳单独充当 build identity。

---

## 47. Tool protocol

CLI、Studio、Inspector、Runtime Dev Session 之间的内部协议使用 `viso-ende`。

原则：

```text
Binary for internal transport
JSON for CLI/AI/external tooling
```

不提供 RON protocol。

---

# Part XII — Reliability / Security

## 48. External process execution

任何 SDK/tool command：

- 参数数组执行，不拼未经转义 shell string；
- log 中区分 executable 与 args；
- secret arg 必须 redact；
- exit status 与 stdout/stderr 捕获结构化；
- timeout/cancellation 可控。

---

## 49. Secrets

禁止在：

```text
Viso.toml
artifact.json
--verbose output
JSON diagnostic
profile trace
```

中泄漏 secret。

Credential 来源：

```text
OS keychain
CI environment
credential provider
platform signing store
```

允许项目配置引用 credential 名称，但不存 secret value。

---

## 50. Network downloads

`viso target install` 等下载必须：

- HTTPS；
- hash/signature verification；
- atomic temp download + rename；
- resumable only if integrity preserved；
- cache version metadata；
- support `--offline`；
- 不执行未验证下载内容。

---

## 51. Generated file safety

`new` / `export` / package 生成文件：

- 默认不覆盖用户已有文件；
- `--force` 仅在命令明确支持时生效；
- 写临时文件后 atomic rename；
- 失败时清理 incomplete output；
- export 先生成 staging tree，再原子替换目标目录或报告冲突。

---

# Part XIII — Human UX

## 52. Help 风格

```bash
viso --help
viso run --help
viso export solid --help
```

Help 顺序：

```text
one-line purpose
usage
common examples
arguments
options
target-specific notes
links/next commands
```

不要先输出几十行内部实现解释。

---

## 53. Error message

错误必须：

```text
What failed
Where
Why
What to do next
Diagnostic code
```

例如：

```text
error[ENV_ANDROID_SDK]: Android SDK was not found

Target: android
Expected one of:
  ANDROID_HOME
  configured SDK path in Viso.toml

Try:
  viso target install android
  viso doctor android
```

---

## 54. Progress

TTY human mode 可以使用单行更新/progress bar。

非 TTY：

```text
stable line-oriented output
```

JSON：

```text
progress events
```

不要向 CI 打大量 spinner control characters。

---

## 55. Interactive prompts

只在：

```text
human mode
TTY present
choice truly ambiguous
```

时允许。

`--json` / CI / non-TTY：

必须报结构化 ambiguity，不等待 stdin。

这对 AI automation 很重要。

---

# Part XIV — AI / Vibe Coding contract

## 56. AI-friendly commands

AI Agent 最常用：

```bash
viso check --json
viso schema Button --json
viso explain E3101 --json
viso dump hir src/app.vs --json
viso test ui --json
viso snapshot compare --json
viso inspect query ... --json
viso doctor --json
viso config show --json
```

这些命令必须：

- deterministic；
- non-interactive；
- bounded output 或支持 filter；
- structured source spans；
- stable diagnostic codes；
- stable exit codes。

---

## 57. AI 不应该解析彩色终端文本

IDE/AI 集成统一走：

```text
Ende JSON schema
```

Human formatter 只是该结构化数据的一个 renderer。

内部流程：

```text
Diagnostic object
   ├── Human renderer
   ├── JSON renderer
   ├── LSP adapter
   └── Studio renderer
```

---

## 58. Schema discoverability

AI 在生成 UI 前可以：

```bash
viso schema --search button --json
viso schema Button --json
```

而不是猜：

```text
property name
event payload
slot name
capability
```

这是 Viso AI authoring 的核心能力之一。

---

# Part XV — Testing CLI itself

## 59. Parser golden tests

每个 command 至少测试：

```text
valid minimal usage
all required args
conflicting args
unknown option
help output
JSON mode
non-TTY behavior
```

Help snapshot 进入 golden tests。

---

## 60. Command integration tests

至少：

```text
new -> check
new -> build headless
new -> run headless
fmt --check
schema lookup
invalid .vs diagnostic
web-dom build
web-gpu build
html export
solid export
package dry-run
snapshot compare
```

---

## 61. Fake target/device backends

CLI 测试不能要求 CI 真有手机。

Target/Device service 必须支持 fake backend：

```text
fake ios device
fake android emulator
fake disconnected device
fake signing error
fake SDK missing
```

用于 parser/orchestration/protocol tests。

---

## 62. JSON contract tests

每种 event：

- schema round-trip；
- required fields；
- unknown field tolerance policy；
- ordering contract；
- summary exactly once；
- no human text contamination。

---

## 63. Ctrl-C / cleanup tests

测试：

```text
serve interrupted
run interrupted
package interrupted
target download interrupted
profile interrupted
```

必须验证 child/temp/lock 清理。

---

# Part XVI — Performance contract

## 64. CLI startup

`viso --help` / `viso --version` 不应初始化：

```text
GPU
runtime
compiler database
platform device scanning
network
```

目标：快速启动。

---

## 65. Incremental build

`viso run` 的主要性能指标不是“CLI 自己快”，而是：

```text
change detection latency
DSL incremental compile latency
Rust rebuild latency
hot reload patch latency
asset reload latency
browser/device deploy latency
```

CLI 必须显示这些阶段 timing，Profile/verbose mode 可观测。

---

## 66. No unnecessary serialization

CLI orchestration 内部如果是同进程函数调用，不因为有 Ende 就强迫 Encode/Decode。

只有：

```text
process boundary
socket boundary
persistent cache
external JSON protocol
```

才编码。

---

# Part XVII — Command grammar

## 67. Informal grammar

```text
viso
  [global-options]
  <command>
  [command-options]
  [arguments]
  [-- app-arguments]
```

Target-taking commands：

```text
viso run [target]
viso build [target]
viso serve [web-target]
viso package [target]
viso test [domain]
```

Export：

```text
viso export html
viso export solid
```

Environment：

```text
viso target <list|info|install> [target]
viso device <list|info|boot|logs> [device]
```

---

## 68. Aliases

核心命令不设计大量 alias。

允许：

```text
-h -> --help
-V -> --version
-v -> --verbose
-q -> --quiet
```

不鼓励：

```text
b -> build
r -> run
p -> package
```

因为它们降低脚本可读性和文档一致性。

---

# Part XVIII — Implementation order for Viso 1.0

## 69. P0 — CLI foundation

实现：

```text
argument parser
global options
project discovery
Viso.toml loader
config precedence
human output
Ende JSON output
stable errors/exit codes
completion metadata
```

验收：

```bash
viso --help
viso --version
viso config show
```

---

## 70. P1 — Host development loop

实现：

```text
new
doctor host
check
fmt
build host
run host
headless
test
clean
```

这一步必须已经形成可日常开发的最小闭环。

---

## 71. P2 — Compiler tooling

实现：

```text
schema
explain
dump
lsp
snapshot headless
inspect query
```

确保 AI/IDE 在 Viso 1.0 开发阶段就有结构化接口。

---

## 72. P3 — Cross-platform environment

实现：

```text
target list/info/install
device list/info/boot/logs
ios run/build
android run/build
```

Device service 与 Platform backend 分离。

---

## 73. P4 — Web

实现：

```text
web-gpu build/run/serve
web-dom build/run/serve
web-hybrid build/run/serve
web capability diagnostics
```

---

## 74. P5 — Delivery/export

实现：

```text
package
artifact manifest
html export
solid export
signing integration
```

---

## 75. P6 — Full observability

实现：

```text
inspect GUI attach
profile
trace output
studio launch/integration
device profile
```

---

# Part XIX — Definition of Done

## 76. CLI 1.0 完成标准

### Project

- `viso new` 生成最小可运行项目；
- project discovery 稳定；
- config precedence 有测试；
- `viso doctor` 可以解释环境缺口。

### Develop

- `viso check/build/run` 覆盖 host；
- `run` 内置 watcher/hot reload；
- Ctrl-C 正确清理；
- headless 可用于 CI。

### Cross-platform

- target/toolchain 统一 grammar；
- device discovery 统一；
- iOS/Android 不要求用户记底层 SDK CLI 语法。

### Web

- `web-gpu`；
- `web-dom`；
- `web-hybrid`；
- `viso serve`；
- capability diagnostics。

### Language

- `fmt`；
- `check`；
- `schema`；
- `explain`；
- `dump`；
- `lsp`。

### Test / Debug

- `test`；
- `snapshot`；
- `inspect`；
- `profile`；
- `studio`。

### Delivery

- `package`；
- artifact manifest；
- `export html`；
- `export solid`。

### Automation

- 所有核心命令支持 `--json`；
- exit codes 稳定；
- non-TTY 不进入交互 prompt；
- Studio/IDE/AI 使用共享 services/protocol；
- CLI 不复制 compiler/build/packager 核心实现。

---

# Appendix A — 常用命令速查

```bash
# Create
viso new my_app
cd my_app

# Develop
viso check
viso run

# Mobile
viso target install ios
viso device list ios
viso run ios --device <id>

viso target install android
viso device list android
viso run android --device <id>

# Web
viso serve web-dom --open
viso serve web-gpu --open
viso serve web-hybrid --open

# Language / AI
viso schema Button
viso schema Button --json
viso check --json
viso dump hir src/app.vs --json

# Test / inspect
viso test ui --headless
viso snapshot compare
viso inspect
viso profile --frames 600

# Delivery
viso package macos
viso package android
viso export html --out dist-html
viso export solid --out web-solid
```

---

# Appendix B — 推荐开发闭环

普通 App：

```text
viso new
   ↓
viso run
   ↓
edit Rust/.vs/assets
   ↓
hot reload / incremental rebuild
   ↓
viso check
   ↓
viso test
   ↓
viso package
```

Web 产品：

```text
viso new --template web
   ↓
viso serve web-dom
   ↓
viso check web-dom
   ↓
viso test web
   ↓
viso package web-dom
```

外部前端交付：

```text
Viso source
   ↓
viso export solid
   ↓
standalone SolidJS project
```

---

# Appendix C — CLI 反模式

禁止：

```text
一个平台一套完全不同的命令语法
CLI 内复制 compiler type checker
Studio 内复制 build graph
--json 只是包人类字符串
CI 需要回答交互 prompt
run 与 watch 各有一套 watcher
build/package/export 语义混在一起
SolidJS 成为 Viso core dependency
HTML exporter silent drop unsupported node
secret 出现在 --verbose log
失败后留下半个 dist tree
```

---

# 结论

Viso CLI 的核心不是“命令多”，而是把 Viso 的完整开发生命周期收敛到一个一致、可自动化、可观测的入口：

```text
source
  ↓
check
  ↓
build
  ↓
run / serve
  ↓
inspect / test / profile
  ↓
package
  ↓
optional export
```

对人类：命令简单、一致。  
对 CI：exit code 和 JSON 稳定。  
对 Studio/IDE：复用同一 service。  
对 AI/Vibe Coding：Schema、Diagnostics、HIR/IR、Snapshot、Inspector 都可结构化查询。  
对架构：CLI 永远是 facade，不反向污染 Runtime/UI/GPU/DSL。
