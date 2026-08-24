# Corpus v1

合同正文在 [`docs/03-corpus-requirements.md`](../docs/03-corpus-requirements.md) §2。
本目录是它的可执行形式。

```
corpus/
  toolchain.toml     钉死的工具版本 + 每个现实排版引擎的确定性配方（唯一真源）
  determinism/       §2.6 重复生成门禁的探针源文件与共享确定性开关
  fonts/             精确 fixture 使用的钉死字体、许可证与可复现子集配方
  fixtures/          每份 fixture 的先行手写 manifest、PDF 与独立裁定结果
```

门禁入口是 workspace 里的 `corpus` 二进制（`crates/corpus`）。

```sh
cargo run -p corpus -- doctor        # §2.8 工具链齐备且版本精确匹配
cargo run -p corpus -- determinism   # §2.6 每个引擎连续构建两次、SHA-256 必须一致
cargo run -p corpus -- build unit-base-01-single-line unit-base-03-structured
cargo run -p corpus -- verify unit-base-01-single-line unit-base-03-structured
cargo run -p corpus -- trailer-id unit-order-01-natural
```

## 为什么这不是 GitHub Actions 上的一个 job

`toolchain.toml` 里的版本是**精确匹配**（§2.6：版本变化视为语料变更）。
qpdf 12.4.0 / poppler 26.08.0 / mutool 1.28.2 / Typst 0.15.1 / TeX Live 2026 这组
组合无法从 ubuntu-latest 的发行版包里复现，硬塞进托管 runner 只会得到一个
长期红着的 job——那比没有更糟，因为它会训练所有人忽略它。

因此分工是：

- **`.github/workflows/ci.yml` 的 `quality` job**：fmt / clippy / test，无外部依赖，
  在任何机器上都能跑，是合并门禁。
- **corpus 门禁**：在装有钉死工具链的机器上跑上面三条命令。语料变更的 PR 必须
  附上这三条命令的输出。

## 生成侧的硬约束

- 不得使用 mimus 生产侧的 lopdf 或 PDFium 生成或裁定 fixture（§2.5）。
  `crates/corpus/tests/no_production_engines.rs` 在依赖闭包上自动守卫这一条。
- expected manifest 必须**先于**生成器手写（§2.1）；现实排版 fixture 的几何期望
  是唯一例外，由 poppler 与 mutool 两个独立解析器一致确立。

## 精确 fixture 与畸形派生

`crates/corpus/src/exact.rs` 是 Corpus/M0 专用的裸字节 writer，不是 mimus 的生产
PDF writer。它不依赖 lopdf、PDFium、`pdfium-render` 或 `mimus-core`，并把对象号、
对象顺序、经典 xref/trailer、fixture ID 派生的 `/ID` 和未压缩 content stream
全部写成显式配方。完整 PDF 先在内存生成，再经同目录临时文件原子替换；失败时保留
既有目标并清理临时文件。

精确 fixture 的 `manifest.toml` 是先行手写规格。`corpus build` 只实现该规格，
`corpus verify` 再通过固定输入字节前缀、对象图、原始 stream 字节、poppler 文本盒、
MuPDF baseline、MuPDF SVG 字形轮廓和两个独立渲染器反向验收。精确几何不会由
`adjudicate` 回填；其 `adjudicated.toml` 只保存渲染哈希。

后续畸形 fixture 使用 `method = "byte-mutation"`，并在 `[lineage]` 中记录合法父本
fixture ID。唯一一条 `[[lineage.mutations]]` 必须同时记录 `byte_offset`、
`original_bytes`、`replacement_bytes` 和变异语义。派生 API 会检查父本偏移处的旧值，
生成后再逐字节核对父子文件只在该连续区间不同，防止一份 fixture 顺带改变第二个变量。

## 独立解析器的已知边界

MuPDF 1.28.2 与 Poppler 26.08.0 都不识别 Adobe Distiller 使用的合法
`DLIdent-H` / `DLIdent-V` Identity CMap 别名，因此无法为这类 fixture 提供文字或
字形几何。`CMAP-02` 的别名 fixture 改由原始对象图、content bytes、CID 序列和钉死
TrueType cmap 的静态组合证明裁定；生产路径测试另行断言别名识别和段落行为。此例外
只适用于该已知别名，不放宽普通 Identity 或嵌入 CMap fixture 的双解析器门禁。
