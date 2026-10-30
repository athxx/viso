# Viso DSL 1.0：Game Profile

> 文档状态：Viso DSL 1.0 Draft，与 `Viso_DSL_1.0.md` 同步修订  
> 规范级别：规范性  
> 范围：游戏表达能力与 Game Profile（§104–§111）。章节编号在 DSL 文档集内全局唯一；语言本身（文法、类型、运行时语义）以 `Viso_DSL_1.0.md` 为准。

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
export trait Startup {
    action startup(cx: GameStart);
}

export trait FixedUpdate {
    action fixed_update(frame: FixedFrame);
}

export trait FrameUpdate {
    action frame_update(frame: RenderFrame);
}

export trait CollisionListener {
    action collision(event: CollisionEvent);
}

export trait AudioProcess {
    action audio_process(block: AudioBlock);
}

export trait AudioCommands {
    action audio_command(command: AudioCommand);
}

export trait AudioListener {
    action audio_event(event: AudioEvent);
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

`system` 规则：

- `system` 与 `component` 共享 `input`/`state`/`computed`/`fn`/`action` 成员面，但只由 Scheduler 驱动、从不挂载，声明 `view`、`event`、`slot`、`effect` 或 `resource`，或执行 `start`，报 `E9109`；只声明 System 的源文件不要求可挂载组件（不报 `E2005`）；
- `implements` 的每个 Bound 必须是 Native Schema 提供的 Scheduler Trait，同一 Trait 至多出现一次，否则报 `E2201`；
- Trait 的每个 Hook 需要同名 `action`，参数类型与 Hook 声明一致且不返回值，否则报 `E2201`；两个 Bound 声明同名 Hook 报 `E2202`；
- Hook 按 Hook 身份（Trait 路径 + Hook 名的 Native Id）绑定到 `action`，编译器不按名字认识任何 Hook；Scheduler 将自己认识的 Hook 身份映射到阶段。

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
// QuickStart: tick、world 与 spawn(desc) -> EntityId，以及 startup-only 资源初始化能力。
// QuickFrame: world, input, dt, tick, time，以及受控 game action surface。
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
QuickGame.start -> scheduler startup hook（与 Startup.startup 同一阶段）
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
- 启动事务包含全部 System 的 `Startup.startup` 与 `QuickGame.start`：第一个 Tick 前按 `system_order` 运行，共享一份 Instruction/Native Call Budget；任一 Hook Fault 则整个启动失败，已运行 Hook 的状态写入与其发出的 Presentation Command 一并丢弃，不产生可运行的 Scheduler；成功后启动期的 Presentation Command 按 `(source_system, sequence)` 先于 Tick 0 的命令投递；
- `QuickStart` 与 `QuickFrame` 是 `GameStart` 与 `FixedFrame` 的视图：同一时钟、输入与 Snapshot 布局，因此 Quick Game 与拆成 `Startup` + `FixedUpdate` 的等价 System 在同一输入下逐 Tick 得到相同 Simulation 状态，Snapshot 可互相 Restore；
- `QuickStart.spawn` 的初始化命令必须在第一个 Tick 前提交，所以返回的 `EntityId` 在首次 `fixed` 时已经有效。

当游戏需要独立 Physics/AI/Combat/Audio/Networking 等生命周期时，SHOULD 拆成多个标准 `system ... implements FixedUpdate/FrameUpdate/...`。

### 105.2 Quick Game Kit

`viso::game::kit` 是标准库提供的高层 typed API：地形、基础模型、相机 Rig（第三人称、追随、俯视）、Prefab（角色、载具）、行为（巡逻、追逐、游荡）、粒子与合成音效。它同样只是 Native Schema，不是 Parser 特例。

规则：

- Kit 调用 Lower 为普通 GameWorld 命令（§108），与手写 System 语义一致；
- 每个 Kit 方法在 Schema 中声明所属层（§106.4）：修改 World 的是 `native action`，粒子、音效、相机抖动和 Debug Draw 是 Presentation Command；
- 模型、Tag、音效与动作都是 Typed Enum 或 Resource Key，不接受任意字符串；
- 调用未知方法或 Variant 报 `E2001`，诊断必须在 `related` 与 `fixes`（§138）中给出最近候选：按编辑距离排序，并按 Receiver 类型与期望类型过滤，使 AI 一步修正。

Schema（标准实现）：每个 Hook Context（`GameStart`、`FixedFrame`、`RenderFrame`、`CollisionEvent`、`QuickStart`、`QuickFrame`）以只读属性 `.kit` 给出借用的 `viso::game::kit::Kit` Handle：

| 方法 | 层 | 语义 |
| ---- | -- | ---- |
| `terrain(Terrain) -> List<EntityId>` | Simulation `action` | 生成地面 Block（顶面 `y = 0`）与丘陵：种子化 Value Noise 高度场按自身极值归一、向谷底压缩、原点附近留平坦广场，量化为 `step` 高的台阶；每级台阶合并为最少矩形 Block |
| `wander(id, range, speed, pause: Duration)` | Simulation `action` | 绕提交时所在点游荡：走到 `range` 内随机点（拒绝采样，取 World 种子随机源），停 `pause` 后再选 |
| `chase(id, tag, range, speed)` | Simulation `action` | 追 `range` 内最近的带 `tag` 实体（平面距离，同距先分配者优先） |
| `patrol(id, route: List<Vec3F32>, speed)` | Simulation `action` | 依次循环走过航点（至多 256 个） |
| `idle(id)` | Simulation `action` | 撤销行为或驾驶 |
| `drive(id, throttle, steer)` | Simulation `action` | 使其成为载具：油门/转向累加后钳到 `[-1, 1]`；加速、制动、滑行、限速，转向权随速度增长、倒车反向 |
| `facing(id) -> Vec3F32` | Simulation `fn`（Read） | 已提交的朝向 `(x, 0, z)` |
| `camera(CameraRig)`、`shake(strength, seconds)` | Presentation | 相机 Rig 与抖动 |
| `burst(Particle, at, count)`、`emit(id, Particle)`、`stop_emit(id)` | Presentation | 粒子 |
| `sound(Sfx)`、`sound_at(Sfx, at)`、`beep(Wave, from_hz, to_hz, seconds)` | Presentation | 合成音效；`sound_at` 按相机定位衰减与声像 |
| `rumble(strong, weak, seconds)` | Presentation | 本地玩家手柄的震动：强（低频）与弱（高频）马达各 `0..=1`（超出钳到 1），持续 `seconds`；两者为 0 或时长为 0 时停止；一帧内后一次替换前一次，帧末交给窗口持有输入焦点时所有已连接的手柄，设备不能震动时无效果 |
| `debug_line(from, to)`、`debug_box(at, size)` | Debug Draw | Release 中移除 |

- 行为与载具 Lower 为普通 World 命令：调用按发出它的 System 记入 Command Buffer（§108），提交时生效；每次 Physics Step 在任何 Body 移动前，按分配顺序从已提交位置算出每个受控 Character 的转向，作为 `walk` 加到其 System 提交的 `walk` 上，接近目标时减速落点；移动后行走者面向其水平速度方向，载具保留速度在朝向上的分量（撞墙即停）。`teleport` 使载具静止，`remove` 撤销其控制；
- 朝向、模型与控制属于 World Revision，进入 Snapshot；全部为单精度 IEEE 运算，载具转向用多项式而非宿主超越函数，Schema 声明 `cross_platform`；
- `SpawnDesc::prefab(Prefab)` 给出现成 Body（`hero`、`villager`、`monster`、`car`、`truck` 为 Character，`wall`、`platform`、`tree`、`rock`、`barrel` 为 Block，`coin`、`gem`、`goal` 为 Sensor），`.model(Model)` 指定绘制模型；`Model::auto` 按 Body 种类绘制；
- `CameraRig` 是值类型：`third_person(target)`（在目标朝向后方，随其转向）、`follow(target)`（固定方向追随）、`top_down(target)`（正上方俯视）、`fixed(eye, look)`，修饰 `.distance`、`.height`、`.pitch`、`.yaw`、`.fov`（度）、`.lag`（缓动时间常数，秒）；`Terrain` 是值类型：`flat(size)`、`hills(size, height)`，修饰 `.seed`、`.cell`、`.step`、`.feature`；
- 模型、Prefab、粒子（`spark`、`smoke`、`dust`、`trail`、`fire`、`confetti`）、音效（`viso::game::kit::Sfx`）与波形（`sine`、`square`、`saw`、`triangle`、`noise`）都是 Schema Enum；
- Presentation 命令落到 Game 的 Stage：相机视图、粒子 Sprite（定容池，满则轮换复用）、待交付的音效 Cue（至多 64 条，宿主排空交给音频线程的实时合成器：定额 Voice、无分配、无锁、PolyBLEP 限带）与 Debug 形状（Tick 发出的保留到下个 Tick 交付，Frame 发出的保留到下一 Frame）；Scheduler 每 Frame 在 `FrameUpdate` 之后推进 Stage，World Rebuild 清空它。

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
- Random 必须来自注入的 Seeded RNG：`world.random(): F64`（`[0, 1)`）与 `world.random_range(lo, hi): I64`（`[lo, hi)`，无偏，空区间为 Native 错误）是 `action`，种子由宿主注入（`Scheduler::with_seed`），RNG 状态进入 Snapshot，发生 Fault 的 Hook 的抽取随其状态写入一起回滚；
- Entity 迭代顺序必须稳定或显式声明无序；
- 多线程系统必须通过 Deterministic Command Buffer 合并；
- 每个 Tick 有 Instruction/Native Call Budget；
- Tick 超限时采用 Profile 策略，不能无限阻塞 UI Thread。

`system_order`：

- `@after(A, B)` / `@before(C)` 声明偏序，参数必须是 System 名，否则报 `E2001`；
- 编译器在 Package 内做稳定拓扑排序：无约束的 System 保持声明顺序（模块图顺序，再源码顺序）；顺序成环报 `E9101`；
- 一个 Tick 按该顺序运行全部 `FixedUpdate`，再把排队到该 Tick 的碰撞事件逐个投递给全部 `CollisionListener`；一帧先运行它欠下的全部 Tick，再按顺序运行全部 `FrameUpdate`；

`physics_order`（当前实现）：一个 Tick 依次是全部 `FixedUpdate` → 提交 → 全部 `PrePhysics.pre_physics(frame: FixedFrame)` → 提交 → Physics Step → 新开始的接触投递给 `CollisionListener` → 提交 → 全部 `PostPhysics.post_physics(frame: FixedFrame)` → 提交；每个阶段内按 `system_order`。`PrePhysics` 看到本 Tick `FixedUpdate` 已提交的命令（如施力前的玩法决策），`PostPhysics` 看到 Step 之后的 World（如掉出世界的回收、回合判定）；两者都是 Simulation Hook，与 `FixedUpdate` 共享 Tick 预算，预算耗尽跳过其后全部阶段。Physics Step 内 Body 按分配顺序处理。

`collision_delivery_order`：`[game] collision_delivery` 取 `event_major`（默认，一个接触交给全部 Listener 再到下一个接触）或 `listener_major`（一个 Listener 看完全部接触再到下一个 Listener）；档位与 `[game] determinism` 一起写入 Module（`Module::collision_delivery`、`Module::determinism`），因此属于 Build Hash。
- 每次 Hook 调用是独立事务：Fault 丢弃该调用的状态写入并被记录，下一个 System 照常运行；
- 一个 Tick 的全部 Hook 共享一份 Instruction/Native Call Budget，一帧的 `FrameUpdate` 共享另一份；Tick 预算耗尽报 `E9102` 并跳过该 Tick 余下的 Hook，Tick 仍然计数。

### 106.1 时钟、暂停与超限

```text
tick:       U64       单调递增；只有 World Rebuild 和 Restore 会改变它
fixed_dt:   Duration  Profile 编译期常量 1 / tick_rate；`[game] tick_rate` 取 1..=1000 Hz，默认 60
time_scale: F32       只缩放 Wall Time 到 Tick 累加器的速率，不改变 fixed_dt
paused:     Bool      暂停时不累加；FrameUpdate 照常运行
```

`frame.time()` 定义为 `tick × fixed_dt`，不是 Wall Clock，Logic-only Reload 不重置它。

超限策略由 Profile 选择：

```viso
export enum TickOverrun {
    DropTime;
    SlowMotion;
}
```

每帧把 Wall Time × `time_scale` 加入累加器，每满一个 `fixed_dt` 欠一个 Tick，一帧至多运行 `max_catch_up_steps`（默认 8）个；负数或非有限的 Wall Time / `time_scale` 按 0 处理。

- `DropTime`（默认）：累加器超过 `max_catch_up_steps` 后丢弃剩余时间，保留不足一个 Tick 的余量；
- `SlowMotion`：保留累加时间，不丢 Tick，游戏整体变慢；保留量至多一帧的追赶量（`max_catch_up_steps × fixed_dt`），超出部分仍被丢弃，防止积压无限增长；
- 两种策略都计入 `game.overrun_ticks`（各帧欠下而超出上限的 Tick 数之和）与 `game.dropped_time`（被丢弃的缩放后秒数）计数器；
- 调试器可 `step(n)` 单步推进 n 个 Tick，暂停时同样可用，单步与正常运行走同一 Scheduler 路径。

### 106.2 输入边沿语义

- 每个 Fixed Tick 经 `frame.input` 看到一份冻结的 `InputSnapshot`（Borrowed Handle）：`pressed`/`released`/`held(action)`、`axis(InputAxis::move_x)` 与 `move_axes()`；
- 一个渲染帧可能运行 0 个或多个 Tick。`pressed`/`released` 边沿归属到它发生之后的第一个 Tick，并且只被看到一次：0 个 Tick 的帧不丢边沿，多个 Tick 的帧不重复边沿；
- `held` 与 Axis 在同一渲染帧的多个 Tick 中取相同值；
- 同一 Tick 内按下又释放时，`pressed` 与 `released` 都为真；
- 文本输入、IME 和 UI 焦点中的按键不进入 Game 输入，除非 `GameViewport` 持有输入焦点；失去输入焦点时释放全部按键、按钮与摇杆，仍按下的动作在下一个 Tick 得到 `released`。
- 手柄每帧在帧首轮询一次（只在状态变化时产生事件），交给持有输入焦点的窗口中的游戏，规则与按键相同；所有已连接的手柄合并为本地玩家 0：任一手柄按住即按住，每根摇杆取推得最远的手柄；扳机越过行程 0.25 时其 `PadButton` 按下、回落到 0.15 以下时松开；手柄断开时释放它按住的一切。System 经 `kit.rumble` 让这些手柄震动（§105.2）。

### 106.3 Typed 输入映射

未声明映射时使用 `viso::game` 的默认动作集 `InputAction` / `InputAxis`。游戏用普通 Enum 声明自己的动作，用常量声明映射：

```viso
import viso::game::{InputMap, Key, KeySet, PadButton, PadStick};

@derive(Eq, Hash, InputAction)
export enum Act {
    Jump;
    Fire;
}

export const CONTROLS: InputMap<Act> = InputMap::new()
    .key(Key::Space, Act::Jump)
    .pad(PadButton::South, Act::Jump)
    .key(Key::J, Act::Fire)
    .pad(PadButton::West, Act::Fire)
    .move_axes(KeySet::wasd(), PadStick::Left);
```

使用：`frame.input.pressed(Act::Jump)`、`frame.input.move_axes()`（Property `x` 向右、`y` 向前）；相机相对移动写 `frame.input.move_axes().relative_to(camera_yaw)`。

规则：

- 一个 Package 至多声明一个 `const NAME: InputMap<E>`，再声明报 `E2202`；`E` 必须派生 `InputAction`，或是默认的 `InputAction` 本身（重映射默认动作集），否则报 `E2201`；
- Package 的每个输入动作都是 `E` 类型：`pressed`/`released`/`held` 接受 `E` 的 Variant，传入其他类型报 `E2103`；未声明映射时 `E` 是 `InputAction`（`jump`、`fire`、`interact`、`pause`），默认绑定 `Space`/`J`/`E`/`Escape`、`South`/`West`/`North`/`Start` 与四个触屏按钮，移动轴为 `KeySet::wasd()` 加左摇杆；
- `InputMap` 构造方法都是 `@const`，映射在编译期求值（§32）并进入 Schema：`new()`、`key(Key, E)`、`pad(PadButton, E)`、`touch(TouchButton, E)`、`move_axes(KeySet, PadStick)`、`dead_zone(r)`；`KeySet` 由 `wasd()`、`arrows()` 或 `of(up, left, down, right)` 构造；
- 一个动作可以有多个绑定，任一绑定按下即 `held`，边沿只在整个动作的 `held` 变化时产生；
- 死区与对角线归一化（长度不超过 1）由 `InputMap` 统一处理：摇杆半径不超过 `dead_zone`（默认 0.2，取值 `[0, 1)`）时为 0，之外线性映射到 `(0, 1]`；键盘方向与摇杆相加后长度超过 1 时归一化；
- 目标平台包含手柄或触屏时，缺少对应路径的动作报 `E9107`（警告）：桌面宿主包含手柄，声明了移动端 Target 的 Package 包含触屏；
- `InputSnapshot` 只保存动作与轴的值，不保存原始按键，所以 Input Tape 与键位重映射无关。

### 106.4 状态分层

Game Profile 把状态分为三层：

| 层         | 声明                    | 可写入方                                           | 进入 Snapshot    | 联机复制     |
| ---------- | ----------------------- | -------------------------------------------------- | ---------------- | ------------ |
| Simulation | System `state`（默认）  | `start`/Startup、FixedUpdate、CollisionListener    | 是               | 是           |
| Derived    | `computed`              | 只读 Simulation 状态，不可写                        | 否，Restore 后重算 | 否，各端重算 |
| Local      | `@local state`          | FrameUpdate、UI、Presentation                       | 否               | 否           |

Simulation 域是 `start`/Startup、FixedUpdate、CollisionListener（Schema 中声明为 Simulation 的 Hook）以及从它们可达的 `fn`/`action`/`computed`，跨模块沿调用图计算。编译期规则：

- `@local` 只标记 System 的 `state`，标在其他成员或 Component 上报 `E9103`；
- Simulation 域读写 `@local` 状态，或调用返回非 `()` 的 Presentation 方法，报 `E9103`；诊断的 note 指出该 Callable 由哪个 Hook 可达；
- Simulation 域使用非确定性来源报 `E9104`：Wall Clock、`Task`/`await`、未预加载的 Resource 结果、未注入的 Random、未声明有序的 Hash 容器迭代、宿主超越函数（§106.5）、UI State 与 Adaptive Environment；Native 的可复现档位低于 Package 的 `[game] determinism`（默认 `same_binary`）即为非确定性来源；
- Simulation 状态类型必须实现 `Snapshot`。值类型、Schema Enum 与 Native 值类型自动派生；闭包、`Task` 与 Native Handle 不能实现，否则报 `E9105`；
- Presentation 可以只读 Simulation 状态，读到的是本帧提交的 World Revision。

Simulation 可以触发粒子、音效、相机抖动和 Debug Draw，但只能作为 Presentation Command：

- 命令返回 `()`，不能影响 Simulation；Simulation 域调用 Presentation 函数时不立即执行，而是作为命令推迟到该 Tick 结束后按键序交付；发生 Fault 的 Hook 发出的命令随其状态写入一起丢弃；Presentation 域（FrameUpdate）中直接执行；
- 命令按 `(tick, source_system, sequence)` 标识；回放、回滚重算与 Restore 时，已交付 Tick 的命令不重复交付；
- Debug Draw 命令在 Release 中被移除：调用连同实参求值都不生成代码。

因此“玩法不依赖本地表现状态”是编译期保证。回放、回滚、存档和热重载都建立在这一分层上。

### 106.5 确定性浮点

`viso.toml` 的 `[game] determinism` 选择：

```text
same_binary      默认；同一目标、同一二进制可逐 Tick 重放
cross_platform   所有 Tier-1 目标上 Snapshot Hash 逐字节一致；联机回滚要求此档
```

`cross_platform` 要求：

- 禁止 FMA 收缩、fast-math、浮点重结合，禁止平台间不一致的 denormal 处理：解释器逐个运算舍入（`a * b - c` 先舍入乘积），表达式严格从左到右求值，`F32` 运算在 `F64` 中计算后舍入到 `F32`（对四则运算与 `sqrt` 与直接单精度运算逐位相同）；
- `sin`/`cos`/`tan`/`atan`/`atan2`/`exp`/`log`/`pow`/`hypot` 使用 `viso::math` 的确定性实现（只由 IEEE 基本运算与位操作构成的软件实现），不调用宿主 libm；结果按版本逐位固定，改变即破坏跨版本回放；
- `sqrt` 与四则运算按 IEEE 754 最近偶数舍入；
- Snapshot 与 Hash 中所有 NaN 编码为同一个 quiet NaN（x86 与 Arm 产生的 NaN 符号与 Payload 不同）；`-0.0` 与 `0.0` 保持区分；
- 归约与迭代顺序固定；并行浮点归约按固定分块顺序合并；
- Physics 等 Native World 在 Schema 中声明自己满足哪一档，不满足时报 `E9104`。

当前实现（Physics 集成契约，`viso_behavior::game::Physics`）：World 拥有 Entity、命令与接触事件，引擎拥有运动。引擎实现 `name`、`determinism`（所达档位）、`step(bodies: &mut StepBodies, dt, contacts)`（按 `StepBodies` 的槽位读写位置、速度、`on_floor`，用 `take_push` 取走本 Tick 提交的 `walk`/`jump`，并按 `order()` 中的位置追加重叠的 Body 对）、`statics_changed`（Block 变化或 Body 被替换）、`save`/`load`（引擎在 Step 之间保存的状态）与 `fork`（同类同设置的新引擎）；World 对接触对规范化、排序、去重并与上一 Tick 的接触集求差，只投递新开始的接触。内置引擎 `Kinematic` 是该契约的一个实现（`cross_platform`，状态全部由 Body 推导，保存为空）。宿主用 `Scheduler::with_physics(vm, seed, engine)` 安装其他引擎：引擎档位低于 Module 的 `determinism` 时返回 `GameError::Physics`（`E9104`），Logic Reload 与 World Rebuild 换入档位更高的 Build 时同样拒绝并保持原游戏；World Rebuild 的影子游戏使用 `fork` 出的新引擎。

### 106.6 Tick 计时器与冷却

```viso
import viso::game::{Cooldown, TickTimer};

export system Gun implements FixedUpdate {
    state cooldown: Cooldown = Cooldown::new(250ms);
    state wave: TickTimer = TickTimer::every(10s);

    action fixed_update(frame: FixedFrame) {
        if frame.input.held(Act::Fire) && cooldown.ready(frame.tick()) {
            cooldown = cooldown.fire(frame.tick());
        }

        if wave.due(frame.tick()) {
            wave = wave.rearm(frame.tick());
        }
    }
}
```

- `fixed_dt` 是编译期常量，`Duration` 在编译期换算为整数 Tick（向上取整，先舍入到整纳秒再做整数除法，`300ms` 在 4 Hz 下为 2 Tick）；实参必须是编译期常量，否则报 `E2501`，为负报 `E2112`；运行时只比较 Tick，不累加浮点时间；
- `Cooldown::new(d)` 初始即就绪：`ready(tick)`、`fire(tick)` 返回 `d` 之后才再次就绪的新值、`remaining(tick)`；
- `TickTimer::every(d)` 首次在 Tick `d` 到期（不足一个 Tick 按一个 Tick），`due(tick)`、`remaining(tick)`；`rearm(tick)` 把到期点推到 `tick` 之后的第一个周期整倍数，迟到的检查不漂移也不补发，未到期时不变；
- Timer 是普通值类型：进入 Snapshot，可回放、可迁移、可在调试器中查看；
- 不提供注册闭包的 `every`/`after` API；在 Simulation 状态中存闭包报 `E9105`；
- 只修改 Timer 初始值的 Logic-only Reload 保留当前剩余 Tick（§94.1）。

### 106.7 Snapshot 与 Restore

编译器为每个 Game Session 生成：

```text
GameSnapshot {
    build_hash
    tick
    rng_state  // 注入的 Seeded RNG 状态
    world      // Native World 通过 Schema 声明的 snapshot/restore：GameWorld 的全部槽位、空闲表、分配顺序与接触集
    physics    // Physics 引擎的 save 状态；引擎不能保存时缺省，Snapshot 为 degraded
    systems    // 每个 System 的 Simulation State，按 Stable ID 排序
}
```

- Snapshot 只包含 Simulation 层；Derived 在 Restore 后重算，Local 保留当前值；
- 内存中的 Snapshot 共享不可变的状态值，取一次不复制状态，供回滚与时间回溯；`build_hash` 是 Module 的哈希；
- 编码使用 Ende 二进制，按 Stable ID 与字段 Schema 版本化，可跨 Logic-only Reload 读取：System 与 State 按 Stable ID 升序，浮点按位写出，编码是规范的，Snapshot Hash 即其 64 位 FNV-1a；Restore 只写回 Stable ID 与类型 Schema 哈希（Record 字段名、Enum Variant 逐层展开）都一致的状态，其余保持当前值并计入 `mismatched`/`missing`；
- 截断或非 Snapshot 的数据解码报错，不 Panic；
- `restore(snapshot(s))` 后继续运行，与不中断运行逐 Tick 一致；
- 用途：Input Tape 回放、联机回滚、存档、时间回溯调试、热重载前后对比；
- Native World 未声明 snapshot 能力时，这些功能在诊断中明确降级，不静默失效。当前实现：引擎 `save` 返回 `None` 时 Snapshot 不含 `physics`（`GameSnapshot::degraded()` 为真，编码保留该标记，Blob 标记为 `GSN3`）；Restore 一个 degraded Snapshot 或引擎状态载入失败时，引擎以 `fork` 重新开始于恢复的 Body 之上，并在 `Restored::degraded` 中报告，此后的 Tick 可能与不中断运行不同。

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

当前实现：`@persist` 标在 System 或 Component 的 `state` 上，标在其他成员上报 `E9106`；Component 的持久化状态只属于视图自身挂载的 Component（一个键对应一个实例），持久化状态的 Component 被嵌入另一视图时报 `E9106`；键为唯一的非空字符串字面量参数，包内重复报 `E9106` 并指向第一处；授权来自 `Viso.toml [package] capabilities`（CLI §38）。编译器把每个持久化状态的键、槽位、类型 Schema（Record/Enum 按 Stable ID 描述字段、变体与字段默认值 Chunk）和类型拼写写入 Module，包内有持久化状态时一并写入 `@migrate` 函数。运行时宿主在 VM 上安装 `Persist` 服务（`PersistStore`：`MemoryStore`，或目录存储 `DirStore`——后台线程写临时文件、fsync 后原子 rename）；链接时未授予 `storage.persist` 则不加载也不写入，并报 `E6103`。Scheduler 在 `start` 之前逐个加载：存储的 Blob 带类型拼写与 Schema，类型相同则直接使用，否则按 §94.1 矩阵转换，再否则用 `from` 等于旧类型拼写、返回新类型的 `@migrate` 函数；任何失败使用 Initializer 并记录 `E9111` 报告（`take_persist_reports`），原 Blob 保留到该状态首次改变。写入发生在 Tick Boundary，只写值有变化的状态，默认每秒（`tick_rate` 个 Tick）至多一次（`set_persist_interval`）；Store 不阻塞 Tick；`suspend()` 与 Scheduler 析构写入变化并阻塞到落盘。World Rebuild 与 Logic Reload 把运行中的持久化值按键带入新实例（类型变化同样转换）；启动失败或影子游戏不写入。视图宿主（`ViewHost`）在视图首次挂载时取走其 `Persist` 服务（`load_view_with` / `load_static_view` 的 `setup` 在挂载前安装服务），在状态连到 Cell 之后、Effect 与 `start` 运行之前加载（未授予时每个状态报一次 `E6103`）；每次提交的写回之后只写值有变化的状态，未存储过的状态在首次写回时写入；`suspend()` 写入变化并阻塞到落盘；Hot Reload 带过已加载的值，编辑新标 `@persist` 的状态按键加载。宿主自身没有 `Persist` 服务时使用 App 安装的存储（`install_persistence`）：`view!` / `component!` 嵌入的宿主在构建节点树之前加载，Release Package 与 Hot Reload 挂载走同一路径；每个持久化的宿主在窗口的 `NodeStore` 上登记一个 suspend 钩子（弱引用宿主，视图释放后自动移除）。App Facade 启动时安装 App 数据目录下 `persist/` 的 `DirStore`（首次读写时才打开；数据目录：macOS/iOS 为 Application Support，Linux/BSD 为 `$XDG_STATE_HOME`，Windows 为 `%LOCALAPPDATA%`，Android 为 `getFilesDir()`；Web 为页面源的 `localStorage`），在 App 进入后台、窗口关闭与事件循环结束时调用 suspend；加载与写入失败的报告输出到 stderr。

### 106.9 渲染插值

- `frame.alpha(): F32` 取值 `[0, 1)`，等于累加器余量除以 `fixed_dt`；`SlowMotion` 积压超过一个 Tick 时取小于 1 的最大值；
- World 为每个 Entity 保留上一 Tick 与当前 Tick 的 Transform，Render Extraction 默认按 `alpha` 插值：`RenderFrame.position(id)` 与宿主的 `GameWorld::extract(alpha)` 返回插值位置；`teleport` 把上一 Tick 的位置也设为目标，本 Tick 不插值；
- FrameUpdate 属于 Presentation：只读 Simulation，只写 Local。

### 106.10 联机复制与回滚

Simulation 层即复制集（§106.4）：每个 Peer 运行整个游戏，只交换输入，不交换状态。

- 每个 Tick 有每个玩家的输入：`frame.players(): I64` 是玩家数，`frame.input_of(player: I64): InputSnapshot` 是该玩家的冻结输入（越界为 Native 错误），`frame.input` 是玩家 0 的；`QuickFrame` 相同。单机游戏只有玩家 0，宿主的设备输入就是它；
- 宿主 API：`Scheduler::set_players(n)`（1..=8）、`step_with(&[TickInput])` 用给定的每玩家输入运行一个 Tick、`sample_input()` 把设备输入冻结为本玩家的 `TickInput`（动作的 held/pressed/released 位，至多 64 个动作，与 Move 向量的位）、`present(wall_dt)` 只运行一帧的 Presentation 部分；
- `RollbackSession::new(scheduler, SessionConfig { players, local, input_delay, max_prediction, hash_interval })` 从游戏当前 Tick 开始；各 Peer 以同一 Build、同一 Seed、同一 Tick 启动，所有玩家的前 `input_delay` 个 Tick 输入为空。每个 Tick 采样本地输入作为 `input_delay` 个 Tick 之后的输入，对已知的玩家输入直接使用、未知的预测（该玩家最新已知输入保持按住、不带边沿），并保存 Tick 前的 Snapshot；
- 传输无关：`packet_for(peer)` 产出字节、`receive(bytes)` 接收，链路可以丢包、延迟、乱序。每个包重复对方尚未确认的全部本地输入（至多 128 个），确认已收到的对方输入，并携带最近的已确认 Snapshot Hash；包带 Build Hash，来自其他 Build、非本局远端玩家或格式错误的包被拒绝（`PacketError`），会话不变；
- 远端输入与该 Tick 使用的预测不同时，恢复该 Tick 的 Snapshot 并用当前已知的最佳输入重算到当前 Tick；更早的 Tick 使用的输入未变，因此结果与从最后确认的 Snapshot 重算相同；
- 一个 Tick 的全部玩家输入都已知即为确认；最后确认 Tick 边界的 Snapshot 是回滚基线（`confirmed_snapshot`），每 `hash_interval` 个确认 Tick 交换一次其 Hash，与对方同一 Tick 的 Hash 不同即记录 `Desync { tick, peer, local, remote }`；
- 运行超过最后确认 Tick `max_prediction` 个 Tick 时停顿等待远端输入，欠下的 Tick 至多保留 `max_prediction` 个；
- 重算走同一 Scheduler：已交付 Tick 的 Presentation 命令不重复交付（§106.4），预测错误的 Tick 已交付的命令不撤回；只回滚 Simulation 状态。
- 验收（`crates/dsl/tests/game_netplay.rs`）：两个进程内 Peer 经丢包 1/4、延迟 1–6 步并乱序的链路运行 220 个 Tick，发生回滚并收敛：双方同一确认 Tick 的 Hash 相同，且等于用双方真实输入单机运行的结果；每 Tick 一条的 Presentation 命令每条只交付一次。

---

## 107. 完整游戏 System 示例

```viso
import viso::game::{
    Startup,
    GameStart,
    FixedUpdate,
    FixedFrame,
    CollisionListener,
    CollisionEvent,
    EntityId,
    SpawnDesc,
    InputAction,
    GameTag,
};
import viso::math::Vec3F32;

export system PlayerController implements Startup + FixedUpdate + CollisionListener {
    state player: Option<EntityId> = Option::None;
    state move_speed: F32 = 6.0f32;
    state jump_speed: F32 = 10.0f32;
    state score: I64 = 0;
    state respawn_point: Vec3F32 = Vec3F32::new(0.0f32, 4.0f32, 0.0f32);

    action startup(cx: GameStart) {
        player = Option::Some(cx.spawn(SpawnDesc::player().at(respawn_point).tag(GameTag::player)));
        cx.spawn(SpawnDesc::block(Vec3F32::new(20.0f32, 1.0f32, 20.0f32)));
        cx.spawn(
            SpawnDesc::sensor(Vec3F32::new(1.0f32, 1.0f32, 1.0f32))
                .at(Vec3F32::new(3.0f32, 1.4f32, 0.0f32))
                .tag(GameTag::coin),
        );
    }

    action fixed_update(frame: FixedFrame) {
        match player {
            Option::Some(id) => {
                let world = frame.world;
                let movement = frame.input.move_axes();
                world.walk(id, movement.x * move_speed, movement.y * move_speed);

                if frame.input.pressed(InputAction::jump) && world.on_floor(id) {
                    world.jump(id, jump_speed);
                }

                if world.position(id).y < -20.0f32 {
                    world.teleport(id, respawn_point);
                }
            },
            Option::None => {},
        }
    }

    action collision(event: CollisionEvent) {
        match player {
            Option::Some(id) => {
                match event.other_of(id) {
                    Option::Some(other) => {
                        if event.world.has_tag(other, GameTag::coin) {
                            event.world.remove(other);
                            score += 1;
                        }
                    },
                    Option::None => {},
                }
            },
            Option::None => {},
        }
    }
}
```

World 不是 System 的 `input`：System 由 Scheduler 创建、没有调用方；World 是 Borrowed Handle，经 `frame.world`、`cx.world` 与 `event.world` 在 Hook 内取得，不能存入 State。

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

Native GameWorld 的 mutating Method 应被标为 `native action`。在多线程或确定性 Profile 中，这些调用可以 Lower 为 Command Buffer；`viso::game::GameWorld` 的写入总是进入 Command Buffer：

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

`viso::game::GameWorld` 的规则：

- 读取（`is_alive`、`position`、`velocity`、`on_floor`、`has_tag`、`query`、`entities`）是非 `deterministic` 的 `fn`（Read），看到的是已提交的 World Revision，本 Tick 尚未提交的命令不可见；`position`/`velocity` 读取不存活的 Entity 是 Native 错误；
- 写入（`spawn`、`walk`、`jump`、`teleport`、`remove`）是 `action`，按发出它的 System 记入 Command Buffer；Hook Fault 时与其状态写入一起丢弃；在 Simulation Hook 之外（如 `FrameUpdate`）写入是 Native 错误（`E7106`）；
- 提交点：启动事务成功后（首个 Tick 前）、一个 Tick 的全部 `FixedUpdate` 之后、全部 `PrePhysics` 之后、该 Tick 的 `CollisionListener` 之后、全部 `PostPhysics` 之后（§106 `physics_order`）；每个提交点按 `(system_order, sequence)` 合并，同一 System 的命令保持发出顺序，因此结果只取决于 System 顺序；
- `walk(id, x, z)` 与 `jump(id, speed)` 累加，被下一次 Physics Step 消耗；`teleport(id, to)` 最后一个获胜，使 Body 静止，本 Tick 不插值；`remove(id)` 结束 Entity，合并顺序中其后针对它的命令被跳过并计数；`spawn(desc)` 立即返回 `EntityId`，Body 自提交起存在；
- Physics Step 在 `PrePhysics` 与 `CollisionListener` 的提交点之间，由 World 的 Physics 引擎执行（§106.5）；内置引擎中 Character 的水平速度为本 Tick 的 `walk`，`jump` 加到竖直速度，重力 `9.81 m/s²` 向下；逐轴（先竖直）对 Block 求解，向下被挡设置 `on_floor`；Step 之后报告新开始的接触（Character 与 Sensor、Character 与 Character），每对中先分配者在前、按分配顺序排列，作为该 Tick 的 `CollisionEvent`（`first`、`second`、`other_of(id)`）投递，排在宿主排队的接触之后；
- `SpawnDesc` 是值类型：`player()`、`character(size)`、`block(size)`、`sensor(size)`、`prefab(prefab)`（§105.2），`.at(pos)`、`.tag(tag)`、`.model(model)`；
- 全部运算为单精度 IEEE、无 FMA，Schema 声明 `cross_platform`；邻近查找用均匀网格 Broadphase（格宽取不小于最宽 Character 的 2 的幂，Block 网格只在 Block 变化时重建），结果与按分配顺序逐对测试逐位一致，开销由 `game_world` Benchmark 跟踪。

### 108.1 迭代与查询

- `world.query(tag)` 与 `world.entities()` 按 EntityId 分配（提交）顺序迭代；
- EntityId 是槽位与 Generation：同一 Session 内 EntityId 不复用，槽位复用时 Generation 递增，过期 Id 不会指向新 Entity；
- Tag 是 `@derive(GameTag)` 的用户 Enum 或 Schema Enum `viso::game::GameTag`（`player`、`enemy`、`ally`、`coin`、`pickup`、`hazard`、`goal`、`projectile`、`platform`、`trigger`），不是字符串：一个 Package 至多一个 Enum 派生 `GameTag`，再派生报 `E2202`；派生的 Enum 必须无 Payload 且至多 64 个 Variant，否则报 `E2201`；Package 的每个 Tag 参数都是该类型，传入其他类型报 `E2103`；
- 迭代中的 Remove/Spawn 进入 Command Buffer，迭代结束后提交。

### 108.2 Release 执行形态

- Dev 使用 Bytecode，以支持 Logic-only Reload；
- Release 可以把 System IR 降低为 Rust，与 App 一起编译。语义以 Bytecode 为准，两者对同一 Input Tape 做差分测试，Snapshot Hash 必须一致；
- Native Lowering 的性能收益是假设，由 Release Benchmark 证实后才能成为默认。

当前实现（`viso_behavior::aot`）：System IR 即 System 运行的已验证 Bytecode——各 System 的 Hook、State Initializer 及它们调用或闭包引用的全部 Chunk。`lower_systems(module, crate_path)` 把它逐 Op 写成一个 Rust 模块：每个 Chunk 一个函数，寄存器是局部变量，基本块是块状态机的分支，另有 `NATIVE` 表（带 Module 的 Build Hash）；输出是确定的。App 把该模块与自身一起编译，`Vm::install_native(&NATIVE)` 之后同一 Scheduler 调用这些 Chunk 时运行编译后的代码，其余 Chunk 仍解释执行；Build Hash 不符时拒绝安装（`NativeCodeMismatch`）。编译后的代码每个 Op 先消耗一单位 Fuel，经同一路径调用 Native、计入同样的内存与调用深度、写入同一 Undo Log，Fault 时记录解释器 Cursor 应在的位置，因此预算、事务、Fault 的种类、消息与源位置都与 Bytecode 一致。差分测试（`crates/dsl/tests/game_native.rs`）用同一 Input Tape 在两种形态下逐 Tick 比较 Snapshot Hash 与每个 Fault（含受限预算下的 `E9102`），并检查仓库中的生成代码与当前 Lowering 输出逐字节相同。Release Benchmark（`benches/game_native.rs`，本机 macOS，一个 Fixed Tick）：Bytecode 密集的 `crowd` 为 145.6 µs → 84.6 µs（约 1.7×），大部分时间在 World、Physics 与 Native 中的 `colony` 为 29.2 µs → 26.9 µs（约 1.1×）；收益只在本机测得，其他目标未测。Release 构建（`viso build --release`）中 `view!` 把视图的整个 Module（含 System 的全部 Chunk，`lower_module`）降成 Rust 并只携带去掉 Bytecode 的 Module（ADR 0041）；视图挂载的 Game Session 与其音频 Host 各自在自己的 VM 上 `install_native` 同一份代码（`GameSession::start(module, native)`），dev reload 换入的 Build 则解释执行。

### 108.3 AudioProcess 实时域

- `AudioProcess` System 在音频线程运行，属于 Presentation 层；
- 禁止堆分配、`Task`/`await`、锁、Resource 加载、未标 `realtime` 的 Native 调用以及无静态上限的循环，违反时报 `E9108`；
- 与其他 System 只通过有界无锁队列传递 typed message。

当前实现：`viso::game::AudioProcess` 的 Hook 为 `audio_process(block: AudioBlock)`，域为 `Realtime`；`AudioBlock` 是借用 Handle，按声道与帧原地读写样本（`channels`、`frames`、`sample_rate`、`input(channel, frame)`、`write(channel, frame, sample)`，均为 realtime-safe）。编译器从每个 `AudioProcess` Hook 沿调用与 Closure 遍历降低后的指令：构造 Record/Tuple/Enum Payload、List、Closure 或 `String`（拼接、格式化），原地写入 Record/List（共享时复制），`start` Task，`emit` 事件，调用 Closure 值，调用未标 realtime-safe 的 Native，以及递归，均报 `E9108`；源码中读取 Resource、`while`/`loop`，以及 `for` 不是遍历头部书写的 Range、或 Range 边界不只由字面量、`const` 与 `AudioBlock` 的方法构成时也报 `E9108`；诊断附注说明由哪个 Hook 到达。实现 `AudioProcess` 的 System 只能再实现 Hook 同在音频线程的 Trait（`AudioCommands`），否则报 `E9108`。头部书写的 Range 由其边界直接迭代，不构造 Range 值。

Typed Message：包内唯一的 `@derive(AudioCommand)` Enum 是发往音频线程的消息类型，唯一的 `@derive(AudioEvent)` Enum 是音频线程发回的消息类型（未派生时为 `()`；再派生报 `E2202`）。Command 按值复制跨线程，Payload 只能是数值、`Bool`、`Char`、无 Payload 的 Enum 及由它们组成的 Record/Tuple，展平后至多 16 个 Token（Variant 本身与每个 Record/Tuple 各算一个），否则报 `E2201`；Event 在音频线程构造，不得分配，因此只能是无 Payload 的 Enum（`E2201`）。`viso::game::send_audio(command) -> Bool` 是 Presentation `action`（Simulation Hook 中调用按 Tick 延迟交付，回放不重发），队满或未连接音频线程时返回 `false`；`AudioCommands.audio_command(command)` 在音频线程、下一个 Block 之前按发送顺序收到每条 Command（Realtime 域，同受上述规则约束）；`block.send(event) -> Bool`（realtime-safe）把 Event 交给下一 Frame 中、`FrameUpdate` 之前运行的 `AudioListener.audio_event(event)`（Presentation 域）。两个方向各是一条有界（256 条）单生产者单消费者无锁环形队列，满时丢弃并计数（`dropped_commands` / `dropped_events`）。

音频线程宿主：`AudioHost::new(module_bytes, natives, channels, max_frames, sample_rate)` 从 Module 的字节解码出独占的 Module、VM 与实例（因此可整体移到音频线程），返回宿主与 `AudioLink`；`Scheduler::attach_audio(link)` 让 `send_audio` 与 `AudioListener` 生效，World Rebuild 与 Logic Reload 保留连接。每个 Block：Block 输出清零，解码队列中的 Command（每个 Variant 复用上一条的值，原地覆盖），依 System 顺序调用 `audio_command`，再调用 `audio_process`，最后把 Block 复制到设备缓冲区。每次调用的预算按 Block 缩放（每个 Sample 16 次 Native 调用）。Hook Fault 使该 System 静音直到宿主重建，Fault 以非阻塞方式留给 App（`AudioStatus::take_fault`）。预热后的调用路径零分配：1000 个 256 帧立体声 Block（含 Command 解码、两个 Hook 与 Event 发送）零次堆分配与释放（`game_audio_alloc` 测试以计数 Global Allocator 验证）。设备输出：`viso_platform::audio::AudioOutput`（macOS/iOS 为 Output Audio Unit，Windows 为 WASAPI 共享模式事件驱动，Linux 为 ALSA，Android 为 AAudio；后两者运行时加载；Web 的线程构建（`[profile.<name>.web] threads = "on"`）为 AudioWorklet：worklet 在自己的线程上以页面的 shared memory 实例化模块，每个 128 帧 Block 调一次 render；非线程的 Web 构建与 BSD 返回 `Unsupported`），以 float32、声道主序回调渲染；`viso::audio::GameAudio::start(module, natives)` 把宿主接到默认输出设备。

---

## 109. HUD 和游戏场景组合

游戏 Scene 与 UI HUD 使用普通 Component 组合：

```viso
system Score implements FixedUpdate {
    state points = 0;

    @query fn score() -> I64 { points }

    action fixed_update(frame: FixedFrame) { /* … */ }
}

export component GameScreen {
    view {
        node root: Stack {
            GameViewport {}

            Column {
                Text {
                    text: format("Score: {}", game.score());
                }

                if game.is_paused() preserve "pause-menu" {
                    PauseMenu {
                        on resume {
                            game.resume();
                        }
                    }
                }
            }
        }
    }
}
```

UI 对游戏状态的读取只经 Reactive Query（ADR 0045）。System 中标 `@query` 的 `fn` 是 Query：无参数，返回值类型（数值、`Bool`、`Char`、`String`、Enum 及由它们组成的 Record/Tuple/List/Option；不能是 Closure 或 Native Handle），在 Read 域对 System 已提交的状态求值；高频 World 数据不逐 Entity 驱动 Widget Tree，而由 System 的 Hook 汇总进状态再经 Query 读出。View 中注入绑定 `game` 指视图根节点运行的游戏（与 `env`、`theme` 一样最后解析，只在 View 中可用），只能以调用读取：`game.<query>()` 按名字读包内的 Query，`game.is_paused() -> Bool` 与 `game.tick() -> I64` 是标准 Query，`game.pause()` 与 `game.resume()` 是 Presentation `action`（只在允许 Action 处调用）。裸 `game`、其他方法、或包内没有 System 时读 `game` 报 `E2001`；`@query` 标在 `fn` 以外、带参数、返回非值类型，或两个 System 声明同名 Query 报 `E9112`。

当前实现：Module 声明了 System 的 View（`view!`、`component!`、Package 与 Dev Plan 三条挂载路径一致）在其根节点上运行一个 `GameSession`：它用同一 Module 建自己的 VM，以 `Natives::standard` 与 Package 的 Capability 链接，按已安装的持久化 Store 持久化，启动失败时根节点不运行游戏，原因排入 `take_game_start_faults`。Session 是该窗口 Store 的 Frame Client，由根节点拥有：每帧在 Hot Reload 提交之后、State Flush 之前以帧的 Wall Delta 推进——先运行时钟欠下的全部 Tick，再运行全部 `FrameUpdate`；未暂停时让帧循环持续出帧。没有 UI 节点持有键盘焦点、或焦点在 `GameViewport` 上时，按键（不含自动重复）按 US 布局位置交给游戏；UI 节点获得焦点或窗口失去焦点时释放游戏持有的全部按键，仍按下的动作在下一 Tick 得到 `released`（§106.2）。根节点释放后的第一帧 Session 随之结束；Hot Reload 重建根节点时 Session 移到新根上继续运行。HUD Query：每个被读取的 Query 降为视图组件的一个隐藏 State Slot（同 `env` 字段，内联实例读同一 Query 共用视图自身的 Slot），Package 以 `GAME` 段、`view!` 以 `__link_game`、Dev Plan 以提交时的链接带上读取表；Session 在每帧的 Tick 与 `FrameUpdate` 之后以无 World 打开的方式对每个被观察的 Query 调用一次其 Chunk 并保存结果，值变化时才在该帧 State Settle 开始时提升该 Query 的 Revision Cell，只有读它的 Binding 与 Region 重新求值；View Host 在 Cell 变化时从 Session 保存的值重填 Slot，不调用 VM。视图在 Regions 与 Values 首次求值之前启动并观察游戏，因此首帧即显示游戏的值。`game.pause()`/`game.resume()` 是 `viso::game::session` 的 Action，经 View Host 借给 VM 的 `GameControl` 在下一帧生效；暂停的游戏仍运行 `FrameUpdate` 与 Query。保留 Session 的 Reload 以 Query 名重新链接同一观察；新 Build 不再声明的 Query 保留最后的值并报告一次。Web Export 拒绝运行游戏的视图。`GameViewport` 显示其根节点 Session 的 World（ADR 0047）：节点内容是一个 3D Scene，每帧在 Session 的 Tick 与 `FrameUpdate` 之后由 Session 填充——相机取 `camera:`（`Option<CameraRig>`，自带缓动）或 Stage 的相机，实例取 `GameWorld::extract` 在时钟 Alpha 处的结果（地形即 `Model::Ground` 物体），每个 `Model` 是一个内建形状或按 Prefab 尺寸布置的组合形状，朝向沿物体 Facing；World 与相机都未变时不提交新帧、不重绘。未写宽高的轴默认 Fill；可获得焦点，指针按下即获得焦点；语义角色 `image`。Rust 侧以 `viso_view::game_viewport(store, node, rig)` 建同样的节点。

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

比较对象是 Last-good 与 Candidate 的 Behavior IR：函数按声明的 Stable ID 配对，逐条比较指令；调用比较被调函数的身份，闭包比较其函数体，Native 调用比较路径与签名，状态读写比较所读写状态的名字而不是槽位，因此移动代码、改注释或在别处增删声明都不触发重载。一个 Hook 可达（经调用与闭包）的任一函数改变，即按该 Hook 的阶段定层：Start Hook 为 World Rebuild，`FixedUpdate` 与 `CollisionListener` 为 Logic-only，`FrameUpdate` 为 Presentation-only。Hook 绑定的增删与改绑同样按其阶段定层；新增或删除的 System 按它的 Hook 与状态定层；System 执行顺序或 Tick Rate 变化为 Logic-only。所有改动中最高的一层即本次重载层，以提示诊断 `E5103` 报告：主 Span 是决定层级的第一个改动，其余改动列为 Related。Shader 改动走 §110.3 自己的管线，不在这一比较之内。

`hotreload::game::swap` 把 Candidate 按其层级应用到运行中的游戏：无改动不切换；Presentation-only 与 Logic-only（含 State Migration）在帧之间 `reload`——新 Build 接管状态后先在副本上跑一个 Smoke Tick（命令丢弃，状态、World、时钟与输入随后还原），故障则放回原 Build、实例、Hook 与时钟并返回该故障；World Rebuild 走 §110.2 的 Shadow 重建；Candidate 不能校验、链接，或其实例、Start、Smoke Tick 故障时返回错误，运行中的游戏不变。

### 110.1 Logic-only Reload

- 替换 System Bytecode；
- 保留 World；
- 保留兼容 System State；
- 在 Tick Boundary 原子切换；
- 当前 Tick 使用其他实现完整执行，不允许半 Tick 混用版本。

当前实现：`Scheduler::reload(vm)` 在两次 `frame` 之间切换，此处既是 Tick Boundary 也是 Frame Boundary，Presentation-only 改动走同一入口，Simulation 状态、World、Tick 与 RNG 原样保留。Simulation State 按 Stable ID + Schema Hash 保留；`@local` 状态同样按 Stable ID + Schema Hash 保留（模块为每个 System 记录 `@local` 状态的身份），值中含 Closure 的除外（Closure 指向旧 Build 的代码），其余取 Initializer。

切换分两步，使一个 revision 能原子地切换多个游戏：`prepare_reload(vm)` / `prepare_rebuild(vm, keep)` 证明切换（新 Build 接管、Smoke Tick 跑过）并返回 `PreparedSwap`，旧 Build 保留其中，游戏在结清之前不推进；`commit_swap(prepared, revision)` 提交（录制中的 Tape 记下该 patch，§110.5），`abort_swap(prepared)` 放回旧 Build。`reload` 与 `rebuild` 是二者连用。

运行中的游戏由 `viso_view::GameSession` 持有：一个 view 的 module 声明 System 时，挂载在其根上启动 session——自己的 VM，链接标准 Native 与 package 的 Capability——由 window 的 frame loop 推进，没有 UI node 持有键盘焦点时接收按键，根释放时结束（`Viso_Hot_Reload.md` §16）。

音频：module 含音频 System 的 session 在 app 安装的设备上启动它们（facade 在默认输出上安装，只为含音频 System 的 module 打开设备），reload 新增音频 System 时同样启动。reload 时新的音频程序——VM、实例、它自己的 Block 与 Message Slot——在音频线程之外构建并预热，经 host 的两个原子指针发布，在下一个 Block 之前装上；换下的程序交回游戏线程，每帧回收。队列与状态留在 host，事件队列随新 Block 移交，游戏持有的链接保持有效；装入与新程序的首个 Block 都不分配。新音频 System 构建失败时旧的继续发声，故障以一条日志报告，切换照常提交。

### 110.2 World Rebuild Reload

- 在 Shadow World 执行新 Build Script/System；
- 运行 Schema、Smoke Tick 和预算检查；
- 成功后交换；
- 失败保持 Last-good World；
- 可选通过 Stable Entity Key 迁移玩家状态。

当前实现：`Scheduler::rebuild(vm, Rebuild)` 用新 Build 组装 Shadow 游戏（新实例、按原 Seed 重置的空 World、沿用当前时钟配置与按新 InputMap 重映射的持有输入），在 Tick 0 运行 Start，再在副本上跑一个 Smoke Tick（其命令丢弃，状态、输入与 World 随后还原）；Start、Smoke Tick 或预算（`E9102`）任一故障都返回该故障，正在运行的游戏与其 Build 不变，成功才整体替换。`rebuild_world(Rebuild)` 对当前 Build 做同样的重建。`Rebuild::KeepCharacters` 按 Stable Entity Key 迁移角色：Key 是角色的 Tag 集合加上它在同 Tag 集合的存活角色中按分配顺序的名次，新 Start 生成的同 Key 角色取得旧角色的位置、上一 Tick 位置、速度与着地状态；Block 与 Sensor 以新 Start 为准。

### 110.3 Shader Reload

- 后台编译新 Pipeline；
- Validation 成功后在 Frame Boundary 交换；
- 失败继续使用当前可用 Pipeline；
- 错误回传源码位置和 Backend 日志。

当前实现：每个 Shader 一个后台编译线程。提交的源码依次经过前端（解析、检查、降为 Program IR）、Validation、Layout 与 Backend 编译；线程开始编译前跳到最新提交的源码，结果到达时若已有更新的提交则丢弃，因此较新的编辑取代较旧的。宿主在 Frame Boundary 轮询：成功则交换 Pipeline 与 Interface，并交回被替换的 Pipeline 供宿主在 GPU Fence 后释放；Instance 或 Uniform Layout 变化时一并交回迁移——新成员取同名同类型的旧成员，其余从零开始——宿主据此重新编码出新 Buffer，与新 Pipeline 在同一帧生效。失败时当前 Pipeline 不变，回传前端与 Validation 的诊断（代码、消息、源码 Span）或 Backend 编译日志。

### 110.4 回溯重放（Dev）

- Dev Runtime 维护 Snapshot 环形缓冲（默认最近 10 s），并持续记录 Input Tape；
- Logic-only Reload 后可以选择“从 T−k 重放”：Restore 旧 Snapshot，用新代码重跑记录的输入，再从当前 Tick 继续；
- 开发者由此直接看到改动对刚才那段玩法的影响，不必手动重现。

当前实现：`Scheduler::keep_history(seconds)`（Dev 宿主默认 `DEFAULT_HISTORY_SECONDS` = 10）在每四分之一秒的 Tick Boundary 记一个 Snapshot，环中只留覆盖窗口所需的 Snapshot，并持续记录每个 Tick 读到的输入；窗口之前的输入折叠为窗口起点前一 Tick 的持有状态。`replay_from(tick)` Restore 不晚于 `tick` 的最近 Snapshot（另一 Build 的 Snapshot 按 Stable ID + Schema Hash 恢复共享的状态），用记录的输入在当前代码上重跑到离开时的 Tick；正在回放的 Tape 照常供给输入，期间设备输入保留到下一 Tick；已投递 Tick 的 Presentation 命令不再投递，`@local` 状态保持。Tick Rate 变化的 Reload 与 World Rebuild 清空历史。`tick` 早于最旧 Snapshot、尚未运行或未保留历史时返回 `ReplayError`，游戏不变。Dev session 中每个 Game Session 自启动即保留历史；`viso game replay`（`Viso_CLI.md` §22.3）经 dev channel（`GameAsk::Replay`）在帧边界让运行该文件的全部 Session 回放，Logic 层 reload 的提交行提示该命令。

### 110.5 游戏测试与 AI 工具合同

命令形状由 `Viso_CLI.md` §22.3 定义：`viso test game`、`viso game record`、`viso game peek`。

- Input Tape 是版本化 Ende 文件，包含 Seed、Build Hash、determinism 档位、`fixed_dt`，以及按 Tick 的 InputSnapshot（动作与轴，不含原始按键）；另有可手写的文本形式，例如 `30: press Jump`、`31..90: axis move = (1, 0)`；
- `@probe` 状态每个 Tick 输出到 JSON Trace，Scenario 可以对 Probe 断言；
- 测试输出 Snapshot Hash、最终 Entity Snapshot，以及可选的 Headless 帧截图 Sheet；
- 同一 Build + Tape 的 Snapshot Hash 在所选 determinism 档位下逐字节一致；
- `--json` 使用 §138 同一 Diagnostic Schema，AI 可以据此闭环。

当前实现（Input Tape，`viso_behavior::game::InputTape`）：Tape 头部是 Seed、Build Hash（手写为 0）、determinism 档位与 Tick Rate（即 `fixed_dt = 1 / tick_rate`）及覆盖的 Tick 数；正文是按 Tick 的 InputSnapshot，存为相邻 Tick 之间的变化——动作按下（带 press 边沿）、松开（带 release 边沿）、静默置位（无边沿，如 Remap）与 Move 向量，同一 Tick 的变化按序在该 Tick 运行前生效，因此同 Tick 的按下加松开即一次 Tap。动作按名字存放，回放时按名字映射到当前 Build 的动作集，Tape 中有 Build 没有的动作、或 Tick Rate 不同，拒绝回放。二进制形式是带 `GTP1` 标记的版本化 Ende Blob，解码有界；文本形式一行一条：`30: press Jump`、`40: release Jump`、`95: tap Fire`、`hold`/`unhold`（无边沿）、`31..90: axis move = (1, 0)`，区间 `a..b` 在 `a` 生效、在 `b` 撤销（`a..=b` 在 `b` 之后撤销：按下变松开，Move 回到 `(0, 0)`），头部行 `seed`、`build`、`determinism`、`tick_rate`、`ticks`，`#` 起注释，出错报告行号。`Scheduler::record` 录制每个 Tick 实际读到的输入，`stop_recording` 取出 Tape；`Scheduler::play` 回放时设备输入不再起作用；Logic Reload 按名字重新映射，World Rebuild 让回放与录制都从 Tick 0 重新开始。同一 Seed 与 Tape 的回放得到相同 Snapshot Hash（`GameSnapshot::hash`）。录制中的 run 每次 live reload 记一个 `TapePatch`（应用前的 Tick、dev revision、新 Build Hash、新 Build 的 module、被重置的状态数），并记下 run 开始时的 module；二进制形式携带它们，文本形式只以注释 `# <tick>: patch r<revision> build 0x…, <n> states reset` 列出。回放核对开始 Build，经 `Scheduler::set_linker` 设的 `Linker` 预先链接全部 patch（链接失败即拒绝回放），并在同一 Tick 之前换入；倒回时之后的 patch 折叠为倒回 Tick 处的一个（`Viso_Hot_Reload.md` §18）。

当前实现（`@probe`）：`@probe` 只标在 System 的 Simulation `state` 上，标在 `@local` 状态、其他成员或 Component 上报 `E9110`；它不改变运行时，也不进入 Release 模块，只由编译器在 Behavior IR 中标记（`SystemLayout::states` 记下每个 System 状态的名字、槽位与类型形状，`@probe` 状态带标记；`viso game peek` 按同一形状写出任意状态），测试 Runner 每个 Tick 按形状把值写进 JSON Trace：`Bool` 写 `true`/`false`，无符号整数按无符号写，单元枚举变体写名字，带载荷变体写 `{"Name": 载荷}`，Record 写字段对象，`Option` 的 `None` 写 `null`；形状不描述的类型（`Vec3F32` 等 Native 值类型、递归类型）按结构写，聚合值写成字段数组。

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
