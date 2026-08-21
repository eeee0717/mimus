# M0 实验 1 · PP-DocLayoutV3 能力与 model order 结论

- 日期：2026-08-21
- 对应 issue：#11（T09）
- 前置：#3–#6（Corpus v1 的实验 1 批次，25 份 fixture 已通过 §2.8 独立验收）
- 复议对象：[ADR-0002](adr/0002-pp-doclayoutv3.md)

本文件记录实验结论本身。产生这些结论的 PoC 是一次性的，按 prototype 约定不入库；它活在
gitignore 的 `.context/m0-lab/` 下，本文件的每一条都注明了复现所需的最小信息。

## 0. 实验装置

| 项 | 值 |
|---|---|
| 模型 | `huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx`，repo commit `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6` |
| `inference.onnx` | 130 502 049 B，SHA-256 `45bf7175…d8b87cf28ba` |
| `inference.yml` | SHA-256 `506fcfac…6286a1f90fdc` |
| 运行时 | `ort = "=2.0.0-rc.13"`（ONNX Runtime 1.28），**CPU EP**，`with_intra_threads(4)` |
| 输入图 | `pdftoppm -r 200 -png`（独立渲染器，不用生产侧引擎） |
| 语料 | `corpus/fixtures/` 的 25 份，共 29 页 |
| 阈值 | `inference.yml` 的 `draw_threshold: 0.5`，未做任何调参 |

预处理严格按 `inference.yml`：`Resize` 800×800 / `keep_ratio: false` / `interp: 2`，
`NormalizeImage` mean 0 std 1 `norm_type: none`，`Permute` HWC→CHW。三个输入张量按
ADR-0002 给定。PoC 里**没有一行启发式**——第 7 列的语义正是被检验的对象，先在代码里替它
下结论就成了循环论证。

配对方法同理：检测框与 manifest 手写块的对应关系**只用几何 IoU**（阈值 0.3）建立，不用
类别也不用文本。

## 1. AC 1 · 官方 ONNX 在 CPU EP 上可用 —— **成立**

模型加载并推理成功，29 页全部跑通。会话签名实测如下：

| | 名称 | 形状 | dtype |
|---|---|---|---|
| 输入 | `image` | `[-1,3,800,800]` | f32 |
| 输入 | `im_shape` | `[-1,2]` | f32 |
| 输入 | `scale_factor` | `[-1,2]` | f32 |
| 输出 | `fetch_name_0` | `[-1,7]` | f32 |
| 输出 | `fetch_name_1` | `[-1]` | **i32** |
| 输出 | `fetch_name_2` | `[-1,200,200]` | **i32** |

25 类词表与 ADR-0002 逐字一致（`inference.yml` 的 `label_list`），实测输出中出现过
`text`、`paragraph_title`、`display_formula`、`inline_formula`、`table`、`header`、
`number`、`aside_text`、`content`、`image`、`figure_title` 等 11 类。

ADR-0002 未记录的三点，见 §5 的复议清单：`fetch_name_2` 是 Int32 而非浮点；
`fetch_name_1` 也是 Int32；输入取值域是 **[0,1]**（PaddleDetection 的 `is_scale` 默认
true，即先除 255），而 `norm_type: none` 极易被读成"原样喂 0–255"。这一条用开关做成了可
观察的：`M0_SCALE=0` 关掉除 255 后，`unit-order-01`、`unit-layout-07`、`unit-para-04`
三页在阈值 0.5 下**各自都从 6 / 5 / 1 个检测掉到 0 个**。取值域错了模型就完全不工作，不是
精度问题。

## 2. AC 2 · 第 7 列语义与绘制顺序不变性

### 2.1 第 7 列是 **query id**，不是行号、不是排名 —— 结论确定

判据用的是 mask 张量，而不是"看着像"：

1. `fetch_name_2[i]`（第 i 张 mask）的非零包围盒与 `fetch_name_0[i]` 的框吻合；与
   `fetch_name_0` 中第 7 列 == i 的那一行**不**吻合。→ **mask 的索引是行号，不是第 7 列。**
   ADR-0002 若按"第 7 列是 mask 索引"实现会全错。
2. `fetch_name_0` 的行按 score 降序排列（`unit-order-01`：0.941 / 0.934 / 0.933 / 0.930
   / 0.924 / 0.916）。
3. **共享同一个第 7 列取值的若干行，框逐字节相同、类别不同。** 例：`unit-order-01` 的
   `col7=53` 出现在 **18 行**上，框全部是 `[76.1, 226.7, 547.2, 339.1]`——`(row 0, text,
   0.9408)`、`(row 9, paragraph_title, 0.0251)`、`(row 22, inline_formula, 0.0148)`、
   `(row 37, abstract, 0.0118)`、`(row 39, vision_footnote, 0.0116)`，一直到
   `(row 273, algorithm, 0.0042)`。`unit-layout-01` 的 `col7=161` 同时给出 `table 0.534`
   与 `image 0.259`。
   → 第 7 列标识的是**一次预测**（RT-DETR 的一个 query），一个 query 会以多个类别各出一
   行。
4. 取值稀疏地散布在 0..299（模型有 300 个 query），既不是 0..M-1 的排名，也不是位置的纯
   函数——版面几乎相同的 `unit-base-02` 与 `unit-order-01` 给出完全不同的两组取值
   （34/56/137/173/202/261 对 23/53/125/154/230/283）。→ 它是被**预测**出来的量，不是索引
   的副产品。

**它同时是阅读顺序键。** 在 13 份可裁定几何的 fixture 上，「按第 7 列升序」与手写的
`reading_order` **全部一致**（29 页中的 17 页参与，其余按下表跳过）：

| fixture | 结果 | col7 序 |
|---|---|---|
| `unit-order-01-natural` | ✅ | L1 L2 L3 R1 R2 R3 |
| `unit-order-02-reversed` | ✅ | 同上 |
| `unit-order-03-interleaved` | ✅ | 同上 |
| `unit-order-04-column-continuation` | ✅ | L1 L2 R1 R2 |
| `unit-order-05-false-jump` | ✅ | B1 B2 B3 |
| `unit-order-06-cross-page` | ✅（**按页内排**） | A1 A2 / B1 B2 / C1 C2 |
| `unit-base-02-two-column` | ✅ | L1 L2 L3 R1 R2 R3 |
| `unit-form-01/02/03` | ✅ | 见 §4 D1 |
| `unit-geom-01-rotate-0`、`unit-geom-03` | ✅ | T1 T2 |
| `unit-layout-07-policy-zones` | ✅（漏 SEAL） | HDR P1 P2 REF FTR |
| `unit-para-07-line-numbers` | ✅ | N5 N10 N15 N20 BODY |

**几何兜底条件**（AC 2 明确要的那一条）：

- **第 7 列只在页内有序。** `unit-order-06-cross-page` 三页的取值是 50/168（p1）、
  45/129（p2）、33/118（p3）——把三页拉平后按 col7 全局排，得到 C1 B1 A1 C2 B2 A2，完全
  错乱；按 `(page_index, col7)` 排则完全正确。**mimus 必须以 `(页号, col7)` 为排序键**，
  跨页衔接自行实现（这与 §3.10 对 D3 的预判一致）。
- **漏检的块不会出现在序列里**，因此顺序正确 ≠ 覆盖完整。见 §3 与 §4 D4/D8。
- **第 7 列不可当作稳定 id 跨页面/跨版本复用**：同一版面微改即整体变号（base-02 vs
  order-01）。它只在单页单次推理内有意义。
- **一个 query 可能在多个类别上各出一行**，按第 7 列去重时必须先取该 query 的最高分行，
  否则同一个框会被计数多次。

### 2.2 绘制顺序不变性 —— **成立**，且比要求更强

`unit-order-01-natural` / `-02-reversed` / `-03-interleaved` 三份 fixture 内容相同、
content stream 绘制顺序分别为自然序 / 倒序 / 双栏行交错，由 corpus 的 `group` 断言保证
**逐像素相同**。三者的 `fetch_name_0` 张量**逐位相同**（SHA-256 前 16 位
`dc3a0a38c8a3acd6`），因此 model order 相同是平凡的。

这正是要点：模型只看渲染出来的像素，**绘制顺序对它不可见**。ORDER-01 因此在模型这一层
天然成立，无须任何补丁。对照组 `unit-base-02`（版面接近但不逐像素相同）给出不同的张量
（`00627d30d05b148a`），证明该哈希确实随输入变化，不是常量。

## 3. 模型能力边界（实测，均为 `draw_threshold: 0.5` 下）

| 现象 | 证据 | 影响 |
|---|---|---|
| **`/Rotate 90` 勉强可用，`270` / `-90` 落到阈值以下** | rotate-90 两块 `aside_text` 0.791 / 0.540；rotate-270 同样两块**几何找对了**（x 73..104、271..304 的两条竖带）但最高分只有 0.458，低于 0.5 → 阈值下 0 检测 | 不是"看不见"，是信心不足。缓解：推理前按 `/Rotate` 把栅格转正，而不是调低阈值 |
| **竖排/旋转文本被归为 `aside_text`** | rotate-90 的两块正文都是 `aside_text`，不是 `vertical_text` | 类别不能直接当语义用，须结合 `/Rotate` 判断 |
| **极端宽高比失效** | `unit-geom-04-oversized-page`（14400×720bp → 40000×2000 px，20:1）：阈值 0.5 下 0 检测；降到 0.1 才有一个 `text` 0.102 | 800×800 非等比缩放把 20:1 压成 1:1，字形被毁。缓解：切片或 letterbox 后分块推理 |
| **不产出嵌套框** | `unit-layout-01-nested-boxes`：只有外层框 `table 0.534`，内层的 `Table 1.` 图注**完全没有**独立检测；左侧正文块 `text 0.286` 也在阈值下 | 见 §4 D6 |
| **无线/有线表格只给一个整框** | `unit-layout-02-table-only`：`table 0.976`，无任何单元格级框 | 见 §4 D9 |

## 4. AC 3 · D1–D12 逐项裁决

| # | 补丁 | 红利 | 裁决 | 证据 |
|---|---|---|---|---|
| **D1** | display/inline 公式重标 | **兑现** | `unit-form-01`：`text` / `display_formula 0.859` / `text` 三块分明；`unit-form-03`：`text` + `inline_formula 0.849`。**`unit-form-02` 是关键正例**——公式被正文框包住约 60%，两个文本解析器都把它并进段落，模型仍单独给出 `display_formula 0.832` 与 `text 0.567`。模型在这一点上**强于两个 oracle** | 不需重新实现 IoU 重标 |
| **D2** | 跨栏合并（y2 差 > 20） | **兑现** | `unit-order-01`–`05` 全部按 col7 得到正确的栏内→跨栏顺序；`unit-order-05-false-jump`（单栏但正文起点人为抬高）**没有**被误判为跨栏，即 BabelDOC 那条 y 突跳判据的假阳性在模型这里不存在 | 删掉魔数 20 |
| **D3** | 跨页合并 | **不兑现**（如预判） | §2.1：col7 页内有序，跨页拉平即错乱 | **必须自行实现**，与 §3.10 的预判一致 |
| **D4** | fallback 全页聚类（9 个魔数） | **部分不兑现 —— 本次实验最重要的负面结论** | 见下方专段 | 窄栏间距下必须保留兜底 |
| **D5** | 交替行号合并 | **兑现** | `unit-para-07-line-numbers`：4 个行号各得一个 `aside_text` 框，正文得一个 `text`；col7 序 N5 N10 N15 N20 BODY | 不需自行实现行号识别，但**行号排在正文之前**，mimus 需按类别把 `aside_text` 剔出翻译流 |
| **D6** | 68 项优先级表 | **不兑现** | `unit-layout-01`：模型不产出嵌套框——内层图注没有独立检测，外层只有 `table 0.534`；同一个 query（col7=161）同时给 `table 0.534` 与 `image 0.259` | 归属冲突从"多来源框仲裁"变成"**同 query 多类别仲裁**"。规模远小于 68 项，但仍要写：至少要有"按 query 取最高分类别"这一条 |
| **D7** | 目录点线切段 | **兑现（类别层面）** | `unit-para-04-toc`：`content 0.955` 一个框盖住整个目录。**25 类中的 `content` 就是目录类**——这回答了 CONTEXT.md 遗留问题 #4 | 目录**页**能被识别；但模型只给整块框，**条目与页码的切分仍需自行实现**。红利是"知道这是目录，可整块跳过或特殊处理"，不是"条目已切好" |
| **D8** | 文本白名单含 header/footer/seal | **部分兑现** | `unit-layout-07-policy-zones`：`header 0.750` ✅；参考条目被判为 `text 0.786`（**不是 `reference`**）；页脚被判为 `number 0.751`（**不是 `footer`**）；**印章完全漏检** | 页眉可靠。页脚/参考/印章不可靠，政策区域（不翻译）的判定**不能只靠模型类别**，须叠加位置规则 |
| **D9** | 二级表格检测模型 | **不兑现** | `unit-layout-02-table-only`：`table 0.976` 单框，无单元格粒度 | 单元格级处理必须自行实现或另接模型 |
| **D10** | 上下标 0.79/1.1 + sticky flag | **不兑现**（如预判） | `unit-form-04-superscript`：真上下标处只有 `text` + `inline_formula 0.758`；small caps 与首字下沉都只是 `text`，模型对它们无概念 | **必须自行实现**，与 §3.10 预判一致 |
| **D11** | 公式字体三层正则（约 130 条） | **兑现** | 同 D1：`inline_formula` 类可直接用，`unit-form-03` 0.849 | 约 130 条正则可退化为兜底 |
| **D12** | 公式合并四组条件 | **兑现** | `unit-form-08-formula-fragments`（每个变量同时带上标与下标，mutool 切成 3 块）：模型给出**单个** `display_formula 0.720` 盖住整行，没有碎片化 | 不需要缝合逻辑 |

### D4 专段：栏间距 8pt 下模型不仅漏检，而且**误判类别**

`unit-layout-08-narrow-gutter`（548×176bp，双栏，栏宽 240pt，**栏间距 8pt**）：

- 模型输出：**一个** `table 0.732` 框，x 73..1448 px（整个版心），阈值降到 0.2 也没有第二个框。
  六个段落一个都没有单独框出，而且整片正文被判成表格。
- 单一变量对照（同页面、同字体、同内容、同宽高比，只改栏间距；探针 PDF 在
  `.context/m0-lab/probe/`，不入库）：

  | 栏间距 | 检测 | 类别 |
  |---|---|---|
  | 8pt | 1 个框盖住两栏 | `table` 0.732 |
  | 30pt | 5 + 1 个框，左右栏分明（左 x 73..727、右 x 795..1444） | 全部 `text`，0.88–0.92 |
  | 60pt | 5 + 1 个框，左右栏分明 | 全部 `text`，0.81–0.94 |

  **失效随栏间距出现，与宽高比无关。**

裁决：D4 的红利在常规版面上兑现（`unit-layout-02` 的表格、`unit-order-*` 的正文都不漏
检），但在窄栏间距下**完全失效**，且失效方式比 DocLayout-YOLO 的"漏检"更危险——被误判
为 `table` 的正文会走表格通路，可能整片不翻译且不报错。BabelDOC 的兜底聚类**必须保留**，
或至少保留一个"模型输出与字符覆盖率不符时报警"的检查。

顺带一条与模型无关但同样重要的观察：8pt 栏间距下 `pdftotext -bbox-layout` 把整页并成一个
11 行的块（x 跨度 30..519.76pt，跨过栏间距连成整行），而 `mutool draw -F stext` 给出干净
的 6 块。两个 oracle 在这一页上分歧，这正是 LAYOUT-08 要暴露的现象，已记进该 fixture 的
`[[adjudication]]`。

## 5. AC 4 · 成 / 败 / 替代方案，与需要复议的 ADR

### 成

- ADR-0002 的核心假设**全部成立**：官方 ONNX 在 ort CPU EP 上可用；800×800 非等比预处理
  与三个输入张量正确；25 类词表准确；**模型自带阅读顺序，且对 content stream 绘制顺序完全
  免疫**。这是相对 BabelDOC 最大的一条红利，已被实验坐实。
- 12 条红利里 6 条兑现（D1、D2、D5、D7 类别层面、D11、D12），可从 midend 删去。

### 败

1. **D3、D10 不兑现**（本就预判如此），**D6、D9 不兑现**，**D4 在窄栏间距下失效**，
   **D8 只兑现 header 一项**。midend 仍需自行实现：跨页合并、上下标/首字下沉、同 query
   多类别仲裁、表格单元格、窄栏兜底聚类、政策区域的位置规则。
2. **模型不是旋转不变的**：`/Rotate 270` 与 `-90` 下检测分数掉到阈值以下。
3. **极端宽高比（20:1）下模型不可用**。
4. **`content`（目录）只给整块框**，条目切分仍要自己写。

以上都不构成推翻 ADR-0002 的理由——DocLayout-YOLO 在这些点上只会更差（10 类、无阅读顺
序）。它们改变的是 **midend 的工作量估计**，应回写到 §3.10 与里程碑排期。

### 替代方案（针对上述失败项）

| 失败项 | 替代方案 | 何时决定 |
|---|---|---|
| 旋转页分数不足 | 推理前按 `/Rotate` 把栅格转正，推理后把框转回页面空间 | M1，属实现细节，不需新 ADR |
| 极端宽高比 | 按长边切片 + letterbox 后分块推理，框做去重合并 | 触发条件：语料里出现宽高比 > 4:1 的真实页面。V1 可先拒绝并报错 |
| 窄栏间距误判为 `table` | 保留 BabelDOC 同款兜底聚类；或加"字符覆盖率 vs 模型框覆盖率"一致性检查，不符时降级走兜底 | M1，须在 §3.10 里把 D4 从"待验证"改为"部分保留" |
| 政策区域判定 | 类别 + 位置双判据（页面上下 N% 且字号小于正文） | M1 |

### 需要复议 / 修订的 ADR

**ADR-0002 需要一次事实修订**（决策本身不变，状态仍为"已接受"）：

1. 第 15 行「第 7 列语义未文档化，推断与阅读顺序相关，**需实验确认**」→ 已确认：**第 7 列是
   RT-DETR 的 query id，同时是页内阅读顺序键**；并需补上「`fetch_name_2` 的索引是**行号**，
   不是第 7 列」这一条，否则按字面实现会全错。
2. 第 15 行的 dtype：`fetch_name_1` 与 `fetch_name_2` 都是 **Int32**，原文未写 dtype 易被读
   成浮点。
3. 第 14 行的预处理：`norm_type: none` 之外必须补上 **`is_scale` 默认 true，即输入取值域是
   [0,1]（先除 255）**。只写 `norm_type: none` 会被实现成 0–255，实测检测质量明显劣化。
4. 第 25 行「`[M,7]` 列语义与 mask 输出的利用方式需要一个先行实验（里程碑 0 项）」→ 该实
   验即本文件，可标记为已完成。
5. 「风险」一节可补：模型**非旋转不变**、**极端宽高比下不可用**、**窄栏间距下会把正文误判
   为 `table`**。

**其余 ADR 无须复议。** 没有遗留未决风险：本文件列出的每一条失败项都配了替代方案与决定时
点，`CONTEXT.md` 遗留问题 #4（25 类是否含目录类）由 §4 D7 结清。

## 6. 复现方式

PoC 不入库。复现所需：

1. 按 §0 的 SHA-256 取 `inference.onnx` 到 `.context/m0-lab/assets/`（`PROVENANCE.md` 记
   来源）。
2. 一个 ~200 行的 Rust 二进制：ort CPU EP 加载模型，按 §0 预处理，把 `fetch_name_0` /
   `fetch_name_1` 原样打成 JSON，`fetch_name_2` 只留每张 mask 的非零计数与包围盒。
   **不写任何后处理启发式。**
3. `pdftoppm -r 200 -png` 渲染 `corpus/fixtures/*/`。
4. 对照脚本：把检测框按几何 IoU 与 `adjudicated.toml` 的 `metric_box` 配对（**不用类别、
   不用文本**），比较「按 `(页号, col7)` 排」与 manifest 的 `reading_order`。
