//! Document layout analysis.
//!
//! The model is deliberately behind a trait. BabelDOC learned this the hard
//! way and ended up with nine interchangeable backends; the label vocabulary
//! is model-specific and must stay data, never a closed enum.
//!
//! Target backend: PP-DocLayout (RT-DETR). Two things differ from the
//! YOLO-family exports and will bite on first contact:
//!
//! 1. It is NMS-free and Paddle's ONNX export usually wants `im_shape` and
//!    `scale_factor` as extra inputs. Output is a fixed set of queries laid out
//!    `[cls, score, x0, y0, x1, y1]` -- note `cls` first, unlike YOLO.
//! 2. Preprocessing is a plain resize to 800x800 with ImageNet mean/std, not a
//!    letterbox with /255. Getting this wrong yields boxes that look almost
//!    right, which is worse than boxes that look wrong.

use mimus_ir::Layout;
use mimus_pdf::PageRaster;

pub trait LayoutModel {
    fn detect(&self, page: &PageRaster) -> anyhow::Result<Vec<Layout>>;
}

/// Regions whose text should be translated. Everything else is passed through
/// or covered. Kept as a function of the label string so a backend swap does
/// not require touching downstream code.
pub fn is_translatable(label: &str) -> bool {
    matches!(
        label,
        "text" | "plain text" | "title" | "paragraph_title" | "abstract"
            | "content" | "figure_caption" | "table_caption" | "footnote"
            | "list_item" | "caption" | "doc_title"
    )
}
