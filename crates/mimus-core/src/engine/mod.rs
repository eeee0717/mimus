use crate::error::{InputReason, MimusError, Result};
use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;

use crate::il::{LayoutLabel, LayoutSource, PageGeometry, Point, Rect};

pub mod onnx_layout;
pub mod pdfium;

pub use onnx_layout::OnnxLayoutDetector;

#[derive(Debug, Clone, PartialEq)]
pub struct PageCharSnapshot {
    pub index: u32,
    pub unicode: Option<char>,
    /// `FPDFText_GetUnicode` 原值；PDF 源字符码必须来自 operator walk，不能与此字段混用。
    pub unicode_value: u32,
    /// `FPDFText_IsHyphen` 的提取语义标志；`None` 表示普通生产路径没有查询该诊断 API。
    pub is_hyphen: Option<bool>,
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

    fn rasterize_page_at_scale(
        &self,
        pdf: &[u8],
        page_index: usize,
        pixels_per_point: f32,
    ) -> Result<RgbaImage> {
        if (pixels_per_point - 1.0).abs() > f32::EPSILON {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                "the rasterizer does not support the detector's requested resolution",
            ));
        }
        self.rasterize_page(pdf, page_index)
    }
}

pub trait PdfEngine: PdfInspector + Rasterizer {}

impl<T> PdfEngine for T where T: PdfInspector + Rasterizer {}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutRegion {
    pub bounds: Rect,
    pub reading_order: usize,
    pub label: LayoutLabel,
    pub source: LayoutSource,
    pub confidence: f32,
}

pub trait LayoutDetector: Send + Sync {
    fn raster_pixels_per_point(&self) -> f32 {
        1.0
    }

    fn detect(
        &self,
        page_index: usize,
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
        _page_index: usize,
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
            label: LayoutLabel::Text,
            source: LayoutSource::FallbackLine,
            confidence: 1.0,
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
                    label: LayoutLabel::Text,
                    source: LayoutSource::FallbackLine,
                    confidence: 1.0,
                });
            } else if let Some(region) = regions.last_mut() {
                region.bounds = region.bounds.union(character.tight_box);
            }
        }
        Ok(regions)
    }
}

const LAYOUT_REPLAY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LayoutRecording {
    schema_version: u32,
    pages: Vec<RecordedLayoutPage>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedLayoutPage {
    page_index: usize,
    geometry: PageGeometry,
    regions: Vec<LayoutRegion>,
}

/// Strict replay of detector-owned snapshots. The input bytes are parsed once;
/// every page is then addressed explicitly, so replay does not depend on call
/// order or thread scheduling.
#[derive(Debug)]
pub struct RecordedLayoutDetector {
    pages: BTreeMap<usize, (PageGeometry, Vec<LayoutRegion>)>,
}

impl RecordedLayoutDetector {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let recording: LayoutRecording = serde_json::from_slice(bytes).map_err(|error| {
            MimusError::input(
                InputReason::EngineMismatch,
                format!("invalid layout recording: {error}"),
            )
        })?;
        if recording.schema_version != LAYOUT_REPLAY_SCHEMA_VERSION {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!(
                    "unsupported layout recording schema {}; expected {}",
                    recording.schema_version, LAYOUT_REPLAY_SCHEMA_VERSION
                ),
            ));
        }
        let mut pages = BTreeMap::new();
        for page in recording.pages {
            validate_recorded_page(&page)?;
            if pages
                .insert(page.page_index, (page.geometry, page.regions))
                .is_some()
            {
                return Err(MimusError::input(
                    InputReason::EngineMismatch,
                    format!("layout recording repeats page {}", page.page_index),
                ));
            }
        }
        Ok(Self { pages })
    }
}

fn validate_recorded_page(page: &RecordedLayoutPage) -> Result<()> {
    let mut orders = BTreeSet::new();
    for region in &page.regions {
        let bounds = region.bounds;
        if !bounds.left.is_finite()
            || !bounds.bottom.is_finite()
            || !bounds.right.is_finite()
            || !bounds.top.is_finite()
            || bounds.right <= bounds.left
            || bounds.top <= bounds.bottom
            || !region.confidence.is_finite()
            || !(0.0..=1.0).contains(&region.confidence)
        {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!(
                    "layout recording page {} has an invalid region",
                    page.page_index
                ),
            ));
        }
        if !orders.insert(region.reading_order) {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!(
                    "layout recording page {} repeats reading order {}",
                    page.page_index, region.reading_order
                ),
            ));
        }
    }
    Ok(())
}

impl LayoutDetector for RecordedLayoutDetector {
    fn detect(
        &self,
        page_index: usize,
        geometry: PageGeometry,
        _raster: &RgbaImage,
        _characters: &[PageCharSnapshot],
    ) -> Result<Vec<LayoutRegion>> {
        let (recorded_geometry, regions) = self.pages.get(&page_index).ok_or_else(|| {
            MimusError::input(
                InputReason::EngineMismatch,
                format!("layout recording has no page {page_index}"),
            )
        })?;
        if *recorded_geometry != geometry {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!("layout recording geometry differs on page {page_index}"),
            ));
        }
        Ok(regions.clone())
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
                is_hyphen: None,
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
                is_hyphen: None,
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
                0,
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
            is_hyphen: None,
            baseline_origin: Point { x: 72.0, y: 120.0 },
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        }];
        characters.push(PageCharSnapshot {
            index: 1,
            unicode: Some('I'),
            unicode_value: u32::from('I'),
            is_hyphen: None,
            baseline_origin: Point { x: 72.0, y: 80.0 },
            tight_box: Rect::default(),
            loose_box: Rect::default(),
        });
        let regions = SingleLineLayoutDetector
            .detect(
                0,
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

    #[test]
    fn recorded_detector_is_strict_and_page_addressed() {
        let bytes = br#"{
            "schema_version": 1,
            "pages": [{
                "page_index": 2,
                "geometry": {"width": 100.0, "height": 200.0, "rotate_degrees": 0},
                "regions": [{
                    "bounds": {"left": 1.0, "bottom": 2.0, "right": 30.0, "top": 40.0},
                    "reading_order": 7,
                    "label": "abstract",
                    "source": "model",
                    "confidence": 0.75
                }]
            }]
        }"#;
        let detector = RecordedLayoutDetector::from_bytes(bytes).unwrap();
        let geometry = PageGeometry {
            width: 100.0,
            height: 200.0,
            rotate_degrees: 0,
        };
        let raster = RgbaImage::new(1, 1, vec![255; 4]).unwrap();
        let first = detector.detect(2, geometry, &raster, &[]).unwrap();
        let second = detector.detect(2, geometry, &raster, &[]).unwrap();
        assert_eq!(first, second);
        assert_eq!(first[0].label, LayoutLabel::Abstract);
        assert!(detector.detect(1, geometry, &raster, &[]).is_err());

        let unknown_field = bytes
            .strip_suffix(b"}")
            .unwrap()
            .iter()
            .copied()
            .chain(br#", "unexpected": true}"#.iter().copied())
            .collect::<Vec<_>>();
        assert!(RecordedLayoutDetector::from_bytes(&unknown_field).is_err());
    }

    #[test]
    fn layout_policy_covers_the_fixed_model_vocabulary() {
        use crate::il::TranslationPolicy::{Passthrough, Translate};

        for label in [
            LayoutLabel::Abstract,
            LayoutLabel::AsideText,
            LayoutLabel::Content,
            LayoutLabel::DocTitle,
            LayoutLabel::FigureTitle,
            LayoutLabel::Footnote,
            LayoutLabel::ParagraphTitle,
            LayoutLabel::Text,
            LayoutLabel::VisionFootnote,
            LayoutLabel::FallbackLine,
        ] {
            assert_eq!(label.translation_policy(), Translate, "{label:?}");
        }
        for label in [
            LayoutLabel::Algorithm,
            LayoutLabel::Chart,
            LayoutLabel::DisplayFormula,
            LayoutLabel::Footer,
            LayoutLabel::FooterImage,
            LayoutLabel::FormulaNumber,
            LayoutLabel::Header,
            LayoutLabel::HeaderImage,
            LayoutLabel::Image,
            LayoutLabel::InlineFormula,
            LayoutLabel::Number,
            LayoutLabel::Reference,
            LayoutLabel::ReferenceContent,
            LayoutLabel::Seal,
            LayoutLabel::Table,
            LayoutLabel::VerticalText,
        ] {
            assert_eq!(label.translation_policy(), Passthrough, "{label:?}");
        }
    }
}
