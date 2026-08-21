//! `corpus` —— Corpus v1 生成合同的可执行门禁。
//!
//! 合同正文在 `docs/03-corpus-requirements.md` §2；本二进制把其中可自动判定的
//! 条款变成命令。它刻意不依赖 `mimus-core`：§2.5 禁止用被测组件生成或裁定被测
//! 输入，全部 PDF 能力都来自 qpdf / poppler / mutool / Typst / TeX 这些独立工具。

mod determinism;
mod doctor;
mod hash;
mod proc;
mod repo;
mod toolchain;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};

use toolchain::Toolchain;

#[derive(Parser)]
#[command(
    name = "corpus",
    version,
    about = "Corpus v1 生成合同的可执行门禁",
    long_about = "Corpus v1 生成合同（docs/03-corpus-requirements.md §2）的可执行门禁。"
)]
struct Cli {
    /// 仓库根；省略时从当前目录向上查找 corpus/toolchain.toml。
    #[arg(long, global = true)]
    repo_root: Option<PathBuf>,

    /// 生成过程的临时目录；省略时用 <repo>/.context/m0-lab/work。
    #[arg(long, global = true)]
    work_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 检查 §2.8 独立验收工具链是否齐备且版本符合钉死值。
    Doctor,

    /// 对每个现实排版引擎跑 §2.6 的重复生成门禁。
    Determinism {
        /// 两次构建之间的时钟间隔（毫秒）。调小会让墙钟时间戳泄漏漏检。
        #[arg(long, default_value_t = determinism::DEFAULT_GAP.as_millis() as u64)]
        gap_ms: u64,
    },

    /// 打印某个 fixture ID 对应的 trailer /ID 常量（§2.6）。
    TrailerId {
        /// fixture ID，例如 unit-order-01-natural。
        id: String,
    },
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("corpus: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<bool> {
    let cli = Cli::parse();

    let repo_root = match &cli.repo_root {
        Some(p) => repo::find_from(p)?,
        None => repo::find()?,
    };
    let work_dir = cli
        .work_dir
        .clone()
        .unwrap_or_else(|| repo_root.join(".context/m0-lab/work"));

    match &cli.command {
        Command::Doctor => {
            let toolchain = Toolchain::load(&repo_root)?;
            doctor::run(&toolchain, &repo_root)
        }
        Command::Determinism { gap_ms } => {
            let toolchain = Toolchain::load(&repo_root)?;
            determinism::run(
                &toolchain,
                &repo_root,
                &work_dir,
                Duration::from_millis(*gap_ms),
            )
        }
        Command::TrailerId { id } => {
            let hex = hash::trailer_id_hex(id);
            println!("fixture id : {id}");
            println!("pdfTeX     : \\pdftrailerid{{{id}}}");
            println!("LuaTeX     : \\pdfvariable trailerid{{[<{hex}> <{hex}>]}}");
            Ok(true)
        }
    }
}
