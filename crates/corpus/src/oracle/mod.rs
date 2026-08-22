//! 独立 oracle（docs/03-corpus-requirements.md §2.8）。
//!
//! 全部能力来自仓库外的第三方工具，且分工是**刻意互补**的：
//!
//! | 工具 | 提供 | 顺序语义 |
//! |---|---|---|
//! | poppler `pdftotext -bbox-layout` | 词的**字体度量盒** | 版面分析后的**阅读顺序** |
//! | mutool `draw -F stext` | 字形的**墨迹盒** + **baseline origin** | content stream 的**绘制顺序** |
//! | qpdf `--check` | 结构合法性 | — |
//! | pdftoppm / mutool draw | 栅格哈希 | — |
//!
//! 两个解析器共同报告的量只有 x 跨度与文本——那正是 §2.1「两者在容差内一致时
//! 才采信」所裁定的对象。y 方向它们报的是**不同的量**（度量盒 vs 墨迹盒），
//! 要求相等是错的，因此改判包含关系。

pub mod mupdf;
pub mod mupdf_svg;
pub mod poppler;
pub mod qpdf;
pub mod render;
pub mod xml;

use crate::geom::{Point, Rect};

/// 一个解析器眼中的文本块。
#[derive(Debug, Clone)]
pub struct ParsedBlock {
    /// 归一化后的完整文本。
    pub text: String,
    /// 页面空间中的包围盒。语义随来源而定：poppler 给度量盒，mutool 给墨迹盒。
    pub rect: Rect,
    /// 首字符的 baseline origin；只有 mutool 给得出。
    pub baseline_origin: Option<Point>,
}

/// 一个解析器眼中的一页。
#[derive(Debug, Clone)]
pub struct ParsedPage {
    pub index: usize,
    /// 解析器报告的观看空间页面尺寸（已应用 `/Rotate`）。
    pub viewer_size: (f64, f64),
    pub blocks: Vec<ParsedBlock>,
}
