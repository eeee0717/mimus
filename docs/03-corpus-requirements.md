# Corpus v1 · 需求矩阵与生成合同

> 状态：生成合同已执行；首批 M0 fixture 正按 §4 清单逐批入库
> 日期：2026-08-21
> 事实基础：BabelDOC 主链路源码（`~/Code/03_Forks/BabelDOC`，commit `79146c3`）第一手分析
> 决策基础：`CONTEXT.md`、`docs/adr/`、GitHub Issue #1

---

## 0. 旧语料作废声明

早期调研阶段曾生成一批 23 份合成 PDF 与两个生成脚本。**该批语料及其生成代码已正式作废**，原因是部分 fixture 存在坐标偏移与视觉质量问题——即 fixture 自身的几何不可信，而语料的全部价值恰恰建立在"期望值可信"之上。

作废意味着：

- 不恢复、不复制、不参考其几何参数与生成代码；
- 不沿用其文件编号（`01_…`–`23_…`）；本文档定义全新的 case ID 与 fixture ID 体系；
- Corpus v1 **从零构建**，且必须通过 §2 的生成合同与 §3 的独立验收才能入库。

作废的根因值得记入教训：旧语料的期望值由生成脚本自身产生，**生成器与期望值同源**，因此坐标偏移无法被自身发现。生成合同的第一原则（§2.1）正是针对这一点。

---

## 1. 本文档的作用

1. 定义 **Corpus v1 需求矩阵**：从 BabelDOC 主链路逆向出的、可由 PDF 输入触发的失效模式清单，每个失效模式给出可构造、可断言、可验证的定义。
2. 定义 **生成合同**：任何 fixture 进入 Corpus v1 前必须满足的约束。
3. 确定 **首批 M0/M1 fixture 清单**。

本文档不生成 PDF，不实现生成器。产出物是需求与合同。

---

## 2. Corpus v1 生成合同

### 2.1 第一原则：期望值与生成器必须独立

**Manifest 是规格，PDF 是实现。**

每份 fixture 的期望值（几何、字符、结构）必须**先于**生成器手写，来源是 PDF 规范与字体度量的推导，而**不是**从生成结果读回。生成器的正确性由 manifest 检验，而非相反。

违反此原则的语料是自证的：生成器写错了什么，期望值就跟着错什么——这正是旧语料坐标偏移未被发现的原因。

推论：

- 禁止"生成后 dump 一份 expected"的做法；
- Manifest 与生成器由不同的推导路径产生，二者不一致时**先怀疑生成器**，但必须逐例人工裁定，不得单方面改 manifest 迁就生成器；
- 每次这类不一致都要在 manifest 里留一行裁定记录。

#### 唯一例外：现实排版 fixture 的几何期望

上述原则对精确 fixture 与畸形 fixture 无条件成立，但对**现实排版引擎产出的 fixture**（Typst / LaTeX，见 §2.5）不可执行——没人能手推 pdfTeX 断行后每个字形的精确坐标。而实验 1 的整个首批恰恰全是这类 fixture。

例外条款：

- **结构化期望仍然手写**——页数、栏数、段落数与顺序、每段的文本内容、每个可断言元素属于哪一栏，这些先于生成写死，生成结果不符即为失败；
- **几何期望由两个互相独立的解析器一致确立**——poppler（`pdftotext -bbox-layout`）与 mutool（`mutool draw -t`）分别提取字形坐标，**两者在容差内一致时才采信**；
- **两者不一致即阻止该 fixture 入库**，记录分歧点并另择构造方式，不得任选其一。

这保住了第一原则的实质：期望值不与生成器同源。生成器是排版引擎，裁定者是两个与它无关的解析器；三方独立，任一方的 bug 都不会自证。

代价要说清楚：这类 fixture 的几何容差只能取到两个解析器的一致精度，达不到精确 fixture 的 1e-3 pt 档。因此现实排版 fixture **不承担**往返几何精度断言，那由 §4.1 的精确基线 fixture 承担。

### 2.2 坐标系与几何约定

**基准坐标系**：PDF 原生用户空间——单位 **pt**（1/72 inch），原点**左下**，x 向右、y 向上。Manifest 中所有几何量均以此表达，不使用左上原点、不使用像素、不使用归一化坐标。

**页面空间的定义**：manifest 的几何值表达在**页面空间（page space）**，即 MediaBox 所定义的、**应用 `/Rotate` 之前**的默认用户空间。`/Rotate` 与 CropBox 是 manifest 中显式记录的元数据，由消费方自行推导观看空间，**不预先烘焙进期望坐标**。

**三种盒子必须分别标注，不得混用**：

| 名称 | 定义 | 来源 |
|---|---|---|
| **baseline origin** | 字符绘制起点（文本空间原点经变换后的点）+ 前进方向 | 由 `Tm`/`Td`/`TJ` 位移精确推导 |
| **字体度量盒（metric box）** | 由字体的 ascent/descent/advance × 字号构成的名义盒，与实际墨迹无关 | 由字体度量表推导 |
| **visual bbox（墨迹盒）** | 字形轮廓的实际外接矩形 | 由字形轮廓（glyph outline）推导 |

三者在 manifest 中是三个独立字段。术语与 `CONTEXT.md` 的**双盒**定义一致：`box` = 度量盒、`visual_bbox` = 墨迹盒；baseline 是双盒之外的第三个量，旧语料的偏移问题很可能就源于三者混淆。

容差分层（超出即为失败，不得放宽迁就实现）：

| 量 | 容差 | 理由 |
|---|---|---|
| baseline origin | ≤ 1e-3 pt | 纯算术推导，无解释歧义 |
| 度量盒 | ≤ 1e-3 pt | 由字体度量表整数值 × 字号推导 |
| visual bbox | ≤ 1e-2 pt | 依赖字形轮廓解释，允许极小的实现差异 |

### 2.3 变换规则

坐标变换链必须在 manifest 中可追溯，组合顺序固定为：

```
字形空间 (glyph space)
  → ×(1/unitsPerEm) → 文本空间 (text space)      [Tf 字号、Tz 水平缩放、Ts 上下移、TL 行距]
  → ×Tm → ×CTM      → 用户空间 (user space)
  → ×(嵌套 Form XObject 的 CTM，由内向外依次左乘)
  → 页面空间 (page space)                          ← manifest 期望值在此
  → [CropBox 裁剪] → [/Rotate 旋转] → 观看空间
```

规则：

- **MediaBox**：定义页面空间。原点不必是 (0,0)——非零原点的 MediaBox 是一个独立的失效模式，manifest 必须能表达它。
- **CropBox**：仅影响观看与光栅化，**不改变**页面空间坐标。缺失时等同 MediaBox。CropBox ⊄ MediaBox 属畸形，单独立例。
- **`/Rotate`**：仅影响观看空间，取值必须是 90 的整数倍。manifest 记录其值，期望几何保持在旋转前。
- **嵌套 CTM**：Form XObject 的 `/Matrix` 与调用点的 `cm` 依次复合。嵌套层级与每层矩阵在 manifest 中逐层列出，不得只给最终结果——否则无法定位是哪一层算错。

### 2.4 单变量原则

**每份 unit fixture 只引入一个主要变量**，其余要素保持在该 fixture 家族的基线配置（同一字体、同一字号、同一页面尺寸、同一对象顺序）。

fixture 分两类：

- **unit fixture**：单变量，与需求矩阵中的 case 近 1:1。绝大多数 fixture 属此类。
- **integration fixture**：显式多变量，仅用于端到端冒烟。每一份都必须在 manifest 中写明"为什么必须合并这些变量"，且不得替代对应的 unit fixture。数量上严格受限。

### 2.5 生成方式（按 fixture 类别）

| 类别 | 生成方式 | 理由 |
|---|---|---|
| **精确几何 fixture** | 手写 content stream + 自研确定性对象 writer（裸字节） | 需要逐字节可控、可读、可 diff；任何库都会带来不受控的对象重排与默认值 |
| **现实排版 fixture** | 固定版本的 Typst 或 LaTeX（pdfTeX / XeTeX / LuaTeX） | 需要真实排版引擎的产物特征（字体嵌入方式、CMap 风格、断行痕迹），手写造不出来 |
| **畸形 fixture** | 从**一个合法最小 PDF** 做**单变量字节级变异** | 每个畸形 fixture 必须有可追溯的合法父本 + 一处明确变异，才能断言"是这处变异导致了这个行为" |

**禁止事项**：

- **不得使用 mimus 生产侧的 lopdf 或 PDFium 生成测试输入**。用被测组件生成被测输入是循环论证——lopdf 写错的结构，语料会原样接受。
- 精确 fixture 的 content stream **不压缩**（不使用 `/FlateDecode`）：可读、可 diff、便于字节级变异，且消除 zlib 实现差异。现实排版 fixture 允许压缩（其产物特征本身是被测对象）。

#### Corpus 自有精确 writer（M0）

精确 writer 位于非生产 crate `crates/corpus`，只服务 M0 语料制造，不是 mimus 的
生产 PDF writer。其依赖闭包不得包含 lopdf、PDFium、`pdfium-render` 或
`mimus-core`。writer 以固定调用顺序分配对象号，显式写出 PDF header、每个
indirect object、经典 xref、trailer、`startxref` 和 EOF；字典项与 content stream
也由 fixture 配方固定，不经过无序容器或 PDF 库重排。

精确 fixture 不写 Info 字典、日期或 XMP，trailer `/ID` 按 §2.6 从 fixture ID
确定性派生。PDF 先完整生成在内存中，再通过同目录临时文件原子替换目标；生成、写入
或替换失败时不得留下半成品，也不得破坏既有目标。`corpus build` 负责复现提交的
PDF，`corpus determinism` 对同一配方连续生成两次并比较 SHA-256。

#### 单字节畸形派生接口

畸形 fixture 的 manifest 必须记录合法父本 fixture ID，并且恰好包含一条变异记录：
唯一字节偏移、原字节、替换字节和变异语义。派生时先核对父本在该偏移的原字节，产出
后再比较完整父子字节串，要求长度不变且差异集合严格等于该唯一偏移。这样后续畸形
fixture 可以复用同一 API，同时仍满足 §2.4 的单变量原则。

现实排版引擎的选择（全部已装并钉死版本，见 `corpus/toolchain.toml`）：

- **Typst 0.15.1**：单二进制、版本易钉、`SOURCE_DATE_EPOCH` 可控，作为现实排版的主力；
- **pdfTeX 1.40.29 / LuaHBTeX 1.24.0**（TeX Live 2026）：产物差异显著（Type1 vs OpenType 嵌入、CMap 构造、字形命名），用于引擎多样性 fixture；
- **XeTeX 0.999998 + xdvipdfmx 20260317：不进 Corpus v1。** 它过不了 §2.6 的重复生成门禁（随机字体子集标签），实测结论见 §2.6。它仍然是真实 arXiv 语料的重要来源——那批语料不入 repo、不做哈希门禁，因此不受影响。

### 2.6 确定性要求

**同一输入重复生成必须得到相同的 SHA-256。** 这是可执行的门禁，不是口号：`corpus determinism` 重新生成一次，比对哈希，不一致即失败。

必须固定或消除的不确定源：

| 不确定源 | 处理 |
|---|---|
| `/CreationDate`、`/ModDate` | 精确/畸形 fixture 不写 Info 字典；排版 fixture 见下表逐引擎的实测开关 |
| trailer `/ID` | 固定为由 fixture ID 派生的常量（取其 UTF-8 前 16 字节、不足补零；`corpus trailer-id <id>` 生成两种引擎的写法） |
| XMP metadata 中的时间戳 | 不生成；若排版引擎强制生成，则在 manifest 中记录并由生成脚本剥离 |
| 对象编号与写出顺序 | writer 按 manifest 声明的固定顺序发号，不依赖字典/哈希遍历顺序 |
| **字体子集标签** | 必须由引擎确定性产出——这是 XeTeX 出局的直接原因，见下 |
| 字体 | 精确 fixture 钉死字体文件 + 记录其 SHA-256；现实排版 fixture 见下表 |
| 工具版本 | 全部写进 `corpus/toolchain.toml` 并由 `corpus doctor` 精确比对；版本变化视为语料变更，需重新走验收 |
| 浮点格式化 | 固定小数位数与舍入规则，不用平台默认 `repr` |

#### 实测结论（2026-08-21，本机；由 `corpus determinism` 复现）

原先本节的排版引擎机制来自文档层面的认知。逐项实测后，**三条被推翻或修正**，结论如下。可执行形式是 `corpus/toolchain.toml` 的 `[[engine]]` 表——那里的 `args` / `env` 就是配方本身，本表是它的散文版。

| 引擎 | Corpus v1 | 实测生效的机制 |
|---|---|---|
| **Typst 0.15.1** | ✅ 可用 | `SOURCE_DATE_EPOCH=0` 把 `/CreationDate`、`/ModDate` 固定为 `D:19700101000000Z`；trailer `/ID` 由文档内容派生，随之固定。`--ignore-system-fonts` 排除系统字体查找。 |
| **pdfTeX 1.40.29** | ✅ 可用 | `\pdfinfoomitdate=1` + `\pdfsuppressptexinfo=-1` + `\pdfomitcharset=1` + `\pdftrailerid{<fixture-id>}`（见 `corpus/determinism/pdftex-deterministic.tex`）。`SOURCE_DATE_EPOCH=0` 与 `FORCE_SOURCE_DATE=1` 是第二道保险。 |
| **LuaHBTeX 1.24.0** | ✅ 可用 | `\pdfvariable omitcharset=1` + `\pdfvariable suppressoptionalinfo=511` + `\pdfvariable trailerid{[<hex> <hex>]}`（见 `corpus/determinism/luatex-deterministic.tex`）。 |
| **XeTeX 0.999998 + xdvipdfmx 20260317** | ❌ **不可用** | 无可用机制。 |

三处对原文的修正：

1. **`\pdfvariable` 不是 pdfTeX 的原语**——那是 LuaTeX 语法。pdfTeX 用的是 `\pdftrailerid` / `\pdfsuppressptexinfo` / `\pdfinfoomitdate` / `\pdfomitcharset` 四个独立原语。原文的写法在 pdfTeX 下直接 `! Undefined control sequence`。
2. **LuaTeX 的 `trailerid` 是原样写入 `/ID` 的记号表**，必须自带完整的 PDF 数组语法 `[<32 位 hex> <32 位 hex>]`；只写裸文本会产出 qpdf 无法解析的 xref 流（表现为 "unknown token while reading object"），且 `/ID` 实际缺失。LuaTeX 的 `suppressoptionalinfo` 位表也与 pdfTeX 的同名整数不同：`1/2/4/8` 是四个 `PTEX.*` 键，`16` Creator、`32` CreationDate、`64` ModDate、`128` Producer、`256` Trapped、`512` ID；取 `511` 才能全抑制而保留 `/ID`。
3. **XeTeX 无法用于 Corpus v1。** xdvipdfmx 为每个子集化字体生成**随机六字母子集标签**，每次运行都不同：同一份 `.xdv` 连续三次转换得到 `DLUKLR` / `KBCCUL` / `TBDBBV`。`SOURCE_DATE_EPOCH` 只影响日期不影响标签；命令行 getopt 串 `:hD:r:m:g:x:y:o:s:p:clf:i:qtvV:z:d:I:K:P:O:MSC:Ee` 里没有对应开关，`-C` 的 `0x0080`–`0x0800` 实测无效。这不只是破坏 SHA-256 门禁——**子集标签正是本节溯源手段的载体之一**，随机标签让那条断言根本写不出来。复议触发条件：dvipdfmx 提供确定性标签开关。

**门禁的灵敏度必须自证。** 两次构建若落在同一秒里，写墙钟时间戳的引擎会碰巧通过，门禁就成了摆设。因此 `corpus determinism` 在两次构建之间强制插入 ≥1.2 s 的时钟间隔（跨过一整秒，PDF 日期的最小分辨率）。已验证的反例：去掉 `SOURCE_DATE_EPOCH` 的 Typst、去掉四个开关的 pdfTeX，两者在该间隔下都稳定失败。XeTeX 在表中被标为"期待失败"，它每次跑都在替门禁做灵敏度自检。

**现实排版 fixture 的字体来源**：不额外 vendored 字体文件。Typst 用 `--ignore-system-fonts`，字体集合收敛为 Typst 二进制内嵌的那一份；TeX 引擎用 TeX Live 2026 自带的 Computer Modern 系列。两者都由已钉死的工具版本本身担保，且**字体完整嵌入产出的 PDF**，再 vendored 一份文件只是第二套 pin，不增加信息。上一段表格里"钉死字体文件 + SHA-256"的要求继续对**精确 fixture** 无条件成立——那批 fixture 的期望墨迹盒是手推的，字体度量必须逐字节可控。

**输入字体的选择**：精确 fixture 一律**嵌入钉死的字体文件**（记录 SHA-256），而非依赖 base-14 的隐式度量——否则"期望墨迹盒"取决于消费方的 base-14 度量表，不可判定。例外是专门测试 base-14 / 非嵌入字体处理的 fixture。

**溯源手段**：判断输出 PDF 中某个字形来自原文还是译文，依据是**对象号 + 子集标签（subset tag）**，二者都是逐 fixture 在 manifest 中钉死的。这是硬约束。

字体族刻意不使用 Noto Sans SC（mimus 的译文输出字体）只是**建议**，不是约束——拉丁侧用 DejaVu 或 Liberation 很方便，但 CJK 输入 fixture（如 CMAP-01 的 GBK 编码用例）可选字体少且体积大，可能不得不与输出字体同族。同族时溯源仍然成立，因为断言不依赖字体族。

本节的确定性要求**不需要为加密 fixture 开例外**：V1 一律拒绝加密 PDF（ADR-0009），DOC-03 的两份 fixture 只需触发拒绝路径，其中 AES 档一次性生成后入库即可，不参与往返。

### 2.7 每份 PDF 配结构化 expected manifest

一份 PDF 一份 manifest，格式 TOML（人工手写、需 code review，故不用 JSON）。必备字段：

- **身份**：fixture ID、名称、类别（unit / integration）、合法性（legal / malformed）、覆盖的 case ID 列表
- **来源**：生成方式、生成器与工具版本、字体文件及其 SHA-256、PDF 自身 SHA-256
- **谱系**（畸形 fixture 必填）：合法父本的 fixture ID + 变异的字节偏移与语义描述
- **页面几何**：逐页 MediaBox、CropBox、`/Rotate`
- **期望内容**：按 §2.2 分列的 baseline origin / 度量盒 / visual bbox；字符与其 Unicode；XObject 嵌套层级与逐层矩阵
- **期望行为**：mimus 处理后**独立可观察**的断言（见 §2.9）
- **oracle**：本 fixture 适用的验证手段清单
- **优先级**：M0 / M1 / M3
- **裁定记录**：manifest 与生成器发生过的不一致及其裁定结论

畸形 fixture 的谱系记录至少采用以下形状；数字 byte 使用 `0..255` 的十进制表示：

```toml
[lineage]
parent = "unit-base-01-single-line"

[[lineage.mutations]]
byte_offset = 123
original_byte = 84
replacement_byte = 81
description = "replace the selected T operator byte with Q"
```

### 2.8 独立验收

每份 fixture 入库前必须通过，**至少一个独立解析器 + 至少一个独立渲染器**，且二者都不得是 mimus 生产侧组件（排除 lopdf 与 PDFium）：

| 角色 | 工具 | 钉死版本 |
|---|---|---|
| 独立解析器（结构） | `qpdf --check` / 结构 dump | qpdf **12.4.0** |
| 独立解析器（文本与坐标） | poppler `pdftotext -bbox-layout` | poppler **26.08.0** |
| 独立解析器（文本与坐标，第二方） | MuPDF `mutool draw -t` | mutool **1.28.2** |
| 独立渲染器 | poppler `pdftoppm`、MuPDF `mutool draw`；备选 Ghostscript | 同上；gs **10.07.1** |
| 独立排版产出 | Typst / pdfTeX / LuaHBTeX | Typst **0.15.1**、TeX Live **2026** |

版本是**精确匹配**，唯一真源是 `corpus/toolchain.toml`，检查入口是 `corpus doctor`。升级工具的正确做法是改那张表并重跑全量验收，不是放宽比对。

验收步骤：

1. **确定性门禁**：重新生成，SHA-256 必须一致；
2. **合法性核验**：legal fixture 必须通过结构检查；malformed fixture 必须以 manifest **声明的方式**失败（失败方式不符也是失败——说明变异没打中目标）；
3. **几何交叉核验**：独立解析器提取的字符位置与 manifest 期望值在 §2.2 容差内一致；
4. **视觉核验**：独立渲染器出图，人工看一次，并存下参考栅格哈希供回归；
5. **裁定与记录**：任何不一致按 §2.1 裁定并留痕。

#### oracle 自身的性质（2026-08-21 实测，写 fixture 时必须知道）

两个解析器不是等价的两份「正确答案」，它们各自只在某些量上可信。以下五条是写第一批 fixture 时逐条测出来的，每条都改变了门禁的实现或某份 fixture 的检查清单：

| # | 观测 | 影响 |
|---|---|---|
| O1 | poppler `-bbox-layout` 的 `<page width height>` **不应用 `/Rotate`**，而同一份输出里的**坐标应用了**。同一份 300×200 的页面，五个 `/Rotate` 取值下它一律报 300×200。 | 页面尺寸交叉核验改为比对**有效框尺寸**；`/Rotate` 的正确性改由渲染器出图的像素朝向（`pdftoppm` 确实应用 `/Rotate`）与跨 fixture 的 `group/geom-equal` 两条一起担保。 |
| O2 | poppler 的块**顺序**在 `/Rotate 270` 与 `-90` 下会翻转；`0`/`90`/`180` 正确。mutool 在五个取值下都按 content stream 次序报出，且两者的**坐标**在五个取值下换算回页面空间后逐块相同。 | `unit-geom-01-rotate-270` 与 `-neg90` 不声明 `reading-order` 检查，改在 manifest 里留 `[[adjudication]]`。阅读顺序在 M0 由模型给出，poppler 只是旁证。 |
| O3 | mutool 的 stext 在 `/Rotate 270` 下**不把多行聚合成段落**（四行正文报成四个块）；`0`/`90`/`180` 正常。 | GEOM 系列 fixture 的正文缩成两行、彼此相距 60bp——那批 fixture 的被观察量是页面几何，行→段聚合不该混进来（§2.4）。 |
| O4 | poppler 的版面分析在两栏的块**逐行对齐成网格**时退化为行优先；栏间距 20pt→80pt 均无变化，判据是行对齐而非栏间距。 | `unit-order-01/02/03` 的纵向槽位改成真实双栏正文的样子（只有首行对齐）。网格式对齐是表格版面，本不该出现在双栏正文 fixture 里。 |
| O5 | 栏间距压到 8pt（约 0.8 字宽）时 poppler 把整页并成**一个**跨栏的块（`unit-layout-08` 上是 11 行、x 跨度 30..519.76pt），mutool 仍给出干净的 6 块。O4 说的是「栏间距在 20–80pt 区间内不影响」，这条说的是「到某个下限就影响」，两者不矛盾。 | `unit-layout-08-narrow-gutter` 的块划分无法双解析器裁定，改用 `glyphs`。分歧本身就是 LAYOUT-08 要暴露的现象。 |
| O6 | MuPDF `stext` 的字符 quad 横向覆盖 advance 范围，不是字形轮廓的真实墨迹外接框；两者对 `MIMUS` 等文本可明显不同。 | 精确 fixture 的 visual bbox 不再用 `stext` quad 裁定。manifest 先按钉死字体轮廓手推，验收时用 `mutool draw -F svg` 导出的字形 path，并计算直线、二次及三次 Bezier 的真实极值；空白与注释外观不计入文本墨迹。现实排版 fixture 继续把 stext quad 作为 §2.1 双解析器裁定下的近似值。 |

还有一条关于字体而非解析器的：mutool 的字形 quad（墨迹盒）会比 poppler 的词盒（度量盒）略大，Typst 内嵌字体下 < 0.05pt，TeX Live 的 Computer Modern Type1 下约 0.22pt。因此 manifest 把「两个解析器报同一个量」的容差（`tolerance_pt`）与「墨迹盒允许越出度量盒的余量」（`ink_margin_pt`）分开声明——用同一个数去卡两件事，要么放松了 x 跨度判据，要么把正常的字体度差判成解析器不一致。

### 2.9 断言必须独立可观察

manifest 中的"期望行为"不得写成"效果好"、"不崩溃"、"排版正确"这类不可判定的表述。合格的断言形如：

- IL 结构断言：`page[0].paragraphs.len() == 3`、`第 2 段的 composition 中 formula 数 == 1`
- 几何断言：译文框不越出所属 layout 框；译文框与 `display_formula` 框不相交
- 内容断言：送往翻译后端的请求中不包含 `reference` 区域的文本；占位符集合翻译前后相等
- 结构守恒断言：输出 PDF 的书签数、注释数与输入相等
- 退出码断言：畸形输入以分类退出码 2 退出，stderr 含指定错误类别

### 2.10 BabelDOC 只作为 differential signal

BabelDOC 的输出**不得作为唯一正确性 oracle**。它是参考实现而非规范，且本文档的全部素材恰恰来自它的 354 处异常捕获——那些代码本身就是它出过错的证据。

允许的用法：把 BabelDOC 在同一 fixture 上的行为记录下来作为**差分信号**——二者不一致时，触发一次人工裁定（谁对谁错都有可能），裁定结论写入 manifest。不允许的用法：把 BabelDOC 的输出直接当作期望值。

### 2.11 Fixture ID 体系

格式：`<class>-<domain>-<nn>-<slug>`，例如 `unit-geom-03-rotate-90`、`mal-parse-01-null-contents`、`intg-01-two-column-formula`。

- `class`：`unit` / `mal`（malformed，仍是 unit 的一种，但独立前缀便于批量处理）/ `intg`
- `domain`：通常与需求矩阵的 case ID 前缀一致（`parse`/`stream`/`font`/`cmap`/`xobj`/`geom`/`write`/`doc`/`para`/`form`/`table`/`order`/`layout`/`type`/`scan`）；`base` 是 §4.1 三份合法父本专用的保留 domain
- 编号在 domain 内单调递增，**永不复用**；fixture 作废时保留编号并标注 retired

不使用旧语料的 `01_…`–`23_…` 编号体系。

---

## 3. 失效模式需求矩阵

### 3.0 读法

**来源**：BabelDOC 主链路源码逐文件分析。**"354 个 except"不等于 354 份 PDF**——同一语义在多处出现算一个失效模式，源码位置列多条即可。排除项：vendored pdfminer、`.venv`/`dist`/缓存目录、以及无法由 PDF 输入触发的类别（网络、RPC、翻译后端、缓存、日志、取消、CLI 参数、资产下载）。

**每条的字段**：源码位置 / 触发条件 / V1 相关性 / 最小构造 / 预期行为（独立可观察）/ oracle / 优先级 / 合法性。

**优先级口径**（与 `docs/02-milestones.md` 对齐）：

- **M0** 仅限服务于三个风险探测实验的 case（模型能力与阅读顺序验证、走查对齐、增量写回）；
- **M1** 最小端到端必须正确的基础能力；
- **M3** 质量攻坚期的启发式精修。

调研 agent 曾把若干扫描件判定 case 标为 M0，本文档按上述口径下调为 M1——它们是 V1 能力而非风险探测项。

**V1 相关性**三档：**相关** / **待验证红利**（PP-DocLayoutV3 可能直接解决，须用语料证明）/ **不相关**（V2 或默认关闭）。

**规模**：本矩阵共 **129 个失效模式**——解析与写回类 68 个（§3.1：PARSE 11 / STREAM 11 / FONT 10 / CMAP 9 / XOBJ 10 / GEOM 5 / WRITE 8 / DOC 4），版面与启发式类 61 个（§3.2–3.8：LAYOUT 8 / ORDER 4 / PARA 16 / FORM 14 / TABLE 3 / TYPE 12 / SCAN 4）。按优先级：**M0 27 个、M1 60 个、M3 42 个**（另有 ORDER-02、FORM-04、TYPE-01、DOC-04 四条跨两个优先级，按其首要标记归类）。

**AST 提取统计**（非 vendored 代码）：**263 个 `except` 处理器**，其中捕获裸 `Exception` 的占 **55.5%**；按行为分类，静默类（`pass` / 只记日志 / 赋兜底值 / 返回默认值 / `continue`）合计 **197 个，占 74.9%**。另有 631 处非 except 的防御性分支（`guard-return` 362、`clamp` 79、`guard-continue` 67 等）。

> **对调研报告"354 个 except"的更正**：两个数字来自同一棵源码树（HEAD `79146c3`），差异纯粹是计数方法——`354` 是 grep 匹配 `except` **子串**的行数（含注释、字符串与散文中的该词），`263` 是行首 `except` **语句**的实际条数。已复核：同一 HEAD 上两种口径分别得到 354 与 263。**以 263 为准**；`docs/01-research.md` 中的 354 保留为历史记录并已就地标注。这不改变该报告的结论——263 个异常处理器加 631 处防御性分支，仍然印证"缺回归网就得靠防御性代码兜底"的判断。

**一个决定性的发现**：BabelDOC 内建了一整套"严格模式"分支（`runtime_settings.py:1` 的 `STRICT` 与 vendored pdfminer 的同名开关），但**生产路径下全部关闭**。它的绝大多数畸形输入处理因此走的是静默兜底而非报错——这正是 mimus"畸形 PDF fail-fast + 三层降级"哲学与参考实现的根本分歧点，也是本矩阵大部分条目的根源。矩阵中反复出现的"**反面教材**"标记，指的就是这类"BabelDOC 静默通过、mimus 必须显式失败"的 case。

### 3.1 解析与写回（PARSE / STREAM / FONT / CMAP / XOBJ / GEOM / WRITE / DOC）

#### PARSE — 文件结构、xref、对象、流解码

**PARSE-01 · null 间接对象**
- 源码：`high_level.py:453-473`（`fix_null_xref`）、`441-450`。读不出的对象**一律改写成 `[]`**，包括本来完好的对象
- 触发：xref 引用了不存在的对象号，或对象体是字面 `null`
- V1：相关——lopdf 对 dangling reference 默认给 `Object::Null`，需显式决定语义
- 构造：页面 `/Resources` 里 `/Font << /F1 99 0 R >>`，obj 99 不存在
- 预期：解析为 Null 不 panic；出现在关键位置（`/Contents`、在用字体）→ `DanglingRef` 分类错误并 fail-fast；非关键位置（`/Annots`）→ 记录并透传
- oracle：退出码 + 错误分类 · **M1** · 故意畸形

**PARSE-02 · 页面对象为 null 被替换成空白页**
- 源码：`high_level.py:441-450`。`delete_page` + `insert_page` 塞入一张全新空白页
- 触发：页树叶子指向不存在/为 null 的对象
- V1：相关——"静默换成空白页"正是 mimus 失败哲学禁止的行为
- 构造：`/Kids [4 0 R 99 0 R]`，obj 99 不存在
- 预期：报 `BadPageTree{index}` 拒绝整篇；**不得**静默生成空白页
- oracle：退出码 + 错误分类 · **M1** · 故意畸形

**PARSE-03 · ASCII85 / LZW 流预解码**
- 源码：`high_level.py:464-469`。整篇扫 xref，把这两种 filter 的流解码后原地重写（注释：`# make pdfminer happy`）
- 触发：任意流用 ASCII85 或 LZW 编码
- V1：相关——mimus 走查要读原始 content stream 字节，必须自己支持 ASCII85Decode / LZWDecode（含 EarlyChange）/ ASCIIHexDecode / RunLengthDecode，否则同样漏文字
- 构造：三份——`/Filter /ASCII85Decode`；`/Filter [/ASCII85Decode /FlateDecode]` 级联；`/LZWDecode` 带 `/DecodeParms << /EarlyChange 0 >>`
- 预期：三者解出的 content stream 与未压缩版**逐字节相同**；级联按数组顺序应用；EarlyChange 0 与 1 结果不同且都正确
- oracle：与独立解析器的解压输出逐字节 diff · **M0**（走查前提）· 合法

**PARSE-04 · 多个 /Contents 流的边界拼接**
- 源码：`high_level.py:476-493` 写回用 `b" "` 连接；解析侧 `page_content_access.py:26-27` 用 `b"\n"` 连接
- 触发：`/Contents` 是数组且一个 token 跨流边界被切断——PDF 中合法但常见的坑
- V1：相关——tokenizer 必须在流边界插入空白；写回时须保持 `/Contents` 数组语义
- 构造：(a) 数字跨界：obj5 结尾 `1 0 0 1 10`、obj6 开头 ` 20 cm`；(b) 字符串跨界：obj5 结尾 `(He`、obj6 开头 `llo) Tj`
- 预期：(a) 走查得到 `10` 与 `20` 两个独立操作数（不是 `1020`）；(b) 报 `UnterminatedString` 并页级降级
- oracle：走查坐标与 PDFium 文本提取对比 / 错误分类 · **M0** · 合法(a) / 故意畸形(b)

**PARSE-05 · /Filter 存成间接引用**
- 源码：`high_level.py:480-484`
- 触发：流字典的 `/Filter` 值是间接引用（合法但罕见）；`/Length`、`/DecodeParms` 同理
- V1：相关——lopdf 取这些键时必须 resolve
- 构造：content stream 字典写成 `<< /Length 8 0 R /Filter 7 0 R >>`
- 预期：正常解码；`/Length` 与实际字节数不符时以 `endstream` 定位为准并记 warning
- oracle：解码结果与独立解析器一致 · **M1** · 合法

**PARSE-06 · 对象体局部损坏时逐键重建**
- 源码：`pymupdf_object_access.py:116-142`。逐键重建并**静默丢弃所有值为 null 的键**，同时丢掉 `/Length` 与流数据
- 触发：对象体语法局部错误（字典中混入非法 token）
- V1：部分相关——这是 PyMuPDF 特有的两级 API 兜底，但"局部损坏时尽力恢复还是 fail-fast"是 mimus 必须表态的设计点
- 构造：字典中间插入裸 `>` 或 `}`
- 预期：报 `ObjectSyntax{objid, offset}` 并 fail-fast（V1 不做逐键恢复）
- oracle：退出码 + 错误分类 · **M3** · 故意畸形

**PARSE-07 · 类型不符时静默默认值**
- 源码：`object_primitives_runtime.py:26-80`，`STRICT = False`。`dict_value → {}`、`list_value → []`、`stream_value → 空流`、`int/num_value → 0`。严格版（`resolved_object_access.py:33-49`）本来是 `raise TypeError`
- 触发：`/Widths` 是标量、`/FontDescriptor` 是数组、`/ToUnicode` 指向字典而非流
- V1：**高度相关**——空流意味着 ToUnicode 变空映射、全部字符退化成 `(cid:N)`；`num_value → 0` 意味着字号或宽度变 0。典型的"静默产出坏译文"源头
- 构造：Type0 字体的 `/ToUnicode` 指向一个字典对象；变体 `/Widths 500`
- 预期：报 `ToUnicodeNotAStream{font}` → 段级降级（该字体的段保留原文），页面其余部分正常翻译
- oracle：输出中该字体文本与输入逐字节一致 + 错误分类 · **M1** · 故意畸形

**PARSE-08 · 嵌套深度上限 128**
- 源码：`object_parser.py:13,33-36`、`tokenizer.py:92,389-392`。**真抛异常**，非静默
- 触发：深度嵌套数组/字典，或 content stream 里 `TJ` 数组套数组
- V1：相关——**自写 tokenizer 若用递归下降，深嵌套即 stack overflow，在 Rust 里是不可捕获的 abort**
- 构造：一个对象含 512 层 `[` + 512 层 `]`，从页面字典引用它
- 预期：固定深度（建议同为 128）返回 `Err(NestingTooDeep)`；进程不 abort、不 stack overflow
- oracle：错误断言 + 退出码是正常错误码而非 SIGSEGV/SIGABRT · **M0**（走查安全红线）· 故意畸形

**PARSE-09 · 缺失 /Contents 静默空页**
- 源码：`pymupdf_object_access.py:97-105`、`pymupdf_page_view_access.py:28-30`。返回空 tuple，**无任何日志**
- 触发：页面无 `/Contents`（合法空白页）或 `/Contents` 为 null
- V1：相关——需与扫描件检测区分：0 文字对象 + 有图像 = 扫描件；0 文字对象 + 空 = 空白页
- 构造：3 页 PDF 第 2 页无 `/Contents`；变体：第 2 页 `/Contents` 只有一个 Image XObject
- 预期：空白页原样透传、不参与翻译、不报错；只有图像的页计入扫描页统计
- oracle：输出该页对象树与输入等价 + 页数不变 · **M1** · 合法

**PARSE-10 · 页树属性继承与环检测**
- 源码：`pymupdf_object_access.py:65-94`。`visited` 集合防环；找不到返回**空资源字典**
- 触发：`/Resources`、`/MediaBox`、`/CropBox`、`/Rotate` 定义在 `/Pages` 上（合法且常见）；恶意情况 `/Parent` 自环
- V1：相关——V1 必须实现可继承属性的向上查找 + 环检测
- 构造：(a) 页面无 `/Resources`、`/Pages` 上有；(b) 三层页树中间层有；(c) `/Parent` 自环
- 预期：(a)(b) 字体与坐标解析结果与非继承版一致；(c) 检测到环返回 `PageTreeCycle`，不死循环
- oracle：坐标与 PDFium 对齐 / 超时 watchdog + 错误分类 · **M1**（(c) 属 M0 安全项）· 合法(a,b) / 故意畸形(c)

**PARSE-11 · 保存失败三级降级（最后一级丢弃全部结构信息）**
- 源码：`high_level.py:88-94`（`save` → `ez_save`）、`120-131`、`97-117`（逐页搬运重建，**丢弃书签/表单/OCG/附件**）
- 触发：对象图有循环引用、ObjStm 损坏、或底层拒绝增量保存
- V1：**高度相关（反面教材）**——mimus 明确要求书签/注释/表单/OCG 原样透传，不能有这一级降级，但必须能识别触发它的输入并 fail-fast
- 构造：`/ObjStm` 内声明的对象数与实际不符；变体：`/Outlines` 的 `/Next` 自环
- 预期：输出的 `/Outlines`、`/AcroForm`、`/OCProperties` 与输入**逐字节相同**（增量写回不重写）；ObjStm 损坏则报 `ObjectStream` 并拒绝
- oracle：独立解析器对比子树 + **结构断言：输出文件前 N 字节 == 输入文件全部字节（增量特征）** · **M0** · 故意畸形

#### STREAM — content stream 操作符走查

**STREAM-01 · 未知操作符导致整页硬失败**
- 源码：`interpreter.py:319-320`（不在支持集即抛 `UnsupportedOperatorError`）。注意 `d0`/`d1` 在关键字表里但**不在**支持集里
- 触发：`BX…EX` 区间内的厂商扩展操作符；Type3 CharProc 必然出现的 `d0`/`d1`；废弃的 `PS`
- V1：**高度相关**——`BX/EX` 的语义就是"括起来的未知操作符必须忽略"，硬失败是错的
- 构造：正常页 + `BX /Foo 1 2 3 SomeVendorOp EX`；变体：Type3 CharProc 以 `10 0 0 0 10 10 d1` 开头
- 预期：`BX/EX` 内未知操作符被跳过且不计错；`d0/d1` 在 Type3 上下文中用于字形度量；区间外的未知操作符记 warning 并跳过（不终止页）
- oracle：走查字符列表与 PDFium 对齐 + warning 计数 · **M0** · 合法

**STREAM-02 · 操作数个数不符沿用 pdfminer 栈语义**
- 源码：`interpreter.py:794-810`。少于期望 → 抛错整页失败；**多于期望则只消费尾部 N 个，多余前缀留在栈上给下一个操作符**（源码注释自陈是在匹配 pdfminer 语义）
- 触发：`1 2 3 4 5 6 7 cm`、`0.5 0.5 g`、`Tj` 前遗留数字
- V1：**高度相关**——这是走查对齐的核心分歧点（Acrobat 的行为与 pdfminer 的"跨操作符继承"不同）
- 构造：`q 1 2 3 4 5 6 7 cm BT /F1 12 Tf (X) Tj ET Q`；变体 `1 0 0 1 100 cm`（缺一个）
- 预期：明确选定一种语义并断言字符最终坐标；操作数不足时跳过该操作符（CTM 不变），页面其余部分继续，记 `Arity` warning
- oracle：字符坐标与独立渲染器的逐操作符 CTM trace 对齐 · **M0** · 故意畸形

**STREAM-03 · 路径操作符缺数字导致路径段静默丢弃**
- 源码：`interpreter.py:827-850` + `418-476`。凑不够数字就**静默丢弃该路径段**，无日志
- 触发：`/Name 100 200 m`、`100 m`、`(str) 10 20 l`
- V1：相关——mimus 要保留矢量图形（表格线、公式框）原样，路径段静默丢失会造成视觉差异
- 构造：一个矩形边框，把其中一条 `l` 写成 `200 l`（缺 y）
- 预期：检测到操作数不足 → 该页进入"图形不可靠"状态 → 页级降级；不得静默改变路径
- oracle：光栅对比（该区域像素差 == 0）· **M3** · 故意畸形

**STREAM-04 · `Q` 多于 `q` 时静默忽略**
- 源码：`interpreter.py:332-336`。栈空时什么都不做但仍发出恢复事件；底层 `state.py:209-212` 本来会抛 underflow，被这层 guard 挡掉
- 触发：`Q` 多于 `q`（流被截断或拼接错误），或 `q` 多于 `Q`（结尾未平衡）
- V1：**高度相关**——mimus 自维护 CTM 栈，栈不平衡直接毁掉后续所有字符坐标
- 构造：`q 1 0 0 1 50 50 cm BT … ET Q Q Q BT /F1 12 Tf 100 700 Td (After) Tj ET`
- 预期：多余 `Q` 后 CTM 保持 base CTM，`(After)` 绝对坐标 == (100,700)；栈不平衡计入 warning；页尾未闭合的 `q` 不影响已产出字符
- oracle：字符坐标 + 逐操作符 CTM trace diff · **M0** · 故意畸形

**STREAM-05 · 文本操作符出现在 BT/ET 之外**
- 源码：`interpreter.py:686-688`（抛 ValueError → 整页失败）
- 触发：`Tj`/`TJ`/`'`/`"` 在 `BT` 之前或 `ET` 之后；`BT` 嵌套。拼接工具产物中不罕见
- V1：相关
- 构造：`/F1 12 Tf 100 700 Td (Orphan) Tj`（无 BT）；变体 `BT … BT … ET`
- 预期：孤立文本操作符 → 记 warning，按隐式 `BT`（Tm 为单位阵）处理或明确跳过——二选一但须**页级一致**且可断言；嵌套 `BT` 按规范重置 Tm
- oracle：与 PDFium 提取的字符集合做集合比较（是否漏字）· **M1** · 故意畸形

**STREAM-06 · 数字与操作符粘连的复合关键字拆分**
- 源码：`tokenizer.py:629-697`。按复合关键字表贪婪最长匹配切分；`10.5.3` 拆成 `10.5` + `.3`
- 触发：`20cm`、`700Td`、`0.5g`、`10.5.3`——缺空格的畸形流
- V1：**高度相关**——纯 tokenizer 行为，必须逐 case 对齐否则坐标全错
- 构造：`BT /F1 12Tf 100 700Td (A) Tj ET`；变体 `1 0 0 1 10 20cm`；变体 `.5.5 g`
- 预期：三例各产出确定的 token 序列（写入 manifest）；`(A)` 最终绝对坐标为确定值
- oracle：token 序列快照 + 坐标与独立解析器对齐 · **M0** · 故意畸形（Acrobat 容忍）

**STREAM-07 · 十六进制字符串奇数长度补 0**
- 源码：`tokenizer.py:511-522`（补 `0`、剥空白，符合规范）；ToUnicode 侧 `to_unicode_parser_runtime.py:84-91` 解析失败直接丢弃该 token
- 触发：`<48656C6C6F2>`（奇数位）、`<48 65 6C>`（带空白）、`<>`（空串）
- V1：相关——Identity-H 的 2 字节 CID 遇奇数 hex 会错位一整串
- 构造：Type0 + Identity-H，`<00480065006C006C006F0> Tj`
- 预期：补 0 后解出确定的 CID 序列（写入 manifest）；含非 hex 字符（`<48G5>`）→ 报 `BadHexString`
- oracle：CID 序列断言 + PDFium 文本对比 · **M1** · 合法（补 0 是规范行为）/ 故意畸形（非 hex）

**STREAM-08 · 未闭合复合结构**
- 源码：`tokenizer.py:509, 517, 531-533, 547-563`。有一条恢复开关 `recover_trailing_composites`，**默认 False 且全代码库无人打开**
- 触发：content stream 被截断在字符串/数组/字典中间——`/Length` 错误或流被裁剪，真实世界常见
- V1：相关
- 构造：content stream 以 `(Hello` 结尾后直接 `endstream`；变体：`/Length` 声明比真实流短 10 字节
- 预期：报 `Unterminated{kind}` → **页级降级**：该页保留原 content stream 原样输出，其余页正常翻译，文档不失败
- oracle：该页 content stream 输出与输入逐字节相同 + 其他页有译文 + 退出码 0 + warning 计数 1 · **M1**（三层降级验证）· 故意畸形

**STREAM-09 · 内联图像 BI/ID/EI 的结束定位**
- 源码：`tokenizer.py:190-206, 224-277, 350-355`。优先用 `/L` 精确跳过；否则扫描 `EI` 并要求其前后是空白、且后随 token 在一个 **30 个操作符的白名单**内；找不到则抛错
- 触发：图像二进制数据里恰好含 ` EI ` 字节序列（未压缩灰度图很容易撞上）；或图像后跟白名单外的操作符
- V1：**高度相关**——这是自写 tokenizer 最经典的坑，且没有完美解，必须实现 `/L` 优先 + 按 `/W /H /BPC /CS` 计算期望字节数
- 构造：`BI /W 8 /H 8 /BPC 8 /CS /G ID <64 字节，其中第 20–23 字节为 0x20 'E' 'I' 0x20> EI`；变体：不带 `/L` 且 `EI` 后跟 `sh`
- 预期：按 `W×H×BPC×分量/8` 计算长度精确跳过，第一例图像数据长度 == 64；后续 `Tj` 字符正常产出；无法计算时（如 `/F /DCTDecode`）退回扫描并记 warning
- oracle：光栅对比 + 走查操作符总数断言 · **M0** · 合法

**STREAM-10 · 渲染模式 3 / 7 的文本处理**
- 源码：`interpreter.py:689-702`。`7 Tr`（clip-only）的字符**不进 IL** 但推进位置；`3 Tr`（不可见）的字符**照常进 IL 并被翻译**
- 触发：`7 Tr` 用于文字挖空显示图片；`3 Tr` 是 OCR 图层的标志性特征
- V1：**高度相关**——V1 拒绝扫描件，而 `3 Tr` 正是其特征；`7 Tr` 剪裁文本被翻译会毁掉视觉效果
- 构造：(a) `BT 7 Tr /F1 40 Tf 100 700 Td (MASK) Tj ET` + 覆盖该区域的图像；(b) 全页图像 + `BT 3 Tr … (invisible ocr text) Tj ET`
- 预期：(a) 识别为剪裁路径，不翻译、原样透传，光栅像素差 == 0；(b) 检测为扫描件，返回 `ScannedPdf` 分类错误并拒绝
- oracle：光栅 diff / 退出码 + 错误分类 · **M1** · 合法

**STREAM-11 · TJ 数组元素类型错误**
- 源码：`interpreter.py:661-672`（抛 ValueError → 整页失败）
- 触发：`[(A) /Name (B)] TJ`、`[(A) [1 2] (B)] TJ`、`(Str) TJ`
- V1：相关
- 构造：`BT /F1 12 Tf 100 700 Td [(Hel) /X (lo)] TJ ET`
- 预期：跳过非法元素并记 warning，`Hel` 与 `lo` 都产出且字距把 `/X` 当作 0；或整个 `TJ` 跳过——二选一并断言字符序列
- oracle：字符序列 + x 坐标断言 · **M1** · 故意畸形

#### FONT — 字体、度量、嵌入

**FONT-01 · 引用不存在的字体名时合成兜底字体**
- 源码：`resources.py:78-111`。凭空造一个 Type1 字体（`font_id_temp = "UNKNOW"`）。源码注释自陈是在保持 pdfminer 的"文本提取不中断"行为
- 触发：`/F1 12 Tf` 但资源里没有 `/F1`；或名字用了 `#` 十六进制转义（`/F#31` vs `/F1`）
- V1：**高度相关**——合成假字体使宽度几乎必然错，是"静默产出坏译文"的直接来源
- 构造：(a) 资源里是 `/F2`、content 里用 `/F1`；(b) 资源里是 `/F#31`、content 里是 `/F1`（应视为同名）
- 预期：(b) 正确匹配，字符宽度与非转义版完全一致；(a) 报 `MissingResource{name}` → 段级降级保留原文，**不合成假字体**
- oracle：(b) 两版输出逐字节相同；(a) 输出该段文本 == 原文 · **M1** · 合法(b) / 故意畸形(a)

**FONT-02 · 标准 14 度量库覆盖文件里的 /Widths**
- 源码：`active_direct_font_backend.py:347-358`。内置度量**优先级高于** PDF 里声明的 `/Widths`；有子集前缀时才回退到 `/Widths`
- 触发：`/BaseFont /Helvetica` 同时带自定义 `/Widths`
- V1：相关——字符宽度直接决定段落切分与排版；mimus 用 PDFium 取度量，须确认其处理是否一致
- 构造：`/BaseFont /Helvetica /FirstChar 65 /LastChar 90 /Widths [1000 ×26]`（故意与标准值 722 不符）+ `(AAAA) Tj`
- 预期：明确选定优先级（建议文件 `/Widths` 优先、缺失才用内置度量），断言四个字符 x 间距 == 12pt 而非 8.664
- oracle：与 PDFium 字符坐标对齐（PDFium 遵循规范：`/Widths` 优先）· **M0**（走查对齐 + PDFium trait 边界验证）· 合法

**FONT-03 · /Widths 缺失导致全 0 宽度**
- 源码：`active_direct_font_backend.py:576-579` + 消费端 `86-98/146-158/224-236`。查不到就用 `MissingWidth`，而它默认为 **0**
- 触发：简单字体无 `/Widths`；或 `/Widths` 是指向 null 的间接引用
- V1：**高度相关**——所有字符宽度为 0 会让它们叠在同一 x 坐标，段落识别彻底崩溃却**不报任何错**。零宽度是"坏译文"的最强前兆信号
- 构造：TrueType 简单字体，`/FontDescriptor` 完整但**无 `/Widths`**，写 `(WWWW) Tj`
- 预期：从 PDFium 取字形 advance 兜底，或报 `NoWidths{font}` 段级降级。断言四个 `W` 的 x 坐标严格递增且间距 > 0
- oracle：坐标间距断言 + 与 PDFium 对齐 · **M1** · 故意畸形

**FONT-04 · FontDescriptor 缺失与 Descent 符号**
- 源码：`active_direct_font_backend.py:341, 383-387`（缺失变 `{}`；正 Descent 静默取负）；CID 版 `547-556` 的异常吞掉后 ascent/descent 停在 0
- 触发：`/FontDescriptor` 缺失；`/Descent 210`（正数，规范要求负数）
- V1：相关——字符 bbox 决定行分组与版面对齐
- 构造：TrueType 字体，`/Descent 210 /Ascent 0`
- 预期：断言字符 bbox 的 y0 == baseline − 210×size/1000（取负后使用），且有 warning 记录符号被修正
- oracle：bbox 数值断言 + 与 PDFium 字符盒对比 · **M3** · 故意畸形

**FONT-05 · 嵌入字体解析失败导致全字符退化为 CID 字面量**
- 源码：`active_direct_font_backend.py:509-514`（`except Exception: ttf = None`）；`font_data_runtime.py:38-49` 表目录读不全时静默留下部分表
- 触发：`/FontFile2` 被截断（`/Length1` 撒谎）、实际是 CFF、或是加密 OpenType
- V1：相关——mimus 用 PDFium 取度量，但 ToUnicode 缺失时的 cmap 反查需自行实现
- 构造：Type0/CIDFontType2 + Identity-H，**无 `/ToUnicode`**，`/FontFile2` 只保留前 100 字节
- 预期：报 `EmbeddedFontUnparsable{font}` → 段级降级保留原文；**输出中绝不出现 `(cid:` 字面量**
- oracle：输出文本子串断言 + 该段 == 原文 · **M1** · 故意畸形

**FONT-06 · Type3 FontMatrix / FontBBox 异常**
- 源码：`active_direct_font_backend.py:429-447`。`/FontMatrix` **无默认值**，缺失时六元解包直接抛 ValueError 且未被捕获；`/FontBBox` 有三重兜底到零。`type3.py:61-97` 与 `type3_font_metrics.py:41,50,53` 在 em 高度非法时退回未缩放字号
- 触发：Type3 缺 `/FontMatrix`；`/FontMatrix [0 0 0 0 0 0]`；`/FontBBox [0 0 0 0]`
- V1：**高度相关**——Type3 在数学/化学论文常见，字号缩放通常是 0.001 量级，算错即差一个数量级
- 构造：三份——正常 `/FontMatrix [0.001 0 0 0.001 0 0]`（CharProc 以 `1000 0 0 0 750 750 d1` 开头）、缺 FontMatrix、退化矩阵
- 预期：正常例断言字符渲染高度为确定 pt 值；缺失/退化 → `Type3NoMatrix` 段级降级，不 panic、不除零
- oracle：光栅 diff / 错误分类 · **M1** · 合法（正常）/ 故意畸形（另两个）

**FONT-07 · Type0 子孙字体缺 Subtype 时退化成简单字体**
- 源码：`active_direct_font_backend.py:320-334`。源码注释自陈是在沿用 pdfminer 的 Type1 兜底。后果：多字节判定变假，**2 字节 CID 被当成 2 个单字节 CID**
- 触发：`/DescendantFonts` 内字典缺 `/Subtype`，或写成未知的 `/CIDFontType9`
- V1：**高度相关**——最隐蔽的静默数据损坏之一：文本数量翻倍且全错，无报错
- 构造：Type0 + Identity-H，子孙字典去掉 `/Subtype`，写 `<00480065006C006C006F> Tj`
- 预期：报 `BadDescendantSubtype` → 段级降级；或按 `/CIDSystemInfo` 存在推断为 CID 字体正确解出。断言字符数 == 5（不是 10）
- oracle：字符数 + Unicode 序列，与 PDFium 对比 · **M1** · 故意畸形

**FONT-08 · 嵌入 Type1 的 Encoding 从字体头部反解**
- 源码：`active_direct_font_backend.py:370-381`（定位 `/Encoding` 解析，任何异常都退回 StandardEncoding）；`font_data_runtime.py:65-109` 逐 token 静默跳过
- 触发：Type1 无 `/Encoding` 字典、带 `/FontFile`，其明文头含自定义 `dup 65 /Alpha put`
- V1：部分相关——V1 用 PDFium 取字符，PDFium 自行处理内嵌 Type1 编码；仅当 mimus 自算 Unicode 时才需处理
- 构造：Type1 内嵌字体，头部含 `dup 65 /Alpha put`，无 `/Encoding` 字典，写 `(A) Tj`
- 预期：字符 Unicode 解为 `α`；不支持则报 `Type1EmbeddedEncoding` 段级降级
- oracle：与 PDFium 文本提取比对 · **M3** · 合法

**FONT-09 · 字体名不是有效 UTF-8**
- 源码：`il_creater.py:699-703`、`il_creater_active.py:677-680`（转 `BASE64:` 前缀）；上游 `active_direct_font_backend.py:39-54` 还有两层兜底，其中一层会把 Python repr 泄漏进字体名
- 触发：`/BaseFont` 含 `#` 转义出的非 UTF-8 字节（如 GBK 中文字体名）——中日文 PDF 常见
- V1：部分相关——字体名参与"是否已嵌入 Noto"的匹配，名字失真可能导致重复嵌入
- 构造：`/BaseFont /#B7#A9#CC#E5` 的 TrueType 字体
- 预期：字体名以字节序列保存不做有损转换；展示时按 latin-1 lossy；字体去重按对象号而非名字
- oracle：输出中该字体未被重复嵌入（对象计数断言）· **M3** · 合法

**FONT-10 · 字形 bbox 取不到时返回全 0，且默认 bbox 被当作无效过滤**
- 源码：`cidfont.py:43-53`（异常与无轮廓都返回 `0,0,0,0`）；`il_creater.py:796-807` 把 `(0,0,500,698)` 与 `(0,0,0,0)` 都当作"默认值"丢弃——**真有这个 bbox 的字形会被误伤**
- 触发：字形索引超出字体的字形总数；空格等无轮廓字形；字体加载失败
- V1：部分相关——mimus 经 PDFium 取字形轮廓/字符盒，需定义"取不到 bbox"的语义
- 构造：子集化 TrueType，content 里用超出字形总数的 CID
- 预期：取不到时返回 `None` 而非全 0，下游按 advance 估算并标记 `bbox_estimated = true`
- oracle：IR 字段断言 · **M3** · 故意畸形

#### CMAP — 编码、ToUnicode、CID

**CMAP-01 · 预定义 CMap 不在内置清单时返回空 CMap**
- 源码：`cid_cmap_runtime.py:76-81`（`STRICT=False` 下返回空 `CMap()`）；`cmap_secure_loader.py:33-59` 只允许清单内文件并校验哈希
- 触发：`/Encoding /UniJIS-UCS2-HW-H`、`/GBK-EUC-V` 等清单外的预定义 CJK CMap
- V1：**高度相关**——解码产出 0 个 CID，**该字体所有文字凭空消失且无任何日志**。预定义 CJK CMap 在中日韩 PDF 中普遍存在，静默丢字是最严重的失效
- 构造：Type0 + `/Encoding /GBK-EUC-H` + CIDFontType0 + `/CIDSystemInfo << /Registry (Adobe) /Ordering (GB1) /Supplement 2 >>`，写一串 GBK 双字节
- 预期：支持则解出正确 CID 数量；不支持则报 `UnsupportedPredefined{name}` 并显式降级报告，**绝不静默产出 0 字符**
- oracle：与 PDFium 字符计数对比 · **M1** · 合法

**CMAP-02 · /Encoding 缺失与名字变体**
- 源码：`cid_cmap_runtime.py:17-34`（名字归一化）、`49-59`（查不到时保持 `"unknown"`）、`60-72`（`DLIdent-H → Identity-H` 别名表）
- 触发：Type0 无 `/Encoding`；`/Encoding /DLIdent-H`（某些生成器的私有别名）；名字带引号
- V1：相关
- 构造：Type0 + `/Encoding /DLIdent-H`；变体：Type0 完全无 `/Encoding`
- 预期：别名被识别为 Identity-H，字符数与标准写法一致；缺 `/Encoding` → 报 `MissingEncoding`（Type0 缺它是非法的）
- oracle：字符数 + CID 序列断言 · **M1** · 合法（别名）/ 故意畸形（缺失）

**CMAP-03 · 嵌入 CMap 流先做 UTF-8 解码**
- 源码：`active_direct_font_backend.py:499-507`（`decode("U8")` 失败即静默弃用嵌入 CMap）；`192-200` 的回退判据 `all(x > 0)` 意味着**任何映射到 CID 0 的字符会让整段回退**
- 触发：`/Encoding` 是嵌入 CMap 流且含非 ASCII 字节
- V1：相关——嵌入 CMap 必须按二进制（PostScript 语法）解析，不能先做 UTF-8 解码
- 构造：嵌入 CMap 流含 `begincidrange <0000> <00FF> 0 endcidrange` + 一个非 ASCII 注释字节
- 预期：按字节解析成功；映射到 CID 0 的字符正常产出（notdef）而非触发整段回退
- oracle：CID 序列断言 · **M3** · 合法

**CMAP-04 · ToUnicode 三级兜底链的猜测式回退**
- 源码：`cid_cmap_runtime.py:84-125`。级 2 在名字含 `"Identity"` 时启用 `IdentityUnicodeMap`——**把 CID 直接当 Unicode 码点**；全失败则字符退化为 `(cid:N)`
- 触发：Identity-H 字体无 `/ToUnicode`（子集化工具常漏）；`/ToUnicode` 指向损坏流
- V1：**高度相关**——对 Identity-H + 任意 TrueType 子集，CID 等于字形索引而非 Unicode，这个回退**几乎必然产出乱码且不报错**。这是 mimus"宁可保留原文"哲学的核心测试点
- 构造：Type0 + Identity-H + CIDFontType2 + `/Ordering (Identity)`，**无 `/ToUnicode`**，`/FontFile2` 是完整可用的子集 TTF（含 cmap 表）
- 预期：从嵌入字体的 cmap 表反查字形→Unicode；反查不到则报 `NoToUnicode{font}` → **段级降级保留原文**；绝不用身份映射猜
- oracle：与 PDFium 文本提取逐字符比对；降级例断言输出文本 == 输入文本 · **M0**（"绝不静默产出坏译文"的验收点）· 合法

**CMAP-05 · ToUnicode 内部畸形逐条静默跳过**
- 源码：`to_unicode_parser_runtime.py`。hex/int 解析失败 `continue`（`:88-97`）；范围长度不匹配 `continue`（`:105-157`）；数组与范围长度不符时**静默截断**（`:146`）；不足一组的尾部直接丢弃（`:166-169`）
- 触发：`beginbfrange <0000> <00FF> [(A) (B)]`（数组长 2 但范围 256）；`begincidrange` 元素数不是 3 的倍数
- V1：相关——部分映射会让译文里混入 `(cid:N)` 字面量
- 构造：Identity-H 字体，ToUnicode 内 `beginbfrange <0000> <00FF> [<0041> <0042>] endbfrange`，文本用 CID 0..5
- 预期：检测到长度不匹配 → 记 `BfrangeArityMismatch`；未映射 CID 触发段级降级而非输出 `(cid:N)`。断言输出不含 `(cid:` 子串
- oracle：输出文本子串断言 + warning 计数 · **M1** · 故意畸形

**CMAP-06 · 编码宽度靠正则猜 codespacerange**
- 源码：`active_direct_font_backend.py:247-274`（正则只抓**第一个** codespace 的位宽）；同一逻辑重复于 `il_creater.py:705-733`；消费端 `pdf_creater.py:132-134` 用它决定写回时的 hex 串宽度
- 触发：混合位宽 CMap（`<00> <80>` 与 `<8140> <FEFE>` 并存的 Shift-JIS 风格）→ 正则抓到 `00` 判为 1 字节（错）→ 写回 hex 宽度错 → **译文字符全部错位或不显示**
- V1：**高度相关**——mimus 写回时嵌入自己的 Identity-H 字体（固定 2 字节）所以写侧安全，但读侧必须按完整 codespacerange 解析而非正则
- 构造：Type0 + 嵌入 CMap，codespacerange 含 `<00> <80>` 与 `<8140> <FEFE>` 两段
- 预期：读侧按完整 codespacerange 正确解析；写侧断言输出 content stream 中所有 `Tj` 的 hex 串长度都是 4 的倍数
- oracle：输出 content stream 正则断言 + PDFium 渲染文本正确 · **M0**（写回正确性）· 合法

**CMAP-07 · Differences 里不可映射的字形名**
- 源码：`active_direct_font_backend.py:596-603`（`except (KeyError, ValueError): pass`，槽位保持基础编码原值）；`font_encoding_runtime.py:103-107` 同
- 触发：`/Differences [65 /g1 /g2 /g3]`——子集化字体常用 `gNN` 命名，此时 Differences 完全无法给出 Unicode；`uniD800` 落在代理区被拒
- V1：相关——回落成基础编码的 `A/B/C` 会让某些字符 Unicode 完全错却看起来正常
- 构造：简单字体，`/Encoding << /BaseEncoding /WinAnsiEncoding /Differences [65 /g1 /g2 /g3] >>`，写 `(ABC) Tj`
- 预期：`gNN` 无法映射 → 该字符 Unicode 标记为 unknown → 段级降级保留原文；**不得**回落成 WinAnsi 的 `A/B/C`
- oracle：IR 中该字符 `unicode` 为 None 的断言（mimus 期望比 PDFium 更严格，需单独断言）· **M1** · 合法

**CMAP-08 · CID 字面量占比 80% 才 fail-fast**
- 源码：`high_level.py:823-833` + `922-923`。这是 BabelDOC 少数正确的 fail-fast，但 79% 的情况静默放行。注意字符数为 0 时判据不成立，**0 文字对象的扫描件不会被这里拦下**
- 触发：整篇 Identity-H 无 ToUnicode（CMAP-04 的极端版）
- V1：相关（作为兜底门槛）——但 mimus 应在**段级**就拒绝，不等到 80%
- 构造：10 页 PDF，7 页正常字体、3 页 Identity-H 无 ToUnicode（CID 占比约 30%）
- 预期：3 页页级降级保留原文、7 页正常翻译、退出码 0；报告明确列出降级页号。**不因低于 80% 就静默混入乱码**
- oracle：输出中 3 页文本 == 原文、7 页有译文 + 降级报告结构断言 · **M1** · 合法

**CMAP-09 · 代理区码点静默跳过**
- 源码：`il_creater.py:308-312, 318-322`（`except Exception: pass  # to skip surrogate pairs`）；`il_creater_active_support.py:503-517` 同
- 触发：ToUnicode 把某 CID 映射到孤立代理码点（非法但真实存在，多见于 UTF-16BE 截断）
- V1：相关——Rust 的 `char` 不能表示孤立代理，必须显式处理
- 构造：ToUnicode 内 `<0041> <D800>`
- 预期：该 CID 标记为无 Unicode（不静默替换成 U+FFFD）→ 段级降级；不 panic
- oracle：IR 字段断言 + 无 panic · **M3** · 故意畸形

#### XOBJ — Form / Image XObject 与嵌套 CTM

**XOBJ-01 · Do 引用不存在的名字时静默返回空**
- 源码：`xobject_content_execution.py:60-64`（无日志）。旧路径 `pdfinterp.py:279-284` 至少还有 STRICT 分支
- 触发：`/X1 Do` 但资源里没有 `/X1`（页面被裁剪/合并后资源未同步）
- V1：**高度相关**——整块内容消失但翻译"成功"，典型静默失败
- 构造：资源里只有 `/X2`，content 里 `q 1 0 0 1 100 100 cm /X1 Do Q`
- 预期：报 `MissingResource{name}` → 页级降级保留原页；报告列出页号
- oracle：输出该页 content stream == 输入 + warning 计数 · **M1** · 故意畸形

**XOBJ-02 · 递归 Form XObject 靠对象身份去环**
- 源码：`xobject_content_execution.py:86-93`（用 `id()` 判定身份——同一对象被解析成两个实例时环检测会失效）；构建期 `prepared_resource_builder.py:173-187` 另有一层
- 触发：`/X1` 的资源里引用 `/X1` 自己；或 X1→X2→X1 互引用
- V1：**高度相关**——自写走查的必须防护，否则栈溢出
- 构造：Form XObject obj5 的 `/Resources` 含 `/X1 5 0 R`，流内容 `q /X1 Do Q`；变体 5→6→5 互环
- 预期：检测到环即停止递归，记 `Recursive{path}`，进程正常结束（无 stack overflow），断言递归深度 ≤ 环长
- oracle：退出码正常 + warning 存在 + 运行时间有界 · **M0**（安全红线）· 故意畸形

**XOBJ-03 · XObject 嵌套深度上限 64**
- 源码：`xobject_content_execution.py:25, 94-101`（超限静默丢弃，有 warning）
- 触发：链式嵌套超过 64 层（合法但极端）
- V1：相关
- 构造：70 个链式 Form XObject，最深处放一个 `Tj`
- 预期：明确深度上限（建议同为 64），超限记 warning 并跳过；断言深度 ≤64 的文本都产出、>64 的没有，warning 数正确
- oracle：字符集合 + warning 计数 · **M1** · 合法

**XOBJ-04 · Form XObject 缺 /BBox 时用单位矩阵执行（新旧路径行为不一致）**
- 源码：`prepared_resource_builder.py:142-158`。缺 `/BBox` 时仍标记为 Form 且 `matrix` 被替换成单位阵、嵌套 XObject 表被清空。**旧路径 `pdfinterp.py:287-296` 是整个跳过**——两条路径行为相反
- 触发：Form XObject 缺 `/BBox`（非法但存在），同时带非单位 `/Matrix`
- V1：**高度相关**——坐标错位的隐蔽来源
- 构造：`<< /Type /XObject /Subtype /Form /Matrix [2 0 0 2 100 100] >>`（**无 /BBox**），流内 `BT /F1 12 Tf 0 0 Td (X) Tj ET`
- 预期：报 `FormMissingBBox{name}` → 该 Do 跳过并降级；**绝不**用单位矩阵执行。若选择执行则必须应用 `/Matrix`，断言字符绝对坐标 == (100,100)
- oracle：坐标断言 + 光栅对比 · **M0**（走查对齐）· 故意畸形

**XOBJ-05 · /BBox 含 null 导致整篇崩溃**
- 源码：`prepared_resource_builder.py:169-172`。该调用**不在**外层的 try 保护内，`float(None)` 的 TypeError 一路冒泡到顶层 → 整篇失败。旧路径有专门的 null 过滤，且源码注释给出了真实样本 `[0 3.052 null 274.9 157.3]`
- 触发：`/BBox` 含 null 或元素不足——源码注释表明这是遇到过的真实文档
- V1：**高度相关**（有真实样本佐证）
- 构造：Form XObject `/BBox [0 3.052 null 274.9 157.3]`
- 预期：报 `BadBBox{name, raw}` → 该 XObject 跳过 + 页级降级；进程不 panic、不整篇失败
- oracle：退出码 0（其他页仍翻译）+ 错误分类 + 该页 content stream 原样 · **M1** · 故意畸形

**XOBJ-06 · /Matrix 元素数不是 6 时解包崩溃**
- 源码：`prepared_resource_builder.py:171-172`（**不截断到 6**）+ 消费端 `state.py:20-22` 六元解包，未捕获
- 触发：`/Matrix [1 0 0 1]`（4 个）或 7 个元素
- V1：相关——Rust 里对应 slice 索引 panic 或 `try_into` 失败
- 构造：Form XObject `/Matrix [1 0 0 1]`
- 预期：元素数 ≠ 6 → 报 `BadMatrix` → 该 XObject 跳过 + 页级降级；不 panic
- oracle：无 panic + 错误分类 · **M1** · 故意畸形

**XOBJ-07 · XObject 无 /Resources 时继承页面资源**
- 源码：`prepared_resource_builder.py:160-165`（正确继承）；但消费端 `resources.py:60-76` 的字体查找是**沿路径累积合并所有祖先层**，同名字体在不同层指向不同对象时取最深那层
- 触发：早期 PDF 生成器产出的 Form XObject 不带自己的 Resources
- V1：相关——字体名的作用域是走查正确性的关键
- 构造：页面 `/Font << /F1 → Helvetica >>`；XObject 自带 `/Font << /F1 → Courier >>`；两处都写 `/F1 12 Tf (III) Tj`
- 预期：XObject 内用 Courier 度量（等宽 600）、页面上用 Helvetica（`I` = 278）→ 两组字符 x 间距不同，断言具体值
- oracle：坐标与 PDFium 对齐 · **M0**（走查对齐）· 合法

**XOBJ-08 · 奇异 CTM 时返回空操作导致坐标系不恢复**
- 源码：`base_operations.py:74-97`（行列式接近 0 时返回 `" "`）；同类回退见 `state.py:33-45`、`matrix_helper.py:304-316`、`pdfinterp.py:324-328`。**写回的 content stream 因此少了一个 `cm`**，该 XObject 之后的所有绘制都在错误坐标系里，无日志
- 触发：`0 0 0 0 0 0 cm` 后 `Do`；XObject `/Matrix [0 0 0 0 0 0]`；`1 0 1 0 0 0 cm`（退化成一条线）
- V1：**高度相关**——直接对应 mimus 的 CTM 栈维护与增量写回
- 构造：`q 0 0 0 0 0 0 cm /X1 Do Q BT /F1 12 Tf 100 700 Td (After) Tj ET`
- 预期：用 `q`/`Q` 配对而非逆矩阵恢复坐标系 → 断言 `(After)` 绝对坐标 == (100,700) 且输出中 `q`/`Q` 计数平衡；奇异 CTM 下 XObject 内文本标记为不可定位（不翻译）
- oracle：q/Q 平衡断言 + 坐标断言 + 光栅 diff · **M0**（增量写回 + CTM 栈）· 合法（退化矩阵本身合法）

**XOBJ-09 · XObject 不是流时静默丢弃**
- 源码：`prepared_resource_builder.py:59-65, 111-119`（条目从资源表消失，随后走 XOBJ-01 的静默返回空）
- 触发：`/XObject << /X1 5 0 R >>` 但 obj5 是普通字典无流；或 obj5 不存在
- V1：相关
- 构造：obj5 = `<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] >>`（无 stream）
- 预期：报 `NotAStream{name}` → 页级降级
- oracle：错误分类 · **M3** · 故意畸形

**XOBJ-10 · XObject 流写回失败后继续，产出半译文档**
- 源码：`pdf_creater.py:1712-1730`（`except Exception: logger.warning(... continue)`）
- 触发：XObject 的对象在 ObjStm（压缩对象流）内、只读、或已被前面的修复函数改成 `[]`
- V1：**高度相关**——该 XObject 保留原文而页面主流已替换，出现"一半译文一半原文"且无错误上报。这正是 mimus 增量写回必须验证的场景
- 构造：Form XObject 存在 `/Type /ObjStm` 压缩对象流里且含文字；页面上也有文字
- 预期：能正确"从 ObjStm 取出 → 修改 → 以非压缩对象追加"；断言该 XObject 文本已翻译且原 ObjStm 中其余对象未受影响。若无法处理则文档级 fail-fast，**不产出半译文档**
- oracle：独立解析器展开后逐对象比对 + 输出文本断言 · **M0**（增量写回风险探测）· 合法

#### GEOM — MediaBox / CropBox / Rotate / 坐标系

**GEOM-01 · MediaBox 用字符串切分解析**
- 源码：`high_level.py:794-820`（`fix_media_box`）。**纯字符串 split**，元素数不为 4 即抛错，只记 warning 后 box 保持原样。函数上方注释给出真实样本 `'[0 nul 792]'`
- 触发：`/MediaBox [0 null 612 792]`；带多余空格导致 split 出空串；`/MediaBox` 是间接引用
- V1：**高度相关**
- 构造：三份——含 null 的、双空格的、间接引用的
- 预期：按 PDF 对象解析（非字符串切分）；含 null → `BadMediaBox{page}` 页级降级；双空格与间接引用正常解析
- oracle：解析出的 box 数值断言 + 错误分类 · **M1** · 合法（双空格/间接引用）/ 故意畸形（null）

**GEOM-02 · MediaBox 原点非零被强行归一化并删除其他 box**
- 源码：`high_level.py:801-820`。把 `[100 100 712 892]` 改写成 `[0 0 712 892]`——**保留了 x1/y1 原值，页面凭空变大 100pt**（正确做法应是 `[0 0 (x1−x0) (y1−y0)]`）；同时把 CropBox/BleedBox/TrimBox/ArtBox **全部置 null**，最后再尝试还原（`pdf_creater.py:1435-1441, 1484-1487`），中途任何失败即永久丢失
- 触发：`/MediaBox [100 100 712 892]`（原点非零，合法且常见于印刷稿）
- V1：**高度相关**——mimus 增量写回不应该动 MediaBox，坐标转换应在内存中完成
- 构造：`/MediaBox [100 100 712 892]` + `/CropBox [150 150 662 842]` + 页面文字
- 预期：输出的 `/MediaBox` 与 `/CropBox` 与输入**逐字节相同**；译文坐标基于 CropBox 原点正确定位
- oracle：独立解析器对比 page dict + 光栅 diff · **M0**（增量写回）· 合法

**GEOM-03 · CropBox 查找不走继承**
- 源码：`pymupdf_page_view_access.py:33-41`。只看页面自身对象，**不走 `/Parent` 继承**；最终靠底层库的 cropbox 兜底（那一层会继承），所以结果碰巧对但路径不一致；CropBox 是间接引用时类型判断失败直接走兜底
- 触发：`/CropBox` 定义在 `/Pages` 节点上（合法继承）
- V1：相关
- 构造：`/Pages << /CropBox [50 50 562 742] … >>`，页面自身无 CropBox 但有 MediaBox `[0 0 612 792]`
- 预期：CropBox 从 `/Pages` 继承，有效裁剪框 == `[50 50 562 742]`，字符坐标基于它计算
- oracle：与 PDFium 页面边界 API 对比 · **M1** · 合法

**GEOM-04 · /Rotate 只交换 CropBox 分量、CTM 完全不旋转**
- 源码：`prepared_page.py:50-54`。90/270 时返回 `(y0, x1, y1, x0)`——**这不是标准的 bbox 交换**（标准应为 `(y0, x0, y1, x1)`），x1 与 x0 位置互换。走查过程中从未用 rotate 旋转 CTM；180° 完全不处理。`high_level.py:494-519` 有一段旋转处理代码在 `return` 之后成为死代码，注释写着 `# skip rotate for now`
- 触发：`/Rotate 90/180/270`（横排扫描与演示稿常见）、`/Rotate -90`（合法，等价 270）、`/Rotate 45`（非法）
- V1：**高度相关**——研究任务点名的页面几何项，且版面模型看到的是旋转后的光栅
- 构造：6 份单页，`/Rotate` 分别为 0/90/180/270/−90/45，内容完全相同（同一句话在左上角）
- 预期：前五者的译文相对**视觉页面**位置一致（都在视觉左上角）；`/Rotate 45` → `BadRotate` 页级降级。断言：渲染后译文文本块的像素重心落在视觉左上象限；输出 rotate 值与输入相同
- oracle：光栅渲染 + 文本块重心坐标断言 + rotate 透传断言 · **M0**（走查对齐 + 模型验证）· 合法（前五）/ 故意畸形（45）

**GEOM-05 · 光栅原点非零只记 warning、DPI 降到 1 仍可能超预算**
- 源码：`raster_geometry.py:118-125, 185-192`（非零原点只 warning 继续，而坐标映射假定原点为 0）；`200-214`（DPI 逐级递减，**降到 1 时即使仍超预算也返回**，源码注释明确说这是有意为之）
- 触发：MediaBox 原点非零且归一化失败（GEOM-01 + GEOM-02 组合）；超大页面（PDF 上限 14400pt）
- V1：相关——直接影响 ONNX 版面模型输入的正确性
- 构造：`/MediaBox [100 100 712 892]` 且无 CropBox 的页面；变体 `/MediaBox [0 0 14400 14400]`
- 预期：光栅→PDF 坐标映射显式包含原点平移，断言模型输出 bbox 反变换后落在 CropBox 内；超大页 DPI 下限 ≥1 且像素数 ≤ 预算，否则报 `PageTooLarge` 拒绝该页
- oracle：反变换后的 bbox 与构造时已知的文字位置比对 · **M0**（模型验证）· 合法

#### WRITE — 写回、保存、字体嵌入、书签

**WRITE-01 · 写回失败后整篇重跑并静默丢弃"字体不在资源里"的字符**
- 源码：`pdf_creater.py:1620-1627`（失败即以 `check_font_exists=True` 重跑）+ `100-106`（该模式下字体名查不到就**静默 return，字符直接消失，无日志**）
- 触发：第一遍写回抛任何异常（字体嵌入失败、xref 冲突、资源字典是间接引用……）
- V1：**高度相关（反面教材）**——"静默产出坏译文"的最严重实例：输出一个缺字的 PDF 并返回成功
- 构造：页面 `/Resources` 是间接引用且指向 ObjStm 内的字典
- 预期：写回失败 → **文档级失败**并保留原文件；绝不进行"丢字重试"。断言失败时输出文件不存在或与输入相同，退出码非 0
- oracle：退出码 + 输出文件哈希断言 · **M0** · 合法

**WRITE-02 · 编码宽度查不到时字符静默丢弃**
- 源码：`pdf_creater.py:122-134`（三层 map 查找失败后只有 debug 级日志然后 `return`；debug 默认不输出）+ `1650-1668`（XObject 层的 map 被页面层覆盖）
- 触发：字符的字体是 FONT-01 合成的兜底字体，或来自某个 XObject 但合并顺序导致丢失
- V1：**高度相关**
- 构造：复用 FONT-01 的"引用不存在的字体名"用例
- 预期：写回时遇到无法确定编码宽度的字符 → 视为**写回失败**并页级降级保留原页；不静默丢字。断言输出字符数不少于输入原文字符数
- oracle：PDFium 字符计数对比 + 错误分类 · **M0** · 故意畸形

**WRITE-03 · 资源字体列表靠正则从 xref 文本里抠**
- 源码：`pdf_creater.py:950-974`。多处 `re.search(...).group(1)` **无 None 检查**；`/Font *<<(.+?)>>` 是非贪婪匹配，**嵌套 `<<` `>>` 会截断**；任何失败返回空集合 → 配合 WRITE-01 → **整页字符全丢**
- 触发：字体字典直接内联（嵌套 `<<`）；`/Font` 字典里名字后跟换行而非空格；`/Resources` 是指向 ObjStm 内对象的间接引用
- V1：**高度相关（反面教材）**——mimus 用 lopdf 对象树天然没有这问题，关键是**确保不退化成文本匹配**
- 构造：`/Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>`（内联字体字典）
- 预期：字体从对象树读取，内联与间接引用行为一致；断言两种写法的输出 PDF 内容等价
- oracle：两个输入产出的译文文本相同 · **M1** · 合法

**WRITE-04 · 资源字典是间接引用时靠异常消息字符串匹配来重试**
- 源码：`pdf_creater.py:692-730`（`if "has indirects" not in str(exc): raise`——**靠异常文本判定**）；`665-670` 的正则要求 generation 必须为 0；`732-774` 在资源引用找不到时 `continue`，**在 content stream 里留下悬空引用**
- 触发：页面 `/Resources` 是间接引用且需要追加字体；generation ≠ 0 的引用会让正则失配；Resources 被多页共享时改一处影响多页
- V1：**高度相关**——mimus 追加字体到 `/Resources/Font` 必须处理这三种情况
- 构造：两页共享同一个 `/Resources 8 0 R`，obj8 内有 `/Font 9 0 R`；另加 generation ≠ 0 的变体
- 预期：Resources 被共享时必须**复制后再改**（copy-on-write），断言另一页的资源字典未被污染；generation ≠ 0 正常处理
- oracle：独立解析器展开后逐对象检查两页的 Resources 引用与内容 · **M0**（增量写回）· 合法

**WRITE-05 · 保存超时四级降级**
- 源码：`pdf_creater.py:1300-1433`（子进程保存 → 超时 kill → `save(clean=False)` → 无选项裸保存）；上游 `high_level.py:88-94` 已是两级
- 触发：超大/超复杂 PDF 使清理型保存超时；子进程 OOM
- V1：部分相关——mimus 增量写回不做 GC 所以没有这条链，但"保存失败时不能产出半成品文件"这个不变量要验
- 构造：大规模压力用例（数千页 × 每页数百个小 XObject）
- 预期：写回是"原文件 + 增量段"追加，断言中途被 kill 时输出文件要么不存在要么是完整的原文件（原子 rename）；成功时前 len(input) 字节与输入相同
- oracle：文件字节前缀断言 + 中断注入测试 · **M3** · 合法

**WRITE-06 · 书签迁移降维丢失结构**
- 源码：`high_level.py:736-791`。`get_toc()` 把书签降维成 `[level, title, page]` 三元组 → **丢失 destination 的精确坐标与缩放、`/Count` 折叠状态、颜色与粗体样式、非页面 action（URI / GoToR）**；失败则整篇无书签只留 error 日志。另注：`757-764` 处 `for i in len(old_doc)` 对整数迭代，在特定选项下必然抛 TypeError，且被上层 except 吞掉
- 触发：输入带书签；书签目标页被删；destination 是命名目标
- V1：**高度相关**——mimus 明确要求书签原样透传，增量写回天然做到，这是验证点
- 构造：3 层嵌套书签，含 `/Dest [3 0 R /XYZ 100 700 2]`、一个 URI action、一个命名目标、`/C [1 0 0] /F 2`（红色粗体）、`/Count -2`（折叠）
- 预期：输出的 `/Outlines` 子树与输入**逐字节相同**；PDFium 书签遍历结果一致，包括缩放值、颜色、折叠状态
- oracle：独立解析器逐对象 diff + PDFium 书签遍历比对 · **M0**（增量写回验证）· 合法

**WRITE-07 · 字体子集化子进程失败则保留全量字体**
- 源码：`pdf_creater.py:555-571, 1268-1269, 1478-1481`（失败只 error 日志，保留未子集化字体）
- 触发：嵌入的中文全字库子集化超时；原 PDF 含子集化工具无法处理的字体（CFF2、可变字体）
- V1：相关（mimus 明确要求 Noto Sans SC 嵌入 + 子集化）
- 构造：一页含 200 个不同汉字的译文场景
- 预期：输出中嵌入字体流显著小于全字库（子集有效）；子集化失败 → 报 `SubsetFailed` 并拒绝输出或明确告警，不静默产出超大文件
- oracle：输出文件大小断言 + 用独立字体库打开嵌入字体检查字形数 · **M1** · 合法

**WRITE-08 · ToUnicode 重建**
- 源码：`pdf_creater.py:524-552`（`to_int(ff[1])` **无 None 检查**；每页 `except Exception` 跳过该页字体）；入口 `high_level.py:216-233`。过滤条件只处理 BabelDOC 自己嵌入的字体
- 触发：嵌入字体子集化后 ToUnicode 与实际字形集合不匹配
- V1：相关——mimus 嵌入 Noto 子集时必须同步生成正确的 ToUnicode，否则译文无法选中/搜索
- 构造：翻译一页中英混排 PDF
- 预期：新嵌入字体的 ToUnicode 覆盖**全部**已用 CID；断言 PDFium 提取的译文字符串 == 期望译文（无缺字、无空字符）
- oracle：PDFium 文本提取与译文精确比对 · **M1** · 合法

#### DOC — 文档级

**DOC-01 · 重复翻译检测**
- 源码：`high_level.py:157-169`（producer 含特定标记则拒绝）+ `534-542`（metadata 读取失败则**静默继续**）；写入侧 `172-213`
- 触发：把译文 PDF 再喂一次；或 `/Info` 损坏/加密导致读不出
- V1：相关——同样需要防重复翻译标记
- 构造：`/Info` 含自身产物标记；变体：`/Info` 指向不存在的对象
- 预期：识别自身产物 → `AlreadyTranslated` 拒绝；`/Info` 损坏 → warning 后继续（这个兜底合理）
- oracle：退出码 + 错误分类 · **M1** · 合法 / 故意畸形

**DOC-02 · 扫描件检测的 SSIM 方案**
- 源码：`detect_scanned_file.py:84-172`（详见 §3.8 SCAN-01/02）。检测成本是**每页渲染两次**
- 触发：图片背景 + 不可见 OCR 文本层；反例：纯白背景上极小字号文字也可能超阈值被误判
- V1：**高度相关**——但 SSIM 方案对 mimus 太重，应基于"文字对象数为 0"或"文字全为 `3 Tr`"这类结构信号
- 构造：(a) 单页只有一个全页 Image XObject、无任何 `BT`；(b) 全页图 + `3 Tr` 文本层；(c) 全页图 + 正常可见文字（不该判扫描）；(d) 10 页中 9 页扫描 1 页正常
- 预期：(a)(b)(d) → `ScannedPdf{scanned_pages, total}` 拒绝，退出码固定；(c) 正常翻译。断言错误结构里的页数统计准确
- oracle：退出码 + 错误分类 + 统计字段断言 · **M1** · 合法

**DOC-03 · 加密 PDF 的拒绝路径**
- 源码：BabelDOC 侧全仓库非 vendored 代码中 `needs_pass` / `authenticate` / `is_encrypted` / `password` / `permissions` **零命中**。加密文档打开后流读取会抛异常，落到 `fix_null_xref` 的兜底 → **把整篇每个对象都改写成 `[]`** → "成功"输出一个空文档。**反面教材**
- 触发：任何 trailer 带 `/Encrypt` 的文档。注意**空 user password 的加密 PDF**（防复制/防打印用途）是最常见的形式，完全合法可读
- V1：**高度相关，但方向是拒绝而非支持**——ADR-0009 决定 V1 一律拒绝加密 PDF。本 case 守护的是拒绝路径本身
- 构造：(a) 空 user password 的 RC4 40-bit 加密；(b) 设置了 user password 的（算法任选，取 AES-128 即可）。两档对应两条不同的库行为路径：(a) 在 lopdf 侧**加载成功**（reader 无条件先试空密码），(b) 在加载阶段直接 `Err(InvalidPassword)`
- 预期：两档都以退出码 2 拒绝，错误 `reason` 相同，人类可读消息给出 qpdf 解密建议。**(a) 是重点**——它是"静默放行"这个失败模式的唯一守卫：若实现误用 `is_encrypted()`（lopdf 加载后会抹掉 `/Encrypt`，该方法返回 `false`），(a) 会被透明解密并一路跑完流水线，可能产出看起来正常的输出而无人察觉
- oracle：退出码 + 错误分类断言；(a) 额外断言**未产生任何输出文件**且未发生翻译调用 · **M1** · 合法

**DOC-04 · 非直立文本的判定与隔离**
- 源码：`il_creater_active.py:1292-1307`（`except` 分支里**没有 `return`**，角度算不出来的字符照样进入 IL，而角度算得出但不在 0°/90° 附近的字符被丢弃——即**判定失败后守卫落空，判定成功反而丢字符**）；`il_creater.py:970-979` 同构
- 触发：字符矩阵含 NaN/Inf（由退化的 `Tm` 或除零产生）；45° 水印、旋转表头、镜像文本在真实 PDF 中常见
- V1：**相关**——决策 #32 已把"非直立文本不翻译、原样 passthrough"定为与翻译政策表同层级的政策，本 case 是其验收
- 构造：按 §2.4 单变量原则拆成 **7 份**，各含一个变量（父本均为 `unit-base-01`）——视觉 90° 旋转 / 45° 旋转 / 镜像（变换行列式为负）/ 斜切 15°（**直立负例，须正常翻译**）/ `0 0 0 0 100 700 Tm` 退化矩阵（畸形）/ `/Rotate 90` 页面上的水平文本（**视觉页框口径的负例**，断言它不被判为非直立）/ 一个正常段落中夹一个 45° 字符（单元级隔离）
- 预期：直立段与 15° 斜切段正常翻译；90° 与 45° 段 `TextTransform` 分别为 `Rotated(90)` / `Rotated(45)`，不翻译且 content stream 片段与输入逐字节相同；退化矩阵标记不可定位并保留原文。同一段落内混有非直立字符时，只隔离该字符，其余照常翻译（单元级隔离）
- oracle：**M1** 断言 IL 中每个字符的 `TextTransform` 取值与 manifest 一致（含 `/Rotate 90` 的负例）；**M3** 追加光栅 diff（非直立区域像素差 == 0）与字符数守恒 · 合法（前四）/ 故意畸形（退化矩阵）
- 优先级：**M1**（分类正确性）+ M3（保真度加强）

### 3.2 版面归属与漏检兜底（LAYOUT）

一个贯穿全篇的结构性事实：**BabelDOC 没有任何几何阅读顺序计算**。段落顺序 == 字符顺序 == content stream 绘制顺序（`paragraph_finder.py:447` 单趟遍历）；`document_il/` 下唯一的排序操作是 layout 优先级（`layout_helper.py:778`）、fallback 行聚类（`extract_char.py:607`）与输出 z-order（`pdf_creater.py:934`），**没有一处对段落做二维重排**。它的跨栏/跨页处理全是事后补丁。这正是 mimus 采用 PP-DocLayoutV3（自带阅读顺序）的最大红利所在，也是最需要语料验证的一点。

#### LAYOUT-01 · 优先级表压过 IoU
- 源码：`layout_helper.py:650-798`。68 项优先级表（含重复项），排序键 `(priority_index, -iou)`：优先级**永远**压过 IoU——与 `plain text`(idx 26) 只重叠 1%、与 `table`(idx 65) 重叠 99%，仍归 `plain text`
- 触发：任何嵌套框——表格内说明文字、图内轴标签、脚注压在正文框底部
- V1：相关，但性质变化。25 类语义更细，无需手工优先级表；但嵌套框仲裁规则 mimus 必须自定义（建议"面积最小的包含框"而非固定优先级）
- 构造：外层 300×200 的 `table` 框 + 内层 120×20 一行文字（完全落入 table），另有一个 `plain text` 框只擦到文字左缘 2pt
- 预期：该行 `layout_label == "table"`
- oracle：IL 结构断言 · **M1** · 合法

#### LAYOUT-02 · fallback 兜底把表格/图内文字升格为可翻译正文
- 源码：`layout_parser.py:178-211`（`fallback_line`，`conf=1`，覆盖全页每个字符聚类）+ `layout_helper.py:844`（列入 `is_text_layout`）+ 优先级 idx 64，**高于 `table`(65)/`figure`(66)/`image`(67)**
- 触发：任何表格、任何图内文字。模型只给粗框时，框内文字被 fallback 抢走并进入翻译流程
- V1：**极高相关，且是政策冲突点**。mimus 政策为表格不翻。若 mimus 也做兜底聚类，兜底产物必须**继承所属大框的语义**（落在 `table` 内的兜底行仍是 table），不得升格为正文
- 构造：一页，仅一个 3×3 有线表格（画线 + 单元格文字），无正文段落
- 预期：表格内文字**不进入任何翻译请求**；输出中表格区域与输入渲染等价
- oracle：翻译请求内容断言（请求列表为空）+ 渲染对比 · **M1** · 合法

#### LAYOUT-03 · display/inline 公式重标（IoU ≥ 0.5）
- 源码：`paragraph_finder.py:60-87`。注意 `calculate_iou_for_boxes`（`layout_helper.py:566`）**不是标准 IoU**，是"交集 / 第一个框面积"
- 触发：display 公式（独占一行）vs inline 公式（嵌在行内）；临界情形是 display 公式恰被过大的 `plain text` 框包住 50%
- V1：**待验证红利**——V3 原生给 `display_formula` / `inline_formula` 两类
- 构造：(a) 单独居中的公式，上下各一段正文；(b) 同一公式但正文框撑大到包住它 60%；(c) 行内 `E=mc²` 嵌在句中
- 预期：(a)(b) 判为 `display_formula`，不进翻译请求且几何不变；(c) 判为 `inline_formula`，以占位符进入请求
- oracle：翻译请求内容断言（占位符计数）+ 几何断言 · **M0**（模型能力验证）· 合法

#### LAYOUT-04 · 字符归属公式框的双阈值（IoU 0.4 / bbox 回退 0.2）
- 源码：`layout_helper.py:852-880`。先判 `iou(visual_bbox, pdf_box) < 0.2` 则改用度量盒，再要求与公式框 `iou > 0.4`
- 触发：墨迹盒与前进宽度严重脱节的 glyph——LaTeX 的 `\sum`、`\int`、`\left(`
- V1：相关（mimus 同样需要墨迹盒）
- 构造：`\sum_{i=1}^{n}` 大符号（glyph 高 3× 字号、advance ≈ 1 字宽）嵌在正文中
- 预期：该 glyph 归入公式，不被当作正文字符参与切段
- oracle：IL 结构断言（所属 composition 是 formula 而非 line）· **M3** · 合法

#### LAYOUT-05 · 框膨胀 ±1px 与 mediabox 向上取整
- 源码：`layout_parser.py:142-153`（`ceil(mediabox.y)` 后 `x0-1,y1+1` 膨胀并 clip）。注意 `table_parser.py:141-148` 用的是渲染像素高宽，**与 layout_parser 的点坐标系不一致**
- 触发：非整数 MediaBox（A4 595.276×841.89）、MediaBox 原点非零、页面带 `/Rotate`。ceil 引入约 0.7pt 系统性偏移
- V1：相关（坐标换算必做，且此处是明显的 bug 源）
- 构造：MediaBox `[10 10 605 852]`，一段正文靠近右下角
- 预期：layout 框换算回 PDF 点坐标后与段落 bbox 偏差 < 1pt；`/Rotate 90` 时框跟随旋转
- oracle：几何断言 · **M1** · 合法

#### LAYOUT-06 · 置信度 0.25 且无 NMS
- 源码：`doclayout.py:243`（`conf > 0.25`）、`base_doclayout.py:21`（仅按 conf 排序，**无 NMS/去重**）、`imgsz` 硬编码 1024
- 触发：同区域同类框重复检出（长表格、跨栏图）；密集版面（幻灯片、报纸）低分框泛滥
- V1：相关。必须明确 PP-DocLayoutV3 后处理是否含 NMS 及重复框合并策略
- 构造：一页 6 个并排小文本块（每块 3 行），间距 4pt
- 预期：每块恰好一个 layout 框；段落数 == 6
- oracle：IL 结构断言 · **M1** · 合法

#### LAYOUT-07 · 文本白名单包含 header/footer/seal，却注释掉 reference
- 源码：`layout_helper.py:801-849`。`# "reference",`（`:815`）被显式注释；而 `header`/`footer`/`seal`/`page_header`/`page_footer` **都在白名单里 → BabelDOC 会翻译页眉页脚和印章**
- 触发：带页眉页脚的期刊/公文；带红章的中文公文
- V1：**相关且政策相反**——mimus 政策为页眉/页脚/印章/参考文献一律不翻。这是必须用语料锁死的政策断言
- 构造：一页含页眉"Journal of X, Vol.3"、页脚页码、正文两段、末尾一条 `[1] Smith et al.` 参考条目、右下角红色印章图 + 章内文字
- 预期：翻译请求恰好 2 条（两段正文）；页眉/页脚/参考/印章文字均不出现在任何请求中且原样保留
- oracle：翻译请求内容断言 + 渲染对比 · **M1** · 合法

#### LAYOUT-08 · fallback 行聚类的 DBSCAN 魔数链
- 源码：`extract_char.py:18-49`。`BAND_CREATION_OVERLAP_THRESHOLD=0.5`、`LINE_CLUSTERING_EPS_MULTIPLIER=3.5`（eps = 3.5 × 平均字宽，manhattan，`min_samples=1`）、`LINE_SPLIT_SIZE_RATIO_THRESHOLD=1.5`、`LINE_SPLIT_DBSCAN_EPS_MULTIPLIER=0.5`、`SPACE_INSERTION_GAP_MULTIPLIER=0.45`、合并三阈值 `1.5/0.6/0.7`
- 触发：**eps = 3.5 × 字宽是致命参数**——双栏且栏间距 < 3.5 字宽时，左右栏同一水平线的字符被连成一行；后续"高行二次拆"只在行高异常时触发，栏间粘连救不回来
- V1：**待验证红利**——若完全信任 V3 的框可以不做兜底聚类，但必须有语料证明模型不漏检
- 构造：A4 双栏，栏宽 240pt，**栏间距 8pt**（10pt 字号下约 0.8 字宽），每栏 20 行
- 预期：段落数 == 左栏段落数 + 右栏段落数；不存在 bbox 宽度 > 250pt 的段落（无横跨两栏者）
- oracle：几何断言 · **M1** · 合法

### 3.3 阅读顺序与多栏（ORDER）

#### ORDER-01 · 阅读顺序等同 content stream 顺序
- 源码：`paragraph_finder.py:447`、`_set_paragraph_render_order`（`:312-347`）、`typesetting.py:1664-1682`。`_sort_characters_in_lines`（`:1032`）**已被注释停用**（`:305`）
- 触发：content stream 顺序 ≠ 视觉顺序的 PDF，极常见——LaTeX 先画正文再画脚注、Word 导出把浮动图文字放最后、某些工具按"左栏第 1 行、右栏第 1 行"交错输出
- V1：**M0 核心验证项**。V3 自带阅读顺序是 mimus 最大的结构性优势，必须证明它在乱序 content stream 上真的兑现
- 构造：三份**视觉渲染逐像素一致**、content stream 顺序不同的 PDF：(a) 自然顺序；(b) 段落倒序绘制；(c) 双栏行交错绘制
- 预期：三份产出的**翻译请求序列完全相同**（文本与顺序均相同）
- oracle：翻译请求内容断言（请求文本列表逐项相等）· **M0** · 合法

#### ORDER-02 · 跨栏合并判据 `y2 差 > 20pt`
- 源码：`il_translator_llm_only.py:456-526`，核心在 `:502`
- 触发：纯粹是在补"没有阅读顺序"——假设 content stream 是"左栏自上而下 → 右栏自上而下"，栏切换时 y 突跳回页顶。**误触发**：分节标题后正文从页顶重新开始、表格跨页续接、页脚注释块
- V1：**待验证红利**（模型给顺序后栏续接即顺序相邻）
- 构造：(a) 真跨栏：双栏，左栏末段句子在栏底被截断，右栏首段是其续接；(b) 假阳性：单栏，一段后接二级标题，标题下正文起点人为抬高 > 20pt
- 预期：(a) 合并为 1 个翻译请求且译文不重复；(b) 保持 2 个独立请求
- oracle：翻译请求内容断言 · **M0**（红利）/ **M3**（假阳性精修）· 合法

#### ORDER-03 · 跨页合并无条件取"上页末段 + 下页首段"
- 源码：`il_translator_llm_only.py:359-454`（`# force submit regardless of token count`，`:437`）；`_is_body_text_paragraph` 只认三个 label（`:268-272`）
- 触发：(a) 真跨页续段；(b) 上页末段已完整结束（句号结尾）、下页首段是新章节——仍被强行合并；(c) 上页末尾是脚注（label 不符被跳过）→ 回退到倒数第二段合并，语义完全错位
- V1：**相关**。V3 的阅读顺序是**页内**的，跨页续接仍需 mimus 自行判断（建议依据末行是否右对齐到栏宽 + 是否以句末标点结尾）
- 构造：三页。p1 末段句子在页底被截断（无句号），p2 首段续接；p2 末段以句号完整结束，p3 首段是新章节正文
- 预期：p1末+p2首 合并为 1 请求；p2末 与 p3首 保持独立
- oracle：翻译请求内容断言 · **M3** · 合法

#### ORDER-04 · 正文段落白名单只认 3 个 label
- 源码：`il_translator_llm_only.py:259-272`。跨页/跨栏合并仅对 `text`/`plain text`/`paragraph_hybrid` 生效
- 触发：摘要跨栏、编号列表跨页、长图注跨栏
- V1：相关（mimus 的 25 类 label 集合完全不同，白名单需重写）
- 构造：双栏首页，Abstract 从左栏底跨到右栏顶
- 预期：摘要跨栏部分正确合并为 1 个翻译请求
- oracle：翻译请求内容断言 · **M3** · 合法

### 3.4 段落切分与行合并（PARA）

#### PARA-01 · 段落边界受 `xobj_id` 变化影响
- 源码：`paragraph_finder.py:465-503`。三条件：layout 变化、**`xobj_id` 变化**、bullet 起段
- 触发：同一段文字被拆进多个 Form XObject（Word/Visio 导出、带批注的 PDF、某些 LaTeX 包）→ 一段被切成 N 段
- V1：**相关**。XObject 边界与语义段落无关，mimus 应在 IR 层把 XObject 内字符 flatten 到页坐标后再切段
- 构造：一页一段 5 行正文，其中第 3 行整行画在一个 Form XObject 内（视觉无缝）
- 预期：段落数 == 1；翻译请求恰好 1 条且含全部 5 行
- oracle：IL 结构断言 · **M1** · 合法

#### PARA-02 · 小字符豁免切段（面积 < 中位数 5%）
- 源码：`paragraph_finder.py:428-441` + `:462-475`
- 触发：上标脚注标记、小数点、连字符、装饰小 glyph——bbox 极小，易落入相邻 layout 框或框缝
- V1：相关
- 构造：一段正文，句中含 `word¹`（上标 5pt / 正文 10pt），且让上标 bbox 恰好压在段落框右边界外 1pt
- 预期：段落数 == 1，上标字符留在段落内
- oracle：IL 结构断言 · **M3** · 合法

#### PARA-03 · 行切分靠垂直扫描直方图（step 0.25，gap 判据 count < 1）
- 源码：`paragraph_finder.py:652-777`。分隔线取 gap 的**起始索引**（`:748-751`，注释说取中点但代码不是）；段落总高 < 5pt 直接单行（`:695`）
- 触发：**行间距为 0 或负（行框重叠）** → 没有 count<1 的空隙 → 整段被当作 1 行。常见于紧排诗歌、化学式上下标、leading 被压到 0 的单元格、下标伸入下一行的公式
- V1：**相关，M1 基础能力**
- 构造：(a) 字号 10 行距 12 的正常段落；(b) 行距 9.5（行框重叠 0.5pt）；(c) 高度 4pt 的单行段落
- 预期：(a)(b) 行数 == N；(c) 行数 == 1
- oracle：IL 结构断言 · **M1** · 合法

#### PARA-04 · 目录条目切分依赖连续 20 个点
- 源码：`paragraph_finder.py:864-889`，`re.search(r"\.{20,}", prev_text)`
- 触发：**20 是硬阈值**——短标题短点线（`1.1 背景....3`）不切；用 `⋯`(U+22EF)、制表符或纯右对齐做 leader 的目录也不切
- V1：**待验证红利**（需确认 25 类是否含目录类）
- 构造：一页 6 个目录条目，leader 分别为：40 个点 / 8 个点 / `⋯` 重复 / 制表符右对齐 / 无 leader 纯右对齐 / 中文点号重复
- 预期：段落数 == 6，页码不与下一条目标题粘连
- oracle：IL 结构断言 · **M3** · 合法

#### PARA-05 · 短行切段（默认关闭，因子 0.8）
- 源码：`paragraph_finder.py:891-903`；`split_short_lines` 默认 `False`，`short_line_split_factor=0.8`（`translation_config.py:178-179`）。中位数取自**整页所有行**而非段内
- 触发：开启则每段末行（天然偏短）都被切开；关闭则"一个 layout 框错误包住两个自然段"无法切分
- V1：**相关**。mimus 需要"框内如何切多段"的规则，合理判据是"首行缩进 + 上一行未满行宽"的组合，而非单看短行
- 构造：一个 `paragraph` 框内含两个自然段（段间无空行，第二段首行缩进 2em，第一段末行占 40% 行宽）
- 预期：段落数 == 2
- oracle：IL 结构断言 · **M3** · 合法

#### PARA-06 · bullet 字符集过宽（含上标数字与中间点）
- 源码：`layout_helper.py:50-52` 的 `BULLET_POINT_PATTERN`，含 `■•⚫◆○●‣▪` 等符号 + **上标数字 `¹²³…`** + 下标数字 + 上标字母 + `¶※‖·`
- 触发：`text¹` 的脚注标记、`A·B` 的乘号、`Smith · 2020` 的分隔点都会被当作项目符号；同时它还会置位 sticky flag 永久关闭该段上下标检测（见 FORM-05）
- V1：相关
- 构造：三段——(a) 真列表 `• item`；(b) 段首为中文着重号 `·`；(c) 段中出现 `¹`
- 预期：(a) 每项一段；(b)(c) 不因该字符切段
- oracle：IL 结构断言 · **M3** · 合法

#### PARA-07 · 交替行号合并（`merge_alternating_line_number_paragraphs`）
- 源码：`paragraph_finder.py:393-418`；判据 `:368-380`、`:382-391`；默认开启
- 触发：为**带行号的稿件**而写（LaTeX `lineno`、法律条文、审稿版论文）——行号是独立文本块，插在字符流中把一段正文切成碎片。**陷阱**：判据对空文本返回 True，任何空段落都会被当成"行号"触发合并
- V1：**待验证红利**（V3 的 `aside_text` 类有可能单独框出行号）
- 构造：一页 20 行连续正文，左边距每 5 行一个行号，行号与正文同属一个 layout 框；变体：行号被单独框为 `aside_text`
- 预期：段落数 == 1；翻译请求文本连贯且**不含行号数字**；输出中行号原位保留
- oracle：翻译请求内容断言 + 渲染对比 · **M3** · 合法

#### PARA-08 · 重叠段落框中点切分（硬编码 ±1pt）
- 源码：`paragraph_finder.py:939-1030`。最多 n² 轮；完全包含则跳过；否则在垂直重叠区中点切开并留 ±1pt 间隙
- 触发：段落 bbox 垂直重叠——末行下伸部、公式下标伸入下一段、行内公式大 glyph 撑高段落框
- V1：**相关**。副作用是压缩后的框成为排版容器 → 逼迫 scale 下调（见 TYPE-01），直接影响"译文框不越界"断言
- 构造：两段正文，上段末行含 `\sum`（下伸 8pt），下段首行紧邻（行距 12pt），使两段 bbox 垂直重叠 3pt
- 预期：两段 bbox 不重叠，**且每段 bbox 仍完全包含其所有字符的墨迹盒**（BabelDOC 会违反后半条——切完框字符伸到框外）
- oracle：几何断言 · **M3** · 合法

#### PARA-09 · 首行缩进阈值 1pt 过松
- 源码：`paragraph_finder.py:160-170`（首字符 x − 段落 box.x > 1pt 即判缩进）；排版侧用 `space_width × 4` 复现（`typesetting.py:1362`）
- 触发：段落首字是引号/括号时，墨迹左边距天然内缩 > 1pt → 误判为缩进
- V1：相关
- 构造：三段——(a) 无缩进；(b) 2em 缩进；(c) 无缩进但首字为 `"`（左侧留白 1.5pt）
- 预期：(a)(c) `first_line_indent == false`；(b) `== true`
- oracle：IL 结构断言 · **M3** · 合法

#### PARA-10 · 隐式空格推断取"第二小的去重间距"
- 源码：`layout_helper.py:237-293` 与 `:492-551`。收集 > 1pt 的字符间距 → `sorted(set(distances))` → **取索引 1** 作阈值
- 触发：不显式编码空格的 PDF（部分 LaTeX 输出、Type3、某些 CJK 排版）。若 kerning 产生大量 1.01/1.02pt 的独立值，阈值被压到约 1pt → **每个字母间都插空格**，英文被切成 `h e l l o`
- V1：**相关，M1 基础能力**——空格推断错误直接毁掉翻译请求文本
- 构造：(a) 含显式 space glyph 的正常英文段落；(b) 同文本但用 TJ 负数微调实现词距、不写 space；(c) 每字母独立 Tj 且带 ±0.3pt kerning
- 预期：三者产出的翻译请求文本**完全相同**
- oracle：翻译请求内容断言（字符串相等）· **M1** · 合法

#### PARA-11 · 换行判据要求回退 10 个字宽
- 源码：`layout_helper.py:147-150`：`curr.box.x2 < prev.box.x - char_width * 10`
- 触发：**窄栏**（栏宽 < 约 100pt @10pt 字号）换行时 x 回退不足 → 不插空格 → 上行末词与下行首词粘连
- V1：相关
- 构造：三栏版面，栏宽 90pt，10pt 字号，每栏 10 行连续英文
- 预期：翻译请求文本在行边界处有空格分隔，无 `wordword` 粘连
- oracle：翻译请求内容断言 · **M3** · 合法

#### PARA-12 · 前导/尾随空格被剥离
- 源码：`paragraph_finder.py:779-813`。全空白行整行丢弃；行内前导空格仅在已有非空格字符后才保留；尾随空格 pop
- 触发：用空格做缩进/对齐的内容（代码块、ASCII 表格、居中诗歌）→ 缩进丢失
- V1：相关（`algorithm`/代码类的处理策略）
- 构造：一个 4 级缩进的代码块，标为 `algorithm`
- 预期：代码块不进翻译请求，原样 passthrough
- oracle：翻译请求内容断言 + 渲染对比 · **M3** · 合法

#### PARA-13 · CID 段落比例 > 0.8 触发全文档报错
- 源码：`paragraph_finder.py:214-225` + `paragraph_helper.py:9-36`。单段内 `^\(cid:\d+\)$` 占比 > 0.8 为 CID 段落；全文档 CID 段落占比 > 0.8 抛 `ExtractTextError`
- 触发：字体缺 ToUnicode CMap 的 PDF（子集化丢 CMap、某些中文排版软件输出）
- V1：**相关**——正是"宁可保留原文，绝不静默产出坏译文"的体现。但阈值需 mimus 自定，且**拒绝粒度应为段落级而非文档级**
- 构造：10 页文档——(a) 8 页全 CID；(b) 7 页全 CID；(c) 每页混 50% CID 字符
- 预期：(a) 分类报错退出；(b) 继续但那 7 页不翻；(c) 行为需明确定义并断言
- oracle：退出码 + 错误类型 + 翻译请求数 · **M1** · **合法**（缺 ToUnicode 不违反规范）

#### PARA-14 · 零段落抛错
- 源码：`paragraph_finder.py:208-212`
- 触发：纯图页、纯矢量图页、只含表单域的页
- V1：相关（应明确 no-op 或分类拒绝，不得 panic）
- 构造：一页只含一个矢量折线图，无任何文字
- 预期：明确的"无可翻译内容"结果；输出 PDF 与输入渲染等价
- oracle：退出码 + 渲染对比 · **M1** · 合法

#### PARA-15 · 单 composition 段落绕过占位符路径
- 源码：`il_translator.py:607-636`。单 composition 时直接用 `paragraph.unicode` 送翻，**不生成占位符**；纯公式返回 None
- 触发：短标题、单行图注、被误标为 text 的 display 公式
- V1：相关（快路径正确性）
- 构造：(a) 一行图注 `Figure 1: Overview`；(b) 一个 display 公式被标为 `paragraph`
- 预期：(a) 1 个翻译请求；(b) 0 个请求且公式原样保留
- oracle：翻译请求内容断言 · **M1** · 合法

#### PARA-16 · 最短翻译长度 5 字符 + 纯数字跳过
- 源码：`translation_config.py:187`、`il_translator.py:984`、`il_translator_llm_only.py:303`、`paragraph_helper.py:39-52`
- 触发：**5 字符在中文里是很长的一段**——`结论`、`参考文献`、`实验设置` 全部不翻。图编号 `Fig. 1` 恰好 5 字符是边界
- V1：**相关**。源语英文、目标中文，阈值应按源语字符数定且 5 偏大
- 构造：一页含 `Intro`(5)、`Data`(4)、`1`、`3.14`、`Fig. 1`
- 预期：`Intro`/`Data`/`Fig. 1` 进请求；`1`/`3.14` 不进
- oracle：翻译请求内容断言 · **M1** · 合法

### 3.5 公式、上下标与样式（FORM）

#### FORM-01 · 三层字体正则判定公式
- 源码：`formular_helper.py:110-309`。顺序：precise 白名单（约 50 条，`STIXTwoMath`/`LatinModernMath`/`NewCM`/`XITSMath`…）→ 非公式黑名单（约 80 条，`Arial.*`/`.*Times.*`/`Calibri.*`/`.*Symbol.*`/`CMR12.*`…）→ broad 公式模式（`CM[^RB]`/`(MS|XY|MT|BL|RM|EU|LA|RS)[A-Z]`/`TeX-`/`.*Mono`/`.*Code`/`.*Math`…）。先剥 subset 前缀
- 触发：这是一份**为 arXiv/LaTeX 过拟合的名单**，且自相矛盾——`.*Symbol.*` 在黑名单而 `AdvPSSym` 在白名单；`CMR12` 在黑名单而 `CM[^RB]` 在 broad。**`.*Mono`/`.*Code` 会把代码块整体判为公式**
- V1：**待验证红利**——有 `inline_formula` 模型类后可大幅减少对字体名的依赖
- 构造：同一句纯英文散文分别用 `CMR10`、`CMMI10`、`STIXTwoMath-Regular`、`SFMono-Regular` 排版
- 预期：仅 `CMMI10`/`STIXTwoMath` 判为公式；`SFMono` 的英文散文**不得**整段变公式
- oracle：翻译请求内容断言 · **M3** · 合法

#### FORM-02 · 公式起始字符判据过宽
- 源码：`formular_helper.py:16-51`。判据含：`(cid:` 前缀、**目标字体缺字即公式**、Unicode category ∈ {Mn,Sk,**Sm**,Zl,Zp,**Zs**,**Co**}、**`0x370–0x400` 全部希腊字母**、**`[0-9\[\]•]`（所有数字与方括号）**
- 触发：**所有阿拉伯数字都是公式起始字符** → `2024 年` 被切成公式；引用编号 `[12]` 是公式；希腊字母全域 → `α 版本`、`β-catenin` 变公式；不间断空格 U+00A0 是公式；目标字体（Noto Sans SC）缺字的任何字符（泰文、谚文、罕见符号）判为公式
- V1：**高度相关**——这是"翻译请求被占位符污染"的第一大来源。mimus 需更保守的规则（建议：落在 `inline_formula` 框内 **且** 公式字体，才算公式）
- 构造：一段正文，句中依次含 `2024`、`[12]`、`α`、`β`、U+00A0、一个 emoji、一个泰文字符
- 预期：`2024`/`[12]`/`α`/`β` 保留在翻译请求文本中；该段占位符数 == 0
- oracle：翻译请求内容断言 · **M1** · 合法

#### FORM-03 · 逗号可在公式中间不可在开头
- 源码：`styles_and_formulas.py:425-443`、状态机 `:511-514`
- 触发：`f(x, y)` 的逗号属公式；`见图 3, 表 2` 的逗号不应把两个数字连成公式
- V1：相关（inline 公式边界确定）
- 构造：一句 `The result f(x, y) = 1, 2, 3 shows ...`
- 预期：`f(x, y)` 为一个公式单元；`1, 2, 3` 不合并成公式
- oracle：IL 结构断言（formula composition 数与文本）· **M3** · 合法

#### FORM-04 · 上下标双阈值 0.79 / 1.1
- 源码：`styles_and_formulas.py:471-503`。进入角标态 `font_size < prev × 0.79`；维持态 `< prev × 1.1`；段首用 next 判断。源码注释自陈 0.79 是为避开 0.799 的大写比例
- 触发：**与 small-caps 冲突**（0.75–0.78 比例的 small caps 被误判）；**首字下沉最严重**——首字放大 3× 后第二个字符满足 `< prev × 0.79` 进入角标态，而 1.1 的维持阈值使**后续所有同字号字符都留在角标态 → 整段变公式**；正文中插入的小字号括注同理整块被判角标
- V1：**高度相关**。V3 没有上下标类，此启发式 mimus 必须自行实现
- 构造：四段——(a) `x² + y₁` 真上下标（0.6 比例）；(b) small caps 标题（0.8）；(c) drop cap 段落（首字 3×）；(d) 正文中插 7pt 括注
- 预期：(a) 上下标进公式；(b)(c)(d) 全部作为可翻译文本进请求，占位符数 == 0
- oracle：翻译请求内容断言 + IL 结构断言 · **M1**（drop cap 属严重）/ **M3**（其余）· 合法

#### FORM-05 · sticky `first_is_bullet` 永久关闭整段角标检测
- 源码：`styles_and_formulas.py:409, 419-423, 594`（flag 在整段所有 composition 间传递且永不复位）
- 触发：列表项 `• x² + y²` —— bullet 关闭角标检测，上标须靠字体/字符集路径才能进公式
- V1：相关（此 flag 的存在本身说明 FORM-04 的 0.79 阈值不可靠）
- 构造：项目符号列表，每项含 `• x² is greater than y₁`
- 预期：上下标识别结果与非列表场景一致
- oracle：IL 结构断言 · **M3** · 合法

#### FORM-06 · 纯数字公式回退为文本
- 源码：`styles_and_formulas.py:950-958` + `:621-648`。全部字符都在模型公式框内则不回退；否则文本匹配 `^[0-9, .]+$` 且 `y_offset ≤ 0.1` 则转回普通文本行
- 触发：把 FORM-02 误判的 `2024`、`3.14` 捞回；但 `y_offset > 0.1` 的上标数字不捞
- V1：相关（若 mimus 修好 FORM-02，此补丁可省）
- 构造：一句含 `In 2024, we measured 3.14 and 1, 2, 3.`
- 预期：这些数字全在翻译请求文本中，占位符 == 0
- oracle：翻译请求内容断言 · **M3** · 合法

#### FORM-07 · 按括号层级拆分公式
- 源码：`styles_and_formulas.py:960-1008` + `:1185-1223`；括号集合含 `(cid:8)/(cid:9)` 等 CID 形式（`layout_helper.py:47-48`）
- 触发：`(a, b)` 不拆（括号内）；`x, y, z` 拆成三公式 + 两逗号文本；括号不匹配（LaTeX `\left.`）时用 `max(0, level-1)` 兜底
- V1：相关但优先级低
- 构造：一句含 `for all x, y ∈ (a, b), we have f(x, y) > 0`
- 预期：占位符计数正确（`(a,b)` 计一个而非三个）
- oracle：翻译请求内容断言 · **M3** · 合法

#### FORM-08 · 公式合并的四组条件
- 源码：`styles_and_formulas.py:1064-1183`。同一 `line_id` 且满足其一：相邻且 x 包含 + y 相交（容差 1.0）／相邻且 x 邻接（容差 2.0）+ y-IoU > 0.5／两者各只有一个相同的 `formula_layout_id`／整体 IoU > 0.8。循环至收敛（最坏 O(n³)）
- 触发：源码注释自陈"角标可能被识别成单独公式，需要合并"——`x²ᵢ`、`\frac` 分子分母分裂、`∫` 上下限。**陷阱**：第三条会把同一大框内相隔很远的两段公式合并，合并后 bbox 取并集 → 排版时占据整行
- V1：相关
- 构造：一行 `x²ᵢ + y₃ⱼ = z`，上下标 6pt、正文 10pt
- 预期：`x²ᵢ` 是**一个**公式单元（非 3 个）；整行公式单元数符合声明值
- oracle：IL 结构断言（composition 数 + 各 bbox）· **M3** · 合法

#### FORM-09 · 公式 offset 的四道非对称裁剪
- 源码：`styles_and_formulas.py:807-903`。同行参照字符判据 `y_true_iou > 0.6`（分母取两框较小高度）；裁剪：`|x_offset|<0.1→0`、`>10→0`、`<-5→0`；`process_page_offsets` 被调用两次（`:369`、`:379`）
- 触发：非对称裁剪是在压制"找错参照字符"——行内公式与文本间有大空隙（右对齐公式编号）、行距紧时上一行字符被误当同行
- V1：相关（inline 公式基线对齐）
- 构造：一行 `The value E = mc² is constant`，行距 11pt（紧），上一行亦有文字
- 预期：公式垂直基线与同行文本对齐，偏差 < 0.5pt
- oracle：几何断言 · **M3** · 合法

#### FORM-10 · 富文本占位符抑制的三条豁免
- 源码：`il_translator.py:678-702`；判据在 `layout_helper.py:344-375`。其中"忽略字号"档的比例窗是 **0.7–1.3**，源码注释称是为首字母放大效果
- 触发：0.7–1.3 窗口很宽——10pt 正文中的 7.5pt 小字注释（比 0.75）被判同样式 → 字号信息丢失。另有"占位符 > 40 则整段关闭富文本"（`:724-729`）
- V1：**部分相关**。mimus 政策为译文中文无斜体，斜体占位符可直接丢；粗体与字号变化仍需保留
- 构造：一段含粗体词组、斜体词组、7.5pt 括注、上标；另造一段含 45 处交替粗体
- 预期：粗体在译文保留；斜体不保留（政策）；45 处那段不崩且译文完整
- oracle：IL 结构断言 + 翻译请求内容断言 · **M3** · 合法

#### FORM-11 · curve/form 归入公式的两级匹配
- 源码：`styles_and_formulas.py:159-360`。Level 1 精确 IoU ≥ 0.95；Level 2 公式框外扩 2.0pt 后按距离打分（`spatial_analyzer.py:20-50`，距离上限 100.0）；要求 `xobj_id` 相同
- 触发：分数线、根号横杠、矩阵大括号、`\overline`；若不跟随公式移动，公式重排后线仍留原位
- V1：**相关**（inline 公式重定位的必要配套）
- 构造：一行含真 `\frac` 分数（有横线 curve）嵌在句中，且译文更长以触发 scale < 1
- 预期：重排后横线与分子分母相对位置保持（横线 bbox 中心与公式 bbox 中心 x 偏差 < 0.5pt）
- oracle：几何断言 + 渲染对比 · **M3** · 合法

#### FORM-12 · 非公式线条删除（默认关闭，阈值 0.9）
- 源码：`styles_and_formulas.py:1225-1276`；阈值在 `translation_config.py:212-214`。注意 `layout_helper.py:884/933` 的**函数默认值 0.3/0.2 与调用点的 0.9 不一致**
- 触发：默认关闭。但"下划线在译文重排后位置错乱"是真实问题
- V1：不相关（默认关闭），但需自定策略
- 构造：一段正文，其中一词带下划线 curve
- 预期：下划线要么跟随对应译文，要么删除——**不得留在原位**
- oracle：几何断言 + 渲染对比 · **M3** · 合法

#### FORM-13 · 高度异常字符黑名单已被墨迹盒机制取代
- 源码：`layout_helper.py:18-44`。原为具名字符黑名单，注释逐条记录了来源文档（arXiv 编号 + 页码 + 具体 glyph：大括号、中括号、竖线、累加号、累乘号、积分号、`√`）；现已设为 `(None,)`，注释称"由于我们有一套 bbox 解析机制了，所以现在不需要这个东西了"
- 触发：这些 glyph 的名义高度远超实际墨迹高度，会撑高公式 bbox → 影响行切分、段落框与排版行距
- V1：**相关，且是重要经验**——mimus **必须**有墨迹盒机制，否则会掉进 BabelDOC 曾经掉过的坑。那份注释里的 arXiv 清单本身就是现成的真实语料指引
- 构造：一行正文中嵌入 display style 的 `\sum_{i=1}^{n}`（glyph 高 3× 字号）
- 预期：所在段落行数不因该 glyph 增加；段落 bbox 高度增量 ≤ glyph 视觉高度
- oracle：IL 结构断言 + 几何断言 · **M1** · 合法

#### FORM-14 · 段落基准样式取交集后众数回退
- 源码：`styles_and_formulas.py:710-745`、`:747-765`（字号相同判据 < 0.02）
- 触发：段内字体/字号混杂时，若 51% 字符是小字号注释，基准样式就变成注释字号 → 整段译文按小字号排
- V1：相关
- 构造：一段中 60% 字符为 7pt 引文、40% 为 10pt 正文
- 预期：译文主字号取视觉主体字号（mimus 须明确定义规则并断言）
- oracle：IL 结构断言 · **M3** · 合法

### 3.6 表格（TABLE）

#### TABLE-01 · 表格检测仅在已有 `table` 框的页上运行
- 源码：`table_parser.py:116-127`
- 触发：**无线表格**常被漏检 → 整页不跑表格检测 → 表格内容走 fallback 被当正文翻译（与 LAYOUT-02 复合）
- V1：**待验证红利**（V3 有 `table` 类，但无线表格召回需验证）
- 构造：(a) 有线 3×4 表格；(b) 完全无线的对齐表格，内容相同
- 预期：两者都识别为表格，内容都不进翻译请求
- oracle：翻译请求内容断言 · **M1** · 合法

#### TABLE-02 · 表格框与 layout 框坐标系不一致
- 源码：`table_parser.py:141-148`（用渲染**像素**高宽翻转）vs `layout_parser.py:142-147`（用 mediabox **点** + ceil）。表格框 `extend` 追加进同一 `page_layout` 列表
- 触发：`/Rotate 90` 的横向表格页、非 72 DPI 渲染路径
- V1：相关（坐标系统一）
- 构造：一页 `/Rotate 90`，含一个横向表格
- 预期：表格 cell 框与实际单元格几何对齐，偏差 < 2pt
- oracle：几何断言 · **M1** · 合法

#### TABLE-03 · 各类 table_cell 均属"文本 layout"
- 源码：`layout_helper.py:826, 835-836`（在白名单）+ 优先级 idx 14/15/33，**远高于 `table`(65)** → BabelDOC 逐单元格翻译表格
- 触发：表格检测成功后每个 cell 成为独立可翻译段落
- V1：**政策冲突**——mimus 政策为表格不翻
- 构造：3×4 有线表格，单元格含英文短语
- 预期：翻译请求数 == 0；表格区域渲染 diff == 0
- oracle：翻译请求内容断言 + 渲染对比 · **M1** · 合法

### 3.7 排版与断行（TYPE）

#### TYPE-01 · scale 搜索状态机（1.0→0.1，两段步长 + 空间扩展 + scale 重置）
- 源码：`typesetting.py:941-1076`。CJK 行距 1.50 / 非 CJK 1.3；步长 >0.6 时 0.05 否则 0.1；scale < 0.7 触发扩展（先向下 `+2`、再向右 `-5`）；**扩展失败且未耗尽扩展档时把 scale 重置回 1.0**；全部失败则递归重来并关闭英文断行规则；最终 `min_scale = 0.1`（字号缩到 10%）。扩展上限 `cropbox.x2 × 0.9` / 下限 `cropbox.y × 1.1`——**CropBox 原点非零时这两个乘法完全错误**
- 触发：紧凑版面（双栏、幻灯片）中的长段落；短标题与图注（中译文虽通常更短，但 `Fig. 1 → 图 1` 这类反例存在）
- V1：**高度相关**——这是"译文框不越界"断言的实现机制。mimus 必须避免"缩到 0.1 也硬塞"的静默劣化
- 构造：(a) 双栏 5 行英文正文，栏宽 240pt；(b) 高度恰好 3 行的图注框，译文需 4 行；(c) 单行标题框，译文需 2 行；(d) 段落下方紧邻图、右侧紧邻另一栏（两个方向都无法扩展）；(e) CropBox 原点非零的页
- 预期：(a) scale == 1.0；(b)(c) 要么明确扩框、要么明确保留原文，**不允许 scale < 阈值的静默缩小**；(d) 明确失败并保留原文；(e) 扩展边界计算正确
- oracle：几何断言 + IL 结构断言（scale 值）+ 翻译结果断言（失败时保留原文）· **M3**（其中 CropBox 非零原点那条为 **M1**）· 合法

#### TYPE-02 · 文档级 scale 众数封顶
- 源码：`typesetting.py:865-939`。按 `unit_count` 加权收集全文档 scale，取众数集合的最小值，**把所有高于众数的段落 scale 强行拉到众数**
- 触发：少数密集页把众数拉低 → 全书字号跟着降。BabelDOC 自身文档的 Limitation 4 已承认此问题
- V1：**相关**——是否做全局字号一致化是明确的设计决策
- 构造：20 页文档，18 页普通正文（scale 1.0）+ 2 页密集小框版面（scale ≈ 0.7），使加权众数偏向 0.7
- 预期：18 页正文字号不被那 2 页拖累（scale == 1.0）
- oracle：IL 结构断言 + 渲染对比 · **M3** · 合法

#### TYPE-03 · 行进阶下限取三者最大（中英混排保护）
- 源码：`typesetting.py:1425-1435`，附有长注释说明成因：整行拉丁字符的逐 glyph bbox 高度远小于 CJK em，若按此推进会让下一行 CJK 压上来
- 触发：**中英混排段落中某行全为拉丁字符**（引文、URL、参考编号、代码片段）
- V1：**高度相关，且是已验证的正确经验，直接采用**。目标语中文，中英混排是常态
- 构造：一段中文译文中夹一整行 `(Smith et al., 2020, https://doi.org/10.xxxx)`
- 预期：相邻行 bbox 不重叠；各行 baseline 间距标准差 < 0.5pt
- oracle：几何断言 + 渲染对比 · **M1** · 合法

#### TYPE-04 · 英文断行保护在失败后被取消
- 源码：`typesetting.py:31-87` 的 `LINE_BREAK_REGEX`（约 40 个 Unicode 区段）+ `:213-219` + `:1285-1298`；失败路径 `:1064-1073` 把 `use_english_line_break` 置 False → **英文单词从中间劈开**
- 触发：窄框（表头、图注、边注栏）中的长英文词、化学名、URL
- V1：**相关且政策相反**——劈词属于坏译文，违反 mimus 的失败哲学
- 构造：宽 60pt 的表头框，译文含超长英文词或长 URL
- 预期：不劈词；要么扩框，要么保留原文并记录失败
- oracle：翻译结果断言 + 几何断言 · **M3** · 合法

#### TYPE-05 · 悬挂标点与行尾禁则
- 源码：`typesetting.py:319-376`（46 个悬挂标点，含英文标点、中文点号、**所有右括号**、`～-–—`、`·`、`/`）与 `:387-411`（15 个不可行尾的左括号/左引号）。悬挂标点**跳过所有换行检查**
- 触发：中文排版禁则。注意 `-` 与 `/` 也在悬挂集合中 → 英文连字符与 URL 斜杠可挂出边界
- V1：**相关**（中文译文排版必做），这份列表可直接复用
- 构造：一段中文译文，构造使各行末尾恰好落在 `，`、`。`、`）`、`（`、`"` 上
- 预期：`，。）` 悬挂（可超右边界 ≤ 1 字宽）；`（"` 不出现在行尾
- oracle：几何断言（各行末字符 unicode + x2 相对 box.x2）· **M3** · 合法

#### TYPE-06 · 中英混排 0.25 汉字宽间隙
- 源码：`typesetting.py:1373-1398`（条件：CJK 属性异或、同行、不在黑名单 `。，：？！`、非行首、非空格）；`space_width` 定义见 `:1330`，实际插入量为 0.25 汉字宽。CJK 判定见 `:228-297`
- 触发：中文译文中夹英文术语/数字——常态
- V1：**高度相关**，直接采用
- 构造：译文 `我们使用 BERT 模型在 GLUE 上评测。`
- 预期：`用`/`B`、`T`/`模` 之间有约 0.25 汉字宽间隙；`。` 前后无额外间隙
- oracle：几何断言（相邻字符 x 间距）· **M3** · 合法

#### TYPE-07 · 段落垂直冲突下压（required_gap 0.5 / 3）
- 源码：`typesetting.py:1157-1215`。`required_gap = 0.5 if para_height < 36 else 3`；冲突时抬高上方段落的 `box.y`。**`:1194-1199` 的条件存在运算符优先级问题**：`not (A and B and C or D)` 中 `and` 绑定强于 `or`，实际语义偏离"横向不重叠则跳过"的意图
- 触发：紧密堆叠的段落（列表、表格式布局、幻灯片）。抬高 box.y 会压缩可用高度 → 反过来影响 TYPE-01 的 scale 搜索
- V1：相关
- 构造：三个高度各 20pt 的段落框，垂直间距 0.3pt（小于 required_gap）
- 预期：处理后三框互不重叠且各自仍能容纳内容；否则明确报空间不足
- oracle：几何断言 · **M3** · 合法

#### TYPE-08 · 排版失败不中断，继续溢出排版
- 源码：`typesetting.py:1440-1444`，源码注释明写"这里不要 break，继续排版剩余内容"
- 触发：所有 scale 都塞不下时以 `min_scale=0.1` 排版，字符**直接排到框外/页外**
- V1：**相关且政策相反**——这正是 mimus"宁可保留原文，绝不静默产出坏译文"要否定的行为
- 构造：一个 40×12pt 的 `paragraph` 小框，内含 3 个英文词，译文更长
- 预期：**不产出溢出**。要么保留原文，要么明确扩框到不与其他元素冲突的范围
- oracle：几何断言（不存在越出 layout box 超阈值的字符）· **M1**（失败哲学的核心断言）· 合法

#### TYPE-09 · 字号众数计算无保护，异常被裸 except 吞掉
- 源码：`typesetting.py:1326-1327`（`statistics.mode(font_sizes)` 无 try）；异常被 `:1003-1005` 的裸 `except: pass` 吞掉 → 该 scale 被静默跳过。对比 `:1342-1346` 的均值计算是有保护的
- 触发：段内所有 unit 都是公式（无字号信息）
- V1：相关（不得静默）
- 构造：一个只含 display 公式的段落被误标为 `paragraph`
- 预期：明确的"无字号信息"处理路径，不静默跳过
- oracle：错误/降级记录断言 + 渲染对比 · **M3** · 合法

#### TYPE-10 · passthrough 快路径
- 源码：`typesetting.py:1264-1269` 与 `:902-905`。段内所有 unit 均为原字符/公式（即未被翻译）时直接 scale = 1.0 原样输出
- 触发：未翻译的段落（太短、纯数字、纯公式、CID、翻译失败）——这是"保留原文"的实现路径
- V1：**高度相关**——mimus 的保留原文必须走这条无损路径
- 构造：一页混合 3 段可翻译正文 + 1 段纯数字 + 1 个公式段 + 1 段 3 字符标题
- 预期：后三者在输出 PDF 中与输入渲染等价
- oracle：渲染对比（这些区域 diff == 0）· **M1** · 合法

#### TYPE-11 · descent 补偿按众数下移整段
- 源码：`remove_descent.py:17-48, 150-168`。每字符 `descent = font.descent × font_size / 1000`；段落层面取众数下移整框
- 触发：段内混用多种字体（不同 descent）时，众数只代表主流字体，其余字符对不齐
- V1：相关（基线对齐）
- 构造：一段 Times 正文（descent −216）中夹入 Courier（descent −300）的行内代码
- 预期：所有字符 baseline y 一致，偏差 < 0.3pt
- oracle：几何断言 · **M3** · 合法

#### TYPE-12 · 首行缩进复现为固定 2 汉字宽
- 源码：`typesetting.py:1361-1362`。原文实际缩进量（1em / 2em / 0.5in / 悬挂）不被保留
- 触发：中文公文的 2 字缩进恰好正确；英文 0.5 inch 缩进变成约 20pt
- V1：相关（中文目标语下 2 字缩进是正确惯例，但需显式声明为政策）
- 构造：三段分别为 1em / 2em / 0.5in 缩进的正文
- 预期：译文均为 2 汉字宽缩进且三段一致
- oracle：几何断言（首行首字符 x − 段落 box.x）· **M3** · 合法

### 3.8 扫描件判定（SCAN）

#### SCAN-01 · 单页判据：删除文字层后 SSIM > 0.95
- 源码：`detect_scanned_file.py:151-172`。以 **72 DPI** 渲染原页 → 重写 content stream 去掉所有字符 → 再渲染 → 灰度 SSIM > 0.95 判为扫描页
- 触发：真扫描件（不可见 OCR 文字层）判定正确。**误判风险**：文字极少的页（封面大标题、纯图表页、扉页）删掉几个字对 SSIM 影响 < 5%；满页深色图 + 少量白色文字同理。**漏判风险**：OCR 文字层可见时 SSIM 显著下降 → 判为原生。**72 DPI 系统性偏向"判为扫描"**——低 DPI 下小字只占几个像素
- V1：**相关**（V1 必须正确拒绝扫描件且不误判原生 PDF）
- 构造：五份单页——(a) 满页扫描图 + 不可见文字层（`3 Tr`）；(b) 满页扫描图 + 可见文字层；(c) 正常 1000 字文字页；(d) 只有一行大标题的扉页；(e) 满页深色照片 + 20 字白色标题。**(d)(e) 是重点误判语料**
- 预期：(a)(b) 判为扫描并拒绝；(c)(d)(e) 判为原生并正常翻译
- oracle：退出码 + 错误类型断言 · **M1** · 合法

#### SCAN-02 · 文档级 80% 阈值存在单页漏判与顺序依赖
- 源码：`detect_scanned_file.py:102-125`。`threshold = max(0.8 × total, 1)`；`non_scanned_threshold = total - threshold`；循环条件不满足时**直接计入 non_scanned 且不再检测**
- 触发：**`total == 1` 时 `non_scanned_threshold == 0`，循环条件 `non_scanned < 0` 恒假 → 唯一一页永不检测 → 单页扫描件 100% 漏判**。`total == 4` 时只要 1 页非扫描即停止检测。提前退出还使结论**依赖页面顺序**。混合文档（3 页扫描封面 + 20 页原生）判为非扫描，那 3 页会被当原生处理
- V1：**高度相关**——单页漏判与顺序依赖都必须避免
- 构造：(a) 单页扫描件；(b) 4 页，第 1 页原生 + 后 3 页扫描；(c) 同样 4 页但顺序颠倒；(d) 20 页，前 3 页扫描封面 + 17 页原生
- 预期：(a) 拒绝；(b)(c) **结论相同**（顺序无关）；(d) 行为明确定义（建议继续处理但标记那 3 页不翻）
- oracle：退出码 + 逐页 `is_scanned` 标记断言 · **M1** · 合法

#### SCAN-03 · 已停用的正则快筛（值得反向采用）
- 源码：`detect_scanned_file.py:68-82`；调用点 `high_level.py:884-893` **整块被注释**。规则：content stream 每命中一次 `(/Artifact|/P)(<</MCID |\s+BDC)` 或 `\s3\s+Tr\s` 计 1 分，总分 > 页数 × 0.8 判为扫描
- 触发：`3 Tr`（不可见文字渲染模式）是 OCR 文字层的标志性特征；`/Artifact BDC` 是 OCR 工具常用标记。这是 O(1) 正则预筛，比 SSIM 渲染快几个数量级，但被停用（大概率因误判率）
- V1：**相关，且是省时机会**——mimus 可将其用作**快速正判**（命中即拒绝）而非快速负判，这样误判方向是安全的
- 构造：(a) 含 `3 Tr` 文字层的扫描页；(b) 正常原生 PDF；(c) **陷阱**：正文正常但含少量 `3 Tr` 隐藏水印的原生页
- 预期：(a) 拒绝；(b)(c) 正常翻译
- oracle：退出码 · **M1** · 合法

#### SCAN-04 · 判定后的分支带全局配置副作用
- 源码：`detect_scanned_file.py:125-142`（OCR 分支会就地修改多项全局配置并重置所有字符的 render_order 与图形状态）
- 触发：V1 只走"拒绝"分支
- V1：相关的部分仅是**拒绝路径的错误消息要清晰可分类**；OCR 分支不相关。另需记取教训：不应模仿这种在 pass 内修改全局配置的副作用式设计
- 构造：任一扫描件 fixture
- 预期：以退出码 2 退出，stderr 含"扫描件暂不支持"类的明确分类
- oracle：退出码 + stderr 断言 · **M1** · 合法

### 3.9 已排除项

以下经分析确认不进入 Corpus v1，记录以免重复分析：

| 项 | 源码 | 排除理由 |
|---|---|---|
| `add_text_fill_background` 白底遮盖 | `paragraph_finder.py:89-122` | 仅 OCR workaround 路径调用，V1 无此路径（V2 再评估） |
| 表格线保护阈值 | `layout_helper.py:883-929` | 依附于默认关闭的删线功能；当前是"两个错误相互抵消"的状态 |
| `_merge_overlapping_clusters` | `paragraph_finder.py:514-598` | **死代码**，从未被调用 |
| `_sort_characters_in_lines` | `paragraph_finder.py:1032` | **死代码**，调用点 `:305` 已注释 |
| `_get_effective_y_bounds` 的 IoU 0.5 分支 | `paragraph_finder.py:610-613` | **死代码**，前一行已 return |
| `HEIGHT_NOT_USFUL_CHAR_IN_CHAR` 黑名单 | `layout_helper.py:18-44` | 已设为 `(None,)`，被墨迹盒机制取代（经验已记入 FORM-13） |
| `DetectScannedFile.fast_check` 调用 | `high_level.py:884-893` | 调用点整块注释（策略价值已记入 SCAN-03） |

### 3.10 PP-DocLayoutV3 红利验证清单

以下 BabelDOC 启发式的存在理由是**补 DocLayout-YOLO 的能力缺口**（10 类粗标签 + 无阅读顺序）。换用 PP-DocLayoutV3（25 类 + 自带阅读顺序）后**可能不需要**，但每一条都必须用语料验证红利是否兑现——不兑现就得把补丁抄回来。这组验证直接决定 mimus 的 midend 要写多少代码，应在 M0 阶段跑通。

| # | 补丁 | 补的缺口 | V3 对应能力 | 验证 case | 不兑现的代价 |
|---|---|---|---|---|---|
| D1 | display/inline 公式重标 | YOLO 无行内/独立公式区分 | 原生两类 | LAYOUT-03 | 需重新实现 IoU 重标 |
| D2 | 跨栏合并（y2 差 > 20） | **无阅读顺序**，靠 y 突跳猜栏切换 | 模型 reading order | ORDER-01、ORDER-02 | 段落顺序错乱、跨栏句被切断 |
| D3 | 跨页合并 | 同上（跨页版） | **不兑现**——模型顺序是页内的 | ORDER-03 | 必须自行实现，**建议直接实现** |
| D4 | fallback 全页聚类（9 个魔数） | YOLO 漏检严重 | 25 类覆盖更全 | LAYOUT-08、LAYOUT-02 | 漏检文字块完全丢失（不翻不显） |
| D5 | 交替行号合并 | YOLO 无行号类 | `aside_text` **可能**框出行号 | PARA-07 | 带行号文档段落全碎 |
| D6 | 68 项优先级表 | 需仲裁多来源框且历史积累 | 单模型 25 类，来源统一 | LAYOUT-01 | 嵌套框归属混乱 |
| D7 | 目录点线切段 | YOLO 无目录类 | **待确认** 25 类是否含目录 | PARA-04 | 目录条目与页码粘连 |
| D8 | 文本白名单含 header/footer/seal | YOLO 无这些类 | 原生 `header`/`footer`/`seal`/`reference` | LAYOUT-07 | 页眉页脚被翻译（违反政策） |
| D9 | 二级表格检测模型 | YOLO 只给粗框、无单元格 | `table` 类；单元格粒度待确认 | TABLE-01 | 表格内容走兜底被误翻 |
| D10 | 上下标 0.79/1.1 + sticky flag | YOLO 无上下标概念 | **不兑现**——25 类无上下标 | FORM-04 | **必须自行实现**，drop cap 会毁整段 |
| D11 | 公式字体三层正则（约 130 条） | 行内公式全靠字体名猜 | `inline_formula` 类可大幅替代 | FORM-01 | 退化为纯字体名匹配 |
| D12 | 公式合并四组条件 | 上下标被切成独立公式需缝回 | 部分缓解 | FORM-08 | 公式碎片化、占位符暴增 |

**验证方法**：把 D1–D12 的最小构造做成一组**双跑语料**——同一 fixture 分别以 (a) 只信任模型输出、(b) 模型 + BabelDOC 同款补丁 两种配置运行，比对 IL 结构断言。结果一致即红利兑现；(a) 明显更差则把补丁抄回。

**2026-08-21 裁决**（M0 实验 1，完整证据见 [04-m0-experiment-1.md](04-m0-experiment-1.md) §4）：

| 兑现（可从 midend 删去） | 不兑现（midend 必须自己写） |
|---|---|
| D1 公式两类原生区分 —— 且强于两个文本 oracle（`unit-form-02` 上模型能把被正文包住的公式单独框出，poppler 与 mutool 都做不到） | D3 跨页合并 —— 第 7 列只在**页内**有序 |
| D2 跨栏合并 —— 且没有 y 突跳判据的假阳性（`unit-order-05`） | D10 上下标 / 首字下沉 —— 25 类无对应概念 |
| D5 行号识别 —— `aside_text` 确实单独框出行号 | D6 嵌套框归属 —— 模型不产出嵌套框；冲突形态变为"同一 query 多类别"，仲裁逻辑规模远小于 68 项但仍要写 |
| D11 公式字体正则 —— `inline_formula` 可替代约 130 条正则 | D9 表格单元格 —— 只给一个整框 |
| D12 公式碎片缝合 —— 模型给单个整框，无碎片化 | **D4 窄栏间距兜底 —— 栏间距 8pt 时模型把整片双栏正文误判为 `table`（0.732，单框盖两栏），比漏检更危险；30pt / 60pt 的单变量对照均正常。兜底聚类必须保留** |
| D7 目录**类别** —— `content` 即目录类 | D7 目录**条目切分** —— 只给整块框 |
| D8 `header` | D8 `footer` / `reference` / `seal` —— 实测分别被判成 `number`、`text`、完全漏检；政策区域判定须叠加位置规则 |

---

## 4. 首批 fixture 清单（M0 / M1）

本节把矩阵中标为 M0 / M1 的 case 落成具体 fixture。M-1 当时的交付物是这份清单本身；进入后续实现 issue 后，每份 PDF 仍必须逐一通过 §2 的生成合同与 §2.8 的独立验收才能入库。

### 4.1 基线 fixture（变异父本与往返参照）

三份合法基线，是绝大多数变异 fixture 的父本，也是"解析→排版→写回"往返的参照系。它们必须最先建立并通过验收。

| fixture | 内容 | 作用 |
|---|---|---|
| `unit-base-01-single-line` | 单页、单行文本、嵌入钉死的拉丁字体子集、content stream 不压缩、无 Info 字典 | 全部 `mal-*` 字节变异的父本；往返精度参照 |
| `unit-base-02-two-column` | 单页双栏正文（现实排版引擎产出，栏宽与栏距记入 manifest） | 阅读顺序与多栏类实验的参照 |
| `unit-base-03-structured` | 单页正文 + 三层书签（含精确 destination、URI action、命名目标、颜色/粗体、折叠状态）+ 注释 + 表单域 + 可选内容组 | 增量写回的结构守恒参照 |

### 4.2 M0 批次（服务于三个风险探测实验）

#### 排期：不等齐这约 45 份（决策 #33）

三个实验不共享前置依赖，等齐全部 fixture 再开工是浪费。**每个实验的最小 fixture 集独立验收通过后，该实验即刻启动。**

顺序由依赖链而非编号决定，这里有一处反直觉的倒置：

- **实验 1 先行。** 它的 fixture 全部由现实排版引擎（Typst / LaTeX）产出，走 §2.1 的双解析器裁定例外，**完全不依赖自建的确定性 PDF writer**。它的依赖链最短。
- **实验 2、3 待 writer 就绪后并行。** 它们要的是精确 fixture 与字节级畸形变异，必须等自建 writer 可用。

顺带的好处是 ADR-0002 遗留的阅读顺序验证被提到最前——它决定 midend 要写多少代码（红利清单 D1–D12），越早知道越好。

**最小首批 10 份**（每组通过 §2.8 验收即启动对应实验）：

| 实验 | 最小 fixture |
|---|---|
| 1 | `unit-base-02-two-column`、`unit-order-01-natural`、`unit-order-02-reversed`、`unit-order-03-interleaved` |
| 2 | `unit-base-01-single-line`、`mal-stream-06-glued-tokens`、`unit-parse-04-contents-array-numeric-split`、`unit-font-01-std14-custom-widths` |
| 3 | `unit-base-03-structured`、`unit-write-02-shared-resources` |

余下约 35 份在对应实验推进过程中补齐。**M-1 仍整体收口**——本节切分只影响 M0 内部的启动顺序，不改变 M-1 的四条收口断言。

#### 实验 1 · 模型能力与阅读顺序（对应 ADR-0002 遗留项）

| fixture | 覆盖 case | 单一变量 |
|---|---|---|
| `unit-order-01-natural` | ORDER-01 | content stream 为自然顺序（基线） |
| `unit-order-02-reversed` | ORDER-01 | 段落倒序绘制（视觉渲染须与 01 逐像素一致） |
| `unit-order-03-interleaved` | ORDER-01 | 双栏行交错绘制（同上） |
| `unit-order-04-column-continuation` | ORDER-02 | 左栏末句在栏底截断、右栏首段续接 |
| `unit-order-05-false-jump` | ORDER-02 | 单栏但正文起点人为抬高（跨栏判据的假阳性） |
| `unit-form-01-display` | LAYOUT-03 | 独立居中公式 |
| `unit-form-02-display-enclosed` | LAYOUT-03 | 同一公式但被正文框包住约 60% |
| `unit-form-03-inline` | LAYOUT-03 | 行内公式嵌在句中 |
| `unit-geom-01-rotate-0/90/180/270/neg90` | GEOM-04 | `/Rotate` 五个取值，内容完全相同（5 份） |
| `mal-geom-02-rotate-45` | GEOM-04 | 非 90 倍数的旋转值 |
| `unit-geom-03-nonzero-origin-raster` | GEOM-05 | MediaBox 原点非零且无 CropBox |
| `unit-geom-04-oversized-page` | GEOM-05 | 接近规范上限的超大页面 |

`mal-geom-02-rotate-45` **推迟到实验 2**：按 §2.5 它必须从合法父本做字节级变异，而字节级变异要等自建的确定性 writer 就绪。

#### 实验 1 · 红利清单验证批（§3.10 的 D1–D12）

D1–D12 决定 midend 要写多少代码，与阅读顺序同属实验 1 的产出，因此这批 fixture 与上表同期入库：

| fixture | 覆盖 case | 验证的红利 | 单一变量 |
|---|---|---|---|
| `unit-order-06-cross-page` | ORDER-03 | D3 | 页边界落在一句话中间（真阳性）与落在两个独立段落之间（假阳性）各一处 |
| `unit-layout-01-nested-boxes` | LAYOUT-01 | D6 | 内层文字 100% 落在表格框内，同时被左侧正文块擦到 2pt |
| `unit-layout-02-table-only` | LAYOUT-02、TABLE-01 | D4、D9 | 页面上除一个 3×3 有线表格外没有任何正文 |
| `unit-layout-07-policy-zones` | LAYOUT-07 | D8 | 同一页并置页眉、页脚、参考条目、印章与两段正文 |
| `unit-layout-08-narrow-gutter` | LAYOUT-08 | D4 | 栏间距压到 8pt（约 0.8 字宽） |
| `unit-para-04-toc` | PARA-04 | D7 | 同一页并置六种目录 leader |
| `unit-para-07-line-numbers` | PARA-07 | D5 | 正文左侧另有一列每 5 行一个的行号 |
| `unit-form-04-superscript` | FORM-04 | D10 | 并置真上下标、small caps、首字下沉与小字号括注 |
| `unit-form-08-formula-fragments` | FORM-08 | D12 | 每个变量同时带上标与下标 |

D2（跨栏合并）由 `unit-order-01`–`05` 覆盖，D11（公式字体正则）由 `unit-form-01`/`-03` 覆盖，两者不另立 fixture。

其中 6 份（`layout-01`、`layout-02`、`layout-08`、`para-04`、`form-04`、`form-08`）的**块划分本身**被实测证明无法由两个独立解析器一致裁定——这正是它们所对应的失效模式的直接证据。它们改用 `glyphs` 检查断言「这一页有且只有这些字符」，分歧逐条记在各自 manifest 的 `[[adjudication]]` 里。

#### 实验 2 · 走查与 PDFium 对齐（对应 ADR-0006 核心假设）

| fixture | 覆盖 case | 单一变量 |
|---|---|---|
| `unit-parse-01-ascii85` / `-02-cascade` / `-03-lzw-earlychange` | PARSE-03 | 三种流编码（3 份） |
| `unit-parse-04-contents-array-numeric-split` | PARSE-04 | 数字 token 跨流边界（合法） |
| `mal-parse-05-contents-array-string-split` | PARSE-04 | 字符串跨流边界（畸形） |
| `mal-parse-06-deep-nesting` | PARSE-08 | 512 层嵌套数组 |
| `mal-parse-07-parent-cycle` | PARSE-10(c) | `/Parent` 自环 |
| `unit-stream-01-bx-ex-unknown-op` | STREAM-01 | `BX…EX` 内的未知操作符 |
| `unit-stream-02-type3-d1` | STREAM-01 + FONT-06 | Type3 CharProc 的 `d1` |
| `mal-stream-03-arity-excess` / `-04-arity-short` | STREAM-02 | 操作数过多 / 不足（2 份） |
| `mal-stream-05-unbalanced-Q` | STREAM-04 | `Q` 多于 `q` |
| `mal-stream-06-glued-tokens` | STREAM-06 | 数字与操作符粘连 |
| `mal-stream-07-double-decimal` | STREAM-06 | `10.5.3` 式双小数点 |
| `unit-stream-08-inline-image-EI-in-data` | STREAM-09 | 图像数据内含 ` EI ` 字节序列 |
| `unit-stream-09-inline-image-no-L` | STREAM-09 | 无 `/L` 且后随白名单外操作符 |
| `unit-font-01-std14-custom-widths` | FONT-02 | 标准 14 字体带非标准 `/Widths` |
| `unit-cmap-01-identity-no-tounicode` | CMAP-04 | Identity-H 无 ToUnicode 但字体 cmap 完好 |
| `mal-xobj-01-self-recursive` / `-02-mutual-recursive` | XOBJ-02 | 自环 / 互环（2 份） |
| `mal-xobj-03-form-no-bbox` | XOBJ-04 | Form 缺 `/BBox` 且带非单位 `/Matrix` |
| `unit-xobj-04-inherited-resources` | XOBJ-07 | XObject 与页面同名字体指向不同字体 |

#### 实验 3 · 增量写回（对应 ADR-0003 §2）

| fixture | 覆盖 case | 单一变量 |
|---|---|---|
| `unit-write-01-bookmarks-rich` | WRITE-06 | 完整书签结构（= `unit-base-03`） |
| `unit-write-02-shared-resources` | WRITE-04 | 两页共享同一间接 `/Resources` |
| `unit-write-03-resources-gen-nonzero` | WRITE-04 | generation ≠ 0 的资源引用 |
| `unit-write-04-xobj-in-objstm` | XOBJ-10 | Form XObject 位于压缩对象流内 |
| `unit-write-05-indirect-resources-objstm` | WRITE-01 + WRITE-02 | `/Resources` 间接且指向 ObjStm 内字典 |
| `unit-geom-05-nonzero-origin-boxes` | GEOM-02 | MediaBox 原点非零 + CropBox 并存 |
| `unit-cmap-02-mixed-codespace` | CMAP-06 | 混合位宽 codespacerange |
| `unit-xobj-05-singular-ctm` | XOBJ-08 | 奇异 CTM 后的坐标系恢复 |
| `mal-parse-08-broken-objstm` | PARSE-11 | ObjStm 声明对象数与实际不符 |
| `mal-parse-09-outlines-cycle` | PARSE-11 | `/Outlines` 的 `/Next` 自环 |

**M0 合计约 45 份**（含变体展开）。

### 4.3 M1 批次

M1 覆盖矩阵中标为 M1 的 60 个 case。按域汇总（fixture 数含变体展开）：

| 域 | case | 约需 fixture |
|---|---|---|
| PARSE | 01, 02, 05, 07, 09, 10(a,b) | 7 |
| STREAM | 05, 07, 08, 10, 11 | 7 |
| FONT | 01, 03, 05, 06, 07 | 8 |
| CMAP | 01, 02, 05, 07, 08 | 6 |
| XOBJ | 01, 03, 05, 06 | 4 |
| GEOM | 01, 03 | 4 |
| WRITE | 03, 07, 08 | 3 |
| DOC | 01, 02, 03, 04 | 13（加密两档 + 扫描四档 + 非直立七档） |
| LAYOUT | 01, 02, 05, 06, 07, 08 | 6 |
| PARA | 01, 03, 10, 13, 14, 15, 16 | 12 |
| FORM | 02, 04（drop cap 部分）, 13 | 6 |
| TABLE | 01, 02, 03 | 4 |
| TYPE | 03, 08, 10 | 4 |
| SCAN | 01, 02, 03, 04 | 9 |
| **合计** | **60 个 case** | **约 93 份** |

### 4.4 规模与取舍

M0 + M1 合计约 **138 份 fixture**（覆盖 87 个 case）——这远超早期那批 23 份，且每份都要过确定性、独立解析、独立渲染三重验收。诚实地说，这是一笔可观的前期投入。

三点缓解：

1. **M-1 只交付清单**，不交付 PDF。真正的生成工作按里程碑分批：M0 那约 45 份先行（它们直接解锁三个风险实验），M1 的 93 份随 pass 链逐步落地。
2. **基线 + 单变量变异的结构大幅摊薄成本**——多数 `mal-*` fixture 是对同一父本的一处字节改动，生成器写一次、变异描述写一行。
3. **M0 内部已按实验切分**（§4.2 排期）：最小首批 10 份验收通过即启动对应实验，不必等齐 45 份。实验 1 因为不依赖自建 writer，可以最先跑起来。

矩阵中标为 M3 的 42 个 case 不在首批清单内，随质量攻坚阶段按需落地。

---

## 5. 附录：BabelDOC 魔数速查表

mimus 实现同类启发式时的参考起点值。**不是照抄目标**——每个值都应由 mimus 自己的语料重新标定，此表的作用是提供数量级与避免从零猜测。

| 常量 | 值 | 源码位置 |
|---|---|---|
| formula→isolate_formula IoU | `0.5` | `paragraph_finder.py:82` |
| 小字符豁免面积比 | `0.05` × 中位面积 | `paragraph_finder.py:463` |
| 扁平段落单行阈值 | `< 5` pt | `paragraph_finder.py:695` |
| 行切分扫描步长 | `0.25` pt | `paragraph_finder.py:708` |
| 行间隙判据 | `count < 1` | `paragraph_finder.py:727` |
| 目录点线 | `\.{20,}` | `paragraph_finder.py:866` |
| 短行切段因子 | `0.8`（默认关） | `translation_config.py:179` |
| 段落重叠切分间隙 | `±1` pt | `paragraph_finder.py:1005-1006` |
| 首行缩进阈值 | `> 1` pt | `paragraph_finder.py:167` |
| CID 段落比例 | `> 0.8` | `paragraph_finder.py:225` |
| 空格推断阈值 | `sorted(set(d))[1]` | `layout_helper.py:257`, `:517` |
| 换行回退阈值 | `char_width × 10` | `layout_helper.py:149` |
| 字符→公式框 IoU | `> 0.4` | `layout_helper.py:877` |
| 墨迹盒/度量盒回退 | `< 0.2` | `layout_helper.py:864` |
| 同样式字号容差 | `< 0.02` | `layout_helper.py:351` |
| 同样式（忽略字号）比例窗 | `0.7 ~ 1.3` | `layout_helper.py:363` |
| 上下标进入/维持阈值 | `< prev × 0.79` / `× 1.1` | `styles_and_formulas.py:479, 489` |
| 公式合并 y-IoU / 整体 IoU | `> 0.5` / `> 0.8` | `styles_and_formulas.py:1128, 1135` |
| 公式合并 x 邻接 / y 相交容差 | `2.0` / `1.0` pt | `styles_and_formulas.py:1034, 1031` |
| offset 同行判据 | `y_true_iou > 0.6` | `styles_and_formulas.py:844, 860` |
| x_offset 裁剪 | `<0.1→0`, `>10→0`, `<-5→0` | `styles_and_formulas.py:878-885` |
| 可翻译公式模式 | `^[0-9, .]+$` | `styles_and_formulas.py:958` |
| curve 归公式（精确/容忍） | IoU `≥0.95` / 扩 `2.0`pt、距离上限 `100` | `styles_and_formulas.py:100`, `spatial_analyzer.py:24` |
| 富文本占位符上限 | `> 40` | `il_translator.py:724` |
| 最短翻译文本 | `5` 字符 | `translation_config.py:187` |
| 跨栏 y2 跳变 | `> 20` pt | `il_translator_llm_only.py:502` |
| CJK / 非 CJK 行距 | `1.50` / `1.3` | `typesetting.py:968` |
| 最小 scale / 步长 | `0.1` / `>0.6:0.05, else 0.1` | `typesetting.py:969, 1012-1015` |
| 扩展触发点 / 留边 | `scale < 0.7` / 下 `+2`、右 `-5` pt | `typesetting.py:1017, 1023, 1042` |
| 扩展上下限 | `cropbox.x2 × 0.9` / `cropbox.y × 1.1` | `typesetting.py:1603, 1639` |
| 段落间最小间隙 | `0.5`（高<36）/ `3` | `typesetting.py:1178` |
| 行进阶下限 | `max(fs·s·skip, mode_h·skip, max_h·1.05)` | `typesetting.py:1431-1435` |
| 中英混排间隙 / 首行缩进 | `0.25` / `2` 汉字宽 | `typesetting.py:1398, 1362` |
| 扫描页 SSIM / 判定 DPI | `> 0.95` / `72` | `detect_scanned_file.py:172, 156` |
| 扫描文档比例 | `max(0.8 × total, 1)` | `detect_scanned_file.py:103-104` |
| 版面检测置信度 / 输入尺寸 | `> 0.25`（无 NMS）/ `1024` | `doclayout.py:243, 219` |
| fallback band 重叠 / DBSCAN eps | `0.5` / `3.5 ×` 平均字宽 | `extract_char.py:22, 26` |
| fallback 行拆分 / 拆分 eps | 行高 `> 1.5 ×` 最大字高 / `0.5 ×` | `extract_char.py:30, 32` |
| fallback 空格插入 | gap `> 0.45 ×` 平均字宽 | `extract_char.py:36` |
| fallback 合并三阈值 | `0.6` / `0.7` / `1.5 ×` | `extract_char.py:44, 47, 49` |

---

## 6. 未决问题

调研过程中暴露、但当时的决策集未覆盖的事项。`CONTEXT.md` 的"待决清单"与本节同源，那边是索引、这边是正文。

### 6.0 已于 2026-08-21 收口

原第 1、7、9 条已在设计会话中定案，移出本节：

| 原条目 | 结论 | 落点 |
|---|---|---|
| 加密 PDF 的 V1 策略 | **一律拒绝**，退出码 2；不做权限位尊重、无密码参数；检测用 `was_encrypted()` | 决策 #31 / [ADR-0009](adr/0009-reject-encrypted-pdf.md) / DOC-03 |
| 任意角度旋转文本的政策 | 并入**非直立文本**概念（旋转/镜像/斜切 > 20°，视觉页框内度量），不翻译、原样 passthrough；字符级检测、单元级隔离 | 决策 #32 / [ADR-0007](adr/0007-ir-design.md) §5 / DOC-04 |
| M0 的 45 份是否再切分 | **按实验切分**，最小首批 10 份验收即启动；实验 1 先行（不依赖自建 writer） | 决策 #33 / §4.2 排期 |

原第 8 条（字体族选择）的**溯源部分**同时收口：改用**对象号 + 子集标签**断言，不再依赖输入输出字体不同族（§2.6）。剩余的选型部分见 6.2。

原第 2、3 条（T02 / issue #4）已于同日实测收口：

| 原条目 | 结论 | 落点 |
|---|---|---|
| 确定性生成的引擎侧机制尚未实测 | Typst / pdfTeX / LuaHBTeX 三条配方实测通过；**XeTeX 因 xdvipdfmx 随机子集标签出局**；原文关于 `\pdfvariable` 的两处描述有误已更正 | §2.5 / §2.6 实测结论表 / `corpus/toolchain.toml` |
| 验收工具链缺四件 | qpdf 12.4.0、poppler 26.08.0、mutool 1.28.2、Typst 0.15.1 均已具备并钉死；检查入口 `corpus doctor` | §2.8 / `corpus/toolchain.toml` |

### 6.1 仍需决策者拍板

1. ~~**旧语料目录仍在磁盘上**~~ → **2026-08-22 已收口**。决策者已删除 `~/Downloads/babeldoc-corpus/`（实测目录不存在）。此前的工作全程未读取、未引用其任何几何参数或生成代码，现在连误引用的可能性也一并消除。

### 6.2 已定去向，拆票执行（不需再决策）

4. **PP-DocLayoutV3 的 25 类是否含目录类未确认**（PARA-04 / 红利清单 D7）。若无目录类，目录页的条目切分需要 mimus 自行实现启发式；这会影响 M1 的工作量估计。由 M0 实验 1 顺带查证。

5. **CJK 输入 fixture 的字体选型**。可选范围小且体积大，可能不得不与输出字体（Noto Sans SC）同族。溯源手段已不依赖字体族差异，所以这纯粹是选型与体积取舍，不再是设计问题。

### 6.3 由实验给出结论，不是决策

6. **走查与 PDFium 不一致时的仲裁规则未定**。至少两处已知分歧：FONT-02（标准 14 内置度量 vs 文件 `/Widths` 的优先级）与 STREAM-02（多余操作数是跨操作符继承还是丢弃）。ADR-0006 说"PDFium text page 用作交叉校验"，但没说校验失败时以谁为准。**由 M0 实验 2 的结论文档确立一条明确规则**（倾向：以 PDF 规范为准，PDFium 与规范不符时记录偏差），否则每个分歧点都要重新争论一次。
