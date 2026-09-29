# Viso CLI 设计规范

> 文档状态：Viso 1.0 Draft / CLI Specification  
> 命令名：`viso`  
> 配置文件：`Viso.toml`  
> 目标读者：Viso CLI/Tooling 工程师、Platform 工程师、Compiler 工程师、Studio 工程师、AI Coding Agent  
> 设计目标：让项目从创建、检查、构建、运行、调试、测试到打包与 Web 导出都通过一个稳定入口完成。

## 与 ADR / Architecture 的关系

本文档是 `Viso_Architecture.md` 第 54 节（CLI/工具链合同锚点）的详细命令与协议规范，位于 Architecture-document 权威层级。CLI 是位于 facade 之上的 thin orchestration 层，不拥有第二套 compiler/renderer/packager/inspector；它复用共享 services，并遵循以下相关 ADR：

- [ADR-0016](./docs/adr/0016-release-aot-package.md) — Release AOT package（`build`/`package`/`export` 产物语义、compiler-absent load path 的边界依据）。
- 开发期 dev-loop（`viso run` 内置 watcher、transactional patch、last-good）的完整协议见 [`Viso_Hot_Reload.md`](./Viso_Hot_Reload.md)（ADR-0015）；CLI 只定义入口 flag 与 Ctrl-C 行为，协议细节 delegate 给该文档。

本文档若引入新的工具链决策（例如新增 public 命令类别、改变依赖方向或产物语义），须回写为新的/更新的 ADR。

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

Viso 1.0 的 public CLI 以开发流程为中心，固定为：

```text
PROJECT
    viso new
    viso doctor
    viso config

MOBILE DEV ENVIRONMENT
    viso android list
    viso android use
    viso android doctor
    viso android emulator list|create|delete|start|stop
    viso android adb ...

    viso ios list
    viso ios use
    viso ios doctor
    viso ios simulator list|create|delete|start|stop

DEVELOP
    viso run
    viso run ios
    viso run android
    viso run web-gpu|web-dom|web-hybrid
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

Desktop host 不作为 public positional target 暴露。开发者在 macOS、Windows、Linux 上直接执行：

```bash
viso run
```

CLI 根据当前 host 唯一确定 desktop backend。禁止要求用户写：

```text
viso run macos
viso run windows
viso run linux
viso run host
```

移动开发只面向 Simulator / Emulator。Viso 1.0 的开发命令不包含 physical-device deployment、真机 attach、provisioning 或 device signing 流程；这些能力如未来加入，必须作为独立设计，不得改变当前开发命令的简单语义。

Android/iOS 平台命令只管理开发环境，不改变 Viso UI/runtime abstraction。Android 的 `adb` 作为明确的底层 escape hatch 保留；Viso 负责定位正确的 SDK、ADB executable 和 emulator serial，然后转发参数，不重新实现 ADB。

---

## 2. 三种产物语义必须区分

### 2.1 `build`

```bash
viso build
viso build ios
viso build android
viso build web-gpu
viso build web-dom
viso build web-hybrid
```

产生 **Viso application artifact**。

无 positional target 时构建当前 desktop host。Desktop 不使用 `macos/windows/linux` positional target；移动和 Web 因为不是当前 host process，必须显式指定逻辑 target。

Artifact 仍由 Viso runtime、Viso generated runtime 或对应 backend 负责执行。

### 2.2 `package`

```bash
viso package
viso package ios
viso package android
viso package web-gpu
viso package web-dom
viso package web-hybrid
```

产生 **可分发产物**。

```text
viso package              current desktop host distributable
viso package ios          iOS distribution artifact
viso package android      APK/AAB
viso package web-*        deployment directory/archive
```

`package` 可以隐式执行 release/shipping build，但必须复用同一 build graph。Signing、provisioning、store metadata 属于 delivery，不属于 `viso run ios/android` 的 simulator/emulator 开发路径。

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

## 3. Runtime target 模型

Viso public development target 只暴露真正需要用户选择的运行环境：

```text
desktop host   implicit: `viso run`
ios            simulator only
android        emulator only
web-gpu
web-dom
web-hybrid
headless        testing/tooling only
```

内部仍然会把 desktop host resolve 为 macOS/Windows/Linux，并选择 Metal/D3D12/Vulkan backend，但这是 tooling/platform implementation detail，不进入普通 `run` grammar。

### 3.1 Desktop host

```bash
viso run
```

永远表示：在当前开发机原生运行当前项目。

```text
macOS   -> native macOS + selected Viso backend
Windows -> native Windows + selected Viso backend
Linux   -> native Linux + selected Viso backend
```

项目配置不得把 `viso run` 的无参数语义改成 iOS、Android 或 Web；否则同一命令在不同仓库中会失去可预测性。项目可以配置 Web 默认 serve target，但不能重定义 `viso run` 的 desktop-host 含义。

### 3.2 Mobile development target

```bash
viso run ios
viso run android
```

两者只表示 simulator/emulator development session：

```text
ios     -> Apple Simulator
android -> Android Emulator
```

没有 `--simulator` / `--emulator` flag，因为 target 已经唯一决定设备类型。

指定本地虚拟设备：

```bash
viso run ios --device iphone-local
viso run android --device pixel-local
```

`--device` 在 Viso 1.0 development CLI 中只接受 Viso 已知的 simulator/emulator profile ID，不接受 physical-device identifier。

### 3.3 Web target

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
    DOM shell + WebGPU islands
```

### 3.4 `headless`

`headless` 是 testing/tooling target，不是 `run` / `serve` / `package` 的 positional target（`viso run headless` 是 usage error，exit 2）。它用于：

```bash
viso check headless                       # 以 headless 能力集做静态检查
viso build headless                       # 仅供 CI/tooling 产出 headless artifact
viso test ui                              # test/snapshot 默认 target 即 headless
viso snapshot capture HomePage --target headless
```

### 3.5 Target 选择语法

- 以 target 为操作对象的命令（`run`、`build`、`serve`、`package`、`check`、`doctor`、`profile`）使用**可选 positional target**；省略时为当前 desktop host（`serve` 省略时见 §15）。
- positional 已被其他对象占用的命令（`test`、`snapshot`、`inspect`、`studio`）使用 `--target <t>`。
- `--target` 不是 global option；对接受 positional target 的命令写 `--target` 是 usage error（exit 2），例如 `viso run --target ios`。
- 任何配置层（`Viso.toml`、`VISO_*` 环境变量）都不能改变“省略 positional = desktop host”的含义（§3.1）。

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

从高到低（括号内为 `viso config show` / `config get` 报告的 provenance 名）：

```text
CLI flags                              (flag)
    ↓
VISO_* environment variables           (env)
    ↓
Viso.toml [profile.<name>] override    (profile)
    ↓
Viso.toml project defaults             (manifest)
    ↓
framework defaults                     (default)
```

规则：

- 每个配置 key 独立按上表逐层解析，取第一个有值的层。
- 支持的环境变量：`VISO_PROFILE`、`VISO_OPT_LEVEL`、`VISO_SOURCE_MAPS`、`VISO_STRIP`。环境变量值无法解析时报 config diagnostic（C0010，exit 1），不得静默跳过该层。
- target 只来自 positional（§3.5），不参与本优先级链。
- `.viso/local.toml`（§12.5）只保存本机 simulator/emulator 选择，不参与 build 配置解析，不得提交到版本库。
- 最终结果通过 `viso config show` 查看（§10）。

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

`--profile` 只接受 `dev|release|shipping`（§38.2）。`--target` 不在此列（§3.5）。

### 6.1 `--json`

不是“把最终人类文本包成 JSON 字符串”，而是切换为 §34–§37 定义的稳定 **Ende JSON event stream**。`--json` 下 `--color` 被忽略，stdout 不含 ANSI 序列。`viso lsp`、`viso studio`、`viso completion` 不接受 `--json`（usage error，exit 2）。

### 6.2 `--quiet`

human 模式只输出：

- fatal diagnostics；
- requested artifact paths；
- final summary。

与 `--json` 同时使用时省略 `progress` 与 `log` 事件，其余事件不变。`--quiet` 与 `--verbose` 同时出现是 usage error。

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

失败类别 → exit code 映射（多个失败并存时取首个导致命令终止的失败；只有 diagnostics 时取 1）：

```text
source/DSL/type/capability diagnostics (E*)          1
config diagnostics C0001, C0003–C0010, C0012          1
test assertion / snapshot mismatch                    1
unknown command/flag, 冲突 flag, 错误 positional      2
C0002 manifest unreadable, C0011 target unavailable   3
SDK/toolchain/simulator/emulator 缺失, lock 被占用     3
Rust compile / link / shader compile 失败             4
app crash, device 连接失败, test runner 崩溃           5
signing / archive / exporter 失败                      6
service protocol 违约, CLI panic                       7
Ctrl-C / SIGINT / SIGTERM                             130
```

`--json` 模式下 `summary.payload.exit_code` 必须等于进程 exit code（§36.3）。AI/CI 不得依赖解析英文文本判断成功失败。

`Viso_DSL_1.0.md` §137 的 0–3 与本表一致；其 `4 = Runtime/Backend` 对应本表 4/5，CLI exit code 以本节为准。

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
viso test ui        # 默认 --target headless
```

必须成功。

### 8.5 JSON / exit codes

`--json`：`progress`、一条 `result`（`payload: {path, template, files[]}`）、`summary`。目标目录非空且未给 `--force-empty-dir`：exit 2；模板写出失败：exit 7。

---

## 9. `viso doctor`

检查当前开发机上的 Viso 基础开发环境。

```bash
viso doctor
```

检查：

```text
Rust / rustup / cargo
Viso CLI/framework compatibility
Viso.toml
host compiler/linker
host GPU backend capability
WASM target / Web build tools
filesystem permissions
required external tools
Android/iOS dev environment summary（只报告，不自动安装）
```

### 9.1 Platform-specific doctor

移动开发环境有自己的 canonical 命令：

```bash
viso android doctor
viso ios doctor
```

为了脚本兼容，`viso doctor android` / `viso doctor ios` 可以作为只读 alias，但文档和 AI 示例统一使用 platform command。

Web 可以：

```bash
viso doctor web-gpu
```

### 9.2 输出示例

```text
Viso Doctor

[ok] Rust toolchain
[ok] Viso project
[ok] Host GPU backend
[ok] WebAssembly target
[warn] Android development environment is not selected
[warn] iOS Simulator runtime is unavailable on this host

Suggested actions:
  viso android list
  viso android use 36
  viso ios list
```

开发期 doctor 不检查 iOS signing identity、provisioning profile 或 physical device。Signing 只在 `viso package ios` / delivery validation 中检查。

### 9.3 `doctor` 不偷偷做大规模修改

默认只读。

安全、确定的小修复可以显式：

```bash
viso doctor --fix
```

Android/iOS SDK、runtime、emulator 等下载和安装必须通过明确的平台命令触发：

```bash
viso android use <api>
viso ios use <runtime>
```

### 9.4 JSON

```bash
viso doctor --json
```

每个 check 输出一个 `doctor_check` 事件（envelope 见 §35，以下只列 `type` 与 `payload`）：

```json
{"type":"doctor_check","payload":{"name":"rust","status":"ok"}}
{"type":"doctor_check","payload":{"name":"android-sdk","status":"missing","code":"ENV_ANDROID_SDK","suggestion":"viso android use 36"}}
```

`status ∈ ok|warn|missing|error`。任一 check 为 `missing|error` 时 exit 3，只有 `warn` 时 exit 0。

---

## 10. `viso config`

用于查看和验证最终配置，不直接替代文本编辑器。

### 10.1 Show

```bash
viso config show
```

输出合并后的配置，每个 key 附 provenance（`flag|env|profile|manifest|default`，§5）。

### 10.2 Get

```bash
viso config get package.name
viso config get profile.release.opt_level
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

### 10.6 JSON / exit codes

`show`/`get`/`path` 各输出一条 `result`（`show`：`{key: {value, origin}}` 映射；`get`：`{key, value, origin}`；`path`：`{path}`）；`validate` 每个问题一条 `diagnostic`（C0001–C0012）。Exit code 按 §7 的 config 诊断映射；`validate` 无 error 时 0。

---

# Part III — Mobile development environment

## 11. `viso android`

`viso android` 管理 Viso 的 Android Emulator 开发环境。它不是 Android 通用 SDK manager 的复制品，而是为 Viso 选择并验证一套可工作的 SDK / platform-tools / build-tools / NDK / emulator / system-image 组合。

### 11.1 `viso android list`

```bash
viso android list
```

列出 Viso 当前支持的 Android API、是否已安装、当前默认版本和推荐 system image：

```text
API   ANDROID   STATUS       SYSTEM IMAGE            DEFAULT
36    16        installed    google_apis/arm64-v8a   *
35    15        available    google_apis/arm64-v8a
34    14        available    google_apis/arm64-v8a
```

在 x86_64 host 上 system image 可自动选择 x86_64；在 ARM64 host 上优先 arm64-v8a。开发者不需要为了 emulator 日常运行手写 ABI。

`--json` 必须给出 machine-readable version/component metadata，供 Studio 和 AI agent 选择版本。

### 11.2 `viso android use <api>`

```bash
viso android use 36
```

语义：**确保 API 36 的 Viso Android development profile 已安装，并把它设为本机默认开发版本。**

如果不存在于 Viso 支持矩阵：

```text
error[ANDROID_API_UNSUPPORTED]
```

如果支持但未安装：

```text
resolve Viso tested component set
    ↓
download/locate command-line tools
    ↓
platform-tools / adb
    ↓
platform android-36
    ↓
build-tools
    ↓
Viso tested NDK
    ↓
emulator
    ↓
default compatible system image
    ↓
license/integrity validation
    ↓
mark API 36 as machine default
```

如果已经安装，必须快速切换，不重复下载。

本地选择属于 machine state，默认写入：

```text
~/.viso/android/
```

不得为了 `use` 修改 Git tracked `Viso.toml`。项目中的 `min_sdk`、package ABI 等仍由项目配置控制。

### 11.3 `viso android doctor`

```bash
viso android doctor
```

检查：

```text
SDK root
command-line tools
selected API/platform
build-tools
platform-tools / adb
NDK
emulator binary
system image
host virtualization capability
licenses
Viso component compatibility
emulator profiles
```

默认只读；不会偷偷下载几十 GB SDK。

### 11.4 Android emulator profiles

```bash
viso android emulator list
viso android emulator create pixel-local
viso android emulator create pixel35 --api 35
viso android emulator start pixel-local
viso android emulator stop pixel-local
viso android emulator delete pixel35
```

`create` 未指定 `--api` 时使用 `viso android use` 当前选择的版本。设备 model/viewport 由 `--preset phone|tablet`（默认 `phone`）选择；ABI 与 system image family 由 Viso 根据 host 和 API 兼容矩阵推导。GPU backend 不可选（Android 固定 Vulkan，ADR-0029）。

示例：

```text
NAME           API   ABI      PROFILE   STATUS
pixel-local    36    arm64    phone     booted
pixel-tablet   36    arm64    tablet    stopped
```

### 11.5 `viso android adb ...`

Viso 保留 ADB escape hatch：

```bash
viso android adb devices
viso android adb --device pixel-local shell getprop
viso android adb --device pixel-local logcat
viso android adb -- shell settings list global
```

实现规则：

```text
resolve selected Android profile
    ↓
resolve verified adb executable
    ↓
optional Viso emulator ID -> adb serial
    ↓
exec adb with remaining args unchanged
```

Viso 不解析/重实现全部 ADB 子命令。`--` 之后的参数必须原样数组转发，避免 shell quoting 差异。

### 11.6 Android logs shortcut

普通开发日志由 `viso run android` 自己汇总；需要底层日志时使用：

```bash
viso android adb --device pixel-local logcat
```

不再额外维护一套 `viso device logs` grammar。

### 11.7 JSON / exit codes

- `list`、`emulator list`：一条 `result`（API/component 或 profile 列表）。
- `use`：`progress`（下载/安装阶段）+ `result{api, installed, default}`。
- `doctor`：同 §9.4 的 `doctor_check`。
- `emulator create|start|stop|delete`：`device` 事件 + `summary`。
- `adb`：不接受 `--json`，stdout/stderr 与 exit code 原样来自 adb；Viso 自身解析 SDK 失败时 exit 3。

Exit code：不在支持矩阵的 API（`ANDROID_API_UNSUPPORTED`）或未知 profile 2；SDK/emulator/host virtualization 不可用 3；emulator 启动失败 5。

---

## 12. `viso ios`

`viso ios` 只管理 **iOS Simulator** 开发环境。Viso 1.0 不在开发 CLI 中管理 physical device、provisioning、开发证书或真机 attach。

该命令只在 macOS host 上可用；其他 host 必须返回明确的 unsupported-host diagnostic，而不是展示不可执行命令。

### 12.1 `viso ios list`

```bash
viso ios list
```

列出当前 Xcode 可使用/可安装的 Simulator runtime：

```text
RUNTIME       STATUS       DEFAULT
iOS 26.0      installed    *
iOS 25.4      installed
iOS 25.0      available
```

### 12.2 `viso ios use <runtime>`

```bash
viso ios use 26.0
```

语义：确保该 Simulator runtime 可用，并把它设为 Viso 本机默认 iOS development runtime。

下载/安装必须通过 Apple/Xcode 支持的机制；Viso 只负责编排、进度、完整性/可用性验证和本机默认选择，不绕过 Apple toolchain contract。

### 12.3 `viso ios doctor`

```bash
viso ios doctor
```

检查：

```text
macOS host
Xcode installation
Command Line Tools
simctl
selected Simulator runtime
available simulator profiles
Rust/iOS simulator target
Viso Metal backend capability
```

开发 doctor 明确不检查 signing identity/provisioning profile。

### 12.4 iOS simulator profiles

```bash
viso ios simulator list
viso ios simulator create iphone-local
viso ios simulator create ipad-local --preset tablet
viso ios simulator start iphone-local
viso ios simulator stop iphone-local
viso ios simulator delete iphone-local
```

普通运行直接：

```bash
viso run ios --device iphone-local
```

如果 profile 存在但未 boot，`viso run` 自动 boot；用户不需要先手动执行 `simulator start`。

### 12.5 本机状态

Android/iOS 选择和 virtual-device profile 都属于 developer-machine state：

```text
~/.viso/android/
~/.viso/ios/
```

项目可有 `.viso/local.toml` 覆盖本机默认 device profile（`--device` 省略时的选择），它必须被 VCS ignore，且不参与 §5 的 build 配置解析。SDK path、emulator serial、Simulator UUID 不进入 `Viso.toml`。

### 12.6 JSON / exit codes

JSON 合同与 §11.7 相同（`list`/`use`/`doctor`/`simulator ...`）。非 macOS host 上所有 `viso ios` 子命令 exit 3。


---

# Part IV — Develop commands

## 13. `viso run`

这是普通开发的主命令。

### 13.1 Desktop host

```bash
viso run
```

无参数时永远在当前 desktop host 运行。CLI 自动 resolve：

```text
project
host OS/backend
Dev profile
incremental build
launch process
Dev Runtime transport
.vs / shader / asset / Rust watcher
structured diagnostics/logs
```

`viso run host|macos|windows|linux|headless` 与 `viso run --target ...` 均为 usage error（§1、§3.5）。

### 13.2 iOS / Android emulator development

```bash
viso run ios
viso run android
```

目标设备语义与 `--device` 取值见 §3.2。

如果没有 `--device`：

1. 存在本机默认 profile（`.viso/local.toml` 或 `~/.viso/`，§12.5）：使用它；
2. 只有一个可用 profile：使用它；
3. 没有 profile但当前是交互 TTY：给出创建建议或确认后创建标准 profile；
4. 有多个且无默认：交互选择；
5. `--json` / non-TTY：绝不 prompt，返回稳定 ambiguity/missing-device diagnostic 和 suggested command。

如果所选 profile 未 boot：

```text
auto boot
    ↓
wait ready
    ↓
build
    ↓
install
    ↓
launch
    ↓
connect Dev Runtime
```

### 13.3 Web

```bash
viso run web-gpu
viso run web-dom
viso run web-hybrid
```

默认启动 Viso dev server 和浏览器开发 session。可选：

```bash
viso run web-gpu --browser chrome
viso run web-dom --browser safari
```

浏览器选择不是 mobile `--device` 的复用概念。

### 13.4 App arguments

`--` 后传给应用：

```bash
viso run -- --open demo.vs --safe-mode
```

移动/ Web 同样遵循 `--` boundary。

### 13.5 Dev Runtime / Hot Reload

`viso run` 永远构建 **Dev artifact**。Dev artifact 默认包含 Viso Dev Runtime，并监听：

```text
.vs
shader source
assets / fonts
Viso.toml relevant dev fields
Rust source
```

按 source domain 分流：

```text
.vs            -> typed semantic patch
shader         -> validated pipeline patch
asset/font     -> resource revision patch
game system    -> tick-boundary system patch
Rust           -> incremental rebuild + stateful warm restart
```

任何 candidate 失败：

```text
keep last-good running app
emit diagnostics
never commit half-valid patch
```

完整协议、Dev Daemon、PatchBundle、DevSnapshot、原子边界和 Warm Restart contract 见仓库根目录 `Viso_Hot_Reload.md`。

### 13.6 Hot Reload 只属于 Dev build

这是 build-time hard contract，不是 release 中的 runtime toggle。

```text
Dev artifact:
    Dev Runtime compiled in
    hot-reload patch receiver compiled in
    DevSnapshot endpoint available
    source/schema hot-reload metadata retained

Release / Shipping artifact:
    no Dev Runtime
    no hot-reload transport listener
    no PatchBundle decoder/apply path
    no DevSnapshot endpoint
    hot-reload-only metadata stripped
```

因此：

- `viso run` 默认支持 Hot Reload；
- `viso run --no-hot-reload` 只是在 Dev artifact 中关闭当前 session 的自动 patch，用于排查开发问题；
- `viso build --profile release`、`viso package` 不能通过配置重新开启 Hot Reload；
- Release steady-state runtime 不允许为了“可能热更”保留每帧分支、listener 或 symbol lookup。

### 13.7 Options

```text
--device <id>           # ios/android simulator/emulator only
--browser <name>        # web only
--no-hot-reload         # Dev session diagnostic switch
--inspect
--profile-frame
--open                  # web convenience
--env KEY=VALUE
--cwd <path>
```

`viso run` 不提供 `--release`。Release/Shipping 验证使用 `viso build --profile release`、`viso package`、benchmark/profile 工具，而不是把开发主命令变成 delivery frontend。

### 13.8 Ctrl-C

在 §43 通用取消规则之上，`run` 额外要求：

1. stop watcher；
2. request child/emulator app graceful shutdown or detach dev session；
3. stop dev transport；
4. keep simulator/emulator boot state by default，避免下次开发重复冷启动；
5. second Ctrl-C force kill owned child processes。

### 13.9 JSON / exit codes

`--json` 事件：`progress`、`diagnostic`、`artifact`（dev artifact）、`device`、`dev`（每个 candidate revision 一条，§36）、`log`、`summary`。Dev session 中的 candidate 失败只产生 `diagnostic` + `dev{outcome:"rejected"}`，不结束命令。

Exit code：首次 build 失败按 §7（1/3/4）；app 正常退出 0；app crash 或非零退出 5；Ctrl-C 结束 session 130。

---

## 14. `viso build`

只构建，不启动。

```bash
viso build
viso build ios
viso build android
viso build web-dom
```

### 14.1 Profiles

Profile 集合固定为 `dev|release|shipping`（语义与 Dev Runtime 关系见 §38.2），不支持用户自定义 profile；未知名称报 C0009（exit 1）。`viso build` 默认 `dev`，`viso package` 默认 `shipping`（§27）。

```bash
viso build web-gpu --profile shipping
```

`viso build --release` 是 `--profile release` 的便利别名，二者同时出现且不一致时为 usage error；它不适用于 `viso run`。

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

`--json` 事件：`progress`、`diagnostic`、每个产物一条 `artifact`、`summary`。Exit code：1（source/config diagnostics）、3（target/SDK unavailable、lock 被占用）、4（compile/link）。

---

## 15. `viso serve`

只服务 Web target。

```bash
viso serve             # 使用 [web] default_target；未配置时为 usage error
viso serve web-dom
viso serve web-gpu
viso serve web-hybrid
```

非 Web positional target 是 usage error（exit 2）。

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

默认只监听 loopback；非 loopback `--host` 必须同时给 `--lan`，否则 usage error（Viso_Hot_Reload.md §46）。

`serve` 与 `viso run web-*` 共用 Web Serve Service 与 Dev Session；区别是 `serve` 不启动浏览器开发 session（`--open` 只打开 URL），用于 LAN/HTTPS/外部浏览器访问。

### 15.2 Port selection

若默认端口被占用：

- human mode：自动选择相邻空闲端口并提示；
- `--json`：同样自动选择，并输出一条 `server` event（`url`、`host`、`port`）；
- 显式 `--port` 被占用：报错（exit 3），不静默改端口。

`--json` 事件：`progress`、`diagnostic`、`artifact`、`server`、`dev`、`log`、`summary`。Exit code 同 §13.9（无 app crash 分支）。

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

Formatter 基于 Lossless CST/AST，不使用正则批量重写。实现复用 `viso-lsp` 同一 formatter（ADR-0018）。

### 16.4 JSON / exit codes

`--json`：每个需要改写（`--check` 时为未格式化）的文件一条 `result`（`payload: {file, changed}`），无法解析的文件输出 `diagnostic`，最后 `summary`。Exit code：`--check` 发现未格式化文件或存在 parse diagnostic 时 1。

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

普通开发用 `viso run`。编辑器/CI 需要连续静态检查时可用 `viso check --watch`；每轮输出完整 diagnostic 集合后接一条 `progress{phase:"idle"}`，只有退出时输出 `summary`。

### 17.4 JSON / exit codes

`--json`：`progress`、每条诊断一条 `diagnostic`（DSL §138 对象）、`summary`（包已加载时另带 `package`、`file_count`）。Exit code：存在 `severity:"error"` 的诊断时 1；target unavailable 3；Rust check 失败 4。warning 不改变 exit code。

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
  text        String             invalidates: MEASURE|LAYOUT|PAINT|SEMANTICS
  disabled    Bool = false       invalidates: STYLE|HIT_TEST|PAINT|SEMANTICS
  icon        Option<Image>      invalidates: MEASURE|LAYOUT|PAINT

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

符号按完整路径或其任意 `::` 后缀匹配（`Button`、`text::upper`、`viso::time::Stopwatch`）；后缀匹配多个符号时报告全部候选并 exit 1。`.member` 收窄到一个 Property、Event 或方法。可查询内置 Widget 与 Native Registry 中的 Library、函数和 Handle 类型。

`--search TERM` 不区分大小写地列出路径包含 `TERM` 的符号与成员，每行一个路径及其类型或签名；无匹配时 exit 0。`--json` 输出一条 `result`，`payload: {query, matches: [{path, kind, detail}]}`。

### 18.2 AI/tool use

```bash
viso schema Button --json
```

输出一条 `result` event，`payload` 为 `Viso_DSL_1.0.md` §139 定义的 schema object；CLI 不定义第二套 schema JSON。Invalidation 名称只用 dirty class 规范名（`STRUCTURE STYLE MEASURE LAYOUT TRANSFORM PAINT HIT_TEST SEMANTICS`）。符号不存在时输出 `diagnostic` 并 exit 1。

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

`--json`：一条 `result`（`payload: {code, title, explanation, suggested_commands[]}`）+ `summary`。未知 code：exit 1。

---

## 20. `viso dump`

用于 compiler/runtime advanced diagnostics。

```bash
viso dump ast src/app.vs
viso dump hir src/app.vs
viso dump ui-ir src/app.vs
viso dump reactive-ir src/app.vs
viso dump behavior-ir src/app.vs
viso dump shader-ir RoundedRect
viso dump system-ir PlayerController
viso dump module-graph
```

支持：

```text
--out <path>        # 写文件而不是 stdout
--symbol <path>     # 只输出指定 symbol
```

`--json`：一条 `result`（`payload: {kind, input, ir}`，`ir` 的形状由对应 IR 的 schema version 决定）+ `summary`；输入有 source error 时只输出 `diagnostic`，exit 1。

`dump` 不属于普通应用 authoring API，但必须稳定到足以支持 compiler tests、Studio 和 AI debugging。

---

## 21. `viso lsp`

启动 Viso Language Server。

默认：

```bash
viso lsp --stdio
```

`viso lsp` 只定位并 exec `viso-lsp` binary（ADR-0018：同步 stdio `Content-Length` JSON-RPC loop），不在 CLI 进程内实现 language server。stdout 专属 JSON-RPC，任何日志只写 stderr；`--json` 不适用（§6.1）；没有 `summary` event。

能力范围：

```text
ADR-0018 最小集（已实现）:
    publishDiagnostics, goto definition, find references, rename, formatting

1.0 目标（ADR-0018 明确 deferred，需后续 ADR/修订）:
    hover, completion, semantic tokens, code actions,
    schema lookup, source-to-generated mapping
```

Exit code：客户端 `shutdown`+`exit` 后 0；未收到 `shutdown` 即 `exit` 或 transport 损坏时 1（LSP 规范）。

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

Rust unit/integration tests 仍可由 Cargo 执行；`viso test` 负责协调 Viso headless/UI/device/browser 测试。

positional 是测试域；执行环境用 `--target <t>`（§3.5），默认 `headless`（`web` 域默认 `web-dom`）。`ios`/`android` 需要 `--device` 或本机默认 profile。

### 22.2 Headless UI

```bash
viso test ui                      # 等价 --target headless
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

```bash
viso test game movement --tape runs/jump.tape      # 回放 Tape，对 @probe 断言
viso game record movement -o runs/jump.tape        # 运行中录制 Input Tape
viso game peek <session> Player.score              # 读取运行中 System State
```

输出包含每 Tick `@probe` Trace、最终 Snapshot Hash、Entity Snapshot，以及可选的 Headless 帧截图 Sheet（`--sheet <png>`）。同一 Build + Tape 在所选 determinism 档位下 Snapshot Hash 逐字节一致（DSL §110.5）。

### 22.4 Test filters

```text
--target <t>
--device <id>
--filter <pattern>
--exact
--fail-fast
--nocapture
--update-snapshots      # 显式写回 golden，等价于随后执行 snapshot update
```

并行度使用 global `--jobs`。

### 22.5 JSON / exit codes

`--json`：`progress`、`diagnostic`、每个用例一条 `test`（`payload: {name, domain, status: pass|fail|skip, duration_ms, message?}`）、`summary`（含 pass/fail/skip 计数）。Exit code：有失败用例 1；source diagnostics 1；target/device 不可用 3；runner 崩溃 5。

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
viso snapshot capture HomePage --output out/home.snap
```

`Viso_DSL_1.0.md` §137 的 `viso snapshot <component> --output=<path>` 即此命令。

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
viso snapshot capture HomePage                  # 默认 --target headless
viso snapshot capture HomePage --target ios --device iphone-local
```

### 23.5 JSON / exit codes

`--json`：`capture` 每个产物一条 `artifact`；`compare` 每个 golden 一条 `test`（`status: pass|fail`，失败时附 diff artifact path）；`update` 每个写回文件一条 `artifact`；最后 `summary`。Exit code：compare 不一致 1；target/device 不可用 3；capture 渲染失败 5。

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

用于 AI/CI automation。`--json`：一条 `result`（`payload: {selector, matches: [{node_id, symbol, source_span, bounds, semantics}]}`）+ `summary`。

`inspect` 可用 `--target <t> [--device <id>]` 选择 Dev Session。Exit code：找不到活跃 Dev Session 3；selector 语法错误 2；零匹配不是错误（exit 0）。

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

`--json`：采样期间 `progress`，结束时一条 `profile`（`payload: {frames, duration_ms, metrics: {<§25.2 指标>: {p50, p95, max}}}`）、trace 文件对应 `artifact`、`summary`。Exit code：无 Dev Session 且无法启动 3；app crash 5。

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

`studio` 是 GUI 进程，不接受 `--json`（usage error）；启动失败按 §7 返回 3/7。

Studio 必须调用与 CLI 相同的：

```text
project resolver
compiler service
build service
Android Emulator / iOS Simulator service
inspection service
package service
```

不得重新实现一套 build pipeline。

---

# Part VII — Delivery commands

## 27. `viso package`

构建可分发 artifact。

```bash
viso package
viso package ios
viso package android
viso package web-gpu
viso package web-dom
viso package web-hybrid
```

无 positional target 时打包当前 desktop host；不提供 `viso package macos/windows/linux` 作为普通 public grammar。

默认 profile 为 `shipping`，可用 `--profile release` 覆盖；`--profile dev` 是 usage error（package 永不包含 Dev Runtime，§13.6）。

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

### 27.5 JSON / exit codes

`--json`：`progress`、`diagnostic`、每个产物一条 `artifact`（含 `artifact.json` 本身）、`summary`。`--dry-run` 以一条 `result`（§27.3 字段）代替 `artifact`。Exit code：1（diagnostics）、3（SDK/lock）、4（build）、6（signing/archive）。

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

### 28.3 JSON / exit codes

`--json`：`progress`、`diagnostic`（含 `EXPORT_*` capability 诊断）、输出目录一条 `artifact`（`kind:"export-dir"`）、`summary`。Exit code：capability diagnostics（Unsupported、`EXPORT_HTML_DYNAMIC_REQUIRED` 等）1；exporter 写出或外部工具（如 package manager）失败 6。

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
            Text { text: format("{}", count); }
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

支持完整 Viso Web DOM runtime contract，包括 Viso resource system、typed runtime metadata，以及 dev profile 下的 Hot Reload。

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

`--json` 下按行输出 JSON event（JSON Lines，UTF-8，每行一个独立合法 JSON object，行内不换行）。长任务持续输出进度，不在结束时输出单个大对象。

示例（省略 envelope 公共字段）：

```json
{"type":"progress","payload":{"phase":"compile","message":"Compiling app"}}
{"type":"diagnostic","payload":{"schema_version":"1.0","severity":"warning","code":"W2104","message":"...","primary":{"file":"src/app.vs","byte_start":120,"byte_end":128,"line":7,"column_utf16":5,"end_line":7,"end_column_utf16":13}}}
{"type":"artifact","payload":{"kind":"binary","path":"dist/macos/app","target":"macos","profile":"dev"}}
{"type":"summary","payload":{"status":"success","exit_code":0,"elapsed_ms":842}}
```

---

## 35. Common event envelope

```json
{
  "schema": "viso.cli.event",
  "schema_version": 1,
  "seq": 0,
  "type": "diagnostic",
  "timestamp_ms": 1780000000000,
  "session_id": "...",
  "command": "build",
  "payload": {}
}
```

```text
schema          固定 "viso.cli.event"
schema_version  整数；envelope 与所有 payload 共用一个版本号
seq             本次调用内从 0 单调递增
type            §36 事件类型
timestamp_ms    Unix epoch 毫秒
session_id      本次 CLI 调用的唯一 ID；viso run/serve 时等于 Dev Session 的 DevSessionId（Viso_Hot_Reload.md §4.1）
command         顶层命令路径，如 "build"、"android emulator list"
payload         type 对应的 payload（§36）
```

版本规则：

- 同一 `schema_version` 内只允许新增 optional 字段、新增事件类型、新增 enum 值；删除/改名/改类型/改语义必须递增 `schema_version`。
- Consumer 必须忽略未知字段与未知 `type`；遇到更高 `schema_version` 可以拒绝。
- `diagnostic` payload 另带 DSL §138 自己的 `schema_version`，两者独立演进。

内部由 Ende derive 生成，JSON 是 Ende schema 的 JSON projection。

---

## 36. Event types

```text
type          payload 关键字段
progress      phase, message, current?, total?
diagnostic    Viso_DSL_1.0.md §138 JSON Diagnostic 对象（原样）
artifact      kind, path, target, profile, build_id, size, hash
device        platform(ios|android), id, name, state(booting|ready|installing|launched|stopped)
test          name, domain, status(pass|fail|skip), duration_ms, message?, artifacts[]?
profile       frames, duration_ms, metrics（§25）
server        url, host, port, lan
dev           见下
doctor_check  name, status(ok|warn|missing|error), code?, message?, suggestion?
result        查询型命令的数据载荷（形状由各命令定义）
log           level, source(app|device|tool), message
summary       见 §36.3
```

规则：

- `result` 用于 `config`、`schema`、`explain`、`dump`、`inspect query`、`android/ios list`、`emulator/simulator list`、`package --dry-run` 等查询型命令；除 `fmt`（每文件一条）外每次调用至多一条。
- 未单独说明 JSON 合同的命令只输出 `progress`、`diagnostic`、`summary`。
- `--json` 模式下 app/device 的 stdout/stderr 只能以 `log` 事件出现。

### 36.1 Diagnostic

`payload` 即 `Viso_DSL_1.0.md` §138 对象：`schema_version, severity, code, message, primary{file, byte_start, byte_end, line, column_utf16, end_line, end_column_utf16}, related[{…位置字段, message}], expected[], actual, notes[], fixes[{title, applicability, edits[{file, byte_start, byte_end, replacement}]}]`。CLI 不定义第二套 diagnostic 形状。Config 诊断（`C0001`–`C0012`）、环境诊断（如 `ENV_ANDROID_SDK`）、exporter 诊断与 CLI 自身诊断使用同一形状；无源码位置时 `primary` 为 `null`，Viso.toml 诊断的 `primary.file` 指向 `Viso.toml`。

CLI 自身诊断 code：

```text
CLI_USAGE               命令行无法解析（--json 已识别时，§37）   exit 2
ENV_CURRENT_DIR         当前目录不可读                           exit 3
ENV_SOURCE_UNREADABLE   命令需要的源文件不可读                   exit 3
```

### 36.2 Artifact

```text
kind      binary|app-bundle|apk|aab|wasm|js|asset-dir|archive|manifest|snapshot|trace|export-dir
path      相对 project root
target    host 解析后的实际平台名（macos|windows|linux）或逻辑 target
profile   dev|release|shipping；export 为 null
build_id  §46
size      bytes
hash      "sha256:<hex>"
```

### 36.3 Summary

每个命令在 protocol 启动后恰好输出一次 `summary`，且为最后一个事件；长驻命令（`run`、`serve`、`check --watch`）在退出时输出。

```text
status          success|failure|cancelled
exit_code       与进程 exit code 相同（§7）
elapsed_ms
warning_count
error_count
artifact_count
```

命令特有计数（如 `test` 的 `passed/failed/skipped`）作为额外字段加入。

### 36.4 Dev event

`viso run`/`serve` 的每个 candidate revision 输出一条 `dev`，字段取自 `Viso_Hot_Reload.md`（§4.1 identity、§8 patch class、§37 ACK/NACK、§51 stage）：

```text
dev_session_id, build_id
base_revision, candidate_revision
patch_class     PATCH|PATCH_WITH_SCOPED_RESET|WARM_RESTART_REQUIRED
outcome         applied|scoped_reset|warm_restarted|rejected
stage           rejected 时为失败所在 §51 stage（watch, parse, ..., snapshot-restore）
diagnostic_codes[]
last_good_revision
elapsed_ms
```

---

## 37. stdout / stderr 规则

Human mode：

```text
stdout -> 命令结果（查询输出、artifact path、summary）与 app stdout
stderr -> progress、diagnostics、errors、app stderr
```

JSON mode：

```text
stdout -> protocol JSON Lines only
stderr -> only unrecoverable pre-protocol launcher failure
```

Human diagnostic：头行 `severity[code]: message`，其后 ` --> file:line:column` 与带下划线（`^`）的源码行；`related` 逐条以 `-` 下划线加标签显示，位于另一文件时先输出一行 ` ::: file:line:column`；最后是 `= note:` 与每个 Fix 的 `= help:` 标题。列按字符计。

- `--json` 已被识别后的 usage error 也以 `diagnostic` + `summary{exit_code:2}` 输出到 stdout。
- 一旦 JSON protocol 已启动，不允许把普通 debug print 混进 stdout。
- 例外：`viso lsp` 的 stdout 专属 LSP JSON-RPC（§21）；`viso completion` 的 stdout 是 shell script。

---

# Part X — Viso.toml 与 CLI

## 38. 最小配置

```toml
[package]
name = "hello-viso"
bundle_id = "com.example.hello"
```

未知 key 报 C0004，类型错误 C0005（exit 1）。`Viso.toml` 不提供改变默认 run/build/package target 的 key（§3.5）。

### 38.1 Web

```toml
[web]
default_target = "web-dom"

[web.serve]
port = 8080
open = true
```

`[web] default_target` 只决定无 positional 的 `viso serve`（§15），不影响 `run`/`build`/`package`/`check`。`[web.serve] port` 视为默认端口（被占用时按 §15.2 自动换端口）；显式 `--port` 才是严格端口。

### 38.2 Profiles

```toml
[profile.dev]
opt_level = 0
source_maps = true

[profile.release]
opt_level = 3

[profile.shipping]
opt_level = "size"
strip = true
```

Hot Reload 不是可在 release/shipping profile 中重新打开的普通配置项。Dev Runtime 是否编入 artifact 由 Viso build mode 固定：

```text
dev               -> Dev Runtime present
release/shipping  -> Dev Runtime absent
```

如果 `Viso.toml` 在 release/shipping profile 中声明 `hot_reload = true` 或同义字段，CLI 必须报 C0008，而不是静默生成可远程 patch 的发布包。

Profile 名只能是 `dev|release|shipping`；其他 `[profile.<name>]` 报 C0009。`opt_level ∈ 0|1|2|3|"size"`。Web 专属优化子表 `[profile.<name>.web]` 见 §14.3。

### 38.3 Android

```toml
[target.android]
min_sdk = 26
```

GPU backend 不是项目配置项：每个 target 的 backend 由 cfg 静态决定（Android 为 Vulkan，ADR-0029）。

### 38.4 iOS

```toml
[target.ios]
minimum_os = "17.0"

[package.ios]
team_id = "ABCDE12345"
```

Simulator development 不需要 signing。`team_id`、provisioning/signing metadata 只属于 package/delivery configuration。Secret 不写进普通 project config。

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
    │   ├── android.rs
    │   ├── ios.rs
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
Target Query Service
Android Toolchain/Emulator Service
Apple Simulator Service
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

### 42.1 `viso completion <shell>`

```bash
viso completion bash|zsh|fish|powershell
```

把由 parser metadata 生成的 completion script 写到 stdout；不读取项目、不接受 `--json`。未知 shell：exit 2。

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
android/ios runtime install
```

Ctrl-C 不能让：

```text
child process
server socket
simulator/emulator install session
temporary package directory
lock file
```

永久泄漏。

规则：

1. 第一次 SIGINT/SIGTERM（Windows Ctrl-C/Ctrl-Break）触发 cooperative cancellation：停止调度新工作，通知 service 取消（`ServiceError::Cancelled`），等待进行中的原子步骤（download rename、artifact rename、patch commit）完成或回滚。
2. 第二次 SIGINT 强制终止本命令拥有的子进程；不终止非本命令启动的 simulator/emulator。
3. 退出前删除 staging/temp 输出（§51）并释放本命令持有的 lock（§44）。
4. 进程 exit code 130；若 JSON protocol 已启动，先输出 `summary{status:"cancelled", exit_code:130}`。
5. 已完成并原子落盘的 artifact 保留；未完成的不得以部分内容出现在 `dist/`。

---

## 44. Project lock

防止同一 project 同一 mutable artifact 被并发破坏。锁是 `target/viso/locks/` 下的 advisory file（不是 `flock`）：

```text
build-<target>.lock                build cache，按 target
package-<target>.lock              dist/<target>/ 输出，按 target
dev-<target>[-<device>].lock       dev session，按 target/device
```

- 获取：`create_new` 原子创建；文件内容记录 holder `pid`、`acquired_unix`、description。
- 不阻塞等待：锁被占用时立即失败，diagnostic 给出 holder pid、age 与锁路径，exit 3。
- 释放：持有者退出时删除（含 §43 取消路径）。
- Stale：只按 age 判定（不探测 pid 存活，pid 可能被复用）；阈值由调用命令给定，移除 stale lock 时输出 warning。
- 不同 target 的 build/package 可以并发；同一 target 的 `run` 与 `build` 共享 `build-<target>.lock`。

---

## 45. Cache layout

```text
target/
└── viso/
    ├── build/<build-id>/     # 按完整 BuildId 分目录，不同配置互不覆盖
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
├── macos/ | windows/ | linux/     # desktop host 解析后的平台
├── headless/
├── ios/
├── android/
└── web/
```

`target/viso/` 是可重建状态，`dist/` 是用户交付产物。`--target-dir` 只替换 `target/` 根。不要在 source tree 到处生成临时中间文件。

### 45.1 `viso clean`

```bash
viso clean
```

删除 `target/viso/`（build、cache、dev、generated、traces、locks），不删除 `dist/` 与 `~/.viso/`。存在未 stale 的 lock 时拒绝执行并 exit 3。`--json`：一条 `result`（`payload: {removed: [path], freed_bytes}`）+ `summary`。

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

不要用时间戳单独充当 build identity。BuildId 的文本形式是 32 位小写 hex，用于路径、`artifact.build_id` 与 `dev.build_id`。

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

`viso android use`、`viso ios use` 以及其他明确的 toolchain/runtime 下载必须：

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
  SDK selected by `viso android use` (~/.viso/android/)

Try:
  viso android list
  viso android use 36
  viso android doctor
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

---

# Part XIV — AI / Vibe Coding contract

## 56. AI-friendly commands

`Viso_DSL_1.0.md` §137 的 AI/CI 命令合同在 CLI 中的对应：

```text
DSL §137                                    CLI
viso fmt <paths>                            §16
viso check [package] --json                 §17
viso schema <symbol> --json                 §18（payload = DSL §139）
viso explain <error-code> --json            §19
viso dump ast|hir|ui-ir|reactive-ir|
          behavior-ir|shader-ir|system-ir   §20
viso test [package] --json                  §22
viso snapshot <component> --output=<path>   viso snapshot capture <component> --output <path>（§23.1）
viso test game <scenario> --frames --seed   §22.3
```

另外常用：`viso snapshot compare --json`、`viso inspect query ... --json`、`viso doctor --json`、`viso config show --json`。`--json` 下所有诊断均为 DSL §138 对象（§36.1），exit code 见 §7。Workspace member 选择语法（DSL 的 `[package]`）尚未定义；在此之前 `check`/`test` 作用于 `--project` 解析出的 project。

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
new -> test ui
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
fake ios simulator
fake android emulator
fake disconnected simulator/emulator
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
android/ios use download interrupted
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
  [-- app-or-forwarded-arguments]
```

Develop：

```text
viso run
viso run ios [--device <simulator-id>]
viso run android [--device <emulator-id>]
viso run web-gpu|web-dom|web-hybrid [--browser <name>]

viso build [headless|ios|android|web-gpu|web-dom|web-hybrid] [--profile <p>|--release]
viso serve [web-gpu|web-dom|web-hybrid]
viso package [ios|android|web-gpu|web-dom|web-hybrid] [--profile shipping|release]
viso check [<target>]
viso profile [<target>] [--device <id>]
viso doctor [<target>]
```

无 target 的 `run/build/package/check/profile` 表示当前 desktop host；positional 与 `--target` 的分工见 §3.5。

Language / test：

```text
viso fmt [<paths>...] [--check]
viso schema <symbol> | --search <text>
viso explain <code>
viso dump <ast|hir|ui-ir|reactive-ir|behavior-ir|shader-ir|system-ir|module-graph> [<file|symbol>]
viso lsp [--stdio]
viso test [unit|ui|game|web|all] [--target <t>] [--device <id>]
viso snapshot <capture|compare|update> [<component>] [--target <t>] [--device <id>]
viso inspect [query <selector>] [--run] [--target <t>]
viso studio [--target <t>]
```

Mobile environment：

```text
viso android list
viso android use <api>
viso android doctor
viso android emulator <list|create|delete|start|stop> ...
viso android adb [--device <emulator-id>] [--] <adb-args...>

viso ios list
viso ios use <runtime>
viso ios doctor
viso ios simulator <list|create|delete|start|stop> ...
```

Delivery / maintenance：

```text
viso export html [--out <dir>] [--static]
viso export solid [--out <dir>]
viso clean
viso completion <bash|zsh|fish|powershell>
```

Target 可用性通过 `viso doctor [<target>]` 查询，不提供单独的 `viso target` 命令。


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
doctor
check
fmt
build
run
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

## 72. P3 — Mobile development environment

实现：

```text
android list/use/doctor
android emulator list/create/delete/start/stop
android adb forwarding
ios list/use/doctor
ios simulator list/create/delete/start/stop
viso run ios/android --device <profile>
ios/android build
```

Viso 1.0 这一阶段只要求 simulator/emulator development；不把 physical-device debugging、signing 或 provisioning 作为开发闭环前置条件。Android Toolchain/Emulator service、Apple Simulator service 与 Platform runtime backend 分离。

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

### Mobile development

- `viso run` 只表示当前 desktop host；
- `viso run ios/android` 只面向 simulator/emulator；
- `--device` 只接受 Viso virtual-device profile；
- `viso android list/use/doctor/emulator/adb` 可独立完成 Android 开发环境管理；
- `viso ios list/use/doctor/simulator` 可独立完成 iOS Simulator 环境管理；
- 开发闭环不要求 signing/provisioning/physical device；
- iOS/Android 不要求用户记 `sdkmanager` / `avdmanager` / `simctl` 的普通工作流命令。

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

- 除 `lsp`、`studio`、`completion` 外所有命令支持 `--json`，且符合 §34–§37；
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

# Desktop development
viso check
viso run

# Android development environment
viso android list
viso android use 36
viso android emulator create pixel-local
viso run android --device pixel-local

# Android escape hatch
viso android adb --device pixel-local shell getprop

# iOS Simulator development (macOS)
viso ios list
viso ios use 26.0
viso ios simulator create iphone-local
viso run ios --device iphone-local

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
viso test ui
viso snapshot compare
viso inspect
viso profile --frames 600

# Delivery
viso package
viso package android
viso export html --out dist-html
viso export solid --out web-solid
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

