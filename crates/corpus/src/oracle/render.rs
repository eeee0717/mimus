//! 独立渲染（§2.8 步骤 4）——两个互相独立的渲染器各出一份逐页栅格哈希。
//!
//! 用两个而不是一个，是因为栅格哈希还要承担一条跨 fixture 的断言：ORDER-01 的
//! 三份顺序变体必须**逐像素一致**。单一渲染器的一致只能证明「这个渲染器忽略了
//! 绘制顺序」，两个才排得掉这种可能。

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::hash;
use crate::proc;

/// 参考栅格的分辨率。72 dpi 是 PDF 的自然单位（1 px = 1 pt），
/// 但对小字号太粗；150 dpi 在灵敏度与产物体积之间取中。
pub const DPI: u32 = 150;

/// 一页在两个渲染器下的哈希。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRaster {
    pub index: usize,
    /// poppler `pdftoppm` 输出的 PNG 的 SHA-256。
    pub poppler_sha256: String,
    /// mutool 自报的渲染位图 MD5（`mutool draw -s 5`）。
    pub mutool_md5: String,
    /// poppler 出图的像素宽高。渲染器**会**应用 /Rotate，因此这一对数字是
    /// 观看空间朝向的独立证据（解析器那边报的 <page width height> 不是）。
    pub pixels: (u32, u32),
}

/// 渲染一份 PDF 的全部页面并求哈希。
pub fn rasterize(pdf: &Path, work_dir: &Path, pages: usize) -> Result<Vec<PageRaster>> {
    let poppler = poppler_pages(pdf, work_dir, pages)?;
    let mutool = mutool_hashes(pdf)?;

    if poppler.len() != pages || mutool.len() != pages {
        bail!(
            "渲染页数不符：manifest {pages} 页，poppler {} 页，mutool {} 页",
            poppler.len(),
            mutool.len()
        );
    }

    Ok((0..pages)
        .map(|i| PageRaster {
            index: i,
            poppler_sha256: poppler[i].0.clone(),
            mutool_md5: mutool[i].clone(),
            pixels: poppler[i].1,
        })
        .collect())
}

fn poppler_pages(pdf: &Path, work_dir: &Path, pages: usize) -> Result<Vec<(String, (u32, u32))>> {
    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("创建渲染目录 {} 失败", work_dir.display()))?;
    let prefix = work_dir.join("page");

    let args: Vec<String> = vec![
        "-r".into(),
        DPI.to_string(),
        "-gray".into(),
        "-png".into(),
        pdf.display().to_string(),
        prefix.display().to_string(),
    ];
    let out = proc::run("pdftoppm", &args, Path::new("."), &BTreeMap::new())?
        .context("pdftoppm 未安装")?;
    if !out.success() {
        bail!("pdftoppm 失败：{}", out.diagnostics());
    }

    // pdftoppm 的页码位数随总页数变化（1..9 → `-1`，10..99 → `-01`）。
    let width = pages.to_string().len();
    (1..=pages)
        .map(|n| {
            let path = work_dir.join(format!("page-{n:0width$}.png"));
            Ok((hash::of_file(&path)?, png_size(&path)?))
        })
        .collect()
}

/// 从 PNG 的 IHDR 读出宽高。
///
/// 只读文件头 24 字节，不引入图像解码依赖——这里要的只是「出图是横的还是
/// 竖的」这一个事实。
fn png_size(path: &Path) -> Result<(u32, u32)> {
    let head = {
        use std::io::Read;
        let mut buf = [0u8; 24];
        let mut f =
            std::fs::File::open(path).with_context(|| format!("打开 {} 失败", path.display()))?;
        f.read_exact(&mut buf)
            .with_context(|| format!("{} 不足 24 字节，不是 PNG", path.display()))?;
        buf
    };
    if &head[..8] != b"\x89PNG\r\n\x1a\n" || &head[12..16] != b"IHDR" {
        bail!("{} 不是 PNG（缺少签名或 IHDR）", path.display());
    }
    let n = |i: usize| u32::from_be_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    Ok((n(16), n(20)))
}

fn mutool_hashes(pdf: &Path) -> Result<Vec<String>> {
    let args: Vec<String> = [
        "draw",
        "-q",
        "-r",
        "",
        "-c",
        "gray",
        "-s",
        "5",
        "-F",
        "png",
        "-o",
        "/dev/null",
    ]
    .iter()
    .enumerate()
    .map(|(i, s)| {
        if i == 3 {
            DPI.to_string()
        } else {
            s.to_string()
        }
    })
    .chain(std::iter::once(pdf.display().to_string()))
    .collect();

    let out =
        proc::run("mutool", &args, Path::new("."), &BTreeMap::new())?.context("mutool 未安装")?;
    if !out.success() {
        bail!("mutool draw 失败：{}", out.diagnostics());
    }
    parse_mutool_md5(&out.combined_text()?)
}

/// `mutool draw -s 5` 每页打印一行 `page <file> <n> <md5>`。
fn parse_mutool_md5(output: &str) -> Result<Vec<String>> {
    output
        .lines()
        .filter(|l| l.starts_with("page "))
        .map(|l| {
            l.split_whitespace()
                .next_back()
                .map(str::to_string)
                .with_context(|| format!("无法从 mutool 的输出行读出 md5：{l}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_size_out_of_a_png_header() {
        let dir = std::env::temp_dir().join("corpus-png-size-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.png");
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&625u32.to_be_bytes());
        bytes.extend_from_slice(&417u32.to_be_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(png_size(&path).unwrap(), (625, 417));

        std::fs::write(&path, b"not a png at all, but long enough........").unwrap();
        assert!(png_size(&path).is_err());
    }

    #[test]
    fn reads_one_md5_per_page() {
        let out = "page a.pdf 1 876412db3c741bcbc4058a3a14050b0d\npage a.pdf 2 000000000000000000000000000000ff\n";
        assert_eq!(
            parse_mutool_md5(out).unwrap(),
            vec![
                "876412db3c741bcbc4058a3a14050b0d".to_string(),
                "000000000000000000000000000000ff".to_string()
            ]
        );
    }

    #[test]
    fn ignores_lines_that_are_not_page_reports() {
        let out = "warning: something\npage a.pdf 1 abc\ntotal 12ms\n";
        assert_eq!(parse_mutool_md5(out).unwrap(), vec!["abc".to_string()]);
    }
}
