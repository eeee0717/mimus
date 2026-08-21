//! qpdf `--check` —— §2.8 步骤 2 的合法性核验。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::proc;

/// 结构检查的结论。
pub struct CheckResult {
    pub passed: bool,
    pub report: String,
}

/// 对一份 PDF 跑 `qpdf --check`。
///
/// 注意 qpdf 的退出码分三档：0 无问题、2 有错误、3 只有警告。**警告也算不通过**
/// ——合法 fixture 的判据是「干净」，容忍警告等于给自己留一条以后会被引用成
/// 「本来就这样」的后路。
pub fn check(pdf: &Path) -> Result<CheckResult> {
    let args = vec!["--check".to_string(), pdf.display().to_string()];
    let out = proc::run("qpdf", &args, Path::new("."), &BTreeMap::new())?.context("qpdf 未安装")?;
    Ok(CheckResult {
        passed: out.status == Some(0),
        report: out.combined,
    })
}
