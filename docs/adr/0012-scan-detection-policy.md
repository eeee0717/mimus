# ADR-0012 · 扫描件判定政策

- 状态：已接受（2026-08-23）
- 决策层级：混合——判定政策（阈值与口径）可逆，复议时更新本 ADR 即可；§5 的协议扩展一经发布即属公开合同，受 ADR-0011 §6 演进规则约束，难逆

## 背景

#16 要求分类拒绝扫描件：纯图、不可见 OCR 层和扫描占比达阈值的文档被拒绝，少量隐藏水印、可见文字图片页和标题页不被误判，且统计机器可见。但判定政策此前从未收口：

- 现实现的 ScanDetect 只统计全文档 walked character 总数、为 0 才拒绝（`pass/mod.rs`），无逐页判据、无阈值、无统计字段；空白文档会被误报 `scanned_pdf`。
- 现有操作符走查是 5 操作符白名单（BT/ET/Tf/Tm/Tj），遇到图像（`Do`、`BI`）或文字渲染模式（`Tr`）在 Parse 阶段即 fail-closed 报 `unsupported_pdf`——**#16 针对的纯图页与隐形文字页根本轮不到 ScanDetect 分类**，报错 reason 是错的。
- BabelDOC 的方案（SSIM 单页判据 + 80% 提前退出聚合，`docs/03-corpus-requirements.md` SCAN-01/02）有单页 100% 漏判与页面顺序依赖两处硬伤，且每页渲染两次对 mimus 太重；其数值只是参考起点，不是 mimus 决策。

2026-08-23 经两轮设计问答（14 个决策点）收口如下。加密 PDF 侧无新政策决策；两类 fixture 对 lopdf 检测行为的实证补充回 ADR-0009。本 ADR 与 ADR-0011 的关系是：ADR-0011 显式排除了 #16 语义（其 §背景），本 ADR 按其 §6 演进规则做 v2 兼容扩展。

## 决策

### 1. 单页判据：纯结构信号，不用光栅

每页分为三元之一：

| 分类 | 判据 | 处理 |
|---|---|---|
| **空白页** | 0 文字对象 且 无图像 | 原样透传、不参与翻译、不报错（PARSE-09 口径） |
| **扫描页** | 有图像 且 0 可见文字对象 | 计入扫描统计；文档继续时整页透传 |
| **内容页** | 其余 | 进入翻译路径 |

口径：

- **可见文字**：`Tr 3`（以及 `Tr 7` 这类不着墨的剪裁模式）渲染的字符不算可见；其余渲染模式算可见。非直立字符**计入**（既定，CONTEXT 单元级隔离条目）。
- **图像**：Image XObject（`/Subtype /Image` 经 `Do`）或 inline image（`BI/ID/EI`），存在即计，**不测覆盖率**——覆盖率需要追踪 CTM，收益不抵复杂度。
- **不用光栅比较**：SSIM 方案每页渲染两次、72 DPI 系统性偏向误判（SCAN-01 调研），结构信号已足以满足全部语料预期。

**有界行为**：「满页图 + 可见文字层」（他方 OCR 的可见文字层）与普通图文页在结构信号上不可区分，**统一按原生分类**——可见文字本可翻译，误判方向安全。SCAN-01(b) 的「拒绝」预期由本 ADR 修订（见 §6）。

### 2. 文档级聚合：≥ 80% × 内容页

- **内容页 = 总页 − 空白页**；扫描比例按内容页算。分母剔除空白页是为了不漏判「1 页扫描 + 9 页空白背面」这类双面扫描件。
- **扫描页 ≥ 80% × 内容页（含等于）→ 整份拒绝**：`Input/scanned_pdf`、退出码 2，拒绝发生在 Translator 与 Write 之前，不产出输出文件。
- 80% 取自 BabelDOC 参考值，属**显式采纳**而非照抄；「含等于」的边界语义由 4/5 边界 fixture 钉死。
- 推论（皆为判据的必然后果，非独立决策）：
  - 单页扫描件 1/1 = 100% 必拒——BabelDOC 的单页漏判不存在。
  - 全量计数、无提前退出——结论与页面顺序无关（SCAN-02 硬约束）。
  - 全空白文档内容页 = 0，比例无定义 → **不拒绝、透传成功**（修复现状把空文档误报 `scanned_pdf` 的行为）。
- **低于阈值**：文档继续翻译；扫描页与空白页**豁免严格 walk、整页透传不翻译**——增量写回模型下未改写的页天然原样保留。不豁免则 PARSE-09 空白页（无 `/Contents`）会被严格 walk 误杀，混合文档也无法继续。

### 3. 范围边界：#16 只保证分类正确

- 图文页（图 + 可见文字）与含少量 `Tr 3` 水印的正文页：**分类为原生（不误判）**，但严格 walk 尚不支持 `Do`/`Tr` 的重放，这些页仍以 `unsupported_pdf` 被拒。端到端翻译分别归 #18（图像 / 完整 operator walk）与后续 Tr 重放支持；#16 不提前实现。
- 无图纯 `Tr 3` 页（如现有 invisible-text fixture）：有文字对象故非空白，无图像故非扫描 → 内容页 → 严格 walk 照旧报 `unsupported_pdf`，最终行为与现状一致。
- 语料预期修订见 §6。

### 4. 数据模型：宽容预扫，严格 walk 不动

- Parse 新增**宽容预扫**（tolerant prepass）：逐页统计可见/隐形文字数与有无图像。它**永不报错**——证据完整时按 §1 的判据分类；任何片段无法完整解析、资源递归超限或证据存在异常时，整页保守归为**内容页**，随后由严格 walk 关闭失败路径。预扫已经统计到的事实仍可保留作内部证据，但不得据此把证据不完整的页面判为空白页或扫描页，以免错误豁免严格检查。
- **严格 walk 原样不动**（白名单、fail-closed 语义均不变），执行时机挪到 ScanDetect 之后、且只跑「将被翻译的页」（§2 豁免规则）。分类优先于保真检查——这是「纯图页必须报 `scanned_pdf` 而非 `unsupported_pdf`」的验收标准强制的。实现建议并入 Layout 开头、不新增公开 stage 值；若实现时改为独立 stage，新 stage 值属 ADR-0011 §6 的 v2 兼容扩展。
- 统计放内部 `ExtractedPage`（pub(crate)）；**公开 IL 保持 schema v1**，不加页级字段。

### 5. 机器协议：v2 兼容扩展，不升版

依据 ADR-0011 §6（可增加消费者必须忽略的字段与非终结事件）：

- **error 事件**：仅 `reason = scanned_pdf` 时新增 `scanned_pages`、`total_pages` 两个字段；其余错误不出现这两个字段。category/退出码不迁移（`Input`/2 不变），故不升 CLI schema。
- **汇总 diagnostic**（单条 typed diagnostic）：携带 `scanned_page_indices` 数组与 scanned/blank/content/total 计数；`scanned_pages > 0` 时**拒绝与继续两条路径都发**（现有 finish 顺序保证 diagnostic 先于终结事件）。单条汇总不吃 100 条上限，SCAN-02 要求的逐页断言直接断言数组。id 命名实现时定稿。完整细分（blank/content）只在 diagnostic，不塞 error。
- **人类模式**：拒绝时 error 行 message 带「N of M content pages are scanned」+ hint 提示 V1 不支持扫描件（OCR 属 V2）；继续路径 stderr 一行 `warning[...]` 带 N/M。
- **inspect 与 translate 分类一致**：两者都跑 ScanDetect，天然成立。

### 6. 语料条款

**加密 2 份**（DOC-03，守护 ADR-0009 拒绝路径）：均由 qpdf（工具链已钉 12.4.0）**一次性生成、二进制入库**；manifest 新增「工具生成入库」的 Method 类目，记录生成命令与 `pdf_sha256`。AES 随机 IV 不可复现（§2.6 已定入库不参与往返），RC4 档同规则处理，一条规则管两份。

**扫描 11 份**（去重合并 SCAN-01/02/03 与 DOC-02 的构造，ID 实现时定稿）：

| # | 构造 | 预期 |
|---|---|---|
| 1 | 单页满页图、无 `BT` | 拒绝 `scanned_pdf`，统计 1/1 |
| 2 | 单页满页图 + `Tr 3` 文字层 | 拒绝 `scanned_pdf` |
| 3 | 单页满页图 + 可见文字（合并 SCAN-01(b)(e)/DOC-02(c) 三档） | 非 scanned；暂 `unsupported_pdf`（#18 解除） |
| 4 | 单页纯文字大标题扉页 | 正常翻译 |
| 5 | 正常正文 + 少量 `Tr 3` 水印 | 非 scanned；暂 `unsupported_pdf`（Tr 重放支持后解除） |
| 6 | 3 页、第 2 页无 `/Contents`（PARSE-09 本体） | 正常翻译，空白页透传 |
| 7 | 3 页、第 2 页只有图（PARSE-09 变体） | 1/3 内容页 = 33% → 继续，该页透传 + diagnostic |
| 8/9 | 1 文字页 + 3 纯图页，两种页序 | 75% < 80% → 都继续且结论相同（顺序无关守卫） |
| 10 | 9 纯图 + 1 文字页 | 90% ≥ 80% → 拒绝，error 带 9/10 |
| 11 | 4 纯图 + 1 文字页 | 恰 80% → 拒绝（钉「含等于」语义） |
| 12 | 1 纯图 + 9 空白页 | 内容页 = 1，100% → 拒绝（钉分母规则） |

- 砍 SCAN-02(d) 的 20 页档：「低比例继续」已被 #7 与 #8/9 覆盖，20 页只是尺寸不同。
- expected manifest **手写先于生成器**（既定规则）。
- 语料预期修订（本 ADR 为准，`docs/03-corpus-requirements.md` 相应 case 行加注记不改写）：SCAN-01(b)(e)、DOC-02(c) 与 SCAN-03(c) 的「拒绝/正常翻译」预期改为「非 scanned_pdf、暂 `unsupported_pdf`、后续 issue 解除」。

## 后果

- 一批此前无法正确分类的输入获得正确 reason：纯图页从 `unsupported_pdf` 变为 `scanned_pdf`；空白文档从误报 `scanned_pdf` 变为透传成功。
- 既有测试须迁移：CLI 测试中「空 content stream → `scanned_pdf`」的预期失效（全空白 = 成功，需换失败 fixture）；「scanned 场景 diagnostics 为空」的断言失效（汇总 diagnostic 会出现）。
- 实现缺口备忘（归实现 PR，本 ADR 只记录）：多页 exact writer、Image XObject writer、纯图 fixture 的 manifest 门禁（`validate_exact_contract` 的文字类检查不适用，需新 Check 或放宽）、DOC-03 类测试须用 `--backend none` 或 `inspect`（默认 openai 后端在 Parse 前即以 Translation/4 短路）。
- 加密侧在 #16 内只补 fixture、`was_encrypted() || is_encrypted()` 双腿检测，以及分别漏掉任一腿时必须失败的守卫测试；拒绝政策仍沿用 ADR-0009。
- 阈值 80% 与可见性口径若被真实语料证伪，复议只需更新本 ADR 与 fixture 预期；协议字段一经发布则按 ADR-0011 §6 只增不删。
