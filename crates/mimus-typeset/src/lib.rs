//! Typesetting translated text back into the original box.
//!
//! The strategy that works, borrowed from BabelDOC: try to fit at scale 1.0;
//! on failure shrink the scale, and once shrinking alone stops being enough,
//! try growing the box downwards and then rightwards into whitespace. Only
//! after both fail does the text get squeezed to the floor.
//!
//! CJK needs line-break prohibition rules (no opening bracket at line end, no
//! closing bracket or terminal punctuation at line start) and hanging
//! punctuation; without them the output reads as obviously machine-made.

use mimus_ir::{Box2, Page, Paragraph};

#[derive(Debug, Clone, Copy)]
pub struct FitParams {
    pub min_scale: f32,
    /// Line advance as a multiple of font size. CJK wants more than Latin.
    pub line_skip: f32,
    pub allow_box_growth: bool,
}

impl Default for FitParams {
    fn default() -> Self {
        Self {
            min_scale: 0.1,
            line_skip: 1.5,
            allow_box_growth: true,
        }
    }
}

/// Lay out a paragraph, returning the scale it settled on.
pub fn fit(_p: &mut Paragraph, _page: &Page, _params: FitParams) -> anyhow::Result<f32> {
    anyhow::bail!("not implemented")
}

/// Largest empty rectangle below `bbox` on `page`, for box growth.
pub fn space_below(_bbox: &Box2, _page: &Page) -> f32 {
    0.0
}
