# M2 实现收口记录

> 验证日期：2026-08-26。范围：GitHub Issues #25–#32。本文记录线性 stack 的
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
空术语 JSON，且不消费段落故障脚本。翻译文本由输入确定，从测试输出字体覆盖的常用
汉字集合按输入稳定采样，并原样保留 `{vN}`、`{lN}` 与 `<bN>...</bN>` 协议标记；它不
复制 production prompt 或 validator，也不再用「中/文」两个占位字规避真实字体覆盖。

全 legal inventory 关闭自动术语以隔离段落翻译，共产生 **131 次段落 Responses 请求**
和 **95 份**输出；7 条拒绝路径产生 0 次请求。每份输出均经 `qpdf --check` 与 Poppler
`pdftotext` 验证；MuPDF 对普通输出执行 `mutool draw -F txt`，对 manifest 明确声明
renderer stack-overflow 的 `unit-xobj-depth-overflow` 执行非递归的 `mutool info`。

正向汉字门禁将两段固定汉字译文从 `06-translate` 逐阶段追踪到 `09-write`，四份 IL 的
含汉字段完全一致；最终 PDF 经 Poppler 与 MuPDF 双提取均包含全部期望串。测试字体只经
公开的 `MIMUS_FONT_REGULAR/BOLD` 资产配置注入。负向矩阵另覆盖：`龘` 覆盖缺失只保留
所属段并报告字体来源与 SHA；echo 在 strict 下仍成功且 cache 重跑新增请求为 0；六种
placeholder subtype 在段级诊断与 degradation summary 的 wire JSON 中一致；28 条 identity
洪泛只截断同 ID，字体与 placeholder 诊断仍出线且 `counts_by_id` 精确；窄框长中文触发
`typeset_overflow`；65 层 Form 谱系的中层坏 Matrix 只触发页级降级且不调用后端。

聚焦矩阵使用默认自动术语：首次运行是 1 次术语提取 + 429 后 2 次段落尝试，共 3 次；
完全相同的第二次运行新增 0 次；model 与 target language 变化各新增 1 次术语提取和
1 次段落翻译；用户 glossary 变化复用自动术语，只新增 1 次段落翻译，主 server 合计
8 次（3 次术语、5 次段落）。未知 placeholder、delayed success、malformed raw body 和
disconnect 的独立 server 均为 1 次术语 + 1 次段落请求；malformed 与 disconnect 只
保留所属段并以 `translation_failure` 汇总。prompt version 没有 CLI override，段落和
术语 prompt 失效分别由 cache-key unit tests 直接钉死。

## 真实论文失效与 L5 结论

2026-08-25 对 `1706.03762v7.pdf` 的真实运行已经确认旧实现不可用：Translate 阶段 197
段有结果，其中 153 段含 7875 个汉字；Typeset 后只剩 36 个恒等段，汉字归零。直接原因是
生产路径误用仅覆盖 9 个码点的 corpus fixture 字体，161 个真实译文段全部成为
`unsupported_font`。同时 137 个短数字、符号、邮箱等原样响应被错误归为
`placeholder_violation`，逐字符 alignment 诊断又使全局 100 条预算丢弃 427 条后续诊断。

本 stack 已分别修复输出字体资产链、echo identity 语义、placeholder subtype 保真、按 ID
诊断预算、cache/strict 传播和上述离线回归矩阵。deterministic gate 仍然**只证明 fake
Responses 后端、固定 layout 输入和测试字体下的离线闭环**；真实 provider 必须由单独的
L5 验收覆盖。

2026-08-26 经用户明确授权，以 SHA-256
`bdfaa68d8984f0dc02beaca527b76f207d99b666d31d1da728ee0728182df697` 的原始
`1706.03762v7.pdf`、`gpt-5.6-luna`、Responses API、`zh-CN`、自动术语、并发 4 和
Noto Sans SC 2.004 可变字体执行 L5。授权运行命中 189 条既有真实缓存并发出 109 个真实
Responses 请求，最终得到 298 条完整缓存；后续修复验证把 HTTP(S) proxy 指向拒绝连接的
loopback，并以伪 key 重放 298/298 命中，因此没有额外公网调用。真实输出先后暴露并修复了：
共享 `TJ` operand 的 Identity/译文冲突、注入文本状态污染后续相对定位、长页跨引擎 owner
归属、4 位 PDF 数值精度累积、模型换行误判为缺字、共享 span 失败扩大为整页回退、PDFium
行末连字符 `U+0002` 等价类，以及 strict 在首个降级后过早终止。

最终 clean replay 的 L5 checklist 全绿：

- `06-translate` 为 334 个 `translated_text`、155 个含汉字段、7,881 个汉字；
  `07-typeset`、`08-font_embed`、`09-write` 及 Poppler/MuPDF 最终提取均为 6,448 个汉字，
  保留率 **81.82%**。1,433 个下降全部有 typed reason：94 个 `typeset_overflow` 段损失
  1,003、6 个 `typeset_protocol` 段损失 42、6 个 `unsupported_font` 段损失 388、1 个
  `unreliable_unicode` 段损失 0；未解释下降为 0。
- 前两页 150 dpi 渲染可见中文；`qpdf --check` 通过且仍为 15 页。输入的 2,215,244 字节
  与输出前缀 SHA-256 完全相同；113 个链接注释、16 个 XObject（10 Form/6 Image）、
  22 个页级 outline 映射和 7 个顶层书签计数不变。
- Poppler 与 MuPDF 提取均无 `{vN}`、`{lN}` 或 `<bN>` 残留；真实运行没有 placeholder
  violation。6 个 `unsupported_output_glyph` 全部可见并各自列出孤立缺字符（`∗`、`Ł`、
  `ϵ`）与字体 SHA；3 个 `page_degraded` 全部可见。唯一被预算截断的是 108 条额外
  `translation_identity` 信息诊断。
- strict 重放命中 298/298 缓存，返回 Translation/4，不发布输出，并与 normal 使用完全
  相同的 summary：页 12–14 加 107 个保留段（94 overflow、6 protocol、6 font、1
  unreliable Unicode）。全部 L5 产物按真实 key 固定字节扫描为零命中。

据约定的 80% 门槛，这篇真实论文的 M2 路径可重新声明为可用，但不是“全文无降级”：仍有
107 个段落和 3 个嵌套 Form 页面保留原文。真实 PP-DocLayoutV3 集成和生产静态双字重仍是
后续范围；本结论不替代对应 issue，也不授权合并 stack。

## Acceptance evidence

| Concern | Production-path evidence |
|---|---|
| Responses API / config | wire gate 只接受 `/v1/responses` 的 `model` + `instructions` + `input`；CLI config matrix 覆盖 flags > env > TOML、空值和 `none` 后端 |
| Placeholder / identity | 全 inventory 的 deterministic response 保持协议标记；六种 violation subtype 贯穿 typed diagnostic 与 summary；未知 `{v999}` 只保留所属段；echo 是非降级 identity，strict 接受且 cache 重跑不调用 API |
| Glossary | 自动/用户术语合并、canonical round trip、稳定 fingerprint 与请求注入由 core tests 覆盖；glossary 变化令 CLI cache miss |
| Cache | 默认自动术语下首次 miss、第二次术语与段落均命中且新增 API 调用为 0；model、target、glossary 各自按所属 key 失效；两个 prompt version 由 cache-key unit tests 覆盖；非法响应不入 cache |
| Concurrency / retry | production paragraph executor 的最大 in-flight 实测为 3（配置值 3；默认 4）；429 精确重试一次并发布有序 retry diagnostic；全 transient 分类的三次退避由可替换 sleeper 单测覆盖 |
| Degradation / strict | placeholder/backend/font coverage/fit failure 只保留所属段；嵌套 Form 失败只保留所属页；normal 发布带汇总的保留结果，strict 对真实降级返回 Translation/4，但 identity 不触发 strict |
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

本 stack 新增 ADR-0016（Responses API 与三层配置）、ADR-0017（分级降级与 strict）和
ADR-0018（输出字体资产链）。placeholder 编码、glossary canonicalization、redb key、rayon worker pool、
退避参数和实验性 table segmentation 都在这些公开边界内，属于可由同一门禁替换的
实现细节。`--translate-table` 默认关闭，因此尚不承诺为稳定政策。

门禁不验证真实公网 provider 的可用性、质量、费率限制差异或翻译质量，也不执行真实
PP-DocLayoutV3 模型；普通 CI 继续使用 deterministic detector / replay。PP-DocLayoutV3
生产集成由 #84 跟踪，统一 `assets pull` 与模型资产由既有 #39 跟踪。真实
论文 L5 复验是重新声明可用性的前置条件，不得推迟为合并后的普通运营反馈。
