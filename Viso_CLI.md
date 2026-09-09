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

`headless` 是 testing/tooling target，不作为普通用户的桌面运行方式：

```bash
viso test ui --headless
viso snapshot capture HomePage --target headless
```

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

`create` 未指定 `--api` 时使用 `viso android use` 当前选择的版本。设备 model/viewport 可由预设或显式参数选择，但 ABI、system image family 和 backend 默认由 Viso 根据 host 和 API 兼容矩阵推导。

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
viso ios simulator create ipad-local --profile tablet
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

项目可有 `.viso/local.toml` 覆盖，但它默认必须被 VCS ignore。SDK path、emulator serial、Simulator UUID 不进入普通 `Viso.toml`。


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

不提供：

```text
viso run host
viso run macos
viso run windows
viso run linux
viso run --target ...
```

### 13.2 iOS / Android emulator development

```bash
viso run ios
viso run android
```

Viso 1.0 中两者只运行 simulator/emulator，不发现或部署 physical device。

指定 profile：

```bash
viso run ios --device iphone-local
viso run android --device pixel-local
```

如果没有 `--device`：

1. 存在配置的 platform default profile：使用它；
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

不需要 `--simulator` / `--emulator` flag。

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

必须：

1. stop watcher；
2. request child/emulator app graceful shutdown or detach dev session；
3. stop dev transport；
4. keep simulator/emulator boot state by default，避免下次开发重复冷启动；
5. second Ctrl-C force kill owned child processes。


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

`viso build --release` 可以保留为 `--profile release` 的构建便利别名；它不适用于 `viso run`。

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

如果 `Viso.toml` 在 release/shipping profile 中声明 `hot_reload = true` 或同义字段，CLI 必须报配置错误，而不是静默生成可远程 patch 的发布包。

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
    │   ├── target.rs
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
  configured SDK path in Viso.toml

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
  [-- app-or-forwarded-arguments]
```

Develop：

```text
viso run
viso run ios [--device <simulator-id>]
viso run android [--device <emulator-id>]
viso run web-gpu|web-dom|web-hybrid [--browser <name>]

viso build [ios|android|web-gpu|web-dom|web-hybrid]
viso serve [web-gpu|web-dom|web-hybrid]
viso package [ios|android|web-gpu|web-dom|web-hybrid]
```

无 target 的 `run/build/package` 表示当前 desktop host。

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

Query-only target metadata 可以保留给 tooling：

```text
viso target list
viso target info <logical-target>
```

`target` 不负责安装 Android/iOS SDK，也不提供 desktop `run macos/windows/linux` grammar。

Export：

```text
viso export html
viso export solid
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
viso test ui --headless
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
