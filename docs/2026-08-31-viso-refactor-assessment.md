# Viso 重构计划评估

> 状态：Assessment 1.0
> 日期：2026-08-31
> 评估对象：`viso/AGENTS.md` + `viso/Viso_Architecture_and_Migration.md`，对照当前 `makepad/` 代码库
> 结论用途：作为 Phase 0 启动前的架构评审备查记录

---

## 结论一句话

这是一份罕见地成熟的框架重写计划——诊断有代码依据、方向正确，且用"迁移语义而非架构 + 先建 baseline"两条原则规避了重写项目最常见的死法。主要风险不在架构对错，而在长尾工作量被低估和 baseline 基础设施的前置性。

---

## 1. 诊断准确（已在代码核对）

| 文档论断 | 代码验证 |
|---|---|
| `WidgetRef` 是 `Rc<RefCell<Option<..>>>` 动态包装 | ✅ `makepad/widgets/src/widget.rs:477` |
| 一个 `Widget` 对象承担事件/绘制/脚本/选区/树遍历所有身份 | ✅ `Widget: WidgetNode` trait 挂 50+ 方法 |
| `platform` crate 职责膨胀 | ✅ 直接依赖 script/network/video/studio-protocol/live-reload |
| 当前源码格式是 `script_mod!`/`ScriptVm` 而非 `.live` | ✅ 已核对；文档特意更正这点是对的（dev 分支已从 `live_design!` 迁走） |

这些不是"某个文件写坏了"，而是长期迭代后自然出现的架构耦合。文档的诊断是可信的。

## 2. 架构方向——认同

- 四条总原则（外部声明式/内部 retained、外部对象化/内部 DOP、开发期 dynamic/发布期 AOT、冷路径可抽象/热路径拒绝抽象税）是经过验证的框架哲学。
- generational `NodeId` arena、SoA 热/温/冷分区、phase-specific `Cx`（禁止 layout 里发网络请求）、精确 dirty 失效类（STRUCTURE/STYLE/MEASURE/LAYOUT/TRANSFORM/PAINT/HIT_TEST/SEMANTICS）、自动 batching + 编译期 backend 特化——全部对症下药。
- **`viso runtime` 不认识 makepad、无运行时桥、无双 runtime 共存**——这是整份计划最正确的决定。"Migrate semantics, not architecture" 避免了绝大多数"重写框架"项目死掉的方式。
- Phase 0 先建 characterization baseline（截图/input tape/性能指标）再动手——这是把"重写"变成"可验证迁移"的关键。

## 3. 风险点（执行时盯住，非否决）

1. **长尾工作量**：232 crate + `viso-ext/` 30+ 扩展 + 大量 apps/examples。Phase 7（widgets 原生重写）和 Phase 10（源码迁移工具）最容易被低估。建议给 widgets 排优先级（先 Button/Label/Text/Scroll/List，PDF/chart/map/webview 归 optional integration）。
2. **Phase 0 baseline 是硬前置**：如果 characterization suite（截图/input tape/性能指标/a11y snapshot）不先建好，后面每个 Phase 的"退出标准"会退化成主观判断。必须是真正做的第一件事之一。
3. **线性 11 阶段的验证盲区**：纯线性推进时，架构假设（NodeArena + 自动失效 + 自动 batching 的组合）要到很晚才端到端验证。建议在 Phase 3–5 之间插一个最小垂直切片（一个 Button 从 `.vs` → NodeArena → paint → GPU 端到端跑通）作为 checkpoint。
4. `viso/crates` 目前为空——这是纯 greenfield，不是原地改 makepad，这点计划本身是自洽的。

## 4. 一致性小问题（不影响方向）

- AGENTS.md §4 顶层布局把 `cli/inspector/studio/packager` 放在 `tools/`，架构文档 §8 一致——OK。
- `.vs` 扩展名、facade `viso::run::<App>()` 在 AGENTS.md 和架构文档间一致，无冲突。

## 5. 迁移执行约定（本次已确认）

- **代码来源策略**：参考行为，原生重写。`makepad/` 仅作 behavior / UX / regression / performance baseline 参考，保持只读。可复用的**算法思想**（如 Turtle 单遍布局、shaping、atlas）允许移植，但必须先定义 viso 边界再迁实现，不得反向修改 viso API 去适应旧结构。
- **忽略范围**：`viso-ext/`（用户自建插件库）不在本次迁移范围内。
- **起点**：Phase 0 —— 锁定架构契约 + 空 workspace 骨架 + 依赖方向 CI gate + characterization/benchmark 基础设施占位。
