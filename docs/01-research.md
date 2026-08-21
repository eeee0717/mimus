# PDF 翻译工具 · 初步调研报告

> 状态：调研，**不含架构决策**
> 日期：2026-08-21
> 标注约定：`[实测]` 读源码/头文件/模型元数据得到；`[查证]` 官方文档或仓库；`[推断]` 我的判断，需验证

---

## 0. 摘要

目标是做一个保留版面的 PDF 翻译 CLI，用 Rust，并把扫描件 OCR 作为一等输入路径。

三条主要结论：

1. **参考实现 BabelDOC 的真实规模是 38k 行手写逻辑**，不是表面上的 78k。差额是 vendored 的 pdfminer 和大量字体数据表，后者在 Rust 里基本不用重写。
2. **最大的成本不在任何单一模块**，而在两处：段落/公式识别的启发式调参，以及畸形 PDF 的长尾。前者代码两周能写完、效果几个月才追得平；后者在 BabelDOC 里表现为 354 个异常捕获和 12 个专门的修复函数。
3. **扫描件路径能绕开绝大部分难点**，同时是 BabelDOC 明确不支持的能力。作为第一个里程碑在成本和差异化上都成立。

---

## 1. 背景

### 1.1 问题定义

输入一份 PDF，输出译文版本，要求版面、公式、图表、字距尽量与原件一致。

这不是"提取文本→翻译→重新排版"。PDF 的 content stream 只是一串绘图指令：

```
BT /F1 11 Tf 72 700 Td (This paper describes how a PDF) Tj ET
```

里面没有段落、没有阅读顺序、没有"这块是标题那块是脚注"。而翻译需要这些——否则会逐行断句、翻译页眉、把公式当正文。

### 1.2 与现有方案的差异点

| | BabelDOC | 本项目意图 |
|---|---|---|
| 扫描件 | 抛 `ScannedPDFError` 拒绝 | 一等路径 |
| 版面模型 | DocLayout-YOLO | PP-DocLayout (RT-DETR) |
| 分发 | Python + onnxruntime/PyMuPDF wheel | 单二进制 |
| 许可 | AGPL-3.0 | 待定 |

---

## 2. 参考实现拆解：BabelDOC

分析对象：`funstory-ai/BabelDOC` v0.6.4 的一个下游分支。

### 2.1 流水线

整体是 **frontend → midend(N passes) → backend**，全部围绕一个中间表示做变换。

`[实测]` 阶段与相对耗时权重（来自 `high_level.py` 的 `TRANSLATE_STAGES`）：

| # | 阶段 | 权重 | 位置 |
|---|---|---:|---|
| 1 | Parse PDF → IR | 14.12 | `new_parser/native_parse.py` |
| 2 | DetectScannedFile | 2.45 | `midend/detect_scanned_file.py` |
| 3 | Parse Page Layout | 14.03 | `midend/layout_parser.py` |
| 4 | Parse Table | 1.00 | `midend/table_parser.py` |
| 5 | Parse Paragraphs | 6.26 | `midend/paragraph_finder.py` |
| 6 | Parse Formulas and Styles | 1.66 | `midend/styles_and_formulas.py` |
| 7 | Extract Terms | 30.00 | `midend/automatic_term_extractor.py` |
| 8 | Translate Paragraphs | 46.96 | `midend/il_translator*.py` |
| 9 | Typesetting | 4.71 | `midend/typesetting.py` |
| 10 | Add Fonts | 0.61 | `utils/fontmap.py` |
| 11 | Generate drawing instructions | 1.96 | `backend/pdf_creater.py` |
| 12 | Subset font | 0.92 | 同上（子进程） |
| 13 | Save PDF | 6.34 | 同上（带超时） |

要点：

- **版面分析只占 286 行代码**（`docvision/doclayout.py`），却占 14% 的耗时。它的产出仅仅是若干个带 label 的框。
- **排版策略**是 scale 递减搜索：1.0 起，>0.6 时步长 0.05、之后 0.1；放不下就尝试向下扩展框、再向右扩展；仍不行则放宽英文断行规则；兜底 0.1。CJK 行距 1.5，西文 1.3。
- **段落归属**用 R-tree 建 layout 空间索引，逐字符算 IoU，按一张约 70 项的 label 优先级表排序取第一个。不在文本 layout 内的字符**直接跳过不翻译**。

### 2.2 中间表示（IL）

`[实测]` schema 手写为 RelaxNG Compact（`il_version_1.rnc`，244 行），转 xsd 后用 xsdata 生成 dataclass（`il_version_1.py`，1371 行）。

粒度是**单字符**。实测一个单页 demo：

```
阶段 1 后:  pdf_character 191,  page_layout 0,  pdf_paragraph 0
阶段 3 后:  page_layout 6  (模型输出 2 + 字符聚类兜底 4)
阶段 5 后:  pdf_character 0,  pdf_paragraph 8   ← 字符被吸收进段落
```

单个字符的实际结构：

```json
{
  "char_unicode": "A",
  "box":         { "x": 72.0,  "y": 742.0, "x2": 83.552, "y2": 758.0 },
  "visual_bbox": { "box": { "x": 72.32, "y": 742.0, "x2": 83.232, "y2": 753.488 } },
  "pdf_style": {
    "font_id": "hebo", "font_size": 16.0,
    "graphic_state": { "passthrough_per_char_instruction": "" }
  },
  "advance": 11.552, "vertical": false, "xobj_id": 0,
  "render_order": 1, "sub_render_order": 0
}
```

三个设计值得注意：

- **双盒**：`box` 是字体度量盒（含 ascent/descent），`visual_bbox` 是墨迹盒。layout 归属用墨迹盒算 IoU 才准。
- **不理解就原样存**：`passthrough_per_char_instruction` 把原始 PDF 操作符存成字符串，输出时透传。这让 IL 不必覆盖 PDF 规范全集，只需覆盖**要修改的部分**。
- **段落是 union 容器**：`PdfParagraphComposition` 五选一（`PDFLine | PDFFormula | PDFSameStyleCharacters | PDFCharacter | PDFSameStyleUnicodeCharacters`），让"已有坐标的原文"和"还没排版的译文"能共存于同一段落。排版的任务就是把后者转成前者。

`[推断]` 这个 union 在 Python 里是"五个 Optional 字段只有一个非 None"，下游到处手写 `if ... elif ...` 分发，漏一个分支就是静默 bug。

### 2.3 代码规模实测

`[实测]`

```
全部 Python              :  77,983 行 / 196 文件
  ├ vendored pdfminer    :  19,900        ← 已被 new_parser 取代，主链路不走
  │   ├ 数据表           :   9,073
  │   └ 逻辑             :  10,827
  └ 自有                 :  58,083
      ├ 数据表/生成代码   :  19,786
      └ 手写业务逻辑      :  38,297        ← 真正的参考量
```

按模块：

| 模块 | 文件 | 行数 |
|---|---:|---:|
| pdfminer（vendored） | 32 | 19,900 |
| new_parser（PDF 解释器） | 66 | 16,866 |
| babelpdf（字体/CMap/CID） | 7 | 7,656 |
| document_il/midend | 12 | 7,603 |
| 其余（main/config/IL dataclass） | 21 | 7,003 |
| document_il/frontend | 5 | 3,694 |
| document_il/utils | 13 | 3,565 |
| docvision（视觉模型） | 12 | 3,439 |
| tools | 14 | 2,741 |
| assets | 2 | 2,574 |
| document_il/backend | 2 | 1,750 |
| translator | 3 | 576 |
| 其他 | 7 | 616 |

那 19,786 行"数据表/生成代码"具体是：base14 字体度量 3,336、Windows core 字体 2,618、fontmetrics 4,464、glyphlist 4,370、encoding 表 1,307、资产清单 1,472、IL dataclass 1,371、其余 848。`[推断]` 这些在 Rust 里大部分由 PDFium / `ttf-parser` 内置，不需要重写。

### 2.4 工程现状与教训

`[实测]`

- **项目历史**：2024-12-13 起，1834 commits，22 位贡献者，约 20 个月
- **累计变更**：新增 175,809 行 / 删除 81,700 行，最终留下 77,983 行 —— **写了约 2.25 倍的代码才收敛**（`new_parser` 取代 `pdfminer` 是最大的一次重写）
- **异常捕获**：354 个 `except`（不含 pdfminer）⚠️ **后经 AST 复核更正为 263 个**——354 是 grep 匹配 `except` 子串的行数（含注释与字符串），263 才是实际的异常处理器条数。详见 `docs/03-corpus-requirements.md` §3.0。下文出现的 354 同此。
- **专门的修复函数** 12 个：`fix_null_page_content` / `fix_null_xref` / `fix_filter` / `fix_media_box` / `fix_cmap` / `reproduce_cmap` / `safe_save`（save 失败降级 ez_save）/ `open_pdf_with_save_fallback` / `save_pdf_with_same_path_fallback` / `rebuild_pdf_by_inserting_pages` / `check_cid_char` / `migrate_toc`
- **两个防崩措施**：`subset_fonts_in_subprocess()`（字体子集化丢子进程隔离）、`save_pdf_with_timeout()`（保存加超时）
- **测试**：933 行，占代码量 1.2%，且**零 PDF 处理测试**

测试文件全部是资产下载、进度协议、CLI 参数解析。CI 只跑一个 `examples/ci/test.pdf`，实测内容为「1 页、20 个字符、0 图、0 绘图」，且命令带 `--only-parse-generate-pdf`（跳过整个翻译链路）。

`[推断]` 没有回归测试网 → 只能靠防御性代码兜底 → 354 个 except 和 12 个修复函数。这是可以避免的：IL 可序列化，天然适合快照测试。

> 附注：`examples/*.xml` 不是测试用例。那是一个叫 DPML（`urn:ns:yadt:dpml`）的格式草案，全代码库无任何解析代码，从未实现。

---

## 3. 关键组件调研

### 3.1 PDF 读取

`[实测]` BabelDOC 自己写了 content stream 解释器（`new_parser`，16,866 行），跑在 PyMuPDF 之上——MuPDF 提供 xref/对象/字形度量，操作符执行是自己的。

**PDFium 能力审计** `[查证]`（对照 `public/fpdf_text.h` 与 `public/fpdf_edit.h` 主干代码）

逐字段对照 BabelDOC 的 `PdfCharacter`：

| IL 字段 | PDFium API | 结论 |
|---|---|---|
| `char_unicode` | `FPDFText_GetUnicode` | 可得 |
| `box`（度量盒） | `FPDFText_GetLooseCharBox` | 可得，语义正好对应 |
| `visual_bbox`（墨迹盒） | `FPDFText_GetCharBox` | 可得，语义正好对应 |
| `font_size` | `FPDFText_GetFontSize` | 可得 |
| `font_id` | `FPDFText_GetFontInfo` / `GetTextObject`→`FPDFFont_GetBaseFontName` | 得到字体**名**与对象句柄，非资源字典 key |
| 字符 CTM | `FPDFText_GetMatrix` | 可得 |
| `advance` | 无直接 API | 需从相邻 `GetCharOrigin` 作差或 `FPDFFont_GetGlyphWidth` 推导 |
| `vertical` | — | **缺**，只有 `GetCharAngle`（弧度），CJK 竖排 WMode 拿不到 |
| `render_order` | text page 索引近似 | 精确 z-order 需另走 `FPDFPage_GetObject` |
| `xobj_id` | `FPDFText_GetTextObject` + `FPDFFormObj_*` | 半支持，见缺口③ |
| `passthrough` 指令 | — | **缺**，见缺口① |

PDFium 额外提供而 BabelDOC 靠自算的：`FPDFText_GetFillColor` / `GetStrokeColor`（每字符颜色）、`FPDFText_IsGenerated`（标记 PDFium 自己合成的空格换行）、`FPDFText_HasUnicodeMapError`（直接对应 `check_cid_char`）、`FPDFText_IsHyphen`。字体侧 `FPDFFont_GetAscent/GetDescent/GetGlyphWidth/GetGlyphPath/GetFontData/GetIsEmbedded` 齐全。

**三个缺口：**

① **拿不到原始操作符字节。** PDFium 把 content stream 解析进 `CPDF_PageObject` 后即丢弃 token 流，公开 API 无任何"给我原始字节"的入口。BabelDOC 那套 passthrough 策略在 PDFium 上做不了。只能拿到 PDFium 建模过的图形状态；soft mask、blend mode、shading pattern 作填充等覆盖不全。`[推断]` 保真度上限被 PDFium 的建模完整度锁死——这很可能就是 BabelDOC 在 MuPDF 之上仍自写解释器的原因。

② **写入端弱。** `FPDFText_LoadCidType2Font(doc, data, size, to_unicode_cmap, cid_to_gid_map, ...)` 存在（比预期好），但**没有字体子集化**、没有任意 CMap 构造，`FPDFPage_GenerateContent()` 是整页重生成。PDFium 是优秀的读者、平庸的写者。

③ **text page 是扁平化视图。** 按 PDFium 自己的启发式重排成阅读顺序并合成字符，与"按 content stream 原序、保留 XObject 嵌套"的模型不一致。`FPDFText_GetTextObject` 提供了 char index → page object 的桥，但重建 `xobj_id` 层级需自行递归 `FPDFPage_GetObject` + `FPDFFormObj_CountObjects/GetObject` 并与 text page 索引对齐，**无官方 API**。

`[推断]` 结论：PDFium 能顶替 MuPDF 在 BabelDOC 里的角色（几何 + 度量 + 光栅化），但自写操作符走查这一步逃不掉。它能省掉的是最容易写错的部分——Type3 字体矩阵、CID 宽度回退、缺字体时的 fallback 度量。

### 3.2 PDF 写回

两种模型，**不可互换**：

- **增量改写**：保留原 PDF 全部对象（图像、矢量图、注释、书签、表单域），只改 content stream + 追加字体对象。BabelDOC 走这条。原生 PDF 路径必需。
- **从零重建**：构建新文档。仅当原页本身就是一张位图时无损——即扫描件路径。

`[推断]` Rust 侧 `krilla` / `pdf-writer` 属于后者，`lopdf` 能做前者。若两条路径都要，需要两个 writer。

### 3.3 版面分析

**现状（DocLayout-YOLO）** `[实测]` 读模型元数据：

```
文件   doclayout_yolo_docstructbench_imgsz1024.onnx   (75,324,598 bytes)
stride 32
输入   images  [batch, 3, height, width]
输出   output0 [batch, N, anchors]   —— 导出时已内置 NMS，实际为 [N,6] = xyxy,conf,cls
类别   10: title, plain text, abandon, figure, figure_caption,
           table, table_caption, table_footnote, isolate_formula, formula_caption
```

预处理：长边缩到 1024、letterbox padding（值 114）、`/255` 归一化、`conf > 0.25` 过滤、反缩放回 PDF pt。macOS 上会把 input shape 固定成 `[1,3,1024,1024]` 以走 CoreML EP（源码注释称这样能让 658/681 个节点走 CoreML，否则仅 3/823）。

**候选（PP-DocLayout / RT-DETR）** `[查证]` 类别丰富得多（20+），含 `paragraph_title` / `abstract` / `reference` / `algorithm` / `formula_number` / `header` / `footer` / `seal` / `chart` / `aside_text` 等。

`[实测]` 有意思的是 BabelDOC 的 `layout_priority` 表里**已经有这些 label 名**，且 8 个 `rpc_doclayout*.py` 后端的类别名完全由服务端返回决定——说明这套设计本就假定 label 词表可替换。

**迁移需注意三点** `[推断]`：

1. RT-DETR 是 DETR 系，无 NMS；Paddle 的 ONNX 导出通常额外需要 `im_shape` 和 `scale_factor` 两个输入，输出为固定数量 query，列序是 `[cls, score, x0, y0, x1, y1]`——**cls 在前**，与 YOLO 相反。
2. 预处理不是 letterbox：默认 resize 到 800×800（不保宽高比），归一化用 ImageNet mean/std，不是 `/255`。
3. 必须走 `paddle2onnx`（非 Python 侧无 Paddle 推理绑定）。`grid_sample` 等算子对 ORT 版本与 EP 有要求，CoreML/DirectML 可能回落 CPU。

**一个易被忽略的机制** `[实测]`：`generate_fallback_line_layout_for_page()` 会把模型漏检区域的字符聚类成 `fallback_line` 伪 layout。没有它，漏检的段落**直接丢失不翻译**。注意扫描页没有字符，这个兜底失效——OCR 的检测框正好能顶上。

### 3.4 OCR

`[实测]` BabelDOC **没有 OCR**。三个易混淆项：

- `--ocr-workaround` 不是 OCR，是处理"已被别人 OCR 过"的 PDF（扫描图 + 不可见文字层）。行为：强制 `skip_scanned_detection` 与 `disable_rich_text_translate`；`paragraph_finder.py:296` 调 `add_text_fill_background()` 后**清空 `page.pdf_character`**；`pdf_creater.py:884` 仅在该模式渲染 `fill_background` 矩形（白底盖住原扫描文字）。
- `DetectScannedFile` 只检测不处理：用 skimage SSIM 比对"渲染含文字层 vs 不含"，命中直接抛 `ScannedPDFError`。
- `RapidOCRModel` 已退役，现为 41 行空桩，注释写明 "retired"。

**PP-OCRv6** `[查证]`

官方直接在 HF 发布 ONNX，无需自行转换：

```
PaddlePaddle/PP-OCRv6_{tiny,small,medium}_det_onnx
PaddlePaddle/PP-OCRv6_{tiny,small,medium}_rec_onnx
```

| 档位 | 参数量 | det Hmean | rec Acc |
|---|---:|---:|---:|
| tiny | 1.5 M | 80.6 | 73.5 |
| small | 7.7 M | 84.1 | 81.3 |
| medium | 34.5 M | 86.2 | 83.2 |

small / medium 支持 50 种语言。架构：PPLCNetV4 骨干（det/rec 共用）、RepLKFPN 检测颈、EncoderWithLightSVTR + CTCHead 识别（NRTRHead 仅训练期辅助，推理移除）；Stage 3/4 用非对称 stride (2,1) 只降高不降宽，再沿高度轴平均池化出 1-D 序列。

**真正的成本在 ONNX 图外的后处理** `[推断]`：

- 检测是 DBNet 系分割头，输出概率图不是框。需：阈值二值化 → 轮廓提取 → Vatti unclip 外扩 → 最小外接矩形 → 按 score 过滤。阈值和 unclip_ratio 建议直接照抄 PaddleOCR `DBPostProcess` 默认值起步。
- 识别是 CTC：argmax → 去重 → 去 blank → 查字典。工作量很小。

Rust 侧已有可参考实现 `[查证]`：`pure-onnx-ocr`（纯 Rust 重实现 DBNet + SVTR_HGNet，det/rec/geometry 分层清晰）、`kreuzberg-paddle-ocr`（ORT 后端）、`paddle-ocr-rs`、`paddleocr_rs_onnx`、`rust-paddle-ocr`（声称支持 v4/v5/v6 但用 MNN 后端）。**注意这些基本停在 v5 时代，v6 的输出张量形状需自行核对。**

**OCR 与 IL 的接口是架构问题，不是模型问题** `[推断]`。OCR 产出与 IL 需求差距很大：

| IL 字段 | PDF 解析 | OCR |
|---|---|---|
| `char_unicode` | 逐字符 | 整行文本串 |
| bbox | 精确 | 仅行级四边形；字符级靠 CTC 时序反推，不准 |
| 字体/字号 | 有 | 无，只能从行高估 |
| 颜色/图形状态 | 有 | 无，只能采样像素 |
| render_order / xobj | 有 | 无意义 |

因此 OCR 路径下原文不再是"可编辑字符"而是"一张图"，输出策略必然是**白底矩形盖住 + 画译文**。BabelDOC 的 `ocr_workaround` 已经把这条输出路径修好了，缺的是前半段。

### 3.5 排版与字体

`[实测]` BabelDOC 侧：`typesetting.py` 的 scale 搜索 + 空间扩展（见 2.1）；`fontmap.py` 挑覆盖目标语言的字体（GoNotoKurrent 系列）；`babelpdf/` 7,656 行处理 base14 / CMap / CIDFont / Type3 度量；子集化丢子进程执行。

`[查证]` Rust 生态：`rustybuzz`（纯 Rust harfbuzz 移植）、`ttf-parser`、`swash`、`cosmic-text`（含 CJK 断行）、`subsetter` 与 `krilla` / `pdf-writer`（Typst 生态）。

`[推断]` 这是 Rust 相对 Python 优势最明显的一段——BabelDOC 中 backend + babelpdf 合计约 9.4k 行的工作，现成 crate 能吃掉大半。

### 3.6 翻译层

`[实测]` `il_translator_llm_only.py` 把段落序列化成带占位符的文本：公式 → `{v1}`，富文本样式 → `<b1>…</b1>`；返回后按占位符还原。配 SQLite 缓存（peewee）、限流、并发线程池、幻觉占位符清理。

`[推断]` 两个必须设计的失败模式：模型丢失/重复占位符；模型原样返回输入。两者都应降级为"保留原文"，绝不能静默产出缺公式的段落。这部分 Rust 直译即可，是最简单的模块之一。

---

## 4. 语言与生态

### 4.1 难度分布决定选型方向

`[推断]` 版面模型只占 BabelDOC 的 286 行 / 58k（0.5%）。**选型应围绕 PDF 读写与字体，而非围绕"哪个语言能跑 Paddle"。**

### 4.2 横向对比

| 语言 | PDF 读 | PDF 写 + 字体子集 | ONNX | 分发 | 评价 |
|---|---|---|---|---|---|
| **Rust** | pdfium-render / mupdf-rs / lopdf | **krilla + subsetter + pdf-writer（强）** | `ort`（成熟，多 EP） | 单二进制 | 推荐 |
| Python | pymupdf / pdfminer.six | fontTools（强） | onnxruntime | 需环境 | 原型最快；但已有 BabelDOC |
| C#/.NET | PdfPig（好） | 弱（CID 构造/子集化） | 一等公民 | NativeAOT | 中间选项 |
| Go | unipdf（商业许可）等，弱 | 弱 | 需 cgo | cgo 后失去优势 | 不建议 |
| C++ | MuPDF/PDFium 直用 | 全可控 | 原生 | 麻烦 | 仅嵌入场景 |

### 4.3 Rust 依赖清单（候选，未固定版本）

| 用途 | crate |
|---|---|
| PDF 几何/度量/光栅化 | `pdfium-render` |
| PDF 对象树 / 增量写 | `lopdf` |
| PDF 从零生成 | `krilla` / `pdf-writer` |
| 字体子集 | `subsetter` |
| 整形 / 断行 | `rustybuzz` / `ttf-parser` / `cosmic-text` |
| ONNX 推理 | `ort` |
| 图像 | `image` / `fast_image_resize` / `imageproc` |
| 多边形外扩（unclip） | `geo` / `cavalier_contours` |
| 空间索引 | `rstar` |
| 并行 | `rayon` |
| 快照测试 | `insta` |

> 版本号刻意未固定——应在实现时 `cargo add` 取当前版本。

---

## 5. 工作量与风险

### 5.1 成本分层 `[推断]`

**Tier 1 — 预计占 50%+ 时间**

- **启发式调参**：`paragraph_finder` + `styles_and_formulas` 约 4,000 行，直译两周可完成，但里面全是经验魔数（IoU 阈值、行距倍数、公式字体黑名单、角标高度比）。`fix_overlapping_paragraphs`、`merge_alternating_line_number_paragraphs`（合并 arXiv 交替行号）这类函数每一个都对应一类真实翻车文档。**写完代码只是起点。**
- **畸形 PDF 长尾**：对应那 354 个 except。Rust 里每个 `Result` 需显式处理，前期更慢、后期更稳。注意 PDFium 崩溃在 Rust 里是 abort 整进程，不是异常——子进程隔离仍然需要。

**Tier 2 — 代码量大但确定性高**

- content stream 解释器（选 PDFium 后估计可从 22k 行降到 6–8k）
- 增量 PDF 改写
- 排版引擎

**Tier 3 — 有现成件**

layout 推理、OCR 后处理、LLM 层、CLI、进度、资产下载。

### 5.2 规模折算 `[推断]`

| BabelDOC 部分 | 行数 | Rust 侧估计 |
|---|---:|---:|
| pdfminer | 19,900 | 0 |
| 数据表/生成代码 | 19,786 | ~1,500 |
| new_parser | 16,866 | ~6,000 |
| babelpdf | 7,656 | ~500 |
| midend | 7,603 | ~7,000 |
| frontend + utils | 7,259 | ~5,000 |
| backend | 1,750 | ~1,500 |
| docvision | 3,439 | ~800 |
| assets/tools/translator/CLI | 5,891 | ~3,000 |
| 新增 OCR 后处理 | — | ~1,500 |
| 新增 快照测试 | 933 | ~2,000 |
| **合计** | | **~28,000** |

按 BabelDOC 实测的 2.25 倍 churn 系数，实际敲出的代码量约 6 万行再删掉一半。

### 5.3 主要风险

| 风险 | 说明 | 缓解方向 |
|---|---|---|
| PDFium 缺口① 触顶 | 无 passthrough → 保真度上限受限 | 需早期在真实文档上量化损失程度 |
| RT-DETR 迁移摩擦 | 导出算子/EP 兼容、输出格式差异 | 先 dump ONNX 的 input/output shape 与算子清单验证 |
| 调参追不平 | 效果比参考实现差且难定位 | 快照测试 + 语料回归，尽早建立 |
| 范围蔓延 | 两条路径同时推进 | 分里程碑，先做能独立交付的那条 |

---

## 6. 待决问题

留给架构设计轮，本报告不预设答案。

**范围**

1. 原生 PDF 与扫描件两条路径，是否都做？先后顺序？
2. 是否需要双语对照输出？左右对照 / 交替页 / 仅译文？
3. 目标语种范围？只做英↔中，还是多语？（影响字体策略与断行规则）
4. 是否需要保留注释、书签、表单域、可选内容组？（直接决定 3.2 的 writer 选型）

**技术**

5. 保真度的可接受下限是什么？有没有客观度量方式？
6. PDFium 缺口①在真实目标文档上的实际损失有多大？需要一个量化实验。
7. 是否接受 PDFium 作为动态库依赖（影响单二进制承诺）？
8. layout 与 OCR 是否共用一次检测（PP-StructureV3 思路）？
9. 翻译后端：只支持 OpenAI 兼容 API，还是要抽象多后端？
10. 是否需要 Python/其他语言绑定，还是纯 CLI？

**工程**

11. 许可选择（BabelDOC 是 AGPL-3.0；若不复用其代码则不受限）
12. 模型分发方式：运行时下载 / 随包 / 用户自备？
13. 是否需要 GPU/NPU 加速路径，还是 CPU 优先？

---

## 附录 A · 测试语料 ⚠️ 已作废

> **本附录所述的 23 份压力测试 PDF 及其两个生成脚本已于 2026-08-21 正式作废**，原因是部分 fixture 存在坐标偏移与视觉质量问题——fixture 自身的几何不可信，而语料的价值完全建立在期望值可信之上。
>
> 根因是生成器与期望值同源：期望值由生成脚本自身产出，因此生成器写错什么，期望值就跟着错什么，偏移无法被自身发现。
>
> **不得恢复、复制或参考其几何参数与生成代码，亦不沿用其文件编号。** 语料从零重建，需求矩阵与生成合同见 `docs/03-corpus-requirements.md`，工作纳入里程碑 M-1（`docs/02-milestones.md`）。

本报告其余部分（BabelDOC 拆解、组件调研、工作量估算）不受此作废影响。

原附录中仍然成立的一条判断：合成语料是受控变量、适合回归，但真实世界的畸形程度造不出来，需另补真实语料——arXiv 论文（不同排版引擎：pdfTeX / XeTeX / LuaTeX / Word 导出）、`pdf.js` 的 `test/pdfs/`、PDFium 的 `testing/resources/`。这一判断已被 Corpus v1 采纳。

---

## 附录 B · 数据来源

**实测**（本机读取源码 / 头文件 / 模型元数据 / 运行流水线）

- BabelDOC 源码：`~/Code/03_Forks/BabelDOC`（v0.6.4 下游分支）
- 模型元数据：`~/.cache/babeldoc/models/doclayout_yolo_docstructbench_imgsz1024.onnx`
- 各阶段 IL 快照：以 `--skip-translation --debug` 运行后落在 `~/.cache/babeldoc/working/<stem>/`

**查证**

- PDFium 公开头文件：`pdfium.googlesource.com/pdfium/+/refs/heads/main/public/fpdf_text.h`、`fpdf_edit.h`
- PP-OCRv6：`huggingface.co/blog/PaddlePaddle/pp-ocrv6`、`paddleocr.ai/main/en/version3.x/algorithm/PP-OCRv6/`、HF 模型列表
- Rust OCR 实现：`github.com/siska-tech/pure-onnx-ocr` 等
