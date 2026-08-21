# ADR-0004 · 许可：MIT 单许可

- 状态：已接受（2026-08-21）
- 决策层级：事实不可逆（接受外部贡献后变更需追溯全部贡献者）

## 背景

不复用 BabelDOC（AGPL-3.0）代码，故不受传染。依赖侧：PDFium BSD-3-Clause、ort MIT/Apache、hayro MIT/Apache、Paddle 系模型 Apache-2.0——均与 MIT 兼容。脚手架时 Cargo.toml 曾写 `MIT OR Apache-2.0`。

## 决策

MIT 单许可。`Cargo.toml` 的 `license` 字段改为 `MIT`，仓库根添加 `LICENSE`。

## 后果

- 最大化使用与集成自由；不含 Apache-2.0 式显式专利授权（决策者知情后选择）。
- 分发产物中需附带 PDFium（BSD）与模型（Apache-2.0）的许可声明文件。
