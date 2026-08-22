//! `corpus determinism` —— §2.6 的重复生成门禁。
//!
//! 「同一输入重复生成必须得到相同的 SHA-256」。这里有一个容易把门禁做成摆设的
//! 细节：连续两次构建常常落在同一秒里，写墙钟时间戳的引擎会**碰巧**通过。因此
//! 两次构建之间强制插入一段时钟间隔，让时间戳泄漏必然显形。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::hash;
use crate::proc;
use crate::toolchain::{Engine, Toolchain};

/// 两次构建之间的默认时钟间隔。取 1.2 s 是因为 PDF 日期的最小分辨率是 1 s，
/// 跨过一整秒即可让墙钟泄漏稳定显形。
pub const DEFAULT_GAP: Duration = Duration::from_millis(1200);

/// 一个引擎的门禁结论。
#[derive(Debug)]
pub enum Verdict {
    /// 两次构建字节一致。
    Reproducible { sha: String },
    /// 两次构建不一致。
    Divergent { first: String, second: String },
    /// 构建本身失败。
    BuildFailed { detail: String },
}

impl Verdict {
    fn reproducible(&self) -> bool {
        matches!(self, Verdict::Reproducible { .. })
    }
}

/// 用引擎配方构建指定源文件（仓库相对路径），返回产物路径。
pub fn build_source(
    engine: &Engine,
    repo_root: &Path,
    source: &str,
    outdir: &Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(outdir)
        .with_context(|| format!("创建输出目录 {} 失败", outdir.display()))?;

    let source_path = repo_root.join(source);
    if !source_path.is_file() {
        bail!("引擎 `{}` 的源文件不存在：{source}", engine.id);
    }
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .with_context(|| format!("源文件名无法作为主名使用：{source}"))?
        .to_string();

    let mut vars: BTreeMap<&str, String> = BTreeMap::from([
        ("source", source.to_string()),
        ("outdir", outdir.display().to_string()),
        ("stem", stem),
    ]);
    let output = proc::expand(&engine.output, &vars);
    vars.insert("output", output.clone());

    let args: Vec<String> = engine.args.iter().map(|a| proc::expand(a, &vars)).collect();

    for pass in 1..=engine.passes {
        let Some(out) = proc::run(&engine.command, &args, repo_root, &engine.env)? else {
            bail!("引擎 `{}` 的命令 `{}` 未安装", engine.id, engine.command);
        };
        if !out.success() {
            bail!(
                "引擎 `{}` 第 {pass} 遍构建失败（退出码 {:?}）：\n{}",
                engine.id,
                out.status,
                tail(&out.diagnostics(), 20)
            );
        }
    }

    let output = PathBuf::from(output);
    if !output.is_file() {
        bail!(
            "引擎 `{}` 声称成功但没有产出 {}",
            engine.id,
            output.display()
        );
    }
    Ok(output)
}

/// 对一个引擎跑重复生成门禁。
pub fn probe(engine: &Engine, repo_root: &Path, work_dir: &Path, gap: Duration) -> Verdict {
    let run = |slot: &str| -> Result<String> {
        let outdir = work_dir.join(&engine.id).join(slot);
        // 每一轮都从空目录开始：LaTeX 会复用上一轮的 .aux，留着就等于让第二次
        // 构建走了和第一次不同的代码路径，门禁会得出假阴性。
        if outdir.exists() {
            std::fs::remove_dir_all(&outdir)
                .with_context(|| format!("清理 {} 失败", outdir.display()))?;
        }
        let pdf = build_source(engine, repo_root, &engine.probe, &outdir)?;
        hash::of_file(&pdf)
    };

    let first = match run("run-a") {
        Ok(sha) => sha,
        Err(e) => {
            return Verdict::BuildFailed {
                detail: format!("{e:#}"),
            };
        }
    };

    std::thread::sleep(gap);

    let second = match run("run-b") {
        Ok(sha) => sha,
        Err(e) => {
            return Verdict::BuildFailed {
                detail: format!("{e:#}"),
            };
        }
    };

    if first == second {
        Verdict::Reproducible { sha: first }
    } else {
        Verdict::Divergent { first, second }
    }
}

/// 跑全表；返回 `true` 表示所有标为可用的引擎都通过了门禁。
pub fn run(
    toolchain: &Toolchain,
    repo_root: &Path,
    work_dir: &Path,
    gap: Duration,
) -> Result<bool> {
    println!(
        "Corpus v1 确定性门禁：每个引擎用固定输入连续构建两次，\
         两次之间插入 {} ms 时钟间隔。\n",
        gap.as_millis()
    );

    let mut all_ok = true;
    for engine in &toolchain.engine {
        let verdict = probe(engine, repo_root, work_dir, gap);
        let reproducible = verdict.reproducible();

        match (&verdict, engine.corpus_v1_usable) {
            (Verdict::Reproducible { sha }, true) => {
                println!("  [ok  ] {:<8} 可用于 Corpus v1；SHA-256 {sha}", engine.id);
            }
            (Verdict::Reproducible { sha }, false) => {
                println!(
                    "  [note] {:<8} 标为不可用，但本次复现成功（SHA-256 {sha}）。\n\
                     \x20        这条 pin 可以复议——先确认不是碰巧，再改 toolchain.toml。",
                    engine.id
                );
            }
            (Verdict::Divergent { first, second }, true) => {
                all_ok = false;
                println!(
                    "  [FAIL] {:<8} 标为可用却不可复现：\n           {first}\n           {second}",
                    engine.id
                );
            }
            (Verdict::Divergent { .. }, false) => {
                println!(
                    "  [ok  ] {:<8} 不可用于 Corpus v1，且如预期般不可复现（门禁灵敏度自检通过）",
                    engine.id
                );
            }
            // 构建失败一律是硬失败，跟可用性无关：探针跑不起来就等于没测。
            (Verdict::BuildFailed { detail }, _) => {
                all_ok = false;
                println!("  [FAIL] {:<8} 构建失败：{detail}", engine.id);
            }
        }

        if !reproducible && !engine.corpus_v1_usable {
            for line in engine.mechanism.trim().lines() {
                println!("         {line}");
            }
        }
    }

    Ok(all_ok)
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail("a\nb\nc", 2), "b\nc");
        assert_eq!(tail("a", 5), "a");
    }

    #[test]
    fn only_identical_hashes_count_as_reproducible() {
        assert!(Verdict::Reproducible { sha: "x".into() }.reproducible());
        assert!(
            !Verdict::Divergent {
                first: "x".into(),
                second: "y".into()
            }
            .reproducible()
        );
        assert!(
            !Verdict::BuildFailed {
                detail: String::new()
            }
            .reproducible()
        );
    }
}
