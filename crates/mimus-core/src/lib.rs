//! `mimus-core` — 保留版面的 PDF 翻译内核。
//!
//! 库边界按决策 #29 划定：IL、pass 链、引擎 trait 与翻译层住在这里，CLI、
//! 进度与配置住在 `mimus` 二进制里。本里程碑（T01）只固化边界本身，功能
//! 从 M1 起逐步落地。

/// 内核版本号，`mimus` 二进制以它作为 `--version` 的真源。
///
/// 版本从 workspace 统一继承，因此库与二进制不会漂移。
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 返回内核版本号。
#[must_use]
pub fn version() -> &'static str {
    VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_the_compiled_crate_version() {
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
