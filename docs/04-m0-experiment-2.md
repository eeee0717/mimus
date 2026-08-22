# M0 实验 2：操作符走查与 PDFium 对齐

> 日期：2026-08-22
> 对应：ADR-0006、Issue #8 / #9 / #12
> 结论：**成**。ADR-0006 的「lopdf 原始字节 + 自写走查 + PDFium 交叉校验」组合可行，无需复议。

## 1. 范围与实现

PoC 位于非生产 crate `crates/m0-experiment-2`，不依赖 `mimus-core`，不提供生产
`PdfInspector` 或 parser API。它只实现本实验 fixture 需要的最小闭包：

- lopdf 读取每个 content stream 的原始字节并有界解码（单流上限 16 MiB）；多
  `/Contents` 在流边界保留 tokenizer 边界，同时让普通操作数跨边界延续；
- 自写 tokenizer：number / name / literal string / hex string / array / operator；全局
  嵌套上限 128；inline image 依次采用 `/L`、逐 scanline 向上取整的可计算长度、
  最大 16 MiB 的受限 `EI` 扫描，并记录所选路径；所有尺寸转换和乘法均检查溢出；
- 操作数栈、`q/Q`、`cm`、`BT/ET`、`Tf/Tm/Td/Tc/Tw/Tz/Ts/Tj`；每个 operator
  边界清空栈，多余操作数取尾部所需 arity 并告警，缺操作数原子跳过；`BX/EX`
  内未知 operator 静默保留，区间外未知 operator 报告 warning；
- 简单字体显式 `/Widths`、Type3 CharProc `d0/d1` metric trace、Identity-H 的
  CID -> GID -> embedded TrueType cmap、字符 advance；
- Form XObject 资源作用域、继承、逐层 `/Matrix`、间接对象 ID 去环；Form 深度上限
  64，缺 `/BBox` 原子跳过；Form 与 Type3 进入统一隔离 scope，退出时完整恢复调用方
  graphics state/stack、operand、resources、compatibility depth 与 Type3 context；
- 未知 operator 的原始 bytes 保留在 JSON trace；PoC 不改写 PDF。

公开复现入口一次处理一份 fixture，避免把 PDFium 的进程级 binding 状态藏进协议：

```sh
mise exec -- cargo run -p m0-experiment-2 -- \
  unit-xobj-04-inherited-resources \
  --repo-root . \
  --pdfium-library /path/to/libpdfium.dylib
```

## 2. 环境与钉死值

| 项 | 实测值 |
|---|---|
| OS | macOS 26.5.1 (25F80), arm64 |
| rustc / cargo | 1.98.0 / 1.98.0 |
| lopdf | 0.44.0，关闭无关的默认 date/rayon feature |
| pdfium-render | 0.9.1，默认 PDFium API 7763 |
| PDFium dylib | arm64；SHA-256 `cb8e259f914dda33f8930751e9a70afd3168893a569f7e59d34d29c4bc5701c3` |
| CI PDFium | chromium/8009 linux-x64；SHA-256 `be513e8021a5bf8eb2116e00d78c3bacb82c5a02b3785156ae14fe5e33084385` |
| ttf-parser | 0.25.1 |
| qpdf / poppler / mutool | 12.4.0 / 26.08.0 / 1.28.2 |

本机 dylib 来自另一个已钉死 `pdfium-render 0.9.1` 的项目。用 0.9.3 的 API 7881
binding 加载同一 dylib 会在 `FPDFTextObj_SetFontSize` 缺 symbol，故 PoC 按 dylib 的
原配 wrapper 固定 0.9.1；这不是放宽 API，而是修正动态库与头文件版本配对。

## 3. Fixture 与哈希

以下均先由 hand-written manifest 固定行为，再生成 PDF；哈希是机械回填的生成结果。

| fixture | SHA-256 |
|---|---|
| `unit-parse-01-ascii85` | `72aa0cebf2dbd9e3b941e7fbd918f8f879ceadb6160081bc5c2327492ee7e67a` |
| `unit-parse-02-cascade` | `5d11788f041cd64cb92e31fbdfffc08ba16f1c1abcea869b2f84677e35731583` |
| `unit-parse-03-lzw-earlychange` | `0749e7845bee09a6f5cf91557badd81c92c36dccd9a903db20a28ce539867cf7` |
| `unit-parse-03-lzw-earlychange-1` | `3b764233aa3a5970f40f10b59292717f80f66b7bea96818090407794ce7dd72c` |
| `unit-parse-04-contents-array-numeric-split` | `e5ab55e8b9f59197767306e3b9d1e6dd94afed692518e2c9b262da973f41f315` |
| `mal-parse-05-contents-array-string-split` | `7060aebcef707fda0c97c9592af8d44cd8654d582e3cbefaf0b5086d3d979e3c` |
| `mal-parse-06-deep-nesting` | `767b0e74f443d2900a73f68990c676c12f7fc5dabfbbebf82b50b14998f22a1a` |
| `unit-stream-01-bx-ex-unknown-op` | `054b8d5ee50134d620d0f722bf10321b3e942419c5abb17bb8114c2f0ba266b2` |
| `unit-stream-02-type3-d1` | `556525e1ce22d1740e3d40521465fb04810201d1385056f6ffb01589c7b1b937` |
| `unit-stream-03-unknown-op-outside-bx` | `71820ebfc7149d4b601bfa71601c3e0243b90966fdb49dc3cd7b0717f366f2d6` |
| `unit-stream-04-type3-d0` | `7143ded599e91297182e2440f3c56bce2477097744e607492ccea486ef5a30b1` |
| `mal-stream-03-arity-excess` | `304bbe9b2fa1650f0cf304e55613d7a92178282e816d19ba81148a0259cea2fe` |
| `mal-stream-04-arity-short` | `430f11ef130317c1dfd5b5df8758cb9527c9ce55709471236ae26f9e9d8e2c5a` |
| `mal-stream-05-unbalanced-Q` | `02a5d8872b72940d3e8b496ee6b7605c12f09d10fb3f7f9e0473573572697a30` |
| `mal-stream-06-glued-tokens` | `4e2b7b0c14690bd3261266281551b3053c0fa2abd5b12ec22b792637da2915d3` |
| `mal-stream-07-double-decimal` | `96f4efaf6ade92db0ed9e04aac19670303ed75ee153a44799e84b392112c036f` |
| `mal-stream-08-unknown-outside-bx` | `32d9284cd92fe5408cb39c9ef76b446b538c042284807469222a0777a4b53f4e` |
| `unit-stream-08-inline-image-EI-in-data` | `b207a2a4d8a84ab00bf98103df99787e37ef820f12dcf51565c30aa383ecb52a` |
| `unit-stream-09-inline-image-no-L` | `50f0a00d7da8e80bb9d48f81eb80e16c9cb7673cb39d1dcf6c246f182525df7b` |
| `unit-stream-10-inline-image-length` | `835f04d66b7c25a3d880120c0628f32cae74a5e3d1a69abb02cecc3c304f1f67` |
| `unit-stream-11-inline-image-filtered-fallback` | `11be9ad0041528b94c492f669fecd5385fff4182de05f72def24cc220edd304b` |
| `unit-font-01-std14-custom-widths` | `6bbf13245639f1c8d3b88157025af4f5ec9fb458893aa3db5535ec67545180fe` |
| `unit-cmap-01-identity-no-tounicode` | `5eaab3099f1fbd89f7d11b771982c8a4a34da934354c9ffd83bcd8c73a0ae6ca` |
| `unit-xobj-00-recursion-parent` | `4a0499d5156713802a7e805053b1b835d933a12a4541bcccbd91fc61b8f7f90e` |
| `mal-xobj-01-self-recursive` | `0cafd3df5b057b7753866a417545c6377b3c771cbea279f4462a33100727d666` |
| `mal-xobj-02-mutual-recursive` | `fa72bc9b8180299aec918df75130729a1487bca304d96aff352a239c04e559a3` |
| `mal-xobj-03-form-no-bbox` | `8c8254b5fab2fb4d51e62008a282858e9bf1a5c4647c7b9d68c02ce4c92b9d5c` |
| `mal-xobj-04-scope-underflow` | `73e8a378888362b889da7157d25932d7b38d8cadeb0df9ef1983602aecfa4fce` |
| `mal-xobj-05-scope-tail` | `2eae4c1d591cb35b05e558cb214f1134f3abcf57f7ada81b3a087c6b6a2782f7` |
| `unit-xobj-04-inherited-resources` | `eefb758e581961ee64ea29a17c03c0c7cdfbcbb1fbc4b02a4f6e2e92e82f4439` |
| `unit-xobj-05-scope-parent` | `f061f834fe245742c4dd62620e9023b3b3f10336b96692d5149aa8e0a535220c` |

`unit-base-01-single-line` 是对齐参照，不计入 #8/#9 新增清单。

## 4. 容差与合法 fixture 结果

容差没有由运行结果反推：baseline / metric box 沿用 manifest 的 `0.001 pt`，visual
bbox 沿用 `0.01 pt`。下表的 `PDFium max |delta|` 是 PDFium 字符 origin 与 PoC
baseline 的逐分量最大绝对差；独立列表示该 fixture 已通过 qpdf + MuPDF/Poppler +
独立 raster 的 `corpus verify`。

| fixture | 走查 text / CID | 首 baseline | PDFium text | PDFium max \|delta\| | 独立 |
|---|---|---:|---|---:|---|
| `unit-base-01-single-line` | `MIMUS` / `77,73,77,85,83` | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-parse-01-ascii85` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-parse-02-cascade` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-parse-03-lzw-earlychange` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-parse-03-lzw-earlychange-1` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `< 0.001` | pass |
| `unit-parse-04-contents-array-numeric-split` | `MIMUS` / 同上 | `(82,140)` | `MIMUS` | `4.52e-6` | pass |
| `unit-stream-01-bx-ex-unknown-op` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-stream-02-type3-d1` | `M` / `77` | `(72,120)` | `M` | `0` | pass |
| `unit-stream-03-unknown-op-outside-bx` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `< 0.001` | pass |
| `unit-stream-04-type3-d0` | `M` / `77` | `(72,120)` | `M` | `< 0.001` | pass |
| `unit-stream-08-inline-image-EI-in-data` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-stream-09-inline-image-no-L` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-stream-10-inline-image-length` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `< 0.001` | pass |
| `unit-stream-11-inline-image-filtered-fallback` | `MIMUS` / 同上 | `(72,120)` | `MIMUS` | `< 0.001` | pass |
| `unit-font-01-std14-custom-widths` | `AAAA` / `65,65,65,65` | `(72,120)` | `AAAA` | `0` | pass |
| `unit-cmap-01-identity-no-tounicode` | `MIMUS` / `7,6,7,11,9` | `(72,120)` | 空 | n/a | pass |
| `unit-xobj-00-recursion-parent` | `MIMUS` / `77,73,77,85,83` | `(72,120)` | `MIMUS` | `4.52e-6` | pass |
| `unit-xobj-04-inherited-resources` | `IIIIIIH` / `73,73,73,73,73,73,72` | `(110,176)` | `IIIIIIH` | `< 0.001` | pass |
| `unit-xobj-05-scope-parent` | `MIMUS` / `77,73,77,85,83` | `(72,120)` | `MIMUS` | `< 0.001` | pass |

MuPDF `stext` 的 raw 坐标是左上原点；页面高 200，转换为 manifest page space 时
`y_page = 200 - y_raw`。代表性原始值：base `(72,80)` -> `(72,120)`、numeric split
`(82,60)` -> `(82,140)`、nested Form `(110,24)` -> `(110,176)`。Type3、FONT-02、
CMAP-04 的 raw origin 均为 `(72,80)`。

嵌套 Form 的完整 JSON trace 中，glyph `M` 为：

```json
{
  "unicode": "M",
  "cid": 77,
  "baseline": [110.0, 176.0],
  "text_matrix": [1.0, 0.0, 0.0, 1.0, 72.0, 120.0],
  "ctm": [1.0, 0.0, 0.0, 1.0, 38.0, 56.0]
}
```

CTM 的 `(38,56)` 可逐层还原为 page `cm(10,15)` + outer Matrix `(20,30)` + outer
`cm(3,4)` + inner Matrix `(5,7)`；加 text matrix `(72,120)` 得 `(110,176)`。PDFium
五字符 origin delta 为 `0, 2.81e-6, -3.91e-6, -1.10e-6, -4.52e-6 pt`（y 全 0）。

Raw token 保留的代表性断言：Type3 CharProc 为
`1000 0 0 0 1000 1000 d1`，且 `d0/d1` 都将 width 写入 glyph metric trace；
`SomeVendorOp` raw hex 为 `536f6d6556656e646f724f70`；两行 9px、1bpp inline image
按每行 2 byte 补齐为 4-byte payload，其中 `20 45 49 20` 没有被误判为终止符。

## 5. 故障注入与有界失败

| fixture | 实测稳定诊断 | 其他可观察结果 |
|---|---|---|
| `mal-parse-05-contents-array-string-split` | `unterminated-string` | 在 object 9 停页，不读 object 10，不产生级联错误 |
| `mal-parse-06-deep-nesting` | `nesting-too-deep-128` | 第 129 层停止，无 panic / stack overflow |
| `mal-stream-03-arity-excess` | `arity-excess` | 尾六个数形成 CTM，baseline `(630,823)`；前缀在 operator 边界丢弃 |
| `mal-stream-04-arity-short` | `arity-short` | `cm` 原子跳过，baseline 仍 `(72,120)` |
| `mal-stream-05-unbalanced-Q` | `graphics-stack-underflow` x2 | base CTM 不变，baseline `(72,120)` |
| `mal-stream-06-glued-tokens` | `glued-token-recovery` x2 | `12,Tf` / `120,Td`，baseline `(100,120)` |
| `mal-stream-07-double-decimal` | `double-decimal` + `arity-excess` | 固定拆为 `10.5` / `.3`，`Tc=.3` |
| `mal-stream-08-unknown-outside-bx` | `unknown-operator` + `compatibility-underflow` | `BX/EX` 外未知 operator 报告，额外 `EX` 有界失败 |
| `mal-xobj-01-self-recursive` | `recursive-form-self` | path `[11,11]`，进入深度 1 后停止 |
| `mal-xobj-02-mutual-recursive` | `recursive-form-mutual` | path `[12,13,12]`，进入深度 2 后停止 |
| `mal-xobj-03-form-no-bbox` | `form-missing-bbox` | object 14 不执行，既无 `(72,120)` 也无 `(154,260)` glyph |
| `mal-xobj-04-scope-underflow` | `graphics-stack-underflow` | 内层多余 `Q` 不弹出调用方状态，后续 baseline 仍为 `(72,120)` |
| `mal-xobj-05-scope-tail` | `scoped-graphics-stack-unbalanced` + `scoped-operands-discarded` | 子 scope 尾随 stack/operand 被报告并丢弃，不泄漏到页面 |

全部诊断都与 manifest 的 `operator-walk:<id>` 精确匹配；测试进程正常返回。Form
对象 ID active-path 去环是第一道边界，深度 64 是第二道；tokenizer 嵌套 128 与 Form
深度是两个独立限制。

## 6. 分歧裁定

### FONT-02

文件 `/Widths [1000 ...]` 优先于 Standard-14 内置度量。PoC、MuPDF 与 PDFium 都得到
`A` origins `72,84,96,108`，而不是 Helvetica 内置宽度导出的 `72,80.004,...`。
内置度量只在文件没有对应 width 时兜底。

### CMAP-04

PDFium text page 对 Identity-H 且无 `/ToUnicode` 返回空 Unicode；它不能成为此 case 的
Unicode oracle。PoC 从字符码得到 CID `[7,6,7,11,9]`，经 `/CIDToGIDMap /Identity`
得到同一 GID 序列，再反查 pinned TTF cmap 得 `MIMUS`；ttf-parser 与 HarfBuzz 已在
fixture 裁定阶段独立确认 GID。绝不把 CID 当 Unicode 码点。

### STREAM-02

多余操作数不跨 operator 继承：当前 operator 取尾部 arity、记录 warning，operator
结束清空全部操作数。缺操作数时 operator 原子跳过，不局部修改 CTM。这样不会把一个
错误扩散到页面后续所有 operator。

### Type3 `d0/d1`

规范的 `d1(1000,0,0,0,1000,1000)` 与独立 painted geometry 给出 12pt advance 和
`[72,120,84,132]` 方盒。Poppler 合成的 metric bbox 只留作 differential evidence，
不覆盖 `d1` 几何；`d0(1000,0)` 同样覆盖 glyph advance，但不声明 cache bbox，painted
geometry 仍由 CharProc 路径独立得到。两者的 PDFium 字符 origin 都与走查一致。

### LZW `/EarlyChange`

`EarlyChange=0` 与 `EarlyChange=1` 使用同一份 384-byte decoded stream，均跨过 9 -> 10 bit
码宽边界；两份 raw stream 字节不同，解码结果逐字节相同，qpdf 与 PoC 都通过。因而忽略
`/EarlyChange` 的 decoder 无法再靠短样本通过。

### Compatibility 与执行 scope

`BX/EX` compatibility depth 决定未知 operator 是否告警。Form 与 Type3 CharProc 进入隔离
scope 后，调用方的 graphics state/stack、operand、resources 和 compatibility depth 都按值
恢复；子 scope 的 underflow、残留 stack 与残留 operand 只产生声明过的诊断，不污染页面。

### Inline image

`/L` 声明长度优先；无 filter 且尺寸可计算时，按 `ceil(W * BPC * components / 8) * H`
逐 scanline 计算；filtered 或尺寸不可计算时才做最大 16 MiB 的 guarded `EI` 扫描并报告
`inline-image-ei-scan` warning。三条路径分别由 fixture 钉死，payload 内的 ` EI ` 不会被
错误截断。

## 7. 全局仲裁规则

发生走查、PDFium、Poppler 或 MuPDF 分歧时，统一按以下顺序裁定，不交给下游逐次选择：

1. **PDF 规范 + hand-written manifest + 与生成器独立的结构/字体/几何推导**是事实层；
2. **至少一个独立 parser/renderer trace**验证该推导是否落在实际 PDF 上；
3. **PDFium 是度量与光栅的交叉证据，不是唯一 oracle**。与前两层一致则提高置信度；
   不一致则以稳定 fixture 记录 engine differential，不反向修改 manifest；
4. 规范本身允许多种恢复时，mimus 选择「不扩散错误、可报告、可有界停止」的策略；
   无法可靠恢复则跳过当前单元并触发既定降级，绝不猜 Unicode 或静默改坐标。

这条规则同时裁定 FONT-02、CMAP-04、STREAM-02、Type3 和 inline image，不再为每个
新分歧临时改变权威顺序。

## 8. 复现命令与结论

```sh
MIMUS_PDFIUM_LIBRARY=/path/to/libpdfium.dylib \
  mise exec -- cargo test -p m0-experiment-2 --all-targets
mise exec -- cargo run -p corpus -- doctor
mise exec -- cargo run -p corpus -- determinism
mise exec -- cargo run -p corpus -- verify
```

PDFium dylib 是实验测试的强制输入：环境变量缺失或路径不存在时测试明确失败；CI 下载
固定 archive 并校验 SHA-256 后运行全 workspace 测试，不再允许静默 skip。实验测试为
7/7，其中 PDFium 强制交叉校验覆盖 19 份合法 fixture；Corpus v1 为 60/60 fixture 通过
独立验收，且每份 `operator-walk:*` check 都实际执行 PoC 并精确比较诊断集合。

结论为**成**：自写走查能
在规范输入上达到 `0.001 pt` 合同，PDFium 可继续位于 ADR-0006 的 trait 边界后做
度量/光栅与交叉校验。唯一已证实的 PDFium 文本缺口是 CMAP-04，生产实现必须保留
embedded-font cmap 路径。若未来 PDFium 在更多规范 case 上持续偏离，最小替代方案是
保持 lopdf + operator walk 不变，只替换 `PdfInspector` / `Rasterizer` 后端；当前没有
触发 ADR-0006 复议。
