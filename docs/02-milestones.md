# 里程碑设计（V1）

> 切法：walking skeleton——先打通极窄端到端，逐里程碑加宽。每个里程碑以**语料上可验证的断言**收口，不以"模块写完"收口。
> 决策依据见 `CONTEXT.md` 与 `docs/adr/`；日期 2026-08-21。
>
> 资产说明：M0–M3 开发期模型/字体经"自备路径"逃生门手动放置；自动下载机制到 M4 才建。
>
> 语料说明：早期 23 份合成语料已因坐标偏移与视觉质量问题**作废**，其几何参数、生成代码与文件编号一律不得参考。Corpus v1 从零构建，由 **M-1** 前置交付需求与合同；此后每份 fixture 按 `docs/03-corpus-requirements.md` §2.8 独立验收后方可入库。**M-1 阻塞 M0 与 M1**。

## M-1 · Corpus Foundation（前置，不交付功能）

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

## M0 · 风险探测（不交付功能）

前置工作项：实现确定性 PDF writer 与验收脚本，按 M-1 清单生成首批 unit fixture，逐份通过 `docs/03-corpus-requirements.md` §2.8 验收后入库——三个实验都以这批 fixture 为输入。

三个实验对应三大技术风险，任何一个翻车都在最便宜的时刻翻。每个实验产出一页结论（成 / 败 / 替代方案），失败即触发对应 ADR 复议。

| # | 实验 | 验证的风险 |
|---|---|---|
| 1 | PP-DocLayoutV3 ONNX 经 ort CPU 跑通；在双栏 fixture 的渲染图上验证 `[M,7]` 第 7 列是否为阅读顺序（ADR-0002 遗留） | 模型可用性 + 阅读顺序红利是否成立（决策 #14 的兜底开关） |
| 2 | lopdf 原始 content stream 字节 ⇄ pdfium-render text page 对齐 PoC：同一页上自写 tokenizer 的字符定位与 PDFium 结果交叉核对 | 操作符走查可行性（ADR-0006 的核心假设） |
| 3 | 增量写回 PoC：lopdf 改写一页 content stream + 追加一个字体对象，输出 PDF 在主流阅读器中有效 | 增量改写模型（ADR-0003 §2） |

**收口断言**：三份实验结论文档齐备，无未决风险遗留。

## M1 · 最小端到端

- workspace 拆分（`mimus-core` + `mimus`）；IL 定义（ADR-0007）+ serde JSON 序列化。
- 固定 pass 链贯通：Parse → ScanDetect(拒绝) → Layout → ParagraphFind → Typeset → FontEmbed → Write（StylesAndFormulas/ExtractTerms/Translate 留桩）。
- `Translator` trait + `none` 后端；Noto Sans SC 嵌入 + subsetter 子集化。
- 测试网与 CI 同步建立：insta IL 快照（逐 pass）、全语料零 panic、CI 绿才合并。

**收口断言**：在基线文本 fixture 上以 `none` 后端产出有效 PDF，字符经"解析→排版→写回"往返后，baseline origin 与度量盒仍在 manifest 容差内；扫描件 fixture 按分类退出码拒绝；畸形 fixture 以 manifest 声明的方式 fail-fast，不 panic。

## M2 · 真翻译

- `openai` 后端（endpoint+key+model 三层配置）；占位符协议 + 守恒断言；StylesAndFormulas pass（公式/富文本识别）。
- ExtractTerms pass（自动术语提取）+ `--glossary`/`--dump-glossary`/`--no-auto-terms`。
- redb 翻译缓存（键含 prompt 版本 + 术语指纹）；段落级并发（默认 4）+ 指数退避重试 3 次。
- 三层降级全链贯通（段→页→文档）+ 结束汇总 + `--strict`。

**收口断言**：合成语料非畸形组端到端全绿；占位符守恒 100%（违者必已降级且计入汇总）；同文档重跑第二次 API 调用数为 0（缓存命中）。

## M3 · 质量攻坚

- 启发式调参主战场：段落识别、fallback_line、layout 归属优先级、排版 scale 搜索——以真实文档反馈驱动。
- 语料扩展：把需求矩阵中标为 M3 的失效模式逐条落成 fixture（同样走生成合同与独立验收）；每修一个真实翻车文档，先补对应 fixture 再改代码。
- 真实语料 checklist：arXiv 按排版引擎分层（pdfTeX / XeTeX / LuaTeX / Word 导出）约 20 份。
- `--bilingual` 交替页（含书签/内链页目标重映射）；几何断言进 CI（译文框不越界、不压图/公式）。

**收口断言**：综合版面 integration fixture 与双栏、公式类 fixture 全部通过几何断言；真实语料 20 份零崩溃、人工 checklist 通过；矩阵中标为 M3 的失效模式全部有对应 fixture 且 CI 全绿。

## M4 · 发布

- 资产机制：清单（名称→URL+sha256）、运行时下载、镜像可配、`assets pull` 预取。
- release archive：二进制 + libpdfium + 许可声明（MIT / PDFium BSD / 模型 Apache-2.0）；macOS arm64/x64 + Linux x64 + Windows x64。
- README / 使用文档 / 术语校对工作流说明。

**收口断言**：在一台干净机器上，从 GitHub Release 下载 archive → 解压 → 首跑自动拉资产 → 完成一篇真实 arXiv 论文的翻译。

## V2 展望（不承诺，触发条件另行立项）

扫描件/OCR 路径（PP-OCRv6，rebuild writer）、子进程隔离（待真实崩溃数据）、渲染像素 diff 回归、宋体字族映射、中→英（西文断字）、表体翻译转正、GUI。
