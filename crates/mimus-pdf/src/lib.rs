//! PDF input and output.
//!
//! Reading is split deliberately. PDFium supplies glyph geometry and font
//! metrics -- the part with the deepest edge cases (Type3 matrices, CID widths,
//! missing-font fallbacks). Walking the raw operators supplies everything
//! PDFium discards: verbatim graphics state, XObject nesting, exact draw order.
//! Neither half alone is enough.
//!
//! Writing has two modes, and they are not interchangeable:
//!
//! - [`WriteMode::Incremental`] edits the original document in place, so
//!   images, vector art, annotations and bookmarks survive untouched. Required
//!   for native PDFs.
//! - [`WriteMode::Rebuild`] emits a fresh document. Lossless *only* when the
//!   source page is already just a raster -- i.e. the scanned path.

use mimus_ir::{Document, PageRaster};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    Incremental,
    Rebuild,
}

pub trait PdfReader {
    /// Rasterise one page. Milestone 1 (scanned path) needs nothing else.
    fn render_page(&self, page: usize, target_long_edge: u32) -> anyhow::Result<PageRaster>;

    /// Lift native characters into the IR. Milestone 2.
    fn parse(&self, _path: &Path) -> anyhow::Result<Document> {
        anyhow::bail!("native parsing not implemented yet -- milestone 2")
    }
}

pub trait PdfWriter {
    fn write(&self, doc: &Document, out: &Path, mode: WriteMode) -> anyhow::Result<()>;
}
