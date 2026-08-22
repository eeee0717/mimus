//! 子进程调用的统一入口。
//!
//! 语料工具的全部 PDF 能力都来自外部的独立工具（qpdf / poppler / mutool /
//! Typst / TeX）——§2.5 禁止用 mimus 生产侧的 lopdf 或 PDFium 生成或裁定
//! 测试输入，所以这里是唯一的「干活」通道。

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output as ProcessOutput};

use anyhow::{Context, Result};

/// 一次子进程调用的结果。二进制 stdout 必须原样保留；调用方只有在明确消费文本
/// 协议时才做 UTF-8 转换。
pub struct Output {
    pub status: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    pub fn stdout_text(&self) -> Result<&str> {
        std::str::from_utf8(&self.stdout).context("子进程 stdout 不是 UTF-8")
    }

    /// 版本命令等文本接口可能把有效输出写到任一通道。
    pub fn combined_text(&self) -> Result<String> {
        let stdout = self.stdout_text()?;
        let stderr = std::str::from_utf8(&self.stderr).context("子进程 stderr 不是 UTF-8")?;
        Ok(format!("{stdout}{stderr}"))
    }

    /// 命令失败时优先显示 stderr；若工具只写 stdout，则回退到 stdout。诊断信息
    /// 不参与任何字节合同，因此允许有损展示。
    pub fn diagnostics(&self) -> String {
        let bytes = if self.stderr.is_empty() {
            &self.stdout
        } else {
            &self.stderr
        };
        String::from_utf8_lossy(bytes).into_owned()
    }
}

impl From<ProcessOutput> for Output {
    fn from(output: ProcessOutput) -> Self {
        Self {
            status: output.status.code(),
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

/// 在 `cwd` 下执行 `command`，返回原始 stdout/stderr。命令不存在不是错误，返回 `Ok(None)`
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
        Ok(out) => Ok(Some(out.into())),
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

    #[test]
    fn preserves_non_utf8_process_output_as_separate_byte_vectors() {
        let mut process_output = Command::new(std::env::current_exe().unwrap())
            .arg("--list")
            .output()
            .unwrap();
        process_output.stdout = vec![0x00, 0xff, 0x80, 0x41];
        process_output.stderr = vec![0xfe, 0x42];

        let output = Output::from(process_output);

        assert_eq!(output.stdout, [0x00, 0xff, 0x80, 0x41]);
        assert_eq!(output.stderr, [0xfe, 0x42]);
    }
}
