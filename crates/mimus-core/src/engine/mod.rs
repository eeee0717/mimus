use crate::error::{InputReason, MimusError, Result};
use crate::il::{PageGeometry, Point, Rect};

pub mod pdfium;

#[derive(Debug, Clone, PartialEq)]
pub struct PageCharSnapshot {
    pub index: u32,
    pub unicode: Option<char>,
    /// `FPDFText_GetUnicode` 原值；PDF 源字符码必须来自 operator walk，不能与此字段混用。
    pub unicode_value: u32,
    pub baseline_origin: Point,
    pub tight_box: Rect,
    pub loose_box: Rect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    width: u32,
    height: u32,
    rgba8: Vec<u8>,
}

impl RgbaImage {
    pub fn new(width: u32, height: u32, rgba8: Vec<u8>) -> Result<Self> {
        let image = Self {
            width,
            height,
            rgba8,
        };
        image.validate()?;
        Ok(image)
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub fn rgba8(&self) -> &[u8] {
        &self.rgba8
    }

    pub fn validate(&self) -> Result<()> {
        let expected = usize::try_from(self.width)
            .ok()
            .and_then(|width| {
                usize::try_from(self.height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| {
                MimusError::input(
                    InputReason::EngineMismatch,
                    "RGBA raster dimensions overflow this platform",
                )
            })?;
        if self.rgba8.len() != expected {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!(
                    "RGBA raster is {} bytes; {}x{} requires {expected}",
                    self.rgba8.len(),
                    self.width,
                    self.height
                ),
            ));
        }
        Ok(())
    }
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
        let mut regions = vec![LayoutRegion {
            bounds: first.tight_box,
            reading_order: 0,
        }];
        let mut baseline_y = first.baseline_origin.y;
        for character in &characters[1..] {
            if !baseline_y.is_finite() || !character.baseline_origin.y.is_finite() {
                return Err(MimusError::input(
                    InputReason::EngineMismatch,
                    "the inspection engine returned a non-finite baseline",
                ));
            }
            if (character.baseline_origin.y - baseline_y).abs() > 0.001 {
                baseline_y = character.baseline_origin.y;
                regions.push(LayoutRegion {
                    bounds: character.tight_box,
                    reading_order: regions.len(),
                });
            } else if let Some(region) = regions.last_mut() {
                region.bounds = region.bounds.union(character.tight_box);
            }
        }
        Ok(regions)
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
                unicode_value: u32::from('M'),
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
                unicode_value: u32::from('I'),
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
                &RgbaImage::new(1, 1, vec![255; 4]).unwrap(),
                &characters,
            )
            .unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].bounds.right, 83.0);
    }

    #[test]
    fn trivial_detector_exposes_multiple_baselines_to_the_m1_guard() {
        let mut characters = vec![PageCharSnapshot {
            index: 0,
            unicode: Some('M'),
            unicode_value: u32::from('M'),
            baseline_origin: Point { x: 72.0, y: 120.0 },
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        }];
        characters.push(PageCharSnapshot {
            index: 1,
            unicode: Some('I'),
            unicode_value: u32::from('I'),
            baseline_origin: Point { x: 72.0, y: 80.0 },
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        });
        let regions = SingleLineLayoutDetector
            .detect(
                PageGeometry {
                    width: 300.0,
                    height: 200.0,
                    rotate_degrees: 0,
                },
                &RgbaImage::new(1, 1, vec![255; 4]).unwrap(),
                &characters,
            )
            .unwrap();
        assert_eq!(regions.len(), 2);
    }

    #[test]
    fn rgba_image_rejects_an_invalid_buffer_length() {
        assert_eq!(
            RgbaImage::new(2, 2, vec![0; 15])
                .unwrap_err()
                .category()
                .code(),
            2
        );
    }
}
