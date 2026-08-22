# M0 实验 3：增量写回与对象图守恒

状态：完成（2026-08-22）

对应 Issue：[#10](https://github.com/eeee0717/mimus/issues/10) → [#13](https://github.com/eeee0717/mimus/issues/13)
对应决策：ADR-0003 §2、ADR-0006

## 结论

**成（PoC 路线可行）**。`lopdf::IncrementalDocument` 能以输入 PDF 的完整字节为前缀追加增量段；新对象从输入最大对象号之后连续分配；共享 `/Resources` 可以 copy-on-write，只改目标页；未修改对象、页面框、书签/命名目标/URI/注释/AcroForm/OCG 结构保持不变。qpdf、Poppler、MuPDF 均能读取并渲染输出。

**范围限制**：这是可丢弃的 M0 PoC，不是生产 writer。字体对象只放入一个 Type1 marker，未实现字体子集化、翻译内容生成、完整 content-stream 重写或双语页模式。生产实现仍需在 `mimus-core` 的 writer 边界内完成，并以本实验的对象图断言为回归门禁。

## 环境

运行平台为 Apple Silicon macOS（`aarch64-apple-darwin`）。版本由本机命令及 `corpus/toolchain.toml` 钉死：

| 工具 | 版本 |
|---|---|
| Rust / Cargo | rustc 1.97.1 / cargo 1.97.1 |
| lopdf（隔离 PoC） | 0.44.0 |
| qpdf | 12.4.0 |
| Poppler | 26.08.0（`pdftotext`/`pdfinfo`/`pdftoppm`） |
| MuPDF | mutool 1.28.2 |
| Typst | 0.15.1 |

PoC crate 位于 [`experiments/m0-experiment-3-poc`](../experiments/m0-experiment-3-poc)，带独立 `[workspace]`，不加入生产 workspace。

## #10：fixture 交付

新增 8 份合法 fixture 与 2 份畸形 fixture，均由 exact writer 或单字节变异 API 生成；`corpus doctor`、`corpus determinism`、`corpus verify` 已通过（总计 37 份 fixture）。

| Fixture | Case | SHA-256 |
|---|---|---|
| `unit-write-01-bookmarks-rich` | WRITE-06 | `625417a93f2764b1937983cb5f647ca3369190487ed25a8507115ba9a3974d42` |
| `unit-write-02-shared-resources` | WRITE-04 | `fe1ab21b6c6aecaa2399cd38d61a7ba6c6a06d451989e43c7e08e6c4e5416aa8` |
| `unit-write-03-resources-gen-nonzero` | WRITE-04 | `1f24d31273e50ca71767e1f672ce60fdb8ff20413060424d4611f053d08de108` |
| `unit-write-04-xobj-in-objstm` | XOBJ-10 | `c41cdec88d931c0120f17c015789920640481520877a9875f1d0b63b0c38dd12` |
| `unit-write-05-indirect-resources-objstm` | WRITE-01/02 | `9cc9bf198c40fc905270c5accfe6c220f5a2a1a3e19af7de5fc7fc9de44f0076` |
| `unit-geom-05-nonzero-origin-boxes` | GEOM-02 | `8f9b048dd321dd2367a18c3055986801b26293a61676dc25f1645789799c984d` |
| `unit-cmap-02-mixed-codespace` | CMAP-06 | `aac3252b5ed89f7cd18b07e27d27c4a30a6620a11de34e668265c565d558a2e5` |
| `unit-xobj-05-singular-ctm` | XOBJ-08 | `decdbfd155e2322d2a26188d036bc3c0a36db4b17d396dede4b8fc7f2a878466` |
| `mal-parse-08-broken-objstm` | PARSE-11 | `68de9604a241df6c99ff9d2329ab01bc5fc1f81278bd66c819e1a7d78bc9cb82` |
| `mal-parse-09-outlines-cycle` | PARSE-11 | `36429acd6d1fb9b90067fc084392acd47cb34a7a62328c97eb00df5ce2a9cc8b` |

畸形 fixture 分别把 ObjStm `/N 1` 改为 `/N 2`，以及把 `/Outlines /First` 改成非法引用；两者都保持父本全部其它字节不变，并由 qpdf 以声明的错误类别 fail-fast。

## #13：PoC 实验

### 输入与写回

输入为 `corpus/fixtures/unit-base-03-structured/unit-base-03-structured.pdf`，包含一页、共享资源、三级书签、命名目标、URI action、Link/Text/Widget 注释、AcroForm、OCG 与页面框。PoC 执行以下增量操作：

1. 用 `IncrementalDocument::create_from` 保留原始字节；
2. clone 目标页的 `/Resources`，追加资源对象并仅让目标页指向它（copy-on-write）；
3. 追加 content stream 和 font marker；
4. 保存到同目录临时文件后 `rename`，避免覆盖原文件或留下半成品。

实测结果：

| 量 | 结果 |
|---|---:|
| 输入 SHA-256 | `d2f6df979fb5ef328cf3a1ac666360563d73caf94884b4ec8fa619f90ba0fbd9` |
| 输出 SHA-256 | `c098c59e943e176df563884dff237b94ef07c2067291dbae8845ee606ddbe6d7` |
| 输入 / 输出长度 | 5181 / 5799 bytes |
| 追加字节 | 618 bytes |
| 输入最大对象号 | 18 |
| 新 Resources / Content / Font | 19 / 20 / 21 |

输出以输入的 5181 字节完整前缀开头。所有新增对象号均大于 18；没有复用 free/deleted object number。原资源对象仍被 AcroForm 默认资源引用，目标页才使用对象 19。

### 对象、xref 与引用差异

增量段新增对象 19、20、21，并新增 xref/trailer 链接到上一段；原始对象 1–18 的字节与对象值未改变。唯一业务引用变化是目标页对象 3 的 `/Resources` 从 `4 0 R` 改为 `19 0 R`，`/Contents` 改为 `20 0 R`；对象 4、书签 8–11、注释 12–14、OCG 15、命名目标 16、ToUnicode 17 与原页面结构引用保持不变。页面 `MediaBox`、`CropBox`、`Rotate` 逐项相等。

### 独立工具验收

输出文件为 `.context/m0-lab/poc/incremental-output.pdf`。执行：

```sh
qpdf --check .context/m0-lab/poc/incremental-output.pdf
pdftotext .context/m0-lab/poc/incremental-output.pdf -
pdfinfo .context/m0-lab/poc/incremental-output.pdf
mutool draw -F stext .context/m0-lab/poc/incremental-output.pdf
mutool draw -o .context/m0-lab/poc/render-%d.png -r 150 .context/m0-lab/poc/incremental-output.pdf
```

结果：qpdf 结构检查通过；Poppler 提取文本为 `POC`；`pdfinfo` 报告 1 页、`300 x 200 pt`、PDF 1.7；MuPDF 输出一页 stext，页面空间坐标与 Poppler 一致，150 dpi 渲染得到有效的 `625 x 417` PNG（默认 72 dpi 时为 `300 x 200`）。原输入的富结构对象仍可由 lopdf 重新加载并逐对象比较。

### 失败原子性

PoC 分别注入三类失败，并在每次失败前写入 `known-good` 哨兵文件：

- 资源复制：在 `add_object` 前注入 `injected resource copy failure`；目标文件不变；
- 字体追加：在字体 marker 的 `add_object` 前注入 `injected font append failure`；目标文件不变；
- 保存发布：将目标设为已存在目录 `failed-output/child`，注入 `rename` 失败（`Is a directory (os error 21)`）。

保存路径先写同目录临时文件，`rename` 失败时主动删除临时文件。`failed-output/sentinel.pdf` 内容仍为 `known-good`，临时文件不存在，因此保存失败不会覆盖既有产物，也不会留下半译输出。资源复制与字体追加共用同一保存事务，若任一步失败则不会发布最终路径。

## 可重放命令

```sh
~/.cargo/bin/cargo run --manifest-path experiments/m0-experiment-3-poc/Cargo.toml
~/.cargo/bin/cargo fmt --all -- --check
~/.cargo/bin/cargo clippy --workspace --all-targets --all-features -- -D warnings
~/.cargo/bin/cargo test --workspace --all-targets --all-features
~/.cargo/bin/cargo run -p corpus -- doctor
~/.cargo/bin/cargo run -p corpus -- determinism
~/.cargo/bin/cargo run -p corpus -- verify
```

## 替代方案与后续

本实验**没有失败**，因此不触发 ADR 复议。若生产 writer 无法满足“输入完整前缀 + COW + 原子发布”，最小替代方案是保留对象图的手写增量 serializer；**从零重建整篇 PDF 不算本实验成功**，只能作为扫描件/V2 路径另立决策。
