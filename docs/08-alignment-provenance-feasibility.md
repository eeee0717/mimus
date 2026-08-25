# T44 前置调研：字符来源桥与 stage-2 配对的可行性

> 日期：2026-08-25
> 问题：ADR-0015 要求的 PDFium 字符来源桥能否建立；若不能，E/F 类的保护判据如何落地？
> **结论：`character -> text object -> font` 现在可建，但 PDFium 公共 API 不提供源流或
> 字节区间。#70 的验收条款必须改写；不建议整票等待上游 O1，而应改走 walk-owned 来源组 +
> engine owned 对象快照的保守相关路径。**

## 0. 结论先行

`docs/05-pdfium-backend-qualification.md` 中 T1/O1 的“未绑定”结论审计的是
`firecrawl-pdfium@1a4c91d...`，不是 mimus 当前使用的 `pdfium-render 0.9.1`。当前 wrapper
已经安全暴露：字符到 text object、text object 到 font、页面对象遍历、Form 递归，以及反查
某个 text object 的全部 text-page 字符。这一半不需要新增 raw binding。

完整的 `char -> text object -> font/byte range` 仍然建不成。PDFium 7763 的公共 API 不返回：

- text-page 字符的原始 charcode/CID/GID；
- text object 内的源字符序号；
- text object 所属的间接对象号、XObject 资源名或 content stream 身份；
- `Tj`/`TJ` 源操作数的字节区间。

这些数据在 PDFium 内部并非全部不存在，例如内部 `CharInfo` 保存 `char_code_`，但没有公共
getter。只给 wrapper 增加现有 C symbol 的绑定不能补出这条边。因此 PDFium 侧可提供对象分组、
字体和诊断交叉证据；字节事实必须继续由 walk 独占，二者只能通过有歧义即放弃的相关算法连接，
不能称为 PDFium 提供的直接来源桥。

对实验 5 原始 JSON 的补充复核还改变了 302 个 E 的证据状态：**302/302 都能在 `0.001 pt`
内唯一对应到一个 walk 字符，源字符码与 PDFium 数值相同；这些 walk 字符全部是
`unicode=None`、`Rotated(90.0)`。** 实验 runner 的几何候选入口只接受 `Upright` walk 字符，
所以同一个绘制事件在两侧分别落成 F 和 E。涉及的 12 个字体对象全部是无 `/ToUnicode` 的
Type3 字体。它们不是已观测到的 walk 漏字。

另一个必须在 #70 前处理的事实是：`unit-align-08-engine-only-overlap` 与
`unit-align-09-engine-only-disjoint` 用 `/ActualText (MI)` 包住一个实际只绘制 `M` 的字形来制造
engine-only 字符。额外的 `I` 是提取文本，不是额外墨迹；PDFium 给它几何也不能证明页面多画了
一个字形。相交 fixture 若直接翻转为段级保留，会把已知提取视图差异升级为误保留。

因此本报告建议：

1. 改写 #70，删除“PDFium 字符直接对应源字节区间”的不可满足断言；保留 walk 字节区间为唯一
   事实层。
2. 不整票等待 firecrawl O1。先用当前 backend 可得的对象/字体/marked-content 快照，配合
   walk 的 source-run 分组做保守相关；后端替换仍须提供同一 owned 合同。
3. stage-2 不采用一个全局 point 半径。钉死由 fixture 推导的**逐 run 动态误差包络**和跨行拒绝
   条件；实验 5 的 `0.5 pt` 不进入生产参数。
4. 先把旋转、未解析、`/ActualText` 展开从 E 中解释掉，再讨论 E/F 保留。现有 ALIGN-08/09
   不能作为“engine-only 墨迹”翻转依据；须有一份独立渲染器证明真实额外墨迹、且 walk 确实
   没有对应绘制事件的 fixture 后，才启用 E 相交保留。

## 1. 范围与不变量

本调研只读源码、API、现有 fixture、实验 5 JSON 和一份本机真实论文。没有修改生产代码、
ADR、issue #70 或实验 runner；真实 PDF 与逐页 JSON 均留在 `.context/` 或原下载目录。

以下不变量贯穿所有结论：

- PDFium 是交叉证据，不是 Unicode、绘制序或源字节事实层（CONTEXT #35）。
- `Char.unicode` 仍只来自 walk 的 ADR-0014 解码链；任何 engine 字段都不得注入 IL。
- 源字节区间只来自 walk tokenizer；未替换字节逐字节透传合同不变。
- 后端数据只能通过 mimus 自有的 owned snapshot 穿过 `PdfInspector` 边界；pass 不得引用
  `pdfium-render` 类型（ADR-0010、CONTEXT #38）。
- 多重集 multiplicity 保留；任何对象、序列或几何配对有歧义时留残差，不猜测。

本报告区分两个容易混淆的目标：

```text
PDFium 内部归组                         跨引擎源对应
char index -> text object -> font       engine object/char ~ walk source run -> byte range
                 可建                                      只能保守相关
```

## 2. 环境与固定版本

| 项 | 值 |
|---|---|
| 主机 | macOS 26.5.1 (25F80)，arm64 |
| Rust | rustc 1.97.1 |
| 调研起始 HEAD | `fca95bda796f443605936f5f56006104982ff496`（PR #72 顶层） |
| 对照 `origin/master` | `85fae3762b02b9707161e6b5518f6b54cbc4601c` |
| Rust wrapper | `pdfium-render = 0.9.1`，features `pdfium_7763, thread_safe` |
| crates.io checksum | `076dd8f3a6c7da9298ddffbcc0d5a109f89caf967fa4871c9a172d5b3498b35b` |
| wrapper API 下限 | PDFium chromium/7763，commit `ca8a943c247c208fd7a9cd21b4de049f22b93070` |
| 本地测试 dylib | chromium/8009，SHA-256 `cfab7b27942132aea1a1ff7ff42ce970c39f7d928c1fc317ea99d3bfa3a43d0c` |
| 独立检查工具 | qpdf 12.4.0；jq 1.7.1 |

`pdfium-render 0.9.1` 打包的 `fpdf_text.h`、`fpdf_edit.h`、`fpdf_searchex.h` 与固定的
PDFium 7763 commit 逐字节一致。本报告以 7763 公共头文件为最低合同；8009 dylib 虽更新，Rust
侧仍编译在 7763 API 表面上。

302 个 E 的补充复核使用实验 5 已有文件：

| 输入 | SHA-256 |
|---|---|
| `.context/engine-alignment/e-pages-v4.json` | `fa84732a844f242d3a91acccef4ba8444dad98a498fd4dc3eb67560d513d8fcb` |
| `new_2025_2510.16033...ISGFAN.pdf` | `2c3663256de8c0c48831a6d8982fab0453ee7c14dc0f56e986911e3c630939d8` |

## 3. A：T1/O1 的实际现状

### 3.1 三档能力矩阵

“现在可取”指当前 `pdfium-render` safe API 已有能力，但 mimus 的 `PageCharSnapshot` 尚未包含
全部字段。“需 wrapper”指 PDFium 7763 与 wrapper raw trait 已有 symbol，但现有 safe wrapper
没有方法，且下游拿不到私有 handle。“上游缺失”指固定公共头文件没有该能力。

| 数据或关联 | 分档 | 当前接口与边界 |
|---|---|---|
| text-page 字符索引 | 现在可取 | `PdfPageTextChar::index()`；只是 PDFium character-list index |
| char -> text object | 现在可取 | `PdfPageTextChar::text_object()`，包装 `FPDFText_GetTextObject` |
| text object -> chars | 现在可取 | `PdfPageText::chars_for_object()`；wrapper 内按 object handle identity 反查 |
| page object 遍历 | 现在可取 | `PdfPage::objects()` 的 `len/get/iter` |
| object 类型与 text/Form 下转 | 现在可取 | `object_type()`、`as_text_object()`、`as_x_object_form_object()` |
| Form 子对象递归 | 现在可取 | `PdfPageXObjectFormObject::{len,get,iter}`；调用方自行保留父级 ordinal path |
| text object -> font | 现在可取 | `PdfPageTextObject::font()`；font 为 borrowed handle 的 safe wrapper |
| font 信息 | 现在可取 | name/family、ascent/descent、embedded 状态、font bytes、glyph width/path |
| 字符 T1 诊断 | 现在可取 | `is_generated()`、`is_hyphen()`；`font_name`/size/matrix/angle/color 也已有 safe API |
| Unicode map error | 需 wrapper | raw `FPDFText_HasUnicodeMapError` 已绑定；缺 `PdfPageTextChar` safe 方法 |
| char index <-> extracted-text index | 需 wrapper | 两个 `FPDFText_Get*Index*` 已绑定；缺 safe 方法，且 text-page handle 私有 |
| content mark、MCID、`/ActualText` 参数 | 需 wrapper | `FPDFPageObj_CountMarks/GetMark` 与 mark 参数 API 已绑定；无 high-level wrapper |
| text-page char -> 原始 charcode/CID/GID | 上游缺失 | `GetUnicode` 只给提取 Unicode；内部 `CharInfo::char_code_` 没有 public getter |
| text object 内源字符 ordinal | 上游缺失 | object 全文与所属 text-page indices 可取，但不能反查原始 show 内位置 |
| 间接对象号、资源名、源 content stream | 上游缺失 | 遍历 ordinal 是 PDFium 对象树位置，不是 PDF 对象身份 |
| content stream / show operand 字节区间 | 上游缺失 | 公共 API 没有 parser offset 或 raw show bytes |

主要源码证据：

- [`PdfPageTextChar::text_object()`](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/document/page/text/char.rs#346-375)
  与固定上游
  [`FPDFText_GetTextObject`](https://pdfium.googlesource.com/pdfium/+/ca8a943c247c208fd7a9cd21b4de049f22b93070/public/fpdf_text.h#77)；
- [`PdfPageText::chars_for_object()`](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/document/page/text.rs#111-147)；
- [`PdfPageTextObject::font()`](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/document/page/object/text.rs#289-296)
  与上游
  [`FPDFTextObj_GetFont`](https://pdfium.googlesource.com/pdfium/+/ca8a943c247c208fd7a9cd21b4de049f22b93070/public/fpdf_edit.h#1472)；
- [页面对象枚举](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/document/page/objects/common.rs#34-89)
  与 [Form 递归](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/document/page/object/x_object_form.rs#54-113)；
- raw
  [`FPDFText_HasUnicodeMapError`](https://docs.rs/crate/pdfium-render/0.9.1/source/src/bindings.rs#6532-6542)
  和 [字符/文本索引换算](https://docs.rs/crate/pdfium-render/0.9.1/source/src/bindings.rs#1257-1284)；
- 上游内部
  [`CPDF_TextPage::CharInfo`](https://pdfium.googlesource.com/pdfium/+/ca8a943c247c208fd7a9cd21b4de049f22b93070/core/fpdftext/cpdf_textpage.h#45)
  保存 charcode，但公共接口没有 getter。

`FPDFFont_GetGlyphWidth/Path` 不能补原始 glyph 映射。其参数虽命名为 `glyph`，7763 实现先调用
`CharCodeFromUnicode()`，即它需要调用者给一个 Unicode-like 值；它不是从某个 text-page 字符
取回源 charcode/GID 的反向 API。

这也修正了 ADR-0014 §5 的一个前提表述：当前 `pdfium-render 0.9.1` 实际已通过
[`PdfFontGlyph::width_at_font_size()`](https://docs.rs/crate/pdfium-render/0.9.1/source/src/pdf/font/glyph.rs#32-48)
安全调用 `FPDFFont_GetGlyphWidth`；“未绑定”只适用于 firecrawl 候选。由于 text-page 字符到
原始 charcode/glyph 的桥仍不存在，该 API 依然不能作为 walk 缺 `/Widths` 时的逐字符 advance
兜底。ADR-0014 选择“不可信即段级保留”的行为结论仍成立，但理由应在维护者另行决策后更新；
本调研不修改 ADR。

### 3.2 与 firecrawl 资格结论的关系

`docs/05` §5 的“crate 暴露”列明确针对 `firecrawl_pdfium::sys::Bindings`。因此：

- 当前 `pdfium-render` backend 可以立刻原型化对象/字体快照；
- firecrawl backend 仍缺同等的 safe/raw 表面，替换资格结论不变；
- 即便 firecrawl 把报告中的 O1 symbol 全部绑定，也只能达到“对象/字体归组”，仍不会凭空得到
  上游没有的源字节区间。

## 4. B：来源桥能否建成

### 4.1 可建的部分

在一次页面 inspection 的借用生命周期内，可以：

1. 遍历 page objects 并递归 Form，以父级局部 ordinal 形成 PDFium 对象树路径；
2. 对每个 text object 调 `chars_for_object()`，取得其 text-page character indices；
3. 抽取 text object 的 matrix、bounds、font size/render mode，以及 font 名称、嵌入状态和数据指纹；
4. 为每个字符附上所属 text object 的 owned key 和 T1 诊断。

这足以把“整页散列字符”变成“按 PDFium text object 分组的字符序列”，对排除 `/ActualText`
展开、generated 字符、Form 复用和字体宽度漂移都有价值。

### 4.2 卡住的部分

walk 字符已有 `(content_object, byte_start, byte_end, encoded, font object)`。PDFium 组没有可与
`content_object` 或 byte interval 等值比较的源身份。其 traversal path 是 PDFium 解析后的对象树
ordinal；PDFium 不承诺它等于 lopdf 间接对象号、资源名或 content stream 顺序。

因此双侧**直接**对应卡在：

```text
PDFium text object
    X  没有公开的源 content object / XObject resource / show offset
walk (content_object, show byte range)
```

剩余可行方案只能用对象级字符序列、字体指纹、变换、几何、multiplicity 与邻近锚点建立复合
相关。该相关可以很强，但必须保留“多候选即不配对”，且不得把其结果描述为 PDFium 证明了
源字节区间。

### 4.3 owned snapshot 形状草案

以下只说明 ADR-0010 边界，不是实现提案：

```rust
struct PageTextProvenanceSnapshot {
    characters: Vec<PageCharSnapshot>,
    text_objects: Vec<TextObjectSnapshot>,
}

struct TextObjectSnapshot {
    key: TextObjectKey,                 // 本次 snapshot 内的 owned key
    engine_object_path: Vec<u32>,       // PDFium 父级局部 ordinal；明确不是 PDF object id
    character_indices: Vec<u32>,        // text-page character index，多重性保留
    matrix: [f64; 6],
    bounds: Rect,
    font_size: f64,
    font: FontSnapshot,
    marked_content: Vec<ContentMarkSnapshot>,
}

struct FontSnapshot {
    base_name: String,
    family_name: String,
    is_embedded: Option<bool>,
    embedded_data_fingerprint: Option<[u8; 32]>,
}

struct ContentMarkSnapshot {
    tag: String,
    mcid: Option<i32>,
    has_actual_text: bool,              // 不把 ActualText 值注入 IL
}
```

`PageCharSnapshot` 可只增 `text_object_key: Option<TextObjectKey>`、
`has_unicode_map_error: Option<bool>`。所有 wrapper handle 在 adapter 返回前销毁；pass 只消费上述
owned 类型。`embedded_data_fingerprint` 仅用于同字体相关，未嵌入字体不得把 PDFium substitute
数据误标为源字体。

## 5. C：不依赖直接来源桥的替代路径

### 5.1 路径比较

| 路径 | 能回答什么 | 可靠性 | 代价与边界 |
|---|---|---|---|
| 纯 walk 解释 | 识别已有绘制事件的 `unicode=None`、非直立、共享 show span、字体失败、`/ActualText` 标记 | 对“这不是 walk 完全漏画”高；对“walk 是否漏了未知事件”不充分 | 需把 marked-content stack/source-run 身份保留到 walk 结果；不能仅凭 walk 证明不存在遗漏 |
| 几何 + 宽度模型 | 区分同一 run 的累积水平漂移与中间缺口 | 对已可靠归组、单调且候选唯一的序列高 | 需动态误差包络、writing direction、run 锚点；Type3、复用 Form、double-draw 歧义时必须退出 |
| 收窄 E 触发 | 仅把无法由任何 walk residual/source run 解释、且与替换单元墨迹相交的 engine 字符升级为保留 | 内容安全高；误判只减少翻译覆盖，不制造坏译文 | 必须先排除 `/ActualText`/generated/不可见/非直立对应；PDFium 只作为 veto rewrite 的交叉证据 |
| 永久只诊断 | 不增加误保留 | 结构上简单 | 真 E 进入翻译后可能漏词，且原字形留在译文上造成残留原文或视觉叠印 |

### 5.2 纯 walk 路径的能力边界

walk 已记录每字符的 encoded code、font object、transform、content object 与 show operand byte
span。它足以把下列“表面 E”降回已有绘制事件：

- 同位置有 `unicode=None` 的 walk 字符；
- 同一源 show 内多个字符共享起点或 advance 不可靠；
- 非直立 walk 字符被 upright-only 诊断匹配器排除；
- Type3/simple encoding 没有可靠 Unicode，但 code 与几何仍存在；
- BDC/EMC 内 `/ActualText` 把一个可见 glyph 展开为多个提取字符。

当前 walker 把 `BMC/BDC/EMC` 识别为 known operator，但不保存 marked-content stack。若采用这条
路径，需要新增 source-run/mark 快照；它只用于解释交叉残差，是否把 `/ActualText` 用作翻译文本
是另一项域决策，本票不得顺带决定。

纯 walk 无法证明一个它没有枚举的绘制事件不存在。因此它可以消掉当前 302 个假 E，却不能单独
完成真正 E 的负向证明。

### 5.3 几何与宽度模型

stage-2 应在 text object/source run 内沿 writing direction 做单调、多重的一对一配对，而不是在
整页做固定半径最近邻。推荐的证据顺序是：

1. exact baseline 多重集先锁定锚点；
2. 用 engine text-object 分组与 walk source-run 分组限制候选；
3. 垂直于 writing direction 的差只允许落在算术误差包络内；
4. 平行方向按 advance 累积区间和序列顺序配对，允许已知宽度模型造成的渐进漂移；
5. 同码、font fingerprint、对象 bounds 与相邻锚点只增加相关强度，任一字段都不单独充当事实层；
6. 多候选、跨行、multiplicity 不等或 source group 无法对应时留残差。

这条路径使用 PDFium 的对象分组与几何作为交叉证据，不采信它的 Unicode 来改写源文。配对成功
只意味着“此 engine 观测可由一个 walk 绘制事件解释”，最终 byte range 仍是 walk 的。

### 5.4 收窄保留判据

若维护者希望在没有直接字节桥的条件下启用 E 保护，最窄的可辩护触发条件是同时满足：

- engine 字符可见、非空白、非 generated/C0/off-page，几何有限；
- 不属于带 `/ActualText` 的 text object，也没有 Unicode-map-error；
- source-run stage-2 后不存在 exact、动态包络或非直立/未解析 walk 候选；
- engine `tight_box` 与一个且仅一个将被替换段落的 walk 墨迹并集相交；
- 该段落内不存在会让归属歧义的 walk residual 或 double-draw 候选。

命中只触发“保留原段落”，不采用 engine Unicode、字体或 bytes。这里采信 PDFium 的理由是：它
不是在裁定文本事实，而是在提供“rewrite 区域可能还有未解释墨迹”的独立反证；响应是撤销
rewrite、保留原字节。假阳性损失翻译覆盖，假阴性才可能产出坏页面，风险方向与 CONTEXT #35
相容。

不过 `/ActualText` fixture 已证明 tight box 相交本身不够：提取字符可以继承底层单个 glyph 的
几何而没有自己的墨迹。

### 5.5 维持诊断-only 的风险

现有 18 份、约 119 万 walk 字符的语料中，补充复核后没有一例已证实的“PDFium 看见真实墨迹、
walk 完全没有绘制事件”；观测发生率为 0，但样本不足以给总体上界。ADR-0014 已保留含
`unicode=None` 或不可靠 advance 的段落，非直立文本也不会进入普通正文翻译，这覆盖了当前
302 个事件。

真实翻译接通后的剩余风险仍是高严重度：若某个可翻译段落内确有 walk 完全漏掉的 show/glyph，
翻译请求会缺词；splice 只会处理已知区间，漏掉的原 glyph 可能继续留在页面上，与重新排版的
译文发生残留原文或视觉叠印。未替换字节透传只保证 PDF 结构安全，不保证这类视觉/语义正确。

因此 diagnostic-only 可以作为短期过渡，不能被描述为 E 已经安全关闭；但也没有证据支持用当前
302 或 ALIGN-08/09 立即扩大段级保留面。

## 6. D：stage-2 窗口数值的先验来源

### 6.1 应推导的是动态误差包络

单个全局 `N pt` 半径没有规范依据。不同 run 的误差随坐标量级、矩阵、writing direction、字符
数和宽度来源变化。fixture 应钉死以下参数和公式，而不是从真实语料拟合一个半径：

```text
epsilon_perp = transform_rounding_bound(origin, CTM, text_matrix)

epsilon_parallel(i) = epsilon_perp
                      + sum(j < i, width_source_error(j) * font_size * |Tz|)
                      + float_accumulation_bound(i)
```

- `transform_rounding_bound`：PDF 数字经 f64 walk 与 PDFium f32 路径的可证明舍入上界，按坐标和
  线性变换幅度缩放；
- `width_source_error`：按 ADR-0014 §5 的来源分别裁定。显式 `/Widths`、CID `W/DW`、Type3
  `d0/d1 × FontMatrix` 各自有 fixture；缺项或不可信来源不是扩大窗口，而是既有段级保留；
- `float_accumulation_bound`：随前序 advance、`Tc/Tw/Tz`、`TJ` 数字和运算次数增长；
- 相邻 baseline 距离和字体盒高度只做**跨行拒绝条件**，不是水平漂移先验。包络若接近下一行或
  同 run 出现多个候选，立即留残差。

如果实测差异超过上述由同一 PDF 数值推导的算术上界，应先判为宽度来源合同不同，而不是继续
放大窗口。此时可借 engine 相邻 baseline 形成的 advance 序列做对象内单调相关，但仍须 fixture
钉死序列规则和歧义退出条件。

### 6.2 现有 fixture 能提供什么

| 现有 fixture | 可直接取得的先验 | 仍缺什么 |
|---|---|---|
| `unit-font-01-std14-custom-widths` | 文件 `/Widths` 优先、单字符/短 run 的绝对 baseline | 长 run 累积与 `Tc/Tw/Tz/TJ` 组合 |
| `unit-stream-02-type3-d1` | Type3 `d1 × FontMatrix` 的 advance | 多字形 Type3 长 run 与旋转 writing direction |
| `unit-cmap-01/02-*` | CID segmentation、`W/DW` 路径的基础输入 | 同一 object 内逐字符累积误差 |
| `unit-xobj-04-inherited-resources` | Form 资源与 CTM 归属 | 嵌套/复用 Form 的对象组相关和误差传播 |
| `unit-xobj-05-singular-ctm` | 奇异矩阵退出 | 非奇异深层矩阵的有限误差上界 |
| `unit-align-06-double-draw` | 同位置 multiplicity | 漂移窗口内 double-draw 与普通序列的歧义退出 |
| `unit-align-08/09-*` | `/ActualText` 提取展开会制造额外 engine 字符 | 不能提供真实 engine-only 墨迹先验 |

现有 fixture 多数只钉一个位置或一个分支，不能直接得出 production stage-2 上界。

### 6.3 需要新增的 fixture

按生成合同逐个保持单变量，至少需要：

1. simple `/Widths` 的长 run 长度阶梯（1/32/256），使用可产生 f32 舍入的分数宽度；
2. 在相同 run 上分别只改变 `Tc`、`Tw`、`Tz`、`TJ` adjustment；
3. CID `W` 显式区间与 `DW` 缺省的长 run；
4. Type3 `d0`/`d1` 与非单位 `FontMatrix` 的长 run；
5. upright、90°、斜切 writing direction 各一份，验证平行/垂直投影而非 x/y 特判；
6. 嵌套与重复引用同一 Form 的 run，验证 instance multiplicity 与对象组退出；
7. window 内 double-draw/极小字号多候选，预期必须留 F；
8. 不含 `/ActualText`、独立 renderer 确认存在额外墨迹、且 walk 诊断装置故意遗漏该事件的真正 E
   资格 fixture。若无法合法、单变量地构造，就说明 E 保留尚无可验收事实对象。

manifest 先由 PDF 运算和字体度量手写出每个字符的理论 origin 与误差上界，再运行 PDFium；不得
回读 PDFium 结果生成 expected。生产常量取覆盖这些规范分支的上界或公式安全系数，不读取实验 5
的 `0.5 pt`。

## 7. E：302 个真实 E 的成因与验证

### 7.1 已证伪“walk 完全漏字”假设

对 `e-pages-v4.json` 三页的每个 E，在同页未配对 walk residual 中搜索 baseline x/y 各差
`<= 0.001 pt` 的候选，并要求 multiplicity 唯一。结果：

| page index | 实验 E | 唯一 walk 几何候选 | 同 code | walk transform | max dx / dy (pt) |
|---:|---:|---:|---:|---|---:|
| 11 | 196 | 196 | 196 | `Rotated(90.0)` | `0.00001747 / 0.00005655` |
| 13 | 95 | 95 | 95 | `Rotated(90.0)` | `0.00001096 / 0.00005216` |
| 14 | 11 | 11 | 11 | `Rotated(90.0)` | `0.00002134 / 0.00003248` |

302 个候选的 `unicode_provenance` 全是 `unresolved`。它们来自 12 个 font object、12 个 content
object、122 个 show operand span；qpdf 逐个检查 12 个 font object，全部为无 `/ToUnicode` 的
Type3 字体。比如图表标签 `B1` 的 walk 字符码仍是 `0x42 0x31`，只是 Type3 Differences 使用
`/uni00000025` 一类不能作为 Unicode 事实的 glyph name。

runner 的 `valid_walk_anchor()` 明确要求 `text_transform == Upright`。因此这些 walk 字符从 exact
candidate graph 入口就被排除，随后被记 F；对应 PDFium 字符没有候选而被记 E。半径从 0.5
扩大到 2.0 pt 当然不会改变数量，因为问题不是半径。

这组证据足以证伪“302 个 E 是 walk 没有看到绘制事件”。它仍不是 PDFium-to-byte 的直接身份
证明：同 code 与唯一几何属于复合相关证据，Unicode 仍不采信 PDFium。但风险响应已经由 walk
侧事实覆盖：字符存在、源 span 存在、Unicode 不可靠；ADR-0014 段级保留或非直立 passthrough
不需要 PDFium 提供文本事实。

### 7.2 对未来 residual 的可证伪假设

| 假设 | 可观察预测 | 验证方法 |
|---|---|---|
| H1：非直立/未解析 walk 字符被匹配前置条件排除 | E 与 F 在 exact baseline、code、multiplicity 上一一对应 | runner 增加“不参与生产配对的 explanation edge”，单独统计，不把非直立字符提升为可翻译 |
| H2：同 object 内宽度累积漂移 | 残差沿 writing direction 单调增长，垂直误差稳定，序列与数量一致 | 输出 source-run/object id、投影坐标、逐步 advance 差；用 fixture 包络复跑 |
| H3：`/ActualText` 或 generated extraction 扩展 | 多个 engine char 同属一个 text object/mark，底层 walk glyph 与独立 raster 只有一个墨迹 | 快照 content marks、object chars；对 ALIGN-08/09 与真实页做 object-level report |
| H4：walk 真正漏掉 show/Form | engine object 有稳定墨迹与字体，但没有任何 walk source-run、recovery 已报告相关资源/解析失败 | 对源 stream 做 tokenizer trace，配合 mutool trace/raster；若 walk 页面已降级，不再归跨引擎 E |
| H5：double-draw/极小字号多候选 | 同一点 multiplicity 或候选数 > 1，任何贪心选择都会改变配对 | 输出完整候选图；fixture 断言留 F，不比较集合去重结果 |

若继续扩展实验 5 runner，新增的逐页 object/source 明细和大体积 JSON 仍写 `.context/`；报告只提交
聚合计数、输入哈希和可复现字段定义。

## 8. 证据边界

1. API 缺失结论针对 PDFium 7763 固定公共头文件。内部 C++ 有 charcode 不等于 wrapper 可以安全
   使用；若未来上游新增 public getter，需按固定版本重审。
2. 当前 safe API 的存在不证明 PDFium traversal ordinal 与 PDF 源对象一一对应。Form 复用、
   malformed object 与 generated text 仍需 fixture。
3. 302 个 E 的补充复核是现有 JSON 上的确定性复合相关，没有改变 runner，也没有建立通用来源桥。
4. `/ActualText` fixture 证明 engine Unicode + tight box 不是“额外墨迹”的充分条件；本报告没有
   构造出一份合法的真实 walk 漏墨迹 fixture。
5. 动态误差包络给出了可操作推导路径，但数值尚未生成；任何具体常量仍须按 corpus 生成合同经
   新 fixture 裁定。
6. 没有执行翻译和 rewrite 视觉验收，因此 M2 风险是结构推演，不是发生率测量。

## 9. 建议

### 9.1 是否改写 #70

**需要改写；不建议整票推迟。** 等待 firecrawl 补 O1 不能解决上游没有源 byte range 的问题。
可行部分应继续：fixture 先验、engine 对象/字体/mark owned snapshot、walk source-run 相关与保守
E veto。真正 E/F 保留的启用条件需从“直接来源桥存在”改成“复合相关已排除所有可解释残差，且
有真实墨迹 fixture 锁定”。

firecrawl backend 的替换仍推迟到它能产出与新 `PdfInspector` owned snapshot 同等的 T1/O1
字段；这不阻止当前 `pdfium-render` backend 完成 T44 的替代路径。

### 9.2 #70 验收条款改写草案

- [ ] stage-2 使用 fixture 推导的逐 source-run 动态误差包络、writing-direction 投影与跨行拒绝
  条件；不引入来自实验 5 `0.5 pt` 的全局半径。
- [ ] `PdfInspector` 新增 backend-neutral owned provenance snapshot：character -> text-object key、
  object-local character-index 集合、对象 traversal path、font identity/embedded fingerprint、T1 与
  marked-content/`ActualText` 诊断；pass 不引用 wrapper 类型。
- [ ] 明确记录能力边界：PDFium 不提供源 charcode/GID、content object 或 byte range；生产 byte
  range 只来自 walk。engine object 与 walk source run 只按对象组、字体、序列、multiplicity、
  transform、几何和锚点保守相关，歧义时留 F。
- [ ] 实验 5 的 302 个 E 全部归入“非直立、Type3、walk unresolved 但源事件存在”的 explained
  residual；三页计数锁为 196/95/11，不再作为 walk 漏字证据。
- [ ] `/ActualText`/generated/off-page/C0/非直立对应先退出 E 墨迹判定；ALIGN-08/09 按生成合同
  重新裁定为 extraction expansion，不把额外提取字符当额外墨迹。
- [ ] 新增真正 engine-only 墨迹 fixture：独立结构与 renderer 证明墨迹存在，walk report 证明无
  source event；若无法构造，E 相交保留继续 diagnostic-only，不以 ALIGN-08/09 替代。
- [ ] 仅对 stage-2 与 source-run explanation 后仍未解释、无候选歧义、且 tight box 与唯一替换
  段落墨迹相交的 E 触发段级保留；不相交仍只诊断。F 只在排除 ADR-0014 已保留项和非直立
  passthrough 后按同一相交条件处理。
- [ ] PDFium Unicode 永不进入 IL；未替换字节透传、`--backend none` 字节恒等与多重集
  multiplicity 合同保持全绿。
- [ ] 若目标 backend 不能产出相同 owned snapshot，记录缺失字段并维持 E/F diagnostic-only；不得
  用全页数组顺序或纯几何最近邻代替来源相关。

### 9.3 M2 前的风险口径

在替代路径落地前，M2 接通翻译的风险是“低观测频率、未定总体概率、高单次影响”：当前语料中
没有已证实真 E，302 个事件均受现有保留/passthrough 覆盖；但若出现真 E，可能造成漏译与残留
原文叠印。建议不因不可获得的直接 byte bridge 阻塞整个 M2，而把上述收窄 E veto 与真正墨迹
fixture 作为启用真实语料翻译前的门；若 fixture 仍无法建立，明确维持 diagnostic-only 并把该
风险记录为未关闭，而不是宣称 #70 已满足。
