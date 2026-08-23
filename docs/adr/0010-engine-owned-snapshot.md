# ADR-0010 · 引擎能力边界：owned snapshot 合同

- 状态：已接受（2026-08-23）
- 决策层级：难逆（并发模型、后端替换资格与走查交叉校验建立其上）

## 背景

ADR-0006 将字符度量与光栅化隐藏在 `PdfInspector` / `Rasterizer` trait 之后，但未定 trait 的表面形态。三个事实约束它：

- PDFium wrapper 资格实验（`docs/05-pdfium-backend-qualification.md` §5）把 mimus 必需而候选 wrapper 未暴露的能力（T1 字符诊断、F1 字体快照、O1 对象来源映射）全部表述为 owned snapshot / owned mapping——这实际上已经是对**任何**后端的需求规格；
- 同实验 §7 实测 PDFium 调用受进程级全局锁约束，4→8 worker 即平台化：借出句柄的 API 会把锁与生命周期泄漏进 pass 代码，与决策 #12 的页内 rayon 并行冲突；
- 资格结论（CONTEXT 决策 #37）要求后端可整体替换。trait 表面若引用具体 wrapper 的类型，替换就不可能零改动。

## 决策

1. `PdfInspector` / `Rasterizer` 的方法**一次性返回 owned 快照**：inspect 返回页字符快照（unicode/unicode_value、baseline origin、tight/loose box，后续按需扩展 T1 诊断与字体字段），rasterize 返回 owned RGBA8 位图。调用返回后不保留任何指向后端内部状态的借用。
2. **快照类型由 `mimus-core` 自行定义。** trait 签名与快照结构不得出现 `pdfium-render`（或任何后端）的类型；`pdfium-render` 依赖只允许出现在 `engine/` 的实现模块内，pass 代码不得引用后端类型。
3. **后端替换流程**：实现同一组 trait，并按 `docs/05` 的资格矩阵在固定 revision 上复跑（对 firecrawl-pdfium 而言前置是补齐 T1/F1/O1）。行为对拍一致本身不足以触发切换（与 ADR-0006 一致）；替换不得引起 pass 代码改动。
4. M1 方法集只取 #14 所需最小子集（页数、页几何、页字符快照、页光栅）；诊断与字体字段随 #18/#19 扩展，扩展只加方法/字段、不改既有语义。

## 后果

- PDFium 的串行段收窄为快照抽取一步，pass 侧全部工作在 owned 数据上进行，天然兼容页内 rayon 并行与 V1 单进程决策（CONTEXT #17）。
- 每页数据被完整复制一次，内存开销上升；被"V1 无性能硬指标"（CONTEXT #28）接受。
- 后端崩溃（abort）面被限制在快照抽取阶段，与 ADR-0006 的风险接受一致。
- 事后无法向后端补查——快照即全部；需要新数据时必须扩展快照结构。这是有意的：它强制能力需求显式化，正是资格矩阵可以逐项审计的前提。
