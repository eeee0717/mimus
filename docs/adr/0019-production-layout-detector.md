# ADR-0019 · 生产版面检测器、模型资产与回退语义

- 状态：已接受（2026-08-26）
- 决策层级：难逆（默认行为、资产失败语义、模型输入和 CI 接缝建立其上）

## 背景

ADR-0002 已选择 PP-DocLayoutV3 官方 ONNX，并由 M0 实验 1 钉死 800 x 800 非等比预处理、
`[0,1]` 输入域、三个输入/输出张量、25 类词表、0.5 阈值与第 7 列 query/read-order 语义。
M2 生产路径仍默认 `SingleLineLayoutDetector`，只能生成 fallback line；五份 recording 只能证明
给定标签后的 pass 行为，不能让真实 PDF 获得模型标签。

模型是 130,502,049 字节的外部资产。ADR-0005 要求大资产运行时下载、SHA-256 缓存、镜像与
自备路径；ADR-0018 已在字体侧实现同一解析链和 Asset/3 启动期失败语义。

## 决策

### 1. 默认 detector 与回退

- `translate` 和 `inspect` 默认构造 `OnnxLayoutDetector`，通过既有 owned `LayoutDetector`
  trait 向 pass 提供 regions；pass 不引用 ort 或模型类型。
- `--layout-replay <JSON>` 优先级最高。它保持 schema v1 严格页号/页几何校验，并完全绕过
  模型资产解析，是 required CI、Corpus 与本地复现的确定性接缝。
- `--layout single-line` 保留为显式降级。默认路径模型缺失、损坏或签名不兼容时以 Asset/3
  fail-fast，不静默回退，也不在输出已开始后切换 detector。

### 2. 模型资产

解析顺序为 `--layout-model` > `MIMUS_LAYOUT_MODEL` > config `layout_model` > 本地 SHA 缓存 >
manifest 下载。显式路径、缓存与下载文件都必须匹配 manifest SHA-256；镜像由 flag/env/config
配置，下载经大小上限、SHA-256 校验和同目录原子发布。
inspect 使用只读取 layout 资产字段的配置入口，不要求翻译 backend、key、字体或 cache 配置。

生产 manifest 钉死：

- upstream commit: `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`;
- file: `inference.onnx`, 130,502,049 bytes;
- SHA-256: `45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba`;
- URL: PaddlePaddle `PP-DocLayoutV3_onnx` Hugging Face repository at that commit.

### 3. 推理与栅格

只启用 ort CPU EP，intra-op threads = 4。模型初始化校验三个 input 和三个 output 的名称、dtype
与形状。后处理先按 query id 取最高分类别，再过滤 0.5 阈值，最后按 query id 升序输出；页号
由 pass 外层保证，因此文档顺序是 `(page_index, query_id)`。

layout raster 使用 M0 相同的 200 DPI（`200/72` pixel/point）再拉伸到 800 x 800。该需求由
detector 通过 trait 声明，PDFium engine 返回 owned RGBA snapshot；Write 对候选页使用相同倍率，
保持输入/输出像素等值合同。recording 与 single-line 的默认倍率仍为 1 pixel/point。

Poppler 与 PDFium 的抗锯齿会改变 query 分数和 id，因此 M0 资格测试使用 M0 的 pinned Poppler
200 DPI raster 比对六个原始框/类别/query order；真实 production renderer 的效果另由真实论文
离线报告验收，不能用 renderer 差异放宽 M0 oracle。

### 4. 政策与测试分级

真实 model label 优先于数学形状启发式。启发式仅在 `LayoutSource::FallbackLine` 上保留为漏检
兜底；`display_formula` 直接 passthrough，`inline_formula` 按 `{vN}` 段内协议处理。

required CI 只使用 recordings 或显式 single-line，不下载、不运行模型、不访问公网。真实模型
资格由 `MIMUS_LAYOUT_MODEL` 显式门禁：未设置时不进入资格路径；一旦设置，模型缺失、损坏或
结果不匹配均使测试失败，不允许 silent skip。

`ort = 2.0.0-rc.13` 声明 Rust 1.88，因此 workspace MSRV 从 1.85 提升到 1.88。CLI schema v2
保持不变，`configuration_resolved` 仅 additive 增加 layout mode、模型来源与模型 SHA-256；IL
schema 仍为 v1。

## 后果

- 首次默认运行可能下载约 131 MB；离线机器必须预置 cache 或显式提供模型路径。
- 模型错误在 PDFium/pass 启动前可诊断，生产不会悄悄退回低质量版面。
- required CI 保持确定性与零公网；真实资格需要开发机持有 pinned model。
- 段落边界会随模型 regions 大面积变化，旧段落级翻译 cache key 的命中率预计显著下降；新的
  端到端中文质量必须在另行授权的真实 API L5 重跑中验收。
