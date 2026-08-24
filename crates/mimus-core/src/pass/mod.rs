use std::collections::{BTreeMap, BTreeSet};

use lopdf::Document as LopdfDocument;

use crate::context::{Document, ExtractedPage, PassContext};
use crate::engine::{PageCharSnapshot, RgbaImage};
use crate::error::{ErrorReason, InputReason, InternalReason, IoReason, MimusError, Result};
use crate::event::{
    Diagnostic, Diagnostics, Event, EventKind, PageDegradeReason, PreservedParagraph, Stage,
};
use crate::il::{
    self, Char, PageGeometry, Paragraph, PassthroughRef, Rect, TextCarrier, TextTransform,
};
use crate::scan::{PageClass, prescan_page};
use crate::walk::walk_page;
use crate::write::{ContentSpanReplacement, PageRewrite, build_incremental, publish};

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
    push_degradation_summary(document);
    Ok(())
}

/// ADR-0013 §5：受影响页与保留段的总账走单条汇总 diagnostic，不进 `result`
/// （ADR-0011 §2 规定 result 只保留 warnings 总数，不重复诊断内容）。
fn push_degradation_summary(document: &mut Document) {
    let degraded_page_indices = document
        .extracted_pages
        .iter()
        .filter(|page| page.degraded.is_some())
        .map(|page| page.index)
        .collect::<Vec<_>>();
    let preserved_paragraphs = document
        .il
        .pages
        .iter()
        .flat_map(|page| {
            page.paragraphs
                .iter()
                .enumerate()
                .filter_map(move |(paragraph_index, paragraph)| {
                    paragraph.preserved.map(|reason| PreservedParagraph {
                        page_index: page.index,
                        paragraph_index,
                        reason,
                    })
                })
        })
        .collect::<Vec<_>>();
    if degraded_page_indices.is_empty() && preserved_paragraphs.is_empty() {
        return;
    }
    document.diagnostics.push(Diagnostic::DegradationSummary {
        degraded_pages: degraded_page_indices.len(),
        degraded_page_indices,
        preserved_paragraphs,
        total_pages: document.extracted_pages.len(),
    });
}

/// 把一页标成降级并同时记一条诊断。两件事必须一起发生：只置位不报告就是静默
/// 失败，只报告不置位则后续 pass 仍会尝试改写这一页。
fn degrade_page(
    page: &mut ExtractedPage,
    diagnostics: &mut Diagnostics,
    reason: PageDegradeReason,
) {
    page.degraded = Some(reason);
    diagnostics.push(Diagnostic::PageDegraded {
        page_index: page.index,
        reason,
    });
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
    // ADR-0009: 空密码文件会被 lopdf 透明解密，只能由 was_encrypted() 识别；
    // 非空密码文件则可能保持 /Encrypt 后成功返回，只能由 is_encrypted() 识别。
    if pdf.was_encrypted() || pdf.is_encrypted() {
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
        let evidence = prescan_page(&pdf, page_id);
        extracted_pages.push(ExtractedPage {
            index,
            page_id,
            geometry,
            evidence,
            class: None,
            degraded: None,
            walked_characters: Vec::new(),
            content_streams: Vec::new(),
            engine_characters: Vec::new(),
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

pub fn scan_detect(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let total_pages = document.extracted_pages.len();
    let mut scanned_page_indices = Vec::new();
    let mut blank_pages = 0usize;
    for page in &mut document.extracted_pages {
        let class = page.evidence.classify();
        page.class = Some(class);
        match class {
            PageClass::Blank => blank_pages += 1,
            PageClass::Scanned => scanned_page_indices.push(page.index),
            PageClass::Content => {}
        }
    }
    let scanned_pages = scanned_page_indices.len();
    let content_pages = total_pages - blank_pages;
    if scanned_pages > 0 {
        document.diagnostics.push(Diagnostic::ScanSummary {
            scanned_page_indices,
            scanned_pages,
            blank_pages,
            content_pages,
            total_pages,
        });
    }
    if content_pages > 0 && scanned_pages * 5 >= content_pages * 4 {
        return Err(MimusError::input(
            InputReason::ScannedPdf,
            format!("{scanned_pages} of {content_pages} content pages are scanned"),
        )
        .with_hint("V1 does not support OCR for scanned PDFs")
        .with_scan_counts(scanned_pages, total_pages));
    }

    let pdf = document.pdf.as_ref().ok_or_else(|| {
        MimusError::internal(
            InternalReason::InvariantViolation,
            "Parse did not retain a PDF",
        )
    })?;
    for page in &mut document.extracted_pages {
        if !page.is_translatable() {
            continue;
        }
        // CONTEXT #32 要求在视觉页框内判定朝向。#14 尚未实现这层坐标变换，
        // 因此旋转页降级透传；透传页不需要重放坐标。
        if page.geometry.rotate_degrees != 0 {
            degrade_page(
                page,
                &mut document.diagnostics,
                PageDegradeReason::UnsupportedRotation,
            );
            continue;
        }
        match walk_page(pdf, page.page_id) {
            Ok(walked) => {
                for recovery in walked.recoveries {
                    document.diagnostics.push(Diagnostic::ContentRecovered {
                        page_index: page.index,
                        recovery,
                    });
                }
                page.walked_characters = walked.characters;
                page.content_streams = walked.content_streams;
            }
            // ADR-0013 §3：内容流的语法错误只毁掉它所在的那一页，整篇不该跟着失败。
            // 能力边界（UnsupportedPdf）仍是文档级失败——那不是这一页坏了，
            // 而是 M1 还不会处理这类内容，降级会把「没实现」伪装成「文件有问题」。
            Err(error) if error.reason() == ErrorReason::Input(InputReason::OperatorWalk) => {
                degrade_page(
                    page,
                    &mut document.diagnostics,
                    PageDegradeReason::ContentStreamSyntax,
                );
                continue;
            }
            Err(error) => return Err(error),
        }
        page.engine_characters = context
            .engine
            .page_characters(&document.original_bytes, page.index)?;
        validate_character_alignment(
            page.index,
            &page.walked_characters,
            &page.engine_characters,
            context.config.baseline_tolerance_pt,
            &mut document.diagnostics,
        )?;
    }
    Ok(())
}

pub fn layout(document: &mut Document, context: &PassContext<'_>) -> Result<()> {
    let total_pages = document.extracted_pages.len();
    for page in &mut document.extracted_pages {
        if page.is_translatable() {
            let raster = context
                .engine
                .rasterize_page(&document.original_bytes, page.index)?;
            raster.validate()?;
            page.layout_regions =
                context
                    .layout_detector
                    .detect(page.geometry, &raster, &page.engine_characters)?;
            page.input_raster = Some(raster);
        }
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
        if !extracted.is_translatable() {
            pages.push(il::Page {
                index: extracted.index,
                geometry: extracted.geometry,
                paragraphs: Vec::new(),
            });
            continue;
        }
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
                preserved: None,
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
            if paragraph.preserved.is_some() {
                paragraph.translated_text = None;
                continue;
            }
            let source = paragraph.source_text();
            paragraph.translated_text = Some(context.translator.translate(&source)?);
        }
    }
    Ok(())
}

pub fn typeset(document: &mut Document, _context: &PassContext<'_>) -> Result<()> {
    let mut rewrites = Vec::with_capacity(document.il.pages.len());
    for page in &document.il.pages {
        if page.paragraphs.is_empty() {
            continue;
        }
        let extracted = document.extracted_pages.get(page.index).ok_or_else(|| {
            MimusError::internal(
                InternalReason::InvariantViolation,
                format!("Typeset could not find extracted page {}", page.index),
            )
        })?;
        if extracted.index != page.index {
            return Err(MimusError::internal(
                InternalReason::InvariantViolation,
                format!(
                    "Typeset page index {} points at extracted page {}",
                    page.index, extracted.index
                ),
            ));
        }
        let streams = extracted
            .content_streams
            .iter()
            .map(|stream| (stream.object_id, stream.decoded.as_slice()))
            .collect::<BTreeMap<_, _>>();
        let mut span_is_replaceable = BTreeMap::<(lopdf::ObjectId, usize, usize), bool>::new();
        for paragraph in &page.paragraphs {
            if paragraph.preserved.is_some() {
                if paragraph.translated_text.is_some() {
                    return Err(MimusError::internal(
                        InternalReason::InvariantViolation,
                        "a preserved paragraph has translated text",
                    ));
                }
                continue;
            }
            let source = paragraph.source_text();
            if paragraph.translated_text.as_deref() != Some(source.as_str()) {
                return Err(MimusError::input(
                    InputReason::UnsupportedPdf,
                    "M1 can typeset only the identity output from --backend none",
                ));
            }
            let chars = paragraph.chars();
            if chars.is_empty() {
                return Err(MimusError::input(
                    InputReason::UnsupportedPdf,
                    "cannot typeset an empty line",
                ));
            }
            for character in chars {
                let matching_streams = streams
                    .keys()
                    .copied()
                    .filter(|object_id| object_id.0 == character.passthrough.content_object)
                    .collect::<Vec<_>>();
                let [content_object] = matching_streams.as_slice() else {
                    return Err(MimusError::internal(
                        InternalReason::InvariantViolation,
                        format!(
                            "character references ambiguous or missing content object {}",
                            character.passthrough.content_object
                        ),
                    ));
                };
                let key = (
                    *content_object,
                    character.passthrough.byte_start,
                    character.passthrough.byte_end,
                );
                span_is_replaceable
                    .entry(key)
                    .and_modify(|replaceable| {
                        *replaceable &= character.text_transform == TextTransform::Upright;
                    })
                    .or_insert(character.text_transform == TextTransform::Upright);
            }
        }
        let replaceable_spans = span_is_replaceable
            .iter()
            .filter_map(|(key, replaceable)| replaceable.then_some(*key))
            .collect::<BTreeSet<_>>();
        if replaceable_spans.is_empty() {
            continue;
        }
        let mut fonts = BTreeSet::new();
        for paragraph in &page.paragraphs {
            for character in paragraph.chars() {
                if streams
                    .keys()
                    .copied()
                    .find(|object_id| object_id.0 == character.passthrough.content_object)
                    .is_some_and(|content_object| {
                        replaceable_spans.contains(&(
                            content_object,
                            character.passthrough.byte_start,
                            character.passthrough.byte_end,
                        ))
                    })
                {
                    fonts.insert(character.font.clone());
                }
            }
        }
        let replacements = replaceable_spans
            .into_iter()
            .map(|(content_object, byte_start, byte_end)| {
                let source = streams[&content_object];
                let replacement = source.get(byte_start..byte_end).ok_or_else(|| {
                    MimusError::internal(
                        InternalReason::InvariantViolation,
                        format!(
                            "content object {} span {byte_start}..{byte_end} exceeds {} bytes",
                            content_object.0,
                            source.len()
                        ),
                    )
                })?;
                Ok(ContentSpanReplacement {
                    content_object,
                    byte_start,
                    byte_end,
                    replacement: replacement.to_vec(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        rewrites.push(PageRewrite {
            page_index: page.index,
            replacements,
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

    let rewritten_pages = document
        .rewrites
        .iter()
        .map(|rewrite| rewrite.page_index)
        .collect::<BTreeSet<_>>();
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
        if !rewritten_pages.contains(&expected.index) {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use crate::engine::{
        LayoutDetector, LayoutRegion, PageCharSnapshot, PdfInspector, Rasterizer, RgbaImage,
        SingleLineLayoutDetector,
    };
    use crate::event::{DiagnosticId, EventKind, PageDegradeReason, RecordingEventSink};
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

    #[derive(Default)]
    struct RecordingSnapshotSink {
        snapshots: Mutex<Vec<(usize, Stage, il::Document)>>,
    }

    impl RecordingSnapshotSink {
        fn snapshots(&self) -> Vec<(usize, Stage, il::Document)> {
            self.snapshots.lock().unwrap().clone()
        }
    }

    impl crate::PassSnapshotSink for RecordingSnapshotSink {
        fn write_snapshot(
            &self,
            pass_index: usize,
            stage: Stage,
            snapshot: &il::Document,
        ) -> Result<()> {
            self.snapshots
                .lock()
                .unwrap()
                .push((pass_index, stage, snapshot.clone()));
            Ok(())
        }
    }

    struct FailingLayoutDetector;

    impl LayoutDetector for FailingLayoutDetector {
        fn detect(
            &self,
            _geometry: PageGeometry,
            _raster: &RgbaImage,
            _characters: &[PageCharSnapshot],
        ) -> Result<Vec<LayoutRegion>> {
            Err(MimusError::input(
                InputReason::UnsupportedPdf,
                "injected layout failure",
            ))
        }
    }

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-base-01-single-line/unit-base-01-single-line.pdf")
    }

    fn scan_fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../corpus/fixtures/unit-scan-01-image-only/unit-scan-01-image-only.pdf")
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
    fn span_typeset_preserves_the_original_content_program_byte_for_byte() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("formatted-input.pdf");
        let output = directory.path().join("formatted-output.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        let original_program =
            b"% keep this comment\nBT /F1 12 Tf\n1 0 0 1 72 120 Tm\n(MIMUS)Tj\nET\n";
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(original_program.to_vec());
        pdf.save(&input).unwrap();
        let mut document = Document::new(&input, &output);
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

        run(&mut document, &context).unwrap();

        let output_pdf = LopdfDocument::load(&output).unwrap();
        let page_id = output_pdf.get_pages()[&1];
        let content_ids = output_pdf.get_page_contents(page_id);
        let [content_id] = content_ids.as_slice() else {
            panic!("expected exactly one output content stream");
        };
        let output_program = output_pdf
            .get_object(*content_id)
            .unwrap()
            .as_stream()
            .unwrap()
            .decompressed_content()
            .unwrap();
        assert_eq!(output_program, original_program);
    }

    #[test]
    fn scanned_rejection_precedes_translation_rasterization_and_write() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("must-not-exist.pdf");
        let mut document = Document::new(scan_fixture(), &output);
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

        let error = run(&mut document, &context).unwrap_err();

        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::ScannedPdf)
        );
        assert_eq!(error.scan_counts(), Some((1, 1)));
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 0);
        assert!(document.rewrites.is_empty());
        assert!(document.write_report.is_none());
        assert!(!output.exists());
    }

    #[test]
    fn all_blank_document_is_an_exact_zero_rewrite_passthrough() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("blank.pdf");
        let output = directory.path().join("blank-output.pdf");
        let mut pdf = LopdfDocument::load(fixture()).unwrap();
        pdf.get_object_mut((9, 0))
            .unwrap()
            .as_stream_mut()
            .unwrap()
            .set_plain_content(Vec::new());
        pdf.save(&input).unwrap();
        let input_bytes = std::fs::read(&input).unwrap();
        let mut document = Document::new(&input, &output);
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

        assert_eq!(result.appended_bytes, 0);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 0);
        assert!(document.il.pages[0].paragraphs.is_empty());
        assert_eq!(std::fs::read(output).unwrap(), input_bytes);
    }

    #[test]
    fn inspect_stops_after_paragraph_find_without_translation_or_write_state() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let snapshots = RecordingSnapshotSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &SingleLineLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: Some(&snapshots),
            config: crate::context::PipelineConfig::default(),
        };

        let result = inspect(&mut document, &context).unwrap();

        assert_eq!(result.pages, 1);
        assert_eq!(result.warnings, 0);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 1);
        assert_eq!(document.output_path(), None);
        assert!(document.rewrites.is_empty());
        assert!(document.write_report.is_none());
        assert!(document.il.pages[0].paragraphs[0].translated_text.is_none());

        let events = events.events();
        let started = events
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::StageStarted { stage } => Some(stage),
                _ => None,
            })
            .collect::<Vec<_>>();
        let finished = events
            .iter()
            .filter_map(|event| match event.kind {
                EventKind::StageFinished { stage } => Some(stage),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(started, ORDER[..INSPECT_STAGE_COUNT]);
        assert_eq!(finished, ORDER[..INSPECT_STAGE_COUNT]);
        assert!(events.iter().all(|event| !event.kind.is_terminal()));

        let snapshots = snapshots.snapshots();
        assert_eq!(
            snapshots
                .iter()
                .map(|(index, stage, _)| (*index, *stage))
                .collect::<Vec<_>>(),
            vec![
                (0, Stage::Parse),
                (1, Stage::ScanDetect),
                (2, Stage::Layout),
                (3, Stage::ParagraphFind),
            ]
        );
        assert_eq!(snapshots.last().unwrap().2, result.il);
    }

    #[test]
    fn failed_pass_keeps_only_completed_snapshot_prefix_and_has_no_finished_event() {
        let mut document = Document::for_inspection(fixture());
        let engine = FakeEngine::default();
        let translator = CountingTranslator::default();
        let events = RecordingEventSink::default();
        let snapshots = RecordingSnapshotSink::default();
        let context = PassContext {
            engine: &engine,
            layout_detector: &FailingLayoutDetector,
            translator: &translator,
            events: &events,
            snapshots: Some(&snapshots),
            config: crate::context::PipelineConfig::default(),
        };

        let error = inspect(&mut document, &context).unwrap_err();
        assert_eq!(
            error.reason(),
            crate::error::ErrorReason::Input(InputReason::UnsupportedPdf)
        );
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            snapshots
                .snapshots()
                .iter()
                .map(|(index, stage, _)| (*index, *stage))
                .collect::<Vec<_>>(),
            vec![(0, Stage::Parse), (1, Stage::ScanDetect)]
        );
        let events = events.events();
        assert!(events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::StageStarted {
                    stage: Stage::Layout
                }
            )
        }));
        assert!(!events.iter().any(|event| {
            matches!(
                event.kind,
                EventKind::StageFinished {
                    stage: Stage::Layout
                }
            )
        }));
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
        let walked = walk_page(&pdf, page_id).unwrap().characters;
        let mut engine = FakeEngine::default().page_characters(&[], 0).unwrap();
        engine[0].baseline_origin.x += 0.01;
        engine[0].baseline_origin.y -= 0.02;
        let mut diagnostics = Diagnostics::default();

        validate_character_alignment(0, &walked, &engine, 0.001, &mut diagnostics).unwrap();
        assert_eq!(diagnostics.entries().len(), 1);
        let Diagnostic::EngineBaselineMismatch {
            page_index,
            character_index,
            delta_x_pt,
            delta_y_pt,
        } = diagnostics.entries()[0]
        else {
            panic!("expected an engine baseline diagnostic");
        };
        assert_eq!(page_index, 0);
        assert_eq!(character_index, 0);
        assert!((delta_x_pt - 0.01).abs() < 1e-12);
        assert!((delta_y_pt - 0.02).abs() < 1e-12);
        assert!(delta_x_pt >= 0.0);
        assert!(delta_y_pt >= 0.0);

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
            replacements: Vec::new(),
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

    #[test]
    fn a_degraded_page_is_preserved_byte_for_byte_and_reported() {
        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join("degraded.pdf");
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

        parse(&mut document, &context).unwrap();
        scan_detect(&mut document, &context).unwrap();
        // 真正的触发点在走查（#18）与几何解析（#17）里；此处注入以单独锁住骨架的行为。
        document.extracted_pages[0].degraded = Some(PageDegradeReason::ContentStreamSyntax);
        document.diagnostics.push(Diagnostic::PageDegraded {
            page_index: 0,
            reason: PageDegradeReason::ContentStreamSyntax,
        });
        for &(_, pass) in &PIPELINE[2..] {
            pass(&mut document, &context).unwrap();
        }
        push_degradation_summary(&mut document);

        // 降级页不进 rewrites，增量写回因此原样复制输入。
        assert!(document.rewrites.is_empty());
        assert_eq!(
            std::fs::read(&output).unwrap(),
            std::fs::read(fixture()).unwrap()
        );
        assert_eq!(document.write_report.as_ref().unwrap().appended_bytes, 0);
        // 降级页不再进入光栅化与翻译。
        assert_eq!(engine.raster_calls.load(Ordering::SeqCst), 0);
        assert_eq!(translator.calls.load(Ordering::SeqCst), 0);
        assert!(document.il.pages[0].paragraphs.is_empty());

        let ids = document
            .diagnostics
            .entries()
            .iter()
            .map(Diagnostic::id)
            .collect::<Vec<_>>();
        assert!(ids.contains(&DiagnosticId::PageDegraded));
        assert!(document.diagnostics.entries().iter().any(|entry| matches!(
            entry,
            Diagnostic::DegradationSummary {
                degraded_page_indices,
                degraded_pages: 1,
                total_pages: 1,
                preserved_paragraphs,
            } if degraded_page_indices == &[0] && preserved_paragraphs.is_empty()
        )));
    }

    #[test]
    fn preserved_paragraphs_are_listed_in_the_degradation_summary() {
        let mut document = Document::for_inspection(fixture());
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

        inspect(&mut document, &context).unwrap();
        // 干净文档不产生汇总。
        assert!(document.diagnostics.entries().is_empty());

        document.il.pages[0].paragraphs[0].preserved = Some(il::PreservedReason::UnreliableUnicode);
        push_degradation_summary(&mut document);

        assert!(document.diagnostics.entries().iter().any(|entry| matches!(
            entry,
            Diagnostic::DegradationSummary {
                degraded_page_indices,
                degraded_pages: 0,
                preserved_paragraphs,
                ..
            } if degraded_page_indices.is_empty()
                && preserved_paragraphs
                    == &[PreservedParagraph {
                        page_index: 0,
                        paragraph_index: 0,
                        reason: il::PreservedReason::UnreliableUnicode,
                    }]
        )));
    }
}
