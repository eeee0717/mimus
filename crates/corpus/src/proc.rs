//! 子进程调用的统一入口。
//!
//! 语料工具的全部 PDF 能力都来自外部的独立工具（qpdf / poppler / mutool /
//! Typst / TeX）——§2.5 禁止用 mimus 生产侧的 lopdf 或 PDFium 生成或裁定
//! 测试输入，所以这里是唯一的「干活」通道。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// 一次子进程调用的结果。stdout 与 stderr 合并——版本号在两者之间的分布因工具
/// 而异（poppler 走 stderr、qpdf 走 stdout），分开处理只会让配置多一个字段。
pub struct Output {
    pub status: Option<i32>,
    pub combined: String,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

/// 在 `cwd` 下执行 `command`，返回合并输出。命令不存在不是错误，返回 `Ok(None)`
/// ——「工具没装」是 doctor 要报告的正常结论，不是工具本身的故障。
pub fn run(
    command: &str,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<Output>> {
    let mut cmd = Command::new(command);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }

    match cmd.output() {
        Ok(out) => {
            let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
            combined.push_str(&String::from_utf8_lossy(&out.stderr));
            Ok(Some(Output {
                status: out.status.code(),
                combined,
            }))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("执行 `{command}` 失败")),
    }
}

/// 把 `{key}` 占位符替换成实际值。
pub fn expand(template: &str, vars: &BTreeMap<&str, String>) -> String {
    vars.iter().fold(template.to_string(), |acc, (k, v)| {
        acc.replace(&format!("{{{k}}}"), v)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_every_occurrence() {
        let vars = BTreeMap::from([("outdir", "/tmp/x".to_string()), ("stem", "p".to_string())]);
        assert_eq!(expand("{outdir}/{stem}.pdf", &vars), "/tmp/x/p.pdf");
        assert_eq!(expand("{stem}-{stem}", &vars), "p-p");
    }

    #[test]
    fn leaves_unknown_placeholders_alone() {
        let vars = BTreeMap::from([("a", "1".to_string())]);
        assert_eq!(expand("{a}/{b}", &vars), "1/{b}");
    }
}
