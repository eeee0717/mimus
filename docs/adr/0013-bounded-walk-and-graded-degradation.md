# ADR-0013 · 有界走查、分级降级与非直立隔离

- 状态：已接受（2026-08-24）
- 决策层级：混合——边界数值与恢复语义可逆（复议时更新本 ADR 与对应 fixture 预期即可）；§5 的降级报告形状一经发布即属公开合同，受 ADR-0011 §6 演进规则约束，难逆；§3 的写回模型是难逆（三个 Issue 的保真上限建立其上）
- 2026-08-28 修订（#98）：允许有界规范化有限、非退化但轴端点倒序的 Form `/BBox`，并为恢复诊断增加有界对象定位。

## 背景

#17、#18、#19 联合设计时发现三者被同一堵墙挡住。截至 `2c818a5`，生产路径的实际状态是：

- 严格 walk（`walk/mod.rs`）只认 `BT/ET/Tf/Tm/Tj` 五个操作符，且限定每页恰一个 content stream、恰一个文本对象、恰一次 text-show；`Do`、`TJ`、`cm`、`q/Q`、`Tr` 全部 fail-closed 报 `unsupported_pdf`。
- `Tm` 的线性部分必须是单位阵，因此 `TextTransform::{Rotated, Mirrored, Skewed}` 虽有分类器，**在生产 IL 路径不可达**（#17 评论已确认）。
- ScanDetect 对任何 `/Rotate != 0` 的内容页整份拒绝（`pass/mod.rs`，注释自陈是 #14 的临时措施）。
- 不存在任何「一页失败、其余继续」的机制：`Pass = fn(&mut Document, &PassContext) -> Result<()>`，任何页错误立即终止整条流水线。`docs/03-corpus-requirements.md` 里 STREAM-08、XOBJ-01/05/06、CMAP-08 等多条 M1 case 的预期都是「页级降级 + 其他页继续」，无处落地。

这三堵墙的根因是同一条：**Typeset 从 IL 重建全新的 content stream**，而 Write 要求改写页的 PDFium 字符快照与光栅像素与输入完全相等。任何不能被重建器忠实重发的状态（未知操作符、图形状态、非单位文本矩阵、Form 调用）都会破坏该等值验证，于是只能在入口处 fail-closed。放宽走查而不先解决重发模型，等于把保真验证变成摆设。

本 ADR 收口三个 Issue 共享的这一层：走查的有界失败模型、降级的粒度与报告形状、写回的重发模型，以及非直立判定所依赖的坐标口径。字体与 CMap 侧的可靠性判定单列 [ADR-0014](0014-font-decoding-reliability.md)——两者演进节奏不同（走查政策连着公开诊断合同，字体链会随 PDFium F1 能力绑定与 M3 case 复议）。

## 决策

### 1. 写回模型：区间替换（span splice）取代整流重建

Typeset 不再从 IL 重建整个 content stream。页面输出 = **原解码流字节，仅把被翻译单元的 text-show 操作数区间替换为新字节**，其余一切原样透传：未知操作符、图形状态、内联图像、Form 调用、非直立单元、降级段落。多 Contents 页逐流替换后逐流重建，**不合并为单流**（合并会放大 diff 且破坏 PARSE-04 的边界语义）。

推论（皆为该模型的必然后果，非独立决策）：

- `--backend none` 的恒等输出下所有替换区间字节不变 → 输出 content 与原流逐字节一致 → Write 的字符与像素等值验证天然成立。**这是三堵墙可以拆而不破坏保真合同的全部理由。**
- 「单元级隔离」= 该单元的区间不进替换集，零额外机制。
- 「段级保留」= 该段全部区间不进替换集。
- 「页级降级」= 该页不产生 `PageRewrite`，增量写回天然逐字节保留原 content stream（与 ADR-0012 §2 的扫描页透传同一机制）。

Typeset 的恒等守卫从「全段单一 upright 字体 run」收窄为「**替换字节必须等于原字节**」。真正的非恒等重排（改字节宽度、嵌入新字体、重新断行）仍推迟到 #22 及以后，本 ADR 不为其预留结构。

> 2026-08-27 修订：[ADR-0020](0020-inline-formula-flow-relocation.md) 进一步收窄了公式
> span 不进入替换集的边界。`display_formula`、段外公式和未翻译段公式仍逐字节透传；
> 已翻译多行段内的 `inline_formula` 可在独占 operand 中整体平移重发，也可在当前段完整
> 拥有且不含其它 passthrough 类字符的共享 operand 中逐 glyph 分割重发。两条路径都必须
> 保留公式 glyph 的源编码字节、字体身份与内部相对几何；独占路径另保留整个 operand 的
> lexical bytes。该修订不允许用译文字体或重编码字节替换公式，无法唯一溯源时仍段级
> fail closed。

### 2. 降级粒度：页级与段级，各有确定的保留语义

| 粒度 | 触发 | 保留语义 | 载体 |
|---|---|---|---|
| **文档级** | 加密、页树环、ObjStm 损坏、关键位置 dangling ref | 分类退出、不产出输出文件 | 现有 `MimusError`（不新增 reason，复用 `pdf_parse`/`unsupported_pdf`） |
| **页级** | tokenizer fatal（未闭合字符串/数组/hex、嵌套超限）、坏 BBox/Matrix、缺 XObject 资源、非法 `/Rotate`、坏 MediaBox | 该页不产生 `PageRewrite`，原 content stream 逐字节保留；其余页继续 | `ExtractedPage.degraded`（pub(crate)） |
| **段级** | 字体或 Unicode 不可信（见 ADR-0014）、退化文本矩阵导致不可定位、翻译响应连续两次违反占位符或内容守恒合同 | 该段全部区间不替换，`translated_text` 保持 `None` | `il::Paragraph.preserved`（可选字段） |

「**单元**」不是新的 IL 层级。IL 保持 Document → Page → Paragraph → Char 四级，单元 = **段内连续的、同一隔离原因的字符区间**，由 Typeset 从 `Char.text_transform` 与可见性标记动态聚合。理由：M1 的隔离语义只需要回答「哪些区间不替换」，派生分组零 schema 成本；#20 若需要更强结构再升 IL 版本。

`il::Paragraph` 新增 `preserved: Option<PreservedReason>`（serde `default`，IL `schema_version` 保持 1）。ADR-0007 本就不承诺跨版本 IR 兼容，ADR-0011 §6 允许 IL 独立演进；additive 可选字段不破坏既有快照消费者。段级保留时 `translated_text` 恒为 `None`，两者语义自洽。

`content_conservation` 是 additive typed reason：生产 executor 与 scorecard 共用
`mimus-quality-contract` 的保守数字/单位/方括号引用 lexer；首个违规响应只允许一次带
缺失 token 提示的纠错重试，第二次仍违规时整段保留。无效响应不得写入翻译 cache。

### 3. 有界失败模型

**tokenizer**：数字 parse 失败降级为 operator 词（粘连恢复的前提）；hex 串奇数 nibble 补 0（规范行为，不告警）；非 hex 字符、未闭合 string/array/hex → 该页 fatal → 页级降级。粘连 token（`12Tf`）仅对定位/状态类操作符白名单剥后缀并立即执行 + warning；双小数点（`10.5.3`）在第二个小数点处切成两个操作数 + warning。后两条沿用 M0 实验 2 已被 `mal-stream-06/07` 钉死的语义。

**操作数栈**：每个 operator 边界清空。多余操作数取尾部 arity + warning；操作数不足时该 operator **原子跳过**（CTM 不变）+ warning。这是 M0 实验 2 §6 STREAM-02 唯一给出完整理由的边界策略——错误不扩散到后续 operator。栈**没有数值上限**，有界性来自「每 operator 清空」这一结构性保证。

**逐场景语义**（写入对应 fixture 的 manifest，作为可断言合同）：

| 场景 | 政策 |
|---|---|
| `q`/`Q` 不平衡 | 多余 `Q` 保持 base CTM + warning；页尾未闭合 `q` 不影响已产出字符 + warning |
| 未知操作符 | `BX`/`EX` 区间内静默跳过；区间外 warning + 跳过，不终止页 |
| 孤立文本操作符（`BT` 之外的 `Tj`/`Td`/`Tf`） | **按隐式 `BT` 处理**（文本矩阵置单位阵）+ warning。取宽容侧是为了与独立渲染器的字符集合一致，避免走查与 PDFium 在合法输入上产生分歧 |
| 嵌套 `BT` | 按规范重置文本矩阵 + warning |
| `TJ` 非法元素 | **跳过该元素**（位移记 0），其余元素照常产出 + warning。丢字面最小，与「不扩散」一致 |
| 多个 `/Contents` | 流边界即 token 边界（等价插入空白）：数字跨界得两个独立操作数；字符串不得跨界，跨界未闭合 → 页级降级 |
| `Tr` 渲染模式 | 走查追踪 `Tr`，`Tr 3`/`Tr 7` 的字符照常产出并标记不可见；不可见字符不翻译（区间不替换），与非直立同一机制 |
| 内联图像 | 长度按 `/L` 声明 > `W·H·BPC` 可计算（溢出检查）> 受限 `EI` 扫描 + 续接合理性检查 三级优先，记录所选路径；扫描窗耗尽 → 页级降级 |

**Form XObject 与 Type3 CharProc**：主防线是**对象 ID active-path 去环**（自环与互环产出不同诊断，路径入 detail），深度上限是第二道边界。`/BBox` 必须是 4 元全数值且有限，两轴均不得退化；轴端点倒序时按 `min/max` 规范化并发出 `normalized_form_bbox` 恢复诊断，缺失、错长、非数值、非有限或退化仍页级 `bad_form_b_box`。`/BBox` 还必须**当作裁剪框**（PDF 32000-1:2008 §8.10.2 Table 95）：把它按 `Matrix ∘ CTM` 变换后取轴对齐外接框（旋转/斜切 form 因此得到超集，宁可少裁不误裁），沿 form 嵌套链求交；度量盒**整体**落在累积裁剪框之外的字符与 `Tr 3` 一样判为 `visible = false`，并发出 `clipped_form_content` 恢复诊断。部分相交的字形一律保留。被裁字符**仍留在走查结果里**——提取视图（poppler、PDFium text page）看得到它们，抽掉会凭空制造 engine-only 残差（ADR-0015）；它们只是不再进入障碍集、`fallback_line` 聚类与翻译集。`/Matrix` 必须是 6 元有限数值，否则该 `Do` 原子跳过并页级降级——**不得回落单位矩阵执行**（XOBJ-04 的验收明文禁止；M0 PoC 对坏 `/Matrix` 回落 IDENTITY 的行为不采纳）。Form 自带 `/Resources` 优先，缺失时整体继承调用方作用域（不 merge）。进入隔离作用域时按值保存 `{图形状态, q/Q 栈, 操作数, 兼容深度}`，退出时全量恢复，残留只产诊断不污染页面。Type3 CharProc 补齐与 Form 同源的递归保护——M0 实验 2 §范围限制已自陈这是留给生产实现的洞。

图形状态结构上，**文本矩阵不进 `q`/`Q` 的保存集**（PDF 规范语义）。M0 PoC 把两者混存是已识别的偏差，生产实现不沿用。

### 4. 边界数值

M0 实验 2 的参数不自动成为生产政策（其 §范围限制明确不外推）。逐条重新裁定：

| 边界 | 取值 | 依据与代价 |
|---|---|---|
| 单流解码上限 | **16 MiB** | 生产 walk、`scan.rs` 预扫、M0 PoC 三处已一致，无反证。代价：超大合法流被拒——按页级降级处理，可控 |
| 内联图像 `EI` 扫描窗 | **与流上限共用同一常量** | 语义同源（单流有界），消除两个同值独立常量 |
| token 嵌套深度 | **128** | `mal-parse-06-deep-nesting` 以诊断行为（第 129 层有界停止、不 panic）钉死；改值须改 fixture 且无收益。数组与字符串括号共享同一预算 |
| Form / Type3 深度 | **64**，且 `scan.rs` 预扫从 32 对齐到 64 | 主防线是去环，深度只防病态长链；两处用不同数字没有依据，对齐消除歧义。代价：动 #16 代码及其测试，方向保守 |
| 页树 / 资源继承深度 | **128** | 生产 walk 与 `scan.rs` 现状一致，此前只是裸字面量、无文档；本 ADR 只是让它成文 |

诊断 ID 命名空间：生产侧沿用 `event.rs` 的 `Diagnostic` 枚举，**不复用 `operator-walk:*`**——那是 M0 PoC 作为 corpus oracle 的合同，PoC 依 CONTEXT「PoC 冻结」条款保持冻结、不被生产代码引用。

### 5. 降级报告：v2 兼容扩展，不升版

依据 ADR-0011 §6（可增加消费者必须忽略的字段与非终结事件）：

- **`PageDegraded { page_index, reason }`**：逐页一条，吃 100 条上限，超限由既有 `dropped_diagnostics` 汇总兜底。
- **`DegradationSummary { degraded_page_indices, preserved_paragraphs, … }`**：单条汇总，仿 `ScanSummary` 的特权——无条件入库、不吃 100 条上限，终结事件之前发出。这满足 #18「终结报告列出受影响页」而**不触碰 `result` 的形状**（ADR-0011 §2 明文规定 result 不重复诊断内容、只保留 `warnings` 总数）。
- **`ContentRecovered { page_index, recovery, … }`**：§3 每一条「+ warning」的出线口。它与 `PageDegraded` 是同一枚硬币的两面——降级说「这一页没翻」，恢复说「这一页翻了，但走查偏离了输入的字面结构」。**每页每类恢复只报一条**，不是每次恢复一条：§3 要求恢复决定页级一致，逐次计数会随内容长度漂移，做不成稳定断言。`normalized_form_bbox` 与 `clipped_form_content` 额外携带按对象号排序且最多 16 项的 `form_object_ids`（`clipped_form_content` 报的是**拥有被裁墨迹的最内层 form**），以及未截断的 `form_object_count`；其他恢复省略这两个 additive v2 字段。
- **不新增 ExitCategory、不新增 reason**：页级与段级降级不是错误，退出码仍为 0。
- 人类模式：逐条 stderr `warning[page_degraded]: …`，外加一行汇总。
- `--debug` 的 `diagnostics.ndjson` 与 stdout 共用同一 serializer，自动获得新诊断，不引入第三个 schema。

### 6. 页面坐标口径与非直立判定

**页面空间**（MediaBox 定义、应用 `/Rotate` 之前）是几何事实层，与 `docs/03-corpus-requirements.md` §2.2/§2.3 的 manifest 口径一致。变换链：

```
字形空间 --Tfs·Tz·Ts（Type3 另乘 FontMatrix）--> 文本空间 --Tm--> --CTM（cm 栈 ∘ Form /Matrix）--> 页面空间
页面空间 --[CropBox 裁剪]--> --[/Rotate 旋转]--> 观看（视觉页框）空间
```

- **非直立判定的输入是 `R(/Rotate) ∘ CTM ∘ Tm` 的线性部分**（`Tz`/`Ts` 不参与朝向）。这兑现 CONTEXT #32 与 ADR-0007 §5 的「在视觉页框内度量」——此前分类器只看 `Tm`，页面 `/Rotate` 从未参与。判定阈值不变：直立窗 0°±0.1°、镜像（行列式为负）优先于旋转、斜切 > 20° 才隔离、180° 算非直立。
- **退化矩阵**（行列式≈0）不进 `TextTransform` 四值，标记为不可定位 → 段级保留 + 诊断。
- **页面几何事实层改由 lopdf 侧提供**（MediaBox/CropBox 解析与继承、`/Rotate` 合法性），engine 的 `page_geometry` 降级为交叉证据：Parse 时断言 engine 几何等于页框与 `/Rotate` 推导出的观看空间尺寸，不符则 `EngineMismatch`。这不改动 `PdfInspector` trait，ADR-0010 的 owned snapshot 合同不受影响。
- 非法 `/Rotate`（非 90 整数倍）→ 页级降级（GEOM-04 验收）。输出页面字典的 Box 键与 `/Rotate` 不被归一化——增量写回本就不触碰它们，本 ADR 要求补上逐字节断言（GEOM-02）。

### 7. 走查与 PDFium 的对齐分级

`validate_character_alignment` 此前要求走查与 PDFium 的字符数与 Unicode 完全一致，否则 `EngineMismatch` 硬失败。宽容走查下这条会误伤：恢复语义（如按隐式 `BT` 处理孤立文本）与 PDFium 的宽容行为可能合法地分歧。分三级：

| 页状态 | 对齐要求 |
|---|---|
| 无恢复警告、无降级 | 维持现状：字符数与 Unicode 硬对齐失败即报错；baseline 超容差记诊断 |
| 有恢复警告 | 对齐降为诊断（不再 fatal） |
| 页级降级 | 跳过对齐 |

依据 CONTEXT #35 / ADR-0006：PDFium 是交叉证据而非事实层，规范与 manifest 才是。

> 2026-08-25 更新：本节分级表已由 [ADR-0015](0015-classified-cross-engine-alignment.md) 取代——「无恢复警告时字符数与 Unicode 按 index 硬对齐」的前提被实验 5（`docs/07-engine-character-alignment-experiment.md`）在真实语料上证伪，字符对齐分歧不再触发文档级 `engine_mismatch`。§6 的页面几何断言不受影响。

## 后果

- 三堵 fail-closed 守卫（ScanDetect 的 `/Rotate`、walk 的 `Tm`、Typeset 的 upright 单 run）在 §1 的写回模型落地后依次拆除。#17 评论要求的「忠实重发或隔离路径成立前守卫必须保留」由实现顺序兑现：写回模型先于拆墙。
  - 中间态（2026-08-24 起）：`/Rotate` 守卫已从**整篇失败**改成**该页降级透传**。这不是拆墙——旋转页依旧不被改写，忠实性由「原样透传」而非「忠实重发」保证；变的只是它不再连累同一文档里的其它页。因此这一阶段 `/Rotate 90/180/270` 与非法取值走同一条降级路径，等 §6 的视觉页框判定落地后，合法取值才转为正常翻译，只剩非法取值降级。
- ADR-0012 §4 的「严格 walk 原样不动（白名单、fail-closed 语义均不变）」由本 ADR 取代——walk 从白名单 fail-closed 变为有界宽容。ADR-0012 的其余条款不变，**尤其是扫描判定的可见性口径**：`Tr 3`/`Tr 7` 不可见与非直立是两条正交维度，非直立字符继续计入可见文字统计。预扫此前不追踪文本矩阵，该口径是被动成立的；实现须加回归测试钉死，防止 §6 的矩阵感知顺手引入「非直立不算可见」的分支。
- ADR-0012 §6 语料表中标注「暂 `unsupported_pdf`（#18 解除）」的档位（SCAN-01(b)(e)、DOC-02(c)）预期按预告翻转，须按生成合同重新裁定。
- 一批 M1 case 从「无处落地」变为可验收：STREAM-08、XOBJ-01/05/06、CMAP-08 的页级/段级降级预期获得实现载体。
- 新增两类公开诊断即成合同，此后按 ADR-0011 §6 只增不删。
- 边界数值若被真实语料证伪，复议只需更新本 ADR 与对应 fixture 预期；`scan.rs` 深度从 32 改 64 会触及 #16 的既有测试，属预期内的一次性迁移。
