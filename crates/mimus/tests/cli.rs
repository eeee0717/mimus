//! T01 验收：CLI 的帮助与版本入口可成功运行。

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_mimus");

#[test]
fn version_flag_reports_the_core_version() {
    let out = Command::new(BIN).arg("--version").output().unwrap();

    assert!(out.status.success(), "`mimus --version` 应以 0 退出");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout.contains(mimus_core::VERSION),
        "版本输出未包含内核版本 {}：{stdout}",
        mimus_core::VERSION
    );
}

#[test]
fn help_flag_succeeds_and_names_the_binary() {
    let out = Command::new(BIN).arg("--help").output().unwrap();

    assert!(out.status.success(), "`mimus --help` 应以 0 退出");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.contains("mimus"), "帮助输出未提到二进制名：{stdout}");
}

#[test]
fn bare_invocation_prints_help_and_exits_zero() {
    let out = Command::new(BIN).output().unwrap();

    assert!(out.status.success(), "裸调用不应占用分类退出码");
    assert!(!out.stdout.is_empty(), "裸调用应打印帮助");
}
