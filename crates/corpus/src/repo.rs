//! 仓库根定位。
//!
//! 所有配方（`toolchain.toml` 的 `[[engine]].args`）里的路径都是**仓库相对**的，
//! 子进程的工作目录也固定为仓库根——否则同一条配方在不同 CWD 下会展开成不同的
//! 命令行，确定性门禁就失去意义。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// 仓库根的判据：存在 `corpus/toolchain.toml`。
const MARKER: &str = "corpus/toolchain.toml";

/// 从 `start` 向上查找仓库根。
pub fn find_from(start: &Path) -> Result<PathBuf> {
    let start = start
        .canonicalize()
        .with_context(|| format!("无法规范化起始目录 {}", start.display()))?;

    for dir in start.ancestors() {
        if dir.join(MARKER).is_file() {
            return Ok(dir.to_path_buf());
        }
    }

    bail!(
        "从 {} 向上找不到仓库根（判据：存在 {MARKER}）",
        start.display()
    )
}

/// 从当前工作目录查找仓库根。
pub fn find() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("读取当前工作目录失败")?;
    find_from(&cwd)
}
