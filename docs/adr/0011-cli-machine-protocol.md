# ADR-0011 · CLI 机器协议 v2：inspect、debug、诊断与输出错误

- 状态：已接受（2026-08-23）
- 决策层级：难逆（脚本、Agent Skill、退出码和调试产物都依赖该公开合同）

## 背景

ADR-0008 已决定所有子命令通过版本化 NDJSON 向脚本和 Agent Skill 暴露机器协议，但 #14 合并后的最小实现尚有四个缺口：没有 `inspect` / `--debug`，诊断只公开总数，NDJSON 写失败会被吞掉，磁盘和内部不变量错误会被归为 Input/2。

本 ADR 只收口 #15 所需的公开观察接口。它不扩展 PDF 能力，不提前设计 #16 及后续 pass 的语义，也不建立第二套执行路径。

## 决策

### 1. inspect 与人类输出

- `mimus inspect INPUT [--debug NEW_DIR]` 只读执行 Parse → ScanDetect → Layout → ParagraphFind；不调用 Translator、Typeset 或 Write。中途失败只报告 error，partial IL 仅在显式 debug 目录中保留。
- 人类模式下，进度、诊断和错误写 stderr；`translate` 成功时仍只把输出路径写 stdout；`inspect` 成功时把 canonical pretty IL JSON 写 stdout。
- `translate` 与 `inspect` 都接受命令级 `--debug NEW_DIR`。目录必须不存在，失败时保留已经成功写出的调试前缀。

### 2. NDJSON v2

CLI 机器协议版本升为 2；IL 的 `schema_version` 独立保持 1。每行沿用扁平 envelope，顶层必须有 `schema_version` 和 `event`：

```json
{"schema_version":2,"event":"diagnostic","id":"engine_character_alignment","page_index":0,"walked_character_count":2,"engine_character_count":2,"extraction_equivalent_count":0,"explained_count":0,"strong_unicode_conflict_count":0,"weak_unicode_conflict_count":0,"unresolved_unicode_count":0,"walk_only_count":0,"engine_only_count":0,"residual_count":0,"baseline_residual_count":1,"baseline_residual_max_delta_x_pt":0.01,"baseline_residual_max_delta_y_pt":0.02}
{"schema_version":2,"event":"result","output":"paper.zh.pdf","pages":1,"warnings":1}
{"schema_version":2,"event":"result","il":{"schema_version":1,"pages":[]},"pages":1,"warnings":1}
{"schema_version":2,"event":"error","category":"input","reason":"encrypted_pdf","message":"...","hint":"..."}
```

- 保留 `stage_started`、`stage_finished` 和 `page_progress`；增加 typed `diagnostic`。结构化进度和诊断属于 stdout NDJSON；只有它们的人类可读 renderer 走 stderr。
- 机器字段中的页和字符位置统一为 0-based `*_index`；人类 renderer 可显示 1-based 页码。
- 普通诊断最多保存并发出 500 条，且每个 `DiagnosticId` 最多 25 条，避免单类洪泛饿死其余诊断。`scan_summary`、`page_degraded`、`degradation_summary` 无条件出线且不占普通预算。超限时在终结事件前额外发一个 `id = dropped_diagnostics` diagnostic 汇总；`count` 保留总丢弃数，additive `counts_by_id` 给出稳定按 ID 排序的分项计数，它本身不占预算。
- `result` 不重复诊断内容，只保留 `warnings` 总数；其语义仍为已保存的普通/豁免诊断数加被丢弃数，debug-only 明细不计入。`inspect` result 额外携带完整 `il`。
- JSON 模式正常运行时 stderr 为空，stdout 只包含 NDJSON。实际执行请求及其用法失败都进入协议；help/version 是 clap 元操作，不属于 v2 wire。
- stdout 保持可写时，最后一行必须是恰一个 `result` 或 `error`，终结后不得再发事件。result 对应退出 0，error 的退出码必须与 `category` 一致。

### 3. 错误类别与 reason

公开类别和退出码为：Success=0、Usage=1、Input=2、Asset=3、Translation=4、Io=5、Internal=6。error 事件必须同时携带 `category` 和 `reason`。

- Io：`input_read`、`output_write`、`atomic_publish`、`debug_write`、`stdout_write`。
- Internal：`output_build`、`output_mismatch`、`event_serialization`、`invariant_violation`。
- PDF 解析、能力边界和引擎分歧继续归 Input；PDFium 可用性归 Asset；翻译后端错误归 Translation。其他既有 reason 名称不变。

### 4. 输出写失败

- 每条 NDJSON 必须先完整序列化为内存中的“JSON + 换行”，再写 stdout，禁止序列化器直接产生可与后续事件粘连的半行。
- EPIPE 表示消费者主动关闭：关闭 stdout 输出、继续业务，进程最终退出码仍反映业务结果。该流不再适用终结事件保证。
- 其他 stdout 写错或部分写将该输出永久标为不可写，禁止继续写入；命令以 Io/5 结束，并在 stderr 尽力给出最后说明。允许无法撤回的一个截断末行，不允许其后再粘事件。
- 事件序列化在接触 stdout 前失败时归 Internal/6，并尝试输出一个最小 error terminal；若终结本身也不可写，则只在 stderr 尽力报告。

### 5. IL 与 debug

- inspect、debug 和 insta 快照必须从同一个 IL snapshot value 和 canonical writer 生成；canonical pretty JSON 固定带末尾换行。
- debug 对每个成功 pass 原子写 `NN-stage.il.json`。`inspect` 最多写 00–03，`translate` 最多写 00–09；失败 pass 不写 partial snapshot，已有文件不清理。
- debug 另写 `diagnostics.ndjson`，其中只含同一 v2 serializer 生成的 diagnostic 和 dropped 汇总，不引入第三个 schema。跨引擎 baseline 残差在普通输出中并入每页一条 `engine_character_alignment`；逐字符 `engine_baseline_mismatch` 明细只写入该 debug 文件。
- 当前 Parse、ScanDetect 和 Layout 尚未把中间状态装入 IL，因此早期快照可以为空或相同；#15 不为制造差异而重塑 IL。

### 6. 演进规则与范围

- v2 可增加消费者必须忽略的字段、非终结事件及新的 stage/reason 值。
- 删除、改名、改变类型或语义，改变终结数量/通道，或迁移 reason 的类别/退出码时必须升级 CLI schema。
- IL schema 独立演进；IL 升版不要求 CLI schema 同步升级。
- #15 不实现 `assets pull`、真实 layout 模型、扫描策略、页面旋转、完整 operator walk、字体/CMap、真实翻译排版、Agent Skill 或配置层。

已按本节做过的兼容扩展（登记，便于审计「只增不删」）：

- ADR-0012 §5（#16）：`scanned_pdf` error 的 `scanned_pages`/`total_pages` 字段与扫描汇总 diagnostic。
- ADR-0013 §5（#17/#18/#19）：`page_degraded` 与降级汇总两类 diagnostic。二者与扫描汇总均无条件入库，不吃普通诊断预算。`result` 形状未变——§2 的「result 不重复诊断内容、只保留 warnings 总数」仍然成立，受影响页号只经 diagnostic 出线。
- ADR-0013 §3（#18）：`content_recovered` diagnostic。与 `page_degraded` 相反的一侧——这一页照常翻译，但走查为此偏离了输入的字面结构，所以偏离本身必须出线。每页每类恢复只报一条：恢复决定是页级一致的，报告次数不应随内容长度漂移。
- ADR-0015（#67/#68/#70 前置决策）：`engine_character_alignment` 逐页聚合分类交叉校验的等价差异、解释边、Unicode 冲突及 D/E/F 残差；`explained_count` 是 v2 的 additive 字段，不升 schema；既有 `engine_character_mismatch` 保留为分类器不可用时的兜底。
- 2026-08-26 真实论文恢复：诊断预算改为总量 500 + 每 ID 25，`dropped_diagnostics.counts_by_id` 为 additive 字段；baseline 残差计数与最大偏移并入逐页 `engine_character_alignment`，逐字符明细降为 debug-only。公开协议仍为 v2。
- 2026-08-28 #98：`content_recovered` 新增可选 `form_object_ids` 与 `form_object_count`，用于 `normalized_form_bbox` 的有界对象定位；公开协议仍为 v2。
- ADR-0018：`configuration_resolved` 增加 Regular/Bold 输出字体的 source 与 SHA-256；
  `unsupported_output_glyph` 增加缺字符样本与字体身份。两者均为 additive v2 扩展。

## 后果

- 脚本能在不解析散文的前提下观察 pass、诊断和最终结果，`inspect` 与 `translate` 仍复用同一条流水线。
- stdout 本身失效时不可能保证可解析终结行；协议明确区分正常可写流和已损坏流，避免作出无法兑现的保证。
- 两个新增退出码会改变此前错分为 Input/2 的路径；CLI schema v2 使消费者能够显式识别这一变化。
- debug 产物只保留 IL 和诊断，不演变成第二套完整事件日志或通用追踪框架。
