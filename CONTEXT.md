# CONTEXT

> 项目共享术语与决策索引。术语随设计会话逐轮补充；难以逆转的技术选择在 `docs/adr/` 以 ADR 形式记录。
> 事实基础：`docs/01-research.md`（2026-08-21 调研报告）+ 2026-08-21 模型/引擎事实查证（要点已并入 ADR-0002/0006）+ `docs/04-m0-experiment-1.md`、`docs/04-m0-experiment-2.md`、`docs/04-m0-experiment-3.md`（M0 三项实验结论）+ `docs/05-pdfium-backend-qualification.md`（PDFium Rust wrapper 资格验证）。

## 已定决策

| # | 决策 | ADR |
|---|---|---|
| 1 | 实现语言 Rust，唯一执行接口为 CLI（暂不做 GUI/MCP）；Agent Skill 通过 `npx skills add eeee0717/mimus` 安装并调用 CLI | [ADR-0001](docs/adr/0001-rust-cli.md)、[ADR-0008](docs/adr/0008-agent-skill.md) |
| 2 | 版面检测模型：PP-DocLayoutV3 官方 ONNX（131 MB，25 类，含阅读顺序） | [ADR-0002](docs/adr/0002-pp-doclayoutv3.md) |
| 3 | V1 仅原生 PDF 路径（扫描件报错拒绝）；OCR 后置 V2，IR/架构预留 | [ADR-0003](docs/adr/0003-v1-native-pdf-path.md) |
| 4 | 许可：MIT 单许可 | [ADR-0004](docs/adr/0004-mit-license.md) |
| 5 | 分发：单 archive 解压即用；模型/字体资产运行时下载 + 自备逃生门 + 镜像可配 | [ADR-0005](docs/adr/0005-distribution.md) |
| 6 | 引擎组合：lopdf（对象树/增量写回/原始字节）+ pdfium-render（当前度量/光栅化后端，trait 边界后）+ 自写操作符走查。firecrawl-pdfium 仅在补齐 T1/F1/O1 并重新资格验证后才可替换 | [ADR-0006](docs/adr/0006-engine-combination.md) |
| 7 | IR：单字符粒度 + 双盒 + Rust enum + serde JSON 快照（schema_version） | [ADR-0007](docs/adr/0007-ir-design.md) |
| 8 | 目标用户：开源发布，面向"读外文 PDF 的中文研究者"；验收场景=作者本人日常翻译 arXiv 论文 | — |
| 9 | 语种：输入语言解耦（取决于文本提取），验收只对英→中；排版仅调优简体中文输出 | — |
| 10 | 输出模式：默认仅译文；`--bilingual` 原/译交替页（修书签/内链页目标映射）；并排不做 | — |
| 11 | 保真范围：书签不翻译、链接热区不调整、表单/OCG 原样透传；断言书签数/注释数不变 | — |
| 12 | 流水线：固定顺序 pass 链（`fn(&mut Document, &PassContext)`——`Document` 只装数据：原始字节/lopdf 文档/IL/诊断；`PassContext` 装引擎实例/配置/事件钩子；无 pass 框架）；pass 内按页 rayon 并行；`--debug` 逐 pass 落盘 IL | — |
| 13 | 阶段草案：Parse → ScanDetect(拒绝) → Layout → ParagraphFind → StylesAndFormulas → ExtractTerms → Translate → Typeset → FontEmbed → Write | — |
| 14 | 阅读顺序：以 V3 模型输出为准 + 几何排序兜底；实测 `[M,7]` 第 7 列是 RT-DETR query id，同时可作页内阅读顺序键；同 query 多类别先取最高分，排序键为 `(page_index, col7)` | [ADR-0002](docs/adr/0002-pp-doclayoutv3.md) |
| 15 | 翻译政策表（见下）；表体默认不翻留 `--translate-table` 实验开关 | — |
| 16 | 错误恢复：三层降级（段→页→文档，宁保原文不出坏译文）+ 结束汇总 + `--strict`；畸形 PDF fail-fast、修复函数语料驱动；退出码 0/1/2/3/4/5/6 分类（Usage/Input/Asset/Translation/Io/Internal） | [ADR-0011](docs/adr/0011-cli-machine-protocol.md) |
| 17 | V1 单进程；PDFium 崩溃（abort）接受，子进程隔离推 V2 | — |
| 18 | 字体：Noto Sans SC（Regular+Bold）单族走资产机制；subsetter 子集化；`--font` 覆盖；italic 映射正常字重 | — |
| 19 | 术语：**保留自动术语提取**（独立 pass，LLM）+ `--glossary` 用户术语表 | — |
| 20 | 翻译层：`Translator` trait 可扩展；V1 实现 openai 兼容 + `none` 直通；占位符协议失败降级保原文 | — |
| 21 | 翻译缓存：V1 即做，redb，键含(原文,模型,目标语,prompt 版本,术语指纹)；`--no-cache` | — |
| 22 | 推理纯 CPU（ort CPU EP）；GPU/NPU EP 不进 V1 | — |
| 23 | 质量回归四件套：全语料 IL 快照 + 占位符守恒 + 零 panic + 几何断言；**CI 绿才能合并**；渲染像素 diff 待渲染路径稳定后加 | — |
| 24 | 语料：**Corpus v1 已从零建立，M1 以 133 份 fixture / 72 个去重 case 收口**；每份均由独立门禁验收，普通 CI 全量执行 production path 与 `corpus verify`。旧 23 份合成语料永久作废、不得参考；真实语料不进 repo、发布前人工 checklist | — |
| 25 | CLI：子命令结构（`translate` / `assets pull` / `inspect`）；配置三层 flags > env > `~/.config/mimus/config.toml`；API key 不走明文 flag；两级人类可读进度；V1 `--json` 输出版本化 NDJSON，当前公开协议为 v2；细粒度 flags 随功能里程碑落地 | [ADR-0008](docs/adr/0008-agent-skill.md)、[ADR-0011](docs/adr/0011-cli-machine-protocol.md) |
| 26 | 里程碑：M-1、M0 与 M1 已收口；M1 以 `none` 后端 walking skeleton 和 133 份全语料门禁完成，仍以语料断言而非模块完成度收口 | [docs/02-milestones.md](docs/02-milestones.md) |
| 27 | 术语细节：用户 `--glossary` 覆盖自动表；`--dump-glossary` 导出复用；`--no-auto-terms` 开关；自动表指纹进缓存键 | — |
| 28 | 性能：V1 无硬指标；方向值=20 页论文除 LLM 外 <5 分钟（arm64 笔记本）；LLM 段落级并发默认 4、指数退避重试 3 次、重试尽降级保原文 | — |
| 29 | crate 结构：**生产侧**两分——`mimus-core`（lib：IL/pass/引擎 trait/翻译层）+ `mimus`（bin：CLI/进度/配置）。workspace 另含非生产成员 `corpus`（语料门禁工具），它不依赖 `mimus-core`，也不进 release archive | — |
| 30 | Agent 集成：仓库提供一个可由 `npx skills add eeee0717/mimus` 安装的 `mimus` Agent Skill；skill 仅编排 CLI、不复制业务逻辑；MCP/daemon/vendor plugin 不进 V1 | [ADR-0008](docs/adr/0008-agent-skill.md) |
| 31 | 加密 PDF：**V1 一律拒绝**（不论是否需要密码、不论 handler），退出码 2；不做权限位尊重、无密码参数、无 `--ignore-permissions`；检测必须同时覆盖 `was_encrypted()` 与 `is_encrypted()`，任一为真即拒绝 | [ADR-0009](docs/adr/0009-reject-encrypted-pdf.md) |
| 32 | 非直立文本（旋转/镜像/斜切 > 20°，在视觉页框内度量）：**不翻译、原样 passthrough**；字符级检测、单元级隔离，同段其余字符照常翻译 | [ADR-0007](docs/adr/0007-ir-design.md) §5 |
| 33 | M0 内部执行方式（历史）：实验 1 先行（不依赖自建确定性写出器），实验 2/3 在写出器就绪后并行；最小首批 10 份 fixture 独立验收后即启动对应实验，没有等待原计划约 45 份全部齐备。该排期不改变 M-1 的整体收口边界 | — |
| 34 | Corpus v1 现实排版引擎钉死为 **Typst 0.15.1 / pdfTeX 1.40.29 / LuaHBTeX 1.24.0**；**XeTeX 出局**——xdvipdfmx 20260317 的随机字体子集标签过不了 SHA-256 复现门禁，且子集标签正是溯源断言的载体。唯一真源 `corpus/toolchain.toml`，门禁 `corpus doctor` / `corpus determinism` | — |
| 35 | 走查仲裁：PDF 规范 + hand-written manifest + 独立结构/字体/几何推导是事实层；独立 parser/renderer trace 验证落地；PDFium 只作交叉证据。恢复存在歧义时选择有界、可报告、不扩散，不能可靠恢复则降级 | [ADR-0006](docs/adr/0006-engine-combination.md) |
| 36 | 增量写回路线已验证：输入完整字节作为前缀、对象号只追加、共享资源 copy-on-write、未修改结构守恒、同目录临时文件原子发布；生产 writer 仍须在 `mimus-core` 内实现并复用这些回归断言 | [ADR-0003](docs/adr/0003-v1-native-pdf-path.md)、[ADR-0006](docs/adr/0006-engine-combination.md) |
| 37 | firecrawl-pdfium 资格结论为 **B：补齐上游后采用**。现有文本/渲染行为、稳定性和性能合格，但 T1 字符诊断、F1 字体快照、O1 对象来源映射尚未暴露；完成并复跑资格矩阵前不得改生产依赖 | [docs/05-pdfium-backend-qualification.md](docs/05-pdfium-backend-qualification.md) |
| 38 | 引擎 trait 表面按 **owned snapshot** 设计：快照类型由 `mimus-core` 自定义，`pdfium-render` 只出现在 `engine/` 实现模块，pass 代码不得引用后端类型；替换后端（如 firecrawl-pdfium）= 重实现 trait + 复跑资格矩阵，pass 代码零改动 | [ADR-0010](docs/adr/0010-engine-owned-snapshot.md) |
| 39 | CLI 机器协议 v2：`inspect` 只读至 ParagraphFind；typed diagnostic 与结构化进度走 stdout NDJSON，人类 renderer 走 stderr；正常可写流以恰一个 result/error 终结；EPIPE、部分写、Io/Internal 退出码和逐 pass debug 合同统一收口 | [ADR-0011](docs/adr/0011-cli-machine-protocol.md) |
| 40 | 扫描件判定：单页判据=有图像且 0 可见文字对象（`Tr 3`/`Tr 7` 不算可见，非直立计入，不用光栅）；文档级=扫描页 ≥ 80% × 内容页（内容页=总页−空白页，含等于）即整份拒绝（`Input/scanned_pdf`、退出码 2），低于阈值继续、扫描/空白页豁免严格 walk 整页透传；数据来自宽容预扫（严格 walk 不动、挪到 ScanDetect 后），公开 IL 保持 v1；统计以 error 字段 `scanned_pages`/`total_pages` + 单条汇总 diagnostic 公开（v2 兼容扩展） | [ADR-0012](docs/adr/0012-scan-detection-policy.md) |
| 41 | 写回改为**区间替换**（只替换被翻译单元的 text-show 操作数区间，其余原样透传），取代整流重建——这是拆除三堵 fail-closed 守卫而不破坏 Write 像素等值合同的前提；走查从白名单 fail-closed 变为有界宽容（恢复语义与边界数值见 ADR）；降级分文档/页/段三级，页级=不产生 rewrite 天然保留原流、段级=`Paragraph.preserved` 且区间不替换；「单元」是段内连续同因字符区间的**派生分组**，不是 IL 层级；非直立判定输入改为 `R(/Rotate)∘CTM∘Tm` 线性部分，页面几何事实层改由 lopdf 提供、engine 几何降为交叉证据 | [ADR-0013](docs/adr/0013-bounded-walk-and-graded-degradation.md) |
| 42 | 字体/CMap 可靠性判定链：ToUnicode → 嵌入字体 cmap → 标准编码链，任一层失败即 `unicode=None`，链中**无「CID 当 Unicode」分支**；段内存在不可解码字符、advance 非正或字体不可解析即整段保留原文；M1 预定义 CMap 只支持 Identity 及别名，其余显式降级；缺 `/Widths` 走段级降级（PDFium glyph-width 未绑定）；CJK 输入 fixture 用 Noto Sans SC 确定性子集入库 | [ADR-0014](docs/adr/0014-font-decoding-reliability.md) |

## 翻译政策表（PP-DocLayoutV3 · 25 类）

| 政策 | 类别 |
|---|---|
| 翻译 | text, paragraph_title, doc_title, abstract, content, figure_title, footnote, vision_footnote, aside_text |
| 不翻·原样保留 | header, footer, header_image, footer_image, image, chart, seal, algorithm, display_formula, formula_number, number, vertical_text, reference, reference_content, table（表体，`--translate-table` 可开） |
| 占位符处理 | inline_formula（在所属段落内以 `{v1}` 占位送翻，返回后还原） |

政策表按**版面类别**划分，与之正交的还有一条按**文本朝向**的划分：**非直立文本一律不翻译、原样 passthrough**，优先于类别政策（决策 #32）。

## 术语表

### 输入路径

- **原生 PDF（born-digital）**：content stream 里有真实文字对象的 PDF。**V1 唯一路径。**
- **扫描件（scanned）**：页面本体是位图。单页判据：有图像且 0 可见文字对象（`Tr 3` 隐形文字不算可见）；0 文字对象且无图像的是**空白页**，不是扫描页。文档级扫描页 ≥ 80% × 内容页时整份拒绝（ADR-0012）。V1 检测后明确报错拒绝，V2 经 OCR 路径支持。
- **OCR 文字层（ocr layer）**：扫描图上叠加的不可见文字（`Tr 3`），他方 OCR 注入的产物；不是 OCR 本身。
- **加密 PDF**：输入带 `/Encrypt` 的文档。V1 在 Parse 打开处一律拒绝（ADR-0009）。lopdf 对空密码档可能解密并抹掉 `/Encrypt`（仅 `was_encrypted()` 为真），对非空密码档也可能加载后保留 `/Encrypt`（仅 `is_encrypted()` 为真），故检测取两者之或；缺任一腿都可能静默放行。

### 解析层

- **操作符走查（operator walk）**：在 lopdf 提供的原始 content stream 字节上自写 tokenizer + 图形状态机 + CTM 栈 + 文本定位，字符坐标自算；度量委托 PDFium，text page 做交叉校验（ADR-0006）。
- **passthrough**：不理解的操作符原样存字节、输出时透传，IL 只覆盖"要修改的部分"。依赖自写走查（PDFium API 拿不到原始字节）。
- **PdfInspector / Rasterizer trait**：字符度量与光栅化的能力边界。当前 V1 后端为 pdfium-render；firecrawl-pdfium 只有在 T1/F1/O1 上游能力补齐并重新通过 `docs/05-pdfium-backend-qualification.md` 的矩阵后才可替换。

### 中间表示

- **IL / IR**：流水线围绕其变换的中间表示，单字符粒度（ADR-0007）。
- **双盒（dual box）**：`box` 字体度量盒 / `visual_bbox` 墨迹盒；layout 归属用墨迹盒算 IoU。
- **文本载体 enum**：tagged enum，V1 仅 `Chars`，V2 加 `OcrLine`（扫描预留）。
- **非直立文本（non-upright text）**：旋转 ≠ 0°、镜像（变换行列式为负）或斜切 > 20° 的字符，三者合并为一个概念。判定在**视觉页框**内做——即先应用页面 `/Rotate`，否则一张 `/Rotate 90` 的页面会整篇被误判为旋转。直立容差 **±0.1°**（沿用 BabelDOC 的实测值），**180° 算非直立**，所以直立窗口只有 `0° ± 0.1°`。斜切 ≤ 20° 且无旋转/镜像分量的**仍算直立、照常翻译**（伪斜体在真实 PDF 中太常见，且字形样式本就按既有 CJK 政策丢弃）。
- **TextTransform**：非直立判定在 IR 中的载体，和类型 `{ Upright, Rotated(deg), Mirrored, Skewed(deg) }`（ADR-0007 §5）。
- **单元（unit）**：段内**连续、同一隔离原因**的字符区间（非直立、不可见 `Tr 3`/`Tr 7`、不可定位等）。它**不是 IL 层级**——IL 保持 Document → Page → Paragraph → Char 四级，单元由 Typeset 从 `Char.text_transform` 与可见性标记动态聚合（ADR-0013 §2）。
- **单元级隔离**：非直立检测是**字符级**的，但隔离是**单元级**的——非直立字符成为独立的 passthrough 单元，同段其余字符照常翻译。落地形式是「该单元的字节区间不进替换集」。非直立字符**参与** layout 归属、**不参与** fallback 聚类、**计入**扫描件判定的文本对象计数。
- **段级保留**：整段不翻译、原文原样保留，载体是 `il::Paragraph.preserved`（可选字段，IL schema 仍为 v1），此时 `translated_text` 恒为 `None`。触发条件见 ADR-0014 §4。
- **页级降级**：该页不产生 `PageRewrite`，增量写回天然逐字节保留其原 content stream；其余页继续处理，受影响页号进汇总 diagnostic（ADR-0013 §2/§5）。
- **fallback_line**：版面模型漏检区域由字符聚类兜底生成的伪 layout，防漏检段落丢失。

### 版面与推理

- **layout 归属**：字符→版面框分配（R-tree + IoU + label 优先级）。
- **模型阅读顺序（model order）**：PP-DocLayoutV3 输出的阅读顺序，段落排序以其为准、几何兜底。
- **EP（Execution Provider）**：onnxruntime 后端。V1 只用 CPU EP。

### 写回

- **增量改写（incremental rewrite）**：保留原 PDF 全部对象，只改 content stream、追加字体。**V1 写回模型**（lopdf）。
- **区间替换（span splice）**：页面输出 content = 原解码流字节，只把被翻译单元的 text-show 操作数区间换成新字节，其余（未知操作符、图形状态、内联图像、Form 调用、隔离单元、保留段）原样透传。取代「从 IL 重建整流」，是有界宽容走查能与 Write 像素等值验证共存的前提（ADR-0013 §1）。多 Contents 页逐流替换、不合并。
- **从零重建（rebuild）**：全新构建文档，扫描件路径（V2，krilla/pdf-writer 一类）。两模型不可互换。

### 翻译层

- **Translator trait**：翻译后端抽象。V1：`openai`（endpoint+key+model）与 `none`（原文直通，离线测试排版链路）。
- **占位符协议**：公式→`{v1}`、富文本→`<b1>…</b1>`；失败模式（占位符丢失/重复、原样回显）一律降级保原文。
- **自动术语提取（ExtractTerms）**：翻译前的独立 LLM pass，产出全文术语表注入翻译 prompt；用户 `--glossary` 优先覆盖；`--dump-glossary` 导出供人工校对后回传（`--no-auto-terms` 可关）。
- **翻译缓存**：段落级，redb，键含 prompt 版本与术语指纹。

### 资产与分发

- **资产（assets）机制**：模型/字体统一管理——运行时下载 + sha256 校验 + 缓存 + 镜像可配 + 自备路径逃生门（ADR-0005）。

### Agent 集成

- **Agent Skill**：仓库内的 `skills/mimus/` 指令包；用户通过 `npx skills add eeee0717/mimus` 安装，使支持 skill 的 agent 能调用 `translate`、`inspect`、`assets pull`。它是 CLI 的薄编排层，不含业务实现。
- **Skill 安装边界**：`npx skills add` 只安装指令包，不安装 `mimus` 二进制、模型或字体；skill 需检查 CLI 是否存在及版本是否兼容，资产仍由 CLI 管理。
- **机器调用协议**：所有子命令的 `--json` 模式；当前 CLI schema 为 v2。结构化进度、typed diagnostic 和结果都走 stdout NDJSON；正常可写流以单个 result/error 事件终结。无 spinner、颜色或交互提示；人类可读 renderer 走 stderr，分类退出码仍是第一层结果信号（ADR-0011）。
- **行为真源**：人、脚本和 agent 都通过同一个 CLI 执行；skill 不复制参数语义、翻译策略或错误恢复逻辑。

### 测试

- **Corpus v1**：从零构建的合成语料，入 repo `corpus/`。截至 2026-08-23，共 74 份 M0 fixture 已按生成合同独立验收入库；M1/M3 语料随 pass 逐批扩展。早期 23 份合成语料永久作废（坐标偏移与视觉质量问题），其几何参数与生成代码不得参考。
- **失效模式（failure mode）/ case**：从 BabelDOC 主链路逆向出的、可由 PDF 输入触发的一类错误。带稳定 case ID，是 Corpus v1 需求矩阵的行。
- **fixture**：语料中的一份 PDF + 其 manifest。分 **unit**（单变量，与 case 近 1:1）、**mal**（畸形，由合法父本做单变量字节级变异）、**intg**（显式多变量，仅端到端冒烟，严格受限）三类。
- **生成合同（generation contract）**：fixture 入库前必须满足的约束集合（坐标系、三种盒子、变换规则、单变量、确定性 SHA-256、独立验收等），见 `docs/03-corpus-requirements.md` §2。
- **expected manifest**：一份 fixture 一份 TOML 规格，**先于生成器手写**，来源是 PDF 规范与字体度量推导而非生成结果回读——生成器由 manifest 检验，不得反向迁就。唯一例外是现实排版 fixture（见下）。
- **现实排版 fixture 的双解析器裁定**：Typst/LaTeX 产出的 fixture 无法手推字形坐标，因此结构化期望仍手写，**几何由两个互相独立的解析器（poppler + mutool）一致确立**；两者不一致即阻止该 fixture 入库。这保住了第一原则的实质——期望值不与生成器同源。见 `docs/03-corpus-requirements.md` §2.1。
- **溯源断言**：判断输出 PDF 中某字形来自原文还是译文，依据是**对象号 + 子集标签（subset tag）**，不依赖"输入输出字体不同族"——后者对 CJK 输入 fixture 不可得（可选字体少且体积大，可能不得不与 Noto Sans SC 同族）。
- **三种盒子**：baseline origin（绘制起点）/ 度量盒（字体 ascent/descent × 字号）/ visual bbox（墨迹）。三者独立标注，混用是旧语料偏移问题的疑似根因。
- **differential signal**：BabelDOC 在同一 fixture 上的行为，仅作差分参考触发人工裁定，**不得作为唯一正确性 oracle**。
- **真实语料**：arXiv 按排版引擎分层（pdfTeX/XeTeX/LuaTeX/Word）下载，不入 repo，发布前人工 checklist。
- **质量四件套**：IL 快照（insta）/ 占位符守恒 / 零 panic / 几何断言（译文框不越界不压图）。CI 强制绿。
- **三层降级**：段落（占位符失败→保原文）→ 页（排版失败→保原页）→ 文档（解析失败→分类退出）。

### 工程

- **churn 系数**：BabelDOC 写了约 2.25 倍代码才收敛的经验值。
- **双 schema_version**：CLI 机器协议（当前 v2）与 IL 序列化（当前 v1）各带独立的 `schema_version`——消费者与演进节奏不同，互不耦合；IL 升版不要求 CLI 同步升版。
- **PoC 冻结**：`m0-experiment-2` crate 是 M0 实验 2 的 PoC，冻结为 corpus `operator-walk:*` 门禁的执行体；生产操作符走查在 `mimus-core` 内另行实现，以实验 2 报告与同批 fixture 为回归基准，不复用 PoC 代码。

## 待决清单

与 `docs/03-corpus-requirements.md` §6 同源，此处为索引。2026-08-21 的设计会话已收口其中三条：加密 PDF（决策 #31 / ADR-0009）、非直立文本（决策 #32）、M0 fixture 排期（决策 #33）。2026-08-23 的设计会话（#16 收口）补上扫描件判定政策（决策 #40 / ADR-0012）。2026-08-24 的 #17/#18/#19 联合设计会话补上有界走查与分级降级（决策 #41 / ADR-0013）、字体可靠性判定链（决策 #42 / ADR-0014），并收口 CJK fixture 字体选型。

**仍需决策者拍板（0 条）**

- ~~旧语料目录的实际删除~~ → 2026-08-22 已由决策者删除 `~/Downloads/babeldoc-corpus/`，实测目录不存在。旧语料的误引用风险就此消失，Corpus v1 从零构建的前提完整成立。

**已定去向，拆票执行（不需再决策）**

- ~~确定性生成的引擎侧机制尚未实测~~ → 已收口（决策 #34）：三条配方实测通过，XeTeX 出局。
- ~~验收工具链缺四件（qpdf / poppler / mupdf-tools / Typst）~~ → 已收口（决策 #34）：四件齐备并钉死版本。
- ~~PP-DocLayoutV3 的 25 类是否含目录类未确认~~ → 已结清（M0 实验 1）：**`content` 就是目录类**，`unit-para-04-toc` 上 0.955 一个框盖住整个目录；但模型只给整块框，条目与页码的切分仍需自行实现。见 [docs/04-m0-experiment-1.md](docs/04-m0-experiment-1.md) §4 D7。
- ~~CJK 输入 fixture 的字体选型~~ → 2026-08-24 收口（决策 #42 / ADR-0014 §6）：Noto Sans SC Regular 的确定性子集入库，沿用 `MimusExact.ttf` 的范式（pyftsubset 钉参数 + OFL 许可证并排 + 两次生成比对 + manifest 钉 SHA-256）。与输出字体同族无碍——溯源依据是对象号 + 子集标签。

**由实验给出结论，不是决策**

- ~~PP-DocLayoutV3 的第 7 列语义、model order 与 D1–D12 红利~~ → 已由 M0 实验 1 结清，结论见 [docs/04-m0-experiment-1.md](docs/04-m0-experiment-1.md)，ADR-0002 已就地修订。
- ~~走查与 PDFium 不一致时的仲裁规则~~ → 已由 M0 实验 2 结清为决策 #35；FONT-02、CMAP-04、STREAM-02、Type3 与 inline image 均按同一规则裁定。见 [docs/04-m0-experiment-2.md](docs/04-m0-experiment-2.md) §7。

M-1 与 M0 已收口，无遗留架构风险阻塞 M1。当前实现入口是 #14：以 `none` 后端往返单行原生 PDF；架构设计只覆盖支撑该 walking skeleton 所需的接口与边界。

V2 展望项（扫描件/OCR、子进程隔离、像素 diff、宋体字族、中→英、GUI）见 `docs/02-milestones.md` 末节，随触发条件另行立项。
