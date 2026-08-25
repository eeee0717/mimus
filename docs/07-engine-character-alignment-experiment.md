# 实验 5：跨引擎字符对齐残差分类

> 日期：2026-08-25
> 问题：真实论文上的 operator walk / PDFium 字符数与 Unicode 差异，哪些是提取视图
> 差异，哪些仍可能表示 walk 漏字？
> **结论：严格数组相等的前提已被语料证伪；分类器已把残差收敛到可解释类别，但本实验不
> 修改生产门禁，也不确定生产容差。**

## 0. 结论先行

`Checking list.pdf` 的拒绝只来自 67 个独立空格 show：walk 为 3,081 字符，PDFium 为
3,014 字符；3,014 个可配对字符全部在 `0.001 pt` 内精确匹配且 Unicode 全等，C/D/E/F
均为 0。

18 份故障诊断论文共 462 个输入页，其中 461 页成功分类。旧的 `0.001 pt` 单阶段比较曾
报告 D=88,554、E=88,453；
两阶段分类后，在 `0.5 pt` 实验窗口下为 D=1,103、E=302、F=10,679。旧 D/E 的主体是
字体宽度导致的同行水平漂移，不是缺字。E 只出现在 1 份 PDF 的 3 个图表页；其余 17 份
PDF 的 E 均为 0。

C 共 9,235 个，但只有 4 个是强 walk 解码链上的未解释冲突。其余为 1,203 个 PDFium
hyphen 标记、988 个 UTF-16 高代理项、28 个 ligature 展开、413 个弱 simple-encoding
冲突，以及 6,599 个 walk 未解析字符。`U+0002` 已由 `FPDFText_IsHyphen` 直接确认，不是
unmapped glyph 标记。

这份结果支持撤销“字符数组不全等就文档级拒绝”的事实前提，但尚不足以直接写生产匹配器：
F 仍含旋转文本、同位置重复绘制、无 Unicode 和序列可对应但逐字几何不可对应等多种残差；
剩余 302 个 E 与 walk 的未解析图表字形尚未建立字符级来源桥。

## 1. 范围与不变量

实验 runner 位于 `experiments/experiment-5-engine-alignment/`，独立 workspace，不进入发布
workspace。它只读取 PDF，调用生产 `walk_page()` 和 `PdfiumEngine`，输出 JSON；不翻译、
不写 PDF。

本次只增加两个观测字段：

- walk 字符记录 Unicode 解码来源：`ToUnicode`、`EmbeddedFontCmap`、
  `SimpleEncoding`、`Unresolved`；解码结果和既有失败/保留语义不变；
- 显式实验诊断入口生成的 PDFium owned snapshot 记录 `FPDFText_IsHyphen`；普通生产
  `page_characters()` 不查询该 API，生产对齐、布局和改写逻辑也不消费该字段。

没有修改 ADR-0006、ADR-0013、ADR-0014，没有修改
`validate_character_alignment`，也没有改变任何生产降级或拒绝路径。

## 2. 环境与固定输入

| 项 | 值 |
|---|---|
| 主机 | macOS 26.5.1 (25F80)，arm64 |
| Rust | rustc 1.97.1 |
| 起始 HEAD | `eebc4a1d0c1e2f53021ba5d407cde5c764f92785` |
| 对照 `origin/master` | `85fae3762b02b9707161e6b5518f6b54cbc4601c` |
| PDFium dylib SHA-256 | `df568fcd17a6a6296956aa79abea1181db187458432f360b084fec1cea7cd4d9` |
| 论文集 | 18 份，462 个输入页、461 个分类页，1,194,405 个 walk 字符，1,175,812 个 PDFium 字符 |

最终原始报告留在 `.context/engine-alignment/`：

| 文件 | SHA-256 |
|---|---|
| `checking-list-final.json` | `ee32df1d518b142633474b83e9ef3e5af58c99d7e56bf94e578c641577fd80ed` |
| `corpus-final.json` | `f6c8a84472ce93099d419bf413635ee002da07da88c43c2343d0ed16c2fca759` |
| `e-final-sweep.json` | `c33166d58c543051f914029abbe767e0d5c8dc02c27cb24d158c5fb161b58c45` |

JSON 包含每份输入的完整 SHA-256、字节数和页数。PDF 本体与 95 MiB 逐页报告不提交。

## 3. 分类装置

### 3.1 配对阶段

1. 在 `0.001 pt` 内做 baseline 多重集精确匹配；同位置重复绘制保留 multiplicity。
2. 只对未匹配 upright 字符，在实验窗口内要求 baseline 与纵向 box 相容，先按相同 Unicode
   或已确认 hyphen 等价做确定性一对一配对。
3. Unicode 不同的候选只有在 walk/engine 双向唯一，且 36 pt 内存在同一左右方向的已匹配
   序列锚点时才配对。
4. 未匹配连续片段若至少 2 个可见字符、至少 75% Unicode 顺序一致，则只记录为
   sequence-only correspondence；它们仍属于 F，不计几何匹配。

`0.5 pt` 是语料探索窗口，不是生产常量。初始半径扫描在 `0.5–1.0 pt` 出现匹配数量平台；
到 `2.0 pt` 歧义明显增加，`5.0 pt` 接近全页歧义。最终 E 文档单独复跑
`0.5 / 1.0 / 2.0 pt`，三档 E 都为 302；更大半径没有解释它们。

### 3.2 A-F 口径

| 类别 | 本实验口径 |
|---|---|
| A | whitespace、不可见 walk 字符、或 tight box 与可见页面无交集的 engine-only 字符 |
| B | 几何匹配后，两引擎数组相对顺序不同；只计数，不作为残差 |
| C | 已配对位置的 Unicode 不同；再按 PDFium 提取标记和 walk provenance 拆分 |
| D | 有可靠 upright 几何和 Unicode，但窗口内没有任何 engine 候选的 walk-only 字符 |
| E | 页面内有可靠几何和 Unicode，但没有 walk 候选的 engine-only 字符 |
| F | 无 Unicode、无可靠几何、候选不唯一，或只能由序列而不能由几何对应的残差 |

## 4. 结果

### 4.1 `Checking list.pdf`

| walk | PDFium | 精确匹配 | A | C | D | E | F |
|---:|---:|---:|---:|---:|---:|---:|---:|
| 3,081 | 3,014 | 3,014 | 67 | 0 | 0 | 0 | 0 |

67 个 A 全是 walk-only 空格。最大 baseline 差为 x `0.000100762 pt`、y
`0.000029297 pt`。

### 4.2 18 份论文聚合

| 量 | 结果 |
|---|---:|
| 精确匹配 | 1,080,409 |
| 同码二阶段匹配 | 86,437 |
| 双向唯一异码匹配 | 142 |
| 几何匹配合计 | 1,166,988 |
| sequence-only correspondence | 1,807 |
| A | 24,210 |
| A 中 engine 离页字符 | 125 |
| C | 9,235 |
| D | 1,103 |
| E | 302 |
| F | 10,679 |
| 有 E 的页 / 文档 | 3 / 1 |
| 独立 walk 错误页 | 1 |

C 的互斥分解：

| C 子类 | 数量 | 解释 |
|---|---:|---|
| PDFium hyphen | 1,203 | walk 为 `-`，PDFium `GetUnicode` 为 `U+0002`，`IsHyphen=true` |
| PDFium UTF-16 surrogate | 988 | walk 为非 BMP 字符，PDFium 单字符 API 只给高代理项 `0xD835` |
| PDFium ligature expansion | 28 | walk 为 `ﬁ` 等单字符，PDFium 为首字符，余字符标为 generated 后被适配器排除 |
| strong other | 4 | walk `/ToUnicode` 为 `U+FFFF`，PDFium 为 `U+0BDC/U+0BDD` |
| weak other | 413 | simple encoding / symbol font 路径上的不同 Unicode |
| unresolved | 6,599 | walk `unicode=None`，现有 ADR-0014 路径本就保留相关段落 |

### 4.3 文档分布

| PDF（文件名前缀） | 页 | A | C | D | E | F |
|---|---:|---:|---:|---:|---:|---:|
| `highcite_2018_1806.01512` | 26 | 43 | 379 | 0 | 0 | 1,061 |
| `highcite_2019_1905.06004` | 7 | 10 | 543 | 0 | 0 | 168 |
| `highcite_2022_2208.06051` | 13 | 10 | 975 | 0 | 0 | 194 |
| `highcite_2022_2211.09551` | 8 | 463 | 3 | 0 | 0 | 194 |
| `highcite_2023_2206.14153` | 64 | 3,518 | 509 | 3 | 0 | 457 |
| `highcite_2023_2307.01429` | 28 | 10,631 | 661 | 0 | 0 | 316 |
| `new_2024_2408.13269` | 4 | 36 | 38 | 0 | 0 | 460 |
| `new_2025_2510.15547` | 11 | 352 | 127 | 23 | 0 | 545 |
| `new_2025_2510.16033` | 18 | 159 | 1,297 | 44 | 302 | 2,050 |
| `new_2025_2511.01258` | 28 | 10 | 127 | 0 | 0 | 360 |
| `new_2025_2511.15174` | 8 | 36 | 164 | 23 | 0 | 399 |
| `new_2026_2606.16684` | 24 | 618 | 27 | 0 | 0 | 64 |
| `new_2026_2606.21991` | 35 | 1,207 | 613 | 0 | 0 | 1,026 |
| `new_2026_2606.24459` | 6 | 453 | 16 | 0 | 0 | 34 |
| `new_2026_2606.24954` | 16 | 430 | 25 | 0 | 0 | 32 |
| `new_2026_2607.01992` | 7 | 159 | 10 | 0 | 0 | 0 |
| `topcite_2020_Lei` | 136 | 5,167 | 1,016 | 1,010 | 0 | 2,946 |
| `topcite_2020_Zhang` | 23 | 908 | 2,705 | 0 | 0 | 373 |

### 4.4 E 与错误页

302 个 E 全部位于 `new_2025_2510.16033...ISGFAN.pdf` 的 page index 11、13、14，分别
196、95、11 个。内容是混淆矩阵/图表标签，例如 `B1 B2 B3 I1 I2 I3 NO1 NO2 NO3`
和 `Accuracy(%)`。同页 walk 有大量 `unicode=None` 或字符串内多字符共享同一 baseline 的
残差，因此当前无法建立逐字符来源对应；半径扩大到 `2.0 pt` 不改变 E 数量。

`new_2025_2510.15547...pdf` page index 3 曾有 89 个表面 E；其坐标位于
`612 x 792 pt` 页面之外（约 x `978–1132`、y `989–1215`），加入页面相交分类后归 A。

`topcite_2020_Zhang...pdf` page index 11 无法独立 walk：Form XObject `/Fm0` object
1032 没有可用 BBox。该页没有进入 A-F 汇总；这与跨引擎匹配无关，是单独的 walk
降级证据。

## 5. 证据边界

1. 本实验匹配器是诊断装置，不是生产实现。它使用确定性贪心一对一匹配和有界序列启发式，
   遇到歧义主动留 F；不能把其 F 数量直接转成生产页级降级阈值。
2. `0.5 pt` 只是当前语料的探索上界。生产容差必须由 fixture 先验和更细的行/字体/对象来源
   合同确定，不能从这份结果反推。
3. 剩余 E 与 walk `unicode=None` 的字形尚无 char → text object → font/source 对应；在建立
   来源桥前，不能断言它们是 walk 完全没看到字形，还是只缺 Unicode/逐字几何。
4. D 主要是 PDFium 对非 BMP 数学字符的提取缺失；它说明 PDFium 少字符不能等同于 PDF
   无字符，但不证明所有 D 都可忽略。
5. 本实验没有执行翻译或 rewrite，因此只回答“旧硬门判据是否成立”和“残差如何分布”，
   不回答新降级矩阵的端到端视觉质量。

复现命令与字段说明见实验 [`README.md`](../experiments/experiment-5-engine-alignment/README.md)。
