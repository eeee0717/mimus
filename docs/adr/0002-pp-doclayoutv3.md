# ADR-0002 · 版面检测模型：PP-DocLayoutV3（ONNX）

- 状态：已接受（2026-08-21）
- 决策层级：难逆（类别词表、预处理、后处理、语料标注都会绑定它）

## 背景

BabelDOC 用 DocLayout-YOLO（10 类）。PP-DocLayout 系类别更细且其 `layout_priority` 表已含这些 label 名，证明 label 词表可替换（`docs/01-research.md` §3.3）。

2026-08-21 查证事实（来源见调研档案）：

- 官方模型：`huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx`（`inference.onnx` 131 MB，Apache-2.0），PaddleOCR-VL-1.5 的版面组件，PaddleOCR ≥ v3.4.0。
- 架构：RT-DETR 系 + mask 头，检测/分割/**阅读顺序**合并进单个 Transformer（论文 RT-DocLayout，arXiv:2606.23344）。
- 输入：`image [N,3,800,800]` + `im_shape [N,2]` + `scale_factor [N,2]`；预处理 resize 800×800 **不保宽高比**，`norm_type: none`（无 ImageNet mean/std）。
- 输出：`fetch_name_0 [M,7]`（7 列，超出常规 `[cls,score,x0,y0,x1,y1]` 的第 7 列语义未文档化，推断与阅读顺序相关，**需实验确认**）、`fetch_name_1 [N]`（bbox_num）、`fetch_name_2 [M,200,200]`（实例 mask）。
- 25 类：abstract, algorithm, aside_text, chart, content, display_formula, doc_title, figure_title, footer, footer_image, footnote, formula_number, header, header_image, image, inline_formula, number, paragraph_title, reference, reference_content, seal, table, text, vertical_text, vision_footnote。

## 决策

版面检测采用 PP-DocLayoutV3 官方 ONNX，经 `ort` CPU EP 推理。

## 后果

- 收益：25 类细粒度（公式/表格/页眉页脚/参考文献原生区分）+ 模型级阅读顺序（BabelDOC 没有的能力）。
- 代价：131 MB 单档模型（无 S/M 轻量档）；`[M,7]` 列语义与 mask 输出的利用方式需要一个先行实验（里程碑 0 项）。
- 风险：RT-DETR 系算子（如 grid_sample）对 EP 兼容性差——已被"纯 CPU"决策规避。
