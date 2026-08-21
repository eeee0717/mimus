# ADR-0002 · 版面检测模型：PP-DocLayoutV3（ONNX）

- 状态：已接受（2026-08-21）
- 决策层级：难逆（类别词表、预处理、后处理、语料标注都会绑定它）

## 背景

BabelDOC 用 DocLayout-YOLO（10 类）。PP-DocLayout 系类别更细且其 `layout_priority` 表已含这些 label 名，证明 label 词表可替换（`docs/01-research.md` §3.3）。

2026-08-21 查证事实（来源见调研档案）：

- 官方模型：`huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx`（`inference.onnx` 131 MB，Apache-2.0），PaddleOCR-VL-1.5 的版面组件，PaddleOCR ≥ v3.4.0。
- 架构：RT-DETR 系 + mask 头，检测/分割/**阅读顺序**合并进单个 Transformer（论文 RT-DocLayout，arXiv:2606.23344）。
- 输入：`image [N,3,800,800]` + `im_shape [N,2]` + `scale_factor [N,2]`（均为 f32）；预处理 resize 800×800 **不保宽高比**（`interp: 2`），`norm_type: none`（无 ImageNet mean/std）但 **`is_scale` 默认 true，即像素先除 255，取值域是 [0,1]**。
- 输出：`fetch_name_0 [M,7]` f32、`fetch_name_1 [N]` **i32**（bbox_num）、`fetch_name_2 [M,200,200]` **i32**（实例 mask）。
- 25 类：abstract, algorithm, aside_text, chart, content, display_formula, doc_title, figure_title, footer, footer_image, footnote, formula_number, header, header_image, image, inline_formula, number, paragraph_title, reference, reference_content, seal, table, text, vertical_text, vision_footnote。

## 决策

版面检测采用 PP-DocLayoutV3 官方 ONNX，经 `ort` CPU EP 推理。

## 后果

- 收益：25 类细粒度（公式/表格/页眉页脚/参考文献原生区分）+ 模型级阅读顺序（BabelDOC 没有的能力）。
- 代价：131 MB 单档模型（无 S/M 轻量档）。
- 风险：RT-DETR 系算子（如 grid_sample）对 EP 兼容性差——已被"纯 CPU"决策规避。
- 风险（M0 实验 1 实测）：模型**非旋转不变**（`/Rotate 270`、`-90` 下分数掉到 `draw_threshold` 以下）；**极端宽高比下不可用**（20:1 的页面 0 检测）；**栏间距 8pt 时把整片双栏正文误判为 `table`**，比漏检更危险。缓解方案见实验结论 §5。

## 修订 · 2026-08-21（M0 实验 1，issue #11）

原文第 15 行遗留的"第 7 列语义未文档化，**需实验确认**"已由 [docs/04-m0-experiment-1.md](../04-m0-experiment-1.md) 结清，本文事实段已就地更正。三条按字面实现会出错的要点：

1. **第 7 列是 RT-DETR 的 query id，同时是页内阅读顺序键。** 按它升序排即得阅读顺序（13 份 fixture 全部一致），但它**只在页内有序**——跨页必须以 `(页号, col7)` 为排序键。同一个 query 会在多个类别上各出一行、框逐字节相同，按第 7 列去重前须先取该 query 的最高分行。
2. **`fetch_name_2` 的索引是行号，不是第 7 列。** 第 i 张 mask 对应 `fetch_name_0` 的第 i 行。
3. **输入取值域是 [0,1]。** 喂 0–255 会导致 0 检测（实测三页全归零），不是精度劣化。

`[M,7]` 与 mask 的利用方式所需的先行实验（原"代价"一节的里程碑 0 项）已完成。
