//! `corpus` —— Corpus v1 生成合同的可执行门禁。
//!
//! 合同正文在 `docs/03-corpus-requirements.md` §2；本二进制把其中可自动判定的
//! 条款变成命令。它刻意不依赖 `mimus-core`：§2.5 禁止用被测组件生成或裁定被测
//! 输入，全部 PDF 能力都来自 qpdf / poppler / mutool / Typst / TeX 这些独立工具。

mod adjudicated;
mod determinism;
mod doctor;
mod exact;
mod geom;
mod hash;
mod manifest;
mod mutation;
mod oracle;
mod proc;
mod repo;
mod text;
mod toolchain;
mod verify;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result};
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

    /// 用引擎配方生成 fixture 的 PDF 并打印其 SHA-256。
    Build {
        /// fixture ID；可给多个，省略时处理全部。
        ids: Vec<String>,
        /// 把测得的 SHA-256 回填进 manifest 的 pdf_sha256。
        #[arg(long)]
        write_hash: bool,
    },

    /// 对 fixture 跑 §2.8 的独立验收。省略 ID 时验收全部。
    Verify {
        /// fixture ID；可给多个。
        ids: Vec<String>,
    },

    /// 重新裁定现实排版几何和所有 fixture 的参考栅格，写入 adjudicated.toml。
    ///
    /// 现实排版几何只有 poppler 与 mutool 在容差内一致时才会落盘（§2.1）。
    Adjudicate {
        /// fixture ID；可给多个，省略时处理全部。
        ids: Vec<String>,
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
            let engines_ok = determinism::run(
                &toolchain,
                &repo_root,
                &work_dir,
                Duration::from_millis(*gap_ms),
            )?;
            let manifests = verify::discover(&repo_root)?;
            let fixtures_ok =
                verify::run_owned_determinism(&manifests, &toolchain, &repo_root, &work_dir)?;
            Ok(engines_ok && fixtures_ok)
        }
        Command::TrailerId { id } => {
            let hex = hash::trailer_id_hex(id);
            println!("fixture id : {id}");
            println!("pdfTeX     : \\pdftrailerid{{{id}}}");
            println!("LuaTeX     : \\pdfvariable trailerid{{[<{hex}> <{hex}>]}}");
            Ok(true)
        }
        Command::Build { ids, write_hash } => {
            let toolchain = Toolchain::load(&repo_root)?;
            let manifests = select(&repo_root, ids)?;
            verify::build(&manifests, &toolchain, &repo_root, *write_hash)
        }
        Command::Verify { ids } => {
            let toolchain = Toolchain::load(&repo_root)?;
            let manifests = select(&repo_root, ids)?;
            verify::run(
                &manifests,
                &toolchain,
                &repo_root,
                &work_dir,
                verify::Mode::Verify,
            )
        }
        Command::Adjudicate { ids } => {
            let toolchain = Toolchain::load(&repo_root)?;
            let manifests = select(&repo_root, ids)?;
            verify::run(
                &manifests,
                &toolchain,
                &repo_root,
                &work_dir,
                verify::Mode::Adjudicate,
            )
        }
    }
}

/// 按 ID 过滤 fixture；空表示全部。未知 ID 一律报错，不静默跳过——
/// 打错一个字就静默变成「零份 fixture 全部通过」是最坏的一种绿。
fn select(repo_root: &Path, ids: &[String]) -> Result<Vec<manifest::Manifest>> {
    let all = verify::discover(repo_root)?;
    if ids.is_empty() {
        return Ok(all);
    }

    let mut selected = Vec::new();
    for id in ids {
        let found = all
            .iter()
            .position(|m| m.id() == id)
            .with_context(|| format!("corpus/fixtures/ 下没有 fixture `{id}`"))?;
        selected.push(found);
    }
    selected.sort_unstable();
    selected.dedup();

    Ok(all
        .into_iter()
        .enumerate()
        .filter(|(i, _)| selected.contains(i))
        .map(|(_, m)| m)
        .collect())
}
