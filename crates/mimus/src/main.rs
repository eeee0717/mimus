//! `mimus` — 保留版面的 PDF 翻译 CLI（ADR-0001：CLI 是唯一执行接口）。
//!
//! T01 只交付外壳：确立二进制边界、`--help` / `--version` 两条入口，以及
//! 「版本号来自 `mimus-core`」这一事实。子命令（`translate` / `inspect` /
//! `assets pull`，决策 #25）随功能里程碑落地，此处刻意不预先声明——声明了
//! 就得同时定下它们的分类退出码语义，而那是 M1 的工作。

use std::io::Write;

use clap::{CommandFactory, Parser};

#[derive(Parser)]
#[command(
    name = "mimus",
    version = mimus_core::VERSION,
    about = "保留版面的 PDF 翻译 CLI",
    long_about = "保留版面的 PDF 翻译 CLI。\n\n\
                  本版本只提供工程外壳：翻译、诊断与资产子命令随功能里程碑落地。"
)]
struct Cli {}

fn main() {
    let _ = Cli::parse();

    // 没有子命令可执行时打印帮助而不是静默退出；退出码保持 0，避免在分类
    // 退出码（决策 #16）尚未定义前就占用其中一个取值。
    let mut help = Vec::new();
    let _ = write!(help, "{}", Cli::command().render_long_help());
    let _ = std::io::stdout().write_all(&help);
}
