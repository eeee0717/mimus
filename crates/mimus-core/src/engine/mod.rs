use crate::error::Result;
use crate::il::{PageGeometry, Point, Rect};

pub mod pdfium;

#[derive(Debug, Clone, PartialEq)]
pub struct PageCharSnapshot {
    pub index: u32,
    pub unicode: Option<char>,
    pub code: u32,
    pub baseline_origin: Point,
    pub tight_box: Rect,
    pub loose_box: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

pub trait PdfInspector: Send + Sync {
    fn page_count(&self, pdf: &[u8]) -> Result<usize>;
    fn page_geometry(&self, pdf: &[u8], page_index: usize) -> Result<PageGeometry>;
    fn page_characters(&self, pdf: &[u8], page_index: usize) -> Result<Vec<PageCharSnapshot>>;
}

pub trait Rasterizer: Send + Sync {
    fn rasterize_page(&self, pdf: &[u8], page_index: usize) -> Result<RgbaImage>;
}

pub trait PdfEngine: PdfInspector + Rasterizer {}

impl<T> PdfEngine for T where T: PdfInspector + Rasterizer {}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutRegion {
    pub bounds: Rect,
    pub reading_order: usize,
}

pub trait LayoutDetector: Send + Sync {
    fn detect(
        &self,
        geometry: PageGeometry,
        raster: &RgbaImage,
        characters: &[PageCharSnapshot],
    ) -> Result<Vec<LayoutRegion>>;
}

#[derive(Debug, Default)]
pub struct SingleLineLayoutDetector;

impl LayoutDetector for SingleLineLayoutDetector {
    fn detect(
        &self,
        _geometry: PageGeometry,
        _raster: &RgbaImage,
        characters: &[PageCharSnapshot],
    ) -> Result<Vec<LayoutRegion>> {
        let Some(first) = characters.first() else {
            return Ok(Vec::new());
        };
        let bounds = characters
            .iter()
            .skip(1)
            .fold(first.tight_box, |bounds, character| {
                bounds.union(character.tight_box)
            });
        Ok(vec![LayoutRegion {
            bounds,
            reading_order: 0,
        }])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trivial_detector_returns_one_region_for_one_line() {
        let characters = vec![
            PageCharSnapshot {
                index: 0,
                unicode: Some('M'),
                code: u32::from('M'),
                baseline_origin: Point { x: 72.0, y: 120.0 },
                tight_box: Rect {
                    left: 73.0,
                    bottom: 119.0,
                    right: 80.0,
                    top: 129.0,
                },
                loose_box: Rect::default(),
            },
            PageCharSnapshot {
                index: 1,
                unicode: Some('I'),
                code: u32::from('I'),
                baseline_origin: Point { x: 80.0, y: 120.0 },
                tight_box: Rect {
                    left: 81.0,
                    bottom: 119.0,
                    right: 83.0,
                    top: 129.0,
                },
                loose_box: Rect::default(),
            },
        ];
        let regions = SingleLineLayoutDetector
            .detect(
                PageGeometry {
                    width: 300.0,
                    height: 200.0,
                    rotate_degrees: 0,
                },
                &RgbaImage {
                    width: 1,
                    height: 1,
                    rgba8: vec![255; 4],
                },
                &characters,
            )
            .unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].bounds.right, 83.0);
    }
}
