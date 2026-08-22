# M0 实验 3：增量写回与对象图守恒

状态：完成（2026-08-22）

对应 Issue：[#10](https://github.com/eeee0717/mimus/issues/10) → [#13](https://github.com/eeee0717/mimus/issues/13)
对应决策：ADR-0003 §2、ADR-0006

## 结论

**成（PoC 路线可行）**。`lopdf::IncrementalDocument` 能以输入 PDF 的完整字节为前缀追加增量段；新对象从输入最大对象号之后连续分配，不复用独立 free xref slot；共享 `/Resources` 可以 copy-on-write，只改目标页；未修改对象、页面框、书签/命名目标/URI/注释/AcroForm/OCG 结构保持不变。qpdf、Poppler、MuPDF 均能读取并渲染输出，新增字体实际挂入 COW Resources 并用于 `POC`。

**范围限制**：这是可丢弃的 M0 PoC，不是生产 writer。字体对象只放入一个 Standard-14 marker，未实现字体子集化、翻译内容生成、完整 content-stream 重写或双语页模式。生产实现仍需在 `mimus-core` 的 writer 边界内完成，并以本实验的对象图断言为回归门禁。

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
| `unit-write-01-bookmarks-rich` | WRITE-06 | `e845f97f78575c79d57e12372a443c947e3c87708052d48b2fc9e87e55fcf337` |
| `unit-write-02-shared-resources` | WRITE-04 | `e8c3388835fec5df2927bbbf4c2969c97dfa8abb8ba12d5232c4d2531053d5ee` |
| `unit-write-03-resources-gen-nonzero` | WRITE-04 | `1113f4558025d67ee58b4fc8a5569d5af6545231aa4f4e187486cb6e3b01c4e0` |
| `unit-write-04-xobj-in-objstm` | XOBJ-10 | `b88b964e36b7de784d16885c375ef65d6b239160ee4d287029ee7863a41c13ba` |
| `unit-write-05-indirect-resources-objstm` | WRITE-01/02 | `52abe0b8c06a9cbd6dac8506db0dccb39853c3f363938819915c5a4e95d49d7b` |
| `unit-geom-05-nonzero-origin-boxes` | GEOM-02 | `c169b3bb8b8f73649ca2070c498c25b8e85de55b895254c4d64a1c2794bb12a0` |
| `unit-cmap-02-mixed-codespace` | CMAP-06 | `9aa8e3f463ced2f15c45ef1d93da23a6f38be8db04e742db6a44e814d69b1439` |
| `unit-xobj-05-singular-ctm` | XOBJ-08 | `4f35cd78b35c335c24a195a1c172695c978559aa683179afc985fa55c3217510` |
| `unit-parse-11-outline-siblings` | PARSE-11 | `e81f83c3a1fc0b4b1bc312603168203af536b75f52f927dabcb829456fe59656` |
| `unit-write-06-free-object-slot` | WRITE-04 | `41cde56eb8cad1a8cbc3498aef86cd270ab3d136eea5b054ce073e7aea72f4eb` |
| `mal-parse-08-broken-objstm` | PARSE-11 | `ca1a1357a29087841f308e71d98b7d6f869b32c6f112a09471caf350b9fd0fc0` |
| `mal-parse-09-outlines-cycle` | PARSE-11 | `5e200fcc2d697965e602fb93f789313f64fce2ced0d585aaea0aed77bb791ec4` |

畸形 fixture 分别把 ObjStm `/N 1` 改为 `/N 2`，以及把合法 sibling 的 `/Next 11 0 R` 单字节变异为真正的 `/Next 10 0 R` 自环；两者都保持父本全部其它字节不变，并由声明的结构 oracle fail-fast。

## #13：PoC 实验

### 输入与写回

主输入为 `corpus/fixtures/unit-base-03-structured/unit-base-03-structured.pdf`，包含一页、共享资源、三级书签、命名目标、URI action、Link/Text/Widget 注释、AcroForm、OCG 与页面框；PoC 同时检查 ObjStm Form、CropBox、generation=7 和 free object 10 的 companion fixtures。PoC 执行以下增量操作：

1. 用 `IncrementalDocument::create_from` 保留原始字节；
2. clone 目标页的 `/Resources`，追加资源对象并仅让目标页指向它（copy-on-write）；
3. 追加 content stream 和 font marker；
4. 保存到同目录临时文件后 `rename`，避免覆盖原文件或留下半成品。

实测结果：

| 量 | 结果 |
|---|---:|
| 输入 SHA-256 | `d2f6df979fb5ef328cf3a1ac666360563d73caf94884b4ec8fa619f90ba0fbd9` |
| 输出 SHA-256 | `ef9d1548c3b5b9c18ee65f7381b72affc0c7f928add7555f74489fb7d08dd594` |
| 输入 / 输出长度 | 5181 / 5809 bytes |
| 追加字节 | 628 bytes |
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
- 保存发布：先写入已存在的 `existing-output.pdf` 临时目标，保存完成后注入失败。

保存路径先写同目录临时文件，注入失败时主动删除临时文件；`existing-output.pdf` 的 SHA-256 保持不变，因此保存失败不会覆盖既有产物，也不会留下半译输出。资源复制与字体追加共用同一保存事务，若任一步失败则不会发布最终路径。

## 可重放命令

```sh
~/.cargo/bin/cargo run --manifest-path experiments/m0-experiment-3-poc/Cargo.toml
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
