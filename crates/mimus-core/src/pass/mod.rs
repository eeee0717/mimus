use std::collections::BTreeSet;

use lopdf::Document as LopdfDocument;

use crate::context::{Document, ExtractedPage, PassContext};
use crate::engine::{PageCharSnapshot, RgbaImage};
use crate::error::{InputReason, InternalReason, IoReason, MimusError, Result};
use crate::event::{Diagnostic, Diagnostics, Event, EventKind, Stage};
use crate::il::{
    self, Char, PageGeometry, Paragraph, PassthroughRef, Rect, TextCarrier, TextTransform,
};
use crate::walk::walk_page;
use crate::write::{PageRewrite, build_incremental, publish};

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

pub const INSPECT_STAGE_COUNT: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationResult {
    pub output: String,
    pub pages: usize,
    pub warnings: usize,
    pub appended_bytes: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InspectionResult {
    pub il: il::Document,
    pub pages: usize,
    pub warnings: usize,
}

pub fn run(document: &mut Document, context: &PassContext<'_>) -> Result<TranslationResult> {
    run_stages(document, context, &PIPELINE)?;
    let output = document.output_path().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "translation document has no output path",
        )
    })?;
    let write_report = document.write_report.as_ref().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "write pass produced no report",
        )
    })?;
    Ok(TranslationResult {
        output: output.to_string_lossy().into_owned(),
        pages: document.il.pages.len(),
        warnings: document.diagnostics.total_count(),
        appended_bytes: write_report.appended_bytes,
    })
}

pub fn inspect(document: &mut Document, context: &PassContext<'_>) -> Result<InspectionResult> {
    run_stages(document, context, &PIPELINE[..INSPECT_STAGE_COUNT])?;
    let il = il::snapshot(&document.il);
    Ok(InspectionResult {
        pages: il.pages.len(),
        warnings: document.diagnostics.total_count(),
        il,
    })
}

fn run_stages(
    document: &mut Document,
    context: &PassContext<'_>,
    stages: &[(Stage, Pass)],
) -> Result<()> {
    for (pass_index, &(stage, pass)) in stages.iter().enumerate() {
        context
            .events
            .emit(Event::new(EventKind::StageStarted { stage }))?;
        pass(document, context)?;
        if let Some(snapshots) = context.snapshots {
            let snapshot = il::snapshot(&document.il);
            snapshots.write_snapshot(pass_index, stage, &snapshot)?;
        }
        context
            .events
            .emit(Event::new(EventKind::StageFinished { stage }))?;
    }
    Ok(())
}

pub fn parse(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let bytes = std::fs::read(document.input_path()).map_err(|error| {
        MimusError::io(
            IoReason::InputRead,
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
                InputReason::PdfParse,
                format!(
                    "could not parse {}: {error}",
                    document.input_path().display()
                ),
            ));
        }
    };
    // ADR-0009: lopdf 会用空密码透明解密并移除 /Encrypt；is_encrypted() 此时为
    // false，只有 was_encrypted() 能阻止这类文档被静默放行。
    if pdf.was_encrypted() {
        return Err(encrypted_pdf_error());
    }
    let lopdf_pages = pdf.get_pages().into_values().collect::<Vec<_>>();
    let engine_pages = context.engine.page_count(&bytes)?;
    if lopdf_pages.len() != engine_pages {
        return Err(MimusError::input(
            InputReason::EngineMismatch,
            format!(
                "lopdf found {} pages but the inspection engine found {engine_pages}",
                lopdf_pages.len()
            ),
        ));
    }
    let mut extracted_pages = Vec::with_capacity(engine_pages);
    for (index, page_id) in lopdf_pages.into_iter().enumerate() {
        let geometry = context.engine.page_geometry(&bytes, index)?;
        // CONTEXT #32 要求在视觉页框内判定朝向。#14 尚未实现这层坐标变换，
        // 因此先 fail-closed，避免把 /Rotate 90 的整页文字误判后静默重排。
        if geometry.rotate_degrees != 0 {
            return Err(MimusError::input(
                InputReason::UnsupportedPdf,
                format!(
                    "M1 does not yet support page /Rotate {}; page {} was not written",
                    geometry.rotate_degrees,
                    index + 1
                ),
            ));
        }
        let walked_characters = walk_page(&pdf, page_id)?;
        let engine_characters = context.engine.page_characters(&bytes, index)?;
        validate_character_alignment(
            index,
            &walked_characters,
            &engine_characters,
            context.config.baseline_tolerance_pt,
            &mut document.diagnostics,
        )?;
        extracted_pages.push(ExtractedPage {
            index,
            geometry,
            walked_characters,
            engine_characters,
            layout_regions: Vec::new(),
            input_raster: None,
        });
        context.events.emit(Event::new(EventKind::PageProgress {
            stage: Stage::Parse,
            page_index: index,
            total_pages: engine_pages,
        }))?;
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
            InputReason::ScannedPdf,
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
        raster.validate()?;
        page.layout_regions =
            context
                .layout_detector
                .detect(page.geometry, &raster, &page.engine_characters)?;
        page.input_raster = Some(raster);
        context.events.emit(Event::new(EventKind::PageProgress {
            stage: Stage::Layout,
            page_index: page.index,
            total_pages,
        }))?;
    }
    Ok(())
}

pub fn paragraph_find(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let mut pages = Vec::with_capacity(document.extracted_pages.len());
    for extracted in &document.extracted_pages {
        if extracted.layout_regions.len() != 1 {
            return Err(MimusError::input(
                InputReason::UnsupportedPdf,
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
                code: walked.code,
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
                    InputReason::UnsupportedPdf,
                    "M1 can typeset only the identity output from --backend none",
                ));
            }
            let chars = paragraph.chars();
            let first = chars.first().ok_or_else(|| {
                MimusError::input(InputReason::UnsupportedPdf, "cannot typeset an empty line")
            })?;
            if chars.iter().any(|value| {
                value.font != first.font
                    || (value.font_size - first.font_size).abs() > f64::EPSILON
                    || value.text_transform != TextTransform::Upright
            }) {
                return Err(MimusError::input(
                    InputReason::UnsupportedPdf,
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
        return Err(MimusError::input(
            InputReason::UnsupportedPdf,
            "embedding a new font is deferred to issue #22",
        ));
    }
    if document
        .rewrites
        .iter()
        .any(|value| value.reused_fonts.is_empty())
    {
        return Err(MimusError::input(
            InputReason::UnsupportedPdf,
            "FontEmbed found a rewrite with no reusable input font",
        ));
    }
    Ok(())
}

pub fn write(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let pdf = document.pdf.as_ref().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "Parse did not retain a PDF",
        )
    })?;
    let output_path = document.output_path().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "Write received a document with no output path",
        )
    })?;
    let (candidate, report) = build_incremental(&document.original_bytes, pdf, &document.rewrites)?;
    validate_output_roundtrip(document, context, &candidate)?;
    publish(output_path, &candidate)?;
    document.write_report = Some(report);
    Ok(())
}

fn validate_output_roundtrip(
    document: &Document,
    context: &PassContext<'_>,
    candidate: &[u8],
) -> Result<()> {
    let page_count = context
        .engine
        .page_count(candidate)
        .map_err(|error| output_mismatch(format!("inspection engine rejected output: {error}")))?;
    if page_count != document.extracted_pages.len() {
        return Err(output_mismatch(format!(
            "output has {page_count} pages; input had {}",
            document.extracted_pages.len()
        )));
    }

    for expected in &document.extracted_pages {
        let geometry = context
            .engine
            .page_geometry(candidate, expected.index)
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} geometry: {error}",
                    expected.index + 1
                ))
            })?;
        validate_output_geometry(expected.index, expected.geometry, geometry)?;
        let characters = context
            .engine
            .page_characters(candidate, expected.index)
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} text: {error}",
                    expected.index + 1
                ))
            })?;
        validate_output_characters(
            expected.index,
            &expected.engine_characters,
            &characters,
            context.config.baseline_tolerance_pt,
        )?;
        let raster = context
            .engine
            .rasterize_page(candidate, expected.index)
            .map_err(|error| {
                output_mismatch(format!(
                    "inspection engine rejected output page {} raster: {error}",
                    expected.index + 1
                ))
            })?;
        raster.validate().map_err(|error| {
            output_mismatch(format!(
                "inspection engine returned an invalid output page {} raster: {error}",
                expected.index + 1
            ))
        })?;
        let input_raster = expected.input_raster.as_ref().ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                "Layout did not retain its input raster",
            )
        })?;
        input_raster.validate()?;
        validate_output_raster(expected.index, input_raster, &raster)?;
    }
    Ok(())
}

fn validate_output_geometry(
    page_index: usize,
    expected: PageGeometry,
    actual: PageGeometry,
) -> Result<()> {
    if expected.rotate_degrees != actual.rotate_degrees
        || !finite_close(expected.width, actual.width, 0.001)
        || !finite_close(expected.height, actual.height, 0.001)
    {
        return Err(output_mismatch(format!(
            "output page {} geometry differs from the input",
            page_index + 1
        )));
    }
    Ok(())
}

fn validate_output_characters(
    page_index: usize,
    expected: &[PageCharSnapshot],
    actual: &[PageCharSnapshot],
    tolerance: f64,
) -> Result<()> {
    if expected.len() != actual.len() {
        return Err(output_mismatch(format!(
            "output page {} has {} characters; input had {}",
            page_index + 1,
            actual.len(),
            expected.len()
        )));
    }
    for (index, (expected, actual)) in expected.iter().zip(actual).enumerate() {
        if expected.index != actual.index
            || expected.unicode != actual.unicode
            || expected.unicode_value != actual.unicode_value
            || !point_close(expected.baseline_origin, actual.baseline_origin, tolerance)
            || !rect_close(expected.tight_box, actual.tight_box, tolerance)
            || !rect_close(expected.loose_box, actual.loose_box, tolerance)
        {
            return Err(output_mismatch(format!(
                "output page {} character {} differs from the input snapshot",
                page_index + 1,
                index
            )));
        }
    }
    Ok(())
}

fn validate_output_raster(
    page_index: usize,
    expected: &RgbaImage,
    actual: &RgbaImage,
) -> Result<()> {
    if expected != actual {
        return Err(output_mismatch(format!(
            "output page {} pixels differ from the input",
            page_index + 1
        )));
    }
    Ok(())
}

fn point_close(expected: il::Point, actual: il::Point, tolerance: f64) -> bool {
    finite_close(expected.x, actual.x, tolerance) && finite_close(expected.y, actual.y, tolerance)
}

fn rect_close(expected: Rect, actual: Rect, tolerance: f64) -> bool {
    finite_close(expected.left, actual.left, tolerance)
        && finite_close(expected.bottom, actual.bottom, tolerance)
        && finite_close(expected.right, actual.right, tolerance)
        && finite_close(expected.top, actual.top, tolerance)
}

fn finite_close(expected: f64, actual: f64, tolerance: f64) -> bool {
    expected.is_finite()
        && actual.is_finite()
        && tolerance.is_finite()
        && tolerance >= 0.0
        && (expected - actual).abs() <= tolerance
}

fn output_mismatch(message: impl Into<String>) -> MimusError {
    MimusError::internal(InternalReason::OutputMismatch, message)
}

fn validate_character_alignment(
    page_index: usize,
    walked: &[crate::walk::WalkedChar],
    engine: &[crate::engine::PageCharSnapshot],
    tolerance: f64,
    diagnostics: &mut Diagnostics,
) -> Result<()> {
    if walked.len() != engine.len() {
        return Err(MimusError::input(
            InputReason::EngineMismatch,
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
                InputReason::EngineMismatch,
                format!(
                    "page {} character {} differs between operator walk and PDFium",
                    page_index + 1,
                    index
                ),
            ));
        }
        let delta_x = (walked.baseline_origin.x - engine.baseline_origin.x).abs();
        let delta_y = (walked.baseline_origin.y - engine.baseline_origin.y).abs();
        if !walked.baseline_origin.x.is_finite()
            || !walked.baseline_origin.y.is_finite()
            || !engine.baseline_origin.x.is_finite()
            || !engine.baseline_origin.y.is_finite()
            || !tolerance.is_finite()
            || tolerance < 0.0
        {
            return Err(MimusError::input(
                InputReason::EngineMismatch,
                format!(
                    "page {} character {} has a non-finite baseline or tolerance",
                    page_index + 1,
                    index
                ),
            ));
        }
        if delta_x > tolerance || delta_y > tolerance {
            // CONTEXT #35: PDFium 是交叉证据而非事实层；有限的 baseline 分歧记为
            // 稳定 diagnostic，最终候选仍必须通过 Write 前的字符与像素往返验证。
            diagnostics.push(Diagnostic::EngineBaselineMismatch {
                page_index,
                character_index: index,
                delta_x_pt: delta_x,
                delta_y_pt: delta_y,
            });
        }
    }
    Ok(())
}

fn encrypted_pdf_error() -> MimusError {
    MimusError::input(
        InputReason::EncryptedPdf,
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
    use crate::event::{DiagnosticId, EventKind, RecordingEventSink};
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
        corrupt_raster_after_bytes: Option<usize>,
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
                    unicode_value: unicode.into(),
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
        fn rasterize_page(&self, pdf: &[u8], page_index: usize) -> Result<RgbaImage> {
            assert_eq!(page_index, 0);
            self.raster_calls.fetch_add(1, Ordering::SeqCst);
            let mut rgba8 = vec![255; 300 * 200 * 4];
            if self
                .corrupt_raster_after_bytes
                .is_some_and(|threshold| pdf.len() > threshold)
            {
                rgba8[0] = 0;
            }
            RgbaImage::new(300, 200, rgba8)
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

    struct NonIdentityTranslator;

    impl Translator for NonIdentityTranslator {
        fn translate(&self, text: &str) -> Result<String> {
            Ok(format!("{text}!"))
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
    }

    #[test]
    fn fixed_pipeline_runs_every_stage_without_owning_the_terminal_event() {
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
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        let result = run(&mut document, &context).unwrap();
        assert_eq!(result.pages, 1);
        assert!(result.appended_bytes > 0);
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 2);
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
        assert!(events.iter().all(|event| !event.kind.is_terminal()));
    }

    #[test]
    fn non_identity_translator_stub_exercises_the_typeset_guard() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("must-not-exist.pdf");
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &NonIdentityTranslator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };

        let error = run(&mut document, &context).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::UnsupportedPdf)
        );
        assert!(
            error
                .to_string()
                .contains("only the identity output from --backend none")
        );
        assert!(!output.exists());
        assert!(document.rewrites.is_empty());
    }

    #[test]
    fn output_validation_failure_preserves_an_existing_destination() {
        let input = std::fs::read(fixture()).unwrap();
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("existing.pdf");
        std::fs::write(&output, b"existing").unwrap();
        let mut document = Document::new(fixture(), &output);
        let engine = FakeEngine {
            raster_calls: AtomicUsize::new(0),
            corrupt_raster_after_bytes: Some(input.len()),
        };
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };

        let error = run(&mut document, &context).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
        assert_eq!(std::fs::read(&output).unwrap(), b"existing");
        assert_eq!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .count(),
            1
        );
    }

    #[test]
    fn output_validation_compares_pdfium_character_indices() {
        let expected = FakeEngine::default().page_characters(&[], 0).unwrap();
        let mut actual = expected.clone();
        actual[0].index += 1;

        let error = validate_output_characters(0, &expected, &actual, 0.001).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Internal(InternalReason::OutputMismatch)
        );
    }

    #[test]
    fn finite_pdfium_baseline_differences_become_bounded_diagnostics() {
        let pdf = LopdfDocument::load(fixture()).unwrap();
        let page_id = pdf.get_pages()[&1];
        let walked = walk_page(&pdf, page_id).unwrap();
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine[0].baseline_origin.x += 0.01;
        let mut diagnostics = Diagnostics::default();

        validate_character_alignment(0, &walked, &engine, 0.001, &mut diagnostics).unwrap();
        assert_eq!(diagnostics.entries().len(), 1);
        assert_eq!(
            diagnostics.entries()[0].id(),
            DiagnosticId::EngineBaselineMismatch
        );

        engine[0].baseline_origin.x = f64::NAN;
        assert!(
            validate_character_alignment(0, &walked, &engine, 0.001, &mut diagnostics).is_err()
        );
    }

    #[test]
    fn font_embed_deferred_paths_are_typed_as_unsupported_input() {
        let engine = FakeEngine::default();
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: None,
            config: crate::context::PipelineConfig::default(),
        };
        let mut document = Document::new(fixture(), "unused.pdf");
        document.rewrites = vec![PageRewrite {
            page_index: 0,
            content: Vec::new(),
            reused_fonts: Vec::new(),
            needs_new_font: true,
        }];
        let error = font_embed(&mut document, &context).unwrap_err();
        assert_eq!(error.category().code(), 2);
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::UnsupportedPdf)
        );

        document.rewrites[0].needs_new_font = false;
        let error = font_embed(&mut document, &context).unwrap_err();
        assert_eq!(error.category().code(), 2);
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::UnsupportedPdf)
        );
    }
}
