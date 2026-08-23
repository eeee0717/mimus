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
            snapshots.push(PageCharSnapshot {
                index: u32::try_from(character.index()).map_err(|_| {
                    MimusError::input(
                        InputReason::UnsupportedPdf,
                        "PDFium returned a negative character index",
                    )
                })?,
                unicode: character.unicode_char(),
                unicode_value: character.unicode_value(),
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

impl Rasterizer for PdfiumEngine {
    fn rasterize_page(&self, pdf: &[u8], page_index: usize) -> Result<RgbaImage> {
        let document = self.load(pdf)?;
        let page = Self::page(&document, page_index)?;
        let width = pixel_dimension(page.width().value, "width")?;
        let height = pixel_dimension(page.height().value, "height")?;
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

fn pixel_dimension(points: f32, name: &str) -> Result<i32> {
    if !points.is_finite() || points <= 0.0 || points.ceil() > i32::MAX as f32 {
        return Err(MimusError::input(
            InputReason::UnsupportedPdf,
            format!("page {name} {points} cannot be rasterized"),
        ));
    }
    Ok(points.ceil() as i32)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn adapter_returns_owned_m1_snapshots() {
        let library = std::env::var_os("MIMUS_PDFIUM_LIBRARY")
            .expect("MIMUS_PDFIUM_LIBRARY must point to the pinned test dylib");
        let engine = PdfiumEngine::new(Path::new(&library)).unwrap();
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf");
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
        assert!(characters.iter().all(|character| {
            character.tight_box.left <= character.tight_box.right
                && character.tight_box.bottom <= character.tight_box.top
                && character.loose_box.left <= character.loose_box.right
                && character.loose_box.bottom <= character.loose_box.top
        }));

        let raster = engine.rasterize_page(&bytes, 0).unwrap();
        assert_eq!((raster.width(), raster.height()), (300, 200));
        assert_eq!(raster.rgba8().len(), 300 * 200 * 4);
    }

    fn assert_close(actual: f64, expected: f64, tolerance: f64) {
        assert!(
            (actual - expected).abs() <= tolerance,
            "expected {expected} +/- {tolerance}, got {actual}"
        );
    }
}
