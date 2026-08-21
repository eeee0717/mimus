//! Rasterised pages.
//!
//! This lives in the IR rather than in `mimus-pdf` so that the vision crates
//! depend only on "an image plus the scale that maps it back to page space",
//! and never on how that image was produced. Layout analysis has no business
//! knowing what a PDF is.

/// A page rendered to pixels for the vision models.
pub struct PageRaster {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGB8, row-major, top-left origin.
    pub rgb: Vec<u8>,
    /// Multiply a pixel length by this to get PDF points.
    pub px_to_pt: f32,
    /// Page rotation already applied during rendering, in degrees.
    pub applied_rotation: u16,
}

impl PageRaster {
    /// Map a box in raster pixel space (top-left origin, y down) to PDF user
    /// space (bottom-left origin, y up). Every model output goes through here.
    pub fn px_box_to_pt(&self, x0: f32, y0: f32, x1: f32, y1: f32) -> crate::Box2 {
        let h = self.height as f32;
        crate::Box2 {
            x0: x0 * self.px_to_pt,
            y0: (h - y1) * self.px_to_pt,
            x1: x1 * self.px_to_pt,
            y1: (h - y0) * self.px_to_pt,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_boxes_flip_the_y_axis() {
        let r = PageRaster {
            width: 100,
            height: 200,
            rgb: Vec::new(),
            px_to_pt: 0.5,
            applied_rotation: 0,
        };
        // A box hugging the TOP of the image must land at the TOP of the page,
        // which in PDF space means high y.
        let b = r.px_box_to_pt(0.0, 0.0, 10.0, 20.0);
        assert_eq!(b.y1, 100.0); // (200 - 0) * 0.5
        assert_eq!(b.y0, 90.0); // (200 - 20) * 0.5
        assert_eq!(b.x1, 5.0);
    }
}
