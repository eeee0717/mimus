//! `corpus/toolchain.toml` 的数据模型与版本提取。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// 本模块能读懂的 `toolchain.toml` schema 版本。
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
pub struct Toolchain {
    pub schema_version: u32,
    #[serde(default)]
    pub tool: Vec<Tool>,
    #[serde(default)]
    pub engine: Vec<Engine>,
}

/// 一件被钉死版本的第三方工具。
#[derive(Debug, Deserialize)]
pub struct Tool {
    pub id: String,
    pub role: String,
    pub command: String,
    pub args: Vec<String>,
    /// 版本行的定位标记；省略时取第一个非空行。
    #[serde(default)]
    pub marker: Option<String>,
    /// 精确匹配的版本号。§2.6：版本变化视为语料变更。
    pub pinned: String,
    /// 对整段输出做的子串检查，用于钉死发行版（如 TeX Live 年份）。
    #[serde(default)]
    pub must_contain: Vec<String>,
}

/// 一个现实排版引擎及其确定性配方。
#[derive(Debug, Deserialize)]
pub struct Engine {
    pub id: String,
    pub label: String,
    /// 该引擎依赖的 `[[tool]].id`。
    pub tool: String,
    /// 是否允许用于 Corpus v1 的现实排版 fixture。
    pub corpus_v1_usable: bool,
    pub command: String,
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 产物路径模板。
    pub output: String,
    /// 确定性探针源文件（仓库相对路径）。
    pub probe: String,
    /// 需要跑几遍才收敛（LaTeX 交叉引用类）。
    #[serde(default = "one")]
    pub passes: u32,
    pub mechanism: String,
}

fn one() -> u32 {
    1
}

impl Toolchain {
    pub fn load(repo_root: &Path) -> Result<Self> {
        let path = repo_root.join("corpus/toolchain.toml");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {} 失败", path.display()))?;
        let parsed: Self =
            toml::from_str(&text).with_context(|| format!("解析 {} 失败", path.display()))?;

        if parsed.schema_version != SUPPORTED_SCHEMA_VERSION {
            bail!(
                "{} 的 schema_version = {}，本工具只支持 {SUPPORTED_SCHEMA_VERSION}",
                path.display(),
                parsed.schema_version
            );
        }

        for engine in &parsed.engine {
            if !parsed.tool.iter().any(|t| t.id == engine.tool) {
                bail!("engine `{}` 指向不存在的 tool `{}`", engine.id, engine.tool);
            }
        }

        Ok(parsed)
    }
}

/// 从命令输出中提取版本号。
///
/// 规则（与 `toolchain.toml` 顶部注释一致）：取第一行含 `marker` 的行——`marker`
/// 为 `None` 时取第一个非空行——再取该行第一个以 ASCII 数字开头的空白分隔 token，
/// 并剥掉尾部标点。刻意不用正则：这条规则要能被人读一遍就确认，而不是被调试。
pub fn extract_version(output: &str, marker: Option<&str>) -> Option<String> {
    let line = match marker {
        Some(m) => output.lines().find(|l| l.contains(m))?,
        None => output.lines().find(|l| !l.trim().is_empty())?,
    };

    line.split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))
        .map(|token| token.trim_end_matches([',', ';', ')', '.']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_from_marked_line() {
        assert_eq!(
            extract_version("qpdf version 12.4.0\nother", Some("qpdf version")).as_deref(),
            Some("12.4.0")
        );
    }

    #[test]
    fn extracts_tex_style_composite_versions() {
        let out = "pdfTeX 3.141592653-2.6-1.40.29 (TeX Live 2026)";
        assert_eq!(
            extract_version(out, Some("pdfTeX")).as_deref(),
            Some("3.141592653-2.6-1.40.29")
        );
    }

    #[test]
    fn skips_leading_words_before_the_number() {
        let out = "This is LuaHBTeX, Version 1.24.0 (TeX Live 2026)";
        assert_eq!(
            extract_version(out, Some("Version")).as_deref(),
            Some("1.24.0")
        );
    }

    #[test]
    fn trims_trailing_punctuation() {
        let out = "This is xdvipdfmx Version 20260317 by the DVIPDFMx project team,";
        assert_eq!(
            extract_version(out, Some("xdvipdfmx Version")).as_deref(),
            Some("20260317")
        );
    }

    #[test]
    fn falls_back_to_the_first_non_empty_line() {
        assert_eq!(
            extract_version("\n\n10.07.1\n", None).as_deref(),
            Some("10.07.1")
        );
    }

    #[test]
    fn reports_absence_rather_than_guessing() {
        assert_eq!(extract_version("no digits here", None), None);
        assert_eq!(extract_version("qpdf version 12.4.0", Some("mutool")), None);
    }
}
