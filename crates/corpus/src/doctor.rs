//! `corpus doctor` —— §2.8 独立验收工具链的统一环境检查。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use crate::proc;
use crate::toolchain::{Tool, Toolchain, extract_version};

/// 单件工具的检查结论。
#[derive(Debug, PartialEq, Eq)]
pub enum Finding {
    Ok { version: String },
    Missing,
    VersionMismatch { found: String },
    VersionUnreadable { output: String },
    MissingMarker { marker: String },
}

impl Finding {
    pub fn is_ok(&self) -> bool {
        matches!(self, Finding::Ok { .. })
    }

    fn symbol(&self) -> &'static str {
        if self.is_ok() { "ok  " } else { "FAIL" }
    }

    fn detail(&self, tool: &Tool) -> String {
        match self {
            Finding::Ok { version } => version.clone(),
            Finding::Missing => format!("未安装（期望 {}）", tool.pinned),
            Finding::VersionMismatch { found } => {
                format!("{found}，期望 {}", tool.pinned)
            }
            Finding::VersionUnreadable { output } => {
                format!("无法从输出解析版本号：{}", first_line(output))
            }
            Finding::MissingMarker { marker } => {
                format!("输出缺少必需子串 {marker:?}")
            }
        }
    }
}

fn first_line(s: &str) -> String {
    s.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .into()
}

/// 检查单件工具。
pub fn check_tool(tool: &Tool, repo_root: &Path) -> Result<Finding> {
    let Some(out) = proc::run(&tool.command, &tool.args, repo_root, &BTreeMap::new())? else {
        return Ok(Finding::Missing);
    };
    let combined = out.combined_text()?;

    for needle in &tool.must_contain {
        if !combined.contains(needle) {
            return Ok(Finding::MissingMarker {
                marker: needle.clone(),
            });
        }
    }

    let Some(found) = extract_version(&combined, tool.marker.as_deref()) else {
        return Ok(Finding::VersionUnreadable { output: combined });
    };

    if found == tool.pinned {
        Ok(Finding::Ok { version: found })
    } else {
        Ok(Finding::VersionMismatch { found })
    }
}

/// 跑一遍全表；返回 `true` 表示全部符合钉死版本。
pub fn run(toolchain: &Toolchain, repo_root: &Path) -> Result<bool> {
    let width = toolchain.tool.iter().map(|t| t.id.len()).max().unwrap_or(0);
    let mut all_ok = true;

    println!("Corpus v1 工具链检查（精确版本匹配，见 corpus/toolchain.toml）\n");
    for tool in &toolchain.tool {
        let finding = check_tool(tool, repo_root)?;
        all_ok &= finding.is_ok();
        println!(
            "  [{}] {:<width$}  {}",
            finding.symbol(),
            tool.id,
            finding.detail(tool),
        );
        if !finding.is_ok() {
            println!("       角色：{}", tool.role);
        }
    }

    println!("\n现实排版引擎的 Corpus v1 可用性（由 `corpus determinism` 裁定）：");
    for engine in &toolchain.engine {
        let mark = if engine.corpus_v1_usable {
            "可用"
        } else {
            "不可用"
        };
        println!("  {:<8} {mark:<6} {}", engine.id, engine.label);
    }

    Ok(all_ok)
}
