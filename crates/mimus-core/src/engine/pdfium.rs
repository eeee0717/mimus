//! `pdfium-render` adapter. No wrapper types leave this module.

use std::path::Path;

use pdfium_render::prelude::*;

use super::{PageCharSnapshot, PdfInspector, Rasterizer, RgbaImage};
use crate::error::{AssetReason, InputReason, MimusError, Result};
use crate::il::{PageGeometry, Point, Rect};

#[derive(Debug)]
pub struct PdfiumEngine {
    pdfium: Pdfium,
}

impl PdfiumEngine {
    pub fn from_environment() -> Result<Self> {
        if let Some(path) = std::env::var_os("MIMUS_PDFIUM_LIBRARY") {
            return Self::new(Path::new(&path));
        }
        let executable = std::env::current_exe().map_err(|error| {
            MimusError::asset(
                AssetReason::PdfiumUnavailable,
                format!("could not locate the mimus executable: {error}"),
            )
        })?;
        let adjacent = executable
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(Pdfium::pdfium_platform_library_name());
        if adjacent.is_file() {
            return Self::new(&adjacent);
        }
        Err(MimusError::asset(
            AssetReason::PdfiumUnavailable,
            format!(
                "PDFium was not found next to the executable ({})",
                adjacent.display()
            ),
        )
        .with_hint("set MIMUS_PDFIUM_LIBRARY to the pinned PDFium dynamic library"))
    }

    pub fn new(library: &Path) -> Result<Self> {
        if !library.is_file() {
            return Err(MimusError::asset(
                AssetReason::PdfiumUnavailable,
                format!("PDFium library does not exist: {}", library.display()),
            ));
        }
        let pdfium = match Pdfium::bind_to_library(library) {
            Ok(bindings) => Pdfium::new(bindings),
            Err(PdfiumError::PdfiumLibraryBindingsAlreadyInitialized) => Pdfium::default(),
            Err(error) => {
                return Err(MimusError::asset(
                    AssetReason::PdfiumUnavailable,
                    format!("could not load PDFium from {}: {error}", library.display()),
                ));
            }
        };
        Ok(Self { pdfium })
    }

    fn load<'a>(&'a self, pdf: &'a [u8]) -> Result<PdfDocument<'a>> {
        self.pdfium
            .load_pdf_from_byte_slice(pdf, None)
            .map_err(|error| {
                MimusError::input(
                    InputReason::UnsupportedPdf,
                    format!("PDFium could not open the input PDF: {error}"),
                )
            })
    }

    fn page<'a>(document: &PdfDocument<'a>, page_index: usize) -> Result<PdfPage<'a>> {
        let index = i32::try_from(page_index).map_err(|_| {
            MimusError::input(
                InputReason::UnsupportedPdf,
                format!("page index {page_index} exceeds PDFium's range"),
            )
        })?;
        document.pages().get(index).map_err(|error| {
            MimusError::input(
                InputReason::UnsupportedPdf,
                format!("PDFium could not open page {page_index}: {error}"),
            )
        })
    }

    /// Queries optional PDFium text flags for offline diagnostics. The normal production
    /// `PdfInspector` path deliberately does not add these calls or their failure modes.
    pub fn page_characters_with_text_diagnostics(
        &self,
        pdf: &[u8],
        page_index: usize,
    ) -> Result<Vec<PageCharSnapshot>> {
        self.page_characters_internal(pdf, page_index, true)
    }

    fn page_characters_internal(
        &self,
        pdf: &[u8],
        page_index: usize,
        inspect_text_flags: bool,
    ) -> Result<Vec<PageCharSnapshot>> {
        let document = self.load(pdf)?;
        let page = Self::page(&document, page_index)?;
        let text = page.text().map_err(|error| {
            MimusError::input(
                InputReason::UnsupportedPdf,
                format!("PDFium could not load text for page {page_index}: {error}"),
            )
        })?;
        let mut snapshots = Vec::new();
        for character in text.chars().iter() {
            if character.is_generated().map_err(|error| {
                MimusError::input(
                    InputReason::UnsupportedPdf,
                    format!("PDFium could not classify a generated character: {error}"),
                )
            })? {
                continue;
            }
            let (x, y) = character.origin().map_err(character_error)?;
            let tight = character.tight_bounds().map_err(character_error)?;
            let loose = character.loose_bounds().map_err(character_error)?;
            let is_hyphen = inspect_text_flags
                .then(|| character.is_hyphen().map_err(character_error))
                .transpose()?;
            snapshots.push(PageCharSnapshot {
                index: u32::try_from(character.index()).map_err(|_| {
                    MimusError::input(
                        InputReason::UnsupportedPdf,
                        "PDFium returned a negative character index",
                    )
                })?,
                unicode: character.unicode_char(),
                unicode_value: character.unicode_value(),
                is_hyphen,
                baseline_origin: Point {
                    x: f64::from(x.value),
                    y: f64::from(y.value),
                },
                tight_box: pdfium_rect(tight),
                loose_box: pdfium_rect(loose),
            });
        }
        Ok(snapshots)
    }
}

impl PdfInspector for PdfiumEngine {
    fn page_count(&self, pdf: &[u8]) -> Result<usize> {
        let document = self.load(pdf)?;
        usize::try_from(document.pages().len()).map_err(|_| {
            MimusError::input(
                InputReason::UnsupportedPdf,
                "PDF page count cannot be represented on this platform",
            )
        })
    }

    fn page_geometry(&self, pdf: &[u8], page_index: usize) -> Result<PageGeometry> {
        let document = self.load(pdf)?;
        let page = Self::page(&document, page_index)?;
        let rotation = page.rotation().map_err(|error| {
            MimusError::input(
                InputReason::UnsupportedPdf,
                format!("PDFium could not read page {page_index} rotation: {error}"),
            )
        })?;
        Ok(PageGeometry {
            width: f64::from(page.width().value),
            height: f64::from(page.height().value),
            rotate_degrees: rotation.as_degrees() as i32,
        })
    }

    fn page_characters(&self, pdf: &[u8], page_index: usize) -> Result<Vec<PageCharSnapshot>> {
        self.page_characters_internal(pdf, page_index, false)
    }
}

impl Rasterizer for PdfiumEngine {
    fn rasterize_page(&self, pdf: &[u8], page_index: usize) -> Result<RgbaImage> {
        self.rasterize_page_at_scale(pdf, page_index, 1.0)
    }

    fn rasterize_page_at_scale(
        &self,
        pdf: &[u8],
        page_index: usize,
        pixels_per_point: f32,
    ) -> Result<RgbaImage> {
        let document = self.load(pdf)?;
        let page = Self::page(&document, page_index)?;
        let width = pixel_dimension(page.width().value, pixels_per_point, "width")?;
        let height = pixel_dimension(page.height().value, pixels_per_point, "height")?;
        let bitmap = page
            .render_with_config(
                &PdfRenderConfig::new()
                    .set_target_size(width, height)
                    .set_format(PdfBitmapFormat::BGRA)
                    .set_clear_color(PdfColor::WHITE)
                    .render_annotations(false)
                    .render_form_data(false),
            )
            .map_err(|error| {
                MimusError::input(
                    InputReason::UnsupportedPdf,
                    format!("PDFium could not rasterize page {page_index}: {error}"),
                )
            })?;
        RgbaImage::new(
            u32::try_from(bitmap.width()).map_err(|_| {
                MimusError::input(InputReason::UnsupportedPdf, "negative raster width")
            })?,
            u32::try_from(bitmap.height()).map_err(|_| {
                MimusError::input(InputReason::UnsupportedPdf, "negative raster height")
            })?,
            bitmap.as_rgba_bytes(),
        )
    }
}

fn character_error(error: PdfiumError) -> MimusError {
    MimusError::input(
        InputReason::UnsupportedPdf,
        format!("PDFium could not inspect a character: {error}"),
    )
}

fn pdfium_rect(value: PdfRect) -> Rect {
    Rect {
        left: f64::from(value.left().value),
        bottom: f64::from(value.bottom().value),
        right: f64::from(value.right().value),
        top: f64::from(value.top().value),
    }
}

fn pixel_dimension(points: f32, pixels_per_point: f32, name: &str) -> Result<i32> {
    let pixels = points * pixels_per_point;
    if !points.is_finite()
        || points <= 0.0
        || !pixels_per_point.is_finite()
        || pixels_per_point <= 0.0
        || pixels.ceil() > i32::MAX as f32
    {
        return Err(MimusError::input(
            InputReason::UnsupportedPdf,
            format!("page {name} {points} cannot be rasterized"),
        ));
    }
    Ok(pixels.ceil() as i32)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_path(id: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures")
            .join(id)
            .join(format!("{id}.pdf"))
    }

    #[test]
    fn adapter_returns_owned_m1_snapshots() {
        let library = std::env::var_os("MIMUS_PDFIUM_LIBRARY")
            .expect("MIMUS_PDFIUM_LIBRARY must point to the pinned test dylib");
        let engine = PdfiumEngine::new(Path::new(&library)).unwrap();
        let fixture = fixture_path("unit-base-01-single-line");
        let bytes = std::fs::read(fixture).unwrap();
        assert_eq!(engine.page_count(&bytes).unwrap(), 1);
        assert_eq!(
            engine.page_geometry(&bytes, 0).unwrap(),
            PageGeometry {
                width: 300.0,
                height: 200.0,
                rotate_degrees: 0,
            }
        );

        let characters = engine.page_characters(&bytes, 0).unwrap();
        assert_eq!(
            characters
                .iter()
                .filter_map(|character| character.unicode)
                .collect::<String>(),
            "MIMUS"
        );
        assert_close(characters[0].baseline_origin.x, 72.0, 0.001);
        assert_close(characters[0].baseline_origin.y, 120.0, 0.001);
        assert!(
            characters
                .iter()
                .all(|character| character.is_hyphen.is_none())
        );
        assert!(characters.iter().all(|character| {
            character.tight_box.left <= character.tight_box.right
                && character.tight_box.bottom <= character.tight_box.top
                && character.loose_box.left <= character.loose_box.right
                && character.loose_box.bottom <= character.loose_box.top
        }));
        assert!(
            engine
                .page_characters_with_text_diagnostics(&bytes, 0)
                .unwrap()
                .iter()
                .all(|character| character.is_hyphen == Some(false))
        );

        let raster = engine.rasterize_page(&bytes, 0).unwrap();
        assert_eq!((raster.width(), raster.height()), (300, 200));
        assert_eq!(raster.rgba8().len(), 300 * 200 * 4);

        assert_rotated_crop_box_view_geometry(&engine);
    }

    fn assert_rotated_crop_box_view_geometry(engine: &PdfiumEngine) {
        for (id, expected) in [
            (
                "unit-geom-01-rotate-0",
                PageGeometry {
                    width: 300.0,
                    height: 200.0,
                    rotate_degrees: 0,
                },
            ),
            (
                "unit-geom-01-rotate-90",
                PageGeometry {
                    width: 200.0,
                    height: 300.0,
                    rotate_degrees: 90,
                },
            ),
            (
                "unit-geom-01-rotate-180",
                PageGeometry {
                    width: 300.0,
                    height: 200.0,
                    rotate_degrees: 180,
                },
            ),
            (
                "unit-geom-01-rotate-270",
                PageGeometry {
                    width: 200.0,
                    height: 300.0,
                    rotate_degrees: 270,
                },
            ),
            (
                "unit-geom-01-rotate-neg90",
                PageGeometry {
                    width: 200.0,
                    height: 300.0,
                    rotate_degrees: 270,
                },
            ),
            (
                "unit-geom-05-nonzero-origin-boxes",
                PageGeometry {
                    width: 260.0,
                    height: 160.0,
                    rotate_degrees: 0,
                },
            ),
        ] {
            let bytes = std::fs::read(fixture_path(id)).unwrap();
            assert_eq!(engine.page_geometry(&bytes, 0).unwrap(), expected, "{id}");
        }
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected} +/- {tolerance}, got {actual}"
        );
    }
}
