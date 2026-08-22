# 实验 4：firecrawl-pdfium 后端资格验证

> 日期：2026-08-23
> 问题：`firecrawl-pdfium` 能否替代 ADR-0006 当前的 `pdfium-render`，在
> `PdfInspector` / `Rasterizer` trait 后承担字符度量、文本交叉校验和光栅化？
> **结论：B．补齐列明的上游改动后采用。当前不得替换。**

## 0. 结论先行

固定在 `1a4c91d0c5f80c0da779088ba241bf1e45271cd5` 的
`firecrawl-pdfium`，其已经暴露的文本与渲染路径，在同一 PDFium、同一配置下与
`pdfium-render` 0.9.1 对拍完全一致；单线程性能也没有超过 25% 的回退门槛。它的进程级
全局 mutex 是 sound 的，但 4 到 8 worker 已平台化，证明“线程安全”不等于 PDFium 调用能
并行。

阻止直接替换的是能力表面，而不是已经观察到的值差异：mimus 必需的诊断、字体和对象来源
API 虽然存在于三档 PDFium dylib，却没有进入该 revision 的 `sys::Bindings`，safe API 更没有
暴露。crate 宣称的 raw escape hatch 因而不能调用这些 symbol；`PdfPage` / text page / page
object / font 的原始 handle 也不公开。实验中的 `FPDFText_IsGenerated` probe 是独立
`libloading` 程序，不是 crate 能力。

最小上游闭包是：

1. 把下表列出的 text/font/page-object symbol 与相关类型加入 `sys::Bindings`；
2. 为三值诊断提供保留 `1 / 0 / -1` 的安全类型，严禁把 `-1` 当成 `false`；
3. 提供 text object → font、页面对象枚举、Form 递归和字符来源路径的 owned safe API；若不
   做 owned 快照，则至少提供持有全局 mutex、限制 handle 生命周期的 scoped raw callback；
4. 为 glyph path 补齐 segment 枚举，并对所有 borrowed handle 做所有权和失效边界封装。

这不推翻 ADR-0006。当前继续使用 `pdfium-render`；只有上述能力进入固定的上游 revision 并重跑
本实验后，才执行 trait 后端替换。

## 1. 前置与范围

开始实验前确认：

- PR #45 合并为 `5139e45f3f32cd28d14e75a679d6306d95823912`，CI
  `fmt · clippy · test` 成功；
- PR #46 合并为 `867ca9852925c18a41b3d7210b1e3f215cab5995`，同一 CI 成功；
- 开始实验时，分支基点与 `origin/master` 均为
  `867ca9852925c18a41b3d7210b1e3f215cab5995`；
- 本地只读 fork 恰为 `1a4c91d0c5f80c0da779088ba241bf1e45271cd5`，工作树未改；
- 没有修改 ADR-0006、生产 crate、生产依赖或本地 fork。

实验 workspace 位于 `experiments/experiment-4-pdfium-qualification/`。reference 与 candidate
是两个独立程序，绝不在同一进程初始化 PDFium；所有 dylib 路径必须显式传入。原始结果、
PDFium 二进制、真实 PDF 和 probe fixture 均留在 `.context/experiment-4/`。

## 2. 环境与固定版本

| 项 | 值 |
|---|---|
| 主机 | MacBook Air (Mac17,3)，Apple M5，24 GB，arm64 |
| OS | macOS 26.5.1 (25F80) |
| Rust | rustc 1.98.0；workspace `rust-version = 1.77` |
| reference | `pdfium-render = 0.9.1`，与 M0 实验 2 精确一致 |
| candidate | `firecrawl-pdfium` Git revision `1a4c91d0c5f80c0da779088ba241bf1e45271cd5` |
| PDF 工具 | qpdf 12.4.0；Poppler `pdfinfo` 26.08.0；MuPDF 1.28.2 |

| PDFium 档位 | 来源 | archive SHA-256 | dylib SHA-256 |
|---|---|---|---|
| chromium/7988 | candidate revision 的 `pdfium.lock.json` / bblanchon mac-arm64 | `2229b8a6ffa0fb1634aa78886ed5425a10a8a0bd01762f7f0c9244081d86a921` | `fbdec47c3f2eaa80705ed25cf8bed5ac420998ba0f3e786d4d297b6238749064` |
| chromium/8009 | bblanchon `chromium/8009/pdfium-mac-arm64.tgz` | `b1f2f17c7432a9942514dda5094ee9822c743bdfd07e7187725efbd34fde941f` | `cfab7b27942132aea1a1ff7ff42ce970c39f7d928c1fc317ea99d3bfa3a43d0c` |
| API 7763（可选） | M0 实验 2 原 dylib | 不适用 | `cb8e259f914dda33f8930751e9a70afd3168893a569f7e59d34d29c4bc5701c3` |

8009 archive URL：
`https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8009/pdfium-mac-arm64.tgz`。
三档 dylib 都导出本节能力矩阵使用的 23 个 symbol。

## 3. 实验装置与比较合同

runner 输出 `schema_version = 1` JSON。每页标准化字段为：PDF SHA-256、1-based 页号、页面
尺寸、`Rotate`、完整 UTF-16 解码文本、逐字符 Unicode/code、baseline origin、tight box、
loose box，以及在 1 point → 1 pixel、白底、无 annotation/form-field 下的 RGBA8 尺寸和
SHA-256。双方都先请求 BGRA，再按各 crate 的公开 API 归一化为 RGBA。

“完整文本”按逐字符 `FPDFText_GetUnicode` code 序列做 UTF-16 解码，两端共用同一个纯函数；
不能由逐个 `Option<char>` 拼接，否则会丢失非 BMP 语义。也不能混用 wrapper 的便捷全文 API：
真实论文中的断词伪字符让 `GetUnicode` / `GetBoundedText` 返回 `U+0002`，而 `GetText` 返回
`U+FFFE`。这三个 API 的规范化差异不是后端差异。正式口径选择逐字符视图，因为它与字符数、
index 和几何位于同一 PDFium 索引域；两版错误装置结果留在 `.context`，不纳入结论。

比较规则：

- origin/tight/loose box 的绝对差不超过 `0.001 pt`；
- 文本、字符数、Unicode/code、页面属性和 RGBA8 hash 精确相等；
- 58 份合法 fixture 的任何无法解释差异都阻止 A；
- 16 份 malformed fixture 各自独立进程、30 秒超时；恢复分类差异可作证据，但 crash、hang
  或协议破坏不允许；
- matrix 每份完成后原子汇总 checkpoint；batch 长跑每 job 写独立原子 sidecar，正常结束才汇总，
  可在中断后续跑且不会产生累计 JSON 的 O(n^2) I/O。

## 4. 正确性与 raw probe

三档 PDFium 上共运行 `74 × 3 = 222` 次 reference/candidate 对拍：

| 输入 | 每档 | 三档合计 | equal | different / crash / timeout / protocol |
|---|---:|---:|---:|---:|
| 合法 Corpus v1 | 58 | 174 | 174 | 0 |
| malformed | 16 | 48 | 48 | 0 |
| 合计 | 74 | 222 | 222 | 0 |

`FPDFText_IsGenerated` 的隔离 probe 使用固定 revision 中的 `hello_world.pdf`，PDF SHA-256
为 `1e06a6e12329f0c3760680a70baea126e040e66e0bf34d3b14a71863096ee1e3`。三档结果一致：普通
`H` 返回 `0`，PDFium 在 index 13 合成的 CR 返回 `1`，越界返回 `-1`。这直接证明安全封装
不能写成 `result != 0` 或 `result == 1` 后丢弃错误上下文。

## 5. 能力审计

### 5.1 如何读表

下表“crate 暴露”同时审计 safe 与 raw：`未绑定` 表示 safe API 没有该字段或对象，而且
`firecrawl_pdfium::sys::Bindings` 也没有函数指针。仅有 dylib symbol 不能改变这一结论。
`7988 / 8009` 均为实际 `nm` 检查；可选 7763 也全部存在。

最小改动代号：

- **T1**：加入 `sys::Bindings`，在 eager `PageText` 提取时存成 owned 字段并保留错误态；
- **F1**：加入 bindings；从字符的 text object 取得 borrowed font，在锁内完成度量/数据读取，
  对外只返回 owned snapshot；
- **O1**：加入 bindings 与 page-object 类型常量，在持锁的 scoped traversal 中枚举页面、递归
  Form，并把 char → text object → object path 固化为 owned source mapping；
- **G1**：在 F1 上再绑定 glyph-path segment count/get 和 segment accessor，输出 owned path。

### 5.2 字符保真与诊断

| API | crate 暴露 | 7988 / 8009 symbol | PDFium 错误语义 | mimus 必需性 | 最小改动 |
|---|---|---|---|---|---|
| `FPDFText_IsGenerated` | 未绑定 | 是 / 是 | `1` 是，`0` 否，`-1` 错误 | 必需：排除 PDFium 合成字符 | T1，三值 enum |
| `FPDFText_HasUnicodeMapError` | 未绑定 | 是 / 是 | `1` 有错，`0` 无已知错，`-1` 调用错误 | 必需：CMap 交叉校验 | T1，三值 enum |
| `FPDFText_IsHyphen` | 未绑定 | 是 / 是 | `1` 是，`0` 否，`-1` 错误 | 必需：断词/段落重建 | T1，三值 enum |
| `FPDFText_GetFontSize` | 未绑定 | 是 / 是 | 返回 point；头文件未给独立错误哨兵 | 必需：度量盒与样式 | T1，先验证 index |
| `FPDFText_GetFontInfo` | 未绑定 | 是 / 是 | 成功返回含 NUL 的 UTF-8 长度，失败 `0` | 必需：字体身份与 flags | T1，两阶段 buffer |
| `FPDFText_GetMatrix` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 out 不变 | 必需：旋转/镜像/斜切策略 | T1，`Result<Matrix>` |
| `FPDFText_GetCharAngle` | 未绑定 | 是 / 是 | 成功弧度 `>= 0`，错误 `-1` | 非独立必需：Matrix 可推导；可交叉校验 | T1，保留 `-1` |
| `FPDFText_GetFillColor` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 RGBA 不变 | 必需：原样样式识别 | T1，`Result<Rgba>` |
| `FPDFText_GetStrokeColor` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 RGBA 不变 | 必需：原样样式识别 | T1，`Result<Rgba>` |

当前 safe `PageChar` 只有 Unicode/code、origin、tight/loose box。`PageText` 是 eager owned
结果，提取完即关闭 `FPDF_TEXTPAGE`；事后无法借 raw API补查。

真实样本还证实 `PageText::text()` 与 `PageText::chars()` 不能按字符串位置一一对应：前者经
`FPDFText_GetText` 在断词处给出 `U+FFFE`，后者经 `FPDFText_GetUnicode` 给出 `U+0002`。
这符合 crate 对两个视图索引不同的文档，但也意味着 mimus 的字符来源映射必须以 text-page
character index 为主，并依赖 `FPDFText_IsHyphen` 判断断词，不能从完整字符串反推字符索引。

### 5.3 字体度量与数据

| API | crate 暴露 | 7988 / 8009 symbol | PDFium 错误语义 | mimus 必需性 | 最小改动 |
|---|---|---|---|---|---|
| `FPDFFont_GetBaseFontName` | 未绑定 | 是 / 是 | 成功返回含 NUL 的长度，错误 `0` | 必需：字体身份/溯源 | F1，两阶段 buffer |
| `FPDFFont_GetAscent` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 out 不变 | 必需：度量盒 | F1 |
| `FPDFFont_GetDescent` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 out 不变 | 必需：度量盒 | F1 |
| `FPDFFont_GetGlyphWidth` | 未绑定 | 是 / 是 | 成功 true；失败 false 且 out 不变 | 必需：操作符走查度量 | F1 |
| `FPDFFont_GetGlyphPath` | 未绑定 | 是 / 是 | 成功 borrowed path handle，失败 NULL | 可选：tight box 已覆盖 V1；Type3/诊断有价值 | G1 |
| `FPDFFont_GetFontData` | 未绑定 | 是 / 是 | 必需参数非空才 true；返回所需长度；非嵌入字体返回 substitute 数据 | 必需：embedded cmap/fallback | F1，并同时报告 embedded 状态 |
| `FPDFFont_GetIsEmbedded` | 未绑定 | 是 / 是 | `1` 嵌入，`0` 未嵌入，`-1` 错误 | 必需：禁止把 substitute 当原字体 | F1，三值 enum |

这些函数都需要 `FPDF_FONT`。当前 crate 没有 text object → font 桥，因此仅把函数指针加入
`Bindings` 仍不够。

### 5.4 对象来源与依赖桥

| API / 能力 | crate 暴露 | 7988 / 8009 symbol | PDFium 错误/所有权语义 | mimus 必需性 | 最小改动 |
|---|---|---|---|---|---|
| `FPDFText_GetTextObject` | 未绑定 | 是 / 是 | 错误 NULL；返回 borrowed text object | 必需：字符来源桥 | O1 |
| `FPDFTextObj_GetFont` | 未绑定 | 是 / 是 | font 由 text object 持有，不转移所有权 | 必需：进入 F1 的桥 | O1 + F1 |
| `FPDFPage_CountObjects` | 未绑定 | 是 / 是 | 返回 count；头文件未定义无效 handle 的独立哨兵 | 必需：页面对象枚举 | O1 |
| `FPDFPage_GetObject` | 未绑定 | 是 / 是 | 失败 NULL；返回 borrowed page object | 必需：页面对象枚举 | O1 |
| `FPDFPageObj_GetType` | 未绑定 | 是 / 是 | 成功为 `FPDF_PAGEOBJ_*`，错误 UNKNOWN | 必需：识别 text/Form | O1 |
| `FPDFFormObj_CountObjects` | 未绑定 | 是 / 是 | 成功 count，错误 `-1` | 必需：Form 递归 | O1 |
| `FPDFFormObj_GetObject` | 未绑定 | 是 / 是 | 错误 NULL；返回 borrowed page object | 必需：Form 递归 | O1 |
| char → Form/XObject 路径 | 无现成 PDFium 单函数 | 不适用 | 需递归枚举后按 object identity 建 owned path | 必需：`xobj_id` 与操作符对齐 | O1 的 crate 级算法 |

`Pdfium::raw()` 虽公开，但 raw 方法仍受进程全局 mutex 合同约束；当前 `PdfPage` handle 是
private，text page 又被 eager 提取关闭。让调用方自行 `libloading` 会绕过 crate 的初始化、
全局锁和生命周期不变量，不是可接受的生产 escape hatch。

## 6. 真实 arXiv 烟测

真实集为 20 份公开 arXiv PDF、707 页，覆盖多种 Producer/Creator：

| 来源族 | 文档 | 页数 |
|---|---:|---:|
| pdfTeX 1.40.17/21/25 | 14 | 651 |
| xdvipdfmx | 1 | 7 |
| Microsoft Word 2010/2013 | 2 | 19 |
| Acrobat Distiller | 1 | 15 |
| dvips + Ghostscript | 1 | 4 |
| WPS 文字 | 1 | 11 |

三档 PDFium 各跑 20 次 reference/candidate 对拍，共 60 次、2,121 个输入页版本，结果
`60 equal / 0 different / 0 crash / 0 timeout / 0 protocol error`。

首轮 pilot 在 API 7763 的 `arxiv-1210.5898` 上曾报告 11 个 page-text 差异；逐字段检查确认
字符数、每字符 Unicode/code、三组几何与 RGBA8 hash 全部相等，唯一差异是断词伪字符：
reference 的 `GetUnicode` 重建视图为 `U+0002`，candidate 的 `GetText` 视图为 `U+FFFE`。
按 §3 统一到 character-index 视图后，该样本三档全部相等，完整 60 项矩阵也全等。这是本实验
发现并修正的 comparator 口径错误，不计作 crate 正确性失败。

PDF 本体和逐页 JSON 不提交。去路径机器摘要记录每份 URL、Producer/Creator、页数与
SHA-256，使样本可复取并能发现 arXiv 替换文件导致的哈希漂移。

## 7. 性能与全局 mutex

release 构建对合法 Corpus（58 份、61 页）先 warm-up 一轮，再测量 5 轮；benchmark 关闭
durable checkpoint，并以 runner 报告的 warm-up 后 `elapsed_us` 为吞吐分母。全部 27 个组合
成功。下表是 pages/s；candidate 括号为相对同档 candidate 1 worker 的倍率。

| PDFium | reference 1T | candidate 1T | candidate 2T | candidate 4T | candidate 8T | candidate 8P 对照 |
|---|---:|---:|---:|---:|---:|---:|
| API 7763 | 470.57 | 445.41 (1.00x) | 834.21 (1.87x) | 1,269.98 (2.85x) | 1,291.52 (2.90x) | 699.96 |
| chromium/7988 | 467.16 | 444.62 (1.00x) | 813.27 (1.83x) | 1,278.24 (2.87x) | 1,380.33 (3.10x) | 700.79 |
| chromium/8009 | 471.41 | 440.51 (1.00x) | 819.30 (1.86x) | 1,289.20 (2.93x) | 1,342.42 (3.05x) | 702.60 |

candidate 1T 相对 reference 分别为 `-5.34% / -4.82% / -6.55%`，远低于 25% 回退门槛。
2T/4T 的收益证明 PDF 读取、RGBA 归一化、哈希和 owned-data 处理能在锁外并行；但 4T→8T
只增加 `1.70% / 7.99% / 4.13%`，文档 p95 从 1T 的 `2.47–2.73 ms` 上升到 8T 的
`8.97–10.14 ms`。这正是 crate 进程级全局 mutex 在高线程数下的排队成本。

8 个独立进程只达到约 `700 pages/s`（candidate 1T 的 1.57–1.60x），反而低于 4T/8T；本组
短小 fixture 上，多份 PDFium 初始化/缓存和 CPU/内存竞争盖过了绕开 mutex 的收益。它只作为
隔离架构的吞吐对照，不改变 V1 单进程决策。每个组合的 p50/p95、peak RSS 与采样序列保存在
机器摘要和 `.context` 原始结果中。

## 8. 常驻进程稳定性

6 个 backend/PDFium 组合并行启动，每个组合在同一进程中完成 200 个合法 Corpus 轮次；
全局上限 8 小时，本次均先达到轮数上限。合计 69,600 个 fixture job、73,200 页，0 job 失败、
0 内容 hash 漂移、0 timeout、0 提前退出。

| PDFium | backend | 完成轮次 | peak RSS | 后半程 vs 前半程 | 调查 |
|---|---|---:|---:|---:|---|
| API 7763 | reference | 200 | 110.88 MiB | +3.65% | 否 |
| API 7763 | candidate | 200 | 115.00 MiB | +3.60% | 否 |
| chromium/7988 | reference | 200 | 111.02 MiB | +2.47% | 否 |
| chromium/7988 | candidate | 200 | 114.31 MiB | +4.01% | 否 |
| chromium/8009 | reference | 200 | 110.75 MiB | +3.63% | 否 |
| chromium/8009 | candidate | 200 | 114.95 MiB | +3.24% | 否 |

所有增长都远低于“后半程高于前半程 20%”门槛；首尾增长也未满足“至少 10 个样本、95%
不下降且增长超过 5%”的联合条件。candidate 相对 reference 多约 3.3–4.2 MiB 常驻内存，但没有
观察到随轮次累积的泄漏信号。逐秒 RSS 样本留在 `.context`，提交版摘要只保留统计量。

## 9. 判定与后续

| 判定 | 是否满足 | 原因 |
|---|---|---|
| A．可直接采用 | 否 | 必需 API 未进入 safe/sys 表面 |
| B．补齐列明的上游改动后采用 | **是** | 已暴露路径正确、性能合格；缺口准确且可封闭 |
| C．保持 pdfium-render | 否（作为永久结论） | 没有发现已暴露路径的正确性或性能硬失败；当前暂时仍保持 |

后续必须先在 `firecrawl-pdfium` 上游完成 T1/F1/O1（G1 可按 V1 是否使用 glyph path 延后），
固定新的 Git revision，然后重跑 7988、当时最新 PDFium、Corpus/malformed、真实烟测、性能与
200 轮长跑。完成前不改 ADR-0006，也不改生产依赖。

复现命令见实验 [`README.md`](../experiments/experiment-4-pdfium-qualification/README.md)。
