# ADR-0015 · 跨引擎字符对齐的分类交叉校验

- 状态：已接受（2026-08-25）
- 决策层级：可逆——等价类清单、配对窗口数值与保留阈值随语料与 fixture 复议；但「PDFium 是交叉证据而非事实层，其异议不得单票否决整份文档」直接派生自 CONTEXT #35 / ADR-0006，属既定哲学，不在复议范围

## 背景

`validate_character_alignment` 在无恢复警告、无页面降级时要求走查与 PDFium 的字符数与 Unicode 序列按 index 完全相等，否则整份文档以 `Input/engine_mismatch`（退出码 2）拒绝（ADR-0013 §7 第一行）。实验 5（[docs/07-engine-character-alignment-experiment.md](../07-engine-character-alignment-experiment.md)）在真实语料上证伪了该判据的前提：

- `Checking list.pdf`（未加密、结构有效、qpdf 通过）仅因 67 个独立空格 show（`[( )] TJ`）被整份拒绝；3,014 个可配对字符在 0.001 pt 内全部精确匹配且 Unicode 全等。
- 18 份真实论文 462 页、约 119 万 walk 字符：97.7% 可几何匹配；此前 8/10 抽样拒绝率的分歧主体是 PDFium 提取视图的固有行为，不是走查错误。
- 已确认的提取视图机制：丢弃独立空格 show；行末连字 `-` 返回 `U+0002` 且 `FPDFText_IsHyphen=true`（1,203 例）；非 BMP 字符的单字符 API 只给 UTF-16 高代理项（988 例）；连字 `ﬁ` 展开为首成分字符 + generated 余字符（28 例）；非 BMP 数学字符成批缺失（D 类主体 1,103 例）；数组顺序与绘制序不同。

结论：PDFium text page 是**面向搜索/提取的规范化视图**，与走查的 content stream 绘制事件枚举不是同一语义层，两者的数组相等没有 PDF 规范背书。要求相等等于把 PDFium 提取启发式提升为事实层，与 CONTEXT #35 及 ADR-0014 的 CMAP-04 先例（禁止 PDFium 当 Unicode oracle）直接冲突。且 `engine_characters` 的全部下游消费（layout 输入、`visual_bbox` 采纳）都已有走查侧回退——硬门没有在保护任何结构依赖。8/10 的真实论文拒绝率与验收场景（作者日常翻译 arXiv 论文）不相容，而被拒分歧的大头与任何安全性质无关。

## 决策

### 1. 撤销字符对齐的文档级硬失败

字符数、Unicode 或顺序分歧在任何页状态下都不再返回 `Input/engine_mismatch`。文档级失败只保留走查自身的 Fatal（能力边界 `unsupported_pdf` 等既有路径）。ADR-0013 §6 的页面几何断言（Parse 时 engine 几何 vs lopdf 页框推导）不属本 ADR 范围，其 `EngineMismatch` 保留。

### 2. 匹配算法：几何锚定多重集匹配 + 提取视图等价类

替代按 index 的 zip 比较：

1. **精确阶段**：baseline origin 在既有 `baseline_tolerance_pt` 内做多重集匹配，multiplicity 保留（double-draw 伪粗体合法存在）。
2. **等价类**（命中即不计为冲突，聚合诊断）：
   - 空白与不可见字符完全退出跨引擎比较——其字节由走查唯一拥有并进入 splice 区间，PDFium 对它们的意见与任何安全性质无关；
   - tight box 与可见页框无交集的 engine-only 字符（离页字符）；
   - engine 侧 C0 控制符（`U+0002` 连字标记等提取标记语义）——`is_hyphen` 字段保持诊断入口专用，生产 `page_characters()` 不新增 API 依赖；
   - engine 值恰为走查非 BMP 字符的 UTF-16 高代理项；
   - 连字折叠：走查单字符连字（`U+FB00`–`U+FB06`）对 engine 首成分字符；
   - `/ActualText` 与 PDFium generated character 造成的提取展开——额外的提取文本不是额外的墨迹；
   - 顺序：匹配按几何不按序——顺序的事实层是 content stream 绘制序，顺序差异不再是分歧。

   增量写回后的混合输出往返校验沿用同一事实层，并补充两条有界的提取视图等价：
   - content stream 在同一 baseline 重复绘制同一 Unicode 时，PDFium 可能把多个 show 折叠成一个提取字符。只有先以 Unicode 与既有 baseline 容差证明过的 typeset 墨迹，才可满足后续同 Unicode、同 baseline 的期望；不同 Unicode、不同 baseline 或唯一缺失字符仍为 `output_mismatch`；
   - 输入 PDFium 视图中的行末连字符标记 `U+0002`，在未修改字节经过增量写回后可能暴露为字面 `-`。只接受同 baseline 的这个单向等价，且候选同时存在时优先精确 Unicode 匹配。

   这两条不以 PDFium 证明写回正确：content show 与 ToUnicode 仍由 qpdf/走查确认，Poppler 与 MuPDF 负责独立验证可提取文本；PDFium 只解释为何其输出快照的 multiplicity 或标记值不同。

   等价类清单钉为常量表，扩充须有对应 fixture。
3. **窗口阶段**：对残差在逐 source-run 的动态误差包络内配对，不设全页通用的 point 半径。候选位置须投影到该 run 的 writing direction：垂直误差只接受 CTM/text matrix 的数值舍入上界，平行误差再累加逐字符宽度来源误差与浮点 advance 累积上界；包络接近相邻 baseline、跨行或出现多个候选即拒绝配对。具体推导路径见 [调研报告 §6.1](../08-alignment-provenance-feasibility.md#61-应推导的是动态误差包络)，公式参数与数值仍须由 fixture 先验钉死，实验 5 的 0.5 pt 只是语料探索上界，不得反推生产值。
4. **来源信息分为两层，不存在 PDFium 直达源字节的桥**：
   - **PDFium 内部归组**：当前 `pdfium-render 0.9.1` 已有 character → text object → font、页面对象遍历与 Form 递归的 safe API；marked-content API 已进入 raw bindings，尚需 wrapper/owned snapshot 封装，不需新增 PDFium C symbol。它们足以按 engine text object 归组字符。
   - **跨引擎源相关**：PDFium 7763 公共 API 不提供源 charcode/CID/GID、text object 内源字符序号、间接对象号/资源名/content stream 身份或 show 操数字节区间，补 wrapper 也不能生成这些上游不存在的数据。源字节区间仍由 walk 独占；engine object 与 walk source run 只能按对象组、字体、序列、multiplicity、transform、几何和邻近锚点做保守相关，任何歧义均留残差。

   `docs/05-pdfium-backend-qualification.md` 的 T1/F1/O1「crate 暴露」结论只审计 firecrawl 候选后端，不描述当前 `pdfium-render` 的能力。后端数据仍须按 ADR-0010 形成 mimus owned snapshot 后跨 `PdfInspector` 边界。

### 3. 分歧判定矩阵

| 类 | 判据 | 响应 |
|---|---|---|
| 提取视图等价 | §2.2 等价类命中 | 聚合诊断，继续 |
| 解释边 | 非直立、`unicode=None` 或 advance 不可靠的 walk 字符在容差内有唯一 engine baseline 对应 | 吸收对应 engine 字符并单独计数；不使 walk 字符可翻译、不改变段级保留、不建立 `tight_box` 采纳链接 |
| C-强冲突 | 已配对位置 Unicode 不等价，走查链为 `ToUnicode`/`EmbeddedFontCmap` | 走查胜出，诊断 + 继续；计数进结束汇总供人工审 |
| C-弱冲突 | 已配对位置 Unicode 不等价，走查链为 `SimpleEncoding` | **段级保留** |
| C-未解析 | 走查 `unicode=None` | ADR-0014 已段级保留；交叉侧仅诊断 |
| D | walk-only 墨迹字符（窗口内无 engine 候选） | 诊断，继续（语料实测主体为 PDFium 非 BMP 提取缺失） |
| E | 提取视图等价与解释边先行、窗口配对和保守源相关均无法解释，且独立证据证明存在额外墨迹的 engine-only 字符 | 与将被替换单元几何相交 → **该段保留**；否则诊断 |
| F | 其余残差 | 非直立 → 本就 passthrough，诊断；不可定位/无 Unicode → 既有保留路径已覆盖；其余与将被替换单元相交 → **段级保留**，否则诊断 |

**E/F 保留规则的生效前提**：解释边先于 E 判定生效，§2.3 动态误差包络由 fixture 钉死，§2.4 的 owned 对象归组与保守相关落地，并有一份独立 renderer 证明真实额外墨迹、walk report 证明无对应 source event 的 E fixture。在此之前，stage-1-only 的假 E（同码字符仅因字体宽度漂移未精确配对，语料上约 8.6 万字符走的是窗口配对）会造成大面积误保留，故过渡期 E/F 只聚合诊断。过渡期的安全性由既有合同承担：splice 只替换走查枚举的区间、未替换字节逐字节透传；ADR-0014 已保留 `unicode=None` 段。实验 5 原记为 E 的 302 个字符现已全部在 0.001 pt 内唯一对应到非直立、`unicode=None` 的 walk 字符，属于解释边而非已观测 walk 漏字。该前提须在翻译后端接通真实语料（M2）前完成。

页级降级不再由跨引擎分歧触发，只来自走查自身失败（实验中 `topcite_2020_Zhang` page 11 的无 BBox Form XObject 属此既有路径）。`--strict` 沿 CONTEXT #16 既有语义升级降级。

### 4. ADR-0014 修订：ToUnicode 映射到非字符视为未映射

`ToUnicode` 产出 Unicode 非字符（U+FFFE/U+FFFF 及全部 66 个 noncharacter）→ 按「该字符未被映射」处理 → `unicode=None` → 既有段级保留。语料中仅有的 4 个强链冲突（`ToUnicode`→`U+FFFF` vs PDFium `U+0BDC`/`U+0BDD`）由此消解为解码失败——注意消解依据是「非字符不是合法映射目标」，不是采信 PDFium 的值；PDFium 仍不是 Unicode 事实层。

### 5. `tight_box` 采纳改为按字符

`engine_boxes_are_aligned` 的全页 zip 前提废除：`visual_bbox` 逐字符采纳几何匹配成功者的 engine tight_box，失配字符回退 `metric_box`。顺带消除「两侧顺序不同但 Unicode 序列逐位偶等时错位采纳 tight_box」的既有缺陷。layout 输入的现状（engine 快照，恢复页回退走查合成快照）不变。

### 6. 安全不变量

1. 区间替换合同不变：替换区间的字节偏移只来自走查 tokenizer；未替换字节逐字节透传；`--backend none` 全文档字节恒等。
2. ADR-0014 判定链不放宽（§4 反而收紧）。
3. **PDFium unicode 永不注入 IL**——`Char.unicode` 只来自走查解码链，交叉校验无权改写。
4. 恢复警告页与降级页的既有处理路径保留。
5. 等价类清单、窗口数值、保留判据均为钉死常量，修改须过 fixture 门禁。

## 后果

- ADR-0013 §7 的对齐分级表由本 ADR 取代（该 ADR 已加指针注记）；其 §6 几何断言与 §2/§5 降级形状不变。
- `validate_character_alignment` 重写为分类器：产出 per-char 匹配链接（供 §5 采纳 tight_box）、聚合诊断与保留标记。`InputReason::EngineMismatch` 从字符对齐路径退场，错误码保留给几何断言。新增分类诊断类型按 ADR-0011 只增不删；既有 `EngineBaselineMismatch` 语义不变，`EngineCharacterMismatch` 类型保留作分类器不可用时的兜底。
- corpus 需覆盖：独立空格 show（`(A) Tj [( )] TJ` 最小复现已有）、连字标记、非 BMP 代理项、连字展开、ToUnicode→非字符、`/ActualText` 提取展开、double-draw 多重集、输出侧同 Unicode 同 baseline 的 coincident show 折叠；启用 E 保留前另须新增真正 engine-only 墨迹 fixture。
- `Checking list.pdf` 与 8/10 被拒论文回到可处理路径；按语料量级，新增保留面极小（弱冲突 413 字符、非字符 4 字符）。
- 待复议 / 证据缺口：动态误差包络参数（fixture 钉死）；backend-neutral owned 对象/字体/mark snapshot 与 walk source-run 保守相关；真正 engine-only 墨迹 fixture；端到端视觉质量（实验 5 未执行翻译与 rewrite，由质量四件套与发布前真实语料 checklist 承接）；若强冲突在非字符规则之外再现，升级政策另行复议。
