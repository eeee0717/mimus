# ADR-0007 · IR：单字符粒度 + 和类型 + JSON 快照

- 状态：已接受（2026-08-21）
- 决策层级：难逆（全流水线围绕 IR 变换，测试网以其序列化为基础）
- 修订：2026-08-31 明确 `Char.font_size` 的页面空间语义；IL schema 保持 v1。

## 背景

BabelDOC 的 IL 经两年收敛验证了单字符粒度 + 双盒模型；其缺陷是"五个 Optional 字段五选一"的伪 union，下游手写分发、漏分支即静默 bug（研究报告 2.2）。V2 扫描件路径的 OCR 产物是行级文本（无字体/字号/图形状态），IR 必须预留（ADR-0003）。

## 决策

1. **粒度**：单字符，双盒（`box` 字体度量盒 / `visual_bbox` 墨迹盒），layout 归属用墨迹盒。
2. **和类型**：段落组成等"多选一"结构一律用 Rust enum，编译器强制穷尽匹配。
3. **扫描预留**：文本载体为 tagged enum——V1 仅 `Chars` 变体；V2 增加 `OcrLine` 变体不破坏 schema。
4. **序列化**：serde + JSON，顶层 `schema_version` 字段。`insta` 快照测试与 `--debug` 逐 pass 落盘共用同一条序列化路径。V1 不承诺跨版本 IR 兼容。
5. **文本朝向**（2026-08-21 补）：字符携带和类型 `TextTransform { Upright, Rotated(deg), Mirrored, Skewed(deg) }`，在**视觉页框**（应用 `/Rotate` 之后）内度量。它是"非直立文本不翻译"政策的载体，与第 2 条同理由——不用几个 `Option<f32>` 表达互斥状态。判定口径见 `CONTEXT.md` 术语表"非直立文本"。
6. **字号口径**（2026-08-31 补）：IL `Char.font_size` 是页面空间的有效 em，取原始
   `Tf` 绝对值乘以 `CTM × Tm` 线性部分的竖直基向量长度。ParagraphFind 的同行、词距、
   列与自然段阈值以及 Typeset 的首选字号都使用这一口径。walker 的 `WalkedChar.font_size`
   仍保存原始 `Tf`，供源 text-show 与公式的字节/字体/矩阵精确重放；两层不得混用。

## 后果

- 快照测试成为一等公民（研究报告点名的"避免 BabelDOC 命运"的手段）。
- JSON 快照体积大（单页数百字符 × 十余字段），用 insta redaction/筛选控制，不改格式。
- passthrough 字段（原样保存的操作符字节）存于字符/图形状态上，与 ADR-0006 的走查配套。
