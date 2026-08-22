# 实验 4：firecrawl-pdfium 后端资格验证

本 workspace 在不接入 `mimus-core`、不修改生产依赖的前提下，对拍
`pdfium-render` 0.9.1 与固定 revision 的 `firecrawl-pdfium`。实验回答的是：后者能否在
ADR-0006 的 `PdfInspector` / `Rasterizer` trait 边界后替代前者。

最终证据与 B 结论见 [`docs/05-pdfium-backend-qualification.md`](../../docs/05-pdfium-backend-qualification.md)，
去除本机路径的机器摘要见 [`summary.json`](summary.json)。

## 程序与隔离

workspace 包含三个可执行程序和一个只承载 JSON schema 的共享 crate：

- `reference-runner`：仅依赖 `pdfium-render = 0.9.1`；
- `candidate-runner`：仅依赖
  `firecrawl-pdfium@1a4c91d0c5f80c0da779088ba241bf1e45271cd5`；
- `orchestrator`：启动 runner、施加超时、采样 RSS、比较版本化 JSON；
- `common`：双方共享的 schema、哈希和原子 checkpoint 实现，不初始化 PDFium。

两个 PDFium wrapper 永远不在同一进程。`run` 与 `batch` 都要求显式传入
`--pdfium-library`，实验没有系统库搜索回退。每页 JSON 包含 PDF SHA-256、页号、页面尺寸、
`Rotate`、完整文本、逐字符 Unicode/code/origin/tight/loose box、RGBA8 渲染尺寸与哈希，
以及文本、渲染和总耗时。比较器使用 schema，不解析人类日志；几何容差是 `0.001 pt`，
其余字段精确比较。

报告中的完整文本是逐字符 `FPDFText_GetUnicode` code 序列的 UTF-16 解码，两端共用同一个
纯函数。不能混用 wrapper 的便捷全文 API：PDFium 对断词伪字符会分别从
`FPDFText_GetUnicode` / `FPDFText_GetBoundedText` 返回 `U+0002`，从 `FPDFText_GetText`
返回 `U+FFFE`。逐字符视图才与字符数、index 和几何处在同一个索引域。

`candidate-runner probe-is-generated` 是隔离的 `libloading` 探针。它直接解析 PDFium symbol
并自行管理 document/page/text-page handle，不把它冒充为 `firecrawl-pdfium::sys` 能力。

## 固定版本

| 组件 | 固定值 |
|---|---|
| firecrawl-pdfium | Git revision `1a4c91d0c5f80c0da779088ba241bf1e45271cd5` |
| pdfium-render | crates.io `0.9.1`，与 M0 实验 2 相同 |
| PDFium 7988 archive | `chromium/7988` mac-arm64，SHA-256 `2229b8a6ffa0fb1634aa78886ed5425a10a8a0bd01762f7f0c9244081d86a921` |
| PDFium 7988 dylib | SHA-256 `fbdec47c3f2eaa80705ed25cf8bed5ac420998ba0f3e786d4d297b6238749064` |
| PDFium 8009 archive | `chromium/8009` mac-arm64，SHA-256 `b1f2f17c7432a9942514dda5094ee9822c743bdfd07e7187725efbd34fde941f` |
| PDFium 8009 dylib | SHA-256 `cfab7b27942132aea1a1ff7ff42ce970c39f7d928c1fc317ea99d3bfa3a43d0c` |
| 可选 API 7763 dylib | SHA-256 `cb8e259f914dda33f8930751e9a70afd3168893a569f7e59d34d29c4bc5701c3` |

7988 的 URL 与 archive hash 来自该 revision 的 `pdfium.lock.json`。8009 取自：

```text
https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8009/pdfium-mac-arm64.tgz
```

## 构建

从仓库根目录执行：

```bash
cargo build --locked --release \
  --manifest-path experiments/experiment-4-pdfium-qualification/Cargo.toml
```

为下列命令准备路径。`PDFIUM_7763` 是可选档；`HELLO_WORLD_PDF` 指向固定 revision 的
`tests/fixtures/pdfium/hello_world.pdf` 副本。所有二进制和原始结果都应留在 `.context`。

```bash
export EXP4=experiments/experiment-4-pdfium-qualification
export ORCHESTRATOR="$EXP4/target/release/orchestrator"
export REFERENCE="$EXP4/target/release/reference-runner"
export CANDIDATE="$EXP4/target/release/candidate-runner"
export PDFIUM_7763=/path/to/api-7763/libpdfium.dylib
export PDFIUM_7988=.context/experiment-4/pdfium/chromium-7988/lib/libpdfium.dylib
export PDFIUM_8009=.context/experiment-4/pdfium/chromium-8009/lib/libpdfium.dylib
export HELLO_WORLD_PDF=.context/experiment-4/hello_world.pdf
```

先校验 dylib；不要在哈希不符时继续：

```bash
shasum -a 256 "$PDFIUM_7763" "$PDFIUM_7988" "$PDFIUM_8009"
```

## 正确性与有界失败

`matrix` 未传 `--request` 时发现 `corpus/fixtures` 下的全部 fixture。每份输入由两个 runner
分别在独立子进程中运行，默认上限 30 秒；每次比较后原子更新 checkpoint，可用相同命令续跑。

```bash
"$ORCHESTRATOR" matrix \
  --reference-runner "$REFERENCE" \
  --candidate-runner "$CANDIDATE" \
  --pdfium "api-7763=$PDFIUM_7763" \
  --pdfium "chromium-7988=$PDFIUM_7988" \
  --pdfium "chromium-8009=$PDFIUM_8009" \
  --repo-root . \
  --output .context/experiment-4/results/correctness \
  --is-generated-fixture "$HELLO_WORLD_PDF" \
  --timeout-seconds 30
```

`matrix` 同时写出 `legal-request.json` 与 `malformed-request.json`。合法输入只接受完全相等；
malformed 可以有等价的分类失败差异，但 crash、hang 和协议错误会使命令失败。

## 性能

性能使用 release runner。`threads` 测单进程 1/2/4/8 worker，`processes` 仅给出吞吐上界：

```bash
"$ORCHESTRATOR" benchmark \
  --reference-runner "$REFERENCE" \
  --candidate-runner "$CANDIDATE" \
  --pdfium "api-7763=$PDFIUM_7763" \
  --pdfium "chromium-7988=$PDFIUM_7988" \
  --pdfium "chromium-8009=$PDFIUM_8009" \
  --request .context/experiment-4/results/correctness/legal-request.json \
  --output .context/experiment-4/results/performance \
  --thread-counts 1,2,4,8 \
  --process-counts 1,2,4,8 \
  --iterations 5
```

runner 先执行一轮 warm-up。摘要记录吞吐、文档 p50/p95、peak RSS 和 RSS samples。
默认再执行 5 轮 measured iteration；其间传入 `--no-durable-checkpoints`，吞吐分母取 runner
报告的 warm-up 后 `elapsed_us`，避免 checkpoint I/O 与进程启动时间污染测量。

## 常驻进程长跑

每个 backend/PDFium 组合只启动一个 `batch` 进程。runner 在同一进程中完成最多 200 轮，
每个 fixture 后将 `BatchJobCheckpoint` 原子写入
`checkpoint.json.jobs/<sha256(input_id)>.json`；正常结束时才汇总完整 `BatchReport`。恢复时会
校验 schema/backend/revision/PDFium/threads 和 sidecar 文件名，再跳过已完成 job，因而 checkpoint
写入量随 job 数线性增长。orchestrator 在一个全局 8 小时截止点内持续等待，超时会杀进程并
`wait`，不会留下后台任务。RSS 从 runner 写出 warm-up-complete marker 后开始每秒采样。

```bash
"$ORCHESTRATOR" long-run \
  --reference-runner "$REFERENCE" \
  --candidate-runner "$CANDIDATE" \
  --pdfium "api-7763=$PDFIUM_7763" \
  --pdfium "chromium-7988=$PDFIUM_7988" \
  --pdfium "chromium-8009=$PDFIUM_8009" \
  --request .context/experiment-4/results/correctness/legal-request.json \
  --output .context/experiment-4/results/long-run \
  --max-rounds 200 \
  --max-hours 8
```

摘要逐 backend/version/fixture 检查去除耗时字段后的 `report_sha256` 是否跨轮稳定。RSS
同时记录 peak、前半程均值、后半程均值与增长比例；后半程高出 20%，或至少 10 个样本中
不下降区间达到 95% 且首尾增长超过 5%，会标记 `rss_requires_investigation`。

## 真实 PDF

真实 arXiv PDF 不入库。按 [`summary.json`](summary.json) 的 URL、Producer、页数和 SHA-256
准备不超过 20 份、总页数不超过 1000 的 request，然后复用 `matrix --request`。原始 PDF、
逐页 JSON 和差异样本必须写入 `.context/experiment-4/`。

## 本地门禁

```bash
cargo fmt --all --manifest-path "$EXP4/Cargo.toml" -- --check
cargo clippy --workspace --all-targets \
  --manifest-path "$EXP4/Cargo.toml" -- -D warnings
cargo test --workspace --manifest-path "$EXP4/Cargo.toml"
```

`target/`、PDFium dylib/archive、真实 PDF、渲染图和原始日志均不属于提交物。
