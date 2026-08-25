# ADR-0014 · 字体与 CMap 的可靠性判定链

- 状态：已接受（2026-08-24）
- 决策层级：可逆——判定链的分支顺序与 M1 支持面随 PDFium F1 能力绑定和 M3 case 复议；但「不可信即保留原文、绝不猜测」这一取向属既定哲学（`docs/03-corpus-requirements.md` CMAP-04），不在复议范围

## 背景

#19 要求「字体或 Unicode 信息不足时不猜出看似合理的乱码，而是以段落为单位保留可读原文」。截至 `2c818a5`，生产侧几乎没有字体解码：

- `walk/mod.rs` 只支持带显式 `/Widths` 与 `/FontDescriptor` 的 Type1/TrueType 简单字体，Standard-14 隐式度量、Type0、Type3 全部 `unsupported_pdf`。
- 字符解码是硬编码的 WinAnsi 近似（`decode_win_ansi`）。全 crate 搜索 `cmap`、`ToUnicode`、`Identity-H`、`codespace` **零命中**。
- 没有段级降级机制——`translate` 无条件填充 `translated_text`，`typeset` 的守卫会把 `None` 当作错误硬拒。

同时 M0 实验 2 已经钉死两条与本 ADR 直接冲突的既有事实，必须继承：

1. **FONT-02**：文件 `/Widths` 优先于 Standard-14 内置度量，内置度量只在文件缺项时兜底。
2. **CMAP-04**：Identity-H 且无 `/ToUnicode` 时 PDFium 的 text page 返回空 Unicode，**它不能作为此 case 的 Unicode oracle**；生产实现必须保留 embedded-font cmap 路径。这是「PDFium 是交叉证据而非事实层」（CONTEXT #35）在字体侧最硬的一处体现。

段级降级的载体与报告形状由 [ADR-0013](0013-bounded-walk-and-graded-degradation.md) §2/§5 定义，本 ADR 只定「什么时候不可信」。

## 决策

### 1. 数据来源分派

| 来源 | 取用内容 | 失败语义 |
|---|---|---|
| 字体字典 | `Subtype`、`BaseFont`、`FirstChar`/`Widths`、`Encoding`（名字或含 `BaseEncoding`+`Differences` 的字典）、`ToUnicode`、`DescendantFonts` | 类型不符（`ToUnicode` 是字典而非流、`Widths` 是数字而非数组）→ 显式诊断 + 段级降级，**不取默认值**（PARSE-07） |
| FontDescriptor | `Ascent`/`Descent`/`MissingWidth`/`Flags` | 简单字体缺 descriptor 且非 Standard-14 → 段级降级 |
| Type0 `/Encoding` | 预定义 CMap 名或嵌入 CMap 流（完整 `codespacerange` + `cidrange`，按字节解析、不做 UTF-8 解码） | 缺 `/Encoding` 是非法（CMAP-02）→ 段级降级；不支持的预定义名 → 段级降级（见 §2） |
| DescendantFont | `Subtype`、`W`/`DW`、`CIDToGIDMap` | 缺 `Subtype` → 段级降级，**不做推断**（FONT-07：静默退化成简单字体会让字符数翻倍且全错，是最隐蔽的数据损坏） |
| ToUnicode 流 | `bfchar` / `bfrange` | `bfrange` 数组长度不匹配 → 逐条诊断；未映射的 CID → 段级降级，**绝不输出 `(cid:N)`** |
| 嵌入字体程序 | `FontFile2` 的 cmap 表，用于 CID→GID→Unicode 反查 | 截断或不可解析 → 段级降级（FONT-05） |
| Type3 | `FontMatrix`、CharProcs 的 `d0`/`d1` 度量、自带 `Resources`（缺失继承页面） | `FontMatrix` 缺失或退化 → 段级降级，不除零（FONT-06） |

嵌入 TrueType 的 cmap 解析用 `ttf-parser`（只读、无 unsafe 面、已在 workspace 依赖图中）。Type3 的 CharProc 走 ADR-0013 §3 的隔离作用域执行，含递归保护。

### 2. M1 的预定义 CMap 支持面：Identity 及其别名

M1 只内置 `Identity-H`/`Identity-V` 与一份钉死的已知别名清单（如 `DLIdent-H`/`DLIdent-V`）。其余预定义 CMap（`GBK-EUC-H`、`UniGB-UCS2-H` 等）→ 显式 `UnsupportedPredefined` 诊断 + 段级降级。

依据：CMAP-01 的验收合同是「不支持则显式降级报告，**绝不静默产出 0 字符**」，不是「必须支持」。内置 Adobe 字符集映射表会引入大体积数据资产与新的确定性面（资产分发、SHA-256 门禁），M1 没有对应验收要求。CMAP-01 的 fixture 按「显式降级」申报预期。

### 3. Unicode 可靠性判定链

每个字体一次判定，逐字符产出 `unicode: Option<char>`：

```
1. ToUnicode 存在且有效     → 用它；该字符未被映射，或映射目标含 Unicode
                              noncharacter → unicode = None，且不回落后续层
2. 否则：嵌入字体 cmap 可反查 → CID → GID → cmap 反查
3. 否则：简单字体标准编码链   → Encoding/Differences → 字形名 → Unicode
                              字形名不可映射（如 CMAP-07 的 gNN）→ 该字符 None，
                              不回落 BaseEncoding
4. 全部失败                  → unicode = None
```

noncharacter 包括 `U+FDD0`–`U+FDEF` 及每个平面末尾的 `U+FFFE/U+FFFF`，共 66 个；该分支由 [ADR-0015 §4](0015-classified-cross-engine-alignment.md#4-adr-0014-修订tounicode-映射到非字符视为未映射) 修订。

判定链中**不存在「把 CID 当 Unicode」的分支**——这是「不出现错误 identity mapping」的第一道防线。第二道防线在验收侧：fixture oracle 断言输出不含 `(cid:` 子串，且降级段的输出字节与输入逐字节相同。

### 4. 段级降级的触发条件

段内满足任一条即整段保留原文：

- 存在 `unicode == None` 的可翻译字符；
- 该段任一字体的字符 advance 不为正或非有限；
- 字体对象本身不可解析（上表任一「段级降级」分支）。

**advance 为正是「可处理」的准入条件**（#19 验收条款 3），不是事后检查。

### 5. 宽度来源

- 简单字体：文件 `/Widths[code − FirstChar]` → 缺项用 descriptor 的 `MissingWidth` → 再缺则该字体不可信。**缺 `/Widths` 整体（FONT-03）在 M1 走段级降级**，不做 PDFium advance 兜底：`pdfium-render` 尚未绑定 `FPDFFont_GetGlyphWidth`（PDFium 资格报告的 F1 项未完成），M1 没有这个数据源。F1 补齐并重跑资格矩阵后可复议为「兜底 + warning」。
- CID 字体：`W` 数组区间查找 → `DW` → 规范缺省 1000。
- Type3：`d0`/`d1` 的前两个操作数 × `FontMatrix`。

### 6. CJK 输入 fixture 的字体

选定 **Noto Sans SC Regular 的确定性子集**（`corpus/fonts/` 下入库，OFL 许可证原文并排提交，README 记录 `pyftsubset` 复现命令并经两次生成 SHA-256 比对，哈希钉入引用它的每份 manifest）。

- 与输出字体（CONTEXT 决策 #18）同族不构成问题：溯源依据已改为**对象号 + subset tag**（CONTEXT「溯源断言」条目），不依赖输入输出字体异族。
- 字形集按 fixture 实际用字裁剪，预计体积远小于完整字体；沿用 `MimusExact.ttf` 已经验证过的入库范式（子集 + 许可证 + 可复现命令 + manifest 钉哈希）。
- 不采用「仓库外托管 + 运行时下载」的分发式方案（BabelDOC 对**输出**字体的做法）：corpus 的确定性合同要求生成过程无网络依赖，而 fixture 的字形集事先已知且极小，入库版本永不失联。

## 后果

- 一批 M1 case 从「生产路径直接拒绝」变为「正确处理或显式降级」：FONT-01/03/05/06/07、CMAP-01/02/05/07/08。既有的 `unit-font-01-std14-custom-widths`、`unit-cmap-01-identity-no-tounicode`、`unit-cmap-02-mixed-codespace` 三份 fixture 在生产 walk 下的预期从「`unsupported_pdf` 拒绝」翻转为对应的处理或降级行为，对应测试须同步迁移。
- Standard-14 的隐式内置度量在 M1 仍不支持（FONT-02 的政策是文件 `/Widths` 优先，内置度量兜底属 M3 范围）。
- 判定链的每一层都可能产出 `unicode = None`，而 `unicode` 在 IL 中本就是 `Option<char>`——IL 结构不变。
- PDFium 在字体侧的角色被进一步收窄为交叉证据：CMAP-04 明确禁止它当 Unicode oracle，走查与 PDFium 在这类页上的分歧按 ADR-0013 §7 的分级处理。
- 若 PDFium 的 F1 能力（字体 owned snapshot、glyph width、embedded 状态）在上游补齐，§5 的 FONT-03 兜底与 §2 的支持面都可复议，届时更新本 ADR。
