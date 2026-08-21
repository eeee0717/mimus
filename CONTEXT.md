# CONTEXT

> 项目共享术语与决策索引。术语随设计会话逐轮补充；难以逆转的技术选择在 `docs/adr/` 以 ADR 形式记录。
> 事实基础：`docs/01-research.md`（2026-08-21 调研报告）+ 2026-08-21 模型/引擎事实查证（要点已并入 ADR-0002/0006）。

## 已定决策

| # | 决策 | ADR |
|---|---|---|
| 1 | 实现语言 Rust，唯一执行接口为 CLI（暂不做 GUI/MCP）；release 附带薄 Agent Skill 调用 CLI | [ADR-0001](docs/adr/0001-rust-cli.md)、[ADR-0008](docs/adr/0008-agent-skill.md) |
| 2 | 版面检测模型：PP-DocLayoutV3 官方 ONNX（131 MB，25 类，含阅读顺序） | [ADR-0002](docs/adr/0002-pp-doclayoutv3.md) |
| 3 | V1 仅原生 PDF 路径（扫描件报错拒绝）；OCR 后置 V2，IR/架构预留 | [ADR-0003](docs/adr/0003-v1-native-pdf-path.md) |
| 4 | 许可：MIT 单许可 | [ADR-0004](docs/adr/0004-mit-license.md) |
| 5 | 分发：单 archive 解压即用；模型/字体资产运行时下载 + 自备逃生门 + 镜像可配 | [ADR-0005](docs/adr/0005-distribution.md) |
| 6 | 引擎组合：lopdf（对象树/增量写回/原始字节）+ pdfium-render（度量/光栅化，trait 边界后）+ 自写操作符走查 | [ADR-0006](docs/adr/0006-engine-combination.md) |
| 7 | IR：单字符粒度 + 双盒 + Rust enum + serde JSON 快照（schema_version） | [ADR-0007](docs/adr/0007-ir-design.md) |
| 8 | 目标用户：开源发布，面向"读外文 PDF 的中文研究者"；验收场景=作者本人日常翻译 arXiv 论文 | — |
| 9 | 语种：输入语言解耦（取决于文本提取），验收只对英→中；排版仅调优简体中文输出 | — |
| 10 | 输出模式：默认仅译文；`--bilingual` 原/译交替页（修书签/内链页目标映射）；并排不做 | — |
| 11 | 保真范围：书签不翻译、链接热区不调整、表单/OCG 原样透传；断言书签数/注释数不变 | — |
| 12 | 流水线：固定顺序 pass 链（`fn(&mut Document)`，无 pass 框架）；pass 内按页 rayon 并行；`--debug` 逐 pass 落盘 IL | — |
| 13 | 阶段草案：Parse → ScanDetect(拒绝) → Layout → ParagraphFind → StylesAndFormulas → ExtractTerms → Translate → Typeset → FontEmbed → Write | — |
| 14 | 阅读顺序：以 V3 模型输出为准 + 几何排序兜底；`[M,7]` 第 7 列语义验证是里程碑 0 实验项 | — |
| 15 | 翻译政策表（见下）；表体默认不翻留 `--translate-table` 实验开关 | — |
| 16 | 错误恢复：三层降级（段→页→文档，宁保原文不出坏译文）+ 结束汇总 + `--strict`；畸形 PDF fail-fast、修复函数语料驱动；退出码 0/1/2/3/4 分类 | — |
| 17 | V1 单进程；PDFium 崩溃（abort）接受，子进程隔离推 V2 | — |
| 18 | 字体：Noto Sans SC（Regular+Bold）单族走资产机制；subsetter 子集化；`--font` 覆盖；italic 映射正常字重 | — |
| 19 | 术语：**保留自动术语提取**（独立 pass，LLM）+ `--glossary` 用户术语表 | — |
| 20 | 翻译层：`Translator` trait 可扩展；V1 实现 openai 兼容 + `none` 直通；占位符协议失败降级保原文 | — |
| 21 | 翻译缓存：V1 即做，redb，键含(原文,模型,目标语,prompt 版本,术语指纹)；`--no-cache` | — |
| 22 | 推理纯 CPU（ort CPU EP）；GPU/NPU EP 不进 V1 | — |
| 23 | 质量回归四件套：全语料 IL 快照 + 占位符守恒 + 零 panic + 几何断言；**CI 绿才能合并**；渲染像素 diff 待渲染路径稳定后加 | — |
| 24 | 语料：**Corpus v1 待从零构建**（旧 23 份合成语料已因坐标偏移/视觉质量问题作废，不得参考）。需求矩阵与生成合同见 [docs/03-corpus-requirements.md](docs/03-corpus-requirements.md)，工作纳入里程碑 M-1；真实语料不进 repo、发布前人工 checklist | — |
| 25 | CLI：子命令结构（`translate` / `assets pull` / `inspect`）；配置三层 flags > env > `~/.config/mimus/config.toml`；API key 不走明文 flag；两级人类可读进度；V1 `--json` 输出带版本的 NDJSON 机器协议；细粒度 flags 随功能里程碑落地 | [ADR-0008](docs/adr/0008-agent-skill.md) |
| 26 | 里程碑：M-1（Corpus Foundation，前置且阻塞 M0/M1）+ walking skeleton 五段 M0–M4，以语料断言收口（[docs/02-milestones.md](docs/02-milestones.md)） | — |
| 27 | 术语细节：用户 `--glossary` 覆盖自动表；`--dump-glossary` 导出复用；`--no-auto-terms` 开关；自动表指纹进缓存键 | — |
| 28 | 性能：V1 无硬指标；方向值=20 页论文除 LLM 外 <5 分钟（arm64 笔记本）；LLM 段落级并发默认 4、指数退避重试 3 次、重试尽降级保原文 | — |
| 29 | crate 结构：workspace 两分——`mimus-core`（lib：IL/pass/引擎 trait/翻译层）+ `mimus`（bin：CLI/进度/配置） | — |
| 30 | Agent 集成：V1 archive 附带一个 `mimus` Agent Skill；skill 仅编排 CLI、不复制业务逻辑；MCP/daemon/vendor plugin 不进 V1 | [ADR-0008](docs/adr/0008-agent-skill.md) |

## 翻译政策表（PP-DocLayoutV3 · 25 类）

| 政策 | 类别 |
|---|---|
| 翻译 | text, paragraph_title, doc_title, abstract, content, figure_title, footnote, vision_footnote, aside_text |
| 不翻·原样保留 | header, footer, header_image, footer_image, image, chart, seal, algorithm, display_formula, formula_number, number, vertical_text, reference, reference_content, table（表体，`--translate-table` 可开） |
| 占位符处理 | inline_formula（在所属段落内以 `{v1}` 占位送翻，返回后还原） |

## 术语表

### 输入路径

- **原生 PDF（born-digital）**：content stream 里有真实文字对象的 PDF。**V1 唯一路径。**
- **扫描件（scanned）**：页面本体是位图、0 文字对象。V1 检测后明确报错拒绝，V2 经 OCR 路径支持。
- **OCR 文字层（ocr layer）**：扫描图上叠加的不可见文字（`Tr 3`），他方 OCR 注入的产物；不是 OCR 本身。

### 解析层

- **操作符走查（operator walk）**：在 lopdf 提供的原始 content stream 字节上自写 tokenizer + 图形状态机 + CTM 栈 + 文本定位，字符坐标自算；度量委托 PDFium，text page 做交叉校验（ADR-0006）。
- **passthrough**：不理解的操作符原样存字节、输出时透传，IL 只覆盖"要修改的部分"。依赖自写走查（PDFium API 拿不到原始字节）。
- **PdfInspector / Rasterizer trait**：字符度量与光栅化的能力边界，V1 由 PDFium 实现，后端可整体替换。

### 中间表示

- **IL / IR**：流水线围绕其变换的中间表示，单字符粒度（ADR-0007）。
- **双盒（dual box）**：`box` 字体度量盒 / `visual_bbox` 墨迹盒；layout 归属用墨迹盒算 IoU。
- **文本载体 enum**：tagged enum，V1 仅 `Chars`，V2 加 `OcrLine`（扫描预留）。
- **fallback_line**：版面模型漏检区域由字符聚类兜底生成的伪 layout，防漏检段落丢失。

### 版面与推理

- **layout 归属**：字符→版面框分配（R-tree + IoU + label 优先级）。
- **模型阅读顺序（model order）**：PP-DocLayoutV3 输出的阅读顺序，段落排序以其为准、几何兜底。
- **EP（Execution Provider）**：onnxruntime 后端。V1 只用 CPU EP。

### 写回

- **增量改写（incremental rewrite）**：保留原 PDF 全部对象，只改 content stream、追加字体。**V1 写回模型**（lopdf）。
- **从零重建（rebuild）**：全新构建文档，扫描件路径（V2，krilla/pdf-writer 一类）。两模型不可互换。

### 翻译层

- **Translator trait**：翻译后端抽象。V1：`openai`（endpoint+key+model）与 `none`（原文直通，离线测试排版链路）。
- **占位符协议**：公式→`{v1}`、富文本→`<b1>…</b1>`；失败模式（占位符丢失/重复、原样回显）一律降级保原文。
- **自动术语提取（ExtractTerms）**：翻译前的独立 LLM pass，产出全文术语表注入翻译 prompt；用户 `--glossary` 优先覆盖；`--dump-glossary` 导出供人工校对后回传（`--no-auto-terms` 可关）。
- **翻译缓存**：段落级，redb，键含 prompt 版本与术语指纹。

### 资产与分发

- **资产（assets）机制**：模型/字体统一管理——运行时下载 + sha256 校验 + 缓存 + 镜像可配 + 自备路径逃生门（ADR-0005）。

### Agent 集成

- **Agent Skill**：随 release archive 附带的 `skills/mimus/` 指令包，使支持 skill 的 agent 能调用 `translate`、`inspect`、`assets pull`。它是 CLI 的薄编排层，不含业务实现。
- **机器调用协议**：所有子命令的 `--json` 模式；stdout 仅输出带 `schema_version` 的 NDJSON 事件流，并以单个 result/error 事件终结。无 spinner、颜色或交互提示；分类退出码仍是第一层结果信号。
- **行为真源**：人、脚本和 agent 都通过同一个 CLI 执行；skill 不复制参数语义、翻译策略或错误恢复逻辑。

### 测试

- **Corpus v1**：从零构建的合成语料，入 repo `corpus/`。**尚未生成任何 PDF**；早期 23 份合成语料已作废（坐标偏移与视觉质量问题），其几何参数与生成代码不得参考。
- **失效模式（failure mode）/ case**：从 BabelDOC 主链路逆向出的、可由 PDF 输入触发的一类错误。带稳定 case ID，是 Corpus v1 需求矩阵的行。
- **fixture**：语料中的一份 PDF + 其 manifest。分 **unit**（单变量，与 case 近 1:1）、**mal**（畸形，由合法父本做单变量字节级变异）、**intg**（显式多变量，仅端到端冒烟，严格受限）三类。
- **生成合同（generation contract）**：fixture 入库前必须满足的约束集合（坐标系、三种盒子、变换规则、单变量、确定性 SHA-256、独立验收等），见 `docs/03-corpus-requirements.md` §2。
- **expected manifest**：一份 fixture 一份 TOML 规格，**先于生成器手写**，来源是 PDF 规范与字体度量推导而非生成结果回读——生成器由 manifest 检验，不得反向迁就。
- **三种盒子**：baseline origin（绘制起点）/ 度量盒（字体 ascent/descent × 字号）/ visual bbox（墨迹）。三者独立标注，混用是旧语料偏移问题的疑似根因。
- **differential signal**：BabelDOC 在同一 fixture 上的行为，仅作差分参考触发人工裁定，**不得作为唯一正确性 oracle**。
- **真实语料**：arXiv 按排版引擎分层（pdfTeX/XeTeX/LuaTeX/Word）下载，不入 repo，发布前人工 checklist。
- **质量四件套**：IL 快照（insta）/ 占位符守恒 / 零 panic / 几何断言（译文框不越界不压图）。CI 强制绿。
- **三层降级**：段落（占位符失败→保原文）→ 页（排版失败→保原页）→ 文档（解析失败→分类退出）。

### 工程

- **churn 系数**：BabelDOC 写了约 2.25 倍代码才收敛的经验值。

## 待决清单

Corpus v1 调研新暴露的范围与排期问题仍列在 `docs/03-corpus-requirements.md` §6；其中加密 PDF、任意角度旋转文本和 M0 fixture 切分需在拆票前由决策者收口。V2 展望项（扫描件/OCR、子进程隔离、像素 diff、宋体字族、中→英、GUI）见 `docs/02-milestones.md` 末节，随触发条件另行立项。
