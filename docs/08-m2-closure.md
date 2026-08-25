# M2 实现收口记录

> 验证日期：2026-08-25。范围：GitHub Issues #25–#32。本文记录线性 stack 的
> 实现门禁；stack 尚未合入 `master`，因此 M2 正式收口仍待 review、合并和 master CI。

## 库存与 M1 基线

当前 `corpus/fixtures/` 包含 **142 份 fixture、80 个唯一 case ID**。其中 102 份
manifest 声明为 legal；production OpenAI 路径为 95 份生成有效输出，另 7 份按既有
Input/2 合同拒绝且不产生输出、不调用翻译后端：

- `intg-scan-10-nine-of-ten`
- `intg-scan-11-four-of-five`
- `intg-scan-12-image-with-blank-backs`
- `unit-doc-03-aes128-user-password`
- `unit-doc-03-rc4-empty-password`
- `unit-scan-01-image-only`
- `unit-scan-02-invisible-ocr`

M1 在 2026-08-24 收口时的库存是 133 份 / 72 个 case。此后为跨引擎 alignment
资格工作新增了 9 份 fixture / 8 个唯一 case；它们不是为 M2 配额补数。没有复制
fixture、修改 M1 manifest、删除断言或放宽容差。

## Deterministic Responses gate

`crates/mimus/tests/m2_gate.rs` 启动每个测试独享的 `127.0.0.1:0` server，由系统分配
端口，并在析构时停止线程和 socket。子进程显式把 HTTP(S) proxy 指向不可用的
loopback 地址，只为 fake server 设置 `NO_PROXY`，所以门禁不能访问公网。

Fake server 只接受 `POST /v1/responses`，并逐请求验证 Bearer canary、`model`、
`instructions` 和 `input`；响应使用 `output_text`。术语提取请求固定返回 canonical
空术语 JSON，且不消费段落故障脚本。翻译文本由输入确定，只保留 `{vN}` 与
`<bN>...</bN>` 协议标记，不复制 production prompt 或 validator。

全 legal inventory 关闭自动术语以隔离段落翻译，共产生 **131 次段落 Responses 请求**
和 **95 份**输出；7 条拒绝路径产生 0 次请求。每份输出均经 `qpdf --check` 与 Poppler
`pdftotext` 验证；MuPDF 对普通输出执行 `mutool draw -F txt`，对 manifest 明确声明
renderer stack-overflow 的 `unit-xobj-depth-overflow` 执行非递归的 `mutool info`。

聚焦矩阵使用默认自动术语：首次运行是 1 次术语提取 + 429 后 2 次段落尝试，共 3 次；
完全相同的第二次运行新增 0 次；model 与 target language 变化各新增 1 次术语提取和
1 次段落翻译；用户 glossary 变化复用自动术语，只新增 1 次段落翻译，主 server 合计
8 次（3 次术语、5 次段落）。未知 placeholder、delayed success、malformed raw body 和
disconnect 的独立 server 均为 1 次术语 + 1 次段落请求；malformed 与 disconnect 只
保留所属段并以 `translation_failure` 汇总。prompt version 没有 CLI override，段落和
术语 prompt 失效分别由 cache-key unit tests 直接钉死。

## Acceptance evidence

| Concern | Production-path evidence |
|---|---|
| Responses API / config | wire gate 只接受 `/v1/responses` 的 `model` + `instructions` + `input`；CLI config matrix 覆盖 flags > env > TOML、空值和 `none` 后端 |
| Placeholder | 全 inventory 的 deterministic response 保持协议标记；未知 `{v999}` 保留整段、输出与输入 PDF 逐字节相同，并报告 `placeholder_violation` |
| Glossary | 自动/用户术语合并、canonical round trip、稳定 fingerprint 与请求注入由 core tests 覆盖；glossary 变化令 CLI cache miss |
| Cache | 默认自动术语下首次 miss、第二次术语与段落均命中且新增 API 调用为 0；model、target、glossary 各自按所属 key 失效；两个 prompt version 由 cache-key unit tests 覆盖；非法响应不入 cache |
| Concurrency / retry | production paragraph executor 的最大 in-flight 实测为 3（配置值 3；默认 4）；429 精确重试一次并发布有序 retry diagnostic；全 transient 分类的三次退避由可替换 sleeper 单测覆盖 |
| Degradation / strict | placeholder/backend/fit failure 只保留所属段；normal 发布带汇总的保留结果，strict 返回 Translation/4 且不发布或覆盖目标文件 |
| Table | 默认 `translate_table=false` 保留 table 输入字节且后端调用为 0；实验开关按 cell 翻译，单 cell 失败只保留该 cell；完整 `TJ` operand 与 active CTM 有回归覆盖 |
| Machine protocol | gate 观察 resolved configuration、stage/page progress、cache miss/hit、retry、degradation summary 和唯一 terminal result/error，均为 schema v2 NDJSON |

Canary `mimus-m2-secret-canary` 不出现在 stdout、stderr、NDJSON、debug pass 文件、
输出 PDF、redb cache 或 Responses request body。Authorization 只在 header 中由 fake
server 比对；测试不读取真实 key、shell history 或用户配置。`.env.local` 保持 ignored，
不属于提交或测试输入。

## Regression and independent oracles

Top branch 的 required local gate 为：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
cargo run -p corpus -- audit
cargo run -p corpus -- doctor
cargo run -p corpus -- determinism
cargo run -p corpus -- verify
```

Workspace tests 同时保留 M1 `inspect` / `translate --backend none` 全 inventory 路径，
以及 scan、encryption、geometry、font、layout、incremental writeback 和原子发布合同。
Corpus 工具独立复核 manifest schema、fixture SHA-256、单变量谱系、qpdf、Poppler、
MuPDF、双 renderer 与钉死版本，不以 production translation 实现作为唯一 oracle。
M2 生成结果本身也逐份经过 qpdf、Poppler 与上述 manifest-aware MuPDF 检查。

PDFium 使用 `pdfium-render` 当前 API 对应的 `chromium/8009`。本地 macOS arm64
`libpdfium.dylib` SHA-256 为
`cfab7b27942132aea1a1ff7ff42ce970c39f7d928c1fc317ea99d3bfa3a43d0c`；
所有 PDFium-backed required tests 都显式设置 `MIMUS_PDFIUM_LIBRARY`，没有静默 skip。

## 决策、可逆性与剩余风险

本 stack 新增 ADR-0016（Responses API 与三层配置）和 ADR-0017（分级降级与
strict）。placeholder 编码、glossary canonicalization、redb key、rayon worker pool、
退避参数和实验性 table segmentation 都在这些公开边界内，属于可由同一门禁替换的
实现细节。`--translate-table` 默认关闭，因此尚不承诺为稳定政策。

门禁不验证真实公网 provider 的可用性、质量、费率限制差异或翻译质量，也不执行真实
PP-DocLayoutV3 模型；普通 CI 继续使用 deterministic detector / replay。跨 provider
兼容性与真实文档质量反馈仍属于合并后运营和 M3 范围。
