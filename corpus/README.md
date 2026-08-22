# Corpus v1

合同正文在 [`docs/03-corpus-requirements.md`](../docs/03-corpus-requirements.md) §2。
本目录是它的可执行形式。

```
corpus/
  toolchain.toml     钉死的工具版本 + 每个现实排版引擎的确定性配方（唯一真源）
  determinism/       §2.6 重复生成门禁的探针源文件与共享确定性开关
```

门禁入口是 workspace 里的 `corpus` 二进制（`crates/corpus`）。

```sh
cargo run -p corpus -- doctor        # §2.8 工具链齐备且版本精确匹配
cargo run -p corpus -- determinism   # §2.6 每个引擎连续构建两次、SHA-256 必须一致
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
