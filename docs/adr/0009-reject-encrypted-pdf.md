# ADR-0009 · V1 拒绝加密 PDF

- 状态：已接受（2026-08-21）
- 决策层级：可逆（是范围收窄，不是架构承诺；解除时新增一条检测分支即可）

## 背景

空 user password 的加密 PDF（防复制、防打印用途）完全合法且常见，出版商 PDF 尤甚。语料调研把它列为范围缺口（`docs/03-corpus-requirements.md` DOC-03）：BabelDOC 对 `/Encrypt` 零处理，加密文档打开后流读取抛异常、落到 `fix_null_xref` 兜底，最终"成功"输出一个空文档。

2026-08-21 对 Rust 侧能力做了查证，结论是**技术上完全可行**：lopdf 0.44.0 起原生支持"解密加载 → 增量追加 → 用原密钥加密新对象 → 恢复 trailer `/Encrypt` 引用"，覆盖 RC4-40/128、AES-128、AES-256，并有 V1/V2/V4/V5 五套 round-trip 测试；pdfium-render 各 `load_pdf_*` 统一接受 `Option<&str>` 密码。

但支持它要拖进来一串附带成本：权限位（`/P`）是否尊重及其 CLI 开关、handler 与 revision 的支持范围、输出保持加密还是降级明文、错误分类粒度、lopdf 需钉 git `main`（两个已修复未发版的补丁 #523/#479 都在加密路径上，而钉 git 依赖会挡住 crates.io 发布）、pdfium-render 的 `PdfPermissions` 对 R5/R6 直接返回 `Err` 需要 unsafe FFI 回退、以及 AES 随机 IV 与语料"确定性 SHA-256"合同的正面冲突。

这些复杂度与它在 V1 验收场景（日常翻译 arXiv 论文，arXiv 不加密）中的价值不成比例。

## 决策

1. **V1 拒绝一切加密 PDF**，无论是否需要密码、无论 handler 与 revision。退出码 2（输入不可处理），`--json` 事件的 `reason` 为单一取值；人类可读消息给出可操作建议（先用 qpdf 解密后重试）。
2. **不提供密码参数，不提供 `--ignore-permissions`**。权限位只存在于 `/Encrypt` 字典内，全拒即等于永远读不到权限位——V1 不做权限尊重，也就没有可覆盖的对象。
3. **检测点在 Parse 的文档打开处**，先于任何其他 pass。
4. **lopdf 走 crates.io 0.44.0**，不钉 git `main`：#523（V4 缺顶层 `/Length` 导致误推 40-bit 密钥）与 #479（加密文档产生未加密 ObjStm）都只在加密路径上，本决策下不可达。
5. 语料保留 **2 份加密 fixture**（M1）守护拒绝路径，见 `docs/03-corpus-requirements.md` DOC-03。

## 实现约束（务必）

**检测必须用 `Document::was_encrypted()`，不能用 `is_encrypted()`。**

lopdf 的 reader 在加载时会**无条件先试空密码**，成功后把 `/Encrypt` 对象从 `objects` 里删掉、并从 trailer 移除。因此一个空 user password 的加密文档加载完成后 `is_encrypted()` 返回 `false`、`was_encrypted()` 返回 `true`。用错的后果不是崩溃，而是**静默放行**：文档被透明解密、一路跑完流水线，甚至可能产出看起来正常的输出——而这条路径我们既不测试，其依赖库又会静默吞掉单对象解密失败（`reader.rs` 中 `let _ = encryption::decrypt_object(...)`，失败只得到垃圾内容而非 `Err`）。拿着乱码去调 LLM 是最坏的失败形态。

两条输入路径都要覆盖：需要密码的文档在 `Document::load*` 阶段直接返回 `Err(InvalidPassword)`；空密码的文档加载成功，只能靠 `was_encrypted()` 拦下。

## 后果

- 一类常见合法输入被拒。这是明知的取舍：验收场景不含它，而 qpdf 一条命令即可绕过。
- 增量写回路径不必处理 `IncrementalDocument::load()` 硬编码 `password: None` 的限制，也不需要 `create_from` 变通。
- 语料生成合同的确定性要求（§2.6）不必为 AES 的随机 IV 开例外。
- 解除本决策时，需一并复议 ADR-0006（引擎组合）中 lopdf 的版本策略，以及权限位的真源归属（结论是 lopdf 而非 pdfium-render——后者 `PdfSecurityHandlerRevision` 只有 R2/R3/R4 变体，对 AES-256 文档所有 `can_*()` 返回 `Err`）。
