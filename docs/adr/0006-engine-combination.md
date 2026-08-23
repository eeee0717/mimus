# ADR-0006 · PDF 引擎组合：lopdf + PDFium（trait 边界）+ 自写操作符走查

- 状态：已接受（2026-08-21）；M0 实验验证通过（2026-08-23）
- 决策层级：难逆（V1 最重的架构决策，解析层全部建立其上）

## 背景

原生路径需要四种能力：① 对象树 + 增量写回；② 字符级提取与字体度量；③ 原始 content stream 字节（passthrough 前提，PDFium 公开 API 拿不到——研究报告缺口①）；④ 页面光栅化（版面模型输入）。

2026-08-21 查证：pdfium-render 0.9.3 动态链接成熟（bblanchon 预编译覆盖全平台）、无预编译静态库；PDFium BSD-3-Clause，崩溃即进程 abort；hayro 0.7.1（纯 Rust 渲染器）filter 全覆盖、Type3/嵌入 CID 已实现，但自述 experimental、无字符级提取 API、不支持非嵌入 CID 字体。

## 决策

1. `lopdf` 承担 ①③；`pdfium-render` 承担 ②④，动态库随 release archive 分发（ADR-0005 档位允许）。
2. ②④ 隐藏在显式 trait 边界（`PdfInspector` / `Rasterizer`）之后：PDFium 是实现细节而非架构承诺，未来 hayro 成熟或 PDFium 触顶时可整体换后端。
3. 在 lopdf 提供的原始 content stream 字节上**自写操作符走查**（tokenizer + 图形状态机 + CTM 栈 + 文本定位）：字符坐标由自写解释器计算，不理解的操作符按 passthrough 策略原样保存。字形宽度、字体解码、缺字体 fallback 等度量难题委托 PDFium；PDFium text page 结果用作自写走查的交叉校验。
4. PDFium 调用集中在 trait 实现模块内，V1 单进程运行；子进程隔离推 V2（届时以真实崩溃数据决策）。

这是 BabelDOC 架构（自写解释器跑在 MuPDF 之上）的 Rust 镜像，MuPDF（AGPL）换成 PDFium（BSD）。

## 实验验证

M0 实验 2 在规范输入上证明了“lopdf 原始字节 + 自写走查 + PDFium 交叉校验”可行：合法比较 fixture 的字符序列与 baseline 满足 `0.001 pt` 合同，PDFium origin 最大绝对差为 `4.52e-6 pt`；畸形输入均有界终止。分歧统一按以下顺序裁定：

1. PDF 规范 + hand-written manifest + 与生成器独立的结构、字体、几何推导是事实层；
2. 至少一个独立 parser/renderer trace 验证推导确实落在输入 PDF 上；
3. PDFium 是交叉证据而非唯一 oracle；不一致时记录 engine differential，不反向修改 manifest；
4. 规范允许多种恢复时选择不扩散、可报告、可有界停止的路径，无法可靠恢复则按既定粒度降级。

M0 实验 3 同时验证了 lopdf 增量路径：输入完整字节可作为输出前缀，新对象只追加，共享资源可 copy-on-write，未修改对象与文档结构保持不变，失败可在原子发布前收住。

2026-08-23 的 wrapper 资格实验对比 pdfium-render 0.9.1 与 firecrawl-pdfium `1a4c91d`：三档 PDFium 上 Corpus `222/222`、20 份 arXiv `60/60` 对拍一致，69,600 个长跑 job 无失败或 hash 漂移，单线程回退 `4.82%–6.55%`。但 firecrawl-pdfium 尚未暴露 mimus 必需的 T1 字符诊断、F1 字体 owned snapshot 与 O1 对象来源映射，因此结论为 **B：补齐上游后采用**。这些能力进入固定 revision 并重跑资格矩阵前，当前实现继续使用 pdfium-render；本 ADR 不变。

证据见 [M0 实验 2](../04-m0-experiment-2.md)、[M0 实验 3](../04-m0-experiment-3.md) 与 [PDFium wrapper 资格报告](../05-pdfium-backend-qualification.md)。

## 后果

- passthrough 保住：保真度上限不被 PDFium 建模完整度锁死。
- 自写走查是 V1 最大单体工作量（报告估 ~6k 行），但字体度量的"最容易写错的部分"不在其中。
- 接受 V1 的 PDFium abort 风险（代价 = 重跑一次命令），崩溃语料驱动修复。
- release archive 含 libpdfium 动态库 + BSD 许可声明。
- wrapper 不是架构承诺，但替换后端必须先满足 trait 所需能力并通过固定版本的资格矩阵；行为对拍一致本身不足以触发切换。
