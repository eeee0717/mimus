# M0 实验 3：增量写回与对象图守恒

状态：完成（2026-08-22）

对应 Issue：[#10](https://github.com/eeee0717/mimus/issues/10) → [#13](https://github.com/eeee0717/mimus/issues/13)
对应决策：ADR-0003 §2、ADR-0006

## 结论

**成（PoC 路线可行）**。`lopdf::IncrementalDocument` 能以输入 PDF 的完整字节为前缀追加增量段；新对象从输入最大对象号之后连续分配，不复用独立 free xref slot；共享 `/Resources` 可以 copy-on-write，只改目标页；未修改对象、页面框、书签/命名目标/URI/注释/AcroForm/OCG 结构保持不变。qpdf、Poppler、MuPDF 均能读取并渲染输出，新增字体实际挂入 COW Resources 并用于 `POC`。

**范围限制**：这是可丢弃的 M0 PoC，不是生产 writer。字体对象只放入一个 Standard-14 marker，未实现字体子集化、翻译内容生成、完整 content-stream 重写或双语页模式。候选写回路径只消费主输入及 ObjStm Form、GEOM、generation、free-slot、shared-resources 五类 companion；singular CTM、mixed codespace 和两份 malformed fixture 只通过 Corpus 输入门禁，没有参与写回，因此本实验不声称这些边缘写回路径已经成立。生产实现仍需在 `mimus-core` 的 writer 边界内完成，并以本实验已证明的对象图断言为回归门禁。

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

新增 10 份合法 fixture 与 2 份畸形 fixture，均由 exact writer 或单字节变异 API 生成；`corpus doctor`、`corpus determinism`、`corpus verify` 已通过。新增的 outline sibling 父本为 PARSE-11 的合法基线，free-slot fixture 明确保留对象 10 的 free xref 槽。

| Fixture | Case | SHA-256 |
|---|---|---|
| `unit-write-01-bookmarks-rich` | WRITE-06 | `1e38b1cbc1c450e14e7b2be0f467a541f0295fe6d73bb05336ca70ac2e13d726` |
| `unit-write-02-shared-resources` | WRITE-04 | `d772b7e7eec883aa8a3fd80d06fd7e4532876c2a621b6aa0dbd4a6fa09faa16f` |
| `unit-write-03-resources-gen-nonzero` | WRITE-04 | `6b4dfce2bc5eea7587c20f158270e113858715359f1d8482fd6c17a19fc6e0d2` |
| `unit-write-04-xobj-in-objstm` | XOBJ-10 | `b0b3d9400fe9ae7e898cc2426b19a53287b0037aaa469265923ed2bc0b7c4278` |
| `unit-write-05-indirect-resources-objstm` | WRITE-01/02 | `43eebfe5d90dc0a3a61144e43b26e5b5e88ac29f57f9b70c74615b85fa655cd2` |
| `unit-geom-05-nonzero-origin-boxes` | GEOM-02 | `e882ed610d1c5976cce3c8256ffd9d30731bccf9f56982d3c8e061eebbb5fd14` |
| `unit-cmap-02-mixed-codespace` | CMAP-06 | `0ba78e199877361fadf1709b08ad7c60dc4e787384c8de737896edeec13966a5` |
| `unit-xobj-05-singular-ctm` | XOBJ-08 | `59e15b706fc8c7129e36b72bafd6c69913762c2c705fdbd9632a037f86c4742e` |
| `unit-parse-11-outline-siblings` | PARSE-11 | `26a21392e2df687718b72b3da2fed51bba1fc2c57e0731988753f49110f049ec` |
| `unit-write-06-free-object-slot` | WRITE-04 | `2926af8fe9f1ad4c8d00bebb2d70733bb2982d109bdc6f4b6281054a49c0d2b2` |
| `mal-parse-08-broken-objstm` | PARSE-11 | `0a27820506acda4e7532c2b5baf1393dce2ce2919047a5d52251b065d4ee749a` |
| `mal-parse-09-outlines-cycle` | PARSE-11 | `c40323553be0a9e5503a4e2872da19aeee8a1743b04c416361a10ff9bb61f107` |

畸形 fixture 分别把 ObjStm `/N 1` 改为 `/N 2`，以及把合法 sibling 的 `/Next 11 0 R` 单字节变异为真正的 `/Next 10 0 R` 自环；两者都保持父本全部其它字节不变，并由声明的结构 oracle fail-fast。

## #13：PoC 实验

### 输入与写回

主输入为 `corpus/fixtures/unit-base-03-structured/unit-base-03-structured.pdf`，包含一页、三级书签、命名目标、URI action、Link/Text/Widget 注释、AcroForm、OCG 与页面框。PoC 还对 ObjStm Form、CropBox、generation=7、free object 10 和两页共享 Resources 的 companion fixtures 各自产生增量输出并检查对象图。PoC 执行以下增量操作：

1. 用 `IncrementalDocument::create_from` 保留原始字节；
2. clone 目标页的 `/Resources`，追加资源对象并仅让目标页指向它（copy-on-write）；
3. 追加 content stream 和 font marker；
4. 保存到同目录临时文件，只有资源复制、字体追加和临时文件写入均成功后才 `rename` 发布。

实测结果：

| 量 | 结果 |
|---|---:|
| 输入 SHA-256 | `17159c6958a5ea359d53e26115aeef01f8917203ac90bfdf1797df0c5c0f46f4` |
| 输出 SHA-256 | `84599f43f20b732c84dca00c0b287dccd519189232c1bca47d82db86799e6bba` |
| 输入 / 输出长度 | 5121 / 5749 bytes |
| 追加字节 | 628 bytes |
| 输入最大对象号 | 18 |
| 新 Resources / Font / Content | 19 / 20 / 21 |

输出以输入的 5121 字节完整前缀开头。所有新增对象号均大于 18；free-slot companion 的输入 `/Size 11`、最大 live object 9，实际新增对象从 11 开始，没有复用对象 10。原资源对象仍被 AcroForm 默认资源引用，目标页才使用对象 19。

### 对象、xref 与引用差异

增量段新增对象 19、20、21，并新增 xref/trailer 链接到上一段；原始对象 1–18 的字节与对象值未改变。唯一业务引用变化是目标页对象 3 的 `/Resources` 从 `4 0 R` 改为 `19 0 R`，`/Contents` 改为 `21 0 R`；对象 4、书签 8–11、注释 12–14、OCG 15、命名目标 16、ToUnicode 17 与原页面结构引用保持不变。页面 `MediaBox`、`CropBox`、`Rotate` 逐项相等。

ObjStm companion 保留活动 Contents 9 及其中的 `/X1 Do`，依次追加页面 Resources 14、Helvetica 15、从 ObjStm 解出的 Form Resources 16 和非压缩 Form 17；新 Form 通过 `/F2` 绘制 `FORM COW`，Poppler 与 MuPDF 均同时提取到 `FORM COW` 和保留的 `MIMUS`。GEOM companion 在保留非零 MediaBox/CropBox 的同时按页面空间 `(150,220)` 写入 `POC`，两个文本引擎均可提取且两套渲染器的像素检查均确认页面非空白。generation companion 保留原 `4 7 R`；shared companion 只改第一页，第二页继续引用 `5 0 R`。shared fixture 的 descriptor 7 明确引用字体流 8，MuPDF 不再回退系统字体。

### 独立工具验收

输出文件位于 `.context/m0-lab/poc/`。独立验收脚本只使用 Python 标准库、qpdf、Poppler 与 MuPDF，不调用 lopdf；它比较输入前缀、qpdf JSON 对象图、活动 Contents/Form 引用、generation/free slot、共享资源、提取文本、MuPDF 字体诊断和两套渲染器的像素。执行：

```sh
~/.cargo/bin/cargo run --manifest-path experiments/m0-experiment-3-poc/Cargo.toml
python3 experiments/m0-experiment-3-poc/verify_outputs.py
```

结果：主输出与全部 companion 均通过 qpdf 结构检查；主输出文本为 `POC`，XOBJ 输出为 `FORM COW` + `MIMUS`，GEOM 输出为可见的 `POC`，shared 输出第二页仍为 `MIMUSC`。qpdf JSON 独立确认原富结构对象不变、新对象引用链有效、Form 仍被活动 Contents 执行、generation 7 保留、free object 10 未复用；Poppler 与 MuPDF 均确认上述文本，且两者对 XOBJ/GEOM 的栅格都不是空白页。

### 已注入失败的原子性

PoC 分别注入三类发生在原子 `rename` 前的失败。每次先把真实输入 PDF 写到即将传给 `incremental_rewrite` 的实际输出目标，再经同一完整写回/发布入口注入失败：

- 资源复制：`failure-resource.pdf` 在 COW Resources 的 `add_object` 前失败；
- 字体追加：`failure-font.pdf` 在字体 marker 的 `add_object` 前失败；
- 保存发布：`failure-save.pdf` 完成临时文件写入后、原子 rename 前失败。

三个实际目标均保持与输入 PDF 逐字节相同，并继续通过 qpdf；保存失败主动删除同目录临时文件。因此本实验只证明资源复制、字体追加和临时文件写入后的这三个已注入失败不会覆盖既有产物或留下半译输出，不把结论外推到发布后的 reload 等任意失败。

## 可重放命令

```sh
~/.cargo/bin/cargo run --manifest-path experiments/m0-experiment-3-poc/Cargo.toml
python3 experiments/m0-experiment-3-poc/verify_outputs.py
~/.cargo/bin/cargo test --manifest-path experiments/m0-experiment-3-poc/Cargo.toml
~/.cargo/bin/cargo fmt --all -- --check
~/.cargo/bin/cargo clippy --workspace --all-targets --all-features -- -D warnings
~/.cargo/bin/cargo test --workspace --all-targets --all-features
~/.cargo/bin/cargo run -p corpus -- doctor
~/.cargo/bin/cargo run -p corpus -- determinism
~/.cargo/bin/cargo run -p corpus -- verify
```

## 替代方案与后续

本实验**没有失败**，因此不触发 ADR 复议。若生产 writer 无法满足“输入完整前缀 + COW + 原子发布”，最小替代方案是保留对象图的手写增量 serializer；**从零重建整篇 PDF 不算本实验成功**，只能作为扫描件/V2 路径另立决策。
