# 里程碑设计（V1）

> 切法：walking skeleton——先打通极窄端到端，逐里程碑加宽。每个里程碑以**语料上可验证的断言**收口，不以"模块写完"收口。
> 决策依据见 `CONTEXT.md` 与 `docs/adr/`；日期 2026-08-21。
>
> 资产说明：M0–M3 开发期模型/字体经"自备路径"逃生门手动放置；自动下载机制到 M4 才建。

## M0 · 风险探测（不交付功能）

三个实验对应三大技术风险，任何一个翻车都在最便宜的时刻翻。每个实验产出一页结论（成 / 败 / 替代方案），失败即触发对应 ADR 复议。

| # | 实验 | 验证的风险 |
|---|---|---|
| 1 | PP-DocLayoutV3 ONNX 经 ort CPU 跑通；在 `02_two_column.pdf` 渲染图上验证 `[M,7]` 第 7 列是否为阅读顺序（ADR-0002 遗留） | 模型可用性 + 阅读顺序红利是否成立（决策 #14 的兜底开关） |
| 2 | lopdf 原始 content stream 字节 ⇄ pdfium-render text page 对齐 PoC：同一页上自写 tokenizer 的字符定位与 PDFium 结果交叉核对 | 操作符走查可行性（ADR-0006 的核心假设） |
| 3 | 增量写回 PoC：lopdf 改写一页 content stream + 追加一个字体对象，输出 PDF 在主流阅读器中有效 | 增量改写模型（ADR-0003 §2） |

**收口断言**：三份实验结论文档齐备，无未决风险遗留。

## M1 · 最小端到端

- workspace 拆分（`mimus-core` + `mimus`）；IL 定义（ADR-0007）+ serde JSON 序列化。
- 固定 pass 链贯通：Parse → ScanDetect(拒绝) → Layout → ParagraphFind → Typeset → FontEmbed → Write（StylesAndFormulas/ExtractTerms/Translate 留桩）。
- `Translator` trait + `none` 后端；Noto Sans SC 嵌入 + subsetter 子集化。
- 测试网与 CI 同步建立：insta IL 快照（逐 pass）、全语料零 panic、CI 绿才合并。

**收口断言**：`mimus translate corpus/01_basic_text.pdf`（none 后端）产出有效 PDF，视觉上与原件等价（文字经解析-排版往返不劣化）；`16/17`（扫描件）按分类退出码拒绝；畸形组（21–23）fail-fast 不 panic。

## M2 · 真翻译

- `openai` 后端（endpoint+key+model 三层配置）；占位符协议 + 守恒断言；StylesAndFormulas pass（公式/富文本识别）。
- ExtractTerms pass（自动术语提取）+ `--glossary`/`--dump-glossary`/`--no-auto-terms`。
- redb 翻译缓存（键含 prompt 版本 + 术语指纹）；段落级并发（默认 4）+ 指数退避重试 3 次。
- 三层降级全链贯通（段→页→文档）+ 结束汇总 + `--strict`。

**收口断言**：合成语料非畸形组端到端全绿；占位符守恒 100%（违者必已降级且计入汇总）；同文档重跑第二次 API 调用数为 0（缓存命中）。

## M3 · 质量攻坚

- 启发式调参主战场：段落识别、fallback_line、layout 归属优先级、排版 scale 搜索——以真实文档反馈驱动。
- 反向语料：分析 BabelDOC 354 except / 12 修复函数 → 失效模式清单 → 扩展 `gen_pathological.py`；每修一个真实翻车文档先补语料。
- 真实语料 checklist：arXiv 按排版引擎分层（pdfTeX / XeTeX / LuaTeX / Word 导出）约 20 份。
- `--bilingual` 交替页（含书签/内链页目标重映射）；几何断言进 CI（译文框不越界、不压图/公式）。

**收口断言**：`18_kitchen_sink.pdf`、双栏、公式语料排版可读；真实语料 20 份零崩溃、人工 checklist 通过；反向语料首批入 repo 并全绿。

## M4 · 发布

- 资产机制：清单（名称→URL+sha256）、运行时下载、镜像可配、`assets pull` 预取。
- release archive：二进制 + libpdfium + 许可声明（MIT / PDFium BSD / 模型 Apache-2.0）；macOS arm64/x64 + Linux x64 + Windows x64。
- README / 使用文档 / 术语校对工作流说明。

**收口断言**：在一台干净机器上，从 GitHub Release 下载 archive → 解压 → 首跑自动拉资产 → 完成一篇真实 arXiv 论文的翻译。

## V2 展望（不承诺，触发条件另行立项）

扫描件/OCR 路径（PP-OCRv6，rebuild writer）、子进程隔离（待真实崩溃数据）、渲染像素 diff 回归、宋体字族映射、中→英（西文断字）、表体翻译转正、GUI。
