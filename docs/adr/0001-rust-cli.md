# ADR-0001 · 实现语言 Rust，交付形态 CLI

- 状态：已接受（2026-08-21）
- 决策层级：不可逆（全部代码建立其上）

## 背景

调研（`docs/01-research.md` §4）表明选型关键不在"哪个语言能跑模型"，而在 PDF 读写与字体生态。Rust 在 PDF 写入 + 字体子集（krilla / subsetter / pdf-writer）、ONNX（ort）、单产物分发三项上综合最优；参考实现 BabelDOC（Python）已存在，重做 Python 无差异化。

## 决策

用 Rust 实现；V1 的唯一执行接口为 CLI，不做 GUI。仓库提供可通过 `npx skills add eeee0717/mimus` 安装的 Agent Skill，它只编排 CLI，不构成第二套实现（ADR-0008）；后续如做 GUI、MCP 或其他入口，仍以 CLI/库为内核另行封装。

## 后果

- 畸形 PDF 长尾在 Rust 中表现为显式 `Result` 处理：前期更慢、后期更稳。
- 原生依赖（如 PDFium）崩溃是进程 abort 而非可捕获异常，隔离策略需单独决策。
- 工具链已由 mise 固定（见仓库 `mise.toml`）。
