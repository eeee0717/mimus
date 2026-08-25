# M1 收口记录

> 收口日期：2026-08-24。范围：GitHub Issues #20–#24。

## 库存与规划估算

M1 收口时 `corpus/fixtures/` 包含 **133 份 fixture**，manifest 的
`identity.cases` 合并后为 **72 个唯一 case ID**。其中 120 份 fixture 带有独立的
`adjudicated.toml`；其余 fixture 由手写 manifest、确定性 writer、单变量
mutation 与独立 oracle 直接裁定。

`docs/03-corpus-requirements.md` §4.4 的约 138 份 / 87 个 case 是 M-1 阶段的容量
估算，不是库存配额。最终数字较小有三个原因：等价变体按已接受政策合并；一份
fixture 可以覆盖多个 case；不同 concern 复用相同的精确父本而不复制 PDF。没有
为凑数加入重复 fixture，也没有删除 manifest 断言或放宽容差。

## Production gate

普通 CI 运行以下两层互相独立的门禁：

1. `cargo test --workspace --all-targets --all-features` 对 133 份 fixture 逐一执行
   production `inspect` 与 `translate --backend none`。每次运行必须正常退出而非
   signal/abort，以恰一个 typed terminal event 结束；成功路径必须落齐逐 pass IL
   快照，`none` 的段落文本守恒，增量输出保留完整输入前缀并通过 `qpdf --check`；
   拒绝路径必须是 Input/2 且不产生输出。专门矩阵继续断言 manifest 几何、字符
   transform、扫描/加密分类、段页降级和资源对象图。
2. `cargo run -p corpus -- audit` 不依赖 `mimus-core`，逐份复核 manifest schema、
   PDF SHA-256、单变量谱系、字体 pin、legal fixture 的 qpdf 合法性，以及已提交的
   Poppler/MuPDF 几何与双渲染器哈希记录。畸形输入的声明错误由 production matrix
   与钉死工具链复核。托管 runner 的 MuPDF/排版引擎版本不符合
   `corpus/toolchain.toml`，因此版本敏感的实时重放仍由钉死工具链上的
   `corpus doctor` + `corpus determinism` + `corpus verify` 执行，不能用 runner
   的不同结果改写裁定文件或放宽容差。

M1 尚无占位符编码阶段：`StylesAndFormulas` 与 `ExtractTerms` 仍是显式空 pass，
`none` translator 是 identity adapter。因此本里程碑的“基础占位符守恒”落实为
每个未保留段在 Translate 快照中的文本与请求源文本完全相同；真正的占位符协议
与失败降级属于 M2。

Layout 的普通 CI 路径只使用确定性的 `SingleLineLayoutDetector` 或提交的 replay
recording。真实 PP-DocLayoutV3 仍限于 ignored/nightly 或显式本地验证，普通 CI
不会下载或执行 131 MB 模型。

## Acceptance map

| Issue | Production evidence |
|---|---|
| #20 | `layout_policy_replay_matrix_matches_manifest_regions_candidates_and_passthrough`：recording 字节回放、25 类政策、fallback 大框继承 |
| #21 | `paragraph_reconstruction_matrix_matches_manifest_order_lines_and_text`：model order、几何兜底、分栏/段落/Form/空格稳定重建 |
| #22 | 字体/CMap matrix 与独立输出检查：Regular/Bold 实际 glyph 子集、完整 ToUnicode、混排换行与安全降级 |
| #23 | `writeback_fixture_matrix_preserves_prefix_structure_and_resource_identity`：资源 COW、generation/ObjStm/free slot、前缀与结构守恒 |
| #24 | `m1_corpus_inventory_runs_every_fixture_through_bounded_production_paths` + CI `corpus verify`：133/72 全库存 production 与独立门禁 |

## PDFium 与外部资产

本地验收使用与 `pdfium-render` API 匹配的 PDFium `chromium/8009`。CI 下载的
Linux x64 archive SHA-256 固定为
`be513e8021a5bf8eb2116e00d78c3bacb82c5a02b3785156ae14fe5e33084385`；本地
macOS arm64 `libpdfium.dylib` 的 SHA-256 为
`cfab7b27942132aea1a1ff7ff42ce970c39f7d928c1fc317ea99d3bfa3a43d0c`。

本收口没有新增 ADR。库存去重、8 pt 最小字号、1.5 em 行距和 subset tag 派生均为
现有 ADR 边界内的可逆实现细节；若真实语料证伪，可在对应最低层修改并重跑同一门禁。
