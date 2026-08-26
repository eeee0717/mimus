use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::{Outlet, TensorElementType, TensorRef, ValueType};

use crate::error::{AssetReason, MimusError, Result};
use crate::il::{LayoutLabel, LayoutSource, PageGeometry, Rect};

use super::{LayoutDetector, LayoutRegion, PageCharSnapshot, RgbaImage};

const MODEL_SIDE: usize = 800;
const CONFIDENCE_THRESHOLD: f32 = 0.5;
const RASTER_PIXELS_PER_POINT: f32 = 200.0 / 72.0;

pub struct OnnxLayoutDetector {
    session: Mutex<Session>,
}

impl fmt::Debug for OnnxLayoutDetector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OnnxLayoutDetector")
            .finish_non_exhaustive()
    }
}

impl OnnxLayoutDetector {
    pub const fn raster_pixels_per_point() -> f32 {
        RASTER_PIXELS_PER_POINT
    }

    pub fn from_file(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Err(model_error());
        }
        let session = Session::builder()
            .map_err(ort_error)?
            .with_intra_threads(4)
            .map_err(ort_error)?
            .commit_from_file(path)
            .map_err(ort_error)?;
        validate_signature(&session)?;
        Ok(Self {
            session: Mutex::new(session),
        })
    }
}

impl LayoutDetector for OnnxLayoutDetector {
    fn raster_pixels_per_point(&self) -> f32 {
        Self::raster_pixels_per_point()
    }

    fn detect(
        &self,
        _page_index: usize,
        geometry: PageGeometry,
        raster: &RgbaImage,
        _characters: &[PageCharSnapshot],
    ) -> Result<Vec<LayoutRegion>> {
        let prepared = preprocess(raster)?;
        let image = TensorRef::from_array_view((
            [1, 3, MODEL_SIDE, MODEL_SIDE],
            prepared.image_chw.as_slice(),
        ))
        .map_err(ort_error)?;
        let im_shape = TensorRef::from_array_view(([1, 2], prepared.im_shape.as_slice()))
            .map_err(ort_error)?;
        let scale_factor = TensorRef::from_array_view(([1, 2], prepared.scale_factor.as_slice()))
            .map_err(ort_error)?;
        let mut session = self.session.lock().map_err(|_| model_error())?;
        let outputs = session
            .run(ort::inputs![
                "image" => image,
                "im_shape" => im_shape,
                "scale_factor" => scale_factor,
            ])
            .map_err(ort_error)?;
        let boxes = outputs.get("fetch_name_0").ok_or_else(model_error)?;
        let (box_shape, box_data) = boxes.try_extract_tensor::<f32>().map_err(ort_error)?;
        let counts = outputs.get("fetch_name_1").ok_or_else(model_error)?;
        let (count_shape, count_data) = counts.try_extract_tensor::<i32>().map_err(ort_error)?;
        if count_shape.as_ref() != [1] || count_data.len() != 1 {
            return Err(model_error());
        }
        let rows = output_rows(box_shape, box_data, count_data[0])?;
        postprocess(&rows, geometry, raster.width(), raster.height())
    }
}

fn validate_signature(session: &Session) -> Result<()> {
    validate_outlets(
        session.inputs(),
        &[
            ("image", TensorElementType::Float32, &[-1, 3, 800, 800]),
            ("im_shape", TensorElementType::Float32, &[-1, 2]),
            ("scale_factor", TensorElementType::Float32, &[-1, 2]),
        ],
    )?;
    validate_outlets(
        session.outputs(),
        &[
            ("fetch_name_0", TensorElementType::Float32, &[-1, 7]),
            ("fetch_name_1", TensorElementType::Int32, &[-1]),
            ("fetch_name_2", TensorElementType::Int32, &[-1, 200, 200]),
        ],
    )
}

fn validate_outlets(
    actual: &[Outlet],
    expected: &[(&str, TensorElementType, &[i64])],
) -> Result<()> {
    if actual.len() != expected.len() {
        return Err(model_error());
    }
    for (name, expected_type, expected_shape) in expected {
        let outlet = actual
            .iter()
            .find(|outlet| outlet.name() == *name)
            .ok_or_else(model_error)?;
        match outlet.dtype() {
            ValueType::Tensor { ty, shape, .. }
                if ty == expected_type && shape.as_ref() == *expected_shape => {}
            _ => return Err(model_error()),
        }
    }
    Ok(())
}

struct PreprocessedPage {
    image_chw: Vec<f32>,
    im_shape: [f32; 2],
    scale_factor: [f32; 2],
}

fn preprocess(raster: &RgbaImage) -> Result<PreprocessedPage> {
    if raster.width() == 0 || raster.height() == 0 {
        return Err(model_error());
    }
    let rgb = raster
        .rgba8()
        .chunks_exact(4)
        .flat_map(|pixel| pixel[..3].iter().copied())
        .collect::<Vec<_>>();
    let image =
        image::RgbImage::from_raw(raster.width(), raster.height(), rgb).ok_or_else(model_error)?;
    let resized = image::imageops::resize(
        &image,
        MODEL_SIDE as u32,
        MODEL_SIDE as u32,
        image::imageops::FilterType::CatmullRom,
    );
    Ok(PreprocessedPage {
        image_chw: normalize_rgb_to_chw(resized.as_raw(), MODEL_SIDE, MODEL_SIDE)?,
        im_shape: [MODEL_SIDE as f32, MODEL_SIDE as f32],
        scale_factor: [
            MODEL_SIDE as f32 / raster.height() as f32,
            MODEL_SIDE as f32 / raster.width() as f32,
        ],
    })
}

fn output_rows(shape: &[i64], data: &[f32], bbox_count: i32) -> Result<Vec<[f32; 7]>> {
    let [row_count, 7] = shape else {
        return Err(model_error());
    };
    let row_count = usize::try_from(*row_count).map_err(|_| model_error())?;
    let bbox_count = usize::try_from(bbox_count).map_err(|_| model_error())?;
    if bbox_count > row_count || data.len() != row_count.saturating_mul(7) {
        return Err(model_error());
    }
    data.chunks_exact(7)
        .take(bbox_count)
        .map(|row| <[f32; 7]>::try_from(row).map_err(|_| model_error()))
        .collect()
}

fn normalize_rgb_to_chw(rgb: &[u8], width: usize, height: usize) -> Result<Vec<f32>> {
    let pixels = width.checked_mul(height).ok_or_else(model_error)?;
    let expected = pixels.checked_mul(3).ok_or_else(model_error)?;
    if pixels == 0 || rgb.len() != expected {
        return Err(model_error());
    }
    let mut chw = vec![0.0; expected];
    for (pixel_index, pixel) in rgb.chunks_exact(3).enumerate() {
        for channel in 0..3 {
            chw[channel * pixels + pixel_index] = f32::from(pixel[channel]) / 255.0;
        }
    }
    Ok(chw)
}

fn label_for_class(class_id: usize) -> Result<LayoutLabel> {
    const LABELS: [LayoutLabel; 25] = [
        LayoutLabel::Abstract,
        LayoutLabel::Algorithm,
        LayoutLabel::AsideText,
        LayoutLabel::Chart,
        LayoutLabel::Content,
        LayoutLabel::DisplayFormula,
        LayoutLabel::DocTitle,
        LayoutLabel::FigureTitle,
        LayoutLabel::Footer,
        LayoutLabel::FooterImage,
        LayoutLabel::Footnote,
        LayoutLabel::FormulaNumber,
        LayoutLabel::Header,
        LayoutLabel::HeaderImage,
        LayoutLabel::Image,
        LayoutLabel::InlineFormula,
        LayoutLabel::Number,
        LayoutLabel::ParagraphTitle,
        LayoutLabel::Reference,
        LayoutLabel::ReferenceContent,
        LayoutLabel::Seal,
        LayoutLabel::Table,
        LayoutLabel::Text,
        LayoutLabel::VerticalText,
        LayoutLabel::VisionFootnote,
    ];
    LABELS.get(class_id).copied().ok_or_else(model_error)
}

fn postprocess(
    rows: &[[f32; 7]],
    geometry: PageGeometry,
    raster_width: u32,
    raster_height: u32,
) -> Result<Vec<LayoutRegion>> {
    if raster_width == 0
        || raster_height == 0
        || !geometry.width.is_finite()
        || !geometry.height.is_finite()
        || geometry.width <= 0.0
        || geometry.height <= 0.0
    {
        return Err(model_error());
    }

    let mut by_query = BTreeMap::<usize, [f32; 7]>::new();
    for row in rows {
        if row.iter().any(|value| !value.is_finite())
            || row[0] < 0.0
            || row[0].fract() != 0.0
            || row[1] < 0.0
            || row[1] > 1.0
            || row[6] < 0.0
            || row[6].fract() != 0.0
        {
            return Err(model_error());
        }
        let class_id = row[0] as usize;
        let query_id = row[6] as usize;
        let _ = label_for_class(class_id)?;
        if query_id >= 300 {
            return Err(model_error());
        }
        by_query
            .entry(query_id)
            .and_modify(|current| {
                if row[1] > current[1] {
                    *current = *row;
                }
            })
            .or_insert(*row);
    }

    let raster_width = raster_width as f64;
    let raster_height = raster_height as f64;
    let mut regions = Vec::new();
    for (query_id, row) in by_query {
        if row[1] < CONFIDENCE_THRESHOLD {
            continue;
        }
        let left_px = f64::from(row[2]).clamp(0.0, raster_width);
        let top_px = f64::from(row[3]).clamp(0.0, raster_height);
        let right_px = f64::from(row[4]).clamp(0.0, raster_width);
        let bottom_px = f64::from(row[5]).clamp(0.0, raster_height);
        if right_px <= left_px || bottom_px <= top_px {
            continue;
        }
        regions.push(LayoutRegion {
            bounds: Rect {
                left: left_px * geometry.width / raster_width,
                bottom: (raster_height - bottom_px) * geometry.height / raster_height,
                right: right_px * geometry.width / raster_width,
                top: (raster_height - top_px) * geometry.height / raster_height,
            },
            reading_order: query_id,
            label: label_for_class(row[0] as usize)?,
            source: LayoutSource::Model,
            confidence: row[1],
        });
    }
    Ok(regions)
}

fn model_error() -> MimusError {
    MimusError::asset(
        AssetReason::LayoutModelUnavailable,
        "PP-DocLayoutV3 model is missing, damaged, or incompatible",
    )
    .with_hint("provide the pinned model with --layout-model or MIMUS_LAYOUT_MODEL")
}

fn ort_error(error: impl fmt::Display) -> MimusError {
    MimusError::asset(
        AssetReason::LayoutModelUnavailable,
        format!("PP-DocLayoutV3 could not be loaded or executed: {error}"),
    )
    .with_hint("provide the pinned model with --layout-model or MIMUS_LAYOUT_MODEL")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::il::{LayoutSource, Rect};

    #[test]
    fn preprocessing_scales_rgb_to_chw_without_mean_or_std_normalization() {
        let chw = normalize_rgb_to_chw(&[255, 0, 0, 0, 128, 255], 2, 1).unwrap();
        assert_eq!(chw, vec![1.0, 0.0, 0.0, 128.0 / 255.0, 0.0, 1.0]);
    }

    #[test]
    fn preprocessing_resizes_to_800_and_builds_the_two_auxiliary_inputs() {
        let raster = RgbaImage::new(2, 4, [64, 128, 255, 0].repeat(8)).unwrap();
        let prepared = preprocess(&raster).unwrap();

        assert_eq!(prepared.image_chw.len(), 3 * MODEL_SIDE * MODEL_SIDE);
        assert_eq!(prepared.im_shape, [800.0, 800.0]);
        assert_eq!(prepared.scale_factor, [200.0, 400.0]);
        assert_eq!(prepared.image_chw[0], 64.0 / 255.0);
        assert_eq!(prepared.image_chw[MODEL_SIDE * MODEL_SIDE], 128.0 / 255.0);
        assert_eq!(prepared.image_chw[2 * MODEL_SIDE * MODEL_SIDE], 1.0);
    }

    #[test]
    fn bbox_count_limits_the_m_by_seven_output_rows() {
        let data = (0..21).map(|value| value as f32).collect::<Vec<_>>();
        assert_eq!(
            output_rows(&[3, 7], &data, 2).unwrap(),
            vec![
                [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
                [7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0],
            ]
        );
        assert!(output_rows(&[3, 6], &data, 2).is_err());
        assert!(output_rows(&[3, 7], &data, 4).is_err());
        assert!(output_rows(&[3, 7], &data, -1).is_err());
    }

    #[test]
    fn label_indices_match_the_pinned_25_class_vocabulary() {
        let labels = (0..25)
            .map(|class_id| label_for_class(class_id).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                LayoutLabel::Abstract,
                LayoutLabel::Algorithm,
                LayoutLabel::AsideText,
                LayoutLabel::Chart,
                LayoutLabel::Content,
                LayoutLabel::DisplayFormula,
                LayoutLabel::DocTitle,
                LayoutLabel::FigureTitle,
                LayoutLabel::Footer,
                LayoutLabel::FooterImage,
                LayoutLabel::Footnote,
                LayoutLabel::FormulaNumber,
                LayoutLabel::Header,
                LayoutLabel::HeaderImage,
                LayoutLabel::Image,
                LayoutLabel::InlineFormula,
                LayoutLabel::Number,
                LayoutLabel::ParagraphTitle,
                LayoutLabel::Reference,
                LayoutLabel::ReferenceContent,
                LayoutLabel::Seal,
                LayoutLabel::Table,
                LayoutLabel::Text,
                LayoutLabel::VerticalText,
                LayoutLabel::VisionFootnote,
            ]
        );
        assert!(label_for_class(25).is_err());
    }

    #[test]
    fn postprocessing_keeps_each_queries_highest_class_and_sorts_by_query_id() {
        let regions = postprocess(
            &[
                [22.0, 0.60, 10.0, 20.0, 30.0, 40.0, 7.0],
                [17.0, 0.80, 10.0, 20.0, 30.0, 40.0, 7.0],
                [18.0, 0.70, 0.0, 0.0, 50.0, 50.0, 2.0],
                [15.0, 0.49, 50.0, 50.0, 80.0, 80.0, 9.0],
            ],
            PageGeometry {
                width: 200.0,
                height: 400.0,
                rotate_degrees: 0,
            },
            100,
            100,
        )
        .unwrap();

        assert_eq!(regions.len(), 2);
        assert_eq!(regions[0].reading_order, 2);
        assert_eq!(regions[0].label, LayoutLabel::Reference);
        assert_eq!(
            regions[0].bounds,
            Rect {
                left: 0.0,
                bottom: 200.0,
                right: 100.0,
                top: 400.0,
            }
        );
        assert_eq!(regions[1].reading_order, 7);
        assert_eq!(regions[1].label, LayoutLabel::ParagraphTitle);
        assert_eq!(
            regions[1].bounds,
            Rect {
                left: 20.0,
                bottom: 240.0,
                right: 60.0,
                top: 320.0,
            }
        );
        assert_eq!(regions[1].source, LayoutSource::Model);
        assert_eq!(regions[1].confidence, 0.8);
    }

    #[test]
    fn model_constants_match_the_pinned_preprocessing_contract() {
        assert_eq!(MODEL_SIDE, 800);
        assert_eq!(CONFIDENCE_THRESHOLD, 0.5);
        assert_eq!(OnnxLayoutDetector::raster_pixels_per_point(), 200.0 / 72.0);
    }

    #[test]
    fn damaged_model_fails_at_detector_construction_as_an_asset_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("damaged.onnx");
        std::fs::write(&path, b"not an ONNX model").unwrap();

        let error = OnnxLayoutDetector::from_file(&path).err().unwrap();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Asset(AssetReason::LayoutModelUnavailable)
        );
        assert_eq!(error.category().code(), 3);
    }

    #[test]
    fn m0_qualification_matches_archived_boxes_classes_and_reading_order() {
        let Some(model) = std::env::var_os("MIMUS_LAYOUT_MODEL") else {
            eprintln!("MIMUS_LAYOUT_MODEL is unset; PP-DocLayoutV3 qualification is gated off");
            return;
        };
        let image = image::load_from_memory(include_bytes!(
            "../../tests/fixtures/pp-doclayoutv3/unit-order-01-natural-200dpi.png"
        ))
        .unwrap()
        .to_rgba8();
        let raster = RgbaImage::new(image.width(), image.height(), image.into_raw()).unwrap();
        let detector = OnnxLayoutDetector::from_file(&PathBuf::from(model)).unwrap();

        let regions = detector
            .detect(
                0,
                PageGeometry {
                    width: 420.0,
                    height: 220.0,
                    rotate_degrees: 0,
                },
                &raster,
                &[],
            )
            .unwrap();

        assert_eq!(regions.len(), 6);
        assert!(regions.iter().all(|region| {
            region.label == LayoutLabel::Text && region.source == LayoutSource::Model
        }));
        assert_eq!(
            regions
                .iter()
                .map(|region| region.reading_order)
                .collect::<Vec<_>>(),
            [23, 53, 125, 154, 230, 283]
        );
        let expected = [
            [27.5081, 151.3401, 198.0938, 192.2530],
            [27.3879, 98.0918, 196.9246, 138.5104],
            [27.4537, 44.2592, 196.6388, 84.5785],
            [222.8104, 161.3252, 391.7443, 192.3295],
            [222.6844, 116.7734, 391.7337, 148.1481],
            [222.7690, 74.1572, 391.5826, 104.3624],
        ];
        for (region, expected) in regions.iter().zip(expected) {
            for (actual, expected) in [
                region.bounds.left,
                region.bounds.bottom,
                region.bounds.right,
                region.bounds.top,
            ]
            .into_iter()
            .zip(expected)
            {
                assert!((actual - expected).abs() <= 0.001, "{actual} != {expected}");
            }
        }
    }
}
