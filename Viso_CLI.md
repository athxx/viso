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
    viso game peek|record
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

嵌入式板卡由项目在 `Viso.toml` 中声明为 device target（`[target.<name>]`，§38.6），以其名字作 positional target：`viso build panel` 以该 target 的 triple 与 Footprint Profile 构建，产物在 `dist/panel/`（§3.6）；`viso run panel` 在 `deploy` 所指的板卡上经 SSH 运行 dev session，`viso run kiosk --emulate` 在 macOS 上仿真 `embedded-cpu` 板卡（§13.11）。

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
<device-target> `Viso.toml` 的 `[target.<name>]`，嵌入式板卡
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

headless 构建：

- 以 facade 的 `headless` feature 编译（`--features viso/headless`），并以本机 Rust target 显式传给 Cargo（`--target <host triple>`），产物在 `target/<host triple>/<profile>/`，与 host 构建互不覆盖；不编入 hot reload；
- 没有窗口与设备：platform 为 headless backend，GPU 为软件光栅（`viso-gpu` 的 `CpuRaster`，`headless` feature 下即该构建的 `Backend`，内嵌 shader 选其 CPU 版本）；全部 service 为 unsupported；
- 每个窗口的 surface 固定，取自启动变量 `VISO_HEADLESS_SURFACE=<width>x<height>@<scale>`（逻辑像素；缺省 `1280x720@1`），由 `viso test` / `viso snapshot` 按 `Viso.toml [test.surface]`（`width`、`height` 为 1–16384，缺省 1280×720；`scale` 大于 0、至多 8，缺省 1）传入；
- 时钟每帧固定前进 1/60 s；文本在请求它的帧内排版与提交（不经后台线程、不受帧预算影响），帧数与机器快慢无关；
- 字体只来自 project（`assets/fonts/` 与 `load_font`），不读系统字体；框架不内置默认字体；
- 运行到 app 不再请求帧为止，或到 `VISO_HEADLESS_FRAMES=<n>` 帧为止。

### 3.5 Target 选择语法

- 以 target 为操作对象的命令（`run`、`build`、`serve`、`package`、`check`、`doctor`、`profile`）使用**可选 positional target**；省略时为当前 desktop host（`serve` 省略时见 §15）。
- positional 已被其他对象占用的命令（`test`、`snapshot`、`inspect`、`studio`）使用 `--target <t>`。
- `--target` 不是 global option；对接受 positional target 的命令写 `--target` 是 usage error（exit 2），例如 `viso run --target ios`。
- 任何配置层（`Viso.toml`、`VISO_*` 环境变量）都不能改变“省略 positional = desktop host”的含义（§3.1）。
- `run`、`build`、`check` 的 positional 另接受 `Viso.toml` 声明的 device target 名（§3.6）。

### 3.6 Device target

Device target 是项目自己命名的板卡（`Viso.toml` 的 `[target.<name>]`，配置见 §38.6），与内建 target 同处 positional 位置：

```bash
viso build panel                 # dist/panel/
viso build panel --release
viso check panel                 # 以 panel 的 profile 检查
viso run panel                   # deploy 所指的板卡（经 SSH），或本机即该板卡
viso run kiosk --emulate         # embedded-cpu 在 macOS 窗口中仿真
```

- positional 先按内建 target 解析；其余符合 device target 命名规则的词作为 device target 名，在解析配置时对照 `Viso.toml`：未声明时为 `C0007`（exit 1），notes 列出内建 target、`Viso.toml` 声明的全部 device target 与最接近的名字。不合命名规则的词、该命令不接受的内建 target 名与平台名（如 `viso build host`、`viso run macos`，§1）仍是 usage error（exit 2）。shell completion 给出内建 target 与当前目录所在项目声明的 device target；
- device target 只在 `build`、`run`、`check` 上接受；`package`、`serve`、`doctor`、`profile` 与 `--target <t>` 形式的命令不接受；
- 构建：`--target <triple>`（`triple` 缺省时为本机 triple，同 headless，产物与 host 构建分开），`--features` 为 profile 的 facade feature（`embedded-gpu` → `viso/gles,viso/kms`，`gpu = "vulkan"` 时 `viso/native-vulkan,viso/kms`；`embedded-cpu` → `viso/cpu,viso/kms`；`desktop`/`mobile`/`web` 不加 feature，backend 由 Rust target 决定）。embedded profile 另加 `--no-default-features`，只留 profile 的 feature，加回 `viso/rasterizer`（glyph rasterizer；`fonts.rasterizer = false` 时不加，Text/Font Runtime §3.6），声明的 locale、静态文本或 charset 用到 Thai/Lao/Khmer/Myanmar 时加 `viso/complex-line-break`（这四种文字的换行模型，Text/Font Runtime §3.7），dev profile 再加 `viso/vm`（behavior 解释器）；facade 的默认 feature `vm`、`desktop`（X11、Wayland、desktop portal、AT-SPI）、`native-vulkan`（Linux/Android 上的 Vulkan）都不进 embedded 构建，Vulkan 板卡只把 `native-vulkan` 加回来。为此 app 依赖 facade 时写 `default-features = false`，由 app 自己的默认 feature 打开它们（`viso new` 生成的 `Cargo.toml` 即如此）：

  ```toml
  [features]
  default = ["viso/vm", "viso/desktop", "viso/native-vulkan", "viso/rasterizer", "viso/complex-line-break"]

  [dependencies]
  viso = { version = "…", default-features = false }
  ```

  app 的 `viso` 依赖仍带默认 feature 时，embedded 构建照常进行，但 desktop stack 照样链接，`build`、`check`、`run` 打一条 `warn` 日志说明此事。Cargo 的环境变量另有：

  ```text
  VISO_FOOTPRINT     profile 名
  VISO_BUDGET        budget 的 key=bytes 列表，按书写顺序，如 heap=50331648,glyph_atlas=4194304
  VISO_WORKERS       workers
  VISO_KMS_MODE      display 的 WxH（width 与 height 均写时）
  VISO_KMS_FORMAT    display.format
  VISO_KMS_ROTATION  display.rotation（度）
  VISO_FONT_CHARSET  fonts.charset，逗号连接（embedded profile，写了 charset 时）
  VISO_FONT_TEXT     静态文本文件的绝对路径（embedded profile）
  VISO_FONT_BAKE     fonts.bake，逗号连接（embedded profile，写了 bake 时）
  VISO_FONT_RASTERIZER  off（embedded profile，fonts.rasterizer = false 时）
  ```

  前三者由 facade 的 build script 烘进构建（profile 与 backend feature 不符时构建失败）；`VISO_KMS_*` 是 KMS backend 的编译期默认值，运行时同名变量覆盖。表中未写的项不传，由 profile 与 backend 取默认；
- 字体子集（Text/Font Runtime §3.4）：embedded profile 的 `build`、`check`、`run` 在 Cargo 运行前把项目的静态文本写进 `target/viso/generated/fonts/<name>.txt`，路径作为 `VISO_FONT_TEXT` 交给 `packaged_fonts!()`；`fonts.charset` 作为 `VISO_FONT_CHARSET`。静态文本是 `src/` 下每个 `.vs` 文件的字符串字面量、`src/` 下 Rust 源码中每个 `ui!` / `component!` 宏体内的字符串字面量，以及 `i18n/` 各 locale 的消息文本，每个字符串一行、去重、按字节序，内容不变时不重写文件（不使增量构建失效）。它是 app 可能显示的文本的超集（asset 路径、theme 名等字面量也算在内）。缺字是宏的编译错误，照原样作为 Rust 诊断报告。desktop、mobile、web 构建不设这两个变量，字体整体嵌入。`fonts.bake` 作为 `VISO_FONT_BAKE` 让宏按这些字号把 glyph bake 成 A8 page，`fonts.rasterizer = false` 作为 `VISO_FONT_RASTERIZER=off` 让宏删去字体的 outline 并在编译期确认每个 glyph 都已 bake（Text/Font Runtime §3.6）。`viso/complex-line-break` 的判定读 device target 的 `locales`，没写时读 `i18n/` 的 catalog locale，再没有时读 `i18n.source`，同时看静态文本与 charset 的 `U+` 项中是否有这四种文字的字符；
- shader 语言：device target 的包只带其 profile 固定的 shader 语言（GLES 板卡 GLSL ES，Vulkan 板卡 SPIR-V，`embedded-cpu` 仅 `None`）。`check` 与 `build`（构建前的源码检查）对每个 `.vs` 文件按这些语言打包它绘制用的 shader，某个 shader 在其中某个语言下构建不出时报 `E3711`，构建因此失败，不运行 Cargo；
- `viso build` 的 dev profile device target 构建不带 `viso/hot-reload`：产物部署到板卡由用户自己启动；带 hot reload 的板卡 session 由 `viso run` 构建（§13.11）；
- 产物在 `dist/<name>/`，`artifact.json` 的 `target` 为该名字；build lock、BuildId 都按名字区分，两块板卡不共用 BuildId。device target 的配置（profile、triple、gpu、display、budget、workers、charset、bake、rasterizer、locales）进入 BuildId，`deploy` 不进入；
- `viso run <name>`：target 写了 `deploy` 时在该板卡上经 SSH 运行 dev session；`--emulate` 时在本机仿真；都没有时只在本机即该板卡时运行：Rust triple 与本机相同，且 embedded profile 要求本机为 Linux（显示经 DRM/KMS），与 desktop host 走同一 dev session（带 hot reload，`--cwd` 可用）。否则以 `ENV_DEVICE_TARGET`（exit 3）结束，message 指出该 target 构建的 triple，notes 指向 `deploy`、`--emulate`（`embedded-cpu`）与 `viso build <name>`。详见 §13.11。

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

### 8.6 当前实现

- 模板编译进 CLI，不需要网络（`--offline` 下同样可用）。四个模板都是一个 Cargo package，依赖与本 CLI 同版本的 `viso`（`viso = "<CLI version>"`，dev 协议双方一致），外加 `Viso.toml`（`[package] name`、`language`，`[i18n] source = "en"`）、`i18n/en.toml`、空的 `assets/` 与 `.gitignore`（`/target`、`/dist`）。View 的文本都取自 `tr`，因此新项目在 `viso check --a11y strict --i18n strict` 下也没有诊断：
  - `app`：`src/main.rs` 只有 `viso::run::<App>()`，`src/app.rs` 用 `view!("app.vs")` 挂载 `src/app.vs`（一个 state、一个 Button、一个 plural 文本），`tests/ui/clicks.tape`；`assets/fonts/` 带一个字体（Noto Sans Regular 与它的 OFL），`App::new` 调 `cx.use_packaged_fonts(viso::packaged_fonts!())`，所以文本在各平台与 headless snapshot 中一致（headless 只用项目字体）。`game`、`web` 同样带它；
  - `game`：同样的 Rust 入口，`src/app.vs` 是一个 `FixedUpdate` System（`@probe state jumps`）和它的 HUD，`Viso.toml` 加 `[game] tick_rate = 60`，`tests/game/jump.tape` 与 `tests/ui/hint.tape`；
  - `web`：`app` 加 `[web] default_target = "web-dom"`；
  - `library`：没有 binary，`src/lib.rs` 导出 `pub fn counter(cx: &mut BuildCx)`（挂载 `src/counter.vs`），没有 `tests/ui/`（UI Scenario 需要可执行文件）。
- `--framework <path>` 改为依赖该路径上的框架：路径是 `viso` crate 本身，或包含 `crates/viso` 的 checkout；依赖写成 `viso = { path = "…" }`，相对项目目录（无共同根时为绝对路径，分隔符写作 `/`）。路径不是 `viso` crate 时为 `NEW_FRAMEWORK`（exit 2，不写任何文件）。
- `ui!`、`component!`、`view!`、`asset!` 展开为经由 facade 的路径，所以只依赖 `viso` 的项目可以编译它的 View。
- Package name 默认取目录名（`.` 取当前目录名），按 Cargo 规则校验：ASCII 字母、数字、`-`、`_`，以字母或 `_` 开头，至多 64 个字符，不是 Rust 关键字、标准库 crate、`viso` 或 Windows 保留设备名；否则 `NEW_NAME_INVALID`（exit 2），名字来自目录时附带一个可用的 `--name`。
- 路径不存在时连同父目录一起创建；路径上有文件、目录非空且未给 `--force-empty-dir`（note 列出已有内容，fix 为加上该 flag 的命令），或 `--force-empty-dir` 下已有项目要写的文件（note 列出它们），为 `NEW_PATH_TAKEN`（exit 2）。`--force-empty-dir` 从不替换文件。
- 写出是原子的：新目录先在同级的 staging 目录写全，再一次 rename；已有目录（如 `.`）在其内部的 staging 目录写全，再逐个文件移入。失败或 Ctrl-C 时删除已写的文件与新建的目录（含新建的父目录），exit 7（`NEW_WRITE`）或 130。
- 不给 `--no-git` 时，目录不在已有 Git work tree 中则 `git init`；没有 Git 或 `git init` 失败是 `NEW_GIT` warning，项目照常创建。
- 人类输出为 `Created the app `<name>` in <path>` 加下一步命令（`cd`，然后 `viso run`/`viso serve`/`viso check` 与 `viso test`）。
- 验收测试（`tools/cli/tests/new.rs`）对每个模板运行 `new --framework <workspace> → check → build headless（library 除外）→ test`，离线。

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

### 9.5 当前实现

- `viso doctor` 的 check（`name`）：`rust`（`rustc -vV`，早于框架的 `rust-version` 为 `ENV_RUST_TOOLCHAIN` error）、`cargo`、`rustup`、`project`（`Viso.toml`、language 版本、配置解析、根目录的 `Cargo.toml`；不在 project 中为 warn，建议 `viso new <name>`）、`framework`（project 的 `Cargo.lock` 中 `viso` 的版本与 CLI 相同）、`linker`（`cc`，Windows 为 Visual Studio Build Tools）、`gpu`（macOS Metal、Windows Direct3D 12、Linux Vulkan loader 与 ICD；没有为 `ENV_GPU` error）、`wasm32`、`wasm-bindgen`（与 `Cargo.lock` 锁定的版本一致）、`wasm-opt`、`filesystem`（project 的 `target/` 与 `~/.viso` 可写）、`git`、`lsp`（`viso-lsp --version` 与 CLI 版本相同；缺失或不同为 warn，建议 `cargo install viso-lsp --version <cli>`）、`android-sdk`、`ios-runtime`；host doctor 中 target 工具的缺口只是 warn。
- `viso doctor <target>` 为 `rust`、`cargo`、`project` 加上该 target 的 check，缺口为 `missing`：`ios` — `xcode`、`xcode-license`、`ios-runtime`、`ios-disk`、`ios-rust`、`ios-profiles`、`gpu`（见 §12.7）；`android` — `android-sdk`（`ENV_ANDROID_SDK`，message 给出 SDK 的来源）、`android-components`（默认 API level 的固定组件是否都装好）、`android-tools`、`android-accel`（`emulator -accel-check`：HVF/KVM/WHPX）、`java`（`javac -version`：APK 的 Java shell 由它编译，缺失为 `ENV_TOOL`）、`android-rust`、`android-profiles`；`web-gpu` — `wasm32`、`wasm-bindgen`、`wasm-opt`；`headless` — `linker`、`filesystem`；`web-dom`/`web-hybrid` — `target`（`C0011`）。不适用的 check（没有 project 时的 `framework`）不输出。
- 各 check 同时运行，单个工具最多 15 s、单个 check 最多 20 s，超时为 error 并说明超时，不会挂住 doctor。
- `--fix` 只做本地可逆的修复：缺少的 Rust target 以 `rustup target add <triple>` 加上，运行前先说出命令（human 为 stderr 一行，JSON 为 `progress{phase:"fix"}`），之后重新检查；不安装 SDK、runtime 或工具。
- 人类输出：`Viso Doctor`、每个 check 一行 `[status] <title>: <message>`、`Suggested actions:` 列出未通过 check 的建议命令。

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

这些检查在 manifest 解析时进行，因此每个读取项目的命令都报告同样的问题；`validate` 额外解析 `VISO_*` 环境变量（C0010）。具体规则：`package.bundle_id` 为 iOS 与 Android 都接受的反向域名（两段以上，每段字母开头、只含字母数字）；`package.version` 为 1–3 段数字加可选 `-pre`/`+build`；`package.ios.team_id` 为 10 位大写字母数字；`target.ios.minimum_os` 为 1–3 段数字；`export.*.out_dir` 为留在 project 内的相对路径；`export.solid.package_manager` ∈ `npm|pnpm|yarn|bun`；`web.default_target` 只能是 `web-*`；`package.windows.timestamp_url` 为 `http://`/`https://` URL；`package.windows.publisher` 为证书 subject 的写法（逗号分隔的 `KEY=value`，KEY 为 `CN`/`O`/`OU`/`L`/`S`/`C`/`E`/`STREET`/`DC`… 或 `OID.<n>.<n>…`，含 `,+="<>#;` 的值加引号）。违反者为 C0007，指向所在行。

### 10.5 不提供隐式 magic write

不设计：

```text
viso config set arbitrary.deep.key ...
```

作为核心工作流。

原因：配置应接受 code review，文本文件是 source of truth。

### 10.6 JSON / exit codes

`show`/`get`/`path` 各输出一条 `result`（`show`：`{key: {value, origin, from?}}` 映射，key 顺序固定；`get`：`{key, value, origin, from?}`；`path`：`{path}`）。`value` 带 JSON 类型（字符串、数字、布尔、字符串数组；未设置且无默认值为 `null`）；`origin` 为 provenance 名，`from` 在 `env`/`profile`/`manifest` 时给出具体位置（`env VISO_OPT_LEVEL`、`Viso.toml [profile.release]:5:9`、`Viso.toml:2:8`）。`validate` 每个问题一条 `diagnostic`（C0001–C0012，manifest 与环境变量的问题一次报全），全部通过时一条 `result{path}`。`get` 未知 key 为 `CLI_USAGE`（exit 2，note 给出最接近的 key）。Exit code 按 §7 的 config 诊断映射；`validate` 无 error 时 0。

Key 集合是完整的 schema：`profile`、`opt_level`、`source_maps`、`strip`（当前 profile 的值，经全部层解析）、`dev_runtime`（由 profile 决定），`package.*`，`profile.<dev|release|shipping>.<opt_level|source_maps|strip>` 与 `profile.<…>.web.<optimize|strip|source_maps|brotli|gzip|split|threads>`（以该 profile 构建时得到的值，环境变量同样作用），以及 `web.*`、`target.*`、`export.*`、`game.*`、`test.surface.<width|height|scale>`、`i18n.source`、`workspace.members`（manifest 值或框架默认值）。`--profile` 决定"当前 profile"。

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
- `adb`：human 模式下 stdout/stderr 与 exit code 原样来自 adb；`--json`（写在 adb 的参数之前，之后的一切原样交给 adb）下 adb 不接 stdin，每行输出一条 `log`（`source: "device"`，stderr 为 `warn`），adb 以失败状态退出为 `ANDROID_ADB_FAILED`（exit 3，message 带其状态），第一次 Ctrl-C 结束它（exit 130）；Viso 自身解析 SDK 失败时 exit 3。

Exit code：不在支持矩阵的 API（`ANDROID_API_UNSUPPORTED`）或未知 profile 2；SDK/emulator/host virtualization 不可用 3；emulator 启动失败 5。

### 11.8 当前实现

- 支持矩阵：API 36（Android 16）、35（15）、34（14），默认推荐 36。每个 level 的组件集固定在 CLI 中：`platform-tools`、`build-tools;36.0.0`、`ndk;28.2.13676358`、`emulator`（各 level 共用）、`platforms;android-<api>` 与 host ABI 的 `system-images;android-<api>;google_apis;<abi>`；每个 archive 按 host（macOS arm64/x86_64、Linux x86_64、Windows x86_64）固定 URL、大小与 Google repository manifest 发布的 SHA-1。不另装 command-line tools：Viso 自己写 AVD，不需要 `sdkmanager`/`avdmanager`。
- SDK 解析顺序：非空的 `ANDROID_HOME`、`ANDROID_SDK_ROOT` > `viso android use` 选过 level 后的 `~/.viso/android/sdk` > 平台默认位置；`list`、`doctor` 与 `use` 的结果说明来源（`origin` ∈ 变量名|`viso`|`default`）。默认 level 记录在 `~/.viso/android/config.toml` 的 `api`，文件中的其他 key 保留；`emulator create` 未指定 `--api` 时用它。
- `list`：`--json` 一条 `result{sdk{root, origin}|null, default, abi, apis[{api, android, status, system_image, default, components[{path, revision, title, installed, download_bytes}]}]}`，`status` ∈ `installed|partial|available`；`--offline` 只列已装的 level。
- `use <api>`：只下载未装的组件。下载前先取得 license 同意：`--accept-licenses`，或 TTY 上显示 license 全文并询问；非 TTY 无 flag 时在任何网络请求之前以 `ENV_ANDROID_LICENSE` 结束。同意记录为 `~/.viso/android/licenses/<id>`（license 文本的 SHA-1，与 Android 工具的记录同形）。每个 archive 以 `curl`（只允许 HTTPS、TLS 1.2+、失败重试）下载到 `~/.viso/android/downloads/<file>.part`，连接中途断开而仍有新字节到达时立即续传，连续 3 次没有新字节才以 `ENV_DOWNLOAD` 结束，已到部分保留，重新运行从那里续传；完成后核对大小与 SHA-1，不符为 `ENV_DOWNLOAD_INTEGRITY` 并删除。解包由 CLI 自己的 zip reader 完成（ZIP64、逐 entry CRC，拒绝绝对路径、`..` 与指向外部的 link，link 最后创建），NDK 只保留 `toolchains/llvm/prebuilt`，platform 只保留 `android.jar`；解到 `sdk/.staging/` 后一次 rename 到位，目录中的 `.viso-component` 记录 path 与 revision，revision 相同即视为已装。进度：TTY 上一行实时刷新，否则（含 `--json`）每秒至多一条 `progress{phase:"download"}`，每个组件装好一条 `progress{phase:"install"}`。`result{api, android, installed[], default:true, sdk, active}`，`active` 为 build 是否会用这个 SDK（`ANDROID_HOME` 等覆盖时为 `false`，人类输出附 note）。Ctrl-C 保留已下载部分，exit 130。
- `doctor` 即 `viso doctor android`（§9.5）。
- `adb`：`--device <profile>` 解析为运行中 emulator 的 serial（`-s <serial>`），未运行为 `ENV_DEVICE`（exit 3，下一步 `viso android emulator start <id>`）；第一个 adb 参数之后的全部参数（包括形如 flag 的）原样交给 adb，进程以 `exec` 替换（Windows 上等待并传回 exit code）；`--json` 为 `CLI_USAGE`。
- 没有 SDK 的命令（`build android`、`run android`、`adb`）以 `ENV_ANDROID_SDK` 结束，下一步为 `viso android use <默认或推荐 level>`。

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

### 12.7 当前实现

- 非 macOS host 上每个 `viso ios` 子命令（含 `simulator ...`）都以一条 `ENV_IOS_HOST`（exit 3）结束。
- Runtime 来自 `xcrun simctl list runtimes -j`（只取 iOS，新版本在前）；Xcode 的 iOS SDK 所配的 runtime 来自 `simctl runtime match list -j`，它是 `xcodebuild -downloadPlatform iOS` 不带 build version 时安装的那个。版本按数字段前缀匹配：`27` 匹配 `27.0`、`26.3` 匹配 `26.3.1`，缺少的段为 0。
- 默认 runtime 记录在 `~/.viso/ios/config.toml` 的 `runtime`（按用户写的版本保存，`27` 跟随最新的 27.x），文件中的其他 key 保留。`ios simulator create` 与 `viso run ios` 新建的 simulator 用默认 runtime（已装时），否则用最新的。
- `list`：`--json` 一条 `result{xcode, default, runtimes[{version, status, build, identifier, unusable, phone, tablet, default}]}`，`status` ∈ `installed|unusable|available`（`available` 只有 Xcode 所配且未装的那个；Xcode 不报告其他可下载版本），`phone`/`tablet` 为 preset 会选的机型，`default` 为新 simulator 用的那一个；人类输出 `RUNTIME STATUS DEFAULT` 表，不可用的 runtime 另起一行说明原因；`--offline` 不列 `available`。
- `use <runtime>`：版本不合法为 `IOS_RUNTIME_INVALID`（exit 2）。已装则只记录默认。未装时：`--offline` 为 `ENV_OFFLINE`；未给 `--download` 且不能在 TTY 上询问为 `IOS_RUNTIME_MISSING`（exit 2，下一步 `viso ios use <v> --download`），Xcode 不被调用；可用空间不足约 10 GiB 为 `ENV_INSTALL`；`xcodebuild -license check` 未通过为 `ENV_XCODE_LICENSE`。下载由 `xcodebuild -downloadPlatform iOS`（不是 Xcode 所配版本时加 `-buildVersion <v>`）完成，它的进度行（TTY 上一行实时刷新，否则每秒至多一条 `progress{phase:"download"}`）转为 Viso 的进度；失败为 `ENV_DOWNLOAD`，message 带 Xcode 的原话，下一步为 Xcode 所配的版本；完成后重新列 runtime，仍无可用的为 `ENV_INSTALL`。Ctrl-C 结束 `xcodebuild`，exit 130。`result{runtime, version, build, identifier, installed, default:true}`。
- `doctor` 即 `viso doctor ios`：`xcode`（`xcode-select -p` 指向 Xcode 而非 Command Line Tools，`xcodebuild -version`；`ENV_XCODE`）、`xcode-license`（`ENV_XCODE_LICENSE`，建议 `sudo xcodebuild -license`）、`ios-runtime`（默认 runtime 未装为 `DEVICE_SYSTEM`，建议 `viso ios use <v> --download`；没有任何 runtime 时建议 `viso ios use <Xcode 所配版本>`）、`ios-disk`（runtime 所在卷可用空间，不足约 10 GiB 为 warn）、`ios-rust`、`ios-profiles`、`gpu`。


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

`viso run host|macos|windows|linux|headless` 与 `viso run --target ...` 均为 usage error（§1、§3.5），本机与其他 target 同时运行用 `--desktop`（§13.3A）；`viso run headless` 建议 `viso test ui` 与 `viso build headless`。

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

### 13.3A 一个 session 多个 target

```bash
viso run web-gpu --desktop
viso run kiosk --emulate --desktop
viso run ios web-gpu
```

`viso run [TARGET]...` 把每个 target 作为同一 Dev Session 的一条 lane 运行（ADR 0046）；`--desktop` 再加上本机，作为第一条 lane（本机仍不是 positional target，§1）。共享一个 session id/token、watcher、source graph、control endpoint 和输出流，一次编辑只 parse/type-check 一次，按 runtime 各自 plan；每条 lane 有自己的 dev lock（全部在任何 build 开始前、按 lane 顺序取得）、Rust plan 与 `RustBuild`、build id、deploy、dev listener 和 restart 状态。两条 lane 构建同一 Rust triple 时，后者用 `<target-dir>/viso/lanes/<target>/`，不会启动另一条 lane 的可执行文件。

- 同一 target 写两次、多于一个 simulator/emulator 均为 usage error；web target 可以有多个（各一次），共用 session 的一个页面 server（§15.4）；`--device`、`--browser`/`--open`、`--emulate`、`--cwd` 作用于合适的 lane，没有合适 lane 时为 usage error；
- Rust 编辑重建 scope 含该文件的每条 lane；warm restart 一次一条 lane（snapshot、close、launch、restore 属于该 lane 直到落地或失败），其余 lane 的新 build 排队；一条 lane 的 build 或 restart 失败不影响其他 lane；
- 多 lane 时 app 日志行以 `[target] ` 开头；`viso game`/`viso inspect` 未指定 runtime 时交给第一条 lane 的 app；
- 所有 lane 的 app 都结束时 session 结束，exit code 取第一条 lane；中途结束的 lane 报告一行，其 runtime 离开 session；第一次中断关闭所有 app，第二次 kill。

`viso serve [TARGET]...` 是只含 web lane 的同一种 session（§15）。

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
--emulate               # embedded-cpu device target, macOS only
```

- `--open`：web target 打开浏览器（`--json` 下同样打开）；非 web target 或与 `--browser none` 同时出现为 usage error；
- `--env KEY=VALUE`：可重复；KEY 为可移植变量名（字母、数字、`_`，不以数字开头），`VISO_DEV_*` 保留给 dev session；desktop 为进程环境变量，iOS/Android/web 经 launch 变量（§13.10），app 一律以 `viso_platform::launch_var` 读取；名字像 secret（含 TOKEN、SECRET、PASSWORD、KEY、CREDENTIAL、AUTH、PRIVATE、COOKIE、SESSION）的值在 `--verbose` 中显示为 `<hidden>`（§49）；
- `--cwd <path>`：desktop app 的工作目录，相对调用 `viso` 的目录，默认 project root；非 host target 或不是目录为 usage error；本机运行的 device target 同样可用，经 SSH 的板卡 session 不可用；
- `--emulate`：device target 的 profile 在本机仿真（§13.11）；
- `--profile-frame`：session 对每个连上的 runtime 开一个 capture（§25.4），每个窗口右上角显示最近 1 秒的帧与 GPU 时间 p50/p95（每 500 ms 更新）；`--json` 下每帧一条 `profile`（`payload: {frame, at_ms, metrics: {<§25.2 指标>: <该帧的值>}}`，该帧没有的指标省略），human 模式每秒在 stderr 打一行 `[viso] profile  120 frames  frame 8.29/8.97 ms  gpu 0.09/0.09 ms`；
- `--inspect`：session 跟随首个连上的 runtime 的首个窗口（§24.5），与 `viso inspect events` 同一 journal：`--json` 下每个事件一条 `inspect`，human 模式每个事件一行；runtime 断开后跟随下一个连上的。

`viso run` 不提供 `--release`。Release/Shipping 验证使用 `viso build --profile release`、`viso package`、benchmark/profile 工具，而不是把开发主命令变成 delivery frontend。

### 13.8 Ctrl-C

在 §43 通用取消规则之上，`run` 额外要求：

1. stop watcher；
2. request child/emulator app graceful shutdown or detach dev session；
3. stop dev transport；
4. keep simulator/emulator boot state by default，避免下次开发重复冷启动；
5. second Ctrl-C force kill owned child processes。

### 13.9 JSON / exit codes

`--json` 事件：`progress`、`diagnostic`、`artifact`（dev artifact）、`device`、`dev`（每个 candidate revision 一条，§36）、`log`、`profile`（`--profile-frame`）、`inspect`（`--inspect`）、`summary`。Dev session 中的 candidate 失败只产生 `diagnostic` + `dev{outcome:"rejected"}`，不结束命令。

Exit code：首次 build 失败按 §7（1/3/4）；app 正常退出 0；app crash 或非零退出 5；Ctrl-C 结束 session 130。

### 13.10 当前实现子集

已实现的是 desktop host 上 `.vs` 的 hot reload：host 监听、编译并对每个挂载所运行的 last-good 算出 typed patch，app 只提交、不含编译器（`Viso_Hot_Reload.md` §9、§35）：

- CLI 先取 dev session lock（`target/viso/locks/`，§44；同一 project 已有 `viso run` 时以 `TargetUnavailable` 退出，exit 3），再在 build 之前启动 project watcher（`Viso_Hot_Reload.md` §4、§6、§7）；
- project root 须是一个 Cargo package（root 下有 `Cargo.toml`，否则 `ENV_CARGO_MANIFEST`）；CLI 以 `VISO_PROFILE=dev cargo build --features viso/hot-reload --message-format=json-render-diagnostics` 构建 Dev artifact（facade 的 build script 拒绝 `hot-reload` 与 `VISO_PROFILE=release|shipping` 同时出现，见 `Viso_Hot_Reload.md` §58），构建须恰好产出一个可执行文件（否则 `ENV_NO_EXECUTABLE`）。`$CARGO` 覆盖 cargo 路径；
- app 以 project root 为工作目录启动，`--` 之后的参数原样传入；
- Dev Runtime transport 是 loopback TCP：CLI 绑定 `127.0.0.1` 的临时端口，经 `VISO_DEV_RUNTIME`（地址）、`VISO_DEV_TOKEN`（每 session 随机 128-bit token）、`VISO_DEV_SESSION`（dev session id）与 `VISO_DEV_BUILD`（所启动 artifact 的 build id）交给 app。app 只连 loopback 地址，先发 `RuntimeHello`（`Viso_Hot_Reload.md` §34）；token 不符、首帧不是 hello 或 5 秒内没有首帧的连接被丢弃且不回应，protocol version、session、build 或编译器 schema 不符的连接收到带原因的 `Reject`；两种情况都记一条 `log{level:"warn", source:"tool"}`（protocol/build/schema 不符时提示 rebuild）。接受时 CLI 回 `HostHello`；app 报告它挂载的文件（mount inventory），CLI 按所报 `source_hash` 找到 build 编译的版本并编译为 last-good，在 host 上编译每个 batch 的编辑，输出 `diagnostic` 与 `dev`，把编译通过的对 last-good 算出 typed patch 发送（ACK 后它成为新的 last-good）、把被拒编辑的错误行发给 app 显示，app 回 ACK/NACK；app 的 log 与丢弃计数也经此 channel；帧为 u32 LE 长度 + ende 二进制 body，上限 4 MiB；
- `--json` 下 app 与 cargo 的 stdout/stderr 逐行成为 `log`（app stdout 为 `info`、app stderr 为 `warn`；cargo 行按前缀 `error`/`warning` 定级，编译诊断按其 level）；human mode 下它们直通终端，每次 reload 在 stderr 打一行摘要；
- `--no-hot-reload`：session 照常建立、watch、编译并报告诊断，但不向 app 发送 patch 与失败 overlay；编译通过的 candidate 输出 `dev{outcome:"held"}`；
- Rust 改动在后台重新 build Dev artifact（`Viso_Hot_Reload.md` §6、§25），旧 app 保持运行：失败输出 `dev{outcome:"rejected", stage:"warm-restart"}`；成功则 warm restart：旧 app 交出 DevSnapshot 后退出，新 build 以 `VISO_DEV_RESTORE` 启动并在第一帧之前恢复状态，输出 `dev{outcome:"warm_restarted", patch_class:"WARM_RESTART_REQUIRED"}`；`--no-hot-reload` 下输出 `dev{outcome:"held"}`、不重启；
- Ctrl-C/SIGTERM：第一次杀掉进行中的 build（整棵进程树）并请 app 退出（Unix 上 SIGTERM），第二次强制结束 app；exit 130，`summary{status:"cancelled"}`；
- `viso run ios|android|web-gpu`（§13.2、§13.3）走同一 dev session，只有 build target、打包、部署与 transport 不同（`Viso_Hot_Reload.md` §5、§27、§28）：
  - device 选择按 `--device`、`.viso/local.toml` 的 `[ios]`/`[android] device`、`~/.viso/<platform>/config.toml` 的 `device`、唯一 profile 的顺序；没有 profile 时 `DEVICE_MISSING`（exit 3），有多个且无默认时 `DEVICE_AMBIGUOUS`、名字不存在时 `DEVICE_UNKNOWN`（exit 2），均附 suggested command；human 输出、stdin 与 stderr 都是终端且无 `CI` 环境变量时，多个无默认则列出 profile 让用户按序号或 ID 选择（无效回答重问，输入结束则回到 `DEVICE_AMBIGUOUS`），并提示写入 `.viso/local.toml` 的默认，没有 profile 则询问是否创建标准 profile `phone`（最新已装系统上的 phone，回车即同意）；`--json`、非 TTY 与 CI 从不等待 stdin；
  - 所选 device 在 launch build 期间 boot，输出 `device{state:"booting"|"ready"|"launched"}`；boot 失败立即以 `ENV_DEVICE`（exit 3）结束；Ctrl-C 不关闭 simulator/emulator；
  - session lock 带 device id；
  - iOS 构建 simulator triple 并打包为未签名 `.app`；Android 构建 emulator ABI 的 `cdylib` 并打包为 dev APK，需要 Android SDK（`ANDROID_HOME`/`ANDROID_SDK_ROOT` 或默认位置）、NDK 与 build-tools，缺失时 `ENV_DEVICE`；web-gpu 构建 `wasm32-unknown-unknown` 并经 `wasm-bindgen` 生成 bundle，打开浏览器（`--browser chrome|safari|firefox|none`，省略为系统默认），输出 `server{target,url,host,port,lan:false}`；
  - launch 变量在 iOS 上经 `SIMCTL_CHILD_*`、Android 上经 intent extras、web 上经页面的 `visoLaunch`，app 一律用 `viso_platform::launch_var` 读取；`--` 之后的参数只交给 desktop 与 iOS，其它 target 给出 warning；
  - Rust 改动的 warm restart 在新 artifact 打包完成后才停止旧 app；
  - `web-dom`/`web-hybrid` 以 `TargetUnavailable` 退出；
- `viso ios simulator` / `viso android emulator` 的 `list|create|start|stop|delete`（§11.4、§12.4）：profile 存于 `~/.viso/ios/simulators/<id>.toml` / `~/.viso/android/emulators/<id>.toml`（`preset`、`system`、`device`，`$VISO_HOME` 可改 root）；iOS 以最新已安装 runtime 与对应 iPhone/iPad 机型创建 Simulator，Android 不依赖 `avdmanager`，直接按最新（或 `--api` 指定）已安装 system image 写出 AVD `viso_<id>`；`start --no-window` 无窗口启动 emulator；`list` 列出 `NAME PROFILE STATUS SYSTEM`；
- Dev artifact 与 `viso build` 共用 Build Service（§14.5）：以 dev profile 的 `--profile dev` 与 `[profile.dev]` 设置构建；每次 build 前取 `build-<target>.lock`，被 `viso build` 占用时等待（输出一条 `log`）而不失败；`--json` 下 launch build 之后输出一条 `artifact`（host `binary`、iOS `app-bundle`、Android `apk`、web `wasm`，`path` 相对 project root），Rust 编译诊断为带 span 的 `diagnostic`；
- `viso serve` 不提供 HTTPS：loopback 页面本身就是 secure context，WebGPU 可用；另一台设备的浏览器经端口转发以 loopback 打开页面（Android `adb reverse`），不为此引入 TLS 依赖。

---

### 13.11 Device target：板卡 session 与桌面仿真

```bash
viso run panel                   # [target.panel] 写了 deploy：板卡上的 dev session
viso run kiosk --emulate         # embedded-cpu：本机 macOS 窗口中的仿真
```

**板卡 session**（Hot Reload §27A）：`[target.<name>]` 写了 `deploy = "user@host[:port]"` 时，`viso run <name>` 总在该板卡上运行，即使本机 triple 与之相同：

1. 构建前先打开 session：`ssh` 到板卡（batch mode，不问密码：板卡须接受本机或 agent 的 key；`-p` 为 port），清空并创建 `~/viso-dev/<project>`，并读取该连接的 `$SSH_CONNECTION`。dev channel 监听在本机通往板卡那一跳的地址上：对 `$SSH_CONNECTION` 中板卡一侧的地址 connect 一个 UDP socket，取其本地地址（不发包）。该地址须是 loopback、IPv4 私有或 link-local、IPv6 unique-local 或 link-local（与 runtime 的 `dev_host` 规则相同），否则 `ENV_DEV_CHANNEL`；SSH 失败为 `ENV_DEPLOY`（exit 3），在构建开始前结束；
2. 为 target 的 triple 与 profile 交叉构建 dev artifact：embedded feature 集（§3.6）加 `viso/hot-reload`，budget、workers、display 与字体子集照常烘入；
3. `scp` 可执行文件进 session 目录，第一次 launch 时连同项目的 `assets/`（之后的 asset 改动经 patch 到达）；
4. `ssh` 启动：远程 `sh` 进入 session 目录、把自己的 pid 写进 `.pid`，再以 `exec env VISO_DEV_RUNTIME=… VISO_DEV_TOKEN=… VISO_DEV_SESSION=… VISO_DEV_BUILD=… ./<bin> <app args>` 启动 app（`--env` 的变量在前）。app 的 stdout/stderr 经该连接成为 `log`，连接的退出状态即 app 的（255 是 SSH 自己的：连接失败或断开，报为 `the SSH connection to the board failed or was lost`）。启动 app 的 `ssh` 在自己的进程组里，终端的 Ctrl-C 只交给 session：session 经另一条 SSH 连接向 `.pid` 发 `SIGTERM`（第二次 Ctrl-C 为 `SIGKILL`），app 如在桌面上一样关闭；
5. patch 与桌面相同。Rust 改动走 warm restart：新 artifact 在旧 app 仍运行时先 `scp` 为 session 目录中的 `.next-<bin>`（限 300 s），旧 app 交出 snapshot 并退出后，以一次 SSH 校验其大小并移到位再启动；未完整到达时直接重新复制；
6. listener 打开后打印一条 `warn`：dev channel 在本地网络上不加密，同一网络上的设备能看到 session 发出的 patch。

`--cwd` 在板卡 session 上是 usage error（app 在 session 目录中运行）。

**桌面仿真**（Architecture §50A.7）：`viso run <name> --emulate` 只接受 `embedded-cpu` 的 device target，只在 macOS 上运行：为本机 triple 构建，feature 为该 target 的 embedded feature 集加 `viso/emulate` 与 `viso/hot-reload`（`--no-default-features`），`VISO_FOOTPRINT`、`VISO_BUDGET`、`VISO_WORKERS`、`VISO_KMS_*` 与字体变量和板卡构建相同；之后如 desktop host 一样在本机启动与 hot reload（`--cwd` 可用）。面板以构建烘入的 `VISO_KMS_MODE`/`FORMAT`/`ROTATION` 显示，运行时同名变量（`--env`）与 `VISO_SCALE` 覆盖。`embedded-gpu` 的 target 为 `ENV_EMULATE`（exit 3：macOS 上没有 GLES，GPU 板卡的驱动不在桌面上），非 macOS 主机同样为 `ENV_EMULATE`；非 embedded 的 device target 与非 device target 的 `--emulate` 是 usage error。`--emulate` 优先于 `deploy`。

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

`[profile.<name>.web]` 的 key 与默认值（`viso config show` 中为 `profile.<name>.web.<key>`）：

```text
optimize     none|speed|size   dev none，release speed，shipping size
strip        bool              默认随该 profile 的 strip（及其来源）
source_maps  bool              默认随该 profile 的 source_maps（及其来源）
brotli       bool              shipping true，其余 false
gzip         bool              shipping true，其余 false
split        auto|off          auto
threads      auto|off          auto
```

- `VISO_STRIP`/`VISO_SOURCE_MAPS` 仍优先于 web 子表（§10 的层序）；
- `source_maps` 让 `wasm-bindgen` 保留 DWARF（`--keep-debug`）并让 Cargo 生成 debug info；`strip` 去掉 name 与 producers section；
- `optimize` 经 `wasm-opt`（`-O3`/`-Oz`，启用 Rust 默认产生的 bulk-memory、mutable-globals、nontrapping-float-to-int、sign-ext、reference-types、multivalue）；保留 names/debug info 时加 `-g`。`wasm-opt` 缺失：dev/release 输出 `log{level:"warn"}` 并保留未优化模块，shipping 以 `ENV_TOOL`（exit 3）失败；
- 模块与 glue 按内容命名（`<bin>_bg.<hash>.wasm`、`<bin>.<hash>.js`，`<hash>` 为文件 SHA-256 的前 16 位十六进制），`index.html` 不带 hash，以 `init({ module_or_path })` 加载该模块，服务端可对带 hash 的文件永久缓存、对页面每次重新验证；
- `brotli`/`gzip` 在每个 `.wasm`/`.js`/`.html` 旁写 `.br`（quality 11、window 24）与 `.gz`（level 9），各文件并行压缩；
- `split`/`threads` 的 `auto` 当前解析为不拆分、单线程（拆分与 shared-memory 线程模块见 C4.2）。

### 14.4 Artifact summary

Human output：

```text
Built web-gpu (shipping) in 41.3 s
  js         dist/web/app.3f9a0c1e7b2d4a65.js              18.0 KiB
  brotli     dist/web/app.3f9a0c1e7b2d4a65.js.br           4.1 KiB
  gzip       dist/web/app.3f9a0c1e7b2d4a65.js.gz           5.0 KiB
  wasm       dist/web/app_bg.c41d8e09a6f3b257.wasm         1.82 MiB
  brotli     dist/web/app_bg.c41d8e09a6f3b257.wasm.br      612 KiB
  gzip       dist/web/app_bg.c41d8e09a6f3b257.wasm.gz      744 KiB
  html       dist/web/index.html                           402 B
  brotli     dist/web/index.html.br                        233 B
  gzip       dist/web/index.html.gz                        281 B
  manifest   dist/web/artifact.json                        1.1 KiB
```

表头与各行写 stdout（`--quiet` 保留）；`--verbose` 另给出各阶段耗时（compile、package、publish）。`--json` 事件：`progress`、`diagnostic`（含 Rust 编译诊断，带 span）、每个产物一条 `artifact`（§36.2）、`summary`（`artifact_count`）。Exit code：1（source/config diagnostics）、3（target/SDK/工具不可用、lock 被占用、输出写不出）、4（compile/link 失败 `BUILD_FAILED`，打包失败 `BUILD_PACKAGE`）、130（Ctrl-C）。

### 14.5 Build Service

`viso build` 与 `viso run` 共用同一 Build Service：

1. 先做 `viso check` 的全部检查（manifest、`.vs`、catalog、assets）；有 error 时 exit 1，Cargo 不运行；
2. 取 `build-<target>.lock`（§44，不等待）；
3. Cargo profile：`dev`→`--profile dev`，`release`→`--profile release`，`shipping`→ Viso 自有的 Cargo profile（`--config profile.shipping.inherits="release" --profile shipping`）；`VISO_PROFILE` 为该 profile，dev profile 带 `--features viso/hot-reload`（headless 与 device target 除外），headless 带 `--features viso/headless` 与 `--target <host triple>`（§3.4），device target 带其 triple、profile 的 feature（embedded profile 另带 `--no-default-features`）与 `VISO_FOOTPRINT`/`VISO_BUDGET`/`VISO_WORKERS`/`VISO_KMS_*`/`VISO_FONT_*`（§3.6）。Viso 设置以 `--config profile.<p>.opt-level|debug|strip` 交给 Cargo，只传与 Cargo 该 profile 默认不同、或被某一层显式命名的值（shipping 默认 `opt-level="s"`、`strip=true`）；web target 的 debug 随 web 子表、strip 交给 `wasm-bindgen`；
4. target：host/headless/device target 原生可执行文件（device target 不打包，`dist/<name>/` 中即该可执行文件）；`ios` 为 simulator triple 的 `.app`（ad-hoc 签名）；`android` 为 emulator ABI 的 `cdylib` 打成 APK（dev 为 dev APK，release/shipping 为 release APK，debug keystore 签名）；`web-gpu` 为 `wasm32-unknown-unknown` 经 §14.3 的 bundle；
5. 打包输出写入 `target/viso/build/<build-id>/out/`（不同 BuildId 互不覆盖），再整体复制（文件系统支持时 clone）为 `dist/` 下的同级临时目录，写入 `artifact.json`，以一次 rename 交换进 `dist/<platform>/`（macOS `renamex_np(RENAME_SWAP)`、Linux `renameat2(RENAME_EXCHANGE)`，不支持时先移开旧目录），旧输出随后删除——`dist/<platform>/` 只会是旧的完整输出或新的完整输出；
6. `artifact.json`：`project`、`version`、`target`、`profile`、`build_id`、`toolchain`（`rustc -V`）、`signing`（`none|ad-hoc|debug-keystore`）、`artifacts[{kind, path（相对 artifact.json 所在目录）, size, hash}]`；
7. Ctrl-C：杀掉 Cargo 与其下全部进程（及进行中的 `wasm-bindgen`/`wasm-opt`），删除 `out/`，`dist/` 不变，exit 130。

---

## 15. `viso serve`

只服务 Web target。

```bash
viso serve             # 使用 [web] default_target；未配置时为 usage error
viso serve web-dom
viso serve web-gpu
viso serve web-hybrid
viso serve web-gpu web-dom web-hybrid   # 一个 session，多条 web lane（§15.4）
```

非 Web positional target 是 usage error（exit 2）；同一 target 写两次亦然。

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
--no-hot-reload
```

默认只监听 loopback；非 loopback `--host` 必须同时给 `--lan`，否则 usage error（Viso_Hot_Reload.md §46）。

`serve` 与 `viso run web-*` 共用 Web Serve Service 与 Dev Session；区别是 `serve` 不启动浏览器开发 session（`--open` 只打开 URL），用于 LAN/外部浏览器访问。

已实现：`--lan` 不给 `--host` 时绑定 loopback 与本机通往网络的那个私有地址（10/8、172.16/12、192.168/16、100.64/10、169.254/16、fc00::/7、fe80::/10）；该地址不是私有地址时 exit 3，要求以 `--host <ip> --lan` 指明接口；`--host 0.0.0.0 --lan` 监听全部接口；`--lan` 配 loopback `--host` 是 usage error。每个请求的 `Host` 必须是 server 所绑定的 IP 字面量或 `localhost`（`0.0.0.0` 时任一 IP 字面量）且端口一致，否则 `421`，域名一律拒绝，以挡 DNS rebinding；WebSocket 的 `Origin` 必须等于 `http://` 加该 `Host`。页面的 dev channel 连接页面自己的来源地址，不再经 launch 变量给出地址。LAN 下 human 输出两个地址并警告：网络上任何设备都能加载页面、驱动该 session；web-gpu 另警告其他设备的浏览器只对 secure context 提供 WebGPU，纯 HTTP 页面不是。`--json` 的 `server` event 在 LAN 下给网络地址与 `lan:true`。

### 15.2 Port selection

若默认端口被占用：

- human mode：自动选择相邻空闲端口并提示；
- `--json`：同样自动选择，并为每条 web lane 输出一条 `server` event（`target`、`url`、`host`、`port`）；
- 显式 `--port` 被占用：报错（exit 3），不静默改端口；`[web.serve] port` 与默认端口从该端口起取后续 100 个中第一个在所有绑定地址上都空闲的。

`--json` 事件：`progress`、`diagnostic`、`artifact`、`server`、`dev`、`log`、`summary`。Exit code 同 §13.9（无 app crash 分支）。

### 15.3 Security headers

WebGPU/threaded WASM 所需 COOP/COEP 等 header 由 target/profile policy 决定，不能要求用户自己拼开发 server 配置。

当前实现：

- 每个响应带 `Content-Type`（`.wasm` 为 `application/wasm`）、`X-Content-Type-Options: nosniff`、`Cross-Origin-Resource-Policy: same-origin`、`Cache-Control: no-store`；
- 页面是 bundle 自己的 `index.html`（它给出按内容命名的模块与 glue，§14.3），launch 变量与页面 log 脚本插在 `</head>` 前；每个响应生成一个 nonce，页面的每个 `<script>` 都带上它，`Content-Security-Policy: script-src 'self' 'nonce-…' 'wasm-unsafe-eval'; object-src 'none'; base-uri 'none'; frame-ancestors 'self'`——只限制脚本来源，app 自己的网络请求、图片与字体不受限制；
- bundle 文件有 `.br`/`.gz` 副本时按请求的 `Accept-Encoding`（权重大于 0，`*` 亦可）优先 Brotli、其次 gzip 发送，带 `Content-Encoding` 与 `Vary: Accept-Encoding`；
- WebGPU 只要求 secure context，不需要 COOP/COEP；`Cross-Origin-Opener-Policy: same-origin` 与 `Cross-Origin-Embedder-Policy: require-corp` 随 shared-memory 线程模块（`[profile.<name>.web] threads`）开启，否则不发——COEP 会挡住 app 加载的跨源资源。按 lane 判定：bundle 的 `.wasm` 导入或定义 shared memory 的 lane，其所有响应带这两个 header，其他 lane 不带。

### 15.4 多条 web lane

`viso serve` 与 `viso run` 都可以带多个 web target（ADR 0046）：一个 session、一个 watcher、一个 source graph、一个 control endpoint、一个页面 server（一个端口）。

- 只有一条 web lane 时页面在 `/`，URL 与以前相同；
- 多条时每条 lane 在 `/<target>/`（`/web-gpu/`、`/web-dom/`、`/web-hybrid/`）；`/` 是列出各 lane 链接的索引页（`Content-Security-Policy: default-src 'none'`）；`/<target>`（无尾斜杠）`308` 到 `/<target>/` 并保留 query；其他路径 `404`；
- 页面的 dev channel 与页面 log channel 都开在页面旁边：`<lane>/__viso/dev`、`<lane>/__viso/page`（runtime 取 `location.pathname` 的目录，页面脚本按页面 URL 相对解析），server 据此知道每个 socket 属于哪条 lane；
- link 接受每条 lane 的 build id；patch 按 runtime 各自下发；一条 lane 的新 build 只让该 lane 的页面 reload；多 lane 时页面 log 行以 `[target] ` 开头；
- 每条 web lane 输出一条 human 行（多 lane 时为 ``[viso] serving `web-dom` at http://127.0.0.1:8000/web-dom/``）与一条 `server` event（`target`、`url` 含 lane 路径、`host`、`port`、`lan`）；LAN 警告只出现一次；
- `--open` 打开每条 lane 的页面。

当前 `web-dom`/`web-hybrid` 尚未可用（`require_available` 拒绝），多 lane 的端到端运行要等它们落地；路由、per-lane header 与 reload 由单元测试覆盖。

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

Formatter 基于 Lossless CST/AST，不使用正则批量重写。CLI 与 `viso-lsp` 调用同一个 `viso_dsl::format`（ADR-0018 修订）。无法解析（含词法错误）的文件输出其 syntax diagnostic，不改写。

- 无路径参数时格式化 project 的 `src/`、`tests/` 下全部 `.vs` 与 `Viso.toml`；给出路径时为这些文件与目录（目录取其下 `.vs`），可在 project 之外；名字相对 project root（无 project 时相对当前目录）。
- 只写出文本有变化的文件，经临时文件 + rename 原子替换，保留权限。
- human：stdout 每个改写的文件一行 `formatted <file>`（`--check` 为 `<file>: not formatted from line <n>`），stderr 末行 `<n> of <m> files formatted|to format[, <k> not parsed]`。
- 读不到文件 `ENV_SOURCE_UNREADABLE`、写不出 `ENV_SOURCE_WRITE`（exit 3）。
- `Viso.toml`（project 的 manifest，或路径中名为 `Viso.toml` 的文件）只规范化 TOML 不赋予意义、也不含注释的空白：`=` 两侧各一个空格、行尾无空白、尾随注释前一个空格、连续空行折为一行、文件以一个换行结束；键、值、顺序、全部注释、数组与 inline table 的排版、缩进原样保留。写出前校验新旧文本是同一文档（同样的键、顺序与值），否则不改写。无法解析为 `C0003`。

### 16.4 JSON / exit codes

`--json`：每个需要改写（`--check` 时为未格式化）的文件一条 `result`（`payload: {file, changed, line}`，`line` 为第一处不同的行），无法解析的文件输出 `diagnostic`，最后 `summary`。Exit code：`--check` 发现未格式化文件或存在 parse diagnostic 时 1（不带 `--check` 时 parse diagnostic 同样 1）。

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

当前实现：`viso check <target>`（`headless`、`ios`、`android`、`web-gpu`、`web-dom`、`web-hybrid`）按该 target 的构建解析配置（`Viso.toml`、`VISO_*`，target 不可用为 `C0011`，exit 3），并以该 target 设备具有的输入检查源码：desktop host 有键盘、鼠标与手柄，iOS、Android 与浏览器另有触摸，headless 只有键盘与指针（Input Action 缺少某设备的绑定为 `E9107` 警告）。不写 target 时为 host 加上 `Viso.toml` 配置的移动 target 与 device target 的设备。`viso check <device-target>` 按其 profile 检查：embedded profile 的板卡有触摸与按键、没有手柄，`desktop`/`mobile`/`web` 同对应 target；包内 shader 语言按 profile 固定（§3.6、§38.6）：`embedded-cpu` 仅 `None`，`embedded-gpu` 的 GLES 板卡为 GLSL ES、Vulkan 板卡（`gpu = "vulkan"`）为 SPIR-V，`web` 为 WGSL 与 `None`，`desktop`/`mobile` 同其他 target；device target 的包中某个 shader 在其语言下构建不出为 `E3711`（`build` 同此）。`--target <t>` 是 usage error，提示写成位置参数（`build` 同此）。`--a11y strict` 使 `E3704`/`E3708` 成为错误，`--i18n strict` 使 `E3705` 成为错误并覆盖字面量文本（DSL §U8.2、§U10）。

### 17.2 Fast default

`viso check` 不应该默认执行完整 package/signing。Rust 代码默认不检查；`--cargo` 以该 target 构建所用的 Rust target、features 与 profile 运行 `cargo check`（不产出可执行文件），编译器诊断作为 `diagnostic`（带 span）报告，失败为 `BUILD_FAILED`（exit 4）。

当前实现：`.vs` 包整体解析、类型检查，并且每个文件挂载的视图按其 `view!` 的方式降级，所以视图无法挂载之处（`E3711`，如尚无运行时路径的 Property；`E9106`）在构建前报告，位置指向该文件。

### 17.3 Watch

普通开发用 `viso run`。编辑器/CI 需要连续静态检查时可用 `viso check --watch`；每轮输出完整 diagnostic 集合后接一条 `progress{phase:"idle"}`，只有退出时输出 `summary`。

当前实现：watcher 与 `viso run` 的相同（`.vs`、`i18n/*.toml`、`assets/`、`Viso.toml`；`--cargo` 时另加 app 链接的 Rust 包）；一次保存写出的多个文件合并为一轮，每轮前有一条 `progress{phase:"check"}` 说明哪个文件变了。人类输出每轮末行为该轮的 `checked …` 摘要。Ctrl-C 结束，exit 130，`summary{status:"cancelled"}` 带最后一轮的计数。

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
  content     OptionalSlot<Node>  default
```

### 18.1 Query forms

```bash
viso schema Button
viso schema viso::widgets::Button
viso schema Button.text
viso schema Card                 # 项目组件
viso schema --search text
viso schema --list widgets       # widgets | components | natives | types
```

符号按完整路径或其任意 `::` 后缀匹配（`Button`、`text::upper`、`viso::time::Stopwatch`、`card::Card`）；后缀匹配多个符号时报告全部候选并 exit 1。`.member` 收窄到一个 Property、Event 或方法。可查询内置 Widget、Native Registry 中的 Library、函数和 Handle 类型，以及当前项目（`--project` 或向上找到的 `Viso.toml`；不在项目中时只查框架）声明的 Component、Template、Record 与 Enum：路径为 `<package>::<module>::<Name>`；Record 的字段与 Enum 的变体是其成员（`Point.x`、`Status.ready`）。

`--search TERM` 不区分大小写地列出路径包含 `TERM` 的符号与成员，每行一个路径及其类型或签名；无匹配时 exit 0。`--list GROUP` 列出一组的全部符号：`widgets`（框架 Widget）、`components`（项目组件及其声明位置）、`natives`（Native Library 与函数）、`types`（项目的 Record 与 Enum 及其声明位置，Native Handle 类型）。`--json` 下二者都输出一条 `result`，`payload: {schema_version, query | list, matches: [{path, kind, detail, origin}]}`，`origin` 为 `"framework"` 或项目声明的 `file:line:column`。

### 18.2 AI/tool use

```bash
viso schema Button --json
```

输出一条 `result` event，`payload` 为 `Viso_DSL_1.0.md` §139 定义的 schema object；CLI 不定义第二套 schema JSON。对象以 `schema_version`（当前 `1.0`，`viso --version` 的 `schema object`）开头：删除字段或改变字段含义时升级，新增字段不升级。Invalidation 名称只用 dirty class 规范名（`STRUCTURE STYLE MEASURE LAYOUT TRANSFORM PAINT HIT_TEST SEMANTICS`）。符号不存在时输出 `diagnostic` 并 exit 1。`tests/golden/schema.json.txt` 固定 Widget、成员、Native 函数与类型、项目组件和 `--list` 的 JSON。

### 18.3 Source origin

如果 schema 来自项目的 Component、Record 或 Enum，应返回：

```text
source file
source span
SymbolId
visibility
schema revision
```

JSON 中为 `origin` 对象：`{file, byte_start, byte_end, line, column_utf16, end_line, end_column_utf16, symbol_id, visibility, revision}`。位置是组件名的位置（`file` 相对项目根）；`visibility` 为 `public`（`export`）或 `private`；`revision` 是 schema 内容的 128 位摘要，只随公开面（Input、Event、Slot、Part、Capability、版本）变化。框架符号的 `origin` 为 `"framework"`。

项目组件的 `version` 是 `Viso.toml` 的 `[package] version`（未写时 `0.0.0`）；Input 的 `required` 为没有默认值，`default` 为默认值源码，`two_way` 为 `@bindable`；`invalidates` 是该 Input 在组件自身 View 中实际绑定到的 Dirty Class 并集——经 `computed`/`fn` 与内联子组件 Input 传递，控件取值按其 Property 计，决定 `if`/`for`/`match` Region 时含 `STRUCTURE`，View 未读的 Input 为空；Event 的 `payload` 为参数列表（`(value: String)`），不冒泡、不可取消。

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

当前实现：三处来源各自持有自己的码——编译器及其 Runtime 的 `E####` 在 `viso_dsl::diag::codes`（标题、解释、触发示例、修复与修复后的源码、建议命令，`docs/diagnostics.md` 由它生成），配置的 `C####` 在 `viso-project`（`ConfigCode` 的标题、解释、修复），CLI 自身的码（`CLI_*`、`ENV_*`、`BUILD_*`、`TEST_*`、`DEVICE_*`、`GAME_*`、`NEW_*` …）在 CLI 的码表中；`viso explain` 只查找与渲染，不需要项目。码不区分大小写。

```text
viso explain E3101          # 编译器码：解释、示例、修复与建议命令
viso explain C0004          # 配置码
viso explain ENV_LOCK_HELD  # CLI 码
viso explain --list         # 全部码与标题，reserved 码标出
```

`--json`：一条 `result`（`payload: {code, producer, severity?, stage?, title, explanation, example, example_kind?, fix, fixed, suggested_commands[], reserved?}`）+ `summary`；`producer` 为 `compiler`、`config` 或 `cli`，`severity`/`stage` 只有编译器码有（CLI 与配置码的 Severity 取决于触发处），`example_kind` 为 `source`（单个 `.vs` 文件，测试保证其编译产生该码）或 `text`（Manifest、热重载、运行时情形的示意）。`--list --json`：`payload: {codes: [{code, producer, severity?, title, reserved?}]}`。

未知 code：`EXPLAIN_UNKNOWN` 诊断并 exit 1，附编辑距离最近的码；`E0XXX` 形式的码是 Rust 编译器的，提示 `rustc --explain`。

诊断链接到解释：编译器或配置的 error 诊断若自身没有 fix（配置诊断：没有 note），附一条无 edits 的 fix `run \`viso explain <code>\``——human 输出为 `= help:` 行，event stream 中即 §36.1 的 fix，不另加字段；warning 与 registry 中没有的码不附。

---

## 20. `viso dump`

用于 compiler/runtime advanced diagnostics：把项目 package 的一种编译器 IR 打印成文本或 JSON。

```bash
viso dump ast src/app.vs
viso dump hir src/app.vs
viso dump ui-ir src/app.vs
viso dump reactive-ir Counter
viso dump behavior-ir Counter.increment
viso dump shader-ir RoundedRect
viso dump system-ir PlayerController
viso dump module-graph
```

`<kind>`：

| kind | 内容 |
| ---- | ---- |
| `ast` | 语法树（trivia 除外）：每个节点 `Kind start..end @line:col`，token 带文本 |
| `hir` | Typed HIR：component/system/template 的 input/state/computed/event/slot/callable，record/enum 的字段与变体，模块级 `fn`/`action`/`task`；每项带类型（callable 为源码签名）、effect、ownership、capabilities、reactive reads、常量值 |
| `ui-ir` | 文件挂载的 view 的 retained-tree 模板：节点按先序编号 `n0…`，折叠后的 style、绑定属性（值表达式与 dirty class）、control 读值、handler、look、role/live、`if`/`for`/`match` 区域，以及内联的 component instance |
| `reactive-ir` | 同一 view 的每个 reactive source 及其写入失效的 dirty class（决定区域的 source 含 `STRUCTURE`；未被读的标 `unread`），每条 binding edge（source → 节点.属性、dirty class、static/dynamic），`dynamic_fallback_nodes` |
| `behavior-ir` | 文件编译出的 Behavior IR：component 槽位、native import、每个函数的指令（JSON 中每条带 span） |
| `shader-ir` | 通过检查的 shader：binding、record、函数与 vertex/fragment 入口，表达式全部加括号、名字全部解析；有错误的 shader 标 `unchecked` |
| `system-ir` | system 按调度顺序：每个 hook 实现的 trait hook 路径与 domain、实现它的函数；每个 state 的 slot、domain（simulation/local/presentation）、`@probe`、`@persist` key；package 的 tick rate 与 game profile |
| `module-graph` | 每个模块的文件、import、native binding、顶层声明（含 export 标记），以及 graph 级诊断 |

`[<input>]`：以 `.vs` 结尾或含路径分隔符时为项目的源文件（相对当前目录或项目根），只 dump 该文件；否则是一个声明，按完整路径或 `::` 后缀（`Counter`、`demo::counter::Counter`），可带 `.member`，dump 声明它的文件并在该 kind 可收窄时只保留它。省略时 dump 每个文件。不是 package 源文件的路径为 `DUMP_INPUT`（exit 2）；不存在或有歧义的声明为 `E2001`（带最近候选），exit 1。

支持：

```text
--out <path>        # 写文件而不是 stdout；--json 时写 IR 的 JSON
--symbol <path>     # 在指定文件内只输出该声明
```

`ui-ir`/`reactive-ir`/`behavior-ir`/`shader-ir`/`system-ir` 以 `view!`、hot reload 与 build 相同的方式逐文件编译（同一 target profile）。

诊断：先报告 package 检查与被 dump 文件编译产生的全部诊断（同一诊断只报一次），再输出已 lower 的部分，所以无效文件仍有 dump；有 error 时 exit 1。

`--json`：每条诊断一个 `diagnostic`，然后一条 `result`（`payload: {kind, input, ir}`；`--out` 时为 `{kind, input, out}`）+ `summary`。`ir` 为 `{dump_version, sections: [{file, module, ir}]}`（`module-graph` 只有一个不带 `file` 的 section）；span 为 `{start, end, line, column_utf16}`（文件字节区间，起点 1-based 行列）。

文本形式供人与 golden test 阅读，可随版本变化；JSON 的 key 由 `dump_version`（`viso --version` 中的 `dump`）版本化，删除或改变含义时递增。`dump` 不属于普通应用 authoring API，但必须稳定到足以支持 compiler tests、Studio 和 AI debugging。

---

## 21. `viso lsp`

启动 Viso Language Server。

默认：

```bash
viso lsp --stdio
```

`viso lsp` 只定位并 exec `viso-lsp` binary（ADR-0018：同步 stdio `Content-Length` JSON-RPC loop），不在 CLI 进程内实现 language server。查找顺序：`viso` 可执行文件所在目录，然后 `PATH`；找到后以它替换当前进程（Windows 上为运行并等待），继承 stdin/stdout/stderr，`--stdio` 原样传给它，exit code 即 server 的 exit code。两处都没有或无法启动时为 `ENV_LSP`（exit 3，提示安装与 CLI 同版本的 `viso-lsp`）。

stdout 专属 JSON-RPC，任何日志只写 stderr；`--json` 不适用（§6.1），给出时为 `CLI_USAGE`（exit 2）；没有 `summary` event。

能力范围：

```text
已实现:
    publishDiagnostics, goto definition, find references, rename,
    formatting（与 `viso fmt` 同一 `viso_dsl::format`）,
    code actions（诊断的 fix）,
    hover（光标下诊断的 code：标题、解释与修复，来自 `viso explain` 读取的同一诊断 registry）

1.0 目标（ADR-0018 明确 deferred，需后续 ADR/修订）:
    completion, semantic tokens, schema lookup, source-to-generated mapping
```

Exit code：客户端 `shutdown`+`exit` 后 0；未收到 `shutdown` 即 `exit`、未收到 `exit` 即输入结束或 transport 损坏时 1（LSP 规范）。

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

当前实现：

- Scenario 是 `tests/ui/<scenario>.tape`，一行一步，`#` 起注释：

  ```text
  # 计数器每点一次加一。
  surface 390x844@3            # 可选头部：覆盖 [test.surface]
  settle 30                    # 可选头部：每步之前最多等待的帧数（缺省 120）
  expect "Count: 0"
  click button "Increment"
  expect "Count: 1"
  type "hello"                 # 提交文本到焦点节点
  key Primary+Shift+Z          # Primary 在 Apple 上是 Command，其他平台是 Control
  scroll "List" 0 240          # 在节点上方滚动（逻辑像素）
  hover "Help"
  wait 30                      # 让 30 帧运行
  expect no "Error"
  expect checkbox "Agree" checked
  expect slider "Volume" value 0.5
  expect snapshot "home-wide"  # 截取此刻的帧，与 golden 比较（§23）
  ```

  节点由 selector 找到：`"<label>"` 或 `<role> "<label>"`——accessible label 完全相等（role 为 `.vs` 的 `Role` Variant：`button`、`check_box`、`slider`、`label`、`text_field`……，也接受 inspector 的写法 `checkbox`、`textfield`），或括号中的 inspector selector（§24.3）：`click (Column > #save)`、`expect ([role=check_box][checked])`；取当前显示（不在隐藏子树中、有面积）的第一个（树序）；使用 inspector selector 的 Scenario 运行时得到包的 source table（`VISO_HEADLESS_SOURCES`），对窗口的 census 求值；状态为 `focused`、`checked`、`unchecked`、`selected`、`expanded`、`collapsed`、`value <number>`；键名为字母、数字、`F1`–`F12` 与 `Enter`、`Escape`、`Tab`、`Space`、`Backspace`、`Delete`、方向键、`Home`、`End`、`PageUp`、`PageDown`，修饰键 `Shift`、`Ctrl`、`Alt`、`Logo`、`Primary`；
- 运行：先解析全部 Scenario（解析失败即该用例失败，`TEST_INPUT_INVALID` 指向其行，不构建；两个 Scenario 取同名 snapshot 亦然），再以 headless 构建 app（§3.4），每个 Scenario 一个独立进程、一个独立的新目录（`target/viso/test/<pid>/ui/<name>/`，进程的工作目录），并行至多 `--jobs` 个；surface、Scenario 路径、报告路径与 snapshot 目录经 `VISO_HEADLESS_SURFACE`、`VISO_HEADLESS_SCRIPT`、`VISO_HEADLESS_REPORT`、`VISO_HEADLESS_SNAPSHOTS` 传入；
- 回放：针对第一个窗口；每步在 app 安定（无动画、无待提交文本）或已过 settle 帧数后执行；输入经 platform 送入，与真实设备相同（点击为指针移到节点中心、按下、松开）；`expect` 读该窗口的 semantics；每步结果写成报告的一行（JSON），第一个失败的步骤结束该次运行，全部结束后关闭窗口；
- Snapshot：`expect snapshot "<name>"`（名字为字母、数字、`_`、`-`，至多 100 字符，同一 Scenario 内不重复）在安定后从 software raster 读回该窗口最后一帧，写 `<name>.png` 与 `<name>.tree`（tree、layout、paint、batches 与计数的稳定文本），报告一行 `snapshot`；没有 snapshot 步骤的 Scenario 若有以其名命名的 golden，则在末尾取一张同名 snapshot。每张 snapshot 按 `viso snapshot compare`（§23）与 golden 比较：缺 golden `TEST_SNAPSHOT_MISSING`、不一致 `TEST_SNAPSHOT_MISMATCH`、golden 不可读 `TEST_SNAPSHOT_INVALID`，均指向该步骤行并使用例失败（exit 1）；`--update-snapshots` 时，走完全部步骤的用例把缺失或不一致的 golden 写回（每个文件一条 `artifact`），用例通过；读帧或写文件失败 `RUN_SNAPSHOT_FAILED`（exit 5）；
- 结果：报告以全部通过结束且每张 snapshot 与 golden 一致为 pass；有失败步骤为 fail，`TEST_EXPECTATION` 指向该行（message 列出窗口当前显示的 label）；进程在报告结束前退出为 runner 崩溃（`RUN_TEST_CRASHED`，exit 5）。失败用例的输出（stdout/stderr）随后显示，`--nocapture` 时通过的也显示；通过用例的目录随即删除。

UI Input Tape（`viso_dev_protocol::tape::UiTape`，版本化 Ende 文件）记录第一个窗口的原始输入：启动环境以 `VISO_RECORD_TAPE` 给出路径时，dev 与 headless 构建的 runtime 记录该窗口运行的每一帧——帧时钟前进的时间（µs）与该帧之前到达窗口的输入（指针、滚轮、按键、提交的文本、IME 组字、粘贴、复制/剪切、窗口焦点、尺寸与缩放变化，坐标为逻辑点），以及开始时的 build id 与 dev revision、每次 live patch 落在的帧与它移到的 revision；窗口关闭或会话结束时写出。headless 构建以 `VISO_HEADLESS_TAPE` 给出 Tape 时回放它（与 `VISO_HEADLESS_SCRIPT` 互斥，exit 2）：未给 `VISO_HEADLESS_SURFACE` 时用录制时的 surface，每帧的输入在该帧之前经 backend 队列送入，帧时钟固定为录制的时间，Tape 结束后每帧前进 1/60 s 直到安定；`VISO_HEADLESS_REPORT` 给出时写一行 `{"kind":"tape","frames","build","revisions"}`。Tape 不携带代码：回放运行所给的 build，报告录制跨过的 revision。

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
viso test game movement --tape runs/jump.tape        # 回放 Tape，对 @probe 断言
viso game record movement --output runs/jump.tape    # 运行中录制 Input Tape
viso game peek movement Player.score                 # 读取运行中 System State
viso game replay movement --seconds 3                # 在 reload 后的代码上重跑最近 3 秒
```

输出包含每 Tick `@probe` Trace、最终 Snapshot Hash、Entity Snapshot，以及可选的 Headless 帧截图 Sheet（`--sheet <png>`）。同一 Build + Tape 在所选 determinism 档位下 Snapshot Hash 逐字节一致（DSL §110.5）。

当前实现：

- Scenario 是 `tests/game/<scenario>.tape`：Input Tape 的文本形式（DSL §110.5）加上期望行 `<tick>: expect <System>.<state> == <JSON 值>`，在该 Tick 运行之后检查，所指须是 `@probe` 状态；省略 `<scenario>` 时按名字顺序运行该目录下全部 `.tape`；
- `--tape <path>` 用给定 Tape（二进制或文本）替换 Scenario 的输入、保留其期望；单独使用时以文件名为 Scenario 名、无期望；`--seed` 覆盖 Tape 的 Seed（缺省 Seed 为 `0x5eed5eed5eed5eed`）；`--frames` 缺省为 Tape 覆盖的 Tick 数与最后一个期望之后的较大者；一帧运行一个 Fixed Tick，再以一个 Tick 的时长运行全部 `FrameUpdate`；Tick Rate 与 determinism 档位取自 `Viso.toml [game]`；
- `--json` 下每个 Tick 输出一条 `trace`（`scenario, tick, probes{"System.state": 值}`），每个 Scenario 一条 `test`（§36，另带 `seed, frames, build, snapshot_hash`（16 位十六进制）、`entities[{id[index, generation], kind, tags, model, position, half_extents}], failures, faults`）；未满足的期望是指向 Scenario 行的 `TEST_EXPECTATION` 诊断，System 故障以其故障码报告；人类输出每个 Scenario 一行 `test game <name> ... ok|FAILED (<n> frames, seed <s>, snapshot <hash>)`，末行 `test result: …`；
- 任一 Scenario 失败退出 1，Scenario 或 Tape 文件缺失或不可读退出 3；
- `viso game peek <game> <System.state>` 与 `viso game record <game> --output <path> [--seconds S | --ticks N]`（缺省 10 秒）询问本 project 正在运行的 `viso run` session 的 app：`viso run` 另开一个 loopback control endpoint，把 `<地址> <token>` 写入 dev session lock（§44，第 4 行）；`viso game` 读 lock、连上 endpoint、带 token 发出请求，token 不符的连接不回应。`<game>` 按 app 挂载的 `.vs` 文件选取：相对 project root 的路径（可省 `.vs`）或文件名 stem，多个匹配时 `GAME_AMBIGUOUS`（exit 2）。`viso run` 经 dev channel 转给 runtime（`HostMessage::Game` / `RuntimeMessage::Game`，`Viso_Hot_Reload.md` §34），由运行该文件的第一个 Game Session 在帧边界回答；
- `peek` 可读 System 的任意 State（不限 `@probe`），host 按该文件 last-good 的类型写成 JSON（无 last-good 时按结构）；人类输出 `<System>.<state> = <JSON> (<file> at tick <n>)`，`--json` 为一条 `result`（`game, system, state, value, tick, sessions`）；
- `record` 先以 `Rebuild::Fresh` 重启该游戏的 World（时钟回到 tick 0、以其 seed 重跑 start），再录制所要求的 Tick 数（`--seconds` 按游戏 Tick Rate 向上取整），录满后 runtime 回传 Tape：未经 live patch 的写成文本形式（可直接作 `viso test game --tape` 输入或补上期望行成为 Scenario），带 live patch 的写成二进制形式（只有二进制携带 patch）；人类输出 `recorded <n> ticks of <file> to <path> (text|binary[, n live patches])`，`--json` 为一条 `result`（`game, path, form, ticks, seed, patches`）。游戏暂停时录制随之暂停；
- `viso game replay <game> [--seconds S]`：dev session 中的每个 Game Session 自启动起保留最近 `DEFAULT_HISTORY_SECONDS`（10 秒）的历史（DSL §110.4）；replay 让运行该文件的每个 Session 各自回到不晚于「当前 Tick − S 秒」的最近 Snapshot，用记录的输入在当前代码上重跑到原 Tick，省略 `--seconds` 或超出历史时从最旧 Snapshot 起；任一 Session 尚无历史时都不回放。Logic / Logic+State Migration 层的 game patch 提交行末尾提示 `(replay: viso game replay <file>)`。人类输出 `replayed <n> ticks of <file> from tick <t>`，`--json` 为一条 `result`（`game, from, ticks, sessions`）；
- 失败码：没有 session 或连不上 `ENV_NO_DEV_SESSION`（exit 3）；文件未挂载、app 未连上或不运行游戏 `GAME_NOT_RUNNING`（3）；没有该 System/State `GAME_UNKNOWN_STATE`（1，note 列出已有 State）；正在回放 Tape 或已在录制 `GAME_BUSY`（3）；尚无可回放的历史 `GAME_NO_HISTORY`（3）；World 重启时故障 `GAME_FAULT`、录制完成前游戏结束 `GAME_ENDED`（5）；Tape 超过 dev frame 上限或回答不可解码 `GAME_ANSWER_INVALID`（7，协议违约）；写 Tape 失败 `ENV_TAPE_WRITE`（3）；
- `--sheet <png>` 把每个 Scenario 的运行画成一张帧图：CLI 以 headless 构建 app（同 `viso test ui`），把包的 behavior module、Scenario 的 Tape 与帧数写成 `GameSheet` 请求，经 `VISO_HEADLESS_GAME_SHEET` 交给该构建；运行不挂载任何 View，以同一 Seed、同一 Tape、每帧一个 Fixed Tick 加全部 `FrameUpdate` 重跑游戏，在均匀分布、止于最后一帧的 12 帧上按 `GameViewport` 的画法（Stage 的相机、Kit 模型、粒子、调试形状）经 software raster 画成 480×270 的 tile，左上角印该 Tick，按每行 4 个排成一张 PNG。多个 Scenario 时各写 `<stem>-<scenario>.png`。每张图一条 `artifact`（kind `sheet`），`test` 事件另带 `sheet{path, ticks}`；人类输出一行 `sheet <path> (ticks …)`。构建失败按构建的 exit code；运行未写出图 `RUN_SHEET_FAILED`（exit 5），并显示运行的输出；`--sheet` 只用于 `game` 域，其他域为用法错误。

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

当前实现：

- 省略测试域即 `all`：依次 `unit`、`ui`、`game`、`web`；某域在此无可运行的内容（没有 `Cargo.toml`、没有 `tests/ui` 或 `tests/game`、`web`）时以该域名记一个 `skip` 用例并给出原因；显式指定的域无内容时为 `ENV_TEST_INPUT` / `ENV_CARGO_MANIFEST` / `ENV_TEST_TARGET`（exit 3）；
- `unit`：`cargo test`（带 `--no-fail-fast`，`--fail-fast` 时去掉），`--filter`/`--exact`/`--nocapture` 交给 test harness，`--jobs` 同时作为 `--test-threads`；编译诊断与 `viso build` 相同，harness 报告的每个测试一个用例（`ignored` 为 `skip`），失败用例带其输出；编译失败 `BUILD_FAILED`（exit 4）；
- `ui` 在 `headless`、`ios`、`android` 上运行，其他 target 为 `ENV_TEST_TARGET`（exit 3）；`web` 尚未实现（`ENV_TEST_TARGET`）；`--target` 不用于 `unit`/`game`（usage error）；`--frames`/`--seed`/`--tape` 只用于 `game`，Scenario 名只用于 `ui`/`game`；
- `--filter <pattern>` 按名字包含（`--exact` 时相等）筛选每个域的用例；`--fail-fast` 在第一个失败用例后停止（进行中的 ui 用例被终止、不报告）；
- `--update-snapshots` 只用于 `ui`（及省略测试域时的 `ui`），见 §22.2；`--device` 只用于 `--target ios|android`（否则 usage error），省略时按 `viso run` 的规则选 profile（本地默认、唯一 profile、TTY 询问）；
- `ui --target ios|android`：每个用例在设备上全新启动一次 app——以本可执行文件运行 `viso run <ios|android> --device <id> --json` 并读它的事件；app 连上 dev 通道且报告完 mounts 后，Scenario 经该 session 的 control endpoint（`ControlAsk::Scenario`）交给 app，由 dev runtime 在第一个窗口上逐步执行（输入以平台事件 post 进 scheduler，与用户输入同一路由；期望读 semantics；inspector selector 用 session 给的 source table），每步结果以 `RuntimeMessage::Scenario` 报告行（与 headless 报告同形，多一种 `skip`）回传，结束后以 Ctrl-C 结束该 session。snapshot 步骤只在 headless 拍摄，设备上记为 skip 并输出一行说明；component 用例（单独挂载组件）只在 headless 运行，记为 skip。session 在 app 连上之前结束（构建、设备或 SDK 失败）时，该用例以 session 最后的诊断失败，域以 session 的 exit code 结束，不再继续后面的用例；app 在 Scenario 结束前退出为 `RUN_TEST_CRASHED`（exit 5）；Ctrl-C 结束当前 session，exit 130；
- `ui --target web-gpu`：同一流程，session 为 `viso run web-gpu --browser none --json`；收到 `server` event 后以 headless 模式（`--headless=new --enable-unsafe-webgpu`，每个用例一个临时 profile）打开该页面，用例结束时关闭浏览器。浏览器取 `VISO_CHROME` 指定的 Chromium 系浏览器，否则依次查找 Chrome、Chromium、Edge 的默认安装位置与 `PATH`；找不到时每个用例记为 skip 并说明原因，域不失败；`web-dom`/`web-hybrid` 仍为 `ENV_TEST_TARGET`；
- 人类输出每个用例一行 `test <domain> <name> ... ok|FAILED|skipped`（被跳过的整个域为 `test <domain> ... skipped (<原因>)`），末行 `test result: ok|FAILED. <n> passed; <n> failed; <n> skipped`。

### 22.5 JSON / exit codes

`--json`：`progress`、`diagnostic`、每个用例一条 `test`（`payload: {name, domain, status: pass|fail|skip, duration_ms, message?}`）、`summary`（`test_counts: {pass, fail, skip}`）。Exit code：有失败用例 1；source diagnostics 1；target/device 不可用 3；构建失败 4；runner 崩溃 5。

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

### 23.6 当前实现

- Snapshot 是 `tests/ui/` 下 Scenario 的 `expect snapshot "<name>"` 步骤（§22.2）；没有 snapshot 步骤的 Scenario 若有以其名命名的 golden，或被 `capture` 按名指定，则在末尾取一张同名 snapshot。`<scenario>` 选一个 Scenario，省略时为全部；两个 Scenario 取同名 snapshot 时后者 `TEST_INPUT_INVALID`；任一 Scenario 解析失败即在构建前结束（exit 1）；
- 不是 Scenario 名的 `<name>` 指组件（`Name` 或 `module::Name`；多个文件声明同名组件时取 export 的，仍不唯一则 `TEST_INPUT_INVALID` 并列出各自的 module path；都没有则 `ENV_TEST_INPUT`）：CLI 把该组件所在源文件加一个只含 `Name {}` 的 harness 组件，按 app 的 view 同样的 profile（grants、i18n catalog、assets）编译成 view package，headless run 由 `VISO_HEADLESS_VIEW` 读入并把它单独挂到第一个窗口（无 caption、不跑 app 自己的 view，但 app 的字体与 assets 照常），在 `[test.surface]` 上取一张以组件名命名的 snapshot。Input 取各自默认值；有无默认值的 input 时在构建前 `TEST_INPUT_INVALID`（exit 1，指向组件声明行）。其 golden 的 sidecar 多一个 `component`（module path），省略 `<name>` 的 `compare`/`update` 据此连同 Scenario 一起重取；组件已不存在的 golden 在 `compare` 为 fail、`update` 为 warning；
- Golden 为 `tests/snapshots/<name>.<target>.png`（8-bit RGBA，自适应行滤波 + deflate）与 sidecar `<name>.<target>.json`：`snapshot_version`（`viso --version` 报告）、`name`、`target`、`renderer`（`software`）、`surface`（`WxH@S`）、`size [w, h]`（device pixel）、`tree_hash`（tree 文本的 `sha256:`）、`build_id`、`tolerance {threshold, pixels}`（缺省 `0.05`、`0`），一键一行；写回时保留原 sidecar 的 `tolerance`；
- 比较：尺寸不同即不一致；字节相同即一致；否则逐像素计算合成到白底后的 YIQ 感知距离（`0`–`1`），距离高于 `threshold` 的像素多于 `pixels` 即不一致。不一致时在 `target/viso/snapshots/` 写 `<name>.<target>.png`（本次）、`.tree`（本次的 tree）与 `.diff.png`（golden 淡化为浅灰，超阈值像素红、低于阈值的差异黄、只一边有的区域品红）；
- `capture`：走完全部步骤的 Scenario，其每张 snapshot 无条件写成 golden（`--output <dir>` 时写入该目录），每个文件一条 `artifact`（`kind: snapshot|manifest`）；
- `compare`：每张 snapshot 一条 `test`（`domain: snapshot`；附 `golden`，不一致时另附 `diff, actual, tree, same_size, differing_pixels, largest_distance, tree_changed`），并以指向步骤行的 `TEST_SNAPSHOT_MISMATCH` / `TEST_SNAPSHOT_MISSING` / `TEST_SNAPSHOT_INVALID` 诊断说明；未取到（Scenario 先失败）的 snapshot 记 fail；省略 `<scenario>` 时，没有 Scenario 取的 golden 亦为 fail（`TEST_SNAPSHOT_MISSING`）；末行 `test result: …`；
- `update [--filter <pattern>]`：只写回缺失或不一致的 golden（名字包含 pattern 的），每个写回文件一条 `artifact`；没有 Scenario 取的 golden 为 warning；人类输出末行 `<n> snapshots updated; <m> unchanged`；
- Scenario 的失败步骤 `TEST_EXPECTATION`（exit 1）、snapshot 步骤本身失败 `RUN_SNAPSHOT_FAILED`（5）、runner 崩溃 `RUN_TEST_CRASHED`（5），该 Scenario 不写 golden；没有 Scenario 取 snapshot `ENV_TEST_INPUT`（3）；`--target` 非 `headless` `ENV_SNAPSHOT_TARGET`（3）；写 golden 或比较结果失败 `ENV_SNAPSHOT_WRITE`（3）。

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

不带子命令时 `viso inspect [--target <t>] [--device <id>] [--attach <session>] [--run]` 附加到 Dev Session：先报告 app（`--json` 一条 `inspect{kind:"attached", windows, nodes, inspector, unavailable}`，human 一行），再像 `viso inspect events` 一样跟随首个窗口直到中断（Ctrl-C 结束跟随，exit 0）或 session 结束；`--attach` 按 `viso run` 在 `dev` 事件中报告的 session ID（或其前缀）选择 session，与 `--target`/`--device`/`--run` 互斥，ID 写在 session lock 中，没有匹配为 `ENV_NO_DEV_SESSION`、多个匹配为用法错误；`--run` 先以同一 project 启动 `viso run [<target>] [--device <id>]`（其输出写到 `target/viso/dev/inspect-run.log`），等它的 app 连上（至多 15 分钟）后附加，跟随结束时像 Ctrl-C 一样停止该 session；session 先于 app 连上就结束为 `RUN_APP_FAILED`（附日志末尾）。

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

### 24.3 Query

不打开 GUI：

```bash
viso inspect query '#save_button' --json
```

用于 AI/CI automation。`--json`：一条 `result`（`payload: {selector, matches: [{node_id, symbol, source_span, bounds, semantics}]}`）+ `summary`。

`inspect` 可用 `--target <t> [--device <id>]` 选择 Dev Session。Exit code：找不到活跃 Dev Session 3；selector 语法错误 2；零匹配不是错误（exit 0）。

当前实现：

- Selector：以 `,` 分隔的若干链，链由 compound 以空格（后代）或 `>`（子节点）相连；compound 为类型（`Button`、组件名，`module::Component` 限定模块）或 `*`，后接任意个 `#name`（`node name: Type { … }` 的名字）、`.Style`（`styles` 应用到该节点的 Style）、`[attribute]` 或 `[attribute=value]`（value 为名字、数字或带 `\"`/`\\` 转义的引号字符串）。类型匹配该类型的 Widget，或该组件的根节点（View 内联其组件，组件即其根节点）。Attribute：`role`（`.vs` 的 Role Variant 名）、`label`、`hint`、`value`、`checked`、`selected`、`expanded`、`focused`、`focusable`、`shown`、`kind`（`container`、`text`、`image`、`path`、`shader`、`asset`）；单独的 `[attr]` 在值存在且非 `false` 时成立。语法错误为 `INSPECT_SELECTOR`（exit 2），note 中以 `^` 指出出错的字节；在找 session 或构建之前报告；
- 两半：运行时给出 census（每个窗口按先序的全部节点：父节点、绘制种类、窗口坐标的 box、是否显示、焦点、semantics，以及由 `.vs` View 挂载的节点在该 View 中的来源——静态序号，或挂载它的 region/arm/item），工具给出 source table（每个可挂载 `.vs` 文件编译出的每个节点：类型、名字、Style、以它为根的组件、声明它的组件与声明的 span），两者按文件与序号连接后求值 selector。`viso_view::inspect` 是 CLI、UI Scenario 与 Studio 共用的这一服务（census、source table、连接、selector），`viso_dsl::inspect` 从编译结果生成 source table；census 只在工具询问时生成，不在 release 的帧路径上；
- Dev Session：经 `viso run` 的控制端点询问（与 `viso game` 同一端点），session 把请求经 dev 通道转给首个连接的 runtime，runtime 在下一帧边界生成 census（以 blob 发送），session 用它为各挂载规划编辑所用的编译结果生成 source table，一并回答；不带 `--device` 时取该 target 唯一的 session，有多个时为用法错误并列出各 device；`--target headless` 以 headless 构建 app（同 `viso test ui`），先播放 `--scenario <name>`（`tests/ui/<name>.tape`，否则只等 app 安定），在 Scenario 结束时由运行写出 census（`VISO_HEADLESS_CENSUS`），source table 由包的源码编译；Scenario 失败为 `TEST_EXPECTATION`，运行崩溃为 `RUN_TEST_CRASHED`；
- 结果：`--json` 一条 `result`：`{selector, matches: [{node_id, window, type, name, styles, components, symbol, source_span, bounds, kind, shown, focusable, focused, semantics}]}`，`node_id` 为 `<index>:<generation>`，`symbol` 为声明它的组件路径加节点名（无名时为类型）：`home::Home::save`，`source_span` 与诊断的位置同形（`file`、`byte_start`、`byte_end`、`line`、`column_utf16`、`end_line`、`end_column_utf16`），`bounds` 为 `{x, y, width, height}`，`semantics` 为 `{role, label, hint, value, heading_level, checked, selected, expanded, range_value, range}` 或 `null`；不由 View 挂载的节点 `type`、`name`、`symbol`、`source_span` 为 `null`；人类输出每个匹配一行（`Button#save.Primary  src/home.vs:16:13  [x, y w×h]  button "Save"`），末行为匹配数；census 或 table 无法解码、超出控制链路帧上限为 `INSPECT_ANSWER_INVALID`（exit 7）。

### 24.4 Tree, node and frame

```bash
viso inspect tree [--target <t>] [--device <id>] [--scenario <name>]
viso inspect node <selector|index:generation> [--target <t>] …
viso inspect frame [--window <n>] [--target <t>] …
```

与 `query` 同一来源（Dev Session 的控制端点，或 `--target headless` 的 headless 运行），同一 census 与 source table 的连接。Runtime 在 hello 中报告其 Inspector 能告诉什么（`tree`、`nodes`、`frame`、`gpu_time`、`events`）；每个结果附 `inspector`（报告的能力）与 `unavailable`（其余）。所需能力不在其中为 `INSPECT_UNSUPPORTED`（exit 3）。

- `tree`：每个窗口按先序的全部节点，人类输出按深度缩进，每节点一行（同 `query` 的行），末行为节点数；`--json` 一条 `result`：`{windows: [{window, nodes: [<query 的 match 字段>…, parent]}], inspector, unavailable}`，`parent` 为父节点的 `node_id` 或 `null`；
- `node`：selector（或 `<index>:<generation>`）所选节点，再向 app 询问它们的 detail（`InspectAsk::Nodes`，按窗口分批，一批至多 4096 个；headless 运行在结束时写出窗口全部节点的 detail，`VISO_HEADLESS_DETAIL`）。每个节点：`layout` / `style` / `content` 三组解析后的属性（如 `kind`、`axis`、`gap`、`padding`、`width`、`height`；`background`、`radius`、`opacity`、`hidden`、`hit_test`；内容种类、`font_size`、`color`、`soft_wrap`、`tint`），`world`（窗口坐标）与 `local` box，`transform_chain`（有平移或滚动的祖先，自根向下），`dirty`（当前 dirty class），`states`（hovered、pressed、focused…），`event_route`（自根向下有 pointer / key handler、可聚焦或自身禁用输入（`enabled: false`，`disabled`）的节点），`bindings`（使它失效的 cell：source table 给出的属性名、state 名、类型与声明位置，cell 的当前值，它失效的 class，是否 dynamic 边），`paint`（其在 paint stream 中的 span、它自己绘制的 primitive 种类，与这些 primitive 落入的 draw batch：按提交顺序的 batch 序号与各自的 draw 数，被裁剪或不产生 draw 的不计，序号即 `inspect frame` 的 batch 序号；无 renderer 时为空）。人类输出每节点一行加缩进的各项；census 之后已卸载的节点 `detail` 为 `null`（人类输出 `gone since the census`）；
- `frame`：窗口 `--window`（默认 0，不存在为用法错误）最后一帧：`counters`（nodes、primitives、draw_calls、batches、instances、upload_bytes、culled_primitives 等），`batches`（提交顺序：pipeline、`start`/`count`、clip、offscreen、cost），`damage`（该帧 present 重画的区域，§30.3：`null` 未重画、`"whole"` 整帧、或若干不相交矩形；Studio/Inspector 据此叠加 damage rect），`resources`（`group` 为 `gpu`：pipelines、buffers、gradient LUT、mask pages、offscreen targets、各 glyph atlas pool；`cache`：fonts、shaped runs、atlas 中的 glyph、asset decode/upload 计数；`asset`：每个资源的状态 `wanted` / `encoded` / `decoded WxH` / `resident WxH` / `vector WxH`；附 `bytes`、`budget`、`note`）；
- detail 在 runtime 的下一帧边界由该窗口最后一帧留下的状态生成，只在工具询问时分配，不在帧路径上；解码失败为 `INSPECT_ANSWER_INVALID`（exit 7）。

### 24.5 Events

```bash
viso inspect events [--window <n>] [--seconds <s>] [--target <t>] [--device <id>] [--scenario <name>]
```

跟随窗口中发生的事：每个输入事件（`pointer_down`/`pointer_up`/`pointer_leave`/`pointer_cancel`、`key_down <Key>`/`key_repeat`/`key_up`、`text`、`paste`、`ime_preedit`、`scroll <dx> <dy>`；pointer move 不记，它造成的 hover 变化以 dirty 记）及它经过的节点（自根向下），焦点移动，以及每个被直接标记 dirty 的节点、class 与原因：`input`、`state`（state cell、source table 给出的名字、类型与声明位置，改变后的值）、`computed`、`animation`、`task`（timer 或 task 完成）、`reload`、`text`、`asset`、`window`（尺寸、scale、环境）、`layout`（virtual list、adaptive scope）或 `other`；向祖先冒泡的 class 由树决定，不记。

- Runtime 在首次被询问（`InspectAsk::Events{window, after}`）时为该窗口开 journal（`viso-ui` 的 `inspect` feature：每个直接 mark 记一次，同节点同原因的连续 mark 合并；不开时没有 journal、mark 不记录），事件从 1 编号，有界（8192 个，丢弃最旧的，丢弃部分在 `dropped` 中报告）；约 1200 帧没有询问后 journal 关闭；
- Dev Session：每 100 ms 询问一次 `after` 之后的事件，直到 `--seconds`（省略为直到中断）或 session 结束；事件中出现 census 中没有的节点时（至多每 500 ms）重新取 census 与 source table 以命名；
- `--target headless`：运行从开始就保留 journal（65536 个），Scenario 结束时写出（`VISO_HEADLESS_EVENTS`），报告全部事件；不接受 `--seconds`；
- `--json`：每个事件一条 `inspect`：`{kind:"input", seq, frame, window, input, at:[x,y]|null, target, route:[node…]}`、`{kind:"focus", …, from, to}`、`{kind:"dirty", …, node, classes, cause:{kind, cell?, state?, type?, value?, declared?}}`，node 为 `{node_id, label}`（label 如 `Button#save.Primary`，不由 View 挂载的节点为其绘制种类）；丢弃时一条 `inspect{kind:"dropped", window, count}`；最后一条 `result{events, ended}`（`ended` 为 session 结束的原因或 `null`）。Human 每个事件一行：`#12    f3     input  pointer_down (60, 20) → Button#tap 6:0`、`#14    f4     dirty  Text#count 5:0  measure, layout, paint, semantics  ← state taps = 2  src/app.vs:2:5`、`#15    f4     focus  none → TextInput#name 9:0`，末行为事件数；
- runtime 不报告 `events` 能力为 `INSPECT_UNSUPPORTED`（exit 3）。

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

每帧采样 UI 线程上各 span 的耗时、GPU 耗时与该帧的计数（`viso_view::profile`，CLI 与 Studio 共用）：

```text
frame               帧总耗时（从输入分发到 present 返回）
input               输入路由与 handler
flush               state flush：reload、动画、timer、task、binding/effect settle、asset
text                text commit/edit/shaping/reflow
layout              dirty 子树的 measure 与 layout（含 adaptive settle）
style               interaction style 解析
paint               dirty 子树 paint 为 primitive
semantics           semantics tree 更新
upload              batching 与 GPU upload
submit              encode 与 present（含等待 drawable）
gpu                 GPU 执行该帧的时间（Metal 以 command buffer 的 GPU 起止；Vulkan、D3D12 以 timestamp query 记该帧 recording 的开始与最后一个 pass 的结束；WebGPU 以各 pass 的 `timestampWrites`，仅当浏览器提供 `timestamp-query`，其精度由浏览器决定；headless 为软件 raster 的 CPU 时间）

nodes               全部窗口挂载的节点
dirty_nodes         被某个改动 invalidate 的节点
measured / laid_out 重新 measure / 重新放置的节点
painted             paint 重建的 primitive
reactive_evals      static binding 求值
dynamic_evals       dynamic binding 求值
dynamic_fallbacks   走 dynamic 依赖边的节点
primitives / draw_calls / batches / instances / upload_bytes
damage_px / damage_rects 该帧重画的像素与矩形数（无变化的帧为 0、整帧重画矩形数为 0，§30.3）
full_frame_fallbacks 本可局部重画却整帧重画的帧（damage 超过大半个 surface、drawable 内容未知）
shaped_runs / glyph_rasters / glyph_upload_bytes
glyph_cache_bytes / atlas_pages
resource_loads      解码的 asset
allocations         app 装了计数 allocator 时该帧的堆分配次数（所有线程）
allocated_bytes     这些分配申请的字节
```

Summary 中 span 按全部帧统计（帧没有运行该 span 记 0）；`gpu` 与计数只按报告了它的帧统计。没有任何帧报告的指标列入 `unavailable`。Runtime 不装 allocator；要计分配，app 在自己的 crate 里声明 `#[global_allocator] static ALLOCATOR: viso::alloc::Counting = viso::alloc::Counting::system();`（或以 `Counting::new(inner)` 包住已用的 allocator），每次分配多两次 relaxed atomic add；没装时 `allocations`、`allocated_bytes` 列入 `unavailable`。p50/p95 为 nearest rank。

### 25.3 Trace output

内部 canonical trace：

```bash
viso profile --output trace.ende
```

可选外部互操作：

```bash
viso profile --chrome trace.json
```

Ende trace（`viso_view::profile::Trace`）：protocol tag + `trace` + u16 版本，之后为 `app`、`target`、`device`、`started_ms`（UTC 毫秒）、`nanos`（采集时长）、`dropped`、每帧 `{frame, at, nanos, spans[9], gpu, counts[21]}`（时间为 ns，`at` 相对首帧，缺失的 span/计数以位掩码省略）。不带 `--output` 时写到 `target/viso/traces/profile-YYYYMMDD-HHMMSS-mmm.ende`（UTC），只保留最新 20 个。Chrome trace 为 trace-event JSON：每帧一个 `frame` X event、每个 span 一个 X event（tid 1 "UI"），计数与 `gpu_ms` 为 C event，可在 Perfetto/`chrome://tracing` 打开。文件经临时文件 + rename 写出。

`--json`：采样期间 `progress`，结束时一条 `profile`（`payload: {target, device, frames, duration_ms, dropped, metrics: {<§25.2 指标>: {p50, p95, max}}, unavailable, caveats}`，时间以 ms、计数以整数）、每个写出的文件一条 `artifact`（`kind: "trace" | "chrome-trace"`，带 sha256）、`summary`。Human 输出为一行概要、每个指标一行 p50/p95/max、未测量项与 caveat。

```json
{"type":"profile","payload":{"target":"macos","device":null,"frames":238,"duration_ms":2004.1,"dropped":0,
 "metrics":{"frame":{"p50":8.29,"p95":8.97,"max":11.2},"gpu":{"p50":0.087,"p95":0.09,"max":0.12},"draw_calls":{"p50":3,"p95":3,"max":3}},
 "unavailable":["allocations","allocated_bytes"],"caveats":[]}}
```

### 25.4 当前实现

- Dev Session：`viso profile [<target>] [--device <id>]` 经 `viso run` 的控制端点（与 `inspect` 同一选择规则）请求首个连接的 runtime 接下来绘制的帧：`--frames <n>`（1–100000）取接下来 n 帧，`--seconds <s>`（0 < s ≤ 3600）取 s 秒内的帧，两者都给时先到者结束，都不给为 5 秒。App 只在有人采集时采样：runtime 同时只跑一个 capture，所有等待的 `viso profile` 与 `run --profile-frame` 共享它，session 在没有消费者时结束它。样本每 32 帧或 100 ms 成批经 dev 通道发送，每帧最多等 4 帧拿到它的 GPU 时间（按 backend 的 frame epoch 对应）。App 空闲不绘制时结果为 0 帧，human 输出提示在采集期间与 app 交互；
- `headless`：以 headless 构建 app（同 `viso test ui`），采样它 settle 或播放 `--scenario <name>` 期间绘制的每一帧，`--frames` 保留前 n 帧；headless 不接受 `--seconds`（它的时钟是自己的）与 `--device`，非 headless 不接受 `--scenario`（`CLI_USAGE`，exit 2）；
- Caveat：`ios-simulator`、`android-emulator` 的时间不代表真机（CPU 为本机，GPU 经 simulator/emulator 转译），`web` 的帧由浏览器调度，GPU 时间只在浏览器提供 `timestamp-query` 时测得且可能被浏览器取整，`headless` 的 `gpu` 是软件 raster 的 CPU 时间；
- GPU 时间目前只有 Metal 与 headless raster 报告，其它 backend 的 `gpu` 列入 `unavailable`；
- 失败码：没有 session `ENV_NO_DEV_SESSION`（exit 3）；采集期间 app 离开 session 或 headless run crash `PROFILE_APP_GONE`（exit 5）；trace 写不出 `PROFILE_WRITE`（exit 3）；回答无法解码 `PROFILE_ANSWER_INVALID`（exit 7）；scenario 失败 `TEST_EXPECTATION`（exit 1）；Ctrl-C 130。

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
name = "notes"
display_name = "Notes"          # 图标下的名字；缺省为 name
bundle_id = "com.example.notes" # macOS/iOS/Android 必需
version = "1.0.0"               # 必需
build = 42                      # CFBundleVersion / versionCode；缺省 1
orientation = "portrait"        # any|portrait|landscape；缺省 any

[package.icons]
source = "assets/icon.png"      # 正方形 PNG，至少 1024 px

[package.permissions]           # 每项权限及系统询问时显示的理由
camera = "Scans receipts into notes"
```

权限 `camera|microphone|location|photos|contacts|bluetooth|notifications` 映射为各平台的 Info.plist usage 说明、hardened runtime entitlement 与 Android `<uses-permission>`。检查在构建前进行，问题以 C diagnostic 指向所在行：缺 `version`、所需平台缺 `bundle_id` 为 C0006；图标不可读、非 PNG、非正方形或小于 1024 px 为 C0007；`provisioning_profile`/`keystore` 指向的文件不存在为 C0007，有 keystore 无 `key_alias` 为 C0006。需要图标的平台没有图标是 warning（系统默认图标）；iOS 图标含透明像素是 warning（铺白底，iOS 图标不透明）。Windows 上版本的 major/minor/patch 与 `build` 各不超过 65535（Windows 版本号是四个 16 位数），超出为 C0007；有图标时 MSIX 的 package name 取 `bundle_id`，缺省取 `name`（`.`、`-`、字母数字以外的字符换成 `-`），须为 3–50 个字符，否则为 C0007，建议设 `bundle_id`。

Windows 的 executable 在链接时带上资源：图标（16、20、24、32、40、48、64 px 为 32 位 bitmap，256 px 为 PNG，从同一源图按线性光缩放）与版本信息（`VS_VERSIONINFO`：数字版本 `major.minor.patch.build`，`ProductName`/`FileDescription` 为 display name，`FileVersion`/`ProductVersion` 为 `version`，配置了 `publisher` 时 `CompanyName` 为其 `O=`（否则 `CN=`），dev profile 标记 debug）。资源由打包器自己写出——MSVC 目标为 `.res`（与 `rc.exe`/`llvm-rc` 的输出逐字节相同），GNU 目标为只含 `.rsrc` section 的 COFF object——放在 build 目录下以内容 hash 命名，经 `cargo rustc --bins -- -Clink-arg=<file>` 交给链接器；内容变了文件名就变，Cargo 因此重新链接。

### 27.2 Signing

`--signing auto|required|off`（默认 `auto`）：

```text
auto      配置的或 keychain 中找到的身份；没有则按平台尽量签（ad-hoc、debug keystore 或不签）并 warning PACKAGE_UNSIGNED
required  只接受真实身份；没有则 PACKAGE_SIGNING，exit 6
off       不签
```

| 平台 | 身份来源 | `auto` 无身份时 |
|---|---|---|
| macOS | `[package.macos] identity`，否则 keychain 中第一个 `Developer ID Application:`；hardened runtime + 按权限生成 entitlements + timestamp，`.dmg` 同样签名 | ad-hoc（只在本机可运行） |
| iOS | `[package.ios] identity`（否则 keychain 中 Apple/iPhone Distribution，再 Development）+ `provisioning_profile`；profile 必须覆盖 `bundle_id`，`team_id` 若配置须与 profile 一致 | 不签 |
| Android | `[package.android] keystore` + `key_alias`；密码只从 `VISO_ANDROID_KEYSTORE_PASSWORD`（key 不同则 `VISO_ANDROID_KEY_PASSWORD`）由 `apksigner`/`jarsigner` 自己读取（`env:` 形式），缺失即 PACKAGE_SIGNING | debug keystore |
| Windows | `[package.windows] identity`（证书 subject 或 SHA-1 thumbprint，在系统证书库）+ 可选 `timestamp_url`，`signtool`；先签 executable，再签 MSIX。有图标（即有 MSIX）时还须 `publisher`（证书的完整 subject），否则 PACKAGE_SIGNING——Windows 只安装 publisher 与签名证书 subject 相同的包 | 不签；MSIX 的 publisher 为 `publisher`（缺省 `CN=<display_name>`）加 unsigned 标记 `OID.2.25.311729368913984317654407730594956997722=1`，可用 `Add-AppxPackage -AllowUnsigned` 安装 |
| Linux / Web | 无签名；`required` 为 PACKAGE_SIGNING | — |

配置中点名的身份不在 keychain 时，`auto` 也失败（不静默降级）。Secret 不出现在命令行、`Viso.toml`、log 或 `artifact.json`；身份只以名字出现。

### 27.3 `--dry-run`

```bash
viso package ios --dry-run
```

不构建，输出一条 `result`（human 为对齐的表）：

```json
{"target":"ios","profile":"shipping",
 "metadata":{"id":"com.example.notes","name":"notes","display_name":"Notes","version":"1.0.0","build":42,"icon":"/…/assets/icon.png","orientation":"portrait","permissions":["camera"]},
 "signing":{"policy":"auto","status":"signed","identity":"Apple Distribution: Example (ABCDE12345)"},
 "steps":["compile for aarch64-apple-ios","bundle the app for devices","compile the icon into the asset catalog","sign it with `…` under `notes.mobileprovision`","archive notes-1.0.0.ipa"],
 "output":"dist/ios","artifacts":[{"kind":"ipa","path":"dist/ios/notes-1.0.0.ipa"}]}
```

metadata 检查与签名决定都在 dry-run 中完成，因此缺身份、缺文件在 dry-run 就失败。

### 27.4 Artifact manifest

每次 package 产生 machine-readable manifest：

```text
dist/<platform>/artifact.json
```

```json
{"project":"notes","version":"1.0.0","build":42,"target":"macos","profile":"shipping",
 "build_id":"…","toolchain":"rustc 1.90.0 (…)",
 "signing":{"policy":"auto","status":"signed","identity":"Developer ID Application: Example (ABCDE12345)"},
 "artifacts":[{"kind":"app-bundle","path":"Notes.app","size":…,"hash":"sha256:…"},
              {"kind":"dmg","path":"notes-1.0.0-aarch64.dmg","size":…,"hash":"sha256:…"}]}
```

`signing.status` ∈ `none|ad-hoc|debug-keystore|signed`（`viso build` 的 `artifact.json` 用同一对象，只有 `status`）。各平台产物（`<name>` 为 `package.name` 中可作文件名的字符）：

```text
macOS    <display_name>.app、<name>-<version>-<arch>.dmg（含 /Applications 链接，UDZO）
Windows  <name>-<version>-<arch>.zip（<name>-<version>-<arch>/<bin>.exe，图标与版本已链接进 exe）；有图标时另有 <name>-<version>-<arch>.msix
Linux    <name>-<version>-<arch>.tar.gz（可执行文件、.desktop、512 px 图标）
iOS      <name>-<version>.ipa（device 架构，actool 编译的 asset catalog）
Android  <name>-<version>.apk 与 .aab（每个 ABI 一个库；`[package.android] abis`，缺省 arm64-v8a）
Web      site/（与 build 相同的 bundle）与 <name>-<version>-<target>.zip
```

zip 与 tar 由 CLI 自己写：条目按名排序、固定时间（1980-01-01）、模式只保留可执行位、无属主，同样的文件得到同样的字节。图标从 source 在线性光、预乘 alpha 下按面积平均缩放到各平台尺寸（ICNS 16–1024、Android 五个 density、Linux 512）。

### 27.5 JSON / exit codes

`--json`：`progress`、`diagnostic`、每个产物一条 `artifact`（含 `artifact.json` 本身）、`summary`。`--dry-run` 以一条 `result`（§27.3 字段）代替 `artifact`。Exit code：1（diagnostics）、2（`--profile dev`）、3（SDK/lock）、4（build）、6（PACKAGE_SIGNING、PACKAGE_ARCHIVE）。持有 `package-<target>.lock`，编译期间另持 `build-<target>.lock`。

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

`--json`：`progress`、`diagnostic`（含 `EXPORT_*` capability 诊断）、输出目录一条 `artifact`（`kind:"export-dir"`，`profile` 为 `null`）、`summary`。Exit code：capability diagnostics（Unsupported、`EXPORT_HTML_DYNAMIC_REQUIRED` 等）1；exporter 写出或外部工具（如 package manager）失败 6。

### 28.4 导出目录

导出目录整体替换：先写到它旁边的 staging 目录，再一次换位，失败时原目录不变。目录内 `.viso-generated.json` 记录 `{generator, files:[{path, sha256}]}`；目录中有导出未写的文件、或写后被改过的文件时以 `EXPORT_CONFLICT`（exit 6）拒绝并列出路径，`--force` 才替换。项目根、它的祖先、`src/` 与 `assets/` 下不能作导出目录（`CLI_USAGE`）。读写失败为 `EXPORT_WRITE`（exit 6）。

---

## 29. `viso export html`

生成标准 HTML/CSS/vanilla JS 产物。

```bash
viso export html --out dist-html
viso export html ui::Card --out card
```

导出的组件是 `<component>`（`Name` 或 `module::Name`，input 取默认值；有无默认值的 input 为 `EXPORT_UNSUPPORTED`），省略时为 `src/app.vs` 导出的组件（没有时 `EXPORT_NO_VIEW`）。目录是 `--out`（相对当前目录），否则 `Viso.toml` 的 `export.html.out_dir`（相对项目根），否则 `dist-html/`。产物：`index.html`、`styles.css`、有变化时的 `app.js`，以及页面显示的 `assets/<path>`；同样的源码得到同样的字节。

目标：

- semantic HTML；
- CSS classes/variables；
- DOM-native accessibility；
- DOM event mapping；
- 对支持的 reactive subset 生成直接 DOM mutation/vanilla JS；
- 不引入 Solid/React/Vue 等 framework dependency。

分类：每个 entry 由解释器在初始 state、input 默认值、默认 theme 与 source locale 下求值，它读到的 state（`Vm::reads`）决定它是常量（直接写进 HTML/CSS）、theme 字段（CSS custom property）还是会变化（由 `app.js` 更新）。`app.js` 把可达的 Behavior IR 函数逐条翻译为 JavaScript，保持解释器的值语义（整数为 BigInt 并检查范围、`F32` 每步舍入、聚合 `{t, f}`、列表写时复制）；每个 handler 是一个 transaction，只刷新读到被改 state 的绑定；`if`/`match`/keyed `for` 区域从 `<template>` 挂载；`emit` 在 `#viso-root` 上派发 `CustomEvent`；`tr` 用 `Intl.PluralRules`/`Intl.NumberFormat` 格式化。消息目录有多个 locale 时，`app.js` 带上每个 locale 的消息表（翻译缺失的条目已按 CLDR fallback 指向其来源表）。页面载入时按 `navigator.languages` 依次查找最近的 locale：先是原 tag，再逐级截短，再取 `Intl.Locale#maximize` 推出的 script（`zh-TW` → `zh-Hant`）；都找不到时用 source。找到后设置 `<html lang>`、`env.locale` 以及数字的格式化语言。这时读到 locale 的 entry 不再写进 HTML，改由脚本在挂载时写入。plural 规则取 pattern 所在表的语言，与原生 runtime 一致。`--static` 导出没有脚本，以 source locale 写成。Solid export 也是如此：组件模块载入时 `settle(MESSAGES)`。页面也运行视图启动的工作，语义与原生 runtime 相同：task 译为 `async` 函数，task native（`time::sleep`）是 await 的 promise，在启动它的 transaction 提交后启动；命名 slot 按策略处理（`keep_latest` 取消前一个并运行 `cancelled`，`drop_new` 丢弃新的，`queue` 与 `parallel(n)` 排队）；`success`/`error` 各是一个新 transaction。`effect` 在挂载它的提交以及每个改变其 `when` 值的提交之后按 run 策略运行，按轮次直到稳定（至多 16 轮），返回的 cleanup 在下一次运行前与卸载时运行。`resource` 依次经过 `loading`/`reloading`/`ready`/`error`，支持 `debounce`、`keep_latest`、`cache_for`/`cache_errors`，过期结果不会覆盖新 key 的状态。区域挂载的组件在每次挂载中各自保存 state、运行 effect、拥有它启动的 task；内容卸载时这些都结束，且不运行任何 handler。`--static` 导出遇到 effect 报 `Need::Dynamic`。Solid export 用同一套运行时：store 在提交后启动 task，按同样的规则运行 effect 与 resource；挂载组件的区域内容是一个 `st.mount`，以自己的 signal 保存该组件的 state，在其中运行它的 effect、拥有它启动的 task，随其 Solid owner 一并结束；读这类 state 的 computed 不做成共享 memo。theme 与 locale 之外的 `env` 字段取自浏览器（页面与 Solid store 共用 `env.js`）：window、safe area（与 web backend 相同的 `env(safe-area-inset-*)` 探测）、keyboard inset（visual viewport）、display features（viewport segments）、input（media query、触摸点、gamepad）、text scale（`font-size: medium`）、reduced motion、orientation、layout direction，各自随相应事件更新。`env.constraints` 与 `env.size_class` 按原生布局的解析方式在读者根元素处测量，依据元素标出的布局信息（`data-root`；`data-fit` 标出由内容决定的方向；`data-scope` 标出 `AdaptiveScope` 及其 basis），并用 `ResizeObserver` 跟随；区域中的每次挂载各自保存。game system 只由 Viso runtime 调度，为 `EXPORT_UNSUPPORTED`（改用 `viso build web-gpu`）。

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

尺寸与运行时一致：leaf 默认 0，容器默认两轴 Fill，写了一边时另一边为 Fit；Flex 主轴 Fill(w) 为 `flex: w 0 0`，交叉轴 Fill 为自然尺寸（除非父容器 stretch）。控件映射为对应 HTML 元素与 role（`button`、`checkbox` + `switch`、`range`、`input`、`img`、tabs/radio group 的 `aria-selected`/`aria-checked`），semantics 为 ARIA，style 的 `When` arm 为 `:hover`/`:active`/`:focus-visible` 规则，transition 附 `prefers-reduced-motion` 规则。

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

组件与目录的选择同 §29（`<component>`、`--out`、`export.solid.out_dir`，默认 `web-solid/`）。实际输出：

```text
web-solid/
├── package.json        solid-js, vite, vite-plugin-solid, typescript
├── tsconfig.json
├── vite.config.ts
├── index.html
└── src/
    ├── index.tsx
    ├── App.tsx                         挂载导出的组件
    ├── components/<Name>.tsx           JSX、signal、props
    ├── components/<Name>.behavior.ts   Behavior IR 翻译的 TypeScript
    ├── viso/runtime.js (+ .d.ts)       值语义、store、Each/With/Choose
    ├── styles.css
    └── assets/
```

导出与 `export html` 共用同一次 lowering 与分类，所以两者支持与拒绝的内容相同；GPU-only 节点为 `EXPORT_SOLID_GPU_ONLY_NODE`。Viso 不运行 package manager；`node_modules/`、`dist/` 与 lockfile 在再次导出时原样保留，不算冲突；artifact 的 hash 只覆盖生成的文件。

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

实际映射：每个 state 一个 `createSignal`；无参的 `computed` 为 `createMemo`（transaction 内直接求值，读到工作副本）；input 为可选 typed prop（`Bool` → `boolean`，整数 → `bigint | number`，浮点 → `number`，`String` → `string`，其余为 Viso 值），缺省时为 input 默认值；event 为 `on<Event>` callback prop。每个 handler 在 store 的工作副本上运行为一个 transaction，成功后在一个 `batch` 中只设置改变了的 signal，再回调 emit 的 event；失败时什么都不变。`if` → `Show`（三个以上 arm 为 `Switch`/`Match`），`match` → `With` + `Switch`/`Match`，keyed `for` → `Each`（按 key 保留 item），`preserve` arm → `Choose`。

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
artifact      kind, path, target, profile, build_id, size, hash, cached
device        platform(ios|android), id, name, state(booting|ready|installing|launched|stopped)
test          name, domain, status(pass|fail|skip), duration_ms, message?, artifacts[]?
trace         scenario, tick, probes（`viso test game` 每 Tick 一条，§22.3）
profile       target, device, frames, duration_ms, dropped, metrics, unavailable[], caveats[]（§25）
inspect       kind(attached|input|focus|dirty|dropped), …（`inspect` 跟随中的 app 事件，§24.5）
server        target, url, host, port, lan（每条 web lane 一条，§15.4）
dev           见下
runtime       dev_session_id, runtime_session_id, target, build_id, connection(connected|closed), running_revision, candidate_revision?, last_good_revision, failing[], rust(idle|building|restarting), bytes_sent, bytes_received, patch?（`Viso_Hot_Reload.md` §52）
doctor_check  name, status(ok|warn|missing|error), code?, message?, suggestion?
result        查询型命令的数据载荷（形状由各命令定义）
log           level, source(app|device|tool), message
summary       见 §36.3
```

规则：

- `result` 用于 `config`、`schema`、`explain`、`dump`、`inspect query`、`android/ios list`、`emulator/simulator list`、`package --dry-run` 等查询型命令；除 `fmt`（每文件一条）外每次调用至多一条。
- 未单独说明 JSON 合同的命令只输出 `progress`、`diagnostic`、`summary`。
- `--json` 模式下 app/device 的 stdout/stderr 只能以 `log` 事件出现。

每种事件的 `payload` 示例（envelope 字段 `schema, schema_version, seq, type, timestamp_ms, session_id, command` 见 §35，这里只列 `type` 与 `payload`；`dev` 与 `runtime` 的完整字段见 §36.4）：

```json
{"type":"progress","payload":{"phase":"build","message":"building headless (dev)"}}
{"type":"diagnostic","payload":{"schema_version":"1.0","severity":"warning","code":"E3705","message":"`text` shows a `format` with literal text; word order differs between languages, so build it with `tr` and arguments","primary":{"file":"src/app.vs","byte_start":136,"byte_end":163,"line":5,"column_utf16":66,"end_line":5,"end_column_utf16":93},"related":[],"expected":[],"actual":null,"notes":[],"fixes":[]}}
{"type":"artifact","payload":{"kind":"trace","path":"target/viso/traces/profile-20261017-071234-512.ende","target":"headless","profile":"dev","build_id":"sha256:4be1…","size":18432,"hash":"sha256:4be1…"}}
{"type":"device","payload":{"platform":"android","id":"pixel-local","name":"Pixel (local)","state":"ready"}}
{"type":"test","payload":{"name":"clicks","domain":"ui","status":"pass","duration_ms":688}}
{"type":"trace","payload":{"scenario":"jump","tick":12,"probes":{"player.y":3.5}}}
{"type":"profile","payload":{"target":"headless","device":null,"frames":6,"duration_ms":12010.4,"dropped":0,"metrics":{"frame":{"p50":0.7,"p95":3.6,"max":3.6}},"unavailable":["allocations","allocated_bytes"],"caveats":[]}}
{"type":"inspect","payload":{"kind":"input","seq":1,"frame":1,"window":0,"input":"pointer_down","at":[60,78],"target":{"node_id":"15:0","label":"Button#tap"},"route":[{"node_id":"0:0","label":"container"},{"node_id":"15:0","label":"Button#tap"}]}}
{"type":"server","payload":{"target":"web-gpu","url":"http://127.0.0.1:8000/","host":"127.0.0.1","port":8000,"lan":false}}
{"type":"dev","payload":{"dev_session_id":"s1","runtime_session_id":"r1","target":"web","build_id":"b1","file":"src/view.vs","base_revision":2,"candidate_revision":3,"patch_class":null,"outcome":"rejected","stage":"parse","diagnostic_codes":["E1001"],"last_good_revision":2,"elapsed_ms":1.5,"mounts":0,"migrated":0,"reset":0,"focus_lost":0,"scroll_lost":0,"handlers_lost":0,"dirty":{"structure":0,"style":0,"measure":0,"layout":0,"transform":0,"paint":0,"hit_test":0,"semantics":0},"game":null}}
{"type":"runtime","payload":{"dev_session_id":"s1","runtime_session_id":"r1","target":"web","build_id":"b1","connection":"connected","running_revision":3,"candidate_revision":null,"last_good_revision":3,"failing":["src/view.vs"],"rust":"building","bytes_sent":1200,"bytes_received":300,"patch":{"candidate_revision":4,"outcome":"rejected","patch_class":null,"stage":"shader-compile","scoped_resets":0,"compile_ms":2.5,"commit_ms":null,"round_trip_ms":4,"bytes":900}}}
{"type":"doctor_check","payload":{"name":"rust","status":"ok","message":"rustc 1.99.1 (aarch64-apple-darwin)"}}
{"type":"result","payload":{"sdk":{"root":"/Users/x/Library/Android/sdk","origin":"ANDROID_HOME"},"default":null,"abi":"arm64-v8a","apis":[…]}}
{"type":"log","payload":{"level":"info","source":"tool","message":"    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.30s"}}
{"type":"summary","payload":{"status":"success","exit_code":0,"elapsed_ms":36,"warning_count":0,"error_count":0,"artifact_count":0,"package":"hr-android","file_count":1}}
```

### 36.1 Diagnostic

`payload` 即 `Viso_DSL_1.0.md` §138 对象（`edits` 为空的 fix 是下一步要运行的命令，§53）：`schema_version, severity, code, message, primary{file, byte_start, byte_end, line, column_utf16, end_line, end_column_utf16}, related[{…位置字段, message}], expected[], actual, notes[], fixes[{title, applicability, edits[{file, byte_start, byte_end, replacement}]}]`。CLI 不定义第二套 diagnostic 形状。Config 诊断（`C0001`–`C0012`）、环境诊断（如 `ENV_ANDROID_SDK`）、exporter 诊断与 CLI 自身诊断使用同一形状；无源码位置时 `primary` 为 `null`，Viso.toml 诊断的 `primary.file` 指向 `Viso.toml`。

CLI 自身诊断 code 各自绑定一个 exit code（CLI 内只有一张表，失败按其 code 取 exit，不另行选择）；前缀即失败类别：`CLI_*` 2、`ENV_*` 3、`BUILD_*` 4、`RUN_*` 5、`TEST_*` 1、`INTERNAL_*` 7，`DEVICE_*`/`GAME_*` 按各自含义：

```text
CLI_USAGE               命令行无法解析或请求命令不做的事（如对    exit 2
                        positional-target 命令写 `--target`、
                        `config get` 未知 key、只构建 dev 的命令
                        收到其他 `--profile`）
INTERNAL_PANIC          CLI 自身 panic；仍以 summary 结束 stream   exit 7
ENV_CURRENT_DIR         当前目录不可读                           exit 3
ENV_SOURCE_UNREADABLE   命令需要的源文件不可读                   exit 3
ENV_CARGO_MANIFEST      project root 下没有要构建的 Cargo.toml    exit 3
ENV_CARGO               cargo 无法启动                           exit 3
ENV_NO_EXECUTABLE       构建没有产出、或产出多个可执行文件       exit 3
ENV_DEV_CHANNEL         dev transport 无法建立（loopback，或通往板卡的本地网络地址） exit 3
ENV_DEVICE              simulator/emulator 工具链缺失或失败       exit 3
ENV_DEVICE_TARGET       device target 不能在本机运行（§3.6）      exit 3
ENV_DEPLOY              板卡经 SSH 不可达，或复制失败（§13.11）     exit 3
ENV_EMULATE             该 profile 不能在本机仿真（§13.11）         exit 3
ENV_STDOUT              stdout 写不出                            exit 3
ENV_SOURCE_WRITE        命令改写的源文件写不出                   exit 3
ENV_LOCK_HELD           命令需要的 lock 被其他 viso 进程持有      exit 3
ENV_REMOVE              要删除的文件/目录删不掉                  exit 3
ENV_BUILD_WRITE         build 输出写不出                         exit 3
ENV_TOOL                build 需要的工具未安装或无法运行         exit 3
ENV_NO_DEV_SESSION      没有可连的 `viso run` session             exit 3
ENV_TAPE_WRITE          录制的 Tape 无法写出                      exit 3
ENV_TEST_INPUT          测试 Scenario 或 Tape 缺失或不可读        exit 3
ENV_TEST_TARGET         测试域在此不能于该 target 上运行          exit 3
ENV_RUST_TOOLCHAIN      Rust toolchain 未安装或早于 Viso 所需     exit 3
ENV_RUST_TARGET         构建需要的 Rust target 未安装             exit 3
ENV_GPU                 host 没有 Viso 渲染所用的 GPU backend     exit 3
ENV_ANDROID_SDK         找不到带 NDK、build tools 与 platform 的 Android SDK  exit 3
ENV_ANDROID_LICENSE     Android SDK license 未接受或被拒绝        exit 3
ENV_DOWNLOAD            toolchain 组件下载失败（已到部分保留）    exit 3
ENV_DOWNLOAD_INTEGRITY  下载内容与固定的大小/SHA-1 不符           exit 3
ENV_INSTALL             组件无法解包或移动到位，或本机状态写不出  exit 3
ENV_OFFLINE             `--offline` 时仍有组件需要下载            exit 3
ANDROID_API_UNSUPPORTED API level 不在 Viso 支持矩阵中             exit 2
ANDROID_ADB_FAILED      `android adb --json` 中 adb 以失败状态退出   exit 3
ENV_IOS_HOST            host 不是 macOS                          exit 3
ENV_XCODE               Xcode 未安装或未被选为 developer directory exit 3
ENV_XCODE_LICENSE       Xcode license 未接受                      exit 3
IOS_RUNTIME_INVALID     `ios use` 的参数不是 runtime 版本          exit 2
IOS_RUNTIME_MISSING     runtime 未安装且未要求下载（`--download`） exit 2
TEST_INPUT_INVALID      Scenario/Tape 无法解析或与 Build 不符     exit 1
TEST_EXPECTATION        测试期望未满足                           exit 1
BUILD_FAILED            cargo build 失败                         exit 4
BUILD_PACKAGE           构建产物无法为 target 打包               exit 4
RUN_APP_FAILED          app 无法启动、crash 或非零退出           exit 5
RUN_TEST_CRASHED        测试用例的 runner 无法启动、crash 或未报告结束 exit 5
NEW_NAME_INVALID        package name 不是 Cargo 接受的名字         exit 2
NEW_PATH_TAKEN          目录是文件、非空且无 `--force-empty-dir`，或已有项目要写的文件 exit 2
NEW_FRAMEWORK           `--framework` 路径不是 `viso` crate 或包含它的 checkout exit 2
NEW_GIT                 Git 无法把新项目建成仓库（warning）         —
NEW_WRITE               项目文件写不出，未留下任何文件             exit 7
DEVICE_MISSING          没有 simulator/emulator profile           exit 3
DEVICE_UNKNOWN          `--device` 或默认值不是已有 profile        exit 2
DEVICE_AMBIGUOUS        多个 profile 且无默认                     exit 2
DEVICE_ID_INVALID       profile ID 不合法                         exit 2
DEVICE_EXISTS           同名 profile 已存在                       exit 2
DEVICE_SYSTEM           没有合适的已装 system image/runtime        exit 3
GAME_NOT_RUNNING        §22.3                                     exit 3
GAME_AMBIGUOUS          §22.3                                     exit 2
GAME_UNKNOWN_STATE      §22.3                                     exit 1
GAME_BUSY               §22.3                                     exit 3
GAME_NO_HISTORY         §22.3                                     exit 3
GAME_ENDED / GAME_FAULT §22.3                                     exit 5
GAME_ANSWER_INVALID     session 的回答不可解码或超出 dev frame     exit 7
```

### 36.2 Artifact

```text
kind      binary|app-bundle|apk|aab|wasm|js|html|brotli|gzip|asset-dir|archive|manifest|snapshot|trace|export-dir
path      相对 project root
target    host 解析后的实际平台名（macos|windows|linux）或逻辑 target
profile   dev|release|shipping；export 为 null
build_id  §46
size      bytes（目录为其全部文件之和）
hash      "sha256:<hex>"；目录按路径序对每个文件的「相对路径（`/` 分隔）、NUL、该文件 SHA-256」求 SHA-256
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

`viso run`/`serve` 的每个 candidate revision 的每个文件输出一条 `dev`（一个 batch 的多个文件共用一个 candidate revision），字段取自 `Viso_Hot_Reload.md`（§4.1 identity、§8 patch class、§37 ACK/NACK、§51 stage）：

```text
dev_session_id, runtime_session_id, target, build_id
base_revision, candidate_revision
patch_class     PATCH|PATCH_WITH_SCOPED_RESET|WARM_RESTART_REQUIRED
outcome         applied|scoped_reset|warm_restarted|rejected|held
stage           rejected 时为失败所在 §51 stage（watch, parse, ..., snapshot-restore）；提交成功时为 runtime-commit；held（`--no-hot-reload` 下编译通过、未发送）时为 transport
diagnostic_codes[]   去重，按首次出现排序
last_good_revision
elapsed_ms
```

Rust candidate（一个 batch 的 Rust 改动触发的一次 build）输出一条 `dev`：`file` 为 `null`，`files[]` 是它带的 Rust 文件（project 内相对 root，其外为绝对路径），`stage` 为 `warm-restart`，`diagnostic_codes` 是 rustc 的错误码（或 restore 被拒时的 NACK code），`build_ms` 是 cargo 用时，restart 的各段为 `prewarm_ms`（新的可执行文件先运行一次、在 `viso::run` 开头退出，供 OS 做首次启动检查，旧 app 照常运行）、`snapshot_ms`（请旧 app 给出 snapshot 到收到）、`close_ms`（之后到新进程启动，旧 app 退出）、`launch_ms`（新进程启动到报告 mount）、`restore_ms`（新进程的恢复）与 `restart_ms`（请 snapshot 到 restart 完成），没有运行的段为 `null`；`elapsed_ms` 为 `build_ms + prewarm_ms + restart_ms`；没有 `.vs` 的 commit 计数。human 输出为 `✓ rust warm restart   built in 2.4 s, restarted in 0.81 s`。它在 restart 完成时输出：`outcome` 为 `warm_restarted`、`patch_class` 为 `WARM_RESTART_REQUIRED`、`last_good_revision` 为新进程的 `0`，`restored` 表示新进程是否取回了 snapshot（旧 app 没有给出 snapshot、restore 被拒、新进程握手被拒——例如 session 中途换了 Viso 版本——或在恢复之前关闭 dev channel 或退出时为 `false`，human 输出写明 state not restored）；build 失败、新的可执行文件起不来或 session 在旧 app 关闭之前结束时为 `rejected`，`--no-hot-reload` 下为 `held`。

`.vs` patch 另带：`file`（相对 project root）、`mounts`（该文件被提交到的 mount 数）以及 commit report 计数 `migrated, reset, focus_lost, scroll_lost, handlers_lost`（rejected 时全为 0）、`dirty`（嵌套 object，§11 八个 dirty class 各一个 node 计数：`structure, style, measure, layout, transform, paint, hit_test, semantics`；rejected 时全为 0）、`game`（该文件的游戏随 patch 切换时为嵌套 object `tier`（`presentation-only|logic-only|logic+state-migration|world-rebuild`）、`sessions`、`tick`（新 Build 运行的首个 Tick）、`carried`、`reset`、`characters`，否则 `null`；游戏层级的 `E5103` 计入 `diagnostic_codes`）。`runtime_session_id` 是该 event 所涉 runtime 的 `RuntimeSessionId`（§4.1）的十六进制，`target` 是它的 target（`desktop|ios-simulator|android-emulator|web`），没有已连接 runtime 时分别为空字符串与 `null`；session 服务多个 runtime 时每个 runtime 各有自己的 `dev` event 与 revision。`patch_class` 在 rejected 与 held 时为 `null`。每条 `dev` 之前先输出该 candidate 的 `diagnostic`。字段均为 revision/计数/stage 名/诊断码，不携带任何可能是密钥的内容（`Viso_Hot_Reload.md` §47）。

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

`[package] capabilities = ["storage.persist"]` 是包被授予的 Capability（DSL §95）：编译期检查据此判定（如 `@persist` 需要 `storage.persist`，否则 `E9106`），运行时由宿主在链接 Native Import 时授予同一集合。

`[test.surface]` 是 headless 运行（`viso test`、`viso snapshot`）中每个窗口的 surface：`width`、`height` 为逻辑像素（整数 1–16384，缺省 1280、720），`scale` 为每逻辑像素的设备像素（整数或小数，大于 0、至多 8，缺省 1），越界为 C0007（§3.4）。

`[images]` 是 release 构建（`release` / `shipping`，dev 从不转换）把 `assets/` 的图像转换为 target 采样格式的方式（Architecture §51.1，Rendering §6.3）：

```toml
[images]
convert   = true       # false：照原样嵌入文件，设备上解码
quality   = "high"     # lossless | high | balanced | compact
svg       = "vector"   # vector | raster：构建期光栅化，设备不链接 SVG 解析
svg_scale = 2          # 光栅化 SVG 每文档单位的像素；缺省 board 1，其余 2
fit       = true       # 有 display 尺寸的 board 把超出 cover fit 所示的图像预缩小
```

- 格式族随 target：host / desktop → BC，`ios` / `android` / mobile → ASTC，embedded-gpu → ETC2，embedded-cpu → RGBA8 / RGB565 / RGBA5551 / RGBA4444（`display.format = "rgb565"` 时不透明图像取 RGB565）；`web-*` 与 `headless` 不转换。`quality`：`lossless` 一律 RGBA8；`high` BC7、ASTC 4×4、ETC2、8-bit 面板 RGBA8；`balanced` 不透明 BC1、ASTC 6×6、16-bit；`compact` ASTC 8×8，soft alpha RGBA4444；
- `build` 在 Cargo 运行前转换，结果写入 `target/viso/generated/images/<target>/`，路径作为 `VISO_ASSET_OVERLAY` 交给 `asset!` / `view!`；转换按 sha256(转换器版本、设置、源字节) 缓存在 `generated/images/cache/`，内容不变不重写；`images` 一条 progress 日志报告张数、缓存命中与前后字节。不能解码的 asset 是 `ENV_BUILD_WRITE`；
- board 构建另设 `VISO_IMAGES_CONVERTED=raster`（facade 不链接 PNG/JPEG 解码），`svg = "raster"` 时含 `svg`（不链接 SVG 解析）；
- 设备不采样所转换的格式时在加载时软件解码为 RGBA8（不失败，只多花一次解码与 4 字节/texel）。

`[i18n] source = "en"` 是声明全部消息的源 Locale（缺省 `en`）；消息目录是 `Viso.toml` 旁的 `i18n/<locale>.toml`（DSL §U10.3）。`viso check` 编译目录并报告其问题（`E3706`，指向目录文件的行列），`tr` 与 `MessageKey` 字面量据此检查；`i18n/` 无法读取是环境错误。

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
identity = "Apple Distribution: Example (ABCDE12345)"
provisioning_profile = "signing/notes.mobileprovision"
```

Simulator development 不需要 signing。`team_id`、provisioning/signing metadata 只属于 package/delivery configuration。Secret 不写进普通 project config。

### 38.4.1 Delivery signing

```toml
[package.macos]
identity = "Developer ID Application: Example (ABCDE12345)"

[package.android]
keystore = "signing/release.jks"
key_alias = "upload"
abis = ["arm64-v8a", "armeabi-v7a"]   # arm64-v8a|armeabi-v7a|x86_64|x86

[package.windows]
identity = "Example Ltd"               # subject 或 SHA-1 thumbprint
timestamp_url = "http://timestamp.example.com"
publisher = "CN=Example Ltd, O=Example Ltd, C=US"   # MSIX 的 publisher：签名证书的 subject
```

规则见 §27.2。

MSIX 由打包器自己写出（同样的文件得到同样的字节）：payload 为 `<bin>.exe` 与 `Assets/` 下的三张 logo（`Square44x44Logo.png` 44 px、`Square150x150Logo.png` 150 px、`StoreLogo.png` 50 px），`AppxManifest.xml` 声明 full-trust desktop app（`Windows.FullTrustApplication`、`runFullTrust`；`TargetDeviceFamily Windows.Desktop`，MinVersion 10.0.17763.0），Identity 为 package name、publisher、`major.minor.patch.0`（最后一位留给 Store）与 host 架构（`x64`/`arm64`/`x86`）；权限映射为 capability：`camera`→`webcam`、`microphone`、`location`、`bluetooth`（device capability），`photos`→`picturesLibrary`、`contacts`（uap capability），`notifications` 不需要。zip 按 app package 格式：PNG 原样存放，其余每 64 KiB block 以 full flush 单独 deflate，`AppxBlockMap.xml` 记录每个 block 的 SHA-256 与压缩后大小，`[Content_Types].xml` 列出每种 part 的类型。

### 38.5 Export

```toml
[export.html]
out_dir = "dist-html"

[export.solid]
out_dir = "web-solid"
package_manager = "pnpm"
```

Exporter config 不影响 Viso runtime semantics。

### 38.6 Device target

`[target.android]`、`[target.ios]` 以外的每个 `[target.<name>]` 都是一个 device target：项目命名的板卡，以 `viso build <name>` 构建（§3.6）。

```toml
[target.panel]
profile = "embedded-gpu"
triple  = "aarch64-unknown-linux-gnu"
display = { width = 1024, height = 600, format = "xrgb8888", rotation = 90 }
budget  = { heap = "48MiB", glyph_atlas = "4MiB", images = "16MiB" }
fonts   = { charset = ["latin", "gb2312"], bake = [14, 18, 24] }
locales = ["zh-Hans", "en"]
workers = 1
deploy  = "root@192.168.1.20"
```

```text
profile   必填  desktop|mobile|web|embedded-gpu|embedded-cpu（Architecture §50A.3）
triple    可选  Rust target triple；缺省为本机。embedded profile 必须是 Linux triple（含 `-linux`）
gpu       可选  gles|vulkan，仅 embedded-gpu；缺省 gles（§3.6 的 feature 与 shader 语言随之）
display   可选  width、height：1–16384 像素，须同时写；format：xrgb8888|rgb565|argb8888；
                rotation：0|90|180|270（顺时针，度）。未写的项取面板的 preferred mode 与 backend 默认
budget    可选  heap|gpu|faces|shaping|glyph_atlas|images = 大于 0 的大小：
                字符串 `<整数>[B|KiB|MiB|GiB]`（如 "48MiB"、"4096"），或字节数整数
workers   可选  主线程以外的 worker 数，0–256；0 为 idle 切片内联执行；缺省为 profile 的
fonts     可选  charset：字符串数组，仅 embedded profile；每项为 ascii|latin|greek|cyrillic|gb2312
                （大小写不敏感）、U+XXXX 或 U+XXXX-YYYY（低 ≤ 高，≤ U+10FFFF）。作为 VISO_FONT_CHARSET
                交给字体子集（§3.6，Text/Font Runtime §3.4）。
                bake：整数数组，仅 embedded profile；UI 绘制的字号（pixels per em，1–1024），作为
                VISO_FONT_BAKE 交给宏，构建期把 glyph bake 成 A8 page（Text/Font Runtime §3.6）。
                rasterizer：布尔，仅 embedded profile，缺省 true；false 时不链接 glyph rasterizer
                （不加 viso/rasterizer），只画 baked glyph，须同时写 bake
locales   可选  BCP 47 tag 数组，仅 embedded profile：板卡文本所用的 locale；决定 Unicode 数据
                的裁剪（是否加 viso/complex-line-break，Text/Font Runtime §3.7）。缺省为 i18n catalog
                的 locale
deploy    可选  "user@host[:port]"：`viso run <name>` 经 SSH 部署的板卡（§13.11）。user 可省略（SSH
                自己的）；带 port 的 IPv6 地址写在方括号中（"root@[fd00::7]:22"）
images    可选  与 `[images]` 同一组 key（§38），逐项覆盖之
```

- 名字：小写字母开头，其后为小写字母、数字、`-`、`_`，至多 64 个字符（它是 `dist/<name>/` 目录与命令行词）；内建 target 名（`host`、`headless`、`ios`、`android`、`web-gpu`、`web-dom`、`web-hybrid`）与其他 `dist/` 目录名（`macos`、`windows`、`linux`、`web`）不可用；违反为 C0007；
- 缺 `profile` 为 C0006；未知 key（含 `display`、`budget`、`fonts` 子表内）为 C0004 并给出最接近的 key；类型错误 C0005；越界或不在取值集合中为 C0007，均指向所写位置；`[target]` 下非 table 的 key 为 C0004；非 embedded-gpu 的 `gpu`、非 embedded profile 的 `fonts.charset`/`fonts.bake`/`fonts.rasterizer`/`locales`、写法不对的 charset 项、越界的 bake 字号、写法不对的 locale 与 `deploy` 为 C0007；`fonts.rasterizer = false` 而没有 `fonts.bake` 为 C0006；
- profile 只选择一组固定的 feature 与运行时默认值，用户不手工组合 feature（Architecture §50A.3）；embedded profile 关掉 facade 的默认 feature（含 `desktop`），见 §3.6；`budget`、`workers` 只覆盖 profile 的默认值，app 仍可经 `Application::memory_budget` 调整；
- device target 不出现在 `viso config show` 的 key 列表中（列表是固定 schema，device target 的 key 随项目命名）。

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

Script 在补全时以 `VISO_COMPLETE=<shell>` 回调 `viso` 本身（`viso` 启动时先检查该变量，未设置时只多一次环境变量读取），候选因此与 parser 同源：subcommand、option、target、profile 等枚举值来自 grammar；configuration key（`config get`）、simulator/emulator profile ID（`--device`、`emulator|simulator start|stop|delete`）与 `tests/game/` 下的 scenario（`test game`）按当时磁盘内容给出；路径参数按文件/目录补全。补全从不构建、启动设备或访问网络。安装方式：bash/zsh `source <(viso completion zsh)`，fish `viso completion fish | source`，PowerShell `viso completion powershell | Out-String | Invoke-Expression`；升级 `viso` 后应重新生成。

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

### 43.1 当前实现

- 长命令开始真正干活时才接管中断（`interrupt::catch`），交互问答期间 Ctrl-C 照常结束进程。接管的有 `run`、`serve`、`check --watch`、`build`、`package`、`test`、`profile`、`inspect events`、`new`、`ios use`，以及 license 确认之后的 `android use`；
- 子进程都经 §48 的 runner 启动，各自在独立进程组（Windows 为 job object）中，中断时连同它们启动的进程一起结束。打包工具（`viso-package` 里的 `codesign`、`apksigner`、`zipalign`、`wasm-bindgen`……）走的也是这个 runner；
- `android use` 中断时：下载留下 `.part` 供续传；解包中的 staging 目录删除；已校验的 archive 保留；
- `tests/cancel.rs`：在 build script 挂住时中断 `build`、`package`、`test`，断言 exit 130、summary `cancelled`、build script 进程已结束、`target/viso/locks/` 为空、`dist/` 下没有 staging 残留。

---

## 44. Project lock

防止同一 project 同一 mutable artifact 被并发破坏。锁是 `target/viso/locks/` 下的 advisory file（不是 `flock`）：

```text
build-<target>.lock                build cache，按 target
package-<target>.lock              dist/<target>/ 输出，按 target
dev-<target>[-<device>].lock       dev session，按 target/device
```

- 获取：`create_new` 原子创建；文件内容记录 holder `pid`、`acquired_unix`、description，以及可选的第 4 行 endpoint：持有者接收请求之处（dev session 的 control endpoint `<地址> <token>`，§22.3），发布时整体替换文件（写临时文件再 rename），读者看到的是旧内容或新内容，不会是半截。
- 不阻塞等待：锁被占用时立即失败，diagnostic 给出 holder pid、age 与锁路径，exit 3。
- 释放：持有者退出时删除（含 §43 取消路径）。
- 刷新：持有者每 20 秒更新锁文件的修改时间（不改内容）。
- Stale：只按 age 判定——修改时间超过 90 秒未刷新即 stale（不探测 pid 存活，pid 可能被复用）；崩溃的持有者留下的锁 90 秒内失效，任意长的 build/session 不会被误判。获取时遇到 stale lock 直接接管并输出 `log{level:"warn"}`，诊断中的 note 说明这一点。
- 不同 target 的 build/package 可以并发；同一 target 的 `run` 与 `build` 共享 `build-<target>.lock`。
- 锁被占用统一报 `ENV_LOCK_HELD`（exit 3），message 写明 holder，note 给出锁路径。命令自己取锁时立即失败；`run` 的每次重建取 build lock 时等待并可取消，因为 session 期间另一个 `build` 只占用片刻。取锁顺序固定为 dev/package → build，不会死锁。

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
    ├── test/<pid>/           # 一次 `viso test` 的用例目录，通过的用例随即删除
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

删除 `target/viso/`（build、cache、dev、generated、traces、locks），不删除 `dist/` 与 `~/.viso/`。存在未 stale 的 lock 时什么都不删，以 `ENV_LOCK_HELD`（exit 3）拒绝，note 列出每个 holder 与锁路径。删除不跟随 symlink：`target/viso/` 内指向外部的链接只删链接本身。`--target-dir` 下清理该 `target/` 的 `viso/`。`--json`：一条 `result`（`payload: {removed: [path], freed_bytes}`）+ `summary`；human 输出 `removed <path> (<size>)`，无可清理时 `nothing to clean`。

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

### 48.1 当前实现

CLI 启动的每个外部程序都经过同一个 runner（`tools/cli/src/process.rs`）：

- 程序与参数以列表给出，从不经 shell；
- `--verbose` 在 stderr 打出 `[viso] running <env> <program> <args> (in <dir>)`；`--json` 下改为 `log` 事件（`source:"tool"`，`level:"debug"`）。按名字是 secret 的变量与 `name=value` 参数显示为 `<hidden>`，紧跟在 secret 名字之后的参数也一样（`--token <hidden>`、`--es VISO_DEV_TOKEN <hidden>`）；
- 问答式调用（`capture`/`run`/`status`）不给 stdin，stdout/stderr 边写边读，结果为 `{status, stdout, stderr}`。失败分为四种：未安装、无法启动、超时（`did not finish in N s`）、被中断；
- 超时或第一次中断时，结束该程序及其启动的所有进程（Unix 进程组，Windows job object）。只有 CLI 自己接管 Ctrl-C 时，子进程才放进独立进程组，否则终端的 Ctrl-C 照常直接到达它；
- 环境变量继承用户环境：SDK 工具要从中读取 `HOME`、代理、`ANDROID_HOME`、`DEVELOPER_DIR` 等配置，回显时 secret 一律隐藏；
- `viso lsp` 以 exec 方式把进程交给 `viso-lsp`，交接之前同样回显。

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

### 50.1 当前实现

`viso android use` 的组件下载（`tools/cli/src/toolchain/download.rs`）：

- 只走 HTTPS（`curl --proto =https --proto-redir =https --tlsv1.2`），代理按环境变量（`HTTPS_PROXY`、`NO_PROXY`）；
- 每个 archive 的大小与 SHA-1 编译进 CLI，取自 SDK repository manifest，也就是发布方签发的那份 digest。传输时不超过 pin 住的大小（`--max-filesize`，并持续盯着 `.part` 的长度），超过即删除；
- 先写入 `<file>.part`，校验通过后才 rename 到位，只有校验过的 archive 才会被解包；
- 断点续传：只追加到同一 URL 的 `.part`，最后整体校验，所以续传不影响完整性。有进展的中断立刻续传；没有进展的尝试按 2 s、4 s 退避，连续 3 次后失败，已收到的部分保留；
- 缓存：已校验的 archive 不会重复下载；已安装的组件记录 `path` 与 `revision`，revision 变了就重新安装；
- `--offline` 是唯一的全局 flag，写在命令行哪个位置都一样。缺组件时直接报 `ENV_OFFLINE`，网络层也拒绝任何下载；
- license manifest 不 pin，但最多读取 16 MiB。

`viso ios use` 的 runtime 由 `xcodebuild -downloadPlatform` 下载，校验与签名由 Xcode 负责；`--offline` 下不会调用它。

---

## 51. Generated file safety

`new` / `export` / package 生成文件：

- 默认不覆盖用户已有文件；
- `--force` 仅在命令明确支持时生效；
- 写临时文件后 atomic rename；
- 失败时清理 incomplete output；
- export 先生成 staging tree，再原子替换目标目录或报告冲突。

### 51.1 当前实现

- 单个文件（`fmt` 的改写、`dump --output`、`game record --output`、profile trace、snapshot golden 及其 diff、toolchain 配置）一律经 `atomic::write`：先写到同目录的隐藏 sibling 并 `fsync`，再一次 rename 到位，权限保持不变，缺的目录会补上。中途失败时原文件原样保留，sibling 被删除；
- `new` 先在 staging 目录里写好整个项目再移入。目标是已有目录时，只接受空目录；加 `--force-empty-dir` 也只往里添加文件，从不替换已有文件。失败或 Ctrl-C 时，它创建的东西全部清掉；
- `export` 经 `generated::write`：staging tree 整体替换目标目录。目录里的 `.viso-generated.json` 记录每个文件的 SHA-256；有人改过或新增的文件、或不是它写的目录，都算冲突，只有 `--force` 才覆盖；
- `build`/`package` 的 `dist/<target>/<profile>` 同样先 staging，再整体换入。

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

- `viso --help` 的命令列表按 §1 分组（PROJECT、MOBILE DEV ENVIRONMENT、DEVELOP、LANGUAGE、TEST / DEBUG、DELIVERY、MAINTENANCE），组内按 §1 顺序；尚无命令的组不显示。分组表与 parser 一一对应：每个顶层命令恰在一组，测试保证。
- 每一层命令（含 `viso android emulator`、`viso config get` 等）都有 2–3 个 examples，每行 `命令  # 作用`；example 必须能被 parser 接受，测试逐条解析。
- 命令自身的 options 在 `Options:`，§6 global options 在其后 `Global options:`；顶层页只有 `Options:`。
- `-h` 与 `--help` 输出同一页（无 long help 变体）：取值列在 option 行尾 `[possible values: …]`。
- 没有 `viso help <command>` 子命令；写 `viso <command> --help`。
- `viso` 或命令组（如 `viso config`）缺子命令时：human 模式在 stderr 打印该层 help，exit 2；`--json` 下为 `CLI_USAGE` 诊断。

`viso --version`（与 `-V` 相同）不加载任何服务，只输出常量：

```text
viso 0.1.0 (4c0177cbeff8)     CLI 版本与构建 commit（12 位，git 外为 unknown）
language 1.0                  DSL language version
event schema 1                §34 envelope schema_version
diagnostic schema 1.0         §36.1 diagnostic schema_version
schema object 1.0             §18 schema object schema_version
dump 1                        §20 dump JSON dump_version
snapshot 1                    §23 golden sidecar snapshot_version
dev protocol 14               dev channel protocol version
```

`--json` 下为一条 `result`（`payload: {version, commit, language, event_schema, diagnostic_schema, schema_object, dump_version, snapshot_version, dev_protocol}`）+ `summary`。

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

CLI 的 human 诊断与编译器诊断同形：

```text
error[DEVICE_UNKNOWN]: no ios simulator profile is named `iphon`
 = note: …
 = help: run `viso ios simulator list`
 = help: run `viso ios simulator create iphon`
```

"What to do next" 是要运行的命令：human 为 `= help: run `<command>``，JSON 为一条无 edit 的 fix（`{title: "run `<command>`", applicability: "maybe-incorrect", edits: []}`）——命令可能改变机器状态，从不无人值守执行。只建议当前存在的命令。usage error 同样是 `CLI_USAGE` 诊断：parser 的 tip、可选值与 usage 行成为 notes，next 为该命令的 `--help`。

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

当前实现：进度只有一个出口 `Output::advance`/`settle`。

| 场景 | 行为 |
|---|---|
| TTY | stderr 上的一行原地刷新：内容 + 已用时间 `(m:ss)`，最多每 200 ms 一次；阶段结束时清掉 |
| 非 TTY human | 普通的 `[viso] …` 行，最多每 10 s 一行，不含任何控制字符 |
| `--json` | `progress` 事件，最多每秒一条 |
| `--quiet` | 不输出 |

阶段开始和结束各打一行 `[viso] …`（`Output::step`）。Cargo 自己的进度由 Cargo 输出。`build` 最后一行是 `Built <target> (<profile>) in N s`，`--verbose` 再加上各阶段耗时。

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

当前实现：能否提问只由一条规则判定（`device::can_ask`）：human 输出，stdin 与 stderr 都是 terminal，且没有 `CI`。会提问的地方及其 flag 等价物：

| 问题 | flag 等价 | 不能提问时 |
|---|---|---|
| `android use` 的 license | `--accept-licenses` | `ENV_ANDROID_LICENSE` |
| `ios use` 让 Xcode 下载 runtime | `--download` | `IOS_RUNTIME_MISSING`，next 为 `viso ios use <v> --download` |
| `run`/`test ui` 在没有 emulator/simulator profile 时创建标准 profile | `viso <platform> <kind> create phone` | `DEVICE_MISSING` |
| `run`/`test ui` 在多个 profile 中选一个 | `--device <id>`，或 `.viso/local.toml` 的默认值 | `DEVICE_AMBIGUOUS` |

`tests/prompts.rs` 把每个会提问的命令放在 stdin 预先写好 "yes" 的管道下运行，断言它报出结构化错误、没有读取 stdin。

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

CLI 测试不能要求 CI 真有手机或板卡。

Target/Device service 必须支持 fake backend：

```text
fake ios simulator
fake android emulator
fake board（`ssh`/`scp` 脚本，以本机目录为板卡的 home）
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

当前实现：这些命令只做 parse，并输出编译期常量或内置表。`tests/startup.rs` 在 `Viso.toml` 损坏、`PATH` 为空、没有 home 的目录下运行 `--version`、`--help`、`completion`、`explain`：全部成功，不创建任何文件。`viso --version` 取 5 次中最快的一次，须低于 250 ms（debug build）。这个预算在 CI 硬件上实测之前只是假设。

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

当前实现：`viso build` 在 Cargo 判定无需重编译之后，还会跳过 staging，条件是 staging 的输入都没变。输入包括：可执行文件的大小与 mtime、target、profile、web 设置、Android SDK、CLI 自身的二进制，web target 另加 `PATH` 上的 `wasm-bindgen`/`wasm-opt`。它们的 key 记在 `target/viso/build/<id>/out.staged`。

若 `dist/<platform>/` 正好包含这些 artifact（hash 一致）和相同的 `artifact.json`，且别无他物，就不再复制。此时 artifact 事件带 `cached: true`，human 输出第一行以 `, unchanged` 结尾。`package` 总是重新打包。`--verbose` 打出 compile/package/publish 各阶段耗时。

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

当前实现：CLI 内部各 service 之间直接传 typed value。解码只发生在边界上：Cargo 与 `simctl` 的 JSON 输出、dev channel、trace/golden sidecar/生成记录文件。

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
viso serve [web-gpu|web-dom|web-hybrid]...
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
viso export html [<component>] [--out <dir>] [--static] [--force]
viso export solid [<component>] [--out <dir>] [--force]
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

