//! SHA-256 —— §2.6 确定性门禁的唯一判据。

use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

pub fn of_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex(&hasher.finalize())
}

pub fn of_file(path: &Path) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("读取 {} 以求哈希失败", path.display()))?;
    Ok(of_bytes(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// 由 fixture ID 派生 trailer `/ID` 的 16 字节常量（§2.6）。
///
/// 规则：取 fixture ID 的 UTF-8 字节，截断或补零到 16 字节。刻意用截断而非
/// 散列——出问题时能一眼从 `/ID` 反读出是哪份 fixture，散列做不到这点。
pub fn trailer_id_bytes(fixture_id: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    let src = fixture_id.as_bytes();
    let n = src.len().min(16);
    out[..n].copy_from_slice(&src[..n]);
    out
}

/// trailer `/ID` 的 32 位 hex 形式，供 LuaTeX 的 `\pdfvariable trailerid` 使用。
pub fn trailer_id_hex(fixture_id: &str) -> String {
    hex(&trailer_id_bytes(fixture_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_the_known_empty_digest() {
        assert_eq!(
            of_bytes(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn trailer_id_pads_short_ids_with_zeros() {
        assert_eq!(trailer_id_hex("abc"), "61626300000000000000000000000000");
    }

    #[test]
    fn trailer_id_truncates_long_ids_to_sixteen_bytes() {
        let hex = trailer_id_hex("corpus-determinism-probe");
        assert_eq!(hex.len(), 32);
        assert_eq!(hex, "636f727075732d64657465726d696e69");
    }
}
