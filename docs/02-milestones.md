# 里程碑设计（V1）

> 切法：walking skeleton——先打通极窄端到端，逐里程碑加宽。每个里程碑以**语料上可验证的断言**收口，不以"模块写完"收口。
> 决策依据见 `CONTEXT.md` 与 `docs/adr/`；制定于 2026-08-21，状态更新于 2026-09-01。
>
> 资产说明：M0–M3 开发期模型/字体经"自备路径"逃生门手动放置；自动下载机制到 M4 才建。
>
> 语料说明：早期 23 份合成语料已因坐标偏移与视觉质量问题**作废**，其几何参数、生成代码与文件编号一律不得参考。Corpus v1 已从零建立；每份 fixture 仍须按 `docs/03-corpus-requirements.md` §2.8 独立验收后方可入库。M-1 与 M0 的阻塞已解除。

## M-1 · Corpus Foundation（已完成，前置，不交付功能）

语料是后续每个里程碑的收口手段，因此先于 M0。本里程碑只产出需求、合同与清单，不产出未经验证的 PDF。

- **需求矩阵**：以 BabelDOC 主链路源码为第一手来源，逆向出可由 PDF 输入触发的失效模式，逐条给出 case ID、源码位置、触发条件、V1 相关性、最小构造方案、可观察预期行为、验证 oracle、优先级、合法性。产物：`docs/03-corpus-requirements.md` §3。
- **生成合同**：坐标系与三种盒子、变换规则、单变量原则、生成方式分层、确定性 SHA-256、manifest 规格、独立验收、differential signal 边界、fixture ID 体系。产物：同文档 §2。
- **首批清单**：从矩阵中选出 M0 与 M1 所需的 fixture，逐份定 ID 与覆盖的 case。产物：同文档 §4。
- **工具链前置**：补装独立验收所需的第三方工具（结构解析器、文本坐标提取、独立渲染器、现实排版引擎），并钉死版本记入合同。

**收口断言**：

1. Corpus v1 需求矩阵完成；
2. 生成合同明确；
3. 首批 M0/M1 fixture 清单确定；
4. **尚未生成任何未经独立验证的 PDF**——不存在游离在合同与验收之外的语料文件。

**状态（2026-08-23）**：四条收口断言均满足。工具链、确定性 writer、manifest schema 和独立验收门禁已落地；M0 fixture 随后均经门禁入库，没有改变 M-1 本身“不接收未经验证 PDF”的边界。

## M0 · 风险探测（已完成，不交付功能）

三个实验对应三大技术风险，任何一个翻车都在最便宜的时刻翻。每个实验产出一页结论（成 / 败 / 替代方案），失败即触发对应 ADR 复议。

| # | 实验 | 验证的风险 |
|---|---|---|
| 1 | **成**：PP-DocLayoutV3 ONNX 经 ort CPU 跑通；第 7 列与模型边界已实测。见 [04-m0-experiment-1.md](04-m0-experiment-1.md) | 模型可用性 + 阅读顺序红利是否成立（决策 #14 的兜底开关） |
| 2 | **成**：lopdf 原始 content stream 字节 ⇄ pdfium-render text page 对齐；仲裁规则已确立。见 [04-m0-experiment-2.md](04-m0-experiment-2.md) | 操作符走查可行性（ADR-0006 的核心假设） |
| 3 | **成**：增量追加、对象图守恒、copy-on-write 与失败原子性已验证。见 [04-m0-experiment-3.md](04-m0-experiment-3.md) | 增量改写模型（ADR-0003 §2） |

**执行记录**（决策 #33）：M0 没有等齐全部 fixture，而是按实验切分，每组最小 fixture 通过 `docs/03-corpus-requirements.md` §2.8 验收后即启动对应实验：

- **实验 1 先行**——它的 fixture 全部由现实排版引擎产出（走 §2.1 的双解析器裁定例外），**不依赖自建的确定性 PDF writer**，依赖链最短；顺带把 ADR-0002 遗留的阅读顺序验证提到最前，而后者决定 midend 要写多少代码。硬前置是补装 poppler 与 mupdf-tools。
- **实验 2、3 在 writer 就绪后并行**——它们需要精确 fixture 与字节级畸形变异，因此先完成自建 writer。

前置工作项中的确定性 PDF writer、验收脚本和最小首批 fixture 均已完成；截至收口，共 74 份 M0 fixture 入库。

**收口断言（已满足）**：三份实验结论文档齐备，无未决风险遗留；实验 2 已给出“PDF 规范 + hand-written manifest + 独立推导”为事实层、PDFium 为交叉证据的统一仲裁规则。

补充资格实验 [05-pdfium-backend-qualification.md](05-pdfium-backend-qualification.md) 的结论为 **B：firecrawl-pdfium 补齐 T1/F1/O1 后采用**。这不阻塞 M1：生产侧继续使用 pdfium-render，待上游能力进入固定 revision 并重跑资格矩阵后再决定替换。

## M1 · 最小端到端（已完成）

当前入口为 GitHub Issue #14：先设计支撑单行原生 PDF 以 `none` 后端完成“解析 → 排版 → 字体嵌入 → 增量写回”的最小接口与所有权边界，再实现这条 walking skeleton；不提前设计 M2–M4 的完整模块表面。

- workspace 拆分（`mimus-core` + `mimus`）；IL 定义（ADR-0007）+ serde JSON 序列化。
- 固定 pass 链贯通：Parse → ScanDetect(拒绝) → Layout → ParagraphFind → Typeset → FontEmbed → Write（StylesAndFormulas/ExtractTerms/Translate 留桩）。
- 两条输入拒绝路径：Parse 打开处拒绝加密 PDF（ADR-0009，判定用 `was_encrypted()`），ScanDetect 拒绝扫描件；两者共用分类退出码 2。
- IL 携带 `TextTransform`，非直立文本判为 passthrough 单元、不进翻译（决策 #32）。
- `Translator` trait + `none` 后端；Noto Sans SC 嵌入 + subsetter 子集化。
- 固化 Agent Skill 所依赖的 CLI 机器调用协议：所有已实现子命令的 `--json` 输出版本化 NDJSON，禁止交互/颜色/spinner，契约测试覆盖终结事件与分类退出码（ADR-0008）。
- 测试网与 CI 同步建立：insta IL 快照（逐 pass）、全语料零 panic、CI 绿才合并。

**收口断言**：在基线文本 fixture 上以 `none` 后端产出有效 PDF，字符经"解析→排版→写回"往返后，baseline origin 与度量盒仍在 manifest 容差内；扫描件与加密 fixture 按分类退出码拒绝（加密的空密码档必须**未产生任何输出文件**——它是"静默放行"这一失败模式的唯一守卫）；非直立 fixture 的 `TextTransform` 取值与 manifest 一致，含 `/Rotate 90` 负例；畸形 fixture 以 manifest 声明的方式 fail-fast，不 panic。

**状态（2026-08-24）**：上述断言均满足。M1 最终库存为 133 份 fixture、72 个去重 case；规划期的约 138/87 是容量估算，实际矩阵通过合并等价变体、让单份 fixture 覆盖多个 case、跨 concern 复用精确父本去重。普通 CI 对全量 fixture 执行 production `inspect`/`none` 路径与逐 pass IL 快照，并以独立 qpdf、Poppler、MuPDF 门禁重新验收 Corpus v1；详见 [M1 收口记录](06-m1-closure.md)。

## M2 · 真翻译

- `openai` 后端（endpoint+key+model 三层配置）；占位符协议 + 守恒断言；StylesAndFormulas pass（公式/富文本识别）。
- ExtractTerms pass（自动术语提取）+ `--glossary`/`--dump-glossary`/`--no-auto-terms`。
- redb 翻译缓存（键含 prompt 版本 + 术语指纹）；段落级并发（默认 4）+ 指数退避重试 3 次。
- 三层降级全链贯通（段→页→文档）+ 结束汇总 + `--strict`。

**收口断言**：合成语料非畸形组端到端全绿；占位符守恒 100%（违者必已降级且计入汇总）；同文档重跑第二次 API 调用数为 0（缓存命中）。

**实现状态（2026-08-25，待 review/merge）**：上述断言已由 loopback deterministic
Responses fake server、142 份 Corpus v1 inventory 和 production CLI 路径满足；M1 的
`none` 后端与独立 corpus oracle 继续全绿。#25–#32 的线性 stack 尚未合入
`master`，因此正式里程碑状态仍待 trunk-first review、合并和 master CI。完整证据见
[M2 实现收口记录](08-m2-closure.md)。

## M3 · 质量攻坚

- 启发式调参主战场：段落识别、fallback_line、layout 归属优先级、排版 scale 搜索——以真实文档反馈驱动。
- 语料扩展：把需求矩阵中标为 M3 的失效模式逐条落成 fixture（同样走生成合同与独立验收）；每修一个真实翻车文档，先补对应 fixture 再改代码。
- 真实语料 checklist：arXiv 按排版引擎分层（pdfTeX / XeTeX / LuaTeX / Word 导出）约 20 份。
- `--bilingual` 交替页（含书签/内链页目标重映射）；几何断言进 CI（译文框不越界、不压图/公式）。

**收口断言**：综合版面 integration fixture 与双栏、公式类 fixture 全部通过几何断言；真实语料 20 份零崩溃、人工 checklist 通过；矩阵中标为 M3 的失效模式全部有对应 fixture 且 CI 全绿。

**状态（2026-09-01，closing stack 待合入）**：收口断言全部满足。Primary M3
覆盖为 46/46：45 个 case 由通过生成合同和独立验收的静态 fixture 覆盖，`WRITE-05`
由真实 CLI 的有界 kill/OOM 子进程矩阵覆盖。Corpus v1 共 201 份 fixture，精确工具链的
doctor、determinism 与独立 verify 已进入 required CI。20 份分层真实论文以守恒 fake
全部发布、零 `Internal/6`，最终产物 INK-01 为零；封闭缓存的默认与双语锚定均为 108 hit、
0 miss、0 provider call。#38 closing PR 合入后按 `#39 → #40 → #41 → #42` 解锁 M4；
逐 case、锚定 SHA、集群归因与 CI 时长以 #38 的 closing evidence 为持久记录。

**M3.8 后续质量修正（2026-09-04，待 review/merge）**：编号标题不再把节号与标题
首项粘连；保留节号与标题分别复原源 x 坐标，残余间距最低为 `0.25em` 并有 typed
证据。TRANS-01 新增 6 个单变量 fixture，Corpus v1 增至 207 份；BERT 封闭缓存回放
197 hit / 0 miss，锚定默认与双语各 108 / 0。宋体 20 篇再基线 20/20 发布、338/338
编号标题通过、160 个 `typeset_overflow` 与 M3.7 一致，且请求准备和字号拟合政策不变。

## M4 · 发布

- 资产机制：清单（名称→URL+sha256）、运行时下载、镜像可配、`assets pull` 预取。
- release archive：二进制 + libpdfium + 许可声明（MIT / PDFium BSD / 模型 Apache-2.0）；macOS arm64/x64 + Linux x64 + Windows x64。
- 在仓库发布 `skills/mimus/`，对外安装入口为 `npx skills add eeee0717/mimus`；skill 声明兼容的 CLI 版本，并明确二进制与资产需另行安装。
- 用 skill 创建器校验结构，并在干净环境中经 `npx skills add` 安装后前向测试 agent 的三条工作流：翻译 PDF、诊断版面/IL、预取资产；skill 不得读取或回显 API key。
- README / 使用文档 / 术语校对工作流说明。

**收口断言**：在一台干净机器上，从 GitHub Release 下载 archive → 解压 → 首跑自动拉资产 → 完成一篇真实 arXiv 论文的翻译；再执行 `npx skills add eeee0717/mimus` 安装 skill，由 agent 通过 CLI 机器协议完成一次 `inspect` 与一次 `translate`，全程不需要人工解析终端文本。

## V2 展望（不承诺，触发条件另行立项）

扫描件/OCR 路径（PP-OCRv6，rebuild writer）、子进程隔离（待真实崩溃数据）、渲染像素 diff 回归、宋体字族映射、中→英（西文断字）、表体翻译转正、GUI、MCP/常驻 daemon/vendor-specific plugin。
