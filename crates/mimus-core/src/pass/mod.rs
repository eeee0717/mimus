use std::collections::BTreeSet;

use lopdf::Document as LopdfDocument;

use crate::context::{Document, ExtractedPage, PassContext};
use crate::error::{ErrorReason, MimusError, Result};
use crate::event::{Event, EventKind, Stage};
use crate::il::{self, Char, Paragraph, PassthroughRef, TextCarrier, TextTransform};
use crate::walk::walk_page;
use crate::write::{PageRewrite, incremental_publish};

pub const ORDER: [Stage; 10] = [
    Stage::Parse,
    Stage::ScanDetect,
    Stage::Layout,
    Stage::ParagraphFind,
    Stage::StylesAndFormulas,
    Stage::ExtractTerms,
    Stage::Translate,
    Stage::Typeset,
    Stage::FontEmbed,
    Stage::Write,
];

pub type Pass = fn(&mut Document, &PassContext<'_>) -> Result<()>;

pub const PIPELINE: [(Stage, Pass); 10] = [
    (Stage::Parse, parse),
    (Stage::ScanDetect, scan_detect),
    (Stage::Layout, layout),
    (Stage::ParagraphFind, paragraph_find),
    (Stage::StylesAndFormulas, styles_and_formulas),
    (Stage::ExtractTerms, extract_terms),
    (Stage::Translate, translate),
    (Stage::Typeset, typeset),
    (Stage::FontEmbed, font_embed),
    (Stage::Write, write),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationResult {
    pub output: String,
    pub pages: usize,
    pub warnings: usize,
    pub appended_bytes: usize,
}

pub fn run(document: &mut Document, context: &PassContext<'_>) -> Result<TranslationResult> {
    let result = run_inner(document, context);
    match &result {
        Ok(result) => context.events.emit(Event::new(EventKind::Result {
            output: result.output.clone(),
            pages: result.pages,
            warnings: result.warnings,
        })),
        Err(error) => context
            .events
            .emit(Event::new(EventKind::from_error(error))),
    }
    result
}

fn run_inner(document: &mut Document, context: &PassContext<'_>) -> Result<TranslationResult> {
    for (stage, pass) in PIPELINE {
        context
            .events
            .emit(Event::new(EventKind::StageStarted { stage }));
        pass(document, context)?;
        context
            .events
            .emit(Event::new(EventKind::StageFinished { stage }));
    }
    let write_report = document.write_report.as_ref().ok_or_else(|| {
        MimusError::input(ErrorReason::OutputWrite, "write pass produced no report")
    })?;
    Ok(TranslationResult {
        output: document.output_path().to_string_lossy().into_owned(),
        pages: document.il.pages.len(),
        warnings: document.diagnostics.total_count(),
        appended_bytes: write_report.appended_bytes,
    })
}

pub fn parse(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let bytes = std::fs::read(document.input_path()).map_err(|error| {
        MimusError::input(
            ErrorReason::InputRead,
            format!(
                "could not read {}: {error}",
                document.input_path().display()
            ),
        )
    })?;
    let pdf = match LopdfDocument::load_mem(&bytes) {
        Ok(value) => value,
        Err(lopdf::Error::InvalidPassword) => return Err(encrypted_pdf_error()),
        Err(error) => {
            return Err(MimusError::input(
                ErrorReason::PdfParse,
                format!(
                    "could not parse {}: {error}",
                    document.input_path().display()
                ),
            ));
        }
    };
    if pdf.was_encrypted() {
        return Err(encrypted_pdf_error());
    }
    let lopdf_pages = pdf.get_pages().into_values().collect::<Vec<_>>();
    let engine_pages = context.engine.page_count(&bytes)?;
    if lopdf_pages.len() != engine_pages {
        return Err(MimusError::input(
            ErrorReason::EngineMismatch,
            format!(
                "lopdf found {} pages but the inspection engine found {engine_pages}",
                lopdf_pages.len()
            ),
        ));
    }
    let mut extracted_pages = Vec::with_capacity(engine_pages);
    for (index, page_id) in lopdf_pages.into_iter().enumerate() {
        let geometry = context.engine.page_geometry(&bytes, index)?;
        let walked_characters = walk_page(&pdf, page_id)?;
        let engine_characters = context.engine.page_characters(&bytes, index)?;
        validate_character_alignment(
            index,
            &walked_characters,
            &engine_characters,
            context.config.baseline_tolerance_pt,
        )?;
        extracted_pages.push(ExtractedPage {
            index,
            geometry,
            walked_characters,
            engine_characters,
            layout_regions: Vec::new(),
        });
        context.events.emit(Event::new(EventKind::PageProgress {
            stage: Stage::Parse,
            page: index + 1,
            total_pages: engine_pages,
        }));
    }
    document.original_bytes = bytes;
    document.pdf = Some(pdf);
    document.extracted_pages = extracted_pages;
    Ok(())
}

pub fn scan_detect(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let characters = document
        .extracted_pages
        .iter()
        .map(|page| page.walked_characters.len())
        .sum::<usize>();
    if characters == 0 {
        return Err(MimusError::input(
            ErrorReason::ScannedPdf,
            "the PDF has no native text; scanned PDFs are not supported in V1",
        ));
    }
    Ok(())
}

pub fn layout(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let total_pages = document.extracted_pages.len();
    for page in &mut document.extracted_pages {
        let raster = context
            .engine
            .rasterize_page(&document.original_bytes, page.index)?;
        page.layout_regions =
            context
                .layout_detector
                .detect(page.geometry, &raster, &page.engine_characters)?;
        context.events.emit(Event::new(EventKind::PageProgress {
            stage: Stage::Layout,
            page: page.index + 1,
            total_pages,
        }));
    }
    Ok(())
}

pub fn paragraph_find(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let mut pages = Vec::with_capacity(document.extracted_pages.len());
    for extracted in &document.extracted_pages {
        if extracted.layout_regions.len() != 1 {
            return Err(MimusError::input(
                ErrorReason::UnsupportedPdf,
                format!(
                    "M1 supports exactly one detected line per page; page {} has {} regions",
                    extracted.index + 1,
                    extracted.layout_regions.len()
                ),
            ));
        }
        let region = extracted.layout_regions[0];
        let chars = extracted
            .walked_characters
            .iter()
            .zip(&extracted.engine_characters)
            .map(|(walked, engine)| Char {
                unicode: walked.unicode,
                code: engine.code,
                font: walked.font.clone(),
                font_size: walked.font_size,
                baseline_origin: walked.baseline_origin,
                r#box: walked.metric_box,
                visual_bbox: engine.tight_box,
                text_transform: walked.text_transform,
                passthrough: PassthroughRef {
                    content_object: walked.content_object.0,
                    byte_start: walked.byte_start,
                    byte_end: walked.byte_end,
                    encoded: walked.encoded.clone(),
                },
            })
            .collect();
        pages.push(il::Page {
            index: extracted.index,
            geometry: extracted.geometry,
            paragraphs: vec![Paragraph {
                reading_order: region.reading_order,
                bounds: region.bounds,
                text: TextCarrier::Chars { chars },
                translated_text: None,
            }],
        });
    }
    document.il = il::Document {
        schema_version: il::SCHEMA_VERSION,
        pages,
    };
    Ok(())
}

pub fn styles_and_formulas(_document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    Ok(())
}

pub fn extract_terms(_document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    Ok(())
}

pub fn translate(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    for page in &mut document.il.pages {
        for paragraph in &mut page.paragraphs {
            let source = paragraph.source_text();
            paragraph.translated_text = Some(context.translator.translate(&source)?);
        }
    }
    Ok(())
}

pub fn typeset(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let mut rewrites = Vec::with_capacity(document.il.pages.len());
    for page in &document.il.pages {
        let mut content = Vec::new();
        let mut fonts = BTreeSet::new();
        for paragraph in &page.paragraphs {
            let source = paragraph.source_text();
            if paragraph.translated_text.as_deref() != Some(source.as_str()) {
                return Err(MimusError::input(
                    ErrorReason::UnsupportedPdf,
                    "M1 can typeset only the identity output from --backend none",
                ));
            }
            let chars = paragraph.chars();
            let first = chars.first().ok_or_else(|| {
                MimusError::input(ErrorReason::UnsupportedPdf, "cannot typeset an empty line")
            })?;
            if chars.iter().any(|value| {
                value.font != first.font
                    || (value.font_size - first.font_size).abs() > f64::EPSILON
                    || value.text_transform != TextTransform::Upright
            }) {
                return Err(MimusError::input(
                    ErrorReason::UnsupportedPdf,
                    "M1 single-line typesetting requires one upright font run",
                ));
            }
            fonts.insert(first.font.clone());
            let encoded = chars
                .iter()
                .flat_map(|value| value.passthrough.encoded.iter().copied())
                .collect::<Vec<_>>();
            let program = format!(
                "BT\n/{} {} Tf\n1 0 0 1 {} {} Tm\n({}) Tj\nET\n",
                pdf_name(&first.font.resource_name),
                pdf_number(first.font_size),
                pdf_number(first.baseline_origin.x),
                pdf_number(first.baseline_origin.y),
                pdf_literal(&encoded),
            );
            content.extend_from_slice(program.as_bytes());
        }
        rewrites.push(PageRewrite {
            page_index: page.index,
            content,
            reused_fonts: fonts.into_iter().collect(),
            needs_new_font: false,
        });
    }
    document.rewrites = rewrites;
    Ok(())
}

pub fn font_embed(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    if document.rewrites.iter().any(|value| value.needs_new_font) {
        return Err(MimusError::asset(
            ErrorReason::UnsupportedPdf,
            "embedding a new font is deferred to issue #22",
        ));
    }
    if document
        .rewrites
        .iter()
        .any(|value| value.reused_fonts.is_empty())
    {
        return Err(MimusError::input(
            ErrorReason::OutputWrite,
            "FontEmbed found a rewrite with no reusable input font",
        ));
    }
    Ok(())
}

pub fn write(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let pdf = document
        .pdf
        .as_ref()
        .ok_or_else(|| MimusError::input(ErrorReason::OutputWrite, "Parse did not retain a PDF"))?;
    document.write_report = Some(incremental_publish(
        &document.original_bytes,
        pdf,
        &document.rewrites,
        document.output_path(),
    )?);
    Ok(())
}

fn validate_character_alignment(
    page_index: usize,
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
) -> Result<()> {
    if walked.len() != engine.len() {
        return Err(MimusError::input(
            ErrorReason::EngineMismatch,
            format!(
                "page {} operator walk found {} characters but PDFium found {}",
                page_index + 1,
                walked.len(),
                engine.len()
            ),
        ));
    }
    for (index, (walked, engine)) in walked.iter().zip(engine).enumerate() {
        if walked.unicode != engine.unicode {
            return Err(MimusError::input(
                ErrorReason::EngineMismatch,
                format!(
                    "page {} character {} differs between operator walk and PDFium",
                    page_index + 1,
                    index
                ),
            ));
        }
        let delta_x = (walked.baseline_origin.x - engine.baseline_origin.x).abs();
        let delta_y = (walked.baseline_origin.y - engine.baseline_origin.y).abs();
        if delta_x > tolerance || delta_y > tolerance {
            return Err(MimusError::input(
                ErrorReason::EngineMismatch,
                format!(
                    "page {} character {} baseline differs by ({delta_x:.6}, {delta_y:.6}) pt",
                    page_index + 1,
                    index
                ),
            ));
        }
    }
    Ok(())
}

fn encrypted_pdf_error() -> MimusError {
    MimusError::input(
        ErrorReason::EncryptedPdf,
        "encrypted PDFs are not supported in V1",
    )
    .with_hint("decrypt the input with qpdf, then retry")
}

fn pdf_number(value: f64) -> String {
    if value.fract().abs() < 1e-9 {
        return format!("{value:.0}");
    }
    let mut value = format!("{value:.6}");
    while value.ends_with('0') {
        value.pop();
    }
    value
}

fn pdf_name(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') {
            output.push(char::from(byte));
        } else {
            output.push_str(&format!("#{byte:02X}"));
        }
    }
    output
}

fn pdf_literal(value: &[u8]) -> String {
    let mut output = String::new();
    for byte in value {
        match byte {
            b'(' | b')' | b'\\' => {
                output.push('\\');
                output.push(char::from(*byte));
            }
            0..=31 | 127..=255 => output.push_str(&format!("\\{:03o}", byte)),
            _ => output.push(char::from(*byte)),
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::engine::{
        PageCharSnapshot, PdfInspector, Rasterizer, RgbaImage, SingleLineLayoutDetector,
    };
    use crate::event::{EventKind, RecordingEventSink};
    use crate::il::{PageGeometry, Point, Rect};
    use crate::translate::Translator;

    use super::*;

    #[test]
    fn pass_order_is_the_decided_order() {
        assert_eq!(
            ORDER,
            [
                Stage::Parse,
                Stage::ScanDetect,
                Stage::Layout,
                Stage::ParagraphFind,
                Stage::StylesAndFormulas,
                Stage::ExtractTerms,
                Stage::Translate,
                Stage::Typeset,
                Stage::FontEmbed,
                Stage::Write,
            ]
        );
    }

    #[derive(Default)]
    struct FakeEngine {
        raster_calls: AtomicUsize,
    }

    impl PdfInspector for FakeEngine {
        fn page_count(&self, _pdf: &[u8]) -> Result<usize> {
            Ok(1)
        }

        fn page_geometry(&self, _pdf: &[u8], page_index: usize) -> Result<PageGeometry> {
            assert_eq!(page_index, 0);
            Ok(PageGeometry {
                width: 300.0,
                height: 200.0,
                rotate_degrees: 0,
            })
        }

        fn page_characters(&self, _pdf: &[u8], page_index: usize) -> Result<Vec<PageCharSnapshot>> {
            assert_eq!(page_index, 0);
            let origins = [72.0, 82.356, 85.896, 96.252, 105.036];
            Ok("MIMUS"
                .chars()
                .zip(origins)
                .enumerate()
                .map(|(index, (unicode, x))| PageCharSnapshot {
                    index: index as u32,
                    unicode: Some(unicode),
                    code: unicode.into(),
                    baseline_origin: Point { x, y: 120.0 },
                    tight_box: Rect {
                        left: x,
                        bottom: 119.0,
                        right: x + 4.0,
                        top: 129.0,
                    },
                    loose_box: Rect {
                        left: x,
                        bottom: 117.168,
                        right: x + 4.0,
                        top: 131.136,
                    },
                })
                .collect())
        }
    }

    impl Rasterizer for FakeEngine {
        fn rasterize_page(&self, _pdf: &[u8], page_index: usize) -> Result<RgbaImage> {
            assert_eq!(page_index, 0);
            self.raster_calls.fetch_add(1, Ordering::SeqCst);
            Ok(RgbaImage {
                width: 300,
                height: 200,
                rgba8: vec![255; 300 * 200 * 4],
            })
        }
    }

    #[derive(Default)]
    struct CountingTranslator {
        calls: AtomicUsize,
    }

    impl Translator for CountingTranslator {
        fn translate(&self, text: &str) -> Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(text.to_owned())
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
    }

    #[test]
    fn fixed_pipeline_runs_every_seam_and_emits_one_terminal_event() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("roundtrip.pdf");
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine::default();
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            config: crate::context::PipelineConfig::default(),
        };
        let result = run(&mut document, &context).unwrap();
        assert_eq!(result.pages, 1);
        assert!(result.appended_bytes > 0);
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 1);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            document.il.pages[0].paragraphs[0]
                .translated_text
                .as_deref(),
            Some("MIMUS")
        );
        let input = std::fs::read(fixture()).unwrap();
        assert!(std::fs::read(output).unwrap().starts_with(&input));

        let events = events.events();
        let started = events
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::StageStarted { stage } => Some(stage),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(started, ORDER);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind.is_terminal())
                .count(),
            1
        );
        assert!(matches!(
            events.last().unwrap().kind,
            EventKind::Result { .. }
        ));
    }
}
